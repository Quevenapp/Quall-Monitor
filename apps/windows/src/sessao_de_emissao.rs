//! **Uma sessão do emissor**: o laço de transmissão, que os dois caminhos usam, e a thread de uma
//! sessão do emissor com vários receptores.
//!
//! # O laço, uma vez só
//!
//! O corpo do laço morava em `emissor.rs::correr_sessao` (o caminho de uma sessão, o produto de
//! hoje). Ele veio para cá **com as mesmas linhas de registro**, e o que dependia do emissor único
//! virou parâmetro ([`ContextoDoLaco`]): a bandeira de parada, a testemunha do monitor, o que
//! publicar na tela, se a régua de handles sai daqui. Com os valores de antes, é o laço de antes.
//!
//! # A thread de uma sessão do emissor com vários receptores
//!
//! [`correr_sessao`] é o porte de `SessaoDeEmissao.swift`, com a regra que é do Windows e de mais
//! ninguém (`emissor.rs`, cabeçalho; `docs/divida-do-nucleo.md`): **o `Ready` — sessão, link e
//! tracks — vive numa variável só, nesta thread, e nada fora dela guarda cópia.** O coordenador
//! (`varias.rs`) fala com ela por três coisas que não são handles do núcleo: o `Cancelamento`, uma
//! bandeira de parada e um canal de ordens. Ela fala com ele por um canal de avisos. Um id de track
//! morto nunca é tocado por outra thread, e o laço de captura roda aqui, pelo mesmo motivo.
//!
//! A ordem do desmonte, que é o que o requisito "o mesmo aparelho reconectando só transmite depois
//! de a sessão velha soltar o que tinha" pede: a captura e o som param, a cadeia desliga **todos**
//! os encoders (o de serviço, a reserva e a oficina, com prazo), o link fecha, o `Ready` cai, o
//! monitor é solto e a testemunha confirma — e só então sai o aviso `Desmontada`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use quall_core::cancel::Cancelamento;
use quall_core::discovery::anuncio;
use quall_core::error::Error;
use quall_core::pairing::{PairedPeers, Pin};
use quall_core::protocol::Capabilities;
use quall_core::session::{hospedar, EventoDeSessao, Ready, SessionConfig};
use quall_core::signaling::SignalingServer;
use quall_core::track::{AmostraDeAudio, QuadroCodificado, TrackConfig, TrackKind};
use quall_core::transport::TransportConfig;

use crate::argumentos::Argumentos;
use crate::idioma;
#[cfg(feature = "tela-estendida-futura")]
use crate::ativacao::{self, Reabertura};
use crate::audio::{self, CadeiaDeAudio};
use crate::capture::{self, PedidoDeCaptura};
use crate::identidade;
use crate::monitor::{ContextoDoCriar, FonteDeMonitor, MonitorDaSessao, OrigemDoMonitor, PedidoDeMonitor, Soltura};
#[cfg(feature = "tela-estendida-futura")]
use crate::monitores_virtuais::{Leitura, Onde, Vivo, PRAZO_DA_CAPTURA, TETO_DA_CAPTURA_COM_O_DONO_MEXENDO};
use crate::registro;
use crate::sessoes::{Falha, Id, Par, Saida, AVISO_DA_CAPTURA_PRESA};
use crate::transmissao::{Cadeia, Contadores, OpcoesDaCadeia, OrigemDaCadeia, PadroesDaTelaEstendida};
use windows::Win32::Graphics::Gdi::HMONITOR;

// =============================================================================================
// O laço de transmissão
// =============================================================================================

/// A testemunha de que o monitor (ou a câmera) ainda existe.
pub struct VigiaDoMonitor<'a> {
    pub id: &'a str,
    pub nome: &'a str,
    pub ainda_existe: &'a dyn Fn() -> bool,
    /// A fonte é uma câmera: `ainda_existe` é a interface habilitada
    /// (`cameras::interface_habilitada`), a testemunha da captura é o leitor (erro, evento com
    /// falha, fim do fluxo, formato que mudou), e o texto do fim fala de câmera
    /// (`docs/camera-no-windows.md` §6.2). **A câmera parada não encerra** (22/09): a tela diz, a
    /// cadeia repete o último quadro, e a sessão segue.
    pub e_camera: bool,
    /// Só da câmera: a interface já foi lida habilitada alguma vez
    /// (`regras_da_camera::TestemunhaDaInterface::ja_habilitada`)? Sem isso a testemunha da
    /// interface é cega, e a pausa encerra em `regras_da_camera::TETO_DA_PAUSA_SEM_INTERFACE`. A
    /// câmera sintética não tem nó, e responde `true`.
    pub interface_confirmada: &'a dyn Fn() -> bool,
}

impl VigiaDoMonitor<'_> {
    fn texto_do_fim(&self) -> String {
        if self.e_camera {
            idioma::tf("A câmera \"{}\" foi desconectada.", &[&self.nome])
        } else {
            idioma::tf("O monitor \"{}\" foi desconectado.", &[&self.nome])
        }
    }
}

/// O que o laço precisa saber de fora.
pub struct ContextoDoLaco<'a> {
    pub parar: &'a AtomicBool,
    /// `Argumentos::espiada_ms`.
    pub espiada: Duration,
    /// `None` na origem sintética: não há monitor que possa sumir.
    pub vigia: Option<VigiaDoMonitor<'a>>,
    /// A régua de handles a cada 15 s. É do **processo**: com várias sessões ela sai do
    /// coordenador, uma vez, e não repetida em cada sessão (revisão adversarial de 13/09/2026).
    pub regua: bool,
    /// Encerrar a sessão quando a cadeia morrer (`Cadeia::motivo_de_morte`). O caminho de uma
    /// sessão só não encerra — é o comportamento de hoje, registrado como defeito.
    pub vigiar_morte: bool,
    /// Batimento para o cão de guarda do coordenador: ms desde `batimento_base`, a cada volta.
    pub batimento: Option<(&'a AtomicU64, Instant)>,
    /// **O monitor virtual**: seguir o alvo (não o nome GDI), reabrir a captura quando o `HMONITOR`
    /// muda, e contar a ausência **sem interrupção** — tolerando enquanto o dono da topologia mexe.
    /// Com ele, `vigia` fica de lado. `None` em todos os outros caminhos: o laço de sempre.
    pub seguidor: Option<&'a RefCell<dyn Seguidor + 'a>>,
    /// **R9b, o controle remoto da câmera**: qual é a câmera desta sessão agora (a ponta dos ajustes
    /// da captura, pelo dono). `None` numa sessão de tela: o laço bombeia o filmador sem câmera, e o
    /// receptor fica sem controles (`sem_camera`). O laço é o leitor do canal de dados da sessão de
    /// vídeo (um leitor por sessão: aqui ninguém mais lê).
    pub camera_remota: Option<&'a dyn Fn() -> Option<crate::ajustes_da_camera::PontaDosAjustes>>,
}

/// O que o laço pergunta ao monitor virtual a cada ~200 ms (`Laco::correr`).
pub trait Seguidor {
    /// Olha o alvo e, se o `HMONITOR` mudou, reabre a captura da cadeia (sem recriar os encoders).
    fn olhar(&mut self, cadeia: &mut Cadeia) -> Seguimento;
    /// O nome do monitor, para a frase da tela.
    fn nome(&self) -> String;
}

/// A resposta do [`Seguidor`].
pub enum Seguimento {
    /// O monitor está lá e a captura está nele.
    Presente,
    /// O dono está mudando a topologia, ou uma reabertura está em curso, ou não deu para ler:
    /// **não conta** como ausência.
    Mexendo,
    /// O alvo não está na área de trabalho: conta, sem interrupção, até a tolerância.
    Ausente(String),
    /// Acabou já: a captura presa (o aviso do usuário), a cobertura que saiu, o tamanho que mudou.
    Falhou(Saida, String),
}

/// Os contadores do laço, que sobrevivem a ele para o registro final.
pub struct Laco {
    pub enviados: u64,
    pub recusados: u64,
    pub audio_enviados: u64,
    pub audio_recusados: u64,
    /// D2: pacotes de som que a captura fez e esta sessão não mandou, por não ser a dona.
    pub audio_calados: u64,
    envio_us: u64,
    n_envio: u64,
    bombeio_us: u64,
    evento_us: u64,
    voltas_do_emissor: u64,
    n_espiadas: u64,
    comeco_da_sessao: Instant,
}

/// Por que o laço acabou.
pub struct FimDoLaco {
    /// O conselho para a tela; vazio quando foi a parada pedida.
    pub motivo: String,
    pub saida: Option<Saida>,
}

impl Default for Laco {
    fn default() -> Self {
        Self::novo()
    }
}

impl Laco {
    pub fn novo() -> Self {
        Laco {
            enviados: 0,
            recusados: 0,
            audio_enviados: 0,
            audio_recusados: 0,
            audio_calados: 0,
            envio_us: 0,
            n_envio: 0,
            bombeio_us: 0,
            evento_us: 0,
            voltas_do_emissor: 0,
            n_espiadas: 0,
            comeco_da_sessao: Instant::now(),
        }
    }

