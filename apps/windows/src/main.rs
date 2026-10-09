//! Sonda de bancada da Frente 2: captura a tela do Dell G3 por N segundos via
//! Windows.Graphics.Capture, codifica em H.264 baseline por um MFT de hardware (NVENC ou Quick
//! Sync, o que a máquina expuser) e escreve o par `.h264` (Annex-B) + `.json` (metadados por
//! quadro) que é o contrato de entrega com a Frente 1. Ver `README.md` desta pasta para as
//! decisões de arquitetura e os números medidos.

use quall_capture_probe::diagnostico_eprintln as eprintln;
use quall_capture_probe::{capture, device, encoder, sidecar};

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use crossbeam_channel::select;

use quall_core::protocol::EncodePreset;

use encoder::{ChosenEncoder, EncodedFrame, EncoderConfig, MftEvent};
use sidecar::{FrameRecord, Header, Sidecar};

/// Nome da API de captura, gravado literalmente no sidecar (`header.capture_api`) — ver
/// `docs/contrato-sidecar.md`. Cada frente de captura usa uma API diferente (ScreenCaptureKit no
/// macOS, MediaProjection no Android...); o contrato pede o nome real, não um enum.
const CAPTURE_API_NAME: &str = "Windows.Graphics.Capture";

/// Faixa de cor do fluxo produzido por este pipeline — `"limited"` (não `"full"`). Não é
/// calculado em tempo de execução: é uma constante porque o MFT de Quick Sync desta bancada
/// converte o ARGB32 de entrada pra NV12/YUV em faixa limitada por padrão (confirmado com
/// `ffprobe`: `pix_fmt=yuv420p`, não `yuvj420p` — o "j" no nome do pixel format do ffmpeg marca
/// faixa completa). Se algum dia este código passar a configurar a faixa de cor explicitamente
/// (via `MF_MT_VIDEO_NOMINAL_RANGE` ou equivalente) em vez de aceitar o default do driver, esta
/// constante precisa ser revista — hoje ela só *declara* o que o pipeline observadamente produz.
const COLOR_RANGE: &str = "limited";

