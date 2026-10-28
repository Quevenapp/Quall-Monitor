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
use crate::confirmacao::{ConfirmacaoPorCodigo, Fase};
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
    /// **Convidado**: o endereço em que a sessão de fato conectou. A casca grava a sessão salva com
    /// ele **depois** de autenticar (M3). `None` no anfitrião.
    pub endereco_do_par: Option<std::net::SocketAddr>,
    /// Por onde [`conectar_ao_par`] achou o aparelho: `"endereco_antigo"`, `"mdns"`, `"udp"` ou
    /// `"sondagem"`. `None` fora dele.
    pub via_do_par: Option<&'static str>,
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

/// O código de 4 dígitos numa perna: a tela compartilhada e, no convidado, o alvo da perna (para o
/// limite de pernas da revisão A1).
#[derive(Clone, Copy)]
struct CodigoDaPerna<'a> {
    confirmacao: &'a ConfirmacaoPorCodigo,
    alvo: Option<std::net::SocketAddr>,
}

/// O desfecho do código para a tela, a partir do erro que encerrou a perna.
fn fase_do_erro(e: &Error) -> Fase {
    match e {
        Error::Timeout(_) => Fase::Expirou,
        Error::Pairing(m) if m.contains("recusou") => Fase::RecusadoLa,
        Error::Closed | Error::Io(_) | Error::Signaling(_) | Error::Ocupado(_) => Fase::RecusadoLa,
        _ => Fase::NaoConferiu,
    }
}

