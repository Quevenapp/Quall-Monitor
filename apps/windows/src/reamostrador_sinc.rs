//! **O reamostrador do som dos emissores**: sinc com janela de Kaiser, numa tabela polifásica
//! interpolada (`docs/som-no-receptor.md` §19.6.2).
//!
//! # Por que existe
//!
//! A disciplina da deriva (§19.6) põe a correção no **conteúdo**: o emissor reamostra o som por
//! `ρ = 1 + u` para o tempo de mídia seguir o relógio do host, e o carimbo anda 960 exatos. O
//! reamostrador passa a rodar sempre, com razão ≠ 1, e a qualidade dele vira a do som:
//! - o linear de antes dá 20 dB de relação sinal/erro a 8 kHz (a 1 + 500 ppm);
//! - este dá ≥ 90 dB até 16 kHz, com uma tabela de 256 fases e interpolação linear entre elas;
//! - para **descer** a taxa (o mixador a 96 kHz), o corte é escalado para a saída: sem isso, um tom
//!   de 30 kHz sairia a 0 dBFS, dobrado para 18 kHz.
//!
//! # A referência de tempo (L-c da segunda crítica)
//!
//! O sinc é simétrico: a saída `y` é centrada na posição de entrada `pos(y)`, com atraso de grupo
//! **zero** em relação a ela. A antecipação de 16 pontos é latência de processamento, e não atraso
//! do som: nada se desconta da hora.
//!
//! # O modo "só contar"
//!
//! Com `so_contar`, as posições e as contagens andam exatamente como no modo normal, mas nenhuma
//! convolução é feita e a saída é zero. É o que deixa os testes simularem horas de som em
//! segundos, com a mesma aritmética de posição do produto.
//!
//! Pura: nada de Win32.

/// Zeros do núcleo de cada lado, em amostras de saída: 32 pontos no total.
pub const MEIA_LARGURA: usize = 16;
/// Fases por amostra na tabela do núcleo. Com interpolação linear entre elas, ≥ 90 dB (a segunda
/// crítica mediu ≥ 93,7 dB a partir de 128).
pub const FASES: usize = 256;
/// O β da janela de Kaiser.
pub const BETA: f64 = 8.0;

/// A função de Bessel modificada de ordem zero, pela série.
fn bessel_i0(x: f64) -> f64 {
    let mut soma = 1.0;
    let mut termo = 1.0;
    let q = x * x / 4.0;
    for k in 1..60 {
        termo *= q / (k as f64 * k as f64);
        soma += termo;
        if termo < 1e-17 * soma {
            break;
        }
    }
    soma
}

/// O núcleo protótipo `K(t) = sinc(t) · kaiser(t / MEIA_LARGURA)`, para `t ≥ 0`, tabelado em
/// `FASES` passos por unidade.
fn tabela_do_nucleo() -> Vec<f32> {
    let n = MEIA_LARGURA * FASES + 2;
    let i0b = bessel_i0(BETA);
    (0..n)
        .map(|i| {
            let t = i as f64 / FASES as f64;
            if t >= MEIA_LARGURA as f64 {
                return 0.0;
            }
            let s = if t == 0.0 { 1.0 } else { (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t) };
            let r = t / MEIA_LARGURA as f64;
            let w = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            (s * w) as f32
        })
        .collect()
}

/// O reamostrador. Entrada e saída intercaladas, com o mesmo número de canais.
#[derive(Debug, Clone)]
pub struct ReamostradorSinc {
    canais: usize,
    /// Amostras de entrada por amostra de saída, sem o ajuste (`taxa_entrada / taxa_saida`).
    razao_nominal: f64,
    /// O ajuste da disciplina: a razão efetiva é `razao_nominal · (1 + u)`.
    u: f64,
    /// A fração do Nyquist de entrada que passa: `min(1, 1 / razao_nominal)`.
    corte: f64,
    tabela: Vec<f32>,
    /// A entrada guardada, intercalada. `entrada[0]` é a amostra de índice absoluto `base`.
    entrada: Vec<f32>,
    base: u64,
    /// O índice absoluto (entrada) do centro da próxima amostra de saída.
    pos: f64,
    /// Amostras de saída, por canal, já produzidas (incluídos os zeros postos na saída).
    produzidas: u64,
    so_contar: bool,
    /// Amostras de entrada, por canal, que o modo "só contar" tem guardadas.
    guardadas_contadas: u64,
}

