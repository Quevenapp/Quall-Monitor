//! Encode H.264 em hardware via Media Foundation Transform (MFT) assíncrono.
//!
//! Por que Media Foundation e não a NVIDIA Video Codec SDK (NVENC) direto: os dois fabricantes
//! da bancada (NVIDIA e Intel) publicam seus encoders de hardware como MFTs registrados na
//! categoria `MFT_CATEGORY_VIDEO_ENCODER` com a flag `MFT_ENUM_FLAG_HARDWARE` — é o mesmo
//! mecanismo que o OBS e o próprio Windows (Xbox Game Bar, Netflix app) usam para gravar/
//! transmitir com aceleração de hardware sem se atar a um fabricante. Integrar o SDK da NVIDIA
//! direto exigiria os headers proprietários `nvEncodeAPI.h` (não presentes no ambiente, não são
//! redistribuíveis livremente) e um caminho de código totalmente separado pro Quick Sync via
//! Media Foundation mesmo assim — ou seja, dois pipelines pra manter. Um MFT de hardware,
//! escolhido pelo fornecedor da textura (o dispositivo D3D11 do adaptador NVIDIA, criado em
//! `device.rs`), me dá NVENC como caminho primário e Quick Sync como plano B com uma única
//! implementação. O preço: menos controle fino sobre parâmetros específicos da NVENC (ex.:
//! `lookahead`, `multi-pass`) que só o SDK exporia — aceitável pro contrato desta entrega.
//!
//! MFTs de hardware são **assíncronos**: não dá pra chamar `ProcessInput`/`ProcessOutput` direto,
//! é preciso desbloquear (`MF_TRANSFORM_ASYNC_UNLOCK`) e dirigir a máquina de estados através dos
//! eventos `METransformNeedInput` / `METransformHaveOutput` do `IMFMediaEventGenerator`. É mais
//! código que o MFT síncrono do encoder por software da Microsoft, mas é o único jeito de chegar
//! na NVENC/Quick Sync por Media Foundation.

use std::mem::ManuallyDrop;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};

use windows::core::{Interface, Result, GUID, PWSTR};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{
    VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_I4, VT_UI4,
};
use windows::Win32::Foundation::{VARIANT_FALSE, VARIANT_TRUE};

/// `Clone` só copia as referências COM (é o que a entrada do monitor virtual usa para garantir o
/// `desligar` em todo caminho de erro — `Cadeia::abrir_no_monitor_virtual`); não cria outro encoder.
#[derive(Clone)]
pub struct ChosenEncoder {
    /// O `IMFActivate` que **criou** este transform.
    ///
    /// Guardado por duas razões, as duas medidas nesta bancada na frente da quinta porta:
    ///
    /// 1. **`ShutdownObject` é a contrapartida de `ActivateObject`.** Sem ela o objeto criado pelo
    ///    activate não é liberado por inteiro. Medido: 20 recriações de encoder sem esta chamada
    ///    deixaram +13 handles por recriação no processo.
    /// 2. **Reativar é mais barato que reenumerar.** `MFTEnumEx` percorre o registro de MFTs e
    ///    cria um `IMFActivate` por candidato; recriar o encoder no meio de uma sessão não precisa
    ///    descobrir de novo qual encoder é o certo — ele já foi escolhido.
    pub activate: IMFActivate,
    pub transform: IMFTransform,
    pub events: IMFMediaEventGenerator,
    /// `None` se este MFT não expõe `ICodecAPI` — alguns não expõem, e todo uso deste campo já
    /// trata a ausência sem derrubar o pipeline (ver `apply_low_latency_params`,
    /// `force_next_keyframe`).
    pub codec_api: Option<ICodecAPI>,
    /// Nome amigável do MFT escolhido (ex.: "NVIDIA H264 Hardware MFT em D3D11 Mode") — é a
    /// resposta ao número 3 do contrato ("qual encoder foi realmente usado").
    pub friendly_name: String,
    pub is_hardware: bool,
}

/// Qual MFT escolher entre os enumerados.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preferencia {
    /// A ordem do produto: NVIDIA primeiro, qualquer hardware depois, software por último.
    Produto,
    /// **Só bancada.** Descarta os candidatos que não são da Intel.
    ///
    /// Existe por um motivo medido, e ele é de procedência: no Dell, o `ActivateObject` do MFT da
    /// NVIDIA **falha na sessão interativa** (`E_UNEXPECTED`) e **funciona na Sessão 0**, que é
    /// onde o SSH cai. Um instrumento de bancada que rodasse pelo SSH sem esta preferência mediria
    /// a NVENC enquanto o produto roda no Quick Sync — dois encoders diferentes, com custos de
    /// montagem que diferem por quatro vezes. Medido em 2026-08-30 por `quall-gemeos`, que subiu
    /// na NVENC sem pedir. Ver `docs/troca-a-quente.md`.
    Intel,
}

/// Enumera MFTs de encode H.264, tentando hardware primeiro (com preferência por nome contendo
/// "NVIDIA"/"NVENC", depois qualquer hardware, ou seja Quick Sync no Dell), e cai pro encoder de
/// software da Microsoft só se nenhum de hardware ativar — o que classificaria como falha do
/// plano B, não como caminho aceito, e é relatado como tal por quem chama.
pub fn find_and_activate_h264_encoder() -> Result<ChosenEncoder> {
    ativar_h264(Preferencia::Produto)
}

/// A mesma enumeração, com a escolha parametrizada. Ver [`Preferencia`].
pub fn ativar_h264(preferencia: Preferencia) -> Result<ChosenEncoder> {
    let output_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };

    let mut candidates: Vec<(String, IMFActivate)> = Vec::new();
    for hardware_only in [true, false] {
        let flags = if hardware_only {
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER
        } else {
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
        };

        let mut array_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                None,
                Some(&output_type),
                &mut array_ptr,
                &mut count,
            )?;
        }

        if !array_ptr.is_null() {
            let slice = unsafe { std::slice::from_raw_parts(array_ptr, count as usize) };
            for activate in slice.iter().flatten() {
                let name = friendly_name(activate).unwrap_or_else(|| "(sem nome)".to_string());
                candidates.push((name, activate.clone()));
            }
            unsafe { CoTaskMemFree(Some(array_ptr as *const _)) };
        }

        if hardware_only && !candidates.is_empty() {
            break;
        }
    }

    if preferencia == Preferencia::Intel {
        candidates.retain(|(n, _)| e_da_intel(n));
    }

    if candidates.is_empty() {
        return Err(windows::core::Error::new(E_NOTIMPL, "nenhum MFT de encode H.264 encontrado"));
    }

    eprintln!("MFTs de encode H.264 candidatos:");
    for (name, _) in &candidates {
        eprintln!("  - {name}");
    }

    // Preferência: nome contendo NVIDIA/NVENC > qualquer outro hardware > primeiro da lista
    // (que, com MFT_ENUM_FLAG_HARDWARE primeiro na busca, já é hardware se algum existir).
    candidates.sort_by_key(|(n, _)| {
        let upper = n.to_uppercase();
        if upper.contains("NVIDIA") || upper.contains("NVENC") { 0 } else { 1 }
    });

    // Ativar não é garantido só porque o MFT foi enumerado: no Dell, `ActivateObject` no MFT da
    // NVIDIA devolve E_UNEXPECTED (0x8000FFFF, "falha catastrófica") de forma reproduzível nesta
    // configuração (processo lançado via Tarefa Agendada na sessão interativa, apartamento COM
    // multithreaded) — ver README.md para a investigação. Por isso o laço tenta cada candidato
    // em ordem de preferência em vez de propagar o erro do primeiro que falhar: é exatamente o
    // "cair pro plano B" que o contrato pede, só que decidido em tempo de execução, não só por
    // enumeração.
    let mut last_err = None;
    for (friendly, activate) in candidates {
        eprintln!("ativando \"{friendly}\"...");
        match ativar(&activate, friendly.clone()) {
            Ok(enc) => return Ok(enc),
            Err(e) => {
                eprintln!("  falhou ativar \"{friendly}\": {e}");
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| windows::core::Error::new(E_NOTIMPL, "nenhum MFT ativou")))
}

