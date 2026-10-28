//! **O que a busca sonda, e quando** (`docs/v3-no-privado.md` §8.9, "pode aliviar a varredura").
//!
//! A sondagem unicast acha quem o multicast e o broadcast não trazem, mas cada datagrama para um
//! endereço sem ninguém é um ARP que falha: no macOS e no iOS o kernel guarda o endereço como
//! incompleto e, por `host_down_time`, fora do ar — e o primeiro toque num celular que cochilou
//! naquela hora morria com `NO_ROUTE` (§8.8). Daí a ordem, das fontes mais baratas para a mais cara:
//!
//! 0. **nunca** o endereço de um item da lista, nem o de um toque recente ([`avisar_do_toque`]) —
//!    menos o item que **respondeu por unicast há pouco** e não tem mDNS (o iPhone, sem o entitlement
//!    de multicast, só vê assim): esse leva uma pergunta a cada [`REFRESCAR_A_CADA`] para seguir vivo.
//!    Ele acabou de responder, o ARP dele está completo, e a pergunta não pede ARP novo;
//! 1. as **dicas** (o último endereço de um vínculo, que a casca dá): poucas vezes;
//! 2. os **vizinhos** da tabela ARP do sistema, só as entradas completas (não pedem ARP novo);
//! 3. a **varredura** da /24 — uma por processo, no ritmo de [`RITMO_DA_VARREDURA`], sozinha só com a
//!    lista vazia (e, começada, até o fim), ou quando a pessoa toca "Procurar de novo".
//!
//! Tudo aqui é puro: o tempo entra por parâmetro, e os testes andam o relógio à mão.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use super::Lido;

/// Sem ninguém na lista depois disto, a varredura automática começa (ou continua a que parou). O
/// mDNS e as perguntas por broadcast respondem em milissegundos: 2 s sem ninguém é "ninguém por esses
/// caminhos".
pub const SONDAR_DEPOIS_DE: Duration = Duration::from_secs(2);
/// Na procura de um aparelho só (o alvo), mais cedo: o IP antigo já teve 1 s.
pub const SONDAR_O_ALVO_DEPOIS_DE: Duration = Duration::from_secs(1);
/// Uma pergunta da varredura a cada isto, em média: 50 por segundo, ~5 s por /24 (antes, 25 a cada
/// 50 ms, ~0,5 s por /24).
pub const RITMO_DA_VARREDURA: Duration = Duration::from_millis(20);
/// Quantas perguntas da varredura podem sair juntas, quando o laço atrasou.
pub const RAJADA_DA_VARREDURA: u32 = 3;
/// Uma varredura completa não se repete sozinha antes disto; só o "Procurar de novo" recomeça.
pub const REPETIR_A_VARREDURA_DEPOIS_DE: Duration = Duration::from_secs(5 * 60);
/// A primeira pergunta a uma dica sai depois disto (o mDNS tem a chance de trazê-la antes).
pub const DICA_DEPOIS_DE: Duration = Duration::from_secs(1);
/// E as seguintes, contadas da anterior. Três perguntas por dica, no máximo, por busca.
pub const DICA_DE_NOVO: [Duration; 2] = [Duration::from_secs(20), Duration::from_secs(40)];
/// A tabela ARP é lida de novo depois disto, e cada vizinho é perguntado no máximo uma vez nisto.
pub const VIZINHOS_A_CADA: Duration = Duration::from_secs(15);
/// O item que só a sondagem vê (sem mDNS) e respondeu há pouco é perguntado de novo a cada isto.
pub const REFRESCAR_A_CADA: Duration = Duration::from_secs(5);
/// Quanto tempo o endereço de um toque fica fora da sondagem.
pub const TOQUE_PROTEGE_POR: Duration = Duration::from_secs(60);
const TOQUES_GUARDADOS: usize = 16;
/// O que uma busca ouviu fica este tempo para as outras do processo (o anúncio vale [`super::VIVO_POR`]).
const ACHADOS_VALEM_POR: Duration = super::VIVO_POR;
const ACHADOS_NO_MAXIMO: usize = 64;

