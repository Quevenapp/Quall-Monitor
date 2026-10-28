//! **O QR do computador, na tela do Exibir** (`docs/conexao.md` §10): o Windows receptor mostra um
//! QR que o celular na mão lê; o celular chama, e o Windows conecta nele como convidado de sempre.
//!
//! Este módulo é a metade sem Windows e sem rede: a matriz do QR (pelo crate `qrcode`), o desenho
//! dela em peças de `estilo.rs`, e a frase de cada fase. Quem cria o convite, vigia a fase e conecta
//! é o `receptor.rs`; quem compõe a tela é o `modelo_da_janela.rs`.
//!
//! # Por que o crate `qrcode`, e sem features
//!
//! Nenhum gerador de QR estava no `Cargo.lock` (nem no da raiz). O `qrcode` 0.14 com
//! `default-features = false` é Rust puro, sem dependência nenhuma (a feature padrão `image` puxaria
//! o crate `image` e meia dúzia de codecs, e o desenho aqui é nosso: retângulos do Direct2D). Licença
//! MIT OR Apache-2.0, a mesma do resto das dependências.
//!
//! # O segredo
//!
//! O texto do QR leva o segredo `S` (§10.2). Ele nunca vai ao diário: este módulo recebe o texto, devolve
//! só a matriz, e quem o chama registra no máximo os endereços e a porta.

use std::sync::Arc;

use crate::estilo::{caixa, Cor, Item, Ret};
use crate::idioma::{t, tf};

/// **A matriz de um QR**: `lado × lado` módulos, linha a linha, `true` = escuro. Sem a zona quieta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matriz {
    pub lado: usize,
    pub modulos: Vec<bool>,
}

impl Matriz {
    pub fn escuro(&self, x: usize, y: usize) -> bool {
        x < self.lado && y < self.lado && self.modulos[y * self.lado + x]
    }
}

/// O QR do texto, em nível M (§10.9: "nível M basta"). `None` se o texto não coube em QR nenhum —
/// não acontece com os ~150 caracteres do convite, e a tela diz "não deu para gerar o QR".
pub fn matriz(texto: &str) -> Option<Matriz> {
    let qr = qrcode::QrCode::with_error_correction_level(texto.as_bytes(), qrcode::EcLevel::M).ok()?;
    let lado = qr.width();
    let modulos = qr.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
    Some(Matriz { lado, modulos })
}

/// Quantos módulos de zona quieta em volta: os 4 da norma (o OBS usa os mesmos).
pub const ZONA_QUIETA: usize = 4;

/// **O desenho do QR** no quadrado `r`: o fundo branco (com a zona quieta) e, em cada linha, um
/// retângulo preto por trecho de módulos escuros seguidos. Cada trecho desce um pouco sobre a linha de
/// baixo, para o antisserrilhado do Direct2D não deixar fios claros entre as linhas quando o módulo
/// não cai em pixel inteiro.
pub fn desenho(r: Ret, m: &Matriz) -> Vec<Item> {
    let lado = r.l.min(r.a);
    let total = (m.lado + 2 * ZONA_QUIETA) as f32;
    let modulo = lado / total;
    let x0 = r.x + (r.l - lado) / 2.0;
    let y0 = r.y + (r.a - lado) / 2.0;
    let mut itens = vec![caixa(Ret::new(x0, y0, lado, lado), 6.0, Cor::rgb(0xFFFFFF))];
    let preto = Cor::rgb(0x000000);
    let sobra = (modulo * 0.15).min(0.5);
    for y in 0..m.lado {
        let mut x = 0;
        while x < m.lado {
            if !m.escuro(x, y) {
                x += 1;
                continue;
            }
            let comeco = x;
            while x < m.lado && m.escuro(x, y) {
                x += 1;
            }
            let ultima = y + 1 == m.lado;
            itens.push(caixa(
                Ret::new(
                    x0 + (ZONA_QUIETA + comeco) as f32 * modulo,
                    y0 + (ZONA_QUIETA + y) as f32 * modulo,
                    (x - comeco) as f32 * modulo,
                    modulo + if ultima { 0.0 } else { sobra },
                ),
                0.0,
                preto,
            ));
        }
    }
    itens
}

