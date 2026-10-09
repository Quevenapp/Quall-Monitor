//! Transporte WebRTC sobre `libdatachannel`, via o crate `datachannel`. Mídia sempre P2P,
//! DTLS-SRTP.
//!
//! Fica atrás da feature `webrtc` (ligada por padrão). Ela é o único pedaço do núcleo que não é
//! Rust puro: arrasta libdatachannel (C++, cmake) e um OpenSSL compilado do fonte. Quem só
//! precisa de protocolo, descoberta, sinalização e pareamento não deveria pagar um toolchain
//! C++ por isso.
//!
//! # LAN-only não é configuração, é o desenho
//!
//! [`TransportConfig`] não tem campo para servidor ICE, e a `RtcConfig` é criada com a lista de
//! servidores **vazia**. Sem STUN e sem TURN, o ICE só reúne candidatos `host` — os endereços
//! das interfaces locais. É o que garante, no código e não no comentário, que a mídia nunca sai
//! da LAN nem passa por intermediário.
//!
//! # Memória
//!
//! Os eventos saem por dois canais separados, e o motivo é o orçamento de ~50 MB da Broadcast
//! Upload Extension do iOS:
//!
//! - **Controle** (SDP, candidatos, mudança de estado): canal comum. São poucos por sessão e
//!   perder um deles impede a conexão de fechar, então não podem ser descartados.
//! - **Dados** (quadros): canal *limitado*, com `try_send`. Se o consumidor não acompanha, o
//!   quadro é **descartado** e contado, em vez de a fila crescer. Descartar quadro é o
//!   comportamento certo para vídeo ao vivo; encher a memória de um processo de 50 MB não é.
//!
//! O contador de descarte é parte da medição, não um detalhe: latência baixa com metade dos
//! quadros no chão não é latência baixa.
//!
//! # Tamanho: libdatachannel cabe na extension do iOS
//!
//! Era a pergunta que o M1 existia para responder, porque um "não" autorizaria revisitar a
//! decisão de transporte (o contraponto seria `str0m`, Rust nativo). Medido em 2026-08-21,
//! `quall-probe` em release — um executável autocontido com núcleo, mDNS, WebSocket, X25519,
//! libdatachannel **e** o OpenSSL dela, tudo estático:
//!
//! | alvo | release | com `strip` total |
//! |---|---|---|
//! | `aarch64-apple-darwin` | 7,91 MB | 7,01 MB |
//! | `aarch64-apple-ios` | 7,84 MB | **6,95 MB** |
//! | `aarch64-linux-android` | 9,82 MB | 7,62 MB |
//! | `armv7-linux-androideabi` | 7,46 MB | 5,60 MB |
//!
//! **Cabe, com folga.** Contra os ~50 MB da Broadcast Upload Extension, ~7 MB de binário deixam
//! o orçamento praticamente inteiro para captura e encode. A decisão por libdatachannel está
//! confirmada por medição, não por expectativa.
//!
//! Ligar a feature `media` do `datachannel-sys` (que traz libsrtp, necessária para tracks RTP de
//! vídeo de verdade, a partir do M3) leva o `aarch64-apple-ios` com `strip` de 6,95 MB para
//! 7,03 MB — cerca de 90 KB. Não muda a conclusão.
//!
//! Cuidado ao ler `libquall.a`: o arquivo tem ~46 MB, mas é um *archive*, e o linker descarta o
//! que ninguém chama. O número que importa é o do binário ligado, que é o da tabela.

// O módulo inteiro some quando a feature está desligada. Fica aqui, e não em `lib.rs`, porque
// `lib.rs` é território comum e este arquivo é o dono da dependência.
#![cfg(feature = "webrtc")]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use datachannel::{
    ConnectionState, DataChannelHandler, DataChannelInfo, DataChannelInit, IceCandidate,
    PeerConnectionHandler, Reliability, RtcConfig, RtcDataChannel, RtcPeerConnection, SdpType,
    SessionDescription,
};

use crate::error::{Error, Result};
use crate::portao::{Barreira, Portao, PRAZO_DA_BARREIRA};
use crate::track::{TrackConfig, TrackEmissor, TrackReceptor};

/// Liga o registro interno da libdatachannel/libjuice, que é a única testemunha de um datagrama
/// descartado no socket de saída (`Send failed, buffer is full`). **Tem de ser chamado antes de a
/// primeira sessão subir**: o crate `datachannel` lê o nível uma vez só, ao criar a conexão.
///
/// Reexportado aqui porque as cascas falam com `quall-core` e não com `quall-rtc`. Ver
/// `quall_rtc::ativar_registro` para o mecanismo e para o porquê de o nível não passar de `Info`.
pub use quall_rtc::{ativar_registro as ativar_registro_da_biblioteca, NivelDeRegistro};

/// Rótulo do canal de dados da sessão.
pub const CHANNEL_LABEL: &str = "quall/media/v1";

/// Quantos quadros cabem na fila de entrada antes de o transporte começar a descartar.
///
/// 64 quadros a 60 fps é pouco mais de um segundo de folga: suficiente para absorver um soluço
/// de agendamento, curto o bastante para que a fila nunca vire um buffer de latência.
pub const FILA_DE_DADOS: usize = 64;

/// **O teto de uma mensagem** de [`Mensageiro`], em bytes: 256 KiB.
///
/// Não é escolha nossa, é o da biblioteca. As duas pontas são libdatachannel, e cada uma anuncia
/// `a=max-message-size` com o `DEFAULT_LOCAL_MAX_MESSAGE_SIZE` dela (`internals.hpp:41`, 256 KiB);
/// o teto efetivo é o menor dos dois (`peerconnection.cpp:118-133`). Acima dele,
/// `DataChannel::outgoing` **lança exceção** (`datachannel.cpp:196-197`) — e a regra de plataforma
/// é não deixar a API C lançar. Por isso o tamanho é conferido **antes** de chegar lá, como
/// `quall_track_send_frame` faz com o quadro.
///
/// Medido no teste `o_teto_da_mensagem_e_o_da_biblioteca`: exatamente este tamanho atravessa, um
/// byte a mais a biblioteca recusa.
pub const TETO_DA_MENSAGEM: usize = 256 * 1024;

/// **O teto que o outro lado anunciou**, lido do SDP dele (`a=max-message-size`), já limitado ao
/// nosso [`TETO_DA_MENSAGEM`].
///
/// É a mesma conta de `PeerConnection::remoteMaxMessageSize` (`peerconnection.cpp:118-133`), feita
/// deste lado para a mensagem ser recusada **antes** da API C: sem o atributo, a biblioteca supõe
/// 65 536 bytes (`DEFAULT_REMOTE_MAX_MESSAGE_SIZE`, `internals.hpp:42`); com `0`, "qualquer
/// tamanho", que o teto local limita. Entre dois Quall o atributo sempre vem com 256 KiB; a conta
/// existe para o dia em que do outro lado houver outra coisa.
fn teto_do_sdp(sdp: &str) -> usize {
    for linha in sdp.lines() {
        if let Some(resto) = linha.trim().strip_prefix("a=max-message-size:") {
            if let Ok(n) = resto.trim().parse::<usize>() {
                return if n == 0 { TETO_DA_MENSAGEM } else { n.min(TETO_DA_MENSAGEM) };
            }
        }
    }
    65_536
}

/// O que a fila de entrada faz quando enche.
///
/// **Depende do que o canal carrega**, e quem diz o que ele carrega é a confiabilidade que quem
/// ofereceu escolheu (ver [`Delivery`]). Não é um ajuste à parte justamente para não haver como
/// combinar os dois errado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuandoEnche {
    /// Descarta a que está chegando. É o comportamento de sempre, e continua sendo o de todo canal
    /// sem retransmissão: é por onde o `quall-probe` mede, e mudar a regra mudaria o que ele mede.
    DescartaANova = 0,
    /// Descarta a mais velha da fila e guarda a que chegou. É o de [`Delivery::ReliableUnordered`],
    /// o modo das mensagens de estado: numa rajada (arrastar a velocidade com o leitor atrasado), o
    /// valor que importa é o **último**, e descartar a nova perdia exatamente ele.
    DescartaAVelha = 1,
}

/// A fila de entrada do canal de dados: limitada, com política de descarte, fechável, e com uma
/// **vaga de espiada** para o padrão `(buf, cap)` da fronteira C.
///
/// # Por que não é mais um `mpsc::sync_channel`
///
/// O `sync_channel` só sabe descartar a que chega (o `try_send` falha), e para mensagens de estado
/// isso é o descarte errado. Também não sabe espiar sem tirar — e a fronteira C precisa perguntar o
/// tamanho de uma mensagem **sem** consumi-la, senão a chamada que só pergunta joga a mensagem fora.
///
/// Quem empurra são as threads da libdatachannel, então [`FilaDeEntrada::empurrar`] **nunca
/// bloqueia** além do cadeado: fila cheia é descarte, contado.
pub(crate) struct FilaDeEntrada {
    estado: Mutex<EstadoDaFila>,
    chegou: Condvar,
    politica: AtomicU8,
    /// Descartadas por fila cheia. É o `dropped_frames` de sempre.
    descartadas: Arc<AtomicU64>,
}

struct EstadoDaFila {
    itens: VecDeque<Vec<u8>>,
    /// Uma mensagem já tirada da fila e ainda não entregue: o que a fronteira C espiou e não coube
    /// no buffer da casca. É **da sessão**, não do handle, para que dois handles não disputem uma
    /// espiada ao meio.
    vaga: Option<Vec<u8>>,
    /// A sessão acabou. O que já estava na fila continua legível; depois dela, [`Tirada::Fechada`].
    fechada: bool,
}

/// O resultado de uma leitura da fila.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Tirada {
    Item(Vec<u8>),
    /// Nada chegou dentro do prazo.
    Nada,
    /// A sessão acabou e a fila esvaziou.
    Fechada,
}

impl FilaDeEntrada {
    fn nova(politica: QuandoEnche, descartadas: Arc<AtomicU64>) -> Self {
        FilaDeEntrada {
            estado: Mutex::new(EstadoDaFila {
                itens: VecDeque::with_capacity(8),
                vaga: None,
                fechada: false,
            }),
            chegou: Condvar::new(),
            politica: AtomicU8::new(politica as u8),
            descartadas,
        }
    }

    fn mudar_politica(&self, politica: QuandoEnche) {
        self.politica.store(politica as u8, Ordering::Relaxed);
    }

    fn politica(&self) -> QuandoEnche {
        if self.politica.load(Ordering::Relaxed) == QuandoEnche::DescartaAVelha as u8 {
            QuandoEnche::DescartaAVelha
        } else {
            QuandoEnche::DescartaANova
        }
    }

    /// Roda numa thread da libdatachannel, por mensagem. Não bloqueia além do cadeado.
    fn empurrar(&self, msg: Vec<u8>) {
        let Ok(mut e) = self.estado.lock() else {
            self.descartadas.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if e.fechada {
            return;
        }
        if e.itens.len() >= FILA_DE_DADOS {
            self.descartadas.fetch_add(1, Ordering::Relaxed);
            match self.politica() {
                QuandoEnche::DescartaANova => return,
                QuandoEnche::DescartaAVelha => {
                    e.itens.pop_front();
                }
            }
        }
        e.itens.push_back(msg);
        drop(e);
        self.chegou.notify_one();
    }

    /// Nada mais entra. O que estava na fila continua legível.
    fn fechar(&self) {
        if let Ok(mut e) = self.estado.lock() {
            e.fechada = true;
        }
        self.chegou.notify_all();
    }

    /// Espera até haver o que ler (na vaga ou na fila), até o fim do prazo. Com o cadeado na mão,
    /// devolve se há algo **na vaga** — é para lá que a próxima mensagem vai antes de ser entregue.
    fn preencher_vaga<'a>(
        &'a self,
        limite: Duration,
    ) -> std::result::Result<std::sync::MutexGuard<'a, EstadoDaFila>, Tirada> {
        let fim = Instant::now() + limite;
        let Ok(mut e) = self.estado.lock() else {
            return Err(Tirada::Fechada);
        };
        loop {
            if e.vaga.is_none() {
                e.vaga = e.itens.pop_front();
            }
            if e.vaga.is_some() {
                return Ok(e);
            }
            if e.fechada {
                return Err(Tirada::Fechada);
            }
            let agora = Instant::now();
            if agora >= fim {
                return Err(Tirada::Nada);
            }
            e = match self.chegou.wait_timeout(e, fim - agora) {
                Ok((guarda, _)) => guarda,
                Err(_) => return Err(Tirada::Fechada),
            };
        }
    }

    /// Tira a próxima, esperando até `limite`. `Duration::ZERO` não espera.
    pub(crate) fn tirar(&self, limite: Duration) -> Tirada {
        match self.preencher_vaga(limite) {
            Ok(mut e) => match e.vaga.take() {
                Some(v) => Tirada::Item(v),
                None => Tirada::Nada,
            },
            Err(t) => t,
        }
    }

    /// Mostra a próxima mensagem **válida** a `aceitar`, e só a tira da fila se `aceitar` devolver
    /// `true`. É a espiada que o padrão `(buf, cap)` precisa: a chamada que só pergunta o tamanho
    /// não consome nada.
    ///
    /// A que `valida` recusa é descartada ali mesmo e contada em `ao_descartar`, e a espera
    /// continua até o fim do mesmo prazo — uma mensagem inválida não pode encurtar a espera de
    /// quem chama nem aparecer para ele como "nada".
    ///
    /// `aceitar` roda com o cadeado da fila na mão; tem de ser curto (copiar para o buffer da
    /// casca, e só).
    fn entregar_se(
        &self,
        limite: Duration,
        valida: impl Fn(&[u8]) -> bool,
        mut ao_descartar: impl FnMut(),
        aceitar: impl FnOnce(&[u8]) -> bool,
    ) -> Espiada {
        let fim = Instant::now() + limite;
        loop {
            let resta = fim.saturating_duration_since(Instant::now());
            let mut e = match self.preencher_vaga(resta) {
                Ok(e) => e,
                Err(Tirada::Fechada) => return Espiada::Fechada,
                Err(_) => return Espiada::Nada,
            };
            let Some(v) = e.vaga.as_ref() else {
                return Espiada::Nada;
            };
            if !valida(v) {
                e.vaga = None;
                drop(e);
                ao_descartar();
                continue;
            }
            let tamanho = v.len();
            let consumida = aceitar(v);
            if consumida {
                e.vaga = None;
            }
            return Espiada::Havia { tamanho, consumida };
        }
    }
}

/// O resultado de uma espiada na fila. Ver [`FilaDeEntrada::entregar_se`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Espiada {
    /// Havia uma mensagem de `tamanho` bytes; `consumida` diz se ela saiu da fila.
    Havia { tamanho: usize, consumida: bool },
    /// Nada chegou dentro do prazo.
    Nada,
    /// A sessão acabou e a fila esvaziou.
    Fechada,
}

/// Eventos de controle da sessão.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEvent {
    /// SDP local pronto para ir pela sinalização. `kind` é `offer` ou `answer`.
    LocalDescription {
        kind: String,
        sdp: String,
    },
    /// Candidato ICE local, trickle.
    LocalCandidate {
        candidate: String,
        mid: String,
    },
    /// Mudança de estado da conexão P2P.
    State(PeerState),
    /// O canal de dados abriu — a partir daqui dá para enviar.
    ChannelOpen,
    ChannelClosed,
    /// Erro relatado pelo libdatachannel.
    Failed(String),
}

/// Estado da conexão P2P, traduzido para não vazar o tipo da dependência.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl From<ConnectionState> for PeerState {
    fn from(e: ConnectionState) -> Self {
        match e {
            ConnectionState::New => PeerState::New,
            ConnectionState::Connecting => PeerState::Connecting,
            ConnectionState::Connected => PeerState::Connected,
            ConnectionState::Disconnected => PeerState::Disconnected,
            ConnectionState::Failed => PeerState::Failed,
            ConnectionState::Closed => PeerState::Closed,
        }
    }
}

/// Por onde a mídia desta sessão está indo, do jeito que o ICE decidiu.
///
/// # Por que isto existe, e por que vale sozinho
///
/// **Uma corrida "pelo cabo" pode fechar pela Wi-Fi e parecer sucesso.** Foi medido nesta
/// bancada em 2026-09-01, e não é hipótese: duas pontas do `quall-probe` na mesma máquina,
/// sinalização por `127.0.0.1:7951`, e o par escolhido pelo ICE foi
/// `192.168.1.131:51698 <-> 192.168.1.131:62493` — a mídia saiu pelo rádio. Sem este relato,
/// qualquer número atribuído a um enlace é atribuição de fé: quem mede não tem como distinguir
/// "medi a rede" de "medi o cabo".
///
/// Até hoje o dado existia só em Rust (`Session::selected_pair`, `local_address`,
/// `remote_address`) e só o `quall-probe` o lia. O pedido de `docs/receptor-ios.md:243` — e de
/// `docs/ipad-destravado.md:391` e `docs/bancada.md:2260` — é justamente este: que a casca
/// também saiba.
///
/// # Os quatro campos são `Option`, e o `None` é informação
///
/// Enquanto o ICE não fecha, não há par escolhido. Isso **não** é falha: é "ainda não". Por isso
/// os campos vêm nulos em vez de a leitura inteira virar erro — confundir "ainda não" com
/// "falhou" é a classe de instrumento que erra em silêncio, e esta casa já pagou por ela.
///
/// # O que ele **não** diz
///
/// Não diz por qual interface o socket saiu, nem se o enlace é cabo ou rádio: diz o endereço
/// que o ICE escolheu, e é de quem lê a tarefa de interpretar. `169.254.x` e `192.168.42.x` são
/// pistas de cabo, não provas — um `169.254.x` também é o que sobra quando o DHCP falha.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CaminhoDaMidia {
    /// Linha `a=candidate:` do lado local, como a libjuice a escreveu. Traz o `typ host`.
    pub local_candidate: Option<String>,
    /// A mesma coisa, do lado remoto.
    pub remote_candidate: Option<String>,
    /// `endereço:porta` local do par escolhido.
    pub local_address: Option<String>,
    /// `endereço:porta` remoto do par escolhido.
    pub remote_address: Option<String>,
}

impl CaminhoDaMidia {
    /// O ICE já escolheu um par?
    ///
    /// É a pergunta que separa "ainda não" de "não deu": com `false`, todos os campos são nulos
    /// **por não haver o que dizer ainda**.
    pub fn fechado(&self) -> bool {
        self.local_candidate.is_some() || self.local_address.is_some()
    }
}

