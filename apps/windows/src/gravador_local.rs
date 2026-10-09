//! **Gravar local no R5 do Windows** (`docs/teleprompter-com-camera.md` §5 e §8.10, peça 5): a câmera
//! do dono, sem o texto e sem espelho, com o som do microfone, num MP4 fragmentado em
//! `Vídeos\Quall`.
//!
//! # O caminho de um quadro
//!
//! 1. **Na thread do dono** ([`EntradaDoGravador`], uma `SaidaDoDono`): a posição do anel é copiada —
//!    ou convertida, quando o anel é YUY2, de faixa completa ou de pixel não quadrado — para uma
//!    textura NV12 **livre** da reserva (8). Sem textura livre o quadro fica de fora e é contado: o
//!    dono nunca espera o gravador, e nada é escrito debaixo do codificador.
//! 2. **Na thread do gravador** (`quall.r5.gravador`): a textura vira uma `IMFTrackedSample` com o
//!    comprimento do buffer declarado (a armadilha da S-W1), e o retorno do alocador devolve a textura
//!    à reserva quando o Media Foundation solta a amostra — com **quarentena** de dois quadros (a
//!    revisão do plano, M4: o encoder pode ler na GPU por outra fila depois de soltar).
//! 3. **O tempo (G3)** é o carimbo real da câmera no zero do dono, menos o do primeiro quadro, e a
//!    duração de cada quadro é a distância até o seguinte (`regras_da_gravacao::LinhaDoVideo`).
//!
//! # O som
//!
//! O PCM mono de 48 kHz do microfone ([`QuadroDePcm`], carimbado no zero do dono) cai numa régua
//! (`regras_da_gravacao::ReguaDoSom`) que só entrega **até o fim do último quadro já escrito** (a
//! comporta) e completa com silêncio o que estiver mais de 350 ms atrás: o arquivo tem trilha de som
//! desde o começo, com o microfone ligado ou não ("Gravando SEM SOM" é a tela que diz). AAC 48 kHz
//! mono 128 kbit/s pelo escritor. **O atraso do AAC não é descontado** (hipótese: o MP4 do Media
//! Foundation escreve a lista de edição; o `ffprobe` de `start_time` decide na bancada).
//!
//! # O arquivo
//!
//! `Quall-AAAAMMDD-HHMMSS.gravando.mp4` enquanto grava, `….mp4` depois do `Finalize`. Um órfão (o
//! processo morto gravando) é recuperado na abertura seguinte da tela R5 ([`recuperar_pendentes`]):
//! a caixa incompleta do fim sai e ele vira `… (interrompido).mp4`.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use windows::core::{implement, Interface, Ref, Result as WinResult, HSTRING, PCWSTR};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};

use crate::capture::CapturedFrame;
use crate::idioma;
use crate::conversor_de_camera::ConversorDeCamera;
use crate::dono_da_captura::{InfoDaCamera, PlacaDoDono, SaidaDoDono};
use crate::regras_da_gravacao::{self as regras, LinhaDoVideo, ReguaDoSom};
use crate::registro;

/// Um quadro do microfone para o gravador: mono, 48 kHz, 16 bits, com a hora da primeira amostra em
/// µs no zero do dono.
#[derive(Clone, Debug)]
pub struct QuadroDePcm {
    pub carimbo_us: u64,
    pub amostras: Vec<i16>,
}

/// Quantas texturas a reserva do gravador tem (a revisão do plano, M4).
const RESERVA: usize = 8;
/// **A quarentena, no relógio** (a revisão do código, B1): uma textura devolvida só volta a ser
/// escrita 70 ms (dois quadros a 30 fps) depois da devolução. Contada em quadros copiados, ela
/// travava: com as oito devolvidas juntas e nenhuma cópia depois, nenhuma saía da quarentena, e sem
/// saída nenhuma cópia acontecia.
const QUARENTENA_US: u64 = 70_000;
/// Sem quadro novo no escritor por isto, com a câmera andando, o arquivo parou (M12).
const ARQUIVO_PARADO: Duration = Duration::from_secs(3);
/// De quanto em quanto tempo o espaço livre é conferido.
const CONFERIR_ESPACO: Duration = Duration::from_secs(5);
/// O som que chega antes do primeiro quadro espera até isto (2 s).
const SOM_ANTES_DO_PRIMEIRO: usize = 100;

/// A pasta das gravações: `Vídeos\Quall` (`SHGetKnownFolderPath(FOLDERID_Videos)`), criada se faltar.
pub fn pasta_padrao() -> Result<PathBuf, String> {
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Videos, KF_FLAG_DEFAULT};
    let base = unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None).map_err(|e| idioma::tf("a pasta Vídeos não foi achada: {}", &[&e]))?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    };
    if base.is_empty() {
        return Err(idioma::t("a pasta Vídeos veio vazia").into());
    }
    Ok(PathBuf::from(base).join("Quall"))
}

/// O espaço livre (para este usuário) no disco da pasta.
pub fn espaco_livre(pasta: &Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let largo: Vec<u16> = pasta.as_os_str().encode_wide_z();
    let mut livre = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(largo.as_ptr()), Some(&mut livre), None, None) }.ok()?;
    Some(livre)
}

trait LargoZ {
    fn encode_wide_z(&self) -> Vec<u16>;
}
impl LargoZ for std::ffi::OsStr {
    fn encode_wide_z(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}

/// A base do nome agora, na hora local.
fn base_agora() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    regras::base_do_nome(t.wYear.into(), t.wMonth.into(), t.wDay.into(), t.wHour.into(), t.wMinute.into(), t.wSecond.into())
}

// =============================================================================================
// A reserva de texturas
// =============================================================================================

struct Reserva {
    texturas: Vec<ID3D11Texture2D>,
    /// Livre para escrever.
    livre: Vec<AtomicBool>,
    /// Quando a textura foi devolvida, em µs desde `base` (a quarentena); 0 nunca foi usada.
    devolvida_em_us: Vec<AtomicU64>,
    base: Instant,
    devolucoes: AtomicU64,
    /// Quantas amostras (mais a mão do escritor) ainda seguram a textura: o quadro repetido num buraco
    /// (`regras::repartir`) vai ao encoder em várias amostras da mesma textura.
    usos: Vec<AtomicU32>,
}

impl Reserva {
    fn agora_us(&self) -> u64 {
        self.base.elapsed().as_micros() as u64
    }
    fn livre_agora(&self) -> Option<usize> {
        let agora = self.agora_us();
        (0..self.texturas.len()).find(|i| {
            self.livre[*i].load(Ordering::SeqCst)
                && regras::fora_da_quarentena(self.devolvida_em_us[*i].load(Ordering::SeqCst), agora, QUARENTENA_US)
        })
    }
    /// Devolve **sem** quarentena: a textura nunca chegou ao Media Foundation (a fila cheia, o
    /// carimbo recusado, a amostra que não nasceu).
    fn devolver_sem_uso(&self, i: usize) {
        self.livre[i].store(true, Ordering::SeqCst);
    }
    fn devolver(&self, i: usize) {
        self.devolvida_em_us[i].store(self.agora_us().max(1), Ordering::SeqCst);
        self.livre[i].store(true, Ordering::SeqCst);
        self.devolucoes.fetch_add(1, Ordering::Relaxed);
    }
    /// Solta um uso; o último devolve a textura (com quarentena se o Media Foundation a viu).
    fn soltar_uso(&self, i: usize, vista: bool) {
        if self.usos[i].fetch_sub(1, Ordering::SeqCst) == 1 {
            if vista {
                self.devolver(i);
            } else {
                self.devolver_sem_uso(i);
            }
        }
    }
}

