//! A janela do produto, em **Win32 puro**.
//!
//! # A decisão de tecnologia, e o que foi descartado
//!
//! O critério que este projeto já usou para escolher dependência é **tamanho de binário** — foi
//! ele que escolheu `libdatachannel` em vez de libwebrtc, para o receptor caber num plugin de OBS.
//! Aplicado à janela:
//!
//! | opção | o que custa | por que não |
//! |---|---|---|
//! | **Win32 puro** | **zero** — o crate `windows` já é dependência deste pacote, e `present.rs` já cria janela, classe e laço de mensagens desde o M2 | escolhido |
//! | `egui`/`eframe` | ~200 crates novos (winit + glow/wgpu), megabytes de binário e uma segunda pilha de GPU dentro de um processo que já tem D3D11, MFT e captura | o próprio critério que escolheu a libdatachannel elimina esta |
//! | WinUI 3 / Windows App SDK | runtime redistribuível separado, identidade de pacote na prática, e bindings Rust imaturos | um produto LAN-only que tem de rodar em máquina de bancada não pode depender de um runtime instalado à parte |
//! | Tauri / WebView2 | um navegador inteiro para desenhar seis linhas de texto e três botões | desproporcional |
//!
//! O peso do Win32 puro é **código**, não binário: não há layout automático, então cada retângulo
//! é uma conta — e desde o "Estúdio de bolso" (`docs/telas-estudio.md` §7 e §11.5) as contas moram
//! numa tabela só, `estilo::lugar`.
//!
//! # A janela do "Estúdio de bolso"
//!
//! Escura, fixa em 880 × 580 DIP, com a **barra lateral** (Espelhar, Exibir, Teleprompter e, no pé,
//! o nome deste computador e Ajustes) e o painel do item escolhido. Com uma sessão de pé, o painel
//! mostra a tela dela (esperando, no ar, vários, conectando, exibindo) e os outros itens apagam:
//! **um papel de cada vez**, como antes, agora dito na barra.
//!
//! Os controles são **nativos**, e o desenho é nosso:
//!
//! - botões, ladrilhos, interruptores e o segmentado são `BUTTON`s de verdade (`BS_PUSHBUTTON`,
//!   `BS_AUTORADIOBUTTON | BS_PUSHLIKE`, `BS_AUTOCHECKBOX | BS_PUSHLIKE`) desenhados no
//!   **`NM_CUSTOMDRAW`** do comctl v6 (que o `app.manifest` pede). **Nunca `BS_OWNERDRAW`**
//!   (§11.5): o botão do dono do desenho não guarda estado (`BM_GETCHECK` responde sempre
//!   "desmarcado", e o som e o microfone ficariam sempre desligados), e perde o Tab e o Narrador. O
//!   estado de cada interruptor é **o do próprio controle**, lido por `BM_GETCHECK` no clique, como
//!   era antes;
//! - a lista "NA REDE AGORA" é um `LISTBOX` com `LBS_OWNERDRAWFIXED | LBS_HASSTRINGS`; o endereço e o
//!   PIN são `EDIT`s escuros (`WM_CTLCOLOREDIT`) dentro do campo pintado em volta; o letreiro do PIN
//!   é um `STATIC` desenhado, cujo texto é o que o Narrador lê ("PIN 4 8 2 7 1 9");
//! - o resto (título, pílula, frases, cartões) é pintado no `WM_PAINT`, pelo Direct2D
//!   (`estilo::d2d`), a partir do [`EstadoDaTela`] que o `modelo_da_janela` compõe.
//!
//! # A janela não sabe nada de rede
//!
//! Ela lê dois estados publicados e escreve gestos. Nunca toca no `Ready`, na track, no decoder
//! nem no disco — ver as notas de plataforma em `emissor.rs` e `receptor.rs` sobre por que os
//! handles do núcleo moram numa thread só. A cada mudança de versão ela monta um [`EstadoDaTela`]
//! (valores simples) e daí em diante só lê dele; o retrato de bancada (`--retratos-de-bancada`)
//! passa estados de exemplo pelo mesmo caminho, sem emissor nem receptor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::os::windows::ffi::OsStrExt;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Accessibility::{CLSID_AccPropServices, IAccPropServices, PROPID_ACC_NAME};
use windows::Win32::UI::Controls::{
    SetWindowTheme, CDDS_PREERASE, CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT,
    CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, EM_SETCUEBANNER, MEASUREITEMSTRUCT, NMCUSTOMDRAW, NMHDR, NM_CUSTOMDRAW, ODS_FOCUS,
    ODS_SELECTED, ODT_LISTBOX, ODT_STATIC,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, IsWindowEnabled, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::bandeja::{self, Bandeja, Escolha};
use crate::emissor::{Emissor, Fase};
use crate::estilo::{self, lugar, Ret};
use crate::fontes::{Especie as EspecieDaFonte, Fonte};
use crate::idioma::{t, tf, tr};
use crate::modelo_da_janela::{
    self as modelo, Aviso, Cena, Controle, ControlesDaCamera, Especie, EstadoDaTela, Ladrilho, LinhaDeAparelho, LinhaDeReceptor, Painel,
    SessaoNaBarra, TelaAjustes, TelaEspelhar, TelaEspera, TelaExibindo, TelaExibir, TelaNoAr, TelaVarios, MAX_LADRILHOS, VOLUMES,
};
use crate::receptor::{FaseDoReceptor, Receptor};
use crate::regras_da_bandeja::{self as regras_bandeja, AoMinimizar, Evento};
use crate::registro;

// Os controles temáticos (Common Controls 6) vêm de `app.manifest`, embutido pelo linker via
// `build.rs`. **Não** de um `#[link_section = ".drectve"]` aqui: essa forma foi tentada primeiro,
// compilou, ligou, rodou — e uma varredura dos bytes do exe não achou o manifesto. E o
// `NM_CUSTOMDRAW` dos botões só chega com eles.

const ID_RELOGIO: usize = 1;

/// A classe da janela principal. A instância única (`instancia.rs`) a procura por ela.
pub(crate) const CLASSE: PCWSTR = w!("QuallMonitorWindow");

// Ícones próprios criados uma vez, válidos pela vida da classe/processo. O cache LR_SHARED
// ignora o tamanho da segunda imagem; armazenar os dois separadamente evita esse problema.
static ICONES_DA_CLASSE: OnceLock<(isize, isize)> = OnceLock::new();

/// `BST_UNCHECKED` e `BST_CHECKED` do `winuser.h`. Escritos à mão pelo mesmo motivo que
/// `fontes.rs` escreve `MONITORINFOF_PRIMARY`: o crate `windows` 0.62 expõe as mensagens
/// (`BM_GETCHECK`, `BM_SETCHECK`) e **não** os dois valores que elas trocam.
const BST_UNCHECKED: usize = 0;
const BST_CHECKED: usize = 1;

/// `EM_SETLIMITTEXT` do `winuser.h` (0x00C5, o mesmo valor de `EM_LIMITTEXT`). Escrito à mão pela
/// mesma razão dos dois acima.
const EM_SETLIMITTEXT: u32 = 0x00C5;

/// `SS_OWNERDRAW` do `winuser.h` (o crate o põe em `System_SystemServices`, que este pacote não
/// liga). É o letreiro do PIN: um `STATIC` (não recebe foco nem clique) que se desenha no
/// `WM_DRAWITEM` e cujo texto é o que o Narrador lê.
const SS_OWNERDRAW: u32 = 0x0000_000D;

/// `IDOK` e `IDCANCEL`: o Enter e o Esc que o `IsDialogMessageW` manda como `WM_COMMAND`.
const ID_ENTER: usize = 1;
const ID_ESC: usize = 2;

/// O pedido de remontar a tela, **postado** (e não chamado na hora) pelos gestos que chegam no
/// meio de outra coisa: o `EN_KILLFOCUS` de um campo que o próprio `aplicar` escondeu chegaria com
/// o `aplicar` de fora ainda no meio (a revisão do ramo, 10).
const WM_ATUALIZAR: u32 = WM_APP + 1;
// A mensagem do ícone da bandeja (`bandeja::WM_BANDEJA`) é a `WM_APP + 2`: as duas chegam no mesmo
// `wndproc`, e uma não pode ser a outra.
const _: () = assert!(bandeja::WM_BANDEJA != WM_ATUALIZAR);

/// Quanto o segundo clique do Esquecer dos Ajustes tem para confirmar (§11.1).
const CONFIRMAR_POR: Duration = Duration::from_secs(5);

/// Quanto o cartão do teleprompter espera a janela aberta fechar antes de desistir: mais que o
/// fecho da tela R5 gravando (15 s da gravação, 6 do vídeo, 3 do microfone, 5 da câmera,
/// `teleprompter/tela_r5.rs`) e os 4 da sessão.
const FECHO_DO_TELEPROMPTER: Duration = Duration::from_secs(45);

/// Quanto o chip diz "Copiado".
const COPIADO_POR: Duration = Duration::from_secs(2);
/// De quanto em quanto tempo o endereço deste computador é perguntado de novo à tabela de rotas
/// (a linha "Na rede" do painel Espelhar).
const IP_A_CADA: Duration = Duration::from_secs(5);

/// O estilo da janela: **fixa** (§11.5), sem borda de redimensionar nem maximizar.
const ESTILO: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0);

struct App {
    emissor: Arc<Emissor>,
    receptor: Arc<Receptor>,
}

struct Janela {
    /// `None` no retrato de bancada: a janela desenha estados de exemplo, e nenhum gesto vai a
    /// lugar nenhum.
    app: Option<App>,
    /// O último estado montado. Tudo o que é desenhado sai daqui.
    modelo: EstadoDaTela,
    /// A composição do `modelo` (o que pintar e onde fica cada controle).
    quadro: modelo::Quadro,
    /// O item da barra que a pessoa escolheu (com sessão de pé, o da sessão).
    painel: Painel,
    controles: HashMap<Controle, HWND>,
    /// Os ladrilhos das origens, refeitos quando a lista de origens muda.
    ladrilhos: Vec<HWND>,
    /// O texto de cada controle, como foi posto (para não reescrever o mesmo a cada volta).
    textos: HashMap<Controle, String>,
    pintor: Option<estilo::d2d::Pintor>,
    /// A falha do desenho já foi dita no registro (uma vez só).
    falha_dita: bool,
    fonte_mono: HFONT,
    pincel_campo: HBRUSH,
    pincel_fundo: HBRUSH,
    dpi: u32,
    ultima_versao: u64,
    /// A versão do idioma da última volta: mudou (o seletor "PT | EN"), a janela reescreve tudo.
    versao_do_idioma: u32,
    /// A revisão da lista de aparelhos que o `LISTBOX` mostra. Remontar a cada 100 ms tiraria a
    /// escolha da mão de quem a está fazendo.
    revisao_dos_aparelhos: u64,
    /// A tela R5 estava aberta na última volta (o Espelhar desligado, com a frase).
    r5_aberta: bool,
    copiado_ate: Option<Instant>,
    /// O campo com o foco (a borda de baixo violeta).
    foco: Option<Controle>,
    ip: Option<String>,
    ip_lido_em: Option<Instant>,
    /// Um `WM_ATUALIZAR` já está na fila.
    atualizacao_pedida: bool,
    /// O controle com o foco quando a janela perdeu a ativação (volta no `WM_ACTIVATE`).
    foco_guardado: Option<HWND>,
    /// O primeiro clique no Esquecer dos Ajustes, até quando o segundo confirma.
    confirmar_esquecer_ate: Option<Instant>,
    /// O microfone está, na ordem do Tab, depois do Diário (a espera da câmera: chip, Esquecer,
    /// microfone, Gravar, Parar), e não depois do Som (o painel Espelhar).
    microfone_na_sessao: bool,
    /// O que não nasceu (o Direct2D, um controle): o retrato de bancada sai com "!!" por isto.
    falhas: Vec<String>,
    /// **As câmeras virtuais vivem aqui**, na thread da janela, porque `IMFVirtualCamera` não é
    /// `Send` e porque largar o objeto **remove o nó** (a vida é de sessão). Enquanto esta janela
    /// existir, as câmeras existem.
    cameras: Vec<crate::baia::CameraVirtual>,
    /// **O ícone na área de notificação** (01/10, `bandeja.rs`): minimizar esconde a janela nele, com
    /// as sessões vivas. `None` no retrato de bancada, que não tem app; soltá-lo tira o ícone.
    bandeja: Option<Bandeja>,
    /// **O driver da tela estendida** (02/10, noite): a situação lida por último
    /// (`driver_da_tela_estendida::ler_situacao`) e a versão do andamento da última volta.
    #[cfg(feature = "tela-estendida-futura")]
    situacao_do_driver: crate::regras_do_driver::Situacao,
    #[cfg(feature = "tela-estendida-futura")]
    versao_do_driver: u64,
}

// =============================================================================================
// Abrir e rodar
// =============================================================================================

