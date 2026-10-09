//! **A câmera do PC como origem da cadeia** (`docs/camera-no-windows.md`, fase 3).
//!
//! É o par de `capture.rs` (a tela pelo WGC) e de `sintetica.rs` (a textura nossa): a mesma caixa
//! postal de uma posição, os mesmos métodos que a `Cadeia` lê (`Captura::Camera`). O que muda é de
//! onde o quadro vem, e as regras que o desenho revisado pediu, cada uma com o motivo no lugar:
//!
//! - **O leitor é assíncrono** (`MF_SOURCE_READER_ASYNC_CALLBACK`, §3.1): o `ReadSample` síncrono
//!   pode não voltar nunca (agosto, achados 2 e 3), e o retorno assíncrono é o que traz os eventos
//!   de dispositivo. O retorno **sempre** pede o próximo `ReadSample`, também quando chega sem
//!   amostra ou com `MF_SOURCE_READERF_STREAMTICK` (a revisão, m4): senão a captura para calada.
//! - **O tipo nativo é escolhido e fixado** com `IMFSourceReaderEx::SetNativeMediaType`, na ordem
//!   da revisão (M7): fps antes da área, área antes do subtipo (`regras_da_camera`). O leitor nunca
//!   escala nem converte cor (sem `ENABLE_ADVANCED_VIDEO_PROCESSING`): o processador dele recarimba
//!   numa grade de 33,33 ms e atrasa um quadro (M9). Quem converte é `conversor_de_camera.rs`.
//! - **A troca de tipo** (`MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED`) não derruba a abertura: antes
//!   do primeiro quadro o tipo é relido e a geometria sai dele; no meio do fluxo a captura só acaba
//!   se o formato ou a geometria mudaram (a revisão do código da fase 3, M2).
//! - **A geometria é a da abertura** (`MF_MT_MINIMUM_DISPLAY_APERTURE`), e a cópia leva só o
//!   retângulo dela: um decodificador que entrega 1920×1088 com 1080 linhas de imagem não põe 8
//!   linhas de enchimento no encoder (M3).
//! - **O quadro é copiado na chegada para um anel nosso** (a revisão, G1): a amostra do leitor sai
//!   de um pool e volta a ele quando é solta; esperar crédito do encoder segurando a amostra deixa o
//!   pool reescrever a superfície debaixo do quadro. A cópia lê **o subrecurso certo**
//!   (`IMFDXGIBuffer::GetSubresourceIndex`), e a amostra é solta logo depois. O carimbo, a origem e
//!   o ritmo são decididos **antes** de tomar uma posição do anel: o quadro descartado não gasta
//!   posição nem cópia (m5). Cópias que falham em seguida acabam a captura com o motivo (M3). **O
//!   contexto é despachado (`Flush`) antes de soltar a amostra**: a cópia vai para a GPU antes de a
//!   superfície voltar ao pool (a T1 da revisão; não observada, M41 e M42, e a defesa é barata).
//! - **O carimbo** é o `MFSampleExtension_DeviceTimestamp`, depois o `GetSampleTime` plausível,
//!   depois a chegada, decidido uma vez por fluxo e monotônico (`regras_da_camera::Carimbador`).
//! - **A câmera em uso** (decisão do Bruno de 18/09): tenta como controladora e, se ela falhar ou
//!   **demorar** mais que `ESPERA_DO_PRIMEIRO_QUADRO` (5 s, contados depois da criação da fonte, e
//!   olhando o Parar), solta a fonte e o leitor, recria a fonte pelo
//!   link — nunca pelo mesmo `IMFActivate`, que devolveria a instância guardada e ignoraria o
//!   atributo novo — e cai para o modo compartilhado do Frame Server. Câmera lenta não é câmera
//!   ocupada (m9), e "em uso por outro app" só aparece com o código que diz isso (M2).
//! - **O fim** tem testemunhas: erro do leitor, evento com falha, fim do fluxo, formato que muda. A
//!   interface é a outra testemunha, e mora em `cameras::interface_habilitada` com
//!   `regras_da_camera::TestemunhaDaInterface`. **Sem quadro por 3 s a câmera está parada, e não
//!   acabou** (a decisão do Bruno de 21/09; até 22/09 isso encerrava a sessão):
//!   [`CapturaDeCamera::parada_ha`], e a cadeia repete a posição do anel que ficou intacta
//!   ([`CapturaDeCamera::quadro_para_repetir`]).
//! - **Uma câmera, uma vez por processo** (o R4, M51): dentro do mesmo processo o Frame Server não
//!   recusa a segunda controladora, ele **toma** a câmera da primeira (`0xC00D3EA3`). A abertura
//!   pelo link espera a vez dela ([`esperar_a_vez`]) e, se outra captura do processo já tem a
//!   câmera, abre **compartilhada direto** (`regras_da_camera::AberturasDoProcesso`).
//! - **A soltura sai da thread da sessão** (o R4, M51): soltar o leitor de uma fonte que o Frame
//!   Server segura espera por ele — 16,9 s no R4, com a sessão presa e o receptor olhando a imagem
//!   congelada; o M54 mediu que a espera é no `Release` do leitor (19,9 s), e não no `Shutdown` da
//!   fonte. O leitor e a fonte são soltos numa thread própria, com o tempo no registro.
//! - **A criação da fonte pelo link corre numa thread própria** (M54): ~4,6 s num nó recém-criado,
//!   numa chamada só; a abertura espera em fatias que olham o Parar e batem o sinal de vida da
//!   sessão.
//!
//! **Nenhum quadro é gravado nem aberto aqui.** Entra amostra, sai textura nossa para o encoder.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use windows::core::{implement, Interface, Ref, Result as WinResult, GUID, HRESULT, HSTRING};
use windows::Win32::Foundation::{GetLastError, ERROR_FILE_NOT_FOUND};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_DECODER, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_WRITE, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_YUY2, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Pipes::WaitNamedPipeW;

use crate::capture::CapturedFrame;
use crate::registro;
use crate::desentrelacador::{Desentrelacador, Ordem};
use crate::regras_da_camera::{
    self as regras, AberturasDoProcesso, CausaDaFalha, Carimbador, Chegada, FalhaDaTentativa, FimDaCamera, Geometria,
    JanelaDaCamera, Papel, PlanoDaAbertura, RelogiosDoFluxo, Subtipo, TetoDaCamera, TipoNativo,
};

/// `MF_DEVSOURCE_ATTRIBUTE_FRAMESERVER_SHARE_MODE` (`mfidl.h` do SDK 10.0.26100, lido no Dell em
/// 18/09). Não existe no crate `windows` 0.62.
pub const MF_DEVSOURCE_ATTRIBUTE_FRAMESERVER_SHARE_MODE: GUID = GUID::from_u128(0x44d1a9bc_2999_4238_ae43_0730ceb2ab1b);

/// Quantas texturas o anel tem: a caixa postal (1), o pendente da cadeia (1), o que o MFT
/// segura (2–3) e uma de folga. O mesmo número do anel do WGC (`capture.rs`).
const TAMANHO_DO_ANEL: usize = 6;

/// **Bancada, fase 5** (`--sem-desentrelacar` do app): a câmera entrelaçada segue como veio, para o
/// A/B de olho e de custo. O produto deixa em `false`.
pub static SEM_DESENTRELACAR: AtomicBool = AtomicBool::new(false);

/// **Bancada, 22/09** (`--desentrelacador bob` do app): o DV volta ao caminho da fase 5 — o leitor
/// com o gerenciador D3D e o bob do processador de vídeo —, para o A/B contra o adapt2 (o padrão:
/// `desentrelacador.rs`, na cópia para o anel). O `--sem-desentrelacar` manda sobre os dois.
pub static DESENTRELACADOR_BOB: AtomicBool = AtomicBool::new(false);

/// O leitor do DV sem o gerenciador falhou uma vez neste processo (a revisão do código, achado 1):
/// daí em diante, o DV abre com o gerenciador e o bob, sem gastar outra tentativa.
static DV_SEM_D3D_FALHOU: AtomicBool = AtomicBool::new(false);

/// O adapt2 foi pedido (nem `--desentrelacador bob` nem `--sem-desentrelacar`), e o leitor sem D3D
/// do DV não falhou neste processo.
fn adapt2_pedido() -> bool {
    !DESENTRELACADOR_BOB.load(Ordering::SeqCst) && !SEM_DESENTRELACAR.load(Ordering::SeqCst) && !DV_SEM_D3D_FALHOU.load(Ordering::SeqCst)
}

/// **Bancada** (`--pausar-camera` do app): as janelas, contadas da criação do estado de cada
/// **tentativa** de abertura (a da compartilhada, depois de uma controladora que falhou, começa
/// de novo), em que o que o leitor entrega é descartado antes da caixa, como se a câmera tivesse
/// parado (`regras::ler_pausas_de_bancada`). Simula a pausa **na nossa fronteira**: o leitor segue
/// entregando, e o driver de uma câmera de verdade não é exercitado. O produto deixa vazio, e
/// paga só a leitura de [`HA_PAUSAS_DE_BANCADA`] por amostra. Use [`configurar_pausas_de_bancada`].
pub static PAUSAS_DE_BANCADA: Mutex<Vec<(Duration, Duration)>> = Mutex::new(Vec::new());
pub static HA_PAUSAS_DE_BANCADA: AtomicBool = AtomicBool::new(false);

/// Liga as pausas de bancada (`--pausar-camera`).
pub fn configurar_pausas_de_bancada(pausas: Vec<(Duration, Duration)>) {
    HA_PAUSAS_DE_BANCADA.store(!pausas.is_empty(), Ordering::SeqCst);
    *PAUSAS_DE_BANCADA.lock().unwrap_or_else(|e| e.into_inner()) = pausas;
}

/// De onde a câmera vem.
#[derive(Clone, Debug)]
pub enum FonteDaCamera {
    /// Uma câmera do catálogo, pelo link simbólico (`cameras.rs`): a identidade guardada (§2.2).
    Link(String),
    /// **Bancada**: a fonte do Quall instanciada **neste** processo (sem nó e sem Frame Server), com
    /// um nome aleatório no repositório dela, e portanto um cano que ninguém serve: ela entrega o
    /// padrão de bancada, que é nosso. É a origem da prova sem câmera de ninguém (a `--direto` da
    /// sonda, e a `--camera-sintetica` do emissor).
    ///
    /// Com `regua`, **este processo serve o cano** do nome aleatório com o quadro de bancada com a
    /// régua (`regua_de_bancada`), depois de conferir que ninguém o serve: a fonte entrega um quadro
    /// nosso com o número dele, e o receptor com `--regua` confere o pixel (a fase 4).
    DoQuallNoProcesso { regua: bool },
}

impl FonteDaCamera {
    pub fn descricao(&self) -> String {
        match self {
            FonteDaCamera::Link(l) => format!("câmera {l}"),
            FonteDaCamera::DoQuallNoProcesso { regua: false } => "a fonte do Quall neste processo (padrão de bancada)".into(),
            FonteDaCamera::DoQuallNoProcesso { regua: true } => {
                "a fonte do Quall neste processo, com o cano servido aqui (padrão de bancada com a régua)".into()
            }
        }
    }
}

/// Em que modo do Frame Server a câmera abriu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modo {
    /// Escolhe o tipo nativo; outro controlador leva `0xC00D3704`.
    Controladora,
    /// Entra ao lado de quem controla, com o tipo que estiver em uso (documentação oficial, M16).
    Compartilhada,
    /// A fonte do Quall no processo: não há Frame Server.
    NoProcesso,
}

/// O QPC em unidades de 100 ns — o domínio do `MFGetSystemTime` e do `DeviceTimestamp`.
pub fn qpc_100ns() -> i64 {
    let mut c = 0i64;
    let mut f = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f == 0 {
        return 0;
    }
    ((c as i128) * 10_000_000 / (f as i128)) as i64
}

/// O cano existe (alguém o serve)? `WaitNamedPipeW` responde sem conectar: abrir o cano para "ver
/// se existe" contaria como cliente para quem o serve.
pub fn cano_existe(cano: &str) -> bool {
    let nome = HSTRING::from(cano);
    if unsafe { WaitNamedPipeW(&nome, 1) }.as_bool() {
        return true;
    }
    let erro = unsafe { GetLastError() };
    erro != ERROR_FILE_NOT_FOUND
}

// =============================================================================================
// O tipo de saída
// =============================================================================================

/// O formato em que o leitor entrega.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatoDoLeitor {
    Nv12,
    Yuy2,
    /// **O I420 da Canon** (a fase 5): o leitor entrega o nativo em memória, sem conversor (não há
    /// textura DXGI I420, e pedir outro formato ao leitor pediria o processador dele, que recarimba
    /// numa grade: M9). A cópia para o anel entrelaça U e V: **o anel é NV12**.
    I420,
}

impl FormatoDoLeitor {
    /// O formato **do anel** (e do preparo): o do leitor, menos o I420, que vira NV12 na cópia.
    pub fn dxgi(self) -> DXGI_FORMAT {
        match self {
            FormatoDoLeitor::Nv12 | FormatoDoLeitor::I420 => DXGI_FORMAT_NV12,
            FormatoDoLeitor::Yuy2 => DXGI_FORMAT_YUY2,
        }
    }

    /// O subtipo **do que fica no anel**, que é o que o conversor e o encoder veem.
    pub fn subtipo(self) -> Subtipo {
        match self {
            FormatoDoLeitor::Nv12 | FormatoDoLeitor::I420 => Subtipo::Nv12,
            FormatoDoLeitor::Yuy2 => Subtipo::Yuy2,
        }
    }

    /// O subtipo que se pede ao leitor.
    fn guid(self) -> GUID {
        match self {
            FormatoDoLeitor::Nv12 => MFVideoFormat_NV12,
            FormatoDoLeitor::Yuy2 => MFVideoFormat_YUY2,
            FormatoDoLeitor::I420 => MFVideoFormat_I420,
        }
    }
}

fn subtipo_de(g: &GUID) -> Subtipo {
    if *g == MFVideoFormat_NV12 {
        Subtipo::Nv12
    } else if *g == MFVideoFormat_YUY2 {
        Subtipo::Yuy2
    } else if *g == MFVideoFormat_MJPG {
        Subtipo::Mjpg
    } else if *g == MFVideoFormat_I420 {
        Subtipo::I420
    } else if [MFVideoFormat_DVSD, MFVideoFormat_DV25, MFVideoFormat_DVSL].contains(g) {
        // Só o DV de definição padrão (a revisão do código da fase 5, L9): o `dv50`, o `dvh1` e o
        // `dvhd` ganhariam pela área e talvez o `mfdvdec` não os decodifique; ninguém mediu.
        Subtipo::Dv
    } else {
        Subtipo::Outro
    }
}

unsafe fn tipo_nativo_de(t: &IMFMediaType) -> TipoNativo {
    unsafe {
        let sub = t.GetGUID(&MF_MT_SUBTYPE).unwrap_or(GUID::zeroed());
        let tam = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        let fps = t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0);
        TipoNativo {
            subtipo: subtipo_de(&sub),
            largura: (tam >> 32) as u32,
            altura: tam as u32,
            fps_num: (fps >> 32) as u32,
            fps_den: fps as u32,
        }
    }
}

