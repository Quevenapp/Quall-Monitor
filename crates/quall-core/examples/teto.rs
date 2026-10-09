//! O que o teto do núcleo faz com cada tela desta bancada — e o que isso custa em pacotes RTP.
//!
//! Existe porque o número que decidiu a frente da tela preta não foi a resolução: foi **quantos
//! pacotes o conjunto de parâmetros passa a ocupar**, e a chance de ele atravessar uma rede que
//! perde (`docs/tela-preta.md` §9.3). Uma tabela de resoluções sem essa coluna esconde
//! exatamente o que importa.
//!
//!     cargo run -p quall-core --example teto
//!
//! Não abre quadro nenhum e não toca em aparelho nenhum: é aritmética sobre o `fmtp` que o
//! próprio núcleo anuncia.

use quall_core::teto::{self, LimitesDoNivel};

/// Maior fragmento FU-A, em bytes — o mesmo `MAX_FRAGMENTO` de `track.rs`.
const CARGA_POR_PACOTE: f64 = 1188.0;

/// A perda medida na foto que abriu a investigação (`docs/tela-preta.md` §1): 156 pacotes
/// perdidos em 1815.
///
/// # A coluna "chance" é um PISO, e ela erra por 60× — medido em 31/08/2026
///
/// A conta `(1 − p)^pacotes` supõe que cada pacote se perca sozinho. **A perda deste enlace não é
/// assim.** `tools/rajada-completa.rs` mediu a fração de quadros que chega inteira, por tamanho, e
/// a forma é outra: 33 de 37 quadros quebrados perderam a **cauda inteira** a partir do pacote
/// ~35, e nenhum perdeu pacote isolado. Com 6,96 % de perda por pacote e 60 pacotes por quadro, a
/// conta abaixo prevê **1,3 %** de quadros inteiros; o enlace entregou **77,5 %**.
///
/// O erro é sempre para o lado pessimista, então a coluna continua servindo como **piso** — "não
/// é pior que isto". Ela não serve para decidir tamanho de quadro. Quem precisa disso usa a curva
/// medida em `docs/idr-que-sobrevive.md`, cujo critério é outro e muito mais simples: quadro de
/// até ~35 pacotes não é truncado.
const PERDA_DA_FOTO: f64 = 0.086;

/// Bytes por quadro-chave, estimados a partir do único ponto medido no fio: o conjunto de
/// parâmetros do Dell a 1920x1080 ocupou **195.495 bytes** (§9.3).
///
/// **Isto é uma extrapolação, e está dita como tal.** A conta assume que o tamanho do IDR
/// acompanha a área — o que é a primeira aproximação razoável e não é uma lei. O número que vale
/// é o de 1080p, que foi medido; os outros são o que se espera, para dar ordem de grandeza ao
/// ganho. Medir os demais exige emitir de cada tela, e está no relatório como não provado.
fn bytes_do_idr(largura: u32, altura: u32) -> f64 {
    let medido_em_1080p = 195_495.0;
    let area_de_1080p = 1920.0 * 1080.0;
    medido_em_1080p * (f64::from(largura) * f64::from(altura)) / area_de_1080p
}

fn main() {
    let limites = LimitesDoNivel::do_sdp_ou_conservador();
    println!();
    // Lê a constante em vez de repetir o número: é a mesma regra que `teto.rs` segue, e é o que
    // impede este exemplo de mentir no dia em que o `fmtp` mudar de novo.
    println!("o que o SDP anuncia: {} -> nível {}.{}, MaxFS {} macroblocos, MaxMBPS {}",
             quall_core::track::PERFIL_H264.split(';').next().unwrap_or("?"),
             limites.level_idc / 10, limites.level_idc % 10, limites.max_fs, limites.max_mbps);
    println!();
    println!("{:<22} {:>14} {:>7} {:>6} {:>9} {:>9}", "emissor", "entrada", "mb", "nível", "pacotes", "chance");
    println!("{}", "-".repeat(74));

    let bancada: &[(&str, u32, u32, u32)] = &[
        ("iOS (iPhone 7)", 750, 1334, 30),
        ("Android (A10s)", 720, 1520, 30),
        ("Windows (Dell)", 1920, 1080, 30),
        ("macOS (MacBook)", 2560, 1664, 60),
    ];

    for (nome, l, a, fps) in bancada {
        for (rotulo, saida) in [
            ("antes", None),
            ("depois", Some(teto::ajustar(*l, *a, *fps))),
        ] {
            let (largura, altura, taxa) = match saida {
                None => (*l, *a, *fps),
                Some(s) => (s.largura, s.altura, s.fps),
            };
            let mb = LimitesDoNivel::macroblocos(largura, altura);
            let cabe = mb <= limites.max_fs && mb * taxa <= limites.max_mbps;
            let pacotes = (bytes_do_idr(largura, altura) / CARGA_POR_PACOTE).ceil();
            // **Piso, e não estimativa.** Ver a nota em `PERDA_DA_FOTO`: medido em 31/08, esta
            // conta erra por 60× para baixo neste enlace.
            let chance = (1.0 - PERDA_DA_FOTO).powf(pacotes) * 100.0;
            println!(
                "{:<22} {:>14} {:>7} {:>6} {:>9} {:>8.2}%",
                format!("{nome} ({rotulo})"),
                format!("{largura}x{altura}@{taxa}"),
                mb,
                if cabe { "ok" } else { "ACIMA" },
                pacotes as u64,
                chance
            );
        }
        println!();
    }

    println!("\"pacotes\" = tamanho estimado do quadro-chave / {CARGA_POR_PACOTE:.0} B de carga FU-A.");
    println!("\"chance\"  = probabilidade de UM conjunto de parâmetros atravessar inteiro a {:.1}%", PERDA_DA_FOTO * 100.0);
    println!("            de perda — a da foto que abriu a investigação. Um pacote perdido");
    println!("            condena o quadro inteiro (rtp.rs), então a conta é 0,914^pacotes.");
    println!("            É um PISO: a perda medida é truncamento de cauda, não perda");
    println!("            independente, e a conta erra por 60x. Ver docs/idr-que-sobrevive.md.");
    println!();
    println!("O único ponto MEDIDO no fio é o Windows antes: 195.495 B = 163 pacotes. Os demais");
    println!("são extrapolação por área, para dar ordem de grandeza — ver o comentário no fonte.");
    println!();
}
