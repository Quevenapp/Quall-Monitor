//! **A busca por três caminhos** — mDNS, broadcast UDP e sondagem unicast da /24 —, a lista que
//! não pisca e a procura de um aparelho pelo `DeviceId`. Ver `docs/conexao.md` §2 e §3.
//!
//! O mDNS de [`crate::discovery`] continua sendo o primeiro caminho, e continua igual. Este módulo
//! soma os outros dois e junta tudo numa tabela só por `DeviceId`:
//!
//! - **O anunciante** ([`Respondedor`], ligado por [`crate::discovery::Advertiser`]) escuta
//!   perguntas na UDP [`PORTA_DA_PERGUNTA`], responde unicast a quem perguntou e se anuncia por
//!   broadcast na [`PORTA_DA_ESCUTA`] de tempos em tempos. Um por processo, com todos os anúncios
//!   do processo: o app que espelha e mostra o teleprompter ao mesmo tempo não disputa a porta
//!   consigo mesmo.
//! - **O navegador** ([`Busca`]) pergunta por broadcast, escuta os anúncios, navega o mDNS e sonda
//!   por unicast — o caminho que funciona no iOS sem o entitlement de multicast e na rede que mata
//!   broadcast. **O que sondar, e quando**, é de `sondagem` (`docs/v3-no-privado.md` §8.9): as dicas,
//!   os vizinhos da tabela ARP e, só como último recurso, a /24, uma vez por processo e devagar.
//!
//! **O datagrama** é JSON numa linha com as **mesmas chaves e os mesmos valores do TXT do mDNS**
//! (no protocolo v3: `v`, `t` — o token efêmero do anunciante, o mesmo do mDNS —, `c`, `p`, `pa`;
//! **nunca** `id` nem `n`, `docs/v3-no-privado.md` §5.3), mais `q`/`f` (assinatura e formato, `2`), `m`
//! (`pergunta` ou `anuncio`), `r` (o nonce da pergunta, ecoado), `cc` (`"1"` quando o aparelho
//! aceita a confirmação por código) e `nm` (o nome que a pessoa escolheu para a rede, quando há —
//! `docs/v3-no-privado.md` §8.7; não autentica nada). Ler é [`crate::discovery::announcement_from_txt`], o mesmo
//! leitor do mDNS: não existe um segundo formato de anúncio para divergir do primeiro.
//!
//! **O endereço de um aparelho é a origem do datagrama**, nunca um campo: quem anuncia não diz
//! onde está, a rede diz.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

use crate::discovery::{
    announcement_from_txt, announcement_to_txt_with_token, discovery_token, nome_anunciado,
    nome_anunciado_do_txt, sanear_nome_anunciado, Browser, DiscoveryEvent, DiscoveredDevice,
    CHAVE_DO_NOME,
};
use crate::error::{Error, Result};
use crate::protocol::{Announcement, DeviceId};

/// UDP: o anunciante escuta perguntas aqui e responde unicast a quem perguntou.
pub const PORTA_DA_PERGUNTA: u16 = 7875;
/// UDP: o navegador escuta aqui os anúncios espontâneos, que saem por broadcast.
pub const PORTA_DA_ESCUTA: u16 = 7876;

/// Assinatura e formato do datagrama. Outra coisa na porta — ou um formato maior que este, de uma
/// build mais nova — é ignorada em silêncio.
const ASSINATURA: &str = "quall";
/// **2 desde o v3** (`docs/v3-no-privado.md` §5.3): o tipo da mensagem saiu da chave `t`, que no TXT v3 é
/// o token, e foi para `m`; uma build v2 (formato 1) ignora o datagrama em silêncio.
const FORMATO: u64 = 2;
/// Teto de um datagrama, nos dois sentidos. Cabe com folga num pacote sem fragmentar.
const TETO_DO_DATAGRAMA: usize = 512;
/// O nome viaja truncado no mesmo teto do rótulo mDNS.
const TETO_DO_NOME: usize = 63;

/// Visto por UDP, o aparelho conta como vivo por este tempo depois do último anúncio. O anunciante
/// se anuncia a cada [`ANUNCIAR_A_CADA`] e o navegador pergunta a cada [`PERGUNTAR_A_CADA`]: doze
/// segundos cobrem duas perguntas perdidas.
pub const VIVO_POR: Duration = Duration::from_secs(12);
/// Depois de deixar de ser visto, o item fica este tempo na lista com "visto há X s", e só então
/// sai. É o que tira o "pisca" (`docs/conexao.md` §2.4).
pub const LEMBRADO_POR: Duration = Duration::from_secs(30);

const PERGUNTAR_A_CADA: Duration = Duration::from_secs(5);
/// A segunda pergunta sai logo depois da primeira: UDP se perde, e a primeira pode chegar antes de o
/// outro lado abrir a porta (medido no Dell, 12/10: um teste perdeu a única pergunta dirigida).
const REPERGUNTAR_LOGO: Duration = Duration::from_millis(500);
const ANUNCIAR_A_CADA: Duration = Duration::from_secs(10);
/// Os primeiros anúncios saem mais juntos: quem abriu a lista agora vê o aparelho em segundos.
const PRIMEIROS_ANUNCIOS: [Duration; 3] =
    [Duration::ZERO, Duration::from_secs(1), Duration::from_secs(3)];
const FATIA: Duration = Duration::from_millis(50);

mod sondagem;
pub use sondagem::{
    avisar_do_toque, tocado_ha_pouco, usar_leitor_de_vizinhos, LeitorDeVizinhos, Varredura,
    VarreduraCompartilhada, DICA_DEPOIS_DE, DICA_DE_NOVO, RAJADA_DA_VARREDURA,
    REPETIR_A_VARREDURA_DEPOIS_DE, RITMO_DA_VARREDURA, SONDAR_DEPOIS_DE, SONDAR_O_ALVO_DEPOIS_DE,
    REFRESCAR_A_CADA, TOQUE_PROTEGE_POR, VIZINHOS_A_CADA,
};
use sondagem::{Momento, Sondador};

/// Quantos aparelhos a lista guarda no máximo (B3: inundação por anúncio forjado).
pub const TETO_DE_APARELHOS: usize = 64;
/// Quantos aparelhos novos podem vir do mesmo IP pela UDP (B3).
pub const APARELHOS_POR_IP: usize = 4;

/// O anunciante responde no máximo isto por segundo, somando todas as origens…
const RESPOSTAS_POR_SEGUNDO: u32 = 20;
/// …e no máximo uma vez neste intervalo à mesma origem.
const INTERVALO_POR_ORIGEM: Duration = Duration::from_millis(500);

// =============================================================================================
// O datagrama
// =============================================================================================

/// O que se leu de um datagrama.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lido {
    /// Alguém procurando. `id` presente é procura dirigida: só aquele aparelho responde.
    Pergunta { r: String, id: Option<DeviceId> },
    /// Um aparelho dizendo quem é. `r` é o nonce da pergunta que ele responde, quando responde.
    /// `nome` é o nome anunciado (chave `nm`, `docs/v3-no-privado.md` §8.7), já saneado.
    Anuncio { anuncio: Announcement, porta: u16, aceita_codigo: bool, nome: Option<String>, r: Option<String> },
}

fn base(tipo: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("q".into(), ASSINATURA.into());
    m.insert("f".into(), FORMATO.into());
    m.insert("m".into(), tipo.into());
    m
}

/// Uma pergunta. `id` dirige a pergunta a um aparelho só.
pub fn pergunta(r: &str, id: Option<&DeviceId>) -> Vec<u8> {
    let mut m = base("pergunta");
    m.insert("r".into(), r.into());
    if let Some(id) = id {
        m.insert("id".into(), id.0.clone().into());
    }
    serde_json::Value::Object(m).to_string().into_bytes()
}

/// Corta um texto em no máximo `teto` bytes, sem partir um caractere.
fn truncar(texto: &str, teto: usize) -> &str {
    if texto.len() <= teto {
        return texto;
    }
    let mut fim = teto;
    while !texto.is_char_boundary(fim) {
        fim -= 1;
    }
    &texto[..fim]
}

/// Um anúncio, com os pares do TXT v3 do mDNS e o token do anunciante. `r` ecoa a pergunta
/// respondida. `None` quando o anúncio não é publicável (versão, porta ou token inválidos).
pub fn anuncio(
    anuncio: &Announcement,
    porta: u16,
    token: &str,
    aceita_codigo: bool,
    r: Option<&str>,
) -> Option<Vec<u8>> {
    anuncio_com_nome(anuncio, porta, token, aceita_codigo, None, r)
}

