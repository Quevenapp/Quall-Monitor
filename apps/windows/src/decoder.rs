//! Decode H.264 em hardware via Media Foundation Transform (MFT) assíncrono.
//!
//! Espelha a decisão de arquitetura do encoder (`encoder.rs`): os MFTs de decode de hardware são
//! registrados em `MFT_CATEGORY_VIDEO_DECODER` com `MFT_ENUM_FLAG_HARDWARE`. Não existe uma "API
//! D3D11VA" separada para chamar direto no Windows — o MFT de hardware **é** o caminho D3D11VA/
//! DXVA2 neste SO: é o mesmo mecanismo que `mf.dll`/Filmes e TV usam. Por isso esta entrega não
//! considerou DXVA2 legado como alternativa separada (pedido no escopo): o MFT de hardware já
//! entrega saída em textura D3D11 direto, sem o caminho DXVA2 clássico (que é mais antigo, pensado
//! para D3D9 e Vista/7) trazer nada a mais aqui.
//!
//! Assíncrono pelas mesmas razões do encoder: desbloquear (`MF_TRANSFORM_ASYNC_UNLOCK`) e dirigir
//! por `METransformNeedInput`/`METransformHaveOutput` via `IMFMediaEventGenerator` — reaproveita
//! `encoder::MftEvent` e `encoder::spawn_event_pump`, que não têm nada específico de encode.
//!
//! O que este arquivo **mede, e não supõe** (pedido explícito do escopo desta frente): se este
//! decoder sofre de alguma cegueira análoga à do encoder (que ignora `AVEncMPVGOPSize` e
//! `CODECAPI_AVEncVideoForceKeyFrame` de verdade, mesmo aceitando a chamada — ver `encoder.rs`),
//! e qual código de erro este MFT devolve para "nada mais por agora" em `ProcessOutput` — o
//! encoder mediu `E_UNEXPECTED` em vez do `MF_E_TRANSFORM_NEED_MORE_INPUT` documentado; o
//! `README.md` (seção "Frente 6") registra o que foi medido aqui, igual ou diferente.

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use windows::core::{Interface, Result, GUID};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;

use crate::encoder::{self, MftEvent};

pub struct ChosenDecoder {
    pub transform: IMFTransform,
    /// `None` quando o MFT é **síncrono** — achado desta bancada, não suposição: nenhum MFT de
    /// decode H.264 se registrou como hardware assíncrono aqui (só o "Microsoft H264 Video
    /// Decoder MFT" apareceu, e ele não expõe `IMFMediaEventGenerator` — `cast` devolve
    /// `E_NOINTERFACE`). Ver README.md, seção "Frente 6": o laço de condução muda de forma
    /// (`main` bin, `is_async`), não só o tipo.
    pub events: Option<IMFMediaEventGenerator>,
    pub friendly_name: String,
    /// `true` só quando o MFT se anunciou como assíncrono (`MF_TRANSFORM_ASYNC`), que nesta
    /// bancada nunca aconteceu para decode — ver `is_hardware` abaixo para o que de fato indica
    /// aceleração.
    pub is_async: bool,
    /// Sinal indireto de hardware: o MFT aceita `MF_SA_D3D11_AWARE` (ou seja, foi desenhado para
    /// trabalhar com superfícies D3D11, não só memória de sistema). **Não é prova** — só o
    /// formato real da amostra de saída (textura D3D11 vs. buffer de memória, conferido em
    /// `extract_texture`) prova hardware de verdade. Ver README.md.
    pub is_hardware: bool,
}

