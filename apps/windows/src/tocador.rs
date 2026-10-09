//! **O Windows toca** (S6 do `docs/som-no-receptor.md`): a porta puxada do núcleo tocada por
//! **WASAPI compartilhado, por evento, com `RATEADJUST`**.
//!
//! # O caminho
//!
//! ```text
//! ReproducaoPuxada (núcleo, casca_reamostra = true)
//!   → decodificação (Opus pelo quall-opus; PCMU pela tabela e pelo interpolador por 6)
//!   → Montador (som_puxado.rs: slots de 20 ms nos pedidos do dispositivo, com o atraso até o DAC)
//!   → ganho em rampa (D1 e D3)
//!   → IAudioRenderClient, 48 kHz float estéreo, sempre
//! ```
//!
//! - **A razão do núcleo vai para o `IAudioClockAdjustment::SetSampleRate`** (`RATEADJUST`): o
//!   mixador do Windows consome a 48 000 × r, e o nível do buffer do núcleo fica parado com o
//!   emissor noutro cristal. É o Varispeed do Mac, pelo lado do sistema.
//! - **O formato do mixador que não é o do motor** — outra taxa (R14 do §11) **ou outros canais** (a
//!   saída 5.1 ou 7.1 a 48 kHz de um fone de jogo ou de um receptor por HDMI) — é convertido pelo
//!   próprio WASAPI: `AUTOCONVERTPCM | SRC_DEFAULT_QUALITY` vão **sempre** (crítica 13, G1). Sem
//!   eles, no modo compartilhado, o motor não põe o conversor de canais e o `Initialize` recusa o
//!   estéreo num mixador de 6 ou 8 canais — mudo para sempre. O formato do mixador (taxa, canais,
//!   subformato e máscara) vai para o diário. Se o `Initialize` recusar a combinação com o
//!   `RATEADJUST`, a saída não liga, o motivo vai para a janela e o diário, e a tentativa se
//!   repete — **não medido**: o Dell tem um endpoint só, estéreo a 48 kHz.
//! - **O atraso até o DAC** de cada puxada é o `GetCurrentPadding` (o que o mixador já tem do
//!   fluxo) mais o `GetStreamLatency` — com o período padrão do dispositivo como piso, porque no
//!   Dell ele devolveu zero —, mais o que o pedido já escreveu antes do slot.
//!
//! - **A thread do render entra no MMCSS** ("Pro Audio", `AvSetMmThreadCharacteristicsW`). Recusado,
//!   segue sem, e o diário diz.
//! - **O render nunca espera cadeado de outra thread** (crítica 13, M5): a razão chega por um
//!   atômico que o dono do tocador atualiza ([`Tocador::atualizar_razao`], fora do render), e a
//!   situação é publicada por `try_lock` — perde uma publicação, como o núcleo, e guarda os picos
//!   para a próxima.
//!
//! # A troca de saída, e o dispositivo que some (crítica 2, M4)
//!
//! A saída padrão é conferida duas vezes por segundo, pelo id do endpoint, **depois** de o pedido
//! do mixador estar atendido; mudou, a saída é remontada no novo padrão. O §7.1 pedia o
//! `ActivateAudioInterfaceAsync` sobre a interface padrão de render, que o sistema migra sozinho;
//! ficou a conferência, porque ela dá o mesmo resultado com meio segundo de atraso no máximo, sem
//! um manipulador COM de conclusão (`IActivateAudioInterfaceCompletionHandler` e `IAgileObject`),
//! e porque a remontagem relê a latência e o tamanho do buffer do dispositivo novo, que a
//! migração do sistema esconderia do atraso até o DAC.
//!
//! `AUDCLNT_E_DEVICE_INVALIDATED` em qualquer chamada (o fone tirado, o driver reiniciado) também
//! remonta. Uma montagem que falha tenta de novo com espera crescente
//! (0,2 · 0,5 · 1 · 2 · 4 s, e depois a cada 4 s), e a janela diz "sem som" enquanto isso — a
//! mesma política do Mac (crítica 9, M7). A porta do núcleo continua viva no meio: a imagem não
//! para, e o som volta onde a porta estiver.
//!
//! # A Sessão 0 e a sessão interativa
//!
//! **Na Sessão 0 do SSH do Dell a saída padrão abre** (18/09/2026: o Realtek, 48 kHz, `RATEADJUST` e
//! MMCSS aceitos, com o `quall-som-local` mudo). Se o que a Sessão 0 escreve chega ao alto-falante
//! não foi testado. Sem saída nenhuma, a montagem falha com o motivo dito e o resto do receptor
//! segue. A prova tocando é `apps/windows/scripts/prova-som-receptor.ps1`, com o sim do Bruno.
//!
//! # O que este arquivo nunca faz
//!
//! Não abre entrada nenhuma: nem microfone, nem loopback. O único cliente de áudio aqui é de
//! renderização.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioClockAdjustment, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_DEVICE_INVALIDATED,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_RATEADJUST, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject,
};

use quall_core::jitter::Entrega;
use quall_core::portao::Barreira;
use quall_core::reproducao::{ContadoresDeReproducao, LeitorDeReproducao, Puxado, ReproducaoPuxada};
use quall_core::track::CodecDeAudio;
use quall_opus::Decodificador;

use crate::som_puxado::{
    aplicar_ganho, mulaw_para_f32, pico, DetectorDeEstouro, Estouro, InterpoladorPor6, Montador,
    Ordem, Retrato, SlotPuxado, CANAIS, INTERPOLADOR_ATRASO_US, QUADROS_POR_SLOT, TAXA,
};

