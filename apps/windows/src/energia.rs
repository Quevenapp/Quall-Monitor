//! **O throttling de energia do processo, desligado** (`docs/teleprompter-com-camera.md` §8.2, a
//! S-W1, e §8.10 peça 1).
//!
//! Na sessão interativa do Bruno, com o *power throttling* como veio, a S-W1 mediu os dois Quick Sync
//! a 21–27 fps, com o despertar da thread atrasado p50 8 ms e p95 15 ms; desligado (`EXECUTION_SPEED`
//! **e** `IGNORE_TIMER_RESOLUTION`, `StateMask = 0`), 30,00 fps sem perda. O processo do app é um só
//! (o dono da captura, a rede, o gravador, o receptor e o prompter vivem nele): o pedido vale para
//! ele inteiro, uma vez, no começo.
//!
//! `EXECUTION_SPEED` sozinho é o que a sonda `quall-capture-probe` pedia desde o M1 (`main.rs`); o
//! `IGNORE_TIMER_RESOLUTION` é o que faz o Windows honrar a resolução de relógio do processo mesmo
//! com a janela atrás de outra (a S-W1 desligou os dois juntos; o efeito de cada um sozinho não foi
//! separado — hipótese).

#![cfg(windows)]

use windows::Win32::System::Threading::{
    GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION as IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
};

/// Desliga o throttling do processo nas duas dimensões. Devolve o texto para o registro: o que foi
/// pedido e o que o `SetProcessInformation` respondeu. Numa versão do Windows que não conhece o
/// `IGNORE_TIMER_RESOLUTION`, o pedido duplo pode ser recusado: então pede só o `EXECUTION_SPEED`
/// e diz.
pub fn desligar_throttling_do_processo() -> String {
    let pedir = |mascara: u32| -> Result<(), String> {
        let estado = PROCESS_POWER_THROTTLING_STATE {
            Version: 1, // PROCESS_POWER_THROTTLING_CURRENT_VERSION
            ControlMask: mascara,
            StateMask: 0,
        };
        unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                &estado as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        }
        .map_err(|e| e.to_string())
    };
    let as_duas = PROCESS_POWER_THROTTLING_EXECUTION_SPEED | IGNORE_TIMER_RESOLUTION;
    match pedir(as_duas) {
        Ok(()) => "throttling de energia desligado: EXECUTION_SPEED | IGNORE_TIMER_RESOLUTION, StateMask 0 (aceito)".into(),
        Err(e) => match pedir(PROCESS_POWER_THROTTLING_EXECUTION_SPEED) {
            Ok(()) => format!(
                "throttling de energia: o pedido duplo foi recusado ({e}); desligado só o EXECUTION_SPEED (aceito)"
            ),
            Err(e2) => format!("!! throttling de energia NÃO desligado: duplo ({e}), só EXECUTION_SPEED ({e2})"),
        },
    }
}