/// `EncodePreset` (de `quall-core`) serializaria como `"Screen"`/`"Camera"` (nome da variante
/// Rust) se fosse jogado direto no `#[derive(Serialize)]` — o contrato do sidecar
/// (`docs/contrato-sidecar.md`) pede minúsculo, espelhando o enum mas não o formato de
/// serialização dele. Passa pelo enum (via `From<PresetArg> for EncodePreset`, já usado pelo
/// resto do pipeline) em vez de converter `PresetArg` direto pra string: garante que o sidecar
/// descreve o mesmo preset que o resto do código, não um espelho solto do argumento de CLI.
fn preset_key(preset: EncodePreset) -> &'static str {
    match preset {
        EncodePreset::Screen => "screen",
        EncodePreset::Camera => "camera",
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum PresetArg {
    Screen,
    Camera,
}

impl From<PresetArg> for EncodePreset {
    fn from(p: PresetArg) -> Self {
        match p {
            PresetArg::Screen => EncodePreset::Screen,
            PresetArg::Camera => EncodePreset::Camera,
        }
    }
}

#[derive(Parser, Debug)]
#[command(about = "Captura a tela e codifica em H.264 de hardware por N segundos.")]
struct Args {
    /// Duração da captura, em segundos.
    #[arg(long, default_value_t = 15)]
    seconds: u64,

    /// Preset de encode: tela (conteúdo estático, mudanças bruscas) ou câmera (ruído, movimento
    /// contínuo). Só a tela está implementada nesta entrega (M1); câmera fica pra M4.
    #[arg(long, value_enum, default_value_t = PresetArg::Screen)]
    preset: PresetArg,

    /// Taxa de quadros alvo.
    #[arg(long, default_value_t = 30)]
    fps: u32,

    /// Taxa de bits alvo, em bits por segundo. Sem a flag, o padrão depende do `--preset`
    /// (docs/ux-m6.md, tarefa 3): tela 4 Mbps (comprime bem em área plana, mas precisa de IDR
    /// rápido numa mudança brusca de cena), câmera 6 Mbps (ruído de sensor custa mais bits por
    /// quadro). Alinhado com `apps/macos/Sources/QuallCaptureKit/H264Encoder.swift`, que é hoje a
    /// referência do projeto para esta divisão — **não remedido no Dell G3 por esta frente**
    /// (M6 não teve o Dell disponível); só o número que já existia (o de "tela") trocou de preset.
    #[arg(long)]
    bitrate: Option<u32>,

    /// Tamanho do GOP (distância entre quadros IDR), em segundos. Sem a flag, o padrão também
    /// depende do `--preset`: 1 s para tela (recuperação rápida numa mudança brusca de cena), 2 s
    /// para câmera (conteúdo já redundante quadro a quadro, refresh frequente não ajuda).
    ///
    /// **Ressalva medida, não teórica** (README.md, achado 7): o MFT de Quick Sync do Dell G3
    /// recusa controlar isso por `ICodecAPI` (`AVEncMPVGOPSize`) — este valor vira
    /// `force_next_keyframe` chamado a cada N quadros pelo laço principal, e o próprio README
    /// mediu que nem isso muda o intervalo real de IDR nesta máquina (fixo em ~128 quadros,
    /// alheio ao pedido). Ou seja: nesta bancada específica, mudar este número é pedido ao
    /// encoder, não garantia de comportamento — documentado aqui para quem for medir de novo não
    /// presumir que o valor pedido é o que sai no bitstream.
    #[arg(long)]
    gop_seconds: Option<u32>,

    /// Diretório de saída para o `.h264` e o `.json`.
    #[arg(long, default_value = "capturas")]
    out_dir: PathBuf,

    /// **Bancada.** Período do refresh intra gradual, em quadros. `0` (o padrão) desliga.
    ///
    /// Pede `CODECAPI_AVEncVideoGradualIntraRefresh` ao MFT — e **não**
    /// `AVEncVideoIntraRefreshMode`/`Period`, que não existem no SDK do Windows (§4.1 de
    /// `docs/idr-pequeno.md`). Nos dois Android da bancada o botão equivalente foi honrado como
    /// **outra coisa** (o A10s emite IDR a cada N quadros) ou aceito e ignorado (o A07), e o
    /// VideoToolbox não tem refresh intra nenhum. Este braço existe para descobrir se o Media
    /// Foundation é diferente — e a resposta é lida em `tools/fatias.py`, no `.h264`, nunca no
    /// retorno do `SetValue`.
    ///
    /// **Ainda não rodou**: a única origem que este emissor captura é um monitor, e o monitor do
    /// Dell é a máquina de trabalho do usuário. Ver §4 daquele documento.
    #[arg(long, default_value_t = 0)]
    refresh_intra: u32,

    /// **Bancada.** Tamanho máximo de fatia, em bytes. `0` (o padrão) desliga.
    ///
    /// Pede `AVEncSliceControlMode=1` (bits) + `AVEncSliceControlSize`. Abaixo de
    /// `MAX_FRAGMENTO` (1188 B) cada fatia cabe num pacote RTP. **Não paga sozinho hoje** — ver
    /// `docs/idr-pequeno.md`, a parte do decodificador.
    #[arg(long, default_value_t = 0)]
    fatia_bytes: u32,
}

fn main() {
    quall_capture_probe::higiene_do_registro::instalar_hook_do_executavel();
    quall_capture_probe::diagnostico_cli::concluir(rodar());
}

fn rodar() -> windows::core::Result<()> {
    let args = quall_capture_probe::diagnostico_cli::interpretar::<Args>();
    let process_start = Instant::now();

    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        )
        .ok()?;
        windows::Win32::Media::MediaFoundation::MFStartup(
            windows::Win32::Media::MediaFoundation::MF_VERSION,
            windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
        )?;
    }

    let resultado = run(&args, process_start);

    unsafe {
        let _ = windows::Win32::Media::MediaFoundation::MFShutdown();
        windows::Win32::System::Com::CoUninitialize();
    }

    resultado
}