/// **O retorno do alocador**: o Media Foundation soltou a amostra, e a textura volta à reserva.
#[implement(IMFAsyncCallback)]
struct Devolucao {
    reserva: Arc<Reserva>,
    posicao: usize,
}

impl IMFAsyncCallback_Impl for Devolucao_Impl {
    fn GetParameters(&self, _pdwflags: *mut u32, _pdwqueue: *mut u32) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }
    fn Invoke(&self, _pasyncresult: Ref<'_, IMFAsyncResult>) -> WinResult<()> {
        self.reserva.soltar_uso(self.posicao, true);
        Ok(())
    }
}

// =============================================================================================
// O estado que a tela lê
// =============================================================================================

/// Em que pé a gravação está.
#[derive(Clone, Debug, PartialEq)]
pub enum FaseDaGravacao {
    /// O escritor está abrindo, ou o primeiro quadro ainda não entrou no arquivo.
    Abrindo,
    /// O primeiro quadro entrou no arquivo.
    Gravando,
    Fechando,
    /// Acabou: o arquivo (se ficou algum), a duração, por quê, e se fechou inteiro.
    Fechada { arquivo: Option<PathBuf>, segundos: f64, motivo: String, inteira: bool },
}

#[derive(Clone, Debug)]
pub struct EstadoDaGravacao {
    pub fase: FaseDaGravacao,
    /// Quando o primeiro quadro entrou no arquivo.
    pub desde: Option<Instant>,
    pub quadros: u64,
    pub livre: Option<u64>,
    pub arquivo: PathBuf,
    /// Recebeu som do microfone há menos de 1 s.
    pub com_som: bool,
}

struct Comum {
    estado: Mutex<EstadoDaGravacao>,
    parar: Mutex<Option<String>>,
    acordar: Box<dyn Fn() + Send + Sync>,
}

impl Comum {
    fn fase(&self, f: FaseDaGravacao) {
        self.estado.lock().unwrap_or_else(|e| e.into_inner()).fase = f;
        (self.acordar)();
    }
}

enum Evento {
    Quadro { posicao: usize, carimbo_us: u64 },
    Som(QuadroDePcm),
}

/// Uma gravação. Criada com [`Gravador::comecar`]; a tela pendura a [`EntradaDoGravador`] no dono e
/// passa o ramal do som ao microfone.
pub struct Gravador {
    comum: Arc<Comum>,
    tx: Sender<Evento>,
    /// A porta do começo (`regras::porta_deixa`): 0 até o escritor ficar pronto; depois, o instante
    /// disso em µs no relógio do dono. Quadro e som capturados antes não entram.
    porta: Arc<AtomicU64>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub numero: u64,
}

/// O que a tela pede.
pub struct PedidoDeGravacao {
    pub pasta: PathBuf,
    pub placa: PlacaDoDono,
    pub info: InfoDaCamera,
    /// O tamanho da imagem em pixel quadrado (o DV anamórfico passa pelo conversor).
    pub tamanho_exibido: (u32, u32),
    /// O fps medido da câmera (declaração do tipo; o arquivo leva os carimbos reais).
    pub fps: f64,
    pub origem: Instant,
    pub quem_pediu: String,
    /// Bancada: um nome de arquivo fixo em vez da hora.
    pub base: Option<String>,
}

static NUMERO: AtomicU64 = AtomicU64::new(0);

/// **O vídeo em grade de fps constante** (o padrão desde a bancada de 27/09, §8.10.6): com o carimbo
/// real (fps variável, a câmera em degraus de 16 ms), o escritor processou 599,966 s e o MP4 saiu com
/// 599,297 s — o mux fMP4 do Media Foundation perde tempo; em grade, A/V +18,6 ms em 10 min. O
/// carimbo real fica para a bancada (`--gravacao-no-carimbo-real`).
static EM_GRADE: AtomicBool = AtomicBool::new(true);

pub fn usar_grade(sim: bool) {
    EM_GRADE.store(sim, Ordering::SeqCst);
}

/// Quantos tempos de amostra o diário guarda no começo e no fim (a bancada de 26/09).
const TEMPOS_NO_DIARIO: usize = 100;

/// Os tempos do escritor para um fluxo (`MF_SINK_WRITER_STATISTICS`): o último carimbo recebido, o
/// último que saiu do encoder e o último que o mux processou, em segundos (a bancada de 26/09: o
/// arquivo do G ficou 0,78 s mais curto que o que entrou, e isto diz em qual etapa).
fn tempos_do_escritor(w: &IMFSinkWriter, fluxo: u32) -> String {
    let mut st = MF_SINK_WRITER_STATISTICS { cb: std::mem::size_of::<MF_SINK_WRITER_STATISTICS>() as u32, ..Default::default() };
    match unsafe { w.GetStatistics(fluxo, &mut st) } {
        Ok(()) => format!(
            "recebido={:.3} codificado={:.3} processado={:.3} s (n {}/{}/{})",
            st.llLastTimestampReceived as f64 / 1e7,
            st.llLastTimestampEncoded as f64 / 1e7,
            st.llLastTimestampProcessed as f64 / 1e7,
            st.qwNumSamplesReceived,
            st.qwNumSamplesEncoded,
            st.qwNumSamplesProcessed
        ),
        Err(e) => format!("(sem estatística: {e})"),
    }
}

/// Uma lista de `(pts, duração)` em 100 ns como `pts/dur` em ms.
fn tempos_em_ms(v: &[(i64, i64)]) -> String {
    v.iter().map(|(p, d)| format!("{:.2}/{:.2}", *p as f64 / 1e4, *d as f64 / 1e4)).collect::<Vec<_>>().join(" ")
}

