//! Tracks de mídia da libdatachannel — o **único** crate do Quall com `unsafe`.
//!
//! `quall-core` tem `#![forbid(unsafe_code)]` e continua tendo. Todo o FFI de mídia mora aqui,
//! numa superfície pequena o bastante para ser lida inteira.
//!
//! # Por que este crate existe
//!
//! O crate seguro `datachannel` 0.16.1 embrulha a libdatachannel muito bem para **canal de
//! dados** — foi com ele que o M1 fechou. Para **mídia** ele não serve, e não é questão de
//! gosto; são quatro buracos, verificados no fonte da versão que está no `Cargo.lock`:
//!
//! 1. **Não existe `on_track`.** `PeerConnectionHandler` (`src/peerconnection.rs:189`) tem
//!    `on_data_channel` e mais nada parecido, e a string `rtcSetTrackCallback` **não aparece em
//!    nenhum arquivo do crate**. Ou seja: o lado que responde nunca fica sabendo que uma track
//!    chegou. Sozinho, isso já inviabiliza qualquer receptor.
//! 2. **Nada da API de mídia é exposto**: nem `rtcSetH264Packetizer`, nem `rtcChainPliHandler`,
//!    nem `rtcRequestKeyframe`, nem as sessões de RTCP. `RtcTrack` (`src/track.rs`) tem
//!    `send`, `mid`, `direction` e `description`, e para.
//! 3. **Os ids da API C estão escondidos.** `RtcTrack.id` é privado e sem acessador;
//!    `PeerConnectionId(i32)` tem o campo privado. A API C inteira é indexada por esses
//!    inteiros, então sem eles não há como chamar nada.
//! 4. Consequência de (3): nem dá para escrever um complemento por fora sem gambiarra — que é
//!    exatamente o que [`id_da_conexao`] é, e está documentado como tal lá.
//!
//! A alternativa seria reescrever também a conexão e o canal de dados sobre `datachannel-sys`,
//! jogando fora o transporte que o M1 provou entre o MacBook e o Dell. Não vale: a superfície
//! nova seria muito maior, e `unsafe` novo em cima do coração do produto é risco pior que uma
//! gambiarra contida e testada.
//!
//! **Encaminhamento certo**: mandar para o `datachannel` upstream um `on_track` e acessadores
//! `i32` dos ids. São mudanças pequenas e este crate encolhe quase todo quando entrarem.
//!
//! # Como os callbacks chegam de volta em Rust
//!
//! Sem ponteiro de usuário. A libdatachannel oferece `rtcSetUserPointer`, mas em `pc` ele já
//! pertence ao `datachannel` (é o endereço da caixa dele), e sobrescrever quebraria os
//! callbacks do M1. Então este crate usa **registros globais indexados pelo id inteiro** que a
//! própria libdatachannel devolve em todo callback.
//!
//! Não é só para conviver com o `datachannel`: é mais seguro. Não há ponteiro cru atravessando
//! a fronteira de FFI, não há como um callback atrasado desreferenciar objeto já destruído — no
//! pior caso ele procura um id que já saiu do registro e não faz nada.
//!
//! Os tratadores são clonados para fora do cadeado **antes** de serem chamados. Chamar com o
//! cadeado na mão serializaria todas as tracks entre si e travaria de vez se um tratador
//! voltasse a entrar neste crate — e o do PLI volta, porque a reação natural a um PLI é mexer
//! na track.
//!
//! # A feature `media` é obrigatória
//!
//! O `bindgen` do `datachannel-sys` roda sobre o `rtc.h` inteiro, e o cabeçalho define
//! `RTC_ENABLE_MEDIA 1` por padrão. Resultado: `rtcChainPliHandler` e companhia **aparecem nos
//! bindings mesmo sem a feature**, e some tudo só na hora do link. Sem `media`, o cmake recebe
//! `NO_MEDIA=ON` e a libsrtp nem é compilada. Por isso a feature está fixada no
//! `Cargo.toml` do workspace, e não atrás de uma feature deste crate: um `cargo check` verde
//! com link quebrado é pior que não compilar.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use datachannel_sys as sys;
mod registro_seguro;

/// Erro deste crate. Fica propositalmente pobre: quem traduz para o `Error` do núcleo é o
/// `quall-core`, que é quem tem o vocabulário do produto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtcError(pub String);

impl std::fmt::Display for RtcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RtcError {}

pub type Result<T> = std::result::Result<T, RtcError>;

/// Traduz o código de retorno da API C.
///
/// A libdatachannel devolve negativo para erro e, em várias funções, um valor útil (id, tamanho)
/// quando dá certo.
fn checar(codigo: c_int, o_que: &str) -> Result<c_int> {
    if codigo >= 0 {
        return Ok(codigo);
    }
    let motivo = match codigo {
        sys::RTC_ERR_INVALID => "argumento inválido",
        sys::RTC_ERR_FAILURE => "falhou",
        sys::RTC_ERR_NOT_AVAIL => "indisponível",
        sys::RTC_ERR_TOO_SMALL => "buffer pequeno demais",
        _ => "erro desconhecido",
    };
    Err(RtcError(format!("{o_que}: {motivo} ({codigo})")))
}

fn c_string(s: &str, campo: &str) -> Result<CString> {
    CString::new(s)
        .map_err(|_| RtcError(format!("{campo} tem byte nulo no meio ({} bytes)", s.len())))
}

// ---------------------------------------------------------------------------------------------
// O registro da própria libdatachannel — a testemunha do caminho de saída
// ---------------------------------------------------------------------------------------------

