//! O som deste computador, por **WASAPI loopback**, codificado em Opus e entregue quadro a quadro.
//!
//! # O que existia antes desta rodada: nada
//!
//! `docs/audio.md` §"o que ninguém captura" é literal: *"Nenhuma casca captura áudio. Continua
//! valendo: nem WASAPI loopback, nem ScreenCaptureKit, nem AudioPlaybackCapture, nem microfone em
//! lugar nenhum."* O caminho de áudio do núcleo — `TrackKind::SystemAudio`,
//! `PRESET_AUDIO_DO_SISTEMA`, `TrackEmissor::enviar_audio` — está pronto e provado **entre duas
//! sondas**, com tom sintético dos dois lados. Este módulo é a primeira origem de áudio real do
//! projeto, em qualquer plataforma.
//!
//! # Três conversões que o núcleo não faz, e por isso são daqui
//!
//! O núcleo transporta bytes já codificados e o `quall-opus` é um binding fino da libopus:
//! **nenhum dos dois reamostra, nem mistura canal, nem converte formato de amostra.** O mixador do
//! WASAPI entrega o que o driver quiser — neste Dell, quase certamente float de 32 bits — e o
//! contrato da track de sistema exige `i16` intercalado, **48 000 Hz**, **2 canais**, quadros de
//! **20 ms** (960 amostras por canal, 1920 `i16`). As três pontes moram aqui:
//!
//! 1. [`amostras_para_f32`] — float32/int16/int32 do mixador para `f32` normalizado;
//! 2. [`mapear_canais`] — mono vira estéreo por duplicação; mais de dois canais viram os **dois
//!    primeiros** (frente esquerda/direita, pela ordem que a máscara de canais do Windows fixa).
//!    Não é uma mistura descendente de verdade, e está dito assim no relatório;
//! 3. o reamostrador da linha do tempo ([`crate::reamostrador_sinc::ReamostradorSinc`], sinc com
//!    janela de Kaiser em tabela polifásica), que também é o atuador da disciplina da deriva
//!    (`som-no-receptor.md` §19.6). Antes era uma interpolação linear, **e só quando precisava**: a 48 000 Hz o caminho era
//!    cópia direta, sem tocar em interpolação nenhuma.
//!
//! `PresetDeAudio::canais` vira `stereo=1` no SDP. A regra do M4 — *"um emissor tem de declarar no
//! bitstream o que ele de fato faz"* — obriga: se a track diz estéreo, o que sai tem de ser
//! estéreo, mesmo que a origem seja mono.
//!
//! # Uma thread própria, e o handle do núcleo continua sem sair da thread da sessão
//!
//! `emissor.rs` documenta a regra de plataforma: o `Ready`, a `Session` e as tracks vivem numa
//! variável só, na thread da sessão. Áudio não pode rodar nessa thread — o WASAPI entrega blocos a
//! cada ~10 ms e o laço de vídeo bloqueia até 25 ms esperando saída do MFT; um áudio pendurado
//! nesse ritmo engasgaria.
//!
//! A saída **não** é dar uma referência da track para a thread de áudio. É a mesma que o vídeo já
//! usa para a captura: esta thread produz **pacotes de Opus prontos** num canal, e a thread da
//! sessão os retira e chama `enviar_audio`. Nenhum handle do núcleo atravessa a fronteira — o que
//! atravessa é `Vec<u8>` e um `u64`.
//!
//! # Como isto é provado sem gravar o que a máquina está tocando
//!
//! `docs/regras-de-frente.md` diz que um vídeo de bancada pode conter a vida do usuário, e
//! `docs/audio.md` §8 diz a mesma coisa do som, com mais força: *"um `.wav` não carrega no nome o
//! que tem dentro"*. O loopback captura **a mistura da máquina inteira** — se houver um vídeo
//! aberto, ele está no fluxo.
//!
//! Então a prova aqui **não é um arquivo**: é
//!
//! - uma **origem sintética própria** ([`TomDeProva`]) — um seno que este processo gera e toca no
//!   mesmo endpoint que o loopback está capturando. É bancada, fica atrás de `--tom-de-prova`, e
//!   **nunca liga sozinho**;
//! - um **contador**, não um artefato: [`razao_do_tom`] é um Goertzel na frequência do tom, que
//!   responde "que fração da energia deste quadro está exatamente nessa raia?" com um número entre
//!   0 e 1. Ele lê as amostras e **não guarda nenhuma**;
//! - a **conferência do fio** pelo byte de TOC, com dois leitores independentes (o nosso e o da
//!   libopus), que é o análogo em áudio do que `sps.rs` faz no vídeo: responde "o que este emissor
//!   declarou no `fmtp` é o que ele está de fato pondo no fio?" sem decodificar nada.
//!
//! Não existe caminho para arquivo neste módulo, pelo mesmo motivo que não existe em
//! `transmissao.rs`: a forma mais barata de nunca vazar é não ter para onde gravar.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};

use windows::core::{Result, GUID};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::linha_do_loopback::{hora_do_pacote, na_origem, LinhaDoLoopback, Pacote, CANAIS, TAXA_DE_SAIDA};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};

use quall_core::track::PresetDeAudio;
use quall_opus::{Aplicacao, Codificador, Sinal, Toc};

use crate::registro;

/// `WAVE_FORMAT_EXTENSIBLE` do `mmreg.h`. Escrito à mão pelo mesmo motivo que
/// `fontes.rs` escreve `MONITORINFOF_PRIMARY`: o crate `windows` expõe a struct e não a constante.
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

