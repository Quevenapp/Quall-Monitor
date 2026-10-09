//! Opus de verdade: libopus vendorizada, codificador, decodificador e o leitor do byte de TOC.
//!
//! # O recorte, e por que este crate não é o núcleo
//!
//! O `contrato-track.md` e o `docs/audio.md` dizem a mesma frase: **o núcleo não captura e não
//! decodifica** — ele transporta quadros já codificados. Este crate é o outro lado dessa
//! fronteira, e por isso ele é um **irmão** do `quall-core`, não uma parte dele. O `quall-core`
//! continua sem uma linha de C de codec; quem quiser codificar pede a este crate.
//!
//! É o que permite a tabela de custo de `docs/audio.md` §11 existir: o Opus é um item de linha
//! separado, que uma plataforma pode não pagar.
//!
//! # O que este crate prova, e o que ele existe para desmentir
//!
//! Antes desta rodada, `useinbandfec`, `stereo` e `maxaveragebitrate` eram **texto conferido no
//! SDP** e nada mais — nenhum encoder jamais os honrou, porque não havia encoder. [`Toc`] é a
//! resposta a isso: o primeiro byte de todo pacote de Opus diz o modo, a largura de banda e a
//! duração do quadro, e é conferível sem decodificar nada. Ver `docs/audio.md` §11.

pub mod sys;

use std::ffi::CStr;
use std::os::raw::c_int;

// -------------------------------------------------------------------------------------------
// Erro
// -------------------------------------------------------------------------------------------

/// Falha vinda da libopus, com o código dela preservado.
///
/// Este crate **não** depende do `quall-core`: ele precisa poder ser compilado (e medido) sozinho,
/// que é metade da tabela de tamanho. Quem integra converte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erro {
    pub codigo: i32,
    pub contexto: &'static str,
}

impl std::fmt::Display for Erro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let texto = unsafe { CStr::from_ptr(sys::opus_strerror(self.codigo as c_int)) };
        write!(
            f,
            "{}: {} ({})",
            self.contexto,
            texto.to_string_lossy(),
            self.codigo
        )
    }
}

impl std::error::Error for Erro {}

pub type Resultado<T> = std::result::Result<T, Erro>;

fn conferir(codigo: c_int, contexto: &'static str) -> Resultado<()> {
    if codigo == sys::OPUS_OK {
        Ok(())
    } else {
        Err(Erro { codigo, contexto })
    }
}

/// A versão da libopus que foi de fato compilada, dita por ela mesma.
///
/// Existe para o relato de bancada não depender do que está escrito no `Cargo.toml`: se alguém
/// trocar `vendor/opus` sem trocar a documentação, é esta função que denuncia.
pub fn versao() -> String {
    unsafe {
        CStr::from_ptr(sys::opus_get_version_string())
            .to_string_lossy()
            .into_owned()
    }
}

// -------------------------------------------------------------------------------------------
// O byte de TOC
// -------------------------------------------------------------------------------------------

/// Em que modo o Opus está operando neste pacote.
///
/// É o campo que decide se o FEC embutido pode existir: **o LBRR só existe em SILK e híbrido**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    /// Só SILK. Fala. É onde o LBRR (o FEC embutido) existe.
    Silk,
    /// Híbrido: SILK embaixo, CELT em cima. O LBRR também existe aqui.
    Hibrido,
    /// Só CELT. Música e latência baixa. **Não há LBRR.**
    Celt,
}

impl Modo {
    /// O FEC embutido do Opus (LBRR) é possível neste modo?
    ///
    /// "Possível", e não "presente": um pacote em SILK sem fala nenhuma pode não carregar LBRR.
    /// Quem responde "presente" é [`tem_lbrr`], que percorre o cabeçalho de verdade.
    pub fn permite_lbrr(self) -> bool {
        matches!(self, Modo::Silk | Modo::Hibrido)
    }
}

/// A largura de banda de áudio do pacote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LarguraDeBanda {
    /// 4 kHz.
    Estreita,
    /// 6 kHz.
    Media,
    /// 8 kHz.
    Larga,
    /// 12 kHz.
    SuperLarga,
    /// 20 kHz.
    Cheia,
}

