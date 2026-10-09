//! **A linha do tempo do som do loopback**: do pacote do motor de áudio ao quadro carimbado
//! (`docs/som-no-receptor.md` §19.2 e §19.6).
//!
//! # Por que é um módulo à parte
//!
//! A peça é aritmética pura: nada de Win32, nada de `quall-core`, nada de rede. Morando fora de
//! `audio.rs` (que só compila no Windows com `net`), os testes dela rodam em qualquer máquina — e
//! são a prova que ela tem: o loopback de verdade captura **a mistura inteira da máquina**, o som
//! do Dell, e por isso não roda em corrida (§8, a linha do tom de bancada). Quem a usa é a thread
//! de captura de `audio.rs`, que passa as amostras (já em estéreo), a hora do pacote em µs do QPC
//! e a posição do dispositivo.
//!
//! # O que ela faz
//!
//! - **O reamostrador** ([`ReamostradorSinc`]) leva o som da taxa do mixador para 48 kHz, com a
//!   razão ajustada pela **disciplina da deriva** ([`DisciplinaDaDeriva`]): o tempo de mídia segue o
//!   relógio do host, e o carimbo anda 20 000 µs exatos por quadro (§19.6.2).
//! - **O erro** é o aplicado: `ε = H − S`, a hora do pacote contra o carimbo que a linha de saída
//!   dá à primeira amostra dele (§19.6.4). A âncora é a primeira amostra: `ε = 0` nela.
//! - **O buraco com som contínuo é achado pela posição do dispositivo** (o `u64DevicePosition` do
//!   `GetBuffer`): um salto dela contra a esperada é som que o dispositivo perdeu, e vira zeros na
//!   **entrada**. A hora do pacote não põe zero nenhum: um pico dela só vai ao ε, recortado (N3 da
//!   segunda crítica; antes, um +50 ms num pacote punha 50 ms de zeros no meio do som).
//! - **O mixador parado**: sem pacote, o ocioso põe zeros **na saída**, no ritmo do host, até agora
//!   menos a folga — sem passar pelo sinc, para o carimbo não derivar no silêncio (N5). Quando o
//!   som volta, a hora do pacote decide o que falta (mais zeros) ou sobra (corte na entrada),
//!   contra a linha de saída: a posição, com o mixador parado, não tem semântica medida.
//! - **O socorro**: com |ε| > 40 ms sustentado, um degrau para a frente (o quadro parcial é jogado
//!   fora, e o degrau entra no primeiro quadro feito só de som de depois) ou um corte na entrada;
//!   nunca um degrau para trás.
//!
//! Pura: quem chama passa as horas.

use crate::disciplina::{DisciplinaDaDeriva, Parametros, Socorro};
use crate::reamostrador_sinc::ReamostradorSinc;

pub const TAXA_DE_SAIDA: u32 = 48_000;
pub const CANAIS: usize = 2;
/// Amostras por canal num quadro de 20 ms a 48 kHz.
pub const QUADRO: usize = 960;
const US_POR_AMOSTRA: f64 = 1e6 / TAXA_DE_SAIDA as f64;
const US_POR_QUADRO: u64 = 20_000;

