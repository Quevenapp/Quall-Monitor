//! Escolha de adaptador e criação do dispositivo Direct3D 11 compartilhado entre captura e
//! encode.
//!
//! A escolha do adaptador é o que decide, na prática, se o encode sai pela NVENC da GTX 1660 Ti
//! ou pelo Quick Sync da UHD 630: cada GPU expõe seu próprio MFT de hardware, e o MFT fala com o
//! driver através do dispositivo D3D11 que a gente registra nele. Por isso enumeramos os
//! adaptadores por `IDXGIFactory1` e preferimos explicitamente o de VendorId da NVIDIA
//! (`0x10DE`), com a Intel (`0x8086`) como plano B — em vez de deixar o `D3D11CreateDevice` sem
//! `pAdapter` escolher o que o driver achar melhor, que num notebook Optimus costuma ser o
//! adaptador que compõe a tela (a Intel), não o mais forte.

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D::{D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ERROR_DEVICE_HUNG,
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_DRIVER_INTERNAL_ERROR,
    DXGI_ERROR_INVALID_CALL,
};
use windows::core::{Interface, Result, HRESULT};

pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_INTEL: u32 = 0x8086;

pub struct ChosenAdapter {
    pub device: ID3D11Device,
    /// O contexto imediato. O receptor o usa para ligar a proteção multithread — ver
    /// [`proteger_contexto`]; o encode e a captura falam com o dispositivo diretamente via COM.
    pub context: ID3D11DeviceContext,
    pub description: String,
    pub vendor_id: u32,
    /// O `AdapterLuid` do adaptador escolhido, com o `HighPart` nos 32 bits de cima. É o que o
    /// monitor virtual precisa para ser desenhado pela mesma placa que captura e codifica
    /// (`SET_RENDER_ADAPTER`, `monitores_virtuais.rs`).
    pub luid: u64,
}

/// **Liga (ou desliga) a proteção multithread do contexto imediato**, e devolve se ela já estava
/// ligada.
///
/// # Por que o receptor liga
///
/// O contexto imediato do D3D11 não é seguro entre threads: duas threads chamando nele ao mesmo
/// tempo corrompem o fluxo de comandos. O decoder H.264 da Microsoft em DXVA recebe o dispositivo
/// pelo `IMFDXGIDeviceManager` e o usa **de threads dele**; o laço da exibição usa o mesmo
/// contexto na thread da sessão (apresentar, escalar para a câmera virtual, `Flush`, `Map`) sem
/// passar pelo cadeado do gerenciador — o `LockDevice` só serializa quem o chama. Com a proteção
/// ligada, o próprio contexto serializa as chamadas.
///
/// Em 10/09/2026 o dispositivo do receptor caiu (`0x887A0005`) duas vezes com a câmera do S24 —
/// 5 s e 2,5 min depois de abrir —, e em nenhum lugar deste código a proteção era ligada; o Media
/// Foundation também não a liga (`depois_do_mft=não`, medido). **O A/B do mesmo dia não reproduziu
/// a queda em nenhum dos dois braços** (`docs/bancada.md` §8.70): a proteção fica porque é o que o
/// compartilhamento pede e custa ~4 % da volta do laço, não porque se provou que consertou.
/// `--sem-protecao-multithread` é o braço de antes.
///
/// O lado que emite (captura + encode) usa o mesmo [`create_device`] e **não** liga a proteção —
/// lá nunca houve queda registrada, e mexer sem medir não é conserto.
pub fn proteger_contexto(contexto: &ID3D11DeviceContext, ligar: bool) -> Result<bool> {
    let mt: ID3D11Multithread = contexto.cast()?;
    unsafe {
        let antes = mt.GetMultithreadProtected().as_bool();
        let _ = mt.SetMultithreadProtected(ligar);
        Ok(antes)
    }
}

/// O estado da proteção multithread do contexto, sem mexer nela. `None` se a interface não existe.
pub fn contexto_protegido(contexto: &ID3D11DeviceContext) -> Option<bool> {
    let mt: ID3D11Multithread = contexto.cast().ok()?;
    Some(unsafe { mt.GetMultithreadProtected() }.as_bool())
}

