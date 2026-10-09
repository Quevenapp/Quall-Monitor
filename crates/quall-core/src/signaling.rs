//! Sinalização: mini servidor HTTP/WebSocket local, hospedado pelo emissor.
//!
//! Zero servidor online — sem STUN, sem TURN, sem intermediário. A troca de SDP e candidatos ICE
//! acontece direto entre os dois aparelhos da LAN, por um WebSocket que o **emissor** hospeda e
//! o receptor procura, seja pelo endereço que o mDNS devolveu, seja pelo IP que o usuário
//! digitou.
//!
//! # Por que síncrono, uma thread por conexão
//!
//! Um runtime async inteiro entraria no núcleo por causa de uma conexão que dura segundos e
//! carrega meia dúzia de mensagens. Na Broadcast Upload Extension do iOS, onde o orçamento é de
//! ~50 MB para captura, encode e envio, isso é caro pelo motivo errado. `TcpStream` com
//! `read_timeout` e `tungstenite` síncrono resolvem o mesmo problema com uma peça a menos.
//!
//! # Memória
//!
//! Os buffers do WebSocket ficam em 8 KiB, contra os 128 KiB de leitura + 128 KiB de escrita que
//! o `tungstenite` usa por padrão. São 256 KiB por conexão que a extension não precisa gastar
//! para trocar um SDP de poucos quilobytes. O limite de mensagem em 256 KiB também é
//! deliberado: SDP com muitos candidatos passa de 10 KiB, mas nunca de 256 — e sem limite uma
//! ponta hostil na LAN derruba a extension só mandando um quadro grande.

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::handshake::HandshakeError;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

use crate::cancel::Cancelamento;
use crate::error::{Error, Result};
use crate::pairing::PairFrame;
use crate::protocol::{Announcement, PROTOCOL_VERSION};

/// Caminho do WebSocket. Versionado no caminho para que uma versão futura possa conviver com
/// esta na mesma porta em vez de quebrar o aparelho antigo.
pub const SIGNALING_PATH: &str = "/quall/v1";

/// Teto de uma mensagem de sinalização.
const MAX_MENSAGEM: usize = 256 * 1024;
/// Buffers de leitura e escrita por conexão.
const BUFFER: usize = 8 * 1024;

/// Quanto tempo o `poll` fica parado no `read` antes de devolver o controle.
///
/// Curto de propósito: é também a granularidade com que as mensagens da fila de saída (SDP e
/// candidatos vindos das threads do libdatachannel) saem pela rede. Um valor grande atrasaria o
/// ICE pelo tempo todo do intervalo.
const FATIA_DE_LEITURA: Duration = Duration::from_millis(20);

/// Quanto tempo o [`Link::close`] aceita ficar parado no `write` do `Bye`.
///
/// **Dívida 19.** Sem isto, `close` com o par sumido pendura até o TCP do Darwin desistir — na
/// casa dos minutos —, e é só por isso que o `broadcastFinished` da extension do iOS não podia
/// chamar `quall_session_close`: o sistema mata a extension muito antes. O `Bye` é uma
/// gentileza, não um requisito: o transporte é P2P e a outra ponta descobre a queda de qualquer
/// jeito. Então o prazo é curto de propósito — cabe numa LAN e não segura o encerramento.
const PRAZO_DE_DESPEDIDA: Duration = Duration::from_millis(200);

/// De quanto em quanto tempo o `connect` não-bloqueante pergunta se cancelaram.
///
/// Dez milissegundos ficam entre as duas fatias que o núcleo já usa (`PASSO_DE_HANDSHAKE`, 5 ms, e
/// `FATIA_DE_LEITURA`, 20 ms). É o atraso máximo entre tocar em Cancelar e a chamada voltar, e é
/// invisível ao lado dos 20 s que ele substitui.
const FATIA_DE_ESPERA: Duration = Duration::from_millis(10);

/// Passo do laço que empurra um handshake WebSocket em socket não bloqueante.
///
/// Curto porque o handshake é uma troca só; é granularidade de espera, não de rede.
const PASSO_DE_HANDSHAKE: Duration = Duration::from_millis(5);

/// Quanto tempo **uma** conexão tem para completar o handshake antes de ser descartada.
///
/// Precisa ser bem menor que o prazo da sessão, e o motivo apareceu num teste: com o prazo
/// inteiro, uma única conexão muda ainda negava o serviço — o `accept` ficava esperando **ela**
/// falar enquanto o receptor de verdade esperava na fila do kernel e desistia. Dois segundos é
/// folga enorme para uma troca de duas mensagens na LAN e curto o bastante para o receptor
/// seguinte entrar dentro do mesmo prazo.
const PRAZO_DE_HANDSHAKE: Duration = Duration::from_secs(2);

/// Uma mensagem do canal de sinalização.
///
/// O SDP viaja como texto, não como estrutura: o núcleo não precisa entender o SDP para
/// repassá-lo, e reserializar SDP é uma chance a mais de perder um atributo que a outra
/// implementação queria.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum SignalMessage {
    /// Primeira mensagem do convidado: quem sou e o que sei fazer.
    Hello { announcement: Announcement },
    /// Resposta do anfitrião, com o anúncio dele.
    Welcome { announcement: Announcement },
    /// Quadro do pareamento, encapsulado.
    Pair(PairFrame),
    /// Oferta SDP. `kind` é `offer` ou `answer`, como o WebRTC define.
    Description { kind: String, sdp: String },
    /// Candidato ICE, trickle.
    Candidate { candidate: String, mid: String },
    /// Não haverá mais candidatos deste lado.
    CandidatesDone,
    /// Fim de papo, com motivo legível.
    Bye { motivo: String },
    /// Recusa. Chega antes do `Bye` quando dá para explicar.
    ///
    /// **Dívida 29.** `motivo` é prosa para humano, e por anos foi a **única** coisa que
    /// atravessava: `e.to_string()` de um lado, `Error::Pairing(motivo)` do outro. O tipo do erro
    /// morria exatamente aqui, no fio — e por isso o anfitrião que dizia, corretamente, "não
    /// conheço este aparelho" chegava ao emissor como recusa genérica, que as cascas rotulavam de
    /// "O PIN não conferiu". A frente do Windows viu esse texto com o PIN certo, e **recusou
    /// adivinhar comparando string de mensagem de erro** — o que teria sido pior.
    ///
    /// `causa` é o código que faltava. `motivo` continua sendo o que se mostra; `causa` é o que se
    /// ramifica. Nunca ramifique por `motivo`.
    Error {
        motivo: String,
        #[serde(default, deserialize_with = "causa_do_fio")]
        causa: CausaDeRecusa,
    },
    /// O que o **receptor** está vendo do enlace, numa janela. Ver [`RelatoDoEnlace`].
    ///
    /// É a única mensagem deste protocolo que viaja **depois** da negociação, e a primeira coisa
    /// a dizer sobre ela é que um par antigo a ignora sozinho: `olhar_sinalizacao` já descartava
    /// toda mensagem pós-negociação com *"ignorar é melhor que derrubar uma sessão que está
    /// funcionando"*. Um receptor novo falando com um emissor velho manda relato para o vazio, e
    /// o emissor velho segue com o bitrate fixo — que é exatamente o comportamento de hoje.
    Enlace(RelatoDoEnlace),
}

// SDP contém credenciais ICE; candidatos e anúncios contêm dados do aparelho. A representação
// de diagnóstico informa o tipo e contagens/códigos, sem reproduzir o payload recebido.
impl core::fmt::Debug for SignalMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Hello { .. } => f.write_str("Hello"),
            Self::Welcome { .. } => f.write_str("Welcome"),
            Self::Pair(quadro) => f.debug_tuple("Pair").field(quadro).finish(),
            Self::Description { sdp, .. } => f
                .debug_struct("Description")
                .field("bytes", &sdp.len())
                .finish(),
            Self::Candidate { .. } => f.write_str("Candidate"),
            Self::CandidatesDone => f.write_str("CandidatesDone"),
            Self::Bye { .. } => f.write_str("Bye"),
            Self::Error { causa, .. } => f.debug_struct("Error").field("causa", causa).finish(),
            Self::Enlace(relato) => f.debug_tuple("Enlace").field(relato).finish(),
        }
    }
}

/// O dano do enlace numa janela, do receptor para o emissor.
///
/// # Por que a sinalização, e não RTCP
///
/// A libdatachannel já carrega RTCP, e este projeto já o usa nos dois sentidos: o receptor emite
/// PLI por `rtcChainRtcpReceivingSession` e o emissor o recebe por `rtcChainPliHandler`. Seria
/// natural pôr o relato ali. Três coisas decidiram contra, e a terceira sozinha bastaria:
///
/// 1. **O PLI não tem carga.** Levar um número por RTCP significa RR, REMB ou TMMBR, e
///    `quall-rtc` **não expõe nenhum dos três** — nem a libdatachannel os entrega pela API C que
///    aquele crate embrulha. Seria trabalho no único crate `unsafe` do projeto, que é o mais caro
///    de mexer e o único cuja quebra derruba mídia.
/// 2. **O RR mede outra coisa.** `fraction_lost` do RFC 3550 é perda de pacote vista pela pilha
///    RTCP, com período próprio (tipicamente segundos). O que decide aqui é a perda **exata** do
///    nosso depacotizador — `packets_lost_for_real`, que já distingue perda de reordenação e que
///    esta bancada mediu contra o teto (486 contra 50 na mesma corrida). Trocar o número que a
///    bancada mediu por outro parecido é projetar o controlador contra outra curva.
/// 3. **A sinalização já está aberta e já é lida.** `Ready::link` sobrevive à sessão inteira nos
///    dois lados; o receptor Android já a consulta a cada volta do laço. O caminho custou **uma
///    variante de enum** e um método de cada lado.
///
/// # O que a escolha custa, dito com o mesmo destaque
///
/// - **TCP.** A sinalização é WebSocket sobre TCP. Um relato atrasado por retransmissão chega
///   tarde, e chega em ordem — pior, um relato perdido segura os seguintes. Por isso cada relato
///   é **auto-contido** (a janela inteira, não um incremento): chegar tarde o torna velho, nunca
///   errado, e quem recebe pode descartar o velho sem perder estado. Por RTCP/UDP um relato
///   perdido simplesmente não chegaria, que é melhor.
/// - **Tráfego a mais no mesmo rádio.** O JSON tem **83 bytes**; com moldura WebSocket e
///   cabeçalho TCP/IP, ~129 no ar. A 2 janelas por segundo são ~2,1 kbps contra os 4 000 kbps do
///   vídeo: **0,05 %**. Está abaixo do ruído da própria medida, e a alternativa por RTCP não
///   seria de graça (SR/RR já ocupam o caminho).
/// - **Um par de versão anterior MATA a sessão ao receber isto.** Uma etiqueta que a build não
///   conhece morre no `serde_json::from_str`, e `session.rs::olhar_sinalizacao` traduz o `Err`
///   para `EventoDeSessao::Falhou` — apesar do comentário de lá prometer que mensagem
///   pós-negociação desconhecida é ignorada. Medido em
///   `etiqueta_desconhecida_derruba_a_mensagem_e_nao_e_ignorada`, abaixo. Não morde hoje porque
///   as chaves de bancada nascem desligadas e o relato não sai; **é bloqueio para ligar por
///   padrão**, e o conserto que já existe é subir `PROTOCOL_VERSION`, que
///   `Announcement::is_compatible` já usa para recusar par de versão diferente.
/// - **Não serve ao `quall-probe`.** Ele fecha o `link` de propósito depois que o transporte
///   sobe (`session.rs`), então a sonda nunca vai relatar nada por aqui. É limitação real e
///   conhecida: o par que esta frente prova é app ↔ app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RelatoDoEnlace {
    /// Duração da janela, em ms.
    pub ms: u64,
    /// Pacotes que o emissor mandou na janela: vistos + perdidos de verdade.
    pub pacotes: u64,
    /// Perda **exata** na janela.
    pub perdidos: u64,
    /// Quadros exibidos com a referência condenada na janela.
    pub suspeitos: u64,
    /// Unidades de acesso IDR truncadas na janela.
    pub idrs_quebrados: u64,
    /// **Quadros que chegaram inteiros e o receptor não conseguiu entregar** na janela — porque a
    /// fila dele transbordou, não porque o rádio perdeu.
    ///
    /// # Por que este campo existe
    ///
    /// Sem ele o emissor é cego para o afogamento do outro lado, e o defeito não é teórico: em
    /// 09/09/2026 o S24 mandava 56 quadros por segundo a 1080p e o Dell decodificava 39,
    /// descartando 17 por segundo — com **perda de rede de 0,00 %** em toda janela. O controlador,
    /// que só olha perda, **subia** a taxa enquanto o receptor afogava. Ver `docs/bancada.md`
    /// §8.63.
    ///
    /// # Compatibilidade
    ///
    /// `#[serde(default)]`: um receptor de versão anterior não escreve o campo, e o emissor novo o
    /// lê como zero — que é exatamente o comportamento de antes. E um receptor novo falando com um
    /// emissor velho manda um campo a mais que o outro lado ignora, porque `serde` descarta chave
    /// desconhecida por padrão neste `struct`.
    #[serde(default)]
    pub nao_decodificados: u64,
}

