//! **A decisão de pôr um monitor virtual na área de trabalho**, pura: sem Windows, sem driver, sem
//! relógio. É a receita do §13.3–§13.4 de `docs/monitor-virtual-windows.md` ("o pedido limpo, na
//! hora"), provada 220 de 220 pela sonda `receita_monitor` (`telas::montar_limpo`), portada para cá
//! com os caminhos do `DisplayConfig` trocados por uma tabela de [`Caminho`]s — o que deixa as regras
//! serem testadas com tabelas sintéticas, sem aparelho.
//!
//! Quem lê o Windows e aplica o pedido é `monitores_virtuais.rs`; aqui só se decide.
//!
//! # As regras, na ordem em que valem
//!
//! 1. **A foto da tela do usuário** ([`separar_foto`]): no instante do `ADD`, os caminhos ativos com
//!    o alvo disponível que não são nossos — nem do adaptador virtual, nem um monitor do SudoVDA
//!    visto por outro adaptador (o clone, reconhecido pelo `monitorDevicePath` `SMKD1CE`). Na
//!    dúvida (o `GET_TARGET_NAME` não respondeu) o caminho **fica** na foto: a dúvida fica do lado de
//!    manter a tela da pessoa.
//! 2. **Só pedir com todos os alvos da foto ainda ativos** ([`decidir`]): um pedido sem um deles
//!    apagaria aquela tela. Foto vazia: nunca pedir.
//! 3. **Esperar diante de um caminho ativo que não é da foto nem nosso**: pode ser um monitor que a
//!    pessoa acabou de ligar, e o pedido o desligaria.
//! 4. **Desfazer o clone**: um monitor nosso ativo por outro adaptador (o Windows o pôs em clone com
//!    a tela integrada, §13.1) — o pedido vai só com a foto e os nossos do adaptador virtual; na
//!    volta seguinte, estender.
//! 5. **Estender**: a foto + os nossos já ativos (com os modos de agora) + um caminho novo para cada
//!    alvo nosso que falta — o primeiro com o alvo disponível e a fonte livre. Sem caminho para o
//!    **novo** (o primeiro alvo), esperar; para os outros, ficam de fora nesta volta.
//! 6. **Pronto** só quando **todos** os nossos (o novo e os que já estavam) estão ativos: a chegada
//!    do 2º tirou o 1º da área de trabalho quando o Windows agiu sozinho (E4, §13.5).
//!
//! Nunca se devolve à `SetDisplayConfig` o que o `QDC_ALL_PATHS` marca ativo sem filtrar, e nunca
//! `SDC_SAVE_TO_DATABASE` — isso é de quem aplica, e está escrito lá.

/// Um par (adaptador, id) do `DisplayConfig`: a fonte ou o alvo de um caminho. O LUID leva o
/// `HighPart` nos 32 bits de cima.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Par {
    pub adaptador: u64,
    pub id: u32,
}

impl Par {
    pub const fn novo(adaptador: u64, id: u32) -> Par {
        Par { adaptador, id }
    }
}

/// De quem é o monitor no alvo de um caminho, pelo `monitorDevicePath` que o `GET_TARGET_NAME` dá.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dono {
    /// Um monitor que não é do SudoVDA.
    Usuario,
    /// Um monitor do SudoVDA (`\\?\DISPLAY#SMKD1CE#…`), por qualquer adaptador.
    Nosso,
    /// O `GET_TARGET_NAME` falhou ou veio sem caminho: não dá para dizer.
    Desconhecido,
}

/// Um caminho do `DisplayConfig`, só com o que as regras usam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caminho {
    pub fonte: Par,
    pub alvo: Par,
    /// `targetAvailable`: um caminho velho (o monitor que acabou de sair) vem com `false`.
    pub disponivel: bool,
    /// Só importa nos caminhos ativos; nos do `QDC_ALL_PATHS` pode ir `Desconhecido`.
    pub dono: Dono,
}

/// Por que um caminho ativo ficou fora da foto do usuário.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fora {
    /// `targetAvailable = 0`: o caminho velho de um monitor que saiu.
    Velho,
    /// Um monitor do adaptador virtual.
    Virtual,
    /// Um monitor do SudoVDA ativo por outro adaptador: o clone do Windows.
    CloneNosso,
}