impl ReamostradorSinc {
    pub fn novo(taxa_entrada: u32, taxa_saida: u32, canais: usize, so_contar: bool) -> Self {
        let razao_nominal = taxa_entrada.max(1) as f64 / taxa_saida.max(1) as f64;
        ReamostradorSinc {
            canais: canais.max(1),
            razao_nominal,
            u: 0.0,
            corte: (1.0 / razao_nominal).min(1.0),
            tabela: if so_contar { Vec::new() } else { tabela_do_nucleo() },
            entrada: Vec::new(),
            base: 0,
            pos: 0.0,
            produzidas: 0,
            so_contar,
            guardadas_contadas: 0,
        }
    }

    /// A razão efetiva: amostras de entrada por amostra de saída.
    pub fn passo(&self) -> f64 {
        self.razao_nominal * (1.0 + self.u)
    }

    pub fn definir_ajuste(&mut self, u: f64) {
        self.u = u;
    }

    pub fn ajuste(&self) -> f64 {
        self.u
    }

    /// A meia largura do núcleo, em amostras de **entrada**.
    fn meia_largura_de_entrada(&self) -> f64 {
        MEIA_LARGURA as f64 / self.corte
    }

    /// O índice absoluto da próxima amostra de entrada a ser empurrada.
    pub fn fim_da_entrada(&self) -> u64 {
        if self.so_contar {
            self.base + self.guardadas_contadas
        } else {
            self.base + (self.entrada.len() / self.canais) as u64
        }
    }

    pub fn posicao(&self) -> f64 {
        self.pos
    }

    pub fn produzidas(&self) -> u64 {
        self.produzidas
    }

    /// O índice de saída (fracionário) cuja posição de entrada é `x`, pela razão corrente.
    pub fn indice_de_saida_de(&self, x: f64) -> f64 {
        self.produzidas as f64 + (x - self.pos) / self.passo()
    }

    pub fn empurrar(&mut self, amostras: &[f32]) {
        if self.so_contar {
            self.guardadas_contadas += (amostras.len() / self.canais) as u64;
        } else {
            self.entrada.extend_from_slice(amostras);
        }
    }

    /// Empurra `quadros` amostras (por canal) sem conteúdo: zeros, ou só a contagem.
    pub fn empurrar_zeros(&mut self, quadros: u64) {
        if self.so_contar {
            self.guardadas_contadas += quadros;
        } else {
            self.entrada.resize(self.entrada.len() + quadros as usize * self.canais, 0.0);
        }
    }

    /// Pula `quadros` amostras de entrada (o corte: a entrada anda sem virar saída).
    pub fn pular_entrada(&mut self, quadros: u64) {
        self.pos += quadros as f64;
    }

    /// Produz toda a saída que a entrada guardada permite.
    pub fn produzir(&mut self, saida: &mut Vec<f32>) {
        let l = self.meia_largura_de_entrada();
        let passo = self.passo();
        let fim = self.fim_da_entrada();
        while (self.pos + l).floor() < fim as f64 {
            if self.so_contar {
                saida.resize(saida.len() + self.canais, 0.0);
            } else {
                self.uma_saida(l, saida);
            }
            self.pos += passo;
            self.produzidas += 1;
        }
        self.descartar_historia(l);
    }

    fn uma_saida(&mut self, l: f64, saida: &mut Vec<f32>) {
        let c = self.corte;
        let p = self.pos;
        let i0 = (p - l).ceil().max(self.base as f64) as u64;
        let i1 = (p + l).floor() as u64;
        let n = saida.len();
        saida.resize(n + self.canais, 0.0);
        let mut acc = [0.0f64; 8];
        for i in i0..=i1 {
            let t = ((i as f64 - p).abs() * c) * FASES as f64;
            let k = t as usize;
            if k + 1 >= self.tabela.len() {
                continue;
            }
            let fr = (t - k as f64) as f32;
            let w = (self.tabela[k] + (self.tabela[k + 1] - self.tabela[k]) * fr) as f64;
            let j = (i - self.base) as usize * self.canais;
            for ch in 0..self.canais.min(8) {
                acc[ch] += self.entrada[j + ch] as f64 * w;
            }
        }
        // **Sem normalizar pela soma dos pesos.** A soma varia com a fase fracionária, e a 500 ppm a
        // fase percorre todas as posições a ~24 Hz: normalizar modula o ganho, e o tom de 1 kHz caía
        // para 87,8 dB, com 256 ou com 1 024 fases (medido). O ganho em DC é o do corte, `c`.
        let norma = c;
        for ch in 0..self.canais.min(8) {
            saida[n + ch] = (acc[ch] * norma) as f32;
        }
    }