/// `KSDATAFORMAT_SUBTYPE_PCM` — `00000001-0000-0010-8000-00aa00389b71`.
const SUBTIPO_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT` — `00000003-0000-0010-8000-00aa00389b71`.
const SUBTIPO_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// Fundo de escala de `i16`, como `f32`. 32768 e não 32767: é o que faz `-1.0` virar exatamente
/// `-32768` sem estourar, e o erro de meio bit no positivo é inaudível e simétrico com o resto do
/// mundo do áudio.
const FUNDO_DE_ESCALA: f32 = 32768.0;

/// Quantos pacotes de 20 ms cabem na fila entre a thread de áudio e a da sessão.
///
/// **50 = 1 segundo.** Não é "zero filas" como no vídeo, e a diferença é do meio: um quadro de
/// vídeo velho é lixo (o próximo mostra a mesma tela mais nova), um quadro de áudio velho é som
/// que ninguém vai ouvir se for descartado. O laço da sessão drena a fila inteira a cada volta, e
/// uma volta dele custa dezenas de milissegundos no pior caso — a fila existe para absorver isso,
/// não para acumular. Se ela encher, o **mais velho** sai, e o contador diz quantas vezes.
const TAMANHO_DA_FILA: usize = 50;

/// Acima desta fração da energia do quadro na raia do tom, e acima de [`RMS_MINIMO`], o quadro
/// conta como "o tom estava lá".
///
/// 0,5 é folgado de propósito: um seno puro dá ~1,0, e silêncio digital dá ruído numérico. O que
/// separa os dois casos por uma ordem de grandeza não precisa de um limiar apertado.
const LIMIAR_DO_TOM: f64 = 0.5;
/// Piso de energia para o quadro ser considerado som, em unidades de fundo de escala de `i16`.
/// ~ -60 dBFS.
const RMS_MINIMO: f64 = 32.0;

// -------------------------------------------------------------------------------------------
// Contadores
// -------------------------------------------------------------------------------------------

/// Os dois lados da fronteira, como o vídeo já faz: o que o WASAPI entregou e o que virou pacote.
///
/// Tudo atômico porque a thread da sessão lê enquanto a thread de áudio escreve. Nada aqui guarda
/// amostra nenhuma — são contagens e somas.
#[derive(Default)]
pub struct ContadoresDeAudio {
    /// Blocos que o `IAudioCaptureClient` entregou.
    pub blocos: AtomicU64,
    /// Quadros PCM (amostras por canal) que o WASAPI entregou, antes de qualquer conversão.
    pub quadros_pcm: AtomicU64,
    /// Blocos que vieram com `AUDCLNT_BUFFERFLAGS_SILENT` — o mixador dizendo "isto é silêncio,
    /// não me dei ao trabalho de zerar o buffer".
    pub blocos_silenciosos: AtomicU64,
    /// Blocos com `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`: houve um buraco antes deste.
    pub descontinuidades: AtomicU64,
    /// Amostras por canal que **nós** inventamos como silêncio para tapar buraco de relógio.
    /// Ver [`LinhaDoLoopback`] (`linha_do_loopback.rs`). Se este número for grande, o loopback
    /// desta máquina não entrega nada quando ninguém está tocando som.
    pub amostras_de_preenchimento: AtomicU64,
    /// Buracos que a **posição do dispositivo** mostrou com som contínuo (som que o dispositivo
    /// perdeu, que vira zeros na entrada; §19.6.4, N3). A parada do mixador não conta aqui: ela
    /// entra em `amostras_de_preenchimento`.
    pub buracos: AtomicU64,
    /// Amostras de entrada cortadas: o excesso depois de uma parada, a posição que voltou até
    /// 1 s, e o socorro.
    pub amostras_cortadas: AtomicU64,
    /// Pacotes sem hora (`AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`, ou hora zero).
    pub pacotes_sem_hora: AtomicU64,
    /// O erro ε do último pacote: a hora dele menos o carimbo que a linha de saída dá à primeira
    /// amostra dele, em µs com sinal (guardado como `i64` num `AtomicU64`). É o que a disciplina da
    /// deriva leva a zero (§19.6.4); em regime, o ruído da hora.
    pub desvio_us: AtomicU64,
    pub maior_desvio_us: AtomicU64,
    /// A frequência estimada da disciplina da deriva, em centésimos de ppm, com sinal (guardada
    /// como `i64` num `AtomicU64`): o dispositivo contra o QPC (§19.6, L5).
    pub f_centesimos_de_ppm: AtomicU64,
    /// Degraus do carimbo (o socorro, §19.6.4). Zero em regime.
    pub degraus: AtomicU64,
    /// Quadros de 20 ms montados e oferecidos ao codificador.
    pub quadros_de_20ms: AtomicU64,
    pub pacotes_opus: AtomicU64,
    pub bytes_opus: AtomicU64,
    pub falhas_de_encode: AtomicU64,
    /// Quantas vezes a fila encheu e o pacote mais velho foi descartado.
    pub descartados_por_fila: AtomicU64,

    // --- a medida do tom: número, nunca gravação ---
    pub quadros_medidos: AtomicU64,
    pub quadros_com_tom: AtomicU64,
    /// Soma das razões de Goertzel × 1000, para tirar média sem ponto flutuante atômico.
    soma_razao_milesimos: AtomicU64,
    /// Soma dos RMS (fundo de escala 32768).
    soma_rms: AtomicU64,
    /// Maior RMS visto. Um `0` aqui com o tom ligado é a máquina muda.
    pub rms_maximo: AtomicU64,

    // --- a conferência do fio, pelo byte de TOC ---
    pub toc_conferidos: AtomicU64,
    pub toc_divergentes: AtomicU64,
}

impl ContadoresDeAudio {
    pub fn razao_media_do_tom(&self) -> f64 {
        let n = self.quadros_medidos.load(Ordering::Relaxed);
        if n == 0 {
            0.0
        } else {
            self.soma_razao_milesimos.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0
        }
    }

    pub fn rms_medio(&self) -> f64 {
        let n = self.quadros_medidos.load(Ordering::Relaxed);
        if n == 0 {
            0.0
        } else {
            self.soma_rms.load(Ordering::Relaxed) as f64 / n as f64
        }
    }

    /// A linha de registro que sai a cada segundo e no fim.
    pub fn linha(&self) -> String {
        format!(
            "blocos={} quadros_pcm={} silenciosos={} descontinuidades={} preenchimento={} \
             buracos={} cortadas={} sem_hora={} desvio_ms={:.3} maior_desvio_ms={:.3} f_ppm={:.2} degraus={} \
             quadros_20ms={} pacotes_opus={} bytes_opus={} falhas_encode={} descartados_fila={} \
             tom={}/{} razao_media={:.3} rms_medio={:.0} rms_max={} toc_ok={} toc_divergentes={}",
            self.blocos.load(Ordering::Relaxed),
            self.quadros_pcm.load(Ordering::Relaxed),
            self.blocos_silenciosos.load(Ordering::Relaxed),
            self.descontinuidades.load(Ordering::Relaxed),
            self.amostras_de_preenchimento.load(Ordering::Relaxed),
            self.buracos.load(Ordering::Relaxed),
            self.amostras_cortadas.load(Ordering::Relaxed),
            self.pacotes_sem_hora.load(Ordering::Relaxed),
            self.desvio_us.load(Ordering::Relaxed) as i64 as f64 / 1000.0,
            self.maior_desvio_us.load(Ordering::Relaxed) as i64 as f64 / 1000.0,
            self.f_centesimos_de_ppm.load(Ordering::Relaxed) as i64 as f64 / 100.0,
            self.degraus.load(Ordering::Relaxed),
            self.quadros_de_20ms.load(Ordering::Relaxed),
            self.pacotes_opus.load(Ordering::Relaxed),
            self.bytes_opus.load(Ordering::Relaxed),
            self.falhas_de_encode.load(Ordering::Relaxed),
            self.descartados_por_fila.load(Ordering::Relaxed),
            self.quadros_com_tom.load(Ordering::Relaxed),
            self.quadros_medidos.load(Ordering::Relaxed),
            self.razao_media_do_tom(),
            self.rms_medio(),
            self.rms_maximo.load(Ordering::Relaxed),
            self.toc_conferidos.load(Ordering::Relaxed),
            self.toc_divergentes.load(Ordering::Relaxed),
        )
    }
}

// -------------------------------------------------------------------------------------------
// O que sai daqui
// -------------------------------------------------------------------------------------------

/// Um quadro de Opus pronto para `TrackEmissor::enviar_audio`.
pub struct PacoteDeAudio {
    pub bytes: Vec<u8>,
    /// Microssegundos no **mesmo relógio monotônico do vídeo** — é o campo que permite alinhar as
    /// duas tracks da mesma sessão, e por isso as duas cadeias recebem a mesma origem.
    ///
    /// Derivado da **contagem de amostras**, não de `Instant::now()` por quadro: o RTP quer uma
    /// linha do tempo de espaçamento uniforme, e o relógio de parede num laço de captura oscila.
    pub timestamp_us: u64,
}

/// Qual saída de áudio capturar.
#[derive(Clone, Debug)]
pub enum Alvo {
    /// A saída padrão do sistema — o caminho de produto. É o que a pessoa está ouvindo.
    Padrao,
    /// **Bancada.** O n-ésimo endpoint de renderização ativo, na ordem de `EnumAudioEndpoints`.
    ///
    /// Existe por um motivo de privacidade, não de conveniência: o loopback da saída padrão
    /// captura a mistura da máquina inteira. Capturar um endpoint que **não** é o padrão — a saída
    /// de áudio de um HDMI, por exemplo — captura só o que este processo mandar para lá, porque
    /// nenhum outro app toca num endpoint que não é o padrão. É a única forma de a origem ser
    /// **provadamente** nossa.
    Indice(u32),
}

impl Alvo {
    pub fn analisar(texto: &str) -> Alvo {
        match texto.trim().parse::<u32>() {
            Ok(i) => Alvo::Indice(i),
            Err(_) => Alvo::Padrao,
        }
    }
}

/// Como montar a cadeia de áudio.
#[derive(Clone, Debug)]
pub struct ConfigDeAudio {
    pub alvo: Alvo,
    /// **Bancada.** Frequência do tom sintético que este processo toca no endpoint capturado.
    /// `None` = não toca nada, que é o padrão de produto.
    pub tom_hz: Option<u32>,
    /// Amplitude do tom, de 0 a 1. 0,03 é ~-30 dBFS: o Goertzel acha um seno puro dessa amplitude
    /// sem esforço nenhum, e quem está na sala com a máquina mal ouve.
    pub tom_amplitude: f32,
}

impl Default for ConfigDeAudio {
    fn default() -> Self {
        ConfigDeAudio { alvo: Alvo::Padrao, tom_hz: None, tom_amplitude: 0.03 }
    }
}

/// A cadeia de áudio montada e correndo. Espelha [`crate::transmissao::Cadeia`] de propósito: a
/// thread da sessão trata as duas do mesmo jeito.
pub struct CadeiaDeAudio {
    pacotes: Receiver<PacoteDeAudio>,
    parar: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    tom: Option<TomDeProva>,
    pub contadores: Arc<ContadoresDeAudio>,
    /// O que o endpoint de fato entregou, para o relatório dizer contra o que os números foram
    /// medidos. Preenchido pela thread assim que ela abre o cliente.
    pub descricao: Arc<std::sync::Mutex<String>>,
}

impl CadeiaDeAudio {
    /// Sobe a thread de captura. Devolve erro **antes** de gastar qualquer coisa se o endpoint não
    /// abrir — quem chama decide se transmite sem som ou desiste.
    ///
    /// `origem` é o mesmo `Instant` que a cadeia de vídeo usa: é o que faz os dois `timestamp_us`
    /// falarem do mesmo relógio.
    pub fn abrir(
        cfg: ConfigDeAudio,
        preset: PresetDeAudio,
        origem: Instant,
    ) -> std::result::Result<CadeiaDeAudio, String> {
        // Abrir o endpoint aqui, na thread de quem chamou, para poder **falhar antes de prometer**.
        // Se isto for para dentro da thread, a falha vira uma linha de registro que ninguém vê e o
        // app diz que está transmitindo som quando não está.
        let sonda = ProvaDeEndpoint::abrir(&cfg.alvo)?;
        let descricao_inicial = sonda.descricao.clone();
        drop(sonda);

        let contadores = Arc::new(ContadoresDeAudio::default());
        let descricao = Arc::new(std::sync::Mutex::new(descricao_inicial));
        let parar = Arc::new(AtomicBool::new(false));
        let (tx, rx) = bounded::<PacoteDeAudio>(TAMANHO_DA_FILA);

        let tom = match cfg.tom_hz {
            Some(hz) => match TomDeProva::iniciar(&cfg.alvo, hz, cfg.tom_amplitude) {
                Ok(t) => {
                    registro::linha(format!(
                        "audio: tom de prova ligado — {hz} Hz, amplitude {:.3} (origem sintética \
                         deste processo; bancada)",
                        cfg.tom_amplitude
                    ));
                    Some(t)
                }
                Err(e) => {
                    registro::linha(format!("audio: tom de prova NÃO subiu: {e}"));
                    None
                }
            },
            None => None,
        };

        let c = Arc::clone(&contadores);
        let d = Arc::clone(&descricao);
        let p = Arc::clone(&parar);
        let alvo = cfg.alvo.clone();
        let hz = cfg.tom_hz;
        let thread = std::thread::Builder::new()
            .name("quall.audio".into())
            .spawn(move || {
                if let Err(e) = correr_captura(alvo, preset, origem, hz, tx, c, d, p) {
                    registro::linha(format!("audio: a captura parou com erro: {e}"));
                }
            })
            .map_err(|e| format!("não consegui criar a thread de áudio: {e}"))?;

        Ok(CadeiaDeAudio {
            pacotes: rx,
            parar,
            thread: Some(thread),
            tom,
            contadores,
            descricao,
        })
    }

    /// **Uma cadeia que só drena uma fila** (o ramal da rede do microfone do R5, `microfone.rs`): sem
    /// thread e sem endpoint. Fechar não fecha o microfone — ele é de quem o abriu, e outra sessão
    /// pode ganhar outro ramal.
    pub fn de_ramal(pacotes: Receiver<PacoteDeAudio>, contadores: Arc<ContadoresDeAudio>, descricao: String) -> CadeiaDeAudio {
        CadeiaDeAudio {
            pacotes,
            parar: Arc::new(AtomicBool::new(false)),
            thread: None,
            tom: None,
            contadores,
            descricao: Arc::new(std::sync::Mutex::new(descricao)),
        }
    }

    /// Tudo o que está pronto, sem esperar por nada. A thread da sessão chama isto uma vez por
    /// volta, logo depois de bombear o vídeo.
    pub fn bombear(&self) -> Vec<PacoteDeAudio> {
        let mut saida = Vec::new();
        while let Ok(p) = self.pacotes.try_recv() {
            saida.push(p);
        }
        saida
    }

    pub fn descricao(&self) -> String {
        self.descricao.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn fechar(&mut self) {
        self.parar.store(true, Ordering::SeqCst);
        if let Some(t) = self.tom.take() {
            t.parar();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CadeiaDeAudio {
    fn drop(&mut self) {
        self.fechar();
    }
}

// -------------------------------------------------------------------------------------------
// Abrir o endpoint
// -------------------------------------------------------------------------------------------

/// Abre o endpoint uma vez só para saber se ele existe e o que ele entrega, e joga fora.
///
/// Existe para separar "não dá para capturar áudio nesta máquina" de "a captura morreu no meio",
/// que são duas mensagens diferentes para a pessoa.
struct ProvaDeEndpoint {
    descricao: String,
}

/// Dá para capturar som deste endpoint? Responde **antes** de a sessão existir, e é por isso que
/// ela existe separada da cadeia.
///
/// A ordem importa e não é detalhe: `tracks` só vale em `hospedar` e **não há renegociação**
/// (dívida 1), então a track de áudio ou entra na oferta ou não existe naquela sessão. Declarar
/// uma track de áudio e só depois descobrir que o WASAPI não abre deixaria o receptor com uma
/// track que nunca recebe um pacote — e do lado dele isso é indistinguível de silêncio.
///
/// Esta prova abre o endpoint e lê o formato do mixador. Ela **não** chama `Initialize` nem
/// `Start`: nada é capturado aqui, e por isso ela pode rodar durante a espera sem compor uma única
/// amostra do som da pessoa.
pub fn conferir(alvo: &Alvo) -> std::result::Result<String, String> {
    ProvaDeEndpoint::abrir(alvo).map(|p| p.descricao)
}

impl ProvaDeEndpoint {
    fn abrir(alvo: &Alvo) -> std::result::Result<ProvaDeEndpoint, String> {
        // Esta função pode ser chamada da thread da sessão, que já está em MTA. `CoInitializeEx`
        // repetido no mesmo apartamento é contagem de referência, não erro — mas o `S_FALSE` que
        // ele devolve **não** é falha, e por isso o retorno é ignorado de propósito.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let r = (|| -> Result<String> {
            let dispositivo = abrir_dispositivo(alvo)?;
            let id = ler_id(&dispositivo);
            let cliente: IAudioClient =
                unsafe { dispositivo.Activate(CLSCTX_ALL, None) }?;
            let formato = FormatoDoMixador::ler(&cliente)?;
            Ok(format!("id={id} {formato}"))
        })();
        match r {
            Ok(descricao) => Ok(ProvaDeEndpoint { descricao }),
            Err(e) => Err(format!("{e}")),
        }
    }
}

fn enumerador() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn abrir_dispositivo(alvo: &Alvo) -> Result<IMMDevice> {
    let e = enumerador()?;
    match alvo {
        Alvo::Padrao => unsafe { e.GetDefaultAudioEndpoint(eRender, eConsole) },
        Alvo::Indice(i) => unsafe {
            let colecao = e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            colecao.Item(*i)
        },
    }
}

/// Os endpoints de renderização ativos, como `id` — para uma corrida de bancada saber o que
/// escolher com `Alvo::Indice`.
///
/// Sem nome amigável de propósito: o nome vive no repositório de propriedades do shell, e lê-lo
/// custaria duas features novas no crate `windows` para uma linha de registro. O `id` identifica o
/// endpoint sem ambiguidade, e o nome bonito sai de fora, por PowerShell, quando alguém precisar.
pub fn listar_saidas() -> Vec<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Ok(e) = enumerador() else { return Vec::new() };
    let Ok(colecao) = (unsafe { e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }) else {
        return Vec::new();
    };
    let n = unsafe { colecao.GetCount() }.unwrap_or(0);
    let padrao = unsafe { e.GetDefaultAudioEndpoint(eRender, eConsole) }
        .ok()
        .map(|d| ler_id(&d))
        .unwrap_or_default();
    (0..n)
        .filter_map(|i| unsafe { colecao.Item(i) }.ok())
        .enumerate()
        .map(|(i, d)| {
            let id = ler_id(&d);
            let marca = if id == padrao { " (padrão)" } else { "" };
            format!("[{i}] {id}{marca}")
        })
        .collect()
}

fn ler_id(dispositivo: &IMMDevice) -> String {
    unsafe {
        match dispositivo.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

// -------------------------------------------------------------------------------------------
// O formato que o mixador entrega
// -------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Amostra {
    Float32,
    Int16,
    Int32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FormatoDoMixador {
    pub(crate) taxa_hz: u32,
    pub(crate) canais: u16,
    pub(crate) bytes_por_quadro: u16,
    pub(crate) amostra: Amostra,
}

impl FormatoDoMixador {
    /// Lê `GetMixFormat` e o libera. **O ponteiro é do chamador**: sem o `CoTaskMemFree` isto
    /// vazaria algumas dezenas de bytes por abertura de sessão.
    pub(crate) fn ler(cliente: &IAudioClient) -> Result<FormatoDoMixador> {
        unsafe {
            let p = cliente.GetMixFormat()?;
            let r = Self::de_ponteiro(p);
            CoTaskMemFree(Some(p as *const _));
            r
        }
    }

    /// `WAVEFORMATEX` é `#[repr(C, packed(1))]`: ler campo por referência é comportamento
    /// indefinido. Copiar a struct inteira com `read_unaligned` e ler os campos da cópia é o
    /// caminho certo, e é o que esta função faz.
    unsafe fn de_ponteiro(p: *const WAVEFORMATEX) -> Result<FormatoDoMixador> {
        let base: WAVEFORMATEX = unsafe { std::ptr::read_unaligned(p) };
        let tag = base.wFormatTag;
        let bits = base.wBitsPerSample;

        let amostra = if tag == WAVE_FORMAT_EXTENSIBLE {
            let ext: WAVEFORMATEXTENSIBLE =
                unsafe { std::ptr::read_unaligned(p as *const WAVEFORMATEXTENSIBLE) };
            let sub = ext.SubFormat;
            if sub == SUBTIPO_FLOAT && bits == 32 {
                Amostra::Float32
            } else if sub == SUBTIPO_PCM && bits == 16 {
                Amostra::Int16
            } else if sub == SUBTIPO_PCM && bits == 32 {
                Amostra::Int32
            } else {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_INVALIDARG,
                    format!("formato do mixador não suportado: extensível, {bits} bits, subtipo {sub:?}")
                        .as_str(),
                ));
            }
        } else if tag == WAVE_FORMAT_IEEE_FLOAT && bits == 32 {
            Amostra::Float32
        } else if tag == WAVE_FORMAT_PCM && bits == 16 {
            Amostra::Int16
        } else if tag == WAVE_FORMAT_PCM && bits == 32 {
            Amostra::Int32
        } else {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_INVALIDARG,
                format!("formato do mixador não suportado: tag {tag}, {bits} bits").as_str(),
            ));
        };

        Ok(FormatoDoMixador {
            taxa_hz: base.nSamplesPerSec,
            canais: base.nChannels,
            bytes_por_quadro: base.nBlockAlign,
            amostra,
        })
    }
}