/// **O motivo que o driver dá para o dispositivo ter caído** — `None` enquanto ele está de pé.
///
/// Toda chamada num dispositivo caído devolve o mesmo `0x887A0005`, que não diz nada além de
/// "caiu". O motivo de verdade só sai daqui.
pub fn motivo_da_queda(device: &ID3D11Device) -> Option<HRESULT> {
    match unsafe { device.GetDeviceRemovedReason() } {
        Ok(()) => None,
        Err(e) => Some(e.code()),
    }
}

/// O nome do motivo, com o que a documentação do DXGI diz dele.
pub fn nome_do_motivo(motivo: HRESULT) -> &'static str {
    match motivo {
        DXGI_ERROR_DEVICE_HUNG => {
            "DEVICE_HUNG (comandos malformados enviados por este processo travaram a GPU)"
        }
        DXGI_ERROR_DEVICE_REMOVED => {
            "DEVICE_REMOVED (a placa saiu do sistema ou o driver foi atualizado)"
        }
        DXGI_ERROR_DEVICE_RESET => "DEVICE_RESET (um comando malformado derrubou o dispositivo)",
        DXGI_ERROR_DRIVER_INTERNAL_ERROR => "DRIVER_INTERNAL_ERROR (o driver falhou por dentro)",
        DXGI_ERROR_INVALID_CALL => "INVALID_CALL",
        _ => "motivo fora da lista do DXGI",
    }
}

/// Enumera os adaptadores físicos e devolve a descrição de cada um, na ordem do sistema — usado
/// só para log/diagnóstico (número 3 do contrato: "qual encoder foi realmente usado" começa por
/// aqui, mostrando o que estava disponível pra escolher).
pub fn list_adapters() -> Result<Vec<String>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut out = Vec::new();
    let mut i = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => a,
            Err(_) => break,
        };
        if let Ok(desc) = unsafe { adapter.GetDesc1() } {
            let name = String::from_utf16_lossy(
                &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len())],
            );
            out.push(format!("{name} (vendor 0x{:04X})", desc.VendorId));
        }
        i += 1;
    }
    Ok(out)
}

/// O adaptador deste LUID é um adaptador **indireto** (IddCx, como o do SudoVDA) ou de software?
/// Pelo `D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERTYPE)`, os bits `SoftwareDevice` (2) e
/// `IndirectDisplayDevice` (6) de `D3DKMT_ADAPTERTYPE` (`d3dkmthk.h`). FFI direto no `gdi32` para
/// não acrescentar uma feature ao crate `windows` por três funções. `None` = não deu para perguntar.
///
/// **Por que importa**: no Dell, o DXGI lista o adaptador do SudoVDA como *"Intel(R) UHD Graphics
/// 630"*, fornecedor 0x8086, ao lado da Intel de verdade, com monitor virtual ou sem. Em 18/09/2026
/// (Frente D) o do SudoVDA deu `0x0342` (Display, IndirectDisplayDevice; **sem** Render) e a Intel
/// `0x232B`. Quem escolhe placa por fabricante ([`create_device`], `monitores_virtuais::placas`)
/// pergunta aqui antes.
pub fn adaptador_indireto(luid: u64) -> Option<bool> {
    use std::ffi::c_void;
    use windows::Win32::Foundation::LUID;
    #[repr(C)]
    struct AbrirPeloLuid {
        luid: LUID,
        adaptador: u32,
    }
    #[repr(C)]
    struct Consulta {
        adaptador: u32,
        tipo: i32,
        dados: *mut c_void,
        tamanho: u32,
    }
    #[repr(C)]
    struct Fechar {
        adaptador: u32,
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn D3DKMTOpenAdapterFromLuid(p: *mut AbrirPeloLuid) -> i32;
        fn D3DKMTQueryAdapterInfo(p: *mut Consulta) -> i32;
        fn D3DKMTCloseAdapter(p: *mut Fechar) -> i32;
    }
    const KMTQAITYPE_ADAPTERTYPE: i32 = 15;
    let l = LUID { LowPart: luid as u32, HighPart: (luid >> 32) as u32 as i32 };
    let mut a = AbrirPeloLuid { luid: l, adaptador: 0 };
    if unsafe { D3DKMTOpenAdapterFromLuid(&mut a) } != 0 {
        return None;
    }
    let mut bits: u32 = 0;
    let mut c = Consulta { adaptador: a.adaptador, tipo: KMTQAITYPE_ADAPTERTYPE, dados: &mut bits as *mut u32 as *mut c_void, tamanho: 4 };
    let r = unsafe { D3DKMTQueryAdapterInfo(&mut c) };
    let mut f = Fechar { adaptador: a.adaptador };
    unsafe {
        let _ = D3DKMTCloseAdapter(&mut f);
    }
    (r == 0).then_some(bits & (1 << 2) != 0 || bits & (1 << 6) != 0)
}