    fn descartar_historia(&mut self, l: f64) {
        let primeira_util = (self.pos - l).floor() - 1.0;
        if primeira_util <= self.base as f64 {
            return;
        }
        let descartar = (primeira_util as u64 - self.base).min(self.fim_da_entrada() - self.base);
        if self.so_contar {
            self.guardadas_contadas -= descartar;
        } else {
            self.entrada.drain(..descartar as usize * self.canais);
        }
        self.base += descartar;
    }

    /// Esvazia o que está no caminho: empurra zeros de entrada até toda a entrada real ter virado
    /// saída (a antecipação do núcleo), e produz. É o que vem antes de pôr zeros na saída.
    pub fn drenar(&mut self, saida: &mut Vec<f32>) {
        let fim = self.fim_da_entrada();
        if fim == 0 || (fim as f64) <= self.pos {
            return;
        }
        // A saída centrada na última amostra real precisa da entrada até ela mais a meia largura.
        let ultima_real = (fim - 1) as f64;
        let alvo = (ultima_real + self.meia_largura_de_entrada()).floor() as u64 + 1;
        if alvo > fim {
            self.empurrar_zeros(alvo - fim);
        }
        self.produzir(saida);
    }

    /// Põe `quadros` zeros **na saída**, sem passar pelo núcleo (o silêncio já está no ritmo da
    /// saída: N5 da segunda crítica). Chame [`Self::drenar`] antes.
    pub fn zeros_na_saida(&mut self, quadros: u64, saida: &mut Vec<f32>) {
        saida.resize(saida.len() + quadros as usize * self.canais, 0.0);
        self.produzidas += quadros;
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    const FS: f64 = 48_000.0;

    /// A relação sinal/erro, em dB, de um tom de `f_hz` à taxa `taxa_entrada`, reamostrado para
    /// 48 kHz com o ajuste `u`, contra o tom ideal nas posições de entrada de cada saída.
    fn snr(f_hz: f64, taxa_entrada: f64, u: f64) -> f64 {
        let mut r = ReamostradorSinc::novo(taxa_entrada as u32, FS as u32, 1, false);
        r.definir_ajuste(u);
        let n = (taxa_entrada * 1.5) as usize;
        let x: Vec<f32> = (0..n).map(|i| (2.0 * std::f64::consts::PI * f_hz * i as f64 / taxa_entrada).sin() as f32).collect();
        let mut y = Vec::new();
        // Em pedaços, como no produto.
        for c in x.chunks(441) {
            r.empurrar(c);
            r.produzir(&mut y);
        }
        let passo = r.passo();
        let (mut s, mut e) = (0.0f64, 0.0f64);
        for (k, v) in y.iter().enumerate().skip(2_000).take(40_000) {
            let pos = k as f64 * passo;
            let ideal = (2.0 * std::f64::consts::PI * f_hz * pos / taxa_entrada).sin();
            s += ideal * ideal;
            e += (*v as f64 - ideal).powi(2);
        }
        10.0 * (s / e).log10()
    }

    #[test]
    fn noventa_db_ate_16_khz_a_500_ppm() {
        for f in [1_000.0, 8_000.0, 16_000.0] {
            for u in [500e-6, -500e-6] {
                let d = snr(f, FS, u);
                eprintln!("tom {f} Hz, u {u}: {d:.1} dB");
                assert!(d >= 90.0, "{f} Hz a {u}: {d:.1} dB");
            }
        }
    }

    #[test]
    fn de_441_para_48_khz_fica_limpo() {
        let d = snr(1_000.0, 44_100.0, 0.0);
        assert!(d >= 90.0, "{d:.1} dB");
    }

    /// De 96 para 48 kHz, um tom de 30 kHz tem de ser cortado, e não dobrado para 18 kHz.
    #[test]
    fn de_96_para_48_khz_um_tom_de_30_khz_e_cortado() {
        let mut r = ReamostradorSinc::novo(96_000, 48_000, 1, false);
        let x: Vec<f32> = (0..96_000).map(|i| (2.0 * std::f64::consts::PI * 30_000.0 * i as f64 / 96_000.0).sin() as f32).collect();
        let mut y = Vec::new();
        r.empurrar(&x);
        r.produzir(&mut y);
        let rms = (y[1_000..40_000].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / 39_000.0).sqrt();
        let dbfs = 20.0 * (rms * 2f64.sqrt()).log10();
        eprintln!("30 kHz a 96 → 48 kHz: {dbfs:.1} dBFS");
        assert!(dbfs < -80.0, "{dbfs:.1} dBFS");
        // E um de 1 kHz passa inteiro.
        let mut r = ReamostradorSinc::novo(96_000, 48_000, 1, false);
        let x: Vec<f32> = (0..96_000).map(|i| (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 96_000.0).sin() as f32).collect();
        let mut y = Vec::new();
        r.empurrar(&x);
        r.produzir(&mut y);
        let rms = (y[1_000..40_000].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / 39_000.0).sqrt();
        assert!((rms * 2f64.sqrt() - 1.0).abs() < 1e-3, "{rms}");
    }

    /// L-c: um pulso na entrada sai centrado na saída cuja posição de entrada é a dele — o atraso
    /// de grupo é zero, e nada se desconta da hora.
    #[test]
    fn o_pulso_sai_na_posicao_dele() {
        for u in [0.0, 500e-6, -1_000e-6] {
            let mut r = ReamostradorSinc::novo(48_000, 48_000, 1, false);
            r.definir_ajuste(u);
            let centro = 2_000.0;
            let x: Vec<f32> = (0..4_000).map(|i| (-((i as f64 - centro) / 6.0).powi(2)).exp() as f32).collect();
            let mut y = Vec::new();
            r.empurrar(&x);
            r.produzir(&mut y);
            let passo = r.passo();
            let (mut m, mut s) = (0.0f64, 0.0f64);
            for (k, v) in y.iter().enumerate() {
                let pos = k as f64 * passo;
                m += pos * *v as f64;
                s += *v as f64;
            }
            let c = m / s;
            assert!((c - centro).abs() < 0.01, "u {u}: o pulso saiu em {c}");
        }
    }

    #[test]
    fn so_contar_anda_igual() {
        let mut a = ReamostradorSinc::novo(48_000, 48_000, 2, false);
        let mut b = ReamostradorSinc::novo(48_000, 48_000, 2, true);
        a.definir_ajuste(300e-6);
        b.definir_ajuste(300e-6);
        let (mut ya, mut yb) = (Vec::new(), Vec::new());
        for _ in 0..500 {
            a.empurrar(&[0.1f32; 960]);
            b.empurrar(&[0.1f32; 960]);
            a.produzir(&mut ya);
            b.produzir(&mut yb);
        }
        assert_eq!(ya.len(), yb.len());
        assert_eq!(a.produzidas(), b.produzidas());
        assert!((a.posicao() - b.posicao()).abs() < 1e-9);
        assert_eq!(a.fim_da_entrada(), b.fim_da_entrada());
    }

    #[test]
    fn drenar_e_zeros_na_saida() {
        let mut r = ReamostradorSinc::novo(48_000, 48_000, 1, false);
        let mut y = Vec::new();
        r.empurrar(&[1.0f32; 480]);
        r.produzir(&mut y);
        let antes = y.len();
        assert!(antes < 480, "a antecipação segura as últimas");
        r.drenar(&mut y);
        assert!(y.len() >= 480, "drenado, tudo saiu: {}", y.len());
        let p = r.produzidas();
        r.zeros_na_saida(1_000, &mut y);
        assert_eq!(r.produzidas(), p + 1_000);
        assert!(y[y.len() - 1_000..].iter().all(|v| *v == 0.0));
    }
}
