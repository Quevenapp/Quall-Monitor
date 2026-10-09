// Os identificadores e endereços de exemplos/fixtures são sintéticos; não identificam a bancada privada.
//! Como as peças se encaixam: descoberta → sinalização → pareamento → transporte.
//!
//! Este módulo existe para que as quatro cascas (SwiftUI, Compose, app do desktop, plugin de
//! OBS) não reimplementem a mesma sequência quatro vezes, cada uma com um bug diferente. A
//! casca cuida de captura, permissões e UI; a coreografia mora aqui.
//!
//! Duas funções, uma por papel:
//!
//! - [`hospedar`] — o **emissor**. Já está com o servidor de sinalização de pé e o mDNS
//!   anunciando; aceita um receptor, pareia, e é quem faz a oferta SDP.
//! - [`conectar`] — o **receptor**. Abre a sinalização no endereço que veio do mDNS **ou** que
//!   o usuário digitou (é a mesma função, de propósito: o fallback não é caminho de código
//!   separado que ninguém exercita), pareia e responde.

#![cfg(feature = "webrtc")]

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cancel::Cancelamento;
use crate::error::{Error, Result};
use crate::pairing::{PairOutcome, PairedPeers, Pairing, Pin, Role};
use crate::protocol::{
    papel_do_anfitriao_serve, papel_do_convidado_serve, Announcement, DeviceId, Papel,
};
use crate::signaling::{
    conferir_anuncio, connect_de, CausaDeRecusa, Link, RelatoDoEnlace, SignalMessage,
    SignalingServer,
};
use crate::track::{TrackConfig, TrackEmissor};
use crate::transport::{
    Delivery, ParDaSessao, PeerState, Session, TransportConfig, TransportEvent,
};

/// **Depois de quanto silêncio uma sessão de teleprompter é dada por caída.**
///
/// As duas pontas de um teleprompter mandam estado a cada segundo, sem exceção (o batimento de
/// `docs/contrato-teleprompter.md` §4). Diferente do vídeo — em que uma tela parada pode não
/// produzir quadro nenhum e é por isso que [`SessionConfig::silencio_do_caminho`] nasce desligado
/// —, aqui silêncio **é** defeito, e o núcleo pode ligar o detector sozinho. Cinco batimentos
/// perdidos; sem isto, o prompter sem rede ou o iPhone bloqueado só cairiam no `CONSENT_TIMEOUT`
/// de 30 s da libjuice, com o envio devolvendo sucesso o tempo todo.
pub const SILENCIO_DO_TELEPROMPTER: Duration = Duration::from_secs(5);

/// Quem é o outro lado, para o transporte (e dele para todo `Mensageiro`). O `device_id` veio no
/// `Hello`/`Welcome` cifrado e foi comparado à identidade confirmada pelo PAKE; é o que diz à réplica do teleprompter se o prompter
/// desta sessão é o da última vez (`docs/contrato-teleprompter.md` §11.2).
fn par_da_sessao(anuncio: &Announcement) -> ParDaSessao {
    ParDaSessao {
        id: anuncio.device_id.0.clone(),
        nome: anuncio.display_name.clone(),
    }
}

/// A entrega e o detector de silêncio que o papel pede. Um lugar só, para as duas pontas.
fn ajustar_ao_papel(cfg: &mut SessionConfig) {
    let Some(papel) = cfg.announcement.papel else {
        return;
    };
    if matches!(papel, Papel::Teleprompter | Papel::ControleRemoto) {
        // Quem decide o canal é quem oferece (`transport::Delivery::ReliableUnordered`). Em quem
        // conecta o campo não muda o canal — muda só a leitura de quem olhar este `TransportConfig`,
        // e por isso fica coerente dos dois lados.
        cfg.transport.delivery = Delivery::ReliableUnordered;
        if cfg.silencio_do_caminho.is_none() {
            cfg.silencio_do_caminho = Some(SILENCIO_DO_TELEPROMPTER);
        }
    }
}

/// O que a casca precisa fornecer para uma sessão.
pub struct SessionConfig {
    /// Quem sou eu, para o outro lado.
    pub announcement: Announcement,
    /// PIN explícito escolhe um novo pareamento. Vazio tenta retomada de vínculo v3 conhecido;
    /// primeiro pareamento e vínculos antigos exigem um novo PIN.
    pub pin: Option<Pin>,
    /// Aparelhos já pareados, lidos do armazenamento da plataforma.
    pub known: PairedPeers,
    pub transport: TransportConfig,
    /// Tracks de mídia que **este** aparelho vai emitir.
    ///
    /// Só vale em [`hospedar`]: quem oferece é quem declara as tracks, porque elas entram na
    /// oferta e o Quall não implementa renegociação. Em [`conectar`] o campo é ignorado, e as
    /// tracks que chegam saem por [`Session::proxima_track`].
    ///
    /// Vazio reproduz exatamente o M1: sessão só com canal de dados.
    pub tracks: Vec<TrackConfig>,
    /// Prazo total para tudo — aceitar, parear e o canal de dados abrir.
    pub timeout: Duration,
    /// O botão Cancelar da tela de espera. Ver [`Cancelamento`] e a dívida 10.
    ///
    /// `Default` nasce sem cancelamento pedido, então quem não usa não paga nada.
    pub cancelamento: Cancelamento,
    /// **Depois de quanto tempo sem nada chegar a sessão é dada por caída.** `None` (o padrão)
    /// desliga o detector.
    ///
    /// # O defeito que ele fecha, e ele está medido
    ///
    /// `docs/receptor-ios.md:243`: a mídia morreu aos 8,6 s e [`Ready::proximo_evento`] foi
    /// chamado a **20 Hz por mais de dez segundos** sem nunca devolver `Desconectou` nem
    /// `Falhou`. Não era defeito do detector: a sinalização continuava de pé pela Wi-Fi enquanto
    /// a mídia ia por um caminho morto, e o estado do ICE só muda no `CONSENT_TIMEOUT` da
    /// libjuice, que é 30 000 ms. Em produto isso é **desplugar o cabo = tela congelada com a
    /// sessão "saudável"**.
    ///
    /// Quem terminou aquela corrida foi um temporizador de silêncio que a casca iOS escreveu por
    /// conta própria (`SessaoDeRecepcao.swift`, 10 s sem quadro). Este campo é o mesmo
    /// mecanismo, um andar abaixo, para que as quatro cascas não o escrevam quatro vezes.
    ///
    /// # Encerra, não migra
    ///
    /// Não há renegociação nem ICE restart neste projeto (`session.rs`, `transport.rs`), então
    /// não existe "trocar de caminho": o conserto possível é **avisar**. O evento é
    /// [`EventoDeSessao::Desconectou`] — deliberadamente **não** um código novo, ver
    /// [`Ready::olhar_caminho`].
    ///
    /// # Por que não é ligado por padrão
    ///
    /// Porque "nada chegou" nem sempre é defeito. Um emissor que só manda não recebe pacote
    /// nenhum — para ele o detector nunca arma, e isso está tratado. O caso que **não** dá para
    /// distinguir daqui é o outro: uma tela parada. Um emissor de tela pode legitimamente não
    /// produzir quadro nenhum enquanto nada muda no vidro, e derrubar essa sessão seria trocar
    /// um defeito por outro pior. Ligar exige saber que a origem produz continuamente, e quem
    /// sabe isso é a casca, não o núcleo.
    ///
    /// Prazo curto demais é oscilação; o precedente medido é 10 s.
    pub silencio_do_caminho: Option<Duration>,
}

/// Sessão de pé.
pub struct Ready {
    pub session: Session,
    /// As tracks de saída, na mesma ordem de [`SessionConfig::tracks`]. Vazio no receptor.
    ///
    /// A casca **precisa** registrar [`TrackEmissor::ao_pedir_idr`] em cada uma antes do
    /// primeiro quadro. Ignorar o pedido é deixar o receptor sem imagem.
    pub tracks: Vec<TrackEmissor>,
    /// Quem é o outro lado.
    pub peer: Announcement,
    /// O que sobrou do pareamento. Se `novo`, a casca tem de persistir com
    /// [`PairedPeers::insert`] — senão o usuário digita PIN de novo na próxima vez.
    pub outcome: PairOutcome,
    /// A sinalização, ainda aberta. Fechar é com quem chama, depois de guardar o que precisa.
    pub link: Link,
    /// **Quantos candidatos caíram antes deste, sem derrubar a espera.**
    ///
    /// Zero é o caso normal. Diferente de zero quer dizer que alguém conectou e sumiu no meio —
    /// o receptor que desiste, o app que fecha, o scanner de porta da LAN — e que a espera
    /// sobreviveu a isso. Ver [`hospedar`] e a nota de 01/09/2026 em
    /// [`crate::signaling::SignalingServer::aceitar_um`].
    ///
    /// Existe para que o conserto **apareça no registro**: um descarte que ninguém conta é como
    /// o defeito volta a viver escondido.
    pub descartados: u32,
    /// Trava de [`Ready::proximo_evento`]: uma vez caída, a sessão não volta.
    caiu: Option<EventoDeSessao>,
    /// Ver [`SessionConfig::silencio_do_caminho`]. `None` desliga o detector.
    silencio_do_caminho: Option<Duration>,
    /// O relato mais recente que o outro lado mandou, ainda não consumido. Ver
    /// [`Ready::relato_do_enlace`].
    ultimo_relato: Option<RelatoDoEnlace>,
    /// Quem atende a porta enquanto a sessão dura. Ver [`Ready::atender_enquanto_dura`].
    atendente: Option<Atendente>,
}

/// O que aconteceu com a sessão **depois** que ela subiu.
///
/// Existe por causa da dívida 20, e ela é boa notícia: as dívidas 5 e 13 estavam formuladas como
/// falta de protocolo — "o emissor não sabe quando a sessão cai", "o emissor fica 30 segundos
/// cego" — e não eram. O `Bye` que o receptor manda ao sair **já chegava** ao emissor: o
/// [`Ready::link`] fica vivo dentro da sessão e simplesmente ninguém fazia `poll()` nele.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventoDeSessao {
    /// Nada por enquanto. É o caso normal, não erro.
    Nenhum,
    /// A outra ponta saiu.
    Desconectou,
    /// O transporte falhou.
    Falhou,
}

