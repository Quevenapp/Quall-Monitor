//! O endereço que a tela de espera mostra: "é assim que os outros te encontram".
//!
//! Mantém a prioridade das sondas IPv4 pela tabela de rotas, mas valida a origem contra interfaces
//! LAN físicas e ativas. Sem IPv4 apto, usa IPv6 ULA/global dessas interfaces. VPN, adaptadores
//! virtuais e celular não viram o endereço oferecido na UI. IPv6 link-local não serve como endereço
//! manual sem conhecer o escopo no receptor; sua descoberta mDNS é responsabilidade do núcleo.
//! Um UDP conectado consulta a rota sem enviar bytes; a enumeração também é somente local.

use std::net::{IpAddr, SocketAddr, UdpSocket};

/// Destinos usados só como pergunta à tabela de rotas. Nenhum byte é enviado para eles.
///
/// Nesta ordem, porque o primeiro sozinho mente numa rede LAN-only sem gateway padrão —
/// que é exatamente o tipo de rede em que este produto tem de funcionar. O segundo é um endereço
/// de LAN privada qualquer: basta existir rota para a própria sub-rede.
const SONDAS: [&str; 3] = ["192.168.1.1:9", "10.0.0.1:9", "8.8.8.8:53"];

/// Um endereço manual da LAN: IPv4 primeiro, depois IPv6 ULA/global físico, ou `None`.
pub fn ip_local() -> Option<IpAddr> {
    #[cfg(windows)]
    let lan = sistema::enderecos_lan();
    // O módulo pertence ao app Windows; esta saída só permite testar as regras no host.
    #[cfg(not(windows))]
    let lan = Vec::new();
    let rotas = SONDAS.into_iter().filter_map(|destino| {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect(destino).ok()?;
        socket.local_addr().ok().map(|a| a.ip())
    });
    escolher(&lan, rotas)
}

fn escolher(lan: &[IpAddr], rotas: impl IntoIterator<Item = IpAddr>) -> Option<IpAddr> {
    rotas.into_iter().find(|ip| ip.is_ipv4() && apto(*ip) && lan.contains(ip))
        .or_else(|| lan.iter().copied().find(|ip| ip.is_ipv4() && apto(*ip)))
        .or_else(|| lan.iter().copied().find(|ip| ip.is_ipv6() && apto(*ip)))
}

fn apto(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast()
                && !ip.is_broadcast() && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
        }
        // Só ULA fc00::/7 ou unicast global 2000::/3. Exclui link-local fe80::/10,
        // multicast, loopback, IPv4 mapeado e prefixos de tradução CLAT/NAT64.
        IpAddr::V6(ip) => {
            let primeiro = ip.segments()[0];
            primeiro & 0xfe00 == 0xfc00 || primeiro & 0xe000 == 0x2000
        }
    }
}

fn interface_lan(tipo: u32, flags: u8, ativa: bool, wan: bool) -> bool {
    // IANA IfType Ethernet (inclui USB/RNDIS Ethernet) ou IEEE80211. No MIB_IF_ROW2,
    // bit 0 = HardwareInterface, bit 1 = FilterInterface. VPN/Hyper-V não são hardware LAN.
    ativa && !wan && matches!(tipo, 6 | 71) && flags & 3 == 1
}

#[cfg(windows)]
mod sistema {
    use super::{apto, interface_lan};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use windows::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIfEntry2, GetUnicastIpAddressTable, MIB_IF_ROW2,
        MIB_UNICASTIPADDRESS_ROW, MIB_UNICASTIPADDRESS_TABLE,
    };
    use windows::Win32::NetworkManagement::Ndis::{
        IfOperStatusUp, NdisMediumWan, NdisMediumWirelessWan,
        NdisPhysicalMediumWiredWAN, NdisPhysicalMediumWirelessWan,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, IpDadStatePreferred};

    struct Tabela(*mut MIB_UNICASTIPADDRESS_TABLE);
    impl Drop for Tabela {
        fn drop(&mut self) { unsafe { FreeMibTable(self.0.cast()) } }
    }

    pub(super) fn enderecos_lan() -> Vec<IpAddr> {
        unsafe {
            let mut ptr = std::ptr::null_mut();
            let erro = GetUnicastIpAddressTable(AF_UNSPEC, &mut ptr);
            if erro.0 != 0 {
                crate::registro::linha(format!("endereços: !! GetUnicastIpAddressTable: {erro:?}")); // i18n: fora (diário)
                return Vec::new();
            }
            if ptr.is_null() { return Vec::new(); }
            let tabela = Tabela(ptr);
            // A API aloca a tabela com NumEntries linhas; o array de uma linha é o membro
            // flexível do SDK. FreeMibTable libera a mesma alocação ao sair, inclusive por panic.
            let linhas = std::slice::from_raw_parts(
                std::ptr::addr_of!((*tabela.0).Table).cast::<MIB_UNICASTIPADDRESS_ROW>(),
                (*tabela.0).NumEntries as usize,
            );
            let mut ips = Vec::new();
            for linha in linhas {
                if linha.SkipAsSource || linha.DadState != IpDadStatePreferred { continue; }
                let ip = match linha.Address.si_family {
                    AF_INET => IpAddr::V4(Ipv4Addr::from(linha.Address.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes())),
                    AF_INET6 => IpAddr::V6(Ipv6Addr::from(linha.Address.Ipv6.sin6_addr.u.Byte)),
                    _ => continue,
                };
                if !apto(ip) { continue; }
                let mut interface = MIB_IF_ROW2 {
                    InterfaceLuid: linha.InterfaceLuid, ..Default::default()
                };
                let erro = GetIfEntry2(&mut interface);
                if erro.0 != 0 {
                    crate::registro::linha(format!("endereços: !! GetIfEntry2: {erro:?}")); // i18n: fora (diário)
                    continue;
                }
                // Alguns modems WWAN se apresentam como Ethernet: o tipo sozinho não basta.
                let wan = interface.MediaType == NdisMediumWan
                    || interface.MediaType == NdisMediumWirelessWan
                    || interface.PhysicalMediumType == NdisPhysicalMediumWiredWAN
                    || interface.PhysicalMediumType == NdisPhysicalMediumWirelessWan;
                if interface_lan(interface.Type, interface.InterfaceAndOperStatusFlags._bitfield,
                    interface.OperStatus == IfOperStatusUp, wan) && !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
            ips
        }
    }
}

