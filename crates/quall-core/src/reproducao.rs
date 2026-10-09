//! A reprodução **puxada** pelo relógio do dispositivo de saída.
//!
//! # Por que ela existe ao lado do [`crate::jitter::BufferDeJitter`]
//!
//! O buffer da porta empurrada (`quall_track_on_audio`) entrega um slot a cada pacote que chega.
//! Ele é cadenciado pelas chegadas e é **estruturalmente cego a subconsumo**: numa rajada de
//! rádio os pacotes chegam juntos, a fila escoa, e nada diz que a hora deles passou
//! (`docs/audio.md` §9 e §14). Um DAC de verdade consome 20 ms a cada 20 ms do relógio dele, e
//! passaria fome na espera.
//!
//! Aqui é o contrário: **a casca chama [`ReproducaoPuxada::puxar`] uma vez por slot que entrega
//! ao dispositivo**, na cadência do dispositivo, e o núcleo responde qual slot sai agora. O
//! relógio manda: o slot devido sai sempre, como quadro, convite a FEC ou ocultação. Pacote que
//! chega depois do seu slot é `tarde_demais`. Fila vazia na hora da puxada é **subconsumo**, e
//! agora ele é contado.
//!
//! O núcleo continua sem tocar em PCM e sem ler relógio de dispositivo nenhum. Ele é **chamado
//! no ritmo** do DAC; quem decodifica, reamostra e fala com o dispositivo é a casca.
//!
//! # A política
//!
//! Está escrita em `docs/som-no-receptor.md` §3.3, com os cenários que a revisão adversarial
//! achou. Cada um virou teste neste arquivo:
//! - pacote sempre tarde não deixa o receptor mudo;
//! - o anel transborda;
//! - o motor sobe tarde;
//! - quantum grande.
//!
//! # Tempo real
//!
//! A puxada roda na thread de áudio da casca. Ela **não espera cadeado de ninguém e não aloca**:
//! - **o anel** entre a thread da rede e a do áudio tem blocos de tamanho fixo, alocados na
//!   criação, e índices atômicos. Cada bloco tem o seu `Mutex`, que o protocolo produtor e
//!   consumidor nunca deixa dois lados segurarem ao mesmo tempo, então o `lock` nunca espera. É
//!   o preço de `#![forbid(unsafe_code)]` neste crate;
//! - **o armazém** do consumidor tem 64 posições indexadas por sequência, com os payloads em
//!   blocos fixos;
//! - **o retrato** dos contadores é publicado com `try_lock`: se quem lê estiver com o cadeado,
//!   a puxada publica na seguinte, sem esperar.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::jitter::{depois_de, distancia, Entrega, Politica};
use crate::media::Clock;
use crate::portao::Barreira;
use crate::rtp::QuadroDeAudio;

/// O maior pacote de áudio aceito, em bytes. Opus cabe em 1 275 por quadro (RFC 6716 §3.4) e
/// 20 ms de G.711 são 160. Maior que isto é descartado e contado em `grandes_demais`.
pub const TAMANHO_MAXIMO_DO_PACOTE: usize = 1_500;

/// Blocos do anel entre a rede e o áudio. A 20 ms por pacote, 1,28 s.
pub const CAPACIDADE_DO_ANEL: usize = 64;

/// Posições do armazém do consumidor, indexadas por `sequência mod 64`.
const JANELA: usize = 64;

/// O teto da fila, em slots: 1 s. Um pacote mais adiantado que isto em relação ao próximo slot
/// faz o núcleo pular para perto dele.
pub const TETO_EM_SLOTS: u16 = 50;

/// Puxadas seguidas sem pacote utilizável até o núcleo desistir do fluxo e ficar ocioso.
pub const PUXADAS_ATE_OCIOSO: u32 = 10;

/// O teto de k, a profundidade efetiva: metade do teto da fila, 500 ms. Sem teto, uma rajada de
/// recuperação de 60 puxadas punha k acima do teto da fila, e a regra do teto passava a disparar
/// sobre o fluxo normal (revisão do código da S1, achado A1).
pub const TETO_DE_K: u16 = TETO_EM_SLOTS / 2;

/// Puxadas a menos de 5 ms uma da outra são da mesma rajada (o mesmo callback de áudio).
const RAJADA_US: u64 = 5_000;

/// O menor intervalo sem puxada que conta como "a casca parou de puxar".
const SALTO_MINIMO_US: u64 = 100_000;

/// Quantos intervalos entre rajadas a medida lembra.
const RAJADAS_LEMBRADAS: usize = 32;

/// Quantos tamanhos de rajada entram na estatística da rajada.
const TAMANHOS_LEMBRADOS: usize = 8;

/// A rajada é o **terceiro maior** tamanho entre os últimos [`TAMANHOS_LEMBRADOS`]: uma rajada
/// avulsa (a casca que recupera atraso, ou que pré-enche a fila) não muda k; um quantum de
/// verdade se repete em toda rajada e muda. Revisão do código da S1, achados A1 e A4.
const POSICAO_DA_RAJADA: usize = 2;

/// **k sobe na hora e só desce depois de tanto tempo querendo menos.** Sem essa espera, um
/// quantum cuja rajada maior aparece em ~25 % a 37 % dos callbacks (4 096 quadros a 48 kHz:
/// rajadas de 4 e 5) punha o terceiro maior subindo e descendo a cada ~0,64 s, e cada volta
/// custava uma inserção e um descarte (reconferência da S1, N1: 939 e 936 em 10 min). Com a
/// espera, a oscilação fica limitada a uma volta a cada 10 s no pior caso, e um quantum de
/// verdade que diminuiu devolve a latência 10 s depois.
///
/// O máximo das 8 rajadas também zerava o caso de 4 096, mas fazia k oscilar no de 1 024
/// (rajadas de 2 em 1 de cada 15 callbacks: 1 873 mudanças e 6,3 % de ocultação em 5 min) e
/// subia k por um soluço avulso da casca. Foi medido e recusado.
const ESPERA_DA_DESCIDA_US: u64 = 10_000_000;

/// **A primeira ancoragem espera a primeira rajada que conta**, por no máximo este tempo desde a
/// primeira puxada. A primeira rajada não conta (pode ser a casca pré-enchendo a fila), e ancorar
/// antes de conhecer o quantum punha k em 2 numa casca de 8 puxadas por callback: a partida
/// gaguejava, com 23 silêncios nos primeiros 640 ms (reconferência da S1, N2). O prazo é para a
/// casca que nunca faz uma rajada que conte, e que antes tocava.
const ESPERA_DA_PRIMEIRA_RAJADA_US: u64 = 500_000;

/// Puxadas com pacote chegando tarde, em drenagens diferentes, que indicam um **degrau** de
/// trânsito e não uma parada: uma parada entrega os atrasados todos juntos, numa drenagem só.
const DRENAGENS_TARDE_PARA_DEGRAU: u16 = 3;

/// No máximo uma inserção por atraso a cada tantas puxadas.
const PUXADAS_ENTRE_INSERCOES_POR_ATRASO: u16 = 10;

/// Sem nova inserção por tanto tempo, a contagem de drenagens com atraso volta a zero.
const PUXADAS_ATE_ESQUECER_O_ATRASO: u16 = 25;

/// O atraso máximo, em slots, de um pacote tarde que conta como sinal de degrau.
const ATRASO_MAXIMO_DE_DEGRAU: u16 = 4;

/// A média exponencial do nível: α por medida (uma por rajada).
const ALFA: f64 = 0.02;

/// Ganho proporcional do controle de deriva: 250 ppm por slot de erro.
const KP: f64 = 250e-6;

/// Tempo integral do controle de deriva, em segundos.
const TI_S: f64 = 200.0;

/// O limite da razão sugerida: ±500 ppm.
const LIMITE: f64 = 500e-6;

/// Medidas congeladas depois de ancoragem, salto, teto ou reajuste de rajada.
const MEDIDAS_CONGELADAS: u32 = 50;

/// A razão sugerida só é publicada depois de 10 s tocando.
const RAZAO_DEPOIS_US: u64 = 10_000_000;

/// A deriva estimada só é publicada depois de 60 s sem descontinuidade.
const DERIVA_DEPOIS_US: u64 = 60_000_000;

/// O nível fora da faixa por 2 s dispara a reserva (só quando a casca não reamostra).
const ESPERA_DA_RESERVA_US: u64 = 2_000_000;

/// O esquecimento da regressão da deriva, em segundos.
const ESQUECIMENTO_S: f64 = 300.0;

/// Como a reprodução puxada é aberta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpcoesDeReproducao {
    /// A mesma política da porta empurrada: profundidade, duração do slot, FEC, salto máximo.
    pub politica: Politica,
    /// A casca aplica a razão sugerida (Varispeed, `RATEADJUST`, o período da thread do OBS).
    /// Com `true`, o núcleo **nunca** descarta nem insere slot por deriva.
    pub casca_reamostra: bool,
}

/// O que sai de uma puxada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Puxado<'a> {
    /// Um slot do fluxo, como na porta empurrada.
    Slot(Entrega<'a>),
    /// Não há fluxo tocando: escreva **zeros**, não ocultação de perda. É o
    /// `QUALL_AUDIO_ORDER_IDLE` da fronteira C.
    Ocioso,
}

/// Os contadores da reprodução, todos do mesmo instante. Ver `docs/contrato-som-puxado.md` §4,
/// que fixa as chaves do JSON.
///
/// Invariante, conferido por teste:
///
/// ```text
/// puxadas = ociosas + quadros + curas_oferecidas + buracos + subconsumos
///         + insercoes_por_deriva + insercoes_de_rajada + insercoes_por_atraso
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ContadoresDeReproducao {
    pub puxadas: u64,
    pub ociosas: u64,
    pub quadros: u64,
    pub curas_oferecidas: u64,
    /// `SILENCE` com pacote posterior em mãos: perda, ou pacote que ainda vai chegar tarde.
    pub buracos: u64,
    /// `SILENCE` sem nenhum pacote utilizável: o DAC passou fome.
    pub subconsumos: u64,
    pub insercoes_por_deriva: u64,
    pub insercoes_de_rajada: u64,
    /// `SILENCE` inserido porque pacotes passaram a chegar tarde em drenagens seguidas: um
    /// degrau de trânsito. Vale também com a casca reamostrando.
    pub insercoes_por_atraso: u64,
    pub descartes_por_deriva: u64,
    /// Slots descartados porque a rajada de puxadas diminuiu e k desceu.
    pub descartes_de_rajada: u64,
    pub descartes_por_teto: u64,
    pub tarde_demais: u64,
    pub duplicados: u64,
    pub reordenados: u64,
    pub ancoragens: u64,
    pub descartados_na_ancoragem: u64,
    pub entradas_em_ocioso: u64,
    pub transbordos: u64,
    pub perdidos_no_anel: u64,
    pub grandes_demais: u64,
    pub saltos: u64,
    pub slots_saltados: u64,
    /// Slots em mãos a partir do próximo, na última puxada.
    pub nivel: u16,
    /// k = profundidade + rajada − 1, com que a última ancoragem foi feita (ou que cresceu depois).
    pub profundidade_efetiva: u16,
    /// A maior rajada de puxadas observada recentemente.
    pub rajada: u16,
    /// Da chegada do pacote mais novo até a saída dele no DAC, na última ancoragem.
    pub latencia_na_ancoragem_us: Option<i64>,
    pub atraso_ate_o_dac_us: u32,
    pub razao_aplicada: f64,
    pub razao_sugerida: Option<f64>,
    pub deriva_ed_ppm: Option<f64>,
}

impl ContadoresDeReproducao {
    /// A soma que tem de dar `puxadas`.
    pub fn soma_das_ordens(&self) -> u64 {
        self.ociosas
            + self.quadros
            + self.curas_oferecidas
            + self.buracos
            + self.subconsumos
            + self.insercoes_por_deriva
            + self.insercoes_de_rajada
            + self.insercoes_por_atraso
    }
}

// ---------------------------------------------------------------------------------------------
// O anel: a thread da rede escreve, a do áudio lê
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Bloco {
    sequencia: u16,
    timestamp_us: u64,
    chegada_us: u64,
    len: usize,
    dados: Box<[u8; TAMANHO_MAXIMO_DO_PACOTE]>,
}

impl Bloco {
    fn novo() -> Self {
        Bloco {
            sequencia: 0,
            timestamp_us: 0,
            chegada_us: 0,
            len: 0,
            dados: Box::new([0u8; TAMANHO_MAXIMO_DO_PACOTE]),
        }
    }
}

#[derive(Debug)]
struct Anel {
    blocos: Vec<Mutex<Bloco>>,
    /// Quantos blocos já foram escritos, desde sempre. Só o produtor escreve.
    escrita: AtomicUsize,
    /// Quantos blocos já foram lidos, desde sempre. Só o consumidor escreve.
    leitura: AtomicUsize,
    /// O anel encheu desde a última puxada: o consumidor trata como descontinuidade.
    transbordou: AtomicBool,
    perdidos: AtomicU64,
    grandes_demais: AtomicU64,
}

/// O lado da rede. Clone-o para dentro do tratador de áudio da track.
#[derive(Debug, Clone)]
pub struct AlimentadorDeReproducao {
    anel: Arc<Anel>,
}