/// Liga o registro interno da libdatachannel (e, com ele, o da libjuice) e manda cada linha para
/// `destino`. Devolve `false` se alguém já tinha instalado um registrador no processo.
///
/// # Por que isto existe, e por que ele é o instrumento central do caminho de saída
///
/// O socket de saída da sessão é **não-bloqueante** (`udp.c`, `ioctlsocket(FIONBIO)`), com
/// `SO_SNDBUF` pedido em 1 MiB. Quando esse buffer enche, o `sendto` volta `EWOULDBLOCK`, e o
/// caminho inteiro trata isso assim:
///
/// - `conn_poll_send` (`libjuice/src/conn_poll.c:415-421`) **registra e devolve erro**;
/// - `juice_send_diffserv` traduz para `JUICE_ERR_AGAIN`;
/// - `IceTransport::outgoing` vira `>= 0` → `false`;
/// - `Track::outgoing` (`impl/track.cpp:187-199`) sobrescreve `ret` a cada fragmento, então
///   **só o último fragmento decide o retorno**;
/// - `rtcSendMessage` devolve sucesso, `enviar_quadro` devolve `Ok(())`.
///
/// Ou seja: o descarte existe, é contado **em log e em lugar nenhum mais**, e o número de
/// sequência já foi gasto pelo pacotizador. É a dívida 30 inteira, e a única testemunha que a
/// biblioteca oferece de graça é esta linha de registro:
///
/// ```text
/// Send failed, buffer is full          (JLOG_INFO, EWOULDBLOCK/EAGAIN)
/// Send failed, errno=N                 (JLOG_WARN, qualquer outro erro)
/// ```
///
/// Sem ligar o registro, o produto é **cego** para esse descarte: `log::max_level()` é `Off` por
/// omissão, o `ensure_logging` do crate `datachannel` mapeia isso para `RTC_LOG_NONE`, e nem a
/// linha nem o erro chegam a lugar nenhum.
///
/// # Por que `log`, e por que o nível vai por aqui
///
/// O crate `datachannel` 0.16.1 chama `rtcInitLogger` **uma vez**, dentro de
/// `RtcPeerConnection::new`, com o nível lido de `log::max_level()`. Instalar o `rtcInitLogger`
/// por fora não adianta: aquele `call_once` roda depois e sobrescreve. O único ponto de controle
/// é a fachada `log`, e é por isso que este crate depende dela.
///
/// # Custo
///
/// `Info` **não** tem registro por pacote em nenhuma das duas bibliotecas: são 39 `PLOG_INFO` na
/// libdatachannel e 50 `JLOG_INFO` na libjuice, todos em troca de estado, derivação de chave e
/// falha. `Debug`/`Trace` têm (`PLOG_VERBOSE << "Send size="` em cada envio) e não servem para
/// medir — o custo de escrever o log mudaria o que se quer medir.
pub fn ativar_registro(nivel: NivelDeRegistro, destino: fn(&str)) -> bool {
    static DESTINO: OnceLock<fn(&str)> = OnceLock::new();
    struct Registrador;
    impl log::Log for Registrador {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, registro: &log::Record<'_>) {
            if let Some(d) = DESTINO.get() {
                let seguro = registro_seguro::mensagem_nativa(&registro.args().to_string());
                d(&format!("[libdatachannel {}] {seguro}", registro.level()));
            }
        }
        fn flush(&self) {}
    }
    let _ = DESTINO.set(destino);
    if log::set_logger(&Registrador).is_err() {
        return false;
    }
    log::set_max_level(match nivel {
        NivelDeRegistro::Aviso => log::LevelFilter::Warn,
        NivelDeRegistro::Informacao => log::LevelFilter::Info,
    });
    true
}

/// Nível do registro da libdatachannel. Deliberadamente **sem** `Debug`/`Verbose`: eles registram
/// por pacote e o custo de escrever mudaria a medição.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NivelDeRegistro {
    /// Só falhas. Pega `Send failed, errno=N`, e **não** pega `Send failed, buffer is full`.
    Aviso,
    /// Falhas e troca de estado. É o nível que a medição do caminho de saída exige.
    Informacao,
}

// ---------------------------------------------------------------------------------------------
// Registro de tratadores
// ---------------------------------------------------------------------------------------------

type TratadorMensagem = Arc<dyn Fn(&[u8]) + Send + Sync>;
type TratadorSimples = Arc<dyn Fn() + Send + Sync>;
type TratadorTrack = Arc<dyn Fn(Track) + Send + Sync>;

#[derive(Default, Clone)]
struct Tratadores {
    mensagem: Option<TratadorMensagem>,
    pli: Option<TratadorSimples>,
    aberta: Option<TratadorSimples>,
    fechada: Option<TratadorSimples>,
}

fn registro_tracks() -> &'static Mutex<HashMap<c_int, Tratadores>> {
    static R: OnceLock<Mutex<HashMap<c_int, Tratadores>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registro_conexoes() -> &'static Mutex<HashMap<c_int, TratadorTrack>> {
    static R: OnceLock<Mutex<HashMap<c_int, TratadorTrack>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lê um tratador e **solta o cadeado** antes de devolver. Ver a nota de reentrância no topo.
fn pegar<T>(id: c_int, escolher: impl Fn(&Tratadores) -> Option<T>) -> Option<T> {
    let guarda = registro_tracks().lock().ok()?;
    escolher(guarda.get(&id)?)
}

fn ajustar(id: c_int, mudar: impl FnOnce(&mut Tratadores)) {
    if let Ok(mut guarda) = registro_tracks().lock() {
        mudar(guarda.entry(id).or_default());
    }
}

// ---------------------------------------------------------------------------------------------
// Ponte de callbacks (as únicas `extern "C"` do projeto)
// ---------------------------------------------------------------------------------------------

