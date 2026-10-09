//! **O coordenador do emissor com vários receptores** (`--varias-sessoes`, bandeira de bancada, e a
//! fonte "Tela estendida" do seletor sem a bandeira — R10, 02/10).
//!
//! Sem a bandeira, o coordenador nasce no primeiro Espelhar da tela estendida e vive até o processo
//! sair, mas só **publica** o estado da janela enquanto é dono da tela
//! (`Emissor::coordenador_na_tela`): o Espelhar de outra fonte devolve a tela ao caminho de uma
//! sessão só, e o coordenador fica ocioso, sem escrever nada.
//!
//! Uma thread, dona da tabela de sessões (`sessoes.rs`) — a única que a toca. Os avisos chegam por
//! dois canais: os pedidos da interface (Espelhar, Parar, Desconectar) e os avisos das threads de
//! sessão (`sessao_de_emissao.rs`). Cada aviso vira uma chamada na tabela, e a tabela pede efeitos
//! por [`Efeitos`](crate::sessoes::Efeitos): abrir uma espera (o servidor, o PIN e a thread da
//! sessão), anunciar, mandar transmitir, mandar encerrar, gravar os índices. Depois de cada aviso,
//! o estado que a janela lê é republicado a partir da tabela — **a fase do app sai da lista de
//! sessões**, e o fim de uma sessão não toca nas outras.
//!
//! # Quem é dono de quê
//!
//! | coisa | dono | quem mais toca |
//! |---|---|---|
//! | `Ready` (sessão, link, tracks do núcleo) | a thread da sessão | ninguém |
//! | `Cadeia` (captura, encoders, oficina) | a thread da sessão | ninguém |
//! | `Cancelamento`, bandeira de parada, canal de ordens | o coordenador (`Controle`) | a sessão só lê |
//! | tabela de sessões, índices | o coordenador | ninguém |
//! | anúncio mDNS | a thread do anunciante | o coordenador só pede |
//! | `Estado` da janela | o coordenador escreve | a janela lê |
//!
//! # A ordem do desmonte
//!
//! Parar: a tabela manda encerrar todas; cada sessão para a captura e o som, desliga os encoders
//! (serviço, reserva e oficina, com prazo), fecha o link, larga o `Ready`, solta o monitor e avisa
//! `Desmontada`; o coordenador junta a thread e tira o `Controle`. A tela volta ao começo quando a
//! última avisa — ou quando `PRAZO_DO_DESMONTE_MS` vence. A saída do processo espera isso e a fila
//! do anúncio antes do `MFShutdown` (`esperar_desmonte`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{select, unbounded, Receiver, Sender};
use quall_core::cancel::Cancelamento;
use quall_core::discovery::{anuncio, Advertiser};
use quall_core::pairing::Pin;
use quall_core::protocol::Capabilities;
use quall_core::signaling::SignalingServer;

use crate::emissor::{Emissor, Fase, ReceptorNaTela};
use crate::encoder::Preferencia;
use crate::fontes::Fonte;
use crate::idioma;
use crate::captura_de_camera::FonteDaCamera;
use crate::monitor::{CamerasDaSessao, FonteDeMonitor, MonitoresFisicos, MonitoresSinteticos, PedidoDeMonitor, FONTE_TELA_ESTENDIDA};
#[cfg(feature = "tela-estendida-futura")]
use crate::monitores_virtuais::MonitoresVirtuais;
use crate::sessao_de_emissao::{correr_sessao, handles_do_processo, DaSessao, Ordem, PedidoDaSessao};
use crate::sessoes::{self, Efeitos, Id, Par, Rodada, Tabela};
use crate::tabela_de_indices::TabelaDeIndices;
use crate::transmissao::PadroesDaTelaEstendida;
use crate::{enderecos, identidade, registro};

/// Quanto tempo sem batimento até o cão de guarda denunciar uma sessão.
const SEM_BATIMENTO: Duration = Duration::from_secs(5);

/// Quanto tempo sem batimento até o coordenador soltar o monitor virtual da sessão pela chave (a
/// revisão, item 3): maior que o alarme, porque uma chamada do WGC com monitores chegando segurou
/// uma sessão viva ~6,5 s (N = 8, 15/09). É o prazo que a tabela dá a um desmonte.
const SEM_BATIMENTO_PARA_SOLTAR: Duration = Duration::from_millis(sessoes::PRAZO_DO_DESMONTE_MS);