    /// O laço. `publicar(contadores, enviados, audio_enviados, camera_parada)` sai uma vez por
    /// segundo; `camera_parada` é há quanto tempo a câmera está parada, quando está.
    ///
    /// `toca_som` (D2, várias sessões): o som só sai com ela ligada; desligada, a captura segue e
    /// os pacotes são contados em `audio_calados`. `None` é o caminho de uma sessão: sempre manda.
    #[allow(clippy::too_many_arguments)]
    pub fn correr(
        &mut self,
        pronto: &mut Ready,
        cadeia: &mut Cadeia,
        cadeia_de_audio: Option<&CadeiaDeAudio>,
        toca_som: Option<&AtomicBool>,
        iv: usize,
        ia: Option<usize>,
        ctx: &ContextoDoLaco<'_>,
        publicar: &mut dyn FnMut(&Contadores, u64, u64, Option<Duration>),
    ) -> FimDoLaco {
        let mut ultimo_relato = Instant::now();
        let mut ultima_conferencia_de_fonte = Instant::now();
        /// Quanto tempo esperar, depois da primeira testemunha da perda da fonte, para ver se a
        /// outra chega. Três segundos: folgado o bastante para um evento de sistema que dependa do
        /// laço de mensagens de outra thread, curto o bastante para não deixar o receptor olhando
        /// uma imagem congelada enquanto se faz ciência.
        const TOLERANCIA_DA_FONTE: Duration = Duration::from_secs(3);
        // As duas testemunhas da perda da fonte, a tolerância e o que registrar: puro, em
        // `regras_da_camera::VigiaDaFonte`. Na câmera a primeira testemunha encerra (o R4, M51).
        let mut vigia_da_fonte =
            crate::regras_da_camera::VigiaDaFonte::novo(ctx.vigia.as_ref().is_some_and(|v| v.e_camera));
        // **A câmera parada** (22/09): estado, e não fim. Uma linha por transição.
        let mut pausa = crate::regras_da_camera::PausaDaCamera::default();
        // Só do monitor virtual (`ctx.seguidor`): desde quando o alvo está fora **sem interrupção**.
        let mut ausente_desde: Option<Instant> = None;
        let mut motivo_do_fim = String::new();
        let mut saida: Option<Saida> = None;
        let mut ultima_espiada_na_sessao = Instant::now() - Duration::from_secs(1);

        // **A régua da sessão longa.** Uma linha a cada 15 s com os handles do processo ao lado
        // do número de recriações e dos quadros — o par que decide se o vazamento de +3,05
        // handles por recriação tem teto. Ver `emissor.rs` (onde ela nasceu) para a história.
        const PERIODO_DA_REGUA: Duration = Duration::from_secs(15);
        let mut ultima_regua = Instant::now();
        // **R9b**: o controle remoto da câmera, pelo canal de dados desta sessão de vídeo. Bombeado
        // sem espera a cada volta (a volta já tem o ritmo da cadeia, ~20 ms).
        let mut bomba = crate::camera_remota::BombaDoFilmador::nova(pronto.session.mensageiro());
        self.comeco_da_sessao = Instant::now();
        if ctx.regua {
            registro::linha(format!(
                "régua: t=0s handles={} recriações=0 quadros=0 (linha de base)",
                handles_do_processo()
            ));
        }

        loop {
            if ctx.parar.load(Ordering::SeqCst) {
                break;
            }
            if let Some((b, base)) = ctx.batimento {
                b.store(base.elapsed().as_millis() as u64, Ordering::Relaxed);
            }

            // O `Bye` que o receptor manda ao sair chega ao emissor (dívida 20) — isto é o que o
            // lê. Sem ele o único sinal seria `enviar_quadro` falhar, 30 s depois. A cada volta
            // desde 30/08 (`Argumentos::espiada_ms`).
            self.voltas_do_emissor += 1;
            if ctx.espiada.is_zero() || ultima_espiada_na_sessao.elapsed() >= ctx.espiada {
                ultima_espiada_na_sessao = Instant::now();
                let marca = Instant::now();
                let proximo = pronto.proximo_evento(Duration::from_millis(0));
                self.evento_us += marca.elapsed().as_micros() as u64;
                self.n_espiadas += 1;
                match proximo {
                    EventoDeSessao::Desconectou => {
                        motivo_do_fim = idioma::t("O outro aparelho saiu.").into();
                        saida = Some(Saida::Saiu);
                        registro::linha("sessão caiu: Desconectou");
                        break;
                    }
                    EventoDeSessao::Falhou => {
                        motivo_do_fim = idioma::t("A conexão caiu.").into();
                        saida = Some(Saida::Caiu);
                        registro::linha("sessão caiu: Falhou");
                        break;
                    }
                    EventoDeSessao::Nenhum => {}
                }
            }

            bomba.bombear(ctx.camera_remota);

            // O receptor entrou no meio do GOP e não monta imagem até o próximo IDR.
            if pronto.tracks[iv].pegar_pedido_de_idr() {
                cadeia.pedir_idr();
            }

            let marca = Instant::now();
            let prontos = cadeia.bombear(Duration::from_millis(20));
            self.bombeio_us += marca.elapsed().as_micros() as u64;
            for quadro in prontos {
                let marca = Instant::now();
                let resultado = pronto.tracks[iv].enviar_quadro(QuadroCodificado {
                    annexb: &quadro.bytes,
                    timestamp_us: quadro.timestamp_us,
                    idr: quadro.idr,
                });
                self.envio_us += marca.elapsed().as_micros() as u64;
                self.n_envio += 1;
                match resultado {
                    Ok(()) => self.enviados += 1,
                    // Antes de o ICE fechar isto é o estado **normal**. Contar e seguir; nunca
                    // enfileirar.
                    Err(_) => self.recusados += 1,
                }
            }

            // O áudio já chegou pronto (Opus codificado) pela fila da thread de captura. **Um
            // quadro por chamada, sempre**: o pacotizador de áudio do núcleo não fragmenta.
            if let (Some(ia), Some(ca)) = (ia, cadeia_de_audio) {
                let manda = toca_som.map_or(true, |t| t.load(Ordering::Relaxed));
                for pacote in ca.bombear() {
                    if !manda {
                        self.audio_calados += 1;
                        continue;
                    }
                    let r = pronto.tracks[ia].enviar_audio(AmostraDeAudio {
                        payload: &pacote.bytes,
                        timestamp_us: pacote.timestamp_us,
                    });
                    match r {
                        Ok(()) => self.audio_enviados += 1,
                        Err(_) => self.audio_recusados += 1,
                    }
                }
            }

            if ctx.regua && ultima_regua.elapsed() >= PERIODO_DA_REGUA {
                ultima_regua = Instant::now();
                registro::linha(format!(
                    "régua: t={}s handles={} recriações={} quadros={} enviados={}",
                    self.comeco_da_sessao.elapsed().as_secs(),
                    handles_do_processo(),
                    cadeia.recriacoes(),
                    cadeia.contadores.encodados,
                    self.enviados,
                ));
            }

            // **O monitor virtual segue o alvo.** A chegada de outro monitor apaga o nome GDI deste
            // por ~0,5 s (E5) e pode trocar o `HMONITOR`: o seguidor reabre a captura, e só a
            // ausência sem interrupção por `TOLERANCIA_DA_FONTE` encerra — nunca a primeira
            // testemunha, nunca o `Closed` do item velho (a revisão, item 1).
            if let Some(s) = ctx.seguidor {
                if ultima_conferencia_de_fonte.elapsed() >= Duration::from_millis(200) {
                    ultima_conferencia_de_fonte = Instant::now();
                    let r = s.borrow_mut().olhar(cadeia);
                    match r {
                        Seguimento::Presente | Seguimento::Mexendo => {
                            if let Some(desde) = ausente_desde.take() {
                                registro::linha(format!(
                                    "o monitor voltou depois de {} ms fora",
                                    desde.elapsed().as_millis()
                                ));
                            }
                        }
                        Seguimento::Ausente(m) => {
                            let desde = *ausente_desde.get_or_insert_with(|| {
                                registro::linha(format!("o monitor está fora da área de trabalho: {m}"));
                                Instant::now()
                            });
                            if desde.elapsed() >= TOLERANCIA_DA_FONTE {
                                motivo_do_fim = idioma::tf("O monitor \"{}\" foi desconectado.", &[&s.borrow().nome()]);
                                registro::linha(format!(
                                    "o monitor ficou fora {} ms sem interrupção ({m}) — encerrando esta sessão",
                                    desde.elapsed().as_millis()
                                ));
                                saida = Some(Saida::FonteSumiu(motivo_do_fim.clone()));
                                break;
                            }
                        }
                        Seguimento::Falhou(s_, motivo) => {
                            registro::linha(format!("o monitor virtual falhou: {motivo} — encerrando esta sessão"));
                            motivo_do_fim = motivo;
                            saida = Some(s_);
                            break;
                        }
                    }
                }
            } else if let Some(vigia) = &ctx.vigia {
                // **A fonte pode sumir no meio.** Duas testemunhas independentes: o `Closed` do
                // `GraphicsCaptureItem` e a reenumeração dos monitores. Ver `docs/app-windows.md`
                // ("O monitor desconectado no meio da transmissão") para qual das duas funcionou. Na
                // câmera, o fim do leitor e a interface (`docs/camera-no-windows.md` §6.2).
                //
                // **Uma linha por transição**, e não por volta (o R4 teve 16 iguais por sessão). No
                // monitor a primeira testemunha não encerra: a tolerância separa "não tinha
                // disparado naquele instante" de "não dispara". **Na câmera encerra**: o leitor que
                // acabou não volta, e a interface tem confirmação própria; esperar a tolerância
                // deixou a [#1] do R4 3 s transmitindo nada (M51).
                if ultima_conferencia_de_fonte.elapsed() >= Duration::from_millis(200) {
                    ultima_conferencia_de_fonte = Instant::now();
                    let item_fechou = cadeia.fonte_sumiu();
                    let sumiu_da_lista = !(vigia.ainda_existe)();
                    let volta = vigia_da_fonte.olhar(item_fechou, sumiu_da_lista, Instant::now(), TOLERANCIA_DA_FONTE);
                    let da_camera = if vigia.e_camera && (volta.registrar || volta.primeira) { cadeia.fim_da_camera() } else { None };
                    if volta.registrar {
                        registro::linha(format!(
                            "{}: {}={item_fechou} {}={sumiu_da_lista} {}={} ({}){}",
                            if item_fechou || sumiu_da_lista {
                                "fonte sumiu"
                            } else {
                                "fonte: as testemunhas voltaram atrás, e a tolerância continua contando"
                            },
                            if vigia.e_camera { "a_captura_parou" } else { "item_closed" },
                            if vigia.e_camera { "interface_desabilitada" } else { "fora_da_enumeracao" },
                            if vigia.e_camera { "camera" } else { "monitor" },
                            vigia.id,
                            vigia.nome,
                            da_camera
                                .as_ref()
                                .map(|f| format!(" — {}{}", f.motivo, if f.desconectada { " (desconectada)" } else { "" }))
                                .unwrap_or_default()
                        ));
                    }
                    if volta.primeira {
                        motivo_do_fim = if vigia.e_camera {
                            // A interface que sumiu é desconexão; sem ela, o leitor decide (o
                            // `0xC00D3EA2` é desconexão, o `0xC00D3EA3` é outro app que tomou).
                            crate::regras_da_camera::texto_do_fim_da_camera(vigia.nome, da_camera.as_ref(), sumiu_da_lista)
                        } else {
                            vigia.texto_do_fim()
                        };
                    }
                    let testemunha_do_sistema =
                        if vigia.e_camera { "o fim do leitor da câmera" } else { "GraphicsCaptureItem::Closed" };
                    if volta.sistema_primeiro && !vigia.e_camera {
                        registro::linha(format!("{testemunha_do_sistema} foi a primeira testemunha"));
                    }
                    if let Some(d) = volta.sistema_depois {
                        registro::linha(format!(
                            "{testemunha_do_sistema} disparou {} ms depois da primeira testemunha",
                            d.as_millis()
                        ));
                    }
                    if volta.encerrar {
                        if vigia.e_camera {
                            registro::linha(format!(
                                "a câmera acabou ({}): encerrando esta sessão na primeira testemunha, sem esperar a tolerância",
                                match (item_fechou, sumiu_da_lista) {
                                    (true, true) => "o fim do leitor e a interface, na mesma volta",
                                    (true, false) => "o fim do leitor",
                                    _ => "a interface sumiu, com o leitor ainda sem fim",
                                }
                            ));
                        } else if !vigia_da_fonte.sistema_visto() {
                            registro::linha(format!(
                                "{testemunha_do_sistema} NÃO disparou em {} ms — a única testemunha da perda da fonte foi \
                                 a reenumeração de monitores",
                                TOLERANCIA_DA_FONTE.as_millis()
                            ));
                        }
                        saida = Some(Saida::FonteSumiu(crate::regras_da_camera::so_a_frase(&motivo_do_fim).to_string()));
                        break;
                    }
                    // **A câmera que pausa não encerra a sessão** (a decisão do Bruno de 21/09). A
                    // Panasonic em DV parou cinco vezes em 21/09 com a interface de pé, e cada
                    // parada derrubou a gravação dele no OBS. Agora a tela diz, a cadeia repete o
                    // último quadro (os receptores de 10 s não desistem), e a sessão segue.
                    if vigia.e_camera {
                        let agora = Instant::now();
                        let confirmada = (vigia.interface_confirmada)();
                        match pausa.olhar(cadeia.camera_parada_ha(agora), agora) {
                            crate::regras_da_camera::MudancaDaPausa::Parou { ha } => registro::linha(format!(
                                "câmera parada: nenhum quadro há {} ms ({}ª pausa); a sessão segue, repetindo o último \
                                 quadro e esperando ela voltar | interface confirmada={confirmada} | {}",
                                ha.as_millis(),
                                pausa.pausas,
                                vigia.nome
                            )),
                            crate::regras_da_camera::MudancaDaPausa::Voltou { depois_de } => registro::linha(format!(
                                "câmera voltou: {} ms sem quadro ({}ª pausa) | repetidos={} repeticoes_puladas={} até aqui",
                                depois_de.as_millis(),
                                pausa.pausas,
                                cadeia.contadores.repetidos,
                                cadeia.contadores.repeticoes_puladas
                            )),
                            crate::regras_da_camera::MudancaDaPausa::Nenhuma => {}
                        }
                        if pausa.encerrar(agora, confirmada) {
                            let fim = crate::regras_da_camera::FimDaCamera::pelo_codigo(
                                format!(
                                    "nenhum quadro da câmera há {} s, e a interface dela nunca foi lida habilitada",
                                    pausa.parada_ha(agora).unwrap_or_default().as_secs()
                                ),
                                false,
                                None,
                            );
                            motivo_do_fim = crate::regras_da_camera::texto_do_fim_da_camera(vigia.nome, Some(&fim), false);
                            registro::linha(format!(
                                "a câmera ficou parada {} s sem a interface confirmada: encerrando esta sessão",
                                crate::regras_da_camera::TETO_DA_PAUSA_SEM_INTERFACE.as_secs()
                            ));
                            saida = Some(Saida::FonteSumiu(crate::regras_da_camera::so_a_frase(&motivo_do_fim).to_string()));
                            break;
                        }
                    }
                }
            }

            if ultimo_relato.elapsed() >= Duration::from_secs(1) {
                ultimo_relato = Instant::now();
                // A cadeia que morreu (encoder que não recriou, GPU que caiu) não produz mais nada:
                // com várias sessões ela sai com motivo, em vez de ficar de pé calada.
                if ctx.vigiar_morte {
                    if let Some(m) = cadeia.motivo_de_morte() {
                        registro::linha(format!("a cadeia morreu: {m} — encerrando esta sessão"));
                        motivo_do_fim = idioma::tf("A transmissão parou: {}.", &[&m]);
                        saida = Some(Saida::NaoIniciou(m));
                        break;
                    }
                }
                let c = cadeia.contadores;
                // **Contadores dos dois lados da fronteira**: os da casca e os do núcleo.
                registro::linha(format!(
                    "casca: capturados={} encodados={} enviados={} recusados={} \
                     idrs={} parametros_injetados={} latencia_media_ms={:.2}{} | \
                     nucleo: quadros={} idrs={} idrs_sem_parametros={} pedidos_de_idr={} bytes={}",
                    c.capturados,
                    c.encodados,
                    self.enviados,
                    self.recusados,
                    c.idrs,
                    c.parametros_injetados,
                    c.latencia_media_ms(),
                    if c.repetidos > 0 { format!(" repetidos={}", c.repetidos) } else { String::new() },
                    pronto.tracks[iv].quadros_enviados(),
                    pronto.tracks[iv].idrs_enviados(),
                    pronto.tracks[iv].idrs_sem_parametros(),
                    pronto.tracks[iv].pedidos_de_idr(),
                    pronto.tracks[iv].bytes_enviados(),
                ));
                registro::linha(format!("perfil do laço: {}", cadeia.perfil().linha()));
                registro::linha(cadeia.linha_dos_degraus());
                registro::linha(self.perfil_do_emissor());
                if let (Some(ia), Some(ca)) = (ia, cadeia_de_audio) {
                    registro::linha(format!(
                        "audio casca: {} | audio nucleo: quadros={} bytes={} | enviados={} \
                         recusados={}",
                        ca.contadores.linha(),
                        pronto.tracks[ia].quadros_enviados(),
                        pronto.tracks[ia].bytes_enviados(),
                        self.audio_enviados,
                        self.audio_recusados,
                    ));
                }
                publicar(&c, self.enviados, self.audio_enviados, pausa.parada_ha(Instant::now()));
            }
        }
        if ctx.vigia.as_ref().is_some_and(|v| v.e_camera) {
            registro::linha(format!("câmera parada nesta sessão: {}", pausa.resumo(Instant::now())));
        }
        // **Por que o laço acabou**, numa linha: no R4 o motivo da sessão que perdeu a câmera não
        // aparecia no registro do emissor de várias sessões (M51).
        registro::linha(format!(
            "fim do laço: {}{}",
            match &saida {
                None => "Parar",
                Some(Saida::Saiu) => "o par saiu",
                Some(Saida::Caiu) => "a conexão caiu",
                Some(Saida::FonteSumiu(_)) => "a fonte sumiu",
                Some(Saida::NaoIniciou(_)) => "a cadeia parou",
                Some(Saida::CapturaPresa) => "a captura presa",
            },
            if motivo_do_fim.is_empty() { String::new() } else { format!(" — {motivo_do_fim}") }
        ));
        FimDoLaco { motivo: motivo_do_fim, saida }
    }