/// Abre a janela e roda o laço de mensagens até ela fechar.
pub fn correr(emissor: Arc<Emissor>, receptor: Arc<Receptor>, cameras: Vec<crate::baia::CameraVirtual>) -> Result<()> {
    // **A procura começa com o app, não com o clique.** Se ela só subisse quando alguém abrisse
    // "Exibir", a lista estaria sempre vazia na hora exata em que alguém olha para ela — um
    // anúncio mDNS leva segundos para chegar.
    receptor.comecar_a_procurar();
    let com_cameras = emissor.argumentos.com_cameras();
    // Os números da exibição começam abertos numa corrida de bancada (§6.6).
    let detalhes = emissor.argumentos.modo_de_bancada();

    unsafe {
        let hwnd = criar_a_janela(CW_USEDEFAULT, CW_USEDEFAULT, WINDOW_EX_STYLE(0))?;
        // A marca da janela da instância de produto, logo que ela existe: é por ela que uma segunda
        // abertura pelo atalho a acha (`instancia.rs`), e não a janela de uma corrida de bancada ao lado.
        if crate::instancia::de_produto() {
            let _ = SetPropW(hwnd, crate::instancia::PROPRIEDADE, Some(windows::Win32::Foundation::HANDLE(1 as *mut core::ffi::c_void)));
        }
        let janela = Janela::nova(hwnd, Some(App { emissor, receptor }), cameras, detalhes);
        let ptr = Box::into_raw(janela);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
        (*ptr).atualizar(hwnd);
        // **O ícone na bandeja nasce antes de a janela aparecer**: um atalho "Executar: minimizada"
        // chega como minimização no primeiro `ShowWindow`, e ela já vai para a bandeja. Sem área de
        // notificação (a Sessão 0), a `Bandeja` fica não posta e o registro diz; a janela abre igual.
        let dica = regras_bandeja::dica(&(*ptr).modelo);
        (*ptr).bandeja = Some(Bandeja::nova(hwnd, &dica));

        let _ = ShowWindow(hwnd, SW_SHOW);
        // Para a frente só se ela ficou à vista: aberta minimizada (o atalho), ela já foi para a
        // bandeja no `WM_SIZE`, e uma janela escondida em primeiro plano tomaria o teclado de quem
        // está noutra coisa.
        if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
            let _ = SetForegroundWindow(hwnd);
        }
        // 100 ms: as threads de sessão publicam estado e a janela o lê aqui. Rápido o bastante
        // para o PIN aparecer "na hora" para um olho humano, devagar o bastante para não disputar
        // nada.
        SetTimer(Some(hwnd), ID_RELOGIO, 100, None);

        // **Câmera que entra ou sai recompõe a lista**, como o `WM_DISPLAYCHANGE` faz com os
        // monitores. Sempre, desde a fase 5; só o `--sem-cameras` (bancada) o dispensa, porque aí não há
        // câmera na lista para recompor. O registro vive o quanto a janela viver; o processo sai junto
        // com ela.
        if com_cameras {
            let filtro = DEV_BROADCAST_DEVICEINTERFACE_W {
                dbcc_size: std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
                dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
                dbcc_reserved: 0,
                dbcc_classguid: crate::cameras::KSCATEGORY_VIDEO_CAMERA,
                dbcc_name: [0],
            };
            let r = RegisterDeviceNotificationW(
                windows::Win32::Foundation::HANDLE(hwnd.0),
                &filtro as *const DEV_BROADCAST_DEVICEINTERFACE_W as *const core::ffi::c_void,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            );
            registro::linha(match r {
                Ok(_) => "câmeras: WM_DEVICECHANGE registrado em KSCATEGORY_VIDEO_CAMERA".to_string(),
                Err(e) => format!("câmeras: o registro do WM_DEVICECHANGE falhou ({e}); a lista só se recompõe na volta à tela inicial"),
            });
        }

        // **O adaptador do SudoVDA que aparece ou some recompõe a lista** (02/10, noite; o Bruno
        // desligou o adaptador no Gerenciador, religou, e o ladrilho só voltou fechando o Quall):
        // ligar e desligar o dispositivo publica e retira a interface de controle dele, e a
        // chegada e a saída da interface viram `WM_DEVICECHANGE`. Sempre, com ou sem câmeras.
        #[cfg(feature = "tela-estendida-futura")]
        {
            let filtro = DEV_BROADCAST_DEVICEINTERFACE_W {
                dbcc_size: std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
                dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
                dbcc_reserved: 0,
                dbcc_classguid: crate::sudovda::INTERFACE,
                dbcc_name: [0],
            };
            let r = RegisterDeviceNotificationW(
                windows::Win32::Foundation::HANDLE(hwnd.0),
                &filtro as *const DEV_BROADCAST_DEVICEINTERFACE_W as *const core::ffi::c_void,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            );
            registro::linha(match r {
                Ok(_) => "driver: WM_DEVICECHANGE registrado na interface do SudoVDA".to_string(),
                Err(e) => format!("driver: !! o registro do WM_DEVICECHANGE do SudoVDA falhou ({e}); o ladrilho só se refaz na volta aos painéis"),
            });
        }

        // `IsDialogMessageW` primeiro: é ele que dá o Tab e o Shift+Tab entre os controles, as
        // setas dentro dos grupos (ladrilhos, volume, itens da barra), o Enter (a ação principal do
        // painel) e o Esc (Cancelar/Parar), sem a janela ser um diálogo.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if IsWindow(Some(hwnd)).as_bool() && IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// Registra a classe e cria a janela escura, com a área de cliente de 880 × 580 DIP no DPI dela.
unsafe fn criar_a_janela(x: i32, y: i32, estendido: WINDOW_EX_STYLE) -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let classe = CLASSE;
        let &(grande, pequeno) = ICONES_DA_CLASSE.get_or_init(|| {
        let dpi = GetDpiForSystem().max(96);
        let icone = |l, a| {
            // Recurso 1: o mesmo quall.ico do Explorer e da bandeja, em dois tamanhos.
            // Os ícones acompanham a classe e são liberados pelo Windows ao terminar o processo.
            match LoadImageW(Some(hinstance.into()), PCWSTR(1usize as *const u16), IMAGE_ICON, l, a, LR_DEFAULTCOLOR) {
                Ok(h) if !h.is_invalid() => h.0 as isize,
                resultado => {
                    registro::linha(format!("janela: !! o ícone principal não carregou ({l}x{a}): {resultado:?}"));
                    LoadIconW(None, IDI_APPLICATION).unwrap_or_default().0 as isize
                }
            }
        };
        (icone(GetSystemMetricsForDpi(SM_CXICON, dpi), GetSystemMetricsForDpi(SM_CYICON, dpi)),
         icone(GetSystemMetricsForDpi(SM_CXSMICON, dpi), GetSystemMetricsForDpi(SM_CYSMICON, dpi)))
        });
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: classe,
            hIcon: HICON(grande as *mut core::ffi::c_void),
            hIconSm: HICON(pequeno as *mut core::ffi::c_void),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(estendido, classe, w!("Quall Monitor"), ESTILO, x, y, 900, 620, None, None, Some(hinstance.into()), None)?; // i18n: fora (nome do produto)
        // O tamanho só depois de a janela existir: o DPI que vale é o do monitor em que ela nasceu.
        ajustar_o_tamanho(hwnd, GetDpiForWindow(hwnd).max(96), None);
        escurecer_a_barra_de_titulo(hwnd);
        Ok(hwnd)
    }
}

/// A área de cliente de 880 × 580 DIP no `dpi` dado. `sugerido` é o retângulo do `WM_DPICHANGED`
/// (a posição dele vale; o tamanho é refeito aqui, para a conta fechar em DIP). A moldura é a do
/// DPI da janela, que no `WM_DPICHANGED` já é o novo — e que no retrato a 150 % continua o do
/// monitor (o DPI do conteúdo é forçado, o da moldura não).
fn ajustar_o_tamanho(hwnd: HWND, dpi: u32, sugerido: Option<RECT>) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: escala(dpi, estilo::LARGURA_MINIMA),
        bottom: escala(dpi, estilo::ALTURA_MINIMA),
    };
    unsafe {
        let da_moldura = GetDpiForWindow(hwnd).max(96);
        // O estilo estendido de verdade: a janela do retrato é `WS_EX_TOOLWINDOW`, de barra de título
        // mais baixa, e a conta com 0 a deixava fora de 880 × 580 (a revisão do ramo, 12).
        let estendido = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
        let _ = AdjustWindowRectExForDpi(&mut r, ESTILO, false, estendido, da_moldura);
        let (x, y, bandeiras) = match sugerido {
            Some(s) => (s.left, s.top, SWP_NOZORDER | SWP_NOACTIVATE),
            None => (0, 0, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE),
        };
        let _ = SetWindowPos(hwnd, None, x, y, r.right - r.left, r.bottom - r.top, bandeiras);
    }
}

/// **A barra de título escura**: o atributo 20 do DWM (o 19 nos Windows 10 anteriores ao build
/// 18985), e no Windows 11 a cor da legenda igual à da barra lateral e o texto claro. O Dell é
/// Windows 11 25H2; o recuo do Windows 10 fica aqui sem prova (§11.5).
pub(crate) fn escurecer_a_barra_de_titulo(hwnd: HWND) {
    let sim: i32 = 1;
    let tamanho = std::mem::size_of::<i32>() as u32;
    unsafe {
        let p = &sim as *const i32 as *const core::ffi::c_void;
        if DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, p, tamanho).is_err() {
            let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(19), p, tamanho);
        }
        let legenda = estilo::BARRA.colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &legenda as *const u32 as *const core::ffi::c_void, 4);
        let tinta = estilo::TEXTO.colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &tinta as *const u32 as *const core::ffi::c_void, 4);
    }
}

fn escala(dpi: u32, v: f32) -> i32 {
    (v * dpi as f32 / 96.0).round() as i32
}

/// Um retângulo em DIP para pixels, arredondando as duas bordas (e não a largura), para dois
/// vizinhos nunca se cobrirem nem deixarem fresta.
fn em_pixels(dpi: u32, r: Ret) -> RECT {
    RECT { left: escala(dpi, r.x), top: escala(dpi, r.y), right: escala(dpi, r.x + r.l), bottom: escala(dpi, r.y + r.a) }
}

fn fonte_gdi(dpi: u32, familia: &str, tamanho_dip: f32) -> HFONT {
    let nome: Vec<u16> = familia.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateFontW(
            -escala(dpi, tamanho_dip),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (FF_DONTCARE.0 | FIXED_PITCH.0) as u32,
            PCWSTR(nome.as_ptr()),
        )
    }
}

fn largo(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// O texto de um `EDIT`, como `String`.
///
/// 128 caracteres bastam para os dois campos deste app (um endereço IPv6 com porta cabe em ~50, e
/// o PIN está limitado a 6 pelo próprio controle). O corte é silencioso de propósito: um endereço
/// mais longo que isso já não seria um endereço.
fn texto_de(hwnd: HWND) -> String {
    let mut buffer = [0u16; 128];
    let n = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..n as usize])
}

fn marcado(h: HWND) -> bool {
    unsafe { SendMessageW(h, BM_GETCHECK, None, None) }.0 as usize == BST_CHECKED
}

fn marcar(h: HWND, sim: bool) {
    unsafe {
        SendMessageW(h, BM_SETCHECK, Some(WPARAM(if sim { BST_CHECKED } else { BST_UNCHECKED })), None);
    }
}

// =============================================================================================
// Criar os controles
// =============================================================================================