impl Gravador {
    /// **Começa**: confere a pasta e o espaço, monta a reserva e o conversor (na thread de quem chama,
    /// que é a da tela; nada de Media Foundation aqui), e sobe a thread do escritor. `Err` com o
    /// motivo legível (vai ao controle remoto, `recusar_gravacao`).
    pub fn comecar(p: PedidoDeGravacao, acordar: Box<dyn Fn() + Send + Sync>) -> Result<(Arc<Gravador>, EntradaDoGravador), String> {
        std::fs::create_dir_all(&p.pasta).map_err(|e| idioma::tf("a pasta {} não foi criada: {}", &[&p.pasta.display(), &e]))?;
        let livre = espaco_livre(&p.pasta);
        if let Some(l) = livre {
            if l < regras::ESPACO_PARA_COMECAR {
                return Err(regras::texto_sem_espaco(l, regras::ESPACO_PARA_COMECAR));
            }
        }
        let (w, h) = (p.tamanho_exibido.0 & !1, p.tamanho_exibido.1 & !1);
        if w < 16 || h < 16 {
            return Err(idioma::tf("a imagem da câmera é pequena demais para gravar ({}x{})", &[&w, &h]));
        }
        let precisa = crate::regras_da_camera::precisa_do_processador(
            p.info.formato.subtipo(),
            p.info.faixa_completa,
            (w, h) != (p.info.largura, p.info.altura),
            p.info.entrelacamento_no_anel != crate::regras_da_camera::Entrelacamento::Progressivo,
        );
        let conversor = if precisa {
            let c = ConversorDeCamera::novo(
                &p.placa.device,
                p.info.formato.dxgi(),
                p.info.largura,
                p.info.altura,
                w,
                h,
                p.info.faixa_completa,
                p.info.matriz_709,
                p.info.entrelacamento_no_anel,
            )
            .map_err(|e| format!("o conversor da gravação não subiu: {e}"))?;
            registro::linha(format!("r5 gravação: conversor: {}", c.descricao));
            Some(c)
        } else {
            None
        };
        let mut texturas = Vec::with_capacity(RESERVA);
        for _ in 0..RESERVA {
            texturas.push(textura_nv12(&p.placa.device, w, h).map_err(|e| format!("a reserva da gravação não foi criada: {e}"))?);
        }
        let reserva = Arc::new(Reserva {
            texturas,
            livre: (0..RESERVA).map(|_| AtomicBool::new(true)).collect(),
            devolvida_em_us: (0..RESERVA).map(|_| AtomicU64::new(0)).collect(),
            base: Instant::now(),
            devolucoes: AtomicU64::new(0),
            usos: (0..RESERVA).map(|_| AtomicU32::new(0)).collect(),
        });
        let base = p.base.clone().unwrap_or_else(base_agora);
        let mut nome = format!("{base}{}", regras::SUFIXO_GRAVANDO);
        let mut k = 2;
        while p.pasta.join(&nome).exists() {
            nome = format!("{base} ({k}){}", regras::SUFIXO_GRAVANDO);
            k += 1;
        }
        let arquivo = p.pasta.join(&nome);
        let numero = NUMERO.fetch_add(1, Ordering::SeqCst) + 1;
        let comum = Arc::new(Comum {
            estado: Mutex::new(EstadoDaGravacao { fase: FaseDaGravacao::Abrindo, desde: None, quadros: 0, livre, arquivo: arquivo.clone(), com_som: false }),
            parar: Mutex::new(None),
            acordar,
        });
        // A fila: ~2 s de quadros e de som. Cheia, o dono descarta (nunca espera).
        let (tx, rx) = bounded::<Evento>(256);
        let taxa = regras::taxa_da_gravacao(w, h, regras::fps_nominal(p.fps));
        registro::linha(format!(
            "r5 gravação #{numero} pedida ({}): {w}x{h} {:.1} fps medidos, {} kbit/s, AAC mono 48 kHz, em {} | livre={}",
            p.quem_pediu,
            p.fps,
            taxa / 1000,
            arquivo.display(),
            livre.map(|l| format!("{} MB", l / (1024 * 1024))).unwrap_or_else(|| "?".into())
        ));
        let c = Arc::clone(&comum);
        let r = Arc::clone(&reserva);
        let pasta = p.pasta.clone();
        let origem = p.origem;
        let gerenciador = Arc::clone(&p.placa.gerenciador);
        let matriz_709 = p.info.matriz_709;
        let fps = regras::fps_nominal(p.fps);
        let porta = Arc::new(AtomicU64::new(0));
        let porta_ = Arc::clone(&porta);
        let h_ = std::thread::Builder::new()
            .name("quall.r5.gravador".into())
            .spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                correr(&c, rx, &r, &porta_, &arquivo, &pasta, origem, &gerenciador.0, (w, h), fps, taxa, matriz_709, numero);
                if com.is_ok() {
                    unsafe { CoUninitialize() };
                }
            })
            .map_err(|e| format!("a thread da gravação não subiu: {e}"))?;
        let entrada = EntradaDoGravador {
            reserva,
            conversor,
            tx: tx.clone(),
            porta: Arc::clone(&porta),
            antes_da_porta: 0,
            origem,
            sem_textura: 0,
            fila_cheia: 0,
            copiados: 0,
            erros: 0,
        };
        Ok((Arc::new(Gravador { comum, tx, porta, thread: Mutex::new(Some(h_)), numero }), entrada))
    }

    pub fn estado(&self) -> EstadoDaGravacao {
        self.comum.estado.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Pede o fim, com o motivo (o primeiro vale). O arquivo fecha na thread do gravador.
    pub fn parar(&self, motivo: &str) {
        let mut g = self.comum.parar.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            registro::linha(format!("r5 gravação #{}: parando: {motivo}", self.numero));
            *g = Some(motivo.to_string());
        }
    }

    /// O ramal do som: o microfone manda cada quadro para cá (sem esperar).
    pub fn ramal_do_som(&self) -> Box<dyn Fn(QuadroDePcm) + Send + Sync> {
        let tx = self.tx.clone();
        let porta = Arc::clone(&self.porta);
        Box::new(move |q| {
            // O som de antes de o escritor ficar pronto não entra (a bancada de 24/09).
            if regras::porta_deixa(porta.load(Ordering::SeqCst), q.carimbo_us) {
                let _ = tx.try_send(Evento::Som(q));
            }
        })
    }

    /// A thread do escritor acabou?
    pub fn terminou(&self) -> bool {
        self.thread.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }
}

/// **Começa a gravar a câmera de um dono** (a tela R5 e a câmera comum, `docs/teleprompter-com-camera.md`
/// §8.10): confere que a câmera está aberta e entregando, e pede a gravação no tamanho em pixel
/// quadrado e no fps medido. A entrada já sai pendurada no dono. `Err` com o motivo legível (vai ao
/// controle remoto e à tela).
pub fn comecar_no_dono(
    dono: &crate::dono_da_captura::DonoDaCaptura,
    pasta: Option<PathBuf>,
    quem: &str,
    acordar: Box<dyn Fn() + Send + Sync>,
) -> Result<Arc<Gravador>, String> {
    use crate::dono_da_captura::FaseDoDono;
    if dono.fase() != FaseDoDono::Aberto {
        return Err(idioma::t("a câmera não está aberta").into());
    }
    if dono.parada_ha().is_some() || dono.ultimo_quadro_ha().is_none_or(|d| d > Duration::from_secs(1)) {
        return Err(idioma::t("a câmera não está entregando imagem").into());
    }
    let (Some(placa), Some(info)) = (dono.placa(), dono.info()) else { return Err(idioma::t("a câmera não está aberta").into()) };
    let Some(pasta) = pasta else { return Err(idioma::t("a pasta Vídeos não foi achada").into()) };
    let tamanho = dono.tamanho_exibido().unwrap_or((info.largura, info.altura));
    let pedido = PedidoDeGravacao {
        pasta,
        placa,
        info,
        tamanho_exibido: tamanho,
        fps: dono.fps_medido(),
        origem: dono.origem,
        quem_pediu: quem.to_string(),
        base: None,
    };
    let (g, entrada) = Gravador::comecar(pedido, acordar)?;
    let velha = dono.pendurar_gravador(Some(Box::new(entrada)));
    drop(velha);
    Ok(g)
}

