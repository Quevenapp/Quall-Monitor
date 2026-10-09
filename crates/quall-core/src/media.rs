//! Pipeline de mídia: quadros, presets de encode e a medição de latência.
//!
//! No M1 ainda não há encode de verdade. O que existe aqui é o **quadro sintético com timestamp
//! embutido**, que é o instrumento com que a latência ponta a ponta é medida — e é melhor que
//! vídeo real para isso, porque separa o custo da rede do custo do encoder. Quando o
//! VideoToolbox e o MediaCodec entrarem, o cabeçalho continua o mesmo e a diferença entre os
//! dois números é exatamente o preço do encode.
//!
//! # Como a latência é medida sem sincronizar relógio
//!
//! Os dois aparelhos têm relógios diferentes, e alinhá-los mediria o NTP, não o Quall. Então o
//! emissor carimba o quadro com o **próprio** relógio monotônico, o receptor devolve o
//! cabeçalho intacto ([`FrameKind::Echo`]) e o emissor calcula `agora - carimbo`. Isso é o
//! tempo de ida e volta; a latência de um sentido é metade dele, sob a hipótese de caminho
//! simétrico — que numa LAN comutada é razoável, e que fica dita em vez de escondida.
//!
//! O que este número **não** é: latência de vidro a vidro. Não tem captura, encode, decode nem
//! apresentação. É o piso do transporte, e é contra ele que o custo de cada etapa seguinte vai
//! ser cobrado.

use std::time::Instant;

use crate::error::{Error, Result};
use crate::protocol::EncodePreset;

/// Marca de um quadro do Quall. Muda se o formato do cabeçalho mudar.
pub const FRAME_MAGIC: [u8; 4] = *b"QUL1";

/// Tamanho do cabeçalho, em bytes. Fixo e explícito para que o `armeabi-v7a` do A10s e o
/// `aarch64` do iPhone leiam a mesma coisa: nada de `size_of::<usize>()` no protocolo.
pub const HEADER_LEN: usize = 24;

/// Tipo do quadro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    /// Quadro sintético indo do emissor para o receptor.
    Pattern = 1,
    /// Cabeçalho devolvido pelo receptor, sem carga, para fechar a conta do tempo.
    Echo = 2,
}

impl FrameKind {
    fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(FrameKind::Pattern),
            2 => Ok(FrameKind::Echo),
            outro => Err(Error::Protocol(format!(
                "tipo de quadro desconhecido: {outro}"
            ))),
        }
    }
}

/// Cabeçalho de um quadro.
///
/// Tudo em little-endian, escrito byte a byte. Não é `#[repr(C)]` nem transmutado: alinhamento
/// e *padding* variam entre ARM de 32 e de 64 bits, e o protocolo não pode depender disso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    pub seq: u32,
    /// Microssegundos no relógio monotônico **do emissor**. Opaco para o receptor: ele só
    /// devolve.
    pub sent_at_micros: u64,
    pub payload_len: u32,
}

impl FrameHeader {
    /// Escreve o cabeçalho no começo de `destino`.
    ///
    /// Recebe uma fatia em vez de devolver um `Vec` porque o emissor reaproveita o mesmo buffer
    /// quadro após quadro. Alocar por quadro é exatamente o que a extension de 50 MB não pode.
    pub fn write(&self, destino: &mut [u8]) -> Result<()> {
        if destino.len() < HEADER_LEN {
            return Err(Error::Invalid(format!(
                "buffer de {} bytes, cabeçalho precisa de {HEADER_LEN}",
                destino.len()
            )));
        }
        destino[0..4].copy_from_slice(&FRAME_MAGIC);
        destino[4] = self.kind as u8;
        destino[5] = 0;
        destino[6] = 0;
        destino[7] = 0;
        destino[8..12].copy_from_slice(&self.seq.to_le_bytes());
        destino[12..20].copy_from_slice(&self.sent_at_micros.to_le_bytes());
        destino[20..24].copy_from_slice(&self.payload_len.to_le_bytes());
        Ok(())
    }

    pub fn parse(origem: &[u8]) -> Result<Self> {
        if origem.len() < HEADER_LEN {
            return Err(Error::Protocol(format!(
                "quadro de {} bytes, menor que o cabeçalho",
                origem.len()
            )));
        }
        if origem[0..4] != FRAME_MAGIC {
            return Err(Error::Protocol("quadro sem a marca do Quall".into()));
        }
        let kind = FrameKind::from_u8(origem[4])?;
        // `expect` sobre `try_into` de fatia de tamanho conhecido: o comprimento já foi
        // conferido acima, então o caminho é inalcançável — mas escrito sem `unwrap` mudo.
        let seq = u32::from_le_bytes(
            origem[8..12]
                .try_into()
                .map_err(|_| Error::Protocol("cabeçalho truncado".into()))?,
        );
        let sent_at_micros = u64::from_le_bytes(
            origem[12..20]
                .try_into()
                .map_err(|_| Error::Protocol("cabeçalho truncado".into()))?,
        );
        let payload_len = u32::from_le_bytes(
            origem[20..24]
                .try_into()
                .map_err(|_| Error::Protocol("cabeçalho truncado".into()))?,
        );
        Ok(FrameHeader {
            kind,
            seq,
            sent_at_micros,
            payload_len,
        })
    }
}

