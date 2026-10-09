//! "O que transmitir": o catálogo de origens que a tela inicial oferece.
//!
//! # Cada monitor é uma linha, e isso é o ponto
//!
//! `docs/ux-m6.md` §1.6 nomeia o caso: *"cada tela quando há mais de um monitor — Windows e macOS
//! multi-monitor é caso real que iOS não tem"*. O que existia em `apps/windows` antes desta
//! rodada era `ScreenCapture::start_primary_monitor()`, com o monitor primário achado por
//! `MonitorFromPoint(0,0)`. Num Dell ligado a um monitor externo, "a tela" não é resposta: o
//! primário é um sorteio que a pessoa só descobriria olhando o outro lado da sala.
//!
//! # A identidade guardada é o nome do dispositivo GDI, não o `HMONITOR`
//!
//! `HMONITOR` é um handle de sessão: ele muda quando alguém desliga um monitor, troca de dock ou
//! muda a resolução, e um handle guardado passaria a apontar para outro monitor **em silêncio** —
//! o mesmo defeito que o irmão do macOS evitou guardando `CGDirectDisplayID` em vez de índice. O
//! que sobrevive a uma reconfiguração é o nome do dispositivo (`\\.\DISPLAY1`), e é ele que a
//! `Fonte` carrega. Na hora de capturar, [`achar_hmonitor`] reenumera e casa pelo nome; se o
//! monitor não estiver mais lá, **falha** em vez de cair para outro.
//!
//! Ressalva honesta: `\\.\DISPLAY1` é identidade de **posição de saída**, não do painel físico.
//! Trocar dois monitores de porta troca os nomes. É melhor que índice de lista e pior que um
//! identificador de painel; o identificador de painel existe (`monitorDevicePath` do
//! DisplayConfig) e não foi usado como chave porque nem toda saída ativa aparece na consulta de
//! DisplayConfig, e uma chave que às vezes não existe é pior que uma chave imperfeita.
//!
//! # A câmera entra na lista
//!
//! Desde 18/09/2026 (Frente C, `docs/camera-no-windows.md`) as câmeras do PC entram no seletor,
//! **depois** dos monitores: atrás de `--cameras` até a fase 5, e por padrão desde 21/09 (só
//! `--sem-cameras`, de bancada, as tira). A identidade guardada de uma câmera é o link
//! simbólico, pela mesma razão do nome GDI aqui embaixo; quem enumera e decide o dono é
//! `cameras.rs`. A bandeira ficou até a webcam de verdade ter sido lida e transmitida (a fase 5: a
//! integrada, a Panasonic nos modos WEB e DV, e a Canon), porque "listar o que o app não sabe abrir
//! é pior que não listar" (`docs/app-windows.md`, "Cortei a câmera").

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};

/// `MONITORINFOF_PRIMARY` do `winuser.h`. Escrito à mão porque o crate `windows` 0.62 não expõe
/// esta constante em `Win32::Graphics::Gdi` (expõe a struct e a função, não o sinalizador).
const MONITORINFOF_PRIMARY: u32 = 1;

/// Que tipo de origem uma linha do seletor é.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Especie {
    /// Um monitor de verdade; `id` é o nome GDI.
    Monitor,
    /// "Tela estendida": um monitor virtual novo por aparelho, pelo SudoVDA que a pessoa instalou
    /// (R10, 02/10: com ou sem `--varias-sessoes`).
    #[cfg(feature = "tela-estendida-futura")]
    TelaEstendida,
    /// Uma câmera deste PC; `id` é o link simbólico (`cameras.rs`).
    Camera,
}

/// Uma origem que a pessoa pode escolher na tela inicial.
#[derive(Clone, Debug)]
pub struct Fonte {
    /// `\\.\DISPLAY1` — ver a nota de identidade acima. Numa câmera, o link simbólico.
    pub id: String,
    /// O nome que a pessoa reconhece: "DELL U2415", "Tela do notebook", "Integrated Webcam".
    pub nome: String,
    pub largura: u32,
    pub altura: u32,
    pub primario: bool,
    pub especie: Especie,
}

impl Fonte {
    /// O rótulo que vai na track e aparece do lado de quem recebe. O mesmo formato para câmera
    /// ("Integrated Webcam de G3BRUNO"): decisão do coordenador em 18/09, o estilo do Windows.
    pub fn rotulo_da_track(&self, nome_do_aparelho: &str) -> String {
        // No idioma da hora em que a sessão abre: o rótulo viaja na oferta e ninguém o compara.
        crate::idioma::tf("{} de {}", &[&self.nome, &nome_do_aparelho])
    }

