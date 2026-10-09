// Os identificadores e endereços de exemplos/fixtures são sintéticos; não identificam a bancada privada.
//! Descoberta de aparelhos na LAN por mDNS/Bonjour, anunciando [`SERVICE_TYPE`].
//!
//! [`SERVICE_TYPE`]: crate::protocol::SERVICE_TYPE
//!
//! Dois caminhos, e o segundo **não** é opcional:
//!
//! 1. [`Advertiser`] anuncia `_quall._tcp` e [`Browser`] navega. É o caminho normal.
//! 2. [`endereco_manual`] resolve um IP ou nome digitado pelo usuário. É o caminho para redes
//!    com mDNS bloqueado ou com *AP isolation* — sem ele uma rede corporativa vira ticket de
//!    suporte. Foi medido na bancada que o Firewall do Windows barra UDP 5353 de entrada por
//!    padrão, mesmo com o perfil de rede em `Private`: o silêncio da descoberta é o
//!    comportamento esperado, não um bug, e o fallback é o que salva.
//!
//! O anúncio público é uma dica efêmera de roteamento. Identidade e nome reais só chegam pelo
//! canal autenticado da sessão; nunca entram em TXT, nome de instância ou hostname do app.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, SocketAddrV6, ToSocketAddrs};
use std::time::{Duration, Instant};

use mdns_sd::{Receiver, RecvTimeoutError, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::error::{Error, Result};
use crate::protocol::{
    Announcement, Capabilities, DeviceId, Papel, PROTOCOL_VERSION, SERVICE_TYPE,
};

/// Porta padrão do servidor de sinalização do emissor.
///
/// É só um padrão: a porta real vai no registro TXT `p` e no `SRV`, porque duas instâncias do
/// Quall na mesma máquina (app + plugin de OBS) não podem disputar a mesma porta.
pub const DEFAULT_SIGNALING_PORT: u16 = 7877;

/// Domínio mDNS completo do serviço, como o `mdns-sd` espera.
fn service_type_domain() -> String {
    format!("{SERVICE_TYPE}.local.")
}

/// Chaves dos registros TXT. Curtas de propósito: um pacote de resposta mDNS cabe melhor em um
/// datagrama, e no Wi-Fi de 2,4 GHz do A10s fragmentar é perder.
mod txt {
    pub const VERSAO: &str = "v";
    pub const TOKEN: &str = "t";
    pub const CAPACIDADES: &str = "c";
    pub const PORTA: &str = "p";
    /// Papel funcional para seleção da rota, sem nome ou identidade de pessoa/aparelho.
    pub const PAPEL: &str = "pa";
}

/// Novo identificador público por anúncio. Falha de entropia impede anunciar.
pub fn discovery_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| Error::Discovery("sem entropia para o anúncio efêmero".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn token_valido(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// O TXT contém somente versão, token efêmero, porta e seleção funcional da rota.
pub fn announcement_to_txt(anuncio: &Announcement, porta: u16) -> Result<Vec<(String, String)>> {
    announcement_to_txt_with_token(anuncio, porta, &discovery_token()?)
}

fn announcement_to_txt_with_token(
    anuncio: &Announcement,
    porta: u16,
    token: &str,
) -> Result<Vec<(String, String)>> {
    if !anuncio.is_compatible() || porta == 0 || !token_valido(token) {
        return Err(Error::Discovery(
            "anúncio público incompatível ou inválido".into(),
        ));
    }
    let mut caps = String::with_capacity(3);
    if anuncio.capabilities.screen_source {
        caps.push('s');
    }
    if anuncio.capabilities.camera_source {
        caps.push('c');
    }
    if anuncio.capabilities.sink {
        caps.push('k');
    }
    let mut registro = vec![
        (txt::VERSAO.into(), anuncio.protocol_version.to_string()),
        (txt::TOKEN.into(), token.to_string()),
        (txt::CAPACIDADES.into(), caps),
        (txt::PORTA.into(), porta.to_string()),
    ];
    if let Some(papel) = anuncio.papel {
        registro.push((txt::PAPEL.into(), papel.como_texto().into()));
    }
    Ok(registro)
}

/// Reconstrói o anúncio e a porta de sinalização a partir dos registros TXT.
pub fn announcement_from_txt(props: &HashMap<String, String>) -> Result<(Announcement, u16)> {
    let obrigatorio = |chave: &str| -> Result<&String> {
        props
            .get(chave)
            .ok_or_else(|| Error::Discovery(format!("TXT sem a chave '{chave}'")))
    };

    let protocol_version: u16 = obrigatorio(txt::VERSAO)?
        .parse()
        .map_err(|_| Error::Discovery("TXT com versão de protocolo não numérica".into()))?;
    if protocol_version != PROTOCOL_VERSION || props.contains_key("id") || props.contains_key("n") {
        return Err(Error::Discovery(
            "anúncio legado ou com identidade pública".into(),
        ));
    }
    let token = obrigatorio(txt::TOKEN)?;
    if !token_valido(token) {
        return Err(Error::Discovery("token de descoberta inválido".into()));
    }
    let porta: u16 = obrigatorio(txt::PORTA)?
        .parse()
        .map_err(|_| Error::Discovery("TXT com porta não numérica".into()))?;
    if porta == 0 {
        return Err(Error::Discovery("TXT com porta zero".into()));
    }
    let caps = obrigatorio(txt::CAPACIDADES)?;

    Ok((
        Announcement {
            protocol_version,
            // Apenas chave da linha descoberta; NUNCA persistir como identidade de um par.
            device_id: DeviceId(format!("discovery-{token}")),
            display_name: format!("Quall {}", &token[..8]),
            capabilities: Capabilities {
                screen_source: caps.contains('s'),
                camera_source: caps.contains('c'),
                sink: caps.contains('k'),
            },
            screen: None,
            // Opcional: ausente é vídeo. Valor desconhecido vira `Desconhecido`, não erro.
            papel: props.get(txt::PAPEL).map(|p| Papel::do_texto(p)),
        },
        porta,
    ))
}

/// Um aparelho visto na rede.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// Placeholder público efêmero; a identidade real está em `session::Ready::peer`.
    pub announcement: Announcement,
    /// Endereços em que o aparelho respondeu, já ordenados do mais utilizável para o menos, e
    /// sem repetição. Ver [`ordenar_enderecos`].
    pub addresses: Vec<ScopedIp>,
    pub signaling_port: u16,
    /// Nome completo da instância mDNS, usado para casar o evento de remoção.
    pub fullname: String,
}

impl DiscoveredDevice {
    /// Melhor endereço para abrir a sinalização.
    pub fn endpoint(&self) -> Option<SocketAddr> {
        self.addresses
            .iter()
            .find_map(|ip| endpoint_com_escopo(ip, self.signaling_port))
    }
}

/// O índice de interface do mDNS pertence a **esta** máquina, não ao aparelho remoto. Não
/// transformar `ScopedIp` em `IpAddr`: em IPv6 link-local isso perderia a rota da sinalização.
fn endpoint_com_escopo(ip: &ScopedIp, porta: u16) -> Option<SocketAddr> {
    match ip {
        ScopedIp::V6(v6) => endpoint_ipv6(*v6.addr(), porta, v6.scope_id().index),
        _ => Some(SocketAddr::new(ip.to_ip_addr(), porta)),
    }
}

fn endpoint_ipv6(ip: std::net::Ipv6Addr, porta: u16, indice: u32) -> Option<SocketAddr> {
    let escopo = if ip.is_unicast_link_local() {
        if indice == 0 {
            return None;
        }
        indice
    } else {
        0 // ULA/global não carrega o índice local recebido na resposta mDNS.
    };
    Some(SocketAddr::V6(SocketAddrV6::new(ip, porta, 0, escopo)))
}

fn ordenar_enderecos_com_escopo(mut enderecos: Vec<ScopedIp>) -> Vec<ScopedIp> {
    let peso_com_escopo = |ip: &ScopedIp| match ip {
        ScopedIp::V6(v6) if v6.addr().is_unicast_link_local() && v6.scope_id().index != 0 => 30,
        _ => peso(&ip.to_ip_addr()),
    };
    enderecos.sort_by(|a, b| {
        peso_com_escopo(a)
            .cmp(&peso_com_escopo(b))
            .then_with(|| a.to_ip_addr().cmp(&b.to_ip_addr()))
            .then_with(|| a.to_string().cmp(&b.to_string()))
    });
    enderecos.dedup_by(|a, b| {
        a.to_ip_addr() == b.to_ip_addr() && endpoint_com_escopo(a, 0) == endpoint_com_escopo(b, 0)
    });
    enderecos
}

/// Quão útil é um endereço para alcançar o aparelho. Menor é melhor.
///
/// Existe porque um MacBook com Wi-Fi, cabo, Thunderbolt-bridge, `awdl0` e umas interfaces de
/// virtualização anuncia **dezenas** de endereços, e o mDNS devolve todos, repetidos.
///
/// Medido na bancada em 2026-08-21: navegando `_quall._tcp` do próprio MacBook, o emissor
/// apareceu com 70 endereços, e a primeira versão desta função — "o primeiro IPv4 que aparecer"
/// — escolheu `169.254.232.1`. É APIPA: o endereço que uma interface se dá quando **não**
/// conseguiu DHCP. Conectar nele nunca ia funcionar, e o sintoma seria "o aparelho aparece na
/// lista mas não conecta" — o pior tipo de bug de rede, porque parece problema do usuário.
fn peso(ip: &IpAddr) -> u8 {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                // Só serve para duas instâncias na mesma máquina; é válido, mas é o último
                // recurso.
                40
            } else if v4.is_link_local() {
                // 169.254/16: APIPA, quer dizer que o DHCP falhou naquela interface.
                50
            } else if v4.is_private() {
                // A LAN doméstica e a corporativa vivem aqui. É o caso normal do Quall.
                0
            } else {
                10
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                41
            } else if v6.segments()[0] & 0xffc0 == 0xfe80 {
                // Link-local IPv6 precisa de índice de interface (`%en0`) para ser usável, e
                // nem toda casca sabe carregar esse escopo. Evitar é mais barato que consertar.
                51
            } else {
                // ULA (fc00::/7) e global. Funcionam, mas IPv4 privado é o caminho batido.
                20
            }
        }
    }
}

