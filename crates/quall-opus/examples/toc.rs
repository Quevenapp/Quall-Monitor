//! Matriz do TOC: o que decide o modo do Opus, e o que decide o LBRR aparecer.
//!
//! Origem **sintética** em todos os casos — seno puro e um sinal com formantes e envelope
//! silábico, gerados aqui. Nenhum microfone é aberto.
use quall_opus::*;

const TAXA: u32 = 48_000;
const N: usize = 960;

/// Seno de 440 Hz: tonal, e o Opus o classifica como música.
fn seno(i: u64, canais: u8) -> Vec<i16> {
    amostrar(i, canais, |t| {
        (2.0 * std::f64::consts::PI * 440.0 * t).sin() * 0.5
    })
}

/// Um sinal com cara de fala: portadora glotal de 120 Hz, três formantes e envelope silábico de
/// 4 Hz. Não é fala — é o mais perto de fala que dá para gerar sem gravar ninguém.
fn quase_fala(i: u64, canais: u8) -> Vec<i16> {
    amostrar(i, canais, |t| {
        let env = 0.5 * (1.0 + (2.0 * std::f64::consts::PI * 4.0 * t).sin()).min(1.0);
        let f0 = 120.0;
        let mut s = 0.0;
        for (h, (fmt, amp)) in [(700.0, 1.0), (1220.0, 0.5), (2600.0, 0.25)]
            .iter()
            .enumerate()
        {
            let _ = h;
            s += amp * (2.0 * std::f64::consts::PI * fmt * t).sin();
        }
        s += 0.6 * (2.0 * std::f64::consts::PI * f0 * t).sin();
        s * env * 0.25
    })
}

fn amostrar(i: u64, canais: u8, f: impl Fn(f64) -> f64) -> Vec<i16> {
    let base = i * N as u64;
    let mut v = Vec::with_capacity(N * usize::from(canais));
    for k in 0..N {
        let t = (base + k as u64) as f64 / f64::from(TAXA);
        let a = (f(t).clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16;
        for _ in 0..canais {
            v.push(a);
        }
    }
    v
}

struct Caso {
    conteudo: &'static str,
    canais: u8,
    bits: u32,
    app: Aplicacao,
    fec: bool,
    perda: u8,
    sinal: Sinal,
}

fn corre(c: &Caso) {
    let gerar: fn(u64, u8) -> Vec<i16> = if c.conteudo == "seno" {
        seno
    } else {
        quase_fala
    };
    let mut e = Codificador::novo(TAXA, c.canais, c.app).unwrap();
    e.definir_taxa_de_bits(c.bits).unwrap();
    e.definir_fec_embutido(c.fec).unwrap();
    e.definir_perda_esperada(c.perda).unwrap();
    e.definir_sinal(c.sinal).unwrap();
    e.definir_dtx(false).unwrap();

    let mut buf = vec![0u8; 4000];
    let (mut bytes, mut lbrr) = (0usize, 0usize);
    let mut modos = std::collections::BTreeMap::new();
    for i in 0..100u64 {
        let n = e.codificar(&gerar(i, c.canais), &mut buf).unwrap();
        let p = &buf[..n];
        bytes += n;
        if tem_lbrr(p).unwrap() {
            lbrr += 1;
        }
        if i >= 10 {
            let t = Toc::do_pacote(p).unwrap();
            *modos.entry(format!("{:?}", t.modo())).or_insert(0usize) += 1;
        }
    }
    let m: Vec<String> = modos.iter().map(|(k, v)| format!("{k} {v}")).collect();
    println!(
        "| {:9} | {:>3} | {:>6} | {:>3} | {:>4} | {:24} | {:>3}/100 | {:>6.1} |",
        c.conteudo,
        c.canais,
        c.bits / 1000,
        if c.fec { "sim" } else { "não" },
        format!("{}%", c.perda),
        m.join(", "),
        lbrr,
        bytes as f64 * 8.0 / 100.0 / 20.0
    );
}

fn main() {
    println!("libopus: {}\n", versao());
    println!(
        "| conteúdo  | ch | kbit/s | FEC | perd | modo (quadros 10..99)    |    LBRR | kbit/s |"
    );
    println!(
        "|-----------|----|--------|-----|------|--------------------------|---------|--------|"
    );
    for conteudo in ["seno", "fala~"] {
        for (fec, perda) in [(true, 0u8), (true, 10), (false, 0)] {
            corre(&Caso {
                conteudo,
                canais: 1,
                bits: 32_000,
                app: Aplicacao::Voz,
                fec,
                perda,
                sinal: Sinal::Automatico,
            });
        }
        corre(&Caso {
            conteudo,
            canais: 1,
            bits: 32_000,
            app: Aplicacao::Voz,
            fec: true,
            perda: 0,
            sinal: Sinal::Voz,
        });
    }
    println!();
    println!("áudio de sistema (estéreo 128 kbit/s):");
    println!(
        "| conteúdo  | ch | kbit/s | FEC | perd | modo (quadros 10..99)    |    LBRR | kbit/s |"
    );
    println!(
        "|-----------|----|--------|-----|------|--------------------------|---------|--------|"
    );
    for conteudo in ["seno", "fala~"] {
        corre(&Caso {
            conteudo,
            canais: 2,
            bits: 128_000,
            app: Aplicacao::Audio,
            fec: false,
            perda: 0,
            sinal: Sinal::Automatico,
        });
    }
}
