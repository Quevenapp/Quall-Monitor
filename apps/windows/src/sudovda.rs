//! **O contrato do SudoVDA**, o driver de monitor virtual da bancada (`docs/monitor-virtual-windows.md`
//! §5.3): os IOCTL, as estruturas, o dispositivo de controle aberto, e o PnP que diz se ele está lá.
//!
//! Portado da sonda `bin/receita_monitor.rs` (módulos `sudovda` e `pnp`), que o transcreveu do
//! cabeçalho público `Common/Include/sudovda-ioctl.h` em SudoMaker/SudoVDA@a4b09fa2 (MIT/CC0), com o
//! comportamento de cada IOCTL lido no `Driver.cpp` do mesmo commit. Nada vem do Apollo nem do
//! Vibepollo (GPL-3). Só o necessário para o app: sem a linha do tempo, sem o `--morrer`.
//!
//! **Só a bancada usa o SudoVDA.** O driver do produto é pergunta aberta ao usuário (§4.3); o app só
//! chega aqui com `--varias-sessoes` e o monitor virtual pedido (a bandeira de bancada ou a fonte
//! "Tela estendida", que só aparece com o adaptador presente).
//!
//! # A identidade vira GUID e EDID, estáveis por aparelho
//!
//! O Windows grava uma entrada no banco de vídeo da pessoa para cada EDID novo, para sempre (§13.1:
//! 408 entradas `SMKD1CE` depois de uma manhã de séries novas). Por isso nada aqui é sorteado: o GUID
//! e os dois textos do EDID saem **só** de `PedidoDeMonitor::identidade` ([`guid_da_identidade`],
//! [`textos_do_edid`]). O mesmo aparelho, no mesmo índice, volta com o mesmo EDID.

#![cfg(windows)]

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, LUID};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;

/// `CTL_CODE` do `winioctl.h`.
pub const fn ctl_code(dispositivo: u32, funcao: u32, metodo: u32, acesso: u32) -> u32 {
    (dispositivo << 16) | (acesso << 14) | (funcao << 2) | metodo
}
const FILE_DEVICE_UNKNOWN: u32 = 0x22;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;

// sudovda-ioctl.h:10-15
pub const IOCTL_ADD: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS);
pub const IOCTL_REMOVE: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS);
pub const IOCTL_SET_RENDER_ADAPTER: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS);
pub const IOCTL_GET_WATCHDOG: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_ANY_ACCESS);
pub const IOCTL_PING: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x888, METHOD_BUFFERED, FILE_ANY_ACCESS);
pub const IOCTL_GET_PROTOCOL_VERSION: u32 = ctl_code(FILE_DEVICE_UNKNOWN, 0x8FF, METHOD_BUFFERED, FILE_ANY_ACCESS);

/// sudovda-ioctl.h:25 — o app recusa um driver que fale outro `major.minor`: as estruturas seriam outras.
pub const PROTOCOLO: (u8, u8, u8) = (0, 2, 1);
/// sudovda-ioctl.h:27
pub const HARDWARE_ID: &str = r"root\sudomaker\sudovda";
/// sudovda-ioctl.h:33
pub const INTERFACE: GUID = GUID::from_u128(0xe5bcc234_1e0c_418a_a0d4_ef8b7501414d);
/// Os monitores do SudoVDA aparecem como `DISPLAY\SMKD1CE\…` (fabricante `SMK`, produto `0xD1CE`,
/// `edid.h:9`) e o `monitorDevicePath` deles como `\\?\DISPLAY#SMKD1CE#…`. É por isto — e pelo
/// adaptador — que se reconhece um monitor nosso, **nunca** pelo nome GDI (que muda, E5).
pub const MARCA_DOS_MONITORES: &str = "SMKD1CE";

/// O espaço de nomes dos GUIDs do app: os 8 bytes de `Data4`. Diferente do da sonda
/// (`…8e51-75616c6c524d`), para que um monitor da sonda e um do app nunca tenham o mesmo GUID.
const ESPACO_DO_APP: [u8; 8] = [0x9B, 0x2E, b'Q', b'u', b'a', b'l', b'l', b'M'];

/// sudovda-ioctl.h:35-42. `DeviceName` vira o nome do produto no EDID e `SerialNumber` o texto de
/// série; os dois passam por `strlen` (`edid.h:43`, `:66`): até **13 bytes + NUL**.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AddParams {
    pub width: u32,
    pub height: u32,
    /// Em Hz (o driver multiplica por 1000 o que vier abaixo de 1000, `Driver.cpp:1546-1548`).
    pub refresh_rate: u32,
    pub monitor_guid: GUID,
    pub device_name: [u8; 14],
    pub serial_number: [u8; 14],
}