/// # Segurança
///
/// Chamada pela libdatachannel numa thread dela. `msg` aponta para `size` bytes válidos durante
/// a chamada, e a fatia não escapa do tratador — que é o que o contrato de mídia pede de todo
/// jeito ("empacota e solta").
unsafe extern "C" fn cb_mensagem(id: c_int, msg: *const c_char, size: c_int, _ptr: *mut c_void) {
    let Some(tratador) = pegar(id, |t| t.mensagem.clone()) else {
        return;
    };
    // `size < 0` é a convenção da libdatachannel para "string terminada em nulo". Numa track de
    // vídeo isso não acontece — RTP é binário —, mas tratar é mais barato que confiar.
    let bytes = if size < 0 {
        CStr::from_ptr(msg).to_bytes()
    } else if msg.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(msg.cast::<u8>(), size as usize)
    };
    tratador(bytes);
}

/// # Segurança
///
/// Só lê `id`. Nada é desreferenciado.
unsafe extern "C" fn cb_pli(id: c_int, _ptr: *mut c_void) {
    if let Some(tratador) = pegar(id, |t| t.pli.clone()) {
        tratador();
    }
}

/// # Segurança
///
/// Só lê `id`.
unsafe extern "C" fn cb_aberta(id: c_int, _ptr: *mut c_void) {
    if let Some(tratador) = pegar(id, |t| t.aberta.clone()) {
        tratador();
    }
}

/// # Segurança
///
/// Só lê `id`.
unsafe extern "C" fn cb_fechada(id: c_int, _ptr: *mut c_void) {
    if let Some(tratador) = pegar(id, |t| t.fechada.clone()) {
        tratador();
    }
}

/// # Segurança
///
/// Só lê os dois ids. `ptr` é o ponteiro de usuário do `pc`, que pertence ao crate
/// `datachannel` — por isso é ignorado, e não desreferenciado.
unsafe extern "C" fn cb_track(pc: c_int, tr: c_int, _ptr: *mut c_void) {
    let tratador = {
        let Ok(guarda) = registro_conexoes().lock() else {
            return;
        };
        guarda.get(&pc).cloned()
    };
    if let Some(tratador) = tratador {
        tratador(Track { id: tr });
    }
}

// ---------------------------------------------------------------------------------------------
// Conexão
// ---------------------------------------------------------------------------------------------

/// O id inteiro que a API C usa para se referir a uma conexão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PcId(pub c_int);

/// Extrai o id inteiro de uma `RtcPeerConnection` do crate `datachannel`.
///
/// # Isto é uma gambiarra, e está aqui de olhos abertos
///
/// `RtcPeerConnection::id()` devolve `PeerConnectionId`, um `pub struct PeerConnectionId(i32)`
/// com o campo **privado** e sem acessador. O tipo deriva `Debug`, então o inteiro sai em
/// `"PeerConnectionId(7)"` — e é daí que este código o tira.
///
/// Depender do `Debug` derivado de um tipo de terceiro é frágil, e não se defende como bom
/// desenho. Se defende como a opção menos ruim entre três:
///
/// - reescrever conexão e canal de dados sobre `datachannel-sys` (muito mais `unsafe` novo, em
///   cima justamente do que o M1 já provou na bancada);
/// - manter um fork do `datachannel` (custo permanente);
/// - isto, que cabe em vinte linhas, falha como `Err` e nunca como pânico, e tem um teste que
///   **usa o id de verdade** — `id_serve_para_criar_track` cria uma track com ele. Formato
///   errado não passa despercebido: quebra na CI.
///
/// Sai quando o upstream expuser o id. Ver a nota no topo do módulo.
pub fn id_da_conexao<P>(pc: &datachannel::RtcPeerConnection<P>) -> Result<PcId>
where
    P: datachannel::PeerConnectionHandler + Send,
    P::DCH: datachannel::DataChannelHandler + Send,
{
    let texto = format!("{:?}", pc.id());
    let cru = texto
        .strip_prefix("PeerConnectionId(")
        .and_then(|resto| resto.strip_suffix(')'))
        .ok_or_else(|| {
            RtcError(format!(
                "o `Debug` de PeerConnectionId mudou de formato: {texto:?}. \
                 Ver `id_da_conexao` em quall-rtc."
            ))
        })?;
    let id = cru.trim().parse::<c_int>().map_err(|_| {
        RtcError(format!(
            "o `Debug` de PeerConnectionId trouxe algo que não é inteiro: {cru:?}"
        ))
    })?;
    Ok(PcId(id))
}

/// Registra o tratador de track que chega do outro lado.
///
/// É o que o crate `datachannel` não tem, e sem o que nenhum receptor funciona.
///
/// Precisa ser chamado **antes** de a descrição remota ser aplicada: a libdatachannel dispara o
/// callback ao processar o SDP da oferta, e um tratador registrado depois disso perde a track.
pub fn ao_chegar_track(pc: PcId, tratador: impl Fn(Track) + Send + Sync + 'static) -> Result<()> {
    if let Ok(mut guarda) = registro_conexoes().lock() {
        guarda.insert(pc.0, Arc::new(tratador));
    }
    // SAFETY: `pc.0` é um id da libdatachannel e `cb_track` é uma função `extern "C"` válida
    // pelo tempo de vida do processo.
    checar(
        unsafe { sys::rtcSetTrackCallback(pc.0, Some(cb_track)) },
        "registrar o callback de track",
    )
    .map(|_| ())
}

/// Esquece o tratador de track de uma conexão. Chame ao destruir a conexão.
pub fn esquecer_conexao(pc: PcId) {
    if let Ok(mut guarda) = registro_conexoes().lock() {
        guarda.remove(&pc.0);
    }
}

// ---------------------------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------------------------