impl AlimentadorDeReproducao {
    /// Entrega um pacote que chegou. Nunca espera: anel cheio descarta o pacote novo e levanta o
    /// sinal de transbordo, que a próxima puxada trata como descontinuidade.
    ///
    /// **Um produtor só.** A libdatachannel serializa o tratador de uma track, e é daí que isto
    /// é chamado. Dois produtores concorrentes não corrompem memória (o código é seguro), mas
    /// podem perder pacote.
    pub fn entregar(&self, quadro: &QuadroDeAudio<'_>, chegada_us: u64) {
        let anel = &self.anel;
        if quadro.payload.len() > TAMANHO_MAXIMO_DO_PACOTE {
            anel.grandes_demais.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let escrita = anel.escrita.load(Ordering::Relaxed);
        let leitura = anel.leitura.load(Ordering::Acquire);
        if escrita.wrapping_sub(leitura) >= CAPACIDADE_DO_ANEL {
            anel.perdidos.fetch_add(1, Ordering::Relaxed);
            anel.transbordou.store(true, Ordering::Release);
            return;
        }
        // O protocolo garante que o consumidor não está neste bloco: ele só lê blocos abaixo de
        // `escrita`, e este é o bloco `escrita`. O `lock` não espera.
        if let Ok(mut b) = anel.blocos[escrita % CAPACIDADE_DO_ANEL].lock() {
            b.sequencia = quadro.sequencia;
            b.timestamp_us = quadro.timestamp_us;
            b.chegada_us = chegada_us;
            b.len = quadro.payload.len();
            b.dados[..quadro.payload.len()].copy_from_slice(quadro.payload);
        } else {
            return;
        }
        anel.escrita.store(escrita.wrapping_add(1), Ordering::Release);
    }
}

// ---------------------------------------------------------------------------------------------
// O armazém do consumidor: 64 posições por sequência, sem alocação
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
struct Armazem {
    ocupado: [bool; JANELA],
    sequencia: [u16; JANELA],
    timestamp_us: [u64; JANELA],
    chegada_us: [u64; JANELA],
    len: [usize; JANELA],
    /// 64 × 1 500 B, contíguos, alocados uma vez na criação.
    dados: Vec<[u8; TAMANHO_MAXIMO_DO_PACOTE]>,
    mais_novo: Option<u16>,
}

impl Armazem {
    fn novo() -> Self {
        Armazem {
            ocupado: [false; JANELA],
            sequencia: [0; JANELA],
            timestamp_us: [0; JANELA],
            chegada_us: [0; JANELA],
            len: [0; JANELA],
            dados: vec![[0u8; TAMANHO_MAXIMO_DO_PACOTE]; JANELA],
            mais_novo: None,
        }
    }

    fn indice(s: u16) -> usize {
        usize::from(s) % JANELA
    }

    fn tem(&self, s: u16) -> Option<usize> {
        let i = Self::indice(s);
        (self.ocupado[i] && self.sequencia[i] == s).then_some(i)
    }

    fn vazio(&self) -> bool {
        !self.ocupado.iter().any(|o| *o)
    }

    fn limpar(&mut self) -> u64 {
        let n = self.ocupado.iter().filter(|o| **o).count() as u64;
        self.ocupado = [false; JANELA];
        self.mais_novo = None;
        n
    }

    fn guardar(&mut self, b: &Bloco) {
        let i = Self::indice(b.sequencia);
        self.ocupado[i] = true;
        self.sequencia[i] = b.sequencia;
        self.timestamp_us[i] = b.timestamp_us;
        self.chegada_us[i] = b.chegada_us;
        self.len[i] = b.len;
        self.dados[i][..b.len].copy_from_slice(&b.dados[..b.len]);
        if self.mais_novo.is_none_or(|n| depois_de(b.sequencia, n)) {
            self.mais_novo = Some(b.sequencia);
        }
    }

    fn tirar(&mut self, s: u16) {
        if let Some(i) = self.tem(s) {
            self.ocupado[i] = false;
        }
        if self.vazio() {
            self.mais_novo = None;
        }
    }

    /// O mais velho guardado, na ordem serial em relação ao mais novo.
    fn mais_velho(&self) -> Option<u16> {
        let n = self.mais_novo?;
        (0..JANELA)
            .filter(|&i| self.ocupado[i])
            .map(|i| self.sequencia[i])
            .max_by_key(|&s| n.wrapping_sub(s))
    }

    /// Tira tudo o que está antes de `p` e devolve quantos saíram.
    fn descartar_antes_de(&mut self, p: u16) -> u64 {
        let mut n = 0;
        for i in 0..JANELA {
            if self.ocupado[i] && depois_de(p, self.sequencia[i]) {
                self.ocupado[i] = false;
                n += 1;
            }
        }
        if self.vazio() {
            self.mais_novo = None;
        }
        n
    }

    /// O menor guardado em `p` ou depois dele.
    fn primeiro_desde(&self, p: u16) -> Option<u16> {
        (0..JANELA)
            .filter(|&i| {
                self.ocupado[i] && (self.sequencia[i] == p || depois_de(self.sequencia[i], p))
            })
            .map(|i| self.sequencia[i])
            .min_by_key(|&s| s.wrapping_sub(p))
    }

    fn payload(&self, i: usize) -> &[u8] {
        &self.dados[i][..self.len[i]]
    }
}

// ---------------------------------------------------------------------------------------------
// A regressão da deriva, com esquecimento
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct Regressao {
    inicio_us: Option<u64>,
    ultimo_s: f64,
    s0: f64,
    st: f64,
    sy: f64,
    stt: f64,
    sty: f64,
}

impl Regressao {
    fn zerar(&mut self) {
        *self = Regressao::default();
    }

    fn somar(&mut self, agora_us: u64, y: f64) {
        let inicio = *self.inicio_us.get_or_insert(agora_us);
        let t = agora_us.saturating_sub(inicio) as f64 / 1e6;
        let esquecer = (-(t - self.ultimo_s).max(0.0) / ESQUECIMENTO_S).exp();
        self.s0 = self.s0 * esquecer + 1.0;
        self.st = self.st * esquecer + t;
        self.sy = self.sy * esquecer + y;
        self.stt = self.stt * esquecer + t * t;
        self.sty = self.sty * esquecer + t * y;
        self.ultimo_s = t;
    }

    /// A inclinação em unidades de y por segundo, se houver história bastante.
    fn inclinacao(&self, agora_us: u64) -> Option<f64> {
        let inicio = self.inicio_us?;
        if agora_us.saturating_sub(inicio) < DERIVA_DEPOIS_US {
            return None;
        }
        let den = self.s0 * self.stt - self.st * self.st;
        (den.abs() > 1e-9).then(|| (self.s0 * self.sty - self.st * self.sy) / den)
    }
}

// ---------------------------------------------------------------------------------------------
// A reprodução
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Estado {
    Ocioso,
    Tocando { proxima: u16 },
}

/// O que a puxada decidiu, antes de emprestar o payload.
enum Decisao {
    Ocioso,
    Quadro { i: usize, sequencia: u16, timestamp_us: u64 },
    Fec { i: usize, sequencia: u16, timestamp_us: u64 },
    Silencio { sequencia: u16, timestamp_us: u64 },
}

/// A soltura da track: tira o produtor do caminho do pacote, com barreira.
type Soltura = Box<dyn FnOnce() -> Barreira + Send>;

/// O lado da leitura: contadores e razão sugerida, de qualquer thread.
#[derive(Debug, Clone)]
pub struct LeitorDeReproducao {
    retrato: Arc<Mutex<ContadoresDeReproducao>>,
}

impl LeitorDeReproducao {
    /// Os contadores publicados pela última puxada que conseguiu publicar.
    pub fn contadores(&self) -> ContadoresDeReproducao {
        self.retrato.lock().map(|g| *g).unwrap_or_default()
    }

    /// A razão de reamostragem sugerida, ou `None` quando não medida.
    pub fn razao_sugerida(&self) -> Option<f64> {
        self.contadores().razao_sugerida
    }
}

/// O consumidor: quem puxa. Uma thread de cada vez.
pub struct ReproducaoPuxada {
    opcoes: OpcoesDeReproducao,
    relogio: Arc<Clock>,
    anel: Arc<Anel>,
    armazem: Armazem,
    estado: Estado,
    c: ContadoresDeReproducao,
    retrato: Arc<Mutex<ContadoresDeReproducao>>,
    soltura: Option<Soltura>,

    // --- as rajadas de puxadas --------------------------------------------------------------
    ultima_puxada_us: Option<u64>,
    primeira_puxada_us: Option<u64>,
    rajada_atual: u16,
    /// A rajada em curso começou depois de um salto, ou é a primeira: não entra na estatística.
    /// A casca que recupera atraso de uma vez, e a que pré-enche a fila na partida, não dizem o
    /// quantum dela.
    rajada_depois_de_salto: bool,
    tamanhos: [u16; TAMANHOS_LEMBRADOS],
    proximo_tamanho: usize,
    intervalos: [u64; RAJADAS_LEMBRADAS],
    proximo_intervalo: usize,

    // --- a reprodução ------------------------------------------------------------------------
    /// k com que a reprodução está tocando.
    k: u16,
    /// Desde quando o k desejado está abaixo de `k`, e o maior k desejado nesse tempo. Ver
    /// [`ESPERA_DA_DESCIDA_US`].
    descida_desde_us: Option<u64>,
    k_na_espera: u16,
    sem_pacote: u32,
    pendentes_de_rajada: u16,
    pendentes_de_deriva: u16,
    /// Slots a descartar porque k desceu. Só descarta quando há folga (nível acima de k + 1).
    pendentes_de_descida: u16,
    pendentes_de_atraso: u16,
    /// Pacote tarde chegou nesta drenagem?
    tarde_nesta_drenagem: bool,
    drenagens_com_tarde: u16,
    /// Puxadas desde a primeira drenagem com atraso da contagem em curso.
    janela_do_atraso: u16,
    puxadas_desde_insercao_por_atraso: u16,
    /// `(sequência, carimbo)` do último slot com carimbo observado, para interpolar.
    ultimo_carimbo: Option<(u16, u64)>,
    tocando_desde_us: Option<u64>,
    ja_tocou: bool,

    // --- a deriva ----------------------------------------------------------------------------
    media_do_nivel: Option<f64>,
    ultima_medida_us: Option<u64>,
    integral: f64,
    razao: f64,
    congeladas: u32,
    acima_desde_us: Option<u64>,
    abaixo_desde_us: Option<u64>,
    ajuste_do_nivel: f64,
    consumo_a_mais: f64,
    regressao: Regressao,
}

impl std::fmt::Debug for ReproducaoPuxada {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReproducaoPuxada")
            .field("estado", &self.estado)
            .field("contadores", &self.c)
            .finish_non_exhaustive()
    }
}

impl ReproducaoPuxada {
    /// Cria o par produtor e consumidor. `relogio` é o relógio de chegada da sessão: o mesmo que
    /// carimba `chegada_us` em [`AlimentadorDeReproducao::entregar`].
    pub fn nova(
        opcoes: OpcoesDeReproducao,
        relogio: Arc<Clock>,
    ) -> (AlimentadorDeReproducao, ReproducaoPuxada) {
        let anel = Arc::new(Anel {
            blocos: (0..CAPACIDADE_DO_ANEL).map(|_| Mutex::new(Bloco::novo())).collect(),
            escrita: AtomicUsize::new(0),
            leitura: AtomicUsize::new(0),
            transbordou: AtomicBool::new(false),
            perdidos: AtomicU64::new(0),
            grandes_demais: AtomicU64::new(0),
        });
        let c = ContadoresDeReproducao {
            razao_aplicada: 1.0,
            ..ContadoresDeReproducao::default()
        };
        let r = ReproducaoPuxada {
            opcoes,
            relogio,
            anel: Arc::clone(&anel),
            armazem: Armazem::novo(),
            estado: Estado::Ocioso,
            c,
            retrato: Arc::new(Mutex::new(c)),
            soltura: None,
            ultima_puxada_us: None,
            primeira_puxada_us: None,
            rajada_atual: 0,
            rajada_depois_de_salto: true,
            tamanhos: [1; TAMANHOS_LEMBRADOS],
            proximo_tamanho: 0,
            intervalos: [0; RAJADAS_LEMBRADAS],
            proximo_intervalo: 0,
            k: opcoes.politica.profundidade,
            descida_desde_us: None,
            k_na_espera: opcoes.politica.profundidade,
            sem_pacote: 0,
            pendentes_de_rajada: 0,
            pendentes_de_deriva: 0,
            pendentes_de_descida: 0,
            pendentes_de_atraso: 0,
            tarde_nesta_drenagem: false,
            drenagens_com_tarde: 0,
            janela_do_atraso: 0,
            puxadas_desde_insercao_por_atraso: PUXADAS_ENTRE_INSERCOES_POR_ATRASO,
            ultimo_carimbo: None,
            tocando_desde_us: None,
            ja_tocou: false,
            media_do_nivel: None,
            ultima_medida_us: None,
            integral: 0.0,
            razao: 0.0,
            congeladas: 0,
            acima_desde_us: None,
            abaixo_desde_us: None,
            ajuste_do_nivel: 0.0,
            consumo_a_mais: 0.0,
            regressao: Regressao::default(),
        };
        // **Aciona cada cadeado uma vez, aqui.** No Mac e no iOS o `Mutex` da std aloca na
        // primeira trava, e a primeira puxada alocava por isso (revisão do código da S1, A7).
        // Criar a reprodução não é tempo real; puxar é.
        drop(r.retrato.lock());
        drop(r.retrato.try_lock());
        for b in &anel.blocos {
            drop(b.lock());
        }
        (AlimentadorDeReproducao { anel }, r)
    }

    /// Prende a soltura da track: o que roda no [`ReproducaoPuxada::encerrar`] e no `Drop`.
    #[cfg_attr(not(feature = "webrtc"), allow(dead_code))]
    pub(crate) fn com_soltura(mut self, soltura: Soltura) -> Self {
        self.soltura = Some(soltura);
        self
    }