/// [`anuncio`] com o **nome anunciado** (chave `nm`, `docs/v3-no-privado.md` §8.7), saneado aqui de
/// novo: quem chama não precisa ter saneado.
pub fn anuncio_com_nome(
    anuncio: &Announcement,
    porta: u16,
    token: &str,
    aceita_codigo: bool,
    nome: Option<&str>,
    r: Option<&str>,
) -> Option<Vec<u8>> {
    let mut curto = anuncio.clone();
    curto.display_name = truncar(&anuncio.display_name, TETO_DO_NOME).to_string();
    let mut m = base("anuncio");
    if let Some(r) = r {
        m.insert("r".into(), r.into());
    }
    for (chave, valor) in announcement_to_txt_with_token(&curto, porta, token).ok()? {
        m.insert(chave, valor.into());
    }
    if aceita_codigo {
        m.insert(CHAVE_ACEITA_CODIGO.into(), "1".into());
    }
    if let Some(nome) = nome.and_then(sanear_nome_anunciado) {
        m.insert(CHAVE_DO_NOME.into(), nome.into());
    }
    Some(serde_json::Value::Object(m).to_string().into_bytes())
}

/// A chave de "aceita a confirmação por código", no datagrama **e** no TXT do mDNS. Ausente é
/// "não aceita", que é o que toda build anterior é.
pub const CHAVE_ACEITA_CODIGO: &str = "cc";

/// Lê um datagrama. `None` para tudo que não é nosso, é de formato mais novo, ou está quebrado.
pub fn ler_datagrama(bytes: &[u8]) -> Option<Lido> {
    if bytes.len() > TETO_DO_DATAGRAMA {
        return None;
    }
    let valor: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let m = valor.as_object()?;
    if m.get("q")?.as_str()? != ASSINATURA || m.get("f")?.as_u64()? != FORMATO {
        return None;
    }
    let texto = |k: &str| m.get(k).and_then(|v| v.as_str()).map(str::to_string);
    match m.get("m")?.as_str()? {
        "pergunta" => Some(Lido::Pergunta { r: texto("r")?, id: texto("id").map(DeviceId) }),
        "anuncio" => {
            let props: HashMap<String, String> = m
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();
            let (anuncio, porta) = announcement_from_txt(&props).ok()?;
            Some(Lido::Anuncio {
                anuncio,
                porta,
                aceita_codigo: props.get(CHAVE_ACEITA_CODIGO).is_some_and(|v| v == "1"),
                nome: nome_anunciado_do_txt(&props),
                r: texto("r"),
            })
        }
        _ => None,
    }
}

// =============================================================================================
// Redes locais, broadcast e a /24
// =============================================================================================

/// Uma rede IPv4 privada deste aparelho.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedeLocal {
    pub ip: Ipv4Addr,
    pub prefixo: u8,
}

impl RedeLocal {
    fn mascara(self) -> u32 {
        if self.prefixo == 0 { 0 } else { u32::MAX << (32 - u32::from(self.prefixo.min(32))) }
    }

    /// O endereço da rede, para mostrar: `192.168.15.0/24`.
    pub fn como_texto(self) -> String {
        let rede = Ipv4Addr::from(u32::from(self.ip) & self.mascara());
        format!("{rede}/{}", self.prefixo)
    }

    /// O endereço de broadcast desta rede.
    pub fn broadcast(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.ip) | !self.mascara())
    }

    pub fn contem(self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & self.mascara() == u32::from(self.ip) & self.mascara()
    }

    /// **Quem a sondagem pergunta**: a /24 do próprio endereço — ou a sub-rede, se for menor que
    /// uma /24 —, sem o próprio endereço, sem o endereço de rede e sem o de broadcast.
    pub fn alvos_da_sondagem(self) -> Vec<Ipv4Addr> {
        let prefixo = self.prefixo.max(24);
        let rede = RedeLocal { ip: self.ip, prefixo };
        let mascara = rede.mascara();
        let inicio = u32::from(self.ip) & mascara;
        let fim = inicio | !mascara;
        (inicio.saturating_add(1)..fim)
            .map(Ipv4Addr::from)
            .filter(|ip| *ip != self.ip)
            .collect()
    }
}

/// Endereço que interessa à busca: IPv4 privado (`10/8`, `172.16/12`, `192.168/16`).
fn e_rede_de_casa(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

/// As redes IPv4 privadas das interfaces, sem repetição, no máximo duas (`docs/conexao.md` §2.3),
/// **a da rota padrão primeiro**.
///
/// # Por que a ordem importa (revisão adversarial de 12/10, M4)
///
/// A primeira versão ficava com as duas primeiras na ordem em que o sistema as enumera. No Dell há
/// `vEthernet (WSL (Hyper-V firewall))`, e WSL, Hyper-V, VirtualBox, VMware e Docker criam redes
/// privadas que não levam a aparelho nenhum: com duas delas na frente, a sondagem e o broadcast iam
/// para lá e a LAN de verdade ficava de fora. Agora escolhe-se antes de cortar: a interface da rota
/// padrão (a que o sistema usaria para sair), depois as que não parecem virtuais, e só então as
/// virtuais. Interface desligada não entra.
pub fn redes_locais() -> Vec<RedeLocal> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let candidatas: Vec<(String, RedeLocal)> = interfaces
        .into_iter()
        .filter(|i| !matches!(i.oper_status, if_addrs::IfOperStatus::Down))
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some((i.name, RedeLocal { ip: v4.ip, prefixo: v4.prefixlen })),
            _ => None,
        })
        .collect();
    escolher_redes(candidatas, ip_da_rota_padrao())
}

/// O IP local que o sistema usaria para sair para fora: `connect` de UDP só escolhe a rota, não manda
/// pacote nenhum. `None` sem rota padrão (rede só local, avião).
fn ip_da_rota_padrao() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?; // TEST-NET-1: nunca é a LAN de ninguém
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// O nome é de interface que não leva a aparelho nenhum? Comparação sem caixa, por pedaço do nome.
fn parece_virtual(nome: &str) -> bool {
    const PEDACOS: [&str; 14] = [
        "vethernet", "wsl", "hyper-v", "virtualbox", "vboxnet", "vmware", "vmnet", "docker", "br-",
        "veth", "virbr", "utun", "zerotier", "tailscale",
    ];
    let n = nome.to_ascii_lowercase();
    PEDACOS.iter().any(|p| n.contains(p))
}

/// A escolha, pura para dar para testar: privadas, sem repetição, a da rota padrão primeiro, depois
/// as reais, depois as virtuais — e só então o corte em duas.
pub(crate) fn escolher_redes(candidatas: Vec<(String, RedeLocal)>, rota: Option<Ipv4Addr>) -> Vec<RedeLocal> {
    let mut boas: Vec<(u8, RedeLocal)> = Vec::new();
    for (nome, rede) in candidatas {
        if rede.ip.is_loopback() || !e_rede_de_casa(rede.ip) {
            continue;
        }
        if boas.iter().any(|(_, r)| r.como_texto() == rede.como_texto()) {
            continue;
        }
        let peso = if Some(rede.ip) == rota {
            0
        } else if parece_virtual(&nome) {
            2
        } else {
            1
        };
        boas.push((peso, rede));
    }
    // Estável: dentro do mesmo peso, a ordem do sistema. Havendo uma rede real, as virtuais saem de
    // vez: sondar a /24 de um comutador do WSL são 253 perguntas que não chegam a aparelho nenhum.
    boas.sort_by_key(|(peso, _)| *peso);
    if boas.iter().any(|(peso, _)| *peso < 2) {
        boas.retain(|(peso, _)| *peso < 2);
    }
    boas.into_iter().map(|(_, r)| r).take(2).collect()
}

/// Ligada pelos testes da fronteira C (`quall-ffi`), que usam a biblioteca de produto e não têm o
/// `cfg(test)` deste crate: ver [`so_o_laco_local`].
static SO_O_LACO_LOCAL: AtomicBool = AtomicBool::new(false);

/// **Só o laço local, daqui em diante, no processo inteiro**: a busca e o anúncio UDP abrem as
/// portas em `127.0.0.1`, o broadcast e a sondagem vão só para o loopback, o mDNS da busca fica
/// desligado e o [`crate::discovery::Advertiser`] não registra nada no mDNS. É para os testes que
/// usam a biblioteca de produto: um `cargo test` no MacBook da bancada não pode pôr aparelhos falsos
/// na lista dos celulares de verdade, nem responder às sondagens deles. Não tem volta.
#[doc(hidden)]
pub fn so_o_laco_local() {
    SO_O_LACO_LOCAL.store(true, Ordering::Release);
}

/// Os testes deste crate, ou a chave de [`so_o_laco_local`].
pub(crate) fn no_laco_local() -> bool {
    cfg!(test) || SO_O_LACO_LOCAL.load(Ordering::Acquire)
}

/// A chave de [`so_o_laco_local`] sozinha (sem o `cfg(test)`): o que o mDNS de verdade consulta.
pub(crate) fn laco_local_pedido() -> bool {
    SO_O_LACO_LOCAL.load(Ordering::Acquire)
}