/// Uma placa de hardware, como a DXGI a lista.
#[derive(Clone, Debug)]
pub struct PlacaDeHardware {
    pub luid: u64,
    pub vendor_id: u32,
    pub descricao: String,
}

/// **As placas de hardware**, na ordem da DXGI, sem as indiretas (o SudoVDA, que se apresenta com o
/// nome e o fabricante da Intel) e sem as de software ([`adaptador_indireto`] e o
/// `DXGI_ADAPTER_FLAG_SOFTWARE`). `None` do kernel fica como antes: concorre. É de onde a câmera tira
/// a placa: a do encoder que ativa, pelo LUID (`docs/camera-no-windows.md` §4.3, a revisão M4).
pub fn placas_de_hardware() -> Result<Vec<PlacaDeHardware>> {
    use windows::Win32::Graphics::Dxgi::DXGI_ADAPTER_FLAG_SOFTWARE;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut placas = Vec::new();
    let mut i = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => a,
            Err(_) => break,
        };
        i += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let luid = ((desc.AdapterLuid.HighPart as u32 as u64) << 32) | u64::from(desc.AdapterLuid.LowPart);
        if adaptador_indireto(luid) == Some(true) {
            continue;
        }
        let descricao = String::from_utf16_lossy(
            &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len())],
        );
        placas.push(PlacaDeHardware { luid, vendor_id: desc.VendorId, descricao });
    }
    Ok(placas)
}

/// **O dispositivo D3D11 da placa de um LUID, sem recuo**: a entrada do monitor virtual, em que a
/// placa é a que o `SET_RENDER_ADAPTER` fixou (`docs/monitor-virtual-windows.md` §14). Placa que não
/// está entre os adaptadores é erro — nunca "a primeira que aparecer", como faz [`create_device`]
/// (que o caminho de uma sessão só continua usando).
pub fn create_device_por_luid(luid: u64) -> Result<ChosenAdapter> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut i = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => a,
            Err(_) => break,
        };
        i += 1;
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        let deste = ((desc.AdapterLuid.HighPart as u32 as u64) << 32) | u64::from(desc.AdapterLuid.LowPart);
        if deste != luid {
            continue;
        }
        let description = String::from_utf16_lossy(
            &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len())],
        );
        let flags = D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0);
        let feature_levels = [D3D_FEATURE_LEVEL_11_0];
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut achieved: D3D_FEATURE_LEVEL = D3D_FEATURE_LEVEL_11_0;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut achieved),
                Some(&mut context),
            )?;
        }
        return Ok(ChosenAdapter {
            device: device.expect("D3D11CreateDevice não devolveu dispositivo"),
            context: context.expect("D3D11CreateDevice não devolveu contexto"),
            description,
            vendor_id: desc.VendorId,
            luid,
        });
    }
    Err(windows::core::Error::new(
        windows::Win32::Foundation::E_INVALIDARG,
        format!("nenhum adaptador DXGI com o LUID {luid:016X}"),
    ))
}