    /// O lado da leitura, para qualquer thread.
    pub fn leitor(&self) -> LeitorDeReproducao {
        LeitorDeReproducao {
            retrato: Arc::clone(&self.retrato),
        }
    }

    /// Encerra: tira o produtor do caminho do pacote e espera a barreira.
    pub fn encerrar(mut self) -> Barreira {
        match self.soltura.take() {
            Some(soltar) => soltar(),
            None => Barreira::Cumprida,
        }
    }

    /// Puxa o slot que sai agora, lendo a hora do relógio da sessão.
    ///
    /// `atraso_ate_o_dac_us`: daqui a quanto a primeira amostra deste slot sai do DAC.
    /// `razao_aplicada`: a razão de reamostragem que a casca de fato aplicou; `NAN` quando não
    /// sabe ou não reamostra.
    pub fn puxar(&mut self, atraso_ate_o_dac_us: u32, razao_aplicada: f64) -> Puxado<'_> {
        let agora = self.relogio.micros();
        self.puxar_em(agora, atraso_ate_o_dac_us, razao_aplicada)
    }

    /// O mesmo, com a hora dada. Para teste, e para quem simula.
    pub fn puxar_em(
        &mut self,
        agora_us: u64,
        atraso_ate_o_dac_us: u32,
        razao_aplicada: f64,
    ) -> Puxado<'_> {
        let decisao = self.decidir(agora_us, atraso_ate_o_dac_us, razao_aplicada);
        self.publicar();
        match decisao {
            Decisao::Ocioso => Puxado::Ocioso,
            Decisao::Quadro {
                i,
                sequencia,
                timestamp_us,
            } => Puxado::Slot(Entrega::Quadro {
                payload: self.armazem.payload(i),
                sequencia,
                timestamp_us,
            }),
            Decisao::Fec {
                i,
                sequencia,
                timestamp_us,
            } => Puxado::Slot(Entrega::Fec {
                socorro: self.armazem.payload(i),
                sequencia,
                timestamp_us,
            }),
            Decisao::Silencio {
                sequencia,
                timestamp_us,
            } => Puxado::Slot(Entrega::Silencio {
                sequencia,
                timestamp_us,
            }),
        }
    }

    fn publicar(&mut self) {
        self.c.perdidos_no_anel = self.anel.perdidos.load(Ordering::Relaxed);
        self.c.grandes_demais = self.anel.grandes_demais.load(Ordering::Relaxed);
        if let Ok(mut g) = self.retrato.try_lock() {
            *g = self.c;
        }
    }

    fn duracao_us(&self) -> u64 {
        u64::from(self.opcoes.politica.duracao_do_quadro_us)
    }

    /// A rajada de puxadas: o terceiro maior tamanho entre as últimas rajadas que contam. Uma
    /// rajada avulsa não muda k; um quantum que se repete muda.
    ///
    /// Com menos de três rajadas medidas, é a maior delas: na partida, faltar folga custa
    /// silêncio, e sobrar custa latência que a espera da descida devolve.
    fn rajada(&self) -> u16 {
        let mut t = self.tamanhos;
        t.sort_unstable_by(|a, b| b.cmp(a));
        let medidas = self.proximo_tamanho.min(TAMANHOS_LEMBRADOS);
        t[POSICAO_DA_RAJADA.min(medidas.saturating_sub(1))].max(1)
    }

    /// k: a profundidade mais a rajada, com teto de [`TETO_DE_K`].
    fn k_desejado(&self) -> u16 {
        (self.opcoes.politica.profundidade + self.rajada() - 1).min(TETO_DE_K)
    }

    /// O k a seguir agora: o desejado quando ele sobe; o atual enquanto o desejado estiver abaixo
    /// há menos de [`ESPERA_DA_DESCIDA_US`]; depois disso, o maior desejado da espera.
    fn alvo_de_k(&mut self, agora_us: u64) -> u16 {
        let desejado = self.k_desejado();
        if desejado >= self.k {
            self.descida_desde_us = None;
            return desejado;
        }
        match self.descida_desde_us {
            None => {
                self.descida_desde_us = Some(agora_us);
                self.k_na_espera = desejado;
                self.k
            }
            Some(desde) => {
                self.k_na_espera = self.k_na_espera.max(desejado);
                if agora_us.saturating_sub(desde) >= ESPERA_DA_DESCIDA_US {
                    self.descida_desde_us = None;
                    self.k_na_espera
                } else {
                    self.k
                }
            }
        }
    }

    /// Acima deste intervalo sem puxada, a casca parou de puxar.
    ///
    /// Leva em conta o tamanho da rajada que acabou: uma casca que puxou 8 slots de uma vez
    /// (160 ms de som) vai ficar 160 ms sem puxar, e isso é o quantum dela, não uma parada.
    /// Antes, o limiar era só o maior intervalo lembrado, e um intervalo classificado como salto
    /// não era lembrado: um quantum acima de 100 ms virava salto em toda rajada, para sempre
    /// (revisão do código da S1, A3).
    fn limiar_do_salto_us(&self, rajada_que_acabou: u16) -> u64 {
        let maior = self.intervalos.iter().copied().max().unwrap_or(0);
        SALTO_MINIMO_US
            .max(2 * maior)
            .max(2 * u64::from(rajada_que_acabou) * self.duracao_us())
    }

    fn congelar(&mut self) {
        self.congeladas = MEDIDAS_CONGELADAS;
        self.acima_desde_us = None;
        self.abaixo_desde_us = None;
        self.regressao.zerar();
        self.ajuste_do_nivel = 0.0;
        self.consumo_a_mais = 0.0;
    }

    fn entrar_em_ocioso(&mut self) {
        if matches!(self.estado, Estado::Tocando { .. }) {
            self.c.entradas_em_ocioso += 1;
        }
        self.estado = Estado::Ocioso;
        self.armazem.limpar();
        self.pendentes_de_rajada = 0;
        self.pendentes_de_deriva = 0;
        self.pendentes_de_descida = 0;
        self.pendentes_de_atraso = 0;
        self.drenagens_com_tarde = 0;
        self.tocando_desde_us = None;
        self.sem_pacote = 0;
        self.media_do_nivel = None;
        self.congelar();
    }

    /// Esvazia o anel para dentro do armazém.
    fn drenar(&mut self) {
        if self.anel.transbordou.swap(false, Ordering::AcqRel) {
            // O anel ficou cheio de pacote velho enquanto ninguém puxava, e os novos foram
            // jogados fora. Guardar os velhos seria latência que não volta: descarta tudo e
            // reancora pelos que vierem.
            self.c.transbordos += 1;
            let escrita = self.anel.escrita.load(Ordering::Acquire);
            self.anel.leitura.store(escrita, Ordering::Release);
            self.entrar_em_ocioso();
            return;
        }
        let escrita = self.anel.escrita.load(Ordering::Acquire);
        let mut leitura = self.anel.leitura.load(Ordering::Relaxed);
        self.tarde_nesta_drenagem = false;
        while leitura != escrita {
            let bloco = Arc::clone(&self.anel);
            if let Ok(b) = bloco.blocos[leitura % CAPACIDADE_DO_ANEL].lock() {
                self.aceitar(&b);
            }
            leitura = leitura.wrapping_add(1);
            self.anel.leitura.store(leitura, Ordering::Release);
        }
        if self.tarde_nesta_drenagem {
            self.drenagens_com_tarde = self.drenagens_com_tarde.saturating_add(1);
        }
    }

    fn aceitar(&mut self, b: &Bloco) {
        let s = b.sequencia;
        if let Estado::Tocando { proxima } = self.estado {
            if !(s == proxima || depois_de(s, proxima)) {
                self.c.tarde_demais += 1;
                if proxima.wrapping_sub(s) <= ATRASO_MAXIMO_DE_DEGRAU {
                    self.tarde_nesta_drenagem = true;
                }
                return;
            }
            let d = s.wrapping_sub(proxima);
            if d > self.opcoes.politica.salto_maximo {
                // Emissor que reiniciou, ou uma parada muito longa: não é perda, é outro fluxo.
                self.entrar_em_ocioso();
            } else if d >= TETO_EM_SLOTS {
                // O teto **só move `proxima` para a frente**. Com k abaixo de `TETO_DE_K` e
                // `d ≥ TETO_EM_SLOTS`, `nova` está sempre à frente; a conferência fica porque,
                // sem ela, um k grande punha `nova` atrás e o `u16` da contagem dava a volta
                // (65 572 descartes, revisão do código da S1, A1).
                let nova = s.wrapping_sub(self.k);
                if depois_de(nova, proxima) {
                    self.armazem.descartar_antes_de(nova);
                    self.c.descartes_por_teto += u64::from(nova.wrapping_sub(proxima));
                    self.estado = Estado::Tocando { proxima: nova };
                    self.congelar();
                }
            }
        }
        // Ocioso, ou tocando e dentro da janela.
        if let Some(n) = self.armazem.mais_novo {
            let adiante = s.wrapping_sub(n);
            let atras = n.wrapping_sub(s);
            if depois_de(s, n) && usize::from(adiante) >= JANELA {
                self.c.descartados_na_ancoragem += self.armazem.limpar();
            } else if depois_de(n, s) && usize::from(atras) >= JANELA {
                self.c.descartados_na_ancoragem += 1;
                return;
            }
        }
        if self.armazem.tem(s).is_some() {
            self.c.duplicados += 1;
            return;
        }
        if self.armazem.mais_novo.is_some_and(|n| depois_de(n, s)) {
            self.c.reordenados += 1;
        }
        self.armazem.guardar(b);
        // Mantém a janela livre de colisão: nada a 64 ou mais atrás do mais novo.
        if let Some(n) = self.armazem.mais_novo {
            let limite = n.wrapping_sub(JANELA as u16 - 1);
            self.c.descartados_na_ancoragem += self.armazem.descartar_antes_de(limite);
        }
    }

    /// Mede as rajadas. Devolve se esta é a primeira puxada de uma rajada, e se o intervalo
    /// antes dela foi um **salto** (a casca parou de puxar).
    fn medir_rajada(&mut self, agora_us: u64) -> (bool, bool) {
        let Some(ultima) = self.ultima_puxada_us else {
            self.ultima_puxada_us = Some(agora_us);
            self.primeira_puxada_us = Some(agora_us);
            self.rajada_atual = 1;
            // A primeira rajada não conta: a casca que pré-enche a fila na partida puxa várias
            // de uma vez e depois uma a cada slot (revisão do código da S1, A4).
            self.rajada_depois_de_salto = true;
            return (true, false);
        };
        let dt = agora_us.saturating_sub(ultima);
        self.ultima_puxada_us = Some(agora_us);
        if dt < RAJADA_US {
            self.rajada_atual = self.rajada_atual.saturating_add(1);
            return (false, false);
        }
        // Fecha a rajada anterior.
        let fechada = self.rajada_atual.max(1);
        let salto = dt > self.limiar_do_salto_us(fechada);
        if !self.rajada_depois_de_salto {
            self.tamanhos[self.proximo_tamanho % TAMANHOS_LEMBRADOS] = fechada;
            self.proximo_tamanho += 1;
        }
        // O intervalo só entra se não for um salto: a pausa da casca não é o ritmo dela.
        if !salto {
            self.intervalos[self.proximo_intervalo % RAJADAS_LEMBRADAS] = dt;
            self.proximo_intervalo += 1;
        }
        self.rajada_atual = 1;
        // A rajada que começa depois de um salto é a casca recuperando o atraso de uma vez
        // (revisão do código da S1, A1): não diz o quantum dela.
        self.rajada_depois_de_salto = salto;
        (true, salto)
    }

    /// k mudou de desejo: sobe por inserção, na hora; desce por descarte quando houver folga,
    /// depois da espera de [`ESPERA_DA_DESCIDA_US`].
    fn reajustar_k(&mut self, agora_us: u64) {
        let k = self.alvo_de_k(agora_us);
        if k > self.k {
            let mais = k - self.k;
            let cancelados = mais.min(self.pendentes_de_descida);
            self.pendentes_de_descida -= cancelados;
            self.pendentes_de_rajada += mais - cancelados;
        } else if k < self.k {
            let menos = self.k - k;
            let cancelados = menos.min(self.pendentes_de_rajada);
            self.pendentes_de_rajada -= cancelados;
            self.pendentes_de_descida += menos - cancelados;
        } else {
            return;
        }
        self.k = k;
        self.c.profundidade_efetiva = k;
        self.congelar();
    }

    /// O degrau de trânsito: pacotes chegando tarde em drenagens diferentes pedem um slot a mais
    /// de folga, **mesmo com a casca reamostrando**, porque isso é degrau e não deriva (revisão do
    /// código da S1, A6). Uma parada de rede entrega os atrasados numa drenagem só e não conta.
    fn vigiar_atraso(&mut self) {
        self.puxadas_desde_insercao_por_atraso =
            self.puxadas_desde_insercao_por_atraso.saturating_add(1);
        if self.drenagens_com_tarde == 0 {
            self.janela_do_atraso = 0;
            return;
        }
        self.janela_do_atraso = self.janela_do_atraso.saturating_add(1);
        if self.drenagens_com_tarde >= DRENAGENS_TARDE_PARA_DEGRAU
            && self.puxadas_desde_insercao_por_atraso >= PUXADAS_ENTRE_INSERCOES_POR_ATRASO
        {
            self.pendentes_de_atraso += 1;
            self.drenagens_com_tarde = 0;
            self.janela_do_atraso = 0;
            self.puxadas_desde_insercao_por_atraso = 0;
        } else if self.janela_do_atraso > PUXADAS_ATE_ESQUECER_O_ATRASO {
            // Atrasados esparsos, mais de meio segundo entre eles: é jitter, não degrau.
            self.drenagens_com_tarde = 0;
            self.janela_do_atraso = 0;
        }
    }

    fn interpolar(&self, s: u16) -> u64 {
        let dur = self.duracao_us();
        if let Some(p) = self.armazem.primeiro_desde(s) {
            if let Some(i) = self.armazem.tem(p) {
                let d = u64::from(p.wrapping_sub(s));
                return self.armazem.timestamp_us[i].saturating_sub(d * dur);
            }
        }
        match self.ultimo_carimbo {
            Some((u, t)) => t + u64::from(s.wrapping_sub(u)) * dur,
            None => 0,
        }
    }

    fn decidir(&mut self, agora_us: u64, atraso_ate_o_dac_us: u32, razao_aplicada: f64) -> Decisao {
        self.c.puxadas += 1;
        self.c.atraso_ate_o_dac_us = atraso_ate_o_dac_us;
        let aplicada = if razao_aplicada.is_finite() && razao_aplicada > 0.0 {
            razao_aplicada
        } else {
            1.0
        };
        self.c.razao_aplicada = aplicada;

        let (primeira_da_rajada, salto) = self.medir_rajada(agora_us);
        self.c.rajada = self.rajada();
        self.drenar();

        // A casca parou de puxar: pula para perto do mais novo em vez de tocar o atraso inteiro.
        if let (Estado::Tocando { proxima }, true) = (self.estado, salto) {
            if let Some(n) = self.armazem.mais_novo {
                let alvo = n.wrapping_sub(self.k);
                if depois_de(alvo, proxima) {
                    self.armazem.descartar_antes_de(alvo);
                    self.c.saltos += 1;
                    self.c.slots_saltados += u64::from(alvo.wrapping_sub(proxima));
                    self.estado = Estado::Tocando { proxima: alvo };
                    self.congelar();
                }
            }
        }

        if self.estado == Estado::Ocioso && !self.ancorar(agora_us, atraso_ate_o_dac_us) {
            self.c.ociosas += 1;
            self.c.nivel = 0;
            self.c.razao_sugerida = None;
            self.c.deriva_ed_ppm = None;
            return Decisao::Ocioso;
        }
        // A rajada de puxadas mudou: a folga cresce por inserção e desce por descarte.
        self.reajustar_k(agora_us);
        self.vigiar_atraso();

        // A medida pode descartar um slot pela reserva, então `proxima` é lida depois dela.
        if primeira_da_rajada {
            self.medir_nivel(agora_us, aplicada);
        }
        let Estado::Tocando { proxima } = self.estado else {
            self.c.ociosas += 1;
            return Decisao::Ocioso;
        };

        // k desceu: descarta **um** slot por puxada, e só quando há folga acima de k + 1.
        let proxima = match (self.pendentes_de_descida, self.armazem.mais_novo) {
            (1.., Some(n))
                if depois_de(n, proxima) && n.wrapping_sub(proxima) + 1 > self.k + 1 =>
            {
                self.pendentes_de_descida -= 1;
                self.armazem.tirar(proxima);
                self.c.descartes_de_rajada += 1;
                // Os slots seguintes saem um slot mais cedo: não é deriva (§21).
                self.ajuste_do_nivel += 1.0;
                let seguinte = proxima.wrapping_add(1);
                self.estado = Estado::Tocando { proxima: seguinte };
                seguinte
            }
            _ => proxima,
        };

        if self.pendentes_de_atraso > 0 {
            self.pendentes_de_atraso -= 1;
            self.c.insercoes_por_atraso += 1;
            // A inserção por atraso responde a um **degrau de trânsito**, que baixa a latência de todo
            // slot seguinte pelo tamanho do degrau; ajustar só o slot inserido deixaria o degrau
            // inteiro na regressão (revisão do código, M1: −536 ppm contra −383 sem ajuste). Um
            // degrau não é deriva: a regressão recomeça (`docs/som-no-receptor.md` §21).
            self.congelar();
            return Decisao::Silencio {
                sequencia: proxima,
                timestamp_us: self.interpolar(proxima),
            };
        }
        if self.pendentes_de_rajada > 0 {
            self.pendentes_de_rajada -= 1;
            self.c.insercoes_de_rajada += 1;
            self.ajuste_do_nivel -= 1.0;
            return Decisao::Silencio {
                sequencia: proxima,
                timestamp_us: self.interpolar(proxima),
            };
        }
        if self.pendentes_de_deriva > 0 {
            self.pendentes_de_deriva -= 1;
            self.c.insercoes_por_deriva += 1;
            return Decisao::Silencio {
                sequencia: proxima,
                timestamp_us: self.interpolar(proxima),
            };
        }

        let seguinte = proxima.wrapping_add(1);
        if let Some(i) = self.armazem.tem(proxima) {
            let timestamp_us = self.armazem.timestamp_us[i];
            let chegada_us = self.armazem.chegada_us[i];
            self.armazem.ocupado[i] = false;
            self.c.quadros += 1;
            self.ultimo_carimbo = Some((proxima, timestamp_us));
            self.sem_pacote = 0;
            self.amostrar_a_latencia(agora_us, atraso_ate_o_dac_us, chegada_us);
            self.estado = Estado::Tocando { proxima: seguinte };
            return Decisao::Quadro {
                i,
                sequencia: proxima,
                timestamp_us,
            };
        }
        let timestamp_us = self.interpolar(proxima);
        self.estado = Estado::Tocando { proxima: seguinte };
        if self.armazem.primeiro_desde(seguinte).is_some() {
            self.sem_pacote = 0;
            if self.opcoes.politica.fec_disponivel {
                if let Some(i) = self.armazem.tem(seguinte) {
                    self.c.curas_oferecidas += 1;
                    self.ultimo_carimbo = Some((proxima, timestamp_us));
                    return Decisao::Fec {
                        i,
                        sequencia: proxima,
                        timestamp_us,
                    };
                }
            }
            self.c.buracos += 1;
            return Decisao::Silencio {
                sequencia: proxima,
                timestamp_us,
            };
        }
        // Nada utilizável: o DAC passou fome.
        self.c.subconsumos += 1;
        self.sem_pacote += 1;
        if self.sem_pacote >= PUXADAS_ATE_OCIOSO {
            self.entrar_em_ocioso();
        }
        Decisao::Silencio {
            sequencia: proxima,
            timestamp_us,
        }
    }

    /// No ocioso: ancora pelo mais novo menos k, se houver folga bastante. Devolve se ancorou.
    fn ancorar(&mut self, agora_us: u64, atraso_ate_o_dac_us: u32) -> bool {
        // A espera da descida corre também no ocioso: a casca continua puxando.
        let k = self.alvo_de_k(agora_us);
        let (Some(n), Some(v)) = (self.armazem.mais_novo, self.armazem.mais_velho()) else {
            return false;
        };
        let conhece_o_quantum = self.proximo_tamanho > 0
            || self
                .primeira_puxada_us
                .is_some_and(|p| agora_us.saturating_sub(p) >= ESPERA_DA_PRIMEIRA_RAJADA_US);
        if !conhece_o_quantum || distancia(n, v).unwrap_or(0) < k {
            return false;
        }
        let proxima = n.wrapping_sub(k);
        self.c.descartados_na_ancoragem += self.armazem.descartar_antes_de(proxima);
        self.c.ancoragens += 1;
        self.k = k;
        self.c.profundidade_efetiva = k;
        self.estado = Estado::Tocando { proxima };
        self.sem_pacote = 0;
        self.pendentes_de_rajada = 0;
        self.pendentes_de_deriva = 0;
        self.pendentes_de_descida = 0;
        self.pendentes_de_atraso = 0;
        self.drenagens_com_tarde = 0;
        self.ultimo_carimbo = None;
        if let Some(i) = self.armazem.tem(n) {
            let sai_no_dac = agora_us
                + u64::from(atraso_ate_o_dac_us)
                + u64::from(k) * self.duracao_us();
            self.c.latencia_na_ancoragem_us =
                Some(sai_no_dac as i64 - self.armazem.chegada_us[i] as i64);
        }
        self.tocando_desde_us = Some(agora_us);
        if self.ja_tocou {
            // Reancoragem depois do ocioso: o fluxo pode ser outro, e a deriva aprendida também.
            self.integral = 0.0;
            self.razao = 0.0;
        }
        self.ja_tocou = true;
        self.media_do_nivel = None;
        self.ultima_medida_us = None;
        self.congelar();
        true
    }

    /// Uma medida por rajada: o nível, a média, o controle e a reserva.
    fn medir_nivel(&mut self, agora_us: u64, aplicada: f64) {
        let Estado::Tocando { proxima } = self.estado else {
            return;
        };
        let nivel = match self.armazem.mais_novo {
            Some(n) if n == proxima || depois_de(n, proxima) => n.wrapping_sub(proxima) + 1,
            _ => 0,
        };
        self.c.nivel = nivel;
        let media = match self.media_do_nivel {
            Some(m) => m * (1.0 - ALFA) + ALFA * f64::from(nivel),
            None => f64::from(nivel),
        };
        self.media_do_nivel = Some(media);
        let dt_s = self
            .ultima_medida_us
            .map(|u| agora_us.saturating_sub(u) as f64 / 1e6)
            .unwrap_or(0.0);
        self.ultima_medida_us = Some(agora_us);

        // O consumo a mais que a casca aplicou, em slots: entra na regressão da deriva
        // (`amostrar_a_latencia`) para que a inclinação seja a deriva e não o resíduo dela.
        let slots_por_s = 1e6 / self.duracao_us() as f64;
        self.consumo_a_mais += (aplicada - 1.0) * dt_s * slots_por_s;

        if self.congeladas > 0 {
            self.congeladas -= 1;
        } else {
            let alvo = f64::from(self.k) + 1.0;
            let erro = media - alvo;
            self.integral = (self.integral + KP / TI_S * erro * dt_s).clamp(-LIMITE, LIMITE);
            self.razao = (KP * erro + self.integral).clamp(-LIMITE, LIMITE);

            if !self.opcoes.casca_reamostra {
                self.reserva(agora_us, media, alvo);
            }
        }

        let tocando = self
            .tocando_desde_us
            .map(|t| agora_us.saturating_sub(t))
            .unwrap_or(0);
        self.c.razao_sugerida = (tocando >= RAZAO_DEPOIS_US).then_some(1.0 + self.razao);
        self.c.deriva_ed_ppm = self
            .regressao
            .inclinacao(agora_us)
            .map(|inclinacao| inclinacao / slots_por_s * 1e6);
    }

    /// **A amostra da regressão da deriva**: a latência do slot que sai agora, em slots — da
    /// chegada do pacote até a hora em que a primeira amostra dele sai do DAC (`agora + atraso até
    /// o DAC`, a conta da `latencia_na_ancoragem_us`, feita a cada quadro tocado) —, mais o
    /// ajuste dos slots que o núcleo descartou ou inseriu e o consumo a mais que a casca aplicou.
    ///
    /// # Por que a latência, e não o nível (rodada de 22/09, `docs/som-no-receptor.md` §21)
    ///
    /// Até aqui a amostra era o **nível inteiro** (`mais_novo − proxima + 1`) uma vez por rajada. O
    /// nível contínuo anda com a deriva; o inteiro é o piso dele, e o resto (0 a 1 slot) depende da
    /// fase entre chegada e puxada, que o PI move enquanto leva a média ao alvo. Numa corrida de
    /// 120 s, um resto que anda uma fração de slot entorta a inclinação em até ~170 ppm. Medido na
    /// simulação da casca do Mac, com uma casca exata: de −94 a +27 ppm conforme a fase da chegada
    /// (o controle 6 no Mac leu +19, dentro disso), e o mesmo número fase a fase por um oráculo com
    /// o nível inteiro; com a latência de cada slot tocado, de −0,1 a 0,0.
    ///
    /// A latência é contínua dos dois lados: a chegada é a hora do pacote, e a saída leva o que
    /// ainda toca antes do slot no ciclo da casca (o atraso que ela informa é por puxada). **Depende
    /// disso**: uma casca que informasse atraso constante volta a ter o resto do lado da puxada.
    /// Um degrau no atraso declarado (troca de saída sem pausa) entra como degrau; com pausa de
    /// 100 ms ou mais é salto, e o salto congela.
    fn amostrar_a_latencia(&mut self, agora_us: u64, atraso_ate_o_dac_us: u32, chegada_us: u64) {
        if self.congeladas > 0 || !matches!(self.estado, Estado::Tocando { .. }) {
            return;
        }
        let sai_no_dac = agora_us as f64 + f64::from(atraso_ate_o_dac_us);
        let latencia = (sai_no_dac - chegada_us as f64) / self.duracao_us() as f64;
        let y = latencia + self.ajuste_do_nivel + self.consumo_a_mais;
        self.regressao.somar(agora_us, y);
    }

    /// Só quando a casca não reamostra: descarta ou insere um slot quando a média fica fora de
    /// `[alvo − 1, alvo + 2]` por [`ESPERA_DA_RESERVA_US`].
    fn reserva(&mut self, agora_us: u64, media: f64, alvo: f64) {
        if media > alvo + 2.0 {
            self.abaixo_desde_us = None;
            let desde = *self.acima_desde_us.get_or_insert(agora_us);
            if agora_us.saturating_sub(desde) >= ESPERA_DA_RESERVA_US {
                if let Estado::Tocando { proxima } = self.estado {
                    self.armazem.tirar(proxima);
                    self.estado = Estado::Tocando {
                        proxima: proxima.wrapping_add(1),
                    };
                    self.c.descartes_por_deriva += 1;
                    self.media_do_nivel = Some(media - 1.0);
                    self.ajuste_do_nivel += 1.0;
                }
                self.acima_desde_us = None;
            }
        } else if media < alvo - 1.0 {
            self.acima_desde_us = None;
            let desde = *self.abaixo_desde_us.get_or_insert(agora_us);
            if agora_us.saturating_sub(desde) >= ESPERA_DA_RESERVA_US {
                self.pendentes_de_deriva += 1;
                self.media_do_nivel = Some(media + 1.0);
                self.ajuste_do_nivel -= 1.0;
                self.abaixo_desde_us = None;
            }
        } else {
            self.acima_desde_us = None;
            self.abaixo_desde_us = None;
        }
    }
}

