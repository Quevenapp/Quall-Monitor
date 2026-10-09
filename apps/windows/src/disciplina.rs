//! **A disciplina da deriva**: o laço que faz o tempo de mídia do som seguir o relógio do host
//! (`docs/som-no-receptor.md` §19.6).
//!
//! O atuador é o **conteúdo**: a saída do laço é o ajuste `u` da razão do reamostrador
//! ([`crate::reamostrador_sinc::ReamostradorSinc`]), e o carimbo anda 960 exatos por quadro.
//!
//! # A regra (§19.6.4)
//!
//! A entrada é o erro **aplicado**, `ε = H − S`: a hora do host da amostra de entrada contra o
//! carimbo que a linha de saída dá a ela, em µs. As medidas se agregam por período de atualização
//! (1 s no Windows e no Android; 10 s no Mac, com o mínimo), e a cada atualização:
//!
//! ```text
//! f ← limitado(f − ε·Δt/T², ±500 ppm)
//! u_alvo = limitado(f − 2ε/T, ±1 000 ppm)
//! u anda até u_alvo no máximo 20 ppm por atualização (depois da partida)
//! ```
//!
//! com T curto na partida e longo depois. A razão lisa é o que evita o flutter (§19.6.2, item 2).
//!
//! **O socorro**: com |ε| acima de 40 ms sustentado, a peça devolve [`Socorro`], e quem chama dá
//! um degrau para a frente (ε > 0) ou corta a entrada (ε < 0), nunca um degrau para trás.
//!
//! Pura, sem relógio: quem chama passa as horas.

/// Como agregar as medidas de um período.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agregado {
    /// A média recortada a ±4 MAD em volta da mediana (Windows, Android).
    MediaRecortada,
    /// O mínimo (Mac: o mínimo da entrega).
    Minimo,
}

#[derive(Debug, Clone, Copy)]
pub struct Parametros {
    pub atualizacao_s: f64,
    pub t_curto_s: f64,
    pub t_longo_s: f64,
    pub partida_s: f64,
    pub agregado: Agregado,
    /// Quanto tempo |ε| tem de ficar acima de 40 ms para o socorro.
    pub socorro_depois_de_s: f64,
}

impl Parametros {
    /// Windows e Android: a unidade de 10 ou 20 ms, a atualização por segundo.
    pub const SEGUNDO: Parametros = Parametros {
        atualizacao_s: 1.0,
        t_curto_s: 5.0,
        t_longo_s: 60.0,
        partida_s: 20.0,
        agregado: Agregado::MediaRecortada,
        socorro_depois_de_s: 1.0,
    };
    /// Mac: a atualização por 10 s, sobre o mínimo da entrega.
    pub const MAC: Parametros = Parametros {
        atualizacao_s: 10.0,
        t_curto_s: 60.0,
        t_longo_s: 300.0,
        partida_s: 300.0,
        agregado: Agregado::Minimo,
        socorro_depois_de_s: 20.0,
    };
}

/// O erro passou de 40 ms e ficou: quem chama corrige e chama [`DisciplinaDaDeriva::reancorar`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Socorro {
    pub eps_us: f64,
}

#[derive(Debug, Clone)]
pub struct DisciplinaDaDeriva {
    p: Parametros,
    /// `false`: o controle (a regra desligada). `u` fica em zero, e o socorro também não age.
    ligada: bool,
    f: f64,
    u: f64,
    inicio_us: Option<f64>,
    inicio_do_periodo_us: Option<f64>,
    medidas: Vec<f64>,
    acima_desde_us: Option<f64>,
    pub eps_us: f64,
    pub maior_eps_us: f64,
    pub socorros: u64,
    pub atualizacoes: u64,
    /// A variação de `u` entre atualizações, depois da partida: o flutter.
    pub maior_du_ppm: f64,
    soma_du2: f64,
    n_du: u64,
}

impl DisciplinaDaDeriva {
    pub const U_MAX: f64 = 1_000e-6;
    pub const F_MAX: f64 = 500e-6;
    pub const PASSO_MAXIMO_DE_U: f64 = 20e-6;
    pub const LIMIAR_DO_SOCORRO_US: f64 = 40_000.0;

    pub fn nova(p: Parametros, ligada: bool) -> Self {
        DisciplinaDaDeriva {
            p,
            ligada,
            f: 0.0,
            u: 0.0,
            inicio_us: None,
            inicio_do_periodo_us: None,
            medidas: Vec::with_capacity(128),
            acima_desde_us: None,
            eps_us: 0.0,
            maior_eps_us: 0.0,
            socorros: 0,
            atualizacoes: 0,
            maior_du_ppm: 0.0,
            soma_du2: 0.0,
            n_du: 0,
        }
    }

    pub fn ligada(&self) -> bool {
        self.ligada
    }