    /// As linhas do fim da sessão, na ordem de antes. Chamado **depois** de a captura e o som
    /// pararem.
    pub fn registrar_fim(&self, cadeia: &Cadeia, pronto: &Ready, iv: usize) {
        let c = cadeia.contadores;
        registro::linha(format!(
            "fim da captura: capturados={} encodados={} enviados={} recusados={} \
             idrs={} parametros_injetados={} saidas_so_de_parametros={} \
             recusas_do_ritmo={} latencia_media_ms={:.2}{}",
            c.capturados,
            c.encodados,
            self.enviados,
            self.recusados,
            c.idrs,
            c.parametros_injetados,
            c.saidas_so_de_parametros,
            c.recusas_do_ritmo,
            c.latencia_media_ms(),
            if c.repetidos > 0 { format!(" repetidos={}", c.repetidos) } else { String::new() },
        ));
        // Só quando há o que dizer, ou quando a origem é câmera: a tela continua com as linhas de
        // antes, byte a byte (a revisão do código da fase 3, M1 e m6).
        if c.carimbos_empurrados > 0 || c.conversor_sem_destino > 0 || cadeia.e_camera() {
            registro::linha(format!(
                "carimbo e conversor: carimbos_empurrados_na_submissao={} conversor_sem_destino_livre={} \
                 repetidos_da_camera_parada={} repeticoes_puladas={}",
                c.carimbos_empurrados, c.conversor_sem_destino, c.repetidos, c.repeticoes_puladas
            ));
        }
        registro::linha(cadeia.linha_dos_degraus());
        registro::linha(format!("idr no fluxo: {}", cadeia.medida_de_idr()));
        registro::linha(format!("quadro-chave no fio: {}", cadeia.medida_de_quadro_chave()));
        registro::linha(format!("quinta porta: {}", cadeia.relato_de_recriacao()));
        registro::linha(format!("controle de taxa:\n{}", cadeia.medida_do_controle_de_taxa()));
        // **`--sair-apos` não serve de denominador**: ele conta do começo do processo. Esta
        // duração conta da sessão, que é a janela em que houve tráfego.
        registro::linha(format!(
            "régua (final): duracao_s={:.1} handles={} recriações={} quadros={}",
            self.comeco_da_sessao.elapsed().as_secs_f64(),
            handles_do_processo(),
            cadeia.recriacoes(),
            c.encodados,
        ));
        registro::linha(format!("perfil do laço: {}", cadeia.perfil().linha()));
        registro::linha(self.perfil_do_emissor());
        match cadeia.resumo_sps() {
            Some(r) => registro::linha(format!("sps (final): {}", r.linha())),
            None => registro::linha("sps (final): nenhum SPS foi visto nesta sessão"),
        }
        registro::linha(format!(
            "nucleo (final): quadros={} idrs={} idrs_sem_parametros={} bytes={} pacotes_entregues={}",
            pronto.tracks[iv].quadros_enviados(),
            pronto.tracks[iv].idrs_enviados(),
            pronto.tracks[iv].idrs_sem_parametros(),
            pronto.tracks[iv].bytes_enviados(),
            // O lado do emissor do par que decide a dívida 30 — ver `quall_core::track`.
            pronto.tracks[iv].pacotes_entregues(),
        ));
    }

