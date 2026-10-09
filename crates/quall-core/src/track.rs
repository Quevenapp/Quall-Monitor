//! Tracks de mídia: a fronteira entre o núcleo e as cascas.
//!
//! Implementa [`docs/contrato-track.md`](../../../docs/contrato-track.md). O núcleo **não
//! captura e não decodifica**: recebe quadro já codificado e entrega quadro codificado. Os nomes
//! deste módulo — `enviar_quadro`, `ao_pedir_idr`, `ao_receber_quadro`, `pedir_idr`,
//! [`QuadroCodificado`], [`TrackKind`] — são o contrato, e três frentes codificam contra eles.
//!
//! # O caminho do quadro não tem fila nem cópia guardada
//!
//! [`TrackEmissor::enviar_quadro`] recebe uma fatia emprestada, entrega ao pacotizador da
//! libdatachannel e volta. Não há `Vec` nosso no meio, não há canal, não há thread de saída.
//! Isso não é preferência de estilo: é o que mantém o núcleo dentro dos ~50 MB da Broadcast
//! Upload Extension do iOS.
//!
//! Existe **uma** cópia, dentro da libdatachannel, quando ela transforma a fatia em mensagem
//! para fatiar em RTP. Ela é transitória — morre no fim da pacotização — e não é evitável pela
//! API C. Dizer que não há cópia nenhuma seria mentira; o que não há é cópia **guardada**.
//!
//! Do lado que recebe, o [`crate::rtp::Depacotizador`] mantém um buffer de remontagem por track,
//! porque um quadro fragmentado em FU-A não existe até o último fragmento chegar. É um buffer
//! fixo por track, reaproveitado, e não uma fila de quadros.
//!
//! # O pedido de IDR
//!
//! É a razão de o contrato ter escolhido track de mídia em vez de canal de dados. O receptor
//! chama [`TrackReceptor::pedir_idr`], que emite PLI (RFC 4585); no emissor,
//! [`TrackEmissor::ao_pedir_idr`] dispara. O tratador da libdatachannel reconhece **PLI e FIR**
//! (RFC 5104) e chama o mesmo tratador — ver [`quall_rtc::Track::ao_pedir_idr`].

use crate::error::{Error, Result};

/// O quadro de áudio que **chega**, re-exportado de [`crate::rtp`].
///
/// Mora lá porque quem o constrói é o depacotizador; aparece aqui porque o contrato de mídia é
/// este módulo, e uma casca não deveria ter de saber em qual arquivo o tipo foi escrito.
pub use crate::rtp::QuadroDeAudio;

/// O que uma track carrega.
///
/// Uma sessão carrega várias simultaneamente, cada uma na sua track. No iOS isso não é opção:
/// a tela vem da Broadcast Upload Extension e a câmera vem do app principal — dois processos,
/// duas tracks, uma sessão.
///
/// # Há **duas** espécies de áudio, e confundi-las é defeito de produto
///
/// A frase do `PROMPT.md` que promete "tela + câmera + microfone simultâneos" fala de microfone
/// de verdade. Mas a tabela de captura do mesmo documento lista, por plataforma, **WASAPI
/// loopback** (Windows), **ScreenCaptureKit** (macOS) e **AudioPlaybackCapture** (Android): os
/// três são *áudio do sistema*, não microfone. Nenhuma dessas três APIs abre um microfone.
///
/// São conteúdos diferentes e pedem codificação diferente. Quem espelha a tela quer o som do
/// que está tocando — música, vídeo, jogo, estéreo, banda larga. Quem compartilha a câmera quer
/// a fala — mono, estreita, e onde perder 20 ms estraga a inteligibilidade. Um preset só
/// serviria mal aos dois: fala em estéreo a 128 kbit/s é desperdício, e música em mono a 32
/// kbit/s é ruim de ouvir.
///
/// Por isso as duas existem separadas, e o preset sai da **espécie** da track — ver
/// [`TrackKind::preset_de_audio`], que é uma tabela justamente para que uma terceira espécie
/// seja uma linha, e não uma cirurgia.
///
/// # A ordem deste enum é ABI, e não pode ser mexida
///
/// Ele vira `QuallTrackKind` no `quall.h` gerado, com os valores explícitos que as cascas já
/// compilaram: `SCREEN = 0`, `CAMERA = 1`, `MICROPHONE = 2`. **Espécie nova entra no fim.**
/// Renumerar ou inserir no meio troca o significado dos números em toda casca já construída, e
/// o compilador de nenhuma delas teria como perceber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TrackKind {
    Screen,
    Camera,
    /// A fala de quem está transmitindo. Ver [`PRESET_MICROFONE`].
    Microphone,
    /// O som que o aparelho está tocando: WASAPI loopback, ScreenCaptureKit,
    /// AudioPlaybackCapture. Ver [`PRESET_AUDIO_DO_SISTEMA`].
    ///
    /// Entrou depois das outras três, e **no fim de propósito** — ver a nota de ABI acima.
    SystemAudio,
}

impl TrackKind {
    /// O `mid` da linha `m=` no SDP. É por ele que os dois lados casam as tracks.
    ///
    /// Em inglês e minúsculo, como o resto dos identificadores que vão para a rede.
    pub fn mid(self) -> &'static str {
        match self {
            TrackKind::Screen => "screen",
            TrackKind::Camera => "camera",
            TrackKind::Microphone => "microphone",
            TrackKind::SystemAudio => "system-audio",
        }
    }

    /// Reconhece o tipo a partir do `mid` que veio no SDP.
    pub fn from_mid(mid: &str) -> Result<Self> {
        match mid {
            "screen" => Ok(TrackKind::Screen),
            "camera" => Ok(TrackKind::Camera),
            "microphone" => Ok(TrackKind::Microphone),
            "system-audio" => Ok(TrackKind::SystemAudio),
            outro => Err(Error::Protocol(format!(
                "track de mid desconhecido: {outro}"
            ))),
        }
    }

    /// SSRC fixo por tipo.
    ///
    /// Poderia ser sorteado — o SDP anuncia o valor, então o receptor descobre de qualquer
    /// jeito. É fixo porque um SSRC previsível transforma um `tcpdump` em algo legível: dá para
    /// separar tela de câmera olhando o pacote, sem correlacionar com a sinalização.
    pub fn ssrc(self) -> u32 {
        match self {
            TrackKind::Screen => 0x5155_0001,
            TrackKind::Camera => 0x5155_0002,
            TrackKind::Microphone => 0x5155_0003,
            TrackKind::SystemAudio => 0x5155_0004,
        }
    }

    /// É vídeo?
    ///
    /// **Escrito como a negação de [`TrackKind::e_audio`], e isso importa.** A versão anterior
    /// era `!matches!(self, TrackKind::Microphone)` — uma lista de exceções. Quando
    /// [`TrackKind::SystemAudio`] entrou, essa forma o teria classificado como **vídeo**, em
    /// silêncio: a track sairia com pacotizador H.264 e `m=video` para carregar Opus, e o
    /// primeiro sinal seria ausência de som. Perguntar "é áudio?" e negar mantém as duas
    /// respostas amarradas a uma definição só.
    pub fn e_video(self) -> bool {
        !self.e_audio()
    }

    pub fn e_audio(self) -> bool {
        matches!(self, TrackKind::Microphone | TrackKind::SystemAudio)
    }

    /// O preset de áudio desta espécie, ou `None` se ela for vídeo.
    ///
    /// **É a tabela.** Codec, canais, taxa, tamanho de quadro e FEC saem daqui e de nenhum outro
    /// lugar; não há `if kind == Microphone` espalhado pelo módulo. Uma espécie nova de áudio é
    /// uma linha neste `match` mais uma constante ao lado — e não uma varredura atrás de todo
    /// ponto que precisaria saber dela.
    pub fn preset_de_audio(self) -> Option<PresetDeAudio> {
        match self {
            TrackKind::Screen | TrackKind::Camera => None,
            TrackKind::Microphone => Some(PRESET_MICROFONE),
            TrackKind::SystemAudio => Some(PRESET_AUDIO_DO_SISTEMA),
        }
    }
}

/// O codec de uma track de áudio.
///
/// # Opus é o codec do produto. O que foi descartado, e por quê
///
/// - **AAC** existe na API C da libdatachannel e tem encoder de hardware em todo aparelho da
///   bancada (MediaCodec, AudioToolbox, Media Foundation). Perdeu por latência: o quadro do AAC-LC
///   é de 1024 amostras — 21,3 ms a 48 kHz — e o encoder ainda cobra *priming* de 1024 a 2048
///   amostras de lookahead antes de emitir o primeiro quadro. São 40 a 60 ms de atraso
///   algorítmico contra os 26,5 ms do Opus a 20 ms, num orçamento que persegue 50.
/// - **G.722** é 16 kHz a 64 kbit/s. Gasta o dobro do Opus para entregar menos banda de áudio.
/// - **PCM cru pelo canal de dados** reinventaria RTP, SRTP, carimbo e sequência — as quatro
///   coisas que a track de mídia dá prontas.
///
/// [`CodecDeAudio::Pcmu`] fica como **piso**, não como alternativa: G.711 µ-law é uma tabela de
/// consulta de 8 bits, então é o único codec que qualquer plataforma consegue produzir sem
/// biblioteca nenhuma. É o que permite provar o caminho de áudio inteiro sem depender de um
/// libopus compilado — e é a saída se algum aparelho da matriz não tiver encoder de Opus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CodecDeAudio {
    /// Opus a 48 kHz (RFC 7587). O codec do produto.
    Opus,
    /// G.711 µ-law a 8 kHz (RFC 3551). Piso sem dependência.
    Pcmu,
}

impl CodecDeAudio {
    /// O tipo de payload RTP.
    ///
    /// **111 para Opus** não é obrigatório — cada linha `m=` tem o próprio espaço de payload
    /// type, e 96 funcionaria. É 111 porque é o que Chrome e Firefox usam há uma década, o que
    /// faz um `tcpdump` desta sessão ser lido corretamente por um Wireshark sem configuração.
    /// É a mesma razão que fixou os SSRC em [`TrackKind::ssrc`]: um número previsível transforma
    /// uma captura em algo legível.
    ///
    /// **0 para PCMU** não é escolha: a RFC 3551 §6 o atribui estaticamente.
    /// Quantos canais este codec de fato transporta, dado quantos o preset da espécie pede.
    ///
    /// **O codec sobrepõe o preset, e não o contrário.** A RFC 3551 §6 atribui o payload type 0 a
    /// `PCMU/8000/1`: G.711 é mono por definição e não há como transportar dois canais nele. Uma
    /// track de áudio de sistema pede estéreo pelo preset; se o codec negociado for PCMU, o que
    /// anda no fio é mono, e quem gravar um `.wav` declarando 2 canais produz um arquivo que toca
    /// no dobro da velocidade e uma oitava acima — sem erro em lugar nenhum.
    ///
    /// Foi exatamente esse defeito, achado em 27/08/2026 na sonda, que motivou esta função existir
    /// em vez de cada chamador derivar canais por conta própria. Os dois lados do `quall-probe`
    /// discordavam entre si: quem emitia fixava 1, quem recebia derivava 2.
    pub fn canais_no_fio(self, pedidos: u8) -> u8 {
        match self {
            CodecDeAudio::Opus => pedidos.max(1),
            CodecDeAudio::Pcmu => 1,
        }
    }

    pub fn payload_type(self) -> u8 {
        match self {
            CodecDeAudio::Opus => 111,
            CodecDeAudio::Pcmu => 0,
        }
    }

    /// **Quanto o conteúdo decodificado sai atrás do carimbo**, em µs: o que o decodificador
    /// devolve na posição `p` de um quadro com carimbo `t` foi capturado em `t + p − este atraso`.
    ///
    /// No Opus é o *lookahead* do codificador do emissor: 312 amostras a 48 kHz, **6,5 ms**, nas
    /// aplicações `VOIP` e `AUDIO` da libopus (2,5 ms de antecipação e 4 ms de compensação), que
    /// são as dos emissores do Quall. Medido de ponta a ponta no T0 do Mac (+6,6 ms,
    /// `docs/som-no-receptor.md` §20.7) e amostra a amostra pela correlação
    /// (`quall-opus`, `o_conteudo_decodificado_sai_atrasado_o_lookahead_do_codificador`: 312, 310
    /// na voz; e 120 no `RESTRICTED_LOWDELAY`, o controle).
    ///
    /// **O receptor não tem como saber pelo pacote**: um emissor em `RESTRICTED_LOWDELAY` (2,5 ms)
    /// sairia 4 ms adiantado nesta conta. O carimbo do emissor continua sendo a captura da primeira
    /// amostra que entrou no codificador, a convenção do RTP (RFC 7587 não manda descontar nada);
    /// quem desconta é o receptor, com este número, como já desconta o filtro do PCMU.
    ///
    /// O PCMU não tem atraso de codec: o que ele tem é o do filtro por 6 de cada casca, que é dela.
    pub fn atraso_do_conteudo_us(self) -> u32 {
        match self {
            CodecDeAudio::Opus => 6_500,
            CodecDeAudio::Pcmu => 0,
        }
    }

    /// A taxa do relógio RTP, em Hz. Ver [`crate::rtp::RELOGIO_OPUS_HZ`].
    pub fn relogio_hz(self) -> u32 {
        match self {
            CodecDeAudio::Opus => crate::rtp::RELOGIO_OPUS_HZ,
            CodecDeAudio::Pcmu => crate::rtp::RELOGIO_PCMU_HZ,
        }
    }

    /// O nome que aparece no `a=rtpmap` do SDP, em minúsculas para comparação.
    pub fn nome_rtpmap(self) -> &'static str {
        match self {
            CodecDeAudio::Opus => "opus",
            CodecDeAudio::Pcmu => "pcmu",
        }
    }

    /// Quantas amostras cabem num quadro da duração dada, nesta taxa.
    ///
    /// A 48 kHz e 20 ms são 960; a 8 kHz e 20 ms, 160.
    pub fn amostras_por_quadro(self, duracao_ms: u32) -> usize {
        (self.relogio_hz() as usize) * (duracao_ms as usize) / 1000
    }
}

