//! **A janela "Ajustes da câmera"** (R9, `docs/controles-de-camera.md` §4.2), em Win32 puro, no
//! molde da do teleprompter (`teleprompter/tela.rs`): uma janela própria, numa thread própria, com
//! o laço de mensagens dela.
//!
//! - **Na câmera comum, com a prévia dentro**: a `PreviaDaCamera` pendurada no dono da câmera comum,
//!   que tem a vaga livre (a janela principal não tem prévia, e ajustar a exposição sem ver a imagem
//!   não serve). **Na tela R5, sem prévia**: o dono tem uma vaga só, e a R5 já a usa.
//! - **O desenho é o do "Estúdio de bolso"**: os botões são `BUTTON`s nativos desenhados no
//!   `NM_CUSTOMDRAW` pelas peças de `estilo.rs` (nunca `BS_OWNERDRAW`, `telas-estudio.md` §11.5), os
//!   deslizantes são `msctls_trackbar32` em degraus, e o resto é pintado pelo Direct2D a partir do
//!   quadro que `modelo_dos_ajustes` compõe.
//! - **A janela não fala com o driver**: ela lê o painel que a thread dos ajustes publica (no máximo
//!   4 vezes por segundo, §3.6) e pede gestos (`PontaDosAjustes::pedir`). Um deslizante arrastado
//!   manda um gesto por posição; a thread junta e manda ao driver a no máximo 15 por segundo.
//! - **Fecha sozinha** quando a câmera fecha (o dono acabou), e solta a prévia antes.
//!
//! **A tela nunca espera o dono sem bombear mensagens** (`previa_da_camera.rs`): pendurar e soltar
//! a prévia toma o cadeado do dono, que pode estar no `Present` — e a DXGI pode mandar mensagem a
//! esta janela. As duas coisas correm numa thread de ajuda, com esta bombeando.
//!
//! # R9b: as duas pontas do controle remoto (`docs/controle-remoto-da-camera.md`)
//!
//! - **No aparelho que filma**, a janela ganha a opção "Permitir controle remoto da câmera" (vale
//!   para o app, `camera_remota::definir_permissao`) e a linha "Controlado por <aparelho>", que a
//!   thread dos ajustes publica no painel. A câmera comum e a tela R5 abrem esta mesma janela, e a
//!   opção fica onde a pessoa já ajusta a câmera.
//! - **No aparelho que recebe** ([`abrir_remota`]), a mesma janela, sem prévia e sem a opção,
//!   desenha a câmera do outro lado a partir do estado do núcleo (`modelo_dos_ajustes_remotos`), e
//!   cada gesto vira um pedido. Ela fecha sozinha quando a sessão de recepção acaba.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    CDDS_PREERASE, CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT, CDRF_SKIPDEFAULT, NMCUSTOMDRAW, NMHDR,
    NM_CUSTOMDRAW,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::camera_remota::ControleRemoto;
use crate::dono_da_captura::{DonoDaCaptura, FaseDoDono};
use crate::estilo::{self, Ret};
use crate::modelo_da_janela::{Aparencia, EstadoDoControle};
use crate::modelo_dos_ajustes::{self as modelo, ControleDosAjustes, EspecieDosAjustes, EstadoDosAjustes, Gesto, QuadroDosAjustes};
use crate::modelo_dos_ajustes_remotos::{EstadoRemoto, GestoRemoto};
use crate::previa_da_camera::{ControleDaPrevia, PreviaDaCamera};
use crate::registro;
use crate::regras_dos_controles::{self as regras, Aba, Acao, ModoDeExposicao};

/// A janela aberta agora (uma por processo). Zero: nenhuma.
static ABERTA: AtomicIsize = AtomicIsize::new(0);
/// **A janela espera a thread da prévia** bombeando: as mensagens desse meio vão ao
/// `DefWindowProcW`, sem tocar na `Janela`, que está emprestada (o molde de `teleprompter/tela.rs`).
static EM_ESPERA: AtomicBool = AtomicBool::new(false);

