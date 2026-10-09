//! **Quanto custam 4K30 e 1080p60, e qual transporte comporta cada um.**
//!
//! Existe porque o pedido do usuário — *"rodar em 4k, em 1080p 60fps, podemos usar o cabo usb e a
//! ethernet"* — liga **transporte** a **modo de captura**, e as duas pontas dessa ligação são
//! aritmética que este núcleo já sabe fazer: [`teto::teto_de_taxa_com`] dá os Mbps e
//! [`LimitesDoNivel`] diz em que nível H.264 cada modo cabe. O que faltava era alguém pôr os dois
//! ao lado dos joelhos que a bancada mediu, em vez de discutir de cabeça.
//!
//!     cargo run -p quall-core --example transporte
//!
//! Não abre socket nenhum e não toca em aparelho nenhum.
//!
//! # As três réguas medidas, e de onde cada uma vem
//!
//! - **rádio, 32–47 Mbps agregados** (`docs/bancada.md` §8.10): três emissores descendo para um
//!   Mac no rádio. 21,6 Mbps deu 0–0,015 % e 0 de 115 IDR quebrados; 47 Mbps deu 0,8–1,55 % e 7
//!   quebrados. É agregado por **cliente de rádio**, não por emissor.
//! - **fio, 75,5 Mbps agregados** (§8.13): nove câmeras reais, receptor no fio, 7.863 pac/s,
//!   **0,023 %** de perda e **321 IDR, nenhum quebrado**. É o maior número limpo desta bancada.
//! - **joelho de IDR, 35 a 50 pacotes** (`docs/idr-que-sobrevive.md`, `docs/joelho-da-perda.md`):
//!   abaixo dele o quadro-chave não é truncado; acima, a cauda some inteira. A perda deste enlace
//!   é truncamento de cauda, não perda independente por pacote.
//!
//! # E a que NÃO é régua
//!
//! `perda ~ carga^2,5` (§8.30): A07 a 5 GHz deu expoente 2,8 e A10s a 2,4 GHz deu 2,4. **Dois
//! pontos por aparelho não desenham curva** — servem para excluir o expoente 1, que é o que um
//! canal com erro fixo por pacote prediz. Aqui ela entra só como coluna de ordem de grandeza, e
//! está marcada como tal.

use quall_core::teto::{self, LimitesDoNivel, FPS_MAXIMO};
use quall_core::track::MAX_FRAGMENTO;

/// Bytes do **maior** quadro-chave de regime medido nesta bancada: A07, 1080p, VBR, 6,75 Mbps
/// pedidos — 122.042 B = 102 pacotes (`docs/bancada.md` §8.47).
///
/// **É medida, e a extrapolação por área é que não é.** O IDR de 1080p é o único ponto no fio; os
/// de 4K são `medido x area/area_1080p`, que é a primeira aproximação razoável e não é lei.
const IDR_1080P_BYTES: f64 = 122_042.0;
const AREA_1080P: f64 = 1920.0 * 1080.0;

/// Joelho de truncamento de cauda, em pacotes por quadro.
const JOELHO_IDR: f64 = 50.0;

/// Os dois joelhos agregados, em Mbps.
const JOELHO_RADIO_MBPS: f64 = 32.0;
const FIO_MEDIDO_MBPS: f64 = 75.5;

fn menor_nivel_que_cabe(l: u32, a: u32, fps: u32) -> Option<LimitesDoNivel> {
    let mb = LimitesDoNivel::macroblocos(l, a);
    // Varre a tabela do núcleo em ordem, e devolve o primeiro que cabe nas DUAS contas da norma.
    // Nunca interpola: um nível ausente da tabela simplesmente não é oferecido.
    [10u8, 11, 12, 13, 20, 21, 22, 30, 31, 32, 40, 41, 42, 50, 51, 52]
        .into_iter()
        .filter_map(LimitesDoNivel::do_idc)
        .find(|n| mb <= n.max_fs && mb * fps <= n.max_mbps)
}

