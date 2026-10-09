//! **O driver da tela estendida que o Quall instala** (decisão do Bruno, 02/10/2026, noite;
//! `docs/monitor-virtual-windows.md` §15): as regras puras, sem Win32, testadas em qualquer máquina.
//!
//! O Quall leva o SudoVDA 1.10.9.289 dentro do `quall-app.exe` (`terceiros/sudovda/`) e o instala
//! **só** quando a pessoa clica em "Instalar o driver da tela estendida" e aceita a caixa que explica
//! o que vai acontecer. A instalação roda num processo elevado à parte (o próprio exe, relançado pelo
//! UAC); a parte Win32 está em `driver_da_tela_estendida.rs`. Aqui ficam:
//!
//! - os **hashes** dos quatro arquivos e a conferência ([`conferir_os_arquivos`], com um SHA-256 nosso,
//!   [`sha256`], para o teste rodar em qualquer máquina e o processo elevado não depender de outro
//!   código para isso);
//! - o **pedido** que o processo elevado aceita ([`ler_pedido`]): exatamente um verbo de uma lista
//!   fechada e um nome de cano no formato que a janela gera — nenhum caminho, nenhum texto livre;
//! - o **protocolo** das linhas que o processo elevado manda à janela pelo cano ([`Linha`]);
//! - a **situação** do driver neste computador e o que a janela oferece em cada uma ([`Situacao`]),
//!   e **quem pode desinstalar** ([`pode_desinstalar`]): só o que o Quall instalou;
//! - os **textos** da caixa, do andamento e do resultado (chaves da tabela de `idioma`).

// =============================================================================================
// O que vai junto
// =============================================================================================

/// O hardware ID do adaptador (o mesmo de `sudovda::HARDWARE_ID`; o `.inf`, linha `Root\SudoMaker\SudoVDA`).
pub const HARDWARE_ID: &str = r"root\sudomaker\sudovda";
/// A classe de dispositivo Display (`ClassGUID` do `.inf`), `GUID_DEVCLASS_DISPLAY`.
pub const CLASSE_DISPLAY: u128 = 0x4d36e968_e325_11ce_bfc1_08002be10318;
/// O nome original do pacote no repositório de drivers: é por ele, e nunca por um `oemNN.inf` às
/// cegas, que a desinstalação reconhece o pacote (§9: a mesma lista traz NVIDIA, Intel e spacedesk).
pub const NOME_ORIGINAL_DO_INF: &str = "sudovda.inf";
/// O provedor do pacote (`Provider=%ManufacturerName%` = "SudoMaker").
pub const PROVEDOR: &str = "SudoMaker"; // i18n: fora
/// O nome do adaptador no Gerenciador de Dispositivos (a testemunha).
pub const NOME_DO_ADAPTADOR: &str = "SudoMaker Virtual Display Adapter"; // i18n: fora
/// A impressão SHA-1 do certificado `CN=sudovda@su.mk` (§4.2, §9): é por ela que o certificado é
/// achado nas lojas para sair.
pub const IMPRESSAO_SHA1: &str = "3C918FC73525AD8B1521B6DB26B71F694277CC49";
/// O assunto do certificado, para a caixa.
pub const ASSUNTO_DO_CERTIFICADO: &str = "CN=sudovda@su.mk";
/// A versão que vai junto.
pub const VERSAO: &str = "1.10.9.289";

/// Um arquivo embutido: o nome com que é escrito na pasta de trabalho, o tamanho e o SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arquivo {
    pub nome: &'static str,
    pub bytes: usize,
    pub sha256: &'static str,
}

/// **Os quatro arquivos, na ordem em que são escritos.** Os valores são os de
/// `terceiros/sudovda/LEIAME.md`: o `.cer` e o `.inf` batem com o §9; o `.cat` e o `.dll` são os
/// que o PnP aceitou no Dell em 14/09 (idênticos às cópias do DriverStore, Authenticode válido por
/// `CN=sudovda@su.mk`).
pub const ARQUIVOS: [Arquivo; 4] = [
    Arquivo { nome: "SudoVDA.inf", bytes: 3644, sha256: "AD69AC682756F0CF339B081FAC7E6E8159FDF2CA01CA69DF8945C7246C286925" }, // i18n: fora
    Arquivo { nome: "SudoVDA.cat", bytes: 2425, sha256: "2F9189DE5604BEC9D86F51640CC540639E394D9AD0F8E689129375E95F2D22F8" }, // i18n: fora
    Arquivo { nome: "SudoVDA.dll", bytes: 83216, sha256: "47EE263CB5DE9382C6630A2D7F3DAFEC4A49419F953BEEC869CA5DD0C460FF63" }, // i18n: fora
    Arquivo { nome: "SudoVDA.cer", bytes: 772, sha256: "6ACCDCD519F6179D967DB4EAA20ECF25A732BA30E87F4CFFEBC768B2C13C9007" }, // i18n: fora
];

/// **Confere** cada `(nome, conteúdo)` contra [`ARQUIVOS`]: o nome tem de ser um dos quatro, o
/// tamanho e o SHA-256 têm de bater, e os quatro têm de estar lá. O erro diz qual e o que veio.
pub fn conferir_os_arquivos(dados: &[(&str, &[u8])]) -> Result<(), String> {
    for a in ARQUIVOS {
        let Some((_, conteudo)) = dados.iter().find(|(n, _)| *n == a.nome) else {
            return Err(format!("{}: -", a.nome)); // i18n: fora (detalhe técnico)
        };
        if conteudo.len() != a.bytes {
            return Err(format!("{}: {} B != {} B", a.nome, conteudo.len(), a.bytes)); // i18n: fora (detalhe técnico)
        }
        let h = hex(&sha256(conteudo));
        if !h.eq_ignore_ascii_case(a.sha256) {
            return Err(format!("{}: SHA-256 {h} != {}", a.nome, a.sha256)); // i18n: fora (detalhe técnico)
        }
    }
    if let Some((n, _)) = dados.iter().find(|(n, _)| !ARQUIVOS.iter().any(|a| a.nome == *n)) {
        return Err(format!("{n}: ?")); // i18n: fora (detalhe técnico)
    }
    Ok(())
}