// =============================================================================================
// A entrada, na thread do dono
// =============================================================================================

/// **O gravador pendurado no dono**: copia (ou converte) cada quadro numa textura livre da reserva e
/// avisa a thread do escritor. Nunca espera.
pub struct EntradaDoGravador {
    reserva: Arc<Reserva>,
    conversor: Option<ConversorDeCamera>,
    tx: Sender<Evento>,
    porta: Arc<AtomicU64>,
    /// Quadros que chegaram com a porta fechada (o escritor abrindo).
    antes_da_porta: u64,
    origem: Instant,
    sem_textura: u64,
    fila_cheia: u64,
    copiados: u64,
    erros: u64,
}

// SAFETY: o conversor é criado na thread da tela e, depois de pendurado, usado só na thread do dono
// (os objetos D3D11 dele são livres de thread; a `Cell` do índice nunca é tocada de duas threads).
unsafe impl Send for EntradaDoGravador {}

impl SaidaDoDono for EntradaDoGravador {
    fn quadro(&mut self, q: &CapturedFrame, placa: &PlacaDoDono) {
        // **A porta** (a bancada de 24/09): com o escritor abrindo, o quadro não ocupa a reserva —
        // antes, oito quadros de antes viravam o zero do arquivo e o nono vinha segundos depois.
        let carimbo_us = q.captured_at.saturating_duration_since(self.origem).as_micros() as u64;
        if !regras::porta_deixa(self.porta.load(Ordering::SeqCst), carimbo_us) {
            self.antes_da_porta += 1;
            return;
        }
        let Some(i) = self.reserva.livre_agora() else {
            self.sem_textura += 1;
            return;
        };
        let fonte = match &self.conversor {
            Some(c) => match c.converter(&q.texture) {
                Ok(t) => t,
                Err(e) => {
                    self.erros += 1;
                    if self.erros <= 3 {
                        registro::linha(format!("r5 gravação: o quadro não foi convertido: {e}"));
                    }
                    return;
                }
            },
            None => q.texture.clone(),
        };
        self.reserva.livre[i].store(false, Ordering::SeqCst);
        unsafe {
            placa.context.CopyResource(&self.reserva.texturas[i], &fonte);
            // A cópia vai para a GPU antes de o escritor (em outra thread) entregar a textura ao
            // encoder (a mesma defesa do `take_frame`, T1).
            placa.context.Flush();
        }
        self.copiados += 1;
        if self.tx.try_send(Evento::Quadro { posicao: i, carimbo_us }).is_err() {
            self.fila_cheia += 1;
            self.reserva.devolver_sem_uso(i);
        }
    }

    fn nome(&self) -> &'static str {
        "gravador"
    }

    fn relato(&mut self) -> Option<String> {
        Some(format!(
            "antes_da_porta={} copiados={} sem_textura_livre={} fila_cheia={} devolucoes={} erros={}",
            self.antes_da_porta,
            self.copiados,
            self.sem_textura,
            self.fila_cheia,
            self.reserva.devolucoes.load(Ordering::Relaxed),
            self.erros
        ))
    }
}

/// Uma textura NV12 que o conversor escreve e o encoder lê.
fn textura_nv12(dispositivo: &ID3D11Device, largura: u32, altura: u32) -> WinResult<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: largura,
        Height: altura,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t: Option<ID3D11Texture2D> = None;
    unsafe { dispositivo.CreateTexture2D(&desc, None, Some(&mut t))? };
    t.ok_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_POINTER, "sem textura"))
}

// =============================================================================================
// O escritor, na thread do gravador
// =============================================================================================

fn par(t: &IMFMediaType, chave: &windows::core::GUID, a: u32, b: u32) -> WinResult<()> {
    unsafe { t.SetUINT64(chave, (u64::from(a) << 32) | u64::from(b)) }
}

fn com<T>(passo: &str, r: WinResult<T>) -> Result<T, String> {
    r.map_err(|e| format!("{passo}: {e}"))
}

struct Escritor {
    w: IMFSinkWriter,
    video: u32,
    som: u32,
    /// Quanto cada etapa da abertura levou (a bancada de 24/09: 4008 ms frio, 203 ms quente).
    etapas: String,
}