    /// Onde o tempo do laço do **emissor** vai — a metade que não é da `Cadeia`.
    fn perfil_do_emissor(&self) -> String {
        let media = |soma: u64, n: u64| if n == 0 { 0.0 } else { soma as f64 / n as f64 / 1000.0 };
        format!(
            "perfil do emissor: voltas={} | bombear={:.2} ms/volta | enviar_quadro={:.2} ms n={} \
             (total {:.1} s) | proximo_evento={:.2} ms n={} (total {:.1} s)",
            self.voltas_do_emissor,
            media(self.bombeio_us, self.voltas_do_emissor),
            media(self.envio_us, self.n_envio),
            self.n_envio,
            self.envio_us as f64 / 1_000_000.0,
            media(self.evento_us, self.n_espiadas),
            self.n_espiadas,
            self.evento_us as f64 / 1_000_000.0,
        )
    }
}

/// Handles abertos por **este** processo, agora. Devolve 0 se a chamada falhar.
pub fn handles_do_processo() -> u32 {
    let mut n = 0u32;
    unsafe {
        let _ = windows::Win32::System::Threading::GetProcessHandleCount(
            windows::Win32::System::Threading::GetCurrentProcess(),
            &mut n,
        );
    }
    n
}

// =============================================================================================
// A thread de uma sessão do emissor com vários receptores
// =============================================================================================

/// O coordenador → a sessão.
pub enum Ordem {
    /// Pode transmitir: o monitor de índice `indice`, no formato da tela do par.
    Transmitir { indice: usize, par: Par },
    Encerrar,
}

/// A sessão → o coordenador.
pub enum DaSessao {
    Conectou(Par),
    /// A espera acabou sem par. **O servidor já foi solto** quando isto chega: a reabertura na
    /// mesma porta não disputa com ele (revisão adversarial de 13/09/2026). É o último aviso.
    FalhouAoHospedar(Falha, String),
    /// Uma sessão no ar acabou sozinha; o `Desmontada` vem depois.
    Saiu(Saida),
    /// A linha da sessão na tela, uma vez por segundo.
    Resumo(String),
    /// O som desta sessão: subiu, ou por que não.
    Som { ativo: bool, recusado: String },
    /// O último aviso de toda sessão que conectou. `monitor_solto`: a testemunha confirmou.
    Desmontada { monitor_solto: bool },
    /// O monitor virtual desta sessão ficou de pé: a chave (o GUID) com que o coordenador o solta
    /// sozinho se a sessão não confirmar, sumir sem batimento ou passar do prazo do Parar.
    MonitorDePe(u128),
}

/// O que a thread de uma sessão recebe ao nascer.
pub struct PedidoDaSessao {
    pub id: Id,
    pub servidor: SignalingServer,
    pub pin: Pin,
    pub com_audio: bool,
    /// D2: esta sessão é a dona do som. Escrito pelo coordenador (`Efeitos::dar_o_som`).
    pub toca_som: Arc<AtomicBool>,
    pub argumentos: Argumentos,
    pub monitores: Arc<dyn FonteDeMonitor>,
    pub avisos: Sender<(Id, DaSessao)>,
    pub ordens: Receiver<Ordem>,
    pub cancelamento: Cancelamento,
    pub parar: Arc<AtomicBool>,
    pub batimento: Arc<AtomicU64>,
    pub batimento_base: Instant,
    pub nome_do_aparelho: String,
    pub device_id: String,
    /// O rótulo da track ("Tela de G3BRUNO").
    pub rotulo: String,
    pub preferencia: crate::encoder::Preferencia,
    pub padroes: PadroesDaTelaEstendida,
}

/// A thread da sessão, do começo ao fim. **Todo caminho termina em exatamente um aviso final**:
/// `FalhouAoHospedar` (não conectou) ou `Desmontada` (conectou). Um pânico no meio também: o
/// coordenador não pode ficar esperando uma sessão que morreu calada.
pub fn correr_sessao(p: PedidoDaSessao) {
    registro::prefixar_esta_thread(&format!("[#{}] ", p.id));
    let id = p.id;
    let avisos = p.avisos.clone();
    let conectou = Arc::new(AtomicBool::new(false));
    let conectou_ = conectou.clone();
    let resultado = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        correr_sessao_por_dentro(p, &conectou_)
    }));
    if resultado.is_err() {
        registro::linha("!! a thread da sessão entrou em pânico — avisando o coordenador");
        let fim = if conectou.load(Ordering::SeqCst) {
            DaSessao::Desmontada { monitor_solto: false }
        } else {
            DaSessao::FalhouAoHospedar(Falha::Outra("a sessão entrou em pânico".into()), String::new())
        };
        let _ = avisos.send((id, fim));
    }
}