fn run(args: &Args, process_start: Instant) -> windows::core::Result<()> {
    fs::create_dir_all(&args.out_dir).expect("cria diretório de saída");
    disable_power_throttling();

    eprintln!("=== adaptadores DXGI disponíveis ===");
    for a in device::list_adapters()? {
        eprintln!("  - {a}");
    }

    // O encoder é escolhido *antes* do dispositivo D3D11: um MFT de hardware só aceita
    // `MFT_MESSAGE_SET_D3D_MANAGER` com um dispositivo criado no mesmo adaptador que ele (medido
    // nesta bancada — ver README.md, seção de achados). Criar o dispositivo primeiro e torcer
    // pra bater com o encoder que a enumeração escolher é frágil; melhor descobrir o encoder e
    // então criar o dispositivo no adaptador certo.
    let chosen_encoder: ChosenEncoder = encoder::find_and_activate_h264_encoder()?;
    eprintln!(
        "encoder ativado: \"{}\" (hardware: {})",
        chosen_encoder.friendly_name, chosen_encoder.is_hardware
    );

    let vendor_guess = if chosen_encoder.friendly_name.to_uppercase().contains("NVIDIA") {
        device::VENDOR_NVIDIA
    } else {
        device::VENDOR_INTEL
    };
    let chosen_adapter = device::create_device(vendor_guess)?;
    eprintln!(
        "adaptador escolhido para captura+encode: {} (vendor 0x{:04X})",
        chosen_adapter.description, chosen_adapter.vendor_id
    );

    let device_manager = encoder::create_device_manager(&chosen_adapter.device)?;

    let capture = capture::ScreenCapture::start_primary_monitor(&chosen_adapter.device)?;
    eprintln!("captura iniciada: {}x{}", capture.width, capture.height);

    let preset: EncodePreset = args.preset.into();
    let bitrate_bps = args.bitrate.unwrap_or(match args.preset {
        PresetArg::Screen => 4_000_000,
        PresetArg::Camera => 6_000_000,
    });
    let gop_seconds = args.gop_seconds.unwrap_or(match args.preset {
        PresetArg::Screen => 1,
        PresetArg::Camera => 2,
    });
    let gop_frames = args.fps * gop_seconds;

    let cfg = EncoderConfig {
        entrada: encoder::FormatoDeEntrada::Argb32,
        width: capture.width,
        height: capture.height,
        fps: args.fps,
        bitrate_bps,
        gop_frames,
        intra_refresh_frames: args.refresh_intra,
        slice_bytes: args.fatia_bytes,
        teto_de_quadro_bits: 0,
    };
    encoder::configure(&chosen_encoder, &device_manager, &cfg)?;
    encoder::start_stream(&chosen_encoder.transform)?;

    let events_rx = encoder::spawn_event_pump(chosen_encoder.events.clone());

    let stamp = process_start.elapsed().as_millis();
    let h264_path = args.out_dir.join(format!("quall-captura-{stamp}.h264"));
    let json_path = args.out_dir.join(format!("quall-captura-{stamp}.json"));
    let mut h264_file = BufWriter::new(File::create(&h264_path).expect("cria arquivo .h264"));

    let mut frame_index: u64 = 0;
    let mut need_input_credits: u32 = 0;
    let mut pending_frame: Option<capture::CapturedFrame> = None;
    // Fila FIFO: sem B-frames e sem reordenação configurados, a ordem de saída do encoder
    // acompanha a ordem de entrada — então casar quadro submetido com quadro codificado por
    // ordem de chegada é suficiente pra medir a latência captura→pacote (número 1 do contrato).
    let mut submitted: VecDeque<(u64, Instant)> = VecDeque::new();
    let mut frame_records: Vec<FrameRecord> = Vec::new();

    let frame_duration_100ns = 10_000_000i64 / args.fps as i64;
    let run_start = Instant::now();

    let mut diag_need_input = 0u64;
    let mut diag_have_output = 0u64;
    let mut diag_frame_ready = 0u64;
    let mut diag_process_input_err = 0u64;
    let mut last_report = run_start;

    while run_start.elapsed() < Duration::from_secs(args.seconds) {
        select! {
            recv(capture.frame_ready) -> _ => {
                diag_frame_ready += 1;
                if let Some(frame) = capture.take_frame() {
                    pending_frame = Some(frame);
                }
            }
            recv(events_rx) -> msg => {
                match msg {
                    Ok(MftEvent::NeedInput) => { need_input_credits += 1; diag_need_input += 1; }
                    Ok(MftEvent::HaveOutput) => {
                        diag_have_output += 1;
                        match encoder::drain_output(&chosen_encoder.transform, encoder::OUTPUT_STREAM_ID) {
                            Ok(frames) => write_frames(frames, &mut submitted, &mut frame_records, &mut h264_file, process_start),
                            Err(e) => eprintln!("aviso: drain_output falhou: {e}"),
                        }
                    }
                    _ => {}
                }
            }
            default(Duration::from_millis(20)) => {}
        }

        if need_input_credits > 0 {
            if let Some(frame) = pending_frame.take() {
                // Tentativa de GOP curto por fora do encoder, já que o MFT recusa
                // `AVEncMPVGOPSize` (ver `apply_low_latency_params`). Medido (ver
                // `force_next_keyframe` em encoder.rs e README.md): o `SetValue` aqui devolve
                // sucesso mas **não força IDR nenhum de verdade** neste MFT — "aceito" no log
                // abaixo só significa que a chamada COM não falhou, não que funcionou. Mantido
                // porque é uma tentativa honesta, documentada como não funcional nesta máquina.
                if frame_index % gop_frames as u64 == 0 {
                    let forced = encoder::force_next_keyframe(&chosen_encoder);
                    if frame_index > 0 {
                        eprintln!(
                            "  pedido de IDR no quadro {frame_index}: chamada {} (não é garantia de efeito — ver achados)",
                            if forced { "aceita" } else { "recusada" }
                        );
                    }
                }

                let t = (frame.captured_at - run_start).as_nanos() as i64 / 100;
                match encoder::sample_from_texture(&frame.texture, 0, t, frame_duration_100ns) {
                    Ok(sample) => {
                        match unsafe { chosen_encoder.transform.ProcessInput(0, &sample, 0) } {
                            Ok(()) => {
                                submitted.push_back((frame_index, frame.captured_at));
                                frame_index += 1;
                                need_input_credits -= 1;
                            }
                            Err(e) => {
                                diag_process_input_err += 1;
                                eprintln!("aviso: ProcessInput recusou o quadro {frame_index}: {e}");
                            }
                        }
                    }
                    Err(e) => eprintln!("aviso: falhou empacotar quadro {frame_index} como amostra: {e}"),
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            eprintln!(
                "diag: frame_ready={diag_frame_ready} need_input={diag_need_input} have_output={diag_have_output} \
                 process_input_err={diag_process_input_err} creditos_pendentes={need_input_credits} \
                 tem_quadro_pendente={}",
                pending_frame.is_some()
            );
            last_report = Instant::now();
        }
    }

    capture.stop();
    eprintln!("captura encerrada; drenando o que já entrou no encoder...");
    let leftover = encoder::end_stream_and_drain(&chosen_encoder.transform, &events_rx)?;
    write_frames(leftover, &mut submitted, &mut frame_records, &mut h264_file, process_start);
    h264_file.flush().expect("descarrega .h264");

    let (media, p50, p95, max) = latency_stats(&frame_records);
    eprintln!(
        "quadros capturados: {}, quadros codificados: {}",
        frame_index,
        frame_records.len()
    );
    eprintln!(
        "latência captura→encode (us): média={media} p50={p50} p95={p95} máx={max}"
    );

    let header = Header {
        width: capture.width,
        height: capture.height,
        target_fps: args.fps,
        preset: preset_key(preset).to_string(),
        capture_api: CAPTURE_API_NAME.to_string(),
        encoder: chosen_encoder.friendly_name.clone(),
        encoder_is_hardware: chosen_encoder.is_hardware,
        target_bitrate_bps: bitrate_bps,
        gop_frames,
        color_range: COLOR_RANGE.to_string(),
        video_file: h264_path.file_name().unwrap().to_string_lossy().to_string(),
    };
    let sidecar = Sidecar { header, frames: frame_records };
    let json = serde_json::to_string_pretty(&sidecar).expect("serializa sidecar");
    fs::write(&json_path, json).expect("escreve .json");

    eprintln!("gravado: {}", h264_path.display());
    eprintln!("gravado: {}", json_path.display());

    Ok(())
}

fn write_frames(
    frames: Vec<EncodedFrame>,
    submitted: &mut VecDeque<(u64, Instant)>,
    frame_records: &mut Vec<FrameRecord>,
    h264_file: &mut impl Write,
    process_start: Instant,
) {
    for frame in frames {
        let Some((number, captured_at)) = submitted.pop_front() else {
            eprintln!("aviso: quadro codificado sem correspondência na fila de submissão; descartando dos metadados (bytes ainda vão pro .h264)");
            let _ = h264_file.write_all(&frame.bytes);
            continue;
        };
        let now = Instant::now();
        h264_file.write_all(&frame.bytes).expect("escreve quadro no .h264");
        frame_records.push(FrameRecord {
            number,
            timestamp_us: (captured_at - process_start).as_micros() as u64,
            bytes: frame.bytes.len() as u32,
            idr: frame.is_idr,
            encode_latency_us: (now - captured_at).as_micros() as u64,
        });
    }
}

/// Tira este processo do regime de "economia de energia" que o Windows aplica a processos sem
/// janela/foco (EcoQoS / Desktop Activity Moderator). Achado medido nesta bancada (ver
/// `README.md`, seção de achados): sem isso, a taxa de entrega de quadros do WGC despenca de
/// ~30-40 fps pra ~2 fps depois de alguns minutos de máquina sem interação humana — mesmo com
/// conteúdo mudando na tela — porque este processo nunca tem foco (não tem janela) e roda via
/// Tarefa Agendada, os dois sinais que o Windows usa pra classificar algo como "background".
///
/// Desde o R5 (`docs/teleprompter-com-camera.md` §8.2, a S-W1) o pedido é o do app:
/// `EXECUTION_SPEED | IGNORE_TIMER_RESOLUTION`, `StateMask = 0` (`energia.rs`). Até 25/09 esta sonda
/// pedia só o `EXECUTION_SPEED`.
fn disable_power_throttling() {
    eprintln!("{}", quall_capture_probe::energia::desligar_throttling_do_processo());
}

fn latency_stats(frames: &[FrameRecord]) -> (u64, u64, u64, u64) {
    if frames.is_empty() {
        return (0, 0, 0, 0);
    }
    let mut values: Vec<u64> = frames.iter().map(|f| f.encode_latency_us).collect();
    values.sort_unstable();
    let sum: u64 = values.iter().sum();
    let media = sum / values.len() as u64;
    let p50 = values[values.len() / 2];
    let p95 = values[(values.len() * 95 / 100).min(values.len() - 1)];
    let max = *values.last().unwrap();
    (media, p50, p95, max)
}
