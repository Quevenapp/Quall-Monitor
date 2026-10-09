//! **O dono da captura do R5 no Windows** (`docs/teleprompter-com-camera.md` §2.4 e §8.10, peça 2).
//!
//! Até a fase 4 a câmera do Windows só abria **dentro** da sessão (`Cadeia::abrir_com`, depois do
//! pareamento, G1). A tela R5 precisa dela aberta com a tela: a prévia, a rede que se pendura e se
//! solta, e o gravador que grava sem receptor. Quatro coisas mudam de dono (§2.4):
//!
//! 1. **O bombeio.** Uma thread própria (`quall.r5.dono`) chama `take_frame` quando o leitor avisa, e
//!    entrega o quadro às saídas penduradas. A `Cadeia` da rede vira **leitora** ([`LeitorDoDono`]).
//! 2. **A GPU.** O dono escolhe a placa — **a Intel primeiro** (a S-W1: dois Quick Sync a 30 fps; o
//!    MFT da NVIDIA falha com `0x8000FFFF`), cada uma conferida ativando um H.264 nela — e abre a
//!    captura nela. A cadeia e o `IMFSinkWriter` herdam o dispositivo e o gerenciador.
//! 3. **O zero de relógio** (`origem`), criado na abertura: a rede, o microfone e o gravador falam do
//!    mesmo instante.
//! 4. **Uma câmera por processo**: com a tela R5 aberta a janela principal não espelha (nem câmera,
//!    nem tela: os dois anúncios teriam o mesmo nome, a revisão do plano, M7), e a tela R5 recusa abrir
//!    com a janela principal emitindo — **uma troca atômica só** ([`tomar_para_r5`],
//!    [`tomar_para_a_janela`]; a revisão do código).
//!
//! # Os três leitores do anel, e a regra do R18
//!
//! O anel da `CapturaDeCamera` tem 6 posições, escritas só em `take_frame`, agora na thread do dono:
//!
//! - **a rede lê o anel direto**, como antes: o anel foi dimensionado para ela (a caixa, o pendente,
//!   o que o MFT segura, uma de folga), e o ritmo em que ele é reescrito continua o da câmera;
//! - **a prévia lê o anel direto**, no mesmo contexto imediato em que a cópia de `take_frame`
//!   escreve, e na mesma thread (`previa_da_camera.rs`): a posição só volta a ser escrita 6 quadros
//!   depois, e o D3D11 ordena os dois no contexto;
//! - **o gravador recebe uma cópia** na reserva dele (`gravador_local.rs`): o `IMFSinkWriter` segura
//!   amostras por um tempo que não controlamos, e a textura só volta à reserva quando o Media
//!   Foundation solta a amostra.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::capture::{CapturedFrame, Posse};
use crate::captura_de_camera::{CapturaDeCamera, FonteDaCamera, FormatoDoLeitor, Modo};
use crate::regras_da_camera::{self as regras, Entrelacamento, FimDaCamera, TetoDaCamera};
use crate::{device, encoder, registro};

/// **Quem emite neste processo**: ninguém, a janela principal (tela ou câmera, esperando ou no ar) ou
/// a tela R5. Uma câmera por processo, e os dois anúncios teriam o mesmo nome de instância (a revisão
/// do plano, M7). Uma troca atômica só: as duas bandeiras de antes deixavam uma corrida (a revisão do
/// código).
static QUEM_EMITE: AtomicU8 = AtomicU8::new(NINGUEM);
const NINGUEM: u8 = 0;
const A_JANELA: u8 = 1;
const A_TELA_R5: u8 = 2;