    pub fn e_camera(&self) -> bool {
        self.especie == Especie::Camera
    }

    /// Texto da linha do seletor.
    pub fn linha_do_seletor(&self) -> String {
        // Câmera sem resolução: lê-la exigiria ativar a câmera (`docs/camera-no-windows.md` §6.1).
        if self.especie == Especie::Camera {
            return crate::idioma::tf("Câmera: {}", &[&self.nome]);
        }
        // Sem tamanho: a fonte "Tela estendida" (não é um monitor; cada aparelho ganha o seu).
        if self.largura == 0 && self.altura == 0 {
            return self.nome.clone();
        }
        let marca = if self.primario { crate::idioma::t(" (principal)") } else { "" };
        format!("{} — {}×{}{}", self.nome, self.largura, self.altura, marca)
    }
}

/// Todos os monitores ativos, na ordem em que o sistema os enumera, com o primário primeiro.
pub fn monitores() -> Vec<Fonte> {
    let mut brutos = enumerar();
    // O primário primeiro: num notebook é quase sempre o que a pessoa quer transmitir, e a ordem
    // não pode depender da ordem de enumeração do sistema, que varia com a topologia.
    brutos.sort_by_key(|f| !f.primario);
    let nomes = nomes_amigaveis();
    for f in &mut brutos {
        if let Some(bonito) = nomes.iter().find(|(gdi, _)| *gdi == f.id).map(|(_, n)| n.clone()) {
            f.nome = bonito;
        }
    }
    // Desempate: dois "Generic PnP Monitor" na tela seriam indistinguíveis. Só numera quando
    // repete — numerar sempre polui o caso comum de um monitor só. A decisão sai da lista inteira
    // antes de renomear: no lugar, só o primeiro de cada par ganhava o sufixo (a revisão de código
    // de 18/09 achou o laço aqui e no das câmeras).
    let itens: Vec<(String, String)> = brutos
        .iter()
        .map(|f| (f.nome.clone(), f.id.rsplit('\\').next().unwrap_or("").to_string()))
        .collect();
    for (f, nome) in brutos.iter_mut().zip(crate::catalogo_de_cameras::desempatar(&itens)) {
        f.nome = nome;
    }
    brutos
}

/// As câmeras que **entram** no seletor, já no formato de linha, e as linhas de registro de todas
/// (inclusive as que ficaram de fora, com o motivo). Não ativa câmera nenhuma (`cameras.rs`).
///
/// Nomes repetidos (duas webcams iguais) ganham o trecho de instância do link
/// (`catalogo_de_cameras::instancia_curta`), como os monitores homônimos ganham o sufixo do
/// dispositivo.
///
/// `camera_de_bancada` é o link de `--camera-de-bancada` (§7.3 de `docs/camera-no-windows.md`): ele
/// entra **só** se for a câmera de bancada de uma sonda viva
/// (`catalogo_de_cameras::conferir_camera_de_bancada`); a recusa vai para as linhas do registro.
pub fn cameras(camera_de_bancada: Option<&str>) -> (Vec<Fonte>, Vec<String>) {
    let lista = match crate::cameras::catalogo() {
        Ok(l) => l,
        Err(e) => return (Vec::new(), vec![format!("câmeras: {e} — nenhuma entra no seletor")]), // i18n: fora (diário e bandeira de bancada)
    };
    let mut linhas = crate::cameras::linhas_do_registro(&lista);
    let mut liberada: Option<String> = None;
    if let Some(pedida) = camera_de_bancada {
        match lista.iter().find(|c| c.link.eq_ignore_ascii_case(pedida)) {
            None => linhas.push(format!("--camera-de-bancada: o link não está na enumeração ({pedida})")), // i18n: fora (diário e bandeira de bancada)
            Some(c) => {
                // De onde veio a imagem do PID: do processo, ou da lista de processos quando a sonda
                // roda elevada e este token não a abre (o R5, M55). A confirmação é a mesma.
                let como = std::cell::RefCell::new(String::new());
                let ler = |pid: u32| {
                    crate::cameras::imagem_e_como_foi_lida(pid).map(|(imagem, c)| {
                        *como.borrow_mut() = c;
                        imagem
                    })
                };
                match crate::catalogo_de_cameras::conferir_camera_de_bancada(&c.nome, &c.dono, &ler) {
                    Ok(pid) => {
                        linhas.push(format!(
                            // i18n: fora (diário e bandeira de bancada)
                            "câmera \"{}\" ENTRA — câmera de bancada liberada por --camera-de-bancada (a sonda de PID {pid}, \
                             quall_camera_local.exe lido {}) | {}",
                            c.nome,
                            como.borrow(),
                            c.link
                        ));
                        liberada = Some(c.link.clone());
                    }
                    Err(motivo) => linhas.push(format!(
                        "--camera-de-bancada RECUSADA: {motivo}{} | {}",
                        if como.borrow().is_empty() { String::new() } else { format!(" (a imagem lida {})", como.borrow()) },
                        c.link
                    )),
                }
            }
        }
    }
    let mut fontes: Vec<Fonte> = lista
        .into_iter()
        .filter(|c| c.dono.vai_para_o_seletor() || liberada.as_deref() == Some(c.link.as_str()))
        .map(|c| Fonte {
            id: c.link,
            nome: c.nome,
            largura: 0,
            altura: 0,
            primario: false,
            especie: Especie::Camera,
        })
        .collect();
    let itens: Vec<(String, String)> = fontes
        .iter()
        .map(|f| (f.nome.clone(), crate::catalogo_de_cameras::instancia_curta(&f.id)))
        .collect();
    for (f, nome) in fontes.iter_mut().zip(crate::catalogo_de_cameras::desempatar(&itens)) {
        f.nome = nome;
    }
    (fontes, linhas)
}