/// Por que a outra ponta recusou, em código em vez de prosa. Ver [`SignalMessage::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausaDeRecusa {
    /// **O campo não veio, ou veio com um código que esta build não conhece.**
    ///
    /// É o valor de compatibilidade, e ele existe pelo mesmo motivo que o
    /// `QUALL_AUDIO_CODEC_DEFAULT = 0`: uma ponta que fala o protocolo anterior tem de cair no
    /// **comportamento de antes** — `Error::Pairing` — e não mudar de significado em silêncio.
    #[default]
    NaoInformada,
    /// O PIN não conferiu. Conselho: digitar de novo.
    PinIncorreto,
    /// Este pareamento não é reconhecido do outro lado. Conselho: pedir um PIN novo ao outro
    /// aparelho. **O conselho oposto do de cima** — é a dívida 29 inteira numa linha.
    NaoPareado,
    /// Recusa de pareamento que não é nenhuma das duas acima.
    Recusa,
    /// **As duas pontas falam versões diferentes do protocolo.** Conselho: atualizar a mais antiga.
    ///
    /// Acrescentada em 2026-08-31 junto com `PROTOCOL_VERSION = 2`, e ela **pode** ser acrescentada
    /// sem quebrar par antigo — ao contrário de uma etiqueta nova de `SignalMessage` — porque
    /// `causa_do_fio` degrada código desconhecido para `NaoInformada` em vez de falhar. O campo foi
    /// desenhado assim de propósito; a etiqueta de mensagem não.
    VersaoIncompativel,
    /// **Papel errado**: um receptor de vídeo num teleprompter, ou um controle num emissor de
    /// vídeo. Conselho: conectar no aparelho certo. Ver `docs/contrato-teleprompter.md` §2.
    ///
    /// Acrescentada pela mesma porta da de cima — `causa_do_fio` degrada para `NaoInformada` numa
    /// build anterior —, e ela só é mandada **no `Hello`**, antes do PIN: numa build anterior o
    /// `Error` que chega ali vira "o emissor recusou: <motivo>", sem olhar a causa.
    PapelIncompativel,
    /// **O outro lado já tem uma sessão**: um teleprompter com controle conectado. Conselho: tentar
    /// de novo daqui a pouco. Vira [`Error::Ocupado`].
    Ocupado,
}

impl CausaDeRecusa {
    /// A causa que corresponde a um erro local, para mandar no fio.
    ///
    /// É uma função do **tipo** do erro, nunca do texto dele. Se algum dia alguém for tentado a
    /// olhar a mensagem aqui, o item 29 do catálogo explica por que não.
    pub fn de(e: &Error) -> Self {
        match e {
            Error::WrongPin(_) => CausaDeRecusa::PinIncorreto,
            Error::NeedsPin(_) => CausaDeRecusa::NaoPareado,
            _ => CausaDeRecusa::Recusa,
        }
    }

    /// Reconstrói o erro do lado de cá a partir do que veio no fio.
    ///
    /// `NaoInformada` cai em [`Error::Pairing`] — o comportamento de antes desta mudança.
    pub fn erro(self, motivo: String) -> Error {
        match self {
            CausaDeRecusa::PinIncorreto => Error::WrongPin(motivo),
            CausaDeRecusa::NaoPareado => Error::NeedsPin(motivo),
            // **Não é pareamento**, e por isso não cai em `Error::Pairing`: uma casca que
            // rotulasse isto de "o PIN não conferiu" mandaria o usuário digitar de novo um PIN
            // que está certo — a dívida 29 outra vez, por outra porta. O conselho aqui é
            // atualizar o aplicativo mais antigo.
            CausaDeRecusa::VersaoIncompativel => Error::Protocol(motivo),
            // Também não é pareamento: o PIN pode estar certo e o aparelho, errado.
            CausaDeRecusa::PapelIncompativel => Error::Protocol(motivo),
            CausaDeRecusa::Ocupado => Error::Ocupado(motivo),
            CausaDeRecusa::NaoInformada | CausaDeRecusa::Recusa => Error::Pairing(motivo),
        }
    }
}

/// Desserializa a causa **sem nunca falhar**, e é de propósito.
///
/// O `derive` recusaria um código que esta build não conhece, e recusar derruba a mensagem
/// **inteira**: uma ponta mais nova que acrescentasse uma causa faria a mais velha trocar uma
/// recusa explicada por um erro de protocolo — pior do que não entender a causa. Código
/// desconhecido e campo ausente caem no mesmo lugar: `NaoInformada`, o comportamento de antes.
fn causa_do_fio<'de, D>(d: D) -> std::result::Result<CausaDeRecusa, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let bruto = Option::<String>::deserialize(d)?;
    Ok(match bruto.as_deref() {
        Some("pin_incorreto") => CausaDeRecusa::PinIncorreto,
        Some("nao_pareado") => CausaDeRecusa::NaoPareado,
        Some("recusa") => CausaDeRecusa::Recusa,
        Some("versao_incompativel") => CausaDeRecusa::VersaoIncompativel,
        Some("papel_incompativel") => CausaDeRecusa::PapelIncompativel,
        Some("ocupado") => CausaDeRecusa::Ocupado,
        _ => CausaDeRecusa::NaoInformada,
    })
}

impl SignalMessage {
    fn to_ws(&self) -> Result<Message> {
        Ok(Message::Text(serde_json::to_string(self)?.into()))
    }
}

/// Uma conexão de sinalização já estabelecida, dos dois lados igual.
///
/// Quem chama fica dono da thread que roda [`Link::poll`]. As threads do libdatachannel não
/// tocam no socket: elas empurram para a fila de saída por um [`LinkSender`], e o `poll` é quem
/// escreve. Assim o `WebSocket` nunca é acessado de duas threads, sem precisar de `Mutex` no
/// caminho quente.
pub struct Link {
    ws: WebSocket<TcpStream>,
    saida_tx: Sender<SignalMessage>,
    saida_rx: Receiver<SignalMessage>,
    peer: SocketAddr,
    fechado: bool,
}

/// Ponta de escrita da fila de saída. Clonável e `Send`: é o que vai para os callbacks do
/// libdatachannel.
#[derive(Clone)]
pub struct LinkSender(Sender<SignalMessage>);

impl LinkSender {
    /// Enfileira uma mensagem. Erro só quando o [`Link`] já foi destruído.
    pub fn send(&self, msg: SignalMessage) -> Result<()> {
        self.0
            .send(msg)
            .map_err(|_| Error::Signaling("o canal de sinalização já fechou".into()))
    }
}

impl Link {
    fn novo(ws: WebSocket<TcpStream>, peer: SocketAddr) -> Self {
        let (saida_tx, saida_rx) = mpsc::channel();
        Link {
            ws,
            saida_tx,
            saida_rx,
            peer,
            fechado: false,
        }
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// Ponta de escrita para outras threads.
    pub fn sender(&self) -> LinkSender {
        LinkSender(self.saida_tx.clone())
    }

    /// Escreve agora, sem passar pela fila. Use da própria thread do `poll`.
    pub fn send(&mut self, msg: &SignalMessage) -> Result<()> {
        self.ws.send(msg.to_ws()?).map_err(traduzir)?;
        self.ws.flush().map_err(traduzir)?;
        Ok(())
    }

    /// Drena a fila de saída para a rede, sem ler nada.
    ///
    /// Separado do [`Link::poll`] porque há um momento em que só se quer garantir que o que foi
    /// enfileirado saiu — na hora de encerrar, por exemplo, quando ler daria erro porque a outra
    /// ponta já foi embora.
    pub fn flush(&mut self) -> Result<()> {
        loop {
            match self.saida_rx.try_recv() {
                Ok(msg) => self.ws.send(msg.to_ws()?).map_err(traduzir)?,
                Err(TryRecvError::Empty) => break,
                // O `Link` é dono de um `Sender`, então `Disconnected` não acontece enquanto ele
                // existir. Tratar como fila vazia é o que resta de sensato.
                Err(TryRecvError::Disconnected) => break,
            }
        }
        self.ws.flush().map_err(traduzir)?;
        Ok(())
    }

    /// Drena a fila de saída e espera até `FATIA_DE_LEITURA` por uma mensagem.
    ///
    /// `Ok(None)` significa "nada por enquanto" — o caso normal, não um erro.
    pub fn poll(&mut self) -> Result<Option<SignalMessage>> {
        self.poll_por(FATIA_DE_LEITURA)
    }

    /// Igual a [`Link::poll`], com a fatia de espera escolhida por quem chama.
    ///
    /// Existe para o detector de queda ([`crate::session::Ready::proximo_evento`]) poder olhar a
    /// sinalização **sem** pagar 20 ms de bloqueio a cada quadro capturado.
    ///
    /// # `Duration::ZERO` quer dizer **não bloqueie**
    ///
    /// E durante um tempo não queria. A versão anterior elevava fatia zero a 1 ms e fazia uma
    /// leitura **bloqueante** — o que é uma tradução defensável de "espere o mínimo" e uma
    /// tradução errada de "não espere". O preço apareceu medido na bancada de 28/08: no emissor
    /// do Windows, `proximo_evento(Duration::ZERO)` custava **29,43 ms por volta** do laço,
    /// contra 7,97 ms de `bombear` e 2,67 ms de `enviar_quadro` — três quartos do laço numa
    /// chamada cujo argumento é zero.
    ///
    /// A causa é de plataforma e o defeito era daqui. O `SO_RCVTIMEO` do Windows não tem
    /// resolução de milissegundo: a espera acorda no tique do temporizador do sistema, ~15,6 ms.
    /// Pedir 1 ms de bloqueio lá é pedir um tique inteiro. O emissor do Windows contornou
    /// espiando a cada 200 ms; o contorno resolvia o sintoma dele e deixava o defeito de pé para
    /// **toda** casca que chamasse com fatia zero.
    ///
    /// O conserto é não armar prazo nenhum: com fatia zero o socket vai para o modo **não
    /// bloqueante** e o `read` devolve `WouldBlock` na hora, que [`seria_bloqueio`] já traduz
    /// para `Ok(None)`. É o mesmo mecanismo que [`apertar_mao_do_servidor`] usa, e pelo mesmo
    /// motivo: `WouldBlock` é o que as três plataformas devolvem, enquanto o `SO_RCVTIMEO`
    /// estourado sai como `TimedOut` no Windows e `WouldBlock` no Darwin.
    ///
    /// O modo é restaurado **sempre**, ainda que a leitura falhe: um socket deixado não
    /// bloqueante mudaria o comportamento de [`Link::send`], de [`Link::close`] (dívida 19) e da
    /// próxima leitura com prazo. Só o miolo desta função vê o socket em outro modo.
    ///
    /// Fatia **maior que zero** não mudou nada: continua bloqueando até a fatia, com piso de 1 ms
    /// — `Some(Duration::ZERO)` é erro no `set_read_timeout` e, no POSIX cru, `SO_RCVTIMEO` zero
    /// significa "sem prazo", que é o oposto da intenção. Quem pedia espera curta continua
    /// esperando o mesmo tanto.
    pub fn poll_por(&mut self, fatia: Duration) -> Result<Option<SignalMessage>> {
        self.flush()?;

        if fatia.is_zero() {
            return self.ler_sem_bloquear();
        }

        // O timeout do socket é o que faz o `read` devolver em vez de bloquear para sempre.
        self.ws
            .get_ref()
            .set_read_timeout(Some(fatia.max(Duration::from_millis(1))))?;
        let lido = self.ws.read();
        self.interpretar(lido)
    }

    /// Uma leitura que devolve na hora, sem tique de relógio nenhum. Ver [`Link::poll_por`].
    fn ler_sem_bloquear(&mut self) -> Result<Option<SignalMessage>> {
        self.ws.get_ref().set_nonblocking(true)?;
        let lido = self.ws.read();
        // Restaurar antes de interpretar, e sem `?`, para que nem um erro de protocolo nem um
        // `Closed` deixem o socket num modo que o resto do arquivo não espera.
        let voltou = self.ws.get_ref().set_nonblocking(false);
        let saida = self.interpretar(lido);
        match voltou {
            Ok(()) => saida,
            // A falha da leitura é a notícia mais importante; a do kernel só aparece se a
            // leitura tiver corrido bem.
            Err(e) => match saida {
                Err(erro) => Err(erro),
                Ok(_) => Err(Error::from(e)),
            },
        }
    }

    /// O que o `read` do `tungstenite` devolveu, em termos do nosso protocolo.
    fn interpretar(
        &mut self,
        lido: std::result::Result<Message, tungstenite::Error>,
    ) -> Result<Option<SignalMessage>> {
        match lido {
            Ok(Message::Text(texto)) => Ok(Some(serde_json::from_str(&texto)?)),
            // Binário não faz parte do protocolo de sinalização. Recusar em vez de ignorar
            // evita que uma divergência de versão vire silêncio.
            Ok(Message::Binary(_)) => {
                Err(Error::Signaling("mensagem binária na sinalização".into()))
            }
            Ok(Message::Close(_)) => {
                self.fechado = true;
                Err(Error::Closed)
            }
            // Ping/Pong/Frame: o `tungstenite` já respondeu o que precisava.
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if seria_bloqueio(&e) => Ok(None),
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                self.fechado = true;
                Err(Error::Closed)
            }
            Err(e) => Err(Error::Signaling(e.to_string())),
        }
    }

