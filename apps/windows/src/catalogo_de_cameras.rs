//! O catálogo de câmeras e a escolha da fonte: **a parte que é aritmética**.
//!
//! Desenho em `docs/camera-no-windows.md` §2 e §6. Aqui não há Win32 nem `quall-core`: é o que
//! decide, a partir de um link simbólico e do que o registro devolveu, **de quem é uma câmera** e
//! se ela entra no seletor; e, a partir de uma lista, **qual fonte fica escolhida**. Separado de
//! `cameras.rs` (que enumera e lê o registro) pelo mesmo motivo de `regua.rs` e `cadeia.rs`: uma
//! regra que só roda com aparelho nunca é exercitada, e esta é a única guarda contra o laço.
//!
//! # A regra do dono, em uma frase
//!
//! Uma câmera criada por `MFCreateVirtualCamera` (link `…#VCAMDEVAPI#…`) entra no seletor **só**
//! se o registro disser, lido de fato, que o dono dela é **outro** (`CustomCaptureSourceClsid`
//! diferente do nosso). Nosso, sem dono, ou não deu para ler: **fica de fora e o motivo vai para o
//! registro do app**. Errar para o lado de esconder custa uma câmera de terceiro sumida da lista;
//! errar para o outro lado é o Quall transmitir o que ele mesmo está recebendo.
//!
//! Câmera que não é desse tipo (USB, a da Canon em `ROOT\…`) **também tem o dono lido**: se o
//! `CustomCaptureSourceClsid` dela for o nosso, ela fica de fora como qualquer câmera do Quall. Hoje
//! o Quall só cria câmeras por `MFCreateVirtualCamera` (`baia.rs`), mas o nome do enumerador
//! (`SWD#VCAMDEVAPI#`) não é contrato documentado, e um prefixo de texto não pode ser a única porta
//! do laço (a revisão de código de 18/09, achado 3 do catálogo). Fora de `VCAMDEVAPI`, não conseguir
//! ler continua deixando entrar: é a webcam USB, que não tem o valor.
//!
//! # A regra da escolha, em duas
//!
//! - `--fonte` que não casa com nada é **erro**, nunca "a primeira da lista": a primeira é o monitor
//!   principal, e cair nele transmitiria a área de trabalho de quem pediu uma câmera (a revisão
//!   adversarial de 18/09, achado 1 do catálogo).
//! - A fonte escolhida que some da lista deixa o seletor **sem escolha**, com o aviso na tela, e
//!   **nunca** vira outra. Se a **mesma** (o mesmo id **e** o mesmo nome) voltar, ela volta escolhida
//!   e o aviso sai: não é escolher outra, é desfazer o sumiço (a revisão de código de 18/09, M1: sem
//!   isto, a TV que desliga e religa deixava o aviso falso na tela e o Espelhar recusando). Com o
//!   nome dela na lista e outro id, o aviso sai e nada é escolhido. O Mac tem a mesma regra
//!   (`QuallCaptureKit/SeletorDeFontes.swift`).

/// O CLSID da fonte de mídia do Quall (`integrations/camera-windows/fonte/src/lib.rs`,
/// `CLSID_FONTE`), como número. Comparado como GUID, nunca como texto: `{5c75…}` e `5C75…` sem
/// chaves são o mesmo dono. O teste `o_clsid_e_o_da_fonte` confere contra a constante da fonte.
pub const CLSID_DA_FONTE_DO_QUALL: u128 = 0x5c75fe52_9204_45f6_b143_58b1ac8048e5;

/// A referência de interface que `MFCreateVirtualCamera` publica: o `Device Parameters` com o dono
/// mora debaixo dela (`docs/camera-virtual.md`, seção 2 de 27/08).
pub const REFERENCIA_DA_CAMERA_VIRTUAL: &str = "{FCEBBA03-9D13-4C13-9940-CC84FCD132D1}";

/// O que um link simbólico de câmera diz de si mesmo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartesDoLink {
    /// `SWD#VCAMDEVAPI#<hash>#{classe}` — sem o prefixo `\\?\` e sem a referência.
    pub interface: String,
    /// A classe de interface, com chaves: `{e5323777-…}`.
    pub classe: String,
    /// O que vem depois da barra: `{fcebba03-…}`, `global`, `eoswebcamcamerasource`.
    pub referencia: Option<String>,
    /// Criada por `MFCreateVirtualCamera`?
    pub virtual_do_mf: bool,
}

/// Separa um link simbólico do Media Foundation. `None` quando não tem a forma
/// `\\?\A#B#C#{classe}[\referência]`.
pub fn partes_do_link(link: &str) -> Option<PartesDoLink> {
    let sem_prefixo = link.strip_prefix(r"\\?\").unwrap_or(link);
    let (interface, referencia) = match sem_prefixo.split_once('\\') {
        Some((i, r)) if !r.is_empty() => (i, Some(r.to_string())),
        Some((i, _)) => (i, None),
        None => (sem_prefixo, None),
    };
    let classe = interface.rsplit('#').next()?;
    if !(classe.starts_with('{') && classe.ends_with('}') && guid(classe).is_some()) {
        return None;
    }
    if interface.split('#').count() < 3 {
        return None;
    }
    Some(PartesDoLink {
        interface: interface.to_string(),
        classe: classe.to_string(),
        referencia,
        virtual_do_mf: interface.to_ascii_uppercase().starts_with("SWD#VCAMDEVAPI#"),
    })
}

/// As chaves de `HKLM` onde o `Device Parameters` da interface pode estar, na ordem de tentar. É o
/// recuo de `cameras.rs` quando `CM_Open_Device_Interface_KeyW` falha: a referência do próprio
/// link, a das câmeras virtuais e a vazia.
pub fn chaves_do_dono(p: &PartesDoLink) -> Vec<String> {
    let base = format!(
        r"SYSTEM\CurrentControlSet\Control\DeviceClasses\{}\##?#{}",
        p.classe, p.interface
    );
    let mut v = Vec::new();
    if let Some(r) = &p.referencia {
        v.push(format!(r"{base}\#{r}\Device Parameters"));
    }
    let virtual_ = format!(r"{base}\#{REFERENCIA_DA_CAMERA_VIRTUAL}\Device Parameters");
    if !v.iter().any(|c| c.eq_ignore_ascii_case(&virtual_)) {
        v.push(virtual_);
    }
    v.push(format!(r"{base}\#\Device Parameters"));
    v
}

/// Um GUID em texto, com ou sem chaves, em qualquer caixa → o número. `None` se não for GUID.
pub fn guid(texto: &str) -> Option<u128> {
    let t = texto.trim();
    let t = t.strip_prefix('{').and_then(|s| s.strip_suffix('}')).unwrap_or(t);
    let partes: Vec<&str> = t.split('-').collect();
    let tamanhos = [8usize, 4, 4, 4, 12];
    if partes.len() != 5 || partes.iter().zip(tamanhos).any(|(p, n)| p.len() != n) {
        return None;
    }
    let hexa: String = partes.concat();
    if !hexa.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(&hexa, 16).ok()
}

/// O que a leitura do `CustomCaptureSourceClsid` devolveu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leitura {
    /// O valor, como texto.
    Valor(String),
    /// A chave abriu e o valor não existe.
    SemValor,
    /// Nem a chave abriu: o motivo, para o registro.
    Falhou(String),
}

