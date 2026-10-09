//! **A janela sintética nossa que cobre um monitor virtual inteiro** — só da bancada.
//!
//! # Por que existe
//!
//! Sem ela, o monitor virtual mostra o papel de parede da pessoa e o que ela arrastar para lá: é
//! material dela, e a regra da casa é que um monitor virtual **só é capturado coberto pela nossa
//! janela** (`docs/regras-de-frente.md`, "Um vídeo de bancada pode conter a vida do usuário"). É a
//! `carga` da sonda `receita_monitor` (`docs/monitor-virtual-windows.md` §11.5), portada para a
//! biblioteca porque quem a abre é o dono da topologia, entre o monitor ficar pronto e a captura
//! abrir.
//!
//! # A regra: ou está no monitor virtual, ou está escondida, ou não existe
//!
//! As quatro camadas da sonda, com uma diferença que o §13.5 pediu, e o portão:
//!
//! 1. nasce **escondida**, e só aparece depois de conferida e desenhada;
//! 2. o procedimento **veta** todo movimento que não seja dela (`WM_WINDOWPOSCHANGING`): quando um
//!    monitor sai, o Windows muda as janelas dele para outro — esta fica onde está, fora da tela;
//! 3. **antes de cada quadro** confere o monitor. **A diferença**: ela segue o **alvo** do monitor
//!    (`AdapterLuid` + `TargetId`), e não o nome GDI, que muda quando outro monitor chega (E5). Se o
//!    monitor do alvo andou, ela se esconde, vai atrás e volta; se o alvo ficou sem caminho ativo
//!    (outro monitor chegando), se ela não conseguiu ir atrás, ou se o retângulo do monitor cruza o
//!    de outro (o Windows arrumando os monitores no meio de uma `SetDisplayConfig`), ela **se
//!    esconde** e tenta de novo até [`GRACA`] sem interrupção; passado isso, ou se o monitor mudou
//!    de tamanho, ela é **destruída**. Na sonda, perder o nome GDI destruía a carga na hora — e era o
//!    que impedia medir o buraco na captura do 1º quando o 2º chegava (E5). Na N = 8 de 15/09, a
//!    primeira versão desta janela ainda desistia na primeira ida atrás que falhava, e num
//!    retângulo cruzado por um instante: quatro sessões caíram na chegada do 8º monitor;
//! 4. se o fio não responder no prazo, o processo encerra: a janela morre com ele, e o vigia do
//!    SudoVDA tira os monitores 2–3 s depois.
//!
//! **O portão** ([`PortaoDeCobertura`]): aberto só depois de ela aparecer e conferir, fechado
//! **antes** de ela se esconder. A captura do monitor descarta na chegada todo quadro que o portão
//! não deixa passar: o que o monitor mostrou enquanto ela ia atrás dele (o papel de parede) nunca
//! chega ao encoder. Uma janela escondida não aparece em monitor nenhum, e um quadro sem ela não
//! passa — as duas metades da regra.
//!
//! # Uma por monitor, então nada é estático
//!
//! A sonda tinha uma carga por processo e guardava os sinais do procedimento em estáticas. Aqui há
//! até oito, cada uma no seu fio: os sinais moram numa caixa que a janela carrega em `GWLP_USERDATA`.
//!
//! Desenha com `ClearView` (listras que andam, nada de pixel de fora), `WS_EX_TOPMOST |
//! WS_EX_NOACTIVATE`, swap chain *flip* com `Present(1)`.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext1, ID3D11RenderTargetView, ID3D11Texture2D, ID3D11View,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetWindowLongPtrW, GetWindowRect, PeekMessageW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    CREATESTRUCTW, GWLP_USERDATA, HWND_TOPMOST, MA_NOACTIVATE, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_HIDE, SW_SHOWNOACTIVATE, WINDOWPOS, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCCREATE,
    WM_PAINT, WM_QUIT, WM_WINDOWPOSCHANGING, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::capture::PortaoDeCobertura;
use crate::device;
use crate::registro;

