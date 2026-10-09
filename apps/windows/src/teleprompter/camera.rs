//! **A sessão de vídeo da tela R5** (`docs/teleprompter-com-camera.md` §3 e §8.10, peça 6): a câmera
//! do dono e o microfone, para **um** receptor por vez, independente da sessão do prompter (a 7979).
//!
//! - Hospeda na **7877** (a regra de firewall "Quall sinalização (TCP 7877)" da bancada; ocupada, uma
//!   porta livre), com o **mesmo PIN** pela vida da tela, e as tracks `Camera` e `Microphone` — a de
//!   microfone **sempre** na oferta, calada com o botão desligado (§4.2: não há renegociação).
//! - Pareou: a rede **se pendura** no dono (`DonoDaCaptura::pendurar_rede`), o ramal do microfone
//!   vira a `CadeiaDeAudio` da sessão, e o laço é o de sempre (`sessao_de_emissao::Laco`).
//! - Caiu: a rede **se solta** (a câmera, a prévia e a gravação não piscam), a cadeia desliga os
//!   encoders dela (`desligar_tudo`, a revisão do plano, B3), e a espera volta com o mesmo PIN.
//! - A câmera que acabou não hospeda de novo: a tela diz, e reabre a câmera se a pessoa pedir.
//! - Um teto de falhas seguidas (40, como no iOS): a espera sem ninguém não conta; a sessão que cai
//!   em menos de 30 s conta.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quall_core::cancel::Cancelamento;
use quall_core::discovery::{anuncio, Advertiser};
use quall_core::error::Error;
use quall_core::pairing::{PairedPeers, Pin};
use quall_core::protocol::Capabilities;
use quall_core::session::{hospedar, SessionConfig};
use quall_core::signaling::SignalingServer;
use quall_core::track::{TrackConfig, TrackKind};
use quall_core::transport::TransportConfig;

use crate::dono_da_captura::{DonoDaCaptura, FaseDoDono};
use crate::microfone::Microfone;
use crate::sessao_de_emissao::{ContextoDoLaco, Laco, VigiaDoMonitor};
use crate::transmissao::{Cadeia, OpcoesDaCadeia, OrigemDaCadeia};
use crate::{enderecos, identidade, registro};

/// A porta do vídeo da tela R5 (a do espelhamento da bancada, com a regra de firewall).
pub const PORTA_DO_VIDEO: u16 = 7877;
/// Falhas seguidas que fazem a espera desistir (o iOS, §8.3).
pub const TETO_DE_FALHAS: u32 = 40;

/// Em que pé o vídeo está.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaseDoVideo {
    /// A câmera ainda não abriu.
    Preparando,
    Esperando,
    Transmitindo,
    /// Não espera mais: a frase para a tela.
    Parada(String),
}

/// O que a tela lê da sessão de vídeo.
#[derive(Clone, Debug)]
pub struct PainelDoVideo {
    pub fase: FaseDoVideo,
    pub pin: String,
    pub endereco: Option<String>,
    pub porta: u16,
    pub par: String,
    pub resumo: String,
    /// O último aviso ("quem recebia saiu; esperando de novo").
    pub aviso: String,
    pub anunciando: bool,
    pub sessoes: u32,
    /// O pareamento falhou por um par esquecido (a dívida 22): a tela oferece "Esquecer pareamentos".
    pub oferece_desparear: bool,
    pub versao: u64,
}

/// Como a sessão de vídeo sobe.
#[derive(Clone, Debug)]
pub struct ConfigDoVideo {
    /// `0` é [`PORTA_DO_VIDEO`].
    pub porta: u16,
    /// O PIN fixo: o da bancada (`--pin-da-camera`), ou o herdado de uma troca de câmera.
    pub pin: Option<String>,
    /// O PIN veio da bancada. Isso nunca autoriza escrevê-lo em diagnóstico.
    pub pin_de_bancada: bool,
    pub anunciar: bool,
    /// Bancada: só por 127.0.0.1.
    pub so_local: bool,
    pub fps: u32,
}

pub struct SessaoDeVideo {
    painel: Mutex<PainelDoVideo>,
    parar: AtomicBool,
    cancelamento: Mutex<Option<Cancelamento>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    acordar: Box<dyn Fn() + Send + Sync>,
}