impl std::fmt::Display for FormatoDoMixador {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mixador={} Hz {} canal(is) {:?}",
            self.taxa_hz, self.canais, self.amostra
        )
    }
}

// -------------------------------------------------------------------------------------------
// Conversões
// -------------------------------------------------------------------------------------------

/// Bytes crus do WASAPI para `f32` normalizado em [-1, 1], intercalado, sem mexer em canal.
pub(crate) fn amostras_para_f32(bruto: &[u8], formato: FormatoDoMixador, saida: &mut Vec<f32>) {
    match formato.amostra {
        Amostra::Float32 => {
            for pedaco in bruto.chunks_exact(4) {
                saida.push(f32::from_le_bytes([pedaco[0], pedaco[1], pedaco[2], pedaco[3]]));
            }
        }
        Amostra::Int16 => {
            for pedaco in bruto.chunks_exact(2) {
                saida.push(i16::from_le_bytes([pedaco[0], pedaco[1]]) as f32 / FUNDO_DE_ESCALA);
            }
        }
        Amostra::Int32 => {
            for pedaco in bruto.chunks_exact(4) {
                let v = i32::from_le_bytes([pedaco[0], pedaco[1], pedaco[2], pedaco[3]]);
                saida.push(v as f32 / 2_147_483_648.0);
            }
        }
    }
}