fn correr_sessao_por_dentro(p: PedidoDaSessao, conectou: &AtomicBool) {
    let PedidoDaSessao {
        id,
        servidor,
        pin,
        com_audio,
        toca_som,
        argumentos,
        monitores,
        avisos,
        ordens,
        cancelamento,
        parar,
        batimento,
        batimento_base,
        nome_do_aparelho,
        device_id,
        rotulo,
        preferencia,
        padroes,
    } = p;
    let avisar = |d: DaSessao| {
        let _ = avisos.send((id, d));
    };
    let bater = || batimento.store(batimento_base.elapsed().as_millis() as u64, Ordering::Relaxed);

    let eu = anuncio(
        &device_id,
        &nome_do_aparelho,
        Capabilities { screen_source: !monitores.e_camera(), camera_source: monitores.e_camera(), sink: false },
    );
    // **O som se decide agora, antes da oferta, ou não existe nesta sessão** (dívida 1). Toda
    // sessão da rodada com som oferece a track; só a dona manda (D2, `sessoes.rs`). O endpoint é
    // conferido antes, como em `emissor.rs`.
    let config_de_audio = argumentos.config_de_audio();
    // A câmera vai **sem som** até a frente 1c, como no Mac (`docs/camera-no-windows.md` §1).
    let audio_prometido = com_audio
        && !monitores.e_camera()
        && match audio::conferir(&config_de_audio.alvo) {
            Ok(descricao) => {
                registro::linha(format!("audio: endpoint conferido — {descricao}"));
                true
            }
            Err(motivo) => {
                registro::linha(format!("audio: NÃO vou declarar a track — o endpoint não abriu: {motivo}"));
                avisar(DaSessao::Som {
                    ativo: false,
                    recusado: "Este computador não deixou capturar o som da saída de áudio; a \
                               transmissão vai só com imagem."
                        .into(),
                });
                false
            }
        };
    // A espécie da track de vídeo sai da fonte: `Camera` quando a sessão transmite uma câmera
    // (`docs/camera-no-windows.md`, fase 3), `Screen` no resto.
    let especie_de_video = if monitores.e_camera() { TrackKind::Camera } else { TrackKind::Screen };
    let mut tracks = vec![TrackConfig::new(especie_de_video, rotulo.clone())];
    if audio_prometido {
        tracks.push(TrackConfig::new(TrackKind::SystemAudio, idioma::tf("Som de {}", &[&nome_do_aparelho])));
    }
    let porta = servidor.port().unwrap_or(0);
    registro::linha(format!(
        "espera aberta: porta={porta} tracks={} som={}",
        tracks.iter().map(|t| format!("{:?}", t.kind)).collect::<Vec<_>>().join("+"),
        audio_prometido,
    ));

    let resultado = hospedar(
        &servidor,
        SessionConfig {
            announcement: eu,
            pin: Some(pin),
            known: identidade::pares_conhecidos(),
            // `--so-local` (bancada): o ICE preso em 127.0.0.1, como a sinalização (`varias.rs`).
            transport: if argumentos.so_local {
                TransportConfig { bind_address: Some("127.0.0.1".into()), ..TransportConfig::default() }
            } else {
                TransportConfig::default()
            },
            tracks,
            timeout: Duration::from_secs(5 * 60),
            silencio_do_caminho: None,
            cancelamento,
        },
    );
    // **O servidor sai assim que `hospedar` volta**, com par ou sem: um receptor com a porta velha
    // na memória não fica no backlog de uma porta que ninguém mais aceita, e a reabertura de uma
    // espera que falhou acha a porta livre (revisão adversarial de 13/09/2026).
    drop(servidor);

    let mut pronto = match resultado {
        Ok(p) => p,
        Err(erro) => {
            registro::linha(format!("hospedar falhou: status={}", crate::diagnostico_rede::status(&erro)));
            avisar(DaSessao::FalhouAoHospedar(falha_de(&erro), erro.to_string()));
            return;
        }
    };
    conectou.store(true, Ordering::SeqCst);

    // O pareamento fechou: grava antes de qualquer outra coisa (serializado em `identidade.rs`:
    // duas sessões pareando juntas não se atropelam).
    let mut novos = PairedPeers::new();
    novos.insert(&pronto.outcome);
    identidade::guardar_pares(&novos);
    let par = Par {
        nome: pronto.peer.display_name.clone(),
        device_id: pronto.peer.device_id.0.clone(),
        // **A tela que o receptor disse no aperto de mão** (`Announcement::screen`). O Windows não
        // lia (`docs/tela-estendida.md`); agora ela vai até o pedido do monitor e até o registro.
        tela: pronto.peer.screen.as_ref().map(|s| (s.width_px, s.height_px)),
    };
    registro::linha(format!(
        "hospedado: tela_do_par={} candidatos_descartados={} pareamento_novo={}",
        par.tela.map(|(l, a)| format!("{l}x{a}")).unwrap_or_else(|| "não disse".into()),
        pronto.descartados,
        pronto.outcome.novo,
    ));
    avisar(DaSessao::Conectou(par));

    // **Espera a ordem do coordenador**, olhando a sessão: se o par sair enquanto a sessão velha do
    // mesmo aparelho ainda desmonta, esta sai também, sem nunca ter transmitido.
    let ordem = loop {
        bater();
        if parar.load(Ordering::SeqCst) {
            break None;
        }
        match ordens.recv_timeout(Duration::from_millis(20)) {
            Ok(Ordem::Transmitir { indice, par }) => break Some((indice, par)),
            Ok(Ordem::Encerrar) => break None,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break None,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
        match pronto.proximo_evento(Duration::from_millis(0)) {
            EventoDeSessao::Desconectou => {
                registro::linha("sessão caiu antes de transmitir: Desconectou");
                avisar(DaSessao::Saiu(Saida::Saiu));
                break None;
            }
            EventoDeSessao::Falhou => {
                registro::linha("sessão caiu antes de transmitir: Falhou");
                avisar(DaSessao::Saiu(Saida::Caiu));
                break None;
            }
            EventoDeSessao::Nenhum => {}
        }
    };
    let Some((indice, par)) = ordem else {
        pronto.link.close("sessão encerrada antes de transmitir");
        drop(pronto);
        registro::linha("sessao encerrada (sem ter transmitido)");
        avisar(DaSessao::Desmontada { monitor_solto: true });
        return;
    };

    // --- o monitor desta sessão, e a cadeia sobre ele -----------------------------------------------
    let pedido = PedidoDeMonitor::para_tela(par.tela, argumentos.fps, indice, &par.nome, &par.device_id);
    // **A track de vídeo pela espécie de vídeo**, tela ou câmera — nunca pela posição (R14).
    let iv = pronto.tracks.iter().position(|t| matches!(t.kind(), TrackKind::Screen | TrackKind::Camera)).unwrap_or(0);
    let ia = pronto.tracks.iter().position(|t| t.kind() == TrackKind::SystemAudio);
    let origem = Instant::now();
    let opcoes = OpcoesDaCadeia {
        fps: argumentos.fps,
        origem_do_relogio: origem,
        idr_por_flush: argumentos.idr_por_flush,
        idr_por_recriacao: argumentos.idr_por_recriacao(),
        taxa_de_entrega: argumentos.taxa_de_entrega,
        piso_entre_recriacoes_ms: argumentos.piso_entre_recriacoes_ms,
        bitrate_alvo: argumentos.bitrate_alvo,
        troca_a_quente: argumentos.troca_a_quente(),
        caixa_unica: argumentos.caixa_unica,
        preferencia,
        // **Câmera sem os padrões da tela estendida** (a revisão do código da fase 3, M1): a
        // repetição carimbada com "agora" faria o carimbo da câmera voltar.
        padroes: if monitores.e_camera() { None } else { Some(padroes) },
    };
    // O cursor da captura do monitor virtual: fora na bancada (sobre a nossa janela), dentro na
    // tela estendida de verdade.
    #[cfg(feature = "tela-estendida-futura")]
    let cursor = argumentos.cobrir_monitor.is_none();
    // **O monitor virtual**: a cadeia abre já, na placa do processo e com a origem preta do tamanho
    // do pedido; o monitor nasce num fio auxiliar, na fila do dono, e o seguidor troca a origem pela
    // captura quando ele (e a janela da bancada) estiverem prontos. As outras fontes: o de sempre.
    #[cfg(feature = "tela-estendida-futura")]
    let aberta = if monitores.cadeia_na_placa_do_processo() {
        abrir_no_monitor_virtual(&monitores, &pedido, opcoes, &parar, cursor, argumentos.monitor_sem_captura, avisos.clone(), id)
            .map(|(c, s)| (None, c, Some(s)))
    } else {
        abrir_como_antes(&*monitores, &pedido, &par, opcoes, &parar, &bater).map(|(m, c)| (Some(m), c, None))
    };
    #[cfg(not(feature = "tela-estendida-futura"))]
    let aberta = abrir_como_antes(&*monitores, &pedido, &par, opcoes, &parar, &bater).map(|(m,c)| (Some(m), c, Option::<()>::None));
    let (monitor_de_sempre, mut cadeia, seguidor_virtual) = match aberta {
        Ok(x) => x,
        Err(FalhaDaAbertura { saida, monitor, texto, fechar }) => {
            registro::linha(format!("!! {texto}"));
            avisar(DaSessao::Saiu(saida));
            pronto.link.close(fechar);
            drop(pronto);
            let solto = monitor.map(|m| monitores.soltar(&m, Duration::from_secs(3)).confirmou()).unwrap_or(true);
            avisar(DaSessao::Desmontada { monitor_solto: solto });
            return;
        }
    };
    registro::linha(format!(
        "captura: {}x{} encoder=\"{}\" hardware={} adaptador={}",
        cadeia.largura, cadeia.altura, cadeia.nome_do_encoder, cadeia.encoder_e_hardware, cadeia.adaptador
    ));

    // O som, só depois de conectar. **Toda sessão com a track captura** (D2): passar o som é só
    // virar `toca_som`, sem reabrir captura — o mesmo desenho do Mac.
    let mut cadeia_de_audio: Option<CadeiaDeAudio> = None;
    if let Some(ia) = ia {
        match pronto.tracks[ia].preset_de_audio() {
            Some(preset) => match CadeiaDeAudio::abrir(config_de_audio.clone(), preset, origem) {
                Ok(c) => {
                    registro::linha(format!("audio: capturando — {}", c.descricao()));
                    cadeia_de_audio = Some(c);
                    avisar(DaSessao::Som { ativo: true, recusado: String::new() });
                }
                Err(erro) => {
                    registro::linha(format!("audio: a captura NÃO subiu depois de a track ser declarada: {erro}"));
                    avisar(DaSessao::Som {
                        ativo: false,
                        recusado: "A track de som foi negociada, mas a captura não subiu: o outro \
                                   aparelho vai receber imagem sem som."
                            .into(),
                    });
                }
            },
            None => registro::linha("audio: a track de sistema veio sem preset do núcleo — não há como codificar"),
        }
    }

    // O receptor entrou no meio: pedir um IDR já, em vez de esperar o próximo do GOP.
    cadeia.pedir_idr();

    let ainda_existe = || monitor_de_sempre.as_ref().is_some_and(|m| m.ainda_existe());
    let interface_confirmada = || monitor_de_sempre.as_ref().map_or(true, |m| m.interface_confirmada());
    let vigia = match monitor_de_sempre.as_ref().map(|m| &m.origem) {
        Some(OrigemDoMonitor::Monitor(f)) => Some(VigiaDoMonitor {
            id: &f.id,
            nome: &f.nome,
            ainda_existe: &ainda_existe,
            e_camera: false,
            interface_confirmada: &interface_confirmada,
        }),
        Some(OrigemDoMonitor::Camera { fonte, .. }) => Some(VigiaDoMonitor {
            id: &fonte.id,
            nome: &fonte.nome,
            ainda_existe: &ainda_existe,
            e_camera: true,
            interface_confirmada: &interface_confirmada,
        }),
        _ => None,
    };
    // O monitor virtual segue o alvo pelo mapa do dono (e não pelo nome GDI, que muda).
    #[cfg(feature = "tela-estendida-futura")]
    let seguidor_virtual: Option<RefCell<SeguidorVirtual>> = seguidor_virtual.map(RefCell::new);
    #[cfg(feature = "tela-estendida-futura")]
    let seguidor: Option<&RefCell<dyn Seguidor + '_>> = seguidor_virtual.as_ref().map(|s| s as &RefCell<dyn Seguidor + '_>);
    #[cfg(not(feature = "tela-estendida-futura"))]
    let seguidor = None;
    let ctx = ContextoDoLaco {
        parar: &*parar,
        espiada: Duration::from_millis(argumentos.espiada_ms),
        vigia,
        regua: false,
        vigiar_morte: true,
        batimento: Some((&*batimento, batimento_base)),
        seguidor,
        // Várias sessões: a tela (e a câmera de bancada, sem dono). O receptor vê `sem_camera`.
        camera_remota: None,
    };
    #[cfg(feature = "tela-estendida-futura")]
    let gdi_agora = || seguidor_virtual.as_ref().map(|s| s.borrow().gdi_atual());
    #[cfg(not(feature = "tela-estendida-futura"))]
    let gdi_agora = || Option::<String>::None;
    let mut laco = Laco::novo();
    let com_som = cadeia_de_audio.is_some();
    let fim = laco.correr(
        &mut pronto,
        &mut cadeia,
        cadeia_de_audio.as_ref(),
        Some(&*toca_som),
        iv,
        ia,
        &ctx,
        &mut |c, enviados, audio_enviados, parada| {
            // A câmera parada: a frase no lugar dos contadores, até ela voltar.
            if let Some(ha) = parada {
                avisar(DaSessao::Resumo(crate::regras_da_camera::texto_da_camera_parada(ha)));
                return;
            }
            // No idioma da hora: o resumo é reescrito a cada relato, e a troca aparece no próximo.
            let onde = gdi_agora().map(|g| format!("{g} · ")).unwrap_or_default();
            let latencia = format!("{:.1}", c.latencia_media_ms());
            let repetidos = if c.repetidos > 0 { idioma::tf(" · {} repetidos", &[&c.repetidos]) } else { String::new() };
            let som = if com_som && audio_enviados > 0 { idioma::tf(" · {} quadros de som", &[&audio_enviados]) } else { String::new() };
            let resumo = idioma::tf("{}{} quadros · {} IDR · captura+encode {} ms{}", &[&onde, &enviados, &c.idrs, &latencia, &format!("{repetidos}{som}")]);
            avisar(DaSessao::Resumo(resumo));
        },
    );
    drop(ctx);
    if let Some(saida) = fim.saida {
        avisar(DaSessao::Saiu(saida));
    }

    // --- o desmonte, na ordem ---------------------------------------------------------------------
    if let Some(mut ca) = cadeia_de_audio.take() {
        ca.fechar();
        registro::linha(format!(
            "audio (fim da casca): {} | enviados={} recusados={} calados={}",
            ca.contadores.linha(),
            laco.audio_enviados,
            laco.audio_recusados,
            laco.audio_calados
        ));
    }
    laco.registrar_fim(&cadeia, &pronto, iv);
    let desligados = cadeia.desligar_tudo(Duration::from_secs(2));
    registro::linha(format!("cadeia desligada: {desligados}"));
    pronto.link.close("transmissão encerrada");
    drop(pronto);
    // O monitor: o de sempre, ou o que o seguidor recebeu do fio auxiliar. Um monitor que ainda
    // estava nascendo quando a sessão acabou cai no fio auxiliar, e o `Drop` do `Vivo` o solta.
    #[cfg(feature = "tela-estendida-futura")]
    let monitor = monitor_de_sempre.or_else(|| seguidor_virtual.and_then(|s| s.into_inner().monitor));
    #[cfg(not(feature = "tela-estendida-futura"))]
    let monitor = monitor_de_sempre;
    let soltura = match &monitor {
        Some(m) => monitores.soltar(m, Duration::from_secs(5)),
        None => Soltura::NadaASoltar,
    };
    registro::linha(format!("sessao encerrada — monitor: {soltura:?}"));
    avisar(DaSessao::Desmontada { monitor_solto: soltura.confirmou() });
}