/// Como uma **espécie** de áudio deve ser codificada.
///
/// Existe para que a resposta a "que codec, quantos canais, que taxa?" seja uma consulta a
/// [`TrackKind::preset_de_audio`] e não um `if` repetido em cinco lugares. A canalização inteira
/// — abertura da track, `a=fmtp`, relógio RTP, tamanho de quadro — lê deste struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetDeAudio {
    pub codec: CodecDeAudio,
    /// 1 para fala, 2 para som de sistema. Vira `stereo=` no `a=fmtp`.
    pub canais: u8,
    /// Alvo de taxa média, em bits por segundo. Vira `maxaveragebitrate=`.
    pub taxa_media_bits: u32,
    /// Duração de um quadro, em milissegundos. Ver [`DURACAO_DO_QUADRO_MS`].
    pub duracao_do_quadro_ms: u32,
    /// Ligar o FEC embutido do Opus? Vira `useinbandfec=`. **Não é preferência** — ver
    /// [`PRESET_AUDIO_DO_SISTEMA`], onde ele está desligado porque o encoder não teria como
    /// honrá-lo.
    pub fec: bool,
    /// A perda de pacote que o encoder deve **supor**, em porcento
    /// (`OPUS_SET_PACKET_LOSS_PERC`).
    ///
    /// # Este campo existe porque `fec: true` sozinho não produz FEC nenhum
    ///
    /// Medido no MacBook Air M4 em 2026-08-27, com a libopus 1.5.2 vendorizada, 100 quadros por
    /// caso, mono a 32 kbit/s:
    ///
    /// | `fec` | perda declarada | modo em regime | quadros com LBRR |
    /// |---|---|---|---|
    /// | `true` | **0%** | CELT 282, híbrido 18 | **0 de 300** |
    /// | `true` | 5% | CELT 282, híbrido 9, SILK 9 | **17 de 300** |
    /// | `true` | 8% | híbrido 199, SILK 101 | **299 de 300** |
    /// | `false` | 0% | CELT 282, híbrido 18 | 0 de 300 |
    ///
    /// Com a perda declarada em 0 — que é o **padrão** da libopus — ligar o `useinbandfec`
    /// produz um fluxo **indistinguível** do de FEC desligado. E não é só escolha de modo:
    /// forçando `OPUS_SET_SIGNAL(VOICE)`, o encoder ficou em **híbrido nos 300 quadros** — um
    /// modo onde o LBRR existe — e ainda assim emitiu **0 de 300**. Quem faz o LBRR aparecer é
    /// este campo, não o `fec`.
    ///
    /// Sem ele, uma casca leria `useinbandfec=1` no preset, ligaria o FEC no encoder e emitiria
    /// um fluxo sem recuperação nenhuma, enquanto o receptor do outro lado dimensionaria o jitter
    /// buffer contando com ela. É o defeito do SPS sem `bitstream_restriction` do M4 outra vez:
    /// **declarar no fio o que não se faz.**
    ///
    /// **O valor 5 vem da varredura, junto de [`PresetDeAudio::conteudo_e_fala`]**: com o sinal
    /// forçado em voz, o patamar de 100% de LBRR começa em **1%** e não se move até 30%; 5 fica
    /// dentro dele com folga. Sem o sinal forçado o patamar só começa em 8% — ver a tabela em
    /// `docs/audio.md` §11.
    ///
    /// **Custa quase nada de banda**: 33,1 contra 33,2 kbit/s com e sem LBRR, no mesmo alvo de
    /// 32 000. Num alvo fixo o encoder realoca bits em vez de acrescentá-los, então o LBRR é pago
    /// em qualidade do quadro principal, não em taxa.
    pub perda_esperada_pct: u8,
    /// O conteúdo desta espécie é **fala**?
    ///
    /// Vira `OPUS_SET_SIGNAL(VOICE)` e `OPUS_APPLICATION_VOIP` no encoder. Não vai para o SDP:
    /// não há parâmetro de `fmtp` que o carregue, e não deveria haver — é uma dica ao **nosso**
    /// encoder, não uma promessa ao outro lado.
    ///
    /// # Por que não deixar a análise do Opus decidir
    ///
    /// Porque nós sabemos, e ela adivinha. A análise do Opus classifica o conteúdo pelo sinal, e
    /// numa track de microfone **nós já sabemos** que é um microfone — a espécie da track é essa
    /// informação. Deixar a análise decidir foi medido em 2026-08-27 e é o que empurrou o preset
    /// de microfone para CELT em 282 de 300 quadros, onde o LBRR **não existe**, com
    /// `useinbandfec=1` declarado no SDP o tempo todo.
    ///
    /// É a regra do M4 na direção de entrada: **um emissor tem de fazer no encoder o que ele
    /// declara no fio.**
    pub conteudo_e_fala: bool,
}

impl PresetDeAudio {
    /// **A complexidade do encoder Opus** (`OPUS_SET_COMPLEXITY`, 0–10): não vai ao fio, só ao encoder.
    ///
    /// **10 nas duas espécies — o padrão da libopus, escrito de propósito** (27/09,
    /// `docs/teleprompter-com-camera.md` §8.12.17). O iPhone 7 quente mostrou o Opus comendo 31–69 % de um
    /// núcleo, e baixar a complexidade corta o custo (MacBook, voz mono a 32 kbit/s: 10 ≈ 0,8 ms por quadro,
    /// 7 ≈ 0,35, 5 ≈ 0,2) — **mas o LBRR que o fio promete (`useinbandfec=1`) não sobrevive**: a libopus
    /// multiplica a taxa equivalente por `(90 + complexidade)/100` antes de decidir o FEC (`decide_fec`), e a
    /// 32 kbit/s com 5 % de perda declarada o limiar fica logo abaixo de 10. Medido: com o sinal da bancada
    /// de FEC (`quall-probe fec`, as notas com parciais agudas), **8 já não emite LBRR** no pacote que cura o
    /// buraco, 9 emite; com o seno puro, o LBRR de um encoder que nasce em 6 ou menos é zero. Baixar o padrão
    /// seria declarar no fio o que não se faz (a §11 do `docs/audio.md`).
    ///
    /// A casca pode baixar **no calor** (`quall_audio_encoder_set_complexity`), onde a troca é dela: um
    /// encoder que já emitia LBRR segue emitindo com a complexidade baixa (a libopus decide o FEC com
    /// histerese sobre a decisão anterior), mas um que não emitia não liga.
    pub fn complexidade_do_encoder(&self) -> u8 {
        10
    }

    /// A linha `a=fmtp` que este preset declara.
    ///
    /// **Montada a partir dos campos, e não escrita à mão.** É o que garante que o SDP diga
    /// exatamente o que o preset diz: um texto fixo ao lado de uma struct é um convite a os dois
    /// discordarem depois de uma edição, e a discordância seria invisível — o SDP sai, o outro
    /// lado aceita, e ninguém confere.
    pub fn fmtp(&self) -> String {
        match self.codec {
            // G.711 não tem parâmetros de fmtp que valha a pena declarar.
            CodecDeAudio::Pcmu => String::new(),
            CodecDeAudio::Opus => {
                let estereo = u8::from(self.canais >= 2);
                format!(
                    "minptime=10;useinbandfec={};usedtx=0;stereo={estereo};\
                     sprop-stereo={estereo};maxaveragebitrate={}",
                    u8::from(self.fec),
                    self.taxa_media_bits
                )
            }
        }
    }

    /// Quantas amostras por canal cabem num quadro deste preset.
    pub fn amostras_por_quadro(&self) -> usize {
        self.codec.amostras_por_quadro(self.duracao_do_quadro_ms)
    }
}

/// Duração de um quadro de áudio, em milissegundos.
///
/// # 20 ms, e o que isso custa
///
/// O Opus aceita 2,5, 5, 10, 20, 40 e 60 ms. 20 ms é o padrão de fato do WebRTC, e a conta que o
/// sustenta é de overhead: a 32 kbit/s um quadro de 20 ms tem ~80 bytes de payload contra ~50
/// bytes fixos de cabeçalho (12 de RTP + ~10 de expansão do SRTP + 28 de UDP/IP), ou 38% de
/// overhead. A 10 ms o payload cai para ~40 bytes e o overhead sobe para 55%, com o dobro de
/// pacotes por segundo para o rádio tratar.
///
/// O que 20 ms custam em latência: o atraso algorítmico do Opus é o quadro **mais** 6,5 ms de
/// lookahead, ou **26,5 ms**. A 10 ms seriam 16,5 ms.
///
/// **Consequência que precisa estar dita**: com quadro de 20 ms e um jitter buffer de dois
/// pacotes — o mínimo que absorve uma troca de ordem —, o piso do áudio é 26,5 + 40 = **66,5 ms
/// antes da rede**. A meta de perseguir < 50 ms do `PROMPT.md` **não é alcançável para áudio
/// nesta configuração**; a de < 150 ms em LAN é folgada. Quem precisar de áudio abaixo de 50 ms
/// tem um caminho, e ele é este: quadro de 10 ms, buffer de dois pacotes, 16,5 + 20 = 36,5 ms —
/// pagando os 55% de overhead. Não é o padrão porque a meta de 50 ms é de vídeo, e porque áudio
/// chegando ~45 ms depois do vídeo está dentro do que a ITU-R BT.1359 considera imperceptível
/// (o ouvido tolera muito mais o som atrasado que o adiantado).
pub const DURACAO_DO_QUADRO_MS: u32 = 20;

/// Preset da fala: **mono, 32 kbit/s, FEC ligado.**
///
/// - **Mono** porque o microfone de um celular ou de um notebook é uma cápsula só. Estéreo
///   gastaria bits com um canal que não existe — e, pior, *declararia* estéreo no SDP, fazendo o
///   outro lado alocar dois canais. É a regra que o M4 do Windows cobrou caro: **um emissor tem
///   de declarar no fio o que ele de fato faz**, porque o consumidor não lê o nosso
///   código-fonte; ele lê os parâmetros e, na dúvida, escolhe o pior caso.
/// - **32 kbit/s** é a faixa em que o Opus é tido como transparente para fala em banda larga
///   mono. O padrão da libdatachannel é 96 000, dimensionado para música em estéreo.
/// - **FEC ligado**, e é aqui que ele *funciona*: o FEC embutido do Opus é o LBRR, um recurso dos
///   modos **SILK e híbrido**, que são os modos em que o Opus opera para fala em taxas como esta.
///   É também o conteúdo onde perder 20 ms mais dói — um fonema comido muda a palavra.
///
///   **E `fec: true` sozinho não basta.** Ver [`PresetDeAudio::perda_esperada_pct`]: medido em
///   2026-08-27, com a perda declarada em 0 o encoder emitiu **0 LBRR em 100 quadros**, e o
///   fluxo ficou indistinguível do de FEC desligado. Os 5% abaixo são o que torna
///   `useinbandfec=1` verdade no fio.
///
/// Isto **não** contradiz a recusa do NACK na track de vídeo. O respondedor de NACK foi recusado
/// porque guarda os últimos N pacotes enviados — a "cópia guardada" que o contrato proíbe — e
/// porque retransmitir custa uma ida e volta. O LBRR não faz nem uma coisa nem outra: ele carrega
/// uma cópia de baixa taxa do quadro *anterior* **dentro do pacote atual**, que já ia sair de
/// qualquer jeito. Custa banda e **zero** latência de rede, zero buffer no emissor.
///
/// **Números de desenho, não medidos.** Ver `docs/audio.md`, "o que eu não provei".
pub const PRESET_MICROFONE: PresetDeAudio = PresetDeAudio {
    codec: CodecDeAudio::Opus,
    canais: 1,
    taxa_media_bits: 32_000,
    duracao_do_quadro_ms: DURACAO_DO_QUADRO_MS,
    fec: true,
    perda_esperada_pct: 5,
    conteudo_e_fala: true,
};

/// Preset do som do sistema: **estéreo, 128 kbit/s, FEC desligado.**
///
/// - **Estéreo** porque o conteúdo é o que o aparelho está tocando — música, vídeo, jogo — e a
///   imagem espelhada sem o estéreo do som é meia entrega.
/// - **128 kbit/s** é o ponto em que o Opus é comumente tido como transparente para música em
///   estéreo. Numa LAN a diferença para os 96 000 do padrão da libdatachannel é livre: 128
///   kbit/s são 0,013% de um enlace de 1 Gbit/s.
/// - **FEC desligado, e este é o item que não é gosto.** O FEC embutido do Opus é o LBRR, que só
///   existe nos modos **SILK e híbrido**. Para música em estéreo a 128 kbit/s o Opus opera em
///   modo **CELT**, onde o LBRR não existe — declarar `useinbandfec=1` ali seria anunciar no SDP
///   um recurso que o encoder não tem como entregar.
///
///   É exatamente o defeito do SPS sem `bitstream_restriction` do M4, num codec diferente:
///   declarar o que não se faz. O consumidor que lesse `useinbandfec=1` teria o direito de
///   contar com uma recuperação que nunca viria, e dimensionaria o jitter buffer dele para
///   menos do que precisa.
///
/// A consequência honesta: **o áudio de sistema não tem recuperação de perda.** Pacote perdido é
/// 20 ms de som perdido, ocultado pela interpolação do decoder e nada mais. Numa LAN comutada
/// isso é raro; num Wi-Fi carregado, não é — e é o [`crate::rtp::Contadores::jitter_us`] que
/// avisa antes de o usuário reclamar.
///
/// **Números de desenho, não medidos.** Ver `docs/audio.md`, "o que eu não provei".
pub const PRESET_AUDIO_DO_SISTEMA: PresetDeAudio = PresetDeAudio {
    codec: CodecDeAudio::Opus,
    canais: 2,
    taxa_media_bits: 128_000,
    duracao_do_quadro_ms: DURACAO_DO_QUADRO_MS,
    fec: false,
    // Sem FEC não há o que a perda declarada pudesse ligar; declarar outra coisa aqui seria
    // ruído. Ver [`PresetDeAudio::perda_esperada_pct`].
    perda_esperada_pct: 0,
    // Música, vídeo, jogo — o que o aparelho está tocando. Ver
    // [`PresetDeAudio::conteudo_e_fala`].
    conteudo_e_fala: false,
};

/// Um quadro de áudio já codificado, indo para a rede.
///
/// Contraparte de [`QuadroCodificado`] do lado do áudio, e deliberadamente **não** é o mesmo
/// tipo: um quadro de áudio não tem `idr` (todo quadro de Opus é independente) e não tem
/// Annex-B. Fundir os dois obrigaria a um campo que só faz sentido em metade dos usos, que é
/// como se produz um contrato que ninguém entende.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmostraDeAudio<'a> {
    /// Um quadro codificado inteiro: um pacote Opus, ou [`DURACAO_DO_QUADRO_MS`] de G.711.
    ///
    /// **Um quadro por chamada.** O pacotizador da libdatachannel não fragmenta: o que entrar
    /// aqui vira exatamente um pacote RTP, então dois quadros numa chamada viram um pacote que
    /// o outro lado decodifica errado.
    pub payload: &'a [u8],
    /// Relógio monotônico da captura, em microssegundos — o mesmo de
    /// [`QuadroCodificado::timestamp_us`], para que áudio e vídeo da mesma sessão possam ser
    /// alinhados.
    pub timestamp_us: u64,
}

/// Um quadro já codificado, indo ou vindo.
///
/// `annexb` é **emprestado**, de propósito: quem envia não perde a posse do buffer de encode, e
/// quem recebe lê direto do buffer de remontagem. Um `Vec` aqui seria uma alocação por quadro no
/// processo de 50 MB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuadroCodificado<'a> {
    /// Um quadro completo em Annex-B, com SPS/PPS junto quando for IDR.
    pub annexb: &'a [u8],
    /// Relógio monotônico da captura, em microssegundos.
    pub timestamp_us: u64,
    pub idr: bool,
}

impl QuadroCodificado<'_> {
    /// Confere se o quadro traz SPS **e** PPS.
    ///
    /// Varre as NAL units procurando os tipos 7 e 8. Custa uma passada pelos start codes, não
    /// pelo quadro inteiro.
    pub fn tem_parametros(&self) -> bool {
        let mut sps = false;
        let mut pps = false;
        for cabecalho in cabecalhos_nal(self.annexb) {
            match cabecalho & 0x1f {
                crate::rtp::nal::SPS => sps = true,
                crate::rtp::nal::PPS => pps = true,
                _ => {}
            }
            if sps && pps {
                return true;
            }
        }
        false
    }
}

