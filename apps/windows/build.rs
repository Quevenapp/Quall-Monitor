//! Embute `app.manifest` e o ícone (`quall.ico`) **só** no binário do app.
//!
//! # Por que não `.cargo/config.toml`
//!
//! Foi a primeira tentativa, e ela quebra o build inteiro. `rustflags` num `config.toml` vale para
//! **toda** invocação de rustc do workspace — inclusive a dos *build scripts* das dependências, que
//! rodam com outro diretório corrente. O resultado foi, literalmente:
//!
//!     error c1010070: Failed to load and parse the manifest.
//!         O sistema não pode encontrar o arquivo especificado.
//!     error: could not compile `libc` (build script)
//!
//! `cargo:rustc-link-arg-bin=NOME=FLAG` resolve as duas coisas de uma vez: vale só para o binário
//! nomeado, e aqui há `CARGO_MANIFEST_DIR` para montar um caminho absoluto — que é o que o
//! `/MANIFESTINPUT` do linker do MSVC exige, já que ele não sabe de onde o Cargo o chamou.
//!
//! # O ícone, sem `rc.exe` e sem crate nova (30/09)
//!
//! O `.exe` não tinha recurso de ícone (`ExtractIconEx` contava 0; ver o comentário do `<Icon>` em
//! `scripts/instalador/Quall.wxs`), e o atalho e "Programas e Recursos" mostravam o genérico. O
//! `link.exe` do MSVC aceita um arquivo `.res` como entrada e o converte sozinho (CVTRES): então este
//! script **escreve o `.res`** a partir do `quall.ico` — um `RT_ICON` por imagem e um `RT_GROUP_ICON`
//! de número 1, que é o que o Explorer e o `<Icon>` do WiX leem — e o passa ao linker do binário. O
//! formato é o de sempre (cabeçalho de 32 bytes por recurso, tudo alinhado a 4); nada a baixar, e o
//! portão continua offline. O `quall.ico` sai de `tools/icones/gerar.py`.

use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=quall.ico");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let raiz = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-arg-bin=quall-monitor=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bin=quall-monitor=/MANIFESTINPUT:{raiz}\\app.manifest");

    let ico = fs::read(Path::new(&raiz).join("quall.ico")).expect("quall.ico (gerado por tools/icones/gerar.py)");
    let icones = res_do_icone(&ico).expect("quall.ico malformado");
    let versao = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let mut res = icones.clone();
    recurso(&mut res, 16, 1, 0x0030, &informacao_de_versao(&versao, "quall-monitor.exe", "Quall Monitor"));
    let saida = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("quall-icone.res");
    fs::write(&saida, res).expect("gravar o .res do ícone");
    println!("cargo:rustc-link-arg-bin=quall-monitor={}", saida.display());


}

/// RT_VERSION (VS_VERSION_INFO), sem rc.exe: os rótulos do Explorer e a versão do Cargo.
/// O formato e os alinhamentos seguem a documentação de Version Information da Microsoft.
fn informacao_de_versao(versao: &str, original: &str, descricao: &str) -> Vec<u8> {
    let mut partes = versao.split('.');
    let major = partes.next().and_then(|p| p.parse::<u16>().ok()).expect("major numérico");
    let minor = partes.next().and_then(|p| p.parse::<u16>().ok()).expect("minor numérico");
    let patch = partes.next().and_then(|p| p.split('-').next()?.parse::<u16>().ok()).expect("patch numérico");
    let ms = (u32::from(major) << 16) | u32::from(minor);
    let ls = u32::from(patch) << 16;
    let fixos: Vec<u8> = [0xFEEF04BDu32, 0x00010000, ms, ls, ms, ls, 0x3F, 0, 0x00040004, 1, 0, 0, 0]
        .iter().flat_map(|d| d.to_le_bytes()).collect();
    let textos: Vec<Vec<u8>> = [
        ("CompanyName", "Veneri & Quellis Ltda."),
        ("FileDescription", descricao),
        ("FileVersion", versao),
        ("InternalName", original.trim_end_matches(".exe")),
        ("OriginalFilename", original),
        ("ProductName", "Quall Monitor"),
        ("ProductVersion", versao),
    ].iter().map(|(chave, valor)| {
        let utf16 = utf16(valor);
        bloco_de_versao(chave, 1, (utf16.len() / 2) as u16, &utf16, &[])
    }).collect();
    // 0409: inglês dos EUA; 04B0: Unicode. Os nomes de produto não dependem do idioma do app.
    let tabela = bloco_de_versao("040904B0", 1, 0, &[], &textos);
    let strings = bloco_de_versao("StringFileInfo", 1, 0, &[], &[tabela]);
    let traducao = bloco_de_versao("Translation", 0, 4, &[0x09, 0x04, 0xB0, 0x04], &[]);
    let variaveis = bloco_de_versao("VarFileInfo", 1, 0, &[], &[traducao]);
    bloco_de_versao("VS_VERSION_INFO", 0, fixos.len() as u16, &fixos, &[strings, variaveis])
}