/// A tela R5 toma a emissão; `false` se a janela principal já emite.
pub fn tomar_para_r5() -> bool {
    QUEM_EMITE.compare_exchange(NINGUEM, A_TELA_R5, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}
pub fn soltar_da_r5() {
    let _ = QUEM_EMITE.compare_exchange(A_TELA_R5, NINGUEM, Ordering::SeqCst, Ordering::SeqCst);
}
/// A janela principal toma a emissão; `false` se a tela R5 está aberta. Tomar de novo (já dela) vale.
pub fn tomar_para_a_janela() -> bool {
    match QUEM_EMITE.compare_exchange(NINGUEM, A_JANELA, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => true,
        Err(atual) => atual == A_JANELA,
    }
}
pub fn soltar_da_janela() {
    let _ = QUEM_EMITE.compare_exchange(A_JANELA, NINGUEM, Ordering::SeqCst, Ordering::SeqCst);
}
/// A tela R5 está aberta neste processo?
pub fn tela_r5_aberta() -> bool {
    QUEM_EMITE.load(Ordering::SeqCst) == A_TELA_R5
}

/// A frase do Espelhar da janela principal com a tela R5 aberta.
/// Em português, como chave da tabela: quem mostra traduz com `idioma::t` (a tradução EN/PT).
pub const FRASE_DA_CAMERA_OCUPADA_PELA_R5: &str = "O teleprompter com câmera está aberto e transmite a câmera deste computador. Feche aquela tela para espelhar daqui."; // i18n: chave
/// A frase da tela R5 com a janela principal emitindo.
/// Em português, como chave da tabela: quem mostra traduz com `idioma::t` (a tradução EN/PT).
pub const FRASE_DA_CAMERA_OCUPADA_PELA_JANELA: &str = "A janela principal está transmitindo (ou esperando para transmitir). Pare lá para abrir o texto com a câmera."; // i18n: chave

/// **O teto da melhor imagem** (§8.10, peça 2): até 3840x2160 a 30 fps, escolhido do que a câmera
/// oferece (`escolher_tipo_nativo`: fluido primeiro, depois a maior área que cabe). A carga de um 4K
/// com dois codificadores é hipótese.
pub const TETO_DA_MELHOR_IMAGEM: TetoDaCamera = TetoDaCamera { max_macroblocos: (3840 / 16) * (2160 / 16), fps: 30 };

/// **Um objeto COM entre threads**, com a razão escrita: o processo é todo MTA (`quall_app.rs`, e
/// cada thread do R5 entra em MTA), e o Media Foundation e o `IMFDXGIDeviceManager` são livres de
/// apartamento. O crate `windows` não marca as interfaces do Media Foundation como `Send`.
pub struct Enviavel<T>(pub T);
// SAFETY: ver o comentário do tipo. Cada uso é de uma thread por vez, ou de objetos que o Media
// Foundation documenta como seguros entre threads (o gerenciador DXGI é feito para isso).
unsafe impl<T> Send for Enviavel<T> {}
unsafe impl<T> Sync for Enviavel<T> {}

/// A placa do dono: o dispositivo, o contexto, o gerenciador DXGI e de onde vieram.
#[derive(Clone)]
pub struct PlacaDoDono {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub gerenciador: Arc<Enviavel<IMFDXGIDeviceManager>>,
    pub luid: u64,
    pub vendor_id: u32,
    pub descricao: String,
    /// O H.264 que ativou na conferência (desligado em seguida).
    pub encoder: String,
}

impl PlacaDoDono {
    /// O `ChosenAdapter` que a `Cadeia` espera, sobre o mesmo dispositivo.
    pub fn como_adaptador(&self) -> device::ChosenAdapter {
        device::ChosenAdapter {
            device: self.device.clone(),
            context: self.context.clone(),
            description: self.descricao.clone(),
            vendor_id: self.vendor_id,
            luid: self.luid,
        }
    }
}

/// O que se sabe da câmera aberta, fixo pela vida do dono.
#[derive(Clone, Debug)]
pub struct InfoDaCamera {
    pub largura: u32,
    pub altura: u32,
    pub formato: FormatoDoLeitor,
    pub faixa_completa: bool,
    pub matriz_709: bool,
    pub entrelacamento_no_anel: Entrelacamento,
    pub modo: Modo,
    pub descricao: String,
}

/// Em que pé o dono está.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaseDoDono {
    /// Escolhendo a placa e abrindo a câmera (até ~10 s num nó recém-criado, M54).
    Abrindo,
    Aberto,
    /// Não abriu: a frase para a tela.
    Falhou(String),
    /// Abriu e acabou (desconectada, tomada, o formato mudou): a frase para a tela.
    Acabou(String),
    Fechado,
}

/// **Uma saída do dono** que recebe o quadro na thread dele (a prévia e o gravador). A rede não é
/// uma destas: ela **puxa** do [`LeitorDoDono`], na thread da sessão.
pub trait SaidaDoDono: Send {
    /// O quadro acabou de ser copiado para o anel. A textura é a posição do anel: **não a guarde**.
    fn quadro(&mut self, q: &CapturedFrame, placa: &PlacaDoDono);
    /// O nome, para a linha de 10 s.
    fn nome(&self) -> &'static str;
    /// Uma linha de 10 s própria, se tiver.
    fn relato(&mut self) -> Option<String> {
        None
    }
}

/// Quantas posições o anel da rede tem: a caixa (1), o pendente da cadeia (1), a repetição da
/// câmera parada (1) e uma de folga. O MFT **nunca** segura posição daqui: a cadeia do dono passa
/// sempre pelo conversor (`transmissao.rs`), e a posição só vive do `take_frame` até o `Blt` dele.
const POSICOES_DA_REDE: usize = 4;

/// **A caixa da rede, com o anel dela** (a revisão do plano, B1): o dono copia a posição do anel da
/// câmera para uma posição **deste** anel que ninguém segura (a posse de `CapturedFrame`), e põe o
/// quadro na caixa. Sem posição livre, a rede perde o quadro e conta: o dono nunca escreve debaixo
/// de quem ainda lê.
struct CaixaDaRede {
    caixa: Mutex<Option<CapturedFrame>>,
    aviso: Sender<()>,
    anel: Vec<ID3D11Texture2D>,
    posses: Vec<Arc<AtomicU32>>,
    /// `(a próxima a tentar, a última escrita)`: só o dono escreve.
    posicao: Mutex<(usize, Option<usize>)>,
    sobrescritos: AtomicU64,
    sem_posicao: AtomicU64,
    copias: AtomicU64,
}

