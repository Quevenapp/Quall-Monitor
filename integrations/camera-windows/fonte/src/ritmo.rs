//! **O ritmo do padrão de bancada**, quando não há cano: puro, para ser testado.
//!
//! Sem cano, a fonte entrega o padrão de bancada no relógio dela, a 30 fps: o pipeline pede o
//! próximo quadro assim que recebe o anterior, e sem ritmo o padrão sairia a milhares por segundo.
//!
//! **O defeito que isto conserta** (o R4 da câmera no Windows, 18/09, `docs/camera-no-windows.md`
//! M51): o ritmo contava do **primeiro** quadro de padrão, mas com o número de quadros entregues
//! **desde o começo do fluxo**, cano incluído. Com o cano servido desde o começo, o primeiro padrão
//! só vem quando o cano cai — e ele esperava `entregues / 30` segundos. No R4 a sonda que servia o
//! cano morreu com 598 quadros entregues, e o `RequestSample` dormiu 19,97 s dentro do Frame
//! Server: nenhum quadro para ninguém, e o `SetStreamState` e o `Shutdown` de quem consumia
//! esperaram atrás dele (o diário da fonte, 20:46:22,694 → 20:46:42,667). No produto é o mesmo
//! com a Câmera Conectada: o Quall que sai (ou cai) com um app lendo a câmera congela esse app por
//! tanto tempo quanto a câmera transmitiu.
//!
//! Agora cada trecho sem cano começa do zero, e a espera nunca passa de um intervalo de quadro.

use std::time::{Duration, Instant};

/// O ritmo de um trecho sem cano.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RitmoSemCano {
    inicio: Option<Instant>,
    entregues: u64,
}

impl RitmoSemCano {
    pub const fn novo() -> Self {
        RitmoSemCano { inicio: None, entregues: 0 }
    }

    /// Veio quadro do cano: o próximo trecho sem cano começa do zero.
    pub fn com_cano(&mut self) {
        self.inicio = None;
        self.entregues = 0;
    }

    /// **Quanto esperar antes de entregar o próximo padrão**, e conta o quadro. O primeiro de um
    /// trecho sai na hora; os seguintes, na grade de `1/fps` a partir dele. Atrasado mais de um
    /// segundo (o consumidor parou de pedir), a grade recomeça em vez de soltar uma rajada. A
    /// espera nunca passa de um intervalo de quadro.
    pub fn espera(&mut self, agora: Instant, fps: u32) -> Duration {
        let intervalo = Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1)));
        let t0 = *self.inicio.get_or_insert(agora);
        let alvo = t0 + intervalo * (self.entregues.min(u64::from(u32::MAX)) as u32);
        if agora.saturating_duration_since(alvo) > Duration::from_secs(1) {
            self.inicio = Some(agora);
            self.entregues = 1;
            return Duration::ZERO;
        }
        self.entregues += 1;
        alvo.saturating_duration_since(agora).min(intervalo)
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn r4_o_cano_que_cai_depois_de_598_quadros_nao_congela_a_fonte() {
        // O R4: 598 quadros pelo cano, e o cano cai. Antes, a primeira espera era 598/30 = 19,93 s.
        let t = Instant::now();
        let mut r = RitmoSemCano::novo();
        for _ in 0..598 {
            r.com_cano();
        }
        assert_eq!(r.espera(t, 30), Duration::ZERO, "o primeiro padrão sai na hora");
        // O segundo, pedido logo em seguida, espera um intervalo — e não mais.
        let e = r.espera(t + Duration::from_millis(1), 30);
        assert!(e <= Duration::from_nanos(33_333_333) && e >= Duration::from_millis(32), "{e:?}");
    }

    #[test]
    fn a_grade_de_30_fps_e_a_volta_do_cano() {
        let t = Instant::now();
        let mut r = RitmoSemCano::novo();
        let mut agora = t;
        // Um consumidor que pede assim que recebe: cada espera leva o quadro à grade.
        for k in 0..90u32 {
            let e = r.espera(agora, 30);
            agora += e;
            let alvo = t + Duration::from_nanos(1_000_000_000 / 30) * k;
            assert_eq!(agora, alvo, "quadro {k}");
        }
        // O cano volta e cai de novo 10 s depois: o trecho novo começa do zero.
        r.com_cano();
        let depois = agora + Duration::from_secs(10);
        assert_eq!(r.espera(depois, 30), Duration::ZERO);
        assert!(r.espera(depois, 30) <= Duration::from_nanos(33_333_333));
    }

    #[test]
    fn o_consumidor_que_some_nao_ganha_rajada_nem_espera_longa() {
        let t = Instant::now();
        let mut r = RitmoSemCano::novo();
        assert_eq!(r.espera(t, 30), Duration::ZERO);
        // Cinco segundos sem pedir: a grade recomeça, sem 150 quadros de uma vez.
        let volta = t + Duration::from_secs(5);
        assert_eq!(r.espera(volta, 30), Duration::ZERO);
        let e = r.espera(volta, 30);
        assert!(e > Duration::from_millis(30), "o seguinte espera a grade nova: {e:?}");
        // Um fps absurdo não faz divisão por zero.
        assert_eq!(RitmoSemCano::novo().espera(t, 0), Duration::ZERO);
    }
}