impl LarguraDeBanda {
    /// O valor que a própria libopus usa em `opus_packet_get_bandwidth`, para conferir o nosso
    /// leitor contra o dela.
    pub fn codigo_da_libopus(self) -> i32 {
        match self {
            LarguraDeBanda::Estreita => sys::OPUS_BANDWIDTH_NARROWBAND,
            LarguraDeBanda::Media => sys::OPUS_BANDWIDTH_MEDIUMBAND,
            LarguraDeBanda::Larga => sys::OPUS_BANDWIDTH_WIDEBAND,
            LarguraDeBanda::SuperLarga => sys::OPUS_BANDWIDTH_SUPERWIDEBAND,
            LarguraDeBanda::Cheia => sys::OPUS_BANDWIDTH_FULLBAND,
        }
    }
}

/// O primeiro byte de todo pacote de Opus, lido campo a campo (RFC 6716 §3.1).
///
/// # Por que este tipo existe
///
/// Porque é a diferença entre *afirmar* e *medir*. O `docs/audio.md` decidiu ligar o FEC no
/// microfone e desligá-lo no áudio de sistema com base em "a 32 kbit/s mono o Opus opera em SILK,
/// a 128 kbit/s estéreo ele opera em CELT" — uma afirmação de documentação de codec, que sustenta
/// a decisão inteira e que **nunca tinha sido conferida num fluxo nosso**.
///
/// O TOC responde isso sem decodificar nada, com o primeiro byte do pacote. É o mesmo movimento do
/// parser de SPS que achou o defeito do `bitstream_restriction` no M4: comparar o que a API
/// prometeu no SDP com o que de fato saiu no fio.
///
/// A leitura aqui é **nossa**, deliberadamente independente da libopus. `Toc::conferir_contra_libopus`
/// põe as duas frente a frente; duas implementações que concordam valem mais que uma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toc {
    /// Os 5 bits altos: 0 a 31. É ele que carrega modo, largura e duração de uma vez.
    pub configuracao: u8,
    /// O bit 4: o pacote é estéreo.
    pub estereo: bool,
    /// Os 2 bits baixos: quantos quadros vêm no pacote (0 = um, 1 = dois iguais, 2 = dois
    /// diferentes, 3 = arbitrário, contado num segundo byte).
    pub codigo_de_quadros: u8,
}

impl Toc {
    /// Lê o byte. Não falha: **todos** os 256 valores são TOC válidos.
    pub fn ler(byte: u8) -> Toc {
        Toc {
            configuracao: byte >> 3,
            estereo: (byte & 0x04) != 0,
            codigo_de_quadros: byte & 0x03,
        }
    }

    /// Lê o TOC do começo de um pacote.
    pub fn do_pacote(pacote: &[u8]) -> Option<Toc> {
        pacote.first().copied().map(Toc::ler)
    }

    /// O modo, pela tabela da RFC 6716 §3.1.
    pub fn modo(self) -> Modo {
        match self.configuracao {
            0..=11 => Modo::Silk,
            12..=15 => Modo::Hibrido,
            _ => Modo::Celt,
        }
    }

    /// A largura de banda, pela mesma tabela.
    pub fn largura_de_banda(self) -> LarguraDeBanda {
        match self.configuracao {
            0..=3 => LarguraDeBanda::Estreita,
            4..=7 => LarguraDeBanda::Media,
            8..=11 => LarguraDeBanda::Larga,
            12..=13 => LarguraDeBanda::SuperLarga,
            14..=15 => LarguraDeBanda::Cheia,
            16..=19 => LarguraDeBanda::Estreita,
            20..=23 => LarguraDeBanda::Larga,
            24..=27 => LarguraDeBanda::SuperLarga,
            _ => LarguraDeBanda::Cheia,
        }
    }

    /// A duração de **um** quadro, em microssegundos.
    ///
    /// Em microssegundos e não em milissegundos porque o CELT aceita 2,5 ms, e um inteiro de
    /// milissegundos arredondaria justamente o caso que a §4 do `docs/audio.md` aponta como saída
    /// para quem precisa de menos de 50 ms.
    pub fn duracao_do_quadro_us(self) -> u32 {
        match self.configuracao {
            // SILK: 10, 20, 40, 60 ms.
            0..=11 => [10_000, 20_000, 40_000, 60_000][(self.configuracao % 4) as usize],
            // Híbrido: 10, 20 ms.
            12..=15 => [10_000, 20_000][(self.configuracao % 2) as usize],
            // CELT: 2,5, 5, 10, 20 ms.
            _ => [2_500, 5_000, 10_000, 20_000][((self.configuracao - 16) % 4) as usize],
        }
    }