/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
const SUBTIPO_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
/// `KSDATAFORMAT_SUBTYPE_PCM`.
const SUBTIPO_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// `SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT`.
const FRENTE_ESQUERDA_E_DIREITA: u32 = 0x3;
/// O buffer pedido ao mixador: 30 ms, o teto do §7.1 (20 a 30 ms). O período do evento é o do
/// mixador (10 ms no modo compartilhado). O WASAPI arredonda para cima: no Realtek do Dell vieram
/// 2 136 quadros (44,5 ms), e o que vale é o `GetBufferSize`.
const BUFFER_100NS: i64 = 300_000;
const ESPERAS: [f64; 5] = [0.2, 0.5, 1.0, 2.0, 4.0];

// -------------------------------------------------------------------------------------------------
// O que a janela e a sessão escrevem, e o que elas leem
// -------------------------------------------------------------------------------------------------

/// Escrito pela sessão e pela janela, lido pelo render; e o que o render conta para quem relata.
/// Sem cadeado: atômicos.
pub struct ControleDoSom {
    ganho_alvo: AtomicU32,
    parar: AtomicBool,
    /// A razão sugerida pelo núcleo, em bits de `f64`, posta aqui **fora** do render
    /// ([`Tocador::atualizar_razao`]): ler o `LeitorDeReproducao` é tomar o cadeado dele, e o render
    /// não espera cadeado (crítica 13, M5).
    razao_bits: AtomicU64,
    /// Quantas vezes o Opus devolveu erro (o slot vira 20 ms de zeros). Lido no relato.
    falhas_de_decodificar: AtomicU64,
    /// `--claquete` (bancada, a S7): o render procura o estouro da claquete no som que sai.
    claquete: AtomicBool,
    /// Os estouros achados, para o relato de 1 Hz. O render só **tenta** o cadeado (`try_lock`) e
    /// nunca cresce o vetor: com o relato segurando, ou cheio, o estouro se perde e o relato conta
    /// um evento a menos — o render não espera ninguém.
    estouros: Mutex<Vec<Estouro>>,
}

impl ControleDoSom {
    pub fn aplicar_ganho(&self, g: f32) {
        // NaN não passa pelo `clamp`: o ganho que não é número vira mudo, e não NaN no WASAPI.
        let g = if g.is_finite() { g.clamp(0.0, 1.0) } else { 0.0 };
        self.ganho_alvo.store(g.to_bits(), Ordering::Relaxed);
    }
    fn ganho(&self) -> f32 {
        f32::from_bits(self.ganho_alvo.load(Ordering::Relaxed))
    }
    fn razao(&self) -> f64 {
        f64::from_bits(self.razao_bits.load(Ordering::Relaxed))
    }
}

/// O que o render publica, para o relato de 1 Hz e para a janela.
#[derive(Clone, Debug, Default)]
pub struct SituacaoDoSom {
    pub ligado: bool,
    /// O endpoint e o formato, como o mixador os descreveu.
    pub saida: String,
    pub taxa_do_mixador: u32,
    pub com_rateadjust: bool,
    pub religamentos: u64,
    pub tentativas_falhas: u64,
    pub ultima_falha: String,
    pub razao_aplicada: f64,
    pub latencia_ms: f64,
    pub buffer_quadros: u32,
    /// Os canais do mixador (2 no Dell; 6 ou 8 numa saída 5.1 ou 7.1).
    pub canais_do_mixador: u16,
    pub retrato: Retrato,
    /// Pico absoluto da saída, depois do ganho, desde a última leitura por [`Tocador::tirar_picos`].
    pub pico: f32,
    /// O mesmo, **antes** do ganho: o que a decodificação entregou. Com o mudo, este anda e o de
    /// cima fica em zero — é a prova de que o mudo não para o motor.
    pub pico_do_sinal: f32,
}

/// O tocador de uma sessão. Nasce com a porta puxada da track de som e a devolve no `parar`,
/// para a sessão encerrá-la com barreira antes de largar a track.
pub struct Tocador {
    controle: Arc<ControleDoSom>,
    situacao: Arc<Mutex<SituacaoDoSom>>,
    leitor: LeitorDeReproducao,
    thread: Option<std::thread::JoinHandle<ReproducaoPuxada>>,
    pub codec: CodecDeAudio,
}

impl Tocador {
    /// Sobe a thread do render. `canais_do_fio` é o do preset da espécie (2 no som do sistema, 1
    /// no microfone; o PCMU é sempre 1). `ganho` vale **desde o primeiro ciclo**.
    pub fn iniciar(
        porta: ReproducaoPuxada,
        codec: CodecDeAudio,
        canais_do_fio: u8,
        ganho: f32,
    ) -> std::result::Result<Tocador, String> {
        let decodificacao = Decodificacao::nova(codec, canais_do_fio)?;
        let controle = Arc::new(ControleDoSom {
            ganho_alvo: AtomicU32::new(0f32.to_bits()),
            parar: AtomicBool::new(false),
            razao_bits: AtomicU64::new(1f64.to_bits()),
            falhas_de_decodificar: AtomicU64::new(0),
            claquete: AtomicBool::new(false),
            estouros: Mutex::new(Vec::with_capacity(256)),
        });
        controle.aplicar_ganho(ganho);
        let situacao = Arc::new(Mutex::new(SituacaoDoSom { razao_aplicada: 1.0, ..Default::default() }));
        let leitor = porta.leitor();
        let (c, s) = (Arc::clone(&controle), Arc::clone(&situacao));
        let thread = std::thread::Builder::new()
            .name("quall.som".into())
            .spawn(move || correr(porta, decodificacao, c, s))
            .map_err(|e| format!("a thread do som não subiu: {e}"))?;
        Ok(Tocador { controle, situacao, leitor, thread: Some(thread), codec })
    }

    pub fn controle(&self) -> Arc<ControleDoSom> {
        Arc::clone(&self.controle)
    }