impl Ready {
    /// Olha se a sessão caiu, esperando até `limite`.
    ///
    /// # Por que isto resolve os 30 segundos de cegueira
    ///
    /// O `CONSENT_TIMEOUT` do libjuice é 30 000 ms: até lá, o transporte ainda diz que está
    /// conectado e `enviar_quadro` continua devolvendo sucesso com o receptor morto. São ~900
    /// quadros capturados, encodados em hardware e empacotados para o vazio.
    ///
    /// A sinalização sabe antes, e sabe de graça. Duas fontes, nesta ordem:
    ///
    /// 1. **A sinalização** — um `Bye`, ou o TCP fechando. Chega em milissegundos.
    /// 2. **O transporte** — `Disconnected`, `Failed`, `Closed` ou o canal de dados fechando. É a
    ///    rede de segurança para quando o outro lado morre sem despedida e o TCP demora a notar.
    ///
    /// # O `Bye` é confiável nesta fronteira, e vale dizer por quê
    ///
    /// Durante a negociação um `Bye` significa só "acabei com a sinalização" — foi o que quebrou
    /// o primeiro ensaio na bancada, e [`negociar`] o ignora de propósito. Depois que a sessão
    /// sobe é diferente: na superfície C **não existe** forma de fechar a sinalização sem
    /// encerrar a sessão — `quall_session_close` faz as duas coisas —, então um `Bye` que chega
    /// aqui só pode ser a outra ponta indo embora.
    ///
    /// Quem programa em Rust direto contra o núcleo pode fechar o [`Ready::link`] por conta
    /// própria e mantendo a sessão viva (a sonda faz isso). Nesse caso, este detector acusa a
    /// queda cedo demais — e é por isso que a regra está escrita aqui, e não subentendida.
    pub fn proximo_evento(&mut self, limite: Duration) -> EventoDeSessao {
        if let Some(caiu) = self.caiu {
            return caiu;
        }
        let fim = Instant::now() + limite;
        loop {
            if let Some(evento) = self.olhar_transporte() {
                self.caiu = Some(evento);
                return evento;
            }
            if let Some(evento) = self.olhar_caminho() {
                self.caiu = Some(evento);
                return evento;
            }
            if let Some(evento) = self.olhar_sinalizacao(fim) {
                self.caiu = Some(evento);
                return evento;
            }
            if Instant::now() >= fim {
                return EventoDeSessao::Nenhum;
            }
        }
    }

    /// Drena o que o transporte tem a dizer, sem esperar.
    ///
    /// **Sem esperar era o que a prosa dizia e não era o que o código fazia.** Ele pedia 1 ms por
    /// volta do laço de drenagem — inclusive na última, a que sempre vem vazia. No Windows,
    /// `recv_timeout(1ms)` acorda no tique de ~15,6 ms, e esse 1 ms era metade dos 29,43 ms que
    /// `proximo_evento(Duration::ZERO)` custava lá. Agora é `Duration::ZERO`, que o transporte
    /// traduz para `try_recv`.
    ///
    /// O que se perde: um evento que chegue **dentro** do milissegundo seguinte não é mais
    /// apanhado nesta volta. Com `limite > 0` ele é apanhado na volta seguinte, ≤ 10 ms depois
    /// ([`FATIA_DE_ESCUTA`]); com `limite` zero, quem pediu zero pediu exatamente isto. Contra os
    /// 30 s do `CONSENT_TIMEOUT` do libjuice, que é a alternativa a este detector, não muda nada.
    fn olhar_transporte(&mut self) -> Option<EventoDeSessao> {
        let mut caiu = None;
        while let Some(evento) = self.session.next_event(Duration::ZERO) {
            match evento {
                TransportEvent::State(
                    PeerState::Disconnected | PeerState::Closed | PeerState::Failed,
                )
                | TransportEvent::ChannelClosed => caiu = Some(EventoDeSessao::Desconectou),
                TransportEvent::Failed(_) => caiu = Some(EventoDeSessao::Falhou),
                _ => {}
            }
        }
        caiu
    }

    /// **O caminho da mídia emudeceu?** A terceira fonte do detector de queda, e a única que
    /// olha para a mídia em vez de para o estado que a descreve.
    ///
    /// Desligada por padrão — ver [`SessionConfig::silencio_do_caminho`], que explica também por
    /// quê.
    ///
    /// # Duas guardas, e as duas importam
    ///
    /// 1. **Sem prazo configurado, nada acontece.** Quem não pediu não paga, e nenhuma casca
    ///    muda de comportamento por esta linha existir.
    /// 2. **`silencio_da_midia()` devolvendo `None` não arma o detector.** `None` é "nada chegou
    ///    nesta sessão desde que ela subiu", que é o estado permanente de quem só emite — e é
    ///    também o estado dos primeiros instantes de quem recebe. Armar ali transformaria
    ///    "a sessão ainda não começou" em "a sessão caiu", que é o erro que este projeto proíbe
    ///    em instrumento. Quem nunca recebeu nada e devia ter recebido é problema do
    ///    [`SessionConfig::timeout`], não deste detector.
    ///
    /// # Por que [`EventoDeSessao::Desconectou`] e não um código novo
    ///
    /// Um valor novo no enum atravessaria a fronteira C como um `QuallSessionEvent` que
    /// **nenhuma casca conhece** — e todas elas comparam com `==` contra `DISCONNECTED` e
    /// `FAILED`. Um código novo compilaria em todas e seria ignorado por todas: a tela ficaria
    /// congelada exatamente como antes, agora com um instrumento que relata para ninguém. Entre
    /// um código preciso que ninguém escuta e um código aproximado que todos já tratam, o
    /// segundo é o que conserta o defeito.
    ///
    /// O custo dessa escolha é honesto e fica registrado: a casca vai dizer "o outro lado saiu"
    /// quando a causa foi "o caminho da mídia morreu". Distinguir os dois é uma mudança de
    /// fronteira mais uma linha por casca, e não é desta frente.
    fn olhar_caminho(&mut self) -> Option<EventoDeSessao> {
        let prazo = self.silencio_do_caminho?;
        let silencio = self.session.silencio_da_midia()?;
        (silencio >= prazo).then_some(EventoDeSessao::Desconectou)
    }

    /// Uma espiada na sinalização, no máximo até `fim`.
    fn olhar_sinalizacao(&mut self, fim: Instant) -> Option<EventoDeSessao> {
        let fatia = fim
            .saturating_duration_since(Instant::now())
            .min(FATIA_DE_ESCUTA);
        let lido = self.link.poll_por(fatia);
        if let Ok(Some(SignalMessage::Enlace(r))) = &lido {
            // Guardado, e não descartado: quem chama `proximo_evento` pode não ser quem lê o
            // relato, e os dois disputam o mesmo socket. Perder o relato porque o laço de eventos
            // o leu primeiro seria um defeito que só aparece na casca que faz as duas coisas.
            self.ultimo_relato = Some(*r);
        }
        match lido {
            Ok(Some(SignalMessage::Bye { .. })) => Some(EventoDeSessao::Desconectou),
            Ok(Some(SignalMessage::Error { .. })) => Some(EventoDeSessao::Falhou),
            // Qualquer outra mensagem depois da negociação é ruído; ignorar é melhor que
            // derrubar uma sessão que está funcionando.
            Ok(_) => None,
            // O TCP caiu, ou veio um quadro de Close: a outra ponta foi embora sem despedida.
            Err(Error::Closed) => Some(EventoDeSessao::Desconectou),
            Err(_) => Some(EventoDeSessao::Falhou),
        }
    }

    /// Manda ao emissor o que este receptor está vendo do enlace. Chame do **receptor**.
    ///
    /// Escreve na hora, sem passar pela fila de saída, e por isso tem de ser chamada da mesma
    /// thread que faz [`Ready::proximo_evento`] — a regra que o [`Link`] inteiro carrega.
    ///
    /// Falhar aqui **não** derruba nada: um emissor de versão antiga simplesmente não escuta, e
    /// um socket que morreu vai ser notado pelo detector de queda que já existe. O receptor não
    /// pode ficar pior por causa de um relato que não saiu.
    pub fn relatar_enlace(&mut self, relato: RelatoDoEnlace) -> Result<()> {
        self.link.send(&SignalMessage::Enlace(relato))
    }

    /// Consome o relato mais recente do outro lado, se houver. Chame do **emissor**.
    ///
    /// `fatia` é quanto esperar na sinalização antes de desistir; `Duration::ZERO` não bloqueia.
    ///
    /// # Só o mais recente, e isso é a política certa
    ///
    /// Se dois relatos chegarem entre duas chamadas, o mais velho é **descartado**. Um relato é
    /// uma janela fechada de meio segundo atrás; agir sobre a de um segundo atrás depois de já
    /// ter a de meio é agir sobre o passado. A alternativa — enfileirar — daria ao controlador
    /// uma fila de decisões atrasadas, que é a receita conhecida de oscilação em laço fechado.
    ///
    /// Um `Bye` ou um `Error` que chegue nesta leitura **é** registrado: a trava de queda é a
    /// mesma de [`Ready::proximo_evento`], então uma casca que só chame esta função continua
    /// sabendo que a sessão caiu.
    ///
    /// # Chame de uma thread que pode bloquear, e o prazo é o seu
    ///
    /// Ao contrário de [`Ready::proximo_evento`], **o prazo daqui não é limitado por
    /// [`FATIA_DE_ESCUTA`]**. Aquele teto existe porque `proximo_evento` é chamado do laço de
    /// captura, e uma espera longa lá viraria atraso no caminho do quadro; esta função é para uma
    /// thread própria, e cortar o prazo dela a 10 ms transforma o laço de quem chama num giro de
    /// 100 Hz.
    ///
    /// **Isto foi escrito com o corte e medido assim.** O `.min(FATIA_DE_ESCUTA)` estava aqui, o
    /// comentário da casca dizia "bloqueia até 200 ms", e o laço da taxa no `MirrorService` girava
    /// a cada ~10 ms durante as corridas de A/B — gastando CPU do emissor sem ganhar nada. O A/B
    /// mediu o braço ligado **com** esse custo e mesmo assim ele ganhou, então o número publicado
    /// é piso, não teto. Ver `docs/taxa-que-escuta.md` §6.
    pub fn relato_do_enlace(&mut self, fatia: Duration) -> Option<RelatoDoEnlace> {
        if self.caiu.is_none() {
            match self.link.poll_por(fatia) {
                Ok(Some(SignalMessage::Enlace(r))) => self.ultimo_relato = Some(r),
                Ok(Some(SignalMessage::Bye { .. })) => {
                    self.caiu = Some(EventoDeSessao::Desconectou)
                }
                Ok(Some(SignalMessage::Error { .. })) => self.caiu = Some(EventoDeSessao::Falhou),
                Ok(_) => {}
                Err(Error::Closed) => self.caiu = Some(EventoDeSessao::Desconectou),
                Err(_) => self.caiu = Some(EventoDeSessao::Falhou),
            }
        }
        self.ultimo_relato.take()
    }

    /// Atende novos candidatos sem alterar a sessão ativa. Depois do Probe efêmero,
    /// devolve somente fechamento genérico 1013; não lê identidade ou anúncio pessoal.
    pub fn atender_enquanto_dura(&mut self, servidor: Arc<SignalingServer>, eu: Announcement) {
        self.atendente = None; // o anterior para e é esperado aqui
        self.atendente = Some(Atendente::iniciar(
            servidor,
            eu,
            self.peer.device_id.clone(),
        ));
    }