#[allow(clippy::too_many_arguments)]
fn abrir_escritor(arquivo: &Path, gerenciador: &IMFDXGIDeviceManager, (w, h): (u32, u32), fps: u32, taxa: u32, matriz_709: bool) -> Result<Escritor, String> {
    let t = Instant::now();
    let mut etapas: Vec<String> = Vec::new();
    let mut marca = |nome: &str| etapas.push(format!("{nome}={}", t.elapsed().as_millis()));
    unsafe {
        let mut a: Option<IMFAttributes> = None;
        com("MFCreateAttributes", MFCreateAttributes(&mut a, 4))?;
        let a = a.ok_or("MFCreateAttributes sem atributos")?;
        com("contêiner", a.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_FMPEG4))?;
        com("hardware", a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1))?;
        com("gerenciador DXGI", a.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, gerenciador))?;
        // O limite é a reserva de texturas: o `WriteSample` não bloqueia esta thread (M6).
        com("sem estrangular", a.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1))?;
        let wr = com("MFCreateSinkWriterFromURL", MFCreateSinkWriterFromURL(&HSTRING::from(arquivo.as_os_str()), None, &a))?;
        marca("criar");

        let saida = com("tipo de vídeo", MFCreateMediaType())?;
        com("vídeo", saida.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video))?;
        com("vídeo", saida.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))?;
        com("vídeo", saida.SetUINT32(&MF_MT_AVG_BITRATE, taxa))?;
        com("vídeo", saida.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32))?;
        com("vídeo", saida.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32))?;
        com("vídeo", par(&saida, &MF_MT_FRAME_SIZE, w, h))?;
        com("vídeo", par(&saida, &MF_MT_FRAME_RATE, fps, 1))?;
        com("vídeo", par(&saida, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1))?;
        let video = com("AddStream vídeo", wr.AddStream(&saida))?;

        let entrada = com("tipo NV12", MFCreateMediaType())?;
        com("NV12", entrada.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video))?;
        com("NV12", entrada.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12))?;
        com("NV12", entrada.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32))?;
        com("NV12", entrada.SetUINT32(&MF_MT_DEFAULT_STRIDE, w))?;
        com("NV12", par(&entrada, &MF_MT_FRAME_SIZE, w, h))?;
        com("NV12", par(&entrada, &MF_MT_FRAME_RATE, fps, 1))?;
        com("NV12", par(&entrada, &MF_MT_PIXEL_ASPECT_RATIO, 1, 1))?;
        // A faixa limitada e a matriz (o conversor entrega faixa limitada; a matriz é a da câmera).
        let _ = entrada.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
        let _ = entrada.SetUINT32(
            &MF_MT_YUV_MATRIX,
            if matriz_709 { MFVideoTransferMatrix_BT709.0 as u32 } else { MFVideoTransferMatrix_BT601.0 as u32 },
        );
        // O encoder: um IDR a cada 2 s e sem quadros B (nada reordenado; o fragmento perdido no fim
        // não leva quadros de antes).
        let mut parametros: Option<IMFAttributes> = None;
        com("MFCreateAttributes", MFCreateAttributes(&mut parametros, 2))?;
        let parametros = parametros.ok_or("MFCreateAttributes sem atributos")?;
        let _ = parametros.SetUINT32(&CODECAPI_AVEncMPVGOPSize, fps.max(1) * 2);
        let _ = parametros.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0);
        // Aqui o escritor procura e ativa o encoder H.264 (hipótese: é onde vai o tempo frio).
        com("SetInputMediaType vídeo", wr.SetInputMediaType(video, &entrada, &parametros))?;
        marca("encoder_de_video");

        let aac = com("tipo AAC", MFCreateMediaType())?;
        com("AAC", aac.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio))?;
        com("AAC", aac.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC))?;
        com("AAC", aac.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000))?;
        com("AAC", aac.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1))?;
        com("AAC", aac.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16))?;
        // 128 kbit/s = 16 000 B/s (um dos quatro valores que o AAC do Media Foundation aceita).
        com("AAC", aac.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16_000))?;
        com("AAC", aac.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0))?;
        let som = com("AddStream som", wr.AddStream(&aac))?;
        let pcm = com("tipo PCM", MFCreateMediaType())?;
        com("PCM", pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio))?;
        com("PCM", pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM))?;
        com("PCM", pcm.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48_000))?;
        com("PCM", pcm.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 1))?;
        com("PCM", pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16))?;
        com("PCM", pcm.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2))?;
        com("PCM", pcm.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 96_000))?;
        com("SetInputMediaType som", wr.SetInputMediaType(som, &pcm, None))?;
        marca("encoder_de_som");
        com("BeginWriting", wr.BeginWriting())?;
        marca("begin");
        Ok(Escritor { w: wr, video, som, etapas: etapas.join(" ") })
    }
}

/// O nome do encoder que o escritor montou (a S-W1 viu um sem nome amigável na UHD 630).
fn encoder_do_escritor(w: &IMFSinkWriter, fluxo: u32) -> String {
    unsafe {
        let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Err(e) = w.GetServiceForStream(fluxo, &windows::core::GUID::zeroed(), &IMFTransform::IID, &mut p) {
            return format!("(não perguntável: {e})");
        }
        let t = IMFTransform::from_raw(p);
        let Ok(a) = t.GetAttributes() else { return "(transform sem atributos)".into() };
        // O par `(buf, cap)` do `IMFAttributes`: o comprimento primeiro, depois o texto.
        let nome = match a.GetStringLength(&MFT_FRIENDLY_NAME_Attribute) {
            Ok(tam) => {
                let mut texto: Vec<u16> = vec![0; tam as usize + 1];
                let mut n = 0u32;
                if a.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut texto, Some(&mut n)).is_ok() {
                    String::from_utf16_lossy(&texto[..(n as usize).min(texto.len())])
                } else {
                    "(sem nome amigável)".into()
                }
            }
            Err(_) => "(sem nome amigável)".into(),
        };
        let hardware = a.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1;
        format!("{nome} (assíncrono/hardware={hardware})")
    }
}

fn amostra_de_video(reserva: &Arc<Reserva>, posicao: usize, pts: i64, duracao: i64) -> WinResult<IMFSample> {
    unsafe {
        let b = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &reserva.texturas[posicao], 0, false)?;
        let b2: IMF2DBuffer = b.cast()?;
        b.SetCurrentLength(b2.GetContiguousLength()?)?;
        let rastreada = MFCreateTrackedSample()?;
        let s: IMFSample = rastreada.cast()?;
        s.AddBuffer(&b)?;
        s.SetSampleTime(pts)?;
        s.SetSampleDuration(duracao.max(1))?;
        let devolucao: IMFAsyncCallback = Devolucao { reserva: Arc::clone(reserva), posicao }.into();
        rastreada.SetAllocator(&devolucao, None)?;
        Ok(s)
    }
}

fn amostra_de_som(p: &regras::PedacoDeSom) -> WinResult<IMFSample> {
    unsafe {
        let bytes = p.amostras.len() * 2;
        let b = MFCreateMemoryBuffer(bytes as u32)?;
        let mut dados: *mut u8 = std::ptr::null_mut();
        b.Lock(&mut dados, None, None)?;
        std::ptr::copy_nonoverlapping(p.amostras.as_ptr() as *const u8, dados, bytes);
        b.Unlock()?;
        b.SetCurrentLength(bytes as u32)?;
        let s = MFCreateSample()?;
        s.AddBuffer(&b)?;
        s.SetSampleTime(p.pts_100ns())?;
        s.SetSampleDuration(p.duracao_100ns())?;
        Ok(s)
    }
}