/// **O MFT de encode H.264 de hardware de uma placa, pelo LUID** — a entrada do monitor virtual
/// (`docs/monitor-virtual-windows.md` §14): a placa foi fixada uma vez no processo (a Intel, a do
/// `SET_RENDER_ADAPTER`), e a sessão usa o MFT e o dispositivo **daquela** placa, não "o primeiro da
/// Intel". `MFTEnum2` com `MFT_ENUM_ADAPTER_LUID` só devolve os MFTs de hardware ligados a ela.
///
/// Sem candidato nenhum por esse caminho, e **só quando a placa é Intel**, cai para [`ativar_h264`]
/// com [`Preferencia::Intel`] (só MFT com "Intel" no nome) — é a mesma placa, salvo num computador
/// com **duas** placas Intel (uma Arc e a integrada), onde pode sair a outra; não medido. Placa de
/// outro fabricante (a tela estendida sem Intel, 02/10) não tem recuo: o MFT da Intel com o
/// dispositivo de outra placa só falharia no `configure`. No Dell o caminho usado é o do LUID (o
/// registro diz qual). O caminho de uma sessão só não passa por aqui.
pub fn ativar_h264_na_placa(luid: u64) -> Result<(ChosenEncoder, &'static str)> {
    ativar_h264_da_placa(luid, false)
}

/// [`ativar_h264_na_placa`], com o recuo "só Intel" desligado quando `estrita`: **a câmera**, que
/// abriu o dispositivo no LUID do encoder que ativou e não pode receber o MFT de outra placa na
/// reserva nem na recriação — o `configure` recusaria, e o registro diria "só Intel" (a revisão do
/// código da fase 3, m7). A câmera também não tem recuo para o MFT de software: ela só abre com
/// um encoder de hardware ligado a uma placa (`docs/camera-no-windows.md` §4.3).
pub fn ativar_h264_da_placa(luid: u64, estrita: bool) -> Result<(ChosenEncoder, &'static str)> {
    if estrita {
        return ativar_h264_so_na_placa(luid).map(|e| (e, "MFTEnum2 pelo LUID da placa, estrito (câmera)"));
    }
    let ultimo = match ativar_h264_so_na_placa(luid) {
        Ok(e) => return Ok((e, "MFTEnum2 pelo LUID da placa")),
        Err(e) => e,
    };
    // O recuo "só Intel" é só para placa Intel: a placa do monitor virtual pode ser NVIDIA ou AMD
    // desde 02/10, e dar a ela o MFT da Intel abriria um encoder de outra placa (a revisão de 02/10,
    // item 2).
    let e_intel = crate::device::placas_de_hardware()
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.luid == luid))
        .is_some_and(|p| p.vendor_id == crate::device::VENDOR_INTEL);
    if !e_intel {
        return Err(ultimo);
    }
    match ativar_h264(Preferencia::Intel) {
        Ok(e) => Ok((e, "sem candidato pelo LUID: a enumeração de sempre, só Intel")),
        Err(e) => Err(if ultimo.code() == E_NOTIMPL { e } else { ultimo }),
    }
}

/// **Só os MFTs daquela placa, sem recuo nenhum** — a entrada da câmera, que tenta as placas uma a
/// uma e cria o dispositivo no LUID da que ativou (`docs/camera-no-windows.md` §4.3). O recuo "só
/// Intel" de [`ativar_h264_na_placa`] aqui seria errado: na sessão interativa o MFT da NVIDIA falha
/// (`E_UNEXPECTED`), o recuo devolveria o da Intel, e o dispositivo nasceria no LUID da NVIDIA — o
/// `configure` recusaria com `E_INVALIDARG`. `E_NOTIMPL` quando a placa não tem candidato.
pub fn ativar_h264_so_na_placa(luid: u64) -> Result<ChosenEncoder> {
    let mut ultimo = None;
    for (nome, a) in candidatos_h264_na_placa(luid)? {
        match ativar(&a, nome.clone()) {
            Ok(e) => return Ok(e),
            Err(e) => {
                eprintln!("  falhou ativar \"{nome}\" (pelo LUID): {e}");
                ultimo = Some(e);
            }
        }
    }
    Err(ultimo.unwrap_or_else(|| windows::core::Error::new(E_NOTIMPL, "nenhum MFT de H.264 pelo LUID da placa")))
}

/// A placa **anuncia** um MFT de encode H.264 de hardware próprio? Só a enumeração pelo LUID, sem
/// ativar nada: a resposta vem do registro dos MFTs e é a mesma de uma abertura para a outra — é o
/// que a escolha da placa do monitor virtual usa (`regras_da_tela_estendida::placa_do_monitor`).
pub fn anuncia_h264_na_placa(luid: u64) -> bool {
    candidatos_h264_na_placa(luid).is_ok_and(|v| !v.is_empty())
}

/// Os MFTs de encode H.264 de hardware ligados à placa deste LUID (`MFTEnum2` com
/// `MFT_ENUM_ADAPTER_LUID`), na ordem do `MFT_ENUM_FLAG_SORTANDFILTER`, sem ativar.
fn candidatos_h264_na_placa(luid: u64) -> Result<Vec<(String, IMFActivate)>> {
    let output_type = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut candidatos: Vec<(String, IMFActivate)> = Vec::new();
    unsafe {
        let mut atributos: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut atributos, 1)?;
        let atributos = atributos.ok_or_else(|| windows::core::Error::new(E_NOTIMPL, "MFCreateAttributes sem atributos"))?;
        let l = windows::Win32::Foundation::LUID { LowPart: luid as u32, HighPart: (luid >> 32) as u32 as i32 };
        let bytes = std::slice::from_raw_parts(&l as *const windows::Win32::Foundation::LUID as *const u8, std::mem::size_of_val(&l));
        atributos.SetBlob(&MFT_ENUM_ADAPTER_LUID, bytes)?;
        let mut ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n: u32 = 0;
        let r = MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output_type),
            &atributos,
            &mut ptr,
            &mut n,
        );
        if r.is_ok() && !ptr.is_null() {
            for a in std::slice::from_raw_parts(ptr, n as usize).iter().flatten() {
                candidatos.push((friendly_name(a).unwrap_or_else(|| "(sem nome)".to_string()), a.clone()));
            }
            CoTaskMemFree(Some(ptr as *const _));
        }
    }
    Ok(candidatos)
}

