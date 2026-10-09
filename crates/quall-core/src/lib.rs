//! Núcleo compartilhado do Quall.
//!
//! Todas as plataformas — macOS, iOS, Android, Windows, plugin de OBS e câmera virtual —
//! consomem este crate. As cascas por plataforma cuidam de captura, permissões e UI; tudo o
//! que é protocolo, descoberta, pareamento e sinalização mora aqui.
//!
//! Restrições que valem para todo código deste crate:
//!
//! - Precisa compilar para `armv7-linux-androideabi` (Galaxy A10s, 32 bits). Nada de assumir
//!   `usize` de 64 bits nem ponteiro de 8 bytes.
//! - Precisa caber na Broadcast Upload Extension do iOS, que tem ~50 MB de memória para tudo.
//!   Sem buffers globais, sem filas que crescem, sem cache de frames.
//! - `panic = "abort"` em release: um panic aqui derruba o app hospedeiro. Erros são `Result`.

#![forbid(unsafe_code)]

pub mod protocol;

pub mod error;

pub mod camera_remota;
pub mod cancel;
pub mod discovery;
pub mod jitter;
pub mod media;
pub mod pairing;
pub mod portao;
pub mod relogio;
pub mod reproducao;
pub mod rtp;
pub mod session;
pub mod signaling;
// **Sem `///` aqui, e é de propósito.** Um comentário externo num `pub mod` que também tem `//!`
// dentro faz o rustdoc resolver os links do módulo inteiro no escopo do **pai**, e todo
// `[`Tipo`]` do cabeçalho vira link quebrado. Custou nove avisos até alguém rodar `cargo doc`.
pub mod taxa;
pub mod teleprompter;
/// O teto do que qualquer emissor pode pôr na rede, derivado do `fmtp` que `track` anuncia.
/// Mora ao lado dele de propósito — ver o cabeçalho do módulo.
pub mod teto;
pub mod track;
pub mod transport;