fn travar<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// =============================================================================================
// Os toques
// =============================================================================================

static TOQUES: Mutex<Vec<(IpAddr, Instant)>> = Mutex::new(Vec::new());

/// **Um toque vai conectar em `ip`**: a sondagem (e o reconhecedor) o deixam em paz por
/// [`TOQUE_PROTEGE_POR`]. Chamado pelo núcleo em toda conexão de saída. O loopback não tem ARP e
/// fica de fora (os testes usam).
pub fn avisar_do_toque(ip: IpAddr) {
    if ip.is_loopback() || ip.is_unspecified() {
        return;
    }
    let agora = Instant::now();
    let mut t = travar(&TOQUES);
    t.retain(|(i, quando)| *i != ip && agora.duration_since(*quando) < TOQUE_PROTEGE_POR);
    if t.len() >= TOQUES_GUARDADOS {
        t.remove(0);
    }
    t.push((ip, agora));
}

/// Os endereços tocados há menos de [`TOQUE_PROTEGE_POR`].
pub(crate) fn tocados_ha_pouco(agora: Instant) -> Vec<IpAddr> {
    travar(&TOQUES)
        .iter()
        .filter(|(_, quando)| agora.saturating_duration_since(*quando) < TOQUE_PROTEGE_POR)
        .map(|(ip, _)| *ip)
        .collect()
}

/// `ip` foi tocado há menos de [`TOQUE_PROTEGE_POR`].
pub fn tocado_ha_pouco(ip: IpAddr) -> bool {
    tocados_ha_pouco(Instant::now()).contains(&ip)
}

// =============================================================================================
// Os vizinhos
// =============================================================================================

/// Quem lê a tabela ARP do sistema: os endereços IPv4 com entrada **completa** (com endereço físico).
pub type LeitorDeVizinhos = fn() -> Vec<Ipv4Addr>;

static LEITOR: OnceLock<Mutex<Option<LeitorDeVizinhos>>> = OnceLock::new();

/// **O leitor da tabela ARP desta plataforma** — a fronteira C põe o do macOS/iOS (`sysctl`), o app
/// do Windows o dele (`GetIpNetTable2`): o núcleo não tem `unsafe`. Sem leitor posto, o Linux (e o
/// Android, que recebe o erro) lê `/proc/net/arp`; as outras plataformas ficam sem vizinhos.
pub fn usar_leitor_de_vizinhos(leitor: LeitorDeVizinhos) {
    *travar(LEITOR.get_or_init(|| Mutex::new(None))) = Some(leitor);
}

/// A tabela ARP, crua (quem chama filtra pela rede).
pub(crate) fn ler_vizinhos() -> Vec<Ipv4Addr> {
    if let Some(f) = *travar(LEITOR.get_or_init(|| Mutex::new(None))) {
        return f();
    }
    if cfg!(any(target_os = "linux", target_os = "android")) {
        return std::fs::read_to_string("/proc/net/arp").map(|t| vizinhos_do_proc(&t)).unwrap_or_default();
    }
    Vec::new()
}

/// `/proc/net/arp`: `IP address  HW type  Flags  HW address  Mask  Device`, flag `0x2` = completa.
pub(crate) fn vizinhos_do_proc(texto: &str) -> Vec<Ipv4Addr> {
    texto
        .lines()
        .skip(1)
        .filter_map(|linha| {
            let mut col = linha.split_whitespace();
            let ip: Ipv4Addr = col.next()?.parse().ok()?;
            let _tipo = col.next()?;
            let flags = u32::from_str_radix(col.next()?.trim_start_matches("0x"), 16).ok()?;
            let mac = col.next()?;
            (flags & 0x2 != 0 && mac != "00:00:00:00:00:00").then_some(ip)
        })
        .collect()
}

// =============================================================================================
// A varredura do processo
// =============================================================================================

static PROXIMA_BUSCA: AtomicU64 = AtomicU64::new(1);