/// Para onde vai um broadcast: o limitado e o de cada rede.
///
/// **Nos testes, só o loopback**: um `cargo test` no MacBook da bancada não pode pôr aparelhos de
/// teste na lista dos celulares de verdade.
fn destinos_de_broadcast(porta: u16) -> Vec<SocketAddr> {
    if no_laco_local() {
        return vec![SocketAddr::from((Ipv4Addr::LOCALHOST, porta))];
    }
    let mut d = vec![SocketAddr::from((Ipv4Addr::BROADCAST, porta))];
    for rede in redes_locais() {
        let alvo = SocketAddr::from((rede.broadcast(), porta));
        if !d.contains(&alvo) {
            d.push(alvo);
        }
    }
    d
}

/// Quem a sondagem pergunta. Nos testes, só o loopback, pelo mesmo motivo de cima.
fn alvos_da_sondagem() -> Vec<Ipv4Addr> {
    if no_laco_local() {
        return vec![Ipv4Addr::LOCALHOST];
    }
    redes_locais().into_iter().flat_map(RedeLocal::alvos_da_sondagem).collect()
}

/// **Os vizinhos que valem perguntar** (`docs/v3-no-privado.md` §8.9): as entradas completas da
/// tabela ARP ([`usar_leitor_de_vizinhos`]) que caem numa das redes da busca, sem o próprio
/// endereço, o de rede e o de broadcast. Nos testes, nenhum.
fn vizinhos_da_busca() -> Vec<Ipv4Addr> {
    if no_laco_local() {
        return Vec::new();
    }
    let redes = redes_locais();
    filtrar_vizinhos(sondagem::ler_vizinhos(), &redes)
}

/// A filtragem de [`vizinhos_da_busca`], pura.
pub(crate) fn filtrar_vizinhos(mut crus: Vec<Ipv4Addr>, redes: &[RedeLocal]) -> Vec<Ipv4Addr> {
    crus.sort();
    crus.dedup();
    crus.retain(|ip| {
        !ip.is_multicast()
            && redes.iter().any(|r| {
                let rede = Ipv4Addr::from(u32::from(r.ip) & r.mascara());
                r.contem(*ip) && *ip != r.ip && *ip != r.broadcast() && *ip != rede
            })
    });
    crus
}

/// Um socket UDP IPv4 com broadcast ligado. `porta` 0 é efêmera; uma porta fixa abre com
/// `SO_REUSEADDR` (e `SO_REUSEPORT` fora do Windows), para o app e o plugin de OBS conviverem
/// na mesma máquina.
fn socket_udp(porta: u16) -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    if porta != 0 {
        s.set_reuse_address(true)?;
        #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
        s.set_reuse_port(true)?;
    }
    s.set_broadcast(true)?;
    let onde = if no_laco_local() { Ipv4Addr::LOCALHOST } else { Ipv4Addr::UNSPECIFIED };
    s.bind(&SocketAddr::from((onde, porta)).into())?;
    let u: UdpSocket = s.into();
    u.set_read_timeout(Some(Duration::from_millis(5)))?;
    Ok(u)
}

/// Um nonce curto para casar pergunta e resposta.
fn nonce() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Lê tudo que já chegou num socket, sem esperar mais que o tempo de leitura dele.
fn drenar(s: &UdpSocket, mut cada: impl FnMut(Lido, SocketAddr)) {
    let mut buf = [0u8; TETO_DO_DATAGRAMA + 1];
    for _ in 0..64 {
        match s.recv_from(&mut buf) {
            Ok((n, origem)) => {
                if let Some(lido) = ler_datagrama(&buf[..n]) {
                    cada(lido, origem);
                }
            }
            Err(_) => return,
        }
    }
}

// =============================================================================================
// O anunciante: um respondedor por processo
// =============================================================================================

/// Um anúncio registrado no respondedor do processo.
#[derive(Clone)]
struct Registro {
    chave: u64,
    anuncio: Announcement,
    porta: u16,
    /// O token efêmero do anúncio v3 — o mesmo do mDNS quando quem liga é o
    /// [`crate::discovery::Advertiser`].
    token: String,
    aceita_codigo: bool,
    /// O nome anunciado com que este anúncio começou (§8.7).
    nome: Option<String>,
}

struct Compartilhado {
    registros: Vec<Registro>,
    parar: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    porta_da_pergunta: u16,
}

/// Um respondedor por porta da pergunta. Em produto é sempre um só (a 7875); os testes usam portas
/// próprias para não se cruzarem.
static RESPONDEDOR: Mutex<Vec<Compartilhado>> = Mutex::new(Vec::new());
static PROXIMA_CHAVE: AtomicU64 = AtomicU64::new(1);

fn respondedor() -> MutexGuard<'static, Vec<Compartilhado>> {
    match RESPONDEDOR.lock() {
        Ok(g) => g,
        Err(envenenado) => envenenado.into_inner(),
    }
}

/// **Um anúncio no ar pela UDP**, enquanto este valor existir. Ver o cabeçalho do módulo.
///
/// Os anúncios do processo dividem um respondedor só (uma thread, um socket na
/// [`PORTA_DA_PERGUNTA`]); o primeiro liga, o último desliga.
pub struct Respondedor {
    chave: u64,
    porta_da_pergunta: u16,
}

impl Respondedor {
    /// Anuncia `anuncio` pela UDP. Não falha: sem a porta da pergunta (outro processo com ela sem
    /// `SO_REUSEPORT`, firewall de saída), o anúncio periódico por broadcast continua saindo.
    ///
    /// Com token próprio, sorteado aqui: é o caminho de quem anuncia o mDNS por fora do núcleo (o iOS,
    /// pelo `mDNSResponder`). O token não é o do Bonjour até a etapa 3 de `docs/v3-no-privado.md`.
    pub fn iniciar(anuncio: &Announcement, porta: u16, aceita_codigo: bool) -> Respondedor {
        let token = discovery_token().unwrap_or_default();
        Self::iniciar_com_token(anuncio, porta, aceita_codigo, &token)
    }

    /// [`Respondedor::iniciar`] com o token do anúncio mDNS, para os dois caminhos dizerem o mesmo.
    /// Leva o nome anunciado do processo ([`crate::discovery::definir_nome_anunciado`]) de agora.
    pub fn iniciar_com_token(
        anuncio: &Announcement,
        porta: u16,
        aceita_codigo: bool,
        token: &str,
    ) -> Respondedor {
        Self::iniciar_com_token_e_nome(anuncio, porta, aceita_codigo, token, nome_anunciado())
    }

    /// [`Respondedor::iniciar_com_token`] com o nome que o mDNS do mesmo token já leva: o
    /// [`crate::discovery::Advertiser`] lê o nome do processo uma vez e dá o mesmo aos dois caminhos.
    pub fn iniciar_com_token_e_nome(
        anuncio: &Announcement,
        porta: u16,
        aceita_codigo: bool,
        token: &str,
        nome: Option<String>,
    ) -> Respondedor {
        Self::iniciar_em_com_nome(
            anuncio,
            porta,
            aceita_codigo,
            token,
            nome,
            PORTA_DA_PERGUNTA,
            PORTA_DA_ESCUTA,
        )
    }

    /// Nas portas dos testes, sem nome.
    #[cfg(test)]
    pub(crate) fn iniciar_em(
        anuncio: &Announcement,
        porta: u16,
        aceita_codigo: bool,
        token: &str,
        porta_da_pergunta: u16,
        porta_da_escuta: u16,
    ) -> Respondedor {
        Self::iniciar_em_com_nome(anuncio, porta, aceita_codigo, token, None, porta_da_pergunta, porta_da_escuta)
    }

    pub(crate) fn iniciar_em_com_nome(
        anuncio: &Announcement,
        porta: u16,
        aceita_codigo: bool,
        token: &str,
        nome: Option<String>,
        porta_da_pergunta: u16,
        porta_da_escuta: u16,
    ) -> Respondedor {
        let chave = PROXIMA_CHAVE.fetch_add(1, Ordering::Relaxed);
        let registro = Registro {
            chave,
            anuncio: anuncio.clone(),
            porta,
            token: token.to_string(),
            aceita_codigo,
            nome: nome.as_deref().and_then(sanear_nome_anunciado),
        };
        let mut g = respondedor();
        match g.iter_mut().find(|c| c.porta_da_pergunta == porta_da_pergunta) {
            Some(c) => c.registros.push(registro),
            None => {
                let parar = Arc::new(AtomicBool::new(false));
                let parar_t = Arc::clone(&parar);
                let thread = std::thread::Builder::new()
                    .name("quall.anuncio-udp".into())
                    .spawn(move || laco_do_respondedor(parar_t, porta_da_pergunta, porta_da_escuta))
                    .ok();
                g.push(Compartilhado {
                    registros: vec![registro],
                    parar,
                    thread,
                    porta_da_pergunta,
                });
            }
        }
        Respondedor { chave, porta_da_pergunta }
    }
}

