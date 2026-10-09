//! Quall Monitor: a small desktop shell for extended displays and screen reception.
#![cfg(windows)]
#![windows_subsystem = "windows"]

use clap::Parser;
use quall_capture_probe::{
    argumentos::Argumentos, emissor::Emissor, identidade, instancia, janela, receptor::Receptor,
    registro,
};
use std::path::PathBuf;
use std::time::Duration;
use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

#[derive(Parser)]
#[command(
    name = "quall-monitor",
    version,
    about = "Quall Monitor — estender a área de trabalho ou exibir uma tela pela rede local"
)]
struct Opcoes {
    /// Arquivo do diário; o padrão pertence apenas ao Quall Monitor.
    #[arg(long)]
    registro: Option<PathBuf>,
    /// Porta da sinalização; zero escolhe uma porta livre.
    #[arg(long, default_value_t = 0)]
    porta: u16,
    /// Taxa alvo do vídeo, de 1 a 60 quadros por segundo.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=60))]
    fps: u32,
    /// Estender sem transmitir o som deste computador.
    #[arg(long)]
    sem_som: bool,
}

fn mensagem(texto: &str, erro: bool) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };
    let largo: Vec<u16> = texto.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(largo.as_ptr()),
            w!("Quall Monitor"),
            MB_OK
                | if erro {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                },
        );
    }
}

fn main() {
    // Setup runs before any user data, network, or media is opened. The MSI is elevated;
    // the app's explicit driver dialog relaunches this executable through UAC.
    let brutos: Vec<String> = std::env::args().skip(1).collect();
    if brutos
        .first()
        .is_some_and(|a| a == quall_capture_probe::regras_do_driver::ARGUMENTO)
    {
        std::process::exit(
            quall_capture_probe::driver_da_tela_estendida::elevado::rodar(&brutos) as i32,
        );
    }
    quall_capture_probe::higiene_do_registro::instalar_hook_do_executavel();
    std::panic::set_hook(Box::new(|falha| {
        let _ = registro::abrir(None);
        let resumo = quall_capture_probe::higiene_do_registro::resumo_panico(falha);
        registro::linha(format!("Quall Monitor: falha inesperada: {resumo}"));
        mensagem(&resumo, true);
    }));
    if let Err(e) = rodar() {
        registro::linha(format!("Quall Monitor: não conseguiu iniciar: {e}"));
        mensagem(
            &format!("Quall Monitor não conseguiu iniciar.\n\n{e}"),
            true,
        );
        std::process::exit(1);
    }
}

fn rodar() -> windows::core::Result<()> {
    let opcoes = match Opcoes::try_parse() {
        Ok(o) => o,
        Err(e) => {
            mensagem(&e.to_string(), e.use_stderr());
            std::process::exit(e.exit_code());
        }
    };
    let caminho = registro::abrir(opcoes.registro.as_deref());
    quall_capture_probe::idioma::iniciar(&identidade::pasta_de_dados());
    let _instancia = match instancia::tomar() {
        instancia::Instancia::Primeira(g) => g,
        instancia::Instancia::OutraAberta => return Ok(()),
    };
    let mut args = Argumentos::parse_from(["quall-monitor"]);
    args.registro = opcoes.registro;
    args.porta = opcoes.porta;
    args.fps = opcoes.fps;
    args.sem_som = opcoes.sem_som;
    args.sem_cameras = true;
    args.sem_cameras_virtuais = true;
    // The public parser has no source, camera, prompter, recording, or synthetic-input flags.
    // The emitter independently limits its selector to its one virtual-display source.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    if caminho.is_none() {
        mensagem("O diário do Quall Monitor não pôde ser aberto.", false);
    }
    registro::linha("quall-monitor-distribuicao: desktop-v1");
    registro::linha(quall_capture_probe::energia::desligar_throttling_do_processo());
    let receptor = Receptor::novo(args.clone());
    let emissor = Emissor::novo(args);
    let resultado = janela::correr(
        std::sync::Arc::clone(&emissor),
        std::sync::Arc::clone(&receptor),
        Vec::new(),
    );
    emissor.encerrar();
    receptor.encerrar();
    receptor.busca.parar();
    let desmontou = emissor.esperar_desmonte(Duration::from_secs(12));
    let restantes = quall_capture_probe::monitores_virtuais::encerrar_tudo(Duration::from_secs(8));
    registro::linha(format!(
        "Quall Monitor encerrou: desmonte={desmontou}, monitores restantes soltos={restantes}"
    ));
    std::thread::sleep(Duration::from_millis(700));
    if desmontou {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
    resultado
}
