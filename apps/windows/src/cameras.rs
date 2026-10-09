//! As câmeras deste PC, **sem abrir nenhuma**: enumerar e ler de quem cada uma é.
//!
//! A regra do dono e a da escolha moram em `catalogo_de_cameras.rs`, que é aritmética e tem os
//! testes. Aqui ficam as duas chamadas de sistema:
//!
//! - `MFEnumDeviceSources` com `VIDCAP`: a mesma enumeração que qualquer app faz, e a que devolve o
//!   `IMFActivate` que a captura vai ativar depois (`docs/camera-no-windows.md` §2.1). **Nunca**
//!   `ActivateObject` aqui: ativar é abrir, e abrir a webcam para desenhar uma lista é o tipo de
//!   coisa que ninguém espera (§6.1);
//! - o `CustomCaptureSourceClsid` do `Device Parameters` da interface **de toda câmera**, por
//!   `CM_Open_Device_Interface_KeyW` (que abre exatamente essa chave a partir do link) e, se ela
//!   falhar, pelo caminho montado de `catalogo_de_cameras::chaves_do_dono`. **Só leitura**
//!   (`KEY_READ`), e sem elevação: o produto roda com o token do usuário.
//!
//! Leitura que falha **esconde a câmera** e deixa o motivo no registro do app — a regra está em
//! `catalogo_de_cameras::classificar`, e o registro é quem conta o que ficou de fora.

use windows::core::{GUID, HSTRING, PWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Open_Device_Interface_KeyW, CR_SUCCESS, RegDisposition_OpenExisting,
};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, MFCreateAttributes, MFEnumDeviceSources,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    REG_VALUE_TYPE,
};

use crate::catalogo_de_cameras::{self as regra, Dono, Leitura};

/// `KSCATEGORY_VIDEO_CAMERA` (`ksmedia.h`; o crate `windows` 0.62 não traz). É a classe dos links
/// que o app guarda (os três do Dell terminam em `{e5323777-…}`), e por isso a classe em que a
/// janela registra o `WM_DEVICECHANGE` — a documentação registra `KSCATEGORY_CAPTURE`, cujo
/// `dbcc_name` traz a outra classe e nunca casaria com o link guardado
/// (`docs/camera-no-windows.md` §6.2).
pub const KSCATEGORY_VIDEO_CAMERA: GUID = GUID::from_u128(0xe5323777_f976_4f5b_9b55_b94699c46e44);

/// Uma câmera enumerada, com o dono já decidido — inclusive as que ficam fora do seletor, para o
/// registro dizer quais foram e por quê.
#[derive(Debug, Clone)]
pub struct CameraDoCatalogo {
    /// O link simbólico: a identidade guardada (`docs/camera-no-windows.md` §2.2).
    pub link: String,
    pub nome: String,
    pub dono: Dono,
    /// Por onde o dono foi lido, para o registro: `CM_Open_Device_Interface_KeyW`, a chave montada,
    /// ou nada (câmera que não é virtual do MF).
    pub lido_por: &'static str,
}

/// Todas as câmeras que o Media Foundation enumera, **sem ativar nenhuma**. Falha da enumeração
/// inteira vira lista vazia e uma linha de erro: o app continua com os monitores.
pub fn catalogo() -> Result<Vec<CameraDoCatalogo>, String> {
    let ativadores = enumerar().map_err(|e| format!("MFEnumDeviceSources falhou: {e}"))?;
    Ok(ativadores
        .into_iter()
        .map(|(nome, link)| {
            // O dono de **toda** câmera, e não só das `VCAMDEVAPI`: o nosso CLSID em qualquer link
            // fica de fora (a revisão de código de 18/09, achado 3 do catálogo). Uma chamada ao
            // registro por câmera, só leitura.
            let partes = regra::partes_do_link(&link);
            let (leitura, lido_por) = match &partes {
                Some(p) => ler_dono(&link, p),
                None => (None, "não lido (o link não tem a forma de link)"), // i18n: fora (registro do Windows e diário)
            };
            let dono = regra::classificar(&link, leitura.as_ref());
            CameraDoCatalogo { link, nome, dono, lido_por }
        })
        .collect())
}