    /// O ajuste da razão do reamostrador.
    pub fn u(&self) -> f64 {
        self.u
    }

    /// A frequência estimada, em ppm: o que o emissor publica (L5).
    pub fn f_ppm(&self) -> f64 {
        self.f * 1e6
    }

    /// O desvio-padrão da variação de `u` entre atualizações, em ppm.
    pub fn flutter_ppm(&self) -> f64 {
        if self.n_du == 0 {
            0.0
        } else {
            (self.soma_du2 / self.n_du as f64).sqrt()
        }
    }

    /// Uma medida de ε (µs) na hora `agora_us` do host.
    pub fn medir(&mut self, agora_us: f64, eps_us: f64) -> Option<Socorro> {
        let inicio = *self.inicio_us.get_or_insert(agora_us);
        let periodo = *self.inicio_do_periodo_us.get_or_insert(agora_us);
        self.eps_us = eps_us;
        if eps_us.abs() > self.maior_eps_us.abs() {
            self.maior_eps_us = eps_us;
        }
        if self.ligada && eps_us.abs() > Self::LIMIAR_DO_SOCORRO_US {
            let desde = *self.acima_desde_us.get_or_insert(agora_us);
            if agora_us - desde >= self.p.socorro_depois_de_s * 1e6 {
                self.socorros += 1;
                self.acima_desde_us = None;
                return Some(Socorro { eps_us });
            }
        } else {
            self.acima_desde_us = None;
        }
        self.medidas.push(eps_us);
        if agora_us - periodo >= self.p.atualizacao_s * 1e6 - 1.0 {
            // O `dt` tem teto de dois períodos: depois de uma parada (o mixador parado, uma rajada
            // sem hora), o período aberto antes dela fecharia com a parada inteira, e
            // `f ← f − ε·dt/T²` multiplicaria o ruído de uma medida pela duração do silêncio
            // (a revisão do código mediu −459 ppm depois de 8 h, com 500 µs de ruído na hora).
            let dt = ((agora_us - periodo) / 1e6).min(2.0 * self.p.atualizacao_s);
            let e = self.agregar();
            self.medidas.clear();
            self.inicio_do_periodo_us = Some(agora_us);
            if self.ligada {
                self.atualizar(e, dt, (agora_us - inicio) / 1e6);
            }
        }
        None
    }

    fn agregar(&self) -> f64 {
        match self.p.agregado {
            Agregado::Minimo => self.medidas.iter().copied().fold(f64::INFINITY, f64::min),
            Agregado::MediaRecortada => {
                let mut s = self.medidas.clone();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let med = s[s.len() / 2];
                let mut desvios: Vec<f64> = s.iter().map(|x| (x - med).abs()).collect();
                desvios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mad = (1.4826 * desvios[desvios.len() / 2]).max(10.0);
                let lim = 4.0 * mad;
                s.iter().map(|x| x.clamp(med - lim, med + lim)).sum::<f64>() / s.len() as f64
            }
        }
    }

    fn atualizar(&mut self, e_us: f64, dt: f64, desde_o_inicio_s: f64) {
        let na_partida = desde_o_inicio_s < self.p.partida_s;
        let t = if na_partida { self.p.t_curto_s } else { self.p.t_longo_s };
        let (kp, ki) = (2.0 / t, 1.0 / (t * t));
        let e = e_us * 1e-6;
        self.f = (self.f - ki * e * dt).clamp(-Self::F_MAX, Self::F_MAX);
        let alvo = (self.f - kp * e).clamp(-Self::U_MAX, Self::U_MAX);
        let novo = if na_partida {
            alvo
        } else {
            alvo.clamp(self.u - Self::PASSO_MAXIMO_DE_U, self.u + Self::PASSO_MAXIMO_DE_U)
        };
        if desde_o_inicio_s > 60.0 {
            let du = (novo - self.u) * 1e6;
            self.maior_du_ppm = self.maior_du_ppm.max(du.abs());
            self.soma_du2 += du * du;
            self.n_du += 1;
        }
        self.u = novo;
        self.atualizacoes += 1;
    }

    /// Depois de um degrau (lacuna, reabertura, socorro): a fase recomeça, a frequência fica.
    pub fn reancorar(&mut self) {
        self.medidas.clear();
        self.inicio_do_periodo_us = None;
        self.acima_desde_us = None;
        self.u = self.f;
    }

    /// O dispositivo mudou (formato novo, endpoint reaberto): tudo recomeça (L4).
    pub fn recomecar(&mut self) {
        *self = DisciplinaDaDeriva::nova(self.p, self.ligada);
    }
}

