//! RTP e o payload H.264 da RFC 6184, do lado de **quem recebe**.
//!
//! # Por que só um lado
//!
//! No emissor, quem empacota é a libdatachannel: `rtcSetH264Packetizer` instala o
//! `H264RtpPacketizer` dela, que faz Single NAL quando o quadro cabe na MTU e FU-A quando não
//! cabe. É código maduro e não há motivo para reescrever.
//!
//! No receptor não existe equivalente. A libdatachannel **tem** um `H264RtpDepacketizer` em
//! C++, mas a API C não o expõe: em `src/capi.cpp` da 0.23.2 há `rtcChainRtcpReceivingSession`,
//! `rtcChainRtcpSrReporter`, `rtcChainRtcpNackResponder`, `rtcChainPliHandler` e
//! `rtcChainRembHandler` — e nenhum `rtcChain…Depacketizer`. A palavra `Depacketizer` não
//! aparece uma única vez no `capi.cpp`. Sem ponte em C, o callback de mensagem da track entrega
//! **pacotes RTP crus**, e remontá-los é nosso.
//!
//! A assimetria é imposta pela biblioteca, não escolhida.
//!
//! # Não há jitter buffer, e isso é o desenho
//!
//! Um depacotizador de uso geral guarda pacotes fora de ordem e espera os atrasados. Aqui não:
//!
//! - o contrato de mídia proíbe fila interna, porque é o que segura os ~50 MB da extension do
//!   iOS;
//! - jitter buffer **é** latência — é literalmente atraso deliberado — e a meta é < 150 ms
//!   perseguindo < 50 ms numa LAN comutada;
//! - existe conserto melhor. Buraco na sequência derruba o quadro em construção, incrementa um
//!   contador, e a casca pede IDR. É exatamente para isso que o contrato exigiu PLI/FIR.
//!
//! Trocar um quadro por um IDR pedido é o negócio certo numa LAN, onde a perda é rara. Numa
//! rede ruim seria caro, e aí o número a olhar é [`Depacotizador::quadros_descartados`].
//!
//! # Memória
//!
//! Um `Vec` de remontagem por track, esvaziado com `clear()` a cada quadro e nunca encolhido.
//! Depois dos primeiros quadros não há mais alocação — o mesmo cuidado que o
//! [`crate::media::SyntheticPattern`] tem no emissor.

use crate::error::{Error, Result};

/// Cabeçalho RTP fixo, em bytes (RFC 3550 §5.1). CSRC e extensão vêm depois.
pub const RTP_HEADER_LEN: usize = 12;

/// Escala do carimbo RTP para vídeo, em Hz (RFC 3551).
pub const RELOGIO_VIDEO_HZ: u32 = 90_000;

/// Escala do carimbo RTP para Opus, em Hz.
///
/// **Sempre 48000, e não é escolha nossa.** A RFC 7587 §4.1 fixa o relógio de RTP do Opus em
/// 48 kHz *independentemente* da taxa interna com que o encoder resolveu trabalhar — um Opus
/// codificando a 16 kHz por dentro continua carimbando a 48 kHz no fio. Usar a taxa interna aqui
/// faria o tempo do receptor andar a um terço da velocidade certa, e o sintoma seria áudio
/// "acelerando" ou "arrastando" sem nenhum contador acusando.
pub const RELOGIO_OPUS_HZ: u32 = 48_000;

/// Escala do carimbo RTP para G.711 µ-law (RFC 3551, tabela 4): 8 kHz.
pub const RELOGIO_PCMU_HZ: u32 = 8_000;

/// Prefixo Annex-B de 4 bytes. É o que sai deste módulo, sempre — mesmo quando o emissor usou o
/// prefixo curto de 3 bytes, que o pacotizador descarta ao fatiar em NAL units.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Tipos de NAL unit da RFC 6184 que aparecem no cabeçalho de payload.
pub mod nal {
    /// Fatia de um quadro IDR (RFC 6184 §5.2, tipo 5).
    pub const IDR: u8 = 5;
    /// Sequence Parameter Set.
    pub const SPS: u8 = 7;
    /// Picture Parameter Set.
    pub const PPS: u8 = 8;
    /// Agregação de NAL units num pacote só.
    pub const STAP_A: u8 = 24;
    /// Fragmentação de uma NAL unit em vários pacotes.
    pub const FU_A: u8 = 28;
}

/// Um pacote RTP já separado em cabeçalho e payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacoteRtp<'a> {
    pub marca: bool,
    pub sequencia: u16,
    /// Carimbo na escala do relógio da mídia (90 kHz para vídeo). Dá a volta em ~13 horas.
    pub carimbo: u32,
    pub ssrc: u32,
    pub payload_type: u8,
    pub payload: &'a [u8],
}

impl<'a> PacoteRtp<'a> {
    /// Separa um pacote RTP. Não copia nada: o payload é uma vista de `bytes`.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() < RTP_HEADER_LEN {
            return Err(Error::Protocol(format!(
                "pacote RTP de {} bytes, menor que o cabeçalho de {RTP_HEADER_LEN}",
                bytes.len()
            )));
        }
        let versao = bytes[0] >> 6;
        if versao != 2 {
            return Err(Error::Protocol(format!("RTP versão {versao}, esperada 2")));
        }
        let tem_padding = bytes[0] & 0b0010_0000 != 0;
        let tem_extensao = bytes[0] & 0b0001_0000 != 0;
        let csrc = usize::from(bytes[0] & 0b0000_1111);

        let marca = bytes[1] & 0b1000_0000 != 0;
        let payload_type = bytes[1] & 0b0111_1111;
        let sequencia = u16::from_be_bytes([bytes[2], bytes[3]]);
        let carimbo = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let ssrc = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);

        let mut inicio = RTP_HEADER_LEN + csrc * 4;
        if bytes.len() < inicio {
            return Err(Error::Protocol("RTP truncado na lista de CSRC".into()));
        }

        if tem_extensao {
            // Extensão: 2 bytes de perfil + 2 bytes de comprimento, em palavras de 32 bits.
            if bytes.len() < inicio + 4 {
                return Err(Error::Protocol(
                    "RTP truncado no cabeçalho de extensão".into(),
                ));
            }
            let palavras = usize::from(u16::from_be_bytes([bytes[inicio + 2], bytes[inicio + 3]]));
            inicio += 4 + palavras * 4;
            if bytes.len() < inicio {
                return Err(Error::Protocol("RTP truncado na extensão".into()));
            }
        }

        let mut fim = bytes.len();
        if tem_padding {
            // O último byte diz quantos bytes de enchimento há, incluindo ele próprio.
            let enchimento = usize::from(bytes[fim - 1]);
            if enchimento == 0 || enchimento > fim - inicio {
                return Err(Error::Protocol(format!(
                    "RTP com enchimento inválido: {enchimento} bytes"
                )));
            }
            fim -= enchimento;
        }

        Ok(PacoteRtp {
            marca,
            sequencia,
            carimbo,
            ssrc,
            payload_type,
            payload: &bytes[inicio..fim],
        })
    }

    /// O pacote é RTCP disfarçado de RTP?
    ///
    /// Com RTP e RTCP multiplexados na mesma porta — que é o que o WebRTC faz —, a RFC 5761 §4
    /// reserva os tipos de payload **64 a 95** para RTCP. Um RTCP Sender Report (PT 200) tem o
    /// segundo byte `0xC8`, que lido como RTP vira "marca ligada, payload type 72": passa no
    /// teste de versão, tem sequência e carimbo em posições que existem, e nada avisa.
    ///
    /// Hoje a `RtcpReceivingSession` da libdatachannel filtra RTCP antes de chegar aqui, mas não
    /// pelo teste que parece: o `payloadType() == 200 || == 201` do ramo `Binary` de
    /// `src/rtcpreceivingsession.cpp` (0.23.2) compara um valor mascarado em 7 bits e **nunca
    /// dispara**. O que filtra é a demultiplexação do transporte, que entrega RTCP como
    /// `Message::Control`, e o ramo `Control` da sessão, que lê o SR e **não repassa** a mensagem
    /// — medido em 18/09/2026: 44 SR na biblioteca, 0 RTCP aqui (`docs/som-no-receptor.md` §2).
    /// Este teste é a segunda tranca: se aquele filtro mudar,
    /// ou se um dia a track for montada sem a sessão de RTCP encadeada, o estrago seria bytes de
    /// RTCP costurados dentro de um quadro H.264 — imagem quebrada sem contador nenhum
    /// acusando, que é a pior classe de defeito que existe.
    pub fn e_rtcp_multiplexado(&self) -> bool {
        (64..=95).contains(&self.payload_type)
    }
}

/// A conta de sequência RTP, **uma só para vídeo e para áudio**.
///
/// Estava escrita à mão dentro do [`Depacotizador`] de vídeo. Saiu para cá quando o áudio
/// chegou, e não por gosto de fatorar: os três números que ela produz —
/// [`Contadores::pacotes_faltando`], [`Contadores::eventos_fora_de_ordem`] e
/// [`Contadores::pacotes_vistos`] — já estão publicados, lidos por quatro cascas, e cada um tem
/// uma definição que custou uma dívida para acertar (itens 25 e 26 de `divida-do-nucleo.md`).
/// Duas implementações da mesma conta seriam duas chances de elas divergirem em silêncio, e o
/// leitor não teria como saber qual das duas está olhando.
#[derive(Debug, Default)]
struct ContaDeSequencia {
    /// Sequência do último pacote aceito. `None` até o primeiro — ver
    /// [`Contadores::pacotes_vistos`].
    ultima: Option<u16>,
    faltando: u64,
    fora_de_ordem: u64,
    vistos: u64,

    // --- a janela que separa perda de reordenação, e por que ela precisou existir --------------
    //
    // `faltando` compara **só com o pacote anterior**. Uma reordenação de distância `d` — os
    // pacotes chegam 100, 120, 101…119, 121 — soma `19` no salto para a frente e mais `1` no
    // reencontro, e nada disso é descontado quando as posições depois chegam. `faltando` fica em
    // 20 com **zero** pacote perdido, e `fora_de_ordem` marca **um** evento.
    //
    // Isso está documentado desde a dívida 26 (`faltando` é teto, não perda) e mesmo assim foi
    // lido como perda em todas as medições desta bancada, porque não havia um número melhor ao
    // lado. Medido em 29/08 numa corrida MacBook → A10s com origem sintética: `faltando` = 486
    // com 70 eventos fora de ordem, enquanto o emissor tinha entregado 27.779 pacotes e o
    // receptor viu 27.729 — perda real de no **máximo 50**, dez vezes menor que o teto.
    //
    // A janela abaixo resolve pelo mesmo mecanismo de qualquer receptor RTP sério: um mapa de bits
    // das últimas `LARGURA_DA_JANELA` posições. Uma posição só é dada por perdida quando sai da
    // janela sem nunca ter sido marcada. A 230 pacotes/s (a forma de tráfego desta bancada),
    // 128 posições são ~550 ms de tolerância a reordenação — muito acima de qualquer coisa que
    // uma LAN produza.
    /// Maior sequência já vista, **desenrolada** (sem a volta de 16 bits).
    topo: Option<u64>,
    /// Menor posição ainda contável: `max(primeira vista, topo - LARGURA_DA_JANELA + 1)`.
    piso: u64,
    /// Mapa de bits das últimas [`LARGURA_DA_JANELA`] posições até `topo`. O bit 0 é `topo`.
    janela: u128,
    /// Posições que saíram da janela sem nunca terem sido vistas. **É a perda exata.**
    nunca_chegaram: u64,
    /// Pacotes que chegaram tão atrasados que a posição deles já tinha saído da janela. Já foram
    /// contados em [`Self::nunca_chegaram`] e não dá para desfazer — se este número não for zero,
    /// a perda exata está superestimada nele.
    tarde_demais: u64,
}

/// Quantas posições de sequência a janela de [`ContaDeSequencia`] guarda.
///
/// 128 é ~550 ms na forma de tráfego desta bancada (230 pacotes/s). Não é ajustável de fora de
/// propósito: um número que se possa mexer é um número que alguém mexe para o resultado sair
/// bonito.
const LARGURA_DA_JANELA: u32 = 128;

/// O que um pacote fez com a sequência. Ver [`ContaDeSequencia::conferir`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Salto {
    /// O seguinte ao anterior — ou o primeiro de todos, que não tem com o que ser comparado.
    Seguido,
    /// Pulou posições para a frente: elas não chegaram, ou ainda não.
    Adiante,
    /// Repetido, ou mais velho que o anterior.
    ParaTras,
}

impl ContaDeSequencia {
    /// Conta o que faltou e devolve **que salto houve**. Quem decide o que fazer com o buraco é
    /// quem chama: o vídeo condena o quadro em construção, o áudio não tem o que condenar.
    fn conferir(&mut self, sequencia: u16) -> Salto {
        // Antes de qualquer teste: este pacote foi visto. Inclusive o primeiro, que não pode ser
        // comparado com nada e que **é** a linha de base.
        self.vistos = self.vistos.saturating_add(1);

        self.marcar_na_janela(sequencia);

        let mut resultado = Salto::Seguido;
        if let Some(anterior) = self.ultima {
            // `wrapping_sub` em u16 dá a distância certa mesmo na volta de 65535 para 0.
            let salto = sequencia.wrapping_sub(anterior);
            if salto != 1 {
                if salto == 0 || salto > u16::MAX / 2 {
                    // Repetido ou fora de ordem (salto "negativo"). Conta **um evento**, e não
                    // `n` pacotes: não há `n` aqui.
                    self.fora_de_ordem = self.fora_de_ordem.saturating_add(1);
                    resultado = Salto::ParaTras;
                } else {
                    // Salto para a frente: `salto − 1` posições de sequência não chegaram.
                    self.faltando = self.faltando.saturating_add(u64::from(salto - 1));
                    resultado = Salto::Adiante;
                }
            }
        }
        self.ultima = Some(sequencia);
        resultado
    }

    /// Marca `sequencia` na janela deslizante e cobra o que saiu dela sem nunca ter chegado.
    ///
    /// A janela é `[piso, topo]`, com `topo` = maior sequência já vista (desenrolada) e
    /// `piso` = `max(primeira vista, topo - LARGURA + 1)`. Uma posição só vira perda quando o
    /// piso passa por cima dela sem que o bit dela tenha sido marcado.
    fn marcar_na_janela(&mut self, sequencia: u16) {
        let Some(topo) = self.topo else {
            // O primeiro pacote **é** a linha de base: nada antes dele é contável, exatamente
            // como em `pacotes_vistos`.
            self.topo = Some(u64::from(sequencia));
            self.piso = u64::from(sequencia);
            self.janela = 1;
            return;
        };
        let avanco = u32::from(sequencia.wrapping_sub(topo as u16));
        if avanco == 0 {
            return; // repetição do topo; já está marcado
        }
        let largura = u64::from(LARGURA_DA_JANELA);

        if avanco <= u32::from(u16::MAX) / 2 {
            // Para a frente. O piso sobe, e o que ele deixa para trás é cobrado.
            let novo_topo = topo + u64::from(avanco);
            let novo_piso = self.piso.max(novo_topo.saturating_sub(largura - 1));
            let mut idx = self.piso;
            while idx < novo_piso {
                // Posição acima do topo antigo nunca pôde ter sido marcada; abaixo dele, o bit
                // responde. Um salto enorme cai no ramo de baixo e é cobrado de uma vez, sem
                // laço de dezenas de milhares de voltas.
                if idx > topo {
                    self.nunca_chegaram = self.nunca_chegaram.saturating_add(novo_piso - idx);
                    break;
                }
                let deslocamento = topo - idx;
                let visto = deslocamento < largura && (self.janela >> deslocamento) & 1 == 1;
                if !visto {
                    self.nunca_chegaram = self.nunca_chegaram.saturating_add(1);
                }
                idx += 1;
            }
            self.janela = if u64::from(avanco) >= largura {
                0
            } else {
                self.janela << avanco
            };
            self.janela |= 1;
            self.topo = Some(novo_topo);
            self.piso = novo_piso;
        } else {
            // Para trás: chegou atrasado. `atraso` é a distância até o topo.
            let atraso = u64::from(u32::from(u16::MAX) + 1 - avanco);
            match topo.checked_sub(atraso) {
                Some(idx) if idx >= self.piso && atraso < largura => {
                    self.janela |= 1u128 << atraso;
                }
                _ => self.tarde_demais = self.tarde_demais.saturating_add(1),
            }
        }
    }

    /// A perda exata **até aqui**, sem contar as posições que ainda estão dentro da janela.
    ///
    /// As posições da janela ficam de fora de propósito: no fim de uma sessão elas são
    /// ambíguas — ninguém sabe se o pacote se perdeu ou se a sessão acabou antes de ele chegar —
    /// e são no máximo [`LARGURA_DA_JANELA`], o que é ruído contra dezenas de milhares.
    fn perda_exata(&self) -> u64 {
        self.nunca_chegaram
    }
}

/// Desenrola o carimbo RTP de 32 bits **pela diferença com sinal em relação ao maior já visto**
/// — o apêndice A.1 da RFC 3550 aplicado ao carimbo.
///
/// # Por que não "contar uma volta quando o carimbo cai"
///
/// Era o que o [`RelogioRtp`] fazia até 18/09/2026, comparando cada carimbo com o **anterior na
/// ordem de chegada**. Um pacote de antes da volta que chega depois de um de depois da volta
/// fazia a conta duas vezes: chegadas 10, 12 (já depois da volta), 11, 13 contavam uma volta em
/// 12 e **outra** em 13, porque 13 é menor que o 11 que acabara de chegar. A linha do tempo
/// pulava 2³² tiques para a frente — 24 h 51 min a 48 kHz — e não voltava. Medido pelas duas
/// críticas da revisão de `docs/som-no-receptor.md` com o código real.
///
/// Aqui só o **maior** carimbo já visto avança a referência. Um atrasado de antes da volta tem
/// diferença negativa e cai um pouco antes, não uma volta à frente.
///
/// O desenrolado começa no **valor cru do primeiro carimbo**, sem volta nenhuma: é o referencial
/// que o relógio da sessão (`crate::relogio`) usa para o reticulado das voltas.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Desenrolador {
    /// `(carimbo cru, valor desenrolado)` do maior carimbo já visto.
    maior: Option<(u32, i64)>,
}

impl Desenrolador {
    /// O carimbo desenrolado, em tiques, no referencial do primeiro carimbo visto.
    pub(crate) fn desenrolar(&mut self, carimbo: u32) -> i64 {
        match self.maior {
            None => {
                let valor = i64::from(carimbo);
                self.maior = Some((carimbo, valor));
                valor
            }
            Some((cru, valor)) => {
                // Diferença de 32 bits com sinal: um salto de mais de meia volta é lido como para
                // trás. A 90 kHz meia volta são 6 h 37 min — nenhum pacote se atrasa tanto.
                let d = i64::from(carimbo.wrapping_sub(cru) as i32);
                let desenrolado = valor + d;
                if d > 0 {
                    self.maior = Some((carimbo, desenrolado));
                }
                desenrolado
            }
        }
    }

    /// Desenrola **sem** mexer na referência: para saber onde um carimbo cai sem que ele passe a
    /// ser o maior. `None` antes do primeiro carimbo.
    pub(crate) fn onde_cai(&self, carimbo: u32) -> Option<i64> {
        self.maior
            .map(|(cru, valor)| valor + i64::from(carimbo.wrapping_sub(cru) as i32))
    }
}

/// O relógio RTP de uma track: desenrola as voltas do carimbo de 32 bits e converte para
/// microssegundos desde a **base** — o primeiro carimbo que passou por aqui.
///
/// Também estava dentro do [`Depacotizador`], fixo em 90 kHz. O áudio obrigou a parametrizar a
/// taxa — Opus carimba a 48 kHz, G.711 a 8 kHz — e o resto da lógica é idêntica.
///
/// **A base do vídeo é o primeiro quadro entregue, não o primeiro pacote**: o depacotizador de
/// vídeo só lê o relógio ao fechar um quadro. É por isso que o relógio da sessão pergunta a base
/// a cada track ([`RelogioRtp::base`]) em vez de supor.
#[derive(Debug)]
struct RelogioRtp {
    hz: u32,
    /// Base do carimbo, fixada no primeiro carimbo lido. Ela é a origem do tempo desta track.
    base: Option<u32>,
    /// A mesma base, publicada para quem **não pode** pedir o cadeado do depacotizador. Ver
    /// [`BasePublicada`].
    publicada: BasePublicada,
    desenrolador: Desenrolador,
}