/// **O subtipo que a abertura vai usar, lido do descritor da fonte**, antes de haver leitor: o da
/// escolha (controladora) ou o tipo em uso (compartilhada), do primeiro fluxo de vídeo. `None` quando
/// o descritor não responde: aí o leitor é o de antes, com o gerenciador D3D.
fn subtipo_pelo_descritor(fonte: &IMFMediaSource, modo: Modo, teto: TetoDaCamera) -> Option<Subtipo> {
    unsafe {
        let pd = fonte.CreatePresentationDescriptor().ok()?;
        let n = pd.GetStreamDescriptorCount().ok()?;
        for i in 0..n {
            let mut selecionado = windows::core::BOOL::default();
            let mut sd: Option<IMFStreamDescriptor> = None;
            if pd.GetStreamDescriptorByIndex(i, &mut selecionado, &mut sd).is_err() {
                continue;
            }
            let Some(h) = sd.and_then(|s| s.GetMediaTypeHandler().ok()) else { continue };
            if h.GetMajorType().ok() != Some(MFMediaType_Video) {
                continue;
            }
            if modo == Modo::Compartilhada {
                return h.GetCurrentMediaType().ok().map(|t| tipo_nativo_de(&t).subtipo);
            }
            let total = h.GetMediaTypeCount().ok()?;
            let tipos: Vec<TipoNativo> = (0..total.min(512)).filter_map(|k| h.GetMediaTypeByIndex(k).ok()).map(|t| tipo_nativo_de(&t)).collect();
            return regras::escolher_tipo_nativo(&tipos, teto).map(|k| tipos[k].subtipo);
        }
        None
    }
}

/// Um `MFVideoArea` de um atributo do tipo, se existir.
unsafe fn abertura_do_tipo(t: &IMFMediaType, chave: &GUID) -> Option<(i32, i32, i32, i32)> {
    let mut blob = [0u8; 16];
    let mut tamanho = 0u32;
    unsafe { t.GetBlob(chave, &mut blob, Some(&mut tamanho)) }.ok()?;
    if tamanho < 16 {
        return None;
    }
    regras::abertura_do_blob(&blob)
}

/// O subtipo e a geometria de um tipo de saída: o quadro de `MF_MT_FRAME_SIZE`, e a imagem da
/// abertura mínima, ou da geométrica, ou o quadro inteiro (M3).
unsafe fn saida_de(t: &IMFMediaType) -> (GUID, Geometria) {
    unsafe {
        let sub = t.GetGUID(&MF_MT_SUBTYPE).unwrap_or(GUID::zeroed());
        let tam = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        let (l, a) = ((tam >> 32) as u32, tam as u32);
        let abertura = abertura_do_tipo(t, &MF_MT_MINIMUM_DISPLAY_APERTURE)
            .or_else(|| abertura_do_tipo(t, &MF_MT_GEOMETRIC_APERTURE));
        (sub, regras::geometria(l, a, abertura))
    }
}

// =============================================================================================
// O retorno do leitor
// =============================================================================================

/// Uma amostra recebida, com o que a chegada diz sobre o tempo.
struct Recebido {
    amostra: IMFSample,
    instante: Instant,
    qpc_100ns: i64,
    tempo_100ns: i64,
}

// SAFETY: o Media Foundation é de apartamento multithread; a amostra e o leitor são objetos
// livres de apartamento, e cada um só é usado por uma thread de cada vez (a caixa tem cadeado).
unsafe impl Send for Recebido {}
struct LeitorEnviavel(IMFSourceReader);
unsafe impl Send for LeitorEnviavel {}

/// O que o retorno do leitor e a sessão dividem.
struct Compartilhado {
    caixa: Mutex<Option<Recebido>>,
    aviso: Sender<()>,
    leitor: Mutex<Option<LeitorEnviavel>>,
    erro: Mutex<Option<String>>,
    /// O código da falha, quando ela tem um (o `HRESULT` do leitor ou do evento).
    codigo: Mutex<Option<u32>>,
    /// A câmera saiu (o leitor com `0xC00D3EA2`, ou o evento de dispositivo removido).
    removida: AtomicBool,
    parado: AtomicBool,
    /// A primeira resposta (quadro ou falha), para a abertura esperar.
    resposta: Mutex<Option<std::result::Result<(), String>>>,
    tem_resposta: Condvar,
    /// O formato e a geometria do anel, depois do primeiro quadro: uma troca de tipo que os mude
    /// acaba a captura; uma que não mude segue (M2). `None` antes: a abertura relê o tipo.
    esperado: Mutex<Option<(GUID, Geometria)>>,
    ultimo_chegado: Mutex<Option<Instant>>,
    /// Num `Arc` próprio: a thread dos ajustes o lê (o vigia da pouca luz, `ajustes_da_camera.rs`)
    /// sem segurar o estado inteiro, com o leitor e a caixa dentro.
    chegados: Arc<AtomicU64>,
    sobrescritos: AtomicU64,
    vazios: AtomicU64,
    ticks: AtomicU64,
    trocas_de_tipo: AtomicU64,
    /// A troca de tipo que acabou a captura mudou formato ou geometria: a frase da tela diz "mudou
    /// de formato", e não "parou" (a câmera que volta de uma pausa noutro formato).
    formato_mudou: AtomicBool,
    /// Quando este estado nasceu: a origem das janelas de [`PAUSAS_DE_BANCADA`].
    nascido: Instant,
    /// As amostras descartadas pelas pausas de bancada.
    descartados_na_bancada: AtomicU64,
    /// **O aspecto** (a fase 5, o DV em 16:9): o subtipo nativo e a geometria de onde ele se
    /// decide, e o aspecto em uso; relido a cada troca de tipo, de saída ou nativa. A ordem entre a
    /// abertura e as releituras está em `regras::AspectoDaSessao` (a revisão curta, A4).
    base_do_aspecto: Mutex<Option<(Subtipo, Geometria)>>,
    aspecto: Mutex<regras::AspectoDaSessao>,
    /// O prefixo da sessão que abriu (`[#2] `): as linhas que saem da thread do Media Foundation
    /// não têm prefixo, e no R4 a falha da [#1] saiu sem dizer de quem era.
    prefixo: String,
}

/// **O aspecto que o leitor diz agora**: a PAR do tipo de saída, a PAR e o pacote VAUX de controle
/// do tipo nativo em uso, pela regra (`regras::aspecto_da_camera`).
fn aspecto_do_leitor(leitor: &IMFSourceReader, nativo: Subtipo, g: Geometria) -> regras::Aspecto {
    let fluxo = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    unsafe {
        let saida = leitor.GetCurrentMediaType(fluxo).ok();
        let em_uso = leitor.GetNativeMediaType(fluxo, MF_SOURCE_READER_CURRENT_TYPE_INDEX.0 as u32).ok();
        let par = |t: &Option<IMFMediaType>| t.as_ref().and_then(|t| t.GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO).ok()).map(regras::Par::do_mf);
        let vaux = em_uso.as_ref().and_then(|t| t.GetUINT32(&MF_MT_DV_VAUX_CTRL_PACK).ok());
        // A regra do DV olha o tamanho do **quadro** (a revisão do código da fase 5, L2).
        regras::aspecto_da_camera(nativo, g.quadro_largura, g.quadro_altura, par(&saida), par(&em_uso), vaux)
    }
}

impl Compartilhado {
    /// Uma linha de registro com o prefixo da sessão, também quando sai da thread do leitor.
    fn linha(&self, texto: String) {
        if registro::prefixo_desta_thread().is_empty() && !self.prefixo.is_empty() {
            registro::linha(format!("{}{texto}", self.prefixo));
        } else {
            registro::linha(texto);
        }
    }

    fn responder(&self, r: std::result::Result<(), String>) {
        let mut g = self.resposta.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(r);
            self.tem_resposta.notify_all();
        }
    }

    fn falhar(&self, motivo: String) {
        self.falhar_com(motivo, None);
    }

    fn falhar_com(&self, motivo: String, codigo: Option<u32>) {
        self.falhar_de_vez(motivo, codigo, false);
    }

    /// A troca de tipo que muda formato ou geometria: a frase da tela diz "mudou de formato". A
    /// bandeira só vale se esta for a **primeira** falha (a revisão do código de 22/09, 5: um fim
    /// anterior, como o do conversor travado, não ganha a frase do formato).
    fn falhar_por_formato(&self, motivo: String) {
        self.falhar_de_vez(motivo, None, true);
    }

    fn falhar_de_vez(&self, motivo: String, codigo: Option<u32>, formato: bool) {
        {
            let mut e = self.erro.lock().unwrap_or_else(|e| e.into_inner());
            if e.is_none() {
                if formato {
                    self.formato_mudou.store(true, Ordering::SeqCst);
                }
                self.linha(format!("câmera: {motivo}"));
                *e = Some(motivo.clone());
                *self.codigo.lock().unwrap_or_else(|e| e.into_inner()) = codigo;
                if codigo == Some(regras::HR_REMOVIDA) {
                    self.removida.store(true, Ordering::SeqCst);
                }
            }
        }
        self.responder(Err(motivo));
        let _ = self.aviso.try_send(());
    }

    fn leitor(&self) -> Option<IMFSourceReader> {
        self.leitor.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|l| l.0.clone())
    }

    /// **Relê o aspecto depois de uma troca de tipo** (a fase 5: o Bruno muda 16:9 ↔ 4:3 com a
    /// câmera transmitindo). Se mudou, guarda e conta: a cadeia encaixa o quadro novo na saída
    /// (`ConversorDeCamera::encaixar`). Antes da base não há o que reler: a abertura decide. O
    /// leitor é lido **sem** a trava do aspecto (a thread dele pode estar esperando por ela), e a
    /// ordem com a abertura é a de `regras::AspectoDaSessao` (a revisão curta, A4).
    fn reler_o_aspecto(&self) {
        let Some((nativo, g)) = *self.base_do_aspecto.lock().unwrap_or_else(|e| e.into_inner()) else { return };
        let Some(leitor) = self.leitor() else { return };
        let novo = aspecto_do_leitor(&leitor, nativo, g);
        let releitura = self.aspecto.lock().unwrap_or_else(|e| e.into_inner()).reler(novo);
        let descrever = |a: regras::Aspecto| format!("{:?} (PAR {}:{})", a.origem, a.par.num, a.par.den);
        match releitura {
            regras::Releitura::Igual => {}
            regras::Releitura::AntesDaAbertura => self.linha(format!(
                "câmera: uma troca de tipo durante a abertura; o aspecto relido, {}, é o que a abertura vai usar",
                descrever(novo)
            )),
            regras::Releitura::Mudou { n, antes } => self.linha(format!(
                "câmera: o aspecto mudou no meio da sessão ({n}ª vez): {} → {} (exibida {:?})",
                descrever(antes),
                descrever(novo),
                regras::tamanho_exibido(g.largura, g.altura, novo.par)
            )),
        }
    }

    fn pedir_o_proximo(&self) {
        if self.parado.load(Ordering::SeqCst) {
            return;
        }
        let g = self.leitor.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(l) = g.as_ref() {
            let r = unsafe { l.0.ReadSample(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0, None, None, None, None) };
            if let Err(e) = r {
                drop(g);
                self.falhar_com(format!("o ReadSample seguinte falhou: {e}"), Some(e.code().0 as u32));
            }
        }
    }

    /// Depois de `CURRENTMEDIATYPECHANGED`: `None` para seguir, ou o motivo para acabar (M2).
    fn conferir_o_tipo(&self) -> Option<String> {
        let n = self.trocas_de_tipo.fetch_add(1, Ordering::Relaxed) + 1;
        // Antes do primeiro quadro não há anel: a abertura relê o tipo e dimensiona por ele.
        let esperado = (*self.esperado.lock().unwrap_or_else(|e| e.into_inner()))?;
        let leitor = self.leitor()?;
        match unsafe { leitor.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) } {
            Ok(t) => {
                let agora = unsafe { saida_de(&t) };
                if agora == esperado {
                    if n <= 3 {
                        self.linha(format!(
                            "câmera: o tipo mudou sem mudar formato nem geometria ({}); segue",
                            agora.1.descricao()
                        ));
                    }
                    None
                } else {
                    Some(format!(
                        "o formato da câmera mudou no meio ({:?} {} → {:?} {}): a transmissão precisa recomeçar",
                        subtipo_de(&esperado.0),
                        esperado.1.descricao(),
                        subtipo_de(&agora.0),
                        agora.1.descricao()
                    ))
                }
            }
            Err(e) => Some(format!("o tipo da câmera mudou e não deu para relê-lo ({e})")),
        }
    }
}

#[implement(IMFSourceReaderCallback)]
struct Retorno {
    estado: Arc<Compartilhado>,
}