/// De quem é uma câmera — e, por isso, se ela entra no seletor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dono {
    /// Não é câmera virtual do `MFCreateVirtualCamera`: webcam USB, câmera de software de outro
    /// fabricante. **Entra.**
    NaoVirtual,
    /// Câmera virtual cujo dono é outro, lido de fato (a Câmera Conectada do Windows, por
    /// exemplo). **Entra** — decisão do usuário de 18/09: câmeras virtuais de terceiros são listadas.
    OutroDono(String),
    /// O nosso CLSID: uma baia do app, a câmera de bancada de uma sonda, o nó de agosto. **Fica de
    /// fora**: listá-la é o laço.
    Quall,
    /// Câmera virtual sem `CustomCaptureSourceClsid` (a carcaça de `docs/camera-virtual.md` §5), ou
    /// com um valor que não é GUID. **Fica de fora.**
    SemDono(String),
    /// Não deu para ler o dono. **Fica de fora**, com o motivo no registro: não saber de quem é não
    /// autoriza listar.
    Ilegivel(String),
}

impl Dono {
    pub fn vai_para_o_seletor(&self) -> bool {
        matches!(self, Dono::NaoVirtual | Dono::OutroDono(_))
    }

    /// Uma palavra para o registro.
    pub fn rotulo(&self) -> String {
        match self {
            Dono::NaoVirtual => "não é câmera virtual do MF".into(), // i18n: fora (diário e bandeira de bancada)
            Dono::OutroDono(c) => format!("câmera virtual de outro dono {c}"), // i18n: fora (diário e bandeira de bancada)
            Dono::Quall => "câmera do próprio Quall (fora: seria o laço)".into(), // i18n: fora (diário e bandeira de bancada)
            Dono::SemDono(m) => format!("câmera virtual sem dono lido (fora): {m}"), // i18n: fora (diário e bandeira de bancada)
            Dono::Ilegivel(m) => format!("dono ilegível (fora): {m}"), // i18n: fora (diário e bandeira de bancada)
        }
    }
}

/// A regra do dono. Link que nem tem a forma de link é tratado como ilegível, e não como "não
/// virtual": quem não se deixa reconhecer não ganha o benefício da dúvida.
///
/// - Câmera virtual do MF (`VCAMDEVAPI`): entra **só** com outro dono lido de fato.
/// - As outras: o nosso CLSID, em qualquer link, deixa de fora; qualquer outra coisa (sem valor,
///   leitura que falhou, outro CLSID) entra, porque a webcam USB não tem o valor e é o caso comum.
pub fn classificar(link: &str, leitura: Option<&Leitura>) -> Dono {
    let Some(partes) = partes_do_link(link) else {
        return Dono::Ilegivel(format!("link sem a forma esperada: {link}")); // i18n: fora (diário e bandeira de bancada)
    };
    if !partes.virtual_do_mf {
        return match leitura {
            Some(Leitura::Valor(v)) if guid(v) == Some(CLSID_DA_FONTE_DO_QUALL) => Dono::Quall,
            _ => Dono::NaoVirtual,
        };
    }
    match leitura {
        None => Dono::Ilegivel("o dono não foi lido".into()), // i18n: fora (diário e bandeira de bancada)
        Some(Leitura::Falhou(m)) => Dono::Ilegivel(m.clone()),
        Some(Leitura::SemValor) => Dono::SemDono("sem CustomCaptureSourceClsid".into()), // i18n: fora (diário e bandeira de bancada)
        Some(Leitura::Valor(v)) => match guid(v) {
            None => Dono::SemDono(format!("CustomCaptureSourceClsid não é GUID: {v:?}")), // i18n: fora (diário e bandeira de bancada)
            Some(g) if g == CLSID_DA_FONTE_DO_QUALL => Dono::Quall,
            Some(_) => Dono::OutroDono(v.trim().to_string()),
        },
    }
}

// =============================================================================================
// A escolha da fonte
// =============================================================================================

/// O que a escolha precisa saber de cada linha do seletor.
#[derive(Debug, Clone, Copy)]
pub struct Candidata<'a> {
    pub id: &'a str,
    pub nome: &'a str,
    /// Um monitor de verdade (nem a tela estendida, nem câmera). É o que `--fonte tela` escolhe.
    pub monitor: bool,
}