/// Quantas tracks este processo já destruiu de fato na libdatachannel.
///
/// # Por que um contador, e não uma consulta
///
/// A pergunta natural — "a libdatachannel ainda conhece este id?" — só tem uma porta na API C:
/// chamar `rtcGetTrackMid` (ou qualquer outra) com o id e ver se dá erro. **Isso trava o processo
/// no Windows.** Medido no Dell G3 em 2026-08-23: toda função da API C busca o objeto assim
///
/// ```text
/// shared_ptr<Track> getTrack(int id) {
///     std::lock_guard lock(mutex);
///     if (auto it = trackMap.find(id); it != trackMap.end()) return it->second;
///     else throw std::invalid_argument("Track ID does not exist");
/// }
/// ```
///
/// e a exceção sobe **de dentro do `lock_guard` do mutex global do `capi.cpp`**. No macOS e no
/// Android o desenrolamento solta o cadeado e a chamada devolve `RTC_ERR_INVALID`, como o
/// contrato promete. No Windows, a chamada seguinte que precise desse mutex — `rtcCreateTrack`,
/// `rtcSendMessage`, `rtcGetBufferedAmount` e, sobretudo, **`rtcCreatePeerConnection`** — bloqueia
/// para sempre, sem CPU e sem log.
///
/// Consequência que vale para o produto, não só para o teste: **nenhuma chamada da API C pode
/// receber um id já destruído**. É por isso que existe a bandeira de sessão viva em
/// `quall_core::track`, e é por isso que este contador substituiu a consulta.
pub static TRACKS_DESTRUIDAS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Direção da track no SDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direcao {
    SendOnly,
    RecvOnly,
    SendRecv,
}

impl Direcao {
    fn cru(self) -> sys::rtcDirection {
        match self {
            Direcao::SendOnly => sys::rtcDirection_RTC_DIRECTION_SENDONLY,
            Direcao::RecvOnly => sys::rtcDirection_RTC_DIRECTION_RECVONLY,
            Direcao::SendRecv => sys::rtcDirection_RTC_DIRECTION_SENDRECV,
        }
    }
}

/// O codec de uma track, do jeito que `rtcTrackInit.codec` o pede.
///
/// É ele que decide se a linha `m=` sai como `video` ou como `audio`: o `rtcAddTrackEx` da
/// libdatachannel escolhe entre `Description::Video` e `Description::Audio` olhando **só** este
/// campo (`src/capi.cpp`, 0.23.2). Errar aqui não dá erro — dá uma track de áudio anunciada
/// como vídeo, que o outro lado aceita e nunca toca.
///
/// Só os três que o Quall usa estão aqui. A libdatachannel também oferece VP8, VP9, H265, AV1,
/// PCMA, G722 e AAC; nenhum entrou porque nenhum tem uso decidido, e um enum que promete mais do
/// que o núcleo sustenta é convite a chamar com o que não foi testado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Opus,
    Pcmu,
}

impl Codec {
    fn cru(self) -> sys::rtcCodec {
        match self {
            Codec::H264 => sys::rtcCodec_RTC_CODEC_H264,
            Codec::Opus => sys::rtcCodec_RTC_CODEC_OPUS,
            Codec::Pcmu => sys::rtcCodec_RTC_CODEC_PCMU,
        }
    }

    /// A linha `m=` sai como `audio`?
    pub fn e_audio(self) -> bool {
        matches!(self, Codec::Opus | Codec::Pcmu)
    }
}

/// Como uma track nasce. Espelha `rtcTrackInit`.
#[derive(Debug, Clone)]
pub struct TrackInit {
    pub direcao: Direcao,
    /// O codec, que também decide se a linha `m=` é `video` ou `audio`. Ver [`Codec`].
    pub codec: Codec,
    /// Tipo de payload RTP. 96 é o primeiro dinâmico, e é o que o Quall usa para H.264.
    pub payload_type: i32,
    pub ssrc: u32,
    /// Identificador da linha `m=` no SDP. É por ele que os dois lados casam as tracks.
    pub mid: String,
    /// Rótulo legível. Vira o `a=msid` e é o que o receptor mostra na tela.
    pub nome: String,
    /// Perfil do codec: `profile-level-id` para H.264, os parâmetros da RFC 7587 para Opus. Vira
    /// a linha `a=fmtp` da track.
    pub perfil: String,
}

/// Ajustes do pacotizador RFC 6184. Espelha `rtcPacketizerInit`.
#[derive(Debug, Clone)]
pub struct PacketizerInit {
    pub ssrc: u32,
    pub cname: String,
    pub payload_type: u8,
    /// 90 kHz para vídeo, por RFC 3551. Não é ajustável na prática; está aqui por simetria.
    pub clock_rate: u32,
    /// Maior fragmento FU-A. `0` usa o padrão da libdatachannel (1188 bytes).
    pub max_fragment_size: u16,
}

/// Uma track de mídia.
///
/// **Não é dona do recurso.** `Drop` não chama `rtcDeleteTrack`, de propósito: uma track vive
/// enquanto a conexão viver, e a libdatachannel destrói as tracks junto com o `pc`. Destruir
/// aqui daria dupla destruição no caminho normal. Quem quiser fechar antes chama
/// [`Track::fechar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    id: c_int,
}

impl Track {
    /// O id cru da API C. Útil para depurar e para casar com log da libdatachannel.
    pub fn id(self) -> c_int {
        self.id
    }