/// Relógio monotônico do processo, em microssegundos.
///
/// Monotônico e não de parede: mudar o fuso, o horário de verão ou o NTP ajustar o relógio no
/// meio da medição não pode virar latência negativa.
#[derive(Debug, Clone)]
pub struct Clock {
    inicio: Instant,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock {
    pub fn new() -> Self {
        Clock {
            inicio: Instant::now(),
        }
    }

    pub fn micros(&self) -> u64 {
        // `as u64` sobre `u128` só estouraria depois de ~584 mil anos de processo.
        self.inicio.elapsed().as_micros() as u64
    }
}

/// Gerador do padrão sintético.
///
/// A carga não é lixo aleatório: é uma sequência determinística derivada de `seq`, para que o
/// receptor **verifique** que o que chegou é o que saiu. Sem isso a sonda mediria latência de
/// bytes corrompidos sem perceber.
pub struct SyntheticPattern {
    payload_len: usize,
    seq: u32,
    clock: Clock,
    buffer: Vec<u8>,
}

impl SyntheticPattern {
    /// `payload_len` é a carga além do cabeçalho.
    ///
    /// O buffer é alocado uma vez, aqui, e reaproveitado em todo quadro.
    pub fn new(payload_len: usize) -> Self {
        SyntheticPattern {
            payload_len,
            seq: 0,
            clock: Clock::new(),
            buffer: vec![0u8; HEADER_LEN + payload_len],
        }
    }

    pub fn frame_len(&self) -> usize {
        HEADER_LEN + self.payload_len
    }

    /// Monta o próximo quadro e devolve uma vista do buffer interno.
    pub fn next_frame(&mut self) -> Result<&[u8]> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let header = FrameHeader {
            kind: FrameKind::Pattern,
            seq,
            sent_at_micros: self.clock.micros(),
            payload_len: u32::try_from(self.payload_len)
                .map_err(|_| Error::Invalid("carga maior que 4 GiB".into()))?,
        };
        header.write(&mut self.buffer)?;
        preencher_padrao(&mut self.buffer[HEADER_LEN..], seq);
        Ok(&self.buffer)
    }

    /// Relógio do emissor, para fechar a conta quando o eco voltar.
    pub fn clock(&self) -> &Clock {
        &self.clock
    }
}

/// Escreve o padrão determinístico de `seq` sobre `destino`.
///
/// Um xorshift de 32 bits: barato o suficiente para não competir com o encode pela CPU do A10s,
/// e diferente o suficiente entre quadros vizinhos para que um quadro repetido não passe por
/// íntegro.
pub fn preencher_padrao(destino: &mut [u8], seq: u32) {
    let mut estado = seq.wrapping_mul(2_654_435_761).wrapping_add(0x9E37_79B9);
    if estado == 0 {
        estado = 0x1234_5678;
    }
    for byte in destino.iter_mut() {
        estado ^= estado << 13;
        estado ^= estado >> 17;
        estado ^= estado << 5;
        *byte = (estado & 0xff) as u8;
    }
}

/// Confere que a carga é a que `seq` deveria produzir.
pub fn conferir_padrao(carga: &[u8], seq: u32) -> bool {
    let mut esperado = vec![0u8; carga.len()];
    preencher_padrao(&mut esperado, seq);
    esperado == carga
}

/// Monta o eco: mesmo cabeçalho, sem carga.
///
/// Devolver só o cabeçalho é de propósito. O que se quer medir é o tempo de ida e volta do
/// caminho, e mandar a carga de volta mediria também o *uplink* do receptor, que na vida real
/// não carrega vídeo nenhum.
pub fn montar_eco(header: &FrameHeader, destino: &mut [u8]) -> Result<usize> {
    let eco = FrameHeader {
        kind: FrameKind::Echo,
        seq: header.seq,
        sent_at_micros: header.sent_at_micros,
        payload_len: 0,
    };
    eco.write(destino)?;
    Ok(HEADER_LEN)
}

