//! Corpo de prova: um binário que codifica **e** decodifica. Ver `docs/audio.md` §11.
fn main() {
    let mut e = quall_opus::Codificador::novo(48_000, 1, quall_opus::Aplicacao::Voz).unwrap();
    e.definir_taxa_de_bits(32_000).unwrap();
    e.definir_fec_embutido(true).unwrap();
    let mut buf = vec![0u8; 4000];
    let n = e.codificar(&vec![0i16; 960], &mut buf).unwrap();

    let mut d = quall_opus::Decodificador::novo(48_000, 1).unwrap();
    let mut pcm = vec![0i16; 960];
    let m = d.decodificar(&buf[..n], &mut pcm).unwrap_or(0);
    println!("{n} {m} {}", quall_opus::versao());
}