    /// Fecha educadamente. Falha aqui não interessa: o socket vai embora de qualquer jeito.
    ///
    /// # Prazo obrigatório na escrita (dívida 19)
    ///
    /// Com o par sumido, o `write` do `Bye` fica pendurado até a pilha TCP desistir — minutos, no
    /// Darwin. Um [`PRAZO_DE_DESPEDIDA`] curto transforma isso num erro que ninguém lê e devolve
    /// o controle a quem chama. É o que permite ao `broadcastFinished` da extension do iOS
    /// chamar `quall_session_close` sem estourar o prazo do sistema.
    ///
    /// O prazo é posto aqui, e não no [`Link`] inteiro, de propósito: uma escrita interrompida
    /// no meio de um quadro deixa o `tungstenite` com metade de uma mensagem na fila. Aqui isso
    /// não custa nada — a conexão morre na linha seguinte —, mas no caminho da negociação
    /// custaria um SDP truncado.
    pub fn close(&mut self, motivo: &str) {
        if self.fechado {
            return;
        }
        // Falha ao armar o prazo não impede a despedida: sem ele o comportamento é o antigo.
        let _ = self
            .ws
            .get_ref()
            .set_write_timeout(Some(PRAZO_DE_DESPEDIDA));
        let _ = self.send(&SignalMessage::Bye {
            motivo: motivo.to_string(),
        });
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
        self.fechado = true;
    }

    /// O socket por baixo. Só para os testes que precisam mexer no kernel, não no protocolo.
    #[cfg(test)]
    fn fluxo(&self) -> &TcpStream {
        self.ws.get_ref()
    }
}

fn config() -> WebSocketConfig {
    // `WebSocketConfig` é `#[non_exhaustive]`: partir do `default()` é o que sobrevive a uma
    // versão nova do tungstenite acrescentando campo.
    let mut cfg = WebSocketConfig::default();
    cfg.read_buffer_size = BUFFER;
    cfg.write_buffer_size = BUFFER;
    cfg.max_write_buffer_size = BUFFER * 4;
    cfg.max_message_size = Some(MAX_MENSAGEM);
    cfg.max_frame_size = Some(MAX_MENSAGEM);
    cfg.accept_unmasked_frames = false;
    cfg
}

fn traduzir(e: tungstenite::Error) -> Error {
    match e {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => Error::Closed,
        outro => Error::Signaling(outro.to_string()),
    }
}

/// `WouldBlock` e `TimedOut` são o timeout de leitura fazendo o trabalho dele, não falha.
///
/// `Interrupted` entra na lista porque um sinal (o `SIGWINCH` de redimensionar o terminal, por
/// exemplo) interrompe o `read` e não tem nada a ver com a conexão.
fn seria_bloqueio(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
    )
}

/// Classifica a falha do `connect` TCP da sinalização em [`Error::NoRoute`] ou [`Error::Io`].
///
/// **Dívida 28.** `NoRoute` nasceu para nomear "não há rota até o outro aparelho", e nascia
/// **num lugar só**: o `PeerState::Failed` do ICE, em [`crate::session`]. Só que o ICE só corre
/// **depois de a sinalização estar de pé** — e quando não há rota, quem morre primeiro é o
/// `connect` TCP daqui, uma camada antes. Aquilo saía como `Error::Io` e o status que nomearia a
/// causa certa era inalcançável **justamente no caso mais comum de não haver rota**.
///
/// A frente do receptor iOS pagou por isso: a tela dizia "sem rota para o host" a quem só
/// precisava tocar em **Permitir** no alerta de Rede Local, e a casca contornou com uma sonda TCP
/// própria. Contornar na casca é o que este catálogo já recusa em outros itens: cada uma das
/// quatro contornaria do seu jeito.
///
/// **A classificação é por `ErrorKind`, nunca por texto de mensagem** — que é a mesma regra que
/// fez o `NoRoute` existir. O `ErrorKind` do Rust já normaliza o errno de cada plataforma
/// (`EHOSTUNREACH`, `ENETUNREACH`, `ENETDOWN`, `EACCES`/`EPERM`), então não há número de sistema
/// operacional escrito aqui.
///
/// ## O que entra, e o que **de propósito** não entra
///
/// | `ErrorKind` | vira | por quê |
/// |---|---|---|
/// | `HostUnreachable` (`EHOSTUNREACH`) | `NoRoute` | é a definição do status. É o que o iOS devolve quando a Rede Local está negada. |
/// | `NetworkUnreachable` (`ENETUNREACH`) | `NoRoute` | não há rota para a sub-rede — Wi-Fi de hóspede, isolamento de AP. |
/// | `NetworkDown` (`ENETDOWN`) | `NoRoute` | a interface caiu; não há rota por definição. |
/// | `PermissionDenied` (`EACCES`/`EPERM`) | `NoRoute` | o sistema recusou **abrir** o socket para este destino: Rede Local no iOS, `INTERNET` no Android. Do ponto de vista do produto é a mesma conversa — falta uma permissão que o usuário concede —, e é o conselho que a casca precisa dar. |
/// | **`ConnectionRefused`** (`ECONNREFUSED`) | **`Io`** | **um RST voltou. Um RST é prova positiva de que existe rota**: o pacote foi e a resposta veio. O que falta é o app do outro lado escutando, e o conselho é o oposto — "abra o Quall no outro aparelho", não "libere a rede". Chamar isto de `NoRoute` seria trocar um diagnóstico errado por outro. |
/// | **`TimedOut`** | **`Io`** | ambíguo por construção: `TcpStream::connect_timeout` devolve `TimedOut` **tanto** para o `ETIMEDOUT` da pilha **quanto** para o nosso próprio prazo estourar. Um firewall que engole o SYN em silêncio cai aqui, e é de fato falta de rota — mas um aparelho lento numa rede boa também cai. Não dá para separar os dois neste ponto, e **um contador que finge saber é pior que um que admite não saber** (a lição da dívida 26). Fica em `Io`. |
///
/// Esta função é o único lugar que decide isso, e é o que impede as quatro cascas de
/// reimplementarem a classificação de quatro jeitos.
fn classificar_falha_de_conexao(destino: SocketAddr, e: &io::Error) -> Error {
    match e.kind() {
        ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable
        | ErrorKind::NetworkDown
        | ErrorKind::PermissionDenied => Error::NoRoute(format!(
            "não há rota até {destino} ({e}). No iOS isto é quase sempre a permissão de Rede \
             Local negada; nas outras plataformas, isolamento de AP ou Wi-Fi de hóspede.",
        )),
        _ => Error::Io(format!("{e}")),
    }
}

/// O servidor que o emissor hospeda.
pub struct SignalingServer {
    listener: TcpListener,
    ipv4: Option<TcpListener>,
    /// Quantos candidatos foram descartados sem que a espera terminasse. Ver
    /// [`SignalingServer::descartados`].
    descartados: AtomicU32,
}