/// **A câmera do link é do próprio Quall?** (uma baia, a câmera de bancada de uma sonda). Pelo dono
/// lido do registro, só leitura, sem ativar nada: é o "tipo da fonte" com que os ajustes de câmera
/// do R9 decidem ficar de fora (`docs/controles-de-camera.md` §2.2).
pub fn e_camera_do_quall(link: &str) -> bool {
    let Some(p) = regra::partes_do_link(link) else { return false };
    let (leitura, _) = ler_dono(link, &p);
    regra::classificar(link, leitura.as_ref()) == Dono::Quall
}

/// `(nome, link)` de cada câmera. Os `IMFActivate` são soltos aqui mesmo: quem for abrir uma câmera
/// reenumera e casa pelo link (a regra de `fontes.rs`: guardar o que sobrevive e casar na hora).
fn enumerar() -> windows::core::Result<Vec<(String, String)>> {
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))?;
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let mut ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        MFEnumDeviceSources(&attrs, &mut ptr, &mut n)?;
        let mut v = Vec::new();
        if !ptr.is_null() {
            for i in 0..n as usize {
                // `read` toma posse da referência, que cai no fim da volta.
                if let Some(a) = std::ptr::read(ptr.add(i)) {
                    let nome = texto(&a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
                    let link = texto(&a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK).unwrap_or_default();
                    v.push((nome, link));
                }
            }
            CoTaskMemFree(Some(ptr as *const _));
        }
        Ok(v)
    }
}

unsafe fn texto(a: &IMFAttributes, chave: &GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut n = 0u32;
    unsafe { a.GetAllocatedString(chave, &mut p, &mut n) }.ok()?;
    let s = unsafe { p.to_string() }.ok();
    if !p.is_null() {
        unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    }
    s
}

/// O `CustomCaptureSourceClsid` da interface. Primeiro pela API que abre a chave a partir do link;
/// se ela falhar, pelas chaves montadas.
fn ler_dono(link: &str, partes: &regra::PartesDoLink) -> (Option<Leitura>, &'static str) {
    let mut chave = HKEY::default();
    let cr = unsafe {
        CM_Open_Device_Interface_KeyW(&HSTRING::from(link), KEY_READ.0, RegDisposition_OpenExisting, &mut chave, 0)
    };
    if cr == CR_SUCCESS {
        let l = ler_valor(chave);
        unsafe {
            let _ = RegCloseKey(chave);
        }
        return (Some(l), "CM_Open_Device_Interface_KeyW");
    }
    let mut ultimo = format!("CM_Open_Device_Interface_KeyW devolveu CONFIGRET {}", cr.0);
    for caminho in regra::chaves_do_dono(partes) {
        let mut k = HKEY::default();
        let r = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(caminho.as_str()), None, KEY_READ, &mut k) };
        if r == ERROR_SUCCESS {
            let l = ler_valor(k);
            unsafe {
                let _ = RegCloseKey(k);
            }
            return (Some(l), "chave montada do link"); // i18n: fora (registro do Windows e diário)
        }
        ultimo = format!("{ultimo}; RegOpenKeyExW({caminho}) = {}", r.0);
    }
    (Some(Leitura::Falhou(ultimo)), "nenhuma chave abriu")
}