/// sudovda-ioctl.h:48-51
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AddOut {
    pub adapter_luid: LUID,
    pub target_id: u32,
}

/// sudovda-ioctl.h:44-46
#[repr(C)]
pub struct RemoveParams {
    pub monitor_guid: GUID,
}

/// sudovda-ioctl.h:53-55
#[repr(C)]
pub struct SetRenderAdapterParams {
    pub adapter_luid: LUID,
}

/// sudovda-ioctl.h:57-60
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WatchdogOut {
    pub timeout: u32,
    pub countdown: u32,
}

/// sudovda-ioctl.h:17-22 e :62-64 (o último campo é um `bool` de C++, lido como `u8`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ProtocolVersion {
    pub major: u8,
    pub minor: u8,
    pub incremental: u8,
    pub test_build: u8,
}

/// **O GUID do monitor, função só da identidade.** `Data1` são os 32 bits de baixo — os que o EDID
/// leva no campo de série (`edid.h:38`) e que têm de ser únicos entre os monitores vivos: o modo
/// preferido é achado comparando o EDID inteiro (`Driver.cpp:1079-1087`), e dois `Data1` iguais
/// fariam um herdar o modo do outro. Os 32 de cima vão em `Data2`/`Data3`; `Data4` é o espaço do app.
pub fn guid_da_identidade(identidade: u64) -> GUID {
    let de_cima = (identidade >> 32) as u32;
    GUID::from_values(identidade as u32, (de_cima >> 16) as u16, de_cima as u16, ESPACO_DO_APP)
}

/// O GUID é de um monitor deste app (o espaço de nomes em `Data4`)? É o que a varredura da partida
/// usa para soltar só os órfãos nossos e recusar quando há monitor de outro programa.
pub fn guid_e_do_app(g: &GUID) -> bool {
    g.data4 == ESPACO_DO_APP
}

/// **Os dois textos do EDID**: o nome do produto `Quall <índice + 1>` e a série `Q` + os 32 bits de
/// baixo em hexa. Nenhum dos dois leva o nome do aparelho: se a pessoa renomeasse o telefone, o EDID
/// mudaria e o Windows gravaria mais uma entrada no banco de vídeo dela. O nome que a pessoa vê na
/// lista de receptores continua sendo o do aparelho (a janela o tem).
pub fn textos_do_edid(identidade: u64, indice: usize) -> (String, String) {
    (format!("Quall Mon {}", indice + 1), format!("Q{:08X}", identidade as u32))
}

/// Um texto do EDID (`DeviceName`/`SerialNumber`): ASCII imprimível, de 1 a 13 bytes, com NUL.
pub fn texto_edid(s: &str) -> Result<[u8; 14], String> {
    if s.is_empty() || s.len() > 13 || !s.bytes().all(|b| (0x20..0x7F).contains(&b)) {
        return Err(format!(
            "'{s}' não serve como texto do EDID: tem de ser ASCII imprimível, de 1 a 13 caracteres \
             (o driver passa o campo por strlen, edid.h:43)"
        ));
    }
    let mut t = [0u8; 14];
    t[..s.len()].copy_from_slice(s.as_bytes());
    Ok(t)
}

/// O LUID como o app o guarda: `HighPart` nos 32 bits de cima.
pub fn luid_u64(l: LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | u64::from(l.LowPart)
}

pub fn luid_de(v: u64) -> LUID {
    LUID { LowPart: v as u32, HighPart: (v >> 32) as u32 as i32 }
}

/// O dispositivo de controle do SudoVDA aberto. O fio de ping abre o **seu** (um `ADD` demorado no
/// mesmo handle seguraria o ping atrás dele: E/S síncrona num handle sem `OVERLAPPED` é serializada).
pub struct Dispositivo {
    h: HANDLE,
}
// O handle é um número do kernel; usá-lo de duas threads é o que `DeviceIoControl` síncrono permite.
unsafe impl Send for Dispositivo {}
unsafe impl Sync for Dispositivo {}