/// Percorre um fluxo Annex-B devolvendo o primeiro byte de cada NAL unit.
///
/// Aceita os dois prefixos — `00 00 01` e `00 00 00 01` — porque os dois aparecem no mesmo
/// fluxo: o VideoToolbox e o MediaCodec não concordam sobre qual usar, e um encoder pode
/// misturar.
fn cabecalhos_nal(annexb: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        while i + 3 < annexb.len() {
            if annexb[i] == 0 && annexb[i + 1] == 0 {
                if annexb[i + 2] == 1 {
                    let cabecalho = annexb[i + 3];
                    i += 4;
                    return Some(cabecalho);
                }
                if annexb[i + 2] == 0 && annexb[i + 3] == 1 && i + 4 < annexb.len() {
                    let cabecalho = annexb[i + 4];
                    i += 5;
                    return Some(cabecalho);
                }
            }
            i += 1;
        }
        None
    })
}

/// O que a casca precisa dizer para abrir uma track.
#[derive(Debug, Clone)]
pub struct TrackConfig {
    pub kind: TrackKind,
    /// Rótulo legível, mostrado ao usuário no receptor. Ex.: "Tela do Galaxy A10s".
    pub label: String,
    /// Como codificar, quando `kind` é áudio. `None` em track de vídeo.
    ///
    /// Nasce da **espécie** da track — [`TrackKind::preset_de_audio`] —, que é onde a decisão
    /// mora. Os dois `com_…` abaixo existem para bancada e para a casca que sabe o que está
    /// fazendo; o caminho normal é não tocar neste campo.
    pub preset_audio: Option<PresetDeAudio>,
    /// **Espaçar a saída de vídeo** a no máximo esta taxa, em kbit/s. Zero é desligado — o
    /// padrão, e o de toda casca que não o pedir. Sem efeito numa track de áudio.
    ///
    /// Existe pela medida de 11/09/2026 (`docs/tela-estendida.md`, corrida B): com o emissor no
    /// cabo, a rajada de um IDR chega ao roteador a 1 Gbit/s, e espalhar a saída a 60 Mbit/s levou
    /// a perda de um receptor vizinho de 7,4 % para 0,17 % sem mexer na taxa média. Só foi medido
    /// com o Mac no cabo; num emissor que já sai pelo Wi-Fi, `docs/idr-que-sobrevive.md` dá o custo
    /// de espalhar (perde-se a agregação de quadros do rádio) — por isso não nasce ligado.
    ///
    /// **Tem de ficar bem acima da taxa do vídeo.** O espaçador guarda numa fila o que passa da
    /// verba, e a fila não tem teto: abaixo da taxa média ela só cresce, e a latência junto.
    pub espacamento_kbps: u32,
}

impl TrackConfig {
    pub fn new(kind: TrackKind, label: impl Into<String>) -> Self {
        TrackConfig {
            kind,
            label: label.into(),
            preset_audio: kind.preset_de_audio(),
            espacamento_kbps: 0,
        }
    }

    /// Liga o espaçador de saída a `kbps`. Ver [`TrackConfig::espacamento_kbps`].
    pub fn com_espacamento(mut self, kbps: u32) -> Self {
        self.espacamento_kbps = kbps;
        self
    }

    /// Troca o preset inteiro. Sem efeito numa track de vídeo.
    pub fn com_preset_de_audio(mut self, preset: PresetDeAudio) -> Self {
        self.preset_audio = Some(preset);
        self
    }

    /// Troca **só o codec**, mantendo o resto do preset da espécie.
    ///
    /// É o que a bancada usa para pedir [`CodecDeAudio::Pcmu`]: G.711 é uma tabela de consulta
    /// de 8 bits e não precisa de biblioteca nenhuma, o que permite provar o caminho de áudio
    /// sem depender de um libopus compilado.
    pub fn com_codec_de_audio(mut self, codec: CodecDeAudio) -> Self {
        let base = self.preset_audio.or_else(|| self.kind.preset_de_audio());
        self.preset_audio = base.map(|p| PresetDeAudio { codec, ..p });
        self
    }
}

/// Perfil H.264 anunciado no SDP.
///
/// `42e034` é Constrained Baseline, **nível 5.2** — `0x34` = 52, o menor nível que comporta 4K a 60 fps. `packetization-mode=1` é **obrigatório**: sem
/// ele o outro lado tem o direito de recusar FU-A, e sem FU-A nenhum quadro maior que a MTU
/// atravessa.
///
/// # Por que 4.0, e não o 3.1 que estava aqui
///
/// **Subido em 01/09/2026, com medida.** O 3.1 tem `MaxFS` 3600 e travava tudo em 720p. O 4.0 tem
/// `MaxFS` 8192, e 1920x1080 ocupa **8160 macroblocos** — cabe com 32 de folga. A 30 fps são
/// 244800 contra o `MaxMBPS` de 245760: cabe por **0,4 %**. Não é escolha de número redondo, é o
/// menor nível da norma em que 1080p30 existe.
///
/// O que a bancada mediu antes de mexer (ver `docs/bancada.md`):
///
/// - **O cabo não é o gargalo.** 935 Mbps medidos num S24 Ultra; 1080p30 usaria 0,9 % disso.
/// - **O codificador do aparelho não é o gargalo.** O S24 faz 1080p em H.264 a 75–113 fps.
/// - **O que morde é este nível**, que consumia meio por cento do cabo.
///
/// # O risco que sobe junto, e é nomeado
///
/// A **abertura fria** volta a ser cara. `docs/joelho-da-perda.md` mediu 163 pacotes no conjunto
/// de parâmetros a 1080p, com 69,33 % de perda reproduzida três vezes — e o teto de 28/08 é que
/// tinha derrubado isso. O que **não** volta é o regime: o quadro-chave estabilizou em **15
/// pacotes** de 12 a 24 fps, e a frase "o IDR é o único objeto grande o bastante para estourar o
/// joelho" morreu com aquele teto. Quem cobre a abertura é a quinta porta (250 ms contra 4,3–6,9
/// s de recuperação).
///
/// # O que **não** subiu junto
///
/// O **teto de taxa** continua em 4 Mbps, literal em cada casca. 1080p tem 2,27x os pixels de
/// 720p, então é o mesmo orçamento de bits para mais que o dobro de área. Para espelhamento de
/// tela isso é defensável — conteúdo de tela comprime muito bem —, para câmera é apertado. Subir
/// esse número é outra mudança, e ela toca todas as cascas.
pub const PERFIL_H264: &str =
    "profile-level-id=42e034;packetization-mode=1;level-asymmetry-allowed=1";

/// Tipo de payload RTP dinâmico usado para vídeo. Cada linha `m=` tem o seu espaço, então o
/// mesmo número serve para todas as tracks.
pub const PAYLOAD_TYPE_VIDEO: u8 = 96;

/// Maior fragmento FU-A, em bytes.
///
/// 1188 é o padrão da libdatachannel, escolhido para caber num datagrama de 1200 bytes com o
/// cabeçalho RTP e a expansão do SRTP. Mexer aqui sem medir MTU de caminho é como se pede
/// fragmentação de IP — que é a forma mais barata de transformar 1% de perda em 30%.
pub const MAX_FRAGMENTO: u16 = 1188;

/// De quanto em quanto tempo o espaçador solta um lote, em ms. Ver
/// [`TrackConfig::espacamento_kbps`].
///
/// Cada lote é um intervalo de verba: a 60 Mbit/s, 2 ms são 15 KB, ~12 pacotes colados — contra os
/// ~400 de um IDR inteiro. Mais curto aproxima do `dummynet` da medida (um pacote por vez), mas o
/// espaçador só credita **um** intervalo por volta, e um relógio que acorda atrasado passa a
/// cortar a taxa; mais longo devolve a rajada.
pub const INTERVALO_DO_ESPACADOR_MS: i32 = 2;

/// Quantos pacotes RTP a libdatachannel vai numerar para esta unidade de acesso Annex-B.
///
/// # Por que isto existe
///
/// O receptor sabe quantos pacotes foram **numerados** (`packets_seen + packets_missing`, porque
/// quem numera é o pacotizador do emissor). Até 2026-08-29 ninguém sabia quantos o emissor tinha
/// **entregado**, e sem esse par a dívida 30 — o `ret` sobrescrito de `impl/track.cpp:187-199` —
/// era teoria: não havia como dizer se um buraco de sequência no receptor correspondia a um
/// fragmento que nunca saiu.
///
/// Esta função fecha o par. Ela replica, byte a byte, o que
/// `H264RtpPacketizer::fragment` → `NalUnit::GenerateFragments` da 0.23.2 faz com o mesmo buffer.
///
/// # A regra, e ela NÃO é `ceil(bytes / 1188)`
///
/// A conta que circulava nesta bancada — `ceil(total / 1188)`, que é o que
/// `apps/windows/src/transmissao.rs::medida_de_quadro_chave` usa — **subestima**, por duas razões
/// independentes:
///
/// 1. **A divisão é por NAL, não pela unidade de acesso.** Um quadro com AUD + SEI + fatia são
///    três NALs, e cada NAL pequeno vira um pacote inteiro. Um IDR carrega ainda SPS e PPS.
/// 2. **A `generateFragments` empareja os fragmentos e depois desconta o cabeçalho FU-A, o que
///    frequentemente acrescenta um fragmento.** Ela calcula `n = ceil(tam / 1188)`, reparte em
///    `m = ceil(tam / n)`, **subtrai 2** (indicador + cabeçalho FU-A) e só então corta a carga
///    (que é `tam - 1`, sem o cabeçalho do NAL) em pedaços de `m - 2`. Para um NAL de 5.958 B a
///    conta ingênua dá 6 e a biblioteca produz **7**.
///
/// Medido no fio: com a conta ingênua o emissor previa ~6 pacotes por quadro não-IDR, e o
/// receptor contava 9,6. A diferença é esta função.
pub fn pacotes_da_unidade(annexb: &[u8]) -> u64 {
    nals_annexb(annexb).map(pacotes_do_nal).sum()
}

/// Fragmentos que um NAL de `tam` bytes produz, pela regra de `NalUnit::generateFragments`.
fn pacotes_do_nal(tam: usize) -> u64 {
    if tam == 0 {
        return 0;
    }
    if tam <= MAX_FRAGMENTO as usize {
        return 1;
    }
    // `fragments_count = ceil(size / maxFragmentSize)`; `maxFragmentSize = ceil(size / n) - 2`.
    let n = tam.div_ceil(MAX_FRAGMENTO as usize);
    let m = tam.div_ceil(n).saturating_sub(2);
    if m == 0 {
        // Não acontece com 1188, mas um `m` de zero seria laço infinito na biblioteca; aqui só
        // não podemos dividir por zero.
        return n as u64;
    }
    // A carga é o NAL sem o byte de cabeçalho, e o laço avança de `m` até acabar.
    (tam - 1).div_ceil(m) as u64
}

/// Divide uma unidade de acesso Annex-B em NALs, do jeito que `H264RtpPacketizer::splitFrame`
/// divide com `Separator::StartSequence`: aceita prefixo de 3 **ou** 4 bytes, e o último NAL vai
/// até o fim do buffer.
fn nals_annexb(dados: &[u8]) -> impl Iterator<Item = usize> + '_ {
    let mut inicios: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= dados.len() {
        if dados[i] == 0 && dados[i + 1] == 0 && dados[i + 2] == 1 {
            inicios.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut tamanhos: Vec<usize> = Vec::with_capacity(inicios.len());
    for (k, &ini) in inicios.iter().enumerate() {
        // O NAL termina onde começa o prefixo do próximo. Um prefixo de 4 bytes é um de 3 com um
        // zero na frente, e esse zero pertence ao prefixo, não ao NAL anterior.
        let fim = match inicios.get(k + 1) {
            Some(&prox) => {
                let mut fim = prox - 3;
                if fim > ini && dados[fim - 1] == 0 {
                    fim -= 1;
                }
                fim
            }
            None => dados.len(),
        };
        tamanhos.push(fim.saturating_sub(ini));
    }
    tamanhos.into_iter()
}

#[cfg(test)]
mod testes_de_pacotes {
    use super::*;

    fn au(nals: &[usize]) -> Vec<u8> {
        let mut v = Vec::new();
        for &n in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend(std::iter::repeat_n(0x41u8, n));
        }
        v
    }

    #[test]
    fn nal_pequeno_vira_um_pacote() {
        assert_eq!(pacotes_do_nal(1), 1);
        assert_eq!(pacotes_do_nal(1188), 1);
    }

    /// O caso que derruba a conta ingênua: 5.958 B dão **7** e não 6.
    #[test]
    fn a_conta_ingenua_subestima() {
        let tam: usize = 5958;
        assert_eq!(tam.div_ceil(MAX_FRAGMENTO as usize), 6);
        assert_eq!(pacotes_do_nal(tam), 7);
    }

    #[test]
    fn separador_de_tres_e_de_quatro_bytes_dao_o_mesmo_nal() {
        let quatro = au(&[100]);
        let tres = {
            let mut v = vec![0, 0, 1];
            v.extend(std::iter::repeat_n(0x41u8, 100));
            v
        };
        assert_eq!(nals_annexb(&quatro).collect::<Vec<_>>(), vec![100]);
        assert_eq!(nals_annexb(&tres).collect::<Vec<_>>(), vec![100]);
    }

    /// Um quadro real: AUD + SEI + fatia. Os dois NALs pequenos custam um pacote cada.
    #[test]
    fn cada_nal_pequeno_custa_um_pacote_inteiro() {
        let dados = au(&[2, 20, 5958]);
        assert_eq!(nals_annexb(&dados).collect::<Vec<_>>(), vec![2, 20, 5958]);
        assert_eq!(pacotes_da_unidade(&dados), 1 + 1 + 7);
    }

    /// Um IDR com conjunto de parâmetros: SPS + PPS + fatia grande.
    #[test]
    fn idr_com_parametros() {
        let dados = au(&[27, 8, 17200]);
        // 17.200 B: n = 15, m = ceil(17200/15) - 2 = 1145, ceil(17199/1145) = 16.
        assert_eq!(pacotes_do_nal(17200), 16);
        assert_eq!(pacotes_da_unidade(&dados), 18);
    }
}

#[cfg(feature = "webrtc")]
mod webrtc {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::jitter::Politica;
    use crate::portao::{Barreira, Portao, PRAZO_DA_BARREIRA};
    use crate::relogio::{DeslocamentoDeCaptura, RelogioDaSessao, RetratoDoRelogio};
    use crate::reproducao::{OpcoesDeReproducao, ReproducaoPuxada};
    use crate::rtp::{
        micros_para_carimbo, BasePublicada, Contadores, Depacotizador, DepacotizadorDeAudio,
        QuadroDeAudio,
    };

