//! Captura de tela por Windows.Graphics.Capture (WGC).
//!
//! Preferida no lugar de Desktop Duplication (IDXGIOutputDuplication) por três razões medidas
//! contra o hardware da bancada, não só teoria:
//!
//! 1. O Dell G3 é um notebook de gráficos híbridos (NVIDIA GTX 1660 Ti + Intel UHD 630, Optimus).
//!    Desktop Duplication amarra a captura ao adaptador que está compondo o desktop (normalmente
//!    o Intel integrado) e historicamente tem bordas ásperas em setups híbridos — perde quadro,
//!    ou falha com `DXGI_ERROR_UNSUPPORTED` quando o adaptador muda. WGC é a API que a própria
//!    Microsoft recomenda para Windows 10 1903+/11 justamente para não amarrar a captura a um
//!    adaptador físico: ela devolve uma textura D3D11 já no dispositivo que a gente escolhe,
//!    independente de qual GPU compõe a tela.
//! 2. WGC entrega a textura já como `ID3D11Texture2D`, então a gente consegue empurrar direto pro
//!    encoder de hardware via `IMFDXGIDeviceManager`, sem baixar pixel pra CPU. Isso é o que
//!    sustenta a meta de latência (< 150 ms, perseguindo < 50 ms) — Desktop Duplication também
//!    devolve textura, então empataria aqui, mas perde nos outros dois pontos.
//! 3. WGC não precisa de elevação nem de sessão de console interativa da forma que Desktop
//!    Duplication historicamente exigiu, e lida melhor com HDR/mudança de modo de vídeo.
//!
//! Desvantagem aceita: WGC desenha um retângulo amarelo ao redor da janela/monitor capturado em
//! versões antigas do Windows 10 (removido a partir do 11) e tem ligeiramente mais latência de
//! entrega de quadro que Desktop Duplication em alguns benchmarks públicos — não medido aqui
//! isoladamente porque o número que importa pro contrato é o pipeline inteiro (captura + encode),
//! medido em `main.rs`.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, HMONITOR, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetWindowRect, IsWindowVisible, WindowFromPoint, GA_ROOT};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{IInspectable, Interface, Result};

/// Um quadro capturado, já em textura D3D11, pronto para virar entrada do encoder.
pub struct CapturedFrame {
    pub texture: ID3D11Texture2D,
    /// Instante de captura no relógio monotônico do processo — é o `t0` de todas as medidas de
    /// latência do contrato (número 1 do relatório).
    pub captured_at: Instant,
    /// **A posse da posição**, quando a textura é de um anel que **outro** escreve (o anel da rede do
    /// dono da captura do R5, `dono_da_captura.rs`): enquanto ela viver, o dono não reescreve a
    /// posição. `None` em toda origem que escreve o anel na mesma thread que o lê.
    pub posse: Option<Posse>,
}

/// A posse de uma posição de anel (ver [`CapturedFrame::posse`]): conta quem ainda segura a
/// posição; clonar conta mais um, largar conta menos um.
pub struct Posse {
    contagem: Arc<std::sync::atomic::AtomicU32>,
}

impl Posse {
    pub fn nova(contagem: Arc<std::sync::atomic::AtomicU32>) -> Posse {
        contagem.fetch_add(1, Ordering::SeqCst);
        Posse { contagem }
    }
}

impl Clone for Posse {
    fn clone(&self) -> Posse {
        Posse::nova(Arc::clone(&self.contagem))
    }
}