/// Hexadecimal em maiúsculas (o formato do `Get-FileHash` e do §9).
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// **SHA-256** (FIPS 180-4), sem dependência: são quatro arquivos pequenos, uma vez por instalação.
pub fn sha256(dados: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
        0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
        0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = dados.to_vec();
    let bits = (dados.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for bloco in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, p) in bloco.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut saida = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        saida[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    saida
}

/// O pacote do repositório de drivers é o do SudoVDA? Pelo **nome original** do `.inf` e pelo
/// provedor, os dois — o `oemNN.inf` sozinho não diz nada (§9).
pub fn pacote_e_do_sudovda(nome_original: &str, provedor: &str) -> bool {
    nome_original.trim().eq_ignore_ascii_case(NOME_ORIGINAL_DO_INF) && provedor.trim() == PROVEDOR
}

/// O nome publicado no repositório de drivers tem a cara de um `oemNN.inf` (só isso é aceito como
/// argumento da remoção: nada de caminho).
pub fn nome_publicado_valido(s: &str) -> bool {
    let b = s.as_bytes();
    let l = s.to_ascii_lowercase();
    l.starts_with("oem") && l.ends_with(".inf") && b.len() > 7 && b.len() <= 12 && b[3..b.len() - 4].iter().all(u8::is_ascii_digit)
}

/// O hardware ID de um nó é o do SudoVDA? (A lista do PnP é um MULTI_SZ; qualquer um deles serve.)
pub fn hardware_ids_do_sudovda(ids: &[String]) -> bool {
    ids.iter().any(|i| i.eq_ignore_ascii_case(HARDWARE_ID))
}

/// Um monitor que o SudoVDA criou (`DISPLAY\SMKD1CE\…`): os fantasmas que saem com ele.
pub fn monitor_do_sudovda(instancia: &str) -> bool {
    instancia.to_ascii_uppercase().starts_with(r"DISPLAY\SMKD1CE\")
}

// =============================================================================================
// O pedido ao processo elevado
// =============================================================================================

/// O primeiro argumento do processo elevado. É ele que o `main` procura **antes de tudo** (antes da
/// instância única, do COM, do registro).
pub const ARGUMENTO: &str = "--driver-tela-estendida";
/// O argumento do cano.
pub const ARGUMENTO_DO_CANO: &str = "--cano";
/// O modo do desinstalador do MSI (ação adiada, como SYSTEM): sem cano, o resultado só pelo código de
/// saída. Só vale com `desinstalar`.
pub const ARGUMENTO_SEM_CANO: &str = "--sem-cano";
/// O prefixo do nome do cano; depois dele, 32 dígitos hexadecimais minúsculos.
pub const PREFIXO_DO_CANO: &str = r"\\.\pipe\quall-driver-";

/// O que o processo elevado faz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Acao {
    #[default]
    Instalar,
    Desinstalar,
}

impl Acao {
    pub fn verbo(self) -> &'static str {
        match self {
            Acao::Instalar => "instalar",
            Acao::Desinstalar => "desinstalar",
        }
    }
}

/// O pedido lido da linha de comando do processo elevado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pedido {
    pub acao: Acao,
    pub confiar_certificado: bool,
    /// `None`: o desinstalador do MSI (`--sem-cano`).
    pub cano: Option<String>,
}

/// O nome do cano, a partir de 128 bits aleatórios.
pub fn nome_do_cano(aleatorio: u128) -> String {
    format!("{PREFIXO_DO_CANO}{aleatorio:032x}")
}

/// O nome do cano tem exatamente o formato que [`nome_do_cano`] gera?
pub fn cano_valido(s: &str) -> bool {
    s.strip_prefix(PREFIXO_DO_CANO)
        .is_some_and(|r| r.len() == 32 && r.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}

/// Os argumentos são um pedido ao processo elevado? `None`: não são (o app segue como sempre).
/// `Some(Err)`: começam com [`ARGUMENTO`] mas não têm o formato exato — o processo **recusa** e sai,
/// sem tocar em nada. Só dois formatos são aceitos (os argumentos **depois** do `argv[0]`):
///
/// - `--driver-tela-estendida <instalar|desinstalar> --cano <nome>` (a janela);
/// - `--driver-tela-estendida desinstalar --sem-cano` (o desinstalador do MSI).
pub fn ler_pedido(args: &[String]) -> Option<Result<Pedido, String>> {
    if args.first().map(String::as_str) != Some(ARGUMENTO) {
        return None;
    }
    Some((|| {
        if (args.len() == 3 || (args.len() == 4 && args[1] == "instalar" && args[3] == "--confiar-certificado")) && matches!(args[1].as_str(), "instalar" | "desinstalar") && args[2] == ARGUMENTO_SEM_CANO {
            return Ok(Pedido { acao: if args[1] == "instalar" { Acao::Instalar } else { Acao::Desinstalar }, confiar_certificado: args.len() == 4, cano: None });
        }
        if args.len() != 4 {
            return Err(format!("{} argumentos; o pedido tem exatamente 4", args.len())); // i18n: fora
        }
        let acao = match args[1].as_str() {
            "instalar" => Acao::Instalar,
            "desinstalar" => Acao::Desinstalar,
            outro => return Err(format!("verbo desconhecido: {outro:?}")),
        };
        if args[2] != ARGUMENTO_DO_CANO {
            return Err(format!("o terceiro argumento é {:?}, e não {ARGUMENTO_DO_CANO}", args[2])); // i18n: fora
        }
        if !cano_valido(&args[3]) {
            return Err("o nome do cano não tem o formato do Quall".into()); // i18n: fora
        }
        Ok(Pedido { acao, confiar_certificado: true, cano: Some(args[3].clone()) })
    })())
}

/// Os parâmetros que a janela passa ao `ShellExecuteExW` (sem o exe).
pub fn parametros(acao: Acao, cano: &str) -> String {
    format!("{ARGUMENTO} {} {ARGUMENTO_DO_CANO} {cano}", acao.verbo())
}

// =============================================================================================
// O protocolo do cano
// =============================================================================================

/// Os passos da instalação, na ordem (o número vai pelo cano; o nome, a janela traduz).
pub const PASSOS_DA_INSTALACAO: [&str; 7] = [
    "Conferindo os arquivos do driver", // i18n: chave
    "Preparando a pasta de trabalho",   // i18n: chave
    "Confiando no certificado do SudoVDA", // i18n: chave
    "Conferindo a assinatura do catálogo", // i18n: chave
    "Criando o adaptador",              // i18n: chave
    "Instalando o driver",              // i18n: chave
    "Conferindo o adaptador",           // i18n: chave
];

/// Os passos da desinstalação.
pub const PASSOS_DA_DESINSTALACAO: [&str; 4] = [
    "Tirando o adaptador",               // i18n: chave
    "Tirando o pacote do driver",        // i18n: chave
    "Tirando o certificado do SudoVDA",  // i18n: chave
    "Conferindo que saiu",               // i18n: chave
];

pub fn passos(acao: Acao) -> &'static [&'static str] {
    match acao {
        Acao::Instalar => &PASSOS_DA_INSTALACAO,
        Acao::Desinstalar => &PASSOS_DA_DESINSTALACAO,
    }
}

/// Uma linha do processo elevado para a janela. Texto de uma linha, UTF-8, terminada em `\n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Linha {
    /// `passo N`: começou o passo N (de 1).
    Passo(usize),
    /// `diario …`: uma linha para o diário da janela (não aparece na tela).
    Diario(String),
    /// `ok`: terminou bem.
    Ok,
    /// `ok-reiniciar`: terminou bem, e o Windows pede reinício.
    OkReiniciar,
    /// `falha N chave<TAB>técnico`: parou no passo N (0: antes de qualquer passo). A chave é um dos
    /// `MOTIVO_*` daqui (a janela a traduz); o técnico (a API e o código) vai como veio.
    Falha(usize, String, String),
}

impl Linha {
    pub fn escrever(&self) -> String {
        let s = match self {
            Linha::Passo(n) => format!("passo {n}"),
            Linha::Diario(t) => format!("diario {}", uma_linha(t)),
            Linha::Ok => "ok".into(),
            Linha::OkReiniciar => "ok-reiniciar".into(),
            Linha::Falha(n, c, t) => format!("falha {n} {}\t{}", uma_linha(c), uma_linha(t)),
        };
        s + "\n"
    }

