//! **Os controles de câmera do R9 no Windows: a thread que fala com o driver**
//! (`docs/controles-de-camera.md` §2.2 e §6).
//!
//! As regras moram em `regras_dos_controles.rs`; aqui só se executa:
//!
//! - **`IAMCameraControl`, `IAMVideoProcAmp` e `IKsControl` por `cast` na `IMFMediaSource`**, numa
//!   thread MTA de trabalho (`quall.camera.ajustes`), **nunca** na thread da janela nem na
//!   `quall.r5.dono` (§6): um `Set` num driver UVC vai ao firmware pela USB, e a janela e o bombeio
//!   não esperam isso. As interfaces são criadas, usadas e soltas nesta thread.
//! - **A abertura não espera** (§2.2): a captura já tem o primeiro quadro quando a thread nasce
//!   (`captura_de_camera.rs`, depois de `aberta`), e o registro guardado é reaplicado daqui, em
//!   segundo plano.
//! - **A anti-cintilação** vai primeiro pelo `IKsControl::KsProperty` (conjunto
//!   `PROPSETID_VIDCAP_VIDEOPROCAMP`, id 13) e, se ele recusar, pelo `IAMVideoProcAmp::Set(13)`; o
//!   diário diz qual funcionou (§6).
//! - **Na soltura** (`encerrar`), devolve `Flags_Auto` (ou o valor de antes, para o que não tem Auto)
//!   a tudo o que o Quall mudou, solta as interfaces e só então a captura solta o leitor e a fonte
//!   (a `Soltura` de `captura_de_camera.rs` espera esta thread antes).
//! - **Em `Modo::Compartilhada`** nada é escrito: os controles ficam apagados com o texto do §3.5.
//!   A bancada (`--ajustes-medir-compartilhada`) tenta um `Set` assim mesmo, para medir.
//! - **O registro** é lido e gravado em `%APPDATA%\Quall\camera-ajustes.json` pela troca de arquivo
//!   de `identidade.rs` (só com `net`; a sonda sem rede fica com o registro na memória). A bancada
//!   (`--ajustes-camera`) parte do padrão e **não grava**.
//!
//! **Nenhum quadro é lido aqui.** A luma de bancada é de `luma_de_bancada.rs`, na thread do dono.
//! Só o **contador** de quadros que chegaram (`chegados`, da captura) é lido, na leitura de 4 vezes
//! por segundo, para o vigia da pouca luz (§3.1, `regras::VigiaDaPoucaLuz`).

#![cfg(windows)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Media::DirectShow::{IAMCameraControl, IAMVideoProcAmp};
use windows::Win32::Media::KernelStreaming::{
    IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0, KSPROPERTY_TYPE_GET, KSPROPERTY_TYPE_SET, KSPROPERTY_VIDEOPROCAMP_S,
    PROPSETID_VIDCAP_VIDEOPROCAMP,
};
use windows::Win32::Media::MediaFoundation::IMFMediaSource;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::captura_de_camera::Modo;
use crate::registro;
use crate::regras_dos_controles::{
    self as regras, Acao, Capacidades, Divergencia, Envio, Faixa, FaseDosAjustes, Interface as Onde, Lido, Lidos,
    MedidorDeFps, PainelDosAjustes, PassoDeBancada, PoucaLuz, Propriedade, Registro, VigiaDaPoucaLuz,
};

// =============================================================================================
// A bancada
// =============================================================================================

/// O que a bancada pede aos ajustes (`--ajustes-camera`, `--ajustes-medir-compartilhada`,
/// `--ajustes-leitura`). O produto deixa tudo vazio.
#[derive(Clone, Debug, Default)]
pub struct ConfigDeBancada {
    /// O roteiro: os passos, contados da câmera pronta para ajustes. Com roteiro, o registro parte
    /// do padrão e nada é gravado no disco.
    pub roteiro: Option<Vec<PassoDeBancada>>,
    /// Em `Modo::Compartilhada`, tenta um `Set` assim mesmo e diz o que voltou (§6, a primeira
    /// medida da frente).
    pub medir_compartilhada: bool,
    /// Uma linha por segundo com o `Get` de tudo (o controlador do teste do compartilhado).
    pub leitura: bool,
}

static BANCADA: Mutex<Option<ConfigDeBancada>> = Mutex::new(None);

/// Liga a bancada dos ajustes (o app chama na partida, com os argumentos).
pub fn configurar_bancada(c: ConfigDeBancada) {
    *BANCADA.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
}