/// Cria o transform a partir de um `IMFActivate` já escolhido.
///
/// Separado de `find_and_activate_h264_encoder` para que a quinta porta possa **reativar o mesmo
/// activate** sem repetir a enumeração — que é a parte cara e a parte que não muda no meio de uma
/// sessão. Ver `reativar`.
fn ativar(activate: &IMFActivate, friendly: String) -> Result<ChosenEncoder> {
    let transform = unsafe { activate.ActivateObject::<IMFTransform>()? };
    let events: IMFMediaEventGenerator = transform.cast()?;
    let is_hardware = unsafe {
        transform
            .GetAttributes()
            .and_then(|attrs| attrs.GetUINT32(&MF_TRANSFORM_ASYNC))
            .unwrap_or(0)
            == 1
    };
    let codec_api: Option<ICodecAPI> = transform.cast().ok();
    eprintln!(
        "  ativado com sucesso (hardware={is_hardware}, ICodecAPI={})",
        codec_api.is_some()
    );
    Ok(ChosenEncoder {
        activate: activate.clone(),
        transform,
        events,
        codec_api,
        friendly_name: friendly,
        is_hardware,
    })
}

/// **Ativa de novo o mesmo MFT que já foi escolhido** — o caminho da quinta porta.
///
/// Não reenumera: o encoder certo já foi decidido na abertura da sessão, e reenumerar no meio de
/// uma sessão custaria tempo para responder uma pergunta que não mudou. Também é o que permite
/// `desligar` chamar `ShutdownObject` no mesmo `IMFActivate` — a contrapartida de
/// `ActivateObject`, sem a qual o processo vaza handles a cada recriação.
pub fn reativar(anterior: &ChosenEncoder) -> Result<ChosenEncoder> {
    ativar(&anterior.activate, anterior.friendly_name.clone())
}

/// O nome amigável é de um MFT da Intel?
///
/// Função à parte, e testada, porque o nome real tem um `®` no meio
/// (`"Intel® Quick Sync Video H.264 Encoder MFT"`) e porque **um filtro errado aqui é mudo**: ele
/// não falha, ele só mede o encoder errado — que foi exatamente o que a primeira corrida de
/// `quall-gemeos` fez ao subir na NVENC e reportar uma recriação de 39 ms contra os 150 do Quick
/// Sync.
pub fn e_da_intel(nome: &str) -> bool {
    let u = nome.to_uppercase();
    u.contains("INTEL") || u.contains("QUICK SYNC")
}

fn friendly_name(activate: &IMFActivate) -> Option<String> {
    unsafe {
        let mut pwstr = PWSTR::null();
        let mut len = 0u32;
        activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut pwstr, &mut len).ok()?;
        let s = pwstr.to_string().ok();
        if !pwstr.is_null() {
            CoTaskMemFree(Some(pwstr.0 as *const _));
        }
        s
    }
}

/// O formato de pixel que entra no encoder.
///
/// **Dois caminhos de configuração do mesmo MFT** (`docs/camera-no-windows.md` §4): a tela entrega
/// BGRA (`Argb32`), a câmera entrega NV12. A ordem de entradas que o Quick Sync do Dell oferece é
/// NV12 privado, NV12, ARGB32 (medido em 18/09); o NVENC com NV12 **não foi medido**.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatoDeEntrada {
    /// `MFVideoFormat_ARGB32`: a textura do WGC e da origem sintética. O de sempre.
    Argb32,
    /// `MFVideoFormat_NV12`: a câmera.
    Nv12,
}

impl FormatoDeEntrada {
    fn subtipo(self) -> GUID {
        match self {
            FormatoDeEntrada::Argb32 => MFVideoFormat_ARGB32,
            FormatoDeEntrada::Nv12 => MFVideoFormat_NV12,
        }
    }
}

/// `Copy` porque a quinta porta precisa **remontar** um encoder com exatamente a mesma
/// configuração no meio da sessão: guardar a config na `Cadeia` e reaplicá-la é o que garante que
/// o encoder novo não seja um encoder *diferente* — inclusive no formato de entrada, que a oficina
/// e a recriação levam junto.
#[derive(Clone, Copy)]
pub struct EncoderConfig {
    /// O formato do que entra. A tela é `Argb32`; a câmera será `Nv12`.
    pub entrada: FormatoDeEntrada,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub gop_frames: u32,
    /// Período do **refresh intra gradual**, em quadros. `0` desliga (o padrão).
    ///
    /// Ver `docs/idr-pequeno.md`: `docs/idr-que-sobrevive.md` mediu que uma unidade de acesso
    /// acima de ~35 pacotes tem a cauda truncada pelo enlace de 2,4 GHz, e o refresh intra é o
    /// único conserto que faz o objeto grande deixar de existir em vez de tentar fazê-lo
    /// sobreviver.
    pub intra_refresh_frames: u32,
    /// Tamanho máximo de fatia. `0` desliga (o padrão). A unidade depende de
    /// `AVEncSliceControlMode`, e aqui o modo pedido é **1 = bits**.
    ///
    /// Fatiar **não** encolhe a rajada; ele reparte o dano. E hoje não paga sozinho: o
    /// depacotizador descarta a unidade de acesso inteira ao primeiro buraco
    /// (`crates/quall-core/src/rtp.rs`), e o `VTDecompressionSession` da Apple **recusa**
    /// unidade de acesso com fatia faltando — medido em `sonda-fatias`.
    pub slice_bytes: u32,
    /// **O teto de quadro da tela estendida**, em bits: o tamanho do buffer do controle de taxa
    /// (`CODECAPI_AVEncCommonBufferSize`, o VBV). `0` não pede nada — o padrão de todos os caminhos
    /// de antes desta frente.
    ///
    /// É o par Windows do `DataRateLimits` do Mac (`H264Encoder.swift`), que lá pede que nenhum
    /// trecho de 1/fps passe de 5 quadros médios: um IDR de 300–400 KB levava ~100 ms para
    /// atravessar e virava tranco (`docs/handover-tela-estendida.md` §3). Um buffer de N bits num
    /// controle de taxa por buffer limita o maior quadro a ~N bits. **Pedido não é obedecido**: no
    /// Mac o mesmo teto foi ignorado na mesa e obedecido na tela. Quem confere é o tamanho do IDR
    /// no fio (`Cadeia::medida_de_quadro_chave`).
    pub teto_de_quadro_bits: u32,
}