/// De `origem` canais para `destino` canais, intercalado.
///
/// - **1 → 2**: duplica. É o que faz um SDP que diz `stereo=1` não mentir quando a origem é mono.
/// - **N → 2** (N > 2): fica com os **dois primeiros** canais. A máscara de canais do Windows põe
///   frente-esquerda e frente-direita nas duas primeiras posições, então isto é "as caixas da
///   frente", **não** uma mistura descendente de 5.1 — o centro e os surrounds somem. Está dito no
///   relatório; uma mistura de verdade é trabalho de outra rodada e ninguém mediu que precise dela.
/// - **N → 1**: média dos canais.
fn mapear_canais(entrada: &[f32], origem: usize, destino: usize, saida: &mut Vec<f32>) {
    if origem == destino {
        saida.extend_from_slice(entrada);
        return;
    }
    let quadros = entrada.len() / origem.max(1);
    for q in 0..quadros {
        let base = q * origem;
        if destino == 1 {
            let soma: f32 = (0..origem).map(|c| entrada[base + c]).sum();
            saida.push(soma / origem as f32);
        } else if origem == 1 {
            let v = entrada[base];
            for _ in 0..destino {
                saida.push(v);
            }
        } else {
            for c in 0..destino {
                saida.push(entrada[base + c.min(origem - 1)]);
            }
        }
    }
}


