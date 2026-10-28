//! **A tabela ARP do sistema, para a sondagem perguntar só a quem existe** (`docs/v3-no-privado.md`
//! §8.9). O núcleo é `forbid(unsafe_code)` e recebe o leitor por `busca::usar_leitor_de_vizinhos`;
//! este é o do macOS e do iOS: o `sysctl` `NET_RT_FLAGS`/`RTF_LLINFO`, o mesmo que o `arp -an` lê.
//! Só as entradas **completas** (com endereço físico) contam: perguntar a elas não pede ARP novo.
//!
//! No iOS, não medido: se a caixa de areia recusar, a lista vem vazia e a busca segue sem vizinhos.
//! Nas outras plataformas a fronteira não põe leitor (o núcleo lê `/proc/net/arp` no Linux).

use std::net::Ipv4Addr;

/// `sizeof(struct rt_msghdr)` no macOS/iOS (conferido com o compilador do sistema, 14/10).
const TAMANHO_DO_CABECALHO: usize = 92;
const AF_INET: u8 = 2;
const AF_LINK: u8 = 18;
const RTA_DST: i32 = 0x1;
const RTA_GATEWAY: i32 = 0x2;

/// O alinhamento dos `sockaddr` numa mensagem de rota (`ROUNDUP` de `route.c`, 4 bytes).
fn arredondar(n: usize) -> usize {
    if n == 0 {
        4
    } else {
        1 + ((n - 1) | 3)
    }
}

