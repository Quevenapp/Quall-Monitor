//! Servidor COM in-proc da câmera virtual do Quall no Windows.
//!
//! É uma *Frame Server Custom Media Source*: o Windows a instancia por CLSID, a partir do valor
//! `CustomCaptureSourceClsid` que `MFCreateVirtualCamera` grava no nó do dispositivo. Quem
//! instancia **não é o app do Quall** — é o Frame Server (`svchost.exe -k Camera`, conta
//! `NT AUTHORITY\LocalService`), e também o processo do app que consome a câmera.
//!
//! Registro: `regsvr32 quall_camera_fonte.dll` (exige administrador — escreve em
//! `HKLM\SOFTWARE\Classes\CLSID`).

#![cfg(windows)]

pub mod diario;
// A mesma barreira de diagnóstico do app, sem dependência do workspace/Win32 do app.
#[path = "../../../../apps/windows/src/higiene_do_registro.rs"]
mod higiene_do_registro;
mod fluxo;
mod fonte;
pub mod quadros;
pub mod ritmo;

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{implement, Interface, Ref, Result, GUID, HRESULT, PCWSTR};
use windows::core::BOOL;
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

/// CLSID da fonte de mídia. É o valor que vai em `--clsid` da sonda e, por ela, no
/// `CustomCaptureSourceClsid` do nó do dispositivo.
pub const CLSID_FONTE: GUID = GUID::from_u128(0x5c75fe52_9204_45f6_b143_58b1ac8048e5);
pub const CLSID_TEXTO: &str = "{5C75FE52-9204-45F6-B143-58B1AC8048E5}";
const NOME_AMIGAVEL: &str = "Quall Virtual Camera Source";

static MODULO: AtomicIsize = AtomicIsize::new(0);

#[no_mangle]
pub extern "system" fn DllMain(modulo: HMODULE, motivo: u32, _reservado: *mut c_void) -> BOOL {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if motivo == DLL_PROCESS_ATTACH {
        MODULO.store(modulo.0 as isize, Ordering::Relaxed);
        // A primeira linha do diário responde à pergunta que decide o desenho de IPC: **em qual
        // processo o Windows carregou esta DLL?**
        diga!("DLL carregada");
    }
    BOOL(1)
}

#[implement(IClassFactory)]
struct Fabrica;

impl IClassFactory_Impl for Fabrica_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<windows::core::IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        unsafe {
            if ppvobject.is_null() {
                return Err(E_POINTER.into());
            }
            *ppvobject = std::ptr::null_mut();
            if punkouter.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let fonte = match fonte::criar() {
                Ok(f) => f,
                Err(e) => {
                    diga!("CreateInstance: falhou ao criar a fonte: {e}");
                    return Err(e);
                }
            };
            let hr = fonte.query(&*riid, ppvobject);
            diga!("CreateInstance riid={:?} -> {:?}", *riid, hr);
            hr.ok()
        }
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();
    if rclsid.is_null() || *rclsid != CLSID_FONTE {
        diga!("DllGetClassObject: CLSID desconhecido");
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let fabrica: IClassFactory = Fabrica.into();
    let hr = fabrica.query(&*riid, ppv);
    diga!("DllGetClassObject -> {hr:?}");
    hr
}

/// Sempre `S_FALSE`: esta DLL nunca pede para ser descarregada.
///
/// Não é preguiça. Ela vive dentro de um `svchost` compartilhado com o serviço de câmera do
/// sistema; descarregá-la no meio de um `RequestSample` de outro fio derruba o serviço da máquina
/// inteira, não só a nossa câmera. O custo de nunca descarregar é alguns megabytes num processo
/// que já está no ar de qualquer forma.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    match registrar() {
        Ok(()) => {
            diga!("DllRegisterServer: ok");
            S_OK
        }
        Err(e) => {
            diga!("DllRegisterServer: {e}");
            e.code()
        }
    }
}

#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    let caminho = format!("SOFTWARE\\Classes\\CLSID\\{CLSID_TEXTO}");
    let r = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, pcwstr(&widen(&caminho))) };
    diga!("DllUnregisterServer -> {r:?}");
    S_OK
}

