//! Declara as bibliotecas de sistema do Windows que o `datachannel-sys` deixa de fora.
//!
//! O `libdatachannel` (com o OpenSSL e o juice dele) chama serviços do Windows que o build script
//! do `datachannel-sys` 0.23 não declara para o toolchain MSVC. Sem isto o link falha com 14
//! `LNK2019`, sempre em `quall-probe` ou em qualquer casca que ligue o núcleo — nunca ao compilar
//! o crate isolado, o que torna o erro confuso: `cargo check` passa e `cargo build` quebra.
//!
//! Cada biblioteca abaixo está aqui por um símbolo concreto que faltou no Dell G3 da bancada:
//!
//! - `advapi32`: `RegisterEventSourceW`, `ReportEventW`, `DeregisterEventSource` (log de eventos do
//!   OpenSSL) e `CryptAcquireContextW`, `CryptGenRandom`, `CryptReleaseContext` (entropia pela
//!   CryptoAPI antiga).
//! - `user32`: `MessageBoxW`, `GetProcessWindowStation`, `GetUserObjectInformationW` — o OpenSSL usa
//!   para decidir se está rodando como serviço antes de tentar mostrar um erro fatal na tela.
//! - `bcrypt`: `BCryptGenRandom`, a fonte de aleatoriedade do `juice`.
//! - `crypt32`: `CertOpenSystemStoreW`, `CertFindCertificateInStore`, `CertFreeCertificateContext`,
//!   `CertCloseStore` — repositório de certificados do sistema.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        for lib in ["advapi32", "user32", "bcrypt", "crypt32"] {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
}