impl Drop for Respondedor {
    fn drop(&mut self) {
        let thread = {
            let mut g = respondedor();
            let Some(i) = g.iter().position(|c| c.porta_da_pergunta == self.porta_da_pergunta) else {
                return;
            };
            g[i].registros.retain(|r| r.chave != self.chave);
            if !g[i].registros.is_empty() {
                return;
            }
            g[i].parar.store(true, Ordering::Release);
            g.remove(i).thread.take()
        };
        // Fora do cadeado: a thread pega o cadeado a cada volta.
        if let Some(t) = thread {
            let _ = t.join();
        }
    }
}

/// O anunciante só fala com a LAN: origem privada, link-local ou loopback.
pub(crate) fn origem_permitida(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.segments()[0] & 0xfe00 == 0xfc00,
    }
}

/// O limite de respostas: [`RESPOSTAS_POR_SEGUNDO`] no total e uma por
/// [`INTERVALO_POR_ORIGEM`] por origem (endereço **e porta**: duas buscas da mesma máquina são
/// duas origens, e a segunda não pode ficar sem resposta por causa da primeira).
#[derive(Default)]
struct Limite {
    por_origem: HashMap<SocketAddr, Instant>,
    janela: Option<Instant>,
    na_janela: u32,
}

impl Limite {
    fn pode(&mut self, origem: SocketAddr, agora: Instant) -> bool {
        if self.por_origem.get(&origem).is_some_and(|t| agora.duration_since(*t) < INTERVALO_POR_ORIGEM) {
            return false;
        }
        match self.janela {
            Some(t) if agora.duration_since(t) < Duration::from_secs(1) => {
                if self.na_janela >= RESPOSTAS_POR_SEGUNDO {
                    return false;
                }
            }
            _ => {
                self.janela = Some(agora);
                self.na_janela = 0;
            }
        }
        self.na_janela += 1;
        if self.por_origem.len() > 256 {
            self.por_origem.retain(|_, t| agora.duration_since(*t) < INTERVALO_POR_ORIGEM);
        }
        self.por_origem.insert(origem, agora);
        true
    }
}

fn laco_do_respondedor(parar: Arc<AtomicBool>, porta_da_pergunta: u16, porta_da_escuta: u16) {
    // A porta fixa é para receber; se não abrir, um socket efêmero ainda anuncia por broadcast.
    let escuta = socket_udp(porta_da_pergunta).ok();
    let Some(envio) = escuta.as_ref().and_then(|s| s.try_clone().ok()).or_else(|| socket_udp(0).ok())
    else {
        return;
    };
    let inicio = Instant::now();
    let mut proximo = 0usize;
    let mut ultimo_anuncio: Option<Instant> = None;
    let mut limite = Limite::default();

    while !parar.load(Ordering::Acquire) {
        let registros = respondedor()
            .iter()
            .find(|c| c.porta_da_pergunta == porta_da_pergunta)
            .map(|c| c.registros.clone())
            .unwrap_or_default();
        let agora = Instant::now();

        // 1. O anúncio periódico, por broadcast.
        let na_hora = match ultimo_anuncio {
            _ if proximo < PRIMEIROS_ANUNCIOS.len() => agora.duration_since(inicio) >= PRIMEIROS_ANUNCIOS[proximo],
            Some(t) => agora.duration_since(t) >= ANUNCIAR_A_CADA,
            None => true,
        };
        if na_hora {
            proximo += 1;
            ultimo_anuncio = Some(agora);
            for destino in destinos_de_broadcast(porta_da_escuta) {
                for r in &registros {
                    let nome = r.nome.as_deref();
                    if let Some(d) = anuncio_com_nome(&r.anuncio, r.porta, &r.token, r.aceita_codigo, nome, None) {
                        let _ = envio.send_to(&d, destino);
                    }
                }
            }
        }

        // 2. As perguntas.
        match escuta.as_ref() {
            Some(s) => {
                let _ = s.set_read_timeout(Some(FATIA));
                drenar(s, |lido, origem| {
                    let Lido::Pergunta { r, id } = lido else { return };
                    if !origem_permitida(origem.ip()) || !limite.pode(origem, Instant::now()) {
                        return;
                    }
                    // **A pergunta dirigida por id não é respondida no v3**: responder só a quem sabe o
                    // id seria um oráculo de "este aparelho está nesta rede?" para qualquer um da LAN
                    // (`docs/v3-no-privado.md` §5.3). Localizar volta pelo `Challenge` na etapa 3.
                    if id.is_some() {
                        return;
                    }
                    for reg in &registros {
                        let nome = reg.nome.as_deref();
                        if let Some(d) =
                            anuncio_com_nome(&reg.anuncio, reg.porta, &reg.token, reg.aceita_codigo, nome, Some(&r))
                        {
                            let _ = envio.send_to(&d, origem);
                        }
                    }
                });
            }
            None => std::thread::sleep(FATIA),
        }
    }
}

/// Quantos anúncios o respondedor desta porta tem no ar. Só para os testes.
#[cfg(test)]
fn anuncios_no_ar(porta_da_pergunta: u16) -> usize {
    respondedor()
        .iter()
        .find(|c| c.porta_da_pergunta == porta_da_pergunta)
        .map_or(0, |c| c.registros.len())
}

// =============================================================================================
// O navegador
// =============================================================================================

/// Um aparelho na lista.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AparelhoVisto {
    pub anuncio: Announcement,
    /// Onde conectar. Preferência: o endereço que respondeu por UDP há pouco (provou que alcança);
    /// senão o melhor do mDNS; senão o último UDP, mesmo velho.
    pub endpoint: Option<SocketAddr>,
    /// Por onde foi visto: `"mdns"`, `"udp"` (broadcast) e/ou `"sondagem"`.
    pub via: Vec<&'static str>,
    /// Há quanto tempo veio o último sinal de vida. Zero enquanto o mDNS o mantém.
    pub visto_ha: Duration,
    /// Visto há no máximo [`VIVO_POR`]. Falso é "visto há X s", e o item sai depois de
    /// [`LEMBRADO_POR`].
    pub vivo: bool,
    /// Aceita a confirmação por código (`docs/conexao.md` §4).
    pub aceita_codigo: bool,
    /// **É um anúncio deste mesmo processo** (o token é de um anunciante daqui): a casca não lista
    /// a si mesma. No v3 o `device_id` da lista é o token, e comparar com o id próprio não serve.
    pub proprio: bool,
    /// **Um par conhecido**, reconhecido pelas dicas do `Challenge` (`session::Reconhecedor`,
    /// `docs/v3-no-privado.md` §5.4): o id autenticado que a casca guardou, para mostrar o nome
    /// local e conectar com o alvo (A2). `None` sem reconhecedor, ainda não sondado ou desconhecido.
    pub conhecido: Option<DeviceId>,
    /// Quem é este item na lista: o token com o endereço (ou a instância do mDNS). Serve de chave
    /// para o cache do reconhecimento.
    pub chave: String,
    /// **O nome que o aparelho anuncia** (chave `nm`, `docs/v3-no-privado.md` §8.7), saneado. Não
    /// autentica nada: a linha o mostra com o alias embaixo ([`rotulo_da_linha`]).
    pub nome_anunciado: Option<String>,
}

/// **O que a linha da lista mostra** (`docs/v3-no-privado.md` §8.7): o título e, embaixo, em letra
/// menor, o alias — quando o título não é ele.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotuloDaLinha {
    pub titulo: String,
    /// O alias `Quall <8 hex>` embaixo do nome anunciado; `None` quando o título já é o alias ou é o
    /// nome de um par guardado (a casca diz "pareado").
    pub alias_embaixo: Option<String>,
    /// O título é o nome guardado de um par que a sonda reconheceu.
    pub pareado: bool,
}

/// **A ordem do nome na linha**: o nome do par guardado (o apelido local, só com a sonda tendo
/// reconhecido o item) > o nome anunciado > o alias. Um nome anunciado nunca cobre um par
/// reconhecido, e sempre leva o alias embaixo: quem se chama "MacBook do Pessoa" na rede não vira,
/// na lista, o MacBook do Pessoa.
pub fn rotulo_da_linha(nome_guardado: Option<&str>, nome_anunciado: Option<&str>, alias: &str) -> RotuloDaLinha {
    let guardado = nome_guardado.map(str::trim).filter(|n| !n.is_empty());
    if let Some(n) = guardado {
        return RotuloDaLinha { titulo: n.to_string(), alias_embaixo: None, pareado: true };
    }
    match nome_anunciado.and_then(sanear_nome_anunciado) {
        Some(n) if n != alias => {
            RotuloDaLinha { titulo: n, alias_embaixo: Some(alias.to_string()), pareado: false }
        }
        _ => RotuloDaLinha { titulo: alias.to_string(), alias_embaixo: None, pareado: false },
    }
}