    /// Lê uma linha. O que não for do protocolo vira `None` (e a janela só anota no diário).
    pub fn ler(s: &str) -> Option<Linha> {
        let s = s.trim_end_matches(['\r', '\n']);
        let (cabeca, resto) = s.split_once(' ').unwrap_or((s, ""));
        match cabeca {
            "passo" => resto.trim().parse().ok().filter(|n| *n >= 1).map(Linha::Passo),
            "diario" => Some(Linha::Diario(resto.to_string())),
            "ok" if resto.is_empty() => Some(Linha::Ok),
            "ok-reiniciar" if resto.is_empty() => Some(Linha::OkReiniciar),
            "falha" => {
                let (n, t) = resto.split_once(' ').unwrap_or((resto, ""));
                let (chave, tecnico) = t.split_once('\t').unwrap_or((t, ""));
                n.parse().ok().map(|n| Linha::Falha(n, chave.to_string(), tecnico.to_string()))
            }
            _ => None,
        }
    }
}

fn uma_linha(t: &str) -> String {
    t.replace(['\r', '\n', '\t'], " ")
}

/// Os códigos de saída do processo elevado. Valem quando o cano não trouxe o fim (o processo morreu
/// antes de conectar, ou o cano caiu).
pub mod saida {
    pub const OK: u32 = 0;
    pub const FALHA: u32 = 1;
    /// O pedido não tinha o formato: nada foi tocado.
    pub const RECUSADO: u32 = 2;
    /// Não é administrador (o UAC foi contornado de algum jeito): nada foi tocado.
    pub const SEM_ADMIN: u32 = 3;
    /// O Windows pede reinício (o `ERROR_SUCCESS_REBOOT_REQUIRED` de sempre).
    pub const OK_REINICIAR: u32 = 3010;
}

// =============================================================================================
// A situação e o andamento, na janela
// =============================================================================================

/// O driver neste computador, do ponto de vista do Quall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Situacao {
    /// O app veio da Microsoft Store (tem identidade de pacote, ou a feature `loja`): o app não
    /// instala drivers (Store Policies 10.1.5 e 10.2.4). O ladrilho apagado manda **baixar o
    /// instalador avulso** (`quall-driver.exe`, decisão de 02/10, noite), e os Ajustes dizem o estado
    /// e mandam desinstalar por ele. `presente`: há um nó do SudoVDA presente (ligado ou não).
    Loja { presente: bool },
    /// O Windows não é x64 (num ARM64 o Quall roda emulado, o `.dll` amd64 não carrega e o PnP exige
    /// WHQL): o texto e o link de antes, e o motivo nos Ajustes.
    SemSuporte,
    /// O adaptador não está presente e nada indica que o Quall o pôs: o botão de instalar.
    #[default]
    Ausente,
    /// O Quall instalou (a marca no HKLM) — presente ou não (desligado no Gerenciador): o botão de
    /// desinstalar nos Ajustes.
    DoQuall { presente: bool },
    /// Presente, sem a marca: instalado à mão ou por outro programa (o Apollo, o Dell da bancada). O
    /// Quall o usa e não o desinstala.
    DeFora,
    /// Um nó do SudoVDA presente sem a interface de controle (desligado no Gerenciador, ou com erro),
    /// e sem a marca: instalar de novo duplicaria o nó.
    Desligado,
}

/// O que se lê do computador para decidir a [`Situacao`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Leitura {
    /// Identidade de pacote (MSIX) ou a feature `loja`.
    pub na_loja: bool,
    /// A arquitetura nativa é x64.
    pub x64: bool,
    /// A interface de controle do SudoVDA está presente (o adaptador ligado e funcionando).
    pub interface: bool,
    /// Um nó com o hardware ID do SudoVDA está presente (ligado ou não).
    pub no_presente: bool,
    /// A marca do Quall no HKLM, válida (o nó dela é o que o Quall criou, ou não existe mais).
    pub marca: bool,
}

/// A situação, pela leitura.
pub fn situacao(l: Leitura) -> Situacao {
    if l.na_loja {
        Situacao::Loja { presente: l.interface || l.no_presente }
    } else if l.marca {
        Situacao::DoQuall { presente: l.interface }
    } else if l.interface {
        Situacao::DeFora
    } else if l.no_presente {
        Situacao::Desligado
    } else if !l.x64 {
        Situacao::SemSuporte
    } else {
        Situacao::Ausente
    }
}

/// **Quem pode desinstalar**: só o que o Quall instalou, e nunca na versão da loja.
pub fn pode_desinstalar(s: Situacao) -> bool {
    matches!(s, Situacao::DoQuall { .. })
}

/// O ladrilho apagado instala (e não abre a página do SudoVDA)? Só fora da loja e sem o adaptador.
/// Com o adaptador desligado no Gerenciador (com a marca ou sem), instalar de novo duplicaria o nó: o
/// ladrilho leva aos Ajustes, que dizem o que houve.
pub fn ladrilho_instala(s: Situacao) -> bool {
    s == Situacao::Ausente
}

/// O que o clique no ladrilho apagado faz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliqueNoApagado {
    /// A caixa de instalar.
    Instalar,
    /// O painel Ajustes, onde a situação está dita.
    Ajustes,
    /// A página do instalador avulso no navegador (`regras_da_tela_estendida::URL_DO_INSTALADOR_DO_DRIVER`):
    /// a loja e o Windows sem suporte.
    Pagina,
}

pub fn clique_no_apagado(s: Situacao) -> CliqueNoApagado {
    match s {
        Situacao::Ausente => CliqueNoApagado::Instalar,
        Situacao::Desligado | Situacao::DoQuall { presente: false } | Situacao::Loja { presente: true } => CliqueNoApagado::Ajustes,
        Situacao::Loja { presente: false } | Situacao::SemSuporte | Situacao::DeFora | Situacao::DoQuall { presente: true } => {
            CliqueNoApagado::Pagina
        }
    }
}

/// O detalhe do ladrilho apagado (chave), pela situação.
pub fn detalhe_do_apagado(s: Situacao) -> &'static str {
    match (clique_no_apagado(s), s) {
        (CliqueNoApagado::Instalar, _) => BOTAO_INSTALAR,
        (CliqueNoApagado::Ajustes, _) => DETALHE_DESLIGADO,
        (CliqueNoApagado::Pagina, Situacao::Loja { .. }) => DETALHE_BAIXAR,
        (CliqueNoApagado::Pagina, _) => DETALHE_PRECISA,
    }
}

/// O nome do ladrilho apagado para o Narrador (chave), pela situação.
pub fn texto_acessivel_do_apagado(s: Situacao) -> &'static str {
    match (clique_no_apagado(s), s) {
        (CliqueNoApagado::Instalar, _) => TEXTO_ACESSIVEL_INSTALAR,
        (CliqueNoApagado::Ajustes, _) => TEXTO_ACESSIVEL_DESLIGADO,
        (CliqueNoApagado::Pagina, Situacao::Loja { .. }) => TEXTO_ACESSIVEL_BAIXAR,
        (CliqueNoApagado::Pagina, _) => TEXTO_ACESSIVEL_PAGINA,
    }
}

/// O botão do cartão do driver nos Ajustes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotaoDosAjustes {
    /// "Desinstalar": o app eleva e desinstala (só o que o Quall instalou, fora da loja).
    Desinstalar,
    /// "Desinstalar pelo instalador do driver": abre a página do instalador avulso (a loja, com o
    /// SudoVDA presente). Nada é elevado dentro do app da loja.
    PaginaDoInstalador,
}

/// O botão do cartão, pela situação. `None`: sem botão.
pub fn botao_dos_ajustes(s: Situacao) -> Option<BotaoDosAjustes> {
    match s {
        Situacao::DoQuall { .. } => None,
        Situacao::Loja { presente: true } => Some(BotaoDosAjustes::PaginaDoInstalador),
        _ => None,
    }
}