unsafe fn criar_controle(pai: HWND, c: Controle) -> Result<HWND> {
    let (classe, proprio) = match c.especie() {
        Especie::Botao => (w!("BUTTON"), BS_PUSHBUTTON as u32),
        Especie::Alternar => (w!("BUTTON"), (BS_AUTOCHECKBOX | BS_PUSHLIKE) as u32),
        Especie::Opcao => (w!("BUTTON"), (BS_AUTORADIOBUTTON | BS_PUSHLIKE) as u32),
        // `LBS_NOTIFY` para o `LBN_SELCHANGE` chegar; `LBS_NOINTEGRALHEIGHT` para a lista ter a
        // altura da tabela, e não a arredondada às linhas.
        Especie::Lista => (
            w!("LISTBOX"),
            (LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT) as u32 | WS_VSCROLL.0,
        ),
        Especie::Campo => (w!("EDIT"), ES_AUTOHSCROLL as u32),
        Especie::Letreiro => (w!("STATIC"), SS_OWNERDRAW),
    };
    let mut estilo = WS_CHILD.0 | proprio;
    if c.especie() != Especie::Letreiro {
        estilo |= WS_TABSTOP.0;
    }
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

impl Janela {
    unsafe fn nova(hwnd: HWND, app: Option<App>, cameras: Vec<crate::baia::CameraVirtual>, detalhes: bool) -> Box<Janela> {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let mut falhas = Vec::new();
        let pintor = match estilo::d2d::Pintor::novo() {
            Ok(p) => Some(p),
            Err(e) => {
                let f = format!("janela: !! o Direct2D não subiu ({e}); os botões ficam com a cara do sistema"); // i18n: fora (diário)
                registro::linha(&f);
                falhas.push(f);
                None
            }
        };
        let familia_mono = pintor.as_ref().map(|p| p.familia(estilo::Familia::Mono).to_string()).unwrap_or_else(|| "Consolas".into());
        let mut controles = HashMap::new();
        for c in Controle::fixos() {
            match unsafe { criar_controle(hwnd, c) } {
                Ok(h) => {
                    controles.insert(c, h);
                }
                Err(e) => {
                    let f = format!("janela: !! o controle {c:?} não nasceu: {e}"); // i18n: fora (diário)
                    registro::linha(&f);
                    falhas.push(f);
                }
            }
        }
        // O endereço desta máquina só com o app: o retrato não toca a pilha de rede.
        let ip = if app.is_some() { crate::enderecos::ip_local().map(|ip| ip.to_string()) } else { None };
        let mut j = Box::new(Janela {
            app,
            modelo: EstadoDaTela::default(),
            quadro: modelo::Quadro::default(),
            painel: Painel::Espelhar,
            controles,
            ladrilhos: Vec::new(),
            textos: HashMap::new(),
            pintor,
            falha_dita: false,
            fonte_mono: fonte_gdi(dpi, &familia_mono, 15.0),
            pincel_campo: unsafe { CreateSolidBrush(windows::Win32::Foundation::COLORREF(estilo::SUPERFICIE.colorref())) },
            pincel_fundo: unsafe { CreateSolidBrush(windows::Win32::Foundation::COLORREF(estilo::FUNDO.colorref())) },
            dpi,
            ultima_versao: u64::MAX,
            versao_do_idioma: crate::idioma::versao(),
            revisao_dos_aparelhos: u64::MAX,
            r5_aberta: false,
            copiado_ate: None,
            foco: None,
            ip,
            ip_lido_em: Some(Instant::now()),
            atualizacao_pedida: false,
            foco_guardado: None,
            confirmar_esquecer_ate: None,
            microfone_na_sessao: false,
            falhas,
            cameras,
            bandeja: None,
            #[cfg(feature = "tela-estendida-futura")]
            situacao_do_driver: Default::default(),
            #[cfg(feature = "tela-estendida-futura")]
            versao_do_driver: 0,
        });
        j.reler_o_driver("abertura");
        unsafe {
            // Seis dígitos, e o controle recusa o sétimo em vez de deixar digitar e falhar no
            // `Pin::parse` depois.
            if let Some(pin) = j.h(Controle::Pin) {
                SendMessageW(pin, EM_SETLIMITTEXT, Some(WPARAM(6)), None);
            }
            // A barra de rolagem escura na lista (Windows 10 1809 em diante), quando os aparelhos
            // passam de três.
            if let Some(lista) = j.h(Controle::Lista) {
                let _ = SetWindowTheme(lista, w!("DarkMode_Explorer"), PCWSTR::null());
            }
            if let Some(d) = j.h(Controle::Detalhes) {
                marcar(d, detalhes);
            }
            // O anel de foco só depois de o teclado ser usado (o jeito do Windows): começa
            // escondido, e o Tab do `IsDialogMessageW` o mostra. Sem isto, numa janela que não é
            // diálogo, o estado da interface fica indefinido (a revisão do ramo, 5).
            SendMessageW(hwnd, WM_CHANGEUISTATE, Some(WPARAM(((UISF_HIDEFOCUS << 16) | UIS_SET) as usize)), None);
        }
        j.dar_nome_aos_campos();
        j.aplicar_dpi();
        j
    }

    /// **O nome dos campos e da lista para o Narrador** (um `EDIT` e um `LISTBOX` não têm texto
    /// próprio que diga o que são): pelo `IAccPropServices`, com os nomes de `texto_acessivel`. E a
    /// dica cinza de dentro dos dois campos (`EM_SETCUEBANNER`). No idioma de agora: o `aplicar` a
    /// chama de novo quando os textos postos foram esquecidos (a troca de idioma, o retrato).
    fn dar_nome_aos_campos(&mut self) {
        unsafe {
            if let Some(pin) = self.h(Controle::Pin) {
                let dica = largo(t("se for a 1ª vez"));
                SendMessageW(pin, EM_SETCUEBANNER, Some(WPARAM(0)), Some(LPARAM(dica.as_ptr() as isize)));
            }
            if let Some(endereco) = self.h(Controle::Endereco) {
                let dica = largo(t("ex.: 192.168.0.12:7877"));
                SendMessageW(endereco, EM_SETCUEBANNER, Some(WPARAM(0)), Some(LPARAM(dica.as_ptr() as isize)));
            }
        }
        let servicos: Option<IAccPropServices> = unsafe { CoCreateInstance(&CLSID_AccPropServices, None, CLSCTX_INPROC_SERVER).ok() };
        let Some(s) = servicos else {
            registro::linha("janela: o IAccPropServices não veio; os campos ficam sem nome para o Narrador");
            return;
        };
        for c in [Controle::Endereco, Controle::Pin, Controle::Lista] {
            let Some(h) = self.h(c) else { continue };
            let nome = largo(&modelo::texto_acessivel(c, &self.modelo));
            unsafe {
                let _ = s.SetHwndPropStr(h, OBJID_CLIENT.0 as u32, CHILDID_SELF, PROPID_ACC_NAME, PCWSTR(nome.as_ptr()));
            }
        }
    }

    /// Pede a remontagem da tela pela fila de mensagens (uma só na fila de cada vez).
    fn pedir_atualizacao(&mut self, hwnd: HWND) {
        if self.atualizacao_pedida {
            return;
        }
        self.atualizacao_pedida = true;
        unsafe {
            if PostMessageW(Some(hwnd), WM_ATUALIZAR, WPARAM(0), LPARAM(0)).is_err() {
                self.atualizacao_pedida = false;
            }
        }
    }

    fn h(&self, c: Controle) -> Option<HWND> {
        match c {
            Controle::Ladrilho(i) => self.ladrilhos.get(i).copied(),
            _ => self.controles.get(&c).copied(),
        }
    }

    /// O que depende do DPI nos controles nativos: a fonte dos campos e a altura da linha da lista.
    fn aplicar_dpi(&mut self) {
        unsafe {
            for c in [Controle::Endereco, Controle::Pin, Controle::Lista] {
                if let Some(h) = self.h(c) {
                    SendMessageW(h, WM_SETFONT, Some(WPARAM(self.fonte_mono.0 as usize)), Some(LPARAM(1)));
                }
            }
            if let Some(lista) = self.h(Controle::Lista) {
                SendMessageW(lista, LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(escala(self.dpi, lugar::LINHA_DA_LISTA) as isize)));
            }
        }
    }

    fn mudar_dpi(&mut self, hwnd: HWND, novo: u32, sugerido: RECT) {
        self.dpi = novo.max(96);
        let familia = self.pintor.as_ref().map(|p| p.familia(estilo::Familia::Mono).to_string()).unwrap_or_else(|| "Consolas".into());
        let velha = self.fonte_mono;
        self.fonte_mono = fonte_gdi(self.dpi, &familia, 15.0);
        self.aplicar_dpi();
        unsafe {
            let _ = DeleteObject(velha.into());
        }
        ajustar_o_tamanho(hwnd, self.dpi, Some(sugerido));
        self.aplicar(hwnd);
    }
}