/// Um número por busca, para a varredura saber quem a pediu.
pub(crate) fn numero_da_busca() -> u64 {
    PROXIMA_BUSCA.fetch_add(1, Ordering::Relaxed)
}

/// **A varredura da /24, uma por processo.** As buscas a dividem: uma varredura interrompida (a lista
/// fechou) continua na próxima busca vazia, e uma completa não se repete sozinha antes de
/// [`REPETIR_A_VARREDURA_DEPOIS_DE`]. O que uma busca ouve vai para [`Varredura::achados`], e as
/// outras do processo o leem: a resposta a uma pergunta volta só ao socket de quem perguntou.
#[derive(Debug, Default)]
pub struct Varredura {
    /// O que falta perguntar, de trás para frente.
    fila: Vec<Ipv4Addr>,
    em_curso: bool,
    /// A busca que pediu "Procurar de novo": só ela anda com a lista cheia.
    dono: Option<u64>,
    terminou: Option<Instant>,
    /// O ritmo (GCRA): quando a próxima pergunta "devia" sair.
    tat: Option<Instant>,
    feitas: u32,
    achados: Vec<(u64, Lido, SocketAddr, Instant)>,
    seq: u64,
}

/// O estado da varredura que uma busca usa: o do processo ou, nos testes, um próprio.
pub type VarreduraCompartilhada = Arc<Mutex<Varredura>>;

static DO_PROCESSO: OnceLock<VarreduraCompartilhada> = OnceLock::new();

impl Varredura {
    /// A do processo — **menos no laço local** (os testes, `so_o_laco_local`), em que cada busca tem
    /// a sua: testes em paralelo não podem dividir a fila nem o intervalo de 5 min.
    pub fn do_processo() -> VarreduraCompartilhada {
        if super::no_laco_local() {
            return Self::nova();
        }
        DO_PROCESSO.get_or_init(Self::nova).clone()
    }

    /// Uma varredura só desta busca.
    pub fn nova() -> VarreduraCompartilhada {
        Arc::new(Mutex::new(Varredura::default()))
    }

    /// Quantas varreduras completas o processo já fez.
    pub fn feitas(&self) -> u32 {
        self.feitas
    }

    pub fn em_curso(&self) -> bool {
        self.em_curso
    }

    fn comecar(&mut self, mut alvos: Vec<Ipv4Addr>, dono: Option<u64>) {
        alvos.reverse();
        self.fila = alvos;
        self.em_curso = !self.fila.is_empty();
        self.dono = dono.filter(|_| self.em_curso);
    }

    /// A busca `dono` saiu: a varredura dela, se ainda anda, passa a ser a automática.
    pub(crate) fn largar(&mut self, dono: u64) {
        if self.dono == Some(dono) {
            self.dono = None;
        }
    }

    /// O próximo endereço, se o ritmo deixa; pula (e gasta) os de `pular`.
    fn proximo(&mut self, agora: Instant, pular: impl Fn(Ipv4Addr) -> bool) -> Option<Ipv4Addr> {
        if !self.em_curso {
            return None;
        }
        let tat = self.tat.map_or(agora, |t| t.max(agora));
        if tat.saturating_duration_since(agora) > RITMO_DA_VARREDURA * (RAJADA_DA_VARREDURA - 1) {
            return None;
        }
        while let Some(ip) = self.fila.pop() {
            if pular(ip) {
                continue;
            }
            self.tat = Some(tat + RITMO_DA_VARREDURA);
            if self.fila.is_empty() {
                self.terminar(agora);
            }
            return Some(ip);
        }
        self.terminar(agora);
        None
    }

    fn terminar(&mut self, agora: Instant) {
        self.em_curso = false;
        self.dono = None;
        self.terminou = Some(agora);
        self.feitas = self.feitas.saturating_add(1);
    }