/// A base do `timestamp_us` de uma track, publicada num atômico **no instante em que é fixada**
/// — antes de o quadro que a fixou chegar ao tratador da casca.
///
/// # Por que ela existe (revisão do código da S1, achado B1)
///
/// A bomba do `TrackReceptor` segura o cadeado do depacotizador enquanto chama o tratador da
/// casca. Perguntar o deslocamento de captura de dentro do tratador — que é o uso que o contrato
/// convida — pedia o mesmo cadeado para ler a base, e a thread da libdatachannel travava para
/// sempre no primeiro quadro. Com a base num atômico, ninguém precisa do cadeado para lê-la.
#[derive(Debug, Clone, Default)]
pub struct BasePublicada(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl BasePublicada {
    /// Marca de "há base" no bit 32: o carimbo cru cabe nos 32 de baixo.
    const PRESENTE: u64 = 1 << 32;

    fn publicar(&self, base: u32) {
        self.0.store(
            Self::PRESENTE | u64::from(base),
            std::sync::atomic::Ordering::Release,
        );
    }

    /// A base, ou `None` antes de ela ser fixada. Sem cadeado.
    pub fn ler(&self) -> Option<u32> {
        let v = self.0.load(std::sync::atomic::Ordering::Acquire);
        (v & Self::PRESENTE != 0).then_some(v as u32)
    }
}

impl RelogioRtp {
    fn novo(hz: u32) -> Self {
        RelogioRtp {
            hz,
            base: None,
            publicada: BasePublicada::default(),
            desenrolador: Desenrolador::default(),
        }
    }

    /// O carimbo cru que virou a origem do tempo desta track, ou `None` se nenhum passou ainda.
    fn base(&self) -> Option<u32> {
        self.base
    }

    /// O carimbo desenrolado, em ticks desde a base desta track.
    ///
    /// A 90 kHz o contador de 32 bits dá a volta a cada ~13 h 15 min; a 48 kHz, a cada ~24 h 51
    /// min. Uma sessão de espelhamento raramente chega lá, mas "raramente" não é "nunca", e uma
    /// volta não tratada faria o tempo andar para trás no meio de uma medição.
    ///
    /// Um carimbo **anterior à base** (um pacote reordenado logo no começo) dá 0, como antes.
    fn ticks(&mut self, carimbo: u32) -> u64 {
        let desenrolado = self.desenrolador.desenrolar(carimbo);
        let base = match self.base {
            Some(b) => b,
            None => {
                self.base = Some(carimbo);
                self.publicada.publicar(carimbo);
                carimbo
            }
        };
        // A base é o primeiro carimbo desenrolado, e o desenrolador começa no valor cru dele.
        (desenrolado - i64::from(base)).max(0) as u64
    }

    fn micros(&mut self, carimbo: u32) -> u64 {
        // Multiplica antes de dividir para não perder resolução. `u64` aguenta milhões de anos.
        ticks_para_micros(self.ticks(carimbo), self.hz)
    }
}

fn ticks_para_micros(ticks: u64, hz: u32) -> u64 {
    ticks.saturating_mul(1_000_000) / u64::from(hz)
}

/// O estimador de jitter de chegada da RFC 3550 §6.4.1.
///
/// Ordena pacotes antes de o depacotizador vê-los.
///
/// # Por que ela existe, e o número que a criou
///
/// **01/09/2026.** Uma sessão pelo cabo, A10s recebendo 1080p do Dell, mediu `perda exata 0
/// (0,000 %)` — nenhum pacote perdido, `tarde demais 0` — e ainda assim **158 quadros exibidos
/// com a referência quebrada**, em rajadas de até 23 seguidos, com 81 pedidos de IDR em dois
/// minutos. A causa é [`ContaDeSequencia::conferir`] marcar buraco tanto no salto para a frente
/// quanto na volta do pacote atrasado, e `aceitar` condenar o quadro no ato: **cada reordenação
/// condenava até dois quadros**, sem que nada tivesse se perdido.
///
/// A decisão registrada de "vídeo não tem jitter buffer" continua **certa para perda**: dado
/// destruído não volta e esperar não adianta. Ela estava sendo aplicada a **reordenação**, que é
/// o caso oposto — nada foi destruído, e esperar resolve inteiro.
///
/// # A propriedade que torna isto barato
///
/// **Pacote que chega na ordem esperada é processado na hora**, sem passar pela fila. A latência
/// só existe quando há buraco: não é atraso permanente, é o preço de um dano que antes se pagava
/// em imagem quebrada.
///
/// # Por que o limite é em pacotes e não em milissegundos
///
/// Um limite em tempo pediria relógio de parede aqui dentro e **não se ajusta ao bitrate**. O
/// anel de N posições dá o mesmo efeito e se escala sozinho: a 340 pacotes/s — a taxa medida na
/// sessão que originou isto — 16 posições são ~47 ms; a 1000 pacotes/s são 16 ms. A espera
/// encolhe justamente quando o fluxo é mais denso, que é quando ela incomodaria.
///
/// [`PROFUNDIDADE_PADRAO`] é 16 porque o salto médio medido foi de **~10,7 posições**, e é onde o
/// anel **começa**. Profundidade **0 desliga a fila** e restaura exatamente o comportamento
/// anterior — é o que permite medir o antes e o depois no mesmo binário.
///
/// # O anel se ajusta ao regime, e o sinal sai de dentro dele (02/09/2026)
///
/// A corrida de 01/09 mediu duas doenças **opostas** no mesmo produto:
///
/// | | cabo | Wi-Fi |
/// |---|---|---|
/// | perda exata | 0,000 % | 3,858 % |
/// | `reorder_events` | **387** | **15** |
/// | `reorderings_absorbed` | 1293 | 72 (0,18 % dos pacotes) |
///
/// **O cabo reordena e não perde; o Wi-Fi perde e não reordena.** Um anel fixo não pode estar
/// certo nos dois: no cabo 16 posições deixaram 387 desistências na mesa, e no Wi-Fi **cada
/// pacote perdido custa o anel inteiro de espera** antes de o quadro ser condenado e o IDR
/// pedido — a 340 pacotes/s, ~47 ms por perda, num regime em que a espera não resolve nada
/// porque o pacote não vem mesmo.
///
/// O sinal que separa os dois regimes não precisa de relógio, de rótulo de interface nem de
/// palpite sobre a rede: **depois de desistir de um buraco, o pacote esperado chega ou não
/// chega.**
///
/// - **Chega atrasado** → não era perda, era reordenação além do anel. O anel era curto. Cresce.
/// - **Nunca chega** → era perda. O anel só está cobrando latência. Encolhe.
///
/// A razão entre as duas coisas, medida sobre [`DESISTENCIAS_POR_DECISAO`] desistências, é a
/// decisão. Ela é deliberadamente **assimétrica e com banda morta** — a mesma disciplina do
/// `taxa.rs`, e pelo mesmo motivo: um controlador de um limiar só bate entre dois valores para
/// sempre.
///
/// # Por que a decisão mora dentro de [`Depacotizador::desistir`]
///
/// Porque **é o único instante em que o anel está comprovadamente vazio**. As posições são
/// indexadas por `sequencia % profundidade`; mudar a profundidade com pacote preso trocaria todos
/// os índices e entregaria pacote na ordem errada — o defeito que esta fila existe para apagar.
/// `desistir` drena tudo e zera a linha de base logo antes, então redimensionar ali é a única
/// versão barata que também é correta.
///
/// # O viés, e ele é de propósito
///
/// Só a **primeira** posição de cada buraco (`base`) fica sob julgamento, e uma desistência nova
/// substitui o julgamento pendente da anterior. Isso subestima a reordenação, nunca a
/// superestima: o anel cresce com prova e encolhe na dúvida. Num regime de perda — onde crescer é
/// o erro caro — essa é a direção certa do engano.
///
/// # ATENÇÃO: o ajuste automático é **mantido e NÃO PROVADO** (02/09/2026)
///
/// Ele foi medido nos dois regimes, com braço de controle no **mesmo enlace** e na mesma noite —
/// o anel cravado em 16 contra o anel solto —, e em nenhum dos dois o proveito apareceu na
/// imagem:
///
/// | | cabo: 16 → 32 | Wi-Fi: 16 (o piso) |
/// |---|---|---|
/// | `reorder_events` | 219 → **180** | — |
/// | `reorderings_absorbed` | 744 → **810** | — |
/// | rupturas / mil quadros | 3,94 → 3,51 | — |
/// | suspeitos / mil quadros | 6,03 → **6,09** | — |
/// | `sem_referencia` máx | 112,6 → **127,7 ms** | — |
///
/// **O mecanismo trabalha e a imagem não se move.** No cabo, dois `rupturas` de diferença em
/// ~4300 quadros e `suspeitos` idêntico, com os indicadores de cauda ligeiramente piores no braço
/// adaptativo — ruído nas duas direções. No Wi-Fi ele não tem para onde ir, porque o piso é o
/// próprio padrão desde que descer foi medido e reprovado (ver [`PROFUNDIDADE_MINIMA`]).
///
/// A frente que pediu isto ("subir para 32 no cabo, contra as 387 desistências que sobraram")
/// estava dimensionada num contador mal lido: `reorder_events` **não é** desistência — é pacote
/// que chega para trás depois de uma, e uma desistência solta uma rajada inteira deles. Com o
/// contador que faltava, 19 desistências produziram 219 eventos; as "387" eram cerca de 34.
///
/// **Fica porque não faz mal** — não oscila (`reorder_adjusts = 1` no cabo, 2 no rádio, sempre
/// monótono), não inventa perda exata, e o piso e o teto são medidos. **Não fica porque foi
/// provado.** Quem for mexer aqui não precisa defender o que ele ganha: ele não ganhou nada
/// ainda. Um enlace que estoure o anel de 16 de verdade — cabo longo, bitrate maior, quadro
/// maior — é o que faltaria para decidir, e esta bancada não achou um.
#[derive(Debug)]
struct FilaDeReordenacao {
    /// Quantas posições o anel comporta. `0` desliga a fila.
    profundidade: usize,
    /// A sequência que a máquina de estados espera receber a seguir. `None` até o primeiro
    /// pacote, que **fixa a linha de base** — a mesma regra de `ContaDeSequencia`.
    proximo: Option<u16>,
    /// O anel, indexado por `sequencia % profundidade`. Guarda o pacote inteiro porque o
    /// depacotizador reanalisa o cabeçalho: a fila não interpreta payload, só ordena.
    presos: Vec<Option<Vec<u8>>>,
    /// Quantas posições do anel estão ocupadas agora.
    guardados: usize,
    /// **Quantas reordenações a fila absorveu.** Sem este número o conserto arrumaria a imagem e
    /// sumiria da medida, que é o oposto da regra desta casa: o que não é contado não existe.
    absorvidos: u64,
    /// A posição de que se desistiu por último, ainda **sob julgamento**: se ela chegar atrasada,
    /// o anel era curto; se nunca chegar, era perda.
    sob_julgamento: Option<u16>,
    /// Desistências desde a última decisão.
    desistencias: u32,
    /// Quantas delas o pacote esperado **chegou atrasado** — ou seja, eram reordenação, não perda.
    absolvidas: u32,
    /// O ajuste automático está ligado? [`Depacotizador::definir_profundidade_de_reordenacao`] o
    /// desliga, porque quem crava um número na bancada quer medir aquele número.
    ajusta_sozinha: bool,
    /// Quantas vezes a profundidade mudou. Sai no relato; é como se vê o regime pelo contador.
    ajustes: u64,
    /// Total de desistências da sessão, sem zerar por janela. Denominador honesto do de cima.
    desistencias_totais: u64,
}

impl FilaDeReordenacao {
    fn nova(profundidade: usize) -> Self {
        FilaDeReordenacao::com_ajuste(profundidade, true)
    }

    fn com_ajuste(profundidade: usize, ajusta_sozinha: bool) -> Self {
        FilaDeReordenacao {
            profundidade,
            proximo: None,
            presos: (0..profundidade).map(|_| None).collect(),
            guardados: 0,
            absorvidos: 0,
            sob_julgamento: None,
            desistencias: 0,
            absolvidas: 0,
            ajusta_sozinha,
            ajustes: 0,
            desistencias_totais: 0,
        }
    }

    /// Anota uma desistência e, quando a janela fecha, decide o tamanho do anel.
    ///
    /// Chamada de [`Depacotizador::desistir`] **depois** do dreno, que é quando o anel está vazio
    /// e a linha de base já foi zerada — ver a nota em [`FilaDeReordenacao`] para por que este é o
    /// único instante em que redimensionar é correto.
    fn registrar_desistencia(&mut self, base: u16) {
        self.sob_julgamento = Some(base);
        self.desistencias = self.desistencias.saturating_add(1);
        self.desistencias_totais = self.desistencias_totais.saturating_add(1);
        if !self.ajusta_sozinha || self.desistencias < DESISTENCIAS_POR_DECISAO {
            return;
        }
        let nova = self.profundidade_decidida();
        self.desistencias = 0;
        self.absolvidas = 0;
        if nova != self.profundidade {
            self.redimensionar(nova);
        }
    }

    /// A regra, e os dois limiares com banda morta entre eles.
    ///
    /// - **Metade ou mais das desistências absolvidas** → o anel está sendo estourado por
    ///   reordenação de verdade. Dobra, até [`PROFUNDIDADE_MAXIMA`].
    /// - **Um oitavo ou menos** → o que ele encontra é perda, e o anel que tinha crescido não
    ///   precisa mais do tamanho. Reduz à metade, até [`PROFUNDIDADE_MINIMA`] — que é o padrão, e
    ///   não menos: descer abaixo dele foi medido e **custa imagem**.
    /// - **Entre os dois** → não mexe. A banda morta é o que impede o ciclo-limite que o
    ///   `taxa.rs` já pagou uma corrida para aprender.
    ///
    /// Os números medidos caem longe das bordas, que é o que faz a regra ser uma decisão e não um
    /// sorteio — e isto **foi conferido em aparelho** em 02/09/2026: no cabo o anel subiu para 32
    /// num passo só e ficou (`reorder_adjusts = 1`); no rádio desceu em dois passos monótonos e
    /// ficou (`reorder_adjusts = 2`, antes de o piso subir para o padrão). Nenhum dos dois
    /// oscilou, que era o modo de falha que a banda morta existe para impedir.
    ///
    /// O que **não** foi provado é o proveito de qualquer um dos dois movimentos. Ver a nota em
    /// [`FilaDeReordenacao`].
    fn profundidade_decidida(&self) -> usize {
        if u64::from(self.absolvidas) * 2 >= u64::from(self.desistencias) {
            return (self.profundidade * 2).min(PROFUNDIDADE_MAXIMA);
        }
        if u64::from(self.absolvidas) * 8 <= u64::from(self.desistencias) {
            return (self.profundidade / 2).max(PROFUNDIDADE_MINIMA);
        }
        self.profundidade
    }

    /// Troca o tamanho do anel. **Só é chamada com o anel vazio** — ver [`Self::registrar_desistencia`].
    fn redimensionar(&mut self, nova: usize) {
        debug_assert_eq!(
            self.guardados, 0,
            "redimensionar com pacote preso troca os índices"
        );
        self.profundidade = nova;
        self.presos = (0..nova).map(|_| None).collect();
        self.guardados = 0;
        self.proximo = None;
        self.ajustes = self.ajustes.saturating_add(1);
    }

    /// O pacote que chegou atrasado era o que estávamos esperando? Então a desistência foi
    /// prematura, e isso é a prova de que o anel é curto para esta rede.
    fn julgar_atrasado(&mut self, sequencia: u16) {
        if self.sob_julgamento == Some(sequencia) {
            self.sob_julgamento = None;
            self.absolvidas = self.absolvidas.saturating_add(1);
        }
    }

    /// Guarda o pacote na posição dele. Devolve `false` se a posição já estava ocupada — pacote
    /// repetido, que se descarta sem drama.
    fn guardar(&mut self, sequencia: u16, bytes: &[u8]) -> bool {
        let i = usize::from(sequencia) % self.profundidade;
        if self.presos[i].is_some() {
            return false;
        }
        self.presos[i] = Some(bytes.to_vec());
        self.guardados += 1;
        true
    }

    /// Tira da posição, se houver.
    fn tirar(&mut self, sequencia: u16) -> Option<Vec<u8>> {
        let i = usize::from(sequencia) % self.profundidade;
        let saiu = self.presos[i].take();
        if saiu.is_some() {
            self.guardados -= 1;
        }
        saiu
    }

    /// Esvazia o anel, devolvendo os pacotes **em ordem de sequência** a partir de `proximo`.
    /// Usado quando se desiste do buraco: o que estava preso não se joga fora, entrega-se.
    fn drenar_tudo(&mut self, a_partir_de: u16) -> Vec<Vec<u8>> {
        let mut fora = Vec::new();
        for k in 0..self.profundidade {
            let s = a_partir_de.wrapping_add(k as u16);
            if let Some(b) = self.tirar(s) {
                fora.push(b);
            }
        }
        fora
    }
}

/// Profundidade **inicial** do anel de reordenação, em pacotes. Ver [`FilaDeReordenacao`].
///
/// 16 porque o salto médio medido em 01/09/2026 foi de ~10,7 posições. Continua sendo o ponto de
/// partida depois do ajuste automático de 02/09 **de propósito**: é o valor neutro, o único que
/// foi medido nos dois regimes, e começar dele faz o anel provar o regime antes de mudar de
/// tamanho em vez de apostar num.
pub const PROFUNDIDADE_PADRAO: usize = 16;

/// Teto do anel, em pacotes.
///
/// 64 posições são ~188 ms a 340 pacotes/s (a taxa medida na sessão que originou a fila) e ~64 ms
/// a 1000/s. O teto existe porque o anel só é barato enquanto a espera dele é menor que o
/// prejuízo que evita: passar disto seria construir o jitter buffer que a decisão registrada
/// recusa — e recusa **com razão para perda**, que é o outro regime.
pub const PROFUNDIDADE_MAXIMA: usize = 64;

/// Piso do anel, em pacotes. **É o próprio [`PROFUNDIDADE_PADRAO`], e isso foi medido.**
///
/// # A versão errada, e o A/B que a derrubou
///
/// Este piso nasceu 4 em 02/09/2026, com o argumento de que num enlace que só perde cada posição
/// do anel é atraso puro antes do pedido de IDR. **O argumento não sobreviveu ao aparelho.**
///
/// A/B no mesmo enlace, na mesma tarde, Dell → A10s em Wi-Fi, com perda praticamente igual nos
/// dois braços (3,764 % contra 3,733 %):
///
/// | | anel cravado em 16 | anel adaptativo (desceu a 4) |
/// |---|---|---|
/// | `reorderings_absorbed` | **102** | 31 |
/// | `reorder_events` | **3** | 39 |
/// | suspeitos por mil quadros | **131,2** | 147,7 |
/// | rupturas por mil quadros | 33,2 | 33,2 |
/// | `sem_referencia` p95 | **713 ms** | 902 ms |
///
/// A causa fecha a conta sozinha: **mesmo num enlace que perde 3,7 % existe reordenação de
/// distância 4 a 15**, que um anel de 16 absorve inteira (3 eventos escaparam) e um de 4 condena
/// (39 escaparam). Cada evento desses condena até dois quadros, e 39 eventos explicam
/// praticamente os 67 suspeitos a mais que o braço adaptativo mediu.
///
/// E o benefício que justificava encolher **não apareceu**: o tempo sem referência piorou em p95,
/// não melhorou. Encolher não comprou nada e custou imagem.
///
/// # Por que o piso é o padrão, e não zero
///
/// Descer continua existindo — um anel que cresceu para 64 no cabo tem de voltar quando o
/// aparelho troca para o rádio. O que deixou de existir é **descer abaixo do valor que foi medido
/// nos dois regimes**. Zero devolveria o defeito de 01/09 inteiro.
///
/// # Cuidado com a comparação entre dias
///
/// A primeira leitura desta frente comparou o braço adaptativo com a corrida de 01/09 e achou uma
/// regressão de 2,3× nos suspeitos (63,1 → 147,7). **Era quase toda do rádio**: o braço de
/// controle desta tarde, com o anel fixo em 16, mediu 131,2 no mesmo enlace. A regressão real é
/// de 12,6 %, e só o A/B no mesmo enlace podia dizer isso — é a terceira vez que este repositório
/// escreve que duas corridas de 2,4 GHz separadas no tempo não se comparam.
pub const PROFUNDIDADE_MINIMA: usize = PROFUNDIDADE_PADRAO;

/// Quantas desistências a fila junta antes de decidir o tamanho do anel.
///
/// 16 é curto o bastante para o cabo achar o tamanho em segundos — 387 desistências em dois
/// minutos são ~3/s — e longo o bastante para uma rajada isolada não mover nada. Uma decisão por
/// desistência seria ruído; cem seriam um controlador que nunca chega.
pub const DESISTENCIAS_POR_DECISAO: u32 = 16;

/// Metade do espaço de sequência: acima disto, `wrapping_sub` significa "para trás".
const METADE_DA_SEQUENCIA: u16 = u16::MAX / 2;

/// # Por que ele existe no áudio e não existia no vídeo
///
/// O vídeo do Quall não tem jitter buffer **por decisão registrada**: buraco na sequência derruba
/// o quadro, a casca pede IDR, e a imagem volta em um quadro. O número que importa lá é
/// [`Contadores::quadros_descartados`].
///
/// Áudio não tem essa saída. Não existe "IDR de áudio" — todo quadro de Opus é independente e o
/// que passou, passou —, e o sumidouro do outro lado é um DAC, que consome exatamente 48 000
/// amostras por segundo para sempre. Entregar pacote a pacote direto ao DAC produz um estalo a
/// cada pacote atrasado. **O jitter buffer é obrigatório**, ele mora na casca (o núcleo não
/// decodifica nem apresenta), e este número é o que diz de que tamanho ele precisa ser.
///
/// # A conta
///
/// Com `S` o carimbo RTP e `R` a chegada medida na mesma escala:
///
/// ```text
/// D(i-1, i) = (R_i - S_i) - (R_{i-1} - S_{i-1})
/// J(i)      = J(i-1) + (|D(i-1, i)| - J(i-1)) / 16
/// ```
///
/// O `/16` é o filtro passa-baixa que a RFC fixa, e o valor é o mesmo que todo *receiver report*
/// do mundo carrega — o que torna este número comparável com o que um Wireshark ou um `chrome
/// //webrtc-internals` mostraria da mesma sessão.
///
/// Guardado internamente **multiplicado por 16**, que é a forma inteira clássica: de
/// `16·J_novo = 16·J_velho + |D| − J_velho` sai `X += |D| − X/16`. Sem isso, a divisão inteira
/// por 16 zeraria a atualização toda vez que `|D| − J` fosse menor que 16 ticks — a 48 kHz, 333
/// µs — e o estimador ficaria preso perto do zero exatamente na faixa que interessa medir.
#[derive(Debug, Default)]
struct Jitter {
    /// `R − S` do pacote anterior, em ticks. `None` até o segundo pacote: o jitter é uma
    /// diferença de diferenças e precisa de dois.
    transito_anterior: Option<i64>,
    /// O estimador, em **dezesseis avos** de tick.
    acumulado_x16: i64,
    /// Alguma amostra já entrou? Distingue "medido como zero" de "não medido".
    tem_amostra: bool,
}

impl Jitter {
    /// `chegada_ticks` e `carimbo_ticks` têm de estar na **mesma** escala — a da track.
    fn amostrar(&mut self, chegada_ticks: u64, carimbo_ticks: u64) {
        let transito = chegada_ticks as i64 - carimbo_ticks as i64;
        if let Some(anterior) = self.transito_anterior {
            let d = (transito - anterior).abs();
            self.acumulado_x16 += d - self.acumulado_x16 / 16;
            self.tem_amostra = true;
        }
        self.transito_anterior = Some(transito);
    }

    /// O jitter em ticks, ou `None` enquanto não houver duas chegadas para comparar.
    fn ticks(&self) -> Option<u64> {
        self.tem_amostra
            .then(|| u64::try_from(self.acumulado_x16 / 16).unwrap_or(0))
    }
}

/// Um quadro remontado, pronto para o decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuadroRemontado<'a> {
    pub annexb: &'a [u8],
    /// Microssegundos desde o primeiro quadro desta track, derivados do carimbo RTP.
    pub timestamp_us: u64,
    pub idr: bool,
}