struct Entrada {
    /// Quando o item entrou na lista: com o mesmo token em dois endereços, vale o mais antigo.
    desde: Instant,
    anuncio: Announcement,
    porta: u16,
    aceita_codigo: bool,
    nome_anunciado: Option<String>,
    via: BTreeSet<&'static str>,
    /// O aparelho como o mDNS o vê, enquanto o mDNS não o removeu.
    mdns: Option<DiscoveredDevice>,
    /// Origem e hora da última resposta ou anúncio por UDP.
    udp: Option<(IpAddr, Instant)>,
    /// Último sinal de vida.
    ultimo: Instant,
}

impl Entrada {
    fn retrato(&self, agora: Instant, chave: &str) -> AparelhoVisto {
        let visto_ha = if self.mdns.is_some() { Duration::ZERO } else { agora.duration_since(self.ultimo) };
        let udp_fresco = self.udp.filter(|(_, t)| agora.duration_since(*t) <= VIVO_POR);
        let endpoint = udp_fresco
            .map(|(ip, _)| SocketAddr::new(ip, self.porta))
            .or_else(|| self.mdns.as_ref().and_then(DiscoveredDevice::endpoint))
            .or_else(|| self.udp.map(|(ip, _)| SocketAddr::new(ip, self.porta)));
        AparelhoVisto {
            anuncio: self.anuncio.clone(),
            endpoint,
            via: self.via.iter().copied().collect(),
            visto_ha,
            vivo: visto_ha <= VIVO_POR,
            aceita_codigo: self.aceita_codigo,
            proprio: self
                .anuncio
                .device_id
                .0
                .strip_prefix("discovery-")
                .is_some_and(|t| tokens_proprios().iter().any(|p| p == t)),
            conhecido: None,
            chave: chave.to_string(),
            nome_anunciado: self.nome_anunciado.clone(),
        }
    }
}

/// **O que mudou na lista, para o diário** (bancada de 13/10: "lista com 1 aparelho(s)" não dizia
/// quem nem por onde). Uma linha por item que entrou, saiu ou mudou de caminho, pela chave do item,
/// com o **alias** do token (`Quall 1a2b3c4d`, nunca o nome pessoal), o endereço e as vias
/// (`mdns`, `udp`, `sondagem`):
///
/// ```text
/// entrou Quall 1a2b3c4d em 192.168.15.7:7877 por sondagem
/// saiu Quall 9f8e7d6c (era 192.168.15.9:7877 por mdns+udp)
/// Quall 1a2b3c4d agora por mdns+sondagem
/// ```
pub fn mudancas_da_lista(antes: &[AparelhoVisto], depois: &[AparelhoVisto]) -> Vec<String> {
    fn alias(a: &AparelhoVisto) -> String {
        match a.anuncio.device_id.0.strip_prefix("discovery-") {
            Some(t) => format!("Quall {}", t.get(..8).unwrap_or(t)),
            None => "um aparelho sem token".into(),
        }
    }
    fn onde(a: &AparelhoVisto) -> String {
        let endereco = a.endpoint.map_or_else(|| "sem endereço".to_string(), |e| e.to_string());
        let mut vias = a.via.clone();
        vias.sort_unstable();
        format!("{endereco} por {}", vias.join("+"))
    }
    let mut linhas = Vec::new();
    for d in depois {
        match antes.iter().find(|a| a.chave == d.chave) {
            None => linhas.push(format!("entrou {} em {}{}", alias(d), onde(d), if d.proprio { " (o próprio anúncio)" } else { "" })),
            Some(a) if onde(a) != onde(d) => {
                linhas.push(format!("{} agora em {}", alias(d), onde(d)));
            }
            Some(_) => {}
        }
    }
    for a in antes {
        if !depois.iter().any(|d| d.chave == a.chave) {
            linhas.push(format!("saiu {} (era {})", alias(a), onde(a)));
        }
    }
    linhas
}

/// Os tokens dos anúncios UDP deste processo (o `Advertiser` e o respondedor do iOS os registram).
pub fn tokens_proprios() -> Vec<String> {
    respondedor().iter().flat_map(|c| c.registros.iter().map(|r| r.token.clone())).collect()
}

/// O que a busca tem a dizer quando a lista está vazia. Ver [`Busca::diagnostico`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostico {
    /// As causas prováveis, da mais para a menos provável.
    pub causas: Vec<Causa>,
    /// As redes deste aparelho, como texto (`192.168.15.0/24`).
    pub redes: Vec<String>,
    /// Quantas sondagens da /24 já saíram.
    pub sondagens: u32,
    /// A porta da escuta (7876) abriu. Sem ela, o broadcast não chega aqui.
    pub escuta_aberta: bool,
}

/// Uma causa provável de a lista estar vazia. O texto é da casca, pelo código.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Causa {
    /// Nenhuma interface com IPv4 privado: avião, só dados móveis, só VPN.
    SemRedeLocal,
    /// O endereço esperado (o da sessão salva) não cabe em nenhuma rede deste aparelho.
    SubRedeDiferente,
    /// Firewall: no Windows, sempre; em qualquer lugar, quando a porta da escuta não abriu.
    Firewall,
    /// A sondagem saiu e ninguém respondeu: rede de convidados, *AP isolation*.
    Isolamento,
}

impl Causa {
    pub fn como_texto(self) -> &'static str {
        match self {
            Causa::SemRedeLocal => "sem_rede_local",
            Causa::SubRedeDiferente => "sub_rede_diferente",
            Causa::Firewall => "firewall",
            Causa::Isolamento => "isolamento",
        }
    }
}

/// As causas, pela regra de `docs/conexao.md` §2.5. Pura, para dar para testar.
pub fn causas_provaveis(
    redes: &[RedeLocal],
    esperado: Option<IpAddr>,
    lista_vazia: bool,
    sondagens: u32,
    escuta_aberta: bool,
    windows: bool,
) -> Vec<Causa> {
    let mut causas = Vec::new();
    if redes.is_empty() {
        causas.push(Causa::SemRedeLocal);
    }
    if let Some(IpAddr::V4(ip)) = esperado {
        if !redes.is_empty() && !redes.iter().any(|r| r.contem(ip)) {
            causas.push(Causa::SubRedeDiferente);
        }
    }
    if lista_vazia && !redes.is_empty() {
        if (windows && sondagens > 0) || !escuta_aberta {
            causas.push(Causa::Firewall);
        }
        if sondagens > 0 {
            causas.push(Causa::Isolamento);
        }
    }
    causas
}

/// Como a busca se comporta. O padrão é a tela de aparelhos.
#[derive(Debug, Clone)]
pub struct OpcoesDaBusca {
    /// Procura dirigida: as perguntas levam este id, e só ele responde.
    pub alvo: Option<DeviceId>,
    /// Um endereço onde o alvo provavelmente está (o da sessão salva): recebe a primeira pergunta
    /// por unicast e entra no diagnóstico.
    pub dica: Option<SocketAddr>,
    pub mdns: bool,
    pub broadcast: bool,
    pub sondar: bool,
    /// **A varredura da /24**, que as buscas do processo dividem ([`Varredura::do_processo`]; no laço
    /// local, uma por busca). `docs/v3-no-privado.md` §8.9.
    pub varredura: VarreduraCompartilhada,
    pub porta_da_pergunta: u16,
    pub porta_da_escuta: u16,
}

impl Default for OpcoesDaBusca {
    fn default() -> Self {
        OpcoesDaBusca {
            alvo: None,
            dica: None,
            mdns: true,
            broadcast: true,
            sondar: true,
            varredura: Varredura::do_processo(),
            porta_da_pergunta: PORTA_DA_PERGUNTA,
            porta_da_escuta: PORTA_DA_ESCUTA,
        }
    }
}

#[derive(Default)]
struct Tabela {
    /// Por **token + IP + porta** ([`chave_da_entrada`], revisão B5 de `docs/v3-no-privado.md`): um
    /// token forjado igual, vindo de outro IP, é outro item — não muda o endereço de quem já está.
    entradas: HashMap<String, Entrada>,
    sondagens: u32,
    escuta_aberta: bool,
    /// As dicas que a casca deu depois de a busca abrir ([`Busca::dar_dica`]), para o laço pegar.
    dicas_novas: Vec<Ipv4Addr>,
    /// A pessoa tocou "Procurar de novo" ([`Busca::procurar_de_novo`]).
    pedido: bool,
}

/// **A busca por três caminhos**, numa thread própria. Ver o cabeçalho do módulo.
///
/// Soltar (ou [`Busca::parar`]) encerra a thread e o mDNS.
pub struct Busca {
    tabela: Arc<Mutex<Tabela>>,
    parar: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    dica: Option<SocketAddr>,
    varredura: VarreduraCompartilhada,
    numero: u64,
}