/// Reencontra o `HMONITOR` de uma [`Fonte`] no momento da captura.
///
/// Devolve `None` — e quem chama **falha** — quando o monitor não está mais conectado. Cair para
/// outro monitor seria transmitir a tela errada sem avisar ninguém.
pub fn achar_hmonitor(id: &str) -> Option<HMONITOR> {
    enumerar_handles().into_iter().find(|(nome, _)| nome == id).map(|(_, h)| h)
}

fn enumerar() -> Vec<Fonte> {
    enumerar_handles()
        .into_iter()
        .filter_map(|(nome_gdi, hmonitor)| {
            let info = info_do_monitor(hmonitor)?;
            let r = info.monitorInfo.rcMonitor;
            Some(Fonte {
                nome: nome_generico(&nome_gdi).unwrap_or_else(|| "Monitor".to_string()), // i18n: fora (o mesmo nos dois idiomas, e o nome entra na identidade da fonte)
                id: nome_gdi,
                largura: (r.right - r.left).unsigned_abs(),
                altura: (r.bottom - r.top).unsigned_abs(),
                primario: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                especie: Especie::Monitor,
            })
        })
        .collect()
}

fn enumerar_handles() -> Vec<(String, HMONITOR)> {
    let mut achados: Vec<(String, HMONITOR)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut achados as *mut Vec<(String, HMONITOR)> as isize),
        );
    }
    achados
}

unsafe extern "system" fn callback(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    dados: LPARAM,
) -> BOOL {
    let lista = unsafe { &mut *(dados.0 as *mut Vec<(String, HMONITOR)>) };
    if let Some(info) = info_do_monitor(hmonitor) {
        lista.push((de_utf16(&info.szDevice), hmonitor));
    }
    // Continuar enumerando.
    BOOL(1)
}

fn info_do_monitor(hmonitor: HMONITOR) -> Option<MONITORINFOEXW> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe {
        GetMonitorInfoW(hmonitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO)
    };
    ok.as_bool().then_some(info)
}

