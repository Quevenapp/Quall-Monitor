//! **O Quall instala o driver da tela estendida** (decisão do Bruno, 02/10/2026, noite;
//! `docs/monitor-virtual-windows.md` §15). As regras puras estão em `regras_do_driver.rs`; aqui fica
//! o Win32, em três partes:
//!
//! - [`elevado`]: o que roda **como administrador**, num processo à parte — o próprio
//!   `quall-app.exe` relançado pelo UAC com `--driver-tela-estendida <verbo> --cano <nome>` (ou
//!   `desinstalar --sem-cano`, pelo desinstalador do MSI). Os quatro arquivos do SudoVDA vêm de
//!   **dentro do exe** (`include_bytes!`), e nada do disco do usuário, das variáveis de ambiente do
//!   perfil ou do HKCU entra nas decisões dele;
//! - [`ler_situacao`]: o que a janela lê para decidir o ladrilho e o cartão dos Ajustes (sem
//!   elevação, só leitura);
//! - [`perguntar`] e [`comecar`]: a caixa que explica antes, e o lançamento elevado com o andamento
//!   lido pelo cano.
//!
//! # Por que o próprio exe, e não um auxiliar com `requireAdministrator`
//!
//! Um auxiliar seria mais um binário no MSI (e no MSIX) e mais um lugar para os arquivos do driver;
//! o próprio exe já os carrega embutidos e já tem o manifesto dos Common Controls. O ramo elevado é
//! interceptado na primeira linha do `main`, antes da instância única, do COM e do registro, e só
//! aceita os dois formatos exatos de `regras_do_driver::ler_pedido`. O UAC mostra o mesmo programa
//! que a pessoa abriu.
//!
//! # Por que SetupAPI, e não o nefcon
//!
//! O `nefconc --create-device-node` + `--install-driver` do §9 é, por dentro, o que o `devcon
//! install` da Microsoft faz: `SetupDiCreateDeviceInfoW` + `DIF_REGISTERDEVICE` +
//! `UpdateDriverForPlugAndPlayDevicesW`. Fazer isso aqui tira um executável de terceiro (1 MB) que
//! teria de ser conferido, gravado em disco e rodado como administrador.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_DevNode_Status, CM_Locate_DevNodeW, DiUninstallDevice, SetupDiCallClassInstaller,
    SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDevicePropertyW, SetupDiGetDeviceRegistryPropertyW,
    SetupDiSetDevicePropertyW, SetupDiSetDeviceRegistryPropertyW, SetupUninstallOEMInfW, UpdateDriverForPlugAndPlayDevicesW,
    CM_DEVNODE_STATUS_FLAGS, CM_LOCATE_DEVNODE_NORMAL, CM_LOCATE_DEVNODE_PHANTOM, CM_PROB, CR_SUCCESS, DICD_GENERATE_ID,
    DIF_REGISTERDEVICE, DIGCF_PRESENT, DN_HAS_PROBLEM, DN_STARTED, GUID_DEVCLASS_DISPLAY, GUID_DEVCLASS_MONITOR, HDEVINFO,
    INSTALLFLAG_NONINTERACTIVE, SPDRP_HARDWAREID, SP_DEVINFO_DATA,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_DriverInfPath, DEVPKEY_Device_DriverProvider, DEVPROPTYPE, DEVPROP_TYPE_STRING,
};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, DEVPROPKEY, ERROR_ALREADY_EXISTS, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SUCCESS, HANDLE, HLOCAL, HWND, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::Cryptography::{
    CertAddEncodedCertificateToStore, CertCloseStore, CertDeleteCertificateFromStore, CertFindCertificateInStore,
    CertOpenStore, CRYPT_INTEGER_BLOB, CERT_FIND_SHA1_HASH, CERT_OPEN_STORE_FLAGS, CERT_QUERY_ENCODING_TYPE,
    CERT_STORE_ADD_NEW, CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_LOCAL_MACHINE, HCERTSTORE, PKCS_7_ASN_ENCODING,
    X509_ASN_ENCODING,
};
use windows::Win32::Security::WinTrust::{
    WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, DeleteFileW, ReadFile, RemoveDirectoryW, WriteFile, CREATE_NEW, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_NONE, FILE_SHARE_READ, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    KEY_WOW64_64KEY, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, GetExitCodeProcess, IsWow64Process2, WaitForSingleObject, INFINITE,
};
use windows::Win32::UI::Shell::{
    IsUserAnAdmin, SHGetKnownFolderPath, ShellExecuteExW, FOLDERID_ProgramData, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC,
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::regras_do_driver::{self as regras, Acao, Andamento, Leitura, Linha, Resultado, Situacao};
use crate::registro;

// =============================================================================================
// O que vai junto, e as marcas
// =============================================================================================

/// Os quatro arquivos, **dentro do exe**. Os bytes são os de `terceiros/sudovda/`, e o teste de
/// `regras_do_driver` confere os mesmos arquivos contra os hashes fixados. **Na build da loja**
/// (feature `loja`) o exe não leva o driver: os quatro ficam vazios, e a conferência recusa.
#[cfg(feature = "loja")]
const EMBUTIDOS: [(&str, &[u8]); 4] = [("SudoVDA.inf", &[]), ("SudoVDA.cat", &[]), ("SudoVDA.dll", &[]), ("SudoVDA.cer", &[])];
#[cfg(not(feature = "loja"))]
const EMBUTIDOS: [(&str, &[u8]); 4] = [
    ("SudoVDA.inf", include_bytes!("../terceiros/sudovda/SudoVDA.inf")),
    ("SudoVDA.cat", include_bytes!("../terceiros/sudovda/SudoVDA.cat")),
    ("SudoVDA.dll", include_bytes!("../terceiros/sudovda/SudoVDA.dll")),
    ("SudoVDA.cer", include_bytes!("../terceiros/sudovda/SudoVDA.cer")),
];

fn embutido(nome: &str) -> &'static [u8] {
    EMBUTIDOS.iter().find(|(n, _)| *n == nome).map(|(_, b)| *b).unwrap_or(&[])
}

/// **A marca da instalação**, em `HKLM\SOFTWARE\Quall Monitor\TelaEstendida`: só administradores escrevem
/// ali. Não em `%ProgramData%\Quall`: no Dell essa pasta dá "modificar" a Todos (herdado da câmera
/// virtual, medido em 02/10), e uma marca forjada liberaria o botão de desinstalar.
///
/// A marca é escrita **antes** de cada efeito, e não no fim (a revisão adversarial, achado 1): se o
/// processo cair no meio, o que ele pôs fica anotado, a janela oferece "Desinstalar" e a
/// desinstalação desfaz o que a marca diz.
const CHAVE_DA_MARCA: &str = r"SOFTWARE\Quall Monitor\TelaEstendida";

/// A propriedade nossa no nó criado (a revisão, achado 5): o `ROOT\DISPLAY\0000` se repete; o nó do
/// Quall é o que tem esta marca, e só ele sai pelo botão.
const DEVPKEY_QUALL: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0xded66b10_9b8c_4b4e_b4f6_667a1c462af9),
    pid: 2,
};
const VALOR_DA_PROPRIEDADE: &str = "Quall Monitor";

/// O mutex de uma instalação por vez, na máquina inteira (dois cliques, duas sessões).
const MUTEX: &str = r"Global\QuallDriverTelaEstendida";

fn largo(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn de_largo(b: &[u16]) -> String {
    let fim = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..fim])
}

fn multi_sz(b: &[u16]) -> Vec<String> {
    b.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

fn ultimo_erro() -> String {
    let e = unsafe { GetLastError() };
    format!("0x{:08X}", e.0)
}

fn hr(e: &windows::core::Error) -> String {
    format!("0x{:08X}", e.code().0 as u32)
}

// --- a marca no HKLM ---

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Marca {
    /// "instalando" ou "instalado".
    estado: String,
    instancia: String,
    inf: String,
    cert_root: bool,
    cert_editores: bool,
}

fn abrir_a_marca(escrever: bool) -> Option<HKEY> {
    let mut h = HKEY::default();
    let acesso = if escrever { KEY_READ | KEY_WRITE } else { KEY_READ };
    let r = unsafe {
        if escrever {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(CHAVE_DA_MARCA),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                acesso | KEY_WOW64_64KEY,
                None,
                &mut h,
                None,
            )
        } else {
            windows::Win32::System::Registry::RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(CHAVE_DA_MARCA), None, acesso | KEY_WOW64_64KEY, &mut h)
        }
    };
    (r == ERROR_SUCCESS).then_some(h)
}