/// Configura o MFT: desbloqueia o modo assíncrono, registra o `IMFDXGIDeviceManager`, define os
/// tipos de mídia de saída (H.264 baseline) e de entrada — o que `cfg.entrada` pedir: BGRA
/// (`Argb32`) para a tela, NV12 para a câmera, os dois sem conversão de cor nossa, porque o MFT de
/// hardware converte o BGRA na GPU e come o NV12 direto — e aplica os parâmetros de baixa latência
/// via `ICodecAPI`. O `MF_MT_MAX_KEYFRAME_SPACING` fica de fora (`tentar_espacamento_de_idr`, que
/// quem monta chama depois).
pub fn configure(
    enc: &ChosenEncoder,
    device_manager: &IMFDXGIDeviceManager,
    cfg: &EncoderConfig,
) -> Result<()> {
    unsafe {
        // Desbloquear o modo assíncrono é o *primeiro* contato com o MFT, antes de qualquer
        // `ProcessMessage` ou `SetInputType`/`SetOutputType` — nessa ordem errada (tentada antes)
        // o MFT devolve `MF_E_TRANSFORM_ASYNC_LOCKED` (0xC00D6D77) em vez de aceitar a mensagem.
        if let Ok(attrs) = enc.transform.GetAttributes() {
            let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
        }

        // MFT_MESSAGE_SET_D3D_MANAGER: é o que permite o MFT aceitar amostras cuja
        // `IMFMediaBuffer` referencia uma textura D3D11 em vez de memória de sistema — o caminho
        // sem cópia que sustenta a meta de latência.
        let mgr_unknown: windows::core::IUnknown = device_manager.cast()?;
        let mgr_ptr = windows::core::Interface::as_raw(&mgr_unknown) as usize;
        enc.transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr_ptr)?;
        eprintln!("  D3D manager registrado no MFT");

        // --- tipo de saída: H.264 baseline, denominador comum entre todas as plataformas do
        // Quall (macOS/iOS decodificam via VideoToolbox, Android via MediaCodec — baseline é o
        // perfil que os três aceitam sem negociação extra). ---
        let out_type = MFCreateMediaType()?;
        out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out_type.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate_bps)?;
        set_attribute_size(&out_type, &MF_MT_FRAME_SIZE, cfg.width, cfg.height)?;
        set_attribute_ratio(&out_type, &MF_MT_FRAME_RATE, cfg.fps, 1)?;
        out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
        out_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 0)?;
        eprintln!("  tipo de saída montado, chamando SetOutputType...");
        enc.transform.SetOutputType(0, &out_type, 0)?;
        eprintln!("  SetOutputType ok");

        // --- tipo de entrada: o formato que a origem entrega, sem estágio de conversão nosso. A tela
        // entrega BGRA (ARGB32); a câmera, NV12. O MFT de hardware converte o BGRA na GPU como parte
        // do encode; o NV12 ele come direto. ---
        let pedido = cfg.entrada.subtipo();
        let mut chosen_input = false;
        let mut i = 0u32;
        loop {
            let candidate = match enc.transform.GetInputAvailableType(0, i) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("  GetInputAvailableType parou em i={i}: {e}");
                    break;
                }
            };
            let subtype = candidate.GetGUID(&MF_MT_SUBTYPE).unwrap_or(GUID::zeroed());
            eprintln!("  candidato de entrada {i}: subtype={subtype:?}");
            if subtype == pedido {
                set_attribute_size(&candidate, &MF_MT_FRAME_SIZE, cfg.width, cfg.height)?;
                set_attribute_ratio(&candidate, &MF_MT_FRAME_RATE, cfg.fps, 1)?;
                match enc.transform.SetInputType(0, &candidate, 0) {
                    Ok(()) => {
                        chosen_input = true;
                        eprintln!("tipo de entrada: {:?} (sem conversão de cor em CPU)", cfg.entrada);
                        break;
                    }
                    Err(e) => eprintln!("  SetInputType recusou {:?}: {e}", cfg.entrada),
                }
            }
            i += 1;
        }
        if !chosen_input {
            return Err(windows::core::Error::new(
                E_NOTIMPL,
                format!("o MFT não aceitou a entrada {:?} direto", cfg.entrada),
            ));
        }

        apply_low_latency_params(enc, cfg)?;
    }
    Ok(())
}

/// `MFSetAttributeSize`/`MFSetAttributeRatio` do SDK C++ são funções **inline** (não fazem parte
/// da ABI de nenhuma DLL), então o `windows` crate — que só liga contra símbolos reais — não as
/// expõe. A implementação é trivial: empacotam dois `u32` num único `u64` (metade alta / metade
/// baixa) e chamam `SetUINT64`, exatamente como o cabeçalho `mfapi.h` faz.
fn set_attribute_size(attrs: &IMFMediaType, key: &GUID, width: u32, height: u32) -> Result<()> {
    let packed = ((width as u64) << 32) | (height as u64);
    unsafe { attrs.SetUINT64(key, packed) }
}

fn set_attribute_ratio(attrs: &IMFMediaType, key: &GUID, numerator: u32, denominator: u32) -> Result<()> {
    let packed = ((numerator as u64) << 32) | (denominator as u64);
    unsafe { attrs.SetUINT64(key, packed) }
}