    /// Confere a nossa leitura contra a da própria libopus, no mesmo pacote.
    ///
    /// Devolve `Err` com uma frase que diz **qual** campo divergiu. Um TOC que os dois leitores
    /// interpretam igual é um TOC em que dá para confiar; um que divergem é achado, não ruído.
    pub fn conferir_contra_libopus(pacote: &[u8]) -> std::result::Result<Toc, String> {
        let Some(toc) = Toc::do_pacote(pacote) else {
            return Err("pacote vazio: não há TOC para ler".into());
        };

        let banda_dela = unsafe { sys::opus_packet_get_bandwidth(pacote.as_ptr()) } as i32;
        if banda_dela != toc.largura_de_banda().codigo_da_libopus() {
            return Err(format!(
                "largura de banda: nós lemos {:?} ({}), a libopus leu {}",
                toc.largura_de_banda(),
                toc.largura_de_banda().codigo_da_libopus(),
                banda_dela
            ));
        }

        // A libopus responde em amostras a 48 kHz; nós respondemos em microssegundos. A 48 kHz,
        // uma amostra é 1/48 000 s, então `amostras * 1e6 / 48000` fecha exato para todos os
        // tamanhos de quadro do Opus — inclusive 2,5 ms, que são 120 amostras.
        let amostras = unsafe { sys::opus_packet_get_samples_per_frame(pacote.as_ptr(), 48_000) };
        let us_dela = (amostras as u64 * 1_000_000 / 48_000) as u32;
        if us_dela != toc.duracao_do_quadro_us() {
            return Err(format!(
                "duração do quadro: nós lemos {} µs, a libopus leu {} µs ({} amostras a 48 kHz)",
                toc.duracao_do_quadro_us(),
                us_dela,
                amostras
            ));
        }

        let canais_dela = unsafe { sys::opus_packet_get_nb_channels(pacote.as_ptr()) };
        let nossos = if toc.estereo { 2 } else { 1 };
        if canais_dela != nossos {
            return Err(format!(
                "canais: nós lemos {nossos}, a libopus leu {canais_dela}"
            ));
        }

        Ok(toc)
    }
}

/// Quantos quadros de Opus vêm neste pacote, pela contagem da própria libopus.
///
/// Vale mais que o `codigo_de_quadros` do TOC porque o código 3 manda a contagem num **segundo**
/// byte, que esta função lê. É a conferência direta da regra da §6 do `docs/audio.md`: o
/// pacotizador da libdatachannel não fragmenta, então **todo pacote nosso tem de ter exatamente
/// um quadro**. Mais de um quer dizer que alguém concatenou quadros numa chamada de
/// `enviar_audio`, que é um defeito que não produz erro em lugar nenhum do caminho.
pub fn quadros_no_pacote(pacote: &[u8]) -> Resultado<u32> {
    let n = unsafe { sys::opus_packet_get_nb_frames(pacote.as_ptr(), pacote.len() as i32) };
    if n < 0 {
        return Err(Erro {
            codigo: n,
            contexto: "opus_packet_get_nb_frames",
        });
    }
    Ok(n as u32)
}

/// O pacote carrega LBRR — a cópia de baixa taxa do quadro anterior?
///
/// **Este é o fato que o `useinbandfec` do SDP promete.** Ele não é legível no TOC: o LBRR mora no
/// cabeçalho SILK, atrás do decodificador de faixa, e quem sabe percorrê-lo é a libopus. Por isso
/// aqui não há leitor nosso concorrente — há a resposta do upstream.
///
/// # O pacote vazio **não** chega à libopus, e isso não é zelo
///
/// `opus_packet_has_lbrr` (`src/opus_decoder.c:1139` no fonte vendorizado) começa chamando
/// `opus_packet_get_mode(packet)` — que lê `packet[0]` — **sem nunca olhar o `len`**. Compare com
/// o vizinho `opus_packet_get_nb_frames`, na linha 1106 do mesmo arquivo, que abre com
/// `if (len<1) return OPUS_BAD_ARG;`. A guarda existe num e falta no outro.
///
/// Medido aqui em 2026-08-27: `tem_lbrr(&[])` **derruba o processo com SIGSEGV**. E isto é
/// alcançável da rede — o socorro que o jitter buffer oferece é um pacote que chegou do fio, e um
/// par quebrado ou hostil pode mandar RTP com carga de zero byte. Num workspace que usa
/// `panic = "abort"` e roda dentro de uma Broadcast Upload Extension, "derruba o processo" é o
/// processo hospedeiro.
///
/// A guarda fica aqui, e não em quem chama, pelo motivo de sempre: quem chama são quatro lugares
/// e vão virar mais.
pub fn tem_lbrr(pacote: &[u8]) -> Resultado<bool> {
    if pacote.is_empty() {
        return Err(Erro {
            codigo: sys::OPUS_BAD_ARG,
            contexto: "opus_packet_has_lbrr",
        });
    }
    let r = unsafe { sys::opus_packet_has_lbrr(pacote.as_ptr(), pacote.len() as i32) };
    if r < 0 {
        return Err(Erro {
            codigo: r,
            contexto: "opus_packet_has_lbrr",
        });
    }
    Ok(r == 1)
}

