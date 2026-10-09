//! **A tela estendida no produto, sem driver nosso** (R10, decisão do Bruno em 02/10/2026).
//!
//! O Quall é grátis e não terá driver próprio de monitor virtual: a tela estendida no Windows usa o
//! SudoVDA **que a pessoa instalar**. O app só detecta o adaptador (o PnP diz; nada é aberto para
//! isso) e não instala nem mexe em certificado ou driver — nem o MSI, nem o MSIX
//! (`docs/monitor-virtual-windows.md` §7, a nota de 02/10).
//!
//! **Mudou em 02/10, à noite** (`docs/monitor-virtual-windows.md` §15): o SudoVDA vai junto com o
//! Quall, e o ladrilho apagado vira o botão "Instalar o driver da tela estendida", com a caixa que
//! explica antes e o UAC. As regras disso estão em `regras_do_driver.rs`; as daqui (quem espelha, o
//! lugar do apagado) seguem valendo. [`DETALHE_SEM_DRIVER`] e [`TEXTO_ACESSIVEL_SEM_DRIVER`] ficam
//! para a loja e o Windows sem suporte, onde o ladrilho volta ao texto e à página de antes.
//!
//! As regras aqui são aritmética, sem Win32 nem rede, e os testes rodam em qualquer máquina:
//!
//! - o ladrilho "Tela estendida" é escolhível com o adaptador presente, e **apagado** sem ele (com o
//!   detalhe [`DETALHE_SEM_DRIVER`] e a página do instalador do driver em [`URL_DO_INSTALADOR_DO_DRIVER`]);
//! - quem espelha é o **coordenador** de várias sessões (`varias.rs`) com a bandeira de bancada
//!   `--varias-sessoes`, como antes, **ou** quando a fonte escolhida é a tela estendida. Qualquer
//!   outra fonte sem a bandeira segue o caminho de uma sessão só, sem mudança;
//! - o lugar do ladrilho apagado na grade: logo depois dos monitores, onde o escolhível fica.

/// **A página do instalador avulso do driver** (`quall-driver.exe`, decisão do Bruno de 02/10,
/// noite): o ladrilho apagado da build da loja e do Windows sem suporte, e o "Desinstalar pelo
/// instalador do driver" dos Ajustes da loja, abrem esta página. O destino foi combinado com a
/// frente do site em 05/10/2026: página `/quall/`, seção `driver`. Ainda não foi publicado nem
/// verificado; confirmar a seção e o download antes de distribuir (`docs/recursos.md`, R10).
/// Substitui o link do GitHub do SudoVDA, que saiu.
pub const URL_DO_INSTALADOR_DO_DRIVER: &str = "https://queven.com.br/quall/#driver";

// Os textos dos ladrilhos ficam em português, como chaves da tabela: quem mostra (a janela) os passa
// por `idioma::t`.

/// O título dos dois ladrilhos (o escolhível e o apagado).
pub const TITULO: &str = "Tela estendida"; // i18n: chave

/// O detalhe do ladrilho escolhível.
pub const DETALHE: &str = "Um monitor novo para cada aparelho"; // i18n: chave

/// O detalhe do ladrilho apagado.
pub const DETALHE_SEM_DRIVER: &str = "Precisa do driver SudoVDA"; // i18n: chave

/// O nome do ladrilho apagado para o Narrador: o que ele é, e o que o clique faz.
pub const TEXTO_ACESSIVEL_SEM_DRIVER: &str =
    "Tela estendida, precisa do driver SudoVDA. Abre no navegador a página com as instruções de instalação"; // i18n: chave

/// Como a tela estendida entra no seletor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoSeletor {
    /// O adaptador do SudoVDA está presente: a fonte entra na lista e pode ser escolhida.
    Escolhivel,
    /// Sem o adaptador: o ladrilho aparece apagado, fora da lista de fontes (não se escolhe).
    Apagada,
}

/// A tela estendida no seletor, pelo adaptador. **Não depende de `--varias-sessoes`**: com a
/// bandeira e sem o adaptador, o ladrilho também aparece apagado (antes não aparecia).
pub fn no_seletor(adaptador_presente: bool) -> NoSeletor {
    if adaptador_presente {
        NoSeletor::Escolhivel
    } else {
        NoSeletor::Apagada
    }
}

/// **Quem espelha**: `true` é o coordenador de várias sessões; `false`, o caminho de uma sessão só.
///
/// - `bandeira`: `--varias-sessoes` (bancada). Com ela, sempre o coordenador, como antes.
/// - `escolheu_tela_estendida`: a fonte escolhida é a "Tela estendida".
/// - `camera_sintetica`: `--camera-sintetica` (bancada) troca a escolha pela câmera sintética nos
///   dois caminhos; sem a bandeira, ela segue no de uma sessão, como antes desta regra.
pub fn usa_o_coordenador(bandeira: bool, escolheu_tela_estendida: bool, camera_sintetica: bool) -> bool {
    bandeira || (escolheu_tela_estendida && !camera_sintetica)
}

