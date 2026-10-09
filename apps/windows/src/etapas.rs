//! **Onde o laço do receptor gasta o tempo de um quadro**, etapa por etapa.
//!
//! # Por que existe: um teto atribuído a quem não era
//!
//! `docs/bancada.md` §8.63 e §8.68 escreveram que *"o teto de ~39 a 51 fps a 1080p é do
//! decodificador de software"*. As duas metades da frase já estavam desmentidas por medidas que o
//! repositório tinha:
//!
//! - **o app não decodifica em software**: o `README.md` desta pasta ("Números medidos", item 3)
//!   mediu os motores de vídeo da GPU trabalhando para o PID do receptor — `engtype_videodecode` —
//!   com 3,4 % de um núcleo de CPU. O MFT da Microsoft com `IMFDXGIDeviceManager` **é** o DXVA;
//! - **e o software deste Dell não para em 39**: o plugin do OBS decodifica em software de
//!   propósito, e na mesma noite, com a mesma câmera do S24, mostrava *"1920x1080 a 59 fps · decode
//!   2.5 ms"*.
//!
//! O que sobra é o laço: **uma thread só** faz, em série, a entrada no decoder, a saída dele, a
//! apresentação na janela e a conversão para a câmera virtual. O custo de um quadro é a soma das
//! parcelas, e a soma nunca foi medida por parcela — por isso o teto foi atribuído à parcela que
//! parecia mais cara.
//!
//! # A forma é a de `fluidez`, de propósito
//!
//! Quatro números, nunca só a média, e o mesmo teto de amostras dito em voz alta quando estoura.
//! `fila` não é tempo: é quantos quadros esperavam na fila da rede quando o laço tirou o próximo —
//! **a medida direta de "o laço está dando conta"**. Fila perto de zero é laço folgado; fila no
//! teto (`FILA_DE_QUADROS`) é laço afogado, e é aí que o transbordo começa.

use std::time::Instant;

/// Teto de amostras por etapa. A 60 fps são ~5,5 minutos de sessão; passado isso a distribuição
/// para de crescer em vez de uma sessão longa comer memória. O mesmo critério de
/// `fluidez::MAXIMO_DE_AMOSTRAS`, com o dobro porque aqui a taxa nominal é o dobro.
const MAXIMO_DE_AMOSTRAS: usize = 20_000;

/// A distribuição de uma grandeza por quadro — em µs quando o rótulo termina em `_us`.
pub struct Etapa {
    rotulo: &'static str,
    amostras: Vec<u32>,
    descartadas: u64,
}

impl Etapa {
    pub fn nova(rotulo: &'static str) -> Self {
        Etapa { rotulo, amostras: Vec::new(), descartadas: 0 }
    }

    pub fn anotar(&mut self, valor: u64) {
        if self.amostras.len() < MAXIMO_DE_AMOSTRAS {
            self.amostras.push(valor.min(u32::MAX as u64) as u32);
        } else {
            self.descartadas += 1;
        }
    }

    /// Anota o decorrido desde `inicio`, em µs.
    pub fn desde(&mut self, inicio: Instant) {
        self.anotar(inicio.elapsed().as_micros() as u64);
    }

    pub fn vazia(&self) -> bool {
        self.amostras.is_empty()
    }

    /// `(n, p50, p95, pior)`. `None` quando não houve amostra.
    pub fn percentis(&self) -> Option<(usize, u32, u32, u32)> {
        if self.amostras.is_empty() {
            return None;
        }
        let mut v = self.amostras.clone();
        v.sort_unstable();
        let em = |p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize];
        Some((v.len(), em(0.5), em(0.95), v[v.len() - 1]))
    }

    /// `rotulo=[n=… p50=… p95=… pior=…]` — a forma que a linha da câmera virtual já usava.
    pub fn linha(&self) -> String {
        let sufixo = if self.descartadas > 0 {
            format!(" (+{} além do teto)", self.descartadas)
        } else {
            String::new()
        };
        match self.percentis() {
            None => format!("{}=[n=0]", self.rotulo),
            Some((n, p50, p95, pior)) => {
                format!("{}=[n={n} p50={p50} p95={p95} pior={pior}]{sufixo}", self.rotulo)
            }
        }
    }
}

#[cfg(test)]
mod testes {
    use super::Etapa;

    #[test]
    fn sem_amostra_diz_zero_e_nao_inventa_percentil() {
        let e = Etapa::nova("tela_us");
        assert_eq!(e.linha(), "tela_us=[n=0]");
        assert!(e.percentis().is_none());
    }

    /// O pior aparece mesmo quando é um só — é nele que o transbordo começa.
    #[test]
    fn o_pior_nao_se_dilui_no_centro() {
        let mut e = Etapa::nova("saida_us");
        for _ in 0..99 {
            e.anotar(2_600);
        }
        e.anotar(28_750);
        let (n, p50, p95, pior) = e.percentis().unwrap();
        assert_eq!((n, p50, p95, pior), (100, 2_600, 2_600, 28_750));
    }

    #[test]
    fn passado_o_teto_diz_quantas_ficaram_de_fora() {
        let mut e = Etapa::nova("fila");
        for _ in 0..(super::MAXIMO_DE_AMOSTRAS + 5) {
            e.anotar(1);
        }
        assert!(e.linha().ends_with("(+5 além do teto)"), "{}", e.linha());
    }
}