/// `ICodecAPI` é a interface COM legada (Windows Media / DirectShow) reaproveitada pelos MFTs de
/// vídeo pra parâmetros que não cabem em `IMFMediaType`. Cada `SetValue` é tentado
/// independentemente e o resultado só é logado: nem todo encoder de hardware implementa toda
/// propriedade, e falhar uma não deveria derrubar o pipeline inteiro.
unsafe fn apply_low_latency_params(enc: &ChosenEncoder, cfg: &EncoderConfig) -> Result<()> {
    let Some(codec_api) = &enc.codec_api else {
        eprintln!("aviso: MFT não expõe ICodecAPI; seguindo com os parâmetros padrão do encoder");
        return Ok(());
    };

    let try_set = |name: &str, api: GUID, value: VARIANT| {
        let ok = codec_api.SetValue(&api, &value).is_ok();
        eprintln!("  ICodecAPI {name}: {}", if ok { "aplicado" } else { "recusado pelo encoder" });
    };

    // Zero filas internas: o encoder não deve acumular quadros de lookahead antes de devolver
    // saída. É a propriedade mais direta que existe pra "zero filas" do lado do encoder.
    try_set("AVLowLatencyMode", CODECAPI_AVLowLatencyMode, variant_bool(true));

    // GOP curto: um IDR a cada `gop_frames` quadros. Tentativa por configuração global — medido
    // nesta bancada que o MFT de Quick Sync **recusa** esta propriedade (`SetValue` falha, sem
    // derrubar o pipeline). Por isso `main.rs` não confia nisso: força IDR por demanda via
    // `force_next_keyframe`, chamado a cada `gop_frames` quadros pelo próprio chamador — é o GOP
    // curto de fato, decidido do lado de fora do MFT já que o MFT não aceita controlar por
    // dentro. Mantida a tentativa aqui mesmo assim: é grátis, e se um MFT diferente (outra
    // máquina, outro driver) aceitar, ótimo.
    try_set("AVEncMPVGOPSize", CODECAPI_AVEncMPVGOPSize, variant_i32(cfg.gop_frames as i32));

    // CBR de baixa latência: taxa de bits previsível é mais importante que qualidade máxima
    // quando o objetivo é orçamento de latência, não de banda.
    try_set(
        "AVEncCommonRateControlMode",
        CODECAPI_AVEncCommonRateControlMode,
        variant_i32(eAVEncCommonRateControlMode_LowDelayVBR.0),
    );

    // Perfil baseline não usa CABAC (usa CAVLC); desligar explicitamente em vez de confiar que o
    // driver deriva isso sozinho do MF_MT_MPEG2_PROFILE.
    try_set("AVEncH264CABACEnable", CODECAPI_AVEncH264CABACEnable, variant_bool(false));

    // Sem B-frames: são a fonte clássica de latência de reordenação (o encoder segura quadros
    // pra decidir a ordem de exibição). Zero aqui é consistente com "zero filas".
    try_set("AVEncMPVDefaultBPictureCount", CODECAPI_AVEncMPVDefaultBPictureCount, variant_i32(0));

    // ------------------------------------------------------------------------------------------
    // Os dois botões de `docs/idr-pequeno.md`. Os dois nascem DESLIGADOS.
    //
    // **E "aplicado" na linha acima não quer dizer que funcionou.** Este arquivo já carrega a
    // prova: `AVEncVideoForceKeyFrame` é aceito e ignorado por este MFT, e foram cinco portas até
    // achar uma que funcionasse (`docs/quinta-porta.md`). Quem confirma é `tools/fatias.py` sobre
    // o `.h264`, contando fatias por unidade de acesso e a distribuição de tamanho de quadro.
    // ------------------------------------------------------------------------------------------
    if cfg.intra_refresh_frames > 0 {
        // **O nome que o briefing desta frente deu não existe.** `CODECAPI_AVEncVideoIntraRefresh
        // Mode` e `...Period` não aparecem em lugar nenhum do SDK 10.0.26100 do Dell — a busca
        // por `AVEncVideoIntraRefresh` na árvore inteira de cabeçalhos devolve **uma** linha, e é
        // esta. O que existe é `CODECAPI_AVEncVideoGradualIntraRefresh`, um `UINT32` só, sem
        // propriedade de modo ao lado. Achado em 31/08/2026 quando o portão reprovou com
        // `E0425: cannot find value` — que é o jeito barato de descobrir isso.
        try_set(
            "AVEncVideoGradualIntraRefresh",
            CODECAPI_AVEncVideoGradualIntraRefresh,
            variant_i32(cfg.intra_refresh_frames as i32),
        );
    }
    if cfg.slice_bytes > 0 {
        // Modo 1 = controle por bits (`AVEncSliceControlSize` passa a ser lido em bits).
        try_set("AVEncSliceControlMode", CODECAPI_AVEncSliceControlMode, variant_i32(1));
        try_set(
            "AVEncSliceControlSize",
            CODECAPI_AVEncSliceControlSize,
            variant_i32((cfg.slice_bytes * 8) as i32),
        );
    }
    if cfg.teto_de_quadro_bits > 0 {
        // `UINT32` (`VT_UI4`) é o tipo que a documentação da propriedade dá; `VT_I4` foi recusado
        // por este driver em outra propriedade (`force_next_keyframe`, com `VT_UI4` ao contrário).
        try_set(
            "AVEncCommonBufferSize",
            CODECAPI_AVEncCommonBufferSize,
            variant_u32(cfg.teto_de_quadro_bits),
        );
    }

    Ok(())
}

/// O que o MFT diz ter para o buffer do controle de taxa, relido por `GetValue` depois de
/// configurado — e ao lado, se ele declara a propriedade (`IsSupported`).
///
/// **Isto é o retorno da API, não o fluxo.** Existe para o registro dizer, numa linha, a distância
/// entre o que se pediu e o que o driver guardou; quem responde se o teto pegou é o tamanho do IDR
/// no fio.
pub fn teto_de_quadro_relido(enc: &ChosenEncoder) -> String {
    let Some(codec_api) = &enc.codec_api else {
        return "sem ICodecAPI".into();
    };
    unsafe {
        let suportada = codec_api.IsSupported(&CODECAPI_AVEncCommonBufferSize).is_ok();
        let lido = codec_api
            .GetValue(&CODECAPI_AVEncCommonBufferSize)
            .ok()
            .map(|v| {
                let inner = &v.Anonymous.Anonymous;
                match inner.vt {
                    VT_UI4 => format!("{} bits (VT_UI4)", inner.Anonymous.ulVal),
                    VT_I4 => format!("{} bits (VT_I4)", inner.Anonymous.lVal),
                    outro => format!("tipo {}", outro.0),
                }
            })
            .unwrap_or_else(|| "GetValue recusado".into());
        format!("IsSupported={suportada} GetValue={lido}")
    }
}

/// Pede ao encoder pra marcar o **próximo** quadro submetido (o próximo `ProcessInput`, não o
/// atual) como IDR — `CODECAPI_AVEncVideoForceKeyFrame` (`VT_BOOL`; testei `VT_UI4` também, e
/// esse tipo é *recusado* de cara, o que confirma que `VT_BOOL` é o tipo certo que este driver
/// espera).
///
/// **`SetValue` devolve sucesso, mas medido contra o `.h264` de verdade (não só o log), isso não
/// força IDR nenhum neste MFT.** Duas rodadas de teste, cada uma pedindo força em ~12 quadros
/// espaçados por intervalos bem diferentes (60 e depois 17 quadros, de propósito distintos e não
/// múltiplos entre si) — nos dois casos os quadros IDR que *de fato* saem no `.h264` ficam em
/// `[0, 128]` ou `[0, 128, 256]`: um intervalo fixo de ~128 quadros, que bate com nenhum dos
/// valores pedidos. Ou seja, o driver aceita a chamada (não é `E_NOTIMPL` nem recusa de tipo) mas
/// não muda o comportamento — o GOP real é interno e fixo, alheio tanto a `AVEncMPVGOPSize`
/// (recusado explicitamente) quanto a este "force" (aceito e ignorado). Ver `README.md`, seção
/// de achados, pra a tabela com os dois experimentos e os números crus.
///
/// **CORREÇÃO DE 2026-08-28, e ela inverte a conclusão abaixo.** O que segue continua verdadeiro
/// sobre *este* controle e sobre as outras três portas de **configuração** — mas "não existe
/// caminho" deixou de valer. A **quinta porta** existe, foi medida e funciona: não pedir nada ao
/// driver e sim **destruir o transform e criar outro** (ver `desligar` e
/// `transmissao::Cadeia::recriar_encoder`). 20 recriações, 20 IDR no fluxo, 187–221 ms do pedido
/// até o quadro-chave sair, conjunto de parâmetros byte a byte idêntico nas 20. Leia esta função
/// como o registro de uma porta fechada, não como o estado do problema.
///
/// **Conclusão prática, não afirmação otimista**: com este MFT, nesta máquina, não existe
/// caminho encontrado por mim pra controlar quando um IDR sai — nem periódico nem sob demanda. Um
/// receptor que entra no meio da sessão ou perde um pacote fica sem imagem até o próximo IDR
/// "espontâneo" do driver (~128 quadros, ~7s a 18 fps medido) — não há como o protocolo pedir um
/// antes disso e confiar que vai vir. Isso é fato medido que deveria pesar no desenho do
/// protocolo: se recuperação rápida for exigida, a saída não está neste controle de encoder — tem
/// que vir de outro lugar (ex.: reenviar o SPS/PPS + o quadro IDR mais recente do buffer pro
/// receptor atrasado, em vez de pedir um novo; ou aceitar o intervalo fixo do driver como o piso
/// de recuperação real e desenhar em torno disso). Não tentei essas alternativas — são decisão de
/// protocolo, fora do escopo de captura desta frente.
///
/// Mantida a função (best-effort, não derruba o pipeline se `false`) porque é barata, documenta a
/// tentativa, e pode se comportar diferente em outro MFT/driver/máquina.
pub fn force_next_keyframe(enc: &ChosenEncoder) -> bool {
    let Some(codec_api) = &enc.codec_api else { return false };
    unsafe { codec_api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_bool(true)) }.is_ok()
}