fn ler_valor(chave: HKEY) -> Leitura {
    let nome = HSTRING::from("CustomCaptureSourceClsid"); // i18n: fora (registro do Windows e diário)
    let mut tipo = REG_VALUE_TYPE::default();
    let mut tamanho = 0u32;
    let r = unsafe { RegQueryValueExW(chave, &nome, None, Some(&mut tipo), None, Some(&mut tamanho)) };
    if r == ERROR_FILE_NOT_FOUND {
        return Leitura::SemValor;
    }
    if r != ERROR_SUCCESS {
        return Leitura::Falhou(format!("RegQueryValueExW (tamanho) = {}", r.0)); // i18n: fora (registro do Windows e diário)
    }
    if tipo != REG_SZ {
        return Leitura::Falhou(format!("CustomCaptureSourceClsid com tipo {} e não REG_SZ", tipo.0)); // i18n: fora (registro do Windows e diário)
    }
    let mut buf = vec![0u16; (tamanho as usize).div_ceil(2) + 1];
    let mut bytes = (buf.len() * 2) as u32;
    let r = unsafe {
        RegQueryValueExW(chave, &nome, None, Some(&mut tipo), Some(buf.as_mut_ptr() as *mut u8), Some(&mut bytes))
    };
    if r != ERROR_SUCCESS {
        return Leitura::Falhou(format!("RegQueryValueExW = {}", r.0)); // i18n: fora (registro do Windows e diário)
    }
    let fim = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Leitura::Valor(String::from_utf16_lossy(&buf[..fim]))
}

/// **A interface da câmera está habilitada?** `DEVPKEY_DeviceInterface_Enabled` sobre o link, só
/// leitura (`docs/camera-no-windows.md` §6.2). É a testemunha de desconexão que "o link ainda existe"
/// não era: a interface de um aparelho ausente continua registrada (as sobras `Unknown` do §2.1), e
/// o nó da Câmera Conectada persiste entre boots (a revisão, 5). Barata: uma consulta ao
/// gerenciador de configuração, sem enumerar câmeras nem abrir o registro — o vigia da sessão a
/// chama a cada 200 ms na thread do `Ready`. `None` quando não deu para perguntar.
///
/// **A interface que não existe mais** (`CR_NO_SUCH_DEVICE_INTERFACE`) é `Some(false)`: é o que um
/// nó removido devolve, e "não habilitada" é o que importa para quem pergunta. Quando o nó da
/// câmera de bancada saiu, a leitura foi a `None` (M36) com um código não registrado; o vigia trata
/// `None` seguidos depois de `Some(true)` como sumiço (`regras_da_camera::TestemunhaDaInterface`), e
/// [`interface_habilitada_com_codigo`] diz o código para a bancada.
pub fn interface_habilitada(link: &str) -> Option<bool> {
    interface_habilitada_com_codigo(link).0
}

/// [`interface_habilitada`] e o `CONFIGRET` da consulta, para o registro da bancada.
pub fn interface_habilitada_com_codigo(link: &str) -> (Option<bool>, u32) {
    use windows::Win32::Devices::DeviceAndDriverInstallation::CM_Get_Device_Interface_PropertyW;
    use windows::Win32::Devices::Properties::{DEVPKEY_DeviceInterface_Enabled, DEVPROPTYPE, DEVPROP_TYPE_BOOLEAN};
    let mut tipo = DEVPROPTYPE::default();
    // `DEVPROP_BOOLEAN` é um byte: 0xFF verdadeiro, 0 falso. O tipo é conferido abaixo.
    let mut valor: u8 = 0;
    let mut tamanho = 1u32;
    let cr = unsafe {
        CM_Get_Device_Interface_PropertyW(
            &HSTRING::from(link),
            &DEVPKEY_DeviceInterface_Enabled,
            &mut tipo,
            Some(&mut valor as *mut u8),
            &mut tamanho,
            0,
        )
    };
    if cr == windows::Win32::Devices::DeviceAndDriverInstallation::CR_NO_SUCH_DEVICE_INTERFACE {
        return (Some(false), cr.0);
    }
    if cr != CR_SUCCESS || tipo != DEVPROP_TYPE_BOOLEAN || tamanho != 1 {
        return (None, cr.0);
    }
    (Some(valor != 0), cr.0)
}

/// O nome do executável do processo `pid`, se ele existe **e ainda roda**. Só leitura. Veio da
/// sonda (`quall_camera_local.rs`), para o produto conferir a câmera de bancada de
/// `--camera-de-bancada` (`catalogo_de_cameras::conferir_camera_de_bancada`).
pub fn imagem_de_processo_vivo(pid: u32) -> Option<String> {
    imagem_e_como_foi_lida(pid).map(|(imagem, _)| imagem)
}

