//! Declara as bibliotecas de sistema do Windows que o `datachannel-sys` deixa de fora.
//!
//! É o mesmo conteúdo de `crates/quall-core/build.rs`, e a duplicação é necessária — não
//! descuido.
//!
//! # Por que os dois precisam declarar
//!
//! Diretivas de link de um build script só chegam ao binário final se o crate que as emite
//! estiver no grafo daquele link. `quall-core` está no grafo de `quall-probe` e das cascas, mas
//! **não** está no grafo do binário de teste do `quall-rtc`: esse liga só `quall-rtc`,
//! `datachannel` e `datachannel-sys`.
//!
//! Foi assim que o Dell G3 da bancada quebrou em 2026-08-21: `cargo build --release -p
//! quall-probe` passava e `cargo test --workspace` falhava com `LNK1120`, **só** no alvo de
//! teste do `quall-rtc`. Um portão da CI vermelho com o produto compilando é a forma mais cara
//! de descobrir isso.
//!
//! Cada biblioteca está aqui por um símbolo concreto que faltou naquele link:
//!
//! - `advapi32`: `RegisterEventSourceW`, `ReportEventW`, `DeregisterEventSource` (log de eventos
//!   do OpenSSL) e `CryptAcquireContextW`, `CryptGenRandom`, `CryptReleaseContext` (entropia pela
//!   CryptoAPI antiga).
//! - `user32`: `MessageBoxW`, `GetProcessWindowStation`, `GetUserObjectInformationW` — o OpenSSL
//!   usa para decidir se está rodando como serviço antes de tentar mostrar um erro na tela.
//! - `bcrypt`: `BCryptGenRandom`, a fonte de aleatoriedade do `juice`.
//! - `crypt32`: `CertOpenSystemStoreW`, `CertFindCertificateInStore`,
//!   `CertFreeCertificateContext`, `CertCloseStore` — repositório de certificados do sistema.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        for lib in ["advapi32", "user32", "bcrypt", "crypt32"] {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
}