/// Como o canal entrega os quadros.
///
/// **O padrão do WebRTC — confiável e ordenado — é o errado para o Quall, e a diferença está na
/// cauda, não na mediana.**
///
/// Medido em 2026-08-21, MacBook (cabo, `192.168.1.131`) → Galaxy A10s (Wi-Fi, `192.168.1.159`,
/// `armeabi-v7a`, Android 11), 900 quadros de 1224 bytes a 60/s, ida e volta pelo canal de dados:
///
/// | entrega | p50 | média | p95 | max | quadros perdidos |
/// |---|---|---|---|---|---|
/// | confiável e ordenada | 10,44 ms | 18,09 ms | 53,60 ms | 242,16 ms | 0 de 900 |
/// | não confiável e não ordenada | 9,23 ms | 11,31 ms | 23,36 ms | 73,53 ms | 0 de 900 |
///
/// A mediana quase não muda; o p95 cai pela metade e o pior caso cai a um terço. É o esperado:
/// com entrega confiável e ordenada o SCTP retransmite o que o Wi-Fi perdeu e **segura tudo o que
/// veio depois** até o buraco fechar — bloqueio de cabeça de fila. O preço não aparece no caso
/// típico, aparece exatamente quando a rede piora, que é quando o espelhamento precisa aguentar.
///
/// Uma medição anterior, no mesmo par de aparelhos e no mesmo dia, deu **p50 de 1741 ms e máximo
/// de 3545 ms** com entrega confiável — com zero perda, de novo. Não deu para reproduzir depois,
/// então não é o número típico e não entra na tabela. Mas é o mesmo mecanismo levado ao limite,
/// e mostra o tamanho do risco que a entrega confiável carrega quando o Wi-Fi piora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Sem retransmissão e sem ordenação. É o certo para mídia ao vivo.
    Realtime,
    /// Confiável e ordenada, como o WebRTC entrega por padrão. Serve para controle e metadados,
    /// e serve para medir o custo da escolha — não para vídeo.
    Reliable,
    /// **Confiável e sem ordem**: o SCTP retransmite o pedaço perdido, mas não segura uma mensagem
    /// atrás de outra. É o modo das sessões de teleprompter (`docs/contrato-teleprompter.md` §1).
    ///
    /// Confiável por causa do texto do roteiro: 128 KiB são ~110 pedaços SCTP, e sem retransmissão
    /// basta perder um para a mensagem inteira ir ao chão (com 1 % de perda por pedaço, ela chega
    /// inteira em ~33 % das tentativas). Sem ordem porque o estado é uma fusão por campo que não
    /// depende de ordem: com entrega ordenada, um "pausar" de 400 bytes esperaria atrás de um texto
    /// de 128 KiB em trânsito — o bloqueio de cabeça de fila da tabela acima.
    ///
    /// **Só quem oferece escolhe.** Quem responde adota o tipo que chega no `OPEN` do DCEP
    /// (`vendor/.../impl/datachannel.cpp:329-381`), então pôr isto no `TransportConfig` de quem
    /// conecta não muda nada no canal. Com este modo a fila de entrada passa a guardar a mensagem
    /// **mais nova** quando enche ([`QuandoEnche::DescartaAVelha`]), dos dois lados.
    ReliableUnordered,
}

impl Delivery {
    /// Retransmite o que se perde?
    pub fn confiavel(self) -> bool {
        !matches!(self, Delivery::Realtime)
    }
}

/// O que dá para ajustar no transporte. Note o que **não** está aqui: servidor ICE.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Faixa de portas UDP para os candidatos locais. `(0, 0)` deixa o sistema escolher.
    ///
    /// Fixar a faixa é o que permite abrir uma regra estreita no Firewall do Windows em vez de
    /// liberar o executável inteiro — e no Dell da bancada o firewall já provou que barra o que
    /// ninguém pediu para liberar.
    pub port_range: (u16, u16),
    /// MTU. `0` usa o padrão da libdatachannel.
    pub mtu: i32,
    /// **Prende os candidatos ICE a um endereço local só.** `None` (o padrão) reúne todas as
    /// interfaces, que é o comportamento de sempre.
    ///
    /// # Ligar isto DESISTE das outras interfaces
    ///
    /// Não é "prefira o cabo": é "só o cabo". Com um endereço aqui, a Wi-Fi deixa de ser
    /// candidata — não há corrida entre cabo e rádio, não há fallback automático, e um endereço
    /// errado não degrada para a rede: a sessão não fecha. É por isso que "pelo cabo" tem de ser
    /// **escolha do usuário** no produto, e não upgrade invisível.
    ///
    /// # Para que serve, e o que ainda não está provado
    ///
    /// O ICE deste projeto **não junta endereço link-local**: `libjuice/src/addr.c:84`
    /// (`addr_is_local`) devolve `true` para `169.254/16`, e `udp.c:501`, `:534`, `:572` e `:588`
    /// só copiam o endereço quando `!addr_is_local(sa)`. Isso deixou de ser leitura de fonte em
    /// 2026-09-01: nesta bancada, com três aparelhos iOS no cabo e quatro interfaces IPv4 na
    /// máquina, o `Candidate gathering done` da libjuice saiu com **um** candidato — o da Wi-Fi.
    /// As três `169.254.*` não viraram candidato, e `127.0.0.1` também não
    /// (`ENABLE_LOCALHOST_ADDRESS` é `OFF`). Ver `docs/bancada.md`.
    ///
    /// O mecanismo pelo qual este campo escaparia daquele filtro está lido em fonte e **não foi
    /// executado**: `udp_get_addrs` (`libjuice/src/udp.c:432`) devolve um único record quando o
    /// socket está bound num endereço específico e **retorna antes** de qualquer
    /// `addr_is_local`. É por desvio do filtro, não por permissão. A cadeia é
    /// `RtcConfig::bind_address` → `rtcConfiguration.bindAddress` → `icetransport.cpp:122` →
    /// `agent.c:230` → `udp.c:154`.
    ///
    /// **Medido em 01/09/2026, e destrava.** iPad em Modo Avião, 720 de 720 quadros e perda
    /// 0,000 %; depois três câmeras iOS simultâneas, uma por cabo, perda zero nas três. E do
    /// outro lado do mesmo dia: o A10s ancorado por USB foi de **190 trancos para 0** quando o
    /// receptor passou a prender — sem prender, com o telefone alcançável pelo cabo **e** pelo
    /// rádio, o ICE escolhia o rádio.
    ///
    /// Duas consequências que só aparecem com aparelho: **basta um lado prender** (o outro forma
    /// o par por *peer-reflexive*), e prender **seleciona** o caminho mesmo fora de
    /// `169.254/16` — não é só o destravamento do link-local.
    ///
    /// # A sinalização também é presa por aqui
    ///
    /// Ver [`TransportConfig::origem`]: o mesmo endereço vale para o socket TCP de sinalização.
    /// Prender só a mídia deixaria a sinalização sair por onde a rota mandasse — e com vários
    /// Android ancorados, cujas sub-redes podem coincidir, "por onde a rota mandar" é ambíguo.
    ///
    /// # O NUL interno mataria o processo, e por isso é validado
    ///
    /// `RtcConfig::bind_address` (`datachannel-0.16.1/src/config.rs:56`) faz
    /// `CString::new(...).unwrap()`. Com `panic = "abort"` no perfil de release desta árvore
    /// (`Cargo.toml:91`) um NUL no meio da string **não vira erro: derruba o processo**. Por isso
    /// [`TransportConfig::to_rtc`] valida antes e devolve [`Error::Invalid`] — e por isso ela
    /// devolve `Result`. Uma string vazia é recusada pelo mesmo motivo de higiene: é erro de quem
    /// chamou, e ela chegaria à libjuice como pedido sem sentido.
    pub bind_address: Option<String>,
    /// Modo de entrega do canal de mídia. Ver [`Delivery`].
    pub delivery: Delivery,
}

impl TransportConfig {
    /// O endereço local a prender, já validado, para quem precisa dele como `IpAddr`.
    ///
    /// Existe para que **a mídia e a sinalização leiam o mesmo campo pelo mesmo caminho**: duas
    /// validações separadas divergiriam, e a que divergisse silenciosamente seria a que sai por
    /// outra interface sem ninguém notar.
    ///
    /// `None` é "não prenda nada", que é o padrão. Texto que não é endereço IP é erro de quem
    /// chamou e vira [`Error::Invalid`] — nunca um "não prendi" silencioso.
    pub fn origem(&self) -> Result<Option<std::net::IpAddr>> {
        let Some(texto) = self.bind_address.as_deref() else {
            return Ok(None);
        };
        texto.parse::<std::net::IpAddr>().map(Some).map_err(|_| {
            Error::Invalid(format!(
                "bind_address não é um endereço IP: {texto:?}. Espera-se o endereço local da                  interface, como \"169.254.75.173\", sem porta."
            ))
        })
    }
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            port_range: (0, 0),
            mtu: 0,
            // `None`: reúne todas as interfaces. Prender a uma é escolha explícita, e cara —
            // ver o campo.
            bind_address: None,
            // Tempo real por padrão: o produto é espelhamento ao vivo, não transferência de
            // arquivo. Quem quiser entrega garantida pede explicitamente.
            delivery: Delivery::Realtime,
        }
    }
}

impl TransportConfig {
    /// Traduz para a configuração da libdatachannel.
    ///
    /// **Devolve `Result` por causa de um `unwrap` da dependência.**
    /// `RtcConfig::bind_address` faz `CString::new(...).unwrap()`
    /// (`datachannel-0.16.1/src/config.rs:56`), e esta árvore compila release com
    /// `panic = "abort"` (`Cargo.toml:91`): um NUL no meio do endereço **não devolveria erro,
    /// derrubaria o processo**. A validação tem de acontecer antes de a string atravessar, e
    /// "antes" é aqui.
    fn to_rtc(&self) -> Result<RtcConfig> {
        // Lista de servidores ICE vazia: sem STUN, sem TURN. Só candidatos `host`.
        let servidores: [&str; 0] = [];
        let mut cfg = RtcConfig::new(&servidores);
        if self.port_range != (0, 0) {
            cfg = cfg
                .port_range_begin(self.port_range.0)
                .port_range_end(self.port_range.1);
        }
        if self.mtu != 0 {
            cfg = cfg.mtu(self.mtu);
        }
        if let Some(endereco) = &self.bind_address {
            // O NUL primeiro: é o que mata o processo, e é o único jeito de a validação
            // acontecer **antes** do `unwrap` da dependência.
            if endereco.as_bytes().contains(&0) {
                return Err(Error::Invalid(format!(
                    "bind_address com NUL no meio ({endereco:?}): a libdatachannel faria \
                     `CString::new(...).unwrap()` e, com `panic = \"abort\"`, isso derruba o \
                     processo em vez de devolver erro"
                )));
            }
            if endereco.is_empty() {
                return Err(Error::Invalid(
                    "bind_address vazio: para reunir todas as interfaces, use `None`".into(),
                ));
            }
            cfg = cfg.bind_address(endereco);
        }
        Ok(cfg)
    }

    fn to_dc_init(&self) -> DataChannelInit {
        let confiabilidade = match self.delivery {
            // `unreliable` + `max_retransmits(0)` = entrega parcial sem retransmissão nenhuma;
            // `unordered` tira a ordenação, que é o que causa o bloqueio de cabeça de fila.
            Delivery::Realtime => Reliability::default().unordered().unreliable(),
            Delivery::Reliable => Reliability::default(),
            // Sem `unreliable`: retransmite até chegar. Com `unordered`: não espera a anterior.
            Delivery::ReliableUnordered => Reliability::default().unordered(),
        };
        DataChannelInit::default()
            .reliability(confiabilidade)
            // O subprotocolo identifica o que trafega no canal. Quando houver mais de um canal
            // por sessão (tela, câmera, microfone), é por aqui que o receptor distingue.
            .protocol("quall/media/v1")
    }
}

/// **Quando o último byte do outro lado chegou.** É o único relógio de vida do caminho da mídia
/// que este núcleo tem.
///
/// Existe porque o estado do ICE não serve de detector rápido: medido em
/// `docs/receptor-ios.md:243`, com a mídia morta aos 8,6 s o detector de queda foi chamado a
/// 20 Hz por **mais de dez segundos** sem nunca devolver `Disconnected` nem `Failed` — o
/// `CONSENT_TIMEOUT` da libjuice é 30 000 ms, e até lá a sessão se diz saudável enquanto a tela
/// está congelada.
///
/// É batido em dois lugares, que são os dois por onde o par nos alcança: a mensagem do canal de
/// dados ([`DcHandler::on_message`]) e o pacote RTP de uma track recebida
/// ([`crate::track::TrackReceptor`]). Custa uma escrita atômica relaxada por pacote.
///
/// **Nunca batido é estado normal, não defeito**: um emissor de produto não recebe nada — ele
/// só manda. Ver [`Batimento::silencio`].
pub(crate) struct Batimento {
    relogio: crate::media::Clock,
    /// Micros do [`Batimento::relogio`] **mais um**, para que `0` signifique "nunca bateu" sem
    /// competir com o microssegundo zero.
    ultimo_us: AtomicU64,
}

impl Batimento {
    pub(crate) fn novo() -> Self {
        Batimento {
            relogio: crate::media::Clock::new(),
            ultimo_us: AtomicU64::new(0),
        }
    }

    /// Chegou alguma coisa do outro lado. Roda em thread da libdatachannel, por pacote.
    pub(crate) fn bater(&self) {
        self.ultimo_us
            .store(self.relogio.micros() + 1, Ordering::Relaxed);
    }

    /// Há quanto tempo nada chega. `None` quer dizer **nada chegou ainda nesta sessão**, e é o
    /// caso de todo emissor: quem nunca recebeu não tem silêncio a medir.
    pub(crate) fn silencio(&self) -> Option<Duration> {
        let marca = self.ultimo_us.load(Ordering::Relaxed);
        if marca == 0 {
            return None;
        }
        Some(Duration::from_micros(
            self.relogio.micros().saturating_sub(marca - 1),
        ))
    }
}

/// Canal compartilhado entre o handler (threads da libdatachannel) e quem envia.
type CanalCompartilhado = Arc<Mutex<Option<Box<RtcDataChannel<DcHandler>>>>>;

/// Em que pé está o canal de dados, para quem manda mensagem saber **sem tocar na API C**.
///
/// Mandar num canal fechado faz `DataChannel::outgoing` lançar (`datachannel.cpp:187-188`). A
/// exceção nasce numa função membro C++ e é apanhada pelo `wrap` do `capi.cpp`, fora do cadeado
/// global — não é o caso que trava o Windows —, mas não chamar é melhor que chamar e confiar.
const CANAL_NUNCA_ABRIU: u8 = 0;
const CANAL_ABERTO: u8 = 1;
const CANAL_FECHOU: u8 = 2;

struct DcHandler {
    ctrl: Sender<TransportEvent>,
    fila: Arc<FilaDeEntrada>,
    estado_do_canal: Arc<AtomicU8>,
    batimento: Arc<Batimento>,
}

impl DataChannelHandler for DcHandler {
    fn on_open(&mut self) {
        self.estado_do_canal.store(CANAL_ABERTO, Ordering::Release);
        let _ = self.ctrl.send(TransportEvent::ChannelOpen);
    }

    fn on_closed(&mut self) {
        self.estado_do_canal.store(CANAL_FECHOU, Ordering::Release);
        let _ = self.ctrl.send(TransportEvent::ChannelClosed);
    }

    fn on_error(&mut self, err: &str) {
        let _ = self.ctrl.send(TransportEvent::Failed(err.to_string()));
    }

    fn on_message(&mut self, msg: &[u8]) {
        // O batimento **antes** de enfileirar: o que ele mede é "o par nos alcançou", e um
        // quadro descartado por fila cheia é prova de vida igual a um quadro entregue. Contá-lo
        // só na entrega faria o detector de silêncio disparar num receptor lento.
        self.batimento.bater();
        // Esta função roda numa thread da libdatachannel. Bloquear aqui trava a recepção
        // inteira, então a fila nunca espera: cheia, descarta (a nova ou a velha, conforme o
        // canal — ver `QuandoEnche`) e conta.
        self.fila.empurrar(msg.to_vec());
    }
}

struct PcHandler {
    ctrl: Sender<TransportEvent>,
    fila: Arc<FilaDeEntrada>,
    estado_do_canal: Arc<AtomicU8>,
    canal: CanalCompartilhado,
    batimento: Arc<Batimento>,
}

impl PeerConnectionHandler for PcHandler {
    type DCH = DcHandler;

    fn data_channel_handler(&mut self, _info: DataChannelInfo) -> Self::DCH {
        DcHandler {
            ctrl: self.ctrl.clone(),
            fila: Arc::clone(&self.fila),
            estado_do_canal: Arc::clone(&self.estado_do_canal),
            batimento: Arc::clone(&self.batimento),
        }
    }

    fn on_description(&mut self, desc: SessionDescription) {
        let kind = match desc.sdp_type {
            SdpType::Offer => "offer",
            SdpType::Answer => "answer",
            SdpType::Pranswer => "pranswer",
            SdpType::Rollback => "rollback",
        };
        let _ = self.ctrl.send(TransportEvent::LocalDescription {
            kind: kind.to_string(),
            sdp: desc.sdp.to_string(),
        });
    }

    fn on_candidate(&mut self, cand: IceCandidate) {
        let _ = self.ctrl.send(TransportEvent::LocalCandidate {
            candidate: cand.candidate,
            mid: cand.mid,
        });
    }

    fn on_connection_state_change(&mut self, state: ConnectionState) {
        let _ = self.ctrl.send(TransportEvent::State(state.into()));
    }

    fn on_data_channel(&mut self, canal: Box<RtcDataChannel<Self::DCH>>) {
        // Quem responde **adota** a confiabilidade que chegou no `OPEN` (`datachannel.cpp:354-366`)
        // e a fila segue o que o canal é: confiável e sem ordem guarda a mais nova. Lido aqui, e
        // não do `TransportConfig` deste lado, porque o deste lado não decide nada no canal.
        let r = canal.reliability();
        if r.unordered && !r.unreliable {
            self.fila.mudar_politica(QuandoEnche::DescartaAVelha);
        }
        // Lado que responde: o canal chega pronto por aqui. Guardar é o que permite responder
        // pelo mesmo canal em vez de abrir um segundo.
        if let Ok(mut guarda) = self.canal.lock() {
            *guarda = Some(canal);
        }
    }
}

/// Uma sessão P2P.
///
/// `offerer` cria o canal de dados e as tracks e emite a oferta. `answerer` espera a oferta
/// chegar e recolhe as tracks que vierem nela.
pub struct Session {
    pc: Box<RtcPeerConnection<PcHandler>>,
    /// O id inteiro da conexão, do jeito que a API C de mídia o exige.
    pc_id: quall_rtc::PcId,
    canal: CanalCompartilhado,
    ctrl_rx: Receiver<TransportEvent>,
    /// A fila de entrada do canal de dados. Compartilhada com todo [`Mensageiro`] desta sessão,
    /// que pode sobreviver a ela.
    fila: Arc<FilaDeEntrada>,
    descartados: Arc<AtomicU64>,
    /// Ver [`CANAL_NUNCA_ABRIU`].
    estado_do_canal: Arc<AtomicU8>,
    /// Os contadores das mensagens, compartilhados com os [`Mensageiro`]s.
    contadores: Arc<ContadoresInternos>,
    /// Número desta sessão no processo. Ver [`Mensageiro::sessao`].
    id: u64,
    /// O teto de mensagem que o outro lado anunciou, lido do SDP dele. Ver [`teto_do_sdp`].
    teto_negociado: Arc<std::sync::atomic::AtomicUsize>,
    /// Tracks que chegaram do outro lado, esperando quem as pegue.
    tracks_rx: Receiver<TrackReceptor>,
    /// Toda track que passou por esta sessão, para limpar o registro do `quall-rtc` no `Drop`.
    tracks: Arc<Mutex<Vec<quall_rtc::Track>>>,
    /// Tracks que chegaram com um `mid` que o Quall não conhece.
    tracks_recusadas: Arc<AtomicU64>,
    /// Portão compartilhado com toda track desta sessão. Ver [`crate::portao`] — e o `Drop`
    /// aqui embaixo, que o fecha antes de destruir qualquer coisa.
    portao: Arc<Portao>,
    /// Quantas tracks **desta** sessão o `Drop` conseguiu apagar dos mapas globais da
    /// libdatachannel. Por sessão, e não global, porque os testes rodam em paralelo e um contador
    /// de processo misturaria as contas de uns com as de outros.
    fechadas: Arc<AtomicU64>,
    /// Quando o outro lado nos alcançou pela última vez. Ver [`Batimento`].
    batimento: Arc<Batimento>,
    /// **Quem é o outro lado**, quando a sessão veio de `session::hospedar`/`conectar` (que já o
    /// sabem pelo aperto de mão e pelo pareamento). `None` numa sessão do transporte montada à mão.
    /// Ver [`Mensageiro::par`].
    par: Option<Arc<ParDaSessao>>,
}