impl CaixaDaRede {
    /// O dono escreve o quadro numa posição livre e o põe na caixa (o que estava nela sai e solta a
    /// posse dele).
    fn entregar(&self, q: &CapturedFrame, placa: &PlacaDoDono) {
        let velho = self.caixa.lock().unwrap_or_else(|e| e.into_inner()).take();
        if velho.is_some() {
            self.sobrescritos.fetch_add(1, Ordering::Relaxed);
        }
        drop(velho);
        let mut pos = self.posicao.lock().unwrap_or_else(|e| e.into_inner());
        let n = self.anel.len();
        let livre = (0..n).map(|k| (pos.0 + k) % n).find(|i| self.posses[*i].load(Ordering::SeqCst) == 0);
        let Some(i) = livre else {
            self.sem_posicao.fetch_add(1, Ordering::Relaxed);
            return;
        };
        unsafe { placa.context.CopyResource(&self.anel[i], &q.texture) };
        self.copias.fetch_add(1, Ordering::Relaxed);
        *pos = ((i + 1) % n, Some(i));
        drop(pos);
        let quadro = CapturedFrame { texture: self.anel[i].clone(), captured_at: q.captured_at, posse: Some(Posse::nova(Arc::clone(&self.posses[i]))) };
        *self.caixa.lock().unwrap_or_else(|e| e.into_inner()) = Some(quadro);
        let _ = self.aviso.try_send(());
    }

    /// A última posição escrita, com posse: a repetição da câmera parada.
    fn ultima(&self) -> Option<(ID3D11Texture2D, Posse)> {
        let pos = self.posicao.lock().unwrap_or_else(|e| e.into_inner());
        pos.1.map(|i| (self.anel[i].clone(), Posse::nova(Arc::clone(&self.posses[i]))))
    }
}

/// A captura, entre threads (ver [`Enviavel`]): a fonte e o leitor do Media Foundation são livres de
/// apartamento, e a captura só é usada sob o cadeado.
struct CapturaEnviavel(CapturaDeCamera);
// SAFETY: ver o comentário do tipo.
unsafe impl Send for CapturaEnviavel {}

/// As estatísticas da janela de 10 s.
#[derive(Default)]
struct Janela {
    comeco: Option<Instant>,
    quadros: u64,
    ultimo: Option<Instant>,
    buraco_maior: Duration,
    custo_us: [(u64, u64); 3],
}

struct Comum {
    parar: AtomicBool,
    fase: Mutex<FaseDoDono>,
    placa: Mutex<Option<PlacaDoDono>>,
    info: Mutex<Option<InfoDaCamera>>,
    captura: Mutex<Option<CapturaEnviavel>>,
    rede: Mutex<Option<Arc<CaixaDaRede>>>,
    previa: Mutex<Option<Box<dyn SaidaDoDono>>>,
    gravador: Mutex<Option<Box<dyn SaidaDoDono>>>,
    acordar: Box<dyn Fn() + Send + Sync>,
    /// Quantos quadros saíram do anel desde a abertura (a prova de que a câmera não cicla).
    entregues: AtomicU64,
    /// O último quadro entregue (µs desde `origem`), para a tela e o gravador.
    ultimo_us: AtomicU64,
    /// O maior intervalo entre dois quadros seguidos desde a abertura, em µs.
    buraco_maior_us: AtomicU64,
    /// **A câmera sumiu pela interface** (a revisão do plano, M9): a interface desabilitada (ou fora
    /// do gerenciador, confirmada), lida pelo dono a cada segundo com a `TestemunhaDaInterface` — a
    /// mesma do emissor. Vale para as três saídas, com o leitor ainda sem fim.
    fim_da_interface: Mutex<Option<String>>,
    /// Os quadros por segundo da última janela de 10 s (o fps nominal da gravação).
    fps_medido_milesimos: AtomicU64,
}