/// Tira repetições e ordena do endereço mais utilizável para o menos.
pub fn ordenar_enderecos(mut enderecos: Vec<IpAddr>) -> Vec<IpAddr> {
    enderecos.sort_by(|a, b| peso(a).cmp(&peso(b)).then_with(|| a.cmp(b)));
    enderecos.dedup();
    enderecos
}

/// O que a navegação encontrou.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    Found(Box<DiscoveredDevice>),
    /// Instância que sumiu da rede, pelo nome completo mDNS.
    Lost(String),
}

/// Anuncia este aparelho na LAN enquanto existir.
///
/// O `Drop` desregistra o serviço e derruba o daemon. Não confie só nisso: um processo morto por
/// `panic = "abort"` não roda `Drop`, e o anúncio só some quando o TTL expira. Prefira
/// [`Advertiser::stop`], que espera a confirmação.
///
/// # Dívida 3: este comentário já foi mentira
///
/// Ele dizia exatamente isto — "o `Drop` desregistra o serviço" — e **não havia `impl Drop`**.
/// O `mdns_sd::ServiceDaemon` também não tem: soltá-lo larga um `Sender` e nada mais, e a
/// thread do daemon fica viva. Como `quall_advertiser_stop` só soltava a caixa, cada sessão
/// deixava para trás uma thread de daemon mDNS **e** um anúncio fantasma na lista dos outros
/// aparelhos. No desktop o processo morre e limpa; no Android o processo sobrevive a dezenas de
/// sessões.
///
/// Quatro cascas leem este arquivo, e é por isso que o comentário errado era caro por si só.
pub struct Advertiser {
    /// `Option` para o `Drop` poder consumir o daemon sem consumir o `Advertiser`.
    daemon: Option<ServiceDaemon>,
    fullname: String,
    discovery_label: String,
}

impl Advertiser {
    /// Começa a anunciar. `porta` é a porta TCP em que o servidor de sinalização já está ouvindo.
    pub fn start(anuncio: &Announcement, porta: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| Error::Discovery(format!("não subiu o daemon mDNS: {e}")))?;

        let token = discovery_token()?;
        let instancia = nome_da_instancia(&token)?;
        let host = format!("quall-{token}.local.");
        let props = announcement_to_txt_with_token(anuncio, porta, &token)?;
        let info = ServiceInfo::new(
            &service_type_domain(),
            &instancia,
            &host,
            (),
            porta,
            &props[..],
        )
        .map_err(|e| Error::Discovery(format!("anúncio inválido: {e}")))?
        // Deixa o daemon descobrir e manter os endereços das interfaces. Escrever a lista à mão
        // erra sempre que o Wi-Fi cai, o cabo entra, ou o DHCP muda o IP no meio da sessão.
        .enable_addr_auto();

        let fullname = info.get_fullname().to_string();
        daemon
            .register(info)
            .map_err(|e| Error::Discovery(format!("não registrou o serviço: {e}")))?;

        Ok(Advertiser {
            daemon: Some(daemon),
            fullname,
            discovery_label: format!("Quall {}", &token[..8]),
        })
    }

    /// Nome completo efêmero da instância anunciada (`Quall <token>._quall._tcp.local.`).
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// Rótulo público efêmero exibido pelos navegadores; nunca é a identidade persistida.
    pub fn discovery_label(&self) -> &str {
        &self.discovery_label
    }

    /// Desregistra e espera a confirmação, para que a lista dos outros aparelhos não fique com
    /// um fantasma até o TTL expirar.
    ///
    /// Chamar duas vezes não é erro; a segunda não faz nada.
    pub fn stop(mut self) -> Result<()> {
        self.parar()
    }

    /// O corpo de [`Advertiser::stop`], que o `Drop` também usa. Idempotente.
    fn parar(&mut self) -> Result<()> {
        let Some(daemon) = self.daemon.take() else {
            return Ok(());
        };
        let recibo = daemon
            .unregister(&self.fullname)
            .map_err(|e| Error::Discovery(format!("não desregistrou: {e}")))?;
        // Um segundo é folga suficiente na LAN; se o daemon já morreu, seguir em frente é o
        // comportamento certo — não há nada a fazer sobre isso na saída do processo.
        let _ = recibo.recv_timeout(Duration::from_secs(1));
        let _ = daemon.shutdown();
        Ok(())
    }

    /// O daemon ainda está de pé? Só para os testes: é o que distingue "parou" de "esqueceu".
    #[cfg(test)]
    fn ativo(&self) -> bool {
        self.daemon.is_some()
    }
}