/// Quanto tempo a janela espera, escondida e sem interrupção, o alvo voltar a ser cobrível (caminho
/// ativo, a janela no lugar dele, nenhum retângulo cruzado) antes de se destruir. A `SetDisplayConfig`
/// do monitor que chega leva até ~1 s (§13.3); o nome GDI do 1º sumiu por 541 ms no E5.
pub const GRACA: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modo {
    /// Desenha uma vez e fica parada.
    Parada,
    /// Redesenha a cada quadro (um `Present` com espera de retraço por volta) — o caso da rolagem.
    Camadas,
    /// A tela inteira numa cor que muda a cada quadro, com um `ClearView` só: o mesmo conteúdo da
    /// origem sintética `cor` do controle, para a diferença entre as duas corridas ser a captura (o
    /// DWM compondo o monitor e o WGC copiando), e não o desenho — as ~120 `ClearView` por quadro
    /// das `camadas` pesam na Intel, que também compõe e codifica (N = 8, 15/09).
    Cor,
}

impl std::str::FromStr for Modo {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "parada" => Ok(Modo::Parada),
            "camadas" => Ok(Modo::Camadas),
            "cor" => Ok(Modo::Cor),
            _ => Err(format!("cobertura \"{s}\": use camadas, cor ou parada")),
        }
    }
}

impl Modo {
    pub fn nome(self) -> &'static str {
        match self {
            Modo::Parada => "parada",
            Modo::Camadas => "camadas",
            Modo::Cor => "cor",
        }
    }
}

/// O que a janela fez na vida dela.
#[derive(Clone, Debug, Default)]
pub struct Relato {
    pub quadros: u64,
    pub falhas_de_present: u64,
    /// Quantas vezes o monitor andou (ou mudou de nome) e ela foi atrás.
    pub mudancas_de_lugar: u32,
    /// Quantas vezes o alvo ficou sem caminho ativo e ela se escondeu, e o maior desses trechos.
    pub escondidas: u32,
    pub maior_escondida_ms: u64,
    /// Quantas idas atrás do monitor falharam (ela ficou escondida e tentou na volta seguinte).
    pub idas_que_falharam: u32,
    /// Quantas voltas ela se viu por baixo de outra janela (e voltou ao topo).
    pub vezes_por_baixo: u32,
    /// O que o portão descartou, motivo a motivo (preenchido no fim, de fora do fio).
    pub portao: crate::capture::DescartesDoPortao,
    /// Por que acabou antes de pedirem, se acabou.
    pub acabou_sozinha: Option<String>,
}

impl Relato {
    pub fn linha(&self) -> String {
        format!(
            "cobertura: quadros={} falhas_de_present={} mudancas_de_lugar={} idas_que_falharam={} escondidas={} (maior {} ms) por_baixo={} acabou_sozinha={} | {}",
            self.quadros,
            self.falhas_de_present,
            self.mudancas_de_lugar,
            self.idas_que_falharam,
            self.escondidas,
            self.maior_escondida_ms,
            self.vezes_por_baixo,
            self.acabou_sozinha.as_deref().map_or("nao".to_string(), |m| format!("\"{m}\"")),
            self.portao.linha()
        )
    }
}

/// O que a janela diz agora a quem precisa saber (a sessão, antes de reabrir a captura).
#[derive(Clone, Debug, Default)]
struct Publico {
    /// O monitor (nome GDI e retângulo) que ela está cobrindo **agora**, visível e conferida.
    cobrindo: Option<(String, RECT)>,
    acabou: Option<String>,
}

/// Os sinais do procedimento da janela, por janela.
struct Sinais {
    pediu_fechar: AtomicBool,
    mover_permitido: AtomicBool,
}

pub struct Cobertura {
    fio: Option<JoinHandle<Relato>>,
    parar: Arc<AtomicBool>,
    hwnd: Arc<AtomicIsize>,
    publico: Arc<Mutex<Publico>>,
    /// Aberto só com a janela visível e conferida sobre o monitor; a captura descarta o resto.
    portao: Arc<PortaoDeCobertura>,
}

