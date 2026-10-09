//! O relógio comum da sessão: liga o `timestamp_us` de todas as tracks receptoras de uma sessão
//! a uma época só, e diz quando essa ligação não é confiável.
//!
//! # De onde vem a ligação
//!
//! O emissor do Quall escreve o carimbo RTP de toda track a partir do **mesmo** `timestamp_us`,
//! sem deslocamento sorteado: `track.rs` chama `rtcSetTrackRtpTimestamp` com
//! `timestamp_us × hz / 10⁶ mod 2³²`. O receptor joga isso fora ao rebasear cada track na base
//! dela (`rtp::RelogioRtp`). Este módulo guarda o que foi jogado fora.
//!
//! Sobra uma ambiguidade: as voltas do contador de 32 bits. A 90 kHz ele dá a volta a cada 13 h
//! 15 min, e o `timestamp_us` pode ser tempo desde o boot. Os deslocamentos possíveis entre as
//! voltas de duas tracks formam um **reticulado**. Em unidades de 1/720 000 s, o mínimo múltiplo
//! comum de 90 000, 48 000 e 8 000, o passo do reticulado é `2³² × mdc(m₁, m₂)`, com
//! `m = 720 000 / hz`: 5 965 s entre vídeo e Opus.
//!
//! # Como o ponto do reticulado é escolhido, e a guarda
//!
//! Pela **diferença dos trânsitos mínimos por janela** entre a track e a referência: a primeira
//! track observada. Numa mesma janela, a deriva entre o relógio do emissor e o do receptor é a
//! mesma para as duas tracks e se cancela. O piso da rede também, porque o caminho é o mesmo. O
//! que sobra depois de tirar o ponto do reticulado é o **resíduo**:
//! - com os relógios das duas tracks iguais, ele é a diferença de atraso de envio entre elas;
//! - com relógios diferentes, é essa diferença mais o descasamento.
//!
//! **A guarda recusa o deslocamento quando o resíduo passa de [`LIMIAR_DA_GUARDA_US`].** A
//! guarda é contínua: é recalculada a cada janela, e pega um degrau no meio da sessão.
//!
//! Medido antes de este código existir, com registros de chegada de sondas em `lo0`
//! (`docs/som-no-receptor.md` §2.2):
//! - controle: −0,1 ms;
//! - 30 ms de atraso de envio: +29,7 ms;
//! - o áudio carimbado 3 s adiantado: −3 000,1 ms, recusado.
//!
//! # O que este módulo não faz
//!
//! Não decide nada sobre reprodução, e não lê o SR. Ver `docs/som-no-receptor.md` §5.6.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::media::Clock;
use crate::rtp::Desenrolador;

/// O maior resíduo aceito, em microssegundos. Acima dele o deslocamento de captura é recusado.
///
/// **Hipótese de trabalho**, e não medida:
/// - os atrasos de envio legítimos que o repositório descreve cabem em ±80 ms;
/// - 150 ms fica perto do limite da ITU para o som atrasado.
///
/// Não pega descasamento menor que isso: a câmera do A10s, a ~103 ms de `MONOTONIC`, passa. O
/// conserto desse caso é no emissor (`docs/som-no-receptor.md` §8).
pub const LIMIAR_DA_GUARDA_US: i64 = 150_000;

/// Depois de recusar, a guarda só volta a valer com o resíduo abaixo disto: histerese de 30 ms.
pub const LIMIAR_DE_VOLTA_US: i64 = 120_000;

/// Acima deste resíduo, **uma** janela basta para recusar. Abaixo dele, e acima de
/// [`LIMIAR_DA_GUARDA_US`], a guarda pede **duas janelas comuns seguidas**.
///
/// Uma janela de 1 s inteira atrasada numa track só (a fila do espaçador de vídeo numa rajada de
/// IDR, por exemplo) tem a mesma cara de um degrau de relógio para trás, e recusava o par por 1 s
/// com os relógios certos (reconferência da S1, B2-novo: +250 ms no vídeo, +160 ms e +400 ms no
/// áudio). Um degrau de segundos, como o `reabrir` do Mac, continua recusado em até 2 s; um de
/// 150 ms a 1 s para trás passa a levar até 3 s. **Hipótese de trabalho**: nenhuma fila de envio
/// medida no repositório passa de 1 s numa track só.
pub const LIMIAR_DE_UMA_JANELA_US: i64 = 1_000_000;

/// Unidades de tempo por segundo: o mínimo múltiplo comum das taxas de relógio do Quall.
const UNIDADES_POR_SEGUNDO: u32 = 720_000;

/// A janela dos mínimos de trânsito, em microssegundos de chegada.
const JANELA_US: u64 = 1_000_000;

/// Quantas janelas cada track guarda. Com uma de 1 s, 16 s de história.
const JANELAS_GUARDADAS: usize = 16;

/// Quantas janelas comuns à track e à referência são necessárias para escolher o ponto.
const JANELAS_COMUNS_MINIMAS: usize = 2;

/// Das janelas comuns, quantas das mais recentes entram no resíduo.
const JANELAS_DO_RESIDUO: usize = 10;

/// Quanto tempo de resíduo válido é necessário para publicar a deriva entre tracks.
const TEMPO_MINIMO_DA_DERIVA_US: u64 = 60_000_000;

/// Onde o deslocamento de captura de uma track está.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeslocamentoDeCaptura {
    /// Ainda não há medida: a track não tem base, ou não há janelas comuns bastantes com a
    /// referência.
    Ainda,
    /// `timestamp_us` desta track + `us` = captura em µs desde a época da sessão.
    Valido { us: i64 },
    /// Recusado: a guarda viu o resíduo passar do limiar, ou a taxa do relógio não é suportada.
    Recusado { motivo: String },
}