/// Estatística de latência, com memória limitada.
///
/// Guarda no máximo [`LatencyStats::MAX_AMOSTRAS`] amostras (32 KiB). Percentil sobre um
/// reservatório é aproximação, e está dito: o que interessa no M1 é a ordem de grandeza contra
/// a meta de 150 ms e o piso de 5,8 ms de ICMP da bancada, não a terceira casa decimal.
#[derive(Debug, Clone, Default)]
pub struct LatencyStats {
    amostras: Vec<u32>,
    n: u64,
    soma: u64,
    min: u32,
    max: u32,
    descartadas: u64,
}

impl LatencyStats {
    pub const MAX_AMOSTRAS: usize = 8192;

    pub fn new() -> Self {
        LatencyStats {
            amostras: Vec::with_capacity(256),
            n: 0,
            soma: 0,
            min: u32::MAX,
            max: 0,
            descartadas: 0,
        }
    }

    /// Registra uma medida de ida e volta, em microssegundos.
    pub fn record_micros(&mut self, micros: u64) {
        let valor = u32::try_from(micros).unwrap_or(u32::MAX);
        self.n += 1;
        self.soma += u64::from(valor);
        self.min = self.min.min(valor);
        self.max = self.max.max(valor);
        if self.amostras.len() < Self::MAX_AMOSTRAS {
            self.amostras.push(valor);
        } else {
            self.descartadas += 1;
        }
    }

    pub fn count(&self) -> u64 {
        self.n
    }

    /// Amostras que não couberam no reservatório e por isso não entram nos percentis.
    pub fn amostras_descartadas(&self) -> u64 {
        self.descartadas
    }

    pub fn min_ms(&self) -> Option<f64> {
        (self.n > 0).then(|| f64::from(self.min) / 1000.0)
    }

    pub fn max_ms(&self) -> Option<f64> {
        (self.n > 0).then(|| f64::from(self.max) / 1000.0)
    }

    pub fn mean_ms(&self) -> Option<f64> {
        (self.n > 0).then(|| (self.soma as f64 / self.n as f64) / 1000.0)
    }

    /// Percentil sobre as amostras guardadas. `p` entre 0 e 100.
    pub fn percentile_ms(&self, p: f64) -> Option<f64> {
        if self.amostras.is_empty() {
            return None;
        }
        let mut ordenadas = self.amostras.clone();
        ordenadas.sort_unstable();
        let pos = (p / 100.0 * (ordenadas.len() - 1) as f64).round();
        let idx = (pos.max(0.0) as usize).min(ordenadas.len() - 1);
        Some(f64::from(ordenadas[idx]) / 1000.0)
    }

    /// Linha pronta para o relato da bancada.
    pub fn resumo(&self) -> String {
        match (self.min_ms(), self.mean_ms(), self.max_ms()) {
            (Some(min), Some(media), Some(max)) => format!(
                "n={} min={:.2} ms p50={:.2} ms média={:.2} ms p95={:.2} ms max={:.2} ms",
                self.n,
                min,
                self.percentile_ms(50.0).unwrap_or(f64::NAN),
                media,
                self.percentile_ms(95.0).unwrap_or(f64::NAN),
                max
            ),
            _ => "sem amostras".to_string(),
        }
    }
}