/// O nome que o GDI dá ao monitor ligado naquela saída: "Generic PnP Monitor", "DELL U2415".
///
/// Serve de piso. O nome bom vem do DisplayConfig (ver [`nomes_amigaveis`]); este continua aqui
/// porque o DisplayConfig às vezes não devolve linha para uma saída ativa, e um nome feio é melhor
/// que "Monitor 1".
fn nome_generico(nome_gdi: &str) -> Option<String> {
    let mut largo: Vec<u16> = nome_gdi.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dd = DISPLAY_DEVICEW {
        cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let ok = unsafe { EnumDisplayDevicesW(PCWSTR(largo.as_mut_ptr()), 0, &mut dd, 0) };
    if !ok.as_bool() {
        return None;
    }
    let nome = de_utf16(&dd.DeviceString);
    (!nome.trim().is_empty()).then_some(nome)
}

/// Pares (`\\.\DISPLAY1`, "DELL U2415") lidos do DisplayConfig.
///
/// É a única API do Windows que devolve o nome que está gravado no EDID do painel — o mesmo que a
/// pessoa vê em Configurações > Sistema > Vídeo. Falha inteira vira lista vazia: o chamador já tem
/// piso.
fn nomes_amigaveis() -> Vec<(String, String)> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    let mut n_caminhos: u32 = 0;
    let mut n_modos: u32 = 0;
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_caminhos, &mut n_modos) }
        != ERROR_SUCCESS
    {
        return Vec::new();
    }
    let mut caminhos = vec![DISPLAYCONFIG_PATH_INFO::default(); n_caminhos as usize];
    let mut modos = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modos as usize];
    if unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_caminhos,
            caminhos.as_mut_ptr(),
            &mut n_modos,
            modos.as_mut_ptr(),
            None,
        )
    } != ERROR_SUCCESS
    {
        return Vec::new();
    }
    caminhos.truncate(n_caminhos as usize);

    let mut pares = Vec::new();
    for caminho in &caminhos {
        let mut origem = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        origem.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        origem.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        origem.header.adapterId = caminho.sourceInfo.adapterId;
        origem.header.id = caminho.sourceInfo.id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut origem.header) } != ERROR_SUCCESS.0 as i32 {
            continue;
        }

        let mut alvo = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        alvo.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        alvo.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        alvo.header.adapterId = caminho.targetInfo.adapterId;
        alvo.header.id = caminho.targetInfo.id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut alvo.header) } != ERROR_SUCCESS.0 as i32 {
            continue;
        }

        let gdi = de_utf16(&origem.viewGdiDeviceName);
        let bonito = de_utf16(&alvo.monitorFriendlyDeviceName);
        if !gdi.is_empty() && !bonito.trim().is_empty() {
            pares.push((gdi, bonito));
        }
    }
    pares
}

/// **O nome GDI (`\\.\DISPLAYn`) de um alvo** — o par `AdapterLuid` + `TargetId` que um driver de
/// monitor virtual devolve ao criar o monitor (`monitor.rs`, `AlvoDoWindows`). `None` enquanto o
/// alvo não tiver caminho ativo: logo depois de criado, e depois de solto — é a testemunha por
/// reenumeração, casada por identidade do alvo e não pelo nome, que é posição de saída.
///
/// `adapter_luid` leva o `HighPart` nos 32 bits de cima. Nenhum chamador de produto ainda: é o
/// caminho que o monitor virtual vai usar (`docs/monitor-virtual-windows.md` §7).
pub fn nome_gdi_do_alvo(adapter_luid: u64, target_id: u32) -> Option<String> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    let mut n_caminhos: u32 = 0;
    let mut n_modos: u32 = 0;
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_caminhos, &mut n_modos) }
        != ERROR_SUCCESS
    {
        return None;
    }
    let mut caminhos = vec![DISPLAYCONFIG_PATH_INFO::default(); n_caminhos as usize];
    let mut modos = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modos as usize];
    if unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_caminhos,
            caminhos.as_mut_ptr(),
            &mut n_modos,
            modos.as_mut_ptr(),
            None,
        )
    } != ERROR_SUCCESS
    {
        return None;
    }
    caminhos.truncate(n_caminhos as usize);
    for caminho in &caminhos {
        let luid = caminho.targetInfo.adapterId;
        let deste = ((luid.HighPart as u32 as u64) << 32) | u64::from(luid.LowPart);
        if deste != adapter_luid || caminho.targetInfo.id != target_id {
            continue;
        }
        let mut origem = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        origem.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        origem.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        origem.header.adapterId = caminho.sourceInfo.adapterId;
        origem.header.id = caminho.sourceInfo.id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut origem.header) } != ERROR_SUCCESS.0 as i32 {
            continue;
        }
        let gdi = de_utf16(&origem.viewGdiDeviceName);
        if !gdi.is_empty() {
            return Some(gdi);
        }
    }
    None
}

fn de_utf16(bruto: &[u16]) -> String {
    let fim = bruto.iter().position(|c| *c == 0).unwrap_or(bruto.len());
    String::from_utf16_lossy(&bruto[..fim])
}