pub(crate) fn para_i16(v: f32) -> i16 {
    let escalado = v * FUNDO_DE_ESCALA;
    escalado.clamp(-32768.0, 32767.0) as i16
}

// -------------------------------------------------------------------------------------------
// A medida do tom: Goertzel, e nenhuma amostra guardada
// -------------------------------------------------------------------------------------------

/// Que fração da energia deste quadro está exatamente na raia de `hz`, e qual o RMS dele.
///
/// Goertzel é o filtro certo aqui porque a pergunta é sobre **uma** frequência conhecida: uma FFT
/// inteira responderia a mesma coisa gastando `N log N` para jogar fora tudo menos uma raia.
///
/// A 48 000 Hz com 960 amostras a raia mede 50 Hz, então **1 000 Hz cai exatamente numa raia** e
/// não há vazamento espectral para descontar. A razão devolvida vale ~1,0 para um seno puro
/// naquela frequência e ~0 para qualquer outra coisa.
///
/// **Esta função lê as amostras e não guarda nenhuma.** É o que a torna um contador e não uma
/// gravação — a mesma distinção que `sps.rs` faz no vídeo.
fn razao_do_tom(quadro: &[i16], canais: usize, hz: f64, taxa_hz: f64) -> (f64, f64) {
    let n = quadro.len() / canais.max(1);
    if n == 0 {
        return (0.0, 0.0);
    }
    let k = (n as f64 * hz / taxa_hz).round();
    let w = 2.0 * std::f64::consts::PI * k / n as f64;
    let coef = 2.0 * w.cos();

    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    let mut energia = 0.0f64;
    for q in 0..n {
        let base = q * canais;
        let mut soma = 0.0f64;
        for c in 0..canais {
            soma += quadro[base + c] as f64;
        }
        let x = soma / canais as f64 / FUNDO_DE_ESCALA as f64;
        energia += x * x;
        let s0 = x + coef * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let potencia = s1 * s1 + s2 * s2 - coef * s1 * s2;
    let rms = (energia / n as f64).sqrt() * FUNDO_DE_ESCALA as f64;
    // Para um seno de amplitude A: energia = N·A²/2 e potência = (N·A/2)². A divisão abaixo dá 1.
    let referencia = energia * n as f64 / 2.0;
    let razao = if referencia > 1e-12 { potencia / referencia } else { 0.0 };
    (razao.clamp(0.0, 1.0), rms)
}

// -------------------------------------------------------------------------------------------
// O laço de captura
// -------------------------------------------------------------------------------------------

/// Estrutura só para dar nome ao que a thread carrega. Nada aqui atravessa a fronteira da thread.
struct Loopback {
    cliente: IAudioClient,
    captura: IAudioCaptureClient,
    formato: FormatoDoMixador,
}

impl Loopback {
    fn abrir(alvo: &Alvo) -> Result<Loopback> {
        let dispositivo = abrir_dispositivo(alvo)?;
        let cliente: IAudioClient = unsafe { dispositivo.Activate(CLSCTX_ALL, None) }?;
        let formato = FormatoDoMixador::ler(&cliente)?;

        unsafe {
            let p = cliente.GetMixFormat()?;
            // 200 ms de buffer. Não é latência: em modo compartilhado o mixador entrega o bloco
            // assim que ele existe. É folga para o caso de esta thread perder o passo.
            let r = cliente.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                2_000_000,
                0,
                p,
                None,
            );
            CoTaskMemFree(Some(p as *const _));
            r?;
        }

        // **Loopback é polling, não evento.** `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` junto de
        // `AUDCLNT_STREAMFLAGS_LOOPBACK` não é suportado: a captura de loopback não tem um relógio
        // próprio para disparar o evento, ela anda quando o mixador anda. Por isso o laço abaixo
        // dorme e pergunta.
        let captura: IAudioCaptureClient = unsafe { cliente.GetService() }?;
        unsafe { cliente.Start() }?;
        Ok(Loopback { cliente, captura, formato })
    }
}

/// A hora de agora no QPC, em µs: o mesmo relógio (e a mesma unidade, depois de dividir por 10) do
/// `u64QPCPosition` que o `GetBuffer` dá a cada pacote.
pub(crate) fn qpc_agora_us() -> u64 {
    let (mut c, mut f) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 || c <= 0 {
        return 0;
    }
    (c as u128 * 1_000_000 / f as u128) as u64
}