enum Pedido {
    Espelhar { fonte: Fonte, com_som: bool },
    Encerrar,
    Desconectar(Id),
    /// Responder `true` quando tudo tiver desmontado.
    EsperarDesmonte(Sender<bool>),
    /// Um conselho do emissor (a fonte que sumiu, "Escolha o que transmitir.", a câmera que ainda
    /// não transmite): vai para a tabela, porque o `publicar` reescreve o conselho da tela com o
    /// dela a cada volta (a revisão de código de 18/09, M3). Vazio tira.
    Conselho(String),
}

/// O lado de fora do coordenador: o que o `Emissor` segura.
pub struct Coordenador {
    pedidos: Sender<Pedido>,
}

impl Coordenador {
    pub fn novo(emissor: Arc<Emissor>) -> Coordenador {
        let (tx, rx) = unbounded::<Pedido>();
        let _ = std::thread::Builder::new()
            .name("quall.coordenador".into())
            .spawn(move || correr(emissor, rx));
        Coordenador { pedidos: tx }
    }

    pub fn espelhar(&self, fonte: Fonte, com_som: bool) {
        let _ = self.pedidos.send(Pedido::Espelhar { fonte, com_som });
    }

    pub fn encerrar(&self) {
        let _ = self.pedidos.send(Pedido::Encerrar);
    }

    pub fn desconectar(&self, id: Id) {
        let _ = self.pedidos.send(Pedido::Desconectar(id));
    }

    pub fn aconselhar(&self, texto: String) {
        let _ = self.pedidos.send(Pedido::Conselho(texto));
    }

    pub fn esperar_desmonte(&self, prazo: Duration) -> bool {
        let (tx, rx) = unbounded();
        if self.pedidos.send(Pedido::EsperarDesmonte(tx)).is_err() {
            return true;
        }
        rx.recv_timeout(prazo).unwrap_or(false)
    }
}

/// O que o coordenador guarda de cada sessão viva — nenhum handle do núcleo.
struct Controle {
    cancelamento: Cancelamento,
    parar: Arc<AtomicBool>,
    ordens: Sender<Ordem>,
    batimento: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
    ultimo_alerta: Option<Instant>,
    /// A chave (o GUID) do monitor virtual desta sessão, quando ela avisou que ele ficou de pé.
    monitor: Option<u128>,
    /// O coordenador já mandou soltar este monitor pela chave (uma vez só).
    soltou_pela_chave: bool,
    /// D2: esta sessão manda o som (`Efeitos::dar_o_som`). A sessão lê a cada volta do laço.
    toca_som: Arc<AtomicBool>,
}

/// O anúncio mDNS numa thread própria: `Advertiser::stop` bloqueia até ~1 s (dívida 3), e o
/// anúncio novo só sai **depois** do adeus do velho — todas as esperas anunciam o mesmo nome de
/// serviço, e um adeus que chegasse depois do anúncio novo apagaria o novo da lista dos outros
/// aparelhos (revisão adversarial de 13/09/2026). Pedidos acumulados viram o último.
struct Anunciante {
    pedidos: Sender<Option<u16>>,
    pendentes: Arc<AtomicUsize>,
}