impl IMFSourceReaderCallback_Impl for Retorno_Impl {
    fn OnReadSample(
        &self,
        hrstatus: HRESULT,
        _dwstreamindex: u32,
        dwstreamflags: u32,
        lltimestamp: i64,
        psample: Ref<'_, IMFSample>,
    ) -> WinResult<()> {
        let instante = Instant::now();
        let qpc = qpc_100ns();
        let e = &self.estado;
        if e.parado.load(Ordering::SeqCst) {
            return Ok(());
        }
        if hrstatus.is_err() {
            e.falhar_com(
                format!("o leitor devolveu {hrstatus:?} ({})", windows::core::Error::from(hrstatus).message()),
                Some(hrstatus.0 as u32),
            );
            return Ok(());
        }
        let bandeiras = dwstreamflags;
        if bandeiras & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            e.falhar("o leitor sinalizou erro (MF_SOURCE_READERF_ERROR)".into());
            return Ok(());
        }
        if bandeiras & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            e.falhar("o fluxo da câmera acabou (MF_SOURCE_READERF_ENDOFSTREAM)".into());
            return Ok(());
        }
        // A troca de tipo: absorvida antes do primeiro quadro e quando não muda formato nem
        // geometria; no resto, a configuração do encoder é fixa pela sessão e a captura acaba (M2,
        // R19). A amostra que veio junto segue.
        if bandeiras & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            if let Some(motivo) = e.conferir_o_tipo() {
                e.falhar_por_formato(motivo);
                return Ok(());
            }
        }
        // O aspecto pode mudar sem mudar formato nem geometria (o DV em 16:9 ↔ 4:3): relido a
        // cada troca, de saída ou nativa.
        if bandeiras & (MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 | MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0) as u32 != 0 {
            e.reler_o_aspecto();
        }
        // **Bancada** (`--pausar-camera`): a câmera "parada" por uma janela. O descarte vem antes da
        // caixa e do `ultimo_chegado` (senão a pausa nunca seria vista), e o próximo `ReadSample`
        // continua sendo pedido, como numa câmera que só parou de mandar.
        let pausada = psample.as_ref().is_some() && HA_PAUSAS_DE_BANCADA.load(Ordering::Relaxed) && {
            let pausas = PAUSAS_DE_BANCADA.lock().unwrap_or_else(|x| x.into_inner());
            !pausas.is_empty() && regras::na_pausa_de_bancada(&pausas, instante.saturating_duration_since(e.nascido))
        };
        if pausada {
            e.descartados_na_bancada.fetch_add(1, Ordering::Relaxed);
            e.pedir_o_proximo();
            return Ok(());
        }
        match psample.as_ref() {
            Some(amostra) => {
                let novo = Recebido { amostra: amostra.clone(), instante, qpc_100ns: qpc, tempo_100ns: lltimestamp };
                let velho = e.caixa.lock().unwrap_or_else(|x| x.into_inner()).replace(novo);
                if velho.is_some() {
                    e.sobrescritos.fetch_add(1, Ordering::Relaxed);
                }
                // A amostra velha volta ao pool aqui, fora do cadeado.
                drop(velho);
                // O `soltar` pode ter esvaziado a caixa entre a conferência de `parado` e o
                // `replace`: a amostra não fica presa até o leitor morrer (m8).
                if e.parado.load(Ordering::SeqCst) {
                    drop(e.caixa.lock().unwrap_or_else(|x| x.into_inner()).take());
                    return Ok(());
                }
                e.chegados.fetch_add(1, Ordering::Relaxed);
                *e.ultimo_chegado.lock().unwrap_or_else(|x| x.into_inner()) = Some(instante);
                e.responder(Ok(()));
                let _ = e.aviso.try_send(());
            }
            None => {
                e.vazios.fetch_add(1, Ordering::Relaxed);
                if bandeiras & MF_SOURCE_READERF_STREAMTICK.0 as u32 != 0 {
                    e.ticks.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        // **Sempre** o próximo: sem amostra, com marca de fluxo, ou com quadro.
        e.pedir_o_proximo();
        Ok(())
    }

    fn OnFlush(&self, _dwstreamindex: u32) -> WinResult<()> {
        Ok(())
    }

    fn OnEvent(&self, _dwstreamindex: u32, pevent: Ref<'_, IMFMediaEvent>) -> WinResult<()> {
        if let Some(ev) = pevent.as_ref() {
            let status = unsafe { ev.GetStatus() }.unwrap_or(HRESULT(0));
            let tipo = unsafe { ev.GetType() }.unwrap_or(0);
            // A câmera sumiu, ou outro app a tomou: os dois chegam como evento e podem vir com
            // status de sucesso, então o tipo basta.
            if tipo == MEVideoCaptureDeviceRemoved.0 as u32 {
                self.estado.falhar_com("a câmera foi removida (MEVideoCaptureDeviceRemoved)".into(), Some(regras::HR_REMOVIDA));
            } else if tipo == MEVideoCaptureDevicePreempted.0 as u32 {
                self.estado.falhar_com("outro app tomou a câmera (MEVideoCaptureDevicePreempted)".into(), Some(regras::HR_TOMADA));
            } else if status.is_err() {
                self.estado.falhar_com(format!("evento {tipo} da câmera com falha {status:?}"), Some(status.0 as u32));
            }
        }
        Ok(())
    }
}

// =============================================================================================
// A abertura
// =============================================================================================

pub struct CapturaDeCamera {
    pub frame_ready: Receiver<()>,
    estado: Arc<Compartilhado>,
    /// A fonte; `None` depois de a soltura sair para a thread dela.
    fonte: Option<IMFMediaSource>,
    dispositivo: ID3D11Device,
    contexto: ID3D11DeviceContext,
    anel: Vec<ID3D11Texture2D>,
    proxima: usize,
    /// A posição do anel que a cadeia pode repetir com a câmera parada (a última cópia boa, se
    /// nenhuma outra foi tomada depois dela).
    repetivel: regras::PosicaoRepetivel,
    staging: Option<ID3D11Texture2D>,
    carimbador: Carimbador,
    /// Os três relógios de cada quadro, contados sem decidir nada (a fase 5, §5).
    relogios: RelogiosDoFluxo,
    /// A linha de 5 s no registro: o fps que chega, o passo do aparelho e a idade (a fase 5).
    janela: JanelaDaCamera,
    ultimo_entregue: Option<Instant>,
    fps_do_teto: u32,
    aberta_em: Instant,
    /// O tamanho da **imagem** (a abertura): o do anel, do conversor e do encoder.
    pub width: u32,
    pub height: u32,
    /// O quadro que o leitor entrega e o retângulo da imagem dentro dele (M3).
    pub geometria: Geometria,
    pub formato: FormatoDoLeitor,
    /// O subtipo **nativo** que a câmera entrega (o MJPG passa pelo decodificador antes do anel).
    pub nativo: Subtipo,
    /// A faixa da entrada é completa? Do tipo, ou da regra (`regras_da_camera::faixa_completa`).
    pub faixa_completa: bool,
    /// A matriz declarada é a BT.709? Sem declaração, BT.601 (`regras_da_camera::matriz_709`).
    pub matriz_709: bool,
    /// Progressivo, ou os campos e a ordem deles, **da fonte** (o DV, a fase 5). Para o registro.
    pub entrelacamento: regras::Entrelacamento,
    /// **O entrelaçamento do quadro no anel**: o que o conversor tem de desentrelaçar. Progressivo
    /// quando o adapt2 já desentrelaçou na cópia (senão o bob correria de novo sobre ele).
    pub entrelacamento_no_anel: regras::Entrelacamento,
    /// Quem desentrelaça (22/09).
    pub quem_desentrelaca: regras::QuemDesentrelaca,
    /// O adapt2 desta captura (com `quem_desentrelaca` = `Cpu`): guarda o quadro anterior.
    desentrelacador: Option<Desentrelacador>,
    /// O custo do adapt2 na cópia (a CPU da thread da sessão): quadros, soma e máximo, em µs.
    custo_do_desentrelacador: (u64, u64, u64),
    pub modo: Modo,
    /// Para o registro: o tipo nativo, o de saída, a lista declarada e os tempos da abertura.
    pub descricao: String,
    /// Despachar o contexto (`Flush`) antes de soltar a amostra (a T1 da revisão). Ligado no
    /// produto; a sonda o desliga com `--sem-flush` para medir a hipótese sem a defesa.
    pub flush_antes_de_soltar: bool,
    recortes: u64,
    entregues: u64,
    fora_do_ritmo: u64,
    dxgi: u64,
    memoria: u64,
    falhas_de_copia: u64,
    falhas_seguidas: u32,
    de_outra_placa: u64,
    subrecursos: std::collections::BTreeSet<u32>,
    arrays: std::collections::BTreeSet<u32>,
    /// Quem serve o cano da sintética com régua: vai com a soltura, e cai depois da fonte que o
    /// lia.
    regua: Option<crate::regua_de_bancada::FonteDeRegua>,
    /// A captura viva desta câmera no processo (só pelo link): a abertura seguinte da mesma câmera
    /// abre compartilhada enquanto ela existir.
    vaga: Option<VagaNoProcesso>,
    /// **Os ajustes da câmera** (R9, `docs/controles-de-camera.md` §6): a thread que fala com o
    /// driver, só pelo link. Encerrada antes da soltura do leitor e da fonte.
    ajustes: Option<crate::ajustes_da_camera::AjustesDaCamera>,
}

/// O resultado de uma tentativa de abertura: tudo vivo e o primeiro quadro já chegou.
struct Aberta {
    fonte: IMFMediaSource,
    /// Quem serve o cano da sintética com régua: vive com a captura.
    regua: Option<crate::regua_de_bancada::FonteDeRegua>,
    estado: Arc<Compartilhado>,
    avisos: Receiver<()>,
    geometria: Geometria,
    formato: FormatoDoLeitor,
    nativo: Subtipo,
    completa_declarada: Option<bool>,
    matriz_709: bool,
    entrelacamento: regras::Entrelacamento,
    quem: regras::QuemDesentrelaca,
    modo: Modo,
    descricao: String,
    /// O fps do tipo nativo (o teto do obturador dos ajustes, `docs/controles-de-camera.md` §3.1).
    fps: f64,
}

/// Solta tudo de uma tentativa **nesta thread**: o retorno para de pedir, o leitor sai do estado
/// (o ciclo leitor → retorno → estado → leitor se desfaz), e a fonte é desligada. Só na abertura,
/// onde a tentativa seguinte precisa da anterior solta; a captura aberta solta fora da sessão
/// ([`soltar_em_outra_thread`]).
fn soltar(estado: &Arc<Compartilhado>, fonte: &IMFMediaSource) {
    estado.parado.store(true, Ordering::SeqCst);
    let leitor = estado.leitor.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(leitor);
    drop(estado.caixa.lock().unwrap_or_else(|e| e.into_inner()).take());
    unsafe {
        let _ = fonte.Shutdown();
    }
}

// =============================================================================================
// A vez de abrir, e a soltura fora da sessão
// =============================================================================================

/// As aberturas de câmera deste processo, e o aviso de quem espera a vez.
static ABERTURAS: Mutex<AberturasDoProcesso> = Mutex::new(AberturasDoProcesso::novo());
static VEZ: Condvar = Condvar::new();

fn aberturas() -> std::sync::MutexGuard<'static, AberturasDoProcesso> {
    ABERTURAS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A vez de abrir uma câmera pelo link: enquanto vive, as outras aberturas da mesma câmera no
/// processo esperam. Solta sem `abriu`, devolve a vez sem contar a câmera.
struct VezNaAbertura {
    link: String,
    abriu: Option<Papel>,
}

impl VezNaAbertura {
    /// A câmera abriu, neste papel: a vez vira uma vaga (uma captura viva do processo).
    fn abriu(mut self, papel: Papel) -> VagaNoProcesso {
        self.abriu = Some(papel);
        VagaNoProcesso { link: self.link.clone(), papel, soltando: false }
    }
}

impl Drop for VezNaAbertura {
    fn drop(&mut self) {
        aberturas().fim_da_abertura(&self.link, self.abriu);
        VEZ.notify_all();
    }
}

/// Uma captura viva do processo nesta câmera. Começa a soltar quando a sessão para, e sai da conta
/// quando a soltura termina (na thread da soltura).
struct VagaNoProcesso {
    link: String,
    papel: Papel,
    soltando: bool,
}

impl VagaNoProcesso {
    fn comecar_a_soltar(&mut self) {
        if !self.soltando {
            self.soltando = true;
            aberturas().comecou_a_soltar(&self.link, self.papel);
            VEZ.notify_all();
        }
    }
}

impl Drop for VagaNoProcesso {
    fn drop(&mut self) {
        self.comecar_a_soltar();
        aberturas().terminou_de_soltar(&self.link);
        VEZ.notify_all();
    }
}

/// **Espera a vez de abrir a câmera `link` neste processo**, olhando o Parar: enquanto outra sessão
/// do processo abre a mesma câmera, e, até [`regras::ESPERA_PELAS_SOLTURAS`], enquanto uma captura
/// dela está soltando. Devolve o plano: controladora com recuo, ou compartilhada direto.
fn esperar_a_vez(link: &str, parar: &dyn Fn() -> bool) -> std::result::Result<(PlanoDaAbertura, VezNaAbertura), FalhaDaTentativa> {
    let comeco = Instant::now();
    let mut avisou = false;
    let mut desistiu_das_solturas = false;
    let mut g = aberturas();
    loop {
        let esperar_solturas = comeco.elapsed() < regras::ESPERA_PELAS_SOLTURAS;
        if !esperar_solturas && !desistiu_das_solturas {
            desistiu_das_solturas = true;
            let (_, soltando, _) = g.estado(link);
            if soltando > 0 {
                registro::linha(format!(
                    "câmera: {soltando} soltura(s) desta câmera ainda em curso depois de {} s; a vez não espera mais por elas",
                    regras::ESPERA_PELAS_SOLTURAS.as_secs()
                ));
            }
        }
        if let Some(plano) = g.pedir_a_vez(link, esperar_solturas) {
            if avisou {
                registro::linha(format!("câmera: a vez de abrir veio em {} ms", comeco.elapsed().as_millis()));
            }
            return Ok((plano, VezNaAbertura { link: link.to_string(), abriu: None }));
        }
        if !avisou {
            avisou = true;
            let (vivas, soltando, abrindo) = g.estado(link);
            registro::linha(format!(
                "câmera: esperando a vez de abrir esta câmera neste processo (abrindo={abrindo} soltando={soltando} vivas={vivas}): \
                 no mesmo processo a segunda controladora toma a câmera da primeira (M51)"
            ));
        }
        if parar() {
            return Err(cancelada(&format!("na espera da vez de abrir ({} ms)", comeco.elapsed().as_millis())));
        }
        g = VEZ.wait_timeout(g, regras::FATIA_DA_ESPERA).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
    }
}

/// Quantas solturas de câmera estão em curso fora das sessões.
static SOLTURAS_PENDENTES: AtomicUsize = AtomicUsize::new(0);

/// Quantas solturas de câmera ainda estão em curso.
pub fn solturas_pendentes() -> usize {
    SOLTURAS_PENDENTES.load(Ordering::SeqCst)
}

/// Espera as solturas em curso até `prazo`, para o processo não desligar o Media Foundation no meio
/// de uma. Devolve quantas sobraram.
pub fn esperar_solturas(prazo: Duration) -> usize {
    let fim = Instant::now() + prazo;
    loop {
        let n = solturas_pendentes();
        if n == 0 || Instant::now() >= fim {
            return n;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// **Na saída do processo, antes do `MFShutdown`**: espera as solturas em curso (e as criações de
/// fonte que o Parar abandonou) **o tempo que for preciso, até `teto`**, com uma linha no registro a
/// cada 5 s. Devolve se o `MFShutdown` pode ser chamado: `false` quando alguma sobrou no teto — aí
/// o processo sai sem ele, como já sai sem o `cleanup` da libdatachannel com uma thread viva (a
/// revisão do código da fase 4, m2: com a DLL de 09/09 a soltura leva ~20 s, M54 e M58, e um
/// `MFShutdown` com uma thread dentro do `Release` do leitor pode travar a saída). Sem nenhuma em
/// curso, volta na hora e não diz nada.
pub fn esperar_solturas_na_saida(teto: Duration) -> bool {
    let antes = solturas_pendentes();
    if antes == 0 {
        return true;
    }
    let t0 = Instant::now();
    registro::linha(format!(
        "câmera: {antes} soltura(s) em curso na saída do processo; esperando antes do MFShutdown (até {} s)",
        teto.as_secs()
    ));
    let mut proximo_aviso = Duration::from_secs(5);
    loop {
        let n = solturas_pendentes();
        let passou = t0.elapsed();
        if n == 0 {
            registro::linha(format!("câmera: as solturas acabaram em {} ms; o MFShutdown segue", passou.as_millis()));
            return true;
        }
        if passou >= teto {
            registro::linha(format!(
                "câmera: !! {n} soltura(s) ainda em curso depois de {} s; o processo sai SEM o MFShutdown",
                teto.as_secs()
            ));
            return false;
        }
        if passou >= proximo_aviso {
            registro::linha(format!("câmera: {n} soltura(s) ainda em curso há {} s", passou.as_secs()));
            proximo_aviso += Duration::from_secs(5);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct FonteEnviavel(IMFMediaSource);
// SAFETY: o processo roda em MTA, e a fonte do Media Foundation é livre de apartamento; depois do
// `stop` só a thread da soltura a toca.
unsafe impl Send for FonteEnviavel {}

/// O que a soltura leva para a thread dela, na ordem em que solta: a amostra da caixa e o leitor,
/// a fonte (`Shutdown`), o cano da régua (depois da fonte que o lia) e a vaga no processo.
struct Soltura {
    /// A thread dos ajustes (R9): esperada antes de tudo, porque ela segura as interfaces da fonte e
    /// devolve a câmera como encontrou.
    ajustes: Option<std::thread::JoinHandle<()>>,
    caixa: Option<Recebido>,
    leitor: Option<LeitorEnviavel>,
    fonte: FonteEnviavel,
    regua: Option<crate::regua_de_bancada::FonteDeRegua>,
    vaga: Option<VagaNoProcesso>,
    prefixo: String,
}

impl Soltura {
    fn soltar(self) {
        let Soltura { ajustes, caixa, leitor, fonte, regua, vaga, prefixo } = self;
        registro::prefixar_esta_thread(&prefixo);
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if let Some(h) = ajustes {
            let t = Instant::now();
            let _ = h.join();
            registro::linha(format!("câmera: a thread dos ajustes acabou em {} ms (antes do leitor e da fonte)", ms(t.elapsed())));
        }
        let t0 = Instant::now();
        drop(caixa);
        drop(leitor);
        let t_leitor = t0.elapsed();
        let t1 = Instant::now();
        unsafe {
            let _ = fonte.0.Shutdown();
        }
        drop(fonte);
        let t_fonte = t1.elapsed();
        let t2 = Instant::now();
        drop(regua);
        let t_regua = t2.elapsed();
        drop(vaga);
        let total = t0.elapsed();
        registro::linha(format!(
            "{}câmera: leitor e fonte soltos fora da thread da sessão em {} ms (amostra e leitor {} ms, Shutdown da fonte {} ms, régua {} ms){}",
            if total >= Duration::from_secs(1) { "!! " } else { "" },
            ms(total),
            ms(t_leitor),
            ms(t_fonte),
            ms(t_regua),
            if total >= Duration::from_secs(1) {
                " — a sessão não esperou por isto; no R4 (M51) foi aqui que ela ficou presa"
            } else {
                ""
            }
        ));
        if com.is_ok() {
            unsafe { CoUninitialize() };
        }
        SOLTURAS_PENDENTES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// **Solta a captura aberta fora desta thread** (o R4, M51). Nesta thread só o que não chama o
/// Frame Server: o retorno para de pedir, e o leitor e a amostra saem do estado. O resto vai para
/// uma thread própria; se ela não subir, a soltura é feita aqui.
fn soltar_em_outra_thread(
    estado: &Arc<Compartilhado>,
    fonte: IMFMediaSource,
    regua: Option<crate::regua_de_bancada::FonteDeRegua>,
    vaga: Option<VagaNoProcesso>,
    ajustes: Option<std::thread::JoinHandle<()>>,
) {
    estado.parado.store(true, Ordering::SeqCst);
    let leitor = estado.leitor.lock().unwrap_or_else(|e| e.into_inner()).take();
    let caixa = estado.caixa.lock().unwrap_or_else(|e| e.into_inner()).take();
    let pacote = Soltura { ajustes, caixa, leitor, fonte: FonteEnviavel(fonte), regua, vaga, prefixo: registro::prefixo_desta_thread() };
    SOLTURAS_PENDENTES.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = bounded::<Soltura>(1);
    let _ = tx.send(pacote);
    let rx_da_thread = rx.clone();
    let subiu = std::thread::Builder::new().name("quall-soltura-da-camera".into()).spawn(move || {
        if let Ok(p) = rx_da_thread.recv() {
            p.soltar();
        }
    });
    if let Err(e) = subiu {
        registro::linha(format!("câmera: a thread da soltura não subiu ({e}); soltando nesta"));
        if let Ok(p) = rx.try_recv() {
            p.soltar();
        }
    }
}

/// Uma falha da tentativa, com o código quando há um.
fn falha(texto: impl Into<String>, codigo: Option<u32>) -> FalhaDaTentativa {
    FalhaDaTentativa { texto: texto.into(), causa: regras::causa_da_falha(codigo, false) }
}

/// Uma falha de chamada Win32, com o `HRESULT` dela.
fn falha_win(contexto: &str, e: windows::core::Error) -> FalhaDaTentativa {
    let codigo = e.code().0 as u32;
    falha(format!("{contexto}: {e}"), Some(codigo))
}

/// A fonte de mídia da tentativa, e quem serve o cano dela quando é a sintética com régua.
fn criar_fonte(
    fonte: &FonteDaCamera,
    modo: Modo,
) -> std::result::Result<(IMFMediaSource, Option<crate::regua_de_bancada::FonteDeRegua>), FalhaDaTentativa> {
    unsafe {
        match fonte {
            FonteDaCamera::Link(link) => {
                let mut attrs: Option<IMFAttributes> = None;
                MFCreateAttributes(&mut attrs, 3).map_err(|e| falha_win("MFCreateAttributes", e))?;
                let attrs = attrs.ok_or_else(|| falha("MFCreateAttributes sem atributos", None))?;
                attrs
                    .SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)
                    .map_err(|e| falha_win("SOURCE_TYPE", e))?;
                attrs
                    .SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &HSTRING::from(link.as_str()))
                    .map_err(|e| falha_win("SYMBOLIC_LINK", e))?;
                if modo == Modo::Compartilhada {
                    attrs
                        .SetUINT32(&MF_DEVSOURCE_ATTRIBUTE_FRAMESERVER_SHARE_MODE, 1)
                        .map_err(|e| falha_win("SHARE_MODE", e))?;
                }
                // **Pelo link, e não por um `IMFActivate`**: um segundo `ActivateObject` antes do
                // `DetachObject` devolve a instância guardada e ignora o atributo novo
                // (documentação oficial; a revisão, 6).
                MFCreateDeviceSource(&attrs).map(|f| (f, None)).map_err(|e| falha_win("MFCreateDeviceSource", e))
            }
            FonteDaCamera::DoQuallNoProcesso { regua } => {
                let nome = format!("quall-camera-sintetica-{}-{:x}", std::process::id(), qpc_100ns());
                let cano = quall_camera_fonte::quadros::cano_do_nome(&nome);
                if cano_existe(&cano) || cano_existe(quall_camera_fonte::quadros::CANO_SEM_NOME) {
                    return Err(falha(
                        format!(
                            "o cano {cano} ou o cano sem nome existe: alguém o serve, e a fonte não entregaria só o padrão de bancada"
                        ),
                        None,
                    ));
                }
                // Com a régua, o cano é servido **antes** de a fonte nascer: ela o abre no `Start`.
                let servidor = regua.then(|| crate::regua_de_bancada::FonteDeRegua::servir(cano.clone()));
                let f: IMFMediaSource = CoCreateInstance(&quall_camera_fonte::CLSID_FONTE, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| falha_win("CoCreateInstance da fonte do Quall", e))?;
                let ex: IMFMediaSourceEx = f.cast().map_err(|e| falha_win("IMFMediaSourceEx", e))?;
                ex.GetSourceAttributes()
                    .and_then(|a| a.SetString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &HSTRING::from(nome.as_str())))
                    .map_err(|e| falha_win("o nome aleatório no repositório da fonte", e))?;
                registro::linha(format!(
                    "câmera: a fonte do Quall no processo, com o nome \"{nome}\" → {cano}, livre{}",
                    if servidor.is_some() { "; servido por este processo com a régua" } else { "" }
                ));
                Ok((f, servidor))
            }
        }
    }
}

/// **A criação da fonte pelo link numa thread própria**, com a espera em fatias de
/// [`regras::FATIA_DA_ESPERA`] que olha o Parar. O R4 de novo (M54) mostrou por quê: a criação
/// levou ~4,6 s num nó recém-criado (5,1 s no M41), numa chamada só, e a thread da sessão passava o
/// prazo do cão de guarda do coordenador (5 s) sem sinal nem Parar. Com o Parar, a espera sai; a
/// fonte que nascer depois dele é desligada pela thread que a criou. Sem prazo novo: a criação que
/// não volta segue esperando, com o Parar valendo e uma linha no registro aos 10 s.
///
/// **A entrega é por uma caixa com cadeado e a bandeira "abandonada"** (a revisão do código da fase
/// 4, m1): pelo canal, a fonte que nascia entre o fim da espera e a queda do canal ficava dentro
/// dele, sem `Shutdown`. Agora quem desiste marca a bandeira sob o cadeado, e a criadora, sob o
/// mesmo cadeado, ou deixa a fonte na caixa (e quem espera a pega) ou a desliga. A thread entra
/// na conta das solturas pendentes enquanto vive: o `MFShutdown` da saída espera por ela.
fn criar_fonte_olhando(
    fonte: &FonteDaCamera,
    modo: Modo,
    parar: &dyn Fn() -> bool,
) -> std::result::Result<(IMFMediaSource, Option<crate::regua_de_bancada::FonteDeRegua>), FalhaDaTentativa> {
    #[derive(Default)]
    struct Caixa {
        fonte: Option<std::result::Result<FonteEnviavel, FalhaDaTentativa>>,
        abandonada: bool,
        terminou: bool,
    }
    /// Marca o fim da thread criadora (também se ela entrar em pânico) e a tira da conta.
    struct AoSair(Arc<(Mutex<Caixa>, Condvar)>);
    impl Drop for AoSair {
        fn drop(&mut self) {
            self.0 .0.lock().unwrap_or_else(|e| e.into_inner()).terminou = true;
            self.0 .1.notify_all();
            SOLTURAS_PENDENTES.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let caixa: Arc<(Mutex<Caixa>, Condvar)> = Arc::new((Mutex::new(Caixa::default()), Condvar::new()));
    let copia = fonte.clone();
    let prefixo = registro::prefixo_desta_thread();
    let comeco = Instant::now();
    SOLTURAS_PENDENTES.fetch_add(1, Ordering::SeqCst);
    let da_thread = caixa.clone();
    let subiu = std::thread::Builder::new().name("quall-fonte-da-camera".into()).spawn(move || {
        let _ao_sair = AoSair(da_thread.clone());
        registro::prefixar_esta_thread(&prefixo);
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let r = criar_fonte(&copia, modo).map(|(f, _)| FonteEnviavel(f));
        let tarde = {
            let mut g = da_thread.0.lock().unwrap_or_else(|e| e.into_inner());
            if g.abandonada {
                Some(r)
            } else {
                g.fonte = Some(r);
                da_thread.1.notify_all();
                None
            }
        };
        // Quem esperava já desistiu (o Parar): a fonte que nasceu tarde não fica viva.
        if let Some(Ok(f)) = tarde {
            unsafe {
                let _ = f.0.Shutdown();
            }
            drop(f);
            registro::linha(format!(
                "câmera: a fonte nasceu {} ms depois do começo, com a abertura já interrompida; desligada",
                comeco.elapsed().as_millis()
            ));
        }
        if com.is_ok() {
            unsafe { CoUninitialize() };
        }
    });
    if let Err(e) = subiu {
        SOLTURAS_PENDENTES.fetch_sub(1, Ordering::SeqCst);
        registro::linha(format!("câmera: a thread da criação da fonte não subiu ({e}); criando nesta"));
        return criar_fonte(fonte, modo);
    }
    let mut avisou = false;
    let mut g = caixa.0.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(r) = g.fonte.take() {
            return r.map(|f| (f.0, None));
        }
        if g.terminou {
            return Err(falha("a thread da criação da fonte acabou sem responder", None));
        }
        if parar() {
            // Sob o cadeado: a criadora, quando voltar, vê a bandeira e desliga a fonte.
            g.abandonada = true;
            return Err(cancelada(&format!(
                "na criação da fonte ({} ms; ela é desligada quando nascer)",
                comeco.elapsed().as_millis()
            )));
        }
        if !avisou && comeco.elapsed() >= Duration::from_secs(10) {
            avisou = true;
            registro::linha("câmera: a criação da fonte passa de 10 s; a abertura segue esperando, e o Parar a interrompe");
        }
        g = caixa.1.wait_timeout(g, regras::FATIA_DA_ESPERA).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
    }
}

fn ms(d: Duration) -> String {
    format!("{:.0}", d.as_secs_f64() * 1000.0)
}

/// A falha de quem pediu Parar durante a abertura.
fn cancelada(onde: &str) -> FalhaDaTentativa {
    FalhaDaTentativa { texto: format!("Parar pedido {onde}"), causa: CausaDaFalha::Cancelada }
}

/// Uma tentativa de abertura, com o recuo do DV (22/09): com o adapt2 pedido, o leitor do DV nasce
/// **sem** o gerenciador D3D (o quadro em memória, para o desentrelaçador da CPU). Esse leitor não foi
/// exercitado com a câmera antes desta frente; se a tentativa com ele falhar por um motivo que outra
/// tentativa pode resolver (`regras::refazer_o_dv_com_d3d`), ela é refeita **com** o gerenciador, e
/// o DV sai pelo bob do processador, como na fase 5. O adapt2 nunca é condição para a câmera abrir.
fn tentar(
    fonte: &FonteDaCamera,
    modo: Modo,
    gerenciador: &IMFDXGIDeviceManager,
    teto: TetoDaCamera,
    parar: &dyn Fn() -> bool,
) -> std::result::Result<Aberta, FalhaDaTentativa> {
    let adapt2 = adapt2_pedido();
    let usou = std::cell::Cell::new(false);
    match tentar_uma(fonte, modo, gerenciador, teto, parar, adapt2, &usou) {
        // Só as causas que o leitor sem D3D pode ter provocado; a câmera ocupada, negada, removida
        // ou o Parar não dizem nada dele.
        Err(f) if usou.get() && matches!(f.causa, CausaDaFalha::Outra | CausaDaFalha::Demorou) => {
            // Lembrado no processo só quando o leitor devolveu erro: a próxima tentativa (o recuo de
            // modo, a próxima sessão) já vai com o gerenciador. A câmera muda (`Demorou`, a fita
            // parada) não acusa o leitor: a próxima sessão tenta o adapt2 de novo.
            let lembrar = regras::lembrar_que_o_dv_sem_d3d_falhou(f.causa);
            if lembrar {
                DV_SEM_D3D_FALHOU.store(true, Ordering::SeqCst);
            }
            if regras::refazer_o_dv_com_d3d(f.causa) {
                registro::linha(format!(
                    "câmera: o DV com o leitor sem D3D (o adapt2) não abriu ({:?}: {}); refazendo com o gerenciador D3D e o bob do processador",
                    f.causa, f.texto
                ));
                tentar_uma(fonte, modo, gerenciador, teto, parar, false, &usou)
            } else {
                registro::linha(format!(
                    "câmera: o DV com o leitor sem D3D (o adapt2) não abriu ({:?}: {}); {}",
                    f.causa,
                    f.texto,
                    if lembrar {
                        "as próximas tentativas deste processo vão com o gerenciador D3D e o bob"
                    } else {
                        "a câmera não mandou quadro (a fita parada?): a próxima tentativa tenta o adapt2 de novo"
                    }
                ));
                Err(f)
            }
        }
        r => r,
    }
}

#[allow(clippy::too_many_arguments)]
fn tentar_uma(
    fonte: &FonteDaCamera,
    modo: Modo,
    gerenciador: &IMFDXGIDeviceManager,
    teto: TetoDaCamera,
    parar: &dyn Fn() -> bool,
    adapt2: bool,
    dv_sem_d3d: &std::cell::Cell<bool>,
) -> std::result::Result<Aberta, FalhaDaTentativa> {
    dv_sem_d3d.set(false);
    if parar() {
        return Err(cancelada("antes de criar a fonte"));
    }
    let comeco = Instant::now();
    // A criação da fonte pelo link é uma chamada só, que não se interrompe, e num nó recém-criado
    // levou 4,6–5,1 s (M41, M42, M51, M54): ela corre numa thread própria, e a espera olha o Parar
    // (e, com ele, dá o sinal de vida da sessão). A fonte do Quall no processo nasce em ~5 ms.
    let (media, servidor_da_regua) = match fonte {
        FonteDaCamera::Link(_) => criar_fonte_olhando(fonte, modo, parar)?,
        FonteDaCamera::DoQuallNoProcesso { .. } => criar_fonte(fonte, modo)?,
    };
    let t_fonte = comeco.elapsed();
    let (tx, rx) = bounded::<()>(1);
    let estado = Arc::new(Compartilhado {
        caixa: Mutex::new(None),
        aviso: tx,
        leitor: Mutex::new(None),
        erro: Mutex::new(None),
        codigo: Mutex::new(None),
        removida: AtomicBool::new(false),
        parado: AtomicBool::new(false),
        resposta: Mutex::new(None),
        tem_resposta: Condvar::new(),
        esperado: Mutex::new(None),
        ultimo_chegado: Mutex::new(None),
        chegados: Arc::new(AtomicU64::new(0)),
        sobrescritos: AtomicU64::new(0),
        vazios: AtomicU64::new(0),
        ticks: AtomicU64::new(0),
        trocas_de_tipo: AtomicU64::new(0),
        formato_mudou: AtomicBool::new(false),
        nascido: Instant::now(),
        descartados_na_bancada: AtomicU64::new(0),
        base_do_aspecto: Mutex::new(None),
        aspecto: Mutex::new(regras::AspectoDaSessao::default()),
        prefixo: registro::prefixo_desta_thread(),
    });
    let retorno: IMFSourceReaderCallback = Retorno { estado: estado.clone() }.into();
    // Os tempos da abertura, para o registro: onde vão os 5,1 s de uma câmera recém-criada (M35).
    let mut tempos = format!("fonte {} ms", ms(t_fonte));
    let montado = (|| -> std::result::Result<Aberta, FalhaDaTentativa> {
        let fluxo = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let t0 = Instant::now();
        // **O leitor leva o gerenciador D3D, ou não, pelo tipo** (a fase 5, p9: a Canon, com o
        // gerenciador no leitor, devolveu 0xC00D36B4 no primeiro ReadSample; sem ele, o quadro veio
        // em memória). O tipo é lido do descritor da fonte, antes de haver leitor.
        let pelo_descritor = subtipo_pelo_descritor(&media, modo, teto);
        let (com_d3d, porque) = match pelo_descritor {
            Some(s) => (
                regras::leitor_com_d3d(s, adapt2),
                if s == Subtipo::Dv && adapt2 {
                    "o tipo do descritor é Dv, e o adapt2 desentrelaça na CPU".to_string()
                } else {
                    format!("o tipo do descritor é {s:?}")
                },
            ),
            None => (true, "o descritor não disse o tipo".to_string()),
        };
        dv_sem_d3d.set(!com_d3d && adapt2 && pelo_descritor == Some(Subtipo::Dv));
        let leitor: IMFSourceReader = unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 3).map_err(|e| falha_win("MFCreateAttributes", e))?;
            let attrs = attrs.ok_or_else(|| falha("MFCreateAttributes sem atributos", None))?;
            attrs.SetUnknown(&MF_SOURCE_READER_ASYNC_CALLBACK, &retorno).map_err(|e| falha_win("ASYNC_CALLBACK", e))?;
            // O gerenciador da mesma placa do encoder: a superfície do leitor é da mesma placa, e a
            // cópia para o anel fica na GPU. **Sem** `ENABLE_ADVANCED_VIDEO_PROCESSING` (M9).
            if com_d3d {
                attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, gerenciador).map_err(|e| falha_win("D3D_MANAGER", e))?;
                attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(|e| falha_win("HARDWARE_TRANSFORMS", e))?;
            }
            MFCreateSourceReaderFromMediaSource(&media, &attrs).map_err(|e| falha_win("MFCreateSourceReaderFromMediaSource", e))?
        };
        if !com_d3d {
            estado.linha(format!(
                "câmera: o leitor SEM o gerenciador D3D ({porque}): os quadros vêm em memória e sobem para o anel pela CPU (p9)"
            ));
        }
        *estado.leitor.lock().unwrap_or_else(|e| e.into_inner()) = Some(LeitorEnviavel(leitor.clone()));
        tempos.push_str(&format!(", leitor {} ms", ms(t0.elapsed())));

        // --- os tipos nativos, e o escolhido ---------------------------------------------------
        let t1 = Instant::now();
        let mut nativos: Vec<(TipoNativo, IMFMediaType)> = Vec::new();
        let mut i = 0u32;
        while let Ok(t) = unsafe { leitor.GetNativeMediaType(fluxo, i) } {
            nativos.push((unsafe { tipo_nativo_de(&t) }, t));
            i += 1;
            if i > 512 {
                break;
            }
        }
        let lista: Vec<TipoNativo> = nativos.iter().map(|(t, _)| *t).collect();
        // O `MF_MT_INTERLACE_MODE` que o nativo declara (o DV da Panasonic diz 2, progressivo: a
        // regra não confia nele, `regras::entrelacamento`).
        let modo_nativo: Option<u32>;
        let escolhido: TipoNativo = if modo == Modo::Compartilhada {
            // Compartilhada não escolhe: usa o tipo que quem controla deixou.
            let t = unsafe { leitor.GetCurrentMediaType(fluxo) }.map_err(|e| falha_win("o tipo em uso (compartilhada)", e))?;
            modo_nativo = unsafe { t.GetUINT32(&MF_MT_INTERLACE_MODE) }.ok();
            unsafe { tipo_nativo_de(&t) }
        } else {
            let k = regras::escolher_tipo_nativo(&lista, teto)
                .ok_or_else(|| falha(format!("a câmera não declara NV12, YUY2, MJPG, I420 nem DV ({} tipos)", lista.len()), None))?;
            modo_nativo = unsafe { nativos[k].1.GetUINT32(&MF_MT_INTERLACE_MODE) }.ok();
            let ex: std::result::Result<IMFSourceReaderEx, _> = leitor.cast();
            match ex {
                Ok(ex) => {
                    if let Err(e) = unsafe { ex.SetNativeMediaType(fluxo, &nativos[k].1) } {
                        registro::linha(format!("câmera: SetNativeMediaType({}) recusado ({e}); segue o tipo padrão", lista[k].descricao()));
                    }
                }
                Err(e) => registro::linha(format!("câmera: sem IMFSourceReaderEx ({e}); segue o tipo padrão")),
            }
            unsafe { leitor.GetCurrentMediaType(fluxo) }.map(|t| unsafe { tipo_nativo_de(&t) }).unwrap_or(lista[k])
        };
        // **O tipo em uso tem de caber no leitor que foi criado** (a revisão do código da fase 5, L6,
        // e a revisão curta, A7): o leitor levou o gerenciador D3D, ou não, pelo tipo do descritor.
        // Se o `SetNativeMediaType` foi recusado e o tipo em uso é outro: com D3D e I420, o primeiro
        // quadro daria o `0xC00D36B4` da Canon (p9), e a abertura falha com o motivo; sem D3D e um tipo
        // de GPU, serve, pela memória.
        match regras::leitor_serve(com_d3d, escolhido.subtipo, adapt2) {
            regras::LeitorServe::Sim => {}
            regras::LeitorServe::PelaMemoria => estado.linha(format!(
                "câmera: o tipo em uso ({}) não é o do descritor ({porque}); o leitor nasceu sem D3D, e segue pela memória",
                escolhido.descricao()
            )),
            regras::LeitorServe::Nao => {
                return Err(falha(
                    format!(
                        "o tipo em uso ({}) não serve ao leitor com o gerenciador D3D ({porque}): o I420 pede o leitor sem ele",
                        escolhido.descricao()
                    ),
                    None,
                ))
            }
        }

        // --- o tipo de saída: o formato do anel -------------------------------------------------
        let pedidos: &[FormatoDoLeitor] = match escolhido.subtipo {
            // O DV: o decodificador de DV do Windows entrega YUY2 (medido no recuo compartilhado do
            // p5 DV: o NV12 foi recusado e o YUY2 aceito); pedir YUY2 primeiro poupa a tentativa.
            Subtipo::Yuy2 | Subtipo::Dv => &[FormatoDoLeitor::Yuy2, FormatoDoLeitor::Nv12],
            // O I420 sai como veio: o entrelaçamento é nosso, na cópia para o anel.
            Subtipo::I420 => &[FormatoDoLeitor::I420],
            _ => &[FormatoDoLeitor::Nv12, FormatoDoLeitor::Yuy2],
        };
        let mut aceito = None;
        for f in pedidos {
            let t = unsafe { MFCreateMediaType() }.map_err(|e| falha_win("MFCreateMediaType", e))?;
            unsafe {
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| falha_win("MF_MT_MAJOR_TYPE", e))?;
                t.SetGUID(&MF_MT_SUBTYPE, &f.guid()).map_err(|e| falha_win("MF_MT_SUBTYPE", e))?;
            }
            if unsafe { leitor.SetCurrentMediaType(fluxo, None, &t) }.is_ok() {
                aceito = Some(*f);
                break;
            }
        }
        let formato =
            aceito.ok_or_else(|| falha(format!("o leitor não entrega {pedidos:?} de {}", escolhido.descricao()), None))?;
        tempos.push_str(&format!(", tipos {} ms", ms(t1.elapsed())));

        // --- o primeiro quadro, ou a falha, ou a demora -------------------------------------------
        let t2 = Instant::now();
        unsafe { leitor.ReadSample(fluxo, 0, None, None, None, None) }.map_err(|e| falha_win("o primeiro ReadSample", e))?;
        let prazo = regras::ESPERA_DO_PRIMEIRO_QUADRO;
        let resposta = regras::esperar_resposta(&estado.resposta, &estado.tem_resposta, prazo, parar);
        tempos.push_str(&format!(", primeiro quadro {} ms", ms(t2.elapsed())));
        match resposta {
            regras::Espera::Veio(Ok(())) => {}
            regras::Espera::Veio(Err(texto)) => {
                let codigo = *estado.codigo.lock().unwrap_or_else(|e| e.into_inner());
                return Err(falha(format!("{texto} [{tempos}]"), codigo));
            }
            regras::Espera::Prazo => {
                return Err(FalhaDaTentativa {
                    texto: format!("nenhum quadro nem falha em {} s [{tempos}]", prazo.as_secs()),
                    causa: CausaDaFalha::Demorou,
                })
            }
            regras::Espera::Parada => return Err(cancelada(&format!("na espera do primeiro quadro [{tempos}]"))),
        }

        // --- o tipo de saída **depois** do primeiro quadro (M2), com a geometria (M3) -------------
        let atual = unsafe { leitor.GetCurrentMediaType(fluxo) }.map_err(|e| falha_win("GetCurrentMediaType", e))?;
        let (sub, geometria) = unsafe { saida_de(&atual) };
        if sub != formato.guid() {
            return Err(falha(
                format!("depois do primeiro quadro o leitor entrega {:?}, e não o {formato:?} pedido", subtipo_de(&sub)),
                None,
            ));
        }
        // **O I420 de tamanho ímpar é recusado** (a revisão do código da fase 5, L5): o croma de um
        // quadro ímpar tem dois arranjos em uso (⌊w/2⌋ e ⌈w/2⌉), e o errado sai trocado sem erro.
        if formato == FormatoDoLeitor::I420 && (geometria.quadro_largura % 2 != 0 || geometria.quadro_altura % 2 != 0) {
            return Err(falha(
                format!("I420 de tamanho ímpar ({}x{}): o arranjo do croma não é conhecido", geometria.quadro_largura, geometria.quadro_altura),
                None,
            ));
        }
        // Daqui em diante, uma troca de tipo é conferida contra isto (`conferir_o_tipo`).
        *estado.esperado.lock().unwrap_or_else(|e| e.into_inner()) = Some((sub, geometria));
        let fps_saida = unsafe { tipo_nativo_de(&atual) }.fps();
        let completa_declarada = match unsafe { atual.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE) } {
            Ok(v) if v == MFNominalRange_0_255.0 as u32 => Some(true),
            Ok(v) if v == MFNominalRange_16_235.0 as u32 => Some(false),
            _ => None,
        };
        let matriz_declarada = match unsafe { atual.GetUINT32(&MF_MT_YUV_MATRIX) } {
            Ok(v) if v == MFVideoTransferMatrix_BT709.0 as u32 => Some(true),
            Ok(v) if v == MFVideoTransferMatrix_BT601.0 as u32 => Some(false),
            _ => None,
        };
        let modo_saida = unsafe { atual.GetUINT32(&MF_MT_INTERLACE_MODE) }.ok();
        // Manda o que a saída declara (o decodificador pode corrigir o nativo); sem ela, o nativo.
        // Fora do DV, a amostra veta os modos fixos e decide o misto (a revisão curta do `08af2cd`,
        // A8): o primeiro quadro está na caixa. A altura é a do quadro, e não a da abertura (L2).
        let amostra = {
            let caixa = estado.caixa.lock().unwrap_or_else(|e| e.into_inner());
            let bandeira = |chave: &GUID| {
                caixa.as_ref().and_then(|r| unsafe { r.amostra.GetUINT32(chave) }.ok()).map(|v| v != 0)
            };
            regras::AmostraDiz {
                entrelacada: bandeira(&MFSampleExtension_Interlaced),
                campo_de_baixo_primeiro: bandeira(&MFSampleExtension_BottomFieldFirst),
            }
        };
        let decidido = regras::entrelacamento(escolhido.subtipo, geometria.quadro_altura, modo_saida.or(modo_nativo), amostra);
        let entrelacamento = if SEM_DESENTRELACAR.load(Ordering::SeqCst) && decidido != regras::Entrelacamento::Progressivo {
            registro::linha(format!(
                "câmera: --sem-desentrelacar (bancada): o quadro {decidido:?} segue entrelaçado, como veio"
            ));
            regras::Entrelacamento::Progressivo
        } else {
            decidido
        };
        // **Quem desentrelaça** (22/09): o adapt2 na cópia para o anel só com o quadro em memória,
        // YUY2 e a imagem com os campos no lugar; senão o bob do processador (o conversor).
        let quem = regras::quem_desentrelaca(
            entrelacamento,
            adapt2,
            !com_d3d,
            formato == FormatoDoLeitor::Yuy2,
            geometria.y,
            geometria.largura,
            geometria.altura,
        );
        // O aspecto (a fase 5, o DV em 16:9): decidido agora, e relido a cada troca de tipo. A base
        // vai **antes** do cálculo, e a abertura só fixa se nenhuma releitura guardou no meio (a
        // revisão curta do `08af2cd`, A4).
        *estado.base_do_aspecto.lock().unwrap_or_else(|e| e.into_inner()) = Some((escolhido.subtipo, geometria));
        let calculado = aspecto_do_leitor(&leitor, escolhido.subtipo, geometria);
        let aspecto = estado.aspecto.lock().unwrap_or_else(|e| e.into_inner()).fixar_na_abertura(calculado);
        let (exibida_l, exibida_a) = regras::tamanho_exibido(geometria.largura, geometria.altura, aspecto.par);
        let trocas = estado.trocas_de_tipo.load(Ordering::Relaxed);
        // Quem converte: o leitor insere um MFT do Windows quando o nativo não é o que ele entrega
        // (o MJPEG, o DV); o I420 é nosso, na cópia para o anel.
        let conversao = match (escolhido.subtipo, formato) {
            (Subtipo::I420, FormatoDoLeitor::I420) => "; o I420 vira NV12 na cópia para o anel (nossa, na CPU)".to_string(),
            (n, f) if n != f.subtipo() => {
                format!("; o leitor converte {n:?} → {f:?} com um MFT do Windows neste processo (sem o processador avançado)")
            }
            _ => String::new(),
        };
        let descricao = format!(
            "modo {modo:?}; nativo {} de {} declarado(s) [{}]; o leitor entrega {:?} {} @{:.2}{conversao}; faixa declarada {}; matriz declarada {}; \
             entrelaçamento {entrelacamento:?} (MF_MT_INTERLACE_MODE: o nativo {}, a saída {}), desentrelaça: {quem:?}; aspecto {:?} PAR {}:{} → exibida {exibida_l}x{exibida_a}{}; tempos: {tempos}",
            escolhido.descricao(),
            lista.len(),
            lista.iter().take(12).map(|t| t.descricao()).collect::<Vec<_>>().join(", "),
            formato,
            geometria.descricao(),
            fps_saida,
            match completa_declarada {
                Some(true) => "0-255",
                Some(false) => "16-235",
                None => "nenhuma",
            },
            match matriz_declarada {
                Some(true) => "BT.709",
                Some(false) => "BT.601",
                None => "nenhuma",
            },
            modo_nativo.map(|m| m.to_string()).unwrap_or_else(|| "nenhum".into()),
            modo_saida.map(|m| m.to_string()).unwrap_or_else(|| "nenhum".into()),
            aspecto.origem,
            aspecto.par.num,
            aspecto.par.den,
            if trocas > 0 { format!("; {trocas} troca(s) de tipo antes do primeiro quadro, absorvidas") } else { String::new() },
        );
        Ok(Aberta {
            fonte: media.clone(),
            regua: None,
            estado: estado.clone(),
            avisos: rx.clone(),
            geometria,
            formato,
            nativo: escolhido.subtipo,
            completa_declarada,
            matriz_709: regras::matriz_709(matriz_declarada),
            entrelacamento,
            quem,
            modo,
            descricao,
            fps: if escolhido.fps() > 0.0 { escolhido.fps() } else { fps_saida },
        })
    })();
    match montado {
        Ok(mut a) => {
            a.regua = servidor_da_regua;
            Ok(a)
        }
        Err(e) => {
            soltar(&estado, &media);
            // O cano da régua cai depois da fonte que o lia.
            drop(servidor_da_regua);
            Err(e)
        }
    }
}

impl CapturaDeCamera {
    /// Abre a câmera: controladora e, se ela falhar ou demorar, compartilhada.
    ///
    /// `dispositivo` e `gerenciador` são os da placa do encoder (`transmissao.rs`); `origem` é o
    /// zero do relógio da sessão, o mesmo do som. `parar` é o Parar da sessão: a abertura o olha
    /// entre as tentativas, na espera da vez, na criação da fonte e na espera do primeiro quadro (a
    /// reconferência da fase 3). `bater` é o sinal de vida da sessão para o cão de guarda do
    /// coordenador: dado **a cada vez que o Parar é olhado** (M54).
    pub fn abrir(
        dispositivo: &ID3D11Device,
        gerenciador: &IMFDXGIDeviceManager,
        fonte: &FonteDaCamera,
        teto: TetoDaCamera,
        origem: Instant,
        parar: Option<&AtomicBool>,
        bater: Option<&dyn Fn()>,
    ) -> std::result::Result<CapturaDeCamera, String> {
        Self::abrir_com_plano(dispositivo, gerenciador, fonte, teto, origem, parar, bater, None)
    }

    /// Como [`CapturaDeCamera::abrir`], com o plano da abertura pelo link **forçado** (a bancada:
    /// `quall_camera_local capturar --link … --compartilhada`, o R3b da revisão do código da fase 4,
    /// M1). `None` é o produto: o plano sai das aberturas do processo. A vez é pedida do mesmo jeito.
    #[allow(clippy::too_many_arguments)]
    pub fn abrir_com_plano(
        dispositivo: &ID3D11Device,
        gerenciador: &IMFDXGIDeviceManager,
        fonte: &FonteDaCamera,
        teto: TetoDaCamera,
        origem: Instant,
        parar: Option<&AtomicBool>,
        bater: Option<&dyn Fn()>,
        plano_forcado: Option<PlanoDaAbertura>,
    ) -> std::result::Result<CapturaDeCamera, String> {
        let parar = || {
            if let Some(b) = bater {
                b();
            }
            parar.is_some_and(|p| p.load(Ordering::SeqCst))
        };
        // A vez desta câmera no processo: solta sem abrir, devolve a vez sem contar (o `?`).
        let mut vez: Option<VezNaAbertura> = None;
        let aberta = match fonte {
            FonteDaCamera::DoQuallNoProcesso { .. } => tentar(fonte, Modo::NoProcesso, gerenciador, teto, &parar)
                .map_err(|f| regras::texto_da_abertura_que_falhou(&f, None))?,
            FonteDaCamera::Link(link) => {
                let (plano, v) = esperar_a_vez(link, &parar).map_err(|f| regras::texto_da_abertura_que_falhou(&f, None))?;
                vez = Some(v);
                let plano = match plano_forcado {
                    Some(p) => {
                        registro::linha(format!("câmera: o plano da abertura forçado pela bancada: {p:?} (o das aberturas do processo seria {plano:?})"));
                        p
                    }
                    None => plano,
                };
                match plano {
                    PlanoDaAbertura::ControladoraComRecuo => match tentar(fonte, Modo::Controladora, gerenciador, teto, &parar) {
                        Ok(a) => a,
                        Err(f1) => {
                            if !regras::recuar_para_compartilhada(f1.causa) {
                                return Err(regras::texto_da_abertura_que_falhou(&f1, None));
                            }
                            registro::linha(format!(
                                "câmera: não abriu como controladora ({:?}: {}); fonte e leitor soltos, recriando pelo link em modo compartilhado",
                                f1.causa, f1.texto
                            ));
                            tentar(fonte, Modo::Compartilhada, gerenciador, teto, &parar)
                                .map_err(|f2| regras::texto_da_abertura_que_falhou(&f1, Some(&f2)))?
                        }
                    },
                    PlanoDaAbertura::SoCompartilhada { outras } => {
                        // **Nunca controladora com uma controladora viva no processo** (M51): o Frame
                        // Server não recusaria, tomaria a câmera da outra sessão.
                        registro::linha(format!(
                            "câmera: {outras} captura(s) deste processo já com esta câmera, uma controladora; compartilhada direto, \
                             sem tentar controladora (no mesmo processo a segunda controladora toma a câmera da primeira, M51)"
                        ));
                        match tentar(fonte, Modo::Compartilhada, gerenciador, teto, &parar) {
                            Ok(a) => a,
                            Err(f1) => {
                                // **A compartilhada que falha sem controladora viva recua para
                                // controladora** (a revisão do código da fase 4, M1): a controladora
                                // pode ter saído no meio desta abertura, e não há recusa a esperar.
                                let ha_controladora = aberturas().ha_controladora(link);
                                if ha_controladora || !regras::recuar_para_controladora(f1.causa) {
                                    return Err(regras::texto_da_abertura_que_falhou(&f1, None));
                                }
                                registro::linha(format!(
                                    "câmera: a compartilhada não abriu ({:?}: {}) e nenhuma controladora deste processo tem a câmera; \
                                     fonte e leitor soltos, tentando como controladora",
                                    f1.causa, f1.texto
                                ));
                                tentar(fonte, Modo::Controladora, gerenciador, teto, &parar)
                                    .map_err(|f2| regras::texto_da_abertura_que_falhou(&f2, Some(&f1)))?
                            }
                        }
                    }
                }
            }
        };
        let mut aberta = aberta;
        let modo = aberta.modo;
        let g = aberta.geometria;
        // As duas saídas de falha soltam **fora desta thread**, como o `stop` (a revisão do código da
        // fase 4, m3): o `Release` do leitor esperou 19,9 s no M54. A vez cai no retorno, sem contar.
        let contexto = match unsafe { dispositivo.GetImmediateContext() } {
            Ok(c) => c,
            Err(e) => {
                soltar_em_outra_thread(&aberta.estado, aberta.fonte.clone(), aberta.regua.take(), None, None);
                return Err(format!("GetImmediateContext: {e}"));
            }
        };
        let mut anel = Vec::with_capacity(TAMANHO_DO_ANEL);
        for _ in 0..TAMANHO_DO_ANEL {
            match textura_do_anel(dispositivo, aberta.formato.dxgi(), g.largura, g.altura) {
                Ok(t) => anel.push(t),
                Err(e) => {
                    soltar_em_outra_thread(&aberta.estado, aberta.fonte.clone(), aberta.regua.take(), None, None);
                    return Err(format!("o anel {:?} {}x{} não foi criado: {e}", aberta.formato, g.largura, g.altura));
                }
            }
        }
        let faixa_completa = regras::faixa_completa(aberta.nativo, aberta.completa_declarada);
        // O adapt2 desta captura: com a geometria que ele recusar, o bob do processador (e o
        // conversor desentrelaça, porque `entrelacamento_no_anel` sai de `quem`).
        let (quem, desentrelacador) = match aberta.quem {
            regras::QuemDesentrelaca::Cpu(e) => {
                let ordem = if e == regras::Entrelacamento::CampoDeCimaPrimeiro { Ordem::CampoDeCimaPrimeiro } else { Ordem::CampoDeBaixoPrimeiro };
                match Desentrelacador::novo_yuy2(g.largura as usize, g.altura as usize, ordem) {
                    Some(d) => (aberta.quem, Some(d)),
                    None => {
                        registro::linha(format!(
                            "câmera: o adapt2 recusou a imagem {}x{}; o bob do processador desentrelaça",
                            g.largura, g.altura
                        ));
                        (regras::QuemDesentrelaca::Processador(e), None)
                    }
                }
            }
            outro => (outro, None),
        };
        // **Os ajustes da câmera** (R9 §2.2): depois do primeiro quadro (a tentativa só volta com ele),
        // numa thread própria, sem atrasar a abertura. Só pelo link: a fonte do Quall no processo
        // fica sem controles pelo tipo.
        let ajustes = match fonte {
            FonteDaCamera::Link(l) => {
                crate::ajustes_da_camera::AjustesDaCamera::iniciar(&aberta.fonte, l, modo, aberta.fps, Arc::clone(&aberta.estado.chegados))
            }
            FonteDaCamera::DoQuallNoProcesso { .. } => None,
        };
        registro::linha(format!(
            "câmera aberta: {} — {}; faixa da entrada {}, matriz {}; anel de {TAMANHO_DO_ANEL} texturas {:?} {}x{}",
            fonte.descricao(),
            aberta.descricao,
            if faixa_completa { "completa" } else { "limitada" },
            if aberta.matriz_709 { "BT.709" } else { "BT.601" },
            aberta.formato,
            g.largura,
            g.altura
        ));
        Ok(CapturaDeCamera {
            frame_ready: aberta.avisos,
            estado: aberta.estado,
            fonte: Some(aberta.fonte),
            dispositivo: dispositivo.clone(),
            contexto,
            anel,
            proxima: 0,
            repetivel: regras::PosicaoRepetivel::default(),
            staging: None,
            carimbador: Carimbador::novo(origem),
            relogios: RelogiosDoFluxo::novo(),
            janela: JanelaDaCamera::novo(),
            ultimo_entregue: None,
            fps_do_teto: teto.fps,
            aberta_em: Instant::now(),
            width: g.largura,
            height: g.altura,
            geometria: g,
            formato: aberta.formato,
            nativo: aberta.nativo,
            faixa_completa,
            matriz_709: aberta.matriz_709,
            entrelacamento: aberta.entrelacamento,
            entrelacamento_no_anel: quem.no_anel(),
            quem_desentrelaca: quem,
            desentrelacador,
            custo_do_desentrelacador: (0, 0, 0),
            modo,
            descricao: aberta.descricao,
            flush_antes_de_soltar: true,
            recortes: 0,
            entregues: 0,
            fora_do_ritmo: 0,
            dxgi: 0,
            memoria: 0,
            falhas_de_copia: 0,
            falhas_seguidas: 0,
            de_outra_placa: 0,
            subrecursos: Default::default(),
            arrays: Default::default(),
            regua: aberta.regua,
            vaga: vez.map(|v| v.abriu(if modo == Modo::Compartilhada { Papel::Compartilhada } else { Papel::Controladora })),
            ajustes,
        })
    }

    /// A ponta dos ajustes desta câmera (R9), se ela tem.
    pub fn ajustes(&self) -> Option<crate::ajustes_da_camera::PontaDosAjustes> {
        self.ajustes.as_ref().map(|a| a.ponta())
    }

    /// O quadro da caixa, carimbado e **copiado para o anel**. `None` quando não há, quando ele é
    /// anterior à origem, quando chegou cedo demais para o fps do teto, ou quando a cópia falhou.
    ///
    /// O carimbo, a origem e o ritmo vêm **antes** da cópia (m5): o quadro que vai ser descartado
    /// não toma posição do anel nem gasta uma cópia de GPU.
    pub fn take_frame(&mut self) -> Option<CapturedFrame> {
        let r = self.estado.caixa.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        let dispositivo_100ns = unsafe { r.amostra.GetUINT64(&MFSampleExtension_DeviceTimestamp) }.ok().map(|v| v as i64);
        let tempo_100ns = unsafe { r.amostra.GetSampleTime() }.unwrap_or(r.tempo_100ns);
        let chegada = Chegada {
            instante: r.instante,
            qpc_100ns: r.qpc_100ns,
            dispositivo_100ns,
            tempo_da_amostra_100ns: Some(tempo_100ns),
        };
        let primeira = self.carimbador.fonte().is_none();
        let passo = self.relogios.observar(&chegada);
        let carimbo = self.carimbador.carimbar(&chegada);
        // A janela de 5 s conta também o quadro descartado antes da origem: ele chegou.
        self.janela.quadro(passo, self.carimbador.idade_do_ultimo());
        if let Some(linha) = self.janela.fechar_se_passou(Instant::now(), self.estado.chegados.load(Ordering::Relaxed), self.entregues) {
            registro::linha(linha);
        }
        if primeira {
            // O que a amostra diz dos campos (o DV: o tipo mente, a amostra pode dizer outra coisa).
            let bandeira = |g: &GUID| match unsafe { r.amostra.GetUINT32(g) } {
                Ok(v) => v.to_string(),
                Err(_) => "—".to_string(),
            };
            registro::linha(format!(
                "câmera: o primeiro quadro diz Interlaced={} BottomFieldFirst={} RepeatFirstField={}; o entrelaçamento decidido na abertura: {:?}",
                bandeira(&MFSampleExtension_Interlaced),
                bandeira(&MFSampleExtension_BottomFieldFirst),
                bandeira(&MFSampleExtension_RepeatFirstField),
                self.entrelacamento
            ));
            registro::linha(format!(
                "câmera: carimbo do fluxo = {:?} (DeviceTimestamp {}, idade {}) | {}",
                self.carimbador.fonte(),
                if dispositivo_100ns.is_some() { "presente" } else { "ausente" },
                self.carimbador
                    .idade_do_ultimo()
                    .map(|i| format!("{:.2} ms", i.as_secs_f64() * 1000.0))
                    .unwrap_or_else(|| "—".into()),
                self.relogios.primeiro_em_texto().unwrap_or_default()
            ));
        }
        let captured_at = carimbo?;
        if !regras::entregar(self.ultimo_entregue, captured_at, self.fps_do_teto) {
            self.fora_do_ritmo += 1;
            return None;
        }
        let indice = self.proxima;
        let destino = self.anel[indice].clone();
        self.proxima = (self.proxima + 1) % self.anel.len();
        self.repetivel.tomada();
        if let Err(e) = self.copiar(&r.amostra, &destino) {
            self.falhas_de_copia += 1;
            self.falhas_seguidas += 1;
            if self.falhas_de_copia <= 3 {
                registro::linha(format!("câmera: o quadro não foi copiado para o anel: {e}"));
            }
            // Cópias que falham em seguida: a sessão não fica "Transmitindo" sem quadro (M3).
            if self.falhas_seguidas == regras::COPIAS_FALHAS_PARA_PARAR {
                self.estado.falhar(format!("{} quadros seguidos não entraram no anel: {e}", self.falhas_seguidas));
            }
            return None;
        }
        self.falhas_seguidas = 0;
        self.repetivel.copiada(indice);
        // **A cópia vai para a GPU antes de a amostra voltar ao pool** (a T1 da revisão): sem isto, o
        // `CopySubresourceRegion` pode ficar no buffer de comandos até a próxima submissão, e o
        // Frame Server reusar a superfície antes. Não observado (M41, M42), e a defesa é uma chamada.
        if self.flush_antes_de_soltar {
            unsafe { self.contexto.Flush() };
        }
        // A amostra volta ao pool **agora**: o quadro já é textura nossa.
        drop(r);
        self.ultimo_entregue = Some(captured_at);
        self.entregues += 1;
        Some(CapturedFrame { texture: destino, captured_at, posse: None })
    }

    fn copiar(&mut self, amostra: &IMFSample, destino: &ID3D11Texture2D) -> std::result::Result<(), String> {
        let t0 = Instant::now();
        let caminho = copiar_amostra_desentrelacando(
            &self.dispositivo,
            &self.contexto,
            amostra,
            destino,
            self.geometria,
            self.formato,
            &mut self.staging,
            self.desentrelacador.as_mut(),
        )?;
        if self.desentrelacador.is_some() {
            // A cópia inteira (travar, adapt2, preparo), que é o que a thread da sessão paga a mais.
            let us = t0.elapsed().as_micros() as u64;
            let c = &mut self.custo_do_desentrelacador;
            *c = (c.0 + 1, c.1 + us, c.2.max(us));
        }
        match caminho {
            CaminhoDaCopia::Gpu { subrecurso, fatias, recorta } => {
                if recorta {
                    self.recortes += 1;
                    if self.recortes == 1 {
                        registro::linha(format!(
                            "câmera: a superfície do leitor é maior que a imagem, e a cópia recorta ({})",
                            self.geometria.descricao()
                        ));
                    }
                }
                self.dxgi += 1;
                if self.subrecursos.len() < 16 {
                    self.subrecursos.insert(subrecurso);
                }
                self.arrays.insert(fatias);
            }
            CaminhoDaCopia::Memoria => self.memoria += 1,
            CaminhoDaCopia::GpuPelaMemoria => {
                self.dxgi += 1;
                self.memoria += 1;
                if self.dxgi == 1 {
                    registro::linha("câmera: a superfície veio pela GPU com o adapt2 ligado; o quadro é lido pela memória para desentrelaçar");
                }
            }
            CaminhoDaCopia::OutraPlaca => {
                self.de_outra_placa += 1;
                self.memoria += 1;
                if self.de_outra_placa == 1 {
                    registro::linha("câmera: a superfície do leitor não é da placa do encoder; o quadro sobe pela memória");
                }
            }
        }
        Ok(())
    }

    pub fn espiar_instante(&self) -> Option<Instant> {
        self.estado.caixa.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|r| r.instante)
    }

    /// A câmera acabou: o leitor falhou, um evento veio com falha, o fluxo acabou, ou o formato
    /// mudou. **A câmera parada não é fim** desde 22/09 ([`CapturaDeCamera::parada_ha`]).
    pub fn item_fechado(&self) -> bool {
        self.fim().is_some()
    }

    pub fn motivo_do_fim(&self) -> Option<String> {
        self.fim().map(|f| f.motivo)
    }

    /// Como a captura acabou, e se foi desconexão (o leitor com `0xC00D3EA2`, ou o evento de
    /// dispositivo removido): o texto do fim diz "foi desconectada" (m1).
    pub fn fim(&self) -> Option<FimDaCamera> {
        let e = self.estado.erro.lock().unwrap_or_else(|x| x.into_inner()).clone()?;
        let codigo = *self.estado.codigo.lock().unwrap_or_else(|x| x.into_inner());
        let mut f = FimDaCamera::pelo_codigo(e, self.estado.removida.load(Ordering::SeqCst), codigo);
        f.formato_mudou = self.estado.formato_mudou.load(Ordering::SeqCst);
        Some(f)
    }

    /// **A câmera parada** (a decisão do Bruno de 21/09): sem quadro por `regras::SEM_QUADRO`,
    /// há quanto tempo. Não é fim: a sessão segue, a tela diz, e a cadeia repete o último quadro.
    pub fn parada_ha(&self, agora: Instant) -> Option<Duration> {
        let ultimo = *self.estado.ultimo_chegado.lock().unwrap_or_else(|x| x.into_inner());
        regras::parada_ha(ultimo, self.aberta_em, agora)
    }

    /// **O quadro que a cadeia repete com a câmera parada**: a posição do anel da última cópia
    /// boa, só se nenhuma outra foi tomada depois dela (`regras::PosicaoRepetivel`). O anel só é
    /// escrito em [`CapturaDeCamera::take_frame`], na thread da sessão, então a posição fica intacta
    /// até o próximo quadro tomado; sem cópia nenhuma.
    pub fn quadro_para_repetir(&self) -> Option<ID3D11Texture2D> {
        self.repetivel.posicao().and_then(|i| self.anel.get(i).cloned())
    }

    pub fn chegados(&self) -> u64 {
        self.estado.chegados.load(Ordering::Relaxed)
    }

    /// O aspecto em uso (a fase 5): decidido na abertura, e relido a cada troca de tipo.
    pub fn aspecto(&self) -> regras::Aspecto {
        self.estado.aspecto.lock().unwrap_or_else(|x| x.into_inner()).atual()
    }

    /// **O tamanho em pixel quadrado** da imagem, pelo aspecto em uso: é por ele que a cadeia
    /// decide o teto e a saída do conversor (720×480 a 16:9 → 854×480).
    pub fn tamanho_exibido(&self) -> (u32, u32) {
        regras::tamanho_exibido(self.width, self.height, self.aspecto().par)
    }

    /// Quantas vezes o aspecto mudou com a sessão no ar.
    pub fn trocas_de_aspecto(&self) -> u64 {
        self.estado.aspecto.lock().unwrap_or_else(|x| x.into_inner()).trocas()
    }

    /// Acaba a captura com um motivo de fora dela (o encoder que segura os quadros da câmera com
    /// conversor, `transmissao.rs`): o vigia da sessão encerra com o texto, como numa falha do leitor.
    pub fn encerrar(&self, motivo: String) {
        self.estado.falhar(motivo);
    }

    pub fn primeiro_quadro(&self) -> Option<Instant> {
        None
    }

    pub fn ultimo_quadro(&self) -> Option<Instant> {
        *self.estado.ultimo_chegado.lock().unwrap_or_else(|x| x.into_inner())
    }

    /// Quem desentrelaçou, e o que o adapt2 custou e fez (22/09).
    pub fn relato_do_desentrelacamento(&self) -> String {
        match (&self.desentrelacador, self.quem_desentrelaca) {
            (Some(d), _) => {
                let (n, soma, max) = self.custo_do_desentrelacador;
                format!(
                    "desentrelaça: adapt2 na CPU ({:?}) quadros={} bob={:.1}‰ cópia_com_adapt2 média={:.2} ms máx={:.2} ms",
                    d.ordem(),
                    d.quadros,
                    if d.pix_total == 0 { 0.0 } else { 1000.0 * d.pix_bob as f64 / d.pix_total as f64 },
                    if n == 0 { 0.0 } else { soma as f64 / n as f64 / 1000.0 },
                    max as f64 / 1000.0
                )
            }
            (None, regras::QuemDesentrelaca::Processador(e)) => format!("desentrelaça: o bob do processador ({e:?})"),
            (None, _) => "desentrelaça: ninguém".to_string(),
        }
    }

    /// Os contadores, para o registro do fim da sessão.
    pub fn relato(&self) -> String {
        let e = &self.estado;
        // Lido uma vez (a revisão curta do `08af2cd`, A9): a origem e a PAR do mesmo aspecto.
        let aspecto = self.aspecto();
        format!(
            "câmera: modo={:?} imagem={} chegados={} entregues={} sobrescritos_na_caixa={} vazios={} marcas_de_fluxo={} \
             trocas_de_tipo={} fora_do_ritmo={} dxgi={} memoria={} de_outra_placa={} falhas_de_copia={} subrecursos={:?} \
             ArraySize={:?} | carimbo={:?} idade_na_chegada {} | antes_da_origem={} forcados_pela_monotonia={} \
             implausiveis={} saltos_do_relogio={} voltas_do_relogio={} recortes={} | aspecto={:?} PAR {}:{} trocas_de_aspecto={} | {} | {}{}{}",
            self.modo,
            self.geometria.descricao(),
            e.chegados.load(Ordering::Relaxed),
            self.entregues,
            e.sobrescritos.load(Ordering::Relaxed),
            e.vazios.load(Ordering::Relaxed),
            e.ticks.load(Ordering::Relaxed),
            e.trocas_de_tipo.load(Ordering::Relaxed),
            self.fora_do_ritmo,
            self.dxgi,
            self.memoria,
            self.de_outra_placa,
            self.falhas_de_copia,
            self.subrecursos,
            self.arrays,
            self.carimbador.fonte(),
            self.carimbador.resumo_das_idades(),
            self.carimbador.antes_da_origem,
            self.carimbador.forcados,
            self.carimbador.implausiveis,
            self.carimbador.saltos,
            self.carimbador.voltas,
            self.recortes,
            aspecto.origem,
            aspecto.par.num,
            aspecto.par.den,
            self.trocas_de_aspecto(),
            self.relato_do_desentrelacamento(),
            self.relogios.resumo(),
            // A chegada média, da abertura ao último quadro (o fps que o leitor entregou).
            match self.ultimo_quadro().map(|u| u.saturating_duration_since(self.aberta_em)) {
                Some(d) if d > Duration::from_millis(500) => format!(
                    " | chegada média {:.1} fps em {:.1} s",
                    e.chegados.load(Ordering::Relaxed).saturating_sub(1) as f64 / d.as_secs_f64(),
                    d.as_secs_f64()
                ),
                _ => String::new(),
            },
            {
                let n = e.descartados_na_bancada.load(Ordering::Relaxed);
                let bancada = if n > 0 { format!(" | descartados_pela_pausa_de_bancada={n}") } else { String::new() };
                let fim = self
                    .fim()
                    .map(|f| format!(" | fim: {}{}", f.motivo, if f.desconectada { " (desconectada)" } else { "" }))
                    .unwrap_or_default();
                format!("{bancada}{fim}")
            }
        )
    }

    /// Para a captura: os contadores no registro e a soltura **fora desta thread** (o R4, M51). A
    /// thread da sessão segue para o `Bye` sem esperar o Frame Server.
    pub fn stop(&mut self) {
        if !self.estado.parado.load(Ordering::SeqCst) {
            registro::linha(self.relato());
        }
        self.soltar_fora();
    }

    fn soltar_fora(&mut self) {
        let Some(fonte) = self.fonte.take() else {
            return;
        };
        if let Some(v) = self.vaga.as_mut() {
            v.comecar_a_soltar();
        }
        // Os ajustes devolvem a câmera e soltam as interfaces antes do leitor e da fonte: a thread
        // deles é esperada **na thread da soltura**, e não nesta (a da sessão não espera o driver).
        let ajustes = self.ajustes.take().and_then(|a| a.encerrar());
        soltar_em_outra_thread(&self.estado, fonte, self.regua.take(), self.vaga.take(), ajustes);
    }
}

impl Drop for CapturaDeCamera {
    fn drop(&mut self) {
        self.soltar_fora();
    }
}

// =============================================================================================
// A cópia para o anel
// =============================================================================================

/// Por onde o quadro entrou no anel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaminhoDaCopia {
    /// Superfície DXGI da nossa placa: cópia na GPU **da fatia `subrecurso`** de um array de
    /// `fatias` (a revisão, G2); `recorta` quando a cópia não leva a superfície inteira.
    Gpu { subrecurso: u32, fatias: u32, recorta: bool },
    /// Memória de sistema (a fonte do Quall no processo entrega assim): subiu por uma textura de
    /// preparo.
    Memoria,
    /// Superfície de outro dispositivo: a cópia na GPU não vale, e o quadro subiu pela memória.
    OutraPlaca,
    /// Superfície da nossa placa com o adapt2 ligado (não esperado: o leitor dele não tem o
    /// gerenciador D3D): lida pela memória para desentrelaçar, em vez de ir com os campos tecidos.
    GpuPelaMemoria,
}

