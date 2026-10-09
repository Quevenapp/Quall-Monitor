//! O jitter buffer de áudio.
//!
//! # Por que ele existe, e por que só agora
//!
//! O vídeo do Quall não tem jitter buffer **por decisão registrada**: buraco na sequência derruba
//! o quadro em construção, a casca pede IDR, e a imagem volta em um quadro. Áudio não tem essa
//! saída — não existe quadro-chave de áudio, e o sumidouro é um DAC que consome exatamente 48 000
//! amostras por segundo para sempre. Entregar pacote a pacote direto ao DAC produz um estalo a
//! cada pacote atrasado.
//!
//! A `§4` do `docs/audio.md` dizia que este buffer é obrigatório e que **mora na casca**. A
//! primeira metade continua verdadeira; a segunda foi **revogada aqui**, e o argumento está em
//! [`BufferDeJitter`].
//!
//! # O que ele decide, e o que ele não decide
//!
//! Este módulo é **cego ao codec**. Ele não decodifica, não sabe o que é LBRR e não sabe o que é
//! um quadro de Opus — o núcleo não decodifica, e essa fronteira não se move. O que ele faz é
//! transformar um fluxo de pacotes que chegam trocados, repetidos, atrasados ou não chegam numa
//! **sequência de ordens em ordem de reprodução**, uma por slot de 20 ms, sem buraco: ver
//! [`Entrega`].
//!
//! Quem executa a ordem é quem tem o decodificador na mão. `Entrega::Fec` é uma **oferta** — o
//! buffer diz *"o slot N não chegou e eu tenho o pacote N+1 na mão"* —, e só a casca sabe se
//! aquele pacote de fato carrega a cópia de socorro. Ver [`Entrega::Fec`].

use std::collections::VecDeque;

use crate::rtp::QuadroDeAudio;

/// Comparação de números de sequência RTP na aritmética serial da RFC 1982.
///
/// `a` está **depois** de `b` se a distância de `b` para `a`, com a volta de 65535 para 0, cai na
/// primeira metade do círculo. É a mesma conta que [`crate::rtp`] já faz com `wrapping_sub`, e
/// está aqui como função nomeada porque este módulo a usa em cinco lugares e uma inversão de
/// sinal em qualquer um deles é um defeito mudo.
pub(crate) fn depois_de(a: u16, b: u16) -> bool {
    let d = a.wrapping_sub(b);
    d != 0 && d < 0x8000
}

/// A distância de `b` até `a` na ordem serial, ou `None` se `a` está antes de `b`.
pub(crate) fn distancia(a: u16, b: u16) -> Option<u16> {
    let d = a.wrapping_sub(b);
    (d < 0x8000).then_some(d)
}

/// O que o buffer manda fazer com um slot de reprodução.
///
/// **Sempre em ordem, sempre um por slot, nunca um buraco.** É a diferença entre este tipo e o
/// [`QuadroDeAudio`] que entra: aquele é o que o fio entregou, este é o que o DAC precisa
/// consumir. Um DAC não aceita "pulei este aqui" — ele vai consumir 20 ms de alguma coisa, e a
/// única escolha é *de qual coisa*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entrega<'a> {
    /// O pacote chegou. Decodifique normalmente.
    Quadro {
        payload: &'a [u8],
        sequencia: u16,
        /// Microssegundos desde o primeiro pacote da track, **como veio do carimbo RTP**.
        timestamp_us: u64,
    },

    /// O pacote `sequencia` não chegou, **e o buffer tem o pacote seguinte na mão**.
    ///
    /// No Opus com `useinbandfec=1`, o pacote *N+1* carrega uma cópia de baixa taxa (LBRR) do
    /// quadro *N*. Esta variante é o convite para chamar `opus_decode(..., decode_fec = 1)` sobre
    /// `socorro` — que é o pacote *N+1* inteiro, sem tocar.
    ///
    /// # É uma oferta, não uma garantia, e a diferença é o ponto
    ///
    /// O núcleo não sabe ler LBRR: isso mora atrás do decodificador de faixa do SILK, e quem
    /// responde é `opus_packet_has_lbrr`. O buffer oferece porque a **estrutura** permite (há um
    /// sucessor imediato em mãos); a casca decide porque só ela sabe se o **conteúdo** permite.
    ///
    /// E há uma armadilha do lado de lá: sem LBRR, `opus_decode` com `decode_fec = 1` **cai na
    /// ocultação de perda em silêncio**, devolvendo um sucesso que não é recuperação nenhuma. A
    /// casca que não conferir `tem_lbrr` antes vai contar como "curado por FEC" um quadro que o
    /// decoder inventou. É o mesmo defeito de sempre: uma API que aceita e não faz.
    ///
    /// # Duas perdas seguidas: a primeira não é recuperável
    ///
    /// Se *N* e *N+1* se perderem, o slot *N* não tem sucessor em mãos e vira
    /// [`Entrega::Silencio`]; o slot *N+1* tem *N+2*, e esse sim é recuperável. **O LBRR cobre a
    /// última perda de uma rajada, não a rajada.** Está medido em `docs/audio.md`.
    Fec {
        /// O pacote *N+1*, inteiro. É dele que sai a cópia do quadro *N*.
        socorro: &'a [u8],
        /// O slot que faltou — *N*, e não *N+1*.
        sequencia: u16,
        /// **Interpolado**, e não observado: o carimbo do sucessor menos a duração do quadro.
        /// Não há pacote *N* de onde ler um carimbo.
        timestamp_us: u64,
    },

    /// O pacote não chegou e não há socorro. Chame a ocultação de perda do decoder (PLC).
    ///
    /// Acontece em três casos, e vale distingui-los ao ler os contadores: o codec não tem FEC
    /// (áudio de sistema, em CELT — ver `docs/audio.md` §3); duas perdas seguidas; ou o buffer
    /// desistiu de esperar e o sucessor ainda não tinha chegado.
    Silencio {
        sequencia: u16,
        /// Interpolado, como em [`Entrega::Fec`].
        timestamp_us: u64,
    },
}