/// Remonta quadros H.264 a partir de pacotes RTP.
///
/// Um por track. Não é `Sync`: a libdatachannel entrega os pacotes de uma track sempre na mesma
/// thread, e exigir sincronização aqui seria pagar por um problema que não existe.
#[derive(Debug)]
pub struct Depacotizador {
    /// Quadro em construção. Realoca nos primeiros quadros e nunca mais.
    buffer: Vec<u8>,
    /// Carimbo RTP do quadro em construção.
    carimbo_atual: Option<u32>,
    /// A conta de sequência, compartilhada com o áudio. Ver [`ContaDeSequencia`].
    sequencia: ContaDeSequencia,
    /// O quadro em construção já foi condenado por perda? Continua consumindo pacotes até a
    /// marca, mas não sai.
    condenado: bool,
    /// Faltou alguma posição de sequência **para a frente** antes de um pacote deste quadro.
    ///
    /// É o que faz [`Depacotizador::abortar`] contar o quadro que morre com o buffer vazio — o
    /// que perdeu a cabeça. Ver o teste `quadro_que_perde_a_cabeca_conta_como_descartado`. Salto
    /// para trás não marca: o retardatário é de um quadro que já morreu e já foi contado.
    faltou_pacote: bool,
    /// Alguma NAL do quadro em construção é IDR.
    tem_idr: bool,
    /// Já vimos o começo (`S`) da FU-A em curso? Fragmento do meio sem começo é lixo.
    dentro_de_fu: bool,

    /// O relógio de 90 kHz desta track. Ver [`RelogioRtp`].
    relogio: RelogioRtp,

    /// Quantos pacotes do quadro em construção **chegaram**. Conta só o que passou pelo fio: não
    /// soma buraco nenhum.
    ///
    /// Existe porque a curva medida em 31/08 (`docs/idr-que-sobrevive.md`) mostrou que a perda
    /// deste enlace é **função do tamanho do quadro**, e não da taxa: até ~35 pacotes colados a
    /// quebra é de 1 %, e acima de ~50 o quadro é **truncado pela cauda**. Num quadro truncado
    /// este número **é o ponto de corte**, que é exatamente o que se quer saber.
    ///
    /// # Por que ele não soma o buraco, e isso custou uma corrida
    ///
    /// A primeira versão somava as posições que faltaram, para estimar o tamanho que o emissor
    /// **mandou**. Ela passa no teste de unidade e mente no aparelho: numa corrida real de
    /// 31/08, com origem sintética cujo maior quadro tem 115 pacotes, o contador imprimiu
    /// **296**. O motivo é que um buraco de 180 posições pode cobrir a cauda do quadro que morre,
    /// **vários quadros inteiros no meio**, e a cabeça do que começa — e não há como o receptor
    /// separar os três. Somar tudo ao quadro que morre é atribuir a um quadro o que era de seis.
    ///
    /// O que sobrou é exato e é um **piso** do tamanho do quadro. Piso exato vale mais que
    /// estimativa que passa de 115 para 296 sem avisar. Ver o teste
    /// `buraco_que_cobre_varios_quadros_nao_infla_o_tamanho`.
    pacotes_do_quadro: u32,

    /// Comprimento do buffer no fim da **última NAL completa**. Tudo depois disto é uma NAL que
    /// começou a chegar e não terminou — meia fatia, que é lixo com cabeçalho.
    ///
    /// É o que permite entregar a cabeça de um quadro truncado sem entregar meia fatia junto.
    /// Ver [`Depacotizador::definir_entrega_de_cabeca`].
    fim_da_ultima_nal_completa: usize,
    /// Quantas NAL de imagem (tipos 1..5) do quadro em construção chegaram **inteiras**.
    fatias_completas: u32,
    /// Ver [`Depacotizador::definir_entrega_de_cabeca`]. Nasce **desligado**.
    entregar_cabeca: bool,

    quadros_prontos: u64,
    quadros_descartados: u64,
    idrs_prontos: u64,
    idrs_quebrados: u64,
    maior_quadro_pronto: u32,
    maior_quadro_quebrado: u32,
    /// Ver [`Contadores::cortes_por_faixa`]. Array fixo de propósito: `Contadores` é `Copy` e o
    /// contrato de mídia proíbe alocação em regime.
    cortes_por_faixa: [u32; FAIXAS_DE_CORTE],
    /// Ver [`Contadores::cortes_de_idr_por_faixa`].
    cortes_de_idr_por_faixa: [u32; FAIXAS_DE_CORTE],
    cabecas_entregues: u64,
    fatias_da_ultima_cabeca: u32,
    rtcp_ignorados: u64,
    /// O estágio de ordenação, na frente da máquina de estados. Ver [`FilaDeReordenacao`].
    fila: FilaDeReordenacao,
}

/// Uma leitura **coerente** de todos os contadores de uma track recebida.
///
/// Existe porque ler os contadores um a um os lê em instantes diferentes: cada acesso pega e
/// solta o cadeado da track, e os pacotes continuam chegando entre um e outro. Um relatório
/// montado assim pode dizer, ao mesmo tempo, quantos quadros havia num instante e quantos
/// pacotes faltavam em outro — e a aritmética que liga os dois deixa de fechar. Com quatro
/// números isso passava despercebido; com seis, e com invariantes entre eles, não passa.
///
/// Uma cópia só, tirada sob o mesmo cadeado, custa menos que as leituras separadas que
/// substitui e sai coerente por construção.
/// Quantas faixas o histograma de corte tem: 20 de dez pacotes (0–9 … 190–199) mais um balde
/// final para 200 ou mais.
pub const FAIXAS_DE_CORTE: usize = 21;

/// A largura de cada faixa do histograma, em pacotes.
pub const LARGURA_DA_FAIXA_DE_CORTE: u32 = 10;