// =============================================================================================
// O monitor e a cadeia de uma sessão: o caminho de sempre, e o do monitor virtual
// =============================================================================================

/// Por que o monitor ou a cadeia não subiram, e o que soltar.
pub struct FalhaDaAbertura {
    pub saida: Saida,
    /// O monitor que chegou a nascer e tem de ser solto.
    pub monitor: Option<MonitorDaSessao>,
    /// A linha do registro.
    pub texto: String,
    /// O motivo que vai no fechamento do link.
    pub fechar: &'static str,
}

/// **O caminho de sempre** (o monitor físico e a origem sintética): o monitor, depois a cadeia
/// pela entrada de sempre (`Cadeia::abrir_com`: encoder → adaptador → captura). É o trecho que
/// morava no meio de `correr_sessao_por_dentro`, movido para cá sem mudar.
fn abrir_como_antes(
    monitores: &dyn FonteDeMonitor,
    pedido: &PedidoDeMonitor,
    par: &Par,
    opcoes: OpcoesDaCadeia,
    parar: &AtomicBool,
    bater: &dyn Fn(),
) -> Result<(MonitorDaSessao, Cadeia), FalhaDaAbertura> {
    let monitor = match monitores.criar(pedido, None) {
        Ok(m) => m,
        Err(erro) => {
            return Err(FalhaDaAbertura {
                texto: format!("o monitor não nasceu: {erro}"),
                saida: Saida::NaoIniciou(erro),
                monitor: None,
                fechar: "o monitor não nasceu",
            })
        }
    };
    registro::linha("conectado: origem preparada");
    let origem_da_cadeia = match &monitor.origem {
        OrigemDoMonitor::Monitor(fonte) => match crate::fontes::achar_hmonitor(&fonte.id) {
            Some(hmonitor) => OrigemDaCadeia::Monitor { fonte, hmonitor },
            None => {
                let t = idioma::tf("o monitor \"{}\" não está mais conectado", &[&fonte.nome]);
                return Err(FalhaDaAbertura { texto: t.clone(), saida: Saida::NaoIniciou(t), monitor: Some(monitor), fechar: "o monitor sumiu" });
            }
        },
        OrigemDoMonitor::Sintetico { largura, altura, fps, carga, ritmo } => OrigemDaCadeia::Sintetica {
            largura: *largura,
            altura: *altura,
            fps: *fps,
            carga: *carga,
            ritmo: *ritmo,
        },
        // A câmera olha o Parar da sessão na abertura (a reconferência da fase 3) e, a cada olhada,
        // bate o sinal de vida: a criação da fonte de um nó novo leva ~4,6 s, e o cão de guarda do
        // coordenador denuncia aos 5 s (o R4 de novo, M54).
        OrigemDoMonitor::Camera { origem, .. } => OrigemDaCadeia::Camera { fonte: origem, parar: Some(parar), bater: Some(bater) },
        #[cfg(feature = "tela-estendida-futura")]
        OrigemDoMonitor::Virtual { .. } => {
            let t = "um monitor virtual chegou pelo caminho de sempre (a fonte não pediu o monitor antes da cadeia)".to_string();
            return Err(FalhaDaAbertura { texto: t.clone(), saida: Saida::NaoIniciou(t), monitor: Some(monitor), fechar: "a captura não subiu" });
        }
    };
    let e_camera = matches!(origem_da_cadeia, OrigemDaCadeia::Camera { .. });
    match Cadeia::abrir_com(origem_da_cadeia, opcoes) {
        Ok(c) => Ok((monitor, c)),
        Err(erro) => Err(FalhaDaAbertura {
            texto: format!("a cadeia não abriu: {erro}"),
            // A câmera diz a frase dela na lista; o detalhe de cada tentativa fica no registro
            // (a fase 4 de `docs/camera-no-windows.md`).
            saida: Saida::NaoIniciou(if e_camera {
                crate::regras_da_camera::so_a_frase(erro.message().as_ref()).to_string()
            } else {
                erro.to_string()
            }),
            monitor: Some(monitor),
            fechar: "a captura não subiu",
        }),
    }
}