/// A frase da situação no cartão dos Ajustes (chave).
pub fn frase_dos_ajustes(s: Situacao) -> &'static str {
    match s {
        Situacao::Loja { presente: true } => AJUSTES_LOJA_PRESENTE,
        Situacao::Loja { presente: false } => AJUSTES_LOJA,
        Situacao::SemSuporte => AJUSTES_SEM_SUPORTE,
        Situacao::Ausente => AJUSTES_AUSENTE,
        Situacao::DoQuall { presente: true } => AJUSTES_DO_QUALL,
        Situacao::DoQuall { presente: false } => AJUSTES_DO_QUALL_DESLIGADO,
        Situacao::DeFora => AJUSTES_DE_FORA,
        Situacao::Desligado => AJUSTES_DESLIGADO,
    }
}

// --- o instalador avulso (`quall-driver.exe`, 02/10, noite) ---

/// A frase da situação no instalador avulso (chave). Ele nunca está "na loja" (é um exe à parte);
/// a frase não fala de Ajustes nem de ladrilho.
pub fn frase_do_instalador(s: Situacao) -> &'static str {
    match s {
        Situacao::Ausente | Situacao::Loja { presente: false } => INSTALADOR_AUSENTE,
        Situacao::DoQuall { presente: true } => AJUSTES_DO_QUALL,
        Situacao::DoQuall { presente: false } => AJUSTES_DO_QUALL_DESLIGADO,
        Situacao::DeFora | Situacao::Loja { presente: true } => INSTALADOR_DE_FORA,
        Situacao::Desligado => AJUSTES_DESLIGADO,
        Situacao::SemSuporte => AJUSTES_SEM_SUPORTE,
    }
}

/// Os botões do instalador avulso: (Instalar, Desinstalar) habilitados? Fechar fica sempre, menos
/// no meio de uma instalação (fechar mataria o processo no meio).
pub fn botoes_do_instalador(s: Situacao, a: &Andamento) -> (bool, bool) {
    let livre = pode_comecar(a);
    (livre && s == Situacao::Ausente, livre && pode_desinstalar(s))
}

/// O parágrafo que vai **no começo do corpo** do instalador avulso, quando a situação pede mais que o
/// rodapé: o SudoVDA de outro programa, com os dois botões apagados (o achado do Bruno, 02/10, noite).
pub fn aviso_no_corpo_do_instalador(s: Situacao) -> Option<&'static str> {
    matches!(s, Situacao::DeFora | Situacao::Loja { presente: true }).then_some(INSTALADOR_DE_FORA)
}

/// Pode fechar o instalador avulso agora?
pub fn pode_fechar(a: &Andamento) -> bool {
    pode_comecar(a)
}

/// O que está acontecendo com o driver, para a tela.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Andamento {
    #[default]
    Parado,
    /// Esperando o UAC ou rodando: o passo de agora (0 antes do primeiro).
    Rodando { acao: Acao, passo: usize },
    Acabou { acao: Acao, resultado: Resultado },
}

/// Como terminou.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resultado {
    Ok,
    OkReiniciar,
    /// A pessoa disse "Não" no UAC (`ERROR_CANCELLED`).
    CanceladoNoUac,
    /// Parou no passo N (0: antes de qualquer passo): a chave do motivo e o detalhe técnico.
    Falha(usize, String, String),
}

/// O tom do aviso.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TomDoAviso {
    Info,
    Ambar,
    Vermelho,
}

/// O resultado pelo que chegou: a última linha de fim do cano, se veio; senão o código de saída.
pub fn resultado(fim_pelo_cano: Option<&Linha>, ultimo_passo: usize, codigo_de_saida: Option<u32>) -> Resultado {
    match fim_pelo_cano {
        Some(Linha::Ok) => return Resultado::Ok,
        Some(Linha::OkReiniciar) => return Resultado::OkReiniciar,
        Some(Linha::Falha(n, c, t)) => return Resultado::Falha(*n, c.clone(), t.clone()),
        _ => {}
    }
    let f = |n: usize, c: &str, t: String| Resultado::Falha(n, c.to_string(), t);
    match codigo_de_saida {
        Some(saida::RECUSADO) => f(0, MOTIVO_RECUSADO, "exit 2".into()), // i18n: fora
        Some(saida::SEM_ADMIN) => f(0, MOTIVO_SEM_ADMIN, "exit 3".into()), // i18n: fora
        Some(c) => f(ultimo_passo, MOTIVO_SEM_FIM, format!("exit {c}")), // i18n: fora
        None => f(ultimo_passo, MOTIVO_SEM_FIM, "exit ?".into()), // i18n: fora
    }
}

/// O nome do passo N (1…), em português (chave da tabela).
pub fn nome_do_passo(acao: Acao, n: usize) -> Option<&'static str> {
    n.checked_sub(1).and_then(|i| passos(acao).get(i).copied())
}

// --- os textos (chaves da tabela; quem mostra passa por `idioma::t`/`tf`) ---

/// O detalhe do ladrilho apagado quando ele instala: é o botão.
pub const BOTAO_INSTALAR: &str = "Instalar o driver da tela estendida"; // i18n: chave
/// O nome do ladrilho para o Narrador, quando ele instala.
pub const TEXTO_ACESSIVEL_INSTALAR: &str =
    "Tela estendida, precisa de um driver. Instalar o driver da tela estendida: abre uma caixa que explica antes de instalar"; // i18n: chave

/// O título da caixa de instalar.
pub const CAIXA_TITULO: &str = "Instalar o driver da tela estendida"; // i18n: chave
/// A frase grande da caixa.
pub const CAIXA_PERGUNTA: &str = "A tela estendida precisa de um driver de monitor virtual"; // i18n: chave
/// O corpo da caixa, em três parágrafos (juntados com uma linha em branco; sem `\n` dentro das
/// chaves, que a varredura de `idioma` não lê): o que é (`{}`: a versão), o UAC, e o certificado
/// (`{}`: o assunto).
pub const CAIXA_CORPO_O_QUE_E: &str = "Para o Windows ganhar um monitor novo para cada aparelho, o Quall instala o SudoVDA {}, um driver de monitor virtual gratuito feito pelo SudoMaker. Ele vem junto com o Quall; nada é baixado."; // i18n: chave
pub const CAIXA_CORPO_UAC: &str = "O Windows vai pedir permissão de administrador."; // i18n: chave
pub const CAIXA_CORPO_CERTIFICADO: &str = "O certificado autoemitido do autor do SudoVDA ({}) será adicionado às lojas Root e TrustedPublisher deste computador. Isso torna esse certificado confiável na máquina. O setup remove somente o que o Quall Monitor instalou quando você desinstalar o aplicativo."; // i18n: chave
pub const CAIXA_INSTALAR: &str = "Instalar"; // i18n: chave

/// A caixa de desinstalar (o corpo: o que sai, e o [`CAIXA_CORPO_UAC`]).
pub const CAIXA_DESINSTALAR_TITULO: &str = "Desinstalar o driver da tela estendida"; // i18n: chave
pub const CAIXA_DESINSTALAR_PERGUNTA: &str = "Tirar o driver da tela estendida deste computador?"; // i18n: chave
pub const CAIXA_DESINSTALAR_CORPO: &str = "O Quall tira o adaptador do SudoVDA, o pacote do driver e o certificado do autor, que ele mesmo pôs. A tela estendida fica apagada até você instalar de novo."; // i18n: chave
pub const CAIXA_DESINSTALAR: &str = "Desinstalar"; // i18n: chave

