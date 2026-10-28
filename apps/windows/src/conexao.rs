//! **A conexão sem dor, do lado do Windows** (`docs/conexao.md`, refeita sobre o v3 em
//! `docs/v3-no-privado.md`): o cartão do código de 4 dígitos, as frases de quando ele não fecha e a
//! sessão salva por identidade. Puro de propósito — o irmão do `core/Conexao.kt` do Android e do
//! `QuallNetKit/Conexao.swift` do Mac —, para os testes rodarem em qualquer máquina, até com
//! `rustc --test` direto neste arquivo.
//!
//! # O que fica fora daqui
//!
//! O núcleo. Quem lê o `quall_core::confirmacao::Retrato` e o traduz numa [`LeituraDoCodigo`] é o
//! `emissor.rs` e o `varias.rs` (o anfitrião, uma espera por vez até oito aparelhos) e o
//! `receptor.rs` (o convidado); quem acha o aparelho da sessão salva e retoma com ele, na mesma conexão, é
//! `quall_core::session::conectar_ao_par`, chamado pelo `receptor.rs`. Os textos são chaves da
//! tabela de tradução (`idioma/janela.rs`, `idioma/receptor.rs`): quem mostra passa por `t`/`tf`.
//!
//! # A escolha entre três (`docs/v3-no-privado.md` §3.6)
//!
//! O **anfitrião** não vê o código: vê três números (o código e dois falsos, em ordem sorteada pelo
//! núcleo e estável enquanto o cartão está aberto) e escolhe o que o outro aparelho mostra; "Nenhum
//! destes" recusa. Um erro na escolha encerra o código daquela espera (P11) — o PIN continua. O
//! **convidado** só mostra o código certo, para a pessoa achá-lo entre os três da outra tela.
//!
//! # A tentativa vai junto do código (revisão de 12/10, M2)
//!
//! O [`CartaoDoCodigo`] leva a tentativa a que os números pertencem, e a janela a devolve no toque
//! (`ConfirmacaoPorCodigo::escolher(tentativa, indice)`): um toque dado no cartão de uma tentativa que
//! já caiu não vale para a seguinte, que é o código de outro aparelho que a pessoa nunca viu.
//!
//! # A sessão salva só é gravada depois de autenticar (revisão de 12/10, M3)
//!
//! A lista e o `Challenge` não são autenticados — qualquer um na LAN responde. Só a retomada (ou o
//! PIN, ou o código) prova que ali é o aparelho salvo. Por isso a única porta de escrita é
//! [`SessaoSalva::depois_de_conectar`], com o id que a sessão **autenticada** diz.

// =============================================================================================
// O cartão do código
// =============================================================================================

/// A fase do confirmador, como a casca a lê do núcleo (`quall_core::confirmacao::Fase`). Repetida
/// aqui para o módulo não depender do núcleo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FaseDoCodigo {
    #[default]
    Esperando,
    Mostrando,
    ConfirmadoAqui,
    Conferiu,
    RecusadoAqui,
    RecusadoLa,
    NaoConferiu,
    Expirou,
}

/// O que a casca leu do confirmador, de uma vez.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LeituraDoCodigo {
    pub fase: FaseDoCodigo,
    /// O código, **só no convidado**: o núcleo não o dá ao anfitrião (revisão, baixo 1), que escolhe
    /// entre os três de [`LeituraDoCodigo::opcoes`].
    pub codigo: Option<String>,
    /// O nome que o outro aparelho disse.
    pub par: Option<String>,
    /// A tentativa a que o código pertence (`Retrato::tentativas`).
    pub tentativa: u32,
    /// **Anfitrião**: os três números, na ordem do núcleo. Vazio no convidado.
    pub opcoes: Vec<String>,
    /// A última escolha foi errada, e o código desta espera acabou (P11).
    pub escolha_errada: bool,
    /// Por que o núcleo recusou a última troca, quando há o que dizer (`RECUSA_JA_PAREADO`).
    pub recusa: Option<String>,
    /// O aviso do toque (`AVISO_PAREAMENTO_ANTERIOR`), quando a retomada não foi reconhecida.
    pub aviso: Option<String>,
}

/// O aviso do confirmador quando o toque caiu do reconectar para o código
/// (`quall_core::session::AVISO_PAREAMENTO_ANTERIOR`).
pub const AVISO_PAREAMENTO_ANTERIOR: &str = "previous_pairing_invalid";
/// A frase do cartão do convidado nesse caso.
pub const FRASE_PAREAMENTO_ANTERIOR: &str = "O pareamento anterior não vale mais. Confirme o código para parear de novo."; // i18n: chave

/// Quantos números o anfitrião vê (`docs/v3-no-privado.md` §3.6).
pub const OPCOES: usize = 3;

/// Quantas trocas pelo código uma espera aceita (`quall_core::session::TENTATIVAS_DO_CODIGO`; o teste
/// com o núcleo confere que é o mesmo número).
pub const TENTATIVAS_DO_CODIGO: u32 = 3;

/// **O cartão do código**, na tela: o do anfitrião (três números para escolher, mais "Nenhum
/// destes") ou o do convidado (o código, para achar na outra tela).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CartaoDoCodigo {
    /// Os quatro dígitos, **só no convidado**. Vazio no anfitrião.
    pub codigo: String,
    /// **Só no anfitrião**: os três números, na ordem em que aparecem (a do núcleo).
    pub opcoes: Vec<String>,
    /// Quem pede (no anfitrião) ou a quem se conecta (no convidado). Vazio: o outro não disse.
    pub par: String,
    /// A tentativa dos números desenhados: o toque vai com ela.
    pub tentativa: u32,
    /// O anfitrião escolheu o número certo: o cartão fica, sem os botões, enquanto o par fecha.
    pub confirmado_aqui: bool,
    /// **De qual confirmador** o cartão saiu ([`DaEspera::id`]): o toque só vale nele
    /// ([`toque_vale`]). Zero: sem confirmador conhecido (nunca vale).
    pub espera: u64,
    /// **O toque tentou reconectar e o outro lado não reconheceu** (`docs/v3-no-privado.md` §8.6): o
    /// código é para parear de novo, e o cartão do convidado diz isso.
    pub pareamento_anterior: bool,
}

impl CartaoDoCodigo {
    /// O cartão é do anfitrião (os três números)?
    pub fn do_anfitriao(&self) -> bool {
        !self.opcoes.is_empty()
    }

    /// O cartão, marcado com o confirmador que o produziu.
    pub fn da_espera(self, espera: u64) -> Self {
        CartaoDoCodigo { espera, ..self }
    }
}

// =============================================================================================
// O confirmador da espera (revisão da integração, baixos 1 e 2)
// =============================================================================================

/// **Um confirmador com identidade**: o id (único no processo, nunca zero) e o PIN da espera a que
/// ele pertence.
#[derive(Clone, Debug)]
pub struct DaEspera<T> {
    pub id: u64,
    pub pin: String,
    pub c: T,
}

/// **O confirmador dura enquanto o PIN for o mesmo** (o modelo `ConfirmacaoDaEspera` do iOS): a
/// espera que volta com o mesmo PIN (o receptor saiu, o prazo venceu) continua com o mesmo
/// confirmador — o limite de três trocas e o fim do código depois de uma escolha errada (P11) não
/// zeram a cada volta. PIN novo, confirmador novo.
#[derive(Debug)]
pub struct EsperasDoCodigo<T> {
    atual: Option<DaEspera<T>>,
}

impl<T> Default for EsperasDoCodigo<T> {
    fn default() -> Self {
        EsperasDoCodigo { atual: None }
    }
}