impl Entrega<'_> {
    /// O slot a que esta ordem se refere.
    pub fn sequencia(&self) -> u16 {
        match self {
            Entrega::Quadro { sequencia, .. }
            | Entrega::Fec { sequencia, .. }
            | Entrega::Silencio { sequencia, .. } => *sequencia,
        }
    }

    /// Microssegundos desde o primeiro pacote da track.
    pub fn timestamp_us(&self) -> u64 {
        match self {
            Entrega::Quadro { timestamp_us, .. }
            | Entrega::Fec { timestamp_us, .. }
            | Entrega::Silencio { timestamp_us, .. } => *timestamp_us,
        }
    }
}

/// A política do buffer. Todo campo aqui custa latência, memória ou qualidade — nenhum é gosto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Politica {
    /// **Quantos pacotes ficam retidos depois de cada entrega.** A latência que o buffer
    /// acrescenta é `profundidade × duracao_do_quadro_us` — com quadro de 20 ms, a profundidade
    /// 2 custa 40 ms.
    ///
    /// Não é adaptativa, e a recusa está em [`BufferDeJitter`].
    pub profundidade: u16,

    /// A duração de um slot, em microssegundos. 20 000 no Quall — ver `docs/audio.md` §2.
    ///
    /// O buffer conta em **pacotes**, não em tempo, e isso só é legítimo porque a cadência da
    /// origem é fixa: um quadro por pacote, um pacote a cada 20 ms, `usedtx=0`. Com DTX ligado o
    /// carimbo daria saltos longos e a conta por pacotes deixaria de valer.
    pub duracao_do_quadro_us: u32,

    /// A casca **pode** recuperar buraco por FEC? Vem do `useinbandfec` do preset/SDP.
    ///
    /// Com `false` todo buraco vira [`Entrega::Silencio`] e o buffer nem oferece o sucessor. É o
    /// caso do áudio de sistema, que roda em CELT e não tem LBRR nenhum: oferecer ali seria
    /// convidar a casca a chamar `decode_fec` num pacote que vai cair na ocultação de perda em
    /// silêncio, e a contar isso como cura.
    pub fec_disponivel: bool,

    /// Salto de sequência a partir do qual o buffer **ressincroniza** em vez de encher o DAC de
    /// ocultação de perda.
    ///
    /// Um salto de 3 pacotes é perda de rádio. Um salto de 3 000 é outra coisa — emissor que
    /// reiniciou, SSRC reaproveitado, uma pausa de rede longa —, e tratá-lo como perda produziria
    /// 3 000 slots de PLC, ou seja **um minuto** de ruído inventado a 20 ms por slot, com o áudio
    /// bom parado atrás. Acima deste limite o buffer joga fora o que tem, adota a nova sequência
    /// como linha de base e conta uma [`ContadoresDeBuffer::resincronizacoes`].
    pub salto_maximo: u16,
}

impl Politica {
    /// A política do microfone: profundidade 2 e FEC ligado.
    ///
    /// Os 40 ms saíram de medição, e não do jitter da RFC 3550 — ver `docs/audio.md`.
    pub const MICROFONE: Politica = Politica {
        profundidade: 2,
        duracao_do_quadro_us: 20_000,
        fec_disponivel: true,
        salto_maximo: 100,
    };

    /// A política do áudio de sistema: mesma profundidade, **sem FEC**, porque o CELT não tem
    /// LBRR e prometer o que não se entrega é o defeito do M4 num codec diferente.
    pub const AUDIO_DO_SISTEMA: Politica = Politica {
        fec_disponivel: false,
        ..Politica::MICROFONE
    };

    /// A latência que esta política acrescenta, em microssegundos.
    pub fn latencia_us(&self) -> u64 {
        u64::from(self.profundidade) * u64::from(self.duracao_do_quadro_us)
    }
}