/// Uma **segunda** tentativa de controlar o espaçamento de IDR, por um caminho diferente do que o
/// achado 7 mediu.
///
/// O achado 7 fechou duas portas: `ICodecAPI::SetValue(CODECAPI_AVEncMPVGOPSize)` é *recusado* por
/// este MFT, e `CODECAPI_AVEncVideoForceKeyFrame` é *aceito e ignorado* (o `.h264` mostra IDR só
/// em 0/128/256, alheio a 24 pedidos espaçados de propósito). O que ele **não** testou é a
/// terceira porta: `MF_MT_MAX_KEYFRAME_SPACING` não é `ICodecAPI`, é um atributo do
/// `IMFMediaType` de saída, e alguns MFTs só honram esse.
///
/// Vale para um app de produto e não valia para uma sonda de captura: com GOP de ~128 quadros, um
/// receptor que entra no meio da sessão fica **~7 s sem imagem** (medido: 3.714,7 ms para entrar a
/// partir do quadro 30, com 98 quadros até o IDR seguinte). Quem espera são olhos humanos.
///
/// Devolve se a chamada foi aceita. **Aceita não é obedecida** — a regra da casa é conferir no
/// fluxo, e quem confere é `transmissao.rs`, medindo o espaçamento real dos IDR que saem.
/// Aditiva: nenhum outro binário deste pacote a chama, então nenhuma medição existente muda.
pub fn tentar_espacamento_de_idr(enc: &ChosenEncoder, gop_frames: u32) -> bool {
    unsafe {
        let Ok(tipo) = enc.transform.GetOutputCurrentType(0) else {
            return false;
        };
        if tipo.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, gop_frames).is_err() {
            return false;
        }
        enc.transform.SetOutputType(0, &tipo, 0).is_ok()
    }
}

/// **A terceira porta: reiniciar o fluxo do MFT para forçar um IDR.**
///
/// O achado 7 fechou duas: `AVEncMPVGOPSize` é recusado, e `AVEncVideoForceKeyFrame` é aceito e
/// ignorado — `S_OK` com `idrs em [0, 128, 256]`, alheio a pedidos espaçados de propósito. A
/// terceira é a que o `README.md` nomeou e ninguém tentou: **`MFT_MESSAGE_COMMAND_FLUSH` seguido
/// de `NOTIFY_START_OF_STREAM`**. Um encoder que reinicia o fluxo tem de recomeçar a cadeia de
/// referência, e recomeçar cadeia de referência é, por definição, emitir um IDR.
///
/// # O perigo, dito antes de o código aparecer
///
/// Este MFT é **assíncrono**, e `transmissao.rs` guarda créditos de `METransformNeedInput`. O
/// `FLUSH` invalida esses créditos: o MFT descarta o que estava dentro dele e volta a emitir
/// `NeedInput` do zero. Um chamador que continuasse gastando os créditos velhos submeteria
/// quadros que o MFT não pediu — e um que zerasse os créditos sem receber os novos **trava o
/// emissor para sempre**. Errar aqui não degrada: para.
///
/// Por isso esta função **não** mexe em contador nenhum. Ela faz as duas chamadas e devolve; quem
/// zera crédito, larga o quadro pendente e limpa a fila de submetidos é `Cadeia::pedir_idr`, que
/// é quem tem esse estado. Separar assim é o que torna a parte perigosa revisável num lugar só.
pub fn reiniciar_fluxo(enc: &ChosenEncoder) -> Result<()> {
    unsafe {
        enc.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
        enc.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }
    Ok(())
}

/// **A quinta porta: desmontar o transform para que outro possa nascer.**
///
/// As quatro portas anteriores foram todas de *configuração* — pedir ao MFT que mude de
/// comportamento. Esta não pede nada: **destrói o encoder**. Um encoder recém-criado é obrigado a
/// começar por IDR (não há cadeia de referência anterior para apontar), e é a única coisa que a
/// norma garante sem depender de o driver querer.
///
/// Esta função é a **metade de baixo** da porta: ela só derruba. Quem recria é
/// `transmissao::Cadeia::recriar_encoder`, que tem o dispositivo, o gerenciador D3D e a
/// configuração — e que é também quem tem o estado de créditos que precisa ser zerado junto.
///
/// # A ordem importa, e cada passo paga uma coisa
///
/// 1. `COMMAND_FLUSH` — solta as amostras que o MFT ainda segura. Sem isto o transform morre
///    ainda apontando para texturas do pool da captura.
/// 2. `NOTIFY_END_OF_STREAM` + `NOTIFY_END_STREAMING` — o fechamento simétrico do que
///    `start_stream` abriu.
/// 3. `SET_D3D_MANAGER` com **zero** — é a forma documentada de o MFT largar a referência ao
///    `IMFDXGIDeviceManager`. Sem isto cada recriação deixaria uma referência viva no gerenciador
///    que a `Cadeia` reusa, e o vazamento só apareceria depois de dezenas de recriações.
/// 4. `IMFShutdown::Shutdown` — **este é o passo que solta a thread do bombeador de eventos**. Ela
///    está parada dentro de `GetEvent`, que é bloqueante; sem o `Shutdown` a fila de eventos nunca
///    devolve, a thread nunca sai, e cada recriação vazaria uma thread mais uma referência COM ao
///    transform velho (que então nunca seria liberado). Se o MFT não expuser `IMFShutdown`, isto é
///    exatamente o que acontece — por isso o retorno diz se o desligamento foi limpo, e por isso a
///    bancada mede contagem de threads e de handles ao longo de repetições.
///
/// Devolve `true` se o `IMFShutdown` existia e aceitou — ou seja, se a thread de eventos vai
/// mesmo morrer. Nunca propaga erro: quem chama já está no caminho de derrubar.
pub fn desligar(enc: &ChosenEncoder) -> bool {
    unsafe {
        let _ = enc.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        let _ = enc.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
        let _ = enc.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        let _ = enc.transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
        let limpo = match enc.transform.cast::<IMFShutdown>() {
            Ok(desligavel) => desligavel.Shutdown().is_ok(),
            Err(_) => false,
        };
        // 5. `ShutdownObject` é a contrapartida de `ActivateObject`, e não é opcional: medido
        //    nesta bancada, 20 recriações **sem** esta chamada deixaram +13 handles por recriação
        //    no processo, em linha reta. É o passo que faz o activate largar o objeto que ele
        //    criou — o `IMFShutdown` acima desliga o transform, este solta o que o criou.
        let _ = enc.activate.ShutdownObject();
        limpo
    }
}