/// `192.168.1.41:7877` ou `[fd12::41]:7877` — o texto que a pessoa digita do outro lado.
///
/// A porta vem de fora porque quem a escolhe é o servidor de sinalização
/// (`SignalingServer::bind(0)` e depois `port()`), não a casca. Perguntar a porta ao servidor em
/// vez de reservá-la antes fecha a corrida clássica: entre "reservei a porta 51234" e "hospedei
/// nela" cabe outro processo pegando a mesma porta.
pub fn para_digitar(porta: u16) -> Option<String> {
    ip_local().map(|ip| com_porta(ip, porta))
}

/// SocketAddr mantém IPv4 igual e delimita IPv6 com os colchetes exigidos no texto digitável.
pub fn com_porta(ip: IpAddr, porta: u16) -> String { SocketAddr::new(ip, porta).to_string() }

#[cfg(test)]
mod testes {
    use super::*;
    fn ip(texto: &str) -> IpAddr { texto.parse().unwrap() }

    #[test]
    fn as_rotas_ipv4_mantem_a_prioridade_anterior() {
        let cabo = ip("192.168.15.8");
        let wifi = ip("10.0.0.8");
        assert_eq!(escolher(&[ip("fd12::8"), cabo, wifi], [wifi, cabo]), Some(wifi));
        assert_eq!(escolher(&[cabo, wifi], [cabo, wifi]), Some(cabo));
    }

    #[test]
    fn rota_de_vpn_nao_esconde_lan_fisica() {
        let lan = ip("192.168.15.8");
        assert_eq!(escolher(&[lan], [ip("10.7.0.8"), lan]), Some(lan));
    }

    #[test]
    fn ipv4_fisico_sem_rota_padrao_precede_ipv6() {
        let lan = ip("192.168.15.8");
        assert_eq!(escolher(&[ip("fd12::8"), lan], []), Some(lan));
    }

    #[test]
    fn cabo_apipa_continua_antes_do_ipv6() {
        let cabo = ip("169.254.5.8");
        assert_eq!(escolher(&[ip("fd12::8"), cabo], []), Some(cabo));
    }

    #[test]
    fn ipv6_ula_e_global_sao_fallbacks_digitaveis() {
        for texto in ["fd12::8", "fc00::8", "2001:db8::8"] {
            let endereco = ip(texto);
            assert_eq!(escolher(&[endereco], []), Some(endereco));
            assert_eq!(com_porta(endereco, 7877), format!("[{texto}]:7877"));
        }
        assert_eq!(com_porta(ip("192.168.15.8"), 7877), "192.168.15.8:7877");
    }

    #[test]
    fn enderecos_de_celular_clat_e_link_local_ipv6_nao_sao_oferecidos() {
        for texto in ["100.64.0.1", "100.127.255.254", "192.0.0.1", "192.0.0.255",
            "0.0.0.0", "127.0.0.1", "224.0.0.1", "255.255.255.255",
            "::", "::1", "::ffff:192.168.15.8", "fe80::8", "febf::8", "ff02::1",
            "64:ff9b::c000:1", "64:ff9b:1::1"] {
            let endereco = ip(texto);
            assert_eq!(escolher(&[endereco], [endereco]), None, "{texto}");
        }
    }

    #[test]
    fn bordas_do_cgnat_e_clat_nao_excluem_outras_redes_ipv4() {
        for texto in ["100.63.255.254", "100.128.0.1", "192.0.1.8", "172.16.0.8"] {
            assert!(apto(ip(texto)), "{texto}");
        }
    }

    #[test]
    fn so_ethernet_usb_e_wifi_fisicos_ativos_sao_lan() {
        assert!(interface_lan(6, 1, true, false));
        assert!(interface_lan(71, 1, true, false));
        assert!(!interface_lan(6, 1, false, false));
        assert!(!interface_lan(6, 0, true, false)); // Hyper-V/TAP/VPN virtual
        assert!(!interface_lan(6, 3, true, false)); // filtro de uma interface física
        assert!(!interface_lan(6, 1, true, true)); // WWAN emulando Ethernet
        for tipo in [23, 24, 131, 243, 244] { assert!(!interface_lan(tipo, 1, true, false)); }
    }

    #[test]
    fn sem_lan_apta_nao_inventa_endereco() {
        assert_eq!(escolher(&[], [ip("192.168.15.8")]), None);
    }
}