    /// Cria uma track no lado que oferece.
    ///
    /// Precisa ser chamado **antes** de a oferta sair. A libdatachannel negocia sozinha assim
    /// que o primeiro canal de dados ou a primeira track aparece, e o que não estava lá na hora
    /// da oferta só entra com renegociação.
    pub fn adicionar(pc: PcId, init: &TrackInit) -> Result<Track> {
        let mid = c_string(&init.mid, "mid")?;
        let nome = c_string(&init.nome, "nome")?;
        let perfil = c_string(&init.perfil, "perfil")?;

        let cru = sys::rtcTrackInit {
            direction: init.direcao.cru(),
            codec: init.codec.cru(),
            payloadType: init.payload_type,
            ssrc: init.ssrc,
            mid: mid.as_ptr(),
            name: nome.as_ptr(),
            msid: nome.as_ptr(),
            trackId: mid.as_ptr(),
            profile: perfil.as_ptr(),
        };

        // SAFETY: `cru` só aponta para os `CString` acima, todos vivos até o fim desta função,
        // e a libdatachannel copia o que precisa antes de retornar.
        let id = checar(unsafe { sys::rtcAddTrackEx(pc.0, &cru) }, "criar a track")?;
        let track = Track { id };
        track.ligar_callbacks()?;
        Ok(track)
    }

    /// Liga os callbacks básicos. Idempotente.
    fn ligar_callbacks(self) -> Result<()> {
        // SAFETY: id válido, funções `extern "C"` estáticas.
        unsafe {
            checar(
                sys::rtcSetMessageCallback(self.id, Some(cb_mensagem)),
                "callback de mensagem",
            )?;
            checar(
                sys::rtcSetOpenCallback(self.id, Some(cb_aberta)),
                "callback de abertura",
            )?;
            checar(
                sys::rtcSetClosedCallback(self.id, Some(cb_fechada)),
                "callback de fechamento",
            )?;
        }
        Ok(())
    }

    /// Prepara uma track que **chegou** pelo [`ao_chegar_track`].
    pub fn preparar_recebida(self) -> Result<()> {
        self.ligar_callbacks()
    }

    pub fn ao_receber(self, tratador: impl Fn(&[u8]) + Send + Sync + 'static) {
        ajustar(self.id, |t| t.mensagem = Some(Arc::new(tratador)));
    }

    pub fn ao_abrir(self, tratador: impl Fn() + Send + Sync + 'static) {
        ajustar(self.id, |t| t.aberta = Some(Arc::new(tratador)));
    }

    pub fn ao_fechar(self, tratador: impl Fn() + Send + Sync + 'static) {
        ajustar(self.id, |t| t.fechada = Some(Arc::new(tratador)));
    }

    /// Instala o pacotizador H.264 da libdatachannel (RFC 6184).
    ///
    /// Single NAL para quadro que cabe na MTU, FU-A para o que não cabe. O separador é
    /// `START_SEQUENCE`, que aceita tanto `00 00 01` quanto `00 00 00 01` — as duas formas
    /// aparecem no mesmo fluxo Annex-B, e o VideoToolbox e o MediaCodec não concordam sobre qual
    /// usar.
    pub fn pacotizador_h264(self, init: &PacketizerInit) -> Result<()> {
        let cname = c_string(&init.cname, "cname")?;
        let cru = sys::rtcPacketizerInit {
            ssrc: init.ssrc,
            cname: cname.as_ptr(),
            payloadType: init.payload_type,
            clockRate: init.clock_rate,
            sequenceNumber: 0,
            timestamp: 0,
            maxFragmentSize: init.max_fragment_size,
            nalSeparator: sys::rtcNalUnitSeparator_RTC_NAL_SEPARATOR_START_SEQUENCE,
            obuPacketization: sys::rtcObuPacketization_RTC_OBU_PACKETIZED_OBU,
            playoutDelayId: 0,
            playoutDelayMin: 0,
            playoutDelayMax: 0,
        };
        // SAFETY: `cname` vive até o fim da função; a libdatachannel copia a struct.
        checar(
            unsafe { sys::rtcSetH264Packetizer(self.id, &cru) },
            "instalar o pacotizador H.264",
        )
        .map(|_| ())
    }

    /// Instala o pacotizador de áudio da libdatachannel.
    ///
    /// # Ele não fragmenta, e isso é o desenho — não uma limitação
    ///
    /// `OpusRtpPacketizer` e `PCMURtpPacketizer` são o **mesmo** tipo com relógios diferentes:
    /// `AudioRtpPacketizer<48000>` e `AudioRtpPacketizer<8000>`
    /// (`include/rtc/rtppacketizer.hpp` da 0.23.2). Nenhum sobrescreve o `fragment` da classe
    /// base, cuja implementação padrão é
    ///
    /// ```text
    /// std::vector<binary> RtpPacketizer::fragment(binary data) { return {std::move(data)}; }
    /// ```
    ///
    /// ou seja: **uma mensagem entra, um pacote RTP sai.** Não há FU-A, não há agregação e não há
    /// buffer de remontagem do outro lado — um quadro de Opus de 20 ms tem ~80 bytes e cabe numa
    /// MTU com folga de quinze vezes. É por isso que o `max_fragment_size` do
    /// [`PacketizerInit`] é ignorado aqui, e é por isso que o depacotizador de áudio do núcleo é
    /// uma fração do de vídeo.
    ///
    /// **Consequência que morde:** como `payloads.size()` é sempre 1, o
    /// `bool mark = i == payloads.size() - 1` do `RtpPacketizer::outgoing` fica **sempre
    /// verdadeiro**. Todo pacote de áudio sai com o bit de marca ligado. A RFC 3551 §4.1 reserva
    /// esse bit para o primeiro pacote depois de um silêncio, então o valor que a libdatachannel
    /// escreve **não significa nada**: quem receber não pode ler começo de rajada nele.
    ///
    /// Recusa `Codec::H264`, porque isso seria pedir o pacotizador errado sem perceber.
    pub fn pacotizador_audio(self, codec: Codec, init: &PacketizerInit) -> Result<()> {
        let cname = c_string(&init.cname, "cname")?;
        let cru = sys::rtcPacketizerInit {
            ssrc: init.ssrc,
            cname: cname.as_ptr(),
            payloadType: init.payload_type,
            clockRate: init.clock_rate,
            sequenceNumber: 0,
            timestamp: 0,
            // Ignorado pelo pacotizador de áudio: ele não fragmenta. Fica em 0 (o "padrão" da
            // API C) em vez de num número inventado que sugeriria o contrário a quem ler.
            maxFragmentSize: 0,
            nalSeparator: sys::rtcNalUnitSeparator_RTC_NAL_SEPARATOR_START_SEQUENCE,
            obuPacketization: sys::rtcObuPacketization_RTC_OBU_PACKETIZED_OBU,
            playoutDelayId: 0,
            playoutDelayMin: 0,
            playoutDelayMax: 0,
        };
        // SAFETY: `cname` vive até o fim da função; a libdatachannel copia a struct.
        let codigo = match codec {
            Codec::Opus => unsafe { sys::rtcSetOpusPacketizer(self.id, &cru) },
            Codec::Pcmu => unsafe { sys::rtcSetPCMUPacketizer(self.id, &cru) },
            Codec::H264 => {
                return Err(RtcError(
                    "pacotizador_audio recebeu H.264; use pacotizador_h264".into(),
                ))
            }
        };
        checar(codigo, "instalar o pacotizador de áudio").map(|_| ())
    }