/// O lugar do ladrilho apagado na grade: depois dos monitores, que vêm primeiro na lista
/// (`emissor::fontes_para_o_seletor`). `e_monitor` diz, linha a linha, se a fonte é um monitor.
pub fn lugar_do_apagado(e_monitor: &[bool]) -> usize {
    e_monitor.iter().take_while(|m| **m).count()
}

/// A casa da grade do ladrilho `i` da lista de fontes, com o apagado na casa `apagado` (ou sem ele).
pub fn casa_do_ladrilho(i: usize, apagado: Option<usize>) -> usize {
    match apagado {
        Some(a) if i >= a => i + 1,
        _ => i,
    }
}

// ================================================================================================
// A nova tentativa depois de uma falha (02/10)
// ================================================================================================

/// Quanto esperar depois de o dono da topologia cair antes de abrir outro: o vigia do SudoVDA tira os
/// monitores dele em 2–3 s sem o ping, e a varredura do novo não confirma o `REMOVE` — um `ADD` com o
/// mesmo GUID nesse meio-tempo devolveria o monitor que está saindo (a revisão de 02/10, item 4).
pub const ESPERA_DEPOIS_DO_DONO_MS: u64 = 5_000;

/// Os monitores virtuais do processo, vistos pelo próximo Espelhar da tela estendida.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instancia {
    /// Nenhuma aberta: a primeira vez, ou a abertura anterior falhou (a falha **não** é guardada).
    Nenhuma,
    /// Aberta, com o dono da topologia de pé.
    Viva,
    /// O dono caiu (pânico). `segurada`: alguém ainda segura o que era dele (uma sessão com o
    /// monitor, o fio do dono ou o do ping terminando); `ms_desde_a_morte`, há quanto tempo caiu.
    DonoMorto { segurada: bool, ms_desde_a_morte: u64 },
}

/// O que o Espelhar faz.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Abertura {
    Reusar,
    /// Abrir de novo. `herdar`: levar da anterior a placa já fixada e as travas do processo (a
    /// captura presa, o adaptador travado) — elas são do Windows, não do dono que caiu.
    Abrir { herdar: bool },
    /// Ainda não: o motivo, para a pessoa.
    Esperar(&'static str),
}

/// O motivo de esperar quando o dono caiu há pouco ou ainda há quem segure o que era dele. Em
/// português (é comparado nos testes e vai ao diário); [`conselho_da_falha`] o traduz.
pub const AINDA_SAINDO: &str = "os monitores da vez anterior ainda estão saindo; espere alguns segundos"; // i18n: chave

/// **A regra da nova tentativa.** Antes, a primeira falha ficava guardada num `OnceLock` e a tela
/// estendida só voltava reiniciando o app; agora só o sucesso fica, e cada Espelhar tenta de novo.
pub fn decidir_abertura(i: Instancia) -> Abertura {
    match i {
        Instancia::Nenhuma => Abertura::Abrir { herdar: false },
        Instancia::Viva => Abertura::Reusar,
        Instancia::DonoMorto { segurada: true, .. } => Abertura::Esperar(AINDA_SAINDO),
        Instancia::DonoMorto { ms_desde_a_morte, .. } if ms_desde_a_morte < ESPERA_DEPOIS_DO_DONO_MS => Abertura::Esperar(AINDA_SAINDO),
        Instancia::DonoMorto { .. } => Abertura::Abrir { herdar: true },
    }
}

/// O conselho da tela inicial quando a tela estendida não abriu: o motivo, e que o próximo Espelhar
/// tenta de novo (sem reiniciar o app). No idioma de agora: a frase em volta pela tabela, e o motivo
/// também quando ele é uma das frases daqui ([`AINDA_SAINDO`], [`sem_placa`]); o detalhe técnico de
/// um erro do sistema vai como veio.
pub fn conselho_da_falha(motivo: &str) -> String {
    let m = crate::idioma::tr(motivo.trim().trim_end_matches('.'));
    crate::idioma::tf("A tela estendida não está disponível: {}. Toque em Espelhar para tentar de novo.", &[&m])
}

// ================================================================================================
// A placa do monitor virtual, com ou sem Intel (02/10)
// ================================================================================================

pub const FORNECEDOR_INTEL: u32 = 0x8086;
pub const FORNECEDOR_NVIDIA: u32 = 0x10DE;