/// Preset de encode a usar, dado o tipo de fonte.
///
/// Tela e câmera têm características opostas — conteúdo estático com mudanças bruscas contra
/// ruído com movimento contínuo — e não toleram o mesmo preset. O M1 não codifica nada; a função
/// existe para que a escolha viva no núcleo desde já, e não espalhada por quatro cascas.
pub fn preset_para(fonte_e_tela: bool) -> EncodePreset {
    if fonte_e_tela {
        EncodePreset::Screen
    } else {
        EncodePreset::Camera
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cabecalho_sobrevive_ida_e_volta() {
        let original = FrameHeader {
            kind: FrameKind::Pattern,
            seq: 4_294_967_290,
            sent_at_micros: 1_234_567_890_123,
            payload_len: 65_000,
        };
        let mut buf = [0u8; HEADER_LEN];
        original.write(&mut buf).expect("escreve");
        assert_eq!(FrameHeader::parse(&buf).expect("lê"), original);
    }

    #[test]
    fn cabecalho_tem_o_tamanho_prometido() {
        // Se alguém mexer no layout sem mexer na constante, quebra aqui e não na bancada.
        let h = FrameHeader {
            kind: FrameKind::Echo,
            seq: 1,
            sent_at_micros: 2,
            payload_len: 3,
        };
        let mut buf = [0u8; HEADER_LEN];
        assert!(h.write(&mut buf).is_ok());
        assert!(h.write(&mut buf[..HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn quadro_sem_marca_e_recusado() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = b'X';
        assert!(FrameHeader::parse(&buf).is_err());
    }

    #[test]
    fn quadro_com_tipo_desconhecido_e_recusado() {
        let mut buf = [0u8; HEADER_LEN];
        buf[0..4].copy_from_slice(&FRAME_MAGIC);
        buf[4] = 99;
        assert!(FrameHeader::parse(&buf).is_err());
    }

    #[test]
    fn quadro_curto_demais_e_recusado() {
        assert!(FrameHeader::parse(&[0u8; 4]).is_err());
    }

    #[test]
    fn padrao_e_verificavel_e_muda_com_a_sequencia() {
        let mut a = [0u8; 64];
        let mut b = [0u8; 64];
        preencher_padrao(&mut a, 7);
        preencher_padrao(&mut b, 8);
        assert_ne!(
            a, b,
            "quadros vizinhos com o mesmo padrão não detectam repetição"
        );
        assert!(conferir_padrao(&a, 7));
        assert!(!conferir_padrao(&a, 8));
    }

    #[test]
    fn padrao_com_seq_zero_nao_degenera() {
        let mut a = [0u8; 32];
        preencher_padrao(&mut a, 0);
        assert!(a.iter().any(|&b| b != 0), "padrão de seq=0 saiu todo zero");
    }

    #[test]
    fn gerador_produz_quadro_completo_e_verificavel() {
        let mut gerador = SyntheticPattern::new(1200);
        assert_eq!(gerador.frame_len(), HEADER_LEN + 1200);

        let quadro = gerador.next_frame().expect("quadro").to_vec();
        let h = FrameHeader::parse(&quadro).expect("cabeçalho");
        assert_eq!(h.kind, FrameKind::Pattern);
        assert_eq!(h.seq, 0);
        assert_eq!(h.payload_len, 1200);
        assert!(conferir_padrao(&quadro[HEADER_LEN..], 0));

        let segundo = gerador.next_frame().expect("segundo").to_vec();
        assert_eq!(FrameHeader::parse(&segundo).expect("h2").seq, 1);
    }

    #[test]
    fn gerador_nao_realoca_entre_quadros() {
        let mut gerador = SyntheticPattern::new(4096);
        let cap = gerador.buffer.capacity();
        for _ in 0..1000 {
            gerador.next_frame().expect("quadro");
        }
        assert_eq!(
            gerador.buffer.capacity(),
            cap,
            "o buffer cresceu: haveria alocação por quadro na extension de 50 MB"
        );
    }

    #[test]
    fn eco_preserva_carimbo_e_sequencia_e_zera_a_carga() {
        let original = FrameHeader {
            kind: FrameKind::Pattern,
            seq: 99,
            sent_at_micros: 555_000,
            payload_len: 1200,
        };
        let mut buf = [0u8; HEADER_LEN];
        let n = montar_eco(&original, &mut buf).expect("eco");
        assert_eq!(n, HEADER_LEN);
        let eco = FrameHeader::parse(&buf).expect("lê eco");
        assert_eq!(eco.kind, FrameKind::Echo);
        assert_eq!(eco.seq, 99);
        assert_eq!(eco.sent_at_micros, 555_000);
        assert_eq!(eco.payload_len, 0);
    }

    #[test]
    fn relogio_e_monotonico() {
        let c = Clock::new();
        let a = c.micros();
        let b = c.micros();
        assert!(b >= a);
    }

    #[test]
    fn estatistica_calcula_o_esperado() {
        let mut s = LatencyStats::new();
        for micros in [1000u64, 2000, 3000, 4000, 5000] {
            s.record_micros(micros);
        }
        assert_eq!(s.count(), 5);
        assert_eq!(s.min_ms(), Some(1.0));
        assert_eq!(s.max_ms(), Some(5.0));
        assert_eq!(s.mean_ms(), Some(3.0));
        assert_eq!(s.percentile_ms(50.0), Some(3.0));
        assert_eq!(s.percentile_ms(0.0), Some(1.0));
        assert_eq!(s.percentile_ms(100.0), Some(5.0));
    }

    #[test]
    fn estatistica_vazia_nao_estoura() {
        let s = LatencyStats::new();
        assert_eq!(s.min_ms(), None);
        assert_eq!(s.percentile_ms(95.0), None);
        assert_eq!(s.resumo(), "sem amostras");
    }

    #[test]
    fn estatistica_nao_cresce_sem_limite() {
        let mut s = LatencyStats::new();
        for i in 0..(LatencyStats::MAX_AMOSTRAS as u64 + 500) {
            s.record_micros(i);
        }
        assert_eq!(s.amostras.len(), LatencyStats::MAX_AMOSTRAS);
        assert_eq!(s.amostras_descartadas(), 500);
        // A contagem e a média continuam corretas mesmo com o reservatório cheio.
        assert_eq!(s.count(), LatencyStats::MAX_AMOSTRAS as u64 + 500);
    }

    #[test]
    fn preset_segue_a_fonte() {
        assert_eq!(preset_para(true), EncodePreset::Screen);
        assert_eq!(preset_para(false), EncodePreset::Camera);
    }
}