/// Os conjuntos de parâmetros (SPS + PPS, em Annex-B) que o MFT guarda no tipo de saída.
///
/// # Por que isto é obrigatório e não um detalhe
///
/// `crates/quall-core/src/track.rs` tem um teste chamado, literalmente, *"IDR sem SPS/PPS tem de
/// ser detectado: é o defeito do M1 no Windows"* — e ele conta (`idrs_sem_parametros`) em vez de
/// recusar. O MFT do Windows entrega o conjunto de parâmetros **uma vez**, aqui, em vez de
/// repeti-lo em cada IDR como o VideoToolbox e o MediaCodec fazem. Um receptor que entra no meio
/// da sessão — que é o caso normal deste produto, já que quem espelha hospeda e espera — nunca viu
/// esse primeiro quadro, e sem SPS/PPS não monta imagem nenhuma.
///
/// Devolve `None` enquanto o MFT ainda não tiver o blob: ele costuma só aparecer depois da
/// renegociação de tipo de saída que segue o primeiro `SetInputType` (achado 5), então quem chama
/// deve tentar de novo.
pub fn conjuntos_de_parametros(enc: &ChosenEncoder) -> Option<Vec<u8>> {
    unsafe {
        let tipo = enc.transform.GetOutputCurrentType(0).ok()?;
        let tamanho = tipo.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
        if tamanho == 0 {
            return None;
        }
        let mut buffer = vec![0u8; tamanho as usize];
        tipo.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut buffer, None).ok()?;
        Some(buffer)
    }
}

fn variant_bool(value: bool) -> VARIANT {
    let inner = VARIANT_0_0 {
        vt: VT_BOOL,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: VARIANT_0_0_0 { boolVal: if value { VARIANT_TRUE } else { VARIANT_FALSE } },
    };
    VARIANT { Anonymous: VARIANT_0 { Anonymous: ManuallyDrop::new(inner) } }
}

fn variant_i32(value: i32) -> VARIANT {
    let inner = VARIANT_0_0 {
        vt: VT_I4,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: VARIANT_0_0_0 { lVal: value },
    };
    VARIANT { Anonymous: VARIANT_0 { Anonymous: ManuallyDrop::new(inner) } }
}

fn variant_u32(value: u32) -> VARIANT {
    let inner = VARIANT_0_0 {
        vt: VT_UI4,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: VARIANT_0_0_0 { ulVal: value },
    };
    VARIANT { Anonymous: VARIANT_0 { Anonymous: ManuallyDrop::new(inner) } }
}

/// Evento relevante do laço assíncrono do MFT, já traduzido do tipo bruto de evento MF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MftEvent {
    NeedInput,
    HaveOutput,
    DrainComplete,
    Other(u32),
}

/// Sobe uma thread dedicada que só bombeia `IMFMediaEventGenerator::GetEvent` e traduz pro canal.
/// É a forma recomendada de dirigir um MFT assíncrono sem travar quem está alimentando quadros —
/// `GetEvent` bloqueia até o próximo evento, então precisa da própria thread.
pub fn spawn_event_pump(events: IMFMediaEventGenerator) -> Receiver<MftEvent> {
    // `windows-core` deliberadamente não implementa `Send` pra interfaces COM — a segurança de
    // cruzar thread depende do modelo de apartamento, que a interface por si não sabe. Nós
    // sabemos: o processo inteiro roda em MTA (`CoInitializeEx(COINIT_MULTITHREADED)` em
    // `main.rs`), e em MTA chamar a mesma interface de threads diferentes sem marshaling extra é
    // válido. Por isso o `unsafe impl Send` explícito abaixo, escopado só a este ponteiro.
    struct SendableEvents(IMFMediaEventGenerator);
    unsafe impl Send for SendableEvents {}
    impl SendableEvents {
        // Um método (em vez de acessar o campo `.0` direto) força a closure abaixo a capturar o
        // `SendableEvents` inteiro — captura "precisa" (edition 2021) capturaria só o campo, que
        // não é `Send`, e a gente perderia o efeito do `unsafe impl Send` acima.
        fn into_inner(self) -> IMFMediaEventGenerator {
            self.0
        }
    }
    let events = SendableEvents(events);

    let (tx, rx) = unbounded();
    thread::spawn(move || {
        let events = events.into_inner();
        loop {
        let ev = unsafe { events.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) };
        let Ok(ev) = ev else { break };
        let Ok(event_type) = (unsafe { ev.GetType() }) else { continue };
        let translated = match event_type {
            x if x == METransformNeedInput.0 as u32 => MftEvent::NeedInput,
            x if x == METransformHaveOutput.0 as u32 => MftEvent::HaveOutput,
            x if x == METransformDrainComplete.0 as u32 => MftEvent::DrainComplete,
            other => MftEvent::Other(other),
        };
        if tx.send(translated).is_err() {
            break;
        }
        }
    });
    rx
}

/// Cria o `IMFDXGIDeviceManager` e registra nele o dispositivo D3D11 compartilhado com a
/// captura.
pub fn create_device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    let mut reset_token = 0u32;
    let mut out: Option<IMFDXGIDeviceManager> = None;
    unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut out)? };
    let manager = out.expect("MFCreateDXGIDeviceManager não devolveu gerenciador");
    unsafe { manager.ResetDevice(device, reset_token)? };
    Ok(manager)
}

/// Envolve uma textura D3D11 (um quadro capturado) como `IMFSample` pronta pra `ProcessInput`,
/// carimbada com o timestamp em unidades de 100ns que o Media Foundation usa internamente.
///
/// **`subrecurso` é o índice da fatia**, e não pode ser sempre 0. A textura do WGC e a da origem
/// sintética são de uma fatia só (0); a superfície que um decodificador de câmera entrega costuma ser
/// a fatia *k* de um array, e com 0 fixo o encoder leria outra fatia — um quadro velho ou fora de
/// ordem, sem erro nenhum (a revisão adversarial de 18/09, G2). O terceiro argumento de
/// `MFCreateDXGISurfaceBuffer` é esse índice.
pub fn sample_from_texture(
    texture: &ID3D11Texture2D,
    subrecurso: u32,
    time_100ns: i64,
    duration_100ns: i64,
) -> Result<IMFSample> {
    unsafe {
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, subrecurso, false)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time_100ns)?;
        sample.SetSampleDuration(duration_100ns)?;
        Ok(sample)
    }
}