/// Os contadores do buffer, no espírito dos de [`crate::rtp::Contadores`]: **uma leitura só, do
/// mesmo instante**, e cada número com uma definição que dá para defender.
///
/// A invariante que os liga:
///
/// ```text
/// slots_entregues = quadros + buracos
/// buracos         = curas_oferecidas + silencios
/// ```
///
/// Um teste a confere, porque foi ela que pegou o primeiro defeito da implementação.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContadoresDeBuffer {
    /// Slots de 20 ms que saíram do buffer, de qualquer espécie. É o que o DAC consumiu.
    pub slots_entregues: u64,
    /// Slots que saíram com um pacote de verdade dentro.
    pub quadros: u64,
    /// Slots cujo pacote nunca chegou a tempo. **É o número que o produto sente.**
    pub buracos: u64,
    /// Buracos para os quais o buffer ofereceu o sucessor ([`Entrega::Fec`]).
    ///
    /// Não é "curados": a cura depende de o sucessor de fato carregar LBRR, e quem sabe disso é a
    /// casca. Ver [`Entrega::Fec`].
    pub curas_oferecidas: u64,
    /// Buracos que viraram ocultação de perda ([`Entrega::Silencio`]).
    pub silencios: u64,
    /// Pacotes que chegaram **depois** de o slot deles já ter sido entregue.
    ///
    /// São jogados fora, e isso não é desperdício: o instante em que aquele áudio deveria ter
    /// tocado já passou. Tocá-lo agora seria trocar um estalo por um estalo e mais 20 ms de
    /// atraso permanente. **Se este número for grande, a profundidade está pequena** — é o
    /// sintoma direto do buffer mal dimensionado.
    pub tarde_demais: u64,
    /// Pacotes com um número de sequência que o buffer já tinha em mãos ou já entregou.
    ///
    /// Contados à parte de [`Self::tarde_demais`]: um duplicado é a rede fazendo eco, não o
    /// buffer sendo curto, e confundir os dois faria alguém aumentar a profundidade à toa.
    pub duplicados: u64,
    /// Pacotes que chegaram fora de ordem **e o buffer salvou** — entraram antes do slot deles
    /// sair.
    ///
    /// É a medida direta do que a profundidade está comprando. Zero aqui com
    /// [`Self::tarde_demais`] zero quer dizer que a rede está entregando em ordem e a
    /// profundidade não está pagando por reordenação — só por jitter.
    pub reordenados: u64,
    /// Quantas vezes a sequência saltou mais que [`Politica::salto_maximo`] e o buffer recomeçou.
    pub resincronizacoes: u64,
    /// Maior ocupação já vista, em pacotes. Serve para conferir que a fila não cresce.
    pub ocupacao_maxima: u16,

    /// **O maior atraso relativo observado, em microssegundos.** Ver [`Self::atraso_por_slots`].
    pub atraso_max_us: u64,

    /// A distribuição do atraso relativo, em faixas de **um slot** de largura.
    ///
    /// # O que é "atraso relativo", e por que não é o jitter da RFC 3550
    ///
    /// Para cada pacote, `trânsito = chegada − carimbo`. O trânsito absoluto não tem sentido (os
    /// dois relógios não têm origem comum), mas a **diferença dele para o menor trânsito
    /// recente** tem: é quanto este pacote chegou atrasado em relação ao pacote mais rápido das
    /// últimas uma ou duas janelas de [`MinimoPorJanela::JANELA_US`]. É essa a quantidade que o
    /// buffer precisa cobrir. **Recente, e não da sessão**, desde 18/09/2026: o mínimo da sessão
    /// não acompanha a deriva entre os relógios — ver [`MinimoPorJanela`].
    ///
    /// **O jitter da RFC 3550 §6.4.1 não serve para dimensionar buffer, e o `docs/audio.md`
    /// usava-o assim.** Aquele número é uma média móvel de `|D|` com filtro de 1/16 — uma medida
    /// de dispersão *típica*. Um buffer não é dimensionado pelo típico: ele é dimensionado pela
    /// **cauda**, porque cada pacote acima da profundidade é um estalo. Um fluxo com jitter de 10
    /// ms e um pico ocasional de 60 ms e um fluxo com jitter de 10 ms e pico de 12 ms dão o mesmo
    /// número na RFC 3550 e precisam de buffers muito diferentes.
    ///
    /// As faixas são: `[0]` = chegou junto com o mais rápido (atraso < 1 slot), `[1]` = atrasou
    /// entre 1 e 2 slots, e assim por diante. A última faixa é aberta: tudo daí para cima cai
    /// nela. **Um pacote na faixa `k` só é aproveitável com profundidade ≥ `k`.**
    pub atraso_por_slots: [u64; 8],
}

impl ContadoresDeBuffer {
    /// A menor profundidade que teria aproveitado **todos** os pacotes observados, em slots.
    ///
    /// Lê o histograma de trás para a frente e devolve a faixa mais alta que recebeu alguma
    /// coisa. É a resposta direta à pergunta *"de que tamanho o buffer precisa ser?"* — e ela
    /// vale para o que **esta corrida** viu, não para a rede em geral.
    pub fn profundidade_necessaria(&self) -> u16 {
        for (i, n) in self.atraso_por_slots.iter().enumerate().rev() {
            if *n > 0 {
                return i as u16;
            }
        }
        0
    }
}

/// O mínimo de uma grandeza **nas últimas janelas**, e não desde o começo da sessão.
///
/// # Por que não o mínimo da sessão inteira
///
/// Era o que o buffer fazia até 18/09/2026, e o mínimo da sessão **só desce**. Com o relógio do
/// emissor um pouco mais lento que o do receptor, o trânsito cresce devagar, e todo pacote novo
/// parece atrasado em relação a um mínimo de uma hora atrás. Medido pela crítica 1 da revisão de
/// `docs/som-no-receptor.md`, com o código real: 50 ppm por 60 min e rede perfeita deram
/// `atraso_max_us = 179 999`, o histograma espalhado pelas 8 faixas e
/// `profundidade_necessaria = 7`, quando o certo era 0.
///
/// Aqui a referência é o menor valor entre a janela atual e a anterior, cada uma de
/// [`MinimoPorJanela::JANELA_US`]. Ela esquece o passado em no máximo duas janelas: a 200 ppm, 20 s
/// de deriva são 4 ms, bem abaixo de um slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct MinimoPorJanela {
    /// `(início da janela, menor valor nela)`.
    atual: Option<(u64, i64)>,
    anterior: Option<i64>,
}

impl MinimoPorJanela {
    /// A duração de uma janela, em microssegundos do relógio de quem observa.
    pub const JANELA_US: u64 = 10_000_000;