impl<T: Clone> EsperasDoCodigo<T> {
    /// O confirmador para uma espera com `pin`: o de antes, se o PIN é o mesmo; senão um novo
    /// (`nova()`), com id novo.
    pub fn para_o_pin(&mut self, pin: &str, nova: impl FnOnce() -> T) -> DaEspera<T> {
        if let Some(a) = self.atual.as_ref().filter(|a| a.pin == pin) {
            return a.clone();
        }
        let d = DaEspera { id: nova_espera(), pin: pin.to_string(), c: nova() };
        self.atual = Some(d.clone());
        d
    }
}

/// Um id de confirmador, único no processo (os da tela, da câmera comum e da R5 não se cruzam).
pub fn nova_espera() -> u64 {
    static PROXIMA: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    PROXIMA.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// **O toque vale?** Só no confirmador que produziu o cartão (baixo 1): numa volta de espera com
/// outro confirmador, o índice tocado no cartão velho não cai nos números novos.
pub fn toque_vale(cartao: &CartaoDoCodigo, atual: Option<u64>) -> bool {
    cartao.espera != 0 && atual == Some(cartao.espera)
}

/// O código tem a forma que as duas telas mostram: quatro dígitos.
fn codigo_valido(c: &str) -> bool {
    c.len() == 4 && c.bytes().all(|b| b.is_ascii_digit())
}

/// **Quando há cartão, e qual.** Os números na tela pedem decisão; confirmado aqui (o anfitrião
/// escolheu o certo), o cartão fica sem os botões até o par fechar. Em qualquer outra fase, nada.
/// Três números tortos ou repetidos não viram cartão: a escolha não teria sentido.
pub fn cartao(l: &LeituraDoCodigo) -> Option<CartaoDoCodigo> {
    let confirmado_aqui = match l.fase {
        FaseDoCodigo::Mostrando => false,
        FaseDoCodigo::ConfirmadoAqui => true,
        _ => return None,
    };
    let par = l.par.clone().unwrap_or_default().trim().to_string();
    let pareamento_anterior = l.aviso.as_deref() == Some(AVISO_PAREAMENTO_ANTERIOR);
    if !l.opcoes.is_empty() {
        let distintos = l.opcoes.iter().enumerate().all(|(i, a)| !l.opcoes[..i].contains(a));
        if l.opcoes.len() != OPCOES || !distintos || !l.opcoes.iter().all(|c| codigo_valido(c)) {
            return None;
        }
        return Some(CartaoDoCodigo { codigo: String::new(), opcoes: l.opcoes.clone(), par, tentativa: l.tentativa, confirmado_aqui, espera: 0, pareamento_anterior });
    }
    let codigo = l.codigo.as_deref().filter(|c| codigo_valido(c))?;
    Some(CartaoDoCodigo { codigo: codigo.to_string(), opcoes: Vec::new(), par, tentativa: l.tentativa, confirmado_aqui, espera: 0, pareamento_anterior })
}

/// **A leitura do confirmador do núcleo**, de uma vez (o `Retrato` não mistura a fase de uma tentativa
/// com o código de outra). Fica atrás de `core` para o resto do arquivo seguir sem dependência — e o
/// `rustc --test` direto nele, sem esta função.
#[cfg(feature = "core")]
pub fn ler(c: &quall_core::confirmacao::ConfirmacaoPorCodigo) -> LeituraDoCodigo {
    use quall_core::confirmacao::Fase;
    let r = c.retrato();
    LeituraDoCodigo {
        fase: match r.fase {
            Fase::Esperando => FaseDoCodigo::Esperando,
            Fase::Mostrando => FaseDoCodigo::Mostrando,
            Fase::ConfirmadoAqui => FaseDoCodigo::ConfirmadoAqui,
            Fase::Conferiu => FaseDoCodigo::Conferiu,
            Fase::RecusadoAqui => FaseDoCodigo::RecusadoAqui,
            Fase::RecusadoLa => FaseDoCodigo::RecusadoLa,
            Fase::NaoConferiu => FaseDoCodigo::NaoConferiu,
            Fase::Expirou => FaseDoCodigo::Expirou,
        },
        codigo: r.codigo,
        par: r.par,
        tentativa: r.tentativas,
        opcoes: r.opcoes,
        escolha_errada: r.escolha_errada,
        recusa: r.recusa.map(str::to_string),
        aviso: r.aviso.map(str::to_string),
    }
}

/// "04 27": o código como as telas o mostram (o Android e o Mac também).
pub fn em_grupos(codigo: &str) -> String {
    if codigo.len() == 4 && codigo.is_ascii() {
        format!("{} {}", &codigo[..2], &codigo[2..])
    } else {
        codigo.to_string()
    }
}

/// "0 4 2 7": o código para o Narrador, dígito a dígito (como o letreiro do PIN).
pub fn para_ler(codigo: &str) -> String {
    codigo.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" ")
}

/// A posição de um número no cartão do anfitrião, para o Narrador ("primeiro de três"): a chave da
/// tabela, em português. Junto com [`MOLDE_DA_OPCAO`].
pub fn posicao(indice: usize) -> &'static str {
    match indice {
        0 => "primeiro", // i18n: chave
        1 => "segundo", // i18n: chave
        _ => "terceiro", // i18n: chave
    }
}

/// O molde do nome de cada número para o Narrador: os dígitos, um a um, e a posição
/// ("0 4 2 7, primeiro de três").
pub const MOLDE_DA_OPCAO: &str = "{}, {} de três"; // i18n: chave

pub const FRASE_RECUSADO_AQUI: &str = "Você recusou o código."; // i18n: chave
pub const FRASE_RECUSADO_LA: &str = "O outro aparelho recusou o código, ou desistiu."; // i18n: chave
pub const FRASE_NAO_CONFERIU: &str = "Os códigos não conferiram. Tente de novo; se repetir, alguém pode estar no meio da rede."; // i18n: chave
pub const FRASE_EXPIROU: &str = "O código expirou sem confirmação."; // i18n: chave
/// O convidado ofereceu o código e o anfitrião não aceita (uma build das lojas, uma espera sem a
/// tela do cartão, ou o código daquela espera já gasto): o PIN resolve.
pub const FRASE_NAO_ACEITA_O_CODIGO: &str = "Este aparelho não aceita o código. Digite o PIN que a tela dele mostra."; // i18n: chave
/// **P11**, no anfitrião: a escolha errada desliga o código desta espera; o PIN continua.
pub const FRASE_ESCOLHA_ERRADA: &str = "Esse não era o número do outro aparelho. O código ficou desligado nesta espera; o PIN continua valendo."; // i18n: chave
/// No anfitrião, as três trocas da espera já foram gastas.
pub const FRASE_CODIGO_ESGOTADO: &str = "O código já foi tentado três vezes nesta espera. O PIN continua valendo."; // i18n: chave
/// **A1**, no convidado: três pernas caíram antes de o código chegar à tela, no mesmo aparelho, em
/// 10 minutos — o núcleo para de tentar o código com ele.
pub const FRASE_INTERFERENCIA: &str = "Algo está interferindo na conexão com este aparelho. Use o PIN."; // i18n: chave
/// **A2**, no convidado: há vínculo com o aparelho e a dica dele não veio (ele esqueceu este
/// computador, ou alguém trocou as dicas no caminho). Falha fechado: nem código nem QR para ele até
/// a pessoa esquecê-lo aqui — ou digitar o PIN.
pub const FRASE_NAO_RECONHECEU: &str = "O outro aparelho não reconheceu este pareamento. Para parear de novo pelo código, esqueça este aparelho; ou digite o PIN."; // i18n: chave
/// **A2 na sessão salva** (`conectar_ao_par`): ninguém na rede tinha a dica do aparelho até o prazo.
pub const MOLDE_NAO_ACHEI: &str = "Não achei {} nesta rede, ou ele não reconheceu este pareamento. Para parear de novo pelo código, esqueça este aparelho; ou digite o PIN."; // i18n: chave
/// O aparelho da sessão salva, sem vínculo v3 aqui (esquecido, ou gravado por uma build anterior).
pub const FRASE_SEM_VINCULO: &str = "Este computador não tem mais o pareamento com esse aparelho. Escolha-o na lista para usar o código, ou digite o PIN."; // i18n: chave
/// **B2**, no convidado: os dois já têm um vínculo, e o código não o substitui
/// (`docs/v3-no-privado.md` §8.1). Quem lembra esquece, ou o PIN substitui como sempre.
pub const FRASE_JA_PAREADO: &str = "Estes dois aparelhos já têm um pareamento, e o código não o substitui. Esqueça o pareamento no aparelho que ainda o guarda, ou digite o PIN."; // i18n: chave
/// **B2 do QR** (`docs/v3-no-privado.md` §8.2): o mesmo, pelo QR.
pub const FRASE_JA_PAREADO_PELO_QR: &str = "Este computador e o celular já têm um pareamento, e o QR não o substitui. Esqueça o pareamento no aparelho que ainda o guarda, ou digite o PIN."; // i18n: chave

