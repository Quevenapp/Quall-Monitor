//! **A tela R5 do Windows: o texto com a câmera** (`docs/teleprompter-com-camera.md` §2.5, §4, §5 e
//! §8.10, peça 7). É um modo da janela única do teleprompter (um prompter por aparelho sai de graça),
//! com as peças que a fase 4 acrescenta:
//!
//! - **o dono da captura** (`dono_da_captura.rs`), aberto com a tela e fechado com ela;
//! - **a prévia** numa janela filha, com swap chain do dono (`previa_da_camera.rs`);
//! - **a sessão de vídeo** (`camera.rs`) na 7877, independente da do prompter (a 7979);
//! - **o microfone** (`microfone.rs`), um botão que começa desligado;
//! - **a gravação** (`gravador_local.rs`), pelo botão e pelo controle remoto (§13 do contrato).
//!
//! **A divisão** (`divisao.rs`): o texto do lado da lente (em cima por padrão), 50/50, a borda
//! arrastável (20 % no mínimo), e a faixa dos estados e da barra **do lado longe da lente**. Nada
//! entre o texto e a lente: nem aviso, nem PIN, nem guia (§8.5 e §8.6).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

use super::super::camera::{ConfigDoVideo, FaseDoVideo, SessaoDeVideo};
use super::super::divisao::{self, LadoDoTexto, Ret};
use super::*;
use crate::idioma;
use crate::captura_de_camera::FonteDaCamera;
use crate::dono_da_captura::{self as dono_mod, DonoDaCaptura, FaseDoDono};
use crate::gravador_local::{FaseDaGravacao, Gravador};
use crate::microfone::Microfone;
use crate::previa_da_camera::{ControleDaPrevia, PreviaDaCamera};
use crate::regras_da_gravacao::{self as regras_g, DecisaoDoPedido, FaseDoGravador};

/// Como a tela R5 abre (a bancada; o produto usa o padrão).
#[derive(Debug, Clone, Default)]
pub struct ConfigDaTelaR5 {
    /// A câmera: o link, ou `sintetica` (a fonte do Quall no processo, com a régua; bancada).
    /// `None`: a lembrada, a integrada, a primeira.
    pub camera: Option<String>,
    pub pin_da_camera: Option<String>,
    /// `0`: a 7877.
    pub porta_da_camera: u16,
    pub so_local: bool,
    /// Bancada (só com `microfone_tom`): liga o microfone S s depois de a câmera abrir.
    pub microfone_apos: Option<f64>,
    /// Bancada: desliga D s depois de ligar.
    pub microfone_por: Option<f64>,
    /// Bancada: o conteúdo do microfone vira um seno desta frequência depois do carimbo.
    pub microfone_tom: Option<u32>,
    pub gravar_apos: Option<f64>,
    pub gravar_por: Option<f64>,
    /// Bancada: `TerminateProcess` no próprio processo M s depois de a gravação começar.
    pub matar_gravando_apos: Option<f64>,
    /// Bancada: esconde a prévia S s depois de abrir, por max(S, 30) s.
    pub esconder_previa_apos: Option<f64>,
    /// Bancada: a pasta das gravações no lugar de `Vídeos\Quall`.
    pub pasta_das_gravacoes: Option<PathBuf>,
    /// Bancada (R9): abre a janela "Ajustes da câmera", sem prévia, S s depois de a câmera abrir.
    pub abrir_ajustes_apos: Option<f64>,
}

/// A altura da faixa da tela R5, em DIP: os estados (duas linhas), os avisos, e duas fileiras de
/// botões.
const FAIXA_R5: i32 = 150;
/// A espessura da borda arrastável, em DIP.
const PEGA_R5: i32 = 10;
/// Quanto um recado da gravação fica na faixa.
const RECADO: Duration = Duration::from_secs(15);

const COR_DA_BORDA: COLORREF = COLORREF(0x00505050);
const VERMELHO_GRAVANDO: COLORREF = COLORREF(0x003C3CE8);

/// As peças da tela R5.
pub(super) struct TelaR5 {
    cfg: ConfigDaTelaR5,
    pub(super) dono: Arc<DonoDaCaptura>,
    microfone: Arc<Microfone>,
    video: Option<Arc<SessaoDeVideo>>,
    gravador: Option<Arc<Gravador>>,
    previa: HWND,
    controle_previa: Arc<ControleDaPrevia>,
    previa_pendurada: bool,
    pub(super) lado: LadoDoTexto,
    fracao: f64,
    escondida: bool,
    pub(super) arrastando_borda: bool,
    cameras: Vec<(String, String)>,
    camera: Option<usize>,
    /// O espelho do texto estava ligado e a tela o desligou ao abrir (§2.5): volta ao sair, se
    /// ninguém o ligou durante.
    espelho_desligado_aqui: bool,
    espelho_ligado_durante: bool,
    pedido_tentado: Option<u64>,
    gravando_relatado: bool,
    recado: Option<(String, Instant)>,
    chave: String,
    mutex: Option<HANDLE>,
    periodo: bool,
    aberta_em: Instant,
    camera_aberta_em: Option<Instant>,
    bancada_microfone: u8,
    bancada_gravar: u8,
    bancada_esconder: u8,
    gravando_desde: Option<Instant>,
    pasta: Option<PathBuf>,
    /// O botão "Ajustes da câmera" à mostra (R9 §4.1): a câmera abriu pelo link e tem controles.
    ajustes_visiveis: bool,
    /// Bancada (R9): a janela dos ajustes já foi aberta pelo `--abrir-ajustes-apos`.
    bancada_ajustes: bool,
}

fn largo_z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn ret_para_rect(r: Ret) -> RECT {
    RECT { left: r.esquerda, top: r.topo, right: r.direita, bottom: r.base }
}

/// A janela da prévia: fundo preto, e nada mais (a swap chain do dono desenha por cima).
unsafe extern "system" fn wndproc_da_previa(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                FillRect(hdc, &ps.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // Os cliques na prévia vão para a janela (o arrasto da borda, o foco).
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn criar_janela_da_previa(pai: HWND) -> windows::core::Result<HWND> {
    unsafe {
        let instancia = GetModuleHandleW(None)?;
        let classe = w!("QuallPreviaR5");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc_da_previa),
            hInstance: instancia.into(),
            lpszClassName: classe,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            classe,
            PCWSTR::null(),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            10,
            10,
            Some(pai),
            None,
            Some(instancia.into()),
            None,
        )
    }
}