// =============================================================================================
// O procedimento da janela
// =============================================================================================

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // A altura da linha da lista é pedida **dentro** do `CreateWindowExW` dela, antes de a janela
    // ter o ponteiro: responde-se aqui, pelo DPI da janela.
    if msg == WM_MEASUREITEM {
        let m = unsafe { &mut *(lp.0 as *mut MEASUREITEMSTRUCT) };
        if m.CtlType == ODT_LISTBOX {
            m.itemHeight = escala(unsafe { GetDpiForWindow(hwnd) }.max(96), lugar::LINHA_DA_LISTA) as u32;
            return LRESULT(1);
        }
    }
    let ponteiro = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Janela;
    if ponteiro.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let janela = unsafe { &mut *ponteiro };

    // **O Explorer recriou a barra de tarefas** (reiniciou, ou subiu depois do app): o ícone volta.
    // `TaskbarCreated` é mensagem registrada, e o número dela só existe em tempo de execução — não
    // cabe num braço do `match`.
    if let Some(b) = janela.bandeja.as_mut() {
        if b.msg_da_barra() != 0 && msg == b.msg_da_barra() {
            b.barra_recriada();
            return LRESULT(0);
        }
        // **O Quall aberto de novo pelo atalho** (`instancia.rs`): a outra abertura sai sem abrir
        // nada e pede a janela desta, que volta como no clique no ícone — da bandeja ou não.
        if b.msg_mostrar() != 0 && msg == b.msg_mostrar() {
            registro::linha("instância única: outra abertura pediu a janela; ela volta");
            bandeja::restaurar(hwnd);
            return LRESULT(0);
        }
    }

    match msg {
        WM_TIMER => {
            janela.pulsar(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            janela.comando(hwnd, wp);
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = unsafe { &*(lp.0 as *const NMHDR) };
            if hdr.code == NM_CUSTOMDRAW {
                if let Some(c) = Controle::de_id(hdr.idFrom) {
                    // Sem o Direct2D, o botão se desenha como o do sistema: feio, mas à vista.
                    if matches!(c.especie(), Especie::Botao | Especie::Alternar | Especie::Opcao) && janela.pintor.is_some() {
                        let cd = unsafe { &*(lp.0 as *const NMCUSTOMDRAW) };
                        // No `PREERASE` também: devolver o padrão ali faz o botão com tema apagar o
                        // fundo pelo `DrawThemeParentBackground`, que pede à janela inteira um
                        // `WM_PRINTCLIENT` com a cena toda — por botão, a cada redesenho (a revisão
                        // do ramo, 7). O desenho é o mesmo nas duas etapas, e cobre o botão todo.
                        if cd.dwDrawStage == CDDS_PREERASE || cd.dwDrawStage == CDDS_PREPAINT {
                            janela.desenhar_controle(c, cd);
                            return LRESULT(CDRF_SKIPDEFAULT as isize);
                        }
                        return LRESULT(CDRF_DODEFAULT as isize);
                    }
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_DRAWITEM => {
            let d = unsafe { &*(lp.0 as *const DRAWITEMSTRUCT) };
            janela.desenhar_item(d);
            LRESULT(1)
        }
        WM_ATUALIZAR => {
            janela.atualizacao_pedida = false;
            janela.atualizar(hwnd);
            LRESULT(0)
        }
        // **O foco volta para onde estava** quando a janela é ativada de novo (a revisão do ramo,
        // 16): sem isto, o `DefWindowProcW` o põe na própria janela, e o Tab recomeça do primeiro.
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                let f = unsafe { GetFocus() };
                janela.foco_guardado = (!f.is_invalid() && unsafe { IsChild(hwnd, f) }.as_bool()).then_some(f);
                return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
            }
            if let Some(f) = janela.foco_guardado {
                let pode = unsafe { IsWindow(Some(f)).as_bool() && IsWindowVisible(f).as_bool() && IsWindowEnabled(f).as_bool() };
                if pode {
                    unsafe {
                        let _ = SetFocus(Some(f));
                    }
                    return LRESULT(0);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_DPICHANGED => {
            let novo = ((wp.0 >> 16) & 0xFFFF) as u32;
            let sugerido = unsafe { *(lp.0 as *const RECT) };
            janela.mudar_dpi(hwnd, novo, sugerido);
            LRESULT(0)
        }
        // **Um monitor entrou ou saiu.** Sem isto a lista era montada uma vez, na abertura.
        WM_DISPLAYCHANGE => {
            if let Some(app) = &janela.app {
                app.emissor.recarregar_fontes();
            }
            janela.pulsar(hwnd);
            LRESULT(0)
        }
        // **Uma câmera entrou ou saiu** (só chega sem o `--sem-cameras`, que dispensa o registro da
        // classe). A lista é refeita inteira, e a escolha que sumiu fica sem escolha, com o aviso na
        // tela. **Fora da thread da janela** (a revisão do código da fase 5, M2): a enumeração das
        // câmeras não pode congelar a janela; a lista nova aparece no pulso seguinte.
        WM_DEVICECHANGE => {
            let evento = wp.0 as u32;
            if (evento == DBT_DEVICEARRIVAL || evento == DBT_DEVICEREMOVECOMPLETE) && lp.0 != 0 {
                let cabecalho = unsafe { &*(lp.0 as *const DEV_BROADCAST_HDR) };
                #[cfg(not(feature = "tela-estendida-futura"))]
                let do_sudovda = false;
                #[cfg(feature = "tela-estendida-futura")]
                let do_sudovda = cabecalho.dbch_devicetype == DBT_DEVTYP_DEVICEINTERFACE
                    && unsafe { (*(lp.0 as *const DEV_BROADCAST_DEVICEINTERFACE_W)).dbcc_classguid } == crate::sudovda::INTERFACE;
                if do_sudovda {
                    #[cfg(feature = "tela-estendida-futura")]
                    {
                    // O adaptador do SudoVDA (ligado, desligado, instalado, tirado): relê a situação
                    // do driver e recompõe a lista, fora da thread da janela.
                    registro::linha(format!(
                        "driver: WM_DEVICECHANGE ({}) na interface do SudoVDA — relendo o driver e a lista",
                        if evento == DBT_DEVICEARRIVAL { "chegou" } else { "saiu" }
                    ));
                    janela.reler_o_driver("WM_DEVICECHANGE do SudoVDA"); // i18n: fora (diário)
                    if let Some(app) = &janela.app {
                        app.emissor.recarregar_fontes_fora("WM_DEVICECHANGE do SudoVDA"); // i18n: fora (diário)
                    }
                    janela.pulsar(hwnd);
                    }
                } else if cabecalho.dbch_devicetype == DBT_DEVTYP_DEVICEINTERFACE {
                    registro::linha(format!(
                        "câmeras: WM_DEVICECHANGE ({}) — recompondo a lista fora da thread da janela",
                        if evento == DBT_DEVICEARRIVAL { "entrou" } else { "saiu" }
                    ));
                    if let Some(app) = &janela.app {
                        app.emissor.recarregar_fontes_fora("WM_DEVICECHANGE");
                    }
                }
            }
            LRESULT(1)
        }
        // Os campos escuros: o `EDIT` pinta o próprio fundo com o pincel que voltar daqui, e o
        // texto com a cor posta no DC.
        WM_CTLCOLOREDIT => unsafe {
            let dc = HDC(wp.0 as *mut core::ffi::c_void);
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(estilo::TEXTO.colorref()));
            SetBkColor(dc, windows::Win32::Foundation::COLORREF(estilo::SUPERFICIE.colorref()));
            LRESULT(janela.pincel_campo.0 as isize)
        },
        WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            let dc = HDC(wp.0 as *mut core::ffi::c_void);
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(estilo::TEXTO.colorref()));
            SetBkColor(dc, windows::Win32::Foundation::COLORREF(estilo::FUNDO.colorref()));
            LRESULT(janela.pincel_fundo.0 as isize)
        },
        WM_ERASEBKGND => LRESULT(1), // o WM_PAINT pinta o fundo inteiro; apagar antes só pisca
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            janela.pintar(hwnd, hdc);
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        // O retrato de bancada (`WM_PRINT` com `PRF_CLIENT`) chega aqui: a janela se desenha no DC
        // que veio.
        WM_PRINTCLIENT => {
            janela.pintar(hwnd, HDC(wp.0 as *mut core::ffi::c_void));
            LRESULT(0)
        }
        // **Minimizar vai para a bandeja** (01/10): o botão de minimizar, o Alt+Espaço › Minimizar.
        // Interceptado aqui, a janela se esconde sem a animação de minimizar e sem nunca ficar
        // minimizada. Sem o ícone posto, o minimizar de sempre.
        WM_SYSCOMMAND => {
            if (wp.0 & 0xFFF0) as u32 == SC_MINIMIZE && janela.esconder_na_bandeja(hwnd) {
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // As minimizações que não passam pelo `WM_SYSCOMMAND` (o Win+M, um atalho "Executar:
        // minimizada", e o que mais minimizar a janela por fora): ela já está minimizada, e se
        // esconde em seguida. O `bandeja::restaurar` a desminimiza.
        WM_SIZE => {
            if wp.0 as u32 == SIZE_MINIMIZED {
                janela.esconder_na_bandeja(hwnd);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        // **O ícone da bandeja**: clique, tecla, menu, aviso.
        bandeja::WM_BANDEJA => {
            let versao_4 = janela.bandeja.as_ref().is_some_and(|b| b.versao_4());
            let ponto = match regras_bandeja::evento(wp.0, lp.0, versao_4) {
                Evento::Abrir => {
                    bandeja::restaurar(hwnd);
                    return LRESULT(0);
                }
                Evento::AvisoMostrado => {
                    registro::linha("bandeja: o aviso da primeira vez apareceu (NIN_BALLOONSHOW)");
                    return LRESULT(0);
                }
                Evento::Nada => return LRESULT(0),
                Evento::Menu { x, y } => Some((x, y)),
                Evento::MenuNoCursor => None,
            };
            // A linha é lida **antes** do menu: o laço modal dele despacha o relógio, e o
            // `--sair-apos` pode destruir a janela (e soltar a `Janela`) lá dentro. Depois do menu,
            // só o `hwnd`.
            let linha = regras_bandeja::linha_do_menu(&janela.modelo);
            match bandeja::abrir_menu(hwnd, ponto, &linha) {
                Escolha::Abrir => bandeja::restaurar(hwnd),
                // "Sair do Quall" é o X: o mesmo `WM_CLOSE` logo abaixo, pela fila (ele destrói a
                // janela, e este braço ainda está no meio).
                Escolha::Sair => unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                },
                Escolha::Nada => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // Fechar a janela **encerra as sessões de verdade**, não as esconde — o X, o Alt+F4 e o
            // "Sair do Quall" do menu da bandeja, que chega aqui. Desde 01/10 a bandeja existe
            // (`bandeja.rs`), mas só o **minimizar** vai para ela: o X continua fechando, por decisão
            // do dono. Quem quer o Quall fora do caminho com as sessões vivas minimiza; o X que
            // escondesse deixaria uma transmissão (ou uma exibição) viva para quem achou que tinha
            // fechado.
            if let Some(app) = &janela.app {
                app.emissor.encerrar();
                app.receptor.encerrar();
                app.receptor.busca.parar();
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
            };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), ID_RELOGIO);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let mut j = Box::from_raw(ponteiro);
                // O ícone sai da área de notificação aqui, com a janela ainda válida (o `Drop` da
                // `Bandeja` faz o `NIM_DELETE`): sem isto ficaria um fantasma até o mouse passar.
                drop(j.bandeja.take());
                let _ = RemovePropW(hwnd, crate::instancia::PROPRIEDADE);
                let _ = DeleteObject(j.fonte_mono.into());
                let _ = DeleteObject(j.pincel_campo.into());
                let _ = DeleteObject(j.pincel_fundo.into());
                drop(j);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

// =============================================================================================
// O relógio, os gestos e o estado da tela
// =============================================================================================

impl Janela {
    fn pulsar(&mut self, hwnd: HWND) {
        let Some(app) = &self.app else { return };
        // A lista de mDNS é copiada da thread de navegação para o estado publicado aqui — é o que
        // faz a janela ler **uma** trava por pulso em vez de duas.
        app.receptor.pulsar();

        // **O nó da câmera de um aparelho recém-pareado nasce aqui**, e não na thread da sessão:
        // o objeto COM tem de ser criado por quem vai guardá-lo. Ver `baias::atender_pendentes`.
        app.receptor.baias.atender_pendentes(&mut self.cameras);

        let (versao_e, sair) = {
            let e = app.emissor.estado();
            (e.versao, e.sair)
        };
        let versao_r = app.receptor.estado().versao;
        if sair {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        }
        let mut sujo = false;
        // O idioma trocou (o seletor, aqui): todos os textos postos são reescritos na volta.
        if crate::idioma::versao() != self.versao_do_idioma {
            self.versao_do_idioma = crate::idioma::versao();
            self.textos.clear();
            // As linhas da lista levam o tipo ("Tela ou câmera"): remontadas no idioma novo.
            self.revisao_dos_aparelhos = u64::MAX;
            sujo = true;
        }
        let r5 = crate::dono_da_captura::tela_r5_aberta();
        if r5 != self.r5_aberta {
            // A versão do emissor não muda com a tela R5: o redesenho é pedido aqui.
            self.r5_aberta = r5;
            sujo = true;
        }
        if self.copiado_ate.is_some_and(|t| Instant::now() >= t) {
            self.copiado_ate = None;
            sujo = true;
        }
        if self.confirmar_esquecer_ate.is_some_and(|t| Instant::now() >= t) {
            self.confirmar_esquecer_ate = None;
            sujo = true;
        }
        if aviso_do_cartao() != self.modelo.aviso_do_teleprompter {
            sujo = true;
        }
        // O andamento do driver mudou (um passo, o fim): redesenha; no fim, relê a situação e a
        // lista de fontes — depois de instalar pelo botão, o ladrilho fica escolhível na hora.
        let mut reler_o_driver = false;
        #[cfg(feature = "tela-estendida-futura")]
        {
        let vd = crate::driver_da_tela_estendida::versao();
        if vd != self.versao_do_driver {
            self.versao_do_driver = vd;
            sujo = true;
            if matches!(crate::driver_da_tela_estendida::andamento(), crate::regras_do_driver::Andamento::Acabou { .. }) {
                reler_o_driver = true;
                app.emissor.recarregar_fontes_fora("driver da tela estendida"); // i18n: fora (diário)
            }
        }
        }
        // A janela do teleprompter abriu, fechou ou trocou de papel: nenhuma versão muda com isso, e a
        // dica da bandeja a diz.
        if teleprompter_aberto() != self.modelo.teleprompter {
            sujo = true;
        }
        if self.ip_lido_em.is_none_or(|t| t.elapsed() >= IP_A_CADA) {
            self.ip_lido_em = Some(Instant::now());
            let ip = crate::enderecos::ip_local().map(|ip| ip.to_string());
            if ip != self.ip {
                self.ip = ip;
                sujo = true;
            }
        }
        if reler_o_driver {
            self.reler_o_driver("fim da instalação ou desinstalação"); // i18n: fora (diário)
        }
        // Uma versão só, somada: a janela não precisa saber **qual** metade mudou para redesenhar.
        let versao = versao_e.wrapping_add(versao_r);
        if versao == self.ultima_versao && !sujo {
            return;
        }
        self.ultima_versao = versao;
        self.atualizar(hwnd);
    }

    /// Monta o estado (do emissor e do receptor, ou o de exemplo já posto) e o aplica.
    fn atualizar(&mut self, hwnd: HWND) {
        if self.app.is_some() {
            self.modelo = self.montar();
            // Com uma sessão de pé, o item dela fica escolhido — e é nele que a pessoa volta
            // quando a sessão acaba.
            if self.modelo.sessao.is_some() {
                self.painel = self.modelo.painel;
            }
        }
        self.aplicar(hwnd);
        // A dica do ícone sai do mesmo estado; a `Bandeja` só fala com o Explorer quando ela muda.
        if let Some(b) = self.bandeja.as_mut() {
            b.mudar_dica(&regras_bandeja::dica(&self.modelo));
        }
    }

    /// **Minimizar vai para a bandeja**: esconde a janela (sai da barra de tarefas) e, na primeira
    /// vez, o ícone avisa onde o Quall foi parar. Nada é encerrado. `false` sem o ícone posto: aí o
    /// minimizar é o de sempre, porque esconder seria sumir sem volta.
    fn esconder_na_bandeja(&mut self, hwnd: HWND) -> bool {
        let Some(b) = self.bandeja.as_mut() else { return false };
        match regras_bandeja::ao_minimizar(b.posta(), b.ja_avisou()) {
            AoMinimizar::Minimizar => false,
            AoMinimizar::Esconder { avisar } => {
                bandeja::esconder(hwnd);
                registro::linha("bandeja: a janela foi para a bandeja; as sessões seguem");
                if avisar {
                    b.avisar();
                }
                true
            }
        }
    }

    fn comando(&mut self, hwnd: HWND, wp: WPARAM) {
        let id = wp.0 & 0xFFFF;
        let aviso = ((wp.0 >> 16) & 0xFFFF) as u32;
        // O Enter e o Esc do `IsDialogMessageW`. O Enter com o foco num botão chega como o clique
        // dele; o `IDOK` chega com o foco em qualquer outra coisa (um ladrilho, um item da barra),
        // e só vale nos campos do Exibir: o Enter no PIN conecta, e num ladrilho não espelha nada
        // (a revisão do ramo, 16).
        if id == ID_ENTER && aviso == 0 {
            let foco = unsafe { GetFocus() };
            let num_campo = [Controle::Endereco, Controle::Pin].iter().any(|c| self.h(*c) == Some(foco));
            if num_campo && self.modelo.cena == Cena::Painel(Painel::Exibir) {
                self.gesto(hwnd, Controle::Exibir);
            }
            return;
        }
        if id == ID_ESC && aviso == 0 {
            if self.modelo.sessao.is_some() {
                self.gesto(hwnd, Controle::Cancelar);
            }
            return;
        }
        let Some(c) = Controle::de_id(id) else { return };
        match (c, aviso) {
            // O traço violeta do campo com o foco: o estado é remontado pela fila, e não aqui — o
            // `EN_KILLFOCUS` chega também de dentro do `aplicar`, quando ele esconde o campo.
            (Controle::Endereco | Controle::Pin, EN_SETFOCUS) => {
                self.foco = Some(c);
                self.pedir_atualizacao(hwnd);
            }
            (Controle::Endereco | Controle::Pin, EN_KILLFOCUS) => {
                if self.foco == Some(c) {
                    self.foco = None;
                }
                self.pedir_atualizacao(hwnd);
            }
            (Controle::Lista, LBN_SELCHANGE) => {
                let Some(h) = self.h(Controle::Lista) else { return };
                let i = unsafe { SendMessageW(h, LB_GETCURSEL, None, None) };
                if i.0 >= 0 {
                    if let Some(app) = &self.app {
                        app.receptor.escolher_aparelho(i.0 as usize);
                    }
                }
            }
            (Controle::Lista, LBN_DBLCLK) => {
                // Clicar duas vezes num nome é o gesto que a lista já ensina em toda parte.
                let pin = self.h(Controle::Pin).map(texto_de).unwrap_or_default();
                if let Some(app) = &self.app {
                    if app.emissor.estado().fase == Fase::Inicial {
                        app.receptor.exibir(String::new(), pin);
                    }
                }
            }
            // **Uma opção só age marcada.** O `BUTTON` manda `BN_CLICKED` também quando uma opção
            // desmarcada **ganha o foco** (o Tab que passa por ela, e o próprio clique, que chega
            // duas vezes): sem esta guarda, o Tab pela barra trocava o painel, pelos ladrilhos
            // trocava a origem, e pelo volume o levava a 25 % (a revisão do ramo, 1).
            (_, BN_CLICKED) if c.especie() == Especie::Opcao => {
                if self.h(c).is_some_and(marcado) {
                    self.gesto(hwnd, c);
                }
            }
            (_, BN_CLICKED) => self.gesto(hwnd, c),
            _ => {}
        }
    }

    /// **Os gestos.** A lógica por trás de cada um é a de antes desta rodada, sem mudança: só o
    /// controle que a dispara mudou de cara.
    fn gesto(&mut self, hwnd: HWND, c: Controle) {
        // Os gestos da própria janela (valem também no retrato, sem app).
        match c {
            Controle::Item(p) => {
                if p == Painel::Teleprompter { return; }
                if modelo::habilitado(c, &self.modelo) {
                    self.painel = p;
                    self.confirmar_esquecer_ate = None;
                    if p == Painel::Ajustes {
                        self.reler_o_driver("Ajustes aberto"); // i18n: fora (diário)
                    }
                    self.pedir_atualizacao(hwnd);
                }
                return;
            }
            Controle::Detalhes => {
                self.pedir_atualizacao(hwnd);
                return;
            }
            Controle::Papel(_) => return,
            Controle::Diario => {
                abrir_a_pasta_do_diario(self.app.as_ref());
                return;
            }
            Controle::Licencas => {
                abrir_as_licencas(hwnd);
                return;
            }
            Controle::Privacidade | Controle::Suporte => {
                if let Some(url) = modelo::pagina_dos_ajustes(c, crate::idioma::atual()) {
                    abrir_a_pagina_dos_ajustes(hwnd, url);
                }
                return;
            }
            // **O seletor "PT | EN"** (a tradução, 02/10): troca na hora e guarda a escolha, que
            // vence o idioma do Windows dali em diante. As outras janelas (o teleprompter, os
            // ajustes da câmera, o menu da bandeja) leem a versão do idioma e se reescrevem.
            Controle::Idioma(i) => {
                let novo = modelo::idioma_do_segmento(i);
                // **Todo toque guarda**, mesmo no idioma que já está (decisão do Bruno, 02/10): depois
                // do primeiro toque o app ignora o idioma do sistema de vez; antes dele, segue o
                // sistema. Não há "seguir o sistema" de volta.
                let mudou = crate::idioma::definir(novo);
                if self.app.is_some() {
                    if let Err(e) = crate::idioma::guardar(&crate::identidade::pasta_de_dados(), novo) {
                        registro::linha(format!("idioma: !! a escolha não foi guardada: {e}"));
                    }
                }
                registro::linha(format!("idioma: {} (escolhido no seletor{})", novo.codigo(), if mudou { "" } else { ", o mesmo de antes" }));
                self.versao_do_idioma = crate::idioma::versao();
                self.textos.clear();
                self.revisao_dos_aparelhos = u64::MAX;
                if self.app.is_none() {
                    self.modelo.idioma = novo;
                }
                self.pedir_atualizacao(hwnd);
                return;
            }
            // **A tela estendida sem o driver** (R10; 02/10, noite): sem o adaptador, a caixa que
            // explica e, com o "Instalar", o UAC e a instalação; na loja e no Windows sem suporte, a
            // página do SudoVDA, como antes; com o adaptador desligado, os Ajustes, que dizem o que
            // houve. A lista se refaz sozinha quando o adaptador aparecer.
            #[cfg(feature = "tela-estendida-futura")]
            Controle::TelaEstendidaSemDriver => {
                use crate::regras_do_driver::{clique_no_apagado, Acao, CliqueNoApagado};
                self.reler_o_driver("clique no ladrilho apagado"); // i18n: fora (diário)
                match clique_no_apagado(self.situacao_do_driver) {
                    CliqueNoApagado::Instalar if self.app.is_some() => {
                        if crate::regras_do_driver::pode_comecar(&crate::driver_da_tela_estendida::andamento())
                            && crate::driver_da_tela_estendida::perguntar(hwnd, Acao::Instalar)
                        {
                            crate::driver_da_tela_estendida::comecar(hwnd, Acao::Instalar, || {});
                        }
                    }
                    CliqueNoApagado::Instalar => {}
                    CliqueNoApagado::Ajustes => {
                        self.painel = Painel::Ajustes;
                    }
                    CliqueNoApagado::Pagina => abrir_a_pagina_do_instalador(),
                }
                self.pedir_atualizacao(hwnd);
                return;
            }
            // **Desinstalar o driver** (nos Ajustes, só o que o Quall instalou): só sem sessão de
            // pé — o coordenador seguraria o dispositivo aberto e os monitores virtuais vivos (a
            // revisão, achado 10).
            #[cfg(feature = "tela-estendida-futura")]
            Controle::DriverDaTelaEstendida => {
                use crate::regras_do_driver::{botao_dos_ajustes, pode_comecar, pode_desinstalar, Acao, BotaoDosAjustes};
                // Na loja, o botão só abre a página do instalador avulso: nada é elevado aqui.
                if botao_dos_ajustes(self.situacao_do_driver) == Some(BotaoDosAjustes::PaginaDoInstalador) {
                    abrir_a_pagina_do_instalador();
                    return;
                }
                let sem_sessao = self.app.as_ref().is_some_and(|a| a.emissor.estado().fase == Fase::Inicial);
                if sem_sessao
                    && pode_desinstalar(self.situacao_do_driver)
                    && pode_comecar(&crate::driver_da_tela_estendida::andamento())
                    && crate::driver_da_tela_estendida::perguntar(hwnd, Acao::Desinstalar)
                {
                    crate::driver_da_tela_estendida::comecar(hwnd, Acao::Desinstalar, || {});
                }
                self.pedir_atualizacao(hwnd);
                return;
            }
            #[cfg(not(feature = "tela-estendida-futura"))]
            Controle::TelaEstendidaSemDriver | Controle::DriverDaTelaEstendida => return,
            Controle::Chip => {
                let endereco = self.modelo.espera.endereco.clone();
                if crate::teleprompter::bancada::copiar(hwnd, &endereco) {
                    self.copiado_ate = Some(Instant::now() + COPIADO_POR);
                    self.pedir_atualizacao(hwnd);
                }
                return;
            }
            // **O Esquecer dos Ajustes pede confirmação** (§11.1): o primeiro clique troca o botão
            // por "Esquecer de verdade" por 5 s, e o segundo esquece. Na espera da câmera ele age
            // na hora, como antes (a retomada acabou de falhar, e a frase já pediu).
            Controle::Esquecer if self.modelo.cena == Cena::Painel(Painel::Ajustes) => {
                let confirmado = self.confirmar_esquecer_ate.is_some_and(|t| Instant::now() < t);
                if !confirmado {
                    self.confirmar_esquecer_ate = Some(Instant::now() + CONFIRMAR_POR);
                    self.pedir_atualizacao(hwnd);
                    return;
                }
                self.confirmar_esquecer_ate = None;
                if let Some(app) = &self.app {
                    app.emissor.esquecer_pares();
                    app.receptor.esquecer_pares();
                }
                self.pedir_atualizacao(hwnd);
                return;
            }
            _ => {}
        }
        let Some(app) = &self.app else { return };
        let (emissor, receptor) = (Arc::clone(&app.emissor), Arc::clone(&app.receptor));
        let ler = |c: Controle| self.h(c).is_some_and(marcado);
        match c {
            // **Um papel de cada vez.** Os dois botões só ficam visíveis nos painéis, onde nenhuma
            // sessão está de pé, então esta guarda é contra a corrida estreita entre o clique e a
            // mudança de fase — não contra um caminho que a pessoa possa percorrer.
            Controle::Espelhar => {
                if receptor.estado().fase == FaseDoReceptor::Parado {
                    emissor.espelhar();
                }
            }
            Controle::Exibir => {
                if emissor.estado().fase == Fase::Inicial {
                    let endereco = self.h(Controle::Endereco).map(texto_de).unwrap_or_default();
                    let pin = self.h(Controle::Pin).map(texto_de).unwrap_or_default();
                    receptor.exibir(endereco, pin);
                }
            }
            // Um botão, duas sessões possíveis — e só uma pode estar de pé, porque só uma pôde
            // começar.
            Controle::Cancelar => {
                if emissor.estado().fase != Fase::Inicial {
                    emissor.encerrar();
                } else {
                    receptor.encerrar();
                }
            }
            // `pares.json` é um só por computador: esquecer serve às duas metades.
            Controle::Esquecer => {
                emissor.esquecer_pares();
                receptor.esquecer_pares();
            }
            // O clique vai com a revisão da lista que o ladrilho mostra: o de uma lista que já foi
            // trocada (a recarga fora da thread da janela) é descartado pelo emissor.
            Controle::Ladrilho(i) => emissor.escolher_fonte(i, self.modelo.revisao_das_fontes),
            // `BS_AUTOCHECKBOX` já trocou o estado antes de esta mensagem chegar; aqui só se lê o
            // que ficou. Escrever o estado de volta daqui brigaria com o controle.
            Controle::Som => emissor.alternar_som(ler(Controle::Som)),
            Controle::Microfone => emissor.alternar_microfone(ler(Controle::Microfone)),
            Controle::Gravar => emissor.alternar_gravacao(),
            // **Os ajustes da câmera** (R9 §4.2): a janela própria, com a prévia dentro.
            // R9b: no Exibindo, a câmera de quem filma (a janela em modo remoto).
            Controle::AjustesDaCamera => {
                if receptor.estado().fase == FaseDoReceptor::Exibindo {
                    receptor.abrir_ajustes_da_camera();
                } else {
                    emissor.abrir_ajustes_da_camera();
                }
            }
            Controle::Mudo => receptor.definir_mudo(ler(Controle::Mudo)),
            Controle::SomComCamera => receptor.definir_som_com_camera(ler(Controle::SomComCamera)),
            Controle::Volume(i) => receptor.definir_volume(VOLUMES[i.min(VOLUMES.len() - 1)].0),
            _ => {}
        }
    }

    /// **O estado da tela**, lido do emissor e do receptor. Chamado a cada mudança de versão.
    fn montar(&self) -> EstadoDaTela {
        let Some(app) = &self.app else { return self.modelo.clone() };
        let emissor = &app.emissor;
        let e = emissor.estado();
        let r = app.receptor.estado();
        // A cena dos vários receptores é a do coordenador dono da tela: com `--varias-sessoes`, ou
        // na tela estendida sem a bandeira (R10, 02/10).
        let varios = e.fase == Fase::Transmitindo && emissor.coordenador_na_tela();
        let fonte = e.escolhida.and_then(|i| e.fontes.get(i)).cloned();
        let camera = emissor.argumentos.camera_sintetica || fonte.as_ref().is_some_and(|f| f.e_camera());
        let cena = match (e.fase, r.fase) {
            (Fase::Esperando | Fase::Encerrando, _) => Cena::Esperando,
            (Fase::Transmitindo, _) if varios => Cena::Varios,
            (Fase::Transmitindo, _) => Cena::NoAr,
            (Fase::Inicial, FaseDoReceptor::Conectando) => Cena::Conectando,
            (Fase::Inicial, FaseDoReceptor::Exibindo | FaseDoReceptor::Encerrando) => Cena::Exibindo,
            _ => Cena::Painel(self.painel),
        };
        // **O estado sai no idioma de agora**: os textos daqui por `t()`, e os que vêm prontos do
        // emissor e do receptor (conselhos, frases) por `tr()` — uns nascem traduzidos lá, outros
        // ficam em português porque alguém os compara; o `tr` traduz os que são chave da tabela e
        // deixa os outros como vieram. Os comparados aqui (a linha do microfone, o rótulo do Gravar)
        // são lidos **antes** da tradução.
        let (nota_do_emissor, nota_do_receptor) = (t(modelo::NOTA_DO_EMISSOR), t(modelo::NOTA_DO_RECEPTOR));
        let sessao = |item, luz, rotulo: &str, nota: &str| Some(SessaoNaBarra { item, luz, rotulo: rotulo.into(), nota: nota.into() });
        let (sessao, painel) = match cena {
            Cena::Esperando => (sessao(Painel::Espelhar, estilo::Luz::Aguardando, t("Aguardando"), nota_do_emissor), Painel::Espelhar),
            Cena::NoAr | Cena::Varios => (
                sessao(Painel::Espelhar, estilo::Luz::NoAr, if camera { t("No ar") } else { t("Espelhando") }, nota_do_emissor),
                Painel::Espelhar,
            ),
            Cena::Conectando => (sessao(Painel::Exibir, estilo::Luz::Aguardando, t("Conectando"), nota_do_receptor), Painel::Exibir),
            Cena::Exibindo => (sessao(Painel::Exibir, estilo::Luz::Conectado, t("Exibindo"), nota_do_receptor), Painel::Exibir),
            Cena::Painel(p) => (None, p),
        };

        // --- Espelhar ---
        let aviso_do_emissor = if !e.conselho.is_empty() {
            Some(Aviso { tom: estilo::Tom::Ambar, texto: tr(&e.conselho) })
        } else if self.r5_aberta {
            Some(Aviso { tom: estilo::Tom::Ambar, texto: t(crate::dono_da_captura::FRASE_DA_CAMERA_OCUPADA_PELA_R5).to_string() })
        } else {
            None
        };
        let espelhar = TelaEspelhar {
            fontes: e.fontes.iter().take(MAX_LADRILHOS).enumerate().map(|(i, f)| ladrilho_da_fonte(f, e.escolhida == Some(i))).collect(),
            camera,
            alguma_escolhida: fonte.is_some() || emissor.argumentos.camera_sintetica,
            som: e.com_som,
            microfone: emissor.microfone_pedido(),
            #[cfg(not(feature = "tela-estendida-futura"))]
            tela_estendida_sem_driver: None,
            ip: self.ip.clone(),
            aviso: aviso_do_emissor,
            // Com a tela R5 aberta, o Espelhar fica apagado, e a frase diz por quê.
            pode_espelhar: !self.r5_aberta,
            // Sem o adaptador do SudoVDA, a tela estendida apagada, depois dos monitores (R10).
            #[cfg(feature = "tela-estendida-futura")]
            tela_estendida_sem_driver: e.tela_estendida_sem_driver.then(|| {
                let monitores: Vec<bool> =
                    e.fontes.iter().take(MAX_LADRILHOS).map(|f| f.especie == EspecieDaFonte::Monitor).collect();
                crate::regras_da_tela_estendida::lugar_do_apagado(&monitores)
            }),
        };

        // --- Exibir ---
        let vazio = if !r.aviso_da_busca.is_empty() {
            tr(&r.aviso_da_busca)
        } else if r.procurando {
            t("Ninguém anunciando. Use o endereço que aparece no outro aparelho.").to_string()
        } else {
            t("A procura na rede não subiu — digite o endereço.").to_string()
        };
        let exibir = TelaExibir {
            aparelhos: r
                .aparelhos
                .iter()
                .map(|a| LinhaDeAparelho {
                    nome: a.nome.clone(),
                    tipo: match (a.tem_tela, a.tem_camera) {
                        (true, true) => t("Tela ou câmera"),
                        (true, false) => t("Tela"),
                        (false, true) => t("Câmera"),
                        (false, false) => "—",
                    }
                    .to_string(),
                    endereco: a.endereco.to_string(),
                })
                .collect(),
            escolhido: r.escolhido,
            procurando: r.procurando,
            vazio,
            pede_pin: r.pede_pin,
            // §11.1: a volta ao PIN (o PIN errado) é Aviso vermelho; o resto, âmbar.
            aviso: (!r.conselho.is_empty())
                .then(|| Aviso { tom: if r.pede_pin { estilo::Tom::Vermelho } else { estilo::Tom::Ambar }, texto: tr(&r.conselho) }),
            foco: self.foco,
        };

        // --- Ajustes ---
        let ajustes = TelaAjustes {
            // A frase diz se há pares no disco. O `oferece_desparear` é outra coisa (a retomada
            // falhou agora) e só libera o botão na espera da câmera, como antes (a revisão do
            // ramo, 3).
            tem_pares: e.ha_pares_conhecidos,
            confirmando: self.confirmar_esquecer_ate.is_some_and(|t| Instant::now() < t),
            pasta_do_diario: pasta_do_diario(emissor.argumentos.registro.as_deref())
                .map(|p| modelo::pasta_curta(&p.display().to_string(), std::env::var("LOCALAPPDATA").ok().as_deref()))
                .unwrap_or_default(),
            versao: env!("CARGO_PKG_VERSION").to_string(),
        };

        // --- a sessão do emissor ---
        // A câmera sintética da bancada (`--camera-sintetica`) não tem nome de fonte.
        let origem = fonte.as_ref().map(nome_da_origem).unwrap_or_else(|| if camera { "de bancada".into() } else { String::new() }); // i18n: fora (bancada)
        // Em português: comparada aqui embaixo (o som indo, a legenda do redondo).
        let linha_do_microfone = if camera { emissor.linha_do_microfone() } else { None };
        let frase_do_microfone = || match &linha_do_microfone {
            Some(l) => tr(l),
            None if emissor.microfone_pedido() => t("O microfone abre quando o outro aparelho conectar.").to_string(),
            None => t("Microfone desligado: a câmera vai sem som.").to_string(),
        };
        let (aviso_da_espera, aviso_ambar) = if e.fase == Fase::Encerrando {
            (t(modelo::ENCERRANDO).to_string(), false)
        } else if e.camera_pelo_dono && !e.resumo.is_empty() {
            // O aviso da espera da câmera (o PIN que não conferiu e mudou, o receptor que saiu).
            (tr(&e.resumo), true)
        } else {
            (String::new(), false)
        };
        let endereco = e.endereco.clone().unwrap_or_else(|| t("sem rede").to_string());
        let espera = TelaEspera {
            pin: e.pin.clone(),
            endereco: endereco.clone(),
            ha_pares: e.ha_pares_conhecidos,
            nome: e.nome_do_aparelho.clone(),
            anunciando: e.anunciando_por_mdns,
            origem: origem.clone(),
            com_som: (!camera).then_some(e.com_som),
            // O que a caixa prometeu, dito como **resultado**: a recusa do WASAPI, ou a linha do
            // microfone (a câmera leva o microfone, e não o som do sistema).
            frase: if camera { frase_do_microfone() } else { tr(&e.som_recusado) },
            aviso: aviso_da_espera,
            aviso_ambar,
            encerrando: e.fase == Fase::Encerrando,
        };
        let sessao_de_camera = camera && !varios && matches!(e.fase, Fase::Esperando | Fase::Transmitindo | Fase::Encerrando);
        let camera_da_tela = sessao_de_camera.then(|| ControlesDaCamera {
            microfone: emissor.microfone_pedido(),
            legenda_do_microfone: modelo::legenda_do_microfone(
                emissor.microfone_pedido(),
                linha_do_microfone.as_deref(),
                linha_do_microfone.as_deref() == Some(crate::regras_r5::FRASE_DA_PRIVACIDADE),
            )
            .to_string(),
            gravar: e.camera_pelo_dono,
            gravar_ativo: e.fase == Fase::Esperando || e.fase == Fase::Transmitindo,
            gravando: e.gravando,
            legenda_do_gravar: modelo::legenda_do_gravar(&e.rotulo_gravar, e.gravando, &e.linha_da_gravacao),
            nome_do_gravar: tr(&e.rotulo_gravar),
            legenda_do_parar: if e.camera_pelo_dono && e.gravando {
                t("Parar e salvar").into()
            } else if e.fase == Fase::Transmitindo {
                t("Parar").into()
            } else {
                t("Cancelar").into()
            },
            linha_da_gravacao: if e.camera_pelo_dono { tr(&e.linha_da_gravacao) } else { String::new() },
            // A dívida 22: "…esqueça os pareamentos" na espera da câmera vem com o botão à mão,
            // como antes desta rodada (os Ajustes ficam apagados com a sessão de pé).
            esquecer: e.camera_pelo_dono && e.oferece_desparear,
            // R9 §4.1: só a câmera comum pelo dono, de verdade (a sintética e a do Quall ficam sem).
            ajustes: e.camera_pelo_dono && emissor.ajustes_da_camera_disponiveis(),
            controlado_por: emissor.camera_controlada_por().unwrap_or_default(),
            pouca_luz: emissor.camera_com_pouca_luz(),
        });
        // i18n: fora (a linha do emissor, comparada em português)
        let som_indo = if camera { linha_do_microfone.as_deref() == Some("Com o som do microfone.") } else { e.som_ativo };
        let no_ar = TelaNoAr {
            par: e.par.clone(),
            origem: fonte.as_ref().map(|f| ladrilho_da_fonte(f, false).titulo).unwrap_or_else(|| origem.clone()),
            imagem: imagem_da_fonte(fonte.as_ref(), camera, emissor.argumentos.fps),
            rede: modelo::sem_porta(&endereco),
            som: match (camera, som_indo) {
                (true, true) => t("Microfone").into(),
                (false, true) => t("Indo junto").into(),
                _ => t("Sem som").into(),
            },
            som_indo,
            resumo: e.resumo.clone(),
            frase: if camera { frase_do_microfone() } else { tr(&e.som_recusado) },
        };
        let n = e.receptores.len();
        let varios_tela = TelaVarios {
            receptores: e.receptores.iter().map(|x| LinhaDeReceptor { nome: x.nome.clone(), monitor: x.monitor.clone(), resumo: x.resumo.clone() }).collect(),
            mais_um: e.esperando_mais_um.then(|| (e.pin.clone(), endereco.clone())),
            mais_um_texto: if n >= crate::sessoes::LIMITE_DE_SESSOES {
                tf("Limite de {} aparelhos: para entrar mais um, outro precisa sair.", &[&crate::sessoes::LIMITE_DE_SESSOES])
            } else {
                t("Abrindo a espera por mais um aparelho…").to_string()
            },
            // O que acabou com uma das sessões enquanto as outras seguem vale mais que a linha do
            // som: toma o lugar dela.
            rodape: if !e.conselho.is_empty() {
                tr(&e.conselho)
            } else if camera {
                frase_do_microfone()
            } else if !e.som_recusado.is_empty() {
                tr(&e.som_recusado)
            } else if e.som_ativo {
                t("Com o som deste computador, num aparelho só.").to_string()
            } else {
                t("Sem som — só imagem.").to_string()
            },
            rodape_aviso: !e.conselho.is_empty(),
        };

        // --- a sessão do receptor ---
        let exibindo = TelaExibindo {
            par: r.par.clone(),
            som: tr(&r.som),
            // Os controles do som só com uma track de som de pé, como antes.
            controles_do_som: r.fase == FaseDoReceptor::Exibindo && !r.som.is_empty(),
            mudo: r.som_mudo,
            volume: modelo::indice_do_volume(r.som_volume),
            som_com_camera: r.som_com_camera,
            detalhes_abertos: self.h(Controle::Detalhes).is_some_and(marcado),
            resumo: r.resumo.clone(),
            cadeia: r.cadeia.clone(),
            alerta: r.cadeia_alerta,
            encerrando: r.fase == FaseDoReceptor::Encerrando,
            ajustes_da_camera: r.fase == FaseDoReceptor::Exibindo && r.camera_remota,
        };

        EstadoDaTela {
            cena,
            painel,
            nome_do_aparelho: e.nome_do_aparelho.clone(),
            sessao,
            espelhar,
            exibir,
            ajustes,
            espera,
            no_ar,
            varios: varios_tela,
            conectando: r.destino.clone(),
            exibindo,
            camera: camera_da_tela,
            copiado: self.copiado_ate.is_some_and(|t| Instant::now() < t),
            // Lidas junto com as listas, debaixo das mesmas travas: um ladrilho nunca leva a
            // revisão de uma lista que ele não mostra.
            revisao_das_fontes: e.revisao_das_fontes,
            revisao_dos_aparelhos: r.revisao_dos_aparelhos,
            aviso_do_teleprompter: aviso_do_cartao(),
            teleprompter: teleprompter_aberto(),
            idioma: crate::idioma::atual(),
            #[cfg(feature = "tela-estendida-futura")]
            driver: modelo::TelaDoDriver { situacao: self.situacao_do_driver, andamento: crate::driver_da_tela_estendida::andamento() },

        }
    }

    /// **Relê a situação do driver** (PnP, HKLM, pacote): na abertura, no `WM_DEVICECHANGE` do
    /// adaptador, no fim de uma instalação e na volta aos Ajustes. Não a cada pulso.
    fn reler_o_driver(&mut self, porque: &str) {
        if self.app.is_none() {
            return;
        }
        #[cfg(feature = "tela-estendida-futura")]
        {
        let s = crate::driver_da_tela_estendida::ler_situacao();
        if s != self.situacao_do_driver {
            registro::linha(format!("driver: situação {:?} → {s:?} ({porque})", self.situacao_do_driver));
            self.situacao_do_driver = s;
            self.ultima_versao = u64::MAX;
        }
        }
    }

    // =========================================================================================
    // Aplicar o estado nos controles
    // =========================================================================================

    /// **Põe o estado nos controles**: os ladrilhos e a lista quando as listas mudam, o lugar, a
    /// visibilidade, o texto (o nome para o Narrador) e o estado de marcado de cada um, e pede o
    /// redesenho.
    fn aplicar(&mut self, hwnd: HWND) {
        // Os textos postos foram esquecidos (o idioma trocou, ou o retrato passa a outro exemplo): o
        // nome dos campos e a dica de dentro deles são escritos de novo, no idioma de agora.
        if self.textos.is_empty() {
            self.dar_nome_aos_campos();
        }
        self.montar_os_ladrilhos(hwnd);
        self.montar_a_lista();
        self.ordenar_o_microfone();
        let quadro = modelo::compor(&self.modelo);
        let dpi = self.dpi;
        let mut todos: Vec<Controle> = Controle::fixos();
        todos.extend((0..self.ladrilhos.len()).map(Controle::Ladrilho));
        for c in todos {
            let Some(h) = self.h(c) else { continue };
            match quadro.lugar(c) {
                Some(r) => {
                    let p = em_pixels(dpi, r);
                    let texto = modelo::texto_acessivel(c, &self.modelo);
                    unsafe {
                        let _ = SetWindowPos(
                            h,
                            None,
                            p.left,
                            p.top,
                            p.right - p.left,
                            p.bottom - p.top,
                            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                        let _ = EnableWindow(h, modelo::habilitado(c, &self.modelo));
                        // O texto do campo é da pessoa: só os botões e o letreiro recebem o nome.
                        if !matches!(c.especie(), Especie::Campo | Especie::Lista) && self.textos.get(&c) != Some(&texto) {
                            let t = largo(&texto);
                            let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
                            self.textos.insert(c, texto);
                        }
                    }
                }
                None => unsafe {
                    let _ = ShowWindow(h, SW_HIDE);
                },
            }
            if matches!(c.especie(), Especie::Alternar | Especie::Opcao) {
                let sim = self.marcado_pelo_estado(c);
                if let Some(sim) = sim {
                    if marcado(h) != sim {
                        marcar(h, sim);
                    }
                }
            }
        }
        self.quadro = quadro;
        unsafe {
            let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    /// O microfone é um controle só em dois lugares: a linha com interruptor do painel Espelhar
    /// (depois do Som, na ordem do Tab) e o redondo da espera da câmera (depois do chip e do
    /// Esquecer). A ordem do Tab é a ordem das janelas irmãs, e ela é trocada aqui quando a cena
    /// troca (a revisão do ramo, 16).
    fn ordenar_o_microfone(&mut self) {
        let na_sessao = self.modelo.camera.is_some() && matches!(self.modelo.cena, Cena::Esperando | Cena::NoAr);
        if na_sessao == self.microfone_na_sessao {
            return;
        }
        let depois = if na_sessao { Controle::Diario } else { Controle::Som };
        if let (Some(m), Some(d)) = (self.h(Controle::Microfone), self.h(depois)) {
            unsafe {
                let _ = SetWindowPos(m, Some(d), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        self.microfone_na_sessao = na_sessao;
    }

    /// O estado de marcado que o emissor e o receptor publicam para cada interruptor e opção. O
    /// "Detalhes" é só da janela (o controle guarda o próprio estado); no retrato, tudo sai do
    /// exemplo.
    fn marcado_pelo_estado(&self, c: Controle) -> Option<bool> {
        if self.app.is_none() {
            return Some(modelo::marcado_no_exemplo(c, &self.modelo));
        }
        let e = &self.modelo;
        match c {
            Controle::Item(p) => Some(p == e.painel),
            Controle::Ladrilho(i) => e.espelhar.fontes.get(i).map(|x| x.escolhido),
            Controle::Som => Some(e.espelhar.som),
            Controle::Microfone => Some(e.espelhar.microfone),
            Controle::Mudo => Some(e.exibindo.mudo),
            Controle::SomComCamera => Some(e.exibindo.som_com_camera),
            Controle::Volume(i) => Some(e.exibindo.volume == i),
            Controle::Idioma(i) => Some(modelo::idioma_do_segmento(i) == e.idioma),
            _ => None,
        }
    }

    /// Os ladrilhos: um `BUTTON` por origem, refeitos só quando o **número** de origens muda (o
    /// desenho e o nome de cada um saem do estado a cada volta), logo depois dos itens da barra na
    /// ordem do Tab.
    fn montar_os_ladrilhos(&mut self, hwnd: HWND) {
        let n = self.modelo.espelhar.fontes.len().min(MAX_LADRILHOS);
        if n == self.ladrilhos.len() {
            return;
        }
        for h in self.ladrilhos.drain(..) {
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
        for i in 0..n {
            self.textos.remove(&Controle::Ladrilho(i));
        }
        let mut antes = self.h(Controle::Item(Painel::Ajustes));
        for i in 0..n {
            match unsafe { criar_controle(hwnd, Controle::Ladrilho(i)) } {
                Ok(h) => {
                    if let Some(a) = antes {
                        unsafe {
                            let _ = SetWindowPos(h, Some(a), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                        }
                    }
                    antes = Some(h);
                    self.ladrilhos.push(h);
                }
                Err(e) => registro::linha(format!("janela: !! o ladrilho {i} não nasceu: {e}")),
            }
        }
    }

    /// A lista de aparelhos: remontada só quando o conteúdo muda.
    fn montar_a_lista(&mut self) {
        let Some(lista) = self.h(Controle::Lista) else { return };
        let revisao = self.modelo.revisao_dos_aparelhos;
        if revisao == self.revisao_dos_aparelhos {
            return;
        }
        self.revisao_dos_aparelhos = revisao;
        unsafe {
            SendMessageW(lista, LB_RESETCONTENT, None, None);
            for a in &self.modelo.exibir.aparelhos {
                // O texto da linha é o que o Narrador lê e o que a busca por letra da lista usa.
                let t = largo(&format!("{}, {}, {}", a.nome, a.tipo, a.endereco));
                SendMessageW(lista, LB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
            }
            if let Some(i) = self.modelo.exibir.escolhido {
                SendMessageW(lista, LB_SETCURSEL, Some(WPARAM(i)), None);
            }
        }
    }

    // =========================================================================================
    // O desenho
    // =========================================================================================

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
                registro::linha(format!("janela: !! o desenho falhou: {e}"));
            }
        }
    }

    /// Um botão, no `NM_CUSTOMDRAW`: o estado é o do próprio controle.
    fn desenhar_controle(&mut self, c: Controle, cd: &NMCUSTOMDRAW) {
        let s = cd.uItemState;
        let tem = |f: windows::Win32::UI::Controls::NMCUSTOMDRAW_DRAW_STATE_FLAGS| (s.0 & f.0) != 0;
        // O anel de foco: o foco, e o estado da interface do botão sem `UISF_HIDEFOCUS` (escondido
        // até o teclado ser usado; ver `Janela::nova`).
        let foco_escondido = (unsafe { SendMessageW(cd.hdr.hwndFrom, WM_QUERYUISTATE, None, None) }.0 as u32 & UISF_HIDEFOCUS) != 0;
        let estado = modelo::EstadoDoControle {
            apertado: tem(CDIS_SELECTED),
            foco: tem(CDIS_FOCUS) && !foco_escondido,
            desligado: tem(CDIS_DISABLED),
            quente: tem(CDIS_HOT),
            marcado: matches!(c.especie(), Especie::Alternar | Especie::Opcao) && marcado(cd.hdr.hwndFrom),
        };
        let (l, a) = self.em_dip(cd.rc);
        let ap = modelo::aparencia(c, &self.modelo, l, a, estado);
        self.desenhar_aparencia(cd.hdc, cd.rc, &ap);
    }

    /// A lista e o letreiro, no `WM_DRAWITEM`.
    fn desenhar_item(&mut self, d: &DRAWITEMSTRUCT) {
        if self.pintor.is_none() {
            self.desenhar_item_sem_d2d(d);
            return;
        }
        let (l, a) = self.em_dip(d.rcItem);
        if d.CtlType == ODT_LISTBOX {
            if d.itemID == u32::MAX {
                // A lista vazia com o foco: só o fundo.
                self.desenhar_aparencia(d.hDC, d.rcItem, &modelo::Aparencia { fundo: estilo::FUNDO, itens: vec![], opacidade: 1.0 });
                return;
            }
            let escolhida = (d.itemState.0 & ODS_SELECTED.0) != 0;
            let foco = (d.itemState.0 & ODS_FOCUS.0) != 0;
            let ap = modelo::aparencia_da_linha(d.itemID as usize, &self.modelo, l, a, escolhida, foco);
            self.desenhar_aparencia(d.hDC, d.rcItem, &ap);
        } else if d.CtlType == ODT_STATIC {
            let ap = modelo::aparencia(Controle::Letreiro, &self.modelo, l, a, modelo::EstadoDoControle::default());
            self.desenhar_aparencia(d.hDC, d.rcItem, &ap);
        }
    }

    /// Sem o Direct2D: a linha da lista e o letreiro como texto do GDI, para nada sumir.
    fn desenhar_item_sem_d2d(&mut self, d: &DRAWITEMSTRUCT) {
        let texto = if d.CtlType == ODT_LISTBOX {
            self.modelo.exibir.aparelhos.get(d.itemID as usize).map(|a| format!("{} · {}", a.nome, a.endereco)).unwrap_or_default()
        } else {
            modelo::pin_para_ler(&self.modelo.espera.pin)
        };
        let mut r = d.rcItem;
        let selecionada = d.CtlType == ODT_LISTBOX && (d.itemState.0 & ODS_SELECTED.0) != 0;
        unsafe {
            let pincel = if selecionada { self.pincel_campo } else { self.pincel_fundo };
            FillRect(d.hDC, &r, pincel);
            SetBkMode(d.hDC, TRANSPARENT);
            SetTextColor(d.hDC, windows::Win32::Foundation::COLORREF(estilo::TEXTO.colorref()));
            let mut w: Vec<u16> = texto.encode_utf16().collect();
            r.left += escala(self.dpi, 12.0);
            DrawTextW(d.hDC, &mut w, &mut r, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        }
    }

    fn em_dip(&self, r: RECT) -> (f32, f32) {
        let k = 96.0 / self.dpi as f32;
        ((r.right - r.left) as f32 * k, (r.bottom - r.top) as f32 * k)
    }

    fn desenhar_aparencia(&mut self, hdc: HDC, area: RECT, ap: &modelo::Aparencia) {
        let dpi = self.dpi;
        let Some(p) = self.pintor.as_mut() else { return };
        if let Err(e) = p.desenhar(hdc, area, dpi, Some(ap.fundo), &ap.itens, ap.opacidade) {
            if !self.falha_dita {
                self.falha_dita = true;
                registro::linha(format!("janela: !! o desenho de um controle falhou: {e}"));
            }
        }
    }
}

// =============================================================================================
// O que vira texto de tela
// =============================================================================================

/// O ladrilho de uma origem (§7.1): o nome, e "L × A" em mono, "Câmera", ou "Um monitor novo para
/// cada aparelho".
fn ladrilho_da_fonte(f: &Fonte, escolhido: bool) -> Ladrilho {
    match f.especie {
        EspecieDaFonte::Camera => Ladrilho { titulo: f.nome.clone(), detalhe: t("Câmera").into(), detalhe_mono: false, icone: estilo::Icone::Camera, escolhido },
        #[cfg(feature = "tela-estendida-futura")]
        EspecieDaFonte::TelaEstendida => Ladrilho {
            titulo: t(crate::regras_da_tela_estendida::TITULO).into(),
            detalhe: t(crate::regras_da_tela_estendida::DETALHE).into(),
            detalhe_mono: false,
            icone: estilo::Icone::TelaEstendida,
            escolhido,
        },

        EspecieDaFonte::Monitor => Ladrilho {
            titulo: if f.primario { tf("{} (principal)", &[&f.nome]) } else { f.nome.clone() },
            detalhe: if f.largura > 0 && f.altura > 0 { format!("{} × {}", f.largura, f.altura) } else { String::new() },
            detalhe_mono: true,
            icone: estilo::Icone::Monitor,
            escolhido,
        },
    }
}

/// O nome da origem na instrução da espera.
fn nome_da_origem(f: &Fonte) -> String {
    match f.especie {
        #[cfg(feature = "tela-estendida-futura")]
        EspecieDaFonte::TelaEstendida => t("a tela estendida").into(),
        _ => f.nome.clone(),
    }
}

/// O cartão "Imagem" do no ar: o tamanho que **sai** (o teto do núcleo, `quall_core::teto::ajustar`,
/// a mesma conta que a transmissão faz) e o fps. A câmera escolhe o tipo nativo dela só ao abrir, e
/// a tela estendida é um monitor que ainda não existe: os dois dizem de onde vem.
fn imagem_da_fonte(f: Option<&Fonte>, camera: bool, fps: u32) -> String {
    match f {
        _ if camera => t("Da câmera").into(),
        #[cfg(feature = "tela-estendida-futura")]
        Some(f) if f.especie == EspecieDaFonte::TelaEstendida => t("Monitor novo").into(),
        Some(f) if f.largura > 0 && f.altura > 0 => {
            let s = quall_core::teto::ajustar(f.largura, f.altura, fps);
            format!("{}×{} · {}", s.largura, s.altura, s.fps)
        }
        _ => "—".into(),
    }
}

/// A pasta do diário: a do `--registro`, ou a padrão.
fn pasta_do_diario(registro_pedido: Option<&Path>) -> Option<PathBuf> {
    let arquivo = registro_pedido.map(Path::to_path_buf).unwrap_or_else(registro::padrao);
    arquivo.parent().map(Path::to_path_buf)
}

/// "Abrir a pasta" do diário, pelo Explorador.
fn abrir_a_pasta_do_diario(app: Option<&App>) {
    let Some(app) = app else { return };
    let Some(pasta) = pasta_do_diario(app.emissor.argumentos.registro.as_deref()) else { return };
    if let Err(e) = std::process::Command::new("explorer.exe").arg(&pasta).spawn() {
        registro::linha(format!("janela: !! a pasta do diário não abriu ({}): {e}", pasta.display()));
    }
}

/// Os avisos que acompanham o app no MSI/MSIX. Abre o texto existente, sem baixar nem escrever.
fn abrir_as_licencas(hwnd: HWND) {
    let arquivo = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|p| p.join("THIRD_PARTY_NOTICES.txt")));
    let erro = match arquivo {
        Some(arquivo) if arquivo.is_file() => {
            let caminho: Vec<u16> = arquivo.as_os_str().encode_wide().chain(Some(0)).collect();
            let r = unsafe {
                windows::Win32::UI::Shell::ShellExecuteW(Some(hwnd), w!("open"), PCWSTR(caminho.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
            };
            if (r.0 as isize) > 32 { return; }
            format!("ShellExecuteW: {} ({})", r.0 as isize, arquivo.display()) // i18n: fora (diário)
        }
        _ => "THIRD_PARTY_NOTICES.txt ausente ao lado do executável".to_string(), // i18n: fora (diário)
    };
    registro::linha(format!("licenças: !! não abriu: {erro}"));
    let mensagem = largo(&t("As licenças de terceiros não puderam ser abertas. Reinstale o Quall Monitor para recuperar o arquivo de avisos."));
    unsafe { MessageBoxW(Some(hwnd), PCWSTR(mensagem.as_ptr()), w!("Quall Monitor"), MB_OK | MB_ICONERROR); } // i18n: fora (nome do produto)
}

/// Abre privacidade/suporte no navegador padrão. Falhas ficam no diário e em uma caixa da janela.
fn abrir_a_pagina_dos_ajustes(hwnd: HWND, url: &str) {
    let endereco = largo(url);
    let r = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(Some(hwnd), w!("open"), PCWSTR(endereco.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
    };
    if (r.0 as isize) > 32 { return; }
    registro::linha(format!("janela: !! a página dos Ajustes não abriu ({url}): código {}", r.0 as isize));
    let mensagem = largo(&t("A página não pôde ser aberta no navegador. Tente novamente ou escreva para suporte@queven.com.br."));
    unsafe { MessageBoxW(Some(hwnd), PCWSTR(mensagem.as_ptr()), w!("Quall Monitor"), MB_OK | MB_ICONERROR); } // i18n: fora (nome do produto)
}

/// A página do instalador avulso do driver no navegador padrão (a loja e o Windows sem suporte; o
/// destino combinado com a frente do site em `regras_da_tela_estendida::URL_DO_INSTALADOR_DO_DRIVER`).
#[cfg(feature = "tela-estendida-futura")]
fn abrir_a_pagina_do_instalador() {
    let url = crate::regras_da_tela_estendida::URL_DO_INSTALADOR_DO_DRIVER;
    let largo_da_url = largo(url);
    let r = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(largo_da_url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    // O `ShellExecuteW` devolve mais que 32 quando deu certo.
    if (r.0 as isize) <= 32 {
        registro::linha(format!("janela: !! a página do instalador do driver não abriu ({url}): código {}", r.0 as isize));
    } else {
        registro::linha(format!("janela: a página do instalador do driver aberta no navegador ({url})"));
    }
}

/// O que o cartão do teleprompter está fazendo, ou por que não abriu (o painel Teleprompter o
/// mostra como aviso). Escrito pela thread do cartão, lido pelo pulso da janela.
static AVISO_DO_CARTAO: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// Um cartão está em curso (fechando a janela velha ou esperando a nova).
static CARTAO_EM_CURSO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// O app está saindo: o cartão em curso não abre mais nada.
static SAINDO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A janela do teleprompter aberta neste processo, e o papel dela, em valores do modelo.
fn teleprompter_aberto() -> Option<modelo::TeleprompterAberto> {
    use crate::teleprompter::PapelAberto;
    crate::teleprompter::papel_aberto().map(|p| match p {
        PapelAberto::Escolha => modelo::TeleprompterAberto::Escolha,
        PapelAberto::Prompter => modelo::TeleprompterAberto::Prompter,
        PapelAberto::Controle => modelo::TeleprompterAberto::Controle,
        PapelAberto::PrompterComCamera => modelo::TeleprompterAberto::PrompterComCamera,
    })
}

fn aviso_do_cartao() -> String {
    AVISO_DO_CARTAO.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn dizer_do_cartao(s: &str) {
    *AVISO_DO_CARTAO.lock().unwrap_or_else(|e| e.into_inner()) = s.to_string();
}

/// **Um cartão do teleprompter**: abre a janela dele **já no papel** (pulando a escolha de lá).
///
/// - Sem janela aberta: abre no papel pedido.
/// - Com a janela aberta **no mesmo papel**: só a traz para a frente (`teleprompter::abrir` faz
///   isso). Fechar ali derrubaria a sessão, e com a tela R5 gravando pararia a gravação (a revisão
///   do ramo, 4).
/// - Com a janela aberta noutro papel (ou na escolha): fecha pelo caminho do Fechar (a sessão cai
///   pela ordem do fim, a réplica é gravada, a gravação da R5 fecha o arquivo), espera até
///   [`FECHO_DO_TELEPROMPTER`] e reabre no papel pedido; o painel diz o que está acontecendo, e diz
///   se a janela velha não fechou.
///
/// Numa thread própria (fechar espera a janela de lá, e a daqui não pode congelar), **um cartão de
/// cada vez**: entre o clique e a janela nova se registrar, `abrir` ainda não a vê, e dois cliques
/// seguidos abririam duas janelas.
fn abrir_o_teleprompter(i: usize) {
    use crate::teleprompter::{ConfigDaTela, Lado, PapelAberto};
    use std::sync::atomic::Ordering;
    let pedido = match i {
        1 => PapelAberto::Controle,
        2 => PapelAberto::PrompterComCamera,
        _ => PapelAberto::Prompter,
    };
    let cfg = ConfigDaTela {
        papel: Some(if i == 1 { Lado::Controle } else { Lado::Prompter }),
        com_camera: i == 2,
        ..Default::default()
    };
    if crate::teleprompter::papel_aberto() == Some(pedido) {
        crate::teleprompter::abrir(cfg);
        return;
    }
    if SAINDO.load(Ordering::SeqCst) || CARTAO_EM_CURSO.swap(true, Ordering::SeqCst) {
        return;
    }
    let lancou = std::thread::Builder::new().name("quall.janela.teleprompter".into()).spawn(move || {
        if crate::teleprompter::aberta() {
            dizer_do_cartao(modelo::AVISO_FECHANDO_O_TELEPROMPTER);
            // Só pede o fecho (prazo zero) e espera aqui, olhando também a saída do app.
            let _ = crate::teleprompter::fechar_se_aberta(Duration::ZERO);
            let fim = Instant::now() + FECHO_DO_TELEPROMPTER;
            while crate::teleprompter::aberta() && Instant::now() < fim && !SAINDO.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if SAINDO.load(Ordering::SeqCst) {
            dizer_do_cartao("");
        } else if crate::teleprompter::aberta() {
            dizer_do_cartao(modelo::AVISO_O_TELEPROMPTER_NAO_FECHOU);
            registro::linha("teleprompter: !! a janela aberta não fechou em 45 s; o cartão não a reabriu no papel pedido");
        } else {
            crate::teleprompter::abrir(cfg);
            // Espera a janela nova se registrar (sem olhar a saída: quem sai espera por isto, e
            // depois a fecha pelo caminho de sempre).
            let fim = Instant::now() + Duration::from_secs(3);
            while !crate::teleprompter::aberta() && Instant::now() < fim {
                std::thread::sleep(Duration::from_millis(20));
            }
            dizer_do_cartao("");
        }
        CARTAO_EM_CURSO.store(false, Ordering::SeqCst);
    });
    if lancou.is_err() {
        CARTAO_EM_CURSO.store(false, Ordering::SeqCst);
    }
}

/// **Antes do desmonte**: o cartão do teleprompter em curso não abre mais nada, e quem sai espera
/// ele terminar (até `prazo`) antes de fechar a janela do teleprompter — senão a janela nova podia
/// nascer depois do fecho (a revisão do ramo, 14). `true` se não havia cartão em curso no fim.
pub fn esperar_o_cartao(prazo: Duration) -> bool {
    use std::sync::atomic::Ordering;
    SAINDO.store(true, Ordering::SeqCst);
    let fim = Instant::now() + prazo;
    while CARTAO_EM_CURSO.load(Ordering::SeqCst) {
        if Instant::now() >= fim {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

// =============================================================================================
// O retrato de bancada
// =============================================================================================

/// **`--retratos-de-bancada=<pasta>`**: cada estado de exemplo da §7 desenhado pela janela de
/// verdade (os mesmos controles, o mesmo `NM_CUSTOMDRAW`, o mesmo Direct2D) num bitmap nosso, e
/// gravado em BMP. A janela nasce **fora da tela** (em −20000, −20000, sem ativar) e a captura é o
/// `WM_PRINT` da própria janela (`teleprompter::bancada::capturar_janela`, medido na Sessão 0 em
/// 14/09): nada de fora dela entra no arquivo, e nada da área de trabalho de ninguém. Sem emissor,
/// sem receptor, sem câmera virtual, sem rede: o processo desenha e sai.
///
/// A 96 dpi todos os estados; a 144 (150 %), uma amostra, com o DPI forçado na janela (a Sessão 0
/// não tem monitor para dar outro).
pub fn retratos(pasta: &Path) -> Result<Vec<String>> {
    let mut linhas = Vec::new();
    unsafe {
        let hwnd = criar_a_janela(-20000, -20000, WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0))?;
        let janela = Janela::nova(hwnd, None, Vec::new(), false);
        // O que não nasceu vai para as linhas com "!!": o processo sai com código 1 (a revisão do
        // ramo, 11).
        for f in &janela.falhas {
            linhas.push(format!("retrato: {f}"));
        }
        let ptr = Box::into_raw(janela);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = std::fs::create_dir_all(pasta);
        let amostra_150 = ["01-espelhar", "04-exibir", "08-esperando", "16-exibindo"];
        // **Os dois idiomas** (a tradução, 02/10): o português com o nome de sempre, o inglês com
        // `-en` no fim. O idioma vale só nesta thread (`com_idioma`), que é a da janela.
        for idioma in [crate::idioma::Idioma::Pt, crate::idioma::Idioma::En] {
        let sufixo = if idioma == crate::idioma::Idioma::En { "-en" } else { "" };
        crate::idioma::com_idioma(idioma, || {
        for dpi in [96u32, 144] {
            {
                let j = &mut *ptr;
                if j.dpi != dpi {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut r);
                    j.mudar_dpi(hwnd, dpi, r);
                }
            }
            for (nome, estado) in modelo::exemplos() {
                if dpi != 96 && !amostra_150.contains(&nome) {
                    continue;
                }
                {
                    let j = &mut *ptr;
                    j.painel = estado.painel;
                    j.modelo = estado;
                    j.modelo.idioma = idioma;
                    j.revisao_dos_aparelhos = u64::MAX;
                    j.textos.clear();
                    j.aplicar(hwnd);
                }
                let arquivo = pasta.join(format!("{nome}{sufixo}@{dpi}dpi.bmp"));
                let esperado = (escala(dpi, estilo::LARGURA_MINIMA), escala(dpi, estilo::ALTURA_MINIMA));
                let linha = match crate::teleprompter::bancada::capturar_janela(hwnd, &arquivo) {
                    Ok((l, a, _)) => {
                        // "Pintou" é ter peça além do fundo: o fundo sozinho não é retrato de nada.
                        let pecas = pixels_alem_do_fundo(&arquivo);
                        let mut avisos = String::new();
                        if (l, a) != esperado {
                            avisos.push_str(&format!(" !! o tamanho devia ser {}x{}", esperado.0, esperado.1)); // i18n: fora (bancada)
                        }
                        if pecas < 2000 {
                            avisos.push_str(&format!(" !! SÓ O FUNDO ({pecas} pixels além dele)")); // i18n: fora (bancada)
                        }
                        format!("retrato: {} {l}x{a}, {pecas} pixels além do fundo{avisos}", arquivo.display()) // i18n: fora (bancada)
                    }
                    Err(e) => format!("retrato: !! {nome}@{dpi}dpi: {e}"),
                };
                registro::linha(&linha);
                linhas.push(linha);
            }
        }
        });
        }
        let _ = DestroyWindow(hwnd);
    }
    Ok(linhas)
}

/// Quantos pixels de um BMP de 32 bits (o de `capturar_janela`) não são nem o fundo do painel nem o
/// da barra lateral.
fn pixels_alem_do_fundo(arquivo: &Path) -> usize {
    let Ok(b) = std::fs::read(arquivo) else { return 0 };
    if b.len() < 54 {
        return 0;
    }
    let inicio = u32::from_le_bytes([b[10], b[11], b[12], b[13]]) as usize;
    let fundos = [estilo::FUNDO, estilo::BARRA];
    let perto = |p: &[u8], c: estilo::Cor| {
        (p[2] as i32 - c.r as i32).abs() <= 2 && (p[1] as i32 - c.g as i32).abs() <= 2 && (p[0] as i32 - c.b as i32).abs() <= 2
    };
    b.get(inicio..).unwrap_or(&[]).chunks_exact(4).filter(|p| !fundos.iter().any(|c| perto(p, *c))).count()
}