/// **Quem é o outro lado de uma sessão**: o `device_id` e o nome que ele anunciou no aperto de mão
/// (`Hello`/`Welcome`), que passou pelo pareamento — que é por `device_id`.
///
/// É o que a réplica do teleprompter usa para saber se o prompter desta sessão é o da última vez
/// (`docs/contrato-teleprompter.md` §11.2). Vem da sessão, e não do fio, porque o controle decide
/// antes da primeira mensagem sair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParDaSessao {
    pub id: String,
    pub nome: String,
}

impl Session {
    fn montar(
        cfg: &TransportConfig,
        criar_canal: bool,
        tracks_a_criar: &[TrackConfig],
    ) -> Result<(Self, Vec<TrackEmissor>)> {
        let (ctrl_tx, ctrl_rx) = mpsc::channel();
        let (tracks_tx, tracks_rx) = mpsc::channel();
        let descartados = Arc::new(AtomicU64::new(0));
        // Quem oferece sabe já o que o canal é. Quem responde começa pela regra de sempre e troca
        // em `on_data_channel`, quando descobre o que o outro lado escolheu.
        let politica = if criar_canal && cfg.delivery == Delivery::ReliableUnordered {
            QuandoEnche::DescartaAVelha
        } else {
            QuandoEnche::DescartaANova
        };
        let fila = Arc::new(FilaDeEntrada::nova(politica, Arc::clone(&descartados)));
        let estado_do_canal = Arc::new(AtomicU8::new(CANAL_NUNCA_ABRIU));
        let contadores = Arc::new(ContadoresInternos::default());
        let canal: CanalCompartilhado = Arc::new(Mutex::new(None));
        let tracks: Arc<Mutex<Vec<quall_rtc::Track>>> = Arc::new(Mutex::new(Vec::new()));
        let tracks_recusadas = Arc::new(AtomicU64::new(0));
        let portao = Arc::new(Portao::novo());
        let fechadas = Arc::new(AtomicU64::new(0));
        let batimento = Arc::new(Batimento::novo());

        let handler = PcHandler {
            ctrl: ctrl_tx.clone(),
            fila: Arc::clone(&fila),
            estado_do_canal: Arc::clone(&estado_do_canal),
            canal: Arc::clone(&canal),
            batimento: Arc::clone(&batimento),
        };

        // Negociação automática desligada nos dois lados. Com ela ligada, a libdatachannel emite
        // a descrição local assim que o **primeiro** canal ou track aparece — e tudo o que for
        // criado depois disso fica de fora da oferta, precisando de renegociação. Como uma
        // sessão do Quall nasce com um canal de dados e até três tracks, isso seria uma corrida
        // perdida de saída. Desligada, quem decide a hora é este código.
        // `to_rtc()?` antes de a conexão nascer: um `bind_address` inválido vira `Error::Invalid`
        // sem que nada tenha sido criado do lado do C++.
        let rtc = cfg.to_rtc()?.disable_auto_negotiation();
        let mut pc = RtcPeerConnection::new(&rtc, handler)
            .map_err(|e| Error::Transport(format!("não criou a conexão: {e}")))?;

        let pc_id = quall_rtc::id_da_conexao(&pc).map_err(|e| Error::Transport(e.0))?;

        // O tratador de track precisa estar registrado **antes** de qualquer descrição remota
        // ser aplicada: a libdatachannel dispara o callback enquanto processa o SDP da oferta, e
        // um tratador tardio perde a track em silêncio.
        {
            let tracks_tx = tracks_tx.clone();
            let conhecidas = Arc::clone(&tracks);
            let recusadas = Arc::clone(&tracks_recusadas);
            let portao_da_track = Arc::clone(&portao);
            let batimento_da_track = Arc::clone(&batimento);
            // Um relógio comum por sessão, compartilhado pelas tracks que chegam: é ele que liga
            // o `timestamp_us` de todas a uma época só (`crate::relogio`).
            let relogio_da_sessao = crate::relogio::RelogioDaSessao::novo();
            quall_rtc::ao_chegar_track(pc_id, move |track| {
                if let Ok(mut guarda) = conhecidas.lock() {
                    guarda.push(track);
                }
                match TrackReceptor::adotar(
                    track,
                    Arc::clone(&portao_da_track),
                    Arc::clone(&batimento_da_track),
                    Arc::clone(&relogio_da_sessao),
                ) {
                    Ok(receptor) => {
                        let _ = tracks_tx.send(receptor);
                    }
                    Err(_) => {
                        // Track de `mid` desconhecido: outra versão do Quall, ou outra coisa.
                        // Não é motivo para derrubar a sessão — as tracks que entendemos seguem.
                        recusadas.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
            .map_err(|e| Error::Transport(e.0))?;
        }

        if criar_canal {
            let dc = pc
                .create_data_channel_ex(
                    CHANNEL_LABEL,
                    DcHandler {
                        ctrl: ctrl_tx,
                        fila: Arc::clone(&fila),
                        estado_do_canal: Arc::clone(&estado_do_canal),
                        batimento: Arc::clone(&batimento),
                    },
                    &cfg.to_dc_init(),
                )
                .map_err(|e| Error::Transport(format!("não criou o canal de dados: {e}")))?;
            if let Ok(mut guarda) = canal.lock() {
                *guarda = Some(dc);
            }
        }

        let mut emissores = Vec::with_capacity(tracks_a_criar.len());
        for cfg_track in tracks_a_criar {
            let emissor = TrackEmissor::abrir(pc_id, cfg_track, Arc::clone(&portao))?;
            if let Ok(mut guarda) = tracks.lock() {
                guarda.push(emissor.track_crua());
            }
            emissores.push(emissor);
        }

        let sessao = Session {
            pc,
            pc_id,
            canal,
            ctrl_rx,
            fila,
            descartados,
            estado_do_canal,
            contadores,
            id: PROXIMA_SESSAO.fetch_add(1, Ordering::Relaxed),
            teto_negociado: Arc::new(std::sync::atomic::AtomicUsize::new(TETO_DA_MENSAGEM)),
            tracks_rx,
            tracks,
            tracks_recusadas,
            portao,
            fechadas,
            batimento,
            par: None,
        };
        Ok((sessao, emissores))
    }

    /// Diz à sessão quem é o outro lado. Só `session::hospedar`/`conectar` chamam, depois do
    /// aperto de mão; todo [`Mensageiro`] pedido depois disso o carrega.
    pub(crate) fn definir_par(&mut self, par: ParDaSessao) {
        self.par = Some(Arc::new(par));
    }

    /// Lado que oferece: cria o canal de dados, e a oferta sai como evento de controle.
    pub fn offerer(cfg: &TransportConfig) -> Result<Self> {
        Session::offerer_com_tracks(cfg, &[]).map(|(s, _)| s)
    }

    /// Lado que oferece, com tracks de mídia.
    ///
    /// As tracks **precisam** ser declaradas aqui, e não depois: elas entram na oferta, e o que
    /// não estava na oferta só entra com renegociação — que o Quall não implementa. Um emissor
    /// que ainda não sabe se vai mandar câmera declara a track mesmo assim e a deixa muda; uma
    /// track sem quadro custa uma linha `m=` no SDP e nada mais.
    pub fn offerer_com_tracks(
        cfg: &TransportConfig,
        tracks: &[TrackConfig],
    ) -> Result<(Self, Vec<TrackEmissor>)> {
        let (mut sessao, emissores) = Session::montar(cfg, true, tracks)?;
        // Com a negociação automática desligada, é esta chamada que emite a oferta — depois de
        // o canal de dados e todas as tracks já existirem.
        sessao
            .pc
            .set_local_description(SdpType::Offer)
            .map_err(|e| Error::Transport(format!("não emitiu a oferta: {e}")))?;
        Ok((sessao, emissores))
    }

    /// Lado que responde: espera a oferta.
    pub fn answerer(cfg: &TransportConfig) -> Result<Self> {
        Session::montar(cfg, false, &[]).map(|(s, _)| s)
    }

    /// **Fecha o portão da sessão e espera os tratadores da casca saírem.**
    ///
    /// É a barreira que faltava ao `quall_session_close`. Quando devolve
    /// [`Barreira::Cumprida`], nenhuma thread está dentro do código da casca e nenhuma vai
    /// entrar: a casca pode liberar o que passou como `user_data`.
    ///
    /// O `Drop` chama isto de qualquer jeito — mas o `Drop` não tem para quem devolver o
    /// resultado, e é o resultado que a fronteira C precisa repassar. Chamar antes de largar a
    /// sessão é de graça: o `Drop` encontra o portão já fechado e vazio, e volta na hora.
    pub fn fechar_portao(&self, prazo: Duration) -> Barreira {
        self.portao.fechar_com_prazo(prazo)
    }

    /// Próxima track que chegou do outro lado, esperando até `limite`.
    ///
    /// O receptor chama num laço depois de a sessão fechar. Uma sessão traz tela, câmera e
    /// microfone; quem decide como compor é ele.
    pub fn proxima_track(&self, limite: Duration) -> Option<TrackReceptor> {
        self.tracks_rx.recv_timeout(limite).ok()
    }

    /// Tracks que chegaram com um `mid` que este núcleo não reconhece.
    pub fn tracks_recusadas(&self) -> u64 {
        self.tracks_recusadas.load(Ordering::Relaxed)
    }

    /// O contador de tracks que o `Drop` desta sessão apagou dos mapas globais da libdatachannel.
    ///
    /// Devolve o `Arc` de propósito: quem quiser conferir precisa segurá-lo **antes** de a sessão
    /// morrer, porque é o `Drop` dela que o preenche. É o instrumento das dívidas 4, 14 e 21.
    ///
    /// Só nos testes. O produto não tem o que fazer com este número, e a alternativa — perguntar
    /// à libdatachannel se o id ainda existe — é a armadilha que trava o Windows.
    #[cfg(test)]
    pub(crate) fn contador_de_fechadas(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.fechadas)
    }

    /// Aplica o SDP que veio pela sinalização.
    pub fn set_remote_description(&mut self, kind: &str, sdp: &str) -> Result<()> {
        let sdp_type = match kind {
            "offer" => SdpType::Offer,
            "answer" => SdpType::Answer,
            "pranswer" => SdpType::Pranswer,
            "rollback" => SdpType::Rollback,
            outro => {
                return Err(Error::Protocol(format!(
                    "tipo de SDP desconhecido: {outro}"
                )))
            }
        };
        let era_oferta = matches!(sdp_type, SdpType::Offer);
        self.teto_negociado
            .store(teto_do_sdp(sdp), Ordering::Relaxed);
        let parsed = datachannel::sdp::parse_sdp(sdp, false)
            .map_err(|e| Error::Protocol(format!("SDP inválido: {e}")))?;
        self.pc
            .set_remote_description(&SessionDescription {
                sdp: parsed,
                sdp_type,
            })
            .map_err(|e| Error::Transport(format!("não aplicou o SDP remoto: {e}")))?;

        // Com a negociação automática desligada (ver [`Session::montar`]), a resposta não sai
        // sozinha ao aplicar a oferta: é preciso pedir. Aqui, e não em quem chama, porque é o
        // único lugar que sabe que acabou de chegar uma oferta — e esquecer isso na casca
        // deixaria a sessão pendurada até o prazo estourar, sem erro nenhum no caminho.
        if era_oferta {
            self.pc
                .set_local_description(SdpType::Answer)
                .map_err(|e| Error::Transport(format!("não emitiu a resposta: {e}")))?;
        }
        Ok(())
    }

    /// Acrescenta um candidato ICE que chegou pela sinalização.
    pub fn add_remote_candidate(&mut self, candidate: &str, mid: &str) -> Result<()> {
        self.pc
            .add_remote_candidate(&IceCandidate {
                candidate: candidate.to_string(),
                mid: mid.to_string(),
            })
            .map_err(|e| Error::Transport(format!("candidato recusado: {e}")))
    }

    /// Envia bytes pelo canal de dados.
    ///
    /// Erro se o canal ainda não abriu — que é estado normal enquanto o ICE não fechou, não
    /// defeito. Acima de [`TETO_DA_MENSAGEM`] é [`Error::Invalid`] **antes** de chegar à
    /// biblioteca, que lançaria exceção.
    pub fn send(&self, bytes: &[u8]) -> Result<()> {
        let teto = self.teto_negociado.load(Ordering::Relaxed);
        if bytes.len() > teto {
            return Err(Error::Invalid(format!(
                "mensagem de {} bytes passa do teto do canal ({teto})",
                bytes.len()
            )));
        }
        self.send_sem_teto(bytes)
    }

    /// O envio cru, sem conferir o teto. Só os testes chamam direto: é como se mede o teto da
    /// **biblioteca**, e não o nosso.
    fn send_sem_teto(&self, bytes: &[u8]) -> Result<()> {
        let mut guarda = self
            .canal
            .lock()
            .map_err(|_| Error::Transport("canal de dados envenenado por um panic".into()))?;
        let canal = guarda
            .as_mut()
            .ok_or_else(|| Error::Transport("o canal de dados ainda não abriu".into()))?;
        canal
            .send(bytes)
            .map_err(|e| Error::Transport(format!("falha ao enviar: {e}")))
    }

    /// Quantos bytes estão esperando para sair.
    ///
    /// É o instrumento para saber se o emissor está gerando mais do que a rede leva. Vale mais
    /// que a latência sozinha: com o buffer subindo, a latência medida ainda é boa e a
    /// experiência já não é.
    pub fn buffered_amount(&self) -> usize {
        self.canal
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.buffered_amount()))
            .unwrap_or(0)
    }

    /// Próximo evento de controle, esperando até `limite`.
    ///
    /// `None` cobre tanto "nada chegou a tempo" quanto "os emissores foram embora": para quem
    /// chama, os dois casos levam à mesma decisão — tentar de novo até o prazo da sessão.
    ///
    /// `Duration::ZERO` quer dizer **não bloqueie**, e é `try_recv` de verdade. Pelo mesmo motivo
    /// de [`crate::signaling::Link::poll_por`]: `recv_timeout(1ms)` no Windows acorda no tique do
    /// temporizador do sistema (~15,6 ms), e essa era a **segunda** metade dos 29,43 ms que
    /// `proximo_evento(Duration::ZERO)` custava no emissor do Windows — a primeira era o socket
    /// da sinalização.
    pub fn next_event(&self, limite: Duration) -> Option<TransportEvent> {
        if limite.is_zero() {
            return self.ctrl_rx.try_recv().ok();
        }
        self.ctrl_rx.recv_timeout(limite).ok()
    }

    /// Próximo quadro recebido, esperando até `limite`.
    ///
    /// `Duration::ZERO` não bloqueia. Ver [`Session::next_event`].
    ///
    /// Os bytes crus, como chegaram — é a porta do `quall-probe`. Quem troca mensagens de
    /// aplicação usa o [`Mensageiro`], que confere o formato; os dois leem **a mesma fila**, então
    /// numa sessão só um dos dois deve ler.
    pub fn next_data(&self, limite: Duration) -> Option<Vec<u8>> {
        match self.fila.tirar(limite) {
            Tirada::Item(v) => Some(v),
            Tirada::Nada | Tirada::Fechada => None,
        }
    }

    /// Quadros descartados por fila cheia desde o início da sessão.
    pub fn dropped_frames(&self) -> u64 {
        self.descartados.load(Ordering::Relaxed)
    }

    /// **O mensageiro da sessão**: mandar e receber mensagens de aplicação pelo canal de dados.
    ///
    /// Pode ser chamado mais de uma vez; todos os mensageiros de uma sessão dividem a mesma fila,
    /// a mesma vaga de espiada e os mesmos contadores. **Sobrevive à sessão**: depois do `Drop`
    /// dela, mandar devolve [`Error::Closed`] sem tocar na API C, e ler entrega o que já tinha
    /// chegado e depois [`Error::Closed`]. Ver [`Mensageiro`].
    pub fn mensageiro(&self) -> Mensageiro {
        Mensageiro {
            sessao: self.id,
            canal: Arc::downgrade(&self.canal),
            portao: Arc::clone(&self.portao),
            fila: Arc::clone(&self.fila),
            estado_do_canal: Arc::clone(&self.estado_do_canal),
            contadores: Arc::clone(&self.contadores),
            descartadas_fila_cheia: Arc::clone(&self.descartados),
            teto: Arc::clone(&self.teto_negociado),
            par: self.par.clone(),
        }
    }

    /// **A entrega que o canal tem de fato**, lida do canal — e não do `TransportConfig` deste
    /// lado, que em quem responde não decide nada. `None` enquanto o canal não existe deste lado,
    /// ou se ele estiver num modo que o Quall não usa (ordenado e sem retransmissão).
    pub fn entrega_do_canal(&self) -> Option<Delivery> {
        let guarda = self.canal.lock().ok()?;
        let r = guarda.as_ref()?.reliability();
        match (r.unordered, r.unreliable) {
            (true, true) => Some(Delivery::Realtime),
            (false, false) => Some(Delivery::Reliable),
            (true, false) => Some(Delivery::ReliableUnordered),
            (false, true) => None,
        }
    }

    /// **Há quanto tempo nada chega do outro lado.** `None` quer dizer que nada chegou ainda —
    /// e é o estado normal de quem só emite.
    ///
    /// Conta mensagem do canal de dados e pacote RTP de track recebida, que são os dois
    /// caminhos por onde o par nos alcança. Não conta RTCP: a sessão de RTCP é consumida dentro
    /// da libdatachannel e não passa por aqui.
    ///
    /// É a matéria-prima do detector de queda de caminho de
    /// [`crate::session::SessionConfig::silencio_do_caminho`]; ver lá por que ele não é ligado
    /// por conta própria.
    pub fn silencio_da_midia(&self) -> Option<Duration> {
        self.batimento.silencio()
    }

    /// Par de candidatos que o ICE escolheu — a prova de que o caminho é direto.
    ///
    /// Com a lista de servidores ICE vazia, os dois lados são `typ host`. Se algum dia aparecer
    /// `srflx` ou `relay` aqui, alguém reabriu a decisão de LAN-only sem avisar.
    pub fn selected_pair(&self) -> Option<(String, String)> {
        self.pc
            .selected_candidate_pair()
            .map(|p| (p.local, p.remote))
    }

    pub fn local_address(&self) -> Option<String> {
        self.pc.local_address()
    }

    pub fn remote_address(&self) -> Option<String> {
        self.pc.remote_address()
    }

    /// Por onde a mídia está indo. Ver [`CaminhoDaMidia`].
    ///
    /// Uma leitura só, e é de propósito: os quatro campos vêm da mesma pergunta à
    /// libdatachannel, e montá-los em chamadas separadas deixaria a casca compor um relato de
    /// dois instantes diferentes.
    pub fn caminho(&self) -> CaminhoDaMidia {
        let (local_candidate, remote_candidate) = match self.selected_pair() {
            Some((l, r)) => (Some(l), Some(r)),
            None => (None, None),
        };
        CaminhoDaMidia {
            local_candidate,
            remote_candidate,
            local_address: self.local_address(),
            remote_address: self.remote_address(),
        }
    }
}

/// Destrói as tracks desta sessão e tira do registro do `quall-rtc` tudo o que ela pôs lá.
///
/// # Por que `fechar()` e não `esquecer()` (dívidas 4, 14 e 21)
///
/// A versão anterior chamava `esquecer()`, que só limpa o registro **do Rust**: `rtcDeleteTrack`
/// nunca era chamado. A suposição por trás era "as tracks morrem com a conexão". Ela é falsa na
/// libdatachannel: `erasePeerConnection` apaga só `peerConnectionMap` e `userPointerMap`, e a
/// track fica presa nos mapas globais do processo. Não é vazamento transitório: é permanente
/// enquanto o processo viver, e no Android o processo sobrevive a dezenas de sessões.
///
/// E é cobrado **por tentativa**, não por sessão: [`Session::offerer_com_tracks`] nasce dentro do
/// laço de tentativas de [`crate::session::hospedar`], depois do pareamento. Numa rede em que o
/// pareamento fecha e o ICE não — isolamento de AP, Wi-Fi de hóspede, permissão de Rede Local
/// negada —, cada tentativa deixava uma track para trás.
///
/// # A ordem importa, e no Windows ela é a diferença entre limpar e travar
///
/// **A primeira versão deste conserto fechava as tracks com a conexão ainda viva**, apoiada em
/// "o corpo do `Drop` roda antes dos campos, então `self.pc` ainda está vivo". Estava correta
/// sobre o Rust e errada sobre a libdatachannel. Medido no Dell G3 em 2026-08-23:
///
/// | `Drop` faz | 5 sessões criadas e destruídas, e depois **mais uma** |
/// |---|---|
/// | `esquecer()` (vaza) | termina em 0,03 s |
/// | `fechar()` com a conexão viva | **`rtcCreatePeerConnection` bloqueia para sempre** |
/// | `fechar()` depois de destruir a conexão | termina normalmente |
///
/// O mecanismo, lido em `libdatachannel/src/impl/init.cpp`: só a `PeerConnection` segura um
/// `init_token` — a `Track` não. Quando a última conexão morre, `~TokenPayload` **destaca uma
/// thread** que chama `doCleanup()`, que toma o `Init::mMutex` e faz `ThreadPool::join()`. Toda
/// criação de conexão passa por `Init::token()`, que quer o **mesmo mutex**. Se o `join()` ficar
/// preso, a próxima sessão do processo nunca nasce.
///
/// Fechar uma track de uma conexão viva deixa trabalho na fila do pool que o `join()` seguinte
/// espera para sempre. Destruir a conexão primeiro tira esse trabalho do caminho: as tracks já
/// estão mortas por dentro, e o `rtcDeleteTrack` que vem depois só apaga a entrada do mapa
/// global — que é tudo o que as dívidas 4, 14 e 21 pediam.
///
/// **Isso não é preferência de estilo, é a diferença entre um vazamento e um travamento.** No
/// receptor de desktop — o plugin de OBS, a câmera virtual — o processo fica horas aberto
/// reconectando; travar na segunda sessão é pior que vazar na centésima.
///
/// Depois daqui, um [`crate::track::TrackEmissor`] que o chamador tenha guardado passa a
/// devolver `Err` em vez de enviar. É o comportamento certo: a sessão acabou.
impl Drop for Session {
    fn drop(&mut self) {
        // 1. **Antes de destruir qualquer coisa**, fechar o portão e esperar esvaziar.
        //
        //    A fronteira C entrega handles de track que sobrevivem à sessão — o contrato de
        //    `quall_session_close` promete que os contadores continuam legíveis. Depois do
        //    `rtcDeleteTrack` abaixo, o id não existe mais para a libdatachannel, e tocá-lo
        //    **trava o processo no Windows**.
        //
        //    Fechar a porta é o que transforma isso em `Err`; **esperar esvaziar** é o que
        //    impede o `rtcDeleteTrack` de acontecer com uma thread da casca no meio de uma
        //    chamada — a corrida que a bandeira booleana anterior deixava aberta — e é a
        //    barreira que a casca precisa para liberar o `user_data`. Ver [`crate::portao`].
        //
        //    Quem quer *saber* se a barreira valeu chama [`Session::fechar_portao`] antes de
        //    largar a sessão; aqui o resultado não tem para quem ser devolvido.
        let _ = self.fechar_portao(PRAZO_DA_BARREIRA);

        // 2. Apagar as entradas dos mapas globais da libdatachannel. Sem este passo elas ficam
        //    lá para sempre: `erasePeerConnection` apaga apenas `peerConnectionMap` e
        //    `userPointerMap`, e a track fica presa com o pacotizador H.264, o relator RTCP e a
        //    cadeia de handlers junto. Não é vazamento transitório — é permanente enquanto o
        //    processo viver, e é cobrado por **tentativa** de sessão, não por sessão.
        //
        //    A ordem contra o `pc` foi testada nos dois sentidos no Dell G3 e **não faz
        //    diferença**; fica como estava, que é a que o `Drop` do Rust dá de graça.
        if let Ok(guarda) = self.tracks.lock() {
            for track in guarda.iter() {
                if track.fechar() {
                    self.fechadas.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // 3. E o registro de tratadores do `quall-rtc`, que é nosso.
        quall_rtc::esquecer_conexao(self.pc_id);

        // 4. A fila de entrada fecha: nada mais entra, e um `Mensageiro` que sobreviveu à sessão
        //    lê o que já tinha chegado e depois ouve "fechada". Um leitor parado esperando acorda.
        //    A ordem de destruição dos campos não muda: o canal continua morrendo depois da
        //    `PeerConnection`, pela ordem da declaração — o `Mensageiro` segura só um `Weak`.
        self.fila.fechar();
    }
}

// =============================================================================================
// Mensagens de aplicação
// =============================================================================================

/// Os contadores que os [`Mensageiro`]s de uma sessão dividem.
#[derive(Default)]
struct ContadoresInternos {
    enviadas: AtomicU64,
    recebidas: AtomicU64,
    descartadas_invalidas: AtomicU64,
}

/// Quanto passou pelo [`Mensageiro`] de uma sessão. Os nomes são os do JSON de
/// `quall_messages_stats_json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ContadoresDeMensagens {
    /// Mensagens que [`Mensageiro::enviar`] entregou à biblioteca.
    pub enviadas: u64,
    /// Mensagens válidas entregues a quem lê.
    pub recebidas: u64,
    /// Descartadas porque a fila de entrada estava cheia ([`FILA_DE_DADOS`]). Num canal
    /// confiável e sem ordem é a **mais velha** que sai; nos outros, a que chegava.
    pub descartadas_fila_cheia: u64,
    /// Chegaram e não eram mensagem: vazias, com NUL no meio, ou que não são UTF-8.
    pub descartadas_invalidas: u64,
}

/// **Mandar e receber mensagens de aplicação** pelo canal de dados de uma sessão.
///
/// A peça "Mensagens entre aparelhos, pelas cascas" (`docs/handover-seis-frentes.md` §2): o
/// teleprompter é o primeiro cliente, e o contrato está em `docs/contrato-teleprompter.md`.
///
/// # Uma mensagem é texto
///
/// UTF-8, sem NUL no meio, de 1 a [`TETO_DA_MENSAGEM`] bytes. Texto e não bytes porque a
/// fronteira C a devolve no padrão `(buf, cap)` de string, e porque o que atravessa é JSON. Uma
/// mensagem vazia é recusada no envio e descartada na chegada: a libopus caiu com pacote de zero
/// byte (`docs/audio.md` §13), e o transporte **leva** zero byte (`sctptransport.cpp:607-610`) —
/// então "vazio" não pode chegar a ninguém confundido com "nada".
///
/// # Sobrevive à sessão, e nunca toca id morto
///
/// Segura o [`Portao`] da sessão e um `Weak` do canal. Mandar entra no portão antes de tocar a
/// API C; o `Drop` da [`Session`] fecha o portão **e espera quem está dentro sair** antes de
/// destruir a conexão e o canal. Depois disso o portão está fechado e mandar devolve
/// [`Error::Closed`] sem chegar à biblioteca — a regra de plataforma do Windows
/// (`docs/divida-do-nucleo.md`) é exatamente não chamar a API C com id que possa estar morto.
///
/// # Threads
///
/// [`Mensageiro::enviar`] pode ser chamado de qualquer thread. Ler ([`Mensageiro::proxima`],
/// [`Mensageiro::entregar_se`]) **avança estado** — é de uma thread só, como as outras funções
/// que avançam estado.
#[derive(Clone)]
pub struct Mensageiro {
    sessao: u64,
    canal: Weak<Mutex<Option<Box<RtcDataChannel<DcHandler>>>>>,
    portao: Arc<Portao>,
    fila: Arc<FilaDeEntrada>,
    estado_do_canal: Arc<AtomicU8>,
    contadores: Arc<ContadoresInternos>,
    descartadas_fila_cheia: Arc<AtomicU64>,
    teto: Arc<std::sync::atomic::AtomicUsize>,
    par: Option<Arc<ParDaSessao>>,
}

/// O que conta como mensagem.
fn e_mensagem(bytes: &[u8]) -> bool {
    !bytes.is_empty() && !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// O número da próxima [`Session`] do processo. Começa em 1: `0` fica para "nenhuma".
static PROXIMA_SESSAO: AtomicU64 = AtomicU64::new(1);

impl std::fmt::Debug for Mensageiro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mensageiro(sessão {})", self.sessao)
    }
}

impl Mensageiro {
    /// **De qual sessão é este mensageiro**: um número por [`Session`] do processo, nunca
    /// repetido. Quem guarda estado entre sessões (a réplica do teleprompter) o usa para saber
    /// que começou uma sessão nova, com um par que ainda não viu nada.
    pub fn sessao(&self) -> u64 {
        self.sessao
    }