// -------------------------------------------------------------------------------------------
// Codificador
// -------------------------------------------------------------------------------------------

/// Para que serve o fluxo. Muda o compromisso que o encoder faz sozinho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aplicacao {
    /// Fala. Favorece inteligibilidade, e é o que empurra o encoder para SILK.
    Voz,
    /// Áudio geral: música, jogo, vídeo.
    Audio,
    /// Latência mínima, sem a camada SILK. **Não tem FEC embutido**, por construção.
    AtrasoBaixo,
}

impl Aplicacao {
    fn codigo(self) -> c_int {
        match self {
            Aplicacao::Voz => sys::OPUS_APPLICATION_VOIP,
            Aplicacao::Audio => sys::OPUS_APPLICATION_AUDIO,
            Aplicacao::AtrasoBaixo => sys::OPUS_APPLICATION_RESTRICTED_LOWDELAY,
        }
    }
}

/// Que tipo de sinal está entrando. `Automatico` deixa a análise do encoder decidir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sinal {
    Automatico,
    Voz,
    Musica,
}

impl Sinal {
    fn codigo(self) -> c_int {
        match self {
            Sinal::Automatico => sys::OPUS_AUTO,
            Sinal::Voz => sys::OPUS_SIGNAL_VOICE,
            Sinal::Musica => sys::OPUS_SIGNAL_MUSIC,
        }
    }
}

/// Um codificador de Opus.
///
/// **Tem estado, e isso muda como se confere um fluxo dele.** Diferente do G.711 — uma tabela de
/// consulta sem memória, em que todo quadro de uma mesma nota sai byte a byte idêntico —, o Opus
/// é preditivo: o quadro *n* depende dos anteriores. Duas consequências que custaram desenho na
/// sonda:
///
/// 1. **Não existe "quadro de referência" de uma nota.** A tabela de quatro quadros que a sonda
///    de PCMU usa para conferir conteúdo não tem equivalente aqui. A conferência de Opus é por
///    decodificação e comparação espectral, não por igualdade de bytes.
/// 2. **Comparar bytes entre dois aparelhos não é conferência válida.** Builds de ponto fixo e de
///    ponto flutuante produzem fluxos diferentes — e válidos — do mesmo PCM, e mesmo entre duas
///    builds de ponto flutuante o arredondamento de arquiteturas diferentes pode divergir. Ver
///    `docs/audio.md` §12.
pub struct Codificador {
    ptr: *mut sys::OpusEncoder,
    canais: usize,
}

// Ver a explicação em `sys::marcadores`: o estado é um bloco próprio, sem nada global. Mover
// entre threads é seguro; usar de duas ao mesmo tempo não é, e o `&mut self` de toda função é
// quem impede isso.
unsafe impl Send for Codificador {}

impl Codificador {
    /// `taxa_hz` tem de ser 8000, 12000, 16000, 24000 ou 48000.
    ///
    /// **No Quall é sempre 48 000.** A RFC 7587 §4.1 fixa o relógio de RTP do Opus em 48 kHz
    /// independentemente da taxa interna, e o `docs/audio.md` §2 já registrou o que erra quem
    /// desalinha isso: áudio que acelera ou arrasta sem um contador acusando.
    pub fn novo(taxa_hz: u32, canais: u8, aplicacao: Aplicacao) -> Resultado<Codificador> {
        let mut erro: c_int = 0;
        let ptr = unsafe {
            sys::opus_encoder_create(
                taxa_hz as i32,
                c_int::from(canais),
                aplicacao.codigo(),
                &mut erro,
            )
        };
        if ptr.is_null() || erro != sys::OPUS_OK {
            return Err(Erro {
                codigo: erro as i32,
                contexto: "opus_encoder_create",
            });
        }
        Ok(Codificador {
            ptr,
            canais: usize::from(canais),
        })
    }

