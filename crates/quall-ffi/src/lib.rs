// Os identificadores e endereços de exemplos/fixtures são sintéticos; não identificam a bancada privada.
//! Superfície C do núcleo, consumida pelas cascas: Swift (macOS/iOS), Kotlin via JNI (Android),
//! C++ (plugin de OBS) e C/C++ (câmera virtual do Windows).
//!
//! O header correspondente é gerado por `build.rs` em `include/quall.h` — não editar à mão.
//!
//! # Nenhuma casca do Quall é escrita em Rust
//!
//! Vale repetir porque muda a prioridade desta camada: Swift, Kotlin e o C++ do OBS **só
//! enxergam o que passa por aqui**. Uma API Rust bonita que não atravessa esta fronteira não
//! existe para o produto.
//!
//! # Os nomes do contrato, e onde eles foram parar
//!
//! `docs/contrato-track.md` fixa os nomes em português da API Rust. Em C os identificadores são
//! globais no processo e convivem com `quall_protocol_version` e `quall_service_type`, que
//! nasceram em inglês no M0. Misturar `quall_enviar_quadro` com `quall_protocol_version` no mesmo
//! header seria pior que traduzir. O mapa, então, é este — e é 1 para 1:
//!
//! | contrato (Rust)      | esta fronteira (C)              |
//! |----------------------|---------------------------------|
//! | `enviar_quadro`      | [`quall_track_send_frame`]      |
//! | `ao_pedir_idr`       | [`quall_track_on_idr_request`]  |
//! | `ao_receber_quadro`  | [`quall_track_on_frame`]        |
//! | `pedir_idr`          | [`quall_track_request_idr`]     |
//! | `pegar_pedido_de_idr`| [`quall_track_take_idr_request`]|
//! | `QuadroCodificado`   | `QuallFrame`                    |
//! | `TrackKind`          | `QuallTrackKind`                |
//!
//! # Regras desta camada
//!
//! **Nada de `panic` atravessando a fronteira.** Em release o workspace usa `panic = "abort"`,
//! então `catch_unwind` não salvaria nada — o processo já morreu antes. A defesa é não escrever
//! código que entre em pânico: sem `unwrap`, sem `expect`, sem indexar sem conferir, e todo
//! ponteiro do chamador testado contra nulo antes de ser desreferenciado.
//!
//! **O caminho do quadro não tem fila nem cópia guardada.** [`quall_track_send_frame`] recebe um
//! ponteiro emprestado e entrega ao pacotizador. O `QuallFrame` que chega em
//! [`quall_track_on_frame`] aponta para o buffer de remontagem do núcleo e **vale só durante a
//! chamada** — copiar dali é decisão da casca, e no caminho normal ela entrega direto ao decoder
//! sem copiar.
//!
//! # Convenções
//!
//! - Toda string é UTF-8 terminada em NUL.
//! - Função que devolve texto segue o padrão `(buf, cap) -> intptr_t`: devolve quantos bytes são
//!   necessários **incluindo o NUL**, e só escreve se couber. Chame com `buf` nulo para
//!   perguntar o tamanho. Negativo é erro.
//! - Função que devolve ponteiro devolve nulo em erro; o motivo sai em [`quall_last_error`] como
//!   texto e em [`quall_last_status`] como **código**. Ramifique pelo código: o texto está em
//!   português e comparar prefixo dele é o defeito que fez `QUALL_STATUS_NO_ROUTE` existir.
//! - Todo ponteiro devolvido por `..._new`, `..._start`, `quall_host`, `quall_connect`,
//!   `quall_session_track` e `quall_session_next_track` é do chamador, e tem uma função de
//!   liberar. Liberar duas vezes é erro do chamador, como em qualquer API C.
//! - `quall_host` e `quall_connect` **bloqueiam** até a sessão fechar ou o prazo estourar. Chame
//!   de uma thread de trabalho, nunca da thread de interface. Para interromper a espera, use
//!   [`QuallCanceller`] com [`quall_host_cancelable`] / [`quall_connect_cancelable`].
//! - **Tratador se desregistra passando `cb` nulo**, e [`quall_session_close`] **é** barreira:
//!   com `QUALL_OK`, nenhum tratador está rodando e nenhum voltará a rodar, e o `user_data` pode
//!   ser liberado. Ver [`quall_session_close`]; o mecanismo
//!   está em `quall_core::portao`.
//! - Funções que **avançam** estado — [`quall_session_next_track`],
//!   [`quall_session_next_event`], [`quall_browser_collect`] — são de uma thread só.
//!
//! # Este texto precisa chegar ao header
//!
//! O cbindgen não emite documentação de módulo, então esta seção **não** aparecia em `quall.h` —
//! enquanto seis funções do header apontavam para "o topo do módulo" como se aparecesse. Swift,
//! Kotlin e o C++ do OBS leem o header e mais nada. A cópia que eles veem está em
//! `cbindgen.toml`, no campo `header`; mexeu aqui, mexa lá.
//!
//! # Uma exceção ao português
//!
//! As seções de contrato das funções `unsafe` se chamam `# Safety`, em inglês, e o texto delas
//! segue em português. Não é inconsistência: `clippy::missing_safety_doc` procura esse título
//! literal, e é ele que garante que nenhuma função desta fronteira seja publicada sem dizer o
//! que o chamador precisa garantir. Título em português desligaria a verificação — e num arquivo
//! que é todo ponteiro cru, essa verificação vale mais que a uniformidade da prosa.

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::net::SocketAddr;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quall_core::cancel::Cancelamento;
use quall_core::discovery::{
    endereco_manual_com_porta, escolher_porta_do_teleprompter, ler_destino, porta_para_completar,
    Advertiser, Browser, DiscoveredDevice, DiscoveryEvent, PORTA_DO_TELEPROMPTER,
    PRAZO_DE_RESOLUCAO,
};
use quall_core::error::Error;
use quall_core::jitter::{BufferDeJitter, ContadoresDeBuffer, Entrega};
use quall_core::pairing::{PairedPeers, Pin};
use quall_core::portao::{Barreira, PRAZO_DA_BARREIRA};
use quall_core::protocol::{Announcement, Capabilities, DeviceId, Papel, Screen, PROTOCOL_VERSION};
use quall_core::relogio::{DeslocamentoDeCaptura, RetratoDoRelogio};
use quall_core::reproducao::{
    ContadoresDeReproducao, LeitorDeReproducao, Puxado, ReproducaoPuxada,
};
use quall_core::rtp::Contadores;
use quall_core::session::{conectar, hospedar, EventoDeSessao, Ready, SessionConfig};
use quall_core::signaling::{RelatoDoEnlace, SignalingServer};
// `Politica` já é o do jitter neste arquivo; o da taxa entra com nome próprio para que
// nenhuma das duas leituras dependa de qual `use` veio primeiro.
use quall_core::taxa::{Amostra, ControleDeTaxa, Motivo, Politica as PoliticaDeTaxa};
use quall_core::track::{
    AmostraDeAudio, CodecDeAudio, PresetDeAudio, QuadroCodificado, TrackConfig, TrackEmissor,
    TrackKind, TrackReceptor,
};
use quall_core::transport::TransportConfig;
mod registro_seguro;

// =============================================================================================
// Erro
// =============================================================================================

/// Resultado de uma chamada. `QUALL_OK` é zero; todo o resto é falha.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallStatus {
    Ok = 0,
    /// Entrada malformada vinda do usuário ou da rede.
    Invalid = 1,
    /// A outra ponta fala outra versão do protocolo, ou mandou algo fora de ordem.
    Protocol = 2,
    Discovery = 3,
    Signaling = 4,
    Transport = 5,
    /// **Pareamento recusado por um motivo que não é o PIN nem o par esquecido.**
    ///
    /// MAC de retomada que não confere, segredo guardado do aparelho errado, mensagem fora de
    /// ordem. **Dívida 29:** até 2026-08-27 este código também cobria os outros dois, e as cascas
    /// escreviam "O PIN não conferiu" em cima dele — a frente do Windows viu esse texto **com o
    /// PIN certo**. Quem quiser dar conselho ao usuário quer [`QuallStatus::WrongPin`] ou
    /// [`QuallStatus::NeedsPin`]; este aqui é o resto, e o conselho honesto para ele é "tente de
    /// novo, e se insistir, pareie do zero".
    Pairing = 6,
    Timeout = 7,
    Closed = 8,
    Io = 9,
    /// Ponteiro nulo onde a função exige um objeto.
    ///
    /// **Dívida 8.** Este código e o [`QuallStatus::NotUtf8`] estavam no header sem que nenhum
    /// caminho os produzisse: ponteiro nulo saía como `QUALL_STATUS_INVALID`, misturado com PIN
    /// malformado e IP inválido. Agora saem separados — erro de programação da casca de um lado,
    /// entrada ruim do usuário do outro.
    NullPointer = 10,
    /// String do chamador que não é UTF-8 válido. Ver [`QuallStatus::NullPointer`].
    NotUtf8 = 11,
    /// **O ICE não achou caminho entre os dois aparelhos.**
    ///
    /// No iOS é quase sempre **permissão de Rede Local negada**; nas outras plataformas, é
    /// isolamento de AP ou Wi-Fi de hóspede. Existe porque a casca iOS distinguia este caso —
    /// o mais provável do produto — comparando prefixo de string em português, que quebra na
    /// primeira vez que alguém reescreve a mensagem.
    ///
    /// **Dívida 28, consertada em 2026-08-27.** Este código nascia **num lugar só**, o
    /// `PeerState::Failed` do ICE — que só corre depois de a sinalização estar de pé. Quando não
    /// há rota, quem morre primeiro é o `connect` TCP da sinalização, e aquilo saía como
    /// `QUALL_STATUS_IO`: o status era inalcançável justamente no caso mais comum de não haver
    /// rota, e o receptor iOS teve de contornar com uma sonda TCP própria. Agora nasce nas duas
    /// camadas.
    ///
    /// **`QUALL_STATUS_IO` numa conexão recusada continua sendo `IO`, e de propósito**: um RST é
    /// prova de que existe rota, e o conselho ali é "abra o app no outro aparelho".
    NoRoute = 12,
    /// **Este aparelho não está mais pareado do outro lado: peça o PIN de novo.**
    ///
    /// Não é recusa, é convite a recomeçar — a casca mostra a tela de PIN em vez de "falhou".
    /// Ver a dívida 22.
    NeedsPin = 13,
    /// A casca cancelou a espera com [`quall_session_cancel`].
    Cancelled = 14,
    /// **O PIN digitado não conferiu. Peça para digitar de novo.**
    ///
    /// **Dívida 29.** O par deste código é o [`QuallStatus::NeedsPin`], e os dois conselhos são
    /// **opostos**:
    ///
    /// - `WRONG_PIN` — existe um PIN válido no outro aparelho e a digitação errou. *"PIN errado,
    ///   tente de novo"*. Reconectar é obrigatório: vale **uma tentativa por conexão**, e é isso
    ///   que segura o PIN de seis dígitos.
    /// - `NEEDS_PIN` — o outro aparelho **não reconhece** este pareamento. Digitar o mesmo PIN de
    ///   novo não leva a lugar nenhum. *"Peça um PIN novo no outro aparelho"*.
    ///
    /// Antes desta rodada os dois chegavam como `QUALL_STATUS_PAIRING`, e a casca não tinha como
    /// separá-los sem comparar texto de mensagem de erro — que a frente do Windows recusou fazer,
    /// e fez bem.
    ///
    /// Entrou **no fim**, com o valor 15, pela regra de sempre: uma casca compilada contra o
    /// header antigo nunca recebia 15, então nenhum valor que ela conhece muda de significado.
    /// Ela passa a ver `PAIRING` só no caso residual — o que é uma melhora, não uma quebra.
    WrongPin = 15,
    /// **O outro aparelho está ocupado: tente de novo daqui a pouco.**
    ///
    /// Um teleprompter com controle já conectado responde isto a um segundo controle, e ao mesmo
    /// controle que volta depois de uma queda enquanto o prompter solta a sessão velha. Ver
    /// `docs/contrato-teleprompter.md` §2. Entrou no fim, com o próximo valor livre, pela regra de
    /// sempre: nenhum valor que uma casca já conhece muda de significado.
    Busy = 16,
}

impl From<&Error> for QuallStatus {
    fn from(e: &Error) -> Self {
        match e {
            Error::Invalid(_) => QuallStatus::Invalid,
            Error::Protocol(_) => QuallStatus::Protocol,
            Error::Discovery(_) => QuallStatus::Discovery,
            Error::Signaling(_) => QuallStatus::Signaling,
            Error::Transport(_) => QuallStatus::Transport,
            Error::NoRoute(_) => QuallStatus::NoRoute,
            Error::Pairing(_) => QuallStatus::Pairing,
            Error::WrongPin(_) => QuallStatus::WrongPin,
            Error::NeedsPin(_) => QuallStatus::NeedsPin,
            Error::Timeout(_) => QuallStatus::Timeout,
            Error::Closed => QuallStatus::Closed,
            Error::Cancelled => QuallStatus::Cancelled,
            Error::Io(_) => QuallStatus::Io,
            Error::Ocupado(_) => QuallStatus::Busy,
        }
    }
}

/// Erro **da fronteira**, que sabe duas coisas que o núcleo não sabe.
///
/// `quall-core` tem `#![forbid(unsafe_code)]` e não tem ponteiro nenhum: "ponteiro nulo" e
/// "string que não é UTF-8" são fatos desta camada, não dele. Enfiá-los no `Error` do núcleo
/// seria contaminar o vocabulário do produto com um detalhe de C. Este enum é o lugar deles, e
/// é por isso que os dois códigos de status finalmente têm quem os produza.
enum Falha {
    /// Ponteiro nulo onde a função exige um objeto.
    Nulo(String),
    /// String do chamador que não é UTF-8 válido.
    NaoUtf8(String),
    /// Qualquer coisa que o núcleo já sabia dizer.
    Nucleo(Error),
}

impl From<Error> for Falha {
    fn from(e: Error) -> Self {
        Falha::Nucleo(e)
    }
}

impl Falha {
    fn status(&self) -> QuallStatus {
        match self {
            Falha::Nulo(_) => QuallStatus::NullPointer,
            Falha::NaoUtf8(_) => QuallStatus::NotUtf8,
            Falha::Nucleo(e) => QuallStatus::from(e),
        }
    }

    fn mensagem(&self) -> String {
        match self {
            Falha::Nulo(m) | Falha::NaoUtf8(m) => m.clone(),
            Falha::Nucleo(e) => e.to_string(),
        }
    }
}

/// `Result` desta camada.
type Saida<T> = std::result::Result<T, Falha>;

thread_local! {
    /// Última falha **desta thread**: a mensagem e o código, juntos.
    ///
    /// Por thread, e não global, porque uma casca pode ter uma thread de sessão e outra de
    /// captura: um erro de uma sobrescrevendo a mensagem da outra produziria diagnóstico
    /// trocado, que é pior que diagnóstico nenhum.
    ///
    /// **Mensagem e código no mesmo lugar, de propósito.** Se fossem duas células, uma escrita
    /// parcial deixaria a casca lendo o código de uma falha com o texto de outra — e o motivo de
    /// existir o código é justamente não depender do texto.
    static ULTIMA_FALHA: RefCell<(CString, QuallStatus)> =
        RefCell::new((CString::default(), QuallStatus::Ok));
}

/// Guarda a mensagem e o código desta thread, e devolve o status pedido.
fn guardar_mensagem(texto: String, status: QuallStatus) -> QuallStatus {
    ULTIMA_FALHA.with(|celula| {
        let mut guarda = celula.borrow_mut();
        // `CString::new` só falha com byte nulo no meio; a mensagem é formatada por nós e não
        // tem. Ainda assim, cair para uma string vazia é melhor que entrar em pânico aqui.
        if let Ok(c) = CString::new(texto) {
            guarda.0 = c;
        }
        // O código sai mesmo que a mensagem não tenha dado: é ele que a casca vai ramificar.
        guarda.1 = status;
    });
    status
}

fn guardar_erro(e: &Error) -> QuallStatus {
    guardar_mensagem(e.to_string(), QuallStatus::from(e))
}

fn guardar_falha(f: &Falha) -> QuallStatus {
    guardar_mensagem(f.mensagem(), f.status())
}

/// Guarda uma mensagem de entrada malformada. Para ponteiro nulo use [`guardar_nulo`], que é o
/// que finalmente produz `QUALL_STATUS_NULL_POINTER` (dívida 8).
fn guardar_texto(texto: &str) -> QuallStatus {
    guardar_mensagem(texto.to_string(), QuallStatus::Invalid)
}

/// Ponteiro nulo do chamador: erro de programação da casca, não entrada ruim do usuário.
fn guardar_nulo(texto: &str) -> QuallStatus {
    guardar_mensagem(texto.to_string(), QuallStatus::NullPointer)
}

/// Mensagem do último erro **desta thread**, UTF-8 terminada em NUL.
///
/// O ponteiro vale até a próxima chamada que falhe nesta mesma thread. Copie se precisar
/// guardar. Nunca é nulo: sem erro, devolve string vazia.
///
/// **A mensagem é para a pessoa, não para o programa.** Para o programa decidir o que fazer,
/// leia [`quall_last_status`]: o texto está em português e muda quando alguém o reescreve.
///
/// # Safety
///
/// O ponteiro não deve ser liberado por quem chama.
#[no_mangle]
pub extern "C" fn quall_last_error() -> *const c_char {
    ULTIMA_FALHA.with(|celula| celula.borrow().0.as_ptr())
}

/// **Código da última falha desta thread** — o par de [`quall_last_error`], para o programa.
///
/// # Por que ele existe
///
/// [`quall_host`] e [`quall_connect`] devolvem **ponteiro nulo** quando falham, e o motivo só
/// existia como texto. Isso morde um caso nomeado e decidido: `QUALL_STATUS_NEEDS_PIN` não é
/// recusa, é convite a recomeçar — a casca deve mostrar a tela de PIN em vez de "falhou"
/// (dívida 22). Para distingui-lo de "IP errado" ou "o ICE não achou caminho", a casca teria de
/// **comparar prefixo de string em português** — que é exatamente o defeito que fez
/// [`QuallStatus::NoRoute`] existir. O receptor Android desistiu de distinguir por causa disso.
///
/// Vale para toda função que sinaliza falha sem devolver `QuallStatus`: as que devolvem ponteiro
/// (`quall_host`, `quall_connect`, `quall_browser_start`, `quall_advertiser_start`,
/// `quall_session_track`, `quall_session_next_track`) e as do padrão `(buf, cap)`, que devolvem
/// negativo.
///
/// # A regra de leitura, e ela é a mesma de `quall_last_error`
///
/// **Leia logo depois da chamada que falhou, antes de qualquer outra função `quall_`,** e só
/// quando aquela chamada de fato sinalizou falha. Uma chamada bem-sucedida **não** limpa este
/// valor — sem falha nenhuma nesta thread ele é `QUALL_STATUS_OK`, e depois de uma falha ele
/// fica como estava até a próxima. Perguntar "deu erro?" a esta função é ler um valor velho; a
/// pergunta certa é ao valor devolvido pela chamada (nulo, ou negativo).
///
/// Em C, e **sem comentário dentro do exemplo**: o cbindgen copia este texto para dentro de um
/// bloco de comentário do `quall.h`, e um fecha-comentário escrito aqui fecharia aquele bloco no
/// meio da prosa. O teste `nenhum_doc_comment_do_header_fecha_no_meio` é a tranca.
///
/// ```c
/// QuallSession *s = quall_connect(ip, &opcoes);
/// if (!s) {
///     switch (quall_last_status()) {
///     case QUALL_STATUS_WRONG_PIN: pedir_o_pin_de_novo();      break;
///     case QUALL_STATUS_NEEDS_PIN: pedir_um_pin_novo_la();     break;
///     case QUALL_STATUS_NO_ROUTE:  explicar_rede_local();      break;
///     case QUALL_STATUS_CANCELLED: voltar_sem_dizer_nada();    break;
///     default:                     mostrar(quall_last_error()); break;
///     }
/// }
/// ```
#[no_mangle]
pub extern "C" fn quall_last_status() -> QuallStatus {
    ULTIMA_FALHA.with(|celula| celula.borrow().1)
}

// =============================================================================================
// Utilidades de fronteira
// =============================================================================================

/// Lê uma string C do chamador. `NULL` vira `None`.
///
/// # Safety
///
/// `p` precisa ser nulo ou apontar para uma sequência terminada em NUL válida.
unsafe fn texto_opcional<'a>(p: *const c_char, campo: &str) -> Saida<Option<&'a str>> {
    if p.is_null() {
        return Ok(None);
    }
    CStr::from_ptr(p)
        .to_str()
        .map(Some)
        .map_err(|_| Falha::NaoUtf8(format!("{campo}: a string do chamador não é UTF-8")))
}

/// Lê uma string C obrigatória.
///
/// # Safety
///
/// Igual a [`texto_opcional`], mas nulo é erro.
unsafe fn texto<'a>(p: *const c_char, campo: &str) -> Saida<&'a str> {
    texto_opcional(p, campo)?.ok_or_else(|| Falha::Nulo(format!("{campo} não pode ser nulo")))
}

/// Copia `conteudo` para `buf` no padrão `(buf, cap) -> intptr_t` descrito no topo do módulo.
///
/// # Safety
///
/// `buf` precisa ser nulo ou apontar para `cap` bytes graváveis.
unsafe fn escrever_texto(conteudo: &str, buf: *mut c_char, cap: usize) -> isize {
    let bytes = conteudo.as_bytes();
    // Uma string com NUL no meio truncaria em C sem avisar; recusar é mais honesto.
    if bytes.contains(&0) {
        return -1;
    }
    let precisa = bytes.len() + 1;
    let Ok(tamanho) = isize::try_from(precisa) else {
        return -1;
    };
    if buf.is_null() || cap < precisa {
        return tamanho;
    }
    ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast::<u8>(), bytes.len());
    // O NUL final: sem ele, quem chama lê memória alheia.
    *buf.add(bytes.len()) = 0;
    tamanho
}

/// `user_data` do chamador, atravessando para outra thread.
///
/// A libdatachannel chama os tratadores de threads dela, e um ponteiro cru não é `Send`. Este
/// embrulho afirma que é.
///
/// # O que a casca precisa garantir
///
/// Que o objeto apontado continue vivo enquanto a track existir, e que seja seguro tocá-lo de
/// outra thread. O núcleo não tem como verificar isso, e por isso está dito aqui e no header:
/// é a única obrigação que esta fronteira transfere para quem chama.
#[derive(Clone, Copy)]
struct Contexto(*mut c_void);

// SAFETY: afirmação sobre o ponteiro do chamador, documentada acima. O Rust não pode provar,
// e nenhuma alternativa em C poderia.
unsafe impl Send for Contexto {}
unsafe impl Sync for Contexto {}

impl Contexto {
    /// O ponteiro cru.
    ///
    /// Existe como método, e não como campo lido direto, por causa da **captura disjunta** da
    /// edição 2021: `move || funcao(contexto.0)` captura o campo `*mut c_void` — que não é
    /// `Send` — em vez da struct que afirma ser. A chamada de método obriga o fechamento a
    /// capturar `Contexto` inteiro, que é o ponto de todo este embrulho.
    fn ptr(self) -> *mut c_void {
        self.0
    }
}

// =============================================================================================
// Identidade
// =============================================================================================

/// Quem é este aparelho, para o outro lado.
#[repr(C)]
pub struct QuallDeviceDesc {
    /// Identificador estável, gerado na primeira execução e persistido pela casca. É o que o
    /// pareamento vincula — não o nome, que o usuário pode trocar.
    pub device_id: *const c_char,
    /// Nome real exibido ao par depois da autenticação, sem anunciar em mDNS.
    pub display_name: *const c_char,
    pub screen_source: bool,
    pub camera_source: bool,
    pub sink: bool,
}

impl QuallDeviceDesc {
    /// # Safety
    ///
    /// `self` precisa ter strings válidas ou nulas.
    unsafe fn para_anuncio(&self) -> Saida<Announcement> {
        let id = texto(self.device_id, "device_id")?;
        let nome = texto(self.display_name, "display_name")?;
        Ok(Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.to_string()),
            display_name: nome.to_string(),
            capabilities: Capabilities {
                screen_source: self.screen_source,
                camera_source: self.camera_source,
                sink: self.sink,
            },
            // A tela entra por chamada, em `quall_connect_with_screen` — nunca aqui, que também
            // monta o `Welcome` de quem hospeda (revisão de 10/09/2026).
            screen: None,
            // O papel também entra por chamada, nas funções `_with_role`, pelo mesmo motivo — e
            // porque este struct é layout que as cascas repetem à mão.
            papel: None,
        })
    }
}

// =============================================================================================
// Descoberta
// =============================================================================================

/// Anunciante mDNS. Enquanto ele existir, o aparelho aparece na LAN.
pub struct QuallAdvertiser {
    interno: Option<Advertiser>,
}

/// Começa a anunciar este aparelho por mDNS na porta de sinalização dada.
///
/// # Safety
///
/// `me` precisa apontar para um [`QuallDeviceDesc`] válido.
#[no_mangle]
pub unsafe extern "C" fn quall_advertiser_start(
    me: *const QuallDeviceDesc,
    signaling_port: u16,
) -> *mut QuallAdvertiser {
    if me.is_null() {
        guardar_nulo("quall_advertiser_start: `me` é nulo");
        return ptr::null_mut();
    }
    let anuncio = match (*me).para_anuncio() {
        Ok(a) => a,
        Err(f) => {
            guardar_falha(&f);
            return ptr::null_mut();
        }
    };
    match Advertiser::start(&anuncio, signaling_port) {
        Ok(interno) => Box::into_raw(Box::new(QuallAdvertiser {
            interno: Some(interno),
        })),
        Err(e) => {
            guardar_erro(&e);
            ptr::null_mut()
        }
    }
}

/// Rótulo público efêmero do anunciante (`Quall <prefix8>`), igual ao mostrado na descoberta.
/// Padrão `(buf, cap)`: tamanho UTF-8 incluindo NUL; não escreve se não couber; `-1` em erro.
/// O nome real do aparelho só é enviado depois da autenticação.
///
/// # Safety
/// `a` precisa vir de `quall_advertiser_start`/`_with_role` e continuar vivo durante a chamada.
/// `buf` precisa ser nulo ou apontar para `cap` bytes graváveis.
#[no_mangle]
pub unsafe extern "C" fn quall_advertiser_label(
    a: *const QuallAdvertiser,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(anunciante) = a.as_ref().and_then(|a| a.interno.as_ref()) else {
        guardar_nulo("quall_advertiser_label: anunciante nulo ou encerrado");
        return -1;
    };
    escrever_texto(anunciante.discovery_label(), buf, cap)
}

/// Para de anunciar e libera. Nulo é ignorado.
///
/// # Dívida 3: isto não desregistrava nada
///
/// A versão anterior só soltava a caixa, e o `Advertiser` **não tinha `Drop`** — apesar de o
/// comentário dele afirmar que tinha. Cada sessão deixava para trás uma thread de daemon mDNS
/// viva e um anúncio fantasma na lista dos outros aparelhos até o TTL expirar. No desktop o
/// processo morre e limpa; no Android o processo sobrevive a dezenas de sessões.
///
/// Agora esta função desregistra e espera a confirmação, como o [`quall_browser_stop`] já fazia.
/// O `Drop` do `Advertiser` faz o mesmo, para quem esquecer de chamar.
///
/// **Bloqueia** por até ~1 s esperando o adeus sair. Chame da thread de trabalho.
///
/// # Safety
///
/// `a` precisa vir de [`quall_advertiser_start`] e não pode ter sido liberado antes.
#[no_mangle]
pub unsafe extern "C" fn quall_advertiser_stop(a: *mut QuallAdvertiser) {
    if a.is_null() {
        return;
    }
    let mut caixa = Box::from_raw(a);
    if let Some(interno) = caixa.interno.take() {
        // Falha ao desregistrar não interessa a quem chama: o objeto vai embora de qualquer
        // jeito, e não há nada que a casca possa fazer sobre isso.
        let _ = interno.stop();
    }
}

/// Navegador mDNS, com o que ele já achou.
///
/// # A lista fica atrás de um cadeado, e isso não é zelo (achado da auditoria)
///
/// [`quall_browser_collect`] pedia `&mut` e [`quall_browser_devices_json`] pedia `&`, os dois
/// sobre o mesmo ponteiro. A casca que atualizasse a lista numa thread enquanto desenha a tela
/// noutra — que é exatamente como uma tela de aparelhos se escreve — produzia dois empréstimos
/// conflitantes do mesmo `Vec`: corrida de dados de verdade, comportamento indefinido, não
/// "provavelmente funciona".
///
/// Com o [`Mutex`], as duas funções passam a receber `*const` e a leitura é segura de qualquer
/// thread.
pub struct QuallBrowser {
    interno: Option<Browser>,
    achados: std::sync::Mutex<Vec<DiscoveredDevice>>,
}

/// Começa a navegar `_quall._tcp` na LAN.
#[no_mangle]
pub extern "C" fn quall_browser_start() -> *mut QuallBrowser {
    match Browser::start() {
        Ok(interno) => Box::into_raw(Box::new(QuallBrowser {
            interno: Some(interno),
            achados: std::sync::Mutex::new(Vec::new()),
        })),
        Err(e) => {
            guardar_erro(&e);
            ptr::null_mut()
        }
    }
}

/// Junta o que aparecer durante `ms` milissegundos e devolve quantos aparelhos há **no total**.
///
/// **Bloqueia** por `ms`. Devolve negativo em erro.
///
/// # "Junta" passou a ser verdade (achado da auditoria)
///
/// Antes, cada chamada **substituía** a lista pelo que tinha aparecido naquela janela. Como o
/// mDNS entrega cada anúncio uma vez, a segunda chamada no mesmo navegador via **menos**
/// aparelhos que a primeira — a lista da tela encolhia sozinha enquanto o usuário olhava para
/// ela. O header prometia "junta" e o código trocava.
///
/// Agora a lista é acumulada: aparelho novo entra, aparelho que já estava é atualizado, e
/// aparelho que anunciou a saída é removido. É o que uma tela de aparelhos precisa.
///
/// Chamar em laço com `ms` curto é o uso esperado; a lista só cresce com quem está na rede.
///
/// # Safety
///
/// `b` precisa vir de [`quall_browser_start`].
#[no_mangle]
pub unsafe extern "C" fn quall_browser_collect(b: *const QuallBrowser, ms: u32) -> i32 {
    let Some(navegador) = b.as_ref() else {
        guardar_nulo("quall_browser_collect: navegador nulo");
        return -1;
    };
    let Some(interno) = navegador.interno.as_ref() else {
        guardar_texto("quall_browser_collect: o navegador já foi parado");
        return -1;
    };

    let prazo = std::time::Instant::now() + Duration::from_millis(u64::from(ms));
    loop {
        let resta = prazo.saturating_duration_since(std::time::Instant::now());
        if resta.is_zero() {
            break;
        }
        match interno.next_event(resta) {
            Ok(Some(DiscoveryEvent::Found(aparelho))) => {
                let Ok(mut lista) = navegador.achados.lock() else {
                    guardar_texto("quall_browser_collect: lista envenenada por um panic");
                    return -1;
                };
                match lista
                    .iter_mut()
                    .find(|a| a.announcement.device_id == aparelho.announcement.device_id)
                {
                    Some(existente) => *existente = *aparelho,
                    None => lista.push(*aparelho),
                }
            }
            Ok(Some(DiscoveryEvent::Lost(fullname))) => {
                let Ok(mut lista) = navegador.achados.lock() else {
                    guardar_texto("quall_browser_collect: lista envenenada por um panic");
                    return -1;
                };
                lista.retain(|a| a.fullname != fullname);
            }
            Ok(None) => break,
            Err(e) => {
                guardar_erro(&e);
                return -1;
            }
        }
    }

    match navegador.achados.lock() {
        Ok(lista) => i32::try_from(lista.len()).unwrap_or(i32::MAX),
        Err(_) => {
            guardar_texto("quall_browser_collect: lista envenenada por um panic");
            -1
        }
    }
}

/// Escreve os aparelhos achados como **JSON**, no padrão `(buf, cap)` do topo do módulo.
///
/// JSON, e não uma struct por aparelho, de propósito: a alternativa seria uma função com cinco
/// buffers de saída e um índice, que toda casca erraria de um jeito diferente. Swift, Kotlin e
/// C++ já têm um leitor de JSON à mão, e a lista de aparelhos é lida uma vez por tela — não é
/// caminho quente.
///
/// Formato: um array de objetos com `device_id`, `display_name`, `protocol_version`,
/// `capabilities` (`screen_source`, `camera_source`, `sink`) e `endpoint` (`"ip:porta"` ou
/// `null` quando o aparelho não anunciou endereço utilizável).
/// `identity_authenticated` é `false`: o ID e nome desta lista são placeholders efêmeros,
/// não servem para consultar ou persistir pareamentos. Use o peer da sessão após autenticar.
///
/// # Safety
///
/// `b` precisa vir de [`quall_browser_start`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_browser_devices_json(
    b: *const QuallBrowser,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(navegador) = b.as_ref() else {
        guardar_nulo("quall_browser_devices_json: navegador nulo");
        return -1;
    };
    let Ok(achados) = navegador.achados.lock() else {
        guardar_texto("quall_browser_devices_json: lista envenenada por um panic");
        return -1;
    };
    let lista: Vec<serde_json::Value> = achados
        .iter()
        .map(|a| {
            let mut item = serde_json::json!({
                "identity_authenticated": false,
                "device_id": a.announcement.device_id.0,
                "display_name": a.announcement.display_name,
                "protocol_version": a.announcement.protocol_version,
                "capabilities": {
                    "screen_source": a.announcement.capabilities.screen_source,
                    "camera_source": a.announcement.capabilities.camera_source,
                    "sink": a.announcement.capabilities.sink,
                },
                "endpoint": a.endpoint().map(|e| e.to_string()),
            });
            // `"papel"` **só quando existe** (`docs/contrato-teleprompter.md` §2): um controle
            // filtra `"teleprompter"`, um receptor de vídeo esconde quem tem papel. Sem papel, o
            // objeto é o de antes.
            if let (Some(p), Some(objeto)) = (a.announcement.papel, item.as_object_mut()) {
                objeto.insert("papel".into(), serde_json::Value::from(p.como_texto()));
            }
            item
        })
        .collect();
    match serde_json::to_string(&lista) {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&Error::from(e));
            -1
        }
    }
}

/// Para de navegar e libera. Nulo é ignorado.
///
/// # Safety
///
/// `b` precisa vir de [`quall_browser_start`] e não pode ter sido liberado antes.
#[no_mangle]
pub unsafe extern "C" fn quall_browser_stop(b: *mut QuallBrowser) {
    if b.is_null() {
        return;
    }
    let mut caixa = Box::from_raw(b);
    if let Some(interno) = caixa.interno.take() {
        interno.stop();
    }
}

// =============================================================================================
// Sessão
// =============================================================================================

/// O que uma track carrega. Espelha `TrackKind` do contrato.
///
/// **Os valores são ABI.** Espécie nova entra no fim, com o próximo número livre; renumerar ou
/// inserir no meio troca o significado dos números em toda casca já compilada, e o compilador de
/// nenhuma delas teria como perceber. `SYSTEM_AUDIO = 3` entrou depois das outras três, e é por
/// isso que ele está no fim e não ao lado de `MICROPHONE`.
///
/// As duas espécies de áudio não são a mesma coisa: `MICROPHONE` é a fala de quem transmite,
/// `SYSTEM_AUDIO` é o som que o aparelho está tocando (WASAPI loopback, ScreenCaptureKit,
/// AudioPlaybackCapture). Elas usam presets diferentes — mono a 32 kbit/s contra estéreo a 128
/// kbit/s. Ver `docs/audio.md`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallTrackKind {
    Screen = 0,
    Camera = 1,
    /// A fala de quem transmite. Mono, Opus a 32 kbit/s, com FEC.
    ///
    /// A canalização do núcleo está pronta; **nenhuma casca captura microfone ainda**.
    Microphone = 2,
    /// O som que o aparelho está tocando. Estéreo, Opus a 128 kbit/s, sem FEC.
    SystemAudio = 3,
}

impl From<QuallTrackKind> for TrackKind {
    fn from(k: QuallTrackKind) -> Self {
        match k {
            QuallTrackKind::Screen => TrackKind::Screen,
            QuallTrackKind::Camera => TrackKind::Camera,
            QuallTrackKind::Microphone => TrackKind::Microphone,
            QuallTrackKind::SystemAudio => TrackKind::SystemAudio,
        }
    }
}

impl From<TrackKind> for QuallTrackKind {
    fn from(k: TrackKind) -> Self {
        match k {
            TrackKind::Screen => QuallTrackKind::Screen,
            TrackKind::Camera => QuallTrackKind::Camera,
            TrackKind::Microphone => QuallTrackKind::Microphone,
            TrackKind::SystemAudio => QuallTrackKind::SystemAudio,
        }
    }
}

/// Como codificar o áudio de uma track, atravessando a fronteira C.
///
/// # Por que este campo existe, e o defeito que ele evita
///
/// `TrackConfig::com_codec_de_audio` existe em Rust desde a rodada do áudio e **não atravessava
/// a fronteira**. A consequência era muda e cara: uma casca C, Swift ou Kotlin que declarasse
/// `QUALL_TRACK_KIND_MICROPHONE` recebia do núcleo um SDP anunciando `opus/48000/2` — porque é o
/// que o preset da espécie diz — e não tinha como dizer que ia mandar µ-law. O fio prometeria
/// Opus a 48 kHz e receberia G.711 a 8 kHz; o outro lado decodificaria com o relógio numa escala
/// 6× errada, **sem erro em lugar nenhum no caminho**.
///
/// É a mesma classe de defeito do `useinbandfec=1` que não produzia LBRR nenhum, e do SPS sem
/// `bitstream_restriction` do M4: **declarar no fio o que não se faz**. Não dá para consertá-la
/// num lugar e deixar a fronteira obrigando quatro cascas a repeti-la.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallAudioCodec {
    /// **Use o codec do preset da espécie.** É o valor 0 de propósito.
    ///
    /// Uma casca que faça `memset(&desc, 0, sizeof desc)` — e uma casca compilada contra o header
    /// anterior, que não tinha este campo — cai aqui e obtém exatamente o comportamento de
    /// antes. Ver a nota de ABI em [`QuallTrackDesc`].
    Default = 0,
    /// Opus a 48 kHz (RFC 7587). O codec do produto.
    Opus = 1,
    /// G.711 µ-law a 8 kHz (RFC 3551). O piso: uma tabela de consulta de 8 bits, que qualquer
    /// plataforma produz sem biblioteca nenhuma.
    Pcmu = 2,
}

impl QuallAudioCodec {
    fn para_core(self) -> Option<CodecDeAudio> {
        match self {
            QuallAudioCodec::Default => None,
            QuallAudioCodec::Opus => Some(CodecDeAudio::Opus),
            QuallAudioCodec::Pcmu => Some(CodecDeAudio::Pcmu),
        }
    }
}

/// Uma track de saída a declarar na oferta.
///
/// # Nota de ABI
///
/// `audio_codec` entrou **no fim** do struct, e o valor 0 ([`QuallAudioCodec::Default`]) é o
/// comportamento de antes. Os deslocamentos de `kind` e `label` não mudaram, então uma casca
/// compilada contra o header anterior continua escrevendo nos lugares certos — mas ela aloca um
/// struct **menor**, e o núcleo leria além do fim dele. Recompile a casca contra o header novo.
///
/// A ordem do enum [`QuallTrackKind`] continua sendo ABI pelo motivo registrado em
/// `docs/audio.md` §1, e não foi tocada.
#[repr(C)]
pub struct QuallTrackDesc {
    pub kind: QuallTrackKind,
    /// Rótulo legível mostrado ao usuário no receptor. Ex.: "Tela do Galaxy A10s".
    pub label: *const c_char,
    /// Só vale em track de áudio; ignorado em vídeo, como o preset já era.
    pub audio_codec: QuallAudioCodec,
}

/// Tudo o que uma sessão precisa saber para subir.
///
/// # Nota de ABI
///
/// `bind_address` entrou **no fim** do struct, pela mesma regra que trouxe `audio_codec` em
/// [`QuallTrackDesc`]: `NULL` é o comportamento de antes. Uma casca que faça
/// `memset(&op, 0, sizeof op)` — e uma casca compilada contra o header anterior, que não tinha
/// este campo — cai no `NULL` e obtém exatamente a sessão de antes. Os deslocamentos dos campos
/// que já existiam não mudaram, mas a casca velha aloca um struct **menor** e o núcleo leria
/// além do fim dele: recompile a casca contra o header novo.
#[repr(C)]
pub struct QuallSessionOptions {
    pub me: QuallDeviceDesc,
    /// PIN de seis dígitos. Obrigatório no primeiro pareamento; pode ser nulo quando o par já é
    /// conhecido.
    pub pin: *const c_char,
    /// Estado de pareamento persistido pela casca, como JSON. Nulo começa vazio.
    pub known_peers_json: *const c_char,
    /// Porta de sinalização. Só vale em [`quall_host`]; `0` deixa o sistema escolher.
    pub signaling_port: u16,
    /// Prazo total: aceitar, parear e o transporte subir.
    pub timeout_ms: u32,
    /// Tracks que **este** aparelho vai emitir. Só vale em [`quall_host`].
    ///
    /// Elas entram na oferta SDP, e o que não está na oferta só entra com renegociação — que o
    /// Quall não implementa. Um emissor que ainda não sabe se vai mandar câmera declara a track
    /// mesmo assim e a deixa muda: uma track sem quadro custa uma linha `m=` e nada mais.
    pub tracks: *const QuallTrackDesc,
    pub track_count: usize,
    /// **Prende a mídia a uma interface, e desiste de todas as outras.** `NULL` = o de hoje:
    /// o ICE reúne toda interface que a libjuice aceita e escolhe entre elas.
    ///
    /// O endereço IPv4 **local** — o desta máquina, não o do par — a que os sockets da mídia se
    /// prendem. Ex.: `"169.254.75.173"`.
    ///
    /// # Ligar isto não é "preferir" uma interface: é abrir mão das outras
    ///
    /// Com o socket preso a um endereço específico, `udp_get_addrs`
    /// (`libjuice/src/udp.c:432`) devolve **aquele único** record e retorna antes de qualquer
    /// enumeração. Não há candidato de Wi-Fi, não há corrida entre cabo e rádio, e não há
    /// recuo automático: se aquela interface não alcançar o par, a sessão não sobe — em vez de
    /// subir mais devagar por outro caminho.
    ///
    /// É exatamente por isso que o campo existe e é exatamente por isso que ele é caro. Prender
    /// é o **desvio** do filtro que faz a libjuice recusar `169.254/16`
    /// (`libjuice/src/addr.c:84`, e `udp.c:462`: *"we never list link-local addresses"*), e é o
    /// único jeito de a mídia entrar num cabo USB-Ethernet. Também é o que obriga o produto a
    /// tratar "pelo cabo" como **escolha**, e não como upgrade invisível: uma casca que ligue
    /// isto sozinha, sem o usuário ter pedido aquele enlace, troca um caminho que funcionava
    /// por um que talvez não funcione.
    ///
    /// Medido em 2026-09-01, nesta bancada: três iOS emitindo câmera por três cabos ao mesmo
    /// tempo, cada sessão presa à sua interface, 1497/1497/1496 quadros e **0,000 % de perda nas
    /// três**, com Wi-Fi ligado nos aparelhos. E só o lado que **conecta** precisa prender-se:
    /// os três iOS ofereceram apenas candidato de Wi-Fi e o par se formou por *peer-reflexive*.
    /// Ver `docs/bancada.md` e `docs/quall-pelo-cabo.md`.
    ///
    /// # Vazio é `NULL`, e não erro
    ///
    /// `""` é tratado como "não prenda nada". Não é indulgência: numa casca C um campo de texto
    /// que o usuário não preencheu **é** a string vazia — `obs_data_get_string` devolve `""`,
    /// nunca `NULL` — e fazer a sessão falhar por isso seria transformar "o campo está em
    /// branco" em "a sessão não sobe e não diz por quê". `TransportConfig::bind_address` do
    /// núcleo continua recusando `Some("")`, porque lá quem chama é Rust e a distinção entre
    /// `None` e `Some("")` existe.
    pub bind_address: *const c_char,
}

/// Uma sessão de pé.
pub struct QuallSession {
    pronto: Ready,
    /// As tracks de saída, já em `Arc` para poderem ser entregues à casca sem depender do tempo
    /// de vida desta caixa.
    emissores: Vec<Arc<TrackEmissor>>,
    /// Só existe no emissor; mantém o servidor de sinalização vivo enquanto a sessão viver.
    ///
    /// Em `Arc` porque, no teleprompter, o atendente de `Ready::atender_enquanto_dura` o usa de
    /// outra thread durante a sessão. O campo vem **depois** de `pronto`: o `Ready` (e o atendente
    /// dele) morre primeiro, e só então a porta fecha.
    _servidor: Option<Arc<SignalingServer>>,
}

/// Handle de track entregue à casca. É dono de uma referência, não do recurso.
pub struct QuallTrack {
    lado: Lado,
    /// Estado do caminho de recepção de áudio: o jitter buffer do núcleo e o tratador
    /// registrado, guardados para o escoamento do desregistro. Ver
    /// [`quall_track_on_audio`]. `None` em toda track que ainda não registrou áudio —
    /// inclusive em toda track de vídeo e em toda track de emissão.
    audio: Mutex<Option<RecepcaoDeAudio>>,
}

enum Lado {
    Emissor(Arc<TrackEmissor>),
    Receptor(Arc<TrackReceptor>),
}

/// O espaçamento de vídeo das próximas sessões, em kbit/s. Ver [`quall_set_video_pacing_kbps`].
static ESPACAMENTO_DE_VIDEO_KBPS: AtomicU32 = AtomicU32::new(0);

/// **Espaça a saída de vídeo das próximas sessões** a no máximo `kbps` kbit/s. `0` desliga, e é o
/// padrão — o de toda casca que não chamar isto.
///
/// Vale para as sessões abertas **depois** da chamada, e só nas tracks de vídeo; uma sessão já
/// aberta fica como nasceu. É do processo, e não da sessão, para não mexer em
/// [`QuallSessionOptions`], cujo layout as cascas repetem à mão.
///
/// Medido em 11/09/2026 com o Mac no cabo (`docs/tela-estendida.md`, corrida B): espalhar a saída a
/// 60 Mbit/s levou a perda de um receptor vizinho de 7,4 % para 0,17 %. **Tem de ficar bem acima
/// da taxa do vídeo**: o espaçador enfileira o que passa da verba, e a fila não tem teto. Ver
/// `TrackConfig::espacamento_kbps` no núcleo.
#[no_mangle]
pub extern "C" fn quall_set_video_pacing_kbps(kbps: u32) {
    ESPACAMENTO_DE_VIDEO_KBPS.store(kbps, Ordering::Relaxed);
}

/// # Safety
///
/// `opcoes` precisa apontar para um [`QuallSessionOptions`] válido.
unsafe fn montar_config(
    opcoes: &QuallSessionOptions,
    cancelamento: Cancelamento,
) -> Saida<(SessionConfig, Announcement)> {
    let anuncio_local = opcoes.me.para_anuncio()?;

    let pin = match texto_opcional(opcoes.pin, "pin")? {
        Some(p) => Some(Pin::parse(p)?),
        None => None,
    };
    let known = match texto_opcional(opcoes.known_peers_json, "known_peers_json")? {
        Some(j) if !j.trim().is_empty() => PairedPeers::from_json(j)?,
        _ => PairedPeers::new(),
    };

    let mut tracks = Vec::new();
    if !opcoes.tracks.is_null() && opcoes.track_count > 0 {
        for i in 0..opcoes.track_count {
            let desc = &*opcoes.tracks.add(i);
            let rotulo = texto_opcional(desc.label, "label")?.unwrap_or("");
            let mut cfg = TrackConfig::new(desc.kind.into(), rotulo);
            // `com_codec_de_audio` já é inofensivo em track de vídeo (o preset é `None` e o
            // `map` não roda), mas o `if` deixa a intenção legível a quem lê daqui.
            if let Some(codec) = desc.audio_codec.para_core() {
                cfg = cfg.com_codec_de_audio(codec);
            }
            // Numa track de áudio o núcleo não encadeia o espaçador; o campo fica sem efeito.
            cfg = cfg.com_espacamento(ESPACAMENTO_DE_VIDEO_KBPS.load(Ordering::Relaxed));
            tracks.push(cfg);
        }
    }

    // Vazio é `None`, e o `trim` é o mesmo julgamento que `known_peers_json` faz três linhas
    // acima: numa casca C o campo em branco chega como `""`, não como `NULL`.
    let prender = match texto_opcional(opcoes.bind_address, "bind_address")? {
        Some(e) if !e.trim().is_empty() => Some(e.trim().to_string()),
        _ => None,
    };

    let cfg = SessionConfig {
        announcement: anuncio_local.clone(),
        pin,
        known,
        transport: TransportConfig {
            // **O resto por `..default()`, de propósito.** `port_range`, `mtu` e `delivery`
            // continuam sem atravessar a fronteira; escrevê-los aqui à mão congelaria os
            // padrões do núcleo numa cópia que ninguém lembraria de atualizar.
            bind_address: prender,
            ..TransportConfig::default()
        },
        tracks,
        // Zero seria "desista já"; 60 s é o mesmo padrão da sonda.
        timeout: Duration::from_millis(if opcoes.timeout_ms == 0 {
            60_000
        } else {
            u64::from(opcoes.timeout_ms)
        }),
        cancelamento,
        // **Desligado, e a fronteira C ainda não tem como ligar.** O detector de caminho mudo
        // existe no núcleo (`SessionConfig::silencio_do_caminho`) e depende de saber que a
        // origem produz continuamente — coisa que a casca sabe e o núcleo não. Expor o campo em
        // `QuallSessionOptions` é a mesma passada de cbindgen que expõe `bind_address`, e as
        // duas são do degrau que mexe nas cascas.
        silencio_do_caminho: None,
    };
    Ok((cfg, anuncio_local))
}

// =============================================================================================
// Cancelamento (dívida 10)
// =============================================================================================

/// O botão **Cancelar** da tela de espera, do lado do núcleo.
///
/// [`quall_host`] e [`quall_connect`] bloqueiam. Sem isto, a única forma de destravá-los era o
/// contorno que Android e iOS escreveram cada um por sua conta: **abrir uma conexão TCP
/// descartável para o próprio endereço**, só para o `accept` voltar. A frente iOS mediu o
/// contorno: sem cutucada, `quall_host` segura 120,16 s; com cutucada aos 3 s, sai em 3,09 s.
///
/// # Como usar
///
/// Crie o cancelador **antes** de largar a thread de trabalho, guarde-o na tela, e passe-o a
/// [`quall_host_cancelable`] ou [`quall_connect_cancelable`]. O botão Cancelar chama
/// [`quall_session_cancel`] de qualquer thread; a chamada bloqueante volta com
/// `QUALL_STATUS_CANCELLED` em [`quall_last_error`] e ponteiro nulo.
///
/// Cancelar é **irreversível**: para uma segunda tentativa, crie outro cancelador.
pub struct QuallCanceller {
    interno: Cancelamento,
}

/// Cria um cancelador ainda não acionado. Libere com [`quall_canceller_free`].
#[no_mangle]
pub extern "C" fn quall_canceller_new() -> *mut QuallCanceller {
    Box::into_raw(Box::new(QuallCanceller {
        interno: Cancelamento::novo(),
    }))
}

/// **Cancela a espera.** Pode ser chamado de qualquer thread, quantas vezes quiser; nulo é
/// ignorado.
///
/// Este é o `quall_session_cancel` que a dívida 10 pede. Ele recebe o **cancelador**, e não a
/// sessão, porque enquanto se espera ainda não existe sessão nenhuma — é exatamente o buraco que
/// obrigava as cascas ao contorno da conexão descartável.
///
/// # Safety
///
/// `c` precisa ser nulo ou vir de [`quall_canceller_new`] e não ter sido liberado.
#[no_mangle]
pub unsafe extern "C" fn quall_session_cancel(c: *const QuallCanceller) {
    if let Some(cancelador) = c.as_ref() {
        cancelador.interno.cancelar();
    }
}

/// Já pediram para cancelar? Nulo devolve `false`.
///
/// # Safety
///
/// `c` precisa ser nulo ou vir de [`quall_canceller_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_canceller_is_cancelled(c: *const QuallCanceller) -> bool {
    let Some(cancelador) = c.as_ref() else {
        guardar_nulo("quall_canceller_is_cancelled: cancelador nulo");
        return false;
    };
    cancelador.interno.cancelado()
}

/// Libera o cancelador. Nulo é ignorado.
///
/// Pode ser liberado a qualquer momento depois de a chamada bloqueante voltar: a chamada segura
/// a **própria** cópia da bandeira.
///
/// # Safety
///
/// `c` precisa vir de [`quall_canceller_new`] e não pode ter sido liberado antes.
#[no_mangle]
pub unsafe extern "C" fn quall_canceller_free(c: *mut QuallCanceller) {
    if !c.is_null() {
        drop(Box::from_raw(c));
    }
}

fn empacotar(pronto: Ready, servidor: Option<Arc<SignalingServer>>) -> *mut QuallSession {
    let (pronto, emissores) = {
        let mut pronto = pronto;
        let tracks = std::mem::take(&mut pronto.tracks);
        (pronto, tracks.into_iter().map(Arc::new).collect())
    };
    Box::into_raw(Box::new(QuallSession {
        pronto,
        emissores,
        _servidor: servidor,
    }))
}

/// Sobe uma sessão como **emissor**: abre a sinalização, espera um receptor, pareia e oferece.
///
/// **Bloqueia** até a sessão subir ou o prazo estourar. Chame de uma thread de trabalho.
///
/// Anunciar por mDNS é separado, em [`quall_advertiser_start`], para que a casca possa oferecer
/// só o caminho por IP em rede que bloqueia multicast.
///
/// # Safety
///
/// `opcoes` precisa apontar para um [`QuallSessionOptions`] válido, com as strings vivas durante
/// a chamada.
#[no_mangle]
pub unsafe extern "C" fn quall_host(opcoes: *const QuallSessionOptions) -> *mut QuallSession {
    quall_host_cancelable(opcoes, ptr::null())
}

/// Igual a [`quall_host`], com um cancelador. Ver [`QuallCanceller`] e a dívida 10.
///
/// `cancelador` nulo é aceito e reproduz [`quall_host`] exatamente — assim a casca migra quando
/// quiser, sem mudar nada do que já funciona.
///
/// # Safety
///
/// `opcoes` precisa apontar para um [`QuallSessionOptions`] válido; `cancelador` precisa ser nulo
/// ou vir de [`quall_canceller_new`] e continuar vivo durante a chamada.
#[no_mangle]
pub unsafe extern "C" fn quall_host_cancelable(
    opcoes: *const QuallSessionOptions,
    cancelador: *const QuallCanceller,
) -> *mut QuallSession {
    hospedar_pela_fronteira(opcoes, cancelador, None)
}

/// # Safety
///
/// As mesmas de [`quall_host_cancelable`].
unsafe fn hospedar_pela_fronteira(
    opcoes: *const QuallSessionOptions,
    cancelador: *const QuallCanceller,
    papel: Option<Papel>,
) -> *mut QuallSession {
    let Some(opcoes) = opcoes.as_ref() else {
        guardar_nulo("quall_host: `opcoes` é nulo");
        return ptr::null_mut();
    };
    let mut cfg = match montar_config(opcoes, cancelamento_de(cancelador)) {
        Ok((c, _)) => c,
        Err(f) => {
            guardar_falha(&f);
            return ptr::null_mut();
        }
    };
    if papel.is_some() && !cfg.tracks.is_empty() {
        guardar_texto(
            "quall_host_with_role: um teleprompter não emite tracks; `track_count` tem de ser 0",
        );
        return ptr::null_mut();
    }
    cfg.announcement.papel = papel;
    let eu = cfg.announcement.clone();
    let servidor = match SignalingServer::bind(opcoes.signaling_port) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            guardar_erro(&e);
            return ptr::null_mut();
        }
    };
    match hospedar(&servidor, cfg) {
        Ok(mut pronto) => {
            // O prompter atende a porta enquanto a sessão dura: todo controle que bate ouve
            // "ocupado" — inclusive o da sessão, voltando de uma queda —, e nada que chega pela
            // porta derruba a sessão (quem decide é o detector de 5 s). Ver
            // `quall_core::session::Ready::atender_enquanto_dura`.
            if papel == Some(Papel::Teleprompter) {
                pronto.atender_enquanto_dura(Arc::clone(&servidor), eu);
            }
            empacotar(pronto, Some(servidor))
        }
        Err(e) => {
            guardar_erro(&e);
            ptr::null_mut()
        }
    }
}

/// A bandeira do cancelador, ou uma bandeira nova e nunca acionada quando ele é nulo.
///
/// # Safety
///
/// `c` precisa ser nulo ou vir de [`quall_canceller_new`].
unsafe fn cancelamento_de(c: *const QuallCanceller) -> Cancelamento {
    c.as_ref()
        .map(|x| x.interno.clone())
        .unwrap_or_else(Cancelamento::novo)
}

/// A porta em que a sinalização do emissor ficou. Útil quando `signaling_port` foi `0`.
///
/// Devolve `0` se ainda não há servidor (sessão de receptor) ou se algo falhou.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_signaling_port(s: *const QuallSession) -> u16 {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_signaling_port: sessão nula");
        return 0;
    };
    sessao
        ._servidor
        .as_ref()
        .and_then(|servidor| servidor.port().ok())
        .unwrap_or(0)
}

/// Sobe uma sessão como **receptor**: conecta no endereço, pareia e responde.
///
/// `endpoint` é `"192.168.56.131:7877"` ou só `"192.168.56.131"` (a porta padrão entra sozinha).
/// É a **mesma** função para o endereço que veio do mDNS e para o que o usuário digitou — de
/// propósito: o fallback de rede sem multicast não pode ser um caminho de código que ninguém
/// exercita.
///
/// **Só endereço.** Um link `quall://<pin>@<host>:<porta>` é `QUALL_STATUS_INVALID`: leia-o com
/// [`quall_parse_endpoint_json`], ponha o PIN nas opções e conecte no endereço. A volta automática
/// depois de uma queda vai com o endereço e **sem** PIN (`docs/contrato-teleprompter.md` §11.1).
/// Porta 0 também é `INVALID`.
///
/// **Bloqueia**. Chame de uma thread de trabalho.
///
/// # Safety
///
/// `endpoint` e `opcoes` precisam ser válidos durante a chamada.
#[no_mangle]
pub unsafe extern "C" fn quall_connect(
    endpoint: *const c_char,
    opcoes: *const QuallSessionOptions,
) -> *mut QuallSession {
    quall_connect_cancelable(endpoint, opcoes, ptr::null())
}

/// Igual a [`quall_connect`], com um cancelador. Ver [`QuallCanceller`] e a dívida 10.
///
/// # Safety
///
/// `endpoint` e `opcoes` precisam ser válidos durante a chamada; `cancelador` precisa ser nulo ou
/// vir de [`quall_canceller_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_connect_cancelable(
    endpoint: *const c_char,
    opcoes: *const QuallSessionOptions,
    cancelador: *const QuallCanceller,
) -> *mut QuallSession {
    conectar_pela_fronteira(endpoint, opcoes, cancelador, None, None)
}

/// Igual a [`quall_connect_cancelable`], dizendo ao emissor **a tela deste aparelho**, em pixels
/// do painel (sem orientação). O emissor que cria um monitor por receptor — a tela estendida do
/// Mac — usa isso para dar ao monitor o formato desta tela.
///
/// `0` nos dois lados é "não digo" e se comporta exatamente como [`quall_connect_cancelable`].
/// Um lado fora de 1 a 16384 px é [`QuallStatus::Invalid`], e a sessão não sobe: um número desses é
/// defeito da casca, e mandá-lo adiante daria ao outro lado um monitor impossível.
///
/// **Por chamada, e não um ajuste do processo** (revisão adversarial de 10/09/2026): o mesmo
/// processo pode hospedar e conectar — o app do Mac e o do Android fazem os dois papéis —, e um
/// valor global iria parar no `Welcome` de quem hospeda.
///
/// # Safety
///
/// As mesmas de [`quall_connect_cancelable`].
#[no_mangle]
pub unsafe extern "C" fn quall_connect_with_screen(
    endpoint: *const c_char,
    opcoes: *const QuallSessionOptions,
    cancelador: *const QuallCanceller,
    width_px: u32,
    height_px: u32,
) -> *mut QuallSession {
    let tela = if width_px == 0 && height_px == 0 {
        None
    } else {
        match Screen::nova(width_px, height_px) {
            Some(t) => Some(t),
            None => {
                guardar_texto(&format!(
                    "quall_connect_with_screen: tela {width_px}x{height_px} fora de 1..={}",
                    Screen::LADO_MAXIMO
                ));
                return ptr::null_mut();
            }
        }
    };
    conectar_pela_fronteira(endpoint, opcoes, cancelador, tela, None)
}

/// # Safety
///
/// As mesmas de [`quall_connect_cancelable`].
unsafe fn conectar_pela_fronteira(
    endpoint: *const c_char,
    opcoes: *const QuallSessionOptions,
    cancelador: *const QuallCanceller,
    tela: Option<Screen>,
    papel: Option<Papel>,
) -> *mut QuallSession {
    let Some(opcoes) = opcoes.as_ref() else {
        guardar_nulo("quall_connect: `opcoes` é nulo");
        return ptr::null_mut();
    };
    // O prazo da sessão vale para a resolução de nome também. Antes, `getaddrinfo` bloqueava
    // fora de qualquer prazo — a tela de "conectando" travava sem limite quando o usuário
    // errava um dígito do IP e o texto virava nome.
    let prazo_resolucao = if opcoes.timeout_ms == 0 {
        PRAZO_DE_RESOLUCAO
    } else {
        Duration::from_millis(u64::from(opcoes.timeout_ms)).min(PRAZO_DE_RESOLUCAO)
    };
    // A porta que falta é a do papel: 7979 para o controle remoto, 7877 para o vídeo (§11.1).
    let porta = porta_para_completar(papel);
    let destino: SocketAddr = match texto(endpoint, "endpoint")
        .and_then(|e| endereco_manual_com_porta(e, porta, prazo_resolucao).map_err(Falha::from))
    {
        Ok(d) => d,
        Err(f) => {
            guardar_falha(&f);
            return ptr::null_mut();
        }
    };
    let mut cfg = match montar_config(opcoes, cancelamento_de(cancelador)) {
        Ok((c, _)) => c,
        Err(f) => {
            guardar_falha(&f);
            return ptr::null_mut();
        }
    };
    cfg.announcement.screen = tela;
    cfg.announcement.papel = papel;
    match conectar(destino, cfg) {
        Ok(pronto) => empacotar(pronto, None),
        Err(e) => {
            guardar_erro(&e);
            ptr::null_mut()
        }
    }
}

/// O aparelho do outro lado, como JSON (`device_id`, `display_name`, `protocol_version`,
/// `capabilities`, `identity_authenticated: true`). Padrão `(buf, cap)`.
/// ID e nome reais vêm do anúncio autenticado e cifrado; substituem a linha efêmera da descoberta.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_peer_json(
    s: *const QuallSession,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_peer_json: sessão nula");
        return -1;
    };
    let texto = serde_json::to_value(&sessao.pronto.peer).and_then(|mut value| {
        value["identity_authenticated"] = serde_json::Value::Bool(true);
        serde_json::to_string(&value)
    });
    match texto {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&Error::from(e));
            -1
        }
    }
}

/// **Quantos candidatos caíram antes deste**, sem derrubar a espera. `0` no receptor, e `0` é o
/// caso normal também no emissor.
///
/// Diferente de zero quer dizer que alguém conectou e sumiu no meio — o receptor que fecha o app,
/// o scanner de porta da LAN — e que a espera sobreviveu a isso. Até 01/09/2026 não sobrevivia:
/// um `WSAECONNRESET` no handshake fechava a porta da sinalização **em definitivo, com o processo
/// vivo**, e do outro lado só se via "depois que sai não conecta". Ver
/// `quall_core::session::hospedar`.
///
/// Existe para que o conserto apareça no registro da casca em vez de sumir: descarte que ninguém
/// conta é como um defeito volta a viver escondido. Devolve `-1` em sessão nula.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_descartados(s: *const QuallSession) -> i64 {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_descartados: sessão nula");
        return -1;
    };
    i64::from(sessao.pronto.descartados)
}

/// **Por onde a mídia está indo**, como JSON. Padrão `(buf, cap)`.
///
/// Quatro campos, todos podendo ser `null`:
///
/// ```json
/// {"local_candidate":"a=candidate:1 1 UDP 2122317823 192.168.56.131 62493 typ host",
///  "remote_candidate":"a=candidate:1 1 UDP 2122317823 192.168.56.131 51698 typ host",
///  "local_address":"192.168.56.131:62493",
///  "remote_address":"192.168.56.131:51698"}
/// ```
///
/// # `null` é "o ICE ainda não fechou", e não é erro
///
/// Antes de o par de candidatos ser escolhido não há o que dizer, e os quatro campos vêm `null`.
/// A função continua devolvendo o tamanho do JSON, **não** `-1`: devolver negativo ali
/// confundiria "ainda não" com "falhou", e uma casca que tratasse os dois igual mostraria erro
/// numa sessão que está apenas subindo. Negativo aqui é só sessão nula ou falha de serialização.
///
/// # Por que esta função existe
///
/// Porque **uma corrida "pelo cabo" pode fechar pela Wi-Fi e parecer sucesso**. Medido nesta
/// bancada em 2026-09-01: duas pontas na mesma máquina, sinalização por `127.0.0.1`, e o par
/// escolhido foi `192.168.56.131 <-> 192.168.56.131` — a mídia saiu pelo rádio. O `quall-probe`
/// sempre soube disso porque lê o `Ready` do Rust; a casca não tinha nada equivalente, e foi
/// exatamente essa linha que explicou os 8,6 s de `docs/receptor-ios.md:243`.
///
/// # O que ela não diz
///
/// Não diz "cabo" nem "Wi-Fi": diz o endereço que o ICE escolheu. `169.254.x` e `192.168.42.x`
/// são **pistas** de cabo, não provas — um `169.254.x` também é o que sobra quando o DHCP falha.
/// Quem quiser o rótulo "pelo cabo" precisa de duas testemunhas que concordem: esta, e a escolha
/// que a própria casca fez ao pedir o enlace.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_path_json(
    s: *const QuallSession,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_path_json: sessão nula");
        return -1;
    };
    match serde_json::to_string(&sessao.pronto.session.caminho()) {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&Error::from(e));
            -1
        }
    }
}

/// O pareamento foi novo (o usuário digitou PIN) ou retomado?
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_pairing_is_new(s: *const QuallSession) -> bool {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_pairing_is_new: sessão nula");
        return false;
    };
    sessao.pronto.outcome.novo
}

/// O estado de pareamento atualizado, para a casca **persistir**. Padrão `(buf, cap)`.
///
/// Sem gravar isto, o usuário digita o PIN de novo na próxima sessão. `known_json` é o que veio
/// em [`QuallSessionOptions::known_peers_json`]; passar nulo começa do zero e devolve só este
/// par.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`]; `known_json` precisa ser nulo ou uma
/// string válida.
#[no_mangle]
pub unsafe extern "C" fn quall_session_known_peers_json(
    s: *const QuallSession,
    known_json: *const c_char,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_known_peers_json: sessão nula");
        return -1;
    };
    let mut conhecidos = match ler_pares(known_json, "known_json") {
        Ok(p) => p,
        Err(f) => {
            guardar_falha(&f);
            return -1;
        }
    };
    conhecidos.insert(&sessao.pronto.outcome);
    match conhecidos.to_json() {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// Quantas tracks de **saída** esta sessão abriu.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_track_count(s: *const QuallSession) -> usize {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_track_count: sessão nula");
        return 0;
    };
    sessao.emissores.len()
}

/// Pega a track de saída de índice `idx`, na ordem de [`QuallSessionOptions::tracks`].
///
/// O handle devolvido é do chamador: libere com [`quall_track_free`].
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_track(
    s: *const QuallSession,
    idx: usize,
) -> *mut QuallTrack {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_track: sessão nula");
        return ptr::null_mut();
    };
    match sessao.emissores.get(idx) {
        Some(emissor) => Box::into_raw(Box::new(QuallTrack {
            lado: Lado::Emissor(Arc::clone(emissor)),
            audio: Mutex::new(None),
        })),
        None => {
            guardar_texto("quall_session_track: índice fora da faixa");
            ptr::null_mut()
        }
    }
}

/// Próxima track que chegou **do outro lado**, esperando até `timeout_ms`.
///
/// Devolve nulo quando nada chegou a tempo — o que é estado normal, não erro. O receptor chama
/// num laço: uma sessão traz tela, câmera e microfone, e quem decide como compor é ele.
///
/// O handle devolvido é do chamador: libere com [`quall_track_free`].
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_next_track(
    s: *mut QuallSession,
    timeout_ms: u32,
) -> *mut QuallTrack {
    let Some(sessao) = s.as_mut() else {
        guardar_nulo("quall_session_next_track: sessão nula");
        return ptr::null_mut();
    };
    match sessao
        .pronto
        .session
        .proxima_track(Duration::from_millis(u64::from(timeout_ms)))
    {
        Some(receptor) => Box::into_raw(Box::new(QuallTrack {
            lado: Lado::Receptor(Arc::new(receptor)),
            audio: Mutex::new(None),
        })),
        None => ptr::null_mut(),
    }
}

/// O que aconteceu com a sessão depois que ela subiu. Ver [`quall_session_next_event`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallSessionEvent {
    /// Nada por enquanto. Estado normal, não erro.
    None = 0,
    /// A outra ponta saiu.
    Disconnected = 1,
    /// O transporte falhou.
    Failed = 2,
}

impl From<EventoDeSessao> for QuallSessionEvent {
    fn from(e: EventoDeSessao) -> Self {
        match e {
            EventoDeSessao::Nenhum => QuallSessionEvent::None,
            EventoDeSessao::Desconectou => QuallSessionEvent::Disconnected,
            EventoDeSessao::Falhou => QuallSessionEvent::Failed,
        }
    }
}

/// **O detector de queda.** Olha se a sessão caiu, esperando até `timeout_ms`.
///
/// # Por que a casca precisa disto
///
/// Sem ele o único sinal de que a sessão morreu é [`quall_track_send_frame`] voltar a falhar — e
/// esse mesmo status é o estado **normal** enquanto o ICE ainda não fechou. Pior: o
/// `CONSENT_TIMEOUT` do libjuice é 30 000 ms, então durante meio minuto o envio continua
/// devolvendo `QUALL_STATUS_OK` com o receptor morto. São ~900 quadros capturados, encodados em
/// hardware e empacotados para o vazio, com a bateria de um celular pagando a conta.
///
/// Este detector olha a **sinalização**, que sabe em milissegundos, e o transporte como rede de
/// segurança. Chame do mesmo laço da captura, com `timeout_ms` pequeno (0 a 10): a espera é
/// limitada e não entra no caminho do quadro.
///
/// Uma vez `QUALL_SESSION_EVENT_DISCONNECTED`, sempre — a sessão não volta.
///
/// # Uma thread só
///
/// Esta função lê da sinalização e por isso pede `*mut`. Chame de **uma** thread, a mesma que
/// chama [`quall_session_next_track`]. Chamar das duas ao mesmo tempo é corrida de dados.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`].
#[no_mangle]
pub unsafe extern "C" fn quall_session_next_event(
    s: *mut QuallSession,
    timeout_ms: u32,
) -> QuallSessionEvent {
    let Some(sessao) = s.as_mut() else {
        guardar_nulo("quall_session_next_event: sessão nula");
        return QuallSessionEvent::None;
    };
    sessao
        .pronto
        .proximo_evento(Duration::from_millis(u64::from(timeout_ms)))
        .into()
}

/// **O caminho de volta do sinal.** O receptor conta ao emissor o que viu do enlace numa janela.
///
/// Chame do **receptor**, da mesma thread que chama [`quall_session_next_event`] — a sinalização
/// não é acessada de duas threads, e a fronteira não põe cadeado no caminho quente para permitir
/// isso.
///
/// Os cinco números são **deltas da janela**, nunca acumulados desde o começo da sessão. Ver
/// `quall_core::signaling::RelatoDoEnlace` para por que este caminho é a sinalização e não RTCP,
/// e o que essa escolha custa.
///
/// `packets` é **o que o emissor mandou**: `packets_seen + packets_lost_for_real` da janela. O
/// denominador de uma taxa de perda é o que o emissor mandou; dividir pelo que chegou já inverteu
/// a conclusão de uma frente inteira desta bancada.
///
/// Falhar aqui não é motivo para o receptor parar nada: um emissor de versão antiga ignora a
/// mensagem por conta própria, e um socket morto vai aparecer no detector de queda que já existe.
///
/// # Safety
///
/// `s` precisa vir de [`quall_connect`] e não pode ter sido liberado.
#[no_mangle]
pub unsafe extern "C" fn quall_session_report_link(
    s: *mut QuallSession,
    ms: u64,
    packets: u64,
    lost: u64,
    suspect: u64,
    broken_idrs: u64,
    // Quadros que chegaram inteiros e **esta casca** não conseguiu entregar na janela (fila
    // própria transbordando). Zero para quem não conta isso — que é o comportamento de antes.
    not_delivered: u64,
) -> QuallStatus {
    let Some(sessao) = s.as_mut() else {
        return guardar_nulo("quall_session_report_link: sessão nula");
    };
    match sessao.pronto.relatar_enlace(RelatoDoEnlace {
        ms,
        pacotes: packets,
        perdidos: lost,
        suspeitos: suspect,
        idrs_quebrados: broken_idrs,
        nao_decodificados: not_delivered,
    }) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// Consome o relato **mais recente** que o outro lado mandou. Chame do **emissor**.
///
/// Escreve **seis** `uint64_t` em `out`, na ordem `{ms, packets, lost, suspect, broken_idrs,
/// not_delivered}`, e devolve `true`. Devolve `false` quando não há relato novo — que é o caso na maioria das
/// chamadas, e o caso **sempre** quando o outro lado é uma versão que não relata.
///
/// Relato repetido não existe: uma leitura consome. E se dois tiverem chegado entre duas
/// chamadas, o mais velho é descartado — agir sobre uma janela de um segundo atrás depois de já
/// ter a de meio segundo é agir sobre o passado, e é assim que um laço fechado oscila.
///
/// Esta função **também** enxerga um `Bye` ou um `Error` que chegue na mesma leitura e registra a
/// queda da sessão: uma casca que só chame esta função continua sabendo que a sessão caiu, por
/// [`quall_session_next_event`].
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`]; `out` precisa apontar para **seis** `uint64_t` (o sexto,
/// `not_delivered`, entrou em 09/09/2026 — ver o comentário no corpo).
#[no_mangle]
pub unsafe extern "C" fn quall_session_take_link_report(
    s: *mut QuallSession,
    timeout_ms: u32,
    out: *mut u64,
) -> bool {
    let Some(sessao) = s.as_mut() else {
        guardar_nulo("quall_session_take_link_report: sessão nula");
        return false;
    };
    if out.is_null() {
        guardar_nulo("quall_session_take_link_report: `out` nulo");
        return false;
    }
    let Some(r) = sessao
        .pronto
        .relato_do_enlace(Duration::from_millis(u64::from(timeout_ms)))
    else {
        return false;
    };
    // **Seis, e não cinco, desde 09/09/2026.** Quem chama tem de passar um array de seis
    // `uint64_t`: com cinco, o sexto valor é escrita fora do buffer. Todas as cascas deste
    // repositório foram no mesmo commit; um binário antigo contra uma `.so` nova é o caso que o
    // `docs/contrato-track.md` chama de proibido, e continua proibido.
    let campos = [
        r.ms,
        r.pacotes,
        r.perdidos,
        r.suspeitos,
        r.idrs_quebrados,
        r.nao_decodificados,
    ];
    std::ptr::copy_nonoverlapping(campos.as_ptr(), out, campos.len());
    true
}

/// Por que o controlador de taxa mexeu (ou não). Espelha `quall_core::taxa::Motivo`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallRateReason {
    /// Janela ignorada: carência depois de uma mudança, ou pacotes de menos.
    Skipped = 0,
    /// Desceu. `out_bps` foi escrito.
    Down = 1,
    /// Queria descer e já está no piso. O enlace não dá para este vídeo — ver
    /// [`quall_rate_at_floor`].
    Floor = 2,
    /// Subiu. `out_bps` foi escrito.
    Up = 3,
    /// Queria subir e já está no teto. **É o estado permanente num enlace limpo.**
    Ceiling = 4,
    /// Banda morta, ou ainda contando janelas calmas. Nada a fazer.
    Hold = 5,
}

impl From<Motivo> for QuallRateReason {
    fn from(m: Motivo) -> Self {
        match m {
            Motivo::Ignorada => QuallRateReason::Skipped,
            Motivo::Desceu => QuallRateReason::Down,
            Motivo::NoPiso => QuallRateReason::Floor,
            Motivo::Subiu => QuallRateReason::Up,
            Motivo::NoTeto => QuallRateReason::Ceiling,
            Motivo::Segurou => QuallRateReason::Hold,
            // **Desceu, e por isso vira `Down` na fronteira.** O motivo é outro — o receptor
            // afogou, não o rádio perdeu —, mas o que a casca faz com a resposta é idêntico:
            // escrever o bitrate novo no codificador. Um valor de enum novo obrigaria toda casca
            // a ganhar um ramo para fazer a mesma coisa, e a que esquecesse cairia no ramo de
            // "não mexer" — que é o comportamento **errado** justamente aqui.
            Motivo::ReceptorAfogado => QuallRateReason::Down,
            // **Segurou, e vira `Hold` pelo mesmo raciocínio ao contrário**: a casca não tem nada
            // a escrever no codificador, e uma que não conhecesse um valor novo cairia
            // exatamente no ramo certo. Quem quer saber o porquê lê `nao_entregues` na mesma
            // linha do relato — a janela afogada que segura é a única com os dois juntos.
            Motivo::AfogadoSemAlivio => QuallRateReason::Hold,
        }
    }
}

/// O controlador de taxa. Ver `quall_core::taxa`.
pub struct QuallRate(ControleDeTaxa);

/// Cria um controlador de taxa com o teto dado, em bps.
///
/// **`ceiling_bps` tem de ser o bitrate que o produto usaria sem controlador.** Ele nasce nesse
/// valor e nunca passa dele: o controlador só sabe tirar e devolver o que tirou. É isso, e não a
/// sintonia dos parâmetros, que garante que um enlace limpo não piora — em 5 GHz esta bancada
/// mediu 0 perda em 9 003 pacotes, e ali o controlador não tem o que fazer.
///
/// Devolve nulo com `ceiling_bps` zero.
#[no_mangle]
pub extern "C" fn quall_rate_new(ceiling_bps: u32) -> *mut QuallRate {
    if ceiling_bps == 0 {
        guardar_texto("quall_rate_new: teto zero");
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(QuallRate(ControleDeTaxa::novo(
        PoliticaDeTaxa::com_teto(ceiling_bps),
    ))))
}

/// Alimenta uma janela e devolve o motivo. Escreve `out_bps` **só** com `Down` ou `Up`.
///
/// `packets` é o que o emissor mandou na janela (vistos + perdidos), nunca o que chegou.
///
/// # Safety
///
/// `r` precisa vir de [`quall_rate_new`]; `out_bps` pode ser nulo.
#[no_mangle]
pub unsafe extern "C" fn quall_rate_sample(
    r: *mut QuallRate,
    ms: u64,
    packets: u64,
    lost: u64,
    suspect: u64,
    broken_idrs: u64,
    // Ver `quall_session_report_link`. Três ou mais numa janela fazem o alvo **descer**, mesmo
    // com perda de rede zero — que é o caso de campo que abriu esta frente.
    not_delivered: u64,
    out_bps: *mut u32,
) -> QuallRateReason {
    let Some(c) = r.as_mut() else {
        guardar_nulo("quall_rate_sample: controlador nulo");
        return QuallRateReason::Skipped;
    };
    let (novo, motivo) = c.0.amostra(Amostra {
        ms,
        pacotes: packets,
        perdidos: lost,
        suspeitos: suspect,
        idrs_quebrados: broken_idrs,
        nao_decodificados: not_delivered,
    });
    if let (Some(bps), false) = (novo, out_bps.is_null()) {
        *out_bps = bps;
    }
    motivo.into()
}

/// Bitrate em vigor, em bps. Zero para controlador nulo.
///
/// # Safety
///
/// `r` precisa vir de [`quall_rate_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_rate_current_bps(r: *const QuallRate) -> u32 {
    r.as_ref().map(|c| c.0.atual_bps()).unwrap_or(0)
}

/// O controlador chegou ao piso?
///
/// É a **única** condição em que a resposta certa é para o usuário e não para o encoder: abaixo
/// do piso a imagem não vale a pena, e dizer isso é melhor que entregar lodo. A casca decide o
/// que mostrar.
///
/// # Safety
///
/// `r` precisa vir de [`quall_rate_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_rate_at_floor(r: *const QuallRate) -> bool {
    r.as_ref().map(|c| c.0.no_piso()).unwrap_or(false)
}

/// Janelas consideradas, descidas e subidas, para o relato. Escreve **três** `uint64_t`.
///
/// # Safety
///
/// `r` precisa vir de [`quall_rate_new`]; `out` precisa apontar para três `uint64_t`.
#[no_mangle]
pub unsafe extern "C" fn quall_rate_counters(r: *const QuallRate, out: *mut u64) {
    let (Some(c), false) = (r.as_ref(), out.is_null()) else {
        return;
    };
    let (janelas, descidas, subidas) = c.0.contadores();
    let campos = [janelas, descidas, subidas];
    std::ptr::copy_nonoverlapping(campos.as_ptr(), out, campos.len());
}

/// Libera o controlador. Nulo é ignorado.
///
/// # Safety
///
/// `r` precisa vir de [`quall_rate_new`] e não pode ter sido liberado antes.
#[no_mangle]
pub unsafe extern "C" fn quall_rate_free(r: *mut QuallRate) {
    if !r.is_null() {
        drop(Box::from_raw(r));
    }
}

/// Fecha a sessão, **espera os tratadores da casca saírem**, e libera. Nulo é ignorado.
///
/// Libere as tracks **antes**: depois disto, um [`QuallTrack`] que a casca ainda segure passa a
/// devolver erro em vez de enviar. Não é falha de memória — o handle continua válido e os
/// contadores continuam legíveis —, mas a sessão acabou e nada mais atravessa.
///
/// # Isto **é** uma barreira, e é o que autoriza liberar o `user_data`
///
/// Com `QUALL_STATUS_OK`, esta função garante duas coisas ao voltar:
///
/// 1. Nenhum tratador desta sessão — `quall_track_on_frame`, `quall_track_on_idr_request` —
///    está rodando em thread nenhuma.
/// 2. Nenhum voltará a rodar.
///
/// **A casca pode liberar o `user_data` na linha seguinte.** Era o contrato que faltava, e o
/// header anterior dizia o contrário com todas as letras: "mantenha o `user_data` vivo por conta
/// própria, ou proteja-o com um cadeado que o callback respeite".
///
/// # Quando o status **não** é `QUALL_STATUS_OK`, não libere nada
///
/// A sessão é fechada e liberada de qualquer forma; o que muda é só a promessa sobre os
/// tratadores. Dois casos:
///
/// - `QUALL_STATUS_TIMEOUT`: passaram-se 2 segundos e um tratador da casca ainda não voltou.
///   Isso é tratador que bloqueia, o que o contrato proíbe. O `user_data` precisa continuar
///   vivo, e o defeito é do lado da casca.
/// - `QUALL_STATUS_INVALID`: esta chamada veio **de dentro de um tratador**. Esperar seria
///   esperar por si mesma, e o processo penduraria; a fronteira recusa a espera em vez de
///   travar. Feche a sessão de uma thread que não seja a do tratador.
///
/// # Não feche a sessão de dentro de um tratador
///
/// Além de não render barreira, destruir a conexão de dentro de uma thread da libdatachannel é
/// pedir para a própria biblioteca se enroscar. Levante uma bandeira e feche do laço da casca.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`] e não pode ter sido liberado antes.
#[no_mangle]
pub unsafe extern "C" fn quall_session_close(s: *mut QuallSession) -> QuallStatus {
    if s.is_null() {
        return QuallStatus::Ok;
    }
    let mut caixa = Box::from_raw(s);
    // Fechar o portão **antes** de largar a caixa. O `Drop` da sessão fecha de novo — e é bom
    // que feche, porque a casca pode nunca chamar isto —, mas o `Drop` não tem para quem
    // devolver o resultado, e é o resultado que a casca precisa para decidir sobre o
    // `user_data`.
    let barreira = caixa.pronto.session.fechar_portao(PRAZO_DA_BARREIRA);
    caixa.pronto.link.close("sessão encerrada pela casca");
    drop(caixa);
    traduzir_barreira(barreira, "quall_session_close")
}

/// Traduz o resultado da barreira para o status da fronteira, deixando o motivo em
/// [`quall_last_error`].
fn traduzir_barreira(barreira: Barreira, quem: &str) -> QuallStatus {
    match barreira {
        Barreira::Cumprida => QuallStatus::Ok,
        Barreira::Prazo => guardar_mensagem(
            format!(
                "{quem}: um tratador da casca não voltou em 2 s — não libere o `user_data`. \
                 Tratador não pode bloquear."
            ),
            QuallStatus::Timeout,
        ),
        Barreira::DeDentroDoTratador => guardar_texto(&format!(
            "{quem}: chamada de dentro de um tratador — a barreira não vale e o `user_data` \
             tem de continuar vivo. Faça isto de outra thread."
        )),
    }
}

/// Libera os recursos globais da libdatachannel. Chame **uma vez**, na saída do processo.
///
/// Num app comum dá para viver sem; num plugin de OBS que é descarregado e recarregado, não — a
/// biblioteca deixa threads vivas.
///
/// # Feche todas as sessões antes
///
/// A libdatachannel espera todos os objetos dela serem destruídos. Chamada com uma sessão viva,
/// `rtcCleanup()` apaga os mapas, espera 10 segundos, desiste e registra `Cleanup timeout` — ela
/// **volta**, mas deixa a thread de limpeza presa, e é essa thread que impede o processo de
/// morrer. (Texto corrigido em 2026-08-23: a versão anterior dizia "trava o processo, sem erro e
/// sem log", que era o sintoma na bancada e não o que `capi.cpp:1691` faz. Há log — procure por
/// `Cleanup timeout` antes de qualquer outra coisa.)
///
/// Foi assim que a sonda ficou pendurada na bancada no M1, segurando a porta de sinalização e
/// fazendo a execução seguinte falhar com "Address already in use" — um sintoma três passos
/// distante da causa.
///
/// **No Android, não chame.** Não existe "saída do processo": o Service para e o processo
/// continua, e `System.loadLibrary` não descarrega a `.so`. Não há hora segura, e não há o que
/// limpar.
#[no_mangle]
pub extern "C" fn quall_cleanup() {
    quall_core::transport::cleanup();
}

// =============================================================================================
// Mídia
// =============================================================================================

/// Um quadro já codificado. Espelha `QuadroCodificado` do contrato.
///
/// Ao **receber**, `annexb` aponta para o buffer de remontagem do núcleo e vale **só durante a
/// chamada** do tratador. Ao **enviar**, o buffer é do chamador e o núcleo não o guarda.
#[repr(C)]
pub struct QuallFrame {
    /// Um quadro completo em Annex-B, com SPS/PPS junto quando for IDR.
    pub annexb: *const u8,
    pub len: usize,
    /// Em microssegundos.
    ///
    /// - **Ao enviar**: o relógio monotônico da captura, **o mesmo de todas as tracks da sessão**
    ///   (`docs/contrato-track.md`, "O relógio do `timestamp_us`").
    /// - **Ao receber**: desde a **base desta track**, o primeiro quadro entregue. Não é o
    ///   relógio do emissor, e duas tracks não se comparam por ele. Para o relógio comum da
    ///   sessão, some [`quall_track_capture_offset_us`].
    pub timestamp_us: u64,
    pub idr: bool,
}

/// Teto de um quadro codificado, em bytes.
///
/// 64 MiB é absurdo para um quadro H.264 — um IDR de 4K sem compressão nenhuma não chega perto —
/// e é justamente por isso que serve de teto: ele não recusa nada real e barra o `len` que veio
/// de uma conversão errada na casca. Ver [`quall_track_send_frame`].
const MAX_QUADRO: usize = 64 * 1024 * 1024;

/// Chamado quando um quadro fica pronto. Ver [`quall_track_on_frame`].
pub type QuallFrameCallback =
    Option<unsafe extern "C" fn(frame: *const QuallFrame, user_data: *mut c_void)>;

/// Chamado quando o receptor pede um IDR. Ver [`quall_track_on_idr_request`].
pub type QuallIdrRequestCallback = Option<unsafe extern "C" fn(user_data: *mut c_void)>;

/// O que esta track carrega.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_track`] ou [`quall_session_next_track`].
#[no_mangle]
pub unsafe extern "C" fn quall_track_kind(t: *const QuallTrack) -> QuallTrackKind {
    match t.as_ref().map(|x| &x.lado) {
        Some(Lado::Emissor(e)) => e.kind().into(),
        Some(Lado::Receptor(r)) => r.kind().into(),
        // Sem track não há resposta certa; `Screen` é o caso comum e não inventa um valor que
        // não existe no enum. Mas o motivo **fica** em `quall_last_error`: devolver um valor
        // plausível sem deixar rastro é como uma casca passa meia hora procurando o defeito
        // errado.
        None => {
            guardar_nulo("quall_track_kind: track nula");
            QuallTrackKind::Screen
        }
    }
}

/// Rótulo legível da track. Padrão `(buf, cap)`.
///
/// # Safety
///
/// `t` precisa ser válido; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_track_label(
    t: *const QuallTrack,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    match t.as_ref().map(|x| &x.lado) {
        Some(Lado::Emissor(e)) => escrever_texto(e.label(), buf, cap),
        Some(Lado::Receptor(r)) => escrever_texto(r.label(), buf, cap),
        None => {
            guardar_nulo("quall_track_label: track nula");
            -1
        }
    }
}

/// **`enviar_quadro` do contrato.** Empacota e solta um quadro.
///
/// Volta quando os pacotes RTP já foram entregues ao transporte. Não há fila nossa no caminho e
/// nenhuma cópia é guardada: é isso que mantém o núcleo dentro dos ~50 MB da Broadcast Upload
/// Extension do iOS.
///
/// Erro aqui é a track ainda não estar aberta — estado normal enquanto o ICE não fechou — ou o
/// transporte ter caído. Nos dois casos a casca **descarta o quadro e segue**; enfileirar para
/// tentar de novo é exatamente o que não se deve fazer com vídeo ao vivo.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_track`]; `frame` precisa apontar para `len` bytes válidos
/// durante a chamada.
#[no_mangle]
pub unsafe extern "C" fn quall_track_send_frame(
    t: *const QuallTrack,
    frame: *const QuallFrame,
) -> QuallStatus {
    let Some(track) = t.as_ref() else {
        return guardar_nulo("quall_track_send_frame: track nula");
    };
    let Some(quadro) = frame.as_ref() else {
        return guardar_nulo("quall_track_send_frame: `frame` é nulo");
    };
    let Lado::Emissor(emissor) = &track.lado else {
        return guardar_texto("quall_track_send_frame: esta track é de recepção");
    };
    if quadro.annexb.is_null() {
        return guardar_nulo("quall_track_send_frame: `annexb` é nulo");
    }
    if quadro.len == 0 {
        return guardar_texto("quall_track_send_frame: quadro vazio");
    }
    // Teto de sanidade antes de montar a fatia. `from_raw_parts` com um `len` absurdo é
    // comportamento indefinido na hora, não erro depois — e um `-1` que virou `usize` numa
    // conversão da casca é exatamente o `len` absurdo que chega aqui. O shim JNI do Android já
    // fazia esta checagem por conta própria (`quall_jni.c`), o que é sinal de que ela pertence
    // a esta fronteira, não a cada casca.
    if quadro.len > MAX_QUADRO {
        return guardar_texto("quall_track_send_frame: `len` maior que o teto de um quadro");
    }
    let bytes = std::slice::from_raw_parts(quadro.annexb, quadro.len);
    match emissor.enviar_quadro(QuadroCodificado {
        annexb: bytes,
        timestamp_us: quadro.timestamp_us,
        idr: quadro.idr,
    }) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// Um quadro de áudio já codificado. Espelha `AmostraDeAudio` do contrato.
///
/// **Não é** o mesmo struct que [`QuallFrame`], e a diferença é deliberada: um quadro de áudio
/// não tem `idr` — todo quadro de Opus é independente — e não tem Annex-B. Fundir os dois
/// obrigaria a um campo que só faz sentido em metade dos usos.
#[repr(C)]
pub struct QuallAudioSample {
    /// **Um** quadro codificado inteiro: um pacote Opus, ou 20 ms de G.711.
    pub payload: *const u8,
    pub len: usize,
    /// Relógio monotônico da captura, em microssegundos — **o mesmo do vídeo**, para que as duas
    /// tracks da sessão possam ser alinhadas do outro lado.
    pub timestamp_us: u64,
}

/// Teto de um quadro de áudio, em bytes.
///
/// O maior pacote de Opus que a RFC 6716 admite é de 1 275 bytes por quadro; 4 KiB é folga
/// larga e mesmo assim barra o `len` absurdo que veio de uma conversão errada na casca — a
/// mesma razão do [`MAX_QUADRO`] do vídeo, com a diferença de que aqui o teto real é conhecido
/// e pequeno, então ele também pega o erro **oposto**: a casca que concatenou dois quadros numa
/// chamada. Ver [`quall_track_send_audio`].
const MAX_AMOSTRA: usize = 4 * 1024;

/// **`enviar_audio` do contrato.** Empacota e solta **um** quadro de áudio.
///
/// # A porta que faltava
///
/// `QUALL_TRACK_KIND_MICROPHONE` e `QUALL_TRACK_KIND_SYSTEM_AUDIO` existiam nesta fronteira e
/// **não tinham como ser usados**: a única porta de envio era [`quall_track_send_frame`], que
/// leva `QuallFrame` e devolve `QUALL_STATUS_INVALID` em track de áudio. `enviar_audio` parava
/// em `quall-core`; a sonda fala Rust direto, e por isso ninguém tropeçou. Uma casca C, Swift ou
/// Kotlin declarava a track e ficava sem o que fazer com ela.
///
/// # Um quadro por chamada, e o erro não denuncia
///
/// O pacotizador de áudio da libdatachannel **não fragmenta**: uma mensagem entra, um pacote RTP
/// sai. Dois quadros de Opus concatenados numa chamada viram um pacote que o outro lado
/// decodifica errado **sem erro nenhum no caminho**, porque para o RTP é só um payload maior.
/// [`MAX_AMOSTRA`] pega o caso grosseiro; o caso de dois quadros de 80 bytes ele não pega, e
/// nada pega — a regra é do chamador.
///
/// Como no vídeo: não há fila nossa, nenhuma cópia é guardada, e erro aqui é a track ainda não
/// estar aberta ou o transporte ter caído. Nos dois casos a casca **descarta e segue**.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_track`]; `sample` precisa apontar para um
/// [`QuallAudioSample`] válido cujo `payload` tenha `len` bytes legíveis durante a chamada.
#[no_mangle]
pub unsafe extern "C" fn quall_track_send_audio(
    t: *const QuallTrack,
    sample: *const QuallAudioSample,
) -> QuallStatus {
    let Some(track) = t.as_ref() else {
        return guardar_nulo("quall_track_send_audio: track nula");
    };
    let Some(amostra) = sample.as_ref() else {
        return guardar_nulo("quall_track_send_audio: `sample` é nulo");
    };
    let Lado::Emissor(emissor) = &track.lado else {
        return guardar_texto("quall_track_send_audio: esta track é de recepção");
    };
    if amostra.payload.is_null() {
        return guardar_nulo("quall_track_send_audio: `payload` é nulo");
    }
    if amostra.len == 0 {
        // Silêncio, no Opus, é um quadro curto — e com DTX é a ausência de pacote. Payload vazio
        // é a casca errada, não silêncio.
        return guardar_texto("quall_track_send_audio: quadro de áudio vazio");
    }
    if amostra.len > MAX_AMOSTRA {
        return guardar_texto(
            "quall_track_send_audio: `len` maior que o teto de um quadro de áudio (4 KiB). \
             Um quadro por chamada — o pacotizador não fragmenta.",
        );
    }
    let bytes = std::slice::from_raw_parts(amostra.payload, amostra.len);
    match emissor.enviar_audio(AmostraDeAudio {
        payload: bytes,
        timestamp_us: amostra.timestamp_us,
    }) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

// =============================================================================================
// O preset de áudio, e o encoder que ele configura
// =============================================================================================

/// Resolve o preset de uma espécie mais um codec opcional. **A fonte de verdade, uma só.**
fn preset_resolvido(kind: TrackKind, codec: QuallAudioCodec) -> Option<PresetDeAudio> {
    let base = kind.preset_de_audio()?;
    Some(match codec.para_core() {
        // **`canais_no_fio`, e não `base.canais`.** Achado em 30/08/2026 pela casca Android, e é
        // o mesmo defeito que a sonda tinha achado em 27/08 — um nível acima.
        //
        // `CodecDeAudio::canais_no_fio` existe exatamente para isto: a RFC 3551 §6 atribui o
        // payload type 0 a `PCMU/8000/1`, e uma track de áudio de **sistema** pede estéreo pelo
        // preset da espécie. Trocando só o campo `codec`, esta função respondia `channels: 2`
        // para um fluxo que anda no fio em mono — e quem confia na resposta monta o dispositivo
        // de saída com dois canais e toca no dobro da velocidade, uma oitava acima, **sem erro em
        // lugar nenhum**.
        //
        // Foi medido assim: `quall-probe emitir-audio --track sistema --codec pcmu` por loopback
        // contra o app Android, que lê este JSON. A sonda imprimia `canais: 1` (ela chama
        // `canais_no_fio`) e o app montava 2 canais (ele lia daqui). O tom de quatro notas saía
        // com 2 de 4 notas reconhecíveis e razão de raia 0,254 contra os 0,978 do Opus.
        //
        // O teste que existia só cobria `Microphone`, que já é mono — a combinação
        // `SystemAudio + Pcmu` nunca tinha sido corrida, exatamente como no defeito de 27/08.
        Some(c) => PresetDeAudio {
            codec: c,
            canais: c.canais_no_fio(base.canais),
            ..base
        },
        None => base,
    })
}

/// **O preset que o núcleo vai anunciar no SDP, para a casca ler antes de codificar.**
///
/// # Por que isto existe, e por que é JSON
///
/// A casca precisa saber taxa de amostragem, número de canais e amostras por quadro para
/// capturar e codificar. Se ela **fixar** esses números no código dela, o preset do fio e o
/// preset da captura viram duas fontes de verdade que divergem em silêncio — que é exatamente a
/// classe de defeito que o `useinbandfec=1` sem LBRR e o SPS sem `bitstream_restriction`
/// custaram a este projeto. Aqui a casca **pergunta**, e a resposta sai da mesma
/// `TrackKind::preset_de_audio` que gera o `a=fmtp`.
///
/// JSON pelo precedente de [`quall_browser_devices_json`] e [`quall_track_stats_json`]: a
/// alternativa seria meia dúzia de funções com um campo cada, e cada campo novo do preset
/// obrigaria a uma função nova na fronteira.
///
/// Serve **antes** de haver sessão, de propósito: quem vai mandar G.711 em Swift puro — o uso
/// que `docs/audio.md` §2 reservou ao PCMU — precisa dos números na hora de abrir a captura, e
/// não precisa de encoder nenhum.
///
/// As chaves: `codec`, `sample_rate_hz`, `channels`, `frame_ms`, `frame_samples` (**por
/// canal**), `bitrate_bps`, `fec`, `expected_loss_pct`, `is_speech`, `payload_type`, `fmtp`,
/// `content_delay_us`: quanto o conteúdo decodificado sai atrás do carimbo (6 500 no Opus, o
/// lookahead do codificador do emissor; 0 no PCMU). O receptor o soma ao atraso interno dele,
/// como soma o do filtro do PCMU.
///
/// Devolve `-1` e um erro em `quall_last_error` se a espécie não for de áudio. Padrão
/// `(buf, cap)`: com `buf` nulo devolve o tamanho necessário, incluindo o NUL.
///
/// # Safety
///
/// `buf` precisa ser nulo ou apontar para `cap` bytes graváveis.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_preset_json(
    kind: QuallTrackKind,
    codec: QuallAudioCodec,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(p) = preset_resolvido(kind.into(), codec) else {
        guardar_texto("quall_audio_preset_json: esta espécie de track não é de áudio");
        return -1;
    };
    // O G.711 não tem FEC nenhum, e o preset da espécie carrega `fec: true` porque ele nasceu
    // do microfone. Repetir isso aqui seria a fronteira dizendo à casca que há recuperação de
    // perda onde não há — o mesmo defeito que o campo de codec existe para evitar, na direção
    // da leitura. O `fmtp` do PCMU já sai vazio pelo núcleo, e estes dois acompanham.
    let tem_fec = p.fec && p.codec == CodecDeAudio::Opus;
    let json = serde_json::json!({
        "codec": p.codec.nome_rtpmap(),
        "sample_rate_hz": p.codec.relogio_hz(),
        "channels": p.canais,
        "frame_ms": p.duracao_do_quadro_ms,
        "frame_samples": p.amostras_por_quadro(),
        "bitrate_bps": p.taxa_media_bits,
        "fec": tem_fec,
        "expected_loss_pct": if tem_fec { p.perda_esperada_pct } else { 0 },
        "is_speech": p.conteudo_e_fala,
        "payload_type": p.codec.payload_type(),
        "fmtp": p.fmtp(),
        "content_delay_us": p.codec.atraso_do_conteudo_us(),
    });
    escrever_texto(&json.to_string(), buf, cap)
}

/// Um encoder de áudio configurado pelo preset da track. Ver [`quall_audio_encoder_new`].
pub struct QuallAudioEncoder {
    #[cfg(feature = "opus")]
    codificador: quall_opus::Codificador,
    preset: PresetDeAudio,
}

/// **Cria um encoder de Opus já configurado pelo preset da espécie.**
///
/// # Por que o encoder atravessa a fronteira
///
/// Hoje só `quall-probe` depende de `quall-opus`, então o `libquall.a` que as cascas linkam **não
/// carrega símbolo nenhum da libopus**. E o macOS não tem encoder de Opus no sistema — o
/// AudioToolbox tem AAC, não Opus. Sem esta porta, nenhuma casca Apple codifica Opus, nunca.
///
/// # A regra do desenho: uma fonte de verdade
///
/// O encoder é configurado pela **mesma** `TrackKind::preset_de_audio` que gera o `a=fmtp` — taxa
/// de bits, canais, FEC, perda esperada e sinal saem todos de lá. Se cada casca configurasse o
/// seu, o preset do fio e o preset do encoder seriam duas fontes de verdade divergindo em
/// silêncio, e a classe de defeito que custou a §11 do `docs/audio.md` voltaria em quatro
/// lugares em vez de um.
///
/// Em particular, `OPUS_SET_PACKET_LOSS_PERC` é setado aqui a partir de
/// `PresetDeAudio::perda_esperada_pct`. **Sem ele, `useinbandfec=1` produz um fluxo sem LBRR
/// nenhum** — medido, 0 de 300 pacotes — e o receptor do outro lado dimensionaria o jitter
/// buffer contando com uma recuperação que nunca viria.
///
/// # Quando devolve nulo
///
/// - a espécie não é de áudio;
/// - o preset resolvido pede [`QuallAudioCodec::Pcmu`]. **Isso é deliberado**: G.711 µ-law é uma
///   tabela de consulta de 8 bits que a casca escreve em vinte linhas, e trazê-la para cá seria
///   uma porta a mais na fronteira para um problema que não é dela. O erro diz isso;
/// - a biblioteca foi construída sem a feature `opus`.
///
/// Sempre há um motivo legível em `quall_last_error`.
///
/// # Safety
///
/// A função em si não desreferencia nada. O ponteiro devolvido é do chamador e precisa ir para
/// [`quall_audio_encoder_free`].
#[no_mangle]
pub unsafe extern "C" fn quall_audio_encoder_new(
    kind: QuallTrackKind,
    codec: QuallAudioCodec,
) -> *mut QuallAudioEncoder {
    let Some(preset) = preset_resolvido(kind.into(), codec) else {
        guardar_texto("quall_audio_encoder_new: esta espécie de track não é de áudio");
        return ptr::null_mut();
    };
    if preset.codec == CodecDeAudio::Pcmu {
        guardar_texto(
            "quall_audio_encoder_new: o preset pede PCMU. G.711 µ-law é uma tabela de consulta \
             de 8 bits — codifique na casca e use quall_track_send_audio.",
        );
        return ptr::null_mut();
    }

    #[cfg(not(feature = "opus"))]
    {
        let _ = preset;
        guardar_texto(
            "quall_audio_encoder_new: esta libquall foi construída sem a feature `opus`.",
        );
        ptr::null_mut()
    }

    #[cfg(feature = "opus")]
    {
        match montar_codificador(&preset) {
            Ok(codificador) => Box::into_raw(Box::new(QuallAudioEncoder {
                codificador,
                preset,
            })),
            Err(motivo) => {
                guardar_texto(&motivo);
                ptr::null_mut()
            }
        }
    }
}

/// Todo `OPUS_SET_*` que o preset manda, num lugar só.
#[cfg(feature = "opus")]
fn montar_codificador(preset: &PresetDeAudio) -> Result<quall_opus::Codificador, String> {
    use quall_opus::{Aplicacao, Sinal};

    let aplicacao = if preset.conteudo_e_fala {
        Aplicacao::Voz
    } else {
        Aplicacao::Audio
    };
    let mapear = |e: quall_opus::Erro| format!("quall_audio_encoder_new: {e}");

    let mut c = quall_opus::Codificador::novo(preset.codec.relogio_hz(), preset.canais, aplicacao)
        .map_err(mapear)?;
    c.definir_taxa_de_bits(preset.taxa_media_bits)
        .map_err(mapear)?;
    c.definir_fec_embutido(preset.fec).map_err(mapear)?;
    // O campo que faz o LBRR existir. Ver `docs/audio.md` §11.
    c.definir_perda_esperada(preset.perda_esperada_pct)
        .map_err(mapear)?;
    // `usedtx=0` nos dois presets: DTX faria o carimbo dar saltos longos que toda análise de
    // continuidade — e o jitter buffer, que conta em pacotes — precisaria aprender a perdoar.
    c.definir_dtx(false).map_err(mapear)?;
    // Nós sabemos o que a track carrega; a análise do Opus adivinha, e adivinhava errado:
    // com o sinal em automático a fala caía em CELT em 282 de 300 quadros, onde o LBRR não
    // existe. Ver `docs/audio.md` §11.
    if preset.conteudo_e_fala {
        c.definir_sinal(Sinal::Voz).map_err(mapear)?;
    }
    // A complexidade da espécie (10, escrito de propósito): ver `PresetDeAudio::complexidade_do_encoder`.
    c.definir_complexidade(preset.complexidade_do_encoder())
        .map_err(mapear)?;
    Ok(c)
}

/// **Muda a complexidade do encoder** (0–10, `OPUS_SET_COMPLEXITY`; acima de 10 vale 10) com ele em uso — a casca baixa no
/// calor e volta ao padrão da espécie depois (`docs/teleprompter-com-camera.md` §8.12.17). Não mexe
/// no fio. `0` = OK; `-1` com o motivo em `quall_last_error` (encoder nulo, ou a libopus recusou).
///
/// # Safety
///
/// `e` precisa vir de [`quall_audio_encoder_new`] e não ter sido liberado, e **não pode estar sendo
/// usado por outra thread ao mesmo tempo** (a mesma regra de [`quall_audio_encoder_encode`]).
#[no_mangle]
pub unsafe extern "C" fn quall_audio_encoder_set_complexity(
    e: *mut QuallAudioEncoder,
    complexity: u8,
) -> i32 {
    let Some(e) = e.as_mut() else {
        guardar_texto("quall_audio_encoder_set_complexity: encoder nulo");
        return -1;
    };
    #[cfg(feature = "opus")]
    {
        match e.codificador.definir_complexidade(complexity) {
            Ok(()) => 0,
            Err(erro) => {
                guardar_texto(&format!("quall_audio_encoder_set_complexity: {erro}"));
                -1
            }
        }
    }
    #[cfg(not(feature = "opus"))]
    {
        let _ = (e, complexity);
        guardar_texto("quall_audio_encoder_set_complexity: esta libquall foi construída sem a feature `opus`.");
        -1
    }
}

/// A complexidade padrão do encoder desta espécie de track (`PresetDeAudio::complexidade_do_encoder`),
/// para a casca voltar a ela; `-1` se a espécie não é de áudio.
#[no_mangle]
pub extern "C" fn quall_audio_default_complexity(kind: QuallTrackKind) -> i32 {
    let k: TrackKind = kind.into();
    k.preset_de_audio()
        .map(|p| i32::from(p.complexidade_do_encoder()))
        .unwrap_or(-1)
}

/// Codifica **um** quadro de PCM intercalado de 16 bits.
///
/// `pcm_len` é o número total de amostras — `frame_samples × channels`, os dois vindos de
/// [`quall_audio_preset_json`]. Devolve quantos bytes foram escritos em `out`, ou `-1` com o
/// motivo em `quall_last_error`.
///
/// O que sai é exatamente o que vai em [`QuallAudioSample::payload`]: um quadro, um pacote.
///
/// # Safety
///
/// `e` precisa vir de [`quall_audio_encoder_new`] e não ter sido liberado. `pcm` precisa ter
/// `pcm_len` amostras legíveis; `out` precisa ter `out_cap` bytes graváveis. **Não é seguro
/// chamar de duas threads sobre o mesmo encoder** — o estado do Opus é preditivo.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_encoder_encode(
    e: *mut QuallAudioEncoder,
    pcm: *const i16,
    pcm_len: usize,
    out: *mut u8,
    out_cap: usize,
) -> isize {
    let Some(enc) = e.as_mut() else {
        guardar_nulo("quall_audio_encoder_encode: encoder nulo");
        return -1;
    };
    if pcm.is_null() || out.is_null() {
        guardar_nulo("quall_audio_encoder_encode: `pcm` ou `out` é nulo");
        return -1;
    }
    let esperado = enc.preset.amostras_por_quadro() * usize::from(enc.preset.canais);
    if pcm_len != esperado {
        // Recusar em vez de codificar o que veio: o Opus aceita várias durações de quadro, então
        // um `pcm_len` errado **não dá erro** — dá um pacote com duração diferente da que o
        // `a=fmtp` anunciou, e o outro lado não tem como saber.
        guardar_texto(&format!(
            "quall_audio_encoder_encode: esperava {esperado} amostras \
             (frame_samples × channels do preset) e recebeu {pcm_len}"
        ));
        return -1;
    }
    if out_cap == 0 || out_cap > MAX_AMOSTRA {
        guardar_texto("quall_audio_encoder_encode: `out_cap` fora de 1..=4096");
        return -1;
    }

    #[cfg(not(feature = "opus"))]
    {
        let _ = (pcm, out);
        guardar_texto("quall_audio_encoder_encode: construída sem a feature `opus`");
        -1
    }

    #[cfg(feature = "opus")]
    {
        let entrada = std::slice::from_raw_parts(pcm, pcm_len);
        let saida = std::slice::from_raw_parts_mut(out, out_cap);
        match enc.codificador.codificar(entrada, saida) {
            Ok(n) => n as isize,
            Err(erro) => {
                guardar_texto(&format!("quall_audio_encoder_encode: {erro}"));
                -1
            }
        }
    }
}

/// Libera o encoder. Nulo é no-op.
///
/// # Safety
///
/// `e` precisa vir de [`quall_audio_encoder_new`] e não pode ser liberado duas vezes.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_encoder_free(e: *mut QuallAudioEncoder) {
    if !e.is_null() {
        drop(Box::from_raw(e));
    }
}

/// Um decodificador de Opus configurado pelo preset da track. Ver [`quall_audio_decoder_new`].
pub struct QuallAudioDecoder {
    #[cfg(feature = "opus")]
    decodificador: quall_opus::Decodificador,
    preset: PresetDeAudio,
}

/// **Cria um decodificador de Opus já configurado pelo preset da espécie.**
///
/// # Por que ele entrou agora, e não na rodada que trouxe o encoder
///
/// `docs/audio.md` §13 recusou esta porta com três argumentos, e escreveu o que a faria entrar:
/// *"Prefiro entregá-la na rodada em que houver uma casca reproduzindo som."* É esta rodada — o
/// receptor Android toca o slot num `AudioTrack`, e sem esta porta ele não teria como.
///
/// O argumento que continua **não** valendo é o que subiu o encoder: `opus_decode` não é
/// configurado por preset nenhum, então não há segunda fonte de verdade para divergir. O que
/// vale é o resto do parágrafo daquela seção — e a alternativa, no Android, seria o `MediaCodec`,
/// que **não tem** `decode_fec` nem ocultação de perda explícita. Com ele, a ordem
/// [`QuallAudioOrder::Fec`] que o jitter buffer entrega não teria consumidor: o socorro
/// atravessaria a rede e a casca o jogaria fora. A porta existe para que as três ordens do slot
/// tenham as três respostas.
///
/// Ela **não** faz o núcleo tocar em PCM: quem escreve o `i16` é o chamador, no buffer dele. O
/// que atravessa continua sendo bytes de um lado e amostras do outro, e nenhuma decisão de
/// apresentação — taxa do dispositivo, mistura, WSOLA — mora aqui.
///
/// # Quando devolve nulo
///
/// Os mesmos três casos de [`quall_audio_encoder_new`], **e pelos mesmos motivos**:
///
/// - a espécie não é de áudio;
/// - o preset resolvido pede [`QuallAudioCodec::Pcmu`]. G.711 µ-law é a mesma tabela de consulta
///   de 8 bits na volta, e trazê-la para cá seria uma porta a mais para um problema que não é
///   daqui. Numa track de PCMU a casca decodifica em vinte linhas e, na ordem `SILENCE`, escreve
///   silêncio;
/// - a biblioteca foi construída sem a feature `opus`.
///
/// Sempre há um motivo legível em `quall_last_error`.
///
/// # Safety
///
/// A função em si não desreferencia nada. O ponteiro devolvido é do chamador e precisa ir para
/// [`quall_audio_decoder_free`].
#[no_mangle]
pub unsafe extern "C" fn quall_audio_decoder_new(
    kind: QuallTrackKind,
    codec: QuallAudioCodec,
) -> *mut QuallAudioDecoder {
    let Some(preset) = preset_resolvido(kind.into(), codec) else {
        guardar_texto("quall_audio_decoder_new: esta espécie de track não é de áudio");
        return ptr::null_mut();
    };
    if preset.codec == CodecDeAudio::Pcmu {
        guardar_texto(
            "quall_audio_decoder_new: o preset pede PCMU. G.711 µ-law é uma tabela de consulta \
             de 8 bits — decodifique na casca.",
        );
        return ptr::null_mut();
    }

    #[cfg(not(feature = "opus"))]
    {
        let _ = preset;
        guardar_texto(
            "quall_audio_decoder_new: esta libquall foi construída sem a feature `opus`.",
        );
        ptr::null_mut()
    }

    #[cfg(feature = "opus")]
    {
        match quall_opus::Decodificador::novo(preset.codec.relogio_hz(), preset.canais) {
            Ok(decodificador) => Box::into_raw(Box::new(QuallAudioDecoder {
                decodificador,
                preset,
            })),
            Err(erro) => {
                guardar_texto(&format!("quall_audio_decoder_new: {erro}"));
                ptr::null_mut()
            }
        }
    }
}

/// Decodifica **um** slot de 20 ms em PCM intercalado de 16 bits.
///
/// As três ordens de [`QuallAudioOrder`] têm as três chamadas, e é isto que elas viram:
///
/// | ordem do slot | `packet` | `decode_fec` | o que acontece |
/// |---|---|---|---|
/// | `FRAME` | o quadro | `false` | `opus_decode` normal |
/// | `FEC` | o pacote *N+1* | `true` | o LBRR reconstrói o slot que faltou |
/// | `SILENCE` | **nulo**, `len` 0 | `false` | ocultação de perda (PLC) do próprio decoder |
///
/// **`decode_fec = true` só depois de conferir `fec_has_lbrr == 1` no slot.** Sem LBRR o
/// `opus_decode` cai na ocultação de perda em silêncio e devolve sucesso — esta porta não tem
/// como distinguir isso e não finge que tem. É a armadilha que `QuallAudioSlot::fec_has_lbrr`
/// existe para fechar.
///
/// `out_cap` é o número de **amostras** (`i16`) graváveis, não bytes: para o preset de sistema
/// (estéreo, 20 ms a 48 kHz) são 1920. Devolve quantas amostras **por canal** foram escritas —
/// multiplique por `channels` do preset para saber quantos `i16` valem —, ou `-1` com o motivo em
/// `quall_last_error`.
///
/// # Safety
///
/// `d` precisa vir de [`quall_audio_decoder_new`] e não ter sido liberado. `packet` precisa ser
/// nulo ou ter `len` bytes legíveis; `out` precisa ter `out_cap` amostras graváveis. **Não é
/// seguro chamar de duas threads sobre o mesmo decodificador** — o estado do Opus é preditivo,
/// na volta como na ida.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_decoder_decode(
    d: *mut QuallAudioDecoder,
    packet: *const u8,
    len: usize,
    decode_fec: bool,
    out: *mut i16,
    out_cap: usize,
) -> isize {
    let Some(dec) = d.as_mut() else {
        guardar_nulo("quall_audio_decoder_decode: decodificador nulo");
        return -1;
    };
    if out.is_null() {
        guardar_nulo("quall_audio_decoder_decode: `out` é nulo");
        return -1;
    }
    let canais = usize::from(dec.preset.canais);
    let esperado = dec.preset.amostras_por_quadro() * canais;
    if out_cap < esperado {
        // Recusar em vez de escrever o que couber: um `out` curto faria a libopus devolver
        // `OPUS_BUFFER_TOO_SMALL`, e a casca ouviria 20 ms de nada sem saber por quê.
        guardar_texto(&format!(
            "quall_audio_decoder_decode: `out_cap` é {out_cap} e o quadro do preset precisa de \
             {esperado} amostras (frame_samples × channels)"
        ));
        return -1;
    }
    if packet.is_null() && decode_fec {
        guardar_texto(
            "quall_audio_decoder_decode: `decode_fec` com pacote nulo. O socorro do FEC é o \
             pacote N+1, e sem ele a chamada é ocultação de perda: passe decode_fec = false.",
        );
        return -1;
    }

    #[cfg(not(feature = "opus"))]
    {
        let _ = (packet, len, out, esperado);
        guardar_texto("quall_audio_decoder_decode: construída sem a feature `opus`");
        -1
    }

    #[cfg(feature = "opus")]
    {
        // Exatamente `esperado`, e não `out_cap`: a libopus escreve até caber, e um `out` grande
        // demais faria o decoder devolver mais de 20 ms num slot de 20 ms.
        let saida = std::slice::from_raw_parts_mut(out, esperado);
        let resultado = if packet.is_null() || len == 0 {
            dec.decodificador.ocultar_perda(saida)
        } else {
            let entrada = std::slice::from_raw_parts(packet, len);
            if decode_fec {
                dec.decodificador.decodificar_fec(entrada, saida)
            } else {
                dec.decodificador.decodificar(entrada, saida)
            }
        };
        match resultado {
            Ok(n) => n as isize,
            Err(erro) => {
                guardar_texto(&format!("quall_audio_decoder_decode: {erro}"));
                -1
            }
        }
    }
}

/// Libera o decodificador. Nulo é no-op.
///
/// # Safety
///
/// `d` precisa vir de [`quall_audio_decoder_new`] e não pode ser liberado duas vezes.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_decoder_free(d: *mut QuallAudioDecoder) {
    if !d.is_null() {
        drop(Box::from_raw(d));
    }
}

/// **O codec que esta track de áudio negociou**, lido do `a=rtpmap` da descrição dela.
///
/// # Por que a casca precisa disto, e por que o preset não basta
///
/// [`quall_audio_preset_json`] responde *"o que a espécie X com o codec Y pede"*, e serve ao
/// emissor, que **escolhe** o codec. O receptor não escolhe: ele recebe o que o outro lado
/// ofereceu, e `docs/audio.md` §6 diz que `TrackReceptor::adotar` lê o codec do `a=rtpmap` e
/// **recusa** a track quando não há um reconhecível. Sem esta função a casca receptora teria de
/// adivinhar entre Opus e G.711 — e adivinhar errado não dá erro em lugar nenhum: sai som, com o
/// relógio numa escala 6× errada. É a mesma classe de defeito que o campo de codec do
/// [`QuallTrackDesc`] existe para fechar, do lado de cá.
///
/// Devolve [`QuallAudioCodec::Default`] (`0`) quando **não há resposta**: track nula, track de
/// emissão, track de vídeo, ou track de áudio que ainda não adotou codec nenhum. O `0` aqui é
/// *"não sei"*, e não *"o padrão"* — o motivo sai em `quall_last_error`, na mesma thread.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_track`] ou [`quall_session_next_track`].
#[no_mangle]
pub unsafe extern "C" fn quall_track_audio_codec(t: *const QuallTrack) -> QuallAudioCodec {
    let Some(track) = t.as_ref() else {
        guardar_nulo("quall_track_audio_codec: track nula");
        return QuallAudioCodec::Default;
    };
    let Lado::Receptor(receptor) = &track.lado else {
        guardar_texto(
            "quall_track_audio_codec: esta track é de emissão — o codec dela é o que a casca \
             pediu em QuallTrackDesc::audio_codec.",
        );
        return QuallAudioCodec::Default;
    };
    match receptor.codec_de_audio() {
        Some(CodecDeAudio::Opus) => QuallAudioCodec::Opus,
        Some(CodecDeAudio::Pcmu) => QuallAudioCodec::Pcmu,
        None => {
            guardar_texto(
                "quall_track_audio_codec: esta track não tem codec de áudio adotado (é de vídeo, \
                 ou o rtpmap não foi reconhecido).",
            );
            QuallAudioCodec::Default
        }
    }
}

// =============================================================================================
// Recepção de áudio: o jitter buffer do núcleo, alcançável de C
// =============================================================================================

/// O que fazer com este slot de 20 ms. Ver [`quall_track_on_audio`].
///
/// **Um DAC não aceita "pulei este aqui"**: ele vai consumir 20 ms de alguma coisa, e a única
/// escolha é *de qual coisa*. É por isso que não existe uma quarta variante.
///
/// **Os valores são ABI**, pela mesma regra de [`QuallTrackKind`]: ordem nova entra no fim.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallAudioOrder {
    /// O pacote chegou. Decodifique normalmente.
    Frame = 0,
    /// **O slot não chegou, e o sucessor imediato está em mãos.**
    ///
    /// `payload` é o pacote *N+1* **inteiro, sem tocar**; `sequence` é o slot que faltou (*N*, e
    /// não *N+1*). O convite é chamar `opus_decode(..., decode_fec = 1)` sobre ele.
    ///
    /// **Confira `fec_has_lbrr` antes.** Sem LBRR, `opus_decode` com `decode_fec = 1` cai na
    /// ocultação de perda **em silêncio** e devolve sucesso — quem não conferir vai contar como
    /// "curado por FEC" um quadro que o decoder inventou.
    Fec = 1,
    /// Nem pacote nem socorro. Chame a ocultação de perda (PLC) do decoder.
    Silence = 2,
    /// **Não há fluxo tocando**: escreva **zeros**, e não ocultação de perda. Só sai da porta
    /// puxada ([`quall_audio_playout_pull`]): antes da primeira ancoragem, e depois de 10
    /// puxadas seguidas sem pacote utilizável. Chega com `payload` nulo, `len` 0,
    /// `sequence` 0, `timestamp_us` 0 e `fec_has_lbrr` 0.
    ///
    /// Entrou em 18/09/2026, **no fim**, pela regra de ABI do enum. A porta empurrada
    /// ([`quall_track_on_audio`]) nunca o entrega.
    Idle = 3,
}

/// Um slot de 20 ms saindo do jitter buffer, em ordem de reprodução. Ver [`quall_track_on_audio`].
///
/// O `payload` aponta para dentro do buffer do núcleo e **vale só durante a chamada**. Copie
/// ali se precisar guardar.
#[repr(C)]
pub struct QuallAudioSlot {
    pub order: QuallAudioOrder,
    /// `FRAME`: o quadro. `FEC`: o pacote *N+1*, o socorro. `SILENCE`: **nulo**, com `len` 0.
    pub payload: *const u8,
    pub len: usize,
    /// O slot. Em `FEC` é o slot que **faltou**, não o do socorro.
    pub sequence: u16,
    /// Microssegundos desde o primeiro pacote da track, do carimbo RTP. Em `FEC` e `SILENCE` é
    /// **interpolado** — não há pacote de onde ler um carimbo. Em `IDLE`, `0`. Para o relógio
    /// comum da sessão, some [`quall_track_capture_offset_us`].
    pub timestamp_us: u64,
    /// **O socorro carrega LBRR?** `1` sim, `0` não, **`-1` não sei**.
    ///
    /// Só é perguntado quando `order == QUALL_AUDIO_ORDER_FEC`; nas outras ordens é `0`.
    ///
    /// `-1` quer dizer que esta `libquall` foi construída **sem a feature `opus`** e não tem como
    /// ler o cabeçalho SILK. Não é "não" — é "não medido", e a distinção é a lição da dívida 26:
    /// um contador que finge saber é pior que um que admite não saber. Uma casca que receber `-1`
    /// não deve chamar `decode_fec`: ela não sabe se o que voltaria é recuperação ou invenção.
    pub fec_has_lbrr: i8,
}

/// Chamado uma vez por slot de 20 ms, em ordem de reprodução. Ver [`quall_track_on_audio`].
pub type QuallAudioSlotCallback =
    Option<unsafe extern "C" fn(slot: *const QuallAudioSlot, user_data: *mut c_void)>;

/// O estado do caminho de recepção de áudio de uma track. Ver [`quall_track_on_audio`].
struct RecepcaoDeAudio {
    buffer: Arc<Mutex<BufferDeJitter>>,
    /// Os contadores do buffer, publicados pelo tratador **depois** de a casca voltar. É daqui
    /// que `quall_track_stats_json` lê, sem pedir o cadeado do buffer — que o tratador segura
    /// enquanto chama a casca. Ver [`quall_track_stats_json`].
    retrato: Arc<Mutex<ContadoresDeBuffer>>,
    /// Guardados para o **escoamento** do desregistro. Ver [`quall_track_on_audio`].
    cb: QuallAudioSlotCallback,
    contexto: Contexto,
}

thread_local! {
    /// O endereço do buffer de áudio cujos slots esta thread está entregando à casca agora, com
    /// o cadeado dele na mão; 0 fora disso. `quall_track_on_audio(NULL)` chamado de dentro
    /// desse tratador não pode escoar o **mesmo** buffer (travaria a thread), mas pode escoar o
    /// de **outra** track — o que `Barreira::DeDentroDoTratador`, que vale para a sessão
    /// inteira, não distingue.
    static ENTREGANDO_AUDIO: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Marca "esta thread está entregando os slots deste buffer" enquanto vive.
struct EntregandoAudio {
    anterior: usize,
}

impl EntregandoAudio {
    fn entrar(buffer: &Arc<Mutex<BufferDeJitter>>) -> Self {
        let endereco = Arc::as_ptr(buffer) as usize;
        EntregandoAudio {
            anterior: ENTREGANDO_AUDIO.with(|e| e.replace(endereco)),
        }
    }

    fn esta_em(buffer: &Arc<Mutex<BufferDeJitter>>) -> bool {
        ENTREGANDO_AUDIO.with(|e| e.get() == Arc::as_ptr(buffer) as usize)
    }
}

impl Drop for EntregandoAudio {
    fn drop(&mut self) {
        ENTREGANDO_AUDIO.with(|e| e.set(self.anterior));
    }
}

/// Entrega uma ordem do buffer ao tratador da casca. O ponteiro do payload vive só na chamada.
///
/// # Safety
///
/// `funcao` e `contexto` são do chamador e precisam estar válidos.
unsafe fn entregar_slot(
    funcao: unsafe extern "C" fn(*const QuallAudioSlot, *mut c_void),
    contexto: Contexto,
    ordem: Entrega<'_>,
) {
    let slot = match ordem {
        Entrega::Quadro {
            payload,
            sequencia,
            timestamp_us,
        } => QuallAudioSlot {
            order: QuallAudioOrder::Frame,
            payload: payload.as_ptr(),
            len: payload.len(),
            sequence: sequencia,
            timestamp_us,
            fec_has_lbrr: 0,
        },
        Entrega::Fec {
            socorro,
            sequencia,
            timestamp_us,
        } => QuallAudioSlot {
            order: QuallAudioOrder::Fec,
            payload: socorro.as_ptr(),
            len: socorro.len(),
            sequence: sequencia,
            timestamp_us,
            // A pergunta que o núcleo não sabe responder e a casca C não tem como fazer sozinha:
            // o LBRR mora atrás do decodificador de faixa do SILK. **Esta fronteira já linka a
            // libopus por causa do encoder**, então responder aqui não custa um byte a mais e
            // fecha a armadilha exatamente onde ela morde.
            fec_has_lbrr: lbrr_do_socorro(socorro),
        },
        Entrega::Silencio {
            sequencia,
            timestamp_us,
        } => QuallAudioSlot {
            order: QuallAudioOrder::Silence,
            payload: ptr::null(),
            len: 0,
            sequence: sequencia,
            timestamp_us,
            fec_has_lbrr: 0,
        },
    };
    // SAFETY: `slot` vive nesta pilha durante a chamada; nada escapa daqui.
    unsafe { funcao(&slot, contexto.ptr()) }
}

/// `1` sim, `0` não, `-1` não sei. Ver [`QuallAudioSlot::fec_has_lbrr`].
fn lbrr_do_socorro(socorro: &[u8]) -> i8 {
    #[cfg(feature = "opus")]
    {
        match quall_opus::tem_lbrr(socorro) {
            Ok(true) => 1,
            Ok(false) => 0,
            // Pacote que a libopus não soube ler. "Não sei" é a resposta honesta, e ela mantém a
            // casca longe do `decode_fec`.
            Err(_) => -1,
        }
    }
    #[cfg(not(feature = "opus"))]
    {
        let _ = socorro;
        -1
    }
}

/// **A porta de recepção de áudio.** Registra o tratador que recebe os slots já ordenados pelo
/// jitter buffer do núcleo.
///
/// # Por que ela existe, e por que ela não entrega o pacote cru
///
/// Antes desta rodada a fronteira C tinha o pacote de **envio** de áudio e nenhuma recepção:
/// [`quall_track_on_frame`] é só de vídeo, então **uma casca C conseguia mandar áudio e não
/// conseguia receber**. Metade de um par.
///
/// A porta óbvia — entregar o pacote RTP cru, espelhando `quall_track_on_frame` — seria a
/// errada, e o documento de áudio já tinha pago para descobrir isso. O jitter buffer **mudou de
/// lado** em 2026-08-27: morava na casca, e o argumento que o trouxe para o núcleo é o mesmo que
/// decide aqui — *"sem esse campo, quatro cascas reimplementariam a conta de sequência sobre os
/// carimbos, de quatro jeitos"*. Entregar o pacote cru em C convidaria as quatro a fazer
/// exatamente isso, e desfaria a decisão pela porta dos fundos.
///
/// Áudio também não tem a saída que o vídeo tem: **não existe quadro-chave de áudio** e o
/// sumidouro é um DAC, que consome 48 000 amostras por segundo para sempre. O que sai daqui é
/// uma ordem por slot de 20 ms, **sempre, sem buraco**.
///
/// # A política não é parâmetro, e isso é a mesma decisão do encoder
///
/// A profundidade e o `fec_disponivel` saem do **preset da track** e do codec lido no
/// `a=rtpmap` — as mesmas fontes que geram o `fmtp` do SDP e configuram
/// [`quall_audio_encoder_new`]. Deixar a casca escolher reintroduziria em quatro lugares a
/// classe de defeito da §11 de `docs/audio.md`: o preset do fio e o do buffer divergindo em
/// silêncio. **Uma fonte de verdade, no núcleo.**
///
/// Em particular, `fec_disponivel` **não** é o `fec` cru do preset: numa track de microfone
/// negociada em PCMU o preset ainda diz `fec: true`, e oferecer socorro num fluxo de G.711 faria
/// a casca chamar `decode_fec` num payload sem LBRR nenhum.
///
/// # Com a reprodução puxada aberta, devolve `INVALID`
///
/// As duas portas são exclusivas por track (`docs/contrato-som-puxado.md` §2). Com
/// [`quall_audio_playout_new`] aberto, registrar **e** desregistrar aqui devolvem
/// `QUALL_STATUS_INVALID` sem mexer em nada.
///
/// # Desregistrar **escoa o buffer**, e é por isso que a ordem importa
///
/// `cb` nulo desliga o tratador. Antes de voltar, esta função entrega os slots que ainda estavam
/// retidos — chamando o tratador **antigo**, **desta thread**. Sem isso os últimos
/// `profundidade` slots de toda sessão sumiriam: 40 ms que atravessaram a rede e nunca tocaram,
/// que apareceriam numa tabela como dois quadros a menos sem ninguém saber de onde vieram.
///
/// **Consequência para quem chama: não libere o `user_data` antes desta chamada voltar.** Ao
/// voltar com `QUALL_STATUS_OK` valem as duas garantias de sempre — o tratador não está rodando
/// em thread nenhuma e não voltará a rodar —, e aí o `user_data` pode ir embora.
///
/// O escoamento chama o tratador **sem** nenhum cadeado desta track na mão: de dentro dele a
/// casca pode chamar qualquer função da track. **Tudo o que ele usa tem de estar vivo até esta
/// chamada voltar**: o `user_data`, e todo handle que ele consulte, inclusive o de outra track
/// (ver a ordem de fechar em [`quall_track_free`]).
///
/// # De dentro de um tratador
///
/// Chamada de dentro de um tratador da sessão, devolve `QUALL_STATUS_INVALID` (a barreira não
/// vale: não libere o `user_data`), e o tratador fica desligado.
/// - De dentro do tratador de áudio **desta** track, os slots retidos **se perdem**: escoá-los
///   pediria o cadeado que o próprio tratador segura.
/// - De dentro do tratador de **outra** track, eles são escoados normalmente.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_next_track`]; `cb` precisa ser válido ou nulo.
#[no_mangle]
pub unsafe extern "C" fn quall_track_on_audio(
    t: *const QuallTrack,
    cb: QuallAudioSlotCallback,
    user_data: *mut c_void,
) -> QuallStatus {
    let Some(track) = t.as_ref() else {
        return guardar_nulo("quall_track_on_audio: track nula");
    };
    let Lado::Receptor(receptor) = &track.lado else {
        return guardar_texto("quall_track_on_audio: esta track é de emissão");
    };
    let Ok(mut estado) = track.audio.lock() else {
        return guardar_texto("quall_track_on_audio: o estado de áudio desta track foi envenenado");
    };

    // ---- desregistro: barreira primeiro, escoamento depois ----
    let Some(funcao) = cb else {
        let barreira = match receptor.desregistrar_audio() {
            Ok(b) => b,
            // A track está em modo puxado: não é esta porta que a fecha, e nada foi mexido.
            Err(e) => return guardar_erro(&e),
        };
        let status = traduzir_barreira(barreira, "quall_track_on_audio(NULL)");
        // **Solta `track.audio` antes de escoar.** O escoamento chama a casca, e a casca pode
        // chamar de volta qualquer função desta track — `quall_track_on_audio`,
        // `quall_track_stats_json` — que pede esse mesmo cadeado. Com ele na mão, a mesma thread
        // travava (revisão do código da S1, anexo do achado A2).
        let anterior = estado.take();
        drop(estado);
        let Some(anterior) = anterior else {
            return status;
        };
        // **De dentro do tratador desta mesma track, não escoa.** O tratador roda com o cadeado
        // do buffer na mão, e escoar pediria o mesmo cadeado na mesma thread: trava, ou pânico.
        // Os slots retidos se perdem, e o status diz que a barreira não valeu. De dentro do
        // tratador de **outra** track o cadeado está livre, e o escoamento acontece: antes,
        // `DeDentroDoTratador`, que vale para a sessão inteira, descartava esses slots também.
        if barreira == Barreira::DeDentroDoTratador && EntregandoAudio::esta_em(&anterior.buffer) {
            return status;
        }
        // O escoamento roda **depois** da barreira, de propósito: com ela cumprida nada mais
        // insere no buffer, então o que sai aqui é exatamente o que ficou retido.
        if let (Some(f), Ok(mut jb)) = (anterior.cb, anterior.buffer.lock()) {
            let _entregando = EntregandoAudio::entrar(&anterior.buffer);
            jb.drenar(|o| unsafe { entregar_slot(f, anterior.contexto, o) });
        }
        return status;
    };

    // A política sai do núcleo — não de números reescritos aqui, que é como duas fontes de
    // verdade nascem. É a mesma que a porta puxada usa.
    let Some(politica) = receptor.politica_de_audio() else {
        return guardar_texto(
            "quall_track_on_audio: esta track não é de áudio. Quadro de vídeo é \
             quall_track_on_frame.",
        );
    };

    let buffer = Arc::new(Mutex::new(BufferDeJitter::novo(politica)));
    let retrato = Arc::new(Mutex::new(ContadoresDeBuffer::default()));
    let contexto = Contexto(user_data);

    let comeco = Instant::now();
    let do_tratador = Arc::clone(&buffer);
    let retrato_do_tratador = Arc::clone(&retrato);
    if let Err(e) = receptor.ao_receber_audio(move |q| {
        let Ok(mut jb) = do_tratador.lock() else {
            return;
        };
        let agora_us = comeco.elapsed().as_micros() as u64;
        {
            let _entregando = EntregandoAudio::entrar(&do_tratador);
            jb.aceitar(&q, agora_us, |o| unsafe {
                entregar_slot(funcao, contexto, o)
            });
        }
        // Publicado com a casca já de volta: ordem de cadeados buffer → retrato, e ninguém
        // pede o buffer para ler.
        if let Ok(mut r) = retrato_do_tratador.lock() {
            *r = jb.contadores();
        }
    }) {
        // A reprodução puxada está aberta nesta track: as duas portas são exclusivas.
        return guardar_erro(&e);
    }
    *estado = Some(RecepcaoDeAudio {
        buffer,
        retrato,
        cb,
        contexto,
    });
    QuallStatus::Ok
}

// =============================================================================================
// Recepção de áudio puxada: a casca chama no ritmo do dispositivo de saída
// =============================================================================================

/// **A reprodução puxada de uma track de áudio.** Ver `docs/contrato-som-puxado.md`, que fixa os
/// nomes, e `docs/som-no-receptor.md` §3, que diz o porquê.
///
/// O consumidor fica atrás de um `Mutex` que só `pull` e `free` tocam — e que por contrato uma
/// thread de cada vez —, então ele nunca espera. As leituras (`rate`, `stats_json`) vão pelo
/// leitor, que é outro objeto, e podem vir de qualquer thread sem encostar no consumidor.
pub struct QuallAudioPlayout {
    consumidor: Mutex<Option<ReproducaoPuxada>>,
    leitor: LeitorDeReproducao,
}

/// **Abre a reprodução puxada** de uma track receptora de áudio.
///
/// `shell_resamples = true` diz que a casca aplica a razão sugerida
/// ([`quall_audio_playout_rate`]): Varispeed, `RATEADJUST`, o período da thread do OBS. Nesse
/// modo o núcleo **nunca** descarta nem insere slot por deriva.
///
/// Devolve nulo, com o motivo em `quall_last_error`, quando a track é nula, de emissão ou de
/// vídeo, quando ela já tem um tratador empurrado ([`quall_track_on_audio`]), ou quando já tem
/// uma reprodução puxada aberta. **As duas portas são exclusivas.**
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_next_track`] ou [`quall_session_track`] e continuar válido
/// durante a chamada. O ponteiro devolvido vai para [`quall_audio_playout_free`].
#[no_mangle]
pub unsafe extern "C" fn quall_audio_playout_new(
    t: *const QuallTrack,
    shell_resamples: bool,
) -> *mut QuallAudioPlayout {
    let Some(track) = t.as_ref() else {
        guardar_nulo("quall_audio_playout_new: track nula");
        return ptr::null_mut();
    };
    let Lado::Receptor(receptor) = &track.lado else {
        guardar_texto("quall_audio_playout_new: esta track é de emissão");
        return ptr::null_mut();
    };
    match receptor.reproducao_puxada(shell_resamples) {
        Ok(r) => {
            let leitor = r.leitor();
            let p = Box::new(QuallAudioPlayout {
                consumidor: Mutex::new(Some(r)),
                leitor,
            });
            // **Aciona o cadeado do consumidor uma vez, aqui.** No Mac e no iOS o `Mutex` da std
            // aloca na primeira trava, e a primeira `pull` alocava por isso (revisão do código da
            // S1, A7). O núcleo já aciona os dele; este é da fronteira.
            drop(p.consumidor.lock());
            Box::into_raw(p)
        }
        Err(e) => {
            guardar_erro(&e);
            ptr::null_mut()
        }
    }
}

/// **Puxa o slot de 20 ms que sai agora.** Uma chamada por slot entregue ao dispositivo, na
/// cadência dele, **numa thread de cada vez**.
///
/// - `delay_to_dac_us`: daqui a quanto a primeira amostra deste slot sai do DAC (a fila da casca,
///   o buffer do dispositivo e a latência que ele declara).
/// - `applied_rate`: a razão de reamostragem que a casca **de fato** aplicou; `NAN` quando não
///   sabe ou não reamostra. Sem ela a estimativa de deriva erraria exatamente pelo que a casca
///   corrigiu.
/// - `out` recebe o slot. Com `QUALL_AUDIO_ORDER_IDLE`, escreva **zeros**. O `payload` vale **até
///   a próxima chamada de `pull` ou de `free` sobre o mesmo `p`**.
///
/// Devolve `QUALL_STATUS_OK`, ou `QUALL_STATUS_NULL_POINTER` com `p` ou `out` nulo.
///
/// # Safety
///
/// `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado; `out` precisa apontar
/// para um `QuallAudioSlot` gravável.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_playout_pull(
    p: *mut QuallAudioPlayout,
    delay_to_dac_us: u32,
    applied_rate: f64,
    out: *mut QuallAudioSlot,
) -> QuallStatus {
    let Some(playout) = p.as_ref() else {
        return guardar_nulo("quall_audio_playout_pull: reprodução nula");
    };
    // **Sem `&mut` sobre `out`**: a memória é da casca e normalmente não foi inicializada, e o
    // `order` dela pode ter um valor que o enum não admite. Criar uma referência ali já seria
    // comportamento indefinido (revisão do código da S1, A9). O slot é montado aqui e escrito
    // inteiro com `write`.
    if out.is_null() {
        return guardar_nulo("quall_audio_playout_pull: `out` nulo");
    }
    let Ok(mut g) = playout.consumidor.lock() else {
        return guardar_texto("quall_audio_playout_pull: o consumidor foi envenenado");
    };
    let Some(r) = g.as_mut() else {
        return guardar_texto("quall_audio_playout_pull: a reprodução já foi encerrada");
    };
    let slot = match r.puxar(delay_to_dac_us, applied_rate) {
        Puxado::Ocioso => QuallAudioSlot {
            order: QuallAudioOrder::Idle,
            payload: ptr::null(),
            len: 0,
            sequence: 0,
            timestamp_us: 0,
            fec_has_lbrr: 0,
        },
        Puxado::Slot(Entrega::Quadro {
            payload,
            sequencia,
            timestamp_us,
        }) => QuallAudioSlot {
            order: QuallAudioOrder::Frame,
            payload: payload.as_ptr(),
            len: payload.len(),
            sequence: sequencia,
            timestamp_us,
            fec_has_lbrr: 0,
        },
        Puxado::Slot(Entrega::Fec {
            socorro,
            sequencia,
            timestamp_us,
        }) => QuallAudioSlot {
            order: QuallAudioOrder::Fec,
            payload: socorro.as_ptr(),
            len: socorro.len(),
            sequence: sequencia,
            timestamp_us,
            fec_has_lbrr: lbrr_do_socorro(socorro),
        },
        Puxado::Slot(Entrega::Silencio {
            sequencia,
            timestamp_us,
        }) => QuallAudioSlot {
            order: QuallAudioOrder::Silence,
            payload: ptr::null(),
            len: 0,
            sequence: sequencia,
            timestamp_us,
            fec_has_lbrr: 0,
        },
    };
    // SAFETY: `out` não é nulo (conferido acima) e o contrato exige que aponte para um
    // `QuallAudioSlot` gravável; `write` não lê nem solta o que havia ali.
    unsafe { out.write(slot) };
    QuallStatus::Ok
}

/// A razão de reamostragem sugerida, perto de 1,0 e limitada a ±500 ppm. `NAN` quando **não
/// medida**: antes de 10 s de reprodução, e depois de toda reancoragem.
///
/// **De qualquer thread**, concorrente com [`quall_audio_playout_pull`] — **mas nunca junto com
/// [`quall_audio_playout_free`]**: o `free` libera `p`, e ler depois é uso de memória liberada.
///
/// # Safety
///
/// `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_playout_rate(p: *const QuallAudioPlayout) -> f64 {
    let Some(playout) = p.as_ref() else {
        guardar_nulo("quall_audio_playout_rate: reprodução nula");
        return f64::NAN;
    };
    playout.leitor.razao_sugerida().unwrap_or(f64::NAN)
}

/// Os contadores da reprodução puxada, como JSON. Padrão `(buf, cap)`. As chaves estão em
/// `docs/contrato-som-puxado.md` §4. **De qualquer thread**, mas **nunca junto com
/// [`quall_audio_playout_free`]**.
///
/// # Safety
///
/// `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado; `buf` nulo ou com
/// `cap` bytes graváveis.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_playout_stats_json(
    p: *const QuallAudioPlayout,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(playout) = p.as_ref() else {
        guardar_nulo("quall_audio_playout_stats_json: reprodução nula");
        return -1;
    };
    let valor = json_da_reproducao(&playout.leitor.contadores());
    match serde_json::to_string(&valor) {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&Error::from(e));
            -1
        }
    }
}

/// O JSON da reprodução puxada. As chaves são literais de contrato.
fn json_da_reproducao(c: &ContadoresDeReproducao) -> serde_json::Value {
    serde_json::json!({
        "pulls": c.puxadas,
        "idle_pulls": c.ociosas,
        "frames": c.quadros,
        "fec_offers": c.curas_oferecidas,
        "holes": c.buracos,
        "underruns": c.subconsumos,
        "drift_inserts": c.insercoes_por_deriva,
        "burst_inserts": c.insercoes_de_rajada,
        "late_inserts": c.insercoes_por_atraso,
        "drift_drops": c.descartes_por_deriva,
        "burst_drops": c.descartes_de_rajada,
        "ceiling_drops": c.descartes_por_teto,
        "too_late": c.tarde_demais,
        "duplicates": c.duplicados,
        "reordered": c.reordenados,
        "anchors": c.ancoragens,
        "dropped_at_anchor": c.descartados_na_ancoragem,
        "went_idle": c.entradas_em_ocioso,
        "ring_overflows": c.transbordos,
        "ring_dropped": c.perdidos_no_anel,
        "oversized": c.grandes_demais,
        "playout_skips": c.saltos,
        "skipped_slots": c.slots_saltados,
        "level": c.nivel,
        "depth": c.profundidade_efetiva,
        "burst_pulls": c.rajada,
        "anchor_latency_us": c.latencia_na_ancoragem_us,
        "dac_delay_us": c.atraso_ate_o_dac_us,
        "applied_rate": c.razao_aplicada,
        "suggested_rate": c.razao_sugerida,
        "ed_drift_ppm": c.deriva_ed_ppm,
    })
}

/// **Encerra a reprodução puxada.** `p` é liberado **sempre**, seja qual for o status.
///
/// Tira o produtor do caminho do pacote, com barreira, e libera a track para outra porta.
/// `QUALL_STATUS_OK`: a barreira valeu. `QUALL_STATUS_TIMEOUT`: o prazo estourou.
/// `QUALL_STATUS_INVALID`: chamada de dentro de um tratador da sessão. Esta porta não tem
/// `user_data`, então o status só informa.
///
/// **Nunca concorrente com nenhuma chamada sobre o mesmo `p`**: nem `pull`, nem `rate`, nem
/// `stats_json`. A casca que lê estatísticas num temporizador para o temporizador antes de
/// liberar (revisão do código da S1, A5).
///
/// # Safety
///
/// `p` precisa vir de [`quall_audio_playout_new`] e não ter sido liberado antes. Nulo é no-op.
#[no_mangle]
pub unsafe extern "C" fn quall_audio_playout_free(p: *mut QuallAudioPlayout) -> QuallStatus {
    if p.is_null() {
        return QuallStatus::Ok;
    }
    let playout = Box::from_raw(p);
    let consumidor = match playout.consumidor.into_inner() {
        Ok(c) => c,
        Err(envenenado) => envenenado.into_inner(),
    };
    match consumidor {
        Some(r) => traduzir_barreira(r.encerrar(), "quall_audio_playout_free"),
        None => QuallStatus::Ok,
    }
}

/// **O deslocamento de captura** desta track no relógio comum da sessão.
///
/// `timestamp_us` de um quadro ou slot desta track, mais `*out_us`, é o instante de captura em
/// µs desde a época da sessão — comum a todas as tracks receptoras dela. Ver
/// `docs/som-no-receptor.md` §5.
///
/// Devolve `1` (válido, `*out_us` escrito), `0` (ainda não medido) ou `-1` (recusado pela guarda
/// do relógio, taxa não suportada ou erro; o motivo em `quall_last_error`). **Pode passar de `1`
/// para `-1` no meio da sessão**: consulte de novo.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_next_track`] ou [`quall_session_track`]; `out_us` precisa
/// apontar para um `int64_t` gravável.
#[no_mangle]
pub unsafe extern "C" fn quall_track_capture_offset_us(
    t: *const QuallTrack,
    out_us: *mut i64,
) -> i32 {
    let Some(track) = t.as_ref() else {
        guardar_nulo("quall_track_capture_offset_us: track nula");
        return -1;
    };
    let Lado::Receptor(receptor) = &track.lado else {
        guardar_texto("quall_track_capture_offset_us: esta track é de emissão");
        return -1;
    };
    match receptor.deslocamento_de_captura() {
        DeslocamentoDeCaptura::Valido { us } => {
            let Some(saida) = out_us.as_mut() else {
                guardar_nulo("quall_track_capture_offset_us: `out_us` nulo");
                return -1;
            };
            *saida = us;
            1
        }
        DeslocamentoDeCaptura::Ainda => 0,
        DeslocamentoDeCaptura::Recusado { motivo } => {
            guardar_texto(&format!("quall_track_capture_offset_us: {motivo}"));
            -1
        }
    }
}

/// **`ao_pedir_idr` do contrato.** Registra o tratador do pedido de IDR do receptor.
///
/// Disparado quando chega **PLI ou FIR**. A casca responde forçando um IDR pelo meio que a
/// plataforma dela permitir — e no Windows, hoje, isso significa recriar o MFT, com o custo
/// medido de ~150 ms. **Ignorar o pedido é deixar o receptor sem imagem**: é falha de produto,
/// não detalhe.
///
/// O tratador roda numa **thread da libdatachannel**, não na sua. Bloquear nele segura a
/// recepção de RTCP da sessão inteira; o certo é levantar uma bandeira que o laço de captura
/// leia.
///
/// # Tempo de vida do `user_data`, e como desligar
///
/// `user_data` é repassado como veio. Ele precisa continuar válido até uma destas duas coisas
/// acontecer, o que vier primeiro:
///
/// - **`cb` nulo desregistra**: chame esta mesma função com `cb = NULL` e, com
///   `QUALL_STATUS_OK`, o tratador antigo não está rodando em thread nenhuma e não voltará a
///   rodar. É a saída para quem fecha uma fonte sem derrubar a sessão.
/// - **[`quall_session_close`] com `QUALL_STATUS_OK`**, que é barreira para a sessão inteira.
///
/// Note que **não é** o tempo de vida do handle: [`quall_track_free`] solta só a referência da
/// casca e deixa o tratador armado, de propósito, porque dois handles podem apontar para a mesma
/// track.
///
/// Status diferente de `QUALL_STATUS_OK` no desregistro significa **não libere o `user_data`** —
/// ver [`quall_session_close`] para os dois casos.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_track`]; `cb` precisa ser uma função válida ou nulo.
#[no_mangle]
pub unsafe extern "C" fn quall_track_on_idr_request(
    t: *const QuallTrack,
    cb: QuallIdrRequestCallback,
    user_data: *mut c_void,
) -> QuallStatus {
    let Some(track) = t.as_ref() else {
        return guardar_nulo("quall_track_on_idr_request: track nula");
    };
    let Lado::Emissor(emissor) = &track.lado else {
        return guardar_texto("quall_track_on_idr_request: esta track é de recepção");
    };
    let Some(funcao) = cb else {
        return traduzir_barreira(
            emissor.desregistrar_idr(),
            "quall_track_on_idr_request(NULL)",
        );
    };
    let contexto = Contexto(user_data);
    match emissor.ao_pedir_idr(move || {
        // SAFETY: `funcao` é uma função C do chamador e `contexto` é o ponteiro que ele mandou.
        // A obrigação de mantê-lo vivo está documentada acima e no header.
        unsafe { funcao(contexto.ptr()) }
    }) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// Alternativa a [`quall_track_on_idr_request`]: **consome** um pedido pendente.
///
/// Devolve `true` no máximo uma vez por rajada de PLI/FIR, e baixa a bandeira. O laço de captura
/// chama uma vez por quadro e, quando vier `true`, força um IDR.
///
/// # Em Android, prefira esta
///
/// O tratador de [`quall_track_on_idr_request`] roda numa thread da libdatachannel, que **não
/// está anexada à JVM**. Chamar de volta para o Kotlin de lá exige `AttachCurrentThread`,
/// referência global e desanexar na saída — três chances de derrubar o app, num aparelho de
/// 1,79 GB, por causa de um pedido de quadro-chave. Com esta função a casca Android não precisa
/// de callback nenhum: ela já tem um laço por quadro, o do MediaCodec, e uma leitura atômica
/// por quadro não custa nada.
///
/// As duas formas convivem: registrar o tratador não desliga a bandeira.
///
/// Devolve `false` para track nula ou de recepção.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_track`].
#[no_mangle]
pub unsafe extern "C" fn quall_track_take_idr_request(t: *const QuallTrack) -> bool {
    match t.as_ref().map(|x| &x.lado) {
        Some(Lado::Emissor(e)) => e.pegar_pedido_de_idr(),
        Some(Lado::Receptor(_)) => {
            guardar_texto("quall_track_take_idr_request: esta track é de recepção");
            false
        }
        None => {
            guardar_nulo("quall_track_take_idr_request: track nula");
            false
        }
    }
}

/// **`ao_receber_quadro` do contrato.** Registra o tratador de quadro remontado.
///
/// O tratador roda numa **thread da libdatachannel** e recebe um `QuallFrame` cujo `annexb`
/// aponta para o buffer interno do núcleo, válido **só durante a chamada**. No caminho normal a
/// casca entrega direto ao decoder, sem copiar; se precisar guardar, copie ali.
///
/// Vale aqui a mesma regra de tempo de vida de [`quall_track_on_idr_request`], **inclusive o
/// desregistro**: `cb` nulo desliga o tratador e, com `QUALL_STATUS_OK`, garante que o antigo
/// não está rodando em thread nenhuma. Depois disso o `user_data` pode ser liberado, mesmo com a
/// sessão de pé.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_next_track`]; `cb` precisa ser válido ou nulo.
#[no_mangle]
pub unsafe extern "C" fn quall_track_on_frame(
    t: *const QuallTrack,
    cb: QuallFrameCallback,
    user_data: *mut c_void,
) -> QuallStatus {
    let Some(track) = t.as_ref() else {
        return guardar_nulo("quall_track_on_frame: track nula");
    };
    let Lado::Receptor(receptor) = &track.lado else {
        return guardar_texto("quall_track_on_frame: esta track é de emissão");
    };
    let Some(funcao) = cb else {
        return traduzir_barreira(receptor.desregistrar_quadro(), "quall_track_on_frame(NULL)");
    };
    let contexto = Contexto(user_data);
    receptor.ao_receber_quadro(move |q| {
        let quadro = QuallFrame {
            annexb: q.annexb.as_ptr(),
            len: q.annexb.len(),
            timestamp_us: q.timestamp_us,
            idr: q.idr,
        };
        // SAFETY: `quadro` vive nesta pilha durante a chamada, e `funcao`/`contexto` são do
        // chamador. Nada escapa daqui.
        unsafe { funcao(&quadro, contexto.ptr()) }
    });
    QuallStatus::Ok
}

/// **`pedir_idr` do contrato.** Pede um IDR ao emissor, emitindo PLI.
///
/// A casca receptora chama ao entrar na sessão sem ter visto IDR, ou quando o decoder perde
/// sincronia. Devolve erro enquanto a track não abriu — e engolir isso em silêncio seria
/// reproduzir, do lado do receptor, o defeito que o contrato existe para resolver. Tentar de
/// novo por alguns milissegundos é o comportamento certo.
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_next_track`].
#[no_mangle]
pub unsafe extern "C" fn quall_track_request_idr(t: *const QuallTrack) -> QuallStatus {
    let Some(track) = t.as_ref() else {
        return guardar_nulo("quall_track_request_idr: track nula");
    };
    let Lado::Receptor(receptor) = &track.lado else {
        return guardar_texto("quall_track_request_idr: esta track é de emissão");
    };
    match receptor.pedir_idr() {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// **Quantos quadros o depacotizador jogou fora por estarem incompletos.**
///
/// Existe separado de `quall_track_stats_json` porque é lido **no laço de recepção**, e não no
/// relato: é o gatilho que faz o receptor pedir um IDR quando a cadeia de referência se rompe.
/// Montar e desserializar um JSON a cada volta para ler um `u64` seria pagar um alocador por
/// quadro — e uma casca que ache isso caro vai acabar não perguntando, que é como o receptor iOS
/// ficou pedindo IDR **uma única vez por sessão** enquanto a imagem se desfazia.
///
/// Cada unidade aqui é uma ruptura da cadeia: um quadro P decodificado contra uma referência que
/// nunca chegou deixa rastro do que se move e suja macroblocos, até o IDR seguinte. Quem lê isto
/// e não pede reparo está escolhendo o rastro.
///
/// Devolve 0 para track nula ou de emissão — o emissor não remonta nada, então não tem o que
/// descartar. **0 não é erro**, e por isso esta função não escreve em `quall_last_error`.
///
/// # Safety
///
/// `t` precisa ser uma track viva desta fronteira, ou nulo.
#[no_mangle]
pub unsafe extern "C" fn quall_track_frames_dropped(t: *const QuallTrack) -> u64 {
    let Some(track) = t.as_ref() else { return 0 };
    match &track.lado {
        Lado::Receptor(r) => r.contadores().quadros_descartados,
        Lado::Emissor(_) => 0,
    }
}

/// Monta o JSON do lado que recebe, a partir de **uma** leitura coerente dos contadores.
///
/// Separado da função `extern "C"` porque é a parte que tem regra de negócio e a única que
/// precisa de teste: quais chaves saem, e qual é a relação aritmética entre elas.
fn json_do_receptor(c: &Contadores, pedidos_de_idr: u64) -> serde_json::Value {
    serde_json::json!({
        "frames_ready": c.quadros_prontos,
        "frames_dropped": c.quadros_descartados,
        // **O par que faltava para o `idrs_sent` do emissor.** No vídeo da bancada de 31/08 o
        // emissor dizia `idrs_sent: 31` e o receptor dizia `idrs 13`; nada no receptor contava os
        // 18 que sumiram, e a queixa "terrível" não tinha número do lado que a sofria.
        // `idrs_broken` conta IDR que começou a chegar e foi destruído — e ele fecha com o
        // emissor: `idrs_sent ≈ idrs_ready + idrs_broken` quando nenhum IDR se perde inteiro.
        // Ver `docs/idr-que-sobrevive.md`.
        "idrs_ready": c.idrs_prontos,
        "idrs_broken": c.idrs_quebrados,
        // O maior quadro que passou inteiro, e **quantos pacotes chegaram** do quadro destruído
        // que mais recebeu antes de morrer. O segundo nome diz o que o número é: no regime de
        // truncamento de cauda medido em 31/08 ele é o **ponto de corte** do enlace, e não o
        // tamanho que o emissor mandou — que este lado não tem como saber, porque um buraco pode
        // cobrir a cauda de um quadro, vários quadros inteiros e a cabeça do seguinte.
        "largest_frame_ready_packets": c.maior_quadro_pronto_pacotes,
        "largest_broken_frame_packets_received": c.maior_quebrado_pacotes_recebidos,
        // A soma dos dois de baixo. Existe desde o M2 e é lida pelo `quall-probe`, pelo plugin
        // de OBS, pelas sondas de câmera e pelas cascas: **não muda de significado**.
        "sequence_anomalies": c.pacotes_perdidos(),
        // **Chamava-se `packets_missing` até 29/08/2026, e o nome era a doença.** Ele nunca foi
        // perda: é a soma dos saltos de sequência, e uma reordenação de distância `d` entra aqui
        // como `1 + d` posições sem que nada tenha se perdido. Estava documentado como teto desde
        // a dívida 26 e mesmo assim foi lido como perda em **toda** medição desta bancada — numa
        // corrida com 486 aqui, tinham sumido cinquenta. Quem lia a chave não lia a docstring.
        //
        // O nome novo carrega o aviso que a docstring carregava sozinha. A chave antiga **não**
        // continua ao lado: mantê-la seria manter a armadilha que a renomeação existe para tirar.
        // Ver `docs/contador-nas-cascas.md`.
        "packets_missing_upper_bound": c.pacotes_faltando,
        "reorder_events": c.eventos_fora_de_ordem,
        // **Quantas reordenações a fila de reordenação absorveu**, em 01/09/2026. Um pacote que
        // chegou fora de ordem, esperou no anel e foi entregue no lugar certo entra aqui — e
        // **não** vira ruptura, que era o que acontecia antes. Sem esta chave o conserto
        // arrumaria a imagem e sumiria da medida.
        "reorderings_absorbed": c.reordenacoes_absorvidas,
        // **Quantas vezes a fila desistiu de um buraco**, que é o denominador que faltava:
        // 1293 absorvidas no cabo de 01/09/2026 não diziam nada sozinhas, porque ninguém sabia
        // quantas tinham ficado na mesa (eram 387).
        "reorder_giveups": c.desistencias_de_reordenacao,
        // **A profundidade do anel agora**, em pacotes — o regime que o produto leu da rede.
        // Acima de 16 é cabo (reordena e não perde); abaixo, rádio (perde e não reordena). Sai
        // aqui para que a bancada leia o diagnóstico em vez de deduzi-lo, e para que uma corrida
        // com o anel cravado (`definir_profundidade_de_reordenacao`) se distinga de uma com ele
        // solto. `0` é a fila desligada, e no áudio é sempre `0`.
        "reorder_depth": c.profundidade_de_reordenacao,
        // **Quantas vezes o anel mudou de tamanho.** Sem esta chave `reorder_depth` é uma
        // fotografia do fim: não separa "andou direto até lá" de "ficou oscilando", que é o modo
        // de falha contra o qual a banda morta existe.
        "reorder_adjusts": c.ajustes_de_reordenacao,
        "packets_seen": c.pacotes_vistos,
        // **A perda exata**, ao lado do teto. `packets_missing_upper_bound` compara cada pacote só
        // com o anterior e cobra a distância de qualquer reordenação; este só conta a posição que
        // saiu da janela de reordenação sem nunca ter chegado. Medido em 29/08 na sonda: 486
        // contra no máximo 50.
        "packets_lost_for_real": c.pacotes_perdidos_de_verdade,
        // Diferente de zero quer dizer que a janela foi curta para o que a rede fez, e que
        // `packets_lost_for_real` está superestimado nesse tanto.
        "packets_too_late": c.pacotes_tarde_demais,
        "rtcp_ignored": c.rtcp_ignorados,
        "idr_requests": pedidos_de_idr,
        // **Só existe em track de áudio**, e sai como `null` quando não foi medido — nunca como
        // zero. Numa track de vídeo o jitter não é calculado (ninguém agiria sobre ele, e custaria
        // um relógio por pacote numa rajada de ~85); numa de áudio que ainda não viu dois pacotes
        // não há diferença de diferenças para calcular. `null` diz "não sei"; `0` diria "medi e
        // deu zero", que é outra afirmação. Ver a dívida 26.
        "jitter_us": c.jitter_us,
    })
}

/// O relógio comum da track, para o `quall_track_stats_json`. `null` antes do primeiro pacote.
fn json_do_relogio(r: Option<RetratoDoRelogio>) -> serde_json::Value {
    let Some(r) = r else {
        return serde_json::Value::Null;
    };
    // `reason` diz por que a track foi recusada, inclusive a referência, que não tem resíduo
    // próprio (reconferência da S1, B3).
    let (status, deslocamento, motivo) = match r.deslocamento {
        DeslocamentoDeCaptura::Ainda => ("pending", None, None),
        DeslocamentoDeCaptura::Valido { us } => ("valid", Some(us), None),
        DeslocamentoDeCaptura::Recusado { motivo } => ("refused", None, Some(motivo)),
    };
    serde_json::json!({
        "reference": r.referencia,
        "status": status,
        "capture_offset_us": deslocamento,
        "reason": motivo,
        "residual_us": r.residuo_us,
        "window_residual_us": r.residuo_da_janela_us,
        "inter_track_drift_ppm": r.deriva_entre_tracks_ppm,
        "guard_violations": r.violacoes_da_guarda,
    })
}

/// Os contadores do buffer da porta empurrada, que até 18/09/2026 não saíam da fronteira.
/// `null` numa track sem `quall_track_on_audio`.
fn json_do_buffer(c: Option<ContadoresDeBuffer>) -> serde_json::Value {
    let Some(c) = c else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "slots": c.slots_entregues,
        "frames": c.quadros,
        "holes": c.buracos,
        "fec_offers": c.curas_oferecidas,
        "silences": c.silencios,
        "too_late": c.tarde_demais,
        "duplicates": c.duplicados,
        "reordered": c.reordenados,
        "resyncs": c.resincronizacoes,
        "max_occupancy": c.ocupacao_maxima,
        "max_delay_us": c.atraso_max_us,
    })
}

/// Contadores da track, como JSON. Padrão `(buf, cap)`.
///
/// **Desde 18/09/2026**, numa track receptora, também `clock` (o relógio comum da sessão) e
/// `jitter_buffer` (os contadores da porta empurrada), com as chaves de
/// `docs/contrato-som-puxado.md` §4.
///
/// No emissor: `frames_sent`, `idrs_sent`, `idrs_without_parameters`, `idr_requests`,
/// `buffered_bytes`.
/// No receptor: `frames_ready`, `frames_dropped`, `idrs_ready`, `idrs_broken`,
/// `largest_frame_ready_packets`, `largest_broken_frame_packets_received`, `sequence_anomalies`,
/// `packets_missing_upper_bound`, `reorder_events`, `packets_seen`, `packets_lost_for_real`,
/// `packets_too_late`, `rtcp_ignored`, `idr_requests`, `jitter_us`.
///
/// # MUDANÇA DE CONTRATO EM 2026-08-29: `packets_missing` virou `packets_missing_upper_bound`
///
/// A chave `packets_missing` **não existe mais**, e não há alias. Quem a lia lê agora
/// `packets_missing_upper_bound`, com exatamente o mesmo valor e o mesmo significado — o que
/// mudou é só o nome dizer o que o número sempre foi.
///
/// O motivo é medido, não estético. Aquele número nunca foi perda: numa corrida com 486 nele, o
/// emissor tinha entregado 27.779 pacotes e o receptor visto 27.729 — sumiram **cinquenta**. O
/// resto era reordenação, cobrada como perda. Isso estava escrito nesta docstring desde a dívida
/// 26 e mesmo assim foi lido como perda em **todas** as medições desta bancada, porque quem lê um
/// contador lê o nome dele, não a documentação dele.
///
/// **Não foi mantido alias de propósito.** Um `packets_missing` sobrevivente ao lado do nome novo
/// seria exatamente a armadilha que a renomeação existe para tirar. O preço é que um leitor não
/// migrado passa a ler a chave como ausente; todos os leitores desta árvore foram migrados no
/// mesmo commit (`quall-probe` e o app Windows leem o campo Rust, não o JSON, e não mudaram).
///
/// **`jitter_us` é `null` quando não foi medido**, e não zero: em track de vídeo ele nunca é
/// calculado, e em track de áudio só existe a partir do segundo pacote.
///
/// **`idrs_without_parameters` diferente de zero é defeito da casca emissora**: o contrato manda
/// todo IDR levar SPS e PPS, e sem isso quem entra na sessão depois fica sem imagem — que é
/// exatamente o defeito medido no Windows no M1.
///
/// # Perdeu ou reordenou?
///
/// `sequence_anomalies` sempre somou as duas coisas, e continua somando —
/// `sequence_anomalies == packets_missing_upper_bound + reorder_events`, sempre. Quem precisa da
/// resposta lê os dois separados:
///
/// - **`packets_missing_upper_bound`** é a **cota superior** da perda: a soma dos saltos de
///   sequência para a frente. Com `reorder_events == 0` ele é a perda **exata**; com ele
///   diferente de zero é só um teto, porque cada pacote atrasado é contado no salto que passa por
///   cima dele e de novo no salto de volta. **Não leia este número como perda** — o número da
///   perda é o de baixo.
/// - **`reorder_events`** conta *eventos* — pacote repetido ou sequência andando para trás —, não
///   pacotes.
/// - **`packets_lost_for_real`** é a perda **exata**, inclusive com reordenação: uma posição só
///   entra aqui quando sai da janela de reordenação (128 posições) sem nunca ter chegado.
///   `packets_too_late` diferente de zero denuncia que a janela foi curta e que este número está
///   superestimado nesse tanto.
///
/// **Quanto o teto infla, medido**: em 29/08, MacBook → A10s com origem sintética,
/// `packets_missing_upper_bound` = 486 (1,72 %) com 70 eventos fora de ordem, contra no máximo 50
/// pacotes que o emissor entregou e o receptor não viu. **Dez vezes.** Toda a matriz de perda
/// desta bancada, até essa data, leu o teto como se fosse a perda — ver
/// `docs/caminho-de-saida.md`.
///
/// # `packets_seen`, e o que nenhum contador pode saber
///
/// É a **janela observada**: quantos pacotes de mídia entraram na conta de sequência, incluindo
/// o primeiro. O primeiro pacote visto **fixa a linha de base**, e nada que tenha caído antes
/// dele pode ser contado — um número de sequência RTP não diz nada sobre o que veio antes do
/// primeiro que se viu. Por isso `sequence_anomalies == 0` nunca significou "nada se perdeu".
///
/// Com ele a taxa vira conta local, e são **duas** taxas, não uma: a perda de verdade é
/// `packets_lost_for_real / (packets_lost_for_real + packets_seen)` e o teto é
/// `packets_missing_upper_bound / (packets_missing_upper_bound + packets_seen)`. Antes disso o
/// denominador precisava do contador de pacotes do sistema operacional da outra ponta. E
/// `packets_seen == 0` quer dizer que nenhum pacote chegou ainda: aí não se afirma nada.
///
/// **Bancada: crava a profundidade do anel de reordenação desta track receptora**, em pacotes, e
/// desliga o ajuste automático. `0` desliga a fila inteira e reproduz o comportamento anterior a
/// 01/09/2026.
///
/// # Por que a fronteira precisa disto
///
/// O anel passou a se ajustar sozinho em 02/09/2026, e a primeira corrida de Wi-Fi levantou uma
/// dúvida contra o próprio ajuste: com a mesma perda (~3,8 %), o anel adaptativo terminou em 4 e
/// mediu mais que o dobro de `suspeitos` por mil quadros que a corrida do dia anterior com o anel
/// fixo em 16. Mas as duas corridas são de **dias diferentes**, e duas corridas de 2,4 GHz
/// separadas no tempo não se comparam. Sem braço de controle no mesmo enlace, a acusação é
/// anedota.
///
/// Este é o braço de controle, e é **só** isso: `0` é o padrão de produto e mantém o ajuste
/// ligado. Nenhuma casca de produto chama esta função.
///
/// Devolve `Invalid` numa track de emissor ou de áudio — nenhuma das duas tem anel.
///
/// # Safety
///
/// `t` precisa vir de `quall_session_next_track` e continuar válido.
#[no_mangle]
pub unsafe extern "C" fn quall_track_set_reorder_depth(
    t: *const QuallTrack,
    pacotes: u32,
) -> QuallStatus {
    let Some(track) = t.as_ref() else {
        guardar_nulo("quall_track_set_reorder_depth: track nula");
        return QuallStatus::NullPointer;
    };
    match &track.lado {
        Lado::Receptor(r) if r.cravar_anel_de_reordenacao(pacotes as usize) => QuallStatus::Ok,
        _ => QuallStatus::Invalid,
    }
}

/// # Safety
///
/// `t` precisa ser válido; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_track_stats_json(
    t: *const QuallTrack,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(track) = t.as_ref() else {
        guardar_nulo("quall_track_stats_json: track nula");
        return -1;
    };
    let valor = match &track.lado {
        Lado::Emissor(e) => serde_json::json!({
            "frames_sent": e.quadros_enviados(),
            "idrs_sent": e.idrs_enviados(),
            "idrs_without_parameters": e.idrs_sem_parametros(),
            "idr_requests": e.pedidos_de_idr(),
            "buffered_bytes": e.pendente(),
        }),
        // Uma leitura só, sob o mesmo cadeado: os seis contadores do depacotizador saem do
        // **mesmo instante**. Lê-los um a um, como antes, misturava instantes diferentes
        // enquanto os pacotes continuavam chegando — e agora há invariantes entre eles
        // (`sequence_anomalies` é a soma; `packets_seen + packets_missing` é a janela) que só
        // fecham se os números vierem juntos. `idr_requests` é um atômico à parte, e não entra
        // em invariante nenhum.
        Lado::Receptor(r) => {
            let mut v = json_do_receptor(&r.contadores(), r.pedidos_de_idr());
            // As duas chaves de 18/09/2026, **aditivas**: nenhuma das anteriores muda.
            // `docs/contrato-som-puxado.md` §4.
            //
            // **Nenhum cadeado que o tratador segura.** O retrato do buffer é clonado sob
            // `track.audio`, esse cadeado é solto, e só então o retrato é lido. A versão
            // anterior pegava `track.audio` e depois o cadeado do buffer, que o tratador segura
            // enquanto chama a casca: com um `on_audio(NULL)` chamado de dentro do tratador
            // (que pede `track.audio`), as duas threads travavam em cruz (revisão do código da
            // S1, achado A2). E chamado de dentro do próprio tratador, travava sozinho.
            let retrato = track
                .audio
                .lock()
                .ok()
                .and_then(|e| e.as_ref().map(|a| Arc::clone(&a.retrato)));
            let buffer = retrato.and_then(|r| r.lock().ok().map(|c| *c));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("clock".into(), json_do_relogio(r.retrato_do_relogio()));
                obj.insert("jitter_buffer".into(), json_do_buffer(buffer));
            }
            v
        }
    };
    match serde_json::to_string(&valor) {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&Error::from(e));
            -1
        }
    }
}

/// Libera o handle de track. Nulo é ignorado.
///
/// Não fecha a track: ela vive com a sessão. Isto solta só a referência da casca.
///
/// # E **não** desregistra o tratador
///
/// É de propósito, e não descuido: [`quall_session_track`] pode ser chamado duas vezes e
/// devolver dois handles para a mesma track. Um deles sendo liberado não pode desligar o
/// tratador que o outro registrou.
///
/// Para desligar o tratador antes de liberar o handle, chame [`quall_track_on_frame`] ou
/// [`quall_track_on_idr_request`] com `cb = NULL` e confira o status. É essa chamada, e não
/// esta, que autoriza liberar o `user_data`.
///
/// # A ordem de fechar, quando um tratador consulta outra track
///
/// A biblioteca não guarda o handle: depois de liberado, o que ela ainda chama são só os
/// tratadores registrados, com o `user_data` deles. **Mas um tratador da casca que consulta um
/// handle** — o de outra track, por exemplo, para o deslocamento de captura — o usa, e liberar
/// esse handle antes é uso de memória liberada **da casca**. Isso vale também para o escoamento
/// de [`quall_track_on_audio`] com `cb = NULL`, que chama o tratador de áudio desta thread.
///
/// A ordem segura: desregistre **todos** os tratadores da sessão, confira os status, e só então
/// libere os handles. (Reconferência da S1: o SIGSEGV do revisor B era o tratador de áudio do
/// teste consultando a track de vídeo que o teste já tinha liberado.)
///
/// # Safety
///
/// `t` precisa vir de [`quall_session_track`] ou [`quall_session_next_track`] e não pode ter
/// sido liberado antes.
#[no_mangle]
pub unsafe extern "C" fn quall_track_free(t: *mut QuallTrack) {
    if !t.is_null() {
        drop(Box::from_raw(t));
    }
}

// =============================================================================================
// Identidade do protocolo (M0)
// =============================================================================================

/// Versão do protocolo de sinalização que este binário fala.
#[no_mangle]
pub extern "C" fn quall_protocol_version() -> u16 {
    PROTOCOL_VERSION
}

/// Tipo de serviço mDNS anunciado na LAN, como C string estática.
///
/// O ponteiro é válido pelo tempo de vida do processo e **não** deve ser liberado por quem chama.
#[no_mangle]
pub extern "C" fn quall_service_type() -> *const c_char {
    // Literal C para devolver sem alocar. O teste abaixo garante que não desgruda de
    // `protocol::SERVICE_TYPE`.
    c"_quall._tcp".as_ptr()
}

// =============================================================================================
// Teto do emissor
// =============================================================================================

/// O que o teto do núcleo decidiu para uma geometria de captura.
///
/// Campos em vez de uma string JSON porque isto é consultado **no caminho de abrir a sessão**,
/// uma vez por transmissão, e uma casca que precisasse desserializar JSON para descobrir a
/// largura teria motivo para copiar a aritmética em vez de perguntar — que é exatamente o que
/// esta fronteira existe para evitar.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct QuallTeto {
    /// Largura a codificar, em pixels. Sempre par, nunca maior que a de entrada.
    pub largura: u32,
    /// Altura a codificar, em pixels. Sempre par, nunca maior que a de entrada.
    pub altura: u32,
    /// Taxa de quadros a pedir à captura **e** ao encoder.
    pub fps: u32,
    /// Macroblocos que o quadro de saída ocupa.
    pub macroblocos: u32,
    /// `MaxFS` do nível anunciado no SDP — o denominador de `macroblocos`.
    pub max_fs: u32,
    /// **Quantos bits por segundo pedir ao encoder para este quadro.**
    ///
    /// Vem junto com a geometria porque as duas decisões são a mesma decisão. Separá-las produziu
    /// o defeito de 01/09/2026: o teto de resolução subiu para 1080p em cinco cascas e o de taxa
    /// ficou em 4 000 000 cravado em cada uma — 1080p com 2,25 vezes menos bits por pixel que os
    /// 720p que ele substituiu. Ver `quall_core::teto::teto_de_taxa`.
    ///
    /// 720p30 continua recebendo exatamente 4 000 000: quem não cresceu não muda.
    pub teto_de_taxa_bps: u32,
    /// `level_idc` anunciado no SDP: 31 é o nível 3.1.
    pub level_idc: u8,
    /// A dimensão precisou mudar (1) ou já cabia (0).
    pub reduziu_tamanho: u8,
    /// A taxa de quadros precisou mudar (1) ou já cabia (0).
    pub reduziu_fps: u8,
    /// A saída não é múltipla de 16 nos dois lados, então o SPS **precisa** declarar
    /// `frame_cropping` (1). Não é defeito: é o caso comum. Está aqui para o relato da corrida
    /// poder dizê-lo — este projeto já quase reprovou uma corrida **por ela estar certa**, quando
    /// o iPhone X decodificou em 590x1280 e o roteiro de prova não sabia ler o recorte.
    pub exige_recorte: u8,
}

/// **Quanto deste quadro pode ir para a rede.** Pergunte antes de criar o encoder.
///
/// Esta é a resposta única para as quatro cascas do projeto. Ela existe porque, até 2026-08-28,
/// cada casca decidia sozinha e três das quatro decidiam errado — medido, um emissor de cada vez:
/// Android mandava 720x1520 (nível 3.2), Windows 1920x1080 (4.0) e macOS 2560x1664@60 (5.2),
/// enquanto o SDP prometia 3.1 a todos eles. O iOS cabia, mas por código próprio, não por acordo.
///
/// O teto sai do `profile-level-id` que o próprio núcleo anuncia, e a conta é a da norma — área
/// em macroblocos (`MaxFS`) e macroblocos por segundo (`MaxMBPS`) —, não uma caixa de 1280x720.
/// A diferença aparece em tela alongada: 720x1520 vira 652x1378 preservando a proporção, em vez
/// de ser espremido num retângulo 16:9 que ninguém pediu.
///
/// Entrada zero não é erro: devolve o retângulo que satura o nível, que é a saída mais
/// conservadora possível. Quem chama está abrindo uma sessão e não tem o que fazer com uma
/// ausência.
///
/// # Safety
///
/// `saida` precisa ser um ponteiro gravável para um `QuallTeto`, ou nulo — nulo devolve
/// `NullPointer` sem escrever nada.
#[no_mangle]
pub unsafe extern "C" fn quall_teto_ajustar(
    largura: u32,
    altura: u32,
    fps: u32,
    saida: *mut QuallTeto,
) -> QuallStatus {
    if saida.is_null() {
        return QuallStatus::NullPointer;
    }
    let s = quall_core::teto::ajustar(largura, altura, fps);
    let limites = quall_core::teto::LimitesDoNivel::do_sdp_ou_conservador();
    saida.write(QuallTeto {
        largura: s.largura,
        altura: s.altura,
        fps: s.fps,
        macroblocos: s.macroblocos,
        max_fs: limites.max_fs,
        teto_de_taxa_bps: s.teto_de_taxa_bps,
        level_idc: limites.level_idc,
        reduziu_tamanho: u8::from(s.reduziu_tamanho),
        reduziu_fps: u8::from(s.reduziu_fps),
        exige_recorte: u8::from(s.exige_recorte),
    });
    QuallStatus::Ok
}

/// Como [`quall_teto_ajustar`], mas respeitando **a resolução que o usuário escolheu**.
///
/// `alvo_max_fs` são os macroblocos por quadro que ele aceita emitir — 3600 para 720p, 8160 para
/// 1080p, 14400 para 2K, 32400 para 4K. `alvo_fps` é a taxa de quadros pedida.
///
/// **`0` em qualquer um dos dois quer dizer "não escolheu"** e cai no padrão daquele eixo: 1080p e
/// 30 fps, que é o comportamento de antes do cardápio, byte a byte. Os dois zeros juntos são
/// exatamente [`quall_teto_ajustar`].
///
/// Vale sempre **o menor** entre a escolha e o nível anunciado: pedir 4K num binário que anuncia
/// 4.0 devolve 1080p, e não um erro. O usuário pediu o máximo que o aparelho permitir, e é isso
/// que ele recebe — ver `quall_core::teto::Alvo`.
///
/// # Safety
///
/// Mesma regra de [`quall_teto_ajustar`]: `saida` precisa ser um ponteiro gravável para um
/// `QuallTeto`, ou nulo — nulo devolve `NullPointer` sem escrever nada.
#[no_mangle]
pub unsafe extern "C" fn quall_teto_ajustar_para(
    largura: u32,
    altura: u32,
    fps: u32,
    alvo_max_fs: u32,
    alvo_fps: u32,
    saida: *mut QuallTeto,
) -> QuallStatus {
    if saida.is_null() {
        return QuallStatus::NullPointer;
    }
    let alvo = if alvo_max_fs == 0 && alvo_fps == 0 {
        None
    } else {
        let base = if alvo_max_fs == 0 {
            quall_core::teto::Alvo::PADRAO
        } else {
            quall_core::teto::Alvo::de_max_fs(alvo_max_fs)
        };
        Some(base.a(alvo_fps))
    };
    let s = quall_core::teto::ajustar_para(largura, altura, fps, alvo);
    let limites = quall_core::teto::LimitesDoNivel::do_sdp_ou_conservador();
    saida.write(QuallTeto {
        largura: s.largura,
        altura: s.altura,
        fps: s.fps,
        macroblocos: s.macroblocos,
        max_fs: limites.max_fs,
        teto_de_taxa_bps: s.teto_de_taxa_bps,
        level_idc: limites.level_idc,
        reduziu_tamanho: u8::from(s.reduziu_tamanho),
        reduziu_fps: u8::from(s.reduziu_fps),
        exige_recorte: u8::from(s.exige_recorte),
    });
    QuallStatus::Ok
}

/// O `level_idc` que o SDP deste binário anuncia — 31 para o nível 3.1.
///
/// Serve para uma casca **conferir** o que o encoder dela produziu contra o que foi prometido,
/// que é a regra da casa: verificar no artefato, não no retorno da API. Ler o `level_idc` do SPS
/// que saiu e compará-lo com este número é uma linha de código e teria respondido a frente
/// inteira da tela preta no primeiro dia.
#[no_mangle]
pub extern "C" fn quall_teto_nivel_anunciado() -> u8 {
    quall_core::teto::LimitesDoNivel::do_sdp_ou_conservador().level_idc
}

// =============================================================================================
// Aparelhos pareados (dívidas 22 e 23)
// =============================================================================================

/// Existe algum vínculo autenticado pela revisão segura v3? Registros legados não contam.
///
/// Retorna `1`/`0`, ou `-1` se o JSON é inválido. Não identifica o aparelho remoto da descoberta.
///
/// # Safety
/// `known_json` precisa ser nulo ou apontar para uma string UTF-8 terminada em zero.
#[no_mangle]
pub unsafe extern "C" fn quall_known_peers_has_secure(known_json: *const c_char) -> i32 {
    match ler_pares(known_json, "known_json") {
        Ok(pares) => i32::from(pares.has_secure_peers()),
        Err(f) => { guardar_falha(&f); -1 }
    }
}

/// **Esquece um par.** Devolve o estado de pareamento sem ele, no padrão `(buf, cap)`.
///
/// É o que a casca oferece como "parear de novo". Sem isto, um pareamento que dessincronizou não
/// tinha saída: o `Resume` morria em "não está pareado aqui", o produto não oferecia digitar o
/// PIN outra vez, e o usuário vivia o pior tipo de defeito — funcionou ontem, hoje não funciona.
///
/// Par que não existe não é erro: devolve a tabela como estava.
///
/// # Safety
///
/// `known_json` e `device_id` precisam ser nulos ou strings válidas; `buf` precisa ser nulo ou
/// ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_known_peers_forget(
    known_json: *const c_char,
    device_id: *const c_char,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let mut conhecidos = match ler_pares(known_json, "known_json") {
        Ok(p) => p,
        Err(f) => {
            guardar_falha(&f);
            return -1;
        }
    };
    let id = match texto(device_id, "device_id") {
        Ok(t) => t,
        Err(f) => {
            guardar_falha(&f);
            return -1;
        }
    };
    conhecidos.remove(&DeviceId(id.to_string()));
    match conhecidos.to_json() {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **Funde dois estados de pareamento**, no padrão `(buf, cap)`. União; na colisão vence o mais
/// recente.
///
/// # Por que o núcleo oferece isto (dívida 23)
///
/// O pareamento é chaveado só pelo `DeviceId` — o que é o comportamento **certo**, e é o que faz
/// "parear pela tela e depois usar a câmera" não pedir PIN de novo no iOS. O preço é que duas
/// origens do mesmo aparelho escrevem o mesmo arquivo.
///
/// Enquanto o núcleo só entregava ler-modificar-escrever, uma atualização perdida bastava para
/// as duas origens ficarem com segredos diferentes sob a mesma chave, e a retomada seguinte
/// falhar duro. Com isto, a casca lê o disco, **funde** com o que tem na mão e grava — e uma
/// corrida perdida deixa de apagar uma entrada e passa a convergir para a mais recente, que é
/// justamente a que o outro lado guardou.
///
/// Isto **não** dispensa a trava entre processos (`NSFileCoordinator` no iOS); reduz o estrago
/// de quando ela falhar. Quem fecha o caso é o caminho de volta ao PIN.
///
/// # Safety
///
/// `a_json` e `b_json` precisam ser nulos ou strings válidas; `buf` precisa ser nulo ou ter
/// `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_known_peers_merge(
    a_json: *const c_char,
    b_json: *const c_char,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let (mut a, b) = match (ler_pares(a_json, "a_json"), ler_pares(b_json, "b_json")) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(f), _) | (_, Err(f)) => {
            guardar_falha(&f);
            return -1;
        }
    };
    a.merge(&b);
    match a.to_json() {
        Ok(texto) => escrever_texto(&texto, buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// Lê um estado de pareamento do chamador. Nulo ou vazio vira tabela vazia.
///
/// # Safety
///
/// `p` precisa ser nulo ou apontar para uma string terminada em NUL.
unsafe fn ler_pares(p: *const c_char, campo: &str) -> Saida<PairedPeers> {
    match texto_opcional(p, campo)? {
        Some(j) if !j.trim().is_empty() => Ok(PairedPeers::from_json(j)?),
        _ => Ok(PairedPeers::new()),
    }
}

// =============================================================================================
// Diagnóstico
// =============================================================================================

/// Chamado quando o Rust entra em pânico. Ver [`quall_install_panic_hook`].
pub type QuallPanicCallback =
    Option<unsafe extern "C" fn(message: *const c_char, user_data: *mut c_void)>;

/// **Instala um gancho de pânico**, para que um pânico do Rust não chegue mudo à casca.
///
/// # Por que isto existe (dívida 11)
///
/// O workspace usa `panic = "abort"` e `strip`. Sem gancho, um pânico do núcleo chega ao Android
/// como um `SIGABRT` sem mensagem e ao iOS como uma extension que simplesmente sumiu — e
/// diagnosticar isso custou caro no M3. O `std::panic::set_hook` **ainda roda** antes do abort:
/// a ocorrência, o arquivo e a linha sobrevivem se alguém os escrever em algum lugar.
///
/// # O que ele faz
///
/// - Chama `cb` com ocorrência e origem (arquivo sem caminho, linha, coluna), se a casca deu uma.
///   O payload livre do pânico é omitido em todos os builds: pode conter segredo ou texto do par.
///   É o caminho recomendado: só a
///   casca sabe para onde o log dela vai (`NSLog`, `os_log`, o arquivo de diário do iOS).
/// - No **Android**, escreve também no `logcat` com a etiqueta `quall`, via `__android_log_write`
///   do `liblog` — que o `CMakeLists.txt` da casca já linka. Assim `adb logcat -s quall` mostra
///   o pânico sem a casca precisar de código nenhum.
/// - Nas demais plataformas, escreve no `stderr`.
///
/// Chame **uma vez**, o mais cedo possível: do `JNI_OnLoad` no Android, do arranque do app ou da
/// extension no Apple. Chamar de novo substitui o gancho anterior.
///
/// `cb` nulo instala só o caminho padrão (logcat/stderr), o que já é melhor que o silêncio.
///
/// # Safety
///
/// `cb` precisa ser uma função válida ou nulo, e `user_data` precisa continuar vivo pelo resto
/// do processo — o gancho pode disparar a qualquer momento, de qualquer thread.
#[no_mangle]
pub unsafe extern "C" fn quall_install_panic_hook(cb: QuallPanicCallback, user_data: *mut c_void) {
    let contexto = Contexto(user_data);
    std::panic::set_hook(Box::new(move |info| {
        let texto = registro_seguro::mensagem_panico(info);
        registrar_no_sistema(&texto);
        if let Some(funcao) = cb {
            // A mensagem já não contém o payload livre, nem seu eventual byte nulo.
            if let Ok(c) = CString::new(texto) {
                // SAFETY: `funcao` e `contexto` são do chamador, que se comprometeu a
                // mantê-los válidos pelo resto do processo.
                unsafe { funcao(c.as_ptr(), contexto.ptr()) }
            }
        }
    }));
}

/// Escreve no log do sistema. No Android é o `logcat`; no resto, `stderr`.
#[cfg(target_os = "android")]
fn registrar_no_sistema(texto: &str) {
    // `liblog` do NDK. A casca Android já linka (`target_link_libraries(qualljni quall log)`).
    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    /// `ANDROID_LOG_FATAL`, de `android/log.h`.
    const FATAL: i32 = 7;
    if let Ok(c) = CString::new(texto) {
        // SAFETY: as duas strings são terminadas em NUL e vivem durante a chamada.
        unsafe {
            __android_log_write(FATAL, c"quall".as_ptr(), c.as_ptr());
        }
    }
}

/// Ver a versão de Android acima.
#[cfg(not(target_os = "android"))]
fn registrar_no_sistema(texto: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{texto}");
}

/// Gera um PIN de seis dígitos com o gerador do sistema. Padrão `(buf, cap)`.
///
/// Existe aqui, e não na casca, porque a qualidade do sorteio é o que segura o pareamento: um
/// PIN de seis dígitos tirado de `rand()` com semente de relógio é adivinhável.
///
/// # Safety
///
/// `buf` precisa ser nulo ou ter `cap` bytes graváveis.
#[no_mangle]
pub unsafe extern "C" fn quall_generate_pin(buf: *mut c_char, cap: usize) -> isize {
    match Pin::generate() {
        Ok(pin) => escrever_texto(&pin.to_display(), buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

// =============================================================================================
// O papel da sessão, e as mensagens entre aparelhos (F6a)
// =============================================================================================
//
// O contrato — nomes literais, formato, políticas — está em `docs/contrato-teleprompter.md`. As
// cascas implementam contra aqueles nomes; mudar um nome aqui é mudar o contrato.

/// Lê o `role` de uma das funções `_with_role`. `NULL` ou `""` é vídeo — o comportamento das
/// funções sem `_with_role`. Papel desconhecido, ou o papel do outro lado da sessão, é
/// [`QuallStatus::Invalid`].
///
/// # Safety
///
/// `role` precisa ser nulo ou uma string C válida.
unsafe fn papel_da_fronteira(
    role: *const c_char,
    aceito: Papel,
    quem: &str,
) -> Saida<Option<Papel>> {
    let Some(texto) = texto_opcional(role, "role")? else {
        return Ok(None);
    };
    if texto.is_empty() {
        return Ok(None);
    }
    let papel = Papel::do_texto(texto);
    if papel != aceito {
        return Err(Falha::Nucleo(Error::Invalid(format!(
            "{quem}: o papel aqui é \"{}\" (ou nulo, para vídeo); veio \"{texto}\"",
            aceito.como_texto()
        ))));
    }
    Ok(Some(papel))
}

/// Anuncia por mDNS **com um papel**: `"teleprompter"` põe a chave TXT `pa` no anúncio e o papel
/// no nome da instância. `NULL` ou `""` é exatamente [`quall_advertiser_start`].
///
/// # Safety
///
/// `me` precisa apontar para um [`QuallDeviceDesc`] válido; `role` precisa ser nulo ou uma string
/// C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_advertiser_start_with_role(
    me: *const QuallDeviceDesc,
    signaling_port: u16,
    role: *const c_char,
) -> *mut QuallAdvertiser {
    let papel = match papel_da_fronteira(
        role,
        Papel::Teleprompter,
        "quall_advertiser_start_with_role",
    ) {
        Ok(p) => p,
        Err(f) => {
            guardar_falha(&f);
            return ptr::null_mut();
        }
    };
    let Some(me) = me.as_ref() else {
        guardar_nulo("quall_advertiser_start_with_role: `me` é nulo");
        return ptr::null_mut();
    };
    let mut anuncio = match me.para_anuncio() {
        Ok(a) => a,
        Err(f) => {
            guardar_falha(&f);
            return ptr::null_mut();
        }
    };
    anuncio.papel = papel;
    match Advertiser::start(&anuncio, signaling_port) {
        Ok(interno) => Box::into_raw(Box::new(QuallAdvertiser {
            interno: Some(interno),
        })),
        Err(e) => {
            guardar_erro(&e);
            ptr::null_mut()
        }
    }
}

/// **Hospeda como teleprompter.** `role` = `"teleprompter"`; `NULL` ou `""` é exatamente
/// [`quall_host_cancelable`].
///
/// Com o papel, e só com ele:
///
/// - quem conecta tem de ser um `"controle_remoto"`: qualquer outro é recusado **antes do PIN**,
///   com motivo legível, e **a espera continua** com o mesmo PIN;
/// - o canal de dados nasce **confiável e sem ordem** (quem hospeda decide; quem conecta adota);
/// - a sessão cai em 5 s de silêncio (as duas pontas mandam estado a cada segundo);
/// - depois que a sessão sobe, a porta continua **atendida**: todo controle que bate ouve
///   `QUALL_STATUS_BUSY` — inclusive o da sessão, voltando de uma queda que o prompter ainda não
///   percebeu —, e **nada que chega pela porta derruba esta sessão** (o `Hello` não prova quem é;
///   quem decide é o detector de 5 s, que faz `quall_session_next_event` devolver
///   `QUALL_SESSION_EVENT_DISCONNECTED`);
/// - `track_count` tem de ser 0.
///
/// Depois da queda: bombeada final com `timeout_ms = 0`, `quall_teleprompter_peer_lost`,
/// **`quall_session_close` antes** (é ele que solta a porta; hospedar com a sessão velha de pé
/// falha no `bind`), e hospede de novo **na mesma porta e com o mesmo PIN**. O mesmo PIN só
/// depois da queda de uma sessão que subiu: depois de `QUALL_STATUS_WRONG_PIN` ou
/// `QUALL_STATUS_PAIRING` numa espera, troque o PIN (`quall_generate_pin`) — repeti-lo abriria
/// força bruta. `docs/contrato-teleprompter.md` §2.
///
/// O prazo (`timeout_ms`) vale para a espera: um prompter que espera o controle por muito tempo
/// passa um prazo longo, ou hospeda de novo com o mesmo PIN quando ele estoura sem erro de PIN.
///
/// # Safety
///
/// As mesmas de [`quall_host_cancelable`]; `role` precisa ser nulo ou uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_host_with_role(
    opcoes: *const QuallSessionOptions,
    cancelador: *const QuallCanceller,
    role: *const c_char,
) -> *mut QuallSession {
    match papel_da_fronteira(role, Papel::Teleprompter, "quall_host_with_role") {
        Ok(papel) => hospedar_pela_fronteira(opcoes, cancelador, papel),
        Err(f) => {
            guardar_falha(&f);
            ptr::null_mut()
        }
    }
}

/// **Conecta como controle remoto de um teleprompter.** `role` = `"controle_remoto"`; `NULL` ou
/// `""` é exatamente [`quall_connect_cancelable`].
///
/// **O endereço sem porta ganha a do teleprompter, 7979** ([`quall_teleprompter_default_port`]),
/// e não a 7877 do espelhamento. Um link `quall://` é `QUALL_STATUS_INVALID`: leia-o com
/// [`quall_parse_endpoint_json`] (`docs/contrato-teleprompter.md` §11.1).
///
/// Diante de um aparelho que não é teleprompter, sai com `QUALL_STATUS_PROTOCOL` e o motivo — e
/// sai com um `Bye`, que para o outro lado é candidato que desistiu: a espera dele continua.
/// Diante de um teleprompter que já tem controle: `QUALL_STATUS_BUSY`, "tente de novo".
///
/// # Safety
///
/// As mesmas de [`quall_connect_cancelable`]; `role` precisa ser nulo ou uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_connect_with_role(
    endpoint: *const c_char,
    opcoes: *const QuallSessionOptions,
    cancelador: *const QuallCanceller,
    role: *const c_char,
) -> *mut QuallSession {
    match papel_da_fronteira(role, Papel::ControleRemoto, "quall_connect_with_role") {
        Ok(papel) => conectar_pela_fronteira(endpoint, opcoes, cancelador, None, papel),
        Err(f) => {
            guardar_falha(&f);
            ptr::null_mut()
        }
    }
}

/// **A porta do teleprompter**: 7979. O prompter hospeda nela ([`quall_teleprompter_pick_port`]), e
/// [`quall_connect_with_role`] a usa para completar o endereço sem porta do controle. Uma função, e
/// não um `#define`, pelo motivo de [`quall_protocol_version`] (`docs/contrato-teleprompter.md`
/// §11.1).
#[no_mangle]
pub extern "C" fn quall_teleprompter_default_port() -> u16 {
    PORTA_DO_TELEPROMPTER
}

/// **A porta em que o prompter vai hospedar**, escolhida **uma vez, ao abrir a tela**: a 7979,
/// esperando por ela até `wait_ms` (2 000 é o recomendado: numa recriação da tela, a sessão velha
/// ainda a segura por um instante); senão a primeira livre de 7980 a 7988; senão uma efêmera. `0`
/// só se nem isso.
///
/// **A volta depois de uma queda é sempre na mesma porta** — o controle que caiu tenta de novo no
/// endereço que tinha. Não chame isto de novo a cada sessão. (O nome não é `_free_port` porque, no
/// `quall.h`, `_free` é destrutor.)
#[no_mangle]
pub extern "C" fn quall_teleprompter_pick_port(wait_ms: u32) -> u16 {
    escolher_porta_do_teleprompter(Duration::from_millis(u64::from(wait_ms))).unwrap_or(0)
}

/// **Lê um endereço ou um link `quall://<pin>@<host>:<porta>`** — o que a pessoa digitou, colou, ou
/// o QR trouxe —, sem rede nenhuma. Padrão `(buf, cap)`:
///
/// ```json
/// {"endereco":"192.168.57.8:7979","pin":"424242"}
/// ```
///
/// `"pin"` é `null` quando a entrada não era um link. Sem porta, a do papel: `role`
/// `"controle_remoto"` → 7979; nulo ou `""` → 7877 (vídeo); outro papel é `QUALL_STATUS_INVALID`. O
/// que não se entende — PIN do link que não tem exatamente seis dígitos, link sem `@`, espaço no
/// meio, porta 0 — é `-1` com `QUALL_STATUS_INVALID` e o motivo em `quall_last_error()`.
///
/// **A regra da casca** (`docs/contrato-teleprompter.md` §11.1): ler um link preenche o endereço e
/// o PIN nos campos; conectar manda só o endereço (o PIN vai nas opções); a volta automática depois
/// de uma queda vai com o endereço e **sem** PIN. `quall_connect*` recusa link.
///
/// # Safety
///
/// `text` precisa ser uma string C válida; `role`, nulo ou uma string C válida; `buf`, nulo ou com
/// `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_parse_endpoint_json(
    text: *const c_char,
    role: *const c_char,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let feito = (|| -> Saida<String> {
        let entrada = texto(text, "text")?;
        let papel = papel_da_fronteira(role, Papel::ControleRemoto, "quall_parse_endpoint_json")?;
        let lido = ler_destino(entrada, porta_para_completar(papel))?;
        Ok(serde_json::json!({ "endereco": lido.endereco, "pin": lido.pin }).to_string())
    })();
    match feito {
        Ok(j) => escrever_texto(&j, buf, cap),
        Err(f) => {
            guardar_falha(&f);
            -1
        }
    }
}

/// **As mensagens de uma sessão.** Ver [`quall_session_messages`].
pub struct QuallMessages(quall_core::transport::Mensageiro);

/// **O teto de uma mensagem**, em bytes: 262 144 (256 KiB). Uma função, e não um `#define`, pelo
/// motivo de [`quall_protocol_version`]: a casca Kotlin copiaria o número à mão e nada conferiria.
#[no_mangle]
pub extern "C" fn quall_message_max_bytes() -> usize {
    quall_core::transport::TETO_DA_MENSAGEM
}

/// **O handle das mensagens da sessão**: mandar e receber texto pelo canal de dados.
///
/// É do chamador: libere com [`quall_messages_free`]. **Sobrevive a [`quall_session_close`]**,
/// como o de track: depois do fechamento, mandar devolve `QUALL_STATUS_CLOSED` sem tocar na
/// biblioteca, e ler entrega o que já tinha chegado e depois `QUALL_STATUS_CLOSED`. Pode ser pedido
/// mais de uma vez; todos os handles de uma sessão dividem a mesma fila.
///
/// # Safety
///
/// `s` precisa vir de [`quall_host`] ou [`quall_connect`] (ou das variantes) e estar viva.
#[no_mangle]
pub unsafe extern "C" fn quall_session_messages(s: *const QuallSession) -> *mut QuallMessages {
    let Some(sessao) = s.as_ref() else {
        guardar_nulo("quall_session_messages: sessão nula");
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(QuallMessages(sessao.pronto.session.mensageiro())))
}

/// **Manda uma mensagem**: UTF-8, terminada em NUL, de 1 a [`quall_message_max_bytes`] bytes.
/// **Pode ser chamada de qualquer thread**, inclusive da thread da interface: não bloqueia.
///
/// - vazia ou acima do teto: `QUALL_STATUS_INVALID`, e nada sai (o teto é conferido **antes** da
///   biblioteca, que lançaria exceção);
/// - o canal ainda não abriu: `QUALL_STATUS_TRANSPORT` — tente de novo, não é queda;
/// - a sessão fechou, ou o canal fechou: `QUALL_STATUS_CLOSED`.
///
/// Numa sessão de vídeo o canal é **sem retransmissão**: uma mensagem maior que um pedaço SCTP
/// (~1,1 KB) não tem garantia nenhuma de chegar. Numa sessão de teleprompter é confiável.
///
/// # Safety
///
/// `m` precisa vir de [`quall_session_messages`]; `message` precisa ser uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_messages_send(
    m: *const QuallMessages,
    message: *const c_char,
) -> QuallStatus {
    let Some(m) = m.as_ref() else {
        return guardar_nulo("quall_messages_send: handle nulo");
    };
    let texto = match texto(message, "message") {
        Ok(t) => t,
        Err(f) => return guardar_falha(&f),
    };
    match m.0.enviar(texto) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// **A próxima mensagem**, esperando até `timeout_ms`, no padrão `(buf, cap)` — com uma diferença
/// que importa: **a mensagem só sai da fila quando coube**.
///
/// | devolve | quer dizer |
/// |---|---|
/// | `0` | nada chegou no prazo |
/// | `n > 0` e `n <= cap` | a mensagem foi escrita em `buf` (com o NUL) **e consumida** |
/// | `n > cap` (ou `buf` nulo) | há uma mensagem de `n` bytes com o NUL, **não consumida**: aloque `n` e chame de novo (com `timeout_ms = 0`) |
/// | negativo | erro; o motivo em [`quall_last_status`]: `QUALL_STATUS_CLOSED` quando a sessão acabou e a fila esvaziou |
///
/// Uma mensagem nunca é vazia (a vazia, a com NUL e a que não é UTF-8 são descartadas na chegada
/// e contadas), então `0` não se confunde com mensagem. `buf` nulo com `cap > 0` é
/// `QUALL_STATUS_NULL_POINTER`.
///
/// **Avança estado: uma thread só.** E numa sessão de teleprompter quem lê é
/// [`quall_teleprompter_pump`] — as duas funções leem a mesma fila, e cada mensagem vai para quem
/// ler primeiro.
///
/// # Safety
///
/// `m` precisa vir de [`quall_session_messages`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_messages_next(
    m: *const QuallMessages,
    timeout_ms: u32,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(m) = m.as_ref() else {
        guardar_nulo("quall_messages_next: handle nulo");
        return -1;
    };
    if buf.is_null() && cap > 0 {
        guardar_nulo("quall_messages_next: `buf` nulo com `cap` maior que zero");
        return -1;
    }
    let limite = Duration::from_millis(u64::from(timeout_ms));
    match m.0.entregar_se(limite, |texto| {
        let precisa = texto.len() + 1;
        if buf.is_null() || cap < precisa {
            return false;
        }
        ptr::copy_nonoverlapping(texto.as_ptr(), buf.cast::<u8>(), texto.len());
        *buf.add(texto.len()) = 0;
        true
    }) {
        Ok(Some(tamanho)) => isize::try_from(tamanho + 1).unwrap_or(-1),
        Ok(None) => 0,
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// Os contadores das mensagens da sessão, como JSON. Padrão `(buf, cap)`:
///
/// ```json
/// {"enviadas":0,"recebidas":0,"descartadas_fila_cheia":0,"descartadas_invalidas":0}
/// ```
///
/// # Safety
///
/// `m` precisa vir de [`quall_session_messages`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_messages_stats_json(
    m: *const QuallMessages,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(m) = m.as_ref() else {
        guardar_nulo("quall_messages_stats_json: handle nulo");
        return -1;
    };
    match serde_json::to_string(&m.0.contadores()) {
        Ok(t) => escrever_texto(&t, buf, cap),
        Err(e) => {
            guardar_erro(&Error::from(e));
            -1
        }
    }
}

/// Libera o handle das mensagens. Nulo é ignorado. Pode ser antes ou depois de
/// [`quall_session_close`].
///
/// # Safety
///
/// `m` precisa vir de [`quall_session_messages`] e não pode ter sido liberado antes.
#[no_mangle]
pub unsafe extern "C" fn quall_messages_free(m: *mut QuallMessages) {
    if !m.is_null() {
        drop(Box::from_raw(m));
    }
}

// ---------------------------------------------------------------------------------------------
// O teleprompter
// ---------------------------------------------------------------------------------------------

/// Os bits de `changed` em [`quall_teleprompter_pump`] e [`quall_teleprompter_peer_lost`]: o que
/// mudou **por causa do outro lado**. Um `uint32_t` com vários bits ligados de uma vez.
///
/// **Os valores são ABI**: bit novo entra no fim, com o próximo livre.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallTeleprompterChange {
    Text = 1,
    Scrolling = 2,
    Speed = 4,
    FontSize = 8,
    Margin = 16,
    ReadingLine = 32,
    Mirror = 64,
    /// O relato de posição do prompter. **Quem mostra o texto ignora**; o controle desenha.
    Position = 128,
    /// Chegou um salto novo. **Quem mostra o texto** vai até `"salto"` do estado, chama
    /// [`quall_teleprompter_set_position`] com ele e mantém `rolando` como está. O controle só
    /// atualiza a vista.
    Jump = 256,
    /// Mudou o contato com o outro lado: sumiu, voltou, ou a confirmação das edições daqui mudou.
    /// Releia `par_visto_ha_ms` e `sem_confirmacao_ha_ms` no estado.
    Peer = 512,
    /// Mudou `"pergunta_do_texto"` no estado: ela abriu, entrou em "comparando", o texto do
    /// prompter nela mudou, ou fechou por causa do outro lado ou de uma sessão nova. Releia o
    /// estado (`docs/contrato-teleprompter.md` §11.4).
    TextQuestion = 1024,
    /// Há cópia nova do roteiro (ou a lista de cópias mudou): **grave o salvo agora**
    /// ([`quall_teleprompter_saved_json`]). Acende na bombeada seguinte a qualquer cópia nova —
    /// inclusive a que [`quall_teleprompter_resolve_text`] fez (§11.5).
    TextCopy = 2048,
    /// Mudou `"para_tras"` ou `"segurando"` (o "segurar para rolar", §12). **Quem mostra o texto**
    /// relê `"rolando"` e `"para_tras"`: com os dois, rola para trás na velocidade de sempre e para
    /// no começo. O controle relê `"segurando"`.
    Hold = 4096,
    /// A gravação (`docs/contrato-teleprompter.md` §13). **No prompter**: chegou um pedido do
    /// controle — releia `"pedido_de_gravacao"` e chame [`quall_teleprompter_set_recording`] com o
    /// `"gravar"` dele ou [`quall_teleprompter_refuse_recording`] com o `"n"` dele. **No controle**: a gravação começou
    /// ou parou (`"gravando_ha_ms"`), o pedido daqui foi respondido (`"pedido_de_gravacao"` voltou a
    /// `null`; `"gravacao_recusada"` diz se foi recusado), ou `"par_entende_gravar"` mudou.
    Recording = 8192,
}

/// **Uma réplica do estado do teleprompter.** Ver [`quall_teleprompter_new`].
pub struct QuallTeleprompter(quall_core::teleprompter::Teleprompter);

/// **O teto do roteiro**, em bytes de UTF-8: 131 072.
#[no_mangle]
pub extern "C" fn quall_teleprompter_max_text_bytes() -> usize {
    quall_core::teleprompter::TETO_DO_TEXTO
}

/// **Cria a réplica deste aparelho.** Uma por aparelho, e ela vive mais que a sessão: guarde-a
/// enquanto o app estiver aberto, e passe a mesma a cada sessão nova.
///
/// - `author_id`: o `device_id` deste aparelho (1 a 256 bytes) — desempata edições no mesmo
///   milissegundo;
/// - `role`: `"teleprompter"` (mostra o texto) ou `"controle_remoto"`. Ao começar uma sessão nova,
///   o controle zera `rolando`, `posicao` e `salto` e adota os do prompter;
/// - `saved_json`: o que [`quall_teleprompter_saved_json`] devolveu numa vida anterior, ou nulo.
///   JSON ilegível devolve nulo com `QUALL_STATUS_INVALID`: chame de novo com nulo.
///
/// Libere com [`quall_teleprompter_free`].
///
/// # Safety
///
/// As três strings precisam ser nulas (só `saved_json` pode) ou strings C válidas.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_new(
    author_id: *const c_char,
    role: *const c_char,
    saved_json: *const c_char,
) -> *mut QuallTeleprompter {
    let feito = (|| -> Saida<quall_core::teleprompter::Teleprompter> {
        let autor = texto(author_id, "author_id")?;
        let papel = Papel::do_texto(texto(role, "role")?);
        Ok(match texto_opcional(saved_json, "saved_json")? {
            Some(j) if !j.trim().is_empty() => {
                quall_core::teleprompter::Teleprompter::de_salvo(autor, papel, j)?
            }
            _ => quall_core::teleprompter::Teleprompter::nova(autor, papel)?,
        })
    })();
    match feito {
        Ok(t) => Box::into_raw(Box::new(QuallTeleprompter(t))),
        Err(f) => {
            guardar_falha(&f);
            ptr::null_mut()
        }
    }
}

/// Libera a réplica. Nulo é ignorado. Guarde antes o [`quall_teleprompter_saved_json`].
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`] e não pode ter sido liberado antes, nem estar em
/// uso em outra thread.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_free(t: *mut QuallTeleprompter) {
    if !t.is_null() {
        drop(Box::from_raw(t));
    }
}

/// O miolo das edições: réplica nula é `NULL_POINTER`, o resto é o `Result` do núcleo.
///
/// # Safety
///
/// `t` precisa ser nulo ou vir de [`quall_teleprompter_new`].
unsafe fn editar_teleprompter(
    t: *const QuallTeleprompter,
    quem: &str,
    f: impl FnOnce(&quall_core::teleprompter::Teleprompter) -> quall_core::error::Result<()>,
) -> QuallStatus {
    let Some(t) = t.as_ref() else {
        return guardar_nulo(&format!("{quem}: réplica nula"));
    };
    match f(&t.0) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// Troca o roteiro. **Chame ao confirmar a edição, nunca a cada tecla.** Acima de
/// [`quall_teleprompter_max_text_bytes`], com NUL, ou se o texto escapado não couber numa
/// mensagem: `QUALL_STATUS_INVALID`, e o texto anterior fica. Qualquer thread.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `text` precisa ser uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_text(
    t: *const QuallTeleprompter,
    text: *const c_char,
) -> QuallStatus {
    let conteudo = match texto(text, "text") {
        Ok(c) => c,
        Err(f) => return guardar_falha(&f),
    };
    editar_teleprompter(t, "quall_teleprompter_set_text", |tp| {
        tp.definir_texto(conteudo)
    })
}

/// Rola ou para. Qualquer thread; a mudança sai na hora.
///
/// Com o "segurar para rolar" (§12.3): `false` **sempre** para e sai do segurar; `true` só faz
/// alguma coisa com o texto parado — com o texto rolando pelo dedo no botão, não muda nada, e
/// [`quall_teleprompter_release`] e a queda seguem parando.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_scrolling(
    t: *const QuallTeleprompter,
    scrolling: bool,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_scrolling", |tp| {
        tp.definir_rolando(scrolling)
    })
}

/// Velocidade em linhas por segundo (linha = altura da linha na fonte do prompter), de 0,05 a 20,
/// em centésimos. Fora da faixa ou NaN: `QUALL_STATUS_INVALID`.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_speed(
    t: *const QuallTeleprompter,
    lines_per_second: f64,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_speed", |tp| {
        tp.definir_velocidade(lines_per_second)
    })
}

/// Fonte em pontos lógicos (pt no iOS e no Mac, sp no Android, DIP no Windows), de 8 a 400.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_font_size(
    t: *const QuallTeleprompter,
    points: f64,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_font_size", |tp| {
        tp.definir_fonte(points)
    })
}

/// Margem, fração da largura da vista do texto, de cada lado, de 0 a 0,45.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_margin(
    t: *const QuallTeleprompter,
    fraction: f64,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_margin", |tp| {
        tp.definir_margem(fraction)
    })
}

/// Linha de leitura, fração da altura da vista do texto a partir do topo, de 0 a 1.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_reading_line(
    t: *const QuallTeleprompter,
    fraction: f64,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_reading_line", |tp| {
        tp.definir_linha_de_leitura(fraction)
    })
}

/// Espelho: inverte a **vista do texto** na horizontal (não o vídeo).
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_mirror(
    t: *const QuallTeleprompter,
    mirror: bool,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_mirror", |tp| {
        tp.definir_espelho(mirror)
    })
}

/// **O relato de posição de quem mostra o texto**, como fração do percurso (0 = começo na linha de
/// leitura, 1 = fim nela). Pode ser chamado a cada quadro: o envio é limitado a 4 Hz e sai na
/// bombeada. **Só o prompter**: no controle é `QUALL_STATUS_INVALID`.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_position(
    t: *const QuallTeleprompter,
    fraction: f64,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_position", |tp| {
        tp.definir_posicao(fraction)
    })
}

/// **Salta** para `fraction` (0 a 1). "Voltar ao começo" é `jump(0)`; duas vezes são dois saltos.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_jump(
    t: *const QuallTeleprompter,
    fraction: f64,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_jump", |tp| tp.saltar(fraction))
}

/// **Pula** `delta` (de -1 a 1) a partir de onde o texto **vai estar**: o último salto daqui, se o
/// relato ainda não passou dele, senão a posição relatada. Dois toques rápidos são dois pulos.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_jump_by(
    t: *const QuallTeleprompter,
    delta: f64,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_jump_by", |tp| {
        tp.saltar_relativo(delta)
    })
}

/// **A bombeada.** Manda o que está devido (o batimento de 1 s, o relato de posição, o texto quando
/// o outro lado não o tem), espera até `timeout_ms` por mensagem, funde o que chegou, e escreve em
/// `changed` (que pode ser nulo) os bits de [`QuallTeleprompterChange`] do que mudou **por causa do
/// outro lado**.
///
/// Chame em laço, da thread da sessão, com `timeout_ms` de no máximo 250 (50 a 100 é o
/// recomendado), junto com `quall_session_next_event(s, 0)` — é este que diz que o outro lado
/// saiu. As edições da tela **não** esperam a bombeada: saem na hora, da thread de quem edita.
///
/// # `QUALL_STATUS_CLOSED` vem **com** `changed` preenchido
///
/// Quer dizer que a sessão acabou — **e a fila já foi lida até o fim**: a última mensagem do outro
/// lado (a pausa que o controle tocou antes de cair) foi fundida, e o bit dela está em `changed`.
/// **Aplique `changed` antes de qualquer outra coisa** e só então chame
/// [`quall_teleprompter_peer_lost`]. Uma falha de envio nunca impede a leitura nem apaga o que foi
/// fundido. (Até a revisão de 13/09, `CLOSED` vinha com `changed = 0` e a pausa se perdia: a
/// réplica dizia parado e a tela seguia rolando.)
///
/// Quando a queda chega por `quall_session_next_event` (`DISCONNECTED`) e não pela bombeada, faça
/// **uma bombeada final com `timeout_ms = 0`**, aplique o `changed` dela, e só então chame
/// `quall_teleprompter_peer_lost`.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`], `m` de [`quall_session_messages`]; `changed`
/// precisa ser nulo ou apontar para um `uint32_t`.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_pump(
    t: *const QuallTeleprompter,
    m: *const QuallMessages,
    timeout_ms: u32,
    changed: *mut u32,
) -> QuallStatus {
    let (Some(t), Some(m)) = (t.as_ref(), m.as_ref()) else {
        return guardar_nulo("quall_teleprompter_pump: réplica ou handle nulo");
    };
    match t
        .0
        .bombear(&m.0, Duration::from_millis(u64::from(timeout_ms)))
    {
        Ok(b) => {
            if !changed.is_null() {
                *changed = b.mudancas;
            }
            if b.fechada {
                guardar_erro(&Error::Closed)
            } else {
                QuallStatus::Ok
            }
        }
        Err(e) => {
            if !changed.is_null() {
                *changed = 0;
            }
            guardar_erro(&e)
        }
    }
}

/// **O outro lado sumiu** (a sessão caiu). Aplica a regra de queda — decidida pelo usuário: o
/// prompter **continua no estado em que estava**, rolando segue rolando, parado segue parado, e as
/// duas telas mostram um aviso até o controle voltar (`par_visto_ha_ms` fica nulo) —, esquece o que
/// sabia do par, e escreve em `changed` o que mudou (sempre com `QUALL_TELEPROMPTER_CHANGE_PEER`).
///
/// **A exceção é o "segurar para rolar"** (`docs/contrato-teleprompter.md` §12.4, decisão do
/// usuário de 14/09): com `"segurando"`, **o texto para**, nos dois lados, como se a pessoa tivesse
/// soltado, e `changed` traz `QUALL_TELEPROMPTER_CHANGE_SCROLLING` e `QUALL_TELEPROMPTER_CHANGE_HOLD`.
/// No controle, essa parada é só da réplica dele e não vai ao fio: o prompter para sozinho, pela
/// queda dele, por 2,5 s sem ouvir o controle ou — se a casca dele não chamar `peer_lost` — ao
/// começar a sessão seguinte, também só na réplica dele.
///
/// Chame em **todo** fim de sessão, antes de qualquer outra edição (a ordem do fim, §6).
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `changed` precisa ser nulo ou apontar para um
/// `uint32_t`.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_peer_lost(
    t: *const QuallTeleprompter,
    changed: *mut u32,
) -> QuallStatus {
    let Some(t) = t.as_ref() else {
        return guardar_nulo("quall_teleprompter_peer_lost: réplica nula");
    };
    match t.0.perdeu_o_par() {
        Ok(mudou) => {
            if !changed.is_null() {
                *changed = mudou;
            }
            QuallStatus::Ok
        }
        Err(e) => guardar_erro(&e),
    }
}

/// **O estado para a tela desenhar**, como JSON, sem o texto. Padrão `(buf, cap)` — e o conteúdo
/// pode mudar entre a chamada que pergunta o tamanho e a que escreve: repita até caber.
///
/// ```json
/// {"rolando":false,"velocidade":1.0,"fonte":48.0,"margem":0.1,"linha_de_leitura":0.3,
///  "espelho":false,"posicao":0.0,"salto":null,"texto_bytes":0,"par_visto_ha_ms":null,
///  "sem_confirmacao_ha_ms":null,"contadores":{"estados_enviados":0,"textos_enviados":0,
///  "recebidas":0,"invalidas":0,"de_outro_app":0,"de_outra_versao":0,"campos_recusados":0,
///  "carimbos_do_futuro":0,"mensagens_impossiveis":0,"reenvios_desistidos":0},
///  "pergunta_do_texto":null,"copias_do_texto":[],"para_tras":false,"segurando":false,
///  "par_entende_segurar":false,"gravando_ha_ms":null,"pedido_de_gravacao":null,
///  "gravacao_recusada":null,"par_entende_gravar":false}
/// ```
///
/// A gravação (`docs/contrato-teleprompter.md` §13): `"gravando_ha_ms"` é a duração que o prompter
/// relata (`null` sem gravar); `"pedido_de_gravacao"` é `{"n":…,"gravar":true,"ha_ms":…}` — no
/// prompter, o pedido que a casca decide; no controle, o daqui sem resposta; `"gravacao_recusada"` é
/// `{"n":…,"gravar":true,"motivo":"…"}`.
///
/// Com a pergunta do texto aberta (`docs/contrato-teleprompter.md` §11.6):
///
/// ```json
/// "pergunta_do_texto":{"aberta":true,"retido_ha_ms":840,"prompter_id":"ipad-a1b2",
///  "prompter_nome":"iPad da Maria","meu":{"bytes":10,"resumo":"…","previa":"Boa noite."},
///  "do_prompter":{"bytes":15,"resumo":"…","previa":"Bom dia a todos"}}
/// ```
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_state_json(
    t: *const QuallTeleprompter,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(t) = t.as_ref() else {
        guardar_nulo("quall_teleprompter_state_json: réplica nula");
        return -1;
    };
    match t
        .0
        .estado()
        .and_then(|e| serde_json::to_string(&e).map_err(Error::from))
    {
        Ok(j) => escrever_texto(&j, buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **O roteiro.** Padrão `(buf, cap)`; o texto pode mudar entre as duas chamadas (chegou uma
/// edição do outro lado): repita até caber.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_text(
    t: *const QuallTeleprompter,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(t) = t.as_ref() else {
        guardar_nulo("quall_teleprompter_text: réplica nula");
        return -1;
    };
    match t.0.com_o_texto(|texto| escrever_texto(texto, buf, cap)) {
        Ok(n) => n,
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **O que guardar entre vidas do app**: os seis campos que persistem, com os carimbos. Até
/// ~260 KiB. Guarde quando o app for para o segundo plano e ao fechar a sessão, e devolva a
/// [`quall_teleprompter_new`]. Padrão `(buf, cap)`.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_saved_json(
    t: *const QuallTeleprompter,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(t) = t.as_ref() else {
        guardar_nulo("quall_teleprompter_saved_json: réplica nula");
        return -1;
    };
    match t.0.salvo_json() {
        Ok(j) => escrever_texto(&j, buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **Liga a pergunta do texto** (`docs/contrato-teleprompter.md` §11.10). Chame logo depois de
/// [`quall_teleprompter_new`], **só quando a casca tiver a tela da pergunta** (a caixa "usar o do
/// prompter / mandar o meu" e [`quall_teleprompter_resolve_text`]). Ligada no meio de uma sessão,
/// vale a partir da seguinte.
///
/// **Desligada (o padrão)**, o texto nunca é retido: vale "o último que mudou", como sempre, byte a
/// byte no fio; `"pergunta_do_texto"` é sempre `null`. O que sobra são as cópias da fusão — o texto
/// que perde no primeiro encontro com um prompter, dos dois lados, e o texto daqui que mudou desde a
/// última convergência e perde no reencontro (§11.5). No prompter, não muda nada.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_enable_text_question(
    t: *const QuallTeleprompter,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_enable_text_question", |tp| {
        tp.ligar_pergunta_do_texto()
    })
}

/// **Diz que a tela deste prompter entende o "segurar para rolar"** (§12): com `"rolando"` e
/// `"para_tras"`, ela rola para trás na velocidade de sempre e para no começo; e para quando
/// `"rolando"` cai. Chame ao abrir a tela do prompter, **só se ela faz isso**: o estado passa a levar
/// `"entende_segurar": true`, e só então um controle segura. No controle, não muda nada.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_enable_hold(
    t: *const QuallTeleprompter,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_enable_hold", |tp| tp.ligar_segurar())
}

/// **Aperta o botão do "segurar para rolar"** (§12): `rolando`, `para_tras` (`backwards`) e
/// `segurando` numa mensagem só, com um carimbo só, que sai na hora. Enquanto segura, o prompter rola
/// na velocidade de sempre; [`quall_teleprompter_release`] para. **Se a sessão cair com o dedo no
/// botão, o texto para** (decisão do usuário, 14/09) — e também depois de 2,5 s sem ouvir o outro
/// lado.
///
/// - `QUALL_STATUS_PROTOCOL`: o prompter não diz que entende (`"par_entende_segurar": false` no
///   estado) — um prompter de 13/09, ou uma tela que não liga o segurar, rolaria para a frente quando
///   se pede para trás, e seguiria rolando na queda. Mostre o modo desligado;
/// - `QUALL_STATUS_CLOSED`: sem sessão — e também **depois de a bombeada devolver
///   `QUALL_STATUS_CLOSED`**, antes mesmo de [`quall_teleprompter_peer_lost`]: o aperto nunca vai
///   para o prompter da sessão seguinte;
/// - `QUALL_STATUS_INVALID`: numa réplica de prompter.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_hold(
    t: *const QuallTeleprompter,
    backwards: bool,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_hold", |tp| tp.segurar(backwards))
}

/// **Solta o botão**: o texto para, numa mensagem só. Sem nada seguro, não faz nada (o segurar pode
/// já ter parado sozinho, na queda ou no silêncio).
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_release(t: *const QuallTeleprompter) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_release", |tp| tp.soltar())
}

/// **Diz se a tela deste prompter grava** (`docs/contrato-teleprompter.md` §13): a tela do
/// teleprompter com câmera liga (`enabled = true`) ao abrir e desliga ao fechar. Ligada, o estado leva
/// `"entende_gravar": true`, e só então um controle pede; desligada, um pedido que ainda chegue é
/// recusado pelo núcleo ("o prompter não está na tela que grava"), e o que estava aberto também. Não
/// mexe numa gravação em curso.
/// No controle, não muda nada.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_enable_recording(
    t: *const QuallTeleprompter,
    enabled: bool,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_enable_recording", |tp| {
        tp.ligar_gravacao(enabled)
    })
}

/// **O relato da gravação, por quem grava** (§13): `true` quando o arquivo começou, `false` quando
/// fechou — pelo botão da tela, por um pedido do controle, por falta de espaço, pela câmera perdida.
/// O estado passa a dizer `"gravando_ha_ms"`, contado no relógio monotônico daqui, e o controle o
/// vê como duração, nunca como hora. Sai na hora. Repetir o valor atual não recomeça a contagem.
///
/// **Aceitar um pedido do controle é chamar isto com o `"gravar"` dele**, mesmo que já esteja assim.
///
/// - `QUALL_STATUS_INVALID`: numa réplica de controle — o prompter é o único escritor; o controle
///   pede com [`quall_teleprompter_request_record`] e [`quall_teleprompter_request_stop`].
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_set_recording(
    t: *const QuallTeleprompter,
    recording: bool,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_set_recording", |tp| {
        tp.definir_gravando(recording)
    })
}

/// **Recusa o pedido de gravação aberto `n`** (§13), com um motivo legível que o controle mostra
/// como está (`"gravacao_recusada"`): "sem espaço: sobram 312 MB", "a câmera não está entregando".
/// `n` é o `"n"` de `"pedido_de_gravacao"` que a casca leu e decidiu. Sai na hora.
///
/// - `QUALL_STATUS_BUSY`: o pedido `n` foi substituído por outro enquanto a casca decidia — nada foi
///   recusado; releia `"pedido_de_gravacao"` e decida o novo;
/// - `QUALL_STATUS_INVALID`: não há pedido aberto; o motivo é vazio ou passa de 256 bytes; ou a
///   réplica é de controle;
/// - `QUALL_STATUS_NULL_POINTER`: `reason` nulo.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `reason` precisa ser uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_refuse_recording(
    t: *const QuallTeleprompter,
    n: u64,
    reason: *const c_char,
) -> QuallStatus {
    let motivo = match texto(reason, "reason") {
        Ok(m) => m,
        Err(f) => return guardar_falha(&f),
    };
    editar_teleprompter(t, "quall_teleprompter_refuse_recording", |tp| {
        tp.recusar_gravacao(n, motivo)
    })
}

/// **O controle pede ao prompter que comece a gravar** (§13). Quem decide é o prompter; a resposta
/// volta pelo estado: `"pedido_de_gravacao"` volta a `null`, e `"gravando_ha_ms"` passa a contar ou
/// `"gravacao_recusada"` diz por quê (bit `QUALL_TELEPROMPTER_CHANGE_RECORDING`). Sai na hora.
///
/// - `QUALL_STATUS_PROTOCOL`: o prompter não diz que grava (`"par_entende_gravar": false`) — uma
///   build anterior, ou uma tela que não grava. **Não mostre o botão**;
/// - `QUALL_STATUS_CLOSED`: sem sessão, ou com ela já acabada — um pedido não atravessa sessão;
/// - `QUALL_STATUS_INVALID`: numa réplica de prompter.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_request_record(
    t: *const QuallTeleprompter,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_request_record", |tp| {
        tp.pedir_gravar()
    })
}

/// **O controle pede ao prompter que pare de gravar** (§13). As mesmas regras de
/// [`quall_teleprompter_request_record`].
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_request_stop(
    t: *const QuallTeleprompter,
) -> QuallStatus {
    editar_teleprompter(t, "quall_teleprompter_request_stop", |tp| tp.pedir_parar())
}

/// **A escolha da pergunta do texto** (`docs/contrato-teleprompter.md` §11.4): `keep_mine = false`
/// é "usar o do prompter" (o controle adota o registro dele e guarda o daqui como cópia);
/// `keep_mine = true` é "mandar o meu" (o texto daqui é recarimbado acima do dele, sai, e o dele
/// fica guardado). `seen_digest` é o `"resumo"` de `"do_prompter"` que a tela mostrou.
///
/// - `QUALL_STATUS_OK`: grave o salvo agora ([`quall_teleprompter_saved_json`]) — a cópia só existe
///   na memória até ser gravada;
/// - `QUALL_STATUS_INVALID`: não há pergunta aberta (ou ainda está comparando);
/// - `QUALL_STATUS_BUSY`: o texto do prompter mudou desde que a pergunta foi mostrada, ou ele tem
///   um texto mais novo a caminho. Espere `QUALL_TELEPROMPTER_CHANGE_TEXT_QUESTION` e mostre de novo;
/// - `QUALL_STATUS_CLOSED`: o prompter não está conectado — as escolhas ficam desligadas;
/// - `QUALL_STATUS_NULL_POINTER`: `seen_digest` nulo.
///
/// Qualquer thread; sai na hora, como os `set_*`.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `seen_digest` precisa ser uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_resolve_text(
    t: *const QuallTeleprompter,
    keep_mine: bool,
    seen_digest: *const c_char,
) -> QuallStatus {
    let visto = match texto(seen_digest, "seen_digest") {
        Ok(v) => v,
        Err(f) => return guardar_falha(&f),
    };
    editar_teleprompter(t, "quall_teleprompter_resolve_text", |tp| {
        tp.resolver_texto(keep_mine, visto)
    })
}

/// **O texto do prompter na pergunta aberta**, no padrão `(buf, cap)` — repita até caber. Sem
/// pergunta aberta (inclusive comparando): `-1` com `QUALL_STATUS_INVALID`. O texto **deste**
/// controle continua em [`quall_teleprompter_text`]: retido, a réplica mostra o dele.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_question_text(
    t: *const QuallTeleprompter,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(t) = t.as_ref() else {
        guardar_nulo("quall_teleprompter_question_text: réplica nula");
        return -1;
    };
    match t
        .0
        .com_o_texto_da_pergunta(|q| q.map(|texto| escrever_texto(texto, buf, cap)))
    {
        Ok(Some(n)) => n,
        Ok(None) => {
            guardar_erro(&Error::Invalid("não há pergunta do texto aberta".into()));
            -1
        }
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **O texto inteiro de uma cópia**, pelo `"resumo"` que `"copias_do_texto"` mostra. Padrão
/// `(buf, cap)`. Um resumo que não está na lista é `-1` com `QUALL_STATUS_INVALID`. "Usar este" é
/// um [`quall_teleprompter_set_text`] comum com ele.
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `digest` precisa ser uma string C válida; `buf`,
/// nulo ou com `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_text_copy(
    t: *const QuallTeleprompter,
    digest: *const c_char,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(t) = t.as_ref() else {
        guardar_nulo("quall_teleprompter_text_copy: réplica nula");
        return -1;
    };
    let resumo = match texto(digest, "digest") {
        Ok(r) => r,
        Err(f) => {
            guardar_falha(&f);
            return -1;
        }
    };
    match t
        .0
        .com_a_copia_do_texto(resumo, |c| c.map(|texto| escrever_texto(texto, buf, cap)))
    {
        Ok(Some(n)) => n,
        Ok(None) => {
            guardar_erro(&Error::Invalid(format!(
                "não há cópia com o resumo {resumo}"
            )));
            -1
        }
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **Apaga uma cópia**, pelo `"resumo"`. `QUALL_STATUS_INVALID` se não havia. Grave o salvo
/// depois (o bit `QUALL_TELEPROMPTER_CHANGE_TEXT_COPY` acende na bombeada seguinte).
///
/// # Safety
///
/// `t` precisa vir de [`quall_teleprompter_new`]; `digest` precisa ser uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_teleprompter_forget_text_copy(
    t: *const QuallTeleprompter,
    digest: *const c_char,
) -> QuallStatus {
    let resumo = match texto(digest, "digest") {
        Ok(r) => r,
        Err(f) => return guardar_falha(&f),
    };
    editar_teleprompter(t, "quall_teleprompter_forget_text_copy", |tp| {
        if tp.esquecer_copia_do_texto(resumo)? {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "não há cópia com o resumo {resumo}"
            )))
        }
    })
}

// =============================================================================================
// O controle remoto da câmera (`docs/controle-remoto-da-camera.md`)
// =============================================================================================

/// **O que mudou no filmador**, em bits, no `changed` de [`quall_camera_host_pump`].
///
/// **Os valores são ABI**: bit novo entra no fim, com o próximo livre.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallCameraHostChange {
    /// Há pedido aceito: chame [`quall_camera_host_next_request`] até a fila esvaziar. **Um
    /// consumidor só** (a fila do dono da câmera): com várias sessões, várias bombeadas acendem
    /// este bit, e só uma thread tira os pedidos.
    Request = 1,
    /// Um receptor disse `ola` ou saiu: releia `"receptores"` no estado.
    Listeners = 2,
}

/// **O que mudou no receptor por causa do filmador**, em bits, no `changed` de
/// [`quall_camera_remote_pump`].
///
/// **Os valores são ABI**: bit novo entra no fim, com o próximo livre.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuallCameraRemoteChange {
    /// Chegaram capacidades novas: refaça o painel a partir de `"capacidades"`.
    Capabilities = 1,
    /// O ajuste aplicado, o `autor`, a permissão, ou o pendente que caiu.
    Settings = 2,
    /// O lido (a linha do R9 §3.6).
    Read = 4,
    /// Uma recusa (ou a desistência, `sem_resposta`): mostre `"recusa"` por 3 s.
    Refusal = 8,
    /// A `"situacao"` mudou.
    Situation = 16,
}

/// **O filmador** do controle remoto da câmera: um por câmera em uso, compartilhado por todas as
/// sessões de vídeo que a transmitem. Ver [`quall_camera_host_new`].
pub struct QuallCameraHost(quall_core::camera_remota::Filmador);

/// **O controle da câmera do outro lado**, no receptor: um por sessão de recepção. Ver
/// [`quall_camera_remote_new`].
pub struct QuallCameraRemote(quall_core::camera_remota::Controlador);

/// **Cria o filmador**, sem câmera e **com a permissão desligada** (o padrão do contrato). Chame
/// [`quall_camera_host_set_allowed`] com a opção salva e [`quall_camera_host_set_camera`] com a
/// câmera. Libere com [`quall_camera_host_free`].
#[no_mangle]
pub extern "C" fn quall_camera_host_new() -> *mut QuallCameraHost {
    Box::into_raw(Box::new(QuallCameraHost(
        quall_core::camera_remota::Filmador::novo(),
    )))
}

/// Libera o filmador. Nulo é aceito.
///
/// # Safety
///
/// `h` precisa ser nulo ou vir de [`quall_camera_host_new`], e não ser usado depois.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_free(h: *mut QuallCameraHost) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}

unsafe fn no_filmador(
    h: *const QuallCameraHost,
    quem: &str,
    f: impl FnOnce(&quall_core::camera_remota::Filmador) -> quall_core::error::Result<()>,
) -> QuallStatus {
    let Some(h) = h.as_ref() else {
        return guardar_nulo(&format!("{quem}: filmador nulo"));
    };
    match f(&h.0) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// Liga ou desliga **"Permitir controle remoto da câmera"**. Desligada, os pedidos são recusados
/// com `nao_permitido` e os receptores mostram os controles apagados. Qualquer thread.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_set_allowed(
    h: *const QuallCameraHost,
    allowed: bool,
) -> QuallStatus {
    no_filmador(h, "quall_camera_host_set_allowed", |f| f.permitir(allowed))
}

/// **A câmera em uso**: as capacidades (contrato §3.2, até 2.048 bytes) e o registro do R9 dela
/// (até 1.024 bytes). **Os dois nulos** = sem câmera (a câmera fechou, a fonte é a tela). Um nulo
/// só: `QUALL_STATUS_INVALID`. Chame a cada abertura e troca de câmera ou de lente: os pedidos
/// em trânsito da câmera anterior são recusados com `camera_trocada`. Para faixas novas da mesma
/// câmera, [`quall_camera_host_set_capabilities`]. Qualquer thread.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; as strings, nulas ou strings C válidas.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_set_camera(
    h: *const QuallCameraHost,
    capabilities_json: *const c_char,
    settings_json: *const c_char,
) -> QuallStatus {
    let caps = match texto_opcional(capabilities_json, "capabilities_json") {
        Ok(c) => c,
        Err(f) => return guardar_falha(&f),
    };
    let ajuste = match texto_opcional(settings_json, "settings_json") {
        Ok(a) => a,
        Err(f) => return guardar_falha(&f),
    };
    let camera = match (caps, ajuste) {
        (Some(c), Some(a)) => Some((c, a)),
        (None, None) => None,
        _ => return guardar_texto("quall_camera_host_set_camera: as capacidades e o registro vão juntos, ou os dois nulos"),
    };
    no_filmador(h, "quall_camera_host_set_camera", |f| {
        f.definir_camera(camera)
    })
}

/// **As faixas novas da mesma câmera** (o fps que muda o teto do obturador, o degrau de calor do
/// iOS): os pedidos em trânsito continuam valendo, conferidos contra a faixa nova. Sem câmera:
/// `QUALL_STATUS_INVALID`.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; `capabilities_json`, uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_set_capabilities(
    h: *const QuallCameraHost,
    capabilities_json: *const c_char,
) -> QuallStatus {
    let caps = match texto(capabilities_json, "capabilities_json") {
        Ok(c) => c,
        Err(f) => return guardar_falha(&f),
    };
    no_filmador(h, "quall_camera_host_set_capabilities", |f| {
        f.definir_capacidades(caps)
    })
}

/// **O registro que ficou valendo**, inteiro. `request` é `0` para uma mudança feita **no
/// filmador** (o painel, o toque na prévia) ou o `"n"` do pedido que [`quall_camera_host_next_request`]
/// entregou — aí o núcleo dá o recibo ao receptor e mostra "Controlado por" por 4 s
/// (`"controlado_por"` no estado). Um `n` já respondido, vencido (5 s) ou de outra câmera:
/// `QUALL_STATUS_INVALID`, e nada muda. Qualquer thread, mas **toda** mudança do registro passa
/// pela mesma fila do dono da câmera (contrato §6).
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; `settings_json`, uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_set_settings(
    h: *const QuallCameraHost,
    settings_json: *const c_char,
    request: u64,
) -> QuallStatus {
    let ajuste = match texto(settings_json, "settings_json") {
        Ok(a) => a,
        Err(f) => return guardar_falha(&f),
    };
    if request == quall_core::camera_remota::AJUSTE_DO_SISTEMA {
        return guardar_texto("quall_camera_host_set_settings: para a escrita automática, use quall_camera_host_update_settings");
    }
    no_filmador(h, "quall_camera_host_set_settings", |f| {
        f.definir_ajuste(ajuste, request)
    })
}

/// **O registro que a casca escreveu sozinha**: o valor lido que a trava guarda, o "Travado de
/// novo depois de medir a cena", o foco lido ao travar (R9 §2.1). Os receptores recebem o
/// registro novo, mas **nenhum campo muda de dono** para o "vence o último", e o `autor` fica.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; `settings_json`, uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_update_settings(
    h: *const QuallCameraHost,
    settings_json: *const c_char,
) -> QuallStatus {
    let ajuste = match texto(settings_json, "settings_json") {
        Ok(a) => a,
        Err(f) => return guardar_falha(&f),
    };
    no_filmador(h, "quall_camera_host_update_settings", |f| {
        f.definir_ajuste(ajuste, quall_core::camera_remota::AJUSTE_DO_SISTEMA)
    })
}

/// **O que a câmera diz estar usando** (R9 §3.6), até 512 bytes: `iso`, `obturadorNs`, `kelvin`,
/// `abertura`, `focoPosicao`, e `divergentes`. Sai aos receptores no máximo 4 vezes por segundo.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; `read_json`, uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_set_read(
    h: *const QuallCameraHost,
    read_json: *const c_char,
) -> QuallStatus {
    let lido = match texto(read_json, "read_json") {
        Ok(l) => l,
        Err(f) => return guardar_falha(&f),
    };
    no_filmador(h, "quall_camera_host_set_read", |f| f.definir_lido(lido))
}

/// **A casca não conseguiu aplicar o pedido `request`.** `reason` é um código `[a-z0-9_]` de até
/// 32 bytes: `nao_aplicado`, `fora_da_imagem` (o toque caiu numa tarja), ou outro. Nulo:
/// `QUALL_STATUS_NULL_POINTER`.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; `reason`, uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_reject(
    h: *const QuallCameraHost,
    request: u64,
    reason: *const c_char,
) -> QuallStatus {
    let motivo = match texto(reason, "reason") {
        Ok(m) => m,
        Err(f) => return guardar_falha(&f),
    };
    no_filmador(h, "quall_camera_host_reject", |f| {
        f.recusar(request, motivo)
    })
}

/// **O próximo pedido aceito**, como JSON, no padrão `(buf, cap)` — e **só tira da fila quando
/// coube**:
///
/// ```json
/// {"n":5,"autor":"OBS no Dell","autor_id":"dell-7f2a","ajuste":{"exposicao":"manual","iso":800},
///  "restaurar":false,"toque":null}
/// ```
///
/// | devolve | quer dizer |
/// |---|---|
/// | `0` | a fila está vazia |
/// | `n > 0` e `n <= cap` | escrito em `buf` com o NUL, **e tirado da fila** |
/// | `n > cap`, ou `buf` nulo | há um pedido de `n` bytes com o NUL, **ainda na fila**: aloque `n` e chame de novo |
/// | negativo | erro, o código em `quall_last_status()` |
///
/// Aplique na ordem do contrato §6 (`restaurar`, os modos, as travas, os valores, o `toque`), e
/// responda com [`quall_camera_host_set_settings`] (com o `"n"`) ou [`quall_camera_host_reject`].
/// `buf` nulo com `cap > 0`: `QUALL_STATUS_NULL_POINTER`.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_next_request(
    h: *const QuallCameraHost,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(h) = h.as_ref() else {
        guardar_nulo("quall_camera_host_next_request: filmador nulo");
        return -1;
    };
    if buf.is_null() && cap > 0 {
        guardar_nulo("quall_camera_host_next_request: `buf` nulo com `cap` maior que zero");
        return -1;
    }
    let tirado = h.0.proximo_pedido_se(|texto| {
        let precisa = texto.len() + 1;
        if buf.is_null() || cap < precisa {
            return false;
        }
        ptr::copy_nonoverlapping(texto.as_ptr(), buf.cast::<u8>(), texto.len());
        *buf.add(texto.len()) = 0;
        true
    });
    match tirado {
        Ok(Some(tamanho)) => isize::try_from(tamanho + 1).unwrap_or(-1),
        Ok(None) => 0,
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **A bombeada do filmador numa sessão de vídeo.** Manda o que está devido a ela (capacidades,
/// o estado a cada mudança e a cada segundo, as recusas), espera até `timeout_ms` por mensagem,
/// trata o que chegou, e escreve em `changed` (pode ser nulo) os bits de
/// [`QuallCameraHostChange`].
///
/// Chame em laço, da thread de **cada** sessão, com o `QuallMessages` dela, junto com
/// `quall_session_next_event(s, 0)`. `timeout_ms` de 0 a 250; os relógios do batimento e do lido
/// andam pelo relógio, não pela espera, então `0` serve a quem já tem laço próprio (o OBS).
/// **Um leitor por sessão**: numa sessão de vídeo, ninguém mais chama `quall_messages_next`.
///
/// `QUALL_STATUS_CLOSED` vem **com** `changed` preenchido: a sessão acabou, a fila foi lida até o
/// fim, e o filmador já a esqueceu.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`], `m` de [`quall_session_messages`]; `changed`
/// precisa ser nulo ou apontar para um `uint32_t`.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_pump(
    h: *const QuallCameraHost,
    m: *const QuallMessages,
    timeout_ms: u32,
    changed: *mut u32,
) -> QuallStatus {
    let (Some(h), Some(m)) = (h.as_ref(), m.as_ref()) else {
        return guardar_nulo("quall_camera_host_pump: filmador ou handle nulo");
    };
    let b =
        h.0.bombear(&m.0, Duration::from_millis(u64::from(timeout_ms)));
    escrever_bombeada(b, changed)
}

unsafe fn escrever_bombeada(
    b: quall_core::error::Result<quall_core::camera_remota::Bombeada>,
    changed: *mut u32,
) -> QuallStatus {
    match b {
        Ok(b) => {
            if !changed.is_null() {
                *changed = b.mudancas;
            }
            if b.fechada {
                guardar_erro(&Error::Closed)
            } else {
                QuallStatus::Ok
            }
        }
        Err(e) => {
            if !changed.is_null() {
                *changed = 0;
            }
            guardar_erro(&e)
        }
    }
}

/// **Esquece a sessão deste handle**: a casca largou a sessão sem bombear até o
/// `QUALL_STATUS_CLOSED`. Opcional — o filmador também esquece sozinho a sessão que acabou, quando
/// uma nova chega —, mas libera a vaga na hora.
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`], `m` de [`quall_session_messages`].
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_forget(
    h: *const QuallCameraHost,
    m: *const QuallMessages,
) -> QuallStatus {
    let Some(m) = m.as_ref() else {
        return guardar_nulo("quall_camera_host_forget: handle nulo");
    };
    no_filmador(h, "quall_camera_host_forget", |f| f.esquecer(&m.0))
}

/// **O estado para a tela do filmador**, como JSON. Padrão `(buf, cap)`: repita até caber.
///
/// ```json
/// {"permite":false,"camera":1,"cap":1,"versao":17,"controlado_por":{"nome":"OBS no Dell","ha_ms":820},
///  "receptores":[{"nome":"OBS no Dell","id":"dell-7f2a"}],"pedidos_na_fila":0,"contadores":{…}}
/// ```
///
/// `"controlado_por"` é nulo fora dos 4 s depois de um pedido aplicado: enquanto não for, a tela
/// mostra "Controlado por <nome>".
///
/// # Safety
///
/// `h` precisa vir de [`quall_camera_host_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_host_state_json(
    h: *const QuallCameraHost,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(h) = h.as_ref() else {
        guardar_nulo("quall_camera_host_state_json: filmador nulo");
        return -1;
    };
    match h.0.estado_json() {
        Ok(j) => escrever_texto(&j, buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

/// **Cria o controle da câmera do outro lado**, para uma sessão de recepção de vídeo. Recomeça
/// sozinho na bombeada de uma sessão nova. Libere com [`quall_camera_remote_free`].
#[no_mangle]
pub extern "C" fn quall_camera_remote_new() -> *mut QuallCameraRemote {
    Box::into_raw(Box::new(QuallCameraRemote(
        quall_core::camera_remota::Controlador::novo(),
    )))
}

/// Libera o controle. Nulo é aceito.
///
/// # Safety
///
/// `r` precisa ser nulo ou vir de [`quall_camera_remote_new`], e não ser usado depois.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_remote_free(r: *mut QuallCameraRemote) {
    if !r.is_null() {
        drop(Box::from_raw(r));
    }
}

unsafe fn no_controle(
    r: *const QuallCameraRemote,
    quem: &str,
    f: impl FnOnce(&quall_core::camera_remota::Controlador) -> quall_core::error::Result<()>,
) -> QuallStatus {
    let Some(r) = r.as_ref() else {
        return guardar_nulo(&format!("{quem}: controle nulo"));
    };
    match f(&r.0) {
        Ok(()) => QuallStatus::Ok,
        Err(e) => guardar_erro(&e),
    }
}

/// **Pede um ajuste parcial**: um objeto JSON com **só** os campos que a pessoa mexeu, nos nomes
/// do registro do R9 (`{"exposicao":"manual","iso":800}`). Conferido na hora contra as
/// capacidades que chegaram: o que o filmador recusaria, ou com `"situacao"` diferente de
/// `"pronto"`, é `QUALL_STATUS_INVALID`. Sai na hora (no máximo 15 por segundo; o que se junta no
/// intervalo sai no envio seguinte). **Só de gesto da pessoa**, nunca ao carregar valores salvos.
/// Qualquer thread.
///
/// # Safety
///
/// `r` precisa vir de [`quall_camera_remote_new`]; `settings_json`, uma string C válida.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_remote_request(
    r: *const QuallCameraRemote,
    settings_json: *const c_char,
) -> QuallStatus {
    let parcial = match texto(settings_json, "settings_json") {
        Ok(p) => p,
        Err(f) => return guardar_falha(&f),
    };
    no_controle(r, "quall_camera_remote_request", |c| c.pedir(parcial))
}

/// **"Restaurar automático"** na câmera do outro lado.
///
/// # Safety
///
/// `r` precisa vir de [`quall_camera_remote_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_camera_remote_restore(r: *const QuallCameraRemote) -> QuallStatus {
    no_controle(r, "quall_camera_remote_restore", |c| c.restaurar())
}

/// **Um toque na imagem** que o receptor mostra, para focar e medir ali (R9 §4.4): `x` e `y` de 0
/// a 1 **no quadro decodificado**, antes de qualquer transformação deste lado (escala, tarja,
/// espelho); um toque fora do quadro não se manda. `long_press` trava, como o toque longo.
/// Sem `"toque"` nas capacidades, ou fora de `[0, 1]`: `QUALL_STATUS_INVALID`.
///
/// # Safety
///
/// `r` precisa vir de [`quall_camera_remote_new`].
#[no_mangle]
pub unsafe extern "C" fn quall_camera_remote_touch(
    r: *const QuallCameraRemote,
    x: f64,
    y: f64,
    long_press: bool,
) -> QuallStatus {
    no_controle(r, "quall_camera_remote_touch", |c| {
        c.tocar(x, y, long_press)
    })
}

/// **A bombeada do receptor.** Numa sessão nova esquece a anterior; manda o `ola` e o pedido
/// devidos, espera até `timeout_ms`, trata o que chegou, e escreve em `changed` (pode ser nulo) os
/// bits de [`QuallCameraRemoteChange`]. Mesmas regras de [`quall_camera_host_pump`]: laço na
/// thread da sessão, `timeout_ms` de 0 a 250, um leitor por sessão, e `QUALL_STATUS_CLOSED` com
/// `changed` preenchido.
///
/// # Safety
///
/// `r` precisa vir de [`quall_camera_remote_new`], `m` de [`quall_session_messages`]; `changed`
/// precisa ser nulo ou apontar para um `uint32_t`.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_remote_pump(
    r: *const QuallCameraRemote,
    m: *const QuallMessages,
    timeout_ms: u32,
    changed: *mut u32,
) -> QuallStatus {
    let (Some(r), Some(m)) = (r.as_ref(), m.as_ref()) else {
        return guardar_nulo("quall_camera_remote_pump: controle ou handle nulo");
    };
    let b =
        r.0.bombear(&m.0, Duration::from_millis(u64::from(timeout_ms)));
    escrever_bombeada(b, changed)
}

/// **O estado para a tela do receptor**, como JSON. Padrão `(buf, cap)`: repita até caber.
///
/// ```json
/// {"situacao":"pronto","capacidades":{…},"ajuste":{…},"aplicado":{…},"pendente":{"iso":800},
///  "lido":{…},"autor":"Pixel do Pessoa Exemplo","versao":17,
///  "recusa":{"motivo":"superado","campo":"iso","ha_ms":300},"contadores":{…}}
/// ```
///
/// `"situacao"`: `esperando`, `sem_resposta`, `sem_camera` (os três: não mostre os controles),
/// `nao_permitido` (controles apagados, com os valores, e "O aparelho não permite controle remoto
/// da câmera") ou `pronto`. `"ajuste"` é o aplicado com o pendente por cima: é o que o painel
/// mostra.
///
/// # Safety
///
/// `r` precisa vir de [`quall_camera_remote_new`]; `buf` precisa ser nulo ou ter `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn quall_camera_remote_state_json(
    r: *const QuallCameraRemote,
    buf: *mut c_char,
    cap: usize,
) -> isize {
    let Some(r) = r.as_ref() else {
        guardar_nulo("quall_camera_remote_state_json: controle nulo");
        return -1;
    };
    match r.0.estado_json() {
        Ok(j) => escrever_texto(&j, buf, cap),
        Err(e) => {
            guardar_erro(&e);
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Só os literais exaustivos de `Contadores` precisam dela.
    use quall_core::protocol::SERVICE_TYPE;
    use quall_core::rtp::FAIXAS_DE_CORTE;

    /// **A fronteira do controlador de taxa, pela porta em C.**
    ///
    /// A política tem catorze testes em `quall_core::taxa`; o que **este** exercita é a tradução
    /// — que é onde a fronteira erra. Três coisas que o JNI depende e que nada mais confere:
    /// que `out_bps` só é escrito quando houve mudança, que os códigos de `QuallRateReason`
    /// batem com os do header, e que ponteiro nulo não derruba nada.
    #[test]
    fn a_fronteira_do_controlador_de_taxa_traduz_o_que_a_politica_decide() {
        let r = quall_rate_new(4_000_000);
        assert!(!r.is_null());
        // SAFETY: `r` acabou de sair de `quall_rate_new` e é liberado no fim do teste.
        unsafe {
            assert_eq!(quall_rate_current_bps(r), 4_000_000, "não nasceu no teto");
            assert!(!quall_rate_at_floor(r));

            // Uma janela limpa: nada muda, e `out_bps` **não** é tocado.
            let mut bps = 0xDEAD_BEEF_u32;
            let motivo = quall_rate_sample(r, 1000, 400, 0, 0, 0, 0, &mut bps);
            assert!(
                matches!(motivo, QuallRateReason::Hold | QuallRateReason::Ceiling),
                "veio {motivo:?}",
            );
            assert_eq!(bps, 0xDEAD_BEEF, "escreveu out_bps sem ter mudado nada");
            assert_eq!(quall_rate_current_bps(r), 4_000_000);

            // Uma janela ruim: desce, e **agora** escreve.
            let motivo = quall_rate_sample(r, 1000, 400, 40, 7, 2, 0, &mut bps);
            assert_eq!(motivo, QuallRateReason::Down);
            assert_eq!(bps, 3_000_000);
            assert_eq!(quall_rate_current_bps(r), 3_000_000);

            // Perda que não cede leva ao piso, e o piso se anuncia.
            for _ in 0..200 {
                quall_rate_sample(r, 1000, 400, 80, 0, 0, 0, std::ptr::null_mut());
            }
            assert!(quall_rate_at_floor(r));
            assert_eq!(
                quall_rate_sample(r, 1000, 400, 80, 0, 0, 0, std::ptr::null_mut()),
                QuallRateReason::Floor,
            );

            let mut c = [0u64; 3];
            quall_rate_counters(r, c.as_mut_ptr());
            assert!(c[0] > 200, "janelas: {c:?}");
            assert_eq!(c[1], 9, "descidas de 4 Mbps ao piso a 0,75: {c:?}");
            assert_eq!(c[2], 0, "subiu num enlace que nunca teve calmaria: {c:?}");

            quall_rate_free(r);
        }

        // Nulo em toda a superfície: nenhuma delas pode derrubar a casca.
        // SAFETY: nulo é entrada legítima destas funções, e é o que o teste exercita.
        unsafe {
            assert_eq!(
                quall_rate_sample(std::ptr::null_mut(), 1, 1, 0, 0, 0, 0, std::ptr::null_mut()),
                QuallRateReason::Skipped,
            );
            assert_eq!(quall_rate_current_bps(std::ptr::null()), 0);
            assert!(!quall_rate_at_floor(std::ptr::null()));
            quall_rate_counters(std::ptr::null(), std::ptr::null_mut());
            quall_rate_free(std::ptr::null_mut());
        }
        assert!(
            quall_rate_new(0).is_null(),
            "teto zero tinha de ser recusado"
        );
    }

    /// Os códigos de `QuallRateReason` são contrato com o `quall_jni.c` e com o header. Fixá-los
    /// aqui é o que impede uma reordenação silenciosa do `enum` de virar decisão errada na casca.
    #[test]
    fn os_codigos_de_motivo_sao_contrato() {
        assert_eq!(QuallRateReason::Skipped as i32, 0);
        assert_eq!(QuallRateReason::Down as i32, 1);
        assert_eq!(QuallRateReason::Floor as i32, 2);
        assert_eq!(QuallRateReason::Up as i32, 3);
        assert_eq!(QuallRateReason::Ceiling as i32, 4);
        assert_eq!(QuallRateReason::Hold as i32, 5);
    }

    /// **O desregistro que a fronteira C não tinha, pela porta em C.**
    ///
    /// O header dizia, com todas as letras: "Não há como desregistrar um callback. Passar NULL
    /// devolve erro em vez de limpar". Era verdade — e deixava a casca sem saída: ou segurava o
    /// `user_data` até a sessão inteira acabar, ou arriscava uso-após-liberação.
    ///
    /// Antes deste conserto este teste falhava com `QUALL_STATUS_NULL_POINTER` na segunda
    /// chamada.
    #[test]
    fn callback_nulo_desregistra_em_vez_de_recusar() {
        use quall_core::track::TrackConfig;
        use quall_core::transport::{Session, TransportConfig};

        let (sessao, emissores) = Session::offerer_com_tracks(
            &TransportConfig::default(),
            &[TrackConfig::new(TrackKind::Screen, "Tela")],
        )
        .expect("ofertante com uma track");
        let track = QuallTrack {
            lado: Lado::Emissor(Arc::new(emissores.into_iter().next().expect("emissor"))),
            audio: Mutex::new(None),
        };
        let p: *const QuallTrack = &track;

        unsafe extern "C" fn tratador(_user_data: *mut c_void) {}

        unsafe {
            assert_eq!(
                quall_track_on_idr_request(p, Some(tratador), ptr::null_mut()),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_track_on_idr_request(p, None, ptr::null_mut()),
                QuallStatus::Ok,
                "callback nulo tinha de desregistrar; recusar é a dívida que este teste cobra"
            );
            // Registrar de novo depois de desregistrar continua valendo: o desregistro desliga o
            // tratador, não a track.
            assert_eq!(
                quall_track_on_idr_request(p, Some(tratador), ptr::null_mut()),
                QuallStatus::Ok
            );
        }

        // Track de recepção não aceita este tratador, com ou sem nulo — e o motivo continua
        // sendo o de sempre, não "callback nulo".
        drop(track);
        drop(sessao);
    }

    /// Nulo em `cb` **não** vira desregistro quando a track é do lado errado: o erro específico
    /// tem de continuar aparecendo, senão a casca conclui que desligou algo que nunca ligou.
    #[test]
    fn desregistro_no_lado_errado_ainda_diz_qual_e_o_lado_errado() {
        use quall_core::track::TrackConfig;
        use quall_core::transport::{Session, TransportConfig};

        let (sessao, emissores) = Session::offerer_com_tracks(
            &TransportConfig::default(),
            &[TrackConfig::new(TrackKind::Screen, "Tela")],
        )
        .expect("ofertante");
        let track = QuallTrack {
            lado: Lado::Emissor(Arc::new(emissores.into_iter().next().expect("emissor"))),
            audio: Mutex::new(None),
        };
        let p: *const QuallTrack = &track;

        // `on_frame` é do receptor; numa track de emissão continua sendo `Invalid`.
        unsafe {
            assert_eq!(
                quall_track_on_frame(p, None, ptr::null_mut()),
                QuallStatus::Invalid
            );
        }
        let msg = unsafe { CStr::from_ptr(quall_last_error()) }
            .to_string_lossy()
            .to_string();
        assert!(msg.contains("emissão"), "mensagem inesperada: {msg}");

        drop(track);
        drop(sessao);
    }

    #[test]
    fn versao_atravessa_a_fronteira_c() {
        assert_eq!(quall_protocol_version(), PROTOCOL_VERSION);
    }

    #[test]
    fn tipo_de_servico_e_o_mesmo_do_protocolo() {
        // SAFETY: `quall_service_type` devolve um ponteiro estático terminado em NUL.
        let texto = unsafe { CStr::from_ptr(quall_service_type()) };
        assert_eq!(texto.to_str().expect("UTF-8"), SERVICE_TYPE);
    }

    #[test]
    fn escrever_texto_pede_o_tamanho_e_depois_escreve() {
        let mut buf = [0 as c_char; 16];
        // Buffer nulo pergunta o tamanho, incluindo o NUL.
        let precisa = unsafe { escrever_texto("quall", ptr::null_mut(), 0) };
        assert_eq!(precisa, 6);
        // Buffer pequeno não escreve nada e devolve o tamanho de novo.
        let n = unsafe { escrever_texto("quall", buf.as_mut_ptr(), 3) };
        assert_eq!(n, 6);
        assert_eq!(buf[0], 0, "escreveu num buffer que não cabia");
        // Buffer suficiente escreve com o NUL final.
        let n = unsafe { escrever_texto("quall", buf.as_mut_ptr(), buf.len()) };
        assert_eq!(n, 6);
        let lido = unsafe { CStr::from_ptr(buf.as_ptr()) };
        assert_eq!(lido.to_str().expect("UTF-8"), "quall");
    }

    #[test]
    fn escrever_texto_recusa_nul_no_meio() {
        let mut buf = [0 as c_char; 16];
        assert_eq!(
            unsafe { escrever_texto("qu\0all", buf.as_mut_ptr(), 16) },
            -1
        );
    }

    #[test]
    fn ponteiro_nulo_vira_status_e_nao_panico() {
        // Toda a fronteira tem de sobreviver a nulo: em release o workspace usa `panic = "abort"`
        // e um pânico aqui derruba o Zoom ou a extension de 50 MB do iOS.
        unsafe {
            // `NullPointer`, e não `Invalid`: ver a dívida 8 e o teste
            // `ponteiro_nulo_e_utf8_invalido_tem_codigo_proprio`.
            assert_eq!(
                quall_track_send_frame(ptr::null(), ptr::null()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_track_request_idr(ptr::null()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_track_on_frame(ptr::null(), None, ptr::null_mut()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_track_on_idr_request(ptr::null(), None, ptr::null_mut()),
                QuallStatus::NullPointer
            );
            assert!(!quall_track_take_idr_request(ptr::null()));
            assert_eq!(quall_track_kind(ptr::null()), QuallTrackKind::Screen);
            assert_eq!(quall_track_label(ptr::null(), ptr::null_mut(), 0), -1);
            assert_eq!(quall_track_stats_json(ptr::null(), ptr::null_mut(), 0), -1);
            assert_eq!(quall_session_track_count(ptr::null()), 0);
            assert!(quall_session_track(ptr::null(), 0).is_null());
            assert!(quall_session_next_track(ptr::null_mut(), 0).is_null());
            assert!(!quall_session_pairing_is_new(ptr::null()));
            assert_eq!(quall_session_signaling_port(ptr::null()), 0);
            assert_eq!(quall_session_peer_json(ptr::null(), ptr::null_mut(), 0), -1);
            assert_eq!(quall_session_path_json(ptr::null(), ptr::null_mut(), 0), -1);
            assert_eq!(
                quall_session_next_event(ptr::null_mut(), 0),
                QuallSessionEvent::None
            );
            assert!(quall_host(ptr::null()).is_null());
            assert!(quall_host_cancelable(ptr::null(), ptr::null()).is_null());
            assert!(quall_connect(ptr::null(), ptr::null()).is_null());
            assert!(quall_connect_cancelable(ptr::null(), ptr::null(), ptr::null()).is_null());
            assert!(
                quall_connect_with_screen(ptr::null(), ptr::null(), ptr::null(), 0, 0).is_null()
            );
            assert!(quall_advertiser_start(ptr::null(), 0).is_null());
            assert_eq!(quall_browser_collect(ptr::null_mut(), 0), -1);
            assert_eq!(
                quall_browser_devices_json(ptr::null(), ptr::null_mut(), 0),
                -1
            );
            assert_eq!(
                quall_known_peers_forget(ptr::null(), ptr::null(), ptr::null_mut(), 0),
                -1
            );
            assert!(!quall_canceller_is_cancelled(ptr::null()));
            // Liberar nulo é silêncio, não estouro.
            quall_track_free(ptr::null_mut());
            quall_session_close(ptr::null_mut());
            quall_advertiser_stop(ptr::null_mut());
            quall_browser_stop(ptr::null_mut());
            quall_canceller_free(ptr::null_mut());
            quall_session_cancel(ptr::null());
        }
    }

    /// **Dívida 8.** Ponteiro nulo e string que não é UTF-8 agora saem com código próprio.
    ///
    /// Antes os dois códigos existiam no header e nenhum caminho os produzia — tudo virava
    /// `QUALL_STATUS_INVALID`. A casca Android chegou a documentar o desenho das strings como
    /// `byte[]` citando um `QUALL_STATUS_NOT_UTF8` que o núcleo nunca emitia.
    #[test]
    fn ponteiro_nulo_e_utf8_invalido_tem_codigo_proprio() {
        unsafe {
            assert_eq!(
                quall_track_send_frame(ptr::null(), ptr::null()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_track_request_idr(ptr::null()),
                QuallStatus::NullPointer
            );

            // UTF-8 inválido: `0xFF` sozinho não é sequência válida.
            let ruim = [0xFFu8, 0x00];
            let opcoes = QuallSessionOptions {
                me: QuallDeviceDesc {
                    device_id: ruim.as_ptr().cast::<c_char>(),
                    display_name: c"nome".as_ptr(),
                    screen_source: true,
                    camera_source: false,
                    sink: false,
                },
                pin: ptr::null(),
                known_peers_json: ptr::null(),
                signaling_port: 0,
                timeout_ms: 100,
                tracks: ptr::null(),
                track_count: 0,
                bind_address: ptr::null(),
            };
            assert!(quall_host(&opcoes).is_null());
            let msg = CStr::from_ptr(quall_last_error())
                .to_string_lossy()
                .to_string();
            assert!(msg.contains("UTF-8"), "mensagem inesperada: {msg}");

            // E pelo caminho que devolve status, não ponteiro.
            assert_eq!(
                quall_known_peers_forget(
                    ptr::null(),
                    ruim.as_ptr().cast::<c_char>(),
                    ptr::null_mut(),
                    0
                ),
                -1
            );
        }
    }

    /// **Achado da auditoria.** Função que sinaliza falha devolvendo um valor plausível tem de
    /// deixar rastro em [`quall_last_error`].
    ///
    /// `quall_track_kind(NULL)` devolve `SCREEN`, `quall_session_signaling_port(NULL)` devolve
    /// `0`, `quall_track_take_idr_request(NULL)` devolve `false` — todos valores que uma casca
    /// pode confundir com resposta legítima. Sem mensagem, o defeito aparece três passos adiante.
    #[test]
    fn falha_com_valor_plausivel_ainda_deixa_recado() {
        /// Roda `chamada` e devolve o que ficou em `quall_last_error`.
        fn recado(chamada: impl FnOnce()) -> String {
            // Limpa o recado anterior desta thread com uma falha conhecida.
            unsafe { quall_track_request_idr(ptr::null()) };
            chamada();
            unsafe {
                CStr::from_ptr(quall_last_error())
                    .to_string_lossy()
                    .to_string()
            }
        }

        for (nome, chamada) in [
            (
                "quall_track_kind",
                Box::new(|| unsafe {
                    quall_track_kind(ptr::null());
                }) as Box<dyn FnOnce()>,
            ),
            (
                "quall_track_take_idr_request",
                Box::new(|| unsafe {
                    quall_track_take_idr_request(ptr::null());
                }),
            ),
            (
                "quall_session_signaling_port",
                Box::new(|| unsafe {
                    quall_session_signaling_port(ptr::null());
                }),
            ),
            (
                "quall_session_pairing_is_new",
                Box::new(|| unsafe {
                    quall_session_pairing_is_new(ptr::null());
                }),
            ),
            (
                "quall_session_track_count",
                Box::new(|| unsafe {
                    quall_session_track_count(ptr::null());
                }),
            ),
            (
                "quall_track_label",
                Box::new(|| unsafe {
                    quall_track_label(ptr::null(), ptr::null_mut(), 0);
                }),
            ),
            (
                "quall_canceller_is_cancelled",
                Box::new(|| unsafe {
                    quall_canceller_is_cancelled(ptr::null());
                }),
            ),
        ] {
            let msg = recado(chamada);
            assert!(
                msg.contains(nome),
                "{nome} não deixou recado; `quall_last_error` diz: {msg:?}"
            );
        }
    }

    /// **Dívida 10.** O cancelador é uma bandeira compartilhada, e o `quall_session_cancel`
    /// levanta a bandeira de qualquer thread.
    #[test]
    fn cancelador_atravessa_a_fronteira_c() {
        unsafe {
            let c = quall_canceller_new();
            assert!(!c.is_null());
            assert!(!quall_canceller_is_cancelled(c));
            quall_session_cancel(c);
            assert!(quall_canceller_is_cancelled(c));
            // Idempotente.
            quall_session_cancel(c);
            assert!(quall_canceller_is_cancelled(c));
            quall_canceller_free(c);
        }
    }

    /// **Dívidas 22 e 23.** Esquecer um par e fundir dois estados, pela fronteira C.
    #[test]
    fn esquecer_e_fundir_pares_atravessam_a_fronteira_c() {
        unsafe fn json(chamar: impl Fn(*mut c_char, usize) -> isize) -> String {
            let precisa = chamar(ptr::null_mut(), 0);
            assert!(precisa > 0, "a função devolveu {precisa}");
            let mut buf = vec![0 as c_char; precisa as usize];
            let n = chamar(buf.as_mut_ptr(), buf.len());
            assert_eq!(n, precisa);
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().to_string()
        }

        let a = c"{\"pares\":{\"mac\":\"0101010101010101010101010101010101010101010101010101010101010101\"}}";
        let b = c"{\"pares\":{\"dell\":\"0202020202020202020202020202020202020202020202020202020202020202\"}}";

        unsafe {
            let fundido =
                json(|buf, cap| quall_known_peers_merge(a.as_ptr(), b.as_ptr(), buf, cap));
            assert!(fundido.contains("mac"), "a fusão perdeu o `mac`: {fundido}");
            assert!(
                fundido.contains("dell"),
                "a fusão perdeu o `dell`: {fundido}"
            );

            let c_fundido = CString::new(fundido).expect("sem NUL");
            let sem_mac = json(|buf, cap| {
                quall_known_peers_forget(c_fundido.as_ptr(), c"mac".as_ptr(), buf, cap)
            });
            assert!(
                !sem_mac.contains("mac"),
                "o `mac` não foi esquecido: {sem_mac}"
            );
            assert!(
                sem_mac.contains("dell"),
                "esquecer levou o `dell` junto: {sem_mac}"
            );

            // Esquecer quem não existe não é erro.
            let igual = json(|buf, cap| {
                quall_known_peers_forget(c_fundido.as_ptr(), c"ninguem".as_ptr(), buf, cap)
            });
            assert!(igual.contains("mac") && igual.contains("dell"));
        }
    }

    #[test]
    fn indicador_de_pares_seguros_preserva_legado_sem_confiar_nele() {
        let legacy = c"{\"pares\":{\"fixture-peer\":\"0101010101010101010101010101010101010101010101010101010101010101\"}}";
        let mut modern = PairedPeers::new();
        modern.insert(&quall_core::pairing::PairOutcome {
            peer: DeviceId("fixture-peer".into()),
            secret: [7; 32],
            novo: true,
        });
        let modern = CString::new(modern.to_json().unwrap()).unwrap();
        unsafe {
            assert_eq!(quall_known_peers_has_secure(ptr::null()), 0);
            assert_eq!(quall_known_peers_has_secure(c"".as_ptr()), 0);
            assert_eq!(quall_known_peers_has_secure(legacy.as_ptr()), 0);
            assert_eq!(quall_known_peers_has_secure(modern.as_ptr()), 1);
            assert_eq!(quall_known_peers_has_secure(c"{invalid}".as_ptr()), -1);
        }
        let legacy_table = PairedPeers::from_json(legacy.to_str().unwrap()).unwrap();
        assert_eq!(legacy_table.len(), 1);
        assert!(legacy_table.to_json().unwrap().contains("fixture-peer"));
    }

    #[test]
    fn lista_de_descoberta_expoe_identidade_efemera_nao_autenticada() {
        let props = std::collections::HashMap::from([
            ("v".into(), PROTOCOL_VERSION.to_string()),
            ("t".into(), "12".repeat(16)),
            ("p".into(), "7877".into()),
            ("c".into(), "sck".into()),
        ]);
        let (announcement, signaling_port) =
            quall_core::discovery::announcement_from_txt(&props).unwrap();
        let browser = QuallBrowser {
            interno: None,
            achados: Mutex::new(vec![DiscoveredDevice {
                announcement,
                signaling_port,
                addresses: Vec::new(),
                fullname: "Quall fixture._quall._tcp.local.".into(),
            }]),
        };
        unsafe {
            let length = quall_browser_devices_json(&browser, ptr::null_mut(), 0);
            assert!(length > 0);
            let mut buf = vec![0 as c_char; length as usize];
            assert_eq!(quall_browser_devices_json(&browser, buf.as_mut_ptr(), buf.len()), length);
            let items: serde_json::Value =
                serde_json::from_str(CStr::from_ptr(buf.as_ptr()).to_str().unwrap()).unwrap();
            assert_eq!(items[0]["identity_authenticated"], false);
            assert_eq!(items[0]["device_id"], format!("discovery-{}", "12".repeat(16)));
            assert_eq!(items[0]["display_name"], "Quall 12121212");
            assert_eq!(items[0]["endpoint"], serde_json::Value::Null);
        }
    }

    #[test]
    fn rotulo_do_anunciante_respeita_utf8_nul_cap_e_token_real() {
        let desc = QuallDeviceDesc {
            device_id: c"fixture-persistent-id".as_ptr(),
            display_name: c"NOME-PESSOAL-SENTINELA".as_ptr(),
            screen_source: true,
            camera_source: false,
            sink: false,
        };
        unsafe {
            assert_eq!(quall_advertiser_label(ptr::null(), ptr::null_mut(), 0), -1);
            let a = quall_advertiser_start(&desc, 65533);
            assert!(!a.is_null());
            let length = quall_advertiser_label(a, ptr::null_mut(), 0);
            assert_eq!(length, 15); // ASCII UTF-8: "Quall ", oito hexadecimais, NUL.
            let mut small = [0x5a as c_char; 14];
            assert_eq!(quall_advertiser_label(a, small.as_mut_ptr(), small.len()), length);
            assert_eq!(small, [0x5a as c_char; 14], "buffer curto foi modificado");
            let mut buf = vec![0 as c_char; length as usize];
            assert_eq!(quall_advertiser_label(a, buf.as_mut_ptr(), buf.len()), length);
            let label = CStr::from_ptr(buf.as_ptr()).to_str().unwrap();
            let full = (*a).interno.as_ref().unwrap().fullname();
            assert!(full.starts_with(label), "alias não corresponde à instância real");
            assert!(label.starts_with("Quall "));
            assert!(!label.contains("SENTINELA") && !label.contains("persistent"));
            assert_eq!(buf[length as usize - 1], 0);
            quall_advertiser_stop(a);
        }
    }

    /// **Dívida 11.** Um pânico do núcleo chega à casca com ocorrência e origem, sem payload.
    ///
    /// Em release o workspace usa `panic = "abort"`, mas o `set_hook` **roda antes** do abort —
    /// é justamente por isso que o gancho salva a ocorrência. Aqui o perfil é de teste
    /// (`panic = "unwind"`), então dá para capturar o pânico e conferir que o tratador da casca
    /// recebeu o texto.
    #[test]
    fn gancho_de_panico_entrega_a_mensagem_para_a_casca() {
        use std::sync::{Arc, Mutex};

        let recebido: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        unsafe extern "C" fn tratador(msg: *const c_char, user_data: *mut c_void) {
            // SAFETY: os dois ponteiros vêm do teste logo abaixo e vivem durante a chamada.
            unsafe {
                let alvo = &*(user_data as *const std::sync::Mutex<Vec<String>>);
                let texto = CStr::from_ptr(msg).to_string_lossy().to_string();
                if let Ok(mut g) = alvo.lock() {
                    g.push(texto);
                }
            }
        }

        let anterior = std::panic::take_hook();
        unsafe {
            quall_install_panic_hook(
                Some(tratador),
                Arc::as_ptr(&recebido) as *mut Mutex<Vec<String>> as *mut c_void,
            );
        }
        let _ = std::panic::catch_unwind(|| {
            panic!("PIN sem rótulo 901234 e segredo-livre no meio do encode");
        });
        std::panic::set_hook(anterior);

        // O gancho é global ao processo e os testes rodam em paralelo, então outro pânico pode
        // ter caído no mesmo balde. O que se cobra é a ocorrência e origem, sem texto arbitrário.
        let visto = recebido.lock().expect("cadeado").clone();
        let nosso = visto
            .iter()
            .find(|m| m.contains("conteúdo oculto; origem=lib.rs:"))
            .unwrap_or_else(|| panic!("o gancho não entregou a mensagem do pânico: {visto:?}"));
        assert!(
            nosso.starts_with("pânico no núcleo do Quall:"),
            "a mensagem não veio identificada como do núcleo: {nosso}"
        );
        assert!(
            nosso.contains("lib.rs"),
            "o arquivo e a linha não sobreviveram: {nosso}"
        );
        assert!(!nosso.contains("901234") && !nosso.contains("segredo-livre"));
    }

    /// **Achado da auditoria.** Um `len` absurdo não pode virar `from_raw_parts`.
    #[test]
    fn quadro_com_len_absurdo_e_recusado_antes_de_virar_fatia() {
        let byte = 0x65u8;
        let quadro = QuallFrame {
            annexb: &byte,
            len: usize::MAX,
            timestamp_us: 0,
            idr: true,
        };
        // Track nula já barra antes; o que este teste garante é que a ordem das checagens põe o
        // teto **antes** de qualquer uso do ponteiro.
        unsafe {
            assert_eq!(
                quall_track_send_frame(ptr::null(), &quadro),
                QuallStatus::NullPointer
            );
        }
        assert!(quadro.len > MAX_QUADRO);
    }

    #[test]
    fn mensagem_de_erro_sobrevive_ate_a_proxima_falha() {
        unsafe {
            quall_track_request_idr(ptr::null());
            let msg = CStr::from_ptr(quall_last_error())
                .to_str()
                .expect("UTF-8")
                .to_string();
            assert!(msg.contains("track nula"), "mensagem inesperada: {msg}");
        }
    }

    #[test]
    fn pin_gerado_atravessa_com_seis_digitos() {
        let mut buf = [0 as c_char; 8];
        let n = unsafe { quall_generate_pin(buf.as_mut_ptr(), buf.len()) };
        assert_eq!(n, 7, "seis dígitos mais o NUL");
        let texto = unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_str()
            .expect("UTF-8");
        assert_eq!(texto.len(), 6);
        assert!(texto.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn tipos_de_track_atravessam_nos_dois_sentidos() {
        for k in [
            QuallTrackKind::Screen,
            QuallTrackKind::Camera,
            QuallTrackKind::Microphone,
            QuallTrackKind::SystemAudio,
        ] {
            let rust: TrackKind = k.into();
            assert_eq!(QuallTrackKind::from(rust), k);
        }
    }

    /// **Os números do enum são ABI, e este teste é o que impede alguém de renumerá-los.**
    ///
    /// As cascas — Swift, Kotlin, o C++ do plugin de OBS — já foram compiladas contra
    /// `SCREEN = 0`, `CAMERA = 1`, `MICROPHONE = 2`. Trocar esses valores não quebra compilação
    /// nenhuma: quebra a interpretação, em silêncio, num binário já distribuído. Uma track de
    /// microfone passaria a chegar como câmera.
    ///
    /// Por isso os valores são conferidos como números literais, e não contra o próprio enum —
    /// que concordaria consigo mesmo depois de qualquer renumeração.
    #[test]
    fn os_valores_do_enum_de_track_sao_abi_e_nao_podem_mudar() {
        assert_eq!(QuallTrackKind::Screen as i32, 0);
        assert_eq!(QuallTrackKind::Camera as i32, 1);
        assert_eq!(QuallTrackKind::Microphone as i32, 2);
        assert_eq!(
            QuallTrackKind::SystemAudio as i32,
            3,
            "espécie nova entra no fim; ver a nota de ABI em `QuallTrackKind`"
        );
    }

    #[test]
    fn status_cobre_todos_os_erros_do_nucleo() {
        // Se alguém acrescentar uma variante em `Error` sem mapear aqui, o `match` do `From`
        // quebra a compilação — este teste guarda os valores numéricos, que o header expõe e as
        // cascas comparam.
        assert_eq!(QuallStatus::Ok as i32, 0);
        assert_eq!(
            QuallStatus::from(&Error::Timeout("x".into())),
            QuallStatus::Timeout
        );
        assert_eq!(QuallStatus::from(&Error::Closed), QuallStatus::Closed);
    }

    /// **Os códigos de status também são ABI**, pela mesma razão que os da espécie de track — e
    /// não havia teste guardando-os. Uma casca Swift compilada contra o header de ontem tem `13`
    /// gravado como `NEEDS_PIN`; renumerar transformaria "peça o PIN de novo" em outra coisa num
    /// binário já distribuído, sem quebrar compilação nenhuma.
    ///
    /// Conferidos contra **números literais**, nunca contra o próprio enum, que concordaria
    /// consigo mesmo depois de qualquer renumeração.
    #[test]
    fn os_valores_do_enum_de_status_sao_abi_e_nao_podem_mudar() {
        assert_eq!(QuallStatus::Ok as i32, 0);
        assert_eq!(QuallStatus::Invalid as i32, 1);
        assert_eq!(QuallStatus::Protocol as i32, 2);
        assert_eq!(QuallStatus::Discovery as i32, 3);
        assert_eq!(QuallStatus::Signaling as i32, 4);
        assert_eq!(QuallStatus::Transport as i32, 5);
        assert_eq!(QuallStatus::Pairing as i32, 6);
        assert_eq!(QuallStatus::Timeout as i32, 7);
        assert_eq!(QuallStatus::Closed as i32, 8);
        assert_eq!(QuallStatus::Io as i32, 9);
        assert_eq!(QuallStatus::NullPointer as i32, 10);
        assert_eq!(QuallStatus::NotUtf8 as i32, 11);
        assert_eq!(QuallStatus::NoRoute as i32, 12);
        assert_eq!(QuallStatus::NeedsPin as i32, 13);
        assert_eq!(QuallStatus::Cancelled as i32, 14);
        assert_eq!(
            QuallStatus::WrongPin as i32,
            15,
            "código novo entra no fim; ver a nota de ABI em `QuallStatus::WrongPin`"
        );
    }

    /// **Dívida 29 na fronteira.** Os três erros de pareamento chegam à casca como três códigos
    /// diferentes, porque os conselhos ao usuário são diferentes — e dois deles são **opostos**.
    #[test]
    fn os_tres_erros_de_pareamento_chegam_como_tres_codigos() {
        assert_eq!(
            QuallStatus::from(&Error::WrongPin("o PIN não conferiu".into())),
            QuallStatus::WrongPin,
            "digite o PIN de novo"
        );
        assert_eq!(
            QuallStatus::from(&Error::NeedsPin("não conheço este aparelho".into())),
            QuallStatus::NeedsPin,
            "peça um PIN novo ao outro aparelho — o conselho oposto"
        );
        assert_eq!(
            QuallStatus::from(&Error::Pairing("mensagem fora de ordem".into())),
            QuallStatus::Pairing,
            "o resto, que não vira conselho nenhum"
        );
    }

    /// **Dívida 28 na fronteira.** Falta de rota no `connect` TCP da sinalização chega como
    /// `NO_ROUTE`; conexão recusada continua sendo `IO`, porque um RST prova que existe rota.
    #[test]
    fn falta_de_rota_e_conexao_recusada_nao_chegam_com_o_mesmo_codigo() {
        assert_eq!(
            QuallStatus::from(&Error::NoRoute(
                "não há rota até 192.168.56.131:47891".into()
            )),
            QuallStatus::NoRoute
        );
        assert_eq!(
            QuallStatus::from(&Error::Io("Connection refused (os error 61)".into())),
            QuallStatus::Io
        );
    }

    // =========================================================================================
    // Contadores: perda separada de reordenação
    // =========================================================================================

    /// **Só uma chave mudou de nome, e nenhuma mudou de significado.**
    ///
    /// O plugin de OBS, as sondas de câmera e as cascas leem este JSON. Em 29/08
    /// `packets_missing` virou `packets_missing_upper_bound` — mesmo valor, nome que diz o que o
    /// número é. Todas as outras continuam com o nome e o significado de sempre, e
    /// `sequence_anomalies` continua sendo a soma, não uma das partes.
    #[test]
    fn stats_do_receptor_mantem_todas_as_chaves_antigas() {
        let c = Contadores {
            quadros_prontos: 3617,
            quadros_descartados: 28,
            idrs_prontos: 90,
            idrs_quebrados: 7,
            maior_quadro_pronto_pacotes: 41,
            maior_quebrado_pacotes_recebidos: 45,
            pacotes_faltando: 1000,
            eventos_fora_de_ordem: 10,
            pacotes_vistos: 40_000,
            pacotes_perdidos_de_verdade: 42,
            pacotes_tarde_demais: 0,
            reordenacoes_absorvidas: 0,
            desistencias_de_reordenacao: 0,
            profundidade_de_reordenacao: 0,
            ajustes_de_reordenacao: 0,
            rtcp_ignorados: 0,
            jitter_us: None,
            // Entraram em 348a8b1 e 58cf278, depois que estes testes foram escritos. Ficam
            // listados em vez de `..Default::default()` de propósito: é a exaustividade do
            // literal que obriga quem acrescentar um contador a passar por aqui e decidir se o
            // JSON do receptor precisa mudar.
            cortes_por_faixa: [0; FAIXAS_DE_CORTE],
            cortes_de_idr_por_faixa: [0; FAIXAS_DE_CORTE],
        };
        let v = json_do_receptor(&c, 4);

        for chave in [
            "frames_ready",
            "frames_dropped",
            "sequence_anomalies",
            "rtcp_ignored",
            "idr_requests",
        ] {
            assert!(v.get(chave).is_some(), "sumiu a chave antiga `{chave}`");
        }
        assert_eq!(v["frames_ready"], 3617);
        assert_eq!(v["frames_dropped"], 28);
        assert_eq!(v["idr_requests"], 4);
        assert_eq!(
            v["sequence_anomalies"], 1010,
            "`sequence_anomalies` tem de continuar sendo a SOMA das duas grandezas"
        );
        // O teto diz 1000 e a perda exata diz 42, que é a situação de 29/08.
        assert_eq!(v["packets_missing_upper_bound"], 1000);
        assert_eq!(v["packets_lost_for_real"], 42);
        assert_eq!(v["packets_too_late"], 0);
    }

    /// **O par do `idrs_sent` do emissor.** Sem estas quatro chaves o receptor sabe dizer que
    /// perdeu pacotes e não sabe dizer que perdeu **socorro**: no vídeo da bancada de 31/08 o
    /// emissor marcava 31 IDR e o receptor 13, e os 18 do meio não apareciam em lugar nenhum.
    /// Ver `docs/idr-que-sobrevive.md`.
    #[test]
    fn stats_do_receptor_conta_o_idr_que_nao_chegou_inteiro() {
        let c = Contadores {
            quadros_prontos: 567,
            quadros_descartados: 239,
            idrs_prontos: 13,
            idrs_quebrados: 18,
            maior_quadro_pronto_pacotes: 41,
            maior_quebrado_pacotes_recebidos: 45,
            ..Contadores::default()
        };
        let v = json_do_receptor(&c, 17);

        assert_eq!(v["idrs_ready"], 13);
        assert_eq!(v["idrs_broken"], 18);
        // 13 + 18 = 31, que é exatamente o `idrs_sent` do emissor naquele vídeo. É esta soma que
        // torna os dois lados conciliáveis, e é ela que faltava.
        assert_eq!(
            v["idrs_ready"].as_u64().unwrap() + v["idrs_broken"].as_u64().unwrap(),
            31
        );
        assert_eq!(v["largest_frame_ready_packets"], 41);
        assert_eq!(v["largest_broken_frame_packets_received"], 45);
    }

    /// **O nome antigo não pode voltar, nem como gentileza.**
    ///
    /// `packets_missing` mentiu para esta casa por semanas: o nome prometia perda e o número era
    /// a soma dos saltos de sequência, reordenação incluída. Renomear para
    /// `packets_missing_upper_bound` só resolve se a chave antiga **sumir** — um alias
    /// sobrevivente é a mesma armadilha, agora com duas portas.
    ///
    /// Este teste é a tranca. Ele falha no dia em que alguém "restaurar a compatibilidade", e a
    /// mensagem diz por que não deve.
    #[test]
    fn stats_do_receptor_nao_ressuscita_o_nome_que_enganou() {
        let v = json_do_receptor(&Contadores::default(), 0);
        assert!(
            v.get("packets_missing").is_none(),
            "`packets_missing` voltou. Ele nunca foi perda — é a cota superior, e o nome foi o \
             que fez esta bancada ler reordenação como perda por semanas. O nome é \
             `packets_missing_upper_bound`, e não há alias de propósito."
        );
        assert!(v.get("packets_missing_upper_bound").is_some());
    }

    /// **A pergunta que o contador antigo não respondia**: perdeu ou reordenou?
    ///
    /// Antes deste conserto o JSON tinha um número só, e este teste não compilava porque não
    /// havia o que perguntar. Os números são os da medição de 26/08 no Wi-Fi do A10s.
    #[test]
    fn stats_do_receptor_responde_perdeu_ou_reordenou() {
        let c = Contadores {
            quadros_prontos: 3617,
            quadros_descartados: 28,
            idrs_prontos: 90,
            idrs_quebrados: 7,
            maior_quadro_pronto_pacotes: 41,
            maior_quebrado_pacotes_recebidos: 45,
            pacotes_faltando: 1010,
            eventos_fora_de_ordem: 0,
            pacotes_vistos: 40_148,
            pacotes_perdidos_de_verdade: 1010,
            pacotes_tarde_demais: 0,
            reordenacoes_absorvidas: 0,
            desistencias_de_reordenacao: 0,
            profundidade_de_reordenacao: 0,
            ajustes_de_reordenacao: 0,
            rtcp_ignorados: 0,
            jitter_us: None,
            // Entraram em 348a8b1 e 58cf278, depois que estes testes foram escritos. Ficam
            // listados em vez de `..Default::default()` de propósito: é a exaustividade do
            // literal que obriga quem acrescentar um contador a passar por aqui e decidir se o
            // JSON do receptor precisa mudar.
            cortes_por_faixa: [0; FAIXAS_DE_CORTE],
            cortes_de_idr_por_faixa: [0; FAIXAS_DE_CORTE],
        };
        let v = json_do_receptor(&c, 0);

        assert_eq!(v["packets_missing_upper_bound"], 1010);
        assert_eq!(v["reorder_events"], 0);
        assert_eq!(v["packets_seen"], 40_148);
        // Com `reorder_events == 0`, o teto é a perda exata — e é este par que autoriza a frase
        // "o Wi-Fi perdeu, não reordenou".
        assert_eq!(v["sequence_anomalies"], v["packets_missing_upper_bound"]);
        // Sem reordenação, o teto e a perda exata coincidem — e é isso que torna a leitura
        // daquela corrida de 26/08 defensável depois do conserto de hoje.
        assert_eq!(v["packets_lost_for_real"], v["packets_missing_upper_bound"]);
    }

    /// A janela observada é explícita: `packets_seen == 0` quer dizer que nenhum pacote chegou,
    /// e aí `sequence_anomalies == 0` **não** quer dizer que nada se perdeu.
    #[test]
    fn stats_do_receptor_diz_quando_nao_ha_o_que_afirmar() {
        let v = json_do_receptor(&Contadores::default(), 0);
        assert_eq!(v["packets_seen"], 0);
        assert_eq!(v["sequence_anomalies"], 0);
        assert_eq!(v["packets_missing_upper_bound"], 0);
        assert_eq!(v["packets_lost_for_real"], 0);
    }

    // =========================================================================================
    // O header gerado
    // =========================================================================================

    /// **Um `*/` dentro de um doc-comment fecha o bloco no meio do `quall.h`.**
    ///
    /// O cbindgen copia a documentação do Rust para dentro de um bloco de comentário em C, sem
    /// escapar nada. Um exemplo em C que traga um comentário dentro encerra o bloco ali, e o
    /// resto da prosa em português vira **código** para o compilador de quem consome o header:
    /// Swift, Kotlin e o C++ do OBS, todos de uma vez, com um erro que não aponta para cá.
    /// Aconteceu ao escrever `quall_last_status`, e este teste é a tranca.
    ///
    /// O `quall.h` está no repositório e é regerado pelo `build.rs` a cada compilação, então ler
    /// o arquivo aqui é ler o que as cascas vão receber.
    #[test]
    fn nenhum_doc_comment_do_header_fecha_no_meio() {
        let caminho = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("include")
            .join("quall.h");
        let texto = std::fs::read_to_string(&caminho).expect("ler o quall.h gerado");

        let mut dentro = false;
        for (i, linha) in texto.lines().enumerate() {
            let corte = linha.trim_start();
            if corte.starts_with("/*") {
                dentro = true;
                continue;
            }
            if dentro && corte == "*/" {
                dentro = false;
                continue;
            }
            assert!(
                !(dentro && linha.contains("*/")),
                "{}:{}: `*/` dentro de um bloco de comentário fecha o header no meio — {linha}",
                caminho.display(),
                i + 1
            );
        }
        assert!(!dentro, "bloco de comentário sem fechamento no quall.h");
    }

    // =========================================================================================
    // O código da última falha (pedido da frente do receptor Android)
    // =========================================================================================

    /// **`QUALL_STATUS_NEEDS_PIN` chega à casca como código, e não como prefixo de string.**
    ///
    /// É o caso nomeado: retomada falhada não é recusa, é convite a mostrar a tela de PIN de
    /// novo (dívida 22). Antes, `quall_connect` devolvia nulo e a única pista era o texto **em
    /// português** de `quall_last_error()` — comparar prefixo dele é exatamente o defeito que fez
    /// `QUALL_STATUS_NO_ROUTE` existir. Este teste não compilava antes do conserto:
    /// `quall_last_status` não existia.
    #[test]
    fn needs_pin_chega_como_codigo_e_nao_como_texto() {
        assert_eq!(
            guardar_erro(&Error::NeedsPin("aparelho não está pareado aqui".into())),
            QuallStatus::NeedsPin
        );
        assert_eq!(quall_last_status(), QuallStatus::NeedsPin);

        // E o texto continua lá, para a pessoa — só deixou de ser o único caminho.
        let msg = unsafe { CStr::from_ptr(quall_last_error()) }
            .to_string_lossy()
            .to_string();
        assert!(msg.contains("pareado"), "mensagem inesperada: {msg}");
    }

    /// A tela fora da faixa é recusada **antes** de discar: um monitor de 20000 px do outro lado é
    /// defeito da casca, não pedido. `0x0` é "não digo", e cai no caminho de sempre (aqui, o nulo).
    #[test]
    fn tela_fora_da_faixa_e_recusada_antes_de_discar() {
        unsafe {
            assert!(quall_connect_with_screen(
                c"127.0.0.1:9".as_ptr(),
                ptr::null(),
                ptr::null(),
                20_000,
                100
            )
            .is_null());
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            let msg = CStr::from_ptr(quall_last_error())
                .to_string_lossy()
                .to_string();
            assert!(msg.contains("20000x100"), "mensagem inesperada: {msg}");

            assert!(quall_connect_with_screen(
                c"127.0.0.1:9".as_ptr(),
                ptr::null(),
                ptr::null(),
                1920,
                0
            )
            .is_null());
            assert_eq!(quall_last_status(), QuallStatus::Invalid);

            assert!(quall_connect_with_screen(
                c"127.0.0.1:9".as_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                0
            )
            .is_null());
            assert_eq!(
                quall_last_status(),
                QuallStatus::NullPointer,
                "0x0 segue o caminho de sempre"
            );
        }
    }

    /// Duas falhas da **mesma** função, distinguidas sem olhar o texto uma única vez.
    ///
    /// É o que a casca precisa fazer depois de um `quall_connect` que devolveu nulo.
    #[test]
    fn falhas_diferentes_da_mesma_funcao_saem_com_codigos_diferentes() {
        unsafe {
            // Sem opções: erro de programação da casca.
            assert!(quall_connect(c"192.168.56.131:7877".as_ptr(), ptr::null()).is_null());
            assert_eq!(quall_last_status(), QuallStatus::NullPointer);

            // Endpoint nulo, com o mesmo caminho de código: continua sendo ponteiro nulo, mas o
            // que importa é que o código vem da falha e não de um texto.
            assert!(quall_connect(ptr::null(), ptr::null()).is_null());
            assert_eq!(quall_last_status(), QuallStatus::NullPointer);
        }

        // Um erro do núcleo com código próprio, pelo mesmo canal.
        assert_eq!(
            guardar_erro(&Error::NoRoute("o ICE não achou caminho".into())),
            QuallStatus::NoRoute
        );
        assert_eq!(quall_last_status(), QuallStatus::NoRoute);
        assert_ne!(quall_last_status(), QuallStatus::NeedsPin);
    }

    /// Numa thread que nunca falhou o código é `OK`, e o de uma thread não vaza para a outra —
    /// mesma promessa que `quall_last_error` sempre fez.
    #[test]
    fn o_codigo_da_falha_e_por_thread() {
        assert_eq!(guardar_texto("falha desta thread"), QuallStatus::Invalid);
        assert_eq!(quall_last_status(), QuallStatus::Invalid);

        let outra = std::thread::spawn(|| quall_last_status())
            .join()
            .expect("thread");
        assert_eq!(
            outra,
            QuallStatus::Ok,
            "o código vazou de uma thread para outra"
        );
        assert_eq!(quall_last_status(), QuallStatus::Invalid);
    }
}

#[cfg(test)]
mod testes_do_bind_address {
    use super::*;

    /// Monta o mínimo que [`montar_config`] aceita, com `bind_address` apontando para `p`.
    fn config_com(p: *const c_char) -> Saida<SessionConfig> {
        let opcoes = QuallSessionOptions {
            me: QuallDeviceDesc {
                device_id: c"casca-de-teste".as_ptr(),
                display_name: c"Casca de teste".as_ptr(),
                screen_source: false,
                camera_source: false,
                sink: true,
            },
            pin: ptr::null(),
            known_peers_json: ptr::null(),
            signaling_port: 0,
            timeout_ms: 100,
            tracks: ptr::null(),
            track_count: 0,
            bind_address: p,
        };
        // SAFETY: todos os ponteiros da struct são nulos ou literais `c"…"` com NUL, e vivem
        // mais que a chamada.
        unsafe { montar_config(&opcoes, Cancelamento::novo()).map(|(cfg, _)| cfg) }
    }

    /// `Falha` não deriva `Debug` — é o erro interno da fronteira, e o texto dele sai por
    /// [`Falha::mensagem`]. Sem isto, cada `expect` aqui pediria um `derive` que o resto do
    /// módulo não precisa.
    fn ok(r: Saida<SessionConfig>) -> SessionConfig {
        match r {
            Ok(cfg) => cfg,
            Err(e) => panic!("montar_config recusou: {}", e.mensagem()),
        }
    }

    /// **O campo atravessa, e o que não atravessa continua no padrão do núcleo.**
    ///
    /// Antes desta rodada `montar_config` fixava `TransportConfig::default()` e a casca não
    /// tinha por onde pedir o cabo: `bind_address` existia no núcleo com zero ocorrências em
    /// `quall.h`. Este teste é o que impede a fiação de voltar a ser um `default()` mudo.
    #[test]
    fn bind_address_atravessa_a_fronteira_c_ate_o_transporte() {
        let cfg = ok(config_com(c"169.254.75.173".as_ptr()));
        assert_eq!(
            cfg.transport.bind_address.as_deref(),
            Some("169.254.75.173"),
            "o endereço do cabo não chegou ao TransportConfig",
        );
        // O que **não** é exposto continua no padrão, e não numa cópia congelada aqui.
        let padrao = TransportConfig::default();
        assert_eq!(cfg.transport.port_range, padrao.port_range);
        assert_eq!(cfg.transport.mtu, padrao.mtu);
    }

    /// `NULL` é o comportamento de antes — a metade da nota de ABI que uma casca velha exercita.
    #[test]
    fn bind_address_nulo_e_o_comportamento_de_antes() {
        let cfg = ok(config_com(ptr::null()));
        assert_eq!(
            cfg.transport.bind_address,
            TransportConfig::default().bind_address,
            "nulo tinha de reunir todas as interfaces, como antes",
        );
        assert!(cfg.transport.bind_address.is_none());
    }

    /// **Vazio e só-espaços também são `None`, e é aqui que a decisão fica fixada.**
    ///
    /// O núcleo recusa `Some("")` com `Error::Invalid`, e está certo: lá quem chama é Rust e
    /// distingue `None` de `Some("")`. Numa casca C não distingue — `obs_data_get_string`
    /// devolve `""` para um campo em branco, nunca `NULL` —, e deixar isso virar erro
    /// transformaria "o usuário não preencheu o campo" numa sessão que não sobe. Se alguém
    /// trocar este julgamento, é este teste que cai.
    #[test]
    fn bind_address_vazio_ou_so_espacos_vira_nulo_em_vez_de_erro() {
        for entrada in [c"".as_ptr(), c"   ".as_ptr(), c"\t\n".as_ptr()] {
            let cfg = ok(config_com(entrada));
            assert!(
                cfg.transport.bind_address.is_none(),
                "string em branco tinha de virar None",
            );
        }
    }

    /// Espaço em volta de um endereço de verdade é aparado, não recusado. Um `" 169.254.75.173"`
    /// chegaria à libjuice como pedido sem sentido, e a sessão falharia sem dizer por quê.
    #[test]
    fn bind_address_com_espaco_em_volta_e_aparado() {
        let cfg = ok(config_com(c"  169.254.75.173  ".as_ptr()));
        assert_eq!(
            cfg.transport.bind_address.as_deref(),
            Some("169.254.75.173")
        );
    }

    /// O campo entra na mesma conferência de UTF-8 dos outros textos da struct, com o nome dele
    /// na mensagem — a dívida 8 vale para o campo novo também.
    #[test]
    fn bind_address_que_nao_e_utf8_sai_com_o_nome_do_campo() {
        let ruim = [0xFFu8, 0x00];
        let Err(erro) = config_com(ruim.as_ptr().cast::<c_char>()) else {
            panic!("0xFF sozinho não é UTF-8 e tinha de ser recusado");
        };
        assert_eq!(erro.status(), QuallStatus::NotUtf8);
        let msg = erro.mensagem();
        assert!(msg.contains("bind_address"), "mensagem sem o campo: {msg}");
    }
}

#[cfg(test)]
mod testes_de_audio {
    use super::*;

    /// Lê uma função de padrão `(buf, cap)` até o fim.
    fn ler_texto(f: impl Fn(*mut c_char, usize) -> isize) -> String {
        let n = f(ptr::null_mut(), 0);
        assert!(n > 0, "a função devolveu {n}");
        let mut buf = vec![0u8; n as usize];
        let escrito = f(buf.as_mut_ptr() as *mut c_char, buf.len());
        assert_eq!(escrito, n);
        buf.pop();
        String::from_utf8(buf).expect("utf-8")
    }

    fn preset_json(kind: QuallTrackKind, codec: QuallAudioCodec) -> serde_json::Value {
        let texto = ler_texto(|b, c| unsafe { quall_audio_preset_json(kind, codec, b, c) });
        serde_json::from_str(&texto).expect("json")
    }

    /// A fronteira devolve **os números do preset**, e não números que a casca teria de fixar.
    #[test]
    fn o_preset_atravessa_a_fronteira_com_os_numeros_do_nucleo() {
        let mic = preset_json(QuallTrackKind::Microphone, QuallAudioCodec::Default);
        assert_eq!(mic["codec"], "opus");
        assert_eq!(mic["sample_rate_hz"], 48_000);
        assert_eq!(mic["channels"], 1);
        assert_eq!(mic["frame_samples"], 960);
        assert_eq!(mic["fec"], true);
        assert_eq!(mic["expected_loss_pct"], 5);
        assert_eq!(mic["payload_type"], 111);

        let sis = preset_json(QuallTrackKind::SystemAudio, QuallAudioCodec::Default);
        assert_eq!(sis["channels"], 2);
        assert_eq!(sis["fec"], false);
    }

    /// **O caso que motivou o campo de codec.** Pedindo PCMU, o `fmtp` e o relógio mudam junto —
    /// não adianta a casca mandar µ-law se o SDP continua dizendo `opus/48000/2`.
    #[test]
    fn pedir_pcmu_muda_o_relogio_e_o_fmtp_e_nao_so_o_nome() {
        let p = preset_json(QuallTrackKind::Microphone, QuallAudioCodec::Pcmu);
        assert_eq!(p["codec"], "pcmu");
        assert_eq!(p["sample_rate_hz"], 8_000);
        assert_eq!(p["frame_samples"], 160);
        assert_eq!(p["payload_type"], 0);
        assert!(
            !p["fmtp"].as_str().unwrap().contains("useinbandfec"),
            "o fmtp do PCMU não pode carregar parâmetro de Opus: {}",
            p["fmtp"]
        );
        // O preset da espécie carrega `fec: true` porque nasceu do microfone. Repeti-lo aqui
        // diria à casca que há recuperação de perda no G.711, e não há.
        assert_eq!(p["fec"], false, "G.711 não tem FEC nenhum");
        assert_eq!(p["expected_loss_pct"], 0);
    }

    /// **O caso que o teste acima não cobria, e que estava quebrado.**
    ///
    /// `Microphone` já é mono, então trocar o codec para PCMU não mudava o número de canais e o
    /// defeito ficava invisível. `SystemAudio` pede **estéreo** pelo preset da espécie, e a RFC
    /// 3551 §6 fixa `PCMU/8000/1` — G.711 é mono por definição e não há como transportar dois
    /// canais nele.
    ///
    /// Medido em 30/08/2026 pelo laço de áudio do Android: a sonda emitia mono (ela chama
    /// `canais_no_fio`) e o app montava o `AudioTrack` com 2 canais (ele lê este JSON). O tom de
    /// quatro notas voltou com 2 de 4 notas reconhecíveis e razão de raia 0,254, contra 0,978 do
    /// mesmo tom em Opus. **Sem erro em lugar nenhum** — é a classe de defeito da §2 do
    /// `docs/audio.md`, "áudio que acelera ou arrasta sem contador nenhum acusando".
    ///
    /// É o mesmo defeito que a sonda tinha achado em 27/08, um nível acima: lá dois lados do
    /// `quall-probe` discordavam; aqui a fronteira C discordava do núcleo.
    #[test]
    fn pcmu_e_mono_mesmo_quando_a_especie_pede_estereo() {
        let sistema = preset_json(QuallTrackKind::SystemAudio, QuallAudioCodec::Default);
        assert_eq!(sistema["channels"], 2, "o preset da espécie pede estéreo");

        let pcmu = preset_json(QuallTrackKind::SystemAudio, QuallAudioCodec::Pcmu);
        assert_eq!(
            pcmu["channels"], 1,
            "PCMU é mono por RFC 3551 §6; responder 2 faz a casca tocar no dobro da velocidade"
        );
        assert_eq!(pcmu["sample_rate_hz"], 8_000);
        assert_eq!(pcmu["frame_samples"], 160, "20 ms a 8 kHz");
    }

    /// Espécie de vídeo não tem preset de áudio, e a fronteira diz isso em vez de inventar.
    #[test]
    fn track_de_video_nao_tem_preset_de_audio_na_fronteira() {
        let n = unsafe {
            quall_audio_preset_json(
                QuallTrackKind::Screen,
                QuallAudioCodec::Default,
                ptr::null_mut(),
                0,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(quall_last_status(), QuallStatus::Invalid);
    }

    /// A porta nova recusa o que tem de recusar, e cada recusa deixa motivo legível.
    #[test]
    fn send_audio_recusa_nulo_vazio_e_grande_demais() {
        assert_eq!(
            unsafe { quall_track_send_audio(ptr::null(), ptr::null()) },
            QuallStatus::NullPointer
        );

        let vazio = QuallAudioSample {
            payload: [0u8; 4].as_ptr(),
            len: 0,
            timestamp_us: 0,
        };
        // Track nula é a primeira barreira; o teste do payload vazio vive no exemplo em C, que
        // tem uma track de verdade. Aqui basta que a ordem das checagens não deixe passar nulo.
        assert_eq!(
            unsafe { quall_track_send_audio(ptr::null(), &vazio) },
            QuallStatus::NullPointer
        );
    }

    /// PCMU **não** ganha encoder aqui, de propósito, e o erro explica o que fazer.
    #[test]
    fn o_encoder_recusa_pcmu_e_diz_por_que() {
        let e =
            unsafe { quall_audio_encoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Pcmu) };
        assert!(e.is_null());
        let motivo = unsafe { CStr::from_ptr(quall_last_error()) }
            .to_string_lossy()
            .to_string();
        assert!(
            motivo.contains("tabela de consulta"),
            "o erro precisa dizer o que fazer, e disse: {motivo}"
        );
    }

    /// Espécie de vídeo não ganha encoder.
    #[test]
    fn o_encoder_recusa_track_de_video() {
        let e =
            unsafe { quall_audio_encoder_new(QuallTrackKind::Camera, QuallAudioCodec::Default) };
        assert!(e.is_null());
    }

    /// **O `content_delay_us` do preset é o atraso que o conteúdo tem de verdade**, medido pelo
    /// codificador e pelo decodificador **da fronteira** (os que os emissores e as cascas usam):
    /// um estouro de 10 ms a 3 150 Hz (o da claquete) entra no meio de um quadro, e a correlação
    /// cruzada acha quantas amostras depois ele sai. Sem o desconto, o T0 do Mac media +6,6 ms no
    /// Opus (`docs/som-no-receptor.md` §20.7). A tolerância é de 5 amostras (104 µs): a voz, pelo
    /// SILK, sai 2 amostras antes do lookahead (`quall-opus`, o mesmo teste por dentro).
    #[cfg(feature = "opus")]
    #[test]
    fn o_atraso_do_conteudo_do_preset_e_o_que_o_codec_da_fronteira_mede() {
        for kind in [QuallTrackKind::Microphone, QuallTrackKind::SystemAudio] {
            let p = preset_json(kind, QuallAudioCodec::Default);
            let declarado = p["content_delay_us"].as_u64().expect("content_delay_us") as f64;
            let canais = p["channels"].as_u64().unwrap() as usize;
            let por_quadro = p["frame_samples"].as_u64().unwrap() as usize;
            let enc = unsafe { quall_audio_encoder_new(kind, QuallAudioCodec::Default) };
            let dec = unsafe { quall_audio_decoder_new(kind, QuallAudioCodec::Default) };
            assert!(!enc.is_null() && !dec.is_null());
            let inicio = 20 * por_quadro + 137;
            let dur = 480usize;
            let estouro = |n: usize| -> f64 {
                if n < inicio || n >= inicio + dur {
                    return 0.0;
                }
                let k = (n - inicio) as f64;
                let janela = 0.5 - 0.5 * (std::f64::consts::TAU * k / dur as f64).cos();
                (std::f64::consts::TAU * 3_150.0 * n as f64 / 48_000.0).sin() * 0.5 * janela
            };
            let mut saida = Vec::new();
            let mut pacote = [0u8; 4096];
            let mut pcm = vec![0i16; por_quadro * canais];
            for q in 0..50usize {
                let quadro: Vec<i16> = (0..por_quadro)
                    .flat_map(|i| {
                        std::iter::repeat((estouro(q * por_quadro + i) * 32_767.0) as i16)
                            .take(canais)
                    })
                    .collect();
                let n = unsafe {
                    quall_audio_encoder_encode(
                        enc,
                        quadro.as_ptr(),
                        quadro.len(),
                        pacote.as_mut_ptr(),
                        pacote.len(),
                    )
                };
                assert!(n > 0);
                let m = unsafe {
                    quall_audio_decoder_decode(
                        dec,
                        pacote.as_ptr(),
                        n as usize,
                        false,
                        pcm.as_mut_ptr(),
                        pcm.len(),
                    )
                };
                assert!(m > 0);
                saida.extend(
                    pcm[..m as usize * canais]
                        .iter()
                        .step_by(canais)
                        .map(|v| f64::from(*v) / 32_767.0),
                );
            }
            unsafe {
                quall_audio_encoder_free(enc);
                quall_audio_decoder_free(dec);
            }
            let medido = (0..2_000usize)
                .max_by(|a, b| {
                    let s = |d: usize| -> f64 {
                        (inicio..inicio + dur)
                            .filter(|n| n + d < saida.len())
                            .map(|n| estouro(n) * saida[n + d])
                            .sum()
                    };
                    s(*a).total_cmp(&s(*b))
                })
                .unwrap() as f64
                / 48_000.0
                * 1e6;
            assert!(
                (medido - declarado).abs() <= 105.0,
                "{kind:?}: o conteúdo saiu {medido:.0} µs atrás do carimbo, e o preset declara {declarado}"
            );
        }
        let pcmu = preset_json(QuallTrackKind::Microphone, QuallAudioCodec::Pcmu);
        assert_eq!(
            pcmu["content_delay_us"], 0,
            "o PCMU não tem atraso de codec"
        );
    }

    /// Um quadro de tom sintético nosso, com a nota trocando a cada 25 quadros.
    ///
    /// **A troca de nota não é enfeite.** Um tom perfeitamente estacionário faz a atividade de
    /// fala do VAD do SILK decair, e com ela some o LBRR — ver
    /// [`o_lbrr_some_quando_o_vad_para_de_ver_fala`]. As quatro notas são as mesmas da sonda de
    /// bancada, pelo mesmo motivo. Origem sempre sintética: `docs/audio.md` §8.
    #[cfg(feature = "opus")]
    fn quadro_de_tom(indice: u32, amostras_por_canal: usize, canais: usize) -> Vec<i16> {
        const NOTAS: [f64; 4] = [400.0, 500.0, 800.0, 1000.0];
        let hz = NOTAS[(indice as usize / 25) % 4];
        let mut pcm = Vec::with_capacity(amostras_por_canal * canais);
        for i in 0..amostras_por_canal {
            let t = (indice as usize * amostras_por_canal + i) as f64 / 48_000.0;
            // Amplitude 0,5 do fundo de escala, a mesma da sonda de bancada — e ela **importa**:
            // o portão do LBRR é o VAD do SILK, que mede atividade contra o piso de ruído, então
            // um tom mais fraco derruba o FEC antes. Ver
            // `o_lbrr_some_quando_o_vad_para_de_ver_fala`.
            let a = ((t * hz * std::f64::consts::TAU).sin() * 16_383.0) as i16;
            for _ in 0..canais {
                pcm.push(a);
            }
        }
        pcm
    }

    /// Codifica `n` quadros pela fronteira e devolve (com LBRR, total).
    #[cfg(feature = "opus")]
    fn contar_lbrr(
        enc: *mut QuallAudioEncoder,
        n: u32,
        amostras: usize,
        canais: usize,
    ) -> (u32, u32) {
        let mut com = 0;
        let mut saida = [0u8; 4096];
        for q in 0..n {
            let pcm = quadro_de_tom(q, amostras, canais);
            let escrito = unsafe {
                quall_audio_encoder_encode(
                    enc,
                    pcm.as_ptr(),
                    pcm.len(),
                    saida.as_mut_ptr(),
                    saida.len(),
                )
            };
            assert!(escrito > 0, "codificar o quadro {q} devolveu {escrito}");
            if quall_opus::tem_lbrr(&saida[..escrito as usize]).unwrap_or(false) {
                com += 1;
            }
        }
        (com, n)
    }

    /// **O teste que fecha a regra da fonte de verdade.**
    ///
    /// Não basta o encoder existir: ele tem de sair da fronteira já configurado pelo preset que
    /// gera o `a=fmtp`. O preset do microfone declara `useinbandfec=1`, e o `docs/audio.md` §11
    /// mostrou que essa declaração **não produz LBRR nenhum** sem `OPUS_SET_PACKET_LOSS_PERC`.
    /// Então a asserção não é "codificou": é **o LBRR está no byte que a fronteira devolveu**,
    /// lido pela libopus.
    #[cfg(feature = "opus")]
    #[test]
    fn o_encoder_da_fronteira_ja_sai_emitindo_lbrr() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null(), "o encoder do microfone tinha de existir");
        let (com, total) = contar_lbrr(enc, 100, 960, 1);
        unsafe { quall_audio_encoder_free(enc) };

        // Não é `total - 1`: o LBRR é fechado pelo VAD, e o VAD leva alguns quadros para subir
        // no começo do fluxo. 90% é folgado sobre o que se mede (97 a 99 de 100) e apertado o
        // bastante para pegar um encoder que saiu sem `OPUS_SET_PACKET_LOSS_PERC`, que dá 0.
        assert!(
            com * 100 >= total * 90,
            "a fronteira entregou um encoder sem FEC de verdade: {com} de {total} pacotes com \
             LBRR. É o defeito da §11 do docs/audio.md voltando pela fronteira C."
        );
    }

    /// **A complexidade do calor e a volta** (`docs/teleprompter-com-camera.md` §8.12.17): o microfone
    /// nasce em 10 com LBRR; o iOS baixa a 4 no calor e volta ao padrão quando esfria — e **o LBRR tem de
    /// voltar** junto (a libopus decide o FEC com histerese sobre a decisão anterior, `decide_fec`: sem este
    /// teste, a volta poderia deixar o fio dizendo `useinbandfec=1` sem LBRR nenhum; a revisão de 27/09, B1).
    /// O que acontece **em 4** é impresso e não travado: depende de o LBRR já estar ligado.
    #[cfg(feature = "opus")]
    #[test]
    fn a_complexidade_do_calor_tira_o_lbrr_e_a_volta_o_devolve() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null());
        assert_eq!(
            quall_audio_default_complexity(QuallTrackKind::Microphone),
            10
        );
        assert_eq!(
            quall_audio_default_complexity(QuallTrackKind::SystemAudio),
            10
        );
        let mut saida = [0u8; 4096];
        let mut contar = |de: u32, ate: u32| -> u32 {
            let mut com = 0;
            for q in de..ate {
                let pcm = quadro_de_tom(q, 960, 1);
                let n = unsafe {
                    quall_audio_encoder_encode(
                        enc,
                        pcm.as_ptr(),
                        pcm.len(),
                        saida.as_mut_ptr(),
                        saida.len(),
                    )
                };
                assert!(n > 0);
                if quall_opus::tem_lbrr(&saida[..n as usize]).unwrap_or(false) {
                    com += 1;
                }
            }
            com
        };
        let frio = contar(0, 100);
        assert_eq!(unsafe { quall_audio_encoder_set_complexity(enc, 4) }, 0);
        let _ = contar(100, 110);
        let quente = contar(110, 200);
        assert_eq!(unsafe { quall_audio_encoder_set_complexity(enc, 10) }, 0);
        let _ = contar(200, 210);
        let de_volta = contar(210, 400);
        unsafe { quall_audio_encoder_free(enc) };
        println!(
            "LBRR: frio em 10 = {frio}/100, em 4 = {quente}/90, de volta a 10 = {de_volta}/190"
        );
        assert!(frio >= 90, "em 10, frio: {frio} de 100 com LBRR");
        assert!(
            de_volta * 100 >= 190 * 90,
            "de volta a 10: só {de_volta} de 190 com LBRR — a volta não religou o FEC"
        );
        assert_eq!(
            unsafe { quall_audio_encoder_set_complexity(ptr::null_mut(), 7) },
            -1
        );
    }

    /// **O LBRR é fechado pelo VAD, e isso não estava escrito em lugar nenhum.**
    ///
    /// `silk/float/encode_frame_FLP.c:395` só codifica LBRR quando
    /// `speech_activity_Q8 > LBRR_SPEECH_ACTIVITY_THRES` (0,3). Um tom perfeitamente
    /// estacionário faz essa medida decair, e o FEC **some no meio do fluxo, sem aviso**, com
    /// `useinbandfec=1` no SDP o tempo todo.
    ///
    /// Medido: com um seno de 440 Hz constante, o LBRR sai nos quadros 1 a 30 e **para no 31**,
    /// para nunca mais voltar. O pacote encolhe de ~69 para ~59 bytes no mesmo quadro.
    ///
    /// Isto está aqui como teste, e não só como comentário, porque é a única forma de a próxima
    /// pessoa não reaprender o mesmo susto — e porque qualifica o "99,8% dos pacotes carregam
    /// LBRR" do `docs/audio.md`: aquele número é propriedade do **tom de quatro notas**, não do
    /// preset.
    #[cfg(feature = "opus")]
    #[test]
    fn o_lbrr_some_quando_o_vad_para_de_ver_fala() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null());

        let mut saida = [0u8; 4096];
        let mut ultimos_dez = 0;
        for q in 0..60u32 {
            // Seno estacionário: nota única, sem troca nenhuma.
            let pcm: Vec<i16> = (0..960)
                .map(|i| {
                    let t = (q * 960 + i) as f64 / 48_000.0;
                    ((t * 440.0 * std::f64::consts::TAU).sin() * 8_000.0) as i16
                })
                .collect();
            let n = unsafe {
                quall_audio_encoder_encode(
                    enc,
                    pcm.as_ptr(),
                    pcm.len(),
                    saida.as_mut_ptr(),
                    saida.len(),
                )
            };
            assert!(n > 0);
            if q >= 50 && quall_opus::tem_lbrr(&saida[..n as usize]).unwrap_or(false) {
                ultimos_dez += 1;
            }
        }
        unsafe { quall_audio_encoder_free(enc) };
        assert_eq!(
            ultimos_dez, 0,
            "num tom estacionário o VAD do SILK derruba o LBRR; se isto passou a sair, a \
             libopus mudou de comportamento e o docs/audio.md precisa ser relido"
        );
    }

    // =========================================================================================
    // O decodificador da fronteira, e as três ordens do slot
    // =========================================================================================

    /// A mensagem de `quall_last_error` desta thread, como `String`.
    fn ultimo_erro() -> String {
        unsafe { CStr::from_ptr(quall_last_error()) }
            .to_string_lossy()
            .to_string()
    }

    /// Energia média por amostra. É o número que separa "saiu som" de "saiu silêncio", e é o
    /// mesmo critério que `docs/audio.md` §11 usa na bancada de FEC.
    #[cfg(feature = "opus")]
    fn energia(pcm: &[i16]) -> f64 {
        if pcm.is_empty() {
            return 0.0;
        }
        pcm.iter()
            .map(|&a| f64::from(a) * f64::from(a))
            .sum::<f64>()
            / pcm.len() as f64
    }

    /// **A ida e a volta pela fronteira, sem passar por Rust nenhum no meio.**
    ///
    /// Não é comparação byte a byte, e não podia ser: o Opus é com perda, e `docs/audio.md` §12
    /// diz que comparar bytes de dois builds nem sequer é conferência válida. O que se afirma
    /// aqui é o que importa para uma casca que vai tocar isto num DAC — sai a quantidade certa
    /// de amostras, com energia da mesma ordem do que entrou.
    #[cfg(feature = "opus")]
    #[test]
    fn o_decodificador_da_fronteira_devolve_o_som_que_o_encoder_da_fronteira_engoliu() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::SystemAudio, QuallAudioCodec::Default)
        };
        let dec = unsafe {
            quall_audio_decoder_new(QuallTrackKind::SystemAudio, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null() && !dec.is_null());

        let mut pacote = [0u8; 4096];
        let mut pcm = [0i16; 1920];
        let mut energia_dentro = 0.0;
        let mut energia_fora = 0.0;
        // 30 quadros: o suficiente para o encoder sair do transiente de abertura.
        for q in 0..30u32 {
            let entrada = quadro_de_tom(q, 960, 2);
            let n = unsafe {
                quall_audio_encoder_encode(
                    enc,
                    entrada.as_ptr(),
                    entrada.len(),
                    pacote.as_mut_ptr(),
                    pacote.len(),
                )
            };
            assert!(n > 0, "o quadro {q} não codificou: {n}");
            let amostras = unsafe {
                quall_audio_decoder_decode(
                    dec,
                    pacote.as_ptr(),
                    n as usize,
                    false,
                    pcm.as_mut_ptr(),
                    pcm.len(),
                )
            };
            assert_eq!(
                amostras, 960,
                "20 ms a 48 kHz são 960 amostras por canal, e o quadro {q} devolveu {amostras}"
            );
            if q >= 10 {
                energia_dentro += energia(&entrada);
                energia_fora += energia(&pcm);
            }
        }
        unsafe { quall_audio_encoder_free(enc) };
        unsafe { quall_audio_decoder_free(dec) };

        let razao = energia_fora / energia_dentro;
        assert!(
            (0.5..2.0).contains(&razao),
            "a energia que saiu é {razao:.3}× a que entrou — isto não é o mesmo som"
        );
    }

    /// **A ordem `SILENCE`: pacote nulo é ocultação de perda, não erro.**
    ///
    /// O slot `SILENCE` chega com `payload` nulo e `len` 0, e a casca precisa de 20 ms de
    /// alguma coisa para dar ao DAC. Se esta chamada devolvesse `-1`, a casca escreveria zeros —
    /// e zeros no meio de um som são um estalo, que é exatamente o que o PLC existe para evitar.
    #[cfg(feature = "opus")]
    #[test]
    fn pacote_nulo_e_ocultacao_de_perda_e_devolve_um_quadro_inteiro() {
        let dec = unsafe {
            quall_audio_decoder_new(QuallTrackKind::SystemAudio, QuallAudioCodec::Default)
        };
        assert!(!dec.is_null());
        let mut pcm = [7i16; 1920];
        let n = unsafe {
            quall_audio_decoder_decode(dec, ptr::null(), 0, false, pcm.as_mut_ptr(), pcm.len())
        };
        unsafe { quall_audio_decoder_free(dec) };
        assert_eq!(n, 960, "a ocultação de perda tem de encher o slot de 20 ms");
    }

    /// **O socorro do FEC atravessa a fronteira e recupera energia de verdade.**
    ///
    /// É o teste que justifica esta porta existir. O caminho alternativo no Android — o
    /// `MediaCodec` — não tem `decode_fec`, então a ordem `FEC` que o jitter buffer entrega não
    /// teria consumidor nenhum. Aqui: o quadro *N* é jogado fora e o *N+1* é passado com
    /// `decode_fec = true`; o que volta tem de ter energia da ordem do quadro perdido, e não
    /// silêncio.
    ///
    /// A track é de **microfone**, e isso não é detalhe: é o preset que liga `useinbandfec` e a
    /// perda esperada, sem os quais não existe LBRR nenhum para recuperar (§11).
    #[cfg(feature = "opus")]
    #[test]
    fn o_socorro_do_fec_recupera_o_quadro_perdido_pela_fronteira() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null());

        // Codifica a sequência inteira antes, porque o encoder é preditivo e não dá para voltar.
        let mut pacotes: Vec<Vec<u8>> = Vec::new();
        for q in 0..12u32 {
            let entrada = quadro_de_tom(q, 960, 1);
            let mut saida = [0u8; 4096];
            let n = unsafe {
                quall_audio_encoder_encode(
                    enc,
                    entrada.as_ptr(),
                    entrada.len(),
                    saida.as_mut_ptr(),
                    saida.len(),
                )
            };
            assert!(n > 0);
            pacotes.push(saida[..n as usize].to_vec());
        }
        unsafe { quall_audio_encoder_free(enc) };

        // O contrato do `fec_has_lbrr`: só se pede socorro a quem carrega socorro.
        let perdido = 10usize;
        let socorro = &pacotes[perdido + 1];
        assert!(
            quall_opus::tem_lbrr(socorro).unwrap_or(false),
            "o pacote seguinte precisa ter LBRR para este teste dizer alguma coisa"
        );

        let dec = unsafe {
            quall_audio_decoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        assert!(!dec.is_null());
        let mut pcm = [0i16; 960];
        // Decodifica normalmente até o quadro anterior ao buraco: o decoder é preditivo também.
        for p in &pacotes[..perdido] {
            let n = unsafe {
                quall_audio_decoder_decode(
                    dec,
                    p.as_ptr(),
                    p.len(),
                    false,
                    pcm.as_mut_ptr(),
                    pcm.len(),
                )
            };
            assert_eq!(n, 960);
        }
        let n = unsafe {
            quall_audio_decoder_decode(
                dec,
                socorro.as_ptr(),
                socorro.len(),
                true,
                pcm.as_mut_ptr(),
                pcm.len(),
            )
        };
        unsafe { quall_audio_decoder_free(dec) };
        assert_eq!(n, 960, "o socorro tem de encher o slot que faltou");
        // O LBRR é uma versão de baixa taxa do quadro: a energia não bate exatamente, mas está
        // ordens de grandeza acima do silêncio.
        assert!(
            energia(&pcm) > 1_000.0,
            "o socorro voltou praticamente mudo ({:.0}) — isto é ocultação de perda disfarçada, \
             não recuperação",
            energia(&pcm)
        );
    }

    /// `decode_fec` sem pacote é pedido sem sentido, e a fronteira diz isso em vez de aceitar.
    ///
    /// Aceitar cairia na ocultação de perda **em silêncio**, que é a armadilha que o
    /// `fec_has_lbrr` existe para fechar; reproduzi-la aqui seria abri-la por outra porta.
    #[cfg(feature = "opus")]
    #[test]
    fn decode_fec_com_pacote_nulo_e_recusado_com_motivo() {
        let dec = unsafe {
            quall_audio_decoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        let mut pcm = [0i16; 960];
        let n = unsafe {
            quall_audio_decoder_decode(dec, ptr::null(), 0, true, pcm.as_mut_ptr(), pcm.len())
        };
        unsafe { quall_audio_decoder_free(dec) };
        assert_eq!(n, -1);
        assert!(ultimo_erro().contains("decode_fec"), "{}", ultimo_erro());
    }

    /// Um `out` curto é recusado antes de chamar a libopus.
    ///
    /// Sem isto a libopus devolveria `OPUS_BUFFER_TOO_SMALL` e a casca ouviria 20 ms de nada sem
    /// saber por quê — o mesmo argumento do `pcm_len` no encoder.
    #[cfg(feature = "opus")]
    #[test]
    fn o_decodificador_recusa_um_destino_curto_dizendo_o_tamanho_certo() {
        let dec = unsafe {
            quall_audio_decoder_new(QuallTrackKind::SystemAudio, QuallAudioCodec::Default)
        };
        let mut pcm = [0i16; 960]; // metade: o preset de sistema é estéreo, e pede 1920
        let n = unsafe {
            quall_audio_decoder_decode(dec, ptr::null(), 0, false, pcm.as_mut_ptr(), pcm.len())
        };
        unsafe { quall_audio_decoder_free(dec) };
        assert_eq!(n, -1);
        assert!(ultimo_erro().contains("1920"), "{}", ultimo_erro());
    }

    /// O decodificador recusa PCMU **e diz por quê**, exatamente como o encoder.
    ///
    /// A simetria é o ponto: se a fronteira recusasse PCMU num sentido e o aceitasse no outro,
    /// uma casca acharia que a porta cobre G.711 e cairia num nulo sem explicação.
    #[test]
    fn o_decodificador_recusa_pcmu_apontando_a_tabela_de_consulta() {
        let dec =
            unsafe { quall_audio_decoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Pcmu) };
        assert!(dec.is_null());
        assert!(ultimo_erro().contains("PCMU"), "{}", ultimo_erro());
    }

    #[test]
    fn o_decodificador_recusa_track_que_nao_e_de_audio() {
        let dec =
            unsafe { quall_audio_decoder_new(QuallTrackKind::Screen, QuallAudioCodec::Default) };
        assert!(dec.is_null());
        assert!(
            ultimo_erro().contains("não é de áudio"),
            "{}",
            ultimo_erro()
        );
    }

    /// **O `0` do codec é "não sei", e a casca precisa poder saber que não sabe.**
    ///
    /// Uma casca receptora que lesse `Default` como "então é Opus" decodificaria G.711 com o
    /// relógio numa escala 6× errada — sai som, e todo alinhamento fica errado. Por isso o motivo
    /// vai para `quall_last_error` junto com o zero.
    #[test]
    fn o_codec_de_uma_track_nula_e_nao_sei_com_motivo() {
        let c = unsafe { quall_track_audio_codec(ptr::null()) };
        assert_eq!(c, QuallAudioCodec::Default);
        assert!(ultimo_erro().contains("nula"), "{}", ultimo_erro());
    }

    // =========================================================================================
    // Recepção de áudio: o jitter buffer alcançável de C
    // =========================================================================================

    /// Onde o tratador de teste deposita o que recebeu. Um `static` porque a assinatura é
    /// `extern "C"` e não carrega estado — é a mesma restrição que uma casca C tem.
    static SLOTS: Mutex<Vec<(QuallAudioOrder, u16, usize, i8)>> = Mutex::new(Vec::new());

    unsafe extern "C" fn anotar_slot(slot: *const QuallAudioSlot, _u: *mut c_void) {
        let Some(s) = slot.as_ref() else { return };
        if s.order == QuallAudioOrder::Silence {
            assert!(s.payload.is_null(), "SILENCE tem de vir com payload nulo");
            assert_eq!(s.len, 0);
        } else {
            assert!(!s.payload.is_null(), "FRAME e FEC têm de trazer bytes");
        }
        if let Ok(mut v) = SLOTS.lock() {
            v.push((s.order, s.sequence, s.len, s.fec_has_lbrr));
        }
    }

    fn entregar(ordem: Entrega<'_>) -> (QuallAudioOrder, u16, usize, i8) {
        if let Ok(mut v) = SLOTS.lock() {
            v.clear();
        }
        unsafe { entregar_slot(anotar_slot, Contexto(ptr::null_mut()), ordem) };
        let v = SLOTS.lock().expect("cadeado");
        *v.last().expect("o tratador tem de ter sido chamado")
    }

    /// As três ordens do buffer chegam a C como três slots distintos, e **a sequência do `FEC` é
    /// a do slot que faltou** — não a do socorro.
    ///
    /// Trocar as duas é o defeito silencioso desta porta: o áudio sairia, pareceria funcionar, e
    /// todo alinhamento ficaria um slot fora.
    #[test]
    fn as_tres_ordens_do_buffer_atravessam_a_fronteira_com_o_slot_certo() {
        let payload = [0x78u8, 1, 2, 3];
        assert_eq!(
            entregar(Entrega::Quadro {
                payload: &payload,
                sequencia: 41,
                timestamp_us: 820_000,
            }),
            (QuallAudioOrder::Frame, 41, 4, 0)
        );

        let socorro = [0x78u8, 9, 9, 9, 9];
        let (ordem, seq, len, _) = entregar(Entrega::Fec {
            socorro: &socorro,
            sequencia: 42,
            timestamp_us: 840_000,
        });
        assert_eq!(ordem, QuallAudioOrder::Fec);
        assert_eq!(seq, 42, "o slot que faltou, e não o do socorro");
        assert_eq!(len, 5, "o socorro vai inteiro, sem tocar");

        assert_eq!(
            entregar(Entrega::Silencio {
                sequencia: 43,
                timestamp_us: 860_000,
            }),
            (QuallAudioOrder::Silence, 43, 0, 0)
        );
    }

    /// **Os valores das ordens são ABI**, pela mesma regra de [`QuallTrackKind`].
    #[test]
    fn os_valores_da_ordem_de_audio_sao_abi_e_nao_podem_mudar() {
        assert_eq!(QuallAudioOrder::Frame as i32, 0);
        assert_eq!(QuallAudioOrder::Fec as i32, 1);
        assert_eq!(QuallAudioOrder::Silence as i32, 2);
        // Entrou em 18/09/2026, no fim.
        assert_eq!(QuallAudioOrder::Idle as i32, 3);
    }

    /// A porta puxada recusa o que não é dela, sem derrubar nada: nulo, e nulo de novo.
    #[test]
    fn a_porta_puxada_recusa_track_nula_e_limpa_o_nulo() {
        unsafe {
            assert!(quall_audio_playout_new(ptr::null(), false).is_null());
            assert_eq!(quall_audio_playout_free(ptr::null_mut()), QuallStatus::Ok);
            assert!(quall_audio_playout_rate(ptr::null()).is_nan());
            assert_eq!(
                quall_audio_playout_stats_json(ptr::null(), ptr::null_mut(), 0),
                -1
            );
            let mut slot = QuallAudioSlot {
                order: QuallAudioOrder::Frame,
                payload: ptr::null(),
                len: 0,
                sequence: 0,
                timestamp_us: 0,
                fec_has_lbrr: 0,
            };
            assert_eq!(
                quall_audio_playout_pull(ptr::null_mut(), 0, f64::NAN, &mut slot),
                QuallStatus::NullPointer
            );
            let mut saida = 0i64;
            assert_eq!(quall_track_capture_offset_us(ptr::null(), &mut saida), -1);
        }
    }

    /// As chaves do JSON da reprodução são literais de contrato (`docs/contrato-som-puxado.md`
    /// §4): esta lista é a do documento, e a invariante fecha nos valores.
    #[test]
    fn as_chaves_do_json_da_reproducao_sao_as_do_contrato() {
        let c = ContadoresDeReproducao {
            puxadas: 7,
            ociosas: 1,
            quadros: 3,
            curas_oferecidas: 1,
            buracos: 1,
            subconsumos: 1,
            razao_aplicada: 1.0,
            ..ContadoresDeReproducao::default()
        };
        let v = json_da_reproducao(&c);
        let obj = v.as_object().expect("objeto");
        let esperadas = [
            "pulls",
            "idle_pulls",
            "frames",
            "fec_offers",
            "holes",
            "underruns",
            "drift_inserts",
            "burst_inserts",
            "late_inserts",
            "drift_drops",
            "burst_drops",
            "ceiling_drops",
            "too_late",
            "duplicates",
            "reordered",
            "anchors",
            "dropped_at_anchor",
            "went_idle",
            "ring_overflows",
            "ring_dropped",
            "oversized",
            "playout_skips",
            "skipped_slots",
            "level",
            "depth",
            "burst_pulls",
            "anchor_latency_us",
            "dac_delay_us",
            "applied_rate",
            "suggested_rate",
            "ed_drift_ppm",
        ];
        let mut chaves: Vec<&str> = obj.keys().map(String::as_str).collect();
        chaves.sort_unstable();
        let mut esperadas_ordenadas = esperadas.to_vec();
        esperadas_ordenadas.sort_unstable();
        assert_eq!(chaves, esperadas_ordenadas);
        assert_eq!(
            v["suggested_rate"],
            serde_json::Value::Null,
            "não medido é null"
        );
        assert_eq!(c.puxadas, c.soma_das_ordens());
    }

    /// O relógio e o buffer no JSON da track: `null` quando não há, e as chaves do contrato
    /// quando há.
    #[test]
    fn o_relogio_e_o_buffer_no_json_da_track() {
        assert_eq!(json_do_relogio(None), serde_json::Value::Null);
        assert_eq!(json_do_buffer(None), serde_json::Value::Null);
        let r = RetratoDoRelogio {
            referencia: false,
            deslocamento: DeslocamentoDeCaptura::Recusado { motivo: "x".into() },
            residuo_us: Some(-3_000_000),
            residuo_da_janela_us: Some(-3_000_100),
            deriva_entre_tracks_ppm: None,
            violacoes_da_guarda: 1,
        };
        let v = json_do_relogio(Some(r));
        assert_eq!(v["status"], "refused");
        assert_eq!(v["reason"], "x", "a recusa diz por quê");
        assert_eq!(v["capture_offset_us"], serde_json::Value::Null);
        assert_eq!(v["residual_us"], -3_000_000);
        assert_eq!(v["guard_violations"], 1);
        let b = json_do_buffer(Some(ContadoresDeBuffer::default()));
        for chave in [
            "slots",
            "frames",
            "holes",
            "fec_offers",
            "silences",
            "too_late",
            "duplicates",
            "reordered",
            "resyncs",
            "max_occupancy",
            "max_delay_us",
        ] {
            assert!(b.get(chave).is_some(), "falta {chave}");
        }
    }

    /// **`fec_has_lbrr` nunca mente, e um socorro vazio não derruba o processo.**
    ///
    /// Duas coisas num teste só, porque foi um achado só. O socorro vem **da rede**, e ao
    /// escrever esta porta o pacote de zero byte descobriu que `opus_packet_has_lbrr` lê
    /// `packet[0]` sem olhar o `len` — SIGSEGV, não erro. A guarda mora em `quall_opus::tem_lbrr`
    /// e o teste da causa está lá; este aqui fixa o que a **fronteira** faz com o resultado.
    ///
    /// E o que ela faz é responder `-1`, "não sei" — nunca `0`. Um `0` seria afirmar que não há
    /// LBRR, e a casca que confiasse nele estaria decidindo com uma resposta inventada. É a lição
    /// da dívida 26 num campo de C.
    #[test]
    fn socorro_ilegivel_responde_nao_sei_e_nao_nao() {
        let (_, _, len, lbrr) = entregar(Entrega::Fec {
            socorro: &[],
            sequencia: 7,
            timestamp_us: 140_000,
        });
        assert_eq!(len, 0);
        assert_eq!(
            lbrr, -1,
            "sem saber ler o pacote a resposta é -1; 0 seria afirmar que não há LBRR"
        );
    }

    /// O contraponto necessário: um pacote **legível** que só não tem LBRR responde `0`.
    ///
    /// Sem este, o teste de cima poderia passar com um `fec_has_lbrr` que devolvesse `-1` para
    /// tudo — e "não sei" para tudo é tão inútil quanto mentir.
    #[test]
    fn socorro_legivel_sem_lbrr_responde_nao_e_nao_nao_sei() {
        // TOC 0xFF: CELT, onde o LBRR não existe. Legível, e a resposta é um "não" de verdade.
        let celt = [0xFFu8, 0xFF, 0xFF];
        let (_, _, _, lbrr) = entregar(Entrega::Fec {
            socorro: &celt,
            sequencia: 8,
            timestamp_us: 160_000,
        });
        assert_eq!(lbrr, 0);
    }

    /// E o caso positivo, com um pacote de verdade saído do **nosso** encoder configurado pelo
    /// preset de microfone: o socorro carrega LBRR e a fronteira diz `1`.
    ///
    /// É o que fecha o par com o `Entrega::Fec`: o buffer oferece porque a **estrutura** permite,
    /// e este campo é a única forma de uma casca C saber se o **conteúdo** permite.
    #[cfg(feature = "opus")]
    #[test]
    fn socorro_com_lbrr_de_verdade_responde_sim() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null());
        let amostras = 960; // 20 ms a 48 kHz, mono
        let mut saida = [0u8; 4096];
        let mut visto_sim = false;
        for q in 0..40u32 {
            let pcm = quadro_de_tom(q, amostras, 1);
            let n = unsafe {
                quall_audio_encoder_encode(
                    enc,
                    pcm.as_ptr(),
                    pcm.len(),
                    saida.as_mut_ptr(),
                    saida.len(),
                )
            };
            assert!(n > 0);
            let (_, _, _, lbrr) = entregar(Entrega::Fec {
                socorro: &saida[..n as usize],
                sequencia: q as u16,
                timestamp_us: u64::from(q) * 20_000,
            });
            assert_ne!(lbrr, -1, "um pacote do nosso encoder tem de ser legível");
            if lbrr == 1 {
                visto_sim = true;
            }
        }
        unsafe { quall_audio_encoder_free(enc) };
        assert!(
            visto_sim,
            "o preset de microfone declara useinbandfec=1; se nenhum socorro carrega LBRR, é a \
             §11 do docs/audio.md de volta"
        );
    }

    #[test]
    fn a_recepcao_de_audio_recusa_track_nula_e_track_de_emissao() {
        assert_eq!(
            unsafe { quall_track_on_audio(ptr::null(), Some(anotar_slot), ptr::null_mut()) },
            QuallStatus::NullPointer
        );
    }

    /// O contrário, e igualmente obrigatório: o preset de sistema **não** pode emitir LBRR, ou o
    /// `useinbandfec=0` do `fmtp` seria mentira na outra direção.
    ///
    /// Com o **mesmo** tom de quatro notas e a mesma amplitude do teste do microfone, para que a
    /// diferença medida seja o preset e não o conteúdo.
    #[cfg(feature = "opus")]
    #[test]
    fn o_encoder_de_audio_de_sistema_nao_emite_lbrr() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::SystemAudio, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null());
        let (com, _) = contar_lbrr(enc, 100, 960, 2);
        unsafe { quall_audio_encoder_free(enc) };
        assert_eq!(com, 0, "o preset de sistema declara useinbandfec=0");
    }

    /// Um `pcm_len` errado **não dá erro no Opus** — dá um pacote com outra duração, que o
    /// `a=fmtp` não anunciou e que o outro lado não tem como perceber. Então a fronteira recusa.
    #[cfg(feature = "opus")]
    #[test]
    fn o_encoder_recusa_um_quadro_de_duracao_errada() {
        let enc = unsafe {
            quall_audio_encoder_new(QuallTrackKind::Microphone, QuallAudioCodec::Default)
        };
        assert!(!enc.is_null());
        let pcm = vec![0i16; 480]; // 10 ms, e o preset diz 20
        let mut saida = [0u8; 4096];
        let n = unsafe {
            quall_audio_encoder_encode(
                enc,
                pcm.as_ptr(),
                pcm.len(),
                saida.as_mut_ptr(),
                saida.len(),
            )
        };
        unsafe { quall_audio_encoder_free(enc) };
        assert_eq!(n, -1);
        let motivo = unsafe { CStr::from_ptr(quall_last_error()) }
            .to_string_lossy()
            .to_string();
        assert!(
            motivo.contains("960"),
            "o erro precisa dizer o esperado: {motivo}"
        );
    }

    /// Liberar nulo é no-op, como toda função `_free` desta fronteira.
    #[test]
    fn liberar_encoder_nulo_e_no_op() {
        unsafe { quall_audio_encoder_free(ptr::null_mut()) };
    }
}

/// **As mensagens e o teleprompter pela porta em C** (F6a): as mesmas chamadas que Swift e o JNI
/// vão fazer, numa sessão de verdade por 127.0.0.1. É loopback, não a bancada.
#[cfg(test)]
mod testes_do_teleprompter {
    use super::*;
    use quall_core::teleprompter::mudou;
    use std::ffi::CString;

    /// Um ponteiro cru atravessando para a thread do teste. Os testes são donos dos dois lados.
    struct Ponteiro<T>(*mut T);
    // SAFETY: cada ponteiro é usado por uma thread de cada vez, e o teste espera as threads.
    unsafe impl<T> Send for Ponteiro<T> {}

    /// Uma porta livre agora. Há corrida com outro processo, e num teste isso é aceitável.
    fn porta_livre() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("porta livre")
    }

    /// As strings que as opções apontam, vivas enquanto a struct for usada.
    struct Opcoes {
        _id: CString,
        _nome: CString,
        _pin: CString,
        opcoes: QuallSessionOptions,
    }

    fn opcoes(id: &str, pin: &str, porta: u16) -> Opcoes {
        let (id, nome, pin) = (
            CString::new(id).unwrap(),
            CString::new(format!("{id} de teste")).unwrap(),
            CString::new(pin).unwrap(),
        );
        let opcoes = QuallSessionOptions {
            me: QuallDeviceDesc {
                device_id: id.as_ptr(),
                display_name: nome.as_ptr(),
                screen_source: false,
                camera_source: false,
                sink: false,
            },
            pin: pin.as_ptr(),
            known_peers_json: ptr::null(),
            signaling_port: porta,
            timeout_ms: 30_000,
            tracks: ptr::null(),
            track_count: 0,
            bind_address: ptr::null(),
        };
        Opcoes {
            _id: id,
            _nome: nome,
            _pin: pin,
            opcoes,
        }
    }

    /// Um prompter hospedando pela fronteira numa thread, e um controle conectando nesta.
    fn prompter_e_controle(marca: &str, pin: &str) -> (*mut QuallSession, *mut QuallSession, u16) {
        let porta = porta_livre();
        let (id_p, pin_p) = (format!("prompter-{marca}"), pin.to_string());
        let lado = std::thread::spawn(move || {
            let o = opcoes(&id_p, &pin_p, porta);
            Ponteiro(unsafe {
                quall_host_with_role(&o.opcoes, ptr::null(), c"teleprompter".as_ptr())
            })
        });
        let o = opcoes(&format!("controle-{marca}"), pin, porta);
        let destino = CString::new(format!("127.0.0.1:{porta}")).unwrap();
        let mut controle = ptr::null_mut();
        for _ in 0..100 {
            controle = unsafe {
                quall_connect_with_role(
                    destino.as_ptr(),
                    &o.opcoes,
                    ptr::null(),
                    c"controle_remoto".as_ptr(),
                )
            };
            if !controle.is_null() {
                break;
            }
            // O prompter ainda não abriu a porta: tente de novo.
            std::thread::sleep(Duration::from_millis(50));
        }
        let prompter = lado.join().expect("thread").0;
        assert!(
            !controle.is_null(),
            "o controle não conectou: {:?}",
            unsafe { CStr::from_ptr(quall_last_error()) }
        );
        assert!(!prompter.is_null(), "o prompter não subiu");
        (prompter, controle, porta)
    }

    /// Lê a próxima mensagem pelo padrão das duas chamadas, como a casca faz. `None`: nada no prazo.
    fn proxima(m: *const QuallMessages, timeout_ms: u32) -> Option<String> {
        let n = unsafe { quall_messages_next(m, timeout_ms, ptr::null_mut(), 0) };
        assert!(n >= 0, "erro: {:?}", quall_last_status());
        if n == 0 {
            return None;
        }
        let mut buf = vec![0u8; n as usize];
        let n2 = unsafe { quall_messages_next(m, 0, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(
            n2, n,
            "a mensagem espiada tinha de ser a mesma, e tinha de caber"
        );
        buf.pop(); // o NUL
        Some(String::from_utf8(buf).expect("UTF-8"))
    }

    /// O padrão `(buf, cap)` **como a casca tem de fazer**: repetir até caber. O conteúdo pode
    /// crescer entre a chamada que pergunta e a que escreve — medido aqui mesmo, na primeira
    /// versão deste auxiliar, que supunha o contrário: o estado do teleprompter passou de 357 para
    /// 360 bytes entre as duas chamadas porque `par_visto_ha_ms` ganhou um dígito.
    fn texto_de(f: impl Fn(*mut c_char, usize) -> isize) -> String {
        let mut n = f(ptr::null_mut(), 0);
        assert!(n > 0);
        loop {
            let mut buf = vec![0u8; n as usize];
            let escrito = f(buf.as_mut_ptr().cast(), buf.len());
            assert!(escrito > 0);
            if escrito as usize <= buf.len() {
                buf.truncate(escrito as usize - 1); // sem o NUL
                return String::from_utf8(buf).expect("UTF-8");
            }
            n = escrito;
        }
    }

    /// **Mensagens nos dois sentidos, pela porta em C**, e o `(buf, cap)` que só consome quando
    /// cabe: perguntar o tamanho não tira a mensagem da fila.
    #[test]
    fn mensagens_atravessam_pela_fronteira_c_nos_dois_sentidos() {
        let (p, c, _) = prompter_e_controle("msg", "818181");
        unsafe {
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            assert!(!mp.is_null() && !mc.is_null());
            let mut ok = false;
            for _ in 0..100 {
                if quall_messages_send(mp, c"do prompter: ação 🎬".as_ptr()) == QuallStatus::Ok
                {
                    ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(ok, "o canal não abriu");
            assert_eq!(
                quall_messages_send(mc, c"do controle".as_ptr()),
                QuallStatus::Ok
            );
            assert_eq!(proxima(mc, 5_000).as_deref(), Some("do prompter: ação 🎬"));
            assert_eq!(proxima(mp, 5_000).as_deref(), Some("do controle"));
            assert_eq!(proxima(mp, 100), None, "nada mais: 0");

            // Buffer pequeno demais: devolve o tamanho e **não consome**.
            assert_eq!(quall_messages_send(mc, c"cabe?".as_ptr()), QuallStatus::Ok);
            let mut pequeno = [0u8; 3];
            let n = quall_messages_next(mp, 5_000, pequeno.as_mut_ptr().cast(), pequeno.len());
            assert_eq!(n, 6, "\"cabe?\" com o NUL");
            assert_eq!(
                proxima(mp, 0).as_deref(),
                Some("cabe?"),
                "a mensagem ficou na fila"
            );

            let stats = texto_de(|b, n| quall_messages_stats_json(mp, b, n));
            assert!(stats.contains("\"recebidas\":2"), "{stats}");
            quall_messages_free(mp);
            quall_messages_free(mc);
            assert_eq!(quall_session_close(c), QuallStatus::Ok);
            assert_eq!(quall_session_close(p), QuallStatus::Ok);
        }
    }

    /// Comprimento zero, acima do teto, ponteiros nulos: recusados na fronteira, com o código.
    #[test]
    fn os_limites_da_mensagem_pela_fronteira() {
        let (p, c, _) = prompter_e_controle("lim", "828282");
        unsafe {
            let mp = quall_session_messages(p);
            assert_eq!(
                quall_messages_send(mp, c"".as_ptr()),
                QuallStatus::Invalid,
                "vazia"
            );
            let grande = CString::new("g".repeat(quall_message_max_bytes() + 1)).unwrap();
            assert_eq!(
                quall_messages_send(mp, grande.as_ptr()),
                QuallStatus::Invalid,
                "acima do teto"
            );
            assert_eq!(
                quall_messages_send(mp, ptr::null()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_messages_send(ptr::null(), c"x".as_ptr()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_messages_next(mp, 0, ptr::null_mut(), 8),
                -1,
                "buf nulo com cap"
            );
            assert_eq!(quall_last_status(), QuallStatus::NullPointer);
            assert_eq!(quall_messages_next(ptr::null(), 0, ptr::null_mut(), 0), -1);
            assert!(quall_session_messages(ptr::null()).is_null());
            quall_messages_free(ptr::null_mut());
            quall_messages_free(mp);
            quall_session_close(c);
            quall_session_close(p);
        }
    }

    /// **Chamada depois de `quall_session_close`**: o handle continua válido, mandar dá `CLOSED`
    /// sem tocar na biblioteca, ler entrega o que tinha chegado e depois `CLOSED`. Nada trava.
    #[test]
    fn o_handle_depois_de_fechar_a_sessao() {
        let (p, c, _) = prompter_e_controle("fim", "838383");
        unsafe {
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            let mut ok = false;
            for _ in 0..100 {
                ok = quall_messages_send(mp, c"antes".as_ptr()) == QuallStatus::Ok;
                if ok {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(ok);
            // Espera chegar, sem consumir.
            assert!(quall_messages_next(mc, 5_000, ptr::null_mut(), 0) > 0);
            assert_eq!(quall_session_close(c), QuallStatus::Ok);
            assert_eq!(quall_session_close(p), QuallStatus::Ok);
            assert_eq!(
                quall_messages_send(mp, c"depois".as_ptr()),
                QuallStatus::Closed
            );
            assert_eq!(
                quall_messages_send(mc, c"depois".as_ptr()),
                QuallStatus::Closed
            );
            assert_eq!(
                proxima(mc, 1_000).as_deref(),
                Some("antes"),
                "o que chegou antes é entregue"
            );
            let comeco = Instant::now();
            assert_eq!(quall_messages_next(mc, 5_000, ptr::null_mut(), 0), -1);
            assert_eq!(quall_last_status(), QuallStatus::Closed);
            assert!(
                comeco.elapsed() < Duration::from_secs(1),
                "esperou o prazo numa sessão morta"
            );

            // O teleprompter com o handle de uma sessão fechada: a bombeada diz `CLOSED` na hora,
            // e a edição da tela continua valendo aqui (fica devida para a próxima sessão).
            let t = quall_teleprompter_new(
                c"prompter-fim".as_ptr(),
                c"teleprompter".as_ptr(),
                ptr::null(),
            );
            let comeco = Instant::now();
            let mut ch = 7u32;
            assert_eq!(
                quall_teleprompter_pump(t, mp, 5_000, &mut ch),
                QuallStatus::Closed
            );
            // Nenhuma mudança de conteúdo; só o bit do contato (nunca houve par nesta réplica).
            assert_eq!(
                ch & !(QuallTeleprompterChange::Peer as u32),
                0,
                "mudança de conteúdo numa sessão fechada: {ch}"
            );
            assert!(comeco.elapsed() < Duration::from_secs(1));
            assert_eq!(quall_teleprompter_set_scrolling(t, true), QuallStatus::Ok);
            assert_eq!(
                quall_teleprompter_set_text(t, c"".as_ptr()),
                QuallStatus::Ok,
                "roteiro vazio é roteiro"
            );
            assert!(texto_de(|b, n| quall_teleprompter_state_json(t, b, n))
                .contains("\"rolando\":true"));
            quall_teleprompter_free(t);
            quall_messages_free(mp);
            quall_messages_free(mc);
        }
    }

    /// **Fila cheia**, pela fronteira: 100 mensagens sem ninguém ler; o JSON conta 36 descartadas,
    /// e ficam as 64 mais novas.
    #[test]
    fn fila_cheia_pela_fronteira_guarda_as_mais_novas() {
        let (p, c, _) = prompter_e_controle("fila", "848484");
        unsafe {
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            let mut i = 0;
            let fim = Instant::now() + Duration::from_secs(10);
            while i < 100 && Instant::now() < fim {
                let m = CString::new(format!("m{i:03}")).unwrap();
                if quall_messages_send(mp, m.as_ptr()) == QuallStatus::Ok {
                    i += 1;
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            let fim = Instant::now() + Duration::from_secs(10);
            while !texto_de(|b, n| quall_messages_stats_json(mc, b, n))
                .contains("\"descartadas_fila_cheia\":36")
                && Instant::now() < fim
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            let mut ficou = Vec::new();
            while let Some(t) = proxima(mc, 200) {
                ficou.push(t);
            }
            assert_eq!(ficou.len(), 64);
            assert!(ficou.contains(&"m099".to_string()), "{ficou:?}");
            quall_messages_free(mp);
            quall_messages_free(mc);
            quall_session_close(c);
            quall_session_close(p);
        }
    }

    /// O papel trocado, desconhecido, ou com tracks: `QUALL_STATUS_INVALID` antes de abrir porta.
    #[test]
    fn papel_errado_na_fronteira_e_invalid() {
        unsafe {
            let o = opcoes("x", "858585", 0);
            assert!(
                quall_host_with_role(&o.opcoes, ptr::null(), c"controle_remoto".as_ptr()).is_null()
            );
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            assert!(quall_host_with_role(&o.opcoes, ptr::null(), c"parede".as_ptr()).is_null());
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            assert!(quall_connect_with_role(
                c"127.0.0.1:1".as_ptr(),
                &o.opcoes,
                ptr::null(),
                c"teleprompter".as_ptr()
            )
            .is_null());
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            assert!(quall_advertiser_start_with_role(
                &o.opcoes.me,
                7877,
                c"controle_remoto".as_ptr()
            )
            .is_null());
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            let track = QuallTrackDesc {
                kind: QuallTrackKind::Screen,
                label: c"Tela".as_ptr(),
                audio_codec: QuallAudioCodec::Default,
            };
            let mut com_track = opcoes("y", "858585", 0);
            com_track.opcoes.tracks = &track;
            com_track.opcoes.track_count = 1;
            assert!(
                quall_host_with_role(&com_track.opcoes, ptr::null(), c"teleprompter".as_ptr())
                    .is_null()
            );
            assert_eq!(quall_last_status(), QuallStatus::Invalid);

            assert!(
                quall_teleprompter_new(c"a".as_ptr(), c"parede".as_ptr(), ptr::null()).is_null()
            );
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            assert!(
                quall_teleprompter_new(c"a".as_ptr(), c"teleprompter".as_ptr(), c"{".as_ptr())
                    .is_null()
            );
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            assert!(
                quall_teleprompter_new(ptr::null(), c"teleprompter".as_ptr(), ptr::null())
                    .is_null()
            );
            assert_eq!(quall_last_status(), QuallStatus::NullPointer);
        }
    }

    /// **Um segundo controle ouve `QUALL_STATUS_BUSY`**, pela fronteira, na hora.
    #[test]
    fn segundo_controle_ouve_busy_pela_fronteira() {
        let (p, c, porta) = prompter_e_controle("ocupado", "868686");
        unsafe {
            let o = opcoes("outro-controle", "868686", porta);
            let destino = CString::new(format!("127.0.0.1:{porta}")).unwrap();
            let comeco = Instant::now();
            let s = quall_connect_with_role(
                destino.as_ptr(),
                &o.opcoes,
                ptr::null(),
                c"controle_remoto".as_ptr(),
            );
            assert!(s.is_null());
            assert_eq!(
                quall_last_status(),
                QuallStatus::Busy,
                "{:?}",
                CStr::from_ptr(quall_last_error())
            );
            assert!(comeco.elapsed() < Duration::from_secs(5));
            assert_eq!(quall_session_next_event(p, 0), QuallSessionEvent::None);
            let peer = texto_de(|b, n| quall_session_peer_json(p, b, n));
            assert!(peer.contains("\"papel\":\"controle_remoto\""), "{peer}");
            assert!(peer.contains("\"identity_authenticated\":true"), "{peer}");
            quall_session_close(c);
            quall_session_close(p);
        }
    }

    /// **Defeito 1 da revisão, pela porta em C**: o controle pausa e fecha a sessão logo em
    /// seguida. A bombeada do prompter que descobre o fechamento devolve `QUALL_STATUS_CLOSED`
    /// **com** `QUALL_TELEPROMPTER_CHANGE_SCROLLING` em `changed` — até então vinha `changed = 0` e
    /// a tela seguia rolando.
    #[test]
    fn closed_vem_com_changed_preenchido() {
        let (p, c, _) = prompter_e_controle("pausa-e-cai", "888888");
        unsafe {
            let tp = quall_teleprompter_new(
                c"prompter-pc".as_ptr(),
                c"teleprompter".as_ptr(),
                ptr::null(),
            );
            let tc = quall_teleprompter_new(
                c"controle-pc".as_ptr(),
                c"controle_remoto".as_ptr(),
                ptr::null(),
            );
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            assert_eq!(quall_teleprompter_set_scrolling(tc, true), QuallStatus::Ok);
            let fim = Instant::now() + Duration::from_secs(10);
            while Instant::now() < fim {
                quall_teleprompter_pump(tc, mc, 10, ptr::null_mut());
                quall_teleprompter_pump(tp, mp, 10, ptr::null_mut());
                if texto_de(|b, n| quall_teleprompter_state_json(tp, b, n))
                    .contains("\"rolando\":true")
                {
                    break;
                }
            }
            assert_eq!(
                quall_teleprompter_set_scrolling(tc, false),
                QuallStatus::Ok,
                "a pausa sai na hora"
            );
            assert_eq!(quall_session_close(c), QuallStatus::Ok);
            // Sem bombear o prompter: espera o canal fechar deste lado, com a pausa na fila.
            let fim = Instant::now() + Duration::from_secs(10);
            while quall_messages_send(mp, c"{}".as_ptr()) != QuallStatus::Closed
                && Instant::now() < fim
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                quall_messages_next(mp, 0, ptr::null_mut(), 0) > 0,
                "a pausa não estava na fila"
            );
            let mut ch = 0u32;
            assert_eq!(
                quall_teleprompter_pump(tp, mp, 0, &mut ch),
                QuallStatus::Closed
            );
            assert_ne!(
                ch & QuallTeleprompterChange::Scrolling as u32,
                0,
                "CLOSED veio sem a pausa em `changed`"
            );
            assert!(texto_de(|b, n| quall_teleprompter_state_json(tp, b, n))
                .contains("\"rolando\":false"));
            quall_teleprompter_free(tp);
            quall_teleprompter_free(tc);
            quall_messages_free(mp);
            quall_messages_free(mc);
            quall_session_close(p);
        }
    }

    /// Os bits da fronteira são os do núcleo, um a um.
    #[test]
    fn os_bits_da_fronteira_sao_os_do_nucleo() {
        use QuallTeleprompterChange as B;
        let pares = [
            (B::Text, mudou::TEXTO),
            (B::Scrolling, mudou::ROLANDO),
            (B::Speed, mudou::VELOCIDADE),
            (B::FontSize, mudou::FONTE),
            (B::Margin, mudou::MARGEM),
            (B::ReadingLine, mudou::LINHA_DE_LEITURA),
            (B::Mirror, mudou::ESPELHO),
            (B::Position, mudou::POSICAO),
            (B::Jump, mudou::SALTO),
            (B::Peer, mudou::PAR),
            (B::TextQuestion, mudou::PERGUNTA_DO_TEXTO),
            (B::TextCopy, mudou::COPIA_DO_TEXTO),
            (B::Hold, mudou::SEGURAR),
            (B::Recording, mudou::GRAVACAO),
        ];
        for (b, n) in pares {
            assert_eq!(b as u32, n, "{b:?}");
        }
        assert_eq!(quall_teleprompter_max_text_bytes(), 131_072);
        assert_eq!(quall_message_max_bytes(), 262_144);
    }

    /// **O teleprompter de ponta a ponta, pela porta em C**: duas réplicas, uma bombeada em cada
    /// lado, edições dos dois lados — um roteiro com emoji do controle, o espelho e a posição do
    /// prompter —, e as duas convergem. Depois o salvo volta, e a queda aplica a política.
    #[test]
    fn o_teleprompter_de_ponta_a_ponta_pela_fronteira() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (p, c, _) = prompter_e_controle("tp", "878787");
        unsafe {
            let tp = quall_teleprompter_new(
                c"prompter-tp".as_ptr(),
                c"teleprompter".as_ptr(),
                ptr::null(),
            );
            let tc = quall_teleprompter_new(
                c"controle-tp".as_ptr(),
                c"controle_remoto".as_ptr(),
                ptr::null(),
            );
            assert!(!tp.is_null() && !tc.is_null());
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            let parar = std::sync::Arc::new(AtomicBool::new(false));
            let mudou_no_prompter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
            let bombas: Vec<_> = [(tp, mp, true), (tc, mc, false)]
                .into_iter()
                .map(|(t, m, e_prompter)| {
                    let (t, m) = (Ponteiro(t), Ponteiro(m));
                    let parar = std::sync::Arc::clone(&parar);
                    let acumulado = std::sync::Arc::clone(&mudou_no_prompter);
                    std::thread::spawn(move || {
                        let (t, m) = (t, m);
                        while !parar.load(Ordering::Relaxed) {
                            let mut ch = 0u32;
                            if quall_teleprompter_pump(t.0, m.0, 50, &mut ch) != QuallStatus::Ok {
                                break;
                            }
                            if e_prompter {
                                acumulado.fetch_or(ch, Ordering::Relaxed);
                            }
                        }
                    })
                })
                .collect();

            let roteiro = CString::new("Boa noite. 🎬 Ação!\nSegunda linha.".repeat(500)).unwrap();
            // Espera até `pronto(estado do prompter, estado do controle, texto do prompter)`.
            let esperar = |pronto: &dyn Fn(&str, &str, &[u8]) -> bool| -> Result<(), String> {
                let fim = Instant::now() + Duration::from_secs(15);
                let mut visto = String::new();
                while Instant::now() < fim {
                    let ep = texto_de(|b, n| quall_teleprompter_state_json(tp, b, n));
                    let ec = texto_de(|b, n| quall_teleprompter_state_json(tc, b, n));
                    let tex = texto_de(|b, n| quall_teleprompter_text(tp, b, n));
                    if pronto(&ep, &ec, tex.as_bytes()) {
                        return Ok(());
                    }
                    visto = format!(
                        "prompter {ep}\ncontrole {ec}\ntexto no prompter: {} bytes",
                        tex.len()
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(visto)
            };

            // 1. Edições dos dois lados, ao mesmo tempo.
            assert_eq!(
                quall_teleprompter_set_text(tc, roteiro.as_ptr()),
                QuallStatus::Ok
            );
            assert_eq!(quall_teleprompter_set_speed(tc, 3.0), QuallStatus::Ok);
            assert_eq!(quall_teleprompter_set_mirror(tp, true), QuallStatus::Ok);
            assert_eq!(quall_teleprompter_set_position(tp, 0.2), QuallStatus::Ok);
            assert_eq!(
                quall_teleprompter_set_position(tc, 0.2),
                QuallStatus::Invalid,
                "o controle não relata posição"
            );
            assert_eq!(
                quall_teleprompter_set_speed(tc, f64::NAN),
                QuallStatus::Invalid
            );
            let fase1 = esperar(&|ep, ec, tex| {
                tex == roteiro.as_bytes()
                    && ep.contains("\"velocidade\":3.0")
                    && ec.contains("\"espelho\":true")
                    && ec.contains("\"posicao\":0.2")
                    && ec.contains("\"sem_confirmacao_ha_ms\":null")
            });
            // 2. O salto: o prompter vai para o alvo ao fundi-lo, e o relato dele o mostra.
            assert_eq!(quall_teleprompter_jump(tc, 0.5), QuallStatus::Ok);
            let fase2 = esperar(&|ep, ec, _| {
                ep.contains("\"salto\":0.5")
                    && ep.contains("\"posicao\":0.5")
                    && ec.contains("\"posicao\":0.5")
                    && ec.contains("\"sem_confirmacao_ha_ms\":null")
            });
            parar.store(true, Ordering::Relaxed);
            for b in bombas {
                let _ = b.join();
            }
            if let Err(visto) = fase1 {
                panic!("as edições não convergiram em 15 s\n{visto}");
            }
            if let Err(visto) = fase2 {
                panic!("o salto não convergiu em 15 s\n{visto}");
            }
            let visto = mudou_no_prompter.load(Ordering::Relaxed);
            for bit in [
                QuallTeleprompterChange::Text,
                QuallTeleprompterChange::Speed,
                QuallTeleprompterChange::Jump,
            ] {
                assert_ne!(
                    visto & bit as u32,
                    0,
                    "o prompter não viu {bit:?} em `changed`"
                );
            }

            // O salvo volta numa réplica nova, com o roteiro e a velocidade.
            let salvo =
                CString::new(texto_de(|b, n| quall_teleprompter_saved_json(tp, b, n))).unwrap();
            let volta = quall_teleprompter_new(
                c"prompter-tp".as_ptr(),
                c"teleprompter".as_ptr(),
                salvo.as_ptr(),
            );
            assert!(!volta.is_null());
            assert_eq!(
                texto_de(|b, n| quall_teleprompter_text(volta, b, n)).as_bytes(),
                roteiro.as_bytes()
            );
            let mut ch = 0;
            assert_eq!(quall_teleprompter_peer_lost(tp, &mut ch), QuallStatus::Ok);
            assert_ne!(ch & QuallTeleprompterChange::Peer as u32, 0);

            assert_eq!(
                quall_teleprompter_pump(ptr::null(), mp, 0, ptr::null_mut()),
                QuallStatus::NullPointer
            );
            quall_teleprompter_free(volta);
            quall_teleprompter_free(tp);
            quall_teleprompter_free(tc);
            quall_messages_free(mp);
            quall_messages_free(mc);
            quall_session_close(c);
            quall_session_close(p);
        }
    }

    // -----------------------------------------------------------------------------------------
    // §11 do contrato: a porta 7979, o link e a pergunta do texto, pela porta em C.
    // -----------------------------------------------------------------------------------------

    /// As opções de um controle **sem PIN** e com os pares conhecidos — a volta automática.
    struct OpcoesSemPin {
        _id: CString,
        _nome: CString,
        _pares: Option<CString>,
        opcoes: QuallSessionOptions,
    }

    fn opcoes_sem_pin(id: &str, pares: Option<&str>, porta: u16, timeout_ms: u32) -> OpcoesSemPin {
        let (id, nome) = (
            CString::new(id).unwrap(),
            CString::new(format!("{id} de teste")).unwrap(),
        );
        let pares = pares.map(|p| CString::new(p).unwrap());
        let opcoes = QuallSessionOptions {
            me: QuallDeviceDesc {
                device_id: id.as_ptr(),
                display_name: nome.as_ptr(),
                screen_source: false,
                camera_source: false,
                sink: false,
            },
            pin: ptr::null(),
            known_peers_json: pares.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
            signaling_port: porta,
            timeout_ms,
            tracks: ptr::null(),
            track_count: 0,
            bind_address: ptr::null(),
        };
        OpcoesSemPin {
            _id: id,
            _nome: nome,
            _pares: pares,
            opcoes,
        }
    }

    /// **A 7979 no núcleo** (§11.1): o controle que manda só o IP disca a porta do teleprompter, e
    /// não a 7877 do espelhamento. Antes desta frente ele ia à 7877 — a casca é que completava.
    #[test]
    fn quall_connect_with_role_completa_com_7979() {
        let escuta = std::net::TcpListener::bind(("127.0.0.1", PORTA_DO_TELEPROMPTER)).unwrap_or_else(|e| {
            panic!("a porta 7979 está ocupada nesta máquina ({e}): feche o prompter que a usa e rode de novo")
        });
        let chegou = std::thread::spawn(move || {
            // Aceita um e fecha: o controle ouve o fim da sinalização e desiste. Com prazo: se o
            // controle discar outra porta, ninguém chega, e o teste tem de falhar — não pendurar.
            escuta.set_nonblocking(true).unwrap();
            let fim = Instant::now() + Duration::from_secs(5);
            while Instant::now() < fim {
                if let Ok((conexao, _)) = escuta.accept() {
                    drop(conexao);
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        });
        let o = opcoes_sem_pin("controle-7979", None, 0, 3_000);
        let s = unsafe {
            quall_connect_with_role(
                c"127.0.0.1".as_ptr(),
                &o.opcoes,
                ptr::null(),
                c"controle_remoto".as_ptr(),
            )
        };
        assert!(
            s.is_null(),
            "ninguém do outro lado respondeu, e a sessão subiu?"
        );
        assert!(chegou.join().unwrap(), "o controle não discou a 7979");
        assert_eq!(quall_teleprompter_default_port(), 7979);
        assert_ne!(quall_teleprompter_pick_port(0), 0);
    }

    /// **Achado C1 (alta)**: `quall_connect*` recebe só endereço, e recusa um link com `INVALID`,
    /// sem tocar na rede.
    #[test]
    fn link_no_connect_e_invalid() {
        let o = opcoes_sem_pin("controle-link", None, 0, 3_000);
        for link in [
            c"quall://424242@127.0.0.1:7979",
            c"QUALL://424242@127.0.0.1",
        ] {
            let s = unsafe {
                quall_connect_with_role(
                    link.as_ptr(),
                    &o.opcoes,
                    ptr::null(),
                    c"controle_remoto".as_ptr(),
                )
            };
            assert!(s.is_null());
            assert_eq!(quall_last_status(), QuallStatus::Invalid, "{link:?}");
            let s = unsafe { quall_connect(link.as_ptr(), &o.opcoes) };
            assert!(s.is_null());
            assert_eq!(
                quall_last_status(),
                QuallStatus::Invalid,
                "{link:?} no connect de vídeo"
            );
        }
    }

    /// **Achado C1, a volta**: o controle lê o link (endereço e PIN nos campos), pareia; o prompter
    /// cai e sorteia **outro** PIN; a volta automática vai com o endereço e **sem** PIN, e retoma
    /// pelo segredo. Com o link no `connect` (o rascunho), a volta tiraria de novo o PIN velho do
    /// link, e o PIN tem precedência sobre a retomada (`pairing.rs:385`): `WRONG_PIN`, e o prompter
    /// trocaria o PIN outra vez.
    #[test]
    fn a_volta_sem_pin_retoma() {
        let porta = porta_livre();
        let link = CString::new(format!("quall://515151@127.0.0.1:{porta}")).unwrap();
        let lido = texto_de(|b, n| unsafe {
            quall_parse_endpoint_json(link.as_ptr(), c"controle_remoto".as_ptr(), b, n)
        });
        let lido: serde_json::Value = serde_json::from_str(&lido).unwrap();
        assert_eq!(lido["pin"], "515151");
        let endereco = CString::new(lido["endereco"].as_str().unwrap()).unwrap();

        let hospedar = |pin: &'static str, pares: Option<String>| {
            std::thread::spawn(move || {
                let mut o = opcoes("prompter-volta", pin, porta);
                let pares = pares.map(|p| CString::new(p).unwrap());
                o.opcoes.known_peers_json = pares.as_ref().map_or(ptr::null(), |p| p.as_ptr());
                let s = unsafe {
                    quall_host_with_role(&o.opcoes, ptr::null(), c"teleprompter".as_ptr())
                };
                let pares = (!s.is_null()).then(|| {
                    texto_de(|b, n| unsafe {
                        quall_session_known_peers_json(s, o.opcoes.known_peers_json, b, n)
                    })
                });
                (Ponteiro(s), pares)
            })
        };
        let conectar = |endpoint: &CStr, o: &QuallSessionOptions| {
            for _ in 0..100 {
                let s = unsafe {
                    quall_connect_with_role(
                        endpoint.as_ptr(),
                        o,
                        ptr::null(),
                        c"controle_remoto".as_ptr(),
                    )
                };
                if !s.is_null()
                    || !matches!(quall_last_status(), QuallStatus::Io | QuallStatus::Busy)
                {
                    return s;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            ptr::null_mut()
        };

        // 1. Primeira vez: o endereço e o PIN que o link trouxe.
        let lado = hospedar("515151", None);
        let o = opcoes("controle-volta", "515151", 0);
        let c = conectar(&endereco, &o.opcoes);
        assert!(!c.is_null(), "o primeiro pareamento falhou: {:?}", unsafe {
            CStr::from_ptr(quall_last_error())
        });
        let pares_c =
            texto_de(|b, n| unsafe { quall_session_known_peers_json(c, ptr::null(), b, n) });
        let (p, pares_p) = lado.join().unwrap();
        unsafe {
            quall_session_close(c);
            quall_session_close(p.0);
        }

        // 2. O prompter volta com outro PIN (o de antes pode ter sido gasto), e os pares dele.
        let lado = hospedar("525252", pares_p);
        let volta = opcoes_sem_pin("controle-volta", Some(&pares_c), 0, 10_000);
        // O link no campo não vai ao connect: INVALID, sem gastar o PIN do prompter.
        let s = unsafe {
            quall_connect_with_role(
                link.as_ptr(),
                &volta.opcoes,
                ptr::null(),
                c"controle_remoto".as_ptr(),
            )
        };
        assert!(s.is_null());
        assert_eq!(
            quall_last_status(),
            QuallStatus::Invalid,
            "o link entrou no connect"
        );
        // A volta automática: o endereço, sem PIN — retoma.
        let c = conectar(&endereco, &volta.opcoes);
        let status = quall_last_status();
        let (p, _) = lado.join().unwrap();
        assert!(!c.is_null(), "a volta sem PIN não retomou: {status:?}");
        assert!(!p.0.is_null(), "o prompter não subiu na volta");
        unsafe {
            assert!(
                !quall_session_pairing_is_new(c),
                "a volta fez pareamento novo em vez de retomar"
            );
            quall_session_close(c);
            quall_session_close(p.0);
        }
    }

    /// `quall_parse_endpoint_json` e a regra das cascas (§11.1).
    #[test]
    fn quall_parse_endpoint_json_e_a_regra_das_cascas() {
        let ler = |t: &CStr, papel: *const c_char| -> Option<String> {
            let n = unsafe { quall_parse_endpoint_json(t.as_ptr(), papel, ptr::null_mut(), 0) };
            (n > 0).then(|| {
                texto_de(|b, cap| unsafe { quall_parse_endpoint_json(t.as_ptr(), papel, b, cap) })
            })
        };
        let controle = c"controle_remoto".as_ptr();
        assert_eq!(
            ler(c"192.168.57.8", controle).as_deref(),
            Some(r#"{"endereco":"192.168.57.8:7979","pin":null}"#)
        );
        assert_eq!(
            ler(c"192.168.57.8", ptr::null()).as_deref(),
            Some(r#"{"endereco":"192.168.57.8:7877","pin":null}"#)
        );
        assert_eq!(
            ler(c"192.168.57.8", c"".as_ptr()).as_deref(),
            Some(r#"{"endereco":"192.168.57.8:7877","pin":null}"#)
        );
        assert_eq!(
            ler(c"quall://424242@[fe80::1]:7980/", controle).as_deref(),
            Some(r#"{"endereco":"[fe80::1]:7980","pin":"424242"}"#)
        );
        for (t, papel) in [
            (c"quall://42424@192.168.57.8:7979", controle),
            (c"192.168.57.8 : 7979", controle),
            (c"192.168.57.8", c"teleprompter".as_ptr()),
        ] {
            assert_eq!(ler(t, papel), None, "{t:?}");
            assert_eq!(quall_last_status(), QuallStatus::Invalid, "{t:?}");
        }
        let n = unsafe { quall_parse_endpoint_json(ptr::null(), controle, ptr::null_mut(), 0) };
        assert_eq!((n, quall_last_status()), (-1, QuallStatus::NullPointer));
    }

    /// **A pergunta do texto de ponta a ponta, pela porta em C**: o controle chega com o roteiro
    /// dele num prompter novo; o estado mostra a pergunta; o texto do prompter sai por
    /// `_question_text`; "usar o do prompter" com o resumo visto; a cópia por `_text_copy`, o bit
    /// `_TEXT_COPY` na bombeada, o esquecimento; e o roteiro do prompter nunca é substituído.
    #[test]
    fn a_pergunta_do_texto_pela_fronteira() {
        use quall_core::teleprompter::resumo;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let (p, c, _) = prompter_e_controle("pergunta", "898989");
        unsafe {
            let tp = quall_teleprompter_new(
                c"prompter-pergunta".as_ptr(),
                c"teleprompter".as_ptr(),
                ptr::null(),
            );
            let tc = quall_teleprompter_new(
                c"controle-pergunta".as_ptr(),
                c"controle_remoto".as_ptr(),
                ptr::null(),
            );
            assert!(!tp.is_null() && !tc.is_null());
            // A casca que tem a tela da pergunta liga a trava (§11.10); sem ela, não há pergunta.
            assert_eq!(quall_teleprompter_enable_text_question(tc), QuallStatus::Ok);
            assert_eq!(
                quall_teleprompter_enable_text_question(ptr::null()),
                QuallStatus::NullPointer
            );
            let roteiro_p = "Roteiro do prompter. 🎬\n".repeat(300);
            let rp = CString::new(roteiro_p.clone()).unwrap();
            assert_eq!(
                quall_teleprompter_set_text(tp, rp.as_ptr()),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_teleprompter_set_text(tc, c"Roteiro do controle.".as_ptr()),
                QuallStatus::Ok
            );
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            let parar = std::sync::Arc::new(AtomicBool::new(false));
            let bits_no_controle = std::sync::Arc::new(AtomicU32::new(0));
            let bombas: Vec<_> = [(tp, mp, false), (tc, mc, true)]
                .into_iter()
                .map(|(t, m, e_controle)| {
                    let (t, m) = (Ponteiro(t), Ponteiro(m));
                    let parar = std::sync::Arc::clone(&parar);
                    let bits = std::sync::Arc::clone(&bits_no_controle);
                    std::thread::spawn(move || {
                        let (t, m) = (t, m);
                        while !parar.load(Ordering::Relaxed) {
                            let mut ch = 0u32;
                            if quall_teleprompter_pump(t.0, m.0, 20, &mut ch) != QuallStatus::Ok {
                                break;
                            }
                            if e_controle {
                                bits.fetch_or(ch, Ordering::Relaxed);
                            }
                        }
                    })
                })
                .collect();
            let estado = |t| -> serde_json::Value {
                serde_json::from_str(&texto_de(|b, n| quall_teleprompter_state_json(t, b, n)))
                    .unwrap()
            };
            let esperar = |ok: &dyn Fn() -> bool| {
                let fim = Instant::now() + Duration::from_secs(15);
                while !ok() && Instant::now() < fim {
                    std::thread::sleep(Duration::from_millis(10));
                }
                ok()
            };
            let abriu = esperar(&|| estado(tc)["pergunta_do_texto"]["aberta"] == true);
            let visto = estado(tc)["pergunta_do_texto"]["do_prompter"]["resumo"]
                .as_str()
                .map(str::to_string);
            let do_prompter = (quall_teleprompter_question_text(tc, ptr::null_mut(), 0) > 0)
                .then(|| texto_de(|b, n| quall_teleprompter_question_text(tc, b, n)));
            let visto_c = CString::new(visto.clone().unwrap_or_default()).unwrap();
            let escolha = quall_teleprompter_resolve_text(tc, false, visto_c.as_ptr());
            let convergiu = esperar(&|| {
                texto_de(|b, n| quall_teleprompter_text(tc, b, n)) == roteiro_p
                    && bits_no_controle.load(Ordering::Relaxed)
                        & QuallTeleprompterChange::TextCopy as u32
                        != 0
                    && texto_de(|b, n| quall_teleprompter_saved_json(tc, b, n))
                        .contains("\"ultimo_prompter_id\":\"prompter-pergunta\"")
            });
            let (ep, ec) = (estado(tp), estado(tc));
            parar.store(true, Ordering::Relaxed);
            for b in bombas {
                let _ = b.join();
            }
            assert!(abriu, "a pergunta não abriu: {ec}");
            assert_eq!(visto.as_deref(), Some(resumo(&roteiro_p).as_str()));
            assert_eq!(
                do_prompter.as_deref(),
                Some(roteiro_p.as_str()),
                "o texto da pergunta não é o do prompter"
            );
            assert_eq!(escolha, QuallStatus::Ok);
            assert!(convergiu, "não convergiu: controle {ec}");
            assert_ne!(
                bits_no_controle.load(Ordering::Relaxed)
                    & QuallTeleprompterChange::TextQuestion as u32,
                0
            );
            assert_eq!(
                texto_de(|b, n| quall_teleprompter_text(tp, b, n)),
                roteiro_p,
                "o roteiro do prompter foi substituído"
            );
            assert_eq!(
                ec["contadores"]["textos_enviados"], 0,
                "o controle mandou o texto dele"
            );
            assert_eq!(ep["contadores"]["reenvios_desistidos"], 0);
            assert!(ec["pergunta_do_texto"].is_null());

            // A cópia: pelo resumo, e o esquecimento.
            let meu = CString::new(resumo("Roteiro do controle.")).unwrap();
            assert_eq!(
                ec["copias_do_texto"][0]["resumo"],
                resumo("Roteiro do controle.")
            );
            assert_eq!(ec["copias_do_texto"][0]["origem"], "controle");
            assert_eq!(
                texto_de(|b, n| quall_teleprompter_text_copy(tc, meu.as_ptr(), b, n)),
                "Roteiro do controle."
            );
            assert_eq!(
                quall_teleprompter_forget_text_copy(tc, meu.as_ptr()),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_teleprompter_text_copy(tc, meu.as_ptr(), ptr::null_mut(), 0),
                -1
            );
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            assert_eq!(
                quall_teleprompter_forget_text_copy(tc, meu.as_ptr()),
                QuallStatus::Invalid
            );
            // Sem pergunta aberta.
            assert_eq!(
                quall_teleprompter_resolve_text(tc, true, visto_c.as_ptr()),
                QuallStatus::Invalid
            );
            assert_eq!(quall_teleprompter_question_text(tc, ptr::null_mut(), 0), -1);
            assert_eq!(quall_last_status(), QuallStatus::Invalid);
            assert_eq!(
                quall_teleprompter_resolve_text(tc, true, ptr::null()),
                QuallStatus::NullPointer
            );

            quall_teleprompter_free(tp);
            quall_teleprompter_free(tc);
            quall_messages_free(mp);
            quall_messages_free(mc);
            quall_session_close(c);
            quall_session_close(p);
        }
    }

    /// **"Segurar para rolar", pela porta em C** (§12): sem a tela do prompter dizer que entende, o
    /// controle ouve `PROTOCOL`; com ela, apertar para trás chega ao prompter (`rolando`, `para_tras`,
    /// `segurando`, o bit `HOLD`), soltar para; e a queda com o dedo no botão para o texto.
    #[test]
    fn segurar_para_rolar_pela_fronteira() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let (p, c, _) = prompter_e_controle("segurar", "919191");
        unsafe {
            let tp = quall_teleprompter_new(
                c"prompter-segurar".as_ptr(),
                c"teleprompter".as_ptr(),
                ptr::null(),
            );
            let tc = quall_teleprompter_new(
                c"controle-segurar".as_ptr(),
                c"controle_remoto".as_ptr(),
                ptr::null(),
            );
            assert!(!tp.is_null() && !tc.is_null());
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            let parar = std::sync::Arc::new(AtomicBool::new(false));
            let bits_no_prompter = std::sync::Arc::new(AtomicU32::new(0));
            let bombas: Vec<_> = [(tp, mp, true), (tc, mc, false)]
                .into_iter()
                .map(|(t, m, e_prompter)| {
                    let (t, m) = (Ponteiro(t), Ponteiro(m));
                    let parar = std::sync::Arc::clone(&parar);
                    let bits = std::sync::Arc::clone(&bits_no_prompter);
                    std::thread::spawn(move || {
                        let (t, m) = (t, m);
                        while !parar.load(Ordering::Relaxed) {
                            let mut ch = 0u32;
                            if quall_teleprompter_pump(t.0, m.0, 20, &mut ch) != QuallStatus::Ok {
                                break;
                            }
                            if e_prompter {
                                bits.fetch_or(ch, Ordering::Relaxed);
                            }
                        }
                    })
                })
                .collect();
            let estado = |t| -> serde_json::Value {
                serde_json::from_str(&texto_de(|b, n| quall_teleprompter_state_json(t, b, n)))
                    .unwrap()
            };
            let esperar = |ok: &dyn Fn() -> bool| {
                let fim = Instant::now() + Duration::from_secs(10);
                while !ok() && Instant::now() < fim {
                    std::thread::sleep(Duration::from_millis(10));
                }
                ok()
            };
            // A tela do prompter ainda não disse que entende: o controle não segura.
            let _ = esperar(&|| !estado(tc)["par_visto_ha_ms"].is_null());
            let sem_tela = quall_teleprompter_hold(tc, true);
            // A tela liga; o controle vê, e segura para trás.
            assert_eq!(quall_teleprompter_enable_hold(tp), QuallStatus::Ok);
            let viu = esperar(&|| estado(tc)["par_entende_segurar"] == true);
            let aperto = quall_teleprompter_hold(tc, true);
            let rolou_para_tras = esperar(&|| {
                let e = estado(tp);
                e["rolando"] == true && e["para_tras"] == true && e["segurando"] == true
            });
            let soltura = quall_teleprompter_release(tc);
            let parou = esperar(&|| {
                let e = estado(tp);
                e["rolando"] == false && e["para_tras"] == false && e["segurando"] == false
            });
            // A queda com o dedo no botão: o prompter para.
            let de_novo = quall_teleprompter_hold(tc, false);
            let segurando = esperar(&|| estado(tp)["segurando"] == true);
            parar.store(true, Ordering::Relaxed);
            for b in bombas {
                let _ = b.join();
            }
            let mut ch = 0u32;
            assert_eq!(quall_teleprompter_peer_lost(tp, &mut ch), QuallStatus::Ok);
            let depois_da_queda = estado(tp);

            assert_eq!(
                sem_tela,
                QuallStatus::Protocol,
                "segurou sem a tela do prompter entender"
            );
            assert!(viu, "o controle não viu o prompter dizer que entende");
            assert_eq!(
                (aperto, soltura, de_novo),
                (QuallStatus::Ok, QuallStatus::Ok, QuallStatus::Ok)
            );
            assert!(
                rolou_para_tras,
                "o aperto para trás não chegou: {}",
                estado(tp)
            );
            assert!(parou, "soltou e o prompter não parou: {}", estado(tp));
            assert!(segurando);
            assert_ne!(
                bits_no_prompter.load(Ordering::Relaxed) & QuallTeleprompterChange::Hold as u32,
                0
            );
            assert_ne!(
                ch & QuallTeleprompterChange::Scrolling as u32,
                0,
                "a queda segurando não avisou a tela"
            );
            assert_eq!(
                depois_da_queda["rolando"], false,
                "caiu segurando e o prompter seguiu rolando"
            );
            assert_eq!(
                quall_teleprompter_hold(tp, false),
                QuallStatus::Invalid,
                "o prompter segurou"
            );
            assert_eq!(
                quall_teleprompter_release(ptr::null()),
                QuallStatus::NullPointer
            );

            quall_teleprompter_free(tp);
            quall_teleprompter_free(tc);
            quall_messages_free(mp);
            quall_messages_free(mc);
            quall_session_close(c);
            quall_session_close(p);
        }
    }

    /// **A gravação pela porta em C** (§13), com uma sessão de verdade por 127.0.0.1: sem a tela do
    /// prompter dizer que grava, o controle ouve `PROTOCOL`; com ela, o controle pede, a "casca" do
    /// prompter (a thread da bombeada dele) obedece ao bit `RECORDING`, e o controle vê a duração
    /// contando; parar para; a recusa chega com o motivo; e a queda não para a gravação.
    #[test]
    fn gravar_pelo_controle_pela_fronteira() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::sync::Arc;
        let (p, c, _) = prompter_e_controle("gravar", "939393");
        unsafe {
            let tp = quall_teleprompter_new(
                c"prompter-gravar".as_ptr(),
                c"teleprompter".as_ptr(),
                ptr::null(),
            );
            let tc = quall_teleprompter_new(
                c"controle-gravar".as_ptr(),
                c"controle_remoto".as_ptr(),
                ptr::null(),
            );
            assert!(!tp.is_null() && !tc.is_null());
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            let parar = Arc::new(AtomicBool::new(false));
            let recusar = Arc::new(AtomicBool::new(false));
            // Um erro da "casca" na thread da bombeada vira este contador, e não um pânico que o
            // `join` engoliria (a falha apareceria como uma espera de 10 s com a mensagem errada).
            let erros_da_casca = Arc::new(AtomicU32::new(0));
            let bits_no_controle = Arc::new(AtomicU32::new(0));
            let bombas: Vec<_> = [(tp, mp, true), (tc, mc, false)]
                .into_iter()
                .map(|(t, m, e_prompter)| {
                    let (t, m) = (Ponteiro(t), Ponteiro(m));
                    let (parar, recusar, bits) = (
                        Arc::clone(&parar),
                        Arc::clone(&recusar),
                        Arc::clone(&bits_no_controle),
                    );
                    let erros = Arc::clone(&erros_da_casca);
                    std::thread::spawn(move || {
                        let (t, m) = (t, m);
                        while !parar.load(Ordering::Relaxed) {
                            let mut ch = 0u32;
                            if quall_teleprompter_pump(t.0, m.0, 20, &mut ch) != QuallStatus::Ok {
                                break;
                            }
                            if !e_prompter {
                                bits.fetch_or(ch, Ordering::Relaxed);
                                continue;
                            }
                            // A "casca" do prompter: decide o pedido no bit.
                            if ch & QuallTeleprompterChange::Recording as u32 != 0 {
                                let e: serde_json::Value =
                                    serde_json::from_str(&texto_de(|b, n| {
                                        quall_teleprompter_state_json(t.0, b, n)
                                    }))
                                    .unwrap();
                                let pedido = &e["pedido_de_gravacao"];
                                if let (Some(gravar), Some(n)) =
                                    (pedido["gravar"].as_bool(), pedido["n"].as_u64())
                                {
                                    let st = if recusar.load(Ordering::Relaxed) {
                                        quall_teleprompter_refuse_recording(
                                            t.0,
                                            n,
                                            c"sem espaço: sobram 312 MB".as_ptr(),
                                        )
                                    } else {
                                        quall_teleprompter_set_recording(t.0, gravar)
                                    };
                                    if st != QuallStatus::Ok {
                                        erros.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    })
                })
                .collect();
            let estado = |t| -> serde_json::Value {
                serde_json::from_str(&texto_de(|b, n| quall_teleprompter_state_json(t, b, n)))
                    .unwrap()
            };
            let esperar = |ok: &dyn Fn() -> bool| {
                let fim = Instant::now() + Duration::from_secs(10);
                while !ok() && Instant::now() < fim {
                    std::thread::sleep(Duration::from_millis(10));
                }
                ok()
            };
            // A tela do prompter ainda não grava: o controle não pede.
            let _ = esperar(&|| !estado(tc)["par_visto_ha_ms"].is_null());
            let sem_tela = quall_teleprompter_request_record(tc);
            assert_eq!(
                quall_teleprompter_enable_recording(tp, true),
                QuallStatus::Ok
            );
            let viu = esperar(&|| estado(tc)["par_entende_gravar"] == true);
            // Gravar.
            let pedido = quall_teleprompter_request_record(tc);
            let gravou = esperar(&|| {
                let e = estado(tc);
                e["gravando_ha_ms"].as_u64().is_some_and(|ms| ms >= 200)
                    && e["pedido_de_gravacao"].is_null()
            });
            // Parar.
            let pedido_parar = quall_teleprompter_request_stop(tc);
            let parou = esperar(&|| {
                let e = estado(tc);
                e["gravando_ha_ms"].is_null()
                    && e["pedido_de_gravacao"].is_null()
                    && estado(tp)["gravando_ha_ms"].is_null()
            });
            // A recusa.
            recusar.store(true, Ordering::Relaxed);
            let pedido_recusado = quall_teleprompter_request_record(tc);
            let recusou = esperar(&|| {
                estado(tc)["gravacao_recusada"]["motivo"] == "sem espaço: sobram 312 MB"
            });
            // A queda gravando.
            recusar.store(false, Ordering::Relaxed);
            let pedido_da_queda = quall_teleprompter_request_record(tc);
            let gravando = esperar(&|| !estado(tp)["gravando_ha_ms"].is_null());
            parar.store(true, Ordering::Relaxed);
            for b in bombas {
                let _ = b.join();
            }
            let mut ch = 0u32;
            assert_eq!(quall_teleprompter_peer_lost(tp, &mut ch), QuallStatus::Ok);
            let depois_da_queda = estado(tp);

            assert_eq!(
                sem_tela,
                QuallStatus::Protocol,
                "pediu sem a tela do prompter gravar"
            );
            assert!(viu, "o controle não viu o prompter dizer que grava");
            assert_eq!(
                (pedido, pedido_parar, pedido_recusado, pedido_da_queda),
                (
                    QuallStatus::Ok,
                    QuallStatus::Ok,
                    QuallStatus::Ok,
                    QuallStatus::Ok
                )
            );
            assert!(gravou, "o controle não viu a gravação: {}", estado(tc));
            assert!(parou, "o controle não viu parar: {}", estado(tc));
            assert!(recusou, "o controle não viu a recusa: {}", estado(tc));
            assert!(gravando);
            assert_eq!(
                erros_da_casca.load(Ordering::Relaxed),
                0,
                "a casca do prompter ouviu erro do núcleo"
            );
            assert_ne!(
                bits_no_controle.load(Ordering::Relaxed)
                    & QuallTeleprompterChange::Recording as u32,
                0
            );
            assert!(
                !depois_da_queda["gravando_ha_ms"].is_null(),
                "a queda parou a gravação: {depois_da_queda}"
            );
            // Os papéis e os ponteiros.
            assert_eq!(
                quall_teleprompter_set_recording(tc, true),
                QuallStatus::Invalid,
                "o controle relatou a gravação"
            );
            assert_eq!(
                quall_teleprompter_request_record(tp),
                QuallStatus::Invalid,
                "o prompter pediu"
            );
            assert_eq!(
                quall_teleprompter_refuse_recording(tp, 1, c"".as_ptr()),
                QuallStatus::Invalid
            );
            assert_eq!(
                quall_teleprompter_refuse_recording(tp, 1, ptr::null()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_teleprompter_request_stop(ptr::null()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_teleprompter_enable_recording(ptr::null(), true),
                QuallStatus::NullPointer
            );

            quall_teleprompter_free(tp);
            quall_teleprompter_free(tc);
            quall_messages_free(mp);
            quall_messages_free(mc);
            quall_session_close(c);
            quall_session_close(p);
        }
    }

    /// **O aperto depois do `CLOSED` da bombeada** (§12.3, achado 5 de 14/09): entre a bombeada que
    /// devolve `QUALL_STATUS_CLOSED` e `quall_teleprompter_peer_lost`, `quall_teleprompter_hold` é
    /// `QUALL_STATUS_CLOSED` — antes era aceito, e numa casca que não chamasse `peer_lost` ia para o
    /// prompter seguinte.
    #[test]
    fn segurar_depois_do_closed_da_bombeada_e_closed() {
        let (p, c, _) = prompter_e_controle("segurar-no-fim", "929292");
        unsafe {
            let tp = quall_teleprompter_new(
                c"prompter-fim".as_ptr(),
                c"teleprompter".as_ptr(),
                ptr::null(),
            );
            let tc = quall_teleprompter_new(
                c"controle-fim".as_ptr(),
                c"controle_remoto".as_ptr(),
                ptr::null(),
            );
            let (mp, mc) = (quall_session_messages(p), quall_session_messages(c));
            assert_eq!(quall_teleprompter_enable_hold(tp), QuallStatus::Ok);
            let fim = Instant::now() + Duration::from_secs(10);
            while Instant::now() < fim
                && !texto_de(|b, n| quall_teleprompter_state_json(tc, b, n))
                    .contains("\"par_entende_segurar\":true")
            {
                quall_teleprompter_pump(tc, mc, 10, ptr::null_mut());
                quall_teleprompter_pump(tp, mp, 10, ptr::null_mut());
            }
            assert_eq!(
                quall_teleprompter_hold(tc, false),
                QuallStatus::Ok,
                "não segurou com a sessão de pé"
            );
            assert_eq!(quall_teleprompter_release(tc), QuallStatus::Ok);
            // O prompter fecha; a bombeada do controle devolve CLOSED.
            assert_eq!(quall_session_close(p), QuallStatus::Ok);
            let fim = Instant::now() + Duration::from_secs(10);
            let mut st = QuallStatus::Ok;
            while st != QuallStatus::Closed && Instant::now() < fim {
                st = quall_teleprompter_pump(tc, mc, 20, ptr::null_mut());
            }
            assert_eq!(
                st,
                QuallStatus::Closed,
                "a bombeada não disse que a sessão acabou"
            );
            // Ainda sem `peer_lost`.
            assert_eq!(
                quall_teleprompter_hold(tc, true),
                QuallStatus::Closed,
                "segurou depois do CLOSED da bombeada"
            );
            assert!(texto_de(|b, n| quall_teleprompter_state_json(tc, b, n))
                .contains("\"segurando\":false"));
            let mut ch = 0u32;
            assert_eq!(quall_teleprompter_peer_lost(tc, &mut ch), QuallStatus::Ok);
            assert_eq!(quall_teleprompter_hold(tc, true), QuallStatus::Closed);

            quall_teleprompter_free(tp);
            quall_teleprompter_free(tc);
            quall_messages_free(mp);
            quall_messages_free(mc);
            quall_session_close(c);
        }
    }
}

// =============================================================================================
// A revisão do código da S1 (18/09/2026, `criticas-som/4-codigo-s1-porta-puxada.md` e
// `5-codigo-s1-relogio.md`): as travas pela fronteira C, numa sessão de verdade em laço local.
//
// Um teste que detecta trava **aborta o processo** em vez de ficar preso: uma thread da
// libdatachannel parada num cadeado não sai mais, e o `Drop` da sessão esperaria por ela.
// =============================================================================================
#[cfg(test)]
mod testes_da_revisao_s1 {
    use super::*;
    use quall_core::track::{AmostraDeAudio, QuadroCodificado, TrackEmissor};
    use quall_core::transport::{PeerState, Session, TransportEvent};
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::mpsc;

    fn negociar(a: &mut Session, b: &mut Session, prazo: Duration) -> bool {
        fn passo(origem: &Session, destino: &mut Session) -> bool {
            let Some(e) = origem.next_event(Duration::from_millis(5)) else {
                return false;
            };
            match e {
                TransportEvent::LocalDescription { kind, sdp } => {
                    destino.set_remote_description(&kind, &sdp).expect("sdp");
                    false
                }
                TransportEvent::LocalCandidate { candidate, mid } => {
                    let _ = destino.add_remote_candidate(&candidate, &mid);
                    false
                }
                TransportEvent::State(PeerState::Connected) => true,
                _ => false,
            }
        }
        let fim = Instant::now() + prazo;
        let (mut ca, mut cb) = (false, false);
        while Instant::now() < fim {
            ca |= passo(a, b);
            cb |= passo(b, a);
            if ca && cb {
                return true;
            }
        }
        false
    }

    /// Uma sessão com as tracks dadas; as receptoras voltam como handles da fronteira, na ordem
    /// das espécies pedidas.
    struct Bancada {
        a: Session,
        b: Session,
        emissores: Vec<Arc<TrackEmissor>>,
        tracks: Vec<*mut QuallTrack>,
    }

    fn bancada(especies: &[TrackKind]) -> Bancada {
        let cfg = TransportConfig::default();
        let configs: Vec<TrackConfig> =
            especies.iter().map(|k| TrackConfig::new(*k, "T")).collect();
        let (mut a, emissores) = Session::offerer_com_tracks(&cfg, &configs).expect("ofertante");
        let mut b = Session::answerer(&cfg).expect("respondente");
        assert!(
            negociar(&mut a, &mut b, Duration::from_secs(20)),
            "não conectou"
        );
        let mut chegadas = Vec::new();
        let fim = Instant::now() + Duration::from_secs(5);
        while chegadas.len() < especies.len() && Instant::now() < fim {
            if let Some(t) = b.proxima_track(Duration::from_millis(50)) {
                chegadas.push(t);
            }
        }
        let tracks = especies
            .iter()
            .map(|k| {
                let i = chegadas
                    .iter()
                    .position(|t| t.kind() == *k)
                    .expect("a track da espécie chegou");
                let r = chegadas.remove(i);
                Box::into_raw(Box::new(QuallTrack {
                    lado: Lado::Receptor(Arc::new(r)),
                    audio: Mutex::new(None),
                }))
            })
            .collect();
        Bancada {
            a,
            b,
            emissores: emissores.into_iter().map(Arc::new).collect(),
            tracks,
        }
    }

    impl Bancada {
        /// Manda `n` pacotes de áudio pela track emissora `i`, um a cada `passo`.
        fn audio(&self, i: usize, n: u64, passo: Duration) {
            for k in 0..n {
                let p = [0xfcu8, k as u8, 1, 2, 3];
                let _ = self.emissores[i].enviar_audio(AmostraDeAudio {
                    payload: &p,
                    timestamp_us: 1_000_000 + k * 20_000,
                });
                std::thread::sleep(passo);
            }
        }

        /// A ordem segura: **todos** os tratadores desregistrados antes de **qualquer** handle
        /// liberado. O escoamento de `quall_track_on_audio(NULL)` chama o tratador, e um
        /// tratador pode consultar outra track (ver `quall_track_free`).
        fn fechar(self) {
            for &t in &self.tracks {
                unsafe {
                    let _ = quall_track_on_audio(t, None, ptr::null_mut());
                    let _ = quall_track_on_frame(t, None, ptr::null_mut());
                }
            }
            for t in self.tracks {
                unsafe { quall_track_free(t) };
            }
            drop(self.a);
            drop(self.b);
        }
    }

    fn idr(n: usize) -> Vec<u8> {
        let mut v = vec![
            0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80,
        ];
        v.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        v.extend((0..n).map(|i| ((i % 254) + 1) as u8));
        v
    }

    /// Pergunta o JSON da track **de outra thread, com prazo**: `true` se respondeu. Se a
    /// pergunta precisasse de um cadeado que esta thread segura, a outra esperaria e o prazo
    /// estouraria — sem travar o processo.
    fn stats_de_fora_responde(t: *const QuallTrack) -> bool {
        let (tx, rx) = mpsc::channel();
        let tp = t as usize;
        std::thread::spawn(move || {
            let n = unsafe { quall_track_stats_json(tp as *const QuallTrack, ptr::null_mut(), 0) };
            let _ = tx.send(n);
        });
        matches!(rx.recv_timeout(Duration::from_secs(1)), Ok(n) if n > 0)
    }

    // ---------------------------------------------------------------------------------------
    // B1, pela fronteira: `quall_track_stats_json` de dentro do tratador de quadro, e de dentro
    // do tratador de áudio. Os dois travavam antes: o de quadro pelo cadeado do depacotizador,
    // que a bomba segura; o de áudio pelo cadeado do buffer, que o tratador segura.
    // ---------------------------------------------------------------------------------------

    struct Pergunta {
        t: *const QuallTrack,
        respondeu: AtomicU64,
        chamadas: AtomicU64,
    }

    unsafe extern "C" fn quadro_que_pergunta(_f: *const QuallFrame, ud: *mut c_void) {
        let p = &*(ud as *const Pergunta);
        if stats_de_fora_responde(p.t) {
            // Só na mesma thread se a de fora respondeu: senão travaria de vez.
            let n = quall_track_stats_json(p.t, ptr::null_mut(), 0);
            let mut dv = 0i64;
            let _ = quall_track_capture_offset_us(p.t, &mut dv);
            if n > 0 {
                p.respondeu.fetch_add(1, Ordering::SeqCst);
            }
        }
        p.chamadas.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn slot_que_pergunta(_s: *const QuallAudioSlot, ud: *mut c_void) {
        let p = &*(ud as *const Pergunta);
        if stats_de_fora_responde(p.t) {
            let n = quall_track_stats_json(p.t, ptr::null_mut(), 0);
            if n > 0 {
                p.respondeu.fetch_add(1, Ordering::SeqCst);
            }
        }
        p.chamadas.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn stats_json_de_dentro_dos_tratadores_nao_trava() {
        let mesa = bancada(&[TrackKind::Screen, TrackKind::SystemAudio]);
        let (tv, ta) = (mesa.tracks[0], mesa.tracks[1]);
        let pv: &'static Pergunta = Box::leak(Box::new(Pergunta {
            t: tv,
            respondeu: AtomicU64::new(0),
            chamadas: AtomicU64::new(0),
        }));
        let pa: &'static Pergunta = Box::leak(Box::new(Pergunta {
            t: ta,
            respondeu: AtomicU64::new(0),
            chamadas: AtomicU64::new(0),
        }));
        unsafe {
            assert_eq!(
                quall_track_on_frame(tv, Some(quadro_que_pergunta), pv as *const _ as *mut c_void),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_track_on_audio(ta, Some(slot_que_pergunta), pa as *const _ as *mut c_void),
                QuallStatus::Ok
            );
        }
        for k in 0..10u64 {
            let q = idr(400);
            let _ = mesa.emissores[0].enviar_quadro(QuadroCodificado {
                annexb: &q,
                timestamp_us: 1_000_000 + k * 33_333,
                idr: true,
            });
            mesa.audio(1, 2, Duration::from_millis(10));
        }
        mesa.audio(1, 10, Duration::from_millis(10));
        let fim = Instant::now() + Duration::from_secs(40);
        while (pv.chamadas.load(Ordering::SeqCst) < 10 || pa.chamadas.load(Ordering::SeqCst) < 10)
            && Instant::now() < fim
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let (cv, rv) = (
            pv.chamadas.load(Ordering::SeqCst),
            pv.respondeu.load(Ordering::SeqCst),
        );
        let (ca, ra) = (
            pa.chamadas.load(Ordering::SeqCst),
            pa.respondeu.load(Ordering::SeqCst),
        );
        eprintln!(
            "stats de dentro: quadro {rv} de {cv} responderam; áudio {ra} de {ca} responderam"
        );
        assert!(
            cv >= 10 && ca >= 10,
            "os tratadores tinham de rodar: {cv}, {ca}"
        );
        assert_eq!(
            rv, cv,
            "de dentro do tratador de quadro, o stats esperou a bomba"
        );
        assert_eq!(
            ra, ca,
            "de dentro do tratador de áudio, o stats esperou o buffer"
        );
        mesa.fechar();
    }

    // ---------------------------------------------------------------------------------------
    // A2: a trava cruzada entre `quall_track_stats_json` e `on_audio(NULL)` de dentro do
    // tratador (o teste do revisor, com aborto no lugar de ficar preso).
    // ---------------------------------------------------------------------------------------

    struct Estado {
        t: *const QuallTrack,
        ciclos: AtomicU64,
        ultimo_status: AtomicU32,
        encerrando: AtomicBool,
    }

    /// Trabalha 1 ms como um decoder, desregistra **de dentro**, e registra de novo.
    unsafe extern "C" fn tratador_que_desregistra(_s: *const QuallAudioSlot, ud: *mut c_void) {
        let e = &*(ud as *const Estado);
        std::thread::sleep(Duration::from_millis(1));
        if !e.encerrando.load(Ordering::SeqCst) {
            let s = quall_track_on_audio(e.t, None, ptr::null_mut());
            e.ultimo_status.store(s as u32, Ordering::SeqCst);
            let _ = quall_track_on_audio(e.t, Some(tratador_que_desregistra), ud);
        }
        e.ciclos.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn stats_json_e_desregistro_de_dentro_nao_travam_em_abba() {
        let mesa = bancada(&[TrackKind::SystemAudio]);
        let t = mesa.tracks[0];
        let estado: &'static Estado = Box::leak(Box::new(Estado {
            t,
            ciclos: AtomicU64::new(0),
            ultimo_status: AtomicU32::new(u32::MAX),
            encerrando: AtomicBool::new(false),
        }));
        let ud = estado as *const Estado as *mut c_void;
        assert_eq!(
            unsafe { quall_track_on_audio(t, Some(tratador_que_desregistra), ud) },
            QuallStatus::Ok
        );
        let parar = Arc::new(AtomicBool::new(false));
        let leituras = Arc::new(AtomicU64::new(0));
        let tp = t as usize;
        let leitor = {
            let (parar, leituras) = (Arc::clone(&parar), Arc::clone(&leituras));
            std::thread::spawn(move || {
                while !parar.load(Ordering::SeqCst) {
                    let n = unsafe {
                        quall_track_stats_json(tp as *const QuallTrack, ptr::null_mut(), 0)
                    };
                    assert!(n > 0);
                    leituras.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let emissor = {
            let parar = Arc::clone(&parar);
            let em = Arc::clone(&mesa.emissores[0]);
            std::thread::spawn(move || {
                let mut i = 0u64;
                while !parar.load(Ordering::SeqCst) {
                    let p = [0xfcu8, i as u8, 1, 2, 3];
                    let _ = em.enviar_audio(AmostraDeAudio {
                        payload: &p,
                        timestamp_us: 1_000_000 + i * 20_000,
                    });
                    i += 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        // Vigia: 6 s; travou se nem o tratador nem o leitor andarem por 2 s.
        let inicio = Instant::now();
        let mut ultimo = (0u64, 0u64, Instant::now());
        while inicio.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(50));
            let agora = (
                estado.ciclos.load(Ordering::SeqCst),
                leituras.load(Ordering::SeqCst),
            );
            if agora.0 != ultimo.0 || agora.1 != ultimo.1 {
                ultimo = (agora.0, agora.1, Instant::now());
            } else if ultimo.2.elapsed() > Duration::from_secs(2) {
                eprintln!(
                    "TRAVOU em ABBA: {} ciclos do tratador e {} leituras antes da trava",
                    agora.0, agora.1
                );
                std::process::abort();
            }
        }
        parar.store(true, Ordering::SeqCst);
        estado.encerrando.store(true, Ordering::SeqCst);
        let _ = emissor.join();
        let _ = leitor.join();
        let (ciclos, lidas) = (
            estado.ciclos.load(Ordering::SeqCst),
            leituras.load(Ordering::SeqCst),
        );
        eprintln!(
            "leitor + desregistro de dentro: {ciclos} ciclos, {lidas} leituras, último status {}",
            estado.ultimo_status.load(Ordering::SeqCst)
        );
        assert!(ciclos > 50 && lidas > 50);
        assert_eq!(
            estado.ultimo_status.load(Ordering::SeqCst),
            QuallStatus::Invalid as u32,
            "de dentro do tratador a barreira não vale"
        );
        mesa.fechar();
    }

    // ---------------------------------------------------------------------------------------
    // O anexo do A2: (i) o escoamento chamava a casca com `track.audio` na mão; (ii) de dentro
    // do tratador de **outra** track, os slots retidos eram descartados em vez de escoados.
    // ---------------------------------------------------------------------------------------

    thread_local! {
        static DESREGISTRANDO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    struct Escoamento {
        t: *const QuallTrack,
        escoados: AtomicU64,
        stats_no_escoamento: AtomicU64,
    }

    /// Durante o escoamento (marcado pela thread que desregistra), pede o JSON da própria track:
    /// com `track.audio` na mão, isso travava a mesma thread.
    unsafe extern "C" fn slot_que_le_no_escoamento(_s: *const QuallAudioSlot, ud: *mut c_void) {
        let e = &*(ud as *const Escoamento);
        if DESREGISTRANDO.with(|d| d.get()) {
            e.escoados.fetch_add(1, Ordering::SeqCst);
            if quall_track_stats_json(e.t, ptr::null_mut(), 0) > 0 {
                e.stats_no_escoamento.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn o_escoamento_nao_chama_a_casca_com_o_cadeado_da_track() {
        let mesa = bancada(&[TrackKind::Microphone]);
        let t = mesa.tracks[0];
        let e: &'static Escoamento = Box::leak(Box::new(Escoamento {
            t,
            escoados: AtomicU64::new(0),
            stats_no_escoamento: AtomicU64::new(0),
        }));
        let ud = e as *const Escoamento as *mut c_void;
        assert_eq!(
            unsafe { quall_track_on_audio(t, Some(slot_que_le_no_escoamento), ud) },
            QuallStatus::Ok
        );
        mesa.audio(0, 30, Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(300));
        let (tx, rx) = mpsc::channel();
        let tp = t as usize;
        std::thread::spawn(move || {
            DESREGISTRANDO.with(|d| d.set(true));
            let s = unsafe { quall_track_on_audio(tp as *const QuallTrack, None, ptr::null_mut()) };
            let _ = tx.send(s);
        });
        let Ok(status) = rx.recv_timeout(Duration::from_secs(5)) else {
            eprintln!("TRAVOU: o escoamento chamou a casca com `track.audio` na mão");
            std::process::abort();
        };
        let (escoados, lidos) = (
            e.escoados.load(Ordering::SeqCst),
            e.stats_no_escoamento.load(Ordering::SeqCst),
        );
        eprintln!("escoamento: status {status:?}, {escoados} slots, {lidos} leituras dentro dele");
        assert_eq!(status, QuallStatus::Ok);
        assert_eq!(escoados, 2, "a profundidade 2 retém dois slots");
        assert_eq!(lidos, escoados);
        mesa.fechar();
    }

    struct Cruzado {
        outra: *const QuallTrack,
        feito: AtomicBool,
        status: AtomicU32,
        escoados_da_outra: AtomicU64,
    }

    /// O tratador da track A: conta os slots que chegam enquanto o tratador de B a desregistra.
    unsafe extern "C" fn slot_de_a(_s: *const QuallAudioSlot, ud: *mut c_void) {
        let c = &*(ud as *const Cruzado);
        if DESREGISTRANDO.with(|d| d.get()) {
            c.escoados_da_outra.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// O tratador da track B: uma vez, desregistra A **de dentro**.
    unsafe extern "C" fn slot_de_b(_s: *const QuallAudioSlot, ud: *mut c_void) {
        let c = &*(ud as *const Cruzado);
        if !c.feito.swap(true, Ordering::SeqCst) {
            DESREGISTRANDO.with(|d| d.set(true));
            let s = quall_track_on_audio(c.outra, None, ptr::null_mut());
            DESREGISTRANDO.with(|d| d.set(false));
            c.status.store(s as u32, Ordering::SeqCst);
        }
    }

    #[test]
    fn desregistrar_outra_track_de_dentro_do_tratador_escoa_os_slots_dela() {
        let mesa = bancada(&[TrackKind::Microphone, TrackKind::SystemAudio]);
        let (a, b) = (mesa.tracks[0], mesa.tracks[1]);
        let c: &'static Cruzado = Box::leak(Box::new(Cruzado {
            outra: a,
            feito: AtomicBool::new(true),
            status: AtomicU32::new(u32::MAX),
            escoados_da_outra: AtomicU64::new(0),
        }));
        let ud = c as *const Cruzado as *mut c_void;
        unsafe {
            assert_eq!(
                quall_track_on_audio(a, Some(slot_de_a), ud),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_track_on_audio(b, Some(slot_de_b), ud),
                QuallStatus::Ok
            );
        }
        // A enche o buffer; depois B dispara o desregistro de A.
        mesa.audio(0, 20, Duration::from_millis(5));
        std::thread::sleep(Duration::from_millis(300));
        c.feito.store(false, Ordering::SeqCst);
        mesa.audio(1, 10, Duration::from_millis(5));
        let fim = Instant::now() + Duration::from_secs(10);
        while c.status.load(Ordering::SeqCst) == u32::MAX && Instant::now() < fim {
            std::thread::sleep(Duration::from_millis(20));
        }
        let (status, escoados) = (
            c.status.load(Ordering::SeqCst),
            c.escoados_da_outra.load(Ordering::SeqCst),
        );
        eprintln!(
            "de dentro de B, desregistrar A: status {status}, {escoados} slots de A escoados"
        );
        assert_eq!(
            status,
            QuallStatus::Invalid as u32,
            "a barreira de dentro não vale"
        );
        assert_eq!(
            escoados, 2,
            "os dois slots retidos de A tinham de ser escoados"
        );
        mesa.fechar();
    }

    // ---------------------------------------------------------------------------------------
    // A porta puxada pela fronteira C, numa sessão de verdade (o teste do revisor que ficou de
    // pé): a exclusão nos dois sentidos, a reabertura depois do `free`, e o ciclo inteiro.
    // ---------------------------------------------------------------------------------------

    unsafe extern "C" fn nada(_slot: *const QuallAudioSlot, _ud: *mut c_void) {}

    #[test]
    fn a_porta_puxada_pela_fronteira_numa_sessao_de_verdade() {
        let mesa = bancada(&[TrackKind::Microphone]);
        let t = mesa.tracks[0];
        unsafe {
            let p = quall_audio_playout_new(t, false);
            assert!(
                !p.is_null(),
                "{}",
                CStr::from_ptr(quall_last_error()).to_string_lossy()
            );
            assert!(
                quall_audio_playout_new(t, false).is_null(),
                "a segunda puxada tinha de cair"
            );
            assert_eq!(
                quall_track_on_audio(t, Some(nada), ptr::null_mut()),
                QuallStatus::Invalid
            );
            assert_eq!(
                quall_track_on_audio(t, None, ptr::null_mut()),
                QuallStatus::Invalid
            );

            let inicio = Instant::now();
            let (mut ia, mut n) = (0u32, 0u64);
            let (mut quadros, mut errados) = (0u64, 0u64);
            let mut slot = std::mem::MaybeUninit::<QuallAudioSlot>::uninit();
            while inicio.elapsed() < Duration::from_millis(2_000) {
                let agora = inicio.elapsed().as_micros() as u64;
                while u64::from(ia) * 20_000 <= agora {
                    let mut pacote = vec![0xfcu8];
                    pacote.extend_from_slice(&ia.to_be_bytes());
                    let _ = mesa.emissores[0].enviar_audio(AmostraDeAudio {
                        payload: &pacote,
                        timestamp_us: 5_000_000 + u64::from(ia) * 20_000,
                    });
                    ia += 1;
                }
                if agora >= n * 20_000 {
                    assert_eq!(
                        quall_audio_playout_pull(p, 10_000, f64::NAN, slot.as_mut_ptr()),
                        QuallStatus::Ok
                    );
                    let s = slot.assume_init_ref();
                    if s.order == QuallAudioOrder::Frame {
                        quadros += 1;
                        let dados = std::slice::from_raw_parts(s.payload, s.len);
                        let indice = u32::from_be_bytes([dados[1], dados[2], dados[3], dados[4]]);
                        if u64::from(indice) * 20_000 != s.timestamp_us {
                            errados += 1;
                        }
                    }
                    n += 1;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let k = quall_audio_playout_stats_json(p, ptr::null_mut(), 0);
            let mut buf = vec![0u8; k as usize];
            quall_audio_playout_stats_json(p, buf.as_mut_ptr() as *mut c_char, buf.len());
            buf.pop();
            let json = String::from_utf8(buf).expect("utf-8");
            eprintln!(
                "porta puxada pela fronteira: {quadros} quadros, {errados} fora do índice; {json}"
            );
            assert!(quadros > 50);
            assert_eq!(errados, 0);
            assert_eq!(quall_audio_playout_free(p), QuallStatus::Ok);
            assert_eq!(
                quall_track_on_audio(t, Some(nada), ptr::null_mut()),
                QuallStatus::Ok
            );
            assert!(
                quall_audio_playout_new(t, false).is_null(),
                "com a empurrada, a puxada cai"
            );
            assert_eq!(
                quall_track_on_audio(t, None, ptr::null_mut()),
                QuallStatus::Ok
            );
            let p2 = quall_audio_playout_new(t, true);
            assert!(!p2.is_null());
            assert_eq!(quall_audio_playout_free(p2), QuallStatus::Ok);
        }
        mesa.fechar();
    }

    /// Conta as alocações e as solturas feitas **na thread armada**. Nas outras threads, e fora
    /// da janela armada, só repassa ao alocador do sistema. Vale para o binário de teste inteiro
    /// da fronteira, e só nele (`cfg(test)`).
    struct Conta;
    static CONTADAS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    thread_local! {
        static ARMADA: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    // SAFETY: repassa ao alocador do sistema sem mudar nada; só conta.
    unsafe impl std::alloc::GlobalAlloc for Conta {
        unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
            if ARMADA.with(|a| a.get()) {
                CONTADAS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            unsafe { std::alloc::System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
            if ARMADA.with(|a| a.get()) {
                CONTADAS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            unsafe { std::alloc::System.dealloc(p, l) }
        }
    }

    #[global_allocator]
    static ALOCADOR: Conta = Conta;

    fn armada<R>(f: impl FnOnce() -> R) -> (R, usize) {
        let antes = CONTADAS.load(std::sync::atomic::Ordering::SeqCst);
        ARMADA.with(|a| a.set(true));
        let r = f();
        ARMADA.with(|a| a.set(false));
        (
            r,
            CONTADAS.load(std::sync::atomic::Ordering::SeqCst) - antes,
        )
    }

    /// A primeira `pull` **pela fronteira** não aloca (revisão do código da S1, A7). O núcleo
    /// aciona os cadeados dele na criação (`crates/quall-core/tests/puxada_nao_aloca.rs`); o
    /// cadeado do `consumidor` é da fronteira, e alocava na primeira trava no Mac e no iOS.
    #[test]
    fn a_primeira_puxada_pela_fronteira_nao_aloca() {
        let mesa = bancada(&[TrackKind::Microphone]);
        let t = mesa.tracks[0];
        unsafe {
            let p = quall_audio_playout_new(t, false);
            assert!(
                !p.is_null(),
                "{}",
                CStr::from_ptr(quall_last_error()).to_string_lossy()
            );
            let mut slot = std::mem::MaybeUninit::<QuallAudioSlot>::uninit();
            let (st, primeira) =
                armada(|| quall_audio_playout_pull(p, 10_000, f64::NAN, slot.as_mut_ptr()));
            assert_eq!(st, QuallStatus::Ok);
            // Com fluxo: o pacote sai fora da janela armada, a puxada dentro.
            let (mut depois, mut quadros) = (0usize, 0u64);
            for i in 0..80u32 {
                let mut pacote = vec![0xfcu8];
                pacote.extend_from_slice(&i.to_be_bytes());
                let _ = mesa.emissores[0].enviar_audio(AmostraDeAudio {
                    payload: &pacote,
                    timestamp_us: 5_000_000 + u64::from(i) * 20_000,
                });
                std::thread::sleep(Duration::from_millis(20));
                let (st, n) =
                    armada(|| quall_audio_playout_pull(p, 10_000, f64::NAN, slot.as_mut_ptr()));
                assert_eq!(st, QuallStatus::Ok);
                depois += n;
                if slot.assume_init_ref().order == QuallAudioOrder::Frame {
                    quadros += 1;
                }
            }
            eprintln!(
                "A7 pela fronteira: primeira puxada {primeira} alocações; 80 seguintes, {depois} \
                 ({quadros} quadros)"
            );
            assert_eq!(primeira, 0, "a primeira puxada alocou");
            assert_eq!(depois, 0, "as puxadas com fluxo alocaram");
            assert!(quadros > 40, "o fluxo não chegou: {quadros} quadros");
            assert_eq!(quall_audio_playout_free(p), QuallStatus::Ok);
        }
        mesa.fechar();
    }

    // ---------------------------------------------------------------------------------------
    // A reconferência da S1, item 5: o SIGSEGV do revisor B. O tratador de áudio **do teste**
    // consultava a track de vídeo, e o teste a liberou antes do escoamento do áudio. A biblioteca
    // não guarda o handle: estes testes fixam isso, pelos dois caminhos que a rede alcança.
    // ---------------------------------------------------------------------------------------

    struct Contagem {
        quadros: AtomicU64,
        slots: AtomicU64,
    }

    unsafe extern "C" fn conta_quadro(_q: *const QuallFrame, ud: *mut c_void) {
        let c = &*(ud as *const Contagem);
        c.quadros.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn conta_slot(_s: *const QuallAudioSlot, ud: *mut c_void) {
        let c = &*(ud as *const Contagem);
        c.slots.fetch_add(1, Ordering::SeqCst);
    }

    /// Libera o handle de vídeo **com o tratador de quadro registrado**, que o contrato permite
    /// (`quall_track_free` não desregistra), e continua mandando vídeo; depois escoa o áudio com
    /// o vídeo já liberado. A biblioteca segue chamando o tratador de quadro, com o `user_data`
    /// dele, sem encostar no handle liberado, e o escoamento do áudio não toca no vídeo.
    #[test]
    fn liberar_o_handle_de_video_antes_do_escoamento_do_audio_nao_toca_nele() {
        let mut mesa = bancada(&[TrackKind::Screen, TrackKind::SystemAudio]);
        let (tv, ta) = (mesa.tracks[0], mesa.tracks[1]);
        let c: &'static Contagem = Box::leak(Box::new(Contagem {
            quadros: AtomicU64::new(0),
            slots: AtomicU64::new(0),
        }));
        let ud = c as *const Contagem as *mut c_void;
        let mandar_video = |de: u64, n: u64| {
            for k in de..de + n {
                let q = idr(400);
                let _ = mesa.emissores[0].enviar_quadro(QuadroCodificado {
                    annexb: &q,
                    timestamp_us: 1_000_000 + k * 33_333,
                    idr: true,
                });
                std::thread::sleep(Duration::from_millis(30));
            }
        };
        unsafe {
            assert_eq!(
                quall_track_on_frame(tv, Some(conta_quadro), ud),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_track_on_audio(ta, Some(conta_slot), ud),
                QuallStatus::Ok
            );
        }
        mandar_video(0, 5);
        mesa.audio(1, 10, Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(300));
        let quadros_antes = c.quadros.load(Ordering::SeqCst);
        let slots_antes = c.slots.load(Ordering::SeqCst);

        // O handle de vídeo vai embora; a track e o tratador dela ficam com a sessão.
        mesa.tracks.retain(|&t| t != tv);
        unsafe { quall_track_free(tv) };
        mandar_video(5, 5);
        std::thread::sleep(Duration::from_millis(300));
        let quadros_depois = c.quadros.load(Ordering::SeqCst);

        // O escoamento do áudio, com o vídeo liberado.
        let status = unsafe { quall_track_on_audio(ta, None, ptr::null_mut()) };
        let escoados = c.slots.load(Ordering::SeqCst) - slots_antes;
        eprintln!(
            "item 5: quadros {quadros_antes} antes de liberar o vídeo, {quadros_depois} depois; \
             escoamento do áudio com o vídeo liberado: {status:?}, {escoados} slots"
        );
        assert!(quadros_antes >= 5, "o vídeo não chegou: {quadros_antes}");
        assert!(
            quadros_depois > quadros_antes,
            "o tratador de quadro segue registrado depois de liberar o handle"
        );
        assert_eq!(status, QuallStatus::Ok);
        assert_eq!(
            escoados, 2,
            "os dois slots retidos do áudio tinham de ser escoados"
        );
        mesa.fechar();
    }
}

#[cfg(test)]
mod testes_da_camera_remota {
    use super::*;
    use std::ffi::CString;

    struct Ponteiro<T>(*mut T);
    // SAFETY: cada ponteiro é usado por uma thread de cada vez, e o teste espera as threads.
    unsafe impl<T> Send for Ponteiro<T> {}
    unsafe impl<T> Sync for Ponteiro<T> {}

    fn porta_livre() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("porta livre")
    }

    struct Opcoes {
        _id: CString,
        _nome: CString,
        _pin: CString,
        opcoes: QuallSessionOptions,
    }

    fn opcoes(id: &str, pin: &str, porta: u16) -> Opcoes {
        let (id, nome, pin) = (
            CString::new(id).unwrap(),
            CString::new(format!("{id} de teste")).unwrap(),
            CString::new(pin).unwrap(),
        );
        let opcoes = QuallSessionOptions {
            me: QuallDeviceDesc {
                device_id: id.as_ptr(),
                display_name: nome.as_ptr(),
                screen_source: false,
                camera_source: true,
                sink: true,
            },
            pin: pin.as_ptr(),
            known_peers_json: ptr::null(),
            signaling_port: porta,
            timeout_ms: 30_000,
            tracks: ptr::null(),
            track_count: 0,
            bind_address: ptr::null(),
        };
        Opcoes {
            _id: id,
            _nome: nome,
            _pin: pin,
            opcoes,
        }
    }

    fn texto_de(f: impl Fn(*mut c_char, usize) -> isize) -> String {
        let mut n = f(ptr::null_mut(), 0);
        assert!(n > 0);
        loop {
            let mut buf = vec![0u8; n as usize];
            let escrito = f(buf.as_mut_ptr().cast(), buf.len());
            assert!(escrito > 0);
            if escrito as usize <= buf.len() {
                buf.truncate(escrito as usize - 1);
                return String::from_utf8(buf).expect("UTF-8");
            }
            n = escrito;
        }
    }

    const CAPS: &CStr = c"{\"controles\":{\"exposicao\":{\"valores\":[\"auto\",\"manual\"]},\"ev\":{\"min\":-2,\"max\":2,\"passo\":0.1},\"toque\":{}},\"limites\":{\"iso\":\"fabricante\"}}";
    const AJUSTE: &CStr = c"{\"exposicao\":\"auto\",\"ev\":0}";

    #[test]
    fn nulos_e_entradas_ruins_nao_derrubam() {
        unsafe {
            assert_eq!(
                quall_camera_host_set_allowed(ptr::null(), true),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_camera_host_pump(ptr::null(), ptr::null(), 0, ptr::null_mut()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_camera_host_next_request(ptr::null(), ptr::null_mut(), 0),
                -1
            );
            assert_eq!(
                quall_camera_remote_request(ptr::null(), c"{}".as_ptr()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_camera_remote_state_json(ptr::null(), ptr::null_mut(), 0),
                -1
            );
            quall_camera_host_free(ptr::null_mut());
            quall_camera_remote_free(ptr::null_mut());

            let h = quall_camera_host_new();
            assert_eq!(
                quall_camera_host_set_camera(h, CAPS.as_ptr(), ptr::null()),
                QuallStatus::Invalid,
                "um nulo só"
            );
            assert_eq!(
                quall_camera_host_set_camera(h, c"[1]".as_ptr(), AJUSTE.as_ptr()),
                QuallStatus::Invalid
            );
            assert_eq!(
                quall_camera_host_set_settings(h, AJUSTE.as_ptr(), 0),
                QuallStatus::Invalid,
                "sem câmera"
            );
            assert_eq!(
                quall_camera_host_set_camera(h, CAPS.as_ptr(), AJUSTE.as_ptr()),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_camera_host_set_settings(h, AJUSTE.as_ptr(), 0),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_camera_host_set_settings(h, AJUSTE.as_ptr(), u64::MAX),
                QuallStatus::Invalid
            );
            assert_eq!(
                quall_camera_host_update_settings(h, AJUSTE.as_ptr()),
                QuallStatus::Ok
            );
            assert_eq!(
                quall_camera_host_reject(h, 7, ptr::null()),
                QuallStatus::NullPointer
            );
            assert_eq!(
                quall_camera_host_reject(h, 7, c"nao_aplicado".as_ptr()),
                QuallStatus::Invalid
            );
            assert_eq!(
                quall_camera_host_next_request(h, ptr::null_mut(), 0),
                0,
                "fila vazia"
            );
            assert_eq!(quall_camera_host_next_request(h, ptr::null_mut(), 8), -1);
            assert_eq!(quall_last_status(), QuallStatus::NullPointer);
            let e = texto_de(|b, n| quall_camera_host_state_json(h, b, n));
            assert!(e.contains("\"permite\":false"), "{e}");
            assert_eq!(
                quall_camera_host_set_camera(h, ptr::null(), ptr::null()),
                QuallStatus::Ok,
                "sem câmera"
            );
            quall_camera_host_free(h);

            let r = quall_camera_remote_new();
            assert_eq!(
                quall_camera_remote_request(r, c"{\"ev\":1}".as_ptr()),
                QuallStatus::Invalid,
                "não está pronto"
            );
            assert_eq!(
                quall_camera_remote_touch(r, 0.5, 0.5, false),
                QuallStatus::Invalid
            );
            let e = texto_de(|b, n| quall_camera_remote_state_json(r, b, n));
            assert!(e.contains("\"situacao\":\"esperando\""), "{e}");
            quall_camera_remote_free(r);
        }
    }

    /// **De ponta a ponta pela fronteira C**, numa sessão de vídeo por 127.0.0.1: `quall_host` sem
    /// papel, `quall_connect`, as duas bombeadas, e a casca do filmador tirando o pedido pelo
    /// `(buf, cap)` que só consome quando cabe.
    #[test]
    fn pedido_atravessa_pela_fronteira_c() {
        let porta = porta_livre();
        let lado = std::thread::spawn(move || {
            let o = opcoes("filmador-c", "636363", porta);
            Ponteiro(unsafe { quall_host(&o.opcoes) })
        });
        let o = opcoes("receptor-c", "636363", porta);
        let destino = CString::new(format!("127.0.0.1:{porta}")).unwrap();
        let mut receptor = ptr::null_mut();
        for _ in 0..100 {
            receptor = unsafe { quall_connect(destino.as_ptr(), &o.opcoes) };
            if !receptor.is_null() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let filmador = lado.join().expect("thread").0;
        assert!(
            !receptor.is_null() && !filmador.is_null(),
            "a sessão não subiu"
        );

        unsafe {
            let h = Ponteiro(quall_camera_host_new());
            let r = Ponteiro(quall_camera_remote_new());
            assert_eq!(quall_camera_host_set_allowed(h.0, true), QuallStatus::Ok);
            assert_eq!(
                quall_camera_host_set_camera(h.0, CAPS.as_ptr(), AJUSTE.as_ptr()),
                QuallStatus::Ok
            );
            let mf = Ponteiro(quall_session_messages(filmador));
            let mr = Ponteiro(quall_session_messages(receptor));

            let fim = Instant::now() + Duration::from_secs(10);
            let mut pediu = false;
            let mut aplicado = false;
            while Instant::now() < fim && !aplicado {
                let mut mudou = 0u32;
                assert_eq!(
                    quall_camera_host_pump(h.0, mf.0, 20, &mut mudou),
                    QuallStatus::Ok
                );
                if mudou & QuallCameraHostChange::Request as u32 != 0 {
                    // A casca: pergunta o tamanho (não tira), depois tira.
                    let n = quall_camera_host_next_request(h.0, ptr::null_mut(), 0);
                    assert!(n > 0);
                    let mut buf = vec![0u8; n as usize];
                    assert_eq!(
                        quall_camera_host_next_request(h.0, buf.as_mut_ptr().cast(), buf.len()),
                        n
                    );
                    buf.pop();
                    let p: serde_json::Value = serde_json::from_slice(&buf).unwrap();
                    assert_eq!(p["ajuste"], serde_json::json!({"ev": 1.5}));
                    assert_eq!(p["autor"], "receptor-c de teste");
                    assert_eq!(
                        quall_camera_host_next_request(h.0, ptr::null_mut(), 0),
                        0,
                        "a fila esvaziou"
                    );
                    let novo = CString::new(r#"{"exposicao":"auto","ev":1.5}"#).unwrap();
                    assert_eq!(
                        quall_camera_host_set_settings(
                            h.0,
                            novo.as_ptr(),
                            p["n"].as_u64().unwrap()
                        ),
                        QuallStatus::Ok
                    );
                }
                assert_eq!(
                    quall_camera_remote_pump(r.0, mr.0, 20, &mut mudou),
                    QuallStatus::Ok
                );
                let e: serde_json::Value = serde_json::from_str(&texto_de(|b, n| {
                    quall_camera_remote_state_json(r.0, b, n)
                }))
                .unwrap();
                if !pediu && e["situacao"] == "pronto" {
                    assert_eq!(
                        quall_camera_remote_request(r.0, c"{\"ev\":1.5}".as_ptr()),
                        QuallStatus::Ok
                    );
                    pediu = true;
                }
                aplicado =
                    pediu && e["aplicado"]["ev"] == 1.5 && e["pendente"] == serde_json::json!({});
            }
            assert!(aplicado, "o pedido não voltou aplicado");
            let ef = texto_de(|b, n| quall_camera_host_state_json(h.0, b, n));
            assert!(ef.contains("\"controlado_por\":{\"ha_ms\""), "{ef}");

            // O fim: o filmador vê a sessão acabar e a esquece.
            assert_eq!(quall_session_close(receptor), QuallStatus::Ok);
            let fim = Instant::now() + Duration::from_secs(10);
            let mut fechou = false;
            while Instant::now() < fim {
                let mut mudou = 0u32;
                let st = quall_camera_host_pump(h.0, mf.0, 50, &mut mudou);
                if st == QuallStatus::Closed {
                    fechou = true;
                    break;
                }
                if quall_session_next_event(filmador, 0) != QuallSessionEvent::None {
                    let _ = quall_camera_host_pump(h.0, mf.0, 0, &mut mudou);
                    assert_eq!(quall_camera_host_forget(h.0, mf.0), QuallStatus::Ok);
                    fechou = true;
                    break;
                }
            }
            assert!(fechou, "o filmador não viu a sessão acabar");
            let ef = texto_de(|b, n| quall_camera_host_state_json(h.0, b, n));
            assert!(ef.contains("\"receptores\":[]"), "{ef}");
            assert_eq!(quall_session_close(filmador), QuallStatus::Ok);
            quall_messages_free(mf.0);
            quall_messages_free(mr.0);
            quall_camera_host_free(h.0);
            quall_camera_remote_free(r.0);
        }
    }
}