impl Dispositivo {
    /// A DACL do dispositivo dá leitura e escrita a Todos (`SudoVDA.inf:39`): o usuário comum abre
    /// sem elevação.
    pub fn abrir(caminho: &str) -> windows::core::Result<Self> {
        let largo: Vec<u16> = caminho.encode_utf16().chain(std::iter::once(0)).collect();
        let h = unsafe {
            CreateFileW(
                PCWSTR(largo.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )?
        };
        Ok(Self { h })
    }

    /// **Todo IOCTL menos o `GET_WATCHDOG` recarrega o vigia** (`Driver.cpp:1488-1491`), venha de que
    /// processo vier.
    fn ioctl(&self, codigo: u32, entrada: Option<(*const c_void, usize)>, saida: Option<(*mut c_void, usize)>) -> windows::core::Result<u32> {
        let mut devolvidos = 0u32;
        unsafe {
            DeviceIoControl(
                self.h,
                codigo,
                entrada.map(|e| e.0),
                entrada.map_or(0, |e| e.1 as u32),
                saida.map(|s| s.0),
                saida.map_or(0, |s| s.1 as u32),
                Some(&mut devolvidos as *mut u32),
                None,
            )?;
        }
        Ok(devolvidos)
    }

    pub fn versao(&self) -> windows::core::Result<ProtocolVersion> {
        let mut v = ProtocolVersion::default();
        let n = self.ioctl(
            IOCTL_GET_PROTOCOL_VERSION,
            None,
            Some((&mut v as *mut ProtocolVersion as *mut c_void, size_of::<ProtocolVersion>())),
        )?;
        if (n as usize) < size_of::<ProtocolVersion>() {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_UNEXPECTED,
                format!("GET_PROTOCOL_VERSION devolveu {n} bytes"),
            ));
        }
        Ok(v)
    }

    /// O prazo do vigia em segundos e a contagem de agora. O único IOCTL que **não** o recarrega.
    pub fn vigia(&self) -> windows::core::Result<WatchdogOut> {
        let mut w = WatchdogOut::default();
        self.ioctl(IOCTL_GET_WATCHDOG, None, Some((&mut w as *mut WatchdogOut as *mut c_void, size_of::<WatchdogOut>())))?;
        Ok(w)
    }

    pub fn ping(&self) -> windows::core::Result<()> {
        self.ioctl(IOCTL_PING, None, None).map(|_| ())
    }

    /// `SET_RENDER_ADAPTER`. **O retorno não prova nada** (`Driver.cpp:1596-1610`): a testemunha é a
    /// placa sob a qual a saída do monitor aparece no DXGI. **Nunca trocar com o driver vivo**: da
    /// NVIDIA de volta para a Intel trava o adaptador até reiniciar (§12.3).
    pub fn fixar_placa(&self, luid: LUID) -> windows::core::Result<()> {
        let p = SetRenderAdapterParams { adapter_luid: luid };
        self.ioctl(
            IOCTL_SET_RENDER_ADAPTER,
            Some((&p as *const SetRenderAdapterParams as *const c_void, size_of::<SetRenderAdapterParams>())),
            None,
        )
        .map(|_| ())
    }

    /// `ADD`. Com um GUID que já está vivo o driver **devolve o monitor vivo** em vez de criar outro
    /// (`Driver.cpp:1520-1536`); sem conector livre, `STATUS_TOO_MANY_NODES` (`:1498-1500`).
    pub fn adicionar(&self, p: &AddParams) -> windows::core::Result<(AddOut, u32)> {
        let mut o = AddOut::default();
        let n = self.ioctl(
            IOCTL_ADD,
            Some((p as *const AddParams as *const c_void, size_of::<AddParams>())),
            Some((&mut o as *mut AddOut as *mut c_void, size_of::<AddOut>())),
        )?;
        Ok((o, n))
    }

    /// `REMOVE` pelo GUID: `IddCxMonitorDeparture` **só daquele** monitor (`Driver.cpp:1566-1595`).
    pub fn soltar(&self, guid: GUID) -> windows::core::Result<()> {
        let p = RemoveParams { monitor_guid: guid };
        self.ioctl(
            IOCTL_REMOVE,
            Some((&p as *const RemoveParams as *const c_void, size_of::<RemoveParams>())),
            None,
        )
        .map(|_| ())
    }
}

impl Drop for Dispositivo {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.h);
        }
    }
}

// ================================================================================================
// PnP: a interface de controle e a presença dos nós — só leitura
// ================================================================================================