/// O dono. Criado com [`DonoDaCaptura::abrir`], que volta na hora: a placa e a câmera abrem na
/// thread dele, e a fase diz quando terminaram.
pub struct DonoDaCaptura {
    comum: Arc<Comum>,
    /// O zero de relógio do R5: a rede, o microfone e o gravador o adotam.
    pub origem: Instant,
    /// O nome da câmera, para a tela e o rótulo da track.
    pub nome: String,
    /// O link (ou `sintetica`).
    pub id: String,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DonoDaCaptura {
    /// Abre o dono **em segundo plano**: a thread escolhe a placa, abre a câmera e começa o
    /// bombeio. `acordar` é chamado a cada mudança de fase (a tela redesenha).
    pub fn abrir(fonte: FonteDaCamera, nome: String, id: String, teto: TetoDaCamera, acordar: Box<dyn Fn() + Send + Sync>) -> Arc<DonoDaCaptura> {
        let comum = Arc::new(Comum {
            parar: AtomicBool::new(false),
            fase: Mutex::new(FaseDoDono::Abrindo),
            placa: Mutex::new(None),
            info: Mutex::new(None),
            captura: Mutex::new(None),
            rede: Mutex::new(None),
            previa: Mutex::new(None),
            gravador: Mutex::new(None),
            acordar,
            entregues: AtomicU64::new(0),
            ultimo_us: AtomicU64::new(0),
            buraco_maior_us: AtomicU64::new(0),
            fim_da_interface: Mutex::new(None),
            fps_medido_milesimos: AtomicU64::new(0),
        });
        let origem = Instant::now();
        let dono = Arc::new(DonoDaCaptura { comum: Arc::clone(&comum), origem, nome: nome.clone(), id, thread: Mutex::new(None) });
        let c = Arc::clone(&comum);
        let link = match &fonte {
            FonteDaCamera::Link(l) => Some(l.clone()),
            _ => None,
        };
        let h = std::thread::Builder::new()
            .name("quall.r5.dono".into())
            .spawn(move || correr(c, fonte, nome, teto, origem, link));
        match h {
            Ok(h) => *dono.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(h),
            Err(e) => {
                *comum.fase.lock().unwrap_or_else(|e| e.into_inner()) = FaseDoDono::Falhou(crate::idioma::tf("a thread da câmera não subiu: {}", &[&e]));
            }
        }
        dono
    }

    pub fn fase(&self) -> FaseDoDono {
        self.comum.fase.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn placa(&self) -> Option<PlacaDoDono> {
        self.comum.placa.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn info(&self) -> Option<InfoDaCamera> {
        self.comum.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Quantos quadros saíram do anel desde a abertura.
    pub fn entregues(&self) -> u64 {
        self.comum.entregues.load(Ordering::Relaxed)
    }

    /// O maior intervalo entre dois quadros seguidos desde a abertura.
    pub fn buraco_maior(&self) -> Duration {
        Duration::from_micros(self.comum.buraco_maior_us.load(Ordering::Relaxed))
    }

    /// Há quanto tempo o último quadro chegou (`None` antes do primeiro).
    pub fn ultimo_quadro_ha(&self) -> Option<Duration> {
        let u = self.comum.ultimo_us.load(Ordering::Relaxed);
        if u == 0 {
            return None;
        }
        Some(self.origem.elapsed().saturating_sub(Duration::from_micros(u)))
    }

    /// A câmera parada (sem quadro há 3 s, `regras_da_camera::SEM_QUADRO`), e há quanto tempo.
    pub fn parada_ha(&self) -> Option<Duration> {
        self.com_captura(|c| c.parada_ha(Instant::now())).flatten()
    }

    /// Como a câmera acabou, se acabou: o leitor, ou a interface que sumiu.
    pub fn fim(&self) -> Option<FimDaCamera> {
        if let Some(f) = self.com_captura(|c| c.fim()).flatten() {
            return Some(f);
        }
        self.comum
            .fim_da_interface
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map(|m| FimDaCamera::pelo_codigo(m, true, Some(regras::HR_REMOVIDA)))
    }

    /// O tamanho da imagem em pixel quadrado (o DV anamórfico sai mais largo), pelo aspecto em uso.
    pub fn tamanho_exibido(&self) -> Option<(u32, u32)> {
        self.com_captura(|c| c.tamanho_exibido())
    }

    /// O fps medido da câmera na última janela de 10 s (30 antes da primeira).
    pub fn fps_medido(&self) -> f64 {
        match self.comum.fps_medido_milesimos.load(Ordering::Relaxed) {
            0 => 30.0,
            m => m as f64 / 1000.0,
        }
    }

    /// **Os ajustes da câmera aberta** (R9, `docs/controles-de-camera.md`): a ponta que a janela
    /// "Ajustes da câmera" lê e em que pede os gestos. `None` antes de abrir, depois de fechar, e na
    /// fonte do Quall no processo (sem controles, pelo tipo).
    pub fn ajustes(&self) -> Option<crate::ajustes_da_camera::PontaDosAjustes> {
        self.com_captura(|c| c.ajustes()).flatten()
    }

    /// A vaga da prévia está ocupada? (A câmera comum a tem livre; a tela R5 a usa.)
    pub fn previa_pendurada(&self) -> bool {
        self.comum.previa.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn com_captura<R>(&self, f: impl FnOnce(&CapturaDeCamera) -> R) -> Option<R> {
        let g = self.comum.captura.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().map(|c| f(&c.0))
    }

    /// **Pendura a rede**: devolve o leitor que a `Cadeia` lê (`OrigemDaCadeia::DoDono`). Uma rede
    /// só: pendurar de novo solta a anterior. `None` antes de a câmera abrir.
    pub fn pendurar_rede(self: &Arc<Self>) -> Option<LeitorDoDono> {
        let info = self.info()?;
        let placa = self.placa()?;
        let (tx, rx) = bounded::<()>(1);
        let mut anel = Vec::with_capacity(POSICOES_DA_REDE);
        for _ in 0..POSICOES_DA_REDE {
            match crate::captura_de_camera::textura_do_anel(&placa.device, info.formato.dxgi(), info.largura, info.altura) {
                Ok(t) => anel.push(t),
                Err(e) => {
                    registro::linha(format!("r5 dono: !! o anel da rede não foi criado: {e}"));
                    return None;
                }
            }
        }
        let caixa = Arc::new(CaixaDaRede {
            caixa: Mutex::new(None),
            aviso: tx,
            anel,
            posses: (0..POSICOES_DA_REDE).map(|_| Arc::new(AtomicU32::new(0))).collect(),
            posicao: Mutex::new((0, None)),
            sobrescritos: AtomicU64::new(0),
            sem_posicao: AtomicU64::new(0),
            copias: AtomicU64::new(0),
        });
        *self.comum.rede.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&caixa));
        registro::linha("r5 dono: a rede se pendurou no dono (a câmera não é tocada)");
        Some(LeitorDoDono { frame_ready: rx, caixa, dono: Arc::clone(self), info, placa, fim_local: Mutex::new(None), solto: false })
    }

    fn soltar_rede(&self, caixa: &Arc<CaixaDaRede>) {
        let mut g = self.comum.rede.lock().unwrap_or_else(|e| e.into_inner());
        if g.as_ref().is_some_and(|c| Arc::ptr_eq(c, caixa)) {
            *g = None;
            registro::linha(format!(
                "r5 dono: a rede se soltou do dono (a câmera continua; copias={} sobrescritos_na_caixa={} sem_posicao_livre={})",
                caixa.copias.load(Ordering::Relaxed),
                caixa.sobrescritos.load(Ordering::Relaxed),
                caixa.sem_posicao.load(Ordering::Relaxed)
            ));
        }
    }

    /// A rede está pendurada agora?
    pub fn rede_pendurada(&self) -> bool {
        self.comum.rede.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Pendura (ou solta, com `None`) a prévia. Ela passa a receber os quadros na thread do dono.
    pub fn pendurar_previa(&self, saida: Option<Box<dyn SaidaDoDono>>) {
        let velha = std::mem::replace(&mut *self.comum.previa.lock().unwrap_or_else(|e| e.into_inner()), saida);
        drop(velha);
    }

    /// Pendura (ou solta, com `None`) o gravador. Devolve a saída que estava (para ela ser largada
    /// fora do cadeado de quem chama).
    pub fn pendurar_gravador(&self, saida: Option<Box<dyn SaidaDoDono>>) -> Option<Box<dyn SaidaDoDono>> {
        std::mem::replace(&mut *self.comum.gravador.lock().unwrap_or_else(|e| e.into_inner()), saida)
    }

    pub fn gravador_pendurado(&self) -> bool {
        self.comum.gravador.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Pede o fim (a thread para de bombear, solta as saídas e a câmera) e volta na hora.
    pub fn pedir_fechar(&self) {
        self.comum.parar.store(true, Ordering::SeqCst);
    }

    /// A thread do dono acabou?
    pub fn terminou(&self) -> bool {
        self.thread.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }

    /// Espera a thread acabar, até `prazo`. **Quem é dona de janela não chama isto**: a prévia
    /// apresenta numa janela filha, e a DXGI pode mandar mensagem a ela; a tela espera bombeando
    /// mensagens (`teleprompter/tela.rs`).
    pub fn esperar(&self, prazo: Duration) -> bool {
        let fim = Instant::now() + prazo;
        while !self.terminou() {
            if Instant::now() >= fim {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(h) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
        true
    }
}

impl Drop for DonoDaCaptura {
    fn drop(&mut self) {
        self.comum.parar.store(true, Ordering::SeqCst);
    }
}

/// A thread do dono: a placa, a câmera, e o bombeio.
fn correr(comum: Arc<Comum>, fonte: FonteDaCamera, nome: String, teto: TetoDaCamera, origem: Instant, link: Option<String>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let abrir = || -> Result<(PlacaDoDono, CapturaDeCamera), String> {
        let placa = escolher_placa()?;
        registro::linha(format!(
            "r5 dono: a placa é \"{}\" (LUID {:016X}, vendor 0x{:04X}); o H.264 \"{}\" ativou nela",
            placa.descricao, placa.luid, placa.vendor_id, placa.encoder
        ));
        let parar = &comum.parar;
        let c = CapturaDeCamera::abrir(&placa.device, &placa.gerenciador.0, &fonte, teto, origem, Some(parar), None)?;
        Ok((placa, c))
    };
    let t0 = Instant::now();
    match abrir() {
        Ok((placa, captura)) => {
            let info = InfoDaCamera {
                largura: captura.width,
                altura: captura.height,
                formato: captura.formato,
                faixa_completa: captura.faixa_completa,
                matriz_709: captura.matriz_709,
                entrelacamento_no_anel: captura.entrelacamento_no_anel,
                modo: captura.modo,
                descricao: captura.descricao.clone(),
            };
            registro::linha(format!(
                "r5 dono: a câmera \"{nome}\" abriu em {} ms — {}x{} {:?} modo={:?} (teto da melhor imagem {} macroblocos a {} fps) | {}",
                t0.elapsed().as_millis(),
                info.largura,
                info.altura,
                info.formato,
                info.modo,
                teto.max_macroblocos,
                teto.fps,
                info.descricao
            ));
            let aviso = captura.frame_ready.clone();
            *comum.placa.lock().unwrap_or_else(|e| e.into_inner()) = Some(placa.clone());
            *comum.info.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
            *comum.captura.lock().unwrap_or_else(|e| e.into_inner()) = Some(CapturaEnviavel(captura));
            *comum.fase.lock().unwrap_or_else(|e| e.into_inner()) = FaseDoDono::Aberto;
            (comum.acordar)();
            bombear(&comum, &placa, aviso, origem, &nome, link.as_deref());
        }
        Err(motivo) => {
            let frase = regras::so_a_frase(&motivo).to_string();
            registro::linha(format!("r5 dono: !! a câmera não abriu em {} ms: {motivo}", t0.elapsed().as_millis()));
            *comum.fase.lock().unwrap_or_else(|e| e.into_inner()) =
                if comum.parar.load(Ordering::SeqCst) { FaseDoDono::Fechado } else { FaseDoDono::Falhou(frase) };
            (comum.acordar)();
        }
    }
    // O fim: as saídas saem antes da câmera (a prévia solta a swap chain nesta thread, que a criou).
    drop(comum.previa.lock().unwrap_or_else(|e| e.into_inner()).take());
    drop(comum.gravador.lock().unwrap_or_else(|e| e.into_inner()).take());
    *comum.rede.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if let Some(mut c) = comum.captura.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let t = Instant::now();
        c.0.stop();
        registro::linha(format!(
            "r5 dono: câmera fechada em {} ms (a soltura do leitor segue fora desta thread); entregues={} buraco_maior={} ms",
            t.elapsed().as_millis(),
            comum.entregues.load(Ordering::Relaxed),
            comum.buraco_maior_us.load(Ordering::Relaxed) / 1000
        ));
    }
    {
        let mut f = comum.fase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*f, FaseDoDono::Aberto | FaseDoDono::Abrindo) {
            *f = FaseDoDono::Fechado;
        }
    }
    (comum.acordar)();
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
}

/// **A placa do dono** (§2.4, item 2): a Intel primeiro, depois as outras, a NVIDIA por último
/// (`regras_r5::ordem_das_placas`); cada uma conferida ativando um H.264 **nela** (desligado em
/// seguida) antes de criar o dispositivo pelo LUID. A proteção multithread ligada: o dono, a prévia,
/// a rede e o gravador usam o contexto imediato de threads diferentes.
fn escolher_placa() -> Result<PlacaDoDono, String> {
    let placas = device::placas_de_hardware().map_err(|e| format!("as placas não foram enumeradas: {e}"))?; // i18n: fora (detalhe técnico, vai ao diário)
    let ordem = crate::regras_r5::ordem_das_placas(&placas.iter().map(|p| (p.luid, p.vendor_id)).collect::<Vec<_>>());
    let mut tentativas = Vec::new();
    for luid in ordem {
        let Some(p) = placas.iter().find(|p| p.luid == luid) else { continue };
        let enc = match encoder::ativar_h264_so_na_placa(luid) {
            Ok(e) => e,
            Err(e) => {
                tentativas.push(format!("{} (LUID {luid:016X}): o H.264 não ativou ({e})", p.descricao)); // i18n: fora (detalhe técnico, vai ao diário)
                continue;
            }
        };
        let nome_do_encoder = enc.friendly_name.clone();
        let _ = encoder::desligar(&enc);
        drop(enc);
        let a = match device::create_device_por_luid(luid) {
            Ok(a) => a,
            Err(e) => {
                tentativas.push(format!("{} (LUID {luid:016X}): o dispositivo não abriu ({e})", p.descricao)); // i18n: fora (detalhe técnico, vai ao diário)
                continue;
            }
        };
        match device::proteger_contexto(&a.context, true) {
            Ok(antes) => registro::linha(format!("r5 dono: proteção multithread do contexto ligada (antes={antes})")),
            Err(e) => registro::linha(format!("r5 dono: !! a proteção multithread não ligou ({e})")),
        }
        let gerenciador = match encoder::create_device_manager(&a.device) {
            Ok(g) => g,
            Err(e) => {
                tentativas.push(format!("{} (LUID {luid:016X}): o gerenciador DXGI não abriu ({e})", p.descricao)); // i18n: fora (detalhe técnico, vai ao diário)
                continue;
            }
        };
        if !tentativas.is_empty() {
            registro::linha(format!("r5 dono: placas recusadas antes: {}", tentativas.join("; ")));
        }
        return Ok(PlacaDoDono {
            device: a.device,
            context: a.context,
            gerenciador: Arc::new(Enviavel(gerenciador)),
            luid,
            vendor_id: a.vendor_id,
            descricao: a.description,
            encoder: nome_do_encoder,
        });
    }
    Err(crate::idioma::tf("nenhuma placa ativou um H.264 para a câmera ({})", &[&tentativas.join("; ")]))
}

/// O bombeio: um quadro do leitor → o anel → as saídas.
fn bombear(comum: &Comum, placa: &PlacaDoDono, aviso: Receiver<()>, origem: Instant, nome: &str, link: Option<&str>) {
    let mut janela = Janela::default();
    let mut testemunha = regras::TestemunhaDaInterface::default();
    let mut ultima_interface = Instant::now();
    let mut anterior: Option<Instant> = None;
    let mut fim_dito = false;
    let mut parada_dita = false;
    let mut ultimo_relato = Instant::now();
    // **A luma de bancada** (R9 §5, `--luma-media`): um quadro a cada 30, nesta thread.
    let mut luma = if crate::luma_de_bancada::LIGADA.load(Ordering::SeqCst) {
        comum.info.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|i| crate::luma_de_bancada::MedidorDeLuma::novo(i.faixa_completa, i.matriz_709))
    } else {
        None
    };
    loop {
        if comum.parar.load(Ordering::SeqCst) {
            break;
        }
        let _ = aviso.recv_timeout(Duration::from_millis(50));
        let quadro = {
            let mut g = comum.captura.lock().unwrap_or_else(|e| e.into_inner());
            g.as_mut().and_then(|c| c.0.take_frame())
        };
        let agora = Instant::now();
        if let Some(q) = quadro {
            comum.entregues.fetch_add(1, Ordering::Relaxed);
            comum.ultimo_us.store(q.captured_at.saturating_duration_since(origem).as_micros().max(1) as u64, Ordering::Relaxed);
            if let Some(a) = anterior {
                let d = q.captured_at.saturating_duration_since(a);
                janela.buraco_maior = janela.buraco_maior.max(d);
                comum.buraco_maior_us.fetch_max(d.as_micros() as u64, Ordering::Relaxed);
            }
            anterior = Some(q.captured_at);
            janela.comeco.get_or_insert(agora);
            janela.quadros += 1;
            janela.ultimo = Some(agora);
            // (a) a rede: uma cópia no anel dela, numa posição que ninguém segura (B1).
            if let Some(r) = comum.rede.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                let t = Instant::now();
                r.entregar(&q, placa);
                let c = &mut janela.custo_us[0];
                *c = (c.0 + 1, c.1 + t.elapsed().as_micros() as u64);
            }
            // (b) a prévia, na mesma thread e no mesmo contexto da cópia.
            {
                let t = Instant::now();
                let mut g = comum.previa.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = g.as_mut() {
                    p.quadro(&q, placa);
                    let c = &mut janela.custo_us[1];
                    *c = (c.0 + 1, c.1 + t.elapsed().as_micros() as u64);
                }
            }
            // (c) o gravador: a cópia na reserva dele.
            {
                let t = Instant::now();
                let mut g = comum.gravador.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(s) = g.as_mut() {
                    s.quadro(&q, placa);
                    let c = &mut janela.custo_us[2];
                    *c = (c.0 + 1, c.1 + t.elapsed().as_micros() as u64);
                }
            }
            // (d) a luma de bancada, só com a bandeira.
            if let Some(m) = luma.as_mut() {
                m.quadro(&q, placa);
            }
        }
        // A interface (M9): a cada segundo, só da câmera pelo link.
        if let Some(l) = link {
            if ultima_interface.elapsed() >= Duration::from_secs(1) {
                ultima_interface = Instant::now();
                let lida = crate::cameras::interface_habilitada(l);
                if !testemunha.presente(lida) {
                    let mut g = comum.fim_da_interface.lock().unwrap_or_else(|e| e.into_inner());
                    if g.is_none() {
                        *g = Some(format!("a interface da câmera sumiu ({lida:?})")); // i18n: fora (detalhe técnico, vai ao diário)
                    }
                }
            }
        }
        // A câmera parada, e o fim: uma linha por transição, e a tela acordada.
        let (parada, fim) = {
            let g = comum.captura.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_ref() {
                Some(c) => (c.0.parada_ha(agora), c.0.fim()),
                None => (None, None),
            }
        };
        let fim = fim.or_else(|| {
            comum.fim_da_interface.lock().unwrap_or_else(|e| e.into_inner()).clone().map(|m| FimDaCamera::pelo_codigo(m, true, Some(regras::HR_REMOVIDA)))
        });
        if parada.is_some() != parada_dita {
            parada_dita = parada.is_some();
            registro::linha(if parada_dita {
                format!("r5 dono: câmera parada (nenhum quadro há {} ms); as saídas esperam", parada.unwrap_or_default().as_millis())
            } else {
                "r5 dono: a câmera voltou".to_string()
            });
            (comum.acordar)();
        }
        if let (Some(f), false) = (&fim, fim_dito) {
            fim_dito = true;
            let frase = regras::so_a_frase(&regras::texto_do_fim_da_camera(nome, Some(f), false)).to_string();
            registro::linha(format!("r5 dono: a câmera acabou: {}{}", f.motivo, if f.desconectada { " (desconectada)" } else { "" }));
            *comum.fase.lock().unwrap_or_else(|e| e.into_inner()) = FaseDoDono::Acabou(frase);
            (comum.acordar)();
        }
        if ultimo_relato.elapsed() >= Duration::from_secs(10) {
            ultimo_relato = Instant::now();
            let fps = match (janela.comeco, janela.ultimo) {
                (Some(a), Some(b)) if b > a && janela.quadros > 1 => (janela.quadros - 1) as f64 / (b - a).as_secs_f64(),
                _ => 0.0,
            };
            if fps > 0.5 {
                comum.fps_medido_milesimos.store((fps * 1000.0) as u64, Ordering::Relaxed);
            }
            let custo = |i: usize| {
                let (n, s) = janela.custo_us[i];
                if n == 0 { "-".to_string() } else { format!("{:.0}us", s as f64 / n as f64) }
            };
            let mut extras = Vec::new();
            for g in [&comum.previa, &comum.gravador] {
                if let Some(s) = g.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    if let Some(l) = s.relato() {
                        extras.push(format!("{}: {l}", s.nome()));
                    }
                }
            }
            registro::linha(format!(
                "r5 dono: camera={fps:.1} fps quadros={} buraco_maior={} ms entregues_total={} rede={} previa={} gravador={} | custo na thread do dono: rede={} previa={} gravador={}{}",
                janela.quadros,
                janela.buraco_maior.as_millis(),
                comum.entregues.load(Ordering::Relaxed),
                if comum.rede.lock().unwrap_or_else(|e| e.into_inner()).is_some() { "pendurada" } else { "solta" },
                if comum.previa.lock().unwrap_or_else(|e| e.into_inner()).is_some() { "pendurada" } else { "solta" },
                if comum.gravador.lock().unwrap_or_else(|e| e.into_inner()).is_some() { "pendurado" } else { "solto" },
                custo(0),
                custo(1),
                custo(2),
                if extras.is_empty() { String::new() } else { format!(" | {}", extras.join(" | ")) }
            ));
            janela = Janela::default();
        }
    }
}

// =============================================================================================
// O leitor da rede
// =============================================================================================

/// **A rede pendurada no dono**: o que a `Cadeia` lê (`Captura::DoDono`), com os mesmos métodos que
/// ela lia da `CapturaDeCamera`. Largar o leitor (ou `stop`) **solta** a rede; a câmera continua.
pub struct LeitorDoDono {
    pub frame_ready: Receiver<()>,
    caixa: Arc<CaixaDaRede>,
    dono: Arc<DonoDaCaptura>,
    pub info: InfoDaCamera,
    pub placa: PlacaDoDono,
    /// Um fim que é só desta rede (o encoder que segurou os quadros): a sessão acaba, a câmera não.
    fim_local: Mutex<Option<String>>,
    solto: bool,
}

impl LeitorDoDono {
    pub fn take_frame(&mut self) -> Option<CapturedFrame> {
        self.caixa.caixa.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn espiar_instante(&self) -> Option<Instant> {
        self.caixa.caixa.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|q| q.captured_at)
    }

    pub fn fim(&self) -> Option<FimDaCamera> {
        if let Some(m) = self.fim_local.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Some(FimDaCamera::pelo_codigo(m, false, None));
        }
        self.dono.fim()
    }

    /// O nome da câmera do dono (para a frase do fim).
    pub fn nome(&self) -> &str {
        &self.dono.nome
    }

    pub fn item_fechado(&self) -> bool {
        self.fim().is_some()
    }

    pub fn parada_ha(&self, agora: Instant) -> Option<Duration> {
        self.dono.com_captura(|c| c.parada_ha(agora)).flatten()
    }

    /// A última posição do anel da rede, **com a posse** (o dono não a reescreve enquanto a repetição
    /// viver). Só é pedida com a câmera parada (`regras_da_camera::repetir_a_camera`).
    pub fn quadro_para_repetir(&self) -> Option<(ID3D11Texture2D, Posse)> {
        self.caixa.ultima()
    }

    pub fn chegados(&self) -> u64 {
        self.dono.com_captura(|c| c.chegados()).unwrap_or(0)
    }

    pub fn ultimo_quadro(&self) -> Option<Instant> {
        self.dono.com_captura(|c| c.ultimo_quadro()).flatten()
    }

    pub fn aspecto(&self) -> regras::Aspecto {
        self.dono.com_captura(|c| c.aspecto()).unwrap_or(regras::Aspecto { par: regras::Par::QUADRADA, origem: regras::OrigemDaPar::Quadrada })
    }

    pub fn tamanho_exibido(&self) -> (u32, u32) {
        self.dono.com_captura(|c| c.tamanho_exibido()).unwrap_or((self.info.largura, self.info.altura))
    }

    pub fn trocas_de_aspecto(&self) -> u64 {
        self.dono.com_captura(|c| c.trocas_de_aspecto()).unwrap_or(0)
    }

    /// A cadeia acaba **esta rede** com um motivo (o encoder que segura os quadros): a sessão cai, a
    /// câmera fica com o dono.
    pub fn encerrar(&self, motivo: String) {
        registro::linha(format!("r5 dono: a rede acabou por conta própria ({motivo}); a câmera continua"));
        *self.fim_local.lock().unwrap_or_else(|e| e.into_inner()) = Some(motivo);
    }

    /// Solta a rede do dono. A câmera continua.
    pub fn stop(&mut self) {
        if !self.solto {
            self.solto = true;
            self.dono.soltar_rede(&self.caixa);
        }
    }
}

impl Drop for LeitorDoDono {
    fn drop(&mut self) {
        self.stop();
    }
}