/// Um pacote do motor de áudio.
#[derive(Debug, Clone, Copy)]
pub struct Pacote<'a> {
    /// Estéreo intercalado, à taxa do mixador. Vazio no modo "só contar".
    pub amostras: &'a [f32],
    /// Amostras por canal.
    pub quadros: u64,
    /// A hora da primeira amostra, em µs do QPC ([`hora_do_pacote`]); `None`: sem hora.
    pub hora_us: Option<u64>,
    /// A posição do dispositivo da primeira amostra, em amostras por canal; `None`: sem posição.
    pub posicao: Option<u64>,
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`.
    pub descontinuidade: bool,
}

/// A linha do tempo. Pura: quem chama passa as horas, em µs do QPC.
#[derive(Debug, Clone)]
pub struct LinhaDoLoopback {
    taxa_entrada: u32,
    ream: ReamostradorSinc,
    pub disc: DisciplinaDaDeriva,
    /// A hora do host da saída 0, em µs do QPC.
    ancora_us: Option<f64>,
    ancora_provisoria: bool,
    /// A soma dos degraus (sempre para a frente), em µs inteiros.
    degraus_us: f64,
    /// Amostras de saída jogadas fora (quadros parciais, nos degraus).
    descartadas: u64,
    proxima_posicao: Option<u64>,
    /// O ocioso pôs zeros desde o último pacote.
    parado: bool,
    /// Amostras de entrada a cortar do começo dos próximos pacotes (o excesso depois de uma parada).
    corte_devido: u64,
    /// A hora do fim do último pacote (a regra antiga, o controle do N3).
    fim_do_pacote_us: Option<f64>,
    /// A hora do fim da entrada, contada no ritmo do host (o controle do N5).
    hora_do_fim_da_entrada_us: Option<f64>,
    saida: Vec<f32>,
    quadros_feitos: u64,
    /// **Controle do N3**: o buraco pela hora, par a par, acima de 5 ms (a regra de antes).
    pub controle_buraco_pela_hora: bool,
    /// **Controle do N5**: o silêncio do ocioso pela entrada, através do sinc (a regra de antes).
    pub controle_silencio_pelo_sinc: bool,
    // O que ela conta.
    pub buracos_por_posicao: u64,
    pub amostras_de_buraco: u64,
    pub buracos_pela_hora: u64,
    pub amostras_de_silencio: u64,
    pub amostras_cortadas: u64,
    pub pacotes_sem_hora: u64,
    pub pacotes_sem_posicao: u64,
    /// A posição do dispositivo voltou mais que [`Self::MAXIMO_DE_VOLTA_S`]: tomada como a
    /// referência nova, sem corte.
    pub posicoes_para_tras: u64,
    pub descontinuidades: u64,
    pub degraus: u64,
    pub maior_degrau_us: f64,
    /// O último ε, e o maior.
    pub desvio_us: f64,
    pub maior_desvio_us: f64,
    /// O carimbo (µs do QPC) que a linha deu à primeira amostra do último pacote com hora.
    pub ultimo_s_us: f64,
    /// Amostras cortadas do começo do último pacote.
    pub ultimo_corte: u64,
}

impl LinhaDoLoopback {
    /// Os 60 ms de folga: preencher no primeiro milissegundo de atraso brigaria com o jitter
    /// normal de entrega do mixador.
    pub const FOLGA_US: f64 = 60_000.0;
    /// No máximo isto de zeros por volta do ocioso.
    pub const MAXIMO_POR_VEZ_US: f64 = 250_000.0;
    /// Um salto da posição maior que isto não é buraco: é outra coisa (um fluxo reaberto).
    pub const MAXIMO_DE_BURACO_S: u64 = 10;
    /// O teto do corte quando a posição do dispositivo volta. Uma volta de verdade é som repetido,
    /// no máximo um buffer; acima disso, é um contador que recomeçou (um fluxo reiniciado), e
    /// cortar a volta inteira punha o resto da sessão em silêncio (a revisão do código: 300 s
    /// cortados com a posição zerada aos 300 s). Acima do teto, a posição nova vira a referência.
    pub const MAXIMO_DE_VOLTA_S: u64 = 1;
    /// A regra antiga do buraco pela hora (só no controle).
    const LIMIAR_DA_HORA_US: f64 = 5_000.0;

    /// `disciplina`: `false` é o controle (a razão fica na nominal). `so_contar`: sem convolução
    /// (os testes longos).
    pub fn nova(taxa_entrada: u32, disciplina: bool, so_contar: bool) -> Self {
        LinhaDoLoopback {
            taxa_entrada: taxa_entrada.max(1),
            ream: ReamostradorSinc::novo(taxa_entrada, TAXA_DE_SAIDA, CANAIS, so_contar),
            disc: DisciplinaDaDeriva::nova(Parametros::SEGUNDO, disciplina),
            ancora_us: None,
            ancora_provisoria: false,
            degraus_us: 0.0,
            descartadas: 0,
            proxima_posicao: None,
            parado: false,
            corte_devido: 0,
            fim_do_pacote_us: None,
            hora_do_fim_da_entrada_us: None,
            saida: Vec::with_capacity(QUADRO * CANAIS * 4),
            quadros_feitos: 0,
            controle_buraco_pela_hora: false,
            controle_silencio_pelo_sinc: false,
            buracos_por_posicao: 0,
            amostras_de_buraco: 0,
            buracos_pela_hora: 0,
            amostras_de_silencio: 0,
            amostras_cortadas: 0,
            pacotes_sem_hora: 0,
            pacotes_sem_posicao: 0,
            posicoes_para_tras: 0,
            descontinuidades: 0,
            degraus: 0,
            maior_degrau_us: 0.0,
            desvio_us: 0.0,
            maior_desvio_us: 0.0,
            ultimo_s_us: 0.0,
            ultimo_corte: 0,
        }
    }

    /// O começo da captura: a âncora provisória, para o silêncio do começo ter onde entrar.
    pub fn comecar(&mut self, agora_us: u64) {
        if agora_us == 0 || self.ancora_us.is_some() {
            return;
        }
        self.ancora_us = Some(agora_us as f64);
        self.ancora_provisoria = true;
        self.hora_do_fim_da_entrada_us = Some(agora_us as f64);
    }

    /// A hora do primeiro quadro (µs do QPC), quando já há âncora.
    pub fn ancora_us(&self) -> Option<u64> {
        self.ancora_us.map(|a| a as u64)
    }

    /// O carimbo que a linha dá ao índice de saída `y`.
    fn s_de(&self, y: f64) -> f64 {
        self.ancora_us.unwrap_or(0.0) + (y - self.descartadas as f64) * US_POR_AMOSTRA + self.degraus_us
    }

    /// O carimbo do fim do que já entrou (a entrada guardada, projetada na saída).
    pub fn fim_da_linha_us(&self) -> f64 {
        self.s_de(self.ream.indice_de_saida_de(self.ream.fim_da_entrada() as f64))
    }

    pub fn f_ppm(&self) -> f64 {
        self.disc.f_ppm()
    }

    pub fn ajuste_ppm(&self) -> f64 {
        self.ream.ajuste() * 1e6
    }

    pub fn pacote(&mut self, p: Pacote<'_>) {
        if p.descontinuidade {
            self.descontinuidades += 1;
        }
        if p.hora_us.is_none() {
            self.pacotes_sem_hora += 1;
        }
        if p.posicao.is_none() {
            self.pacotes_sem_posicao += 1;
        }
        let taxa = self.taxa_entrada as f64;
        let hora = p.hora_us.map(|h| h as f64);

        // A âncora: a hora do primeiro pacote, se nenhum silêncio entrou antes dele.
        if let Some(h) = hora {
            let sem_nada = self.ream.fim_da_entrada() == 0 && self.ream.produzidas() == 0;
            if self.ancora_us.is_none() || (self.ancora_provisoria && sem_nada) {
                self.ancora_us = Some(h);
                self.hora_do_fim_da_entrada_us = Some(h);
            }
        } else if self.ancora_us.is_none() {
            // Sem hora e sem âncora: o som entra, e a âncora vem com a primeira hora.
            self.empurrar(p, 0);
            self.proxima_posicao = p.posicao.map(|q| q + p.quadros);
            return;
        }
        self.ancora_provisoria = false;

        let mut zeros_de_entrada = 0u64;
        if self.parado {
            // **Depois de uma parada**: a hora do pacote que volta decide, contra a linha de saída.
            if let Some(h) = hora {
                if !self.controle_silencio_pelo_sinc {
                    self.ream.drenar(&mut self.saida);
                }
                let falta = h - self.fim_da_linha_us();
                if falta > 0.0 {
                    let n = (falta / US_POR_AMOSTRA).round() as u64;
                    self.ream.zeros_na_saida(n, &mut self.saida);
                    self.amostras_de_silencio += n;
                } else {
                    self.corte_devido += (-falta * taxa / 1e6).round() as u64;
                }
                // A fase acabou de ser decidida pela hora: o laço recomeça a medir daqui, e a
                // frequência fica (a revisão do código, A).
                self.disc.reancorar();
            }
            self.parado = false;
        } else if self.controle_buraco_pela_hora {
            // O controle do N3: a regra antiga, a hora contra o fim do pacote anterior.
            if let (Some(h), Some(fim)) = (hora, self.fim_do_pacote_us) {
                let b = h - fim;
                if b > Self::LIMIAR_DA_HORA_US && b <= (Self::MAXIMO_DE_BURACO_S * 1_000_000) as f64 {
                    zeros_de_entrada = (b * taxa / 1e6).round() as u64;
                    self.buracos_pela_hora += 1;
                }
            }
        } else if let (Some(q), Some(esperada)) = (p.posicao, self.proxima_posicao) {
            // **Com som contínuo, o buraco é da posição**: som que o dispositivo perdeu.
            if q > esperada {
                let d = q - esperada;
                if d <= Self::MAXIMO_DE_BURACO_S * self.taxa_entrada as u64 {
                    zeros_de_entrada = d;
                    self.buracos_por_posicao += 1;
                }
            } else if q < esperada {
                let d = esperada - q;
                if d <= Self::MAXIMO_DE_VOLTA_S * self.taxa_entrada as u64 {
                    self.corte_devido += d;
                } else {
                    self.posicoes_para_tras += 1;
                }
            }
        }
        // Sem posição, a esperada anda o tamanho do pacote: senão o seguinte mostraria um buraco
        // falso do tamanho dele (a revisão do código, leve 9).
        self.proxima_posicao = match p.posicao {
            Some(q) => Some(q + p.quadros),
            None => self.proxima_posicao.map(|e| e + p.quadros),
        };
        self.fim_do_pacote_us = hora.map(|h| h + p.quadros as f64 * 1e6 / taxa);
        if zeros_de_entrada > 0 {
            self.ream.empurrar_zeros(zeros_de_entrada);
            self.amostras_de_buraco += zeros_de_entrada;
        }

        // O corte devido sai do começo deste pacote.
        let cortar = self.corte_devido.min(p.quadros);
        self.corte_devido -= cortar;
        self.amostras_cortadas += cortar;
        self.ultimo_corte = cortar;
        let x0 = self.ream.fim_da_entrada() as f64;
        self.empurrar(p, cortar);
        if let Some(fim) = self.hora_do_fim_da_entrada_us.as_mut() {
            *fim = hora.map(|h| h + p.quadros as f64 * 1e6 / taxa).unwrap_or(*fim + (p.quadros - cortar) as f64 * 1e6 / taxa);
        }

        // O erro aplicado, e o laço.
        if let Some(h) = hora {
            let hk = h + cortar as f64 * 1e6 / taxa;
            let s = self.s_de(self.ream.indice_de_saida_de(x0));
            self.ultimo_s_us = s;
            let eps = hk - s;
            self.desvio_us = eps;
            if eps.abs() > self.maior_desvio_us.abs() {
                self.maior_desvio_us = eps;
            }
            if let Some(soc) = self.disc.medir(hk, eps) {
                self.socorro(soc, x0, hk);
            }
        }
        self.ream.definir_ajuste(self.disc.u());
        self.ream.produzir(&mut self.saida);
    }

    fn empurrar(&mut self, p: Pacote<'_>, cortar: u64) {
        if p.amostras.is_empty() {
            self.ream.empurrar_zeros(p.quadros - cortar);
        } else {
            let de = (cortar as usize * CANAIS).min(p.amostras.len());
            self.ream.empurrar(&p.amostras[de..]);
        }
    }

    fn socorro(&mut self, soc: Socorro, x0: f64, hk: f64) {
        self.degraus += 1;
        if soc.eps_us > 0.0 {
            // Para a frente: o som de antes que ainda não virou quadro sai, e o degrau entra no
            // primeiro quadro feito só de som de depois.
            let parcial = (self.saida.len() / CANAIS) as u64;
            self.saida.clear();
            self.descartadas += parcial;
            let degrau = (hk - self.s_de(self.ream.indice_de_saida_de(x0))).max(0.0).round();
            self.degraus_us += degrau;
            self.maior_degrau_us = self.maior_degrau_us.max(degrau);
        } else {
            // Nunca para trás: a entrada é cortada, e a linha alcança o host.
            let n = (-soc.eps_us * self.taxa_entrada as f64 / 1e6).round() as u64;
            self.ream.pular_entrada(n);
            self.amostras_cortadas += n;
        }
        self.disc.reancorar();
    }

    /// Nenhum pacote nesta volta: zeros **na saída**, no ritmo do host, até agora menos a folga.
    pub fn ocioso(&mut self, agora_us: u64) {
        if self.ancora_us.is_none() {
            return;
        }
        let agora = agora_us as f64;
        if self.controle_silencio_pelo_sinc {
            // O controle do N5: zeros na entrada, contados no ritmo do host, e depois pelo sinc.
            let fim = self.hora_do_fim_da_entrada_us.unwrap_or(agora);
            if agora > fim + Self::FOLGA_US {
                let us = (agora - Self::FOLGA_US - fim).min(Self::MAXIMO_POR_VEZ_US);
                let n = (us * self.taxa_entrada as f64 / 1e6) as u64;
                self.ream.empurrar_zeros(n);
                self.hora_do_fim_da_entrada_us = Some(fim + n as f64 * 1e6 / self.taxa_entrada as f64);
                self.amostras_de_silencio += n;
                self.parado = true;
                self.ream.produzir(&mut self.saida);
            }
            return;
        }
        if agora <= self.fim_da_linha_us() + Self::FOLGA_US {
            return;
        }
        if !self.parado {
            self.ream.drenar(&mut self.saida);
            self.parado = true;
        }
        let us = (agora - Self::FOLGA_US - self.fim_da_linha_us()).min(Self::MAXIMO_POR_VEZ_US);
        if us > 0.0 {
            let n = (us / US_POR_AMOSTRA) as u64;
            self.ream.zeros_na_saida(n, &mut self.saida);
            self.amostras_de_silencio += n;
        }
    }

    /// O próximo quadro de 20 ms (estéreo intercalado, 48 kHz), com o carimbo em µs do QPC.
    pub fn proximo_quadro(&mut self) -> Option<(Vec<f32>, u64)> {
        if self.saida.len() < QUADRO * CANAIS {
            return None;
        }
        let quadro: Vec<f32> = self.saida.drain(..QUADRO * CANAIS).collect();
        let carimbo = self.ancora_us.unwrap_or(0.0) as u64 + self.quadros_feitos * US_POR_QUADRO + self.degraus_us as u64;
        self.quadros_feitos += 1;
        Some((quadro, carimbo))
    }
}

/// A hora da primeira amostra de um pacote, em µs do QPC, a partir do que o `GetBuffer` deu: a
/// posição em unidades de 100 ns e as bandeiras. `None` quando o motor diz que a hora está errada
/// (`bandeira_de_erro`, o `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`) ou quando ela veio zero.
pub fn hora_do_pacote(bandeiras: u32, bandeira_de_erro: u32, posicao_qpc_100ns: u64) -> Option<u64> {
    if bandeiras & bandeira_de_erro != 0 || posicao_qpc_100ns == 0 {
        None
    } else {
        Some(posicao_qpc_100ns / 10)
    }
}

/// Um carimbo em µs do QPC levado à origem comum da sessão (a mesma do vídeo), por um par
/// (`Instant` desde a origem, QPC) lido junto. Nunca negativo.
pub fn na_origem(par_origem_us: u64, par_qpc_us: u64, carimbo_qpc_us: u64) -> u64 {
    (par_origem_us as i128 + carimbo_qpc_us as i128 - par_qpc_us as i128).max(0) as u64
}

#[cfg(test)]
mod testes {
    //! Os testes do §19.6.6 que cabem no Windows, **contra a linha real** (N3 da segunda crítica),
    //! cada um com o controle: a regra desligada ou a regra de antes.
    //!
    //! O mixador de mentira dá pacotes de 10 ms à taxa de um dispositivo com a deriva pedida, com a
    //! hora que o motor carimbaria (µs do QPC, com o ruído pedido) e a posição do dispositivo, e os
    //! entrega ao laço um pouco depois; o laço consulta a cada ~5 ms, como o de `audio.rs`. A
    //! testemunha é a hora **verdadeira** de cada pacote contra o carimbo que a linha deu a ele.
    use super::*;

    const T0: u64 = 5_000_000_000;

    struct Lcg(u64);
    impl Lcg {
        fn unif(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
        fn gauss(&mut self, s: f64) -> f64 {
            let (u1, u2) = (self.unif().max(1e-300), self.unif());
            s * (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        }
    }

    #[derive(Clone, Copy)]
    enum Ruido {
        Nenhum,
        Gauss(f64),
        /// Gaussiano, com 1 % dos pacotes a ±5 ms.
        Cauda(f64),
    }

    struct Cenario {
        segundos: f64,
        ppm: f64,
        ruido: Ruido,
        /// (início, duração) em s: o mixador parado (a posição não anda).
        pausas: Vec<(f64, f64)>,
        /// (início, fim) em s: a entrega presa (a hora e a posição seguem).
        presas: Vec<(f64, f64)>,
        /// Índices de pacotes perdidos: a posição e a hora dos seguintes seguem certas.
        perdidos: Vec<u64>,
        /// (índice, amplitude µs, duração em pacotes): a hora mente, o conteúdo segue.
        picos: Vec<(u64, f64, u64)>,
        sem_hora: Vec<u64>,
        /// Pacotes sem posição (`None`).
        sem_posicao: Vec<u64>,
        /// A partir deste pacote, a posição do dispositivo recomeça do zero (um fluxo que zerou).
        posicao_zera_no_pacote: Option<u64>,
        comeco_s: f64,
        disciplina: bool,
        buraco_pela_hora: bool,
        silencio_pelo_sinc: bool,
    }

    impl Cenario {
        fn novo(segundos: f64, ppm: f64) -> Self {
            Cenario { segundos, ppm, ruido: Ruido::Nenhum, pausas: vec![], presas: vec![], perdidos: vec![], picos: vec![], sem_hora: vec![], sem_posicao: vec![], posicao_zera_no_pacote: None, comeco_s: 0.0, disciplina: true, buraco_pela_hora: false, silencio_pelo_sinc: false }
        }
    }

    #[derive(Default, Debug)]
    struct Resultado {
        /// O pior |hora verdadeira − carimbo|, em µs, no total e depois de 10 min.
        pior_us: f64,
        depois_de_10_min_us: f64,
        final_us: f64,
        /// Os carimbos de quadro que não andaram 20 000 µs (fora os degraus contados).
        incrementos_errados: u64,
        carimbos_para_tras: u64,
        f_ppm: f64,
        maior_du_ppm: f64,
        /// O pior |S do fim da linha − (agora − folga)| no meio das paradas, em µs.
        pior_no_silencio_us: f64,
        /// O maior |f − ppm| depois da primeira parada, em ppm (e o `f` da 1ª atualização depois).
        maior_desvio_de_f_depois_da_parada_ppm: f64,
        pior_depois_da_parada_us: f64,
        /// Quadros de saída depois que a posição zerou.
        quadros_depois_do_zero: u64,
    }

    fn correr(c: &Cenario) -> (Resultado, LinhaDoLoopback) {
        let mut l = LinhaDoLoopback::nova(48_000, c.disciplina, true);
        l.controle_buraco_pela_hora = c.buraco_pela_hora;
        l.controle_silencio_pelo_sinc = c.silencio_pelo_sinc;
        let mut rnd = Lcg(7);
        let taxa_real = 48_000.0 * (1.0 + c.ppm * 1e-6);
        let mut r = Resultado::default();
        l.comecar(T0);
        // Os pacotes, na ordem: (hora verdadeira, hora medida, entrega, posição).
        let mut pacotes: Vec<(f64, Option<u64>, f64, Option<u64>)> = Vec::new();
        let fim_da_parada = c.pausas.first().map(|&(ini, dur)| T0 as f64 + (ini + dur) * 1e6);
        let zero_em = c.posicao_zera_no_pacote.map(|z| T0 as f64 + z as f64 * 480.0 / taxa_real * 1e6);
        let mut amostras = 0u64;
        let mut pausa_us = c.comeco_s * 1e6;
        let mut pausas = c.pausas.clone();
        let mut k = 0u64;
        let mut posicao = 0u64;
        loop {
            let base = T0 as f64 + pausa_us + amostras as f64 / taxa_real * 1e6;
            if let Some(&(ini, dur)) = pausas.first() {
                if base >= T0 as f64 + ini * 1e6 {
                    pausa_us += dur * 1e6;
                    pausas.remove(0);
                    continue;
                }
            }
            if base > T0 as f64 + c.segundos * 1e6 {
                break;
            }
            let mut medida = base + match c.ruido {
                Ruido::Nenhum => 0.0,
                Ruido::Gauss(s) => rnd.gauss(s),
                Ruido::Cauda(s) => if rnd.unif() < 0.01 { if rnd.unif() < 0.5 { -5_000.0 } else { 5_000.0 } } else { rnd.gauss(s) },
            };
            for &(ini, amp, dur) in &c.picos {
                if k >= ini && k < ini + dur {
                    medida += amp;
                }
            }
            let mut entrega = base + 10_000.0 + 2_000.0 + rnd.unif() * 3_000.0;
            for &(ini, fim) in &c.presas {
                if entrega >= T0 as f64 + ini * 1e6 && entrega < T0 as f64 + fim * 1e6 {
                    entrega = T0 as f64 + fim * 1e6;
                }
            }
            let hora = if c.sem_hora.contains(&k) { None } else { Some(medida.max(1.0) as u64) };
            if c.posicao_zera_no_pacote == Some(k) {
                posicao = 0;
            }
            if !c.perdidos.contains(&k) {
                let pos = if c.sem_posicao.contains(&k) { None } else { Some(posicao) };
                pacotes.push((base, hora, entrega, pos));
            }
            amostras += 480;
            posicao += 480;
            k += 1;
        }
        for i in 1..pacotes.len() {
            if pacotes[i].2 < pacotes[i - 1].2 {
                pacotes[i].2 = pacotes[i - 1].2;
            }
        }
        let fim = pacotes.last().map(|p| p.2 + 1.0).unwrap_or(0.0);
        let mut agora = T0 as f64;
        let mut i = 0usize;
        let mut ultimo_carimbo: Option<u64> = None;
        let degraus_antes = |l: &LinhaDoLoopback| l.degraus;
        while agora < fim {
            let mut veio = false;
            while i < pacotes.len() && pacotes[i].2 <= agora {
                veio = true;
                let (verdade, hora, _, pos) = pacotes[i];
                i += 1;
                let d0 = degraus_antes(&l);
                l.pacote(Pacote { amostras: &[], quadros: 480, hora_us: hora, posicao: pos, descontinuidade: false });
                if hora.is_some() && l.ancora_us().is_some() {
                    let e = (verdade + l.ultimo_corte as f64 * 1e6 / taxa_real) - l.ultimo_s_us;
                    r.pior_us = r.pior_us.max(e.abs());
                    if agora > T0 as f64 + 600e6 {
                        r.depois_de_10_min_us = r.depois_de_10_min_us.max(e.abs());
                    }
                    r.final_us = e;
                    if fim_da_parada.map_or(false, |f| verdade >= f) {
                        r.maior_desvio_de_f_depois_da_parada_ppm = r.maior_desvio_de_f_depois_da_parada_ppm.max((l.f_ppm() - c.ppm).abs());
                        r.pior_depois_da_parada_us = r.pior_depois_da_parada_us.max(e.abs());
                    }
                }
                let depois_do_zero = zero_em.map_or(false, |z| verdade >= z);
                while let Some((_, carimbo)) = l.proximo_quadro() {
                    if depois_do_zero {
                        r.quadros_depois_do_zero += 1;
                    }
                    if let Some(u) = ultimo_carimbo {
                        if carimbo < u {
                            r.carimbos_para_tras += 1;
                        } else if carimbo - u != 20_000 && l.degraus == d0 {
                            r.incrementos_errados += 1;
                        }
                    }
                    ultimo_carimbo = Some(carimbo);
                }
            }
            if !veio {
                l.ocioso(agora as u64);
                // No meio de uma parada, a linha tem de estar na hora do host (menos a folga).
                let parada = c.pausas.iter().any(|&(ini, dur)| {
                    agora > T0 as f64 + (ini + 1.0) * 1e6 && agora < T0 as f64 + (ini + dur) * 1e6
                });
                if parada {
                    let e = l.fim_da_linha_us() - (agora - LinhaDoLoopback::FOLGA_US);
                    r.pior_no_silencio_us = r.pior_no_silencio_us.max(e.abs().min(1e9));
                }
                while let Some((_, carimbo)) = l.proximo_quadro() {
                    if let Some(u) = ultimo_carimbo {
                        if carimbo < u {
                            r.carimbos_para_tras += 1;
                        }
                    }
                    ultimo_carimbo = Some(carimbo);
                }
            }
            agora += 5_000.0 + rnd.unif() * 1_000.0;
        }
        r.f_ppm = l.f_ppm();
        r.maior_du_ppm = l.disc.maior_du_ppm;
        (r, l)
    }

    // --- 1. o controle, e a deriva ---

    #[test]
    fn sem_disciplina_50_ppm_por_30_min_da_90_ms_e_com_ela_nao() {
        let mut c = Cenario::novo(1_800.0, 50.0);
        c.disciplina = false;
        let (r, _) = correr(&c);
        eprintln!("controle: final {:.1} ms", r.final_us / 1000.0);
        assert!((r.final_us.abs() - 90_000.0).abs() < 1_000.0, "{}", r.final_us);
        let c = Cenario::novo(1_800.0, 50.0);
        let (r, l) = correr(&c);
        eprintln!("disciplina: pior {:.3} ms, depois de 10 min {:.1} µs, f {:.2} ppm", r.pior_us / 1000.0, r.depois_de_10_min_us, r.f_ppm);
        assert!(r.pior_us < 200.0, "{}", r.pior_us);
        assert!(r.depois_de_10_min_us < 25.0, "{}", r.depois_de_10_min_us);
        assert!((r.f_ppm - 50.0).abs() < 0.5);
        assert_eq!(l.amostras_de_buraco + l.amostras_de_silencio + l.amostras_cortadas, 0);
        assert_eq!(r.incrementos_errados + r.carimbos_para_tras, 0);
    }

    #[test]
    fn a_deriva_de_mais_e_menos_300_ppm_por_2_h_com_ruido() {
        for (ppm, ruido, pior_max, depois_max) in [
            (300.0, Ruido::Gauss(11.0), 800.0, 25.0),
            (-300.0, Ruido::Gauss(11.0), 800.0, 25.0),
            // Com 1 ms de ruído, o pior da partida é o da âncora, que é uma hora só (L7 da primeira
            // crítica): medido 1,24 ms, e o laço o tira em ~T. O regime é o que importa.
            (100.0, Ruido::Gauss(1_000.0), 3_000.0, 120.0),
            (300.0, Ruido::Cauda(500.0), 900.0, 80.0),
        ] {
            let mut c = Cenario::novo(7_200.0, ppm);
            c.ruido = ruido;
            let (r, l) = correr(&c);
            eprintln!("{ppm} ppm: pior {:.3} ms, depois de 10 min {:.1} µs, f {:.2}, Δu máx {:.1} ppm, buracos {} zeros {}",
                r.pior_us / 1000.0, r.depois_de_10_min_us, r.f_ppm, r.maior_du_ppm, l.buracos_por_posicao, l.amostras_de_buraco);
            assert!(r.pior_us < pior_max, "{ppm}: pior {}", r.pior_us);
            assert!(r.depois_de_10_min_us < depois_max, "{ppm}: {}", r.depois_de_10_min_us);
            assert!(r.maior_du_ppm <= 25.0 + 1e-6, "flutter {}", r.maior_du_ppm);
            assert_eq!(l.amostras_de_buraco + l.amostras_de_silencio, 0, "nenhum zero no meio do som");
            assert_eq!(r.incrementos_errados + r.carimbos_para_tras, 0);
        }
    }

    // --- 5. picos só na hora (N3), contra a linha real ---

    #[test]
    fn picos_na_hora_nao_poem_zero_no_meio_do_som() {
        for (amp, dur) in [(5_000.0, 1u64), (50_000.0, 1), (50_000.0, 2), (50_000.0, 5), (50_000.0, 100)] {
            let mut c = Cenario::novo(300.0, 50.0);
            c.ruido = Ruido::Gauss(100.0);
            c.picos = vec![(12_000, amp, dur)];
            let (r, l) = correr(&c);
            assert_eq!(l.amostras_de_buraco + l.amostras_de_silencio, 0, "+{amp} µs por {dur}");
            assert!(r.final_us.abs() < 500.0, "+{amp} µs por {dur}: {}", r.final_us);
        }
        // O controle: a regra antiga (o buraco pela hora, 5 ms) põe 50 ms de zeros num pico só.
        let mut c = Cenario::novo(300.0, 50.0);
        c.picos = vec![(12_000, 50_000.0, 1)];
        c.buraco_pela_hora = true;
        let (_, l) = correr(&c);
        eprintln!("controle do N3: {} buraco(s) pela hora, {} zeros", l.buracos_pela_hora, l.amostras_de_buraco);
        assert!(l.amostras_de_buraco >= 48 * 49, "{}", l.amostras_de_buraco);
    }

    #[test]
    fn a_cauda_pesada_por_1_h_nao_poe_zero() {
        let mut c = Cenario::novo(3_600.0, 0.0);
        c.ruido = Ruido::Cauda(500.0);
        let (r, l) = correr(&c);
        assert_eq!(l.amostras_de_buraco + l.amostras_de_silencio, 0);
        assert!(r.depois_de_10_min_us < 100.0, "{}", r.depois_de_10_min_us);
        let mut c = Cenario::novo(3_600.0, 0.0);
        c.ruido = Ruido::Cauda(500.0);
        c.buraco_pela_hora = true;
        let (_, l) = correr(&c);
        eprintln!("controle do N3, cauda por 1 h: {} buracos, {:.0} ms de zeros", l.buracos_pela_hora, l.amostras_de_buraco as f64 / 48.0);
        assert!(l.buracos_pela_hora > 100);
    }

    // --- 4. o buraco pela posição ---

    #[test]
    fn cada_pacote_perdido_vira_os_zeros_dele_pela_posicao() {
        let mut c = Cenario::novo(60.0, 0.0);
        c.ruido = Ruido::Gauss(300.0);
        c.perdidos = (0..20).map(|k| 500 + 200 * k).collect();
        let (r, l) = correr(&c);
        assert_eq!(l.buracos_por_posicao, 20);
        assert_eq!(l.amostras_de_buraco, 20 * 480);
        assert!(r.pior_us < 1_500.0, "{}", r.pior_us);
        assert!(r.final_us.abs() < 500.0, "{}", r.final_us);
    }

    // --- o mixador parado, a entrega presa, o começo ---

    #[test]
    fn mixador_parado_2_s_ganha_2_s_de_zeros_na_saida() {
        let mut c = Cenario::novo(60.0, 0.0);
        c.pausas = vec![(30.0, 2.0)];
        let (r, l) = correr(&c);
        let ms = l.amostras_de_silencio as f64 / 48.0;
        eprintln!("parado 2 s: {ms:.1} ms de silêncio, cortadas {}, final {:.2} ms", l.amostras_cortadas, r.final_us / 1000.0);
        assert!((ms - 2_000.0).abs() < 2.0, "{ms}");
        assert!(r.final_us.abs() < 500.0, "{}", r.final_us);
        assert_eq!(r.carimbos_para_tras, 0);
    }

    #[test]
    fn entrega_presa_corta_o_excesso() {
        let mut c = Cenario::novo(60.0, 0.0);
        c.presas = vec![(30.0, 30.2)];
        let (r, l) = correr(&c);
        eprintln!("entrega presa 200 ms: {:.1} ms de silêncio, {:.1} ms cortados, final {:.2} ms",
            l.amostras_de_silencio as f64 / 48.0, l.amostras_cortadas as f64 / 48.0, r.final_us / 1000.0);
        assert!(l.amostras_de_silencio > 48 * 100);
        assert!(((l.amostras_de_silencio as i64) - (l.amostras_cortadas as i64)).abs() < 48 * 2);
        assert!(r.final_us.abs() < 500.0, "{}", r.final_us);
    }

    #[test]
    fn mixador_parado_no_comeco() {
        let mut c = Cenario::novo(10.0, 0.0);
        c.comeco_s = 0.5;
        let (r, l) = correr(&c);
        assert_eq!(l.ancora_us(), Some(T0));
        assert!((l.amostras_de_silencio as f64 / 48.0 - 500.0).abs() < 2.0, "{}", l.amostras_de_silencio);
        assert!(r.final_us.abs() < 500.0, "{}", r.final_us);
    }

    /// A revisão do código (A): a primeira atualização depois de uma parada do mixador fechava o
    /// período aberto antes dela, com `dt` igual à parada inteira, e `f ← f − ε·dt/T²` multiplicava
    /// o ruído de uma medida pela duração do silêncio (o controle: 497,8 ppm depois de 8 h, com
    /// 500 µs de ruído na hora; a revisão mediu −459 ppm).
    ///
    /// Com o `dt` limitado, **a duração da parada sai do `f`**. O que sobra é a fase da volta,
    /// decidida pela hora de um pacote só: com 500 µs de ruído, ~2 ppm de excursão (ε₀/T), igual
    /// numa parada de 10 min e numa de 8 h; com o ruído do Dell, ~0.
    #[test]
    fn uma_parada_longa_nao_puxa_o_f() {
        let mut desvios = Vec::new();
        for (sigma, parada_s) in [(500.0, 600.0), (500.0, 8.0 * 3_600.0), (11.0, 8.0 * 3_600.0)] {
            let mut c = Cenario::novo(600.0 + parada_s + 600.0, 50.0);
            c.ruido = Ruido::Gauss(sigma);
            c.pausas = vec![(600.0, parada_s)];
            let (r, l) = correr(&c);
            eprintln!("parada de {:.0} s, {sigma} µs: |f − 50| máx depois {:.2} ppm, pior ε depois {:.3} ms, f final {:.2}",
                parada_s, r.maior_desvio_de_f_depois_da_parada_ppm, r.pior_depois_da_parada_us / 1000.0, r.f_ppm);
            assert_eq!(r.carimbos_para_tras, 0);
            assert_eq!(l.amostras_de_buraco, 0);
            desvios.push(r.maior_desvio_de_f_depois_da_parada_ppm);
        }
        // 10 min e 8 h dão o mesmo, e pouco; com o ruído do Dell, quase nada.
        assert!(desvios[1] < 3.0, "{}", desvios[1]);
        assert!((desvios[1] - desvios[0]).abs() < 1.0, "{desvios:?}");
        assert!(desvios[2] < 0.5, "{}", desvios[2]);
    }

    /// A revisão do código (B): a posição do dispositivo que volta a zero (um fluxo reiniciado; a
    /// semântica da posição não foi medida) somava a volta inteira ao corte, sem teto: 300 s de
    /// entrada cortados, e o resto da sessão em silêncio (medido pela revisão).
    #[test]
    fn a_posicao_que_zera_nao_vira_silencio() {
        let mut c = Cenario::novo(600.0, 0.0);
        c.ruido = Ruido::Gauss(100.0);
        c.posicao_zera_no_pacote = Some(30_000);
        let (r, l) = correr(&c);
        eprintln!("posição zerada aos 300 s: cortadas {:.1} s, quadros depois {} (de ~15 000), voltas {}, final {:.2} ms",
            l.amostras_cortadas as f64 / 48_000.0, r.quadros_depois_do_zero, l.posicoes_para_tras, r.final_us / 1000.0);
        assert_eq!(l.amostras_cortadas, 0);
        assert_eq!(l.posicoes_para_tras, 1);
        assert!(r.quadros_depois_do_zero > 14_900, "{}", r.quadros_depois_do_zero);
        assert!(r.final_us.abs() < 500.0, "{}", r.final_us);
    }

    /// A revisão do código (leve 9): um pacote sem posição não fazia a posição esperada andar, e o
    /// seguinte mostrava um buraco falso do tamanho dele.
    #[test]
    fn um_pacote_sem_posicao_nao_vira_buraco() {
        let mut c = Cenario::novo(300.0, 0.0);
        c.sem_posicao = vec![10_000];
        let (r, l) = correr(&c);
        assert_eq!(l.buracos_por_posicao, 0);
        assert_eq!(l.amostras_de_buraco, 0);
        assert!(r.final_us.abs() < 500.0, "{}", r.final_us);
    }

    // --- 10. o silêncio do Windows (N5) ---

    #[test]
    fn meia_hora_de_silencio_a_50_ppm_nao_deriva() {
        let mut c = Cenario::novo(2_400.0, 50.0);
        c.pausas = vec![(300.0, 1_800.0)];
        let (r, l) = correr(&c);
        eprintln!("silêncio de 30 min a 50 ppm: pior no silêncio {:.2} ms, final {:.3} ms", r.pior_no_silencio_us / 1000.0, r.final_us / 1000.0);
        assert!(r.pior_no_silencio_us < 1_000.0, "{}", r.pior_no_silencio_us);
        assert!(r.final_us.abs() < 500.0);
        assert_eq!(r.carimbos_para_tras, 0);
        let _ = l;
        // O controle: o silêncio pelo sinc, à razão ajustada para o dispositivo.
        let mut c = Cenario::novo(2_400.0, 50.0);
        c.pausas = vec![(300.0, 1_800.0)];
        c.silencio_pelo_sinc = true;
        let (r, _) = correr(&c);
        eprintln!("controle do N5: pior no silêncio {:.1} ms", r.pior_no_silencio_us / 1000.0);
        assert!(r.pior_no_silencio_us > 80_000.0, "{}", r.pior_no_silencio_us);
    }

    // --- 7. fora do alcance ---

    #[test]
    fn fora_do_alcance_o_socorro_age_sem_carimbo_para_tras() {
        for ppm in [1_500.0, -1_500.0] {
            let c = Cenario::novo(1_200.0, ppm);
            let (r, l) = correr(&c);
            eprintln!("{ppm} ppm: {} degrau(s), pior {:.1} ms", l.degraus, r.pior_us / 1000.0);
            assert!(l.degraus > 3, "{ppm}: {}", l.degraus);
            assert!(r.pior_us < 41_500.0, "{ppm}: {}", r.pior_us);
            assert_eq!(r.carimbos_para_tras, 0);
        }
    }

    #[test]
    fn pacote_sem_hora_nao_mexe() {
        let mut c = Cenario::novo(30.0, 0.0);
        c.sem_hora = vec![700, 701, 1_500];
        let (r, l) = correr(&c);
        assert_eq!(l.pacotes_sem_hora, 3);
        assert_eq!(l.amostras_de_buraco + l.amostras_de_silencio + l.amostras_cortadas, 0);
        assert!(r.final_us.abs() < 100.0);
    }

    // --- 13. a testemunha do Dell: a série gravada do IAudioClock pela peça de produto ---

    /// Com `QUALL_SERIE_DO_DELL=<relogio-do-dispositivo.csv>`, cada leitura nova de (posição, QPC) do
    /// render em silêncio vira um pacote: a hora é o QPC, a posição é a do dispositivo. O `f` tem de
    /// dar a deriva que a reta independente mede. **O sinal**: a análise mede a reta de `H − D` (a
    /// hora menos o tempo do dispositivo), que inclina −3,69 ppm — o dispositivo anda 3,69 ppm **mais
    /// depressa** que o QPC. O `f` da linha é amostras de entrada por saída menos um: +3,69 ppm. Sem a
    /// variável, não roda.
    #[test]
    fn a_serie_do_dell_da_a_deriva_do_dell() {
        let Ok(caminho) = std::env::var("QUALL_SERIE_DO_DELL") else {
            eprintln!("sem QUALL_SERIE_DO_DELL: pulado");
            return;
        };
        let texto = std::fs::read_to_string(caminho).expect("a série");
        let mut l = LinhaDoLoopback::nova(48_000, true, true);
        let mut ultimo: Option<(u64, u64)> = None;
        let mut n = 0u64;
        let mut eps_final = Vec::new();
        for linha in texto.lines().skip(1) {
            let c: Vec<u64> = linha.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if c.len() < 4 || c[1] == 0 {
                continue;
            }
            let (pos, freq, qpc) = (c[1], c[2], c[3]);
            let quadros = pos * 48_000 / freq;
            if let Some((q0, h0)) = ultimo {
                if quadros <= q0 {
                    continue;
                }
                l.pacote(Pacote { amostras: &[], quadros: quadros - q0, hora_us: Some(h0), posicao: Some(q0), descontinuidade: false });
                n += 1;
                if n > 30_000 {
                    eps_final.push(l.desvio_us);
                }
                while l.proximo_quadro().is_some() {}
            }
            ultimo = Some((quadros, qpc / 10));
        }
        let pior = eps_final.iter().fold(0.0f64, |a, b| a.max(b.abs()));
        eprintln!("série do Dell: {n} pacotes, f {:.2} ppm, |ε| depois de 30 000 pacotes ≤ {pior:.1} µs", l.f_ppm());
        assert!((l.f_ppm() - 3.69).abs() < 0.3, "{}", l.f_ppm());
        assert!(pior < 100.0, "{pior}");
    }

    // --- os ajudantes ---

    #[test]
    fn hora_do_pacote_recusa_a_bandeira_de_erro_e_a_hora_zero() {
        const ERRO: u32 = 0x4; // AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR
        assert_eq!(hora_do_pacote(0, ERRO, 123_456_789_0), Some(123_456_789));
        assert_eq!(hora_do_pacote(0x1, ERRO, 123_456_789_0), Some(123_456_789));
        assert_eq!(hora_do_pacote(ERRO, ERRO, 123_456_789_0), None);
        assert_eq!(hora_do_pacote(0, ERRO, 0), None);
    }

    #[test]
    fn na_origem_leva_o_carimbo_do_qpc_a_origem_da_sessao() {
        let (o, q) = (1_500_000, 9_000_000_000);
        assert_eq!(na_origem(o, q, q + 30_000), 1_530_000);
        assert_eq!(na_origem(o, q, q - 5_000), 1_495_000);
        assert_eq!(na_origem(1_000, q, q - 5_000), 0);
    }

    #[test]
    fn com_som_de_verdade_o_quadro_sai_com_o_som() {
        // A linha com convolução (sem "só contar"): um tom entra, o tom sai, 960 por quadro.
        let mut l = LinhaDoLoopback::nova(48_000, true, false);
        let mut hora = T0;
        let mut quadros = Vec::new();
        for k in 0..200u64 {
            let a: Vec<f32> = (0..480).flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * 1_000.0 * (k * 480 + i) as f64 / 48_000.0).sin() as f32 * 0.5;
                [v, v]
            }).collect();
            l.pacote(Pacote { amostras: &a, quadros: 480, hora_us: Some(hora), posicao: Some(k * 480), descontinuidade: false });
            hora += 10_000;
            while let Some(q) = l.proximo_quadro() {
                quadros.push(q);
            }
        }
        assert!(quadros.len() >= 98, "{}", quadros.len());
        for w in quadros.windows(2) {
            assert_eq!(w[1].1 - w[0].1, 20_000);
        }
        let pico = quadros[50].0.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!((pico - 0.5).abs() < 0.01, "{pico}");
    }
}