/// **A abertura do monitor virtual**, na ordem que a medida pediu: a placa do processo (a Intel,
/// fixada uma vez) → o encoder daquela placa → o dispositivo daquele LUID → a cadeia sobre a
/// **origem preta** do tamanho do pedido — e o receptor já recebe quadro. O monitor nasce num fio
/// auxiliar, na fila do dono da topologia; o seguidor troca a origem pela captura dele quando ele (e
/// a janela da bancada) estiverem prontos. Sem isto, na N = 2 do Dell (15/09) o 1º receptor esperou
/// **4,7 s** da track ao primeiro quadro, atrás da ativação do 2º — e o Android e o iOS desistem com
/// 10 s (a revisão, item 7). Nenhum caminho de erro deixa MFT ativado sem `desligar`.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "tela-estendida-futura")]
fn abrir_no_monitor_virtual(
    monitores: &Arc<dyn FonteDeMonitor>,
    pedido: &PedidoDeMonitor,
    opcoes: OpcoesDaCadeia,
    parar: &Arc<AtomicBool>,
    cursor: bool,
    sem_captura: bool,
    avisos: Sender<(Id, DaSessao)>,
    id: Id,
) -> Result<(Cadeia, SeguidorVirtual), FalhaDaAbertura> {
    let falha = |saida: Saida, texto: String| FalhaDaAbertura { saida, monitor: None, texto, fechar: "a captura não subiu" };
    let placa = match monitores.placa_do_processo() {
        Some(Ok(p)) => p,
        Some(Err(e)) => return Err(falha(Saida::NaoIniciou(e.clone()), e)),
        None => return Err(falha(Saida::NaoIniciou("a fonte não diz a placa".into()), "a fonte não diz a placa".into())),
    };
    let (enc, como) = match crate::encoder::ativar_h264_na_placa(placa) {
        Ok(x) => x,
        Err(e) => return Err(falha(Saida::NaoIniciou(format!("o encoder da placa do monitor não ativou: {e}")), format!("encoder da placa {placa:016X}: {e}"))),
    };
    registro::linha(format!("encoder da placa {placa:016X}: \"{}\" ({como})", enc.friendly_name));
    let adaptador = match crate::device::create_device_por_luid(placa) {
        Ok(a) => a,
        Err(e) => {
            crate::encoder::desligar(&enc);
            return Err(falha(Saida::NaoIniciou(format!("o dispositivo da placa do monitor não abriu: {e}")), format!("dispositivo {placa:016X}: {e}")));
        }
    };
    // **A proteção multithread do contexto**: a captura do monitor virtual copia cada quadro para uma
    // textura nossa no fio do WGC, no mesmo contexto imediato que o laço usa (a revisão de 15/09,
    // item 1). O caminho de uma sessão só não passa por aqui e segue sem ela.
    match crate::device::proteger_contexto(&adaptador.context, true) {
        Ok(antes) => registro::linha(format!("d3d11: proteção multithread ligada no dispositivo do monitor virtual (antes={antes})")),
        Err(e) => {
            crate::encoder::desligar(&enc);
            return Err(falha(Saida::NaoIniciou(format!("a proteção multithread do dispositivo não ligou: {e}")), format!("proteção multithread: {e}")));
        }
    }
    let preta = match crate::sintetica::OrigemSintetica::nova(
        &adaptador.device,
        pedido.largura,
        pedido.altura,
        pedido.fps,
        crate::sintetica::Carga::Preta,
        crate::sintetica::Ritmo::Movendo,
    ) {
        Ok(o) => o,
        Err(e) => {
            crate::encoder::desligar(&enc);
            return Err(falha(Saida::NaoIniciou(format!("a origem preta não subiu: {e}")), format!("origem preta: {e}")));
        }
    };
    let cadeia = match Cadeia::abrir_no_monitor_virtual(enc, adaptador, capture::Captura::Sintetica(preta), opcoes) {
        Ok(c) => c,
        Err(e) => return Err(falha(Saida::NaoIniciou(e.to_string()), format!("a cadeia do monitor virtual não abriu: {e}"))),
    };
    // O monitor, num fio auxiliar: a fila do dono, a ativação limpa, a janela da bancada. Se a
    // sessão já tiver acabado quando ele chegar, o `send` falha, o `MonitorDaSessao` cai aqui e o
    // `Drop` do `Vivo` manda soltá-lo.
    let (tx, rx) = crossbeam_channel::bounded(1);
    let (m2, p2, parar2) = (monitores.clone(), pedido.clone(), parar.clone());
    let fio = std::thread::Builder::new().name(format!("quall.monitor.{id}")).spawn(move || {
        registro::prefixar_esta_thread(&format!("[#{id}] "));
        let r = m2.criar_com(&p2, &ContextoDoCriar { parar: &parar2, bater: &|| {} });
        let _ = tx.send(r);
    });
    if let Err(e) = fio {
        let mut cadeia = cadeia;
        cadeia.fechar();
        let _ = cadeia.desligar_tudo(Duration::from_secs(2));
        return Err(falha(Saida::NaoIniciou(format!("o fio do monitor não subiu: {e}")), format!("fio do monitor: {e}")));
    }
    registro::linha(format!(
        "conectado: monitor=\"{}\" {} — a origem preta vai ao receptor enquanto o monitor nasce na fila do dono",
        pedido.nome,
        pedido.descricao()
    ));
    Ok((cadeia, SeguidorVirtual::novo(rx, pedido.nome.clone(), cursor, sem_captura, avisos, id)))
}

/// Uma abertura (ou reabertura) em curso: o fio auxiliar abrindo a captura inteira (o item, o pool
/// e a sessão do WGC, `capture::pedir_captura`), e onde ela vai abrir.
#[cfg(feature = "tela-estendida-futura")]
struct Reabrindo {
    pedido: PedidoDeCaptura,
    desde: Instant,
    /// De onde o prazo da captura conta: da última vez que o dono estava mexendo (ver o passo 3).
    prazo_desde: Instant,
    onde: Onde,
    motivo: String,
}

/// **O seguidor do monitor virtual**, em três tempos: espera o monitor sair da fila do dono (o
/// receptor recebe a origem preta); abre a captura dele (num fio auxiliar, com prazo, com a janela da
/// bancada cobrindo); e depois segue o alvo — quando o `HMONITOR` muda, reabre a captura no novo, sem
/// parar o laço (a cadeia repete o último quadro enquanto isso) e sem recriar os encoders. O monitor
/// que só anda de lugar com o mesmo `HMONITOR` não reabre: o item do WGC segue o monitor, e o portão
/// descarta o que a janela ainda não cobre (a revisão de 15/09, item 14). **Nenhuma chamada do WGC no
/// fio da sessão**: a abertura é no fio auxiliar, e a captura velha fecha noutro (item 10). A troca só
/// com o tamanho igual.
#[cfg(feature = "tela-estendida-futura")]
struct SeguidorVirtual {
    /// O monitor ainda nascendo no fio auxiliar.
    aguardo: Option<crossbeam_channel::Receiver<Result<MonitorDaSessao, String>>>,
    desde: Instant,
    monitor: Option<MonitorDaSessao>,
    nome: String,
    cursor: bool,
    /// A bancada do controle (`--monitor-sem-captura`): segue o alvo, nunca abre a captura.
    sem_captura: bool,
    avisos: Sender<(Id, DaSessao)>,
    id: Id,
    /// Onde a captura de agora está; `None` = ainda a origem preta.
    atual: Option<Onde>,
    reabrindo: Option<Reabrindo>,
    erros: u32,
    /// Depois de uma troca de `HMONITOR`: o último quadro da captura velha, o motivo, e quando —
    /// para medir o buraco quando o primeiro quadro da nova passar.
    medindo: Option<(Option<Instant>, String, Instant)>,
}

#[cfg(feature = "tela-estendida-futura")]
impl SeguidorVirtual {
    fn novo(
        aguardo: crossbeam_channel::Receiver<Result<MonitorDaSessao, String>>,
        nome: String,
        cursor: bool,
        sem_captura: bool,
        avisos: Sender<(Id, DaSessao)>,
        id: Id,
    ) -> Self {
        SeguidorVirtual {
            aguardo: Some(aguardo),
            desde: Instant::now(),
            monitor: None,
            nome,
            cursor,
            sem_captura,
            avisos,
            id,
            atual: None,
            reabrindo: None,
            erros: 0,
            medindo: None,
        }
    }

    fn vivo(&self) -> Option<&Vivo> {
        self.monitor.as_ref().and_then(|m| m.virtual_.as_ref())
    }

    fn gdi_atual(&self) -> String {
        match (&self.atual, &self.monitor) {
            (Some(o), _) => o.gdi.clone(),
            (None, Some(_)) => "abrindo a captura do monitor".into(),
            (None, None) => "esperando o monitor".into(),
        }
    }
}

#[cfg(feature = "tela-estendida-futura")]
impl Seguidor for SeguidorVirtual {
    fn nome(&self) -> String {
        self.nome.clone()
    }