/// Copia a **imagem** do quadro de `amostra` — o retângulo `geometria` dele — para `destino`, uma
/// textura do anel do tamanho da imagem no `formato`, e diz por onde. É o corpo da cópia na
/// chegada, fora da captura para a sonda poder exercitá-lo com texturas sintéticas
/// (`quall_camera_local anel`) sem câmera nenhuma.
///
/// A superfície pode ser **maior** que o quadro que o tipo declara (a de um decodificador, alinhada
/// a 16): basta que contenha o retângulo da imagem (M3).
pub fn copiar_amostra(
    dispositivo: &ID3D11Device,
    contexto: &ID3D11DeviceContext,
    amostra: &IMFSample,
    destino: &ID3D11Texture2D,
    geometria: Geometria,
    formato: FormatoDoLeitor,
    staging: &mut Option<ID3D11Texture2D>,
) -> std::result::Result<CaminhoDaCopia, String> {
    copiar_amostra_desentrelacando(dispositivo, contexto, amostra, destino, geometria, formato, staging, None)
}

/// [`copiar_amostra`] com o adapt2 (22/09): com `desentrelacador`, o quadro YUY2 é desentrelaçado
/// **na subida pela memória** (`subir_da_memoria`), do buffer travado direto para o preparo. Uma
/// superfície da nossa placa, que iria pela GPU, é lida pela memória (`GpuPelaMemoria`).
#[allow(clippy::too_many_arguments)]
pub fn copiar_amostra_desentrelacando(
    dispositivo: &ID3D11Device,
    contexto: &ID3D11DeviceContext,
    amostra: &IMFSample,
    destino: &ID3D11Texture2D,
    geometria: Geometria,
    formato: FormatoDoLeitor,
    staging: &mut Option<ID3D11Texture2D>,
    mut desentrelacador: Option<&mut Desentrelacador>,
) -> std::result::Result<CaminhoDaCopia, String> {
    let g = geometria;
    unsafe {
        let buffer = amostra.GetBufferByIndex(0).map_err(|e| format!("GetBufferByIndex: {e}"))?;
        if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
            let sub = dxgi.GetSubresourceIndex().map_err(|e| format!("GetSubresourceIndex: {e}"))?;
            let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut p).map_err(|e| format!("GetResource: {e}"))?;
            if p.is_null() {
                return Err("GetResource devolveu nulo".into());
            }
            let origem = ID3D11Texture2D::from_raw(p);
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            origem.GetDesc(&mut desc);
            let (x1, y1) = (g.x + g.largura, g.y + g.altura);
            if desc.Format != formato.dxgi() {
                return Err(format!("a superfície é {:?}, e o anel é {:?}", desc.Format, formato.dxgi()));
            }
            // Menor que a imagem, ou maior que o quadro alinhado: recusada, com o motivo (M3 e a
            // reconferência: uma superfície maior sem limite sairia recortada, calada).
            let recorta = regras::superficie_aceita(desc.Width, desc.Height, &g).map_err(|e| format!("{e} ({})", g.descricao()))?;
            // A superfície tem de ser **do nosso dispositivo** (o leitor recebeu o gerenciador
            // dele); se não for, a cópia na GPU não vale, e o quadro sobe pela memória.
            let do_nosso = match (origem.GetDevice(), dispositivo.cast::<windows::core::IUnknown>()) {
                (Ok(d), Ok(nosso)) => d.cast::<windows::core::IUnknown>().map(|u| u.as_raw() == nosso.as_raw()).unwrap_or(false),
                _ => false,
            };
            if !do_nosso {
                // Travada pela memória, a superfície expõe o NV12 **dela**: o plano UV começa depois
                // das linhas da superfície, e não das do tipo (a reconferência da fase 3).
                subir_da_memoria(dispositivo, contexto, &buffer, destino, g.na_superficie(desc.Width, desc.Height), formato, staging, desentrelacador)?;
                return Ok(CaminhoDaCopia::OutraPlaca);
            }
            if desentrelacador.is_some() {
                // Pela memória, como a de outra placa: o `Lock2D` da superfície a lê de volta.
                subir_da_memoria(dispositivo, contexto, &buffer, destino, g.na_superficie(desc.Width, desc.Height), formato, staging, desentrelacador.take())?;
                return Ok(CaminhoDaCopia::GpuPelaMemoria);
            }
            if sub >= desc.ArraySize * desc.MipLevels.max(1) {
                return Err(format!("o subrecurso {sub} não existe num array de {} fatias", desc.ArraySize));
            }
            // **A fatia certa** do array (G2), e **só a imagem** dela (M3). A superfície inteira
            // vai sem caixa, como antes.
            let caixa = D3D11_BOX { left: g.x, top: g.y, front: 0, right: x1, bottom: y1, back: 1 };
            let inteira = (g.x, g.y, x1, y1) == (0, 0, desc.Width, desc.Height);
            contexto.CopySubresourceRegion(
                destino,
                0,
                0,
                0,
                0,
                &origem,
                sub,
                if inteira { None } else { Some(&caixa as *const D3D11_BOX) },
            );
            return Ok(CaminhoDaCopia::Gpu { subrecurso: sub, fatias: desc.ArraySize, recorta });
        }
        subir_da_memoria(dispositivo, contexto, &buffer, destino, g, formato, staging, desentrelacador)?;
        Ok(CaminhoDaCopia::Memoria)
    }
}