/// A máquina PAKE contém somente material efêmero público antes de confirmar ambas as pontas.
/// Toda identidade/SDP/ICE/relato subsequente passa pelo Link cifrado, sem modo legado.
///
/// Com `codigo`, a perna pode virar a troca do código de 4 dígitos (`docs/v3-no-privado.md` §3):
/// o anfitrião mostra três números e espera a escolha; o convidado mostra o código. O prazo da
/// autenticação ganha [`ESPERA_PELA_PESSOA`] quando o código aparece, e `prazo` (o da sessão) é
/// estendido para a negociação que vem depois ainda caber.
fn parear_link(
    link: &mut Link,
    cfg: &SessionConfig,
    role: Role,
    prazo: &mut Instant,
    codigo: Option<CodigoDaPerna<'_>>,
    ate_o_desafio: Option<Instant>,
    qr: Option<(DeviceId, [u8; crate::pairing::CHAVE_DO_QR_LEN])>,
) -> Result<(PairOutcome, u8, Option<DeviceId>)> {
    conferir_anuncio(&cfg.announcement)?;
    let mut maquina = Pairing::new_with_store(
        role,
        cfg.announcement.device_id.clone(),
        cfg.pin.clone(),
        &cfg.known,
    )?;
    maquina.bind_local_role(codigo_do_papel(cfg.announcement.papel)?)?;
    maquina.com_codigo(codigo.is_some(), &cfg.announcement.display_name)?;
    // O QR invertido (`docs/v3-no-privado.md` §4): o anfitrião aceita os convites lidos neste
    // processo; o convidado que recebeu o chamado pareia com a chave do QR.
    match (role, qr) {
        (Role::Host, _) => maquina.com_convites(crate::qr::lidos_de(link.peer_addr().ip()))?,
        (Role::Guest, Some((celular, chave))) => maquina.com_qr(celular, chave)?,
        (Role::Guest, None) => {}
    }
    let mut convite_tomado = false;
    let mut auth_prazo = (*prazo).min(Instant::now() + Duration::from_secs(30));
    let mut frames = 0u8;
    let mut codigo_comecou = false;
    let mut codigo_mostrado: Option<Instant> = None;
    if role == Role::Guest {
        link.send(&SignalMessage::Pair(maquina.open()?))?;
    }
    let falhar = |maquina: &Pairing, link: &mut Link, e: Error, mostrado: bool| -> Error {
        // O celular que leu o QR fica sabendo como terminou (`qr::desfecho`).
        if role == Role::Host {
            if let Some(id) = maquina.convite_do_qr() {
                let d = if matches!(e, Error::JaPareado(_)) { "already_paired" } else { "failed" };
                crate::qr::registrar_desfecho(id, d);
            }
        }
        // **O anfitrião que não aceita o código** (bancada de 13/10, iPhone 7 → prompter R5 do S24):
        // a conexão cai logo depois do `Hello {codigo}`, sem código nenhum na tela de ninguém. Não é
        // "o outro recusou": a tela tem de pedir o PIN, e o cartão não pode dizer outra coisa.
        let codigo_nao_aceito = role == Role::Guest
            && maquina.hello_do_codigo_sem_resposta()
            && matches!(e, Error::Closed | Error::Io(_) | Error::Signaling(_));
        if let Some(c) = codigo {
            if maquina.e_por_codigo() {
                if matches!(e, Error::JaPareado(_)) {
                    c.confirmacao.recusar_por("already_paired");
                }
                if codigo_nao_aceito {
                    c.confirmacao.recusar_por("code_not_accepted");
                }
                // "Recusou" só depois de um código na tela: antes dele, ninguém recusou nada, e o
                // desfecho genérico de queda volta ao "esperando" (o status da chamada diz o resto).
                let fase = match fase_do_erro(&e) {
                    Fase::RecusadoLa if !mostrado => Fase::Esperando,
                    f => f,
                };
                c.confirmacao.encerrar(fase);
                // **A1**: a perna em que o nonce do convidado saiu e o código não chegou à tela
                // conta contra este alvo — é o relé que descarta pernas até o código casar.
                // Cancelar não é perna caída (revisão, baixo 4).
                if role == Role::Guest
                    && maquina.nonce_do_codigo_saiu()
                    && !mostrado
                    && !matches!(e, Error::Cancelled)
                {
                    if let Some(alvo) = c.alvo {
                        registrar_perna_caida(alvo);
                    }
                }
            }
        }
        if codigo_nao_aceito {
            link.close("pareamento recusado");
            return Error::NeedsPin("este aparelho não aceita o código; digite o PIN".into());
        }
        link.close("pareamento recusado");
        erro_da_tentativa(maquina, role, e)
    };
    loop {
        // **A recusa do convidado vem antes do próximo quadro** (revisão, baixo 3): sem isto, a
        // `CodigoDecisao {ok}` que chegasse na mesma volta seria processada e o par fecharia com a
        // recusa da pessoa já dada.
        if role == Role::Guest && codigo_mostrado.is_some() {
            if let Some(c) = codigo {
                if c.confirmacao.tomar_decisao() == Some(false) {
                    let e = Error::Pairing("a pessoa recusou o código neste aparelho".into());
                    return Err(falhar(&maquina, link, e, true));
                }
            }
        }
        // **Prazo curto até o desafio** (revisão M-c): na procura da sessão salva, um candidato
        // mudo não consome a procura inteira; quem já respondeu o `Challenge` ganha o prazo cheio.
        let limite = match ate_o_desafio {
            Some(a) if !maquina.desafio_recebido() => a.min(auth_prazo),
            _ => auth_prazo,
        };
        let passo = (|| -> Result<Option<crate::pairing::Step>> {
            restante(limite, &cfg.cancelamento)?;
            match link.poll()? {
                Some(SignalMessage::Pair(quadro)) => {
                    frames = frames.saturating_add(1);
                    if frames > 14 {
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
        let passo = match passo {
            Ok(p) => p,
            Err(e) => return Err(falhar(&maquina, link, e, codigo_mostrado.is_some())),
        };
        // **O convite do QR gasta uma tentativa** quando a dica dele aparece (três por convite lido).
        if role == Role::Host && !convite_tomado {
            if let Some(id) = maquina.convite_do_qr().cloned() {
                convite_tomado = true;
                if crate::qr::tomar(&id).is_none() {
                    let e = Error::Pairing("o convite do QR já gastou as tentativas".into());
                    return Err(falhar(&maquina, link, e, codigo_mostrado.is_some()));
                }
            }
        }
        if let Some(passo) = passo {
            if let Some(resposta) = passo.reply {
                if let Err(e) = link.send(&SignalMessage::Pair(resposta)) {
                    return Err(falhar(&maquina, link, e, codigo_mostrado.is_some()));
                }
            }
            if let Some(resultado) = passo.done {
                if let Some(id) = maquina.convite_do_qr() {
                    crate::qr::concluir(id);
                }
                if let (Some(c), true) = (codigo, maquina.e_por_codigo()) {
                    if let Some(nome) = maquina.nome_do_par() {
                        c.confirmacao.nomear(nome);
                    }
                    c.confirmacao.encerrar(Fase::Conferiu);
                }
                let papel = maquina.peer_role()?;
                link.enable_secure(maquina.take_secure_channel()?)?;
                return Ok((resultado, papel, maquina.convite_do_qr().cloned()));
            }
        }

        // O código, quando esta perna é por código.
        let Some(c) = codigo else { continue };
        if !maquina.e_por_codigo() {
            continue;
        }
        if !codigo_comecou && (role == Role::Guest || maquina.nome_do_par().is_some()) {
            c.confirmacao.comecar(maquina.nome_do_par().unwrap_or(""), role == Role::Host);
            codigo_comecou = true;
        }
        if codigo_mostrado.is_none() {
            if let Some(cod) = maquina.codigo_na_tela() {
                c.confirmacao.mostrar(cod, maquina.opcoes_na_tela());
                let agora = Instant::now();
                codigo_mostrado = Some(agora);
                auth_prazo = agora + ESPERA_PELA_PESSOA;
                *prazo = (*prazo).max(agora + ESPERA_PELA_PESSOA + Duration::from_secs(30));
            }
        }
        let Some(desde) = codigo_mostrado else { continue };
        // Só o anfitrião consome a decisão aqui; a do convidado é lida no começo da volta.
        let decisao_do_anfitriao = if role == Role::Host && maquina.esperando_a_pessoa() {
            c.confirmacao.tomar_decisao()
        } else {
            None
        };
        match decisao_do_anfitriao {
            Some(decisao) => {
                let passo = match maquina.decidir(decisao) {
                    Ok(p) => p,
                    Err(e) => return Err(falhar(&maquina, link, e, true)),
                };
                if let Some(resposta) = passo.reply {
                    let _ = link.send(&SignalMessage::Pair(resposta));
                }
                if !decisao {
                    let e = Error::Pairing("a pessoa recusou o código neste aparelho".into());
                    return Err(falhar(&maquina, link, e, true));
                }
            }
            _ => {}
        }
        if maquina.esperando_a_pessoa() && desde.elapsed() > ESPERA_PELA_PESSOA {
            let e = Error::Timeout("o código expirou sem decisão".into());
            return Err(falhar(&maquina, link, e, true));
        }
    }
}

fn erro_da_tentativa(maquina: &Pairing, role: Role, e: Error) -> Error {
    if role == Role::Host
        && (maquina.e_por_codigo() || maquina.convite_do_qr().is_some())
        && !matches!(e, Error::Cancelled)
    {
        // O código e o QR não têm segredo de baixa entropia a proteger: uma troca que não fecha é
        // acidente do candidato, a espera segue com o mesmo PIN, e a tentativa já foi contada
        // (`docs/v3-no-privado.md` §3.4 e §4.3).
        return Error::Signaling(format!("o pareamento não fechou: {e}"));
    }
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
    } else if role == Role::Host
        && !maquina.attempt_started()
        && matches!(
            e,
            Error::NeedsPin(_) | Error::Pairing(_) | Error::WrongPin(_) | Error::Protocol(_)
        )
    {
        // **M4** (`docs/v3-no-privado.md` §3.7): recusado antes do KE1 — dica de retomada que não
        // casa, `Hello {pin}` numa espera sem PIN, quadro ilegível — não houve palpite de PIN
        // nenhum. É acidente do candidato: a espera segue com o mesmo PIN. Sem isto, um quadro de
        // qualquer aparelho da LAN encerrava a espera e a casca sorteava outro PIN.
        Error::Signaling(format!("candidato recusado antes do KE1: {e}"))
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
    prazo: &mut Instant,
    codigo: Option<&ConfirmacaoPorCodigo>,
) -> Result<Option<(Link, Announcement, PairOutcome)>> {
    let mut link = servidor
        .accept_cancelavel(restante(*prazo, &cfg.cancelamento)?, &cfg.cancelamento)?
        .ok_or_else(|| Error::Timeout("nenhum receptor conectou".into()))?;

    let perna = codigo.map(|confirmacao| CodigoDaPerna { confirmacao, alvo: None });
    let (resultado, papel_autenticado, convite) =
        parear_link(&mut link, cfg, Role::Host, prazo, perna, None, None)?;
    let falhou_o_qr = |e: Error| {
        if let Some(id) = &convite {
            crate::qr::registrar_desfecho(id, "failed");
        }
        e
    };
    let par = loop {
        match link.poll().map_err(falhou_o_qr)? {
            Some(SignalMessage::Hello { announcement }) => {
                conferir_identidade_autenticada(&announcement, &resultado, papel_autenticado)
                    .map_err(falhou_o_qr)?;
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
                })
                .map_err(falhou_o_qr)?;
                match &convite {
                    Some(id) => crate::qr::registrar_desfecho(id, "connected"),
                    // **O QR que virou retomada** (os dois lados com o vínculo): o celular que leu o
                    // QR também fica sabendo, e a chave lida sai da caixa.
                    None => {
                        let origem = link.peer_addr().ip();
                        if crate::qr::lidos_de(origem).iter().any(|(id, _)| *id == resultado.peer) {
                            crate::qr::registrar_desfecho(&resultado.peer, "connected");
                            crate::qr::concluir(&resultado.peer);
                        }
                    }
                }
                break announcement;
            }
            Some(outra) => {
                return Err(Error::Protocol(format!(
                    "esperava Hello cifrado, veio {outra:?}"
                )))
            }
            None => {
                restante(*prazo, &cfg.cancelamento).map_err(falhou_o_qr)?;
            }
        }
    };

    Ok(Some((link, par, resultado)))
}

pub fn hospedar(servidor: &SignalingServer, cfg: SessionConfig) -> Result<Ready> {
    hospedar_com_codigo(servidor, cfg, None)
}

/// **Quantas trocas pelo código uma espera do anfitrião aceita** (`docs/v3-no-privado.md` §3.4).
/// Contam recusa, escolha errada, compromisso ou selo que não confere e cartão que expira.
pub const TENTATIVAS_DO_CODIGO: u32 = 3;

/// Quanto a pessoa tem para decidir o cartão do código, a partir do código na tela.
pub const ESPERA_PELA_PESSOA: Duration = Duration::from_secs(60);

/// [`hospedar`], aceitando também o **código de 4 dígitos** (`docs/v3-no-privado.md` §3).
///
/// Com `codigo`, um convidado sem PIN e sem vínculo pode parear pela comparação: esta espera
/// mostra três números (`ConfirmacaoPorCodigo::retrato`) e a pessoa escolhe o que o outro aparelho
/// mostra (`ConfirmacaoPorCodigo::escolher`). No máximo [`TENTATIVAS_DO_CODIGO`] trocas por espera;
/// uma escolha errada encerra o código desta espera (P11). Nada disso gasta o PIN, que continua
/// valendo junto. Com `None` é exatamente [`hospedar`].
pub fn hospedar_com_codigo(
    servidor: &SignalingServer,
    cfg: SessionConfig,
    codigo: Option<ConfirmacaoPorCodigo>,
) -> Result<Ready> {
    let mut cfg = cfg;
    ajustar_ao_papel(&mut cfg);
    let mut prazo = Instant::now() + cfg.timeout;
    let descartados_ao_abrir = servidor.descartados();
    let mut caidos: u32 = 0;

    // 1 e 2. Aceitar, apresentar-se e parear. **Repetível de propósito**, e só isto: enquanto
    //    não se passa daqui, não existe `Session`, não existe `Track` e não há o que vazar por
    //    volta — que é a objeção registrada (dívida 21) contra laço de tentativas na casca. O
    //    prazo total e o cancelamento continuam sendo os dois únicos limites.
    let (mut link, par, resultado) = loop {
        let ativo = codigo
            .as_ref()
            .filter(|c| c.tentativas() < TENTATIVAS_DO_CODIGO && !c.bloqueado());
        match abrir_candidato(servidor, &cfg, &mut prazo, ativo) {
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
        endereco_do_par: None,
        via_do_par: None,
        caiu: None,
        silencio_do_caminho: cfg.silencio_do_caminho,
        ultimo_relato: None,
        atendente: None,
    })
}

/// **A1**: quantas pernas pelo código podem cair, por alvo, antes de o convidado parar de tentar.
pub const PERNAS_CAIDAS_POR_ALVO: usize = 3;
/// A janela em que elas contam.
pub const JANELA_DAS_PERNAS: Duration = Duration::from_secs(600);

fn pernas_caidas() -> std::sync::MutexGuard<'static, std::collections::HashMap<std::net::SocketAddr, Vec<Instant>>> {
    static PERNAS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::net::SocketAddr, Vec<Instant>>>,
    > = std::sync::OnceLock::new();
    match PERNAS.get_or_init(Default::default).lock() {
        Ok(g) => g,
        Err(envenenado) => envenenado.into_inner(),
    }
}

fn registrar_perna_caida(alvo: std::net::SocketAddr) {
    let mut p = pernas_caidas();
    let agora = Instant::now();
    let lista = p.entry(alvo).or_default();
    lista.retain(|t| agora.duration_since(*t) < JANELA_DAS_PERNAS);
    lista.push(agora);
}

fn pernas_caidas_de(alvo: std::net::SocketAddr) -> usize {
    let mut p = pernas_caidas();
    let agora = Instant::now();
    let Some(lista) = p.get_mut(&alvo) else { return 0 };
    lista.retain(|t| agora.duration_since(*t) < JANELA_DAS_PERNAS);
    lista.len()
}

/// [`conectar`], podendo parear pelo **código de 4 dígitos** (`docs/v3-no-privado.md` §3).
///
/// A ordem de sempre vale: PIN digitado primeiro, depois a retomada pelas dicas. Só sem os dois
/// entra o código — **uma tentativa por chamada** (a casca chama a cada toque em Conectar; a
/// reconexão automática e [`conectar_ao_par`] nunca usam esta função). Diante de um anfitrião que
/// não aceita o código, a conexão fecha logo depois do `Hello {codigo}` e o erro pede o PIN.
pub fn conectar_com_codigo(
    destino: std::net::SocketAddr,
    cfg: SessionConfig,
    codigo: Option<ConfirmacaoPorCodigo>,
) -> Result<Ready> {
    conectar_com_codigo_ao_alvo(destino, cfg, codigo, None)
}

/// **O toque em Conectar** (`docs/v3-no-privado.md` §8.6): [`conectar_com_codigo`] sabendo **quem**
/// a pessoa tocou (o item da lista com o nome guardado, a sessão salva tocada).
///
/// - **Com vínculo local com `alvo`**: reconecta pela rota de agora — `destino` —, só com o vínculo
///   dele (M5), sem código. Se o outro lado **não reconhece** (o `Challenge` sem a dica dele), segue
///   no mesmo toque para um pareamento novo pelo código, sem vínculo nenhum na mesa; o confirmador
///   ganha o aviso `"previous_pairing_invalid"`, e o vínculo velho só é trocado quando a sessão sobe
///   e a casca grava por cima. Uma queda de rede na retomada **não** vira pareamento novo: tenta de
///   novo pela procura da sessão salva ([`conectar_ao_par`], o `destino` e a busca) até o prazo, e
///   devolve o erro de rede se ninguém aparecer.
/// - **Sem vínculo** (ou sem `alvo`): é [`conectar_com_codigo`] — retomada se alguma dica casar,
///   senão o código.
/// - Sem confirmador, a retomada que não é reconhecida falha fechado (`ParNaoReconhecido`, A2), como
///   antes; com PIN, o PIN pareia de novo, como sempre.
pub fn conectar_com_codigo_ao_alvo(
    destino: std::net::SocketAddr,
    cfg: SessionConfig,
    codigo: Option<ConfirmacaoPorCodigo>,
    alvo: Option<&DeviceId>,
) -> Result<Ready> {
    conectar_pelo_toque(destino, cfg, codigo, alvo, |alvo, destino, cfg| {
        conectar_ao_par(alvo, Some(destino), cfg)
    })
}

/// O aviso do confirmador quando o toque cai do reconectar para o código.
pub const AVISO_PAREAMENTO_ANTERIOR: &str = "previous_pairing_invalid";

/// Uma cópia de `cfg` com outros pares, PIN e prazo (o `SessionConfig` não é `Clone`).
fn com_outros(cfg: &SessionConfig, known: PairedPeers, pin: Option<Pin>, timeout: Duration) -> SessionConfig {
    SessionConfig {
        announcement: cfg.announcement.clone(),
        pin,
        known,
        transport: cfg.transport.clone(),
        tracks: cfg.tracks.clone(),
        timeout,
        cancelamento: cfg.cancelamento.clone(),
        silencio_do_caminho: cfg.silencio_do_caminho,
    }
}

/// [`conectar_com_codigo_ao_alvo`] com a procura injetada (os testes não saem para a LAN).
fn conectar_pelo_toque(
    destino: std::net::SocketAddr,
    cfg: SessionConfig,
    codigo: Option<ConfirmacaoPorCodigo>,
    alvo: Option<&DeviceId>,
    procurar: impl FnOnce(&DeviceId, std::net::SocketAddr, SessionConfig) -> Result<Ready>,
) -> Result<Ready> {
    let Some(alvo) = alvo.filter(|a| cfg.known.get(a).is_some()) else {
        return conectar_pareando(destino, cfg, codigo);
    };
    let so_o_alvo = cfg.known.so_de(alvo);
    // **Com PIN digitado**, o PIN pareia de novo (substitui, como sempre); a retomada nem é tentada.
    if cfg.pin.is_some() {
        let pin = cfg.pin.clone();
        let timeout = cfg.timeout;
        return conectar_interno_com(
            destino,
            com_outros(&cfg, so_o_alvo, pin, timeout),
            None,
            None,
            None,
            EsperaDaRota::AtePrazo,
        );
    }
    let fim = Instant::now() + cfg.timeout;
    // 1. **Reconectando**: a retomada pela rota de agora, só com o alvo.
    let primeira = com_outros(&cfg, so_o_alvo.clone(), None, cfg.timeout);
    match conectar_interno(destino, primeira, None, Some(PRAZO_ATE_O_DESAFIO), None) {
        Ok(pronto) => return Ok(pronto),
        // O `Challenge` veio sem a dica do alvo: o outro lado não reconhece este pareamento.
        Err(Error::NeedsPin(_)) => {}
        // Cancelado; ou a dica casou e a retomada não autenticou (um relé, um segredo corrompido); ou
        // o papel do outro lado não serve. Nada disso é "pareie de novo".
        Err(e @ (Error::Cancelled | Error::Pairing(_) | Error::Protocol(_))) => return Err(e),
        // 2. **Rede**: não vira pareamento novo. A procura da sessão salva tenta de novo (o endereço do
        // item e a busca) até o prazo do toque.
        Err(e) => {
            let resto = fim.saturating_duration_since(Instant::now());
            if resto.is_zero() {
                return Err(e);
            }
            match procurar(alvo, destino, com_outros(&cfg, so_o_alvo, None, resto)) {
                Ok(pronto) => return Ok(pronto),
                Err(Error::ParNaoReconhecido(_)) => {}
                Err(e) => return Err(e),
            }
        }
    }
    // 3. **Não reconheceu**: o código, para parear de novo — só com confirmador (um toque).
    let Some(c) = codigo else {
        return Err(Error::ParNaoReconhecido(
            "o outro aparelho não reconheceu este pareamento; esqueça-o para parear de novo".into(),
        ));
    };
    c.avisar(AVISO_PAREAMENTO_ANTERIOR);
    // Sem vínculo nenhum na mesa: nada é retomado com outro par que responda neste endereço (M5), e a
    // troca vai ao código. O prazo é o do toque, de novo: é outra etapa, com uma pessoa no meio.
    let timeout = cfg.timeout;
    conectar_pareando(destino, com_outros(&cfg, PairedPeers::new(), None, timeout), Some(c))
}

/// O caminho de um toque sem vínculo com o alvo: retomada se alguma dica casar, senão o código (com
/// o limite de pernas do A1).
fn conectar_pareando(
    destino: std::net::SocketAddr,
    cfg: SessionConfig,
    codigo: Option<ConfirmacaoPorCodigo>,
) -> Result<Ready> {
    if codigo.is_some() && cfg.pin.is_none() && pernas_caidas_de(destino) >= PERNAS_CAIDAS_POR_ALVO {
        return Err(Error::Interferencia(
            "algo está interferindo na conexão com este aparelho; use o PIN".into(),
        ));
    }
    // Um toque (o código à mão, ou um PIN digitado) espera a rota até o prazo dele (bancada de 14/10).
    let espera = if codigo.is_some() || cfg.pin.is_some() { EsperaDaRota::AtePrazo } else { EsperaDaRota::Nao };
    conectar_interno_com(destino, cfg, codigo.as_ref(), None, None, espera)
}

/// **O desktop conecta no celular que chamou pelo QR** (`docs/v3-no-privado.md` §4). O endereço é o
/// de origem do chamado com a porta que ele selou; o pareamento é o OPAQUE com `K_par`
/// (`PairMode::Qr`), e o id autenticado do outro lado tem de ser o do chamado. Pareamento novo: a
/// casca guarda o segredo, e a próxima vez é retomada.
pub fn conectar_pelo_qr(
    chamado: &crate::qr::Chamado,
    chave: [u8; crate::pairing::CHAVE_DO_QR_LEN],
    cfg: SessionConfig,
) -> Result<Ready> {
    conectar_interno(chamado.endpoint, cfg, None, None, Some((chamado.par.clone(), chave)))
}

/// Lado do receptor: conecta, pareia e responde.
///
/// `destino` vem do mDNS ([`crate::discovery::DiscoveredDevice::endpoint`]) ou do que o usuário
/// digitou ([`crate::discovery::endereco_manual`]).
pub fn conectar(destino: std::net::SocketAddr, cfg: SessionConfig) -> Result<Ready> {
    conectar_interno(destino, cfg, None, None, None)
}

/// **Se o TCP que volta "sem rota" na hora espera a rota** (bancada de 14/10, Mac → iPhone 7).
///
/// No macOS e no iOS, um endereço da LAN cujo ARP falhou há pouco fica marcado como fora do ar
/// (`net.link.ether.inet.host_down_time`, 20 s no MacBook da bancada), e o `connect` devolve
/// `EHOSTUNREACH` (os error 65) **em menos de 1 ms**, sem mandar nada — medido no Mac, contra um
/// endereço da /24 que a sondagem tinha perguntado: 0,1 ms por ~40 s, e só então um ARP de novo. Um
/// celular que dormiu um pouco (ou que a sondagem da /24 pegou dormindo) aparece na lista pelo mDNS,
/// que não precisa de ARP, e o primeiro toque nele morria com `NO_ROUTE`; o segundo, segundos depois,
/// chegava. Isso não é "não há rota": é "ainda não". O toque espera até o prazo dele, perguntando
/// de novo a cada [`RECUO_DA_ROTA`].
///
/// Só o toque (o código à mão, ou o PIN digitado): a procura da sessão salva tem o recuo dela
/// ([`RECUO_DO_CANDIDATO`]) e anda pelos outros candidatos enquanto isso, e a reconexão sozinha já
/// tenta de novo a cada volta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EsperaDaRota {
    Nao,
    AtePrazo,
}

/// De quanto em quanto tempo o toque pergunta de novo pela rota.
pub const RECUO_DA_ROTA: Duration = Duration::from_millis(500);

/// `tentar` de novo enquanto ele devolver `NoRoute` e couber outro [`RECUO_DA_ROTA`] antes de `fim`.
/// O cancelamento corta a espera. Devolve o último resultado.
fn esperando_a_rota<T>(
    fim: Instant,
    cancelamento: &Cancelamento,
    mut tentar: impl FnMut() -> Result<T>,
) -> Result<T> {
    loop {
        match tentar() {
            Err(Error::NoRoute(m)) => {
                if fim.saturating_duration_since(Instant::now()) <= RECUO_DA_ROTA {
                    return Err(Error::NoRoute(m));
                }
                let volta = Instant::now() + RECUO_DA_ROTA;
                while Instant::now() < volta {
                    if cancelamento.cancelado() {
                        return Err(Error::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            outro => return outro,
        }
    }
}

#[cfg(test)]
mod espera_da_rota {
    use super::*;

    /// Bancada de 14/10: o primeiro toque voltava `NO_ROUTE` em 3 ms e o segundo, 10 s depois, chegava.
    #[test]
    fn o_toque_espera_a_rota_que_volta() {
        let mut vezes = 0;
        let r = esperando_a_rota(Instant::now() + Duration::from_secs(5), &Cancelamento::novo(), || {
            vezes += 1;
            if vezes < 3 { Err(Error::NoRoute("ainda não".into())) } else { Ok(vezes) }
        });
        assert_eq!(r.unwrap(), 3);
    }

    #[test]
    fn sem_rota_ate_o_prazo_devolve_sem_rota() {
        let inicio = Instant::now();
        let r: Result<()> = esperando_a_rota(inicio + Duration::from_millis(1200), &Cancelamento::novo(), || {
            Err(Error::NoRoute("nunca".into()))
        });
        assert!(matches!(r, Err(Error::NoRoute(_))), "{r:?}");
        assert!(inicio.elapsed() < Duration::from_millis(1500), "não passa do prazo: {:?}", inicio.elapsed());
    }

    #[test]
    fn outro_erro_nao_espera_e_o_cancelamento_corta() {
        let mut vezes = 0;
        let r: Result<()> = esperando_a_rota(Instant::now() + Duration::from_secs(5), &Cancelamento::novo(), || {
            vezes += 1;
            Err(Error::Io("recusou".into()))
        });
        assert!(matches!(r, Err(Error::Io(_))) && vezes == 1, "{r:?} {vezes}");
        let c = Cancelamento::novo();
        let inicio = Instant::now();
        let r: Result<()> = esperando_a_rota(inicio + Duration::from_secs(5), &c, || {
            c.cancelar();
            Err(Error::NoRoute("sem rota".into()))
        });
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(inicio.elapsed() < Duration::from_secs(1));
    }
}

/// `ate_o_desafio`: o prazo para conectar, abrir o WebSocket e receber o `Challenge`; depois dele,
/// o prazo cheio de `cfg.timeout` (a procura da sessão salva, revisão M-c).
fn conectar_interno(
    destino: std::net::SocketAddr,
    cfg: SessionConfig,
    codigo: Option<&ConfirmacaoPorCodigo>,
    ate_o_desafio: Option<Duration>,
    qr: Option<(DeviceId, [u8; crate::pairing::CHAVE_DO_QR_LEN])>,
) -> Result<Ready> {
    conectar_interno_com(destino, cfg, codigo, ate_o_desafio, qr, EsperaDaRota::Nao)
}

fn conectar_interno_com(
    destino: std::net::SocketAddr,
    cfg: SessionConfig,
    codigo: Option<&ConfirmacaoPorCodigo>,
    ate_o_desafio: Option<Duration>,
    qr: Option<(DeviceId, [u8; crate::pairing::CHAVE_DO_QR_LEN])>,
    espera: EsperaDaRota,
) -> Result<Ready> {
    // A sondagem da lista deixa este endereço em paz por um minuto (`docs/v3-no-privado.md` §8.9): um
    // datagrama nosso para um celular cochilando é o ARP que falha e o põe "fora do ar" (§8.8).
    crate::busca::avisar_do_toque(destino.ip());
    let mut cfg = cfg;
    ajustar_ao_papel(&mut cfg);
    let mut prazo = Instant::now() + cfg.timeout;
    let ate_o_desafio = ate_o_desafio.map(|d| (Instant::now() + d).min(prazo));

    // **A sinalização sai pela mesma interface que a mídia.** Prender só a mídia deixaria o TCP
    // sair por onde a tabela de rotas mandasse — e com vários Android ancorados, cujas sub-redes
    // podem coincidir, isso deixa de ter resposta. Ver `signaling::connect_de`.
    let origem = cfg.transport.origem()?;
    let ate_conectar = ate_o_desafio.unwrap_or(prazo);
    let discar = || connect_de(destino, restante(ate_conectar, &cfg.cancelamento)?, origem, &cfg.cancelamento);
    let mut link = match espera {
        EsperaDaRota::Nao => discar()?,
        EsperaDaRota::AtePrazo => esperando_a_rota(ate_conectar, &cfg.cancelamento, discar)?,
    };
    let perna = codigo.map(|confirmacao| CodigoDaPerna { confirmacao, alvo: Some(destino) });
    let (resultado, papel_autenticado, _) =
        parear_link(&mut link, &cfg, Role::Guest, &mut prazo, perna, ate_o_desafio, qr)?;
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
        endereco_do_par: Some(destino),
        via_do_par: None,
        caiu: None,
        silencio_do_caminho: cfg.silencio_do_caminho,
        ultimo_relato: None,
        atendente: None,
    })
}

/// **A sessão salva, sem id no anúncio** (`docs/v3-no-privado.md` §5.2): acha o aparelho `alvo` e
/// retoma com ele **na mesma conexão**.
///
/// Tenta o endereço antigo e, ao mesmo tempo, os anfitriões da lista ([`crate::busca::Busca`]:
/// mDNS, broadcast e a sondagem da /24). Em cada um, a retomada de sempre — só com o segredo do
/// `alvo` (**M5**: outro par conhecido que responda não é retomado) —; quem não tem a dica dele no
/// `Challenge` é largado antes do `Hello` e não gasta PIN nenhum lá (M4). O id autenticado tem de
/// ser o do `alvo`. **Nunca** cai no código. Sem ninguém que case até o prazo, falha fechado.
///
/// O endereço em que conectou volta em [`Ready::endereco_do_par`], e por onde em
/// [`Ready::via_do_par`] (`"endereco_antigo"`, `"mdns"`, `"udp"` ou `"sondagem"`); a casca grava o
/// novo **depois** de autenticar, como sempre.
pub fn conectar_ao_par(
    alvo: &DeviceId,
    ultimo: Option<std::net::SocketAddr>,
    cfg: SessionConfig,
) -> Result<Ready> {
    // Sem vínculo, nem abre a busca.
    if cfg.known.so_de(alvo).is_empty() {
        return Err(sem_vinculo_com_o_alvo());
    }
    let busca = crate::busca::Busca::com(crate::busca::OpcoesDaBusca {
        dica: ultimo,
        ..Default::default()
    });
    conectar_ao_par_entre(alvo, ultimo, cfg, || {
        busca
            .aparelhos()
            .into_iter()
            .filter_map(|a| {
                let via = if a.via.contains(&"mdns") {
                    "mdns"
                } else if a.via.contains(&"sondagem") {
                    "sondagem"
                } else {
                    "udp"
                };
                a.endpoint.map(|e| (e, via, a.anuncio.papel))
            })
            .collect()
    })
}

fn sem_vinculo_com_o_alvo() -> Error {
    Error::NeedsPin("não há vínculo v3 com este aparelho; é preciso parear de novo".into())
}

/// Até o `Challenge` de um candidato da procura: conectar, abrir o WebSocket, mandar o `Probe` e
/// receber o desafio (revisão M-c). Quem casa a dica ganha o prazo cheio.
pub const PRAZO_ATE_O_DESAFIO: Duration = Duration::from_millis(2500);
/// Um candidato que não chegou ao desafio (recusou o TCP, ocupado, mudo, ICE que falhou) volta a
/// ser tentado depois deste recuo (revisão M-b): só quem **respondeu o desafio sem a dica** sai da
/// procura.
pub const RECUO_DO_CANDIDATO: Duration = Duration::from_secs(1);

pub(crate) fn conectar_ao_par_entre(
    alvo: &DeviceId,
    ultimo: Option<std::net::SocketAddr>,
    cfg: SessionConfig,
    mut candidatos: impl FnMut() -> Vec<(std::net::SocketAddr, &'static str, Option<Papel>)>,
) -> Result<Ready> {
    let so_o_alvo = cfg.known.so_de(alvo);
    if so_o_alvo.is_empty() {
        return Err(sem_vinculo_com_o_alvo());
    }
    let fim = Instant::now() + cfg.timeout;
    let mut sem_a_dica: Vec<std::net::SocketAddr> = Vec::new();
    let mut recuo: std::collections::HashMap<std::net::SocketAddr, Instant> = Default::default();
    let mut ultimo_erro: Option<Error> = None;
    // Os candidatos da busca que servem ao papel (o endereço antigo fica fora: ele decide sozinho).
    let mut que_servem: Vec<std::net::SocketAddr> = Vec::new();
    loop {
        if cfg.cancelamento.cancelado() {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= fim {
            break;
        }
        // O endereço antigo entra sempre, **sem o filtro de papel** (revisão M-a): o papel dele é
        // desconhecido, e o `host_role` do `Challenge` é conferido em `parear_link`.
        let lista: Vec<(std::net::SocketAddr, &'static str, Option<Option<Papel>>)> = ultimo
            .map(|u| (u, "endereco_antigo", None))
            .into_iter()
            .chain(candidatos().into_iter().map(|(e, v, p)| (e, v, Some(p))))
            .collect();
        let mut algum = false;
        for (endereco, via, papel) in lista {
            if sem_a_dica.contains(&endereco)
                || recuo.get(&endereco).is_some_and(|t| Instant::now() < *t)
                || papel.is_some_and(|p| papel_do_anfitriao_serve(cfg.announcement.papel, p).is_err())
            {
                continue;
            }
            if via != "endereco_antigo" && !que_servem.contains(&endereco) {
                que_servem.push(endereco);
            }
            let restante = fim.saturating_duration_since(Instant::now());
            if restante.is_zero() {
                break;
            }
            algum = true;
            let tentativa = SessionConfig {
                announcement: cfg.announcement.clone(),
                pin: None,
                known: so_o_alvo.clone(),
                transport: cfg.transport.clone(),
                tracks: cfg.tracks.clone(),
                timeout: restante,
                cancelamento: cfg.cancelamento.clone(),
                silencio_do_caminho: cfg.silencio_do_caminho,
            };
            match conectar_interno(endereco, tentativa, None, Some(PRAZO_ATE_O_DESAFIO), None) {
                Ok(mut pronto) => {
                    pronto.via_do_par = Some(via);
                    return Ok(pronto);
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                // Respondeu o desafio sem a dica do alvo: não é ele (ou o `Challenge` foi trocado
                // no caminho). Sai da procura.
                Err(Error::NeedsPin(_)) => sem_a_dica.push(endereco),
                // Casou a dica e não autenticou: também não se insiste.
                Err(e @ Error::Pairing(_)) => {
                    sem_a_dica.push(endereco);
                    ultimo_erro = Some(e);
                }
                // Não chegou ao desafio, ou caiu depois dele: volta depois do recuo.
                Err(e) => {
                    recuo.insert(endereco, Instant::now() + RECUO_DO_CANDIDATO);
                    ultimo_erro = Some(e);
                }
            }
        }
        if !algum {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    // **A2 na sessão salva**: quem respondeu o desafio sem a dica do alvo é um aparelho que não
    // reconhece este pareamento — o endereço antigo, ou o único candidato que serve ao papel. Isso
    // não é prazo (ninguém respondeu): é "esqueça e pareie de novo". Com vários candidatos sem a
    // dica, nenhum deles é o alvo com certeza, e o desfecho continua sendo o prazo.
    let antigo_sem_a_dica = ultimo.is_some_and(|u| sem_a_dica.contains(&u));
    let unico_sem_a_dica = que_servem.len() == 1 && sem_a_dica.contains(&que_servem[0]);
    Err(match ultimo_erro {
        Some(e @ Error::Pairing(_)) => e,
        _ if antigo_sem_a_dica || unico_sem_a_dica => Error::ParNaoReconhecido(format!(
            "{} respondeu e não reconheceu este pareamento; esqueça-o para parear de novo",
            alvo.0
        )),
        _ => Error::Timeout(format!(
            "não achei {} nesta rede, ou ele não reconheceu este pareamento",
            alvo.0
        )),
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

/// **M4 da revisão de 12/10** (`docs/v3-no-privado.md` §3.7): uma recusa **antes do KE1** — dica de
/// retomada que não casa, `Hello {pin}` numa espera sem PIN, KE1 ilegível — é acidente do
/// candidato. A espera segue, com o mesmo PIN, e o aparelho de verdade entra em seguida. No
/// `e752ed61` puro um único quadro forjado encerrava a espera (e a casca sorteava outro PIN).
#[cfg(test)]
mod testes_do_m4 {
    use super::*;
    use crate::pairing::{PairFrame, PairMode};
    use crate::protocol::{Capabilities, DeviceId, PROTOCOL_VERSION};
    use crate::signaling::connect;
    use std::thread;

    fn anuncio(id: &str) -> Announcement {
        Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.into()),
            display_name: id.into(),
            capabilities: Capabilities { screen_source: true, camera_source: true, sink: true },
            screen: None,
            papel: None,
        }
    }

    fn config(id: &str, pin: Option<Pin>, known: PairedPeers) -> SessionConfig {
        SessionConfig {
            announcement: anuncio(id),
            pin,
            known,
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(30),
            cancelamento: Cancelamento::novo(),
            silencio_do_caminho: None,
        }
    }

    /// O intruso: `Probe`, espera o `Challenge` e manda o `Hello` dado. Devolve quando o anfitrião
    /// fecha ou em 5 s.
    fn intruso(destino: std::net::SocketAddr, hello: PairFrame) {
        let mut link = connect(destino, Duration::from_secs(10)).expect("o intruso conecta");
        link.send(&SignalMessage::Pair(PairFrame::Probe {
            version: PROTOCOL_VERSION,
            guest_nonce: "42".repeat(16),
            guest_role: 0,
        }))
        .expect("Probe");
        let ate = Instant::now() + Duration::from_secs(5);
        let mut challenge = false;
        while Instant::now() < ate && !challenge {
            match link.poll() {
                Ok(Some(SignalMessage::Pair(PairFrame::Challenge { .. }))) => challenge = true,
                Ok(_) => {}
                Err(e) => panic!("o intruso tinha de receber o Challenge: {e}"),
            }
        }
        assert!(challenge, "o Challenge não chegou");
        link.send(&SignalMessage::Pair(hello)).expect("o Hello forjado sai");
        let ate = Instant::now() + Duration::from_secs(5);
        while Instant::now() < ate {
            match link.poll() {
                Ok(_) => {}
                Err(_) => return, // o anfitrião recusou e fechou: é o esperado
            }
        }
    }

    /// Um intruso manda `hello`; depois o receptor de verdade conecta com `cfg_receptor`. A espera
    /// tem de fechar com o receptor de verdade, e o intruso conta como um candidato que caiu.
    fn a_espera_sobrevive(
        cfg_emissor: SessionConfig,
        hello: PairFrame,
        cfg_receptor: SessionConfig,
    ) {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let emissor = thread::spawn(move || hospedar(&servidor, cfg_emissor));
        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");

        intruso(destino, hello);
        let id_do_receptor = cfg_receptor.announcement.device_id.clone();
        let receptor = conectar(destino, cfg_receptor)
            .expect("o receptor de verdade tinha de entrar depois do quadro forjado");
        let pronto = emissor
            .join()
            .expect("thread do emissor")
            .expect("a espera caiu com um quadro forjado antes do KE1 (M4)");
        assert_eq!(pronto.peer.device_id, id_do_receptor, "fechou com o par errado");
        assert_eq!(pronto.descartados, 1, "o intruso conta como um candidato que caiu");
        drop(receptor);
    }

    /// Um vínculo v3 já feito entre `a` e `b`, para a espera sem PIN (só retomada).
    fn par_v3(a: &str, b: &str) -> (PairedPeers, PairedPeers) {
        let segredo = [0x5au8; 32];
        let mut de_a = PairedPeers::new();
        de_a.insert(&PairOutcome { peer: DeviceId(b.into()), secret: segredo, novo: true });
        let mut de_b = PairedPeers::new();
        de_b.insert(&PairOutcome { peer: DeviceId(a.into()), secret: segredo, novo: true });
        (de_a, de_b)
    }

    #[test]
    fn retomada_com_dica_que_nao_casa_nao_derruba_a_espera_nem_troca_o_pin() {
        let pin = Pin::parse("424242").expect("pin");
        a_espera_sobrevive(
            config("m4-emissor-a", Some(pin.clone()), PairedPeers::new()),
            PairFrame::Hello { mode: PairMode::Resume, hint: Some("ab".repeat(32)), ke1: "00".into() },
            // O mesmo PIN: se a espera tivesse trocado, o receptor sairia com WrongPin.
            config("m4-receptor-a", Some(pin), PairedPeers::new()),
        );
    }

    #[test]
    fn hello_de_pin_numa_espera_sem_pin_nao_derruba_a_espera() {
        let (do_emissor, do_receptor) = par_v3("m4-emissor-b", "m4-receptor-b");
        a_espera_sobrevive(
            config("m4-emissor-b", None, do_emissor),
            PairFrame::Hello { mode: PairMode::Pin, hint: None, ke1: "00".into() },
            config("m4-receptor-b", None, do_receptor),
        );
    }

    #[test]
    fn dica_ilegivel_nao_derruba_a_espera() {
        let pin = Pin::parse("515151").expect("pin");
        a_espera_sobrevive(
            config("m4-emissor-c", Some(pin.clone()), PairedPeers::new()),
            PairFrame::Hello { mode: PairMode::Resume, hint: Some("zz".into()), ke1: "00".into() },
            config("m4-receptor-c", Some(pin), PairedPeers::new()),
        );
    }

    /// O KE1 de PIN aceito continua gastando a espera: é a regra do v3 contra o PIN de baixa entropia.
    #[test]
    fn um_ke1_de_pin_aceito_continua_gastando_a_espera() {
        let pin = Pin::parse("616161").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let cfg = config("m4-emissor-d", Some(pin), PairedPeers::new());
        let emissor = thread::spawn(move || hospedar(&servidor, cfg));
        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let errado = conectar(destino, config("m4-receptor-d", Some(Pin::parse("161616").expect("pin")), PairedPeers::new()));
        assert!(matches!(errado, Err(Error::WrongPin(_))), "{:?}", errado.err());
        let r = emissor.join().expect("thread do emissor");
        assert!(matches!(r, Err(Error::Pairing(_))), "a tentativa de PIN tinha de gastar a espera: {:?}", r.err());
    }
}

/// O código de 4 dígitos e a sessão salva sobre o v3, com a sinalização de verdade em loopback
/// (`docs/v3-no-privado.md` §3 e §5). Cada teste diz qual achado da revisão adversarial ele fixa.
#[cfg(test)]
mod testes_do_codigo {
    use super::*;
    use crate::pairing::{PairFrame, PairOutcome};
    use crate::protocol::{Capabilities, DeviceId, PROTOCOL_VERSION};
    use std::thread;

    fn anuncio(id: &str, nome: &str) -> Announcement {
        Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.into()),
            display_name: nome.into(),
            capabilities: Capabilities { screen_source: true, camera_source: true, sink: true },
            screen: None,
            papel: None,
        }
    }

    fn config(id: &str, nome: &str, pin: Option<&str>, known: PairedPeers, cancelar: &Cancelamento) -> SessionConfig {
        SessionConfig {
            announcement: anuncio(id, nome),
            pin: pin.map(|p| Pin::parse(p).expect("pin")),
            known,
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(30),
            cancelamento: cancelar.clone(),
            silencio_do_caminho: None,
        }
    }

    /// Um anfitrião numa thread, com o código ligado. Devolve o endereço e a thread.
    fn anfitriao(
        id: &'static str,
        nome: &'static str,
        pin: Option<&'static str>,
        known: PairedPeers,
        codigo: Option<ConfirmacaoPorCodigo>,
        cancelar: &Cancelamento,
    ) -> (std::net::SocketAddr, thread::JoinHandle<Result<Ready>>) {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let cfg = config(id, nome, pin, known, cancelar);
        let t = thread::spawn(move || hospedar_com_codigo(&servidor, cfg, codigo));
        (format!("127.0.0.1:{porta}").parse().expect("endereço"), t)
    }

    /// A pessoa diante do anfitrião: quando os três números aparecem, toca o que `escolha` disser
    /// (recebe as opções e o código que o convidado mostra).
    fn pessoa(
        h: ConfirmacaoPorCodigo,
        g: ConfirmacaoPorCodigo,
        escolha: impl Fn(&[String], &str) -> Option<usize> + Send + 'static,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let fim = Instant::now() + Duration::from_secs(20);
            while Instant::now() < fim {
                let rh = h.retrato();
                let rg = g.retrato();
                if rh.fase == Fase::Mostrando {
                    if let Some(codigo_g) = rg.codigo {
                        match escolha(&rh.opcoes, &codigo_g) {
                            Some(i) => {
                                h.escolher(rh.tentativas, i);
                            }
                            None => {
                                h.recusar(rh.tentativas);
                            }
                        }
                        return;
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        })
    }

    fn certo(opcoes: &[String], codigo: &str) -> Option<usize> {
        opcoes.iter().position(|o| o == codigo)
    }

    fn errado(opcoes: &[String], codigo: &str) -> Option<usize> {
        opcoes.iter().position(|o| o != codigo)
    }

    fn so_um(outcome: &PairOutcome) -> PairedPeers {
        let mut t = PairedPeers::new();
        t.insert(outcome);
        t
    }

    /// A sequência completa: sem PIN, a pessoa escolhe no anfitrião o número que o convidado
    /// mostra, os dois guardam o mesmo segredo, e a próxima vez é retomada, sem código.
    #[test]
    fn sem_pin_pela_escolha_certa_e_a_proxima_e_retomada() {
        let parar = Cancelamento::novo();
        let (c_h, c_g) = (ConfirmacaoPorCodigo::nova(), ConfirmacaoPorCodigo::nova());
        let (destino, host) = anfitriao("cod-anfitriao", "MacBook", Some("424242"), PairedPeers::new(), Some(c_h.clone()), &parar);
        let p = pessoa(c_h.clone(), c_g.clone(), certo);
        let g = conectar_com_codigo(destino, config("cod-convidado", "Galaxy A07", None, PairedPeers::new(), &parar), Some(c_g.clone()))
            .expect("o convidado entra pelo código");
        let h = host.join().expect("thread").expect("o anfitrião fecha");
        p.join().expect("pessoa");
        assert_eq!(g.outcome.secret, h.outcome.secret);
        assert!(g.outcome.novo && h.outcome.novo);
        assert_eq!((g.peer.device_id.0.as_str(), h.peer.device_id.0.as_str()), ("cod-anfitriao", "cod-convidado"));
        assert_eq!(c_h.retrato().par.as_deref(), Some("Galaxy A07"), "o cartão do anfitrião disse quem pedia");
        assert_eq!(c_g.retrato().par.as_deref(), Some("MacBook"), "o nome do anfitrião chegou no fim (M2)");
        assert_eq!((c_h.retrato().fase, c_g.retrato().fase), (Fase::Conferiu, Fase::Conferiu));
        assert_eq!(g.endereco_do_par, Some(destino));
        let (vinculo_g, vinculo_h) = (so_um(&g.outcome), so_um(&h.outcome));
        drop((g, h));

        // A próxima: retomada, sem código e sem PIN.
        let c_h2 = ConfirmacaoPorCodigo::nova();
        let (destino, host) = anfitriao("cod-anfitriao", "MacBook", None, vinculo_h, Some(c_h2.clone()), &parar);
        let g = conectar_com_codigo(destino, config("cod-convidado", "Galaxy A07", None, vinculo_g, &parar), Some(ConfirmacaoPorCodigo::nova()))
            .expect("retomada");
        let h = host.join().expect("thread").expect("anfitrião");
        assert!(!g.outcome.novo && !h.outcome.novo, "retomada, não pareamento novo");
        assert_eq!(c_h2.retrato().tentativas, 0, "nenhum código");
    }

    /// **O anfitrião que não aceita o código** (bancada de 13/10: o Controlar do iPhone 7 no
    /// prompter R5 do S24, que hospedava sem confirmador): o convidado ouve "digite o PIN"
    /// (`NeedsPin`), e o cartão **não** diz "o outro aparelho recusou" — ninguém viu código nenhum.
    /// A espera do anfitrião segue com o mesmo PIN, e o PIN entra em seguida.
    #[test]
    fn anfitriao_sem_codigo_pede_o_pin_e_o_cartao_nao_diz_recusou() {
        let parar = Cancelamento::novo();
        let (destino, host) = anfitriao("sem-cod-anfitriao", "S24", Some("737373"), PairedPeers::new(), None, &parar);
        let c_g = ConfirmacaoPorCodigo::nova();
        let r = conectar_com_codigo(destino, config("sem-cod-convidado", "iPhone 7", None, PairedPeers::new(), &parar), Some(c_g.clone()));
        assert!(matches!(&r, Err(Error::NeedsPin(_))), "{:?}", r.err());
        let retrato = c_g.retrato();
        assert_eq!(retrato.fase, Fase::Esperando, "sem código na tela, ninguém recusou");
        assert_eq!(retrato.recusa, Some("code_not_accepted"));
        let g = conectar(destino, config("sem-cod-convidado", "iPhone 7", Some("737373"), PairedPeers::new(), &parar))
            .expect("o PIN da tela do anfitrião entra");
        let h = host.join().expect("thread").expect("anfitrião");
        assert_eq!(h.peer.device_id.0, "sem-cod-convidado");
        drop(g);
    }

    /// **P11**: um erro na escolha recusa, e o código desta espera acaba — o próximo `Hello
    /// {codigo}` é recusado e o convidado ouve "digite o PIN" —, mas a espera segue com o mesmo PIN.
    #[test]
    fn escolha_errada_encerra_o_codigo_da_espera_e_o_pin_continua() {
        let parar = Cancelamento::novo();
        let (c_h, c_g) = (ConfirmacaoPorCodigo::nova(), ConfirmacaoPorCodigo::nova());
        let (destino, host) = anfitriao("p11-anfitriao", "Mac", Some("515151"), PairedPeers::new(), Some(c_h.clone()), &parar);
        let p = pessoa(c_h.clone(), c_g.clone(), errado);
        let r = conectar_com_codigo(destino, config("p11-convidado", "A07", None, PairedPeers::new(), &parar), Some(c_g.clone()));
        p.join().expect("pessoa");
        assert!(matches!(&r, Err(Error::Pairing(m)) if m.contains("recusou")), "{:?}", r.err());
        assert!(c_h.bloqueado() && c_h.retrato().escolha_errada);
        assert_eq!(c_g.retrato().fase, Fase::RecusadoLa);

        let r = conectar_com_codigo(destino, config("p11-outro", "Outro", None, PairedPeers::new(), &parar), Some(ConfirmacaoPorCodigo::nova()));
        assert!(matches!(&r, Err(Error::NeedsPin(_))), "{:?}", r.err());
        assert_eq!(c_h.retrato().tentativas, 1, "o segundo Hello {{codigo}} nem começou");

        let g = conectar(destino, config("p11-pin", "Com PIN", Some("515151"), PairedPeers::new(), &parar))
            .expect("o mesmo PIN continua valendo");
        let h = host.join().expect("thread").expect("anfitrião");
        assert_eq!(h.peer.device_id.0, "p11-pin");
        assert!(h.descartados >= 2);
        drop(g);
    }

    /// No máximo três trocas pelo código por espera; a quarta pede o PIN.
    #[test]
    fn tres_recusas_esgotam_o_codigo_da_espera() {
        let parar = Cancelamento::novo();
        let c_h = ConfirmacaoPorCodigo::nova();
        let (destino, host) = anfitriao("lim-anfitriao", "Mac", Some("616161"), PairedPeers::new(), Some(c_h.clone()), &parar);
        for i in 0..TENTATIVAS_DO_CODIGO {
            let c_g = ConfirmacaoPorCodigo::nova();
            let p = pessoa(c_h.clone(), c_g.clone(), |_, _| None);
            let r = conectar_com_codigo(destino, config(&format!("lim-{i}"), "A07", None, PairedPeers::new(), &parar), Some(c_g));
            p.join().expect("pessoa");
            assert!(matches!(&r, Err(Error::Pairing(_))), "{:?}", r.err());
        }
        let r = conectar_com_codigo(destino, config("lim-quarto", "A07", None, PairedPeers::new(), &parar), Some(ConfirmacaoPorCodigo::nova()));
        assert!(matches!(&r, Err(Error::NeedsPin(_))), "{:?}", r.err());
        assert_eq!(c_h.retrato().tentativas, TENTATIVAS_DO_CODIGO);
        parar.cancelar();
        assert!(matches!(host.join().expect("thread"), Err(Error::Cancelled)));
    }

    /// **A1**: o relé que larga a perna do convidado depois de ver `n_g` e antes do código. Cada
    /// perna dessas conta contra o alvo; na terceira o convidado para, sem nem conectar.
    #[test]
    fn pernas_que_caem_antes_do_codigo_param_o_convidado() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let destino: std::net::SocketAddr = format!("127.0.0.1:{}", servidor.port().expect("porta")).parse().expect("e");
        let rele = thread::spawn(move || {
            let mut pernas = 0;
            while let Ok(Some(mut link)) = servidor.accept(Duration::from_secs(5)) {
                let mut h = Pairing::new_with_store(Role::Host, DeviceId("rele".into()), None, &PairedPeers::new()).expect("h");
                h.bind_local_role(0).expect("papel");
                h.com_codigo(true, "Relé").expect("código");
                let fim = Instant::now() + Duration::from_secs(5);
                while Instant::now() < fim {
                    match link.poll() {
                        Ok(Some(SignalMessage::Pair(PairFrame::CodigoNonce { .. }))) => break, // larga a perna
                        Ok(Some(SignalMessage::Pair(q))) => {
                            if let Some(r) = h.step(q).expect("passo").reply {
                                link.send(&SignalMessage::Pair(r)).expect("envio");
                            }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                pernas += 1;
                drop(link);
            }
            pernas
        });
        let parar = Cancelamento::novo();
        for i in 0..PERNAS_CAIDAS_POR_ALVO {
            let c = ConfirmacaoPorCodigo::nova();
            let r = conectar_com_codigo(destino, config(&format!("a1-{i}"), "A07", None, PairedPeers::new(), &parar), Some(c.clone()));
            assert!(r.is_err());
            assert_eq!(c.retrato().codigo, None, "o código nunca chegou à tela");
        }
        assert_eq!(pernas_caidas_de(destino), PERNAS_CAIDAS_POR_ALVO);
        let r = conectar_com_codigo(destino, config("a1-ultimo", "A07", None, PairedPeers::new(), &parar), Some(ConfirmacaoPorCodigo::nova()));
        assert!(matches!(&r, Err(Error::Interferencia(_))), "{:?}", r.err());
        assert_eq!(rele.join().expect("relé"), PERNAS_CAIDAS_POR_ALVO, "a quarta nem conectou");
    }

    /// **A1**: a reconexão (`conectar`) nunca manda `Hello {codigo}`, mesmo diante de um anfitrião
    /// que aceita o código.
    #[test]
    fn conectar_sem_confirmador_nunca_tenta_o_codigo() {
        let parar = Cancelamento::novo();
        let c_h = ConfirmacaoPorCodigo::nova();
        let (destino, host) = anfitriao("rec-anfitriao", "Mac", None, PairedPeers::new(), Some(c_h.clone()), &parar);
        let r = conectar(destino, config("rec-convidado", "A07", None, PairedPeers::new(), &parar));
        assert!(matches!(&r, Err(Error::NeedsPin(_))), "{:?}", r.err());
        assert_eq!(c_h.retrato().tentativas, 0);
        parar.cancelar();
        let _ = host.join();
    }

    /// Um vínculo v3 com `id`, só deste lado (o outro o esqueceu, ou um relé trocou as dicas).
    fn vinculo_so_daqui(id: &str) -> PairedPeers {
        so_um(&PairOutcome { peer: DeviceId(id.into()), secret: [0x33; 32], novo: true })
    }

    /// **A2**: com vínculo v3 com o alvo e nenhuma dica dele no `Challenge` (que não é
    /// autenticado), o convidado falha fechado: nenhum `Hello {codigo}` sai, e o segredo guardado
    /// não é trocado. O mesmo na procura da sessão salva.
    #[test]
    fn com_vinculo_e_sem_a_dica_falha_fechado_sem_cair_no_codigo() {
        let parar = Cancelamento::novo();
        let c_h = ConfirmacaoPorCodigo::nova();
        let (destino, host) = anfitriao("a2-anfitriao", "Mac", Some("717171"), PairedPeers::new(), Some(c_h.clone()), &parar);
        let alvo = DeviceId("a2-anfitriao".into());
        // Sem confirmador: a reconexão automática, que nunca abre o código (§3.4). Com ele, o toque
        // seguiria para o código (§8.6, `o_toque_*`).
        let r = conectar_com_codigo_ao_alvo(
            destino,
            config("a2-convidado", "A07", None, vinculo_so_daqui(&alvo.0), &parar),
            None,
            Some(&alvo),
        );
        assert!(matches!(&r, Err(Error::ParNaoReconhecido(_))), "{:?}", r.err());
        assert_eq!(c_h.retrato().tentativas, 0, "nenhum código começou");

        let mut cfg = config("a2-convidado", "A07", None, vinculo_so_daqui(&alvo.0), &parar);
        cfg.timeout = Duration::from_secs(3);
        let r = conectar_ao_par_entre(&alvo, Some(destino), cfg, Vec::new);
        // O endereço antigo respondeu o desafio sem a dica: A2, não prazo (casca Android).
        assert!(matches!(&r, Err(Error::ParNaoReconhecido(_))), "{:?}", r.err());
        assert_eq!(c_h.retrato().tentativas, 0);

        // E a espera do anfitrião sobreviveu às duas, com o mesmo PIN.
        let g = conectar(destino, config("a2-pin", "Com PIN", Some("717171"), PairedPeers::new(), &parar)).expect("PIN");
        let h = host.join().expect("thread").expect("anfitrião");
        assert_eq!(h.peer.device_id.0, "a2-pin");
        drop(g);
    }

    /// **M5** e a sessão salva sem id no anúncio: o convidado conhece A e B, procura B, e A responde
    /// primeiro — A não é retomado; B é, na mesma conexão, e o endereço antigo morto não prende a
    /// procura.
    #[test]
    fn a_procura_da_sessao_salva_so_retoma_com_o_alvo() {
        let parar = Cancelamento::novo();
        let (s_a, s_b) = ([0xa1u8; 32], [0xb2u8; 32]);
        let vinculo = |de: &str, segredo: [u8; 32]| PairOutcome { peer: DeviceId(de.into()), secret: segredo, novo: true };
        let (end_a, host_a) = anfitriao("m5-a", "A", None, so_um(&vinculo("m5-g", s_a)), None, &parar);
        let (end_b, host_b) = anfitriao("m5-b", "B", None, so_um(&vinculo("m5-g", s_b)), None, &parar);
        let mut do_g = PairedPeers::new();
        do_g.insert(&vinculo("m5-a", s_a));
        do_g.insert(&vinculo("m5-b", s_b));
        let morto: std::net::SocketAddr = "127.0.0.1:1".parse().expect("e");
        let g = conectar_ao_par_entre(&DeviceId("m5-b".into()), Some(morto), config("m5-g", "G", None, do_g, &parar), || {
            vec![(end_a, "udp", None), (end_b, "sondagem", None)]
        })
        .expect("achou B");
        assert_eq!(g.peer.device_id.0, "m5-b");
        assert_eq!(g.endereco_do_par, Some(end_b));
        assert_eq!(g.via_do_par, Some("sondagem"));
        assert!(!g.outcome.novo);
        let h_b = host_b.join().expect("thread").expect("B");
        assert_eq!(h_b.peer.device_id.0, "m5-g");
        // A só viu um candidato que caiu antes do KE1, e continua esperando.
        thread::sleep(Duration::from_millis(200));
        assert!(!host_a.is_finished(), "A não foi retomado nem derrubado");
        parar.cancelar();
        assert!(matches!(host_a.join().expect("thread"), Err(Error::Cancelled)));
    }

    /// **A2 na sessão salva, com o papel de vídeo e o controle**: o único candidato que serve
    /// responde sem a dica (o outro lado esqueceu) → `ParNaoReconhecido`; dois candidatos sem a
    /// dica, ou ninguém respondendo, → `Timeout`.
    #[test]
    fn a_sessao_salva_distingue_quem_esqueceu_de_quem_nao_respondeu() {
        let parar = Cancelamento::novo();
        let esqueceu = |alvo: &str| {
            let mut t = PairedPeers::new();
            t.insert(&PairOutcome { peer: DeviceId(alvo.into()), secret: [0x6c; 32], novo: true });
            t
        };
        // Vídeo: o único candidato da busca esqueceu.
        let (end, host) = anfitriao("a2s-h", "Mac", Some("626262"), PairedPeers::new(), None, &parar);
        let mut cfg = config("a2s-g", "A07", None, esqueceu("a2s-h"), &parar);
        cfg.timeout = Duration::from_secs(3);
        let r = conectar_ao_par_entre(&DeviceId("a2s-h".into()), None, cfg, || vec![(end, "udp", None)]);
        assert!(matches!(&r, Err(Error::ParNaoReconhecido(_))), "{:?}", r.err());

        // Dois candidatos sem a dica: nenhum é o alvo com certeza.
        let (end2, host2) = anfitriao("a2s-h2", "Outro", Some("636363"), PairedPeers::new(), None, &parar);
        let mut cfg = config("a2s-g", "A07", None, esqueceu("a2s-h"), &parar);
        cfg.timeout = Duration::from_secs(3);
        let r = conectar_ao_par_entre(&DeviceId("a2s-h".into()), None, cfg, || {
            vec![(end, "udp", None), (end2, "sondagem", None)]
        });
        assert!(matches!(&r, Err(Error::Timeout(_))), "{:?}", r.err());

        // Ninguém respondeu.
        let morto: std::net::SocketAddr = "127.0.0.1:1".parse().expect("e");
        let mut cfg = config("a2s-g", "A07", None, esqueceu("a2s-h"), &parar);
        cfg.timeout = Duration::from_secs(2);
        let r = conectar_ao_par_entre(&DeviceId("a2s-h".into()), Some(morto), cfg, Vec::new);
        assert!(matches!(&r, Err(Error::Timeout(_))), "{:?}", r.err());

        parar.cancelar();
        let _ = (host.join(), host2.join());
    }

    #[test]
    fn sem_vinculo_com_o_alvo_a_procura_pede_o_pin() {
        let parar = Cancelamento::novo();
        let r = conectar_ao_par_entre(&DeviceId("ninguem".into()), None, config("g", "G", None, PairedPeers::new(), &parar), Vec::new);
        assert!(matches!(&r, Err(Error::NeedsPin(_))), "{:?}", r.err());
    }
}

/// A revisão adversarial de `986a6f1f` (médios M-a…M-c e os baixos 2–4), cada um com o seu teste.
#[cfg(test)]
mod testes_da_revisao_do_codigo {
    use super::*;
    use crate::pairing::{PairFrame, PairOutcome};
    use crate::protocol::{Capabilities, DeviceId, PROTOCOL_VERSION};
    use std::thread;

    fn anuncio(id: &str, papel: Option<Papel>) -> Announcement {
        Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.into()),
            display_name: format!("{id} nome"),
            capabilities: Capabilities { screen_source: true, camera_source: true, sink: true },
            screen: None,
            papel,
        }
    }

    fn config(id: &str, papel: Option<Papel>, pin: Option<&str>, known: PairedPeers, parar: &Cancelamento) -> SessionConfig {
        SessionConfig {
            announcement: anuncio(id, papel),
            pin: pin.map(|p| Pin::parse(p).expect("pin")),
            known,
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(30),
            cancelamento: parar.clone(),
            silencio_do_caminho: None,
        }
    }

    fn vinculo(de: &str, segredo: [u8; 32]) -> PairedPeers {
        let mut t = PairedPeers::new();
        t.insert(&PairOutcome { peer: DeviceId(de.into()), secret: segredo, novo: true });
        t
    }

    fn anfitriao_em(
        servidor: SignalingServer,
        cfg: SessionConfig,
        codigo: Option<ConfirmacaoPorCodigo>,
    ) -> thread::JoinHandle<Result<Ready>> {
        thread::spawn(move || hospedar_com_codigo(&servidor, cfg, codigo))
    }

    fn servidor() -> (SignalingServer, std::net::SocketAddr) {
        let s = SignalingServer::bind(0).expect("bind");
        let e = format!("127.0.0.1:{}", s.port().expect("porta")).parse().expect("e");
        (s, e)
    }

    /// **M-a**: o controle de um teleprompter acha o prompter pelo endereço antigo — o filtro de
    /// papel não vale para ele (o papel dele é desconhecido; o `Challenge` confere).
    #[test]
    fn o_controle_acha_o_prompter_pelo_endereco_antigo() {
        let parar = Cancelamento::novo();
        let segredo = [0x4au8; 32];
        let (s, end) = servidor();
        let host = anfitriao_em(s, config("ma-prompter", Some(Papel::Teleprompter), None, vinculo("ma-controle", segredo), &parar), None);
        let g = conectar_ao_par_entre(
            &DeviceId("ma-prompter".into()),
            Some(end),
            config("ma-controle", Some(Papel::ControleRemoto), None, vinculo("ma-prompter", segredo), &parar),
            Vec::new,
        )
        .expect("o controle achou o prompter no endereço antigo");
        assert_eq!(g.via_do_par, Some("endereco_antigo"));
        assert_eq!(host.join().expect("thread").expect("prompter").peer.device_id.0, "ma-controle");
    }

    /// **A2 no controle** (casca Android): o prompter esqueceu este controle e responde o desafio
    /// sem a dica no endereço antigo → `ParNaoReconhecido`, não `Timeout`.
    #[test]
    fn o_controle_esquecido_pelo_prompter_ouve_par_nao_reconhecido() {
        let parar = Cancelamento::novo();
        let (s, end) = servidor();
        let host = anfitriao_em(s, config("mz-prompter", Some(Papel::Teleprompter), Some("646464"), PairedPeers::new(), &parar), None);
        let mut cfg = config("mz-controle", Some(Papel::ControleRemoto), None, vinculo("mz-prompter", [0x4d; 32]), &parar);
        cfg.timeout = Duration::from_secs(3);
        let r = conectar_ao_par_entre(&DeviceId("mz-prompter".into()), Some(end), cfg, Vec::new);
        assert!(matches!(&r, Err(Error::ParNaoReconhecido(_))), "{:?}", r.err());
        parar.cancelar();
        let _ = host.join();
    }

    /// **M-b**: o endereço que recusou o TCP agora volta a ser tentado depois do recuo — o
    /// aparelho que ainda não tinha aberto a espera é achado quando abre.
    #[test]
    fn quem_recusou_o_tcp_volta_depois_do_recuo() {
        let parar = Cancelamento::novo();
        let segredo = [0x5bu8; 32];
        let porta = std::net::TcpListener::bind("127.0.0.1:0").expect("l").local_addr().expect("a").port();
        let end: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().expect("e");
        let parar_h = parar.clone();
        let host = thread::spawn(move || {
            thread::sleep(Duration::from_millis(1500)); // a espera abre depois da primeira tentativa
            let s = SignalingServer::bind(porta).expect("bind tardio");
            hospedar(&s, config("mb-h", None, None, vinculo("mb-g", segredo), &parar_h))
        });
        let mut cfg = config("mb-g", None, None, vinculo("mb-h", segredo), &parar);
        cfg.timeout = Duration::from_secs(10);
        let g = conectar_ao_par_entre(&DeviceId("mb-h".into()), Some(end), cfg, Vec::new).expect("achou depois do recuo");
        assert_eq!(g.peer.device_id.0, "mb-h");
        host.join().expect("thread").expect("h");
    }

    /// **M-c**: um candidato mudo (abre o WebSocket e não manda o desafio) não consome a procura:
    /// o prazo até o desafio é curto, e o aparelho certo, depois dele, é achado bem antes do fim.
    #[test]
    fn um_candidato_mudo_nao_consome_a_procura() {
        let parar = Cancelamento::novo();
        let segredo = [0x6cu8; 32];
        let (mudo, end_mudo) = servidor();
        let segura = thread::spawn(move || {
            let link = mudo.accept(Duration::from_secs(10)).ok().flatten();
            thread::sleep(Duration::from_secs(8));
            drop(link);
        });
        let (s, end) = servidor();
        let host = anfitriao_em(s, config("mc-h", None, None, vinculo("mc-g", segredo), &parar), None);
        let mut cfg = config("mc-g", None, None, vinculo("mc-h", segredo), &parar);
        cfg.timeout = Duration::from_secs(20);
        let inicio = Instant::now();
        let g = conectar_ao_par_entre(&DeviceId("mc-h".into()), None, cfg, || {
            vec![(end_mudo, "udp", None), (end, "udp", None)]
        })
        .expect("achou depois do mudo");
        assert_eq!(g.peer.device_id.0, "mc-h");
        assert!(inicio.elapsed() < PRAZO_ATE_O_DESAFIO + Duration::from_secs(4), "{:?}", inicio.elapsed());
        host.join().expect("thread").expect("h");
        let _ = segura.join();
    }

    /// **Baixo 2 (a)**: com o alvo e o vínculo, só o vínculo do alvo entra — o DHCP deu o IP do
    /// alvo a outro par conhecido (A), e a retomada **não** fecha com A.
    #[test]
    fn com_o_alvo_nao_retoma_com_outro_par_no_mesmo_ip() {
        let parar = Cancelamento::novo();
        let (s_a, s_b) = ([0x7au8; 32], [0x7bu8; 32]);
        let (s, end) = servidor();
        let c_a = ConfirmacaoPorCodigo::nova();
        let host_a = anfitriao_em(s, config("b2-a", None, None, vinculo("b2-g", s_a), &parar), Some(c_a.clone()));
        let mut do_g = vinculo("b2-a", s_a);
        do_g.merge(&vinculo("b2-b", s_b));
        // Sem confirmador (sem toque), a retomada não reconhecida falha fechado; com ele, seria o código
        // para parear de novo (§8.6) — o que não muda é que A **não** é retomado.
        let r = conectar_com_codigo_ao_alvo(end, config("b2-g", None, None, do_g, &parar), None, Some(&DeviceId("b2-b".into())));
        assert!(matches!(&r, Err(Error::ParNaoReconhecido(_))), "{:?}", r.err());
        assert_eq!(c_a.retrato().tentativas, 0, "nenhum código");
        thread::sleep(Duration::from_millis(200));
        assert!(!host_a.is_finished(), "A não foi retomado");
        parar.cancelar();
        let _ = host_a.join();
    }

    fn pessoa(h: ConfirmacaoPorCodigo, g: ConfirmacaoPorCodigo, recusar_no_convidado: bool) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let fim = Instant::now() + Duration::from_secs(20);
            while Instant::now() < fim {
                let (rh, rg) = (h.retrato(), g.retrato());
                assert_eq!(rh.codigo, None, "baixo 1: o anfitrião não recebe o código");
                if rh.fase == Fase::Mostrando {
                    if let Some(codigo) = rg.codigo {
                        if recusar_no_convidado {
                            assert!(g.recusar(rg.tentativas));
                        }
                        let i = rh.opcoes.iter().position(|o| *o == codigo).expect("certo");
                        h.escolher(rh.tentativas, i);
                        return;
                    }
                }
                thread::sleep(Duration::from_millis(10));
            }
        })
    }

    /// **O código substitui o vínculo que não retomou** (§8.6; até 13/10 era a B2 do código, que
    /// recusava no fim). O convidado que ainda guarda o anfitrião (que o esqueceu) cai no código sem
    /// saber quem é, e no fim grava por cima; o anfitrião que ainda guarda o convidado (que o esqueceu)
    /// também. Nos dois, só depois da escolha certa, e os dois lados ficam com o mesmo segredo novo.
    #[test]
    fn o_codigo_substitui_o_vinculo_que_nao_retomou() {
        let parar = Cancelamento::novo();
        // O convidado lembra; o anfitrião esqueceu.
        let (s, end) = servidor();
        let (c_h, c_g) = (ConfirmacaoPorCodigo::nova(), ConfirmacaoPorCodigo::nova());
        let host = anfitriao_em(s, config("b2b-h", None, Some("818181"), PairedPeers::new(), &parar), Some(c_h.clone()));
        let p = pessoa(c_h.clone(), c_g.clone(), false);
        let lembrado = vinculo("b2b-h", [0x11; 32]);
        let g = conectar_com_codigo(end, config("b2b-g", None, None, lembrado, &parar), Some(c_g.clone()))
            .unwrap_or_else(|e| panic!("o código pareia de novo: {e}"));
        p.join().expect("pessoa");
        let h = host.join().expect("thread").expect("h");
        assert!(g.outcome.novo && h.outcome.novo);
        assert_eq!(g.outcome.secret, h.outcome.secret);
        assert_ne!(g.outcome.secret, [0x11; 32], "o segredo velho saiu");
        assert_eq!(c_g.retrato().recusa, None);
        drop((g, h));

        // O anfitrião lembra; o convidado esqueceu.
        let (s, end) = servidor();
        let (c_h, c_g) = (ConfirmacaoPorCodigo::nova(), ConfirmacaoPorCodigo::nova());
        let host = anfitriao_em(s, config("b2c-h", None, None, vinculo("b2c-g", [0x22; 32]), &parar), Some(c_h.clone()));
        let p = pessoa(c_h.clone(), c_g.clone(), false);
        let g = conectar_com_codigo(end, config("b2c-g", None, None, PairedPeers::new(), &parar), Some(c_g))
            .unwrap_or_else(|e| panic!("o código pareia de novo: {e}"));
        p.join().expect("pessoa");
        let h = host.join().expect("thread").expect("h");
        assert_eq!(h.peer.device_id.0, "b2c-g");
        assert_eq!(g.outcome.secret, h.outcome.secret);
        assert_ne!(h.outcome.secret, [0x22; 32], "o anfitrião grava por cima");
        assert_eq!(c_h.retrato().recusa, None, "nenhuma recusa por vínculo");
    }

    /// Um servidor que aceita e derruba toda conexão antes do `Challenge`: a queda de rede.
    fn derruba_tudo() -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicBool>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let end = l.local_addr().expect("end");
        let parar = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let p = Arc::clone(&parar);
        l.set_nonblocking(true).expect("nb");
        thread::spawn(move || {
            while !p.load(std::sync::atomic::Ordering::Relaxed) {
                match l.accept() {
                    Ok((c, _)) => drop(c),
                    Err(_) => thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        (end, parar)
    }

    /// **O toque em Conectar** (§8.6), as quatro situações, numa sessão de verdade por 127.0.0.1.
    ///
    /// 1. Os dois com o vínculo: reconecta, sem código, o segredo fica.
    #[test]
    fn o_toque_com_o_vinculo_dos_dois_reconecta() {
        let parar = Cancelamento::novo();
        let segredo = [0x31; 32];
        let (s, end) = servidor();
        let c_h = ConfirmacaoPorCodigo::nova();
        let host = anfitriao_em(s, config("tq1-h", None, None, vinculo("tq1-g", segredo), &parar), Some(c_h.clone()));
        let c_g = ConfirmacaoPorCodigo::nova();
        let g = conectar_pelo_toque(end, config("tq1-g", None, None, vinculo("tq1-h", segredo), &parar), Some(c_g.clone()),
            Some(&DeviceId("tq1-h".into())), |_, _, _| panic!("não procura"))
            .expect("reconecta");
        let h = host.join().expect("thread").expect("h");
        assert!(!g.outcome.novo && !h.outcome.novo, "retomada");
        assert_eq!(g.outcome.secret, segredo);
        assert_eq!((c_h.retrato().tentativas, c_g.retrato().tentativas), (0, 0), "nenhum código");
        assert_eq!(c_g.retrato().aviso, None);
    }

    /// 2. Só o anfitrião com o vínculo (o convidado esqueceu; o item da lista é anônimo): o código, e o
    /// anfitrião substitui depois da escolha certa.
    #[test]
    fn o_toque_so_com_o_vinculo_do_anfitriao_pareia_de_novo_pelo_codigo() {
        let parar = Cancelamento::novo();
        let (s, end) = servidor();
        let (c_h, c_g) = (ConfirmacaoPorCodigo::nova(), ConfirmacaoPorCodigo::nova());
        let host = anfitriao_em(s, config("tq2-h", None, Some("828282"), vinculo("tq2-g", [0x32; 32]), &parar), Some(c_h.clone()));
        let p = pessoa(c_h.clone(), c_g.clone(), false);
        let g = conectar_pelo_toque(end, config("tq2-g", None, None, PairedPeers::new(), &parar), Some(c_g.clone()), None,
            |_, _, _| panic!("não procura"))
            .unwrap_or_else(|e| panic!("pareia pelo código: {e}"));
        p.join().expect("pessoa");
        let h = host.join().expect("thread").expect("h");
        assert!(g.outcome.novo && h.outcome.novo);
        assert_eq!(g.outcome.secret, h.outcome.secret);
        assert_ne!(h.outcome.secret, [0x32; 32]);
        assert_eq!(c_h.retrato().fase, Fase::Conferiu);
    }

    /// 3. Só o convidado com o vínculo (o anfitrião esqueceu; o toque tem o alvo): a retomada não é
    /// reconhecida, o cartão avisa, e o código pareia de novo no mesmo toque.
    #[test]
    fn o_toque_so_com_o_vinculo_do_convidado_avisa_e_pareia_de_novo() {
        let parar = Cancelamento::novo();
        let (s, end) = servidor();
        let (c_h, c_g) = (ConfirmacaoPorCodigo::nova(), ConfirmacaoPorCodigo::nova());
        let host = anfitriao_em(s, config("tq3-h", None, Some("838383"), PairedPeers::new(), &parar), Some(c_h.clone()));
        let p = pessoa(c_h.clone(), c_g.clone(), false);
        let g = conectar_pelo_toque(end, config("tq3-g", None, None, vinculo("tq3-h", [0x33; 32]), &parar), Some(c_g.clone()),
            Some(&DeviceId("tq3-h".into())), |_, _, _| panic!("não é rede: não procura"))
            .unwrap_or_else(|e| panic!("pareia de novo: {e}"));
        p.join().expect("pessoa");
        let h = host.join().expect("thread").expect("h");
        assert_eq!(c_g.retrato().aviso, Some(AVISO_PAREAMENTO_ANTERIOR), "o cartão diz por quê");
        assert!(g.outcome.novo && h.outcome.novo);
        assert_eq!(g.outcome.secret, h.outcome.secret);
        assert_ne!(g.outcome.secret, [0x33; 32], "o convidado grava por cima");
        assert_eq!(g.peer.device_id.0, "tq3-h");
    }

    /// 4. A queda de rede na retomada **não** vira pareamento novo: a procura tenta de novo até o
    /// prazo, nenhum código começa, e o erro de rede volta.
    #[test]
    fn o_toque_com_queda_de_rede_nao_pareia_de_novo() {
        let parar = Cancelamento::novo();
        let (end, fim_do_servidor) = derruba_tudo();
        let c_g = ConfirmacaoPorCodigo::nova();
        let mut cfg = config("tq4-g", None, None, vinculo("tq4-h", [0x34; 32]), &parar);
        cfg.timeout = Duration::from_secs(3);
        let procurou = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let p = Arc::clone(&procurou);
        let r = conectar_pelo_toque(end, cfg, Some(c_g.clone()), Some(&DeviceId("tq4-h".into())), move |alvo, destino, cfg| {
            p.store(true, std::sync::atomic::Ordering::Relaxed);
            conectar_ao_par_entre(alvo, Some(destino), cfg, Vec::new)
        });
        fim_do_servidor.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(procurou.load(std::sync::atomic::Ordering::Relaxed), "tentou de novo pela procura");
        assert!(
            !matches!(&r, Ok(_) | Err(Error::ParNaoReconhecido(_)) | Err(Error::NeedsPin(_))),
            "a queda é rede, não 'pareie de novo': {:?}",
            r.err()
        );
        let retrato = c_g.retrato();
        assert_eq!(retrato.tentativas, 0, "nenhum código começou");
        assert_eq!(retrato.aviso, None);
    }

    /// **Baixo 3**: a recusa dada no convidado vale antes do próximo quadro — mesmo que o
    /// anfitrião confirme logo em seguida, o par não fecha.
    #[test]
    fn a_recusa_do_convidado_vale_antes_da_decisao_do_anfitriao() {
        let parar = Cancelamento::novo();
        let (s, end) = servidor();
        let (c_h, c_g) = (ConfirmacaoPorCodigo::nova(), ConfirmacaoPorCodigo::nova());
        let host = anfitriao_em(s, config("b3-h", None, None, PairedPeers::new(), &parar), Some(c_h.clone()));
        let p = pessoa(c_h.clone(), c_g.clone(), true);
        let r = conectar_com_codigo(end, config("b3-g", None, None, PairedPeers::new(), &parar), Some(c_g.clone()));
        p.join().expect("pessoa");
        assert!(matches!(&r, Err(Error::Pairing(m)) if m.contains("recusou")), "{:?}", r.err());
        assert_eq!(c_g.retrato().fase, Fase::RecusadoAqui);
        thread::sleep(Duration::from_millis(300));
        assert!(!host.is_finished(), "o anfitrião não fechou par nenhum");
        parar.cancelar();
        let _ = host.join();
    }

    /// **Baixo 4**: cancelar no meio do código não conta como perna caída do A1.
    #[test]
    fn cancelar_nao_conta_como_perna_caida() {
        let (s, end) = servidor();
        let segura = thread::spawn(move || {
            let Ok(Some(mut link)) = s.accept(Duration::from_secs(5)) else { return };
            let mut h = Pairing::new_with_store(Role::Host, DeviceId("b4-rele".into()), None, &PairedPeers::new()).expect("h");
            h.bind_local_role(0).expect("papel");
            h.com_codigo(true, "Relé").expect("código");
            let fim = Instant::now() + Duration::from_secs(5);
            while Instant::now() < fim {
                match link.poll() {
                    Ok(Some(SignalMessage::Pair(PairFrame::CodigoNonce { .. }))) => {
                        thread::sleep(Duration::from_secs(3)); // segura a perna, sem revelar
                        return;
                    }
                    Ok(Some(SignalMessage::Pair(q))) => {
                        if let Some(r) = h.step(q).expect("passo").reply {
                            link.send(&SignalMessage::Pair(r)).expect("envio");
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
        let parar = Cancelamento::novo();
        let p = parar.clone();
        let cancela = thread::spawn(move || {
            thread::sleep(Duration::from_millis(1200));
            p.cancelar();
        });
        let r = conectar_com_codigo(end, config("b4-g", None, None, PairedPeers::new(), &parar), Some(ConfirmacaoPorCodigo::nova()));
        assert!(matches!(r, Err(Error::Cancelled)), "{:?}", r.err());
        assert_eq!(pernas_caidas_de(end), 0);
        cancela.join().expect("cancela");
        let _ = segura.join();
    }
}

#[cfg(test)]
mod testes_do_qr {
    //! A sequência completa do QR invertido sobre o v3, em loopback (`docs/v3-no-privado.md` §4): o
    //! celular hospeda com o PIN na tela, lê o QR e chama (selado); o desktop conecta como convidado
    //! e pareia pelo OPAQUE com `K_par`.
    use super::*;
    use crate::pairing::PairFrame;
    use crate::protocol::{Capabilities, PROTOCOL_VERSION};
    use crate::qr::{chamar, Chamado, ConteudoDoQr, Convite, ResultadoDoChamado};
    use std::thread;

    fn anuncio(id: &str) -> Announcement {
        Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.into()),
            display_name: format!("Aparelho {id}"),
            capabilities: Capabilities { screen_source: true, camera_source: true, sink: true },
            screen: None,
            papel: None,
        }
    }

    fn config(id: &str, pin: Option<&str>, prazo_s: u64) -> SessionConfig {
        SessionConfig {
            announcement: anuncio(id),
            pin: pin.map(|p| Pin::parse(p).expect("pin")),
            known: PairedPeers::new(),
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(prazo_s),
            cancelamento: Cancelamento::novo(),
            silencio_do_caminho: None,
        }
    }

    /// O celular hospedando com o PIN na tela, numa thread. Devolve a porta e a thread.
    fn celular_esperando(id: &str) -> (u16, thread::JoinHandle<Result<Ready>>) {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let cfg = config(id, Some("424242"), 30);
        (porta, thread::spawn(move || hospedar(&servidor, cfg)))
    }

    /// O desktop mostra o QR e o celular o lê e chama. Devolve o convite (já chamado) e o chamado.
    fn ler_e_chamar(desktop: &str, celular: &str, porta: u16) -> (Convite, Chamado) {
        let convite = Convite::novo(&DeviceId(desktop.into()), "MacBook", Duration::from_secs(60))
            .expect("convite");
        let lido = ConteudoDoQr::ler(&convite.texto()).expect("o celular lê o QR");
        let r = chamar(&lido, &DeviceId(celular.into()), "A07", porta, Duration::from_secs(5))
            .expect("chama");
        assert!(matches!(r, ResultadoDoChamado::Aceito { .. }), "{r:?}");
        let chamado = convite.esperar_chamado(&Cancelamento::novo()).expect("o desktop ouve");
        (convite, chamado)
    }

    #[test]
    fn a_sequencia_completa_sobe_a_sessao_sem_pin_e_a_proxima_e_retomada() {
        let (porta, celular) = celular_esperando("qr-a07");
        let (convite, chamado) = ler_e_chamar("qr-mac", "qr-a07", porta);
        assert_eq!(chamado.endpoint.port(), porta);
        assert_eq!(chamado.par, DeviceId("qr-a07".into()));

        let desktop = conectar_pelo_qr(&chamado, convite.chave_do_par(), config("qr-mac", None, 30))
            .expect("o desktop conecta pelo QR, sem PIN");
        let celular = celular.join().expect("thread").expect("o celular sobe");
        assert_eq!(desktop.outcome.secret, celular.outcome.secret);
        assert!(desktop.outcome.novo && celular.outcome.novo, "é pareamento novo: as cascas guardam");
        assert_eq!(desktop.peer.device_id, DeviceId("qr-a07".into()));
        assert_eq!(celular.peer.device_id, DeviceId("qr-mac".into()));
        assert_eq!(crate::qr::tomar(&DeviceId("qr-mac".into())), None, "a chave lida sai da caixa");
        assert_eq!(crate::qr::desfecho(&DeviceId("qr-mac".into())), Some("connected"), "o celular sabe");

        celular.session.send(b"pelo qr").expect("envia");
        assert_eq!(desktop.session.next_data(Duration::from_secs(5)).expect("chegou"), b"pelo qr");

        // A próxima vez é retomada, sem QR e sem PIN.
        let mut lado_d = PairedPeers::new();
        lado_d.insert(&desktop.outcome);
        let mut lado_c = PairedPeers::new();
        lado_c.insert(&celular.outcome);
        drop((desktop, celular, convite));
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let mut cfg_c = config("qr-a07", None, 30);
        cfg_c.known = lado_c;
        let celular = thread::spawn(move || hospedar(&servidor, cfg_c));
        let mut cfg_d = config("qr-mac", None, 30);
        cfg_d.known = lado_d;
        let desktop = conectar(format!("127.0.0.1:{porta}").parse().unwrap(), cfg_d).expect("retoma");
        let celular = celular.join().expect("thread").expect("o celular retoma");
        assert!(!desktop.outcome.novo && !celular.outcome.novo);
    }

    /// Quem viu o chamado (ou chuta) e conecta antes no celular sem `K_par` não pareia, e a espera
    /// continua para o desktop de verdade, com o mesmo PIN.
    #[test]
    fn um_hello_qr_sem_a_chave_nao_derruba_a_espera_do_celular() {
        let (porta, celular) = celular_esperando("qr-a07-corrida");
        let (convite, chamado) = ler_e_chamar("qr-mac-corrida", "qr-a07-corrida", porta);

        let mut atacante = config("qr-mac-corrida", None, 10);
        atacante.announcement.display_name = "Intruso".into();
        let e = conectar_pelo_qr(&chamado, [0u8; 32], atacante).err().expect("o intruso não pareia");
        assert!(matches!(e, Error::Pairing(_) | Error::Signaling(_) | Error::Closed | Error::NeedsPin(_)), "{e:?}");

        let desktop = conectar_pelo_qr(&chamado, convite.chave_do_par(), config("qr-mac-corrida", None, 30))
            .expect("o desktop de verdade ainda entra");
        let celular = celular.join().expect("thread").expect("o celular sobe");
        assert_eq!(desktop.outcome.secret, celular.outcome.secret);
        assert_eq!(celular.descartados, 1, "o intruso conta como candidato que caiu");
    }

    #[test]
    fn sem_qr_lido_o_celular_recusa_e_o_pin_continua_valendo() {
        let (porta, celular) = celular_esperando("qr-a07-semqr");
        let chamado = Chamado {
            endpoint: format!("127.0.0.1:{porta}").parse().unwrap(),
            par: DeviceId("qr-a07-semqr".into()),
            nome: String::new(),
        };
        assert!(conectar_pelo_qr(&chamado, [1u8; 32], config("qr-mac-semqr", None, 10)).is_err());
        // O plugin de OBS publicado (só PIN) entra como sempre na mesma espera.
        let obs = conectar(chamado.endpoint, config("obs-publicado", Some("424242"), 30)).expect("PIN");
        let celular = celular.join().expect("thread").expect("o celular sobe");
        assert_eq!(celular.peer.device_id.0, "obs-publicado");
        drop(obs);
    }

    /// O desktop confere que quem atendeu é o celular do chamado: outro id autenticado é recusado.
    #[test]
    fn quem_atende_tem_de_ser_o_celular_do_chamado() {
        let (porta, celular) = celular_esperando("qr-a07-certo");
        let (convite, mut chamado) = ler_e_chamar("qr-mac-certo", "qr-a07-certo", porta);
        chamado.par = DeviceId("outro-celular".into());
        let e = conectar_pelo_qr(&chamado, convite.chave_do_par(), config("qr-mac-certo", None, 10)).err().expect("recusado");
        assert!(matches!(e, Error::Pairing(_)), "{e:?}");
        drop(celular);
    }

    /// **Sentinela** e **B2**: o `Hello {qr}` não leva id nem nome, só a dica; o modo é o do QR.
    #[test]
    fn o_hello_do_qr_leva_so_a_dica() {
        let mut g = Pairing::new(Role::Guest, DeviceId("ID-DESKTOP-SENTINELA".into()), None, None).unwrap();
        g.bind_local_role(0).unwrap();
        g.com_qr(DeviceId("ID-CELULAR-SENTINELA".into()), [3u8; 32]).unwrap();
        let mut h = Pairing::new(Role::Host, DeviceId("ID-CELULAR-SENTINELA".into()), None, None).unwrap();
        h.bind_local_role(0).unwrap();
        h.com_convites(vec![(DeviceId("ID-DESKTOP-SENTINELA".into()), [3u8; 32])]).unwrap();
        let probe = g.open().unwrap();
        let challenge = h.step(probe).unwrap().reply.unwrap();
        let hello = g.step(challenge).unwrap().reply.unwrap();
        let json = serde_json::to_string(&hello).unwrap();
        assert!(matches!(hello, PairFrame::Hello { mode: crate::pairing::PairMode::Qr, hint: Some(_), .. }));
        assert!(!json.contains("SENTINELA"), "{json}");
        let ack = h.step(hello).unwrap().reply.unwrap();
        assert_eq!(h.convite_do_qr().map(|d| d.0.as_str()), Some("ID-DESKTOP-SENTINELA"));
        assert!(!h.attempt_started(), "o QR não gasta o PIN");
        let confirm = g.step(ack).unwrap().reply.unwrap();
        let fim = h.step(confirm).unwrap();
        let rh = fim.done.unwrap();
        let rg = g.step(fim.reply.unwrap()).unwrap().done.unwrap();
        assert_eq!(rg.secret, rh.secret);
        assert!(rg.novo && rh.novo);
    }

    /// Um `Hello {qr}` com a dica de outro convite é recusado antes do KE1 (M4: não derruba nada).
    #[test]
    fn a_dica_de_outro_convite_e_recusada_antes_do_ke1() {
        let mut g = Pairing::new(Role::Guest, DeviceId("desk".into()), None, None).unwrap();
        g.bind_local_role(0).unwrap();
        g.com_qr(DeviceId("cel".into()), [4u8; 32]).unwrap();
        let mut h = Pairing::new(Role::Host, DeviceId("cel".into()), None, None).unwrap();
        h.bind_local_role(0).unwrap();
        h.com_convites(vec![(DeviceId("desk".into()), [5u8; 32])]).unwrap();
        let challenge = h.step(g.open().unwrap()).unwrap().reply.unwrap();
        let hello = g.step(challenge).unwrap().reply.unwrap();
        assert!(matches!(h.step(hello), Err(Error::NeedsPin(_))));
        assert!(h.convite_do_qr().is_none() && !h.attempt_started());
    }
}

#[cfg(test)]
mod testes_da_revisao_do_qr {
    use super::*;
    use crate::pairing::PairOutcome;
    use crate::protocol::{Capabilities, PROTOCOL_VERSION};
    use crate::qr::{chamar, ConteudoDoQr, Convite, ResultadoDoChamado};
    use std::thread;

    fn config(id: &str, pin: Option<&str>, known: PairedPeers) -> SessionConfig {
        SessionConfig {
            announcement: Announcement {
                protocol_version: PROTOCOL_VERSION,
                device_id: DeviceId(id.into()),
                display_name: id.into(),
                capabilities: Capabilities { screen_source: true, camera_source: true, sink: true },
                screen: None,
                papel: None,
            },
            pin: pin.map(|p| Pin::parse(p).expect("pin")),
            known,
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(15),
            cancelamento: Cancelamento::novo(),
            silencio_do_caminho: None,
        }
    }

    fn vinculo(de: &str) -> PairedPeers {
        let mut t = PairedPeers::new();
        t.insert(&PairOutcome { peer: DeviceId(de.into()), secret: [0x44; 32], novo: true });
        t
    }

    /// Um QR novo do mesmo Mac começa outra tentativa: o desfecho velho ("connected" de uma sessão
    /// que caiu) some no chamado, e a consulta volta a "pendente" até a tentativa nova terminar.
    #[test]
    fn o_chamado_novo_apaga_o_desfecho_velho() {
        let desktop = DeviceId("qrdes-mac".into());
        crate::qr::registrar_desfecho(&desktop, "connected");
        assert_eq!(crate::qr::desfecho(&desktop), Some("connected"));
        let convite = Convite::novo(&desktop, "Mac", Duration::from_secs(60)).expect("convite");
        let lido = ConteudoDoQr::ler(&convite.texto()).expect("lê");
        let r = chamar(&lido, &DeviceId("qrdes-a07".into()), "A07", 7877, Duration::from_secs(5)).expect("chama");
        assert!(matches!(r, ResultadoDoChamado::Aceito { .. }));
        assert_eq!(crate::qr::desfecho(&desktop), None, "o desfecho velho não responde pela tentativa nova");
        crate::qr::registrar_desfecho(&desktop, "failed");
        let convite = Convite::novo(&desktop, "Mac", Duration::from_secs(60)).expect("convite");
        let lido = ConteudoDoQr::ler(&convite.texto()).expect("lê");
        let _ = chamar(&lido, &DeviceId("qrdes-a07".into()), "A07", 7877, Duration::from_secs(5));
        assert_eq!(crate::qr::desfecho(&desktop), None, "um failed velho também some");
    }

    /// **O QR com vínculo** (bancada de 13/10): com o vínculo bom dos dois lados, ler o QR
    /// **reconecta** (retomada, o segredo fica); com o vínculo de um lado só, ou com segredos que não
    /// casam, o QR **substitui** — o vínculo que não retomou não abre nada do outro lado. Nos dois, o
    /// celular que leu fica sabendo ("connected") e a chave lida sai da caixa.
    #[test]
    fn o_qr_reconecta_com_o_vinculo_e_substitui_o_que_nao_retoma() {
        let segredo = |b: u8| [b; 32];
        let com = |de: &str, s: [u8; 32]| {
            let mut t = PairedPeers::new();
            t.insert(&PairOutcome { peer: DeviceId(de.into()), secret: s, novo: true });
            t
        };
        // (sufixo, segredo no desktop, segredo no celular, retoma?)
        for (sufixo, no_d, no_c, retoma) in [
            ("d", Some(segredo(0x44)), None, false),
            ("c", None, Some(segredo(0x44)), false),
            ("dc", Some(segredo(0x44)), Some(segredo(0x44)), true),
            ("xx", Some(segredo(0x44)), Some(segredo(0x55)), false),
        ] {
            let (id_d, id_c) = (format!("qrv-mac-{sufixo}"), format!("qrv-a07-{sufixo}"));
            let servidor = SignalingServer::bind(0).expect("bind");
            let porta = servidor.port().expect("porta");
            let known_c = no_c.map_or_else(PairedPeers::new, |s| com(&id_d, s));
            let cfg_c = config(&id_c, Some("424242"), known_c);
            let celular = thread::spawn(move || hospedar(&servidor, cfg_c));
            let convite = Convite::novo(&DeviceId(id_d.clone()), "Mac", Duration::from_secs(60)).expect("convite");
            let lido = ConteudoDoQr::ler(&convite.texto()).expect("lê");
            let r = chamar(&lido, &DeviceId(id_c.clone()), "A07", porta, Duration::from_secs(5)).expect("chama");
            assert!(matches!(r, ResultadoDoChamado::Aceito { .. }));
            let chamado = convite.esperar_chamado(&Cancelamento::novo()).expect("chamado");
            let known_d = no_d.map_or_else(PairedPeers::new, |s| com(&id_c, s));
            let desktop = conectar_pelo_qr(&chamado, convite.chave_do_par(), config(&id_d, None, known_d))
                .unwrap_or_else(|e| panic!("o QR tinha de conectar ({sufixo}): {e}"));
            let celular = celular.join().expect("thread").expect("o celular sobe");
            assert_eq!(desktop.peer.device_id.0, id_c, "{sufixo}");
            assert_eq!(celular.peer.device_id.0, id_d, "{sufixo}");
            assert_eq!(desktop.outcome.secret, celular.outcome.secret, "{sufixo}");
            if retoma {
                assert!(!desktop.outcome.novo && !celular.outcome.novo, "retomada, sem substituir ({sufixo})");
                assert_eq!(desktop.outcome.secret, segredo(0x44), "o vínculo ficou o mesmo");
            } else {
                assert!(desktop.outcome.novo && celular.outcome.novo, "pareamento novo: as cascas gravam por cima ({sufixo})");
            }
            assert_eq!(crate::qr::desfecho(&DeviceId(id_d.clone())), Some("connected"), "{sufixo}");
            assert_eq!(crate::qr::tomar(&DeviceId(id_d.clone())), None, "a chave lida saiu da caixa ({sufixo})");
        }
    }

    /// Com o vínculo dos dois lados, a retomada pelo QR é **só com o par do QR**: um outro conhecido
    /// do desktop no mesmo endereço não é retomado (M5).
    #[test]
    fn a_retomada_pelo_qr_e_so_com_o_par_do_qr() {
        let mut g = Pairing::new_with_store(Role::Guest, DeviceId("qrm5-mac".into()), None, &{
            let mut t = PairedPeers::new();
            t.insert(&PairOutcome { peer: DeviceId("qrm5-outro".into()), secret: [7; 32], novo: true });
            t
        })
        .unwrap();
        g.bind_local_role(0).unwrap();
        g.com_qr(DeviceId("qrm5-a07".into()), [4u8; 32]).unwrap();
        let mut h = Pairing::new_with_store(Role::Host, DeviceId("qrm5-outro".into()), None, &{
            let mut t = PairedPeers::new();
            t.insert(&PairOutcome { peer: DeviceId("qrm5-mac".into()), secret: [7; 32], novo: true });
            t
        })
        .unwrap();
        h.bind_local_role(0).unwrap();
        let challenge = h.step(g.open().unwrap()).unwrap().reply.unwrap();
        let hello = g.step(challenge).unwrap().reply.unwrap();
        assert!(
            matches!(hello, crate::pairing::PairFrame::Hello { mode: crate::pairing::PairMode::Qr, .. }),
            "a dica de outro conhecido não vira retomada pelo QR"
        );
    }
}

/// **Quem da lista é um par conhecido** (`crate::reconhecer`, `docs/v3-no-privado.md` §5.4), contra
/// uma espera de verdade em loopback.
#[cfg(test)]
mod testes_do_reconhecer {
    use super::*;
    use crate::busca::AparelhoVisto;
    use crate::protocol::{Capabilities, DeviceId, PROTOCOL_VERSION};
    use crate::reconhecer::{quem_e, Reconhecedor};
    use std::thread;

    fn config(id: &str, pin: Option<&str>, known: PairedPeers) -> SessionConfig {
        SessionConfig {
            announcement: Announcement {
                protocol_version: PROTOCOL_VERSION,
                device_id: DeviceId(id.into()),
                display_name: id.into(),
                capabilities: Capabilities { screen_source: true, camera_source: true, sink: true },
                screen: None,
                papel: None,
            },
            pin: pin.map(|p| Pin::parse(p).expect("pin")),
            known,
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(20),
            cancelamento: Cancelamento::novo(),
            silencio_do_caminho: None,
        }
    }

    fn vinculo(de: &str, segredo: [u8; 32]) -> PairedPeers {
        let mut t = PairedPeers::new();
        t.insert(&PairOutcome { peer: DeviceId(de.into()), secret: segredo, novo: true });
        t
    }

    fn item(endpoint: std::net::SocketAddr) -> AparelhoVisto {
        AparelhoVisto {
            anuncio: Announcement {
                protocol_version: PROTOCOL_VERSION,
                device_id: DeviceId(format!("discovery-{}", "ab".repeat(16))),
                display_name: "Quall abababab".into(),
                capabilities: Capabilities { screen_source: true, camera_source: true, sink: true },
                screen: None,
                papel: None,
            },
            endpoint: Some(endpoint),
            via: vec!["udp"],
            visto_ha: Duration::ZERO,
            vivo: true,
            aceita_codigo: true,
            proprio: false,
            conhecido: None,
            chave: format!("{}|{endpoint}", "ab".repeat(16)),
            nome_anunciado: None,
        }
    }

    /// A sonda diz quem é o par guardado, um estranho não é ninguém, e a espera do anfitrião não
    /// gasta nada com isso: o receptor de verdade entra depois, pelo PIN.
    #[test]
    fn a_sonda_reconhece_o_par_e_nao_gasta_a_espera() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let destino: std::net::SocketAddr = format!("127.0.0.1:{porta}").parse().unwrap();
        let cfg = config("rec-h", Some("737373"), vinculo("rec-g", [0x31; 32]));
        let host = thread::spawn(move || hospedar(&servidor, cfg));

        let meus = vinculo("rec-h", [0x31; 32]);
        let c = Cancelamento::novo();
        let achado = quem_e(destino, DeviceId("rec-g".into()), 0, &meus, Duration::from_secs(3), &c)
            .expect("a sonda lê o desafio");
        assert_eq!(achado, Some(DeviceId("rec-h".into())));
        // Um par guardado com outro segredo (outro aparelho com o mesmo nome) não casa.
        let outro = vinculo("rec-h", [0x32; 32]);
        let achado = quem_e(destino, DeviceId("rec-x".into()), 0, &outro, Duration::from_secs(3), &c)
            .expect("a sonda lê o desafio");
        assert_eq!(achado, None);

        // O rótulo da lista: a primeira chamada põe para sondar, uma seguinte traz o id, e o item
        // não é sondado de novo (a espera conta só três candidatos que caíram).
        let r = Reconhecedor::novo(DeviceId("rec-g".into()), None, meus).expect("reconhecedor");
        let fim = Instant::now() + Duration::from_secs(5);
        let mut rotulado = None;
        while Instant::now() < fim && rotulado.is_none() {
            let mut lista = [item(destino)];
            r.rotular(&mut lista);
            rotulado = lista[0].conhecido.clone();
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(rotulado, Some(DeviceId("rec-h".into())));
        for _ in 0..5 {
            let mut lista = [item(destino)];
            r.rotular(&mut lista);
            assert_eq!(lista[0].conhecido, Some(DeviceId("rec-h".into())));
        }
        // O próprio anúncio e um item sem endereço não são sondados.
        let mut proprio = item(destino);
        proprio.proprio = true;
        proprio.chave = "proprio".into();
        let mut sem_endereco = item(destino);
        sem_endereco.endpoint = None;
        sem_endereco.chave = "sem".into();
        let mut lista = [proprio, sem_endereco];
        r.rotular(&mut lista);
        thread::sleep(Duration::from_millis(300));
        r.rotular(&mut lista);
        assert!(lista.iter().all(|i| i.conhecido.is_none()));

        let g = conectar(destino, config("rec-pin", Some("737373"), PairedPeers::new()))
            .expect("a espera continua de pé para o PIN");
        let pronto = host.join().expect("thread").expect("o anfitrião sobe");
        assert_eq!(pronto.peer.device_id.0, "rec-pin");
        assert_eq!(pronto.descartados, 3, "as três sondas são candidatos que caíram, e só");
        drop(g);
    }

    /// Sem par guardado, nada é sondado.
    #[test]
    fn sem_pares_nao_sonda() {
        let r = Reconhecedor::novo(DeviceId("rec-vazio".into()), None, PairedPeers::new()).expect("r");
        // Um endereço que ninguém escuta: se sondasse, só gastaria tempo; o rótulo fica vazio.
        let mut lista = [item("127.0.0.1:9".parse().unwrap())];
        r.rotular(&mut lista);
        assert!(lista[0].conhecido.is_none());
    }
}