fn utf16(texto: &str) -> Vec<u8> {
    texto.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

fn bloco_de_versao(chave: &str, tipo: u16, tamanho_do_valor: u16, valor: &[u8], filhos: &[Vec<u8>]) -> Vec<u8> {
    let mut dados = Vec::new();
    dados.extend_from_slice(&0u16.to_le_bytes()); // wLength, preenchido no fim
    dados.extend_from_slice(&tamanho_do_valor.to_le_bytes());
    dados.extend_from_slice(&tipo.to_le_bytes());
    dados.extend_from_slice(&utf16(chave));
    while dados.len() % 4 != 0 { dados.push(0); }
    dados.extend_from_slice(valor);
    for filho in filhos {
        while dados.len() % 4 != 0 { dados.push(0); }
        dados.extend_from_slice(filho);
    }
    let tamanho = u16::try_from(dados.len()).expect("recurso de versão grande demais");
    dados[..2].copy_from_slice(&tamanho.to_le_bytes());
    dados
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

/// O `.res` com o ícone: o recurso nulo do começo, um `RT_ICON` por imagem do ICO (ids 1..n) e o
/// `RT_GROUP_ICON` 1 que aponta para eles. `None` se o ICO não for um ICO.
fn res_do_icone(ico: &[u8]) -> Option<Vec<u8>> {
    let u16_em = |i: usize| -> Option<u16> { Some(u16::from_le_bytes(ico.get(i..i + 2)?.try_into().ok()?)) };
    let u32_em = |i: usize| -> Option<u32> { Some(u32::from_le_bytes(ico.get(i..i + 4)?.try_into().ok()?)) };
    if u16_em(0)? != 0 || u16_em(2)? != 1 {
        return None;
    }
    let n = u16_em(4)? as usize;
    let mut res = Vec::new();
    recurso(&mut res, 0, 0, 0, &[]); // o recurso nulo que abre todo .res de 32 bits
    let mut grupo = Vec::new();
    grupo.extend_from_slice(&0u16.to_le_bytes());
    grupo.extend_from_slice(&1u16.to_le_bytes());
    grupo.extend_from_slice(&(n as u16).to_le_bytes());
    for k in 0..n {
        let e = 6 + 16 * k;
        let (largura, altura, cores, planos, bits) = (*ico.get(e)?, *ico.get(e + 1)?, *ico.get(e + 2)?, u16_em(e + 4)?, u16_em(e + 6)?);
        let (tamanho, deslocamento) = (u32_em(e + 8)? as usize, u32_em(e + 12)? as usize);
        let dados = ico.get(deslocamento..deslocamento + tamanho)?;
        let id = (k + 1) as u16;
        recurso(&mut res, RT_ICON, id, 0x1010, dados);
        // GRPICONDIRENTRY: a mesma entrada do ICO, com o id do recurso no lugar do deslocamento.
        grupo.extend_from_slice(&[largura, altura, cores, 0]);
        grupo.extend_from_slice(&planos.to_le_bytes());
        grupo.extend_from_slice(&bits.to_le_bytes());
        grupo.extend_from_slice(&(tamanho as u32).to_le_bytes());
        grupo.extend_from_slice(&id.to_le_bytes());
    }
    recurso(&mut res, RT_GROUP_ICON, 1, 0x1030, &grupo);
    Some(res)
}

/// Um recurso com tipo e nome numéricos: cabeçalho de 32 bytes (tamanho dos dados, tamanho do
/// cabeçalho, 0xFFFF+tipo, 0xFFFF+nome, versão dos dados, bandeiras, idioma neutro, versão,
/// características) e os dados, alinhados a 4.
fn recurso(res: &mut Vec<u8>, tipo: u16, nome: u16, bandeiras: u16, dados: &[u8]) {
    res.extend_from_slice(&(dados.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&tipo.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&nome.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(&bandeiras.to_le_bytes());
    res.extend_from_slice(&0u16.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(dados);
    while res.len() % 4 != 0 {
        res.push(0);
    }
}