/// O quadro em memória de sistema para o anel: o retângulo da imagem, do plano Y e do UV (NV12) ou
/// das linhas YUY2, linha a linha, respeitando o passo dos dois lados, **e sem ler além do buffer**
/// — o tamanho vem do `IMF2DBuffer2::Lock2DSize` ou do `Lock`; só o `IMF2DBuffer` antigo não o diz.
/// O plano UV começa depois das linhas do **quadro** (`quadro_altura`), e não das da imagem.
unsafe fn subir_da_memoria(
    dispositivo: &ID3D11Device,
    contexto: &ID3D11DeviceContext,
    buffer: &IMFMediaBuffer,
    destino: &ID3D11Texture2D,
    g: Geometria,
    formato: FormatoDoLeitor,
    staging: &mut Option<ID3D11Texture2D>,
    desentrelacador: Option<&mut Desentrelacador>,
) -> std::result::Result<(), String> {
    unsafe {
        if staging.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: g.largura,
                Height: g.altura,
                MipLevels: 1,
                ArraySize: 1,
                Format: formato.dxgi(),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
            };
            let mut t: Option<ID3D11Texture2D> = None;
            dispositivo.CreateTexture2D(&desc, None, Some(&mut t)).map_err(|e| format!("staging: {e}"))?;
            *staging = t;
        }
        let preparo = staging.clone().ok_or("sem staging")?;
        let (qw, qh) = (g.quadro_largura as usize, g.quadro_altura as usize);
        let (x, y, w, h) = (g.x as usize, g.y as usize, g.largura as usize, g.altura as usize);
        let bytes_por_pixel = match formato {
            FormatoDoLeitor::Nv12 | FormatoDoLeitor::I420 => 1,
            FormatoDoLeitor::Yuy2 => 2,
        };
        let linha_do_quadro = qw * bytes_por_pixel;
        let linha_da_imagem = w * bytes_por_pixel;
        // Quantas linhas de `passo` o quadro ocupa: Y e UV (NV12), ou as linhas YUY2. O I420 ocupa
        // o mesmo que o NV12 (U e V com metade do passo cada, `regras::tamanho_i420`).
        let linhas = match formato {
            FormatoDoLeitor::Nv12 | FormatoDoLeitor::I420 => qh + qh / 2,
            FormatoDoLeitor::Yuy2 => qh,
        };
        enum Trava {
            DoisD2(IMF2DBuffer2),
            DoisD(IMF2DBuffer),
            Linear,
        }
        let mut base: *mut u8 = std::ptr::null_mut();
        let mut passo: isize = linha_do_quadro as isize;
        let trava = if let Ok(b2) = buffer.cast::<IMF2DBuffer2>() {
            let mut p: i32 = 0;
            let mut inicio: *mut u8 = std::ptr::null_mut();
            let mut tamanho = 0u32;
            b2.Lock2DSize(MF2DBuffer_LockFlags_Read, &mut base, &mut p, &mut inicio, &mut tamanho)
                .map_err(|e| format!("Lock2DSize: {e}"))?;
            let disponivel = (tamanho as isize) - (base as isize - inicio as isize);
            // Até o último byte lido: no I420, o fim da última linha do V (com passo de sobra, o
            // V passa do que o cálculo do NV12 cobre).
            let precisa = match formato {
                FormatoDoLeitor::I420 if p > 0 => regras::fim_i420(p as usize, qw, qh) as isize,
                _ => (p as isize) * (linhas as isize - 1) + linha_do_quadro as isize,
            };
            if p < linha_do_quadro as i32 || disponivel < precisa {
                let _ = b2.Unlock2D();
                return Err(format!("buffer 2D com passo {p} e {disponivel} bytes para {qw}x{qh} {formato:?}"));
            }
            passo = p as isize;
            Trava::DoisD2(b2)
        } else if let Ok(b) = buffer.cast::<IMF2DBuffer>() {
            let mut p: i32 = 0;
            b.Lock2D(&mut base, &mut p).map_err(|e| format!("Lock2D: {e}"))?;
            if p < linha_do_quadro as i32 {
                let _ = b.Unlock2D();
                return Err(format!("buffer 2D com passo {p} para {qw}x{qh} {formato:?}"));
            }
            passo = p as isize;
            Trava::DoisD(b)
        } else {
            let mut tamanho = 0u32;
            buffer.Lock(&mut base, None, Some(&mut tamanho)).map_err(|e| format!("Lock: {e}"))?;
            if (tamanho as usize) < linha_do_quadro * linhas {
                let _ = buffer.Unlock();
                return Err(format!("buffer de {tamanho} bytes para {qw}x{qh} {formato:?}"));
            }
            Trava::Linear
        };
        let resultado = (|| -> std::result::Result<(), String> {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            contexto.Map(&preparo, 0, D3D11_MAP_WRITE, 0, Some(&mut m)).map_err(|e| format!("Map: {e}"))?;
            let passo_destino = m.RowPitch as usize;
            let alvo = m.pData as *mut u8;
            let deslocamento_x = (x * bytes_por_pixel) as isize;
            match desentrelacador {
                // **O adapt2 (22/09)**: do buffer travado direto para o preparo, sem a cópia crua.
                // O tamanho do buffer foi conferido acima (as `linhas` de `passo`), e a imagem cabe
                // no quadro (`Geometria`); o preparo tem `h` linhas de `passo_destino`.
                Some(d) if formato == FormatoDoLeitor::Yuy2 => {
                    if (d.largura(), d.altura()) != (w, h) {
                        contexto.Unmap(&preparo, 0);
                        return Err(format!("o adapt2 é de {}x{}, e a imagem é {w}x{h}", d.largura(), d.altura()));
                    }
                    let entrada = std::slice::from_raw_parts(
                        base.offset(y as isize * passo + deslocamento_x),
                        passo as usize * (h - 1) + linha_da_imagem,
                    );
                    let saida = std::slice::from_raw_parts_mut(alvo, passo_destino * (h - 1) + linha_da_imagem);
                    d.yuy2(entrada, passo as usize, saida, passo_destino);
                }
                Some(_) => {
                    // Não acontece hoje (`quem_desentrelaca` exige YUY2, e a troca de tipo é
                    // recusada); se acontecer, falha com o motivo em vez de mandar os campos tecidos
                    // a um conversor que os acha progressivos.
                    contexto.Unmap(&preparo, 0);
                    return Err(format!("o adapt2 só lê YUY2, e o quadro é {formato:?}"));
                }
                None => {
                    for linha in 0..h {
                        std::ptr::copy_nonoverlapping(
                            base.offset((y + linha) as isize * passo + deslocamento_x),
                            alvo.add(linha * passo_destino),
                            linha_da_imagem,
                        );
                    }
                }
            }
            if formato == FormatoDoLeitor::Nv12 {
                // O plano UV começa depois de `quadro_altura` linhas na origem e de `altura` no
                // preparo (a mesma suposição de `escala_nv12.rs`; medida certa a 1920x1080 no Quick
                // Sync, §4.1). Um par UV por dois pixels: o deslocamento em bytes é o mesmo x.
                let uv_origem = base.offset(qh as isize * passo);
                let uv_destino = alvo.add(h * passo_destino);
                for linha in 0..h / 2 {
                    std::ptr::copy_nonoverlapping(
                        uv_origem.offset((y / 2 + linha) as isize * passo + x as isize),
                        uv_destino.add(linha * passo_destino),
                        w,
                    );
                }
            } else if formato == FormatoDoLeitor::I420 {
                // **O I420 vira NV12 aqui** (a fase 5, a Canon): o U e o V, cada um com metade do
                // passo e metade das linhas, um depois do outro (`regras::planos_i420`), entram
                // entrelaçados na linha UV do preparo. `w / 2` amostras de cada, a partir de `x / 2`.
                // O tamanho do buffer já foi conferido acima: o I420 ocupa o mesmo que o NV12.
                let (inicio_u, inicio_v, passo_uv) = regras::planos_i420(passo as usize, qh);
                let uv_destino = alvo.add(h * passo_destino);
                for linha in 0..h / 2 {
                    let deslocamento = (y / 2 + linha) * passo_uv + x / 2;
                    let u = std::slice::from_raw_parts(base.add(inicio_u + deslocamento), w / 2);
                    let v = std::slice::from_raw_parts(base.add(inicio_v + deslocamento), w / 2);
                    let destino = std::slice::from_raw_parts_mut(uv_destino.add(linha * passo_destino), w);
                    regras::entrelacar_uv(u, v, destino);
                }
            }
            contexto.Unmap(&preparo, 0);
            contexto.CopyResource(destino, &preparo);
            Ok(())
        })();
        match trava {
            Trava::DoisD2(b) => {
                let _ = b.Unlock2D();
            }
            Trava::DoisD(b) => {
                let _ = b.Unlock2D();
            }
            Trava::Linear => {
                let _ = buffer.Unlock();
            }
        }
        resultado
    }
}