impl SessaoDeVideo {
    pub fn iniciar(dono: Arc<DonoDaCaptura>, microfone: Arc<Microfone>, cfg: ConfigDoVideo, acordar: Box<dyn Fn() + Send + Sync>) -> Arc<SessaoDeVideo> {
        let pin = match cfg.pin.as_deref().map(Pin::parse) {
            Some(Ok(p)) => Ok(p),
            _ => Pin::generate(),
        };
        let s = Arc::new(SessaoDeVideo {
            painel: Mutex::new(PainelDoVideo {
                fase: match &pin {
                    Ok(_) => FaseDoVideo::Preparando,
                    Err(e) => FaseDoVideo::Parada(crate::idioma::tf("Não consegui preparar o PIN do vídeo: {}", &[e])),
                },
                pin: pin.as_ref().map(|p| p.to_display()).unwrap_or_default(),
                endereco: None,
                porta: 0,
                par: String::new(),
                resumo: String::new(),
                aviso: String::new(),
                anunciando: false,
                sessoes: 0,
                oferece_desparear: false,
                versao: 1,
            }),
            parar: AtomicBool::new(false),
            cancelamento: Mutex::new(None),
            thread: Mutex::new(None),
            acordar,
        });
        let Ok(pin) = pin else { return s };
        let eu = Arc::clone(&s);
        let h = std::thread::Builder::new().name("quall.r5.video".into()).spawn(move || {
            registro::prefixar_esta_thread("[r5 vídeo] ");
            eu.correr(dono, microfone, cfg, pin);
        });
        if let Ok(h) = h {
            *s.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
        }
        s
    }

    pub fn painel(&self) -> PainelDoVideo {
        self.painel.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn mudar(&self, f: impl FnOnce(&mut PainelDoVideo)) {
        {
            let mut p = self.painel.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut p);
            p.versao += 1;
        }
        (self.acordar)();
    }

