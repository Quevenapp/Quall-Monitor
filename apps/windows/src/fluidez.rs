//! **A distribuição dos intervalos entre apresentações** — o número que faltava para "sem fluidez"
//! deixar de ser impressão.
//!
//! # Por que a média não serve, e por que ela existia
//!
//! `Contadores::latencia_media_ms` já media fila→tela e dava **6,5 ms** numa corrida em que o
//! usuário, olhando para a tela, disse que faltava fluidez comparada à do aparelho de origem. A
//! média estava certa e não respondia a pergunta: o pior caso da mesma corrida era **226 ms**, e é
//! nele que a pessoa vê a imagem parar. Média não vê tranco — um segundo com 29 quadros pontuais e
//! um buraco de 200 ms tem a mesma média de um segundo regular.
//!
//! O que se vê é o **intervalo entre um quadro e o seguinte na tela**, e o que descreve isso é a
//! distribuição dele, não o centro.
//!
//! # O intervalo é entre **apresentações**, e isso é escolha
//!
//! Não entre chegadas, não entre decodificações: entre os instantes em que um quadro de fato foi
//! para o vidro. É o único ponto do caminho que corresponde ao que o olho recebe, e é por isso que
//! ele mede também o custo das políticas desta casca — a porta que segura o quadro condenado
//! aparece aqui como intervalo maior, que é exatamente o que ela custa e o que precisava ficar
//! visível. Medir chegadas esconderia a porta.
//!
//! # `trancos` é convenção de comparação, não afirmação perceptual
//!
//! A 30 fps o orçamento é 33 ms. `trancos` conta os intervalos acima de [`TRANCO_MS`] — três
//! tempos de quadro. **Não** se está afirmando que 100 ms é o limiar em que uma pessoa percebe; o
//! que se afirma é que duas corridas com o mesmo emissor e a mesma origem podem ser comparadas por
//! esse número. Quem quiser outro corte tem os percentis ao lado.

use std::time::Instant;

/// O corte de `trancos`: três tempos de quadro a 30 fps. Ver a nota do módulo — é convenção de
/// comparação, e a distribuição completa sai junto para quem quiser outro corte.
pub const TRANCO_MS: u64 = 100;

/// Teto de amostras guardadas. A 30 fps são ~5 minutos de sessão; passado isso a distribuição
/// para de crescer em vez de a sessão longa comer memória. Mesmo teto de `cadeia::Condenacao`.
const MAXIMO_DE_AMOSTRAS: usize = 10_000;

/// Os intervalos entre apresentações de uma sessão.
pub struct Fluidez {
    anterior: Option<Instant>,
    intervalos_us: Vec<u64>,
    /// Quantas amostras foram descartadas por teto — dito em voz alta em vez de fingir que o `n`
    /// é a sessão inteira.
    descartadas: u64,
}

impl Fluidez {
    pub fn nova() -> Self {
        Fluidez { anterior: None, intervalos_us: Vec::new(), descartadas: 0 }
    }

    /// Marca que um quadro foi para a tela agora.
    ///
    /// A primeira chamada **só ancora**: não existe intervalo antes do primeiro quadro, e contar o
    /// tempo desde a abertura da sessão como se fosse um intervalo poria a subida do ICE dentro da
    /// distribuição da imagem.
    pub fn apresentou(&mut self, agora: Instant) {
        if let Some(antes) = self.anterior {
            let us = agora.saturating_duration_since(antes).as_micros() as u64;
            if self.intervalos_us.len() < MAXIMO_DE_AMOSTRAS {
                self.intervalos_us.push(us);
            } else {
                self.descartadas += 1;
            }
        }
        self.anterior = Some(agora);
    }

    /// Quantos intervalos passaram de [`TRANCO_MS`].
    pub fn trancos(&self) -> u64 {
        self.intervalos_us.iter().filter(|us| **us > TRANCO_MS * 1000).count() as u64
    }

    /// `[n p50 p95 max]` em milissegundos, mais `trancos`. Mesma forma de `sem_referencia_ms`, de
    /// propósito: quatro números e não um, porque este repositório já pagou por relatório que
    /// mostrava só o centro.
    pub fn linha(&self) -> String {
        let sufixo = if self.descartadas > 0 {
            format!(" (+{} além do teto)", self.descartadas)
        } else {
            String::new()
        };
        if self.intervalos_us.is_empty() {
            return format!("fluidez_ms=[n=0 p50=0 p95=0 max=0] trancos=0{sufixo}");
        }
        let mut v = self.intervalos_us.clone();
        v.sort_unstable();
        let p = |q: f64| -> f64 {
            let i = ((q * (v.len() - 1) as f64) as usize).min(v.len() - 1);
            v[i] as f64 / 1000.0
        };
        format!(
            "fluidez_ms=[n={} p50={:.0} p95={:.0} max={:.0}] trancos={}{sufixo}",
            v.len(),
            p(0.50),
            p(0.95),
            v[v.len() - 1] as f64 / 1000.0,
            self.trancos(),
        )
    }
}