const ID_RELOGIO: usize = 1;
const BST_CHECKED: usize = 1;
const BST_UNCHECKED: usize = 0;
// As mensagens do trackbar (`commctrl.h`), escritas à mão como as `BST_*` de `janela.rs`.
const TBM_GETPOS: u32 = WM_USER;
const TBM_SETPOS: u32 = WM_USER + 5;
const TBM_SETRANGEMIN: u32 = WM_USER + 7;
const TBM_SETRANGEMAX: u32 = WM_USER + 8;
const TBS_NOTICKS: u32 = 0x0010;
const TB_THUMBTRACK: u32 = 5;
const TB_ENDTRACK: u32 = 8;
const ESTILO: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);

fn escala(dpi: u32, v: f32) -> i32 {
    (v * dpi as f32 / 96.0).round() as i32
}

fn em_pixels(dpi: u32, r: Ret) -> RECT {
    RECT { left: escala(dpi, r.x), top: escala(dpi, r.y), right: escala(dpi, r.x + r.l), bottom: escala(dpi, r.y + r.a) }
}

fn largo(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// De quem é a câmera que a janela ajusta.
#[derive(Clone)]
enum Fonte {
    /// A câmera deste aparelho, pelo dono (a thread dos ajustes).
    Local(Arc<DonoDaCaptura>),
    /// **R9b**: a câmera de quem filma, do outro lado da sessão de recepção.
    Remota(Arc<ControleRemoto>),
}

/// **Abre a janela** dos ajustes da câmera do `dono` (com a prévia dentro na câmera comum, sem ela
/// na R5). Já aberta: só vem para a frente.
pub fn abrir(dono: Arc<DonoDaCaptura>, com_previa: bool) {
    abrir_com(Fonte::Local(dono), com_previa);
}

/// **Abre a janela em modo remoto** (R9b): a câmera do aparelho que filma, nesta sessão de
/// recepção. Já aberta (a local ou a remota): só vem para a frente.
pub fn abrir_remota(controle: Arc<ControleRemoto>) {
    abrir_com(Fonte::Remota(controle), false);
}

fn abrir_com(fonte: Fonte, com_previa: bool) {
    let h = ABERTA.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let hwnd = HWND(h as *mut core::ffi::c_void);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }
    let prefixo = registro::prefixo_desta_thread();
    let _ = std::thread::Builder::new().name("quall.camera.ajustes.janela".into()).spawn(move || {
        registro::prefixar_esta_thread(&prefixo);
        if let Err(e) = correr(fonte, com_previa) {
            registro::linha(format!("ajustes: !! a janela não abriu: {e}"));
        }
        ABERTA.store(0, Ordering::SeqCst);
    });
}

/// Há uma janela de ajustes aberta?
pub fn aberta() -> bool {
    ABERTA.load(Ordering::SeqCst) != 0
}

