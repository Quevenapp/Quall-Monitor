//! **As regras da bandeja** (o ícone do Quall na área de notificação, desde 01/10/2026): o texto da
//! dica e da linha de estado do menu, o que cada aviso que o ícone manda quer dizer, e o que
//! minimizar faz. Aritmética pura, sem Win32, para os testes rodarem no portão e em qualquer máquina.
//! O Win32 (o `Shell_NotifyIconW`, o menu, a marca do primeiro aviso) é `bandeja.rs`.
//!
//! Tudo o que a bandeja diz sai do mesmo [`EstadoDaTela`] que a janela desenha: ela não lê o emissor
//! nem o receptor por conta própria, e não pode dizer uma coisa enquanto a janela diz outra.

use crate::estilo::Icone;
use crate::idioma::{t, tf, Idioma};
use crate::modelo_da_janela::{Cena, EstadoDaTela, TeleprompterAberto, ENCERRANDO};

/// O tamanho de `szTip` do `NOTIFYICONDATAW`, **com** o nulo do fim: 128 unidades UTF-16.
pub const TAMANHO_DA_DICA: usize = 128;
/// O de `szInfoTitle` (64) e o de `szInfo` (256), também com o nulo.
pub const TAMANHO_DO_TITULO_DO_AVISO: usize = 64;
pub const TAMANHO_DO_AVISO: usize = 256;

/// **O aviso da primeira vez** (uma vez por pasta de dados). No Windows 11 um ícone novo cai no menu
/// escondido (o "^") da área de notificação, e quem minimizou não acha onde o Quall foi parar. Em
/// português, como chave da tabela: `bandeja.rs` o passa por `t()` na hora de pedir o aviso.
pub const TITULO_DO_AVISO: &str = "Quall Monitor"; // i18n: fora (a marca)
pub const TEXTO_DO_AVISO: &str = "O Quall continua rodando aqui. Clique no ícone para abrir."; // i18n: chave

/// A linha do menu quando nada está de pé: a mesma palavra do Mac. O inglês sai de [`ocioso`], e
/// não da tabela: "Pronto" lá é o botão (o glossário diz "Done"), e aqui é o estado ("Ready").
pub const OCIOSO: &str = "Pronto"; // i18n: fora (o inglês é o de `ocioso`)

/// [`OCIOSO`] no idioma de agora.
pub fn ocioso() -> &'static str {
    match crate::idioma::atual() {
        Idioma::Pt => OCIOSO,
        Idioma::En => "Ready", // i18n: fora (já é o inglês)
    }
}

/// O quanto a linha de estado do menu pode ter (em unidades UTF-16) antes do corte: um nome de
/// aparelho enorme não estica o menu pela tela.
const TAMANHO_DA_LINHA_DO_MENU: usize = 80;

/// **A linha de estado**: o que o Quall está fazendo agora, em poucas palavras — a sessão da janela
/// e o teleprompter de janela própria, que "Sair do Quall" também fecha. Vazia com nada de pé (a
/// dica fica só "Quall", e o menu diz [`OCIOSO`]).
///
/// **Sem o PIN**: quem compartilha a tela numa chamada mostraria o PIN ao passar o mouse no ícone.
/// Ele fica na janela.
pub fn linha_de_estado(e: &EstadoDaTela) -> String {
    let sessao = linha_da_sessao(e);
    match (e.teleprompter.map(linha_do_teleprompter), sessao.is_empty()) {
        (None, _) => sessao,
        (Some(tp), true) => tp.to_string(),
        (Some(tp), false) => format!("{sessao} · {tp}"),
    }
}

/// O teleprompter aberto, com o papel (os nomes dos cartões do painel Teleprompter).
fn linha_do_teleprompter(aberto: TeleprompterAberto) -> &'static str {
    match aberto {
        TeleprompterAberto::Escolha => t("Teleprompter aberto"),
        TeleprompterAberto::Prompter => t("Teleprompter: mostrando o texto"),
        TeleprompterAberto::Controle => t("Teleprompter: controlando"),
        TeleprompterAberto::PrompterComCamera => t("Teleprompter: texto com a câmera"),
    }
}