/// Cria o dispositivo D3D11 no adaptador pedido por `preferred_vendor` (`VENDOR_NVIDIA` ou
/// `VENDOR_INTEL`), se existir; senão tenta NVIDIA, depois Intel, depois o primeiro adaptador que
/// aparecer. `D3D11_CREATE_DEVICE_BGRA_SUPPORT` é obrigatório para o interop com
/// Windows.Graphics.Capture (que produz `B8G8R8A8`); `D3D11_CREATE_DEVICE_VIDEO_SUPPORT` é
/// obrigatório para o MFT de hardware aceitar textura direto via `IMFDXGIDeviceManager`.
///
/// O parâmetro importa mais do que parece: um `IMFTransform` de hardware só aceita
/// `MFT_MESSAGE_SET_D3D_MANAGER` com um dispositivo D3D11 criado no *mesmo* adaptador que o MFT —
/// registrar o dispositivo NVIDIA no MFT de Quick Sync (Intel) devolve `E_INVALIDARG`, medido
/// nesta bancada (ver `README.md`). Por isso o dispositivo só é criado depois de saber qual
/// encoder ativou de verdade, não antes.
///
/// **Adaptador indireto ou de software nunca é escolhido por fabricante** ([`adaptador_indireto`]),
/// salvo se não sobrar nenhum outro. O do SudoVDA se apresenta no DXGI com o nome e o fabricante da
/// placa que o desenha ("Intel(R) UHD Graphics 630", 0x8086), e "o primeiro 0x8086" só acertava pela
/// ordem da `EnumAdapters1`. Medido no Dell em 18/09/2026 (Frente D), sem monitor virtual nenhum:
/// na sessão interativa a Intel de verdade vem primeiro (é a placa da tela principal), mas na
/// Sessão 0 do SSH a ordem é a do kernel e o adaptador do SudoVDA vem em primeiro — e era nele que
/// este código criava o dispositivo das bancadas pelo SSH que pedem a Intel.
pub fn create_device(preferred_vendor: u32) -> Result<ChosenAdapter> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };

    let mut preferred: Option<IDXGIAdapter1> = None;
    let mut nvidia: Option<IDXGIAdapter1> = None;
    let mut intel: Option<IDXGIAdapter1> = None;
    let mut first: Option<IDXGIAdapter1> = None;
    let mut first_any: Option<IDXGIAdapter1> = None;
    let mut preferred_desc = String::new();
    let mut first_desc = String::new();
    let mut first_any_desc = String::new();
    let mut nvidia_desc = String::new();
    let mut intel_desc = String::new();

    let mut i = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => a,
            Err(_) => break,
        };
        if let Ok(desc) = unsafe { adapter.GetDesc1() } {
            let name = String::from_utf16_lossy(
                &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len())],
            );
            if first_any.is_none() {
                first_any = Some(adapter.clone());
                first_any_desc = name.clone();
            }
            // `None` (não deu para perguntar ao kernel) fica como antes: concorre. Só sai quem o
            // kernel diz que é indireto ou de software.
            let luid = ((desc.AdapterLuid.HighPart as u32 as u64) << 32) | u64::from(desc.AdapterLuid.LowPart);
            if adaptador_indireto(luid) == Some(true) {
                i += 1;
                continue;
            }
            if first.is_none() {
                first = Some(adapter.clone());
                first_desc = name.clone();
            }
            if desc.VendorId == preferred_vendor && preferred.is_none() {
                preferred = Some(adapter.clone());
                preferred_desc = name.clone();
            }
            if desc.VendorId == VENDOR_NVIDIA && nvidia.is_none() {
                nvidia = Some(adapter.clone());
                nvidia_desc = name.clone();
            }
            if desc.VendorId == VENDOR_INTEL && intel.is_none() {
                intel = Some(adapter.clone());
                intel_desc = name.clone();
            }
        }
        i += 1;
    }

    let (adapter, description, vendor_id) = if let Some(a) = preferred {
        (a, preferred_desc, preferred_vendor)
    } else if let Some(a) = nvidia {
        (a, nvidia_desc, VENDOR_NVIDIA)
    } else if let Some(a) = intel {
        (a, intel_desc, VENDOR_INTEL)
    } else if let Some(a) = first {
        (a, first_desc, 0)
    } else {
        (first_any.expect("nenhum adaptador DXGI encontrado"), first_any_desc, 0)
    };
    // Só lido, para quem precisa saber a placa (o monitor virtual): a escolha acima não muda.
    let luid = unsafe { adapter.GetDesc1() }
        .map(|d| ((d.AdapterLuid.HighPart as u32 as u64) << 32) | u64::from(d.AdapterLuid.LowPart))
        .unwrap_or(0);

    let flags = D3D11_CREATE_DEVICE_FLAG(
        D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0,
    );
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut achieved: D3D_FEATURE_LEVEL = D3D_FEATURE_LEVEL_11_0;

    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            flags,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut achieved),
            Some(&mut context),
        )?;
    }

    Ok(ChosenAdapter {
        device: device.expect("D3D11CreateDevice não devolveu dispositivo"),
        context: context.expect("D3D11CreateDevice não devolveu contexto"),
        description,
        vendor_id,
        luid,
    })
}