/// Em que pé está o QR do Exibir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaseDoQr {
    /// O convite está nascendo.
    Preparando,
    /// O QR está na tela, valendo por mais `segundos`.
    Esperando { segundos: u32 },
    /// Um celular leu e chamou: o QR sai, e a conexão começa.
    Chamado { nome: String },
    /// O convite venceu: outro está nascendo.
    Renovando,
    /// Este computador não está em rede local nenhuma (`NoRoute`): não há QR que sirva.
    SemRede,
    /// Não deu para abrir a escuta do chamado (a porta, o sistema).
    Falhou,
}

/// **O QR na tela**: a matriz (só nas fases em que ele vale) e a fase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrNaTela {
    pub matriz: Option<Arc<Matriz>>,
    pub fase: FaseDoQr,
}

impl QrNaTela {
    /// O QR é desenhado só esperando: chamado, ele já foi usado; vencido, não vale mais.
    pub fn mostra_o_qr(&self) -> bool {
        matches!(self.fase, FaseDoQr::Esperando { .. }) && self.matriz.is_some()
    }
}

/// "2:41".
pub fn minutos(segundos: u32) -> String {
    format!("{}:{:02}", segundos / 60, segundos % 60)
}

/// **A frase sob o QR**, no idioma de agora. Curta: a coluna tem a largura do QR.
pub fn frase(f: &FaseDoQr) -> String {
    match f {
        FaseDoQr::Preparando => t("Preparando o QR…").into(),
        FaseDoQr::Esperando { segundos } => tf("Vale por {}", &[&minutos(*segundos)]),
        FaseDoQr::Chamado { nome } if nome.is_empty() => t("Um celular chamou…").into(),
        FaseDoQr::Chamado { nome } => tf("{} chamou…", &[nome]),
        FaseDoQr::Renovando => t("Expirou — gerando outro…").into(),
        FaseDoQr::SemRede => t("Sem rede local").into(),
        FaseDoQr::Falhou => t("O QR não abriu").into(),
    }
}

/// **Por que o computador recusou o chamado** (`docs/v3-no-privado.md` §4.1), pelo motivo que vai no
/// fio (`quall_core::qr::MotivoDaRecusa::como_texto`): `expirou`, `usado`, `invalido`, `endereco`. A
/// recusa não é autenticada — é só dica. Quem a ouve é o celular que chamou (as frases são as do
/// iOS e do Android); o Windows mostra o QR e não chama, e a tabela fica aqui para as cinco
/// plataformas dizerem o mesmo. Motivo desconhecido (uma versão futura) é o `invalido`.
pub fn frase_da_recusa(motivo: &str) -> &'static str {
    match motivo {
        "expirou" => t("O código do computador expirou. Peça um QR novo na tela dele."),
        "usado" => t("Este QR já foi usado. Peça um QR novo na tela do computador."),
        "endereco" => t("O computador vê este celular por outro endereço (VPN ou rede diferente). Desligue a VPN ou use a mesma rede."),
        _ => t("O computador recusou o chamado. Peça um QR novo na tela dele."),
    }
}