impl Anunciante {
    fn novo(emissor: Arc<Emissor>, device_id: String, nome: String, desligado: bool) -> Anunciante {
        let (tx, rx) = unbounded::<Option<u16>>();
        let pendentes = Arc::new(AtomicUsize::new(0));
        let p = pendentes.clone();
        let _ = std::thread::Builder::new().name("quall.anuncio".into()).spawn(move || {
            let eu = anuncio(
                &device_id,
                &nome,
                Capabilities { screen_source: true, camera_source: false, sink: false },
            );
            let mut atual: Option<Advertiser> = None;
            while let Ok(mut porta) = rx.recv() {
                let mut juntos = 1;
                while let Ok(mais) = rx.try_recv() {
                    porta = mais;
                    juntos += 1;
                }
                if let Some(a) = atual.take() {
                    let _ = a.stop();
                }
                let anunciando = match porta {
                    // `--so-local` (bancada): nada escuta fora do loopback, nem o mDNS.
                    Some(porta) if desligado => {
                        registro::linha(format!("mdns: desligado por --so-local (a espera na porta {porta} só em 127.0.0.1)"));
                        false
                    }
                    Some(porta) => match Advertiser::start(&eu, porta) {
                        Ok(a) => {
                            atual = Some(a);
                            registro::linha(format!("mdns: anunciando a porta {porta}"));
                            true
                        }
                        Err(e) => {
                            registro::linha(format!("mdns: não anunciou a porta {porta}: status={}", crate::diagnostico_rede::status(&e)));
                            false
                        }
                    },
                    None => {
                        registro::linha("mdns: parado");
                        false
                    }
                };
                {
                    let mut e = emissor.estado();
                    if e.anunciando_por_mdns != anunciando {
                        e.anunciando_por_mdns = anunciando;
                        e.versao += 1;
                    }
                }
                p.fetch_sub(juntos, Ordering::SeqCst);
            }
            if let Some(a) = atual.take() {
                let _ = a.stop();
            }
        });
        Anunciante { pedidos: tx, pendentes }
    }