#[allow(clippy::too_many_arguments)]
fn correr(
    comum: &Arc<Comum>,
    rx: Receiver<Evento>,
    reserva: &Arc<Reserva>,
    porta: &AtomicU64,
    arquivo: &Path,
    pasta: &Path,
    origem: Instant,
    gerenciador: &IMFDXGIDeviceManager,
    tamanho: (u32, u32),
    fps: u32,
    taxa: u32,
    matriz_709: bool,
    numero: u64,
) {
    let t0 = Instant::now();
    let escritor = match abrir_escritor(arquivo, gerenciador, tamanho, fps, taxa, matriz_709) {
        Ok(e) => e,
        Err(motivo) => {
            registro::linha(format!("r5 gravação #{numero}: !! o escritor não abriu: {motivo}"));
            let _ = std::fs::remove_file(arquivo);
            comum.fase(FaseDaGravacao::Fechada { arquivo: None, segundos: 0.0, motivo: idioma::tf("o arquivo não abriu: {}", &[&motivo]), inteira: false });
            return;
        }
    };
    // **A porta abre agora**: o zero das duas trilhas é o primeiro quadro capturado daqui em diante.
    let aberta_em_us = (origem.elapsed().as_micros() as u64).max(1);
    porta.store(aberta_em_us, Ordering::SeqCst);
    let t_pronto = Instant::now();
    registro::linha(format!(
        "r5 gravação #{numero}: arquivo aberto em {} ms ({}) — vídeo \"{}\", som \"{}\"; a porta abre em {} ms no relógio do dono; vídeo {}",
        t0.elapsed().as_millis(),
        escritor.etapas,
        encoder_do_escritor(&escritor.w, escritor.video),
        encoder_do_escritor(&escritor.w, escritor.som),
        aberta_em_us / 1000,
        if EM_GRADE.load(Ordering::SeqCst) { format!("em grade de {fps} fps") } else { "no carimbo real (bancada)".to_string() }
    ));
    let mut g = Gravacao {
        escritor: &escritor,
        reserva,
        comum,
        numero,
        t0,
        origem,
        aberta_em_us,
        passo: regras::passo_do_fps(fps),
        repetidos: 0,
        linha: if EM_GRADE.load(Ordering::SeqCst) { LinhaDoVideo::em_grade(fps) } else { LinhaDoVideo::default() },
        primeiros_tempos: Vec::new(),
        ultimos_tempos: std::collections::VecDeque::new(),
        primeiros_carimbos: Vec::new(),
        regua: None,
        som_antes: Vec::new(),
        ultimo_quadro: Instant::now(),
        ultimo_som: None,
        quadros_escritos: 0,
        pedacos_de_som: 0,
        erro: None,
        fim_do_som_pts: 0,
    };
    let mut ultima_conferencia = Instant::now();
    let mut ultimo_relato = Instant::now();
    let motivo: String;

    loop {
        if let Some(m) = comum.parar.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            motivo = m;
            break;
        }
        if let Some(e) = &g.erro {
            motivo = e.clone();
            break;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(e) => g.evento(e),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                motivo = idioma::t("a gravação ficou sem entrada").into();
                break;
            }
        }
        {
            let mut e = comum.estado.lock().unwrap_or_else(|x| x.into_inner());
            e.com_som = g.ultimo_som.is_some_and(|t| t.elapsed() < Duration::from_secs(1));
            e.quadros = g.quadros_escritos;
        }
        // **O primeiro quadro tem prazo** (a revisão do código, M5): sem ele a gravação ficava em
        // "abrindo" para sempre, e o pedido do controle sem resposta.
        // Contado da porta (antes contava a abertura do escritor, que chegou a levar 4 s).
        if g.linha.zero_us().is_none() && t_pronto.elapsed() >= PRAZO_DO_PRIMEIRO_QUADRO {
            motivo = idioma::tf("a câmera não entregou imagem ao arquivo em {} s", &[&PRAZO_DO_PRIMEIRO_QUADRO.as_secs()]);
            break;
        }
        // O arquivo parado (M12): nenhum quadro chegou ao escritor por 3 s **e** a reserva está
        // esgotada — o codificador não devolve as texturas. A câmera parada não é isto: a pausa não
        // para a gravação, e o som segue entrando (abaixo).
        if g.ultimo_quadro.elapsed() >= ARQUIVO_PARADO && g.linha.zero_us().is_some() && reserva.livre_agora().is_none() {
            motivo = idioma::t("o arquivo parou de receber imagem (o codificador não devolve as texturas)").into();
            break;
        }
        // **A câmera parada não segura o som** (a revisão do código, M4): sem quadro há mais de
        // 500 ms, o som vai ao escritor até "agora − 350 ms"; o quadro na mão cobre esse trecho quando
        // o próximo chegar (ou no fim).
        g.soltar_som_na_pausa();
        if ultima_conferencia.elapsed() >= CONFERIR_ESPACO {
            ultima_conferencia = Instant::now();
            if let Some(l) = espaco_livre(pasta) {
                comum.estado.lock().unwrap_or_else(|x| x.into_inner()).livre = Some(l);
                if l < regras::ESPACO_PARA_SEGUIR {
                    motivo = regras::texto_sem_espaco(l, regras::ESPACO_PARA_SEGUIR);
                    break;
                }
            }
        }
        if ultimo_relato.elapsed() >= Duration::from_secs(10) {
            ultimo_relato = Instant::now();
            let mut st = MF_SINK_WRITER_STATISTICS { cb: std::mem::size_of::<MF_SINK_WRITER_STATISTICS>() as u32, ..Default::default() };
            let _ = unsafe { escritor.w.GetStatistics(escritor.video, &mut st) };
            registro::linha(format!(
                "r5 gravação #{numero}: tempos do escritor — vídeo: {} | som: {} | na mão {:.3} s{}",
                tempos_do_escritor(&escritor.w, escritor.video),
                tempos_do_escritor(&escritor.w, escritor.som),
                g.linha.pts_na_mao().unwrap_or(0) as f64 / 1e7,
                if g.linha.em_grade_de().is_some() { format!(", grade (na mesma vaga {})", g.linha.na_mesma_vaga) } else { String::new() }
            ));
            registro::linha(format!(
                "r5 gravação #{numero}: {:.1} s escritos={} repetidos={} recusados_por_carimbo={} maior_buraco={} ms | escritor: recebidos={} codificados={} processados={} | {} | pedacos_de_som={}",
                g.linha.pts_na_mao().unwrap_or(0) as f64 / 1e7,
                g.quadros_escritos,
                g.repetidos,
                g.linha.recusados_por_carimbo,
                g.linha.maior_buraco_100ns / 10_000,
                st.qwNumSamplesReceived,
                st.qwNumSamplesEncoded,
                st.qwNumSamplesProcessed,
                g.regua.as_ref().map(|r| r.linha()).unwrap_or_else(|| "som: (antes do primeiro quadro)".into()),
                g.pedacos_de_som,
            ));
        }
    }

    // **O que está na fila entra** (a revisão do código): os quadros e o som que a entrada já tinha
    // mandado quando o parar chegou. Com o escritor em erro, não.
    if g.erro.is_none() {
        while let Ok(e) = rx.try_recv() {
            g.evento(e);
        }
    }
    // O fim: o quadro na mão sai (cobrindo o som já entregue), o som completa até o fim dele, e o
    // arquivo fecha.
    comum.fase(FaseDaGravacao::Fechando);
    let segundos = g.terminar();
    let quadros_escritos = g.quadros_escritos;
    let repetidos = g.repetidos;
    let linha_do_som = g.regua.as_ref().map(|r| r.linha()).unwrap_or_default();
    let maior_buraco_ms = g.linha.maior_buraco_100ns / 10_000;
    registro::linha(format!(
        "r5 gravação #{numero}: antes do Finalize — vídeo: {} | som: {}",
        tempos_do_escritor(&escritor.w, escritor.video),
        tempos_do_escritor(&escritor.w, escritor.som)
    ));
    registro::linha(format!(
        "r5 gravação #{numero}: os primeiros {} carimbos da câmera (ms desde o primeiro): {}",
        g.primeiros_carimbos.len(),
        g.primeiros_carimbos.iter().map(|c| format!("{:.2}", *c as f64 / 1000.0)).collect::<Vec<_>>().join(" ")
    ));
    registro::linha(format!(
        "r5 gravação #{numero}: os primeiros {} SetSampleTime/SetSampleDuration do vídeo (ms): {}",
        g.primeiros_tempos.len(),
        tempos_em_ms(&g.primeiros_tempos)
    ));
    let ultimos: Vec<(i64, i64)> = g.ultimos_tempos.iter().copied().collect();
    registro::linha(format!("r5 gravação #{numero}: os últimos {} SetSampleTime/SetSampleDuration do vídeo (ms): {}", ultimos.len(), tempos_em_ms(&ultimos)));
    drop(g);
    let tf = Instant::now();
    let fechou = unsafe { escritor.w.Finalize() };
    drop(escritor);
    let (arquivo_final, inteira) = match (&fechou, quadros_escritos) {
        (_, 0) => {
            let _ = std::fs::remove_file(arquivo);
            (None, false)
        }
        (Ok(()), _) => {
            let base = arquivo.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(regras::SUFIXO_GRAVANDO)).unwrap_or("Quall").to_string();
            let existe = |n: &str| pasta.join(n).exists();
            let nome = regras::nome_final(&base, &existe);
            let destino = pasta.join(&nome);
            match std::fs::rename(arquivo, &destino) {
                Ok(()) => (Some(destino), true),
                Err(e) => {
                    registro::linha(format!("r5 gravação #{numero}: !! o arquivo fechou mas não foi renomeado ({e})"));
                    (Some(arquivo.to_path_buf()), true)
                }
            }
        }
        (Err(e), _) => {
            registro::linha(format!("r5 gravação #{numero}: !! o Finalize falhou ({e}); o arquivo fica como órfão e é recuperado na próxima abertura"));
            (Some(arquivo.to_path_buf()), false)
        }
    };
    registro::linha(format!(
        "r5 gravação #{numero}: fechada ({motivo}) em {} ms — {} {:.1} s, {quadros_escritos} quadros (+{repetidos} repetidos nos buracos), maior buraco {maior_buraco_ms} ms | {linha_do_som} | devoluções={}",
        tf.elapsed().as_millis(),
        arquivo_final.as_ref().map(|a| a.display().to_string()).unwrap_or_else(|| "(sem arquivo)".into()),
        segundos,
        reserva.devolucoes.load(Ordering::Relaxed)
    ));
    comum.fase(FaseDaGravacao::Fechada { arquivo: arquivo_final, segundos, motivo, inteira });
}