/// A escolha na abertura do app.
///
/// - Sem `--fonte`: o primeiro **monitor**, que é o principal (`fontes::monitores` o põe na
///   frente). É o que a pessoa vê escolhido; não é recuo de nada. **Sem monitor, nenhuma escolha:
///   câmera nunca é padrão** (a regra do Mac, §6.1). Antes daqui o padrão era a primeira linha, e
///   numa lista sem monitor ela é uma câmera: desde a fase 3 isso a abria sem ninguém escolher (a
///   revisão do código da fase 3, M4). A tela estendida também não é padrão: ela cria um monitor.
/// - `--fonte tela`: o primeiro **monitor**. Sem monitor, erro.
/// - `--fonte <id ou nome>`: o casamento exato, sem caixa. **Sem casamento, erro**: o chamador sai
///   do processo. Nunca a primeira da lista.
pub fn escolher_inicial(lista: &[Candidata], pedida: Option<&str>) -> Result<Option<usize>, String> {
    let Some(pedida) = pedida else {
        return Ok(lista.iter().position(|c| c.monitor));
    };
    if pedida.eq_ignore_ascii_case("tela") { // i18n: fora (diário e bandeira de bancada)
        return match lista.iter().position(|c| c.monitor) {
            Some(i) => Ok(Some(i)),
            None => Err("--fonte tela: nenhum monitor listado".into()), // i18n: fora (diário e bandeira de bancada)
        };
    }
    match lista
        .iter()
        .position(|c| c.id.eq_ignore_ascii_case(pedida) || c.nome.eq_ignore_ascii_case(pedida))
    {
        Some(i) => Ok(Some(i)),
        None => Err(format!(
            // i18n: fora (diário e bandeira de bancada)
            "--fonte {pedida:?} não casa com nenhuma fonte listada ({}). O app não escolhe outra no \
             lugar.",
            lista.iter().map(|c| c.nome).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// O que a lista nova fez com a escolha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reescolha {
    /// A escolha continua (no índice novo), ou continua sem escolha.
    Ficou(Option<usize>),
    /// A escolhida saiu da lista: sem escolha, e o aviso para a tela. Quem chama guarda o id dela
    /// como "a que sumiu".
    Sumiu { aviso: String },
    /// A que tinha sumido voltou, **pelo mesmo id e com o mesmo nome**: escolhida de novo, e o aviso
    /// sai da tela.
    Voltou(usize),
    /// Uma fonte com o **nome** da que sumiu, mas com outro id, está na lista: pode ser ela (o Sidecar
    /// do Mac volta com outro id) ou outra (um segundo monitor igual). Continua **sem escolha**, mas o
    /// aviso "não está mais disponível" sai, porque a lista mostra o nome dele.
    Reapareceu,
}

/// Depois de a lista mudar: a escolha de antes, **pelo id**, na lista nova.
///
/// `antes` é a escolhida (id e nome); `sumida` é a que saiu da lista numa mudança anterior (id e
/// nome) e ainda não foi trocada por uma escolha da pessoa. A escolha que sumiu **não** vira outra:
/// fica sem escolha até a pessoa escolher, ou até **a mesma** voltar.
///
/// "A mesma" é o id **e** o nome (a reconferência de 18/09). O id de monitor no Windows é o nome GDI
/// (`\\.\DISPLAY4`), que é da **saída** de vídeo: um projetor ligado na porta da TV volta com o
/// mesmo id e outro nome, e escolhê-lo seria escolher outra no lugar. Mesmo id com outro nome dá
/// `Ficou(None)`, e o aviso continua verdadeiro. Mesmo nome com outro id dá `Reapareceu`.
///
/// A escolha **presente** (`antes`) continua casando só pelo id, como no `main`: o nome de um
/// monitor pode mudar sem ele sair (o desempate de homônimos acrescenta o sufixo quando um gêmeo
/// entra), e exigir o nome ali daria "sumiu" com o monitor na tela.
pub fn reescolher(antes: Option<(&str, &str)>, sumida: Option<(&str, &str)>, nova: &[Candidata]) -> Reescolha {
    let posicao = |id: &str| nova.iter().position(|c| c.id.eq_ignore_ascii_case(id));
    match antes {
        Some((id, nome)) => match posicao(id) {
            Some(i) => Reescolha::Ficou(Some(i)),
            // No idioma da hora: quem chama guarda este texto e o tira do conselho por ele mesmo
            // (`sem_o_aviso`), então a troca de idioma no meio não o deixa preso.
            None => Reescolha::Sumiu { aviso: crate::idioma::tf("\"{}\" não está mais disponível. Escolha outra fonte.", &[&nome]) },
        },
        None => {
            let Some((id, nome)) = sumida else {
                return Reescolha::Ficou(None);
            };
            match posicao(id) {
                Some(i) if nova[i].nome == nome => Reescolha::Voltou(i),
                _ if nova.iter().any(|c| c.nome == nome) => Reescolha::Reapareceu,
                _ => Reescolha::Ficou(None),
            }
        }
    }
}

/// O conselho com o aviso da fonte que sumiu **acrescentado**: o que já estava (o motivo do fim de
/// uma sessão) fica na frente.
pub fn com_o_aviso(conselho: &str, aviso: &str) -> String {
    if conselho.trim().is_empty() {
        aviso.to_string()
    } else {
        format!("{} {aviso}", conselho.trim_end())
    }
}

/// O conselho sem o aviso da fonte que sumiu, e com o resto intacto. Se o aviso não está mais lá (o
/// fim de uma sessão escreveu por cima dele), o conselho volta igual.
pub fn sem_o_aviso(conselho: &str, aviso: &str) -> String {
    if aviso.is_empty() || !conselho.contains(aviso) {
        return conselho.to_string();
    }
    conselho.replacen(aviso, "", 1).trim().to_string()
}

/// O trecho de instância do link, para desempatar nomes repetidos: `6&b813745&0&0000` numa webcam
/// USB composta (derivado da porta), o número de série numa não composta, **inteiros** até 24
/// caracteres; acima disso (o hash de 64 de uma câmera virtual), os 12 **últimos**. **Não** os
/// últimos caracteres do link, que são da referência e se repetem em toda webcam (`}\global`) e em
/// toda câmera virtual (a revisão de código de 18/09); nem os primeiros de um número de série, que
/// costumam ser o prefixo comum de um lote (a reconferência).
pub fn instancia_curta(link: &str) -> String {
    let ultimos = |s: &str, n: usize| -> String {
        let v: Vec<char> = s.chars().collect();
        v[v.len().saturating_sub(n)..].iter().collect()
    };
    let Some(p) = partes_do_link(link) else {
        return ultimos(link, 12);
    };
    let instancia = p.interface.split('#').nth(2).unwrap_or("");
    if instancia.chars().count() > 24 {
        ultimos(instancia, 12)
    } else {
        instancia.to_string()
    }
}

/// Nomes repetidos ganham `" (sufixo)"`, **todos** os do grupo, e a decisão sai da lista original:
/// renomear no lugar deixava a segunda de cada par sem sufixo, e com três iguais duas linhas ficavam
/// idênticas (a revisão de código de 18/09, que achou o mesmo laço no desempate de monitores).
pub fn desempatar(itens: &[(String, String)]) -> Vec<String> {
    itens
        .iter()
        .enumerate()
        .map(|(i, (nome, sufixo))| {
            let repetido = itens.iter().enumerate().any(|(j, (outro, _))| j != i && outro == nome);
            if repetido {
                format!("{nome} ({sufixo})")
            } else {
                nome.clone()
            }
        })
        .collect()
}

// =============================================================================================
// A câmera de bancada da sonda
// =============================================================================================

/// O nome que `quall_camera_local ler --criar-camera <base>` dá à câmera que cria: nenhum aparelho
/// pareado tem esse nome, então o `Quall.exe` do usuário nunca serve o cano dela.
pub fn nome_da_camera_de_bancada(base: &str, pid: u32) -> String {
    format!("{base} sonda {pid}")
}

/// O nome enumerado é **exatamente** o pedido? O Media Foundation acrescenta
/// " (Câmera Virtual do Windows)" ao nome amigável; então casa o nome inteiro, ou o nome seguido de
/// " (". **Não** por prefixo: "X sonda 1234" é prefixo de "X sonda 12345", e uma sonda ativaria a
/// câmera da outra (a revisão de código de 18/09).
pub fn e_o_nome_pedido(enumerado: &str, pedido: &str) -> bool {
    enumerado == pedido || enumerado.strip_prefix(pedido).is_some_and(|resto| resto.starts_with(" ("))
}

/// O PID de uma câmera de bancada, lido do nome enumerado (`"<base> sonda <PID>"`, com ou sem o
/// " (…)" do sistema). `None` para qualquer outro nome: uma baia do `Quall.exe` do usuário tem o nome
/// de um aparelho.
pub fn pid_da_camera_de_bancada(enumerado: &str) -> Option<u32> {
    let i = enumerado.rfind(" sonda ")?;
    let depois = &enumerado[i + " sonda ".len()..];
    let fim = depois.find(|c: char| !c.is_ascii_digit()).unwrap_or(depois.len());
    let (digitos, resto) = depois.split_at(fim);
    if digitos.is_empty() || !(resto.is_empty() || (resto.starts_with(" (") && resto.ends_with(')'))) {
        return None;
    }
    digitos.parse().ok()
}

/// **A câmera de bancada que `--camera-de-bancada` pode liberar** (`docs/camera-no-windows.md` §7.3):
/// dono Quall, o nome de uma câmera de bancada ("<base> sonda <PID>", `pid_da_camera_de_bancada`)
/// e o PID de um `quall_camera_local.exe` vivo (`imagem_do_pid` devolve o nome do executável de um
/// processo vivo, ou `None`). Devolve o PID, ou o motivo da recusa. Uma baia do `Quall.exe` do
/// usuário tem o dono Quall e o nome de um aparelho: recusada.
pub fn conferir_camera_de_bancada(nome: &str, dono: &Dono, imagem_do_pid: &dyn Fn(u32) -> Option<String>) -> Result<u32, String> {
    if !matches!(dono, Dono::Quall) {
        return Err(format!("\"{nome}\" não é câmera do Quall ({}): a bandeira só libera a câmera de bancada", dono.rotulo())); // i18n: fora (diário e bandeira de bancada)
    }
    let Some(pid) = pid_da_camera_de_bancada(nome) else {
        return Err(format!(
            "\"{nome}\" não tem o nome de uma câmera de bancada (\"<base> sonda <PID>\"): pode ser uma baia do Quall.exe do usuário" // i18n: fora (diário e bandeira de bancada)
        ));
    };
    match imagem_do_pid(pid) {
        Some(imagem) if imagem.eq_ignore_ascii_case("quall_camera_local.exe") => Ok(pid),
        Some(imagem) => Err(format!("o PID {pid} do nome é de \"{imagem}\", e não de uma sonda")), // i18n: fora (diário e bandeira de bancada)
        None => Err(format!("o PID {pid} do nome não é de um processo vivo")), // i18n: fora (diário e bandeira de bancada)
    }
}

/// **As linhas do catálogo que mudaram** desde as últimas escritas no registro: as novas, na ordem
/// de agora, e as que saíram, com "saiu do catálogo: ". Na primeira vez (sem anteriores), todas.
/// O R5 (M55) mostrou por quê: a espera pela câmera de bancada relia o catálogo a cada ~260 ms e
/// escrevia as cinco linhas de novo a cada volta, por 10 s. Uma linha por mudança.
pub fn linhas_que_mudaram(anteriores: &[String], agora: &[String]) -> Vec<String> {
    let mut saida: Vec<String> = agora.iter().filter(|l| !anteriores.contains(l)).cloned().collect();
    saida.extend(anteriores.iter().filter(|l| !agora.contains(l)).map(|l| format!("saiu do catálogo: {l}"))); // i18n: fora (diário e bandeira de bancada)
    saida
}

// =============================================================================================
// A recarga do seletor fora da thread da janela (a revisão curta do `08af2cd`, A1–A3)
// =============================================================================================

/// **O clique no seletor vale?** (A1). A janela manda o índice **e a revisão da lista que ela
/// mostrava**. A recarga fora da thread da janela troca a lista a qualquer momento, e a janela só
/// remonta o seletor no pulso seguinte (até 100 ms): um clique nesse intervalo apontava, pelo
/// índice, para outra linha da lista nova — com o monitor principal na linha 0, podia virar a área
/// de trabalho no lugar da câmera. Com a revisão diferente, o clique é descartado e o seletor
/// remontado mostra a escolha que vale. Devolve o índice a escolher, ou `None` (inválido, repetido
/// ou de outra lista).
pub fn escolha_do_clique(indice: usize, revisao_da_tela: u64, revisao: u64, linhas: usize, escolhida: Option<usize>) -> Option<usize> {
    (revisao_da_tela == revisao && indice < linhas && escolhida != Some(indice)).then_some(indice)
}

/// **A lista enumerada é a mais nova?** (A2). O `WM_DEVICECHANGE`, o `WM_DISPLAYCHANGE` e a volta à
/// tela inicial enumeram, cada um no seu tempo, e aplicam a lista sob a trava do estado. Cada
/// enumeração toma um número **antes** de começar, e só se aplica uma lista mais nova que a última
/// aplicada: uma enumeração lenta que termina depois de uma rápida não põe a lista velha de volta.
pub fn lista_mais_nova(geracao: u64, aplicada: u64) -> bool {
    geracao > aplicada
}

/// **Uma thread de recarga por vez, e os pedidos juntos** (A3). Cada `WM_DEVICECHANGE` subia uma
/// thread, e uma câmera USB que entra manda vários: as threads enfileiravam na trava e enumeravam
/// uma depois da outra, todas para o mesmo resultado. Com o juntador, o pedido que chega com uma
/// recarga rodando marca "pendente", e a que roda dá **mais uma volta** no fim; nenhum pedido se
/// perde, e nunca há duas rodando.
///
/// O uso: `if j.pedir() { subir a thread }`; na thread, `let _vez = j.vez();` e `loop {
/// j.comecar_volta(); recarregar; if !j.mais_uma_volta() { break } }`.
///
/// A prova da ordem são os testes com os pontos de parada (`pedir_com`, `mais_uma_volta_com`): eles
/// param uma chamada no meio e fazem a outra ali, e pegam cada um dos três defeitos que a revisão do
/// `ef71d20` plantou (M1). O teste com threads fica como fumaça: com os dois defeitos que perdem
/// pedido, ele passou 4000 vezes em 4000.
#[derive(Debug, Default)]
pub struct Juntador {
    pendente: std::sync::atomic::AtomicBool,
    rodando: std::sync::atomic::AtomicBool,
}

impl Juntador {
    pub const fn novo() -> Self {
        Juntador { pendente: std::sync::atomic::AtomicBool::new(false), rodando: std::sync::atomic::AtomicBool::new(false) }
    }

    /// Um pedido. `true`: quem pediu sobe a thread; `false`: uma já roda e vai dar mais uma volta.
    pub fn pedir(&self) -> bool {
        self.pedir_com(|| {})
    }

    /// O `pedir`, com um ponto de parada entre marcar o pendente e tentar a vez (só os testes param
    /// ali).
    fn pedir_com(&self, entre: impl FnOnce()) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.pendente.store(true, SeqCst);
        entre();
        self.rodando.compare_exchange(false, true, SeqCst, SeqCst).is_ok()
    }

    /// A thread começa uma volta: os pedidos até aqui estão atendidos por ela.
    pub fn comecar_volta(&self) {
        self.pendente.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// A volta acabou. `true`: chegou pedido durante ela, e a thread dá mais uma; `false`: a thread
    /// sai, e o próximo pedido sobe outra. Um pedido que chega entre a saída e o `rodando = false`
    /// não se perde: ou ele vê `rodando` falso e sobe a thread, ou a thread o vê pendente e retoma.
    pub fn mais_uma_volta(&self) -> bool {
        self.mais_uma_volta_com(|| {}, || {})
    }

    /// O `mais_uma_volta`, com um ponto de parada antes e outro depois de soltar a vez (só os
    /// testes param ali).
    fn mais_uma_volta_com(&self, antes_de_soltar: impl FnOnce(), depois_de_soltar: impl FnOnce()) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        if self.pendente.load(SeqCst) {
            return true;
        }
        antes_de_soltar();
        self.rodando.store(false, SeqCst);
        depois_de_soltar();
        self.pendente.load(SeqCst) && self.rodando.compare_exchange(false, true, SeqCst, SeqCst).is_ok()
    }

    /// Se uma thread roda agora (para o teste e para a thread que não subiu).
    pub fn rodando(&self) -> bool {
        self.rodando.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A thread não subiu: quem pediu solta a vez, e a recarga é feita por ele.
    pub fn soltar(&self) {
        self.rodando.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// **A vez da thread que roda, com a guarda de pânico** (a revisão do `ef71d20`, L1). Criada no
    /// começo da thread. Se a thread morrer em pânico no meio de uma volta (a FFI da enumeração, um
    /// índice), a guarda solta a vez ao desenrolar. Sem ela, o juntador ficava em "rodando" para
    /// sempre: todo pedido seguinte voltava calado, e o seletor nunca mais se recompunha pelo
    /// `WM_DEVICECHANGE`. O pedido da volta que morreu fica para o evento seguinte.
    pub fn vez(&self) -> VezDaRecarga<'_> {
        VezDaRecarga { juntador: self }
    }
}

/// A guarda de [`Juntador::vez`]: solta a vez só se a thread estiver em pânico. Na saída normal
/// quem solta é o `mais_uma_volta`.
#[derive(Debug)]
pub struct VezDaRecarga<'a> {
    juntador: &'a Juntador,
}

impl Drop for VezDaRecarga<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.juntador.soltar();
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// **O clique de outra lista** (a revisão curta do `08af2cd`, A1): a janela mostrava a revisão
    /// 3 (o monitor e a câmera na linha 1), a recarga fora da thread aplicou a revisão 4 (a câmera
    /// saiu, a linha 1 é outro monitor), e o clique na linha 1 chegou antes do pulso. Sem a revisão,
    /// o índice 1 escolhia o outro monitor.
    #[test]
    fn revisao_curta_a1_o_clique_de_outra_lista_nao_escolhe() {
        assert_eq!(escolha_do_clique(1, 3, 4, 2, None), None, "a lista mudou debaixo do clique");
        assert_eq!(escolha_do_clique(1, 4, 4, 2, None), Some(1));
        assert_eq!(escolha_do_clique(1, 4, 4, 2, Some(1)), None, "a mesma de novo não muda nada");
        assert_eq!(escolha_do_clique(2, 4, 4, 2, None), None, "fora da lista");
        assert_eq!(escolha_do_clique(0, 4, 4, 2, Some(1)), Some(0));
    }

    /// **A lista velha que chega depois** (A2): a volta à tela inicial enumerou (geração 7) com a
    /// câmera ainda na lista e demorou; o `WM_DEVICECHANGE` da câmera saindo enumerou depois (8) e
    /// aplicou antes. Sem a geração, a lista 7 punha a câmera que saiu de volta no seletor.
    #[test]
    fn revisao_curta_a2_a_lista_velha_nao_volta() {
        let mut aplicada = 0u64;
        let mut aplicar = |g: u64| {
            let ok = lista_mais_nova(g, aplicada);
            if ok {
                aplicada = g;
            }
            ok
        };
        assert!(aplicar(8), "a do WM_DEVICECHANGE, que terminou primeiro");
        assert!(!aplicar(7), "a da volta ao início, mais velha, chegou depois");
        assert!(!aplicar(8), "a mesma geração não se aplica duas vezes");
        assert!(aplicar(9));
    }

    /// **Os pedidos juntos** (A3): dez `WM_DEVICECHANGE` de uma câmera que entra sobem uma thread
    /// só, que dá uma volta a mais por conta dos que chegaram durante a primeira; o pedido que
    /// chega depois de ela sair sobe outra.
    #[test]
    fn revisao_curta_a3_os_pedidos_se_juntam() {
        let j = Juntador::novo();
        assert!(j.pedir(), "o primeiro sobe a thread");
        let mut voltas = 0;
        loop {
            j.comecar_volta();
            voltas += 1;
            if voltas == 1 {
                for _ in 0..9 {
                    assert!(!j.pedir(), "com uma rodando, ninguém sobe outra");
                }
            }
            if !j.mais_uma_volta() {
                break;
            }
        }
        assert_eq!(voltas, 2, "uma volta pelos pedidos do meio, e só uma");
        assert!(!j.rodando());
        assert!(j.pedir(), "depois de sair, o próximo sobe outra");
        j.comecar_volta();
        assert!(!j.mais_uma_volta());
        // A thread que não subiu solta a vez, e o seguinte pode subir.
        assert!(j.pedir());
        j.soltar();
        assert!(j.pedir());
    }

    /// **O pedido que cai na janela da saída** (a revisão do `ef71d20`, M1). A thread que roda
    /// viu o pendente limpo e vai soltar a vez; o pedido chega ali. Antes de soltar: o pedido não
    /// pega a vez, e a que roda tem de retomar ("sem a segunda checagem" cai aqui). Depois de
    /// soltar: o pedido pega a vez e sobe outra thread, e a que roda tem de sair ("sem o CAS na
    /// retomada", que dava duas voltas juntas, cai aqui).
    #[test]
    fn revisao_ef71d20_o_pedido_na_janela_da_saida() {
        let j = Juntador::novo();
        assert!(j.pedir());
        j.comecar_volta();
        let mut subiu = None;
        assert!(j.mais_uma_volta_com(|| subiu = Some(j.pedir()), || {}), "a que roda retoma");
        assert_eq!(subiu, Some(false), "com uma rodando, o pedido não sobe outra");
        j.comecar_volta();
        let mut subiu = None;
        assert!(!j.mais_uma_volta_com(|| {}, || subiu = Some(j.pedir())), "a que roda sai");
        assert_eq!(subiu, Some(true), "o pedido pega a vez que ela soltou");
        assert!(j.rodando());
    }

    /// **A saída no meio do pedido** (M1): o pedido marcou o pendente e ainda não tentou a vez; a
    /// thread que roda termina a volta ali. Ou ela retoma, ou o pedido sobe outra: o pedido nunca
    /// fica sem ninguém ("o CAS antes do pendente" cai aqui).
    #[test]
    fn revisao_ef71d20_a_saida_no_meio_do_pedido() {
        let j = Juntador::novo();
        assert!(j.pedir());
        j.comecar_volta();
        let mut retomou = None;
        let subiu = j.pedir_com(|| retomou = Some(j.mais_uma_volta()));
        assert!(subiu || retomou == Some(true), "o pedido ficou sem ninguém: subiu={subiu} retomou={retomou:?}");
        assert!(j.rodando());
    }

    /// **O pânico na thread da recarga** (a revisão do `ef71d20`, L1): a guarda solta a vez, e o
    /// pedido seguinte sobe outra thread. Sem a guarda, o juntador ficava em "rodando" e todo pedido
    /// seguinte voltava calado.
    #[test]
    fn revisao_ef71d20_o_panico_na_recarga_solta_a_vez() {
        let j = std::sync::Arc::new(Juntador::novo());
        assert!(j.pedir());
        let na_thread = j.clone();
        let fim = std::thread::spawn(move || {
            let _vez = na_thread.vez();
            na_thread.comecar_volta();
            panic!("a enumeração morreu (de propósito, no teste)");
        })
        .join();
        assert!(fim.is_err(), "a thread morreu em pânico");
        assert!(!j.rodando(), "a guarda soltou a vez");
        assert!(j.pedir(), "o pedido seguinte sobe outra thread");
        // Na saída normal a guarda não mexe: quem solta é o `mais_uma_volta`, e a vez é da thread
        // nova até ela sair.
        {
            let _vez = j.vez();
            j.comecar_volta();
            assert!(!j.mais_uma_volta());
            assert!(j.pedir(), "o pedido depois da saída sobe outra");
        }
        assert!(j.rodando(), "a guarda sem pânico não solta a vez de ninguém");
    }

    /// O juntador sob threads de verdade: muitos pedidos de muitas threads, e nunca duas voltas ao
    /// mesmo tempo; o último pedido é sempre atendido por uma volta que começa depois dele. **É
    /// fumaça, e não prova** (a revisão do `ef71d20`, M1): a janela da perda são duas instruções, e o
    /// pedido que cai nela quase nunca é o último. A prova são os dois testes acima.
    #[test]
    fn revisao_curta_a3_o_juntador_com_threads() {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
        use std::sync::Arc;
        let j = Arc::new(Juntador::novo());
        let ao_mesmo_tempo = Arc::new(AtomicUsize::new(0));
        let pior = Arc::new(AtomicUsize::new(0));
        let pedidos = Arc::new(AtomicU64::new(0));
        let atendido = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let (j, ao_mesmo_tempo, pior, pedidos, atendido) =
                (j.clone(), ao_mesmo_tempo.clone(), pior.clone(), pedidos.clone(), atendido.clone());
            threads.push(std::thread::spawn(move || {
                let mut filhas = Vec::new();
                for _ in 0..200 {
                    pedidos.fetch_add(1, Ordering::SeqCst);
                    if j.pedir() {
                        let (j, ao_mesmo_tempo, pior, pedidos, atendido) =
                            (j.clone(), ao_mesmo_tempo.clone(), pior.clone(), pedidos.clone(), atendido.clone());
                        filhas.push(std::thread::spawn(move || loop {
                            j.comecar_volta();
                            let visto = pedidos.load(Ordering::SeqCst);
                            let n = ao_mesmo_tempo.fetch_add(1, Ordering::SeqCst) + 1;
                            pior.fetch_max(n, Ordering::SeqCst);
                            std::thread::yield_now();
                            ao_mesmo_tempo.fetch_sub(1, Ordering::SeqCst);
                            atendido.fetch_max(visto, Ordering::SeqCst);
                            if !j.mais_uma_volta() {
                                break;
                            }
                        }));
                    }
                }
                for f in filhas {
                    f.join().unwrap();
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(pior.load(Ordering::SeqCst), 1, "nunca duas voltas ao mesmo tempo");
        assert_eq!(atendido.load(Ordering::SeqCst), pedidos.load(Ordering::SeqCst), "o último pedido foi atendido");
        assert!(!j.rodando());
    }

    // Os três links **reais** do Dell de 18/09 (`quall_camera_local listar`, M5) e o formato de uma
    // câmera do Quall (a câmera de bancada criada pela sonda na mesma data).
    const WEBCAM: &str = r"\\?\usb#vid_0bda&pid_5521&mi_00#6&b813745&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
    const CANON: &str = r"\\?\root#eoswebcamsource#0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\eoswebcamcamerasource";
    const S24: &str = r"\\?\swd#vcamdevapi#0a8f5aa123254a5cabf62054d76f2f85bfe491b934261ba5e7705124a75e6396#{e5323777-f976-4f5b-9b55-b94699c46e44}\{fcebba03-9d13-4c13-9940-cc84fcd132d1}";
    const DO_QUALL: &str = r"\\?\swd#vcamdevapi#cb1a50a125506b5ad03004ff51fa7a77228fb37a9ec930ddbcc5b8d7e9b16aec#{e5323777-f976-4f5b-9b55-b94699c46e44}\{fcebba03-9d13-4c13-9940-cc84fcd132d1}";

    #[test]
    fn m55_o_catalogo_vai_ao_registro_so_quando_muda() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        let primeira = l(&["câmera A ENTRA", "câmera B FORA", "--camera-de-bancada RECUSADA: o PID 5644"]);
        // A primeira vez: todas.
        assert_eq!(linhas_que_mudaram(&[], &primeira), primeira);
        // A espera do R5: 38 releituras iguais, nenhuma linha.
        for _ in 0..38 {
            assert!(linhas_que_mudaram(&primeira, &primeira).is_empty());
        }
        // A sonda aparece liberada: a linha nova e a recusa que saiu.
        let depois = l(&["câmera A ENTRA", "câmera B FORA", "câmera C ENTRA — câmera de bancada liberada"]);
        assert_eq!(
            linhas_que_mudaram(&primeira, &depois),
            l(&["câmera C ENTRA — câmera de bancada liberada", "saiu do catálogo: --camera-de-bancada RECUSADA: o PID 5644"])
        );
    }

    #[test]
    fn os_links_reais_se_separam() {
        let p = partes_do_link(S24).unwrap();
        assert!(p.virtual_do_mf);
        assert_eq!(p.classe, "{e5323777-f976-4f5b-9b55-b94699c46e44}");
        assert_eq!(p.referencia.as_deref(), Some("{fcebba03-9d13-4c13-9940-cc84fcd132d1}"));
        assert!(p.interface.starts_with("swd#vcamdevapi#0a8f5aa1"));
        assert!(!partes_do_link(WEBCAM).unwrap().virtual_do_mf);
        assert!(!partes_do_link(CANON).unwrap().virtual_do_mf);
        assert_eq!(partes_do_link(WEBCAM).unwrap().referencia.as_deref(), Some("global"));
    }

    #[test]
    fn a_chave_do_dono_e_a_que_o_inventario_de_27_08_achou() {
        let c = chaves_do_dono(&partes_do_link(S24).unwrap());
        assert_eq!(
            c[0],
            r"SYSTEM\CurrentControlSet\Control\DeviceClasses\{e5323777-f976-4f5b-9b55-b94699c46e44}\##?#swd#vcamdevapi#0a8f5aa123254a5cabf62054d76f2f85bfe491b934261ba5e7705124a75e6396#{e5323777-f976-4f5b-9b55-b94699c46e44}\#{fcebba03-9d13-4c13-9940-cc84fcd132d1}\Device Parameters"
        );
        // A referência do link é a das câmeras virtuais: não repete a mesma chave em outra caixa.
        assert_eq!(c.len(), 2);
        assert!(c[1].ends_with(r"\#\Device Parameters"));
    }

    #[test]
    fn link_sem_forma_nao_e_reconhecido() {
        assert!(partes_do_link("").is_none());
        assert!(partes_do_link(r"\\?\usb#sem-classe").is_none());
        assert!(partes_do_link(r"\\?\a#{nao-e-guid}\x").is_none());
    }

    #[test]
    fn guid_compara_como_numero() {
        let a = guid("{5C75FE52-9204-45F6-B143-58B1AC8048E5}").unwrap();
        assert_eq!(a, CLSID_DA_FONTE_DO_QUALL);
        assert_eq!(guid("5c75fe52-9204-45f6-b143-58b1ac8048e5"), Some(a));
        assert_eq!(guid("  {5c75fe52-9204-45f6-b143-58b1ac8048e5} "), Some(a));
        assert!(guid("{5C75FE52-9204-45F6-B143-58B1AC8048E}").is_none());
        assert!(guid("{5C75FE52920445F6B14358B1AC8048E5}").is_none());
        assert!(guid("{GC75FE52-9204-45F6-B143-58B1AC8048E5}").is_none());
    }

    #[test]
    fn a_camera_do_quall_fica_de_fora_em_qualquer_caixa() {
        for v in [
            "{5C75FE52-9204-45F6-B143-58B1AC8048E5}",
            "{5c75fe52-9204-45f6-b143-58b1ac8048e5}",
            "5C75FE52-9204-45F6-B143-58B1AC8048E5",
        ] {
            let d = classificar(DO_QUALL, Some(&Leitura::Valor(v.into())));
            assert_eq!(d, Dono::Quall, "{v}");
            assert!(!d.vai_para_o_seletor());
        }
    }

    #[test]
    fn a_camera_conectada_do_s24_entra() {
        let d = classificar(S24, Some(&Leitura::Valor("{E9F83CF2-E0C0-4CA7-AF01-E90C70BEF496}".into())));
        assert_eq!(d, Dono::OutroDono("{E9F83CF2-E0C0-4CA7-AF01-E90C70BEF496}".into()));
        assert!(d.vai_para_o_seletor());
    }

    #[test]
    fn nao_ler_o_dono_esconde_e_nunca_vira_nao_virtual() {
        let falhou = classificar(S24, Some(&Leitura::Falhou("acesso negado (5)".into())));
        assert!(matches!(falhou, Dono::Ilegivel(_)));
        assert!(!falhou.vai_para_o_seletor());
        assert!(!classificar(S24, None).vai_para_o_seletor());
        assert!(!classificar(S24, Some(&Leitura::SemValor)).vai_para_o_seletor());
        assert!(!classificar(S24, Some(&Leitura::Valor("lixo".into()))).vai_para_o_seletor());
        assert!(!classificar("não é link", None).vai_para_o_seletor());
    }

    #[test]
    fn webcam_e_canon_entram_sem_o_valor() {
        assert_eq!(classificar(WEBCAM, None), Dono::NaoVirtual);
        assert_eq!(classificar(WEBCAM, Some(&Leitura::SemValor)), Dono::NaoVirtual);
        assert_eq!(classificar(CANON, Some(&Leitura::Falhou("acesso negado (5)".into()))), Dono::NaoVirtual);
        assert!(classificar(WEBCAM, None).vai_para_o_seletor());
    }

    #[test]
    fn o_nosso_clsid_fora_de_vcamdevapi_tambem_fica_de_fora() {
        // Um enumerador que não é `SWD#VCAMDEVAPI#` (a forma hipotética da revisão de código) e a
        // própria webcam, com o nosso CLSID no registro: fora, como qualquer câmera do Quall.
        let outro_enumerador =
            r"\\?\swd#sgdevapi#0a8f5aa1#{e5323777-f976-4f5b-9b55-b94699c46e44}\{fcebba03-9d13-4c13-9940-cc84fcd132d1}";
        for link in [outro_enumerador, WEBCAM] {
            let d = classificar(link, Some(&Leitura::Valor("{5c75fe52-9204-45f6-b143-58b1ac8048e5}".into())));
            assert_eq!(d, Dono::Quall, "{link}");
            assert!(!d.vai_para_o_seletor());
        }
        // Outro CLSID fora de `VCAMDEVAPI`: entra.
        let outro = classificar(CANON, Some(&Leitura::Valor("{E9F83CF2-E0C0-4CA7-AF01-E90C70BEF496}".into())));
        assert!(outro.vai_para_o_seletor());
    }

    fn lista() -> Vec<Candidata<'static>> {
        vec![
            Candidata { id: r"\\.\DISPLAY1", nome: "Generic PnP Monitor", monitor: true },
            Candidata { id: r"\\.\DISPLAY4", nome: "LG TV", monitor: true },
            Candidata { id: WEBCAM, nome: "Integrated Webcam", monitor: false },
        ]
    }

    #[test]
    fn fonte_que_nao_casa_e_erro_e_nunca_o_monitor_principal() {
        let l = lista();
        assert!(escolher_inicial(&l, Some("Câmera que não existe")).is_err());
        assert!(escolher_inicial(&l, Some(r"\\.\DISPLAY9")).is_err());
        // Casamento exato, não trecho: "Integrated" não é "Integrated Webcam".
        assert!(escolher_inicial(&l, Some("Integrated")).is_err());
    }

    #[test]
    fn fonte_que_casa_por_id_ou_nome() {
        let l = lista();
        assert_eq!(escolher_inicial(&l, None), Ok(Some(0)));
        assert_eq!(escolher_inicial(&l, Some("lg tv")), Ok(Some(1)));
        assert_eq!(escolher_inicial(&l, Some(&WEBCAM.to_uppercase())), Ok(Some(2)));
        assert_eq!(escolher_inicial(&l, Some("tela")), Ok(Some(0)));
        assert_eq!(escolher_inicial(&[], None), Ok(None));
    }

    #[test]
    fn fase4_a_camera_de_bancada_so_com_a_sonda_viva() {
        let sonda = |pid: u32| (pid == 4242).then(|| "quall_camera_local.exe".to_string());
        let quall_exe = |pid: u32| (pid == 4242).then(|| "Quall.exe".to_string());
        let morto = |_: u32| None;
        let nome = "Quall-bancada sonda 4242 (Câmera Virtual do Windows)";
        assert_eq!(conferir_camera_de_bancada(nome, &Dono::Quall, &sonda), Ok(4242));
        assert_eq!(conferir_camera_de_bancada("Quall-bancada sonda 4242", &Dono::Quall, &sonda), Ok(4242));
        // O PID é de outro programa, ou de ninguém.
        assert!(conferir_camera_de_bancada(nome, &Dono::Quall, &quall_exe).unwrap_err().contains("Quall.exe"));
        assert!(conferir_camera_de_bancada(nome, &Dono::Quall, &morto).unwrap_err().contains("não é de um processo vivo"));
        // Uma baia do Quall.exe do usuário: o nome de um aparelho.
        assert!(conferir_camera_de_bancada("SM-S928B (Câmera Virtual do Windows)", &Dono::Quall, &sonda).is_err());
        // Outro PID no fim do nome não casa com a sonda de 4242.
        assert!(conferir_camera_de_bancada("Quall-bancada sonda 42420", &Dono::Quall, &sonda).is_err());
        // Não é do Quall: a bandeira não libera nada.
        assert!(conferir_camera_de_bancada(nome, &Dono::NaoVirtual, &sonda).is_err());
        assert!(conferir_camera_de_bancada(nome, &Dono::OutroDono("{E9F83CF2}".into()), &sonda).is_err());
    }

    #[test]
    fn m4_camera_nunca_e_padrao() {
        // A revisão do código da fase 3: sem `--fonte` e sem monitor, o padrão era a câmera.
        let so_cameras = [
            Candidata { id: WEBCAM, nome: "Integrated Webcam", monitor: false },
            Candidata { id: r"\\?\swd#vcamdevapi#0a8f#{e5323777-f976-4f5b-9b55-b94699c46e44}\{fcebba03}", nome: "S24", monitor: false },
        ];
        assert_eq!(escolher_inicial(&so_cameras, None), Ok(None));
        // Uma câmera antes do monitor (não acontece hoje: a lista põe os monitores antes): o monitor.
        let camera_primeiro = [
            Candidata { id: WEBCAM, nome: "Integrated Webcam", monitor: false },
            Candidata { id: r"\\.\DISPLAY1", nome: "Generic PnP Monitor", monitor: true },
        ];
        assert_eq!(escolher_inicial(&camera_primeiro, None), Ok(Some(1)));
        // A Sessão 0 do SSH tem um monitor: a área de 1024x768, com o id "WinDisc" (lido em 18/09).
        let sessao_0 = [
            Candidata { id: "WinDisc", nome: "Monitor", monitor: true },
            Candidata { id: WEBCAM, nome: "Integrated Webcam", monitor: false },
        ];
        assert_eq!(escolher_inicial(&sessao_0, None), Ok(Some(0)));
        // Pedida pelo nome, a câmera continua escolhível: só o padrão mudou.
        assert_eq!(escolher_inicial(&so_cameras, Some("Integrated Webcam")), Ok(Some(0)));
    }

    #[test]
    fn tela_sem_monitor_e_erro() {
        let so_camera = [Candidata { id: WEBCAM, nome: "Integrated Webcam", monitor: false }];
        assert!(escolher_inicial(&so_camera, Some("tela")).is_err());
    }

    #[test]
    fn a_fonte_que_some_deixa_sem_escolha_com_aviso() {
        let l = lista();
        // A webcam saiu: sem escolha, e o aviso com o nome dela.
        let sem_webcam = &l[..2];
        match reescolher(Some((WEBCAM, "Integrated Webcam")), None, sem_webcam) {
            Reescolha::Sumiu { aviso } => assert!(aviso.contains("Integrated Webcam")),
            outra => panic!("{outra:?}"),
        }
        // O primeiro monitor saiu: a LG TV continua escolhida no índice novo, sem aviso.
        let sem_o_primeiro = &l[1..];
        assert_eq!(reescolher(Some((r"\\.\DISPLAY4", "LG TV")), None, sem_o_primeiro), Reescolha::Ficou(Some(0)));
        // Sem escolha antes e nada sumido: continua sem escolha.
        assert_eq!(reescolher(None, None, &l), Reescolha::Ficou(None));
    }

    #[test]
    fn a_tv_que_sai_e_volta_volta_escolhida() {
        let l = lista();
        let sem_tv = [l[0], l[2]];
        // 1. A TV escolhida sai: sem escolha, com aviso; quem chama guarda o id dela.
        assert!(matches!(reescolher(Some((r"\\.\DISPLAY4", "LG TV")), None, &sem_tv), Reescolha::Sumiu { .. }));
        let sumida = Some((r"\\.\DISPLAY4", "LG TV"));
        // 2. Outra mudança sem a TV: continua sem escolha (nunca o monitor principal).
        assert_eq!(reescolher(None, sumida, &sem_tv), Reescolha::Ficou(None));
        // 3. A TV volta, pelo mesmo id (em outra caixa) e com o mesmo nome: escolhida de novo.
        assert_eq!(reescolher(None, Some((r"\\.\display4", "LG TV")), &l), Reescolha::Voltou(1));
    }

    #[test]
    fn um_projetor_na_porta_da_tv_nao_volta_escolhido() {
        // `\\.\DISPLAY4` é da saída de vídeo: o projetor que entra na porta da TV herda o id.
        let l = lista();
        let com_projetor = [l[0], Candidata { id: r"\\.\DISPLAY4", nome: "EPSON PJ", monitor: true }, l[2]];
        assert_eq!(reescolher(None, Some((r"\\.\DISPLAY4", "LG TV")), &com_projetor), Reescolha::Ficou(None));
    }

    #[test]
    fn o_mesmo_nome_com_outro_id_tira_o_aviso_sem_escolher() {
        // O Sidecar do Mac volta com outro id (`tela:64` → `tela:66`, registro de 10/09); no Windows,
        // o nome GDI de um monitor pode mudar. Não se escolhe (pode ser outro aparelho igual), mas o
        // aviso "não está mais disponível" seria falso com o nome na lista.
        let l = lista();
        let outra_tv = [l[0], Candidata { id: r"\\.\DISPLAY7", nome: "LG TV", monitor: true }];
        assert_eq!(reescolher(None, Some((r"\\.\DISPLAY4", "LG TV")), &outra_tv), Reescolha::Reapareceu);
    }

    #[test]
    fn o_motivo_do_fim_fica_quando_o_aviso_sai() {
        let motivo = "O monitor \"LG TV\" foi desconectado.";
        let aviso = "\"LG TV\" não está mais disponível. Escolha outra fonte.";
        // O fim da sessão escreveu o motivo, e o aviso veio depois: os dois na tela.
        let junto = com_o_aviso(motivo, aviso);
        assert_eq!(junto, format!("{motivo} {aviso}"));
        // A TV volta: sai só o aviso.
        assert_eq!(sem_o_aviso(&junto, aviso), motivo);
        // O aviso sozinho sai todo.
        assert_eq!(sem_o_aviso(&com_o_aviso("", aviso), aviso), "");
        // O motivo chegou depois e escreveu por cima do aviso: nada a tirar, o motivo fica.
        assert_eq!(sem_o_aviso("A captura parou sozinha.", aviso), "A captura parou sozinha.");
    }

    #[test]
    fn um_monitor_so_que_some_e_volta() {
        // O desktop com DisplayPort que "desconecta" ao dormir: a lista fica vazia e volta.
        let painel = [Candidata { id: r"\\.\DISPLAY1", nome: "Generic PnP Monitor", monitor: true }];
        assert!(matches!(
            reescolher(Some((r"\\.\DISPLAY1", "Generic PnP Monitor")), None, &[]),
            Reescolha::Sumiu { .. }
        ));
        assert_eq!(
            reescolher(None, Some((r"\\.\DISPLAY1", "Generic PnP Monitor")), &painel),
            Reescolha::Voltou(0)
        );
    }

    #[test]
    fn nomes_repetidos_ganham_a_instancia_e_todos_ganham() {
        let a = r"\\?\usb#vid_046d&pid_082d&mi_00#6&b813745&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        let b = r"\\?\usb#vid_046d&pid_082d&mi_00#6&2c0a9e1&0&0000#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        assert_eq!(instancia_curta(a), "6&b813745&0&0000");
        assert_eq!(instancia_curta(S24), "5124a75e6396");
        // Webcams não compostas: a instância é o número de série, com o prefixo do lote em comum.
        let s1 = r"\\?\usb#vid_1234&pid_0001#SN2024A00017#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        let s2 = r"\\?\usb#vid_1234&pid_0001#SN2024A00042#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        assert_ne!(instancia_curta(s1), instancia_curta(s2));
        let itens = vec![
            ("HD Pro Webcam C920".to_string(), instancia_curta(a)),
            ("HD Pro Webcam C920".to_string(), instancia_curta(b)),
            ("Integrated Webcam".to_string(), instancia_curta(WEBCAM)),
        ];
        let nomes = desempatar(&itens);
        assert_eq!(nomes[0], "HD Pro Webcam C920 (6&b813745&0&0000)");
        assert_eq!(nomes[1], "HD Pro Webcam C920 (6&2c0a9e1&0&0000)");
        assert_eq!(nomes[2], "Integrated Webcam");
        // Três iguais: três linhas diferentes.
        let tres: Vec<(String, String)> =
            ["1", "2", "3"].iter().map(|s| ("X".to_string(), s.to_string())).collect();
        assert_eq!(desempatar(&tres), vec!["X (1)", "X (2)", "X (3)"]);
    }

    #[test]
    fn a_camera_de_bancada_pelo_nome_exato() {
        let pedido = nome_da_camera_de_bancada("Quall-bancada", 1234);
        assert_eq!(pedido, "Quall-bancada sonda 1234");
        assert!(e_o_nome_pedido("Quall-bancada sonda 1234", &pedido));
        assert!(e_o_nome_pedido("Quall-bancada sonda 1234 (Câmera Virtual do\u{a0}Windows)", &pedido));
        // A câmera de outra sonda, com um PID que tem o nosso como prefixo, não é a nossa.
        assert!(!e_o_nome_pedido("Quall-bancada sonda 12345 (Câmera Virtual do Windows)", &pedido));
        assert!(!e_o_nome_pedido("Quall-bancada sonda 12345", &pedido));
    }

    #[test]
    fn o_pid_so_sai_do_nome_de_uma_camera_de_bancada() {
        // O nome enumerado da corrida positiva de 18/09 (M27).
        assert_eq!(pid_da_camera_de_bancada("Quall-bancada sonda 14384 (Câmera Virtual do\u{a0}Windows)"), Some(14384));
        assert_eq!(pid_da_camera_de_bancada("Quall-bancada sonda 14384"), Some(14384));
        // As baias do `Quall.exe`: nome de aparelho.
        assert_eq!(pid_da_camera_de_bancada("SM-S928B (Câmera Virtual do Windows)"), None);
        assert_eq!(pid_da_camera_de_bancada("MacBook Air de Bruno"), None);
        assert_eq!(pid_da_camera_de_bancada("X sonda "), None);
        assert_eq!(pid_da_camera_de_bancada("X sonda 12a"), None);
        assert_eq!(pid_da_camera_de_bancada("X sonda 12 depois"), None);
    }
}