    /// Encadeia o relator de RTCP SR (sender report).
    ///
    /// Barato e necessário: sem SR o receptor não tem como relacionar o relógio RTP com tempo de
    /// parede, e sem isso não há sincronia entre tela e microfone quando o áudio entrar.
    pub fn relator_rtcp(self) -> Result<()> {
        // SAFETY: id válido.
        checar(
            unsafe { sys::rtcChainRtcpSrReporter(self.id) },
            "encadear o relator de RTCP",
        )
        .map(|_| ())
    }

    /// Encadeia a sessão de recepção de RTCP.
    ///
    /// É ela que constrói e envia o PLI quando [`Track::pedir_idr`] é chamado, e que responde os
    /// relatórios do emissor. Sem ela `rtcRequestKeyframe` devolve falso e o pedido some.
    pub fn sessao_rtcp(self) -> Result<()> {
        // SAFETY: id válido.
        checar(
            unsafe { sys::rtcChainRtcpReceivingSession(self.id) },
            "encadear a sessão de RTCP",
        )
        .map(|_| ())
    }

    /// Encadeia o tratador de PLI **e de FIR**.
    ///
    /// O nome na API C engana: `PliHandler::incoming` (`src/plihandler.cpp` da libdatachannel
    /// 0.23.2) reconhece os dois — payload type 206 com FMT 1 é PLI (RFC 4585), payload type
    /// 196 é FIR (RFC 5104) — e chama o mesmo tratador. É exatamente o que o contrato de mídia
    /// pede, e não precisa de código nosso.
    pub fn ao_pedir_idr(self, tratador: impl Fn() + Send + Sync + 'static) -> Result<()> {
        ajustar(self.id, |t| t.pli = Some(Arc::new(tratador)));
        // SAFETY: id válido, `cb_pli` é estática.
        checar(
            unsafe { sys::rtcChainPliHandler(self.id, Some(cb_pli)) },
            "encadear o tratador de PLI",
        )
        .map(|_| ())
    }

    /// Encadeia o **espaçador** de saída: no máximo `bits_por_segundo`, em lotes de um
    /// `intervalo_ms` de verba cada.
    ///
    /// Tem de ser **o último** da corrente: ele segura os pacotes e os entrega direto ao
    /// transporte, e um tratador encadeado depois dele não os veria. Só existe na cópia da
    /// libdatachannel em `vendor/datachannel-sys` — a 0.23.2 publicada não o expõe na API C, e a
    /// guarda do agendamento dela vinha invertida. Ver `vendor/datachannel-sys/QUALL-PATCH.md`.
    pub fn espacar(self, bits_por_segundo: f64, intervalo_ms: i32) -> Result<()> {
        // SAFETY: id válido; a função recusa taxa ou intervalo não positivos.
        checar(
            unsafe { sys::rtcChainPacingHandler(self.id, bits_por_segundo, intervalo_ms) },
            "encadear o espaçador",
        )
        .map(|_| ())
    }

    /// Pede um quadro-chave ao outro lado. Emite PLI.
    ///
    /// Exige [`Track::sessao_rtcp`] instalada e track de vídeo: `Track::requestKeyframe()` na
    /// libdatachannel só empurra PLI quando `description().type() == "video"` e existe um
    /// tratador de mídia encadeado.
    ///
    /// **Numa track de áudio isto nunca funciona, e o erro é mudo.** `requestKeyframe()`
    /// devolve `false` sem tocar na rede (`src/track.cpp`, "only push PLI for video"), e o
    /// `wrap` da API C traduz isso para `RTC_ERR_FAILURE` — indistinguível de "a track ainda
    /// não abriu". Quem chama do núcleo barra o caso antes de chegar aqui; ver
    /// `TrackReceptor::pedir_idr`.
    pub fn pedir_idr(self) -> Result<()> {
        // SAFETY: id válido.
        checar(unsafe { sys::rtcRequestKeyframe(self.id) }, "pedir IDR").map(|_| ())
    }