/// A foto: quais caminhos ativos são da pessoa (índices em `ativos`) e quais ficaram de fora.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Foto {
    pub usuario: Vec<usize>,
    pub fora: Vec<(usize, Fora)>,
}

/// A regra 1: separa, dos caminhos ativos de agora, os da pessoa.
pub fn separar_foto(ativos: &[Caminho], adaptador_virtual: u64) -> Foto {
    let mut f = Foto::default();
    for (i, p) in ativos.iter().enumerate() {
        if !p.disponivel {
            f.fora.push((i, Fora::Velho));
        } else if p.alvo.adaptador == adaptador_virtual {
            f.fora.push((i, Fora::Virtual));
        } else if p.dono == Dono::Nosso {
            f.fora.push((i, Fora::CloneNosso));
        } else {
            f.usuario.push(i);
        }
    }
    f
}

/// O que fazer nesta volta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decisao {
    /// Todos os nossos estão ativos e disponíveis.
    Pronto,
    /// Nada a pedir agora; o texto diz por quê.
    Esperar(String),
    /// Pedir a foto + `virtuais` (índices em `ativos`), sem os `clones` (índices em `ativos`).
    DesfazerClone { virtuais: Vec<usize>, clones: Vec<usize> },
    /// Pedir a foto + `virtuais` (índices em `ativos`) + um caminho novo para cada `novos` (índices
    /// em `todos`, com os índices de modo inválidos: o Windows escolhe modo e posição).
    Estender { virtuais: Vec<usize>, novos: Vec<usize> },
}

/// As regras 2 a 6.
///
/// - `alvos`: os nossos que têm de estar na área de trabalho — **o novo primeiro**, depois os que
///   já estavam;
/// - `foto`: os caminhos da foto do usuário, como estavam no `ADD`;
/// - `ativos`: o `QDC_ONLY_ACTIVE_PATHS` de agora, com o [`Dono`] de cada um;
/// - `todos`: o `QDC_ALL_PATHS` de agora (só serve para achar o caminho livre).
pub fn decidir(alvos: &[Par], adaptador_virtual: u64, foto: &[Caminho], ativos: &[Caminho], todos: &[Caminho]) -> Decisao {
    if alvos.is_empty() {
        return Decisao::Pronto;
    }
    let ativo_agora = |a: Par| ativos.iter().any(|p| p.disponivel && p.alvo == a);
    if alvos.iter().all(|a| ativo_agora(*a)) {
        return Decisao::Pronto;
    }
    if foto.is_empty() {
        return Decisao::Esperar("a foto do usuário não tem caminho nenhum: não peço, para não tirar a tela da pessoa".into());
    }
    let faltando: Vec<String> = foto.iter().filter(|f| !ativo_agora(f.alvo)).map(|f| curto(f)).collect();
    if !faltando.is_empty() {
        return Decisao::Esperar(format!(
            "caminho(s) da foto do usuário não ativo(s) agora: [{}] — não peço",
            faltando.join(" | ")
        ));
    }
    let na_foto = |a: Par| foto.iter().any(|f| f.alvo == a);
    let (mut virtuais, mut clones, mut desconhecidos) = (Vec::new(), Vec::new(), Vec::new());
    for (i, p) in ativos.iter().enumerate() {
        if !p.disponivel || na_foto(p.alvo) {
            continue;
        }
        if p.alvo.adaptador == adaptador_virtual {
            virtuais.push(i);
        } else if p.dono == Dono::Nosso {
            clones.push(i);
        } else {
            desconhecidos.push(i);
        }
    }
    if !desconhecidos.is_empty() {
        return Decisao::Esperar(format!(
            "caminho ativo que não é da foto nem nosso: [{}] — não peço",
            desconhecidos.iter().map(|&i| curto(&ativos[i])).collect::<Vec<_>>().join(" | ")
        ));
    }
    if !clones.is_empty() {
        return Decisao::DesfazerClone { virtuais, clones };
    }
    let mut fontes: Vec<Par> = foto.iter().map(|f| f.fonte).chain(virtuais.iter().map(|&i| ativos[i].fonte)).collect();
    let mut novos = Vec::new();
    for (k, &alvo) in alvos.iter().enumerate() {
        if ativo_agora(alvo) {
            continue;
        }
        match todos.iter().position(|p| p.alvo == alvo && p.disponivel && !fontes.contains(&p.fonte)) {
            Some(j) => {
                fontes.push(todos[j].fonte);
                novos.push(j);
            }
            None if k == 0 => {
                return Decisao::Esperar(format!("alvo {} sem caminho disponível com fonte livre", alvo.id));
            }
            None => {}
        }
    }
    Decisao::Estender { virtuais, novos }
}

