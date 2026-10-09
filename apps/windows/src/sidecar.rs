//! Formato do par de saída `.h264` + `.json` que é o contrato com as outras frentes: o elementar
//! stream Annex-B de um lado, os metadados por quadro do outro.
//!
//! Nomes de chave em **inglês**, fixados por `docs/contrato-sidecar.md` — a primeira rodada do
//! M1 especificou o contrato em prosa sem fixar nomes, e esta frente (Windows) e a de macOS
//! escolheram nomes razoáveis e incompatíveis (`cabecalho`/`quadros` aqui, `header`/`frames` lá).
//! Nomear é parte do contrato, não detalhe de implementação — por isso os nomes dos campos deste
//! struct são literalmente as chaves do JSON (nomes de campo em Rust já em snake_case batem com
//! as chaves esperadas, sem precisar de `#[serde(rename)]`).
//!
//! Validado por `tools/valida-sidecar.py`, que roda fora deste código.
//!
//! `Deserialize` foi acrescentado pela Frente 6 (M2): o receptor lê o mesmo `.json` que este
//! arquivo escreve, para exercitar o decoder contra captura real de bancada enquanto a track do
//! núcleo não existe (ver `connect.rs` e `bin/quall_receiver_probe.rs`). Ler e escrever com o
//! mesmo struct é o que garante que os dois lados nunca divergem silenciosamente nos nomes de
//! campo — o problema que este contrato existe para evitar.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub target_fps: u32,
    /// `"screen"` ou `"camera"`, minúsculo — espelha `EncodePreset` de `quall-core`, mas como
    /// string literal em vez do enum: `EncodePreset` serializa como `"Screen"`/`"Camera"` (nome
    /// da variante Rust), e o contrato pede minúsculo. Convertida em `main.rs`.
    pub preset: String,
    /// Qual API capturou, com o nome real dela — não um enum, porque o contrato quer o nome
    /// legível da API, e outras frentes (ScreenCaptureKit, MediaProjection...) têm nomes
    /// completamente diferentes.
    pub capture_api: String,
    /// Nome amigável do MFT realmente ativado (ex.: "Intel® Quick Sync Video H.264 Encoder
    /// MFT") — o que rodou de verdade, não o que era pretendido (NVENC pode enumerar e não
    /// ativar; ver README.md).
    pub encoder: String,
    pub encoder_is_hardware: bool,
    pub target_bitrate_bps: u32,
    pub gop_frames: u32,
    /// `"full"` ou `"limited"` — faixa de cor do fluxo. Obrigatório no contrato porque as duas
    /// primeiras entregas (macOS, Windows) divergiram nisso sem que ninguém tivesse declarado o
    /// que cada pipeline realmente produzia.
    pub color_range: String,
    pub video_file: String,
}

#[derive(Serialize, Deserialize)]
pub struct FrameRecord {
    pub number: u64,
    /// Timestamp de captura em microssegundos, relógio monotônico do processo (`Instant`), não
    /// relógio de parede — o mesmo instante usado pra calcular a latência captura→pacote.
    pub timestamp_us: u64,
    pub bytes: u32,
    pub idr: bool,
    /// Latência captura → amostra codificada disponível, em microssegundos. É o número 1 do
    /// contrato de medição, por quadro; `README.md` reporta o agregado (média/p50/p95/máx).
    pub encode_latency_us: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Sidecar {
    pub header: Header,
    pub frames: Vec<FrameRecord>,
}
