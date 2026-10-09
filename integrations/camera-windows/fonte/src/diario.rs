//! Diário em arquivo.
//!
//! Esta DLL roda dentro de `svchost.exe -k Camera` (conta `NT AUTHORITY\LocalService`), dentro do
//! Frame Server Monitor (`LocalSystem`) e dentro do processo do app que consome a câmera. Nenhum
//! desses tem console, nenhum é depurável por SSH sem cerimônia, e o do Frame Server nem sequer
//! roda na sessão do usuário. **O arquivo é o único jeito de saber o que aconteceu**, e por isso
//! toda linha carrega processo e PID: sem isso não dá para responder à pergunta que decide o
//! desenho — *em qual processo a fonte de mídia foi instanciada?*

use std::fmt::Write as _;
use std::io::Write as _;

pub const PASTA: &str = r"C:\ProgramData\Quall";

pub fn caminho() -> String {
    format!(r"{PASTA}\camera-fonte.log")
}

pub fn linha(texto: &str) {
    let texto = crate::higiene_do_registro::sanitizar(texto);
    let mut s = String::new();
    let _ = write!(
        s,
        "{} [{} pid={} tid={}] {}\n",
        agora(),
        processo(),
        std::process::id(),
        thread_id(),
        texto
    );
    let _ = std::fs::create_dir_all(PASTA);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(caminho())
    {
        let _ = f.write_all(s.as_bytes());
    }
}

#[macro_export]
macro_rules! diga {
    ($($t:tt)*) => { $crate::diario::linha(&format!($($t)*)) };
}

fn thread_id() -> u32 {
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}

fn processo() -> String {
    // Mesmo laço de `lib.rs`, e não um `[0u16; 260]`: `GetModuleFileNameW` trunca em silêncio e
    // devolve um número que parece sucesso. Aqui o dano seria só cosmético (só o nome do arquivo
    // entra na linha do diário), mas duas leituras do mesmo contrato não podem discordar — foi
    // assim que o padrão `(buf, cap)` acabou lido ao contrário em três cascas.
    let caminho = crate::caminho_de_modulo(None);
    let arquivo = caminho
        .rsplit('\\')
        .next()
        .unwrap_or(&caminho)
        .to_ascii_lowercase();
    // Nomes arbitrários de executável também podem identificar o usuário; mantém categorias
    // técnicas conhecidas, PID/TID e horários, sem inventariar apps do consumidor.
    match arquivo.as_str() {
        "svchost.exe" => "svchost",
        "dllhost.exe" => "dllhost",
        "quall-app.exe" | "quall-camera-sonda.exe" => "quall",
        "obs64.exe" | "obs32.exe" => "obs",
        _ => "outro-consumidor",
    }.into()
}

/// Relógio de parede em texto. Evita `chrono` — uma dependência a mais numa DLL que o Frame Server
/// carrega não se paga por um carimbo de hora.
fn agora() -> String {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t: SYSTEMTIME = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