    /// Fixa o carimbo RTP do próximo quadro, na escala de 90 kHz.
    pub fn carimbo_rtp(self, carimbo: u32) -> Result<()> {
        // SAFETY: id válido.
        checar(
            unsafe { sys::rtcSetTrackRtpTimestamp(self.id, carimbo) },
            "fixar o carimbo RTP",
        )
        .map(|_| ())
    }

    /// Entrega um quadro Annex-B inteiro ao pacotizador.
    ///
    /// A libdatachannel fatia e manda. Não há fila nossa no caminho: esta função volta quando os
    /// pacotes já foram entregues ao transporte.
    pub fn enviar(self, bytes: &[u8]) -> Result<()> {
        let tamanho = c_int::try_from(bytes.len())
            .map_err(|_| RtcError(format!("quadro de {} bytes não cabe em c_int", bytes.len())))?;
        // SAFETY: `bytes` é válido por `tamanho` bytes durante a chamada, e a libdatachannel
        // copia para a própria mensagem antes de retornar.
        checar(
            unsafe { sys::rtcSendMessage(self.id, bytes.as_ptr().cast::<c_char>(), tamanho) },
            "enviar o quadro",
        )
        .map(|_| ())
    }

    /// Quantos bytes ainda esperam para sair.
    pub fn pendente(self) -> usize {
        // SAFETY: id válido; a função só lê.
        let n = unsafe { sys::rtcGetBufferedAmount(self.id) };
        usize::try_from(n).unwrap_or(0)
    }

    /// O `mid` negociado. Vazio se a track ainda não tem descrição.
    pub fn mid(self) -> String {
        self.texto(|buf, tam| {
            // SAFETY: `buf` tem `tam` bytes graváveis.
            unsafe { sys::rtcGetTrackMid(self.id, buf, tam) }
        })
    }

    /// A descrição SDP da track (a linha `m=` e os atributos dela).
    pub fn descricao(self) -> String {
        self.texto(|buf, tam| {
            // SAFETY: `buf` tem `tam` bytes graváveis.
            unsafe { sys::rtcGetTrackDescription(self.id, buf, tam) }
        })
    }