/// O motivo da recusa do núcleo no `Retrato` do confirmador (`Retrato::recusa`) quando a troca caiu
/// porque os dois já têm vínculo (B2, nos dois lados).
pub const RECUSA_JA_PAREADO: &str = "already_paired";

/// **O que dizer quando a tentativa pelo código não fechou**, em português (a chave da tabela);
/// `None` nas outras fases. As mesmas frases do Mac e do Android.
pub fn desfecho(fase: FaseDoCodigo) -> Option<&'static str> {
    match fase {
        FaseDoCodigo::RecusadoAqui => Some(FRASE_RECUSADO_AQUI),
        FaseDoCodigo::RecusadoLa => Some(FRASE_RECUSADO_LA),
        FaseDoCodigo::NaoConferiu => Some(FRASE_NAO_CONFERIU),
        FaseDoCodigo::Expirou => Some(FRASE_EXPIROU),
        _ => None,
    }
}

/// **O anfitrião, entre uma tentativa e outra**: a frase da última que não fechou, enquanto outra
/// não começa. Com os números na tela (ou nenhuma tentativa ainda), nada. A espera continua depois
/// de um código recusado — é a regra do núcleo (`docs/v3-no-privado.md` §3.4) —, e a frase diz por
/// que o cartão sumiu; depois de uma escolha errada (P11) ou das três trocas, diz que o código acabou
/// nesta espera e que o PIN continua; quando o núcleo recusou por vínculo existente (B2), diz isso.
pub fn aviso_do_anfitriao(l: &LeituraDoCodigo) -> Option<&'static str> {
    if l.tentativa == 0 || matches!(l.fase, FaseDoCodigo::Mostrando | FaseDoCodigo::ConfirmadoAqui) {
        return None;
    }
    if l.escolha_errada {
        return Some(FRASE_ESCOLHA_ERRADA);
    }
    if l.recusa.as_deref() == Some(RECUSA_JA_PAREADO) {
        return Some(FRASE_JA_PAREADO);
    }
    let frase = desfecho(l.fase)?;
    Some(if l.tentativa >= TENTATIVAS_DO_CODIGO { FRASE_CODIGO_ESGOTADO } else { frase })
}

// =============================================================================================
// A falha do convidado
// =============================================================================================

/// A variante do erro do núcleo que encerrou a conexão, sem o núcleo (para este módulo seguir puro).
/// `Interferencia`, `ParNaoReconhecido` e `JaPareado` são as variantes próprias do A1, do A2 e do
/// B2 (`docs/v3-no-privado.md` §8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipoDoErro {
    NeedsPin,
    Pairing,
    Timeout,
    Interferencia,
    ParNaoReconhecido,
    JaPareado,
    Outro,
}

/// **Como a conexão do convidado acabou**, do que a casca sabe: a variante do erro, como a troca
/// pelo código terminou (`None` sem código à mão) e o caminho que se tomou.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FalhaDoConvidado {
    pub tipo: TipoDoErro,
    pub fase: Option<FaseDoCodigo>,
    /// O confirmador diz que a troca caiu por vínculo existente (`RECUSA_JA_PAREADO`).
    pub recusa_ja_pareado: bool,
    /// Havia vínculo v3 com o aparelho esperado (a sessão salva, ou o item que a lista reconheceu).
    pub alvo_com_vinculo: bool,
    /// Foi a procura da sessão salva (`conectar_ao_par`).
    pub procura: bool,
}

/// **O conselho da tela** depois de uma falha: a frase (a chave da tabela; com `com_nome`, um molde
/// com o nome do aparelho), se o campo do PIN pede atenção, e se a tela oferece "Esquecer este
/// aparelho".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conselho {
    pub frase: &'static str,
    pub com_nome: bool,
    pub pede_pin: bool,
    pub esquecer: bool,
}

const fn conselho(frase: &'static str, pede_pin: bool, esquecer: bool) -> Conselho {
    Conselho { frase, com_nome: false, pede_pin, esquecer }
}

/// **O ponto único erro → frase do convidado**, antes das do status do núcleo; `None` quando quem
/// explica é o status.
///
/// - **A1** (`Interferencia`, três pernas caídas): o PIN. **B2** (`JaPareado`, ou o motivo no
///   confirmador): esquecer de um lado, ou o PIN.
/// - **A2** (`ParNaoReconhecido`, ou outra recusa com vínculo com o alvo): falha fechado, com
///   "Esquecer este aparelho" e o PIN; na procura da sessão salva, "não achei" pelo prazo, e "sem
///   vínculo" se ele já não existe.
/// - O código entrou em cena e não fechou: a frase é dele, e o PIN não é a saída.
/// - O código estava à mão (conectou sem PIN), nenhuma troca começou, e o outro lado pediu PIN: é um
///   aparelho que não aceita o código. O PIN resolve.
pub fn falha_do_convidado(f: &FalhaDoConvidado) -> Option<Conselho> {
    match f.tipo {
        TipoDoErro::Interferencia => return Some(conselho(FRASE_INTERFERENCIA, true, false)),
        TipoDoErro::JaPareado => return Some(conselho(FRASE_JA_PAREADO, true, false)),
        TipoDoErro::ParNaoReconhecido => return Some(conselho(FRASE_NAO_RECONHECEU, true, true)),
        _ => {}
    }
    if f.recusa_ja_pareado {
        return Some(conselho(FRASE_JA_PAREADO, true, false));
    }
    if f.procura {
        return match f.tipo {
            TipoDoErro::NeedsPin => Some(conselho(FRASE_SEM_VINCULO, true, false)),
            TipoDoErro::Pairing => Some(conselho(FRASE_NAO_RECONHECEU, true, true)),
            TipoDoErro::Timeout => Some(Conselho { frase: MOLDE_NAO_ACHEI, com_nome: true, pede_pin: true, esquecer: true }),
            _ => None,
        };
    }
    if f.alvo_com_vinculo && matches!(f.tipo, TipoDoErro::Pairing | TipoDoErro::NeedsPin) {
        return Some(conselho(FRASE_NAO_RECONHECEU, true, true));
    }
    let fase = f.fase?;
    if let Some(frase) = desfecho(fase) {
        return Some(conselho(frase, false, false));
    }
    if f.tipo == TipoDoErro::NeedsPin && fase == FaseDoCodigo::Esperando {
        return Some(conselho(FRASE_NAO_ACEITA_O_CODIGO, true, false));
    }
    None
}