pub mod pnp {
    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_PropertyW,
        CM_Locate_DevNodeW, CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CR_BUFFER_SMALL,
        CR_NO_SUCH_DEVNODE, CR_SUCCESS,
    };
    use windows::Win32::Devices::Properties::{DEVPKEY_Device_InstanceId, DEVPROPTYPE};

    fn largo(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn multi_sz(bruto: &[u16]) -> Vec<String> {
        bruto.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }

    /// As interfaces **presentes** de uma classe de interface (o padrão `(buf, cap)` do CfgMgr).
    pub fn interfaces(classe: &GUID) -> Result<Vec<String>, String> {
        for _ in 0..5 {
            let mut n = 0u32;
            let cr = unsafe { CM_Get_Device_Interface_List_SizeW(&mut n, classe, PCWSTR::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
            if cr != CR_SUCCESS {
                return Err(format!("CM_Get_Device_Interface_List_SizeW devolveu CONFIGRET {}", cr.0));
            }
            if n <= 1 {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u16; n as usize];
            let cr = unsafe { CM_Get_Device_Interface_ListW(classe, PCWSTR::null(), &mut buf, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
            if cr == CR_BUFFER_SMALL {
                continue;
            }
            if cr != CR_SUCCESS {
                return Err(format!("CM_Get_Device_Interface_ListW devolveu CONFIGRET {}", cr.0));
            }
            return Ok(multi_sz(&buf));
        }
        Err("a lista de interfaces cresceu entre as duas chamadas cinco vezes seguidas".into())
    }

    /// A interface de controle do SudoVDA, se o adaptador está presente e a publica. Só lê o PnP:
    /// não abre o dispositivo (abrir e mandar qualquer IOCTL recarrega o vigia).
    pub fn interface_do_sudovda() -> Result<Option<String>, String> {
        let v = interfaces(&super::INTERFACE)?;
        match v.len() {
            0 => Ok(None),
            1 => Ok(Some(v[0].clone())),
            n => Err(format!("{n} adaptadores SudoVDA: não sei qual é o da bancada")),
        }
    }

    /// **A testemunha do `Present` do PnP**: `CR_NO_SUCH_DEVNODE` é "não está"; qualquer outro código
    /// é "não deu para perguntar" — nunca "saiu".
    pub fn presenca(instancia: &str) -> Result<bool, u32> {
        let l = largo(instancia);
        let mut dn = 0u32;
        let cr = unsafe { CM_Locate_DevNodeW(&mut dn, PCWSTR(l.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) };
        if cr == CR_SUCCESS {
            Ok(true)
        } else if cr == CR_NO_SUCH_DEVNODE {
            Ok(false)
        } else {
            Err(cr.0)
        }
    }

    /// **Os monitores do SudoVDA presentes** (`DISPLAY\SMKD1CE\…`), cada um com o `ContainerId` do
    /// nó — que o driver põe igual ao GUID do monitor (`Driver.cpp:868`). É a varredura da partida:
    /// os do espaço de nomes do app são órfãos de uma corrida anterior; os outros são de outro
    /// programa.
    pub fn monitores_presentes() -> Result<Vec<(String, Option<GUID>)>, String> {
        use windows::Win32::Devices::DeviceAndDriverInstallation::{
            CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_GETIDLIST_FILTER_ENUMERATOR,
        };
        use windows::Win32::Devices::Properties::DEVPKEY_Device_ContainerId;
        let filtro = largo("DISPLAY");
        let mut todos = Vec::new();
        for _ in 0..5 {
            let mut n = 0u32;
            let cr = unsafe { CM_Get_Device_ID_List_SizeW(&mut n, PCWSTR(filtro.as_ptr()), CM_GETIDLIST_FILTER_ENUMERATOR) };
            if cr != CR_SUCCESS {
                return Err(format!("CM_Get_Device_ID_List_SizeW devolveu CONFIGRET {}", cr.0));
            }
            if n <= 1 {
                break;
            }
            let mut buf = vec![0u16; n as usize];
            let cr = unsafe { CM_Get_Device_ID_ListW(PCWSTR(filtro.as_ptr()), &mut buf, CM_GETIDLIST_FILTER_ENUMERATOR) };
            if cr == CR_BUFFER_SMALL {
                continue;
            }
            if cr != CR_SUCCESS {
                return Err(format!("CM_Get_Device_ID_ListW devolveu CONFIGRET {}", cr.0));
            }
            todos = multi_sz(&buf);
            break;
        }
        let prefixo = format!(r"DISPLAY\{}\", super::MARCA_DOS_MONITORES);
        let mut v = Vec::new();
        for i in todos {
            if !i.to_ascii_uppercase().starts_with(&prefixo) || presenca(&i) != Ok(true) {
                continue;
            }
            let l = largo(&i);
            let mut dn = 0u32;
            let mut guid = None;
            if unsafe { CM_Locate_DevNodeW(&mut dn, PCWSTR(l.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS {
                let mut tipo = DEVPROPTYPE(0);
                let mut g = GUID::zeroed();
                let mut tam = std::mem::size_of::<GUID>() as u32;
                let cr = unsafe {
                    CM_Get_DevNode_PropertyW(dn, &DEVPKEY_Device_ContainerId, &mut tipo, Some(&mut g as *mut GUID as *mut u8), &mut tam, 0)
                };
                if cr == CR_SUCCESS && tam as usize == std::mem::size_of::<GUID>() {
                    guid = Some(g);
                }
            }
            v.push((i, guid));
        }
        Ok(v)
    }

    /// O ID de instância do nó dono de uma interface — do monitor, a partir do `monitorDevicePath`.
    /// Lido **enquanto o monitor existe**: depois que ele sai, a interface some.
    pub fn instancia_da_interface(caminho: &str) -> Option<String> {
        let l = largo(caminho);
        let mut tipo = DEVPROPTYPE(0);
        let mut tam = 0u32;
        let cr = unsafe { CM_Get_Device_Interface_PropertyW(PCWSTR(l.as_ptr()), &DEVPKEY_Device_InstanceId, &mut tipo, None, &mut tam, 0) };
        if cr != CR_BUFFER_SMALL || tam == 0 {
            return None;
        }
        let mut buf = vec![0u8; tam as usize];
        let cr = unsafe {
            CM_Get_Device_Interface_PropertyW(PCWSTR(l.as_ptr()), &DEVPKEY_Device_InstanceId, &mut tipo, Some(buf.as_mut_ptr()), &mut tam, 0)
        };
        if cr != CR_SUCCESS {
            return None;
        }
        let u: Vec<u16> = buf[..(tam as usize).min(buf.len())].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        multi_sz(&u).into_iter().next()
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn os_ioctl_sao_os_do_cabecalho() {
        // CTL_CODE(0x22, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS) = 0x00222000, e assim por diante.
        assert_eq!(IOCTL_ADD, 0x0022_2000);
        assert_eq!(IOCTL_REMOVE, 0x0022_2004);
        assert_eq!(IOCTL_SET_RENDER_ADAPTER, 0x0022_2008);
        assert_eq!(IOCTL_GET_WATCHDOG, 0x0022_200C);
        assert_eq!(IOCTL_PING, 0x0022_2220);
        assert_eq!(IOCTL_GET_PROTOCOL_VERSION, 0x0022_23FC);
    }

    #[test]
    fn as_estruturas_tem_o_tamanho_do_c() {
        assert_eq!(size_of::<AddParams>(), 56);
        assert_eq!(size_of::<AddOut>(), 12);
        assert_eq!(size_of::<RemoveParams>(), 16);
        assert_eq!(size_of::<SetRenderAdapterParams>(), 8);
        assert_eq!(size_of::<WatchdogOut>(), 8);
        assert_eq!(size_of::<ProtocolVersion>(), 4);
        assert_eq!(std::mem::offset_of!(AddParams, monitor_guid), 12);
        assert_eq!(std::mem::offset_of!(AddParams, device_name), 28);
        assert_eq!(std::mem::offset_of!(AddParams, serial_number), 42);
    }

    #[test]
    fn o_guid_e_o_edid_sao_funcao_so_da_identidade() {
        let id = 0x1234_5678_9ABC_DEF0u64;
        let g = guid_da_identidade(id);
        assert_eq!(g.data1, 0x9ABC_DEF0, "Data1 = os 32 de baixo, os que o EDID leva");
        assert_eq!((g.data2, g.data3), (0x1234, 0x5678));
        assert_eq!(g, guid_da_identidade(id), "o mesmo toda vez");
        // Oito índices do mesmo aparelho: oito Data1 diferentes (o índice mora no byte de cima).
        let d1: std::collections::BTreeSet<u32> = (0..8u64).map(|i| guid_da_identidade((i << 24) | 0x00AB_CDEF).data1).collect();
        assert_eq!(d1.len(), 8);
        let (nome, serie) = textos_do_edid(id, 2);
        assert_eq!(nome, "Quall Mon 3");
        assert_eq!(serie, "Q9ABCDEF0");
        assert!(texto_edid(&nome).is_ok() && texto_edid(&serie).is_ok(), "cabem nos 13 do EDID");
        assert_eq!(textos_do_edid(u64::MAX, 7).0, "Quall Mon 8");
    }

    #[test]
    fn texto_do_edid_ate_13_ascii_com_nul() {
        assert!(texto_edid("").is_err());
        assert!(texto_edid("12345678901234").is_err());
        assert!(texto_edid("Quall — X").is_err());
        let t = texto_edid("1234567890123").unwrap();
        assert_eq!(t[13], 0);
    }

    #[test]
    fn o_luid_vai_e_volta() {
        let v = 0xFFFF_FFFE_0001_0717u64;
        assert_eq!(luid_u64(luid_de(v)), v);
    }
}