/// Enumera MFTs de decode H.264, hardware primeiro (mesma preferência por NVIDIA do encoder, de
/// propósito: é o teste natural para "o decoder ativa onde o encoder não ativou?" — ver
/// `README.md`), e cai para o decoder de software da Microsoft só se nenhum de hardware ativar.
pub fn find_and_activate_h264_decoder() -> Result<ChosenDecoder> {
    let input_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };

    let mut candidates: Vec<(String, IMFActivate)> = Vec::new();
    for hardware_only in [true, false] {
        let flags = if hardware_only {
            // **`ASYNCMFT` junto.** `MFTEnumEx` exclui MFTs assíncronos quando a bandeira não é
            // pedida, e decodificador de hardware com nome próprio costuma ser assíncrono. Pedida
            // em 09/09/2026, ela **não mudou nada** nesta bancada: continua um candidato só, o
            // "Microsoft H264 Video Decoder MFT". A bandeira fica porque está certa e não custa.
            //
            // **E o nome desse MFT não diz se o decode é de hardware.** Com um
            // `IMFDXGIDeviceManager` registrado (ver [`configure`]) ele é o caminho DXVA do
            // sistema; sem ele, é software. Em 09/09 escrevi que o app "caía para o decodificador
            // de software" e que o teto de 39 fps era dele — **sem controle nenhum**, e contra uma
            // medida que o repositório já tinha: o `README.md` desta pasta ("Números medidos",
            // item 3) viu `engtype_videodecode` trabalhando para o PID do receptor. A prova de
            // hoje não é o nome: é a linha `textura de saída … BIND_DECODER=` do registro e os
            // motores da GPU atribuídos ao PID (`tools/prova-sintetica-no-dell.py --gpu`).
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
        } else {
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
        };

        let mut array_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                flags,
                Some(&input_type),
                None,
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

    if candidates.is_empty() {
        return Err(windows::core::Error::new(E_NOTIMPL, "nenhum MFT de decode H.264 encontrado"));
    }

    // **No registro, não no `stderr`.** Sob Tarefa Agendada — que é como o app roda em toda
    // corrida desta bancada — o `stderr` não vai a lugar nenhum, e por isso a pergunta *"por que
    // só um candidato de decode?"* ficou dias sem resposta enquanto o app decodificava 1080p em
    // software num Dell que tem Quick Sync. Ver `docs/bancada.md` §8.63.
    crate::registro::linha(format!(
        "MFTs de decode H.264 candidatos ({}): {}",
        candidates.len(),
        candidates
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    ));

    candidates.sort_by_key(|(n, _)| {
        let upper = n.to_uppercase();
        if upper.contains("NVIDIA") || upper.contains("NVENC") || upper.contains("NVDEC") {
            0
        } else {
            1
        }
    });

    let mut last_err = None;
    for (friendly, activate) in candidates {
        crate::registro::linha(format!("ativando decoder \"{friendly}\"..."));
        match unsafe { activate.ActivateObject::<IMFTransform>() } {
            Ok(transform) => {
                // Ao contrário do encoder desta bancada (sempre assíncrono), o cast para
                // `IMFMediaEventGenerator` aqui **falha** para o único MFT de decode H.264 que
                // esta máquina expõe — medido, não suposto (ver doc do struct). Isso não é razão
                // para descartar o candidato: é um MFT síncrono, e síncrono é conduzido chamando
                // `ProcessInput`/`ProcessOutput` direto, sem fila de eventos. O chamador decide o
                // laço certo olhando `is_async`.
                let events: Option<IMFMediaEventGenerator> = match transform.cast() {
                    Ok(e) => Some(e),
                    Err(e) => {
                        eprintln!(
                            "  \"{friendly}\" não expõe IMFMediaEventGenerator ({e}) — tratando como MFT síncrono"
                        );
                        None
                    }
                };
                let is_async = events.is_some();
                let is_hardware = unsafe {
                    transform
                        .GetAttributes()
                        .and_then(|attrs| attrs.GetUINT32(&MF_SA_D3D11_AWARE))
                        .unwrap_or(0)
                        == 1
                };
                eprintln!(
                    "  ativado com sucesso (assíncrono={is_async}, MF_SA_D3D11_AWARE={is_hardware} — só indício, ver README.md)"
                );
                return Ok(ChosenDecoder {
                    transform,
                    events,
                    friendly_name: friendly,
                    is_async,
                    is_hardware,
                });
            }
            Err(e) => {
                // No registro pelo mesmo motivo da lista de candidatos: um decoder de hardware que
                // é enumerado e **não ativa** é exatamente o caso que se quer ver, e ele some no
                // `stderr` de uma Tarefa Agendada.
                crate::registro::linha(format!("falhou ativar decoder \"{friendly}\": {e}"));
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| windows::core::Error::new(E_NOTIMPL, "nenhum MFT de decode ativou")))
}

fn friendly_name(activate: &IMFActivate) -> Option<String> {
    unsafe {
        let mut pwstr = windows::core::PWSTR::null();
        let mut len = 0u32;
        activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut pwstr, &mut len).ok()?;
        let s = pwstr.to_string().ok();
        if !pwstr.is_null() {
            CoTaskMemFree(Some(pwstr.0 as *const _));
        }
        s
    }
}