// =============================================================================================
// A sessão salva (`docs/conexao.md` §3, `docs/v3-no-privado.md` §5)
// =============================================================================================

/// O arquivo da sessão salva do Exibir, na pasta de dados (`%APPDATA%\Quall Monitor`).
pub const ARQUIVO_DA_SESSAO: &str = "sessao-exibir.txt";

/// A primeira linha do arquivo: o formato. Outra coisa no arquivo é "sem sessão salva".
const CABECALHO: &str = "quall-sessao-exibir 1";

/// **A última sessão do Exibir**: quem era o outro lado (o `DeviceId`), o nome, e o último endereço
/// — só como dica.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessaoSalva {
    pub device_id: String,
    pub nome: String,
    pub endereco: String,
}

/// Tira quebras de linha (o arquivo é uma chave por linha) e as pontas.
fn uma_linha(s: &str) -> String {
    s.replace(['\r', '\n'], " ").trim().to_string()
}

impl SessaoSalva {
    /// Lê o arquivo. Sem endereço, ou de outro formato, é "sem sessão salva".
    pub fn ler(texto: &str) -> Option<SessaoSalva> {
        let mut linhas = texto.lines();
        if linhas.next().map(str::trim) != Some(CABECALHO) {
            return None;
        }
        let mut s = SessaoSalva::default();
        for l in linhas {
            let Some((chave, valor)) = l.split_once('=') else { continue };
            let valor = valor.trim().to_string();
            match chave.trim() {
                "id" => s.device_id = valor,
                "nome" => s.nome = valor,
                "endereco" => s.endereco = valor,
                _ => {}
            }
        }
        (!s.endereco.is_empty()).then_some(s)
    }

    /// O texto do arquivo.
    pub fn escrever(&self) -> String {
        format!(
            "{CABECALHO}\nid={}\nnome={}\nendereco={}\n",
            uma_linha(&self.device_id),
            uma_linha(&self.nome),
            uma_linha(&self.endereco)
        )
    }

    /// **A sessão a gravar depois que a conexão fechou** (M3): o id é o que a sessão autenticada diz,
    /// e o endereço é o que de fato conectou (na procura, o que o núcleo achou:
    /// `Ready::endereco_do_par`). Sem id do par, mantém o id que havia se o endereço é o mesmo; senão
    /// grava sem id — e a próxima vez não procura. A mesma regra do Mac (`SessaoSalva.depoisDeConectar`).
    pub fn depois_de_conectar(antes: Option<&SessaoSalva>, id_do_par: &str, nome: &str, endereco: &str) -> SessaoSalva {
        let id = if !id_do_par.trim().is_empty() {
            id_do_par.trim().to_string()
        } else {
            antes.filter(|a| a.endereco == endereco).map(|a| a.device_id.clone()).unwrap_or_default()
        };
        SessaoSalva { device_id: id, nome: uma_linha(nome), endereco: uma_linha(endereco) }
    }
}

/// **De onde veio o destino do Exibir**, para decidir como conectar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrigemDoDestino<'a> {
    /// O endereço digitado no campo.
    Digitado(&'a str),
    /// Um aparelho que a busca está vendo agora ("Quall 1a2b3c4d"): anônimo, sem alvo.
    VistoNaLista,
    /// A linha da sessão salva na lista (o nome guardado, "pareado").
    LembradoNaLista,
    /// Um item da busca que a sonda reconheceu como um par guardado (`reconhecer`, §5.4): o id dele.
    /// Pista, não prova — quem decide é a retomada só com o vínculo dele.
    ReconhecidoNaLista(&'a str),
}

/// **Como o Exibir conecta** (`docs/v3-no-privado.md` §3.4, §3.5 e §5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaminhoDoExibir {
    /// O de sempre, sem alvo: o PIN se digitado; senão a retomada pelas dicas e, sem ela, o código
    /// (um por toque).
    Livre,
    /// **O toque num aparelho pareado** (`docs/v3-no-privado.md` §8.6): a linha da sessão salva, o
    /// item reconhecido ou o endereço salvo digitado. `conectar_com_codigo_ao_alvo`: reconecta pela
    /// rota de agora só com o vínculo dele; se ele não reconhece mais, o código para parear de novo;
    /// se a rota cai, o núcleo procura o aparelho pela busca.
    AoAlvo { device_id: String },
}

/// **Qual caminho o Exibir toma.** Com PIN digitado, o de sempre: o PIN é o caminho de quem está
/// pareando de novo (e substitui o vínculo), e o endereço é o que a pessoa leu agora na outra tela.
/// Sem PIN e com vínculo v3 com o aparelho da sessão salva: a linha dela vai pela procura, e o
/// endereço salvo digitado vai ao alvo. Um item que a lista reconheceu vai ao alvo dele (a A2 vale a
/// partir da lista). O resto, sem alvo. `tem_vinculo`: o do aparelho da origem (o reconhecido, ou o
/// da sessão salva).
pub fn caminho_do_exibir(origem: OrigemDoDestino<'_>, com_pin: bool, salva: Option<&SessaoSalva>, tem_vinculo: bool) -> CaminhoDoExibir {
    if let OrigemDoDestino::ReconhecidoNaLista(id) = origem {
        return if tem_vinculo && !com_pin && !id.is_empty() { CaminhoDoExibir::AoAlvo { device_id: id.to_string() } } else { CaminhoDoExibir::Livre };
    }
    let Some(s) = salva.filter(|s| !s.device_id.is_empty() && tem_vinculo && !com_pin) else {
        return CaminhoDoExibir::Livre;
    };
    match origem {
        OrigemDoDestino::Digitado(e) if e.trim() == s.endereco => CaminhoDoExibir::AoAlvo { device_id: s.device_id.clone() },
        OrigemDoDestino::LembradoNaLista => CaminhoDoExibir::AoAlvo { device_id: s.device_id.clone() },
        _ => CaminhoDoExibir::Livre,
    }
}

/// **O item vivo que é o aparelho da sessão salva** (bancada de 13/10: "Quall d18d7c97 · Câmera" e
/// "Iphone 7 · Pareado" no mesmo 192.168.15.6:7877). No v3 o anúncio não traz id: quem diz que um item
/// é um par é a sonda, pela dica — e um aparelho que **esqueceu** este computador não tem mais a dica,
/// então o item fica anônimo e a sessão salva entrava como uma segunda linha. O item vivo no mesmo
/// endereço (IP e porta) que a sonda não rotulou como outro par é o mesmo aparelho: uma linha só, com o
/// nome e o alvo da sessão salva. Pista, não prova: o toque decide (a retomada só com o vínculo dele, e
/// o código se ele não reconhecer). `conhecidos`: o rótulo da sonda de cada item, na mesma ordem.
pub fn item_da_sessao_salva(salva: &SessaoSalva, enderecos: &[String], conhecidos: &[Option<&str>]) -> Option<usize> {
    if salva.device_id.is_empty() || conhecidos.iter().any(|c| *c == Some(salva.device_id.as_str())) {
        return None;
    }
    enderecos
        .iter()
        .zip(conhecidos)
        .position(|(e, c)| *e == salva.endereco && c.is_none())
}