/// O prazo do primeiro quadro no arquivo (M5).
const PRAZO_DO_PRIMEIRO_QUADRO: Duration = Duration::from_secs(5);
/// Sem quadro há isto, o som vai ao escritor sem esperar o vídeo (M4).
const PAUSA_PARA_O_SOM: Duration = Duration::from_millis(500);
/// Na pausa, o som vai até "agora" menos isto (o que ainda está chegando do microfone).
const FOLGA_DO_SOM_NA_PAUSA_US: u64 = 350_000;

/// O zero da régua é o do vídeo: o PTS (100 ns, do zero do arquivo) vira µs no relógio do dono.
fn r_limite(r: &ReguaDoSom, pts_100ns: i64) -> u64 {
    r.zero_us() + (pts_100ns.max(0) as u64) / 10
}

/// O estado da thread do gravador entre os eventos.
struct Gravacao<'a> {
    escritor: &'a Escritor,
    reserva: &'a Arc<Reserva>,
    comum: &'a Arc<Comum>,
    numero: u64,
    t0: Instant,
    origem: Instant,
    /// A porta (µs no relógio do dono): nada de antes entra, nem o que já estivesse na fila.
    aberta_em_us: u64,
    /// Um quadro no fps nominal, em 100 ns (o tamanho dos pedaços de um buraco).
    passo: i64,
    /// Amostras a mais do mesmo quadro, nos buracos.
    repetidos: u64,
    linha: LinhaDoVideo<usize>,
    /// Os primeiros e os últimos `SetSampleTime`/`SetSampleDuration` do vídeo (100 ns).
    primeiros_tempos: Vec<(i64, i64)>,
    ultimos_tempos: std::collections::VecDeque<(i64, i64)>,
    /// Os primeiros carimbos que chegaram, em µs desde o primeiro.
    primeiros_carimbos: Vec<u64>,
    regua: Option<ReguaDoSom>,
    som_antes: Vec<QuadroDePcm>,
    ultimo_quadro: Instant,
    ultimo_som: Option<Instant>,
    quadros_escritos: u64,
    pedacos_de_som: u64,
    erro: Option<String>,
    /// O fim do som já entregue ao escritor, em 100 ns do zero do arquivo.
    fim_do_som_pts: i64,
}