/// A sessão que a janela mostra (vazia nos painéis). No idioma de agora: a dica é refeita a cada
/// `atualizar` da janela, que roda também quando o idioma troca.
fn linha_da_sessao(e: &EstadoDaTela) -> String {
    // A origem escolhida é a tela estendida (um monitor novo para cada aparelho)?
    let estendida = e.espelhar.fontes.iter().any(|f| f.escolhido && f.icone == Icone::TelaEstendida);
    let quem_espelha = if e.espelhar.camera { t("Câmera no ar") } else { t("Espelhando") };
    let para = |base: &str, par: &str| {
        let par = par.trim();
        if par.is_empty() {
            base.to_string()
        } else {
            tf("{} para {}", &[&base, &par])
        }
    };
    match e.cena {
        Cena::Painel(_) => String::new(),
        // Pelo estado, e não pelo texto do aviso (que muda com o idioma).
        Cena::Esperando if e.espera.encerrando => t(ENCERRANDO).to_string(),
        Cena::Esperando => t("Esperando um aparelho").to_string(),
        Cena::NoAr if estendida => t("Tela estendida: 1 aparelho").to_string(),
        Cena::NoAr => para(quem_espelha, &e.no_ar.par),
        Cena::Varios => {
            let n = e.varios.receptores.len();
            let aparelhos = if n == 1 { t("1 aparelho").to_string() } else { tf("{} aparelhos", &[&n]) };
            match (estendida, n) {
                (true, 0) => t("Tela estendida: esperando um aparelho").to_string(),
                (true, _) => tf("Tela estendida: {}", &[&aparelhos]),
                (false, 0) => t("Esperando um aparelho").to_string(),
                (false, _) => tf("{} para {}", &[&quem_espelha, &aparelhos]),
            }
        }
        Cena::Conectando if e.conectando.trim().is_empty() => t("Conectando…").to_string(),
        Cena::Conectando => tf("Conectando a {}", &[&e.conectando.trim()]),
        Cena::Exibindo if e.exibindo.encerrando => t(ENCERRANDO).to_string(),
        Cena::Exibindo if e.exibindo.par.trim().is_empty() => t("Exibindo").to_string(),
        Cena::Exibindo => tf("Exibindo {}", &[&e.exibindo.par.trim()]),
    }
}

/// **A dica do ícone**: "Quall", e a linha de estado embaixo quando há uma. Cabe em `szTip` (o corte,
/// com reticências, é feito aqui, e nunca parte um par substituto do UTF-16).
pub fn dica(e: &EstadoDaTela) -> String {
    let linha = linha_de_estado(e);
    let texto = if linha.is_empty() { "Quall Monitor".to_string() } else { format!("Quall Monitor\n{linha}") }; // i18n: fora (a marca)
    cortar(&texto, TAMANHO_DA_DICA - 1)
}

/// **A linha de estado do menu** (o primeiro item, apagado): a mesma da dica, ou [`OCIOSO`]. O `&`
/// vira `&&`, senão o menu o come como atalho de teclado e sublinha a letra seguinte ("Bruno & Ana").
pub fn linha_do_menu(e: &EstadoDaTela) -> String {
    let linha = linha_de_estado(e);
    let linha = if linha.is_empty() { ocioso().to_string() } else { linha };
    cortar(&linha, TAMANHO_DA_LINHA_DO_MENU).replace('&', "&&")
}

/// O texto em no máximo `max` unidades UTF-16, com "…" no fim quando foi cortado. O corte é sempre
/// entre caracteres (um emoji no nome do aparelho é um par substituto, e metade dele seria lixo).
pub fn cortar(texto: &str, max: usize) -> String {
    if texto.encode_utf16().count() <= max {
        return texto.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut saida = String::new();
    let mut usado = 0;
    for c in texto.chars() {
        // O "…" é uma unidade só; ele tem de caber depois do último caractere.
        if usado + c.len_utf16() + 1 > max {
            break;
        }
        saida.push(c);
        usado += c.len_utf16();
    }
    saida.push('…');
    saida
}

// =============================================================================================
// O que o ícone manda
// =============================================================================================

/// Os códigos que chegam na palavra baixa do `lParam` da mensagem do ícone (`shellapi.h` e
/// `winuser.h`). Escritos aqui, e não importados do crate `windows`, para este módulo ficar sem Win32.
pub const NIN_SELECT: u32 = 0x0400;
/// `NIN_SELECT | NINF_KEY`: Enter ou Espaço no ícone com o foco do teclado.
pub const NIN_KEYSELECT: u32 = 0x0401;
pub const NIN_BALLOONSHOW: u32 = 0x0402;
pub const NIN_BALLOONUSERCLICK: u32 = 0x0405;
pub const WM_CONTEXTMENU: u32 = 0x007B;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
pub const WM_RBUTTONUP: u32 = 0x0205;

/// O que fazer com um aviso do ícone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evento {
    /// Clique, duplo clique, Enter ou Espaço no ícone, ou clique no aviso: a janela volta.
    Abrir,
    /// O menu, ancorado neste ponto da tela (pixels físicos: o processo é ciente de DPI por monitor).
    Menu { x: i32, y: i32 },
    /// O menu, onde o cursor estiver: o formato antigo (sem a versão 4) não diz o ponto.
    MenuNoCursor,
    /// O aviso da primeira vez apareceu (vai para o registro: é a prova de que ele saiu).
    AvisoMostrado,
    Nada,
}