/// Como a imagem de um processo foi lida, quando foi pelo próprio processo.
pub const PELO_PROCESSO: &str = "pelo processo";

/// **A imagem de um processo vivo, e de onde ela veio.** Primeiro pelo próprio processo
/// (`PROCESS_QUERY_LIMITED_INFORMATION`); quando ele **não se deixa abrir**, pela lista de processos
/// do sistema (Toolhelp32), que dá o mesmo nome de executável sem pedir acesso a ele.
///
/// O R5 (M55) mostrou o caso: a sonda roda elevada, pela Sessão 0 do SSH, e o app na sessão do
/// usuário com token limitado. O `OpenProcess` falhou, e a trava dizia "não é de um processo vivo".
/// O código da falha não foi lido; o esperado é `E_ACCESSDENIED` (um processo elevado se abre para
/// Administradores e SYSTEM, e o token limitado tem Administradores só para negar: a regra do
/// Windows, não medida aqui), e a linha do registro passa a dizer qual foi. **A confirmação
/// continua a mesma** — o PID do nome é de um processo vivo cujo executável é
/// `quall_camera_local.exe` —, lida de outro lugar: a trava não afrouxa. Por isso qualquer falha do
/// `OpenProcess` vai à lista, e não só a negação: um PID que não existe não está nela, e um
/// processo que já saiu também não, mesmo com alguém segurando o handle dele (o teste
/// `m55_o_processo_que_saiu_nao_esta_na_lista`).
pub fn imagem_e_como_foi_lida(pid: u32) -> Option<(String, String)> {
    match imagem_pelo_processo(pid) {
        Ok(Some(imagem)) => Some((imagem, PELO_PROCESSO.to_string())),
        Ok(None) => None,
        Err(SemAcesso(motivo)) => imagem_pela_lista_de_processos(pid)
            .map(|imagem| (imagem, format!("pela lista de processos do sistema ({motivo})"))), // i18n: fora (registro do Windows e diário)
    }
}

/// O processo não se deixou abrir, ou abriu e não disse a imagem: o motivo, para o registro.
struct SemAcesso(String);

/// Só o nome do arquivo de um caminho do Windows.
fn so_o_arquivo(caminho: &str) -> String {
    caminho.rsplit('\\').next().unwrap_or(caminho).to_string()
}

/// A imagem pela lista de processos do sistema (Toolhelp32): só processos vivos, sem abrir nenhum.
pub fn imagem_pela_lista_de_processos(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    unsafe {
        let foto = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut achado = None;
        let mut tem = Process32FirstW(foto, &mut e).is_ok();
        while tem {
            if e.th32ProcessID == pid {
                let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                achado = Some(so_o_arquivo(&String::from_utf16_lossy(&e.szExeFile[..n])));
                break;
            }
            tem = Process32NextW(foto, &mut e).is_ok();
        }
        let _ = CloseHandle(foto);
        achado
    }
}

/// A imagem pelo próprio processo: `Ok(None)` quando ele abriu e já saiu; `Err(SemAcesso)` quando
/// ele não se deixa abrir (inclusive o PID que não existe: a lista decide), ou abre e não diz a
/// imagem.
fn imagem_pelo_processo(pid: u32) -> Result<Option<String>, SemAcesso> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const AINDA_RODA: u32 = 259; // STILL_ACTIVE
    unsafe {
        let h = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(e) => return Err(SemAcesso(format!("o OpenProcess devolveu {:#010x}", e.code().0 as u32))), // i18n: fora (registro do Windows e diário)
        };
        let mut codigo = 0u32;
        let vivo = GetExitCodeProcess(h, &mut codigo).is_ok() && codigo == AINDA_RODA;
        // O padrão `(buf, cap)`: o `lpdwSize` de entrada é a **capacidade**, e com o caminho maior
        // que ela a chamada falha com `ERROR_INSUFFICIENT_BUFFER`. Então um `Vec` que cresce até o
        // teto de caminho longo do Windows (32.767), e o tamanho de saída conferido contra ela.
        let mut buf: Vec<u16> = vec![0; 260];
        let mut caminho = None;
        loop {
            let mut n = buf.len() as u32;
            match QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n) {
                Ok(()) if (n as usize) < buf.len() => {
                    caminho = Some(String::from_utf16_lossy(&buf[..n as usize]));
                    break;
                }
                Err(e) if e.code() == windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER.to_hresult() && buf.len() < 32_768 => {
                    buf.resize(buf.len() * 2, 0);
                }
                _ => break,
            }
        }
        let _ = CloseHandle(h);
        if !vivo {
            return Ok(None);
        }
        match caminho {
            Some(c) => Ok(Some(so_o_arquivo(&c))),
            None => Err(SemAcesso("o processo abriu e não disse a imagem".into())), // i18n: fora (registro do Windows e diário)
        }
    }
}