/// O tempo de gravação, "m:ss" ou "h:mm:ss".
fn tempo(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn gb(bytes: u64) -> String {
    format!("{} GB", idioma::decimal(bytes as f64 / (1024.0 * 1024.0 * 1024.0), 1))
}

impl Tela {
    // -----------------------------------------------------------------------------------------
    // Abrir e fechar
    // -----------------------------------------------------------------------------------------

    /// **A tela R5**: o prompter de sempre, e as peças da câmera.
    pub(super) fn abrir_prompter_com_camera(&mut self, hwnd: HWND) {
        let cfg = self.cfg.r5.clone().unwrap_or_default();
        // Uma câmera por processo, e o mesmo nome de anúncio (a revisão do plano, M7): com a janela
        // principal emitindo, esta tela não abre.
        if !dono_mod::tomar_para_r5() {
            self.mensagem = dono_mod::FRASE_DA_CAMERA_OCUPADA_PELA_JANELA.into();
            registro::linha(format!("teleprompter: r5: recusada — {}", self.mensagem));
            self.mudar_de_modo(hwnd, Modo::Escolha);
            return;
        }
        // Uma tela R5 por computador (M8): o mutex nomeado atravessa processos.
        let mutex = unsafe {
            let h = windows::Win32::System::Threading::CreateMutexW(None, true, w!("Local\\QuallTelaR5"));
            let ja = windows::Win32::Foundation::GetLastError() == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
            match h {
                Ok(h) if !ja => Some(h),
                Ok(h) => {
                    let _ = windows::Win32::Foundation::CloseHandle(h);
                    self.mensagem = "Outro Quall já mostra o teleprompter com a câmera neste computador.".into(); // i18n: chave (traduzida ao mostrar)
                    registro::linha(format!("teleprompter: r5: recusada — {}", self.mensagem));
                    dono_mod::soltar_da_r5();
                    self.mudar_de_modo(hwnd, Modo::Escolha);
                    return;
                }
                Err(_) => None,
            }
        };
        self.abrir_prompter(hwnd);
        if self.modo != Modo::Prompter {
            dono_mod::soltar_da_r5();
            if let Some(m) = mutex {
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(m);
                }
            }
            return;
        }
        // O controle remoto vê que esta tela grava (§13.5).
        if let Some(t) = &self.teleprompter {
            match t.ligar_gravacao(true) {
                Ok(()) => registro::linha("teleprompter: r5: a tela diz que grava (ligar_gravacao)"),
                Err(e) => registro::linha(format!("teleprompter: r5: !! ligar_gravacao falhou: {e}")),
            }
        }
        // **O espelho do texto nasce desligado** (§2.5), e a dívida fica nos ajustes (sobrevive à
        // morte do processo: o prompter comum paga na abertura seguinte).
        let mut espelho_desligado_aqui = false;
        if self.estado.as_ref().is_some_and(|e| e.espelho) {
            self.editar(hwnd, "espelho=false (a tela com câmera nasce sem espelho)", |t| t.definir_espelho(false)); // i18n: fora
            self.ajustes.r5_espelho_devido = true;
            super::super::gravar_ajustes(&self.ajustes);
            espelho_desligado_aqui = true;
        }
        // As câmeras do catálogo, e a escolhida.
        let (fontes, _) = crate::fontes::cameras(None);
        let cameras: Vec<(String, String)> = fontes.iter().map(|f| (f.id.clone(), f.nome.clone())).collect();
        let (fonte, nome, id, indice) = match cfg.camera.as_deref() {
            Some("sintetica") => (FonteDaCamera::DoQuallNoProcesso { regua: true }, "Câmera sintética".to_string(), "sintetica".to_string(), None), // i18n: fora (bancada)
            Some(link) => match cameras.iter().position(|(i, _)| i.eq_ignore_ascii_case(link)) {
                Some(i) => (FonteDaCamera::Link(cameras[i].0.clone()), cameras[i].1.clone(), cameras[i].0.clone(), Some(i)),
                None => {
                    registro::linha(format!("teleprompter: r5: !! --r5-camera {link} não está no catálogo; a escolha é a de sempre"));
                    self.escolha_inicial(&cameras)
                }
            },
            None => self.escolha_inicial(&cameras),
        };
        let previa = match criar_janela_da_previa(hwnd) {
            Ok(h) => h,
            Err(e) => {
                registro::linha(format!("teleprompter: r5: !! a janela da prévia não nasceu: {e}"));
                HWND::default()
            }
        };
        let alvo = hwnd.0 as isize;
        let acordar = move || unsafe {
            let _ = PostMessageW(Some(HWND(alvo as *mut core::ffi::c_void)), WM_ACORDAR, WPARAM(0), LPARAM(0));
        };
        let dono = DonoDaCaptura::abrir(fonte, nome.clone(), id, dono_mod::TETO_DA_MELHOR_IMAGEM, Box::new(acordar));
        let microfone = Microfone::novo(dono.origem, cfg.microfone_tom, Box::new(acordar));
        let video_cfg = ConfigDoVideo {
            porta: cfg.porta_da_camera,
            pin: cfg.pin_da_camera.clone(),
            pin_de_bancada: cfg.pin_da_camera.is_some(),
            anunciar: !self.cfg.sem_mdns,
            so_local: cfg.so_local,
            fps: 30,
        };
        let video = SessaoDeVideo::iniciar(Arc::clone(&dono), Arc::clone(&microfone), video_cfg, Box::new(acordar));
        // O `IGNORE_TIMER_RESOLUTION` do processo só vale com um pedido de resolução (a revisão do
        // plano, menor): 1 ms enquanto a tela R5 estiver aberta.
        let periodo = unsafe { windows::Win32::Media::timeBeginPeriod(1) } == 0;
        let lado = self.ajustes.r5_lado.as_deref().and_then(LadoDoTexto::da_chave).unwrap_or_default();
        let fracao = self.ajustes.r5_fracao.unwrap_or(divisao::FRACAO_PADRAO);
        let espelho_da_previa = self.ajustes.r5_previa_espelhada.unwrap_or(true);
        let pasta = cfg.pasta_das_gravacoes.clone().or_else(|| crate::gravador_local::pasta_padrao().ok());
        registro::linha(format!(
            "teleprompter: r5: tela com câmera aberta — câmera \"{nome}\" ({} no catálogo), lado do texto {}, fração {:.2}, prévia espelhada {}, gravações em {}",
            cameras.len(),
            lado.chave(),
            fracao,
            espelho_da_previa,
            pasta.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(sem pasta)".into())
        ));
        // Os pendentes, fora da janela (é disco).
        if let Some(p) = pasta.clone() {
            let _ = std::thread::Builder::new().name("quall.r5.pendentes".into()).spawn(move || {
                let linhas = crate::gravador_local::recuperar_pendentes(&p);
                if linhas.is_empty() {
                    registro::linha("r5 pendentes: nenhum");
                }
                for l in linhas {
                    registro::linha(format!("r5 pendentes: {l}"));
                }
            });
        }
        self.r5 = Some(Box::new(TelaR5 {
            cfg,
            dono,
            microfone,
            video: Some(video),
            gravador: None,
            previa,
            controle_previa: ControleDaPrevia::novo(espelho_da_previa),
            previa_pendurada: false,
            lado,
            fracao,
            escondida: false,
            arrastando_borda: false,
            cameras,
            camera: indice,
            espelho_desligado_aqui,
            espelho_ligado_durante: false,
            pedido_tentado: None,
            gravando_relatado: false,
            recado: None,
            chave: String::new(),
            mutex,
            periodo,
            aberta_em: Instant::now(),
            camera_aberta_em: None,
            bancada_microfone: 0,
            bancada_gravar: 0,
            bancada_esconder: 0,
            gravando_desde: None,
            pasta,
            ajustes_visiveis: false,
            bancada_ajustes: false,
        }));
        self.publicar_papel();
        self.preencher_cameras();
        unsafe {
            SendMessageW(self.c.espelho_previa, BM_SETCHECK, Some(WPARAM(if espelho_da_previa { BST_CHECKED } else { 0 })), None);
        }
        self.aplicar_titulo(hwnd);
        self.pedir_layout(hwnd);
        self.posicionar(hwnd);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }

    /// A câmera da abertura: a lembrada, a integrada, a primeira (`regras_r5::camera_inicial`).
    fn escolha_inicial(&self, cameras: &[(String, String)]) -> (FonteDaCamera, String, String, Option<usize>) {
        match crate::regras_r5::camera_inicial(cameras, self.ajustes.r5_camera.as_deref()) {
            Some(i) => (FonteDaCamera::Link(cameras[i].0.clone()), cameras[i].1.clone(), cameras[i].0.clone(), Some(i)),
            None => (FonteDaCamera::Link(String::new()), "nenhuma câmera".into(), String::new(), None), // i18n: chave (o nome; traduzido ao mostrar)
        }
    }

    pub(super) fn preencher_cameras(&self) {
        let Some(r) = &self.r5 else { return };
        unsafe {
            SendMessageW(self.c.camera, CB_RESETCONTENT, None, None);
            for (_, nome) in &r.cameras {
                let t = largo_z(&idioma::tf("Câmera: {}", &[nome]));
                SendMessageW(self.c.camera, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
            }
            if let Some(i) = r.camera {
                SendMessageW(self.c.camera, CB_SETCURSEL, Some(WPARAM(i)), None);
            }
        }
    }

    /// **Espera bombeando mensagens** (a DXGI pode mandar mensagem à janela da prévia enquanto a thread
    /// do dono apresenta: a thread da janela nunca bloqueia esperando o dono).
    fn esperar_bombeando(&self, prazo: Duration, pronto: &dyn Fn() -> bool) -> bool {
        let fim = Instant::now() + prazo;
        loop {
            if pronto() {
                return true;
            }
            if Instant::now() >= fim {
                return false;
            }
            // **Só as mensagens enviadas** (`SendMessage`, o que a DXGI pode mandar à janela da
            // prévia), e com a janela da tela em espera: o procedimento dela não toca na `Tela`, que
            // está emprestada aqui (a revisão do código: despachar `WM_PAINT` reentrava nela).
            super::EM_ESPERA.store(true, Ordering::SeqCst);
            unsafe {
                let mut msg = MSG::default();
                let _ = MsgWaitForMultipleObjectsEx(None, 10, QS_SENDMESSAGE, MWMO_INPUTAVAILABLE);
                let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
            }
            super::EM_ESPERA.store(false, Ordering::SeqCst);
        }
    }

    /// Fecha a tela R5, na ordem: a gravação (o arquivo fecha e o controle vê), a sessão de vídeo, o
    /// microfone, a prévia, o dono. O espelho do texto volta em [`Tela::r5_devolver_o_espelho`], depois
    /// de a sessão do prompter parar.
    pub(super) fn r5_fechar(&mut self, hwnd: HWND) {
        if self.r5.is_none() {
            return;
        }
        if self.r5.as_ref().is_some_and(|r| r.gravador.is_some()) {
            self.parar_gravacao("a tela com câmera fechou"); // i18n: fora (diário)
            let g = self.r5.as_ref().and_then(|r| r.gravador.clone());
            if let Some(g) = g {
                if !self.esperar_bombeando(Duration::from_secs(15), &|| g.terminou()) {
                    registro::linha("teleprompter: r5: !! a gravação não fechou em 15 s");
                }
            }
            self.r5_tique_da_gravacao(hwnd);
        }
        if let Some(t) = &self.teleprompter {
            let _ = t.definir_gravando(false);
            let _ = t.ligar_gravacao(false);
        }
        let Some(r) = self.r5.as_mut() else { return };
        // A prévia para de desenhar **antes** de qualquer espera (a revisão do código, M1).
        r.controle_previa.visivel.store(false, Ordering::SeqCst);
        let video = r.video.take();
        if let Some(v) = &video {
            v.pedir_parada();
        }
        let microfone = Arc::clone(&r.microfone);
        microfone.pedir_desligar("a tela com câmera fechou"); // i18n: fora (diário)
        // As esperas bombeiam as mensagens enviadas (a janela nunca bloqueia esperando outra thread).
        if let Some(v) = video {
            if !self.esperar_bombeando(Duration::from_secs(6), &|| v.terminou()) {
                registro::linha("teleprompter: r5: !! a sessão de vídeo não terminou em 6 s");
            }
        }
        if !self.esperar_bombeando(Duration::from_secs(3), &|| microfone.terminou()) {
            registro::linha("teleprompter: r5: !! o microfone não fechou em 3 s");
        }
        // A janela dos ajustes (R9) fecha antes da câmera: a thread dela devolve a câmera na soltura.
        if !crate::janela_dos_ajustes::fechar_se_aberta(Duration::ZERO) && !self.esperar_bombeando(Duration::from_secs(3), &|| !crate::janela_dos_ajustes::aberta()) {
            registro::linha("teleprompter: r5: !! a janela dos ajustes da câmera não fechou em 3 s");
        }
        let Some(r) = self.r5.as_mut() else { return };
        let dono = Arc::clone(&r.dono);
        let previa = r.previa;
        let mutex = r.mutex.take();
        let periodo = std::mem::take(&mut r.periodo);
        dono.pedir_fechar();
        if !self.esperar_bombeando(Duration::from_secs(5), &|| dono.terminou()) {
            registro::linha("teleprompter: r5: !! o dono da captura não fechou em 5 s");
        }
        let _ = dono.esperar(Duration::from_millis(10));
        unsafe {
            if !previa.is_invalid() {
                let _ = DestroyWindow(previa);
            }
            if let Some(m) = mutex {
                let _ = windows::Win32::System::Threading::ReleaseMutex(m);
                let _ = windows::Win32::Foundation::CloseHandle(m);
            }
            if periodo {
                let _ = windows::Win32::Media::timeEndPeriod(1);
            }
        }
        dono_mod::soltar_da_r5();
        registro::linha(format!(
            "teleprompter: r5: tela com câmera fechada — a câmera entregou {} quadros, maior buraco {} ms",
            dono.entregues(),
            dono.buraco_maior().as_millis()
        ));
    }

    /// O espelho do texto volta ao sair (se esta tela o desligou e ninguém o ligou durante), **depois**
    /// de a sessão do prompter parar (o controle não vê a volta) e antes de o salvo ser gravado.
    pub(super) fn r5_devolver_o_espelho(&mut self, hwnd: HWND) {
        let Some(r) = self.r5.as_ref() else { return };
        let devolver = r.espelho_desligado_aqui && !r.espelho_ligado_durante && self.ajustes.r5_espelho_devido;
        if devolver {
            if let Some(t) = &self.teleprompter {
                let _ = t.definir_espelho(true);
                registro::linha("teleprompter: r5: o espelho do texto volta a ligado (a tela o desligou ao abrir)");
            }
        }
        if self.ajustes.r5_espelho_devido {
            self.ajustes.r5_espelho_devido = false;
            super::super::gravar_ajustes(&self.ajustes);
        }
        let _ = hwnd;
    }

    // -----------------------------------------------------------------------------------------
    // A divisão
    // -----------------------------------------------------------------------------------------

    fn r5_cliente(&self, hwnd: HWND) -> Ret {
        let mut c = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut c);
        }
        Ret::novo(c.left, c.top, c.right, c.bottom)
    }

    /// A faixa some só com H: a tela cheia tira a moldura do Windows, não a faixa (ela fica do lado
    /// longe da lente, e é dela o botão de sair da tela cheia).
    fn r5_altura_da_faixa(&self) -> i32 {
        if self.faixas_ocultas {
            0
        } else {
            escala(self.dpi, FAIXA_R5)
        }
    }

    pub(super) fn r5_divisao(&self, hwnd: HWND) -> Option<divisao::Divisao> {
        let r = self.r5.as_ref()?;
        Some(divisao::dividir(self.r5_cliente(hwnd), r.lado, r.fracao, self.r5_altura_da_faixa(), escala(self.dpi, PEGA_R5)))
    }

    /// A área do texto na tela R5.
    pub(super) fn r5_area_do_texto(&self, hwnd: HWND) -> Option<RECT> {
        self.r5_divisao(hwnd).map(|d| ret_para_rect(d.texto))
    }

    pub(super) fn r5_na_borda(&self, hwnd: HWND, x: i32, y: i32) -> bool {
        self.r5_divisao(hwnd).is_some_and(|d| d.borda.contem(x, y)) && self.rascunho.is_none()
    }

    pub(super) fn r5_cursor_da_borda(&self) -> PCWSTR {
        if self.r5.as_ref().is_some_and(|r| r.lado.lado_a_lado()) {
            IDC_SIZEWE
        } else {
            IDC_SIZENS
        }
    }

    pub(super) fn r5_comecar_borda(&mut self, hwnd: HWND, x: i32, y: i32) -> bool {
        if !self.r5_na_borda(hwnd, x, y) {
            return false;
        }
        if let Some(r) = self.r5.as_mut() {
            r.arrastando_borda = true;
        }
        true
    }

    pub(super) fn r5_mover_borda(&mut self, hwnd: HWND, x: i32, y: i32) {
        let cliente = self.r5_cliente(hwnd);
        let faixa = self.r5_altura_da_faixa();
        let Some(r) = self.r5.as_mut() else { return };
        if !r.arrastando_borda {
            return;
        }
        let f = divisao::fracao_do_arrasto(cliente, r.lado, faixa, x, y);
        if (f - r.fracao).abs() > 1e-4 {
            r.fracao = f;
            self.posicionar(hwnd);
            self.pedir_layout(hwnd);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    }

    pub(super) fn r5_soltar_borda(&mut self) {
        let Some(r) = self.r5.as_mut() else { return };
        if std::mem::take(&mut r.arrastando_borda) {
            self.ajustes.r5_fracao = Some(r.fracao);
            super::super::gravar_ajustes(&self.ajustes);
            registro::linha(format!("teleprompter: r5: a borda ficou em {:.0} % para o texto", r.fracao * 100.0));
        }
    }

    // -----------------------------------------------------------------------------------------
    // A disposição e a pintura
    // -----------------------------------------------------------------------------------------

    /// Os botões da tela R5 na faixa, e a janela da prévia. Chamada no fim de `posicionar`.
    pub(super) fn r5_posicionar(&mut self, hwnd: HWND) {
        let r5_botoes = [self.c.microfone, self.c.gravar, self.c.esconder_previa, self.c.espelho_previa, self.c.lado_do_texto, self.c.camera, self.c.ajustes_da_camera];
        let editando = self.rascunho.is_some();
        let Some(d) = self.r5_divisao(hwnd) else {
            for h in r5_botoes {
                mostrar(h, false);
            }
            return;
        };
        let faixa_visivel = d.faixa.altura() > 0 && !editando;
        for h in r5_botoes {
            mostrar(h, faixa_visivel);
        }
        let ajustes_visiveis = self.r5.as_ref().is_some_and(|r| r.ajustes_visiveis);
        mostrar(self.c.ajustes_da_camera, faixa_visivel && ajustes_visiveis);
        // **Com as faixas ocultas, nada de botão solto** (a revisão do código): o "Esperar de novo" e
        // o "Fechar" da espera parada ficariam no alto — entre o texto e a lente. H traz a faixa.
        if d.faixa.altura() <= 0 {
            mostrar(self.c.esperar_de_novo, false);
            mostrar(self.c.sair, false);
        }
        let dpi = self.dpi;
        let mover = |h: HWND, x: i32, y: i32, w: i32, a: i32| unsafe {
            let _ = MoveWindow(h, x, y, w, a, true);
        };
        if faixa_visivel {
            let a = escala(dpi, 30);
            let espaco = escala(dpi, 4);
            let y2 = d.faixa.base - escala(dpi, 38);
            let y1 = y2 - escala(dpi, 36);
            // Os comandos do prompter, a "Fonte automática", a tela cheia (um quadrado) e o Fechar: a
            // fileira de cima da barra.
            let n = self.c.comandos.len() as i32 + 3;
            let largura = d.faixa.largura() - 2 * escala(dpi, 8);
            let q = a;
            let w = ((largura - q - espaco * (n + 2)) / n).clamp(escala(dpi, 34), escala(dpi, 96));
            let mut x = d.faixa.esquerda + escala(dpi, 8);
            if !editando && self.modo == Modo::Prompter {
                for (_, h) in &self.c.comandos {
                    mover(*h, x, y1, w, a);
                    x += w + espaco;
                }
                mover(self.c.fonte_auto, x, y1, 2 * w + espaco, a);
                x += 2 * w + 2 * espaco;
                mover(self.c.tela_cheia, x, y1, q, a);
                x += q + espaco;
                mover(self.c.sair, x, y1, w, a);
            }
            // A fileira da câmera.
            let mut x = d.faixa.esquerda + escala(dpi, 8);
            let larguras = [150, 150, 150, 150, 140];
            for (h, lw) in [self.c.microfone, self.c.gravar, self.c.esconder_previa, self.c.espelho_previa, self.c.lado_do_texto].into_iter().zip(larguras) {
                let lw = escala(dpi, lw);
                mover(h, x, y2, lw, a);
                x += lw + espaco;
            }
            // Os ajustes da câmera (R9 §4.1), antes do seletor da câmera.
            if ajustes_visiveis {
                let lw = escala(dpi, 40);
                mover(self.c.ajustes_da_camera, x, y2, lw, a);
                x += lw + espaco;
            }
            let resto = (d.faixa.direita - escala(dpi, 8) - x).max(escala(dpi, 120));
            mover(self.c.camera, x, y2, resto, escala(dpi, 300));
            // A espera parada do prompter: o "Esperar de novo" também na faixa.
            mover(self.c.esperar_de_novo, d.faixa.direita - escala(dpi, 160), d.faixa.topo + escala(dpi, 4), escala(dpi, 150), escala(dpi, 26));
        }
        let rotulos = self.r5_rotulos();
        unsafe {
            let _ = SetWindowTextW(self.c.microfone, PCWSTR(largo_z(&rotulos.0).as_ptr()));
            let _ = SetWindowTextW(self.c.gravar, PCWSTR(largo_z(&rotulos.1).as_ptr()));
            let _ = SetWindowTextW(self.c.lado_do_texto, PCWSTR(largo_z(&rotulos.2).as_ptr()));
        }
        // A janela da prévia: na área dela, à mostra só com a câmera aberta, sem esconder e sem falha.
        let Some(r) = self.r5.as_ref() else { return };
        let mostrar_previa = !r.escondida
            && r.previa_pendurada
            && r.dono.fase() == FaseDoDono::Aberto
            && r.controle_previa.falha.lock().unwrap_or_else(|e| e.into_inner()).is_none()
            && !editando;
        if !r.previa.is_invalid() {
            unsafe {
                let _ = MoveWindow(r.previa, d.previa.esquerda, d.previa.topo, d.previa.largura().max(1), d.previa.altura().max(1), true);
            }
            mostrar(r.previa, mostrar_previa);
            r.controle_previa.definir_tamanho(d.previa.largura().max(0) as u32, d.previa.altura().max(0) as u32);
            r.controle_previa.visivel.store(mostrar_previa, Ordering::SeqCst);
        }
    }

    /// Os rótulos dos botões: o microfone, o gravar, o lado do texto.
    fn r5_rotulos(&self) -> (String, String, String) {
        let Some(r) = self.r5.as_ref() else { return Default::default() };
        let m = r.microfone.estado();
        let mic = match (m.ligado, m.aberto) {
            (true, true) => idioma::t("Microfone: ligado").to_string(),
            (true, false) => idioma::t("Microfone: abrindo…").to_string(),
            (false, _) if m.frase == crate::regras_r5::FRASE_DA_PRIVACIDADE => idioma::t("Microfone: negado").to_string(),
            (false, _) if !m.frase.is_empty() => idioma::t("Microfone: falhou").to_string(),
            _ => idioma::t("Microfone: desligado").to_string(),
        };
        let grav = match r.gravador.as_ref().map(|g| g.estado()) {
            Some(e) => match e.fase {
                FaseDaGravacao::Gravando => idioma::tf("■ Parar {}", &[&tempo(e.desde.map(|d| d.elapsed()).unwrap_or_default())]),
                FaseDaGravacao::Abrindo => idioma::t("Gravar (abrindo…)").into(),
                FaseDaGravacao::Fechando => idioma::t("Fechando o arquivo…").into(),
                FaseDaGravacao::Fechada { .. } => idioma::t("● Gravar").into(),
            },
            None => {
                if m.ligado {
                    idioma::t("● Gravar").into()
                } else {
                    idioma::t("● Gravar (sem som)").into()
                }
            }
        };
        (mic, grav, idioma::t(r.lado.rotulo()).to_string())
    }

    /// A pintura da tela R5: a faixa (os dois PINs, os estados, os avisos) e a área da prévia quando
    /// ela não está à mostra. O texto é desenhado pelo caminho de sempre (`desenhar_texto`).
    pub(super) fn r5_pintar(&self, hwnd: HWND, hdc: HDC) {
        let Some(d) = self.r5_divisao(hwnd) else { return };
        let Some(r) = self.r5.as_ref() else { return };
        let dpi = self.dpi;
        // A borda entre o texto e a prévia.
        unsafe {
            let b = CreateSolidBrush(COR_DA_BORDA);
            let mut linha = ret_para_rect(d.borda);
            if r.lado.lado_a_lado() {
                let meio = (linha.left + linha.right) / 2;
                linha.left = meio - 1;
                linha.right = meio + 1;
            } else {
                let meio = (linha.top + linha.bottom) / 2;
                linha.top = meio - 1;
                linha.bottom = meio + 1;
            }
            FillRect(hdc, &linha, b);
            let _ = DeleteObject(b.into());
        }
        // A prévia, quando ela não está à mostra: a frase de por quê.
        let fase = r.dono.fase();
        let falha_da_previa = r.controle_previa.falha.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let na_previa = match &fase {
            // O nome é o da câmera (do sistema), e as falhas vêm do dono (outro módulo): traduzidas se
            // forem frases da tabela, e o detalhe técnico como veio.
            FaseDoDono::Abrindo => Some(idioma::tf("Abrindo a câmera \"{}\"…", &[&idioma::tr(&r.dono.nome)])),
            FaseDoDono::Falhou(f) => Some(idioma::tf("A câmera não abriu: {}", &[&idioma::tr(f)])),
            FaseDoDono::Acabou(f) => Some(idioma::tf("{} Escolha a câmera de novo para reabrir.", &[&idioma::tr(f)])),
            FaseDoDono::Fechado => Some(idioma::t("A câmera está fechada.").into()),
            FaseDoDono::Aberto if r.escondida => Some(idioma::t("Câmera escondida — a transmissão e a gravação seguem.").into()),
            FaseDoDono::Aberto => falha_da_previa
                .map(|f| idioma::tf("A prévia não desenha nesta sessão ({}). A câmera, a transmissão e a gravação seguem.", &[&f])),
        };
        if let Some(texto) = na_previa {
            let mut p = ret_para_rect(d.previa);
            unsafe {
                FillRect(hdc, &p, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            }
            p.left += escala(dpi, 16);
            p.right -= escala(dpi, 16);
            desenhar_em(hdc, &self.fontes.corpo, TINTA_CLARA_FRACA, &mut p, &texto, DT_CENTER | DT_VCENTER | DT_WORDBREAK);
        }
        if d.faixa.altura() <= 0 {
            return;
        }
        // A faixa: fundo escuro, e as linhas de estado no alto dela.
        let f = ret_para_rect(d.faixa);
        unsafe {
            FillRect(hdc, &f, self.fundo_escuro);
        }
        let x0 = f.left + escala(dpi, 12);
        let x1 = f.right - escala(dpi, 12);
        let meio = (x0 + x1) / 2;
        let y = f.top + escala(dpi, 4);
        let linha_a = escala(dpi, 22);
        // O texto (7979), à esquerda.
        let painel = self.painel.clone();
        let fase_p = painel.as_ref().map(|p| p.fase);
        let texto_do_prompter = if self.cfg.sem_sessao {
            "Texto: sem controle (bancada)".to_string() // i18n: fora (bancada)
        } else if fase_p == Some(Fase::Parada) {
            idioma::tf("Texto: a espera parou. {}", &[&idioma::tr(&painel.as_ref().map(|p| p.mensagem.clone()).unwrap_or_default())])
        } else {
            let pin = painel.as_ref().map(|p| p.pin.clone()).unwrap_or_default();
            let end = painel.as_ref().and_then(|p| p.endereco.clone()).unwrap_or_else(|| idioma::t("sem rede").into());
            let quem = if self.avisos.par_sumido && painel.as_ref().is_some_and(|p| p.ja_houve_sessao) {
                idioma::t("CONTROLE SUMIDO — o texto segue").to_string()
            } else if fase_p == Some(Fase::Conectada) {
                idioma::tf("controlado por {}", &[&idioma::tr(&painel.as_ref().map(|p| p.par.clone()).unwrap_or_default())])
            } else {
                idioma::t("esperando o controle").to_string()
            };
            idioma::tf("Texto: PIN {} · {} · {}", &[&pin, &end, &quem])
        };
        let cor_p = if self.avisos.par_sumido { LARANJA } else { TINTA_CLARA };
        let mut ra = RECT { left: x0, top: y, right: meio - escala(dpi, 8), bottom: y + linha_a };
        desenhar_em(hdc, &self.fontes.pequeno, cor_p, &mut ra, &texto_do_prompter, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        // O vídeo (7877), à direita.
        let v = r.video.as_ref().map(|v| v.painel());
        let texto_do_video = match &v {
            None => idioma::t("Vídeo: encerrado").to_string(),
            Some(p) => {
                let end = p.endereco.clone().unwrap_or_else(|| idioma::t("sem rede").into());
                match &p.fase {
                    FaseDoVideo::Preparando => idioma::t("Vídeo: esperando a câmera abrir").to_string(),
                    FaseDoVideo::Esperando => idioma::tf("Vídeo: PIN {} · {} · esperando o receptor", &[&p.pin, &end]),
                    FaseDoVideo::Transmitindo => idioma::tf("Vídeo: transmitindo para {} · {}", &[&p.par, &idioma::tr(&p.resumo)]),
                    FaseDoVideo::Parada(f) => idioma::tf("Vídeo: parado — {}", &[&idioma::tr(f)]),
                }
            }
        };
        let mut rb = RECT { left: meio + escala(dpi, 8), top: y, right: x1, bottom: y + linha_a };
        desenhar_em(hdc, &self.fontes.pequeno, TINTA_CLARA, &mut rb, &texto_do_video, DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        // Os avisos, numa linha: a gravação (vermelho), o microfone, o recado, e o resto.
        let mut avisos: Vec<(String, COLORREF)> = Vec::new();
        if let Some(g) = r.gravador.as_ref() {
            let e = g.estado();
            if e.fase == FaseDaGravacao::Gravando {
                let m = r.microfone.estado();
                let mut t = idioma::tf("● GRAVANDO {}", &[&tempo(e.desde.map(|d| d.elapsed()).unwrap_or_default())]);
                if let Some(l) = e.livre {
                    t.push_str(&idioma::tf(" · sobram {}", &[&gb(l)]));
                }
                if !m.ligado {
                    t.push_str(idioma::t(" · Gravando SEM SOM — ligue o microfone"));
                }
                avisos.push((t, VERMELHO_GRAVANDO));
            }
        }
        if let Some((t, quando)) = &r.recado {
            if quando.elapsed() < RECADO {
                avisos.push((idioma::tr(t), LARANJA));
            }
        }
        let m = r.microfone.estado();
        if !m.frase.is_empty() {
            // A frase é do microfone (outro módulo): traduzida se estiver na tabela.
            avisos.push((idioma::tr(&m.frase), LARANJA));
        }
        if let Some(p) = &v {
            if !p.aviso.is_empty() && p.fase != FaseDoVideo::Transmitindo {
                avisos.push((idioma::tr(&p.aviso), TINTA_CLARA_FRACA));
            }
        }
        if let Some(a) = self.aviso_da_fonte_mostrado() {
            avisos.push((a, LARANJA));
        }
        // R9b: quem recebe o vídeo mexeu na câmera agora.
        if let Some(n) = r.dono.ajustes().and_then(|p| p.painel_controlado_por()) {
            avisos.push((idioma::tf("Controlado por {}", &[&n]), LARANJA));
        }
        // Pouca luz (§3.1 dos controles): o automático baixou o fps para clarear. A frase inteira,
        // no tom fraco: é informação, e não falha (a faixa é larga o bastante para ela).
        if let Some(l) = r.dono.ajustes().and_then(|p| p.painel_pouca_luz()) {
            avisos.push((l.texto(), TINTA_CLARA_FRACA));
        }
        if self.avisos.atualize_o_app {
            avisos.push((idioma::t("O controle fala outra versão: atualize o app").into(), LARANJA));
        }
        let mut rc = RECT { left: x0, top: y + linha_a, right: x1, bottom: y + 2 * linha_a };
        if avisos.is_empty() {
            let e = self.estado_ou_padrao();
            let espelho_do_texto = if e.espelho { idioma::t(" · espelho do texto") } else { "" };
            let previa = if r.controle_previa.espelho.load(Ordering::Relaxed) { idioma::t("como espelho") } else { idioma::t("sem espelho") };
            let (velocidade, fonte, margem, linha) = (idioma::decimal(e.velocidade, 2), um_decimal(e.fonte), pct(e.margem), pct(e.linha_de_leitura));
            let valores = idioma::tf("{} linha/s · fonte {} · margem {} · linha {}{} · prévia {}", &[&velocidade, &fonte, &margem, &linha, &espelho_do_texto, &previa]);
            desenhar_em(hdc, &self.fontes.pequeno, TINTA_CLARA_FRACA, &mut rc, &valores, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        } else {
            let cor = avisos[0].1;
            let texto = avisos.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("  ·  ");
            desenhar_em(hdc, &self.fontes.corpo, cor, &mut rc, &texto, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        }
    }

    /// As áreas que a tela R5 repinta quando o que ela diz muda: a faixa e a prévia (com a frase).
    pub(super) fn r5_invalidar(&self, hwnd: HWND) {
        let Some(d) = self.r5_divisao(hwnd) else { return };
        unsafe {
            let _ = InvalidateRect(Some(hwnd), Some(&ret_para_rect(d.faixa)), false);
            let _ = InvalidateRect(Some(hwnd), Some(&ret_para_rect(d.previa)), false);
            let _ = InvalidateRect(Some(hwnd), Some(&ret_para_rect(d.borda)), false);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Os botões
    // -----------------------------------------------------------------------------------------

    pub(super) fn r5_comando(&mut self, hwnd: HWND, id: usize, aviso: u32) -> bool {
        if self.r5.is_none() {
            return false;
        }
        match (id, aviso) {
            (ID_MICROFONE, BN_CLICKED) => self.alternar_microfone(hwnd, "o botão"), // i18n: fora (diário)
            (ID_GRAVAR, BN_CLICKED) => {
                let gravando = self.r5.as_ref().and_then(|r| r.gravador.as_ref()).is_some_and(|g| !matches!(g.estado().fase, FaseDaGravacao::Fechada { .. }));
                if gravando {
                    self.parar_gravacao("o botão"); // i18n: fora (diário)
                } else if let Err(m) = self.comecar_gravacao("o botão") { // i18n: fora (diário)
                    self.r5_recado(idioma::tf("Não gravou: {}", &[&idioma::tr(&m)]));
                }
            }
            (ID_ESCONDER_PREVIA, BN_CLICKED) => {
                let esconder = marcado(self.c.esconder_previa);
                self.esconder_previa(hwnd, esconder, "o botão"); // i18n: fora (diário)
            }
            (ID_ESPELHO_PREVIA, BN_CLICKED) => {
                let v = marcado(self.c.espelho_previa);
                if let Some(r) = self.r5.as_ref() {
                    r.controle_previa.espelho.store(v, Ordering::SeqCst);
                }
                self.ajustes.r5_previa_espelhada = Some(v);
                super::super::gravar_ajustes(&self.ajustes);
                registro::linha(format!("teleprompter: r5: prévia como espelho: {v}"));
            }
            (ID_LADO_DO_TEXTO, BN_CLICKED) => {
                if let Some(r) = self.r5.as_mut() {
                    r.lado = r.lado.proximo();
                    self.ajustes.r5_lado = Some(r.lado.chave().to_string());
                    registro::linha(format!("teleprompter: r5: lado do texto: {}", r.lado.chave()));
                }
                super::super::gravar_ajustes(&self.ajustes);
                self.pedir_layout(hwnd);
            }
            (ID_AJUSTES_DA_CAMERA, BN_CLICKED) => {
                // **Sem prévia** (R9 §4.2): a vaga do dono é da prévia desta tela, que fica à vista.
                if let Some(r) = self.r5.as_ref() {
                    crate::janela_dos_ajustes::abrir(Arc::clone(&r.dono), false);
                }
                return true;
            }
            (ID_CAMERA, CBN_DROPDOWN) => {
                // O catálogo de agora (uma câmera pode ter entrado ou voltado), com a escolhida
                // pelo link (M3).
                let (fontes, _) = crate::fontes::cameras(None);
                if let Some(r) = self.r5.as_mut() {
                    let atual = r.camera.and_then(|i| r.cameras.get(i)).map(|c| c.0.clone());
                    r.cameras = fontes.iter().map(|f| (f.id.clone(), f.nome.clone())).collect();
                    r.camera = atual.and_then(|a| r.cameras.iter().position(|c| c.0.eq_ignore_ascii_case(&a)));
                }
                self.preencher_cameras();
                return true;
            }
            (ID_CAMERA, CBN_SELCHANGE) => {
                let i = unsafe { SendMessageW(self.c.camera, CB_GETCURSEL, None, None) }.0;
                if i >= 0 {
                    self.trocar_de_camera(hwnd, i as usize);
                }
            }
            _ => return false,
        }
        self.posicionar(hwnd);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        true
    }

    fn r5_recado(&mut self, t: String) {
        registro::linha(format!("teleprompter: r5: {t}"));
        if let Some(r) = self.r5.as_mut() {
            r.recado = Some((t, Instant::now()));
        }
    }

    fn alternar_microfone(&mut self, hwnd: HWND, porque: &str) {
        let Some(r) = self.r5.as_ref() else { return };
        let m = Arc::clone(&r.microfone);
        if m.ligado() {
            m.pedir_desligar(porque);
        } else {
            m.ligar(porque);
        }
        let _ = hwnd;
    }

    fn esconder_previa(&mut self, hwnd: HWND, esconder: bool, porque: &str) {
        let Some(r) = self.r5.as_mut() else { return };
        if r.escondida == esconder {
            return;
        }
        r.escondida = esconder;
        registro::linha(format!(
            "teleprompter: r5: prévia {} ({porque}) — a câmera continua: entregues={} rede={} gravador={}",
            if esconder { "escondida" } else { "à mostra" },
            r.dono.entregues(),
            if r.dono.rede_pendurada() { "pendurada" } else { "solta" },
            if r.dono.gravador_pendurado() { "pendurado" } else { "solto" }
        ));
        unsafe {
            SendMessageW(self.c.esconder_previa, BM_SETCHECK, Some(WPARAM(if esconder { BST_CHECKED } else { 0 })), None);
        }
        self.posicionar(hwnd);
    }

    /// Troca a câmera: fecha o vídeo e o dono, e abre de novo com a outra (o mesmo PIN do vídeo).
    /// Gravando, não troca.
    fn trocar_de_camera(&mut self, hwnd: HWND, i: usize) {
        let Some(r) = self.r5.as_ref() else { return };
        if r.gravador.is_some() {
            self.r5_recado(idioma::t("Pare a gravação para trocar de câmera.").into());
            self.preencher_cameras();
            return;
        }
        let mesma = r.camera == Some(i) && matches!(r.dono.fase(), FaseDoDono::Aberto | FaseDoDono::Abrindo);
        if mesma || i >= r.cameras.len() {
            return;
        }
        let (link, nome) = r.cameras[i].clone();
        let pin = r.video.as_ref().map(|v| v.painel().pin.replace(' ', ""));
        let mic_ligado = r.microfone.ligado();
            registro::linha("teleprompter: r5: trocando de câmera");
        // Fecha o que existe, menos a janela da prévia: a prévia para primeiro, e as esperas bombeiam
        // as mensagens enviadas (M1).
        let (video, mic_velho, dono_velho) = {
            let r = self.r5.as_mut().unwrap();
            r.controle_previa.visivel.store(false, Ordering::SeqCst);
            let v = r.video.take();
            if let Some(v) = &v {
                v.pedir_parada();
            }
            r.microfone.pedir_desligar("troca de câmera"); // i18n: fora (diário)
            (v, Arc::clone(&r.microfone), Arc::clone(&r.dono))
        };
        if let Some(v) = video {
            let _ = self.esperar_bombeando(Duration::from_secs(6), &|| v.terminou());
        }
        let _ = self.esperar_bombeando(Duration::from_secs(3), &|| mic_velho.terminou());
        dono_velho.pedir_fechar();
        let _ = self.esperar_bombeando(Duration::from_secs(5), &|| dono_velho.terminou());
        let alvo = hwnd.0 as isize;
        let acordar = move || unsafe {
            let _ = PostMessageW(Some(HWND(alvo as *mut core::ffi::c_void)), WM_ACORDAR, WPARAM(0), LPARAM(0));
        };
        let r = self.r5.as_mut().unwrap();
        let dono = DonoDaCaptura::abrir(FonteDaCamera::Link(link.clone()), nome, link.clone(), dono_mod::TETO_DA_MELHOR_IMAGEM, Box::new(acordar));
        let microfone = Microfone::novo(dono.origem, r.cfg.microfone_tom, Box::new(acordar));
        if mic_ligado {
            microfone.ligar("a troca de câmera (estava ligado)"); // i18n: fora (diário)
        }
        // O PIN do vídeo continua o mesmo, mas é **herdado**, não de bancada: não vai ao registro (M2).
        let pin_de_bancada = r.cfg.pin_da_camera.is_some();
        let video_cfg = ConfigDoVideo { porta: r.cfg.porta_da_camera, pin, pin_de_bancada, anunciar: !self.cfg.sem_mdns, so_local: r.cfg.so_local, fps: 30 };
        r.video = Some(SessaoDeVideo::iniciar(Arc::clone(&dono), Arc::clone(&microfone), video_cfg, Box::new(acordar)));
        r.dono = dono;
        r.microfone = microfone;
        r.camera = Some(i);
        r.previa_pendurada = false;
        r.camera_aberta_em = None;
        *r.controle_previa.falha.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.ajustes.r5_camera = Some(link);
        super::super::gravar_ajustes(&self.ajustes);
    }

    // -----------------------------------------------------------------------------------------
    // A gravação
    // -----------------------------------------------------------------------------------------

    /// **Começa a gravar** (o botão, o controle, a bancada). `Err` com o motivo legível — é o que vai
    /// ao controle remoto (`recusar_gravacao`).
    fn comecar_gravacao(&mut self, quem: &str) -> std::result::Result<(), String> {
        // Guardado em português; a tela traduz ao mostrar. (Os motivos do gravador nascem no idioma
        // do prompter, e é nele que a recusa vai ao controle pelo fio.) i18n: chave
        let Some(r) = self.r5.as_ref() else { return Err("o prompter não está na tela que grava".into()) };
        if r.gravador.is_some() {
            return Ok(());
        }
        let alvo = self.hwnd_para_acordar();
        let acordar = move || unsafe {
            let _ = PostMessageW(Some(HWND(alvo as *mut core::ffi::c_void)), WM_ACORDAR, WPARAM(0), LPARAM(0));
        };
        let g = crate::gravador_local::comecar_no_dono(&r.dono, r.pasta.clone(), quem, Box::new(acordar))?;
        let r = self.r5.as_mut().unwrap();
        r.microfone.pendurar_gravador(Some(g.ramal_do_som()));
        r.gravador = Some(g);
        r.gravando_relatado = false;
        r.gravando_desde = None;
        Ok(())
    }

    fn hwnd_para_acordar(&self) -> isize {
        super::ABERTA.load(Ordering::SeqCst)
    }

    /// Pede o fim da gravação: o dono e o microfone soltam o gravador na hora, e o arquivo fecha na
    /// thread dele. A resposta ao controle (`definir_gravando(false)`) sai quando ele fechar.
    fn parar_gravacao(&mut self, motivo: &str) {
        let Some(r) = self.r5.as_mut() else { return };
        let Some(g) = r.gravador.clone() else { return };
        g.parar(motivo);
        let velha = r.dono.pendurar_gravador(None);
        drop(velha);
        r.microfone.pendurar_gravador(None);
    }

    /// O estado do gravador → a réplica (§13): `definir_gravando(true)` quando o primeiro quadro
    /// entrou no arquivo, `false` quando ele fechou, venha de onde vier.
    fn r5_tique_da_gravacao(&mut self, _hwnd: HWND) {
        let Some(r) = self.r5.as_mut() else { return };
        let Some(g) = r.gravador.clone() else { return };
        let e = g.estado();
        match &e.fase {
            FaseDaGravacao::Gravando if !r.gravando_relatado => {
                r.gravando_relatado = true;
                r.gravando_desde = Some(Instant::now());
                if let Some(t) = &self.teleprompter {
                    let res = t.definir_gravando(true);
                    registro::linha(format!("teleprompter: r5: gravando — definir_gravando(true) → {res:?}"));
                }
            }
            FaseDaGravacao::Fechada { arquivo, segundos, motivo, inteira } => {
                let recado = match (arquivo, inteira) {
                    (Some(a), true) => {
                        let nome = a.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        idioma::tf("Gravação salva ({}) em {}", &[&tempo(Duration::from_secs_f64(*segundos)), &nome])
                    }
                    // O motivo é do gravador (outro módulo): traduzido se estiver na tabela.
                    (Some(_), false) => {
                        idioma::tf("A gravação fechou com defeito ({}); o arquivo é recuperado na próxima abertura", &[&idioma::tr(motivo)])
                    }
                    (None, _) => idioma::tf("Não gravou: {}", &[&idioma::tr(motivo)]),
                };
                let relatado = std::mem::take(&mut r.gravando_relatado);
                r.gravador = None;
                r.gravando_desde = None;
                let _ = r.dono.pendurar_gravador(None);
                r.microfone.pendurar_gravador(None);
                if let Some(t) = &self.teleprompter {
                    if relatado {
                        let res = t.definir_gravando(false);
                        registro::linha(format!("teleprompter: r5: a gravação fechou ({motivo}) — definir_gravando(false) → {res:?}"));
                    } else if let Some(p) = t.estado().ok().and_then(|e| e.pedido_de_gravacao) {
                        // Um "gravar" do controle que começou e não chegou ao primeiro quadro: recusa.
                        if p.gravar && r.pedido_tentado == Some(p.n) {
                            let _ = t.recusar_gravacao(p.n, &regras_g::motivo_para_o_fio(motivo));
                        }
                    }
                }
                self.r5_recado(recado);
            }
            _ => {}
        }
    }

    /// **O pedido do controle** (§13.2): relido a cada bit `GRAVACAO` e a cada mudança do gravador.
    fn r5_decidir_pedido(&mut self) {
        let Some(t) = self.teleprompter.clone() else { return };
        let Some(p) = t.estado().ok().and_then(|e| e.pedido_de_gravacao) else { return };
        let Some(r) = self.r5.as_ref() else { return };
        let fase = match r.gravador.as_ref().map(|g| g.estado().fase) {
            None | Some(FaseDaGravacao::Fechada { .. }) => FaseDoGravador::Parado,
            Some(FaseDaGravacao::Abrindo) => FaseDoGravador::Abrindo,
            Some(FaseDaGravacao::Gravando) => FaseDoGravador::Gravando,
            Some(FaseDaGravacao::Fechando) => FaseDoGravador::Fechando,
        };
        match regras_g::decidir_pedido(p.n, p.gravar, fase, r.pedido_tentado) {
            DecisaoDoPedido::Nada => {}
            DecisaoDoPedido::Aceitar(g) => {
                let res = t.definir_gravando(g);
                registro::linha(format!("teleprompter: r5: pedido do controle n={} já atendido — definir_gravando({g}) → {res:?}", p.n));
            }
            DecisaoDoPedido::Comecar => {
                registro::linha(format!("teleprompter: r5: gravação: pedido do controle n={} gravar", p.n));
                if let Some(r) = self.r5.as_mut() {
                    r.pedido_tentado = Some(p.n);
                }
                if let Err(m) = self.comecar_gravacao("o controle") { // i18n: fora (diário)
                    let res = t.recusar_gravacao(p.n, &regras_g::motivo_para_o_fio(&m));
                    registro::linha(format!("teleprompter: r5: recusado ({m}) → {res:?}"));
                    self.r5_recado(idioma::tf("O controle pediu para gravar, e não deu: {}", &[&idioma::tr(&m)]));
                }
            }
            DecisaoDoPedido::Parar => {
                registro::linha(format!("teleprompter: r5: gravação: pedido do controle n={} parar", p.n));
                if let Some(r) = self.r5.as_mut() {
                    r.pedido_tentado = Some(p.n);
                }
                self.parar_gravacao("pedido do controle remoto"); // i18n: fora (diário)
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // O tique
    // -----------------------------------------------------------------------------------------

    /// A cada tique da janela: a prévia pendurada quando a câmera abre, a gravação e o controle, o
    /// espelho ligado durante, a bancada, e a faixa repintada quando o que ela diz muda.
    pub(super) fn r5_tique(&mut self, hwnd: HWND, bits: u32) {
        let Some(r) = self.r5.as_mut() else { return };
        // Os ajustes da câmera (R9 §4.1): o botão aparece quando a câmera pelo link tem controles.
        let ajustes = r.dono.ajustes().is_some_and(|p| p.fase() != crate::regras_dos_controles::FaseDosAjustes::SemControles);
        if ajustes != r.ajustes_visiveis {
            r.ajustes_visiveis = ajustes;
            self.posicionar(hwnd);
        }
        let Some(r) = self.r5.as_mut() else { return };
        // A câmera abriu: a prévia se pendura (nada de DXGI aqui; a swap chain nasce na thread do dono).
        if !r.previa_pendurada && r.dono.fase() == FaseDoDono::Aberto && !r.previa.is_invalid() {
            if let Some(info) = r.dono.info() {
                let p = PreviaDaCamera::nova(r.previa, Arc::clone(&r.controle_previa), info.faixa_completa, info.matriz_709);
                r.dono.pendurar_previa(Some(Box::new(p)));
                r.previa_pendurada = true;
                r.camera_aberta_em = Some(Instant::now());
                registro::linha(format!("teleprompter: r5: a câmera abriu ({}x{}); a prévia se pendurou no dono", info.largura, info.altura));
                self.posicionar(hwnd);
            }
        }
        // A câmera que acabou (ou não abriu): o seletor fica sem escolha, e escolher a mesma de novo a
        // reabre (M3; o `CBN_SELCHANGE` não dispara no item já escolhido).
        if let Some(r) = self.r5.as_mut() {
            if matches!(r.dono.fase(), FaseDoDono::Acabou(_) | FaseDoDono::Falhou(_)) && r.camera.is_some() {
                r.camera = None;
                unsafe {
                    SendMessageW(self.c.camera, CB_SETCURSEL, Some(WPARAM(usize::MAX)), None);
                }
            }
        }
        // A câmera que acabou gravando: a gravação para (§5.3, a câmera perdida). A pausa não para
        // (a decisão de 21/09, M12).
        let acabou = matches!(self.r5.as_ref().map(|r| r.dono.fase()), Some(FaseDoDono::Acabou(_)));
        if acabou && self.r5.as_ref().is_some_and(|r| r.gravador.is_some()) {
            if let Some(FaseDoDono::Acabou(f)) = self.r5.as_ref().map(|r| r.dono.fase()) {
                self.parar_gravacao(&format!("a câmera acabou: {f}")); // i18n: fora (diário)
            }
        }
        self.r5_tique_da_gravacao(hwnd);
        if bits & quall_core::teleprompter::mudou::GRAVACAO != 0 || self.r5.as_ref().is_some_and(|r| r.gravador.is_some()) {
            self.r5_decidir_pedido();
        } else if self.estado.as_ref().is_some_and(|e| e.pedido_de_gravacao.is_some()) {
            self.r5_decidir_pedido();
        }
        if self.estado.as_ref().is_some_and(|e| e.espelho) {
            if let Some(r) = self.r5.as_mut() {
                r.espelho_ligado_durante = true;
            }
        }
        self.r5_bancada(hwnd);
        // A faixa: repinta quando o que ela diz muda (e a cada segundo gravando).
        let Some(r) = self.r5.as_ref() else { return };
        let v = r.video.as_ref().map(|v| v.painel().versao).unwrap_or(0);
        let m = r.microfone.estado();
        let g = r.gravador.as_ref().map(|g| {
            let e = g.estado();
            format!("{:?}|{}|{:?}", e.fase, e.desde.map(|d| d.elapsed().as_secs()).unwrap_or(0), e.livre.map(|l| l / (100 * 1024 * 1024)))
        });
        // R9b: "Controlado por <aparelho>" entra e sai da faixa (4 s depois de um pedido remoto).
        let controlada = r.dono.ajustes().and_then(|p| p.painel_controlado_por());
        // Pouca luz (§3.1 dos controles): entra e sai da faixa pelo vigia do fps.
        let pouca_luz = r.dono.ajustes().and_then(|p| p.painel_pouca_luz());
        let chave = format!(
            "{:?}|{v}|{}|{}|{}|{:?}|{g:?}|{}|{:?}|{}|{controlada:?}|{pouca_luz:?}",
            r.dono.fase(),
            m.ligado,
            m.aberto,
            m.frase,
            r.recado.as_ref().map(|(t, q)| (t.clone(), q.elapsed() < RECADO)),
            r.escondida,
            r.controle_previa.falha.lock().unwrap_or_else(|e| e.into_inner()).is_some(),
            r.previa_pendurada
        );
        if chave != r.chave {
            let mudou_a_previa = !r.chave.is_empty();
            if let Some(r) = self.r5.as_mut() {
                r.chave = chave;
            }
            let _ = mudou_a_previa;
            self.posicionar(hwnd);
            self.r5_invalidar(hwnd);
        }
    }

    /// **A bancada da tela R5** (só com as bandeiras): o microfone (só com o tom), a gravação, a
    /// morte gravando e a prévia escondida.
    fn r5_bancada(&mut self, hwnd: HWND) {
        let Some(r) = self.r5.as_ref() else { return };
        let Some(aberta) = r.camera_aberta_em else { return };
        let s = aberta.elapsed().as_secs_f64();
        let cfg = r.cfg.clone();
        // **R9**: os ajustes da câmera pela bancada (`--abrir-ajustes-apos`), pelo mesmo caminho do
        // botão — a janela sem prévia, com a da R5 à vista.
        if let (Some(apos), false, true) = (cfg.abrir_ajustes_apos, r.bancada_ajustes, r.ajustes_visiveis) {
            if s >= apos {
                registro::linha("teleprompter: r5: bancada: abrindo os ajustes da câmera (--abrir-ajustes-apos)");
                crate::janela_dos_ajustes::abrir(Arc::clone(&r.dono), false);
                self.r5.as_mut().unwrap().bancada_ajustes = true;
            }
        }
        let Some(r) = self.r5.as_ref() else { return };
        let (bm, bg, be) = (r.bancada_microfone, r.bancada_gravar, r.bancada_esconder);
        // O microfone da bancada **só com o tom** (`audio.md` §8.1): sem ele, o microfone de verdade
        // só abre pelo botão.
        if let Some(apos) = cfg.microfone_apos {
            if cfg.microfone_tom.is_none() {
                if bm == 0 {
                    registro::linha("teleprompter: r5: !! --microfone-apos sem --microfone-tom: recusado (a bancada só liga o microfone com o tom)");
                    self.r5.as_mut().unwrap().bancada_microfone = 3;
                }
            } else if bm == 0 && s >= apos {
                self.r5.as_mut().unwrap().bancada_microfone = 1;
                self.alternar_microfone(hwnd, "bancada: --microfone-apos");
            } else if bm == 1 {
                if let Some(por) = cfg.microfone_por {
                    if s >= apos + por {
                        self.r5.as_mut().unwrap().bancada_microfone = 2;
                        self.alternar_microfone(hwnd, "bancada: --microfone-por");
                    }
                }
            }
        }
        if let Some(apos) = cfg.gravar_apos {
            if bg == 0 && s >= apos {
                self.r5.as_mut().unwrap().bancada_gravar = 1;
                if let Err(m) = self.comecar_gravacao("bancada") {
                    self.r5_recado(format!("Não gravou (bancada): {m}")); // i18n: fora (bancada)
                }
            }
        }
        let desde = self.r5.as_ref().and_then(|r| r.gravando_desde);
        if let (Some(d), 1) = (desde, bg) {
            if let Some(m) = cfg.matar_gravando_apos {
                if d.elapsed().as_secs_f64() >= m {
                    registro::linha("teleprompter: r5: bancada: TerminateProcess agora, gravando (--matar-gravando-apos)");
                    unsafe {
                        let _ = windows::Win32::System::Threading::TerminateProcess(windows::Win32::System::Threading::GetCurrentProcess(), 9);
                    }
                }
            }
            if let Some(por) = cfg.gravar_por {
                if d.elapsed().as_secs_f64() >= por {
                    self.r5.as_mut().unwrap().bancada_gravar = 2;
                    self.parar_gravacao("bancada: --gravar-por");
                }
            }
        }
        if let Some(apos) = cfg.esconder_previa_apos {
            let por = apos.max(30.0);
            if be == 0 && s >= apos {
                self.r5.as_mut().unwrap().bancada_esconder = 1;
                self.esconder_previa(hwnd, true, "bancada: --esconder-previa-apos");
            } else if be == 1 && s >= apos + por {
                self.r5.as_mut().unwrap().bancada_esconder = 2;
                self.esconder_previa(hwnd, false, "bancada: fim da prévia escondida"); // i18n: fora (bancada)
            }
        }
    }

    /// O relato de bancada da tela R5, para o registro final.
    pub(super) fn r5_relato(&self) -> Option<serde_json::Value> {
        let r = self.r5.as_ref()?;
        let m = r.microfone.estado();
        Some(serde_json::json!({
            "camera": r.dono.nome,
            "fase_da_camera": format!("{:?}", r.dono.fase()),
            "entregues": r.dono.entregues(),
            "buraco_maior_ms": r.dono.buraco_maior().as_millis() as u64,
            "lado_do_texto": r.lado.chave(),
            "fracao_do_texto": r.fracao,
            "previa_escondida": r.escondida,
            "previa_desenhados": r.controle_previa.desenhados.load(Ordering::Relaxed),
            "microfone_ligado": m.ligado,
            "microfone_frase": m.frase,
            "video": r.video.as_ref().map(|v| format!("{:?}", v.painel().fase)),
            "aberta_ha_s": r.aberta_em.elapsed().as_secs_f64(),
        }))
    }

    // -----------------------------------------------------------------------------------------
    // O controle: gravar no prompter (§13.8)
    // -----------------------------------------------------------------------------------------

    /// O botão "Gravar no prompter" da tela de controle: só quando o prompter diz que grava.
    pub(super) fn controle_gravar_visivel(&self) -> bool {
        self.modo == Modo::Controle
            && self.sessao.as_ref().is_some_and(|s| !s.terminou())
            && self.estado.as_ref().is_some_and(|e| e.par_entende_gravar)
    }

    pub(super) fn controle_rotulo_de_gravar(&self) -> String {
        match self.estado.as_ref().and_then(|e| e.gravando_ha_ms) {
            Some(ms) => idioma::tf("■ Parar a gravação ({})", &[&tempo(Duration::from_millis(ms))]),
            None => match self.estado.as_ref().and_then(|e| e.pedido_de_gravacao.clone()) {
                Some(p) if p.gravar => idioma::t("Gravando… (esperando o prompter)").into(),
                Some(_) => idioma::t("Parando… (esperando o prompter)").into(),
                None => idioma::t("● Gravar no prompter").into(),
            },
        }
    }

    pub(super) fn controle_gravar(&mut self) {
        let Some(t) = self.teleprompter.clone() else { return };
        let gravando = self.estado.as_ref().is_some_and(|e| e.gravando_ha_ms.is_some());
        let r = if gravando { t.pedir_parar() } else { t.pedir_gravar() };
        registro::linha(format!("teleprompter: controle: pedido de {} → {r:?}", if gravando { "parar" } else { "gravar" }));
    }

    /// A linha da gravação na tela de controle: gravando há quanto, ou a última recusa.
    pub(super) fn controle_linha_de_gravar(&self) -> Option<String> {
        let e = self.estado.as_ref()?;
        if !e.par_entende_gravar {
            return None;
        }
        if let Some(ms) = e.gravando_ha_ms {
            return Some(idioma::tf("● O prompter está gravando há {}", &[&tempo(Duration::from_millis(ms))]));
        }
        // O motivo vem do prompter, pelo fio (no idioma dele): traduzido se estiver na tabela.
        e.gravacao_recusada.as_ref().map(|r| {
            let motivo = idioma::tr(&r.motivo);
            if r.gravar {
                idioma::tf("O prompter recusou gravar: {}", &[&motivo])
            } else {
                idioma::tf("O prompter recusou parar: {}", &[&motivo])
            }
        })
    }
}