/// **Lê um aviso do ícone.** Na versão 4 (`NOTIFYICON_VERSION_4`) a palavra baixa do `lParam` é o
/// evento e o `wParam` traz o ponto da âncora (x na palavra baixa, y na alta, com sinal: um monitor
/// à esquerda do principal tem x negativo). Sem a versão 4 (o `NIM_SETVERSION` recusado), o `lParam`
/// é a mensagem de mouse, e o clique esquerdo e o direito chegam como `WM_LBUTTONUP` e
/// `WM_RBUTTONUP`. Na versão 4 o clique esquerdo chega também como `NIN_SELECT`, e só ele abre (os
/// dois juntos abririam duas vezes; abrir é idempotente, mas o registro diria duas).
pub fn evento(wp: usize, lp: isize, versao_4: bool) -> Evento {
    let codigo = (lp as usize & 0xFFFF) as u32;
    match codigo {
        NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONDBLCLK | NIN_BALLOONUSERCLICK => Evento::Abrir,
        WM_LBUTTONUP if !versao_4 => Evento::Abrir,
        WM_CONTEXTMENU if versao_4 => {
            Evento::Menu { x: (wp & 0xFFFF) as u16 as i16 as i32, y: ((wp >> 16) & 0xFFFF) as u16 as i16 as i32 }
        }
        WM_RBUTTONUP if !versao_4 => Evento::MenuNoCursor,
        NIN_BALLOONSHOW => Evento::AvisoMostrado,
        _ => Evento::Nada,
    }
}

// =============================================================================================
// Minimizar
// =============================================================================================

/// O que minimizar faz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AoMinimizar {
    /// A janela se esconde (sai da barra de tarefas) e fica só o ícone; `avisar` na primeira vez.
    Esconder { avisar: bool },
    /// O minimizar de sempre, para a barra de tarefas.
    Minimizar,
}

/// **Minimizar vai para a bandeja só com o ícone posto.** Sem ele (a Sessão 0 do SSH, o Explorer
/// fora do ar, o `Shell_NotifyIconW` recusado) esconder a janela seria fazê-la sumir sem caminho de
/// volta, com as sessões vivas e nada na tela que as mostre.
pub fn ao_minimizar(icone_na_bandeja: bool, ja_avisou: bool) -> AoMinimizar {
    if icone_na_bandeja {
        AoMinimizar::Esconder { avisar: !ja_avisou }
    } else {
        AoMinimizar::Minimizar
    }
}

// =============================================================================================
// A instância única
// =============================================================================================