/// Uma textura do anel no formato do leitor. O NV12 precisa ser lido pelo MFT (como superfície DXGI)
/// e pelo processador de vídeo; o YUY2 só pelo processador. Tenta do mais completo ao mais simples.
pub fn textura_do_anel(dispositivo: &ID3D11Device, formato: DXGI_FORMAT, largura: u32, altura: u32) -> WinResult<ID3D11Texture2D> {
    let mut ultimo = None;
    for bind in [
        (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        D3D11_BIND_SHADER_RESOURCE.0 as u32,
        D3D11_BIND_RENDER_TARGET.0 as u32,
        D3D11_BIND_DECODER.0 as u32,
    ] {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: largura,
            Height: altura,
            MipLevels: 1,
            ArraySize: 1,
            Format: formato,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut t: Option<ID3D11Texture2D> = None;
        match unsafe { dispositivo.CreateTexture2D(&desc, None, Some(&mut t)) } {
            Ok(()) => {
                if let Some(t) = t {
                    return Ok(t);
                }
            }
            Err(e) => ultimo = Some(e),
        }
    }
    Err(ultimo.unwrap_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_POINTER, "sem textura")))
}

#[cfg(test)]
mod testes {
    use super::*;

    /// **A vez, com threads de verdade** (o R4, M51): a segunda abertura da mesma câmera espera a
    /// primeira acabar de abrir e sai compartilhada; o Parar tira da espera; a soltura em curso
    /// segura a vez de quem recomeça. As regras puras estão em `regras_da_camera`; isto é o
    /// `Condvar` e o `static` em volta delas.
    #[test]
    fn r4_a_vez_passa_de_uma_thread_para_outra() {
        let link = r"\\?\teste#a-vez-de-abrir#r4";
        let (plano, vez) = esperar_a_vez(link, &|| false).unwrap();
        assert_eq!(plano, PlanoDaAbertura::ControladoraComRecuo);
        let t0 = Instant::now();
        let outra = std::thread::spawn(move || {
            let (plano, vez) = esperar_a_vez(link, &|| false).unwrap();
            (plano, t0.elapsed(), vez)
        });
        std::thread::sleep(Duration::from_millis(300));
        let mut vaga = vez.abriu(Papel::Controladora);
        let (plano2, esperou, vez2) = outra.join().unwrap();
        assert_eq!(plano2, PlanoDaAbertura::SoCompartilhada { outras: 1 });
        assert!(esperou >= Duration::from_millis(300), "a segunda esperou a primeira abrir: {esperou:?}");
        let vaga2 = vez2.abriu(Papel::Compartilhada);
        assert_eq!(aberturas().estado(link), (2, 0, false));

        // O Parar tira da espera, em uma fatia.
        let (_, abrindo) = esperar_a_vez(link, &|| false).unwrap();
        let parar = AtomicBool::new(false);
        let comeco = Instant::now();
        std::thread::scope(|e| {
            e.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                parar.store(true, Ordering::SeqCst);
            });
            let r = esperar_a_vez(link, &|| parar.load(Ordering::SeqCst));
            assert_eq!(r.err().map(|f| f.causa), Some(CausaDaFalha::Cancelada));
        });
        assert!(comeco.elapsed() < Duration::from_millis(100) + regras::FATIA_DA_ESPERA * 4);
        drop(abrindo);
        assert_eq!(aberturas().estado(link), (2, 0, false), "a abertura que não abriu não conta");

        // Quem recomeça espera a soltura em curso da mesma câmera.
        vaga.comecar_a_soltar();
        drop(vaga2);
        assert_eq!(aberturas().estado(link), (0, 1, false));
        let soltando = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(vaga);
        });
        let comeco = Instant::now();
        let (plano3, vez3) = esperar_a_vez(link, &|| false).unwrap();
        assert!(comeco.elapsed() >= Duration::from_millis(150), "esperou a soltura: {:?}", comeco.elapsed());
        assert_eq!(plano3, PlanoDaAbertura::ControladoraComRecuo, "a que saiu não conta mais");
        soltando.join().unwrap();
        drop(vez3);
        assert_eq!(aberturas().estado(link), (0, 0, false));
    }

    /// **A criação da fonte pela thread própria** (M54) devolve a falha dela à abertura, com o
    /// Parar sendo olhado (e o sinal de vida batido) enquanto espera. Um link que não existe: nenhuma
    /// câmera é aberta, e a falha volta pelo canal, sem virar "Parar".
    #[test]
    fn m54_a_criacao_da_fonte_pela_thread_devolve_a_falha() {
        let comeco = Instant::now();
        let r = criar_fonte_olhando(&FonteDaCamera::Link(r"\\?\teste#m54#naoexiste".into()), Modo::Controladora, &|| false);
        let f = r.err().expect("um link que não existe não vira fonte");
        assert_ne!(f.causa, CausaDaFalha::Cancelada, "{}", f.texto);
        assert!(f.texto.contains("MFCreateDeviceSource") || f.texto.contains("MFCreateAttributes"), "{}", f.texto);
        assert!(comeco.elapsed() < Duration::from_secs(5), "{:?}", comeco.elapsed());
        // Com o Parar já pedido, a espera sai em uma fatia, quer a criação volte antes ou não.
        let r = criar_fonte_olhando(&FonteDaCamera::Link(r"\\?\teste#m54#naoexiste".into()), Modo::Controladora, &|| true);
        assert!(r.is_err());
        // **A thread criadora entra na conta das solturas pendentes** e sai dela quando acaba, também
        // a abandonada pelo Parar (a revisão do código da fase 4, m1): o `MFShutdown` da saída espera.
        assert_eq!(esperar_solturas(Duration::from_secs(5)), 0, "a conta volta a zero");
    }
}