/// Uma placa de hardware (sem as indiretas, como a do SudoVDA, e sem as de software), na ordem da
/// DXGI: o LUID, o fornecedor, e se ela **anuncia** um encoder H.264 de hardware próprio
/// (`MFTEnum2` pelo LUID, sem ativar — a resposta é do registro, não muda de uma vez para outra).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacaCandidata {
    pub luid: u64,
    pub fornecedor: u32,
    pub tem_h264: bool,
}

/// **A placa que desenha o monitor virtual e codifica a imagem dele** (o `SET_RENDER_ADAPTER`, o
/// encoder e o dispositivo das sessões são todos dela).
///
/// A ordem é a da câmera R5 (`regras_r5::ordem_das_placas`): a Intel, as outras, a NVIDIA por
/// último; e a escolhida é a **primeira que anuncia um H.264**. Por que nunca "a primeira que
/// ativa": trocar a placa do SudoVDA de um processo para outro trava o adaptador até reiniciar
/// (`docs/monitor-virtual-windows.md` §12.3), e uma falha passageira de ativação faria o processo
/// seguinte escolher outra. Pelo anúncio, a escolha é a mesma em toda abertura do mesmo computador;
/// se a escolhida não ativar, a abertura recusa (e o próximo Espelhar tenta de novo, na mesma).
///
/// - No Dell (Intel UHD 630 + GTX 1660 Ti): a Intel, como sempre — o Quick Sync ativa na sessão
///   interativa e o MFT da NVIDIA não (`E_UNEXPECTED`).
/// - Só NVIDIA, só AMD, ou AMD + NVIDIA: a primeira da ordem que anuncia H.264.
/// - `sem_intel` (a bandeira de bancada `--monitor-sem-intel`): a Intel sai da lista, para provar
///   no Dell o caminho de quem não tem Intel.
pub fn placa_do_monitor(placas: &[PlacaCandidata], sem_intel: bool) -> Option<u64> {
    let posto = |f: u32| match f {
        FORNECEDOR_INTEL => 0u8,
        FORNECEDOR_NVIDIA => 2,
        _ => 1,
    };
    let mut v: Vec<(usize, &PlacaCandidata)> =
        placas.iter().enumerate().filter(|(_, p)| !(sem_intel && p.fornecedor == FORNECEDOR_INTEL)).collect();
    v.sort_by_key(|(i, p)| (posto(p.fornecedor), *i));
    v.into_iter().find(|(_, p)| p.tem_h264).map(|(_, p)| p.luid)
}

