//! **A tabela de vizinhos do Windows, para a sondagem perguntar só a quem existe**
//! (`docs/v3-no-privado.md` §8.9). O núcleo não tem `unsafe` e recebe o leitor por
//! `quall_core::busca::usar_leitor_de_vizinhos`; este é o do Windows: `GetIpNetTable2(AF_INET)`, sem
//! permissão nova. Só os estados em que o endereço físico é conhecido contam — perguntar a eles não
//! pede resolução nova. **Não rodou no Dell.**

use std::net::Ipv4Addr;

/// `NL_NEIGHBOR_STATE`: `Probe` 2, `Delay` 3, `Stale` 4, `Reachable` 5, `Permanent` 6 têm endereço
/// físico; `Unreachable` 0 e `Incomplete` 1 não (são as "incompletas" do `arp -an`).
pub(crate) fn estado_completo(estado: i32) -> bool {
    (2..=6).contains(&estado)
}

#[cfg(windows)]
fn ler() -> Vec<Ipv4Addr> {
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIpNetTable2, MIB_IPNET_TABLE2};
    use windows::Win32::Networking::WinSock::AF_INET;

    let mut tabela: *mut MIB_IPNET_TABLE2 = std::ptr::null_mut();
    // SAFETY: `tabela` recebe uma tabela alocada pelo sistema, liberada com `FreeMibTable` abaixo.
    let r = unsafe { GetIpNetTable2(AF_INET, &mut tabela) };
    if r.0 != 0 || tabela.is_null() {
        return Vec::new();
    }
    let mut saida = Vec::new();
    // SAFETY: com `NO_ERROR`, `tabela` aponta para `NumEntries` linhas contíguas a partir de `Table`.
    unsafe {
        let n = (*tabela).NumEntries as usize;
        let linhas = std::slice::from_raw_parts((*tabela).Table.as_ptr(), n);
        for l in linhas {
            if l.Address.si_family != AF_INET || l.PhysicalAddressLength == 0 || !estado_completo(l.State.0) {
                continue;
            }
            let bytes = l.Address.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes();
            saida.push(Ipv4Addr::from(bytes));
        }
        FreeMibTable(tabela.cast());
    }
    saida
}

/// Põe o leitor no núcleo, uma vez. Fora do Windows, nada (o núcleo lê `/proc/net/arp` no Linux).
pub fn instalar() {
    #[cfg(windows)]
    {
        static FEITO: std::sync::Once = std::sync::Once::new();
        FEITO.call_once(|| quall_core::busca::usar_leitor_de_vizinhos(ler));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn so_os_estados_com_endereco_fisico() {
        assert!(!estado_completo(0), "Unreachable");
        assert!(!estado_completo(1), "Incomplete");
        for e in 2..=6 {
            assert!(estado_completo(e));
        }
        assert!(!estado_completo(7), "NlnsMaximum");
    }
}
