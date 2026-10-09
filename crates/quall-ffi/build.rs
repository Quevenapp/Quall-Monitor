use std::{env, path::PathBuf};

/// Gera `include/quall.h` a cada build, para que o header que Swift, Kotlin e C++ consomem nunca
/// fique atrás do Rust que ele descreve.
fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let header = crate_dir.join("include").join("quall.h");

    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    match cbindgen::generate(&crate_dir) {
        Ok(bindings) => {
            bindings.write_to_file(&header);
        }
        // Um header desatualizado quebra a compilação das cascas de forma legível; abortar o
        // build do Rust por causa disso esconde o erro real quando o cbindgen é que está infeliz.
        Err(erro) => println!("cargo:warning=cbindgen não gerou o header: {erro}"),
    }
}