    /// Quem consome o áudio de uma track receptora. As duas portas são **exclusivas**: a
    /// empurrada (`ao_receber_audio`, `quall_track_on_audio`) e a puxada
    /// (`reproducao_puxada`, `quall_audio_playout_new`).
    ///
    /// O estado mora aqui, e não na fronteira C, porque o Windows chama o núcleo em Rust direto
    /// e precisa da mesma exclusão.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ModoDeAudio {
        Nenhum,
        Empurrado,
        Puxado,
    }

    fn traduzir(e: quall_rtc::RtcError) -> Error {
        Error::Transport(e.0)
    }

    /// O portão da sessão a que esta track pertence. Ver [`crate::portao`].
    ///
    /// # Por que ele é obrigatório, e não zelo (achado no Dell G3, 2026-08-23)
    ///
    /// Quando a [`crate::transport::Session`] morre, ela chama `rtcDeleteTrack` em todas as
    /// tracks — é o conserto das dívidas 4, 14 e 21. A partir daí o id **não existe mais** para a
    /// libdatachannel, e a fronteira C entrega handles de track que sobrevivem à sessão: o
    /// contrato de `quall_session_close` diz, com todas as letras, que os contadores continuam
    /// legíveis depois dela.
    ///
    /// Chamar a API C com um id morto **trava o processo no Windows**. Toda função da API C busca
    /// o objeto com `getTrack`, que lança `std::invalid_argument` de dentro do `lock_guard` do
    /// mutex global do `capi.cpp`; no Windows a chamada seguinte que precise desse mutex bloqueia
    /// para sempre, sem CPU e sem log. Medido: `rtcCreatePeerConnection` pendurado por mais de
    /// 45 s com 0,15 s de CPU.
    ///
    /// # Por que virou portão, e não continuou uma `AtomicBool` (2026-08-26)
    ///
    /// A bandeira era um `Arc<AtomicBool>` e cada função conferia antes de tocar a API C. Isso
    /// deixava **conferir e usar em dois passos**: a thread de captura via a bandeira de pé,
    /// o `Drop` da sessão baixava a bandeira e chamava `rtcDeleteTrack`, e só então a captura
    /// entregava o quadro — com o id já morto. É a armadilha do Windows por uma porta que a
    /// própria bandeira abria.
    ///
    /// O [`Portao`] fecha essa janela sem mudar o custo do caminho do quadro: quem vai tocar a
    /// API C pega um passe, e o `Drop` da sessão **espera** os passes serem devolvidos antes de
    /// destruir qualquer track. De quebra é a barreira que faltava ao `quall_session_close`, que
    /// é a dívida que este módulo veio pagar. `esta_viva()` continua existindo, com o mesmo nome
    /// e o mesmo custo, para quem só quer relatar estado.
    pub(crate) type SessaoViva = Arc<Portao>;

    /// Onde o tratador de IDR da casca fica guardado.
    ///
    /// `Arc` por dentro, e não `Box`, para poder ser clonado para fora do cadeado antes de ser
    /// chamado: chamar com o cadeado na mão travaria se o tratador voltasse a mexer na track, que
    /// é a reação natural a um PLI.
    type TratadorDeIdr = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;

    /// Onde o tratador de quadro da casca fica guardado, do lado que recebe.
    ///
    /// Mesma forma do [`TratadorDeIdr`] e pelo mesmo motivo: clonar para fora do cadeado antes
    /// de chamar, e poder trocar por `None` — que é o desregistro.
    type TratadorDeQuadro =
        Arc<Mutex<Option<Arc<dyn for<'a> Fn(QuadroCodificado<'a>) + Send + Sync>>>>;

    /// O par do [`TratadorDeQuadro`] no caminho do áudio. O segundo argumento é a chegada do
    /// pacote, em µs do relógio da sessão: a porta puxada precisa dela.
    type TratadorDeAudio =
        Arc<Mutex<Option<Arc<dyn for<'a> Fn(QuadroDeAudio<'a>, u64) + Send + Sync>>>>;

    /// Quem remonta os pacotes desta track: o de vídeo, ou o de áudio.
    ///
    /// Um enum e não dois tipos de `TrackReceptor` de propósito. `Ready::tracks` é um
    /// `Vec<TrackEmissor>` e `Session::proxima_track` devolve `Option<TrackReceptor>`; trocar
    /// esses tipos por enums quebraria a compilação do app Windows, do plugin de OBS e das
    /// cascas Android e iOS de uma vez, para ganhar uma checagem que o `Result` já dá. A
    /// segurança aqui é de execução, e o erro diz qual função usar.
    ///
    /// **Sobre o `allow`:** desde o histograma de cortes (`Contadores::cortes_por_faixa`,
    /// 04/09) a variante de vídeo tem 416 bytes contra 176 da de áudio, e o clippy sugere
    /// `Box`. Boxar aqui poria uma indireção **no caminho de cada pacote RTP**, que é o mais
    /// quente do receptor, para economizar 240 bytes por track de áudio — ruído absoluto contra
    /// os ~50 MB que o contrato de memória da extension do iOS persegue. O warning está certo
    /// sobre o fato e errado sobre a troca.
    #[allow(clippy::large_enum_variant)]
    enum Remontador {
        Video(Depacotizador),
        Audio(DepacotizadorDeAudio),
    }

    impl Remontador {
        fn contadores(&self) -> Contadores {
            match self {
                Remontador::Video(d) => d.contadores(),
                Remontador::Audio(d) => d.contadores(),
            }
        }

        /// A base do `timestamp_us` entregue, publicada num atômico: o primeiro quadro entregue
        /// no vídeo, o primeiro pacote no áudio. Ver [`crate::rtp::BasePublicada`].
        fn base_publicada(&self) -> BasePublicada {
            match self {
                Remontador::Video(d) => d.base_publicada(),
                Remontador::Audio(d) => d.base_publicada(),
            }
        }
    }

    thread_local! {
        /// O endereço do depacotizador cuja bomba esta thread está rodando agora, com o cadeado
        /// na mão; 0 fora de qualquer bomba. Quem precisa daquele cadeado, e é chamado pela casca
        /// de dentro do tratador, recusa em vez de travar a própria thread.
        static NA_BOMBA: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// Marca "esta thread está na bomba deste depacotizador" enquanto vive.
    struct DentroDaBomba {
        anterior: usize,
    }

    impl DentroDaBomba {
        fn entrar(endereco: usize) -> Self {
            DentroDaBomba {
                anterior: NA_BOMBA.with(|n| n.replace(endereco)),
            }
        }

        fn esta_em(endereco: usize) -> bool {
            NA_BOMBA.with(|n| n.get() == endereco)
        }
    }

    impl Drop for DentroDaBomba {
        fn drop(&mut self) {
            NA_BOMBA.with(|n| n.set(self.anterior));
        }
    }

    /// Informa ao relógio da sessão a base publicada, se ainda não informou. Sem cadeado de
    /// depacotizador nenhum.
    fn informar_base(
        publicada: &BasePublicada,
        informada: &AtomicBool,
        relogio: &RelogioDaSessao,
        indice: usize,
    ) {
        if informada.load(Ordering::Acquire) {
            return;
        }
        if let Some(b) = publicada.ler() {
            relogio.fixar_base(indice, b);
            informada.store(true, Ordering::Release);
        }
    }

    impl Remontador {

        /// Crava a profundidade do anel de reordenação e desliga o ajuste automático. Ver
        /// [`crate::rtp::Depacotizador::definir_profundidade_de_reordenacao`].
        fn cravar_anel(&mut self, pacotes: usize) -> bool {
            match self {
                Remontador::Video(d) => {
                    d.definir_profundidade_de_reordenacao(pacotes);
                    true
                }
                // O áudio não tem anel: ele tem jitter buffer, e ele mora na casca.
                Remontador::Audio(_) => false,
            }
        }
    }

    /// Lê o codec de áudio no `a=rtpmap` da descrição da track.
    ///
    /// # Por que isto não pode ser adivinhado
    ///
    /// A única coisa que o codec muda **dentro do núcleo** é a taxa do relógio RTP: 48 kHz no
    /// Opus, 8 kHz no G.711. Errar por um fator de seis não produz erro nenhum — produz carimbos
    /// seis vezes errados, e o sintoma é áudio que parece acelerar ou arrastar sem nenhum
    /// contador acusando. Por isso o codec vem do SDP, que é onde o outro lado o declarou, e não
    /// de um padrão nosso.
    ///
    /// A linha que a libdatachannel escreve é `a=rtpmap:111 opus/48000/2` (ver
    /// `Description::Audio::addAudioCodec`, que força `/48000/2` para Opus e `/8000/1` para
    /// G.711).
    fn codec_do_sdp(descricao: &str) -> Option<CodecDeAudio> {
        descricao.lines().find_map(|linha| {
            let valor = linha.trim().strip_prefix("a=rtpmap:")?;
            // `111 opus/48000/2` → `opus`
            let nome = valor.split_whitespace().nth(1)?.split('/').next()?;
            let nome = nome.to_ascii_lowercase();
            [CodecDeAudio::Opus, CodecDeAudio::Pcmu]
                .into_iter()
                .find(|c| c.nome_rtpmap() == nome)
        })
    }

    /// Codifica o rótulo para caber num `msid` do SDP.
    ///
    /// # Por que não dá para escrever o rótulo cru
    ///
    /// A gramática do SDP define `msid` como um **token**, e token não tem espaço. A
    /// libdatachannel escreve `a=msid:Tela de teste screen` sem reclamar, mas o outro lado
    /// reserializa e o rótulo volta truncado — medido aqui em 2026-08-21, numa sessão em C entre
    /// dois processos: mandou `"Tela de teste"`, chegou `"Tela de"`. O analisador cortou no
    /// espaço, leu `"de"` como o track id e jogou o resto fora.
    ///
    /// Então o rótulo vai percent-encoded. `%` **está** no conjunto de caracteres de token do
    /// SDP, o que faz da codificação por porcento a escolha que não briga com a gramática. Fica
    /// preservado inclusive acento — "Câmera" vira `C%C3%A2mera` e volta inteiro.
    fn codificar_rotulo(rotulo: &str) -> String {
        const SEGUROS: &[u8] = b"-._~";
        let mut saida = String::with_capacity(rotulo.len());
        for byte in rotulo.as_bytes() {
            if byte.is_ascii_alphanumeric() || SEGUROS.contains(byte) {
                saida.push(*byte as char);
            } else {
                saida.push('%');
                saida.push_str(&format!("{byte:02X}"));
            }
        }
        saida
    }

    /// Desfaz [`codificar_rotulo`].
    ///
    /// Um rótulo que não é percent-encoded — porque veio de outra implementação — passa
    /// inalterado: `%` sem dois dígitos hex atrás é copiado como está, em vez de virar erro.
    /// Rótulo é texto de tela; recusar a track por causa dele seria desproporcional.
    fn decodificar_rotulo(texto: &str) -> String {
        let bytes = texto.as_bytes();
        let mut saida = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = &texto[i + 1..i + 3];
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    saida.push(b);
                    i += 3;
                    continue;
                }
            }
            saida.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&saida).into_owned()
    }

    /// Tira o rótulo legível do `a=msid` da descrição da track.
    ///
    /// A libdatachannel escreve `a=msid:<rótulo> <track id>`, e o Quall usa o `mid` como track
    /// id — então o rótulo é a linha inteira menos o `mid` do fim. Medido numa track de teste:
    ///
    /// ```text
    /// a=msid:Tela de teste screen
    /// ```
    ///
    /// Note o rótulo **com espaços**. A gramática do SDP não os prevê no `msid`, e a
    /// libdatachannel escreve assim mesmo; por isso o corte é pelo sufixo conhecido, e não por
    /// `split_whitespace`, que quebraria "Tela de teste" em três.
    ///
    /// Sem `a=msid` — outra implementação do outro lado —, quem chama cai para o `mid`.
    fn rotulo_do_sdp(descricao: &str, mid: &str) -> Option<String> {
        let valor = descricao
            .lines()
            .find_map(|linha| linha.trim().strip_prefix("a=msid:"))?;
        let rotulo = valor
            .strip_suffix(mid)
            .map(str::trim_end)
            .unwrap_or(valor)
            .trim();
        (!rotulo.is_empty()).then(|| decodificar_rotulo(rotulo))
    }

    /// Uma track que **sai**. A casca captura, codifica e entrega aqui.
    pub struct TrackEmissor {
        track: quall_rtc::Track,
        kind: TrackKind,
        /// `Some` numa track de áudio, e é ele que decide qual das duas funções de envio vale.
        preset_audio: Option<PresetDeAudio>,
        label: String,
        enviados: AtomicU64,
        bytes_enviados: AtomicU64,
        /// Pacotes RTP que a libdatachannel vai numerar para o que já entregamos. Ver
        /// [`super::pacotes_da_unidade`] — é o lado do emissor do par que decide a dívida 30.
        pacotes_entregues: AtomicU64,
        idrs: AtomicU64,
        idrs_sem_parametros: AtomicU64,
        /// Compartilhado com o tratador de PLI, que roda numa thread da libdatachannel e não
        /// pode emprestar `self`.
        pedidos_de_idr: Arc<AtomicU64>,
        /// Bandeira levantada a cada PLI/FIR e baixada por [`TrackEmissor::pegar_pedido_de_idr`].
        pendente: Arc<AtomicBool>,
        /// Tratador da casca, se ela registrou um. Ver [`TratadorDeIdr`].
        tratador: TratadorDeIdr,
        /// Ver [`SessaoViva`]. Baixada pelo `Drop` da sessão, antes do `rtcDeleteTrack`.
        viva: SessaoViva,
    }

    impl TrackEmissor {
        /// Abre uma track de saída numa conexão. Uso interno de
        /// [`crate::transport::Session::adicionar_track`].
        pub(crate) fn abrir(
            pc: quall_rtc::PcId,
            cfg: &TrackConfig,
            viva: SessaoViva,
        ) -> Result<Self> {
            // **A espécie decide, e a tabela responde.** Nada aqui pergunta "é microfone?" ou
            // "é áudio do sistema?": pergunta se há preset. Uma espécie nova de áudio entra em
            // `TrackKind::preset_de_audio` e esta função não muda uma linha.
            let audio: Option<PresetDeAudio> = if cfg.kind.e_audio() {
                // O `or_else` cobre a casca que zerou o campo à mão numa track de áudio: cair
                // no preset da espécie é melhor que abrir a track como vídeo em silêncio.
                cfg.preset_audio.or_else(|| cfg.kind.preset_de_audio())
            } else {
                None
            };

            // O codec é o campo que faz o `rtcAddTrackEx` escolher entre `m=video` e `m=audio`.
            // Ver `quall_rtc::Codec`.
            let (codec, payload_type, perfil) = match audio {
                None => (
                    quall_rtc::Codec::H264,
                    PAYLOAD_TYPE_VIDEO,
                    PERFIL_H264.to_string(),
                ),
                Some(preset) => (
                    match preset.codec {
                        CodecDeAudio::Opus => quall_rtc::Codec::Opus,
                        CodecDeAudio::Pcmu => quall_rtc::Codec::Pcmu,
                    },
                    preset.codec.payload_type(),
                    // Montado do preset, não escrito à mão. Ver `PresetDeAudio::fmtp`.
                    preset.fmtp(),
                ),
            };

            let track = quall_rtc::Track::adicionar(
                pc,
                &quall_rtc::TrackInit {
                    direcao: quall_rtc::Direcao::SendOnly,
                    codec,
                    payload_type: i32::from(payload_type),
                    ssrc: cfg.kind.ssrc(),
                    mid: cfg.kind.mid().to_string(),
                    // Percent-encoded: ver `codificar_rotulo`. Sem isto o rótulo volta
                    // truncado no primeiro espaço do outro lado.
                    nome: codificar_rotulo(&cfg.label),
                    perfil,
                },
            )
            .map_err(traduzir)?;

            match audio {
                Some(preset) => {
                    track
                        .pacotizador_audio(
                            codec,
                            &quall_rtc::PacketizerInit {
                                ssrc: cfg.kind.ssrc(),
                                cname: "quall".into(),
                                payload_type,
                                clock_rate: preset.codec.relogio_hz(),
                                // Ignorado: o pacotizador de áudio não fragmenta.
                                max_fragment_size: 0,
                            },
                        )
                        .map_err(traduzir)?;
                }
                None => {
                    track
                        .pacotizador_h264(&quall_rtc::PacketizerInit {
                            ssrc: cfg.kind.ssrc(),
                            cname: "quall".into(),
                            payload_type: PAYLOAD_TYPE_VIDEO,
                            clock_rate: crate::rtp::RELOGIO_VIDEO_HZ,
                            max_fragment_size: MAX_FRAGMENTO,
                        })
                        .map_err(traduzir)?;
                }
            }

            // Relator de RTCP SR: barato, e é o que dá ao receptor a relação entre o relógio RTP
            // e o tempo de parede. **Com áudio na sessão ele deixa de ser luxo**: é a única
            // coisa que liga o relógio de 90 kHz do vídeo ao de 48 kHz do áudio num tempo comum,
            // e sem essa ligação não há sincronia de lábios possível — as duas tracks têm bases
            // de carimbo independentes e nada mais as relaciona.
            track.relator_rtcp().map_err(traduzir)?;

            // **Sem** `rtcChainRtcpNackResponder`, e é decisão, não esquecimento: o respondedor
            // de NACK guarda os últimos N pacotes enviados para poder retransmitir. Isso é
            // exatamente a "cópia guardada" que o contrato proíbe, e retransmissão acrescenta
            // latência num produto que persegue < 50 ms. A recuperação de perda aqui é o IDR
            // pedido por PLI, que custa um quadro e não uma fila.
            //
            // No áudio a recuperação não é o IDR — não existe quadro-chave de áudio — e sim o
            // FEC embutido do Opus, declarado no preset. Ver `PRESET_MICROFONE` por que ele **não** é o
            // mesmo trato que o NACK.

            let pedidos_de_idr = Arc::new(AtomicU64::new(0));
            let pendente = Arc::new(AtomicBool::new(false));
            let tratador: TratadorDeIdr = Arc::new(Mutex::new(None));

            // Um único tratador encadeado, registrado aqui e não em `ao_pedir_idr`. Ele conta,
            // levanta a bandeira e só então chama o que a casca registrou — nessa ordem, para
            // que o número e a bandeira existam mesmo que o tratador da casca demore.
            //
            // **Só no vídeo.** Numa track de áudio, encadear o tratador de PLI seria instalar um
            // caminho que nunca dispara: o receptor não tem como pedir quadro-chave de áudio
            // (`Track::requestKeyframe` da libdatachannel sai fora quando o tipo não é "video"),
            // e um gancho que não dispara é convite a alguém concluir que o pedido "não está
            // chegando" quando ele nunca foi enviado.
            if audio.is_none() {
                let contador = Arc::clone(&pedidos_de_idr);
                let bandeira = Arc::clone(&pendente);
                let da_casca = Arc::clone(&tratador);
                let portao = Arc::clone(&viva);
                track
                    .ao_pedir_idr(move || {
                        contador.fetch_add(1, Ordering::Relaxed);
                        bandeira.store(true, Ordering::Relaxed);
                        // **A barreira do `quall_session_close` mora nesta linha.** O código da
                        // casca só é chamado com um passe do portão da sessão na mão; enquanto
                        // ele durar, o fechamento espera. Medido antes disto: o `Drop` da sessão
                        // voltava em 0,49 ms com o tratador de PLI ainda dentro da casca, que é
                        // exatamente onde ela liberaria o `user_data`.
                        let Some(_passe) = portao.entrar() else {
                            return;
                        };
                        let f = da_casca.lock().ok().and_then(|g| g.clone());
                        if let Some(f) = f {
                            f();
                        }
                    })
                    .map_err(traduzir)?;
            }

            // **O espaçador é o último da corrente**, e é a única fila do caminho de envio. Ele
            // entrega os pacotes direto ao transporte, e um tratador encadeado depois dele não
            // os veria. Desligado por padrão. Ver `TrackConfig::espacamento_kbps`.
            if audio.is_none() && cfg.espacamento_kbps > 0 {
                track
                    .espacar(
                        f64::from(cfg.espacamento_kbps) * 1000.0,
                        INTERVALO_DO_ESPACADOR_MS,
                    )
                    .map_err(traduzir)?;
            }

            Ok(TrackEmissor {
                track,
                kind: cfg.kind,
                preset_audio: audio,
                label: cfg.label.clone(),
                enviados: AtomicU64::new(0),
                bytes_enviados: AtomicU64::new(0),
                pacotes_entregues: AtomicU64::new(0),
                idrs: AtomicU64::new(0),
                idrs_sem_parametros: AtomicU64::new(0),
                pedidos_de_idr,
                pendente,
                tratador,
                viva,
            })
        }

        /// Empacota e solta um quadro.
        ///
        /// Volta quando os pacotes RTP já foram entregues ao transporte — ou, com o espaçador
        /// ligado ([`TrackConfig::espacamento_kbps`]), à fila dele, que os solta no ritmo pedido.
        /// Erro aqui é a track ainda não estar aberta (normal enquanto o ICE não fechou) ou o
        /// transporte ter caído; nos dois casos a casca descarta o quadro e segue — nunca
        /// enfileira.
        pub fn enviar_quadro(&self, quadro: QuadroCodificado<'_>) -> Result<()> {
            if self.preset_audio.is_some() {
                return Err(Error::Invalid(
                    "esta é uma track de áudio; use `enviar_audio`".into(),
                ));
            }
            if quadro.annexb.is_empty() {
                return Err(Error::Invalid("quadro vazio".into()));
            }
            // A conferência de SPS/PPS acontece antes do envio, mas os contadores só sobem
            // depois que o quadro sai. Contar antes fazia o relatório do emissor não fechar com
            // o do receptor enquanto a track ainda não tinha aberto — e um par de números que
            // não bate é a primeira coisa que faz alguém desconfiar da medição inteira.
            let sem_parametros = quadro.idr && !quadro.tem_parametros();
            // O passe fica na mão até o quadro sair. Não é zelo: conferir uma bandeira e só
            // depois chamar a API C deixava o `rtcDeleteTrack` do `Drop` da sessão caber no
            // meio, e um id morto na API C **trava o processo no Windows**.
            let _passe = self.entrar()?;

            self.track
                .carimbo_rtp(micros_para_carimbo(quadro.timestamp_us))
                .map_err(traduzir)?;
            self.track.enviar(quadro.annexb).map_err(traduzir)?;

            self.enviados.fetch_add(1, Ordering::Relaxed);
            self.bytes_enviados
                .fetch_add(quadro.annexb.len() as u64, Ordering::Relaxed);
            // Contado **depois** do envio, junto dos outros, pelo mesmo motivo deles: contar
            // antes fazia o relatório do emissor não fechar com o do receptor enquanto a track
            // ainda não tinha aberto.
            self.pacotes_entregues
                .fetch_add(super::pacotes_da_unidade(quadro.annexb), Ordering::Relaxed);
            if quadro.idr {
                self.idrs.fetch_add(1, Ordering::Relaxed);
                if sem_parametros {
                    // Contado, não recusado. Ver `idrs_sem_parametros`.
                    self.idrs_sem_parametros.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(())
        }

        /// Empacota e solta **um** quadro de áudio.
        ///
        /// Mesmo contrato do [`TrackEmissor::enviar_quadro`]: sem fila, sem cópia guardada, volta
        /// quando o pacote já foi entregue ao transporte. Erro aqui é a track ainda não estar
        /// aberta (normal enquanto o ICE não fechou) ou o transporte ter caído; nos dois casos a
        /// casca **descarta o quadro e segue**.
        ///
        /// Descartar é mesmo o certo, e no áudio custa menos que no vídeo: 20 ms de som que não
        /// saíram são 20 ms que o outro lado oculta com o FEC ou com a interpolação do decoder.
        /// Enfileirar seria pior — áudio atrasado não tem valor, porque o instante em que ele
        /// deveria ter tocado já passou e o DAC não espera.
        ///
        /// # Um quadro por chamada
        ///
        /// O pacotizador de áudio da libdatachannel **não fragmenta**: o que entrar aqui vira
        /// exatamente um pacote RTP. Mandar dois quadros de Opus concatenados numa chamada
        /// produz um pacote que o decodificador do outro lado lê errado — e sem erro nenhum no
        /// caminho, porque para o RTP é só um payload maior.
        pub fn enviar_audio(&self, amostra: AmostraDeAudio<'_>) -> Result<()> {
            let Some(preset) = self.preset_audio else {
                return Err(Error::Invalid(
                    "esta é uma track de vídeo; use `enviar_quadro`".into(),
                ));
            };
            if amostra.payload.is_empty() {
                return Err(Error::Invalid("quadro de áudio vazio".into()));
            }
            // O passe fica na mão até o pacote sair. Ver `enviar_quadro`: conferir uma bandeira
            // e só depois chamar a API C deixava o `rtcDeleteTrack` caber no meio, e um id morto
            // na API C **trava o processo no Windows**.
            let _passe = self.entrar()?;

            self.track
                .carimbo_rtp(crate::rtp::micros_para_carimbo_em(
                    amostra.timestamp_us,
                    preset.codec.relogio_hz(),
                ))
                .map_err(traduzir)?;
            self.track.enviar(amostra.payload).map_err(traduzir)?;

            self.enviados.fetch_add(1, Ordering::Relaxed);
            self.bytes_enviados
                .fetch_add(amostra.payload.len() as u64, Ordering::Relaxed);
            Ok(())
        }

        /// O codec desta track, ou `None` se ela for de vídeo.
        pub fn codec_de_audio(&self) -> Option<CodecDeAudio> {
            self.preset_audio.map(|p| p.codec)
        }

        /// O preset com que esta track foi aberta, ou `None` se ela for de vídeo.
        pub fn preset_de_audio(&self) -> Option<PresetDeAudio> {
            self.preset_audio
        }

        /// Bytes de mídia entregues ao pacotizador — sem cabeçalho de RTP, de SRTP nem de UDP.
        ///
        /// É o numerador da taxa de bits real do fluxo. No áudio ele importa mais que no vídeo:
        /// o Opus é de taxa variável e o número de quadros não diz nada sobre quanta banda a
        /// track está usando.
        pub fn bytes_enviados(&self) -> u64 {
            self.bytes_enviados.load(Ordering::Relaxed)
        }

        /// **Pacotes RTP que entregamos à libdatachannel**, pela regra do pacotizador dela.
        ///
        /// É o lado do emissor do par que decide a dívida 30. O receptor conta os **numerados**
        /// (`packets_seen + packets_missing`); este contador diz quantos deveriam ter sido
        /// numerados. Se os dois batem, todo buraco de sequência do receptor corresponde a um
        /// pacote que a libdatachannel realmente numerou e mandou ao socket — e a perda é depois
        /// disso. Se o do receptor for **maior**, sobra numeração sem entrega, que é a assinatura
        /// da dívida.
        ///
        /// Zero para track de áudio: o pacotizador de áudio não fragmenta e cada quadro vira
        /// exatamente um pacote, que já é o que `enviados` conta.
        pub fn pacotes_entregues(&self) -> u64 {
            self.pacotes_entregues.load(Ordering::Relaxed)
        }

        /// Registra o tratador do pedido de IDR do receptor.
        ///
        /// Chamado quando chega **PLI ou FIR**. A casca responde forçando um IDR pelo meio que a
        /// plataforma dela permitir — e no Windows, hoje, isso significa recriar o MFT, com o
        /// custo medido de ~150 ms. Ignorar o pedido é deixar o receptor sem imagem.
        ///
        /// O tratador roda numa **thread da libdatachannel**. Bloquear nele segura a recepção de
        /// RTCP da sessão inteira: o certo é levantar uma bandeira que o laço de captura leia.
        pub fn ao_pedir_idr(&self, tratador: impl Fn() + Send + Sync + 'static) -> Result<()> {
            let mut guarda = self
                .tratador
                .lock()
                .map_err(|_| Error::Transport("tratador de IDR envenenado por um pânico".into()))?;
            *guarda = Some(Arc::new(tratador));
            Ok(())
        }

        /// **Desregistra o tratador de IDR, com barreira.**
        ///
        /// Quando devolve [`Barreira::Cumprida`], o tratador antigo não está rodando em thread
        /// nenhuma e não voltará a rodar: a casca pode largar o que ele capturava — no caminho
        /// de C, o `user_data`.
        ///
        /// Não fecha a sessão nem a track: a casca pode registrar outro tratador depois, e o
        /// envio de quadros segue igual.
        pub fn desregistrar_idr(&self) -> Barreira {
            let Ok(mut guarda) = self.tratador.lock() else {
                // Cadeado envenenado: não dá para tirar o tratador do lugar, então não dá para
                // prometer barreira nenhuma.
                return Barreira::Prazo;
            };
            *guarda = None;
            drop(guarda);
            // Tirado o tratador do lugar, basta ver o portão vazio **uma vez**: todo despacho
            // acontece com um passe na mão, então portão vazio prova que ninguém ficou dentro do
            // tratador antigo.
            self.viva.esperar_vazio(PRAZO_DA_BARREIRA)
        }

        /// Alternativa ao [`TrackEmissor::ao_pedir_idr`]: **consome** um pedido pendente.
        ///
        /// Devolve `true` no máximo uma vez por rajada de PLI/FIR, e baixa a bandeira. O laço de
        /// captura da casca chama uma vez por quadro e, quando vier `true`, força um IDR.
        ///
        /// # Por que as duas formas existem
        ///
        /// Callback é o caminho natural em Swift e em C++. **Em Kotlin não é**: o tratador roda
        /// numa thread da libdatachannel, que não está anexada à JVM, e chamar de volta para o
        /// Java de lá exige `AttachCurrentThread`, referência global e desanexar na saída — três
        /// chances de derrubar o app, num aparelho de 1,79 GB, por causa de um pedido de
        /// quadro-chave.
        ///
        /// Com esta função a casca Android **não precisa de callback nenhum**: ela já tem um laço
        /// por quadro (o do MediaCodec), e uma leitura atômica por quadro custa nada. É também o
        /// que o contrato de mídia já recomendava — "levante uma bandeira que o laço de captura
        /// leia" —, agora disponível sem que cada casca a reimplemente.
        ///
        /// As duas formas convivem: registrar o tratador não desliga a bandeira.
        pub fn pegar_pedido_de_idr(&self) -> bool {
            self.pendente.swap(false, Ordering::Relaxed)
        }

        pub fn kind(&self) -> TrackKind {
            self.kind
        }

        pub fn label(&self) -> &str {
            &self.label
        }

        pub fn quadros_enviados(&self) -> u64 {
            self.enviados.load(Ordering::Relaxed)
        }

        pub fn idrs_enviados(&self) -> u64 {
            self.idrs.load(Ordering::Relaxed)
        }

        /// Quantos IDR saíram **sem** SPS/PPS junto.
        ///
        /// O contrato manda que todo IDR leve SPS e PPS. Isto conta, e não recusa, por dois
        /// motivos: recusar o quadro apagaria a imagem em vez de consertá-la, e a decisão de
        /// como reagir é da casca, que sabe se está começando ou se degradou.
        ///
        /// **Qualquer valor diferente de zero é defeito da casca emissora**, e o sintoma é
        /// exatamente o do M1 no Windows: quem entra na sessão depois fica sem imagem até o
        /// próximo IDR completo.
        pub fn idrs_sem_parametros(&self) -> u64 {
            self.idrs_sem_parametros.load(Ordering::Relaxed)
        }

        /// Quantos PLI/FIR chegaram do receptor.
        pub fn pedidos_de_idr(&self) -> u64 {
            self.pedidos_de_idr.load(Ordering::Relaxed)
        }

        /// Bytes ainda esperando para sair. Subindo, o emissor gera mais do que a rede leva.
        ///
        /// Zero depois de a sessão morrer: ver [`SessaoViva`]. Perguntar à libdatachannel por um
        /// id já destruído trava o processo no Windows.
        pub fn pendente(&self) -> usize {
            let Some(_passe) = self.viva.entrar() else {
                return 0;
            };
            self.track.pendente()
        }

        pub fn mid(&self) -> String {
            let Some(_passe) = self.viva.entrar() else {
                return String::new();
            };
            self.track.mid()
        }

        /// A sessão desta track ainda existe? Ver [`SessaoViva`].
        ///
        /// Leitura solta, para relatar estado. Quem vai **tocar a API C** usa
        /// [`TrackEmissor::entrar`], porque entre conferir e usar cabe um `rtcDeleteTrack`.
        pub fn esta_viva(&self) -> bool {
            self.viva.esta_aberto()
        }

        /// Pega um passe do portão da sessão, ou diz que a sessão acabou.
        fn entrar(&self) -> Result<crate::portao::Passe<'_>> {
            self.viva
                .entrar()
                .ok_or_else(|| Error::Transport("a sessão desta track já foi encerrada".into()))
        }

        /// A track crua, para o [`crate::transport::Session`] poder limpar o registro no `Drop`.
        pub(crate) fn track_crua(&self) -> quall_rtc::Track {
            self.track
        }
    }

    /// Uma track que **chega**. O núcleo remonta os quadros; decodificar é da casca.
    pub struct TrackReceptor {
        track: quall_rtc::Track,
        kind: TrackKind,
        /// `Some` numa track de áudio. Vem do `a=rtpmap` do SDP, não de um padrão nosso — ver
        /// [`codec_do_sdp`].
        codec_audio: Option<CodecDeAudio>,
        label: String,
        /// O depacotizador precisa de `&mut` e o callback da libdatachannel é `Fn`. O cadeado é
        /// por track e nunca disputado — os pacotes de uma track chegam sempre na mesma thread —,
        /// então custa uma instrução atômica por pacote e nenhuma espera.
        depacotizador: Arc<Mutex<Remontador>>,
        /// O relógio de chegada **da sessão**, o mesmo para todas as tracks dela. Carimba a
        /// chegada de cada pacote para o jitter da RFC 3550, para o relógio comum
        /// ([`crate::relogio`]) e para a porta puxada. Até 18/09/2026 era um por track, com
        /// origens diferentes, e as chegadas de duas tracks não se comparavam.
        chegada: Arc<crate::media::Clock>,
        /// O relógio comum da sessão, e o índice desta track nele.
        relogio: Arc<RelogioDaSessao>,
        indice_no_relogio: usize,
        /// A base do `timestamp_us` já foi informada ao relógio comum? Ver
        /// [`RelogioDaSessao::fixar_base`] sobre por que isso tem de ser feito na hora.
        base_informada: Arc<AtomicBool>,
        /// A base que o depacotizador fixou, legível sem o cadeado dele.
        base_publicada: BasePublicada,
        /// Os contadores do depacotizador, publicados pela bomba depois de cada pacote. É daqui
        /// que [`TrackReceptor::contadores`] lê, sem o cadeado do depacotizador.
        retrato_contadores: Arc<Mutex<Contadores>>,
        /// Quem consome o áudio: ninguém, a porta empurrada ou a puxada. Ver [`ModoDeAudio`].
        modo_audio: Arc<Mutex<ModoDeAudio>>,
        pedidos: AtomicU64,
        /// Tratador da casca, se ela registrou um. Mesma forma do [`TrackEmissor`], e pelo mesmo
        /// motivo: dá para trocar e para **tirar**, que é o desregistro que a fronteira C não
        /// tinha.
        tratador: TratadorDeQuadro,
        /// O mesmo, do lado do áudio. Só um dos dois é olhado, e quem decide é o
        /// [`Remontador`].
        tratador_audio: TratadorDeAudio,
        /// A bomba que puxa pacotes já foi instalada na libdatachannel?
        ///
        /// Ela é instalada uma vez, no primeiro [`TrackReceptor::ao_receber_quadro`], e daí em
        /// diante lê o tratador do campo acima a cada pacote. Antes disto o tratador **era** a
        /// bomba, e por isso não havia como desregistrar sem destruir a track.
        bomba: AtomicBool,
        /// Ver [`SessaoViva`]. Fechado pelo `Drop` da sessão, antes do `rtcDeleteTrack`.
        viva: SessaoViva,
        /// O relógio de vida do caminho da mídia, compartilhado com a sessão. Ver
        /// [`crate::transport::Session::silencio_da_midia`]. É batido por **pacote**, e não por
        /// quadro pronto: quem quer saber se o caminho morreu não pode esperar a remontagem
        /// terminar, porque com fragmento faltando ela nunca termina.
        batimento: Arc<crate::transport::Batimento>,
        /// **Só de bancada**: a troca de ordem na chegada. Ver
        /// [`TrackReceptor::cravar_troca_de_bancada`].
        troca: Arc<TrocaDeBancada>,
    }

    /// A reordenação que o controle 5 do `docs/som-no-receptor.md` §9.3 precisa e `lo0` não tem.
    ///
    /// Com `a_cada = N`, um pacote a cada N é **segurado** na chegada, e entregue ao núcleo logo
    /// depois do seguinte: o par chega trocado, com a hora de chegada de quando cada um é
    /// entregue. Fica **antes** de tudo — do batimento, do relógio comum e do depacotizador —, que
    /// é onde a rede reordenaria. `0` (o padrão) desliga, e aí custa uma leitura atômica por
    /// pacote. Nenhuma casca de produto liga: quem liga é a sonda (`receber-video
    /// --trocar-no-som`) e os testes do relógio comum.
    #[derive(Default)]
    struct TrocaDeBancada {
        a_cada: std::sync::atomic::AtomicU32,
        vistos: AtomicU64,
        trocados: AtomicU64,
        /// Pares trocados **através da volta de 32 bits** do carimbo RTP: o que chegou primeiro já
        /// deu a volta (carimbo cru menor, e à frente pela diferença com sinal), e o segurado é de
        /// antes dela. É o caso das críticas 1 e 3 da S1; sem ele o controle 5 não pegou nada.
        trocados_na_volta: AtomicU64,
        segurado: Mutex<Option<Vec<u8>>>,
    }

    /// O carimbo RTP cru de um pacote (bytes 4 a 7), ou `None` se ele for curto demais.
    fn carimbo_cru(pacote: &[u8]) -> Option<u32> {
        pacote
            .get(4..8)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    impl TrackReceptor {
        pub(crate) fn adotar(
            track: quall_rtc::Track,
            viva: SessaoViva,
            batimento: Arc<crate::transport::Batimento>,
            relogio: Arc<RelogioDaSessao>,
        ) -> Result<Self> {
            track.preparar_recebida().map_err(traduzir)?;
            let mid = track.mid();
            let kind = TrackKind::from_mid(&mid)?;
            let descricao = track.descricao();
            // O rótulo legível veio no `a=msid` do SDP, posto lá pelo emissor.
            let label = rotulo_do_sdp(&descricao, &mid).unwrap_or_else(|| mid.clone());

            let codec_audio = if kind.e_audio() {
                // **Recusar é melhor que adivinhar.** Sem o rtpmap não dá para saber a taxa do
                // relógio, e entregar áudio com carimbo numa escala errada é pior que não
                // entregar: o som sai, parece funcionar, e todo alinhamento com o vídeo fica
                // errado por um fator de seis. A track vira `tracks_recusadas`, que é um número
                // que alguém lê.
                Some(codec_do_sdp(&descricao).ok_or_else(|| {
                    Error::Protocol(format!(
                        "track de áudio sem `a=rtpmap` de codec conhecido: {descricao:?}"
                    ))
                })?)
            } else {
                None
            };

            // A sessão de recepção de RTCP é o que dá voz ao receptor: sem ela
            // `rtcRequestKeyframe` devolve falso e o PLI nunca sai do aparelho.
            //
            // Numa track de áudio ela continua valendo a pena, ainda que o PLI não exista: é ela
            // que impede o RTCP de vazar para o caminho do RTP, onde viraria `rtcp_ignorados`.
            // Ela lê os Sender Reports do emissor e **não os repassa**: medido em 18/09/2026, 44
            // SR na biblioteca e 0 aqui. O relógio comum das tracks não vem do SR, e sim dos
            // carimbos crus (`crate::relogio`, `docs/som-no-receptor.md` §5).
            track.sessao_rtcp().map_err(traduzir)?;

            let remontador = match codec_audio {
                Some(codec) => Remontador::Audio(DepacotizadorDeAudio::new(codec.relogio_hz())),
                None => Remontador::Video(Depacotizador::new()),
            };
            let hz = codec_audio
                .map(|c| c.relogio_hz())
                .unwrap_or(crate::rtp::RELOGIO_VIDEO_HZ);
            let indice_no_relogio = relogio.registrar(hz);
            let base_publicada = remontador.base_publicada();
            let retrato_contadores = Arc::new(Mutex::new(remontador.contadores()));

            Ok(TrackReceptor {
                track,
                kind,
                codec_audio,
                label,
                depacotizador: Arc::new(Mutex::new(remontador)),
                chegada: relogio.relogio_de_chegada(),
                relogio,
                indice_no_relogio,
                base_informada: Arc::new(AtomicBool::new(false)),
                base_publicada,
                retrato_contadores,
                modo_audio: Arc::new(Mutex::new(ModoDeAudio::Nenhum)),
                pedidos: AtomicU64::new(0),
                tratador: Arc::new(Mutex::new(None)),
                tratador_audio: Arc::new(Mutex::new(None)),
                bomba: AtomicBool::new(false),
                viva,
                batimento,
                troca: Arc::new(TrocaDeBancada::default()),
            })
        }

        /// **Só de bancada**: troca a ordem de 1 par de pacotes a cada `a_cada`, na chegada, antes
        /// do núcleo ver. `0` desliga; `1` é recusado (seguraria todo pacote).
        ///
        /// É a reordenação do controle 5 do `docs/som-no-receptor.md` §9.3: o desenrolar da volta
        /// de 32 bits com um pacote de antes da volta chegando depois de um de depois (críticas 1 e
        /// 3 da S1). Em `lo0` não há reordenação natural, e sem ela o controle passa sem pegar o
        /// defeito. Pode ser chamada antes ou depois de a bomba estar instalada.
        pub fn cravar_troca_de_bancada(&self, a_cada: u32) -> bool {
            if a_cada == 1 {
                return false;
            }
            self.troca.a_cada.store(a_cada, Ordering::Relaxed);
            true
        }

        /// Quantos pares a troca de bancada já entregou trocados, e quantos deles atravessavam a
        /// volta de 32 bits do carimbo RTP.
        pub fn trocados_na_bancada(&self) -> (u64, u64) {
            (
                self.troca.trocados.load(Ordering::Relaxed),
                self.troca.trocados_na_volta.load(Ordering::Relaxed),
            )
        }

        /// Registra o tratador de quadro remontado.
        ///
        /// O tratador roda numa **thread da libdatachannel** e recebe uma fatia do buffer de
        /// remontagem, válida só durante a chamada. Copiar dali é escolha da casca — e no
        /// caminho normal ela entrega direto ao decoder, sem copiar.
        pub fn ao_receber_quadro(
            &self,
            tratador: impl Fn(QuadroCodificado<'_>) + Send + Sync + 'static,
        ) {
            if let Ok(mut guarda) = self.tratador.lock() {
                *guarda = Some(Arc::new(tratador));
            }
            self.instalar_bomba();
        }

        /// Registra o tratador de quadro de áudio.
        ///
        /// Mesmo contrato do [`TrackReceptor::ao_receber_quadro`]: roda numa **thread da
        /// libdatachannel**, recebe uma fatia válida só durante a chamada, e bloquear nele segura
        /// a recepção da sessão inteira.
        ///
        /// Registrar o tratador errado para o tipo da track não é erro — é silêncio: a bomba
        /// entrega ao tratador que casa com o [`Remontador`], e o outro nunca é chamado. Quem
        /// quiser saber de que tipo a track é pergunta a [`TrackReceptor::codec_de_audio`] ou a
        /// [`TrackReceptor::kind`] **antes** de registrar.
        ///
        /// **Exclusivo com [`TrackReceptor::reproducao_puxada`]**: com a porta puxada aberta,
        /// devolve `Error::Invalid` e não troca nada. Antes de 18/09/2026 devolvia `()` e trocava o
        /// tratador em silêncio, o que desligaria a porta puxada sem ninguém saber.
        pub fn ao_receber_audio(
            &self,
            tratador: impl Fn(QuadroDeAudio<'_>) + Send + Sync + 'static,
        ) -> Result<()> {
            let mut modo = self
                .modo_audio
                .lock()
                .map_err(|_| Error::Invalid("o modo de áudio desta track foi envenenado".into()))?;
            if *modo == ModoDeAudio::Puxado {
                return Err(Error::Invalid(
                    "esta track tem a reprodução puxada aberta: feche-a antes de registrar um \
                     tratador empurrado"
                        .into(),
                ));
            }
            if let Ok(mut guarda) = self.tratador_audio.lock() {
                *guarda = Some(Arc::new(move |q, _chegada| tratador(q)));
            }
            *modo = ModoDeAudio::Empurrado;
            drop(modo);
            self.instalar_bomba();
            Ok(())
        }

        /// **Abre a reprodução puxada** desta track: a porta que a casca chama no ritmo do
        /// dispositivo de saída. Ver [`crate::reproducao`] e `docs/contrato-som-puxado.md`.
        ///
        /// Recusa com `Error::Invalid` quando a track não é de áudio, quando já há um tratador
        /// empurrado, ou quando já há uma reprodução puxada aberta. A política (profundidade,
        /// FEC) sai do preset da espécie e do codec negociado, como na porta empurrada: uma fonte
        /// de verdade só.
        ///
        /// Fechar é [`ReproducaoPuxada::encerrar`], ou largar o valor.
        pub fn reproducao_puxada(&self, casca_reamostra: bool) -> Result<ReproducaoPuxada> {
            let politica = self.politica_de_audio().ok_or_else(|| {
                Error::Invalid(
                    "esta track não é de áudio; a reprodução puxada é só de áudio".into(),
                )
            })?;
            let mut modo = self
                .modo_audio
                .lock()
                .map_err(|_| Error::Invalid("o modo de áudio desta track foi envenenado".into()))?;
            if *modo != ModoDeAudio::Nenhum {
                return Err(Error::Invalid(format!(
                    "esta track já tem quem consuma o áudio ({:?}): as duas portas são exclusivas",
                    *modo
                )));
            }
            let (alimentador, reproducao) = ReproducaoPuxada::nova(
                OpcoesDeReproducao {
                    politica,
                    casca_reamostra,
                },
                Arc::clone(&self.chegada),
            );
            if let Ok(mut guarda) = self.tratador_audio.lock() {
                *guarda = Some(Arc::new(move |q, chegada_us| {
                    alimentador.entregar(&q, chegada_us)
                }));
            }
            *modo = ModoDeAudio::Puxado;
            drop(modo);
            self.instalar_bomba();

            let tratador = Arc::clone(&self.tratador_audio);
            let modo = Arc::clone(&self.modo_audio);
            let viva = Arc::clone(&self.viva);
            Ok(reproducao.com_soltura(Box::new(move || {
                if let Ok(mut g) = tratador.lock() {
                    *g = None;
                }
                let barreira = viva.esperar_vazio(PRAZO_DA_BARREIRA);
                if let Ok(mut m) = modo.lock() {
                    *m = ModoDeAudio::Nenhum;
                }
                barreira
            })))
        }

        /// Quem consome o áudio desta track agora.
        pub fn modo_de_audio(&self) -> ModoDeAudio {
            self.modo_audio
                .lock()
                .map(|m| *m)
                .unwrap_or(ModoDeAudio::Nenhum)
        }

        /// A política do buffer desta track de áudio: a mesma que o preset anuncia no SDP.
        ///
        /// `fec_disponivel` não é o `fec` cru do preset: numa track de microfone negociada em
        /// PCMU o preset ainda diz `fec: true`, e G.711 não tem LBRR. `None` numa track de vídeo.
        pub fn politica_de_audio(&self) -> Option<Politica> {
            let preset = self.kind.preset_de_audio()?;
            let base = if preset.fec {
                Politica::MICROFONE
            } else {
                Politica::AUDIO_DO_SISTEMA
            };
            Some(Politica {
                fec_disponivel: base.fec_disponivel
                    && matches!(self.codec_audio, Some(CodecDeAudio::Opus)),
                ..base
            })
        }

        /// O deslocamento de captura desta track no relógio comum da sessão. Ver
        /// [`crate::relogio`].
        pub fn deslocamento_de_captura(&self) -> DeslocamentoDeCaptura {
            self.informar_base();
            self.relogio.deslocamento(self.indice_no_relogio)
        }

        /// O retrato do relógio comum desta track, para relatório. `None` antes do primeiro
        /// pacote.
        pub fn retrato_do_relogio(&self) -> Option<RetratoDoRelogio> {
            self.informar_base();
            self.relogio.retrato(self.indice_no_relogio)
        }

        /// Informa ao relógio da sessão a base que o depacotizador fixou, se ainda não informou.
        ///
        /// **Não pede o cadeado do depacotizador**, que a bomba segura enquanto chama o tratador
        /// da casca: lê a base do atômico em que o depacotizador a publica ao fixá-la. Antes, isto
        /// travava a thread da libdatachannel quando a casca perguntava o deslocamento de dentro
        /// do tratador de quadro (revisão do código da S1, achado B1).
        fn informar_base(&self) {
            informar_base(
                &self.base_publicada,
                &self.base_informada,
                &self.relogio,
                self.indice_no_relogio,
            );
        }

        /// Instala, **uma vez**, a bomba que puxa pacotes da libdatachannel.
        ///
        /// Daí em diante ela lê o tratador do campo a cada pacote. Fazer o contrário — o tratador
        /// **ser** a bomba, como era antes — é o que deixava a fronteira C sem desregistro: para
        /// tirar o tratador era preciso destruir a track.
        fn instalar_bomba(&self) {
            if self.bomba.swap(true, Ordering::SeqCst) {
                return;
            }
            let depacotizador = Arc::clone(&self.depacotizador);
            let da_casca = Arc::clone(&self.tratador);
            let da_casca_audio = Arc::clone(&self.tratador_audio);
            let portao = Arc::clone(&self.viva);
            let chegada = Arc::clone(&self.chegada);
            let batimento = Arc::clone(&self.batimento);
            let relogio = Arc::clone(&self.relogio);
            let indice = self.indice_no_relogio;
            let base_informada = Arc::clone(&self.base_informada);
            let base_publicada = self.base_publicada.clone();
            let retrato = Arc::clone(&self.retrato_contadores);
            let endereco = Arc::as_ptr(&self.depacotizador) as usize;
            let troca = Arc::clone(&self.troca);
            let processar = move |bytes: &[u8]| {
                // Chegou pacote: o caminho da mídia está vivo. Antes da remontagem de propósito
                // — um pacote malformado, ou um fragmento do meio de um quadro que nunca vai
                // fechar, continua sendo prova de que o par nos alcança.
                batimento.bater();
                // A chegada é lida **antes** do cadeado, de propósito: o que o jitter mede é
                // quando o pacote chegou, não quando conseguimos processá-lo. Ler depois faria
                // qualquer disputa de cadeado entrar na conta como se fosse jitter de rede.
                let agora_us = chegada.micros();
                // O relógio comum vê todo pacote, antes da remontagem: o trânsito mínimo por
                // janela precisa do carimbo cru e da chegada, e um fragmento que morre também
                // serve.
                relogio.observar_pacote(indice, bytes, agora_us);
                let Ok(mut d) = depacotizador.lock() else {
                    // Cadeado envenenado por um pânico em outra thread. Com `panic = "abort"`
                    // em release isso não acontece; em debug, desistir do pacote é melhor que
                    // propagar o pânico para dentro do C++.
                    return;
                };
                // A base do `timestamp_us` entregue vai ao relógio **na hora** em que o
                // depacotizador a fixa, e **antes** de o quadro chegar à casca: calculada depois,
                // numa sessão de mais de 6 h 37 min, ela cairia uma volta para o lado errado; e a
                // casca que pergunta o deslocamento no primeiro quadro precisa dela já informada.
                let informar =
                    || informar_base(&base_publicada, &base_informada, &relogio, indice);
                // Marca que esta thread está dentro da bomba desta track, com o cadeado na mão:
                // quem precisar do cadeado e for chamado de dentro do tratador recusa em vez de
                // travar. Ver `NA_BOMBA`.
                let _dentro = DentroDaBomba::entrar(endereco);
                match &mut *d {
                    Remontador::Video(d) => {
                        let atual = da_casca.lock().ok().and_then(|g| g.clone());
                        // Pacote malformado não derruba a track: o depacotizador já contou, e o
                        // conserto é o IDR que a casca vai pedir.
                        let _ = d.aceitar(bytes, |q| {
                            informar();
                            if let Some(f) = &atual {
                                f(QuadroCodificado {
                                    annexb: q.annexb,
                                    timestamp_us: q.timestamp_us,
                                    idr: q.idr,
                                });
                            }
                        });
                    }
                    Remontador::Audio(d) => {
                        let atual = da_casca_audio.lock().ok().and_then(|g| g.clone());
                        // Idem: pacote malformado é contado e esquecido. No áudio não há sequer
                        // quadro em construção para derrubar.
                        let _ = d.aceitar(bytes, agora_us, |q| {
                            informar();
                            if let Some(f) = &atual {
                                f(q, agora_us);
                            }
                        });
                    }
                }
                informar();
                // Os contadores saem num retrato próprio, publicado **depois** de a casca voltar:
                // `contadores()` não pede o cadeado do depacotizador, e pode ser chamado de dentro
                // do tratador. Antes, `quall_track_stats_json` chamado dali travava a thread.
                if let Ok(mut r) = retrato.lock() {
                    *r = d.contadores();
                }
            };
            self.track.ao_receber(move |bytes| {
                // O passe primeiro, e por dois motivos: enquanto ele durar, o fechamento da
                // sessão espera — é a barreira que autoriza a casca a liberar o `user_data` —
                // e ninguém entra aqui depois que a porta fecha.
                let Some(_passe) = portao.entrar() else {
                    return;
                };
                let a_cada = troca.a_cada.load(Ordering::Relaxed);
                if a_cada == 0 {
                    processar(bytes);
                    return;
                }
                // A troca de bancada (`cravar_troca_de_bancada`): o segurado sai logo depois do
                // pacote que chegou atrás dele.
                let k = troca.vistos.fetch_add(1, Ordering::Relaxed);
                let Ok(mut segurado) = troca.segurado.lock() else {
                    processar(bytes);
                    return;
                };
                if let Some(anterior) = segurado.take() {
                    drop(segurado);
                    processar(bytes);
                    processar(&anterior);
                    troca.trocados.fetch_add(1, Ordering::Relaxed);
                    if let (Some(novo), Some(velho)) = (carimbo_cru(bytes), carimbo_cru(&anterior)) {
                        if novo < velho && (novo.wrapping_sub(velho) as i32) > 0 {
                            troca.trocados_na_volta.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                } else if k % u64::from(a_cada) == u64::from(a_cada) - 1 {
                    *segurado = Some(bytes.to_vec());
                } else {
                    drop(segurado);
                    processar(bytes);
                }
            });
        }

        /// **Desregistra o tratador de quadro, com barreira.**
        ///
        /// Quando devolve [`Barreira::Cumprida`], o tratador antigo não está rodando em thread
        /// nenhuma e não voltará a rodar: a casca pode largar o `user_data` sem esperar a sessão
        /// acabar. É o caso do plugin de OBS que remove uma fonte com a sessão ainda de pé.
        ///
        /// A track continua recebendo e os contadores continuam andando; o que some é a chamada
        /// para o código da casca.
        pub fn desregistrar_quadro(&self) -> Barreira {
            let Ok(mut guarda) = self.tratador.lock() else {
                return Barreira::Prazo;
            };
            *guarda = None;
            drop(guarda);
            self.viva.esperar_vazio(PRAZO_DA_BARREIRA)
        }

        /// **Desregistra o tratador de áudio empurrado, com barreira.** Ver
        /// [`TrackReceptor::desregistrar_quadro`], de que este é o par exato.
        ///
        /// Numa track com a **reprodução puxada** aberta devolve `Error::Invalid` e não mexe em
        /// nada: quem fecha a porta puxada é [`ReproducaoPuxada::encerrar`]. Antes de 18/09/2026
        /// não havia porta puxada, e isto devolvia a barreira direto.
        pub fn desregistrar_audio(&self) -> Result<Barreira> {
            let mut modo = self
                .modo_audio
                .lock()
                .map_err(|_| Error::Invalid("o modo de áudio desta track foi envenenado".into()))?;
            if *modo == ModoDeAudio::Puxado {
                return Err(Error::Invalid(
                    "esta track tem a reprodução puxada aberta: feche-a por ela".into(),
                ));
            }
            let Ok(mut guarda) = self.tratador_audio.lock() else {
                return Ok(Barreira::Prazo);
            };
            *guarda = None;
            drop(guarda);
            *modo = ModoDeAudio::Nenhum;
            drop(modo);
            Ok(self.viva.esperar_vazio(PRAZO_DA_BARREIRA))
        }

        /// Pede um IDR ao emissor. Emite PLI.
        ///
        /// A casca chama ao entrar na sessão sem ter visto IDR, ou quando o decoder perde
        /// sincronia. Devolve `Result` — e não nada, como o esboço do contrato — porque o pedido
        /// **falha de verdade**: antes de a track abrir, ou sem a sessão de RTCP encadeada, a
        /// libdatachannel devolve erro e o PLI não sai do aparelho. Engolir isso em silêncio
        /// seria reproduzir, do lado do receptor, o defeito que o contrato existe para resolver.
        pub fn pedir_idr(&self) -> Result<()> {
            if self.codec_audio.is_some() {
                // **Barrado aqui, e não repassado.** `Track::requestKeyframe` da libdatachannel
                // devolve `false` sem tocar na rede quando o tipo não é "video", e o `wrap` da
                // API C traduz isso para `RTC_ERR_FAILURE` — indistinguível de "a track ainda
                // não abriu", que é justamente o erro que a casca receptora trata tentando de
                // novo. Ela ficaria num laço de 50 tentativas contra uma parede.
                //
                // E não é limitação da biblioteca: **não existe quadro-chave de áudio**. Todo
                // quadro de Opus é independente, e o que se perdeu não pode ser pedido de volta.
                // A recuperação de perda no áudio é o FEC embutido, que já vai declarado no
                // `a=fmtp` — ver `PRESET_MICROFONE`.
                return Err(Error::Invalid(
                    "pedido de IDR numa track de áudio: não existe quadro-chave de áudio".into(),
                ));
            }
            self.pedidos.fetch_add(1, Ordering::Relaxed);
            let Some(_passe) = self.viva.entrar() else {
                return Err(Error::Transport(
                    "a sessão desta track já foi encerrada".into(),
                ));
            };
            self.track.pedir_idr().map_err(traduzir)
        }

        /// A sessão desta track ainda existe? Ver [`SessaoViva`].
        pub fn esta_viva(&self) -> bool {
            self.viva.esta_aberto()
        }

        pub fn kind(&self) -> TrackKind {
            self.kind
        }

        pub fn label(&self) -> &str {
            &self.label
        }

        pub fn pedidos_de_idr(&self) -> u64 {
            self.pedidos.load(Ordering::Relaxed)
        }

        /// Quadros entregues inteiros. No áudio, um pacote é um quadro.
        pub fn quadros_prontos(&self) -> u64 {
            self.contadores().quadros_prontos
        }

        /// Quadros jogados fora por perda ou desordem. Ver [`crate::rtp`] sobre o desenho sem
        /// jitter buffer.
        ///
        /// **Sempre 0 numa track de áudio**, e não por omissão: lá um pacote perdido já é um
        /// quadro perdido e está em [`Self::pacotes_faltando`]. Ver
        /// [`crate::rtp::DepacotizadorDeAudio::contadores`].
        pub fn quadros_descartados(&self) -> u64 {
            self.contadores().quadros_descartados
        }

        /// Anomalias na sequência RTP. Ver [`crate::rtp::Depacotizador::pacotes_perdidos`]: é a
        /// soma de [`Self::pacotes_faltando`] com [`Self::eventos_fora_de_ordem`], e uma troca de
        /// ordem aparece três vezes — então o número superestima em Wi-Fi carregado.
        pub fn pacotes_perdidos(&self) -> u64 {
            self.contadores().pacotes_perdidos()
        }

        /// Posições de sequência que nunca chegaram. Ver
        /// [`crate::rtp::Depacotizador::pacotes_faltando`].
        pub fn pacotes_faltando(&self) -> u64 {
            self.contadores().pacotes_faltando
        }

        /// Repetição ou troca de ordem, contada por evento. Ver
        /// [`crate::rtp::Depacotizador::eventos_fora_de_ordem`].
        pub fn eventos_fora_de_ordem(&self) -> u64 {
            self.contadores().eventos_fora_de_ordem
        }

        /// A janela observada. Ver [`crate::rtp::Depacotizador::pacotes_vistos`].
        pub fn pacotes_vistos(&self) -> u64 {
            self.contadores().pacotes_vistos
        }

        /// **Jitter de chegada da RFC 3550, em microssegundos.** `None` fora do áudio, e antes
        /// do segundo pacote. Ver [`Contadores::jitter_us`].
        pub fn jitter_us(&self) -> Option<u32> {
            self.contadores().jitter_us
        }

        /// **Todos os contadores num instante só.**
        ///
        /// É o que um relatório deve usar, e é por isso que os acessores acima passam todos por
        /// aqui em vez de pegarem o cadeado cada um por sua conta: um relatório montado com sete
        /// leituras separadas mistura sete instantes, e a aritmética que liga os números deixa
        /// de fechar. Ver [`Contadores`].
        ///
        /// **Não pede o cadeado do depacotizador**: lê o retrato que a bomba publica depois de
        /// cada pacote, com a casca já de volta. Pode ser chamado de dentro do tratador — antes
        /// de 18/09/2026 isso travava a thread da libdatachannel, e com ela a sessão inteira.
        pub fn contadores(&self) -> Contadores {
            self.retrato_contadores
                .lock()
                .map(|c| *c)
                .unwrap_or_default()
        }

        /// **Bancada: crava a profundidade do anel de reordenação e desliga o ajuste
        /// automático.** `0` desliga a fila inteira.
        ///
        /// # Por que existe, e por que existe no núcleo
        ///
        /// O anel passou a se ajustar sozinho em 02/09/2026, e a primeira corrida de Wi-Fi
        /// levantou uma dúvida contra o próprio ajuste: com a mesma perda (~3,8 %), o anel
        /// adaptativo terminou em 4 e mediu **mais que o dobro** de `suspeitos` por mil quadros
        /// que a corrida de 01/09 com o anel fixo em 16.
        ///
        /// Só que as duas corridas são de **dias diferentes**, e este repositório repete em três
        /// frentes que duas corridas de 2,4 GHz separadas no tempo não se comparam — o rádio anda
        /// ao longo da medição. Sem um jeito de rodar o braço de controle **no mesmo enlace**, a
        /// comparação continua sendo anedota, e um controlador que se acusa por anedota é pior
        /// que nenhum.
        ///
        /// Este botão é o braço de controle. Ele não muda produto: `0` é o padrão e mantém o
        /// ajuste ligado.
        ///
        /// Devolve `false` se a track for de áudio (que não tem anel) ou se o cadeado cair.
        ///
        /// De dentro do tratador desta track devolve `false` sem fazer nada: o cadeado do
        /// depacotizador está com a bomba, na mesma thread.
        pub fn cravar_anel_de_reordenacao(&self, pacotes: usize) -> bool {
            if DentroDaBomba::esta_em(Arc::as_ptr(&self.depacotizador) as usize) {
                return false;
            }
            let Some(_passe) = self.viva.entrar() else {
                return false;
            };
            let Ok(mut d) = self.depacotizador.lock() else {
                return false;
            };
            let ok = d.cravar_anel(pacotes);
            if let Ok(mut r) = self.retrato_contadores.lock() {
                *r = d.contadores();
            }
            ok
        }

        /// Pacotes RTCP que vazaram para o caminho do RTP. Tem de ser zero.
        pub fn rtcp_ignorados(&self) -> u64 {
            self.contadores().rtcp_ignorados
        }

        /// O codec desta track, ou `None` se ela for de vídeo. Lido do `a=rtpmap` do SDP na
        /// adoção — ver `codec_do_sdp`.
        pub fn codec_de_audio(&self) -> Option<CodecDeAudio> {
            self.codec_audio
        }

        pub fn mid(&self) -> String {
            let Some(_passe) = self.viva.entrar() else {
                return String::new();
            };
            self.track.mid()
        }
    }

    #[cfg(test)]
    mod testes_rotulo {
        use super::{codificar_rotulo, decodificar_rotulo, rotulo_do_sdp};

        const DESCRICAO: &str = "m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
             a=mid:screen\r\n\
             a=sendonly\r\n\
             a=ssrc:1 msid:Tela%20do%20Galaxy%20A10s screen\r\n\
             a=msid:Tela%20do%20Galaxy%20A10s screen\r\n\
             a=rtcp-mux";

        #[test]
        fn rotulo_percent_encoded_volta_inteiro() {
            assert_eq!(
                rotulo_do_sdp(DESCRICAO, "screen").as_deref(),
                Some("Tela do Galaxy A10s")
            );
        }

        #[test]
        fn rotulo_sobrevive_a_ida_e_volta_com_espaco_e_acento() {
            for original in [
                "Tela do Galaxy A10s",
                "Câmera do iPhone 7",
                "screen",
                "a b  c",
                "100% da tela",
            ] {
                let codificado = codificar_rotulo(original);
                assert!(
                    !codificado.contains(' '),
                    "o codificado ainda tem espaço: {codificado:?}"
                );
                assert_eq!(decodificar_rotulo(&codificado), original);
            }
        }

        #[test]
        fn rotulo_de_outra_implementacao_passa_como_veio() {
            // Sem percent-encoding, e sem `%` para decodificar: copia.
            assert_eq!(decodificar_rotulo("Tela"), "Tela");
            // `%` solto no fim não pode virar erro nem comer bytes.
            assert_eq!(decodificar_rotulo("50%"), "50%");
        }

        #[test]
        fn sem_msid_nao_inventa_rotulo() {
            assert_eq!(
                rotulo_do_sdp("m=video 9 UDP/TLS/RTP/SAVPF 96", "screen"),
                None
            );
        }

        #[test]
        fn msid_so_com_o_track_id_nao_vira_rotulo_vazio() {
            assert_eq!(rotulo_do_sdp("a=msid:screen", "screen"), None);
        }
    }
}

#[cfg(feature = "webrtc")]
pub use webrtc::{TrackEmissor, TrackReceptor};

#[cfg(test)]
mod tests {
    use super::*;

    /// Todas as espécies, para os testes que precisam varrer o enum inteiro.
    ///
    /// Escrita à mão porque `TrackKind` não deriva enumeração — e é justamente por isso que
    /// vale: acrescentar uma espécie sem acrescentá-la aqui faz
    /// `toda_especie_esta_na_lista_dos_testes` falhar, em vez de deixar a espécie nova passar
    /// sem nenhum teste em cima dela.
    const TODAS_AS_ESPECIES: [TrackKind; 4] = [
        TrackKind::Screen,
        TrackKind::Camera,
        TrackKind::Microphone,
        TrackKind::SystemAudio,
    ];

    /// A tranca da lista acima: o `match` é exaustivo, então uma espécie nova **não compila**
    /// sem passar por aqui, e quem passar por aqui é lembrado de somá-la ao total.
    #[test]
    fn toda_especie_esta_na_lista_dos_testes() {
        for k in TODAS_AS_ESPECIES {
            let _cobre: () = match k {
                TrackKind::Screen
                | TrackKind::Camera
                | TrackKind::Microphone
                | TrackKind::SystemAudio => (),
            };
        }
        assert_eq!(TODAS_AS_ESPECIES.len(), 4);
    }

    /// Monta um quadro Annex-B a partir de NAL units cruas, com o prefixo longo.
    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn mid_ida_e_volta() {
        for k in TODAS_AS_ESPECIES {
            assert_eq!(TrackKind::from_mid(k.mid()).expect("volta"), k);
        }
    }

    #[test]
    fn mid_desconhecido_e_erro() {
        assert!(TrackKind::from_mid("tela").is_err());
    }

    #[test]
    fn cada_tipo_tem_ssrc_proprio() {
        // Par a par, e não três comparações à mão: com quatro espécies a forma antiga já
        // deixaria um par de fora, e é assim que duas tracks acabam com o mesmo SSRC.
        for (i, a) in TODAS_AS_ESPECIES.iter().enumerate() {
            for b in &TODAS_AS_ESPECIES[i + 1..] {
                assert_ne!(a.ssrc(), b.ssrc(), "{a:?} e {b:?} têm o mesmo SSRC");
            }
        }
    }

    #[test]
    fn cada_tipo_tem_mid_proprio() {
        for (i, a) in TODAS_AS_ESPECIES.iter().enumerate() {
            for b in &TODAS_AS_ESPECIES[i + 1..] {
                assert_ne!(a.mid(), b.mid(), "{a:?} e {b:?} têm o mesmo mid");
            }
        }
    }

    #[test]
    fn microfone_nao_e_video() {
        assert!(TrackKind::Screen.e_video());
        assert!(TrackKind::Camera.e_video());
        assert!(!TrackKind::Microphone.e_video());
    }

    /// **O áudio do sistema não pode ser classificado como vídeo.**
    ///
    /// A versão anterior de `e_video` era `!matches!(self, TrackKind::Microphone)` — uma lista
    /// de exceções. Quando `SystemAudio` entrou, essa forma o teria dado como **vídeo**: a track
    /// abriria com pacotizador H.264 e `m=video` para carregar Opus, sem erro nenhum, e o único
    /// sintoma seria silêncio. Este teste é a tranca.
    #[test]
    fn audio_do_sistema_e_audio_e_nao_video() {
        assert!(TrackKind::SystemAudio.e_audio());
        assert!(!TrackKind::SystemAudio.e_video());
    }

    #[test]
    fn video_e_audio_sao_exclusivos_e_cobrem_tudo() {
        for k in TODAS_AS_ESPECIES {
            assert_ne!(
                k.e_video(),
                k.e_audio(),
                "{k:?} não é nem uma coisa nem outra"
            );
        }
    }

    /// Toda espécie de áudio tem preset, e nenhuma de vídeo tem. É o invariante que permite a
    /// `abrir` perguntar "há preset?" em vez de "que espécie é?".
    #[test]
    fn so_as_especies_de_audio_tem_preset() {
        for k in TODAS_AS_ESPECIES {
            assert_eq!(
                k.preset_de_audio().is_some(),
                k.e_audio(),
                "{k:?}: preset e espécie discordam"
            );
        }
    }

    /// As duas espécies de áudio existem **porque** pedem codificação diferente. Se os presets
    /// forem iguais, a separação não está pagando o próprio custo.
    #[test]
    fn microfone_e_audio_do_sistema_nao_pedem_a_mesma_coisa() {
        let mic = PRESET_MICROFONE;
        let sis = PRESET_AUDIO_DO_SISTEMA;
        assert_ne!(mic, sis);
        assert_eq!(mic.canais, 1, "fala é mono");
        assert_eq!(sis.canais, 2, "som de sistema é estéreo");
        assert!(sis.taxa_media_bits > mic.taxa_media_bits);
    }

    /// **O `a=fmtp` tem de dizer o que o preset diz.** Um texto fixo ao lado da struct é um
    /// convite a os dois discordarem depois de uma edição — e a discordância seria invisível: o
    /// SDP sai, o outro lado aceita, e ninguém confere.
    #[test]
    fn o_fmtp_reflete_o_preset_que_o_gerou() {
        let mic = PRESET_MICROFONE.fmtp();
        assert!(mic.contains("stereo=0"), "{mic}");
        assert!(mic.contains("sprop-stereo=0"), "{mic}");
        assert!(mic.contains("useinbandfec=1"), "{mic}");
        assert!(mic.contains("maxaveragebitrate=32000"), "{mic}");

        let sis = PRESET_AUDIO_DO_SISTEMA.fmtp();
        assert!(sis.contains("stereo=1"), "{sis}");
        assert!(sis.contains("sprop-stereo=1"), "{sis}");
        assert!(sis.contains("maxaveragebitrate=128000"), "{sis}");
        // O item que não é gosto: a 128 kbit/s em estéreo o Opus opera em CELT, onde o LBRR não
        // existe. Declarar `useinbandfec=1` ali seria anunciar um recurso que o encoder não tem
        // como entregar — o mesmo defeito do SPS sem `bitstream_restriction` do M4.
        assert!(
            sis.contains("useinbandfec=0"),
            "áudio de sistema não pode declarar FEC que o modo CELT não entrega: {sis}"
        );
    }

    #[test]
    fn um_quadro_de_20ms_tem_960_amostras_em_opus_e_160_em_g711() {
        assert_eq!(PRESET_MICROFONE.amostras_por_quadro(), 960);
        assert_eq!(CodecDeAudio::Pcmu.amostras_por_quadro(20), 160);
    }

    /// Trocar só o codec preserva o resto do preset da espécie — é o que a bancada usa para
    /// pedir G.711 sem perder canais e taxa.
    #[test]
    fn trocar_o_codec_preserva_o_resto_do_preset() {
        let cfg =
            TrackConfig::new(TrackKind::SystemAudio, "Som").com_codec_de_audio(CodecDeAudio::Pcmu);
        let p = cfg.preset_audio.expect("track de áudio tem preset");
        assert_eq!(p.codec, CodecDeAudio::Pcmu);
        assert_eq!(p.canais, PRESET_AUDIO_DO_SISTEMA.canais);
        assert_eq!(
            p.duracao_do_quadro_ms,
            PRESET_AUDIO_DO_SISTEMA.duracao_do_quadro_ms
        );
    }

    #[test]
    fn track_de_video_nao_ganha_preset_de_audio() {
        assert!(TrackConfig::new(TrackKind::Screen, "Tela")
            .preset_audio
            .is_none());
    }

    #[test]
    fn idr_com_sps_e_pps_passa_na_conferencia() {
        let bytes = annexb(&[&[0x67, 0x42, 0xe0], &[0x68, 0xce], &[0x65, 0x88, 0x84]]);
        let q = QuadroCodificado {
            annexb: &bytes,
            timestamp_us: 0,
            idr: true,
        };
        assert!(q.tem_parametros());
    }

    #[test]
    fn idr_sem_parametros_e_detectado() {
        let bytes = annexb(&[&[0x65, 0x88, 0x84]]);
        let q = QuadroCodificado {
            annexb: &bytes,
            timestamp_us: 0,
            idr: true,
        };
        assert!(
            !q.tem_parametros(),
            "IDR sem SPS/PPS tem de ser detectado: é o defeito do M1 no Windows"
        );
    }

    #[test]
    fn so_sps_sem_pps_nao_basta() {
        let bytes = annexb(&[&[0x67, 0x42], &[0x65, 0x88]]);
        let q = QuadroCodificado {
            annexb: &bytes,
            timestamp_us: 0,
            idr: true,
        };
        assert!(!q.tem_parametros());
    }

    #[test]
    fn prefixo_curto_de_tres_bytes_tambem_e_reconhecido() {
        // O VideoToolbox e o MediaCodec não concordam sobre o tamanho do start code, e um
        // encoder pode misturar os dois no mesmo quadro.
        let mut bytes = vec![0, 0, 1, 0x67, 0x42];
        bytes.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce]);
        bytes.extend_from_slice(&[0, 0, 1, 0x65, 0x88]);
        let q = QuadroCodificado {
            annexb: &bytes,
            timestamp_us: 0,
            idr: true,
        };
        assert!(q.tem_parametros());
    }

    #[test]
    fn quadro_vazio_nao_estoura_a_conferencia() {
        let q = QuadroCodificado {
            annexb: &[],
            timestamp_us: 0,
            idr: true,
        };
        assert!(!q.tem_parametros());
    }

    #[test]
    fn perfil_declara_packetization_mode_1() {
        // Sem `packetization-mode=1` o outro lado pode recusar FU-A, e sem FU-A nenhum quadro
        // maior que a MTU atravessa — ou seja, nenhum IDR.
        assert!(PERFIL_H264.contains("packetization-mode=1"));
    }
}