    fn olhar(&mut self, cadeia: &mut Cadeia) -> Seguimento {
        // 1. O monitor ainda nascendo na fila do dono: o receptor recebe a origem preta.
        if let Some(rx) = self.aguardo.as_ref() {
            match rx.try_recv() {
                Ok(Ok(m)) => {
                    registro::linha(format!(
                        "o monitor chegou {} ms depois do Transmitir: {} — a origem preta segue até a captura abrir",
                        self.desde.elapsed().as_millis(),
                        m.descricao
                    ));
                    if let Some(v) = m.virtual_.as_ref() {
                        let _ = self.avisos.send((self.id, DaSessao::MonitorDePe(v.chave())));
                    }
                    self.monitor = Some(m);
                    self.aguardo = None;
                }
                Ok(Err(e)) => {
                    self.aguardo = None;
                    let saida = if e == AVISO_DA_CAPTURA_PRESA { Saida::CapturaPresa } else { Saida::NaoIniciou(e.clone()) };
                    return Seguimento::Falhou(saida, format!("o monitor virtual não nasceu: {e}"));
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return Seguimento::Mexendo,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.aguardo = None;
                    return Seguimento::Falhou(Saida::NaoIniciou("o fio do monitor caiu".into()), "o fio do monitor caiu sem responder".into());
                }
            }
        }
        let Some(vivo) = self.vivo() else {
            return Seguimento::Falhou(Saida::NaoIniciou("o monitor virtual veio sem o que o segue".into()), "sem Vivo".into());
        };
        let alvo = vivo.alvo().target_id;
        let portao = vivo.portao();
        let (_epoca, mexendo, leitura) = vivo.onde_agora();
        let coberto = vivo.coberto_agora();

        // 2. O buraco da última troca, quando o primeiro quadro da captura nova passar.
        let mut medida_feita = false;
        if let Some((ultimo_velho, motivo, desde)) = self.medindo.as_ref() {
            if let Some(p) = cadeia.primeiro_quadro_da_captura() {
                let buraco = ultimo_velho.map(|u| p.saturating_duration_since(u)).unwrap_or_else(|| p.saturating_duration_since(*desde));
                registro::linha(format!(
                    "captura reaberta no HMONITOR novo: buraco de {} ms entre o último quadro da velha e o primeiro da nova ({motivo})",
                    buraco.as_millis()
                ));
                crate::monitores_virtuais::anotar_reabertura(alvo, buraco.as_millis() as u64, motivo);
                medida_feita = true;
            } else if desde.elapsed() >= Duration::from_secs(10) {
                registro::linha(format!("captura reaberta: nenhum quadro 10 s depois ({motivo})"));
                medida_feita = true;
            }
        }
        if medida_feita {
            self.medindo = None;
        }

        // 3. Uma abertura (ou reabertura) em curso?
        if let Some(r) = self.reabrindo.as_ref() {
            match r.pedido.resposta.try_recv() {
                Ok(Ok((nova, duracoes))) => {
                    let r = self.reabrindo.take().expect("em curso");
                    // A testemunha é **este** alvo no mapa: o mesmo nome, `HMONITOR` e retângulo de
                    // quando a captura foi pedida, e a janela cobrindo. Nem a época do mapa (sobe com a
                    // mudança de **qualquer** alvo) nem o "dono mexendo": com oito chegando uma atrás
                    // da outra o dono mexe o tempo todo, e na N = 8 de 15/09 nenhuma captura entrou.
                    // Se o `HMONITOR` morrer logo depois, o mapa mostra o novo e a sessão reabre; o
                    // portão da cobertura segura o que passar no meio.
                    let rotulo = format!("a captura de {} (hmonitor {:X})", r.onde.gdi, r.onde.hmonitor);
                    if !(leitura == Leitura::Ativo(r.onde.clone()) && nova.width == r.onde.largura() && nova.height == r.onde.altura() && coberto) {
                        // A arrumação mudou de novo enquanto ela abria: fecha fora daqui e tenta de novo.
                        capture::fechar_em_segundo_plano(nova, rotulo);
                        return Seguimento::Mexendo;
                    }
                    let ultimo_velho = cadeia.ultimo_quadro_da_captura();
                    let t_troca = Instant::now();
                    match cadeia.trocar_captura(nova) {
                        Ok(velha) => {
                            let troca_us = t_troca.elapsed().as_micros();
                            match velha {
                                capture::Captura::Tela(c) => capture::fechar_em_segundo_plano(c, format!("a captura velha de {}", self.gdi_atual())),
                                capture::Captura::Sintetica(mut s) => s.parar(),
                                // O monitor virtual nunca troca uma câmera; se trocasse, ela para aqui.
                                capture::Captura::Camera(mut c) => c.stop(),
                                capture::Captura::DoDono(mut l) => l.stop(),
                            }
                            self.erros = 0;
                            let tempos = format!(
                                "item {} ms, pool e sessão {} ms no fio auxiliar; a troca {} µs no fio da sessão",
                                duracoes.item_ms, duracoes.captura_ms, troca_us
                            );
                            match self.atual.as_ref() {
                                None => registro::linha(format!(
                                    "a captura do monitor entrou na cadeia {} ms depois do Transmitir: {} (hmonitor {:X}) — a origem preta saiu ({tempos})",
                                    self.desde.elapsed().as_millis(),
                                    r.onde.gdi,
                                    r.onde.hmonitor
                                )),
                                Some(a) => {
                                    registro::linha(format!(
                                        "captura trocada: {} → {} (hmonitor {:X} → {:X}) em {} ms, sem recriar os encoders ({}; {tempos})",
                                        a.gdi,
                                        r.onde.gdi,
                                        a.hmonitor,
                                        r.onde.hmonitor,
                                        r.desde.elapsed().as_millis(),
                                        r.motivo
                                    ));
                                    self.medindo = Some((ultimo_velho, r.motivo, Instant::now()));
                                }
                            }
                            self.atual = Some(r.onde);
                            return Seguimento::Presente;
                        }
                        Err((nova, t)) => {
                            capture::fechar_em_segundo_plano(nova, rotulo);
                            return Seguimento::Falhou(Saida::FonteSumiu(idioma::tf("O monitor \"{}\" mudou de tamanho.", &[&self.nome])), t);
                        }
                    }
                }
                Ok(Err(e)) => {
                    self.reabrindo = None;
                    // Com o dono mexendo, o `HMONITOR` do mapa pode ter acabado de morrer: não conta.
                    // E só erros seguidos contam: uma abertura boa zera (a revisão de 15/09, item 13).
                    if !mexendo {
                        self.erros += 1;
                    }
                    registro::linha(format!("captura: a abertura devolveu erro ({e}); tento de novo com o HMONITOR de agora"));
                    if self.erros > 5 {
                        return Seguimento::Falhou(Saida::NaoIniciou(idioma::t("a captura do monitor não abriu").into()), format!("{} erros seguidos na abertura da captura", self.erros));
                    }
                    return Seguimento::Mexendo;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // **O prazo conta com o dono parado.** Na N = 8 de 15/09, três `CreateForMonitor`
                    // passaram dos 5 s com duas `SetDisplayConfig` de ~2 s dentro deles, e os pedidos
                    // seguintes do mesmo processo voltaram: não era o Windows preso de ~490 monitores
                    // (§13.5). Com o dono mexendo o relógio recomeça — até um teto de 30 s no total.
                    if mexendo && r.desde.elapsed() < TETO_DA_CAPTURA_COM_O_DONO_MEXENDO {
                        if let Some(r) = self.reabrindo.as_mut() {
                            r.prazo_desde = Instant::now();
                        }
                        return Seguimento::Mexendo;
                    }
                    let (parado, total) = (r.prazo_desde.elapsed(), r.desde.elapsed());
                    if parado >= PRAZO_DA_CAPTURA || total >= TETO_DA_CAPTURA_COM_O_DONO_MEXENDO {
                        // O fio fica para trás, preso: só este caminho o marca como abandonado.
                        if let Some(r) = self.reabrindo.take() {
                            r.pedido.abandonar();
                        }
                        if let Some(v) = self.vivo() {
                            v.marcar_captura_travada();
                        }
                        return Seguimento::Falhou(
                            Saida::CapturaPresa,
                            format!(
                                "a abertura da captura não voltou: {} ms com o dono parado, {} ms desde o pedido — {AVISO_DA_CAPTURA_PRESA}",
                                parado.as_millis(),
                                total.as_millis()
                            ),
                        );
                    }
                    return Seguimento::Mexendo;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.reabrindo = None;
                    return Seguimento::Mexendo;
                }
            }
        }

        // 4. O mapa do dono. Com ele mexendo, a ausência não conta (o monitor sai da área de
        // trabalho por ~0,1–1,2 s em cada chegada); abrir e reabrir, sim — o mapa de um alvo só
        // muda quando o dono o publica, e o portão da cobertura segura o que passar.
        match leitura {
            // Não deu para ler (a tela bloqueada, por exemplo): não é saída (a revisão, item 15).
            Leitura::Erro(_) => Seguimento::Mexendo,
            // O dono dos monitores caiu, ou devolveu a tela da pessoa: o motivo é dele, e é o que a
            // sessão diz (a revisão de 15/09, itens 5 e 17).
            Leitura::Terminal(m) => Seguimento::Falhou(Saida::FonteSumiu(m.clone()), format!("o monitor virtual acabou: {m}")),
            Leitura::Descoberto(m) => Seguimento::Falhou(
                Saida::FonteSumiu(format!("A janela sintética da bancada saiu do monitor \"{}\".", self.nome)),
                format!("a cobertura acabou: {m}"),
            ),
            // Antes da primeira captura, "fora" é o monitor ainda chegando: não conta.
            Leitura::Inativo(m) if self.atual.is_some() && !mexendo => Seguimento::Ausente(m),
            Leitura::Inativo(_) => Seguimento::Mexendo,
            Leitura::Ativo(onde) => {
                let fechou = cadeia.fonte_sumiu();
                let motivo = match self.atual.as_ref() {
                    None => Some("a primeira captura do monitor".to_string()),
                    Some(a) => match ativacao::reabertura(a.hmonitor, Some(onde.hmonitor), fechou) {
                        // O mesmo `HMONITOR` e o item vivo: a captura segue o monitor, ande ele de
                        // lugar ou mude de nome — não reabre (cada reabertura custou 474–1.555 ms, a
                        // revisão de 15/09, item 14). O portão descarta o que a janela ainda não cobre.
                        Reabertura::Manter => None,
                        _ if fechou && onde.hmonitor == a.hmonitor => Some("o item da captura fechou com o alvo ativo".to_string()),
                        _ => Some(format!("{} {:?} → {} {:?}", a.gdi, a.rect, onde.gdi, onde.rect)),
                    },
                };
                let Some(motivo) = motivo else {
                    if let Some(a) = self.atual.as_ref().filter(|a| **a != onde) {
                        registro::linha(format!(
                            "o monitor andou com o mesmo HMONITOR ({} {:?} → {} {:?}): a captura segue, sem reabrir",
                            a.gdi, a.rect, onde.gdi, onde.rect
                        ));
                        self.atual = Some(onde);
                    }
                    return Seguimento::Presente;
                };
                if self.sem_captura {
                    return Seguimento::Presente;
                }
                // Sem a nossa janela cobrindo o monitor, esperar: sem ela, ele mostra o papel de
                // parede da pessoa.
                if !coberto {
                    return Seguimento::Mexendo;
                }
                registro::linha(format!("seguindo o alvo {alvo}: abro a captura ({motivo})"));
                self.reabrindo = Some(Reabrindo {
                    pedido: capture::pedir_captura(cadeia.dispositivo(), HMONITOR(onde.hmonitor as *mut std::ffi::c_void), portao, self.cursor),
                    desde: Instant::now(),
                    prazo_desde: Instant::now(),
                    onde,
                    motivo,
                });
                Seguimento::Mexendo
            }
        }
    }
}

/// A falha do núcleo, na classificação da tabela de sessões. Os braços são os de
/// `emissor.rs::ao_falhar`.
pub fn falha_de(erro: &Error) -> Falha {
    match erro {
        Error::Cancelled => Falha::Cancelada,
        Error::NeedsPin(_) => Falha::PrecisaDePin,
        Error::NoRoute(_) => Falha::SemRota,
        Error::Timeout(_) => Falha::Prazo,
        // Dívida 29: o PIN errado tem variante própria no núcleo, e o `ao_falhar` do caminho de uma
        // sessão só ainda não a separa (cai no texto cru do erro).
        Error::WrongPin(_) => Falha::PinErrado,
        Error::Pairing(_) => Falha::Pareamento,
        outro => Falha::Outra(outro.to_string()),
    }
}