    /// Quantos candidatos o atendente respondeu durante a sessão (ocupado, papel, versão).
    pub fn atendidos_durante_a_sessao(&self) -> u32 {
        self.atendente
            .as_ref()
            .map(|a| a.atendidos.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

/// A thread que atende a porta de uma sessão de pé. Ver [`Ready::atender_enquanto_dura`].
struct Atendente {
    parar: Cancelamento,
    atendidos: Arc<std::sync::atomic::AtomicU32>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Quanto o atendente espera o `Hello` de quem bateu. Curto: quem bate e não fala não segura a
/// porta — e o `Drop` do atendente espera no máximo isto mais uma fatia de `accept`.
const PRAZO_DO_HELLO_NO_ATENDENTE: Duration = Duration::from_millis(1500);

impl Atendente {
    fn iniciar(servidor: Arc<SignalingServer>, eu: Announcement, par: DeviceId) -> Atendente {
        let parar = Cancelamento::novo();
        let atendidos = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let thread = {
            let parar = parar.clone();
            let atendidos = Arc::clone(&atendidos);
            std::thread::Builder::new()
                .name("quall-atendente".into())
                .spawn(move || {
                    while !parar.cancelado() {
                        match servidor.accept_cancelavel(Duration::from_millis(200), &parar) {
                            Ok(Some(mut link)) => {
                                atender_um(&mut link, &eu, &par);
                                atendidos.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(None) => {}
                            Err(Error::Cancelled) => break,
                            // Um candidato que cai no handshake não é motivo para parar de
                            // atender: o próximo pode ser o controle de volta.
                            Err(_) => std::thread::sleep(Duration::from_millis(20)),
                        }
                    }
                })
                .ok()
        };
        Atendente {
            parar,
            atendidos,
            thread,
        }
    }
}

impl Drop for Atendente {
    fn drop(&mut self) {
        self.parar.cancelar();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Responde **um** candidato que bateu durante a sessão, e fecha.
fn atender_um(link: &mut Link, _eu: &Announcement, _par: &DeviceId) {
    let fim = Instant::now() + PRAZO_DO_HELLO_NO_ATENDENTE;
    loop {
        match link.poll_por(Duration::from_millis(50)) {
            Ok(Some(SignalMessage::Pair(_))) => {
                // Sem autenticar uma segunda sessão ou ler qualquer identidade pessoal.
                link.close_busy();
                return;
            }
            Ok(Some(_)) | Err(_) => {
                link.close("protocolo inválido");
                return;
            }
            Ok(None) if Instant::now() >= fim => {
                link.close("prazo encerrado");
                return;
            }
            Ok(None) => {}
        }
    }
}

/// Teto da espera de uma espiada na sinalização.
///
/// A casca chama [`Ready::proximo_evento`] do laço de captura, e uma fatia longa aqui viraria
/// atraso no caminho do quadro.
const FATIA_DE_ESCUTA: Duration = Duration::from_millis(10);

/// Fatia de espera dos laços. Curta para que sinalização e transporte se revezem sem que
/// nenhum dos dois fique parado esperando o outro.
const FATIA: Duration = Duration::from_millis(10);

/// **Um candidato que cai é acidente; um candidato que é recusado é decisão.** Só o primeiro faz
/// [`hospedar`] voltar a esperar.
///
/// # A linha, e por que ela fica exatamente aqui
///
/// **Acidente** é a conexão morrer: `Io`, `Closed`, e o `Signaling` que embrulha tanto o
/// `WSAECONNRESET` medido em 01/09/2026 quanto o `Bye` de quem desistiu. Nenhum deles diz nada
/// sobre esta sessão — dizem que *aquele* candidato foi embora. Insistir em morrer junto com ele
/// é o defeito que esta função existe para fechar.
///
/// **Decisão** é tudo o mais, e cada uma tem de terminar a hospedagem:
///
/// - `WrongPin`, `NeedsPin` e `Pairing` — **é aqui que a segurança do PIN mora**. O PIN de seis
///   dígitos é segurado por *uma tentativa por conexão*, e uma tentativa por conexão só vale
///   alguma coisa se a conexão levar o PIN embora com ela. Quem erra o PIN derruba a hospedagem,
///   a casca volta ao início, e o próximo PIN é outro. Um laço que engolisse recusa de pareamento
///   transformaria seis dígitos num alvo de força bruta na LAN.
///
///   Repare no que **não** muda com o conserto: quem cai antes de tentar parear não gasta
///   tentativa nenhuma e também não aprende nada. Reesperar depois de um acidente não devolve
///   fôlego a um atacante — devolve a porta a quem ia usá-la.
/// - `Protocol` — versão incompatível e mensagem fora de ordem. Reesperar em silêncio apagaria
///   o diagnóstico de incompatibilidade. Antes de autenticar, o fechamento não inclui payload.
/// - `Timeout` e `Cancelled` — são os dois limites do próprio laço. É o que garante que ele
///   termina.
fn e_acidente_do_candidato(e: &Error) -> bool {
    matches!(e, Error::Io(_) | Error::Closed | Error::Signaling(_))
}

/// Os papéis são parâmetros funcionais públicos do protocolo PAKE, não identidades.
fn codigo_do_papel(papel: Option<Papel>) -> Result<u8> {
    match papel {
        None => Ok(0),
        Some(Papel::Teleprompter) => Ok(1),
        Some(Papel::ControleRemoto) => Ok(2),
        Some(Papel::Desconhecido) => Err(Error::Protocol("papel desconhecido".into())),
    }
}

fn papel_do_codigo(codigo: u8) -> Result<Option<Papel>> {
    match codigo {
        0 => Ok(None),
        1 => Ok(Some(Papel::Teleprompter)),
        2 => Ok(Some(Papel::ControleRemoto)),
        _ => Err(Error::Protocol("papel funcional inválido".into())),
    }
}

fn conferir_identidade_autenticada(
    anuncio: &Announcement,
    resultado: &PairOutcome,
    papel: u8,
) -> Result<()> {
    conferir_anuncio(anuncio)?;
    if anuncio.device_id != resultado.peer || codigo_do_papel(anuncio.papel)? != papel {
        return Err(Error::Protocol(
            "anúncio diverge da identidade ou papel autenticado".into(),
        ));
    }
    Ok(())
}

/// A máquina PAKE contém somente material efêmero público antes de confirmar ambas as pontas.
/// Toda identidade/SDP/ICE/relato subsequente passa pelo Link cifrado, sem modo legado.
fn parear_link(
    link: &mut Link,
    cfg: &SessionConfig,
    role: Role,
    prazo: Instant,
) -> Result<(PairOutcome, u8)> {
    conferir_anuncio(&cfg.announcement)?;
    let mut maquina = Pairing::new_with_store(
        role,
        cfg.announcement.device_id.clone(),
        cfg.pin.clone(),
        &cfg.known,
    )?;
    maquina.bind_local_role(codigo_do_papel(cfg.announcement.papel)?)?;
    let auth_prazo = prazo.min(Instant::now() + Duration::from_secs(30));
    let mut frames = 0u8;
    if role == Role::Guest {
        link.send(&SignalMessage::Pair(maquina.open()?))?;
    }
    loop {
        let passo = (|| -> Result<Option<crate::pairing::Step>> {
            restante(auth_prazo, &cfg.cancelamento)?;
            match link.poll()? {
                Some(SignalMessage::Pair(quadro)) => {
                    frames = frames.saturating_add(1);
                    if frames > 12 {
                        return Err(Error::Protocol("excesso de quadros de autenticação".into()));
                    }
                    // Os hints públicos só podem recusar uma rota, nunca conceder identidade
                    // ou privilégios. O papel será novamente comparado após autenticação AEAD.
                    if let crate::pairing::PairFrame::Probe { guest_role, .. } = &quadro {
                        if role == Role::Host
                            && papel_do_convidado_serve(
                                cfg.announcement.papel,
                                papel_do_codigo(*guest_role)?,
                            )
                            .is_err()
                        {
                            link.close_role();
                            return Err(Error::Signaling(
                                "papel recusado antes do pareamento".into(),
                            ));
                        }
                    }
                    if let crate::pairing::PairFrame::Challenge { host_role, .. } = &quadro {
                        if role == Role::Guest {
                            papel_do_anfitriao_serve(
                                cfg.announcement.papel,
                                papel_do_codigo(*host_role)?,
                            )
                            .map_err(Error::Protocol)?;
                        }
                    }
                    Ok(Some(maquina.step(quadro)?))
                }
                Some(_) => Err(Error::Protocol(
                    "sinalização fora do PAKE antes da autenticação".into(),
                )),
                None => Ok(None),
            }
        })();
        match passo {
            Ok(Some(passo)) => {
                if let Some(resposta) = passo.reply {
                    if let Err(e) = link.send(&SignalMessage::Pair(resposta)) {
                        return Err(erro_da_tentativa(&maquina, role, e));
                    }
                }
                if let Some(resultado) = passo.done {
                    let papel = maquina.peer_role()?;
                    link.enable_secure(maquina.take_secure_channel()?)?;
                    return Ok((resultado, papel));
                }
            }
            Ok(None) => {}
            Err(e) => {
                link.close("pareamento recusado");
                return Err(erro_da_tentativa(&maquina, role, e));
            }
        }
    }
}

fn erro_da_tentativa(maquina: &Pairing, role: Role, e: Error) -> Error {
    // Guest pode detectar PIN incorreto em KE2 e fechar sem KE3. A tentativa já foi consumida.
    if role == Role::Host
        && maquina.attempt_started()
        && matches!(
            e,
            Error::Io(_) | Error::Closed | Error::Signaling(_) | Error::Timeout(_)
        )
    {
        Error::Pairing("tentativa interrompida; gere outro PIN".into())
    } else if role == Role::Host && !maquina.attempt_started() && matches!(e, Error::Timeout(_)) {
        // Probe/Challenge públicos não consumiram um palpite. A espera global continua limitada.
        Error::Signaling("candidato não completou a abertura de autenticação".into())
    } else {
        e
    }
}

/// Aceita **um** candidato e o leva até o fim do pareamento. Nada de mídia nasce aqui — é o que
/// torna a repetição barata e sem vazamento. Ver [`hospedar`].
///
/// `Ok(None)` é **um candidato recusado por papel ou que desistiu por papel** — e a espera
/// continua. Ver `docs/contrato-teleprompter.md` §2: ele não chegou a tentar PIN nenhum, então
/// não há tentativa gasta a proteger, e derrubar a espera trocaria o PIN na tela de quem estava
/// para digitá-lo.
fn abrir_candidato(
    servidor: &SignalingServer,
    cfg: &SessionConfig,
    prazo: Instant,
) -> Result<Option<(Link, Announcement, PairOutcome)>> {
    let mut link = servidor
        .accept_cancelavel(restante(prazo, &cfg.cancelamento)?, &cfg.cancelamento)?
        .ok_or_else(|| Error::Timeout("nenhum receptor conectou".into()))?;

    let (resultado, papel_autenticado) = parear_link(&mut link, cfg, Role::Host, prazo)?;
    let par = loop {
        match link.poll()? {
            Some(SignalMessage::Hello { announcement }) => {
                conferir_identidade_autenticada(&announcement, &resultado, papel_autenticado)?;
                if let Err(motivo) =
                    papel_do_convidado_serve(cfg.announcement.papel, announcement.papel)
                {
                    let _ = link.send(&SignalMessage::Error {
                        motivo,
                        causa: CausaDeRecusa::PapelIncompativel,
                    });
                    link.close("papel incompatível");
                    return Ok(None);
                }
                link.send(&SignalMessage::Welcome {
                    announcement: cfg.announcement.clone(),
                })?;
                break announcement;
            }
            Some(outra) => {
                return Err(Error::Protocol(format!(
                    "esperava Hello cifrado, veio {outra:?}"
                )))
            }
            None => {
                restante(prazo, &cfg.cancelamento)?;
            }
        }
    };

    Ok(Some((link, par, resultado)))
}

pub fn hospedar(servidor: &SignalingServer, cfg: SessionConfig) -> Result<Ready> {
    let mut cfg = cfg;
    ajustar_ao_papel(&mut cfg);
    let prazo = Instant::now() + cfg.timeout;
    let descartados_ao_abrir = servidor.descartados();
    let mut caidos: u32 = 0;

    // 1 e 2. Aceitar, apresentar-se e parear. **Repetível de propósito**, e só isto: enquanto
    //    não se passa daqui, não existe `Session`, não existe `Track` e não há o que vazar por
    //    volta — que é a objeção registrada (dívida 21) contra laço de tentativas na casca. O
    //    prazo total e o cancelamento continuam sendo os dois únicos limites.
    let (mut link, par, resultado) = loop {
        match abrir_candidato(servidor, &cfg, prazo) {
            Ok(Some(pronto)) => break pronto,
            // Recusado por papel: conta como candidato que caiu, e a espera segue.
            Ok(None) => {
                caidos = caidos.saturating_add(1);
                continue;
            }
            Err(e) if e_acidente_do_candidato(&e) => {
                caidos = caidos.saturating_add(1);
                continue;
            }
            Err(e) => return Err(e),
        }
    };

    // 3. Transporte. As tracks nascem junto com o canal de dados, **antes** da oferta: o que
    //    não está na oferta só entra com renegociação, que o Quall não implementa.
    //
    //    **Daqui para baixo não se repete nada.** Uma falha depois desta linha já custou uma
    //    `Session` e as `Track`s dela, e reesperar aqui as vazaria por volta.
    let (mut sessao, tracks) = Session::offerer_com_tracks(&cfg.transport, &cfg.tracks)?;
    sessao.definir_par(par_da_sessao(&par));
    negociar(&mut link, &mut sessao, prazo, &cfg.cancelamento)?;

    Ok(Ready {
        session: sessao,
        tracks,
        peer: par,
        outcome: resultado,
        link,
        descartados: caidos
            .saturating_add(servidor.descartados().saturating_sub(descartados_ao_abrir)),
        caiu: None,
        silencio_do_caminho: cfg.silencio_do_caminho,
        ultimo_relato: None,
        atendente: None,
    })
}

/// Lado do receptor: conecta, pareia e responde.
///
/// `destino` vem do mDNS ([`crate::discovery::DiscoveredDevice::endpoint`]) ou do que o usuário
/// digitou ([`crate::discovery::endereco_manual`]).
pub fn conectar(destino: std::net::SocketAddr, cfg: SessionConfig) -> Result<Ready> {
    let mut cfg = cfg;
    ajustar_ao_papel(&mut cfg);
    let prazo = Instant::now() + cfg.timeout;

    // **A sinalização sai pela mesma interface que a mídia.** Prender só a mídia deixaria o TCP
    // sair por onde a tabela de rotas mandasse — e com vários Android ancorados, cujas sub-redes
    // podem coincidir, isso deixa de ter resposta. Ver `signaling::connect_de`.
    let mut link = connect_de(
        destino,
        restante(prazo, &cfg.cancelamento)?,
        cfg.transport.origem()?,
        &cfg.cancelamento,
    )?;
    let (resultado, papel_autenticado) = parear_link(&mut link, &cfg, Role::Guest, prazo)?;
    link.send(&SignalMessage::Hello {
        announcement: cfg.announcement.clone(),
    })?;
    let par = loop {
        match link.poll()? {
            Some(SignalMessage::Welcome { announcement }) => {
                conferir_identidade_autenticada(&announcement, &resultado, papel_autenticado)?;
                if let Err(motivo) =
                    papel_do_anfitriao_serve(cfg.announcement.papel, announcement.papel)
                {
                    link.close(&motivo);
                    return Err(Error::Protocol(motivo));
                }
                break announcement;
            }
            Some(SignalMessage::Error { motivo, causa }) => return Err(causa.erro(motivo)),
            Some(SignalMessage::Bye { motivo }) => return Err(Error::Signaling(motivo)),
            Some(outra) => {
                return Err(Error::Protocol(format!(
                    "esperava Welcome cifrado, veio {outra:?}"
                )))
            }
            None => {
                restante(prazo, &cfg.cancelamento)?;
            }
        }
    };

    let mut sessao = Session::answerer(&cfg.transport)?;
    sessao.definir_par(par_da_sessao(&par));
    negociar(&mut link, &mut sessao, prazo, &cfg.cancelamento)?;

    Ok(Ready {
        session: sessao,
        // Quem responde não declara tracks: as do outro lado chegam pela oferta e saem por
        // `Session::proxima_track`.
        tracks: Vec::new(),
        peer: par,
        outcome: resultado,
        link,
        // Quem conecta não espera candidato nenhum: só o anfitrião pode descartar.
        descartados: 0,
        caiu: None,
        silencio_do_caminho: cfg.silencio_do_caminho,
        ultimo_relato: None,
        atendente: None,
    })
}

/// Bombeia SDP e candidatos entre a sinalização e o transporte até o canal de dados abrir.
///
/// É o mesmo laço dos dois lados. A assimetria toda está em quem criou a sessão como `offerer`.
fn negociar(
    link: &mut Link,
    sessao: &mut Session,
    prazo: Instant,
    cancelar: &Cancelamento,
) -> Result<()> {
    let mut canal_abriu = false;
    let mut conectou = false;
    // Depois que a outra ponta encerra a sinalização, escrever nela só produz erro. Parar de
    // escrever não atrapalha: se ela saiu, é porque já tem o que precisava.
    let mut sinalizacao_encerrou = false;

    loop {
        restante(prazo, cancelar)?;

        // Do transporte para a rede. Os eventos vêm das threads da libdatachannel.
        while let Some(evento) = sessao.next_event(FATIA) {
            match evento {
                TransportEvent::LocalDescription { kind, sdp } => {
                    if !sinalizacao_encerrou {
                        link.send(&SignalMessage::Description { kind, sdp })?;
                    }
                }
                TransportEvent::LocalCandidate { candidate, mid } => {
                    if !sinalizacao_encerrou {
                        link.send(&SignalMessage::Candidate { candidate, mid })?;
                    }
                }
                TransportEvent::ChannelOpen => canal_abriu = true,
                TransportEvent::State(PeerState::Connected) => conectou = true,
                // `NoRoute` e não `Transport`: é o caso mais provável do produto — permissão de
                // Rede Local negada no iOS, isolamento de AP, Wi-Fi de hóspede — e a casca tem o
                // que dizer ao usuário. O texto continua o mesmo, com o mesmo prefixo, para não
                // quebrar quem casa por string enquanto migra para o código de status.
                TransportEvent::State(PeerState::Failed) => {
                    return Err(Error::NoRoute(
                        "o ICE não achou caminho entre os dois aparelhos".into(),
                    ))
                }
                TransportEvent::State(PeerState::Closed) => {
                    return Err(Error::Transport("a conexão fechou durante a oferta".into()))
                }
                TransportEvent::Failed(motivo) => return Err(Error::Transport(motivo)),
                _ => {}
            }
        }

        // Da rede para o transporte. Com a sinalização já encerrada não há o que ler; só resta
        // esperar o transporte terminar de fechar sozinho.
        if sinalizacao_encerrou {
            if canal_abriu && conectou {
                return Ok(());
            }
            continue;
        }
        match link.poll() {
            Ok(Some(SignalMessage::Description { kind, sdp })) => {
                sessao.set_remote_description(&kind, &sdp)?;
            }
            Ok(Some(SignalMessage::Candidate { candidate, mid })) => {
                // Candidato recusado não derruba a sessão: é comum chegar um candidato de uma
                // interface que este lado não alcança, e o ICE segue com os outros.
                let _ = sessao.add_remote_candidate(&candidate, &mid);
            }
            Ok(Some(SignalMessage::CandidatesDone)) | Ok(None) => {}
            Ok(Some(SignalMessage::Error { motivo, .. })) => {
                return Err(Error::Signaling(format!("a outra ponta recusou: {motivo}")))
            }
            // O `Bye` da outra ponta durante a negociação **não** é motivo para desistir.
            //
            // Foi o que quebrou o primeiro ensaio na bancada: os dois canais de dados não abrem
            // no mesmo instante, e o lado que abriu primeiro fechava a sinalização enquanto o
            // outro ainda esperava o próprio `ChannelOpen` — que chegaria de qualquer jeito,
            // porque o SCTP já estava de pé. Desistir aqui é confundir "a sinalização acabou"
            // com "a conexão falhou". A sinalização é descartável depois que o ICE fecha; a
            // mídia é P2P e não depende dela.
            Ok(Some(SignalMessage::Bye { .. })) | Err(Error::Closed) => {
                sinalizacao_encerrou = true;
            }
            Ok(Some(outra)) => {
                return Err(Error::Protocol(format!(
                    "mensagem inesperada na negociação: {outra:?}"
                )))
            }
            Err(e) => return Err(e),
        }

        if canal_abriu && conectou {
            return Ok(());
        }
    }
}

/// Quanto falta do prazo, ou erro se acabou — ou se a casca pediu para cancelar.
///
/// Os dois motivos de parar moram juntos porque são consultados no mesmo lugar: toda espera do
/// núcleo passa por aqui, e é isso que faz o cancelamento chegar a todos os laços sem um `if`
/// espalhado por cada um.
fn restante(prazo: Instant, cancelar: &Cancelamento) -> Result<Duration> {
    if cancelar.cancelado() {
        return Err(Error::Cancelled);
    }
    let agora = Instant::now();
    if agora >= prazo {
        return Err(Error::Timeout("a sessão não fechou dentro do prazo".into()));
    }
    Ok(prazo - agora)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::PairFrame;
    use crate::protocol::{Capabilities, DeviceId, PROTOCOL_VERSION};
    // Só os testes abrem uma sinalização "na mão": o produto entra por `conectar`.
    use crate::signaling::connect;
    use std::thread;

    fn anuncio(id: &str, nome: &str) -> Announcement {
        Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.into()),
            display_name: nome.into(),
            capabilities: Capabilities {
                screen_source: true,
                camera_source: true,
                sink: true,
            },
            screen: None,
            papel: None,
        }
    }

    /// Fecha uma sessão inteira — sinalização, pareamento por PIN e canal de dados — entre duas
    /// threads no mesmo processo.
    ///
    /// **Isto é loopback, não é a bancada.** Prova a coreografia e o protocolo. Latência de LAN
    /// e travessia entre máquinas se medem com a sonda, no MacBook e no Dell.
    #[test]
    fn hospedar_e_conectar_fecham_a_sessao_com_pin() {
        let pin = Pin::parse("424242").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-1", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let receptor = conectar(
            destino,
            SessionConfig {
                // A tela do receptor atravessa o aperto de mão (a tela estendida do Mac depende disso).
                announcement: Announcement {
                    screen: crate::protocol::Screen::nova(1125, 2436),
                    ..anuncio("receptor-1", "Receptor")
                },
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("receptor fecha a sessão");

        let mut emissor = emissor.join().expect("thread do emissor").expect("emissor");

        assert_eq!(emissor.peer.device_id.0, "receptor-1");
        assert_eq!(receptor.peer.device_id.0, "emissor-1");
        assert_eq!(
            emissor.peer.screen,
            crate::protocol::Screen::nova(1125, 2436)
        );
        assert_eq!(receptor.peer.screen, None, "quem hospeda não diz tela");
        assert_eq!(
            emissor.outcome.secret, receptor.outcome.secret,
            "os dois lados têm de derivar o mesmo segredo"
        );
        assert!(emissor.outcome.novo && receptor.outcome.novo);

        // Prova que o canal carrega dados de verdade, não só que abriu.
        emissor.session.send(b"quall M1").expect("envia");
        let recebido = receptor
            .session
            .next_data(Duration::from_secs(5))
            .expect("chegou algo");
        assert_eq!(recebido, b"quall M1");

        // Sem servidor ICE, os dois candidatos escolhidos têm de ser `host`.
        if let Some((local, remoto)) = emissor.session.selected_pair() {
            assert!(
                local.contains("typ host") && remoto.contains("typ host"),
                "o ICE escolheu um par que não é direto: {local} <-> {remoto}"
            );
        }

        emissor.link.close("fim do teste");
        drop(receptor);
    }

    /// Oito receptores, uma identidade de Monitor e um Studio simultâneo. Só protocolo e
    /// transporte em loopback: não cria monitor virtual, não captura nem decodifica imagem.
    #[test]
    fn oito_monitores_e_studio_mantem_sessoes_independentes_e_retomam_sem_pin() {
        use crate::track::TrackKind;

        fn abrir_par(
            emissor_id: &str,
            receptor_id: &str,
            pin: Option<Pin>,
            conhecidos_emissor: PairedPeers,
            conhecidos_receptor: PairedPeers,
        ) -> (SignalingServer, Ready, Ready) {
            let servidor = SignalingServer::bind(0).expect("porta independente");
            let porta = servidor.port().expect("porta");
            let mut cfg_emissor = config(
                anuncio(emissor_id, emissor_id),
                &Pin::parse("313131").unwrap(),
            );
            cfg_emissor.known = conhecidos_emissor;
            cfg_emissor.announcement.capabilities = Capabilities {
                screen_source: true,
                camera_source: false,
                sink: false,
            };
            cfg_emissor.tracks = vec![TrackConfig::new(TrackKind::Screen, "Tela estendida")];
            let emissor = thread::spawn(move || {
                let pronto = hospedar(&servidor, cfg_emissor).expect("emissor conectado");
                (servidor, pronto)
            });
            let mut cfg_receptor = config(
                anuncio(receptor_id, receptor_id),
                &Pin::parse("313131").unwrap(),
            );
            cfg_receptor.pin = pin;
            cfg_receptor.known = conhecidos_receptor;
            cfg_receptor.announcement.capabilities = Capabilities {
                screen_source: false,
                camera_source: false,
                sink: true,
            };
            cfg_receptor.announcement.screen = crate::protocol::Screen::nova(1920, 1080);
            let receptor = conectar(em(porta), cfg_receptor).expect("receptor conectado");
            let (servidor, emissor) = emissor.join().expect("thread do emissor");
            let track = receptor
                .session
                .proxima_track(Duration::from_secs(5))
                .expect("track de tela");
            assert_eq!(track.kind(), TrackKind::Screen);
            assert_eq!(
                emissor.peer.screen,
                crate::protocol::Screen::nova(1920, 1080)
            );
            assert_eq!(emissor.outcome.secret, receptor.outcome.secret);
            (servidor, emissor, receptor)
        }

        fn conferir_dados(emissor: &Ready, receptor: &Ready, valor: u8) {
            emissor.session.send(&[valor]).expect("envio independente");
            assert_eq!(
                receptor.session.next_data(Duration::from_secs(5)),
                Some(vec![valor])
            );
        }

        let pin = || Some(Pin::parse("313131").expect("PIN de teste"));
        let (servidor_studio, studio, receptor_studio) = abrir_par(
            "studio",
            "receptor-studio",
            pin(),
            PairedPeers::new(),
            PairedPeers::new(),
        );
        let mut conhecidos_monitor = PairedPeers::new();
        let mut conhecidos_receptores = Vec::new();
        let mut sessoes = Vec::new();
        for indice in 0..8 {
            let (servidor, emissor, receptor) = abrir_par(
                "monitor",
                &format!("receptor-{indice}"),
                pin(),
                conhecidos_monitor.clone(),
                PairedPeers::new(),
            );
            assert_ne!(servidor.port().unwrap(), servidor_studio.port().unwrap());
            assert!(sessoes
                .iter()
                .all(
                    |(_, s, _, _): &(usize, SignalingServer, Ready, Ready)| s.port().unwrap()
                        != servidor.port().unwrap()
                ));
            assert!(emissor.outcome.novo && receptor.outcome.novo);
            conhecidos_monitor.insert(&emissor.outcome);
            let mut conhecidos = PairedPeers::new();
            conhecidos.insert(&receptor.outcome);
            conhecidos_receptores.push(conhecidos);
            sessoes.push((indice, servidor, emissor, receptor));
        }
        for (indice, _, emissor, receptor) in &sessoes {
            conferir_dados(emissor, receptor, *indice as u8);
        }
        conferir_dados(&studio, &receptor_studio, 99);

        // Reabre cada conexão por segredo salvo, mantendo as outras sete e o Studio vivos.
        for indice in 0..8 {
            let (slot, servidor, emissor, receptor) = sessoes.remove(0);
            assert_eq!(slot, indice);
            drop((servidor, emissor, receptor));
            let (servidor, emissor, receptor) = abrir_par(
                "monitor",
                &format!("receptor-{indice}"),
                None,
                conhecidos_monitor.clone(),
                conhecidos_receptores[indice].clone(),
            );
            assert!(!emissor.outcome.novo && !receptor.outcome.novo);
            conferir_dados(&emissor, &receptor, indice as u8 + 10);
            sessoes.push((indice, servidor, emissor, receptor));
        }
        for (indice, _, emissor, receptor) in &sessoes {
            conferir_dados(emissor, receptor, *indice as u8 + 20);
        }
        conferir_dados(&studio, &receptor_studio, 100);
    }

    /// **O caminho de volta do sinal**, ponta a ponta: o receptor relata o enlace e o emissor lê.
    ///
    /// É o teste que prova que o carimbo escolhido (a sinalização, e não RTCP) atravessa uma
    /// sessão de verdade — não só que a mensagem serializa. Prova também as duas propriedades da
    /// política de leitura: **só o mais recente sobrevive**, e uma segunda leitura sem relato
    /// novo devolve `None` em vez de repetir o velho.
    ///
    /// **Isto é loopback, não é a bancada.**
    #[test]
    fn o_relato_do_enlace_atravessa_do_receptor_para_o_emissor() {
        let pin = Pin::parse("515151").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-taxa", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let mut receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-taxa", "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("receptor fecha a sessão");

        let mut emissor = emissor.join().expect("thread do emissor").expect("emissor");

        // Nada mandado ainda: o emissor não pode inventar relato nenhum.
        assert_eq!(emissor.relato_do_enlace(Duration::from_millis(20)), None);

        let velho = RelatoDoEnlace {
            ms: 500,
            pacotes: 100,
            perdidos: 9,
            suspeitos: 3,
            idrs_quebrados: 1,
            nao_decodificados: 0,
        };
        let novo = RelatoDoEnlace {
            ms: 500,
            pacotes: 220,
            perdidos: 7,
            suspeitos: 2,
            idrs_quebrados: 0,
            nao_decodificados: 4,
        };
        receptor.relatar_enlace(velho).expect("relato 1 sai");
        receptor.relatar_enlace(novo).expect("relato 2 sai");

        // Os dois estão no socket; o emissor lê até achar o segundo. Cada volta consome uma
        // mensagem, e o campo guarda só a mais recente.
        let mut visto = None;
        for _ in 0..50 {
            if let Some(r) = emissor.relato_do_enlace(Duration::from_millis(10)) {
                visto = Some(r);
                if r == novo {
                    break;
                }
            }
        }
        assert_eq!(
            visto,
            Some(novo),
            "o emissor não recebeu o relato mais recente"
        );

        // E não repete: sem relato novo, a leitura seguinte é vazia.
        assert_eq!(emissor.relato_do_enlace(Duration::from_millis(20)), None);

        emissor.link.close("fim do teste");
        drop(receptor);
    }

    /// A coreografia inteira do M2: descoberta de par por sinalização, PIN, e **vídeo pela
    /// track** — não pelo canal de dados.
    ///
    /// Prova, num só teste, as quatro coisas que o M2 pede: a track atravessa, o quadro chega
    /// idêntico depois de passar por FU-A, o pedido de IDR do receptor chega ao emissor, e duas
    /// tracks convivem na mesma sessão.
    ///
    /// **Isto é loopback, não é a bancada.**
    #[test]
    fn sessao_com_tracks_leva_video_e_o_pedido_de_idr_volta() {
        use crate::track::{QuadroCodificado, TrackConfig, TrackKind};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Arc, Mutex};

        let pin = Pin::parse("242424").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-m2", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: vec![
                        TrackConfig::new(TrackKind::Screen, "Tela"),
                        TrackConfig::new(TrackKind::Camera, "Câmera"),
                    ],
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-m2", "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("receptor fecha a sessão");

        let mut emissor = emissor.join().expect("thread do emissor").expect("emissor");
        assert_eq!(
            emissor.tracks.len(),
            2,
            "o emissor tinha de sair com duas tracks"
        );

        let pediram = Arc::new(AtomicU64::new(0));
        for t in &emissor.tracks {
            let c = Arc::clone(&pediram);
            t.ao_pedir_idr(move || {
                c.fetch_add(1, Ordering::Relaxed);
            })
            .expect("tratador de IDR");
        }

        // As tracks chegam ao receptor enquanto ele processa a oferta, então já devem estar
        // esperando na fila.
        let mut chegadas = Vec::new();
        while let Some(t) = receptor.session.proxima_track(Duration::from_secs(5)) {
            chegadas.push(t);
            if chegadas.len() == 2 {
                break;
            }
        }
        assert_eq!(
            chegadas.len(),
            2,
            "chegaram {} tracks, esperadas 2",
            chegadas.len()
        );

        let tela = chegadas
            .iter()
            .find(|t| t.kind() == TrackKind::Screen)
            .expect("a track de tela");

        /// Quadro e se ele é IDR. Nomeado para o clippy não reclamar do tipo aninhado.
        type Colhidos = Arc<Mutex<Vec<(Vec<u8>, bool)>>>;
        let recebidos: Colhidos = Arc::new(Mutex::new(Vec::new()));
        {
            let alvo = Arc::clone(&recebidos);
            tela.ao_receber_quadro(move |q| {
                if let Ok(mut g) = alvo.lock() {
                    g.push((q.annexb.to_vec(), q.idr));
                }
            });
        }

        // Um IDR de 16 KiB: catorze fragmentos FU-A, que é onde um depacotizador errado quebra.
        let mut quadro = vec![0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f];
        quadro.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
        quadro.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        quadro.extend((0..16_384).map(|i| ((i % 254) + 1) as u8));

        let emissor_tela = emissor
            .tracks
            .iter()
            .find(|t| t.kind() == TrackKind::Screen)
            .expect("emissor de tela");

        let fim = Instant::now() + Duration::from_secs(15);
        while Instant::now() < fim {
            let _ = emissor_tela.enviar_quadro(QuadroCodificado {
                annexb: &quadro,
                timestamp_us: 2_000_000,
                idr: true,
            });
            let _ = tela.pedir_idr();
            let pronto = recebidos.lock().map(|g| !g.is_empty()).unwrap_or(false);
            if pronto && pediram.load(Ordering::Relaxed) > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        let chegou = recebidos.lock().expect("cadeado").clone();
        assert!(!chegou.is_empty(), "nenhum quadro atravessou a track");
        assert!(chegou[0].1, "o quadro tinha de vir marcado como IDR");
        assert_eq!(
            chegou[0].0,
            quadro,
            "o quadro remontado difere do que entrou ({} bytes contra {})",
            chegou[0].0.len(),
            quadro.len()
        );
        assert!(
            pediram.load(Ordering::Relaxed) > 0,
            "o PLI do receptor não chegou ao emissor"
        );
        assert_eq!(emissor_tela.idrs_sem_parametros(), 0);

        emissor.link.close("fim do teste");
        drop(receptor);
    }

    /// Regressão do primeiro ensaio na bancada.
    ///
    /// O emissor fecha a sinalização **no instante** em que a sessão dele sobe, enquanto o
    /// receptor ainda está negociando. Antes da correção, o receptor recebia o `Bye` e morria
    /// com "a outra ponta saiu", apesar de o SCTP já estar de pé e a mídia já ser P2P.
    #[test]
    fn emissor_fechar_a_sinalizacao_cedo_nao_derruba_o_receptor() {
        let pin = Pin::parse("135790").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<()> {
            let mut pronto = hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-3", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )?;
            // Sem folga nenhuma: é exatamente o que quebrava.
            pronto.link.close("sessão estabelecida");
            // Segura a sessão viva enquanto o receptor termina de negociar.
            std::thread::sleep(Duration::from_secs(3));
            Ok(())
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-3", "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        );

        assert!(
            receptor.is_ok(),
            "o receptor caiu porque o emissor fechou a sinalização cedo: {:?}",
            receptor.err()
        );
        emissor.join().expect("thread do emissor").expect("emissor");
    }

    /// **Dívida 22, pela coreografia inteira.** O receptor acha que está pareado, o emissor
    /// perdeu o segredo, e a sessão **fecha mesmo assim** — pelo PIN, na mesma conexão.
    ///
    /// É o caso que o usuário vive como "funcionou ontem, hoje não funciona". Antes ele morria
    /// em `aparelho não está pareado aqui` e o produto não oferecia digitar o PIN outra vez.
    #[test]
    fn retomada_perdida_fecha_a_sessao_pelo_pin_na_mesma_conexao() {
        let pin = Pin::parse("909090").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        // O emissor não conhece ninguém: é o lado que perdeu o `pares.json`.
        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-22", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        // O receptor carrega um segredo fantasma para o emissor e vai tentar retomar.
        let mut fantasma = PairedPeers::new();
        fantasma.insert(&crate::pairing::PairOutcome {
            peer: crate::protocol::DeviceId("emissor-22".into()),
            secret: [0xABu8; crate::pairing::PAIR_SECRET_LEN],
            novo: true,
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-22", "Receptor"),
                pin: Some(pin),
                known: fantasma,
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("a retomada falhada tinha de cair no PIN e fechar a sessão");

        let mut emissor = emissor.join().expect("thread do emissor").expect("emissor");

        assert_eq!(
            emissor.outcome.secret, receptor.outcome.secret,
            "os dois lados têm de sair com o mesmo segredo novo"
        );
        assert!(
            emissor.outcome.novo && receptor.outcome.novo,
            "o pareamento veio do PIN, então é novo — e é isso que faz a casca gravar por cima \
             do segredo fantasma"
        );
        assert_ne!(
            receptor.outcome.secret,
            [0xABu8; crate::pairing::PAIR_SECRET_LEN],
            "o segredo fantasma não podia sobreviver"
        );

        emissor.link.close("fim do teste");
        drop(receptor);
    }

    /// **Dívida 20**, e com ela as dívidas 5 e 13.
    ///
    /// O receptor sai; o emissor precisa saber **agora**, não depois dos 30 s do
    /// `CONSENT_TIMEOUT` do libjuice. O `Bye` já chegava — faltava alguém olhar.
    #[test]
    fn quando_o_receptor_sai_o_emissor_fica_sabendo_na_hora() {
        let pin = Pin::parse("606060").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-queda", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let mut receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-queda", "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("receptor fecha a sessão");

        let mut emissor = emissor.join().expect("thread do emissor").expect("emissor");

        // Com os dois de pé e ninguém saindo, o detector tem de ficar quieto. Um detector que
        // grita sozinho seria pior que nenhum: derrubaria toda sessão saudável.
        assert_eq!(
            emissor.proximo_evento(Duration::from_millis(300)),
            EventoDeSessao::Nenhum,
            "o detector acusou queda numa sessão viva"
        );

        // É exatamente o que `quall_session_close` faz do lado da casca.
        receptor.link.close("o usuário fechou o receptor");

        let inicio = Instant::now();
        let mut visto = EventoDeSessao::Nenhum;
        while inicio.elapsed() < Duration::from_secs(5) {
            visto = emissor.proximo_evento(Duration::from_millis(100));
            if visto != EventoDeSessao::Nenhum {
                break;
            }
        }
        let levou = inicio.elapsed();

        assert_eq!(
            visto,
            EventoDeSessao::Desconectou,
            "o emissor não percebeu a saída do receptor em {levou:?}"
        );
        assert!(
            levou < Duration::from_secs(2),
            "o emissor levou {levou:?} — o `CONSENT_TIMEOUT` do libjuice é 30 s, e a graça é não \
             esperar por ele"
        );

        // A trava: uma vez caída, a sessão não volta a dizer que está de pé.
        assert_eq!(
            emissor.proximo_evento(Duration::from_millis(0)),
            EventoDeSessao::Desconectou
        );
        drop(receptor);
    }

    /// **O defeito que o emissor do Windows desviou em vez de consertar (bancada, 28/08).**
    ///
    /// `proximo_evento(Duration::ZERO)` custava **29,43 ms por volta** do laço do emissor, contra
    /// 7,97 ms de `bombear` e 2,67 ms de `enviar_quadro` — três quartos do laço numa chamada cujo
    /// argumento é zero. São dois bloqueios somados, um em cada metade do detector:
    ///
    /// - `signaling.rs::poll_por` elevava a fatia zero a 1 ms e fazia leitura **bloqueante**;
    /// - `olhar_transporte` pedia `recv_timeout(1ms)` por volta do laço de drenagem.
    ///
    /// No Windows cada um desses milissegundos vira um tique de ~15,6 ms do temporizador do
    /// sistema, e dois tiques são os 29,4 ms medidos. O emissor contornou espiando a cada 200 ms;
    /// o defeito era do núcleo e valia para qualquer casca.
    ///
    /// Este teste mede a chamada inteira, que é o que a casca vê. O piso da versão anterior era
    /// 2 ms por volta em **qualquer** plataforma (1 ms de socket + 1 ms de canal), logo estas 200
    /// voltas custariam ≥ 400 ms aqui e ~6 s no Windows. O teto de 100 ms é folga de 4× abaixo
    /// daquele piso.
    #[test]
    fn espiar_a_sessao_com_fatia_zero_nao_bloqueia() {
        let pin = Pin::parse("707070").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-espiada", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-espiada", "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("receptor fecha a sessão");

        let mut emissor = emissor.join().expect("thread do emissor").expect("emissor");

        const VOLTAS: u32 = 200;
        let inicio = Instant::now();
        for _ in 0..VOLTAS {
            assert_eq!(
                emissor.proximo_evento(Duration::ZERO),
                EventoDeSessao::Nenhum,
                "o detector acusou queda numa sessão viva"
            );
        }
        let levou = inicio.elapsed();

        assert!(
            levou < Duration::from_millis(100),
            "{VOLTAS} espiadas com fatia zero levaram {levou:?} ({:.3} ms por volta). É o defeito \
             que o emissor do Windows desviou com uma espiada a cada 200 ms.",
            levou.as_secs_f64() * 1000.0 / f64::from(VOLTAS)
        );

        emissor.link.close("fim do teste");
        drop(receptor);
    }

    /// **Dívida 10.** A tela de espera precisa de um botão Cancelar que funcione.
    ///
    /// A frente iOS mediu o contorno em uso: sem cutucada, `quall_host` segura 120,16 s; com uma
    /// conexão TCP descartável para o próprio endereço aos 3 s, sai em 3,09 s. Este teste pede a
    /// mesma coisa pela porta da frente, e com o prazo generoso: cancelar aos 300 ms tem de
    /// devolver [`Error::Cancelled`] em muito menos que os 30 s do prazo.
    ///
    /// A espera é medida por canal, e não por `join`, para o teste **falhar** em vez de ficar
    /// pendurado se o cancelamento não chegar ao laço.
    #[test]
    fn cancelar_destrava_o_hospedar_sem_esperar_o_prazo() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let cancelar = Cancelamento::novo();
        let copia = cancelar.clone();

        let (tx, rx) = std::sync::mpsc::channel();
        let inicio = Instant::now();
        thread::spawn(move || {
            let r = hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-cancel", "Emissor"),
                    pin: Some(Pin::parse("777777").expect("pin")),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: copia,
                    silencio_do_caminho: None,
                },
            );
            let _ = tx.send((inicio.elapsed(), r.err()));
        });

        thread::sleep(Duration::from_millis(300));
        cancelar.cancelar();

        let (levou, erro) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("o cancelamento não chegou ao laço: `hospedar` continuou esperando");
        assert!(
            matches!(erro, Some(Error::Cancelled)),
            "esperava Cancelled, veio {erro:?}"
        );
        assert!(
            levou < Duration::from_secs(3),
            "cancelar levou {levou:?} para destravar a espera"
        );
    }

    /// A outra metade do contrato: sem ninguém cancelar, a espera vai **até o prazo**.
    ///
    /// Guarda contra o conserto da dívida 10 desistir cedo por conta própria, que seria trocar um
    /// defeito por outro pior — uma tela de espera que desiste sozinha.
    #[test]
    fn sem_cancelamento_a_espera_vai_ate_o_prazo() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let inicio = Instant::now();
        let r = hospedar(
            &servidor,
            SessionConfig {
                announcement: anuncio("emissor-prazo", "Emissor"),
                pin: Some(Pin::parse("888888").expect("pin")),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_millis(800),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        );
        let levou = inicio.elapsed();
        assert!(matches!(r, Err(Error::Timeout(_))), "veio {:?}", r.err());
        assert!(
            levou >= Duration::from_millis(700),
            "a espera desistiu em {levou:?}, antes do prazo"
        );
    }

    #[test]
    fn pin_errado_derruba_a_sessao_dos_dois_lados() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let emissor = thread::spawn(move || {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-2", "Emissor"),
                    pin: Some(Pin::parse("111111").expect("pin")),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(20),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let r = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-2", "Receptor"),
                pin: Some(Pin::parse("222222").expect("pin")),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(20),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        );

        assert!(
            matches!(&r, Err(Error::WrongPin(_))),
            "PIN errado no receptor tem de ser recusa de pareamento, não HTTP: {:?}", r.err()
        );
        assert!(
            matches!(emissor.join().expect("thread do emissor"), Err(Error::Pairing(_))),
            "o emissor precisa consumir a tentativa interrompida e exigir outro PIN"
        );
    }

    /// **O defeito de 01/09/2026: um candidato que cai matava o listener em definitivo.**
    ///
    /// Aqui o candidato nem chega a falar WebSocket — manda lixo e sai. Antes do conserto o
    /// `apertar_mao_do_servidor(...)?` propagava, `hospedar` devolvia à casca, a porta fechava e
    /// o mDNS saía do ar com o processo vivo. O receptor de verdade que vem logo atrás é a
    /// medida: **a espera tinha de ter sobrevivido ao primeiro**.
    #[test]
    fn candidato_que_fala_lixo_nao_derruba_a_espera() {
        let pin = Pin::parse("313131").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-lixo", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");

        // O candidato ruim: abre, fala o que não é HTTP, e some.
        {
            use std::io::Write;
            let mut cru = std::net::TcpStream::connect(destino).expect("conectar cru");
            let _ = cru.write_all(b"isto nao e um upgrade de websocket\r\n\r\n");
            let _ = cru.flush();
        }

        let receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-lixo", "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("o receptor de verdade tinha de fechar a sessão depois do candidato ruim");

        let pronto = emissor.join().expect("thread do emissor").expect("emissor");
        assert_eq!(
            pronto.peer.device_id.0, "receptor-lixo",
            "fechou com o par errado"
        );
        assert!(
            pronto.descartados >= 1,
            "o descarte tem de ser contado, senão o conserto some da medida"
        );
        drop(receptor);
    }

    /// O mesmo defeito um passo adiante: o candidato **fala WebSocket**, inicia o Probe, e some
    /// antes de parear. É o "depois que sai não conecta" na forma em que ele aparece no produto —
    /// o receptor que fecha o app enquanto o emissor espera.
    ///
    /// O candidato some antes de mandar KE1, então não gastou tentativa de PIN nenhuma.
    /// Quem erra o PIN continua
    /// derrubando a hospedagem — é o que `pin_errado_derruba_a_sessao_dos_dois_lados` cobra.
    #[test]
    fn candidato_que_cai_antes_de_parear_nao_derruba_a_espera() {
        let pin = Pin::parse("323232").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio("emissor-sumico", "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: None,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");

        // O candidato desiste depois do Challenge público, antes de qualquer tentativa PAKE.
        {
            let mut link = connect(destino, Duration::from_secs(10)).expect("candidato conecta");
            link.send(&SignalMessage::Pair(PairFrame::Probe {
                version: PROTOCOL_VERSION,
                guest_nonce: "31".repeat(16),
                guest_role: 0,
            }))
            .expect("Probe");
            let ate = Instant::now() + Duration::from_secs(5);
            let mut recebeu_challenge = false;
            while Instant::now() < ate {
                match link.poll() {
                    Ok(Some(SignalMessage::Pair(PairFrame::Challenge { .. }))) => {
                        recebeu_challenge = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => panic!("o candidato tinha de receber Challenge: {e}"),
                }
            }
            assert!(recebeu_challenge, "Challenge não chegou dentro do prazo");
            // Sem `close`: some sem se despedir, que é o caso real.
        }

        let receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio("receptor-sumico", "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: None,
            },
        )
        .expect("o receptor de verdade tinha de fechar a sessão depois do candidato que sumiu");

        let pronto = emissor.join().expect("thread do emissor").expect("emissor");
        assert_eq!(
            pronto.peer.device_id.0, "receptor-sumico",
            "fechou com o par errado"
        );
        assert_eq!(
            pronto.descartados, 1,
            "um candidato caiu, e um é o que tem de ser contado"
        );
        drop(receptor);
    }

    /// A classificação é o coração da mudança, e ela é uma **decisão de segurança**: só acidente
    /// reespera. Um teste direto sobre ela custa nada e é o que impede alguém de acrescentar
    /// `Pairing` à lista num dia apressado.
    #[test]
    fn so_acidente_de_transporte_faz_reesperar() {
        for acidente in [
            Error::Io("os error 10054".into()),
            Error::Closed,
            Error::Signaling("IO error: connection reset".into()),
        ] {
            assert!(
                e_acidente_do_candidato(&acidente),
                "{acidente} tinha de fazer a espera continuar"
            );
        }
        for decisao in [
            Error::WrongPin("o PIN não conferiu".into()),
            Error::NeedsPin("não conheço este aparelho".into()),
            Error::Pairing("MAC de retomada inválido".into()),
            Error::Protocol("esperava Hello".into()),
            Error::Timeout("nenhum receptor conectou".into()),
            Error::Cancelled,
            Error::NoRoute("sem caminho".into()),
            Error::Transport("o canal não abriu".into()),
        ] {
            assert!(
                !e_acidente_do_candidato(&decisao),
                "{decisao} tinha de terminar a hospedagem"
            );
        }
    }

    /// Sobe uma sessão de loopback e devolve os dois lados, com o prazo de silêncio que cada um
    /// pediu. Fatorado porque os dois testes do detector de caminho mudo só diferem nisso.
    fn sessao_de_loopback(
        marca: &str,
        pin_texto: &str,
        silencio_do_emissor: Option<Duration>,
        silencio_do_receptor: Option<Duration>,
    ) -> (Ready, Ready) {
        let pin = Pin::parse(pin_texto).expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let pin_emissor = pin.clone();
        let id_emissor = format!("emissor-{marca}");
        let id_receptor = format!("receptor-{marca}");
        let nome_emissor = id_emissor.clone();
        let emissor = thread::spawn(move || -> Result<Ready> {
            hospedar(
                &servidor,
                SessionConfig {
                    announcement: anuncio(&nome_emissor, "Emissor"),
                    pin: Some(pin_emissor),
                    known: PairedPeers::new(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: Duration::from_secs(30),
                    cancelamento: Cancelamento::novo(),
                    silencio_do_caminho: silencio_do_emissor,
                },
            )
        });

        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let receptor = conectar(
            destino,
            SessionConfig {
                announcement: anuncio(&id_receptor, "Receptor"),
                pin: Some(pin),
                known: PairedPeers::new(),
                transport: TransportConfig::default(),
                tracks: Vec::new(),
                timeout: Duration::from_secs(30),
                cancelamento: Cancelamento::novo(),
                silencio_do_caminho: silencio_do_receptor,
            },
        )
        .expect("receptor fecha a sessão");

        let emissor = emissor.join().expect("thread do emissor").expect("emissor");
        (emissor, receptor)
    }

    /// **O defeito medido em `docs/receptor-ios.md:243`, reproduzido no piso.**
    ///
    /// A sinalização fica de pé, o transporte não muda de estado, e o caminho da mídia
    /// simplesmente para de entregar. Antes deste detector, `proximo_evento` devolvia `Nenhum`
    /// para sempre — na bancada foram mais de dez segundos a 20 Hz — e a tela ficava congelada
    /// com a sessão "saudável".
    ///
    /// **Isto é loopback, não é a bancada.** O que está provado aqui é o detector; que ele
    /// dispare ao desplugar um cabo de verdade é medida que ninguém fez.
    #[test]
    fn caminho_mudo_derruba_a_sessao_com_a_sinalizacao_de_pe() {
        let (emissor, mut receptor) =
            sessao_de_loopback("mudo", "313131", None, Some(Duration::from_millis(300)));

        // Um byte, para armar: o detector não conta silêncio de quem nunca recebeu nada.
        emissor.session.send(b"vivo").expect("envia");
        assert_eq!(
            receptor
                .session
                .next_data(Duration::from_secs(5))
                .expect("chegou algo"),
            b"vivo"
        );

        // Antes do prazo, nada acontece — senão o detector seria um gerador de falso positivo.
        assert_eq!(
            receptor.proximo_evento(Duration::from_millis(50)),
            EventoDeSessao::Nenhum,
            "disparou antes do prazo de silêncio"
        );

        // E agora o silêncio: ninguém manda mais nada, e a sinalização do emissor continua
        // aberta (o `emissor` está vivo nesta função, então não há `Bye` nem TCP fechado).
        assert_eq!(
            receptor.proximo_evento(Duration::from_secs(5)),
            EventoDeSessao::Desconectou,
            "o caminho da mídia emudeceu e a sessão continuou se dizendo saudável"
        );

        drop(emissor);
        drop(receptor);
    }

    /// A guarda que impede o detector de derrubar **todo emissor de produto**: quem só manda
    /// nunca recebe pacote nenhum, e "nada chegou desde que a sessão subiu" não é silêncio — é
    /// ausência de medida.
    #[test]
    fn silencio_do_caminho_nao_arma_sem_nunca_ter_chegado_nada() {
        let prazo = Duration::from_millis(50);
        let (mut emissor, receptor) = sessao_de_loopback("nunca", "323232", Some(prazo), None);

        // Dez vezes o prazo sem nada chegar, e a sessão continua de pé.
        assert_eq!(
            emissor.proximo_evento(Duration::from_millis(500)),
            EventoDeSessao::Nenhum,
            "o detector armou sem nunca ter recebido nada — todo emissor cairia sozinho"
        );
        assert_eq!(
            emissor.session.silencio_da_midia(),
            None,
            "nada chegou: não há silêncio a medir"
        );

        drop(receptor);
        drop(emissor);
    }

    // -----------------------------------------------------------------------------------------
    // O papel no aperto de mão (F6a, `docs/contrato-teleprompter.md` §2)
    // -----------------------------------------------------------------------------------------

    fn com_papel(id: &str, papel: Option<Papel>) -> Announcement {
        Announcement {
            papel,
            capabilities: crate::protocol::Capabilities {
                screen_source: papel.is_none(),
                camera_source: false,
                sink: papel.is_none(),
            },
            ..anuncio(id, id)
        }
    }

    fn config(anuncio: Announcement, pin: &Pin) -> SessionConfig {
        SessionConfig {
            announcement: anuncio,
            pin: Some(pin.clone()),
            known: PairedPeers::new(),
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(30),
            cancelamento: Cancelamento::novo(),
            silencio_do_caminho: None,
        }
    }

    fn em(porta: u16) -> std::net::SocketAddr {
        format!("127.0.0.1:{porta}").parse().expect("endereço")
    }

    /// Um receptor de vídeo v3 pode negociar com anfitrião sem track e sem papel.
    ///
    /// O anúncio cifrado omite o papel (`protocol::anuncio_sem_papel_omite_a_chave_e_declara_v3`).
    /// O teste mede o caminho de vídeo vazio da versão atual; versões antigas são recusadas.
    ///
    /// 1. A sessão sobe dos dois lados: PAKE, anúncio cifrado, ICE, canal aberto.
    /// 2. Nenhuma track chega em 3 s.
    /// 3. A sessão continua "saudável": `proximo_evento` diz `Nenhum`.
    /// 4. O que o anfitrião manda pelo canal se acumula no receptor, que não lê: 64 ficam, o resto
    ///    é descartado e contado. Nada cai.
    ///
    /// O que cada casca receptora faz nesse estado (prazos de 15 a 20 s, e o OBS tentando de novo
    /// a cada 3 s) está em `docs/contrato-teleprompter.md` §7, lido do código das cascas.
    #[test]
    fn receptor_de_video_v3_num_anfitriao_sem_track_sobe_e_fica_sem_imagem() {
        let pin = Pin::parse("616161").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let p = pin.clone();
        let anfitriao = thread::spawn(move || {
            hospedar(
                &servidor,
                config(com_papel("anfitriao-sem-track", None), &p),
            )
        });
        let mut receptor = conectar(em(porta), config(com_papel("receptor-antigo", None), &pin))
            .expect("hoje, a sessão sobe");
        let anfitriao = anfitriao.join().expect("thread").expect("anfitrião");

        assert!(
            receptor
                .session
                .proxima_track(Duration::from_secs(3))
                .is_none(),
            "chegou track?"
        );
        assert_eq!(
            receptor.proximo_evento(Duration::ZERO),
            EventoDeSessao::Nenhum
        );

        let m = anfitriao.session.mensageiro();
        let mut mandou = 0;
        let fim = Instant::now() + Duration::from_secs(10);
        while mandou < 100 && Instant::now() < fim {
            if m.enviar(&format!("{{\"n\":{mandou}}}")).is_ok() {
                mandou += 1;
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(mandou, 100);
        let fim = Instant::now() + Duration::from_secs(10);
        while receptor.session.dropped_frames() < 36 && Instant::now() < fim {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            receptor.session.dropped_frames(),
            36,
            "64 ficam na fila, o resto é contado"
        );
        assert_eq!(
            receptor.proximo_evento(Duration::ZERO),
            EventoDeSessao::Nenhum,
            "nada cai"
        );
        drop(receptor);
        drop(anfitriao);
    }

    /// **O prompter recusa o receptor de vídeo antes do PIN, com motivo legível, e segue
    /// esperando com o mesmo PIN** — o controle que vem depois entra. E a sessão que sobe é
    /// confiável e sem ordem dos dois lados, com o detector de silêncio ligado.
    #[test]
    fn o_prompter_recusa_o_receptor_de_video_antes_do_pin_e_segue_esperando() {
        let pin = Pin::parse("626262").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let p = pin.clone();
        let prompter = thread::spawn(move || {
            hospedar(
                &servidor,
                config(com_papel("prompter", Some(Papel::Teleprompter)), &p),
            )
        });

        // Um receptor de vídeo (sem papel), com o PIN certo.
        let erro = conectar(em(porta), config(com_papel("receptor", None), &pin))
            .err()
            .expect("o receptor de vídeo tinha de ser recusado");
        let texto = erro.to_string();
        assert!(matches!(erro, Error::Protocol(_)), "{erro:?}");
        assert!(
            texto.contains("teleprompter"),
            "o motivo tem de dizer por quê: {texto}"
        );

        // O controle, com o **mesmo** PIN, depois.
        let controle = conectar(
            em(porta),
            config(com_papel("controle", Some(Papel::ControleRemoto)), &pin),
        )
        .expect("o controle entra");
        let prompter = prompter
            .join()
            .expect("thread")
            .expect("a espera do prompter continuou");
        assert_eq!(
            prompter.descartados, 1,
            "o receptor recusado conta como candidato que caiu"
        );
        assert_eq!(prompter.peer.papel, Some(Papel::ControleRemoto));
        assert_eq!(controle.peer.papel, Some(Papel::Teleprompter));
        assert_eq!(
            prompter.session.entrega_do_canal(),
            Some(Delivery::ReliableUnordered)
        );
        assert!(
            (0..200).any(|_| {
                thread::sleep(Duration::from_millis(10));
                controle.session.entrega_do_canal() == Some(Delivery::ReliableUnordered)
            }),
            "o controle tem de adotar a entrega do prompter"
        );
        assert_eq!(prompter.silencio_do_caminho, Some(SILENCIO_DO_TELEPROMPTER));
        assert_eq!(controle.silencio_do_caminho, Some(SILENCIO_DO_TELEPROMPTER));
        drop(controle);
        drop(prompter);
    }

    /// **O mensageiro sabe quem é o par** (`docs/contrato-teleprompter.md` §11.2): o `device_id` e o
    /// nome do aperto de mão, dos dois lados, em todo mensageiro pedido depois — é o que diz à
    /// réplica do controle se o prompter é o da última vez, antes da primeira mensagem sair. Uma
    /// sessão do transporte montada à mão não tem par.
    #[test]
    fn o_mensageiro_sabe_quem_e_o_par() {
        let pin = Pin::parse("636363").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let p = pin.clone();
        let prompter = thread::spawn(move || {
            hospedar(
                &servidor,
                config(com_papel("prompter-par", Some(Papel::Teleprompter)), &p),
            )
        });
        let controle = conectar(
            em(porta),
            config(com_papel("controle-par", Some(Papel::ControleRemoto)), &pin),
        )
        .expect("o controle entra");
        let prompter = prompter.join().expect("thread").expect("prompter");
        let (mp, mc) = (prompter.session.mensageiro(), controle.session.mensageiro());
        assert_eq!(
            mc.par().map(|p| (p.id.as_str(), p.nome.as_str())),
            Some(("prompter-par", "prompter-par"))
        );
        assert_eq!(mp.par().map(|p| p.id.as_str()), Some("controle-par"));
        assert!(Session::offerer(&TransportConfig::default())
            .expect("sessão")
            .mensageiro()
            .par()
            .is_none());
        drop(controle);
        drop(prompter);
    }

    /// **O controle que bate num emissor de vídeo sai com `Bye`**, e o emissor segue esperando —
    /// um `Error` ali encerraria a espera dele (é decisão; `Bye` é acidente).
    #[test]
    fn o_controle_recusa_o_emissor_de_video_com_bye_e_o_emissor_segue_esperando() {
        let pin = Pin::parse("636363").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let p = pin.clone();
        let emissor =
            thread::spawn(move || hospedar(&servidor, config(com_papel("emissor", None), &p)));

        let erro = conectar(
            em(porta),
            config(com_papel("controle", Some(Papel::ControleRemoto)), &pin),
        )
        .err()
        .expect("o controle tinha de recusar um emissor de vídeo");
        assert!(matches!(erro, Error::Protocol(_)), "{erro:?}");
        assert!(erro.to_string().contains("não é um teleprompter"), "{erro}");

        let receptor = conectar(em(porta), config(com_papel("receptor", None), &pin))
            .expect("o receptor de vídeo entra depois");
        let emissor = emissor
            .join()
            .expect("thread")
            .expect("a espera do emissor continuou");
        assert_eq!(emissor.descartados, 1);
        assert_eq!(
            emissor.session.entrega_do_canal(),
            Some(Delivery::Realtime),
            "uma sessão de vídeo não muda de entrega"
        );
        drop(receptor);
        drop(emissor);
    }

    /// O atendente devolve ocupado antes do PAKE sem revelar a identidade da sessão ativa.
    /// Um candidato que tem o mesmo ID local não pode substituir o controle autenticado.
    #[test]
    fn o_atendente_responde_ocupado_e_um_hello_nao_derruba_a_sessao() {
        let pin = Pin::parse("646464").expect("pin");
        let servidor = std::sync::Arc::new(SignalingServer::bind(0).expect("bind"));
        let porta = servidor.port().expect("porta");
        let (p, s) = (pin.clone(), std::sync::Arc::clone(&servidor));
        let eu = com_papel("prompter", Some(Papel::Teleprompter));
        let eu_t = eu.clone();
        let prompter = thread::spawn(move || hospedar(&s, config(eu_t, &p)));
        let controle = conectar(
            em(porta),
            config(com_papel("controle-a", Some(Papel::ControleRemoto)), &pin),
        )
        .expect("o controle entra");
        let mut prompter = prompter.join().expect("thread").expect("prompter");
        prompter.atender_enquanto_dura(std::sync::Arc::clone(&servidor), eu);

        // Um segundo controle: "ocupado", na hora.
        let comeco = Instant::now();
        let erro = conectar(
            em(porta),
            config(com_papel("controle-b", Some(Papel::ControleRemoto)), &pin),
        )
        .err()
        .expect("o segundo controle tinha de ouvir ocupado");
        assert!(matches!(erro, Error::Ocupado(_)), "{erro:?}");
        assert!(
            comeco.elapsed() < Duration::from_secs(5),
            "levou {:?}",
            comeco.elapsed()
        );
        assert_eq!(
            prompter.proximo_evento(Duration::ZERO),
            EventoDeSessao::Nenhum,
            "a sessão de A segue"
        );

        // A sessão ocupada não autentica nem revela papéis ou identidades a outro candidato.
        let erro = conectar(em(porta), config(com_papel("receptor", None), &pin))
            .err()
            .expect("recusado");
        assert!(matches!(erro, Error::Ocupado(_)), "{erro:?}");

        // O mesmo ID persistente local também ouve ocupado antes de poder transmiti-lo.
        let erro = conectar(
            em(porta),
            config(com_papel("controle-a", Some(Papel::ControleRemoto)), &pin),
        )
        .err()
        .expect("o mesmo id ouve ocupado");
        assert!(matches!(erro, Error::Ocupado(_)), "{erro:?}");
        assert_eq!(
            prompter.proximo_evento(Duration::ZERO),
            EventoDeSessao::Nenhum,
            "um Hello, que não prova nada, derrubou a sessão"
        );
        assert_eq!(prompter.atendidos_durante_a_sessao(), 3);
        drop(controle);
        drop(prompter);
    }

    /// **A sessão de teleprompter cai em cinco segundos de silêncio**, e não nos 30 do ICE: as
    /// duas pontas mandam estado a cada segundo, então silêncio aqui é defeito. O controle manda
    /// uma mensagem e para — como um iPhone que bloqueou.
    #[test]
    fn a_sessao_de_teleprompter_cai_em_cinco_segundos_de_silencio() {
        let pin = Pin::parse("656565").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let p = pin.clone();
        let prompter = thread::spawn(move || {
            hospedar(
                &servidor,
                config(com_papel("prompter-s", Some(Papel::Teleprompter)), &p),
            )
        });
        let controle = conectar(
            em(porta),
            config(com_papel("controle-s", Some(Papel::ControleRemoto)), &pin),
        )
        .expect("controle");
        let mut prompter = prompter.join().expect("thread").expect("prompter");
        let m = controle.session.mensageiro();
        assert!((0..100).any(|_| {
            thread::sleep(Duration::from_millis(20));
            m.enviar("{}").is_ok()
        }));
        let comeco = Instant::now();
        let mut evento = EventoDeSessao::Nenhum;
        while evento == EventoDeSessao::Nenhum && comeco.elapsed() < Duration::from_secs(12) {
            evento = prompter.proximo_evento(Duration::from_millis(100));
        }
        let levou = comeco.elapsed();
        assert_eq!(evento, EventoDeSessao::Desconectou, "não caiu em {levou:?}");
        assert!(
            levou >= Duration::from_secs(4),
            "caiu cedo demais: {levou:?}"
        );
        assert!(
            levou < Duration::from_secs(8),
            "caiu tarde demais: {levou:?}"
        );
        drop(controle);
        drop(prompter);
    }
}