    /// Guarda o que uma busca ouviu, para as outras.
    pub(crate) fn guardar_achado(&mut self, lido: Lido, origem: SocketAddr, agora: Instant) {
        self.achados.retain(|(_, _, _, t)| agora.saturating_duration_since(*t) <= ACHADOS_VALEM_POR);
        if self.achados.len() >= ACHADOS_NO_MAXIMO {
            self.achados.remove(0);
        }
        self.seq = self.seq.saturating_add(1);
        self.achados.push((self.seq, lido, origem, agora));
    }

    /// O que chegou depois de `cursor` (e ainda vale), e o cursor novo.
    pub(crate) fn achados_depois(&self, cursor: u64, agora: Instant) -> (Vec<(Lido, SocketAddr)>, u64) {
        let novos = self
            .achados
            .iter()
            .filter(|(s, _, _, t)| *s > cursor && agora.saturating_duration_since(*t) <= ACHADOS_VALEM_POR)
            .map(|(_, l, o, _)| (l.clone(), *o))
            .collect();
        (novos, self.seq)
    }
}

// =============================================================================================
// O que uma busca sonda
// =============================================================================================

#[derive(Debug, Clone)]
struct Dica {
    ip: Ipv4Addr,
    desde: Instant,
    vezes: usize,
    ultima: Option<Instant>,
}

/// O que a busca sabe agora, para decidir.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Momento<'a> {
    pub agora: Instant,
    /// Os endereços dos itens da lista (vivos ou "vistos há X s").
    pub na_lista: &'a [IpAddr],
    /// Falta alguém: a lista não tem item que não seja deste processo (ou falta o alvo).
    pub falta: bool,
    /// Os endereços de toques recentes ([`tocados_ha_pouco`]).
    pub tocados: &'a [IpAddr],
    /// Os itens sem mDNS que responderam por unicast há pouco: os únicos da lista que se pergunta.
    pub a_refrescar: &'a [Ipv4Addr],
}

/// **As regras de uma busca**: dicas, vizinhos e a parte dela na varredura. Ver o cabeçalho.
#[derive(Debug)]
pub(crate) struct Sondador {
    numero: u64,
    inicio: Instant,
    espera: Duration,
    dicas: Vec<Dica>,
    vizinhos: Vec<Ipv4Addr>,
    lidos_em: Option<Instant>,
    /// Quem as dicas e os vizinhos já perguntaram, e quando: a varredura os pula.
    perguntados: HashMap<Ipv4Addr, Instant>,
    refrescados: HashMap<Ipv4Addr, Instant>,
    decidiu: bool,
    /// Esta busca anda a varredura automática (começou-a, ou pegou a que parou): até o fim.
    varrendo: bool,
    /// Varreduras de que esta busca participou (ou que encontrou feitas há pouco): o diagnóstico.
    pub varreduras: u32,
}

impl Sondador {
    pub(crate) fn novo(numero: u64, inicio: Instant, espera: Duration) -> Self {
        Sondador {
            numero,
            inicio,
            espera,
            dicas: Vec::new(),
            vizinhos: Vec::new(),
            lidos_em: None,
            perguntados: HashMap::new(),
            refrescados: HashMap::new(),
            decidiu: false,
            varrendo: false,
            varreduras: 0,
        }
    }

    /// Uma dica nova (repetida não conta de novo).
    pub(crate) fn dar_dica(&mut self, ip: Ipv4Addr, agora: Instant) {
        if !self.dicas.iter().any(|d| d.ip == ip) {
            self.dicas.push(Dica { ip, desde: agora, vezes: 0, ultima: None });
        }
    }

    /// "Procurar de novo": recomeça a varredura agora, com esta busca de dona.
    pub(crate) fn procurar_de_novo(&mut self, varredura: &mut Varredura, alvos: Vec<Ipv4Addr>) {
        varredura.comecar(alvos, Some(self.numero));
        self.decidiu = true;
        self.varreduras = self.varreduras.saturating_add(1);
    }