    /// **Quem é o outro lado** desta sessão ([`ParDaSessao`]), quando ela veio de
    /// `session::hospedar`/`conectar`. `None` numa sessão do transporte montada à mão — e aí a
    /// réplica do teleprompter segue a regra de antes, sem pergunta e sem cópia.
    pub fn par(&self) -> Option<&ParDaSessao> {
        self.par.as_deref()
    }

    /// Manda uma mensagem.
    ///
    /// - vazia, com NUL, ou acima de [`TETO_DA_MENSAGEM`]: [`Error::Invalid`], e nada sai;
    /// - o canal ainda não abriu: [`Error::Transport`] — **tente de novo**, não é queda. Logo
    ///   depois de `conectar` voltar, quem responde pode estar a milissegundos do `on_open`;
    /// - a sessão acabou, ou o canal fechou: [`Error::Closed`].
    pub fn enviar(&self, mensagem: &str) -> Result<()> {
        let bytes = mensagem.as_bytes();
        if bytes.is_empty() {
            return Err(Error::Invalid("mensagem vazia".into()));
        }
        if bytes.contains(&0) {
            return Err(Error::Invalid("mensagem com NUL no meio".into()));
        }
        let teto = self.teto();
        if bytes.len() > teto {
            return Err(Error::Invalid(format!(
                "mensagem de {} bytes passa do teto de {teto}",
                bytes.len()
            )));
        }
        // Dentro do portão até a biblioteca devolver: o `Drop` da sessão espera este passe sair
        // antes de destruir o canal.
        let Some(_passe) = self.portao.entrar() else {
            return Err(Error::Closed);
        };
        match self.estado_do_canal.load(Ordering::Acquire) {
            CANAL_ABERTO => {}
            CANAL_NUNCA_ABRIU => {
                return Err(Error::Transport("o canal de dados ainda não abriu".into()))
            }
            _ => return Err(Error::Closed),
        }
        let Some(canal) = self.canal.upgrade() else {
            return Err(Error::Closed);
        };
        let mut guarda = canal
            .lock()
            .map_err(|_| Error::Transport("canal de dados envenenado por um panic".into()))?;
        let Some(dc) = guarda.as_mut() else {
            return Err(Error::Transport("o canal de dados ainda não abriu".into()));
        };
        dc.send(bytes)
            .map_err(|e| Error::Transport(format!("falha ao enviar: {e}")))?;
        self.contadores.enviadas.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// A próxima mensagem, esperando até `limite`. `Duration::ZERO` não espera.
    ///
    /// `Ok(None)` é "nada chegou no prazo". [`Error::Closed`] é "a sessão acabou e não sobrou nada
    /// na fila" — o que chegou antes do fim é entregue antes.
    pub fn proxima(&self, limite: Duration) -> Result<Option<String>> {
        let mut saida = None;
        match self.entregar_se(limite, |texto| {
            saida = Some(texto.to_string());
            true
        })? {
            Some(_) => Ok(saida),
            None => Ok(None),
        }
    }

    /// A espiada que a fronteira C precisa: mostra a próxima mensagem a `aceitar` e só a tira da
    /// fila se `aceitar` devolver `true`.
    ///
    /// Devolve o tamanho **em bytes** da mensagem mostrada (`Some`), `None` quando nada chegou no
    /// prazo, e [`Error::Closed`] quando a sessão acabou e a fila esvaziou. A vaga é **da sessão**:
    /// uma mensagem mostrada e não aceita volta a ser a primeira para qualquer mensageiro dela.
    pub fn entregar_se(
        &self,
        limite: Duration,
        aceitar: impl FnOnce(&str) -> bool,
    ) -> Result<Option<usize>> {
        let contadores = Arc::clone(&self.contadores);
        let espiada = self.fila.entregar_se(
            limite,
            e_mensagem,
            || {
                contadores
                    .descartadas_invalidas
                    .fetch_add(1, Ordering::Relaxed);
            },
            // `e_mensagem` já conferiu que é UTF-8; o `unwrap_or` é só para não haver `unwrap`.
            |bytes| aceitar(std::str::from_utf8(bytes).unwrap_or("")),
        );
        match espiada {
            Espiada::Havia { tamanho, consumida } => {
                if consumida {
                    self.contadores.recebidas.fetch_add(1, Ordering::Relaxed);
                }
                Ok(Some(tamanho))
            }
            Espiada::Nada => Ok(None),
            Espiada::Fechada => Err(Error::Closed),
        }
    }

    /// **A sessão deste mensageiro acabou?** O `Drop` da sessão fechou o portão, ou o canal
    /// fechou. Não toca na API C. Quem guarda um mensageiro por sessão (o filmador do controle
    /// remoto da câmera, `camera_remota`) usa isto para esquecer a sessão que ninguém bombeia mais.
    pub fn acabou(&self) -> bool {
        let Some(_passe) = self.portao.entrar() else {
            return true;
        };
        self.estado_do_canal.load(Ordering::Acquire) == CANAL_FECHOU || self.canal.upgrade().is_none()
    }

    /// **O teto de uma mensagem nesta sessão**: [`TETO_DA_MENSAGEM`], ou menos se o outro lado
    /// anunciou menos no SDP (entre dois Quall, nunca).
    pub fn teto(&self) -> usize {
        self.teto.load(Ordering::Relaxed)
    }

    /// Quanto passou por aqui.
    pub fn contadores(&self) -> ContadoresDeMensagens {
        ContadoresDeMensagens {
            enviadas: self.contadores.enviadas.load(Ordering::Relaxed),
            recebidas: self.contadores.recebidas.load(Ordering::Relaxed),
            descartadas_fila_cheia: self.descartadas_fila_cheia.load(Ordering::Relaxed),
            descartadas_invalidas: self.contadores.descartadas_invalidas.load(Ordering::Relaxed),
        }
    }

    /// Quantos bytes estão esperando para sair. Num canal confiável, é o que se acumula quando o
    /// outro lado não confirma.
    ///
    /// Entra no portão como [`Mensageiro::enviar`]: `0` depois de a sessão acabar.
    pub fn pendente(&self) -> usize {
        let Some(_passe) = self.portao.entrar() else {
            return 0;
        };
        self.canal
            .upgrade()
            .and_then(|c| {
                c.lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|dc| dc.buffered_amount()))
            })
            .unwrap_or(0)
    }
}

/// Libera os recursos globais da libdatachannel.
///
/// Chame uma vez, na saída do processo. Num app comum dá para viver sem; num plugin de OBS que é
/// descarregado e recarregado, não — a biblioteca deixa threads vivas.
///
/// # Destrua todas as [`Session`] antes
///
/// **Correção do mecanismo, 2026-08-23.** A versão anterior deste texto dizia que chamar com uma
/// `Session` viva "trava o processo, sem erro e sem log". Isso era o **sintoma** visto na
/// bancada, não o que o fonte faz. Lido em `libdatachannel/src/capi.cpp:1691` da 0.23.2 que está
/// no `Cargo.lock`:
///
/// ```text
/// void rtcCleanup() {
///     try {
///         size_t count = eraseAll();
///         if (count != 0) PLOG_INFO << count << " objects were not properly destroyed ...";
///         if (rtc::Cleanup().wait_for(10s) == std::future_status::timeout)
///             throw std::runtime_error("Cleanup timeout (possible deadlock ...)");
///     } catch (const std::exception &e) { PLOG_ERROR << e.what(); }
/// }
/// ```
///
/// Ou seja: `rtcCleanup()` **volta**, em cerca de 10 segundos, e **registra** — um `PLOG_INFO`
/// com a contagem de objetos vivos e um `PLOG_ERROR` com "Cleanup timeout". Quem fica pendurada
/// é a thread de limpeza, presa no `Init::mMutex`, e é ela que impede o processo de morrer.
///
/// A consequência prática não muda: **destrua tudo antes**. O que muda é o diagnóstico — há log,
/// e procurá-lo é o primeiro passo, não o último.
///
/// Foi assim que a sonda ficou pendurada na bancada em 2026-08-21: o relatório da medição saía
/// inteiro na tela e o processo não morria, segurando a porta 7877 e fazendo a execução seguinte
/// falhar com "Address already in use" — um sintoma três passos distante da causa. O `Drop` da
/// `Session` rodava depois do `cleanup()`, porque em Rust os locais são destruídos no fim da
/// função, e não antes da última chamada dela.
///
/// ```no_run
/// # use quall_core::transport::{Session, TransportConfig, cleanup};
/// # fn exemplo() -> quall_core::error::Result<()> {
/// let sessao = Session::offerer(&TransportConfig::default())?;
/// // ... usa a sessão ...
/// drop(sessao); // explícito: sem isto o `cleanup` abaixo trava
/// cleanup();
/// # Ok(())
/// # }
/// ```
pub fn cleanup() {
    datachannel::cleanup();
}

#[cfg(test)]
mod tests {