impl Drop for Loopback {
    fn drop(&mut self) {
        unsafe {
            let _ = self.cliente.Stop();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn correr_captura(
    alvo: Alvo,
    preset: PresetDeAudio,
    origem: Instant,
    tom_hz: Option<u32>,
    tx: Sender<PacoteDeAudio>,
    contadores: Arc<ContadoresDeAudio>,
    descricao: Arc<std::sync::Mutex<String>>,
    parar: Arc<AtomicBool>,
) -> std::result::Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let _fim_do_com = FimDoCom;

    let taxa_alvo = preset.codec.relogio_hz();
    let canais = preset.canais as usize;
    let por_quadro = preset.amostras_por_quadro(); // amostras por canal, 20 ms
    let i16_por_quadro = por_quadro * canais;

    let entrada = Loopback::abrir(&alvo).map_err(|e| format!("{e}"))?;
    let formato = entrada.formato;

    let mut codificador = Codificador::novo(taxa_alvo, preset.canais, aplicacao_do(&preset))
        .map_err(|e| format!("libopus não abriu: {e}"))?;
    configurar(&mut codificador, &preset).map_err(|e| format!("libopus recusou o preset: {e}"))?;
    let lookahead = codificador.lookahead().unwrap_or(0);

    let texto = format!(
        "{} | alvo={taxa_alvo} Hz {} canal(is) quadro={} amostras ({} ms) opus={} bps \
         lookahead={lookahead} amostras libopus=\"{}\"",
        formato,
        preset.canais,
        por_quadro,
        preset.duracao_do_quadro_ms,
        preset.taxa_media_bits,
        quall_opus::versao(),
    );
    registro::linha(format!("audio: {texto}"));
    *descricao.lock().unwrap_or_else(|e| e.into_inner()) = texto;

    // **A linha do tempo** (`linha_do_loopback.rs`, `som-no-receptor.md` §19.2 e §19.6): o
    // reamostrador com a disciplina da deriva, o buraco pela posição do dispositivo, o silêncio na
    // saída. Ela faz 48 kHz estéreo, que é o preset do som do sistema (Opus); outro preset seria
    // outra cadeia, e é melhor recusar aqui do que mandar som na taxa errada.
    if taxa_alvo != TAXA_DE_SAIDA || canais != CANAIS {
        return Err(format!(
            "a linha do loopback faz 48 kHz estéreo; o preset pede {taxa_alvo} Hz com {canais} canal(is)"
        ));
    }
    // Os carimbos da linha estão em µs do QPC: a hora da primeira amostra, e depois 20 000 µs
    // exatos por quadro. Vão à origem comum (a mesma do vídeo) por um par (`Instant`, QPC) lido
    // junto aqui. Antes o deslocamento era lido depois do `Start`, e não na primeira amostra
    // (crítica 3, miúdo).
    let par_origem_us = origem.elapsed().as_micros() as u64;
    let par_qpc_us = qpc_agora_us();
    let mut linha = LinhaDoLoopback::nova(formato.taxa_hz, true, false);
    linha.comecar(par_qpc_us);

    let mut para_f32: Vec<f32> = Vec::new();
    let mut mapeado: Vec<f32> = Vec::new();
    let mut quadro: Vec<i16> = Vec::with_capacity(i16_por_quadro);
    let mut saida_opus = vec![0u8; 4000];
    let mut proxima_linha_da_disciplina = Instant::now() + Duration::from_secs(60);

    while !parar.load(Ordering::SeqCst) {
        let mut veio_alguma_coisa = false;

        loop {
            let disponivel = match unsafe { entrada.captura.GetNextPacketSize() } {
                Ok(n) => n,
                Err(e) => return Err(format!("GetNextPacketSize falhou: {e}")),
            };
            if disponivel == 0 {
                break;
            }
            veio_alguma_coisa = true;

            let mut dados: *mut u8 = std::ptr::null_mut();
            let mut quadros: u32 = 0;
            let mut bandeiras: u32 = 0;
            // A posição do dispositivo da primeira amostra do pacote: é por ela que um buraco no
            // meio do som é achado (N3 da segunda crítica da disciplina), e não pela hora.
            let mut posicao_do_dispositivo: u64 = 0;
            // A hora da primeira amostra do pacote, que o próprio motor de áudio carimba, em
            // unidades de 100 ns do QPC (§19.2).
            let mut posicao_qpc: u64 = 0;
            unsafe {
                entrada
                    .captura
                    .GetBuffer(
                        &mut dados,
                        &mut quadros,
                        &mut bandeiras,
                        Some(&mut posicao_do_dispositivo),
                        Some(&mut posicao_qpc),
                    )
                    .map_err(|e| format!("GetBuffer falhou: {e}"))?;
            }
            let hora_us = hora_do_pacote(
                bandeiras,
                AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32,
                posicao_qpc,
            );
            let descontinuidade = bandeiras & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;

            contadores.blocos.fetch_add(1, Ordering::Relaxed);
            contadores.quadros_pcm.fetch_add(quadros as u64, Ordering::Relaxed);
            if descontinuidade {
                contadores.descontinuidades.fetch_add(1, Ordering::Relaxed);
            }

            para_f32.clear();
            if bandeiras & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                // O mixador está dizendo "isto é silêncio e eu não zerei o buffer". Ler aqueles
                // bytes seria ler lixo — e lixo de um buffer de áudio do sistema é exatamente o
                // tipo de coisa que esta casa não lê.
                contadores.blocos_silenciosos.fetch_add(1, Ordering::Relaxed);
                para_f32.resize(quadros as usize * formato.canais as usize, 0.0);
            } else {
                let bytes = quadros as usize * formato.bytes_por_quadro as usize;
                let bruto = unsafe { std::slice::from_raw_parts(dados, bytes) };
                amostras_para_f32(bruto, formato, &mut para_f32);
            }

            unsafe {
                let _ = entrada.captura.ReleaseBuffer(quadros);
            }

            mapeado.clear();
            mapear_canais(&para_f32, formato.canais as usize, CANAIS, &mut mapeado);
            linha.pacote(Pacote {
                amostras: &mapeado,
                quadros: quadros as u64,
                hora_us,
                posicao: Some(posicao_do_dispositivo),
                descontinuidade,
            });
        }

        // O mixador parado: zeros **na saída**, no ritmo do host, até agora menos a folga.
        if !veio_alguma_coisa {
            linha.ocioso(qpc_agora_us());
        }
        contadores
            .amostras_de_preenchimento
            .store(linha.amostras_de_silencio + linha.amostras_de_buraco, Ordering::Relaxed);
        contadores.buracos.store(linha.buracos_por_posicao, Ordering::Relaxed);
        contadores.amostras_cortadas.store(linha.amostras_cortadas, Ordering::Relaxed);
        contadores.pacotes_sem_hora.store(linha.pacotes_sem_hora, Ordering::Relaxed);
        contadores.desvio_us.store(linha.desvio_us as i64 as u64, Ordering::Relaxed);
        contadores.maior_desvio_us.store(linha.maior_desvio_us as i64 as u64, Ordering::Relaxed);
        contadores
            .f_centesimos_de_ppm
            .store((linha.f_ppm() * 100.0).round() as i64 as u64, Ordering::Relaxed);
        contadores.degraus.store(linha.degraus, Ordering::Relaxed);
        if Instant::now() >= proxima_linha_da_disciplina {
            proxima_linha_da_disciplina += Duration::from_secs(60);
            registro::linha(format!(
                "audio: disciplina f_ppm={:.2} ajuste_ppm={:.2} desvio_us={:.0} degraus={} buracos_por_posicao={} \
                 silencio_ms={:.0} cortadas={} posicoes_para_tras={} sem_posicao={}",
                linha.f_ppm(),
                linha.ajuste_ppm(),
                linha.desvio_us,
                linha.degraus,
                linha.buracos_por_posicao,
                linha.amostras_de_silencio as f64 / 48.0,
                linha.amostras_cortadas,
                linha.posicoes_para_tras,
                linha.pacotes_sem_posicao,
            ));
        }

        // Quadros inteiros de 20 ms, e só eles. `codificar` exige exatamente este tamanho.
        while let Some((amostras, carimbo_qpc_us)) = linha.proximo_quadro() {
            quadro.clear();
            quadro.extend(amostras.iter().map(|v| para_i16(*v)));
            contadores.quadros_de_20ms.fetch_add(1, Ordering::Relaxed);

            if let Some(hz) = tom_hz {
                let (razao, rms) =
                    razao_do_tom(&quadro, canais, hz as f64, taxa_alvo as f64);
                contadores.quadros_medidos.fetch_add(1, Ordering::Relaxed);
                contadores
                    .soma_razao_milesimos
                    .fetch_add((razao * 1000.0) as u64, Ordering::Relaxed);
                contadores.soma_rms.fetch_add(rms as u64, Ordering::Relaxed);
                contadores.rms_maximo.fetch_max(rms as u64, Ordering::Relaxed);
                if razao >= LIMIAR_DO_TOM && rms >= RMS_MINIMO {
                    contadores.quadros_com_tom.fetch_add(1, Ordering::Relaxed);
                }
            }

            // **O carimbo é o da linha**: a hora da primeira amostra, e 20 000 µs exatos por
            // quadro — o tempo de mídia, que a disciplina mantém no relógio do host reamostrando o
            // conteúdo (§19.6). Ele já está fixado quando o encode roda: se o encode falhar, o
            // quadro ocupou os 20 ms dele assim mesmo (crítica 3, miúdo).
            let timestamp_us = na_origem(par_origem_us, par_qpc_us, carimbo_qpc_us);

            let n = match codificador.codificar(&quadro, &mut saida_opus) {
                Ok(n) => n,
                Err(e) => {
                    contadores.falhas_de_encode.fetch_add(1, Ordering::Relaxed);
                    registro::linha(format!("audio: codificar falhou: {e}"));
                    continue;
                }
            };
            let bytes = saida_opus[..n].to_vec();

            // **Conferir no fio, não no retorno da API.** O `fmtp` promete estéreo e sem FEC; o
            // byte de TOC do pacote que está saindo é quem confirma. Os dois leitores são
            // independentes de propósito — um parser que confirma a si mesmo não prova nada.
            if contadores.toc_conferidos.load(Ordering::Relaxed) < 20 {
                match Toc::conferir_contra_libopus(&bytes) {
                    Ok(_) => {
                        contadores.toc_conferidos.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(divergencia) => {
                        contadores.toc_divergentes.fetch_add(1, Ordering::Relaxed);
                        registro::linha(format!("audio: TOC divergente: {divergencia}"));
                    }
                }
            }

            contadores.pacotes_opus.fetch_add(1, Ordering::Relaxed);
            contadores.bytes_opus.fetch_add(n as u64, Ordering::Relaxed);

            if tx.try_send(PacoteDeAudio { bytes, timestamp_us }).is_err() {
                // A fila encheu: a thread da sessão está um segundo inteiro atrasada, o que só
                // acontece se ela travou. **Descartar e contar**, nunca bloquear: bloquear aqui
                // pararia a captura do WASAPI e o buraco viraria descontinuidade no mixador.
                //
                // O pacote descartado é o **novo**, e não o mais velho, porque o carimbo de tempo
                // é tempo de mídia: descartar o novo deixa um buraco no fim da linha do tempo, que
                // o receptor entende como perda; descartar o velho embaralharia a ordem dos
                // carimbos que já estão na fila.
                contadores.descartados_por_fila.fetch_add(1, Ordering::Relaxed);
            }
        }

        // 5 ms: metade do período típico do mixador (10 ms). Dormir o período inteiro deixaria um
        // bloco esperando meio período em média, e isso é latência de régua.
        std::thread::sleep(Duration::from_millis(5));
    }

    registro::linha(format!("audio (final): {}", contadores.linha()));
    Ok(())
}

/// `CoUninitialize` no fim da thread, inclusive se ela sair por erro.
struct FimDoCom;
impl Drop for FimDoCom {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// A aplicação da libopus que o preset implica.
///
/// **Lido do preset, não escrito à mão** — é a mesma regra que a sonda de áudio segue: se alguém
/// mudar `PRESET_AUDIO_DO_SISTEMA` no núcleo e esta casca tiver `128_000` cravado, o SDP anuncia
/// uma coisa e o encoder faz outra.
pub(crate) fn aplicacao_do(preset: &PresetDeAudio) -> Aplicacao {
    if preset.conteudo_e_fala {
        Aplicacao::Voz
    } else {
        Aplicacao::Audio
    }
}

pub(crate) fn configurar(c: &mut Codificador, preset: &PresetDeAudio) -> quall_opus::Resultado<()> {
    c.definir_taxa_de_bits(preset.taxa_media_bits)?;
    c.definir_fec_embutido(preset.fec)?;
    c.definir_perda_esperada(preset.perda_esperada_pct)?;
    c.definir_sinal(if preset.conteudo_e_fala { Sinal::Voz } else { Sinal::Automatico })?;
    // **DTX desligado, sempre.** Com ele um trecho de silêncio vira pacote de 1 byte, e o receptor
    // que não implementa a retomada ouve o silêncio como buraco. O `fmtp` já anuncia `usedtx=0`; o
    // encoder tem de concordar com o que o SDP promete.
    c.definir_dtx(false)?;
    // A complexidade da espécie (10, escrito de propósito), como a fronteira C
    // (`docs/teleprompter-com-camera.md` §8.12.17): hoje é o padrão da libopus, então nada muda no fio.
    c.definir_complexidade(preset.complexidade_do_encoder())?;
    Ok(())
}

// -------------------------------------------------------------------------------------------
// O tom de prova
// -------------------------------------------------------------------------------------------

/// **Bancada.** Um seno que este processo toca no endpoint que o loopback está capturando.
///
/// É a origem sintética que `docs/audio.md` §8 exige: sem ela, a única forma de saber se o áudio
/// chegou seria olhar (ouvir) o que a máquina estava tocando — que é a vida do usuário. Com ela, a
/// pergunta "o caminho funciona?" vira "a raia de 1 000 Hz tem energia?", que é um número.
///
/// Nunca liga sozinho: só com `--tom-de-prova`.
struct TomDeProva {
    parar: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TomDeProva {
    fn iniciar(alvo: &Alvo, hz: u32, amplitude: f32) -> std::result::Result<TomDeProva, String> {
        let parar = Arc::new(AtomicBool::new(false));
        let p = Arc::clone(&parar);
        let alvo = alvo.clone();
        let (pronto_tx, pronto_rx) = bounded::<std::result::Result<(), String>>(1);
        let thread = std::thread::Builder::new()
            .name("quall.tom".into())
            .spawn(move || {
                let r = correr_tom(&alvo, hz, amplitude, &p, &pronto_tx);
                if let Err(e) = r {
                    registro::linha(format!("audio: tom de prova parou: {e}"));
                }
            })
            .map_err(|e| format!("thread do tom: {e}"))?;

        // Esperar o cliente de renderização abrir antes de dizer que o tom subiu: um "ligado" que
        // é mentira estragaria a leitura de todos os contadores desta corrida.
        match pronto_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(TomDeProva { parar, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("o tom não respondeu em 3 s".into()),
        }
    }

    fn parar(mut self) {
        self.parar.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn correr_tom(
    alvo: &Alvo,
    hz: u32,
    amplitude: f32,
    parar: &AtomicBool,
    pronto: &Sender<std::result::Result<(), String>>,
) -> std::result::Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let _fim = FimDoCom;

    let montar = || -> Result<(IAudioClient, IAudioRenderClient, FormatoDoMixador, u32)> {
        let dispositivo = abrir_dispositivo(alvo)?;
        let cliente: IAudioClient = unsafe { dispositivo.Activate(CLSCTX_ALL, None) }?;
        let formato = FormatoDoMixador::ler(&cliente)?;
        unsafe {
            let p = cliente.GetMixFormat()?;
            let r = cliente.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, p, None);
            CoTaskMemFree(Some(p as *const _));
            r?;
        }
        let render: IAudioRenderClient = unsafe { cliente.GetService() }?;
        let tamanho = unsafe { cliente.GetBufferSize() }?;
        Ok((cliente, render, formato, tamanho))
    };

    let (cliente, render, formato, tamanho) = match montar() {
        Ok(t) => {
            let _ = pronto.try_send(Ok(()));
            t
        }
        Err(e) => {
            let _ = pronto.try_send(Err(format!("{e}")));
            return Err(format!("{e}"));
        }
    };

    unsafe { cliente.Start() }.map_err(|e| format!("Start do tom: {e}"))?;
    let mut fase: f64 = 0.0;
    let passo = 2.0 * std::f64::consts::PI * hz as f64 / formato.taxa_hz as f64;

    while !parar.load(Ordering::SeqCst) {
        let usado = unsafe { cliente.GetCurrentPadding() }.unwrap_or(0);
        let livre = tamanho.saturating_sub(usado);
        if livre > 0 {
            if let Ok(destino) = unsafe { render.GetBuffer(livre) } {
                escrever_seno(destino, livre, formato, amplitude, &mut fase, passo);
                let _ = unsafe { render.ReleaseBuffer(livre, 0) };
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    unsafe {
        let _ = cliente.Stop();
    }
    Ok(())
}

/// Escreve `quadros` quadros de seno no formato que o mixador pediu.
///
/// O mesmo valor em todos os canais: um tom mono tocado em estéreo. É de propósito — o Goertzel do
/// outro lado tira a média dos canais, e um tom que só existe num canal daria metade da energia e
/// um limiar que precisaria de explicação.
fn escrever_seno(
    destino: *mut u8,
    quadros: u32,
    formato: FormatoDoMixador,
    amplitude: f32,
    fase: &mut f64,
    passo: f64,
) {
    let canais = formato.canais as usize;
    for q in 0..quadros as usize {
        let v = (fase.sin() as f32) * amplitude;
        *fase += passo;
        if *fase > 2.0 * std::f64::consts::PI {
            *fase -= 2.0 * std::f64::consts::PI;
        }
        for c in 0..canais {
            let indice = q * canais + c;
            unsafe {
                match formato.amostra {
                    Amostra::Float32 => {
                        let p = (destino as *mut f32).add(indice);
                        std::ptr::write_unaligned(p, v);
                    }
                    Amostra::Int16 => {
                        let p = (destino as *mut i16).add(indice);
                        std::ptr::write_unaligned(p, para_i16(v));
                    }
                    Amostra::Int32 => {
                        let p = (destino as *mut i32).add(indice);
                        let escalado = (v as f64 * 2_147_483_648.0)
                            .clamp(-2_147_483_648.0, 2_147_483_647.0)
                            as i32;
                        std::ptr::write_unaligned(p, escalado);
                    }
                }
            }
        }
    }
}

// -------------------------------------------------------------------------------------------
// Testes
// -------------------------------------------------------------------------------------------

#[cfg(test)]
mod testes {
    use super::*;

    fn seno(n: usize, canais: usize, hz: f64, taxa: f64, amplitude: f64) -> Vec<i16> {
        (0..n)
            .flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * hz * i as f64 / taxa).sin() * amplitude;
                let a = (v * 32768.0) as i16;
                std::iter::repeat(a).take(canais)
            })
            .collect()
    }

    #[test]
    fn goertzel_acha_o_tom_na_raia_e_ignora_outra_frequencia() {
        // 1 000 Hz cai exatamente numa raia de 960 amostras a 48 kHz (a raia mede 50 Hz), então um
        // seno puro ali tem de dar ~1,0 sem vazamento nenhum.
        let quadro = seno(960, 2, 1000.0, 48000.0, 0.5);
        let (razao, rms) = razao_do_tom(&quadro, 2, 1000.0, 48000.0);
        assert!(razao > 0.98, "seno de 1 kHz medido na raia de 1 kHz deu {razao}");
        assert!(rms > 10_000.0, "rms de um seno de amplitude 0,5 deu {rms}");

        // O mesmo quadro, medido noutra raia: tem de sumir.
        let (outra, _) = razao_do_tom(&quadro, 2, 3000.0, 48000.0);
        assert!(outra < 0.05, "seno de 1 kHz medido na raia de 3 kHz deu {outra}");
    }

    #[test]
    fn goertzel_no_silencio_nao_inventa_tom() {
        let quadro = vec![0i16; 960 * 2];
        let (razao, rms) = razao_do_tom(&quadro, 2, 1000.0, 48000.0);
        assert_eq!(rms as u64, 0);
        assert!(razao < LIMIAR_DO_TOM || rms < RMS_MINIMO, "silêncio contou como tom");
    }

    #[test]
    fn mono_vira_estereo_por_duplicacao() {
        // A track de sistema anuncia `stereo=1` no SDP. Entregar mono faria o SDP mentir — é a
        // regra "declarar no fio o que se faz de verdade".
        let entrada = vec![0.25f32, -0.5, 1.0];
        let mut saida = Vec::new();
        mapear_canais(&entrada, 1, 2, &mut saida);
        assert_eq!(saida, vec![0.25, 0.25, -0.5, -0.5, 1.0, 1.0]);
    }

    #[test]
    fn cinco_canais_viram_os_dois_da_frente() {
        // Um quadro de 5.1: FL, FR, C, LFE, SL, SR. Ficamos com FL e FR.
        let entrada = vec![0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        let mut saida = Vec::new();
        mapear_canais(&entrada, 6, 2, &mut saida);
        assert_eq!(saida, vec![0.1, 0.2]);
    }

    /// O reamostrador de antes (linear) saiu: o de agora é o da linha do tempo, com os testes dele
    /// em `reamostrador_sinc.rs` (qualidade, a descida de 96 kHz, o pulso). Daqui fica a saturação
    /// da conversão para `i16`, que era conferida junto.
    #[test]
    fn para_i16_satura_nas_pontas() {
        assert_eq!(para_i16(1.0), 32767);
        assert_eq!(para_i16(-1.0), -32768);
        assert_eq!(para_i16(2.0), 32767);
    }

    #[test]
    fn int16_do_mixador_volta_ao_mesmo_valor() {
        let formato = FormatoDoMixador {
            taxa_hz: 48_000,
            canais: 2,
            bytes_por_quadro: 4,
            amostra: Amostra::Int16,
        };
        let bruto: Vec<u8> = [1000i16, -1000, 32767, -32768]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut saida = Vec::new();
        amostras_para_f32(&bruto, formato, &mut saida);
        let de_volta: Vec<i16> = saida.iter().map(|v| para_i16(*v)).collect();
        assert_eq!(de_volta, vec![1000, -1000, 32767, -32768]);
    }
}