/// Em qual faixa cai um corte de `pacotes` pacotes recebidos.
#[inline]
fn faixa_do_corte(pacotes: u32) -> usize {
    ((pacotes / LARGURA_DA_FAIXA_DE_CORTE) as usize).min(FAIXAS_DE_CORTE - 1)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Contadores {
    pub quadros_prontos: u64,
    pub quadros_descartados: u64,
    /// Quadros entregues que carregavam fatia IDR.
    pub idrs_prontos: u64,
    /// **IDR que começou a chegar e foi destruído.** O par dele do outro lado é `idrs_sent`, e a
    /// diferença entre os dois era invisível até 31/08: no vídeo da bancada o emissor dizia 31 e
    /// o receptor dizia 13, e nada no receptor contava os 18 que faltavam. Ver
    /// `docs/idr-que-sobrevive.md`.
    ///
    /// Um IDR truncado é sempre contado: a perda deste enlace mata a **cauda** da rajada, e o
    /// tipo da NAL (que é o que marca o quadro como IDR) viaja no primeiro pacote dela.
    pub idrs_quebrados: u64,
    /// Tamanho, em pacotes, do maior quadro **entregue inteiro**. É exato: um quadro que fechou
    /// teve todos os pacotes dele contados.
    pub maior_quadro_pronto_pacotes: u32,
    /// **Quantos pacotes chegaram** do quadro destruído que mais recebeu antes de morrer — e não
    /// o tamanho que o emissor mandou, que este lado não tem como saber. No regime de truncamento
    /// de cauda medido em 31/08 este número **é o ponto de corte do enlace**.
    ///
    /// Ver [`Depacotizador::pacotes_do_quadro`] para o que aconteceu quando ele tentava estimar o
    /// tamanho mandado: imprimiu 296 pacotes numa origem cujo maior quadro tem 115.
    pub maior_quebrado_pacotes_recebidos: u32,
    /// **A distribuição dos pontos de corte**, e não só o máximo.
    ///
    /// `maior_quebrado_pacotes_recebidos` responde "qual foi o pior", e isso não distingue os dois
    /// regimes que decidem o conserto — `docs/bancada.md` §8.24:
    ///
    /// - **truncamento por fila:** a rajada é *causada* pelo quadro grande, o corte cai sempre na
    ///   mesma faixa, e a rajada **caça** o IDR. Aí o conserto é encolher o quadro abaixo do corte.
    /// - **rajada cega:** a perda é do meio e atinge o quadro por acaso; os cortes se espalham
    ///   por toda a faixa. Aí encolher compra pouco e o conserto é de transporte.
    ///
    /// Em 04/09 os quatro máximos disponíveis caíram entre 46 e 69 pacotes, o que *parece*
    /// truncamento — mas quatro pontos não são uma distribuição, e a hipótese ficou aberta por
    /// falta exatamente deste contador.
    ///
    /// Índice `i` conta os quadros abortados com `i*10 .. i*10+9` pacotes recebidos; o último
    /// balde acumula 200 ou mais. Ver [`FAIXAS_DE_CORTE`].
    pub cortes_por_faixa: [u32; FAIXAS_DE_CORTE],
    /// **O mesmo histograma, mas só dos IDR** — e sem ele o outro não decide nada.
    ///
    /// `cortes_por_faixa` conta **todo** quadro abortado, e a esmagadora maioria dos quadros é P.
    /// Um P de 15 pacotes que perde um conta como "corte em 14", o que não é truncamento: é o
    /// quadro inteiro morrendo. Misturados, os dois enchem as faixas baixas e **imitam** a
    /// assinatura de truncamento que se está procurando — foi o que quase aconteceu na primeira
    /// leitura de 04/09, com nove cortes entre 10 e 69 dos quais só um era de IDR.
    ///
    /// A pergunta da §8.24 é sobre o IDR, que é o objeto grande e o que congela a tela quando
    /// morre. Este contador é o que a responde: se os cortes de **IDR** se agrupam muito abaixo do
    /// tamanho do IDR, o enlace trunca; se acompanham o tamanho, a rajada é cega.
    pub cortes_de_idr_por_faixa: [u32; FAIXAS_DE_CORTE],
    /// Ver [`Depacotizador::pacotes_faltando`].
    pub pacotes_faltando: u64,
    /// Ver [`Depacotizador::eventos_fora_de_ordem`].
    pub eventos_fora_de_ordem: u64,
    /// Ver [`Depacotizador::pacotes_vistos`].
    pub pacotes_vistos: u64,
    /// **A perda exata**: posições de sequência que saíram da janela de reordenação sem nunca
    /// terem chegado. Ver [`Depacotizador::pacotes_perdidos_de_verdade`].
    pub pacotes_perdidos_de_verdade: u64,
    /// Pacotes que chegaram depois de a posição deles já ter saído da janela. Se não for zero,
    /// [`Self::pacotes_perdidos_de_verdade`] está superestimado nesse tanto.
    pub pacotes_tarde_demais: u64,
    /// Quantas reordenações a fila absorveu — pacotes que chegaram fora de ordem, esperaram no
    /// anel e foram entregues no lugar certo **sem condenar quadro nenhum**. Ver
    /// [`FilaDeReordenacao`].
    pub reordenacoes_absorvidas: u64,
    /// **Quantas vezes a fila desistiu de um buraco.** Era o número que faltava para ler o anel:
    /// 1293 absorvidas no cabo de 01/09/2026 não diziam nada sozinhas, porque ninguém sabia
    /// quantas tinham ficado na mesa. Ver [`Depacotizador::desistencias_de_reordenacao`].
    pub desistencias_de_reordenacao: u64,
    /// **A profundidade do anel agora, em pacotes.** É o regime que o produto leu da rede: acima
    /// de [`PROFUNDIDADE_PADRAO`] é cabo (reordena e não perde), abaixo é rádio (perde e não
    /// reordena). Ver [`FilaDeReordenacao`].
    pub profundidade_de_reordenacao: u32,
    /// **Quantas vezes o anel mudou de tamanho.** Sem ele, [`Self::profundidade_de_reordenacao`]
    /// é uma fotografia do fim e não diz se o controlador andou direto até lá ou ficou oscilando
    /// — que é justamente o modo de falha contra o qual a banda morta existe. Pego na primeira
    /// corrida de bancada do anel, em 02/09/2026: o Wi-Fi terminou em 4 e não havia como saber
    /// em quantos passos.
    pub ajustes_de_reordenacao: u64,
    pub rtcp_ignorados: u64,
    /// **Jitter de chegada da RFC 3550 §6.4.1, em microssegundos.** Ver [`Jitter`].
    ///
    /// `None` quer dizer **não medido**, e não zero. Hoje é `None` em duas situações, e as duas
    /// são honestas:
    ///
    /// - **em toda track de vídeo**, porque o vídeo não tem jitter buffer por decisão registrada
    ///   e ninguém agiria sobre o número. Medi-lo custaria um `Instant::now()` por pacote — e um
    ///   IDR de 1080p são ~85 pacotes em rajada;
    /// - **numa track de áudio que ainda não recebeu dois pacotes**, porque jitter é uma
    ///   diferença de diferenças e um pacote só não tem com o que ser comparado.
    ///
    /// A distinção entre "não medido" e "medido como zero" é a mesma lição da dívida 26: um
    /// contador que finge saber é pior que um que admite não saber.
    pub jitter_us: Option<u32>,
}

impl Contadores {
    /// A soma histórica, que é o que `sequence_anomalies` sempre significou.
    pub fn pacotes_perdidos(&self) -> u64 {
        self.pacotes_faltando
            .saturating_add(self.eventos_fora_de_ordem)
    }
}

impl Default for Depacotizador {
    fn default() -> Self {
        Self::new()
    }
}

impl Depacotizador {
    /// Quanto o buffer de remontagem reserva de saída.
    ///
    /// 256 KiB cobre um IDR de 1080p com folga; quadros maiores fazem o `Vec` crescer uma vez e
    /// ficar grande. É um custo fixo por track, não por quadro.
    const RESERVA: usize = 256 * 1024;

    pub fn new() -> Self {
        Depacotizador {
            buffer: Vec::with_capacity(Self::RESERVA),
            carimbo_atual: None,
            sequencia: ContaDeSequencia::default(),
            condenado: false,
            faltou_pacote: false,
            tem_idr: false,
            dentro_de_fu: false,
            relogio: RelogioRtp::novo(RELOGIO_VIDEO_HZ),
            pacotes_do_quadro: 0,
            quadros_prontos: 0,
            quadros_descartados: 0,
            idrs_prontos: 0,
            idrs_quebrados: 0,
            maior_quadro_pronto: 0,
            maior_quadro_quebrado: 0,
            cortes_por_faixa: [0; FAIXAS_DE_CORTE],
            cortes_de_idr_por_faixa: [0; FAIXAS_DE_CORTE],
            fim_da_ultima_nal_completa: 0,
            fatias_completas: 0,
            entregar_cabeca: false,
            cabecas_entregues: 0,
            fatias_da_ultima_cabeca: 0,
            rtcp_ignorados: 0,
            fila: FilaDeReordenacao::nova(PROFUNDIDADE_PADRAO),
        }
    }

    /// **Entregar a cabeça de um quadro truncado em vez de jogá-la fora.** Nasce desligado.
    ///
    /// # O defeito que isto endereça
    ///
    /// `docs/idr-que-sobrevive.md` mediu que a fila de descida deste enlace de 2,4 GHz satura em
    /// ~50 fragmentos IP e **trunca a cauda**: dos 37 quadros grandes quebrados, 34 têm a forma
    /// `N+M−` — a cabeça chega inteira, a cauda morre inteira. Num IDR de 60 pacotes cortado em
    /// 40 chegam **dois terços da imagem**, e hoje [`Depacotizador::abortar`] joga os dois terços
    /// fora junto com o terço que faltou.
    ///
    /// # Por que nasce desligado, e o que falta para ligá-lo
    ///
    /// Medido em 31/08/2026 (`docs/pintar-a-cabeca.md`): os três Android da bancada **aceitam** a
    /// unidade truncada e pintam a cabeça — mas o `VTDecompressionSession` da Apple a **recusa**
    /// com `-12909` (`docs/idr-pequeno.md`, §5). O núcleo é um só e alimenta as cinco cascas, e
    /// entregar a cabeça a uma casca Apple faria os contadores dizerem que a referência foi
    /// restaurada quando o decodificador dela não decodificou nada — um contador que mente é pior
    /// que um quadro perdido. Ligar isto em produto exige antes uma marca no quadro entregue
    /// (`QuadroCodificado`), que é mudança de fronteira e não é desta frente.
    ///
    /// # A garantia que ele dá
    ///
    /// A cabeça entregue termina **no fim de uma NAL completa**, nunca no meio de uma. Meia fatia
    /// é uma NAL com cabeçalho e sem fim, e um decodificador que a aceite decodifica os
    /// macroblocos que chegaram — mas essa é sorte de fornecedor, não contrato, e o
    /// depacotizador não a distribui.
    pub fn definir_entrega_de_cabeca(&mut self, ligado: bool) {
        self.entregar_cabeca = ligado;
    }

    /// Quantas cabeças de quadro truncado foram entregues. Zero quando a chave está desligada.
    ///
    /// Contador separado de `quadros_prontos` **de propósito**: um quadro entregue pela metade
    /// não é um quadro entregue, e somá-los apagaria exatamente a distinção que esta frente
    /// existe para medir.
    pub fn cabecas_entregues(&self) -> u64 {
        self.cabecas_entregues
    }

    /// Fatias completas que a última cabeça entregue carregava. Zero quando não houve nenhuma.
    pub fn fatias_da_ultima_cabeca(&self) -> u32 {
        self.fatias_da_ultima_cabeca
    }

    /// O carimbo RTP cru que virou a origem do `timestamp_us` entregue: o do **primeiro quadro
    /// entregue**, e não o do primeiro pacote. `None` antes do primeiro quadro.
    pub fn base_do_relogio(&self) -> Option<u32> {
        self.relogio.base()
    }

    /// A base publicada num atômico: legível **sem** o cadeado de quem guarda este depacotizador.
    /// Ver [`BasePublicada`].
    pub fn base_publicada(&self) -> BasePublicada {
        self.relogio.publicada.clone()
    }

    /// Todos os contadores de uma vez, no mesmo instante. Ver [`Contadores`].
    pub fn contadores(&self) -> Contadores {
        Contadores {
            quadros_prontos: self.quadros_prontos,
            quadros_descartados: self.quadros_descartados,
            idrs_prontos: self.idrs_prontos,
            idrs_quebrados: self.idrs_quebrados,
            maior_quadro_pronto_pacotes: self.maior_quadro_pronto,
            maior_quebrado_pacotes_recebidos: self.maior_quadro_quebrado,
            cortes_por_faixa: self.cortes_por_faixa,
            cortes_de_idr_por_faixa: self.cortes_de_idr_por_faixa,
            pacotes_faltando: self.sequencia.faltando,
            eventos_fora_de_ordem: self.sequencia.fora_de_ordem,
            pacotes_vistos: self.sequencia.vistos,
            pacotes_perdidos_de_verdade: self.sequencia.perda_exata(),
            pacotes_tarde_demais: self.sequencia.tarde_demais,
            reordenacoes_absorvidas: self.fila.absorvidos,
            desistencias_de_reordenacao: self.fila.desistencias_totais,
            profundidade_de_reordenacao: self.fila.profundidade as u32,
            ajustes_de_reordenacao: self.fila.ajustes,
            rtcp_ignorados: self.rtcp_ignorados,
            // Ver [`Contadores::jitter_us`]: no vídeo ele não é medido, e `None` diz isso em vez
            // de fingir um zero.
            jitter_us: None,
        }
    }

    /// Quadros entregues inteiros.
    pub fn quadros_prontos(&self) -> u64 {
        self.quadros_prontos
    }

    /// Quadros jogados fora por perda ou desordem. É o número que diz se a rede está ruim o
    /// bastante para o desenho sem jitter buffer doer.
    pub fn quadros_descartados(&self) -> u64 {
        self.quadros_descartados
    }

    /// Quadros entregues que carregavam fatia IDR. Ver [`Contadores::idrs_prontos`].
    pub fn idrs_prontos(&self) -> u64 {
        self.idrs_prontos
    }

    /// IDR que começou a chegar e foi destruído. Ver [`Contadores::idrs_quebrados`].
    pub fn idrs_quebrados(&self) -> u64 {
        self.idrs_quebrados
    }

    /// Tamanho, em posições de sequência, do maior quadro entregue inteiro.
    pub fn maior_quadro_pronto_pacotes(&self) -> u32 {
        self.maior_quadro_pronto
    }

    /// Quantos pacotes chegaram do quadro destruído que mais recebeu antes de morrer.
    pub fn maior_quebrado_pacotes_recebidos(&self) -> u32 {
        self.maior_quadro_quebrado
    }

    /// Anomalias na sequência RTP: pacote que faltou, repetiu ou chegou fora de ordem.
    ///
    /// **É a soma de [`Self::pacotes_faltando`] com [`Self::eventos_fora_de_ordem`]**, e continua
    /// valendo exatamente o que sempre valeu — é o número que sai como `sequence_anomalies` e que
    /// o `quall-probe`, o plugin de OBS, as sondas de câmera e as cascas já leem.
    ///
    /// **Não é a contagem exata de pacotes perdidos.** Sem jitter buffer, uma única troca de
    /// ordem (`…99, 101, 100, 102…`) aparece três vezes: o salto para 101, o salto para trás até
    /// 100 e o salto de volta para 102. Quem quiser saber **se perdeu ou se reordenou** lê os
    /// dois lados separados, que é para isso que eles existem.
    pub fn pacotes_perdidos(&self) -> u64 {
        self.sequencia
            .faltando
            .saturating_add(self.sequencia.fora_de_ordem)
    }

    /// **Posições de sequência que nunca chegaram** — a soma dos saltos para a frente.
    ///
    /// É perda de verdade, e em bloco: um salto de `n` conta `n − 1`.
    ///
    /// # É um teto, e é exato quando ninguém reordenou
    ///
    /// Um pacote atrasado é contado duas vezes por este número: uma no salto que passou por cima
    /// dele e outra no salto de volta que o ultrapassa de novo. Um pacote deslocado de `d`
    /// posições infla este contador em `1 + d` sem que nada tenha se perdido de fato. Por isso a
    /// leitura correta é:
    ///
    /// - com [`Self::eventos_fora_de_ordem`] em **zero**, este número é a perda **exata**;
    /// - com ele diferente de zero, este número é um **limite superior** da perda, e o excesso é
    ///   proporcional a quão longe os pacotes foram deslocados — que este desenho não mede.
    ///
    /// A alternativa que mediria a perda exata mesmo sob reordenação é a conta de
    /// `expected − received` da RFC 3550 §A.3, com número de sequência estendido. Ela foi
    /// descartada aqui porque mudaria o que [`Self::pacotes_perdidos`] devolve — e esse número já
    /// está publicado em `sequence_anomalies`, lido por quatro cascas.
    pub fn pacotes_faltando(&self) -> u64 {
        self.sequencia.faltando
    }

    /// **Pacote repetido ou que chegou fora de ordem**, contado como *um evento*.
    ///
    /// Não é contagem de pacote: é contagem de vezes em que a sequência andou para trás ou
    /// repetiu. Diferente de zero quer dizer que [`Self::pacotes_faltando`] está inflado, e que a
    /// resposta à pergunta "perdeu ou reordenou?" é "as duas coisas".
    pub fn eventos_fora_de_ordem(&self) -> u64 {
        self.sequencia.fora_de_ordem
    }

    /// **Pacotes que entraram na conta de sequência**, incluindo o primeiro.
    ///
    /// É a *janela observada*, e existe para tornar explícita a única coisa que este contador
    /// não pode saber: `ultima_sequencia` começa `None`, **o primeiro pacote visto fixa a linha
    /// de base**, e nada que tenha caído antes dele pode ser contado. Um número de sequência RTP
    /// não diz nada sobre o que veio antes do primeiro que se viu — não é limitação da
    /// implementação, é do protocolo, e o contador não deve fingir o contrário.
    ///
    /// Com ele, a taxa de perda vira uma conta local:
    /// `pacotes_faltando / (pacotes_faltando + pacotes_vistos)`. Sem ele, o denominador precisa
    /// vir de fora — na medição de 26/08 ele veio do `/proc/net/dev` **da outra máquina**.
    ///
    /// Vale como invariante, quando não houve repetição nem troca de ordem:
    /// `pacotes_vistos + pacotes_faltando` é o tamanho do intervalo de sequência coberto.
    ///
    /// Zero quer dizer que nenhum pacote de mídia chegou ainda — e aí nem "não perdeu nada" pode
    /// ser afirmado.
    pub fn pacotes_vistos(&self) -> u64 {
        self.sequencia.vistos
    }

    /// **A perda exata.** Posições de sequência que saíram da janela de reordenação
    /// (`LARGURA_DA_JANELA` posições, ~550 ms na forma de tráfego desta bancada) sem nunca terem
    /// chegado.
    ///
    /// # Por que ele não é [`Self::pacotes_faltando`], e por que a diferença é grande
    ///
    /// `pacotes_faltando` compara cada pacote **só com o anterior**. Uma reordenação de distância
    /// `d` cobra `d` posições que depois chegam e nunca são descontadas — está documentado como
    /// teto desde a dívida 26, e mesmo assim foi lido como perda em toda medição desta bancada,
    /// porque não havia um número melhor ao lado.
    ///
    /// Medido em 29/08, MacBook → A10s com origem sintética: `pacotes_faltando` = 486 (1,72 %)
    /// com 70 eventos fora de ordem, contra no máximo 50 pacotes que o emissor entregou e o
    /// receptor não viu. **Dez vezes.**
    ///
    /// Este número exclui as posições que ainda estão dentro da janela no momento da leitura: no
    /// fim de uma sessão elas são ambíguas, e são no máximo `LARGURA_DA_JANELA`.
    pub fn pacotes_perdidos_de_verdade(&self) -> u64 {
        self.sequencia.perda_exata()
    }

    /// Pacotes que chegaram depois de a posição deles já ter saído da janela de reordenação.
    ///
    /// Diferente de zero quer dizer que [`Self::pacotes_perdidos_de_verdade`] está
    /// **superestimado** nesse tanto — a janela foi curta demais para o que a rede fez.
    pub fn pacotes_tarde_demais(&self) -> u64 {
        self.sequencia.tarde_demais
    }

    /// Pacotes RTCP que chegaram pelo caminho do RTP e foram ignorados.
    ///
    /// Deve ser **zero**: a libdatachannel filtra RTCP antes daqui. Diferente de zero significa
    /// que a track está sem a sessão de RTCP encadeada, ou que o filtro dela mudou.
    pub fn rtcp_ignorados(&self) -> u64 {
        self.rtcp_ignorados
    }

    /// Crava a profundidade do anel, em pacotes, e **desliga o ajuste automático**. `0` desliga a
    /// fila inteira e restaura exatamente o comportamento anterior a 01/09/2026 — o que faz deste
    /// botão o instrumento para medir o antes e o depois no mesmo binário, em vez de comparar duas
    /// compilações.
    ///
    /// Desligar o ajuste junto não é efeito colateral: quem crava um número na bancada quer medir
    /// **aquele** número, e um anel que fugisse do valor pedido no meio da corrida tornaria o
    /// braço de aferição impossível de ler.
    ///
    /// Ver [`FilaDeReordenacao`] para por que o limite é em pacotes e não em milissegundos, e para
    /// como o anel escolhe sozinho quando ninguém crava.
    pub fn definir_profundidade_de_reordenacao(&mut self, pacotes: usize) {
        self.fila = FilaDeReordenacao::com_ajuste(pacotes, false);
    }

    /// A profundidade do anel **agora**. Diferente de [`PROFUNDIDADE_PADRAO`] quer dizer que o
    /// ajuste automático leu o regime e mexeu — para cima é cabo (reordena), para baixo é rádio
    /// (perde). Ver [`FilaDeReordenacao`].
    pub fn profundidade_de_reordenacao(&self) -> usize {
        self.fila.profundidade
    }

    /// Quantas vezes o anel mudou de tamanho nesta sessão.
    pub fn ajustes_de_reordenacao(&self) -> u64 {
        self.fila.ajustes
    }

    /// Quantas vezes a fila desistiu de um buraco — o denominador de que o ajuste automático se
    /// serve, e o número que dizia "sobraram 387" no cabo de 01/09/2026.
    pub fn desistencias_de_reordenacao(&self) -> u64 {
        self.fila.desistencias_totais
    }

    /// Quantas reordenações a fila absorveu — pacotes que chegaram fora de ordem, esperaram, e
    /// foram entregues no lugar certo **sem condenar quadro nenhum**. É o número que prova que
    /// o estágio trabalhou.
    pub fn reordenacoes_absorvidas(&self) -> u64 {
        self.fila.absorvidos
    }

    /// Consome um pacote RTP e, quando ele fecha um quadro, chama `entregar`.
    ///
    /// A entrega é por callback, e não por retorno, porque o quadro é uma vista do buffer
    /// interno: devolvê-lo obrigaria a emprestar `self` até o consumidor terminar, o que
    /// impediria o próximo pacote de entrar. Callback é o que mantém "empacota e solta".
    ///
    /// Pacote malformado é **erro**, não pânico, e não derruba a track: quem chama conta e
    /// segue.
    /// Consome um pacote RTP e, quando ele fecha um quadro, chama `entregar`.
    ///
    /// **Ordena antes de entregar.** O pacote que chega na sequência esperada atravessa na hora,
    /// sem custo; o que chega adiantado espera no anel até o buraco fechar. Ver
    /// [`FilaDeReordenacao`] para o número que criou este estágio.
    ///
    /// A entrega é por callback, e não por retorno, porque o quadro é uma vista do buffer
    /// interno: devolvê-lo obrigaria a emprestar `self` até o consumidor terminar, o que
    /// impediria o próximo pacote de entrar. Callback é o que mantém "empacota e solta".
    ///
    /// Pacote malformado é **erro**, não pânico, e não derruba a track: quem chama conta e
    /// segue.
    pub fn aceitar(
        &mut self,
        bytes: &[u8],
        mut entregar: impl FnMut(QuadroRemontado<'_>),
    ) -> Result<()> {
        // O RTCP sai **antes** da fila: o "número de sequência" de um RTCP é, na verdade, o
        // campo de comprimento dele, e deixá-lo entrar aqui inventaria buracos — o mesmo motivo
        // pelo qual ele já saía antes do `conferir`.
        let (sequencia, e_rtcp) = {
            let p = PacoteRtp::parse(bytes)?;
            (p.sequencia, p.e_rtcp_multiplexado())
        };
        if e_rtcp {
            self.rtcp_ignorados += 1;
            return Ok(());
        }
        if self.fila.profundidade == 0 {
            return self.aceitar_em_ordem(bytes, &mut entregar);
        }

        // O primeiro pacote **fixa a linha de base**, como em `ContaDeSequencia`: não há com o
        // que comparar, e nada que tenha caído antes dele pode ser contado.
        let proximo = match self.fila.proximo {
            None => {
                self.fila.proximo = Some(sequencia);
                sequencia
            }
            Some(p) => p,
        };
        let distancia = sequencia.wrapping_sub(proximo);

        // 1. Na ordem: caminho quente, sem fila e sem latência.
        if distancia == 0 {
            self.aceitar_em_ordem(bytes, &mut entregar)?;
            self.fila.proximo = Some(proximo.wrapping_add(1));
            return self.escoar(&mut entregar);
        }

        // 2. Para trás: chegou depois de já termos desistido dele. Entregar agora faria a
        //    máquina de estados ver a sequência andar para trás, que é exatamente o defeito que
        //    esta fila existe para apagar. Conta e descarta.
        if distancia > METADE_DA_SEQUENCIA {
            // **Conta como evento fora de ordem, e não como `tarde_demais`.** O segundo tem
            // significado documentado e específico — "a janela de 128 posições foi curta para o
            // que a rede fez" — e sequestrá-lo aqui apagaria a informação que ele carrega. Um
            // pacote que chega depois de a fila desistir é, literalmente, um pacote fora de
            // ordem, e é assim que ele entra na conta.
            self.sequencia.fora_de_ordem = self.sequencia.fora_de_ordem.saturating_add(1);
            self.sequencia.vistos = self.sequencia.vistos.saturating_add(1);
            // **E marca na janela, que é o conserto de 18h30 de 01/09/2026.** Sem esta linha o
            // pacote é descartado sem que a janela de 128 posições saiba que ele CHEGOU, e a
            // posição vira `nunca_chegaram` — ou seja, `packets_lost_for_real` acusa perda que
            // não houve. A primeira corrida com a fila mediu `perda exata = teto = reorder_events
            // = 379`, os três idênticos, contra perda exata ZERO no mesmo cabo sem a fila: três
            // números iguais não são rede, são assinatura de contabilidade.
            //
            // Não é estética: `packets_lost_for_real` é o número que o `taxa.rs` usa para
            // decidir, e perda fantasma faria o controlador recuar sem motivo.
            self.sequencia.marcar_na_janela(sequencia);
            // **E é aqui que o anel descobre em que rede está.** Se este é justamente o pacote de
            // que a fila desistiu, a desistência foi prematura: era reordenação, não perda, e o
            // anel é curto para esta rede. Ver `FilaDeReordenacao`.
            self.fila.julgar_atrasado(sequencia);
            return Ok(());
        }

        // 3. Adiantado e dentro do anel: espera o buraco fechar.
        if (distancia as usize) < self.fila.profundidade {
            self.fila.guardar(sequencia, bytes);
            if self.fila.guardados >= self.fila.profundidade {
                // O anel encheu sem o buraco fechar: desiste dele. O que estava preso **não se
                // joga fora** — entrega-se em ordem, e o salto que sobra é visto pelo
                // `ContaDeSequencia`, que condena o quadro como sempre condenou.
                self.desistir(&mut entregar)?;
            }
            return Ok(());
        }

        // 4. Longe demais à frente para o anel: o buraco não vai fechar.
        self.desistir(&mut entregar)?;
        self.aceitar_em_ordem(bytes, &mut entregar)?;
        self.fila.proximo = Some(sequencia.wrapping_add(1));
        self.escoar(&mut entregar)
    }

    /// Solta os pacotes que ficaram contíguos depois de o buraco fechar.
    fn escoar<F: FnMut(QuadroRemontado<'_>)>(&mut self, entregar: &mut F) -> Result<()> {
        while let Some(prox) = self.fila.proximo {
            let Some(bytes) = self.fila.tirar(prox) else {
                break;
            };
            // **Uma reordenação absorvida.** É o número que prova que a fila trabalhou; sem ele
            // o conserto arruma a imagem e some da medida.
            self.fila.absorvidos = self.fila.absorvidos.saturating_add(1);
            self.aceitar_em_ordem(&bytes, entregar)?;
            self.fila.proximo = Some(prox.wrapping_add(1));
        }
        Ok(())
    }

    /// Desiste do buraco: entrega em ordem o que estiver preso e volta a fixar a linha de base
    /// no próximo pacote. O salto que sobra chega ao `ContaDeSequencia` e condena o quadro, que
    /// é o comportamento correto para **perda** — e continua intacto.
    fn desistir<F: FnMut(QuadroRemontado<'_>)>(&mut self, entregar: &mut F) -> Result<()> {
        let base = self.fila.proximo.unwrap_or(0);
        for bytes in self.fila.drenar_tudo(base) {
            self.aceitar_em_ordem(&bytes, entregar)?;
        }
        self.fila.proximo = None;
        // **Depois do dreno, e não antes**: o anel está vazio e a linha de base zerada, que é o
        // único instante em que trocar a profundidade não embaralha os índices.
        self.fila.registrar_desistencia(base);
        Ok(())
    }

    fn aceitar_em_ordem<F: FnMut(QuadroRemontado<'_>)>(
        &mut self,
        bytes: &[u8],
        entregar: &mut F,
    ) -> Result<()> {
        let pacote = PacoteRtp::parse(bytes)?;
        if pacote.e_rtcp_multiplexado() {
            // Sai antes de tocar na sequência: o número de sequência de um RTCP é, na verdade,
            // o campo de comprimento dele, e deixá-lo entrar em `conferir_sequencia` inventaria
            // milhares de pacotes perdidos.
            self.rtcp_ignorados += 1;
            return Ok(());
        }
        if pacote.payload.is_empty() {
            return Err(Error::Protocol("pacote RTP sem payload".into()));
        }

        // O tamanho do buraco, e não só a existência dele. É o que permite estimar o tamanho do
        // quadro em posições de sequência mesmo quando a cauda dele nunca chegou — o caso que a
        // curva de 31/08 mostrou ser o normal, não a exceção.
        let salto = self.sequencia.conferir(pacote.sequencia);

        // Carimbo diferente do quadro em construção significa que o anterior nunca fechou —
        // perdemos o pacote da marca. O quadro em construção morre aqui.
        //
        // **É por esta porta que passa o truncamento de cauda**, e não pela do `fechar`: quando a
        // fila do enlace corta a rajada no pacote ~40, o pacote da marca é justamente um dos que
        // morrem, e o quadro só é notado como morto quando chega o primeiro pacote do seguinte.
        // Por isso a tentativa de entregar a cabeça mora aqui.
        if matches!(self.carimbo_atual, Some(atual) if atual != pacote.carimbo) {
            self.abortar_entregando_a_cabeca(&mut *entregar);
        }
        // A condenação é marcada **depois** do `abortar`, e não dentro do `conferir_sequencia`.
        //
        // Não é detalhe de ordem: `abortar` chama `limpar`, que zera `condenado`. Marcando
        // antes, um buraco que caísse exatamente na fronteira entre dois quadros seria esquecido
        // ao trocar de carimbo — e o quadro novo sairia com um pedaço faltando, silenciosamente.
        // O pacote perdido pode ser do quadro velho (que já morreu) ou do novo; não dá para
        // saber, e entregar quadro furado é pior que descartar um quadro bom.
        if salto != Salto::Seguido {
            self.condenado = true;
        }
        if salto == Salto::Adiante {
            self.faltou_pacote = true;
        }
        self.pacotes_do_quadro = self.pacotes_do_quadro.saturating_add(1);
        self.carimbo_atual = Some(pacote.carimbo);

        let cabecalho = pacote.payload[0];
        let tipo = cabecalho & 0x1f;

        match tipo {
            1..=23 => self.nal_unica(pacote.payload),
            nal::STAP_A => self.stap_a(pacote.payload)?,
            nal::FU_A => self.fu_a(pacote.payload)?,
            outro => {
                // STAP-B (25), MTAP16 (26), MTAP24 (27) e FU-B (29) existem na RFC 6184 mas
                // nenhum emissor do Quall os produz — o pacotizador da libdatachannel só emite
                // Single NAL e FU-A. Chegar aqui é a outra ponta não ser o Quall.
                self.abortar();
                return Err(Error::Protocol(format!(
                    "payload H.264 do tipo {outro}, que o Quall não produz nem consome"
                )));
            }
        }

        if pacote.marca {
            self.fechar(pacote.carimbo, &mut *entregar);
        }
        Ok(())
    }

    fn nal_unica(&mut self, payload: &[u8]) {
        self.dentro_de_fu = false;
        self.marcar_idr(payload[0] & 0x1f);
        self.buffer.extend_from_slice(&START_CODE);
        self.buffer.extend_from_slice(payload);
        self.fechou_uma_nal(payload[0] & 0x1f);
    }

    /// Uma NAL acabou de entrar **inteira** no buffer.
    ///
    /// Só marca enquanto o quadro não estiver condenado: depois do primeiro buraco, o que vem
    /// atrás pode ser a continuação de outra coisa, e a cabeça segura é o **prefixo anterior ao
    /// primeiro buraco**. Congelar aqui é o que faz [`Depacotizador::entregar_cabeca`] valer
    /// também para o buraco no meio, e não só para o truncamento de cauda.
    fn fechou_uma_nal(&mut self, tipo: u8) {
        if self.condenado {
            return;
        }
        self.fim_da_ultima_nal_completa = self.buffer.len();
        if (1..=5).contains(&tipo) {
            self.fatias_completas = self.fatias_completas.saturating_add(1);
        }
    }

    fn stap_a(&mut self, payload: &[u8]) -> Result<()> {
        self.dentro_de_fu = false;
        // 1 byte de cabeçalho STAP-A, depois pares (tamanho de 16 bits, NAL unit).
        let mut i = 1usize;
        while i < payload.len() {
            if i + 2 > payload.len() {
                self.abortar();
                return Err(Error::Protocol("STAP-A truncado no tamanho".into()));
            }
            let tamanho = usize::from(u16::from_be_bytes([payload[i], payload[i + 1]]));
            i += 2;
            if tamanho == 0 || i + tamanho > payload.len() {
                self.abortar();
                return Err(Error::Protocol(format!(
                    "STAP-A anuncia NAL de {tamanho} bytes e só há {}",
                    payload.len() - i
                )));
            }
            self.marcar_idr(payload[i] & 0x1f);
            self.buffer.extend_from_slice(&START_CODE);
            self.buffer.extend_from_slice(&payload[i..i + tamanho]);
            self.fechou_uma_nal(payload[i] & 0x1f);
            i += tamanho;
        }
        Ok(())
    }

    fn fu_a(&mut self, payload: &[u8]) -> Result<()> {
        if payload.len() < 3 {
            self.abortar();
            return Err(Error::Protocol("FU-A sem cabeçalho de fragmentação".into()));
        }
        let indicador = payload[0];
        let cabecalho_fu = payload[1];
        let comeco = cabecalho_fu & 0b1000_0000 != 0;
        let fim = cabecalho_fu & 0b0100_0000 != 0;
        let tipo = cabecalho_fu & 0x1f;

        if comeco {
            // Reconstrói o cabeçalho da NAL original: F e NRI vêm do indicador, o tipo vem do
            // cabeçalho de fragmentação (RFC 6184 §5.8).
            let original = (indicador & 0b1110_0000) | tipo;
            self.marcar_idr(tipo);
            self.buffer.extend_from_slice(&START_CODE);
            self.buffer.push(original);
            self.dentro_de_fu = true;
        } else if !self.dentro_de_fu {
            // Fragmento do meio sem ter visto o começo: o começo se perdeu. Escrever isto no
            // buffer produziria uma NAL sem cabeçalho, que é lixo para o decoder.
            self.condenado = true;
            return Ok(());
        }

        self.buffer.extend_from_slice(&payload[2..]);
        if fim {
            self.dentro_de_fu = false;
            self.fechou_uma_nal(tipo);
        }
        Ok(())
    }

    fn marcar_idr(&mut self, tipo: u8) {
        // SPS e PPS acompanham o IDR e não bastam sozinhos para marcar o quadro; quem manda é a
        // fatia IDR de fato.
        if tipo == nal::IDR {
            self.tem_idr = true;
        }
    }

    fn abortar(&mut self) {
        // Buffer vazio **com** pacote faltando é o quadro que perdeu a cabeça: o resto dele
        // chegou, e fragmento de FU-A sem começo não escreve nada. Ele morreu como qualquer outro e
        // tem de subir `quadros_descartados`, que é o gatilho do pedido de IDR nas cascas. Buffer
        // vazio **sem** falta é entrar no fluxo pelo meio de um quadro, e isso não é perda.
        if !self.buffer.is_empty() || self.faltou_pacote {
            self.quadros_descartados += 1;
            if self.tem_idr {
                self.idrs_quebrados += 1;
            }
            self.maior_quadro_quebrado = self.maior_quadro_quebrado.max(self.pacotes_do_quadro);
            let faixa = faixa_do_corte(self.pacotes_do_quadro);
            self.cortes_por_faixa[faixa] += 1;
            if self.tem_idr {
                self.cortes_de_idr_por_faixa[faixa] += 1;
            }
        }
        self.limpar();
    }

    fn limpar(&mut self) {
        // `clear` mantém a capacidade: é o que garante zero alocação em regime.
        self.buffer.clear();
        self.condenado = false;
        self.faltou_pacote = false;
        self.tem_idr = false;
        self.dentro_de_fu = false;
        self.carimbo_atual = None;
        self.pacotes_do_quadro = 0;
        self.fim_da_ultima_nal_completa = 0;
        self.fatias_completas = 0;
    }

    /// Antes de matar o quadro em construção, tenta entregar a **cabeça** dele.
    ///
    /// Devolve `true` quando entregou. Ver [`Depacotizador::definir_entrega_de_cabeca`] para o
    /// porquê e para o que falta antes de isto poder ser ligado em produto.
    ///
    /// As condições são todas necessárias e nenhuma é conservadorismo decorativo:
    ///
    /// - **pelo menos uma fatia completa** — meia fatia não é cabeça de nada;
    /// - **o quadro carrega IDR** — a curva de 31/08 diz que quem atravessa o joelho é o IDR
    ///   (p50 dos demais: 8 pacotes), e a cabeça de um quadro P truncado é a única variante que
    ///   esta bancada **não** mediu: o ocultador do `MediaCodec` pinta preto o que falta, e num
    ///   quadro P isso trocaria a imagem antiga por preto. Medir antes de ligar;
    /// - **corte no fim de uma NAL completa**, que é o que `fim_da_ultima_nal_completa` guarda.
    fn tentar_entregar_cabeca(&mut self, entregar: &mut impl FnMut(QuadroRemontado<'_>)) -> bool {
        if !self.entregar_cabeca
            || !self.tem_idr
            || self.fatias_completas == 0
            || self.fim_da_ultima_nal_completa == 0
        {
            return false;
        }
        let Some(carimbo) = self.carimbo_atual else {
            return false;
        };
        let timestamp_us = self.relogio.micros(carimbo);
        self.cabecas_entregues = self.cabecas_entregues.saturating_add(1);
        self.fatias_da_ultima_cabeca = self.fatias_completas;
        entregar(QuadroRemontado {
            annexb: &self.buffer[..self.fim_da_ultima_nal_completa],
            timestamp_us,
            idr: self.tem_idr,
        });
        true
    }

    /// O quadro em construção morreu — mas talvez a cabeça dele sirva.
    ///
    /// A contabilidade **não muda**: um quadro cuja cauda morreu continua sendo
    /// `quadros_descartados` e `idrs_quebrados`, porque foi isso que aconteceu no fio. A cabeça
    /// entregue é contada à parte, em [`Depacotizador::cabecas_entregues`]. Somar as duas coisas
    /// apagaria a distinção que este eixo inteiro existe para medir.
    fn abortar_entregando_a_cabeca(&mut self, entregar: &mut impl FnMut(QuadroRemontado<'_>)) {
        self.tentar_entregar_cabeca(entregar);
        self.abortar();
    }

    fn fechar(&mut self, carimbo: u32, entregar: &mut impl FnMut(QuadroRemontado<'_>)) {
        if self.condenado || self.buffer.is_empty() {
            self.abortar_entregando_a_cabeca(entregar);
            return;
        }
        let timestamp_us = self.relogio.micros(carimbo);
        self.quadros_prontos += 1;
        if self.tem_idr {
            self.idrs_prontos += 1;
        }
        self.maior_quadro_pronto = self.maior_quadro_pronto.max(self.pacotes_do_quadro);
        entregar(QuadroRemontado {
            annexb: &self.buffer,
            timestamp_us,
            idr: self.tem_idr,
        });
        self.limpar();
    }
}

/// Converte microssegundos do relógio da captura em carimbo RTP de 90 kHz (vídeo).
///
/// Trunca de propósito para 32 bits: é o tamanho do campo, e a volta é normal em RTP.
pub fn micros_para_carimbo(micros: u64) -> u32 {
    micros_para_carimbo_em(micros, RELOGIO_VIDEO_HZ)
}

/// O mesmo, na taxa que a track pedir: 90 kHz para vídeo, 48 kHz para Opus, 8 kHz para G.711.
///
/// A fração `hz / 1 000 000` é reduzida pelo maior divisor comum **antes** de multiplicar. Não é
/// esmero: `micros * 48 000` estoura `u64` em ~6 anos de áudio, enquanto `micros * 6 / 125` — a
/// mesma fração reduzida — aguenta milhões. E reduzir antes preserva a precisão que
/// `micros / 1000 * 48` jogaria fora.
pub fn micros_para_carimbo_em(micros: u64, hz: u32) -> u32 {
    const MICROS_POR_SEGUNDO: u64 = 1_000_000;

    const fn mdc(a: u64, b: u64) -> u64 {
        if b == 0 {
            a
        } else {
            mdc(b, a % b)
        }
    }

    let hz = u64::from(hz.max(1));
    let g = mdc(hz, MICROS_POR_SEGUNDO).max(1);
    let numerador = hz / g;
    let denominador = MICROS_POR_SEGUNDO / g;
    ((micros.wrapping_mul(numerador) / denominador) & u64::from(u32::MAX)) as u32
}

/// Um quadro de áudio que chegou, apontando **direto para o pacote RTP**.
///
/// Não há buffer de remontagem: o pacotizador de áudio da libdatachannel não fragmenta, então um
/// pacote RTP é exatamente um quadro codificado. `payload` é uma vista do buffer que a
/// libdatachannel entregou, válida só durante a chamada do tratador — copiar dali é escolha da
/// casca, e no caminho normal ela entrega direto ao decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuadroDeAudio<'a> {
    /// Um quadro codificado inteiro — um pacote Opus, ou 20 ms de G.711 — como veio do fio.
    pub payload: &'a [u8],
    /// Microssegundos desde o primeiro pacote desta track, derivados do carimbo RTP.
    pub timestamp_us: u64,
    /// **O número de sequência RTP, cru.**
    ///
    /// O vídeo esconde este número: o depacotizador o consome para achar buraco e o quadro sai
    /// sem ele. O áudio **precisa** entregá-lo, e é a diferença de contrato mais importante
    /// entre os dois caminhos.
    ///
    /// O motivo é o jitter buffer. Ele é obrigatório no áudio (ver [`Jitter`]), ele mora na
    /// casca — o núcleo não decodifica nem apresenta —, e um jitter buffer que não vê o número
    /// de sequência não consegue fazer as duas coisas que definem um: reordenar o que chegou
    /// trocado e saber *qual* pacote faltou para pedir ao decoder a ocultação de perda certa.
    /// Sem este campo, a casca teria de reimplementar a conta de sequência sobre os carimbos, e
    /// quatro cascas fariam isso de quatro jeitos.
    pub sequencia: u16,
    /// O bit de marca do RTP, **e ele não significa nada aqui**.
    ///
    /// A RFC 3551 §4.1 o reserva para o primeiro pacote depois de um silêncio. O pacotizador de
    /// áudio da libdatachannel o liga em **todo** pacote, porque nunca fragmenta e a conta
    /// `mark = (i == payloads.size() - 1)` dá sempre verdadeiro. Fica exposto para não sumir
    /// informação do fio, com este aviso colado: ninguém deve ler começo de rajada nele.
    pub marca: bool,
}

/// Entrega quadros de áudio a partir de pacotes RTP.
///
/// Um por track. É a contraparte do [`Depacotizador`] de vídeo e é **muito** menor, porque o
/// trabalho difícil de lá não existe aqui:
///
/// | | vídeo (H.264, RFC 6184) | áudio (Opus/G.711) |
/// |---|---|---|
/// | um quadro ocupa | 1 a ~85 pacotes | sempre 1 pacote |
/// | remontagem | FU-A, STAP-A, buffer de 256 KiB | nenhuma |
/// | fim do quadro | bit de marca | o próprio pacote |
/// | perda de 1 pacote | derruba o quadro inteiro | perde 20 ms, e só |
/// | recuperação | IDR pedido por PLI | **não existe** |
///
/// A última linha é a que manda no desenho. Não há quadro-chave de áudio: todo pacote de Opus é
/// independente e o que passou, passou. Por isso o núcleo não descarta nada aqui — ele entrega
/// tudo o que chegou, na ordem em que chegou, com o número de sequência à vista, e deixa a
/// política de reprodução para quem tem o relógio do DAC na mão.
#[derive(Debug)]
pub struct DepacotizadorDeAudio {
    sequencia: ContaDeSequencia,
    relogio: RelogioRtp,
    jitter: Jitter,
    quadros_prontos: u64,
    rtcp_ignorados: u64,
}

impl DepacotizadorDeAudio {
    /// `relogio_hz` é o da track: [`RELOGIO_OPUS_HZ`] ou [`RELOGIO_PCMU_HZ`].
    pub fn new(relogio_hz: u32) -> Self {
        DepacotizadorDeAudio {
            sequencia: ContaDeSequencia::default(),
            relogio: RelogioRtp::novo(relogio_hz),
            jitter: Jitter::default(),
            quadros_prontos: 0,
            rtcp_ignorados: 0,
        }
    }

    /// O carimbo RTP cru que virou a origem do `timestamp_us` entregue: o do primeiro pacote
    /// aceito. `None` antes dele.
    pub fn base_do_relogio(&self) -> Option<u32> {
        self.relogio.base()
    }

    /// A base publicada num atômico: legível **sem** o cadeado de quem guarda este depacotizador.
    /// Ver [`BasePublicada`].
    pub fn base_publicada(&self) -> BasePublicada {
        self.relogio.publicada.clone()
    }

    /// Consome um pacote RTP e entrega o quadro que ele carrega.
    ///
    /// `agora_us` é o relógio monotônico local **na chegada do pacote**, e existe por um motivo
    /// só: sem ele não há jitter. É parâmetro em vez de um `Instant::now()` aqui dentro para que
    /// o teste possa injetar uma chegada e conferir o número contra a conta da RFC — com o
    /// relógio real, a única asserção possível seria "é maior que zero".
    ///
    /// Diferente do vídeo, **nunca descarta**. Um buraco na sequência é contado e o pacote
    /// seguinte é entregue assim mesmo: não há quadro em construção para condenar, e segurar
    /// áudio bom porque o anterior caiu só transformaria um estalo em dois.
    pub fn aceitar(
        &mut self,
        bytes: &[u8],
        agora_us: u64,
        mut entregar: impl FnMut(QuadroDeAudio<'_>),
    ) -> Result<()> {
        let pacote = PacoteRtp::parse(bytes)?;
        if pacote.e_rtcp_multiplexado() {
            // Sai antes de tocar na sequência, pelo mesmo motivo do vídeo: o "número de
            // sequência" de um RTCP é o campo de comprimento dele, e deixá-lo entrar na conta
            // inventaria milhares de pacotes perdidos.
            self.rtcp_ignorados += 1;
            return Ok(());
        }
        if pacote.payload.is_empty() {
            // Um pacote de Opus tem no mínimo o byte de TOC; 20 ms de G.711 têm 160 bytes.
            // Payload vazio é pacote malformado, não silêncio — silêncio, no Opus, é um quadro
            // curto, e com DTX é a ausência de pacote.
            return Err(Error::Protocol("pacote RTP de áudio sem payload".into()));
        }

        self.sequencia.conferir(pacote.sequencia);

        let carimbo_ticks = self.relogio.ticks(pacote.carimbo);
        // A chegada precisa estar na mesma escala do carimbo para a conta da RFC 3550 fechar.
        let chegada_ticks = u64::from(self.relogio.hz).saturating_mul(agora_us) / 1_000_000;
        self.jitter.amostrar(chegada_ticks, carimbo_ticks);

        self.quadros_prontos += 1;
        entregar(QuadroDeAudio {
            payload: pacote.payload,
            timestamp_us: ticks_para_micros(carimbo_ticks, self.relogio.hz),
            sequencia: pacote.sequencia,
            marca: pacote.marca,
        });
        Ok(())
    }

    /// **Todos os contadores num instante só.** Ver [`Contadores`] — e a dívida 26, que é por que
    /// esta função existe em vez de sete acessores.
    ///
    /// `quadros_descartados` é sempre **0**, e isso não é um contador esquecido: no áudio um
    /// pacote perdido *é* um quadro perdido, e ele já está em `pacotes_faltando`. Não existe
    /// remontagem que possa ser abortada, então não há nada que este número pudesse contar sem
    /// contar duas vezes o que o outro já conta.
    pub fn contadores(&self) -> Contadores {
        Contadores {
            quadros_prontos: self.quadros_prontos,
            quadros_descartados: 0,
            // Os quatro de vídeo ficam em zero aqui pela mesma razão que `quadros_descartados`:
            // não existe IDR nem remontagem em áudio, e um número inventado seria pior que
            // nenhum. Ver a dívida 26.
            idrs_prontos: 0,
            idrs_quebrados: 0,
            maior_quadro_pronto_pacotes: 0,
            maior_quebrado_pacotes_recebidos: 0,
            cortes_por_faixa: [0; FAIXAS_DE_CORTE],
            cortes_de_idr_por_faixa: [0; FAIXAS_DE_CORTE],
            // **Zero aqui não é contador esquecido.** O áudio não tem fila de reordenação no
            // núcleo: ele tem jitter buffer, obrigatório, e ele mora na casca — o núcleo não
            // decodifica nem apresenta. Quem ordena áudio é o `BufferDeJitter`, e o número dele
            // é outro.
            reordenacoes_absorvidas: 0,
            desistencias_de_reordenacao: 0,
            profundidade_de_reordenacao: 0,
            ajustes_de_reordenacao: 0,
            pacotes_faltando: self.sequencia.faltando,
            eventos_fora_de_ordem: self.sequencia.fora_de_ordem,
            pacotes_vistos: self.sequencia.vistos,
            pacotes_perdidos_de_verdade: self.sequencia.perda_exata(),
            pacotes_tarde_demais: self.sequencia.tarde_demais,
            rtcp_ignorados: self.rtcp_ignorados,
            jitter_us: self
                .jitter
                .ticks()
                .map(|t| ticks_para_micros(t, self.relogio.hz).min(u64::from(u32::MAX)) as u32),
        }
    }

    /// Quadros de áudio entregues.
    pub fn quadros_prontos(&self) -> u64 {
        self.quadros_prontos
    }

    /// A taxa do relógio RTP desta track, em Hz.
    pub fn relogio_hz(&self) -> u32 {
        self.relogio.hz
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Um depacotizador **com a fila de reordenação desligada**.
    ///
    /// Os testes desta seção foram escritos contra o contrato de fluxo — buraco de sequência
    /// condena o quadro no ato — e é esse contrato que eles guardam. A fila de reordenação, que
    /// chegou em 01/09/2026, é um estágio **na frente** dessa máquina e tem os seus próprios
    /// testes em [`testes_da_fila`]. Misturar os dois faria cada teste falhar pelo motivo do
    /// outro.
    pub(super) fn sem_fila() -> Depacotizador {
        let mut d = Depacotizador::new();
        d.definir_profundidade_de_reordenacao(0);
        d
    }

    /// Monta um pacote RTP com o payload dado.
    pub(super) fn rtp(seq: u16, carimbo: u32, marca: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(RTP_HEADER_LEN + payload.len());
        p.push(0x80); // versão 2, sem padding, sem extensão, 0 CSRC
        p.push(if marca { 0x80 | 96 } else { 96 });
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&carimbo.to_be_bytes());
        p.extend_from_slice(&0x5155_4131u32.to_be_bytes());
        p.extend_from_slice(payload);
        p
    }

    /// Coleta os quadros que o depacotizador entregar.
    pub(super) fn engolir(
        d: &mut Depacotizador,
        pacote: &[u8],
        saida: &mut Vec<(Vec<u8>, bool, u64)>,
    ) {
        d.aceitar(pacote, |q| {
            saida.push((q.annexb.to_vec(), q.idr, q.timestamp_us))
        })
        .expect("pacote válido");
    }

    #[test]
    fn cabecalho_rtp_sai_como_entrou() {
        let p = rtp(7, 1234, true, &[0x41, 0xaa, 0xbb]);
        let lido = PacoteRtp::parse(&p).expect("parse");
        assert_eq!(lido.sequencia, 7);
        assert_eq!(lido.carimbo, 1234);
        assert_eq!(lido.payload_type, 96);
        assert!(lido.marca);
        assert_eq!(lido.payload, &[0x41, 0xaa, 0xbb]);
    }

    #[test]
    fn rtp_com_csrc_e_extensao_acha_o_payload() {
        let mut p = vec![0x80 | 0x10 | 2]; // extensão + 2 CSRC
        p.push(96);
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&[0xaa; 8]); // dois CSRC
        p.extend_from_slice(&[0xbe, 0xde, 0x00, 0x01]); // extensão de 1 palavra
        p.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        p.extend_from_slice(&[0x41, 0x99]); // payload
        let lido = PacoteRtp::parse(&p).expect("parse");
        assert_eq!(lido.payload, &[0x41, 0x99]);
    }

    #[test]
    fn rtp_com_enchimento_corta_o_enchimento() {
        let mut p = rtp(1, 0, true, &[0x41, 0x99]);
        p[0] |= 0b0010_0000; // liga o bit de padding
        p.extend_from_slice(&[0x00, 0x00, 0x03]); // 3 bytes de enchimento, o último é a conta
        let lido = PacoteRtp::parse(&p).expect("parse");
        assert_eq!(lido.payload, &[0x41, 0x99]);
    }

    #[test]
    fn rtp_de_outra_versao_e_recusado() {
        let mut p = rtp(1, 0, true, &[0x41]);
        p[0] = 0x40; // versão 1
        assert!(PacoteRtp::parse(&p).is_err());
    }

    #[test]
    fn rtp_curto_e_recusado() {
        assert!(PacoteRtp::parse(&[0x80, 96, 0, 1]).is_err());
    }

    #[test]
    fn nal_unica_vira_annexb() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 0, true, &[0x41, 1, 2, 3]), &mut saida);
        assert_eq!(saida.len(), 1);
        assert_eq!(saida[0].0, vec![0, 0, 0, 1, 0x41, 1, 2, 3]);
        assert!(!saida[0].1, "0x41 é fatia não-IDR");
        assert_eq!(d.quadros_prontos(), 1);
    }

    #[test]
    fn nal_de_idr_marca_o_quadro() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        // 0x65 = F=0, NRI=3, tipo=5 (IDR)
        engolir(&mut d, &rtp(1, 0, true, &[0x65, 9]), &mut saida);
        assert!(saida[0].1, "fatia tipo 5 tinha de marcar IDR");
    }

    #[test]
    fn fu_a_remonta_a_nal_original() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        // NAL original: 0x65 (NRI=3, tipo=5). Indicador FU-A: 0x7c (NRI=3, tipo=28).
        engolir(
            &mut d,
            &rtp(1, 90, false, &[0x7c, 0x85, 0xaa, 0xbb]),
            &mut saida,
        );
        engolir(&mut d, &rtp(2, 90, false, &[0x7c, 0x05, 0xcc]), &mut saida);
        engolir(&mut d, &rtp(3, 90, true, &[0x7c, 0x45, 0xdd]), &mut saida);

        assert_eq!(saida.len(), 1);
        assert_eq!(
            saida[0].0,
            vec![0, 0, 0, 1, 0x65, 0xaa, 0xbb, 0xcc, 0xdd],
            "a NAL remontada tem de ter o cabeçalho original e os fragmentos em ordem"
        );
        assert!(saida[0].1);
    }

    #[test]
    fn stap_a_desagrega_em_varias_nals() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        // STAP-A (0x78) com SPS (0x67) de 2 bytes e PPS (0x68) de 3 bytes.
        let payload = vec![0x78, 0, 2, 0x67, 0x42, 0, 3, 0x68, 0xce, 0x01];
        engolir(&mut d, &rtp(1, 0, true, &payload), &mut saida);
        assert_eq!(
            saida[0].0,
            vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0x01]
        );
    }

    #[test]
    fn quadro_com_sps_pps_e_idr_sai_inteiro_e_marcado() {
        // O caso real: o emissor manda SPS, PPS e IDR como NALs separadas do mesmo quadro.
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 3600, false, &[0x67, 0x42]), &mut saida);
        engolir(&mut d, &rtp(2, 3600, false, &[0x68, 0xce]), &mut saida);
        engolir(&mut d, &rtp(3, 3600, true, &[0x65, 0x88]), &mut saida);

        assert_eq!(saida.len(), 1, "as três NALs são um quadro só");
        assert_eq!(
            saida[0].0,
            vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88]
        );
        assert!(saida[0].1, "o quadro tem fatia IDR");
    }

    #[test]
    fn buraco_na_sequencia_derruba_o_quadro_e_conta() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x85, 0xaa]), &mut saida);
        // Pula a sequência 2.
        engolir(&mut d, &rtp(3, 90, true, &[0x7c, 0x45, 0xdd]), &mut saida);

        assert!(saida.is_empty(), "quadro com buraco não pode ser entregue");
        assert_eq!(d.pacotes_perdidos(), 1);
        assert_eq!(d.quadros_descartados(), 1);
        assert_eq!(d.quadros_prontos(), 0);

        // O corte entra no histograma, na faixa do próprio tamanho e só nela.
        let c = d.contadores();
        assert_eq!(c.cortes_por_faixa.iter().sum::<u32>(), 1);
        // Este quadro **tem** fatia IDR (`0x85` é FU-A com S=1 e tipo 5), então ele entra também
        // no histograma de IDR, e na mesma faixa. A separação dos dois é o que impede quadro P
        // pequeno morto de imitar a assinatura de truncamento — ver o teste logo abaixo.
        assert_eq!(c.cortes_de_idr_por_faixa.iter().sum::<u32>(), 1);
        assert_eq!(
            c.cortes_de_idr_por_faixa[faixa_do_corte(c.maior_quebrado_pacotes_recebidos)],
            1
        );
        assert_eq!(
            c.cortes_por_faixa[faixa_do_corte(c.maior_quebrado_pacotes_recebidos)],
            1
        );
    }

    // ---------------------------------------------------------------------------------------
    // Os contadores de IDR e de tamanho de quadro (31/08/2026)
    //
    // `docs/regras-de-frente.md`: instrumento não aferido contra caso conhecido não é
    // instrumento — **nos dois sentidos**. Os quatro testes abaixo aferem os dois: que o
    // contador acha o IDR quebrado quando ele existe, e que ele **não** o inventa quando a
    // corrida é limpa. O segundo é o que quase nunca se escreve, e é o que pega um contador que
    // sempre incrementa.
    // ---------------------------------------------------------------------------------------

    /// Um IDR de `pacotes` posições, fragmentado em FU-A, começando na sequência `seq`.
    fn idr_fragmentado(seq: u16, carimbo: u32, pacotes: u16) -> Vec<Vec<u8>> {
        (0..pacotes)
            .map(|i| {
                let cabecalho_fu = match i {
                    0 => 0x80 | nal::IDR,                     // S
                    n if n == pacotes - 1 => 0x40 | nal::IDR, // E
                    _ => nal::IDR,
                };
                rtp(
                    seq + i,
                    carimbo,
                    i == pacotes - 1,
                    &[0x7c, cabecalho_fu, 0xaa, 0xbb],
                )
            })
            .collect()
    }

    #[test]
    fn idr_inteiro_conta_como_pronto_e_mede_o_tamanho() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        for p in idr_fragmentado(1, 90, 12) {
            engolir(&mut d, &p, &mut saida);
        }
        assert_eq!(saida.len(), 1);
        assert!(saida[0].1, "o quadro é IDR");
        assert_eq!(d.idrs_prontos(), 1);
        assert_eq!(d.idrs_quebrados(), 0);
        assert_eq!(d.maior_quadro_pronto_pacotes(), 12);
        assert_eq!(d.maior_quebrado_pacotes_recebidos(), 0);
    }

    #[test]
    fn idr_truncado_na_cauda_conta_o_ponto_de_corte() {
        // A forma exata que a curva de 31/08 mediu: os cinco primeiros pacotes de um IDR de 12
        // chegam e os sete de trás — inclusive o da marca — somem. É `5+7-` no instrumento, e o
        // número que o receptor pode afirmar é **5**: onde o enlace cortou.
        let mut d = sem_fila();
        let mut saida = Vec::new();
        for p in idr_fragmentado(1, 90, 12).into_iter().take(5) {
            engolir(&mut d, &p, &mut saida);
        }
        // O quadro seguinte, com o carimbo novo, é quem mata o anterior.
        engolir(&mut d, &rtp(13, 180, true, &[0x41, 0x11]), &mut saida);

        assert_eq!(d.idrs_quebrados(), 1, "o IDR truncado tem de aparecer");
        assert_eq!(d.idrs_prontos(), 0);
        assert_eq!(
            d.maior_quebrado_pacotes_recebidos(),
            5,
            "o que este lado sabe é quantos chegaram (5), não quantos foram mandados (12)"
        );
        // E o quadro seguinte também morre: a condenação cai nele, por decisão registrada em
        // `aceitar`. É o preço de um IDR truncado, e ele custa DOIS quadros, não um.
        assert!(saida.is_empty());
        assert_eq!(d.quadros_descartados(), 2);
    }

    #[test]
    fn buraco_que_cobre_varios_quadros_nao_infla_o_tamanho() {
        // **A aferição que faltava, e a corrida de bancada que a exigiu.** A primeira versão
        // somava as posições que faltaram ao quadro que morria, para estimar o tamanho mandado.
        // Numa corrida real com origem sintética de 115 pacotes no maior quadro, ela imprimiu
        // 296 — porque um buraco pode cobrir a cauda de um quadro, vários quadros inteiros e a
        // cabeça do seguinte, e o receptor não tem como separar os três.
        //
        // Aqui o buraco é de 500 posições. Nenhum contador de tamanho pode passar do que chegou.
        let mut d = sem_fila();
        let mut saida = Vec::new();
        for p in idr_fragmentado(1, 90, 4).into_iter().take(3) {
            engolir(&mut d, &p, &mut saida);
        }
        engolir(&mut d, &rtp(504, 180, true, &[0x41, 0x11]), &mut saida);

        assert!(
            d.pacotes_perdidos() >= 500,
            "o buraco continua sendo contado como perda"
        );
        assert_eq!(
            d.maior_quebrado_pacotes_recebidos(),
            3,
            "chegaram 3; um buraco de 500 não pode virar um quadro de 503"
        );
        assert_eq!(d.maior_quadro_pronto_pacotes(), 0);
    }

    #[test]
    fn quadro_comum_que_quebra_nao_vira_idr_quebrado() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x81, 0xaa]), &mut saida);
        engolir(&mut d, &rtp(2, 90, false, &[0x7c, 0x01, 0xbb]), &mut saida);
        engolir(&mut d, &rtp(9, 180, true, &[0x41, 0x11]), &mut saida);

        assert_eq!(d.idrs_quebrados(), 0, "não havia IDR nenhum para quebrar");
        assert_eq!(d.quadros_descartados(), 2);
        assert_eq!(
            d.maior_quebrado_pacotes_recebidos(),
            2,
            "chegaram 2 antes do corte"
        );
    }

    #[test]
    fn corrida_limpa_nao_inventa_idr_quebrado() {
        // A aferição do outro sentido. Trinta quadros perfeitos, tamanhos variados, nada perdido:
        // o contador de quebra tem de ficar em ZERO e o de tamanho tem de acompanhar o maior.
        let mut d = sem_fila();
        let mut saida = Vec::new();
        let mut seq = 1u16;
        for n in 0..30u32 {
            let pacotes = 3 + (n as u16 % 8) * 5; // 3, 8, 13, ... 38
            for p in idr_fragmentado(seq, 90 * (n + 1), pacotes) {
                engolir(&mut d, &p, &mut saida);
            }
            seq += pacotes;
        }
        assert_eq!(saida.len(), 30);
        assert_eq!(d.idrs_prontos(), 30);
        assert_eq!(d.idrs_quebrados(), 0);
        assert_eq!(d.quadros_descartados(), 0);
        assert_eq!(d.maior_quadro_pronto_pacotes(), 38);
        assert_eq!(d.maior_quebrado_pacotes_recebidos(), 0);
    }

    #[test]
    fn depois_de_um_buraco_o_quadro_seguinte_sai() {
        // O que garante que a perda é um soluço e não o fim da track.
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x85, 0xaa]), &mut saida);
        engolir(&mut d, &rtp(3, 90, true, &[0x7c, 0x45, 0xdd]), &mut saida);
        engolir(&mut d, &rtp(4, 180, true, &[0x65, 0x11]), &mut saida);

        assert_eq!(saida.len(), 1);
        assert_eq!(saida[0].0, vec![0, 0, 0, 1, 0x65, 0x11]);
        assert_eq!(d.quadros_prontos(), 1);
    }

    #[test]
    fn carimbo_perdido_no_meio_derruba_o_quadro_anterior() {
        // Sem o pacote de marca, o carimbo muda e o quadro anterior nunca fecha.
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x41, 0xaa]), &mut saida);
        engolir(&mut d, &rtp(2, 180, true, &[0x41, 0xbb]), &mut saida);

        assert_eq!(saida.len(), 1, "só o segundo quadro fecha");
        assert_eq!(saida[0].0, vec![0, 0, 0, 1, 0x41, 0xbb]);
        assert_eq!(d.quadros_descartados(), 1);
    }

    /// Buraco exatamente na troca de quadro: o quadro **novo** também tem de morrer.
    ///
    /// O pacote perdido pode ter sido do quadro velho ou do novo, e não há como saber. Numa
    /// versão anterior deste módulo a condenação era marcada antes do `abortar`, que a apagava
    /// ao limpar o buffer — e o quadro novo saía com um pedaço faltando, sem nenhum contador
    /// acusando. Um quadro furado num decoder vira imagem quebrada que dura até o próximo IDR.
    #[test]
    fn buraco_na_troca_de_quadro_condena_o_quadro_novo() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        // Quadro 1: dois pacotes, sem marca no segundo (a marca se perdeu junto).
        engolir(&mut d, &rtp(10, 90, false, &[0x41, 0xaa]), &mut saida);
        // Sequência 11 sumiu; chega a 12 já com o carimbo do quadro seguinte e com a marca.
        engolir(&mut d, &rtp(12, 180, true, &[0x41, 0xbb]), &mut saida);

        assert!(
            saida.is_empty(),
            "o quadro novo saiu apesar do buraco na fronteira: {saida:?}"
        );
        assert_eq!(d.pacotes_perdidos(), 1);
        assert_eq!(d.quadros_prontos(), 0);
    }

    /// **O quadro que perde a cabeça também conta** — e era ele que sujava a tela do iPhone X.
    ///
    /// Um quadro P da tela estendida é uma NAL só, fragmentada em FU-A. Quando a perda leva
    /// exatamente o começo dele (o fragmento com `S`), o resto chega, é condenado pelo buraco e
    /// descartado — sem ter escrito um byte no buffer, porque fragmento sem começo é lixo. Até
    /// 11/09/2026 o `abortar` só contava buffer não vazio, e esse quadro morria **sem subir
    /// `quadros_descartados`**. As cascas pedem IDR por esse número: a perda passava calada, o
    /// decodificador pintava os quadros seguintes sobre a referência que faltou, e a sujeira
    /// ficava até o IDR programado — 7 a 8 s na corrida do iPhone X em que isto foi achado
    /// (`docs/tela-estendida.md`).
    #[test]
    fn quadro_que_perde_a_cabeca_conta_como_descartado() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, true, &[0x41, 0xaa]), &mut saida);
        // Quadro 2: FU-A da sequência 2 à 5, e a 2 (a que tem `S`) se perdeu.
        engolir(&mut d, &rtp(3, 180, false, &[0x5c, 0x01, 0xbb]), &mut saida);
        engolir(&mut d, &rtp(4, 180, false, &[0x5c, 0x01, 0xcc]), &mut saida);
        engolir(&mut d, &rtp(5, 180, true, &[0x5c, 0x41, 0xdd]), &mut saida);
        engolir(&mut d, &rtp(6, 270, true, &[0x41, 0xee]), &mut saida);

        assert_eq!(saida.len(), 2, "saem o 1 e o 3; o 2 não tem cabeça");
        assert_eq!(d.pacotes_perdidos(), 1);
        assert_eq!(
            d.quadros_descartados(),
            1,
            "o quadro 2 morreu e tem de ser contado"
        );
        let c = d.contadores();
        assert_eq!(
            c.cortes_por_faixa.iter().sum::<u32>(),
            1,
            "entra no histograma como todo quadro abortado"
        );
        assert_eq!(
            c.idrs_quebrados, 0,
            "o tipo da NAL viajava no pacote perdido: não há como dizer que era IDR"
        );
    }

    /// A mesma cabeça perdida, levando também a marca: quem mata o quadro sem cabeça é o
    /// carimbo do seguinte, pela outra porta do `abortar`.
    #[test]
    fn quadro_sem_cabeca_e_sem_marca_tambem_conta() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, true, &[0x41, 0xaa]), &mut saida);
        // Quadro 2: FU-A da 2 à 5; somem a 2 (`S`) e a 5 (`E` e marca).
        engolir(&mut d, &rtp(3, 180, false, &[0x5c, 0x01, 0xbb]), &mut saida);
        engolir(&mut d, &rtp(4, 180, false, &[0x5c, 0x01, 0xcc]), &mut saida);
        // O 3 chega depois do buraco da 5 e morre por ele, como qualquer quadro na fronteira.
        engolir(&mut d, &rtp(6, 270, true, &[0x41, 0xee]), &mut saida);

        assert_eq!(saida.len(), 1, "só o 1 sai: {saida:?}");
        assert_eq!(d.quadros_descartados(), 2, "morreram o 2 e o 3");
    }

    #[test]
    fn fragmento_do_meio_sem_comeco_nao_vira_nal_sem_cabecalho() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        // Começa direto num fragmento do meio (S=0, E=0).
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x05, 0xcc]), &mut saida);
        engolir(&mut d, &rtp(2, 90, true, &[0x7c, 0x45, 0xdd]), &mut saida);
        assert!(saida.is_empty());
        // O outro lado de `quadro_que_perde_a_cabeca_conta_como_descartado`: entrar no fluxo
        // pelo meio de um quadro não é perda — nada antes do primeiro pacote é contável.
        assert_eq!(d.quadros_descartados(), 0);
    }

    /// Um Sender Report do RTCP não pode virar bytes de vídeo.
    ///
    /// O segundo byte de um SR é `0xC8`, que lido como RTP diz "marca ligada, payload type 72".
    /// Sem o teste da RFC 5761 ele passaria pelo depacotizador inteiro: o campo de comprimento
    /// viraria número de sequência (inventando perda em massa) e o corpo do relatório seria
    /// costurado dentro do quadro H.264.
    #[test]
    fn rtcp_multiplexado_e_ignorado_e_nao_vira_video() {
        let mut d = sem_fila();
        let mut saida = Vec::new();

        engolir(&mut d, &rtp(1, 90, true, &[0x41, 0xaa]), &mut saida);

        // Sender Report cru: V=2, PT=200, comprimento 6, SSRC.
        let mut sr = vec![0x80u8, 200, 0x00, 0x06];
        sr.extend_from_slice(&0x5155_4131u32.to_be_bytes());
        sr.extend_from_slice(&[0u8; 20]);
        d.aceitar(&sr, |_| panic!("RTCP não pode virar quadro"))
            .expect("ignorado sem erro");

        engolir(&mut d, &rtp(2, 180, true, &[0x41, 0xbb]), &mut saida);

        assert_eq!(d.rtcp_ignorados(), 1);
        assert_eq!(
            d.pacotes_perdidos(),
            0,
            "o RTCP entrou na conta de sequência e inventou perda"
        );
        assert_eq!(saida.len(), 2);
        assert_eq!(saida[1].0, vec![0, 0, 0, 1, 0x41, 0xbb]);
    }

    #[test]
    fn tipo_de_payload_que_o_quall_nao_produz_e_erro() {
        let mut d = sem_fila();
        // FU-B é o tipo 29.
        let erro = d.aceitar(&rtp(1, 0, true, &[29, 0x85, 0xaa]), |_| {});
        assert!(matches!(erro, Err(Error::Protocol(_))));
    }

    #[test]
    fn stap_a_truncado_e_erro_e_nao_panico() {
        let mut d = sem_fila();
        // Anuncia 99 bytes e só há 2.
        let erro = d.aceitar(&rtp(1, 0, true, &[0x78, 0, 99, 0x67, 0x42]), |_| {});
        assert!(erro.is_err());
    }

    #[test]
    fn carimbo_vira_microssegundos_desde_o_primeiro_quadro() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 900_000, true, &[0x41, 1]), &mut saida);
        // 3000 ticks a 90 kHz = 1/30 s = 33 333 µs.
        engolir(&mut d, &rtp(2, 903_000, true, &[0x41, 2]), &mut saida);

        assert_eq!(saida[0].2, 0, "o primeiro quadro é a origem do tempo");
        assert_eq!(saida[1].2, 33_333);
    }

    #[test]
    fn micros_e_carimbo_sao_inversos_na_pratica() {
        for micros in [0u64, 33_333, 1_000_000, 16_666_666] {
            let carimbo = micros_para_carimbo(micros);
            let voltou = u64::from(carimbo) * 1_000_000 / u64::from(RELOGIO_VIDEO_HZ);
            let erro = voltou.abs_diff(micros);
            assert!(
                erro <= 12,
                "ida e volta de {micros} µs errou {erro} µs (um tick de 90 kHz são 11,1 µs)"
            );
        }
    }

    #[test]
    fn sequencia_dando_a_volta_nao_e_perda() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(65_535, 90, true, &[0x41, 1]), &mut saida);
        engolir(&mut d, &rtp(0, 180, true, &[0x41, 2]), &mut saida);
        assert_eq!(
            d.pacotes_perdidos(),
            0,
            "65535 -> 0 é o próximo, não um buraco"
        );
        assert_eq!(saida.len(), 2);
    }

    /// **Perda e troca de ordem deixam de cair no mesmo balde.**
    ///
    /// As duas corridas abaixo dão exatamente o **mesmo** `sequence_anomalies` — 3 e 3 —, e é
    /// por isso que a frente que caçou a anomalia de rádio não conseguiu responder "perdeu ou
    /// reordenou?". Antes deste conserto não havia sequer como perguntar: `pacotes_perdidos` era
    /// o único número, e o `assert_ne!` entre as duas corridas falhava com `3 != 3`.
    #[test]
    fn perda_e_reordenacao_saem_separadas() {
        // Corrida A: 101, 102 e 103 nunca chegaram. Perda de verdade, três pacotes.
        let mut perda = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut perda, &rtp(100, 90, true, &[0x41, 1]), &mut saida);
        engolir(&mut perda, &rtp(104, 180, true, &[0x41, 2]), &mut saida);

        // Corrida B: nada se perdeu; o 101 chegou depois do 102.
        let mut reordem = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut reordem, &rtp(100, 90, true, &[0x41, 1]), &mut saida);
        engolir(&mut reordem, &rtp(102, 180, true, &[0x41, 2]), &mut saida);
        engolir(&mut reordem, &rtp(101, 270, true, &[0x41, 3]), &mut saida);
        engolir(&mut reordem, &rtp(103, 360, true, &[0x41, 4]), &mut saida);

        // O contador antigo continua idêntico nos dois casos: nenhum leitor de
        // `sequence_anomalies` muda de comportamento por causa deste conserto.
        assert_eq!(perda.pacotes_perdidos(), 3);
        assert_eq!(reordem.pacotes_perdidos(), 3);

        // E agora dá para separar.
        assert_eq!(perda.pacotes_faltando(), 3, "três posições nunca chegaram");
        assert_eq!(perda.eventos_fora_de_ordem(), 0, "nada foi reordenado");

        assert_eq!(reordem.eventos_fora_de_ordem(), 1, "uma troca de ordem");
        assert_eq!(
            reordem.pacotes_faltando(),
            2,
            "o atrasado é contado duas vezes: no salto que passou por cima dele e no de volta. \
             É o teto documentado, não a perda — e `eventos_fora_de_ordem > 0` denuncia isso"
        );
    }

    /// A soma continua sendo a soma, **por construção**: não há um terceiro campo que possa
    /// divergir dos dois. Se algum dia divergir, é porque alguém reintroduziu o balde único.
    #[test]
    fn o_contador_antigo_e_exatamente_a_soma_dos_dois_novos() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        for (seq, carimbo) in [(10u16, 90u32), (14, 180), (13, 270), (13, 360), (20, 450)] {
            engolir(&mut d, &rtp(seq, carimbo, true, &[0x41, 1]), &mut saida);
        }
        let c = d.contadores();
        assert_eq!(
            d.pacotes_perdidos(),
            c.pacotes_faltando + c.eventos_fora_de_ordem
        );
        assert_eq!(d.pacotes_perdidos(), c.pacotes_perdidos());
    }

    /// **A reordenação inflava a perda, e agora existe um número que não infla.**
    ///
    /// Os pacotes chegam 10, 30, 11..29, 31: nada se perdeu, tudo chegou. `pacotes_faltando`
    /// cobra 19 no salto para a frente e mais 1 no reencontro do 31 — é o teto documentado da
    /// dívida 26. `pacotes_perdidos_de_verdade` cobra **zero**, porque toda posição foi marcada
    /// na janela antes de sair dela.
    ///
    /// Esta é a diferença que reorganizou a leitura de toda a matriz de perda desta bancada em
    /// 29/08: uma corrida com 486 "faltando" tinha no máximo 50 pacotes perdidos de verdade.
    #[test]
    fn reordenacao_nao_e_perda_no_contador_exato() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        let mut ordem = vec![10u16, 30];
        ordem.extend(11..=29);
        ordem.push(31);
        for (i, seq) in ordem.iter().enumerate() {
            engolir(
                &mut d,
                &rtp(*seq, 90 * (i as u32 + 1), true, &[0x41, 1]),
                &mut saida,
            );
        }
        let c = d.contadores();
        assert_eq!(c.pacotes_vistos, 22);
        assert_eq!(c.pacotes_faltando, 20, "o teto continua sendo o teto");
        assert_eq!(c.eventos_fora_de_ordem, 1);
        assert_eq!(
            c.pacotes_perdidos_de_verdade, 0,
            "nada se perdeu: tudo chegou, só fora de ordem"
        );
        assert_eq!(c.pacotes_tarde_demais, 0);
    }

    /// Perda de verdade continua sendo cobrada — a janela não é uma desculpa universal.
    ///
    /// Chegam 10, 30, 11..19 e 21..29 (o 20 nunca chega), depois 200: o salto para 200 arrasta a
    /// janela inteira para fora e a posição 20 é cobrada.
    #[test]
    fn perda_de_verdade_continua_sendo_cobrada() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        let mut ordem = vec![10u16, 30];
        ordem.extend(11..=19);
        ordem.extend(21..=29);
        ordem.push(200);
        for (i, seq) in ordem.iter().enumerate() {
            engolir(
                &mut d,
                &rtp(*seq, 90 * (i as u32 + 1), true, &[0x41, 1]),
                &mut saida,
            );
        }
        let c = d.contadores();
        // O salto para 200 sobe o piso para 200-127 = 73, e cobra as posições 10..72 que
        // ficaram para trás: a 20 (que nunca chegou) e as 42 posições de 31 a 72. As posições
        // 73..199 continuam **dentro** da janela e por isso não entram — é a escolha explicada
        // em `pacotes_perdidos_de_verdade`: no fim de uma sessão elas seriam ambíguas.
        assert_eq!(c.pacotes_perdidos_de_verdade, 43);
        assert_eq!(c.pacotes_tarde_demais, 0);
    }

    /// Um pacote que chega **depois** de a janela ter passado é contado, e o contador diz isso.
    #[test]
    fn atraso_maior_que_a_janela_e_denunciado() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(10, 90, true, &[0x41, 1]), &mut saida);
        // Um salto de 300 posições joga tudo para fora da janela de 128.
        engolir(&mut d, &rtp(310, 180, true, &[0x41, 1]), &mut saida);
        // E agora o pacote 11 chega, tarde demais para ser reconciliado.
        engolir(&mut d, &rtp(11, 270, true, &[0x41, 1]), &mut saida);
        let c = d.contadores();
        assert_eq!(c.pacotes_tarde_demais, 1);
        assert_eq!(
            c.pacotes_perdidos_de_verdade, 172,
            "o piso subiu para 310-127=183 e cobrou 11..182; a 11 está entre elas, e é o \
             contador de tarde demais quem denuncia que ela tinha chegado"
        );
    }

    /// A volta de 65535 para 0 não pode virar perda de 65 mil posições.
    #[test]
    fn a_volta_de_sequencia_nao_inventa_perda_no_contador_exato() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        for (i, seq) in [65534u16, 65535, 0, 1, 2].iter().enumerate() {
            engolir(
                &mut d,
                &rtp(*seq, 90 * (i as u32 + 1), true, &[0x41, 1]),
                &mut saida,
            );
        }
        let c = d.contadores();
        assert_eq!(c.pacotes_perdidos_de_verdade, 0);
        assert_eq!(c.pacotes_faltando, 0);
    }

    /// Pacote repetido é **um evento**, não um pacote faltando. Antes ele engordava o mesmo
    /// balde da perda de verdade.
    #[test]
    fn pacote_repetido_nao_conta_como_pacote_faltando() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(7, 90, true, &[0x41, 1]), &mut saida);
        engolir(&mut d, &rtp(7, 90, true, &[0x41, 1]), &mut saida);

        assert_eq!(d.pacotes_faltando(), 0, "nada faltou: o pacote repetiu");
        assert_eq!(d.eventos_fora_de_ordem(), 1);
        assert_eq!(d.pacotes_perdidos(), 1, "o número antigo não muda");
    }

    /// **A linha de base fica explícita na saída, e ela não é fingimento.**
    ///
    /// O depacotizador entra no meio de um fluxo que já ia na sequência 5000. O que caiu antes
    /// disso é invisível — número de sequência RTP não diz nada sobre o que veio antes do
    /// primeiro que se viu — e `pacotes_vistos` é o que torna isso legível: ele diz de que
    /// tamanho é a janela sobre a qual `pacotes_faltando` fala.
    #[test]
    fn pacotes_vistos_diz_o_tamanho_da_janela_observada() {
        let mut d = sem_fila();
        let mut saida = Vec::new();

        assert_eq!(
            d.pacotes_vistos(),
            0,
            "sem pacote nenhum não dá nem para afirmar que nada se perdeu"
        );

        engolir(&mut d, &rtp(5000, 90, true, &[0x41, 1]), &mut saida);
        assert_eq!(
            d.pacotes_vistos(),
            1,
            "o primeiro conta, e é a linha de base"
        );
        assert_eq!(
            d.pacotes_faltando(),
            0,
            "os 5000 anteriores não podem ser contados, e o contador não pode fingir que sim"
        );

        engolir(&mut d, &rtp(5001, 180, true, &[0x41, 2]), &mut saida);
        engolir(&mut d, &rtp(5004, 270, true, &[0x41, 3]), &mut saida);

        assert_eq!(d.pacotes_vistos(), 3);
        assert_eq!(d.pacotes_faltando(), 2);
        // O invariante que fecha a conta sem denominador emprestado de outra máquina.
        assert_eq!(
            d.pacotes_vistos() + d.pacotes_faltando(),
            5004 - 5000 + 1,
            "vistos + faltando tem de cobrir o intervalo de sequência inteiro"
        );
    }

    /// RTCP que vaza para o caminho do RTP não pode entrar em `pacotes_vistos`: ele nunca foi
    /// mídia, e inflaria o denominador da taxa de perda.
    #[test]
    fn rtcp_ignorado_nao_entra_na_janela_observada() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, true, &[0x41, 0xaa]), &mut saida);

        let mut sr = vec![0x80u8, 200, 0x00, 0x06];
        sr.extend_from_slice(&0x5155_4131u32.to_be_bytes());
        sr.extend_from_slice(&[0u8; 20]);
        d.aceitar(&sr, |_| panic!("RTCP não pode virar quadro"))
            .expect("ignorado sem erro");

        assert_eq!(d.pacotes_vistos(), 1);
        assert_eq!(d.rtcp_ignorados(), 1);
    }

    #[test]
    fn buffer_nao_cresce_em_regime() {
        let mut d = sem_fila();
        let capacidade = d.buffer.capacity();
        for i in 0..2000u16 {
            let carimbo = u32::from(i) * 3000;
            let _ = d.aceitar(&rtp(i, carimbo, true, &[0x41; 900]), |_| {});
        }
        assert_eq!(
            d.buffer.capacity(),
            capacidade,
            "o buffer de remontagem cresceu: haveria alocação por quadro na extension de 50 MB"
        );
        assert_eq!(d.quadros_prontos(), 2000);
    }

    // --------------------------------------------------------------------------------------
    // Entregar a cabeça de um quadro truncado
    //
    // A forma que estes testes reproduzem é a **medida**, não a imaginada:
    // `docs/idr-que-sobrevive.md` mediu 34 de 37 quadros grandes quebrados com a forma `N+M−` —
    // a cabeça chega inteira, a cauda morre inteira, e o pacote da marca morre com ela. Por isso
    // todo teste daqui perde a **cauda** e descobre a morte pelo carimbo do quadro seguinte.
    // --------------------------------------------------------------------------------------

    /// Um IDR de quatro fatias, cada uma numa NAL única, com SPS e PPS na frente.
    ///
    /// Devolve os pacotes na ordem em que o emissor os põe no fio.
    fn idr_de_quatro_fatias(carimbo: u32, seq0: u16) -> Vec<Vec<u8>> {
        vec![
            rtp(seq0, carimbo, false, &[0x67, 0x42, 0xe0, 0x1f]),
            rtp(seq0 + 1, carimbo, false, &[0x68, 0xce, 0x01]),
            rtp(seq0 + 2, carimbo, false, &[0x65, 0x11, 0x11]),
            rtp(seq0 + 3, carimbo, false, &[0x65, 0x22, 0x22]),
            rtp(seq0 + 4, carimbo, false, &[0x65, 0x33, 0x33]),
            rtp(seq0 + 5, carimbo, true, &[0x65, 0x44, 0x44]),
        ]
    }

    #[test]
    fn desligado_o_truncamento_continua_jogando_a_cabeca_fora() {
        // A aferição do braço "antes": sem a chave, o comportamento é o de hoje, byte a byte.
        let mut d = sem_fila();
        let mut saida = Vec::new();
        for p in idr_de_quatro_fatias(3600, 1).iter().take(4) {
            engolir(&mut d, p, &mut saida);
        }
        // A cauda (as duas últimas fatias, com a marca) morreu. O quadro seguinte denuncia.
        engolir(&mut d, &rtp(5, 7200, true, &[0x41, 0x99]), &mut saida);

        assert_eq!(saida.len(), 1, "só o quadro seguinte saiu");
        assert!(!saida[0].1, "e ele não é IDR");
        assert_eq!(d.cabecas_entregues(), 0);
        assert_eq!(d.idrs_quebrados(), 1);
        assert_eq!(d.idrs_prontos(), 0);
    }

    #[test]
    fn ligado_o_truncamento_de_cauda_entrega_a_cabeca_ate_a_ultima_fatia_completa() {
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let mut saida = Vec::new();
        for p in idr_de_quatro_fatias(3600, 1).iter().take(4) {
            engolir(&mut d, p, &mut saida);
        }
        engolir(&mut d, &rtp(5, 7200, true, &[0x41, 0x99]), &mut saida);

        assert_eq!(saida.len(), 2, "a cabeça e o quadro seguinte");
        assert!(
            saida[0].1,
            "a cabeça carrega a fatia IDR e é marcada como IDR"
        );
        assert_eq!(
            saida[0].0,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, // SPS
                0, 0, 0, 1, 0x68, 0xce, 0x01, // PPS
                0, 0, 0, 1, 0x65, 0x11, 0x11, // fatia 1
                0, 0, 0, 1, 0x65, 0x22, 0x22, // fatia 2
            ],
            "a cabeça vai até o fim da última NAL completa, com SPS e PPS junto"
        );
        assert_eq!(d.cabecas_entregues(), 1);
        assert_eq!(d.fatias_da_ultima_cabeca(), 2);

        // **A contabilidade não muda.** Um quadro cuja cauda morreu continua descartado e
        // continua sendo um IDR quebrado: foi isso que aconteceu no fio, e é o número que
        // `docs/medida-universal.md` cruza com o `idrs_sent` do emissor.
        assert_eq!(d.idrs_quebrados(), 1);
        assert_eq!(d.idrs_prontos(), 0);
        assert_eq!(d.quadros_descartados(), 1);
    }

    #[test]
    fn a_cabeca_nunca_carrega_meia_fatia() {
        // A fatia 3 chega pela metade: o primeiro fragmento da FU-A entra e o resto morre. O
        // corte tem de cair **antes** dela.
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 3600, false, &[0x67, 0x42]), &mut saida);
        engolir(&mut d, &rtp(2, 3600, false, &[0x68, 0xce]), &mut saida);
        engolir(
            &mut d,
            &rtp(3, 3600, false, &[0x65, 0x11, 0x11]),
            &mut saida,
        );
        // FU-A começa (S=1) e nunca termina.
        engolir(
            &mut d,
            &rtp(4, 3600, false, &[0x7c, 0x85, 0xaa]),
            &mut saida,
        );
        engolir(&mut d, &rtp(5, 7200, true, &[0x41, 0x99]), &mut saida);

        assert_eq!(d.cabecas_entregues(), 1);
        assert_eq!(d.fatias_da_ultima_cabeca(), 1);
        assert_eq!(
            saida[0].0,
            vec![
                0, 0, 0, 1, 0x67, 0x42, //
                0, 0, 0, 1, 0x68, 0xce, //
                0, 0, 0, 1, 0x65, 0x11, 0x11,
            ],
            "o fragmento solto da fatia 3 não pode aparecer na cabeça"
        );
    }

    #[test]
    fn sem_nenhuma_fatia_completa_nao_ha_cabeca() {
        // Só SPS e PPS chegaram. Entregar isso seria entregar um quadro sem imagem nenhuma.
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 3600, false, &[0x67, 0x42]), &mut saida);
        engolir(&mut d, &rtp(2, 3600, false, &[0x68, 0xce]), &mut saida);
        engolir(&mut d, &rtp(3, 7200, true, &[0x41, 0x99]), &mut saida);

        assert_eq!(d.cabecas_entregues(), 0);
        assert_eq!(saida.len(), 1);
    }

    #[test]
    fn quadro_sem_idr_nao_tem_a_cabeca_entregue() {
        // A curva diz que quem atravessa o joelho é o IDR (p50 dos demais: 8 pacotes), e o
        // ocultador do `MediaCodec` pinta **preto** o que falta — num quadro P isso trocaria a
        // imagem antiga por preto. Não medido, logo não entregue.
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 3600, false, &[0x41, 0x11]), &mut saida);
        engolir(&mut d, &rtp(2, 3600, false, &[0x41, 0x22]), &mut saida);
        engolir(&mut d, &rtp(3, 7200, true, &[0x41, 0x99]), &mut saida);

        assert_eq!(d.cabecas_entregues(), 0);
    }

    #[test]
    fn buraco_no_meio_corta_a_cabeca_no_buraco_e_nao_depois_dele() {
        // A outra forma de quebra. As fatias 1 e 2 chegam; a 3 se perde; a 4 chega e fecha o
        // quadro com a marca. A cabeça segura é o **prefixo anterior ao buraco** — pôr a fatia 4
        // ali dentro entregaria uma imagem com um pedaço no lugar errado.
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 3600, false, &[0x67, 0x42]), &mut saida);
        engolir(&mut d, &rtp(2, 3600, false, &[0x68, 0xce]), &mut saida);
        engolir(
            &mut d,
            &rtp(3, 3600, false, &[0x65, 0x11, 0x11]),
            &mut saida,
        );
        engolir(
            &mut d,
            &rtp(4, 3600, false, &[0x65, 0x22, 0x22]),
            &mut saida,
        );
        // A sequência 5 morre; a 6 chega com a marca.
        engolir(&mut d, &rtp(6, 3600, true, &[0x65, 0x44, 0x44]), &mut saida);

        assert_eq!(saida.len(), 1);
        assert_eq!(d.cabecas_entregues(), 1);
        assert_eq!(d.fatias_da_ultima_cabeca(), 2);
        assert_eq!(
            saida[0].0,
            vec![
                0, 0, 0, 1, 0x67, 0x42, //
                0, 0, 0, 1, 0x68, 0xce, //
                0, 0, 0, 1, 0x65, 0x11, 0x11, //
                0, 0, 0, 1, 0x65, 0x22, 0x22,
            ],
            "a fatia que chegou depois do buraco não entra na cabeça"
        );
    }

    #[test]
    fn quadro_inteiro_continua_saindo_inteiro_e_nao_conta_como_cabeca() {
        // A aferição do instrumento: com a chave ligada, o caminho feliz não muda em nada.
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let mut saida = Vec::new();
        for p in idr_de_quatro_fatias(3600, 1) {
            engolir(&mut d, &p, &mut saida);
        }
        assert_eq!(saida.len(), 1);
        assert!(saida[0].1);
        assert_eq!(d.cabecas_entregues(), 0, "quadro inteiro não é cabeça");
        assert_eq!(d.idrs_prontos(), 1);
        assert_eq!(d.quadros_descartados(), 0);
    }

    #[test]
    fn o_quadro_seguinte_a_um_truncamento_e_condenado_e_nao_tem_cabeca() {
        // **Propriedade, não defeito, e ela limita o alcance da entrega de cabeça.** O buraco
        // deixado pela cauda morta só é notado quando chega o primeiro pacote do quadro
        // **seguinte** — e é a esse quadro que a condenação é atribuída, porque não há como saber
        // de qual dos dois eram as posições perdidas (ver o comentário em `aceitar`).
        //
        // Consequência: numa rajada de truncamentos consecutivos, uma cabeça sai a cada **dois**
        // quadros. No regime medido isso não morde — quem atravessa o joelho é o IDR, e ele é um
        // em sessenta —, mas é a diferença entre o que este código faz e o que um leitor
        // apressado suporia que ele faz.
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let mut saida = Vec::new();
        // Primeiro IDR truncado: chega limpo, e a cabeça sai.
        engolir(&mut d, &rtp(1, 3600, false, &[0x65, 0x11]), &mut saida);
        engolir(&mut d, &rtp(2, 3600, false, &[0x65, 0x22]), &mut saida);
        // As posições 3 e 4 morrem. O segundo IDR começa em 5, e leva a culpa do buraco.
        engolir(&mut d, &rtp(5, 7200, false, &[0x65, 0x33]), &mut saida);
        engolir(&mut d, &rtp(6, 7200, false, &[0x65, 0x44]), &mut saida);
        // As posições 7 e 8 morrem. O terceiro quadro começa em 9.
        engolir(&mut d, &rtp(9, 10800, true, &[0x41, 0x55]), &mut saida);

        assert_eq!(d.cabecas_entregues(), 1, "só a cabeça do primeiro IDR");
        assert_eq!(d.idrs_quebrados(), 2, "os dois IDR quebraram no fio");
    }

    #[test]
    fn a_cabeca_nao_faz_o_buffer_crescer() {
        // Mesma exigência de `buffer_nao_cresce_em_regime`: a extension do iOS tem 50 MB, e uma
        // alocação por quadro truncado seria uma alocação por quadro no regime que **mais**
        // trunca.
        //
        // A forma da corrida é a medida: um IDR grande truncado a cada 60 quadros pequenos
        // inteiros, que é a proporção de `docs/idr-que-sobrevive.md` (p50 dos quadros P: 8
        // pacotes; só o IDR atravessa o joelho).
        let mut d = sem_fila();
        d.definir_entrega_de_cabeca(true);
        let capacidade = d.buffer.capacity();
        let mut seq: u16 = 0;
        let mut carimbo: u32 = 0;
        for _ in 0..500 {
            // O IDR: duas fatias chegam, a cauda (duas posições) morre.
            let _ = d.aceitar(&rtp(seq, carimbo, false, &[0x65; 700]), |_| {});
            let _ = d.aceitar(&rtp(seq + 1, carimbo, false, &[0x65; 700]), |_| {});
            seq = seq.wrapping_add(4);
            carimbo = carimbo.wrapping_add(3000);
            // Um quadro P inteiro: ele leva a condenação do buraco e morre, e o seguinte limpa a
            // conta de sequência para o IDR da volta seguinte.
            for _ in 0..2 {
                let _ = d.aceitar(&rtp(seq, carimbo, true, &[0x41; 700]), |_| {});
                seq = seq.wrapping_add(1);
                carimbo = carimbo.wrapping_add(3000);
            }
        }
        assert_eq!(
            d.buffer.capacity(),
            capacidade,
            "o buffer cresceu: haveria alocação por quadro truncado"
        );
        assert_eq!(d.cabecas_entregues(), 500, "uma cabeça por IDR truncado");
        assert_eq!(
            d.quadros_prontos(),
            500,
            "e um dos dois quadros P de cada volta"
        );
    }
}

