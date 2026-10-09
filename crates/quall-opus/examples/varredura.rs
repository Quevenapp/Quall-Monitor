//! Varredura: o que segura o encoder em SILK/híbrido, onde o LBRR existe.
//!
//! Origem sintética (o mesmo tom de quatro notas da sonda). Nenhum microfone é aberto.
use quall_opus::*;

const TAXA: u32 = 48_000;
const N: usize = 960;
const NOTAS: [f64; 4] = [400.0, 500.0, 800.0, 1000.0];

fn tom(i: u64, canais: u8) -> Vec<i16> {
    let nota = NOTAS[((i / 25) as usize) % 4];
    let base = i * N as u64;
    let mut v = Vec::with_capacity(N * usize::from(canais));
    for k in 0..N {
        let n = base + k as u64;
        let f = 2.0 * std::f64::consts::PI * nota * (n as f64) / f64::from(TAXA);
        let a = (f.sin() * 0.5 * f64::from(i16::MAX)) as i16;
        for _ in 0..canais {
            v.push(a);
        }
    }
    v
}

fn corre(perda: u8, sinal: Sinal, quadros: u64) -> (String, usize, f64) {
    let mut e = Codificador::novo(TAXA, 1, Aplicacao::Voz).unwrap();
    e.definir_taxa_de_bits(32_000).unwrap();
    e.definir_fec_embutido(true).unwrap();
    e.definir_perda_esperada(perda).unwrap();
    e.definir_sinal(sinal).unwrap();
    e.definir_dtx(false).unwrap();
    let mut buf = vec![0u8; 4000];
    let (mut lbrr, mut bytes) = (0usize, 0usize);
    let mut modos = std::collections::BTreeMap::new();
    for i in 0..quadros {
        let n = e.codificar(&tom(i, 1), &mut buf).unwrap();
        let p = &buf[..n];
        bytes += n;
        if tem_lbrr(p).unwrap() {
            lbrr += 1;
        }
        *modos
            .entry(format!("{:?}", Toc::do_pacote(p).unwrap().modo()))
            .or_insert(0usize) += 1;
    }
    let m: Vec<String> = modos.iter().map(|(k, v)| format!("{k} {v}")).collect();
    (
        m.join(", "),
        lbrr,
        bytes as f64 * 8.0 / quadros as f64 / 20.0,
    )
}

fn main() {
    let q = 300u64; // 6 s, a mesma corrida da sonda
    for (rotulo, sinal) in [
        ("Automatico", Sinal::Automatico),
        ("Voz (forçado)", Sinal::Voz),
    ] {
        println!("\n### sinal = {rotulo} — mono 32 kbit/s, FEC ligado, {q} quadros (6 s)\n");
        println!(
            "| perda declarada | modo no fio                    | pacotes com LBRR | kbit/s |"
        );
        println!("|---|---|---|---|");
        for perda in [0u8, 1, 2, 3, 5, 8, 10, 15, 20, 30] {
            let (m, l, kb) = corre(perda, sinal, q);
            println!(
                "| {perda:>2}% | {m:30} | {l:>3} de {q} ({:>3.0}%) | {kb:>5.1} |",
                100.0 * l as f64 / q as f64
            );
        }
    }
}