    /// **A mídia e a sinalização leem o mesmo campo pelo mesmo caminho.** Duas validações
    /// separadas divergiriam, e a que divergisse em silêncio seria a que sai por outra interface
    /// sem ninguém notar.
    #[test]
    fn origem_devolve_o_endereco_quando_ha_um_e_nada_quando_nao_ha() {
        let mut cfg = TransportConfig::default();
        assert!(cfg.origem().expect("padrão é válido").is_none());
        cfg.bind_address = Some("169.254.75.173".into());
        assert_eq!(
            cfg.origem().expect("endereço válido"),
            Some("169.254.75.173".parse().unwrap())
        );
    }

    /// Texto que não é endereço é **erro de quem chamou**, e não um "não prendi" silencioso: o
    /// segundo faria a sessão sair pela interface errada com tudo parecendo bem.
    #[test]
    fn origem_recusa_o_que_nao_e_endereco_em_vez_de_ignorar() {
        let mut cfg = TransportConfig::default();
        for lixo in ["en8", "169.254.75.173:7877", "", "169.254.75"] {
            cfg.bind_address = Some(lixo.into());
            let erro = cfg.origem().expect_err("devia recusar {lixo}");
            assert!(matches!(erro, Error::Invalid(_)), "{lixo:?} deu {erro:?}");
        }
    }
    use super::*;
    use crate::track::{
        AmostraDeAudio, CodecDeAudio, QuadroCodificado, TrackKind, PRESET_AUDIO_DO_SISTEMA,
        PRESET_MICROFONE,
    };
    use std::sync::atomic::AtomicBool;

    /// Onde os testes juntam o que chegou pela track: quadro, se é IDR, e o carimbo.
    type Colhidos = Arc<Mutex<Vec<(Vec<u8>, bool, u64)>>>;

    /// Fecha uma sessão inteira entre duas `Session` no mesmo processo, trocando SDP e
    /// candidatos na mão — que é exatamente o que a sinalização faz, sem a rede no meio.
    ///
    /// Isto é loopback: prova a integração com a libdatachannel, o trickle de candidatos e o
    /// canal de dados. **Não** prova latência de LAN nem travessia entre máquinas. Esses
    /// números saem da sonda, na bancada.
    #[test]
    fn duas_sessoes_fecham_e_trocam_dados() {
        let cfg = TransportConfig::default();
        let mut a = Session::offerer(&cfg).expect("ofertante");
        let mut b = Session::answerer(&cfg).expect("respondente");

        let mut abriu = false;
        let mut recebido: Option<Vec<u8>> = None;
        let prazo = std::time::Instant::now() + Duration::from_secs(20);

        while std::time::Instant::now() < prazo {
            // Eventos do ofertante vão para o respondente, e vice-versa.
            if let Some(e) = a.next_event(Duration::from_millis(10)) {
                match e {
                    TransportEvent::LocalDescription { kind, sdp } => {
                        b.set_remote_description(&kind, &sdp).expect("sdp em b");
                    }
                    TransportEvent::LocalCandidate { candidate, mid } => {
                        let _ = b.add_remote_candidate(&candidate, &mid);
                    }
                    TransportEvent::ChannelOpen => abriu = true,
                    _ => {}
                }
            }
            if let Some(e) = b.next_event(Duration::from_millis(10)) {
                match e {
                    TransportEvent::LocalDescription { kind, sdp } => {
                        a.set_remote_description(&kind, &sdp).expect("sdp em a");
                    }
                    TransportEvent::LocalCandidate { candidate, mid } => {
                        let _ = a.add_remote_candidate(&candidate, &mid);
                    }
                    TransportEvent::ChannelOpen => abriu = true,
                    _ => {}
                }
            }

            if abriu && recebido.is_none() && a.send(b"quall").is_ok() {
                if let Some(d) = b.next_data(Duration::from_millis(500)) {
                    recebido = Some(d);
                }
            }
            if recebido.is_some() {
                break;
            }
        }

        assert!(abriu, "o canal de dados não abriu dentro do prazo");
        assert_eq!(recebido.as_deref(), Some(&b"quall"[..]));
        assert_eq!(a.dropped_frames(), 0);
    }

    /// Negocia duas sessões no mesmo processo — SDP e candidatos na mão — e **para de bombear
    /// assim que as duas conectam**.
    ///
    /// # Parar de bombear não é economia, é obrigatório
    ///
    /// A primeira versão deste laço continuava aplicando descrição remota e candidatos enquanto a
    /// mídia já corria. No macOS passava. **No Dell G3 travava para sempre**, medido em
    /// 2026-08-21: os dois testes que esperam um quadro atravessar ficavam pendurados sem
    /// consumir CPU, e `cargo test --workspace` nunca terminava — o pior tipo de portão vermelho,
    /// porque não dá erro, só não acaba. Bissecção por teste isolado mostrou que eles paravam
    /// **dentro** do laço, e não no encerramento.
    ///
    /// Não é defeito do produto, e isso foi verificado, não presumido: a sonda
    /// `emitir-video`/`receber-video` levou 416 quadros do MacBook ao Dell com zero perda, no
    /// mesmo par de máquinas e no mesmo dia. O produto **nunca** mexe em SDP depois de conectar —
    /// [`crate::session::negociar`] sai do laço quando o canal abre. Era o teste que fazia o que
    /// o produto não faz, e por isso esbarrou num travamento da libdatachannel que não interessa
    /// a ninguém.
    ///
    /// `ao_passo` roda a cada volta e serve para recolher as tracks, que chegam durante o
    /// processamento do SDP — antes de o ICE fechar.
    ///
    /// Devolve se as duas pontas chegaram a `Connected`.
    fn negociar(
        a: &mut Session,
        b: &mut Session,
        prazo: Duration,
        mut ao_passo: impl FnMut(&Session, &Session),
    ) -> bool {
        /// Um passo: tira um evento de `origem` e aplica em `destino`. Devolve se `origem`
        /// anunciou que conectou.
        fn passo(origem: &Session, destino: &mut Session) -> bool {
            let Some(e) = origem.next_event(Duration::from_millis(5)) else {
                return false;
            };
            match e {
                TransportEvent::LocalDescription { kind, sdp } => {
                    destino.set_remote_description(&kind, &sdp).expect("sdp");
                    false
                }
                TransportEvent::LocalCandidate { candidate, mid } => {
                    // Candidato recusado é normal: chega candidato de interface que este lado
                    // não alcança, e o ICE segue com os outros.
                    let _ = destino.add_remote_candidate(&candidate, &mid);
                    false
                }
                TransportEvent::State(PeerState::Connected) => true,
                _ => false,
            }
        }

        let fim = std::time::Instant::now() + prazo;
        let mut conectou_a = false;
        let mut conectou_b = false;
        while std::time::Instant::now() < fim {
            conectou_a |= passo(a, b);
            conectou_b |= passo(b, a);
            ao_passo(a, b);
            if conectou_a && conectou_b {
                return true;
            }
        }
        false
    }

    /// Espera até `condicao` ou até o prazo, **sem tocar em SDP**. É o que o produto faz depois
    /// de conectar.
    fn esperar(prazo: Duration, mut condicao: impl FnMut() -> bool) -> bool {
        let fim = std::time::Instant::now() + prazo;
        while std::time::Instant::now() < fim {
            if condicao() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        condicao()
    }

    /// Monta um quadro Annex-B sintético: SPS, PPS e um IDR grande o bastante para exigir FU-A.
    ///
    /// O pacotizador da libdatachannel não interpreta H.264 — ele fatia nos start codes —, então
    /// NAL units sintéticas exercitam exatamente o mesmo caminho que as reais, e a comparação
    /// byte a byte na volta vira uma prova forte.
    fn quadro_idr(tamanho_idr: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x65]);
        // Carga determinística e sem `00 00 01` acidental: começando em 1 e nunca chegando a 0.
        v.extend((0..tamanho_idr).map(|i| ((i % 254) + 1) as u8));
        v
    }