fn ler_texto(h: HKEY, nome: &str) -> String {
    let mut buf = vec![0u16; 512];
    let mut n = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(h, PCWSTR::null(), &HSTRING::from(nome), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut n)) };
    if r == ERROR_SUCCESS {
        de_largo(&buf)
    } else {
        String::new()
    }
}

fn ler_numero(h: HKEY, nome: &str) -> u32 {
    let mut v = 0u32;
    let mut n = 4u32;
    let r = unsafe { RegGetValueW(h, PCWSTR::null(), &HSTRING::from(nome), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut c_void), Some(&mut n)) };
    if r == ERROR_SUCCESS {
        v
    } else {
        0
    }
}

fn ler_marca() -> Option<Marca> {
    let h = abrir_a_marca(false)?;
    let m = Marca {
        estado: ler_texto(h, "Estado"),
        instancia: ler_texto(h, "Instancia"),
        inf: ler_texto(h, "Inf"),
        cert_root: ler_numero(h, "CertRoot") == 1,
        cert_editores: ler_numero(h, "CertEditores") == 1,
    };
    unsafe {
        let _ = RegCloseKey(h);
    }
    (!m.estado.is_empty()).then_some(m)
}

fn gravar_marca(m: &Marca) -> Result<(), String> {
    let h = abrir_a_marca(true).ok_or_else(|| format!("RegCreateKeyExW: {}", ultimo_erro()))?;
    let texto = |nome: &str, v: &str| -> WIN32_ERROR {
        let w = largo(v);
        let bytes = unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2) };
        unsafe { RegSetValueExW(h, &HSTRING::from(nome), None, REG_SZ, Some(bytes)) }
    };
    let numero = |nome: &str, v: bool| -> WIN32_ERROR {
        let b = (v as u32).to_le_bytes();
        unsafe { RegSetValueExW(h, &HSTRING::from(nome), None, REG_DWORD, Some(&b)) }
    };
    let rs = [
        texto("Estado", &m.estado),
        texto("Instancia", &m.instancia),
        texto("Inf", &m.inf),
        texto("Versao", regras::VERSAO),
        numero("CertRoot", m.cert_root),
        numero("CertEditores", m.cert_editores),
    ];
    unsafe {
        let _ = RegCloseKey(h);
    }
    match rs.iter().find(|r| **r != ERROR_SUCCESS) {
        Some(r) => Err(format!("RegSetValueExW: 0x{:08X}", r.0)),
        None => Ok(()),
    }
}

fn apagar_marca() -> Result<(), String> {
    let r = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(CHAVE_DA_MARCA)) };
    if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("RegDeleteTreeW: 0x{:08X}", r.0))
    }
}

// --- os nós do PnP ---

/// Um nó com o hardware ID do SudoVDA.
struct No {
    instancia: String,
    do_quall: bool,
    inf: String,
    provedor: String,
}

/// Uma lista de dispositivos da SetupAPI que se destrói sozinha.
struct Lista(HDEVINFO);

impl Drop for Lista {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn dados_vazios() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() }
}

fn propriedade_texto(lista: HDEVINFO, d: &SP_DEVINFO_DATA, chave: &DEVPROPKEY) -> String {
    let mut tipo = DEVPROPTYPE::default();
    let mut buf = vec![0u8; 1024];
    let ok = unsafe { SetupDiGetDevicePropertyW(lista, d, chave, &mut tipo, Some(&mut buf), None, 0) };
    if ok.is_ok() && tipo == DEVPROP_TYPE_STRING {
        let u: Vec<u16> = buf.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        de_largo(&u)
    } else {
        String::new()
    }
}

fn hardware_ids(lista: HDEVINFO, d: &SP_DEVINFO_DATA) -> Vec<String> {
    let mut buf = vec![0u8; 2048];
    let ok = unsafe { SetupDiGetDeviceRegistryPropertyW(lista, d, SPDRP_HARDWAREID, None, Some(&mut buf), None) };
    if ok.is_err() {
        return Vec::new();
    }
    let u: Vec<u16> = buf.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
    multi_sz(&u)
}

fn instancia(lista: HDEVINFO, d: &SP_DEVINFO_DATA) -> String {
    let mut buf = vec![0u16; 512];
    match unsafe { SetupDiGetDeviceInstanceIdW(lista, d, Some(&mut buf), None) } {
        Ok(()) => de_largo(&buf),
        Err(_) => String::new(),
    }
}

/// Os nós do SudoVDA na classe Display. `presentes`: só os presentes (ligados ou desligados); sem
/// isso, também os fantasmas.
fn nos_do_sudovda(presentes: bool) -> Vec<No> {
    let flags = if presentes { DIGCF_PRESENT } else { Default::default() };
    let Ok(h) = (unsafe { SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), PCWSTR::null(), None, flags) }) else {
        return Vec::new();
    };
    let lista = Lista(h);
    let mut v = Vec::new();
    for i in 0.. {
        let mut d = dados_vazios();
        if unsafe { SetupDiEnumDeviceInfo(lista.0, i, &mut d) }.is_err() {
            break;
        }
        if !regras::hardware_ids_do_sudovda(&hardware_ids(lista.0, &d)) {
            continue;
        }
        v.push(No {
            instancia: instancia(lista.0, &d),
            do_quall: propriedade_texto(lista.0, &d, &DEVPKEY_QUALL) == VALOR_DA_PROPRIEDADE,
            inf: propriedade_texto(lista.0, &d, &DEVPKEY_Device_DriverInfPath),
            provedor: propriedade_texto(lista.0, &d, &DEVPKEY_Device_DriverProvider),
        });
    }
    v
}

/// O nó desta instância tem a propriedade do Quall? `None`: o nó não existe (nem fantasma).
fn no_e_do_quall(inst: &str) -> Option<bool> {
    let l = largo(inst);
    let mut dn = 0u32;
    let cr = unsafe { CM_Locate_DevNodeW(&mut dn, PCWSTR(l.as_ptr()), CM_LOCATE_DEVNODE_PHANTOM) };
    if cr != CR_SUCCESS {
        return None;
    }
    let mut tipo = DEVPROPTYPE::default();
    let mut buf = vec![0u8; 64];
    let mut n = buf.len() as u32;
    let cr = unsafe { CM_Get_DevNode_PropertyW(dn, &DEVPKEY_QUALL, &mut tipo, Some(buf.as_mut_ptr()), &mut n, 0) };
    if cr != CR_SUCCESS || tipo != DEVPROP_TYPE_STRING {
        return Some(false);
    }
    let u: Vec<u16> = buf[..n as usize].chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
    Some(de_largo(&u) == VALOR_DA_PROPRIEDADE)
}