    fn ctl(&mut self, pedido: c_int, valor: c_int, contexto: &'static str) -> Resultado<()> {
        conferir(
            unsafe { sys::opus_encoder_ctl(self.ptr, pedido, valor) },
            contexto,
        )
    }

    /// A taxa média alvo, em bits por segundo. Vira o `maxaveragebitrate` do fmtp.
    pub fn definir_taxa_de_bits(&mut self, bits_por_segundo: u32) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_BITRATE_REQUEST,
            bits_por_segundo as c_int,
            "OPUS_SET_BITRATE",
        )
    }

    /// Liga ou desliga o FEC embutido (LBRR). Vira o `useinbandfec` do fmtp.
    ///
    /// **Ligar não basta para o LBRR aparecer.** Ele só é emitido em SILK ou híbrido, e o encoder
    /// só gasta bits com ele se achar que vale — o que depende da perda declarada em
    /// [`Codificador::definir_perda_esperada`]. Com perda 0% o encoder pode, legitimamente, não
    /// emitir LBRR nenhum mesmo com o FEC ligado. É exatamente o tipo de promessa de API que este
    /// projeto manda conferir no fio: quem responde é [`tem_lbrr`].
    pub fn definir_fec_embutido(&mut self, ligado: bool) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_INBAND_FEC_REQUEST,
            c_int::from(ligado),
            "OPUS_SET_INBAND_FEC",
        )
    }

    /// A perda de pacote que o encoder deve supor, em porcento. É o que faz o LBRR valer a pena.
    pub fn definir_perda_esperada(&mut self, porcento: u8) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_PACKET_LOSS_PERC_REQUEST,
            c_int::from(porcento.min(100)),
            "OPUS_SET_PACKET_LOSS_PERC",
        )
    }

    /// DTX: parar de transmitir no silêncio. Vira o `usedtx` do fmtp.
    ///
    /// O Quall o mantém **desligado** nos dois presets — ver `docs/audio.md` §3.
    pub fn definir_dtx(&mut self, ligado: bool) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_DTX_REQUEST,
            c_int::from(ligado),
            "OPUS_SET_DTX",
        )
    }

    /// Taxa variável.
    pub fn definir_vbr(&mut self, ligado: bool) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_VBR_REQUEST,
            c_int::from(ligado),
            "OPUS_SET_VBR",
        )
    }

    /// Dica sobre o conteúdo.
    pub fn definir_sinal(&mut self, sinal: Sinal) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_SIGNAL_REQUEST,
            sinal.codigo(),
            "OPUS_SET_SIGNAL",
        )
    }

    /// Força mono ou estéreo na saída, independentemente dos canais de entrada.
    pub fn definir_canais_forcados(&mut self, canais: Option<u8>) -> Resultado<()> {
        let v = match canais {
            None => sys::OPUS_AUTO,
            Some(n) => c_int::from(n),
        };
        self.ctl(
            sys::OPUS_SET_FORCE_CHANNELS_REQUEST,
            v,
            "OPUS_SET_FORCE_CHANNELS",
        )
    }

    /// Teto de largura de banda que o encoder pode usar.
    pub fn definir_largura_maxima(&mut self, largura: LarguraDeBanda) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_MAX_BANDWIDTH_REQUEST,
            largura.codigo_da_libopus() as c_int,
            "OPUS_SET_MAX_BANDWIDTH",
        )
    }

    /// Complexidade, 0 a 10. Custa CPU; no A10s isso não é detalhe.
    pub fn definir_complexidade(&mut self, nivel: u8) -> Resultado<()> {
        self.ctl(
            sys::OPUS_SET_COMPLEXITY_REQUEST,
            c_int::from(nivel.min(10)),
            "OPUS_SET_COMPLEXITY",
        )
    }

    /// O lookahead do encoder, em amostras. É a metade do atraso algorítmico que o
    /// `docs/audio.md` §2 contabiliza como 6,5 ms — e agora dá para **medir** em vez de citar.
    pub fn lookahead(&mut self) -> Resultado<u32> {
        let mut v: c_int = 0;
        conferir(
            unsafe { sys::opus_encoder_ctl(self.ptr, sys::OPUS_GET_LOOKAHEAD_REQUEST, &mut v) },
            "OPUS_GET_LOOKAHEAD",
        )?;
        Ok(v as u32)
    }

    /// Codifica **um** quadro. `pcm` é intercalado e tem de ter
    /// `amostras_por_canal * canais` valores.
    ///
    /// Devolve quantos bytes de `saida` foram usados. Um retorno de 1 byte não é erro: é o pacote
    /// vazio que o DTX emite — e é por isso que o Quall mantém `usedtx=0`.
    pub fn codificar(&mut self, pcm: &[i16], saida: &mut [u8]) -> Resultado<usize> {
        if self.canais == 0 || pcm.len() % self.canais != 0 {
            return Err(Erro {
                codigo: sys::OPUS_BAD_ARG,
                contexto: "pcm não é múltiplo do número de canais",
            });
        }
        let amostras_por_canal = pcm.len() / self.canais;
        let n = unsafe {
            sys::opus_encode(
                self.ptr,
                pcm.as_ptr(),
                amostras_por_canal as c_int,
                saida.as_mut_ptr(),
                saida.len() as i32,
            )
        };
        if n < 0 {
            return Err(Erro {
                codigo: n,
                contexto: "opus_encode",
            });
        }
        Ok(n as usize)
    }
}