#[cfg(test)]
mod testes {
    //! O laço sozinho, com o erro verdadeiro evoluindo como na simulação do §19.6.5
    //! (`quall-scratch/s8/desenho-deriva-3.py`): a cada unidade, `ε += ((1 + u)/(1 + d) − 1)·Δt`.
    //! Os números de cada linha são os da simulação, com folga. A cadeia do Windows inteira tem os
    //! testes dela em `linha_do_loopback.rs`.
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn unif(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
        fn gauss(&mut self, sigma: f64) -> f64 {
            let (u1, u2) = (self.unif().max(1e-300), self.unif());
            sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        }
    }

    struct Resultado {
        pior_us: f64,
        depois_de_10_min_us: f64,
        f_ppm: f64,
        maior_du_ppm: f64,
        socorros: u64,
    }

    fn correr(ppm: f64, horas: f64, sigma_us: f64, ligada: bool, pico: Option<(u64, f64, u64)>) -> Resultado {
        let d = ppm * 1e-6;
        let mut disc = DisciplinaDaDeriva::nova(Parametros::SEGUNDO, ligada);
        let mut rnd = Lcg(1);
        let unidade = 0.01;
        let mut eps = 0.0f64;
        let (mut pior, mut depois) = (0.0f64, 0.0f64);
        let n = (horas * 3600.0 / unidade) as u64;
        for k in 0..n {
            let t = (k + 1) as f64 * unidade;
            let mut medido = eps + if sigma_us > 0.0 { rnd.gauss(sigma_us) } else { 0.0 };
            if let Some((ini, amp, dur)) = pico {
                if k >= ini && k < ini + dur {
                    medido += amp;
                }
            }
            if disc.medir(t * 1e6, medido).is_some() {
                eps = 0.0;
                disc.reancorar();
            }
            eps += ((1.0 + disc.u()) / (1.0 + d) - 1.0) * unidade * 1e6;
            pior = pior.max(eps.abs());
            if t > 600.0 {
                depois = depois.max(eps.abs());
            }
        }
        Resultado { pior_us: pior, depois_de_10_min_us: depois, f_ppm: disc.f_ppm(), maior_du_ppm: disc.maior_du_ppm, socorros: disc.socorros }
    }

    #[test]
    fn o_controle_deixa_a_deriva_inteira() {
        let r = correr(50.0, 0.5, 0.0, false, None);
        assert!((r.pior_us - 90_000.0).abs() < 100.0, "{}", r.pior_us);
    }

    #[test]
    fn a_rampa_converge_sem_erro_de_regime() {
        for (ppm, pior_max) in [(50.0, 150.0), (100.0, 280.0), (300.0, 760.0), (-300.0, 760.0)] {
            let r = correr(ppm, 2.0, 0.0, true, None);
            assert!(r.pior_us < pior_max, "{ppm} ppm: pior {}", r.pior_us);
            assert!(r.depois_de_10_min_us < 5.0, "{ppm} ppm: {}", r.depois_de_10_min_us);
            assert!((r.f_ppm - ppm).abs() < 0.5, "{ppm} ppm: f {}", r.f_ppm);
            assert_eq!(r.socorros, 0);
        }
    }

    #[test]
    fn com_ruido_de_1_ms_a_razao_fica_lisa() {
        let r = correr(300.0, 2.0, 1_000.0, true, None);
        assert!(r.pior_us < 950.0, "{}", r.pior_us);
        assert!(r.depois_de_10_min_us < 80.0, "{}", r.depois_de_10_min_us);
        assert!(r.maior_du_ppm <= 25.0 + 1e-9, "flutter {}", r.maior_du_ppm);
    }

    #[test]
    fn um_pico_de_50_ms_na_hora_por_1_s_quase_nao_mexe() {
        let r = correr(50.0, 0.5, 100.0, true, Some((60_000, 50_000.0, 100)));
        assert!(r.pior_us < 400.0, "{}", r.pior_us);
        assert_eq!(r.socorros, 0);
    }

    #[test]
    fn fora_do_alcance_o_socorro_age() {
        let r = correr(1_500.0, 0.5, 0.0, true, None);
        assert!(r.socorros > 10, "{}", r.socorros);
        assert!(r.pior_us < 41_000.0, "{}", r.pior_us);
        let r = correr(600.0, 0.5, 0.0, true, None);
        assert_eq!(r.socorros, 0, "600 ppm cabe na razão");
    }

    #[test]
    fn a_reancoragem_guarda_a_frequencia() {
        let mut d = DisciplinaDaDeriva::nova(Parametros::SEGUNDO, true);
        let mut eps = 0.0;
        for k in 0..60_000u64 {
            d.medir((k + 1) as f64 * 1e4, eps);
            eps += ((1.0 + d.u()) / (1.0 + 100e-6) - 1.0) * 1e4;
        }
        let f = d.f_ppm();
        d.reancorar();
        assert!((d.f_ppm() - f).abs() < 1e-9);
        assert!((d.u() * 1e6 - f).abs() < 1e-6);
    }
}