/// **Dívida 3.** Sem isto, soltar o `Advertiser` deixava a thread do daemon mDNS viva e o
/// anúncio no ar até o TTL expirar. O `mdns_sd::ServiceDaemon` não tem `Drop` próprio — soltá-lo
/// larga um `Sender` e nada mais.
impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.parar();
    }
}

/// Navega a LAN procurando `_quall._tcp`.
pub struct Browser {
    daemon: ServiceDaemon,
    eventos: Receiver<ServiceEvent>,
}

impl Browser {
    pub fn start() -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| Error::Discovery(format!("não subiu o daemon mDNS: {e}")))?;
        let eventos = daemon
            .browse(&service_type_domain())
            .map_err(|e| Error::Discovery(format!("não iniciou a navegação: {e}")))?;
        Ok(Browser { daemon, eventos })
    }

    /// Espera o próximo evento útil por até `limite`.
    ///
    /// Devolve `Ok(None)` quando o prazo passou sem nada — que é o caso normal numa rede parada,
    /// não um erro. Anúncios de versão incompatível são descartados aqui: quem chama não deveria
    /// precisar saber que existem.
    pub fn next_event(&self, limite: Duration) -> Result<Option<DiscoveryEvent>> {
        let prazo = Instant::now() + limite;
        loop {
            let restante = prazo.saturating_duration_since(Instant::now());
            if restante.is_zero() {
                return Ok(None);
            }
            match self.eventos.recv_timeout(restante) {
                Ok(ServiceEvent::ServiceResolved(servico)) => {
                    let props = servico.txt_properties.clone().into_property_map_str();
                    let Ok((anuncio, porta)) = announcement_from_txt(&props) else {
                        continue; // TXT de outra coisa, ou truncado: ignora e segue navegando.
                    };
                    if !anuncio.is_compatible() {
                        continue;
                    }
                    let addresses =
                        ordenar_enderecos_com_escopo(servico.addresses.iter().cloned().collect());
                    return Ok(Some(DiscoveryEvent::Found(Box::new(DiscoveredDevice {
                        announcement: anuncio,
                        addresses,
                        signaling_port: porta,
                        fullname: servico.fullname.clone(),
                    }))));
                }
                Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                    return Ok(Some(DiscoveryEvent::Lost(fullname)));
                }
                // SearchStarted/ServiceFound/SearchStopped não interessam a quem chama.
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Discovery("o daemon mDNS encerrou".into()))
                }
            }
        }
    }

    /// Junta tudo que aparecer durante `duracao`, deduplicando por `DeviceId`.
    ///
    /// Conveniência para a sonda e para a primeira tela do app; um app de verdade prefere
    /// [`Browser::next_event`], que atualiza a lista à medida que os aparelhos entram e saem.
    pub fn collect(&self, duracao: Duration) -> Result<Vec<DiscoveredDevice>> {
        let prazo = Instant::now() + duracao;
        let mut achados: Vec<DiscoveredDevice> = Vec::new();
        loop {
            let restante = prazo.saturating_duration_since(Instant::now());
            if restante.is_zero() {
                break;
            }
            match self.next_event(restante)? {
                Some(DiscoveryEvent::Found(aparelho)) => {
                    match achados
                        .iter_mut()
                        .find(|a| a.announcement.device_id == aparelho.announcement.device_id)
                    {
                        Some(existente) => *existente = *aparelho,
                        None => achados.push(*aparelho),
                    }
                }
                Some(DiscoveryEvent::Lost(fullname)) => {
                    achados.retain(|a| a.fullname != fullname);
                }
                None => break,
            }
        }
        Ok(achados)
    }

    pub fn stop(self) {
        let _ = self.daemon.stop_browse(&service_type_domain());
        let _ = self.daemon.shutdown();
    }
}

/// Resolve o que o usuário digitou no campo de fallback.
///
/// Aceita `192.168.56.41`, `192.168.56.41:7877`, `[fe80::1]:7877` e nomes (`computador-exemplo.local`).
/// Sem porta, usa [`DEFAULT_SIGNALING_PORT`].
///
/// Este é o caminho do M6 que foi antecipado para o M1 de propósito: a rede que bloqueia mDNS
/// não avisa, e descobrir isso só na hora da demonstração é caro.
pub fn endereco_manual(entrada: &str) -> Result<SocketAddr> {
    endereco_manual_com_prazo(entrada, PRAZO_DE_RESOLUCAO)
}

/// Prazo padrão da resolução de nome.
///
/// Cinco segundos é generoso para um `.local` na LAN e curto o bastante para não parecer
/// travamento. Quem tem um prazo próprio — a sessão inteira, por exemplo — passa o seu por
/// [`endereco_manual_com_prazo`].
pub const PRAZO_DE_RESOLUCAO: Duration = Duration::from_secs(5);

/// Igual a [`endereco_manual`], com prazo para a resolução de nome.
///
/// # Por que a resolução precisa de prazo
///
/// `to_socket_addrs` chama o `getaddrinfo` do sistema, que **bloqueia sem limite nosso**: um
/// servidor DNS que não responde segura a chamada por dezenas de segundos, e no macOS um nome
/// `.local` passa pelo resolvedor mDNS, que tem a sua própria paciência. Como o `quall_connect`
/// resolve o endereço **antes** de montar a sessão, o `timeout_ms` que a casca passou não
/// cobria nada disso — a tela de "conectando" ficava presa fora de qualquer prazo.
///
/// Não morde quem digita um IP: `"192.168.56.131"` e `"192.168.56.131:7877"` saem pelos atalhos
/// acima sem tocar no resolvedor. Morde o **erro de digitação** (`192.168.56.13x` vira nome) e o
/// nome de host — que é justamente quem o usuário digita quando o mDNS não funciona.
///
/// # A thread que fica para trás
///
/// Não existe `getaddrinfo` cancelável portátil. Quando o prazo estoura, a thread de resolução
/// continua viva até o sistema desistir, e então morre sozinha sem tocar em nada — ela só
/// escreve num canal cujo receptor já foi embora. É o preço de ter prazo, e é menor que o de não
/// ter: uma thread parada contra uma tela travada.
pub fn endereco_manual_com_prazo(entrada: &str, limite: Duration) -> Result<SocketAddr> {
    endereco_manual_com_porta(entrada, DEFAULT_SIGNALING_PORT, limite)
}

// =============================================================================================
// A porta do teleprompter e o link `quall://` (`docs/contrato-teleprompter.md` §11.1)
// =============================================================================================

/// **A porta do teleprompter**: o prompter hospeda nela, e o controle completa com ela o endereço
/// digitado sem porta. A 7877 ([`DEFAULT_SIGNALING_PORT`]) é a do espelhamento: um aparelho pode
/// espelhar a tela e mostrar o roteiro sem as duas disputarem a porta.
pub const PORTA_DO_TELEPROMPTER: u16 = 7979;

/// Quantas portas, contando a 7979, o prompter tenta antes de pedir uma efêmera: 7979 a 7988.
pub const PORTAS_DO_TELEPROMPTER: u16 = 10;

/// **Quanto o prompter espera pela 7979** antes de passar à 7980. Numa recriação da tela, a sessão
/// velha ainda segura a porta por uma bombeada (~100–300 ms); sem a espera, a tela nova iria para a
/// 7980 e o controle que caiu não a acharia mais. É a `TOLERANCIA_DA_PORTA_MS` que o Android já
/// tinha (`SessaoDoPrompter.kt`).
pub const ESPERA_PELA_PORTA_DO_TELEPROMPTER: Duration = Duration::from_secs(2);