/// A fila de reordenação, testada sozinha.
///
/// Ela é um estágio **na frente** da máquina de estados, e por isso tem seção própria: os testes
/// de `tests` guardam o contrato de fluxo com a fila desligada, e estes guardam o contrato da
/// fila. O número que a originou está em [`FilaDeReordenacao`].
#[cfg(test)]
mod testes_da_fila {
    use super::tests::{engolir, rtp};
    use super::*;

    /// Um buraco em que o pacote esperado **volta** depois de a fila desistir: a assinatura do
    /// cabo. Devolve a primeira sequência livre depois dele.
    ///
    /// A fila desiste quando alguém chega a `base + profundidade` — distância igual à
    /// profundidade não cabe no anel —, e nesse mesmo passo a linha de base é refixada. Só então
    /// o retardatário aparece, que é a ordem em que a rede de verdade faz isso.
    fn buraco_com_volta(d: &mut Depacotizador, base: u16, carimbo: u32) -> u16 {
        let p = d.profundidade_de_reordenacao() as u16;
        let mut saida = Vec::new();
        for k in 1..=p {
            engolir(
                d,
                &rtp(base.wrapping_add(k), carimbo, false, &[0x41, 0xaa]),
                &mut saida,
            );
        }
        engolir(d, &rtp(base, carimbo, false, &[0x41, 0xbb]), &mut saida);
        base.wrapping_add(p + 1)
    }