impl Drop for ReproducaoPuxada {
    fn drop(&mut self) {
        if let Some(soltar) = self.soltura.take() {
            let _ = soltar();
        }
    }
}

/// Os cenários da revisão adversarial de `docs/som-no-receptor.md`, como `cargo test`. O
/// simulador intercala chegadas e puxadas pela hora, na ordem em que aconteceriam.
#[cfg(test)]
mod tests {
    use super::*;

    const Q: u64 = 20_000;

    fn opcoes(casca_reamostra: bool) -> OpcoesDeReproducao {
        OpcoesDeReproducao {
            politica: Politica::MICROFONE,
            casca_reamostra,
        }
    }

    /// O que uma puxada devolveu, sem o empréstimo.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Saiu {
        Quadro(u16),
        Fec(u16),
        Silencio(u16),
        Ocioso,
    }

    fn resumir(p: Puxado<'_>) -> Saiu {
        match p {
            Puxado::Ocioso => Saiu::Ocioso,
            Puxado::Slot(Entrega::Quadro { payload, sequencia, .. }) => {
                assert_eq!(payload, &sequencia.to_be_bytes(), "o payload é o do slot");
                Saiu::Quadro(sequencia)
            }
            Puxado::Slot(Entrega::Fec { socorro, sequencia, .. }) => {
                assert_eq!(socorro, &sequencia.wrapping_add(1).to_be_bytes(), "o socorro é o N+1");
                Saiu::Fec(sequencia)
            }
            Puxado::Slot(Entrega::Silencio { sequencia, .. }) => Saiu::Silencio(sequencia),
        }
    }

    /// Uma bancada: `chegada(i)` diz quando o pacote `i` chega (ou `None` se ele se perde), e
    /// `puxadas` são as horas das puxadas. `razao` recebe o leitor e devolve a razão que a
    /// casca aplica.
    struct Bancada {
        alimentador: AlimentadorDeReproducao,
        rep: ReproducaoPuxada,
        saidas: Vec<(u64, Saiu)>,
        /// O maior k visto depois de qualquer puxada.
        k_max: u16,
    }

    impl Bancada {
        fn nova(casca_reamostra: bool) -> Self {
            let (alimentador, rep) =
                ReproducaoPuxada::nova(opcoes(casca_reamostra), Arc::new(Clock::new()));
            Bancada {
                alimentador,
                rep,
                saidas: Vec::new(),
                k_max: 0,
            }
        }

        fn chegar(&self, i: u64, hora: u64) {
            let s = i as u16;
            let payload = s.to_be_bytes();
            self.alimentador.entregar(
                &QuadroDeAudio {
                    payload: &payload,
                    timestamp_us: i * Q,
                    sequencia: s,
                    marca: true,
                },
                hora,
            );
        }

        fn puxar(&mut self, hora: u64, razao: f64) -> Saiu {
            let s = resumir(self.rep.puxar_em(hora, 10_000, razao));
            self.saidas.push((hora, s));
            self.k_max = self.k_max.max(self.rep.c.profundidade_efetiva);
            s
        }

        /// Quantas vezes um quadro saiu com a sequência **atrás** do quadro anterior: `proxima`
        /// andando para trás. Tem de ser zero.
        fn regressoes(&self) -> usize {
            let quadros: Vec<u16> = self
                .saidas
                .iter()
                .filter_map(|(_, s)| match s {
                    Saiu::Quadro(q) => Some(*q),
                    _ => None,
                })
                .collect();
            quadros
                .windows(2)
                .filter(|w| !depois_de(w[1], w[0]))
                .count()
        }

        fn contadores(&self) -> ContadoresDeReproducao {
            let c = self.rep.leitor().contadores();
            assert_eq!(
                c.puxadas,
                c.soma_das_ordens(),
                "a invariante dos contadores quebrou: {c:?}"
            );
            c
        }
    }

    /// Roda de `de` até `ate` µs: chegadas pelo `chegada`, puxadas pelo `puxadas`. Os dois são
    /// iteradores de horas crescentes; `chegada(i)` pode devolver `None` (perda).
    fn correr(
        b: &mut Bancada,
        ate: u64,
        chegada: impl Fn(u64) -> Option<u64>,
        proxima_puxada: impl FnMut(u64, &LeitorDeReproducao) -> Option<u64>,
    ) {
        correr_com(b, ate, false, chegada, proxima_puxada)
    }

    /// `aplica`: a casca aplica a razão sugerida, e a informa como aplicada na puxada.
    fn correr_com(
        b: &mut Bancada,
        ate: u64,
        aplica: bool,
        chegada: impl Fn(u64) -> Option<u64>,
        mut proxima_puxada: impl FnMut(u64, &LeitorDeReproducao) -> Option<u64>,
    ) {
        // As chegadas, na ordem da hora e não do índice: é assim que a reordenação aparece.
        let limite_de_indices = ate / Q + 10_000;
        let mut chegadas: Vec<(u64, u64)> = (0..limite_de_indices)
            .filter_map(|i| chegada(i).map(|h| (h, i)))
            .filter(|(h, _)| *h <= ate)
            .collect();
        chegadas.sort_by_key(|(h, i)| (*h, *i));
        let mut chegadas = chegadas.into_iter().peekable();
        let mut hora_da_puxada = proxima_puxada(0, &b.rep.leitor());
        loop {
            let proxima = match (chegadas.peek().copied(), hora_da_puxada) {
                (Some((hp, i)), Some(hx)) if hp <= hx => {
                    b.chegar(i, hp);
                    chegadas.next();
                    continue;
                }
                (_, Some(hx)) => hx,
                (Some((hp, i)), None) => {
                    b.chegar(i, hp);
                    chegadas.next();
                    continue;
                }
                (None, None) => break,
            };
            if proxima > ate {
                break;
            }
            let razao = if aplica {
                b.rep.leitor().razao_sugerida().unwrap_or(1.0)
            } else {
                f64::NAN
            };
            b.puxar(proxima, razao);
            hora_da_puxada = proxima_puxada(proxima, &b.rep.leitor());
        }
    }

    /// Puxadas a cada 20 ms, com fase de 7 ms, de `de` até sempre.
    fn a_cada_20ms(de: u64) -> impl FnMut(u64, &LeitorDeReproducao) -> Option<u64> {
        let mut n = 0u64;
        move |_, _| {
            let h = de + 7_000 + n * Q;
            n += 1;
            Some(h)
        }
    }

    fn quadros_entre(b: &Bancada, de: u64, ate: u64) -> usize {
        b.saidas
            .iter()
            .filter(|(h, s)| *h >= de && *h < ate && matches!(s, Saiu::Quadro(_)))
            .count()
    }

    #[test]
    fn antes_de_qualquer_pacote_e_ocioso() {
        let mut b = Bancada::nova(false);
        assert_eq!(b.puxar(0, f64::NAN), Saiu::Ocioso);
        let c = b.contadores();
        assert_eq!(c.ociosas, 1);
        assert_eq!(c.razao_sugerida, None);
    }

    /// Fluxo perfeito: depois da ancoragem, só quadro, em ordem, sem subconsumo.
    #[test]
    fn fluxo_perfeito_toca_tudo_em_ordem() {
        let mut b = Bancada::nova(false);
        correr(&mut b, 10_000_000, |i| Some(i * Q + 5_000), a_cada_20ms(0));
        let c = b.contadores();
        assert_eq!(c.subconsumos, 0);
        assert_eq!(c.buracos, 0);
        assert_eq!(c.ancoragens, 1);
        let quadros: Vec<u16> = b
            .saidas
            .iter()
            .filter_map(|(_, s)| match s {
                Saiu::Quadro(q) => Some(*q),
                _ => None,
            })
            .collect();
        assert!(quadros.windows(2).all(|w| w[1] == w[0].wrapping_add(1)), "em ordem");
        assert!(c.nivel <= 3, "nível {}", c.nivel);
        let lat = c.latencia_na_ancoragem_us.expect("ancorou");
        assert!((50_000..=80_000).contains(&lat), "latência na ancoragem {lat} µs");
    }

    /// **Degrau de trânsito** (crítica 1, achado 1): em t = 5 s todo pacote passa a chegar 60 ms
    /// mais tarde, para sempre. A primeira versão ficava muda para sempre; aqui o som volta.
    #[test]
    fn degrau_de_transito_nao_deixa_o_receptor_mudo() {
        // +60 ms: a inserção por atraso absorve (desde a revisão do código da S1). +300 ms: o
        // atraso passa de 4 slots, não conta como degrau, e é o ocioso que reancora.
        for (passo, pelo_ocioso) in [(60_000u64, false), (300_000, true)] {
            degrau_de(passo, pelo_ocioso);
        }
    }

    fn degrau_de(passo: u64, pelo_ocioso: bool) {
        let mut b = Bancada::nova(false);
        let degrau = 5_000_000u64;
        correr(
            &mut b,
            15_000_000,
            |i| {
                let captura = i * Q;
                Some(captura + if captura >= degrau { 5_000 + passo } else { 5_000 })
            },
            a_cada_20ms(0),
        );
        let c = b.contadores();
        if pelo_ocioso {
            assert!(c.entradas_em_ocioso >= 1, "{c:?}");
        } else {
            assert!(c.insercoes_por_atraso >= 1, "{c:?}");
        }
        let volta = b
            .saidas
            .iter()
            .find(|(h, s)| *h > degrau + 100_000 && matches!(s, Saiu::Quadro(_)))
            .map(|(h, _)| *h)
            .expect("o som tinha de voltar");
        assert!(
            volta - degrau < 500_000,
            "o primeiro quadro voltou {} ms depois do degrau",
            (volta - degrau) / 1000
        );
        // E depois de voltar, toca tudo.
        let esperados = ((15_000_000 - 7_000_000) / Q) as usize;
        let tocados = quadros_entre(&b, 7_000_000, 15_000_000);
        assert!(tocados + 2 >= esperados, "{tocados} de {esperados}");
    }

    /// **O anel transborda** (crítica 1 achado 2; crítica 2 M3): a casca para de puxar 2 s, o
    /// anel de 64 enche. A primeira versão guardava os velhos e ficava com 787 ms de latência e
    /// 37 buracos; aqui reancora pelo que chega depois.
    #[test]
    fn anel_que_transborda_reancora_sem_latencia_velha() {
        let mut b = Bancada::nova(false);
        let para = 3_000_000u64;
        let volta = 5_000_000u64;
        let mut normal = a_cada_20ms(0);
        correr(
            &mut b,
            8_000_000,
            |i| Some(i * Q + 5_000),
            move |h, l| {
                let mut p = normal(h, l)?;
                while p > para && p < volta {
                    p = normal(h, l)?;
                }
                Some(p)
            },
        );
        let c = b.contadores();
        assert_eq!(c.transbordos, 1, "{c:?}");
        let buracos_depois = b
            .saidas
            .iter()
            .filter(|(h, s)| *h >= volta && matches!(s, Saiu::Silencio(_)))
            .count();
        assert!(buracos_depois <= 2, "{buracos_depois} silêncios depois da volta");
        assert!(c.nivel <= 4, "nível {} depois da volta", c.nivel);
        let lat = c.latencia_na_ancoragem_us.expect("reancorou");
        assert!(lat < 120_000, "latência depois da reancoragem: {lat} µs");
    }

    /// **O motor sobe tarde** (crítica 1 achado 3): 1 s de pacotes antes da primeira puxada. A
    /// primeira versão ancorava no mais velho e ficava com 1 s de latência fixa.
    #[test]
    fn motor_que_sobe_tarde_ancora_no_mais_novo() {
        let mut b = Bancada::nova(false);
        correr(&mut b, 6_000_000, |i| Some(i * Q + 5_000), a_cada_20ms(1_000_000));
        let c = b.contadores();
        assert!(c.descartados_na_ancoragem >= 40, "{c:?}");
        let lat = c.latencia_na_ancoragem_us.expect("ancorou");
        assert!(lat < 100_000, "latência na ancoragem {lat} µs");
        assert_eq!(c.subconsumos, 0);
    }

    /// **Quantum grande** (crítica 1 achado 4): a casca puxa 5 slots de uma vez a cada 100 ms, e
    /// a rede tem jitter de 0 a 15 ms. Com a folga pela rajada, depois do aquecimento quase nada
    /// falta.
    #[test]
    fn quantum_grande_nao_passa_fome() {
        for fase in [0u64, 20_000, 40_000, 60_000, 80_000] {
            let mut b = Bancada::nova(false);
            let jitter = |i: u64| (i.wrapping_mul(2_654_435_761) >> 7) % 15_000;
            let mut n = 0u64;
            correr(
                &mut b,
                20_000_000,
                |i| Some(i * Q + 3_000 + jitter(i)),
                move |_, _| {
                    // 5 puxadas quase juntas, 0,1 ms uma da outra, a cada 100 ms.
                    let rajada = n / 5;
                    let dentro = n % 5;
                    n += 1;
                    Some(fase + rajada * 100_000 + dentro * 100)
                },
            );
            let c = b.contadores();
            assert_eq!(c.rajada, 5, "a rajada tinha de ser medida");
            assert_eq!(c.profundidade_efetiva, 6);
            let faltas: usize = b
                .saidas
                .iter()
                .filter(|(h, s)| *h > 2_000_000 && matches!(s, Saiu::Silencio(_)))
                .count();
            let total = b.saidas.iter().filter(|(h, _)| *h > 2_000_000).count();
            assert!(
                faltas * 100 <= total,
                "fase {fase}: {faltas} silêncios em {total} puxadas depois do aquecimento"
            );
        }
    }

    /// **A casca para de puxar 300 ms** (menos que o anel): salto, e a latência volta.
    #[test]
    fn parada_curta_da_casca_salta_para_o_mais_novo() {
        let mut b = Bancada::nova(false);
        let para = 3_000_000u64;
        let volta = 3_300_000u64;
        let mut normal = a_cada_20ms(0);
        correr(
            &mut b,
            6_000_000,
            |i| Some(i * Q + 5_000),
            move |h, l| {
                let mut p = normal(h, l)?;
                while p > para && p < volta {
                    p = normal(h, l)?;
                }
                Some(p)
            },
        );
        let c = b.contadores();
        assert_eq!(c.saltos, 1, "{c:?}");
        assert_eq!(c.transbordos, 0);
        assert!(c.slots_saltados >= 12, "{c:?}");
        assert!(c.nivel <= 4, "nível {} depois do salto", c.nivel);
    }

    /// Paradas curtas da rede (150 ms) mantêm a latência: o relógio manda, e os atrasados são
    /// `tarde_demais`.
    #[test]
    fn parada_curta_da_rede_mantem_a_latencia() {
        let mut b = Bancada::nova(false);
        correr(
            &mut b,
            8_000_000,
            |i| {
                let c = i * Q;
                // Entre 4,00 e 4,15 s nada chega; tudo o que foi capturado aí chega junto em 4,15.
                if (4_000_000..4_150_000).contains(&c) {
                    Some(4_150_000 + (c - 4_000_000) / 1000)
                } else {
                    Some(c + 5_000)
                }
            },
            a_cada_20ms(0),
        );
        let c = b.contadores();
        assert!(c.subconsumos >= 4 && c.subconsumos <= 9, "{c:?}");
        assert!(c.tarde_demais >= 4, "{c:?}");
        assert_eq!(c.entradas_em_ocioso, 0);
        assert!(c.nivel <= 3);
    }

    /// Perda isolada com FEC: o slot que faltou sai como convite a FEC, com o sucessor.
    #[test]
    fn perda_isolada_oferece_o_sucessor() {
        let mut b = Bancada::nova(false);
        correr(
            &mut b,
            4_000_000,
            |i| (i != 100).then_some(i * Q + 5_000),
            a_cada_20ms(0),
        );
        assert!(b.saidas.iter().any(|(_, s)| *s == Saiu::Fec(100)));
        let c = b.contadores();
        assert_eq!(c.curas_oferecidas, 1);
        assert_eq!(c.subconsumos, 0);
    }

    /// Sem FEC na política, o mesmo buraco é silêncio.
    #[test]
    fn sem_fec_o_buraco_e_silencio() {
        let (alimentador, rep) = ReproducaoPuxada::nova(
            OpcoesDeReproducao {
                politica: Politica::AUDIO_DO_SISTEMA,
                casca_reamostra: false,
            },
            Arc::new(Clock::new()),
        );
        let mut b = Bancada {
            alimentador,
            rep,
            saidas: Vec::new(),
            k_max: 0,
        };
        correr(
            &mut b,
            4_000_000,
            |i| (i != 100).then_some(i * Q + 5_000),
            a_cada_20ms(0),
        );
        assert!(b.saidas.iter().any(|(_, s)| *s == Saiu::Silencio(100)));
        assert_eq!(b.contadores().buracos, 1);
    }

    /// Reordenação dentro da folga é salva; duplicado é contado à parte.
    #[test]
    fn reordenado_e_salvo_e_duplicado_e_contado() {
        let mut b = Bancada::nova(false);
        correr(
            &mut b,
            4_000_000,
            |i| match i {
                101 => Some(100 * Q + 5_000),
                100 => Some(100 * Q + 10_000),
                _ => Some(i * Q + 5_000),
            },
            a_cada_20ms(0),
        );
        let c = b.contadores();
        assert_eq!(c.reordenados, 1, "{c:?}");
        assert_eq!(c.curas_oferecidas + c.buracos, 0);
    }

    #[test]
    fn duplicado_e_contado_a_parte() {
        let mut b = Bancada::nova(false);
        for i in 0..5u64 {
            b.chegar(i, i * Q);
        }
        b.chegar(3, 3 * Q + 1_000);
        b.puxar(5 * Q, f64::NAN);
        let c = b.contadores();
        assert_eq!(c.duplicados, 1, "{c:?}");
        assert_eq!(c.reordenados, 0, "{c:?}");
    }

    /// Salto de sequência pequeno (60) enquanto toca: o teto pula para perto do mais novo.
    #[test]
    fn salto_de_sequencia_acima_do_teto_pula() {
        let mut b = Bancada::nova(false);
        correr(
            &mut b,
            6_000_000,
            |i| {
                // A partir do índice 150 a sequência pula 60: os índices 150..209 não existem.
                if (150..210).contains(&i) {
                    None
                } else {
                    let captura = if i >= 210 { (i - 60) * Q } else { i * Q };
                    Some(captura + 5_000)
                }
            },
            a_cada_20ms(0),
        );
        let c = b.contadores();
        assert!(c.descartes_por_teto >= 1, "{c:?}");
        assert!(c.buracos < 10, "o teto não pode virar dezenas de buracos: {c:?}");
    }

    /// Salto de sequência grande (3 000): é outro fluxo, reancora.
    #[test]
    fn salto_de_sequencia_grande_reancora() {
        let mut b = Bancada::nova(false);
        correr(
            &mut b,
            6_000_000,
            |i| {
                if (150..3150).contains(&i) {
                    None
                } else {
                    let captura = if i >= 3150 { (i - 3000) * Q } else { i * Q };
                    Some(captura + 5_000)
                }
            },
            a_cada_20ms(0),
        );
        let c = b.contadores();
        assert_eq!(c.ancoragens, 2, "{c:?}");
        assert_eq!(c.entradas_em_ocioso, 1);
    }

    /// Pacote maior que 1 500 B é descartado e contado, não copiado pela metade.
    #[test]
    fn pacote_grande_demais_e_contado() {
        let b = Bancada::nova(false);
        let grande = vec![0u8; TAMANHO_MAXIMO_DO_PACOTE + 1];
        b.alimentador.entregar(
            &QuadroDeAudio {
                payload: &grande,
                timestamp_us: 0,
                sequencia: 0,
                marca: true,
            },
            0,
        );
        let mut b = b;
        b.puxar(0, f64::NAN);
        assert_eq!(b.contadores().grandes_demais, 1);
    }

    /// **Deriva longa sem reamostragem**: o emissor 200 ppm mais rápido que o DAC por 30 min. A
    /// reserva descarta um slot a cada ~100 s, sem subconsumo, e a deriva estimada bate.
    #[test]
    fn deriva_longa_sem_reamostragem_descarta_na_taxa_da_deriva() {
        let mut b = Bancada::nova(false);
        let fim = 30 * 60 * 1_000_000u64;
        correr(
            &mut b,
            fim,
            |i| Some(((i * Q) as f64 * (1.0 - 200e-6)) as u64 + 5_000),
            a_cada_20ms(0),
        );
        let c = b.contadores();
        assert_eq!(c.subconsumos, 0, "{c:?}");
        assert!(
            (14..=20).contains(&c.descartes_por_deriva),
            "200 ppm em 30 min são ~18 slots: {c:?}"
        );
        let ppm = c.deriva_ed_ppm.expect("a deriva tinha de estar medida");
        assert!((ppm - 200.0).abs() < 2.0, "deriva estimada {ppm} ppm (a ±30 até 22/09, pelo nível inteiro; §21)");
    }

    /// **Deriva longa com a casca reamostrando pela razão sugerida**: nada é descartado nem
    /// inserido pelo núcleo, e a razão converge para a deriva.
    #[test]
    fn deriva_longa_com_reamostragem_converge_sem_descarte() {
        let mut b = Bancada::nova(true);
        let fim = 30 * 60 * 1_000_000u64;
        let mut hora = 7_000f64;
        correr_com(
            &mut b,
            fim,
            true,
            |i| Some(((i * Q) as f64 * (1.0 - 200e-6)) as u64 + 5_000),
            move |_, leitor| {
                let r = leitor.razao_sugerida().unwrap_or(1.0);
                let h = hora as u64;
                // A casca consome r vezes mais depressa: o período encurta.
                hora += Q as f64 / r;
                Some(h)
            },
        );
        let c = b.contadores();
        assert_eq!(c.descartes_por_deriva + c.insercoes_por_deriva, 0, "{c:?}");
        assert_eq!(c.subconsumos, 0, "{c:?}");
        let r = c.razao_sugerida.expect("razão medida");
        assert!(((r - 1.0) * 1e6 - 200.0).abs() < 40.0, "razão {r}");
        assert!(c.nivel <= 6, "nível {}", c.nivel);
        let ppm = c.deriva_ed_ppm.expect("deriva medida");
        assert!((ppm - 200.0).abs() < 2.0, "deriva estimada {ppm} ppm (a ±30 até 22/09, pelo nível inteiro; §21)");
    }

    /// A razão sugerida não existe antes de 10 s tocando.
    #[test]
    fn a_razao_so_aparece_depois_de_dez_segundos() {
        let mut b = Bancada::nova(false);
        correr(&mut b, 5_000_000, |i| Some(i * Q + 5_000), a_cada_20ms(0));
        assert_eq!(b.contadores().razao_sugerida, None);
        let mut b = Bancada::nova(false);
        correr(&mut b, 12_000_000, |i| Some(i * Q + 5_000), a_cada_20ms(0));
        let r = b.contadores().razao_sugerida.expect("depois de 10 s tocando, medida");
        assert!((r - 1.0).abs() < 1e-4, "fluxo sem deriva: razão {r}");
    }

    /// Fim de fluxo: depois de 10 puxadas sem nada, ocioso, e o `IDLE` não conta subconsumo.
    #[test]
    fn fim_de_fluxo_vira_ocioso() {
        let mut b = Bancada::nova(false);
        correr(
            &mut b,
            4_000_000,
            |i| (i < 100).then_some(i * Q + 5_000),
            a_cada_20ms(0),
        );
        let c = b.contadores();
        assert_eq!(c.entradas_em_ocioso, 1);
        assert_eq!(c.subconsumos, u64::from(PUXADAS_ATE_OCIOSO));
        assert!(matches!(b.saidas.last(), Some((_, Saiu::Ocioso))));
    }

    // -----------------------------------------------------------------------------------------
    // A revisão do código da S1 (18/09/2026, `criticas-som/4-codigo-s1-porta-puxada.md`). Os
    // cenários são os do revisor; as asserções são as que o conserto tem de cumprir.
    // -----------------------------------------------------------------------------------------

    /// splitmix64: jitter determinístico.
    fn misturar(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Por segundo, a partir de `de`: `(segundo, puxadas que não saíram como quadro, puxadas)`.
    fn perfil(b: &Bancada, de: u64, ate: u64) -> Vec<(u64, usize, usize)> {
        let mut v = Vec::new();
        let mut s = de;
        while s < ate {
            let e = s + 1_000_000;
            let total = b.saidas.iter().filter(|(h, _)| *h >= s && *h < e).count();
            let ruins = b
                .saidas
                .iter()
                .filter(|(h, x)| *h >= s && *h < e && !matches!(x, Saiu::Quadro(_)))
                .count();
            v.push((s / 1_000_000, ruins, total));
            s = e;
        }
        v
    }

    /// **Achado A1 (grave)**: a casca trava 1,2 s e recupera o atraso de uma vez, com 60 puxadas
    /// seguidas. Antes do conserto, k subia para 62 e ficava lá: ~1,26 s de latência para sempre.
    #[test]
    fn travada_e_recuperacao_em_rajada_nao_prende_k() {
        let mut b = Bancada::nova(false);
        let mut n = 0u64;
        correr(
            &mut b,
            60_000_000,
            |i| Some(i * Q + 5_000),
            move |_, _| {
                let h = if n < 250 {
                    7_000 + n * Q
                } else if n < 310 {
                    7_000 + 310 * Q + (n - 250) * 50
                } else {
                    7_000 + n * Q
                };
                n += 1;
                Some(h)
            },
        );
        let c = b.contadores();
        eprintln!(
            "A1 travada: k no fim {} k máximo {} nível {} teto {} saltos {}",
            c.profundidade_efetiva, b.k_max, c.nivel, c.descartes_por_teto, c.saltos
        );
        assert_eq!(c.profundidade_efetiva, 2, "k tinha de voltar a 2: {c:?}");
        assert!(b.k_max <= TETO_DE_K, "k passou do teto: {}", b.k_max);
        assert!(c.nivel <= 4, "latência que ficou: nível {}", c.nivel);
        assert!(c.descartes_por_teto < 100, "{c:?}");
        assert_eq!(b.regressoes(), 0, "`proxima` andou para trás");
    }

    /// **Achado A1 (grave)**: 60 puxadas de uma vez sem travada antes (uma casca que enche uma
    /// fila de 1 s). Antes: k acima do teto de 50, `descartes_por_teto` = 65 572 pela volta do
    /// `u16`, e `proxima` andando para trás.
    #[test]
    fn rajada_de_sessenta_nao_passa_do_teto_nem_volta_a_contagem() {
        let mut b = Bancada::nova(false);
        let mut n = 0u64;
        correr(
            &mut b,
            120_000_000,
            |i| Some(i * Q + 5_000),
            move |_, _| {
                let h = if n < 250 {
                    7_000 + n * Q
                } else if n < 310 {
                    7_000 + 250 * Q + (n - 250) * 50
                } else {
                    7_000 + (n - 59) * Q
                };
                n += 1;
                Some(h)
            },
        );
        let c = b.contadores();
        eprintln!(
            "A1 rajada de 60: k no fim {} k máximo {} teto {} regressões {}",
            c.profundidade_efetiva,
            b.k_max,
            c.descartes_por_teto,
            b.regressoes()
        );
        assert!(c.descartes_por_teto < 1_000, "{c:?}");
        assert!(b.k_max <= TETO_DE_K, "k passou do teto: {}", b.k_max);
        assert_eq!(c.profundidade_efetiva, 2, "uma rajada avulsa não fixa k: {c:?}");
        assert_eq!(b.regressoes(), 0, "`proxima` andou para trás");
    }

    /// **Achado A3**: quantum de 160 ms (8 puxadas por rajada), emissor 200 ppm mais rápido, 30
    /// min, com e sem a casca reamostrando. Antes: toda rajada virava salto (o limiar nunca
    /// aprendia), 48 slots descartados com a casca reamostrando, e a razão no sentido errado.
    #[test]
    fn quantum_de_160ms_nao_vira_salto() {
        for reamostra in [false, true] {
            let mut b = Bancada::nova(reamostra);
            let fim = 30 * 60 * 1_000_000u64;
            let mut hora = 0f64;
            let mut n = 0u64;
            correr_com(
                &mut b,
                fim,
                reamostra,
                |i| {
                    let j = misturar(i) % 15_000;
                    Some(((i * Q) as f64 * (1.0 - 200e-6)) as u64 + 3_000 + j)
                },
                move |_, leitor| {
                    let dentro = n % 8;
                    n += 1;
                    if dentro == 0 && n > 1 {
                        let r = if reamostra {
                            leitor.razao_sugerida().unwrap_or(1.0)
                        } else {
                            1.0
                        };
                        hora += 160_000.0 / r;
                    }
                    Some(hora as u64 + dentro * 100)
                },
            );
            let c = b.contadores();
            eprintln!(
                "A3 reamostra={reamostra}: saltos {} rajada {} k {} deriva {:?} razão {:?}",
                c.saltos, c.rajada, c.profundidade_efetiva, c.deriva_ed_ppm, c.razao_sugerida
            );
            assert_eq!(c.saltos, 0, "reamostra={reamostra}: {c:?}");
            assert_eq!(c.slots_saltados, 0, "reamostra={reamostra}: {c:?}");
            let ppm = c.deriva_ed_ppm.expect("deriva medida");
            assert!((ppm - 200.0).abs() < 5.0, "reamostra={reamostra}: deriva {ppm} (a ±40 até 22/09; §21)");
            if reamostra {
                assert_eq!(c.descartes_por_deriva + c.insercoes_por_deriva, 0, "{c:?}");
                let r = c.razao_sugerida.expect("razão medida");
                assert!(((r - 1.0) * 1e6 - 200.0).abs() < 60.0, "razão {r}");
            }
        }
    }

    /// **Achado A4**: a casca pré-enche a própria fila na partida (5 puxadas de uma vez) e depois
    /// puxa uma a cada 20 ms. Antes: k em 6 para sempre, +80 ms de latência pela sessão inteira.
    #[test]
    fn pre_enchimento_da_casca_nao_sobe_k() {
        let mut b = Bancada::nova(false);
        let mut n = 0u64;
        correr(
            &mut b,
            60_000_000,
            |i| Some(i * Q + 5_000),
            move |_, _| {
                let h = if n < 5 { n * 100 } else { (n - 4) * Q + 7_000 };
                n += 1;
                Some(h)
            },
        );
        let c = b.contadores();
        assert_eq!(c.profundidade_efetiva, 2, "{c:?}");
        let lat = c.latencia_na_ancoragem_us.expect("ancorou");
        assert!(lat < 90_000, "latência na ancoragem {lat} µs");
    }

    /// **Achado A6**: degrau de trânsito **parcial** (parte dos pacotes ainda chega a tempo), com
    /// e sem a casca reamostrando. Antes: com a casca reamostrando, 6 a 14 s em ocultação, porque
    /// só a reserva realinhava e ela não roda nesse modo.
    #[test]
    fn degrau_parcial_se_recupera_com_e_sem_reamostragem() {
        let degrau = 15_000_000u64;
        let fim = 60_000_000u64;
        for reamostra in [false, true] {
            for d in [30_000u64, 40_000, 45_000, 50_000] {
                let mut b = Bancada::nova(reamostra);
                let mut hora = 7_000f64;
                let chegada = move |i: u64| {
                    let captura = i * Q;
                    let j = misturar(i) % 30_000;
                    Some(captura + 5_000 + j / 6 + if captura >= degrau { d + j } else { 0 })
                };
                correr_com(&mut b, fim, reamostra, chegada, move |_, leitor| {
                    let r = if reamostra {
                        leitor.razao_sugerida().unwrap_or(1.0)
                    } else {
                        1.0
                    };
                    let h = hora as u64;
                    hora += Q as f64 / r;
                    Some(h)
                });
                let p = perfil(&b, degrau, fim);
                let ultimo_ruim = p
                    .iter()
                    .filter(|x| x.1 * 20 > x.2)
                    .map(|x| x.0)
                    .max()
                    .unwrap_or(degrau / 1_000_000);
                let segundos = ultimo_ruim + 1 - degrau / 1_000_000;
                let c = b.contadores();
                eprintln!(
                    "A6 reamostra={reamostra} degrau +{} ms: {segundos} s com >5 % ruins; \
                     inserções por atraso {}, por deriva {}",
                    d / 1000,
                    c.insercoes_por_atraso,
                    c.insercoes_por_deriva
                );
                assert!(
                    segundos <= 3,
                    "reamostra={reamostra}, degrau +{} ms: {segundos} s com mais de 5 % ruins; \
                     {:?}",
                    d / 1000,
                    b.contadores()
                );
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // A reconferência da S1 (18/09/2026, `criticas-som/6-reconferencia-a.md`): N1 e N2. A casca
    // de quantum é a do revisor (`reconf_quantum_4096_alterna_4_e_5`).
    // -----------------------------------------------------------------------------------------

    /// Uma casca de quantum fixo a 48 kHz: a cada callback de `quantum` quadros, puxa quantos
    /// slots de 960 quadros faltarem para cobrir o callback, a 0,1 ms um do outro.
    fn casca_de_quantum(quantum: i64) -> impl FnMut(u64, &LeitorDeReproducao) -> Option<u64> {
        let (mut sobra, mut callback, mut dentro, mut faltam) = (0i64, 0u64, 0u64, 0i64);
        move |_, _| loop {
            if faltam > 0 {
                faltam -= 1;
                sobra += 960;
                let h = (callback as f64 * quantum as f64 / 48_000.0 * 1e6) as u64 + dentro * 100;
                dentro += 1;
                if faltam == 0 {
                    sobra -= quantum;
                }
                return Some(h);
            }
            callback += 1;
            dentro = 0;
            let precisa = quantum - sobra;
            faltam = if precisa > 0 { (precisa + 959) / 960 } else { 0 };
            if faltam == 0 {
                sobra -= quantum;
            }
        }
    }

    /// `(inserções + descartes de rajada, slots fora de quadro depois de 30 s, slots depois de
    /// 30 s)` de uma casca de quantum, em `minutos` de sessão.
    fn oscilacao_do_quantum(
        quantum: i64,
        reamostra: bool,
        minutos: u64,
    ) -> (u64, usize, usize, ContadoresDeReproducao) {
        let mut b = Bancada::nova(reamostra);
        let fim = minutos * 60 * 1_000_000;
        correr_com(
            &mut b,
            fim,
            reamostra,
            |i| Some(i * Q + 3_000 + misturar(i) % 10_000),
            casca_de_quantum(quantum),
        );
        let c = b.contadores();
        let p = perfil(&b, 30_000_000, fim);
        let ruins: usize = p.iter().map(|x| x.1).sum();
        let total: usize = p.iter().map(|x| x.2).sum();
        (c.insercoes_de_rajada + c.descartes_de_rajada, ruins, total, c)
    }

    /// **Achado N1 da reconferência (grave).** Quantum de 4 096 quadros (85,33 ms, o iOS com a
    /// tela travada): as rajadas saem com 4 ou 5 puxadas, e as de 5 são 4 em cada 15 callbacks.
    /// Com o terceiro maior de 8 e k descendo sem histerese, k subia e descia a cada ~0,64 s:
    /// 939 inserções e 936 descartes em 10 min, 3,1 % de ocultação.
    #[test]
    fn quantum_de_4096_quadros_nao_faz_k_oscilar() {
        for reamostra in [false, true] {
            let (mudancas, ruins, total, c) = oscilacao_do_quantum(4096, reamostra, 10);
            eprintln!(
                "N1 4096, reamostra={reamostra}: k={} rajada={} ins+desc de rajada={mudancas} \
                 fora de quadro depois de 30 s: {ruins}/{total}",
                c.profundidade_efetiva, c.rajada
            );
            assert!(mudancas <= 6, "reamostra={reamostra}: k oscilou, {mudancas} mudanças");
            assert!(ruins * 1_000 <= total, "reamostra={reamostra}: {ruins}/{total} fora de quadro");
        }
    }

    /// A varredura do revisor, com asserção: nenhum quantum de 256 a 4 800 quadros faz k oscilar.
    #[test]
    fn nenhum_quantum_faz_k_oscilar() {
        let mut ruins_por_quantum = Vec::new();
        for quantum in [256i64, 512, 1024, 1536, 2048, 2560, 3072, 3584, 4096, 4800] {
            let (mudancas, ruins, total, c) = oscilacao_do_quantum(quantum, false, 5);
            eprintln!(
                "N1 varredura, quantum {quantum:>4}: k={} rajada={} ins+desc de rajada={mudancas} \
                 fora de quadro {ruins}/{total}",
                c.profundidade_efetiva, c.rajada
            );
            ruins_por_quantum.push((quantum, mudancas, ruins, total));
        }
        for (quantum, mudancas, ruins, total) in ruins_por_quantum {
            assert!(mudancas <= 6, "quantum {quantum}: k oscilou, {mudancas} mudanças");
            assert!(ruins * 1_000 <= total, "quantum {quantum}: {ruins}/{total} fora de quadro");
        }
    }

    /// Uma casca de 20 ms que, a cada 3 s, acorda 60 ms atrasada e enche o que falta de uma vez,
    /// com 4 puxadas: o WASAPI que acorda tarde. O intervalo (80 ms) fica abaixo do limiar do
    /// salto, então a rajada conta. **Uma rajada avulsa não é o quantum**: k não sobe, e nada é
    /// inserido nem descartado. É o que o máximo das 8 rajadas, sozinho, não cumpre.
    #[test]
    fn soluco_avulso_da_casca_nao_sobe_k() {
        let mut b = Bancada::nova(false);
        let mut n = 0u64;
        correr(
            &mut b,
            60_000_000,
            |i| Some(i * Q + 3_000 + misturar(i) % 10_000),
            move |_, _| {
                let r = n % 150;
                let h = if (146..150).contains(&r) {
                    7_000 + (n - r + 149) * Q + (r - 146) * 100
                } else {
                    7_000 + n * Q
                };
                n += 1;
                Some(h)
            },
        );
        let c = b.contadores();
        let p = perfil(&b, 2_000_000, 60_000_000);
        let ruins: usize = p.iter().map(|x| x.1).sum();
        let total: usize = p.iter().map(|x| x.2).sum();
        eprintln!(
            "soluço avulso: k={} k máx={} ins de rajada={} desc de rajada={} saltos={} \
             fora de quadro {ruins}/{total}",
            c.profundidade_efetiva, b.k_max, c.insercoes_de_rajada, c.descartes_de_rajada, c.saltos
        );
        assert_eq!(c.saltos, 0, "80 ms não é salto: {c:?}");
        assert_eq!(b.k_max, 2, "k subiu por uma rajada avulsa: {c:?}");
        assert_eq!(c.insercoes_de_rajada + c.descartes_de_rajada, 0, "{c:?}");
    }

    /// **Achado N2 da reconferência (miúdo).** Quantum de 160 ms (8 puxadas de uma vez): depois
    /// do primeiro quadro, a partida gaguejava, com 23 silêncios nos primeiros 640 ms.
    #[test]
    fn quantum_de_160ms_nao_gagueja_na_partida() {
        let mut b = Bancada::nova(false);
        let mut n = 0u64;
        correr(
            &mut b,
            20_000_000,
            |i| Some(i * Q + 3_000 + misturar(i) % 15_000),
            move |_, _| {
                let r = n / 8;
                let dentro = n % 8;
                n += 1;
                Some(r * 160_000 + dentro * 100)
            },
        );
        let primeiro = b
            .saidas
            .iter()
            .find(|(_, s)| matches!(s, Saiu::Quadro(_)))
            .map(|(h, _)| *h)
            .expect("tocou");
        let silencios: Vec<u64> = b
            .saidas
            .iter()
            .filter(|(h, s)| *h >= primeiro && matches!(s, Saiu::Silencio(_)))
            .map(|(h, _)| *h / 1_000)
            .collect();
        let c = b.contadores();
        eprintln!(
            "N2 160 ms: primeiro quadro aos {} ms; {} silêncios depois dele, em ms {:?}; \
             ins de rajada={} subconsumos={} k={} latência na ancoragem {:?}",
            primeiro / 1_000,
            silencios.len(),
            silencios,
            c.insercoes_de_rajada,
            c.subconsumos,
            c.profundidade_efetiva,
            c.latencia_na_ancoragem_us
        );
        assert!(silencios.len() <= 1, "a partida gaguejou: {silencios:?}");
    }

    /// **A casca do Mac, simulada** (a corrida do controle 6 de 21/09, `docs/som-no-receptor.md`
    /// §20.7 e §21): ciclos de 512 quadros a 48 kHz no relógio do DAC (`d_ppm` contra o host), o
    /// Varispeed consumindo `512 × r` quadros de fonte por ciclo, o montador puxando um slot de 960
    /// quando a sobra acaba e informando o atraso até o DAC **com o que ainda toca antes do slot**
    /// (`SomPuxado.swift`, `render`), a razão sugerida lida a 30 Hz e passada como `f32`
    /// (`Tocador.ajustarRazao`). O emissor anda `e_ppm` mais depressa que o host.
    struct CenarioDoMac {
        e_ppm: f64,
        d_ppm: f64,
        segundos: u64,
        /// A fase da chegada: a hora do pacote 0.
        fase_us: u64,
        /// O que a rede soma à chegada do pacote `i` (jitter, rajada), em µs.
        rede: fn(u64, u64) -> u64,
        /// Um degrau no atraso declarado: `(quando, µs a mais dali em diante, pausa da casca)`.
        degrau: Option<(u64, u32, u64)>,
    }

    fn sem_rede(_: u64, _: u64) -> u64 {
        0
    }

    /// Devolve `(deriva estimada pelo núcleo, a mesma regressão sobre o nível inteiro)`, em ppm. A
    /// segunda é o estimador de antes de 22/09, refeito por fora como oráculo: o controle do
    /// controle. **Só sem inserção de silêncio**: o oráculo conta toda puxada, e o nível antigo não
    /// mudava com uma inserção (revisão do código, m3). Nos cenários daqui não há inserção; a
    /// revisão conferiu o oráculo contra o `reproducao.rs` antigo, fase a fase, a 0,6 ppm.
    fn corrida_como_o_mac(c: &CenarioDoMac) -> (Option<f64>, Option<f64>) {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut b = Bancada::nova(true);
        let fim = c.segundos * 1_000_000;
        let e = 1.0 + c.e_ppm * 1e-6;
        let taxa_do_dac = 48_000.0 * (1.0 + c.d_ppm * 1e-6);
        let ciclo_us = 512.0 / taxa_do_dac * 1e6;
        let (fase, rede) = (c.fase_us, c.rede);
        let chegada = move |i: u64| ((i * Q) as f64 / e) as u64 + fase + rede(i, fase);
        let mut chegadas: Vec<u64> = (0..fim / Q + 100).map(chegada).collect();
        chegadas.sort_unstable();
        let oraculo = Rc::new(RefCell::new(Regressao::default()));
        let o2 = Rc::clone(&oraculo);
        let mut hora_do_ciclo = 0f64;
        let mut sobra = 0.0f64;
        let mut razao = 1.0f64;
        let mut ultima_leitura = 0f64;
        let mut pendentes: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        let mut puxadas = 0u64;
        let mut consumo = 0.0f64;
        let mut ultima: Option<u64> = None;
        let mut razao_da_ultima = 1.0f64;
        let degrau = c.degrau;
        let mut pausou = false;
        correr_com_razao(&mut b, fim, chegada, move |antes, leitor| {
            if let Some(u) = ultima {
                puxadas += 1;
                consumo += (razao_da_ultima - 1.0) * antes.saturating_sub(u) as f64 / 1e6 * 50.0;
                if antes > fase + 1_500_000 {
                    let chegados = chegadas.partition_point(|&h| h <= antes) as f64;
                    o2.borrow_mut().somar(antes, chegados - puxadas as f64 + consumo);
                }
            }
            ultima = Some(antes);
            loop {
                if let Some(atraso) = pendentes.pop_front() {
                    razao_da_ultima = razao;
                    return Some((hora_do_ciclo as u64, atraso, razao));
                }
                hora_do_ciclo += ciclo_us;
                let mut extra = 0u32;
                if let Some((quando, mais, pausa)) = degrau {
                    if hora_do_ciclo as u64 >= quando {
                        if !pausou {
                            pausou = true;
                            hora_do_ciclo += pausa as f64;
                        }
                        extra = mais;
                    }
                }
                if hora_do_ciclo - ultima_leitura >= 33_000.0 {
                    ultima_leitura = hora_do_ciclo;
                    razao = f64::from(leitor.razao_sugerida().unwrap_or(1.0) as f32);
                }
                // O Varispeed pede `512 × r` quadros de fonte; o montador puxa quando a sobra acaba,
                // e o slot novo sai depois do que já foi escrito neste ciclo.
                let mut resta = 512.0 * razao;
                let mut escritos = 0.0f64;
                while sobra < resta {
                    escritos += sobra / razao;
                    resta -= sobra;
                    sobra = 960.0;
                    pendentes.push_back(12_800 + extra + (escritos / taxa_do_dac * 1e6) as u32);
                }
                sobra -= resta;
            }
        });
        let ppm = |r: &Regressao| r.inclinacao(fim).map(|x| x / 50.0 * 1e6);
        let oraculo = ppm(&oraculo.borrow());
        (b.contadores().deriva_ed_ppm, oraculo)
    }

    /// Como `correr_com`, mas a casca diz a hora, o atraso até o DAC e a razão de cada puxada.
    fn correr_com_razao(
        b: &mut Bancada,
        ate: u64,
        chegada: impl Fn(u64) -> u64,
        mut proxima_puxada: impl FnMut(u64, &LeitorDeReproducao) -> Option<(u64, u32, f64)>,
    ) {
        let mut chegadas: Vec<(u64, u64)> = (0..ate / Q + 100).map(|i| (chegada(i), i)).collect();
        chegadas.sort_unstable();
        let mut chegadas = chegadas.into_iter().peekable();
        let mut prox = proxima_puxada(0, &b.rep.leitor());
        while let Some((hx, atraso, razao)) = prox {
            while let Some(&(hp, i)) = chegadas.peek() {
                if hp > hx {
                    break;
                }
                b.chegar(i, hp);
                chegadas.next();
            }
            if hx > ate {
                break;
            }
            let s = resumir(b.rep.puxar_em(hx, atraso, razao));
            b.saidas.push((hx, s));
            prox = proxima_puxada(hx, &b.rep.leitor());
        }
    }

    /// Os erros (estimado − esperado) do núcleo e do oráculo do nível inteiro em 20 fases de chegada.
    fn varrer_fases(e_ppm: f64, d_ppm: f64, segundos: u64, rede: fn(u64, u64) -> u64) -> (Vec<f64>, Vec<f64>) {
        let (mut nucleo, mut inteiro) = (Vec::new(), Vec::new());
        for f in 0..20u64 {
            let (ed, oi) = corrida_como_o_mac(&CenarioDoMac {
                e_ppm,
                d_ppm,
                segundos,
                fase_us: 500 + f * 1_000,
                rede,
                degrau: None,
            });
            nucleo.push(ed.expect("a deriva tinha de estar publicada") - (e_ppm - d_ppm));
            inteiro.push(oi.expect("oráculo") - (e_ppm - d_ppm));
        }
        (nucleo, inteiro)
    }

    fn pior(v: &[f64]) -> f64 {
        v.iter().fold(0.0f64, |m, x| m.max(x.abs()))
    }

    /// **O controle 6 do Mac, sem aparelho** (§21): o estimador de antes leu 114,7 ppm contra 95,8
    /// numa corrida de 120 s. Com uma casca exata, a regressão sobre o nível inteiro erra de −94 a
    /// +27 ppm conforme a fase da chegada; a de agora, sobre a latência de cada slot tocado, erra
    /// menos de 1 ppm em todas as fases, nos três cenários medidos no Mac (o c6, a S4 a +200 ppm por
    /// 150 s, e a PCMU de 90 s sem deriva injetada).
    #[test]
    fn a_deriva_de_uma_corrida_curta_nao_depende_da_fase_da_chegada() {
        let mut pior_do_inteiro = 0.0f64;
        for (e, d, s) in [(100.0, 4.19, 120u64), (200.0, 3.8, 150), (0.0, 3.73, 90)] {
            let (nucleo, inteiro) = varrer_fases(e, d, s, sem_rede);
            eprintln!(
                "deriva e={e} d={d} {s} s: núcleo pior {:.2} ppm; nível inteiro de {:.0} a {:.0}",
                pior(&nucleo),
                inteiro.iter().cloned().fold(f64::INFINITY, f64::min),
                inteiro.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            );
            assert!(pior(&nucleo) <= 1.0, "e={e} {s} s: erros do núcleo {nucleo:?}");
            pior_do_inteiro = pior_do_inteiro.max(pior(&inteiro));
        }
        // O controle do controle: a mesma simulação separa os dois estimadores.
        assert!(pior_do_inteiro > 20.0, "o oráculo do nível inteiro tinha de errar: {pior_do_inteiro}");
    }

    /// Com jitter de chegada de 0 a 15 ms (o do A3), a latência de cada pacote carrega o jitter,
    /// e a regressão o média. Previsão escrita antes (§21): até 10 ppm em 120 s.
    #[test]
    fn a_deriva_com_jitter_de_chegada() {
        fn jitter(i: u64, _: u64) -> u64 {
            misturar(i) % 15_000
        }
        let (nucleo, inteiro) = varrer_fases(100.0, 4.19, 120, jitter);
        eprintln!("jitter 0–15 ms, 120 s: núcleo pior {:.2} ppm; nível inteiro pior {:.1}", pior(&nucleo), pior(&inteiro));
        assert!(pior(&nucleo) <= 10.0, "erros do núcleo {nucleo:?}");
    }

    /// Com a chegada em **rajadas de rádio** (cada pacote segura até o próximo múltiplo de 80 ms),
    /// a chegada só diz a deriva em degraus de 4 slots, e nenhum estimador sobre ela acerta em 120
    /// s (a revisão do desenho, 2.A: de −100 a +132 ppm). O que se exige é não piorar contra o de
    /// antes; os números vão para o §21.
    #[test]
    fn um_degrau_de_transito_congela_a_deriva() {
        // A rede passa a entregar tudo 60 ms mais tarde aos 100 s: pacotes tarde em drenagens
        // seguidas, inserções por atraso, e a regressão recomeça em cada uma (revisão do código,
        // M1: com o ajuste de um slot, −536 ppm; sem nada, −383). Publicada 60 s depois da última.
        fn degrau(i: u64, _: u64) -> u64 {
            if i * Q >= 100_000_000 {
                60_000
            } else {
                0
            }
        }
        let mut erros = Vec::new();
        for f in 0..5u64 {
            let ed = corrida_como_o_mac(&CenarioDoMac {
                e_ppm: 100.0,
                d_ppm: 4.19,
                segundos: 220,
                fase_us: 500 + f * 4_000,
                rede: degrau,
                degrau: None,
            })
            .0
            .expect("publicada 60 s depois do congelamento");
            erros.push(ed - (100.0 - 4.19));
        }
        eprintln!("degrau de trânsito de 60 ms aos 100 s, 220 s: erros {erros:?}");
        assert!(pior(&erros) <= 1.0, "erros {erros:?}");
    }

    #[test]
    fn a_deriva_com_chegada_em_rajadas_de_radio() {
        fn rajada(i: u64, fase: u64) -> u64 {
            let h = ((i * Q) as f64 / (1.0 + 100e-6)) as u64 + fase;
            h.div_ceil(80_000) * 80_000 - h
        }
        let (nucleo, inteiro) = varrer_fases(100.0, 4.19, 120, rajada);
        eprintln!(
            "rajadas de 80 ms, 120 s: núcleo pior {:.1} ppm {:?}; nível inteiro pior {:.1}",
            pior(&nucleo),
            nucleo.iter().map(|x| x.round()).collect::<Vec<_>>(),
            pior(&inteiro)
        );
        assert!(pior(&nucleo) <= pior(&inteiro) * 1.1 + 1.0, "o núcleo piorou: {nucleo:?} contra {inteiro:?}");
    }

    /// **Um degrau no atraso declarado**: com pausa de 150 ms da casca (uma troca de saída que
    /// recria o cliente), a puxada vira salto, o salto congela, e a regressão recomeça limpa; sem
    /// pausa, o degrau entra na latência (a limitação escrita no §21: o estimador de antes, pelo
    /// nível, não via o atraso declarado).
    #[test]
    fn um_degrau_no_atraso_declarado_com_e_sem_pausa() {
        let cenario = |pausa: u64| CenarioDoMac {
            e_ppm: 100.0,
            d_ppm: 4.19,
            segundos: 200,
            fase_us: 5_500,
            rede: sem_rede,
            degrau: Some((60_000_000, 10_000, pausa)),
        };
        let erro = |x: Option<f64>| x.map(|v| v - (100.0 - 4.19));
        let com_pausa = erro(corrida_como_o_mac(&cenario(150_000)).0);
        let sem_pausa = erro(corrida_como_o_mac(&cenario(0)).0);
        eprintln!("degrau de 10 ms aos 60 s: com pausa {com_pausa:?} ppm; sem pausa {sem_pausa:?}");
        assert!(com_pausa.expect("publicada 60 s depois do salto").abs() <= 1.0);
        assert!(
            sem_pausa.expect("publicada").abs() > 5.0,
            "o degrau sem pausa passou a ser tratado: atualize o §21 e este teste"
        );
    }
}