    /// Registra `valor` no instante `agora_us` e devolve a referência: o menor valor entre a
    /// janela atual (já com este) e a anterior.
    pub fn observar(&mut self, valor: i64, agora_us: u64) -> i64 {
        match self.atual {
            Some((inicio, minimo)) if agora_us.saturating_sub(inicio) < Self::JANELA_US => {
                self.atual = Some((inicio, minimo.min(valor)));
            }
            Some((_, minimo)) => {
                self.anterior = Some(minimo);
                self.atual = Some((agora_us, valor));
            }
            None => self.atual = Some((agora_us, valor)),
        }
        let atual = self.atual.map_or(valor, |(_, m)| m);
        self.anterior.map_or(atual, |a| a.min(atual))
    }
}

/// Um slot ocupado, com o payload copiado.
#[derive(Debug)]
struct Guardado {
    sequencia: u16,
    timestamp_us: u64,
    payload: Vec<u8>,
}

/// O jitter buffer de áudio: reordena, absorve jitter, e transforma buraco em ordem executável.
///
/// # Ele mudou de lado, e o argumento é este
///
/// A `§4` do `docs/audio.md` dizia que o jitter buffer mora na casca, "pela mesma fronteira que o
/// contrato já desenhou — o núcleo não decodifica e não apresenta". A fronteira está certa e não
/// se move; a conclusão não seguia dela. **Reordenar não é decodificar.** O que este tipo faz é
/// aritmética de números de sequência e de carimbos — exatamente a matéria de [`crate::rtp`], que
/// já mora no núcleo — e o que ele *não* faz continua sendo o que o contrato proíbe: ele não
/// chama decoder, não toca em PCM e não conhece relógio de DAC.
///
/// O custo de deixá-lo na casca era conhecido e está escrito no próprio documento: *"sem esse
/// campo, quatro cascas reimplementariam a conta de sequência sobre os carimbos, de quatro
/// jeitos"*. Isso valia para o número de sequência cru; vale com mais força para a política
/// inteira. E há um custo que não estava escrito: **enquanto ele morasse na casca, a recuperação
/// por FEC não podia ser provada por ninguém** — foi exatamente o que aconteceu, e a §13 registrou
/// como *"a recuperação por FEC nunca foi exercida"*.
///
/// # Profundidade fixa. A adaptativa foi recusada, e não por preguiça
///
/// Um buffer adaptativo de verdade — o NetEq do WebRTC é o exemplar — não muda de profundidade
/// somando ou tirando pacotes. Ele muda **esticando e encolhendo o áudio no tempo**, por WSOLA:
/// tira 3 ms de um trecho estacionário sem mudar o tom, e ninguém ouve. É isso que torna a
/// adaptação transparente.
///
/// Sem modificação de escala de tempo, "adaptar" só tem dois movimentos: **descartar** um pacote
/// para encurtar (um estalo) ou **inserir** silêncio/PLC para alongar (outro estalo). Ou seja: um
/// buffer adaptativo sem WSOLA troca os estalos da rede por estalos próprios, em troca de uma
/// latência média menor. Numa LAN, onde a latência já é folgada contra a meta de 150 ms e a perda
/// é rara, esse é o lado errado do trato.
///
/// WSOLA é processamento de sinal sobre PCM. PCM é o que o núcleo não toca. Então a decisão de
/// produto e a fronteira de arquitetura apontam para o mesmo lugar, e a profundidade é fixa.
///
/// **O que fica aberto e está honesto:** numa rede pior que uma LAN — 4G, Wi-Fi congestionado — a
/// profundidade fixa é a escolha errada, e [`ContadoresDeBuffer::atraso_por_slots`] é justamente
/// o instrumento que denuncia quando ela ficou errada.
///
/// # Memória
///
/// A fila tem capacidade fixa, decidida no construtor, e os `Vec` dos payloads são reciclados por
/// uma lista de livres do mesmo tamanho. **Nada aqui cresce com o tempo de sessão** — é a
/// restrição de 50 MB da Broadcast Upload Extension do iOS aplicada a um tipo que, por natureza,
/// seria uma fila que cresce.
#[derive(Debug)]
pub struct BufferDeJitter {
    politica: Politica,
    capacidade: usize,

    /// Ordenada por sequência serial, ascendente.
    fila: VecDeque<Guardado>,
    /// `Vec` de payload reciclados. Nunca passa de `capacidade` elementos.
    livres: Vec<Vec<u8>>,

    /// O slot que vai sair na próxima entrega. `None` até o primeiro pacote — que **é** a linha de
    /// base, como a dívida 25 documenta: um número de sequência não diz nada sobre o que veio
    /// antes do primeiro que se viu.
    proxima: Option<u16>,

    /// Menor `chegada − carimbo` **recente**, em microssegundos: a referência do atraso relativo.
    /// Ver [`MinimoPorJanela`].
    transito_minimo: MinimoPorJanela,

    contadores: ContadoresDeBuffer,
}

impl BufferDeJitter {
    /// Quantos pacotes a fila pode segurar além da profundidade, para absorver reordenação sem
    /// realocar. Quatro é folga barata: a 1 500 bytes por payload, a fila inteira de uma política
    /// de profundidade 2 são ~9 KB.
    const FOLGA: usize = 4;

    pub fn novo(politica: Politica) -> Self {
        let capacidade = usize::from(politica.profundidade) + Self::FOLGA;
        BufferDeJitter {
            politica,
            capacidade,
            fila: VecDeque::with_capacity(capacidade),
            livres: Vec::with_capacity(capacidade),
            proxima: None,
            transito_minimo: MinimoPorJanela::default(),
            contadores: ContadoresDeBuffer::default(),
        }
    }

    pub fn politica(&self) -> Politica {
        self.politica
    }

    /// Todos os contadores de uma vez, do mesmo instante. Ver [`ContadoresDeBuffer`].
    pub fn contadores(&self) -> ContadoresDeBuffer {
        self.contadores
    }