impl Drop for Posse {
    fn drop(&mut self) {
        self.contagem.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Sessão de captura de tela ativa. Mantém viva a cadeia de objetos WinRT (item, frame pool,
/// sessão) — deixá-los cair encerra a captura.
pub struct ScreenCapture {
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// Dispara (sem carregar dado) toda vez que um quadro novo fica pronto em `slot`. O dado em
    /// si não viaja pelo canal — só o aviso — porque `crossbeam_channel::Sender` não permite
    /// "trocar" um item já enfileirado, e a gente quer manter só o quadro mais recente.
    pub frame_ready: Receiver<()>,
    slot: Arc<Mutex<Option<CapturedFrame>>>,
    /// O sistema avisou que a origem desta captura deixou de existir — quase sempre porque o
    /// monitor foi desconectado.
    ///
    /// Existe porque **perder a fonte no meio da transmissão não tinha sinal nenhum**: o WGC
    /// simplesmente para de entregar quadro, e uma tela parada é indistinguível de uma tela sem
    /// mudança (que é o comportamento correto e comum — achado 8 do `README.md`). Sem este
    /// sinalizador o emissor ficaria vivo, codificando nada, e o receptor não teria como saber a
    /// diferença.
    fechado: Arc<AtomicBool>,
    /// Quantos quadros o **WGC entregou**, contados na própria callback `FrameArrived`.
    ///
    /// Existe porque `Contadores::capturados` conta outra coisa: quadros que o laço de
    /// `transmissao.rs` **consumiu** da caixa postal. Os dois números só coincidem se o laço nunca
    /// perder um quadro, e a diferença entre eles é exatamente o que separa "a captura entrega
    /// pouco" de "o laço não dá conta do que a captura entrega". Sem os dois, medir "por que 11,7
    /// fps quando 30 foram pedidos" é adivinhação — foi o que faltou na sessão de 28/08.
    chegados: Arc<AtomicU64>,
    pub width: u32,
    pub height: u32,
    /// O `HMONITOR` de onde esta captura saiu, como número. É o que a sessão do monitor virtual
    /// compara com o `HMONITOR` do alvo agora para saber se tem de reabrir (`ativacao::reabertura`).
    pub hmonitor: isize,
    /// O primeiro e o último quadro que passaram, só na captura do monitor virtual
    /// ([`ScreenCapture::start_for_item`]): é com eles que a sessão mede o buraco da troca de
    /// `HMONITOR`. `None` no caminho de sempre.
    marcas: Option<Arc<Mutex<(Option<Instant>, Option<Instant>)>>>,
}

// =================================================================================================
// O monitor virtual: o portão, e a captura aberta num fio auxiliar com prazo (ao lado do caminho de
// sempre, que não muda)
// =================================================================================================

/// Agora, no relógio do `QueryPerformanceCounter` em unidades de 100 ns — o mesmo de
/// `Direct3D11CaptureFrame::SystemRelativeTime`, a hora em que o DWM compôs o quadro.
pub fn agora_100ns() -> i64 {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    static FREQUENCIA: AtomicI64 = AtomicI64::new(0);
    let mut f = FREQUENCIA.load(Ordering::Relaxed);
    if f == 0 {
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        if f <= 0 {
            return 0;
        }
        FREQUENCIA.store(f, Ordering::Relaxed);
    }
    let mut c = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
    }
    ((c as i128) * 10_000_000 / (f as i128)) as i64
}

pub fn rect_igual(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

/// O retângulo da janela na tela (pixels físicos: o processo é *per monitor v2*).
pub fn retangulo_da_janela(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok().map(|_| r)
}

/// O retângulo do monitor de um `HMONITOR` (como número), agora.
pub fn retangulo_do_monitor(hmonitor: isize) -> Option<RECT> {
    let mut i = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(HMONITOR(hmonitor as *mut std::ffi::c_void), &mut i) }.as_bool().then_some(i.rcMonitor)
}

/// **A janela está por cima de tudo no retângulo?** Nove pontos — os cantos e os meios das bordas a 4
/// px para dentro, e o centro — têm de ser dela no `WindowFromPoint`. Uma janela "sempre visível" da
/// pessoa arrastada para o monitor, a barra de tarefas das telas secundárias (que o Explorer cria
/// depois da chegada) ou o Win+Tab ficariam acima (a revisão de 15/09, item 3). Chamado de fora do
/// fio da janela: o `WindowFromPoint` não manda `WM_NCHITTEST` a janela de outro fio.
pub fn janela_por_cima(hwnd: HWND, r: &RECT) -> bool {
    let (x0, xm, x1) = (r.left + 4, (r.left + r.right) / 2, r.right - 5);
    let (y0, ym, y1) = (r.top + 4, (r.top + r.bottom) / 2, r.bottom - 5);
    for x in [x0, xm, x1] {
        for y in [y0, ym, y1] {
            let w = unsafe { WindowFromPoint(POINT { x, y }) };
            if w != hwnd && unsafe { GetAncestor(w, GA_ROOT) } != hwnd {
                return false;
            }
        }
    }
    true
}

/// A hora da composição de um quadro, se ela for razoável: antes da chegada (até 100 ms de sobra
/// para o arredondamento dos relógios) e não mais que 2 s antes. `None` = use a da chegada, que é
/// sempre mais tarde que a composição (conservador).
pub fn hora_da_composicao(quadro_100ns: Option<i64>, agora_100ns: i64) -> Option<i64> {
    quadro_100ns.filter(|&q| q <= agora_100ns + 1_000_000 && agora_100ns - q <= 20_000_000)
}

/// O quadro composto em `quando` passou da folga depois de o portão abrir (`aberto`) e da última
/// conferência ruim (`ruim`)? Tudo em 100 ns.
pub fn passou_a_folga(quando: i64, aberto: i64, ruim: i64) -> bool {
    aberto != 0 && quando >= aberto.max(ruim) + PortaoDeCobertura::FOLGA_100NS
}

/// Por que o portão jogou um quadro fora, motivo a motivo.
#[derive(Clone, Copy, Debug, Default)]
pub struct DescartesDoPortao {
    /// O portão fechado: a janela escondida (indo atrás do monitor, ou ele sumiu) ou ainda nascendo.
    pub fechado: u64,
    /// Composto antes de o portão abrir — ou de a última conferência ruim — mais a folga.
    pub folga: u64,
    /// A janela invisível, ou fora do retângulo que ela conferiu ao aparecer.
    pub janela: u64,
    /// O monitor da captura (o `HMONITOR` dela) não está no retângulo da janela: ele andou.
    pub monitor: u64,
    /// Outra janela por cima da nossa num dos nove pontos.
    pub por_cima: u64,
    /// A hora da composição veio fora do razoável, e o portão usou a da chegada (não é descarte).
    pub relogio_estranho: u64,
}

impl DescartesDoPortao {
    pub fn total(&self) -> u64 {
        self.fechado + self.folga + self.janela + self.monitor + self.por_cima
    }

    pub fn linha(&self) -> String {
        format!(
            "portão descartou: fechado={} folga={} janela={} monitor={} por_cima={} (hora da composição estranha em {})",
            self.fechado, self.folga, self.janela, self.monitor, self.por_cima, self.relogio_estranho
        )
    }
}

/// **O portão da cobertura**: um monitor virtual só pode ser capturado **coberto** pela janela
/// sintética da bancada (`cobertura.rs`). A janela abre o portão depois de aparecer conferida sobre o
/// monitor, com o retângulo que conferiu, e o fecha **antes** de se esconder. A captura decide **cada
/// quadro na chegada**, sem esperar o fio da janela perceber (a revisão de 15/09, item 2):
///
/// 1. o portão aberto;
/// 2. o quadro **composto** (`SystemRelativeTime`, não a hora da chegada — item 4) depois de o portão
///    abrir e de a última conferência ruim, mais [`PortaoDeCobertura::FOLGA`];
/// 3. a janela visível e no retângulo conferido;
/// 4. o monitor da captura (o `HMONITOR` dela) nesse mesmo retângulo — o monitor que anda com o mesmo
///    `HMONITOR` (17:41) e deixa a janela no lugar velho, pelo veto, cai aqui;
/// 5. a janela por cima nos nove pontos ([`janela_por_cima`]) — senão a janela é avisada para voltar ao
///    topo (item 3).
///
/// Qualquer conferência que falha marca a hora: a folga recomeça dela. O quadro que passa é **copiado
/// para uma textura nossa antes de devolver o buffer ao pool** ([`ScreenCapture::start_for_item`]):
/// o encoder nunca lê um buffer que o DWM pode ter recomposto descoberto depois (item 1).
#[derive(Default)]
pub struct PortaoDeCobertura {
    /// Quando abriu, no relógio da composição; 0 = fechado.
    aberto_100ns: AtomicI64,
    /// A última conferência ruim (ou o último fechar), no mesmo relógio.
    ruim_100ns: AtomicI64,
    /// A janela da cobertura e o retângulo que ela conferiu ao aparecer.
    hwnd: AtomicIsize,
    retangulo: Mutex<Option<RECT>>,
    /// Alguém ficou por cima: a janela volta ao topo na próxima volta dela.
    coberta_por_outra: AtomicBool,
    d_fechado: AtomicU64,
    d_folga: AtomicU64,
    d_janela: AtomicU64,
    d_monitor: AtomicU64,
    d_por_cima: AtomicU64,
    relogio_estranho: AtomicU64,
}

impl PortaoDeCobertura {
    pub const FOLGA: std::time::Duration = std::time::Duration::from_millis(150);
    pub const FOLGA_100NS: i64 = 1_500_000;

    /// A janela que o portão confere (uma vez, quando ela nasce).
    pub fn ligar_janela(&self, hwnd: isize) {
        self.hwnd.store(hwnd, Ordering::SeqCst);
    }

    /// A janela apareceu conferida sobre `rect`. Aberto em outro retângulo conta como conferência
    /// ruim: a folga recomeça.
    pub fn abrir_agora(&self, rect: RECT) {
        let agora = agora_100ns().max(1);
        if let Ok(mut r) = self.retangulo.lock() {
            if r.is_some_and(|v| !rect_igual(&v, &rect)) {
                self.ruim_100ns.store(agora, Ordering::SeqCst);
            }
            *r = Some(rect);
        }
        let _ = self.aberto_100ns.compare_exchange(0, agora, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Fecha **antes** de a janela sair da tela.
    pub fn fechar(&self) {
        self.aberto_100ns.store(0, Ordering::SeqCst);
        self.ruim_100ns.store(agora_100ns(), Ordering::SeqCst);
        if let Ok(mut r) = self.retangulo.lock() {
            *r = None;
        }
    }

    /// Uma conferência (daqui ou da janela) falhou agora: a folga recomeça.
    pub fn marcar_ruim(&self) {
        self.ruim_100ns.store(agora_100ns(), Ordering::SeqCst);
    }

    /// A captura viu outra janela por cima desde a última pergunta?
    pub fn pediram_o_topo(&self) -> bool {
        self.coberta_por_outra.swap(false, Ordering::SeqCst)
    }

    /// Aberto, e passada a folga desde a abertura e a última conferência ruim — o que a sessão
    /// confere antes de pedir a captura.
    pub fn aberto_ha_folga(&self) -> bool {
        let a = self.aberto_100ns.load(Ordering::SeqCst);
        a != 0 && agora_100ns() >= a.max(self.ruim_100ns.load(Ordering::SeqCst)) + Self::FOLGA_100NS
    }

    /// **O quadro composto em `quadro_100ns`, da captura de `hmonitor`, pode passar?** Ver o
    /// cabeçalho do tipo. `None` na hora da composição usa a da chegada.
    pub fn deixa(&self, quadro_100ns: Option<i64>, hmonitor: isize) -> bool {
        let aberto = self.aberto_100ns.load(Ordering::SeqCst);
        if aberto == 0 {
            self.d_fechado.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let agora = agora_100ns();
        let quando = hora_da_composicao(quadro_100ns, agora).unwrap_or_else(|| {
            self.relogio_estranho.fetch_add(1, Ordering::Relaxed);
            agora
        });
        if !passou_a_folga(quando, aberto, self.ruim_100ns.load(Ordering::SeqCst)) {
            self.d_folga.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let Some(rect) = self.retangulo.lock().ok().and_then(|r| *r) else {
            self.d_fechado.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let hwnd = HWND(self.hwnd.load(Ordering::SeqCst) as *mut std::ffi::c_void);
        let visivel = !hwnd.is_invalid() && unsafe { IsWindowVisible(hwnd) }.as_bool();
        if !visivel || retangulo_da_janela(hwnd).is_none_or(|j| !rect_igual(&j, &rect)) {
            self.d_janela.fetch_add(1, Ordering::Relaxed);
            self.marcar_ruim();
            return false;
        }
        if retangulo_do_monitor(hmonitor).is_none_or(|m| !rect_igual(&m, &rect)) {
            self.d_monitor.fetch_add(1, Ordering::Relaxed);
            self.marcar_ruim();
            return false;
        }
        if !janela_por_cima(hwnd, &rect) {
            self.d_por_cima.fetch_add(1, Ordering::Relaxed);
            self.coberta_por_outra.store(true, Ordering::SeqCst);
            self.marcar_ruim();
            return false;
        }
        true
    }

    pub fn descartes(&self) -> DescartesDoPortao {
        DescartesDoPortao {
            fechado: self.d_fechado.load(Ordering::Relaxed),
            folga: self.d_folga.load(Ordering::Relaxed),
            janela: self.d_janela.load(Ordering::Relaxed),
            monitor: self.d_monitor.load(Ordering::Relaxed),
            por_cima: self.d_por_cima.load(Ordering::Relaxed),
            relogio_estranho: self.relogio_estranho.load(Ordering::Relaxed),
        }
    }

    /// Quantos quadros o portão jogou fora, somados.
    pub fn descartados(&self) -> u64 {
        self.descartes().total()
    }
}

/// Quantos fios de abertura da captura ficaram presos neste processo (abandonados pelo prazo).
pub static FIOS_ABANDONADOS: AtomicU64 = AtomicU64::new(0);

/// Quantos dos **abandonados pelo prazo** voltaram depois — só esses (a revisão de 15/09, item 12):
/// é o que separa "preso para sempre" (o estado de ~490 monitores, §13.5) de "demorou mais que o
/// prazo" (na N = 8 de 15/09, três prazos venceram com oito monitores chegando).
pub static FIOS_QUE_VOLTARAM_TARDE: AtomicU64 = AtomicU64::new(0);

/// Quanto durou cada passo da abertura, no fio auxiliar.
#[derive(Clone, Copy, Debug, Default)]
pub struct DuracoesDaAbertura {
    /// A fábrica do interop e o `CreateForMonitor`.
    pub item_ms: u64,
    /// O pool, a sessão e o `StartCapture`.
    pub captura_ms: u64,
}

/// Uma abertura em curso ([`pedir_captura`]).
pub struct PedidoDeCaptura {
    pub resposta: Receiver<Result<(ScreenCapture, DuracoesDaAbertura)>>,
    abandonado: Arc<AtomicBool>,
}

impl PedidoDeCaptura {
    /// O prazo venceu: quem pediu desiste, e o fio fica para trás. Só este caminho marca o fio como
    /// abandonado — a sessão que acaba no meio de uma abertura só larga o pedido.
    pub fn abandonar(self) {
        self.abandonado.store(true, Ordering::SeqCst);
        FIOS_ABANDONADOS.fetch_add(1, Ordering::SeqCst);
    }
}

/// **Abre a captura de um monitor num fio auxiliar**: o item (`CreateForMonitor`), o pool, a sessão
/// e o `StartCapture` — **nada disso no fio da sessão**, que segue mandando quadro (a repetição de
/// 500 ms) enquanto espera. Quem pede olha a resposta quando quiser e conta o prazo ele mesmo.
///
/// Por quê: no Dell, depois de ~490 monitores virtuais desde o boot, `CreateForMonitor` **prendeu
/// para sempre**, duas de duas vezes (§13.5); e na N = 8 de 15/09, com monitores chegando, a troca da
/// captura no fio da sessão (o pool, a sessão, o `Close` da velha) segurou sessões vivas 6,1–7,0 s sem
/// bater o coração (a revisão de 15/09, item 10).
///
/// **O que fica vazado quando o fio é abandonado**: o próprio fio (a pilha, 2 MiB reservados), a
/// entrada dele no apartamento COM multithread (`CoInitializeEx`), a fábrica do interop, o que a
/// chamada presa segura no `dwm` — e **uma referência ao dispositivo D3D11 da cadeia**, que o pool
/// usa: o dispositivo não morre antes do processo. É o preço de o fio da sessão não prender; o
/// processo marca "captura travada" e recusa `ADD` novo, então é no máximo um por sessão. Se o fio
/// voltar depois, a captura que ele abriu é fechada ali mesmo e nada começa.
pub fn pedir_captura(
    dispositivo: &ID3D11Device,
    hmonitor: HMONITOR,
    portao: Option<Arc<PortaoDeCobertura>>,
    cursor: bool,
) -> PedidoDeCaptura {
    let (tx, rx) = bounded::<Result<(ScreenCapture, DuracoesDaAbertura)>>(1);
    let abandonado = Arc::new(AtomicBool::new(false));
    let ab = abandonado.clone();
    // O `HMONITOR` é um ponteiro cru (não é `Send`); atravessa como número.
    let h = hmonitor.0 as isize;
    let dispositivo = dispositivo.clone();
    let tx_erro = tx.clone();
    let fio = std::thread::Builder::new().name("quall.abrir-captura".into()).spawn(move || {
        let comeco = Instant::now();
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let r = (|| -> Result<(ScreenCapture, DuracoesDaAbertura)> {
            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(HMONITOR(h as *mut std::ffi::c_void))? };
            let item_ms = comeco.elapsed().as_millis() as u64;
            let t = Instant::now();
            let c = ScreenCapture::start_for_item(&dispositivo, item, HMONITOR(h as *mut std::ffi::c_void), portao, cursor)?;
            Ok((c, DuracoesDaAbertura { item_ms, captura_ms: t.elapsed().as_millis() as u64 }))
        })();
        if let Err(e) = tx.send(r) {
            // Quem pediu já não espera: a captura que abriu fecha aqui, e nada começa.
            if let Ok((c, _)) = e.into_inner() {
                c.stop();
            }
            let ms = comeco.elapsed().as_millis();
            if ab.load(Ordering::SeqCst) {
                FIOS_QUE_VOLTARAM_TARDE.fetch_add(1, Ordering::SeqCst);
                crate::registro::linha(format!("captura: uma abertura abandonada pelo prazo voltou {ms} ms depois de pedida (fechada)"));
            } else {
                crate::registro::linha(format!("captura: uma abertura voltou {ms} ms depois de pedida, sem ninguém esperando (fechada)"));
            }
        }
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    });
    if let Err(e) = fio {
        let _ = tx_erro.send(Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            format!("não consegui criar o fio da abertura da captura: {e}"),
        )));
    }
    PedidoDeCaptura { resposta: rx, abandonado }
}

/// **Fecha uma captura de tela fora do fio de quem chama**: o `Close` da sessão e do pool do WGC
/// segurou a sessão com monitores chegando (a revisão de 15/09, item 10). A duração vai para o
/// registro quando passa de 200 ms.
pub fn fechar_em_segundo_plano(c: ScreenCapture, rotulo: String) {
    let no_fio = rotulo.clone();
    let r = std::thread::Builder::new().name("quall.fechar-captura".into()).spawn(move || {
        let t = Instant::now();
        c.stop();
        drop(c);
        let ms = t.elapsed().as_millis();
        if ms > 200 {
            crate::registro::linha(format!("captura: fechar {no_fio} levou {ms} ms (fora do fio da sessão)"));
        }
    });
    if let Err(e) = r {
        crate::registro::linha(format!("captura: não consegui o fio para fechar a origem ({e}); a captura cai sem Close"));
    }
}

/// O anel de texturas nossas para onde o quadro que passou no portão é copiado antes de o buffer
/// voltar ao pool. Seis: a caixa postal (1), o pendente (1) e o que o MFT segura (até 2–3) cabem.
struct AnelDeCopia {
    texturas: Vec<ID3D11Texture2D>,
    proxima: usize,
}

const TAMANHO_DO_ANEL_DE_COPIA: usize = 6;

impl AnelDeCopia {
    /// A próxima textura do anel, criada sob medida para `fonte` na primeira volta.
    fn proxima(&mut self, dispositivo: &ID3D11Device, fonte: &ID3D11Texture2D) -> Option<ID3D11Texture2D> {
        if self.texturas.len() < TAMANHO_DO_ANEL_DE_COPIA {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { fonte.GetDesc(&mut desc) };
            desc.Usage = D3D11_USAGE_DEFAULT;
            desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
            desc.CPUAccessFlags = 0;
            desc.MiscFlags = 0;
            let mut t: Option<ID3D11Texture2D> = None;
            unsafe { dispositivo.CreateTexture2D(&desc, None, Some(&mut t)) }.ok()?;
            let t = t?;
            self.texturas.push(t.clone());
            return Some(t);
        }
        let t = self.texturas[self.proxima % TAMANHO_DO_ANEL_DE_COPIA].clone();
        self.proxima = (self.proxima + 1) % TAMANHO_DO_ANEL_DE_COPIA;
        Some(t)
    }
}

impl ScreenCapture {
    /// **A captura sobre um item já criado** — a do monitor virtual ([`pedir_captura`], num fio
    /// auxiliar), ao lado de [`ScreenCapture::start_for_monitor`], que o caminho de sempre continua
    /// usando sem mudança. Três diferenças, todas do monitor virtual:
    ///
    /// - o `portao`: o quadro que ele não deixa passar é fechado ali mesmo e não entra na caixa
    ///   postal (a janela sintética não o cobria) — ver [`PortaoDeCobertura`];
    /// - **a cópia**: o quadro que passa é copiado para uma textura nossa (um anel de seis) **antes**
    ///   de o `Close` devolver o buffer ao pool. O encoder lê a nossa, nunca um buffer que o DWM pode
    ///   ter recomposto — descoberto — depois da decisão do portão (a revisão de 15/09, item 1). A
    ///   cópia é feita no fio do WGC, no contexto imediato do dispositivo da cadeia: quem abre esta
    ///   captura liga a proteção multithread dele (`device::proteger_contexto`). Com a caixa postal
    ///   ainda cheia, a cópia vai para a mesma textura (ninguém mais a segura);
    /// - o `cursor`: na bancada, **fora** (sobre a nossa janela, o ponteiro de quem está no Dell não
    ///   é conteúdo nosso); na tela estendida de verdade, dentro, como no caminho de sempre.
    ///
    /// O `start_for_monitor` (uma sessão só) entrega o buffer do pool depois do `Close`, como sempre
    /// entregou: o mesmo padrão, que ali não tem portão a furar — não mexido (decisão da revisão).
    pub fn start_for_item(
        d3d_device: &ID3D11Device,
        item: GraphicsCaptureItem,
        hmonitor: HMONITOR,
        portao: Option<Arc<PortaoDeCobertura>>,
        cursor: bool,
    ) -> Result<Self> {
        let winrt_device = winrt_device_from_d3d11(d3d_device)?;
        let size: SizeInt32 = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size)?;
        let slot: Arc<Mutex<Option<CapturedFrame>>> = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = bounded::<()>(1);
        let slot_producer = slot.clone();
        let chegados = Arc::new(AtomicU64::new(0));
        let contador = chegados.clone();
        let marcas: Arc<Mutex<(Option<Instant>, Option<Instant>)>> = Arc::new(Mutex::new((None, None)));
        let marcar = marcas.clone();
        let dispositivo = d3d_device.clone();
        let contexto = unsafe { d3d_device.GetImmediateContext()? };
        let anel = Mutex::new(AnelDeCopia { texturas: Vec::new(), proxima: 0 });
        let h = hmonitor.0 as isize;
        pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
            let Some(pool) = pool.as_ref() else { return Ok(()) };
            let Ok(frame) = pool.TryGetNextFrame() else { return Ok(()) };
            let captured_at = Instant::now();
            if let Some(p) = portao.as_ref() {
                let composto = frame.SystemRelativeTime().ok().map(|t| t.Duration);
                if !p.deixa(composto, h) {
                    let _ = frame.Close();
                    return Ok(());
                }
            }
            let Ok(fonte) = texture_from_frame(&frame) else {
                let _ = frame.Close();
                return Ok(());
            };
            // A cópia, com a caixa postal presa: o laço não tira o quadro no meio dela.
            let mut caixa = slot_producer.lock().unwrap();
            let destino = match caixa.as_ref() {
                Some(q) => Some(q.texture.clone()),
                None => anel.lock().ok().and_then(|mut a| a.proxima(&dispositivo, &fonte)),
            };
            let Some(destino) = destino else {
                drop(caixa);
                let _ = frame.Close();
                return Ok(());
            };
            unsafe { contexto.CopyResource(&destino, &fonte) };
            // Só agora o buffer volta ao pool.
            drop(fonte);
            let _ = frame.Close();
            *caixa = Some(CapturedFrame { texture: destino, captured_at, posse: None });
            drop(caixa);
            if let Ok(mut m) = marcar.lock() {
                m.0.get_or_insert(captured_at);
                m.1 = Some(captured_at);
            }
            contador.fetch_add(1, Ordering::Relaxed);
            notify(&ready_tx);
            Ok(())
        }))?;
        let fechado = Arc::new(AtomicBool::new(false));
        let marcador = fechado.clone();
        item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            marcador.store(true, Ordering::SeqCst);
            Ok(())
        }))?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        session.StartCapture()?;
        Ok(Self {
            _item: item,
            pool,
            session,
            frame_ready: ready_rx,
            slot,
            fechado,
            chegados,
            width: size.Width as u32,
            height: size.Height as u32,
            hmonitor: hmonitor.0 as isize,
            marcas: Some(marcas),
        })
    }

    /// Quando passou o primeiro quadro (só na captura do monitor virtual).
    pub fn primeiro_quadro(&self) -> Option<Instant> {
        self.marcas.as_ref().and_then(|m| m.lock().ok().and_then(|m| m.0))
    }

    /// Quando passou o último quadro (só na captura do monitor virtual).
    pub fn ultimo_quadro(&self) -> Option<Instant> {
        self.marcas.as_ref().and_then(|m| m.lock().ok().and_then(|m| m.1))
    }
    /// Inicia a captura do monitor primário no dispositivo D3D11 informado (deve ser o mesmo
    /// dispositivo registrado no `IMFDXGIDeviceManager` do encoder, para a textura não precisar
    /// atravessar adaptador).
    pub fn start_primary_monitor(d3d_device: &ID3D11Device) -> Result<Self> {
        Self::start_for_monitor(d3d_device, primary_monitor()?)
    }

    /// Inicia a captura de **um monitor escolhido**.
    ///
    /// É o que o app de produto precisa e a sonda de bancada não precisava: `docs/ux-m6.md` §1.6
    /// pede uma linha por monitor no seletor, porque num desktop com dois monitores "a tela" não é
    /// resposta. O `HMONITOR` vem de `fontes::achar_hmonitor`, que o reencontra pelo nome do
    /// dispositivo GDI no instante da captura — guardar o handle entre a escolha e o Espelhar
    /// deixaria a captura apontando para outro monitor depois de uma troca de dock, sem aviso.
    pub fn start_for_monitor(d3d_device: &ID3D11Device, hmonitor: HMONITOR) -> Result<Self> {
        let winrt_device = winrt_device_from_d3d11(d3d_device)?;

        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(hmonitor)? };
        let size: SizeInt32 = item.Size()?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2, // dois buffers: o mínimo pra não travar o produtor esperando o consumidor
            size,
        )?;

        // Mailbox de 1 posição: "zero filas" na entrada do encoder. Se o consumidor estiver
        // atrasado, o quadro velho é sobrescrito pelo novo — latência de fila zero à custa de,
        // ocasionalmente, pular um quadro. Para tela (conteúdo majoritariamente estático) essa é
        // a troca certa.
        let slot: Arc<Mutex<Option<CapturedFrame>>> = Arc::new(Mutex::new(None));
        let (ready_tx, ready_rx) = bounded::<()>(1);
        let slot_producer = slot.clone();
        let chegados = Arc::new(AtomicU64::new(0));
        let contador = chegados.clone();

        pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
            move |pool, _| {
                if let Some(pool) = pool.as_ref() {
                    if let Ok(frame) = pool.TryGetNextFrame() {
                        let captured_at = Instant::now();
                        if let Ok(texture) = texture_from_frame(&frame) {
                            // Contado ANTES da caixa postal: este é o número de quadros que o WGC
                            // entregou, independentemente de o laço consumir ou sobrescrever.
                            contador.fetch_add(1, Ordering::Relaxed);
                            let _ = frame.Close();
                            *slot_producer.lock().unwrap() = Some(CapturedFrame { texture, captured_at, posse: None });
                            notify(&ready_tx);
                        } else {
                            let _ = frame.Close();
                        }
                    }
                }
                Ok(())
            },
        ))?;

        // O aviso de que a origem morreu. É um evento do **item**, não da sessão nem do pool: quem
        // deixa de existir quando o cabo sai é o monitor, e o item é o que o representa.
        let fechado = Arc::new(AtomicBool::new(false));
        let marcador = fechado.clone();
        item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
            move |_, _| {
                marcador.store(true, Ordering::SeqCst);
                Ok(())
            },
        ))?;

        let session = pool.CreateCaptureSession(&item)?;
        // Cursor ligado: é o comportamento esperado de um produto de espelhamento de tela (quem
        // recebe quer ver o ponteiro do mouse de quem está emitindo). A primeira versão desligava
        // isso pra "medir só o conteúdo" — mas medido na bancada (ver README.md) o achado foi
        // outro: com a tela sem nenhuma mudança visível, o DWM não recompõe e o WGC não entrega
        // quadro novo nenhum (correto, não é bug — evita gastar GPU à toa numa tela parada). Numa
        // bancada desatendida isso zera a taxa de captura em segundos. Cursor visível é tanto o
        // comportamento certo do produto quanto o que garante alguma mudança de tela pra medir.
        if let Err(e) = session.SetIsCursorCaptureEnabled(true) {
            eprintln!("aviso: não consegui ligar a captura do cursor: {e}");
        }
        session.StartCapture()?;

        Ok(Self {
            _item: item,
            pool,
            session,
            frame_ready: ready_rx,
            slot,
            fechado,
            chegados,
            width: size.Width as u32,
            height: size.Height as u32,
            hmonitor: hmonitor.0 as isize,
            marcas: None,
        })
    }

    /// Retira o quadro mais recente da caixa postal, se houver um pronto.
    pub fn take_frame(&self) -> Option<CapturedFrame> {
        self.slot.lock().unwrap().take()
    }

    /// O instante de captura do quadro que está na caixa postal, **sem tirá-lo de lá**.
    ///
    /// Existe para o laço poder medir o intervalo entre quadros entregues sem consumir o quadro:
    /// no caminho de caixa única (`--caixa-unica`) quem tira o quadro é a submissão, e não o
    /// ramo do `select!`, mas o perfil de intervalo de captura tem de continuar existindo — ele
    /// é a testemunha de que a captura entrega mais do que o encoder pede.
    pub fn espiar_instante(&self) -> Option<Instant> {
        self.slot.lock().unwrap().as_ref().map(|q| q.captured_at)
    }

    /// O sistema já disse que esta origem acabou?
    pub fn item_fechado(&self) -> bool {
        self.fechado.load(Ordering::SeqCst)
    }

    /// Quantos quadros o WGC entregou desde o começo — contados na callback, não no laço.
    pub fn chegados(&self) -> u64 {
        self.chegados.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// **De onde a cadeia tira os quadros**: a tela, pelo WGC, ou uma textura nossa
/// (`crate::sintetica`), que é a origem da prova do emissor com vários receptores e a
/// implementação provisória do monitor da sessão até existir o monitor virtual.
///
/// As duas têm a mesma semântica de caixa postal de uma posição, e a `Cadeia` lê as duas pelos
/// mesmos quatro métodos — o caminho do quadro não sabe qual das duas está do outro lado.
pub enum Captura {
    Tela(ScreenCapture),
    Sintetica(crate::sintetica::OrigemSintetica),
    /// Uma câmera do PC (`captura_de_camera.rs`): o leitor assíncrono do Media Foundation, com o
    /// quadro copiado na chegada para um anel nosso (NV12 ou YUY2).
    Camera(crate::captura_de_camera::CapturaDeCamera),
    /// **A câmera do dono da captura do R5** (`dono_da_captura.rs`): a rede lê o anel dela, que o
    /// dono escreve por cópia com posse por posição. Parar solta a rede; a câmera é do dono.
    DoDono(crate::dono_da_captura::LeitorDoDono),
}

impl Captura {
    /// O canal de aviso. Clonado por quem faz `select!`: o `take_frame` da origem sintética
    /// precisa de `&mut`, e um empréstimo do canal vivo no corpo do `select!` o impediria.
    pub fn avisos(&self) -> &Receiver<()> {
        match self {
            Captura::Tela(c) => &c.frame_ready,
            Captura::Sintetica(s) => &s.frame_ready,
            Captura::Camera(c) => &c.frame_ready,
            Captura::DoDono(l) => &l.frame_ready,
        }
    }

    pub fn take_frame(&mut self) -> Option<CapturedFrame> {
        match self {
            Captura::Tela(c) => c.take_frame(),
            Captura::Sintetica(s) => s.take_frame(),
            Captura::Camera(c) => c.take_frame(),
            Captura::DoDono(l) => l.take_frame(),
        }
    }

    pub fn espiar_instante(&self) -> Option<Instant> {
        match self {
            Captura::Tela(c) => c.espiar_instante(),
            Captura::Sintetica(s) => s.espiar_instante(),
            Captura::Camera(c) => c.espiar_instante(),
            Captura::DoDono(l) => l.espiar_instante(),
        }
    }

    /// Uma origem sintética nunca some sozinha. A câmera some com erro do leitor, evento com falha
    /// ou sem quadro por 3 s.
    pub fn item_fechado(&self) -> bool {
        match self {
            Captura::Tela(c) => c.item_fechado(),
            Captura::Sintetica(_) => false,
            Captura::Camera(c) => c.item_fechado(),
            Captura::DoDono(l) => l.item_fechado(),
        }
    }

    pub fn chegados(&self) -> u64 {
        match self {
            Captura::Tela(c) => c.chegados(),
            Captura::Sintetica(s) => s.chegados(),
            Captura::Camera(c) => c.chegados(),
            Captura::DoDono(l) => l.chegados(),
        }
    }

    pub fn stop(&mut self) {
        match self {
            Captura::Tela(c) => c.stop(),
            Captura::Sintetica(s) => s.parar(),
            Captura::Camera(c) => c.stop(),
            Captura::DoDono(l) => l.stop(),
        }
    }

    pub fn largura(&self) -> u32 {
        match self {
            Captura::Tela(c) => c.width,
            Captura::Sintetica(s) => s.width,
            Captura::Camera(c) => c.width,
            Captura::DoDono(l) => l.info.largura,
        }
    }

    pub fn altura(&self) -> u32 {
        match self {
            Captura::Tela(c) => c.height,
            Captura::Sintetica(s) => s.height,
            Captura::Camera(c) => c.height,
            Captura::DoDono(l) => l.info.altura,
        }
    }

    pub fn e_sintetica(&self) -> bool {
        matches!(self, Captura::Sintetica(_))
    }

    /// O `HMONITOR` da captura de tela, como número; `None` na origem sintética e na câmera.
    pub fn hmonitor(&self) -> Option<isize> {
        match self {
            Captura::Tela(c) => Some(c.hmonitor),
            Captura::Sintetica(_) | Captura::Camera(_) | Captura::DoDono(_) => None,
        }
    }

    pub fn primeiro_quadro(&self) -> Option<Instant> {
        match self {
            Captura::Tela(c) => c.primeiro_quadro(),
            Captura::Sintetica(_) => None,
            Captura::Camera(c) => c.primeiro_quadro(),
            Captura::DoDono(_) => None,
        }
    }

    pub fn ultimo_quadro(&self) -> Option<Instant> {
        match self {
            Captura::Tela(c) => c.ultimo_quadro(),
            Captura::Sintetica(_) => None,
            Captura::Camera(c) => c.ultimo_quadro(),
            Captura::DoDono(l) => l.ultimo_quadro(),
        }
    }

    pub fn e_camera(&self) -> bool {
        matches!(self, Captura::Camera(_) | Captura::DoDono(_))
    }

    /// Acaba a captura da câmera com um motivo de fora dela (o encoder que segura os quadros da
    /// câmera com conversor). Nas outras origens não faz nada.
    pub fn encerrar_camera(&self, motivo: String) {
        match self {
            Captura::Camera(c) => c.encerrar(motivo),
            // **A câmera do dono não acaba por causa da rede** (a revisão do plano, B2): o encoder que
            // segura os quadros acaba esta sessão, e a prévia e a gravação seguem.
            Captura::DoDono(l) => l.encerrar(motivo),
            _ => {}
        }
    }

    /// Como a câmera acabou, quando acabou, e se foi desconexão (para o registro e a tela).
    pub fn fim_da_camera(&self) -> Option<crate::regras_da_camera::FimDaCamera> {
        match self {
            Captura::Camera(c) => c.fim(),
            Captura::DoDono(l) => l.fim(),
            _ => None,
        }
    }

    /// A câmera está parada (sem quadro por `regras_da_camera::SEM_QUADRO`), e há quanto tempo.
    /// `None` nas outras origens.
    pub fn camera_parada_ha(&self, agora: Instant) -> Option<Duration> {
        match self {
            Captura::Camera(c) => c.parada_ha(agora),
            Captura::DoDono(l) => l.parada_ha(agora),
            _ => None,
        }
    }

    /// O quadro da câmera que pode ser repetido com ela parada (`CapturaDeCamera::quadro_para_repetir`).
    /// Do dono, a posição volta com a posse dela: o dono não a reescreve enquanto a repetição viver.
    pub fn quadro_da_camera_para_repetir(&self) -> Option<(ID3D11Texture2D, Option<Posse>)> {
        match self {
            Captura::Camera(c) => c.quadro_para_repetir().map(|t| (t, None)),
            Captura::DoDono(l) => l.quadro_para_repetir().map(|(t, p)| (t, Some(p))),
            _ => None,
        }
    }
}

/// Avisa o consumidor que há quadro novo. Se já havia um aviso pendente (consumidor atrasado),
/// não empilha um segundo — o quadro em `slot` já foi sobrescrito, um aviso só é suficiente.
fn notify(tx: &Sender<()>) {
    let _ = tx.try_send(());
}

fn texture_from_frame(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D> {
    let surface = frame.Surface()?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    unsafe { access.GetInterface::<ID3D11Texture2D>() }
}

fn primary_monitor() -> Result<HMONITOR> {
    Ok(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) })
}