fn travar(t: &Mutex<Tabela>) -> MutexGuard<'_, Tabela> {
    match t.lock() {
        Ok(g) => g,
        Err(envenenado) => envenenado.into_inner(),
    }
}

impl Busca {
    /// A tela de aparelhos: os três caminhos, sem alvo.
    pub fn iniciar() -> Busca {
        Self::com(OpcoesDaBusca::default())
    }

    pub fn com(opcoes: OpcoesDaBusca) -> Busca {
        let tabela = Arc::new(Mutex::new(Tabela::default()));
        let parar = Arc::new(AtomicBool::new(false));
        let dica = opcoes.dica;
        let varredura = Arc::clone(&opcoes.varredura);
        let numero = sondagem::numero_da_busca();
        let (t, p) = (Arc::clone(&tabela), Arc::clone(&parar));
        let thread = std::thread::Builder::new()
            .name("quall.busca".into())
            .spawn(move || laco_da_busca(opcoes, numero, t, p))
            .ok();
        Busca { tabela, parar, thread, dica, varredura, numero }
    }

    /// **Uma dica**: um endereço onde um aparelho conhecido esteve (o último de um vínculo). Recebe
    /// poucas perguntas unicast, e só enquanto não está na lista (`docs/v3-no-privado.md` §8.9). Só
    /// IPv4 privado conta (no laço local, o loopback); o resto é ignorado.
    pub fn dar_dica(&self, ip: IpAddr) {
        if let IpAddr::V4(v4) = ip {
            if v4.is_private() || (no_laco_local() && v4.is_loopback()) {
                travar(&self.tabela).dicas_novas.push(v4);
            }
        }
    }

    /// **"Procurar de novo"**: a varredura da /24 recomeça agora, com a lista cheia ou não, no ritmo
    /// de sempre (`docs/v3-no-privado.md` §8.9). Só esta busca a anda enquanto for a dona.
    pub fn procurar_de_novo(&self) {
        travar(&self.tabela).pedido = true;
    }

    /// A lista agora: vivos primeiro, depois os "vistos há X s", cada grupo por nome. Quem passou
    /// de [`VIVO_POR`] + [`LEMBRADO_POR`] já saiu.
    pub fn aparelhos(&self) -> Vec<AparelhoVisto> {
        let agora = Instant::now();
        let mut lista: Vec<AparelhoVisto> =
            travar(&self.tabela).entradas.iter().map(|(k, e)| e.retrato(agora, k)).collect();
        // Pelo nome que a linha mostra (o anunciado, senão o alias); o par guardado a casca reordena.
        let nome = |a: &AparelhoVisto| a.nome_anunciado.clone().unwrap_or_else(|| a.anuncio.display_name.clone());
        lista.sort_by(|a, b| {
            b.vivo
                .cmp(&a.vivo)
                .then_with(|| nome(a).to_lowercase().cmp(&nome(b).to_lowercase()))
                .then_with(|| a.anuncio.display_name.cmp(&b.anuncio.display_name))
                .then_with(|| a.anuncio.device_id.0.cmp(&b.anuncio.device_id.0))
        });
        lista
    }

    /// Um aparelho pelo id da lista (`discovery-<token>`), se está nela. Com o mesmo token em dois
    /// endereços (um forjado), devolve **o que entrou primeiro** (revisão, baixo 6): quem chega
    /// depois com um token copiado não toma o lugar de quem já estava.
    pub fn aparelho(&self, id: &DeviceId) -> Option<AparelhoVisto> {
        let agora = Instant::now();
        travar(&self.tabela)
            .entradas
            .iter()
            .filter(|(_, e)| e.anuncio.device_id == *id)
            .min_by_key(|(_, e)| e.desde)
            .map(|(k, e)| e.retrato(agora, k))
    }

    /// Por que a lista pode estar vazia (`docs/conexao.md` §2.5).
    pub fn diagnostico(&self) -> Diagnostico {
        let redes = if no_laco_local() { Vec::new() } else { redes_locais() };
        let t = travar(&self.tabela);
        Diagnostico {
            causas: causas_provaveis(
                &redes,
                self.dica.map(|d| d.ip()),
                t.entradas.is_empty(),
                t.sondagens,
                t.escuta_aberta,
                cfg!(windows),
            ),
            redes: redes.iter().map(|r| r.como_texto()).collect(),
            sondagens: t.sondagens,
            escuta_aberta: t.escuta_aberta,
        }
    }

    pub fn parar(mut self) {
        self.encerrar();
    }

    fn encerrar(&mut self) {
        self.parar.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        varredura_travada(&self.varredura).largar(self.numero);
    }
}

impl Drop for Busca {
    fn drop(&mut self) {
        self.encerrar();
    }
}

/// A chave de um item visto **só por UDP**: o token (no `device_id` efêmero `discovery-<token>`),
/// o IP de origem e a porta. O item do mDNS tem a chave de [`chave_do_mdns`].
fn chave_da_entrada(id: &str, ip: Option<IpAddr>, porta: u16) -> String {
    match ip {
        Some(ip) => format!("{id}|{ip}|{porta}"),
        None => format!("{id}|-|{porta}"),
    }
}

/// A chave de um item do mDNS: o nome completo da instância (`Quall <token>._quall._tcp.local.`).
/// Quando o melhor endereço do aparelho muda, o item é o mesmo (revisão M-d).
fn chave_do_mdns(fullname: &str) -> String {
    format!("mdns|{fullname}")
}

/// O item tem este IP — pela origem do UDP ou entre os endereços do mDNS.
fn tem_o_ip(e: &Entrada, ip: IpAddr) -> bool {
    e.udp.is_some_and(|(i, _)| i == ip)
        || e.mdns.as_ref().is_some_and(|d| d.addresses.iter().any(|a| a.to_ip_addr() == ip))
}

/// O teto da lista e o teto por IP (B3), para quem ainda não está nela. `true` = não cabe.
fn nao_cabe(t: &Tabela, ips: &[IpAddr]) -> bool {
    let do_mesmo_ip = t.entradas.values().filter(|e| ips.iter().any(|ip| tem_o_ip(e, *ip))).count();
    t.entradas.len() >= TETO_DE_APARELHOS || do_mesmo_ip >= APARELHOS_POR_IP
}

/// **O token muda a cada espera** (`docs/v3-no-privado.md` §5.3): um item novo no mesmo IP, porta e
/// papel de um item que já está na lista é o mesmo aparelho que recomeçou a anunciar, e o substitui
/// em vez de ficar "visto há 14 s" ao lado.
fn tirar_o_anuncio_anterior(t: &mut Tabela, chave: &str, ip: IpAddr, porta: u16, anuncio: &Announcement) {
    t.entradas.retain(|k, e| {
        if k == chave || e.porta != porta || e.anuncio.papel != anuncio.papel {
            return true;
        }
        !tem_o_ip(e, ip)
    });
}

fn anotar_udp(tabela: &Mutex<Tabela>, lido: Lido, origem: SocketAddr, via: &'static str) {
    let Lido::Anuncio { anuncio, porta, aceita_codigo, nome, .. } = lido else { return };
    if !anuncio.is_compatible() {
        return;
    }
    let agora = Instant::now();
    let mut t = travar(tabela);
    // **O mesmo aparelho pelo mDNS** (revisão M-d): o anúncio UDP do mesmo token, vindo de um dos
    // endereços que o mDNS deu, junta-se ao item do mDNS em vez de virar outro.
    let do_mdns = t
        .entradas
        .iter()
        .find(|(_, e)| {
            e.anuncio.device_id == anuncio.device_id
                && e.porta == porta
                && e.mdns.as_ref().is_some_and(|d| d.addresses.iter().any(|a| a.to_ip_addr() == origem.ip()))
        })
        .map(|(k, _)| k.clone());
    let chave = do_mdns.unwrap_or_else(|| chave_da_entrada(&anuncio.device_id.0, Some(origem.ip()), porta));
    // **Teto contra inundação** (revisão de 12/10, B3): um só aparelho na LAN pode mandar anúncios
    // com mil ids. Item novo só entra abaixo de [`TETO_DE_APARELHOS`] e com no máximo
    // [`APARELHOS_POR_IP`] já vindos do mesmo IP (um aparelho anuncia vídeo e teleprompter; quatro é
    // folga). Quem já está na lista continua sendo atualizado. O teto vem **antes** de tirar o
    // anúncio anterior (revisão, baixo 6): com a lista cheia, um forjado não tira o verdadeiro.
    if !t.entradas.contains_key(&chave) {
        if nao_cabe(&t, &[origem.ip()]) {
            return;
        }
        tirar_o_anuncio_anterior(&mut t, &chave, origem.ip(), porta, &anuncio);
    }
    let e = t.entradas.entry(chave).or_insert_with(|| Entrada {
        desde: agora,
        anuncio: anuncio.clone(),
        porta,
        aceita_codigo,
        nome_anunciado: None,
        via: BTreeSet::new(),
        mdns: None,
        udp: None,
        ultimo: agora,
    });
    e.anuncio = anuncio;
    e.porta = porta;
    e.aceita_codigo = aceita_codigo;
    e.nome_anunciado = nome;
    e.via.insert(via);
    e.udp = Some((origem.ip(), agora));
    e.ultimo = agora;
}