/// A porta que completa um endereço digitado sem porta, para quem conecta com este papel: 7979
/// para o controle remoto; 7877 para todo o resto, inclusive o vídeo (`None`).
pub fn porta_para_completar(papel: Option<Papel>) -> u16 {
    match papel {
        Some(Papel::ControleRemoto) => PORTA_DO_TELEPROMPTER,
        _ => DEFAULT_SIGNALING_PORT,
    }
}

/// O que se leu de um endereço digitado, colado ou lido de um QR. Ver [`ler_destino`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinoLido {
    /// `host:porta`, com IPv6 entre colchetes. Ainda **não resolvido**: um nome continua nome.
    pub endereco: String,
    /// Os seis dígitos do PIN, quando a entrada era um link `quall://<pin>@<host>:<porta>`.
    pub pin: Option<String>,
}

/// O prefixo do link de pareamento, o formato que o iOS fixou (`docs/app-macos.md`).
const PREFIXO_DO_LINK: &str = "quall://";

fn e_link(entrada: &str) -> bool {
    entrada
        .get(..PREFIXO_DO_LINK.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(PREFIXO_DO_LINK))
}

/// **Lê o que a pessoa digitou, colou, ou o QR trouxe** — um endereço ou o link
/// `quall://<pin>@<host>[:<porta>]` —, sem DNS e sem rede nenhuma. Sem porta, `porta`
/// (ver [`porta_para_completar`]).
///
/// É a união do que as três cascas aceitavam, ficando com a mais segura onde elas divergiam
/// (`docs/contrato-teleprompter.md` §11.1): link com PIN que não é **exatamente** seis dígitos
/// ASCII, ou sem `@`, é recusado — um QR quebrado não pode virar uma tentativa de PIN errado, que
/// faz o prompter trocar o PIN dele. Espaço no meio, `/ ? #` fora de um link, porta 0 ou acima de
/// 65535 e IPv6 com zona também são [`Error::Invalid`], com o motivo.
///
/// **O link não vai para o `connect`**: quem lê um link preenche o endereço e o PIN nos campos da
/// tela, e conecta com o endereço ([`endereco_manual_com_porta`] recusa link). A volta automática
/// depois de uma queda vai com o endereço e sem PIN — com o PIN velho do link, ela viraria
/// pareamento novo e `WRONG_PIN` (§11.1, achado C1).
pub fn ler_destino(entrada: &str, porta: u16) -> Result<DestinoLido> {
    let entrada = entrada.trim();
    if entrada.is_empty() {
        return Err(Error::Invalid("endereço vazio".into()));
    }
    if !e_link(entrada) {
        return Ok(DestinoLido {
            endereco: normalizar_endereco(entrada, porta)?,
            pin: None,
        });
    }
    let mut resto = &entrada[PREFIXO_DO_LINK.len()..];
    // Um leitor de QR genérico acrescenta `/`, `?…` ou `#…`: tudo depois do endereço sai.
    if let Some(corte) = resto.find(['/', '?', '#']) {
        resto = &resto[..corte];
    }
    let Some(arroba) = resto.rfind('@') else {
        return Err(Error::Invalid(
            "o link não traz o PIN: o formato é quall://<pin>@<host>:<porta>".into(),
        ));
    };
    let (pin, host) = (&resto[..arroba], &resto[arroba + 1..]);
    if pin.len() != 6 || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid(
            "o PIN do link tem de ter exatamente seis dígitos: um PIN quebrado viraria uma \
             tentativa errada, e o prompter trocaria o PIN dele"
                .into(),
        ));
    }
    if host.is_empty() {
        return Err(Error::Invalid(
            "o link não traz o endereço depois do @".into(),
        ));
    }
    Ok(DestinoLido {
        endereco: normalizar_endereco(host, porta)?,
        pin: Some(pin.to_string()),
    })
}

/// Uma porta digitada: só dígitos, de 1 a 65535.
fn ler_porta(texto: &str) -> Result<u16> {
    let porta = if !texto.is_empty() && texto.bytes().all(|b| b.is_ascii_digit()) {
        texto
            .parse::<u32>()
            .ok()
            .filter(|p| (1..=u32::from(u16::MAX)).contains(p))
    } else {
        None
    };
    porta
        .map(|p| p as u16)
        .ok_or_else(|| Error::Invalid(format!("porta '{texto}' fora de 1..65535")))
}

/// `host`, `host:porta`, `v6`, `[v6]` ou `[v6]:porta` → `host:porta`, sem resolver nada.
fn normalizar_endereco(host: &str, porta: u16) -> Result<String> {
    if host.chars().any(char::is_whitespace) {
        return Err(Error::Invalid(format!(
            "'{host}': espaço no meio do endereço"
        )));
    }
    if host.contains(['/', '?', '#']) {
        return Err(Error::Invalid(format!(
            "'{host}': '/', '?' e '#' só valem num link quall://"
        )));
    }
    if let Ok(addr) = host.parse::<SocketAddr>() {
        if addr.port() == 0 {
            return Err(Error::Invalid(format!("'{host}': porta 0")));
        }
        return Ok(addr.to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, porta).to_string());
    }
    if let Some(dentro) = host.strip_prefix('[') {
        let Some(fecha) = dentro.find(']') else {
            return Err(Error::Invalid(format!("'{host}': colchete sem fechar")));
        };
        let literal = &dentro[..fecha];
        let (ip, zona) = ler_ipv6_com_zona(literal)?;
        let depois = &dentro[fecha + 1..];
        let porta = match depois.strip_prefix(':') {
            _ if depois.is_empty() => porta,
            Some(p) => ler_porta(p)?,
            None => return Err(Error::Invalid(format!("'{host}': o que vem depois do ]"))),
        };
        return Ok(match zona {
            Some(zona) => format!("[{ip}%{zona}]:{porta}"),
            None => SocketAddr::new(IpAddr::V6(ip), porta).to_string(),
        });
    }
    if host.contains('%') {
        let (ip, zona) = ler_ipv6_com_zona(host)?;
        return Ok(format!(
            "[{ip}%{}]:{porta}",
            zona.expect("a entrada tem zona")
        ));
    }
    // Sobrou nome, com ou sem `:porta`. Um nome com dois-pontos a mais é um IPv6 que o sistema não
    // aceitou.
    match host.rfind(':') {
        Some(i) => {
            let (nome, p) = (&host[..i], &host[i + 1..]);
            if nome.is_empty() || nome.contains([':', '[', ']']) {
                return Err(Error::Invalid(format!("'{host}' não é um endereço")));
            }
            Ok(format!("{nome}:{}", ler_porta(p)?))
        }
        None if host.contains(['[', ']']) => {
            Err(Error::Invalid(format!("'{host}' não é um endereço")))
        }
        None => Ok(format!("{host}:{porta}")),
    }
}

/// Zona numérica sai pelo parser de `SocketAddr`; nome de interface passa pelo resolvedor do
/// sistema, com o mesmo prazo dos nomes de host. Nunca adivinhamos o índice de outra máquina.
fn ler_ipv6_com_zona(literal: &str) -> Result<(std::net::Ipv6Addr, Option<&str>)> {
    let (ip, zona) = match literal.split_once('%') {
        Some((ip, zona))
            if !zona.is_empty()
                && zona
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)) =>
        {
            (ip, Some(zona))
        }
        Some(_) => return Err(Error::Invalid(format!("'{literal}': zona IPv6 inválida"))),
        None => (literal, None),
    };
    let ip = ip
        .parse()
        .map_err(|_| Error::Invalid(format!("'{literal}': IPv6 que o sistema não aceita")))?;
    Ok((ip, zona))
}