/// Envolve o `ID3D11Device` (Win32) como `IDirect3DDevice` (WinRT), que é o que as APIs de
/// Windows.Graphics.Capture pedem. É o mesmo dispositivo D3D11 usado pelo encoder — por isso a
/// textura entregue pela captura não precisa de cópia entre adaptadores.
fn winrt_device_from_d3d11(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    let dxgi_device: windows::Win32::Graphics::Dxgi::IDXGIDevice = device.cast()?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)? };
    inspectable.cast()
}

/// Não usado diretamente (fica documentado por que descartamos `D3D_DRIVER_TYPE_UNKNOWN` como
/// caminho de criação de dispositivo aqui — a escolha de adaptador acontece em `main.rs`, que
/// sempre passa um `pAdapter` explícito e por isso usa `D3D_DRIVER_TYPE_UNKNOWN`).
#[allow(dead_code)]
const _: windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE = D3D_DRIVER_TYPE_UNKNOWN;

#[cfg(test)]
mod testes {
    use super::*;

    const MS: i64 = 10_000;

    #[test]
    fn a_folga_conta_da_composicao_e_da_ultima_conferencia_ruim() {
        let aberto = 1_000 * MS;
        // Composto 100 ms depois de abrir: dentro da folga de 150 ms, cai.
        assert!(!passou_a_folga(aberto + 100 * MS, aberto, 0));
        assert!(passou_a_folga(aberto + 150 * MS, aberto, 0));
        // Uma conferência ruim depois de abrir: a folga recomeça dela.
        let ruim = aberto + 400 * MS;
        assert!(!passou_a_folga(ruim + 149 * MS, aberto, ruim));
        assert!(passou_a_folga(ruim + 150 * MS, aberto, ruim));
        // Fechado não deixa nada.
        assert!(!passou_a_folga(aberto + 10_000 * MS, 0, 0));
    }

    #[test]
    fn o_quadro_composto_antes_de_abrir_e_que_chegou_depois_cai() {
        // O caso do item 4: o quadro chegou 300 ms depois de abrir, mas foi composto antes.
        let aberto = 5_000 * MS;
        let chegada = aberto + 300 * MS;
        let composto = aberto - 20 * MS;
        let quando = hora_da_composicao(Some(composto), chegada).expect("razoável");
        assert!(!passou_a_folga(quando, aberto, 0));
        // Pela hora da chegada, teria passado: é o defeito que a hora da composição fecha.
        assert!(passou_a_folga(chegada, aberto, 0));
    }

    #[test]
    fn hora_da_composicao_fora_do_razoavel_usa_a_da_chegada() {
        let agora = 100_000 * MS;
        assert_eq!(hora_da_composicao(Some(agora - 16 * MS), agora), Some(agora - 16 * MS));
        // Do futuro (além do arredondamento) ou velha demais: não se confia.
        assert_eq!(hora_da_composicao(Some(agora + 200 * MS), agora), None);
        assert_eq!(hora_da_composicao(Some(agora - 3_000 * MS), agora), None);
        assert_eq!(hora_da_composicao(None, agora), None);
    }
}