/// O alvo aparece em algum caminho com `targetAvailable` — a condição para tentar um pedido (o
/// Windows ainda não terminou de apresentá-lo antes disso).
pub fn alvo_disponivel(todos: &[Caminho], alvo: Par) -> bool {
    todos.iter().any(|p| p.alvo == alvo && p.disponivel)
}

/// O alvo tem caminho ativo (o que conta como "está na área de trabalho").
pub fn alvo_ativo(ativos: &[Caminho], alvo: Par) -> bool {
    ativos.iter().any(|p| p.alvo == alvo)
}

/// `fonte>alvo av=…` em poucas letras, para o registro.
pub fn curto(p: &Caminho) -> String {
    format!(
        "{:X}:{}>{:X}:{} av={}",
        p.fonte.adaptador as u32, p.fonte.id, p.alvo.adaptador as u32, p.alvo.id, p.disponivel as u8
    )
}

// ------------------------------------------------------------------------------------------------
// O relógio da ativação
// ------------------------------------------------------------------------------------------------

/// Os limites de uma ativação, pelo relógio (as `SetDisplayConfig` de até ~1 s contam).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limites {
    /// O prazo total por monitor (§13.4, item 5): 8 s.
    pub prazo_ms: u64,
    /// Entre dois pedidos, no mínimo: 50 ms (a sonda).
    pub entre_pedidos_ms: u64,
    /// Pedidos no máximo: 8 (a sonda).
    pub pedidos: u32,
}

impl Limites {
    pub const DA_RECEITA: Limites = Limites { prazo_ms: 8_000, entre_pedidos_ms: 50, pedidos: 8 };
}

/// Quantos pedidos saíram e quando, para decidir se o próximo pode sair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tentativas {
    pub feitas: u32,
    ultima_ms: Option<u64>,
}

impl Tentativas {
    /// Um pedido pode sair em `agora_ms` (ms desde o `ADD`)?
    pub fn pode_pedir(&self, agora_ms: u64, l: &Limites) -> bool {
        self.feitas < l.pedidos && self.ultima_ms.is_none_or(|u| agora_ms.saturating_sub(u) >= l.entre_pedidos_ms)
    }

    pub fn pediu(&mut self, agora_ms: u64) {
        self.feitas += 1;
        self.ultima_ms = Some(agora_ms);
    }
}

/// O prazo venceu? Pelo relógio, e não pela soma do que se dormiu.
pub fn prazo_vencido(agora_ms: u64, l: &Limites) -> bool {
    agora_ms >= l.prazo_ms
}

// ------------------------------------------------------------------------------------------------
// Os outros monitores durante a chegada de um novo (E4/E5)
// ------------------------------------------------------------------------------------------------

/// Quanto tempo cada um dos outros nossos passou fora da área de trabalho enquanto um novo chegava —
/// a medida que faltou em E4/E5 (§13.5), aqui de 10 em 10 ms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ausencias {
    /// (alvo, fora desde, total fora, maior trecho fora, vezes que saiu), em ms.
    pub por_alvo: Vec<(Par, Option<u64>, u64, u64, u32)>,
}

impl Ausencias {
    pub fn para(outros: &[Par]) -> Ausencias {
        Ausencias { por_alvo: outros.iter().map(|&a| (a, None, 0, 0, 0)).collect() }
    }

    /// Uma leitura em `agora_ms`: `ativo(alvo)` diz se ele tem caminho ativo agora.
    pub fn observar(&mut self, agora_ms: u64, ativo: impl Fn(Par) -> bool) {
        for (a, desde, total, maior, vezes) in self.por_alvo.iter_mut() {
            match (ativo(*a), *desde) {
                (true, Some(d)) => {
                    let trecho = agora_ms.saturating_sub(d);
                    *total += trecho;
                    *maior = (*maior).max(trecho);
                    *desde = None;
                }
                (false, None) => {
                    *desde = Some(agora_ms);
                    *vezes += 1;
                }
                _ => {}
            }
        }
    }

