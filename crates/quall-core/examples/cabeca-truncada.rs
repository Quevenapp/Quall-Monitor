//! **O que o receptor entrega quando o enlace corta a rajada no pacote N.**
//!
//!     cargo run -p quall-core --example cabeca-truncada -- entrada.h264 saida.h264 40
//!
//! Fecha o laço entre as duas metades do conserto de `docs/pintar-a-cabeca.md`, que até aqui
//! estavam aferidas cada uma do seu lado e não uma contra a outra:
//!
//! - `crates/quall-core/src/rtp.rs` sabe produzir a **cabeça** de um quadro truncado, e nove
//!   testes fixam os bytes exatos que ela tem;
//! - a `SondaDeFatiasActivity` sabe que o `MediaCodec` **aceita** uma unidade cortada no fim de
//!   uma fatia, em 84 cortes.
//!
//! O que faltava é o arnês que pega a saída de uma e põe na entrada da outra. É este programa: ele
//! pacotiza a unidade de acesso como a libdatachannel pacotiza, **joga fora tudo a partir do
//! pacote N** — inclusive o da marca, que é o que de fato acontece no ar —, passa o resto pelo
//! depacotizador de produto com a entrega de cabeça ligada, e grava o que sair. O arquivo que ele
//! grava vai direto para o aparelho:
//!
//!     apps/android/tools/fatias-no-decodificador.py --todos --h264 saida.h264 --cortes 1
//!
//! # Por que o corte é em PACOTE, e não em fatia
//!
//! Porque é assim que o defeito acontece. `docs/idr-que-sobrevive.md` mediu a fila do enlace de
//! 2,4 GHz saturando em ~50 fragmentos IP e cortando a rajada em pontos **quantizados** —
//! {35, 40, 50, 52} —, sem nenhuma relação com onde as fatias terminam. Cortar em fatia, como a
//! sonda faz, responde "o decodificador aceita?"; cortar em pacote responde "**o que sobra**
//! quando o enlace corta?", que é a pergunta de produto. Quantas fatias inteiras sobrevivem a um
//! corte no pacote 40 é uma propriedade do fluxo, não uma escolha nossa — e este programa a
//! imprime.
//!
//! # Nada vai ao ar, e nenhum quadro é aberto
//!
//! Não há socket: os pacotes são construídos em memória e consumidos em memória. Nenhum pixel é
//! decodificado aqui — o programa move bytes de NAL e conta.

use quall_core::rtp::Depacotizador;
use std::fs;
use std::path::PathBuf;

/// Maior fragmento FU-A — o mesmo `MAX_FRAGMENTO` de `track.rs`.
const MAX_FRAGMENTO: usize = 1188;

/// Um NAL dentro do Annex-B: a faixa **sem** o start code.
fn nals(b: &[u8]) -> Vec<(usize, usize)> {
    let mut inicios = Vec::new();
    let mut i = 0usize;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            inicios.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut saida = Vec::with_capacity(inicios.len());
    for (k, &ini) in inicios.iter().enumerate() {
        let mut fim = if k + 1 < inicios.len() {
            inicios[k + 1] - 3
        } else {
            b.len()
        };
        while fim > ini && b[fim - 1] == 0 {
            fim -= 1;
        }
        saida.push((ini, fim));
    }
    saida
}

/// `first_mb_in_slice`, o primeiro Exp-Golomb do cabeçalho de fatia.
fn primeiro_mb(b: &[u8], cabecalho: usize) -> Option<u32> {
    let mut bit = 0usize;
    let mut u1 = || {
        let idx = cabecalho + 1 + (bit >> 3);
        let v = if idx < b.len() {
            (b[idx] >> (7 - (bit & 7))) & 1
        } else {
            0
        };
        bit += 1;
        u32::from(v)
    };
    let mut zeros = 0u32;
    while u1() == 0 && zeros < 32 {
        zeros += 1;
    }
    if zeros >= 32 {
        return None;
    }
    let mut resto = 0u32;
    for _ in 0..zeros {
        resto = (resto << 1) | u1();
    }
    Some((1u32 << zeros) - 1 + resto)
}

/// Monta um pacote RTP com carga já pronta.
fn rtp(sequencia: u16, carimbo: u32, marca: bool, carga: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + carga.len());
    p.push(0x80);
    p.push(if marca { 96 | 0x80 } else { 96 });
    p.extend_from_slice(&sequencia.to_be_bytes());
    p.extend_from_slice(&carimbo.to_be_bytes());
    p.extend_from_slice(&0x5155_4131u32.to_be_bytes()); // SSRC
    p.extend_from_slice(carga);
    p
}