/// Os Ajustes: o nome do cartão e as frases de cada situação.
pub const AJUSTES_NOME: &str = "Driver da tela estendida"; // i18n: chave
pub const AJUSTES_DO_QUALL: &str = "Instalado pelo Quall Monitor (SudoVDA). Sai ao desinstalar o aplicativo."; // i18n: chave
pub const AJUSTES_DO_QUALL_DESLIGADO: &str = "Instalado pelo Quall, mas o adaptador está desligado no Gerenciador de Dispositivos."; // i18n: chave
/// O SudoVDA posto por outro programa (o Dell da bancada): o que significa e o que fazer (o Bruno, 02/10,
/// noite: com os dois botões apagados, "só funciona o Fechar" não explicava nada). O mesmo texto nos
/// Ajustes e no corpo do instalador avulso.
pub const AJUSTES_DE_FORA: &str = "Este computador já tem o SudoVDA, instalado por outro programa. A tela estendida do Quall já funciona com ele. Para instalar ou remover por aqui, remova antes o SudoVDA pelo programa que o instalou."; // i18n: chave
pub const AJUSTES_AUSENTE: &str = "Não instalado. Execute novamente o instalador do Quall Monitor ou clique em Tela estendida."; // i18n: chave
pub const AJUSTES_LOJA: &str = "Não instalado. Baixe o instalador do driver pelo ladrilho Tela estendida, em Espelhar."; // i18n: chave
pub const AJUSTES_LOJA_PRESENTE: &str = "Instalado (SudoVDA). Para tirar, use o instalador do driver da tela estendida."; // i18n: chave
pub const AJUSTES_SEM_SUPORTE: &str = "Este Windows não é x64: o driver da tela estendida não roda nele."; // i18n: chave
pub const AJUSTES_DESLIGADO: &str = "Há um adaptador SudoVDA desligado ou com erro no Gerenciador de Dispositivos. Ligue-o lá."; // i18n: chave
/// Os botões do cartão.
pub const AJUSTES_DESINSTALAR: &str = "Desinstalar"; // i18n: chave
pub const AJUSTES_PAGINA_DO_INSTALADOR: &str = "Desinstalar pelo instalador do driver"; // i18n: chave

/// O detalhe do ladrilho apagado quando ele não instala (o Windows sem suporte, a loja, e o do
/// adaptador desligado).
pub const DETALHE_PRECISA: &str = "Precisa do driver SudoVDA"; // i18n: chave
pub const DETALHE_BAIXAR: &str = "Baixe o driver da tela estendida"; // i18n: chave
pub const DETALHE_DESLIGADO: &str = "Adaptador desligado — veja Ajustes"; // i18n: chave
pub const TEXTO_ACESSIVEL_PAGINA: &str =
    "Tela estendida, precisa do driver SudoVDA. Abre no navegador a página do instalador do driver"; // i18n: chave
pub const TEXTO_ACESSIVEL_BAIXAR: &str =
    "Tela estendida, precisa de um driver. Baixe o driver da tela estendida: abre no navegador a página do instalador"; // i18n: chave

/// O instalador avulso (`quall-driver.exe`): o título da janela, o botão de fechar, e as frases que
/// não são as dos Ajustes.
pub const INSTALADOR_TITULO: &str = "Instalador do driver da tela estendida do Quall Monitor"; // i18n: chave
pub const INSTALADOR_FECHAR: &str = "Fechar"; // i18n: chave
pub const INSTALADOR_AUSENTE: &str = "O driver não está instalado neste computador."; // i18n: chave
pub const INSTALADOR_DE_FORA: &str = AJUSTES_DE_FORA;
pub const INSTALADOR_JA_PODE: &str = "Pode fechar: o Quall já vê a tela estendida, mesmo aberto."; // i18n: chave
pub const TEXTO_ACESSIVEL_DESLIGADO: &str = "Tela estendida, o adaptador está desligado. Abre os Ajustes"; // i18n: chave

/// Os motivos de falha (o processo elevado manda a chave; a janela traduz).
pub const MOTIVO_ARQUIVOS: &str = "os arquivos do driver dentro do Quall não conferem"; // i18n: chave
pub const MOTIVO_PLATAFORMA: &str = "este Windows não é x64"; // i18n: chave
pub const MOTIVO_JA_EXISTE: &str = "já existe um adaptador SudoVDA neste computador"; // i18n: chave
pub const MOTIVO_OUTRA: &str = "outra instalação do driver está em andamento"; // i18n: chave
pub const MOTIVO_PASTA: &str = "não deu para preparar a pasta de trabalho"; // i18n: chave
pub const MOTIVO_MARCA: &str = "não deu para guardar a marca da instalação"; // i18n: chave
pub const MOTIVO_CERTIFICADO: &str = "o Windows não aceitou o certificado"; // i18n: chave
pub const MOTIVO_CATALOGO: &str = "a assinatura do catálogo não conferiu"; // i18n: chave
pub const MOTIVO_NO: &str = "o Windows não criou o adaptador"; // i18n: chave
pub const MOTIVO_DRIVER: &str = "o Windows não instalou o driver"; // i18n: chave
pub const MOTIVO_TESTEMUNHA: &str = "o adaptador não ficou pronto em 10 s"; // i18n: chave
pub const MOTIVO_SEM_MARCA: &str = "o Quall não instalou este driver"; // i18n: chave
pub const MOTIVO_NO_FICOU: &str = "o adaptador não saiu"; // i18n: chave
pub const MOTIVO_PACOTE_FICOU: &str = "o pacote do driver não saiu"; // i18n: chave
pub const MOTIVO_CERTIFICADO_FICOU: &str = "o certificado não saiu"; // i18n: chave
pub const MOTIVO_RECUSADO: &str = "o pedido foi recusado"; // i18n: chave
pub const MOTIVO_SEM_ADMIN: &str = "faltou a permissão de administrador"; // i18n: chave
pub const MOTIVO_SEM_FIM: &str = "o processo do driver terminou sem dizer como"; // i18n: chave
pub const MOTIVO_NAO_ABRIU: &str = "o Windows não abriu o processo do driver"; // i18n: chave

/// O andamento e o resultado.
pub const ESPERANDO_O_UAC: &str = "Esperando a permissão do Windows…"; // i18n: chave
pub const RODANDO_INSTALAR: &str = "Instalando o driver da tela estendida — passo {} de {}: {}…"; // i18n: chave
pub const RODANDO_DESINSTALAR: &str = "Desinstalando o driver da tela estendida — passo {} de {}: {}…"; // i18n: chave
pub const OK_INSTALAR: &str = "Driver da tela estendida instalado. A Tela estendida já pode ser escolhida."; // i18n: chave
pub const OK_DESINSTALAR: &str = "Driver da tela estendida desinstalado."; // i18n: chave
pub const OK_REINICIAR: &str = "Pronto, mas o Windows pede para reiniciar o computador antes de usar."; // i18n: chave
pub const CANCELADO_INSTALAR: &str = "Instalação cancelada: o Windows não recebeu a permissão. Nada foi instalado."; // i18n: chave
pub const CANCELADO_DESINSTALAR: &str = "Desinstalação cancelada: o Windows não recebeu a permissão. Nada foi tirado."; // i18n: chave
pub const FALHA_INSTALAR: &str = "Não deu para instalar o driver ({}): {}."; // i18n: chave
pub const FALHA_DESINSTALAR: &str = "Não deu para desinstalar o driver ({}): {}."; // i18n: chave
pub const ANTES_DE_COMECAR: &str = "antes de começar"; // i18n: chave