pub struct DecoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Configura o decoder: registra o `IMFDXGIDeviceManager` (saída em textura D3D11, sem baixar
/// pixel pra CPU — o mesmo caminho sem cópia que o encoder usa na entrada), define o tipo de
/// entrada (H.264 comprimido) e escolhe NV12 como formato de saída (o nativo dos decoders de
/// hardware desta bancada — a conversão pra RGB fica pro Video Processor em `present.rs`, também
/// em hardware, não em CPU).
pub fn configure(
    dec: &ChosenDecoder,
    device_manager: &IMFDXGIDeviceManager,
    cfg: &DecoderConfig,
) -> Result<()> {
    unsafe {
        if let Ok(attrs) = dec.transform.GetAttributes() {
            let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
            // `MF_LOW_LATENCY` como atributo do MFT (não da sessão de mídia) não é documentado
            // como suportado por todo decoder — tentativa best-effort, resultado só logado, igual
            // ao `try_set` do `encoder.rs`. É exatamente o tipo de "SetValue aceita, será que
            // funciona?" que o encoder ensinou a desconfiar; não medi o efeito real disto no
            // bitstream (não há um "GOP" pra decoder cujo efeito dê pra medir do lado de fora
            // como se mediu no encoder) — ver README.md.
            let ok = attrs.SetUINT32(&MF_LOW_LATENCY, 1).is_ok();
            eprintln!("  MF_LOW_LATENCY no decoder: {}", if ok { "aceito" } else { "recusado" });
        }

        let mgr_unknown: windows::core::IUnknown = device_manager.cast()?;
        let mgr_ptr = windows::core::Interface::as_raw(&mgr_unknown) as usize;
        dec.transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr_ptr)?;
        eprintln!("  D3D manager registrado no MFT de decode");

        // --- tipo de entrada: H.264 comprimido, no tamanho declarado pelo sidecar de captura. ---
        let in_type = MFCreateMediaType()?;
        in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        set_attribute_size(&in_type, &MF_MT_FRAME_SIZE, cfg.width, cfg.height)?;
        set_attribute_ratio(&in_type, &MF_MT_FRAME_RATE, cfg.fps, 1)?;
        in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        eprintln!("  chamando SetInputType (H264)...");
        dec.transform.SetInputType(0, &in_type, 0)?;
        eprintln!("  SetInputType ok");

        // --- tipo de saída: primeiro candidato NV12 que a enumeração oferecer. A enumeração de
        // saída só fica disponível depois do `SetInputType` em decoders — o MFT precisa saber o
        // que vai decodificar antes de dizer o que consegue entregar. ---
        let mut chosen_output = false;
        let mut i = 0u32;
        loop {
            let candidate = match dec.transform.GetOutputAvailableType(0, i) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("  GetOutputAvailableType parou em i={i}: {e}");
                    break;
                }
            };
            let subtype = candidate.GetGUID(&MF_MT_SUBTYPE).unwrap_or(GUID::zeroed());
            eprintln!("  candidato de saída {i}: subtype={subtype:?}");
            if subtype == MFVideoFormat_NV12 {
                match dec.transform.SetOutputType(0, &candidate, 0) {
                    Ok(()) => {
                        chosen_output = true;
                        eprintln!("tipo de saída: MFVideoFormat_NV12 (conversão de cor fica pro Video Processor)");
                        break;
                    }
                    Err(e) => eprintln!("  SetOutputType recusou NV12: {e}"),
                }
            }
            i += 1;
        }
        if !chosen_output {
            return Err(windows::core::Error::new(
                E_NOTIMPL,
                "MFT de decode não ofereceu NV12 na saída",
            ));
        }
    }
    Ok(())
}

fn set_attribute_size(attrs: &IMFMediaType, key: &GUID, width: u32, height: u32) -> Result<()> {
    let packed = ((width as u64) << 32) | (height as u64);
    unsafe { attrs.SetUINT64(key, packed) }
}

fn set_attribute_ratio(attrs: &IMFMediaType, key: &GUID, numerator: u32, denominator: u32) -> Result<()> {
    let packed = ((numerator as u64) << 32) | (denominator as u64);
    unsafe { attrs.SetUINT64(key, packed) }
}

/// Marca o começo do fluxo. Idêntico ao do encoder — `ProcessMessage` não tem nada específico de
/// direção — então reaproveita `encoder::start_stream` em vez de duplicar duas linhas.
pub fn start_stream(transform: &IMFTransform) -> Result<()> {
    encoder::start_stream(transform)
}