/// Pacotiza uma unidade de acesso como `H264RtpPacketizer::fragment` da libdatachannel: NAL única
/// quando cabe, FU-A quando não — e o desconto de dois bytes do cabeçalho **depois** de repartir,
/// que é o que faz a conta não ser `ceil(bytes / 1188)`.
fn pacotizar(au: &[u8], carimbo: u32, seq0: u16) -> Vec<Vec<u8>> {
    let mut saida = Vec::new();
    let mut seq = seq0;
    for (ini, fim) in nals(au) {
        let nal = &au[ini..fim];
        if nal.len() <= MAX_FRAGMENTO {
            saida.push(rtp(seq, carimbo, false, nal));
            seq = seq.wrapping_add(1);
            continue;
        }
        let n = nal.len().div_ceil(MAX_FRAGMENTO);
        let m = nal.len().div_ceil(n).saturating_sub(2).max(1);
        let indicador = (nal[0] & 0b1110_0000) | 28; // FU-A
        let tipo = nal[0] & 0x1f;
        let corpo = &nal[1..];
        let mut i = 0usize;
        while i < corpo.len() {
            let ate = (i + m).min(corpo.len());
            let comeco = i == 0;
            let fim_do_nal = ate == corpo.len();
            let mut carga = Vec::with_capacity(2 + ate - i);
            carga.push(indicador);
            carga.push(
                (if comeco { 0b1000_0000 } else { 0 }) | (if fim_do_nal { 0b0100_0000 } else { 0 }) | tipo,
            );
            carga.extend_from_slice(&corpo[i..ate]);
            saida.push(rtp(seq, carimbo, false, &carga));
            seq = seq.wrapping_add(1);
            i = ate;
        }
    }
    // A marca vai no último pacote da unidade. É ela que morre no truncamento de cauda.
    if let Some(ultimo) = saida.last_mut() {
        ultimo[1] |= 0x80;
    }
    saida
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "uso: cargo run -p quall-core --example cabeca-truncada -- \
             <entrada.h264> <saida.h264> [corte_em_pacotes=40]"
        );
        std::process::exit(2);
    }
    let entrada = PathBuf::from(&args[1]);
    let saida = PathBuf::from(&args[2]);
    let corte: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(40);

    let bytes = fs::read(&entrada).unwrap_or_else(|e| {
        eprintln!("!! não consegui ler {}: {e}", entrada.display());
        std::process::exit(1);
    });

    // A unidade de acesso do IDR: do primeiro NAL até a última fatia antes de a próxima imagem
    // começar (`first_mb_in_slice == 0` de novo).
    let lista = nals(&bytes);
    let mut inicio_da_au = 0usize;
    let mut fim_da_au = bytes.len();
    let mut ja_viu_fatia = false;
    let mut tem_idr = false;
    for &(ini, _fim) in &lista {
        let tipo = bytes[ini] & 0x1f;
        if (1..=5).contains(&tipo) {
            let primeira = primeiro_mb(&bytes, ini) == Some(0);
            if ja_viu_fatia && primeira {
                fim_da_au = ini - 3;
                break;
            }
            ja_viu_fatia = true;
            if tipo == 5 {
                tem_idr = true;
            }
        }
    }
    if !tem_idr {
        eprintln!("!! a primeira unidade de acesso não tem IDR");
        std::process::exit(1);
    }
    while inicio_da_au + 3 < bytes.len()
        && !(bytes[inicio_da_au] == 0 && bytes[inicio_da_au + 1] == 0 && bytes[inicio_da_au + 2] == 1)
    {
        inicio_da_au += 1;
    }
    let au = &bytes[inicio_da_au..fim_da_au];

    let pacotes = pacotizar(au, 3600, 1);
    let fatias_inteiras = lista
        .iter()
        .filter(|&&(ini, _)| ini < fim_da_au && (1..=5).contains(&(bytes[ini] & 0x1f)))
        .count();
    println!("== {} — unidade de acesso do IDR", entrada.display());
    println!(
        "   {} B · {} fatias · {} pacotes RTP",
        au.len(),
        fatias_inteiras,
        pacotes.len()
    );

    if corte >= pacotes.len() {
        println!(
            "   !! corte em {corte} pacotes não trunca nada: a unidade tem {}",
            pacotes.len()
        );
    }

    let mut d = Depacotizador::new();
    d.definir_entrega_de_cabeca(true);
    let mut cabeca: Vec<u8> = Vec::new();
    for p in pacotes.iter().take(corte) {
        let _ = d.aceitar(p, |q| {
            // Um quadro **inteiro** aqui significaria que o corte não cortou nada.
            cabeca = q.annexb.to_vec();
        });
    }
    // O primeiro pacote do quadro seguinte é o que denuncia a morte do anterior: o pacote da marca
    // morreu com a cauda, e sem esta linha o depacotizador continuaria esperando por ele. É
    // exatamente a sequência que o ar produz.
    let seguinte = rtp(
        (corte as u16).wrapping_add(20),
        7200,
        true,
        &[0x41, 0x99, 0x00],
    );
    let _ = d.aceitar(&seguinte, |q| {
        cabeca = q.annexb.to_vec();
    });

    if d.cabecas_entregues() == 0 {
        println!("   >>> NENHUMA CABEÇA ENTREGUE — o corte não deixou fatia completa nenhuma");
        std::process::exit(1);
    }

    let fatias_da_cabeca = d.fatias_da_ultima_cabeca();
    println!(
        "   corte em {corte} pacotes -> cabeça de {} B com {} de {} fatias \
         ({} % da imagem em fatias)",
        cabeca.len(),
        fatias_da_cabeca,
        fatias_inteiras,
        fatias_da_cabeca as usize * 100 / fatias_inteiras.max(1)
    );
    println!(
        "   contadores: cabecas_entregues={} idrs_quebrados={} idrs_prontos={} quadros_descartados={}",
        d.cabecas_entregues(),
        d.idrs_quebrados(),
        d.idrs_prontos(),
        d.quadros_descartados()
    );

    fs::write(&saida, &cabeca).unwrap_or_else(|e| {
        eprintln!("!! não consegui gravar {}: {e}", saida.display());
        std::process::exit(1);
    });
    println!("   gravado em {}", saida.display());
    println!();
    println!("   confira o artefato e leve-o ao aparelho:");
    println!("     tools/fatias.py {}", saida.display());
    println!(
        "     apps/android/tools/fatias-no-decodificador.py --todos --h264 {} --cortes {}",
        saida.display(),
        fatias_da_cabeca
    );
}