fn rect_igual(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

fn rect_cruza(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

unsafe extern "system" fn procedimento(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lp.0 as *const CREATESTRUCTW;
        if !cs.is_null() {
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize) };
        }
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let sinais = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Sinais;
    let sinais = if sinais.is_null() { None } else { Some(unsafe { &*sinais }) };
    match msg {
        // O fechar só marca: quem destrói é o laço, com a swap chain já solta.
        WM_CLOSE => {
            if let Some(s) = sinais {
                s.pediu_fechar.store(true, Ordering::SeqCst);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // **O veto**: ninguém além da própria janela a move ou redimensiona. Esconder e mostrar
        // continuam passando (não são movimento).
        WM_WINDOWPOSCHANGING => {
            if let Some(s) = sinais {
                if !s.mover_permitido.load(Ordering::SeqCst) {
                    let pos = lp.0 as *mut WINDOWPOS;
                    if !pos.is_null() {
                        unsafe { (*pos).flags |= SWP_NOMOVE | SWP_NOSIZE };
                    }
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // A pessoa no Dell pode clicar no monitor virtual; a janela não rouba o foco.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Encerra o processo porque uma janela não responde: ela morre com ele, o fio de ping também, e o
/// vigia do SudoVDA tira os monitores 2–3 s depois. O último recurso da regra.
fn encerrar_por_seguranca(motivo: &str) -> ! {
    registro::linha(format!(
        "cobertura: {motivo}; encerro o processo por segurança — a janela morre com ele, e o vigia do SudoVDA tira os monitores em 2–3 s"
    ));
    unsafe {
        let _ = windows::Win32::System::Threading::TerminateProcess(windows::Win32::System::Threading::GetCurrentProcess(), 98);
    }
    std::process::exit(98);
}

fn esperar_o_fio(fio: &JoinHandle<Relato>, prazo: Duration) -> bool {
    let t = Instant::now();
    while !fio.is_finished() {
        if t.elapsed() >= prazo {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

impl Cobertura {
    /// Abre a janela sobre o monitor do `alvo` (LUID do adaptador + `TargetId`). Volta depois de ela
    /// existir, **estar no monitor certo** e ter o primeiro quadro; se ela cair noutro, é destruída
    /// sem aparecer.
    pub fn iniciar(modo: Modo, alvo: (u64, u32), placa: u64) -> Result<(Cobertura, String), String> {
        let parar = Arc::new(AtomicBool::new(false));
        let hwnd = Arc::new(AtomicIsize::new(0));
        let publico = Arc::new(Mutex::new(Publico::default()));
        let portao = Arc::new(PortaoDeCobertura::default());
        let (tx, rx) = mpsc::channel::<Result<String, String>>();
        let (p2, h2, pub2, por2) = (parar.clone(), hwnd.clone(), publico.clone(), portao.clone());
        let fio = std::thread::Builder::new()
            .name(format!("quall.cobertura.{}", alvo.1))
            .spawn(move || fio(modo, alvo, placa, p2, h2, pub2, por2, tx))
            .map_err(|e| format!("cobertura: não consegui criar o fio: {e}"))?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(linha)) => Ok((Cobertura { fio: Some(fio), parar, hwnd, publico, portao }, linha)),
            Ok(Err(e)) => {
                let _ = fio.join();
                Err(e)
            }
            Err(_) => {
                let mut c = Cobertura { fio: Some(fio), parar, hwnd, publico, portao };
                c.pedir_fim();
                let saiu = c.fio.as_ref().is_some_and(|f| esperar_o_fio(f, Duration::from_secs(3)));
                if !saiu {
                    encerrar_por_seguranca("a janela não ficou pronta em 5 s e o fio não saiu em mais 3");
                }
                if let Some(f) = c.fio.take() {
                    let _ = f.join();
                }
                Err("cobertura: a janela não ficou pronta em 5 s (o fio saiu sem ela)".into())
            }
        }
    }

    /// O portão que a captura do monitor consulta a cada quadro.
    pub fn portao(&self) -> Arc<PortaoDeCobertura> {
        self.portao.clone()
    }

    /// O monitor que ela cobre agora, visível e conferido: `(nome GDI, retângulo)`.
    pub fn cobrindo(&self) -> Option<(String, RECT)> {
        self.publico.lock().ok().and_then(|p| p.cobrindo.clone())
    }

    /// Por que ela acabou sozinha, se acabou.
    pub fn acabou(&self) -> Option<String> {
        self.publico.lock().ok().and_then(|p| p.acabou.clone())
    }

    /// Espera até `prazo` ela estar cobrindo o monitor `gdi` inteiro. `false` se não chegou lá.
    pub fn esperar_cobrir(&self, gdi: &str, prazo: Duration) -> bool {
        let t = Instant::now();
        loop {
            if self.cobrindo().is_some_and(|(g, _)| g.eq_ignore_ascii_case(gdi)) {
                return true;
            }
            if self.acabou().is_some() || t.elapsed() >= prazo {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pedir_fim(&self) {
        self.portao.fechar();
        self.parar.store(true, Ordering::SeqCst);
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Fecha a janela e devolve o relato. Chamado **antes** de soltar o monitor. Se o fio não sair em
    /// 5 s, o processo encerra (a regra da janela vale mais que a medida).
    pub fn parar(mut self) -> Relato {
        self.pedir_fim();
        let Some(f) = self.fio.take() else { return Relato::default() };
        if !esperar_o_fio(&f, Duration::from_secs(5)) {
            encerrar_por_seguranca("o fio da cobertura não saiu em 5 s depois do pedido");
        }
        let mut r = f.join().unwrap_or_default();
        r.portao = self.portao.descartes();
        r
    }
}

impl Drop for Cobertura {
    fn drop(&mut self) {
        if let Some(f) = self.fio.take() {
            self.pedir_fim();
            if !esperar_o_fio(&f, Duration::from_secs(5)) {
                encerrar_por_seguranca("o fio da cobertura não saiu em 5 s na saída");
            }
            let _ = f.join();
        }
    }
}

// --- os monitores do GDI ------------------------------------------------------------------------

fn info(h: HMONITOR) -> Option<MONITORINFOEXW> {
    let mut i = MONITORINFOEXW::default();
    i.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    unsafe { GetMonitorInfoW(h, &mut i as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool().then_some(i)
}

fn de_utf16(bruto: &[u16]) -> String {
    let fim = bruto.iter().position(|c| *c == 0).unwrap_or(bruto.len());
    String::from_utf16_lossy(&bruto[..fim])
}

unsafe extern "system" fn junta(h: HMONITOR, _hdc: HDC, _r: *mut RECT, dados: LPARAM) -> BOOL {
    let lista = unsafe { &mut *(dados.0 as *mut Vec<(String, RECT)>) };
    if let Some(i) = info(h) {
        lista.push((de_utf16(&i.szDevice), i.monitorInfo.rcMonitor));
    }
    BOOL(1)
}

/// Os monitores do GDI: `(nome, retângulo)` em pixels físicos (o processo é *per monitor v2*).
fn monitores_do_gdi() -> Vec<(String, RECT)> {
    let mut v: Vec<(String, RECT)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(junta), LPARAM(&mut v as *mut Vec<(String, RECT)> as isize));
    }
    v
}

fn monitor_da_janela(hwnd: HWND) -> Option<String> {
    let h = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
    if h.is_invalid() {
        return None;
    }
    info(h).map(|i| de_utf16(&i.szDevice))
}

fn retangulo_da_janela(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    Some(r)
}

enum Conferencia {
    /// A janela ocupa exatamente o monitor do alvo.
    Certa(String, RECT),
    /// O monitor do alvo está noutro lugar (ou com outro nome), do mesmo tamanho: ir atrás.
    Andou(String, RECT),
    /// O alvo não tem caminho ativo agora: esconder, e destruir se passar da [`GRACA`].
    Sumiu(String),
    /// O retângulo do monitor cruza o de outro — o Windows arrumando os monitores no meio de uma
    /// `SetDisplayConfig` (N = 8, 15/09), ou o clone: esconder, e destruir se passar da [`GRACA`].
    Cruza(String),
    /// Não deu para ler o `DisplayConfig` (a tela bloqueada devolve `ERROR_ACCESS_DENIED`):
    /// esconder, mas **não** contar para a destruição — erro de leitura não é o monitor saindo.
    Ilegivel(String),
    /// Mudou de tamanho: destruir (a captura só troca com o tamanho igual).
    Errada(String),
}

/// Onde o monitor do alvo está: o nome GDI pelo `DisplayConfig` (o par, não o nome, é a identidade;
/// e só um caminho com a **fonte no adaptador virtual** — o clone pela Intel não serve, §13.1).
fn gdi_do_alvo(alvo: (u64, u32)) -> Result<Option<String>, String> {
    crate::monitores_virtuais::ccd::gdi_do_alvo(alvo.0, alvo.1)
}

/// A conferência de cada quadro. O nome GDI do alvo vem do cache (`conhecido`); o `DisplayConfig`
/// só é relido quando algo não bate, ou a cada 250 ms — ler o CCD a cada quadro, com oito janelas,
/// disputaria a trava dele com a `SetDisplayConfig` de quem está chegando.
fn conferir(hwnd: HWND, alvo: (u64, u32), w: i32, h: i32, conhecido: &mut Option<(String, Instant)>) -> Conferencia {
    let medir = |gdi: &str| -> Option<Conferencia> {
        let todos = monitores_do_gdi();
        let r = todos.iter().find(|(n, _)| n.eq_ignore_ascii_case(gdi)).map(|(_, r)| *r)?;
        if r.right - r.left != w || r.bottom - r.top != h {
            return Some(Conferencia::Errada(format!(
                "o monitor mudou de tamanho ({}x{} → {}x{})",
                w,
                h,
                r.right - r.left,
                r.bottom - r.top
            )));
        }
        if let Some((outro, _)) = todos.iter().find(|(n, o)| !n.eq_ignore_ascii_case(gdi) && rect_cruza(o, &r)) {
            return Some(Conferencia::Cruza(format!("o retângulo de {gdi} cruza o de {outro}")));
        }
        let janela = retangulo_da_janela(hwnd)?;
        if !rect_igual(&janela, &r) {
            return Some(Conferencia::Andou(gdi.to_string(), r));
        }
        if monitor_da_janela(hwnd).is_some_and(|g| g.eq_ignore_ascii_case(gdi)) {
            Some(Conferencia::Certa(gdi.to_string(), r))
        } else {
            None
        }
    };
    // O caminho barato: o nome conhecido, relido do CCD há menos de 250 ms, e tudo batendo.
    if let Some((gdi, quando)) = conhecido.as_ref() {
        if quando.elapsed() < Duration::from_millis(250) {
            if let Some(c @ Conferencia::Certa(..)) = medir(gdi) {
                return c;
            }
        }
    }
    // O caminho do CCD: quem é o monitor do alvo agora.
    match gdi_do_alvo(alvo) {
        Err(motivo) => {
            *conhecido = None;
            Conferencia::Ilegivel(motivo)
        }
        Ok(None) => {
            *conhecido = None;
            Conferencia::Sumiu("o alvo não tem caminho ativo com a fonte no adaptador virtual".into())
        }
        Ok(Some(gdi)) => {
            *conhecido = Some((gdi.clone(), Instant::now()));
            match medir(&gdi) {
                Some(c) => c,
                None => Conferencia::Sumiu(format!("{gdi} ainda não está na enumeração do GDI")),
            }
        }
    }
}

fn mover(hwnd: HWND, sinais: &Sinais, r: &RECT) -> bool {
    sinais.mover_permitido.store(true, Ordering::SeqCst);
    let ok = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE) }.is_ok();
    sinais.mover_permitido.store(false, Ordering::SeqCst);
    ok
}

/// O quadro `t` do modo: as listras das `camadas` (também o quadro único da `parada`), ou a cor.
fn pintar(modo: Modo, ctx: &ID3D11DeviceContext1, vista: &ID3D11View, w: i32, h: i32, t: u64) {
    match modo {
        Modo::Cor => unsafe { ctx.ClearView(vista, &cor((t % 90) as f32 / 90.0), None) },
        Modo::Camadas | Modo::Parada => desenhar(ctx, vista, w, h, t),
    }
}

/// As listras que andam: 24 faixas de cor deslocadas `t` quadros, e barras que sobem — o bastante
/// para a tela inteira mudar a cada quadro (a carga da sonda).
fn desenhar(ctx: &ID3D11DeviceContext1, vista: &ID3D11View, w: i32, h: i32, t: u64) {
    unsafe {
        ctx.ClearView(vista, &[0.95, 0.95, 0.95, 1.0], None);
        let alto = (h as f32 * 0.35) as i32;
        for i in 0..24i32 {
            let x = ((i as i64 * 80 + t as i64 * 8) % (w as i64 + 80)) as i32 - 80;
            let r = RECT { left: x.max(0), top: 0, right: (x + 36).min(w), bottom: alto };
            if r.right <= r.left {
                continue;
            }
            ctx.ClearView(vista, &cor(i as f32 / 24.0), Some(&[r]));
        }
        let passo = 16i32;
        let desloc = (t as i32 * 3) % passo;
        let mut y = alto + 8 - desloc;
        let mut n = 0i32;
        while y < h {
            let largura = (w / 3) + ((n * 37 + t as i32) % (w / 2).max(1));
            let r = RECT { left: 12, top: y.max(alto), right: (12 + largura).min(w), bottom: (y + 10).min(h) };
            if r.bottom > r.top && r.right > r.left {
                ctx.ClearView(vista, &[0.1, 0.1, 0.1, 1.0], Some(&[r]));
            }
            y += passo;
            n += 1;
        }
    }
}

fn cor(matiz: f32) -> [f32; 4] {
    let (s, v) = (0.6f32, 0.95f32);
    let h6 = matiz * 6.0;
    let f = h6 - h6.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match h6 as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b, 1.0]
}

#[allow(clippy::too_many_arguments)]
fn fio(
    modo: Modo,
    alvo: (u64, u32),
    placa_luid: u64,
    parar: Arc<AtomicBool>,
    hwnd_fora: Arc<AtomicIsize>,
    publico: Arc<Mutex<Publico>>,
    portao: Arc<PortaoDeCobertura>,
    pronto: mpsc::Sender<Result<String, String>>,
) -> Relato {
    let mut relato = Relato::default();
    let gdi0 = match gdi_do_alvo(alvo) {
        Ok(Some(g)) => g,
        Ok(None) => {
            let _ = pronto.send(Err("cobertura: o alvo não tem caminho ativo".into()));
            return relato;
        }
        Err(e) => {
            let _ = pronto.send(Err(format!("cobertura: {e}")));
            return relato;
        }
    };
    let Some(r) = monitores_do_gdi().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(&gdi0)).map(|(_, r)| r) else {
        let _ = pronto.send(Err(format!("cobertura: {gdi0} não está na enumeração do GDI")));
        return relato;
    };
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    // A placa do monitor virtual (a do `SET_RENDER_ADAPTER`, pelo LUID): desenhar nela evita cópia
    // entre placas — e "a primeira Intel" do DXGI pode ser o próprio adaptador do SudoVDA.
    let placa = match device::create_device_por_luid(placa_luid) {
        Ok(p) => p,
        Err(e) => {
            let _ = pronto.send(Err(format!("cobertura: criar o dispositivo D3D11 falhou: {e}")));
            return relato;
        }
    };
    let sinais = Box::new(Sinais { pediu_fechar: AtomicBool::new(false), mover_permitido: AtomicBool::new(true) });
    let sinais_ptr: *const Sinais = &*sinais;
    // A janela nasce **sem** `WS_VISIBLE`: nada aparece até a conferência e o primeiro quadro.
    let criada = unsafe {
        match GetModuleHandleW(None) {
            Err(e) => Err(e),
            Ok(inst) => {
                let classe = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(procedimento),
                    hInstance: inst.into(),
                    lpszClassName: w!("QuallCoberturaDoMonitor"),
                    ..Default::default()
                };
                // Já registrada (outra cobertura no mesmo processo) não é erro que importe.
                let _ = RegisterClassExW(&classe);
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("QuallCoberturaDoMonitor"),
                    w!("Quall - cobertura sintetica do monitor virtual"),
                    WS_POPUP,
                    r.left,
                    r.top,
                    w,
                    h,
                    None,
                    None,
                    Some(inst.into()),
                    Some(sinais_ptr as *const std::ffi::c_void),
                )
            }
        }
    };
    sinais.mover_permitido.store(false, Ordering::SeqCst);
    let hwnd = match criada {
        Ok(h) => h,
        Err(e) => {
            let _ = pronto.send(Err(format!("cobertura: criar a janela falhou: {e}")));
            return relato;
        }
    };
    hwnd_fora.store(hwnd.0 as isize, Ordering::SeqCst);
    portao.ligar_janela(hwnd.0 as isize);
    let destruir_e_recusar = |motivo: String| {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        let _ = pronto.send(Err(format!("cobertura: {motivo}; destruída sem aparecer")));
    };
    let mut conhecido: Option<(String, Instant)> = Some((gdi0.clone(), Instant::now()));
    match conferir(hwnd, alvo, w, h, &mut conhecido) {
        Conferencia::Certa(..) => {}
        Conferencia::Andou(g, _) => {
            destruir_e_recusar(format!("a janela não nasceu no retângulo de {g}"));
            return relato;
        }
        Conferencia::Sumiu(e) | Conferencia::Cruza(e) | Conferencia::Errada(e) | Conferencia::Ilegivel(e) => {
            destruir_e_recusar(e);
            return relato;
        }
    }

    let montar = || -> windows::core::Result<(IDXGISwapChain1, ID3D11DeviceContext1, ID3D11View)> {
        let dispositivo: &ID3D11Device = &placa.device;
        let fabrica: IDXGIFactory2 = unsafe { dispositivo.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()? };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w as u32,
            Height: h as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let cadeia = unsafe { fabrica.CreateSwapChainForHwnd(dispositivo, hwnd, &desc, None, None)? };
        let fundo: ID3D11Texture2D = unsafe { cadeia.GetBuffer(0)? };
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        unsafe { dispositivo.CreateRenderTargetView(&fundo, None, Some(&mut rtv))? };
        let vista: ID3D11View = rtv.expect("CreateRenderTargetView sem vista").cast()?;
        let ctx: ID3D11DeviceContext1 = placa.context.cast()?;
        Ok((cadeia, ctx, vista))
    };
    let (cadeia, ctx, vista) = match montar() {
        Ok(x) => x,
        Err(e) => {
            destruir_e_recusar(format!("swap chain: {e}"));
            return relato;
        }
    };
    // O primeiro quadro antes de aparecer; e a conferência de novo, logo antes do `ShowWindow`.
    pintar(modo, &ctx, &vista, w, h, 0);
    if unsafe { cadeia.Present(1, DXGI_PRESENT(0)) }.is_err() {
        relato.falhas_de_present += 1;
    }
    relato.quadros += 1;
    let (gdi, rect) = match conferir(hwnd, alvo, w, h, &mut conhecido) {
        Conferencia::Certa(g, r) => (g, r),
        _ => {
            drop(cadeia);
            destruir_e_recusar(format!("{gdi0} mudou entre montar e mostrar"));
            return relato;
        }
    };
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    portao.abrir_agora(rect);
    if let Ok(mut p) = publico.lock() {
        p.cobrindo = Some((gdi.clone(), rect));
    }
    let _ = pronto.send(Ok(format!(
        "cobertura: modo={} janela={}x{} em ({},{}) monitor={gdi} alvo={:X}:{} placa={} (0x{:04X})",
        modo.nome(),
        w,
        h,
        rect.left,
        rect.top,
        alvo.0 as u32,
        alvo.1,
        placa.description,
        placa.vendor_id
    )));

    // Esconder sempre na mesma ordem: o portão fecha **antes** de a janela sair da tela.
    let esconder = |publico: &Mutex<Publico>| {
        portao.fechar();
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        if let Ok(mut p) = publico.lock() {
            p.cobrindo = None;
        }
    };
    let mostrar = |publico: &Mutex<Publico>, g: String, r: RECT| {
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        portao.abrir_agora(r);
        if let Ok(mut p) = publico.lock() {
            p.cobrindo = Some((g, r));
        }
    };

    let mut cadeia = Some(cadeia);
    let mut t = 1u64;
    let mut destruida = false;
    let mut escondida_desde: Option<Instant> = None;
    // Desde quando o alvo está sem caminho ativo **lido** (a graça conta daqui, não de um erro de leitura).
    let mut sumiu_desde: Option<Instant> = None;
    loop {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                return relato;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if destruida {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let pedido = parar.load(Ordering::SeqCst) || sinais.pediu_fechar.load(Ordering::SeqCst);
        let mut acabar: Option<String> = None;
        if !pedido {
            match conferir(hwnd, alvo, w, h, &mut conhecido) {
                Conferencia::Certa(g, r) => {
                    sumiu_desde = None;
                    if let Some(desde) = escondida_desde.take() {
                        // O alvo voltou no mesmo lugar: aparece de novo.
                        relato.maior_escondida_ms = relato.maior_escondida_ms.max(desde.elapsed().as_millis() as u64);
                        mostrar(&publico, g, r);
                    } else if let Ok(mut p) = publico.lock() {
                        if p.cobrindo.as_ref().is_none_or(|(g0, r0)| !g0.eq_ignore_ascii_case(&g) || !rect_igual(r0, &r)) {
                            p.cobrindo = Some((g, r));
                        }
                    }
                    // **Por cima de tudo** (a revisão de 15/09, item 3): uma janela da pessoa, a barra
                    // de tarefas da tela secundária ou o Win+Tab por cima da nossa, visto aqui ou pela
                    // captura: o portão marca a conferência ruim (a captura descarta até os nove
                    // pontos voltarem a ser dela) e a janela volta ao topo.
                    let por_baixo = !crate::capture::janela_por_cima(hwnd, &r);
                    let pediram = portao.pediram_o_topo();
                    if por_baixo || pediram {
                        if por_baixo {
                            relato.vezes_por_baixo += 1;
                            portao.marcar_ruim();
                        }
                        unsafe {
                            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                        }
                    }
                }
                Conferencia::Andou(g, novo) => {
                    // Esconde já (portão fechado antes), vai para o lugar novo, confere, e só então
                    // volta a aparecer.
                    esconder(&publico);
                    if mover(hwnd, &sinais, &novo) && matches!(conferir(hwnd, alvo, w, h, &mut conhecido), Conferencia::Certa(..)) {
                        sumiu_desde = None;
                        relato.mudancas_de_lugar += 1;
                        if let Some(desde) = escondida_desde.take() {
                            relato.maior_escondida_ms = relato.maior_escondida_ms.max(desde.elapsed().as_millis() as u64);
                        }
                        mostrar(&publico, g, novo);
                    } else {
                        // O monitor andou de novo enquanto ela ia (a arrumação ainda no meio):
                        // escondida, tenta na próxima volta; desiste só depois da graça.
                        if escondida_desde.is_none() {
                            escondida_desde = Some(Instant::now());
                            relato.escondidas += 1;
                        }
                        relato.idas_que_falharam += 1;
                        if sumiu_desde.get_or_insert_with(Instant::now).elapsed() >= GRACA {
                            acabar = Some(format!("o monitor andou e a janela não conseguiu ir atrás por {} ms", GRACA.as_millis()));
                        }
                    }
                }
                Conferencia::Cruza(motivo) => {
                    if escondida_desde.is_none() {
                        esconder(&publico);
                        escondida_desde = Some(Instant::now());
                        relato.escondidas += 1;
                    }
                    if sumiu_desde.get_or_insert_with(Instant::now).elapsed() >= GRACA {
                        acabar = Some(format!("{motivo} há mais de {} ms", GRACA.as_millis()));
                    }
                }
                Conferencia::Sumiu(motivo) => {
                    if escondida_desde.is_none() {
                        esconder(&publico);
                        escondida_desde = Some(Instant::now());
                        relato.escondidas += 1;
                    }
                    if sumiu_desde.get_or_insert_with(Instant::now).elapsed() >= GRACA {
                        acabar = Some(format!("{motivo} há mais de {} ms", GRACA.as_millis()));
                    }
                }
                Conferencia::Ilegivel(motivo) => {
                    // Escondida enquanto não der para ler; o relógio da graça não anda.
                    sumiu_desde = None;
                    if escondida_desde.is_none() {
                        esconder(&publico);
                        escondida_desde = Some(Instant::now());
                        relato.escondidas += 1;
                        registro::linha(format!("cobertura: não consigo ler o DisplayConfig ({motivo}); escondida até ler"));
                    }
                }
                Conferencia::Errada(e) => {
                    esconder(&publico);
                    acabar = Some(e);
                }
            }
        }
        if pedido || acabar.is_some() {
            portao.fechar();
            if let Some(motivo) = acabar {
                relato.acabou_sozinha = Some(motivo.clone());
                if let Ok(mut p) = publico.lock() {
                    p.acabou = Some(motivo);
                }
            }
            if let Ok(mut p) = publico.lock() {
                p.cobrindo = None;
            }
            // A swap chain sai antes da janela.
            cadeia = None;
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            destruida = true;
            continue;
        }
        if escondida_desde.is_some() {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let Some(c) = cadeia.as_ref() else { continue };
        match modo {
            Modo::Camadas | Modo::Cor => {
                pintar(modo, &ctx, &vista, w, h, t);
                if unsafe { c.Present(1, DXGI_PRESENT(0)) }.is_err() {
                    relato.falhas_de_present += 1;
                }
                t += 1;
                relato.quadros += 1;
            }
            Modo::Parada => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}
