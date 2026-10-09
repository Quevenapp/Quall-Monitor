//! Compila a libopus vendorizada em `vendor/opus` com o crate `cc`.
//!
//! # Por que `cc` compilando as fontes, e o que foi descartado
//!
//! Medido nesta bancada em 2026-08-27. As alternativas todas existem e todas caíram por um motivo
//! escrito:
//!
//! - **`audiopus_sys` 0.2.2** (e o `opus` 0.3.x, que depende dele). Traz a libopus como submódulo
//!   vendorizado e a constrói por **cmake**. O `CMakeLists.txt` daquela libopus declara
//!   `cmake_minimum_required(VERSION 3.1)`, e o CMake desta bancada é o **4.4.2**, que removeu a
//!   compatibilidade com < 3.5. Conferido rodando: *"Compatibility with CMake < 3.5 has been
//!   removed from CMake"*, configuração abortada. Dá para contornar com
//!   `-DCMAKE_POLICY_VERSION_MINIMUM=3.5`, mas o `build.rs` que passaria essa flag é de terceiro —
//!   contorná-lo exigiria fork ou fixar uma versão antiga de cmake na bancada e na CI. Nenhum dos
//!   dois é reprodutível.
//! - **`opusic-sys` 0.7.x** (e o `magnum-opus`). cmake **mais bindgen obrigatório**. O bindgen
//!   arrastaria libclang para o build do *nosso* código e precisaria receber o sysroot certo para
//!   o `armv7-linux-androideabi`. Precisamos de **doze** funções da libopus; gerar ligações para
//!   elas é mais maquinário do que escrevê-las.
//! - **libopus do sistema por `pkg-config`**. Não existe libopus em iOS nem em Windows — é
//!   exatamente o problema que esta frente veio resolver.
//! - **Submódulo git**. O repositório não usa submódulo nenhum hoje, e o precedente do projeto é
//!   vendorizar por dentro do crate (`datachannel-sys` com a feature `vendored`). Submódulo também
//!   estraga `cargo vendor` e build offline.
//!
//! O que fez `cc` ganhar:
//!
//! 1. **Não acrescenta crate nenhum ao grafo.** `cc` 1.4.4 já estava no `Cargo.lock`, puxado por
//!    `cmake` e `openssl-src` por conta do `datachannel-sys`.
//! 2. **O cross-compile do Android sai de graça.** O `tools/android-env.sh` já exporta
//!    `CC_armv7_linux_androideabi`, `AR_*` e `RANLIB_*` exatamente na forma minúscula-sublinhado
//!    que o crate `cc` lê. Nada de novo a ensinar à bancada nem à CI.
//! 3. **É a única opção que deixa *escolher o que compilar*** — que é a pergunta inteira do custo
//!    de tamanho. Ver abaixo.
//!
//! # A lista de fontes vem do upstream, não da minha mão
//!
//! A libopus publica as próprias listas de fontes como fragmentos de makefile — `opus_sources.mk`,
//! `celt_sources.mk`, `silk_sources.mk`. Este build script **lê esses arquivos** em vez de carregar
//! uma lista copiada à mão. Subir de versão passa a ser trocar `vendor/opus` e recompilar; não há
//! lista minha para sair de sincronia com a de lá em silêncio.
//!
//! # O que foi podado do upstream, e quanto pesava
//!
//! A árvore de 1.5.2 tem 16 MB. `vendor/opus` tem **2,8 MB**. A diferença é quase toda o
//! diretório **`dnn/` (11 MB)**: LPCNet, DRED e OSCE — a ocultação de perda por rede neural que
//! entrou no Opus 1.5. Ela é opcional por construção: todo o código que a chama está sob
//! `ENABLE_DEEP_PLC`, `ENABLE_DRED` e `ENABLE_OSCE`, e nenhum desses símbolos é definido aqui.
//! Também ficaram de fora `tests/`, `doc/` e os três programas de demonstração de `src/`
//! (`opus_demo.c`, `opus_compare.c`, `repacketizer_demo.c`).
//!
//! # Sobre "desabilitar o encoder onde só se decodifica"
//!
//! A libopus **não tem** uma opção de build que produza uma biblioteca só de decodificação. Ela
//! não precisa: encoder e decoder moram em unidades de tradução separadas (`opus_encoder.c` contra
//! `opus_decoder.c`, `celt_encoder.c` contra `celt_decoder.c`, `enc_API.c` contra `dec_API.c`).
//! Como isto vira um `.a`, o ligador descarta os membros que ninguém referencia — quem só
//! decodifica não paga pelo encoder **sem precisar de flag nenhuma**. Está medido em
//! `docs/audio.md` §11, e é por isso que o tamanho do arquivo `.a` não é o número que importa.

use std::path::{Path, PathBuf};