fn main() {
    let anunciado = LimitesDoNivel::do_sdp_ou_conservador();
    println!();
    println!(
        "SDP de hoje: {} -> nível {}.{} · MaxFS {} · MaxMBPS {} · MaxBR {} kbps · FPS_MAXIMO {}",
        quall_core::track::PERFIL_H264.split(';').next().unwrap_or("?"),
        anunciado.level_idc / 10,
        anunciado.level_idc % 10,
        anunciado.max_fs,
        anunciado.max_mbps,
        anunciado.max_br_kbps,
        FPS_MAXIMO
    );
    println!();

    let modos: &[(&str, u32, u32, u32)] = &[
        ("720p30 (referência)", 1280, 720, 30),
        ("1080p30 (o de hoje)", 1920, 1080, 30),
        ("1080p60", 1920, 1080, 60),
        ("4K30", 3840, 2160, 30),
        ("4K60", 3840, 2160, 60),
    ];

    println!(
        "{:<22} {:>7} {:>6} {:>8} {:>8} {:>7} {:>7} {:>7}",
        "modo", "mb", "nível", "Mbps", "pac/s", "IDR pac", "n/rádio", "n/fio"
    );
    println!("{}", "-".repeat(82));

    for (nome, l, a, fps) in modos {
        let Some(nivel) = menor_nivel_que_cabe(*l, *a, *fps) else {
            println!("{nome:<22} nenhum nível da tabela comporta");
            continue;
        };
        let mb = LimitesDoNivel::macroblocos(*l, *a);
        let bps = teto::teto_de_taxa_com(*l, *a, *fps, nivel);
        let mbps = f64::from(bps) / 1e6;
        let pac_s = f64::from(bps) / (f64::from(MAX_FRAGMENTO) * 8.0);
        let idr_pac = (IDR_1080P_BYTES * (f64::from(*l) * f64::from(*a)) / AREA_1080P
            / f64::from(MAX_FRAGMENTO))
            .ceil();
        let cabe = if nivel.level_idc <= anunciado.level_idc && *fps <= FPS_MAXIMO {
            format!("{}.{}", nivel.level_idc / 10, nivel.level_idc % 10)
        } else {
            format!("{}.{}!", nivel.level_idc / 10, nivel.level_idc % 10)
        };
        println!(
            "{:<22} {:>7} {:>6} {:>8.1} {:>8.0} {:>7.0} {:>7.1} {:>7.1}",
            nome,
            mb,
            cabe,
            mbps,
            pac_s,
            idr_pac,
            JOELHO_RADIO_MBPS / mbps,
            FIO_MEDIDO_MBPS / mbps
        );
    }

    println!();
    println!("\"nível\" com ! = ACIMA do que o SDP anuncia hoje ou do FPS_MAXIMO: não sai deste");
    println!("         binário sem mudar `PERFIL_H264` e/ou `FPS_MAXIMO`.");
    println!("\"Mbps\"  = teto_de_taxa_com(l, a, fps, nível) — a densidade de 0,145 bit/pixel do");
    println!("         produto, aplicada à taxa de pixels de saída. É o que se PEDE ao encoder.");
    println!("\"pac/s\" = Mbps / {MAX_FRAGMENTO} B de carga FU-A. Confere com a bancada: §8.13 mediu");
    println!("         1694 pac/s para 16,3 Mbps, e esta conta dá 1715 (+1,2 %).");
    println!("\"IDR pac\" = 122.042 B medidos a 1080p (§8.47) escalados por ÁREA. O joelho de");
    println!("         truncamento é {JOELHO_IDR:.0} pacotes — acima dele a cauda some inteira.");
    println!("\"n/rádio\" = quantas fontes cabem sob {JOELHO_RADIO_MBPS:.0} Mbps, o piso do joelho de §8.10");
    println!("         (receptor no rádio). \"n/fio\" = sob os {FIO_MEDIDO_MBPS:.1} Mbps que §8.13 mediu");
    println!("         com receptor no fio, 0,023 % de perda, 321 IDR e nenhum quebrado.");
    println!();
    println!("As duas colunas de fontes são TETOS DE BANDA, não de host: nada aqui mede decode,");
    println!("composição do OBS nem barramento USB. Ver o relatório.");
    println!();
}