/// **As entradas completas de um despejo `NET_RT_FLAGS`.** Puro, para dar para testar: cada mensagem
/// é um `rt_msghdr`, o `sockaddr_in` do destino e o `sockaddr_dl` do gateway; completa é a que tem
/// `sdl_alen > 0`. Mensagem truncada ou estranha para a leitura, sem pânico.
pub(crate) fn completas_do_despejo(buf: &[u8]) -> Vec<Ipv4Addr> {
    let mut saida = Vec::new();
    let mut i = 0usize;
    while i + TAMANHO_DO_CABECALHO <= buf.len() {
        let tamanho = usize::from(u16::from_ne_bytes([buf[i], buf[i + 1]]));
        if tamanho < TAMANHO_DO_CABECALHO || i + tamanho > buf.len() {
            break;
        }
        let msg = &buf[i..i + tamanho];
        i += tamanho;
        let enderecos = i32::from_ne_bytes([msg[12], msg[13], msg[14], msg[15]]);
        if enderecos & (RTA_DST | RTA_GATEWAY) != (RTA_DST | RTA_GATEWAY) {
            continue;
        }
        let dst = &msg[TAMANHO_DO_CABECALHO..];
        if dst.len() < 8 || dst[1] != AF_INET {
            continue;
        }
        let ip = Ipv4Addr::new(dst[4], dst[5], dst[6], dst[7]);
        let pulo = arredondar(usize::from(dst[0]));
        let Some(gw) = dst.get(pulo..) else { continue };
        if gw.len() < 8 || gw[1] != AF_LINK {
            continue;
        }
        if gw[6] > 0 {
            saida.push(ip);
        }
    }
    saida
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod sistema {
    use std::os::raw::{c_int, c_uint, c_void};

    extern "C" {
        fn sysctl(
            name: *mut c_int,
            namelen: c_uint,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }

    /// `CTL_NET, PF_ROUTE, 0, AF_INET, NET_RT_FLAGS, RTF_LLINFO`.
    const MIB: [c_int; 6] = [4, 17, 0, 2, 2, 0x400];

    pub(super) fn despejo() -> Vec<u8> {
        for _ in 0..3 {
            let mut mib = MIB;
            let mut tamanho: usize = 0;
            // SAFETY: `mib` tem 6 inteiros; com `oldp` nulo o sistema só escreve o tamanho em `tamanho`.
            let r = unsafe {
                sysctl(mib.as_mut_ptr(), 6, std::ptr::null_mut(), &mut tamanho, std::ptr::null_mut(), 0)
            };
            if r != 0 || tamanho == 0 {
                return Vec::new();
            }
            // Folga: a tabela pode crescer entre as duas chamadas.
            let mut buf = vec![0u8; tamanho + tamanho / 4 + 512];
            let mut cabe = buf.len();
            // SAFETY: `buf` tem `cabe` bytes graváveis e o sistema escreve no máximo isso, devolvendo
            // em `cabe` quanto escreveu.
            let r = unsafe {
                sysctl(mib.as_mut_ptr(), 6, buf.as_mut_ptr().cast(), &mut cabe, std::ptr::null_mut(), 0)
            };
            if r == 0 {
                buf.truncate(cabe);
                return buf;
            }
            // ENOMEM: cresceu demais; de novo.
        }
        Vec::new()
    }
}

/// O leitor que a fronteira dá ao núcleo.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn ler() -> Vec<Ipv4Addr> {
    completas_do_despejo(&sistema::despejo())
}

/// Põe o leitor desta plataforma no núcleo, uma vez.
pub(crate) fn instalar() {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        static FEITO: std::sync::Once = std::sync::Once::new();
        FEITO.call_once(|| quall_core::busca::usar_leitor_de_vizinhos(ler));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mensagem(ip: [u8; 4], alen: u8, enderecos: i32) -> Vec<u8> {
        let mut m = vec![0u8; TAMANHO_DO_CABECALHO];
        m[12..16].copy_from_slice(&enderecos.to_ne_bytes());
        // sockaddr_inarp: 16 bytes.
        let mut sin = vec![0u8; 16];
        sin[0] = 16;
        sin[1] = AF_INET;
        sin[4..8].copy_from_slice(&ip);
        m.extend(sin);
        // sockaddr_dl: 20 bytes, com `alen` bytes de endereço físico.
        let mut sdl = vec![0u8; 20];
        sdl[0] = 20;
        sdl[1] = AF_LINK;
        sdl[6] = alen;
        m.extend(sdl);
        let n = u16::try_from(m.len()).expect("cabe");
        m[0..2].copy_from_slice(&n.to_ne_bytes());
        m
    }

    #[test]
    fn so_as_completas_e_o_truncado_para_sem_panico() {
        let mut buf = mensagem([192, 168, 15, 1], 6, 3);
        buf.extend(mensagem([192, 168, 15, 3], 0, 3));
        buf.extend(mensagem([192, 168, 15, 8], 6, 3));
        buf.extend(mensagem([192, 168, 15, 10], 6, 1));
        assert_eq!(completas_do_despejo(&buf), vec![Ipv4Addr::new(192, 168, 15, 1), Ipv4Addr::new(192, 168, 15, 8)]);
        let truncado = &buf[..buf.len() - 5];
        assert_eq!(completas_do_despejo(truncado).len(), 2);
        assert!(completas_do_despejo(&[1, 2, 3]).is_empty());
    }

    fn completas_do_arp(texto: &str) -> Vec<Ipv4Addr> {
        texto
            .lines()
            .filter(|l| !l.contains("incomplete"))
            .filter_map(|l| l.split(['(', ')']).nth(1)?.parse().ok())
            .collect()
    }

    /// **Um despejo de verdade**, quando há: `QUALL_DESPEJO_ARP` (os bytes do `sysctl`) e
    /// `QUALL_ARP_AN` (o `arp -an` da mesma hora). No Mac da bancada, o despejo sai de um `python3`
    /// com `ctypes` — um binário nosso recebe a tabela vazia (§8.9) —, e o leitor tem de achar
    /// exatamente as completas do `arp`.
    #[test]
    fn um_despejo_de_verdade_bate_com_o_arp() {
        let (Ok(d), Ok(a)) = (std::env::var("QUALL_DESPEJO_ARP"), std::env::var("QUALL_ARP_AN")) else { return };
        let despejo = std::fs::read(d).expect("o despejo");
        let arp = std::fs::read_to_string(a).expect("o arp");
        let mut nossas = completas_do_despejo(&despejo);
        let mut do_arp = completas_do_arp(&arp);
        nossas.sort();
        do_arp.sort();
        eprintln!("despejo: {} completas, arp: {}", nossas.len(), do_arp.len());
        assert!(!nossas.is_empty());
        assert_eq!(nossas, do_arp);
    }

    /// **No Mac, nesta hora**: o que o leitor acha é das completas do `arp -an` do mesmo processo.
    /// (Num binário nosso fora do app as duas vêm vazias neste Mac; o teste só confere coerência.)
    #[cfg(target_os = "macos")]
    #[test]
    fn no_mac_o_leitor_e_o_arp_concordam() {
        let nossas = ler();
        let Ok(saida) = std::process::Command::new("/usr/sbin/arp").arg("-an").output() else { return };
        let do_arp = completas_do_arp(&String::from_utf8_lossy(&saida.stdout));
        eprintln!("vizinhos: {} nossos, {} do arp", nossas.len(), do_arp.len());
        let comuns = nossas.iter().filter(|ip| do_arp.contains(ip)).count();
        assert!(comuns + 2 >= nossas.len().min(do_arp.len()), "nossas {nossas:?} / arp {do_arp:?}");
    }
}