    /// Fecha os trechos em aberto em `agora_ms` (o fim da ativação).
    pub fn fechar(&mut self, agora_ms: u64) {
        self.observar(agora_ms, |_| true);
    }

    /// Algum dos outros saiu da área de trabalho?
    pub fn algum_saiu(&self) -> bool {
        self.por_alvo.iter().any(|x| x.4 > 0)
    }

    /// `nenhum` ou `alvo 258: fora 1x, 540 ms (maior 540 ms)`.
    pub fn linha(&self) -> String {
        let fora: Vec<String> = self
            .por_alvo
            .iter()
            .filter(|x| x.4 > 0)
            .map(|(a, _, total, maior, vezes)| format!("alvo {}: fora {vezes}x, {total} ms (maior {maior} ms)", a.id))
            .collect();
        if self.por_alvo.is_empty() {
            "não havia outro".into()
        } else if fora.is_empty() {
            format!("nenhum dos {} saiu", self.por_alvo.len())
        } else {
            fora.join("; ")
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Seguir o alvo, não o nome GDI (§13.4, item 6)
// ------------------------------------------------------------------------------------------------

/// O que a sessão faz com a captura depois de olhar o alvo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reabertura {
    /// A captura está no monitor do alvo.
    Manter,
    /// O alvo está ativo noutro `HMONITOR` (o nome GDI mudou quando outro monitor chegou), ou o item
    /// da captura fechou com o alvo ainda ativo: reabrir no `HMONITOR` novo.
    Reabrir,
    /// O alvo não tem caminho ativo agora: esperar (a tolerância da sessão decide o fim).
    Esperar,
}

/// `hmonitor_da_captura`: onde a captura aberta está; `hmonitor_do_alvo`: onde o alvo está agora
/// (`None` = sem caminho ativo); `item_fechado`: o `Closed` do item da captura disparou.
pub fn reabertura(hmonitor_da_captura: isize, hmonitor_do_alvo: Option<isize>, item_fechado: bool) -> Reabertura {
    match hmonitor_do_alvo {
        None => Reabertura::Esperar,
        Some(h) if h != hmonitor_da_captura || item_fechado => Reabertura::Reabrir,
        Some(_) => Reabertura::Manter,
    }
}

// ------------------------------------------------------------------------------------------------
// A soltura
// ------------------------------------------------------------------------------------------------

/// A saída testemunhada (§12.2 J, §13.4 item 7): o caminho ativo **e** o nó PnP. O `GET_TARGET_NAME`
/// não entra — ele continua respondendo pelo **conector** do adaptador depois de o monitor sair.
/// Leitura que falhou não é saída: `None` conta como "ainda lá".
pub fn saiu(caminho_ativo: Option<bool>, pnp_presente: Option<bool>) -> bool {
    caminho_ativo == Some(false) && pnp_presente == Some(false)
}

#[cfg(test)]
mod testes {
    use super::*;

    const INTEL: u64 = 0x0000_0000_0001_0717;
    const VIRTUAL: u64 = 0x0000_0000_0002_2981;
    const TELA: Par = Par::novo(INTEL, 265_988);

    fn c(fonte: (u64, u32), alvo: (u64, u32), disponivel: bool, dono: Dono) -> Caminho {
        Caminho { fonte: Par::novo(fonte.0, fonte.1), alvo: Par::novo(alvo.0, alvo.1), disponivel, dono }
    }

    /// A tela integrada do Dell: fonte 0 da Intel, alvo 265988.
    fn tela() -> Caminho {
        c((INTEL, 0), (INTEL, 265_988), true, Dono::Usuario)
    }

    /// Os caminhos do `QDC_ALL_PATHS` que o SudoVDA oferece para um alvo: fontes 0..9 do virtual.
    fn livres(alvo: u32, disponivel: bool) -> Vec<Caminho> {
        (0..10).map(|s| c((VIRTUAL, s), (VIRTUAL, alvo), disponivel, Dono::Desconhecido)).collect()
    }

    #[test]
    fn a_foto_so_leva_a_tela_da_pessoa() {
        let ativos = vec![
            tela(),
            c((VIRTUAL, 0), (VIRTUAL, 256), true, Dono::Nosso),
            // O clone: o monitor do SudoVDA como alvo da Intel (0x04000000 + conector).
            c((INTEL, 0), (INTEL, 0x0400_0101), true, Dono::Nosso),
            // O caminho velho do monitor que acabou de sair.
            c((VIRTUAL, 1), (VIRTUAL, 257), false, Dono::Desconhecido),
            // Um monitor cujo GET_TARGET_NAME não respondeu: a dúvida fica do lado da pessoa.
            c((INTEL, 1), (INTEL, 7), true, Dono::Desconhecido),
        ];
        let f = separar_foto(&ativos, VIRTUAL);
        assert_eq!(f.usuario, vec![0, 4]);
        assert_eq!(f.fora, vec![(1, Fora::Virtual), (2, Fora::CloneNosso), (3, Fora::Velho)]);
    }

    #[test]
    fn o_primeiro_monitor_estende_com_a_tela_da_pessoa() {
        let foto = vec![tela()];
        let ativos = vec![tela()];
        let novo = Par::novo(VIRTUAL, 256);
        let todos: Vec<Caminho> = std::iter::once(tela()).chain(livres(256, true)).collect();
        // O caminho livre é o primeiro com o alvo disponível e a fonte livre: índice 1 (fonte 0 do virtual).
        assert_eq!(decidir(&[novo], VIRTUAL, &foto, &ativos, &todos), Decisao::Estender { virtuais: vec![], novos: vec![1] });
    }

    #[test]
    fn sem_caminho_disponivel_para_o_novo_espera() {
        let foto = vec![tela()];
        let novo = Par::novo(VIRTUAL, 256);
        let todos: Vec<Caminho> = std::iter::once(tela()).chain(livres(256, false)).collect();
        assert!(matches!(decidir(&[novo], VIRTUAL, &foto, &[tela()], &todos), Decisao::Esperar(_)));
        assert!(!alvo_disponivel(&todos, novo));
    }

    #[test]
    fn foto_vazia_nunca_pede() {
        let novo = Par::novo(VIRTUAL, 256);
        let todos = livres(256, true);
        let d = decidir(&[novo], VIRTUAL, &[], &[], &todos);
        assert!(matches!(d, Decisao::Esperar(ref t) if t.contains("foto")), "{d:?}");
    }

    #[test]
    fn com_um_alvo_da_foto_fora_nao_pede() {
        // A pessoa tinha dois monitores; um deles sumiu dos ativos (ou ficou indisponível).
        let segundo = c((INTEL, 1), (INTEL, 7), true, Dono::Usuario);
        let foto = vec![tela(), segundo];
        let ativos = vec![tela(), c((INTEL, 1), (INTEL, 7), false, Dono::Usuario)];
        let novo = Par::novo(VIRTUAL, 256);
        let todos: Vec<Caminho> = ativos.iter().copied().chain(livres(256, true)).collect();
        assert!(matches!(decidir(&[novo], VIRTUAL, &foto, &ativos, &todos), Decisao::Esperar(ref t) if t.contains("foto")));
    }

    #[test]
    fn um_monitor_que_a_pessoa_acabou_de_ligar_faz_esperar() {
        let foto = vec![tela()];
        let ativos = vec![tela(), c((INTEL, 1), (INTEL, 9), true, Dono::Usuario)];
        let novo = Par::novo(VIRTUAL, 256);
        let todos: Vec<Caminho> = ativos.iter().copied().chain(livres(256, true)).collect();
        assert!(matches!(decidir(&[novo], VIRTUAL, &foto, &ativos, &todos), Decisao::Esperar(ref t) if t.contains("nem nosso")));
        // O mesmo com o dono desconhecido: pode ser dela.
        let ativos = vec![tela(), c((INTEL, 1), (INTEL, 9), true, Dono::Desconhecido)];
        assert!(matches!(decidir(&[novo], VIRTUAL, &foto, &ativos, &todos), Decisao::Esperar(_)));
    }

    #[test]
    fn o_clone_do_windows_e_desfeito_antes_de_estender() {
        let foto = vec![tela()];
        let novo = Par::novo(VIRTUAL, 257);
        let ja = Par::novo(VIRTUAL, 256);
        let ativos = vec![
            tela(),
            c((VIRTUAL, 0), (VIRTUAL, 256), true, Dono::Nosso),
            c((INTEL, 0), (INTEL, 0x0400_0101), true, Dono::Nosso),
        ];
        let todos: Vec<Caminho> = ativos.iter().copied().chain(livres(257, true)).collect();
        // O que já estava ativo no virtual fica no pedido; o clone sai.
        assert_eq!(
            decidir(&[novo, ja], VIRTUAL, &foto, &ativos, &todos),
            Decisao::DesfazerClone { virtuais: vec![1], clones: vec![2] }
        );
    }

    #[test]
    fn com_outro_nosso_ativo_o_pedido_leva_todos_e_nao_reusa_a_fonte_dele() {
        let foto = vec![tela()];
        let novo = Par::novo(VIRTUAL, 257);
        let ja = Par::novo(VIRTUAL, 256);
        let ativos = vec![tela(), c((VIRTUAL, 0), (VIRTUAL, 256), true, Dono::Nosso)];
        let todos: Vec<Caminho> = ativos.iter().copied().chain(livres(257, true)).collect();
        // O caminho livre do novo não pode usar a fonte 0 do virtual (a do 256): é o índice 3 (fonte 1).
        let d = decidir(&[novo, ja], VIRTUAL, &foto, &ativos, &todos);
        assert_eq!(d, Decisao::Estender { virtuais: vec![1], novos: vec![3] });
        assert_eq!(todos[3].fonte, Par::novo(VIRTUAL, 1));
    }

    #[test]
    fn se_a_chegada_tirou_um_dos_outros_o_pedido_o_devolve() {
        // E4: o Windows ativou o 2º (257) e tirou o 1º (256). O pedido devolve o 1º.
        let foto = vec![tela()];
        let novo = Par::novo(VIRTUAL, 257);
        let ja = Par::novo(VIRTUAL, 256);
        let ativos = vec![tela(), c((VIRTUAL, 0), (VIRTUAL, 257), true, Dono::Nosso)];
        let todos: Vec<Caminho> = ativos.iter().copied().chain(livres(256, true)).collect();
        let d = decidir(&[novo, ja], VIRTUAL, &foto, &ativos, &todos);
        // O 257 fica (virtuais), e o 256 volta por um caminho com fonte livre (fonte 1: índice 3).
        assert_eq!(d, Decisao::Estender { virtuais: vec![1], novos: vec![3] });
    }

    #[test]
    fn um_dos_outros_sem_caminho_nao_segura_o_novo() {
        let foto = vec![tela()];
        let novo = Par::novo(VIRTUAL, 257);
        let perdido = Par::novo(VIRTUAL, 256);
        let todos: Vec<Caminho> = std::iter::once(tela()).chain(livres(257, true)).collect();
        assert_eq!(decidir(&[novo, perdido], VIRTUAL, &foto, &[tela()], &todos), Decisao::Estender { virtuais: vec![], novos: vec![1] });
    }

    #[test]
    fn pronto_so_com_todos_os_nossos_ativos() {
        let foto = vec![tela()];
        let a = Par::novo(VIRTUAL, 256);
        let b = Par::novo(VIRTUAL, 257);
        let ativos = vec![tela(), c((VIRTUAL, 0), (VIRTUAL, 256), true, Dono::Nosso), c((VIRTUAL, 1), (VIRTUAL, 257), true, Dono::Nosso)];
        assert_eq!(decidir(&[b, a], VIRTUAL, &foto, &ativos, &ativos), Decisao::Pronto);
        // Um deles com o alvo indisponível não conta como ativo.
        let ativos2 = vec![tela(), c((VIRTUAL, 0), (VIRTUAL, 256), false, Dono::Nosso), c((VIRTUAL, 1), (VIRTUAL, 257), true, Dono::Nosso)];
        assert_ne!(decidir(&[b, a], VIRTUAL, &foto, &ativos2, &ativos2), Decisao::Pronto);
    }

    #[test]
    fn o_caminho_velho_de_quem_saiu_nao_entra_no_pedido() {
        // O monitor 258 acabou de sair e o caminho dele ainda aparece ativo com av=0.
        let foto = vec![tela()];
        let novo = Par::novo(VIRTUAL, 256);
        let ativos = vec![tela(), c((VIRTUAL, 2), (VIRTUAL, 258), false, Dono::Desconhecido)];
        let todos: Vec<Caminho> = ativos.iter().copied().chain(livres(256, true)).collect();
        match decidir(&[novo], VIRTUAL, &foto, &ativos, &todos) {
            Decisao::Estender { virtuais, novos } => {
                assert!(virtuais.is_empty(), "o velho não é mantido");
                assert_eq!(todos[novos[0]].alvo, novo);
            }
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn as_tentativas_respeitam_o_intervalo_e_o_teto() {
        let l = Limites::DA_RECEITA;
        let mut t = Tentativas::default();
        assert!(t.pode_pedir(0, &l));
        t.pediu(0);
        assert!(!t.pode_pedir(49, &l));
        assert!(t.pode_pedir(50, &l));
        for k in 1..8 {
            t.pediu(100 * k);
        }
        assert_eq!(t.feitas, 8);
        assert!(!t.pode_pedir(10_000, &l), "no máximo 8 pedidos");
    }

    #[test]
    fn o_prazo_e_pelo_relogio() {
        let l = Limites::DA_RECEITA;
        assert!(!prazo_vencido(7_999, &l));
        assert!(prazo_vencido(8_000, &l));
    }

    #[test]
    fn as_ausencias_dos_outros_sao_medidas_por_trecho() {
        let a = Par::novo(VIRTUAL, 256);
        let b = Par::novo(VIRTUAL, 257);
        let mut aus = Ausencias::para(&[a, b]);
        aus.observar(0, |_| true);
        aus.observar(10, |p| p != a);
        aus.observar(20, |p| p != a);
        aus.observar(560, |_| true);
        aus.observar(570, |p| p != a);
        aus.fechar(600);
        assert!(aus.algum_saiu());
        let (_, _, total, maior, vezes) = aus.por_alvo[0];
        assert_eq!((total, maior, vezes), (580, 550, 2));
        assert_eq!(aus.por_alvo[1].4, 0);
        assert!(aus.linha().contains("alvo 256: fora 2x, 580 ms (maior 550 ms)"));
        assert_eq!(Ausencias::para(&[b]).linha(), "nenhum dos 1 saiu");
    }

    #[test]
    fn a_captura_segue_o_alvo_e_nao_o_nome() {
        assert_eq!(reabertura(10, Some(10), false), Reabertura::Manter);
        assert_eq!(reabertura(10, Some(11), false), Reabertura::Reabrir, "o HMONITOR mudou");
        assert_eq!(reabertura(10, Some(10), true), Reabertura::Reabrir, "o item fechou com o alvo ativo");
        assert_eq!(reabertura(10, None, true), Reabertura::Esperar, "sem caminho ativo não se reabre");
    }

    #[test]
    fn a_saida_so_conta_com_as_duas_testemunhas_lidas() {
        assert!(saiu(Some(false), Some(false)));
        assert!(!saiu(Some(false), Some(true)));
        assert!(!saiu(None, Some(false)), "leitura que falhou não é saída");
        assert!(!saiu(Some(false), None));
    }

    #[test]
    fn o_pedido_usa_a_tela_da_foto_e_nao_a_de_agora() {
        // A foto guarda a tela como estava no ADD; o pedido é montado com ela. Aqui a checagem é a
        // de que só o alvo importa para "ativo agora": a fonte da tela pode ter mudado de índice.
        let foto = vec![tela()];
        let ativos = vec![c((INTEL, 3), (INTEL, 265_988), true, Dono::Usuario)];
        let novo = Par::novo(VIRTUAL, 256);
        let todos: Vec<Caminho> = ativos.iter().copied().chain(livres(256, true)).collect();
        assert!(matches!(decidir(&[novo], VIRTUAL, &foto, &ativos, &todos), Decisao::Estender { .. }));
        let _ = TELA;
    }
}