impl Drop for Codificador {
    fn drop(&mut self) {
        unsafe { sys::opus_encoder_destroy(self.ptr) }
    }
}

// -------------------------------------------------------------------------------------------
// Decodificador
// -------------------------------------------------------------------------------------------

/// Um decodificador de Opus.
pub struct Decodificador {
    ptr: *mut sys::OpusDecoder,
    canais: usize,
}

unsafe impl Send for Decodificador {}

impl Decodificador {
    pub fn novo(taxa_hz: u32, canais: u8) -> Resultado<Decodificador> {
        let mut erro: c_int = 0;
        let ptr =
            unsafe { sys::opus_decoder_create(taxa_hz as i32, c_int::from(canais), &mut erro) };
        if ptr.is_null() || erro != sys::OPUS_OK {
            return Err(Erro {
                codigo: erro as i32,
                contexto: "opus_decoder_create",
            });
        }
        Ok(Decodificador {
            ptr,
            canais: usize::from(canais),
        })
    }

    /// Decodifica um pacote. Devolve quantas amostras **por canal** foram escritas.
    pub fn decodificar(&mut self, pacote: &[u8], pcm: &mut [i16]) -> Resultado<usize> {
        self.chamar(Some(pacote), pcm, false)
    }

    /// Ocultação de perda: pede ao decoder que invente o quadro que não chegou.
    ///
    /// É o que o jitter buffer da §4 do `docs/audio.md` vai chamar quando um pacote faltar — e o
    /// motivo de `QuadroDeAudio` carregar o número de sequência cru.
    pub fn ocultar_perda(&mut self, pcm: &mut [i16]) -> Resultado<usize> {
        self.chamar(None, pcm, false)
    }

    /// Recupera o quadro **anterior** a partir do LBRR deste pacote. É o FEC embutido em uso.
    ///
    /// Só produz algo se o pacote tiver LBRR — ver [`tem_lbrr`]. Sem LBRR o decoder cai na
    /// ocultação de perda, sem avisar que caiu; é mais um lugar onde a promessa precisa ser
    /// conferida no fio.
    pub fn decodificar_fec(&mut self, pacote: &[u8], pcm: &mut [i16]) -> Resultado<usize> {
        self.chamar(Some(pacote), pcm, true)
    }

    fn chamar(&mut self, pacote: Option<&[u8]>, pcm: &mut [i16], fec: bool) -> Resultado<usize> {
        if self.canais == 0 || pcm.len() % self.canais != 0 {
            return Err(Erro {
                codigo: sys::OPUS_BAD_ARG,
                contexto: "pcm não é múltiplo do número de canais",
            });
        }
        let capacidade = pcm.len() / self.canais;
        let (ptr, tamanho) = match pacote {
            Some(p) => (p.as_ptr(), p.len() as i32),
            None => (std::ptr::null(), 0),
        };
        let n = unsafe {
            sys::opus_decode(
                self.ptr,
                ptr,
                tamanho,
                pcm.as_mut_ptr(),
                capacidade as c_int,
                c_int::from(fec),
            )
        };
        if n < 0 {
            return Err(Erro {
                codigo: n,
                contexto: "opus_decode",
            });
        }
        Ok(n as usize)
    }
}

impl Drop for Decodificador {
    fn drop(&mut self) {
        unsafe { sys::opus_decoder_destroy(self.ptr) }
    }
}

#[cfg(test)]
mod testes;