/// A frase no lugar do QR, quando ele não é desenhado.
pub fn no_lugar_do_qr(f: &FaseDoQr) -> String {
    match f {
        FaseDoQr::Chamado { .. } => t("Conectando ao celular que leu o QR.").into(),
        FaseDoQr::SemRede => t("Ligue o Wi-Fi ou o cabo na mesma rede do celular.").into(),
        FaseDoQr::Falhou => t("A porta do chamado não abriu. O diário diz por quê.").into(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// O texto do QR é `v=2` desde o v3 (P5, `docs/v3-no-privado.md` §4.1).
    const CONVITE: &str = "quall://chamar?v=2&a=192.168.15.9,10.0.0.4&p=7874&i=win-3F2504E0-4F89-11D3-9A0C-0305E82C3301&n=DELL-G3&s=00112233445566778899aabbccddeeff";

    #[test]
    fn o_convite_vira_qr_de_tamanho_legivel() {
        let m = matriz(CONVITE).expect("cabe");
        // ~150 bytes em nível M: versão 7 ou 8 (45 ou 49 módulos), §10.2.
        assert!((41..=53).contains(&m.lado), "lado {}", m.lado);
        assert_eq!(m.modulos.len(), m.lado * m.lado);
        // Os três padrões de posição: os cantos de cima à esquerda, de cima à direita e de baixo.
        for (cx, cy) in [(0, 0), (m.lado - 7, 0), (0, m.lado - 7)] {
            assert!(m.escuro(cx, cy) && m.escuro(cx + 6, cy + 6) && !m.escuro(cx + 1, cy + 1) && m.escuro(cx + 3, cy + 3));
        }
        assert!(!m.escuro(m.lado, 0), "fora da matriz é claro");
    }

    #[test]
    fn o_desenho_cabe_no_quadrado_e_cobre_os_escuros() {
        let m = matriz(CONVITE).unwrap();
        let r = Ret::new(100.0, 50.0, 172.0, 172.0);
        let itens = desenho(r, &m);
        let mut area_preta = 0.0f32;
        for (i, it) in itens.iter().enumerate() {
            let Item::Caixa { ret, fundo, .. } = it else { panic!("só caixas") };
            assert!(ret.x >= r.x - 0.01 && ret.y >= r.y - 0.01 && ret.direita() <= r.direita() + 0.01 && ret.baixo() <= r.baixo() + 0.01);
            if i == 0 {
                assert_eq!(*fundo, Cor::rgb(0xFFFFFF), "o fundo branco primeiro");
            } else {
                area_preta += ret.l;
            }
        }
        let modulo = 172.0 / (m.lado + 2 * ZONA_QUIETA) as f32;
        let escuros = m.modulos.iter().filter(|e| **e).count() as f32;
        assert!((area_preta - escuros * modulo).abs() < 0.5, "cada módulo escuro desenhado uma vez");
        assert!(itens.len() < m.lado * m.lado / 2, "um retângulo por trecho, não por módulo");
    }

    #[test]
    fn as_frases() {
        assert_eq!(minutos(161), "2:41");
        assert_eq!(minutos(5), "0:05");
        assert_eq!(frase(&FaseDoQr::Esperando { segundos: 179 }), "Vale por 2:59");
        assert_eq!(frase(&FaseDoQr::Chamado { nome: "Galaxy A07".into() }), "Galaxy A07 chamou…");
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert_eq!(frase(&FaseDoQr::Esperando { segundos: 179 }), "Valid for 2:59");
            assert_eq!(frase(&FaseDoQr::Renovando), "Expired — making another…");
        });
        let q = QrNaTela { matriz: matriz(CONVITE).map(Arc::new), fase: FaseDoQr::Esperando { segundos: 10 } };
        assert!(q.mostra_o_qr());
        assert!(!QrNaTela { fase: FaseDoQr::Chamado { nome: String::new() }, ..q.clone() }.mostra_o_qr(), "chamado, o QR sai");
        assert!(!QrNaTela { matriz: None, ..q }.mostra_o_qr());
    }

    /// As quatro recusas do chamado, nos dois idiomas; o motivo desconhecido é o `invalido`.
    #[test]
    fn as_frases_da_recusa() {
        assert!(frase_da_recusa("expirou").contains("expirou"));
        assert!(frase_da_recusa("usado").starts_with("Este QR já foi usado"));
        assert!(frase_da_recusa("endereco").contains("VPN"));
        assert_eq!(frase_da_recusa("invalido"), frase_da_recusa("algo-novo"));
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert!(frase_da_recusa("usado").starts_with("This QR code was already used"));
            assert!(frase_da_recusa("endereco").contains("VPN"));
        });
    }

    /// O núcleo lê o `v=2` do exemplo (e recusa o `v=1` de 12/10): o texto das telas de exemplo é o
    /// de verdade.
    #[cfg(feature = "core")]
    #[test]
    fn o_convite_de_exemplo_e_o_v2_do_nucleo() {
        assert_eq!(quall_core::qr::VERSAO, 2);
        let lido = quall_core::qr::ConteudoDoQr::ler(CONVITE).expect("v=2");
        assert_eq!(lido.porta, 7874);
        assert!(quall_core::qr::ConteudoDoQr::ler(&CONVITE.replace("v=2", "v=1")).is_err(), "v=1 é de antes do v3");
    }
}