/// O retrato do relógio de uma track, para relatório.
#[derive(Debug, Clone, PartialEq)]
pub struct RetratoDoRelogio {
    /// Esta track fixou a época da sessão.
    pub referencia: bool,
    pub deslocamento: DeslocamentoDeCaptura,
    /// `(mín. trânsito desta − mín. trânsito da referência) − ponto do reticulado`, em µs, sobre
    /// as últimas 10 janelas comuns.
    ///
    /// **Negativo** quer dizer que esta track chega mais cedo do que o carimbo dela diria em
    /// relação à referência: carimbo adiantado, ou envio mais rápido. `None` na referência e
    /// antes da medida.
    pub residuo_us: Option<i64>,
    /// O mesmo, só da última janela comum completa: é o que a guarda olha.
    pub residuo_da_janela_us: Option<i64>,
    /// A inclinação do resíduo, em ppm, depois de 60 s de guarda válida. Com os relógios iguais, é o relógio de mídia do áudio contra o do vídeo, no
    /// emissor.
    pub deriva_entre_tracks_ppm: Option<f64>,
    /// Quantas vezes o resíduo passou do limiar.
    pub violacoes_da_guarda: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Guarda {
    Pendente,
    Valida,
    Recusada,
}

#[derive(Debug)]
struct Track {
    /// Unidades por tique do relógio desta track, ou `None` se a taxa não divide 720 000.
    m: Option<i64>,
    desenrolador: Desenrolador,
    /// `(índice da janela, menor trânsito nela em unidades)`, da mais velha para a mais nova.
    janelas: VecDeque<(u64, i128)>,
    /// O ponto do reticulado escolhido, em unidades. Fixo depois de escolhido.
    ponto: Option<i128>,
    /// Sobre as últimas 10 janelas comuns: o publicado, e a base da deriva.
    residuo: Option<i128>,
    /// Da última janela comum completa: o que a guarda olha.
    residuo_da_janela: Option<i128>,
    guarda: Guarda,
    violacoes: u64,
    /// `(chegada em µs, resíduo em unidades)` do começo do trecho válido, para a deriva.
    inicio_valido: Option<(u64, i128)>,
    /// A última medida válida, para a deriva.
    ultima_valida: Option<(u64, i128)>,
    /// A base do `timestamp_us` que esta track entrega, desenrolada **no momento em que foi
    /// fixada**, em unidades.
    ///
    /// Tem de ser guardada na hora, e não calculada quando alguém pergunta. O desenrolar é pela
    /// diferença com sinal em relação ao maior carimbo já visto, e isso só vale a menos de meia
    /// volta: 6 h 37 min a 90 kHz. Numa sessão mais longa que isso, a base do vídeo calculada no
    /// fim cairia uma volta para o lado errado.
    base: Option<i128>,
}

#[derive(Debug, Default)]
struct Estado {
    tracks: Vec<Track>,
    referencia: Option<usize>,
    /// O carimbo desenrolado do primeiro pacote da referência, em unidades.
    epoca: Option<i128>,
}

/// O relógio comum de uma sessão. Um por sessão, compartilhado pelos `TrackReceptor` dela.
#[derive(Debug)]
pub struct RelogioDaSessao {
    /// O relógio de chegada de **todas** as tracks da sessão. Antes de 18/09/2026 era um
    /// por track, com origens diferentes, e as chegadas de duas tracks não se comparavam.
    chegada: Arc<Clock>,
    estado: Mutex<Estado>,
}

impl RelogioDaSessao {
    pub fn novo() -> Arc<Self> {
        Arc::new(RelogioDaSessao {
            chegada: Arc::new(Clock::new()),
            estado: Mutex::new(Estado::default()),
        })
    }

    /// O relógio de chegada da sessão.
    pub fn relogio_de_chegada(&self) -> Arc<Clock> {
        Arc::clone(&self.chegada)
    }

    /// Registra uma track com a taxa do relógio RTP dela e devolve o índice.
    pub fn registrar(&self, hz: u32) -> usize {
        let m = if hz > 0 && UNIDADES_POR_SEGUNDO % hz == 0 {
            Some(i64::from(UNIDADES_POR_SEGUNDO / hz))
        } else {
            None
        };
        let Ok(mut e) = self.estado.lock() else {
            return usize::MAX;
        };
        e.tracks.push(Track {
            m,
            desenrolador: Desenrolador::default(),
            janelas: VecDeque::with_capacity(JANELAS_GUARDADAS),
            ponto: None,
            residuo: None,
            residuo_da_janela: None,
            guarda: Guarda::Pendente,
            violacoes: 0,
            inicio_valido: None,
            ultima_valida: None,
            base: None,
        });
        e.tracks.len() - 1
    }

    /// Observa um pacote cru da track `indice`. RTCP e lixo são ignorados.
    pub fn observar_pacote(&self, indice: usize, bytes: &[u8], chegada_us: u64) {
        if let Some(carimbo) = carimbo_rtp(bytes) {
            self.observar(indice, carimbo, chegada_us);
        }
    }

    /// Observa o carimbo cru de um pacote da track `indice`, chegado em `chegada_us`.
    pub fn observar(&self, indice: usize, carimbo: u32, chegada_us: u64) {
        let Ok(mut e) = self.estado.lock() else {
            return;
        };
        let e = &mut *e;
        let Some(t) = e.tracks.get_mut(indice) else {
            return;
        };
        let Some(m) = t.m else {
            return;
        };
        let desenrolado = i128::from(t.desenrolador.desenrolar(carimbo)) * i128::from(m);
        if e.referencia.is_none() {
            e.referencia = Some(indice);
            e.epoca = Some(desenrolado);
        }
        let transito = em_unidades(chegada_us) - desenrolado;
        let janela = chegada_us / JANELA_US;
        let nova_janela = match t.janelas.back_mut() {
            Some((j, minimo)) if *j == janela => {
                *minimo = (*minimo).min(transito);
                false
            }
            _ => {
                if t.janelas.len() == JANELAS_GUARDADAS {
                    t.janelas.pop_front();
                }
                t.janelas.push_back((janela, transito));
                true
            }
        };
        if nova_janela {
            atualizar_guardas(e, chegada_us);
        }
    }

    /// Informa a base do `timestamp_us` que a track `indice` entrega: o carimbo cru que o
    /// depacotizador dela fixou como origem. **Chame assim que o depacotizador a fixar**; só a
    /// primeira chamada conta. Ver o campo `base` de `Track` sobre o porquê da pressa.
    pub fn fixar_base(&self, indice: usize, base_bruta: u32) {
        let Ok(mut e) = self.estado.lock() else {
            return;
        };
        let Some(t) = e.tracks.get_mut(indice) else {
            return;
        };
        if t.base.is_some() {
            return;
        }
        if let (Some(m), Some(d)) = (t.m, t.desenrolador.onde_cai(base_bruta)) {
            t.base = Some(i128::from(d) * i128::from(m));
        }
    }