/// A frase e o tom de um andamento, em português (chaves); `None` quando não há o que dizer. A
/// janela traduz com `t`/`tf` pelas mesmas chaves (`texto_do_andamento` em `janela.rs` monta as
/// partes): aqui o que se decide é **qual** frase e **qual** tom.
pub fn frase(a: &Andamento) -> Option<(TomDoAviso, &'static str)> {
    match a {
        Andamento::Parado => None,
        Andamento::Rodando { passo: 0, .. } => Some((TomDoAviso::Info, ESPERANDO_O_UAC)),
        Andamento::Rodando { acao: Acao::Instalar, .. } => Some((TomDoAviso::Info, RODANDO_INSTALAR)),
        Andamento::Rodando { acao: Acao::Desinstalar, .. } => Some((TomDoAviso::Info, RODANDO_DESINSTALAR)),
        Andamento::Acabou { acao, resultado } => Some(match (acao, resultado) {
            (_, Resultado::OkReiniciar) => (TomDoAviso::Ambar, OK_REINICIAR),
            (Acao::Instalar, Resultado::Ok) => (TomDoAviso::Info, OK_INSTALAR),
            (Acao::Desinstalar, Resultado::Ok) => (TomDoAviso::Info, OK_DESINSTALAR),
            (Acao::Instalar, Resultado::CanceladoNoUac) => (TomDoAviso::Ambar, CANCELADO_INSTALAR),
            (Acao::Desinstalar, Resultado::CanceladoNoUac) => (TomDoAviso::Ambar, CANCELADO_DESINSTALAR),
            (Acao::Instalar, Resultado::Falha(..)) => (TomDoAviso::Vermelho, FALHA_INSTALAR),
            (Acao::Desinstalar, Resultado::Falha(..)) => (TomDoAviso::Vermelho, FALHA_DESINSTALAR),
        }),
    }
}