/// Fecha a janela, se houver, e espera até `prazo` (a janela principal e a tela R5 chamam ao fechar).
pub fn fechar_se_aberta(prazo: Duration) -> bool {
    let h = ABERTA.load(Ordering::SeqCst);
    if h == 0 {
        return true;
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(h as *mut core::ffi::c_void)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    let fim = Instant::now() + prazo;
    while ABERTA.load(Ordering::SeqCst) != 0 {
        if Instant::now() >= fim {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

struct Janela {
    fonte: Fonte,
    estado: EstadoDosAjustes,
    quadro: QuadroDosAjustes,
    controles: HashMap<ControleDosAjustes, HWND>,
    textos: HashMap<ControleDosAjustes, String>,
    versao: Option<u64>,
    /// A `idioma::versao()` dos rótulos postos: quando ela muda, [`Janela::trocar_idioma`].
    idioma: u32,
    pintor: Option<estilo::d2d::Pintor>,
    dpi: u32,
    falha_dita: bool,
    pincel_fundo: HBRUSH,
    previa: Option<(HWND, Arc<ControleDaPrevia>)>,
    previa_pendurada: bool,
    /// O deslizante que a pessoa está arrastando: a posição dele não é reescrita pelo estado.
    arrastando: Option<ControleDosAjustes>,
    fechando: bool,
}

/// O título: "Ajustes da câmera", e no remoto o nome de quem filma.
fn titulo_de(fonte: &Fonte) -> String {
    match fonte {
        Fonte::Local(_) => crate::idioma::t(regras::TITULO_DA_JANELA).to_string(),
        Fonte::Remota(c) => crate::idioma::tf("Ajustes da câmera — {}", &[&c.par]),
    }
}

fn correr(fonte: Fonte, com_previa: bool) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let instancia = GetModuleHandleW(None)?;
        let classe = w!("QuallAjustesDaCamera");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instancia.into(),
            lpszClassName: classe,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let titulo = largo(&titulo_de(&fonte));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            classe,
            PCWSTR(titulo.as_ptr()),
            ESTILO,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            600,
            500,
            None,
            None,
            Some(instancia.into()),
            None,
        )?;
        let dpi = GetDpiForWindow(hwnd).max(96);
        let estado = match &fonte {
            Fonte::Local(_) => EstadoDosAjustes { aba: Aba::Exposicao, com_previa, permitir: Some(crate::camera_remota::permite()), ..Default::default() },
            Fonte::Remota(c) => EstadoDosAjustes { aba: Aba::Exposicao, remoto: Some(EstadoRemoto::de_json(&c.estado_json())), ..Default::default() },
        };
        let (l, a) = modelo::tamanho(&estado);
        let mut r = RECT { left: 0, top: 0, right: escala(dpi, l), bottom: escala(dpi, a) };
        let _ = AdjustWindowRectExForDpi(&mut r, ESTILO, false, WINDOW_EX_STYLE(0), dpi);
        let _ = SetWindowPos(hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        crate::janela::escurecer_a_barra_de_titulo(hwnd);
        let mut controles = HashMap::new();
        for c in ControleDosAjustes::todos() {
            match criar_controle(hwnd, c) {
                Ok(h) => {
                    controles.insert(c, h);
                }
                Err(e) => registro::linha(format!("ajustes: !! o controle {c:?} não nasceu: {e}")),
            }
        }
        let previa = if com_previa {
            match criar_janela_da_previa(hwnd) {
                Ok(h) => Some((h, ControleDaPrevia::novo(true))),
                Err(e) => {
                    registro::linha(format!("ajustes: !! a janela da prévia não nasceu: {e}"));
                    None
                }
            }
        } else {
            None
        };
        let pintor = match estilo::d2d::Pintor::novo() {
            Ok(p) => Some(p),
            Err(e) => {
                registro::linha(format!("ajustes: !! o Direct2D não subiu ({e}); os botões ficam com a cara do sistema"));
                None
            }
        };
        let remota = matches!(fonte, Fonte::Remota(_));
        let janela = Box::new(Janela {
            fonte,
            estado,
            quadro: QuadroDosAjustes::default(),
            controles,
            textos: HashMap::new(),
            versao: None,
            idioma: crate::idioma::versao(),
            pintor,
            dpi,
            falha_dita: false,
            pincel_fundo: CreateSolidBrush(COLORREF(estilo::FUNDO.colorref())),
            previa,
            previa_pendurada: false,
            arrastando: None,
            fechando: false,
        });
        let ponteiro = Box::into_raw(janela);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ponteiro as isize);
        if let Some(j) = janela_de(hwnd) {
            j.atualizar(hwnd);
        }
        // O remoto lê o estado do núcleo mais vezes: o recibo e o pendente chegam em décimos.
        let _ = SetTimer(Some(hwnd), ID_RELOGIO, if remota { 100 } else { 250 }, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        ABERTA.store(hwnd.0 as isize, Ordering::SeqCst);
        registro::linha(format!(
            "ajustes: a janela abriu ({})",
            if remota {
                "câmera remota, de quem filma"
            } else if com_previa {
                "câmera comum, com a prévia"
            } else {
                "tela R5, sem prévia"
            }
        ));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if IsWindow(Some(hwnd)).as_bool() && IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // A janela acabou: a `Janela` sai agora, fora de qualquer mensagem.
        let j = Box::from_raw(ponteiro);
        let _ = DeleteObject(j.pincel_fundo.into());
        drop(j);
        registro::linha("ajustes: a janela fechou");
    }
    Ok(())
}

unsafe fn criar_controle(pai: HWND, c: ControleDosAjustes) -> windows::core::Result<HWND> {
    let (classe, proprio) = match c.especie() {
        EspecieDosAjustes::Botao => (w!("BUTTON"), BS_PUSHBUTTON as u32),
        EspecieDosAjustes::Alternar => (w!("BUTTON"), (BS_AUTOCHECKBOX | BS_PUSHLIKE) as u32),
        EspecieDosAjustes::Opcao => (w!("BUTTON"), (BS_AUTORADIOBUTTON | BS_PUSHLIKE) as u32),
        EspecieDosAjustes::Deslizante => (w!("msctls_trackbar32"), TBS_NOTICKS),
    };
    let mut estilo = WS_CHILD.0 | WS_TABSTOP.0 | proprio;
    if c.abre_grupo() {
        estilo |= WS_GROUP.0;
    }
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            classe,
            PCWSTR::null(),
            WINDOW_STYLE(estilo),
            0,
            0,
            10,
            10,
            Some(pai),
            Some(HMENU(c.id() as *mut core::ffi::c_void)),
            None,
            None,
        )
    }
}

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
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn criar_janela_da_previa(pai: HWND) -> windows::core::Result<HWND> {
    unsafe {
        let instancia = GetModuleHandleW(None)?;
        let classe = w!("QuallPreviaDosAjustes");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc_da_previa),
            hInstance: instancia.into(),
            lpszClassName: classe,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        CreateWindowExW(WINDOW_EX_STYLE(0), classe, PCWSTR::null(), WS_CHILD | WS_CLIPSIBLINGS, 0, 0, 10, 10, Some(pai), None, Some(instancia.into()), None)
    }
}