/// As linhas do catálogo para o registro do app: uma por câmera, com o dono e se entrou.
pub fn linhas_do_registro(lista: &[CameraDoCatalogo]) -> Vec<String> {
    lista
        .iter()
        .map(|c| {
            format!(
                "câmera \"{}\" {} — {} (dono lido por: {}) | {}", // i18n: fora (registro do Windows e diário)
                c.nome,
                if c.dono.vai_para_o_seletor() { "ENTRA" } else { "FORA" },
                c.dono.rotulo(),
                c.lido_por,
                c.link
            )
        })
        .collect()
}

#[cfg(test)]
mod testes {
    #[test]
    fn o_clsid_e_o_da_fonte() {
        // O número da regra e a constante da fonte de mídia não podem divergir: é por este CLSID que
        // uma câmera do Quall é reconhecida e escondida.
        assert_eq!(
            crate::catalogo_de_cameras::CLSID_DA_FONTE_DO_QUALL,
            quall_camera_fonte::CLSID_FONTE.to_u128()
        );
        assert_eq!(
            crate::catalogo_de_cameras::guid(quall_camera_fonte::CLSID_TEXTO),
            Some(quall_camera_fonte::CLSID_FONTE.to_u128())
        );
    }

    /// A lista de processos dá o mesmo nome de executável que o próprio processo (o R5, M55).
    #[test]
    fn m55_a_lista_de_processos_da_o_mesmo_nome_que_o_processo() {
        let eu = std::process::id();
        let pela_lista = super::imagem_pela_lista_de_processos(eu).expect("este processo está na lista");
        let exe = std::env::current_exe().unwrap();
        let esperado = exe.file_name().unwrap().to_string_lossy().to_string();
        assert!(pela_lista.eq_ignore_ascii_case(&esperado), "{pela_lista} contra {esperado}");
        let (pelo_processo, como) = super::imagem_e_como_foi_lida(eu).unwrap();
        assert!(pelo_processo.eq_ignore_ascii_case(&pela_lista));
        assert_eq!(como, super::PELO_PROCESSO, "este processo se deixa abrir");
        // Um PID que não existe falha no `OpenProcess`, vai à lista, e a lista não o tem.
        // Um PID que não existe (os PIDs do Windows são múltiplos de 4).
        assert_eq!(super::imagem_pela_lista_de_processos(0xFFFF_FFF1), None);
        assert_eq!(super::imagem_e_como_foi_lida(0xFFFF_FFF1), None);
    }

    /// **O processo que saiu não está na lista**, mesmo com o handle dele ainda aberto aqui: a
    /// leitura pela lista não confunde um processo morto com um vivo.
    #[test]
    fn m55_o_processo_que_saiu_nao_esta_na_lista() {
        let mut filho = std::process::Command::new("cmd.exe")
            .args(["/c", "exit", "0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("cmd.exe");
        let pid = filho.id();
        let _ = filho.wait();
        // `filho` ainda segura o handle do processo: ele existe como objeto, mas saiu.
        assert_eq!(super::imagem_pela_lista_de_processos(pid), None);
        assert_eq!(super::imagem_de_processo_vivo(pid), None);
        drop(filho);
    }
}
