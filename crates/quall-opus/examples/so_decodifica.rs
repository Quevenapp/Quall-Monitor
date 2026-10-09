//! Corpo de prova: um binário que **só decodifica**. Ver `docs/audio.md` §11.
fn main() {
    let mut d = quall_opus::Decodificador::novo(48_000, 1).unwrap();
    let mut pcm = vec![0i16; 960];
    let pacote = [0xfcu8, 0, 0, 0, 0, 0, 0, 0];
    let n = d.decodificar(&pacote, &mut pcm).unwrap_or(0);
    println!("{n} {}", quall_opus::versao());
}