/// Um aparelho visto (ou revisto) pelo mDNS. O item é o da instância ([`chave_do_mdns`]); o que
/// o UDP do mesmo token já tinha posto, de um dos endereços dele, passa a ser este (revisão M-d).
fn anotar_mdns(tabela: &Mutex<Tabela>, aparelho: DiscoveredDevice, aceita_codigo: bool) {
    let agora = Instant::now();
    let mut t = travar(tabela);
    let ips: Vec<IpAddr> = aparelho.addresses.iter().map(|a| a.to_ip_addr()).collect();
    let chave = chave_do_mdns(&aparelho.fullname);
    // O item que o UDP do mesmo token já pôs, de um destes endereços, passa a ser
    // este (revisão M-d): um aparelho, um item.
    let do_udp: Vec<String> = t
        .entradas
        .iter()
        .filter(|(k, e)| {
            **k != chave
                && e.mdns.is_none()
                && e.anuncio.device_id == aparelho.announcement.device_id
                && e.porta == aparelho.signaling_port
                && e.udp.is_some_and(|(ip, _)| ips.contains(&ip))
        })
        .map(|(k, _)| k.clone())
        .collect();
    let herdado = do_udp.iter().filter_map(|k| t.entradas.remove(k)).next();
    if !t.entradas.contains_key(&chave) && herdado.is_none() {
        // O mesmo teto da UDP (B3), antes de tirar o anterior: o mDNS também se forja.
        if nao_cabe(&t, &ips) {
            return;
        }
        for ip in &ips {
            tirar_o_anuncio_anterior(&mut t, &chave, *ip, aparelho.signaling_port, &aparelho.announcement);
        }
    }
    if let Some(h) = herdado {
        t.entradas.entry(chave.clone()).or_insert(h);
    }
    let e = t.entradas.entry(chave).or_insert_with(|| Entrada {
        desde: agora,
        anuncio: aparelho.announcement.clone(),
        porta: aparelho.signaling_port,
        aceita_codigo,
        nome_anunciado: None,
        via: BTreeSet::new(),
        mdns: None,
        udp: None,
        ultimo: agora,
    });
    e.anuncio = aparelho.announcement.clone();
    e.porta = aparelho.signaling_port;
    e.aceita_codigo = aceita_codigo;
    e.nome_anunciado.clone_from(&aparelho.nome_anunciado);
    e.via.insert("mdns");
    e.mdns = Some(aparelho);
    e.ultimo = agora;
}

fn varredura_travada(v: &VarreduraCompartilhada) -> MutexGuard<'_, Varredura> {
    match v.lock() {
        Ok(g) => g,
        Err(envenenado) => envenenado.into_inner(),
    }
}

/// **Falta alguém na lista?** Sem alvo: nenhum item que não seja deste processo. Com alvo: o alvo
/// não está. Puro, para os testes.
pub(crate) fn falta_alguem(itens: &[AparelhoVisto], alvo: Option<&DeviceId>) -> bool {
    match alvo {
        Some(id) => !itens.iter().any(|a| a.anuncio.device_id == *id),
        None => itens.iter().all(|a| a.proprio),
    }
}

/// Os itens que **só a sondagem vê** (sem mDNS) e que responderam por unicast há menos de
/// [`VIVO_POR`]: os únicos da lista que o sondador pergunta de novo (`sondagem`, regra 0).
fn a_refrescar(t: &Tabela, agora: Instant) -> Vec<Ipv4Addr> {
    let mut ips: Vec<Ipv4Addr> = t
        .entradas
        .values()
        .filter(|e| e.mdns.is_none())
        .filter_map(|e| match e.udp {
            // O loopback fica de fora: é outro processo desta máquina, que ouve o broadcast (e, nos
            // testes, todos os anunciantes dividem o 127.0.0.1).
            Some((IpAddr::V4(ip), quando))
                if !ip.is_loopback() && agora.saturating_duration_since(quando) <= VIVO_POR =>
            {
                Some(ip)
            }
            _ => None,
        })
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

/// Os endereços dos itens da tabela: a origem do UDP e os do mDNS.
fn enderecos_na_lista(t: &Tabela) -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = Vec::new();
    for e in t.entradas.values() {
        if let Some((ip, _)) = e.udp {
            ips.push(ip);
        }
        if let Some(d) = e.mdns.as_ref() {
            ips.extend(d.addresses.iter().map(|a| a.to_ip_addr()));
        }
    }
    ips.sort();
    ips.dedup();
    ips
}

fn laco_da_busca(opcoes: OpcoesDaBusca, numero: u64, tabela: Arc<Mutex<Tabela>>, parar: Arc<AtomicBool>) {
    let mdns = if opcoes.mdns && !laco_local_pedido() { Browser::start().ok() } else { None };
    let escuta = if opcoes.broadcast { socket_udp(opcoes.porta_da_escuta).ok() } else { None };
    travar(&tabela).escuta_aberta = escuta.is_some();
    // O socket das perguntas é efêmero: as respostas unicast voltam para ele, e não para a porta
    // fixa, que pode estar dividida com outro processo.
    let Ok(perguntas) = socket_udp(0) else {
        return;
    };
    let r = nonce();
    let corpo = pergunta(&r, opcoes.alvo.as_ref());

    let inicio = Instant::now();
    let mut ultima_pergunta: Option<Instant> = None;
    let espera = if opcoes.alvo.is_some() { SONDAR_O_ALVO_DEPOIS_DE } else { SONDAR_DEPOIS_DE };
    let mut sondador = Sondador::novo(numero, inicio, espera);
    // A dica da sessão salva é uma dica como as outras: poucas perguntas, e nenhuma se já está na lista.
    if let Some(IpAddr::V4(ip)) = opcoes.dica.map(|d| d.ip()) {
        sondador.dar_dica(ip, inicio);
    }
    let mut cursor_dos_achados: u64 = 0;
    let mut sondando = false;

    let mut perguntas_feitas: u32 = 0;

    while !parar.load(Ordering::Acquire) {
        let agora = Instant::now();

        // 1. Perguntar a todos por broadcast. Logo, de novo em meio segundo — a primeira pode chegar
        //    antes de o outro lado abrir a porta, ou se perder no rádio, e é UDP — e depois a cada
        //    `PERGUNTAR_A_CADA`. A dica (o IP antigo da sessão salva) não leva mais uma pergunta a cada
        //    volta: é do sondador (`docs/v3-no-privado.md` §8.9).
        let intervalo = if perguntas_feitas < 2 { REPERGUNTAR_LOGO } else { PERGUNTAR_A_CADA };
        if ultima_pergunta.is_none_or(|t| agora.duration_since(t) >= intervalo) {
            ultima_pergunta = Some(agora);
            perguntas_feitas = perguntas_feitas.saturating_add(1);
            if opcoes.broadcast {
                for d in destinos_de_broadcast(opcoes.porta_da_pergunta) {
                    let _ = perguntas.send_to(&corpo, d);
                }
            }
        }

        // 2. A sondagem (`docs/v3-no-privado.md` §8.9): as dicas, os vizinhos e, só como último
        //    recurso, a /24 — nunca o endereço de um item da lista nem o de um toque recente.
        if opcoes.sondar {
            let (na_lista, refrescar, falta, dicas, pedido) = {
                let mut t = travar(&tabela);
                let itens: Vec<AparelhoVisto> = t.entradas.iter().map(|(k, e)| e.retrato(agora, k)).collect();
                let falta = falta_alguem(&itens, opcoes.alvo.as_ref());
                (
                    enderecos_na_lista(&t),
                    a_refrescar(&t, agora),
                    falta,
                    std::mem::take(&mut t.dicas_novas),
                    std::mem::take(&mut t.pedido),
                )
            };
            for d in dicas {
                sondador.dar_dica(d, agora);
            }
            let tocados = sondagem::tocados_ha_pouco(agora);
            let alvos = {
                let mut v = varredura_travada(&opcoes.varredura);
                if pedido {
                    sondador.procurar_de_novo(&mut v, alvos_da_sondagem());
                }
                let m = Momento { agora, na_lista: &na_lista, falta, tocados: &tocados, a_refrescar: &refrescar };
                sondador.passo(&m, &mut v, vizinhos_da_busca, alvos_da_sondagem)
            };
            travar(&tabela).sondagens = sondador.varreduras;
            for ip in alvos {
                sondando = true;
                let _ = perguntas.send_to(&corpo, SocketAddr::from(SocketAddrV4::new(ip, opcoes.porta_da_pergunta)));
            }
        }

        // 3. O que chegou: respostas (no socket das perguntas) e anúncios (na escuta).
        let via_resposta = if sondando { "sondagem" } else { "udp" };
        let mut ouvidos: Vec<(Lido, SocketAddr)> = Vec::new();
        drenar(&perguntas, |lido, origem| {
            // Uma resposta a outra pergunta (de outra busca no mesmo processo) não é nossa, mas
            // um anúncio é um anúncio: vale do mesmo jeito.
            if sondando {
                ouvidos.push((lido.clone(), origem));
            }
            anotar_udp(&tabela, lido, origem, via_resposta)
        });
        // **O que as outras buscas do processo ouviram**: a resposta a uma pergunta da varredura volta
        // só ao socket de quem perguntou, e a varredura é dividida (`sondagem::Varredura`).
        let de_outras = {
            let mut v = varredura_travada(&opcoes.varredura);
            let (antes, _) = v.achados_depois(cursor_dos_achados, agora);
            for (lido, origem) in ouvidos {
                v.guardar_achado(lido, origem, agora);
            }
            // O cursor pula o que esta busca acabou de guardar.
            cursor_dos_achados = v.achados_depois(u64::MAX, agora).1;
            antes
        };
        for (lido, origem) in de_outras {
            anotar_udp(&tabela, lido, origem, "sondagem");
        }
        if let Some(e) = escuta.as_ref() {
            drenar(e, |lido, origem| anotar_udp(&tabela, lido, origem, "udp"));
        }

        // 4. O mDNS, pelo tempo que sobra da fatia.
        match mdns.as_ref() {
            Some(b) => match b.next_event_com_codigo(FATIA) {
                Ok(Some((DiscoveryEvent::Found(aparelho), aceita_codigo))) => {
                    anotar_mdns(&tabela, *aparelho, aceita_codigo);
                }
                Ok(Some((DiscoveryEvent::Lost(fullname), _))) => {
                    // Um `Lost` não tira ninguém na hora: o item passa a "visto há X s".
                    let agora = Instant::now();
                    let mut t = travar(&tabela);
                    for e in t.entradas.values_mut() {
                        if e.mdns.as_ref().is_some_and(|d| d.fullname == fullname) {
                            e.mdns = None;
                            e.ultimo = agora;
                        }
                    }
                }
                Ok(None) => {}
                Err(_) => std::thread::sleep(FATIA),
            },
            None => std::thread::sleep(FATIA),
        }

        // 5. Esquecer quem passou do tempo.
        let agora = Instant::now();
        travar(&tabela)
            .entradas
            .retain(|_, e| e.mdns.is_some() || agora.duration_since(e.ultimo) <= VIVO_POR + LEMBRADO_POR);
    }
    if let Some(b) = mdns {
        b.stop();
    }
}