    /// **O que perguntar agora.** `ler_vizinhos` e `alvos` (a /24) só são chamados quando precisa.
    pub(crate) fn passo(
        &mut self,
        m: &Momento<'_>,
        varredura: &mut Varredura,
        ler_vizinhos: impl FnOnce() -> Vec<Ipv4Addr>,
        alvos: impl FnOnce() -> Vec<Ipv4Addr>,
    ) -> Vec<Ipv4Addr> {
        let agora = m.agora;
        let evitar = |ip: Ipv4Addr| {
            let ip = IpAddr::V4(ip);
            !ip.is_loopback() && (m.na_lista.contains(&ip) || m.tocados.contains(&ip))
        };
        let mut saida = Vec::new();

        // 0. Quem só a sondagem vê e acabou de responder: segue vivo.
        for ip in m.a_refrescar {
            if m.tocados.contains(&IpAddr::V4(*ip))
                || self.refrescados.get(ip).is_some_and(|t| agora.saturating_duration_since(*t) < REFRESCAR_A_CADA)
            {
                continue;
            }
            self.refrescados.insert(*ip, agora);
            saida.push(*ip);
        }
        self.refrescados.retain(|ip, _| m.a_refrescar.contains(ip));

        // 1. As dicas.
        for d in &mut self.dicas {
            if d.vezes > DICA_DE_NOVO.len() || evitar(d.ip) {
                continue;
            }
            let vez = match d.ultima {
                None => agora.saturating_duration_since(d.desde) >= DICA_DEPOIS_DE,
                Some(u) => agora.saturating_duration_since(u) >= DICA_DE_NOVO[d.vezes - 1],
            };
            if vez {
                d.vezes += 1;
                d.ultima = Some(agora);
                self.perguntados.insert(d.ip, agora);
                saida.push(d.ip);
            }
        }

        // 2. Os vizinhos.
        if self.lidos_em.is_none_or(|t| agora.saturating_duration_since(t) >= VIZINHOS_A_CADA) {
            self.vizinhos = ler_vizinhos();
            self.lidos_em = Some(agora);
        }
        for ip in &self.vizinhos {
            if evitar(*ip)
                || saida.contains(ip)
                || self.perguntados.get(ip).is_some_and(|t| agora.saturating_duration_since(*t) < VIZINHOS_A_CADA)
            {
                continue;
            }
            self.perguntados.insert(*ip, agora);
            saida.push(*ip);
        }

        // 3. A varredura: decide uma vez, depois da espera, se a lista ficou vazia. Começada (ou pega a
        //    que parou noutra busca), vai até o fim: no iPhone o primeiro achado não diz nada dos outros.
        let passou = agora.saturating_duration_since(self.inicio) >= self.espera;
        if !self.decidiu && m.falta && passou {
            self.decidiu = true;
            self.varreduras = self.varreduras.saturating_add(1);
            let recente = varredura
                .terminou
                .is_some_and(|t| agora.saturating_duration_since(t) < REPETIR_A_VARREDURA_DEPOIS_DE);
            if !varredura.em_curso && !recente {
                varredura.comecar(alvos(), None);
            }
            self.varrendo = varredura.em_curso;
        }
        // Anda se é a dona, ou se é a automática que esta busca pegou.
        let anda = match varredura.dono {
            Some(d) => d == self.numero,
            None => self.varrendo,
        };
        if anda {
            let perguntados = &self.perguntados;
            while let Some(ip) = varredura.proximo(agora, |ip| evitar(ip) || perguntados.contains_key(&ip)) {
                saida.push(ip);
            }
        }
        saida
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(d: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 15, d)
    }

    fn a24() -> Vec<Ipv4Addr> {
        (1..=254).filter(|d| *d != 9).map(ip).collect()
    }

    struct Relogio {
        t0: Instant,
    }
    impl Relogio {
        fn em(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
    }