/// Empacota bytes Annex-B (um quadro do `.h264`) como `IMFSample` de entrada. Ao contrário da
/// amostra de entrada do encoder (que referencia uma textura D3D11 da captura), esta é memória de
/// sistema — o `.h264` chega como bytes comprimidos, não como pixel.
pub fn sample_from_bytes(bytes: &[u8], time_100ns: i64, duration_100ns: i64) -> Result<IMFSample> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
        let mut data_ptr: *mut u8 = std::ptr::null_mut();
        buffer.Lock(&mut data_ptr, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr, bytes.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(bytes.len() as u32)?;

        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time_100ns)?;
        sample.SetSampleDuration(duration_100ns)?;
        Ok(sample)
    }
}

/// Um quadro decodificado: textura D3D11 (NV12) mais o índice do subrecurso — decoders de
/// hardware costumam reciclar um pool pequeno de texturas (arrays), então "o quadro" é sempre um
/// par (textura, índice), nunca só a textura.
///
/// # O campo que não se usa, e por que ele é o mais importante da struct
///
/// **Medido em 2026-08-27, pela régua de blocos, e este defeito existia desde o M2.** A versão
/// anterior largava a `IMFSample` dentro de `extract_texture` e guardava só a textura. A referência
/// COM mantém o *recurso* vivo — mas não diz ao MFT que aquela superfície ainda está em uso, e o
/// alocador dele a recicla no `ProcessOutput` seguinte. Com `drain_output` juntando um lote antes
/// de o chamador olhar, as primeiras entradas do lote já tinham sido **sobrescritas pelas
/// últimas** quando alguém as lia.
///
/// O sintoma era invisível para toda medição que este projeto tinha: 1024 submetidos, 1024
/// decodificados, 1024 apresentados, latência plausível — e **10% dos quadros apresentados eram o
/// quadro errado**, tipicamente quatro à frente. Contador nenhum acusa isso; só a régua acusou,
/// porque ela é a única medida que pergunta *qual* quadro chegou em vez de *quantos*.
///
/// Segurar a amostra é metade do conserto. A outra metade é o chamador consumir um quadro antes de
/// pedir o próximo — ver [`proximo_quadro`].
pub struct DecodedFrame {
    pub texture: ID3D11Texture2D,
    pub subresource_index: u32,
    /// Mantida viva de propósito. Nunca lida.
    _amostra: IMFSample,
}

pub const OUTPUT_STREAM_ID: u32 = 0;

/// Drena toda a saída disponível do MFT no momento.
///
/// Ao contrário do encoder — que trata `E_UNEXPECTED` como sinônimo medido de "nada mais por
/// agora" nesta bancada — este laço só trata o `MF_E_TRANSFORM_NEED_MORE_INPUT` documentado até
/// prova em contrário: é o comportamento a **confirmar por medição** nesta entrega, não a copiar
/// do encoder por semelhança. Se a bancada mostrar o mesmo padrão de erro não documentado, o
/// ajuste fica registrado aqui e no README.md, não escondido.
/// **Um `ProcessOutput`, um quadro** — e o chamador tem de consumi-lo antes de pedir o próximo.
///
/// É o par de [`DecodedFrame::_amostra`]: junto, os dois fecham o defeito de reciclagem de textura
/// que a régua achou. `Ok(None)` quer dizer "nada pronto agora", que é o caso normal e não erro.
///
/// Prefira esta a [`drain_output`] em qualquer caminho que **olhe** o conteúdo do quadro
/// (apresentar, ler a régua). `drain_output` continua existindo para quem só quer contar.
pub fn proximo_quadro(
    transform: &IMFTransform,
    output_stream_id: u32,
) -> Result<Option<DecodedFrame>> {
    let (mut um, _) = drain_ate(transform, output_stream_id, 1)?;
    Ok(um.pop())
}

/// Como [`proximo_quadro`], e ainda diz **se o fluxo trocou de geometria** nesta volta.
///
/// Existe porque quem desenha precisa saber: a janela e o escalador da câmera virtual são montados
/// para um tamanho, e um `MF_E_TRANSFORM_STREAM_CHANGE` os deixa desalinhados sem erro nenhum.
pub fn proximo_quadro_com_mudanca(
    transform: &IMFTransform,
    output_stream_id: u32,
) -> Result<(Option<DecodedFrame>, Option<(u32, u32)>)> {
    let (mut um, mudou) = drain_ate(transform, output_stream_id, 1)?;
    Ok((um.pop(), mudou))
}