    /// O quadro que perde a cabeça conta também quando quem acha o buraco é a fila, ao desistir.
    ///
    /// É o caminho do produto — a fila nasce ligada —, e o salto que o dreno entrega ao
    /// `ContaDeSequencia` tem de marcar o quadro sem cabeça como o caminho sem fila marca. Ver
    /// `tests::quadro_que_perde_a_cabeca_conta_como_descartado`.
    #[test]
    fn quadro_sem_cabeca_conta_quando_a_fila_desiste() {
        let mut d = Depacotizador::new();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, true, &[0x41, 0xaa]), &mut saida);
        // Quadro 2: FU-A da 2 à 5, e a 2 (a que tem `S`) nunca chega.
        engolir(&mut d, &rtp(3, 180, false, &[0x5c, 0x01, 0xbb]), &mut saida);
        engolir(&mut d, &rtp(4, 180, false, &[0x5c, 0x01, 0xcc]), &mut saida);
        engolir(&mut d, &rtp(5, 180, true, &[0x5c, 0x41, 0xdd]), &mut saida);
        // Quadros inteiros até a fila desistir da 2 e soltar tudo em ordem.
        let p = d.profundidade_de_reordenacao() as u16;
        for k in 0..p {
            let carimbo = 270 + 90 * u32::from(k);
            engolir(
                &mut d,
                &rtp(6 + k, carimbo, true, &[0x41, 0xee]),
                &mut saida,
            );
        }

        assert_eq!(d.desistencias_de_reordenacao(), 1);
        assert_eq!(saida.len(), 1 + usize::from(p), "sai tudo menos o quadro 2");
        assert_eq!(d.quadros_descartados(), 1, "e o quadro 2 é contado");
    }

    /// O mesmo buraco, sem volta: o pacote esperado nunca chega. A assinatura do rádio.
    fn buraco_sem_volta(d: &mut Depacotizador, base: u16, carimbo: u32) -> u16 {
        let p = d.profundidade_de_reordenacao() as u16;
        let mut saida = Vec::new();
        for k in 1..=p {
            engolir(
                d,
                &rtp(base.wrapping_add(k), carimbo, false, &[0x41, 0xaa]),
                &mut saida,
            );
        }
        base.wrapping_add(p + 1)
    }

    /// Fixa a linha de base sem exercitar nada.
    fn comecar(d: &mut Depacotizador, base: u16) {
        let mut saida = Vec::new();
        engolir(d, &rtp(base, 90, false, &[0x41, 0x01]), &mut saida);
    }

    /// **O cabo: reordena e não perde.** Dezesseis buracos em que o pacote esperado volta, e o
    /// anel dobra sozinho. É a frente "anel de 32 no cabo" respondida pelo produto em vez de por
    /// uma constante — e ele chega lá em segundos, porque no cabo de 01/09/2026 as desistências
    /// vinham a ~3/s.
    #[test]
    fn o_anel_cresce_quando_o_esperado_volta() {
        let mut d = Depacotizador::new();
        assert_eq!(d.profundidade_de_reordenacao(), PROFUNDIDADE_PADRAO);
        comecar(&mut d, 1000);
        let mut base = 1001u16;
        for i in 0..DESISTENCIAS_POR_DECISAO {
            base = buraco_com_volta(&mut d, base, 90 + i * 3000);
        }
        assert_eq!(
            d.profundidade_de_reordenacao(),
            PROFUNDIDADE_PADRAO * 2,
            "com o esperado voltando, o anel era curto e tinha de dobrar"
        );
        assert_eq!(d.ajustes_de_reordenacao(), 1);
        assert_eq!(
            d.desistencias_de_reordenacao(),
            u64::from(DESISTENCIAS_POR_DECISAO)
        );
    }

    /// **O rádio: perde e não reordena — e mesmo assim o anel NÃO desce abaixo do padrão.**
    ///
    /// Esta é a versão corrigida em 02/09/2026, e ela reprova a primeira. O anel encolhia até 4
    /// aqui, com o argumento de que esperar perda é atraso puro; o A/B no aparelho, no mesmo
    /// enlace e com a mesma perda, mediu o contrário — ver [`PROFUNDIDADE_MINIMA`]. Um enlace que
    /// perde 3,7 % **também reordena**, e o anel raso condena o que o anel de 16 absorvia.
    #[test]
    fn o_anel_nao_desce_abaixo_do_padrao_no_radio() {
        let mut d = Depacotizador::new();
        comecar(&mut d, 1000);
        let mut base = 1001u16;
        let mut carimbo = 90u32;
        for _ in 0..(DESISTENCIAS_POR_DECISAO * 6) {
            base = buraco_sem_volta(&mut d, base, carimbo);
            carimbo = carimbo.wrapping_add(3000);
        }
        assert_eq!(
            d.profundidade_de_reordenacao(),
            PROFUNDIDADE_PADRAO,
            "descer abaixo do padrão foi medido e custa imagem"
        );
        assert_eq!(d.ajustes_de_reordenacao(), 0, "não havia para onde descer");
    }

    /// **O que continua existindo é a volta.** Um anel que cresceu no cabo tem de voltar ao padrão
    /// quando o aparelho troca para o rádio — o que ele não faz mais é passar do padrão para
    /// baixo.
    #[test]
    fn o_anel_que_cresceu_volta_ao_padrao_quando_o_regime_muda() {
        let mut d = Depacotizador::new();
        comecar(&mut d, 1000);
        let mut base = 1001u16;
        let mut carimbo = 90u32;
        for _ in 0..(DESISTENCIAS_POR_DECISAO * 4) {
            base = buraco_com_volta(&mut d, base, carimbo);
            carimbo = carimbo.wrapping_add(3000);
        }
        let no_cabo = d.profundidade_de_reordenacao();
        assert!(
            no_cabo > PROFUNDIDADE_PADRAO,
            "o cabo tinha de ter feito o anel crescer"
        );

        for _ in 0..(DESISTENCIAS_POR_DECISAO * 6) {
            base = buraco_sem_volta(&mut d, base, carimbo);
            carimbo = carimbo.wrapping_add(3000);
        }
        assert_eq!(
            d.profundidade_de_reordenacao(),
            PROFUNDIDADE_PADRAO,
            "voltou ao padrão, e parou nele"
        );
    }

    /// Os dois limites, e eles existem para que o controlador não vire outra coisa: para cima,
    /// um jitter buffer que a decisão registrada recusa; para baixo, a fila desligada, que
    /// devolveria o defeito de 01/09 inteiro.
    #[test]
    fn o_anel_para_nos_dois_limites() {
        let mut subindo = Depacotizador::new();
        comecar(&mut subindo, 1000);
        let mut base = 1001u16;
        let mut carimbo = 90u32;
        for _ in 0..(DESISTENCIAS_POR_DECISAO * 10) {
            base = buraco_com_volta(&mut subindo, base, carimbo);
            carimbo = carimbo.wrapping_add(3000);
        }
        assert_eq!(subindo.profundidade_de_reordenacao(), PROFUNDIDADE_MAXIMA);

        let mut descendo = Depacotizador::new();
        comecar(&mut descendo, 1000);
        let mut base = 1001u16;
        let mut carimbo = 90u32;
        for _ in 0..(DESISTENCIAS_POR_DECISAO * 10) {
            base = buraco_sem_volta(&mut descendo, base, carimbo);
            carimbo = carimbo.wrapping_add(3000);
        }
        assert_eq!(descendo.profundidade_de_reordenacao(), PROFUNDIDADE_MINIMA);
    }

    /// **O botão da bancada crava.** Quem pede 16 para medir 16 tem de medir 16 — um anel que
    /// fugisse do valor pedido no meio da corrida tornaria o braço de aferição ilegível.
    #[test]
    fn cravar_a_profundidade_desliga_o_ajuste() {
        let mut d = Depacotizador::new();
        d.definir_profundidade_de_reordenacao(PROFUNDIDADE_PADRAO);
        comecar(&mut d, 1000);
        let mut base = 1001u16;
        let mut carimbo = 90u32;
        for _ in 0..(DESISTENCIAS_POR_DECISAO * 4) {
            base = buraco_com_volta(&mut d, base, carimbo);
            carimbo = carimbo.wrapping_add(3000);
        }
        assert_eq!(d.profundidade_de_reordenacao(), PROFUNDIDADE_PADRAO);
        assert_eq!(d.ajustes_de_reordenacao(), 0, "cravado é cravado");
    }

    /// A banda morta, que é o que impede o ciclo-limite. Entre um oitavo e a metade das
    /// desistências absolvidas o anel **não se mexe** — é a mesma disciplina do `taxa.rs`, e ela
    /// entrou lá depois de uma corrida inteira perdida para a oscilação.
    #[test]
    fn na_banda_morta_o_anel_nao_se_mexe() {
        let mut d = Depacotizador::new();
        comecar(&mut d, 1000);
        let mut base = 1001u16;
        let mut carimbo = 90u32;
        // Um quarto das desistências com volta: acima de 1/8, abaixo de 1/2.
        for i in 0..DESISTENCIAS_POR_DECISAO {
            base = if i % 4 == 0 {
                buraco_com_volta(&mut d, base, carimbo)
            } else {
                buraco_sem_volta(&mut d, base, carimbo)
            };
            carimbo = carimbo.wrapping_add(3000);
        }
        assert_eq!(d.profundidade_de_reordenacao(), PROFUNDIDADE_PADRAO);
        assert_eq!(d.ajustes_de_reordenacao(), 0);
    }

    /// O ajuste **não pode** inventar perda. `packets_lost_for_real` é o número com que o
    /// `taxa.rs` decide, e a primeira versão da fila já mediu perda fantasma uma vez — perda
    /// exata = teto = `reorder_events`, os três idênticos. Redimensionar o anel zera a linha de
    /// base, e é exatamente o tipo de mexida que traria aquilo de volta.
    #[test]
    fn redimensionar_nao_inventa_perda() {
        let mut d = Depacotizador::new();
        comecar(&mut d, 1000);
        let mut base = 1001u16;
        let mut carimbo = 90u32;
        for _ in 0..(DESISTENCIAS_POR_DECISAO * 3) {
            base = buraco_com_volta(&mut d, base, carimbo);
            carimbo = carimbo.wrapping_add(3000);
        }
        assert!(
            d.profundidade_de_reordenacao() > PROFUNDIDADE_PADRAO,
            "o anel tinha de ter crescido"
        );
        assert_eq!(
            d.pacotes_perdidos_de_verdade(),
            0,
            "todo pacote chegou; perda exata diferente de zero seria contabilidade, não rede"
        );
    }

    /// **O caso que a fila existe para consertar.** 1, 3, 2 — o 3 chega adiantado, espera, o 2
    /// fecha o buraco, e os três saem em ordem. Antes de 01/09/2026 isto condenava dois quadros
    /// sem que um único pacote tivesse se perdido.
    #[test]
    fn reordenacao_e_absorvida_e_nao_condena_quadro() {
        let mut d = Depacotizador::new();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x85, 0xaa]), &mut saida);
        engolir(&mut d, &rtp(3, 90, true, &[0x7c, 0x45, 0xcc]), &mut saida);
        assert!(
            saida.is_empty(),
            "o 3 tem de esperar o 2, e não fechar o quadro"
        );
        engolir(&mut d, &rtp(2, 90, false, &[0x7c, 0x05, 0xbb]), &mut saida);

        assert_eq!(saida.len(), 1, "com o buraco fechado o quadro sai inteiro");
        assert_eq!(d.quadros_prontos(), 1);
        assert_eq!(
            d.quadros_descartados(),
            0,
            "nada se perdeu: nada pode ser condenado"
        );
        assert_eq!(d.pacotes_perdidos_de_verdade(), 0);
        // **Um**, e não dois: o 3 esperou no anel, mas o 2 chegou na sequência esperada e foi
        // direto para a máquina de estados. O contador conta quem de fato esperou — que é o que
        // torna ele útil para saber se a fila está sendo exercitada.
        assert_eq!(d.reordenacoes_absorvidas(), 1, "só o 3 passou pelo anel");
    }

    /// O caminho quente não paga nada: quem chega na ordem nunca entra no anel.
    #[test]
    fn na_ordem_nao_custa_nada() {
        let mut d = Depacotizador::new();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x85, 0xaa]), &mut saida);
        engolir(&mut d, &rtp(2, 90, true, &[0x7c, 0x45, 0xbb]), &mut saida);

        assert_eq!(saida.len(), 1);
        assert_eq!(d.reordenacoes_absorvidas(), 0, "sem buraco, sem fila");
    }

    /// **Perda de verdade continua condenando.** O anel enche esperando um pacote que nunca vem,
    /// a fila desiste, o salto chega ao `ContaDeSequencia` e o quadro morre — que é o
    /// comportamento correto para perda e é o que a decisão registrada sempre disse.
    #[test]
    fn perda_de_verdade_enche_o_anel_e_o_quadro_e_condenado() {
        let mut d = Depacotizador::new();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x85, 0xaa]), &mut saida);
        // A sequência 2 nunca chega. Mandam-se `PROFUNDIDADE_PADRAO` pacotes adiante dela.
        for k in 0..PROFUNDIDADE_PADRAO as u16 {
            engolir(
                &mut d,
                &rtp(3 + k, 90, false, &[0x7c, 0x05, 0xdd]),
                &mut saida,
            );
        }
        assert!(d.pacotes_faltando() > 0, "o salto tem de aparecer na conta");
        assert_eq!(d.quadros_prontos(), 0, "quadro com buraco real não sai");
    }

    /// Pacote que chega depois de a fila desistir conta como **evento fora de ordem** — e não
    /// como `tarde_demais`, que tem significado próprio e não pode ser sequestrado.
    #[test]
    fn pacote_atrasado_demais_conta_como_fora_de_ordem_e_nao_como_tarde_demais() {
        let mut d = Depacotizador::new();
        let mut saida = Vec::new();
        engolir(
            &mut d,
            &rtp(100, 90, false, &[0x7c, 0x85, 0xaa]),
            &mut saida,
        );
        // Adiante do anel de 16, mas **bem dentro** da janela de 128: a fila desiste do buraco e
        // refixa a linha de base. É o caso real — a fila desiste depois de 16 posições, não de
        // 400. A primeira versão deste teste usava um salto de 400 e media outra coisa: ali o
        // retardatário cai fora da janela, e `tarde_demais` está **certo** ao contá-lo.
        engolir(
            &mut d,
            &rtp(120, 90, false, &[0x7c, 0x05, 0xbb]),
            &mut saida,
        );
        let fora_antes = d.contadores().eventos_fora_de_ordem;
        let tarde_antes = d.pacotes_tarde_demais();
        // E agora chega o retardatário, que a máquina de estados já não pode aproveitar.
        engolir(
            &mut d,
            &rtp(101, 90, false, &[0x7c, 0x05, 0xcc]),
            &mut saida,
        );

        assert_eq!(d.contadores().eventos_fora_de_ordem, fora_antes + 1);
        assert_eq!(
            d.pacotes_tarde_demais(),
            tarde_antes,
            "`tarde_demais` não é sequestrado"
        );
    }

    /// **Regressão de 01/09/2026.** O pacote que chega depois de a fila desistir foi entregue
    /// tarde, mas foi entregue: ele não pode aparecer como perda exata. A primeira versão da
    /// fila descartava sem marcar na janela, e a bancada mediu 379 perdas que não existiam —
    /// número que o `taxa.rs` usaria para recuar a taxa sem motivo.
    #[test]
    fn retardatario_nao_vira_perda_exata() {
        let mut d = Depacotizador::new();
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(10, 90, false, &[0x7c, 0x85, 0xaa]), &mut saida);
        // Adiante do anel, dentro da janela de 128: é assim que a fila desiste na vida real.
        engolir(&mut d, &rtp(30, 90, false, &[0x7c, 0x05, 0xbb]), &mut saida);
        // O retardatário chega depois. Ele CHEGOU — não pode contar como nunca chegado.
        engolir(&mut d, &rtp(11, 90, false, &[0x7c, 0x05, 0xcc]), &mut saida);

        assert_eq!(
            d.pacotes_perdidos_de_verdade(),
            0,
            "nenhum pacote deixou de chegar: os três foram entregues ao depacotizador"
        );
    }

    /// Profundidade 0 tem de restaurar o comportamento anterior **exatamente** — é o que permite
    /// medir o antes e o depois no mesmo binário.
    #[test]
    fn profundidade_zero_restaura_o_comportamento_de_fluxo() {
        let mut d = Depacotizador::new();
        d.definir_profundidade_de_reordenacao(0);
        let mut saida = Vec::new();
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x85, 0xaa]), &mut saida);
        engolir(&mut d, &rtp(3, 90, true, &[0x7c, 0x45, 0xdd]), &mut saida);

        assert!(saida.is_empty());
        assert_eq!(
            d.quadros_descartados(),
            1,
            "sem fila, o buraco condena no ato"
        );
        assert_eq!(d.reordenacoes_absorvidas(), 0);
    }
}