fn bancada() -> ConfigDeBancada {
    BANCADA.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

// =============================================================================================
// A ponta da tela, e o dono da thread
// =============================================================================================

struct Comum {
    painel: Mutex<PainelDosAjustes>,
    versao: AtomicU64,
    pedidos: Mutex<Vec<Acao>>,
    aviso: Condvar,
    parar: AtomicBool,
    /// **R9b**: o filmador desta captura (`camera_remota.rs`), que as sessões de vídeo bombeiam e
    /// esta thread alimenta. Nasce e morre com a captura.
    #[cfg(feature = "net")]
    filmador: Arc<quall_core::camera_remota::Filmador>,
    /// Há pedido remoto na fila do filmador: a espera da thread acorda por ele também (um
    /// `notify_all` sem gesto local seria engolido pelo predicado, a revisão do plano, achado 3).
    remoto: AtomicBool,
}

impl Comum {
    fn publicar(&self, f: impl FnOnce(&mut PainelDosAjustes)) {
        f(&mut self.painel.lock().unwrap_or_else(|e| e.into_inner()));
        self.versao.fetch_add(1, Ordering::SeqCst);
    }
}

/// **O que a tela segura**: lê o painel e pede gestos. Clonável e de qualquer thread; nenhuma
/// interface COM passa por aqui.
#[derive(Clone)]
pub struct PontaDosAjustes(Arc<Comum>);

impl PontaDosAjustes {
    pub fn painel(&self) -> PainelDosAjustes {
        self.0.painel.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Só a fase (barato: sem copiar as faixas).
    pub fn fase(&self) -> FaseDosAjustes {
        self.0.painel.lock().unwrap_or_else(|e| e.into_inner()).fase.clone()
    }

    /// **R9b**: quem mexeu nesta câmera de longe, nos 4 s depois (barato: sem copiar as faixas).
    pub fn painel_controlado_por(&self) -> Option<String> {
        self.0.painel.lock().unwrap_or_else(|e| e.into_inner()).controlado_por.clone()
    }

    /// **Pouca luz** (§3.1): o automático baixou o fps para clarear (barato: sem copiar as faixas).
    pub fn painel_pouca_luz(&self) -> Option<PoucaLuz> {
        self.0.painel.lock().unwrap_or_else(|e| e.into_inner()).pouca_luz
    }

    /// Muda a cada publicação: a tela redesenha quando ela mudou.
    pub fn versao(&self) -> u64 {
        self.0.versao.load(Ordering::SeqCst)
    }

    /// Um gesto. Os gestos se juntam e a thread os aplica em ordem; os envios ao driver são
    /// agrupados a no máximo 15 por segundo, e o último valor vence (§2.2).
    pub fn pedir(&self, a: Acao) {
        self.0.pedidos.lock().unwrap_or_else(|e| e.into_inner()).push(a);
        self.0.aviso.notify_all();
    }

    /// **R9b**: o filmador desta captura, para as sessões de vídeo bombearem.
    #[cfg(feature = "net")]
    pub fn filmador(&self) -> Option<Arc<quall_core::camera_remota::Filmador>> {
        Some(Arc::clone(&self.0.filmador))
    }

    /// **R9b**: chegou pedido remoto (a bombeada viu o bit): a thread o tira da fila na hora.
    pub fn acordar(&self) {
        // O cadeado dos pedidos em volta: sem ele, o aviso poderia cair entre o predicado e a espera.
        let _g = self.0.pedidos.lock().unwrap_or_else(|e| e.into_inner());
        self.0.remoto.store(true, Ordering::SeqCst);
        self.0.aviso.notify_all();
    }
}

/// **A thread dos ajustes de uma captura**, dona das interfaces. Quem a cria é a captura
/// (`captura_de_camera.rs`), e quem a encerra é a soltura dela.
pub struct AjustesDaCamera {
    ponta: PontaDosAjustes,
    thread: Option<JoinHandle<()>>,
}

/// A fonte, entre threads: o Media Foundation é livre de apartamento, e o processo é MTA (o mesmo
/// argumento de `captura_de_camera.rs`).
struct FonteEnviavel(IMFMediaSource);
// SAFETY: ver o comentário do tipo; a thread dos ajustes só faz `cast` e solta.
unsafe impl Send for FonteEnviavel {}

impl AjustesDaCamera {
    /// Sobe a thread dos ajustes da câmera `link`, aberta no `modo`, com o fps do tipo nativo e o
    /// contador de quadros que chegam da captura (o vigia da pouca luz). Volta na hora: a leitura
    /// das faixas e a reaplicação correm na thread.
    pub fn iniciar(fonte: &IMFMediaSource, link: &str, modo: Modo, fps: f64, chegados: Arc<AtomicU64>) -> Option<AjustesDaCamera> {
        let comum = Arc::new(Comum {
            painel: Mutex::new(PainelDosAjustes { fps, ..Default::default() }),
            versao: AtomicU64::new(0),
            pedidos: Mutex::new(Vec::new()),
            aviso: Condvar::new(),
            parar: AtomicBool::new(false),
            #[cfg(feature = "net")]
            filmador: crate::camera_remota::novo_filmador(),
            remoto: AtomicBool::new(false),
        });
        let c = Arc::clone(&comum);
        let f = FonteEnviavel(fonte.clone());
        let link = link.to_string();
        let prefixo = registro::prefixo_desta_thread();
        let h = std::thread::Builder::new().name("quall.camera.ajustes".into()).spawn(move || {
            registro::prefixar_esta_thread(&prefixo);
            correr(c, f, link, modo, fps, chegados);
        });
        match h {
            Ok(h) => Some(AjustesDaCamera { ponta: PontaDosAjustes(comum), thread: Some(h) }),
            Err(e) => {
                registro::linha(format!("ajustes: !! a thread não subiu ({e}); a câmera fica sem ajustes"));
                None
            }
        }
    }

    pub fn ponta(&self) -> PontaDosAjustes {
        self.ponta.clone()
    }

    /// **Pede o fim** (a devolução e a soltura das interfaces) e devolve a thread, para a soltura
    /// da captura esperá-la antes de soltar o leitor e a fonte.
    pub fn encerrar(mut self) -> Option<JoinHandle<()>> {
        self.ponta.0.parar.store(true, Ordering::SeqCst);
        self.ponta.0.aviso.notify_all();
        self.thread.take()
    }
}

impl Drop for AjustesDaCamera {
    fn drop(&mut self) {
        self.ponta.0.parar.store(true, Ordering::SeqCst);
        self.ponta.0.aviso.notify_all();
    }
}

// =============================================================================================
// As interfaces
// =============================================================================================

fn hr(e: &windows::core::Error) -> String {
    format!("0x{:08X}", e.code().0 as u32)
}

/// O caminho que a anti-cintilação tomou, para o diário dizer uma vez (§6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaminhoDaCintilacao {
    KsControl,
    VideoProcAmp,
}

struct Interfaces {
    camera: Option<IAMCameraControl>,
    amp: Option<IAMVideoProcAmp>,
    ks: Option<IKsControl>,
    caminho_dito: Option<CaminhoDaCintilacao>,
}

impl Interfaces {
    fn de(fonte: &IMFMediaSource) -> Interfaces {
        let camera = fonte.cast::<IAMCameraControl>();
        let amp = fonte.cast::<IAMVideoProcAmp>();
        let ks = fonte.cast::<IKsControl>();
        registro::linha(format!(
            "ajustes: interfaces na fonte: IAMCameraControl {}, IAMVideoProcAmp {}, IKsControl {}",
            camera.as_ref().map(|_| "sim".to_string()).unwrap_or_else(|e| format!("não ({})", hr(e))),
            amp.as_ref().map(|_| "sim".to_string()).unwrap_or_else(|e| format!("não ({})", hr(e))),
            ks.as_ref().map(|_| "sim".to_string()).unwrap_or_else(|e| format!("não ({})", hr(e))),
        ));
        Interfaces { camera: camera.ok(), amp: amp.ok(), ks: ks.ok(), caminho_dito: None }
    }

    fn get_range(&self, p: Propriedade) -> Result<Faixa, String> {
        let (onde, id) = p.onde();
        let (mut min, mut max, mut passo, mut padrao, mut bandeiras) = (0i32, 0i32, 0i32, 0i32, 0i32);
        let r = unsafe {
            match onde {
                Onde::CameraControl => self.camera.as_ref().ok_or("sem IAMCameraControl")?.GetRange(id, &mut min, &mut max, &mut passo, &mut padrao, &mut bandeiras), // i18n: fora (diário)
                Onde::VideoProcAmp => self.amp.as_ref().ok_or("sem IAMVideoProcAmp")?.GetRange(id, &mut min, &mut max, &mut passo, &mut padrao, &mut bandeiras), // i18n: fora (diário)
            }
        };
        r.map_err(|e| hr(&e))?;
        Faixa::do_get_range(min, max, passo, padrao, bandeiras).ok_or_else(|| format!("faixa sem sentido [{min}, {max}]")) // i18n: fora (diário)
    }

    /// A faixa de cada propriedade, com o motivo das que faltam (para o diário).
    fn capacidades(&self) -> (Capacidades, Vec<String>) {
        let mut caps = Capacidades::new();
        let mut linhas = Vec::new();
        for p in Propriedade::TODAS {
            match self.get_range(p) {
                Ok(f) => {
                    linhas.push(format!("{}=[{}..{} passo {} padrão {} bandeiras {}]", p.chave(), f.min, f.max, f.passo, f.padrao, f.bandeiras)); // i18n: fora (diário)
                    caps.insert(p, f);
                }
                Err(e) if p == Propriedade::AntiCintilacao => {
                    // Sem o `GetRange(13)` no proxy, o `IKsControl` ainda pode ler: 0 a 2 (UVC 1.1).
                    match self.ks_get() {
                        Ok(v) => {
                            linhas.push(format!("{}=[0..2, pelo IKsControl; o GetRange(13) deu {e}; valor {v}]", p.chave())); // i18n: fora (diário)
                            caps.insert(p, Faixa { min: 0, max: 2, passo: 1, padrao: v, bandeiras: regras::FLAGS_MANUAL });
                        }
                        Err(e2) => linhas.push(format!("{}=não ({e}; IKsControl {e2})", p.chave())), // i18n: fora (diário)
                    }
                }
                Err(e) => linhas.push(format!("{}=não ({e})", p.chave())), // i18n: fora (diário)
            }
        }
        (caps, linhas)
    }

    fn propriedade_ks(tipo: u32) -> KSPROPERTY_VIDEOPROCAMP_S {
        KSPROPERTY_VIDEOPROCAMP_S {
            Property: KSIDENTIFIER {
                Anonymous: KSIDENTIFIER_0 {
                    Anonymous: KSIDENTIFIER_0_0 { Set: PROPSETID_VIDCAP_VIDEOPROCAMP, Id: regras::ID_DA_ANTI_CINTILACAO as u32, Flags: tipo },
                },
            },
            ..Default::default()
        }
    }

    fn ks_get(&self) -> Result<i32, String> {
        let ks = self.ks.as_ref().ok_or("sem IKsControl")?; // i18n: fora (diário)
        let mut s = Self::propriedade_ks(KSPROPERTY_TYPE_GET);
        let tam = std::mem::size_of::<KSPROPERTY_VIDEOPROCAMP_S>() as u32;
        let mut voltou = 0u32;
        let p = &s.Property as *const KSIDENTIFIER;
        unsafe { ks.KsProperty(p, tam, &mut s as *mut _ as *mut core::ffi::c_void, tam, &mut voltou) }.map_err(|e| hr(&e))?;
        Ok(s.Value)
    }

    fn ks_set(&self, v: i32) -> Result<(), String> {
        let ks = self.ks.as_ref().ok_or("sem IKsControl")?; // i18n: fora (diário)
        let mut s = Self::propriedade_ks(KSPROPERTY_TYPE_SET);
        s.Value = v;
        s.Flags = regras::FLAGS_MANUAL as u32;
        let tam = std::mem::size_of::<KSPROPERTY_VIDEOPROCAMP_S>() as u32;
        let mut voltou = 0u32;
        let p = &s.Property as *const KSIDENTIFIER;
        unsafe { ks.KsProperty(p, tam, &mut s as *mut _ as *mut core::ffi::c_void, tam, &mut voltou) }.map_err(|e| hr(&e))
    }

    fn ler(&self, p: Propriedade) -> Result<Lido, String> {
        let (onde, id) = p.onde();
        if p == Propriedade::AntiCintilacao {
            if let Ok(v) = self.ks_get() {
                return Ok(Lido { valor: v, bandeiras: regras::FLAGS_MANUAL });
            }
        }
        let (mut v, mut b) = (0i32, 0i32);
        let r = unsafe {
            match onde {
                Onde::CameraControl => self.camera.as_ref().ok_or("sem IAMCameraControl")?.Get(id, &mut v, &mut b), // i18n: fora (diário)
                Onde::VideoProcAmp => self.amp.as_ref().ok_or("sem IAMVideoProcAmp")?.Get(id, &mut v, &mut b), // i18n: fora (diário)
            }
        };
        r.map_err(|e| hr(&e))?;
        Ok(Lido { valor: v, bandeiras: b })
    }

    fn ler_todos(&self, caps: &Capacidades) -> Lidos {
        caps.keys().filter_map(|p| self.ler(*p).ok().map(|l| (*p, l))).collect()
    }

    fn escrever(&mut self, e: &Envio) -> Result<(), String> {
        let (onde, id) = e.prop.onde();
        if e.prop == Propriedade::AntiCintilacao {
            // §6: o `IKsControl` primeiro, o `IAMVideoProcAmp::Set(13)` de alternativa.
            let pelo_ks = self.ks_set(e.valor);
            let r = match &pelo_ks {
                Ok(()) => Ok(CaminhoDaCintilacao::KsControl),
                Err(e1) => match self.amp.as_ref().map(|a| unsafe { a.Set(id, e.valor, e.bandeiras) }) {
                    Some(Ok(())) => Ok(CaminhoDaCintilacao::VideoProcAmp),
                    Some(Err(e2)) => Err(format!("IKsControl {e1}; IAMVideoProcAmp::Set(13) {}", hr(&e2))),
                    None => Err(format!("IKsControl {e1}; sem IAMVideoProcAmp")), // i18n: fora (diário)
                },
            };
            if let Ok(c) = r {
                if self.caminho_dito != Some(c) {
                    self.caminho_dito = Some(c);
                    registro::linha(match c {
                        CaminhoDaCintilacao::KsControl => {
                            "ajustes: a anti-cintilação foi pelo IKsControl (PROPSETID_VIDCAP_VIDEOPROCAMP, id 13)".to_string()
                        }
                        CaminhoDaCintilacao::VideoProcAmp => format!(
                            "ajustes: a anti-cintilação foi pelo IAMVideoProcAmp::Set(13) (o IKsControl recusou: {})", // i18n: fora (diário)
                            pelo_ks.err().unwrap_or_default()
                        ),
                    });
                }
            }
            return r.map(|_| ());
        }
        let r = unsafe {
            match onde {
                Onde::CameraControl => self.camera.as_ref().ok_or("sem IAMCameraControl")?.Set(id, e.valor, e.bandeiras), // i18n: fora (diário)
                Onde::VideoProcAmp => self.amp.as_ref().ok_or("sem IAMVideoProcAmp")?.Set(id, e.valor, e.bandeiras), // i18n: fora (diário)
            }
        };
        r.map_err(|e| hr(&e))
    }
}

// =============================================================================================
// O registro no disco
// =============================================================================================

#[cfg(feature = "net")]
fn carregar(link: &str) -> Registro {
    crate::identidade::ajustes_da_camera(link)
}

#[cfg(not(feature = "net"))]
fn carregar(_link: &str) -> Registro {
    Registro::default()
}

#[cfg(feature = "net")]
fn guardar(link: &str, r: &Registro) {
    crate::identidade::guardar_ajustes_da_camera(link, r);
}

#[cfg(not(feature = "net"))]
fn guardar(_link: &str, _r: &Registro) {}

// =============================================================================================
// A thread
// =============================================================================================

/// O texto curto de um envio, para o diário.
fn envio_em_texto(e: &Envio) -> String {
    format!(
        "{}={} ({})",
        e.prop.chave(),
        regras::texto_do_valor(e.prop, e.valor),
        if e.bandeiras == regras::FLAGS_AUTO { "Auto" } else { "Manual" } // i18n: fora (diário)
    )
}

// =============================================================================================
// O lado remoto (R9b): esta thread é a fila serial do dono da câmera
// =============================================================================================

/// **O filmador desta captura, visto da fila serial** (`docs/controle-remoto-da-camera.md` §6 e
/// §12): a câmera publicada, as mudanças locais (`n = 0`), os pedidos remotos tirados da fila e
/// aplicados aqui, o lido e o "Controlado por". Sem `net` (a sonda de bancada) tudo é nada.
#[cfg(feature = "net")]
struct LadoRemoto {
    f: Arc<quall_core::camera_remota::Filmador>,
}

#[cfg(feature = "net")]
impl LadoRemoto {
    fn de(c: &Comum) -> LadoRemoto {
        LadoRemoto { f: Arc::clone(&c.filmador) }
    }

    /// A câmera aberta, com as capacidades (do mesmo mapa que monta o painel) e o registro.
    fn camera(&self, caps: &Capacidades, fps: f64, compartilhada: bool, reg: &Registro) {
        let capacidades = crate::regras_da_camera_remota::capacidades(caps, fps, compartilhada).to_string();
        let ajuste = crate::regras_da_camera_remota::registro_em_json(reg);
        match self.f.definir_camera(Some((&capacidades, &ajuste))) {
            Ok(()) => registro::linha(format!("ajustes: câmera remota: capacidades publicadas ({} bytes): {capacidades}", capacidades.len())),
            Err(e) => registro::linha(format!("ajustes: câmera remota: !! as capacidades não entraram no núcleo: {e}")),
        }
    }

    /// Uma mudança feita aqui (o painel, a bancada): vale sempre e na hora (§4, regra 1).
    fn local(&self, reg: &Registro) {
        if let Err(e) = self.f.definir_ajuste(&crate::regras_da_camera_remota::registro_em_json(reg), 0) {
            registro::linha(format!("ajustes: câmera remota: !! o registro local não entrou no núcleo: {e}"));
        }
    }

    /// **Os pedidos remotos**, um consumidor só (§6, achado I5): tira da fila até vazia, aplica na
    /// ordem do contrato sobre o lido de agora e devolve o registro que ficou valendo (ou recusa).
    /// Devolve se o registro mudou.
    fn drenar(&self, reg: &mut Registro, caps: &Capacidades, lidos: &Lidos, fps: f64, compartilhada: bool) -> bool {
        use crate::regras_da_camera_remota as rr;
        let mut mudou = false;
        loop {
            let json = match self.f.proximo_pedido() {
                Ok(Some(j)) => j,
                Ok(None) => break,
                Err(e) => {
                    registro::linha(format!("ajustes: câmera remota: !! a fila não se leu: {e}"));
                    break;
                }
            };
            let Some(p) = rr::ler_pedido(&json) else {
                registro::linha(format!("ajustes: câmera remota: !! pedido ilegível: {json}"));
                continue;
            };
            let autor = p.autor.clone().unwrap_or_default();
            if compartilhada {
                // Os controles locais estão apagados neste modo (decisão do Bruno), e o remoto também.
                let _ = self.f.recusar(p.n, rr::OUTRO_APP);
                registro::linha(format!("ajustes: câmera remota: pedido {} recusado: modo compartilhado", p.n));
                continue;
            }
            match rr::aplicar_pedido(reg, &p, caps, lidos, fps) {
                Ok(novo) => {
                    mudou |= novo != *reg;
                    *reg = novo;
                    let j = rr::registro_em_json(reg);
                    // O pedido venceu (5 s) ou a câmera trocou no meio: o registro já mudou aqui, e
                    // o núcleo fica com ele como mudança local (a revisão do plano, achado 9).
                    if let Err(e) = self.f.definir_ajuste(&j, p.n) {
                        registro::linha(format!("ajustes: câmera remota: o recibo do pedido {} não entrou ({e}); vai como mudança local", p.n));
                        let _ = self.f.definir_ajuste(&j, 0);
                    }
                    registro::linha(format!("ajustes: câmera remota: pedido {} de \"{autor}\" aplicado: {}", p.n, serde_json::Value::Object(p.ajuste.clone())));
                }
                Err(m) => {
                    let _ = self.f.recusar(p.n, m);
                    registro::linha(format!("ajustes: câmera remota: pedido {} de \"{autor}\" recusado ({m}): {json}", p.n));
                }
            }
        }
        mudou
    }

    fn lido(&self, lidos: &Lidos, caps: &Capacidades, divergentes: &[Propriedade]) {
        let _ = self.f.definir_lido(&crate::regras_da_camera_remota::lido(lidos, caps, divergentes).to_string());
    }

    fn controlado_por(&self) -> Option<String> {
        self.f.estado_json().ok().and_then(|j| crate::regras_da_camera_remota::controlado_por(&j))
    }

    /// A câmera fecha: os receptores escondem o painel, e o que estava na fila é recusado.
    fn fechar(&self) {
        let _ = self.f.definir_camera(None);
    }
}

#[cfg(not(feature = "net"))]
struct LadoRemoto;

#[cfg(not(feature = "net"))]
impl LadoRemoto {
    fn de(_c: &Comum) -> LadoRemoto {
        LadoRemoto
    }
    fn camera(&self, _caps: &Capacidades, _fps: f64, _compartilhada: bool, _reg: &Registro) {}
    fn local(&self, _reg: &Registro) {}
    fn drenar(&self, _reg: &mut Registro, _caps: &Capacidades, _lidos: &Lidos, _fps: f64, _compartilhada: bool) -> bool {
        false
    }
    fn lido(&self, _lidos: &Lidos, _caps: &Capacidades, _divergentes: &[Propriedade]) {}
    fn controlado_por(&self) -> Option<String> {
        None
    }
    fn fechar(&self) {}
}

/// A gravação no disco espera isto depois da última mudança: um deslizante a 15 por segundo, local
/// ou remoto, não vira 15 trocas de arquivo por segundo (§6, achado I4).
const ADIAR_A_GRAVACAO: Duration = Duration::from_millis(500);

/// O estado de envio da thread: o que o driver recebeu, o que o Quall mudou, o último erro.
struct Envios {
    tocados: BTreeSet<Propriedade>,
    ultimo: BTreeMap<Propriedade, Envio>,
    hr: BTreeMap<Propriedade, String>,
}

impl Envios {
    /// Manda o que mudou desde o último envio. Devolve as linhas para o diário.
    fn executar(&mut self, i: &mut Interfaces, plano: &[Envio], originais: &Lidos) -> Vec<String> {
        let mut linhas = Vec::new();
        for e in plano {
            if self.ultimo.get(&e.prop) == Some(e) {
                continue;
            }
            let devolvida = e.bandeiras == regras::FLAGS_AUTO || originais.get(&e.prop).is_some_and(|o| o.valor == e.valor && o.bandeiras == e.bandeiras);
            match i.escrever(e) {
                Ok(()) => {
                    regras::marcar_tocado(&mut self.tocados, e, devolvida);
                    self.ultimo.insert(e.prop, *e);
                    self.hr.insert(e.prop, "ok".into());
                    linhas.push(envio_em_texto(e));
                }
                Err(x) => {
                    // O driver recusou: o Quall não sabe como a propriedade ficou; ela entra na
                    // devolução por garantia.
                    self.tocados.insert(e.prop);
                    self.hr.insert(e.prop, x.clone());
                    linhas.push(format!("{} recusado ({x})", envio_em_texto(e)));
                }
            }
        }
        linhas
    }
}

fn correr(c: Arc<Comum>, fonte: FonteEnviavel, link: String, modo: Modo, fps: f64, chegados: Arc<AtomicU64>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let t0 = Instant::now();
    // **Pelo tipo da fonte** (§2.2): a câmera virtual do próprio Quall não tem controles. O dono é
    // lido do registro do Windows (só leitura), e não do resultado do `cast`.
    if crate::cameras::e_camera_do_quall(&link) {
        registro::linha("ajustes: a câmera é do próprio Quall (pelo dono): sem controles");
        c.publicar(|p| p.fase = FaseDosAjustes::SemControles);
        drop(fonte);
        if com.is_ok() {
            unsafe { CoUninitialize() };
        }
        return;
    }
    let mut i = Interfaces::de(&fonte.0);
    // A fonte não é segurada além das interfaces: a soltura da captura é quem a desliga.
    drop(fonte);
    let (caps, linhas) = i.capacidades();
    let originais = i.ler_todos(&caps);
    let compartilhada = modo == Modo::Compartilhada;
    registro::linha(format!(
        "ajustes: {} em {} ms (modo {modo:?}, fps {fps:.2}, teto do obturador {}): {}",
        if caps.is_empty() { "a câmera não declara controle nenhum" } else { "faixas lidas" },
        t0.elapsed().as_millis(),
        regras::texto_do_obturador(regras::teto_do_obturador(fps)),
        linhas.join(" ")
    ));
    for p in Propriedade::TODAS {
        // O diário fica em português, qualquer que seja o idioma da tela.
        if let Some(f) = crate::idioma::com_idioma(crate::idioma::Idioma::Pt, || regras::frase_do_limite(p, &caps)) {
            registro::linha(format!("ajustes: nao se aplica prop={}: {f}", p.chave()));
        }
    }
    registro::linha(format!(
        "ajustes: como a câmera abriu: {}",
        originais.iter().map(|(p, l)| format!("{}={} (bandeiras {})", p.chave(), regras::texto_do_valor(*p, l.valor), l.bandeiras)).collect::<Vec<_>>().join(" ")
    ));
    let banc = bancada();
    // **Abre no automático** (07/10): o guardado não é reaplicado; vira "meus ajustes", oferecido no
    // painel. A bancada parte do padrão e não grava nada.
    let mut meus = if banc.roteiro.is_some() {
        registro::linha("ajustes: bancada (--ajustes-camera): o registro parte do padrão, e nada é gravado no disco");
        None
    } else {
        regras::meus_ajustes(carregar(&link))
    };
    let mut reg = Registro::default();
    registro::linha(format!("ajustes: abre no automático; meus ajustes {}", if meus.is_some() { "guardados" } else { "nenhum" }));
    c.publicar(|p| {
        p.fase = if compartilhada { FaseDosAjustes::Compartilhada } else { FaseDosAjustes::Pronto };
        p.caps = caps.clone();
        p.lidos = originais.clone();
        p.registro = reg.clone();
        p.meus_ajustes = meus.clone();
        p.fps = fps;
        p.linha_lida = regras::linha_lida(&originais);
    });
    // **R9b**: a câmera desta captura, para quem recebe (no modo compartilhado, sem controles e com
    // `outro_app`). O fps é o do tipo nativo e não muda nesta abertura: o teto do obturador também
    // não, e `definir_capacidades` não tem quando ser chamado.
    let remoto = LadoRemoto::de(&c);
    remoto.camera(&caps, fps, compartilhada, &reg);
    let mut envios = Envios { tocados: BTreeSet::new(), ultimo: BTreeMap::new(), hr: BTreeMap::new() };
    // **A câmera em automático de verdade** (§2.2, 06/10): o driver que outro app deixou em manual
    // recebe o Auto que a tela mostra, e esse passa a ser o "como abriu" da devolução.
    let normalizacao = if compartilhada { Vec::new() } else { regras::normalizacao_da_abertura(&reg, &caps, fps, &originais) };
    if !normalizacao.is_empty() {
        let l = envios.executar(&mut i, &normalizacao, &originais);
        registro::linha(format!("ajustes: a câmera não estava no automático que o registro pede: {}", l.join(", ")));
    }
    let originais = regras::originais_depois_da_normalizacao(&originais, &normalizacao);
    if compartilhada {
        registro::linha(format!("ajustes: modo compartilhado: os controles ficam apagados (\"{}\")", regras::FRASE_OUTRO_APP));
        if banc.medir_compartilhada {
            medir_compartilhada(&mut i, &caps, &originais);
        }
    } else {
        let p = regras::plano(&reg, &caps, fps, &originais, &envios.tocados);
        if !p.is_empty() {
            let l = envios.executar(&mut i, &p, &originais);
            registro::linha(format!("ajustes: o registro guardado foi reaplicado depois do primeiro quadro, em {} ms: {}", t0.elapsed().as_millis(), l.join(", ")));
        } else {
            registro::linha("ajustes: o registro guardado está todo no automático: nada é mandado à câmera");
        }
    }

    // O laço: os gestos, o roteiro da bancada, e a leitura de volta 4 vezes por segundo.
    let zero = Instant::now();
    let mut lidos = originais.clone();
    let mut ultima_leitura = Instant::now();
    let mut ultima_linha_de_bancada = Instant::now();
    let mut ultimo_envio: Option<Duration> = None;
    let mut pendente = false;
    let mut divergencias: BTreeMap<Propriedade, Divergencia> = BTreeMap::new();
    let roteiro = banc.roteiro.clone().unwrap_or_default();
    let mut proximo_passo = 0usize;
    let mut medida: Option<(usize, Instant)> = roteiro.first().map(|p| (0, zero + p.em.saturating_sub(Duration::from_millis(500))));
    let mut recusa_dita = false;
    // A gravação adiada (§6, achado I4): quando gravar o registro no disco.
    let mut gravar_em: Option<Instant> = None;
    let mut controlado: Option<String> = None;
    // A pouca luz (§3.1): o fps que chega, medido na leitura de 4 vezes por segundo.
    let mut medidor = MedidorDeFps::default();
    let mut vigia = VigiaDaPoucaLuz::default();
    let mut pouca_luz: Option<PoucaLuz> = None;
    loop {
        {
            let g = c.pedidos.lock().unwrap_or_else(|e| e.into_inner());
            let espera = if pendente { regras::INTERVALO_DOS_ENVIOS } else { Duration::from_millis(100) };
            let _ = c.aviso.wait_timeout_while(g, espera, |p| p.is_empty() && !c.remoto.load(Ordering::SeqCst) && !c.parar.load(Ordering::SeqCst));
        }
        if c.parar.load(Ordering::SeqCst) {
            break;
        }
        let mut gestos: Vec<Acao> = std::mem::take(&mut *c.pedidos.lock().unwrap_or_else(|e| e.into_inner()));
        // O roteiro da bancada: os passos que já venceram entram como gestos.
        while let Some(passo) = roteiro.get(proximo_passo).filter(|p| zero.elapsed() >= p.em) {
            for (chave, alvo) in &passo.ajustes {
                match regras::acoes_de_bancada(chave, *alvo, &caps, fps) {
                    Ok(v) => gestos.extend(v),
                    Err(e) => registro::linha(format!("ajustes: bancada: passo {} {chave}={alvo:?} pulado: {e}", proximo_passo + 1)),
                }
            }
            registro::linha(format!(
                "ajustes: bancada: passo {} aos {:.1} s: {}",
                proximo_passo + 1,
                passo.em.as_secs_f64(),
                passo.ajustes.iter().map(|(k, v)| format!("{k}={v:?}")).collect::<Vec<_>>().join(",")
            ));
            proximo_passo += 1;
            let prazo = roteiro.get(proximo_passo).map(|p| p.em.saturating_sub(passo.em)).unwrap_or(Duration::from_secs(5));
            let espera = prazo.saturating_sub(Duration::from_millis(500)).min(Duration::from_millis(3500));
            medida = Some((proximo_passo, Instant::now() + espera));
        }
        if !gestos.is_empty() {
            if compartilhada {
                if !recusa_dita {
                    recusa_dita = true;
                    registro::linha("ajustes: gesto ignorado: a câmera está no modo compartilhado");
                }
            } else {
                let mut mudou = false;
                for a in gestos {
                    if a == Acao::UsarMeusAjustes {
                        if let Some(m) = meus.as_ref().filter(|m| **m != reg) {
                            reg = m.clone();
                            mudou = true;
                            registro::linha("ajustes: usar meus ajustes");
                        }
                        continue;
                    }
                    mudou |= regras::aplicar_acao(&mut reg, a, &caps, &lidos);
                }
                if mudou {
                    pendente = true;
                    gravar_em = Some(Instant::now() + ADIAR_A_GRAVACAO);
                    c.publicar(|p| p.registro = reg.clone());
                    for d in divergencias.values_mut() {
                        d.limpar();
                    }
                    // A mudança local entra no núcleo **antes** de a fila remota ser tirada, nesta
                    // mesma volta: ela foi feita depois do que o receptor viu, e vence (§4, B1).
                    remoto.local(&reg);
                }
            }
        }
        // **Os pedidos remotos** (R9b), na mesma fila serial que os gestos locais. A fila é
        // conferida a cada volta (no máximo 100 ms), e o aviso da bombeada só encurta a espera.
        c.remoto.store(false, Ordering::SeqCst);
        if remoto.drenar(&mut reg, &caps, &lidos, fps, compartilhada) {
            pendente = true;
            gravar_em = Some(Instant::now() + ADIAR_A_GRAVACAO);
            c.publicar(|p| p.registro = reg.clone());
            for d in divergencias.values_mut() {
                d.limpar();
            }
        }
        if gravar_em.is_some_and(|t| Instant::now() >= t) {
            gravar_em = None;
            if banc.roteiro.is_none() {
                guardar(&link, &reg);
                if let Some(m) = regras::meus_ajustes(reg.clone()) {
                    meus = Some(m);
                    c.publicar(|p| p.meus_ajustes = meus.clone());
                }
            }
        }
        if pendente && regras::pode_enviar(ultimo_envio, zero.elapsed()) {
            pendente = false;
            ultimo_envio = Some(zero.elapsed());
            let p = regras::plano(&reg, &caps, fps, &originais, &envios.tocados);
            let l = envios.executar(&mut i, &p, &originais);
            if !l.is_empty() {
                registro::linha(format!("ajustes: enviado: {}", l.join(", ")));
            }
        }
        // A medida da bancada: o `Get` do que o passo mexeu, com a luma e o fps.
        if let Some((n, quando)) = medida {
            if Instant::now() >= quando {
                medida = None;
                let chaves: Vec<&str> = if n == 0 {
                    roteiro.iter().flat_map(|p| p.ajustes.iter().map(|(k, _)| k.as_str())).collect()
                } else {
                    roteiro[n - 1].ajustes.iter().map(|(k, _)| k.as_str()).collect()
                };
                let (luma, fps_medido) = crate::luma_de_bancada::ultima().map(|(l, f)| (Some(l), Some(f))).unwrap_or((None, None));
                let mut vistas = BTreeSet::new();
                for chave in chaves {
                    let props: Vec<Propriedade> = match Propriedade::da_chave(chave) {
                        Some(p) => vec![p],
                        None => caps.keys().copied().collect(),
                    };
                    for p in props {
                        if !vistas.insert(p) {
                            continue;
                        }
                        let lido = i.ler(p).ok();
                        let pedido = envios.ultimo.get(&p).copied();
                        let hr = envios.hr.get(&p).cloned().unwrap_or_else(|| "-".into());
                        registro::linha(regras::linha_da_medida(n, p.chave(), pedido, lido, luma, fps_medido, &hr));
                    }
                }
            }
        }
        if ultima_leitura.elapsed() >= regras::INTERVALO_DA_LINHA {
            ultima_leitura = Instant::now();
            let novos = i.ler_todos(&caps);
            if !novos.is_empty() {
                lidos = novos;
            }
            let agora = zero.elapsed();
            let desejado = regras::desejado(&reg, &caps, fps);
            let mut achada: Option<(Propriedade, i32, i32)> = None;
            let mut divergentes: Vec<Propriedade> = Vec::new();
            for p in regras::vigiadas(&reg) {
                let (Some(pedido), Some(lido), Some(f)) = (desejado.get(&p), lidos.get(&p), caps.get(&p)) else { continue };
                if divergencias.entry(p).or_default().observar(*pedido, lido.valor, f.passo, agora) {
                    divergentes.push(p);
                    if achada.is_none() {
                        achada = Some((p, lido.valor, *pedido));
                    }
                }
            }
            // R9b: o lido vai a quem recebe (o núcleo o manda no máximo 4 vezes por segundo), e o
            // "Controlado por" volta do núcleo para o painel.
            remoto.lido(&lidos, &caps, &divergentes);
            let agora_controlado = remoto.controlado_por();
            if agora_controlado != controlado {
                if let Some(n) = &agora_controlado {
                    registro::linha(format!("ajustes: câmera remota: controlado por \"{n}\""));
                }
                controlado = agora_controlado.clone();
            }
            // A frase da tela no idioma de agora (refeita a cada leitura: uma troca de idioma chega
            // aqui), e a do diário em português.
            let frase = achada.map(|(p, l, d)| regras::frase_da_divergencia(p, l, d));
            if frase.is_some() && c.painel.lock().unwrap_or_else(|e| e.into_inner()).divergencia != frase {
                if let Some((p, l, d)) = achada {
                    let no_diario = crate::idioma::com_idioma(crate::idioma::Idioma::Pt, || regras::frase_da_divergencia(p, l, d));
                    registro::linha(format!("ajustes: {no_diario}"));
                }
            }
            // **Pouca luz** (§3.1): o vigia do Mac sobre o fps que chega, só com o automático no
            // comando da exposição; no manual apaga na hora.
            let medido = medidor.observar(chegados.load(Ordering::Relaxed), agora);
            let agora_pouca_luz = if regras::vigia_da_pouca_luz_vale(&reg, &lidos, compartilhada) {
                medido.and_then(|m| vigia.observar(m, fps, agora)).map(|f| PoucaLuz::de(f, fps, &caps, compartilhada))
            } else {
                vigia.apagar();
                None
            };
            // O diário diz quando acende e apaga (e não a cada fps novo), com o fps medido.
            if agora_pouca_luz.is_some() != pouca_luz.is_some() {
                match agora_pouca_luz {
                    Some(_) => registro::linha(format!("ajustes: pouca luz acesa: {:.1} fps de {fps:.2}", medido.unwrap_or(0.0))),
                    None => registro::linha("ajustes: pouca luz apagada"),
                }
            }
            pouca_luz = agora_pouca_luz;
            let linha = regras::linha_lida(&lidos);
            c.publicar(|p| {
                p.lidos = lidos.clone();
                p.linha_lida = linha;
                p.divergencia = frase;
                p.controlado_por = controlado.clone();
                p.pouca_luz = pouca_luz;
            });
        }
        if banc.leitura && ultima_linha_de_bancada.elapsed() >= Duration::from_secs(1) {
            ultima_linha_de_bancada = Instant::now();
            let (luma, fps_medido) = crate::luma_de_bancada::ultima().map(|(l, f)| (Some(l), Some(f))).unwrap_or((None, None));
            for p in caps.keys() {
                registro::linha(regras::linha_da_medida(9999, p.chave(), None, lidos.get(p).copied(), luma, fps_medido, "-"));
            }
        }
    }

    // R9b: a câmera fecha para quem recebe, e a gravação que estava adiada sai agora.
    remoto.fechar();
    if gravar_em.is_some() && banc.roteiro.is_none() {
        guardar(&link, &reg);
    }
    // A devolução (§2.2): `Flags_Auto` no que o Quall mudou, ou o valor de antes.
    let t = Instant::now();
    let devolver = regras::devolver_tudo(&caps, &originais, &envios.tocados);
    if devolver.is_empty() {
        registro::linha("ajustes: nada a devolver (o Quall não mudou a câmera)");
    } else {
        // `executar` pula o que já está igual ao último envio; a devolução vai inteira.
        envios.ultimo.clear();
        let l = envios.executar(&mut i, &devolver, &originais);
        registro::linha(format!("ajustes: a câmera devolvida como estava em {} ms: {}", t.elapsed().as_millis(), l.join(", ")));
    }
    drop(i);
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
}

/// **A primeira medida da frente** (§6): o modo compartilhado aceita `Set`? Tenta um passo a mais no
/// brilho (ou no ganho), **segura 3 s** (a controladora da bancada lê uma vez por segundo, e é ela
/// quem diz se o valor chegou ao driver) e volta. Esta linha só diz o que **este** processo viu: o
/// `Set` devolveu ok e o `Get` daqui leu o pedido não provam que o driver mudou.
fn medir_compartilhada(i: &mut Interfaces, caps: &Capacidades, originais: &Lidos) {
    let Some((p, f)) = [Propriedade::Brilho, Propriedade::Ganho, Propriedade::Balanco]
        .into_iter()
        .find_map(|p| caps.get(&p).filter(|f| f.tem_manual() && f.max > f.min).map(|f| (p, *f)))
    else {
        registro::linha("ajustes: compartilhada: nenhuma propriedade manual para medir o Set");
        return;
    };
    let Some(antes) = originais.get(&p).copied() else {
        registro::linha(format!("ajustes: compartilhada: o Get de {} falhou; sem medida", p.chave()));
        return;
    };
    let alvo = if antes.valor + f.passo <= f.max { antes.valor + f.passo } else { antes.valor - f.passo };
    let e = Envio { prop: p, valor: alvo, bandeiras: regras::FLAGS_MANUAL };
    registro::linha(format!("ajustes: compartilhada: Set prop={} pedido={alvo} segurando 3 s", p.chave()));
    let r = i.escrever(&e);
    std::thread::sleep(Duration::from_millis(3000));
    let depois = i.ler(p);
    let volta = i.escrever(&Envio { prop: p, valor: antes.valor, bandeiras: if antes.bandeiras == 0 { regras::FLAGS_MANUAL } else { antes.bandeiras } });
    let aceitou = r.is_ok() && depois.as_ref().is_ok_and(|l| l.valor == alvo);
    registro::linha(format!(
        "ajustes: compartilhada: medida Set prop={} antes={} pedido={alvo} hr={} lido_depois={} volta={} b_leu_o_pedido={}",
        p.chave(),
        antes.valor,
        r.as_ref().map(|_| "ok".to_string()).unwrap_or_else(|e| e.clone()),
        depois.as_ref().map(|l| l.valor.to_string()).unwrap_or_else(|e| e.clone()),
        volta.as_ref().map(|_| "ok".to_string()).unwrap_or_else(|e| e.clone()),
        if aceitou { "sim" } else { "nao" }
    ));
}