pub fn drain_output(transform: &IMFTransform, output_stream_id: u32) -> Result<Vec<DecodedFrame>> {
    Ok(drain_ate(transform, output_stream_id, usize::MAX)?.0)
}

fn drain_ate(
    transform: &IMFTransform,
    output_stream_id: u32,
    teto: usize,
) -> Result<(Vec<DecodedFrame>, Option<(u32, u32)>)> {
    let mut out = Vec::new();
    let mut mudou: Option<(u32, u32)> = None;
    loop {
        if out.len() >= teto {
            break;
        }
        let stream_info = unsafe { transform.GetOutputStreamInfo(output_stream_id)? };
        let provides_own_samples =
            (stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

        if !provides_own_samples {
            // Não medido nesta bancada (os MFTs de decode de hardware do Dell provêem a própria
            // amostra, D3D11-aware, quando o `IMFDXGIDeviceManager` está registrado — ver
            // README.md). Alocar o buffer de saída manualmente via `IMFDXGIDeviceManager` seria o
            // próximo passo se algum dia isto disparar; não implementado porque não foi o caso.
            return Err(windows::core::Error::new(
                E_NOTIMPL,
                "MFT de decode não provê a própria amostra de saída (caminho não implementado nesta entrega)",
            ));
        }

        let mut output_buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: output_stream_id,
            pSample: ManuallyDrop::new(None),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };

        let mut status = 0u32;
        let hr = unsafe {
            transform.ProcessOutput(0, std::slice::from_mut(&mut output_buffer), &mut status)
        };

        match hr {
            Ok(()) => {
                let taken = std::mem::replace(&mut output_buffer.pSample, ManuallyDrop::new(None));
                if let Some(sample) = ManuallyDrop::into_inner(taken) {
                    if let Some(frame) = extract_texture(&sample) {
                        out.push(frame);
                    }
                }
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let new_type = unsafe { transform.GetOutputAvailableType(output_stream_id, 0)? };
                unsafe { transform.SetOutputType(output_stream_id, &new_type, 0)? };
                // **Isto acontecia em silêncio, e silêncio aqui é caro.** O decoder trocar de tipo
                // de saída no meio do fluxo muda a geometria com que **todo o resto** — a janela e
                // o escalador da câmera virtual — foi montado. Uma imagem inteira verde com listras
                // verticais é exatamente o sintoma de plano UV lido no passo errado, e foi o que
                // Bruno fotografou em 09/09/2026 depois de sair do app do S24 e voltar, com
                // `suspeitos=5` na sessão — ou seja, com a cadeia de referência **sã**.
                //
                // O tamanho vai para o registro e sobe para quem desenha decidir o que fazer.
                let tam = unsafe { new_type.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
                let (w, h) = ((tam >> 32) as u32, (tam & 0xFFFF_FFFF) as u32);
                crate::registro::linha(format!(
                    "decoder: o fluxo mudou de tipo no meio da sessão — geometria nova {w}x{h}"
                ));
                mudou = Some((w, h));
            }
            Err(e) => {
                eprintln!(
                    "    [diag decode] erro não tratado em ProcessOutput (0x{:08X}), devolvendo {} quadro(s) já extraído(s): {e}",
                    e.code().0,
                    out.len()
                );
                break;
            }
        }
    }
    Ok((out, mudou))
}

/// O tamanho **codificado** da saída e o retângulo dentro dele que é imagem de verdade.
///
/// Os dois não são iguais, e em 10/09/2026 isso apareceu no app: um fluxo 1920x**1080** do x264
/// abre o decoder, e na primeira saída o MFT troca o tipo para 1920x**1088** — a textura alinhada
/// a macrobloco. Tratar os 1088 como imagem põe as oito linhas de enchimento dentro da câmera
/// virtual. Quem diz o recorte é `MF_MT_MINIMUM_DISPLAY_APERTURE`; é a mesma leitura que
/// `integrations/camera-windows/sonda/src/receber.rs::abertura_do_tipo` já fazia.
pub fn abertura_de_saida(
    transform: &IMFTransform,
) -> Option<((u32, u32), windows::Win32::Foundation::RECT)> {
    use windows::Win32::Foundation::RECT;
    unsafe {
        let tipo = transform.GetOutputCurrentType(OUTPUT_STREAM_ID).ok()?;
        let empacotado = tipo.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let codificado = ((empacotado >> 32) as u32, empacotado as u32);
        let inteira = RECT { left: 0, top: 0, right: codificado.0 as i32, bottom: codificado.1 as i32 };

        // `MFVideoArea`: OffsetX (u16 fração + i16 valor), OffsetY (idem), Area (dois i32).
        let mut area = [0u8; 16];
        let mut lidos = 0u32;
        let visivel = match tipo.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, Some(&mut lidos)) {
            Ok(()) if lidos as usize == area.len() => {
                let ox = i16::from_le_bytes([area[2], area[3]]) as i32;
                let oy = i16::from_le_bytes([area[6], area[7]]) as i32;
                let cx = i32::from_le_bytes([area[8], area[9], area[10], area[11]]);
                let cy = i32::from_le_bytes([area[12], area[13], area[14], area[15]]);
                if cx > 0
                    && cy > 0
                    && ox >= 0
                    && oy >= 0
                    && (ox + cx) as u32 <= codificado.0
                    && (oy + cy) as u32 <= codificado.1
                {
                    RECT { left: ox, top: oy, right: ox + cx, bottom: oy + cy }
                } else {
                    inteira
                }
            }
            // Sem abertura declarada, a textura inteira **é** a imagem (720 já é múltiplo de 16).
            _ => inteira,
        };
        Some((codificado, visivel))
    }
}