    /// Pede o fim: a espera destrava, a sessão no ar para pelo caminho do Parar.
    pub fn pedir_parada(&self) {
        self.parar.store(true, Ordering::SeqCst);
        if let Some(c) = self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.cancelar();
        }
    }

    pub fn terminou(&self) -> bool {
        self.thread.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }

    fn correr(&self, dono: Arc<DonoDaCaptura>, microfone: Arc<Microfone>, cfg: ConfigDoVideo, pin: Pin) {
        let mut pin = pin;
        let device_id = identidade::device_id();
        let nome = identidade::nome_do_aparelho();
        let mut falhas = 0u32;
        let mut disse_esperando_a_camera = false;
        while !self.parar.load(Ordering::SeqCst) {
            // A câmera primeiro: sem ela não há o que oferecer.
            match dono.fase() {
                FaseDoDono::Abrindo => {
                    if !disse_esperando_a_camera {
                        disse_esperando_a_camera = true;
                        self.mudar(|p| p.fase = FaseDoVideo::Preparando);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                FaseDoDono::Aberto => {}
                FaseDoDono::Falhou(f) | FaseDoDono::Acabou(f) => {
                    registro::linha(format!("a câmera não está aberta ({f}): a espera do vídeo para"));
                    self.mudar(|p| p.fase = FaseDoVideo::Parada(f.clone()));
                    break;
                }
                FaseDoDono::Fechado => break,
            }
            if falhas >= TETO_DE_FALHAS {
                let f = crate::idioma::tf("O vídeo desistiu depois de {} tentativas seguidas que não firmaram.", &[&TETO_DE_FALHAS]);
                registro::linha(format!("!! {f}"));
                self.mudar(|p| p.fase = FaseDoVideo::Parada(f.clone()));
                break;
            }
            let porta = if cfg.porta == 0 { PORTA_DO_VIDEO } else { cfg.porta };
            let abrir = |p: u16| {
                if cfg.so_local {
                    SignalingServer::bind_em(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), p)
                } else {
                    SignalingServer::bind(p)
                }
            };
            let servidor = match abrir(porta).or_else(|e| {
                registro::linha(format!("a porta {porta} não abriu ({e}); uma livre"));
                abrir(0)
            }) {
                Ok(s) => s,
                Err(e) => {
                    falhas += 1;
                    registro::linha(format!("bind falhou: {e}"));
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            let porta_real = servidor.port().unwrap_or(porta);
            let endereco = if cfg.so_local { Some(format!("127.0.0.1:{porta_real}")) } else { enderecos::para_digitar(porta_real) };
            let eu_anuncio = anuncio(&device_id, &nome, Capabilities { screen_source: false, camera_source: true, sink: false });
            let anunciante = if cfg.anunciar && !cfg.so_local { Advertiser::start(&eu_anuncio, porta_real).ok() } else { None };
            let camera = dono.nome.clone();
            let rotulo = format!("{camera} de {nome}"); // i18n: fora (o nome da trilha vai no protocolo)
            let tracks = vec![
                TrackConfig::new(TrackKind::Camera, rotulo.clone()),
                // **Sempre na oferta** (§4.2): o botão pode ligar no meio, e não há renegociação.
                TrackConfig::new(TrackKind::Microphone, format!("Microfone de {nome}")), // i18n: fora (protocolo)
            ];
            self.mudar(|p| {
                p.fase = FaseDoVideo::Esperando;
                p.endereco = endereco.clone();
                p.porta = porta_real;
                p.anunciando = anunciante.is_some();
                p.par.clear();
                p.resumo.clear();
            });
            registro::linha(format!(
                "esperando o receptor: porta={porta_real} endereco_disponivel={} mdns={} tracks=Camera + Microphone (sempre na oferta)",
                endereco.is_some(),
                anunciante.is_some(),
            ));
            let cancelamento = Cancelamento::novo();
            *self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancelamento.clone());
            if self.parar.load(Ordering::SeqCst) {
                break;
            }
            let resultado = hospedar(
                &servidor,
                SessionConfig {
                    announcement: eu_anuncio,
                    pin: Some(pin.clone()),
                    known: identidade::pares_conhecidos(),
                    transport: if cfg.so_local {
                        TransportConfig { bind_address: Some("127.0.0.1".into()), ..TransportConfig::default() }
                    } else {
                        TransportConfig::default()
                    },
                    tracks,
                    timeout: Duration::from_secs(10 * 60),
                    silencio_do_caminho: None,
                    cancelamento,
                },
            );
            if let Some(a) = anunciante {
                let _ = a.stop();
            }
            let mut pronto = match resultado {
                Ok(p) => p,
                Err(Error::Cancelled) => break,
                Err(Error::Timeout(_)) => {
                    // Ninguém veio em 10 min: de graça, e a espera recomeça (a porta sai e volta).
                    drop(servidor);
                    continue;
                }
                // **O PIN que não conferiu troca** (a promessa do núcleo, `pairing.rs`: PIN novo a cada
                // erro; a revisão do código da câmera comum, 1): quem erra não tenta de novo no mesmo.
                // O par esquecido (a dívida 22) é o mesmo erro: a tela oferece "Esquecer pareamentos".
                Err(e @ (Error::Pairing(_) | Error::NeedsPin(_))) => {
                    falhas += 1;
                    let novo = Pin::generate();
                    let texto = match &e {
                        Error::NeedsPin(_) => crate::idioma::t("Um aparelho tentou entrar com um pareamento que este computador não reconhece mais: peça para ele digitar o PIN novo, ou esqueça os pareamentos.").to_string(),
                        _ => crate::idioma::t("O pareamento não fechou (o PIN não conferiu, ou um pareamento esquecido): o PIN mudou; digite o novo no outro aparelho.").to_string(),
                    };
                    registro::linha(format!("hospedar: o pareamento falhou ({falhas}ª seguida): status={} — o PIN do vídeo muda", crate::diagnostico_rede::status(&e)));
                    if let Ok(n) = novo {
                        pin = n;
                    }
                    let mostrado = pin.to_display();
                    self.mudar(|p| {
                        p.aviso = texto;
                        p.pin = mostrado;
                        p.oferece_desparear = true;
                    });
                    drop(servidor);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                Err(e) => {
                    falhas += 1;
                    registro::linha(format!("hospedar falhou ({falhas}ª seguida): status={}", crate::diagnostico_rede::status(&e)));
                    self.mudar(|p| p.aviso = crate::idioma::tf("A conexão não fechou: {}", &[&e]));
                    drop(servidor);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let mut novos = PairedPeers::new();
            novos.insert(&pronto.outcome);
            identidade::guardar_pares(&novos);
            let par = pronto.peer.display_name.clone();
            let comeco = Instant::now();
            registro::linha("conectado: a transmissão se pendura no dono");
            let iv = pronto.tracks.iter().position(|t| t.kind() == TrackKind::Camera).unwrap_or(0);
            let ia = pronto.tracks.iter().position(|t| t.kind() == TrackKind::Microphone);
            let Some(leitor) = dono.pendurar_rede() else {
                registro::linha("!! a câmera do dono não está aberta; a sessão fecha");
                pronto.link.close("a câmera não está aberta"); // i18n: fora (protocolo)
                drop(pronto);
                falhas += 1;
                continue;
            };
            let aberta = Cadeia::abrir_com(
                OrigemDaCadeia::DoDono(leitor),
                OpcoesDaCadeia {
                    fps: cfg.fps,
                    origem_do_relogio: dono.origem,
                    idr_por_flush: false,
                    idr_por_recriacao: true,
                    taxa_de_entrega: None,
                    piso_entre_recriacoes_ms: 0,
                    bitrate_alvo: None,
                    troca_a_quente: true,
                    caixa_unica: false,
                    preferencia: crate::encoder::Preferencia::Intel,
                    padroes: None,
                },
            );
            let mut cadeia = match aberta {
                Ok(c) => c,
                Err(e) => {
                    registro::linha(format!("!! a cadeia da rede não abriu: {e}"));
                    self.mudar(|p| p.aviso = crate::idioma::tf("A transmissão não começou: {}", &[&e.message()]));
                    pronto.link.close("a cadeia não abriu"); // i18n: fora (protocolo)
                    drop(pronto);
                    falhas += 1;
                    continue;
                }
            };
            registro::linha(format!(
                "sessão de pé: {}x{} encoder=\"{}\" adaptador={} microfone={} — transmissão pendurada no dono",
                cadeia.largura,
                cadeia.altura,
                cadeia.nome_do_encoder,
                cadeia.adaptador,
                if ia.is_some() { "na oferta" } else { "FORA da oferta (o receptor não aceitou?)" }
            ));
            let mut som = ia.map(|_| microfone.ramal_da_rede());
            self.mudar(|p| {
                p.fase = FaseDoVideo::Transmitindo;
                p.par = par.clone();
                p.sessoes += 1;
                p.aviso.clear();
                p.oferece_desparear = false;
            });
            cadeia.pedir_idr();
            let sempre = || true;
            let camera_do_dono = || dono.ajustes();
            let vigia = VigiaDoMonitor { id: &dono.id, nome: &dono.nome, ainda_existe: &sempre, e_camera: true, interface_confirmada: &sempre };
            let ctx = ContextoDoLaco {
                parar: &self.parar,
                espiada: Duration::from_millis(0),
                vigia: Some(vigia),
                regua: false,
                vigiar_morte: true,
                batimento: None,
                seguidor: None,
                // **R9b**: a câmera do dono (a comum e a R5), que quem recebe pode controlar.
                camera_remota: Some(&camera_do_dono),
            };
            let mut laco = Laco::novo();
            let (largura, altura) = (cadeia.largura, cadeia.altura);
            let fim = laco.correr(&mut pronto, &mut cadeia, som.as_ref(), None, iv, ia, &ctx, &mut |c, enviados, audio, parada| {
                let texto = match parada {
                    Some(ha) => crate::regras_da_camera::texto_da_camera_parada(ha),
                    None => {
                        let som = if audio > 0 { crate::idioma::tf(" · {} quadros de som", &[&audio]) } else { String::new() };
                        crate::idioma::tf("câmera {}×{} · {} quadros · {} IDR{}", &[&largura, &altura, &enviados, &c.idrs, &som])
                    }
                };
                self.mudar(|p| p.resumo = texto);
            });
            microfone.soltar_ramal_da_rede();
            if let Some(mut s) = som.take() {
                s.fechar();
            }
            laco.registrar_fim(&cadeia, &pronto, iv);
            registro::linha(cadeia.desligar_tudo(Duration::from_secs(3)));
            pronto.link.close("transmissão encerrada"); // i18n: fora (protocolo)
            drop(pronto);
            let durou = comeco.elapsed();
            registro::linha(format!(
                "transmissão solta do dono ({}); durou {:.1} s; a câmera continua{}",
                if fim.motivo.is_empty() { "Parar".to_string() } else { fim.motivo.clone() },
                durou.as_secs_f64(),
                if self.parar.load(Ordering::SeqCst) { "" } else { " — esperando de novo com o mesmo PIN" }
            ));
            if durou < Duration::from_secs(30) {
                falhas += 1;
            } else {
                falhas = 0;
            }
            // O motivo do fim é do laço de envio (outro módulo): traduzido se for uma frase da tabela.
            let aviso = if fim.motivo.is_empty() {
                crate::idioma::t("Quem recebia saiu; esperando de novo.").to_string()
            } else {
                crate::idioma::tf("{} Esperando de novo.", &[&crate::idioma::tr(&fim.motivo)])
            };
            self.mudar(|p| {
                p.aviso = aviso;
                p.par.clear();
                p.resumo.clear();
            });
        }
        *self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()) = None;
        {
            let mut p = self.painel.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(p.fase, FaseDoVideo::Parada(_)) {
                p.fase = FaseDoVideo::Parada(crate::idioma::t("O vídeo foi encerrado.").into());
            }
            p.versao += 1;
        }
        (self.acordar)();
        registro::linha("a sessão de vídeo da tela R5 acabou");
    }
}