/// **A sessão salva entra na lista?** Só com id, com o vínculo v3 ainda guardado (P3: o nome local
/// do pareado), e quando a sonda ainda não reconheceu o aparelho entre os itens da busca
/// (`reconhecidos`) — reconhecido, ele já está lá com o endereço de agora.
pub fn lembrar_na_lista(salva: Option<&SessaoSalva>, tem_vinculo: bool, reconhecidos: &[&str]) -> bool {
    tem_vinculo && salva.is_some_and(|s| !s.device_id.is_empty() && !reconhecidos.contains(&s.device_id.as_str()))
}

/// **Os nomes locais dos pares**, por id (`nomes-dos-pares.txt`): o nome que a sessão autenticada
/// deu, guardado depois de conectar, nos dois papéis. A lista mostra este nome no item que a sonda
/// reconheceu; o `pares.json` do núcleo não guarda nomes.
pub const ARQUIVO_DOS_NOMES: &str = "nomes-dos-pares.txt";
const CABECALHO_DOS_NOMES: &str = "quall-nomes-dos-pares 1";
/// Teto de nomes guardados (o núcleo elege no máximo 128 pares).
const NOMES_NO_MAXIMO: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NomesDosPares(pub Vec<(String, String)>);

impl NomesDosPares {
    /// Lê o arquivo (uma linha `id\tnome`). Outro formato é "nenhum nome".
    pub fn ler(texto: &str) -> NomesDosPares {
        let mut linhas = texto.lines();
        if linhas.next().map(str::trim) != Some(CABECALHO_DOS_NOMES) {
            return NomesDosPares::default();
        }
        NomesDosPares(
            linhas
                .filter_map(|l| l.split_once('\t'))
                .map(|(id, n)| (id.trim().to_string(), n.trim().to_string()))
                .filter(|(id, n)| !id.is_empty() && !n.is_empty())
                .take(NOMES_NO_MAXIMO)
                .collect(),
        )
    }

    pub fn escrever(&self) -> String {
        let mut s = format!("{CABECALHO_DOS_NOMES}\n");
        for (id, n) in &self.0 {
            s.push_str(&format!("{}\t{}\n", sem_tab(id), sem_tab(n)));
        }
        s
    }

    pub fn nome(&self, id: &str) -> Option<&str> {
        self.0.iter().find(|(i, _)| i == id).map(|(_, n)| n.as_str())
    }

    /// Guarda (ou troca) o nome de `id`; o mais recente fica no começo, e o mais velho sai no teto.
    pub fn guardar(&mut self, id: &str, nome: &str) {
        let (id, nome) = (sem_tab(id), sem_tab(nome));
        if id.is_empty() || nome.is_empty() {
            return;
        }
        self.0.retain(|(i, _)| *i != id);
        self.0.insert(0, (id, nome));
        self.0.truncate(NOMES_NO_MAXIMO);
    }

    pub fn esquecer(&mut self, id: &str) {
        self.0.retain(|(i, _)| i != id);
    }
}

fn sem_tab(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ").trim().to_string()
}

// =============================================================================================
// O nome que os outros aparelhos veem (`docs/v3-no-privado.md` §8.7)
// =============================================================================================

/// **O nome que vai no anúncio**: o guardado (`nome-na-rede.txt`; vazio é a pessoa que apagou, e os
/// anúncios saem sem nome — a lista dos outros mostra o alias), ou o nome do computador enquanto ela
/// não escolheu outro. O saneamento de verdade (40 bytes, sem controle) é o do núcleo.
pub fn nome_na_rede(guardado: Option<&str>, do_computador: &str) -> String {
    sem_tab(guardado.unwrap_or(do_computador))
}

/// **O nome da sessão** (o `display_name` que o `Hello` leva, cifrado): no Windows, o mesmo campo — o
/// "Nome deste computador" de antes. Com o campo apagado, o nome do computador: a sessão sempre tem
/// nome, a rede não.
pub fn nome_da_sessao(guardado: Option<&str>, do_computador: &str) -> String {
    let n = nome_na_rede(guardado, do_computador);
    if n.is_empty() { sem_tab(do_computador) } else { n }
}

/// **O título da linha da lista e o alias embaixo** (§8.7), pela regra do núcleo
/// (`quall_core::busca::rotulo_da_linha`): o nome guardado do par reconhecido > o nome anunciado,
/// com o alias embaixo > o alias.
#[cfg(feature = "core")]
pub fn rotulo_da_linha(guardado: Option<&str>, anunciado: Option<&str>, alias: &str) -> (String, Option<String>) {
    let r = quall_core::busca::rotulo_da_linha(guardado, anunciado, alias);
    (r.titulo, r.alias_embaixo)
}

/// **O nome da tela de espera** (bancada de 14/10, `docs/v3-no-privado.md` §8.7.1): o que os outros veem
/// na lista (o nome com que o anúncio subiu) e o alias para conferir — a mesma regra da linha da lista,
/// sem par guardado. Sem nome, o alias sozinho; `None` sem anúncio (alias vazio).
#[cfg(feature = "core")]
pub fn nome_da_espera(anunciado: &str, alias: &str) -> Option<(String, Option<String>)> {
    if alias.trim().is_empty() {
        return None;
    }
    Some(rotulo_da_linha(None, Some(anunciado), alias))
}