    /// Quantos pacotes estão retidos agora.
    pub fn ocupacao(&self) -> usize {
        self.fila.len()
    }

    /// O caminho normal: guarda o pacote e entrega o que já pode sair.
    ///
    /// `agora_us` é o relógio monotônico local **na chegada**, e existe pelo mesmo motivo que em
    /// [`crate::rtp::DepacotizadorDeAudio::aceitar`]: sem ele não há medida de atraso, e é
    /// parâmetro em vez de um `Instant::now()` aqui dentro para que o teste possa injetar uma
    /// chegada e conferir o número.
    pub fn aceitar(
        &mut self,
        quadro: &QuadroDeAudio<'_>,
        agora_us: u64,
        entregar: impl FnMut(Entrega<'_>),
    ) {
        self.inserir(quadro, agora_us);
        self.escoar(entregar);
    }

    /// Guarda o pacote sem entregar nada. Separado de [`Self::escoar`] para a casca que puxa pelo
    /// relógio do DAC em vez de pela chegada.
    pub fn inserir(&mut self, quadro: &QuadroDeAudio<'_>, agora_us: u64) {
        self.medir_atraso(quadro.timestamp_us, agora_us);

        let seq = quadro.sequencia;

        match self.proxima {
            None => {
                // O primeiro pacote é a linha de base. Nada que caiu antes dele pode ser contado
                // — dívida 25.
                self.proxima = Some(seq);
            }
            Some(proxima) => {
                if !depois_de(seq, proxima) && seq != proxima {
                    // Antes do slot que vai sair: o instante dele já passou.
                    self.contadores.tarde_demais += 1;
                    return;
                }
                if self.fila.iter().any(|g| g.sequencia == seq) {
                    self.contadores.duplicados += 1;
                    return;
                }
                if let Some(d) = distancia(seq, proxima) {
                    if d > self.politica.salto_maximo {
                        self.ressincronizar(seq);
                    }
                }
            }
        }

        // Um `Vec` reciclado, ou um novo se a lista de livres estiver vazia.
        let mut payload = self.livres.pop().unwrap_or_default();
        payload.clear();
        payload.extend_from_slice(quadro.payload);

        // Posição na ordem serial. A fila tem no máximo `capacidade` elementos (6 na política
        // padrão), então a busca linear é mais barata que qualquer estrutura ordenada.
        let pos = self
            .fila
            .iter()
            .position(|g| depois_de(g.sequencia, seq))
            .unwrap_or(self.fila.len());
        if pos != self.fila.len() {
            // Entrou na frente de alguém que já estava aqui: chegou fora de ordem, e o buffer o
            // salvou.
            self.contadores.reordenados += 1;
        }
        self.fila.insert(
            pos,
            Guardado {
                sequencia: seq,
                timestamp_us: quadro.timestamp_us,
                payload,
            },
        );

        let ocupacao = self.fila.len() as u16;
        if ocupacao > self.contadores.ocupacao_maxima {
            self.contadores.ocupacao_maxima = ocupacao;
        }
    }

    /// Entrega tudo o que já pode sair, mantendo [`Politica::profundidade`] pacotes retidos.
    pub fn escoar(&mut self, entregar: impl FnMut(Entrega<'_>)) {
        self.bombear(usize::from(self.politica.profundidade), entregar);
    }

    /// Fim de fluxo: entrega **tudo**, inclusive o que estava retido, e zera a fila.
    ///
    /// Sem isto, os últimos `profundidade` pacotes de toda sessão sumiriam — 40 ms de áudio que
    /// atravessou a rede e nunca tocou. Numa corrida de 10 s isso é 0,4% dos quadros, e é
    /// exatamente o tipo de perda que apareceria numa tabela como "recebidos: 498" sem que
    /// ninguém soubesse de onde veio.
    pub fn drenar(&mut self, entregar: impl FnMut(Entrega<'_>)) {
        self.bombear(0, entregar);
    }

    fn bombear(&mut self, reter: usize, mut entregar: impl FnMut(Entrega<'_>)) {
        let quadro_us = u64::from(self.politica.duracao_do_quadro_us);

        while self.fila.len() > reter {
            let Some(proxima) = self.proxima else { return };
            let cabeca = self.fila[0].sequencia;

            if cabeca == proxima {
                // Empréstimos disjuntos: `self.fila` só é lida enquanto `entregar` roda; os
                // contadores são outro campo.
                let g = &self.fila[0];
                self.contadores.slots_entregues += 1;
                self.contadores.quadros += 1;
                entregar(Entrega::Quadro {
                    payload: &g.payload,
                    sequencia: g.sequencia,
                    timestamp_us: g.timestamp_us,
                });
                if let Some(g) = self.fila.pop_front() {
                    self.reciclar(g.payload);
                }
                self.proxima = Some(proxima.wrapping_add(1));
                continue;
            }

            // Buraco no slot `proxima`. O carimbo dele é interpolado a partir da cabeça: não há
            // pacote de onde ler um.
            let distancia_ate_a_cabeca = u64::from(cabeca.wrapping_sub(proxima));
            let timestamp_us = self.fila[0]
                .timestamp_us
                .saturating_sub(distancia_ate_a_cabeca * quadro_us);

            self.contadores.slots_entregues += 1;
            self.contadores.buracos += 1;

            // O sucessor imediato está em mãos? É ele — e só ele — que carrega o LBRR do slot
            // que faltou.
            let tem_socorro = self.politica.fec_disponivel && cabeca == proxima.wrapping_add(1);
            if tem_socorro {
                let g = &self.fila[0];
                self.contadores.curas_oferecidas += 1;
                entregar(Entrega::Fec {
                    socorro: &g.payload,
                    sequencia: proxima,
                    timestamp_us,
                });
            } else {
                self.contadores.silencios += 1;
                entregar(Entrega::Silencio {
                    sequencia: proxima,
                    timestamp_us,
                });
            }

            // O buraco não consumiu pacote nenhum: a fila continua do mesmo tamanho, e a próxima
            // volta do laço decide sobre o slot seguinte. `salto_maximo` é o que garante que este
            // laço termina em tempo limitado.
            self.proxima = Some(proxima.wrapping_add(1));
        }
    }

    fn ressincronizar(&mut self, nova_base: u16) {
        while let Some(g) = self.fila.pop_front() {
            self.reciclar(g.payload);
        }
        self.proxima = Some(nova_base);
        self.contadores.resincronizacoes += 1;
    }

    fn reciclar(&mut self, payload: Vec<u8>) {
        if self.livres.len() < self.capacidade {
            self.livres.push(payload);
        }
    }

    /// O atraso deste pacote em relação ao mais rápido **recente**. Ver
    /// [`ContadoresDeBuffer::atraso_por_slots`] e [`MinimoPorJanela`].
    fn medir_atraso(&mut self, carimbo_us: u64, agora_us: u64) {
        let transito = agora_us as i64 - carimbo_us as i64;
        let minimo = self.transito_minimo.observar(transito, agora_us);
        let atraso = (transito - minimo).max(0) as u64;
        if atraso > self.contadores.atraso_max_us {
            self.contadores.atraso_max_us = atraso;
        }
        let faixa = (atraso / u64::from(self.politica.duracao_do_quadro_us)) as usize;
        let ultima = self.contadores.atraso_por_slots.len() - 1;
        self.contadores.atraso_por_slots[faixa.min(ultima)] += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constrói um `QuadroDeAudio` de teste. O payload é um byte com o próprio número de
    /// sequência, para que a asserção possa dizer *qual* pacote saiu.
    fn quadro(seq: u16, payload: &[u8]) -> QuadroDeAudio<'_> {
        QuadroDeAudio {
            payload,
            timestamp_us: u64::from(seq) * 20_000,
            sequencia: seq,
            marca: true,
        }
    }

    /// Roda uma sequência de chegadas e devolve a lista de ordens que saíram, em ordem.
    ///
    /// `chegadas` é `(sequência, chegada_us)`. O payload é `[sequência as u8]`.
    fn correr(politica: Politica, chegadas: &[(u16, u64)]) -> (Vec<String>, ContadoresDeBuffer) {
        let mut b = BufferDeJitter::novo(politica);
        let mut saida = Vec::new();
        for (seq, agora) in chegadas {
            let p = [*seq as u8];
            b.aceitar(&quadro(*seq, &p), *agora, |e| saida.push(descrever(&e)));
        }
        b.drenar(|e| saida.push(descrever(&e)));
        let c = b.contadores();
        (saida, c)
    }

    fn descrever(e: &Entrega<'_>) -> String {
        match e {
            Entrega::Quadro {
                payload, sequencia, ..
            } => format!("Q{sequencia}:{}", payload[0]),
            Entrega::Fec {
                socorro, sequencia, ..
            } => format!("F{sequencia}<-{}", socorro[0]),
            Entrega::Silencio { sequencia, .. } => format!("S{sequencia}"),
        }
    }

    /// A cadência limpa: tudo sai em ordem, uma vez cada.
    #[test]
    fn fluxo_sem_perda_sai_inteiro_e_em_ordem() {
        let chegadas: Vec<(u16, u64)> = (0..10).map(|i| (i, u64::from(i) * 20_000)).collect();
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        let esperado: Vec<String> = (0..10).map(|i| format!("Q{i}:{i}")).collect();
        assert_eq!(saida, esperado);
        assert_eq!(c.quadros, 10);
        assert_eq!(c.buracos, 0);
        assert_eq!(c.slots_entregues, 10);
    }

    /// **A invariante da `ContadoresDeBuffer`**, conferida sobre um fluxo com de tudo dentro.
    #[test]
    fn os_contadores_fecham_a_aritmetica() {
        // 0..20 com 4, 5 e 12 perdidos, 8 e 9 trocados de ordem e 15 duplicado.
        let mut chegadas: Vec<(u16, u64)> = Vec::new();
        for i in 0..20u16 {
            if i == 4 || i == 5 || i == 12 {
                continue;
            }
            chegadas.push((i, u64::from(i) * 20_000));
        }
        // troca 8 e 9
        let p8 = chegadas.iter().position(|(s, _)| *s == 8).unwrap();
        chegadas.swap(p8, p8 + 1);
        chegadas.push((15, 400_000));

        let (_, c) = correr(Politica::MICROFONE, &chegadas);
        assert_eq!(c.slots_entregues, c.quadros + c.buracos);
        assert_eq!(c.buracos, c.curas_oferecidas + c.silencios);
        assert_eq!(c.buracos, 3);
        assert_eq!(c.duplicados + c.tarde_demais, 1);
    }

    /// Uma perda isolada vira uma oferta de FEC, e o socorro é **o pacote seguinte**.
    #[test]
    fn perda_isolada_oferece_o_sucessor_como_socorro() {
        let chegadas: Vec<(u16, u64)> = (0..10)
            .filter(|i| *i != 4)
            .map(|i| (i, u64::from(i) * 20_000))
            .collect();
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        assert!(
            saida.contains(&"F4<-5".to_string()),
            "o slot 4 tinha de ser oferecido com o pacote 5 como socorro; saiu {saida:?}"
        );
        assert_eq!(c.buracos, 1);
        assert_eq!(c.curas_oferecidas, 1);
        assert_eq!(c.silencios, 0);
    }

    /// **A regra que o LBRR impõe ao produto**: numa rajada de duas perdas, só a última é
    /// recuperável. A primeira não tem sucessor em mãos.
    #[test]
    fn em_duas_perdas_seguidas_so_a_segunda_tem_socorro() {
        let chegadas: Vec<(u16, u64)> = (0..12)
            .filter(|i| *i != 4 && *i != 5)
            .map(|i| (i, u64::from(i) * 20_000))
            .collect();
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        assert!(saida.contains(&"S4".to_string()), "saiu {saida:?}");
        assert!(saida.contains(&"F5<-6".to_string()), "saiu {saida:?}");
        assert_eq!(c.buracos, 2);
        assert_eq!(c.curas_oferecidas, 1);
        assert_eq!(c.silencios, 1);
    }

    /// Sem FEC no preset, o buffer **não oferece** — nem quando o sucessor está em mãos.
    #[test]
    fn sem_fec_na_politica_todo_buraco_vira_silencio() {
        let chegadas: Vec<(u16, u64)> = (0..10)
            .filter(|i| *i != 4)
            .map(|i| (i, u64::from(i) * 20_000))
            .collect();
        let (saida, c) = correr(Politica::AUDIO_DO_SISTEMA, &chegadas);
        assert!(saida.contains(&"S4".to_string()), "saiu {saida:?}");
        assert_eq!(c.curas_oferecidas, 0);
        assert_eq!(c.silencios, 1);
    }

    /// Reordenação dentro da profundidade é **salva**: sai em ordem, sem buraco nenhum.
    #[test]
    fn reordenacao_dentro_da_profundidade_e_salva() {
        let chegadas = [
            (0u16, 0u64),
            (1, 20_000),
            (3, 60_000),
            (2, 62_000), // chegou depois do 3, mas antes de o slot 2 sair
            (4, 80_000),
            (5, 100_000),
        ];
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        assert_eq!(
            saida,
            vec!["Q0:0", "Q1:1", "Q2:2", "Q3:3", "Q4:4", "Q5:5"],
            "a reordenação tinha de ser invisível na saída"
        );
        assert_eq!(c.reordenados, 1);
        assert_eq!(c.buracos, 0);
        assert_eq!(c.tarde_demais, 0);
    }

    /// Fora da profundidade não há salvação: o slot já saiu, e o pacote é jogado fora.
    ///
    /// É o contador que diz "a profundidade está pequena", e ele precisa ser distinguível do de
    /// duplicado.
    #[test]
    fn reordenacao_fora_da_profundidade_chega_tarde_demais() {
        let raso = Politica {
            profundidade: 0,
            ..Politica::MICROFONE
        };
        let chegadas = [
            (0u16, 0u64),
            (1, 20_000),
            (3, 60_000),
            (2, 62_000),
            (4, 80_000),
        ];
        let (saida, c) = correr(raso, &chegadas);
        assert_eq!(c.tarde_demais, 1);
        assert_eq!(c.duplicados, 0);
        assert!(
            saida.contains(&"F2<-3".to_string()),
            "com profundidade 0 o slot 2 vira buraco antes de o pacote chegar; saiu {saida:?}"
        );
    }

    /// Duplicado é contado como duplicado, não como atraso.
    #[test]
    fn duplicado_na_fila_e_contado_a_parte() {
        let chegadas = [
            (0u16, 0u64),
            (1, 20_000),
            (2, 40_000),
            (3, 60_000),
            (3, 60_100), // eco: ainda está na fila
            (4, 80_000),
        ];
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        assert_eq!(c.duplicados, 1);
        assert_eq!(c.tarde_demais, 0);
        assert_eq!(saida, vec!["Q0:0", "Q1:1", "Q2:2", "Q3:3", "Q4:4"]);
    }

    /// Um salto enorme ressincroniza em vez de despejar milhares de slots de PLC no DAC.
    #[test]
    fn salto_grande_ressincroniza_em_vez_de_inventar_um_minuto_de_plc() {
        let chegadas = [
            (0u16, 0u64),
            (1, 20_000),
            (2, 40_000),
            (5000, 60_000),
            (5001, 80_000),
            (5002, 100_000),
            (5003, 120_000),
        ];
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        assert_eq!(c.resincronizacoes, 1);
        assert!(
            c.buracos <= 1,
            "ressincronizar não pode inventar buraco: {} buracos, saída {saida:?}",
            c.buracos
        );
        assert!(
            saida.len() < 20,
            "saiu {} ordens; devia ser punhado",
            saida.len()
        );
    }

    /// A profundidade é o que ela diz ser: com 2, o pacote N só sai quando N+2 chega.
    #[test]
    fn a_profundidade_e_a_latencia_que_ela_promete() {
        let mut b = BufferDeJitter::novo(Politica::MICROFONE);
        assert_eq!(Politica::MICROFONE.latencia_us(), 40_000);

        let mut saiu = Vec::new();
        for i in 0..3u16 {
            let p = [i as u8];
            b.aceitar(&quadro(i, &p), u64::from(i) * 20_000, |e| {
                saiu.push(e.sequencia())
            });
        }
        // Chegaram 0, 1 e 2; só o 0 pode ter saído.
        assert_eq!(saiu, vec![0]);
        assert_eq!(b.ocupacao(), 2);
    }

    /// `drenar` não pode deixar os últimos pacotes na fila.
    #[test]
    fn drenar_entrega_o_que_estava_retido() {
        let chegadas: Vec<(u16, u64)> = (0..5).map(|i| (i, u64::from(i) * 20_000)).collect();
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        assert_eq!(saida.len(), 5, "saiu {saida:?}");
        assert_eq!(c.slots_entregues, 5);
    }

    /// A volta de 65535 para 0 não pode virar um salto de 65 mil slots.
    #[test]
    fn a_volta_da_sequencia_de_16_bits_nao_inventa_buraco() {
        let chegadas: Vec<(u16, u64)> = (0..10u64)
            .map(|i| ((65_530u16).wrapping_add(i as u16), i * 20_000))
            .collect();
        let (saida, c) = correr(Politica::MICROFONE, &chegadas);
        assert_eq!(c.buracos, 0, "saiu {saida:?}");
        assert_eq!(c.resincronizacoes, 0);
        assert_eq!(c.quadros, 10);
    }

    /// A fila não cresce: com reordenação e perda por 1 000 pacotes, a ocupação fica no teto.
    #[test]
    fn a_fila_nao_cresce_com_o_tempo_de_sessao() {
        let mut b = BufferDeJitter::novo(Politica::MICROFONE);
        for i in 0..1_000u16 {
            if i % 37 == 0 {
                continue; // perda periódica
            }
            let seq = if i % 11 == 0 { i.wrapping_add(1) } else { i };
            let p = [seq as u8];
            b.aceitar(&quadro(seq, &p), u64::from(seq) * 20_000, |_| {});
        }
        let c = b.contadores();
        assert!(
            c.ocupacao_maxima <= Politica::MICROFONE.profundidade + 4,
            "ocupação máxima {} passou do teto",
            c.ocupacao_maxima
        );
        assert!(b.ocupacao() <= usize::from(Politica::MICROFONE.profundidade) + 4);
    }

    /// O histograma de atraso é o instrumento que dimensiona a profundidade — e ele mede **em
    /// relação ao pacote mais rápido**, não em relação ao anterior.
    #[test]
    fn o_histograma_de_atraso_poe_cada_pacote_na_faixa_certa() {
        let mut b = BufferDeJitter::novo(Politica::MICROFONE);
        // Pacote 0 é o mais rápido (trânsito 0). O 1 atrasa 25 ms (faixa 1), o 2 atrasa 45 ms
        // (faixa 2), o 3 volta ao mínimo (faixa 0).
        let chegadas = [(0u16, 0u64), (1, 45_000), (2, 85_000), (3, 60_000)];
        for (seq, agora) in chegadas {
            let p = [seq as u8];
            b.inserir(&quadro(seq, &p), agora);
        }
        let c = b.contadores();
        assert_eq!(c.atraso_por_slots[0], 2, "o 0 e o 3 chegaram no mínimo");
        assert_eq!(c.atraso_por_slots[1], 1);
        assert_eq!(c.atraso_por_slots[2], 1);
        assert_eq!(c.atraso_max_us, 45_000);
        assert_eq!(c.profundidade_necessaria(), 2);
    }

    /// `profundidade_necessaria` responde a pergunta do dimensionamento, e responde 0 quando a
    /// rede entregou tudo no mesmo trânsito.
    #[test]
    fn profundidade_necessaria_e_zero_num_fluxo_perfeito() {
        let chegadas: Vec<(u16, u64)> = (0..50).map(|i| (i, u64::from(i) * 20_000)).collect();
        let (_, c) = correr(Politica::MICROFONE, &chegadas);
        assert_eq!(c.profundidade_necessaria(), 0);
        assert_eq!(c.atraso_max_us, 0);
    }

    /// **Deriva longa**: o relógio do receptor 50 ppm mais rápido que o do emissor, rede
    /// perfeita, uma hora. É o cenário da crítica 1 (§6c) que, com o mínimo da sessão inteira, deu
    /// `profundidade_necessaria = 7`.
    #[test]
    fn a_deriva_longa_nao_espalha_o_histograma() {
        // A referência é conferida no `MinimoPorJanela`, que é quem a decide: uma hora a 50/s
        // passa da volta da sequência de 16 bits, e o teste pelo buffer inteiro, abaixo, fica em
        // 20 minutos por isso. A chegada cresce 20 001 µs por quadro: 50 ppm.
        let n = 60 * 60 * 50u64;
        let mut m = MinimoPorJanela::default();
        let mut pior = 0i64;
        for s in 0..n {
            let captura = (s * 20_000) as i64;
            let chegada = s * 20_000 + s + 5_000;
            let transito = chegada as i64 - captura;
            let referencia = m.observar(transito, chegada);
            pior = pior.max(transito - referencia);
        }
        assert!(
            pior < 1_000,
            "o atraso relativo máximo tem de ficar no ruído da janela, e foi {pior} µs"
        );
    }

    /// O mesmo cenário pelo buffer inteiro, com carimbos que não dão a volta: 20 minutos.
    #[test]
    fn a_deriva_longa_pelo_buffer_da_profundidade_zero() {
        let mut b = BufferDeJitter::novo(Politica::MICROFONE);
        let n = 20 * 60 * 50u64;
        for s in 0..n {
            let captura = s * 20_000;
            let chegada = captura + captura / 20_000 + 5_000;
            let p = [0u8];
            let q = QuadroDeAudio {
                payload: &p,
                timestamp_us: captura,
                sequencia: s as u16,
                marca: true,
            };
            b.aceitar(&q, chegada, |_| {});
        }
        let c = b.contadores();
        assert_eq!(
            c.profundidade_necessaria(),
            0,
            "histograma {:?}, atraso máximo {} µs",
            c.atraso_por_slots,
            c.atraso_max_us
        );
    }
}