/// O motivo, para a pessoa, de nenhuma placa servir. Em português (vai também ao diário):
/// [`conselho_da_falha`] o traduz quando chega à tela.
pub fn sem_placa(placas: usize, sem_intel: bool) -> String {
    if placas == 0 {
        "nenhuma placa de vídeo de hardware foi encontrada".into() // i18n: chave
    } else if sem_intel {
        // i18n: fora (a bandeira de bancada `--monitor-sem-intel`)
        "nenhuma placa sem ser a Intel tem um codificador H.264 de hardware (a bandeira --monitor-sem-intel tira a Intel)".into()
    } else {
        "nenhuma placa de vídeo deste computador tem um codificador H.264 de hardware".into() // i18n: chave
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn a_placa_do_monitor_e_a_primeira_da_ordem_que_anuncia_h264() {
        let p = |luid, fornecedor, tem_h264| PlacaCandidata { luid, fornecedor, tem_h264 };
        let dell = [p(0x10717, FORNECEDOR_INTEL, true), p(0x10705, FORNECEDOR_NVIDIA, true)];
        assert_eq!(placa_do_monitor(&dell, false), Some(0x10717), "no Dell, a Intel, como sempre");
        let dell_ao_contrario = [dell[1], dell[0]];
        assert_eq!(placa_do_monitor(&dell_ao_contrario, false), Some(0x10717), "a ordem da DXGI não muda a escolha");
        assert_eq!(placa_do_monitor(&dell, true), Some(0x10705), "a bandeira de bancada tira a Intel");
        assert_eq!(placa_do_monitor(&[p(9, FORNECEDOR_NVIDIA, true)], false), Some(9), "só NVIDIA");
        assert_eq!(placa_do_monitor(&[p(7, 0x1002, true)], false), Some(7), "só AMD");
        assert_eq!(placa_do_monitor(&[p(9, FORNECEDOR_NVIDIA, true), p(7, 0x1002, true)], false), Some(7), "AMD + NVIDIA: a AMD");
        assert_eq!(
            placa_do_monitor(&[p(1, FORNECEDOR_INTEL, false), p(9, FORNECEDOR_NVIDIA, true)], false),
            Some(9),
            "uma Intel que não anuncia H.264 (sem Quick Sync) fica para trás"
        );
        assert_eq!(placa_do_monitor(&[p(1, FORNECEDOR_INTEL, false)], false), None);
        assert_eq!(placa_do_monitor(&[], false), None);
        assert_eq!(placa_do_monitor(&[p(1, FORNECEDOR_INTEL, true)], true), None, "só Intel, e a bandeira tira a Intel");
        assert_eq!(placa_do_monitor(&[p(5, FORNECEDOR_INTEL, true), p(4, FORNECEDOR_INTEL, true)], false), Some(5), "duas Intel: a ordem da DXGI");
        assert!(sem_placa(0, false).contains("nenhuma placa"));
        assert!(sem_placa(2, true).contains("--monitor-sem-intel"));
    }


    #[test]
    fn a_falha_nao_fica_guardada_e_o_dono_morto_espera_sair() {
        assert_eq!(decidir_abertura(Instancia::Nenhuma), Abertura::Abrir { herdar: false }, "nenhuma (ou a anterior falhou): abre de novo");
        assert_eq!(decidir_abertura(Instancia::Viva), Abertura::Reusar);
        assert_eq!(
            decidir_abertura(Instancia::DonoMorto { segurada: true, ms_desde_a_morte: 60_000 }),
            Abertura::Esperar(AINDA_SAINDO),
            "uma sessão ainda segura um monitor do dono que caiu: não abrir outro por cima"
        );
        assert_eq!(
            decidir_abertura(Instancia::DonoMorto { segurada: false, ms_desde_a_morte: ESPERA_DEPOIS_DO_DONO_MS - 1 }),
            Abertura::Esperar(AINDA_SAINDO),
            "o vigia ainda pode estar tirando os monitores"
        );
        assert_eq!(
            decidir_abertura(Instancia::DonoMorto { segurada: false, ms_desde_a_morte: ESPERA_DEPOIS_DO_DONO_MS }),
            Abertura::Abrir { herdar: true },
            "ninguém segura e o vigia já passou: abre, herdando a placa e as travas"
        );
        assert_eq!(
            conselho_da_falha("o adaptador SudoVDA não abre: acesso negado."),
            "A tela estendida não está disponível: o adaptador SudoVDA não abre: acesso negado. Toque em Espelhar para tentar de novo."
        );
    }

    #[test]
    fn o_seletor_segue_o_adaptador_e_nao_a_bandeira() {
        assert_eq!(no_seletor(true), NoSeletor::Escolhivel);
        assert_eq!(no_seletor(false), NoSeletor::Apagada);
    }

    #[test]
    fn sem_a_bandeira_so_a_tela_estendida_vai_ao_coordenador() {
        // Monitor ou câmera sem a bandeira: o caminho de uma sessão, como sempre.
        assert!(!usa_o_coordenador(false, false, false));
        // A tela estendida escolhida: o coordenador.
        assert!(usa_o_coordenador(false, true, false));
        // A câmera sintética de bancada troca a escolha: segue no caminho de uma sessão.
        assert!(!usa_o_coordenador(false, true, true));
        assert!(!usa_o_coordenador(false, false, true));
    }

    #[test]
    fn com_a_bandeira_tudo_vai_ao_coordenador_como_antes() {
        for estendida in [false, true] {
            for sintetica in [false, true] {
                assert!(usa_o_coordenador(true, estendida, sintetica));
            }
        }
    }

    #[test]
    fn o_apagado_fica_depois_dos_monitores() {
        assert_eq!(lugar_do_apagado(&[]), 0);
        assert_eq!(lugar_do_apagado(&[true]), 1);
        assert_eq!(lugar_do_apagado(&[true, true, false, false]), 2);
        // Sem monitor (a Sessão 0, por exemplo), na frente das câmeras.
        assert_eq!(lugar_do_apagado(&[false, false]), 0);
    }

    #[test]
    fn as_casas_pulam_a_do_apagado() {
        assert_eq!((0..4).map(|i| casa_do_ladrilho(i, None)).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!((0..4).map(|i| casa_do_ladrilho(i, Some(2))).collect::<Vec<_>>(), vec![0, 1, 3, 4]);
        assert_eq!((0..2).map(|i| casa_do_ladrilho(i, Some(0))).collect::<Vec<_>>(), vec![1, 2]);
        // O apagado no fim: ninguém se mexe.
        assert_eq!((0..2).map(|i| casa_do_ladrilho(i, Some(2))).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn a_url_e_a_do_instalador_do_driver() {
        // Provisória (02/10, noite): https e sem o GitHub do SudoVDA.
        assert_eq!(URL_DO_INSTALADOR_DO_DRIVER, "https://queven.com.br/quall/#driver");
        assert!(!URL_DO_INSTALADOR_DO_DRIVER.contains("github"));
    }
}