fn registrar() -> Result<()> {
    let dll = caminho_do_modulo();
    // **Não registrar caminho vazio.** `GetModuleFileNameW` devolve 0 quando falha de verdade, e
    // gravar `InprocServer32 = ""` deixaria o CLSID registrado apontando para lugar nenhum — pior
    // que não registrar, porque o `regsvr32` diria "êxito" e a câmera apareceria sem fonte.
    if dll.is_empty() {
        diga!("registrar: GetModuleFileNameW não devolveu caminho; NÃO registrando InprocServer32");
        return Err(E_POINTER.into());
    }
    let base = format!("SOFTWARE\\Classes\\CLSID\\{CLSID_TEXTO}");
    escrever_sz(&base, None, NOME_AMIGAVEL)?;
    // `ThreadingModel = Both`: é o que a câmera virtual do próprio Windows usa
    // (`CrossDeviceVirtualCameraSource.dll`, conferido no registro do Dell). Deixa o objeto ser
    // criado tanto num apartamento único quanto no multithread, que é o que o Frame Server faz.
    escrever_sz(&format!("{base}\\InprocServer32"), None, &dll)?;
    escrever_sz(&format!("{base}\\InprocServer32"), Some("ThreadingModel"), "Both")?;
    Ok(())
}

fn escrever_sz(subchave: &str, nome: Option<&str>, valor: &str) -> Result<()> {
    unsafe {
        let mut chave = HKEY::default();
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            pcwstr(&widen(subchave)),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut chave,
            None,
        )
        .ok()?;
        let dados = widen_bytes(valor);
        let nome_w = nome.map(widen);
        let pnome = match &nome_w {
            Some(w) => PCWSTR(w.as_ptr()),
            None => PCWSTR::null(),
        };
        let r = RegSetValueExW(chave, pnome, None, REG_SZ, Some(&dados));
        let _ = RegCloseKey(chave);
        r.ok()?;
    }
    Ok(())
}

fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}

fn widen(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn widen_bytes(s: &str) -> Vec<u8> {
    widen(s).iter().flat_map(|c| c.to_le_bytes()).collect()
}

fn caminho_do_modulo() -> String {
    let h = HMODULE(MODULO.load(Ordering::Relaxed) as *mut c_void);
    caminho_de_modulo(Some(h))
}

/// O caminho de um módulo carregado, **sem buffer fixo**.
///
/// `GetModuleFileNameW` é do padrão `(buf, cap)` e é o pior membro da família: ela **não** devolve
/// erro quando o buffer é curto. Ela copia o que cabe, devolve `nSize` — um número que passa por
/// sucesso — e só põe `ERROR_INSUFFICIENT_BUFFER` no último erro. Zero só sai em falha de verdade.
///
/// Isto não é hipótese. Este arquivo usava `[0u16; 520]` e mandava o resultado direto para
/// `HKLM\SOFTWARE\Classes\CLSID\{…}\InprocServer32` em [`registrar`]. Um caminho truncado ali
/// registra um servidor COM apontando para um arquivo que não existe: a câmera aparece na lista do
/// Zoom e **nunca entrega quadro**, sem erro em lugar nenhum — o mesmo formato de defeito que
/// parou de gravar o `pares.json` do macOS em 26/08 e a mesma família que
/// `tools/confere-fronteira.py` cobra nas cascas de C e Swift.
///
/// 520 não era absurdo: `MAX_PATH` é 260. Mas o Windows 10 1607+ tem caminho estendido até 32 767
/// unidades UTF-16 com a política de caminho longo ligada, e "cabe hoje nesta bancada" é
/// exatamente o julgamento que o repositório já pagou. O laço cresce até o retorno ser **menor**
/// que a capacidade, que é a única leitura em que "coube" e "cortei" se distinguem.
pub(crate) fn caminho_de_modulo(h: Option<HMODULE>) -> String {
    use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
    // Teto do caminho estendido do Windows, com o NUL. Acima disso o próprio sistema não tem o que
    // devolver, e crescer mais seria laço infinito num erro que não é de tamanho.
    const TETO: usize = 32_768;
    let mut cap = 260usize;
    loop {
        let mut buf = vec![0u16; cap];
        let n = unsafe { GetModuleFileNameW(h, &mut buf) } as usize;
        if n == 0 {
            return String::new();
        }
        if n < buf.len() || cap >= TETO {
            return String::from_utf16_lossy(&buf[..n]);
        }
        cap = (cap * 2).min(TETO);
    }
}