/// O mesmo numa linha só: "Dell da sala (Quall 1a2b3c4d)"; vazio sem anúncio.
#[cfg(feature = "core")]
pub fn nome_da_espera_em_linha(anunciado: &str, alias: &str) -> String {
    match nome_da_espera(anunciado, alias) {
        Some((nome, Some(a))) => format!("{nome} ({a})"),
        Some((nome, None)) => nome,
        None => String::new(),
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn leitura(fase: FaseDoCodigo, codigo: Option<&str>, tentativa: u32) -> LeituraDoCodigo {
        LeituraDoCodigo { fase, codigo: codigo.map(str::to_string), par: Some(" Galaxy A07 ".into()), tentativa, ..Default::default() }
    }

    fn tres() -> Vec<String> {
        vec!["8812".into(), "0427".into(), "5530".into()]
    }

    fn do_anfitriao(fase: FaseDoCodigo, tentativa: u32) -> LeituraDoCodigo {
        LeituraDoCodigo { opcoes: tres(), ..leitura(fase, None, tentativa) }
    }

    #[test]
    fn o_cartao_do_convidado_so_aparece_com_o_codigo_na_tela() {
        let c = cartao(&leitura(FaseDoCodigo::Mostrando, Some("0427"), 1)).expect("cartão");
        assert_eq!(c, CartaoDoCodigo { codigo: "0427".into(), opcoes: vec![], par: "Galaxy A07".into(), tentativa: 1, confirmado_aqui: false, espera: 0, pareamento_anterior: false });
        let refazer = LeituraDoCodigo { aviso: Some(AVISO_PAREAMENTO_ANTERIOR.into()), ..leitura(FaseDoCodigo::Mostrando, Some("0427"), 1) };
        assert!(cartao(&refazer).unwrap().pareamento_anterior, "o toque que caiu para o código diz por quê (§8.6)");
        assert!(!c.do_anfitriao());
        for fase in [
            FaseDoCodigo::Esperando,
            FaseDoCodigo::Conferiu,
            FaseDoCodigo::RecusadoAqui,
            FaseDoCodigo::RecusadoLa,
            FaseDoCodigo::NaoConferiu,
            FaseDoCodigo::Expirou,
        ] {
            assert_eq!(cartao(&leitura(fase, Some("0427"), 1)), None, "{fase:?}");
        }
        assert_eq!(cartao(&leitura(FaseDoCodigo::Mostrando, None, 1)), None, "sem código");
        assert_eq!(cartao(&leitura(FaseDoCodigo::Mostrando, Some("42"), 1)), None, "código torto");
        assert_eq!(cartao(&leitura(FaseDoCodigo::Mostrando, Some("04a7"), 1)), None, "código torto");
    }

    /// §3.6: o anfitrião vê três números, na ordem do núcleo, e nunca o código.
    #[test]
    fn o_cartao_do_anfitriao_tem_os_tres_numeros_e_nao_o_codigo() {
        let c = cartao(&do_anfitriao(FaseDoCodigo::Mostrando, 2)).expect("cartão");
        assert!(c.do_anfitriao());
        assert_eq!(c.opcoes, tres(), "a ordem é a do núcleo (sorteada lá, estável no cartão)");
        assert_eq!(c.codigo, "", "o anfitrião não recebe o código");
        assert_eq!((c.tentativa, c.confirmado_aqui), (2, false));
        // O código que viesse junto (não vem do núcleo) não entra no cartão do anfitrião.
        let com_codigo = LeituraDoCodigo { codigo: Some("0427".into()), ..do_anfitriao(FaseDoCodigo::Mostrando, 1) };
        assert_eq!(cartao(&com_codigo).unwrap().codigo, "");
        assert!(cartao(&do_anfitriao(FaseDoCodigo::ConfirmadoAqui, 1)).unwrap().confirmado_aqui);
        assert_eq!(cartao(&do_anfitriao(FaseDoCodigo::RecusadoAqui, 1)), None);
        // Dois, quatro, repetidos ou tortos: sem cartão.
        for opcoes in [vec!["0427", "8812"], vec!["0427", "8812", "5530", "1111"], vec!["0427", "0427", "5530"], vec!["0427", "88a2", "5530"]] {
            let l = LeituraDoCodigo { opcoes: opcoes.iter().map(|s| s.to_string()).collect(), ..do_anfitriao(FaseDoCodigo::Mostrando, 1) };
            assert_eq!(cartao(&l), None, "{opcoes:?}");
        }
    }

    /// **Baixo 2**: o confirmador dura enquanto o PIN for o mesmo — a volta de espera com o mesmo PIN
    /// devolve o mesmo (o P11 e o limite de três não zeram); PIN novo, confirmador novo e id novo.
    #[test]
    fn o_confirmador_dura_enquanto_o_pin_for_o_mesmo() {
        let mut esperas: EsperasDoCodigo<std::rc::Rc<std::cell::Cell<u32>>> = EsperasDoCodigo::default();
        let nova = || std::rc::Rc::new(std::cell::Cell::new(0));
        let a = esperas.para_o_pin("482719", nova);
        a.c.set(3); // três trocas gastas (ou a escolha errada) nesta espera
        let b = esperas.para_o_pin("482719", nova);
        assert_eq!((b.id, b.c.get()), (a.id, 3), "a volta com o mesmo PIN continua com o mesmo confirmador");
        let c = esperas.para_o_pin("590133", nova);
        assert!(c.id != a.id && c.c.get() == 0, "PIN novo: confirmador novo");
        assert!(a.id != 0 && c.id != 0);
    }

    /// **Baixo 1**: o toque só vale no confirmador que produziu o cartão.
    #[test]
    fn o_toque_vai_ao_confirmador_do_cartao() {
        let (velho, novo) = (nova_espera(), nova_espera());
        let cartao = cartao(&do_anfitriao(FaseDoCodigo::Mostrando, 1)).unwrap().da_espera(velho);
        assert!(toque_vale(&cartao, Some(velho)));
        assert!(!toque_vale(&cartao, Some(novo)), "o índice do cartão velho não cai nos números novos");
        assert!(!toque_vale(&cartao, None));
        assert!(!toque_vale(&cartao.clone().da_espera(0), Some(0)), "sem confirmador conhecido, nunca");
    }

    /// M2: a tentativa do cartão é a da leitura; um cartão novo com outra tentativa é outro cartão
    /// (a janela redesenha, e o toque vai com a tentativa nova).
    #[test]
    fn a_tentativa_vai_no_cartao() {
        let a = cartao(&do_anfitriao(FaseDoCodigo::Mostrando, 1)).unwrap();
        let b = cartao(&do_anfitriao(FaseDoCodigo::Mostrando, 2)).unwrap();
        assert_ne!(a, b, "mesmos números, outra tentativa: outro cartão");
        assert_eq!(b.tentativa, 2);
    }

    /// O confirmador recém-nascido do núcleo: nada na tela, nenhuma tentativa.
    #[cfg(feature = "core")]
    #[test]
    fn le_o_confirmador_do_nucleo() {
        let l = ler(&quall_core::confirmacao::ConfirmacaoPorCodigo::nova());
        assert_eq!(l, LeituraDoCodigo::default());
        assert_eq!(cartao(&l), None);
        assert_eq!(aviso_do_anfitriao(&l), None);
    }

    #[cfg(feature = "net")]
    #[test]
    fn as_tentativas_sao_as_do_nucleo() {
        assert_eq!(TENTATIVAS_DO_CODIGO, quall_core::session::TENTATIVAS_DO_CODIGO);
    }

    #[test]
    fn o_codigo_em_grupos_e_para_ler() {
        assert_eq!(em_grupos("0427"), "04 27");
        assert_eq!(em_grupos("42"), "42");
        assert_eq!(para_ler("0427"), "0 4 2 7");
        assert_eq!([posicao(0), posicao(1), posicao(2)], ["primeiro", "segundo", "terceiro"]);
    }

    #[test]
    fn as_frases_do_desfecho() {
        assert_eq!(desfecho(FaseDoCodigo::RecusadoAqui), Some(FRASE_RECUSADO_AQUI));
        assert_eq!(desfecho(FaseDoCodigo::RecusadoLa), Some(FRASE_RECUSADO_LA));
        assert_eq!(desfecho(FaseDoCodigo::NaoConferiu), Some(FRASE_NAO_CONFERIU));
        assert_eq!(desfecho(FaseDoCodigo::Expirou), Some(FRASE_EXPIROU));
        assert_eq!(desfecho(FaseDoCodigo::Conferiu), None);
        assert_eq!(desfecho(FaseDoCodigo::Mostrando), None);
    }

    /// O anfitrião: nada antes da primeira tentativa nem com os números na tela; a frase da última
    /// depois; P11 depois de uma escolha errada; e o código esgotado depois da terceira.
    #[test]
    fn o_aviso_do_anfitriao() {
        assert_eq!(aviso_do_anfitriao(&do_anfitriao(FaseDoCodigo::Esperando, 0)), None);
        assert_eq!(aviso_do_anfitriao(&do_anfitriao(FaseDoCodigo::Mostrando, 3)), None);
        assert_eq!(aviso_do_anfitriao(&do_anfitriao(FaseDoCodigo::ConfirmadoAqui, 1)), None);
        assert_eq!(aviso_do_anfitriao(&do_anfitriao(FaseDoCodigo::RecusadoLa, 2)), Some(FRASE_RECUSADO_LA));
        assert_eq!(aviso_do_anfitriao(&do_anfitriao(FaseDoCodigo::RecusadoAqui, 1)), Some(FRASE_RECUSADO_AQUI), "Nenhum destes");
        let errada = LeituraDoCodigo { escolha_errada: true, ..do_anfitriao(FaseDoCodigo::RecusadoAqui, 1) };
        assert_eq!(aviso_do_anfitriao(&errada), Some(FRASE_ESCOLHA_ERRADA), "P11: o código acabou, o PIN continua");
        assert!(FRASE_ESCOLHA_ERRADA.contains("PIN continua"));
        assert_eq!(aviso_do_anfitriao(&do_anfitriao(FaseDoCodigo::Expirou, 3)), Some(FRASE_CODIGO_ESGOTADO));
    }

    fn falha(tipo: TipoDoErro, fase: Option<FaseDoCodigo>) -> FalhaDoConvidado {
        FalhaDoConvidado { tipo, fase, recusa_ja_pareado: false, alvo_com_vinculo: false, procura: false }
    }

    #[test]
    fn a_falha_do_convidado() {
        // Com PIN digitado não há confirmador: quem explica é o status.
        assert_eq!(falha_do_convidado(&falha(TipoDoErro::NeedsPin, None)), None);
        let f = |tipo, fase| falha_do_convidado(&falha(tipo, Some(fase))).map(|c| (c.frase, c.pede_pin, c.esquecer));
        assert_eq!(f(TipoDoErro::Outro, FaseDoCodigo::RecusadoLa), Some((FRASE_RECUSADO_LA, false, false)));
        assert_eq!(f(TipoDoErro::NeedsPin, FaseDoCodigo::NaoConferiu), Some((FRASE_NAO_CONFERIU, false, false)));
        assert_eq!(f(TipoDoErro::NeedsPin, FaseDoCodigo::Esperando), Some((FRASE_NAO_ACEITA_O_CODIGO, true, false)));
        // O código nem começou e a falha é de rede: o status explica.
        assert_eq!(f(TipoDoErro::Outro, FaseDoCodigo::Esperando), None);
    }

    /// A1, A2 e B2 pelas variantes próprias do núcleo (`docs/v3-no-privado.md` §8.3).
    #[test]
    fn a1_a2_e_b2_no_convidado() {
        let c = falha_do_convidado(&falha(TipoDoErro::Interferencia, Some(FaseDoCodigo::Esperando))).unwrap();
        assert_eq!((c.frase, c.pede_pin, c.esquecer), (FRASE_INTERFERENCIA, true, false), "A1: use o PIN");
        let c = falha_do_convidado(&falha(TipoDoErro::JaPareado, Some(FaseDoCodigo::NaoConferiu))).unwrap();
        assert_eq!((c.frase, c.pede_pin), (FRASE_JA_PAREADO, true), "B2 vem antes da frase genérica do código");
        // B2 pelo motivo do confirmador, quando quem recusou foi o outro lado.
        let pelo_motivo = FalhaDoConvidado { recusa_ja_pareado: true, ..falha(TipoDoErro::Outro, Some(FaseDoCodigo::RecusadoLa)) };
        assert_eq!(falha_do_convidado(&pelo_motivo).unwrap().frase, FRASE_JA_PAREADO);
        // A2: falha fechado, com o Esquecer, com ou sem alvo marcado.
        let c = falha_do_convidado(&falha(TipoDoErro::ParNaoReconhecido, None)).unwrap();
        assert_eq!((c.frase, c.pede_pin, c.esquecer), (FRASE_NAO_RECONHECEU, true, true));
        let a2 = FalhaDoConvidado { alvo_com_vinculo: true, ..falha(TipoDoErro::Pairing, None) };
        assert_eq!(falha_do_convidado(&a2).unwrap().frase, FRASE_NAO_RECONHECEU);
        // A2 na procura da sessão salva: o prazo é "não achei", com o nome; sem vínculo, o PIN.
        let procura = |tipo| FalhaDoConvidado { procura: true, alvo_com_vinculo: true, ..falha(tipo, None) };
        let c = falha_do_convidado(&procura(TipoDoErro::Timeout)).unwrap();
        assert_eq!((c.frase, c.com_nome, c.esquecer), (MOLDE_NAO_ACHEI, true, true));
        assert_eq!(falha_do_convidado(&procura(TipoDoErro::NeedsPin)).unwrap().frase, FRASE_SEM_VINCULO);
        assert_eq!(falha_do_convidado(&procura(TipoDoErro::Pairing)).unwrap().frase, FRASE_NAO_RECONHECEU);
        assert_eq!(falha_do_convidado(&procura(TipoDoErro::Outro)), None, "a rede: o status explica");
    }

    /// B2 no anfitrião: o motivo do confirmador vira a frase.
    #[test]
    fn o_b2_no_anfitriao() {
        let l = LeituraDoCodigo { recusa: Some(RECUSA_JA_PAREADO.into()), ..do_anfitriao(FaseDoCodigo::NaoConferiu, 1) };
        assert_eq!(aviso_do_anfitriao(&l), Some(FRASE_JA_PAREADO));
        let outra = LeituraDoCodigo { recusa: Some("outro".into()), ..do_anfitriao(FaseDoCodigo::NaoConferiu, 1) };
        assert_eq!(aviso_do_anfitriao(&outra), Some(FRASE_NAO_CONFERIU));
    }

    #[test]
    fn a_sessao_salva_vai_e_volta() {
        let s = SessaoSalva { device_id: "android-1a2b3c4d".into(), nome: "Galaxy\nA07".into(), endereco: "192.168.15.9:7877".into() };
        let texto = s.escrever();
        assert_eq!(texto.lines().count(), 4, "a quebra no nome não vira linha: {texto:?}");
        let lida = SessaoSalva::ler(&texto).expect("lida");
        assert_eq!(lida.device_id, "android-1a2b3c4d");
        assert_eq!(lida.nome, "Galaxy A07");
        assert_eq!(lida.endereco, "192.168.15.9:7877");
        assert_eq!(SessaoSalva::ler(""), None);
        assert_eq!(SessaoSalva::ler("outro formato\nendereco=1.2.3.4:7877"), None);
        assert_eq!(SessaoSalva::ler("quall-sessao-exibir 1\nid=x\n"), None, "sem endereço não é sessão");
        // Chave desconhecida (uma versão futura) é ignorada; `=` no valor fica.
        let futura = SessaoSalva::ler("quall-sessao-exibir 1\nid=a\nnovo=1\nnome=a=b\nendereco=10.0.0.2:7877\n").unwrap();
        assert_eq!(futura.nome, "a=b");
    }

    /// M3: o id gravado é o da sessão autenticada; o endereço, o que conectou.
    #[test]
    fn depois_de_conectar_grava_o_id_autenticado() {
        let antes = SessaoSalva { device_id: "android-velho".into(), nome: "A07".into(), endereco: "192.168.15.9:7877".into() };
        let nova = SessaoSalva::depois_de_conectar(Some(&antes), "android-novo", "A07", "192.168.15.23:7877");
        assert_eq!((nova.device_id.as_str(), nova.endereco.as_str()), ("android-novo", "192.168.15.23:7877"));
        // Sem id do par: mantém o de antes só no mesmo endereço.
        assert_eq!(SessaoSalva::depois_de_conectar(Some(&antes), "", "A07", "192.168.15.9:7877").device_id, "android-velho");
        assert_eq!(SessaoSalva::depois_de_conectar(Some(&antes), "", "A07", "192.168.15.23:7877").device_id, "");
        assert_eq!(SessaoSalva::depois_de_conectar(None, "", "A07", "1.2.3.4:7877").device_id, "");
    }

    #[test]
    fn o_caminho_do_exibir() {
        let s = SessaoSalva { device_id: "android-1".into(), nome: "A07".into(), endereco: "192.168.15.9:7877".into() };
        let ao_alvo = CaminhoDoExibir::AoAlvo { device_id: "android-1".into() };
        assert_eq!(caminho_do_exibir(OrigemDoDestino::LembradoNaLista, false, Some(&s), true), ao_alvo, "a linha da sessão salva: o toque (§8.6)");
        assert_eq!(caminho_do_exibir(OrigemDoDestino::Digitado(" 192.168.15.9:7877 "), false, Some(&s), true), ao_alvo, "o endereço salvo: ao alvo (A2)");
        assert_eq!(caminho_do_exibir(OrigemDoDestino::Digitado("192.168.15.10:7877"), false, Some(&s), true), CaminhoDoExibir::Livre, "outro endereço");
        assert_eq!(caminho_do_exibir(OrigemDoDestino::VistoNaLista, false, Some(&s), true), CaminhoDoExibir::Livre, "anônimo da lista");
        assert_eq!(caminho_do_exibir(OrigemDoDestino::LembradoNaLista, true, Some(&s), true), CaminhoDoExibir::Livre, "o PIN substitui");
        assert_eq!(caminho_do_exibir(OrigemDoDestino::LembradoNaLista, false, Some(&s), false), CaminhoDoExibir::Livre, "sem vínculo");
        let sem_id = SessaoSalva { device_id: String::new(), ..s.clone() };
        assert_eq!(caminho_do_exibir(OrigemDoDestino::LembradoNaLista, false, Some(&sem_id), true), CaminhoDoExibir::Livre);
        assert_eq!(caminho_do_exibir(OrigemDoDestino::LembradoNaLista, false, None, true), CaminhoDoExibir::Livre);
    }

    /// Bancada de 13/10: o aparelho que esqueceu este computador aparecia em duas linhas.
    #[test]
    fn o_item_vivo_no_endereco_da_sessao_salva_e_ela() {
        let s = SessaoSalva { device_id: "ios-7".into(), nome: "Iphone 7".into(), endereco: "192.168.15.6:7877".into() };
        let end = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(item_da_sessao_salva(&s, &end(&["192.168.15.9:7877", "192.168.15.6:7877"]), &[None, None]), Some(1));
        assert_eq!(item_da_sessao_salva(&s, &end(&["192.168.15.6:7979"]), &[None]), None, "outra porta, outro papel");
        assert_eq!(item_da_sessao_salva(&s, &end(&["192.168.15.6:7877"]), &[Some("mac-2")]), None, "a sonda disse que é outro par");
        assert_eq!(
            item_da_sessao_salva(&s, &end(&["192.168.15.6:7877", "192.168.15.8:7877"]), &[None, Some("ios-7")]),
            None,
            "reconhecido noutro endereço: é aquele"
        );
        assert_eq!(item_da_sessao_salva(&SessaoSalva { device_id: String::new(), ..s }, &end(&["192.168.15.6:7877"]), &[None]), None);
    }

    #[test]
    fn a_sessao_salva_na_lista() {
        let s = SessaoSalva { device_id: "android-1".into(), nome: "A07".into(), endereco: "192.168.15.9:7877".into() };
        assert!(lembrar_na_lista(Some(&s), true, &[]));
        assert!(lembrar_na_lista(Some(&s), true, &["mac-2"]));
        assert!(!lembrar_na_lista(Some(&s), true, &["android-1"]), "reconhecido na busca: já está na lista");
        assert!(!lembrar_na_lista(Some(&s), false, &[]), "esquecido: some da lista");
        assert!(!lembrar_na_lista(None, true, &[]));
        assert!(!lembrar_na_lista(Some(&SessaoSalva { device_id: String::new(), ..s }), true, &[]));
    }

    /// §5.4: o item que a sonda reconheceu vai ao alvo dele (a A2 vale a partir da lista).
    #[test]
    fn o_item_reconhecido_vai_ao_alvo() {
        let ao_alvo = CaminhoDoExibir::AoAlvo { device_id: "mac-2".into() };
        assert_eq!(caminho_do_exibir(OrigemDoDestino::ReconhecidoNaLista("mac-2"), false, None, true), ao_alvo);
        assert_eq!(caminho_do_exibir(OrigemDoDestino::ReconhecidoNaLista("mac-2"), true, None, true), CaminhoDoExibir::Livre, "o PIN substitui");
        assert_eq!(caminho_do_exibir(OrigemDoDestino::ReconhecidoNaLista("mac-2"), false, None, false), CaminhoDoExibir::Livre, "esquecido aqui");
    }

    #[test]
    fn os_nomes_dos_pares() {
        let mut n = NomesDosPares::default();
        n.guardar("mac-2", "MacBook do\tBruno");
        n.guardar("android-1", "Galaxy A07");
        n.guardar("mac-2", "MacBook do Bruno");
        assert_eq!(n.0[0], ("mac-2".to_string(), "MacBook do Bruno".to_string()), "o mais recente primeiro, sem repetir");
        assert_eq!(n.0.len(), 2);
        let lido = NomesDosPares::ler(&n.escrever());
        assert_eq!(lido, n);
        assert_eq!(lido.nome("android-1"), Some("Galaxy A07"));
        assert_eq!(NomesDosPares::ler("outro\nmac-2\tX"), NomesDosPares::default());
        let mut m = lido.clone();
        m.esquecer("mac-2");
        assert_eq!(m.nome("mac-2"), None);
        n.guardar("", "x");
        n.guardar("y", " ");
        assert_eq!(n.0.len(), 2, "sem id ou sem nome não entra");
    }

    /// §8.7: nunca escolhido é o nome do computador; apagado, a rede fica sem nome e a sessão com o do
    /// computador.
    #[test]
    fn o_nome_na_rede_tem_padrao_e_pode_ser_apagado() {
        assert_eq!(nome_na_rede(None, "DELL-G3"), "DELL-G3");
        assert_eq!(nome_na_rede(Some(" Estúdio\r\n"), "DELL-G3"), "Estúdio");
        assert_eq!(nome_na_rede(Some(""), "DELL-G3"), "");
        assert_eq!(nome_da_sessao(Some(""), "DELL-G3"), "DELL-G3");
        assert_eq!(nome_da_sessao(Some("Câmera 2"), "DELL-G3"), "Câmera 2");
        assert_eq!(nome_da_sessao(None, "DELL-G3"), "DELL-G3");
    }

    /// §8.7: o nome guardado > o anunciado (com o alias embaixo) > o alias.
    #[cfg(feature = "core")]
    #[test]
    fn a_espera_diz_o_nome_que_os_outros_veem_com_o_alias() {
        let alias = "Quall a71bca2b";
        assert_eq!(nome_da_espera("Dell da sala", alias), Some(("Dell da sala".into(), Some(alias.into()))));
        assert_eq!(nome_da_espera("", alias), Some((alias.into(), None)), "sem nome: o alias, como antes");
        assert_eq!(nome_da_espera("Dell", ""), None, "sem anúncio: nada");
        assert_eq!(nome_da_espera_em_linha("Dell da sala", alias), "Dell da sala (Quall a71bca2b)");
        assert_eq!(nome_da_espera_em_linha("", alias), alias);
        assert_eq!(nome_da_espera_em_linha("Dell", ""), "");
    }

    #[cfg(feature = "core")]
    #[test]
    fn a_linha_mostra_o_nome_anunciado_com_o_alias_embaixo() {
        let alias = "Quall 46eb4d0a";
        assert_eq!(rotulo_da_linha(None, Some("iPhone 7"), alias), ("iPhone 7".into(), Some(alias.into())));
        assert_eq!(rotulo_da_linha(Some("Galaxy A07"), Some("Impostor"), alias), ("Galaxy A07".into(), None));
        assert_eq!(rotulo_da_linha(None, None, alias), (alias.into(), None));
    }
}