/// Lê uma variável de um fragmento de makefile do upstream.
///
/// O formato é `NOME = \` seguido de uma linha por arquivo, cada uma terminada em `\` menos a
/// última. A comparação do nome é exata de propósito: `SILK_SOURCES` não pode casar com
/// `SILK_SOURCES_FIXED`, que é uma lista diferente e incompatível.
fn lista_do_makefile(texto: &str, nome: &str) -> Vec<String> {
    let mut saida = Vec::new();
    let mut dentro = false;

    for linha in texto.lines() {
        let t = linha.trim();
        if !dentro {
            let Some(resto) = t.strip_prefix(nome) else {
                continue;
            };
            let resto = resto.trim_start();
            let Some(resto) = resto.strip_prefix('=') else {
                // Casou o prefixo mas não é esta variável (ex.: `SILK_SOURCES_FIXED`).
                continue;
            };
            dentro = true;
            let item = resto.trim().trim_end_matches('\\').trim();
            if !item.is_empty() {
                saida.push(item.to_string());
            }
            if !t.ends_with('\\') {
                break;
            }
        } else {
            let continua = t.ends_with('\\');
            let item = t.trim_end_matches('\\').trim();
            if !item.is_empty() {
                saida.push(item.to_string());
            }
            if !continua {
                break;
            }
        }
    }

    saida
}

fn ler(caminho: &Path) -> String {
    std::fs::read_to_string(caminho)
        .unwrap_or_else(|e| panic!("não deu para ler {}: {e}", caminho.display()))
}

fn main() {
    let raiz = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("opus");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", raiz.display());

    let ponto_fixo = std::env::var("CARGO_FEATURE_FIXED_POINT").is_ok();
    let sem_float = std::env::var("CARGO_FEATURE_SEM_API_DE_FLOAT").is_ok();

    // As listas, lidas do upstream.
    let mk_opus = ler(&raiz.join("opus_sources.mk"));
    let mk_celt = ler(&raiz.join("celt_sources.mk"));
    let mk_silk = ler(&raiz.join("silk_sources.mk"));

    let mut fontes: Vec<String> = Vec::new();
    fontes.extend(lista_do_makefile(&mk_opus, "OPUS_SOURCES"));
    fontes.extend(lista_do_makefile(&mk_celt, "CELT_SOURCES"));
    fontes.extend(lista_do_makefile(&mk_silk, "SILK_SOURCES"));

    if ponto_fixo {
        fontes.extend(lista_do_makefile(&mk_silk, "SILK_SOURCES_FIXED"));
    } else {
        fontes.extend(lista_do_makefile(&mk_silk, "SILK_SOURCES_FLOAT"));
        // `OPUS_SOURCES_FLOAT` é a análise de sinal que decide música contra fala. Ela só existe
        // na build de ponto flutuante, e é ela que faz o encoder escolher SILK ou CELT sozinho —
        // exatamente o que a §3 do `docs/audio.md` afirma sobre os presets.
        fontes.extend(lista_do_makefile(&mk_opus, "OPUS_SOURCES_FLOAT"));
    }

    assert!(
        fontes.len() > 100,
        "as listas do upstream vieram vazias ou truncadas: {} arquivos. \
         O formato de `*_sources.mk` mudou?",
        fontes.len()
    );

    let mut build = cc::Build::new();
    build
        .include(raiz.join("include"))
        .include(&raiz)
        .include(raiz.join("celt"))
        .include(raiz.join("silk"));

    build.include(
        raiz.join("silk")
            .join(if ponto_fixo { "fixed" } else { "float" }),
    );

    for f in &fontes {
        build.file(raiz.join(f));
    }

    // Sem `HAVE_CONFIG_H`: não geramos `config.h`: tudo o que o autoconf definiria entra aqui,
    // onde dá para ler numa tela só.
    build.define("OPUS_BUILD", None);

    let msvc = build.get_compiler().is_like_msvc();
    if msvc {
        // O `stack_alloc.h` exige um dos três modos de alocação temporária. O MSVC não tem
        // vetor de tamanho variável de C99; tem `_alloca`.
        build.define("USE_ALLOCA", None);
    } else {
        build.define("VAR_ARRAYS", None);
        // `lrintf`/`lrint` de C99. Sem isto a libopus cai num arredondamento de software.
        build.define("HAVE_LRINTF", None);
        build.define("HAVE_LRINT", None);
    }

    if ponto_fixo {
        build.define("FIXED_POINT", None);
    }
    if sem_float {
        build.define("DISABLE_FLOAT_API", None);
    }

    // `opus_get_version_string()` monta a resposta a partir daqui. Sem isto o build quebra no
    // pré-processador, e é o único lugar em que o autoconf realmente faz falta.
    build.define("PACKAGE_VERSION", Some("\"1.5.2-quall\""));

    build
        // A libopus é código de terceiro e compila com avisos que não são nossos para consertar.
        // O `RUSTFLAGS: -D warnings` da CI não alcança C, mas o ruído esconderia um aviso real.
        .warnings(false)
        .flag_if_supported("-fvisibility=hidden")
        .compile("opus");

    println!("cargo:vendorizado=1.5.2");
}