#[cfg(test)]
mod testes {
    use super::{Fluidez, TRANCO_MS};
    use std::time::{Duration, Instant};

    fn em(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// **A primeira chamada só ancora.** Não existe intervalo antes do primeiro quadro, e contar o
    /// tempo desde a abertura da sessão poria a subida do ICE dentro da distribuição da imagem.
    #[test]
    fn a_primeira_apresentacao_so_ancora() {
        let mut f = Fluidez::nova();
        f.apresentou(Instant::now());
        assert!(f.linha().contains("n=0"), "{}", f.linha());
    }

    /// Dois quadros, um intervalo — e ele é o decorrido real.
    #[test]
    fn dois_quadros_dao_um_intervalo() {
        let t = Instant::now();
        let mut f = Fluidez::nova();
        f.apresentou(t);
        f.apresentou(em(t, 33));
        let l = f.linha();
        assert!(l.contains("n=1"), "{l}");
        assert!(l.contains("p50=33"), "{l}");
        assert!(l.contains("max=33"), "{l}");
    }

    /// **O que a média escondia.** Vinte e nove intervalos de 33 ms e um de 226 ms — a média dá
    /// ~39 ms e parece saudável; o `max` mostra o buraco e `trancos` o conta. É a corrida real de
    /// 01/09/2026 em miniatura.
    #[test]
    fn um_buraco_no_meio_de_uma_sessao_regular_aparece_no_max_e_nos_trancos() {
        let t = Instant::now();
        let mut f = Fluidez::nova();
        let mut agora = 0u64;
        f.apresentou(em(t, agora));
        for i in 0..30 {
            agora += if i == 15 { 226 } else { 33 };
            f.apresentou(em(t, agora));
        }
        let l = f.linha();
        assert!(l.contains("n=30"), "{l}");
        assert!(l.contains("p50=33"), "{l}");
        assert!(l.contains("max=226"), "{l}");
        assert!(l.contains("trancos=1"), "{l}");
    }

    /// O corte de `trancos` é estrito: exatamente no limiar **não** conta, e um micro acima conta.
    /// Fica fixado para que a comparação entre duas corridas não dependa de arredondamento.
    #[test]
    fn o_corte_do_tranco_e_estrito() {
        let t = Instant::now();
        let mut f = Fluidez::nova();
        f.apresentou(t);
        f.apresentou(t + Duration::from_millis(TRANCO_MS));
        assert_eq!(f.trancos(), 0, "no limiar não é tranco");
        f.apresentou(t + Duration::from_millis(TRANCO_MS) + Duration::from_millis(TRANCO_MS + 1));
        assert_eq!(f.trancos(), 1, "um milissegundo acima é");
    }

    /// **A porta aparece aqui, e é o ponto.** Segurar um quadro condenado não o conserta: a tela
    /// para. Este teste é a forma que isso tem na distribuição — os intervalos em que a porta
    /// segurou três quadros viram um intervalo de quatro tempos.
    #[test]
    fn a_porta_que_segura_quadros_aparece_como_intervalo_maior() {
        let t = Instant::now();
        let mut f = Fluidez::nova();
        let mut agora = 0u64;
        f.apresentou(em(t, agora));
        for i in 0..10 {
            // A cada cinco quadros, três ficam retidos: o intervalo seguinte é 4 x 33.
            agora += if i % 5 == 0 { 33 * 4 } else { 33 };
            f.apresentou(em(t, agora));
        }
        let l = f.linha();
        assert!(l.contains("max=132"), "{l}");
        assert_eq!(f.trancos(), 2, "dois intervalos de 132 ms passam do corte de 100");
    }

    /// Uma sessão sem quadro nenhum não afirma nada — e não divide por zero.
    #[test]
    fn sem_quadro_nenhum_a_linha_existe_e_nao_mente() {
        let f = Fluidez::nova();
        assert!(f.linha().contains("n=0"));
        assert_eq!(f.trancos(), 0);
    }
}