/// **O endereço para conectar**: um endereço digitado (nunca um link), completado com `porta`
/// quando falta, e resolvido com prazo. É o que todo `quall_connect*` faz, com a porta do papel
/// ([`porta_para_completar`]).
///
/// Um link é [`Error::Invalid`]: leia-o com [`ler_destino`], ponha o PIN nas opções e conecte no
/// endereço dele (§11.1, achado C1). Um IP não passa pelo resolvedor; um nome, sim, com `limite`.
pub fn endereco_manual_com_porta(
    entrada: &str,
    porta: u16,
    limite: Duration,
) -> Result<SocketAddr> {
    let entrada = entrada.trim();
    if e_link(entrada) {
        return Err(Error::Invalid(
            "é um link quall://: leia-o com quall_parse_endpoint_json (ler_destino) e conecte no \
             endereço dele, com o PIN nas opções"
                .into(),
        ));
    }
    let com_porta = ler_destino(entrada, porta)?.endereco;
    if let Ok(addr) = com_porta.parse::<SocketAddr>() {
        return Ok(addr);
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let (host, porta) = com_porta
        .rsplit_once(':')
        .expect("endereço normalizado traz porta");
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_string();
    let porta = porta.parse::<u16>().expect("porta normalizada é válida");
    std::thread::spawn(move || {
        let r = (host.as_str(), porta).to_socket_addrs().map(|iter| {
            // Mesma preferência por IPv4 do `DiscoveredDevice::endpoint`, pelo mesmo motivo.
            iter.fold(None, |melhor: Option<SocketAddr>, addr| match melhor {
                Some(m) if m.is_ipv4() => Some(m),
                _ => Some(addr),
            })
        });
        // O receptor já pode ter desistido; mandar para o vazio é o fim normal desta thread.
        let _ = tx.send(r.map_err(|e| e.to_string()));
    });

    match rx.recv_timeout(limite) {
        Ok(Ok(Some(addr))) => Ok(addr),
        Ok(Ok(None)) => Err(Error::Invalid(format!(
            "'{entrada}' não resolveu para nenhum endereço"
        ))),
        Ok(Err(e)) => Err(Error::Invalid(format!("não resolveu '{entrada}': {e}"))),
        Err(_) => Err(Error::Timeout(format!(
            "'{entrada}' não resolveu em {} ms",
            limite.as_millis()
        ))),
    }
}

/// **A porta em que o prompter vai hospedar**, escolhida **uma vez, ao abrir a tela**: a 7979,
/// esperando por ela até `espera` ([`ESPERA_PELA_PORTA_DO_TELEPROMPTER`]); senão a primeira livre
/// de 7980 a 7988; senão uma efêmera que o sistema der. `None` só se nem isso.
///
/// **A volta depois de uma queda é sempre na mesma porta**: o controle que caiu tenta de novo no
/// endereço que tinha (§2). Chamar isto de novo a cada sessão mudaria a porta sob os pés dele.
///
/// A prova usa a mesma escuta dual-stack de `SignalingServer::bind`. Entre soltar
/// a porta de teste e hospedar há uma janela em que outro programa pode pegá-la; quem perde essa
/// corrida vê o `bind` do núcleo falhar e tenta de novo — o mesmo que o Mac e o Android já faziam.
pub fn escolher_porta_do_teleprompter(espera: Duration) -> Option<u16> {
    escolher_porta_a_partir_de(PORTA_DO_TELEPROMPTER, PORTAS_DO_TELEPROMPTER, espera)
}

pub(crate) fn escolher_porta_a_partir_de(base: u16, quantas: u16, espera: Duration) -> Option<u16> {
    let livre = |p: u16| crate::signaling::SignalingServer::bind(p).is_ok();
    let fim = Instant::now() + espera;
    loop {
        if livre(base) {
            return Some(base);
        }
        if Instant::now() >= fim {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for p in (1..quantas).filter_map(|d| base.checked_add(d)) {
        if livre(p) {
            return Some(p);
        }
    }
    crate::signaling::SignalingServer::bind(0)
        .and_then(|l| l.port())
        .ok()
        .filter(|p| *p != 0)
}

/// Rótulo público efêmero, sempre ASCII e menor que os 63 bytes do DNS.
pub fn nome_da_instancia(token: &str) -> Result<String> {
    if !token_valido(token) {
        return Err(Error::Discovery("token de descoberta inválido".into()));
    }
    Ok(format!("Quall {token}"))
}

/// Anúncio deste aparelho, com a versão de protocolo já preenchida.
pub fn anuncio(device_id: &str, display_name: &str, capabilities: Capabilities) -> Announcement {
    Announcement {
        protocol_version: PROTOCOL_VERSION,
        device_id: DeviceId(device_id.to_string()),
        display_name: display_name.to_string(),
        capabilities,
        screen: None,
        papel: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exemplo() -> Announcement {
        anuncio(
            "a10s-teste-abc123",
            "Galaxy A10s",
            Capabilities {
                screen_source: true,
                camera_source: false,
                sink: true,
            },
        )
    }

    #[test]
    fn txt_sobrevive_ida_e_volta() {
        let original = exemplo();
        let props: HashMap<String, String> = announcement_to_txt(&original, 7877)
            .expect("TXT")
            .into_iter()
            .collect();
        let (voltou, porta) = announcement_from_txt(&props).expect("decodifica");
        assert!(voltou.device_id.0.starts_with("discovery-"));
        assert_ne!(voltou.device_id, original.device_id);
        assert_ne!(voltou.display_name, original.display_name);
        assert_eq!(voltou.capabilities, original.capabilities);
        assert_eq!(porta, 7877);
    }

    /// A superfície pública não leva identidade nem nome de aparelho.
    #[test]
    fn txt_sem_papel_nao_tem_identidade_persistente() {
        let txt = announcement_to_txt(&exemplo(), 7877).expect("TXT");
        let chaves: Vec<&str> = txt.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(chaves, ["v", "t", "c", "p"]);
    }

    /// O papel funcional sobrevive à descoberta, sem identidade pessoal.
    #[test]
    fn txt_com_papel_preserva_selecao_funcional_sem_nome() {
        let mut a = exemplo();
        a.papel = Some(Papel::Teleprompter);
        let props: HashMap<String, String> = announcement_to_txt(&a, 7877)
            .expect("TXT")
            .into_iter()
            .collect();
        assert_eq!(props.get("pa").map(String::as_str), Some("teleprompter"));
        let (voltou, _) = announcement_from_txt(&props).expect("decodifica");
        assert_eq!(voltou.papel, Some(Papel::Teleprompter));

        // A build anterior: o mesmo leitor, sem a chave nova no mapa que ela conhece.
        let mut sem_pa = props.clone();
        sem_pa.remove("pa");
        let (antigo, _) = announcement_from_txt(&sem_pa).expect("a build anterior decodifica");
        assert_eq!(antigo.papel, None);
        assert_ne!(antigo.display_name, a.display_name);

        // Um valor que esta build não conhece não derruba o anúncio.
        let mut futuro = props;
        futuro.insert("pa".into(), "parede".into());
        let (f, _) = announcement_from_txt(&futuro).expect("decodifica");
        assert_eq!(f.papel, Some(Papel::Desconhecido));
    }

    #[test]
    fn txt_sem_chave_obrigatoria_e_erro() {
        let mut props: HashMap<String, String> = announcement_to_txt(&exemplo(), 7877)
            .expect("TXT")
            .into_iter()
            .collect();
        props.remove("t");
        assert!(announcement_from_txt(&props).is_err());
    }

    #[test]
    fn capacidades_ausentes_nao_viram_true() {
        let mut props: HashMap<String, String> = announcement_to_txt(&exemplo(), 7877)
            .expect("TXT")
            .into_iter()
            .collect();
        props.insert("c".into(), String::new());
        let (voltou, _) = announcement_from_txt(&props).expect("decodifica");
        assert_eq!(
            voltou.capabilities,
            Capabilities {
                screen_source: false,
                camera_source: false,
                sink: false
            }
        );
    }

    #[test]
    fn endereco_manual_aceita_ip_puro() {
        let addr = endereco_manual("192.168.56.41").expect("resolve");
        assert_eq!(
            addr.to_string(),
            format!("192.168.56.41:{DEFAULT_SIGNALING_PORT}")
        );
    }

    #[test]
    fn endereco_manual_aceita_ip_com_porta() {
        let addr = endereco_manual("192.168.56.41:9000").expect("resolve");
        assert_eq!(addr.port(), 9000);
    }

    #[test]
    fn endereco_manual_aceita_ipv6_com_colchetes() {
        let addr = endereco_manual("[::1]:9000").expect("resolve");
        assert!(addr.is_ipv6());
        assert_eq!(addr.port(), 9000);
    }

    #[test]
    fn ipv6_com_zona_preserva_indice_e_porta() {
        for (entrada, esperado) in [
            ("fe80::1%7", "[fe80::1%7]:7877"),
            ("[fe80::1%7]", "[fe80::1%7]:7877"),
            ("[fe80::1%7]:9000", "[fe80::1%7]:9000"),
        ] {
            assert_eq!(endereco_manual(entrada).unwrap().to_string(), esperado);
        }
        assert_eq!(
            ler_destino("fe80::1%en0", 7979).unwrap().endereco,
            "[fe80::1%en0]:7979"
        );
        assert_eq!(
            ler_destino("[fe80::1%en0]:7980", 7979).unwrap().endereco,
            "[fe80::1%en0]:7980"
        );
        assert_eq!(
            ler_destino("quall://424242@[fe80::1%7]:7980", 7979)
                .unwrap()
                .pin
                .as_deref(),
            Some("424242")
        );
        for entrada in [
            "fe80::1%",
            "fe80::1%en0%en1",
            "[fe80::1%en0]:0",
            "[fe80::1%en0]:70000",
            "192.0.2.1%7",
        ] {
            assert!(ler_destino(entrada, 7979).is_err(), "{entrada}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn zona_por_nome_usa_resolvedor_do_sistema_com_prazo() {
        let addr = endereco_manual_com_prazo("[fe80::1%lo0]:9000", Duration::from_secs(2)).unwrap();
        assert!(matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0 && v6.port() == 9000));
    }

    #[test]
    fn endpoint_ipv6_link_local_exige_e_preserva_interface() {
        let ip = "fe80::1234".parse().unwrap();
        assert_eq!(endpoint_ipv6(ip, 7877, 0), None);
        assert_eq!(
            endpoint_ipv6(ip, 7877, 9).unwrap().to_string(),
            "[fe80::1234%9]:7877"
        );
        assert_ne!(endpoint_ipv6(ip, 7877, 9), endpoint_ipv6(ip, 7877, 10));
        let ula = "fd00::1234".parse().unwrap();
        assert_eq!(
            endpoint_ipv6(ula, 7877, 9).unwrap().to_string(),
            "[fd00::1234]:7877"
        );
        let a = aparelho(vec!["fe80::1234"]);
        assert!(
            a.endpoint().is_none(),
            "não inventar uma rota para endereço sem escopo"
        );
        assert_eq!(
            aparelho(vec!["fd00::1234"]).endpoint().unwrap().to_string(),
            "[fd00::1234]:7877"
        );
    }

    #[test]
    fn endereco_manual_recusa_vazio() {
        assert!(endereco_manual("   ").is_err());
    }

    /// **Achado da auditoria.** A resolução de nome tem de caber num prazo.
    ///
    /// O `quall_connect` resolve **antes** de montar a sessão, então o `timeout_ms` da casca não
    /// cobria o `getaddrinfo`. Um servidor DNS que não responde segurava a tela de "conectando"
    /// fora de qualquer prazo.
    ///
    /// O nome usado aqui é do domínio reservado pela RFC 6761 para "não existe", e cai no
    /// caminho de resolução por ter letras.
    ///
    /// **O que este teste prova, e o que não prova.** Ele prova que o prazo está ligado ao
    /// caminho da resolução e que a chamada volta dentro dele. Ele **não** reproduz um resolvedor
    /// pendurado: não há como forçar o `getaddrinfo` do sistema a travar de dentro de um teste.
    /// Nesta máquina o resolvedor responde NXDOMAIN em milissegundos, então o caminho exercitado
    /// é o rápido — o prazo é a rede de segurança para a rede que não responde, e essa não foi
    /// reproduzida.
    #[test]
    fn resolver_nome_respeita_o_prazo() {
        let inicio = Instant::now();
        let r = endereco_manual_com_prazo("nao-existe.invalid", Duration::from_millis(300));
        let levou = inicio.elapsed();
        assert!(r.is_err(), "'nao-existe.invalid' não devia resolver");
        assert!(
            levou < Duration::from_secs(3),
            "a resolução levou {levou:?} — o prazo pedido era de 300 ms"
        );

        // Com prazo de um nanossegundo, o único desfecho possível é o do prazo — a menos que o
        // resolvedor ganhe a corrida, e aí o desfecho é `Invalid`, que também é voltar.
        let r = endereco_manual_com_prazo("nao-existe.invalid", Duration::from_nanos(1));
        assert!(
            matches!(r, Err(Error::Timeout(_)) | Err(Error::Invalid(_))),
            "veio {r:?}"
        );
    }

    /// **A porta do teleprompter no núcleo** (`docs/contrato-teleprompter.md` §11.1): o controle
    /// completa com 7979; todo o resto, com a 7877 de sempre.
    #[test]
    fn o_controle_completa_com_a_porta_do_teleprompter() {
        let controle = porta_para_completar(Some(Papel::ControleRemoto));
        let video = porta_para_completar(None);
        assert_eq!((controle, video), (7979, 7877));
        assert_eq!(porta_para_completar(Some(Papel::Teleprompter)), 7877);
        let e =
            endereco_manual_com_porta("192.168.57.8", controle, Duration::from_millis(1)).unwrap();
        assert_eq!(e.to_string(), "192.168.57.8:7979");
        let e = endereco_manual_com_porta("192.168.57.8:8000", controle, Duration::from_millis(1))
            .unwrap();
        assert_eq!(e.port(), 8000, "a porta digitada é mexida");
        assert_eq!(
            endereco_manual("192.168.57.8").unwrap().port(),
            7877,
            "o vídeo mudou de porta"
        );
    }

    /// A tabela de `ler_destino` (§11.1): a união do que Android, iOS e Mac aceitavam, ficando com
    /// a mais segura onde divergiam.
    #[test]
    fn o_link_traz_o_pin_e_a_porta() {
        let ler = |e: &str| ler_destino(e, PORTA_DO_TELEPROMPTER).map(|d| (d.endereco, d.pin));
        let ok = |e: &str, a: &str, pin: Option<&str>| {
            assert_eq!(
                ler(e).ok(),
                Some((a.to_string(), pin.map(str::to_string))),
                "entrada {e:?}"
            );
        };
        ok("192.168.57.8", "192.168.57.8:7979", None);
        ok("  192.168.57.8  ", "192.168.57.8:7979", None);
        ok("quall-944d0e.local", "quall-944d0e.local:7979", None);
        ok("192.168.57.8:8000", "192.168.57.8:8000", None);
        ok("192.168.57.8:7877", "192.168.57.8:7877", None);
        ok("[fe80::1]:8000", "[fe80::1]:8000", None);
        ok("[fe80::1]", "[fe80::1]:7979", None);
        ok(
            "2804:1b1:fec0:1458::1",
            "[2804:1b1:fec0:1458::1]:7979",
            None,
        );
        ok(
            "quall://424242@192.168.57.8:7979",
            "192.168.57.8:7979",
            Some("424242"),
        );
        ok(
            "QUALL://424242@192.168.57.8/",
            "192.168.57.8:7979",
            Some("424242"),
        );
        ok(
            "  QUALL://424242@192.168.57.20:7979/  ",
            "192.168.57.20:7979",
            Some("424242"),
        );
        ok(
            "quall://424242@192.168.57.8:7979?x=1",
            "192.168.57.8:7979",
            Some("424242"),
        );
        ok(
            "quall://424242@[fe80::1]:7980#frag",
            "[fe80::1]:7980",
            Some("424242"),
        );
        ok(
            "quall://123456@ipad.local",
            "ipad.local:7979",
            Some("123456"),
        );
        for ruim in [
            "",
            "   ",
            "192.168.57.8:0",
            "192.168.57.8:70000",
            "192.168.57.8:abc",
            "192.168.57.8:+80",
            ":7979",
            "quall://424242@",
            "quall://42424@192.168.57.8:7979",
            "quall://4242424@192.168.57.8:7979",
            "quall://abcdef@192.168.57.8:7979",
            "quall://4２4242@192.168.57.8:7979",
            "quall://192.168.57.8:7979",
            "[fe80::1",
            "[fe80::1]x",
            "192.168.57.8 : 7979",
            "192.168.57.8:7979/",
            "ipad.local?x",
            "2804:1b1::zz",
        ] {
            assert!(
                matches!(ler(ruim), Err(Error::Invalid(_))),
                "{ruim:?} devia ser INVALID, veio {:?}",
                ler(ruim)
            );
        }
    }

    /// **O vídeo lê o endereço como antes.** Uma cópia literal do algoritmo de `b7e11f5`, e a
    /// comparação numa tabela de entradas sem link e com porta válida: o mesmo endereço. As únicas
    /// mudanças para o vídeo são as da §11.1 — porta 0 e link viram `INVALID` antes do `connect`.
    #[test]
    fn o_video_le_o_endereco_como_antes() {
        fn de_13_09(entrada: &str) -> Option<String> {
            let entrada = entrada.trim();
            if entrada.is_empty() {
                return None;
            }
            if let Ok(addr) = entrada.parse::<SocketAddr>() {
                return Some(addr.to_string());
            }
            if let Ok(ip) = entrada.parse::<IpAddr>() {
                return Some(SocketAddr::new(ip, DEFAULT_SIGNALING_PORT).to_string());
            }
            Some(match entrada.rfind(':') {
                Some(i) if entrada[i + 1..].parse::<u16>().is_ok() => entrada.to_string(),
                _ => format!("{entrada}:{DEFAULT_SIGNALING_PORT}"),
            })
        }
        for e in [
            "192.168.56.41",
            "192.168.56.41:9000",
            " 192.168.56.131 ",
            "169.254.75.173:7877",
            "[::1]:9000",
            "::1",
            "fe80::1",
            "[fe80::1]",
            "2804:1b1:fec0:1458::1",
            "computador-exemplo.local",
            "computador-exemplo.local:9000",
            "localhost",
        ] {
            assert_eq!(
                ler_destino(e, DEFAULT_SIGNALING_PORT)
                    .ok()
                    .map(|d| d.endereco),
                de_13_09(e),
                "{e:?} mudou para o vídeo"
            );
            if let Some(Ok(antes)) = de_13_09(e).map(|s| s.parse::<SocketAddr>()) {
                assert_eq!(endereco_manual(e).ok(), Some(antes), "{e:?}");
            }
        }
        assert_eq!(
            de_13_09("192.168.56.41:0").as_deref(),
            Some("192.168.56.41:0")
        );
        assert!(
            matches!(endereco_manual("192.168.56.41:0"), Err(Error::Invalid(_))),
            "a porta 0 passou"
        );
    }

    /// **Achado C1 (alta)**: o `connect` recebe só endereço. Um link é `INVALID` — leia-o com
    /// `ler_destino`, ponha o PIN nas opções e conecte no endereço. Com o link no `connect`, a volta
    /// automática tiraria de novo o PIN velho dele e viraria `WRONG_PIN`.
    #[test]
    fn link_no_connect_e_invalid() {
        for e in [
            "quall://424242@192.168.57.8:7979",
            "QUALL://424242@127.0.0.1",
            "quall://127.0.0.1:7979",
        ] {
            for porta in [DEFAULT_SIGNALING_PORT, PORTA_DO_TELEPROMPTER] {
                assert!(
                    matches!(
                        endereco_manual_com_porta(e, porta, Duration::from_millis(1)),
                        Err(Error::Invalid(_))
                    ),
                    "{e:?} entrou no connect"
                );
            }
        }
    }

    /// **Achado C3**: a porta do prompter espera pela preferida antes de passar à seguinte — numa
    /// recriação da tela, a sessão velha ainda a segura por um instante. Numa faixa efêmera, para
    /// não disputar a 7979 desta máquina.
    #[test]
    fn a_porta_do_prompter_espera_pela_preferida_e_depois_pula() {
        let base = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let preferida = base.local_addr().unwrap().port();
        // Ocupada o tempo todo: espera o prazo, e passa a outra.
        let inicio = Instant::now();
        let outra = escolher_porta_a_partir_de(preferida, 3, Duration::from_millis(300)).unwrap();
        assert_ne!(outra, preferida);
        assert!(
            inicio.elapsed() >= Duration::from_millis(300),
            "não esperou pela preferida"
        );
        // Solta no meio da espera: fica com a preferida.
        let solta = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(base);
        });
        let volta = escolher_porta_a_partir_de(preferida, 3, Duration::from_secs(3)).unwrap();
        solta.join().unwrap();
        assert_eq!(volta, preferida, "não esperou a preferida soltar");
        assert!(escolher_porta_do_teleprompter(Duration::ZERO).is_some());
    }

    /// E o prazo não pode aparecer no caminho de quem digita um IP, que é o caminho batido.
    #[test]
    fn ip_digitado_nao_passa_pelo_resolvedor() {
        let inicio = Instant::now();
        let addr = endereco_manual_com_prazo("192.168.56.41:9000", Duration::from_millis(1))
            .expect("IP literal não depende de DNS");
        assert_eq!(addr.port(), 9000);
        assert!(inicio.elapsed() < Duration::from_millis(200));
    }

    fn aparelho(enderecos: Vec<&str>) -> DiscoveredDevice {
        DiscoveredDevice {
            announcement: exemplo(),
            addresses: ordenar_enderecos_com_escopo(
                enderecos
                    .into_iter()
                    .map(|e| e.parse::<IpAddr>().expect("endereço").into())
                    .collect(),
            ),
            signaling_port: 7877,
            fullname: "x._quall._tcp.local.".into(),
        }
    }

    #[test]
    fn endpoint_prefere_ipv4_privado_a_ipv6_link_local() {
        assert_eq!(
            aparelho(vec!["fe80::1", "192.168.56.41"])
                .endpoint()
                .expect("endpoint")
                .to_string(),
            "192.168.56.41:7877"
        );
    }

    /// Regressão da bancada: o MacBook anunciou 70 endereços e o `169.254.232.1` (APIPA, de uma
    /// interface sem DHCP) vinha antes do `192.168.56.131` na lista do mDNS.
    #[test]
    fn endpoint_nao_escolhe_apipa_havendo_endereco_de_lan() {
        assert_eq!(
            aparelho(vec![
                "169.254.232.1",
                "127.0.0.1",
                "fe80::94c7:3bff:fe76:8eba",
                "192.168.56.131",
            ])
            .endpoint()
            .expect("endpoint")
            .to_string(),
            "192.168.56.131:7877"
        );
    }

    #[test]
    fn apipa_ainda_serve_quando_nao_ha_mais_nada() {
        assert_eq!(
            aparelho(vec!["169.254.232.1"])
                .endpoint()
                .expect("endpoint")
                .ip()
                .to_string(),
            "169.254.232.1"
        );
    }

    #[test]
    fn enderecos_repetidos_somem() {
        let a = aparelho(vec![
            "192.168.56.131",
            "fe80::1",
            "192.168.56.131",
            "fe80::1",
            "192.168.56.131",
        ]);
        assert_eq!(a.addresses.len(), 2, "sobrou repetição: {:?}", a.addresses);
    }

    /// **Dívida 3.** O anúncio tem de sumir da LAN quando o `Advertiser` morre — por `stop` ou
    /// por `Drop`.
    ///
    /// Isto usa mDNS de verdade, no loopback e nas interfaces desta máquina. É o único jeito de
    /// provar o item: o mapa de serviços vive dentro do daemon do `mdns-sd`, e a única janela
    /// para ele é a rede. Se o multicast estiver bloqueado, o teste **diz isso** em vez de
    /// passar por engano.
    #[test]
    fn soltar_o_anunciante_tira_o_anuncio_da_lan() {
        // Id único por execução: a bancada tem outros aparelhos Quall na mesma LAN, e um
        // `device_id` fixo faria este teste depender deles.
        let id = format!(
            "teste-divida-3-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let mut anuncio = exemplo();
        anuncio.device_id = DeviceId(id.clone());

        let anunciante = Advertiser::start(&anuncio, 7877).expect("anunciante");
        let fullname = anunciante.fullname().to_owned();
        // Cada fase usa um navegador **novo**: ele manda uma consulta nova, e um daemon que
        // morreu não responde. Reaproveitar o navegador testaria o cache dele, não a LAN.
        let esta_na_lan = |quanto: Duration| -> bool {
            let navegador = Browser::start().expect("navegador");
            navegador
                .collect(quanto)
                .map(|lista| lista.iter().any(|a| a.fullname == fullname))
                .unwrap_or(false)
        };

        assert!(anunciante.ativo());

        if !esta_na_lan(Duration::from_secs(3)) {
            // Sem multicast não há o que provar aqui, e afirmar que passou seria mentira.
            panic!(
                "o mDNS não achou o próprio anúncio nesta máquina; sem multicast este teste não \
                 prova nada e não deve ser lido como verde"
            );
        }

        drop(anunciante);

        // O `unregister` manda um adeus com TTL 0; o daemon do navegador tira da lista.
        assert!(
            !esta_na_lan(Duration::from_secs(3)),
            "o anúncio continuou na LAN depois de o `Advertiser` morrer"
        );
    }

    /// **Defeito 6 da revisão**: o nome da instância mDNS é um rótulo DNS, até 63 bytes. Com o
    /// papel acrescentado, um prompter de nome longo passava disso e o `mdns-sd` descartava o
    /// registro **em silêncio** — `Advertiser::start` devolvia `Ok` e o aparelho não aparecia em
    /// lista nenhuma. Usa mDNS de verdade, como o teste da dívida 3.
    #[test]
    fn prompter_de_nome_longo_e_acentuado_aparece_na_lista() {
        let id = format!(
            "teste-nome-longo-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let mut a = exemplo();
        a.device_id = DeviceId(id.clone());
        a.display_name = "Teleprompter do estúdio de gravação — câmera à esquerda, Ângulo B".into();
        a.papel = Some(Papel::Teleprompter);
        let anunciante = Advertiser::start(&a, 7879).expect("anunciante");
        let navegador = Browser::start().expect("navegador");
        let achados = navegador
            .collect(Duration::from_secs(3))
            .expect("navegação");
        navegador.stop();
        let achado = achados.iter().find(|x| x.fullname == anunciante.fullname());
        assert!(achado.is_some(), "o prompter de nome longo sumiu da lista");
        let achado = achado
            .map(|x| x.announcement.clone())
            .unwrap_or_else(exemplo);
        assert_ne!(
            achado.display_name, a.display_name,
            "o nome pessoal não vai na rede"
        );
        assert!(achado.device_id.0.starts_with("discovery-"));
        assert_eq!(achado.papel, Some(Papel::Teleprompter));
    }

    #[test]
    fn rotulos_efemeros_nao_dependem_da_identidade_real() {
        let a = exemplo();
        let first: HashMap<_, _> = announcement_to_txt(&a, 7877).unwrap().into_iter().collect();
        let second: HashMap<_, _> = announcement_to_txt(&a, 7877).unwrap().into_iter().collect();
        assert_ne!(first["t"], second["t"]);
        assert!(!first.contains_key("id") && !first.contains_key("n"));
        let instance = nome_da_instancia(&first["t"]).unwrap();
        assert!(instance.len() <= 63 && instance.is_ascii());
        assert!(!instance.contains(&a.display_name) && !instance.contains(&a.device_id.0));
        let public = serde_json::to_string(&first).unwrap();
        assert!(!public.contains(&a.display_name) && !public.contains(&a.device_id.0));
    }

    #[test]
    fn descoberta_recusa_legado_token_invalido_e_identidade_publica() {
        let base: HashMap<_, _> = announcement_to_txt(&exemplo(), 7877)
            .unwrap()
            .into_iter()
            .collect();
        for (field, value) in [
            ("v", "2"),
            ("t", "nome-de-aparelho"),
            ("t", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            ("id", "id-persistente"),
            ("n", "nome pessoal"),
            ("p", "0"),
        ] {
            let mut props = base.clone();
            props.insert(field.into(), value.into());
            assert!(announcement_from_txt(&props).is_err(), "{field}");
        }
        assert!(nome_da_instancia("nome pessoal").is_err());
    }

    #[test]
    fn sem_endereco_nao_ha_endpoint() {
        assert!(aparelho(vec![]).endpoint().is_none());
    }
}