    pub fn situacao(&self) -> SituacaoDoSom {
        self.situacao.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Os picos desde a última chamada, `(do sinal, na saída)`, e zera. **Só o relato de 1 Hz
    /// chama.**
    pub fn tirar_picos(&self) -> (f32, f32) {
        self.situacao
            .lock()
            .map(|mut s| (std::mem::take(&mut s.pico_do_sinal), std::mem::take(&mut s.pico)))
            .unwrap_or((0.0, 0.0))
    }

    pub fn contadores_da_porta(&self) -> ContadoresDeReproducao {
        self.leitor.contadores()
    }

    /// Leva a razão sugerida pelo núcleo ao render. **Do dono do tocador**, fora do render (o
    /// laço da sessão do receptor, a cada 100 ms; o `quall-som-local`, a cada 100 ms): é aqui que
    /// o cadeado do `LeitorDeReproducao` é tomado. `None` (não medida) é 1.
    pub fn atualizar_razao(&self) {
        let r = self
            .leitor
            .razao_sugerida()
            .filter(|r| r.is_finite())
            .unwrap_or(1.0)
            .clamp(1.0 - 500e-6, 1.0 + 500e-6);
        self.controle.razao_bits.store(r.to_bits(), Ordering::Relaxed);
    }

    /// `--claquete` (bancada, a S7): liga o detector do estouro no render.
    pub fn ligar_claquete(&self) {
        self.controle.claquete.store(true, Ordering::Relaxed);
    }

    /// Os estouros achados desde a última chamada. **Só o relato de 1 Hz chama.**
    pub fn tirar_estouros(&self) -> Vec<Estouro> {
        self.controle.estouros.lock().map(|mut v| v.drain(..).collect()).unwrap_or_default()
    }

    /// Quantos slots Opus viraram zeros por erro de decodificação, desde o começo.
    pub fn falhas_de_decodificar(&self) -> u64 {
        self.controle.falhas_de_decodificar.load(Ordering::Relaxed)
    }

    /// Para o render, **de forma síncrona**, e encerra a porta com barreira. Depois de voltar,
    /// nenhum render roda e a track de som pode ser largada.
    pub fn parar(mut self) -> Barreira {
        self.controle.parar.store(true, Ordering::SeqCst);
        match self.thread.take().map(|t| t.join()) {
            Some(Ok(porta)) => porta.encerrar(),
            // A thread entrou em pânico: a porta foi junto, e o núcleo a solta no `Drop`.
            _ => Barreira::Cumprida,
        }
    }
}

impl Drop for Tocador {
    fn drop(&mut self) {
        self.controle.parar.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            if let Ok(porta) = t.join() {
                let _ = porta.encerrar();
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Decodificar
// -------------------------------------------------------------------------------------------------

struct Decodificacao {
    opus: Option<Decodificador>,
    /// Quanto o conteúdo decodificado sai atrás do carimbo pelo codec (6,5 ms no Opus, o lookahead
    /// do codificador do emissor; 0 no PCMU): `CodecDeAudio::atraso_do_conteudo_us` do núcleo.
    atraso_do_conteudo_us: f64,
    canais_do_fio: usize,
    interp: InterpoladorPor6,
    pcm: Vec<i16>,
    a8k: Vec<f32>,
    a48: Vec<f32>,
    ultima_pcmu: f32,
    falhas: u64,
}

impl Decodificacao {
    /// O que o sinal que sai daqui tem de atraso contra o carimbo: o filtro por 6 do PCMU, ou o
    /// lookahead do codificador Opus do emissor.
    fn atraso_interno_us(&self) -> f64 {
        (if self.opus.is_none() { INTERPOLADOR_ATRASO_US } else { 0.0 }) + self.atraso_do_conteudo_us
    }

    fn nova(codec: CodecDeAudio, canais_do_fio: u8) -> std::result::Result<Self, String> {
        let (opus, canais) = match codec {
            CodecDeAudio::Opus => {
                let canais = canais_do_fio.clamp(1, 2);
                let d = Decodificador::novo(TAXA, canais).map_err(|e| format!("o decodificador Opus não abriu: {e:?}"))?;
                (Some(d), usize::from(canais))
            }
            CodecDeAudio::Pcmu => (None, 1),
        };
        Ok(Decodificacao {
            opus,
            atraso_do_conteudo_us: f64::from(codec.atraso_do_conteudo_us()),
            canais_do_fio: canais,
            interp: InterpoladorPor6::novo(),
            pcm: vec![0; QUADROS_POR_SLOT * 2],
            a8k: vec![0.0; 160],
            a48: vec![0.0; QUADROS_POR_SLOT],
            ultima_pcmu: 0.0,
            falhas: 0,
        })
    }

    /// Escreve o slot em `destino` (48 kHz, estéreo intercalado).
    fn escrever(&mut self, puxado: Puxado<'_>, destino: &mut [f32]) -> SlotPuxado {
        let (ordem, carimbo) = match &puxado {
            Puxado::Ocioso => return SlotPuxado::ocioso(),
            Puxado::Slot(Entrega::Quadro { timestamp_us, .. }) => (Ordem::Quadro, *timestamp_us),
            Puxado::Slot(Entrega::Fec { timestamp_us, .. }) => (Ordem::Cura, *timestamp_us),
            Puxado::Slot(Entrega::Silencio { timestamp_us, .. }) => (Ordem::Silencio, *timestamp_us),
        };
        match self.opus.as_mut() {
            None => {
                // PCMU: a tabela, e o interpolador por 6. Silêncio e socorro: uma rampa de 5 ms da
                // última amostra até zero (sem o estalo do corte seco, crítica 9, miúdo 3).
                match &puxado {
                    Puxado::Slot(Entrega::Quadro { payload, .. }) if payload.len() >= 160 => {
                        for (a, &u) in self.a8k.iter_mut().zip(payload.iter()) {
                            *a = mulaw_para_f32(u);
                        }
                    }
                    _ => {
                        let de = self.ultima_pcmu;
                        for (i, a) in self.a8k.iter_mut().enumerate() {
                            *a = if i < 40 { de * (1.0 - (i + 1) as f32 / 40.0) } else { 0.0 };
                        }
                    }
                }
                self.ultima_pcmu = self.a8k[159];
                self.interp.processar(&self.a8k, &mut self.a48);
                for (q, &v) in self.a48.iter().enumerate() {
                    destino[q * CANAIS] = v;
                    destino[q * CANAIS + 1] = v;
                }
            }
            Some(d) => {
                let n = self.canais_do_fio;
                let pcm = &mut self.pcm[..QUADROS_POR_SLOT * n];
                let r = match &puxado {
                    Puxado::Slot(Entrega::Quadro { payload, .. }) => d.decodificar(payload, pcm),
                    // Só com LBRR de verdade: sem ele, `decode_fec` cai na ocultação e devolve
                    // sucesso (a armadilha que `Entrega::Fec` descreve).
                    Puxado::Slot(Entrega::Fec { socorro, .. })
                        if quall_opus::tem_lbrr(socorro).unwrap_or(false) =>
                    {
                        d.decodificar_fec(socorro, pcm)
                    }
                    _ => d.ocultar_perda(pcm),
                };
                let escritas = match r {
                    Ok(k) => k.min(QUADROS_POR_SLOT),
                    Err(_) => {
                        self.falhas += 1;
                        0
                    }
                };
                for q in 0..QUADROS_POR_SLOT {
                    let (e, dd) = if q < escritas {
                        if n == 1 {
                            let v = f32::from(pcm[q]) / 32768.0;
                            (v, v)
                        } else {
                            (f32::from(pcm[q * 2]) / 32768.0, f32::from(pcm[q * 2 + 1]) / 32768.0)
                        }
                    } else {
                        (0.0, 0.0)
                    };
                    destino[q * CANAIS] = e;
                    destino[q * CANAIS + 1] = dd;
                }
            }
        }
        SlotPuxado { ordem, quadros: QUADROS_POR_SLOT, carimbo_us: carimbo }
    }
}

// -------------------------------------------------------------------------------------------------
// A thread do render
// -------------------------------------------------------------------------------------------------

/// Por que o laço de uma saída voltou.
enum Fim {
    Parado,
    Trocou,
    Invalidado,
    Falhou(String),
}

struct Saida {
    /// O enumerador da montagem, reaproveitado na conferência do padrão: nada de `CoCreateInstance`
    /// duas vezes por segundo na thread do render.
    enumerador: IMMDeviceEnumerator,
    cliente: IAudioClient,
    render: IAudioRenderClient,
    ajuste: Option<IAudioClockAdjustment>,
    evento: HANDLE,
    tamanho: u32,
    latencia_us: f64,
    id: String,
}

impl Drop for Saida {
    fn drop(&mut self) {
        unsafe {
            let _ = self.cliente.Stop();
            let _ = CloseHandle(self.evento);
        }
    }
}

struct FimDoCom;
impl Drop for FimDoCom {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn enumerador() -> windows::core::Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn ler_id(d: &IMMDevice) -> String {
    unsafe {
        match d.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

/// O id da saída padrão agora, ou vazio.
fn id_da_saida_padrao(e: &IMMDeviceEnumerator) -> String {
    unsafe { e.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map(|d| ler_id(&d))
        .unwrap_or_default()
}

fn formato_do_motor() -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE,
            nChannels: CANAIS as u16,
            nSamplesPerSec: TAXA,
            nAvgBytesPerSec: TAXA * (CANAIS as u32) * 4,
            nBlockAlign: (CANAIS as u16) * 4,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
        dwChannelMask: FRENTE_ESQUERDA_E_DIREITA,
        SubFormat: SUBTIPO_FLOAT,
    }
}

/// O formato do mixador, lido da `GetMixFormat`: taxa, canais, e o subformato e a máscara quando
/// ele é `WAVE_FORMAT_EXTENSIBLE`.
struct FormatoDoMixador {
    taxa: u32,
    canais: u16,
    descricao: String,
}

/// # Safety
/// `p` vem da `GetMixFormat` e ainda não foi solto.
unsafe fn ler_formato_do_mixador(p: *const WAVEFORMATEX) -> FormatoDoMixador {
    let base: WAVEFORMATEX = std::ptr::read_unaligned(p);
    let (taxa, canais, bits, tag) = (base.nSamplesPerSec, base.nChannels, base.wBitsPerSample, base.wFormatTag);
    let resto = if tag == WAVE_FORMAT_EXTENSIBLE && base.cbSize >= 22 {
        let ext: WAVEFORMATEXTENSIBLE = std::ptr::read_unaligned(p as *const WAVEFORMATEXTENSIBLE);
        let sub = ext.SubFormat;
        let mascara = ext.dwChannelMask;
        let nome = if sub == SUBTIPO_FLOAT {
            "float".to_string()
        } else if sub == SUBTIPO_PCM {
            "pcm".to_string()
        } else {
            format!("{sub:?}")
        };
        format!("{nome} mascara=0x{mascara:x}")
    } else {
        format!("tag=0x{tag:04x}")
    };
    FormatoDoMixador { taxa, canais, descricao: format!("{taxa} Hz x {canais} canais {bits} bits {resto}") }
}

/// Abre a saída padrão no formato do motor. Devolve também a descrição e o formato do mixador.
fn montar() -> std::result::Result<(Saida, String, FormatoDoMixador, bool), String> {
    let texto = |o: &str, e: windows::core::Error| format!("{o}: {e}");
    let enumerador = enumerador().map_err(|e| texto("MMDeviceEnumerator", e))?;
    let dispositivo = unsafe { enumerador.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| texto("sem saída de som padrão nesta sessão", e))?;
    let id = ler_id(&dispositivo);
    let cliente: IAudioClient =
        unsafe { dispositivo.Activate(CLSCTX_ALL, None) }.map_err(|e| texto("Activate", e))?;
    let mixador = unsafe {
        let p = cliente.GetMixFormat().map_err(|e| texto("GetMixFormat", e))?;
        let f = ler_formato_do_mixador(p);
        CoTaskMemFree(Some(p as *const _));
        f
    };
    let formato = formato_do_motor();
    // **Sempre** o conversor do WASAPI (crítica 13, G1): a taxa (R14) e os canais. Quando o
    // formato já bate, ele não custa nada.
    let bandeiras = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_RATEADJUST
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    unsafe {
        cliente.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            bandeiras,
            BUFFER_100NS,
            0,
            &formato as *const WAVEFORMATEXTENSIBLE as *const WAVEFORMATEX,
            None,
        )
    }
    .map_err(|e| texto(&format!("Initialize (48 kHz float estéreo, mixador {})", mixador.descricao), e))?;
    let evento = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map_err(|e| texto("CreateEventW", e))?;
    let montada = (|| -> windows::core::Result<(IAudioRenderClient, Option<IAudioClockAdjustment>, u32, i64, i64)> {
        unsafe {
            cliente.SetEventHandle(evento)?;
            let tamanho = cliente.GetBufferSize()?;
            let render: IAudioRenderClient = cliente.GetService()?;
            let ajuste: Option<IAudioClockAdjustment> = cliente.GetService().ok();
            let latencia = cliente.GetStreamLatency()?;
            let mut periodo = 0i64;
            let _ = cliente.GetDevicePeriod(Some(&mut periodo), None);
            // O primeiro buffer em silêncio, para o `Start` não tocar lixo.
            let _ = render.GetBuffer(tamanho)?;
            render.ReleaseBuffer(tamanho, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
            Ok((render, ajuste, tamanho, latencia, periodo))
        }
    })();
    let (render, ajuste, tamanho, latencia_declarada, periodo) = match montada {
        Ok(m) => m,
        Err(e) => {
            unsafe {
                let _ = CloseHandle(evento);
            }
            return Err(texto("montar o cliente de renderização", e));
        }
    };
    if let Err(e) = unsafe { cliente.Start() } {
        unsafe {
            let _ = CloseHandle(evento);
        }
        return Err(texto("Start", e));
    }
    let com_rateadjust = ajuste.is_some();
    // **O piso do período.** No Dell, na Sessão 0, o `GetStreamLatency` do Realtek com
    // `RATEADJUST` devolveu **zero** (18/09/2026). O mixador consome o que está na frente do buffer
    // um período depois, no mínimo; zero faria o atraso até o DAC ser só a fila. Quando a latência
    // declarada é menor que o período padrão do dispositivo, vale o período.
    let latencia = latencia_declarada.max(periodo);
    let descricao = format!(
        "id={id} mixador=[{}] buffer={tamanho} quadros latencia={:.1} ms \
         (declarada {:.1} ms, periodo {:.1} ms){}",
        mixador.descricao,
        latencia as f64 / 10_000.0,
        latencia_declarada as f64 / 10_000.0,
        periodo as f64 / 10_000.0,
        if com_rateadjust { "" } else { " SEM RATEADJUST" }
    );
    Ok((
        Saida { enumerador, cliente, render, ajuste, evento, tamanho, latencia_us: latencia as f64 / 10.0, id },
        descricao,
        mixador,
        com_rateadjust,
    ))
}

fn e_invalidado(e: &windows::core::Error) -> bool {
    e.code() == AUDCLNT_E_DEVICE_INVALIDATED
}

fn correr(
    mut porta: ReproducaoPuxada,
    mut dec: Decodificacao,
    controle: Arc<ControleDoSom>,
    situacao: Arc<Mutex<SituacaoDoSom>>,
) -> ReproducaoPuxada {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let _fim = FimDoCom;
    let mut indice_da_tarefa = 0u32;
    let mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut indice_da_tarefa) };
    match &mmcss {
        Ok(_) => crate::registro::linha("som: a thread do render está no MMCSS (Pro Audio)"),
        Err(e) => crate::registro::linha(format!("som: o MMCSS recusou a thread do render ({e}); segue sem")),
    }
    // A hora do render (`inicio`) e o QPC lidos juntos: o `Instant` do Windows **é** o QPC, e a
    // claquete (S7) leva a hora do DAC para o relógio da imagem (`cano::qpc_us`) por esta soma.
    let base_qpc_us = crate::cano::qpc_us();
    let inicio = Instant::now();
    // O detector do estouro (S7): criado aqui, fora do render, para o render não alocar. O PCMU
    // passa pelo interpolador, que atrasa o sinal 1,49 ms, e o Opus sai 6,5 ms atrás do carimbo
    // pelo lookahead do codificador do emissor (o T0 do Mac mediu +6,6 sem o desconto,
    // `docs/som-no-receptor.md` §20.7 e §21): o carimbo do estouro desconta os dois, e a hora do DAC
    // não (o que sai no DAC é o sinal já atrasado). O atraso passado ao núcleo continua o físico
    // (`contrato-som-puxado.md`); o Mac é que soma o interno ao dele.
    let mut detector = DetectorDeEstouro::novo(f64::from(TAXA), QUADROS_POR_SLOT);
    let atraso_interno_us = dec.atraso_interno_us();
    {
        let ctl = &controle;
        let fonte = |atraso_us: u32, no_dac: u64, razao: f64, destino: &mut [f32]| {
            let p = porta.puxar(atraso_us, razao);
            let slot = dec.escrever(p, destino);
            ctl.falhas_de_decodificar.store(dec.falhas, Ordering::Relaxed);
            if ctl.claquete.load(Ordering::Relaxed) && slot.ordem != Ordem::Ocioso && slot.quadros > 0 {
                let n = (slot.quadros * CANAIS).min(destino.len());
                if let Some(o) = detector.processar(&destino[..n], CANAIS) {
                    let r = if razao.is_finite() && razao > 0.0 { razao } else { 1.0 };
                    let em_midia_us = o / f64::from(TAXA) * 1e6;
                    let no_dac_qpc = base_qpc_us as f64 + no_dac as f64 + em_midia_us / r;
                    let no_carimbo = slot.carimbo_us as f64 + em_midia_us - atraso_interno_us;
                    if let Ok(mut v) = ctl.estouros.try_lock() {
                        if v.len() < v.capacity() {
                            v.push(Estouro {
                                no_dac_qpc_us: no_dac_qpc.max(0.0) as u64,
                                carimbo_us: no_carimbo as i64,
                            });
                        }
                    }
                }
            }
            slot
        };
        let mut montador = Montador::novo(fonte);
        let mut ganho_atual = controle.ganho();
        let mut tentativa = 0usize;
        while !controle.parar.load(Ordering::SeqCst) {
            match montar() {
                Ok((saida, descricao, mixador, com_rateadjust)) => {
                    tentativa = 0;
                    crate::registro::linha(format!("som: saída ligada — {descricao}"));
                    if let Ok(mut s) = situacao.lock() {
                        s.ligado = true;
                        s.saida = descricao;
                        s.taxa_do_mixador = mixador.taxa;
                        s.canais_do_mixador = mixador.canais;
                        s.com_rateadjust = com_rateadjust;
                        s.latencia_ms = saida.latencia_us / 1000.0;
                        s.buffer_quadros = saida.tamanho;
                        s.ultima_falha.clear();
                    }
                    let fim = tocar(&saida, &mut montador, &mut ganho_atual, &controle, &situacao, inicio);
                    drop(saida);
                    if let Ok(mut s) = situacao.lock() {
                        s.ligado = false;
                        s.retrato = montador.retrato;
                    }
                    match fim {
                        Fim::Parado => break,
                        Fim::Trocou | Fim::Invalidado => {
                            let motivo = if matches!(fim, Fim::Trocou) {
                                "a saída padrão mudou"
                            } else {
                                "a saída foi invalidada"
                            };
                            crate::registro::linha(format!("som: {motivo}; remontando"));
                            if let Ok(mut s) = situacao.lock() {
                                s.religamentos += 1;
                            }
                        }
                        Fim::Falhou(e) => {
                            crate::registro::linha(format!("som: !! a saída parou: {e}"));
                            if let Ok(mut s) = situacao.lock() {
                                s.religamentos += 1;
                                s.ultima_falha = e;
                            }
                            // Um erro que se repita logo depois de montar não gira a thread: espera
                            // meio segundo antes de remontar (crítica 13, miúdo 5).
                            let ate = Instant::now() + Duration::from_millis(500);
                            while Instant::now() < ate && !controle.parar.load(Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(20));
                            }
                        }
                    }
                }
                Err(e) => {
                    let espera = ESPERAS[tentativa.min(ESPERAS.len() - 1)];
                    if tentativa < ESPERAS.len() || tentativa % 15 == 0 {
                        crate::registro::linha(format!(
                            "som: !! a saída não ligou (tentativa {}): {e}; de novo em {espera} s",
                            tentativa + 1
                        ));
                    }
                    tentativa += 1;
                    if let Ok(mut s) = situacao.lock() {
                        s.ligado = false;
                        s.tentativas_falhas += 1;
                        s.ultima_falha = e;
                    }
                    let ate = Instant::now() + Duration::from_secs_f64(espera);
                    while Instant::now() < ate && !controle.parar.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
    }
    if let Ok(h) = mmcss {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(h);
        }
    }
    porta
}

/// O laço de uma saída montada: espera o evento, enche o que está livre.
fn tocar<F: crate::som_puxado::FonteDeSlots>(
    saida: &Saida,
    montador: &mut Montador<F>,
    ganho_atual: &mut f32,
    controle: &ControleDoSom,
    situacao: &Mutex<SituacaoDoSom>,
    inicio: Instant,
) -> Fim {
    let mut razao_aplicada = 1.0f64;
    let mut conferiu_padrao = Instant::now();
    // Os picos que não couberam numa publicação (o cadeado estava com o relato) vão na próxima.
    let (mut pico_guardado, mut pico_do_sinal_guardado) = (0.0f32, 0.0f32);
    loop {
        if controle.parar.load(Ordering::SeqCst) {
            return Fim::Parado;
        }
        // O prazo: um dispositivo que some sem invalidar não prende a thread (o `parar` e a troca
        // de padrão continuam sendo vistos). Sem evento, o pedido abaixo acha o buffer cheio.
        let _ = unsafe { WaitForSingleObject(saida.evento, 200) } == WAIT_OBJECT_0;
        let padding = match unsafe { saida.cliente.GetCurrentPadding() } {
            Ok(p) => p,
            Err(e) if e_invalidado(&e) => return Fim::Invalidado,
            Err(e) => return Fim::Falhou(format!("GetCurrentPadding: {e}")),
        };
        let livre = saida.tamanho.saturating_sub(padding);
        if livre == 0 {
            // Buffer cheio (ou o evento não veio): só a conferência do padrão.
            if padrao_mudou(&mut conferiu_padrao, saida) {
                return Fim::Trocou;
            }
            continue;
        }
        let ptr = match unsafe { saida.render.GetBuffer(livre) } {
            Ok(p) => p,
            Err(e) if e_invalidado(&e) => return Fim::Invalidado,
            Err(e) => return Fim::Falhou(format!("GetBuffer: {e}")),
        };
        let buffer = unsafe { std::slice::from_raw_parts_mut(ptr as *mut f32, livre as usize * CANAIS) };
        // O que o mixador já tem do fluxo sai antes deste pedido: a fila, lida à razão, mais a
        // latência que o fluxo declara.
        let atraso_us = f64::from(padding) / (f64::from(TAXA) * razao_aplicada) * 1e6 + saida.latencia_us;
        let agora_us = inicio.elapsed().as_micros() as u64;
        montador.render(buffer, atraso_us, razao_aplicada, agora_us);
        let p_sinal = pico(buffer);
        aplicar_ganho(buffer, ganho_atual, controle.ganho());
        let p = pico(buffer);
        if let Err(e) = unsafe { saida.render.ReleaseBuffer(livre, 0) } {
            return if e_invalidado(&e) { Fim::Invalidado } else { Fim::Falhou(format!("ReleaseBuffer: {e}")) };
        }
        // A razão sugerida vai para o `RATEADJUST`; a que vale para o núcleo é a que o mixador
        // aceitou. Ela chega por um atômico: o render não toma o cadeado do núcleo.
        let sugerida = controle.razao();
        if (sugerida - razao_aplicada).abs() > 1e-6 {
            if let Some(a) = saida.ajuste.as_ref() {
                if unsafe { a.SetSampleRate((f64::from(TAXA) * sugerida) as f32) }.is_ok() {
                    razao_aplicada = sugerida;
                }
            }
        }
        pico_guardado = pico_guardado.max(p);
        pico_do_sinal_guardado = pico_do_sinal_guardado.max(p_sinal);
        // `try_lock`: o relato de 1 Hz clona a situação com o cadeado na mão, e o render não espera
        // por ele (crítica 13, M5). Sem o cadeado, esta publicação fica para o próximo evento.
        if let Ok(mut s) = situacao.try_lock() {
            s.pico = s.pico.max(pico_guardado);
            s.pico_do_sinal = s.pico_do_sinal.max(pico_do_sinal_guardado);
            s.razao_aplicada = razao_aplicada;
            s.retrato = montador.retrato;
            pico_guardado = 0.0;
            pico_do_sinal_guardado = 0.0;
        }
        // A troca de padrão, com o pedido já atendido: a conferência não atrasa o mixador.
        if padrao_mudou(&mut conferiu_padrao, saida) {
            return Fim::Trocou;
        }
    }
}

/// A saída padrão mudou? Confere no máximo duas vezes por segundo.
fn padrao_mudou(conferiu: &mut Instant, saida: &Saida) -> bool {
    if conferiu.elapsed() < Duration::from_millis(500) {
        return false;
    }
    *conferiu = Instant::now();
    let agora = id_da_saida_padrao(&saida.enumerador);
    !agora.is_empty() && agora != saida.id
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::som_puxado::{amplitude_em, mulaw_de_i16, tom_de_prova};

    /// Um slot de quadro, como a porta o entregaria.
    fn quadro(payload: &[u8], i: u16) -> Puxado<'_> {
        Puxado::Slot(Entrega::Quadro { payload, sequencia: i, timestamp_us: u64::from(i) * 20_000 })
    }

    /// A amplitude de cada nota no canal esquerdo de um trecho de 48 kHz estéreo.
    fn notas(trecho: &[f32]) -> Vec<f32> {
        crate::som_puxado::NOTAS_DE_PROVA_HZ.iter().map(|n| amplitude_em(f64::from(*n), trecho, CANAIS, TAXA)).collect()
    }

    /// **Opus**: o tom de prova, codificado pelo `quall-opus` como o emissor do Windows codifica
    /// (48 kHz estéreo), volta pela decodificação do tocador com a nota certa e a amplitude da
    /// origem. Os dois canais iguais, e o carimbo do slot preservado.
    #[test]
    fn a_decodificacao_opus_devolve_o_tom() {
        let mut enc = quall_opus::Codificador::novo(TAXA, 2, quall_opus::Aplicacao::Audio).expect("codificador");
        enc.definir_taxa_de_bits(128_000).expect("taxa");
        let mut dec = Decodificacao::nova(CodecDeAudio::Opus, 2).expect("decodificação");
        let mut pacote = vec![0u8; 4_000];
        let mut saida = vec![0.0f32; QUADROS_POR_SLOT * CANAIS];
        let mut juntas = Vec::new();
        // 25 slots = 0,5 s = a primeira nota (400 Hz) inteira.
        for i in 0..25u16 {
            let pcm: Vec<i16> = (0..QUADROS_POR_SLOT as u64)
                .flat_map(|q| {
                    let v = (tom_de_prova(u64::from(i) * QUADROS_POR_SLOT as u64 + q, TAXA) * 32767.0) as i16;
                    [v, v]
                })
                .collect();
            let n = enc.codificar(&pcm, &mut pacote).expect("codificar");
            let slot = dec.escrever(quadro(&pacote[..n], i), &mut saida);
            assert_eq!(slot.ordem, Ordem::Quadro);
            assert_eq!(slot.carimbo_us, u64::from(i) * 20_000);
            assert_eq!(slot.quadros, QUADROS_POR_SLOT);
            juntas.extend_from_slice(&saida);
        }
        // Os últimos 10 slots: longe do atraso do codificador.
        let fim = &juntas[15 * QUADROS_POR_SLOT * CANAIS..];
        let a = notas(fim);
        assert!((a[0] - 0.5).abs() < 0.05, "400 Hz com amplitude 0,5: {a:?}");
        assert!(a[1..].iter().all(|x| *x < 0.02), "só a primeira nota: {a:?}");
        assert!(fim.chunks(2).all(|q| (q[0] - q[1]).abs() < 1e-3), "os dois canais iguais");
        assert_eq!(dec.falhas, 0);
    }

    /// **O carimbo do estouro no Opus desconta o lookahead do codificador** (rodada de 22/09): um
    /// estouro de 10 ms a 3 150 Hz entra no codificador no meio do quadro 20, sai decodificado 312
    /// amostras depois, e o carimbo que o render calcula (`carimbo + índice − atraso interno`) cai na
    /// captura verdadeira. Sem o desconto, cai 6,5 ms depois — o +6,6 do T0 do Mac.
    #[test]
    fn o_carimbo_do_estouro_no_opus_cai_na_captura() {
        let mut enc = quall_opus::Codificador::novo(TAXA, 2, quall_opus::Aplicacao::Audio).expect("codificador");
        enc.definir_taxa_de_bits(128_000).expect("taxa");
        let mut dec = Decodificacao::nova(CodecDeAudio::Opus, 2).expect("decodificação");
        let mut detector = DetectorDeEstouro::novo(f64::from(TAXA), QUADROS_POR_SLOT);
        let inicio = 20 * QUADROS_POR_SLOT + 137;
        let verdade_us = inicio as f64 / f64::from(TAXA) * 1e6;
        let mut pacote = vec![0u8; 4_000];
        let mut saida = vec![0.0f32; QUADROS_POR_SLOT * CANAIS];
        let mut achado = None;
        for i in 0..40u16 {
            let pcm: Vec<i16> = (0..QUADROS_POR_SLOT)
                .flat_map(|q| {
                    let n = usize::from(i) * QUADROS_POR_SLOT + q;
                    let v = if n >= inicio && n < inicio + 480 {
                        ((2.0 * std::f64::consts::PI * 3_150.0 * n as f64 / f64::from(TAXA)).sin() * 0.5 * 32_767.0) as i16
                    } else {
                        0
                    };
                    [v, v]
                })
                .collect();
            let n = enc.codificar(&pcm, &mut pacote).expect("codificar");
            let slot = dec.escrever(quadro(&pacote[..n], i), &mut saida);
            if let Some(o) = detector.processar(&saida[..slot.quadros * CANAIS], CANAIS) {
                let em_midia_us = o / f64::from(TAXA) * 1e6;
                achado = Some(slot.carimbo_us as f64 + em_midia_us - dec.atraso_interno_us());
                break;
            }
        }
        let carimbo = achado.expect("o estouro tinha de ser achado");
        assert_eq!(dec.atraso_interno_us(), 6_500.0);
        assert!(
            (carimbo - verdade_us).abs() < 500.0,
            "o carimbo do estouro caiu {:.0} µs da captura (sem o desconto cairia +6 500)",
            carimbo - verdade_us
        );
    }

    /// **PCMU**: o tom a 8 kHz em µ-law volta a 48 kHz, mono duplicado nos dois canais, com a nota e
    /// a amplitude da origem. O silêncio desce em rampa (5 ms) da última amostra até zero.
    #[test]
    fn a_decodificacao_pcmu_devolve_o_tom_e_o_silencio_desce_em_rampa() {
        let mut dec = Decodificacao::nova(CodecDeAudio::Pcmu, 1).expect("decodificação");
        let mut saida = vec![0.0f32; QUADROS_POR_SLOT * CANAIS];
        let mut juntas = Vec::new();
        for i in 0..25u16 {
            let payload: Vec<u8> = (0..160u64)
                .map(|q| mulaw_de_i16((tom_de_prova(u64::from(i) * 160 + q, 8_000) * 32767.0) as i16))
                .collect();
            let slot = dec.escrever(quadro(&payload, i), &mut saida);
            assert_eq!(slot.ordem, Ordem::Quadro);
            juntas.extend_from_slice(&saida);
        }
        let fim = &juntas[5 * QUADROS_POR_SLOT * CANAIS..];
        let a = notas(fim);
        assert!((a[0] - 0.5).abs() < 0.03, "400 Hz com amplitude 0,5: {a:?}");
        assert!(a[1..].iter().all(|x| *x < 0.02), "só a primeira nota: {a:?}");
        assert!(fim.chunks(2).all(|q| q[0] == q[1]), "mono duplicado");

        let silencio = Puxado::Slot(Entrega::Silencio { sequencia: 25, timestamp_us: 500_000 });
        let slot = dec.escrever(silencio, &mut saida);
        assert_eq!(slot.ordem, Ordem::Silencio);
        // Depois da rampa e do atraso do filtro (≈ 1,5 ms), zero.
        assert!(saida[(10 * 48) * CANAIS..].iter().all(|v| v.abs() < 1e-3), "silêncio é zero depois da rampa");
        let maior_passo = saida.chunks(2).map(|q| q[0]).collect::<Vec<_>>().windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(maior_passo < 0.05, "a rampa não estala: passo de {maior_passo}");
    }

    /// O ocioso não escreve nada e devolve o slot ocioso: o montador põe zeros.
    #[test]
    fn o_ocioso_nao_decodifica() {
        let mut dec = Decodificacao::nova(CodecDeAudio::Opus, 2).expect("decodificação");
        let mut saida = vec![7.0f32; QUADROS_POR_SLOT * CANAIS];
        assert_eq!(dec.escrever(Puxado::Ocioso, &mut saida), SlotPuxado::ocioso());
        assert!(saida.iter().all(|v| *v == 7.0));
    }
}