    fn momento<'a>(agora: Instant, na_lista: &'a [IpAddr], falta: bool) -> Momento<'a> {
        Momento { agora, na_lista, falta, tocados: &[], a_refrescar: &[] }
    }

    /// Anda o sondador de 50 em 50 ms (o laço da busca) e junta o que saiu, com a hora.
    #[allow(clippy::too_many_arguments)]
    fn andar(
        s: &mut Sondador,
        v: &mut Varredura,
        r: &Relogio,
        de: u64,
        ate: u64,
        na_lista: &[IpAddr],
        falta: bool,
        vizinhos: &[Ipv4Addr],
    ) -> Vec<(u64, Ipv4Addr)> {
        let mut saida = Vec::new();
        let mut t = de;
        while t < ate {
            let m = momento(r.em(t), na_lista, falta);
            for i in s.passo(&m, v, || vizinhos.to_vec(), a24) {
                saida.push((t, i));
            }
            t += 50;
        }
        saida
    }

    #[test]
    fn com_a_lista_cheia_nada_de_varredura_so_vizinhos() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        let lista = [IpAddr::V4(ip(6))];
        let saida = andar(&mut s, &mut v, &r, 0, 60_000, &lista, false, &[ip(1), ip(6), ip(8)]);
        // O 6 está na lista: nunca. O 1 e o 8: aos 0 s e a cada 15 s (0, 15, 30, 45).
        assert!(saida.iter().all(|(_, i)| *i != ip(6)));
        assert_eq!(saida.iter().filter(|(_, i)| *i == ip(8)).count(), 4);
        assert_eq!(saida.len(), 8);
        assert!(!v.em_curso() && v.feitas() == 0);
        assert_eq!(s.varreduras, 0);
    }

    #[test]
    fn lista_vazia_varre_uma_vez_depois_de_2_s_no_ritmo() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        let saida = andar(&mut s, &mut v, &r, 0, 60_000, &[], true, &[]);
        assert_eq!(saida.len(), 253, "a /24 inteira, uma vez");
        assert!(saida.iter().all(|(t, _)| *t >= 2_000), "nada antes de 2 s");
        let ultimo = saida.last().map(|(t, _)| *t).unwrap_or(0);
        // 50 por segundo: 253 em ~5 s, e nunca mais de 3 na mesma volta.
        assert!((6_500..=8_000).contains(&ultimo), "terminou aos {ultimo} ms");
        let mut por_volta: HashMap<u64, usize> = HashMap::new();
        for (t, _) in &saida {
            *por_volta.entry(*t).or_default() += 1;
        }
        assert!(por_volta.values().all(|n| *n <= RAJADA_DA_VARREDURA as usize));
        assert_eq!(v.feitas(), 1);
        assert_eq!(s.varreduras, 1);
    }

    #[test]
    fn a_varredura_nao_se_repete_e_outra_busca_nao_recomeca_em_5_min() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        andar(&mut s, &mut v, &r, 0, 10_000, &[], true, &[]);
        assert_eq!(v.feitas(), 1);
        // Fecha e abre a lista aos 60 s: nada.
        let mut s2 = Sondador::novo(2, r.em(60_000), SONDAR_DEPOIS_DE);
        assert!(andar(&mut s2, &mut v, &r, 60_000, 120_000, &[], true, &[]).is_empty());
        assert_eq!(s2.varreduras, 1, "conta a de há pouco, para o diagnóstico");
        // Depois de 5 min, sim.
        let mut s3 = Sondador::novo(3, r.em(400_000), SONDAR_DEPOIS_DE);
        assert_eq!(andar(&mut s3, &mut v, &r, 400_000, 410_000, &[], true, &[]).len(), 253);
    }

    #[test]
    fn a_interrompida_continua_na_proxima_busca_vazia() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        // A procura de 5 s do Android: a varredura anda de 2 s a 5 s.
        let primeira = andar(&mut s, &mut v, &r, 0, 5_000, &[], true, &[]);
        assert!(primeira.len() > 100 && primeira.len() < 253);
        assert!(v.em_curso());
        let mut s2 = Sondador::novo(2, r.em(6_000), SONDAR_DEPOIS_DE);
        let segunda = andar(&mut s2, &mut v, &r, 6_000, 20_000, &[], true, &[]);
        assert_eq!(primeira.len() + segunda.len(), 253, "nenhum endereço duas vezes");
        assert!(segunda.iter().all(|(t, _)| *t >= 8_000), "a próxima também espera os 2 s dela");
        assert_eq!(v.feitas(), 1);
    }

    /// No iPhone só a sondagem acha: o primeiro achado não para a varredura, que vai até o fim, uma vez.
    #[test]
    fn comecada_a_automatica_vai_ate_o_fim_mesmo_com_alguem_na_lista() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        let antes = andar(&mut s, &mut v, &r, 0, 3_000, &[], true, &[]);
        assert!(!antes.is_empty());
        let lista = [IpAddr::V4(ip(6))];
        let depois = andar(&mut s, &mut v, &r, 3_000, 60_000, &lista, false, &[]);
        assert_eq!(antes.len() + depois.len(), 253);
        assert_eq!(v.feitas(), 1);
    }

    /// Uma busca que abre com a lista cheia não pega a varredura que outra deixou pela metade.
    #[test]
    fn a_interrompida_nao_anda_numa_busca_com_a_lista_cheia() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        andar(&mut s, &mut v, &r, 0, 3_000, &[], true, &[]);
        assert!(v.em_curso());
        let lista = [IpAddr::V4(ip(6))];
        let mut s2 = Sondador::novo(2, r.em(4_000), SONDAR_DEPOIS_DE);
        assert!(andar(&mut s2, &mut v, &r, 4_000, 30_000, &lista, false, &[]).is_empty());
    }

    /// O item que só a sondagem vê segue vivo: uma pergunta a cada 5 s, e nenhuma se foi tocado.
    #[test]
    fn o_item_so_da_sondagem_e_refrescado_e_o_tocado_nao() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        let lista = [IpAddr::V4(ip(6)), IpAddr::V4(ip(7))];
        let refrescar = [ip(6), ip(7)];
        let tocados = [IpAddr::V4(ip(7))];
        let mut saida = Vec::new();
        let mut t = 0;
        while t < 30_000 {
            let m = Momento { agora: r.em(t), na_lista: &lista, falta: false, tocados: &tocados, a_refrescar: &refrescar };
            saida.extend(s.passo(&m, &mut v, Vec::new, a24));
            t += 50;
        }
        assert_eq!(saida.iter().filter(|i| **i == ip(6)).count(), 6, "0, 5, 10, 15, 20, 25 s");
        assert!(!saida.contains(&ip(7)));
    }

    #[test]
    fn procurar_de_novo_varre_na_hora_com_a_lista_cheia_e_so_a_dona_anda() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let lista = [IpAddr::V4(ip(6))];
        let mut outra = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        let mut s = Sondador::novo(2, r.t0, SONDAR_DEPOIS_DE);
        s.procurar_de_novo(&mut v, a24());
        let mut da_dona = Vec::new();
        let mut da_outra = Vec::new();
        let mut t = 0;
        while t < 10_000 {
            da_outra.extend(outra.passo(&momento(r.em(t), &lista, false), &mut v, Vec::new, a24));
            da_dona.extend(s.passo(&momento(r.em(t), &lista, false), &mut v, Vec::new, a24));
            t += 50;
        }
        assert!(da_outra.is_empty());
        assert_eq!(da_dona.len(), 252, "a /24 menos o próprio e o da lista");
        assert!(!da_dona.contains(&ip(6)));
        assert_eq!(v.feitas(), 1);
    }

    #[test]
    fn a_dona_que_sai_deixa_a_varredura_para_a_automatica() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(7, r.t0, SONDAR_DEPOIS_DE);
        s.procurar_de_novo(&mut v, a24());
        let parte = andar(&mut s, &mut v, &r, 0, 1_000, &[], false, &[]);
        v.largar(7);
        let mut s2 = Sondador::novo(8, r.em(2_000), SONDAR_DEPOIS_DE);
        let resto = andar(&mut s2, &mut v, &r, 2_000, 20_000, &[], true, &[]);
        assert_eq!(parte.len() + resto.len(), 253);
    }

    #[test]
    fn o_endereco_tocado_e_o_da_lista_nunca_sao_sondados() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        s.dar_dica(ip(7), r.t0);
        let tocados = [IpAddr::V4(ip(7)), IpAddr::V4(ip(20))];
        let mut saida = Vec::new();
        let mut t = 0;
        while t < 120_000 {
            let m = Momento { agora: r.em(t), na_lista: &[], falta: true, tocados: &tocados, a_refrescar: &[] };
            saida.extend(s.passo(&m, &mut v, || vec![ip(20)], a24));
            t += 50;
        }
        assert!(!saida.contains(&ip(7)) && !saida.contains(&ip(20)));
        assert_eq!(saida.len(), 251);
    }

    #[test]
    fn a_dica_sai_tres_vezes_e_nao_se_estiver_na_lista() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        s.dar_dica(ip(30), r.t0);
        s.dar_dica(ip(31), r.t0);
        let lista = [IpAddr::V4(ip(6)), IpAddr::V4(ip(31))];
        let saida = andar(&mut s, &mut v, &r, 0, 300_000, &lista, false, &[]);
        let vezes: Vec<u64> = saida.iter().filter(|(_, i)| *i == ip(30)).map(|(t, _)| *t).collect();
        assert_eq!(vezes, vec![1_000, 21_000, 61_000]);
        assert!(saida.iter().all(|(_, i)| *i != ip(31)));
    }

    #[test]
    fn a_varredura_pula_os_vizinhos_e_as_dicas_ja_perguntados() {
        let r = Relogio { t0: Instant::now() };
        let mut v = Varredura::default();
        let mut s = Sondador::novo(1, r.t0, SONDAR_DEPOIS_DE);
        s.dar_dica(ip(30), r.t0);
        let saida = andar(&mut s, &mut v, &r, 0, 15_000, &[], true, &[ip(1), ip(8)]);
        for alvo in [ip(1), ip(8), ip(30)] {
            assert_eq!(saida.iter().filter(|(_, i)| *i == alvo).count(), 1, "{alvo}");
        }
        assert_eq!(saida.len(), 253);
    }

    #[test]
    fn os_achados_de_uma_busca_valem_para_as_outras() {
        let t0 = Instant::now();
        let mut v = Varredura::default();
        let lido = Lido::Pergunta { r: "ab".into(), id: None };
        let origem: SocketAddr = "192.168.15.7:7875".parse().expect("endereço");
        v.guardar_achado(lido.clone(), origem, t0);
        let (novos, cursor) = v.achados_depois(0, t0 + Duration::from_secs(1));
        assert_eq!(novos, vec![(lido, origem)]);
        assert!(v.achados_depois(cursor, t0 + Duration::from_secs(1)).0.is_empty());
        assert!(v.achados_depois(0, t0 + Duration::from_secs(60)).0.is_empty(), "velho não vale");
    }

    #[test]
    fn proc_net_arp_so_as_completas() {
        let texto = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.168.15.1     0x1         0x2         44:89:6d:2d:21:30     *        wlan0\n\
                     192.168.15.3     0x1         0x0         00:00:00:00:00:00     *        wlan0\n\
                     192.168.15.8     0x1         0x6         e4:54:e8:01:78:b4     *        wlan0\n";
        assert_eq!(vizinhos_do_proc(texto), vec![ip(1), ip(8)]);
    }

    #[test]
    fn o_toque_protege_o_endereco_e_o_loopback_fica_de_fora() {
        let alvo = IpAddr::V4(Ipv4Addr::new(10, 99, 0, 77));
        avisar_do_toque(alvo);
        avisar_do_toque(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(tocado_ha_pouco(alvo));
        assert!(!tocado_ha_pouco(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!tocados_ha_pouco(Instant::now() + TOQUE_PROTEGE_POR + Duration::from_secs(1)).contains(&alvo));
    }
}