/// Um quadro de saída já extraído do `IMFSample` do encoder: bytes Annex-B crus (o MFT de H.264
/// da Microsoft, mesmo em modo hardware, entrega o stream elementar já com start codes — não
/// length-prefixed como em MP4) e se contém um NAL de IDR.
pub struct EncodedFrame {
    pub bytes: Vec<u8>,
    pub is_idr: bool,
}

pub const OUTPUT_STREAM_ID: u32 = 0;

/// Drena toda a saída disponível do MFT no momento (chamado depois de um `METransformHaveOutput`
/// — pode haver mais de uma amostra pronta).
pub fn drain_output(transform: &IMFTransform, output_stream_id: u32) -> Result<Vec<EncodedFrame>> {
    let mut out = Vec::new();
    loop {
        let stream_info = unsafe { transform.GetOutputStreamInfo(output_stream_id)? };
        let provides_own_samples =
            (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: output_stream_id,
            pSample: ManuallyDrop::new(None),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };
        if !provides_own_samples {
            let buffer = unsafe { MFCreateMemoryBuffer(stream_info.cbSize)? };
            let sample = unsafe { MFCreateSample()? };
            unsafe { sample.AddBuffer(&buffer)? };
            output_buffer.pSample = ManuallyDrop::new(Some(sample));
        }

        let mut status = 0u32;
        let hr = unsafe {
            transform.ProcessOutput(0, std::slice::from_mut(&mut output_buffer), &mut status)
        };

        match hr {
            Ok(()) => {
                let taken = std::mem::replace(&mut output_buffer.pSample, ManuallyDrop::new(None));
                if let Some(sample) = ManuallyDrop::into_inner(taken) {
                    if let Some(frame) = extract_annex_b(&sample) {
                        out.push(frame);
                    }
                }
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
            // O MFT de Quick Sync desta máquina devolve `E_UNEXPECTED` (0x8000FFFF) em vez do
            // `MF_E_TRANSFORM_NEED_MORE_INPUT` documentado quando não há mais saída pronta —
            // reproduzido de forma consistente nesta bancada em centenas de chamadas (ver
            // README.md, seção de achados). Trato como o mesmo sinal de "nada mais por agora": um
            // MFT que fica alternando "uma amostra real" / "E_UNEXPECTED" a cada chamada não está
            // com erro real a cada segunda vez, só não tem mais o que dar naquele instante.
            Err(e) if e.code() == windows::Win32::Foundation::E_UNEXPECTED => break,
            // O MFT do Quick Sync devolve isso na primeira saída depois do `SetInputType`: o
            // formato de saída precisa ser renegociado antes que qualquer amostra saia — mesmo
            // já tendo sido setado antes do `SetInputType`. Medido nesta bancada (ver
            // `README.md`). A correção documentada pela própria Microsoft é reconsultar
            // `GetOutputAvailableType` e chamar `SetOutputType` de novo antes de repetir
            // `ProcessOutput`.
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let new_type = unsafe { transform.GetOutputAvailableType(output_stream_id, 0)? };
                unsafe { transform.SetOutputType(output_stream_id, &new_type, 0)? };
            }
            // Qualquer outro erro: para de tentar essa drenagem, mas devolve o que já foi
            // extraído com sucesso em vez de descartar — um `IMFSample` já retirado do MFT não
            // deve ser jogado fora por causa de uma chamada seguinte que falhou.
            Err(e) => {
                eprintln!("    [diag] erro não tratado, devolvendo {} quadro(s) já extraído(s): {e}", out.len());
                break;
            }
        }
    }
    Ok(out)
}

fn extract_annex_b(sample: &IMFSample) -> Option<EncodedFrame> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer().ok()?;
        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        let mut cur_len = 0u32;
        buffer.Lock(&mut data_ptr, None, Some(&mut cur_len)).ok()?;
        let bytes = std::slice::from_raw_parts(data_ptr, cur_len as usize).to_vec();
        let _ = buffer.Unlock();

        let is_idr = contains_idr_nal(&bytes);
        Some(EncodedFrame { bytes, is_idr })
    }
}

/// Varre o Annex-B em busca de um NAL tipo 5 (IDR). Reconhece start codes de 3 e 4 bytes.
fn contains_idr_nal(bytes: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 3 < bytes.len() {
        let (start_len, is_start) = if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 {
            (3, true)
        } else if i + 4 < bytes.len() && bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 0 && bytes[i + 3] == 1 {
            (4, true)
        } else {
            (1, false)
        };
        if is_start {
            let nal_byte_idx = i + start_len;
            if nal_byte_idx < bytes.len() {
                let nal_type = bytes[nal_byte_idx] & 0x1F;
                if nal_type == 5 {
                    return true;
                }
            }
            i += start_len;
        } else {
            i += 1;
        }
    }
    false
}

/// Marca o começo do fluxo depois que os tipos de mídia estão setados.
pub fn start_stream(transform: &IMFTransform) -> Result<()> {
    unsafe {
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }
    Ok(())
}

/// Drena e fecha o fluxo — chamado ao final da captura pra escoar os quadros que já entraram no
/// encoder mas ainda não saíram.
pub fn end_stream_and_drain(transform: &IMFTransform, events_rx: &Receiver<MftEvent>) -> Result<Vec<EncodedFrame>> {
    let mut out = Vec::new();
    unsafe { transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)? };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() > deadline {
            eprintln!("aviso: drenagem do encoder não terminou em 5s, seguindo com o que saiu até aqui");
            break;
        }
        match events_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(MftEvent::HaveOutput) => match drain_output(transform, OUTPUT_STREAM_ID) {
                Ok(mut frames) => out.append(&mut frames),
                Err(e) => eprintln!("aviso: drain_output (drenagem final) falhou: {e}"),
            },
            Ok(MftEvent::DrainComplete) => break,
            Ok(_) => {}
            Err(_) => continue,
        }
    }
    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
    }
    Ok(out)
}

#[cfg(test)]
mod testes {
    use super::e_da_intel;

    #[test]
    fn reconhece_o_quick_sync_pelo_nome_real_do_dell() {
        // O nome exato que o `MFTEnumEx` deste Dell devolve, `®` incluído.
        assert!(e_da_intel("Intel® Quick Sync Video H.264 Encoder MFT"));
    }

    #[test]
    fn recusa_a_nvidia_e_o_encoder_de_software() {
        assert!(!e_da_intel("NVIDIA H.264 Encoder MFT"));
        assert!(!e_da_intel("H264 Encoder MFT"));
        assert!(!e_da_intel("Microsoft H264 Video Encoder MFT"));
    }

    #[test]
    fn nao_depende_da_caixa_nem_do_simbolo_de_marca() {
        assert!(e_da_intel("intel quick sync video h.264 encoder mft"));
        assert!(e_da_intel("Quick Sync Video Encoder"));
    }
}