/// Um botão de instalar ou desinstalar pode começar agora? Um de cada vez.
pub fn pode_comecar(a: &Andamento) -> bool {
    !matches!(a, Andamento::Rodando { .. })
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn sha256_dos_vetores_do_nist() {
        assert_eq!(hex(&sha256(b"")), "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855");
        assert_eq!(hex(&sha256(b"abc")), "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD");
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248D6A61D20638B8E5C026930C3E6039A33CE45964FF2167F6ECEDD419DB06C1"
        );
        // 1 000 000 de 'a': atravessa muitos blocos.
        assert_eq!(hex(&sha256(&vec![b'a'; 1_000_000])), "CDC76E5C9914FB9281A1C7E284D73E67F1809A48A497200E046D39CCC7112CD0");
    }

    /// **Os arquivos do repositório batem com os hashes fixados** — e são os mesmos bytes que o exe
    /// embute (o mesmo `include_bytes!` de `driver_da_tela_estendida.rs`).
    #[test]
    fn os_arquivos_de_terceiros_batem() {
        let dados: [(&str, &[u8]); 4] = [
            ("SudoVDA.inf", include_bytes!("../terceiros/sudovda/SudoVDA.inf")),
            ("SudoVDA.cat", include_bytes!("../terceiros/sudovda/SudoVDA.cat")),
            ("SudoVDA.dll", include_bytes!("../terceiros/sudovda/SudoVDA.dll")),
            ("SudoVDA.cer", include_bytes!("../terceiros/sudovda/SudoVDA.cer")),
        ];
        assert_eq!(conferir_os_arquivos(&dados), Ok(()));
        // O `.inf` é UTF-16LE com BOM (como o Visual Studio o grava).
        let bruto = dados[0].1;
        assert_eq!(&bruto[..2], &[0xFF, 0xFE]);
        let unidades: Vec<u16> = bruto[2..].chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        let inf = String::from_utf16(&unidades).unwrap();
        assert!(inf.contains("DriverVer = 07/14/2025,1.10.9.289"));
        assert!(inf.contains(r"Root\SudoMaker\SudoVDA"));
        assert!(inf.contains("CatalogFile=sudovda.cat"));
        assert!(inf.contains("\r\n"), "o git não pode ter trocado o fim de linha");
    }

    #[test]
    fn um_byte_trocado_um_que_falta_ou_um_a_mais_param() {
        let inf = include_bytes!("../terceiros/sudovda/SudoVDA.inf").to_vec();
        let cat: &[u8] = include_bytes!("../terceiros/sudovda/SudoVDA.cat");
        let dll: &[u8] = include_bytes!("../terceiros/sudovda/SudoVDA.dll");
        let cer: &[u8] = include_bytes!("../terceiros/sudovda/SudoVDA.cer");
        let mut mexido = inf.clone();
        mexido[100] ^= 1;
        let e = conferir_os_arquivos(&[("SudoVDA.inf", &mexido), ("SudoVDA.cat", cat), ("SudoVDA.dll", dll), ("SudoVDA.cer", cer)]);
        assert!(e.unwrap_err().starts_with("SudoVDA.inf: SHA-256 "));
        let e = conferir_os_arquivos(&[("SudoVDA.inf", &inf), ("SudoVDA.cat", cat), ("SudoVDA.dll", dll)]);
        assert_eq!(e, Err("SudoVDA.cer: -".into()));
        let e = conferir_os_arquivos(&[("SudoVDA.inf", &inf[..10]), ("SudoVDA.cat", cat), ("SudoVDA.dll", dll), ("SudoVDA.cer", cer)]);
        assert!(e.unwrap_err().contains("10 B != 3644 B"));
        let e = conferir_os_arquivos(&[
            ("SudoVDA.inf", &inf),
            ("SudoVDA.cat", cat),
            ("SudoVDA.dll", dll),
            ("SudoVDA.cer", cer),
            ("outro.exe", b"x"),
        ]);
        assert_eq!(e, Err("outro.exe: ?".into()));
    }

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn o_processo_elevado_so_aceita_o_formato_exato() {
        let cano = nome_do_cano(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        assert_eq!(cano, r"\\.\pipe\quall-driver-0123456789abcdef0011223344556677");
        assert!(cano_valido(&cano));
        assert_eq!(
            ler_pedido(&a(&["--driver-tela-estendida", "instalar", "--cano", &cano])),
            Some(Ok(Pedido { acao: Acao::Instalar, confiar_certificado: true, cano: Some(cano.clone()) }))
        );
        assert_eq!(ler_pedido(&a(&["--driver-tela-estendida", "desinstalar", "--cano", &cano])).unwrap().unwrap().acao, Acao::Desinstalar);
        // o desinstalador do MSI: só desinstalar, sem cano
        assert_eq!(
            ler_pedido(&a(&["--driver-tela-estendida", "desinstalar", "--sem-cano"])),
            Some(Ok(Pedido { acao: Acao::Desinstalar, confiar_certificado: false, cano: None }))
        );
        assert_eq!(
            ler_pedido(&a(&["--driver-tela-estendida", "instalar", "--sem-cano"])),
            Some(Ok(Pedido { acao: Acao::Instalar, confiar_certificado: false, cano: None }))
        );
        assert_eq!(
            ler_pedido(&a(&["--driver-tela-estendida", "instalar", "--sem-cano", "--confiar-certificado"])),
            Some(Ok(Pedido { acao: Acao::Instalar, confiar_certificado: true, cano: None }))
        );
        // o caminho de sempre do app: não é pedido
        assert_eq!(ler_pedido(&a(&[])), None);
        assert_eq!(ler_pedido(&a(&["--varias-sessoes"])), None);
        // recusados
        for ruim in [
            a(&["--driver-tela-estendida"]),
            a(&["--driver-tela-estendida", "instalar", "--cano", &cano, "extra"]),
            a(&["--driver-tela-estendida", "INSTALAR", "--cano", &cano]),
            a(&["--driver-tela-estendida", "apagar", "--cano", &cano]),
            a(&["--driver-tela-estendida", "instalar", "--inf", &cano]),
            a(&["--driver-tela-estendida", "instalar", "--cano", r"\\.\pipe\outro"]),
            a(&["--driver-tela-estendida", "instalar", "--cano", r"\\.\pipe\quall-driver-0123456789ABCDEF0011223344556677"]),
            a(&["--driver-tela-estendida", "instalar", "--cano", r"\\servidor\pipe\quall-driver-0123456789abcdef0011223344556677"]),
            a(&["--driver-tela-estendida", "instalar", "--cano", r"C:\Users\x\SudoVDA.inf"]),
            a(&["--driver-tela-estendida", "instalar", "--cano", &format!("{cano}0")]),

            a(&["--driver-tela-estendida", "desinstalar", "--sem-cano", "x"]),
        ] {
            assert!(matches!(ler_pedido(&ruim), Some(Err(_))), "{ruim:?}");
        }
        assert_eq!(
            parametros(Acao::Instalar, &cano),
            r"--driver-tela-estendida instalar --cano \\.\pipe\quall-driver-0123456789abcdef0011223344556677"
        );
    }

    #[test]
    fn as_linhas_vao_e_voltam() {
        for l in [
            Linha::Passo(3),
            Linha::Diario("certificado posto em Root".into()),
            Linha::Ok,
            Linha::OkReiniciar,
            Linha::Falha(6, MOTIVO_DRIVER.into(), "UpdateDriverForPlugAndPlayDevicesW: 0xE0000247".into()),
            Linha::Falha(0, MOTIVO_OUTRA.into(), String::new()),
        ] {
            assert_eq!(Linha::ler(&l.escrever()), Some(l.clone()), "{l:?}");
        }
        assert_eq!(Linha::Falha(2, "a\tb".into(), "c\r\nd".into()).escrever(), "falha 2 a b\tc  d\n");
        assert_eq!(Linha::ler("passo 0"), None);
        assert_eq!(Linha::ler("passo x"), None);
        assert_eq!(Linha::ler("ok mais"), None);
        assert_eq!(Linha::ler("qualquer coisa"), None);
        assert_eq!(Linha::ler("falha 4"), Some(Linha::Falha(4, String::new(), String::new())));
        assert_eq!(Linha::ler("falha x y"), None);
    }

    #[test]
    fn o_resultado_vem_do_cano_e_senao_do_codigo() {
        let falha = |r: Resultado| match r {
            Resultado::Falha(n, c, _) => (n, c),
            outro => panic!("{outro:?}"),
        };
        assert_eq!(resultado(Some(&Linha::Ok), 7, Some(1)), Resultado::Ok, "o cano vence o código");
        assert_eq!(resultado(Some(&Linha::OkReiniciar), 7, Some(3010)), Resultado::OkReiniciar);
        assert_eq!(
            resultado(Some(&Linha::Falha(5, MOTIVO_NO.into(), "x".into())), 5, Some(1)),
            Resultado::Falha(5, MOTIVO_NO.into(), "x".into())
        );
        assert_eq!(falha(resultado(None, 4, Some(1))), (4, MOTIVO_SEM_FIM.into()));
        assert_eq!(falha(resultado(None, 0, Some(saida::RECUSADO))), (0, MOTIVO_RECUSADO.into()));
        assert_eq!(falha(resultado(None, 0, Some(saida::SEM_ADMIN))), (0, MOTIVO_SEM_ADMIN.into()));
        assert_eq!(falha(resultado(None, 3, Some(0))), (3, MOTIVO_SEM_FIM.into()), "0 sem o fim não é sucesso");
        assert_eq!(falha(resultado(None, 2, None)), (2, MOTIVO_SEM_FIM.into()));
    }

    #[test]
    fn a_situacao_e_quem_pode_desinstalar() {
        let x64 = Leitura { x64: true, ..Leitura::default() };
        assert_eq!(situacao(Leitura { na_loja: true, ..x64 }), Situacao::Loja { presente: false });
        assert_eq!(
            situacao(Leitura { na_loja: true, interface: true, no_presente: true, marca: true, ..x64 }),
            Situacao::Loja { presente: true },
            "a loja vence: nem desinstalar pelo app"
        );
        assert_eq!(situacao(Leitura { na_loja: true, no_presente: true, ..x64 }), Situacao::Loja { presente: true });
        assert_eq!(situacao(x64), Situacao::Ausente);
        assert_eq!(situacao(Leitura::default()), Situacao::SemSuporte, "ARM64");
        assert_eq!(situacao(Leitura { interface: true, no_presente: true, ..x64 }), Situacao::DeFora, "o Dell da bancada: instalado à mão");
        assert_eq!(situacao(Leitura { no_presente: true, ..x64 }), Situacao::Desligado, "desligado no Gerenciador, sem a marca");
        assert_eq!(situacao(Leitura { interface: true, no_presente: true, marca: true, ..x64 }), Situacao::DoQuall { presente: true });
        assert_eq!(situacao(Leitura { no_presente: true, marca: true, ..x64 }), Situacao::DoQuall { presente: false });
        assert_eq!(situacao(Leitura { marca: true, ..x64 }), Situacao::DoQuall { presente: false }, "a marca sem nó: desinstalar limpa o resto");

        assert!(!pode_desinstalar(Situacao::Loja { presente: true }));
        assert!(!pode_desinstalar(Situacao::Ausente));
        assert!(!pode_desinstalar(Situacao::DeFora), "o que o Quall não pôs, o Quall não tira");
        assert!(!pode_desinstalar(Situacao::Desligado));
        assert!(pode_desinstalar(Situacao::DoQuall { presente: true }));
        assert!(pode_desinstalar(Situacao::DoQuall { presente: false }));

        assert!(ladrilho_instala(Situacao::Ausente));
        assert!(!ladrilho_instala(Situacao::Loja { presente: false }), "na loja, baixar o instalador avulso");
        assert!(!ladrilho_instala(Situacao::DoQuall { presente: false }), "instalar de novo duplicaria o nó");
        assert!(!ladrilho_instala(Situacao::Desligado));

        assert_eq!(clique_no_apagado(Situacao::Ausente), CliqueNoApagado::Instalar);
        assert_eq!(clique_no_apagado(Situacao::Loja { presente: false }), CliqueNoApagado::Pagina);
        assert_eq!(clique_no_apagado(Situacao::Loja { presente: true }), CliqueNoApagado::Ajustes, "na loja, desligado: os Ajustes dizem");
        assert_eq!(clique_no_apagado(Situacao::SemSuporte), CliqueNoApagado::Pagina);
        assert_eq!(clique_no_apagado(Situacao::Desligado), CliqueNoApagado::Ajustes);
        assert_eq!(clique_no_apagado(Situacao::DoQuall { presente: false }), CliqueNoApagado::Ajustes);
        assert_eq!(detalhe_do_apagado(Situacao::Ausente), "Instalar o driver da tela estendida");
        assert_eq!(detalhe_do_apagado(Situacao::Loja { presente: false }), "Baixe o driver da tela estendida");
        assert_eq!(detalhe_do_apagado(Situacao::SemSuporte), "Precisa do driver SudoVDA");
        assert_eq!(botao_dos_ajustes(Situacao::DoQuall { presente: false }), None, "o setup gerencia o driver junto com o app");
        assert_eq!(botao_dos_ajustes(Situacao::Loja { presente: true }), Some(BotaoDosAjustes::PaginaDoInstalador), "na loja, sem elevar");
        assert_eq!(botao_dos_ajustes(Situacao::Loja { presente: false }), None);
        assert_eq!(botao_dos_ajustes(Situacao::DeFora), None);
        // O instalador avulso: instalar só sem o adaptador; desinstalar só o que o Quall pôs; nada no
        // meio de outra; fechar não, no meio.
        let parado = Andamento::Parado;
        let rodando = Andamento::Rodando { acao: Acao::Instalar, passo: 3 };
        assert_eq!(botoes_do_instalador(Situacao::Ausente, &parado), (true, false));
        assert_eq!(botoes_do_instalador(Situacao::DoQuall { presente: true }, &parado), (false, true));
        assert_eq!(botoes_do_instalador(Situacao::DeFora, &parado), (false, false), "o Dell: nem instala por cima nem tira");
        // ... e o corpo diz por quê, e o que fazer (o achado do Bruno, 02/10, noite).
        let corpo = aviso_no_corpo_do_instalador(Situacao::DeFora).expect("o SudoVDA de fora explica no corpo");
        assert!(corpo.contains("instalado por outro programa"));
        assert!(corpo.contains("já funciona com ele"));
        assert!(corpo.contains("remova antes o SudoVDA pelo programa que o instalou"));
        assert_eq!(frase_dos_ajustes(Situacao::DeFora), corpo, "o mesmo texto nos Ajustes");
        assert_eq!(aviso_no_corpo_do_instalador(Situacao::Ausente), None);
        assert_eq!(aviso_no_corpo_do_instalador(Situacao::DoQuall { presente: true }), None);
        assert_eq!(botoes_do_instalador(Situacao::Desligado, &parado), (false, false));
        assert_eq!(botoes_do_instalador(Situacao::Ausente, &rodando), (false, false));
        assert!(!pode_fechar(&rodando));
        assert!(pode_fechar(&Andamento::Acabou { acao: Acao::Instalar, resultado: Resultado::Ok }));
        for s in [
            Situacao::Loja { presente: false },
            Situacao::Loja { presente: true },
            Situacao::SemSuporte,
            Situacao::Ausente,
            Situacao::DoQuall { presente: true },
            Situacao::DoQuall { presente: false },
            Situacao::DeFora,
            Situacao::Desligado,
        ] {
            assert!(!frase_dos_ajustes(s).is_empty());
            assert!(!texto_acessivel_do_apagado(s).is_empty());
            assert!(!frase_do_instalador(s).contains("Ajustes"), "o instalador avulso não tem Ajustes: {s:?}");
        }
    }

    #[test]
    fn o_andamento_e_a_caixa() {
        assert_eq!(frase(&Andamento::Parado), None);
        assert_eq!(frase(&Andamento::Rodando { acao: Acao::Instalar, passo: 0 }), Some((TomDoAviso::Info, ESPERANDO_O_UAC)));
        assert_eq!(frase(&Andamento::Rodando { acao: Acao::Instalar, passo: 3 }), Some((TomDoAviso::Info, RODANDO_INSTALAR)));
        assert_eq!(
            frase(&Andamento::Acabou { acao: Acao::Instalar, resultado: Resultado::CanceladoNoUac }),
            Some((TomDoAviso::Ambar, CANCELADO_INSTALAR))
        );
        assert_eq!(
            frase(&Andamento::Acabou { acao: Acao::Desinstalar, resultado: Resultado::Falha(2, MOTIVO_PACOTE_FICOU.into(), "x".into()) }),
            Some((TomDoAviso::Vermelho, FALHA_DESINSTALAR))
        );
        assert_eq!(frase(&Andamento::Acabou { acao: Acao::Instalar, resultado: Resultado::Ok }), Some((TomDoAviso::Info, OK_INSTALAR)));
        assert!(!pode_comecar(&Andamento::Rodando { acao: Acao::Instalar, passo: 2 }), "um de cada vez");
        assert!(pode_comecar(&Andamento::Parado));
        assert!(pode_comecar(&Andamento::Acabou { acao: Acao::Instalar, resultado: Resultado::CanceladoNoUac }));
        assert_eq!(nome_do_passo(Acao::Instalar, 1), Some("Conferindo os arquivos do driver"));
        assert_eq!(nome_do_passo(Acao::Instalar, 0), None);
        assert_eq!(nome_do_passo(Acao::Desinstalar, 5), None);
        // A caixa diz as três coisas que o Bruno pediu: o que é, o UAC e o certificado.
        assert!(CAIXA_CORPO_O_QUE_E.contains("driver de monitor virtual"));
        assert!(CAIXA_CORPO_UAC.contains("administrador"));
        assert!(CAIXA_CORPO_CERTIFICADO.contains("certificado"));
        assert!(CAIXA_CORPO_CERTIFICADO.contains("Root e TrustedPublisher"));
        assert_eq!(CAIXA_CORPO_O_QUE_E.matches("{}").count() + CAIXA_CORPO_CERTIFICADO.matches("{}").count(), 2);
        for k in [CAIXA_CORPO_O_QUE_E, CAIXA_CORPO_UAC, CAIXA_CORPO_CERTIFICADO, CAIXA_DESINSTALAR_CORPO] {
            assert!(!k.contains('\n'), "a varredura do idioma não lê \\n numa chave");
        }
    }

    #[test]
    fn o_pacote_e_os_nomes() {
        assert!(pacote_e_do_sudovda("sudovda.inf", "SudoMaker"));
        assert!(pacote_e_do_sudovda("SudoVDA.inf", "SudoMaker"));
        assert!(!pacote_e_do_sudovda("nv_dispi.inf", "NVIDIA"));
        assert!(!pacote_e_do_sudovda("sudovda.inf", "Outro"), "o nome sozinho não basta");
        assert!(nome_publicado_valido("oem221.inf"));
        assert!(nome_publicado_valido("OEM7.INF"));
        assert!(!nome_publicado_valido("oem.inf"));
        assert!(!nome_publicado_valido(r"..\oem1.inf"));
        assert!(!nome_publicado_valido("sudovda.inf"));
        assert!(!nome_publicado_valido("oem12345678.inf"));
        assert!(hardware_ids_do_sudovda(&["ROOT\\SudoMaker\\SudoVDA".to_string()]));
        assert!(!hardware_ids_do_sudovda(&["PCI\\VEN_8086".to_string()]));
        assert!(monitor_do_sudovda(r"DISPLAY\SMKD1CE\5&2b5e3a1&0&UID256"));
        assert!(!monitor_do_sudovda(r"DISPLAY\LGD05F2\4&1ab2c996&0&UID265988"));
    }
}