impl Gravacao<'_> {
    fn evento(&mut self, e: Evento) {
        match e {
            Evento::Quadro { posicao, carimbo_us } => {
                if !regras::porta_deixa(self.aberta_em_us, carimbo_us) {
                    self.reserva.devolver_sem_uso(posicao);
                    return;
                }
                self.ultimo_quadro = Instant::now();
                let primeiro = self.linha.zero_us().is_none();
                if self.primeiros_carimbos.len() < TEMPOS_NO_DIARIO {
                    let zero = self.linha.zero_us().unwrap_or(carimbo_us);
                    self.primeiros_carimbos.push(carimbo_us.saturating_sub(zero));
                }
                match self.linha.chegou(carimbo_us, posicao) {
                    Ok(saiu) => {
                        if primeiro {
                            let mut r = ReguaDoSom::nova(carimbo_us);
                            for s in self.som_antes.drain(..) {
                                r.som(s.carimbo_us, &s.amostras);
                            }
                            self.regua = Some(r);
                        }
                        if let Some(q) = saiu {
                            let fim = q.pts_100ns + q.duracao_100ns;
                            self.escrever_video(q);
                            // A comporta: o som só até o fim do quadro que acabou de ser escrito.
                            let pedaco = self.regua.as_mut().and_then(|r| {
                                let limite = r_limite(r, fim);
                                r.liberar(limite)
                            });
                            if let Some(p) = pedaco {
                                self.escrever_som(&p);
                            }
                        }
                    }
                    // Recusado antes de o Media Foundation ver a textura: volta sem quarentena.
                    Err(p) => self.reserva.devolver_sem_uso(p),
                }
            }
            Evento::Som(s) => {
                if !regras::porta_deixa(self.aberta_em_us, s.carimbo_us) {
                    return;
                }
                self.ultimo_som = Some(Instant::now());
                match self.regua.as_mut() {
                    Some(r) => r.som(s.carimbo_us, &s.amostras),
                    None => {
                        if self.som_antes.len() >= SOM_ANTES_DO_PRIMEIRO {
                            self.som_antes.remove(0);
                        }
                        self.som_antes.push(s);
                    }
                }
            }
        }
    }

    /// Um quadro ao escritor. **A textura volta pelo alocador** quando a amostra existiu (mesmo que o
    /// `WriteSample` recuse: a amostra solta e devolve); à mão, só se a amostra nem nasceu (a revisão
    /// do código: antes devolvia em dobro).
    fn escrever_video(&mut self, q: regras::QuadroNoTempo<usize>) {
        // **O buraco vira o mesmo quadro repetido** (`regras::repartir`, a bancada de 24/09): cada
        // pedaço é uma amostra da mesma textura; a mão do escritor segura um uso até o fim do laço, e
        // a última amostra solta devolve a textura.
        let i = q.carga;
        let pedacos = match self.linha.em_grade_de() {
            Some(fps) => regras::fatiar_na_grade(q.pts_100ns, q.duracao_100ns, fps),
            None => regras::repartir(q.pts_100ns, q.duracao_100ns, self.passo),
        };
        self.reserva.usos[i].store(1, Ordering::SeqCst);
        let mut vista = false;
        for (n, (pts, duracao)) in pedacos.iter().copied().enumerate() {
            self.reserva.usos[i].fetch_add(1, Ordering::SeqCst);
            let amostra = match amostra_de_video(self.reserva, i, pts, duracao) {
                Ok(s) => s,
                Err(e) => {
                    self.reserva.usos[i].fetch_sub(1, Ordering::SeqCst);
                    if self.erro.is_none() {
                        self.erro = Some(format!("a amostra do quadro não nasceu: {e}"));
                    }
                    break;
                }
            };
            vista = true;
            let escrito = unsafe { self.escritor.w.WriteSample(self.escritor.video, &amostra) };
            drop(amostra);
            if escrito.is_ok() {
                if self.primeiros_tempos.len() < TEMPOS_NO_DIARIO {
                    self.primeiros_tempos.push((pts, duracao));
                }
                if self.ultimos_tempos.len() == TEMPOS_NO_DIARIO {
                    self.ultimos_tempos.pop_front();
                }
                self.ultimos_tempos.push_back((pts, duracao));
            }
            match escrito {
                Ok(()) if n == 0 => self.primeiro_escrito(),
                Ok(()) => self.repetidos += 1,
                Err(e) => {
                    if self.erro.is_none() {
                        self.erro = Some(format!("o escritor recusou um quadro: {e}"));
                    }
                    break;
                }
            }
        }
        self.reserva.soltar_uso(i, vista);
    }

    /// Um quadro novo (não repetido) entrou no escritor. **"Gravando" no primeiro** (a revisão do
    /// código: antes saía na chegada, antes de qualquer `WriteSample`).
    fn primeiro_escrito(&mut self) {
        self.quadros_escritos += 1;
        if self.quadros_escritos != 1 {
            return;
        }
        {
            let mut e = self.comum.estado.lock().unwrap_or_else(|x| x.into_inner());
            e.fase = FaseDaGravacao::Gravando;
            e.desde = Some(Instant::now());
        }
        (self.comum.acordar)();
        registro::linha(format!(
            "r5 gravação #{}: gravando — o primeiro quadro está no arquivo ({} ms depois de pedida)",
            self.numero,
            self.t0.elapsed().as_millis()
        ));
    }

    fn escrever_som(&mut self, p: &regras::PedacoDeSom) {
        match amostra_de_som(p).and_then(|s| unsafe { self.escritor.w.WriteSample(self.escritor.som, &s) }) {
            Ok(()) => {
                self.pedacos_de_som += 1;
                self.fim_do_som_pts = self.fim_do_som_pts.max(p.pts_100ns() + p.duracao_100ns());
            }
            Err(e) => {
                if self.erro.is_none() {
                    self.erro = Some(format!("o escritor recusou o som: {e}"));
                }
            }
        }
    }

    /// M4: a câmera parada há mais de 500 ms não segura o som na régua.
    fn soltar_som_na_pausa(&mut self) {
        if self.linha.zero_us().is_none() || self.ultimo_quadro.elapsed() < PAUSA_PARA_O_SOM {
            return;
        }
        let agora_us = self.origem.elapsed().as_micros() as u64;
        let pedaco = self.regua.as_mut().and_then(|r| {
            let limite = agora_us.saturating_sub(FOLGA_DO_SOM_NA_PAUSA_US);
            if limite <= r.zero_us() {
                None
            } else {
                r.liberar(limite)
            }
        });
        if let Some(p) = pedaco {
            self.escrever_som(&p);
        }
    }

    /// O fim: o quadro na mão sai com a duração que cobre o som já entregue, e o som completa até o
    /// fim dele. Devolve a duração do arquivo, em segundos.
    fn terminar(&mut self) -> f64 {
        let Some(q) = self.linha.terminar_ate(self.fim_do_som_pts) else { return 0.0 };
        let fim = q.pts_100ns + q.duracao_100ns;
        self.escrever_video(q);
        let pedaco = self.regua.as_mut().and_then(|r| {
            let limite = r_limite(r, fim);
            r.terminar(limite)
        });
        if let Some(p) = pedaco {
            self.escrever_som(&p);
        }
        fim as f64 / 1e7
    }
}

// =============================================================================================
// Os pendentes
// =============================================================================================

/// **Os órfãos na volta** (§5.4): todo `*.gravando.mp4` da pasta que ninguém segura tem a caixa
/// incompleta do fim cortada e vira `… (interrompido).mp4`. Devolve as linhas para o registro.
pub fn recuperar_pendentes(pasta: &Path) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut linhas = Vec::new();
    let Ok(lista) = std::fs::read_dir(pasta) else { return linhas };
    for entrada in lista.flatten() {
        let caminho = entrada.path();
        let Some(nome) = caminho.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()) else { continue };
        if !nome.ends_with(regras::SUFIXO_GRAVANDO) {
            continue;
        }
        // Ninguém segura? Abrir para escrita sem compartilhar falha se outro processo grava nele.
        let arquivo = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&caminho)
        };
        let mut f = match arquivo {
            Ok(f) => f,
            Err(e) => {
                linhas.push(format!("pendente {nome}: em uso ou sem acesso ({e}); fica para depois"));
                continue;
            }
        };
        let tamanho = f.metadata().map(|m| m.len()).unwrap_or(0);
        let mut ler = |pos: u64| -> Vec<u8> {
            let mut b = vec![0u8; 16];
            if f.seek(SeekFrom::Start(pos)).is_err() {
                return Vec::new();
            }
            let mut lidos = 0;
            while lidos < 16 {
                match f.read(&mut b[lidos..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => lidos += n,
                }
            }
            b.truncate(lidos);
            b
        };
        let corte = regras::corte_do_orfao(tamanho, &mut ler);
        let existe = |n: &str| pasta.join(n).exists();
        match corte {
            Some(c) => {
                if c < tamanho {
                    let _ = f.set_len(c);
                }
                drop(f);
                let novo = regras::nome_do_interrompido(&nome, &existe).unwrap_or_else(|| format!("{nome}.interrompido.mp4"));
                match std::fs::rename(&caminho, pasta.join(&novo)) {
                    Ok(()) => linhas.push(format!("pendente {nome}: {} de {} bytes guardados, agora \"{novo}\"", c, tamanho)),
                    Err(e) => linhas.push(format!("pendente {nome}: cortado, mas não renomeado ({e})")),
                }
            }
            None => {
                drop(f);
                let novo = nome.replace(regras::SUFIXO_GRAVANDO, " (ilegível).mp4");
                let _ = std::fs::rename(&caminho, pasta.join(&novo));
                linhas.push(format!("pendente {nome}: ilegível ({tamanho} bytes, nenhuma caixa inteira); guardado como \"{novo}\""));
            }
        }
    }
    linhas
}