    /// A base já foi fixada para a track `indice`?
    pub fn base_fixada(&self, indice: usize) -> bool {
        self.estado
            .lock()
            .ok()
            .and_then(|e| e.tracks.get(indice).map(|t| t.base.is_some()))
            .unwrap_or(false)
    }

    /// O deslocamento de captura da track `indice`. Ver [`DeslocamentoDeCaptura`].
    pub fn deslocamento(&self, indice: usize) -> DeslocamentoDeCaptura {
        let Ok(e) = self.estado.lock() else {
            return DeslocamentoDeCaptura::Recusado {
                motivo: "o estado do relógio da sessão foi envenenado".into(),
            };
        };
        deslocamento(&e, indice)
    }

    /// O retrato do relógio da track `indice`. `None` antes do primeiro pacote dela.
    pub fn retrato(&self, indice: usize) -> Option<RetratoDoRelogio> {
        let e = self.estado.lock().ok()?;
        let t = e.tracks.get(indice)?;
        if t.m.is_none() {
            // Taxa que não divide 720 000: recusado desde sempre, e o JSON diz isso, em vez de
            // `null` ao lado de um `-1` da fronteira (revisão do código da S1, B7).
            return Some(RetratoDoRelogio {
                referencia: false,
                deslocamento: deslocamento(&e, indice),
                residuo_us: None,
                residuo_da_janela_us: None,
                deriva_entre_tracks_ppm: None,
                violacoes_da_guarda: 0,
            });
        }
        t.desenrolador.onde_cai(0)?;
        let referencia = e.referencia == Some(indice);
        let deriva = match (t.inicio_valido, t.ultima_valida) {
            (Some((t0, r0)), Some((t1, r1))) if t1.saturating_sub(t0) >= TEMPO_MINIMO_DA_DERIVA_US => {
                let dres_us = em_microssegundos(r1 - r0) as f64;
                Some(dres_us / (t1 - t0) as f64 * 1e6)
            }
            _ => None,
        };
        Some(RetratoDoRelogio {
            referencia,
            deslocamento: deslocamento(&e, indice),
            residuo_us: if referencia {
                None
            } else {
                t.residuo.map(em_microssegundos)
            },
            residuo_da_janela_us: if referencia {
                None
            } else {
                t.residuo_da_janela.map(em_microssegundos)
            },
            deriva_entre_tracks_ppm: deriva,
            violacoes_da_guarda: t.violacoes,
        })
    }
}

fn deslocamento(e: &Estado, indice: usize) -> DeslocamentoDeCaptura {
    let Some(t) = e.tracks.get(indice) else {
        return DeslocamentoDeCaptura::Recusado {
            motivo: "track desconhecida do relógio da sessão".into(),
        };
    };
    if t.m.is_none() {
        return DeslocamentoDeCaptura::Recusado {
            motivo: "a taxa do relógio RTP desta track não divide 720 000 Hz".into(),
        };
    }
    let (Some(base), Some(epoca), Some(referencia)) = (t.base, e.epoca, e.referencia) else {
        return DeslocamentoDeCaptura::Ainda;
    };
    let ponto = if indice == referencia {
        if guarda_do_par(e, referencia) == Some(Guarda::Recusada) {
            let quem = e.tracks.iter().enumerate().find(|(k, t)| {
                *k != referencia && t.m.is_some() && t.guarda == Guarda::Recusada
            });
            let (k, residuo) = quem
                .map(|(k, t)| (k, t.residuo_da_janela.map(em_microssegundos).unwrap_or(0)))
                .unwrap_or((usize::MAX, 0));
            return DeslocamentoDeCaptura::Recusado {
                motivo: format!(
                    "a track {k} da sessão recusou o relógio desta, que é a referência: resíduo \
                     de {residuo} µs na última janela dela, contra o limite de \
                     {LIMIAR_DA_GUARDA_US} µs; o par só vale com as duas válidas"
                ),
            };
        }
        0
    } else {
        match (t.guarda, t.ponto) {
            (Guarda::Pendente, _) | (_, None) => return DeslocamentoDeCaptura::Ainda,
            (Guarda::Recusada, _) => {
                return DeslocamentoDeCaptura::Recusado {
                    motivo: format!(
                        "o relógio desta track e o da referência se separaram: resíduo de {} µs \
                         na última janela, contra o limite de {LIMIAR_DA_GUARDA_US} µs",
                        t.residuo_da_janela.map(em_microssegundos).unwrap_or(0)
                    ),
                }
            }
            (Guarda::Valida, Some(p)) => p,
        }
    };
    DeslocamentoDeCaptura::Valido {
        us: em_microssegundos(base + ponto - epoca),
    }
}

/// A guarda, a partir do resíduo da última janela comum completa e do da anterior a ela:
/// - recusa com a última acima de [`LIMIAR_DE_UMA_JANELA_US`];
/// - recusa com as duas acima de [`LIMIAR_DA_GUARDA_US`] (reconferência da S1, B2-novo);
/// - recusada, só volta a valer com a última abaixo de [`LIMIAR_DE_VOLTA_US`]. Sem histerese, um
///   resíduo perto do limiar oscilaria entre válido e recusado a cada janela (revisão do código
///   da S1, B7).
fn decidir_guarda(anterior: Guarda, ultima_us: i64, penultima_us: i64) -> Guarda {
    let (r, r_antes) = (ultima_us.abs(), penultima_us.abs());
    match anterior {
        Guarda::Recusada if r <= LIMIAR_DE_VOLTA_US => Guarda::Valida,
        Guarda::Recusada => Guarda::Recusada,
        _ if r > LIMIAR_DE_UMA_JANELA_US => Guarda::Recusada,
        _ if r > LIMIAR_DA_GUARDA_US && r_antes > LIMIAR_DA_GUARDA_US => Guarda::Recusada,
        _ => Guarda::Valida,
    }
}

/// A guarda do **par**, vista da referência: válida se alguma outra track medida valida o relógio
/// dela; recusada se todas as outras medidas recusam; `None` se nenhuma foi medida ainda.
///
/// A referência não tem resíduo próprio, e antes era válida para sempre: quando era ela que saía
/// do relógio (o áudio que chegou primeiro e deu um degrau), a casca que só conferia o áudio via
/// "válido" e perdia a recusa (revisão do código da S1, B3). **O par só vale com as duas
/// válidas.**
fn guarda_do_par(e: &Estado, referencia: usize) -> Option<Guarda> {
    let mut alguma_recusada = false;
    for (k, t) in e.tracks.iter().enumerate() {
        if k == referencia || t.m.is_none() {
            continue;
        }
        match t.guarda {
            Guarda::Valida => return Some(Guarda::Valida),
            Guarda::Recusada => alguma_recusada = true,
            Guarda::Pendente => {}
        }
    }
    alguma_recusada.then_some(Guarda::Recusada)
}

/// Recalcula o ponto, o resíduo e a guarda de toda track que não é a referência.
///
/// Só entram janelas **completas**: as de índice menor que a janela de `agora_us`. A janela em
/// curso tem poucos pacotes, e o mínimo dela ainda não é o mínimo.
fn atualizar_guardas(e: &mut Estado, agora_us: u64) {
    let Some(r) = e.referencia else {
        return;
    };
    let (Some(janelas_r), Some(m_r)) = (
        e.tracks.get(r).map(|t| t.janelas.clone()),
        e.tracks.get(r).and_then(|t| t.m),
    ) else {
        return;
    };
    let atual = agora_us / JANELA_US;
    for (k, t) in e.tracks.iter_mut().enumerate() {
        if k == r {
            continue;
        }
        let Some(m_k) = t.m else {
            continue;
        };
        // As janelas completas presentes nas duas, das mais novas para as mais velhas:
        // `(mínimo desta, mínimo da referência)`.
        let comuns: Vec<(i128, i128)> = t
            .janelas
            .iter()
            .rev()
            .filter(|(j, _)| *j < atual)
            .filter_map(|(j, tk)| {
                janelas_r
                    .iter()
                    .find(|(jr, _)| jr == j)
                    .map(|(_, tr)| (*tk, *tr))
            })
            .take(JANELAS_DO_RESIDUO)
            .collect();
        if comuns.len() < JANELAS_COMUNS_MINIMAS {
            continue;
        }
        let min_k = comuns.iter().map(|c| c.0).min().unwrap_or(0);
        let min_r = comuns.iter().map(|c| c.1).min().unwrap_or(0);
        let estimado = min_k - min_r;
        let ponto = *t.ponto.get_or_insert_with(|| {
            let passo = passo_do_reticulado(m_k, m_r);
            arredondar_ao_multiplo(estimado, passo)
        });
        // O resíduo publicado e a deriva usam as últimas 10 janelas: é mais liso. A **guarda**
        // usa só a última janela completa: com o mínimo de 10 janelas, um degrau para trás (o
        // carimbo que fica atrasado, o `reabrir` do Mac) só aparecia 10 s depois, porque o
        // mínimo continuava preso às janelas de antes (revisão do código da S1, B2).
        let residuo = estimado - ponto;
        let (tk, tr) = comuns[0];
        let da_janela = (tk - tr) - ponto;
        // `comuns` tem pelo menos [`JANELAS_COMUNS_MINIMAS`] = 2 entradas.
        let (tk_antes, tr_antes) = comuns[1];
        let da_janela_antes = (tk_antes - tr_antes) - ponto;
        t.residuo = Some(residuo);
        t.residuo_da_janela = Some(da_janela);
        let nova = decidir_guarda(
            t.guarda,
            em_microssegundos(da_janela),
            em_microssegundos(da_janela_antes),
        );
        if nova == Guarda::Valida {
            if t.guarda != Guarda::Valida {
                t.inicio_valido = Some((agora_us, residuo));
            }
            t.ultima_valida = Some((agora_us, residuo));
        } else {
            if t.guarda != Guarda::Recusada {
                t.violacoes += 1;
            }
            t.inicio_valido = None;
            t.ultima_valida = None;
        }
        t.guarda = nova;
    }
    // A referência não tem resíduo próprio: a guarda dela é a do par, e a recusa do par conta
    // como violação nela também. Antes, a referência recusada saía no JSON com
    // `guard_violations: 0`, sem número que explicasse (reconferência da S1, B3).
    let par = guarda_do_par(e, r).unwrap_or(Guarda::Pendente);
    if let Some(t) = e.tracks.get_mut(r) {
        if par == Guarda::Recusada && t.guarda != Guarda::Recusada {
            t.violacoes += 1;
        }
        t.guarda = par;
    }
}

/// O passo do reticulado de voltas entre duas tracks, em unidades: `2³² × mdc(m₁, m₂)`.
fn passo_do_reticulado(m1: i64, m2: i64) -> i128 {
    (1i128 << 32) * i128::from(mdc(m1, m2))
}

fn mdc(a: i64, b: i64) -> i64 {
    if b == 0 {
        a.abs()
    } else {
        mdc(b, a % b)
    }
}

/// O múltiplo de `passo` mais próximo de `x`.
fn arredondar_ao_multiplo(x: i128, passo: i128) -> i128 {
    let metade = passo / 2;
    (x + metade).div_euclid(passo) * passo
}

/// Microssegundos → unidades de 1/720 000 s (× 18/25).
fn em_unidades(us: u64) -> i128 {
    i128::from(us) * 18 / 25
}

/// Unidades de 1/720 000 s → microssegundos (× 25/18), com arredondamento para baixo.
fn em_microssegundos(unidades: i128) -> i64 {
    let us = (unidades * 25).div_euclid(18);
    us.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// O carimbo de um pacote RTP cru, ou `None` se não for RTP de mídia: curto demais, versão
/// errada, ou RTCP multiplexado (tipos 64 a 95 no segundo byte, RFC 5761 §4).
fn carimbo_rtp(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < crate::rtp::RTP_HEADER_LEN || bytes[0] >> 6 != 2 {
        return None;
    }
    let pt = bytes[1] & 0x7f;
    if (64..=95).contains(&pt) {
        return None;
    }
    Some(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ_VIDEO: u32 = 90_000;
    const HZ_OPUS: u32 = 48_000;

    fn carimbo(us: u64, hz: u32) -> u32 {
        crate::rtp::micros_para_carimbo_em(us, hz)
    }

    /// Uma sessão simulada: vídeo a 30 q/s e Opus a 50 pacotes/s, os dois carimbados pelo mesmo
    /// relógio do emissor a partir de `origem_us`. A chegada é `emissor + transito`, com o
    /// relógio do receptor andando `ppm_receptor` mais rápido. `mexer_no_audio(t)` desloca o
    /// carimbo do áudio em função do tempo de emissão, para simular relógio trocado.
    struct Simulacao {
        relogio: Arc<RelogioDaSessao>,
        video: usize,
        audio: usize,
    }

    impl Simulacao {
        fn nova() -> Self {
            let relogio = RelogioDaSessao::novo();
            let video = relogio.registrar(HZ_VIDEO);
            let audio = relogio.registrar(HZ_OPUS);
            Simulacao {
                relogio,
                video,
                audio,
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn correr(
            &self,
            origem_us: u64,
            de_s: u64,
            ate_s: u64,
            ppm_receptor: f64,
            atraso_de_envio_audio_us: u64,
            mexer_no_audio: impl Fn(u64) -> i64,
            com_video: bool,
        ) {
            let fim = ate_s * 1_000_000;
            let mut t = de_s * 1_000_000;
            let mut proximo_video = t;
            let mut proximo_audio = t;
            while t < fim {
                let chegada = |emissao: u64, atraso: u64| {
                    let e = emissao + atraso + 3_000;
                    (e as f64 * (1.0 + ppm_receptor * 1e-6)) as u64
                };
                if com_video && proximo_video <= t {
                    let c = carimbo(origem_us + proximo_video, HZ_VIDEO);
                    self.relogio.observar(self.video, c, chegada(proximo_video, 8_000));
                    proximo_video += 33_333;
                }
                if proximo_audio <= t {
                    let captura = (origem_us + proximo_audio) as i64 + mexer_no_audio(proximo_audio);
                    let c = carimbo(captura as u64, HZ_OPUS);
                    self.relogio
                        .observar(self.audio, c, chegada(proximo_audio, atraso_de_envio_audio_us));
                    proximo_audio += 20_000;
                }
                t += 1_000;
            }
        }

        fn deslocamento(&self, indice: usize, base_us: u64, hz: u32) -> DeslocamentoDeCaptura {
            self.relogio.fixar_base(indice, carimbo(base_us, hz));
            self.relogio.deslocamento(indice)
        }
    }

    /// O mesmo relógio, com o emissor ligado há 30 h: os dois contadores já deram voltas em
    /// números diferentes. O deslocamento tem de ligar os dois ao microssegundo.
    #[test]
    fn o_mesmo_relogio_com_voltas_diferentes_liga_ao_microssegundo() {
        let s = Simulacao::nova();
        let origem = 30 * 3_600_000_000u64 + 123_457;
        s.correr(origem, 0, 5, 0.0, 20_000, |_| 0, true);
        // A base de cada track é o primeiro carimbo dela; o vídeo começou em origem + 0 e o áudio
        // em origem + 0 também.
        let v = s.deslocamento(s.video, origem, HZ_VIDEO);
        let a = s.deslocamento(s.audio, origem, HZ_OPUS);
        let (DeslocamentoDeCaptura::Valido { us: dv }, DeslocamentoDeCaptura::Valido { us: da }) =
            (v.clone(), a.clone())
        else {
            panic!("os dois tinham de ser válidos: vídeo {v:?}, áudio {a:?}");
        };
        assert!(
            (da - dv).abs() <= 25,
            "a mesma captura tem de cair no mesmo instante: vídeo {dv}, áudio {da}"
        );
        let retrato = s.relogio.retrato(s.audio).unwrap();
        let residuo = retrato.residuo_us.unwrap();
        assert!(
            (residuo - 12_000).abs() < 2_000,
            "o resíduo é a diferença de atraso de envio (20 − 8 ms), e deu {residuo} µs"
        );
    }

    /// O vídeo atravessando a volta de 90 kHz no meio da corrida.
    #[test]
    fn a_volta_do_video_no_meio_nao_move_o_deslocamento() {
        let s = Simulacao::nova();
        // A volta de 90 kHz: 2³² / 90 000 s ≈ 47 721,858 s. A origem fica 2 s antes dela.
        let volta_us = (1u64 << 32) * 1_000_000 / 90_000;
        let origem = volta_us - 2_000_000;
        s.correr(origem, 0, 6, 0.0, 20_000, |_| 0, true);
        let v = s.deslocamento(s.video, origem, HZ_VIDEO);
        let a = s.deslocamento(s.audio, origem, HZ_OPUS);
        match (v, a) {
            (DeslocamentoDeCaptura::Valido { us: dv }, DeslocamentoDeCaptura::Valido { us: da }) => {
                assert!((da - dv).abs() <= 25, "vídeo {dv}, áudio {da}")
            }
            outro => panic!("{outro:?}"),
        }
    }

    /// O áudio num relógio 3 s adiantado, abaixo da guarda antiga de 5 s: recusado. É a corrida
    /// `d3` da crítica 3.
    #[test]
    fn relogios_de_track_diferentes_abaixo_de_cinco_segundos_sao_recusados() {
        for deslocamento_us in [3_000_000i64, 1_000_000, -400_000, 200_000] {
            let s = Simulacao::nova();
            let origem = 5 * 3_600_000_000u64;
            s.correr(origem, 0, 5, 0.0, 20_000, |_| deslocamento_us, true);
            let a = s.deslocamento(s.audio, origem, HZ_OPUS);
            assert!(
                matches!(a, DeslocamentoDeCaptura::Recusado { .. }),
                "{deslocamento_us} µs de descasamento tinha de ser recusado, e deu {a:?}"
            );
        }
    }

    /// O atraso de envio legítimo passa: 30 ms a mais no áudio é a corrida `c30` do §2.
    #[test]
    fn atraso_de_envio_legitimo_passa_e_nao_move_o_deslocamento() {
        let s = Simulacao::nova();
        let origem = 1_000_000_000u64;
        s.correr(origem, 0, 5, 0.0, 38_000, |_| 0, true);
        let v = s.deslocamento(s.video, origem, HZ_VIDEO);
        let a = s.deslocamento(s.audio, origem, HZ_OPUS);
        match (v, a) {
            (DeslocamentoDeCaptura::Valido { us: dv }, DeslocamentoDeCaptura::Valido { us: da }) => {
                assert!((da - dv).abs() <= 25, "vídeo {dv}, áudio {da}")
            }
            outro => panic!("{outro:?}"),
        }
    }

    /// Uma track que começa 7 h depois da outra, com o relógio do receptor 200 ppm mais rápido:
    /// a guarda da primeira versão (primeiras chegadas) recusaria estando certa.
    #[test]
    fn track_que_comeca_sete_horas_depois_com_deriva_nao_e_recusada() {
        let s = Simulacao::nova();
        let origem = 2_000_000_000u64;
        let chegada = |e: u64, atraso: u64| ((e + atraso + 3_000) as f64 * 1.0002) as u64;
        let sete_horas: u64 = 7 * 3600;
        // Só vídeo por 7 h, quadro a quadro: o desenrolar precisa ver o fluxo contínuo, como
        // na sessão de verdade.
        // O primeiro quadro, e a base fixada na hora, como a bomba do `TrackReceptor` faz.
        s.relogio
            .observar(s.video, carimbo(origem, HZ_VIDEO), chegada(0, 8_000));
        s.relogio.fixar_base(s.video, carimbo(origem, HZ_VIDEO));
        let mut pv = 33_333u64;
        while pv < sete_horas * 1_000_000 {
            s.relogio
                .observar(s.video, carimbo(origem + pv, HZ_VIDEO), chegada(pv, 8_000));
            pv += 33_333;
        }
        // Daí em diante, vídeo e áudio por 5 s.
        let fim = (sete_horas + 5) * 1_000_000;
        let mut pa = sete_horas * 1_000_000;
        while pv < fim || pa < fim {
            if pv <= pa {
                s.relogio
                    .observar(s.video, carimbo(origem + pv, HZ_VIDEO), chegada(pv, 8_000));
                pv += 33_333;
            } else {
                s.relogio
                    .observar(s.audio, carimbo(origem + pa, HZ_OPUS), chegada(pa, 20_000));
                pa += 20_000;
            }
        }
        let base_audio = origem + sete_horas * 1_000_000;
        let a = s.deslocamento(s.audio, base_audio, HZ_OPUS);
        let v = s.deslocamento(s.video, origem, HZ_VIDEO);
        match (v, a) {
            (DeslocamentoDeCaptura::Valido { us: dv }, DeslocamentoDeCaptura::Valido { us: da }) => {
                let esperado = (sete_horas * 1_000_000) as i64;
                assert!(
                    ((da - dv) - esperado).abs() <= 25,
                    "o áudio começou 7 h depois: {} µs de diferença",
                    da - dv
                );
            }
            outro => panic!("{outro:?}"),
        }
    }

    /// Um degrau no meio da sessão: o carimbo do áudio fica 2 s para trás a partir de 10 s — o
    /// `reabrir` do Mac (crítica 3 §1b). Válido antes, recusado depois, uma violação.
    #[test]
    fn degrau_no_meio_da_sessao_e_pego_pela_guarda_continua() {
        let s = Simulacao::nova();
        let origem = 3_000_000_000u64;
        let mexer = |t: u64| if t >= 10_000_000 { -2_000_000 } else { 0 };
        s.correr(origem, 0, 10, 0.0, 20_000, mexer, true);
        assert!(matches!(
            s.deslocamento(s.audio, origem, HZ_OPUS),
            DeslocamentoDeCaptura::Valido { .. }
        ));
        s.correr(origem, 10, 25, 0.0, 20_000, mexer, true);
        assert!(matches!(
            s.deslocamento(s.audio, origem, HZ_OPUS),
            DeslocamentoDeCaptura::Recusado { .. }
        ));
        let r = s.relogio.retrato(s.audio).unwrap();
        assert_eq!(r.violacoes_da_guarda, 1);
    }

    /// O relógio de mídia do áudio 100 ppm mais rápido que o do vídeo: a deriva entre tracks
    /// aparece como a inclinação do resíduo.
    #[test]
    fn a_deriva_entre_tracks_aparece_na_inclinacao_do_residuo() {
        let s = Simulacao::nova();
        let origem = 4_000_000_000u64;
        // O carimbo do áudio anda 100 ppm mais depressa: aos t µs, está t × 1e-4 à frente.
        s.correr(origem, 0, 130, 0.0, 20_000, |t| (t / 10_000) as i64, true);
        let r = s.relogio.retrato(s.audio).unwrap();
        let ppm = r.deriva_entre_tracks_ppm.expect("depois de 60 s válidos tinha de haver deriva");
        // Carimbo adiantado → chega "mais cedo" que o carimbo diz → resíduo desce.
        assert!((ppm + 100.0).abs() < 5.0, "deu {ppm} ppm");
    }

    /// Taxa que não divide 720 000: recusada, com motivo.
    #[test]
    fn taxa_nao_suportada_e_recusada() {
        let relogio = RelogioDaSessao::novo();
        let i = relogio.registrar(44_100);
        relogio.observar(i, 1000, 0);
        assert!(matches!(
            { relogio.fixar_base(i, 1000); relogio.deslocamento(i) },
            DeslocamentoDeCaptura::Recusado { .. }
        ));
    }

    /// Antes de haver janelas comuns, é "ainda".
    #[test]
    fn sem_janelas_comuns_e_ainda() {
        let s = Simulacao::nova();
        s.correr(0, 0, 1, 0.0, 20_000, |_| 0, false);
        assert_eq!(
            s.deslocamento(s.audio, 0, HZ_OPUS),
            DeslocamentoDeCaptura::Valido { us: 0 },
            "o áudio é a referência aqui, e a referência é válida desde a base"
        );
        assert_eq!(
            s.deslocamento(s.video, 0, HZ_VIDEO),
            DeslocamentoDeCaptura::Ainda,
            "o vídeo não chegou"
        );
    }

    /// O que o nome acima prometia e não testava (revisão do código da S1, B7): uma track que
    /// **não** é a referência, com uma janela comum só, ainda não tem deslocamento.
    #[test]
    fn track_com_uma_janela_comum_so_e_ainda() {
        let s = Simulacao::nova();
        let origem = 7_000_000_000u64;
        // 1,9 s: a janela 0 fica completa quando chega um pacote da janela 1; só ela é comum.
        s.correr(origem, 0, 1, 0.0, 20_000, |_| 0, true);
        s.relogio
            .observar(s.video, carimbo(origem + 1_900_000, HZ_VIDEO), 1_900_000 + 11_000);
        assert_eq!(
            s.deslocamento(s.audio, origem, HZ_OPUS),
            DeslocamentoDeCaptura::Ainda,
            "com uma janela comum só, não há ponto do reticulado"
        );
    }

    /// **B2**: a guarda pega o degrau nos dois sentidos. Antes, o degrau para trás levava 10 s,
    /// porque o mínimo de 10 janelas continuava preso às de antes.
    ///
    /// Os prazos (reconferência da S1, B2-novo):
    /// - acima de 1 s, uma janela basta: **até 2 s** nos dois sentidos. É o `reabrir` do Mac;
    /// - de 150 ms a 1 s, a guarda pede duas janelas seguidas: **até 2 s** para a frente e **até
    ///   3 s** para trás. Um degrau para trás aos 10,0 s deixa na janela 10 o pacote de antes
    ///   dele, que chega aos 10,003 s; a primeira janela limpa é a 11, completa aos 12 s, e a
    ///   segunda, aos 13 s. Uma janela só não separa esse degrau de uma janela inteira atrasada
    ///   (`uma_janela_inteira_atrasada_numa_track_so_nao_recusa`).
    #[test]
    fn a_guarda_pega_o_degrau_nos_dois_sentidos() {
        for (degrau, prazo_s) in [
            (-2_000_000i64, 2u64),
            (2_000_000, 2),
            (-1_500_000, 2),
            (1_500_000, 2),
            (-300_000, 3),
            (300_000, 2),
        ] {
            let s = Simulacao::nova();
            let origem = 3_000_000_000u64;
            let mexer = move |t: u64| if t >= 10_000_000 { degrau } else { 0 };
            s.correr(origem, 0, 10, 0.0, 20_000, mexer, true);
            assert!(
                matches!(
                    s.deslocamento(s.audio, origem, HZ_OPUS),
                    DeslocamentoDeCaptura::Valido { .. }
                ),
                "degrau {degrau}: válido antes"
            );
            let mut recusou_em = None;
            for seg in 11..=20u64 {
                s.correr(origem, seg - 1, seg, 0.0, 20_000, mexer, true);
                if recusou_em.is_none()
                    && matches!(
                        s.deslocamento(s.audio, origem, HZ_OPUS),
                        DeslocamentoDeCaptura::Recusado { .. }
                    )
                {
                    recusou_em = Some(seg - 10);
                }
            }
            eprintln!("B2 degrau de {degrau} µs aos 10 s: recusado {recusou_em:?} s depois");
            let s_depois = recusou_em.expect("tinha de recusar");
            assert!(
                s_depois <= prazo_s,
                "degrau {degrau}: recusado {s_depois} s depois, e o prazo é {prazo_s} s"
            );
        }
    }

    /// **B3**: o áudio chega primeiro, vira a referência, e é o carimbo **dele** que dá um degrau
    /// de 2 s. Antes: a referência (o áudio, que saiu do relógio) seguia válida, e só o vídeo,
    /// que estava certo, era recusado. Agora o par é recusado nos dois lados.
    #[test]
    fn a_referencia_e_recusada_com_o_par() {
        let r = RelogioDaSessao::novo();
        let a = r.registrar(HZ_OPUS);
        let v = r.registrar(HZ_VIDEO);
        let origem = 3_000_000_000u64;
        let (mut ta, mut tv) = (0u64, 5_000u64);
        let mut fixou = (false, false);
        let mut valido_antes = None;
        while ta < 30_000_000 || tv < 30_000_000 {
            if ta <= tv {
                let degrau = if ta >= 10_000_000 { 2_000_000 } else { 0 };
                let k = carimbo(origem + ta - degrau, HZ_OPUS);
                r.observar(a, k, ta + 20_000);
                if !fixou.0 {
                    r.fixar_base(a, k);
                    fixou.0 = true;
                }
                ta += 20_000;
            } else {
                let k = carimbo(origem + tv, HZ_VIDEO);
                r.observar(v, k, tv + 8_000);
                if !fixou.1 {
                    r.fixar_base(v, k);
                    fixou.1 = true;
                }
                tv += 33_333;
            }
            if ta == 9_000_000 {
                valido_antes = Some((r.deslocamento(a), r.deslocamento(v)));
            }
        }
        let (a_antes, v_antes) = valido_antes.expect("amostrado aos 9 s");
        assert!(matches!(a_antes, DeslocamentoDeCaptura::Valido { .. }), "{a_antes:?}");
        assert!(matches!(v_antes, DeslocamentoDeCaptura::Valido { .. }), "{v_antes:?}");
        assert!(
            matches!(r.deslocamento(a), DeslocamentoDeCaptura::Recusado { .. }),
            "a referência que saiu do relógio tinha de ser recusada: {:?}",
            r.deslocamento(a)
        );
        assert!(matches!(r.deslocamento(v), DeslocamentoDeCaptura::Recusado { .. }));
    }

    /// **B7**: taxa que não divide 720 000 tem retrato, e ele diz "recusado", e não `None`.
    #[test]
    fn taxa_nao_suportada_tem_retrato_recusado() {
        let relogio = RelogioDaSessao::novo();
        let i = relogio.registrar(44_100);
        let r = relogio.retrato(i).expect("retrato mesmo sem pacote");
        assert!(matches!(r.deslocamento, DeslocamentoDeCaptura::Recusado { .. }));
    }

    /// **B7**: a guarda tem histerese. Um resíduo perto do limiar não oscila a cada janela.
    #[test]
    fn a_guarda_tem_histerese() {
        // Uma janela a 155 ms não recusa; a segunda seguida, sim. Recusada, só volta abaixo de
        // 120 ms. Acima de 1 s, uma janela basta.
        let sequencia = [
            140_000i64, 155_000, 145_000, 155_000, 160_000, 145_000, 125_000, 115_000, 145_000,
            1_200_000, 100_000,
        ];
        let mut g = Guarda::Pendente;
        let mut anterior = 0i64;
        let mut estados = Vec::new();
        for r in sequencia {
            g = decidir_guarda(g, r, anterior);
            anterior = r;
            estados.push(g);
        }
        use Guarda::{Recusada as R, Valida as V};
        assert_eq!(estados, vec![V, V, V, V, R, R, R, V, V, R, V]);
    }

    #[test]
    fn o_passo_do_reticulado_e_o_da_conta() {
        assert_eq!(passo_do_reticulado(8, 15), 1i128 << 32);
        assert_eq!(passo_do_reticulado(8, 90), 1i128 << 33);
        assert_eq!(passo_do_reticulado(8, 8), 1i128 << 35);
        assert_eq!(arredondar_ao_multiplo(-3, 10), 0);
        assert_eq!(arredondar_ao_multiplo(-6, 10), -10);
        assert_eq!(arredondar_ao_multiplo(14, 10), 10);
    }

    #[test]
    fn rtcp_nao_e_observado() {
        let mut sr = vec![0x80u8, 200, 0, 6];
        sr.extend_from_slice(&[0u8; 24]);
        assert_eq!(carimbo_rtp(&sr), None);
        let mut rtp = vec![0x80u8, 111, 0, 1, 0, 0, 0x03, 0xe8];
        rtp.extend_from_slice(&[0u8; 4]);
        assert_eq!(carimbo_rtp(&rtp), Some(1000));
    }

    /// Os relógios iguais, e **uma** track com todos os pacotes de uma janela de chegada atrasados:
    /// a emissão de 19,5 s a 21,5 s chega `atraso_extra_us` mais tarde, e só a janela de chegada
    /// 20 fica inteira atrasada. É o cenário do revisor (`criticas-som/7-reconferencia-b.md`).
    /// Devolve as transições `(ms de chegada, estado do áudio, estado do vídeo)`.
    fn janela_atrasada(atraso_extra_us: u64, no_video: bool) -> Vec<(u64, char, char)> {
        let r = RelogioDaSessao::novo();
        let v = r.registrar(HZ_VIDEO);
        let a = r.registrar(HZ_OPUS);
        let origem = 3_000_000_000u64;
        let extra = |t: u64| {
            if (19_500_000..21_500_000).contains(&t) {
                atraso_extra_us
            } else {
                0
            }
        };
        let mut eventos: Vec<(u64, u32, usize)> = Vec::new();
        let mut t = 0u64;
        while t < 30_000_000 {
            let ex = if no_video { extra(t) } else { 0 };
            eventos.push((t + 8_000 + ex, carimbo(origem + t, HZ_VIDEO), v));
            t += 33_333;
        }
        let mut t = 0u64;
        while t < 30_000_000 {
            let ex = if no_video { 0 } else { extra(t) };
            eventos.push((t + 20_000 + ex, carimbo(origem + t, HZ_OPUS), a));
            t += 20_000;
        }
        eventos.sort();
        let letra = |d: DeslocamentoDeCaptura| match d {
            DeslocamentoDeCaptura::Valido { .. } => 'V',
            DeslocamentoDeCaptura::Recusado { .. } => 'R',
            DeslocamentoDeCaptura::Ainda => 'A',
        };
        let mut fixou = [false, false];
        let mut transicoes = Vec::new();
        let mut ultimo = (' ', ' ');
        for (chegada, c, k) in eventos {
            r.observar(k, c, chegada);
            if !fixou[k] {
                r.fixar_base(k, c);
                fixou[k] = true;
            }
            let e = (letra(r.deslocamento(a)), letra(r.deslocamento(v)));
            if e != ultimo {
                transicoes.push((chegada / 1_000, e.0, e.1));
                ultimo = e;
            }
        }
        transicoes
    }

    /// **B2-novo da reconferência (miúdo).** Uma janela de 1 s inteira atrasada numa track só,
    /// com os relógios certos, não recusa o par. Com a guarda de uma janela, os quatro casos do
    /// revisor recusavam as duas tracks por 1 s. Acima de 1 s, uma janela basta para recusar, de
    /// propósito: é o degrau de relógio que precisa sair em até 2 s.
    #[test]
    fn uma_janela_inteira_atrasada_numa_track_so_nao_recusa() {
        for (extra, no_video) in [
            (160_000u64, true),
            (250_000, true),
            (160_000, false),
            (400_000, false),
            (900_000, false),
        ] {
            let t = janela_atrasada(extra, no_video);
            eprintln!(
                "B2-novo: janela 20 atrasada {} ms no {}: transições (ms, áudio, vídeo) {t:?}",
                extra / 1_000,
                if no_video { "vídeo (referência)" } else { "áudio" }
            );
            assert!(
                t.iter().all(|x| x.1 != 'R' && x.2 != 'R'),
                "{} ms no {}: recusou com os relógios certos: {t:?}",
                extra / 1_000,
                if no_video { "vídeo" } else { "áudio" }
            );
        }
        let t = janela_atrasada(1_200_000, false);
        eprintln!("B2-novo: janela 20 atrasada 1200 ms no áudio: {t:?}");
        assert!(t.iter().any(|x| x.1 == 'R'), "acima de 1 s, uma janela recusa: {t:?}");
    }

    /// **B3, o miúdo da reconferência.** A referência recusada com o par diz por quê: a
    /// violação é contada nela, e o motivo nomeia a track que recusou e o resíduo dela.
    #[test]
    fn a_referencia_recusada_diz_por_que() {
        let r = RelogioDaSessao::novo();
        let a = r.registrar(HZ_OPUS);
        let v = r.registrar(HZ_VIDEO);
        let origem = 3_000_000_000u64;
        let (mut ta, mut tv) = (0u64, 5_000u64);
        let mut fixou = (false, false);
        while ta < 20_000_000 || tv < 20_000_000 {
            if ta <= tv {
                let degrau = if ta >= 10_000_000 { 2_000_000 } else { 0 };
                let k = carimbo(origem + ta - degrau, HZ_OPUS);
                r.observar(a, k, ta + 20_000);
                if !fixou.0 {
                    r.fixar_base(a, k);
                    fixou.0 = true;
                }
                ta += 20_000;
            } else {
                let k = carimbo(origem + tv, HZ_VIDEO);
                r.observar(v, k, tv + 8_000);
                if !fixou.1 {
                    r.fixar_base(v, k);
                    fixou.1 = true;
                }
                tv += 33_333;
            }
        }
        let ra = r.retrato(a).expect("retrato da referência");
        eprintln!("B3 miúdo: retrato da referência recusada: {ra:?}");
        assert!(ra.referencia);
        let DeslocamentoDeCaptura::Recusado { motivo } = &ra.deslocamento else {
            panic!("a referência tinha de estar recusada: {ra:?}");
        };
        assert_eq!(ra.violacoes_da_guarda, 1, "a violação do par conta na referência: {ra:?}");
        assert!(motivo.contains(&format!("track {v}")), "o motivo nomeia a outra track: {motivo}");
        assert!(motivo.contains("µs"), "o motivo traz o resíduo: {motivo}");
    }
}