    fn pedir(&self, porta: Option<u16>) {
        self.pendentes.fetch_add(1, Ordering::SeqCst);
        if self.pedidos.send(porta).is_err() {
            self.pendentes.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn ocioso(&self) -> bool {
        self.pendentes.load(Ordering::SeqCst) == 0
    }
}

/// Os efeitos de verdade da tabela.
struct EfeitosReais {
    emissor: Arc<Emissor>,
    controles: HashMap<Id, Controle>,
    anunciante: Anunciante,
    tx_sessoes: Sender<(Id, DaSessao)>,
    base: Instant,
    /// O que vale para as sessões da rodada em curso.
    monitores: Option<Arc<dyn FonteDeMonitor>>,
    rotulo: String,
    nome: String,
    device_id: String,
    preferencia: Preferencia,
}

impl Efeitos for EfeitosReais {
    fn abrir_espera(
        &mut self,
        id: Id,
        porta: u16,
        pin: Option<&str>,
        com_audio: bool,
    ) -> Result<(u16, String), String> {
        let Some(monitores) = self.monitores.clone() else {
            return Err("não há rodada".into()); // i18n: fora (diário e caso interno)
        };
        // `--so-local` (bancada): a espera só em 127.0.0.1 — no Dell, um `.exe` novo escutando fora
        // do loopback faz o firewall mostrar um aviso na tela do usuário. O produto escuta em todas.
        let so_local = self.emissor.argumentos.so_local;
        let abrir = |p: u16| {
            if so_local {
                SignalingServer::bind_em(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), p)
            } else {
                SignalingServer::bind(p)
            }
        };
        let servidor = match abrir(porta) {
            Ok(s) => s,
            Err(e) if porta != 0 => {
                registro::linha(format!(
                    "[#{id}] a porta {porta} não abriu de novo ({e}) — a espera vai para uma porta livre"
                ));
                abrir(0).map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };
        let porta_de_fato = servidor.port().map_err(|e| e.to_string())?;
        let pin = match pin {
            Some(p) => Pin::parse(p),
            None => Pin::generate(),
        }
        .map_err(|e| e.to_string())?;
        let pin_texto = pin.to_display();
        let (tx_ordens, rx_ordens) = unbounded();
        let cancelamento = Cancelamento::novo();
        let parar = Arc::new(AtomicBool::new(false));
        let batimento = Arc::new(AtomicU64::new(self.base.elapsed().as_millis() as u64));
        let toca_som = Arc::new(AtomicBool::new(false));
        let pedido = PedidoDaSessao {
            id,
            servidor,
            pin,
            com_audio,
            toca_som: toca_som.clone(),
            argumentos: self.emissor.argumentos.clone(),
            monitores,
            avisos: self.tx_sessoes.clone(),
            ordens: rx_ordens,
            cancelamento: cancelamento.clone(),
            parar: parar.clone(),
            batimento: batimento.clone(),
            batimento_base: self.base,
            nome_do_aparelho: self.nome.clone(),
            device_id: self.device_id.clone(),
            rotulo: self.rotulo.clone(),
            preferencia: self.preferencia,
            padroes: PadroesDaTelaEstendida {
                teto_de_quadro_em_medios: self.emissor.argumentos.teto_quadro_em_medios.max(0.0),
                ..PadroesDaTelaEstendida::DO_MAC
            },
        };
        let thread = std::thread::Builder::new()
            .name(format!("quall.sessao.{id}"))
            .spawn(move || correr_sessao(pedido))
            .map_err(|e| e.to_string())?;
        self.controles.insert(
            id,
            Controle {
                cancelamento,
                parar,
                ordens: tx_ordens,
                batimento,
                thread: Some(thread),
                ultimo_alerta: None,
                monitor: None,
                soltou_pela_chave: false,
                toca_som,
            },
        );
        registro::linha(format!(
            "[#{id}] espera: porta={porta_de_fato} com_som={com_audio}"
        ));
        Ok((porta_de_fato, pin_texto))
    }

    fn anunciar(&mut self, porta: Option<u16>) {
        self.anunciante.pedir(porta);
    }

    fn transmitir(&mut self, id: Id, indice: usize, par: &Par) {
        if let Some(c) = self.controles.get(&id) {
            let _ = c.ordens.send(Ordem::Transmitir { indice, par: par.clone() });
        }
    }

    fn encerrar(&mut self, id: Id) {
        if let Some(c) = self.controles.get(&id) {
            c.parar.store(true, Ordering::SeqCst);
            c.cancelamento.cancelar();
            let _ = c.ordens.send(Ordem::Encerrar);
        }
    }

    fn dar_o_som(&mut self, id: Id, mandar: bool) {
        if let Some(c) = self.controles.get(&id) {
            c.toca_som.store(mandar, Ordering::SeqCst);
        }
    }

    fn gravar_indices(&mut self, tabela: &TabelaDeIndices) {
        identidade::gravar_indices_de_monitor(tabela);
    }

    fn registrar(&mut self, linha: String) {
        registro::linha(linha);
    }
}

impl EfeitosReais {
    /// **Solta pela chave** o monitor virtual da sessão `id`, uma vez: a sessão não confirmou a
    /// soltura, sumiu sem batimento, ou o Parar venceu o prazo com ela presa (a revisão, item 3).
    fn soltar_pela_chave(&mut self, id: Id, porque: &str) {
        let Some(m) = self.monitores.clone() else { return };
        if let Some(c) = self.controles.get_mut(&id) {
            if let (Some(chave), false) = (c.monitor, c.soltou_pela_chave) {
                c.soltou_pela_chave = true;
                registro::linha(format!("[#{id}] monitor virtual solto pelo coordenador ({porque})"));
                m.soltar_pela_chave(chave);
            }
        }
    }

    /// O último aviso de uma sessão chegou: a thread está saindo — junta e esquece.
    fn finalizar(&mut self, id: Id) {
        if let Some(mut c) = self.controles.remove(&id) {
            if let Some(t) = c.thread.take() {
                let comeco = Instant::now();
                while !t.is_finished() && comeco.elapsed() < Duration::from_secs(2) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                if t.is_finished() {
                    let _ = t.join();
                } else {
                    registro::linha(format!("[#{id}] !! a thread não terminou 2 s depois do último aviso"));
                }
            }
        }
    }
}

fn agora_ms(base: Instant) -> u64 {
    base.elapsed().as_millis() as u64
}

fn correr(emissor: Arc<Emissor>, pedidos: Receiver<Pedido>) {
    let base = Instant::now();
    let (tx_sessoes, rx_sessoes) = unbounded::<(Id, DaSessao)>();
    let nome = emissor.estado().nome_do_aparelho.clone();
    let device_id = identidade::device_id();
    let preferencia = if emissor.argumentos.preferir_intel {
        Preferencia::Intel
    } else {
        Preferencia::Produto
    };
    let mut fx = EfeitosReais {
        anunciante: Anunciante::novo(Arc::clone(&emissor), device_id.clone(), nome.clone(), emissor.argumentos.so_local),
        emissor: Arc::clone(&emissor),
        controles: HashMap::new(),
        tx_sessoes,
        base,
        monitores: None,
        rotulo: String::new(),
        nome,
        device_id,
        preferencia,
    };
    let mut tabela = Tabela::nova(identidade::indices_de_monitor());
    let mut resumos: HashMap<Id, String> = HashMap::new();
    // O som de cada sessão: subiu, ou por que não (D2: toda sessão com som captura).
    let mut som_das_sessoes: HashMap<Id, (bool, String)> = HashMap::new();
    let mut esperas_do_desmonte: Vec<Sender<bool>> = Vec::new();
    let mut ultima_regua = Instant::now();
    registro::linha(format!(
        "várias sessões: coordenador de pé — limite {} | preferência de MFT {:?} | índices gravados: {}",
        sessoes::LIMITE_DE_SESSOES,
        preferencia,
        tabela.indices().len()
    ));

    loop {
        select! {
            recv(pedidos) -> m => {
                let Ok(p) = m else { break };
                match p {
                    Pedido::Espelhar { fonte, com_som } => {
                        // O monitor virtual: a bandeira de bancada, ou a fonte "Tela estendida" que a
                        // janela oferece com o adaptador do SudoVDA presente (R10: com ou sem
                        // `--varias-sessoes`).
                        #[cfg(feature = "tela-estendida-futura")]
                        let virtual_ = emissor.argumentos.monitor_virtual || fonte.id == FONTE_TELA_ESTENDIDA;
                        #[cfg(not(feature = "tela-estendida-futura"))]
                        let virtual_ = false;
                        // **A câmera** (fase 3 de `docs/camera-no-windows.md`): a escolhida na tela
                        // inicial, ou a fonte do Quall no processo com
                        // `--camera-sintetica` (bancada, padrão de bancada nosso).
                        let camera = if emissor.argumentos.camera_sintetica {
                            Some(CamerasDaSessao { fonte: fonte.clone(), origem: FonteDaCamera::DoQuallNoProcesso { regua: true } })
                        } else if fonte.e_camera() {
                            Some(CamerasDaSessao { fonte: fonte.clone(), origem: FonteDaCamera::Link(fonte.id.clone()) })
                        } else {
                            None
                        };
                        let e_camera = camera.is_some();
                        let monitores: Arc<dyn FonteDeMonitor> = if let Some(c) = camera {
                            Arc::new(c)
                        } else if virtual_ {
                            #[cfg(not(feature = "tela-estendida-futura"))]
                            unreachable!("monitor virtual excluído do release");
                            #[cfg(feature = "tela-estendida-futura")]
                            match MonitoresVirtuais::do_processo(emissor.argumentos.cobrir_monitor, emissor.argumentos.monitor_sem_intel) {
                                Ok(m) => m,
                                Err(e) => {
                                    registro::linha(format!("espelhar (várias sessões): os monitores virtuais não sobem: {e}"));
                                    // O motivo, e que o próximo Espelhar tenta de novo: a falha não fica
                                    // guardada (`MonitoresVirtuais::do_processo`).
                                    tabela.definir_conselho(crate::regras_da_tela_estendida::conselho_da_falha(&e));
                                    // A fase não sai da inicial: a câmera volta a ser de quem pedir
                                    // (a tela R5), que o `publicar` só solta numa volta ao início.
                                    crate::dono_da_captura::soltar_da_janela();
                                    publicar(&emissor, &tabela, &resumos, &som_das_sessoes);
                                    continue;
                                }
                            }
                        } else {
                            match emissor.argumentos.origem_sintetica {
                                Some(carga) => Arc::new(MonitoresSinteticos { carga, ritmo: emissor.argumentos.ritmo_sintetico }),
                                None => Arc::new(MonitoresFisicos { fonte: fonte.clone() }),
                            }
                        };
                        fx.rotulo = if emissor.argumentos.camera_sintetica {
                            format!("Câmera sintética de {}", fx.nome) // i18n: fora (bancada)
                        } else if e_camera {
                            fonte.rotulo_da_track(&fx.nome)
                        } else if virtual_ {
                            idioma::tf("Tela estendida de {}", &[&fx.nome])
                        } else if emissor.argumentos.origem_sintetica.is_some() {
                            format!("Tela estendida (sintética) de {}", fx.nome) // i18n: fora (bancada)
                        } else {
                            fonte.rotulo_da_track(&fx.nome)
                        };
                        registro::linha(format!(
                            "espelhar (várias sessões): monitores = {} | som={com_som}",
                            monitores.descricao()
                        ));
                        fx.monitores = Some(monitores);
                        resumos.clear();
                        som_das_sessoes.clear();
                        let rodada = Rodada {
                            varias: true,
                            limite: sessoes::LIMITE_DE_SESSOES,
                            com_som,
                            passar_som: !emissor.argumentos.sem_passar_som,
                            porta: emissor.argumentos.porta,
                            pin: emissor.argumentos.pin.clone(),
                        };
                        if !tabela.espelhar(rodada, agora_ms(base), &mut fx) {
                            // Nem a primeira espera abriu (o conselho diz por quê): a fase fica na
                            // inicial, e a câmera volta a ser de quem pedir.
                            crate::dono_da_captura::soltar_da_janela();
                        }
                    }
                    Pedido::Encerrar => {
                        // O monitor virtual: tirar todos da área de trabalho de uma vez, antes de
                        // cada sessão soltar o seu (a revisão, item 2) — por isso **antes** de a
                        // tabela mandar as sessões pararem: na N = 8 de 15/09, uma soltura entrou na
                        // fila antes do recolher e custou um reparo no meio do Parar.
                        if let Some(m) = &fx.monitores {
                            m.recolher();
                        }
                        tabela.encerrar(&mut fx);
                    }
                    Pedido::Desconectar(id) => tabela.desconectar(id, &mut fx),
                    Pedido::EsperarDesmonte(tx) => esperas_do_desmonte.push(tx),
                    // Só na tela inicial: com sessões no ar o conselho é delas.
                    Pedido::Conselho(texto) => {
                        if tabela.fase() == sessoes::Fase::Inicial {
                            tabela.definir_conselho(texto);
                        }
                    }
                }
            }
            recv(rx_sessoes) -> m => {
                if let Ok((id, d)) = m {
                    let agora = agora_ms(base);
                    match d {
                        DaSessao::Conectou(par) => tabela.conectou(id, par, agora, &mut fx),
                        DaSessao::FalhouAoHospedar(falha, texto) => {
                            if !texto.is_empty() {
                                registro::linha(format!("[#{id}] a espera acabou: {texto}"));
                            }
                            tabela.falhou_ao_hospedar(id, falha, agora, &mut fx);
                            fx.finalizar(id);
                        }
                        DaSessao::Saiu(saida) => tabela.saiu(id, saida, agora, &mut fx),
                        DaSessao::Resumo(t) => {
                            resumos.insert(id, t);
                        }
                        DaSessao::Som { ativo, recusado } => {
                            // D2: a sessão sem som não segura o som (crítica 13, M3).
                            if !ativo {
                                tabela.sem_som(id, &mut fx);
                            }
                            som_das_sessoes.insert(id, (ativo, recusado));
                        }
                        DaSessao::MonitorDePe(chave) => {
                            if let Some(c) = fx.controles.get_mut(&id) {
                                c.monitor = Some(chave);
                            }
                        }
                        DaSessao::Desmontada { monitor_solto } => {
                            if !monitor_solto {
                                tabela.monitor_nao_soltou(id, &mut fx);
                                fx.soltar_pela_chave(id, "a sessão não confirmou a soltura"); // i18n: fora (diário e caso interno)
                            }
                            tabela.desmontada(id, agora, &mut fx);
                            resumos.remove(&id);
                            som_das_sessoes.remove(&id);
                            fx.finalizar(id);
                        }
                    }
                }
            }
            default(Duration::from_millis(250)) => {}
        }

        let agora = agora_ms(base);
        let fase_antes = tabela.fase();
        tabela.tique(agora, &mut fx);
        // O Parar venceu o prazo com sessões presas: os monitores delas saem pela chave.
        if fase_antes != sessoes::Fase::Inicial && tabela.fase() == sessoes::Fase::Inicial {
            let presas: Vec<Id> = fx.controles.keys().copied().collect();
            for id in presas {
                fx.soltar_pela_chave(id, "o Parar venceu o prazo com a sessão presa"); // i18n: fora (diário e caso interno)
            }
        }
        vigiar_batimentos(&tabela, &mut fx, agora);
        // O batimento do coordenador, a cada volta: o fio de ping do monitor virtual só pinga com
        // ele recente (um coordenador travado não mantém monitor de pé na tela de ninguém). As
        // implementações provisórias não fazem nada aqui.
        if let Some(m) = &fx.monitores {
            m.alimentar();
        }
        if !fx.controles.is_empty() && ultima_regua.elapsed() >= Duration::from_secs(15) {
            ultima_regua = Instant::now();
            registro::linha(format!(
                "régua (processo): handles={} sessões_vivas={} fase={:?}",
                handles_do_processo(),
                fx.controles.len(),
                tabela.fase()
            ));
        }
        publicar(&emissor, &tabela, &resumos, &som_das_sessoes);
        if !esperas_do_desmonte.is_empty()
            && tabela.fase() == sessoes::Fase::Inicial
            && fx.controles.is_empty()
            && fx.anunciante.ocioso()
        {
            for tx in esperas_do_desmonte.drain(..) {
                let _ = tx.send(true);
            }
        }
    }
}

/// **O cão de guarda**: uma sessão transmitindo que para de bater o laço por mais de
/// [`SEM_BATIMENTO`] provavelmente está presa — e no Windows a causa mais provável é a que trava o
/// processo inteiro (o mutex global da API C da libdatachannel, `docs/divida-do-nucleo.md`). Não há
/// como matar uma thread; o que há é deixar escrito quando e qual, uma vez a cada 10 s.
fn vigiar_batimentos(tabela: &Tabela, fx: &mut EfeitosReais, agora: u64) {
    for s in tabela.sessoes() {
        // Só quem conectou bate o laço (a espera pela ordem, e depois a transmissão). Uma espera
        // em `hospedar` não bate — e uma espera cancelada vira `Encerrando` sem nunca ter batido:
        // era o alarme falso da primeira prova de mecanismo.
        if !s.conectou || !(s.estado == sessoes::Estado::Transmitindo || s.estado == sessoes::Estado::Encerrando) {
            continue;
        }
        let Some(c) = fx.controles.get_mut(&s.id) else {
            continue;
        };
        let batida = c.batimento.load(Ordering::Relaxed);
        let sem = agora.saturating_sub(batida);
        if sem >= SEM_BATIMENTO.as_millis() as u64
            && c.ultimo_alerta.map(|t| t.elapsed() >= Duration::from_secs(10)).unwrap_or(true)
        {
            c.ultimo_alerta = Some(Instant::now());
            registro::linha(format!(
                "[#{}] !! a thread da sessão não dá sinal há {:.1} s ({:?}) — pode estar presa",
                s.id,
                sem as f64 / 1000.0,
                s.estado
            ));
        }
        // Presa e com monitor virtual: o monitor sai pela chave (o ping global o manteria de pé). Com
        // prazo próprio, maior que o do alarme: na N = 8 de 15/09 a sessão viva ficou ~6,5 s dentro
        // do WGC (a troca da captura com monitores chegando), e soltar aos 5 s tirou cinco monitores
        // de sessões que transmitiam — e cada `REMOVE` fez o Windows mexer nos outros.
        if sem >= SEM_BATIMENTO_PARA_SOLTAR.as_millis() as u64 && c.monitor.is_some() && !c.soltou_pela_chave {
            let id = s.id;
            fx.soltar_pela_chave(id, "a sessão não dá sinal"); // i18n: fora (diário e caso interno)
        }
    }
}

/// O estado da janela, a partir da tabela — **só com o coordenador dono da tela**
/// (`Emissor::coordenador_na_tela`, conferido com o estado travado): ocioso depois de uma tela
/// estendida, ele não escreve por cima do caminho de uma sessão só (R10).
fn publicar(
    emissor: &Arc<Emissor>,
    tabela: &Tabela,
    resumos: &HashMap<Id, String>,
    som: &HashMap<Id, (bool, String)>,
) {
    let r = tabela.retrato();
    let fase = match r.fase {
        sessoes::Fase::Inicial => Fase::Inicial,
        sessoes::Fase::Esperando => Fase::Esperando,
        sessoes::Fase::Transmitindo => Fase::Transmitindo,
        sessoes::Fase::Encerrando => Fase::Encerrando,
    };
    let receptores: Vec<ReceptorNaTela> = r
        .receptores
        .iter()
        .map(|x| ReceptorNaTela {
            id: x.id,
            // No idioma da hora: a lista é republicada a cada relato das sessões.
            nome: if x.nome.is_empty() { idioma::t("outro aparelho").into() } else { x.nome.clone() },
            monitor: match (x.aguardando_a_velha, x.indice) {
                (true, _) => idioma::t("esperando a sessão anterior deste aparelho sair").into(),
                (false, Some(i)) => {
                    let p = PedidoDeMonitor::para_tela(x.tela, emissor.argumentos.fps, i, &x.nome, "");
                    idioma::tf("{} × {}, índice {}", &[&p.largura, &p.altura, &i])
                }
                (false, None) => String::new(),
            },
            // D2: quem recebe o som diz na própria linha.
            resumo: match (resumos.get(&x.id), x.com_o_som) {
                // A frase da câmera parada vai sozinha (a revisão do código de 22/09, 4).
                (Some(t), true) if !crate::regras_da_camera::e_texto_da_camera_parada(t) => idioma::tf("{} · com o som", &[t]),
                (Some(t), _) => t.clone(),
                (None, _) => String::new(),
            },
        })
        .collect();
    let mut e = emissor.estado();
    if !emissor.coordenador_na_tela() {
        return;
    }
    let voltou = e.fase != Fase::Inicial && fase == Fase::Inicial;
    if voltou {
        // A janela principal não emite mais: a tela R5 pode abrir (`dono_da_captura::QUEM_EMITE`).
        crate::dono_da_captura::soltar_da_janela();
    }
    let porta_da_espera = if r.pin.is_empty() { None } else { r.porta };
    let par = receptores.iter().map(|x| x.nome.clone()).collect::<Vec<_>>().join(", ");
    let resumo = if receptores.len() == 1 { receptores[0].resumo.clone() } else { String::new() };
    // Ativo: alguma sessão da rodada captura. O recusado é o de uma delas quando nenhuma subiu.
    let da_rodada = || som.iter().filter(|(id, _)| tabela.sessao(**id).is_some());
    let som_ativo = da_rodada().any(|(_, (ativo, _))| *ativo);
    let som_recusado = if som_ativo {
        String::new()
    } else {
        da_rodada().map(|(_, (_, r))| r.clone()).find(|r| !r.is_empty()).unwrap_or_default()
    };
    let endereco = r.porta.and_then(enderecos::para_digitar);
    let mudou = e.fase != fase
        || e.pin != r.pin
        || e.porta_da_espera != porta_da_espera
        || e.endereco != endereco
        || e.esperando_mais_um != r.esperando_mais_um
        || e.receptores != receptores
        || e.par != par
        || e.resumo != resumo
        || e.conselho != r.conselho
        || e.oferece_desparear != r.oferece_desparear
        || e.som_ativo != som_ativo
        || e.som_recusado != som_recusado;
    if !mudou {
        return;
    }
    e.fase = fase;
    e.pin = r.pin;
    e.porta_da_espera = porta_da_espera;
    e.endereco = endereco;
    e.esperando_mais_um = r.esperando_mais_um;
    e.receptores = receptores;
    e.par = par;
    e.resumo = resumo;
    e.conselho = r.conselho;
    e.oferece_desparear = r.oferece_desparear;
    e.som_ativo = som_ativo;
    e.som_recusado = som_recusado;
    // **A recarga adiada** (R10): um monitor ou uma câmera que entrou ou saiu durante a sessão (os
    // monitores virtuais entrando e saindo, inclusive) ficou para depois (`recarregar_fontes`). O
    // caminho de uma sessão a atende na volta ao início; aqui, também, fora da thread do coordenador.
    let adiada = voltou && std::mem::take(&mut e.recarga_adiada);
    if voltou {
        e.ha_pares_conhecidos = identidade::ha_pares_conhecidos();
    }
    e.versao += 1;
    drop(e);
    if adiada {
        emissor.recarregar_fontes_fora("a volta da tela estendida ao início"); // i18n: fora (diário e caso interno)
    }
}