fn janela_de(hwnd: HWND) -> Option<&'static mut Janela> {
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Janela;
        p.as_mut()
    }
}

/// Roda `f` numa thread de ajuda **bombeando as mensagens desta** até ela acabar, ou até `prazo`
/// (ver o cabeçalho: o cadeado da prévia do dono pode estar no `Present`).
fn esperar_bombeando(prazo: Duration, f: impl FnOnce() + Send + 'static) -> bool {
    let h = match std::thread::Builder::new().name("quall.camera.ajustes.previa".into()).spawn(f) {
        Ok(h) => h,
        Err(e) => {
            registro::linha(format!("ajustes: !! a thread da prévia não subiu ({e})"));
            return false;
        }
    };
    let fim = Instant::now() + prazo;
    EM_ESPERA.store(true, Ordering::SeqCst);
    while !h.is_finished() {
        if Instant::now() >= fim {
            EM_ESPERA.store(false, Ordering::SeqCst);
            registro::linha("ajustes: !! a prévia não se pendurou/soltou no prazo; segue sem esperar");
            return false;
        }
        unsafe {
            let _ = MsgWaitForMultipleObjectsEx(None, 10, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_TIMER {
                    // O relógio desta janela, agora, reentraria no meio do gesto.
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    EM_ESPERA.store(false, Ordering::SeqCst);
    let _ = h.join();
    true
}

impl Janela {
    fn h(&self, c: ControleDosAjustes) -> Option<HWND> {
        self.controles.get(&c).copied()
    }

    /// **O relógio, no remoto**: o estado do núcleo (o aplicado com o pendente por cima), e o fim
    /// com a sessão de recepção.
    fn atualizar_remota(&mut self, hwnd: HWND, c: &Arc<ControleRemoto>) {
        if !c.viva() {
            registro::linha("ajustes: a sessão de recepção acabou; a janela remota fecha junto");
            self.fechar(hwnd);
            return;
        }
        let novo = EstadoRemoto::de_json(&c.estado_json());
        if self.estado.remoto.as_ref() != Some(&novo) {
            // O deslizante arrastado não volta: o estado de agora já traz o pendente dele.
            self.estado.remoto = Some(novo);
            self.aplicar(hwnd);
        }
        if crate::idioma::versao() != self.idioma {
            self.trocar_idioma(hwnd);
        }
    }

    /// O relógio: o painel novo, a prévia que se pendura quando a câmera abre, e o fim com a câmera.
    fn atualizar(&mut self, hwnd: HWND) {
        if self.fechando {
            return;
        }
        let dono = match &self.fonte {
            Fonte::Local(d) => Arc::clone(d),
            Fonte::Remota(c) => {
                let c = Arc::clone(c);
                self.atualizar_remota(hwnd, &c);
                return;
            }
        };
        let fase = dono.fase();
        if matches!(fase, FaseDoDono::Acabou(_) | FaseDoDono::Falhou(_) | FaseDoDono::Fechado) {
            registro::linha(format!("ajustes: a câmera fechou ({fase:?}); a janela fecha junto"));
            self.fechar(hwnd);
            return;
        }
        if fase == FaseDoDono::Aberto && !self.previa_pendurada {
            if let Some((h, controle)) = self.previa.clone() {
                if dono.previa_pendurada() {
                    registro::linha("ajustes: a vaga da prévia do dono está ocupada; a janela segue sem prévia");
                    self.previa_pendurada = true;
                } else if let Some(info) = dono.info() {
                    let r = em_pixels(self.dpi, estilo::lugar::ajustes::PREVIA);
                    unsafe {
                        let _ = MoveWindow(h, r.left, r.top, r.right - r.left, r.bottom - r.top, true);
                        let _ = ShowWindow(h, SW_SHOW);
                    }
                    controle.definir_tamanho((r.right - r.left) as u32, (r.bottom - r.top) as u32);
                    let p = PreviaDaCamera::nova(h, Arc::clone(&controle), info.faixa_completa, info.matriz_709);
                    let dono = Arc::clone(&dono);
                    esperar_bombeando(Duration::from_secs(3), move || dono.pendurar_previa(Some(Box::new(p))));
                    self.previa_pendurada = true;
                    registro::linha("ajustes: a prévia se pendurou no dono da câmera comum");
                }
            }
        }
        let ponta = dono.ajustes();
        let versao = ponta.as_ref().map(|p| p.versao());
        // A opção do remoto pode ter mudado por outra janela (a câmera comum e a R5 dividem a opção).
        let permite = Some(crate::camera_remota::permite());
        if self.estado.permitir != permite {
            self.estado.permitir = permite;
            self.versao = None;
        }
        if versao != self.versao || self.versao.is_none() {
            self.versao = versao;
            if let Some(p) = &ponta {
                let mut painel = p.painel();
                // O gesto local que a thread ainda não publicou não pisca de volta: o registro da
                // tela é o que ela pediu por último até a publicação seguinte.
                if self.arrastando.is_some() {
                    painel.registro = self.estado.painel.registro.clone();
                }
                self.estado.painel = painel;
            } else {
                self.estado.painel = Default::default();
            }
            self.aplicar(hwnd);
        }
        if crate::idioma::versao() != self.idioma {
            self.trocar_idioma(hwnd);
        }
    }

    /// **A troca de idioma na hora** (o seletor PT | EN da janela principal): o título, e todos os
    /// rótulos de novo. Os textos dos controles (os nomes para o Narrador) são reescritos pelo
    /// [`Janela::aplicar`], que compara com o último posto; os botões e o resto se pintam com o
    /// idioma de agora no redesenho que ele pede. A linha do alto e o aviso de divergência vêm da
    /// thread dos ajustes, que os refaz na leitura seguinte (no máximo 250 ms).
    fn trocar_idioma(&mut self, hwnd: HWND) {
        self.idioma = crate::idioma::versao();
        let titulo = largo(&titulo_de(&self.fonte));
        unsafe {
            let _ = SetWindowTextW(hwnd, PCWSTR(titulo.as_ptr()));
        }
        self.textos.clear();
        self.aplicar(hwnd);
    }

    /// Põe o quadro nos controles e pede o redesenho.
    fn aplicar(&mut self, hwnd: HWND) {
        let q = modelo::compor(&self.estado);
        let dpi = self.dpi;
        for c in ControleDosAjustes::todos() {
            let Some(h) = self.h(c) else { continue };
            match q.achar(c) {
                Some(x) => {
                    let r = em_pixels(dpi, x.ret);
                    let texto = modelo::texto_acessivel(c, &self.estado);
                    unsafe {
                        let _ = SetWindowPos(h, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW);
                        let _ = EnableWindow(h, x.habilitado);
                        if self.textos.get(&c) != Some(&texto) {
                            let t = largo(&texto);
                            let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
                            self.textos.insert(c, texto);
                        }
                        match c.especie() {
                            EspecieDosAjustes::Opcao | EspecieDosAjustes::Alternar => {
                                SendMessageW(h, BM_SETCHECK, Some(WPARAM(if x.marcado { BST_CHECKED } else { BST_UNCHECKED })), None);
                            }
                            EspecieDosAjustes::Deslizante => {
                                if let (Some(d), false) = (x.degraus, self.arrastando == Some(c)) {
                                    SendMessageW(h, TBM_SETRANGEMIN, Some(WPARAM(0)), Some(LPARAM(0)));
                                    SendMessageW(h, TBM_SETRANGEMAX, Some(WPARAM(1)), Some(LPARAM(d.max.max(0) as isize)));
                                    SendMessageW(h, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(d.pos.clamp(0, d.max.max(0)) as isize)));
                                }
                            }
                            EspecieDosAjustes::Botao => {}
                        }
                    }
                }
                None => unsafe {
                    let _ = ShowWindow(h, SW_HIDE);
                },
            }
        }
        if let Some((h, controle)) = &self.previa {
            controle.visivel.store(self.previa_pendurada, Ordering::SeqCst);
            if !self.previa_pendurada {
                unsafe {
                    let _ = ShowWindow(*h, SW_HIDE);
                }
            }
        }
        self.quadro = q;
        unsafe {
            let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    /// Um gesto: a aba troca aqui; o resto vai à thread dos ajustes, e o registro da tela muda já.
    fn gesto(&mut self, hwnd: HWND, c: ControleDosAjustes, degrau: Option<i32>) {
        match modelo::gesto(c, &self.estado, degrau) {
            Some(Gesto::Aba(a)) => self.estado.aba = a,
            Some(Gesto::Acao(a)) => {
                if let Fonte::Local(dono) = &self.fonte {
                    if let Some(p) = dono.ajustes() {
                        p.pedir(a);
                    }
                }
                let painel = &mut self.estado.painel;
                regras::aplicar_acao(&mut painel.registro, a, &painel.caps.clone(), &painel.lidos.clone());
                // "O Manual leva à aba ISO e obturador" (§4.3).
                if a == Acao::Exposicao(ModoDeExposicao::Manual) && matches!(c, ControleDosAjustes::Exposicao(_) | ControleDosAjustes::PassarParaManual) {
                    self.estado.aba = Aba::GanhoEObturador;
                }
            }
            Some(Gesto::Permitir(v)) => {
                crate::camera_remota::definir_permissao(v);
                self.estado.permitir = Some(v);
            }
            Some(Gesto::Remoto(g)) => {
                if let Fonte::Remota(controle) = &self.fonte {
                    match &g {
                        GestoRemoto::Pedido(v) => controle.pedir(&v.to_string()),
                        GestoRemoto::Restaurar => controle.restaurar(),
                        GestoRemoto::Aba(_) => {}
                    }
                    // O pendente já está no estado do núcleo: a tela o mostra sem esperar o relógio.
                    self.estado.remoto = Some(EstadoRemoto::de_json(&controle.estado_json()));
                    // "O Manual leva à aba ISO e obturador" (§4.3), também de longe.
                    if matches!(c, ControleDosAjustes::Exposicao(1) | ControleDosAjustes::PassarParaManual) {
                        self.estado.aba = Aba::GanhoEObturador;
                    }
                }
            }
            None => {}
        }
        self.aplicar(hwnd);
    }

    fn fechar(&mut self, hwnd: HWND) {
        if self.fechando {
            return;
        }
        self.fechando = true;
        unsafe {
            let _ = KillTimer(Some(hwnd), ID_RELOGIO);
        }
        if let Fonte::Local(dono) = &self.fonte {
            if self.previa_pendurada && self.previa.is_some() && dono.previa_pendurada() {
                let dono = Arc::clone(dono);
                esperar_bombeando(Duration::from_secs(3), move || dono.pendurar_previa(None));
                registro::linha("ajustes: a prévia se soltou do dono");
            }
        }
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    fn pintar(&mut self, hwnd: HWND, hdc: HDC) {
        let mut cliente = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut cliente);
        }
        let dpi = self.dpi;
        let Some(p) = self.pintor.as_mut() else {
            unsafe {
                FillRect(hdc, &cliente, self.pincel_fundo);
            }
            return;
        };
        if let Err(e) = p.desenhar(hdc, cliente, dpi, Some(estilo::FUNDO), &self.quadro.itens, 1.0) {
            if !self.falha_dita {
                self.falha_dita = true;
                registro::linha(format!("ajustes: !! o desenho falhou: {e}"));
            }
        }
    }

    fn desenhar_controle(&mut self, c: ControleDosAjustes, cd: &NMCUSTOMDRAW) {
        let s = cd.uItemState;
        let tem = |f: windows::Win32::UI::Controls::NMCUSTOMDRAW_DRAW_STATE_FLAGS| (s.0 & f.0) != 0;
        let foco_escondido = (unsafe { SendMessageW(cd.hdr.hwndFrom, WM_QUERYUISTATE, None, None) }.0 as u32 & UISF_HIDEFOCUS) != 0;
        let estado = EstadoDoControle {
            apertado: tem(CDIS_SELECTED),
            foco: tem(CDIS_FOCUS) && !foco_escondido,
            desligado: tem(CDIS_DISABLED),
            quente: tem(CDIS_HOT),
            marcado: unsafe { SendMessageW(cd.hdr.hwndFrom, BM_GETCHECK, None, None) }.0 as usize == BST_CHECKED,
        };
        let k = 96.0 / self.dpi as f32;
        let (l, a) = ((cd.rc.right - cd.rc.left) as f32 * k, (cd.rc.bottom - cd.rc.top) as f32 * k);
        let rotulo = modelo::rotulo(c, &self.estado);
        let ap: Aparencia = modelo::aparencia(c, &rotulo, l, a, estado);
        let dpi = self.dpi;
        if let Some(p) = self.pintor.as_mut() {
            if let Err(e) = p.desenhar(cd.hdc, cd.rc, dpi, Some(ap.fundo), &ap.itens, ap.opacidade) {
                if !self.falha_dita {
                    self.falha_dita = true;
                    registro::linha(format!("ajustes: !! o desenho de um controle falhou: {e}"));
                }
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if EM_ESPERA.load(Ordering::SeqCst) {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let Some(j) = janela_de(hwnd) else {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    match msg {
        WM_TIMER => {
            j.atualizar(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wp.0 & 0xFFFF;
            let aviso = ((wp.0 >> 16) & 0xFFFF) as u32;
            // O Esc do `IsDialogMessageW` (`IDCANCEL`) fecha a janela.
            if id == 2 {
                j.fechar(hwnd);
                return LRESULT(0);
            }
            if aviso == BN_CLICKED {
                if let Some(c) = ControleDosAjustes::de_id(id) {
                    j.gesto(hwnd, c, None);
                }
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            let h = HWND(lp.0 as *mut core::ffi::c_void);
            let codigo = (wp.0 & 0xFFFF) as u32;
            let id = unsafe { GetDlgCtrlID(h) } as usize;
            if let Some(c) = ControleDosAjustes::de_id(id) {
                let pos = unsafe { SendMessageW(h, TBM_GETPOS, None, None) }.0 as i32;
                j.arrastando = if codigo == TB_THUMBTRACK { Some(c) } else { None };
                if codigo != TB_ENDTRACK {
                    j.gesto(hwnd, c, Some(pos));
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = unsafe { &*(lp.0 as *const NMHDR) };
            if hdr.code == NM_CUSTOMDRAW && j.pintor.is_some() {
                if let Some(c) = ControleDosAjustes::de_id(hdr.idFrom) {
                    if c.especie() != EspecieDosAjustes::Deslizante {
                        let cd = unsafe { &*(lp.0 as *const NMCUSTOMDRAW) };
                        if cd.dwDrawStage == CDDS_PREERASE || cd.dwDrawStage == CDDS_PREPAINT {
                            j.desenhar_controle(c, cd);
                            return LRESULT(CDRF_SKIPDEFAULT as isize);
                        }
                        return LRESULT(CDRF_DODEFAULT as isize);
                    }
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // O fundo dos deslizantes (o trackbar pede pelo `WM_CTLCOLORSTATIC`) e dos botões.
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            let dc = HDC(wp.0 as *mut core::ffi::c_void);
            SetTextColor(dc, COLORREF(estilo::TEXTO.colorref()));
            SetBkColor(dc, COLORREF(estilo::FUNDO.colorref()));
            LRESULT(j.pincel_fundo.0 as isize)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            j.pintar(hwnd, hdc);
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            j.fechar(hwnd);
            LRESULT(0)
        }
        // A `Janela` é solta por `correr`, depois do laço: aqui ela ainda pode estar emprestada
        // (o `fechar` chama o `DestroyWindow`).
        WM_DESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