// =============================================================================================
// Item 2: achar o aparelho da sessão salva pelo id
// =============================================================================================

/// Onde o aparelho está agora. Ver [`localizar`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Localizado {
    pub endpoint: SocketAddr,
    /// `"endereco_antigo"`, `"udp"`, `"sondagem"` ou `"mdns"`.
    pub via: &'static str,
    /// O endereço não é mais o da dica: a casca grava o novo, sem perguntar.
    pub mudou: bool,
    pub aceita_codigo: bool,
}

/// Quanto o endereço antigo tem para atender por TCP.
pub const PRAZO_DO_ENDERECO_ANTIGO: Duration = Duration::from_secs(1);
/// Depois do TCP antigo atender, quanto ainda se espera uma resposta com id que confirme (ou
/// desminta) que ali é o mesmo aparelho.
#[allow(dead_code)] // de `localizar_com`, que volta na etapa 3 de `docs/v3-no-privado.md`
const CONFIRMACAO_DO_ENDERECO_ANTIGO: Duration = Duration::from_millis(150);

/// **Acha o aparelho da sessão salva pelo `DeviceId`** (`docs/conexao.md` §3).
///
/// Ao mesmo tempo: TCP no endereço antigo com prazo de 1 s, pergunta dirigida por unicast ao IP
/// antigo e por broadcast, mDNS e, se nada em 1 s, uma sondagem dirigida da /24. Vence a primeira
/// resposta com o id certo. O endereço antigo que só atende o TCP (anfitrião de build anterior)
/// vence se ninguém disser, desse mesmo IP, que ali é outro aparelho.
///
/// **Desligado sobre o v3** até a etapa 3 de `docs/v3-no-privado.md`: o anúncio não traz mais o id,
/// e a procura dirigida o mandava em claro. Sempre `Invalid`; a casca conecta no endereço salvo.
pub fn localizar(id: &DeviceId, ultimo: Option<SocketAddr>, prazo: Duration) -> Result<Localizado> {
    let _ = (id, ultimo, prazo);
    Err(Error::Invalid(
        "localizar pelo id volta sobre o protocolo v3 (docs/v3-no-privado.md, etapa 3)".into(),
    ))
}

#[allow(dead_code)] // volta pelo `Challenge` na etapa 3 de `docs/v3-no-privado.md`; os testes antigos ficam ignorados
pub(crate) fn localizar_com(
    id: &DeviceId,
    ultimo: Option<SocketAddr>,
    prazo: Duration,
    mut opcoes: OpcoesDaBusca,
) -> Result<Localizado> {
    let fim = Instant::now() + prazo;
    opcoes.alvo = Some(id.clone());
    opcoes.dica = ultimo;
    let porta_da_pergunta = opcoes.porta_da_pergunta;
    let busca = Busca::com(opcoes);

    // **Quem está no endereço antigo?** Uma pergunta **não** dirigida, só para ele: o TCP atender
    // não prova que é o mesmo aparelho (o DHCP pode ter dado o IP a outro Quall), e a pergunta
    // dirigida da busca não ouve os outros.
    let quem_esta_la = socket_udp(0).ok();
    if let (Some(s), Some(antigo)) = (quem_esta_la.as_ref(), ultimo) {
        let _ = s.send_to(&pergunta(&nonce(), None), SocketAddr::new(antigo.ip(), porta_da_pergunta));
    }
    let mut outro_no_antigo = false;

    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(antigo) = ultimo {
        std::thread::spawn(move || {
            let ok = TcpStream::connect_timeout(&antigo, PRAZO_DO_ENDERECO_ANTIGO).is_ok();
            let _ = tx.send((ok, Instant::now()));
        });
    }
    let mut antigo_atendeu: Option<Instant> = None;

    loop {
        if let Some(a) = busca.aparelho(id).filter(|a| a.vivo) {
            if let Some(endpoint) = a.endpoint {
                let via = a.via.iter().copied().find(|v| *v != "mdns").unwrap_or("mdns");
                return Ok(Localizado {
                    endpoint,
                    via,
                    mudou: ultimo != Some(endpoint),
                    aceita_codigo: a.aceita_codigo,
                });
            }
        }
        if let Ok((true, quando)) = rx.try_recv() {
            antigo_atendeu = Some(quando);
            // De novo, agora que se sabe que há alguém ali: a primeira pode ter se perdido.
            if let (Some(s), Some(antigo)) = (quem_esta_la.as_ref(), ultimo) {
                let _ = s.send_to(&pergunta(&nonce(), None), SocketAddr::new(antigo.ip(), porta_da_pergunta));
            }
        }
        if let (Some(s), Some(antigo)) = (quem_esta_la.as_ref(), ultimo) {
            drenar(s, |lido, origem| {
                if let Lido::Anuncio { anuncio, porta, .. } = lido {
                    if origem.ip() == antigo.ip() && porta == antigo.port() && anuncio.device_id != *id {
                        outro_no_antigo = true;
                    }
                }
            });
        }
        if let (Some(quando), Some(antigo)) = (antigo_atendeu, ultimo) {
            let outro_ali = outro_no_antigo
                || busca.aparelhos().iter().any(|a| {
                    a.anuncio.device_id != *id && a.endpoint == Some(antigo)
                });
            if outro_ali {
                // O DHCP deu o IP antigo a outro Quall: o TCP não serve de prova.
                antigo_atendeu = None;
            } else if quando.elapsed() >= CONFIRMACAO_DO_ENDERECO_ANTIGO {
                return Ok(Localizado {
                    endpoint: antigo,
                    via: "endereco_antigo",
                    mudou: false,
                    aceita_codigo: false,
                });
            }
        }
        if Instant::now() >= fim {
            let d = busca.diagnostico();
            let causas: Vec<&str> = d.causas.iter().map(|c| c.como_texto()).collect();
            return Err(Error::Timeout(format!(
                "o aparelho {} não respondeu na rede (causas prováveis: {})",
                id.0,
                if causas.is_empty() { "nenhuma identificada".to_string() } else { causas.join(", ") }
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests;