#[cfg(test)]
mod testes_de_corte_de_idr {
    use super::tests::{engolir, rtp, sem_fila};
    use super::*;

    /// **A separação que decide a §8.24.** Um quadro P que morre entra no histograma geral e
    /// **não** no de IDR — sem isso, um punhado de quadros P pequenos enche as faixas baixas e
    /// imita a assinatura de truncamento que se está procurando.
    #[test]
    fn quadro_p_quebrado_nao_entra_no_histograma_de_idr() {
        let mut d = sem_fila();
        let mut saida = Vec::new();
        // `0x7c` é FU-A; `0x81` tem S=1 e tipo **1** (fatia não-IDR), `0x41` tem E=1 e tipo 1.
        engolir(&mut d, &rtp(1, 90, false, &[0x7c, 0x81, 0xaa]), &mut saida);
        // Pula a sequência 2: o quadro morre com um pacote recebido.
        engolir(&mut d, &rtp(3, 90, true, &[0x7c, 0x41, 0xdd]), &mut saida);

        let c = d.contadores();
        assert_eq!(c.quadros_descartados, 1, "o quadro P morreu");
        assert_eq!(c.cortes_por_faixa.iter().sum::<u32>(), 1, "e entra no geral");
        assert_eq!(
            c.cortes_de_idr_por_faixa.iter().sum::<u32>(),
            0,
            "mas não no de IDR — é essa a separação"
        );
        assert_eq!(c.idrs_quebrados, 0);
    }
}