    /// Padrão de "pergunte o tamanho, aloque, peça o conteúdo" da API C.
    ///
    /// Devolve `String` vazia em vez de erro: os dois usos são informativos (log e casamento de
    /// track), e nenhum deles justifica derrubar uma sessão.
    fn texto(self, mut chamar: impl FnMut(*mut c_char, c_int) -> c_int) -> String {
        let tamanho = chamar(std::ptr::null_mut(), 0);
        if tamanho <= 0 {
            return String::new();
        }
        let mut buf = vec![0u8; tamanho as usize];
        if chamar(buf.as_mut_ptr().cast::<c_char>(), tamanho) < 0 {
            return String::new();
        }
        // A libdatachannel escreve com terminador nulo; corta nele.
        let fim = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..fim]).into_owned()
    }

    /// Fecha a track e apaga a entrada dela nos mapas globais da libdatachannel.
    ///
    /// Devolve se o `rtcDeleteTrack` deu certo — isto é, se o id **estava** no mapa global e saiu
    /// dele. É o que prova as dívidas 4, 14 e 21 sem precisar consultar um id morto, o que no
    /// Windows é uma armadilha (ver [`TRACKS_DESTRUIDAS`]).
    ///
    /// Chamar duas vezes para o mesmo id devolve `false` **e provoca uma exceção C++ dentro da
    /// libdatachannel** — não faça.
    pub fn fechar(self) -> bool {
        if let Ok(mut guarda) = registro_tracks().lock() {
            guarda.remove(&self.id);
        }
        // SAFETY: id válido; depois disto ele não é mais usado por esta `Track`.
        let r = unsafe { sys::rtcDeleteTrack(self.id) };
        if r >= 0 {
            TRACKS_DESTRUIDAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Tira os tratadores do registro sem destruir a track na libdatachannel.
    pub fn esquecer(self) {
        if let Ok(mut guarda) = registro_tracks().lock() {
            guarda.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datachannel::{
        DataChannelHandler, DataChannelInfo, PeerConnectionHandler, RtcConfig, RtcPeerConnection,
    };

    struct SemCanal;
    impl DataChannelHandler for SemCanal {}

    struct SemNada;
    impl PeerConnectionHandler for SemNada {
        type DCH = SemCanal;
        fn data_channel_handler(&mut self, _: DataChannelInfo) -> Self::DCH {
            SemCanal
        }
    }

    fn conexao() -> Box<RtcPeerConnection<SemNada>> {
        let vazio: [&str; 0] = [];
        RtcPeerConnection::new(&RtcConfig::new(&vazio), SemNada).expect("conexão")
    }

    fn init(mid: &str) -> TrackInit {
        TrackInit {
            direcao: Direcao::SendOnly,
            codec: Codec::H264,
            payload_type: 96,
            ssrc: 0x5155_4131,
            mid: mid.to_string(),
            nome: "tela".to_string(),
            perfil: "profile-level-id=42e028;packetization-mode=1".to_string(),
        }
    }

    fn init_opus(mid: &str) -> TrackInit {
        TrackInit {
            direcao: Direcao::SendOnly,
            codec: Codec::Opus,
            payload_type: 111,
            ssrc: 0x5155_0003,
            mid: mid.to_string(),
            nome: "microfone".to_string(),
            perfil: "minptime=10;useinbandfec=1;stereo=0".to_string(),
        }
    }

    /// O teste que segura a gambiarra do [`id_da_conexao`].
    ///
    /// Não confere o formato do `Debug` — confere que o id **funciona**: cria uma track de
    /// verdade com ele. Se o upstream mudar o `Debug`, ou se o número extraído for outro, isto
    /// falha na CI e não na bancada.
    #[test]
    fn id_serve_para_criar_track() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id da conexão");
        let track = Track::adicionar(id, &init("0")).expect("track com o id extraído");
        assert!(track.id() >= 0);
        track.fechar();
    }

    #[test]
    fn id_de_conexoes_diferentes_e_diferente() {
        let a = conexao();
        let b = conexao();
        assert_ne!(
            id_da_conexao(&a).expect("id a"),
            id_da_conexao(&b).expect("id b"),
            "duas conexões com o mesmo id: a extração está pegando o número errado"
        );
    }

    #[test]
    fn track_de_video_aceita_pacotizador_e_tratador_de_pli() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let track = Track::adicionar(id, &init("0")).expect("track");

        track
            .pacotizador_h264(&PacketizerInit {
                ssrc: 0x5155_4131,
                cname: "quall".into(),
                payload_type: 96,
                clock_rate: 90_000,
                max_fragment_size: 1188,
            })
            .expect("pacotizador");
        track.relator_rtcp().expect("relator");
        track.ao_pedir_idr(|| {}).expect("tratador de PLI");

        // A descrição sai como vídeo H.264 — se sair vazia, a feature `media` não está ligada.
        let descricao = track.descricao();
        assert!(
            descricao.contains("H264") || descricao.contains("h264"),
            "descrição sem H264: {descricao:?}"
        );
        track.fechar();
    }

    /// **A prova de que o codec vira uma linha `m=audio` de verdade, e não um vídeo disfarçado.**
    ///
    /// `rtcAddTrackEx` escolhe entre `Description::Video` e `Description::Audio` olhando só o
    /// campo `codec`. Se ele ficasse em H.264 por engano, a track sairia anunciada como vídeo,
    /// o outro lado aceitaria, os pacotes atravessariam — e nada tocaria. Não há erro nesse
    /// caminho; só o SDP denuncia. Por isso o teste lê o SDP.
    #[test]
    fn track_de_opus_sai_como_audio_no_sdp() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let track = Track::adicionar(id, &init_opus("microphone")).expect("track de opus");

        let descricao = track.descricao();
        assert!(
            descricao.contains("m=audio"),
            "a track de Opus não saiu como `m=audio`: {descricao:?}"
        );
        assert!(
            !descricao.contains("m=video"),
            "a track de Opus saiu como vídeo: {descricao:?}"
        );
        // RFC 7587 §7: o relógio do Opus em RTP é sempre 48000 e o campo de canais é sempre 2,
        // mesmo em mono — quem decide mono é o `stereo=` do fmtp, não o rtpmap.
        assert!(
            descricao.contains("opus/48000/2"),
            "rtpmap do Opus fora do que a RFC 7587 manda: {descricao:?}"
        );
        assert!(
            descricao.contains("111"),
            "o payload type 111 não chegou ao SDP: {descricao:?}"
        );
        track.fechar();
    }

    #[test]
    fn track_de_audio_aceita_o_pacotizador_de_opus() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let track = Track::adicionar(id, &init_opus("microphone")).expect("track");

        track
            .pacotizador_audio(
                Codec::Opus,
                &PacketizerInit {
                    ssrc: 0x5155_0003,
                    cname: "quall".into(),
                    payload_type: 111,
                    clock_rate: 48_000,
                    max_fragment_size: 0,
                },
            )
            .expect("pacotizador de opus");
        track.relator_rtcp().expect("relator");
        track.fechar();
    }

    /// Pedir o pacotizador de áudio com H.264 é erro nosso, não da libdatachannel — e tem de
    /// falhar aqui, onde a mensagem ainda diz o que fazer.
    #[test]
    fn pacotizador_de_audio_recusa_h264() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let track = Track::adicionar(id, &init("0")).expect("track");
        let erro = track.pacotizador_audio(
            Codec::H264,
            &PacketizerInit {
                ssrc: 1,
                cname: "quall".into(),
                payload_type: 96,
                clock_rate: 90_000,
                max_fragment_size: 0,
            },
        );
        assert!(erro.is_err());
        track.fechar();
    }

    /// Áudio e vídeo na **mesma** conexão, que é a promessa do `PROMPT.md`: tela, câmera e
    /// microfone simultâneos numa sessão. Cada um é uma linha `m=` própria.
    #[test]
    fn video_e_audio_convivem_na_mesma_conexao() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let tela = Track::adicionar(id, &init("screen")).expect("tela");
        let microfone = Track::adicionar(id, &init_opus("microphone")).expect("microfone");

        assert_ne!(tela.id(), microfone.id());
        assert!(tela.descricao().contains("m=video"));
        assert!(microfone.descricao().contains("m=audio"));

        tela.fechar();
        microfone.fechar();
    }

    #[test]
    fn mid_volta_como_foi_pedido() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let track = Track::adicionar(id, &init("tela")).expect("track");
        assert_eq!(track.mid(), "tela");
        track.fechar();
    }

    #[test]
    fn varias_tracks_na_mesma_conexao() {
        // O contrato exige tela, câmera e microfone simultâneos. Cada uma é uma linha `m=`.
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let tela = Track::adicionar(id, &init("tela")).expect("tela");
        let camera = Track::adicionar(id, &init("camera")).expect("camera");
        assert_ne!(tela.id(), camera.id());
        assert_eq!(tela.mid(), "tela");
        assert_eq!(camera.mid(), "camera");
        tela.fechar();
        camera.fechar();
    }

    #[test]
    fn mid_com_byte_nulo_e_erro_e_nao_panico() {
        let pc = conexao();
        let id = id_da_conexao(&pc).expect("id");
        let mut mau = init("0");
        mau.mid = "te\0la".to_string();
        assert!(Track::adicionar(id, &mau).is_err());
    }
}