/// **A abertura de produto**: o `quall-app.exe` aberto **sem nenhum argumento** além do nome do
/// programa (recebe os argumentos já sem ele, `std::env::args_os().skip(1)`). É assim que a pessoa o
/// abre: o atalho do menu Iniciar (`scripts/instalador/Quall.wxs`, o `<Shortcut>` sem `Arguments`), o
/// duplo clique no exe, o fixado na barra de tarefas.
///
/// Só ela leva a instância única (`instancia.rs`). Toda bandeira de `argumentos.rs` é de bancada, e
/// os roteiros e as sondas sempre passam pelo menos uma (`--registro`, `--dados`, `--espelhar-ja`,
/// `--exibir-ja`…) — e rodam dois processos na mesma máquina, emissor e receptor, que a instância
/// única proibiria. Por isso a regra é "nenhum argumento", e não "nenhuma bandeira de uma lista": uma
/// bandeira nova de bancada fica de fora sem ninguém lembrar de pô-la numa lista.
pub fn abertura_de_produto<I: IntoIterator>(argumentos_depois_do_programa: I) -> bool {
    argumentos_depois_do_programa.into_iter().next().is_none()
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::modelo_da_janela::exemplos;

    fn exemplo(nome: &str) -> EstadoDaTela {
        exemplos().into_iter().find(|(n, _)| *n == nome).unwrap_or_else(|| panic!("exemplo {nome}")).1
    }

    /// O mesmo estado, com o ladrilho da tela estendida (o segundo dos exemplos) escolhido.
    #[cfg(feature = "tela-estendida-futura")]
    fn com_tela_estendida(mut e: EstadoDaTela) -> EstadoDaTela {
        for f in &mut e.espelhar.fontes {
            f.escolhido = f.icone == Icone::TelaEstendida;
        }
        assert!(e.espelhar.fontes.iter().any(|f| f.escolhido), "os exemplos têm o ladrilho da tela estendida");
        e
    }

    #[test]
    fn a_linha_de_cada_cena() {
        assert_eq!(linha_de_estado(&exemplo("01-espelhar")), "");
        assert_eq!(linha_de_estado(&exemplo("07-ajustes")), "");
        assert_eq!(linha_de_estado(&exemplo("08-esperando")), "Esperando um aparelho");
        assert_eq!(linha_de_estado(&exemplo("11-no-ar")), "Espelhando para iPad do Bruno");
        assert_eq!(linha_de_estado(&exemplo("15-conectando")), "Conectando a iPhone do Bruno (192.168.15.4:7877)");
        assert_eq!(linha_de_estado(&exemplo("16-exibindo")), "Exibindo iPhone do Bruno");
        assert_eq!(linha_de_estado(&exemplo("13-varios")), "Espelhando para 3 aparelhos");
        assert_eq!(linha_de_estado(&exemplo("14-varios-no-limite")), "Espelhando para 8 aparelhos");

        let mut camera = exemplo("12-no-ar-camera-gravando");
        camera.espelhar.camera = true;
        assert_eq!(linha_de_estado(&camera), "Câmera no ar para iPad do Bruno");

        let mut encerrando = exemplo("08-esperando");
        encerrando.espera.aviso = ENCERRANDO.into();
        encerrando.espera.encerrando = true;
        assert_eq!(linha_de_estado(&encerrando), "Encerrando…");
        // O estado, e não o texto: o aviso em inglês ("Ending…") continua sendo o encerrando.
        let mut aviso_em_ingles = encerrando.clone();
        aviso_em_ingles.espera.aviso = "Ending…".into();
        assert_eq!(linha_de_estado(&aviso_em_ingles), "Encerrando…");
        let mut encerrando = exemplo("16-exibindo");
        encerrando.exibindo.encerrando = true;
        assert_eq!(linha_de_estado(&encerrando), "Encerrando…");

        let mut sem_par = exemplo("16-exibindo");
        sem_par.exibindo.par = "  ".into();
        assert_eq!(linha_de_estado(&sem_par), "Exibindo");
        // O PIN não sai da janela: nem na dica, nem no menu (a tela compartilhada numa chamada).
        let espera = exemplo("08-esperando");
        assert!(!espera.espera.pin.is_empty());
        for texto in [dica(&espera), linha_do_menu(&espera)] {
            assert!(!texto.contains(&espera.espera.pin) && !texto.contains("482 719") && !texto.contains("PIN"), "{texto}");
        }
    }

    #[cfg(feature = "tela-estendida-futura")]
    #[test]
    fn a_tela_estendida_conta_aparelhos() {
        let mut e = com_tela_estendida(exemplo("13-varios"));
        e.varios.receptores.truncate(2);
        assert_eq!(linha_de_estado(&e), "Tela estendida: 2 aparelhos");
        e.varios.receptores.truncate(1);
        assert_eq!(linha_de_estado(&e), "Tela estendida: 1 aparelho");
        e.varios.receptores.clear();
        assert_eq!(linha_de_estado(&e), "Tela estendida: esperando um aparelho");
        let mut so_monitor = exemplo("13-varios");
        so_monitor.varios.receptores.clear();
        assert_eq!(linha_de_estado(&so_monitor), "Esperando um aparelho");
    }

    #[test]
    fn a_dica_e_o_menu() {
        assert_eq!(dica(&exemplo("01-espelhar")), "Quall Monitor");
        assert_eq!(dica(&exemplo("11-no-ar")), "Quall Monitor\nEspelhando para iPad do Bruno");
        assert_eq!(linha_do_menu(&exemplo("06-teleprompter")), OCIOSO);
        assert_eq!(linha_do_menu(&exemplo("01-espelhar")), "Pronto");
        let mut e = exemplo("16-exibindo");
        e.exibindo.par = "iPad da Ana & do Bruno".into();
        assert_eq!(linha_do_menu(&e), "Exibindo iPad da Ana && do Bruno");
        // A dica não passa de `szTip`, nem com um nome enorme cheio de emoji.
        e.exibindo.par = "📱".repeat(200);
        let d = dica(&e);
        assert!(d.encode_utf16().count() < TAMANHO_DA_DICA, "{}", d.encode_utf16().count());
        assert!(d.ends_with('…'));
        assert!(linha_do_menu(&e).encode_utf16().count() <= TAMANHO_DA_LINHA_DO_MENU);
        // E nenhuma sessão que a janela conhece deixa a dica sem o "Quall" na frente.
        for (nome, e) in exemplos() {
            assert!(dica(&e).starts_with("Quall"), "{nome}");
            let tem_sessao = !matches!(e.cena, Cena::Painel(_));
            assert_eq!(dica(&e).contains('\n'), tem_sessao, "{nome}");
        }
    }

    #[test]
    fn o_teleprompter_aberto_aparece() {
        let mut e = exemplo("06-teleprompter");
        e.teleprompter = Some(TeleprompterAberto::Prompter);
        assert_eq!(linha_de_estado(&e), "Teleprompter: mostrando o texto");
        assert_eq!(dica(&e), "Quall Monitor\nTeleprompter: mostrando o texto");
        assert_eq!(linha_do_menu(&e), "Teleprompter: mostrando o texto");
        e.teleprompter = Some(TeleprompterAberto::Escolha);
        assert_eq!(linha_de_estado(&e), "Teleprompter aberto");
        e.teleprompter = Some(TeleprompterAberto::Controle);
        assert_eq!(linha_de_estado(&e), "Teleprompter: controlando");
        e.teleprompter = Some(TeleprompterAberto::PrompterComCamera);
        assert_eq!(linha_de_estado(&e), "Teleprompter: texto com a câmera");
        // Com uma sessão de pé, as duas coisas.
        let mut no_ar = exemplo("11-no-ar");
        no_ar.teleprompter = Some(TeleprompterAberto::Controle);
        assert_eq!(linha_de_estado(&no_ar), "Espelhando para iPad do Bruno · Teleprompter: controlando");
        no_ar.teleprompter = None;
        assert_eq!(linha_de_estado(&no_ar), "Espelhando para iPad do Bruno");
    }

    #[test]
    fn o_corte_nunca_parte_um_caractere() {
        assert_eq!(cortar("Quall", 10), "Quall");
        assert_eq!(cortar("Quall", 5), "Quall");
        assert_eq!(cortar("Quall", 4), "Qua…");
        assert_eq!(cortar("Quall", 0), "");
        // "😀" são duas unidades: com 4 de limite cabem um emoji e o "…" (3), e não um emoji e meio.
        assert_eq!(cortar("😀😀😀", 4), "😀…");
        assert_eq!(cortar("😀😀😀", 3), "😀…");
        assert_eq!(cortar("😀😀😀", 2), "…");
        assert_eq!(cortar("ação", 3), "aç…");
    }

    #[test]
    fn os_textos_do_aviso_cabem() {
        assert!(TITULO_DO_AVISO.encode_utf16().count() < TAMANHO_DO_TITULO_DO_AVISO);
        assert!(TEXTO_DO_AVISO.encode_utf16().count() < TAMANHO_DO_AVISO);
        let texto_em_ingles = crate::idioma::com_idioma(Idioma::En, || t(TEXTO_DO_AVISO));
        assert_ne!(texto_em_ingles, TEXTO_DO_AVISO, "o aviso tem inglês na tabela");
        assert!(texto_em_ingles.encode_utf16().count() < TAMANHO_DO_AVISO);
    }

    #[test]
    fn em_ingles() {
        use crate::idioma::com_idioma;
        com_idioma(Idioma::En, || {
            assert_eq!(linha_de_estado(&exemplo("08-esperando")), "Waiting for a device");
            assert_eq!(linha_de_estado(&exemplo("11-no-ar")), "Mirroring to iPad do Bruno");
            assert_eq!(linha_de_estado(&exemplo("13-varios")), "Mirroring to 3 devices");
            assert_eq!(linha_de_estado(&exemplo("16-exibindo")), "Receiving iPhone do Bruno");
            assert_eq!(linha_do_menu(&exemplo("01-espelhar")), "Ready", "o estado ocioso, e não o \"Done\" do botão");
            let mut e = exemplo("08-esperando");
            e.espera.encerrando = true;
            assert_eq!(linha_de_estado(&e), "Ending…");
            #[cfg(feature = "tela-estendida-futura")]
            {
            let mut e = com_tela_estendida(exemplo("13-varios"));
            e.varios.receptores.truncate(2);
            assert_eq!(linha_de_estado(&e), "Extended display: 2 devices");
            }
            let mut e = exemplo("06-teleprompter");
            e.teleprompter = Some(TeleprompterAberto::Controle);
            assert_eq!(dica(&e), "Quall Monitor\nTeleprompter: remote");
        });
        // E de volta ao português fora do `com_idioma`.
        assert_eq!(linha_do_menu(&exemplo("01-espelhar")), "Pronto");
    }

    #[test]
    fn os_avisos_do_icone() {
        // Versão 4: o evento na palavra baixa do lParam (o id do ícone na alta), o ponto no wParam.
        let lp = |codigo: u32| ((1u32 << 16) | codigo) as isize;
        assert_eq!(evento(0, lp(NIN_SELECT), true), Evento::Abrir);
        assert_eq!(evento(0, lp(NIN_KEYSELECT), true), Evento::Abrir);
        assert_eq!(evento(0, lp(WM_LBUTTONDBLCLK), true), Evento::Abrir);
        assert_eq!(evento(0, lp(NIN_BALLOONUSERCLICK), true), Evento::Abrir);
        assert_eq!(evento(0, lp(WM_LBUTTONUP), true), Evento::Nada, "na versão 4 quem abre é o NIN_SELECT");
        assert_eq!(evento(0, lp(WM_RBUTTONUP), true), Evento::Nada, "na versão 4 o menu vem no WM_CONTEXTMENU");
        assert_eq!(evento(0, lp(NIN_BALLOONSHOW), true), Evento::AvisoMostrado);
        assert_eq!(evento(0, lp(0x0200), true), Evento::Nada, "WM_MOUSEMOVE");
        let ponto = |x: i16, y: i16| ((y as u16 as usize) << 16) | x as u16 as usize;
        assert_eq!(evento(ponto(1500, 1040), lp(WM_CONTEXTMENU), true), Evento::Menu { x: 1500, y: 1040 });
        assert_eq!(evento(ponto(-300, 700), lp(WM_CONTEXTMENU), true), Evento::Menu { x: -300, y: 700 }, "monitor à esquerda");
        // Sem a versão 4: o lParam é a mensagem de mouse.
        assert_eq!(evento(1, WM_LBUTTONUP as isize, false), Evento::Abrir);
        assert_eq!(evento(1, WM_RBUTTONUP as isize, false), Evento::MenuNoCursor);
        assert_eq!(evento(1, WM_CONTEXTMENU as isize, false), Evento::Nada);
    }

    #[test]
    fn so_a_abertura_sem_argumentos_e_de_produto() {
        assert!(abertura_de_produto(Vec::<String>::new()), "o atalho do menu Iniciar não passa argumento");
        assert!(abertura_de_produto(std::iter::empty::<std::ffi::OsString>()));
        assert!(!abertura_de_produto(["--espelhar-ja"]));
        assert!(!abertura_de_produto(["--registro", "C:\\Users\\bruno\\quall-app.log"]));
        assert!(!abertura_de_produto(["--dados", "C:\\bancada", "--exibir-ja", "127.0.0.1:7877"]));
        // Os argumentos chegam sem o nome do programa: quem esquecesse o `skip(1)` veria tudo como
        // bancada, e a instância única nunca valeria. O `main` passa `args_os().skip(1)`.
        assert!(!abertura_de_produto(["C:\\Program Files\\Quall\\quall-app.exe"]));
        assert!(abertura_de_produto(["C:\\Program Files\\Quall\\quall-app.exe"].into_iter().skip(1)));
    }

    #[test]
    fn minimizar_so_esconde_com_o_icone_posto() {
        assert_eq!(ao_minimizar(true, false), AoMinimizar::Esconder { avisar: true });
        assert_eq!(ao_minimizar(true, true), AoMinimizar::Esconder { avisar: false });
        assert_eq!(ao_minimizar(false, false), AoMinimizar::Minimizar);
        assert_eq!(ao_minimizar(false, true), AoMinimizar::Minimizar);
    }
}