    /// **O teste do M2.** Uma track de vídeo atravessa de ponta a ponta: pacotizador da
    /// libdatachannel de um lado, nosso depacotizador da RFC 6184 do outro, SRTP no meio.
    ///
    /// O IDR tem 8 KiB, bem acima do fragmento de 1188 bytes, então o caminho exercitado é
    /// FU-A — que é onde um depacotizador errado quebra. A comparação é byte a byte contra o
    /// que entrou.
    ///
    /// Isto é laço no mesmo processo: prova a integração e o formato, **não** prova latência de
    /// LAN nem travessia entre máquinas. Esses números saem da sonda, na bancada.
    #[test]
    fn track_de_video_atravessa_e_o_quadro_volta_igual() {
        let cfg = TransportConfig::default();
        let (mut a, emissores) = Session::offerer_com_tracks(
            &cfg,
            &[TrackConfig::new(TrackKind::Screen, "Tela de teste")],
        )
        .expect("ofertante com track");
        let mut b = Session::answerer(&cfg).expect("respondente");
        let emissor = emissores.into_iter().next().expect("um emissor");

        // A track do outro lado chega durante o processamento do SDP.
        let mut receptor: Option<TrackReceptor> = None;
        let conectou = negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            if receptor.is_none() {
                receptor = b.proxima_track(Duration::from_millis(1));
            }
        });
        assert!(conectou, "as duas pontas não conectaram dentro do prazo");
        let receptor = receptor.expect("a track não chegou no lado que responde");
        assert_eq!(receptor.kind(), TrackKind::Screen);

        let recebido: Colhidos = Arc::new(Mutex::new(Vec::new()));
        {
            let alvo = Arc::clone(&recebido);
            receptor.ao_receber_quadro(move |q| {
                if let Ok(mut g) = alvo.lock() {
                    g.push((q.annexb.to_vec(), q.idr, q.timestamp_us));
                }
            });
        }

        let quadro = quadro_idr(8192);
        let mut enviou = false;
        esperar(Duration::from_secs(20), || {
            if !enviou {
                enviou = emissor
                    .enviar_quadro(QuadroCodificado {
                        annexb: &quadro,
                        timestamp_us: 1_000_000,
                        idr: true,
                    })
                    .is_ok();
            }
            recebido.lock().map(|g| !g.is_empty()).unwrap_or(false)
        });

        let chegaram = recebido.lock().expect("cadeado").clone();
        assert_eq!(
            chegaram.len(),
            1,
            "chegou {} quadro(s), esperado 1",
            chegaram.len()
        );
        assert!(chegaram[0].1, "o quadro tinha de vir marcado como IDR");
        assert_eq!(
            chegaram[0].0,
            quadro,
            "o quadro remontado difere do que entrou: {} bytes contra {}",
            chegaram[0].0.len(),
            quadro.len()
        );
        assert_eq!(receptor.quadros_descartados(), 0);
        assert_eq!(receptor.pacotes_perdidos(), 0);
        assert_eq!(emissor.idrs_sem_parametros(), 0);

        // Os contadores separados, na track de verdade e não num depacotizador de mesa: um IDR
        // de 8 KiB são vários fragmentos FU-A, e num laço local nenhum deles se perde nem
        // reordena. `pacotes_vistos` maior que zero é o que distingue "não perdeu nada" de
        // "não chegou nada" — e é a leitura que `sequence_anomalies == 0` sozinho nunca deu.
        let c = receptor.contadores();
        assert!(
            c.pacotes_vistos > 1,
            "um IDR de 8 KiB tinha de chegar em vários pacotes; vieram {}",
            c.pacotes_vistos
        );
        assert_eq!(c.pacotes_faltando, 0);
        assert_eq!(c.eventos_fora_de_ordem, 0);
        assert_eq!(c.pacotes_perdidos(), receptor.pacotes_perdidos());
    }

    /// **O relógio comum e a porta puxada numa sessão de verdade**, em `lo0`, com a libdatachannel
    /// no meio (`docs/som-no-receptor.md` §5 e §3).
    ///
    /// O emissor carimba vídeo e som a partir de **20 h de uptime**: o contador de 90 kHz já deu
    /// uma volta e o de 48 kHz ainda não. O receptor puxa o som numa thread própria, a cada
    /// 20 ms do relógio de verdade, e cada quadro e cada pacote carregam o próprio índice. No
    /// fim, `timestamp_us + deslocamento` das duas tracks tem de reproduzir a diferença de
    /// captura que o emissor pôs no fio, ao microssegundo.
    ///
    /// É também a conferência da **premissa** de que o carimbo no fio sai de `timestamp_us` sem
    /// deslocamento sorteado. Se alguém trocar o envio por `rtcSendFrame` com `timestampSeconds`,
    /// o sorteio de `rtppacketizationconfig.cpp` volta, e este teste reprova (miúdo da crítica 3).
    #[test]
    fn relogio_comum_e_porta_puxada_numa_sessao_de_verdade() {
        sessao_com_relogio_comum(20 * 3_600_000_000 + 123_457, 0);
    }

    /// O mesmo, com o contador de **90 kHz dando a volta no meio** da corrida. Trazido da
    /// revisão do código da S1 (B6): as 20 h de cima exercitam o reticulado, mas não atravessam
    /// volta nenhuma.
    #[test]
    fn relogio_comum_com_a_volta_de_90_khz_no_meio() {
        let volta = (1u64 << 32) * 1_000_000 / 90_000;
        sessao_com_relogio_comum(2 * volta - 1_500_000, 0);
    }

    /// O mesmo, com o contador de **48 kHz dando a volta no meio** da corrida (B6).
    #[test]
    fn relogio_comum_com_a_volta_de_48_khz_no_meio() {
        let volta = (1u64 << 32) * 1_000_000 / 48_000;
        sessao_com_relogio_comum(volta - 1_500_000, 0);
    }

    /// O mesmo, com o contador de 48 kHz dando a volta no meio **e a ordem trocada na chegada**:
    /// um pacote de som a cada dois é entregue ao núcleo depois do seguinte
    /// (`TrackReceptor::cravar_troca_de_bancada`). É o controle 5 do `docs/som-no-receptor.md`
    /// §9.3 de ponta a ponta: as sequências das críticas 1 e 3 da S1 só existiam em teste de
    /// unidade, e em `lo0` não há reordenação natural.
    ///
    /// **Por que duas sessões.** A troca segura os pacotes ímpares **na contagem da chegada**, e
    /// os primeiros pacotes do emissor podem não sair (a track ainda não abriu), então a paridade
    /// do par da volta não é conhecida de antemão. As duas bases põem a volta entre os quadros
    /// 75|76 e 76|77 do emissor: numa delas o pacote de antes da volta chega depois do de depois.
    /// O teste exige que isso tenha acontecido pelo menos uma vez (`trocados_na_volta`), senão o
    /// controle não controlou nada.
    #[test]
    fn relogio_comum_com_a_volta_de_48_khz_e_a_ordem_trocada_na_chegada() {
        let volta = (1u64 << 32) * 1_000_000 / 48_000;
        let (t1, v1) = sessao_com_relogio_comum(volta - 1_500_000, 2);
        let (t2, v2) = sessao_com_relogio_comum(volta - 1_520_000, 2);
        eprintln!("troca na chegada: {t1} e {t2} pares; {v1} e {v2} através da volta");
        assert!(t1 > 50 && t2 > 50, "a troca não trocou: {t1} e {t2} pares");
        assert!(
            v1 + v2 >= 1,
            "nenhum par trocado atravessou a volta de 32 bits: o controle não exercitou o defeito"
        );
    }

    /// Devolve os pares que a troca de bancada entregou trocados, e quantos deles atravessavam a
    /// volta de 32 bits.
    fn sessao_com_relogio_comum(base: u64, trocar_a_cada: u32) -> (u64, u64) {
        use crate::relogio::DeslocamentoDeCaptura;
        use crate::reproducao::Puxado;
        use std::sync::atomic::AtomicBool;

        #[allow(non_snake_case)]
        let BASE: u64 = base;
        const Q_VIDEO: u64 = 33_333;
        const Q_AUDIO: u64 = 20_000;

        let cfg = TransportConfig::default();
        let (mut a, emissores) = Session::offerer_com_tracks(
            &cfg,
            &[
                TrackConfig::new(TrackKind::Screen, "Tela"),
                TrackConfig::new(TrackKind::SystemAudio, "Som"),
            ],
        )
        .expect("ofertante");
        let mut b = Session::answerer(&cfg).expect("respondente");

        let mut chegadas: Vec<TrackReceptor> = Vec::new();
        let conectou = negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            while let Some(t) = b.proxima_track(Duration::from_millis(1)) {
                chegadas.push(t);
            }
        });
        assert!(conectou, "as duas pontas não conectaram");
        esperar(Duration::from_secs(5), || {
            while let Some(t) = b.proxima_track(Duration::from_millis(1)) {
                chegadas.push(t);
            }
            chegadas.len() >= 2
        });
        let pos_video = chegadas.iter().position(|t| t.kind() == TrackKind::Screen);
        let pos_audio = chegadas.iter().position(|t| t.kind() == TrackKind::SystemAudio);
        let (Some(pv), Some(pa)) = (pos_video, pos_audio) else {
            panic!("as duas tracks tinham de chegar");
        };
        let video = &chegadas[pv];
        let audio = &chegadas[pa];
        if trocar_a_cada > 0 {
            assert!(audio.cravar_troca_de_bancada(trocar_a_cada));
        }

        // Vídeo: (timestamp_us, índice que o quadro carrega).
        let quadros: Arc<Mutex<Vec<(u64, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let alvo = Arc::clone(&quadros);
            video.ao_receber_quadro(move |q| {
                // O índice vem logo depois do cabeçalho da NAL de IDR.
                let n = q.annexb.len();
                let indice = u32::from_be_bytes([
                    q.annexb[n - 4],
                    q.annexb[n - 3],
                    q.annexb[n - 2],
                    q.annexb[n - 1],
                ]);
                if let Ok(mut g) = alvo.lock() {
                    g.push((q.timestamp_us, indice));
                }
            });
        }

        // Som: a porta puxada, numa thread que puxa a cada 20 ms do relógio de verdade.
        let mut reproducao = audio.reproducao_puxada(false).expect("porta puxada");
        assert!(
            audio.ao_receber_audio(|_| {}).is_err(),
            "com a porta puxada aberta, a empurrada é recusada"
        );
        let leitor = reproducao.leitor();
        let parar = Arc::new(AtomicBool::new(false));
        let tocados: Arc<Mutex<Vec<(u64, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let puxador = {
            let parar = Arc::clone(&parar);
            let tocados = Arc::clone(&tocados);
            std::thread::spawn(move || {
                let inicio = std::time::Instant::now();
                let mut n = 0u64;
                while !parar.load(Ordering::Relaxed) {
                    if let Puxado::Slot(crate::jitter::Entrega::Quadro {
                        payload,
                        timestamp_us,
                        ..
                    }) = reproducao.puxar(10_000, f64::NAN)
                    {
                        let indice = u32::from_be_bytes([
                            payload[1], payload[2], payload[3], payload[4],
                        ]);
                        if let Ok(mut g) = tocados.lock() {
                            g.push((timestamp_us, indice));
                        }
                    }
                    n += 1;
                    let alvo = inicio + Duration::from_micros(n * Q_AUDIO);
                    if let Some(espera) = alvo.checked_duration_since(std::time::Instant::now()) {
                        std::thread::sleep(espera);
                    }
                }
                reproducao.encerrar()
            })
        };

        // O emissor: 3,5 s de vídeo a 30 q/s e som a 50 pacotes/s, no relógio da captura.
        let inicio = std::time::Instant::now();
        let (mut iv, mut ia) = (0u32, 0u32);
        while inicio.elapsed() < Duration::from_millis(3_500) {
            let agora = inicio.elapsed().as_micros() as u64;
            while u64::from(iv) * Q_VIDEO <= agora {
                let mut quadro = quadro_idr(400);
                quadro.extend_from_slice(&iv.to_be_bytes());
                let _ = emissores[0].enviar_quadro(QuadroCodificado {
                    annexb: &quadro,
                    timestamp_us: BASE + u64::from(iv) * Q_VIDEO,
                    idr: true,
                });
                iv += 1;
            }
            while u64::from(ia) * Q_AUDIO <= agora {
                let mut pacote = vec![0xfcu8];
                pacote.extend_from_slice(&ia.to_be_bytes());
                let _ = emissores[1].enviar_audio(AmostraDeAudio {
                    payload: &pacote,
                    timestamp_us: BASE + u64::from(ia) * Q_AUDIO,
                });
                ia += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // O retrato antes de o emissor parar: os subconsumos daqui são do fluxo, e não da cauda.
        let durante = leitor.contadores();
        std::thread::sleep(Duration::from_millis(200));
        parar.store(true, Ordering::Relaxed);
        let barreira = puxador.join().expect("a thread do puxador");
        assert!(barreira.cumprida(), "a barreira da porta puxada: {barreira:?}");
        assert!(
            audio.ao_receber_audio(|_| {}).is_ok(),
            "fechada a porta puxada, a track volta a aceitar a empurrada"
        );

        let (DeslocamentoDeCaptura::Valido { us: dv }, DeslocamentoDeCaptura::Valido { us: da }) =
            (video.deslocamento_de_captura(), audio.deslocamento_de_captura())
        else {
            panic!(
                "os dois deslocamentos tinham de ser válidos: vídeo {:?}, áudio {:?}, retrato do \
                 áudio {:?}",
                video.deslocamento_de_captura(),
                audio.deslocamento_de_captura(),
                audio.retrato_do_relogio()
            );
        };

        let quadros = quadros.lock().expect("cadeado").clone();
        let tocados = tocados.lock().expect("cadeado").clone();
        assert!(quadros.len() > 60, "{} quadros de vídeo chegaram", quadros.len());
        assert!(tocados.len() > 100, "{} slots de som tocaram", tocados.len());
        let c = leitor.contadores();
        assert_eq!(c.puxadas, c.soma_das_ordens(), "{c:?}");

        // A conferência: para cada par (quadro, slot), a diferença de captura que o relógio comum
        // reconstrói é a que o emissor pôs no fio.
        let mut pior = 0i64;
        for (ts_v, iv) in quadros.iter().step_by(7) {
            for (ts_a, ia) in tocados.iter().step_by(11) {
                let reconstruida = (*ts_a as i64 + da) - (*ts_v as i64 + dv);
                let verdade = (u64::from(*ia) * Q_AUDIO) as i64 - (u64::from(*iv) * Q_VIDEO) as i64;
                pior = pior.max((reconstruida - verdade).abs());
            }
        }
        eprintln!(
            "relógio comum: pior erro {pior} µs em {} quadros × {} slots; deslocamentos vídeo \
             {dv} µs, áudio {da} µs; retrato do áudio {:?}; subconsumos durante o fluxo {}; \
             porta puxada no fim {c:?}",
            quadros.len(),
            tocados.len(),
            audio.retrato_do_relogio(),
            durante.subconsumos
        );
        assert!(
            durante.subconsumos <= 3,
            "subconsumo durante o fluxo em laço local: {durante:?}"
        );
        assert!(
            pior <= 40,
            "o relógio comum errou a diferença de captura em até {pior} µs (tolerância: a \
             quantização de um tique de 48 kHz mais um de 90 kHz)"
        );
        audio.trocados_na_bancada()
    }

    /// **Nada que a casca chame de dentro do tratador pode pedir o cadeado que a bomba segura**
    /// (revisão do código da S1, achado B1).
    ///
    /// Antes do conserto, pedir o deslocamento de captura no primeiro quadro, ou os contadores em
    /// qualquer quadro, de dentro do tratador travava a thread da libdatachannel para sempre, e a
    /// sessão parava de receber.
    ///
    /// O teste pergunta primeiro **de outra thread, com prazo de 1 s**. Se a pergunta precisasse
    /// do cadeado, a outra thread esperaria o tratador voltar, o prazo estouraria e o teste
    /// reprovaria, sem travar o processo. Só se ela respondeu é que ele pergunta **na mesma
    /// thread**, que é o caso que travava de vez.
    #[test]
    fn perguntar_de_dentro_do_tratador_de_quadro_nao_trava() {
        use std::sync::mpsc;
        let cfg = TransportConfig::default();
        let (mut a, emissores) =
            Session::offerer_com_tracks(&cfg, &[TrackConfig::new(TrackKind::Screen, "Tela")])
                .expect("ofertante");
        let mut b = Session::answerer(&cfg).expect("respondente");
        let mut receptor: Option<TrackReceptor> = None;
        let conectou = negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            if receptor.is_none() {
                receptor = b.proxima_track(Duration::from_millis(1));
            }
        });
        assert!(conectou, "as duas pontas não conectaram");
        let video = Arc::new(receptor.expect("a track chegou"));

        // Por quadro: (a outra thread respondeu no prazo, o `cravar_anel` de dentro recusou).
        let resultados: Arc<Mutex<Vec<(bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let fraco = Arc::downgrade(&video);
            let resultados = Arc::clone(&resultados);
            video.ao_receber_quadro(move |_q| {
                let Some(v) = fraco.upgrade() else {
                    return;
                };
                let (tx, rx) = mpsc::channel();
                let de_fora = Arc::clone(&v);
                std::thread::spawn(move || {
                    let _ = de_fora.deslocamento_de_captura();
                    let _ = de_fora.retrato_do_relogio();
                    let _ = de_fora.contadores();
                    let _ = tx.send(());
                });
                let respondeu = rx.recv_timeout(Duration::from_secs(1)).is_ok();
                let mut recusou = false;
                if respondeu {
                    let _ = v.deslocamento_de_captura();
                    let _ = v.retrato_do_relogio();
                    let _ = v.contadores();
                    recusou = !v.cravar_anel_de_reordenacao(4);
                }
                if let Ok(mut g) = resultados.lock() {
                    g.push((respondeu, recusou));
                }
            });
        }

        for i in 0..20u64 {
            let q = quadro_idr(400);
            let _ = emissores[0].enviar_quadro(QuadroCodificado {
                annexb: &q,
                timestamp_us: 1_000_000 + i * 33_333,
                idr: true,
            });
            std::thread::sleep(Duration::from_millis(33));
        }
        esperar(Duration::from_secs(25), || {
            resultados.lock().map(|g| g.len() >= 20).unwrap_or(false)
        });
        let r = resultados.lock().expect("cadeado").clone();
        eprintln!(
            "de dentro do tratador: {} quadros; outra thread respondeu em {}; o cravar_anel recusou \
             em {}",
            r.len(),
            r.iter().filter(|x| x.0).count(),
            r.iter().filter(|x| x.1).count()
        );
        assert_eq!(r.len(), 20, "os 20 quadros tinham de passar pelo tratador");
        assert!(
            r.iter().all(|x| x.0),
            "a pergunta de outra thread esperou o cadeado que a bomba segura: {r:?}"
        );
        assert!(
            r.iter().all(|x| x.1),
            "o `cravar_anel` de dentro do tratador tinha de recusar, e não travar: {r:?}"
        );
        assert!(
            matches!(
                video.deslocamento_de_captura(),
                crate::relogio::DeslocamentoDeCaptura::Valido { .. }
            ),
            "a referência tem de ter deslocamento depois do primeiro quadro"
        );
    }

    /// Quanto um IDR de `tamanho` bytes leva do `enviar_quadro` até sair remontado do outro lado,
    /// com o espaçador a `espacamento_kbps` (zero: sem espaçador), e o que chegou. `None` quando
    /// não chegou em 20 s.
    fn tempo_ate_chegar(espacamento_kbps: u32, tamanho: usize) -> (Option<Duration>, Vec<u8>) {
        let cfg = TransportConfig::default();
        let (mut a, emissores) = Session::offerer_com_tracks(
            &cfg,
            &[TrackConfig::new(TrackKind::Screen, "Tela").com_espacamento(espacamento_kbps)],
        )
        .expect("ofertante com track");
        let mut b = Session::answerer(&cfg).expect("respondente");
        let emissor = emissores.into_iter().next().expect("um emissor");

        let mut receptor: Option<TrackReceptor> = None;
        let conectou = negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            if receptor.is_none() {
                receptor = b.proxima_track(Duration::from_millis(1));
            }
        });
        assert!(conectou, "as duas pontas não conectaram dentro do prazo");
        let receptor = receptor.expect("a track não chegou no lado que responde");

        type Chegada = Arc<Mutex<Option<(std::time::Instant, Vec<u8>)>>>;
        let chegada: Chegada = Arc::new(Mutex::new(None));
        {
            let alvo = Arc::clone(&chegada);
            receptor.ao_receber_quadro(move |q| {
                if let Ok(mut g) = alvo.lock() {
                    g.get_or_insert((std::time::Instant::now(), q.annexb.to_vec()));
                }
            });
        }

        // A track abre um pouco depois do ICE, e até lá o envio falha: o relógio começa no
        // primeiro envio aceito.
        let quadro = quadro_idr(tamanho);
        let mut saiu: Option<std::time::Instant> = None;
        esperar(Duration::from_secs(20), || {
            if saiu.is_none() {
                let aceito = emissor
                    .enviar_quadro(QuadroCodificado {
                        annexb: &quadro,
                        timestamp_us: 1_000_000,
                        idr: true,
                    })
                    .is_ok();
                if aceito {
                    saiu = Some(std::time::Instant::now());
                }
            }
            chegada.lock().map(|g| g.is_some()).unwrap_or(false)
        });
        let chegou = chegada.lock().expect("cadeado").clone();
        match (saiu, chegou) {
            (Some(s), Some((c, bytes))) => (Some(c.saturating_duration_since(s)), bytes),
            _ => (None, Vec::new()),
        }
    }

    /// **O espaçador espalha o quadro grande e o entrega inteiro** — e um quadro só basta.
    ///
    /// As duas metades pegam os dois modos de falhar. Com a guarda invertida da libdatachannel
    /// 0.23.2 (`vendor/datachannel-sys/QUALL-PATCH.md`), o primeiro quadro nunca era agendado e
    /// esperava o seguinte para sair: aqui, que só há um, ele não chegaria nunca. E um espaçador
    /// que não espaça chegaria no mesmo tempo que sem ele. A 4 Mbit/s, 100 KB são ~200 ms de
    /// verba; sem espaçador, o laço local os entrega em poucos ms.
    #[test]
    fn espacador_espalha_o_quadro_grande_e_entrega_inteiro() {
        let (sem, _) = tempo_ate_chegar(0, 100_000);
        let (com, chegou) = tempo_ate_chegar(4_000, 100_000);
        let sem = sem.expect("sem espaçador o quadro não chegou");
        let com = com.expect(
            "com espaçador o quadro não chegou: é a assinatura da guarda invertida, que segurava \
             o primeiro quadro até chegar outro",
        );
        assert_eq!(
            chegou,
            quadro_idr(100_000),
            "o quadro espaçado chegou diferente"
        );
        assert!(
            sem < Duration::from_millis(100),
            "sem espaçador levou {sem:?}: o laço local está lento demais para a comparação valer"
        );
        assert!(
            com >= Duration::from_millis(150),
            "com espaçador a 4 Mbit/s chegou em {com:?} (sem: {sem:?}); 100 KB não passam em \
             menos de ~200 ms"
        );
        assert!(
            com < Duration::from_millis(1500),
            "com espaçador a 4 Mbit/s levou {com:?}: o relógio do espaçador está cortando a taxa"
        );
    }

    /// **O item mais importante do M2**: o receptor pede IDR e o emissor fica sabendo.
    ///
    /// É a razão de o contrato ter escolhido track em vez de canal de dados. Sem isto, o Windows
    /// fica ~7 s sem imagem — o defeito medido no M1.
    #[test]
    fn pedido_de_idr_do_receptor_chega_no_emissor() {
        let cfg = TransportConfig::default();
        let (mut a, emissores) =
            Session::offerer_com_tracks(&cfg, &[TrackConfig::new(TrackKind::Screen, "Tela")])
                .expect("ofertante");
        let mut b = Session::answerer(&cfg).expect("respondente");
        let emissor = emissores.into_iter().next().expect("emissor");

        let pediu = Arc::new(AtomicU64::new(0));
        {
            let contador = Arc::clone(&pediu);
            emissor
                .ao_pedir_idr(move || {
                    contador.fetch_add(1, Ordering::Relaxed);
                })
                .expect("registrar o tratador de IDR");
        }

        let mut receptor: Option<TrackReceptor> = None;
        let conectou = negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            if receptor.is_none() {
                receptor = b.proxima_track(Duration::from_millis(1));
            }
        });
        assert!(conectou, "as duas pontas não conectaram dentro do prazo");
        let receptor = receptor.expect("a track não chegou");

        // O PLI só sai depois de o transporte estar de pé; tentar até conseguir é o que a casca
        // receptora faz de verdade ao entrar na sessão sem ter visto IDR.
        esperar(Duration::from_secs(20), || {
            let _ = receptor.pedir_idr();
            pediu.load(Ordering::Relaxed) > 0
        });

        assert!(
            pediu.load(Ordering::Relaxed) > 0,
            "o PLI do receptor não chegou ao emissor: `ao_pedir_idr` nunca disparou"
        );
        assert!(emissor.pedidos_de_idr() > 0);
    }

    /// Tela e câmera na mesma sessão, cada uma na sua linha `m=`.
    ///
    /// No iOS isso não é opção: a tela vem da Broadcast Upload Extension e a câmera vem do app
    /// principal — dois processos, duas tracks, uma sessão.
    #[test]
    fn duas_tracks_na_mesma_sessao_chegam_separadas() {
        let cfg = TransportConfig::default();
        let (mut a, emissores) = Session::offerer_com_tracks(
            &cfg,
            &[
                TrackConfig::new(TrackKind::Screen, "Tela"),
                TrackConfig::new(TrackKind::Camera, "Câmera"),
            ],
        )
        .expect("ofertante com duas tracks");
        let mut b = Session::answerer(&cfg).expect("respondente");
        assert_eq!(emissores.len(), 2);

        let mut chegadas: Vec<TrackReceptor> = Vec::new();
        negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            while let Some(t) = b.proxima_track(Duration::from_millis(1)) {
                chegadas.push(t);
            }
        });

        assert_eq!(
            chegadas.len(),
            2,
            "chegaram {} tracks, esperadas 2",
            chegadas.len()
        );
        let tipos: Vec<TrackKind> = chegadas.iter().map(|t| t.kind()).collect();
        assert!(tipos.contains(&TrackKind::Screen));
        assert!(tipos.contains(&TrackKind::Camera));
        assert_eq!(b.tracks_recusadas(), 0);
    }

    /// **Tela, câmera, áudio do sistema e microfone na mesma sessão.**
    ///
    /// Substituiu `track_de_audio_ainda_nao_e_implementada_e_diz_isso`, que fixava o
    /// comportamento antigo: `offerer_com_tracks` devolvia `Error::Invalid` para qualquer track
    /// de áudio.
    ///
    /// Cobre as **duas** espécies de áudio de uma vez, e é de propósito: elas percorrem o mesmo
    /// caminho e diferem só pelo preset, então um teste que exercitasse uma só deixaria a outra
    /// sem nenhuma prova de que atravessa.
    ///
    /// Confere o que uma contagem de tracks não pegaria: que cada track de áudio chega do outro
    /// lado **reconhecida como áudio**, com o codec lido do `a=rtpmap` do SDP e não presumido.
    /// Uma track de áudio anunciada por engano como vídeo passaria por toda a negociação sem
    /// erro nenhum — o receptor a adotaria, montaria um depacotizador de H.264 em cima de
    /// pacotes de Opus, e o defeito só apareceria como silêncio.
    #[test]
    fn tela_camera_e_as_duas_especies_de_audio_na_mesma_sessao() {
        let cfg = TransportConfig::default();
        let (mut a, emissores) = Session::offerer_com_tracks(
            &cfg,
            &[
                TrackConfig::new(TrackKind::Screen, "Tela"),
                TrackConfig::new(TrackKind::Camera, "Câmera"),
                TrackConfig::new(TrackKind::SystemAudio, "Som do sistema"),
                TrackConfig::new(TrackKind::Microphone, "Microfone"),
            ],
        )
        .expect("ofertante com quatro tracks");
        let mut b = Session::answerer(&cfg).expect("respondente");
        assert_eq!(emissores.len(), 4);

        // Do lado do emissor: exatamente as duas espécies de áudio têm preset, e os presets são
        // os da tabela — não um padrão único aplicado às duas.
        let com_audio: Vec<_> = emissores
            .iter()
            .filter(|e| e.preset_de_audio().is_some())
            .collect();
        assert_eq!(
            com_audio.len(),
            2,
            "só o som do sistema e o microfone são áudio"
        );

        let sistema = emissores
            .iter()
            .find(|e| e.kind() == TrackKind::SystemAudio)
            .expect("track de áudio do sistema");
        let microfone = emissores
            .iter()
            .find(|e| e.kind() == TrackKind::Microphone)
            .expect("track de microfone");
        assert_eq!(sistema.preset_de_audio(), Some(PRESET_AUDIO_DO_SISTEMA));
        assert_eq!(microfone.preset_de_audio(), Some(PRESET_MICROFONE));

        let mut chegadas: Vec<TrackReceptor> = Vec::new();
        negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            while let Some(t) = b.proxima_track(Duration::from_millis(1)) {
                chegadas.push(t);
            }
        });

        assert_eq!(
            chegadas.len(),
            4,
            "chegaram {} tracks, esperadas 4",
            chegadas.len()
        );
        assert_eq!(
            b.tracks_recusadas(),
            0,
            "alguma track foi recusada na adoção — provavelmente uma de áudio, por rtpmap"
        );

        for especie in [TrackKind::SystemAudio, TrackKind::Microphone] {
            let t = chegadas
                .iter()
                .find(|t| t.kind() == especie)
                .unwrap_or_else(|| panic!("a track {especie:?} tem de chegar"));
            assert_eq!(
                t.codec_de_audio(),
                Some(CodecDeAudio::Opus),
                "{especie:?}: o codec tem de vir do `a=rtpmap` do SDP do outro lado"
            );
        }

        // E as de vídeo continuam sem codec de áudio: a detecção não pode vazar de um lado
        // para o outro dentro da mesma sessão.
        for t in chegadas.iter().filter(|t| t.kind().e_video()) {
            assert_eq!(t.codec_de_audio(), None, "track de vídeo virou áudio");
        }
    }

    /// Chamar a função errada para o tipo da track é erro, e o erro diz qual usar.
    #[test]
    fn as_duas_funcoes_de_envio_nao_se_confundem() {
        let cfg = TransportConfig::default();
        let (_a, emissores) = Session::offerer_com_tracks(
            &cfg,
            &[
                TrackConfig::new(TrackKind::Screen, "Tela"),
                TrackConfig::new(TrackKind::SystemAudio, "Som do sistema"),
            ],
        )
        .expect("ofertante");

        let tela = &emissores[0];
        let microfone = &emissores[1];

        // Áudio numa track de vídeo.
        assert!(matches!(
            tela.enviar_audio(AmostraDeAudio {
                payload: &[0xfc, 0xff, 0xfe],
                timestamp_us: 0,
            }),
            Err(Error::Invalid(_))
        ));

        // Vídeo numa track de áudio.
        assert!(matches!(
            microfone.enviar_quadro(QuadroCodificado {
                annexb: &[0, 0, 0, 1, 0x65, 0x88],
                timestamp_us: 0,
                idr: true,
            }),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn enviar_antes_de_abrir_e_erro_e_nao_panico() {
        let a = Session::answerer(&TransportConfig::default()).expect("sessão");
        let erro = a.send(b"cedo demais").expect_err("tem de falhar");
        assert!(matches!(erro, Error::Transport(_)));
    }

    #[test]
    fn sdp_de_tipo_desconhecido_e_recusado() {
        let mut a = Session::answerer(&TransportConfig::default()).expect("sessão");
        assert!(a.set_remote_description("bolinho", "v=0\r\n").is_err());
    }

    /// **Dívidas 4, 14 e 21.** Uma sessão que morre não pode deixar a `Track` presa nos mapas
    /// globais da libdatachannel.
    ///
    /// # Como se prova isso sem travar o Windows
    ///
    /// A primeira versão deste teste perguntava à libdatachannel se o id ainda existia, chamando
    /// `rtcGetTrackMid` depois do `Drop`. Funcionava no macOS e no Android e **travava o
    /// processo no Windows** — ver [`quall_rtc::TRACKS_DESTRUIDAS`], que carrega o mecanismo.
    ///
    /// A prova agora é pelo outro lado: `rtcDeleteTrack` devolve sucesso **apenas** quando o id
    /// estava no mapa global e saiu dele. Contar os sucessos prova a mesma coisa — a entrada
    /// existia e foi removida — sem nunca tocar num id morto.
    #[test]
    fn sessao_destruida_nao_deixa_track_viva_na_libdatachannel() {
        let cfg = TransportConfig::default();
        let (sessao, emissores) = Session::offerer_com_tracks(
            &cfg,
            &[
                TrackConfig::new(TrackKind::Screen, "Tela"),
                TrackConfig::new(TrackKind::Camera, "Câmera"),
            ],
        )
        .expect("ofertante com duas tracks");
        assert!(
            emissores.iter().all(|e| e.esta_viva()),
            "as tracks tinham de estar vivas enquanto a sessão vive"
        );

        // A casca larga os handles dela primeiro, como manda o contrato da fronteira C.
        let guardado = Arc::new(emissores.into_iter().next().expect("um emissor"));
        let fechadas = sessao.contador_de_fechadas();
        drop(sessao);

        let destruidas = fechadas.load(Ordering::Relaxed);
        assert_eq!(
            destruidas, 2,
            "a sessão tinha de apagar as 2 tracks dos mapas globais, apagou {destruidas}"
        );

        // E o handle que a casca ainda segura precisa **responder**, não travar.
        assert!(!guardado.esta_viva());
        assert_eq!(guardado.pendente(), 0);
        assert!(guardado
            .enviar_quadro(QuadroCodificado {
                annexb: &[0, 0, 0, 1, 0x65],
                timestamp_us: 0,
                idr: true,
            })
            .is_err());
    }

    /// **O lado que a dívida 4 não mencionava, e que é o pior dos dois.**
    ///
    /// `Session::tracks` recebe também as tracks que **chegam** (ver `ao_chegar_track` em
    /// [`Session::montar`]), não só as declaradas na oferta. Então o receptor vazava igual — e o
    /// receptor de desktop é justamente o processo que fica horas aberto, com o plugin de OBS ou
    /// a câmera virtual reconectando sessão atrás de sessão.
    #[test]
    fn o_receptor_tambem_nao_deixa_track_viva() {
        let cfg = TransportConfig::default();
        let (mut a, _emissores) = Session::offerer_com_tracks(
            &cfg,
            &[
                TrackConfig::new(TrackKind::Screen, "Tela"),
                TrackConfig::new(TrackKind::Camera, "Câmera"),
            ],
        )
        .expect("ofertante");
        let mut b = Session::answerer(&cfg).expect("respondente");

        let mut chegadas: Vec<TrackReceptor> = Vec::new();
        negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            while let Some(t) = b.proxima_track(Duration::from_millis(1)) {
                chegadas.push(t);
            }
        });
        assert_eq!(chegadas.len(), 2, "as tracks não chegaram ao receptor");
        assert!(chegadas.iter().all(|t| t.esta_viva()));

        // O handle sobrevive à sessão, como na fronteira C.
        let guardado = chegadas.into_iter().next().expect("uma track");
        let fechadas = b.contador_de_fechadas();
        drop(b);

        let destruidas = fechadas.load(Ordering::Relaxed);
        assert_eq!(
            destruidas, 2,
            "o receptor tinha de apagar as 2 tracks de entrada, apagou {destruidas}"
        );
        assert!(!guardado.esta_viva());
        assert!(guardado.pedir_idr().is_err(), "pedir IDR numa sessão morta");
    }

    /// O mesmo pela porta que o produto usa de verdade: uma tentativa que **não fecha o ICE**.
    ///
    /// É o caso da dívida 21 — pareamento fecha, ICE não —, e é onde o vazamento era cobrado por
    /// tentativa. Aqui as sessões nem chegam a negociar: nascem e morrem, como numa tentativa que
    /// estourou o prazo.
    #[test]
    fn tentativa_que_nao_fecha_o_ice_nao_acumula_track() {
        let cfg = TransportConfig::default();
        let tentativas = 5u64;
        let mut destruidas = 0u64;
        for _ in 0..tentativas {
            let (sessao, emissores) =
                Session::offerer_com_tracks(&cfg, &[TrackConfig::new(TrackKind::Screen, "Tela")])
                    .expect("ofertante");
            let fechadas = sessao.contador_de_fechadas();
            drop(emissores);
            drop(sessao);
            destruidas += fechadas.load(Ordering::Relaxed);
        }
        assert_eq!(
            destruidas, tentativas,
            "{tentativas} tentativas tinham de apagar {tentativas} tracks, apagaram {destruidas}"
        );
    }

    /// Monta uma sessão de verdade entre duas pontas e devolve o emissor e o receptor já
    /// conectados. É o preâmbulo de todos os testes de barreira daqui para baixo.
    ///
    /// Devolve `(ofertante, respondente, emissor, receptor)`. As duas `Session` precisam
    /// continuar vivas: destruí-las é o que os testes vão medir.
    fn sessao_conectada_com_track() -> (Session, Session, TrackEmissor, TrackReceptor) {
        let cfg = TransportConfig::default();
        let (mut a, emissores) =
            Session::offerer_com_tracks(&cfg, &[TrackConfig::new(TrackKind::Screen, "Tela")])
                .expect("ofertante");
        let mut b = Session::answerer(&cfg).expect("respondente");
        let emissor = emissores.into_iter().next().expect("emissor");

        let mut receptor: Option<TrackReceptor> = None;
        let conectou = negociar(&mut a, &mut b, Duration::from_secs(20), |_, b| {
            if receptor.is_none() {
                receptor = b.proxima_track(Duration::from_millis(1));
            }
        });
        assert!(conectou, "as duas pontas não conectaram dentro do prazo");
        let receptor = receptor.expect("a track não chegou ao receptor");
        (a, b, emissor, receptor)
    }

    /// Uma casca de mentira: conta quantas threads estão **dentro** do tratador dela agora.
    ///
    /// `dentro` é o número que interessa. Quando `quall_session_close` volta, é nele que a casca
    /// libera o `user_data`; qualquer valor diferente de zero ali é uso-após-liberação.
    ///
    /// # Por que o tratador espera uma ordem em vez de dormir um tanto
    ///
    /// A primeira versão dormia 60 ms e **passava sem o conserto**: destruir a
    /// `PeerConnection` já leva mais que isso, então o tratador saía sozinho antes de o
    /// `drop` voltar e o teste não distinguia barreira de coincidência. Aqui o tratador só sai
    /// quando o teste manda, e quem manda é uma terceira thread com prazo conhecido — assim a
    /// única forma de `dentro` ser zero no retorno é o fechamento ter **esperado**.
    struct CascaDeMentira {
        entrou: AtomicU64,
        dentro: AtomicU64,
        solta: AtomicBool,
    }

    impl CascaDeMentira {
        fn nova() -> Arc<Self> {
            Arc::new(CascaDeMentira {
                entrou: AtomicU64::new(0),
                dentro: AtomicU64::new(0),
                solta: AtomicBool::new(false),
            })
        }

        /// O que o tratador da casca faz: entra, fica lá o tempo que o teste mandar, e sai.
        fn tratar(&self) {
            self.entrou.fetch_add(1, Ordering::SeqCst);
            self.dentro.fetch_add(1, Ordering::SeqCst);
            while !self.solta.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            self.dentro.fetch_sub(1, Ordering::SeqCst);
        }

        /// Solta o tratador daqui a `demora`, de outra thread.
        fn soltar_depois(self: &Arc<Self>, demora: Duration) -> std::thread::JoinHandle<()> {
            let eu = Arc::clone(self);
            std::thread::spawn(move || {
                std::thread::sleep(demora);
                eu.solta.store(true, Ordering::SeqCst);
            })
        }
    }

    /// Uma thread de captura despejando quadros, como o laço do MediaCodec ou do VideoToolbox.
    ///
    /// Devolve a bandeira de parar e a alça da thread; o teste é dono dos dois.
    fn despejar_quadros(emissor: TrackEmissor) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let parar = Arc::new(AtomicBool::new(false));
        let bandeira = Arc::clone(&parar);
        let alca = std::thread::spawn(move || {
            let quadro = quadro_idr(4096);
            while !bandeira.load(Ordering::Relaxed) {
                let _ = emissor.enviar_quadro(QuadroCodificado {
                    annexb: &quadro,
                    timestamp_us: 0,
                    idr: true,
                });
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        (parar, alca)
    }

    /// **A metade que faltava da dívida do `quall_session_close`: ele não era barreira.**
    ///
    /// Este é o caso adversarial pedido: o tratador da casca disparando numa thread da
    /// libdatachannel **exatamente durante** o fechamento. O tratador entra e fica lá; o
    /// fechamento é disparado com ele comprovadamente dentro; uma terceira thread o solta 2 s
    /// depois.
    ///
    /// # Por que o pedido de IDR, e não o quadro recebido
    ///
    /// Porque é o caminho onde o defeito existia de verdade. Medido aqui em 2026-08-26, no
    /// MacBook Air M4, com a mesma armação dos dois lados:
    ///
    /// | caminho | o `Drop` voltava em | tratador ainda dentro? |
    /// |---|---|---|
    /// | quadro recebido | 2,00 s | não |
    /// | **pedido de IDR** | **0,49 ms** | **sim** |
    ///
    /// A primeira linha nunca foi mérito nosso: `rtcDeleteTrack` espera o despacho de mensagem
    /// da própria libdatachannel terminar. É detalhe de implementação dela, num sistema
    /// operacional só, e o header prometia uma coisa que só valia por acaso. A segunda linha é
    /// o defeito no estado puro — e é o lado do **emissor**, o celular espelhando e a fonte do
    /// plugin de OBS.
    ///
    /// Depois do portão: 2,00 s e ninguém dentro, nos dois caminhos.
    #[test]
    fn fechar_a_sessao_espera_o_tratador_de_idr_sair_do_codigo_da_casca() {
        let (a, _b, emissor, receptor) = sessao_conectada_com_track();

        let casca = CascaDeMentira::nova();
        {
            let casca = Arc::clone(&casca);
            emissor
                .ao_pedir_idr(move || casca.tratar())
                .expect("registrar o tratador de IDR");
        }

        // O receptor pedindo IDR em laço, como a casca receptora faz ao entrar na sessão sem
        // ter visto imagem.
        let parar = Arc::new(AtomicBool::new(false));
        let bandeira = Arc::clone(&parar);
        let pedindo = std::thread::spawn(move || {
            while !bandeira.load(Ordering::Relaxed) {
                let _ = receptor.pedir_idr();
                std::thread::sleep(Duration::from_millis(20));
            }
        });

        assert!(
            esperar(Duration::from_secs(20), || casca
                .dentro
                .load(Ordering::SeqCst)
                > 0),
            "o tratador de IDR não chegou a entrar no código da casca"
        );
        // Bem abaixo de `PRAZO_DA_BARREIRA`: assim, voltar com o portão vazio só pode ter
        // sido espera, e nunca prazo estourado.
        let soltador = casca.soltar_depois(Duration::from_millis(800));

        let comecou = std::time::Instant::now();
        drop(a);
        let levou = comecou.elapsed();

        assert_eq!(
            casca.dentro.load(Ordering::SeqCst),
            0,
            "fechar a sessão voltou em {levou:?} com o tratador ainda dentro do código da \
             casca — é exatamente aí que a casca libera o `user_data`"
        );

        parar.store(true, Ordering::Relaxed);
        let _ = soltador.join();
        let _ = pedindo.join();
    }

    /// O mesmo contrato pelo caminho do quadro recebido.
    ///
    /// **Este passava antes do conserto**, e é honesto dizer: quem esperava era o
    /// `rtcDeleteTrack` da libdatachannel, não o Quall. Fica como guarda do contrato — se um dia
    /// a libdatachannel deixar de esperar, ou se o Windows nunca tiver esperado, é aqui que
    /// aparece, e agora a garantia é nossa.
    #[test]
    fn fechar_a_sessao_espera_o_tratador_de_quadro_sair_do_codigo_da_casca() {
        let (_a, b, emissor, receptor) = sessao_conectada_com_track();

        let casca = CascaDeMentira::nova();
        {
            let casca = Arc::clone(&casca);
            receptor.ao_receber_quadro(move |_q| casca.tratar());
        }
        let (parar, capturando) = despejar_quadros(emissor);

        assert!(
            esperar(Duration::from_secs(20), || casca
                .dentro
                .load(Ordering::SeqCst)
                > 0),
            "o tratador não chegou a entrar no código da casca"
        );
        // Bem abaixo de `PRAZO_DA_BARREIRA`: assim, voltar com o portão vazio só pode ter
        // sido espera, e nunca prazo estourado.
        let soltador = casca.soltar_depois(Duration::from_millis(800));

        let comecou = std::time::Instant::now();
        drop(b);
        let levou = comecou.elapsed();

        assert_eq!(
            casca.dentro.load(Ordering::SeqCst),
            0,
            "fechar a sessão voltou em {levou:?} com um tratador ainda dentro do código da \
             casca — é aqui que a casca libera o `user_data`"
        );
        let _ = soltador.join();

        // E nenhum tratador dispara depois disso.
        let ate_aqui = casca.entrou.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            casca.entrou.load(Ordering::SeqCst),
            ate_aqui,
            "o tratador da casca disparou depois de a sessão fechar"
        );

        parar.store(true, Ordering::Relaxed);
        let _ = capturando.join();
    }

    /// **Desregistro com barreira, com a sessão ainda de pé.**
    ///
    /// É o caso do plugin de OBS que remove uma fonte sem derrubar a sessão: ele precisa poder
    /// largar o `user_data` daquela fonte. Antes disto não havia como — `quall_track_free`
    /// soltava só a referência da casca e o tratador continuava armado.
    #[test]
    fn desregistrar_o_tratador_de_quadro_e_barreira_e_nao_so_promessa() {
        let (_a, _b, emissor, receptor) = sessao_conectada_com_track();

        let casca = CascaDeMentira::nova();
        {
            let casca = Arc::clone(&casca);
            receptor.ao_receber_quadro(move |_q| casca.tratar());
        }
        let (parar, capturando) = despejar_quadros(emissor);

        assert!(
            esperar(Duration::from_secs(20), || casca
                .dentro
                .load(Ordering::SeqCst)
                > 0),
            "o tratador não chegou a entrar no código da casca"
        );
        let soltador = casca.soltar_depois(Duration::from_millis(600));

        assert_eq!(
            receptor.desregistrar_quadro(),
            Barreira::Cumprida,
            "o desregistro não conseguiu provar a barreira"
        );
        assert_eq!(
            casca.dentro.load(Ordering::SeqCst),
            0,
            "o desregistro voltou com o tratador antigo ainda rodando"
        );

        // E o tratador antigo não volta a disparar, mesmo com a sessão viva e quadros chegando.
        let ate_aqui = casca.entrou.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            casca.entrou.load(Ordering::SeqCst),
            ate_aqui,
            "o tratador desregistrado continuou sendo chamado"
        );
        // A sessão continua de pé: os contadores da track seguem andando.
        assert!(receptor.esta_viva());

        parar.store(true, Ordering::Relaxed);
        let _ = soltador.join();
        let _ = capturando.join();
    }

    /// O mesmo do lado do emissor, para o tratador de IDR.
    #[test]
    fn desregistrar_o_tratador_de_idr_para_de_chamar_a_casca() {
        let (_a, _b, emissor, receptor) = sessao_conectada_com_track();

        let chamou = Arc::new(AtomicU64::new(0));
        {
            let chamou = Arc::clone(&chamou);
            emissor
                .ao_pedir_idr(move || {
                    chamou.fetch_add(1, Ordering::SeqCst);
                })
                .expect("registrar");
        }

        assert!(
            esperar(Duration::from_secs(20), || {
                let _ = receptor.pedir_idr();
                chamou.load(Ordering::SeqCst) > 0
            }),
            "o PLI do receptor não chegou ao emissor"
        );

        assert_eq!(emissor.desregistrar_idr(), Barreira::Cumprida);
        let ate_aqui = chamou.load(Ordering::SeqCst);

        // Mais PLI, e o tratador da casca não pode mais ser chamado...
        for _ in 0..20 {
            let _ = receptor.pedir_idr();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            chamou.load(Ordering::SeqCst),
            ate_aqui,
            "o tratador desregistrado continuou sendo chamado"
        );
        // ... mas o contador e a bandeira continuam, que é o que a casca Android usa.
        assert!(emissor.pedidos_de_idr() > 0);
    }

    /// **Reentrância pela porta do produto**: a casca desregistra de dentro do próprio tratador.
    ///
    /// Não pode pendurar. A barreira não vale nesse caso — e dizer isso é a resposta certa.
    #[test]
    fn desregistrar_de_dentro_do_proprio_tratador_nao_pendura() {
        let (_a, _b, emissor, receptor) = sessao_conectada_com_track();

        let receptor = Arc::new(receptor);
        let visto = Arc::new(Mutex::new(None::<Barreira>));
        {
            let (r, visto) = (Arc::clone(&receptor), Arc::clone(&visto));
            receptor.ao_receber_quadro(move |_q| {
                let b = r.desregistrar_quadro();
                if let Ok(mut g) = visto.lock() {
                    g.get_or_insert(b);
                }
            });
        }
        let (parar, capturando) = despejar_quadros(emissor);

        assert!(
            esperar(Duration::from_secs(20), || visto
                .lock()
                .map(|g| g.is_some())
                .unwrap_or(false)),
            "o tratador não rodou, ou pendurou ao desregistrar a si mesmo"
        );
        assert_eq!(
            visto.lock().expect("cadeado").take(),
            Some(Barreira::DeDentroDoTratador),
            "desregistrar de dentro do tratador tem de recusar a barreira, não prometê-la"
        );

        parar.store(true, Ordering::Relaxed);
        let _ = capturando.join();
    }

    #[test]
    fn configuracao_nao_tem_servidor_ice() {
        // Guarda contra alguém "consertar" a conectividade acrescentando um STUN público —
        // que quebraria a promessa de zero servidor online sem quebrar nenhum teste.
        let cfg = TransportConfig::default().to_rtc().expect("configuração");
        assert!(
            cfg.ice_servers.is_empty(),
            "apareceu servidor ICE na configuração: {:?}",
            cfg.ice_servers
        );
    }

    /// **O teste mais importante do `bind_address`.**
    ///
    /// `RtcConfig::bind_address` faz `CString::new(...).unwrap()`
    /// (`datachannel-0.16.1/src/config.rs:56`). Com `panic = "abort"` no perfil de release desta
    /// árvore (`Cargo.toml:91`), um NUL no meio da string **não devolve erro: mata o processo**.
    /// Um endereço vem de campo de texto de casca, e um `\0` colado ali não pode derrubar o app
    /// do usuário.
    ///
    /// O que este teste prova é que a validação acontece **antes** de a string atravessar: se um
    /// dia alguém tirar a conferência, o `to_rtc` deixa de devolver `Err` — e em release aborta.
    /// Ele roda em debug, onde o `unwrap` desenrolaria em vez de abortar; o que ele fixa é o
    /// caminho, e o caminho é o mesmo nos dois perfis.
    #[test]
    fn bind_address_com_nul_vira_erro_em_vez_de_derrubar_o_processo() {
        let cfg = TransportConfig {
            bind_address: Some("169.254\u{0}.75.173".to_string()),
            ..TransportConfig::default()
        };
        match cfg.to_rtc() {
            Err(Error::Invalid(msg)) => {
                assert!(
                    msg.contains("NUL"),
                    "o motivo tem de nomear o NUL, senão ninguém conserta: {msg}"
                );
            }
            Err(outro) => panic!("erro errado: esperado Invalid, veio {outro:?}"),
            Ok(_) => panic!(
                "um NUL interno passou para a libdatachannel — em release isso ABORTA o processo"
            ),
        }
    }

    #[test]
    fn bind_address_vazio_e_recusado() {
        // Vazio não é "todas as interfaces": isso é `None`. Deixar passar mandaria à libjuice um
        // pedido sem sentido, e o sintoma seria uma sessão que não fecha sem dizer por quê.
        let cfg = TransportConfig {
            bind_address: Some(String::new()),
            ..TransportConfig::default()
        };
        assert!(
            matches!(cfg.to_rtc(), Err(Error::Invalid(_))),
            "bind_address vazio devia ser recusado"
        );
    }

    #[test]
    fn bind_address_valido_chega_na_configuracao_e_o_padrao_nao_liga_nada() {
        // O padrão continua reunindo todas as interfaces: nenhuma sessão existente muda de
        // comportamento por este campo passar a existir.
        let padrao = TransportConfig::default().to_rtc().expect("configuração");
        assert!(
            padrao.bind_address.is_none(),
            "o padrão não pode prender a interface nenhuma"
        );

        let cfg = TransportConfig {
            bind_address: Some("169.254.75.173".to_string()),
            ..TransportConfig::default()
        };
        let rtc = cfg.to_rtc().expect("configuração");
        assert_eq!(
            rtc.bind_address.as_ref().map(|c| c.to_bytes()),
            Some(&b"169.254.75.173"[..]),
            "o endereço não chegou à configuração da libdatachannel"
        );
    }

    /// O caso `null` do [`CaminhoDaMidia`], e ele importa tanto quanto o preenchido: antes de o
    /// ICE fechar não há par escolhido, e isso é "ainda não", não "falhou".
    #[test]
    fn caminho_da_midia_e_nulo_antes_de_o_ice_fechar() {
        let sessao = Session::offerer(&TransportConfig::default()).expect("sessão");
        let caminho = sessao.caminho();
        assert!(!caminho.fechado(), "ICE fechou sem par nenhum do outro lado");
        assert_eq!(
            serde_json::to_string(&caminho).expect("json"),
            r#"{"local_candidate":null,"remote_candidate":null,"local_address":null,"remote_address":null}"#,
            "é este JSON que a casca vê enquanto a sessão sobe; mudá-lo quebra quem o lê"
        );
    }

    /// O batimento nasce sem silêncio a medir — e é isso que impede o detector de queda de
    /// armar num emissor, que nunca recebe nada.
    #[test]
    fn batimento_so_mede_silencio_depois_de_bater_uma_vez() {
        let b = Batimento::novo();
        assert_eq!(b.silencio(), None, "nada chegou ainda: não há o que medir");
        b.bater();
        let primeiro = b.silencio().expect("bateu, logo há silêncio a medir");
        assert!(
            primeiro < Duration::from_secs(1),
            "silêncio absurdo logo depois de bater: {primeiro:?}"
        );
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            b.silencio().expect("continua batido") >= Duration::from_millis(25),
            "o silêncio não anda com o relógio"
        );
        b.bater();
        assert!(
            b.silencio().expect("batido de novo") < Duration::from_millis(25),
            "bater de novo tem de zerar a conta"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Mensagens de aplicação (F6a)
    // -----------------------------------------------------------------------------------------

    /// Duas pontas conectadas, com o canal de dados **aberto dos dois lados**, com a entrega que
    /// quem oferece escolheu.
    fn par_com_canal(entrega: Delivery) -> (Session, Session) {
        let cfg = TransportConfig { delivery: entrega, ..TransportConfig::default() };
        let mut a = Session::offerer(&cfg).expect("ofertante");
        // Quem responde fica com o padrão de propósito: a entrega dele não decide nada.
        let mut b = Session::answerer(&TransportConfig::default()).expect("respondente");
        assert!(negociar(&mut a, &mut b, Duration::from_secs(20), |_, _| {}), "não conectou");
        assert!(
            esperar(Duration::from_secs(10), || {
                a.estado_do_canal.load(Ordering::Acquire) == CANAL_ABERTO
                    && b.estado_do_canal.load(Ordering::Acquire) == CANAL_ABERTO
            }),
            "o canal de dados não abriu nos dois lados"
        );
        (a, b)
    }

    fn receber_ate(m: &Mensageiro, n: usize, prazo: Duration) -> Vec<String> {
        let fim = std::time::Instant::now() + prazo;
        let mut v = Vec::new();
        while v.len() < n && std::time::Instant::now() < fim {
            if let Ok(Some(t)) = m.proxima(Duration::from_millis(50)) {
                v.push(t);
            }
        }
        v
    }

    /// **As mensagens atravessam nos dois sentidos**, e **quem responde adota a confiabilidade de
    /// quem oferece**: o respondente nasceu com `Realtime` no `TransportConfig` e o canal dele é
    /// confiável e sem ordem, porque o ofertante escolheu assim (`datachannel.cpp:329-381`).
    #[test]
    fn mensagens_atravessam_nos_dois_sentidos_e_quem_responde_adota_a_entrega() {
        let (a, b) = par_com_canal(Delivery::ReliableUnordered);
        assert_eq!(a.entrega_do_canal(), Some(Delivery::ReliableUnordered));
        assert_eq!(
            b.entrega_do_canal(),
            Some(Delivery::ReliableUnordered),
            "quem responde tem de adotar a entrega de quem oferece"
        );
        let (ma, mb) = (a.mensageiro(), b.mensageiro());
        assert_ne!(ma.sessao(), mb.sessao(), "cada sessão tem o seu número");
        for i in 0..20 {
            ma.enviar(&format!("de a {i}")).expect("a manda");
            mb.enviar(&format!("de b {i}, com acento: ção")).expect("b manda");
        }
        let em_b = receber_ate(&mb, 20, Duration::from_secs(10));
        let em_a = receber_ate(&ma, 20, Duration::from_secs(10));
        assert_eq!(em_b.len(), 20, "b recebeu {em_b:?}");
        assert_eq!(em_a.len(), 20, "a recebeu {em_a:?}");
        let mut esperado_b: Vec<String> = (0..20).map(|i| format!("de a {i}")).collect();
        let mut recebido_b = em_b.clone();
        esperado_b.sort();
        recebido_b.sort();
        assert_eq!(recebido_b, esperado_b, "sem ordem, mas todas, e iguais");
        assert!(em_a.iter().all(|t| t.ends_with("ção")));
        assert_eq!(mb.contadores().recebidas, 20);
        assert_eq!(ma.contadores().enviadas, 20);
    }

    /// **As sessões de vídeo não mudam**: sem pedir, o canal continua sem retransmissão, dos dois
    /// lados — e a fila continua descartando a que chega.
    #[test]
    fn sessao_de_video_continua_sem_retransmissao() {
        let (a, b) = par_com_canal(Delivery::Realtime);
        assert_eq!(a.entrega_do_canal(), Some(Delivery::Realtime));
        assert_eq!(b.entrega_do_canal(), Some(Delivery::Realtime));
        assert_eq!(b.fila.politica(), QuandoEnche::DescartaANova);
        assert_eq!(TransportConfig::default().delivery, Delivery::Realtime);
    }

    /// Vazia, com NUL, ou acima do teto: recusada **antes** da biblioteca. Exatamente no teto:
    /// atravessa inteira (256 KiB, num canal confiável).
    #[test]
    fn os_tetos_da_mensagem_sao_conferidos_antes_da_biblioteca() {
        let (a, b) = par_com_canal(Delivery::ReliableUnordered);
        let (ma, mb) = (a.mensageiro(), b.mensageiro());
        assert!(matches!(ma.enviar(""), Err(Error::Invalid(_))), "vazia");
        assert!(matches!(ma.enviar("a\0b"), Err(Error::Invalid(_))), "NUL");
        assert_eq!(ma.teto(), TETO_DA_MENSAGEM, "entre dois Quall, o teto é o de 256 KiB");
        let grande = "x".repeat(TETO_DA_MENSAGEM + 1);
        assert!(matches!(ma.enviar(&grande), Err(Error::Invalid(_))), "acima do teto");
        assert_eq!(ma.contadores().enviadas, 0, "nada disso pode ter saído");
        let no_teto: String =
            (0..TETO_DA_MENSAGEM).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        ma.enviar(&no_teto).expect("exatamente no teto passa");
        let chegou = receber_ate(&mb, 1, Duration::from_secs(10));
        assert_eq!(chegou.len(), 1, "a mensagem de 256 KiB não chegou");
        assert_eq!(chegou[0], no_teto, "chegou diferente");
    }

    /// **O teto é o da biblioteca, medido**: um byte a mais que o nosso, mandado cru (sem a nossa
    /// conferência), a biblioteca recusa. Fora do Windows: lá a recusa é uma exceção dentro de um
    /// cadeado da API C, e a regra de plataforma é não provocá-la.
    #[cfg(not(windows))]
    #[test]
    fn o_teto_da_mensagem_e_o_da_biblioteca() {
        let (a, b) = par_com_canal(Delivery::ReliableUnordered);
        assert!(
            a.send_sem_teto(&vec![b'y'; TETO_DA_MENSAGEM + 1]).is_err(),
            "a biblioteca aceitou mais que o teto"
        );
        assert!(a.send_sem_teto(&vec![b'y'; TETO_DA_MENSAGEM]).is_ok());
        let mb = b.mensageiro();
        assert_eq!(receber_ate(&mb, 1, Duration::from_secs(10)).len(), 1);
    }

    #[test]
    fn o_teto_vem_do_sdp_do_outro_lado() {
        assert_eq!(teto_do_sdp("v=0\r\na=max-message-size:262144\r\n"), TETO_DA_MENSAGEM);
        assert_eq!(teto_do_sdp("a=max-message-size:1000\n"), 1000);
        assert_eq!(teto_do_sdp("a=max-message-size:0\n"), TETO_DA_MENSAGEM, "0 é qualquer tamanho");
        assert_eq!(teto_do_sdp("v=0\n"), 65_536, "sem o atributo, o que a biblioteca supõe");
    }

    /// **Zero byte atravessa o transporte** (`sctptransport.cpp:607-610`) — e o mensageiro não o
    /// entrega a ninguém: descarta e conta, como o que não é UTF-8.
    #[test]
    fn zero_byte_e_binario_que_chegam_sao_descartados_e_contados() {
        let (a, b) = par_com_canal(Delivery::ReliableUnordered);
        let mb = b.mensageiro();
        a.send(&[]).expect("zero byte cru");
        a.send(&[0xff, 0xfe, 0x00]).expect("binário cru");
        a.send(b"depois").expect("uma mensagem de verdade");
        let chegou = receber_ate(&mb, 1, Duration::from_secs(10));
        assert_eq!(chegou, vec!["depois".to_string()], "o vazio e o binário não podem aparecer");
        assert_eq!(mb.contadores().descartadas_invalidas, 2);
    }

    /// **Fila cheia num canal confiável: guarda a mais nova.** 100 mensagens sem ninguém ler:
    /// ficam as 64 últimas, 36 são descartadas e contadas, e a última é das que ficaram — era a
    /// que se perdia com o `try_send`.
    #[test]
    fn fila_cheia_no_canal_confiavel_guarda_a_mais_nova() {
        let (a, b) = par_com_canal(Delivery::ReliableUnordered);
        assert_eq!(b.fila.politica(), QuandoEnche::DescartaAVelha, "quem responde trocou a regra");
        let (ma, mb) = (a.mensageiro(), b.mensageiro());
        for i in 0..100 {
            ma.enviar(&format!("m{i:03}")).expect("manda");
        }
        assert!(
            esperar(Duration::from_secs(10), || mb.contadores().descartadas_fila_cheia == 36),
            "descartadas: {}",
            mb.contadores().descartadas_fila_cheia
        );
        let ficou = receber_ate(&mb, 64, Duration::from_secs(5));
        assert_eq!(ficou.len(), 64);
        assert!(ficou.contains(&"m099".to_string()), "a mais nova foi descartada: {ficou:?}");
        assert!(!ficou.contains(&"m000".to_string()), "a mais velha devia ter saído");
        assert!(mb.proxima(Duration::from_millis(100)).expect("nada").is_none());
    }

    /// **Fila cheia no canal de vídeo: descarta a que chega**, como sempre fez.
    #[test]
    fn fila_cheia_no_canal_de_video_descarta_a_que_chega() {
        let (a, b) = par_com_canal(Delivery::Realtime);
        let mb = b.mensageiro();
        for i in 0..100 {
            // Devagar o bastante para o laço local não perder nada sem retransmissão.
            a.send(format!("m{i:03}").as_bytes()).expect("manda");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(esperar(Duration::from_secs(10), || b.dropped_frames() >= 30), "{}", b.dropped_frames());
        let ficou = receber_ate(&mb, 64, Duration::from_secs(5));
        assert!(ficou.contains(&"m000".to_string()), "a primeira tinha de ficar: {ficou:?}");
        assert!(!ficou.contains(&"m099".to_string()), "a última tinha de ser descartada");
    }

    /// **A espiada não consome** até a mensagem ser aceita — e a vaga é da sessão: o que um
    /// mensageiro espiou e não aceitou é o primeiro para qualquer outro.
    #[test]
    fn a_espiada_nao_consome_e_a_vaga_e_da_sessao() {
        let (a, b) = par_com_canal(Delivery::ReliableUnordered);
        let (ma, m1, m2) = (a.mensageiro(), b.mensageiro(), b.mensageiro());
        ma.enviar("primeira").expect("manda");
        ma.enviar("segunda").expect("manda");
        let mut vista = String::new();
        let t = m1
            .entregar_se(Duration::from_secs(10), |x| {
                vista = x.to_string();
                false
            })
            .expect("não fechou");
        assert_eq!(t, Some(vista.len()), "devolve o tamanho da que mostrou");
        let t2 = m2.entregar_se(Duration::ZERO, |_| false).expect("não fechou");
        assert_eq!(t2, Some(vista.len()), "outro mensageiro vê a mesma");
        assert_eq!(m2.proxima(Duration::ZERO).expect("não fechou"), Some(vista.clone()));
        let resto = receber_ate(&m1, 1, Duration::from_secs(10));
        assert_eq!(resto.len(), 1);
        assert_ne!(resto[0], vista);
    }

    /// **Chamada depois do fim da sessão**: mandar devolve `Closed` sem chegar à API C, ler
    /// entrega o que já tinha chegado e depois `Closed`, e nada espera o prazo.
    #[test]
    fn o_mensageiro_depois_do_fim_da_sessao() {
        let (a, b) = par_com_canal(Delivery::ReliableUnordered);
        let (ma, mb) = (a.mensageiro(), b.mensageiro());
        ma.enviar("antes do fim").expect("manda");
        assert!(esperar(Duration::from_secs(10), || b
            .fila
            .estado
            .lock()
            .map(|e| !e.itens.is_empty())
            .unwrap_or(false)));
        drop(a);
        drop(b);
        assert!(matches!(ma.enviar("depois"), Err(Error::Closed)), "mandar depois do fim");
        assert!(matches!(mb.enviar("depois"), Err(Error::Closed)));
        assert_eq!(ma.pendente(), 0);
        assert_eq!(
            mb.proxima(Duration::from_secs(1)).expect("o que chegou antes"),
            Some("antes do fim".into())
        );
        let comeco = std::time::Instant::now();
        assert!(matches!(mb.proxima(Duration::from_secs(5)), Err(Error::Closed)), "depois, fechado");
        assert!(comeco.elapsed() < Duration::from_secs(1), "não pode esperar o prazo numa sessão morta");
    }

    /// Antes de o canal abrir, mandar é "tente de novo", não queda.
    #[test]
    fn mandar_antes_de_abrir_e_tente_de_novo() {
        let a = Session::offerer(&TransportConfig::default()).expect("ofertante");
        assert!(matches!(a.mensageiro().enviar("cedo"), Err(Error::Transport(_))));
    }
}