fn extract_texture(sample: &IMFSample) -> Option<DecodedFrame> {
    unsafe {
        let buffer = sample.GetBufferByIndex(0).ok()?;
        let dxgi_buffer: IMFDXGIBuffer = buffer.cast().ok()?;

        // `IMFDXGIBuffer::GetResource` não ganhou o invólucro genérico `Result<T>` do
        // `windows-rs` (ao contrário de, por exemplo, `IMFActivate::ActivateObject`) — medido
        // direto no erro do compilador nesta versão (0.62.2): a assinatura exposta é a COM crua,
        // com `riid`/`ppv` de saída. `Interface::IID` dá o GUID sem precisar declará-lo à mão.
        let mut texture: Option<ID3D11Texture2D> = None;
        dxgi_buffer
            .GetResource(
                &ID3D11Texture2D::IID,
                &mut texture as *mut Option<ID3D11Texture2D> as *mut *mut core::ffi::c_void,
            )
            .ok()?;
        let texture = texture?;

        let subresource_index = dxgi_buffer.GetSubresourceIndex().ok()?;
        Some(DecodedFrame { texture, subresource_index, _amostra: sample.clone() })
    }
}

/// Drena e fecha o fluxo. `events_rx` é `None` no caminho síncrono medido nesta bancada (ver
/// `ChosenDecoder::events`) — sem fila de eventos, a drenagem é só chamar `drain_output` em
/// laço até ele não devolver mais nada, sem esperar `DrainComplete`.
pub fn end_stream_and_drain(
    transform: &IMFTransform,
    events_rx: Option<&Receiver<MftEvent>>,
) -> Result<Vec<DecodedFrame>> {
    let mut out = Vec::new();
    unsafe { transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)? };
    let deadline = Instant::now() + Duration::from_secs(5);

    match events_rx {
        Some(rx) => loop {
            if Instant::now() > deadline {
                eprintln!("aviso: drenagem do decoder não terminou em 5s, seguindo com o que saiu até aqui");
                break;
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(MftEvent::HaveOutput) => match drain_output(transform, OUTPUT_STREAM_ID) {
                    Ok(mut frames) => out.append(&mut frames),
                    Err(e) => eprintln!("aviso: drain_output (drenagem final) falhou: {e}"),
                },
                Ok(MftEvent::DrainComplete) => break,
                Ok(_) => {}
                Err(_) => continue,
            }
        },
        None => loop {
            if Instant::now() > deadline {
                eprintln!("aviso: drenagem do decoder não terminou em 5s, seguindo com o que saiu até aqui");
                break;
            }
            match drain_output(transform, OUTPUT_STREAM_ID) {
                Ok(frames) if frames.is_empty() => break,
                Ok(mut frames) => out.append(&mut frames),
                Err(e) => {
                    eprintln!("aviso: drain_output (drenagem final, síncrono) falhou: {e}");
                    break;
                }
            }
        },
    }

    unsafe {
        let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
    }
    Ok(out)
}
