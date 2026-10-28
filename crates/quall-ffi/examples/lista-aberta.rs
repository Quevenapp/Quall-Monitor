//! **A lista aberta por N segundos, sem aparelho** (`docs/v3-no-privado.md` §8.9): abre o navegador
//! pela fronteira C, como as cascas, lê a lista a cada 250 ms e, no fim, diz quantos itens viu e o
//! diagnóstico. Serve para medir o que a busca põe na rede — `arp -an | grep -c incomplete` antes e
//! depois, e o `Use` das entradas incompletas em `netstat -rnl -f inet`.
//!
//! ```text
//! cargo run --release -p quall-ffi --example lista-aberta -- 60            # a lista aberta 60 s
//! cargo run --release -p quall-ffi --example lista-aberta -- 60 procurar   # e "Procurar de novo" no meio
//! cargo run --release -p quall-ffi --example lista-aberta -- 60 reabrir    # fecha e abre a lista no meio
//! cargo run --release -p quall-ffi --example lista-aberta -- anunciar 90   # só anuncia (outro processo)
//! ```

use std::ffi::CStr;
use std::time::{Duration, Instant};

use quall::QuallBrowser;

fn texto(f: impl Fn(*mut std::os::raw::c_char, usize) -> isize) -> String {
    let n = f(std::ptr::null_mut(), 0);
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u8; usize::try_from(n).unwrap_or(0) + 1];
    let escrito = f(buf.as_mut_ptr().cast(), buf.len());
    if escrito < 0 {
        return String::new();
    }
    CStr::from_bytes_until_nul(&buf).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default()
}

fn anunciar(segundos: u64) {
    use quall_core::protocol::{Announcement, Capabilities, DeviceId, PROTOCOL_VERSION};
    let anuncio = Announcement {
        protocol_version: PROTOCOL_VERSION,
        device_id: DeviceId("medida-da-varredura".into()),
        display_name: "Medida".into(),
        capabilities: Capabilities { screen_source: false, camera_source: true, sink: false },
        screen: None,
        papel: None,
    };
    let _a = quall_core::discovery::Advertiser::start(&anuncio, 7877).expect("o anúncio subiu");
    println!("anunciando por {segundos} s");
    std::thread::sleep(Duration::from_secs(segundos));
}

fn relatar(b: *mut QuallBrowser, maior: i32) {
    // SAFETY: `b` veio de `quall_browser_start` e ainda não foi liberado; os buffers são os de `texto`.
    let lista = texto(|p, c| unsafe { quall::quall_browser_devices_json(b, p, c) });
    let dica = texto(|p, c| unsafe { quall::quall_browser_hint_json(b, p, c) });
    let itens: Vec<serde_json::Value> = serde_json::from_str(&lista).unwrap_or_default();
    println!("itens no fim: {} (no máximo {maior})", itens.len());
    for i in &itens {
        println!("  {} via {} próprio={}", i["endpoint"], i["via"], i["own"]);
    }
    println!("diagnóstico: {dica}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("anunciar") {
        anunciar(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(90));
        return;
    }
    let segundos: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(60);
    let modo = args.get(1).map(String::as_str).unwrap_or("");
    let mut b = quall::quall_browser_start();
    assert!(!b.is_null(), "o navegador não subiu");
    let inicio = Instant::now();
    let mut feito = false;
    let mut maior = 0;
    while inicio.elapsed() < Duration::from_secs(segundos) {
        // SAFETY: `b` veio de `quall_browser_start` e só é liberado no fim (ou na troca abaixo).
        let n = unsafe { quall::quall_browser_collect(b, 250) };
        maior = maior.max(n);
        if !feito && inicio.elapsed() >= Duration::from_secs(segundos / 2) {
            feito = true;
            match modo {
                "procurar" => {
                    // SAFETY: idem.
                    unsafe { quall::quall_browser_rescan(b) };
                    println!("{:>5.1} s: procurar de novo", inicio.elapsed().as_secs_f32());
                }
                "reabrir" => {
                    relatar(b, maior);
                    // SAFETY: `b` não é usado depois de parado; o novo vem de `quall_browser_start`.
                    unsafe { quall::quall_browser_stop(b) };
                    b = quall::quall_browser_start();
                    assert!(!b.is_null(), "o navegador não subiu de novo");
                    println!("{:>5.1} s: a lista fechou e abriu", inicio.elapsed().as_secs_f32());
                }
                _ => {}
            }
        }
    }
    relatar(b, maior);
    // SAFETY: `b` não é usado depois.
    unsafe { quall::quall_browser_stop(b) };
}