/// O nó está presente, iniciado e sem problema?
fn no_pronto(inst: &str) -> bool {
    let l = largo(inst);
    let mut dn = 0u32;
    if unsafe { CM_Locate_DevNodeW(&mut dn, PCWSTR(l.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS {
        return false;
    }
    let mut status = CM_DEVNODE_STATUS_FLAGS::default();
    let mut problema = CM_PROB::default();
    if unsafe { CM_Get_DevNode_Status(&mut status, &mut problema, dn, 0) } != CR_SUCCESS {
        return false;
    }
    status.0 & DN_STARTED.0 != 0 && status.0 & DN_HAS_PROBLEM.0 == 0
}

// =============================================================================================
// A situação, para a janela (sem elevação)
// =============================================================================================

/// O app tem identidade de pacote (veio de um MSIX, a loja)? `GetCurrentPackageFullName` devolve
/// `APPMODEL_ERROR_NO_PACKAGE` (15700) para quem não tem.
fn na_loja() -> bool {
    if cfg!(feature = "loja") {
        return true;
    }
    let mut n = 0u32;
    let r = unsafe { windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName(&mut n, None) };
    r.0 != 15700
}

/// A arquitetura nativa é x64? (Num ARM64 o Quall x64 roda emulado e o `IsWow64Process2` conta.)
pub fn x64_nativo() -> bool {
    use windows::Win32::System::SystemInformation::{IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64};
    let mut processo = IMAGE_FILE_MACHINE::default();
    let mut nativa = IMAGE_FILE_MACHINE::default();
    match unsafe { IsWow64Process2(GetCurrentProcess(), &mut processo, Some(&mut nativa)) } {
        Ok(()) => nativa == IMAGE_FILE_MACHINE_AMD64,
        // Sem a função (Windows 10 antigo): só existe x64 e x86 ali, e o exe é x64.
        Err(_) => true,
    }
}

/// **A situação de agora**, pelas leituras (PnP, HKLM, pacote). Barata o bastante para o
/// `WM_DEVICECHANGE` e a volta aos painéis; não para cada pulso.
pub fn ler_situacao() -> Situacao {
    regras::situacao(ler_leitura())
}

/// A situação para o **instalador avulso**: ele é um exe à parte, nunca "da loja", mesmo que o
/// Quall da loja esteja instalado e aberto ao lado.
pub fn ler_situacao_do_instalador() -> Situacao {
    regras::situacao(Leitura { na_loja: false, ..ler_leitura() })
}

fn ler_leitura() -> Leitura {
    let interface = matches!(crate::sudovda::pnp::interface_do_sudovda(), Ok(Some(_)));
    let no_presente = !nos_do_sudovda(true).is_empty();
    let marca = ler_marca().is_some_and(|m| m.instancia.is_empty() || no_e_do_quall(&m.instancia) != Some(false));
    Leitura { na_loja: na_loja(), x64: x64_nativo(), interface, no_presente, marca }
}

// =============================================================================================
// O processo elevado
// =============================================================================================

pub mod elevado {
    use super::*;

    /// O cano para a janela. Escrever nunca derruba a instalação: se a janela fechou, o processo
    /// segue até o fim (ou até desfazer) sozinho. **No instalador avulso** (`quall-driver.exe`, que
    /// já roda elevado) não há cano: as linhas viram o andamento e o diário do próprio processo
    /// (`local`).
    pub(super) struct Cano(Option<HANDLE>, Option<Acao>);

    impl Cano {
        fn local(acao: Acao) -> Cano {
            Cano(None, Some(acao))
        }

        fn abrir(nome: Option<&str>) -> Cano {
            let Some(nome) = nome else { return Cano(None, None) };
            let n = HSTRING::from(nome);
            let ate = Instant::now() + Duration::from_secs(5);
            loop {
                // Só escrita, e o servidor só pode **identificar** quem conectou, nunca agir como ele.
                let r = unsafe {
                    CreateFileW(
                        &n,
                        FILE_GENERIC_WRITE.0,
                        FILE_SHARE_NONE,
                        None,
                        OPEN_EXISTING,
                        FILE_FLAGS_AND_ATTRIBUTES(SECURITY_SQOS_PRESENT.0 | SECURITY_IDENTIFICATION.0),
                        None,
                    )
                };
                match r {
                    Ok(h) => return Cano(Some(h), None),
                    Err(e) if Instant::now() < ate && (e.code() == ERROR_PIPE_BUSY.to_hresult() || e.code() == ERROR_FILE_NOT_FOUND.to_hresult()) => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => return Cano(None, None),
                }
            }
        }

        pub(super) fn mandar(&mut self, l: Linha) {
            if let Some(acao) = self.1 {
                match l {
                    Linha::Passo(k) => {
                        anotar_local(format!("driver: passo {k}: {}", regras::nome_do_passo(acao, k).unwrap_or("?")));
                        mudar(Andamento::Rodando { acao, passo: k });
                    }
                    Linha::Diario(s) => anotar_local(s),
                    _ => {}
                }
                return;
            }
            let Some(h) = self.0 else { println!("{}", l.escrever().trim_end()); return };
            let s = l.escrever();
            if unsafe { WriteFile(h, Some(s.as_bytes()), None, None) }.is_err() {
                unsafe {
                    let _ = CloseHandle(h);
                }
                self.0 = None;
            }
        }

        pub(super) fn diario(&mut self, s: impl Into<String>) {
            self.mandar(Linha::Diario(s.into()));
        }
    }

    impl Drop for Cano {
        fn drop(&mut self) {
            if let Some(h) = self.0.take() {
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
        }
    }

    /// Uma falha: o passo, a chave do motivo e o técnico.
    pub(super) struct Falha(pub usize, pub &'static str, pub String);

    /// O mutex de uma instalação por vez, tomado **uma vez** por processo e guardado até o fim (o
    /// instalador avulso instala e desinstala no mesmo processo; tomar de novo daria "já existe").
    /// `false`: outro processo o tem.
    fn tomar_o_mutex() -> bool {
        static TOMADO: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *TOMADO.get_or_init(|| {
            let m = unsafe { CreateMutexW(None, true, &HSTRING::from(MUTEX)) };
            let ocupado = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            m.is_ok() && !ocupado
        })
    }

    /// **O instalador avulso** (`quall-driver.exe`, já elevado pelo manifesto): instala ou
    /// desinstala **neste processo**, com o mesmo código do processo elevado do app, e o andamento
    /// no global de [`andamento`](super::andamento). Bloqueia: chame numa thread (`comecar_aqui`).
    pub fn rodar_aqui(acao: Acao) -> Resultado {
        let mut cano = Cano::local(acao);
        if !unsafe { IsUserAnAdmin() }.as_bool() {
            return Resultado::Falha(0, regras::MOTIVO_SEM_ADMIN.into(), "IsUserAnAdmin".into());
        }
        if !tomar_o_mutex() {
            return Resultado::Falha(0, regras::MOTIVO_OUTRA.into(), String::new());
        }
        anotar_local(format!("driver: instalador avulso, {}", acao.verbo()));
        let r = match acao {
            Acao::Instalar => instalar(&mut cano, true),
            Acao::Desinstalar => desinstalar(&mut cano, false),
        };
        match r {
            Ok(true) => Resultado::OkReiniciar,
            Ok(false) => Resultado::Ok,
            Err(Falha(n, chave, tecnico)) => {
                anotar_local(format!("driver: !! parou no passo {n}: {chave} ({tecnico})"));
                Resultado::Falha(n, chave.into(), tecnico)
            }
        }
    }

    /// **A entrada do processo elevado.** Chamada pelo `main` antes de qualquer outra coisa, quando
    /// os argumentos começam com `--driver-tela-estendida`. Devolve o código de saída.
    pub fn rodar(args: &[String]) -> u32 {
        // Antes de tudo: nenhuma DLL da pasta do exe (que pode ser de quem a criou) carrega daqui
        // para a frente; só as do System32 (a revisão, achado 14).
        unsafe {
            let _ = windows::Win32::System::LibraryLoader::SetDefaultDllDirectories(
                windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32,
            );
        }
        let pedido = match regras::ler_pedido(args) {
            Some(Ok(p)) => p,
            _ => return regras::saida::RECUSADO,
        };
        let mut cano = Cano::abrir(pedido.cano.as_deref());
        if !unsafe { IsUserAnAdmin() }.as_bool() {
            cano.mandar(Linha::Falha(0, regras::MOTIVO_SEM_ADMIN.into(), "IsUserAnAdmin".into()));
            return regras::saida::SEM_ADMIN;
        }
        // Uma por vez, na máquina inteira. O mutex vive até o processo sair.
        if !tomar_o_mutex() {
            cano.mandar(Linha::Falha(0, regras::MOTIVO_OUTRA.into(), String::new()));
            return regras::saida::FALHA;
        }
        cano.diario(format!("driver: processo elevado, {} ({})", pedido.acao.verbo(), if pedido.cano.is_some() { "pela janela" } else { "pelo MSI" }));
        let r = match pedido.acao {
            // A setup keeps an existing adapter without adopting its ownership.
            Acao::Instalar if pedido.cano.is_none() && !nos_do_sudovda(true).is_empty() => {
                cano.diario("driver: adaptador pré-existente preservado pelo setup");
                Ok(false)
            }
            Acao::Instalar => instalar(&mut cano, pedido.confiar_certificado),
            Acao::Desinstalar => desinstalar(&mut cano, pedido.cano.is_none()),
        };
        match r {
            Ok(reiniciar) => {
                cano.mandar(if reiniciar { Linha::OkReiniciar } else { Linha::Ok });
                if reiniciar {
                    regras::saida::OK_REINICIAR
                } else {
                    regras::saida::OK
                }
            }
            Err(Falha(n, chave, tecnico)) => {
                cano.diario(format!("driver: !! parou no passo {n}: {chave} ({tecnico})"));
                cano.mandar(Linha::Falha(n, chave.into(), tecnico));
                regras::saida::FALHA
            }
        }
    }

    // --- a pasta de trabalho, só de administradores ---

    /// A pasta de trabalho: nova a cada vez, criada **por este processo** direto em `%ProgramData%`
    /// (pelo `SHGetKnownFolderPath`, nunca pela variável de ambiente, que o perfil do usuário define
    /// — a revisão, achado 3), com a DACL protegida "só SYSTEM e Administradores". O dono é o grupo
    /// Administradores; os Usuários não podem renomeá-la nem apagar o que está dentro (em
    /// `%ProgramData%` eles só criam). Some no `Drop`, em todo caminho de saída.
    pub(super) struct Pasta {
        pub caminho: String,
    }

    impl Pasta {
        fn nova() -> Result<Pasta, String> {
            let base = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }.map_err(|e| format!("SHGetKnownFolderPath: {}", hr(&e)))?;
            let base_s = unsafe { base.to_string() }.unwrap_or_default();
            unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(base.0 as *const c_void)) };
            if base_s.is_empty() {
                return Err("SHGetKnownFolderPath: vazio".into());
            }
            let mut aleatorio = [0u8; 8];
            aleatorio_do_sistema(&mut aleatorio)?;
            let caminho = format!(r"{base_s}\QuallMonitor-driver-{}", regras::hex(&aleatorio));
            let mut sd = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    &HSTRING::from("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"),
                    1,
                    &mut sd,
                    None,
                )
            }
            .map_err(|e| format!("SDDL: {}", hr(&e)))?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            // Falha se já existir: uma pasta que outro criou antes nunca é usada.
            let r = unsafe { CreateDirectoryW(&HSTRING::from(caminho.as_str()), Some(&sa)) };
            unsafe {
                let _ = LocalFree(Some(HLOCAL(sd.0)));
            }
            r.map_err(|e| format!("CreateDirectoryW: {}", hr(&e)))?;
            Ok(Pasta { caminho })
        }

        pub(super) fn arquivo(&self, nome: &str) -> String {
            format!(r"{}\{nome}", self.caminho)
        }

        /// Escreve os quatro arquivos (`CREATE_NEW`, sem compartilhar escrita) e os relê do disco,
        /// conferindo o SHA-256 outra vez.
        fn preparar(&self) -> Result<(), String> {
            for (nome, bytes) in EMBUTIDOS {
                let h = unsafe {
                    CreateFileW(&HSTRING::from(self.arquivo(nome)), FILE_GENERIC_WRITE.0, FILE_SHARE_NONE, None, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, None)
                }
                .map_err(|e| format!("CreateFileW {nome}: {}", hr(&e)))?;
                let r = unsafe { WriteFile(h, Some(bytes), None, None) };
                unsafe {
                    let _ = CloseHandle(h);
                }
                r.map_err(|e| format!("WriteFile {nome}: {}", hr(&e)))?;
            }
            let mut lidos = Vec::new();
            for (nome, bytes) in EMBUTIDOS {
                lidos.push((nome, ler_arquivo(&self.arquivo(nome), bytes.len() + 1)?));
            }
            let dados: Vec<(&str, &[u8])> = lidos.iter().map(|(n, b)| (*n, b.as_slice())).collect();
            regras::conferir_os_arquivos(&dados)
        }
    }

    impl Drop for Pasta {
        fn drop(&mut self) {
            for (nome, _) in EMBUTIDOS {
                unsafe {
                    let _ = DeleteFileW(&HSTRING::from(self.arquivo(nome)));
                }
            }
            unsafe {
                let _ = RemoveDirectoryW(&HSTRING::from(self.caminho.as_str()));
            }
        }
    }

    fn aleatorio_do_sistema(b: &mut [u8]) -> Result<(), String> {
        use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        let s = unsafe { BCryptGenRandom(None, b, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if s.is_ok() {
            Ok(())
        } else {
            Err(format!("BCryptGenRandom: 0x{:08X}", s.0 as u32))
        }
    }

    pub(super) fn ler_arquivo(caminho: &str, maximo: usize) -> Result<Vec<u8>, String> {
        let h = unsafe { CreateFileW(&HSTRING::from(caminho), FILE_GENERIC_READ.0, FILE_SHARE_READ, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None) }
            .map_err(|e| format!("CreateFileW {caminho}: {}", hr(&e)))?;
        let mut v = vec![0u8; maximo];
        let mut total = 0usize;
        loop {
            let mut n = 0u32;
            let r = unsafe { ReadFile(h, Some(&mut v[total..]), Some(&mut n), None) };
            if r.is_err() || n == 0 {
                break;
            }
            total += n as usize;
            if total >= v.len() {
                break;
            }
        }
        unsafe {
            let _ = CloseHandle(h);
        }
        v.truncate(total);
        Ok(v)
    }

    // --- o certificado ---

    const LOJAS: [&str; 2] = ["Root", "TrustedPublisher"];

    fn abrir_loja(nome: &str) -> Result<HCERTSTORE, String> {
        let n = largo(nome);
        unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE),
                Some(n.as_ptr() as *const c_void),
            )
        }
        .map_err(|e| format!("CertOpenStore {nome}: {}", hr(&e)))
    }

    fn impressao() -> Vec<u8> {
        (0..regras::IMPRESSAO_SHA1.len() / 2).map(|i| u8::from_str_radix(&regras::IMPRESSAO_SHA1[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect()
    }

    /// O certificado do SudoVDA está nesta loja?
    pub(super) fn certificado_na_loja(nome: &str) -> Result<bool, String> {
        let loja = abrir_loja(nome)?;
        let mut sha1 = impressao();
        let blob = CRYPT_INTEGER_BLOB { cbData: sha1.len() as u32, pbData: sha1.as_mut_ptr() };
        let c = unsafe {
            CertFindCertificateInStore(loja, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0, CERT_FIND_SHA1_HASH, Some(&blob as *const _ as *const c_void), None)
        };
        let achou = !c.is_null();
        unsafe {
            if achou {
                let _ = windows::Win32::Security::Cryptography::CertFreeCertificateContext(Some(c));
            }
            let _ = CertCloseStore(Some(loja), 0);
        }
        Ok(achou)
    }

    fn por_certificado(nome: &str) -> Result<(), String> {
        let loja = abrir_loja(nome)?;
        let r = unsafe { CertAddEncodedCertificateToStore(Some(loja), X509_ASN_ENCODING, embutido("SudoVDA.cer"), CERT_STORE_ADD_NEW, None) };
        unsafe {
            let _ = CertCloseStore(Some(loja), 0);
        }
        r.map_err(|e| format!("CertAddEncodedCertificateToStore {nome}: {}", hr(&e)))
    }

    pub(super) fn tirar_certificado(nome: &str) -> Result<(), String> {
        let loja = abrir_loja(nome)?;
        let mut sha1 = impressao();
        let blob = CRYPT_INTEGER_BLOB { cbData: sha1.len() as u32, pbData: sha1.as_mut_ptr() };
        let mut r = Ok(());
        loop {
            let c = unsafe {
                CertFindCertificateInStore(loja, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0, CERT_FIND_SHA1_HASH, Some(&blob as *const _ as *const c_void), None)
            };
            if c.is_null() {
                break;
            }
            // `CertDeleteCertificateFromStore` libera o contexto, dê certo ou não.
            if let Err(e) = unsafe { CertDeleteCertificateFromStore(c) } {
                r = Err(format!("CertDeleteCertificateFromStore {nome}: {}", hr(&e)));
                break;
            }
        }
        unsafe {
            let _ = CertCloseStore(Some(loja), 0);
        }
        r
    }

    /// Tira das lojas o que a marca diz que o Quall pôs, e atualiza a marca.
    fn desfazer_certificados(m: &mut Marca, cano: &mut Cano) {
        for (i, nome) in LOJAS.iter().enumerate() {
            let posto = if i == 0 { m.cert_root } else { m.cert_editores };
            if !posto {
                continue;
            }
            match tirar_certificado(nome) {
                Ok(()) => {
                    cano.diario(format!("driver: certificado tirado de LocalMachine\\{nome}"));
                    if i == 0 {
                        m.cert_root = false;
                    } else {
                        m.cert_editores = false;
                    }
                }
                Err(e) => cano.diario(format!("driver: !! o certificado não saiu de {nome}: {e}")),
            }
        }
    }

    // --- o catálogo ---

    /// O catálogo está assinado e a cadeia fecha (depois de o certificado entrar em `Root`)? Sem
    /// conferir revogação: o certificado é autoassinado e não tem lista.
    fn conferir_catalogo(caminho: &str) -> Result<(), String> {
        let c = largo(caminho);
        let mut arquivo = WINTRUST_FILE_INFO {
            cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(c.as_ptr()),
            ..Default::default()
        };
        let mut dados = WINTRUST_DATA {
            cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 { pFile: &mut arquivo },
            dwStateAction: WTD_STATEACTION_VERIFY,
            ..Default::default()
        };
        let mut acao = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        let r = unsafe { WinVerifyTrust(HWND(-1isize as *mut c_void), &mut acao, &mut dados as *mut _ as *mut c_void) };
        dados.dwStateAction = WTD_STATEACTION_CLOSE;
        unsafe {
            let _ = WinVerifyTrust(HWND(-1isize as *mut c_void), &mut acao, &mut dados as *mut _ as *mut c_void);
        }
        if r == 0 {
            Ok(())
        } else {
            Err(format!("WinVerifyTrust: 0x{:08X}", r as u32))
        }
    }

    // --- o nó e o driver ---

    /// Cria o nó raiz `ROOT\DISPLAY\NNNN` com o hardware ID do SudoVDA, e põe nele a propriedade do
    /// Quall. Devolve a instância.
    fn criar_no() -> Result<String, String> {
        let lista = Lista(unsafe { SetupDiCreateDeviceInfoList(Some(&GUID_DEVCLASS_DISPLAY), None) }.map_err(|e| format!("SetupDiCreateDeviceInfoList: {}", hr(&e)))?);
        let mut d = dados_vazios();
        unsafe { SetupDiCreateDeviceInfoW(lista.0, &HSTRING::from("Display"), &GUID_DEVCLASS_DISPLAY, PCWSTR::null(), None, DICD_GENERATE_ID, Some(&mut d)) }
            .map_err(|e| format!("SetupDiCreateDeviceInfoW: {}", hr(&e)))?;
        // REG_MULTI_SZ: o ID, o nulo dele e o nulo do fim; o tamanho em bytes.
        let mut ids: Vec<u16> = regras::HARDWARE_ID.encode_utf16().collect();
        ids.extend([0, 0]);
        let bytes = unsafe { std::slice::from_raw_parts(ids.as_ptr() as *const u8, ids.len() * 2) };
        unsafe { SetupDiSetDeviceRegistryPropertyW(lista.0, &mut d, SPDRP_HARDWAREID, Some(bytes)) }
            .map_err(|e| format!("SetupDiSetDeviceRegistryPropertyW: {}", hr(&e)))?;
        unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, lista.0, Some(&d)) }.map_err(|e| format!("DIF_REGISTERDEVICE: {}", hr(&e)))?;
        let inst = instancia(lista.0, &d);
        let valor = largo(VALOR_DA_PROPRIEDADE);
        let vb = unsafe { std::slice::from_raw_parts(valor.as_ptr() as *const u8, valor.len() * 2) };
        if let Err(e) = unsafe { SetupDiSetDevicePropertyW(lista.0, &d, &DEVPKEY_QUALL, DEVPROP_TYPE_STRING, Some(vb), 0) } {
            // Sem a propriedade o botão não reconheceria o nó: desfaz já.
            let _ = remover_no_da_lista(lista.0, &d);
            return Err(format!("SetupDiSetDevicePropertyW: {}", hr(&e)));
        }
        Ok(inst)
    }

    fn remover_no_da_lista(lista: HDEVINFO, d: &SP_DEVINFO_DATA) -> Result<bool, String> {
        let mut reiniciar = windows::core::BOOL(0);
        unsafe { DiUninstallDevice(HWND::default(), lista, d, 0, Some(&mut reiniciar)) }.map_err(|e| format!("DiUninstallDevice: {}", hr(&e)))?;
        Ok(reiniciar.as_bool())
    }

    /// Remove o nó desta instância (presente ou fantasma), **só** se ele tem o hardware ID do SudoVDA
    /// e a propriedade do Quall. Devolve se o Windows pede reinício.
    pub(super) fn remover_no(inst: &str, classe: &GUID, exigir_do_quall: bool) -> Result<bool, String> {
        let h = unsafe { SetupDiGetClassDevsW(Some(classe), PCWSTR::null(), None, Default::default()) }.map_err(|e| format!("SetupDiGetClassDevsW: {}", hr(&e)))?;
        let lista = Lista(h);
        for i in 0.. {
            let mut d = dados_vazios();
            if unsafe { SetupDiEnumDeviceInfo(lista.0, i, &mut d) }.is_err() {
                break;
            }
            if !instancia(lista.0, &d).eq_ignore_ascii_case(inst) {
                continue;
            }
            if exigir_do_quall
                && !(regras::hardware_ids_do_sudovda(&hardware_ids(lista.0, &d)) && propriedade_texto(lista.0, &d, &DEVPKEY_QUALL) == VALOR_DA_PROPRIEDADE)
            {
                return Err(format!("{inst}: sem DEVPKEY_QUALL"));
            }
            return remover_no_da_lista(lista.0, &d);
        }
        Ok(false)
    }

    fn instalar_driver(inf: &str) -> Result<bool, String> {
        let mut reiniciar = windows::core::BOOL(0);
        unsafe { UpdateDriverForPlugAndPlayDevicesW(None, &HSTRING::from(regras::HARDWARE_ID), &HSTRING::from(inf), INSTALLFLAG_NONINTERACTIVE, Some(&mut reiniciar)) }
            .map_err(|e| format!("UpdateDriverForPlugAndPlayDevicesW: {}", hr(&e)))?;
        Ok(reiniciar.as_bool())
    }

    /// O `oemNN.inf` publicado é o nosso? Pelo nome (formato), pelo conteúdo — o `%windir%\INF\oemNN.inf`
    /// é a cópia byte a byte do `.inf` original, então o SHA-256 tem de ser o do `SudoVDA.inf` — e
    /// pelo provedor do nó.
    pub(super) fn inf_publicado_e_nosso(nome: &str, provedor: &str) -> Result<bool, String> {
        if !regras::nome_publicado_valido(nome) {
            return Ok(false);
        }
        let mut buf = vec![0u16; 260];
        let n = unsafe { windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(Some(&mut buf)) } as usize;
        if n == 0 || n >= buf.len() {
            return Err(format!("GetSystemWindowsDirectoryW: {}", ultimo_erro()));
        }
        let caminho = format!(r"{}\INF\{nome}", String::from_utf16_lossy(&buf[..n]));
        let bytes = match ler_arquivo(&caminho, 64 * 1024) {
            Ok(b) => b,
            Err(_) => return Ok(false),
        };
        let inf = regras::ARQUIVOS[0];
        Ok(regras::hex(&regras::sha256(&bytes)).eq_ignore_ascii_case(inf.sha256) && (provedor.is_empty() || provedor == regras::PROVEDOR))
    }

    /// A testemunha: o adaptador do nó criado iniciado e sem problema, e a interface de controle
    /// presente. Até 10 s.
    fn testemunhar(inst: &str) -> bool {
        let ate = Instant::now() + Duration::from_secs(10);
        loop {
            let interface = matches!(crate::sudovda::pnp::interface_do_sudovda(), Ok(Some(_)));
            if interface && no_pronto(inst) {
                return true;
            }
            if Instant::now() >= ate {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // --- instalar ---

    fn instalar(cano: &mut Cano, confiar_certificado: bool) -> Result<bool, Falha> {
        // 1. Conferir antes de qualquer efeito: os arquivos, a plataforma, e que não há adaptador.
        cano.mandar(Linha::Passo(1));
        regras::conferir_os_arquivos(&EMBUTIDOS).map_err(|e| Falha(1, regras::MOTIVO_ARQUIVOS, e))?;
        cano.diario("driver: os quatro arquivos conferem (SHA-256 dos embutidos)");
        if !x64_nativo() {
            return Err(Falha(1, regras::MOTIVO_PLATAFORMA, String::new()));
        }
        let presentes = nos_do_sudovda(true);
        if let Some(n) = presentes.first() {
            return Err(Falha(1, regras::MOTIVO_JA_EXISTE, n.instancia.clone()));
        }
        // Silent setup never grants certificate trust. Existing trust is reusable;
        // otherwise the user must accept the explicit interactive installer/app dialog.
        if !confiar_certificado {
            for loja in LOJAS {
                let confiavel = certificado_na_loja(loja).map_err(|e| Falha(1, regras::MOTIVO_MARCA, e))?;
                if !confiavel {
                    return Err(Falha(1, "O certificado SudoVDA ainda não é confiável. Execute o instalador interativo e aceite a confirmação do driver.", loja.into()));
                }
            }
        }
        // Uma marca antiga (o nó dela sumiu): herda o que ela diz do certificado, para ele sair um
        // dia (a revisão, achado 6).
        let antiga = ler_marca();
        if let Some(m) = &antiga {
            cano.diario(format!("driver: marca antiga ({}, nó {:?}, CertRoot={}, CertEditores={})", m.estado, m.instancia, m.cert_root, m.cert_editores));
        }

        // 2. A pasta de trabalho, só de administradores, com os arquivos conferidos de novo do disco.
        cano.mandar(Linha::Passo(2));
        let pasta = Pasta::nova().map_err(|e| Falha(2, regras::MOTIVO_PASTA, e))?;
        pasta.preparar().map_err(|e| Falha(2, regras::MOTIVO_ARQUIVOS, e))?;
        cano.diario(format!("driver: pasta de trabalho {} (SY e BA), arquivos relidos e conferidos", pasta.caminho));

        // 3. O certificado, nas duas lojas. A marca vai antes de cada efeito.
        cano.mandar(Linha::Passo(3));
        let mut m = Marca {
            estado: "instalando".into(),
            cert_root: antiga.as_ref().is_some_and(|a| a.cert_root),
            cert_editores: antiga.as_ref().is_some_and(|a| a.cert_editores),
            ..Default::default()
        };
        gravar_marca(&m).map_err(|e| Falha(3, regras::MOTIVO_MARCA, e))?;
        for (i, nome) in LOJAS.iter().enumerate() {
            let ja = certificado_na_loja(nome).map_err(|e| Falha(3, regras::MOTIVO_CERTIFICADO, e))?;
            if ja {
                cano.diario(format!("driver: o certificado já estava em LocalMachine\\{nome}"));
                continue;
            }
            if i == 0 {
                m.cert_root = true;
            } else {
                m.cert_editores = true;
            }
            gravar_marca(&m).map_err(|e| Falha(3, regras::MOTIVO_MARCA, e))?;
            if let Err(e) = por_certificado(nome) {
                desfazer_certificados(&mut m, cano);
                let _ = gravar_marca(&m);
                limpar_marca_vazia(&m);
                return Err(Falha(3, regras::MOTIVO_CERTIFICADO, e));
            }
            cano.diario(format!("driver: certificado {} posto em LocalMachine\\{nome}", regras::ASSUNTO_DO_CERTIFICADO));
        }

        // 4. O catálogo: assinado, e a cadeia fecha agora.
        cano.mandar(Linha::Passo(4));
        if let Err(e) = conferir_catalogo(&pasta.arquivo("SudoVDA.cat")) {
            desfazer_certificados(&mut m, cano);
            let _ = gravar_marca(&m);
            limpar_marca_vazia(&m);
            return Err(Falha(4, regras::MOTIVO_CATALOGO, e));
        }
        cano.diario("driver: o catálogo confere (WinVerifyTrust)");

        // 5. O nó.
        cano.mandar(Linha::Passo(5));
        let inst = match criar_no() {
            Ok(i) => i,
            Err(e) => {
                desfazer_certificados(&mut m, cano);
                let _ = gravar_marca(&m);
                limpar_marca_vazia(&m);
                return Err(Falha(5, regras::MOTIVO_NO, e));
            }
        };
        m.instancia = inst.clone();
        let _ = gravar_marca(&m);
        cano.diario(format!("driver: nó {inst} criado ({})", regras::HARDWARE_ID));

        // 6. O driver.
        cano.mandar(Linha::Passo(6));
        let reiniciar = match instalar_driver(&pasta.arquivo("SudoVDA.inf")) {
            Ok(r) => r,
            Err(e) => {
                match remover_no(&inst, &GUID_DEVCLASS_DISPLAY, true) {
                    Ok(_) => {
                        cano.diario(format!("driver: nó {inst} tirado de volta"));
                        m.instancia.clear();
                    }
                    Err(e2) => cano.diario(format!("driver: !! o nó {inst} não saiu: {e2}")),
                }
                desfazer_certificados(&mut m, cano);
                let _ = gravar_marca(&m);
                limpar_marca_vazia(&m);
                return Err(Falha(6, regras::MOTIVO_DRIVER, e));
            }
        };
        if let Some(no) = nos_do_sudovda(false).into_iter().find(|n| n.instancia.eq_ignore_ascii_case(&inst)) {
            m.inf = no.inf.clone();
            cano.diario(format!("driver: driver instalado ({} de {}){}", no.inf, no.provedor, if reiniciar { "; o Windows pede reinício" } else { "" }));
        }
        let _ = gravar_marca(&m);

        // 7. A testemunha.
        cano.mandar(Linha::Passo(7));
        if reiniciar {
            m.estado = "instalado".into();
            let _ = gravar_marca(&m);
            return Ok(true);
        }
        if !testemunhar(&inst) {
            // Fica instalado (e com a marca): a pessoa vê nos Ajustes e pode desinstalar.
            m.estado = "instalado".into();
            let _ = gravar_marca(&m);
            return Err(Falha(7, regras::MOTIVO_TESTEMUNHA, inst));
        }
        m.estado = "instalado".into();
        gravar_marca(&m).map_err(|e| Falha(7, regras::MOTIVO_MARCA, e))?;
        cano.diario(format!("driver: \"{}\" presente e OK ({inst}); marca gravada em HKLM\\{CHAVE_DA_MARCA}", regras::NOME_DO_ADAPTADOR));
        drop(pasta);
        Ok(false)
    }

    /// Sem nó e sem certificado anotados, a marca não diz mais nada: sai.
    fn limpar_marca_vazia(m: &Marca) {
        if m.instancia.is_empty() && !m.cert_root && !m.cert_editores {
            let _ = apagar_marca();
        }
    }

    // --- desinstalar ---

    fn desinstalar(cano: &mut Cano, pelo_msi: bool) -> Result<bool, Falha> {
        let Some(mut m) = ler_marca() else {
            if pelo_msi {
                // O MSI pergunta sempre; sem a marca, o Quall não pôs nada, e não há o que tirar.
                return Ok(false);
            }
            return Err(Falha(0, regras::MOTIVO_SEM_MARCA, String::new()));
        };
        cano.diario(format!("driver: marca: {} nó {:?} inf {:?} CertRoot={} CertEditores={}", m.estado, m.instancia, m.inf, m.cert_root, m.cert_editores));
        let mut reiniciar = false;
        let mut primeira: Option<Falha> = None;
        let anotar = |f: Falha, primeira: &mut Option<Falha>| {
            if primeira.is_none() {
                *primeira = Some(f);
            }
        };

        // 1. O adaptador: só o da marca, e só se ele é o que o Quall criou.
        cano.mandar(Linha::Passo(1));
        let mut inf = m.inf.clone();
        let mut provedor = String::new();
        if !m.instancia.is_empty() {
            if let Some(no) = nos_do_sudovda(false).into_iter().find(|n| n.instancia.eq_ignore_ascii_case(&m.instancia)) {
                if no.do_quall {
                    if inf.is_empty() {
                        inf = no.inf.clone();
                    }
                    provedor = no.provedor.clone();
                    match remover_no(&m.instancia, &GUID_DEVCLASS_DISPLAY, true) {
                        Ok(r) => {
                            reiniciar |= r;
                            cano.diario(format!("driver: nó {} tirado{}", m.instancia, if r { " (pede reinício)" } else { "" }));
                            m.instancia.clear();
                            let _ = gravar_marca(&m);
                        }
                        Err(e) => anotar(Falha(1, regras::MOTIVO_NO_FICOU, e), &mut primeira),
                    }
                } else {
                    cano.diario(format!("driver: o nó {} não tem a marca do Quall; fica", m.instancia));
                    m.instancia.clear();
                }
            } else {
                m.instancia.clear();
            }
        }
        // Ghost monitors from other SudoVDA users are not ours to remove.
        // Runtime monitor removal uses this app's own GUID namespace.

        // 2. O pacote: só o `oemNN.inf` que é o nosso, e sem forçar (se outro adaptador o usa, fica).
        cano.mandar(Linha::Passo(2));
        if !inf.is_empty() {
            match inf_publicado_e_nosso(&inf, &provedor) {
                Ok(true) => {
                    let ok = unsafe { SetupUninstallOEMInfW(&HSTRING::from(inf.as_str()), 0, None) }.as_bool();
                    if ok {
                        cano.diario(format!("driver: pacote {inf} ({}) tirado do repositório de drivers", regras::NOME_ORIGINAL_DO_INF));
                        m.inf.clear();
                    } else {
                        anotar(Falha(2, regras::MOTIVO_PACOTE_FICOU, format!("SetupUninstallOEMInfW {inf}: {}", ultimo_erro())), &mut primeira);
                    }
                }
                Ok(false) => {
                    cano.diario(format!("driver: {inf} não é o pacote do SudoVDA (ou já saiu); fica"));
                    m.inf.clear();
                }
                Err(e) => anotar(Falha(2, regras::MOTIVO_PACOTE_FICOU, e), &mut primeira),
            }
            let _ = gravar_marca(&m);
        }

        // 3. O certificado, das lojas onde o Quall o pôs.
        cano.mandar(Linha::Passo(3));
        desfazer_certificados(&mut m, cano);
        let _ = gravar_marca(&m);

        // 4. As testemunhas.
        cano.mandar(Linha::Passo(4));
        let sobrou_no = nos_do_sudovda(false).into_iter().any(|n| n.do_quall);
        if sobrou_no {
            anotar(Falha(4, regras::MOTIVO_NO_FICOU, "DEVPKEY_QUALL".into()), &mut primeira);
        }
        for (i, nome) in LOJAS.iter().enumerate() {
            let posto = if i == 0 { m.cert_root } else { m.cert_editores };
            if posto {
                anotar(Falha(3, regras::MOTIVO_CERTIFICADO_FICOU, format!("LocalMachine\\{nome}")), &mut primeira);
            }
        }
        match primeira {
            Some(f) => {
                let _ = gravar_marca(&m);
                Err(f)
            }
            None => {
                apagar_marca().map_err(|e| Falha(4, regras::MOTIVO_MARCA, e))?;
                cano.diario("driver: nada do que o Quall pôs ficou; marca apagada");
                Ok(reiniciar)
            }
        }
    }

    fn fantasmas_do_sudovda() -> Vec<String> {
        let Ok(h) = (unsafe { SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_MONITOR), PCWSTR::null(), None, Default::default()) }) else {
            return Vec::new();
        };
        let lista = Lista(h);
        let mut v = Vec::new();
        for i in 0.. {
            let mut d = dados_vazios();
            if unsafe { SetupDiEnumDeviceInfo(lista.0, i, &mut d) }.is_err() {
                break;
            }
            let inst = instancia(lista.0, &d);
            if !regras::monitor_do_sudovda(&inst) {
                continue;
            }
            // Só os ausentes: um monitor do SudoVDA de pé é de alguém (a bancada, outro programa).
            let l = largo(&inst);
            let mut dn = 0u32;
            let presente = unsafe { CM_Locate_DevNodeW(&mut dn, PCWSTR(l.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS;
            if !presente {
                v.push(inst);
            }
        }
        v
    }
}

// =============================================================================================
// A janela: a caixa, o lançamento e o andamento
// =============================================================================================

static ANDAMENTO: Mutex<Andamento> = Mutex::new(Andamento::Parado);

/// **O diário do instalador avulso, em memória.** O `quall-driver.exe` roda inteiro como
/// administrador e não escreve arquivo nenhum em pasta do usuário (o `%LOCALAPPDATA%` é dele, e uma
/// junção ali levaria a escrita do administrador para outro lugar): as linhas ficam aqui e a janela
/// as mostra em "Detalhes".
static DIARIO_LOCAL: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn anotar_local(s: String) {
    if let Ok(mut d) = DIARIO_LOCAL.lock() {
        d.push(s);
    }
    VERSAO.fetch_add(1, Ordering::SeqCst);
}

/// As linhas do diário do instalador avulso até agora.
pub fn diario_local() -> Vec<String> {
    DIARIO_LOCAL.lock().map(|d| d.clone()).unwrap_or_default()
}
static VERSAO: AtomicU64 = AtomicU64::new(0);

/// O andamento de agora.
pub fn andamento() -> Andamento {
    ANDAMENTO.lock().map(|a| a.clone()).unwrap_or_default()
}

/// Sobe a cada mudança do andamento: a janela redesenha quando vê outra.
pub fn versao() -> u64 {
    VERSAO.load(Ordering::SeqCst)
}

fn mudar(a: Andamento) {
    if let Ok(mut g) = ANDAMENTO.lock() {
        *g = a;
    }
    VERSAO.fetch_add(1, Ordering::SeqCst);
}

/// **A caixa que explica antes.** `true` só com o clique em Instalar (ou Desinstalar). O botão leva
/// o escudo do UAC.
pub fn perguntar(hwnd: HWND, acao: Acao) -> bool {
    use crate::idioma::{t, tf};
    use windows::Win32::UI::Controls::{
        TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TASKDIALOG_BUTTON, TASKDIALOG_NOTIFICATIONS, TDCBF_CANCEL_BUTTON,
        TDF_ALLOW_DIALOG_CANCELLATION, TDF_POSITION_RELATIVE_TO_WINDOW, TDM_SET_BUTTON_ELEVATION_REQUIRED_STATE, TDN_CREATED,
        TD_SHIELD_ICON,
    };
    use windows::Win32::Foundation::{LPARAM, S_OK, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    const ID_SIM: i32 = 1000;
    unsafe extern "system" fn ao_criar(h: HWND, msg: TASKDIALOG_NOTIFICATIONS, _w: WPARAM, _l: LPARAM, _d: isize) -> windows::core::HRESULT {
        if msg == TDN_CREATED {
            // O escudo no botão: quem clica sabe que vem o UAC.
            unsafe {
                SendMessageW(h, TDM_SET_BUTTON_ELEVATION_REQUIRED_STATE.0 as u32, Some(WPARAM(ID_SIM as usize)), Some(LPARAM(1)));
            }
        }
        S_OK
    }

    let (titulo, pergunta, corpo, sim) = match acao {
        Acao::Instalar => (
            t(regras::CAIXA_TITULO).to_string(),
            t(regras::CAIXA_PERGUNTA).to_string(),
            [
                tf(regras::CAIXA_CORPO_O_QUE_E, &[&regras::VERSAO]),
                t(regras::CAIXA_CORPO_UAC).to_string(),
                tf(regras::CAIXA_CORPO_CERTIFICADO, &[&regras::ASSUNTO_DO_CERTIFICADO]),
            ]
            .join("\n\n"),
            t(regras::CAIXA_INSTALAR).to_string(),
        ),
        Acao::Desinstalar => (
            t(regras::CAIXA_DESINSTALAR_TITULO).to_string(),
            t(regras::CAIXA_DESINSTALAR_PERGUNTA).to_string(),
            [t(regras::CAIXA_DESINSTALAR_CORPO), t(regras::CAIXA_CORPO_UAC)].join("\n\n"),
            t(regras::CAIXA_DESINSTALAR).to_string(),
        ),
    };
    let (titulo, pergunta, corpo, sim) = (largo(&titulo), largo(&pergunta), largo(&corpo), largo(&sim));
    let botoes = [TASKDIALOG_BUTTON { nButtonID: ID_SIM, pszButtonText: PCWSTR(sim.as_ptr()) }];
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: hwnd,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(titulo.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 { pszMainIcon: TD_SHIELD_ICON },
        pszMainInstruction: PCWSTR(pergunta.as_ptr()),
        pszContent: PCWSTR(corpo.as_ptr()),
        cButtons: botoes.len() as u32,
        pButtons: botoes.as_ptr(),
        // O padrão é Cancelar: o Enter sem ler não instala.
        nDefaultButton: 2, // IDCANCEL
        pfCallback: Some(ao_criar),
        ..Default::default()
    };
    let mut clicado = 0i32;
    match unsafe { TaskDialogIndirect(&config, Some(&mut clicado), None, None) } {
        Ok(()) => {
            let sim = clicado == ID_SIM;
            registro::linha(format!("driver: a caixa de {} — {}", acao.verbo(), if sim { "a pessoa aceitou" } else { "cancelada" }));
            sim
        }
        Err(e) => {
            registro::linha(format!("driver: !! a caixa não abriu ({}); nada foi feito", hr(&e)));
            false
        }
    }
}

/// O SID do usuário deste processo, em texto (para a DACL do cano).
fn sid_do_usuario() -> Option<String> {
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::System::Threading::OpenProcessToken;
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut buf = vec![0u8; 256];
        let mut n = 0u32;
        let r = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut c_void), buf.len() as u32, &mut n);
        let _ = CloseHandle(token);
        r.ok()?;
        let usuario = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(usuario.User.Sid, &mut s).ok()?;
        let texto = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
        texto
    }
}

struct Handle(HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// **Começa** a instalação ou a desinstalação: o UAC, o processo elevado e o cano, numa thread. O
/// andamento muda por [`mudar`]; `ao_acabar` roda na thread, no fim (a janela recarrega a lista).
pub fn comecar(hwnd: HWND, acao: Acao, ao_acabar: impl FnOnce() + Send + 'static) {
    if !regras::pode_comecar(&andamento()) {
        registro::linha("driver: já há uma instalação ou desinstalação em andamento; o clique fica sem efeito");
        return;
    }
    mudar(Andamento::Rodando { acao, passo: 0 });
    let janela = hwnd.0 as isize;
    let r = std::thread::Builder::new().name("quall.driver".into()).spawn(move || {
        // O `ShellExecuteExW` pede COM na thread que o chama (a documentação dele); STA, como o
        // shell gosta, só nesta thread.
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
        let resultado = lancar(HWND(janela as *mut c_void), acao);
        registro::linha(format!("driver: {} terminou — {resultado:?}", acao.verbo()));
        mudar(Andamento::Acabou { acao, resultado });
        ao_acabar();
    });
    if let Err(e) = r {
        mudar(Andamento::Acabou { acao, resultado: Resultado::Falha(0, regras::MOTIVO_NAO_ABRIU.into(), e.to_string()) });
    }
}

/// **Começa no próprio processo** (o instalador avulso, já elevado): o mesmo andamento de
/// [`comecar`], sem UAC nem cano.
pub fn comecar_aqui(acao: Acao) {
    if !regras::pode_comecar(&andamento()) {
        return;
    }
    mudar(Andamento::Rodando { acao, passo: 0 });
    let r = std::thread::Builder::new().name("quall.driver".into()).spawn(move || {
        let resultado = elevado::rodar_aqui(acao);
        anotar_local(format!("driver: {} terminou — {resultado:?}", acao.verbo()));
        mudar(Andamento::Acabou { acao, resultado });
    });
    if let Err(e) = r {
        mudar(Andamento::Acabou { acao, resultado: Resultado::Falha(0, regras::MOTIVO_NAO_ABRIU.into(), e.to_string()) });
    }
}

fn lancar(hwnd: HWND, acao: Acao) -> Resultado {
    let falha = |c: &str, t: String| Resultado::Falha(0, c.to_string(), t);
    // O cano: nome aleatório, uma instância só (`FIRST_PIPE_INSTANCE`), só entrada, sem cliente
    // remoto, e a DACL deste usuário, dos Administradores e do SYSTEM.
    let mut aleatorio = [0u8; 16];
    if let Err(e) = elevado_aleatorio(&mut aleatorio) {
        return falha(regras::MOTIVO_NAO_ABRIU, e);
    }
    let nome = regras::nome_do_cano(u128::from_be_bytes(aleatorio));
    let Some(sid) = sid_do_usuario() else { return falha(regras::MOTIVO_NAO_ABRIU, "TokenUser".into()) };
    let mut sd = PSECURITY_DESCRIPTOR::default();
    if let Err(e) = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{sid})")), 1, &mut sd, None) } {
        return falha(regras::MOTIVO_NAO_ABRIU, format!("SDDL do cano: {}", hr(&e)));
    }
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    let h = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(nome.as_str()),
            FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_INBOUND.0 | FILE_FLAG_FIRST_PIPE_INSTANCE),
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            4096,
            0,
            Some(&sa),
        )
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    if h.is_invalid() {
        return falha(regras::MOTIVO_NAO_ABRIU, format!("CreateNamedPipeW: {}", ultimo_erro()));
    }
    let cano = Arc::new(Handle(h));

    // O próprio exe, elevado. `SW_HIDE`: o processo não tem janela (o console, se aparecer, some).
    let exe = match std::env::current_exe() {
        Ok(p) => p.display().to_string(),
        Err(e) => return falha(regras::MOTIVO_NAO_ABRIU, format!("current_exe: {e}")),
    };
    let (exe_w, parametros, verbo) = (largo(&exe), largo(&regras::parametros(acao, &nome)), largo("runas"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        hwnd,
        lpVerb: PCWSTR(verbo.as_ptr()),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(parametros.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    registro::linha(format!("driver: pedindo o UAC para {} ({exe})", acao.verbo()));
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        if e.code() == ERROR_CANCELLED.to_hresult() {
            registro::linha("driver: o UAC foi recusado; nada foi feito");
            return Resultado::CanceladoNoUac;
        }
        return falha(regras::MOTIVO_NAO_ABRIU, format!("ShellExecuteExW: {}", hr(&e)));
    }
    if info.hProcess.is_invalid() {
        return falha(regras::MOTIVO_NAO_ABRIU, "ShellExecuteExW: hProcess nulo".into());
    }
    let processo = Arc::new(Handle(info.hProcess));
    mudar(Andamento::Rodando { acao, passo: 0 });

    // Quem espera o processo: quando ele sai sem ter conectado, conecta-se ao próprio cano para
    // soltar o `ConnectNamedPipe` (e a leitura acaba no fim do arquivo).
    let conectou = Arc::new(AtomicBool::new(false));
    let vigia = {
        let (processo, conectou, nome) = (Arc::clone(&processo), Arc::clone(&conectou), nome.clone());
        std::thread::spawn(move || {
            unsafe { WaitForSingleObject(processo.0, INFINITE) };
            if !conectou.load(Ordering::SeqCst) {
                let r = unsafe { CreateFileW(&HSTRING::from(nome.as_str()), FILE_GENERIC_WRITE.0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None) };
                if let Ok(h) = r {
                    unsafe {
                        let _ = CloseHandle(h);
                    }
                }
            }
        })
    };

    let mut ultimo_passo = 0usize;
    let mut fim: Option<Linha> = None;
    let c = unsafe { ConnectNamedPipe(cano.0, None) };
    let ok = c.is_ok() || c.as_ref().is_err_and(|e| e.code() == ERROR_PIPE_CONNECTED.to_hresult());
    conectou.store(true, Ordering::SeqCst);
    if ok {
        let mut resto = String::new();
        let mut buf = [0u8; 1024];
        loop {
            let mut n = 0u32;
            if unsafe { ReadFile(cano.0, Some(&mut buf), Some(&mut n), None) }.is_err() || n == 0 {
                break;
            }
            resto.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
            while let Some(p) = resto.find('\n') {
                let linha: String = resto.drain(..=p).collect();
                match Linha::ler(&linha) {
                    Some(Linha::Passo(k)) => {
                        ultimo_passo = k;
                        registro::linha(format!("driver: passo {k}: {}", regras::nome_do_passo(acao, k).unwrap_or("?")));
                        mudar(Andamento::Rodando { acao, passo: k });
                    }
                    Some(Linha::Diario(s)) => registro::linha(s),
                    Some(l @ (Linha::Ok | Linha::OkReiniciar | Linha::Falha(..))) => fim = Some(l),
                    None => registro::linha(format!("driver: linha fora do protocolo: {:?}", linha.trim_end())),
                }
            }
            if resto.len() > 64 * 1024 {
                resto.clear();
            }
        }
    }
    let _ = vigia.join();
    let mut codigo = 0u32;
    let codigo = unsafe { GetExitCodeProcess(processo.0, &mut codigo) }.ok().map(|_| codigo);
    registro::linha(format!("driver: o processo elevado saiu com {codigo:?}"));
    regras::resultado(fim.as_ref(), ultimo_passo, codigo)
}

fn elevado_aleatorio(b: &mut [u8]) -> Result<(), String> {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let s = unsafe { BCryptGenRandom(None, b, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if s.is_ok() {
        Ok(())
    } else {
        Err(format!("BCryptGenRandom: 0x{:08X}", s.0 as u32))
    }
}