#[cfg(test)]
mod testes_de_faixa {
    use super::*;

    /// As bordas, que é onde histograma erra: o último balde tem de **saturar** em vez de estourar
    /// o índice, e um corte de zero pacotes tem de cair na primeira faixa em vez de sumir.
    #[test]
    fn faixa_do_corte_satura_e_nao_estoura() {
        assert_eq!(faixa_do_corte(0), 0);
        assert_eq!(faixa_do_corte(9), 0);
        assert_eq!(faixa_do_corte(10), 1);
        assert_eq!(faixa_do_corte(46), 4);
        assert_eq!(faixa_do_corte(69), 6);
        assert_eq!(faixa_do_corte(199), 19);
        assert_eq!(faixa_do_corte(200), FAIXAS_DE_CORTE - 1);
        assert_eq!(faixa_do_corte(u32::MAX), FAIXAS_DE_CORTE - 1);
    }
}

/// O desenrolar do carimbo com reordenação na volta de 32 bits — os dois cenários das críticas
/// da revisão de `docs/som-no-receptor.md` (18/09/2026), que com o código anterior faziam a linha
/// do tempo pular 2³² tiques e não voltar.
#[cfg(test)]
mod testes_do_desenrolar {
    use super::tests::rtp;
    use super::*;

    /// Chegadas 10, 12 (já depois da volta), 11, 13, 14 — a sequência da crítica 3, no
    /// depacotizador de áudio de verdade.
    #[test]
    fn audio_reordenado_na_volta_do_contador_nao_pula_uma_volta() {
        const PASSO: u32 = 960; // 20 ms a 48 kHz
        // O 11 é o último antes da volta; o 12, o primeiro depois dela.
        let carimbo = |seq: u16| -> u32 {
            let do_11 = u32::MAX - 100;
            do_11.wrapping_add(u32::from(seq).wrapping_sub(11).wrapping_mul(PASSO))
        };
        let mut d = DepacotizadorDeAudio::new(RELOGIO_OPUS_HZ);
        let mut saida = Vec::new();
        for seq in [10u16, 12, 11, 13, 14] {
            d.aceitar(&rtp(seq, carimbo(seq), true, &[0xfc]), 0, |q| {
                saida.push((q.sequencia, q.timestamp_us))
            })
            .expect("pacote válido");
        }
        let esperado: Vec<(u16, u64)> = [10u16, 12, 11, 13, 14]
            .iter()
            .map(|&s| (s, u64::from(s - 10) * 20_000))
            .collect();
        assert_eq!(
            saida, esperado,
            "cada pacote cai no seu lugar, e nenhum 24 h 51 min à frente"
        );
    }

    /// A sequência de seis pacotes da crítica 1: a volta no meio, com o atrasado chegando depois
    /// de dois de depois da volta.
    #[test]
    fn a_linha_do_tempo_nao_anda_uma_volta_com_o_atrasado_depois_de_dois_novos() {
        let mut r = RelogioRtp::novo(RELOGIO_OPUS_HZ);
        // c(0), c(1), c(2) antes da volta; c(3) em diante depois dela.
        let base = u32::MAX - 3 * 960 + 1;
        let c = |i: u32| base.wrapping_add(i * 960);
        // Chegada: 0, 1, 3, 4 (depois da volta), 2 (antes dela), 5.
        let tempos: Vec<u64> = [0u32, 1, 3, 4, 2, 5].iter().map(|&i| r.micros(c(i))).collect();
        assert_eq!(tempos, vec![0, 20_000, 60_000, 80_000, 40_000, 100_000]);
    }

    /// Sem reordenação, a volta continua certa nas duas taxas.
    #[test]
    fn a_volta_em_ordem_continua_certa() {
        for hz in [RELOGIO_VIDEO_HZ, RELOGIO_OPUS_HZ] {
            let mut r = RelogioRtp::novo(hz);
            let passo = hz / 50;
            let base = u32::MAX - passo / 2;
            let tempos: Vec<u64> = (0..4u32)
                .map(|i| r.micros(base.wrapping_add(i * passo)))
                .collect();
            assert_eq!(tempos, vec![0, 20_000, 40_000, 60_000], "{hz} Hz");
            assert_eq!(r.base(), Some(base));
        }
    }

    /// Um pacote anterior à base (reordenado logo no começo) dá 0, como antes.
    #[test]
    fn carimbo_anterior_a_base_da_zero() {
        let mut r = RelogioRtp::novo(RELOGIO_OPUS_HZ);
        assert_eq!(r.micros(10_000), 0);
        assert_eq!(r.micros(10_000 - 960), 0);
        assert_eq!(r.micros(10_000 + 960), 20_000);
    }

    /// A base do vídeo é o primeiro quadro **entregue**: um FU-A do meio de um quadro que morre
    /// não fixa nada. É o achado 9 da crítica 3, e a razão de o relógio da sessão perguntar a
    /// base ao depacotizador em vez de supor.
    #[test]
    fn a_base_do_video_e_o_primeiro_quadro_entregue() {
        let mut d = super::tests::sem_fila();
        let fu_do_meio = [0x7c, 0x05, 0xaa, 0xbb];
        let _ = d.aceitar(&rtp(1, 3000, false, &fu_do_meio), |_| {});
        assert_eq!(d.base_do_relogio(), None, "nada foi entregue ainda");
        let mut idr = vec![0x65, 0x88];
        idr.extend_from_slice(&[0x11u8; 8]);
        let mut entregues = Vec::new();
        let _ = d.aceitar(&rtp(2, 6000, true, &idr), |q| entregues.push(q.timestamp_us));
        assert_eq!(entregues, vec![0]);
        assert_eq!(d.base_do_relogio(), Some(6000));
    }

    /// O desenrolador em si: `onde_cai` não mexe na referência.
    #[test]
    fn onde_cai_nao_avanca_o_maior() {
        let mut d = Desenrolador::default();
        assert_eq!(d.onde_cai(5), None);
        assert_eq!(d.desenrolar(u32::MAX - 1), i64::from(u32::MAX - 1));
        assert_eq!(d.onde_cai(3), Some(i64::from(u32::MAX) + 4));
        assert_eq!(d.desenrolar(u32::MAX - 3), i64::from(u32::MAX - 3));
        assert_eq!(d.desenrolar(2), i64::from(u32::MAX) + 3);
    }
}