impl SignalingServer {
    /// Abre a porta. `0` deixa o sistema escolher — use [`SignalingServer::port`] depois, e
    /// publique esse número no TXT do mDNS.
    ///
    /// Escuta em todas as interfaces porque na bancada o MacBook está no cabo e o Dell no
    /// Wi-Fi: amarrar numa interface só perderia metade dos aparelhos.
    /// Escuta IPv6 e IPv4 na mesma porta, com sockets separados. No Darwin, um socket IPv6
    /// dual-stack com reuso permite coexistir com um servidor IPv4 já ativo: a porta parece
    /// livre, mas clientes IPv4 chegam ao programa errado. Ver o teste da porta do prompter.
    /// Se o sistema não consegue criar um socket IPv6, conserva IPv4; erros de configuração,
    /// bind ou listen continuam visíveis, sem contornar uma porta ocupada em silêncio.
    pub fn bind(porta: u16) -> Result<Self> {
        let (listener, ipv4) = match Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP)) {
            Ok(socket) => {
                socket.set_only_v6(true)?;
                #[cfg(unix)]
                socket.set_reuse_address(true)?; // mesmo comportamento de TcpListener::bind no Unix
                socket.bind(&SockAddr::from(SocketAddr::from(([0u16; 8], porta))))?;
                socket.listen(128)?;
                let listener = TcpListener::from(socket);
                let ipv4 = TcpListener::bind(("0.0.0.0", listener.local_addr()?.port()))?;
                (listener, Some(ipv4))
            }
            Err(ipv6) => (
                TcpListener::bind(("0.0.0.0", porta)).map_err(|ipv4| {
                    io::Error::new(
                        ipv4.kind(),
                        format!("escuta IPv6: {ipv6}; escuta IPv4: {ipv4}"),
                    )
                })?,
                None,
            ),
        };
        Ok(SignalingServer {
            listener,
            ipv4,
            descartados: AtomicU32::new(0),
        })
    }

    /// Abre a porta **num endereço só**. O produto usa [`SignalingServer::bind`] (todas as
    /// interfaces); isto é da bancada que roda emissor e receptores na mesma máquina e não pode
    /// escutar fora dela: `quall-varias --so-local` prende a sinalização em `127.0.0.1` — no Dell, um
    /// `.exe` novo escutando fora do loopback faz o firewall do Windows mostrar um aviso na tela do
    /// usuário (`docs/monitor-virtual-windows.md` §14).
    pub fn bind_em(endereco: std::net::IpAddr, porta: u16) -> Result<Self> {
        let listener = TcpListener::bind((endereco, porta))?;
        Ok(SignalingServer {
            listener,
            ipv4: None,
            descartados: AtomicU32::new(0),
        })
    }

    /// **Quantos candidatos caíram sem derrubar a espera**, desde que o servidor abriu.
    ///
    /// Existe porque o conserto de 01/09/2026 (ver [`SignalingServer::aceitar_um`]) transforma
    /// um erro barulhento num descarte silencioso, e descarte silencioso que ninguém conta é
    /// exatamente como um defeito volta a viver escondido. [`crate::session::Ready::descartados`]
    /// leva este número até a casca, que o imprime no registro da sessão.
    pub fn descartados(&self) -> u32 {
        self.descartados.load(Ordering::Relaxed)
    }

    pub fn port(&self) -> Result<u16> {
        Ok(self.listener.local_addr()?.port())
    }

    /// Espera um receptor por até `limite`.
    ///
    /// Uma conexão por vez, no M1: a sessão é entre dois aparelhos e aceitar em paralelo só
    /// adiciona estado que ninguém usa ainda. O M5 (várias fontes no OBS) é que vai precisar
    /// disso, e vai precisar com um desenho pensado, não com um `accept` num laço.
    ///
    /// # O prazo vale para o handshake também (dívida 18)
    ///
    /// `limite` é o prazo **inteiro**: esperar a conexão chegar e completar o handshake
    /// WebSocket. Antes, ele cobria só a espera — uma conexão TCP que abrisse e não falasse
    /// pendurava aqui para sempre, porque `accept_hdr_with_config` num socket bloqueante sem
    /// prazo espera o keepalive do TCP ou um RST. Isso desmontava a peça em que as cascas se
    /// apoiam: o "prazo curto re-armado em laço" que iOS e Android usam como ponto de
    /// cancelamento não tinha prazo nenhum nesse caminho, e qualquer scanner de porta da LAN
    /// derrubava a tela de espera do produto para sempre.
    ///
    /// Uma conexão que não completa o handshake é **descartada**, e a espera continua até o
    /// prazo — quem não fala não pode negar o serviço a quem ia falar.
    pub fn accept(&self, limite: Duration) -> Result<Option<Link>> {
        self.accept_cancelavel(limite, &Cancelamento::novo())
    }

    /// Igual a [`SignalingServer::accept`], mas devolve [`Error::Cancelled`] quando a casca pede
    /// para parar. É aqui que a espera do emissor mora, e por isso é aqui que o botão Cancelar da
    /// tela de espera precisa chegar. Ver a dívida 10.
    pub fn accept_cancelavel(
        &self,
        limite: Duration,
        cancelar: &Cancelamento,
    ) -> Result<Option<Link>> {
        let fim = Instant::now() + limite;
        self.listener.set_nonblocking(true)?;
        if let Some(ipv4) = &self.ipv4 {
            if let Err(e) = ipv4.set_nonblocking(true) {
                let _ = self.listener.set_nonblocking(false);
                return Err(e.into());
            }
        }
        let resultado = self.aceitar_ate(fim, cancelar);
        // Volta ao estado de origem mesmo se algo falhou no meio: o listener sobrevive à chamada.
        let _ = self.listener.set_nonblocking(false);
        if let Some(ipv4) = &self.ipv4 {
            let _ = ipv4.set_nonblocking(false);
        }
        resultado
    }

    fn aceitar_ate(&self, fim: Instant, cancelar: &Cancelamento) -> Result<Option<Link>> {
        loop {
            let fluxo = loop {
                if cancelar.cancelado() {
                    return Err(Error::Cancelled);
                }
                let mut aceito = None;
                for listener in self.ipv4.iter().chain(std::iter::once(&self.listener)) {
                    match listener.accept() {
                        Ok((fluxo, _)) => {
                            aceito = Some(fluxo);
                            break;
                        }
                        Err(ref e) if seria_bloqueio(e) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                if let Some(fluxo) = aceito {
                    break fluxo;
                }
                if Instant::now() >= fim {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(20));
            };

            match self.aceitar_um(fluxo, fim) {
                Ok(Some(link)) => return Ok(Some(link)),
                // Conexão muda: fecha e volta a esperar quem vai falar.
                Ok(None) => continue,
                // **Candidato que morre não derruba a espera.** Ver [`Self::aceitar_um`].
                Err(_) => {
                    self.descartados.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    /// Tenta transformar **um** socket aceito num [`Link`]. `Ok(None)` é a conexão muda que não
    /// falou dentro do prazo dela; `Err` é a que morreu no meio.
    ///
    /// # O defeito que separar isto conserta (01/09/2026)
    ///
    /// O `accept_cancelavel` já dizia, em prosa, que *"uma conexão que não completa o handshake é
    /// **descartada**, e a espera continua até o prazo — quem não fala não pode negar o serviço a
    /// quem ia falar"*. O código cumpria isso para a conexão **muda** e não cumpria para a que
    /// **cai**: `apertar_mao_do_servidor(...)?` propagava, e um `?` numa função que devolve
    /// `Result` sobe até [`crate::session::hospedar`], que devolve à casca, que volta à tela
    /// inicial — com o listener fechado e o mDNS fora do ar.
    ///
    /// Medido na bancada de 01/09/2026, no emissor do Windows:
    ///
    /// ```text
    /// hospedar falhou: sinalização: IO error: Foi forçado o cancelamento de uma
    ///                  conexão existente pelo host remoto. (os error 10054)
    /// mdns: sumiu G3BRUNO
    /// ```
    ///
    /// Um `WSAECONNRESET` — o receptor que desiste ou o app que fecha no meio do handshake —
    /// fechava a porta 7877 **em definitivo, com o processo vivo**. Do outro lado só se via
    /// "depois que sai não conecta", que é o caso mais comum que existe: reconectar.
    ///
    /// `set_nodelay` e `peer_addr` entram aqui pelo mesmo motivo: os dois falham num socket cujo
    /// par já mandou RST, e nenhum dos dois é notícia sobre o **listener**.
    fn aceitar_um(&self, fluxo: TcpStream, fim: Instant) -> Result<Option<Link>> {
        // Sinalização é uma mensagem pequena de cada vez. Sem isto, o Nagle segura o
        // candidato ICE esperando companhia, e o ICE fica mais lento pelo motivo mais bobo
        // possível.
        fluxo.set_nodelay(true)?;
        let peer = match fluxo.peer_addr()? {
            SocketAddr::V6(v6) => v6
                .ip()
                .to_ipv4_mapped()
                .map(|ip| SocketAddr::from((ip, v6.port())))
                .unwrap_or(SocketAddr::V6(v6)),
            v4 => v4,
        };

        // Cada conexão tem o **seu** prazo, nunca o da sessão inteira: ver
        // [`PRAZO_DE_HANDSHAKE`].
        let prazo_desta = (Instant::now() + PRAZO_DE_HANDSHAKE).min(fim);
        Ok(apertar_mao_do_servidor(fluxo, prazo_desta)?.map(|ws| Link::novo(ws, peer)))
    }
}

/// O WebSocket já de pé, dos dois lados igual.
type WsFluxo = WebSocket<TcpStream>;

/// Empurra o handshake do **servidor** até `fim`, sem bloquear.
///
/// # Por que socket não bloqueante, e não `set_read_timeout`
///
/// O `tungstenite` só sabe retomar um handshake quando o erro de E/S é `WouldBlock`
/// ([`HandshakeError::Interrupted`]); qualquer outro vira `Failure`, que **consome** o estado do
/// meio do handshake e não dá para retomar. Um `SO_RCVTIMEO` estourado devolve `WouldBlock` no
/// Darwin e no Linux, mas `TimedOut` no Windows — e o Dell da bancada é Windows. Socket não
/// bloqueante devolve `WouldBlock` nas três plataformas.
///
/// Devolve `Ok(None)` quando o prazo acabou com o handshake pela metade: a conexão é descartada.
fn apertar_mao_do_servidor(fluxo: TcpStream, fim: Instant) -> Result<Option<WsFluxo>> {
    fluxo.set_nonblocking(true)?;
    let mut meio =
        match tungstenite::accept_hdr_with_config(fluxo, conferir_caminho, Some(config())) {
            Ok(ws) => return normalizar(ws).map(Some),
            Err(HandshakeError::Interrupted(m)) => m,
            Err(HandshakeError::Failure(e)) => {
                return Err(Error::Signaling(format!("handshake WebSocket falhou: {e}")))
            }
        };
    loop {
        if Instant::now() >= fim {
            return Ok(None);
        }
        std::thread::sleep(PASSO_DE_HANDSHAKE);
        meio = match meio.handshake() {
            Ok(ws) => return normalizar(ws).map(Some),
            Err(HandshakeError::Interrupted(m)) => m,
            Err(HandshakeError::Failure(e)) => {
                return Err(Error::Signaling(format!("handshake WebSocket falhou: {e}")))
            }
        };
    }
}

/// O mesmo do lado do **cliente**. Ver [`apertar_mao_do_servidor`] para o porquê do não
/// bloqueante.
fn apertar_mao_do_cliente(fluxo: TcpStream, url: &str, fim: Instant) -> Result<Option<WsFluxo>> {
    fluxo
        .set_nonblocking(true)
        .map_err(|e| Error::Io(format!("pôr o handshake em não-bloqueante: {e}")))?;
    let mut meio = match tungstenite::client::client_with_config(url, fluxo, Some(config())) {
        Ok((ws, _resp)) => return normalizar(ws).map(Some),
        Err(HandshakeError::Interrupted(m)) => m,
        Err(HandshakeError::Failure(e)) => {
            return Err(Error::Signaling(format!("handshake WebSocket falhou: {e}")))
        }
    };
    loop {
        if Instant::now() >= fim {
            return Ok(None);
        }
        std::thread::sleep(PASSO_DE_HANDSHAKE);
        meio = match meio.handshake() {
            Ok((ws, _resp)) => return normalizar(ws).map(Some),
            Err(HandshakeError::Interrupted(m)) => m,
            Err(HandshakeError::Failure(e)) => {
                return Err(Error::Signaling(format!("handshake WebSocket falhou: {e}")))
            }
        };
    }
}

/// Devolve o socket ao modo bloqueante que o [`Link::poll`] espera.
fn normalizar(ws: WsFluxo) -> Result<WsFluxo> {
    ws.get_ref()
        .set_nonblocking(false)
        .map_err(|e| Error::Io(format!("voltar o handshake a bloquear: {e}")))?;
    // Sem prazo de escrita no caminho normal: quem o arma, curto e de propósito, é
    // [`Link::close`]. Ver a dívida 19.
    ws.get_ref()
        .set_write_timeout(None)
        .map_err(|e| Error::Io(format!("tirar o prazo de escrita: {e}")))?;
    Ok(ws)
}

/// Recusa qualquer caminho que não seja o nosso.
///
/// Sem isto, qualquer página web aberta no mesmo aparelho poderia abrir um WebSocket para
/// `localhost` e conversar com o emissor. Não é defesa completa — o M6 deve conferir também a
/// `Origin` — mas fecha o erro de digitação e o scanner de porta bobo.
// `ErrorResponse` é um `http::Response` inteiro, e o clippy reclama do tamanho da variante de
// erro. Aqui isso não custa nada: a assinatura é ditada pelo `tungstenite`, e a função roda uma
// vez por handshake — nunca no caminho quente.
#[allow(clippy::result_large_err)]
fn conferir_caminho(req: &Request, resp: Response) -> std::result::Result<Response, ErrorResponse> {
    if req.uri().path() == SIGNALING_PATH {
        Ok(resp)
    } else {
        let mut recusa =
            ErrorResponse::new(Some(format!("o Quall atende apenas em {SIGNALING_PATH}")));
        *recusa.status_mut() = tungstenite::http::StatusCode::NOT_FOUND;
        Err(recusa)
    }
}

/// Abre a sinalização como receptor.
///
/// Serve tanto para o endereço que veio do mDNS quanto para o que o usuário digitou — é a mesma
/// função, e é por isso que o fallback por IP não é um caminho de código separado que ninguém
/// exercita.
/// `limite` cobre o prazo **inteiro**: abrir o TCP e completar o handshake. Um servidor que
/// aceita a conexão e não responde ao `GET` do WebSocket é o espelho da dívida 18 deste lado, e
/// penduraria o receptor da mesma forma.
///
/// **Sem cancelamento**: quem chama aqui espera o prazo inteiro. É o que os testes querem e o que
/// um roteiro de linha de comando quer; o produto entra por [`crate::session::conectar`], que
/// carrega a bandeira da casca. Ver [`abrir_tcp_atento`].
pub fn connect(destino: SocketAddr, limite: Duration) -> Result<Link> {
    connect_de(destino, limite, None, &Cancelamento::novo())
}

/// [`connect`], com a opção de **prender o socket a uma interface local** antes de conectar.
///
/// # Por que a sinalização também precisa disso
///
/// `TransportConfig::bind_address` prende a **mídia**, e por muito tempo isso pareceu bastar. Não
/// basta: a sinalização é um TCP comum, e um TCP comum sai por onde a tabela de rotas mandar.
///
/// O caso que torna isso concreto é o estúdio de câmeras por cabo. Cada Android ancorado por USB
/// cria uma sub-rede própria, e **elas podem coincidir** — a ancoragem da Samsung entrega faixas
/// como `192.168.42.0/24` e não há nada que impeça dois aparelhos de receberem a mesma. Com duas
/// rotas iguais, "por onde a rota mandar" deixa de ser pergunta com resposta: o SYN sai por uma
/// interface e o aparelho que responde pode ser o outro.
///
/// Nesta bancada o mesmo problema já apareceu em menor escala com três cabos de iOS: três rotas
/// `169.254/16` concorrentes, todas `UCSI`, e só a rota de host que o ARP cria desempatando.
///
/// # `bind` e depois `connect`, que a biblioteca padrão não faz
///
/// `TcpStream::connect_timeout` conecta com prazo e não sabe prender; `TcpListener::bind` prende e
/// não conecta. Não há a combinação em `std`, e é por isso que `socket2` entra aqui — ele é a
/// casca fina sobre o socket do sistema, e a sequência é a que o sistema sempre soube fazer.
///
/// **A família tem de bater.** Prender um endereço IPv4 e conectar num destino IPv6 é erro de quem
/// chamou, e sai como [`Error::Invalid`] em vez de um `bind` que falha com mensagem do sistema
/// sobre uma coisa que o produto não vai saber explicar.
/// Abre o TCP **consultando a bandeira de cancelamento**, que é o que `connect_timeout` não faz.
///
/// # Por que não dá para usar `connect_timeout`
///
/// `TcpStream::connect_timeout` e `Socket::connect_timeout` bloqueiam a thread dentro do sistema
/// até conectar, falhar ou o prazo estourar. Enquanto ela está lá, ninguém lê o [`Cancelamento`] —
/// e a FFI promete por escrito, em `quall-ffi/src/lib.rs`, que `quall_session_cancel` faz a
/// chamada bloqueante voltar. **A promessa não valia para esta fase**, que é justamente a mais
/// demorada das três (abrir o TCP, apertar a mão do WebSocket, parear).
///
/// Medido em 07/09 (`docs/bancada.md` §8.51): o receptor do OBS disca a cada tecla digitada, e
/// `192.1` — que o `getaddrinfo` expande para `192.0.0.1`, endereço roteável — segurou a thread
/// pelos **20 s inteiros** do prazo, com o cancelamento acionado a cada tecla e ninguém para
/// atendê-lo. Duas famílias de aparelho, 20,002 s e 20,003 s.
///
/// # O desenho, refeito em 09/09/2026 depois que ele reprovou no Windows
///
/// A primeira forma era socket não-bloqueante mais um laço que perguntava, em fatias curtas:
/// cancelou, deu erro, **conectou**. A terceira pergunta era `peer_addr().is_ok()` — "só um socket
/// já conectado tem par".
///
/// **Isso não é verdade no Windows, e o preço foi o receptor inteiro.** Medido nesta bancada em
/// 09/09/2026: o `quall-app.exe` do Dell falhava ao discar para o Mac com
/// `os error 10022` (`WSAEINVAL`) **quatro milissegundos** depois de começar, e o mesmo binário
/// conectava por `127.0.0.1` sem reclamar. O passo que devolvia o número, depois de nomeá-los um
/// a um, era o `set_nodelay` **de fora desta função** — ou seja, o laço tinha dado a conexão como
/// pronta e devolvido um socket que ainda estava a meio caminho. No Windows o `getpeername`
/// responde assim que o SYN sai; no Unix ele responde `ENOTCONN` até o aperto de mão fechar. O
/// teste de conclusão era correto num sistema e mentia no outro, e por isso o laço local passava:
/// em `127.0.0.1` a conexão fecha antes da primeira pergunta.
///
/// A forma de agora **não tem teste de conclusão próprio**: quem espera é
/// `socket2::Socket::connect_timeout`, que no Windows é `WSAPoll(POLLWRNORM)` e no Unix é `poll`,
/// numa thread; esta função dorme em fatias de [`FATIA_DE_ESPERA`] só perguntando se cancelaram.
/// Não é elegante ter uma thread aqui, e a versão anterior **argumentava contra ela por escrito**
/// — "não fica thread nem conexão órfã". O argumento continua verdadeiro e deixou de ser
/// suficiente: o que ele comprava era um teste de conclusão escrito à mão, e foi ele que quebrou.
///
/// **O que a thread custa, dito com todas as letras**: ao cancelar, ela continua discando até
/// conectar ou o prazo dela vencer, e só então larga o socket. Se conectar depois de o produto ter
/// desistido, o emissor do outro lado vê um candidato que abre e some — caso que ele já trata e
/// **conta**, desde 01/09, em `SignalingServer::descartados`.
fn abrir_tcp_atento(
    destino: SocketAddr,
    fim: Instant,
    origem: Option<IpAddr>,
    cancelamento: &Cancelamento,
) -> Result<TcpStream> {
    // **Dívida 28.** O `?` aqui era `From<io::Error>`, que joga tudo em `Error::Io` — e era por
    // ele que "não há rota" chegava à casca sem nome. Ver `classificar_falha_de_conexao`.
    // **Diagnóstico de bancada, 09/09/2026.** O receptor do Windows falhava com
    // `os error 10022` (WSAEINVAL) **um milissegundo** depois de discar para o Mac, e o mesmo
    // binário conectava por `127.0.0.1` sem reclamar. Com o erro dizendo só "e/s", não havia como
    // saber qual das seis chamadas desta função devolveu o número — e cada uma aponta para um
    // conserto diferente. O passo entra no texto do erro; a classificação de rota continua tendo
    // precedência, porque "não há rota" é conselho para o usuário e o passo é para nós.
    let erro_em = |passo: &str, e: &io::Error| match classificar_falha_de_conexao(destino, e) {
        Error::Io(m) => Error::Io(format!("{passo}: {m}")),
        outro => outro,
    };

    // Antes de abrir descritor nenhum: quem chegou aqui já cancelado não paga por um socket, e o
    // resultado deixa de depender de a pilha do sistema recusar na hora ou pendurar.
    if cancelamento.cancelado() {
        return Err(Error::Cancelled);
    }

    let dominio = Domain::for_address(destino);
    let sock = Socket::new(dominio, Type::STREAM, Some(Protocol::TCP))
        .map_err(|e| erro_em("abrir socket", &e))?;

    if let Some(local) = origem {
        // **A família tem de bater.** Prender IPv4 e conectar em IPv6 é erro de quem chamou, e
        // sai nomeado em vez de virar mensagem do sistema sobre coisa que o produto não explica.
        if local.is_ipv4() != destino.is_ipv4() {
            return Err(Error::Invalid(format!(
                "não dá para prender em {local} e conectar em {destino}: as famílias de endereço \
                 não batem"
            )));
        }
        // Porta 0: quem escolhe é o sistema. O que se está fixando é a **interface**, não o
        // número da porta de origem.
        sock.bind(&SockAddr::from(SocketAddr::new(local, 0)))
            .map_err(|e| erro_em("prender na origem", &e))?;
    }

    // A discagem inteira mora na thread: `connect_timeout` põe o socket em não-bloqueante,
    // disca, e espera na primitiva que cada sistema oferece para isto. Um canal traz de volta o
    // socket conectado ou o erro.
    let prazo = fim.saturating_duration_since(Instant::now());
    let alvo = SockAddr::from(destino);
    let (remetente, caixa) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("quall.discagem".into())
        .spawn(move || {
            let r = sock.connect_timeout(&alvo, prazo);
            // O `send` falha quando quem pediu já foi embora (cancelou ou estourou o prazo). Aí
            // o socket é largado aqui mesmo, no `drop` desta closure.
            let _ = remetente.send(r.map(|()| sock));
        })
        .map_err(|e| erro_em("subir a thread de discagem", &e))?;

    loop {
        if cancelamento.cancelado() {
            return Err(Error::Cancelled);
        }
        match caixa.recv_timeout(FATIA_DE_ESPERA) {
            Ok(Ok(sock)) => return Ok(TcpStream::from(sock)),
            // `TimedOut` do `poll_connect` é o prazo **desta discagem**, e o nome que a casca
            // mostra tem de ser o do prazo, não "e/s".
            Ok(Err(e)) if e.kind() == ErrorKind::TimedOut => {
                return Err(Error::Timeout(format!(
                    "não conectou em {destino} dentro do prazo"
                )))
            }
            Ok(Err(e)) => return Err(erro_em("discar", &e)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= fim {
                    return Err(Error::Timeout(format!(
                        "não conectou em {destino} dentro do prazo"
                    )));
                }
            }
            // A thread morreu sem falar. Não deveria acontecer; se acontecer, dizer isso é melhor
            // que esperar o prazo inteiro por uma resposta que não vem.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Io(format!(
                    "a discagem para {destino} terminou sem dizer o que houve"
                )))
            }
        }
    }
}

pub fn connect_de(
    destino: SocketAddr,
    limite: Duration,
    origem: Option<IpAddr>,
    cancelamento: &Cancelamento,
) -> Result<Link> {
    let fim = Instant::now() + limite;
    let fluxo = abrir_tcp_atento(destino, fim, origem, cancelamento)?;
    fluxo
        .set_nodelay(true)
        .map_err(|e| Error::Io(format!("desligar o Nagle: {e}")))?;

    // O escopo pertence ao socket local, não ao Host HTTP enviado ao outro aparelho.
    let url = url_de_sinalizacao(destino);
    match apertar_mao_do_cliente(fluxo, &url, fim)? {
        Some(ws) => Ok(Link::novo(ws, destino)),
        None => Err(Error::Timeout(
            "o outro aparelho aceitou a conexão e não respondeu ao handshake".into(),
        )),
    }
}

fn url_de_sinalizacao(destino: SocketAddr) -> String {
    let autoridade = SocketAddr::new(destino.ip(), destino.port());
    format!("ws://{autoridade}{SIGNALING_PATH}")
}

/// Confere o anúncio que chegou antes de qualquer outra coisa.
pub fn conferir_anuncio(anuncio: &Announcement) -> Result<()> {
    if !anuncio.is_compatible() {
        return Err(Error::Protocol(format!(
            "o outro aparelho fala a versão {} do protocolo; este fala a {}",
            anuncio.protocol_version, PROTOCOL_VERSION
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Capabilities, DeviceId};
    use std::thread;

    #[test]
    fn debug_da_sinalizacao_nao_reproduz_payloads() {
        let sdp = "v=0\r\na=ice-pwd:segredo-livre\r\nc=IN IP6 fd00::123".to_owned();
        let casos = [
            SignalMessage::Hello {
                announcement: anuncio(),
            },
            SignalMessage::Description {
                kind: "offer-segredo".into(),
                sdp: sdp.clone(),
            },
            SignalMessage::Candidate {
                candidate: "candidate:1 UDP 192.0.2.10 5000".into(),
                mid: "mid-privado".into(),
            },
            SignalMessage::Bye {
                motivo: "o PIN livre 901234".into(),
            },
            SignalMessage::Error {
                motivo: "segredo-livre".into(),
                causa: CausaDeRecusa::PinIncorreto,
            },
        ];
        for caso in casos {
            let debug = format!("{caso:?}");
            for proibido in [
                "segredo",
                "901234",
                "192.0.2.10",
                "fd00",
                "mid-privado",
                "Teste",
                "teste",
            ] {
                assert!(!debug.contains(proibido), "Debug expôs {proibido}: {debug}");
            }
            let serializado = serde_json::to_string(&caso).unwrap();
            assert_eq!(
                serde_json::from_str::<SignalMessage>(&serializado).unwrap(),
                caso
            );
        }
        assert_eq!(
            format!(
                "{:?}",
                SignalMessage::Error {
                    motivo: sdp,
                    causa: CausaDeRecusa::PinIncorreto
                }
            ),
            "Error { causa: PinIncorreto }"
        );
    }

    // ---- §8.51: o connect consulta o cancelamento ----

    /// Cancelado antes de começar, não abre socket e volta na hora.
    ///
    /// O endereço é irrelevante de propósito: o ponto é que a resposta **não** depende de a pilha
    /// do sistema recusar, pendurar ou rotear. Um teste que dependesse disso mediria a rede da
    /// máquina de CI em vez do código.
    #[test]
    fn cancelado_antes_de_conectar_nao_toca_na_rede() {
        let c = Cancelamento::novo();
        c.cancelar();
        let destino: SocketAddr = "192.0.2.1:7877".parse().expect("endereço");
        let antes = Instant::now();
        let r = abrir_tcp_atento(destino, antes + Duration::from_secs(30), None, &c);
        assert!(matches!(r, Err(Error::Cancelled)), "veio {r:?}");
        assert!(
            antes.elapsed() < Duration::from_millis(100),
            "voltou em {:?}, devia ser imediato",
            antes.elapsed()
        );
    }

    /// Prazo estourado devolve `Timeout` com o destino no texto, e não uma espera infinita.
    #[test]
    fn prazo_no_passado_volta_na_hora() {
        let c = Cancelamento::novo();
        let destino: SocketAddr = "192.0.2.1:7877".parse().expect("endereço");
        let antes = Instant::now();
        // Prazo já vencido: o laço faz uma volta, não acha par, e desiste.
        match abrir_tcp_atento(destino, antes, None, &c) {
            Err(Error::Timeout(m)) => assert!(m.contains("192.0.2.1"), "mensagem sem destino: {m}"),
            // Rede de CI sem rota para TEST-NET-1 recusa na hora, e isso também é resposta certa:
            // o que este teste proíbe é **demorar**.
            Err(Error::NoRoute(_)) | Err(Error::Io(_)) => {}
            outro => panic!("veio {outro:?}"),
        }
        assert!(
            antes.elapsed() < Duration::from_secs(1),
            "voltou em {:?}",
            antes.elapsed()
        );
    }

    // ---- Dívida 28: `NoRoute` alcançável na camada em que a rota de fato falta ----

    fn classificar(kind: ErrorKind) -> Error {
        let destino: SocketAddr = "192.168.1.131:47891".parse().expect("endereço");
        classificar_falha_de_conexao(destino, &io::Error::from(kind))
    }

    #[test]
    fn falta_de_rota_no_connect_tcp_vira_no_route_e_nao_io() {
        for kind in [
            ErrorKind::HostUnreachable,
            ErrorKind::NetworkUnreachable,
            ErrorKind::NetworkDown,
            ErrorKind::PermissionDenied,
        ] {
            assert!(
                matches!(classificar(kind), Error::NoRoute(_)),
                "{kind:?} tinha de virar NoRoute, veio {:?}",
                classificar(kind)
            );
        }
    }

    /// Um RST é **prova positiva de que existe rota**. Chamar isto de `NoRoute` mandaria o usuário
    /// mexer na rede quando o que falta é abrir o app do outro lado.
    #[test]
    fn conexao_recusada_nao_e_falta_de_rota() {
        assert!(matches!(
            classificar(ErrorKind::ConnectionRefused),
            Error::Io(_)
        ));
    }

    /// `connect_timeout` devolve `TimedOut` tanto para o `ETIMEDOUT` da pilha quanto para o nosso
    /// próprio prazo estourar. Não dá para separar aqui, então não se finge saber.
    #[test]
    fn tempo_esgotado_nao_e_promovido_a_falta_de_rota() {
        assert!(matches!(classificar(ErrorKind::TimedOut), Error::Io(_)));
    }

    /// A classificação é por `ErrorKind`, e o `ErrorKind` só serve se o Rust de fato traduzir o
    /// errno da plataforma. Isto fixa a tradução em vez de supô-la — se uma versão futura mudar o
    /// mapeamento, o teste cai aqui em vez de a casca voltar a receber `Io`.
    #[test]
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    fn o_errno_da_plataforma_chega_como_kind_e_nao_como_texto() {
        #[cfg(target_vendor = "apple")]
        let (ehostunreach, enetunreach, econnrefused) = (65, 51, 61);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let (ehostunreach, enetunreach, econnrefused) = (113, 101, 111);

        let destino: SocketAddr = "192.168.1.131:47891".parse().expect("endereço");
        let de_errno = |n| classificar_falha_de_conexao(destino, &io::Error::from_raw_os_error(n));

        assert!(matches!(de_errno(ehostunreach), Error::NoRoute(_)));
        assert!(matches!(de_errno(enetunreach), Error::NoRoute(_)));
        assert!(matches!(de_errno(econnrefused), Error::Io(_)));
    }

    // ---- O relato do enlace, e o que ele custa em compatibilidade ----

    #[test]
    fn o_relato_do_enlace_faz_ida_e_volta_por_json() {
        let r = RelatoDoEnlace {
            ms: 512,
            pacotes: 231,
            perdidos: 7,
            suspeitos: 3,
            idrs_quebrados: 1,
            nao_decodificados: 5,
        };
        let json = serde_json::to_string(&SignalMessage::Enlace(r)).expect("serializa");
        assert!(
            json.contains("\"t\":\"enlace\""),
            "a etiqueta mudou: {json}"
        );
        match serde_json::from_str::<SignalMessage>(&json).expect("desserializa") {
            SignalMessage::Enlace(v) => assert_eq!(v, r),
            outra => panic!("veio {outra:?}"),
        }
    }

    /// **Uma etiqueta desconhecida NÃO é ignorada: ela derruba a mensagem inteira.**
    ///
    /// Isto contradiz o comentário de `session.rs::olhar_sinalizacao` — *"qualquer outra mensagem
    /// depois da negociação é ruído; ignorar é melhor que derrubar uma sessão que está
    /// funcionando"* — e a contradição é anterior a `Enlace`: aquele `Ok(_) => None` só é
    /// alcançável por uma variante que **esta** build conhece. Uma que ela não conheça morre aqui,
    /// no `serde`, e `olhar_sinalizacao` traduz o `Err` para `EventoDeSessao::Falhou`.
    ///
    /// A consequência prática, e ela decide quando `Enlace` pode ser ligado por padrão: um par de
    /// versão anterior que receba `{"t":"enlace",…}` **mata a sessão** em vez de ignorá-la. Hoje
    /// isso não acontece porque as duas chaves de bancada nascem desligadas e o relato não sai;
    /// ligar por padrão exige antes **subir `PROTOCOL_VERSION`** (e `Announcement::is_compatible`
    /// já recusa par de versão diferente, então o problema some pelo mecanismo que existe) ou
    /// negociar a capacidade. Ver `docs/taxa-que-escuta.md` §6.
    /// **A recusa de versão tem de chegar como prosa, não como erro de transporte.**
    ///
    /// Medido em aparelho em 2026-08-31: com o A07 na versão 2 e o A10s na 1, o lado antigo
    /// mostrava `WebSocket protocol error: Connection reset without closing handshake` — a
    /// mensagem que manda caçar rede e cabo quando a resposta é "atualize o aplicativo". Ver
    /// `dizer_por_que_antes_de_fechar`, em `session.rs`.
    #[test]
    fn recusa_de_versao_chega_como_prosa_e_nao_como_pareamento() {
        let cru = r#"{"t":"error","motivo":"o outro aparelho fala a versão 1 do protocolo; este fala a 2","causa":"versao_incompativel"}"#;
        let msg = serde_json::from_str::<SignalMessage>(cru).expect("um par novo entende");
        let SignalMessage::Error { motivo, causa } = msg else {
            panic!("esperava Error");
        };
        assert_eq!(causa, CausaDeRecusa::VersaoIncompativel);
        // **Não pode virar `Error::Pairing`**: uma casca que rotulasse isto de "o PIN não
        // conferiu" mandaria o usuário digitar de novo um PIN que está certo.
        assert!(matches!(causa.erro(motivo.clone()), Error::Protocol(_)));
        assert!(motivo.contains("versão 1") && motivo.contains("fala a 2"));
    }

    /// O mesmo quadro, lido por uma build que **não conhece** a causa nova — que é exatamente o
    /// que um par da versão 1 faz com `versao_incompativel`. O código degrada, **a prosa
    /// sobrevive**, e é a prosa que o usuário lê.
    #[test]
    fn par_antigo_nao_entende_a_causa_e_ainda_assim_le_o_motivo() {
        let cru = r#"{"t":"error","motivo":"o outro aparelho fala a versão 2 do protocolo; este fala a 1","causa":"uma_causa_de_uma_versao_futura"}"#;
        let msg = serde_json::from_str::<SignalMessage>(cru)
            .expect("causa desconhecida NÃO pode derrubar a mensagem");
        let SignalMessage::Error { motivo, causa } = msg else {
            panic!("esperava Error");
        };
        assert_eq!(causa, CausaDeRecusa::NaoInformada);
        assert!(motivo.contains("versão 2"));
    }

    #[test]
    fn etiqueta_desconhecida_derruba_a_mensagem_e_nao_e_ignorada() {
        let cru = r#"{"t":"uma_coisa_que_esta_build_nao_conhece","x":1}"#;
        let saiu = serde_json::from_str::<SignalMessage>(cru);
        assert!(
            saiu.is_err(),
            "se um dia isto passar a ser Ok, o comentário de olhar_sinalizacao virou verdade e \
             este teste é que está errado — conferir os dois juntos",
        );
    }

    // ---- Dívida 29: a causa da recusa atravessa o fio como código, não como prosa ----

    /// O caminho inteiro da dívida 29 **sem socket**: o erro do anfitrião vira causa, a mensagem
    /// vai e volta por JSON, e o convidado reconstrói o erro. É exatamente o que `session.rs`
    /// faz, e é onde o tipo do erro morria.
    fn atravessa(e: &Error) -> Error {
        let saindo = SignalMessage::Error {
            motivo: e.to_string(),
            causa: CausaDeRecusa::de(e),
        };
        let json = serde_json::to_string(&saindo).expect("serializa");
        match serde_json::from_str::<SignalMessage>(&json).expect("desserializa") {
            SignalMessage::Error { motivo, causa } => causa.erro(motivo),
            outra => panic!("veio {outra:?}"),
        }
    }

    #[test]
    fn o_pin_errado_chega_do_outro_lado_como_pin_errado() {
        assert!(matches!(
            atravessa(&Error::WrongPin("o PIN não conferiu".into())),
            Error::WrongPin(_)
        ));
    }

    /// **O caso que a frente do Windows viu.** O anfitrião esqueceu o par; antes, isto chegava ao
    /// emissor como `Error::Pairing` e a casca escrevia "O PIN não conferiu" — com o PIN certo.
    #[test]
    fn o_par_esquecido_chega_como_needs_pin_e_nao_como_pin_errado() {
        let chegou = atravessa(&Error::NeedsPin("não conheço este aparelho".into()));
        assert!(matches!(chegou, Error::NeedsPin(_)), "veio {chegou:?}");
        assert!(
            !matches!(chegou, Error::WrongPin(_)),
            "dizer 'PIN errado' aqui é a dívida 29"
        );
    }

    #[test]
    fn recusa_generica_continua_generica() {
        assert!(matches!(
            atravessa(&Error::Pairing("mensagem fora de ordem".into())),
            Error::Pairing(_)
        ));
    }

    /// **Compatibilidade, pelo precedente do `QUALL_AUDIO_CODEC_DEFAULT = 0`.** Uma ponta que
    /// fala o protocolo anterior não manda `causa`. A ausência tem de cair no comportamento de
    /// antes — `Error::Pairing` —, nunca mudar de significado em silêncio e nunca derrubar a
    /// mensagem.
    #[test]
    fn recusa_de_uma_ponta_antiga_sem_causa_cai_no_comportamento_de_antes() {
        let velho = r#"{"t":"error","motivo":"pareamento: PIN incorreto"}"#;
        let msg: SignalMessage =
            serde_json::from_str(velho).expect("uma ponta antiga tem de ser lida");
        match msg {
            SignalMessage::Error { motivo, causa } => {
                assert_eq!(causa, CausaDeRecusa::NaoInformada);
                assert!(matches!(causa.erro(motivo), Error::Pairing(_)));
            }
            outra => panic!("veio {outra:?}"),
        }
    }

    /// E a direção contrária: uma ponta **mais nova** com uma causa que esta build não conhece.
    ///
    /// O `derive` recusaria a mensagem inteira, e recusar troca uma recusa explicada por um erro
    /// de protocolo — pior do que não entender a causa. Cai em `NaoInformada`.
    #[test]
    fn causa_desconhecida_de_uma_ponta_mais_nova_nao_derruba_a_mensagem() {
        let novo = r#"{"t":"error","motivo":"algo","causa":"coisa_que_ainda_nao_existe"}"#;
        let msg: SignalMessage = serde_json::from_str(novo).expect("não pode falhar");
        match msg {
            SignalMessage::Error { causa, .. } => {
                assert_eq!(causa, CausaDeRecusa::NaoInformada)
            }
            outra => panic!("veio {outra:?}"),
        }
    }

    /// A causa é função do **tipo** do erro, nunca do texto. Este teste é a tranca contra alguém
    /// voltar a comparar mensagem — que é o que a frente do Windows recusou fazer.
    #[test]
    fn a_causa_sai_do_tipo_do_erro_e_nao_da_mensagem() {
        // Mesma prosa, tipos diferentes: as causas têm de divergir.
        let texto = "o PIN não conferiu";
        assert_eq!(
            CausaDeRecusa::de(&Error::WrongPin(texto.into())),
            CausaDeRecusa::PinIncorreto
        );
        assert_eq!(
            CausaDeRecusa::de(&Error::NeedsPin(texto.into())),
            CausaDeRecusa::NaoPareado
        );
        assert_eq!(
            CausaDeRecusa::de(&Error::Pairing(texto.into())),
            CausaDeRecusa::Recusa
        );
    }

    fn anuncio() -> Announcement {
        Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId("teste".into()),
            display_name: "Teste".into(),
            capabilities: Capabilities {
                screen_source: true,
                camera_source: false,
                sink: true,
            },
            screen: None,
            papel: None,
        }
    }

    #[test]
    fn mensagens_sobrevivem_a_ida_e_volta_por_json() {
        let casos = vec![
            SignalMessage::Hello {
                announcement: anuncio(),
            },
            SignalMessage::Description {
                kind: "offer".into(),
                sdp: "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n".into(),
            },
            SignalMessage::Candidate {
                candidate: "candidate:1 1 UDP 2130706431 192.168.1.131 50000 typ host".into(),
                mid: "0".into(),
            },
            SignalMessage::CandidatesDone,
            SignalMessage::Bye {
                motivo: "fim".into(),
            },
        ];
        for caso in casos {
            let texto = serde_json::to_string(&caso).expect("serializa");
            let volta: SignalMessage = serde_json::from_str(&texto).expect("desserializa");
            assert_eq!(caso, volta);
        }
    }

    #[test]
    fn anuncio_de_outra_versao_e_recusado() {
        let mut a = anuncio();
        a.protocol_version = PROTOCOL_VERSION + 1;
        assert!(conferir_anuncio(&a).is_err());
        assert!(conferir_anuncio(&anuncio()).is_ok());
    }

    /// Sobe servidor e cliente em `127.0.0.1` e troca mensagens nos dois sentidos.
    ///
    /// É loopback, não a LAN: prova o protocolo e o enquadramento, **não** prova latência nem
    /// travessia de rede. Os números de rede são medidos na bancada, com a sonda.
    #[test]
    fn servidor_e_cliente_trocam_mensagens() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let lado_servidor = thread::spawn(move || -> Result<SignalMessage> {
            let mut link = servidor
                .accept(Duration::from_secs(5))?
                .ok_or_else(|| Error::Timeout("ninguém conectou".into()))?;
            link.send(&SignalMessage::Welcome {
                announcement: anuncio(),
            })?;
            for _ in 0..250 {
                if let Some(msg) = link.poll()? {
                    return Ok(msg);
                }
            }
            Err(Error::Timeout("nada chegou".into()))
        });

        let destino: SocketAddr = format!("127.0.0.1:{porta}").parse().expect("addr");
        let mut cliente = connect(destino, Duration::from_secs(5)).expect("conecta");

        let mut boas_vindas = None;
        for _ in 0..250 {
            if let Some(msg) = cliente.poll().expect("poll") {
                boas_vindas = Some(msg);
                break;
            }
        }
        assert!(
            matches!(boas_vindas, Some(SignalMessage::Welcome { .. })),
            "recebido: {boas_vindas:?}"
        );

        // Pelo `LinkSender`, que é o caminho que os callbacks do libdatachannel usam.
        cliente
            .sender()
            .send(SignalMessage::CandidatesDone)
            .expect("enfileira");
        // `flush` e não `poll`: o servidor encerra assim que recebe, e ler depois disso pega o
        // reset da conexão — que seria uma falha do teste, não do código.
        cliente.flush().expect("drena a fila");

        let recebido = lado_servidor.join().expect("thread").expect("mensagem");
        assert_eq!(recebido, SignalMessage::CandidatesDone);
    }

    #[test]
    fn caminho_errado_e_recusado() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let lado_servidor = thread::spawn(move || {
            // O `accept` falha porque o handshake é recusado — é o resultado esperado.
            let _ = servidor.accept(Duration::from_secs(5));
        });

        let fluxo = TcpStream::connect(format!("127.0.0.1:{porta}")).expect("tcp");
        let erro = tungstenite::client::client_with_config(
            format!("ws://127.0.0.1:{porta}/nao-e-o-quall"),
            fluxo,
            Some(config()),
        );
        assert!(erro.is_err(), "o servidor aceitou um caminho estranho");
        lado_servidor.join().expect("thread");
    }

    #[test]
    fn bind_em_prende_a_escuta_no_endereco_pedido() {
        let local = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        let servidor = SignalingServer::bind_em(local, 0).expect("bind_em");
        let endereco = servidor.listener.local_addr().expect("local_addr");
        assert_eq!(endereco.ip(), local, "a escuta fica só no loopback");
        assert!(endereco.port() > 0);
        // E o de sempre continua em todas as interfaces.
        let todas = SignalingServer::bind(0).expect("bind");
        assert!(todas
            .listener
            .local_addr()
            .expect("local_addr")
            .ip()
            .is_unspecified());
    }

    /// Troca mensagens reais pelo WebSocket nas duas famílias, na mesma porta. Um bind IPv6
    /// sozinho não prova que a escuta preservou o cliente IPv4 nem que o Host IPv6 é aceito.
    #[test]
    fn sinalizacao_dual_stack_troca_mensagens_ipv4_e_ipv6() {
        fn receber(link: &mut Link) -> SignalMessage {
            let fim = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(mensagem) = link.poll().unwrap() {
                    return mensagem;
                }
                assert!(
                    Instant::now() < fim,
                    "a mensagem não chegou dentro do prazo"
                );
            }
        }
        let servidor = SignalingServer::bind(0).expect("bind dual-stack");
        assert!(
            servidor.listener.local_addr().unwrap().is_ipv6(),
            "ambiente do teste requer IPv6"
        );
        let porta = servidor.port().unwrap();
        let worker = thread::spawn(move || {
            for ipv6 in [false, true] {
                let mut link = servidor
                    .accept(Duration::from_secs(2))
                    .unwrap()
                    .expect("cliente");
                assert_eq!(link.peer_addr().is_ipv6(), ipv6);
                let pedido = receber(&mut link);
                assert_eq!(pedido, SignalMessage::CandidatesDone);
                link.send(&SignalMessage::Bye {
                    motivo: "teste IPv6".into(),
                })
                .unwrap();
            }
        });
        for ip in [
            IpAddr::from([127, 0, 0, 1]),
            IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]),
        ] {
            let destino = SocketAddr::new(ip, porta);
            let mut link = connect_de(
                destino,
                Duration::from_secs(2),
                Some(ip),
                &Cancelamento::novo(),
            )
            .unwrap();
            link.send(&SignalMessage::CandidatesDone).unwrap();
            assert_eq!(
                receber(&mut link),
                SignalMessage::Bye {
                    motivo: "teste IPv6".into()
                }
            );
        }
        worker.join().unwrap();
    }

    #[test]
    fn bind_ipv6_ocupado_falha_sem_abrir_outro_servidor_ipv4() {
        let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP)).unwrap();
        socket.set_only_v6(true).unwrap();
        socket
            .bind(&SockAddr::from(SocketAddr::from(([0u16; 8], 0))))
            .unwrap();
        socket.listen(1).unwrap();
        let ocupado = TcpListener::from(socket);
        let porta = ocupado.local_addr().unwrap().port();
        assert!(
            SignalingServer::bind(porta).is_err(),
            "uma porta IPv6 ocupada não admite fallback silencioso"
        );
    }

    #[test]
    fn websocket_ipv6_nao_envia_indice_local_no_host_http() {
        use tungstenite::client::IntoClientRequest;
        let destino = "[fe80::1234%9]:7877".parse().unwrap();
        let url = url_de_sinalizacao(destino);
        assert_eq!(url, "ws://[fe80::1234]:7877/quall/v1");
        let pedido = url.into_client_request().unwrap();
        assert_eq!(pedido.headers()["Host"], "[fe80::1234]:7877");
    }

    #[test]
    fn accept_devolve_none_quando_ninguem_conecta() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let r = servidor
            .accept(Duration::from_millis(120))
            .expect("sem erro");
        assert!(r.is_none());
    }

    /// **Prender o socket de sinalização não quebra a conexão que já funcionava.**
    ///
    /// O caso feliz é o que protege o produto: prendendo em `127.0.0.1` contra um servidor em
    /// `127.0.0.1`, o handshake fecha igual. Sem este teste, o `bind` poderia estar errado e só
    /// aparecer no dia em que alguém usasse um cabo.
    #[test]
    fn prender_a_sinalizacao_numa_interface_nao_quebra_o_handshake() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let destino = SocketAddr::from(([127, 0, 0, 1], servidor.port().expect("porta")));
        let fio = thread::spawn(move || {
            servidor
                .accept(Duration::from_secs(5))
                .expect("sem erro")
                .expect("alguém chegou")
        });
        let cliente = connect_de(
            destino,
            Duration::from_secs(5),
            Some(IpAddr::from([127, 0, 0, 1])),
            &Cancelamento::novo(),
        )
        .expect("conecta preso");
        assert_eq!(cliente.peer_addr(), destino);
        drop(fio.join().expect("a thread do servidor não caiu"));
    }

    /// **Família trocada é erro de quem chamou, e sai nomeado.**
    ///
    /// Prender IPv6 e conectar em IPv4 falharia no `bind` do sistema com uma mensagem que o
    /// produto não saberia explicar. `Error::Invalid` diz o que está errado e com quais dois
    /// endereços — que é o que uma casca pode mostrar a alguém.
    #[test]
    fn prender_em_familia_diferente_do_destino_e_recusado_com_nome() {
        let destino = SocketAddr::from(([127, 0, 0, 1], 7877));
        let erro = match connect_de(
            destino,
            Duration::from_millis(200),
            Some(IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1])),
            &Cancelamento::novo(),
        ) {
            Ok(_) => panic!("conectou com a família trocada"),
            Err(e) => e,
        };
        assert!(matches!(erro, Error::Invalid(_)), "veio {erro:?}");
        assert!(
            erro.to_string().contains("famílias de endereço não batem"),
            "a mensagem não explica: {erro}"
        );
    }

    /// **Prender num endereço que esta máquina não tem falha na hora, e não no prazo.**
    ///
    /// É a diferença entre "o outro aparelho não respondeu" e "o pedido não fazia sentido". Sem
    /// isso, um endereço digitado errado viraria um `TimedOut` de vinte segundos e a pessoa
    /// procuraria o defeito na rede do vizinho.
    #[test]
    fn prender_em_endereco_que_a_maquina_nao_tem_falha_rapido() {
        let destino = SocketAddr::from(([127, 0, 0, 1], 7877));
        let inicio = std::time::Instant::now();
        if let Ok(_) = connect_de(
            destino,
            Duration::from_secs(20),
            Some(IpAddr::from([203, 0, 113, 7])),
            &Cancelamento::novo(),
        ) {
            panic!("conectou preso a um endereço que esta máquina não tem");
        }
        assert!(
            inicio.elapsed() < Duration::from_secs(2),
            "demorou {:?} — devia falhar no bind, não no prazo",
            inicio.elapsed()
        );
    }

    /// Roda `trabalho` em outra thread e devolve quanto ele demorou, ou `None` se ele não
    /// terminou dentro de `paciencia`.
    ///
    /// A thread é deixada para trás de propósito quando o prazo estoura: o defeito que estes
    /// testes reproduzem é exatamente uma chamada que **nunca** volta, e não há como interrompê-la
    /// de fora. Sem isto o teste não falharia — ficaria pendurado, que é o pior tipo de portão
    /// vermelho (ver a nota sobre o Dell G3 em `transport.rs`).
    fn medir_em_outra_thread(
        paciencia: Duration,
        trabalho: impl FnOnce() + Send + 'static,
    ) -> Option<Duration> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let inicio = std::time::Instant::now();
            trabalho();
            let _ = tx.send(inicio.elapsed());
        });
        rx.recv_timeout(paciencia).ok()
    }

    /// **Dívida 18.** Uma conexão TCP que abre e não fala pendurava o `accept` para sempre — e
    /// com ele o `quall_host` inteiro, que é a tela de espera do produto.
    ///
    /// Antes do conserto este teste não falha em 1,2 s: ele **nunca** volta. Por isso a medição
    /// é feita em outra thread.
    #[test]
    fn conexao_que_abre_e_nao_fala_nao_pendura_o_accept() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        // O scanner de porta da LAN, ou o app que morreu entre o `connect` e o `GET`.
        let mudo = TcpStream::connect(format!("127.0.0.1:{porta}")).expect("tcp");

        let levou = medir_em_outra_thread(Duration::from_secs(20), move || {
            let _ = servidor.accept(Duration::from_millis(600));
        })
        .expect("o `accept` ficou pendurado no handshake de uma conexão muda");

        assert!(
            levou < Duration::from_secs(10),
            "o `accept` levou {levou:?} para desistir de uma conexão muda"
        );
        drop(mudo);
    }

    /// A conexão muda não pode **negar o serviço** a quem ia falar: o `accept` descarta a muda e
    /// continua esperando dentro do mesmo prazo.
    #[test]
    fn receptor_de_verdade_entra_apesar_de_uma_conexao_muda_na_frente() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let mudo = TcpStream::connect(format!("127.0.0.1:{porta}")).expect("tcp");

        let lado_servidor = thread::spawn(move || -> Result<Option<Link>> {
            servidor.accept(Duration::from_secs(10))
        });

        // Folga para o `accept` já ter pegado e descartado a conexão muda.
        thread::sleep(Duration::from_millis(200));
        let destino: SocketAddr = format!("127.0.0.1:{porta}").parse().expect("addr");
        let cliente = connect(destino, Duration::from_secs(5)).expect("conecta");

        let link = lado_servidor
            .join()
            .expect("thread")
            .expect("sem erro")
            .expect("o receptor de verdade tinha de entrar");
        assert_eq!(link.peer_addr().ip(), cliente.peer_addr().ip());
        drop(mudo);
    }

    /// **Dívida 19.** Com o par sumido — aqui, com o socket entupido porque ninguém lê do outro
    /// lado —, o `write` do `Bye` ficava pendurado até a pilha TCP desistir, na casa dos minutos.
    ///
    /// É por isso que o `broadcastFinished` da extension do iOS não podia chamar
    /// `quall_session_close`: o sistema mata a extension muito antes de o TCP desistir.
    #[test]
    fn close_nao_pendura_quando_o_par_nao_le_mais() {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        // A outra ponta aceita e **nunca lê**. Segurar o `Link` vivo é o que mantém o socket
        // aberto sem consumir nada — o par que sumiu sem fechar.
        let surdo = thread::spawn(move || {
            let link = servidor
                .accept(Duration::from_secs(10))
                .expect("accept")
                .expect("link");
            thread::sleep(Duration::from_secs(20));
            drop(link);
        });

        let destino: SocketAddr = format!("127.0.0.1:{porta}").parse().expect("addr");
        let mut cliente = connect(destino, Duration::from_secs(5)).expect("conecta");

        // Entope o socket até o kernel recusar mais um byte. A partir daqui, qualquer escrita
        // bloqueia — que é o que acontece de verdade quando o aparelho do outro lado some.
        let entupiu = entupir(&cliente);
        assert!(
            entupiu,
            "não deu para encher o socket; o teste não estaria provando nada"
        );

        let levou = medir_em_outra_thread(Duration::from_secs(30), move || {
            cliente.close("o par sumiu");
        })
        .expect("o `close` ficou pendurado escrevendo o `Bye` num socket entupido");

        assert!(
            levou < Duration::from_secs(5),
            "o `close` levou {levou:?} para desistir do `Bye`"
        );
        surdo.join().expect("thread do surdo");
    }

    /// Escreve no socket até o kernel devolver `WouldBlock`. Devolve se conseguiu entupir.
    fn entupir(link: &Link) -> bool {
        use std::io::Write;

        let fluxo = link.fluxo();
        if fluxo.set_nonblocking(true).is_err() {
            return false;
        }
        let bloco = vec![0xAAu8; 64 * 1024];
        let mut total = 0usize;
        let mut cheio = false;
        // Teto para o caso de o buffer crescer sem parar: 64 MiB já é absurdo para loopback.
        while total < 64 * 1024 * 1024 {
            let mut escritor: &TcpStream = fluxo;
            match escritor.write(&bloco) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    cheio = true;
                    break;
                }
                Err(_) => break,
            }
        }
        let _ = fluxo.set_nonblocking(false);
        cheio
    }

    // ---- Fatia zero quer dizer não bloqueie (o defeito medido em 28/08 no emissor do Windows) ----

    /// Um par de [`Link`] conectados por loopback, os dois vivos até quem chama largar.
    fn par_conectado() -> (Link, Link) {
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");

        let lado_servidor = thread::spawn(move || {
            servidor
                .accept(Duration::from_secs(10))
                .expect("accept")
                .expect("ninguém conectou")
        });

        let destino: SocketAddr = format!("127.0.0.1:{porta}").parse().expect("addr");
        let cliente = connect(destino, Duration::from_secs(10)).expect("conecta");
        let anfitriao = lado_servidor.join().expect("thread do anfitrião");
        (anfitriao, cliente)
    }

    /// **O defeito.** `poll_por(Duration::ZERO)` bloqueava, e no Windows bloqueava por um tique do
    /// temporizador do sistema.
    ///
    /// O piso da versão anterior era 1 ms por chamada em **qualquer** plataforma — ela elevava a
    /// fatia com `max(1ms)` e fazia leitura bloqueante —, logo estas 200 voltas custariam ≥ 200 ms
    /// aqui, e ~3,1 s no Windows, onde o `SO_RCVTIMEO` acorda no tique de ~15,6 ms. O teto de
    /// 60 ms é folga de mais de 3× abaixo daquele piso e ordens de grandeza acima do custo de uma
    /// leitura não bloqueante.
    ///
    /// Este teste falha na versão anterior do código em qualquer plataforma. Ele **não** reproduz
    /// os 29,43 ms medidos na bancada: aquele número é do tique do Windows, e no Darwin o piso
    /// antigo era ~1 ms. O que ele fixa é a regra que vale nas três — fatia zero não espera.
    #[test]
    fn fatia_zero_nao_bloqueia() {
        let (_anfitriao, mut cliente) = par_conectado();

        const VOLTAS: u32 = 200;
        let inicio = Instant::now();
        for _ in 0..VOLTAS {
            assert!(
                cliente.poll_por(Duration::ZERO).expect("poll").is_none(),
                "não havia nada para ler"
            );
        }
        let levou = inicio.elapsed();

        assert!(
            levou < Duration::from_millis(60),
            "{VOLTAS} espiadas com fatia zero levaram {levou:?} ({:.3} ms por volta). Fatia zero \
             está bloqueando de novo: o piso de 1 ms sozinho daria {VOLTAS} ms.",
            levou.as_secs_f64() * 1000.0 / f64::from(VOLTAS)
        );
    }

    /// Não bloquear não é deixar de entregar: o que **já chegou** sai na espiada de fatia zero.
    #[test]
    fn fatia_zero_entrega_a_mensagem_que_ja_chegou() {
        let (mut anfitriao, mut cliente) = par_conectado();
        anfitriao
            .send(&SignalMessage::CandidatesDone)
            .expect("envia");

        // O laço existe porque fatia zero **não** espera a mensagem atravessar o loopback — e não
        // deve. O que ela promete é entregar o que já está no buffer, e é isso que se afere.
        let prazo = Instant::now() + Duration::from_secs(5);
        let mut visto = None;
        while Instant::now() < prazo {
            if let Some(msg) = cliente.poll_por(Duration::ZERO).expect("poll") {
                visto = Some(msg);
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(visto, Some(SignalMessage::CandidatesDone));
    }

    /// A tranca contra o conserto virar regressão: quem pede espera continua esperando, **e** o
    /// socket volta ao modo bloqueante depois da espiada de fatia zero.
    ///
    /// A segunda metade é o risco real do conserto. Se o modo não bloqueante vazasse da função,
    /// toda leitura seguinte devolveria `WouldBlock` na hora — o `poll()` de 20 ms da negociação
    /// viraria giro em vazio, e o `close` da dívida 19 deixaria de respeitar o prazo de escrita.
    #[test]
    fn fatia_maior_que_zero_espera_e_o_socket_volta_a_bloquear_depois_da_fatia_zero() {
        let (_anfitriao, mut cliente) = par_conectado();
        let espera = Duration::from_millis(60);

        let marca = Instant::now();
        assert!(cliente.poll_por(espera).expect("poll").is_none());
        let antes_de_qualquer_zero = marca.elapsed();

        assert!(cliente.poll_por(Duration::ZERO).expect("poll").is_none());

        let marca = Instant::now();
        assert!(cliente.poll_por(espera).expect("poll").is_none());
        let depois_da_fatia_zero = marca.elapsed();

        for (quando, levou) in [
            ("antes de qualquer fatia zero", antes_de_qualquer_zero),
            ("depois de uma fatia zero", depois_da_fatia_zero),
        ] {
            assert!(
                levou >= Duration::from_millis(40),
                "a espera de {espera:?} {quando} durou {levou:?} — o socket não estava bloqueando"
            );
        }

        // E a escrita continua funcionando pelo caminho normal.
        cliente.send(&SignalMessage::CandidatesDone).expect("envia");
    }
}
