//! O estado do teleprompter: **dois escritores, e vale o último que mudou**.
//!
//! O contrato está em `docs/contrato-teleprompter.md`; este módulo é a forma executável dele. Mora
//! no núcleo, e não em cada casca, pelo motivo de `session.rs`: a regra de fusão é sutil (carimbo
//! de Lamport com piso de parede, desempate por autor), e quatro implementações — Swift, Kotlin,
//! o C do JNI e o Rust do Windows — seriam quatro jeitos diferentes de errar a mesma regra. As
//! cascas Swift e Kotlin chegam aqui pela fronteira C (`quall_teleprompter_*`); o Windows, direto.
//!
//! # O desenho, em uma tela
//!
//! - Cada campo é um **registrador "último escritor vence"** com carimbo próprio `(carimbo,
//!   autor)`. Campo por campo, e não o estado num bloco: o controle mudando a velocidade enquanto
//!   alguém no prompter liga o espelho mantém **as duas** mudanças.
//! - O **texto** é um registrador à parte, que viaja numa mensagem própria (`"tipo":"texto"`) e só
//!   quando muda ou quando o outro lado mostra que não o tem. O resto viaja no `"tipo":"estado"`,
//!   que leva só a **referência** do texto. Um toque em pausar nunca carrega o roteiro.
//! - O carimbo é um **relógio de Lamport com piso no relógio de parede**: toda mudança local faz
//!   `relogio = max(relogio + 1, agora_ms)`, toda mensagem recebida faz `relogio = max(relogio,
//!   maior carimbo que veio)`. Causalidade dentro da sessão (quem editou depois de ver a edição do
//!   outro sempre vence) e "a mais recente vence" entre sessões.
//! - A fusão é idempotente, comutativa e associativa: perda, duplicata, desordem e reenvio não
//!   mudam para onde as duas réplicas convergem. O reenvio periódico leva o carimbo **original**,
//!   então uma edição antiga que chega depois de uma nova simplesmente perde a comparação.
//!
//! # O que este módulo **não** faz
//!
//! Não mistura textos: duas edições concorrentes do roteiro não viram um texto costurado — a
//! vencedora entra inteira. Fusão por caractere seria um CRDT de texto, e não está no contrato.

#![cfg(feature = "webrtc")]

use std::cmp::Ordering as Ordem;
use std::ops::RangeInclusive;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::protocol::Papel;
use crate::transport::{Mensageiro, ParDaSessao, TETO_DA_MENSAGEM};

// =============================================================================================
// O contrato, em constantes
// =============================================================================================

/// O valor da chave `"app"` de toda mensagem do teleprompter.
pub const APP: &str = "teleprompter";

/// A versão **deste contrato** (a chave `"v"`). Campo aditivo não a sobe; quem recebe ignora chave
/// que não conhece. Mensagem com outra versão é descartada e contada.
pub const VERSAO: u64 = 1;

/// **O teto do roteiro**: 131 072 bytes de UTF-8 (~20 mil palavras, mais de duas horas de fala).
///
/// Metade do [`TETO_DA_MENSAGEM`], para que o texto caiba numa mensagem mesmo com o que o JSON
/// acrescenta ao escapar aspas e quebras de linha. [`Replica::definir_texto`] confere as duas
/// coisas: o texto cru e a mensagem pronta.
pub const TETO_DO_TEXTO: usize = 128 * 1024;

/// Velocidade, em linhas por segundo (linha = altura da linha na fonte do prompter).
pub const FAIXA_DA_VELOCIDADE: RangeInclusive<f64> = 0.05..=20.0;
/// Fonte, em pontos lógicos (pt no iOS e no Mac, sp no Android, DIP no Windows).
pub const FAIXA_DA_FONTE: RangeInclusive<f64> = 8.0..=400.0;
/// Margem, fração da largura, de cada lado.
pub const FAIXA_DA_MARGEM: RangeInclusive<f64> = 0.0..=0.45;
/// Linha de leitura, fração da altura a partir do topo.
pub const FAIXA_DA_LINHA: RangeInclusive<f64> = 0.0..=1.0;
/// Posição e salto: fração do percurso — 0 é o começo na linha de leitura, 1 é o fim nela.
pub const FAIXA_DA_POSICAO: RangeInclusive<f64> = 0.0..=1.0;

pub const VELOCIDADE_PADRAO: f64 = 1.0;
pub const FONTE_PADRAO: f64 = 48.0;
pub const MARGEM_PADRAO: f64 = 0.1;
pub const LINHA_PADRAO: f64 = 0.3;

/// Sem mudança nenhuma, o estado sai a cada segundo: é o batimento, e é o reparo de qualquer perda.
pub const BATIMENTO: Duration = Duration::from_millis(1000);
/// A posição sozinha (o relato de quem rola) sai no máximo a cada 250 ms.
pub const INTERVALO_DA_POSICAO: Duration = Duration::from_millis(250);
/// O texto pedido pelo outro lado é reenviado no máximo a cada 2 s — sem isto, com o texto em
/// trânsito, cada estado que chegasse mostrando a referência velha pediria outro envio. Os 2 s
/// contam de **qualquer** envio de texto, o original ou um reenvio.
pub const REENVIO_DO_TEXTO: Duration = Duration::from_millis(2000);

/// **O reenvio rápido do grupo do "segurar"** (§12.3): depois de apertar, de soltar e da parada do
/// silêncio, o estado sai de novo nestes instantes, contados da mudança, **até o outro lado mostrar
/// que tem o grupo** — no máximo três vezes. Só a mudança do grupo agenda; o resto do estado segue o
/// batimento.
///
/// Por quê: o soltar que se perde no enlace só chegava pela retransmissão do SCTP (o RTO mínimo do
/// libdatachannel é 200 ms, `vendor/datachannel-sys/libdatachannel/src/impl/sctptransport.cpp:126-128`,
/// e dobra a cada perda) ou pelo batimento de 1 s — o texto seguindo até 1 s depois de a
/// pessoa tirar o dedo (medido pela Frente I em 14/09: 2 de 20 soltares num iPhone X na Wi-Fi em 489 e
/// 1 004,7 ms, os outros 18 em até ~130 ms). Os números: **50 ms** já passa da confirmação comum (uma
/// ida e volta, abaixo de 1 ms em loopback), então o primeiro reenvio quase só sai quando algo se
/// perdeu ou atrasou; os intervalos dobram (50, 100, 200 ms) para não caírem todos na mesma rajada de
/// perda; e os três terminam em 350 ms, antes de a retransmissão do SCTP chegar ao segundo RTO. Com
/// perdas independentes de 30 %, o aperto ou o soltar só fica sem nenhuma das quatro cópias em
/// 0,3⁴ ≈ 0,8 % das vezes; aí sobram o SCTP e o batimento. Uma duplicata não faz mal: a fusão é pelo
/// carimbo, e o outro lado não responde a ela.
pub const REENVIOS_DO_SEGURAR: [Duration; 3] =
    [Duration::from_millis(50), Duration::from_millis(150), Duration::from_millis(350)];

/// **O texto só sai com o buffer de saída quase vazio.** A fila de saída da libdatachannel não
/// tem teto (`sctptransport.cpp:163`, `:387-393`) e o SCTP daqui não intercala mensagens: um texto
/// de 128 KiB entra na frente de tudo o que vier depois, e num enlace lento os reenvios se
/// empilhariam. Com mais que isto esperando para sair, o texto espera a próxima bombeada; o estado
/// (pequeno) continua saindo.
pub const LIMITE_DO_BUFFER_PARA_TEXTO: usize = 16 * 1024;

/// **Um carimbo mais de 24 h à frente do relógio daqui é recusado**, na chegada e no salvo.
///
/// Sem teto, um aparelho com o relógio errado (ou um carimbo perto de `u64::MAX`) venceria toda
/// edição até o relógio de parede alcançá-lo — e, persistido, **contaminaria** todo aparelho que
/// sincronizasse com ele. Com o teto, enquanto um relógio estiver mais de um dia errado, as
/// edições dele não entram aqui (e são contadas em `carimbos_do_futuro`, que a tela pode mostrar).
pub const TOLERANCIA_DO_FUTURO: Duration = Duration::from_secs(24 * 3600);

/// O `device_id` que assina as edições. Sem teto no `DeviceId`; aqui há, porque ele vai em cada
/// campo de cada estado — e é conferido na edição, no salvo e na chegada.
pub const TETO_DO_AUTOR: usize = 256;

/// **A rede de segurança dos reenvios**: quantas vezes o mesmo texto é reenviado, numa sessão, a
/// um par que continua mostrando que não o tem. Depois disso, desiste (e conta
/// `reenvios_desistidos`) até o texto mudar ou a sessão ser outra.
///
/// A regra principal do defeito 5 da revisão de 13/09 é outra, e precisa: se o nosso texto está
/// mais de [`TOLERANCIA_DO_FUTURO`] à frente do relógio que o par mostra, ele o recusaria com
/// certeza, e o texto não é reenviado. Este teto é para uma recusa que não se sabe prever. Ele é
/// alto de propósito: a primeira versão usava 3, e a simulação com 30 % de perda achou uma semente
/// em que quatro envios seguidos se perderam e a réplica desistiu de convergir. Com 20 (40 s a um
/// reenvio a cada 2 s), perder todos sob 30 % de perda é chance de 10⁻¹¹; e numa sessão de
/// teleprompter o canal é confiável, então isto só conta recusa.
pub const REENVIOS_DO_MESMO_TEXTO: u32 = 20;

/// **Quantas cópias do roteiro o controle guarda** (`docs/contrato-teleprompter.md` §11.5): o que
/// saiu numa pergunta, o que perdeu uma fusão antes da convergência, o nosso texto que mudou desde a
/// última convergência e perdeu. A mais nova primeiro; a quarta tira a mais velha; sem repetição
/// pelo resumo. Vão no salvo.
pub const COPIAS_DO_TEXTO: usize = 3;

/// A prévia de um texto no estado da tela: os primeiros até 240 bytes, cortados numa fronteira de
/// caractere.
pub const PREVIA_DO_TEXTO: usize = 240;

/// **O teto do motivo de uma recusa de gravação** (`docs/contrato-teleprompter.md` §13), em bytes de
/// UTF-8. Na recusa local, acima disso (ou vazio, ou com NUL) é [`Error::Invalid`]; na chegada, o
/// motivo é cortado numa fronteira de caractere — uma recusa nunca some por causa do texto dela.
pub const TETO_DO_MOTIVO: usize = 256;

/// **O motivo da recusa que o próprio núcleo dá** a um pedido de gravação que chega a um prompter
/// cuja tela não liga a gravação (§13.3). Um controle só pede a quem diz que grava, então isto só
/// aparece numa corrida — a tela do prompter desligou a gravação com o pedido já no fio.
pub const MOTIVO_SEM_A_TELA: &str = "o prompter não está na tela que grava";

/// **A maior duração de gravação que o controle aceita no fio** (§13.1): 30 dias. Acima disso o
/// campo é recusado e contado — um valor absurdo ficaria preso a gravação inteira, pela regra do
/// começo mais cedo (revisão de 24/09, m3).
pub const TETO_DA_GRAVACAO: Duration = Duration::from_secs(30 * 24 * 3600);

/// **A resolução de cada número**, aplicada na edição local. Duas razões: o valor que a casca
/// relê e reescreve (um `Float` do Android que vira `0.10000000149…` em `double`) não gera carimbo
/// novo, e o JSON fica legível. Velocidade em centésimos, fonte em décimos, frações em 1/10000.
const PASSO_DA_VELOCIDADE: f64 = 100.0;
const PASSO_DA_FONTE: f64 = 10.0;
const PASSO_DA_FRACAO: f64 = 10_000.0;

fn quantizar(valor: f64, por_unidade: f64) -> f64 {
    (valor * por_unidade).round() / por_unidade
}

/// **A regra única do texto** — na edição, no salvo e na chegada (defeito 4b da revisão: o salvo
/// só conferia tamanho cru e NUL, e um texto de 50 mil caracteres de controle, 50 KB cru e 300 KB
/// escapado, entrava e travava o envio).
///
/// Até [`TETO_DO_TEXTO`] bytes crus, sem NUL, autor até [`TETO_DO_AUTOR`], e a mensagem pronta —
/// no pior caso de carimbo e relógio — dentro de [`TETO_DA_MENSAGEM`]. Texto só de aspas e barras
/// cresce quase o dobro ao escapar; texto de caracteres de controle, seis vezes: por isso a conta
/// é feita na mensagem, e não no texto.
fn texto_cabe(texto: &str, autor: &str) -> std::result::Result<(), String> {
    if texto.len() > TETO_DO_TEXTO {
        return Err(format!("o texto tem {} bytes e o teto é {TETO_DO_TEXTO}", texto.len()));
    }
    if texto.contains('\0') {
        return Err("o texto tem um NUL no meio".into());
    }
    if autor.len() > TETO_DO_AUTOR {
        return Err(format!("autor de {} bytes; o teto é {TETO_DO_AUTOR}", autor.len()));
    }
    let prova = MensagemDeTexto {
        app: APP.into(),
        v: VERSAO,
        tipo: "texto".into(),
        relogio: u64::MAX,
        texto: Registro { valor: texto.to_string(), carimbo: u64::MAX, autor: autor.to_string() },
    };
    let tamanho = serde_json::to_string(&prova).map(|s| s.len()).unwrap_or(usize::MAX);
    if tamanho > TETO_DA_MENSAGEM {
        return Err(format!(
            "o texto, escapado em JSON, dá uma mensagem de {tamanho} bytes e o teto do canal é \
             {TETO_DA_MENSAGEM}"
        ));
    }
    Ok(())
}

/// **O que o prompter faz quando o controle some.** Decidido pelo usuário em 13/09/2026: o
/// prompter **continua no estado em que estava** — rolando segue rolando, parado segue parado — e
/// os dois aparelhos mostram um aviso visível até o controle voltar
/// (`docs/contrato-teleprompter.md` §2). Um lugar só, para a regra não se espalhar pelas cascas.
pub const POLITICA_SEM_PAR: PoliticaSemPar = PoliticaSemPar::ContinuaComoEsta;

/// **O que acontece quando o outro lado some com o dedo no botão** do modo "segurar para rolar"
/// (`docs/contrato-teleprompter.md` §12). Decidido pelo usuário em 14/09/2026: **o texto para**, como
/// se a pessoa tivesse soltado — a exceção à [`POLITICA_SEM_PAR`], só enquanto `segurando`. Vale nos
/// dois lados, na queda da sessão ([`Replica::perdeu_o_par`]) e em [`PAR_SUMIDO`] de silêncio. Na
/// queda, a parada do controle é só da réplica dele e não viaja: o prompter para sozinho (§12.4).
pub const POLITICA_SEM_PAR_AO_SEGURAR: PoliticaSemPar = PoliticaSemPar::Pausa;

/// Ver [`POLITICA_SEM_PAR`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoliticaSemPar {
    /// Continua no estado em que está: se rolava, segue rolando. As duas telas avisam.
    ContinuaComoEsta,
    /// Para de rolar.
    Pausa,
}

/// Os bits do que mudou **por causa do outro lado** numa bombeada. Os valores são os de
/// `QUALL_TELEPROMPTER_CHANGED_*` na fronteira C.
pub mod mudou {
    pub const TEXTO: u32 = 1 << 0;
    pub const ROLANDO: u32 = 1 << 1;
    pub const VELOCIDADE: u32 = 1 << 2;
    pub const FONTE: u32 = 1 << 3;
    pub const MARGEM: u32 = 1 << 4;
    pub const LINHA_DE_LEITURA: u32 = 1 << 5;
    pub const ESPELHO: u32 = 1 << 6;
    pub const POSICAO: u32 = 1 << 7;
    /// Chegou um salto novo: quem mostra o texto vai para `estado().salto`.
    pub const SALTO: u32 = 1 << 8;
    /// Mudou o contato com o outro lado: sumiu, voltou, ou a confirmação das edições daqui mudou.
    pub const PAR: u32 = 1 << 9;
    /// Mudou `pergunta_do_texto` no estado: abriu, entrou em "comparando", o texto do prompter nela
    /// mudou, ou fechou por causa do outro lado ou de uma sessão nova (§11.6 do contrato).
    pub const PERGUNTA_DO_TEXTO: u32 = 1 << 10;
    /// Há cópia nova do roteiro (ou a lista mudou): **grave o salvo** (§11.5). Acende na bombeada
    /// seguinte a qualquer cópia nova, inclusive a que uma chamada daqui fez.
    pub const COPIA_DO_TEXTO: u32 = 1 << 11;
    /// Mudou `para_tras` ou `segurando` (o modo "segurar para rolar", §12): quem mostra o texto
    /// relê `rolando` e `para_tras`; o controle relê `segurando`.
    pub const SEGURAR: u32 = 1 << 12;
    /// A gravação (§13). **No prompter**: chegou um pedido do controle — releia
    /// `"pedido_de_gravacao"` e faça `definir_gravando(gravar)` ou `recusar_gravacao(n, motivo)`. **No
    /// controle**: a gravação começou ou parou (`"gravando_ha_ms"`), o pedido daqui foi respondido
    /// (`"pedido_de_gravacao"` voltou a nulo, e `"gravacao_recusada"` diz se foi recusado), ou o
    /// prompter passou a dizer, ou deixou de dizer, que grava (`"par_entende_gravar"`).
    pub const GRAVACAO: u32 = 1 << 13;
}

/// O que mudou numa bombeada. Ver [`mudou`].
pub type Mudancas = u32;

// =============================================================================================
// O registrador
// =============================================================================================

/// Um registrador "último escritor vence". No fio: `{"valor": …, "carimbo": u64, "autor": "…"}`.
///
/// Carimbo `0` com autor `""` é "ninguém escreveu ainda", e **nunca vence** — nem de outro
/// registrador vazio. É o que torna seguro um campo ausente numa mensagem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Registro<T> {
    pub(crate) valor: T,
    pub(crate) carimbo: u64,
    pub(crate) autor: String,
}

impl<T: Default> Default for Registro<T> {
    fn default() -> Self {
        Registro { valor: T::default(), carimbo: 0, autor: String::new() }
    }
}

/// A ordem total dos valores, só para o desempate de último recurso (carimbo e autor iguais, o que
/// dois aparelhos distintos nunca produzem). Existe para a convergência não depender de ninguém
/// ter configurado dois aparelhos com o mesmo `device_id`.
pub(crate) trait Valor {
    fn ordem(&self, outro: &Self) -> Ordem;
}

impl Valor for bool {
    fn ordem(&self, outro: &Self) -> Ordem {
        self.cmp(outro)
    }
}

impl Valor for f64 {
    fn ordem(&self, outro: &Self) -> Ordem {
        self.total_cmp(outro)
    }
}

impl Valor for Option<f64> {
    fn ordem(&self, outro: &Self) -> Ordem {
        match (self, outro) {
            (None, None) => Ordem::Equal,
            (None, Some(_)) => Ordem::Less,
            (Some(_), None) => Ordem::Greater,
            (Some(a), Some(b)) => a.total_cmp(b),
        }
    }
}

impl Valor for String {
    /// **Pelo resumo, e só depois pelos bytes.** Tem de ser a mesma ordem que a anti-entropia usa
    /// ao comparar a referência do texto (que leva o resumo, não o conteúdo): se as duas ordens
    /// discordassem, um lado acharia que o outro está atrás e reenviaria o texto a cada 2 s, e o
    /// outro o recusaria a cada 2 s — para sempre.
    fn ordem(&self, outro: &Self) -> Ordem {
        resumo(self)
            .cmp(&resumo(outro))
            .then_with(|| self.as_bytes().cmp(outro.as_bytes()))
    }
}

/// **O resumo do texto**: os primeiros 8 bytes do SHA-256, em hex. Vai na referência do texto de
/// todo `estado`, e é o que deixa dois textos com o mesmo carimbo e o mesmo autor serem
/// distinguidos — o que acontece quando dois aparelhos têm o mesmo `device_id`, e **acontece**: o
/// iOS guarda o id no App Group (`Identidade.swift`), e restaurar um iPhone num iPad copia o id.
pub fn resumo(texto: &str) -> String {
    let d = Sha256::digest(texto.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

impl<T: Valor> Registro<T> {
    /// `novo` vence `self`? Carimbo, depois autor (em bytes), depois o valor.
    fn perde_para(&self, novo: &Registro<T>) -> bool {
        if novo.carimbo == 0 {
            return false;
        }
        novo.carimbo
            .cmp(&self.carimbo)
            .then_with(|| novo.autor.as_bytes().cmp(self.autor.as_bytes()))
            .then_with(|| novo.valor.ordem(&self.valor))
            == Ordem::Greater
    }

    /// Funde `novo` em `self`. Devolve se mudou.
    fn fundir(&mut self, novo: Registro<T>) -> bool {
        if self.perde_para(&novo) {
            *self = novo;
            true
        } else {
            false
        }
    }

    fn marca(&self) -> (u64, &str) {
        (self.carimbo, self.autor.as_str())
    }

    /// Ninguém escreveu ainda (carimbo 0). Um registro assim nunca vence, e os campos novos do
    /// `estado` (os de §12) não vão ao fio enquanto estiverem assim: o `estado` de quem nunca usou
    /// o "segurar" é o de antes, byte a byte.
    fn nunca_escrito(&self) -> bool {
        self.carimbo == 0
    }
}

fn falso(b: &bool) -> bool {
    !*b
}

// =============================================================================================
// As mensagens
// =============================================================================================

/// A referência do texto que viaja no `estado`: qual versão do roteiro quem mandou tem.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct ReferenciaDoTexto {
    carimbo: u64,
    autor: String,
    bytes: u64,
    /// Ver [`resumo`].
    #[serde(default)]
    resumo: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct MensagemDeEstado {
    app: String,
    v: u64,
    tipo: String,
    #[serde(default)]
    relogio: u64,
    #[serde(default)]
    rolando: Registro<bool>,
    #[serde(default)]
    velocidade: Registro<f64>,
    #[serde(default)]
    fonte: Registro<f64>,
    #[serde(default)]
    margem: Registro<f64>,
    #[serde(default)]
    linha_de_leitura: Registro<f64>,
    #[serde(default)]
    espelho: Registro<bool>,
    #[serde(default)]
    posicao: Registro<f64>,
    #[serde(default)]
    salto: Registro<Option<f64>>,
    // §12, o "segurar para rolar": aditivos, sem subir o `v`, e **fora do fio enquanto ninguém os
    // escreveu** — o `estado` de hoje continua byte a byte. Uma build de 13/09 ignora as chaves.
    #[serde(default, skip_serializing_if = "Registro::nunca_escrito")]
    para_tras: Registro<bool>,
    #[serde(default, skip_serializing_if = "Registro::nunca_escrito")]
    segurando: Registro<bool>,
    /// O prompter cuja **tela** rola para trás e para quando `rolando` cai diz isto; sem ele, o
    /// controle não segura (§12). Só vai quando é `true`.
    #[serde(default, skip_serializing_if = "falso")]
    entende_segurar: bool,
    // §13, a gravação: aditivos, sem subir o `v`, e **fora do fio enquanto não existem** — o
    // `estado` de quem nunca gravou nem pediu é o de antes, byte a byte.
    /// Só do prompter: a gravação, com a duração **relatada por ele** no instante do envio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gravando_ha_ms: Option<GravandoNoFio>,
    /// Só do prompter cuja tela grava: sem isto, o controle não pede.
    #[serde(default, skip_serializing_if = "falso")]
    entende_gravar: bool,
    /// Só do controle, e só enquanto o prompter não o respondeu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pedido_de_gravacao: Option<PedidoNoFio>,
    /// Só do prompter: a resposta ao último pedido que ele decidiu nesta sessão.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resposta_de_gravacao: Option<RespostaNoFio>,
    #[serde(default)]
    texto: ReferenciaDoTexto,
}

/// §13: a gravação no fio, `{"valor": ms | null, "carimbo": u64, "autor": "…"}`. É um registrador
/// cujo **carimbo é o do começo (ou da parada)** — fica o mesmo a gravação inteira — e cujo valor é
/// a duração no instante em que o prompter mandou. Nunca um instante de relógio de parede: os
/// relógios dos dois aparelhos não são comuns (revisão m-1 do R5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct GravandoNoFio {
    valor: Option<u64>,
    carimbo: u64,
    autor: String,
}

/// §13: o pedido do controle. `n` é crescente por controle (um carimbo do relógio de Lamport dele,
/// que tem piso de parede e é salvo: cresce também entre vidas do app).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PedidoNoFio {
    n: u64,
    gravar: bool,
    autor: String,
}

/// §13: a resposta do prompter ao pedido `n` do controle `autor`. Sem `gravacao_recusada`, aceito.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RespostaNoFio {
    n: u64,
    autor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gravacao_recusada: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MensagemDeTexto {
    app: String,
    v: u64,
    tipo: String,
    #[serde(default)]
    relogio: u64,
    texto: Registro<String>,
}

/// O que se guarda entre sessões. Ver [`Replica::salvo_json`].
#[derive(Debug, Serialize, Deserialize)]
struct Salvo {
    v: u64,
    #[serde(default)]
    relogio: u64,
    #[serde(default)]
    texto: Registro<String>,
    #[serde(default)]
    velocidade: Registro<f64>,
    #[serde(default)]
    fonte: Registro<f64>,
    #[serde(default)]
    margem: Registro<f64>,
    #[serde(default)]
    linha_de_leitura: Registro<f64>,
    #[serde(default)]
    espelho: Registro<bool>,
    // As três de §11.6, **só quando existem** (sem elas, o salvo é o de antes, byte a byte). Lidas
    // como `Value` e conferidas uma a uma no `carregar`: uma cópia ilegível fica de fora e é
    // contada, sem derrubar o salvo inteiro — e com ele o roteiro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ultimo_prompter_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    referencia_convergida: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copias_do_texto: Option<Value>,
}

/// Uma cópia, como vai no salvo.
#[derive(Debug, Serialize, Deserialize)]
struct CopiaSalva {
    origem: OrigemDaCopia,
    prompter_id: String,
    #[serde(default)]
    prompter_nome: String,
    #[serde(default)]
    quando_ms: u64,
    texto: String,
}

// =============================================================================================
// A pergunta do texto e as cópias (docs/contrato-teleprompter.md §11)
// =============================================================================================

/// De quem era o texto que saiu e ficou guardado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrigemDaCopia {
    /// O texto que estava neste controle.
    Controle,
    /// O texto que estava no prompter.
    Prompter,
}

/// Um texto, do jeito que a tela o mostra numa pergunta ou numa cópia: tamanho, resumo e prévia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VistaDoTexto {
    pub bytes: usize,
    pub resumo: String,
    /// Os primeiros até [`PREVIA_DO_TEXTO`] bytes, cortados numa fronteira de caractere.
    pub previa: String,
}

/// **A pergunta do texto**, no estado da tela (`"pergunta_do_texto"`). Ver o contrato, §11.4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PerguntaDoTexto {
    /// `false` enquanto compara (o texto do prompter ainda não chegou); `true` com a pergunta
    /// aberta, os dois textos em mãos e diferentes.
    pub aberta: bool,
    /// Há quanto tempo o texto deste controle está retido, nesta sessão.
    pub retido_ha_ms: u64,
    pub prompter_id: String,
    pub prompter_nome: String,
    /// O texto deste controle.
    pub meu: VistaDoTexto,
    /// O texto do prompter; `None` enquanto compara.
    pub do_prompter: Option<VistaDoTexto>,
}

/// Uma cópia guardada, no estado da tela (`"copias_do_texto"`). O texto inteiro sai por
/// [`Replica::copia_do_texto`], pelo `resumo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopiaDoTexto {
    pub origem: OrigemDaCopia,
    pub prompter_id: String,
    pub prompter_nome: String,
    /// O relógio de parede da hora da cópia, em ms desde 1970.
    pub quando_ms: u64,
    pub bytes: usize,
    pub resumo: String,
    pub previa: String,
}

/// Uma cópia, na memória.
#[derive(Debug, Clone)]
struct Copia {
    origem: OrigemDaCopia,
    prompter_id: String,
    prompter_nome: String,
    quando_ms: u64,
    texto: String,
    resumo: String,
}

/// **O texto do controle, retido ou não** (§11.3). Só o controle sai de [`Retencao::Livre`].
#[derive(Debug)]
enum Retencao {
    /// A regra de sempre: o texto se funde por "vale o último que mudou".
    Livre,
    /// Primeiro encontro, e o texto do prompter ainda não chegou: o nosso não sai, e o estado
    /// anuncia a referência de quem nunca escreveu, para o prompter mandar o dele.
    Comparando { desde_ms: u64 },
    /// Os dois textos em mãos, diferentes e não vazios: a pergunta. O do prompter fica aqui, sem
    /// fundir, e o estado anuncia a referência dele.
    Perguntando { desde_ms: u64, deles: Registro<String>, resumo_deles: String },
}

/// A prévia de um texto: os primeiros até [`PREVIA_DO_TEXTO`] bytes, numa fronteira de caractere.
fn previa(texto: &str) -> String {
    let mut corte = texto.len().min(PREVIA_DO_TEXTO);
    while !texto.is_char_boundary(corte) {
        corte -= 1;
    }
    texto[..corte].to_string()
}

/// Corta `texto` em até [`TETO_DO_AUTOR`] bytes, numa fronteira de caractere.
fn cortado(texto: &str) -> String {
    cortado_em(texto, TETO_DO_AUTOR)
}

/// Corta `texto` em até `teto` bytes, numa fronteira de caractere.
fn cortado_em(texto: &str, teto: usize) -> String {
    let mut corte = texto.len().min(teto);
    while !texto.is_char_boundary(corte) {
        corte -= 1;
    }
    texto[..corte].to_string()
}

// =============================================================================================
// A gravação (docs/contrato-teleprompter.md §13)
// =============================================================================================

/// **Um pedido de gravação**, no estado da tela (`"pedido_de_gravacao"`). No prompter, o que o
/// controle pediu e a casca ainda não decidiu; no controle, o pedido daqui que o prompter ainda não
/// respondeu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PedidoDeGravacao {
    /// O número do pedido, crescente por controle.
    pub n: u64,
    /// `true` é "gravar"; `false`, "parar".
    pub gravar: bool,
    /// Há quanto tempo (no relógio monotônico daqui) o pedido existe aqui.
    pub ha_ms: u64,
}

/// **Uma recusa**, no estado da tela (`"gravacao_recusada"`). No controle, a do último pedido daqui;
/// no prompter, a última que ele deu nesta sessão.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecusaDeGravacao {
    pub n: u64,
    pub gravar: bool,
    /// Legível, para a tela mostrar como está ("sem espaço: sobram 312 MB").
    pub motivo: String,
}

/// A gravação como esta réplica a conhece: o carimbo do começo (ou da parada) e, gravando, **quando
/// começou no relógio monotônico daqui**. No prompter é o instante de `definir_gravando(true)`; no
/// controle, a chegada menos a duração relatada — a menor dessas contas entre as mensagens da mesma
/// gravação, que é a da mensagem que menos demorou no caminho. Com sinal: no controle o começo pode
/// ser anterior à origem do relógio daqui.
#[derive(Debug, Clone, Default)]
struct Gravacao {
    carimbo: u64,
    autor: String,
    inicio_ms: Option<i64>,
}

impl Gravacao {
    fn ha_ms(&self, agora: Agora) -> Option<u64> {
        self.inicio_ms
            .map(|i| u64::try_from(i64::try_from(agora.mono_ms).unwrap_or(i64::MAX).saturating_sub(i)).unwrap_or(0))
    }
}

/// Um pedido de gravação na memória: o do controle esperando resposta, ou o que o prompter recebeu
/// e a casca ainda não decidiu.
#[derive(Debug, Clone)]
struct PedidoAberto {
    n: u64,
    gravar: bool,
    autor: String,
    desde_ms: u64,
}

// =============================================================================================
// A réplica
// =============================================================================================

/// Os dois relógios de um instante: o de parede (para carimbar) e o monotônico (para medir
/// intervalo). Separados de propósito: o NTP pode mexer no de parede no meio da sessão, e um
/// batimento medido por ele poderia nunca vencer.
#[derive(Debug, Clone, Copy)]
pub struct Agora {
    /// Milissegundos desde 1970, do relógio do aparelho.
    pub parede_ms: u64,
    /// Milissegundos monotônicos, de uma origem qualquer.
    pub mono_ms: u64,
}

/// Quanto passou pela réplica. Vai no JSON de `estado_json`, em `"contadores"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ContadoresDoTeleprompter {
    pub estados_enviados: u64,
    pub textos_enviados: u64,
    pub recebidas: u64,
    /// Não eram JSON, ou eram JSON sem a forma do contrato.
    pub invalidas: u64,
    /// JSON com outro `"app"` — de outra frente que use o mesmo canal.
    pub de_outro_app: u64,
    /// `"v"` diferente de [`VERSAO`]. **Diferente de zero, a tela deve dizer "atualize o app"**:
    /// uma sessão de pé que recebe só isto nunca vai sincronizar.
    pub de_outra_versao: u64,
    /// Um campo com valor fora da faixa, que não se lê, ou texto grande demais: ignorado, e o
    /// resto da mensagem entra.
    pub campos_recusados: u64,
    /// Carimbos mais de [`TOLERANCIA_DO_FUTURO`] à frente do relógio daqui: ignorados. Diferente
    /// de zero, o relógio de um dos dois aparelhos está errado.
    pub carimbos_do_futuro: u64,
    /// Mensagens que não havia como mandar (não cabiam no canal): saíram da lista do que é
    /// devido, sem travar a leitura.
    pub mensagens_impossiveis: u64,
    /// Vezes em que se desistiu de reenviar o texto porque o outro lado continuou mostrando que
    /// não o tem depois de [`REENVIOS_DO_MESMO_TEXTO`] reenvios — ele o recusa (o caso típico: o
    /// relógio de um dos dois mais de um dia errado). A tela pode dizer que o roteiro não passou.
    pub reenvios_desistidos: u64,
}

/// O estado para a tela desenhar. É o JSON de `quall_teleprompter_state_json`, literal:
///
/// ```json
/// {"rolando":false,"velocidade":1.0,"fonte":48.0,"margem":0.1,"linha_de_leitura":0.3,
///  "espelho":false,"posicao":0.0,"salto":null,"texto_bytes":0,"par_visto_ha_ms":null,
///  "sem_confirmacao_ha_ms":null,"contadores":{…}}
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Estado {
    pub rolando: bool,
    pub velocidade: f64,
    pub fonte: f64,
    pub margem: f64,
    pub linha_de_leitura: f64,
    pub espelho: bool,
    pub posicao: f64,
    /// O alvo do salto mais recente, ou `null` se nunca houve salto.
    pub salto: Option<f64>,
    /// O tamanho do texto, em bytes. O texto em si sai por [`Replica::texto`].
    pub texto_bytes: usize,
    /// Há quantos ms chegou a última mensagem do outro lado nesta sessão; `null` se nada chegou.
    pub par_visto_ha_ms: Option<u64>,
    /// Há quantos ms existe uma edição **daqui** que o outro lado ainda não confirmou; `null` se
    /// está tudo confirmado. Numa sessão sã a confirmação volta em uma ida e volta; acima de
    /// 1,5 s a tela deve avisar que o comando não chegou.
    pub sem_confirmacao_ha_ms: Option<u64>,
    pub contadores: ContadoresDoTeleprompter,
    /// A pergunta do texto: `None` sem nada retido (§11.4 do contrato).
    pub pergunta_do_texto: Option<PerguntaDoTexto>,
    /// As cópias guardadas, a mais nova primeiro (§11.5).
    pub copias_do_texto: Vec<CopiaDoTexto>,
    /// §12: rolar para trás (com `rolando`), na velocidade de sempre.
    pub para_tras: bool,
    /// §12: o controle está com o dedo no botão. Na queda, o texto para.
    pub segurando: bool,
    /// §12: o outro lado disse, no último estado dele, que a tela dele entende o "segurar" (rola
    /// para trás e para na queda). Sem isto, o controle não segura.
    pub par_entende_segurar: bool,
    /// §13: há quanto tempo o prompter grava, **relatado por ele** (no controle, a duração que veio
    /// somada ao tempo desde a chegada); `null` quando não grava.
    pub gravando_ha_ms: Option<u64>,
    /// §13: o pedido aberto. Ver [`PedidoDeGravacao`].
    pub pedido_de_gravacao: Option<PedidoDeGravacao>,
    /// §13: a última recusa. Ver [`RecusaDeGravacao`].
    pub gravacao_recusada: Option<RecusaDeGravacao>,
    /// §13: o outro lado disse, no último estado dele, que a tela dele grava. Sem isto, o controle
    /// não mostra o botão e não pede.
    pub par_entende_gravar: bool,
}

#[derive(Debug, Default)]
struct Envio {
    /// Um campo pequeno mudou aqui e o estado ainda não saiu.
    estado_devido: bool,
    /// Só a posição mudou aqui (o relato de quem rola).
    posicao_devida: bool,
    /// O texto mudou aqui e ainda não saiu.
    texto_local_devido: bool,
    /// O outro lado mostrou que não tem o nosso texto: sai, com o limite de [`REENVIO_DO_TEXTO`].
    texto_pedido_pelo_par: bool,
    /// Quantas vezes o texto atual foi reenviado nesta sessão porque o outro lado pediu. Ver
    /// [`REENVIOS_DO_MESMO_TEXTO`]. Zera quando o texto muda e a cada sessão.
    reenvios_do_texto: u32,
    /// Já se desistiu de reenviar o texto atual nesta sessão (para contar a desistência uma vez).
    desistiu_do_texto: bool,
    ultimo_estado_ms: Option<u64>,
    ultimo_texto_ms: Option<u64>,
    /// §12.3: o reenvio rápido do grupo do segurar em curso ([`REENVIOS_DO_SEGURAR`]).
    reenvio_do_segurar: Option<ReenvioDoSegurar>,
}

/// Um reenvio rápido do grupo do segurar: desde quando (a mudança, no relógio monotônico) e quantos
/// dos [`REENVIOS_DO_SEGURAR`] já passaram.
#[derive(Debug, Clone, Copy)]
struct ReenvioDoSegurar {
    desde_ms: u64,
    feitos: usize,
}

/// O que se sabe do outro lado **nesta sessão**.
#[derive(Debug, Default)]
struct VistaDoPar {
    visto_ms: Option<u64>,
    /// O relógio que o último estado do par mostrou (o maior entre o de Lamport e o de parede
    /// dele), quando estava no prazo. É o que diz se ele recusaria o nosso texto por carimbo do
    /// futuro.
    relogio: Option<u64>,
    /// As marcas `(carimbo, autor)` do último estado dele, na ordem de [`CAMPOS_CONFIRMAVEIS`].
    marcas: Option<Vec<(u64, String)>>,
    /// Desde quando há uma edição daqui sem confirmação.
    pendente_desde_ms: Option<u64>,
    /// O que a última bombeada anunciou, para o bit [`mudou::PAR`] só acender quando muda.
    anunciado_sumido: bool,
    anunciado_confirmado: bool,
    /// O último estado dele trouxe `"entende_segurar": true` (§12).
    entende_segurar: bool,
    /// O último estado dele trouxe `"entende_gravar": true` (§13).
    entende_gravar: bool,
}

/// Os campos cuja edição daqui espera confirmação. A posição fica de fora: é relato, não comando,
/// e o prompter a escreve a cada quadro.
const CAMPOS_CONFIRMAVEIS: usize = 10; // texto, rolando, velocidade, fonte, margem, linha, espelho, salto, para_tras, segurando

/// Depois de quanto silêncio o outro lado conta como sumido para a tela (o bit [`mudou::PAR`]).
/// A queda da **sessão** é outra conta, mais longa: `session::SILENCIO_DO_TELEPROMPTER`.
pub const PAR_SUMIDO: Duration = Duration::from_millis(2500);

/// **Uma réplica do estado do teleprompter.** Cada aparelho tem a sua; as duas convergem.
///
/// # Uma por aparelho, e ela sabe o seu papel
///
/// A réplica vive **mais que a sessão**: a casca a guarda enquanto o app está aberto, e ela
/// atravessa as quedas e as reconexões. O papel decide o que acontece com os campos que são da
/// sessão — `rolando`, `posicao` e `salto` — quando uma sessão nova começa:
///
/// - **o prompter os mantém.** Se o controle caiu com o texto rolando, o texto segue rolando (a
///   [`POLITICA_SEM_PAR`]), e o controle que volta vê isso — menos com o dedo no botão do
///   "segurar", que não atravessa sessão ([`POLITICA_SEM_PAR_AO_SEGURAR`]);
/// - **o controle os zera** (carimbo 0). Assim ele adota o que o prompter tem — e um prompter que
///   reiniciou começa parado e no começo, em vez de receber o `rolando=true` e o salto que o
///   controle guardava da sessão anterior.
///
/// Não é `Sync`: para usar de mais de uma thread (a tela edita, a sessão bombeia), use
/// [`Teleprompter`], que é esta mesma coisa atrás de um cadeado.
#[derive(Debug)]
pub struct Replica {
    autor: String,
    papel: Papel,
    relogio: u64,
    texto: Registro<String>,
    /// O [`resumo`] do texto atual, guardado para não recalcular a cada estado.
    resumo_do_texto: String,
    rolando: Registro<bool>,
    velocidade: Registro<f64>,
    fonte: Registro<f64>,
    margem: Registro<f64>,
    linha_de_leitura: Registro<f64>,
    espelho: Registro<bool>,
    posicao: Registro<f64>,
    salto: Registro<Option<f64>>,
    /// §12: rolar para trás. Da sessão, como `rolando`.
    para_tras: Registro<bool>,
    /// §12: o dedo no botão do "segurar para rolar". Da sessão.
    segurando: Registro<bool>,
    origem: Instant,
    /// A sessão da última bombeada (`Mensageiro::sessao`); `0` é nenhuma.
    sessao: u64,
    /// **O relógio na última troca de mensagem** (mandada ou recebida) da sessão corrente. Tudo
    /// com carimbo até aqui já existia na sessão; o que vier depois — uma edição feita na queda,
    /// ou logo depois de a sessão nova subir e antes da primeira bombeada — é da sessão seguinte.
    /// É o que [`Replica::nova_sessao`] usa para zerar só o que é da sessão velha.
    relogio_da_ultima_troca: u64,
    /// **O outro lado já mostrou ter o nosso último salto.** Enquanto não mostrou, "pular" parte
    /// do alvo do salto, e não do relato de posição — um relato gerado antes de o prompter aplicar
    /// o salto pode ter carimbo maior (o relógio dele adiantado) e ainda assim ser velho
    /// (defeito 3 da revisão). Só vale no controle.
    salto_reconhecido: bool,
    envio: Envio,
    par: VistaDoPar,
    contadores: ContadoresDoTeleprompter,
    // --- §11: a pergunta do texto e as cópias (só o controle usa; o prompter carrega) ---------
    /// O texto do controle, retido ou não.
    retencao: Retencao,
    /// Quem é o outro lado **desta** sessão (`Mensageiro::par`); `None` entre sessões, depois de
    /// [`Replica::perdeu_o_par`], e numa sessão do transporte montada à mão.
    par_da_sessao: Option<ParDaSessao>,
    /// O prompter da retenção de agora (e o das cópias que ela fizer): a pergunta continua na tela
    /// depois de uma queda, com as escolhas desligadas.
    par_da_retencao: Option<ParDaSessao>,
    /// **A maior referência de texto do prompter vista nesta sessão**, nos estados e nos textos
    /// dele: `(carimbo, autor, resumo)`. A escolha só vale contra ela (achado B2: a do último estado
    /// não serve, porque um estado atrasado chega depois de um mais novo).
    maior_ref_do_par: Option<(u64, String, String)>,
    /// O `device_id` do prompter com quem os textos convergiram pela última vez (§11.2).
    ultimo_prompter_id: Option<String>,
    /// A referência do texto em que convergiram. O nosso texto que mudou desde ela e perde uma
    /// fusão vai para as cópias (achado B6).
    referencia_convergida: Option<ReferenciaDoTexto>,
    /// As cópias, a mais nova primeiro. Ver [`COPIAS_DO_TEXTO`].
    copias: Vec<Copia>,
    /// Bits nascidos fora de uma chegada (uma sessão nova, uma cópia feita por uma chamada
    /// daqui), entregues na bombeada seguinte.
    mudancas_pendentes: Mudancas,
    /// **A trava da pergunta** (§11.10): a casca a liga quando tiver a tela da pergunta. Desligada
    /// (o padrão), o texto nunca é retido: vale "o último que mudou", e sobram as cópias da fusão.
    pergunta_ligada: bool,
    /// §12: **a tela deste prompter entende o "segurar"** — rola para trás e para quando `rolando`
    /// cai. Ligada pela casca; só então o estado diz `"entende_segurar": true`.
    segurar_ligado: bool,
    /// §12.4, só no controle: **o carimbo da última escrita do grupo pelo segurar daqui** — o
    /// apertar, o soltar, a parada do silêncio. É o que não passa para a sessão seguinte
    /// ([`Replica::esquecer_o_segurar`]). `0` é nenhuma.
    carimbo_do_segurar: u64,
    /// §12.3: a bombeada já disse que esta sessão acabou (ou um envio ouviu [`Error::Closed`]). O
    /// segurar é recusado daí até a sessão seguinte, com ou sem `perdeu_o_par` (achado 5 de 14/09).
    sessao_acabou: bool,
    // --- §13: a gravação --------------------------------------------------------------------
    /// A gravação: no prompter, a dele (o único escritor); no controle, a que o prompter relatou.
    /// Da sessão no controle, e nunca no salvo.
    gravacao: Gravacao,
    /// Só no prompter: **a tela dele grava** (a casca liga). Só então o estado diz
    /// `"entende_gravar": true` e um pedido pode ser aceito.
    gravacao_ligada: bool,
    /// No prompter: o pedido que chegou e a casca ainda não decidiu. No controle: o pedido daqui que
    /// o prompter ainda não respondeu — vai em todo estado até a resposta, e não passa da sessão.
    pedido_de_gravacao: Option<PedidoAberto>,
    /// Só no prompter: `(autor, n)` do último pedido aplicado. Um pedido com `n` até este, do mesmo
    /// controle, é velho (o canal é sem ordem) ou repetido, e é ignorado.
    ultimo_pedido: Option<(String, u64)>,
    /// Só no prompter: a resposta ao último pedido decidido, que vai em todo estado da sessão.
    resposta_de_gravacao: Option<RespostaNoFio>,
    /// A última recusa (no controle, a do pedido daqui; no prompter, a última que deu).
    gravacao_recusada: Option<RecusaDeGravacao>,
}

fn parede_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn conferir(valor: f64, faixa: &RangeInclusive<f64>, nome: &str) -> Result<()> {
    if !valor.is_finite() || !faixa.contains(&valor) {
        return Err(Error::Invalid(format!(
            "{nome} {valor} fora da faixa {}..={}",
            faixa.start(),
            faixa.end()
        )));
    }
    Ok(())
}

fn na_faixa(valor: f64, faixa: &RangeInclusive<f64>) -> bool {
    valor.is_finite() && faixa.contains(&valor)
}

fn registro_padrao<T>(valor: T) -> Registro<T> {
    Registro { valor, carimbo: 0, autor: String::new() }
}

/// Um campo da mensagem de estado, lido **sozinho**: `Ok(None)` se não veio, `Err(())` se veio e
/// não se lê. Ler campo a campo, e não a mensagem num bloco, é o que impede um campo ruim (um NaN
/// que o `serde_json` escreveu como `null`) de derrubar o estado inteiro — e, com ele, o batimento
/// e o reenvio do texto.
fn campo<T: DeserializeOwned>(msg: &Value, nome: &str) -> std::result::Result<Option<T>, ()> {
    match msg.get(nome) {
        None => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|_| ()),
    }
}

impl Replica {
    /// Uma réplica nova, com os valores padrão.
    ///
    /// `autor` é o `device_id` deste aparelho (1 a [`TETO_DO_AUTOR`] bytes): é ele que desempata
    /// duas edições no mesmo milissegundo. `papel` é [`Papel::Teleprompter`] (mostra o texto) ou
    /// [`Papel::ControleRemoto`].
    pub fn nova(autor: &str, papel: Papel) -> Result<Replica> {
        if autor.is_empty() || autor.len() > TETO_DO_AUTOR {
            return Err(Error::Invalid(format!(
                "o autor da réplica é o device_id deste aparelho, de 1 a {TETO_DO_AUTOR} bytes"
            )));
        }
        if !matches!(papel, Papel::Teleprompter | Papel::ControleRemoto) {
            return Err(Error::Invalid(
                "a réplica é do teleprompter ou do controle remoto".into(),
            ));
        }
        Ok(Replica {
            autor: autor.to_string(),
            papel,
            relogio: 0,
            texto: registro_padrao(String::new()),
            resumo_do_texto: resumo(""),
            rolando: registro_padrao(false),
            velocidade: registro_padrao(VELOCIDADE_PADRAO),
            fonte: registro_padrao(FONTE_PADRAO),
            margem: registro_padrao(MARGEM_PADRAO),
            linha_de_leitura: registro_padrao(LINHA_PADRAO),
            espelho: registro_padrao(false),
            posicao: registro_padrao(0.0),
            salto: registro_padrao(None),
            para_tras: registro_padrao(false),
            segurando: registro_padrao(false),
            origem: Instant::now(),
            sessao: 0,
            relogio_da_ultima_troca: 0,
            salto_reconhecido: true,
            envio: Envio::default(),
            par: VistaDoPar::default(),
            contadores: ContadoresDoTeleprompter::default(),
            retencao: Retencao::Livre,
            par_da_sessao: None,
            par_da_retencao: None,
            maior_ref_do_par: None,
            ultimo_prompter_id: None,
            referencia_convergida: None,
            copias: Vec::new(),
            mudancas_pendentes: 0,
            pergunta_ligada: false,
            segurar_ligado: false,
            carimbo_do_segurar: 0,
            sessao_acabou: false,
            gravacao: Gravacao::default(),
            gravacao_ligada: false,
            pedido_de_gravacao: None,
            ultimo_pedido: None,
            resposta_de_gravacao: None,
            gravacao_recusada: None,
        })
    }

    /// **Liga a pergunta do texto** (§11.10). Chame logo depois de criar a réplica, quando a casca
    /// tiver a tela da pergunta; ligada no meio de uma sessão, vale a partir da sessão seguinte. Sem
    /// isto, o texto nunca é retido — vale "o último que mudou", como antes — e sobram as cópias da
    /// fusão (§11.5). No prompter, não muda nada.
    pub fn ligar_pergunta_do_texto(&mut self) {
        self.pergunta_ligada = true;
    }

    /// **Diz que a tela deste prompter entende o "segurar para rolar"** (§12): rola para trás na
    /// velocidade de sempre com `para_tras`, e para quando `rolando` cai. O estado passa a levar
    /// `"entende_segurar": true`, e só então um controle segura. No controle, não muda nada.
    pub fn ligar_segurar(&mut self) {
        self.segurar_ligado = true;
        self.envio.estado_devido = true;
    }

    /// Uma réplica a partir do que [`Replica::salvo_json`] devolveu numa vida anterior.
    ///
    /// Os seis campos que persistem voltam **com os carimbos**; `rolando`, `posicao` e `salto`
    /// começam do padrão. Um campo salvo com valor fora da faixa volta ao padrão; um carimbo
    /// salvo mais de [`TOLERANCIA_DO_FUTURO`] à frente do relógio de agora (o relógio estava
    /// errado quando se salvou) é recarimbado agora, como edição local.
    ///
    /// JSON ilegível ou de outra versão: [`Error::Invalid`], e a casca recomeça do padrão.
    pub fn de_salvo(autor: &str, papel: Papel, json: &str) -> Result<Replica> {
        let mut r = Replica::nova(autor, papel)?;
        let agora = r.agora();
        r.carregar(json, agora)?;
        Ok(r)
    }

    fn carregar(&mut self, json: &str, agora: Agora) -> Result<()> {
        let s: Salvo = serde_json::from_str(json)
            .map_err(|e| Error::Invalid(format!("estado salvo do teleprompter ilegível: {e}")))?;
        if s.v != VERSAO {
            return Err(Error::Invalid(format!(
                "estado salvo do teleprompter na versão {}, esta build lê a {VERSAO}",
                s.v
            )));
        }
        // **O salvo passa pelas regras da edição e da chegada** (defeito 4 da revisão de 13/09):
        // autor até 256 bytes, texto que caiba numa mensagem, número na faixa. O que não passa
        // fica no padrão e é contado; o resto entra.
        //
        // Um carimbo mais de 24 h à frente do relógio de agora (o relógio estava errado quando
        // se salvou) é **recarimbado agora**, como edição local — o valor é do usuário e fica; o
        // carimbo do futuro, não. E o relógio salvo do futuro é **descartado** (não limitado a
        // "agora + 24 h", que faria a primeira edição sair um dia à frente e ser aceita e
        // persistida dos dois lados).
        let limite = agora.parede_ms.saturating_add(ms(TOLERANCIA_DO_FUTURO));
        let eu = self.autor.clone();
        let mut do_futuro = 0u64;
        let mut recusados = 0u64;
        let mut no_prazo = |reg_carimbo: &mut u64, reg_autor: &mut String| -> bool {
            if reg_autor.len() > TETO_DO_AUTOR {
                recusados += 1;
                return false;
            }
            if *reg_carimbo > limite {
                do_futuro += 1;
                *reg_carimbo = agora.parede_ms;
                *reg_autor = eu.clone();
            }
            true
        };
        let mut texto = s.texto;
        let mut texto_recusado = false;
        if no_prazo(&mut texto.carimbo, &mut texto.autor) {
            if texto_cabe(&texto.valor, &texto.autor).is_ok() {
                if self.texto.fundir(texto) {
                    self.texto_mudou();
                }
            } else {
                texto_recusado = texto.carimbo > 0;
            }
        }
        for (reg, mut novo, faixa) in [
            (&mut self.velocidade, s.velocidade, &FAIXA_DA_VELOCIDADE),
            (&mut self.fonte, s.fonte, &FAIXA_DA_FONTE),
            (&mut self.margem, s.margem, &FAIXA_DA_MARGEM),
            (&mut self.linha_de_leitura, s.linha_de_leitura, &FAIXA_DA_LINHA),
        ] {
            if no_prazo(&mut novo.carimbo, &mut novo.autor) && na_faixa(novo.valor, faixa) {
                reg.fundir(novo);
            }
        }
        let mut espelho = s.espelho;
        if no_prazo(&mut espelho.carimbo, &mut espelho.autor) {
            self.espelho.fundir(espelho);
        }
        // §11: o prompter da última vez, a referência em que convergiram e as cópias — cada um
        // conferido sozinho. Uma referência do futuro apaga os dois (o próximo encontro com
        // qualquer prompter pergunta, que é o lado seguro).
        let mut ultimo = match s.ultimo_prompter_id {
            None => None,
            Some(Value::String(id)) if !id.is_empty() && id.len() <= TETO_DO_AUTOR => Some(id),
            Some(_) => {
                recusados += 1;
                None
            }
        };
        let convergida = match s.referencia_convergida.map(serde_json::from_value::<ReferenciaDoTexto>) {
            None => None,
            Some(Ok(r)) if r.carimbo > limite => {
                do_futuro += 1;
                ultimo = None;
                None
            }
            Some(Ok(r)) if r.autor.len() <= TETO_DO_AUTOR => Some(r),
            Some(_) => {
                recusados += 1;
                None
            }
        };
        self.ultimo_prompter_id = ultimo;
        self.referencia_convergida = convergida;
        match s.copias_do_texto {
            None => {}
            Some(Value::Array(itens)) => {
                for item in itens {
                    let copia = serde_json::from_value::<CopiaSalva>(item).ok().filter(|c| {
                        !c.prompter_id.is_empty()
                            && c.prompter_id.len() <= TETO_DO_AUTOR
                            && !c.texto.is_empty()
                            && texto_cabe(&c.texto, &eu).is_ok()
                    });
                    let Some(c) = copia else {
                        recusados += 1;
                        continue;
                    };
                    let r = resumo(&c.texto);
                    if self.copias.len() >= COPIAS_DO_TEXTO || self.copias.iter().any(|x| x.resumo == r) {
                        recusados += 1;
                        continue;
                    }
                    self.copias.push(Copia {
                        origem: c.origem,
                        prompter_id: c.prompter_id,
                        prompter_nome: cortado(&c.prompter_nome),
                        quando_ms: c.quando_ms,
                        texto: c.texto,
                        resumo: r,
                    });
                }
            }
            Some(_) => recusados += 1,
        }
        let relogio_salvo = if s.relogio > limite {
            do_futuro += 1;
            0
        } else {
            s.relogio
        };
        self.relogio = relogio_salvo.max(self.maior_carimbo());
        self.contadores.carimbos_do_futuro += do_futuro;
        self.contadores.campos_recusados += recusados + u64::from(texto_recusado);
        Ok(())
    }

    /// O que guardar entre sessões: os seis campos que persistem, com os carimbos, e o relógio.
    /// Ver `docs/contrato-teleprompter.md` §3 — sem os carimbos, "vale o último que mudou"
    /// deixaria de valer ao reabrir o app. Até ~260 KiB (o texto domina).
    pub fn salvo_json(&self) -> String {
        let s = Salvo {
            v: VERSAO,
            relogio: self.relogio,
            texto: self.texto.clone(),
            velocidade: self.velocidade.clone(),
            fonte: self.fonte.clone(),
            margem: self.margem.clone(),
            linha_de_leitura: self.linha_de_leitura.clone(),
            espelho: self.espelho.clone(),
            ultimo_prompter_id: self.ultimo_prompter_id.clone().map(Value::String),
            referencia_convergida: self
                .referencia_convergida
                .as_ref()
                .and_then(|r| serde_json::to_value(r).ok()),
            copias_do_texto: (!self.copias.is_empty()).then(|| {
                Value::Array(
                    self.copias
                        .iter()
                        .filter_map(|c| {
                            serde_json::to_value(CopiaSalva {
                                origem: c.origem,
                                prompter_id: c.prompter_id.clone(),
                                prompter_nome: c.prompter_nome.clone(),
                                quando_ms: c.quando_ms,
                                texto: c.texto.clone(),
                            })
                            .ok()
                        })
                        .collect(),
                )
            }),
        };
        serde_json::to_string(&s).unwrap_or_default()
    }

    fn agora(&self) -> Agora {
        Agora { parede_ms: parede_ms(), mono_ms: ms(self.origem.elapsed()) }
    }

    fn maior_carimbo(&self) -> u64 {
        [
            self.texto.carimbo,
            self.rolando.carimbo,
            self.velocidade.carimbo,
            self.fonte.carimbo,
            self.margem.carimbo,
            self.linha_de_leitura.carimbo,
            self.espelho.carimbo,
            self.posicao.carimbo,
            self.salto.carimbo,
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
    }

    /// O papel desta réplica.
    pub fn papel(&self) -> Papel {
        self.papel
    }

    /// O carimbo de uma mudança local: `max(relogio + 1, agora_ms)`. Saturado: nunca volta a 0.
    fn carimbar(&mut self, agora: Agora) -> u64 {
        self.relogio = self.relogio.saturating_add(1).max(agora.parede_ms);
        self.relogio
    }

    fn meu<T>(&mut self, valor: T, agora: Agora) -> Registro<T> {
        let carimbo = self.carimbar(agora);
        Registro { valor, carimbo, autor: self.autor.clone() }
    }

    /// Um registro daqui com um carimbo **já tirado**: é como `rolando`, `para_tras` e `segurando`,
    /// escritos juntos, dividem um carimbo só (§12.3).
    fn meu_com<T>(&self, valor: T, carimbo: u64) -> Registro<T> {
        Registro { valor, carimbo, autor: self.autor.clone() }
    }

    // -----------------------------------------------------------------------------------------
    // Edições locais
    // -----------------------------------------------------------------------------------------

    /// Troca o roteiro. Acima de [`TETO_DO_TEXTO`] bytes, com NUL, ou se a mensagem pronta não
    /// couber em [`TETO_DA_MENSAGEM`]: [`Error::Invalid`], e o texto anterior fica. Chame **ao
    /// confirmar a edição**, nunca a cada tecla: cada chamada que muda o texto é um texto novo no
    /// fio.
    pub fn definir_texto(&mut self, texto: &str) -> Result<()> {
        let agora = self.agora();
        self.definir_texto_em(texto, agora)
    }

    /// O play e a pausa de sempre. **A pausa sempre para e sai do "segurar"** (§12.3); o play só
    /// faz alguma coisa com o texto parado — com o texto rolando pelo dedo no botão, ele não mexe no
    /// segurar, e soltar e a queda seguem parando.
    pub fn definir_rolando(&mut self, rolando: bool) -> Result<()> {
        let agora = self.agora();
        self.definir_rolando_em(rolando, agora)
    }

    /// Em linhas por segundo, [`FAIXA_DA_VELOCIDADE`], em centésimos.
    pub fn definir_velocidade(&mut self, linhas_por_segundo: f64) -> Result<()> {
        let agora = self.agora();
        self.definir_velocidade_em(linhas_por_segundo, agora)
    }

    /// Em pontos lógicos, [`FAIXA_DA_FONTE`], em décimos.
    pub fn definir_fonte(&mut self, pontos: f64) -> Result<()> {
        let agora = self.agora();
        self.definir_fonte_em(pontos, agora)
    }

    /// Fração da largura da vista do texto, de cada lado, [`FAIXA_DA_MARGEM`].
    pub fn definir_margem(&mut self, fracao: f64) -> Result<()> {
        let agora = self.agora();
        self.definir_margem_em(fracao, agora)
    }

    /// Fração da altura da vista do texto, a partir do topo, [`FAIXA_DA_LINHA`].
    pub fn definir_linha_de_leitura(&mut self, fracao: f64) -> Result<()> {
        let agora = self.agora();
        self.definir_linha_de_leitura_em(fracao, agora)
    }

    pub fn definir_espelho(&mut self, espelho: bool) -> Result<()> {
        let agora = self.agora();
        self.definir_espelho_em(espelho, agora)
    }

    /// O relato de **quem mostra o texto**: onde a linha de leitura está agora, como fração do
    /// percurso. Pode ser chamado a cada quadro; o envio é limitado a [`INTERVALO_DA_POSICAO`].
    /// Não é comando e não espera confirmação. **Só o prompter**: no controle é
    /// [`Error::Invalid`], porque dois escritores disputando a posição fariam o relato de um
    /// apagar o do outro.
    pub fn definir_posicao(&mut self, fracao: f64) -> Result<()> {
        let agora = self.agora();
        self.definir_posicao_em(fracao, agora)
    }

    /// Pede que quem mostra o texto vá para `fracao`. "Voltar ao começo" é `saltar(0.0)`. Sempre
    /// é um salto novo, mesmo para o mesmo lugar: é comando, não estado.
    pub fn saltar(&mut self, fracao: f64) -> Result<()> {
        let agora = self.agora();
        self.saltar_em(fracao, agora)
    }

    /// "Pular": um salto de `delta` a partir de onde o texto **vai estar** — o alvo do último
    /// salto daqui se ele ainda não apareceu no relato, senão a posição relatada. Dois toques
    /// rápidos em "pular" são dois pulos, e não um pulo calculado duas vezes do mesmo lugar.
    /// O alvo é limitado a 0..1.
    pub fn saltar_relativo(&mut self, delta: f64) -> Result<()> {
        let agora = self.agora();
        self.saltar_relativo_em(delta, agora)
    }

    pub(crate) fn definir_texto_em(&mut self, texto: &str, agora: Agora) -> Result<()> {
        texto_cabe(texto, &self.autor).map_err(Error::Invalid)?;
        if self.texto.valor == texto {
            return Ok(());
        }
        self.texto = self.meu(texto.to_string(), agora);
        self.texto_mudou();
        self.envio.texto_local_devido = true;
        self.envio.estado_devido = true;
        self.marcar_pendente(agora);
        // Com a pergunta aberta, o texto novo continua retido e a pergunta segue com o "meu" novo
        // — mas igual ao do prompter ela fecha, e vazio ela fecha pela regra de hoje (§11.4).
        let m = self.reavaliar_pergunta(agora);
        self.mudancas_pendentes |= m;
        Ok(())
    }

    pub(crate) fn definir_rolando_em(&mut self, rolando: bool, agora: Agora) -> Result<()> {
        // §12.3. O play e a pausa de sempre saem do modo "segurar" — o play normal é sempre para a
        // frente; sem isto, um `para_tras` que ficou de um segurar (adotado do prompter ao
        // reconectar) faria o play seguinte rolar para trás. Mas **o play que não muda `rolando` não
        // mexe no segurar** (achado 4 de 14/09): com o texto já rolando pelo dedo no botão, ele virava
        // play livre — soltar não fazia nada, e a queda o deixava rolando. A pausa, essa, sempre para.
        let muda = if rolando {
            !self.rolando.valor
        } else {
            self.rolando.valor || self.segurando.valor || self.para_tras.valor
        };
        if muda {
            let com_o_segurar = self.segurar_em_jogo();
            self.rolando_fora_do_segurar(rolando, com_o_segurar, agora);
        }
        Ok(())
    }

    /// `para_tras` e `segurando` entram no play e na pausa? Quando o segurar existe aqui: algum dos
    /// dois já foi escrito, ou esta é a tela de prompter que o liga. Fora disso o play e a pausa
    /// escrevem só `rolando`, e o `estado` de quem nunca usou o segurar é o de antes, byte a byte
    /// (§12.1).
    fn segurar_em_jogo(&self) -> bool {
        !self.para_tras.nunca_escrito()
            || !self.segurando.nunca_escrito()
            || (self.papel == Papel::Teleprompter && self.segurar_ligado)
    }

    /// Escreve `rolando` e tira do segurar (`para_tras` e `segurando` a `false`) com **um carimbo
    /// só para os três** (§12.3): o grupo ganha ou perde inteiro. Com carimbos seguidos, uma pausa
    /// no prompter no mesmo milissegundo de um aperto em "Rolar para cima" empatava só em
    /// `para_tras`, e o texto rolava para a frente com o dedo em "para trás" (achado 2 de 14/09).
    /// Devolve os bits do que mudou.
    fn rolando_fora_do_segurar(&mut self, rolando: bool, com_o_segurar: bool, agora: Agora) -> Mudancas {
        let mut m = 0;
        if self.rolando.valor != rolando {
            m |= mudou::ROLANDO;
        }
        if com_o_segurar && (self.segurando.valor || self.para_tras.valor) {
            m |= mudou::SEGURAR;
        }
        let carimbo = self.carimbar(agora);
        self.rolando = self.meu_com(rolando, carimbo);
        if com_o_segurar {
            self.segurando = self.meu_com(false, carimbo);
            self.para_tras = self.meu_com(false, carimbo);
        }
        self.envio.estado_devido = true;
        self.marcar_pendente(agora);
        m
    }

    /// **Aperta o botão do "segurar para rolar"** (§12): `rolando`, `para_tras` e `segurando` numa
    /// mensagem só, com um carimbo só. Enquanto segura, o prompter rola na velocidade de sempre, para
    /// a frente ou para trás; soltar ([`Replica::soltar`]) para. Se a sessão cair com o dedo no
    /// botão, o texto para ([`POLITICA_SEM_PAR_AO_SEGURAR`]).
    ///
    /// - no prompter: [`Error::Invalid`] (quem segura é o controle);
    /// - sem sessão, **ou com a sessão já acabada** — a bombeada disse `fechada`, ou um envio ouviu
    ///   [`Error::Closed`] —, mesmo antes de [`Replica::perdeu_o_par`]: [`Error::Closed`];
    /// - o prompter não disse que entende (`"entende_segurar"`): [`Error::Protocol`] — um prompter
    ///   de 13/09, ou uma tela que não liga o "segurar", ignoraria `para_tras` e rolaria para a
    ///   frente, e na queda seguiria rolando.
    pub fn segurar(&mut self, para_tras: bool) -> Result<()> {
        let agora = self.agora();
        self.segurar_em(para_tras, agora)
    }

    pub(crate) fn segurar_em(&mut self, para_tras: bool, agora: Agora) -> Result<()> {
        if self.papel != Papel::ControleRemoto {
            return Err(Error::Invalid("quem segura para rolar é o controle".into()));
        }
        // Achado 5 de 14/09: entre o fim da sessão e o `perdeu_o_par`, o aperto era aceito — e, numa
        // casca que não chamasse `perdeu_o_par`, ia para o prompter seguinte, mesmo um que não entende.
        if self.par_da_sessao.is_none() || self.sessao_acabou {
            return Err(Error::Closed);
        }
        if !self.par.entende_segurar {
            return Err(Error::Protocol(
                "o prompter não diz que entende \"segurar para rolar\" (rolar para trás e parar na \
                 queda): atualize o app do prompter"
                    .into(),
            ));
        }
        // §12.3: os três, sempre os três, com **o mesmo carimbo** — o grupo ganha ou perde inteiro
        // (achado 2 de 14/09; ver `rolando_fora_do_segurar`).
        let carimbo = self.carimbar(agora);
        self.rolando = self.meu_com(true, carimbo);
        self.para_tras = self.meu_com(para_tras, carimbo);
        self.segurando = self.meu_com(true, carimbo);
        self.carimbo_do_segurar = carimbo;
        self.envio.estado_devido = true;
        self.marcar_pendente(agora);
        self.agendar_o_reenvio_do_segurar(agora);
        Ok(())
    }

    /// **Solta o botão**: o texto para (`rolando`, `segurando` e `para_tras` de volta a `false`,
    /// numa mensagem só). Sem nada seguro, não faz nada — o segurar pode já ter parado sozinho
    /// (a queda, ou o silêncio do outro lado).
    pub fn soltar(&mut self) -> Result<()> {
        let agora = self.agora();
        self.soltar_em(agora)
    }

    pub(crate) fn soltar_em(&mut self, agora: Agora) -> Result<()> {
        if self.segurando.valor {
            self.parar_o_segurar(agora);
        }
        Ok(())
    }

    /// Para o texto do "segurar": `rolando`, `segurando` e `para_tras` a `false`, como edição daqui,
    /// com um carimbo só. No controle, fica marcada como escrita do segurar: não passa para a sessão
    /// seguinte ([`Replica::esquecer_o_segurar`]). Devolve os bits do que mudou.
    fn parar_o_segurar(&mut self, agora: Agora) -> Mudancas {
        let m = self.rolando_fora_do_segurar(false, true, agora);
        if self.papel == Papel::ControleRemoto {
            self.carimbo_do_segurar = self.rolando.carimbo;
        }
        self.agendar_o_reenvio_do_segurar(agora);
        m
    }

    /// §12.3: agenda o reenvio rápido do grupo do segurar ([`REENVIOS_DO_SEGURAR`]), contado de agora.
    fn agendar_o_reenvio_do_segurar(&mut self, agora: Agora) {
        self.envio.reenvio_do_segurar = Some(ReenvioDoSegurar { desde_ms: agora.mono_ms, feitos: 0 });
    }

    /// O outro lado já mostrou ter o nosso grupo do segurar, ou um mais novo? O grupo tem o carimbo
    /// de `rolando`, e `rolando` sempre está nas marcas dele — até nas de um prompter de 13/09.
    fn grupo_confirmado(&self) -> bool {
        self.par.marcas.as_ref().and_then(|d| d.get(1)).is_some_and(|(c, a)| {
            (*c, a.as_bytes()) >= (self.rolando.carimbo, self.rolando.autor.as_bytes())
        })
    }

    /// Quando sai o próximo reenvio do grupo do segurar (no relógio monotônico), se ainda falta algum
    /// e o outro lado não confirmou.
    fn proximo_reenvio_do_segurar_ms(&self) -> Option<u64> {
        let r = self.envio.reenvio_do_segurar?;
        if self.grupo_confirmado() {
            return None;
        }
        REENVIOS_DO_SEGURAR.get(r.feitos).map(|d| r.desde_ms.saturating_add(ms(*d)))
    }

    /// A espera da bombeada, encurtada até o próximo reenvio do segurar: ele sai na hora marcada, e
    /// não na bombeada seguinte a ela. Um reenvio já vencido não encurta nada — a bombeada acabou de
    /// tentar mandá-lo, e esperar zero a faria girar sem parar enquanto o canal não abre.
    fn espera_da_bombeada(&self, agora: Agora, limite: Duration) -> Duration {
        match self.proximo_reenvio_do_segurar_ms() {
            Some(t) if t > agora.mono_ms => limite.min(Duration::from_millis(t - agora.mono_ms)),
            _ => limite,
        }
    }

    /// **O segurar não passa da sessão** (§12.4). Volta ao padrão — carimbo 0, que nunca vence e
    /// não vai ao fio — o grupo ainda seguro e, no controle, o que o segurar daqui escreveu (o
    /// apertar, o soltar, a parada do silêncio). Assim nada disso chega à sessão seguinte, e o
    /// controle adota o que o prompter tem. Um play ou uma pausa ficam: seguem a regra dos outros
    /// campos da sessão. No prompter (que nunca escreve pelo segurar), é só o grupo ainda seguro: a
    /// sessão nova que começa com ele ([`Replica::nova_sessao_com_par`]).
    ///
    /// É a direção do coordenador para o achado 1 de 14/09: a parada da queda, carimbada, viajava —
    /// parava na volta um prompter que rolava por um play, ou desfazia o play dado no prompter depois
    /// da queda (com o relógio daqui adiantado). O prompter para sozinho: o `perdeu_o_par` dele, ou o
    /// silêncio de [`PAR_SUMIDO`].
    fn esquecer_o_segurar(&mut self) -> Mudancas {
        let c = self.carimbo_do_segurar;
        let do_segurar = |r: &Registro<bool>| c > 0 && r.carimbo == c && r.autor == self.autor;
        let esquecer = self.segurando.valor
            || do_segurar(&self.rolando)
            || do_segurar(&self.segurando)
            || do_segurar(&self.para_tras);
        self.carimbo_do_segurar = 0;
        if !esquecer {
            return 0;
        }
        let mut m = 0;
        if self.rolando.valor {
            m |= mudou::ROLANDO;
        }
        if self.segurando.valor || self.para_tras.valor {
            m |= mudou::SEGURAR;
        }
        self.rolando = registro_padrao(false);
        self.segurando = registro_padrao(false);
        self.para_tras = registro_padrao(false);
        m
    }

    /// **O silêncio com o dedo no botão** (§12): segurando, e o outro lado sem mandar nada há
    /// [`PAR_SUMIDO`], o texto para — nos dois lados, sem esperar a sessão cair (5 s). A bombeada
    /// chama isto a cada volta.
    pub(crate) fn vigiar_o_segurar_em(&mut self, agora: Agora) -> Mudancas {
        if !self.segurando.valor || POLITICA_SEM_PAR_AO_SEGURAR != PoliticaSemPar::Pausa {
            return 0;
        }
        let sumido = self
            .par
            .visto_ms
            .is_none_or(|v| agora.mono_ms.saturating_sub(v) >= ms(PAR_SUMIDO));
        if sumido {
            self.parar_o_segurar(agora)
        } else {
            0
        }
    }

    // -----------------------------------------------------------------------------------------
    // §13: a gravação
    // -----------------------------------------------------------------------------------------

    /// **Diz se a tela deste prompter grava** (§13): a tela do teleprompter com câmera liga ao abrir
    /// e desliga ao fechar. Ligada, o estado leva `"entende_gravar": true`, e só então um controle
    /// pede; desligada, um pedido que ainda chegue é recusado pelo núcleo com
    /// [`MOTIVO_SEM_A_TELA`] — **e também o que já estava aberto**, que a tela que fecha não vai mais
    /// decidir (revisão de 24/09, M1: sem isto, o controle ficava "esperando" até a sessão acabar). Não
    /// mexe numa gravação em curso. No controle, não muda nada.
    pub fn ligar_gravacao(&mut self, ligada: bool) {
        if self.papel == Papel::Teleprompter && self.gravacao_ligada != ligada {
            self.gravacao_ligada = ligada;
            self.envio.estado_devido = true;
            if !ligada {
                self.responder_o_pedido(Some(MOTIVO_SEM_A_TELA.to_string()));
            }
        }
    }

    /// **O relato da gravação, por quem grava** (§13): `true` quando o arquivo começou, `false`
    /// quando fechou — pelo botão da tela, por um pedido do controle, por falta de espaço, pela
    /// câmera perdida. **Só o prompter** ([`Error::Invalid`] no controle): ele é o único escritor, e
    /// o controle pede com [`Replica::pedir_gravar`] e [`Replica::pedir_parar`].
    ///
    /// Repetir o valor atual não recarimba nem recomeça a contagem. Com um pedido aberto que pede
    /// **este** valor, responde a ele como aceito — então aceitar um pedido é chamar isto com o
    /// `gravar` dele, mesmo que já esteja assim.
    pub fn definir_gravando(&mut self, gravando: bool) -> Result<()> {
        let agora = self.agora();
        self.definir_gravando_em(gravando, agora)
    }

    pub(crate) fn definir_gravando_em(&mut self, gravando: bool, agora: Agora) -> Result<()> {
        if self.papel != Papel::Teleprompter {
            return Err(Error::Invalid(
                "só o prompter relata a gravação; o controle pede com pedir_gravar e pedir_parar".into(),
            ));
        }
        if self.gravacao.inicio_ms.is_some() != gravando {
            let carimbo = self.carimbar(agora);
            self.gravacao = Gravacao {
                carimbo,
                autor: self.autor.clone(),
                inicio_ms: gravando.then(|| i64::try_from(agora.mono_ms).unwrap_or(i64::MAX)),
            };
            self.envio.estado_devido = true;
        }
        if self.pedido_de_gravacao.as_ref().is_some_and(|p| p.gravar == gravando) {
            self.responder_o_pedido(None);
        }
        Ok(())
    }

    /// **Recusa o pedido aberto `n`** (§13), com um motivo legível que o controle mostra como está (1
    /// a [`TETO_DO_MOTIVO`] bytes, sem NUL). Só o prompter; sem pedido aberto, [`Error::Invalid`].
    ///
    /// **O `n` é o do pedido que a casca decidiu** (o `"n"` de `"pedido_de_gravacao"` que ela leu). Se
    /// outro pedido o substituiu enquanto ela decidia, [`Error::Ocupado`], e nada é recusado: a casca
    /// relê e decide o novo (revisão de 24/09, M2 — sem o `n`, a falha da câmera ao gravar recusava o
    /// "parar" que tinha chegado no meio).
    pub fn recusar_gravacao(&mut self, n: u64, motivo: &str) -> Result<()> {
        if self.papel != Papel::Teleprompter {
            return Err(Error::Invalid("quem recusa um pedido de gravação é o prompter".into()));
        }
        if motivo.is_empty() || motivo.len() > TETO_DO_MOTIVO || motivo.contains('\0') {
            return Err(Error::Invalid(format!(
                "o motivo da recusa tem de 1 a {TETO_DO_MOTIVO} bytes, sem NUL"
            )));
        }
        match &self.pedido_de_gravacao {
            None => return Err(Error::Invalid("não há pedido de gravação aberto".into())),
            Some(p) if p.n != n => {
                return Err(Error::Ocupado(format!(
                    "o pedido {n} foi substituído pelo {}: releia o pedido e decida de novo",
                    p.n
                )))
            }
            Some(_) => {}
        }
        self.responder_o_pedido(Some(motivo.to_string()));
        Ok(())
    }

    /// Fecha o pedido aberto do prompter com a resposta — aceito (`None`) ou recusado — que vai no
    /// estado seguinte e em todos os da sessão.
    fn responder_o_pedido(&mut self, recusa: Option<String>) {
        let Some(p) = self.pedido_de_gravacao.take() else { return };
        if let Some(motivo) = &recusa {
            self.gravacao_recusada = Some(RecusaDeGravacao { n: p.n, gravar: p.gravar, motivo: motivo.clone() });
        }
        self.resposta_de_gravacao = Some(RespostaNoFio { n: p.n, autor: p.autor, gravacao_recusada: recusa });
        self.envio.estado_devido = true;
    }

    /// **O controle pede ao prompter que comece a gravar** (§13). Quem decide é o prompter: a
    /// resposta volta pelo estado — `"pedido_de_gravacao"` volta a nulo, e `"gravando_ha_ms"` passa a
    /// contar ou `"gravacao_recusada"` diz por quê. Sai na hora, numa mensagem.
    ///
    /// - no prompter: [`Error::Invalid`];
    /// - sem sessão, ou com ela já acabada (como o segurar): [`Error::Closed`] — um pedido não
    ///   atravessa sessão;
    /// - o prompter não diz que grava (`"par_entende_gravar": false`: uma build anterior, ou uma tela
    ///   que não grava): [`Error::Protocol`].
    pub fn pedir_gravar(&mut self) -> Result<()> {
        let agora = self.agora();
        self.pedir_em(true, agora)
    }

    /// **O controle pede ao prompter que pare de gravar** (§13). As mesmas regras de
    /// [`Replica::pedir_gravar`].
    pub fn pedir_parar(&mut self) -> Result<()> {
        let agora = self.agora();
        self.pedir_em(false, agora)
    }

    pub(crate) fn pedir_em(&mut self, gravar: bool, agora: Agora) -> Result<()> {
        if self.papel != Papel::ControleRemoto {
            return Err(Error::Invalid("quem pede a gravação é o controle; o prompter usa definir_gravando".into()));
        }
        if self.par_da_sessao.is_none() || self.sessao_acabou {
            return Err(Error::Closed);
        }
        if !self.par.entende_gravar {
            return Err(Error::Protocol(
                "o prompter não diz que grava (uma build anterior, ou a tela dele não é a que grava)".into(),
            ));
        }
        // O número: um carimbo do relógio daqui. Cresce a cada pedido, e também entre vidas do app
        // (o relógio vai no salvo e tem piso de parede) — um `n` que recomeçasse em 1 seria ignorado
        // pelo prompter que guardou o último.
        let n = self.carimbar(agora);
        self.pedido_de_gravacao = Some(PedidoAberto { n, gravar, autor: self.autor.clone(), desde_ms: agora.mono_ms });
        self.gravacao_recusada = None;
        self.envio.estado_devido = true;
        Ok(())
    }

    /// O prompter recebe um pedido. Devolve [`mudou::GRAVACAO`] quando a casca tem de decidir (ou
    /// quando o núcleo recusou por ela).
    fn chegou_pedido_de_gravacao(&mut self, p: PedidoNoFio, agora: Agora) -> Mudancas {
        // O `n` é um carimbo do controle: o teto do futuro vale para ele como para todo carimbo
        // (revisão de 24/09, m2). Sem isto, um `n` de um relógio dias à frente ficava guardado como o
        // último, e todo pedido do mesmo controle, depois de acertado o relógio, era "velho".
        if !self.no_prazo(p.n, agora) {
            return 0;
        }
        // **O pedido velho que chega atrasado é ignorado**, e o repetido também: o canal é confiável
        // mas sem ordem, e o controle manda o pedido em todo estado até a resposta.
        if self.ultimo_pedido.as_ref().is_some_and(|(a, n)| *a == p.autor && p.n <= *n) {
            return 0;
        }
        self.ultimo_pedido = Some((p.autor.clone(), p.n));
        self.pedido_de_gravacao = Some(PedidoAberto { n: p.n, gravar: p.gravar, autor: p.autor, desde_ms: agora.mono_ms });
        self.envio.estado_devido = true;
        if !self.gravacao_ligada {
            self.responder_o_pedido(Some(MOTIVO_SEM_A_TELA.to_string()));
            return mudou::GRAVACAO;
        }
        // **Sempre a casca decide**, mesmo quando o núcleo acha que já está como se pede (revisão de
        // 24/09, m1): o núcleo só sabe o que a casca já relatou, e o gravador pode estar no meio de
        // abrir o arquivo pelo botão da tela. Aceitar é `definir_gravando(gravar)`, que não muda nada
        // quando já está assim. Um pedido aberto é substituído pelo novo, e a casca decide de novo.
        mudou::GRAVACAO
    }

    /// O controle recebe a gravação do prompter e a resposta ao pedido daqui.
    fn chegou_gravacao_do_prompter(&mut self, msg: &Value, agora: Agora) -> Mudancas {
        let mut m = 0;
        match campo::<GravandoNoFio>(msg, "gravando_ha_ms") {
            Ok(None) => {}
            Err(()) => self.contadores.campos_recusados += 1,
            Ok(Some(g)) if g.valor.is_some_and(|v| v > ms(TETO_DA_GRAVACAO)) => {
                self.contadores.campos_recusados += 1;
            }
            Ok(Some(g)) if g.carimbo > 0 && self.aceitavel(g.carimbo, &g.autor, agora) => {
                self.avancar_relogio(g.carimbo);
                let chegada = i64::try_from(agora.mono_ms).unwrap_or(i64::MAX);
                let inicio = g.valor.map(|v| chegada.saturating_sub(i64::try_from(v).unwrap_or(i64::MAX)));
                match (g.carimbo, g.autor.as_bytes()).cmp(&(self.gravacao.carimbo, self.gravacao.autor.as_bytes())) {
                    Ordem::Greater => {
                        self.gravacao = Gravacao { carimbo: g.carimbo, autor: g.autor, inicio_ms: inicio };
                        m |= mudou::GRAVACAO;
                    }
                    // A mesma gravação: fica o começo mais cedo, que é o da mensagem que menos
                    // demorou. Um estado atrasado (duração menor, chegada tardia) não faz a contagem
                    // andar para trás.
                    Ordem::Equal => {
                        if let (Some(a), Some(b)) = (self.gravacao.inicio_ms, inicio) {
                            self.gravacao.inicio_ms = Some(a.min(b));
                        }
                    }
                    Ordem::Less => {}
                }
            }
            Ok(Some(_)) => {}
        }
        match campo::<RespostaNoFio>(msg, "resposta_de_gravacao") {
            Ok(None) => {}
            Err(()) => self.contadores.campos_recusados += 1,
            Ok(Some(r)) => {
                let e_a_minha = r.autor == self.autor
                    && self.pedido_de_gravacao.as_ref().is_some_and(|p| p.n == r.n);
                if e_a_minha {
                    let p = self.pedido_de_gravacao.take().expect("conferido acima");
                    self.gravacao_recusada = r.gravacao_recusada.map(|motivo| RecusaDeGravacao {
                        n: p.n,
                        gravar: p.gravar,
                        motivo: cortado_em(&motivo, TETO_DO_MOTIVO),
                    });
                    m |= mudou::GRAVACAO;
                }
            }
        }
        m
    }

    /// Esquece o que da gravação é da sessão, ao começar outra (§13.4). O controle esquece o pedido
    /// dele e a recusa, e zera a gravação que vinha da sessão anterior (adota a do prompter no
    /// primeiro estado); o prompter esquece só a resposta — a gravação é dele, e o pedido aberto
    /// continua para a casca decidir.
    fn gravacao_na_sessao_nova(&mut self, velho: u64) -> Mudancas {
        let mut m = 0;
        if self.papel == Papel::ControleRemoto {
            if self.pedido_de_gravacao.take().is_some() | self.gravacao_recusada.take().is_some() {
                m |= mudou::GRAVACAO;
            }
            if self.gravacao.carimbo <= velho && self.gravacao.carimbo > 0 {
                if self.gravacao.inicio_ms.is_some() {
                    m |= mudou::GRAVACAO;
                }
                self.gravacao = Gravacao::default();
            }
        } else {
            // Um pedido não atravessa sessão: o controle esquece o dele, e o prompter esquece o
            // número do último — nenhuma mensagem da sessão velha chega na nova.
            self.resposta_de_gravacao = None;
            self.gravacao_recusada = None;
            self.ultimo_pedido = None;
        }
        m
    }

    /// O miolo das edições de número: confere, quantiza, e só carimba se o valor mudou.
    fn definir_numero(
        &mut self,
        qual: fn(&mut Replica) -> &mut Registro<f64>,
        valor: f64,
        faixa: &RangeInclusive<f64>,
        por_unidade: f64,
        nome: &str,
        agora: Agora,
    ) -> Result<()> {
        conferir(valor, faixa, nome)?;
        let valor = quantizar(valor, por_unidade).clamp(*faixa.start(), *faixa.end());
        if qual(self).valor == valor {
            return Ok(());
        }
        let novo = self.meu(valor, agora);
        *qual(self) = novo;
        self.envio.estado_devido = true;
        self.marcar_pendente(agora);
        Ok(())
    }

    pub(crate) fn definir_velocidade_em(&mut self, v: f64, agora: Agora) -> Result<()> {
        self.definir_numero(|r| &mut r.velocidade, v, &FAIXA_DA_VELOCIDADE, PASSO_DA_VELOCIDADE, "velocidade", agora)
    }

    pub(crate) fn definir_fonte_em(&mut self, v: f64, agora: Agora) -> Result<()> {
        self.definir_numero(|r| &mut r.fonte, v, &FAIXA_DA_FONTE, PASSO_DA_FONTE, "fonte", agora)
    }

    pub(crate) fn definir_margem_em(&mut self, v: f64, agora: Agora) -> Result<()> {
        self.definir_numero(|r| &mut r.margem, v, &FAIXA_DA_MARGEM, PASSO_DA_FRACAO, "margem", agora)
    }

    pub(crate) fn definir_linha_de_leitura_em(&mut self, v: f64, agora: Agora) -> Result<()> {
        self.definir_numero(
            |r| &mut r.linha_de_leitura,
            v,
            &FAIXA_DA_LINHA,
            PASSO_DA_FRACAO,
            "linha de leitura",
            agora,
        )
    }

    pub(crate) fn definir_espelho_em(&mut self, espelho: bool, agora: Agora) -> Result<()> {
        if self.espelho.valor != espelho {
            self.espelho = self.meu(espelho, agora);
            self.envio.estado_devido = true;
            self.marcar_pendente(agora);
        }
        Ok(())
    }

    pub(crate) fn definir_posicao_em(&mut self, v: f64, agora: Agora) -> Result<()> {
        if self.papel != Papel::Teleprompter {
            return Err(Error::Invalid(
                "só quem mostra o texto relata a posição; o controle pede com saltar".into(),
            ));
        }
        conferir(v, &FAIXA_DA_POSICAO, "posição")?;
        let v = quantizar(v, PASSO_DA_FRACAO);
        if self.posicao.valor != v {
            self.posicao = self.meu(v, agora);
            self.envio.posicao_devida = true;
        }
        Ok(())
    }

    pub(crate) fn saltar_em(&mut self, v: f64, agora: Agora) -> Result<()> {
        conferir(v, &FAIXA_DA_POSICAO, "salto")?;
        let v = quantizar(v, PASSO_DA_FRACAO).clamp(0.0, 1.0);
        self.salto = self.meu(Some(v), agora);
        match self.papel {
            // Quem mostra o texto vai para o alvo: a posição relatada é ele já agora.
            Papel::Teleprompter => self.posicao = self.meu(v, agora),
            // No controle, até o prompter mostrar que tem este salto, "pular" parte dele.
            _ => self.salto_reconhecido = false,
        }
        self.envio.estado_devido = true;
        self.marcar_pendente(agora);
        Ok(())
    }

    pub(crate) fn saltar_relativo_em(&mut self, delta: f64, agora: Agora) -> Result<()> {
        if !delta.is_finite() || delta.abs() > 1.0 {
            return Err(Error::Invalid(format!("pulo de {delta}: fora de -1..=1")));
        }
        // Onde o texto vai estar: o alvo do último salto, enquanto o prompter não mostrar que o
        // tem; depois disso, o relato dele — que a partir dali é posterior à aplicação do salto.
        // A primeira versão comparava o carimbo do relato com o do salto, e um relato velho com
        // o relógio adiantado virava base (defeito 3 da revisão).
        let base = match self.salto.valor {
            Some(alvo) if !self.salto_reconhecido => alvo,
            _ => self.posicao.valor,
        };
        self.saltar_em((base + delta).clamp(0.0, 1.0), agora)
    }

    /// **O outro lado sumiu**: a sessão caiu. Aplica [`POLITICA_SEM_PAR`] — o prompter continua no
    /// estado em que estava — **com a exceção do "segurar"** ([`POLITICA_SEM_PAR_AO_SEGURAR`]): com
    /// o dedo no botão, o texto para nos dois lados. Esquece o que sabia do par. Devolve o que mudou
    /// (para a tela), sempre com [`mudou::PAR`].
    ///
    /// Chame em **todo** fim de sessão, antes de qualquer outra edição (§6, a ordem do fim).
    pub fn perdeu_o_par(&mut self) -> Mudancas {
        let agora = self.agora();
        self.perdeu_o_par_em(agora)
    }

    pub(crate) fn perdeu_o_par_em(&mut self, agora: Agora) -> Mudancas {
        let mut m = mudou::PAR;
        // §12.4, decidido pelo usuário em 14/09: **com o dedo no botão, a queda para o texto** — nos
        // dois lados, como se a pessoa tivesse soltado. É a exceção à política de sempre, e vem antes
        // dela.
        if POLITICA_SEM_PAR_AO_SEGURAR == PoliticaSemPar::Pausa {
            match self.papel {
                // O prompter para o texto dele, carimbado: é a tela que rola, e na volta o controle
                // adota o estado dele, como sempre.
                Papel::Teleprompter if self.segurando.valor => m |= self.parar_o_segurar(agora),
                // O controle para **só na réplica dele**, e a parada não viaja (achado 1 de 14/09):
                // o prompter para sozinho.
                Papel::ControleRemoto => m |= self.esquecer_o_segurar(),
                _ => {}
            }
        }
        if self.papel == Papel::Teleprompter
            && POLITICA_SEM_PAR == PoliticaSemPar::Pausa
            && self.rolando.valor
        {
            let _ = self.definir_rolando_em(false, agora);
            m |= mudou::ROLANDO;
        }
        // §13: **a queda não mexe na gravação** — o prompter segue gravando, e o controle segue
        // mostrando a última que viu, com o aviso de par sumido. O pedido do controle sem resposta
        // não passa da sessão: sai já, para a tela não esperar uma resposta que não vem.
        if self.papel == Papel::ControleRemoto && self.pedido_de_gravacao.take().is_some() {
            m |= mudou::GRAVACAO;
        }
        if self.par.entende_gravar {
            m |= mudou::GRAVACAO;
        }
        self.par = VistaDoPar { anunciado_sumido: true, ..VistaDoPar::default() };
        // Sem sessão, não há a quem reenviar o grupo do segurar (§12.3).
        self.envio.reenvio_do_segurar = None;
        // §11.4: sem sessão, as escolhas ficam desligadas (`par_da_sessao` nulo). A pergunta aberta
        // continua na tela; o "comparando", sem nada em mãos, volta a livre. A sessão seguinte
        // decide de novo, do zero.
        self.par_da_sessao = None;
        if matches!(self.retencao, Retencao::Comparando { .. }) {
            self.retencao = Retencao::Livre;
            m |= mudou::PERGUNTA_DO_TEXTO;
        }
        m | std::mem::take(&mut self.mudancas_pendentes)
    }

    // -----------------------------------------------------------------------------------------
    // Leitura
    // -----------------------------------------------------------------------------------------

    /// O texto do roteiro.
    pub fn texto(&self) -> &str {
        &self.texto.valor
    }

    /// O estado para a tela desenhar.
    pub fn estado(&self) -> Estado {
        let agora = self.agora();
        self.estado_em(agora)
    }

    pub(crate) fn estado_em(&self, agora: Agora) -> Estado {
        Estado {
            rolando: self.rolando.valor,
            velocidade: self.velocidade.valor,
            fonte: self.fonte.valor,
            margem: self.margem.valor,
            linha_de_leitura: self.linha_de_leitura.valor,
            espelho: self.espelho.valor,
            posicao: self.posicao.valor,
            salto: self.salto.valor,
            texto_bytes: self.texto.valor.len(),
            par_visto_ha_ms: self.par.visto_ms.map(|v| agora.mono_ms.saturating_sub(v)),
            sem_confirmacao_ha_ms: self
                .par
                .pendente_desde_ms
                .map(|v| agora.mono_ms.saturating_sub(v)),
            contadores: self.contadores,
            para_tras: self.para_tras.valor,
            segurando: self.segurando.valor,
            par_entende_segurar: self.par.entende_segurar,
            gravando_ha_ms: self.gravacao.ha_ms(agora),
            pedido_de_gravacao: self.pedido_de_gravacao.as_ref().map(|p| PedidoDeGravacao {
                n: p.n,
                gravar: p.gravar,
                ha_ms: agora.mono_ms.saturating_sub(p.desde_ms),
            }),
            gravacao_recusada: self.gravacao_recusada.clone(),
            par_entende_gravar: self.par.entende_gravar,
            pergunta_do_texto: self.pergunta_em(agora),
            copias_do_texto: self
                .copias
                .iter()
                .map(|c| CopiaDoTexto {
                    origem: c.origem,
                    prompter_id: c.prompter_id.clone(),
                    prompter_nome: c.prompter_nome.clone(),
                    quando_ms: c.quando_ms,
                    bytes: c.texto.len(),
                    resumo: c.resumo.clone(),
                    previa: previa(&c.texto),
                })
                .collect(),
        }
    }

    /// A pergunta do texto, para a tela. `None` com o texto livre.
    fn pergunta_em(&self, agora: Agora) -> Option<PerguntaDoTexto> {
        let (desde_ms, do_prompter) = match &self.retencao {
            Retencao::Livre => return None,
            Retencao::Comparando { desde_ms } => (*desde_ms, None),
            Retencao::Perguntando { desde_ms, deles, resumo_deles } => (
                *desde_ms,
                Some(VistaDoTexto {
                    bytes: deles.valor.len(),
                    resumo: resumo_deles.clone(),
                    previa: previa(&deles.valor),
                }),
            ),
        };
        let par = self.par_da_retencao.as_ref();
        Some(PerguntaDoTexto {
            aberta: do_prompter.is_some(),
            retido_ha_ms: agora.mono_ms.saturating_sub(desde_ms),
            prompter_id: par.map(|p| p.id.clone()).unwrap_or_default(),
            prompter_nome: par.map(|p| p.nome.clone()).unwrap_or_default(),
            meu: VistaDoTexto {
                bytes: self.texto.valor.len(),
                resumo: self.resumo_do_texto.clone(),
                previa: previa(&self.texto.valor),
            },
            do_prompter,
        })
    }

    /// O texto do prompter na pergunta aberta; `None` sem pergunta aberta (inclusive comparando).
    /// O deste controle é o de sempre, [`Replica::texto`]: retido, a réplica mostra o dele.
    pub fn texto_da_pergunta(&self) -> Option<&str> {
        match &self.retencao {
            Retencao::Perguntando { deles, .. } => Some(&deles.valor),
            _ => None,
        }
    }

    /// O texto inteiro de uma cópia, pelo `resumo` que `copias_do_texto` mostra.
    pub fn copia_do_texto(&self, resumo: &str) -> Option<&str> {
        self.copias.iter().find(|c| c.resumo == resumo).map(|c| c.texto.as_str())
    }

    /// Apaga uma cópia, pelo resumo. Devolve se havia. Acende [`mudou::COPIA_DO_TEXTO`] na bombeada
    /// seguinte, para a casca gravar o salvo.
    pub fn esquecer_copia_do_texto(&mut self, resumo: &str) -> bool {
        let antes = self.copias.len();
        self.copias.retain(|c| c.resumo != resumo);
        let apagou = self.copias.len() != antes;
        if apagou {
            self.mudancas_pendentes |= mudou::COPIA_DO_TEXTO;
        }
        apagou
    }

    /// O relógio de Lamport desta réplica. Só para diagnóstico e para os testes.
    pub fn relogio(&self) -> u64 {
        self.relogio
    }

    // -----------------------------------------------------------------------------------------
    // Confirmação
    // -----------------------------------------------------------------------------------------

    /// As marcas dos campos confirmáveis, na ordem de [`CAMPOS_CONFIRMAVEIS`].
    fn marcas_confirmaveis(&self) -> [(u64, &str); CAMPOS_CONFIRMAVEIS] {
        [
            // Retido, o texto sai da conta: ele não está saindo, e o aviso "o comando não chegou"
            // acenderia com a pergunta aberta (§11.3).
            if self.retido() { (0, "") } else { self.texto.marca() },
            self.rolando.marca(),
            self.velocidade.marca(),
            self.fonte.marca(),
            self.margem.marca(),
            self.linha_de_leitura.marca(),
            self.espelho.marca(),
            self.salto.marca(),
            // §12.1 (achado 3 de 14/09): os do segurar só contam com um prompter que diz que os
            // entende. Um de 13/09 nunca os devolve, e "o comando não chegou" ficava aceso a sessão
            // inteira. Com o prompter que entende, eles vêm no mesmo carimbo de `rolando`.
            if self.par.entende_segurar { self.para_tras.marca() } else { (0, "") },
            if self.par.entende_segurar { self.segurando.marca() } else { (0, "") },
        ]
    }

    /// Existe edição **daqui** que o outro lado ainda não mostrou ter visto?
    fn ha_pendencia(&self) -> bool {
        let minhas = self.marcas_confirmaveis();
        let Some(dele) = self.par.marcas.as_ref() else {
            // Nada dele nesta sessão: pendente se há qualquer edição daqui.
            return minhas.iter().any(|(c, a)| *c > 0 && *a == self.autor);
        };
        minhas.iter().zip(dele.iter()).any(|((c, a), (dc, da))| {
            *c > 0 && *a == self.autor && (*dc, da.as_bytes()) < (*c, a.as_bytes())
        })
    }

    fn marcar_pendente(&mut self, agora: Agora) {
        if self.par.pendente_desde_ms.is_none() {
            self.par.pendente_desde_ms = Some(agora.mono_ms);
        }
    }

    /// Recalcula a pendência e o sumiço, e devolve [`mudou::PAR`] se o que a tela mostra mudou.
    fn olhar_o_par(&mut self, agora: Agora) -> Mudancas {
        if !self.ha_pendencia() {
            self.par.pendente_desde_ms = None;
        } else if self.par.pendente_desde_ms.is_none() {
            self.par.pendente_desde_ms = Some(agora.mono_ms);
        }
        let confirmado = self.par.pendente_desde_ms.is_none();
        let sumido = match self.par.visto_ms {
            None => true,
            Some(v) => agora.mono_ms.saturating_sub(v) >= ms(PAR_SUMIDO),
        };
        let mut m = 0;
        if confirmado != self.par.anunciado_confirmado || sumido != self.par.anunciado_sumido {
            m |= mudou::PAR;
        }
        self.par.anunciado_confirmado = confirmado;
        self.par.anunciado_sumido = sumido;
        m
    }

    // -----------------------------------------------------------------------------------------
    // Recepção
    // -----------------------------------------------------------------------------------------

    /// Funde uma mensagem que chegou do outro lado. Devolve o que mudou por causa dela.
    ///
    /// Nunca falha: o que não é mensagem do teleprompter, ou é de outra versão, é contado e
    /// ignorado; um campo que não se lê, fora da faixa ou com carimbo do futuro é recusado e o
    /// resto da mensagem entra.
    pub fn receber(&mut self, mensagem: &str) -> Mudancas {
        let agora = self.agora();
        self.receber_em(mensagem, agora)
    }

    pub(crate) fn receber_em(&mut self, mensagem: &str, agora: Agora) -> Mudancas {
        // Qualquer coisa que chega é prova de vida do outro lado.
        self.par.visto_ms = Some(agora.mono_ms);
        let valor: Value = match serde_json::from_str(mensagem) {
            Ok(v) => v,
            Err(_) => {
                self.contadores.invalidas += 1;
                return self.olhar_o_par(agora);
            }
        };
        if valor.get("app").and_then(Value::as_str) != Some(APP) {
            self.contadores.de_outro_app += 1;
            return self.olhar_o_par(agora);
        }
        if valor.get("v").and_then(Value::as_u64) != Some(VERSAO) {
            self.contadores.de_outra_versao += 1;
            return self.olhar_o_par(agora);
        }
        let m = match valor.get("tipo").and_then(Value::as_str) {
            Some("estado") => {
                self.contadores.recebidas += 1;
                self.fundir_estado(&valor, agora)
            }
            Some("texto") => {
                self.contadores.recebidas += 1;
                self.fundir_texto(&valor, agora)
            }
            _ => {
                self.contadores.invalidas += 1;
                0
            }
        };
        // **A confirmação sai já**, e não no próximo batimento: uma mudança do outro lado que
        // entrou aqui é respondida com o nosso estado na bombeada seguinte — é o que deixa o
        // controle saber em uma ida e volta que o comando chegou. Não vira pingue-pongue: a
        // resposta leva os mesmos carimbos e não muda nada do lado de lá. A posição fica de fora:
        // é relato de 4 Hz, não comando.
        if m & !(mudou::POSICAO | mudou::PAR) != 0 {
            self.envio.estado_devido = true;
        }
        self.relogio_da_ultima_troca = self.relogio;
        m | self.olhar_o_par(agora)
    }

    /// O carimbo cabe no prazo? Mais de [`TOLERANCIA_DO_FUTURO`] à frente é recusado e contado.
    fn no_prazo(&mut self, carimbo: u64, agora: Agora) -> bool {
        if carimbo > agora.parede_ms.saturating_add(ms(TOLERANCIA_DO_FUTURO)) {
            self.contadores.carimbos_do_futuro += 1;
            false
        } else {
            true
        }
    }

    /// Um registrador que chegou pode entrar? Carimbo no prazo **e** autor até
    /// [`TETO_DO_AUTOR`] bytes (defeito 4b: o autor não tinha teto na chegada). O que não pode é
    /// contado.
    fn aceitavel(&mut self, carimbo: u64, autor: &str, agora: Agora) -> bool {
        if autor.len() > TETO_DO_AUTOR {
            self.contadores.campos_recusados += 1;
            return false;
        }
        self.no_prazo(carimbo, agora)
    }

    /// Lê e funde um registrador numérico do estado.
    fn fundir_numero(
        &mut self,
        msg: &Value,
        nome: &str,
        qual: fn(&mut Replica) -> &mut Registro<f64>,
        faixa: &RangeInclusive<f64>,
        bit: Mudancas,
        agora: Agora,
    ) -> Mudancas {
        match campo::<Registro<f64>>(msg, nome) {
            Ok(None) => 0,
            Err(()) => {
                self.contadores.campos_recusados += 1;
                0
            }
            Ok(Some(novo)) => {
                if !self.aceitavel(novo.carimbo, &novo.autor, agora) {
                    return 0;
                }
                if !na_faixa(novo.valor, faixa) {
                    if novo.carimbo > 0 {
                        self.contadores.campos_recusados += 1;
                    }
                    return 0;
                }
                self.avancar_relogio(novo.carimbo);
                if qual(self).fundir(novo) {
                    bit
                } else {
                    0
                }
            }
        }
    }

    fn fundir_booleano(
        &mut self,
        msg: &Value,
        nome: &str,
        qual: fn(&mut Replica) -> &mut Registro<bool>,
        bit: Mudancas,
        agora: Agora,
    ) -> Mudancas {
        match campo::<Registro<bool>>(msg, nome) {
            Ok(None) => 0,
            Err(()) => {
                self.contadores.campos_recusados += 1;
                0
            }
            Ok(Some(novo)) => {
                if !self.aceitavel(novo.carimbo, &novo.autor, agora) {
                    return 0;
                }
                self.avancar_relogio(novo.carimbo);
                if qual(self).fundir(novo) {
                    bit
                } else {
                    0
                }
            }
        }
    }

    fn avancar_relogio(&mut self, visto: u64) {
        self.relogio = self.relogio.max(visto);
    }

    fn fundir_estado(&mut self, msg: &Value, agora: Agora) -> Mudancas {
        if let Some(r) = msg.get("relogio").and_then(Value::as_u64) {
            if self.no_prazo(r, agora) {
                self.avancar_relogio(r);
                self.par.relogio = Some(r);
            }
        }
        let referencia = match campo::<ReferenciaDoTexto>(msg, "texto") {
            Ok(r) => r,
            Err(()) => {
                self.contadores.campos_recusados += 1;
                None
            }
        };

        // As marcas dele, para a confirmação — antes de fundir, do jeito que vieram. Campo que
        // não veio ou não se lê conta como "ele não tem".
        let marca = |nome: &str| -> (u64, String) {
            msg.get(nome)
                .map(|v| {
                    (
                        v.get("carimbo").and_then(Value::as_u64).unwrap_or(0),
                        v.get("autor").and_then(Value::as_str).unwrap_or("").to_string(),
                    )
                })
                .unwrap_or((0, String::new()))
        };
        self.par.marcas = Some(vec![
            marca("texto"),
            marca("rolando"),
            marca("velocidade"),
            marca("fonte"),
            marca("margem"),
            marca("linha_de_leitura"),
            marca("espelho"),
            marca("salto"),
            marca("para_tras"),
            marca("segurando"),
        ]);
        // §12: a tela do outro lado entende o "segurar"? Só o último estado diz.
        self.par.entende_segurar = msg.get("entende_segurar").and_then(Value::as_bool) == Some(true);

        let mut m = 0;
        // §13: a gravação. Cada lado lê só o que o outro papel escreve: o prompter, o pedido; o
        // controle, a gravação, a resposta e se o prompter grava. **O que chega ao prompter como
        // `"gravando_ha_ms"` é ignorado** — ele é o único escritor, como da `posicao`.
        let entende_gravar = msg.get("entende_gravar").and_then(Value::as_bool) == Some(true);
        if entende_gravar != self.par.entende_gravar {
            self.par.entende_gravar = entende_gravar;
            m |= mudou::GRAVACAO;
        }
        match self.papel {
            Papel::Teleprompter => match campo::<PedidoNoFio>(msg, "pedido_de_gravacao") {
                Ok(None) => {}
                Ok(Some(p)) if p.n > 0 && !p.autor.is_empty() && p.autor.len() <= TETO_DO_AUTOR => {
                    m |= self.chegou_pedido_de_gravacao(p, agora);
                }
                _ => self.contadores.campos_recusados += 1,
            },
            _ => m |= self.chegou_gravacao_do_prompter(msg, agora),
        }

        m |= self.fundir_booleano(msg, "rolando", |r| &mut r.rolando, mudou::ROLANDO, agora);
        m |= self.fundir_booleano(msg, "espelho", |r| &mut r.espelho, mudou::ESPELHO, agora);
        m |= self.fundir_booleano(msg, "para_tras", |r| &mut r.para_tras, mudou::SEGURAR, agora);
        m |= self.fundir_booleano(msg, "segurando", |r| &mut r.segurando, mudou::SEGURAR, agora);
        m |= self.fundir_numero(msg, "velocidade", |r| &mut r.velocidade, &FAIXA_DA_VELOCIDADE, mudou::VELOCIDADE, agora);
        m |= self.fundir_numero(msg, "fonte", |r| &mut r.fonte, &FAIXA_DA_FONTE, mudou::FONTE, agora);
        m |= self.fundir_numero(msg, "margem", |r| &mut r.margem, &FAIXA_DA_MARGEM, mudou::MARGEM, agora);
        m |= self.fundir_numero(
            msg,
            "linha_de_leitura",
            |r| &mut r.linha_de_leitura,
            &FAIXA_DA_LINHA,
            mudou::LINHA_DE_LEITURA,
            agora,
        );
        m |= self.fundir_numero(msg, "posicao", |r| &mut r.posicao, &FAIXA_DA_POSICAO, mudou::POSICAO, agora);
        match campo::<Registro<Option<f64>>>(msg, "salto") {
            Ok(None) => {}
            Err(()) => self.contadores.campos_recusados += 1,
            Ok(Some(novo)) => {
                if self.aceitavel(novo.carimbo, &novo.autor, agora) {
                    match novo.valor {
                        Some(v) if !na_faixa(v, &FAIXA_DA_POSICAO) => {
                            self.contadores.campos_recusados += 1
                        }
                        _ => {
                            self.avancar_relogio(novo.carimbo);
                            // O outro lado já tem o nosso salto (o dele é igual ou mais novo): a
                            // posição que veio nesta mesma mensagem é posterior à aplicação dele
                            // — o prompter a acerta no alvo ao fundir o salto, antes de mandar
                            // qualquer estado. Daqui em diante "pular" parte do relato.
                            if (novo.carimbo, novo.autor.as_bytes())
                                >= (self.salto.carimbo, self.salto.autor.as_bytes())
                            {
                                self.salto_reconhecido = true;
                            }
                            if self.salto.fundir(novo) {
                                m |= mudou::SALTO;
                                // Quem mostra o texto vai para o alvo: a posição relatada passa a
                                // ser ele já agora, e o estado de resposta a leva junto com o
                                // salto (defeito 3 da revisão).
                                if self.papel == Papel::Teleprompter {
                                    if let Some(alvo) = self.salto.valor {
                                        self.posicao = self.meu(alvo, agora);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // **Anti-entropia do texto.** A referência dele menor que o nosso texto, na mesma ordem
        // da fusão (carimbo, autor, resumo), quer dizer que ele não tem o nosso: agenda o reenvio
        // (com limite). Maior, é ele que vai mandar quando vir o nosso estado.
        if let Some(r) = referencia {
            if self.aceitavel(r.carimbo, &r.autor, agora) {
                self.avancar_relogio(r.carimbo);
                self.viu_texto_do_par(r.carimbo, &r.autor, &r.resumo);
                let dele = (r.carimbo, r.autor.as_bytes(), r.resumo.as_str());
                let nosso = (
                    self.texto.carimbo,
                    self.texto.autor.as_bytes(),
                    self.resumo_do_texto.as_str(),
                );
                // **Marca e desmarca.** O estado mais novo do par é quem diz o que ele tem: um
                // pedido de reenvio feito por um estado que chegou antes do texto não pode
                // sobreviver ao estado seguinte, que já mostra o texto — senão o roteiro inteiro
                // sai de novo 2 s depois, à toa (medido pela `quall-probe` em 13/09/2026: 100 KB
                // mandados duas vezes numa sessão sã).
                self.envio.texto_pedido_pelo_par = self.texto.carimbo > 0 && dele < nosso;
                // Ele pede, mas recusaria (o nosso texto está no futuro dele), ou pediu de novo
                // depois dos reenvios todos (defeito 5): `proxima_devida` não reenvia, e conta uma
                // vez. **Com o texto retido, não conta** (achado B9): ele não está saindo, e não há
                // reenvio de que desistir.
                if self.envio.texto_pedido_pelo_par
                    && !self.retido()
                    && (self.par_recusaria_o_texto()
                        || self.envio.reenvios_do_texto >= REENVIOS_DO_MESMO_TEXTO)
                    && !self.envio.desistiu_do_texto
                {
                    self.envio.desistiu_do_texto = true;
                    self.contadores.reenvios_desistidos += 1;
                }
                if self.papel == Papel::ControleRemoto {
                    m |= self.olhar_a_referencia_do_prompter(&r);
                }
            }
        }
        m
    }

    /// O controle viu a referência do texto do prompter (§11.2): um prompter que **nunca
    /// escreveu** solta o "comparando" (ele não vai mandar texto nenhum); e, livre, a referência
    /// igual à do texto daqui é a **convergência** — o prompter desta sessão passa a ser o da
    /// última vez, e a referência é a convergida.
    fn olhar_a_referencia_do_prompter(&mut self, r: &ReferenciaDoTexto) -> Mudancas {
        let mut m = 0;
        if matches!(self.retencao, Retencao::Comparando { .. }) && r.carimbo == 0 {
            self.retencao = Retencao::Livre;
            m |= mudou::PERGUNTA_DO_TEXTO;
        }
        if matches!(self.retencao, Retencao::Livre)
            && r.carimbo == self.texto.carimbo
            && r.autor == self.texto.autor
            && r.resumo == self.resumo_do_texto
        {
            if let Some(p) = &self.par_da_sessao {
                self.ultimo_prompter_id = Some(p.id.clone());
                self.referencia_convergida = Some(r.clone());
            }
        }
        m
    }

    /// Guarda a maior referência de texto do prompter vista nesta sessão (achado B2).
    fn viu_texto_do_par(&mut self, carimbo: u64, autor: &str, resumo: &str) {
        let visto = (carimbo, autor.to_string(), resumo.to_string());
        if self.maior_ref_do_par.as_ref().is_none_or(|maior| visto > *maior) {
            self.maior_ref_do_par = Some(visto);
        }
    }

    /// O texto está retido (§11.3)?
    fn retido(&self) -> bool {
        !matches!(self.retencao, Retencao::Livre)
    }

    /// A referência do texto que o estado anuncia: a do texto daqui; comparando, a de quem nunca
    /// escreveu (para o prompter mandar o dele); perguntando, a do texto do prompter guardado (para
    /// ele parar de reenviar). Ver §11.3.
    fn referencia_anunciada(&self) -> ReferenciaDoTexto {
        match &self.retencao {
            Retencao::Livre => ReferenciaDoTexto {
                carimbo: self.texto.carimbo,
                autor: self.texto.autor.clone(),
                bytes: self.texto.valor.len() as u64,
                resumo: self.resumo_do_texto.clone(),
            },
            Retencao::Comparando { .. } => {
                ReferenciaDoTexto { carimbo: 0, autor: String::new(), bytes: 0, resumo: resumo("") }
            }
            Retencao::Perguntando { deles, resumo_deles, .. } => ReferenciaDoTexto {
                carimbo: deles.carimbo,
                autor: deles.autor.clone(),
                bytes: deles.valor.len() as u64,
                resumo: resumo_deles.clone(),
            },
        }
    }

    /// O prompter das cópias: o desta sessão, ou o da retenção (a pergunta que ficou de uma sessão
    /// caída). `None` numa sessão sem par conhecido — e aí não há cópia, como antes.
    fn prompter_das_copias(&self) -> Option<ParDaSessao> {
        self.par_da_sessao.clone().or_else(|| self.par_da_retencao.clone())
    }

    /// O texto daqui é o da última convergência?
    fn texto_e_o_convergido(&self) -> bool {
        self.referencia_convergida.as_ref().is_some_and(|r| {
            r.carimbo == self.texto.carimbo
                && r.autor == self.texto.autor
                && r.resumo == self.resumo_do_texto
        })
    }

    /// Guarda uma cópia (§11.5): a mais nova primeiro, sem repetição pelo resumo, no máximo
    /// [`COPIAS_DO_TEXTO`]. Acende [`mudou::COPIA_DO_TEXTO`] na bombeada seguinte.
    fn guardar_copia(&mut self, texto: String, origem: OrigemDaCopia, prompter: &ParDaSessao, agora: Agora) {
        if texto.is_empty() {
            return;
        }
        let resumo_da_copia = resumo(&texto);
        self.copias.retain(|c| c.resumo != resumo_da_copia);
        self.copias.insert(
            0,
            Copia {
                origem,
                prompter_id: prompter.id.clone(),
                prompter_nome: cortado(&prompter.nome),
                quando_ms: agora.parede_ms,
                texto,
                resumo: resumo_da_copia,
            },
        );
        self.copias.truncate(COPIAS_DO_TEXTO);
        self.mudancas_pendentes |= mudou::COPIA_DO_TEXTO;
    }

    /// **Adota o registro do prompter** como veio (valor, carimbo e autor) e solta o texto. Os
    /// pedidos de envio do texto daqui são desmarcados na hora: senão a bombeada seguinte
    /// devolveria ao prompter o próprio texto dele (§11.3).
    fn adotar(&mut self, deles: Registro<String>) {
        self.texto = deles;
        self.texto_mudou();
        self.envio.texto_local_devido = false;
        self.envio.texto_pedido_pelo_par = false;
        self.envio.estado_devido = true;
        self.retencao = Retencao::Livre;
    }

    /// **A fusão do texto no controle, livre** — "vale o último que mudou" — com a regra das
    /// cópias (§11.5): o nosso texto que perde, se mudou desde a última convergência (ou se ainda
    /// não houve convergência com este prompter); e, antes da primeira convergência, o do prompter
    /// que perde. Um texto vazio, ou igual ao vencedor, não vira cópia.
    fn fundir_livre(&mut self, novo: Registro<String>, agora: Agora) -> Mudancas {
        let prompter = self.prompter_das_copias();
        if self.texto.perde_para(&novo) {
            if let Some(p) = &prompter {
                if !self.texto.valor.is_empty()
                    && self.texto.valor != novo.valor
                    && !self.texto_e_o_convergido()
                {
                    let nosso = self.texto.valor.clone();
                    self.guardar_copia(nosso, OrigemDaCopia::Controle, p, agora);
                }
            }
            self.texto = novo;
            self.texto_mudou();
            mudou::TEXTO
        } else {
            if let Some(p) = &prompter {
                if self.referencia_convergida.is_none()
                    && !novo.valor.is_empty()
                    && novo.valor != self.texto.valor
                {
                    self.guardar_copia(novo.valor, OrigemDaCopia::Prompter, p, agora);
                }
            }
            0
        }
    }

    /// O texto do prompter chegou a este controle.
    fn chegou_texto_no_controle(&mut self, novo: Registro<String>, agora: Agora) -> Mudancas {
        match std::mem::replace(&mut self.retencao, Retencao::Livre) {
            Retencao::Livre => self.fundir_livre(novo, agora),
            Retencao::Comparando { desde_ms } => self.decidir_no_primeiro_encontro(novo, desde_ms, agora),
            Retencao::Perguntando { desde_ms, deles, resumo_deles } => {
                if deles.perde_para(&novo) {
                    let resumo_deles = resumo(&novo.valor);
                    self.retencao = Retencao::Perguntando { desde_ms, deles: novo, resumo_deles };
                    mudou::PERGUNTA_DO_TEXTO | self.reavaliar_pergunta(agora)
                } else {
                    // Um texto mais velho do prompter (desordem, duplicata): não muda a pergunta.
                    self.retencao = Retencao::Perguntando { desde_ms, deles, resumo_deles };
                    0
                }
            }
        }
    }

    /// **O primeiro texto do prompter num primeiro encontro** (a tabela da §11.2).
    fn decidir_no_primeiro_encontro(
        &mut self,
        deles: Registro<String>,
        desde_ms: u64,
        agora: Agora,
    ) -> Mudancas {
        if deles.valor == self.texto.valor {
            // Iguais: adota o registro dele. Nada volta pelo fio, e a tela dele não refaz o layout
            // de um texto igual.
            self.adotar(deles);
            mudou::PERGUNTA_DO_TEXTO
        } else if self.texto.valor.is_empty() {
            // O nosso vazio não substitui o roteiro dele.
            self.adotar(deles);
            mudou::TEXTO | mudou::PERGUNTA_DO_TEXTO
        } else if deles.valor.is_empty() {
            // O vazio dele não substitui o nosso: vai o nosso, recarimbado se o vazio venceria.
            if self.texto.perde_para(&deles) {
                let valor = std::mem::take(&mut self.texto.valor);
                self.texto = self.meu(valor, agora);
                self.texto_mudou();
            }
            self.envio.texto_local_devido = true;
            self.envio.estado_devido = true;
            self.retencao = Retencao::Livre;
            mudou::PERGUNTA_DO_TEXTO
        } else {
            let resumo_deles = resumo(&deles.valor);
            self.retencao = Retencao::Perguntando { desde_ms, deles, resumo_deles };
            mudou::PERGUNTA_DO_TEXTO
        }
    }

    /// Com a pergunta aberta, um dos dois textos mudou: iguais, fecha e adota o dele; um dos dois
    /// vazio, fecha **pela regra de hoje** — vale o último que mudou — e o outro vai para as cópias
    /// (achado B10). Devolve os bits.
    fn reavaliar_pergunta(&mut self, agora: Agora) -> Mudancas {
        let fecha = match &self.retencao {
            Retencao::Perguntando { deles, .. } => {
                deles.valor == self.texto.valor
                    || deles.valor.is_empty()
                    || self.texto.valor.is_empty()
            }
            _ => false,
        };
        if !fecha {
            return 0;
        }
        let Retencao::Perguntando { deles, .. } = std::mem::replace(&mut self.retencao, Retencao::Livre)
        else {
            return 0;
        };
        if deles.valor == self.texto.valor {
            self.adotar(deles);
            return mudou::PERGUNTA_DO_TEXTO;
        }
        mudou::PERGUNTA_DO_TEXTO | self.fundir_livre(deles, agora)
    }

    /// **A escolha** (§11.4): "usar o do prompter" (`manter_o_meu = false`) adota o registro dele
    /// e guarda o daqui; "mandar o meu" recarimba o daqui, acima do dele, e guarda o dele. Nas duas
    /// o texto solta, e o primeiro encontro segue até a convergência.
    ///
    /// `resumo_visto` é o `"resumo"` de `"do_prompter"` que a tela mostrou.
    ///
    /// - sem pergunta aberta (ou ainda comparando): [`Error::Invalid`];
    /// - sem o prompter conectado: [`Error::Closed`] — as escolhas ficam desligadas;
    /// - o texto do prompter na pergunta não é o que a pessoa viu, ou **a maior referência de texto
    ///   do prompter vista nesta sessão** é maior que a da pergunta (ele tem um texto mais novo a
    ///   caminho): [`Error::Ocupado`]. A pergunta se atualiza em seguida, com o bit.
    pub fn resolver_texto(&mut self, manter_o_meu: bool, resumo_visto: &str) -> Result<()> {
        let agora = self.agora();
        self.resolver_texto_em(manter_o_meu, resumo_visto, agora)
    }

    pub(crate) fn resolver_texto_em(
        &mut self,
        manter_o_meu: bool,
        resumo_visto: &str,
        agora: Agora,
    ) -> Result<()> {
        let Retencao::Perguntando { deles, resumo_deles, .. } = &self.retencao else {
            return Err(Error::Invalid("não há pergunta do texto aberta".into()));
        };
        let Some(prompter) = self.par_da_sessao.clone() else {
            return Err(Error::Closed);
        };
        if resumo_deles != resumo_visto {
            return Err(Error::Ocupado(
                "o texto do prompter mudou desde que a pergunta foi mostrada".into(),
            ));
        }
        let da_pergunta = (deles.carimbo, deles.autor.clone(), resumo_deles.clone());
        if self.maior_ref_do_par.as_ref().is_some_and(|maior| *maior > da_pergunta) {
            return Err(Error::Ocupado("o prompter tem um texto mais novo a caminho".into()));
        }
        let Retencao::Perguntando { deles, .. } = std::mem::replace(&mut self.retencao, Retencao::Livre)
        else {
            return Err(Error::Invalid("não há pergunta do texto aberta".into()));
        };
        if manter_o_meu {
            self.guardar_copia(deles.valor, OrigemDaCopia::Prompter, &prompter, agora);
            let valor = std::mem::take(&mut self.texto.valor);
            self.texto = self.meu(valor, agora);
            self.texto_mudou();
            self.envio.texto_local_devido = true;
            self.envio.estado_devido = true;
            self.marcar_pendente(agora);
        } else {
            let nosso = self.texto.valor.clone();
            self.guardar_copia(nosso, OrigemDaCopia::Controle, &prompter, agora);
            self.adotar(deles);
        }
        Ok(())
    }

    fn fundir_texto(&mut self, msg: &Value, agora: Agora) -> Mudancas {
        if let Some(r) = msg.get("relogio").and_then(Value::as_u64) {
            if self.no_prazo(r, agora) {
                self.avancar_relogio(r);
            }
        }
        let novo = match campo::<Registro<String>>(msg, "texto") {
            Ok(Some(t)) => t,
            Ok(None) | Err(()) => {
                self.contadores.campos_recusados += 1;
                return 0;
            }
        };
        if !self.aceitavel(novo.carimbo, &novo.autor, agora) {
            return 0;
        }
        // A mesma regra da edição: um texto que daqui não daria para reenviar não entra.
        if texto_cabe(&novo.valor, &novo.autor).is_err() {
            self.contadores.campos_recusados += 1;
            return 0;
        }
        self.avancar_relogio(novo.carimbo);
        if self.papel == Papel::ControleRemoto {
            self.viu_texto_do_par(novo.carimbo, &novo.autor, &resumo(&novo.valor));
            return self.chegou_texto_no_controle(novo, agora);
        }
        let de_outro_autor = novo.autor != self.autor;
        if self.texto.fundir(novo) {
            self.texto_mudou();
            // **Achado B5**: o texto deste aparelho mudou por uma mensagem de outro autor enquanto
            // ele era prompter. No Android os dois papéis dividem um salvo; ao voltar a ser
            // controle, ele não pode empurrar esse roteiro em silêncio para o prompter da última
            // vez. Apagado, o próximo encontro é um primeiro encontro, que retém e pergunta.
            if de_outro_autor {
                self.ultimo_prompter_id = None;
                self.referencia_convergida = None;
            }
            mudou::TEXTO
        } else {
            0
        }
    }

    // -----------------------------------------------------------------------------------------
    // Envio
    // -----------------------------------------------------------------------------------------

    /// Começou uma sessão nova: o par não viu nada ainda. Os relógios de envio zeram (o estado sai
    /// já), e o que se sabia do outro lado é esquecido. O que estava para sair continua para sair.
    ///
    /// **No controle, os campos da sessão que vêm da sessão anterior voltam ao padrão (carimbo
    /// 0)**: ele adota o que o prompter tem. Ver a documentação de [`Replica`].
    ///
    /// **Só os da sessão anterior** — carimbo até [`Replica::relogio_da_ultima_troca`]. A primeira
    /// versão zerava os três sempre, e apagava uma edição feita **para esta sessão**: com a máquina
    /// carregada, o salto do controle saiu antes da primeira bombeada e a primeira bombeada o
    /// apagou (medido na suíte da fronteira, 13/09/2026; teste
    /// `a_edicao_feita_antes_da_primeira_bombeada_da_sessao_sobrevive`).
    #[cfg(test)]
    pub(crate) fn nova_sessao(&mut self, sessao: u64) {
        let agora = self.agora();
        self.nova_sessao_com_par(sessao, None, agora);
    }

    /// [`Replica::nova_sessao`], sabendo quem é o outro lado (`Mensageiro::par`). É a entrada que
    /// os testes usam para injetar o par sem uma sessão de verdade (achado B11).
    ///
    /// **No controle, §11.2**: um par conhecido que não é o prompter da última vez abre um
    /// **primeiro encontro** — o texto fica retido em "comparando", e `ultimo_prompter_id` e a
    /// referência convergida são apagados (achado B8). Toda sessão recomeça do zero: uma pergunta
    /// que ficou de uma sessão caída é descartada, e o texto do prompter guardado com ela também
    /// (achado B1 — a pergunta vale por sessão). O mesmo prompter da última vez, ou um par
    /// desconhecido, deixa o texto livre.
    ///
    /// **O segurar não atravessa sessão** (§12.4): o controle esquece o que o segurar dele escreveu;
    /// o prompter que chega aqui ainda segurando — a casca não chamou `perdeu_o_par` — sai do
    /// segurar, só na réplica dele, antes do primeiro estado da sessão nova.
    pub(crate) fn nova_sessao_com_par(&mut self, sessao: u64, par: Option<&ParDaSessao>, agora: Agora) {
        self.maior_ref_do_par = None;
        self.par_da_sessao = par.cloned();
        if self.papel == Papel::ControleRemoto {
            let estava_retido = self.retido();
            self.retencao = Retencao::Livre;
            self.par_da_retencao = None;
            if let Some(p) = par.filter(|p| self.ultimo_prompter_id.as_deref() != Some(p.id.as_str())) {
                // Um primeiro encontro. O da última vez sai já, **com ou sem a trava**: é o que faz a
                // fusão guardar o perdedor até a convergência (§11.5), que sobra mesmo sem pergunta.
                self.ultimo_prompter_id = None;
                self.referencia_convergida = None;
                // Só com a trava ligada (§11.10) o texto fica retido e a pergunta existe.
                if self.pergunta_ligada {
                    self.retencao = Retencao::Comparando { desde_ms: agora.mono_ms };
                    self.par_da_retencao = Some(p.clone());
                }
            }
            if estava_retido || self.retido() {
                self.mudancas_pendentes |= mudou::PERGUNTA_DO_TEXTO;
            }
        }
        self.sessao = sessao;
        self.sessao_acabou = false;
        self.envio.ultimo_estado_ms = None;
        self.envio.ultimo_texto_ms = None;
        self.envio.reenvio_do_segurar = None;
        self.envio.reenvios_do_texto = 0;
        self.envio.desistiu_do_texto = false;
        self.par = VistaDoPar::default();
        // §13.4: o que da gravação é da sessão.
        self.mudancas_pendentes |= self.gravacao_na_sessao_nova(self.relogio_da_ultima_troca);
        if self.papel == Papel::ControleRemoto {
            // §12.4: o segurar daqui não passa da sessão — nem o que não saiu (um soltar ou a parada
            // do silêncio que o canal já fechado recusou), nem numa casca que não chamou
            // `perdeu_o_par`. Não depende de `velho`: um aperto só existe depois de o prompter desta
            // sessão dizer que entende, então tudo o que o segurar escreveu é de uma sessão anterior.
            self.mudancas_pendentes |= self.esquecer_o_segurar();
            let velho = self.relogio_da_ultima_troca;
            if self.rolando.carimbo <= velho {
                self.rolando = registro_padrao(false);
            }
            if self.posicao.carimbo <= velho {
                self.posicao = registro_padrao(0.0);
            }
            if self.salto.carimbo <= velho {
                self.salto = registro_padrao(None);
                self.salto_reconhecido = true;
            }
            // §12: o sentido e o dedo no botão também são da sessão.
            if self.para_tras.carimbo <= velho {
                self.para_tras = registro_padrao(false);
            }
            if self.segurando.carimbo <= velho {
                self.segurando = registro_padrao(false);
            }
        } else if self.segurando.valor && POLITICA_SEM_PAR_AO_SEGURAR == PoliticaSemPar::Pausa {
            // §12.4 (pedido do coordenador, 14/09): o prompter que começa sessão nova **ainda
            // segurando** só pode ter perdido a anterior com o dedo no botão sem que a casca chamasse
            // `perdeu_o_par` — e a queda com o dedo no botão para o texto (decisão do usuário). Sai do
            // segurar como o controle no achado 1: só na réplica daqui (carimbo 0, sem viajar — um
            // play dado no controle durante a queda continua valendo), e antes de o primeiro estado
            // da sessão nova entrar.
            self.mudancas_pendentes |= self.esquecer_o_segurar();
        }
    }

    /// O `"relogio"` que vai no fio: o maior entre o de Lamport e o de parede de quem manda. Um
    /// aparelho parado (sem editar) tem o de Lamport atrás da hora; mandando a hora junto, o outro
    /// lado sabe o relógio de verdade dele — e sabe se ele recusaria um texto por carimbo do futuro
    /// (defeito 5). Quem recebe faz `max` com ele, como já fazia.
    fn relogio_no_fio(&self, agora: Agora) -> u64 {
        self.relogio.max(agora.parede_ms)
    }

    fn mensagem_de_estado(&self, agora: Agora) -> String {
        let e = MensagemDeEstado {
            app: APP.into(),
            v: VERSAO,
            tipo: "estado".into(),
            relogio: self.relogio_no_fio(agora),
            rolando: self.rolando.clone(),
            velocidade: self.velocidade.clone(),
            fonte: self.fonte.clone(),
            margem: self.margem.clone(),
            linha_de_leitura: self.linha_de_leitura.clone(),
            espelho: self.espelho.clone(),
            posicao: self.posicao.clone(),
            salto: self.salto.clone(),
            para_tras: self.para_tras.clone(),
            segurando: self.segurando.clone(),
            entende_segurar: self.papel == Papel::Teleprompter && self.segurar_ligado,
            gravando_ha_ms: (self.papel == Papel::Teleprompter && self.gravacao.carimbo > 0).then(|| {
                GravandoNoFio {
                    valor: self.gravacao.ha_ms(agora),
                    carimbo: self.gravacao.carimbo,
                    autor: self.gravacao.autor.clone(),
                }
            }),
            entende_gravar: self.papel == Papel::Teleprompter && self.gravacao_ligada,
            pedido_de_gravacao: match self.papel {
                Papel::ControleRemoto => self.pedido_de_gravacao.as_ref().map(|p| PedidoNoFio {
                    n: p.n,
                    gravar: p.gravar,
                    autor: p.autor.clone(),
                }),
                _ => None,
            },
            resposta_de_gravacao: match self.papel {
                Papel::Teleprompter => self.resposta_de_gravacao.clone(),
                _ => None,
            },
            texto: self.referencia_anunciada(),
        };
        serde_json::to_string(&e).unwrap_or_default()
    }

    fn mensagem_de_texto(&self, agora: Agora) -> String {
        let t = MensagemDeTexto {
            app: APP.into(),
            v: VERSAO,
            tipo: "texto".into(),
            relogio: self.relogio_no_fio(agora),
            texto: self.texto.clone(),
        };
        serde_json::to_string(&t).unwrap_or_default()
    }

    /// A próxima mensagem devida agora, se houver. Não marca nada: quem manda chama
    /// [`Replica::marcar_enviada`] depois que a mensagem de fato saiu.
    ///
    /// `pendente_no_canal` é o que está esperando para sair na biblioteca: com mais que
    /// [`LIMITE_DO_BUFFER_PARA_TEXTO`], o texto espera (o estado não).
    pub(crate) fn proxima_devida(
        &self,
        agora: Agora,
        pendente_no_canal: usize,
    ) -> Option<(TipoDeMensagem, String)> {
        let desde = |t: Option<u64>| t.map(|t| agora.mono_ms.saturating_sub(t));
        let desde_texto = desde(self.envio.ultimo_texto_ms);
        // A edição daqui sai assim que o buffer deixa (edições seguidas enquanto o texto anterior
        // ainda está saindo se juntam numa só: a mensagem é montada do registrador, que só tem a
        // última). O reenvio pedido pelo par respeita os 2 s desde qualquer envio de texto.
        // E no máximo `REENVIOS_DO_MESMO_TEXTO` vezes o mesmo texto por sessão: um par que continua
        // pedindo depois disso está recusando o texto, e mandar de novo não o convence.
        // Retido (§11.3), o texto daqui não sai — nem por edição nem a pedido do par. Os pedidos
        // continuam marcados, e valem quando solta.
        let texto_vencido = !self.retido()
            && pendente_no_canal <= LIMITE_DO_BUFFER_PARA_TEXTO
            && (self.envio.texto_local_devido
                || (self.envio.texto_pedido_pelo_par
                    && !self.par_recusaria_o_texto()
                    && self.envio.reenvios_do_texto < REENVIOS_DO_MESMO_TEXTO
                    && desde_texto.is_none_or(|d| d >= ms(REENVIO_DO_TEXTO))));
        if texto_vencido {
            return Some((TipoDeMensagem::Texto, self.mensagem_de_texto(agora)));
        }
        let desde_estado = desde(self.envio.ultimo_estado_ms);
        let batimento = desde_estado.is_none_or(|d| d >= ms(BATIMENTO));
        let posicao = self.envio.posicao_devida
            && desde_estado.is_none_or(|d| d >= ms(INTERVALO_DA_POSICAO));
        // §12.3: o reenvio rápido do grupo do segurar, até o outro lado confirmar.
        let reenvio = self.proximo_reenvio_do_segurar_ms().is_some_and(|t| agora.mono_ms >= t);
        if self.envio.estado_devido || batimento || posicao || reenvio {
            return Some((TipoDeMensagem::Estado, self.mensagem_de_estado(agora)));
        }
        None
    }

    pub(crate) fn marcar_enviada(&mut self, tipo: TipoDeMensagem, agora: Agora) {
        self.relogio_da_ultima_troca = self.relogio;
        match tipo {
            TipoDeMensagem::Texto => {
                // Um envio que não foi por edição daqui é um reenvio a pedido do par.
                if !self.envio.texto_local_devido {
                    self.envio.reenvios_do_texto += 1;
                }
                self.envio.texto_local_devido = false;
                self.envio.texto_pedido_pelo_par = false;
                self.envio.ultimo_texto_ms = Some(agora.mono_ms);
                // O estado vai logo atrás, com a referência nova.
                self.envio.estado_devido = true;
                self.contadores.textos_enviados += 1;
            }
            TipoDeMensagem::Estado => {
                self.envio.estado_devido = false;
                self.envio.posicao_devida = false;
                self.envio.ultimo_estado_ms = Some(agora.mono_ms);
                self.contadores.estados_enviados += 1;
                // §12.3: todo estado leva o grupo do segurar, então qualquer um que saia depois de um
                // dos instantes do reenvio rápido conta como esse reenvio.
                if let Some(r) = self.envio.reenvio_do_segurar.as_mut() {
                    while REENVIOS_DO_SEGURAR
                        .get(r.feitos)
                        .is_some_and(|d| agora.mono_ms >= r.desde_ms.saturating_add(ms(*d)))
                    {
                        r.feitos += 1;
                    }
                    if r.feitos >= REENVIOS_DO_SEGURAR.len() {
                        self.envio.reenvio_do_segurar = None;
                    }
                }
            }
        }
    }

    /// O par recusaria o nosso texto? O carimbo dele está mais de [`TOLERANCIA_DO_FUTURO`] à frente
    /// do relógio que o par mostrou — ele o conta como carimbo do futuro e não o funde. Sem relógio
    /// do par ainda, não se sabe: `false`.
    fn par_recusaria_o_texto(&self) -> bool {
        self.par
            .relogio
            .is_some_and(|r| self.texto.carimbo > r.saturating_add(ms(TOLERANCIA_DO_FUTURO)))
    }

    /// O texto mudou (aqui ou lá): a conta de reenvios recomeça.
    fn texto_mudou(&mut self) {
        self.resumo_do_texto = resumo(&self.texto.valor);
        self.envio.reenvios_do_texto = 0;
        self.envio.desistiu_do_texto = false;
    }

    /// Manda pelo mensageiro tudo o que está devido. Devolve **se a sessão acabou** — e só isso:
    /// nenhuma falha de envio é erro de quem chama.
    ///
    /// - canal ainda não aberto: o que estava devido continua devido, e sai na próxima;
    /// - sessão acabada: devolve `true`, e **quem chama continua lendo** — a última mensagem do
    ///   outro lado pode estar na fila (defeito 1 da revisão de 13/09: o `?` daqui fazia a
    ///   bombeada voltar "fechou" sem ler, ou jogar fora o que já tinha fundido);
    /// - mensagem impossível de mandar (`Invalid`: um texto que, escapado, não cabe no canal):
    ///   sai da lista do que é devido e é contada em `mensagens_impossiveis`. Nunca trava a
    ///   leitura (defeito 4b: um texto assim ficava primeiro na fila para sempre, e toda bombeada
    ///   falhava antes de ler).
    fn enviar_devidas(&mut self, m: &Mensageiro) -> bool {
        if m.sessao() != self.sessao {
            let agora = self.agora();
            self.nova_sessao_com_par(m.sessao(), m.par(), agora);
        }
        // No máximo duas por volta (texto e estado), e mais uma se uma delas foi descartada.
        for _ in 0..3 {
            let agora = self.agora();
            let Some((tipo, msg)) = self.proxima_devida(agora, m.pendente()) else {
                return false;
            };
            match m.enviar(&msg) {
                Ok(()) => self.marcar_enviada(tipo, agora),
                Err(Error::Closed) => {
                    self.sessao_acabou = true;
                    return true;
                }
                Err(Error::Invalid(_)) => {
                    self.contadores.mensagens_impossiveis += 1;
                    self.descartar_devida(tipo);
                }
                // Ainda não abriu, ou a biblioteca recusou agora: fica devida.
                Err(_) => return false,
            }
        }
        false
    }

    /// Tira da lista do que é devido uma mensagem que não há como mandar.
    fn descartar_devida(&mut self, tipo: TipoDeMensagem) {
        match tipo {
            TipoDeMensagem::Texto => {
                self.envio.texto_local_devido = false;
                self.envio.texto_pedido_pelo_par = false;
            }
            TipoDeMensagem::Estado => {
                self.envio.estado_devido = false;
                self.envio.posicao_devida = false;
            }
        }
    }

    /// **A bombeada**: manda o que está devido, espera até `limite` pela primeira mensagem, funde
    /// o que chegou (sem esperar pelas seguintes), manda o que ficou devido por causa disso, e
    /// devolve **o que mudou por causa do outro lado e se a sessão acabou — as duas coisas
    /// juntas**.
    ///
    /// Com a sessão acabada, a bombeada ainda **lê a fila até o fim** e funde o que estava lá: a
    /// última pausa do controle, que chegou antes de o canal fechar, entra e aparece em
    /// `mudancas`. Só depois disso a sessão é dada por acabada (`fechada`).
    ///
    /// Chame da thread da sessão, em laço, com `limite` de no máximo 250 ms (50 a 100 ms é o
    /// recomendado): é ela que faz o batimento e o relato de posição saírem.
    pub fn bombear(&mut self, m: &Mensageiro, limite: Duration) -> Result<Bombeada> {
        let mut fechada = self.enviar_devidas(m);
        let mut mudancas = 0;
        let agora = self.agora();
        let mut espera = if fechada { Duration::ZERO } else { self.espera_da_bombeada(agora, limite) };
        // Um teto por bombeada, para uma rajada não prender a thread da sessão: a fila inteira e
        // a vaga de espiada.
        for _ in 0..=crate::transport::FILA_DE_DADOS {
            match m.proxima(espera) {
                Ok(Some(texto)) => {
                    let agora = self.agora();
                    mudancas |= self.receber_em(&texto, agora);
                    espera = Duration::ZERO;
                }
                Ok(None) => break,
                Err(_) => {
                    fechada = true;
                    break;
                }
            }
        }
        let agora = self.agora();
        mudancas |= self.olhar_o_par(agora);
        mudancas |= self.vigiar_o_segurar_em(agora);
        if !fechada {
            fechada = self.enviar_devidas(m);
        }
        self.sessao_acabou |= fechada;
        mudancas |= std::mem::take(&mut self.mudancas_pendentes);
        Ok(Bombeada { mudancas, fechada })
    }
}

/// O resultado de uma bombeada: o que mudou por causa do outro lado, **e** se a sessão acabou.
/// Os dois juntos, de propósito — ver [`Replica::bombear`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bombeada {
    /// Os bits de [`mudou`].
    pub mudancas: Mudancas,
    /// A sessão acabou (a fila já foi lida até o fim). Aplique `mudancas` e chame
    /// [`Replica::perdeu_o_par`].
    pub fechada: bool,
}

/// Qual das duas mensagens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TipoDeMensagem {
    Texto,
    Estado,
}

// =============================================================================================
// A réplica para mais de uma thread
// =============================================================================================

/// **A réplica atrás de um cadeado**, para a tela editar de uma thread e a sessão bombear de
/// outra. É o que a fronteira C embrulha (`QuallTeleprompter`) e o que o Windows usa.
///
/// - A bombeada **não segura o cadeado enquanto espera mensagem**: uma edição da tela nunca espera
///   o prazo de uma bombeada.
/// - **A edição sai já**: guardado o mensageiro da última bombeada, uma edição da tela manda o
///   estado na hora, da thread da tela — [`Mensageiro::enviar`] pode ser chamado de qualquer
///   thread e não bloqueia. Um "pausar" não espera a bombeada seguinte.
#[derive(Debug)]
pub struct Teleprompter {
    interno: Mutex<Interno>,
}

#[derive(Debug)]
struct Interno {
    replica: Replica,
    mensageiro: Option<Mensageiro>,
}

impl Teleprompter {
    pub fn nova(autor: &str, papel: Papel) -> Result<Teleprompter> {
        Ok(Teleprompter {
            interno: Mutex::new(Interno { replica: Replica::nova(autor, papel)?, mensageiro: None }),
        })
    }

    pub fn de_salvo(autor: &str, papel: Papel, json: &str) -> Result<Teleprompter> {
        Ok(Teleprompter {
            interno: Mutex::new(Interno {
                replica: Replica::de_salvo(autor, papel, json)?,
                mensageiro: None,
            }),
        })
    }

    fn com<R>(&self, f: impl FnOnce(&mut Interno) -> R) -> Result<R> {
        let mut g = self
            .interno
            .lock()
            .map_err(|_| Error::Invalid("réplica do teleprompter envenenada por um panic".into()))?;
        Ok(f(&mut g))
    }

    /// Uma edição local, e o envio na hora pelo mensageiro da última bombeada, se houver. Falha
    /// de envio aqui não é erro da edição: o que não saiu fica devido e sai na bombeada.
    fn editar(&self, f: impl FnOnce(&mut Replica) -> Result<()>) -> Result<()> {
        self.com(|i| {
            f(&mut i.replica)?;
            if let Some(m) = i.mensageiro.as_ref() {
                let _ = i.replica.enviar_devidas(m);
            }
            Ok(())
        })?
    }

    pub fn definir_texto(&self, texto: &str) -> Result<()> {
        self.editar(|r| r.definir_texto(texto))
    }
    pub fn definir_rolando(&self, v: bool) -> Result<()> {
        self.editar(|r| r.definir_rolando(v))
    }
    pub fn definir_velocidade(&self, v: f64) -> Result<()> {
        self.editar(|r| r.definir_velocidade(v))
    }
    pub fn definir_fonte(&self, v: f64) -> Result<()> {
        self.editar(|r| r.definir_fonte(v))
    }
    pub fn definir_margem(&self, v: f64) -> Result<()> {
        self.editar(|r| r.definir_margem(v))
    }
    pub fn definir_linha_de_leitura(&self, v: f64) -> Result<()> {
        self.editar(|r| r.definir_linha_de_leitura(v))
    }
    pub fn definir_espelho(&self, v: bool) -> Result<()> {
        self.editar(|r| r.definir_espelho(v))
    }
    pub fn definir_posicao(&self, v: f64) -> Result<()> {
        // O relato de posição não sai na hora: é limitado a 4 Hz e quem o manda é a bombeada.
        self.com(|i| i.replica.definir_posicao(v))?
    }
    pub fn saltar(&self, v: f64) -> Result<()> {
        self.editar(|r| r.saltar(v))
    }
    pub fn saltar_relativo(&self, delta: f64) -> Result<()> {
        self.editar(|r| r.saltar_relativo(delta))
    }
    pub fn perdeu_o_par(&self) -> Result<Mudancas> {
        self.com(|i| {
            i.mensageiro = None;
            i.replica.perdeu_o_par()
        })
    }
    pub fn estado(&self) -> Result<Estado> {
        self.com(|i| i.replica.estado())
    }
    pub fn texto(&self) -> Result<String> {
        self.com(|i| i.replica.texto().to_string())
    }
    /// Escreve o texto em `f` sem copiar para uma `String` antes (a fronteira C o copia direto
    /// para o buffer da casca).
    pub fn com_o_texto<R>(&self, f: impl FnOnce(&str) -> R) -> Result<R> {
        self.com(|i| f(i.replica.texto()))
    }
    pub fn salvo_json(&self) -> Result<String> {
        self.com(|i| i.replica.salvo_json())
    }
    pub fn papel(&self) -> Result<Papel> {
        self.com(|i| i.replica.papel())
    }
    /// A escolha da pergunta do texto: [`Replica::resolver_texto`]. Como as edições, sai na hora.
    /// Depois de `Ok`, grave o salvo (§11.5).
    pub fn resolver_texto(&self, manter_o_meu: bool, resumo_visto: &str) -> Result<()> {
        self.editar(|r| r.resolver_texto(manter_o_meu, resumo_visto))
    }
    /// O texto do prompter na pergunta aberta. Ver [`Replica::texto_da_pergunta`].
    pub fn texto_da_pergunta(&self) -> Result<Option<String>> {
        self.com(|i| i.replica.texto_da_pergunta().map(str::to_string))
    }
    /// Como [`Teleprompter::com_o_texto`], para o texto do prompter na pergunta aberta.
    pub fn com_o_texto_da_pergunta<R>(&self, f: impl FnOnce(Option<&str>) -> R) -> Result<R> {
        self.com(|i| f(i.replica.texto_da_pergunta()))
    }
    /// O texto inteiro de uma cópia, pelo resumo. Ver [`Replica::copia_do_texto`].
    pub fn copia_do_texto(&self, resumo: &str) -> Result<Option<String>> {
        self.com(|i| i.replica.copia_do_texto(resumo).map(str::to_string))
    }
    /// Como [`Teleprompter::com_o_texto`], para uma cópia.
    pub fn com_a_copia_do_texto<R>(&self, resumo: &str, f: impl FnOnce(Option<&str>) -> R) -> Result<R> {
        self.com(|i| f(i.replica.copia_do_texto(resumo)))
    }
    /// Apaga uma cópia. Ver [`Replica::esquecer_copia_do_texto`].
    pub fn esquecer_copia_do_texto(&self, resumo: &str) -> Result<bool> {
        self.com(|i| i.replica.esquecer_copia_do_texto(resumo))
    }
    /// Liga a pergunta do texto. Ver [`Replica::ligar_pergunta_do_texto`].
    pub fn ligar_pergunta_do_texto(&self) -> Result<()> {
        self.com(|i| i.replica.ligar_pergunta_do_texto())
    }
    /// Diz que a tela deste prompter entende o "segurar". Ver [`Replica::ligar_segurar`].
    pub fn ligar_segurar(&self) -> Result<()> {
        self.editar(|r| {
            r.ligar_segurar();
            Ok(())
        })
    }
    /// Aperta o botão do "segurar para rolar": sai na hora, numa mensagem. Ver [`Replica::segurar`].
    pub fn segurar(&self, para_tras: bool) -> Result<()> {
        self.editar(|r| r.segurar(para_tras))
    }
    /// Solta o botão: sai na hora, numa mensagem. Ver [`Replica::soltar`].
    pub fn soltar(&self) -> Result<()> {
        self.editar(|r| r.soltar())
    }
    /// Diz se a tela deste prompter grava (§13). Ver [`Replica::ligar_gravacao`].
    pub fn ligar_gravacao(&self, ligada: bool) -> Result<()> {
        self.editar(|r| {
            r.ligar_gravacao(ligada);
            Ok(())
        })
    }
    /// O relato da gravação, só no prompter: sai na hora. Ver [`Replica::definir_gravando`].
    pub fn definir_gravando(&self, gravando: bool) -> Result<()> {
        self.editar(|r| r.definir_gravando(gravando))
    }
    /// Recusa o pedido aberto, só no prompter: sai na hora. Ver [`Replica::recusar_gravacao`].
    pub fn recusar_gravacao(&self, n: u64, motivo: &str) -> Result<()> {
        self.editar(|r| r.recusar_gravacao(n, motivo))
    }
    /// O controle pede que grave: sai na hora. Ver [`Replica::pedir_gravar`].
    pub fn pedir_gravar(&self) -> Result<()> {
        self.editar(|r| r.pedir_gravar())
    }
    /// O controle pede que pare: sai na hora. Ver [`Replica::pedir_parar`].
    pub fn pedir_parar(&self) -> Result<()> {
        self.editar(|r| r.pedir_parar())
    }

    /// A bombeada de [`Replica::bombear`], sem segurar o cadeado durante a espera. Mesma regra:
    /// falha de envio não impede a leitura nem descarta o que foi fundido, e o resultado traz as
    /// mudanças **e** o fim da sessão. `Err` só com o cadeado envenenado por um panic.
    pub fn bombear(&self, m: &Mensageiro, limite: Duration) -> Result<Bombeada> {
        let (mut fechada, ate_o_reenvio) = self.com(|i| {
            i.mensageiro = Some(m.clone());
            let fechada = i.replica.enviar_devidas(m);
            let agora = i.replica.agora();
            (fechada, i.replica.espera_da_bombeada(agora, limite))
        })?;
        let mut mudancas = 0;
        // A espera acaba no próximo reenvio rápido do segurar, se houver (§12.3).
        let mut espera = if fechada { Duration::ZERO } else { ate_o_reenvio };
        for _ in 0..=crate::transport::FILA_DE_DADOS {
            // A espera acontece **fora** do cadeado.
            match m.proxima(espera) {
                Ok(Some(texto)) => {
                    mudancas |= self.com(|i| {
                        let agora = i.replica.agora();
                        i.replica.receber_em(&texto, agora)
                    })?;
                    espera = Duration::ZERO;
                }
                Ok(None) => break,
                Err(_) => {
                    fechada = true;
                    break;
                }
            }
        }
        let (m_par, fechou_agora) = self.com(|i| {
            let agora = i.replica.agora();
            let m_par = i.replica.olhar_o_par(agora) | i.replica.vigiar_o_segurar_em(agora);
            let fechou = if fechada { true } else { i.replica.enviar_devidas(m) };
            // Achado 5 de 14/09: daqui até a sessão seguinte, o segurar é recusado (`CLOSED`).
            i.replica.sessao_acabou |= fechou;
            (m_par | std::mem::take(&mut i.replica.mudancas_pendentes), fechou)
        })?;
        Ok(Bombeada { mudancas: mudancas | m_par, fechada: fechou_agora })
    }
}

// =============================================================================================
// Testes
// =============================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const P: Papel = Papel::Teleprompter;
    const C: Papel = Papel::ControleRemoto;

    fn em(parede_ms: u64, mono_ms: u64) -> Agora {
        Agora { parede_ms, mono_ms }
    }

    fn nova(autor: &str, papel: Papel) -> Replica {
        Replica::nova(autor, papel).expect("réplica")
    }

    /// Tudo o que `de` tem devido agora, já marcado como enviado (canal vazio).
    fn mensagens(de: &mut Replica, agora: Agora) -> Vec<String> {
        let mut v = Vec::new();
        while let Some((tipo, msg)) = de.proxima_devida(agora, 0) {
            de.marcar_enviada(tipo, agora);
            v.push(msg);
        }
        v
    }

    /// Entrega tudo o que `de` tem devido em `a`. Devolve as mudanças em `a`.
    fn trocar(de: &mut Replica, a: &mut Replica, agora_de: Agora, agora_a: Agora) -> Mudancas {
        mensagens(de, agora_de)
            .iter()
            .fold(0, |m, msg| m | a.receber_em(msg, agora_a))
    }

    /// As duas mandam o que têm, cruzado, `vezes` vezes.
    fn cruzar(a: &mut Replica, b: &mut Replica, agora: Agora, vezes: usize) {
        for _ in 0..vezes {
            let ma = mensagens(a, agora);
            let mb = mensagens(b, agora);
            for m in &mb {
                a.receber_em(m, agora);
            }
            for m in &ma {
                b.receber_em(m, agora);
            }
        }
    }

    fn igual(a: &Replica, b: &Replica) {
        let (ea, eb) = (a.estado_em(em(0, 0)), b.estado_em(em(0, 0)));
        assert_eq!(a.texto(), b.texto(), "os textos divergiram");
        assert_eq!(ea.rolando, eb.rolando, "rolando divergiu");
        assert_eq!(ea.velocidade.to_bits(), eb.velocidade.to_bits(), "velocidade divergiu");
        assert_eq!(ea.fonte.to_bits(), eb.fonte.to_bits(), "fonte divergiu");
        assert_eq!(ea.margem.to_bits(), eb.margem.to_bits(), "margem divergiu");
        assert_eq!(ea.linha_de_leitura.to_bits(), eb.linha_de_leitura.to_bits(), "linha divergiu");
        assert_eq!(ea.espelho, eb.espelho, "espelho divergiu");
        assert_eq!(ea.posicao.to_bits(), eb.posicao.to_bits(), "posição divergiu");
        assert_eq!(ea.salto, eb.salto, "salto divergiu");
        assert_eq!((ea.para_tras, ea.segurando), (eb.para_tras, eb.segurando), "o segurar divergiu");
        assert_eq!(a.texto.marca(), b.texto.marca());
    }

    /// O carimbo de uma mudança local é `max(relogio + 1, parede)`, e o relógio salta para o
    /// maior carimbo que chega.
    #[test]
    fn o_carimbo_tem_piso_de_parede_e_respeita_a_causalidade() {
        const T: u64 = 10_000_000;
        let mut a = nova("a", C);
        a.definir_velocidade_em(2.0, em(T, 0)).unwrap();
        assert_eq!(a.velocidade.carimbo, T, "o piso é o relógio de parede");
        a.definir_velocidade_em(3.0, em(T, 1)).unwrap();
        assert_eq!(a.velocidade.carimbo, T + 1, "no mesmo milissegundo, +1");
        a.definir_velocidade_em(4.0, em(T - 100, 2)).unwrap();
        assert_eq!(a.velocidade.carimbo, T + 2, "relógio de parede voltou: o de Lamport não volta");

        // Um aparelho com o relógio de parede UMA HORA atrasado ainda vence quem ele viu.
        const UMA_HORA: u64 = 3_600_000;
        let mut b = nova("b", P);
        trocar(&mut a, &mut b, em(T + 2, 3), em(T + 2 - UMA_HORA, 3));
        b.definir_velocidade_em(5.0, em(T + 2 - UMA_HORA, 4)).unwrap();
        assert!(
            b.velocidade.carimbo > T + 2,
            "quem editou depois de ver a edição do outro tem de vencer, mesmo com o relógio atrasado"
        );
        trocar(&mut b, &mut a, em(T - UMA_HORA, 5), em(T + 10, 5));
        assert_eq!(a.velocidade.valor, 5.0, "a edição causalmente posterior venceu em A");
    }

    /// O caso comum: um edita, o outro vê, o outro edita — nos dois sentidos.
    #[test]
    fn o_ultimo_que_mudou_vence_nos_dois_sentidos() {
        let mut p = nova("prompter", P);
        let mut c = nova("controle", C);
        c.definir_velocidade_em(2.5, em(10_000, 0)).unwrap();
        c.definir_rolando_em(true, em(10_001, 1)).unwrap();
        let m = trocar(&mut c, &mut p, em(10_001, 1), em(10_002, 1));
        assert_ne!(m & mudou::VELOCIDADE, 0);
        assert_ne!(m & mudou::ROLANDO, 0);
        assert!(p.rolando.valor && p.velocidade.valor == 2.5);

        // O prompter pausa pela própria tela; o controle vê.
        p.definir_rolando_em(false, em(10_500, 2)).unwrap();
        let m = trocar(&mut p, &mut c, em(10_500, 2), em(10_501, 2));
        assert_ne!(m & mudou::ROLANDO, 0);
        assert!(!c.rolando.valor);
        cruzar(&mut p, &mut c, em(10_600, 3), 2);
        igual(&p, &c);
    }

    /// **O empate**: as duas edições no mesmo milissegundo, sem uma ter visto a outra. Decide o
    /// autor maior em bytes, e as duas réplicas chegam ao mesmo valor.
    #[test]
    fn empate_de_carimbo_decide_pelo_autor_e_as_duas_convergem() {
        let mut a = nova("aaa", P);
        let mut z = nova("zzz", C);
        a.definir_velocidade_em(2.0, em(50_000, 0)).unwrap();
        z.definir_velocidade_em(7.0, em(50_000, 0)).unwrap();
        assert_eq!(a.velocidade.carimbo, z.velocidade.carimbo, "o empate tem de ser de verdade");
        cruzar(&mut a, &mut z, em(50_001, 1), 1);
        assert_eq!(a.velocidade.valor, 7.0, "\"zzz\" > \"aaa\": vale o de z nos dois");
        assert_eq!(z.velocidade.valor, 7.0);
        cruzar(&mut a, &mut z, em(51_100, 1_100), 2);
        igual(&a, &z);
    }

    /// **A edição cruzada do texto**: os dois editam o roteiro sem ver a edição do outro. A
    /// vencedora entra inteira nos dois; a perdedora sai inteira.
    #[test]
    fn edicao_cruzada_de_texto_converge_para_a_vencedora_inteira() {
        let mut p = nova("prompter", P);
        let mut c = nova("controle", C);
        p.definir_texto_em("Boa noite. Versão do prompter.", em(20_000, 0)).unwrap();
        c.definir_texto_em("Boa noite. Versão do controle, corrigida.", em(20_040, 0)).unwrap();
        cruzar(&mut p, &mut c, em(20_050, 1), 1);
        assert_eq!(p.texto(), "Boa noite. Versão do controle, corrigida.");
        assert_eq!(c.texto(), "Boa noite. Versão do controle, corrigida.");
        cruzar(&mut p, &mut c, em(22_100, 2_100), 2);
        igual(&p, &c);
    }

    /// **Achado E2**: dois aparelhos com o **mesmo `device_id`** (o restauro de iPhone num iPad
    /// copia o id do App Group) editam o texto no mesmo milissegundo. Carimbo e autor iguais,
    /// conteúdos diferentes: sem o resumo na referência, divergiriam em silêncio; com a ordem da
    /// fusão diferente da da anti-entropia, reenviariam para sempre. Tem de convergir **e parar**.
    #[test]
    fn mesmo_carimbo_e_mesmo_autor_com_textos_diferentes_convergem_e_param() {
        let mut a = nova("ipad-restaurado", P);
        let mut b = nova("ipad-restaurado", C);
        a.definir_texto_em("XY", em(30_000, 0)).unwrap();
        b.definir_texto_em("Z", em(30_000, 0)).unwrap();
        assert_eq!(a.texto.marca(), b.texto.marca(), "o empate total tem de ser de verdade");
        let mut textos = 0;
        for k in 0..10u64 {
            let agora = em(30_000 + k * 2_100, k * 2_100);
            let ma = mensagens(&mut a, agora);
            let mb = mensagens(&mut b, agora);
            textos += ma.iter().chain(&mb).filter(|m| m.contains("\"tipo\":\"texto\"")).count();
            for m in &mb {
                a.receber_em(m, agora);
            }
            for m in &ma {
                b.receber_em(m, agora);
            }
        }
        assert_eq!(a.texto(), b.texto(), "divergiram com o mesmo carimbo e autor");
        assert!(textos <= 3, "o texto foi reenviado {textos} vezes: é a tempestade");
    }

    /// Uma edição antiga que chega depois de uma nova — por desordem, duplicata ou reenvio —
    /// perde a comparação e não muda nada.
    #[test]
    fn edicao_antiga_que_chega_depois_da_nova_nao_muda_nada() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        c.definir_velocidade_em(2.0, em(30_000, 0)).unwrap();
        let velha = mensagens(&mut c, em(30_000, 0));
        c.definir_velocidade_em(3.0, em(30_100, 1)).unwrap();
        let nova_ = mensagens(&mut c, em(30_100, 1));
        for m in &nova_ {
            p.receber_em(m, em(30_200, 2));
        }
        let m = velha.iter().fold(0, |acc, x| acc | p.receber_em(x, em(30_300, 3)));
        assert_eq!(m & mudou::VELOCIDADE, 0, "a edição velha não pode mudar nada");
        assert_eq!(p.velocidade.valor, 3.0);
    }

    /// **O reenvio periódico leva o carimbo original.**
    #[test]
    fn o_batimento_nao_recarimba() {
        let mut p = nova("prompter", P);
        let mut c = nova("controle", C);
        p.definir_velocidade_em(2.0, em(40_000, 0)).unwrap();
        trocar(&mut p, &mut c, em(40_000, 0), em(40_000, 0));
        let _eco = mensagens(&mut c, em(40_000, 0));
        let carimbo = p.velocidade.carimbo;
        c.definir_velocidade_em(9.0, em(40_500, 1)).unwrap();
        let batimento = mensagens(&mut p, em(41_500, 1_500));
        assert_eq!(batimento.len(), 1, "um segundo depois sai o batimento");
        assert!(batimento[0].contains(&format!("\"carimbo\":{carimbo}")), "{}", batimento[0]);
        c.receber_em(&batimento[0], em(41_600, 1_600));
        assert_eq!(c.velocidade.valor, 9.0, "o batimento velho não desfaz a edição nova");
    }

    /// **Pausar nunca carrega o roteiro.**
    #[test]
    fn pausar_nao_carrega_o_texto() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        let roteiro = "Uma linha do roteiro, com acento e \"aspas\".\n".repeat(2_000);
        c.definir_texto_em(&roteiro, em(60_000, 0)).unwrap();
        cruzar(&mut c, &mut p, em(60_000, 0), 2);
        assert_eq!(p.texto(), roteiro);

        c.definir_rolando_em(true, em(60_100, 10)).unwrap();
        let saiu = mensagens(&mut c, em(60_100, 10));
        assert_eq!(saiu.len(), 1, "só o estado: {saiu:?}");
        assert!(saiu[0].contains("\"tipo\":\"estado\""));
        assert!(saiu[0].len() < 2_000, "o estado tem {} bytes — está carregando o texto?", saiu[0].len());
    }

    /// **O reencontro depois de uma queda em que os dois editaram.**
    #[test]
    fn reencontro_depois_da_queda_com_os_dois_editando() {
        let mut p = nova("prompter", P);
        let mut c = nova("controle", C);
        c.definir_texto_em("texto v1", em(70_000, 0)).unwrap();
        cruzar(&mut c, &mut p, em(70_000, 0), 2);

        // A queda. Cada um edita sem ver o outro.
        p.definir_espelho_em(true, em(80_000, 100)).unwrap();
        c.definir_fonte_em(72.0, em(80_500, 100)).unwrap();
        p.definir_velocidade_em(1.5, em(81_000, 101)).unwrap();
        c.definir_velocidade_em(4.0, em(82_000, 101)).unwrap();
        p.definir_texto_em("texto v2, do prompter", em(83_000, 102)).unwrap();

        // O reencontro: sessão nova dos dois lados.
        p.nova_sessao(2);
        c.nova_sessao(2);
        cruzar(&mut p, &mut c, em(90_000, 200), 3);
        assert!(c.espelho.valor, "o espelho do prompter sobreviveu");
        assert_eq!(p.fonte.valor, 72.0, "a fonte do controle sobreviveu");
        assert_eq!(p.velocidade.valor, 4.0, "a velocidade mais recente (a do controle) venceu");
        assert_eq!(c.texto(), "texto v2, do prompter", "o texto mais recente chegou ao controle");
        igual(&p, &c);
    }

    /// **Achado E1**: a réplica atravessa sessões, e os campos da sessão seguem o prompter.
    ///
    /// 1. O controle cai com o texto rolando e volta: o prompter seguiu rolando e o controle vê.
    /// 2. O prompter **reinicia** (réplica nova) e o controle, que guardou `rolando=true` e um
    ///    salto, volta: o prompter começa parado e não pula.
    #[test]
    fn os_campos_da_sessao_seguem_o_prompter_ao_reconectar() {
        let mut p = nova("prompter", P);
        let mut c = nova("controle", C);
        p.nova_sessao(1);
        c.nova_sessao(1);
        c.definir_rolando_em(true, em(1_000, 0)).unwrap();
        c.saltar_em(0.5, em(1_001, 0)).unwrap();
        cruzar(&mut c, &mut p, em(1_002, 1), 2);
        assert!(p.rolando.valor && p.salto.valor == Some(0.5));

        // 1. O controle cai e volta; o prompter seguiu rolando.
        p.definir_posicao_em(0.6, em(5_000, 4_000)).unwrap();
        p.nova_sessao(2);
        c.nova_sessao(2);
        assert!(!c.rolando.valor, "o controle zera os campos da sessão ao reconectar");
        let m = trocar(&mut p, &mut c, em(5_100, 4_100), em(5_100, 4_100));
        assert!(c.rolando.valor, "o controle adota: o prompter segue rolando");
        assert_eq!(c.posicao.valor, 0.6);
        assert_eq!(m & mudou::SALTO, mudou::SALTO, "o controle só atualiza a vista do salto");

        // 2. O prompter reinicia; o controle guarda o `rolando=true` e o salto da sessão velha.
        let mut p2 = nova("prompter", P);
        p2.nova_sessao(3);
        c.nova_sessao(3);
        let m = trocar(&mut c, &mut p2, em(9_000, 8_000), em(9_000, 8_000));
        assert!(!p2.rolando.valor, "o prompter reiniciado começou rolando");
        assert_eq!(m & mudou::SALTO, 0, "o prompter reiniciado pulou para o salto de antes");
        cruzar(&mut c, &mut p2, em(9_100, 8_100), 2);
        assert!(!c.rolando.valor, "o controle adota o prompter parado");
    }

    /// **A edição feita antes da primeira bombeada da sessão nova sobrevive.** O controle salta e
    /// dá play logo que a sessão sobe, antes de a thread da sessão bombear pela primeira vez: a
    /// sessão nova não pode apagar isso (o que ela zera é o que veio da sessão anterior). Achado
    /// na suíte da fronteira com a máquina carregada; e na queda também: a edição feita com a
    /// sessão caída vale para a próxima.
    #[test]
    fn a_edicao_feita_antes_da_primeira_bombeada_da_sessao_sobrevive() {
        // Primeira sessão: nada trocado ainda, e o controle já edita.
        let mut c = nova("controle", C);
        c.saltar_em(0.5, em(1_000, 0)).unwrap();
        c.definir_rolando_em(true, em(1_001, 1)).unwrap();
        c.nova_sessao(1);
        assert_eq!(c.salto.valor, Some(0.5), "a sessão nova apagou o salto pedido para ela");
        assert!(c.rolando.valor);

        // A sessão troca mensagens; depois cai, e o controle pausa com ela caída.
        let mut p = nova("prompter", P);
        p.nova_sessao(1);
        cruzar(&mut c, &mut p, em(1_100, 100), 2);
        c.definir_rolando_em(false, em(2_000, 1_000)).unwrap();
        c.nova_sessao(2);
        assert!(!c.rolando.valor, "a pausa feita na queda vale para a sessão seguinte");
        assert_eq!(c.salto.valor, None, "o salto da sessão velha foi zerado");
    }

    /// Um gerador de números determinístico, para as simulações serem repetíveis.
    struct Dado(u64);
    impl Dado {
        fn prox(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn chance(&mut self, por_cento: u64) -> bool {
            self.prox() % 100 < por_cento
        }
    }

    /// **Perda, duplicata e desordem**, simuladas: um canal que perde 30 %, duplica 10 % e entrega
    /// fora de ordem. Os dois lados editam todos os campos, texto inclusive, durante 30 s de
    /// relógio simulado; depois param e o canal continua ruim. Tem de convergir — é a promessa de
    /// que o formato não depende da entrega confiável. Vinte sementes.
    #[test]
    fn converge_com_perda_duplicata_e_desordem() {
        for semente in 1..=20u64 {
            let mut dado = Dado(semente);
            let mut p = nova("prompter", P);
            let mut c = nova("controle", C);
            let mut para_p: Vec<String> = Vec::new();
            let mut para_c: Vec<String> = Vec::new();
            let base = 1_000_000u64;
            for passo in 0..1_200u64 {
                let t = passo * 50;
                let agora = em(base + t, t);
                if passo < 600 {
                    for (r, alvo) in [(&mut p, 0u64), (&mut c, 1u64)] {
                        match dado.prox() % 12 {
                            0 => r.definir_velocidade_em(0.1 + (dado.prox() % 190) as f64 / 10.0, agora).unwrap(),
                            1 => r.definir_rolando_em(dado.chance(50), agora).unwrap(),
                            2 => r.definir_espelho_em(dado.chance(50), agora).unwrap(),
                            3 => r.definir_fonte_em(8.0 + (dado.prox() % 300) as f64 / 3.0, agora).unwrap(),
                            4 => r.definir_margem_em((dado.prox() % 45) as f64 / 100.0, agora).unwrap(),
                            5 => r.definir_linha_de_leitura_em((dado.prox() % 100) as f64 / 99.0, agora).unwrap(),
                            6 => r.saltar_em((dado.prox() % 100) as f64 / 100.0, agora).unwrap(),
                            7 if dado.chance(20) => r
                                .definir_texto_em(&format!("roteiro {alvo}-{passo}-{}", dado.prox()), agora)
                                .unwrap(),
                            8 if alvo == 0 => r.definir_posicao_em((passo % 100) as f64 / 100.0, agora).unwrap(),
                            _ => {}
                        }
                    }
                }
                for (de, fila) in [(&mut p, &mut para_c), (&mut c, &mut para_p)] {
                    for m in mensagens(de, agora) {
                        if dado.chance(30) {
                            continue;
                        }
                        if dado.chance(10) {
                            fila.push(m.clone());
                        }
                        fila.push(m);
                    }
                }
                for (fila, r) in [(&mut para_p, &mut p), (&mut para_c, &mut c)] {
                    let n = fila.len() / 2 + usize::from(!fila.is_empty());
                    for _ in 0..n {
                        let i = (dado.prox() as usize) % fila.len();
                        let m = fila.swap_remove(i);
                        r.receber_em(&m, agora);
                    }
                }
            }
            for m in para_p.drain(..) {
                p.receber_em(&m, em(base + 60_000, 60_000));
            }
            for m in para_c.drain(..) {
                c.receber_em(&m, em(base + 60_000, 60_000));
            }
            for k in 0..6u64 {
                cruzar(&mut p, &mut c, em(base + 61_000 + k * 2_100, 61_000 + k * 2_100), 1);
            }
            igual(&p, &c);
        }
    }

    /// O número que sai no fio volta **idêntico**. Sem a feature `float_roundtrip` do
    /// `serde_json`, `0.1 + 3.7` voltava como `3.8` e as réplicas divergiam para sempre — foi o
    /// que a simulação acima achou em 13/09/2026.
    #[test]
    fn o_numero_volta_do_fio_identico() {
        for v in [0.1_f64 + 3.7, 1.0 / 3.0, 0.1 + 0.2, 19.999_999_999_999_996, 0.000_1, 399.9] {
            let texto = serde_json::to_string(&v).unwrap();
            let voltou: f64 = serde_json::from_str(&texto).unwrap();
            assert_eq!(v.to_bits(), voltou.to_bits(), "{v} saiu como {texto} e voltou {voltou}");
        }
    }

    /// **Achado E5**: o valor que a casca relê e reescreve não gera carimbo. O `Float` do Android
    /// vira `0.10000000149…` em `double`; quantizado, é o mesmo 0,1 que já estava lá.
    #[test]
    fn reescrever_o_mesmo_valor_nao_gera_carimbo() {
        let mut c = nova("controle", C);
        c.definir_margem_em(0.1_f32 as f64, em(1_000, 0)).unwrap();
        assert_eq!(c.margem.carimbo, 0, "0,1 em float é o padrão: não é mudança");
        c.definir_velocidade_em(2.0, em(2_000, 1)).unwrap();
        let carimbo = c.velocidade.carimbo;
        c.definir_velocidade_em(2.0_f32 as f64, em(3_000, 2)).unwrap();
        c.definir_velocidade_em(2.001, em(3_001, 3)).unwrap();
        assert_eq!(c.velocidade.carimbo, carimbo, "2,001 em centésimos é 2,00");
        assert!(c.definir_velocidade_em(f64::NAN, em(3_002, 4)).is_err());
    }

    /// **Achado E3**: um campo que não se lê (o NaN que o `serde_json` escreve como `null`) é
    /// recusado sozinho; o resto da mensagem entra, e a referência do texto também.
    #[test]
    fn campo_ilegivel_nao_derruba_o_estado_inteiro() {
        let mut p = nova("prompter", P);
        let ruim = r#"{"app":"teleprompter","v":1,"tipo":"estado","relogio":9,
            "velocidade":{"valor":null,"carimbo":9,"autor":"x"},
            "fonte":{"valor":"quarenta","carimbo":9,"autor":"x"},
            "espelho":{"valor":true,"carimbo":9,"autor":"x"}}"#;
        let m = p.receber_em(ruim, em(10, 0));
        assert!(p.espelho.valor, "o espelho entrou apesar dos outros dois campos ruins");
        assert_ne!(m & mudou::ESPELHO, 0);
        assert_eq!(p.velocidade.valor, VELOCIDADE_PADRAO);
        assert_eq!(p.estado_em(em(10, 0)).contadores.campos_recusados, 2);
    }

    /// **Achado E4**: carimbo do futuro — mais de 24 h à frente, ou perto de `u64::MAX` — é
    /// recusado, não mexe no relógio e não contamina o que se salva.
    #[test]
    fn carimbo_do_futuro_e_recusado_e_nao_contamina() {
        let agora = em(1_757_800_000_000, 0);
        let mut p = nova("prompter", P);
        for carimbo in [u64::MAX, agora.parede_ms + 25 * 3_600_000] {
            let msg = format!(
                r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":{carimbo},
                "velocidade":{{"valor":9.0,"carimbo":{carimbo},"autor":"relogio-errado"}}}}"#
            );
            p.receber_em(&msg, agora);
        }
        assert_eq!(p.velocidade.valor, VELOCIDADE_PADRAO, "o carimbo do futuro entrou");
        assert!(p.relogio() < agora.parede_ms + 1, "o relógio foi arrastado para o futuro");
        assert!(p.estado_em(agora).contadores.carimbos_do_futuro >= 2);
        // Dentro da tolerância entra: relógio adiantado de uma hora é aceito.
        let msg = format!(
            r#"{{"app":"teleprompter","v":1,"tipo":"estado",
            "velocidade":{{"valor":9.0,"carimbo":{},"autor":"adiantado"}}}}"#,
            agora.parede_ms + 3_600_000
        );
        p.receber_em(&msg, agora);
        assert_eq!(p.velocidade.valor, 9.0);
        // E um salvo com carimbo do futuro é recarimbado agora, como edição local.
        let salvo = format!(
            r#"{{"v":1,"relogio":{m},"fonte":{{"valor":60.0,"carimbo":{m},"autor":"x"}}}}"#,
            m = u64::MAX
        );
        let mut r = nova("eu", C);
        r.carregar(&salvo, agora).unwrap();
        assert_eq!(r.fonte.valor, 60.0);
        assert_eq!(r.fonte.carimbo, agora.parede_ms);
        assert!(r.relogio() <= agora.parede_ms + 24 * 3_600_000);
    }

    /// **Achado E6**: a posição tem um escritor só.
    #[test]
    fn so_o_prompter_relata_a_posicao() {
        let mut c = nova("controle", C);
        assert!(matches!(c.definir_posicao_em(0.5, em(1, 0)), Err(Error::Invalid(_))));
        let mut p = nova("prompter", P);
        p.definir_posicao_em(0.5, em(1, 0)).unwrap();
    }

    /// Dois toques rápidos em "pular" são dois pulos, mesmo antes de o relato mostrar o primeiro.
    #[test]
    fn pular_duas_vezes_rapido_sao_dois_pulos() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        p.definir_posicao_em(0.2, em(1_000, 0)).unwrap();
        trocar(&mut p, &mut c, em(1_000, 0), em(1_000, 0));
        c.saltar_relativo_em(0.1, em(1_100, 100)).unwrap();
        c.saltar_relativo_em(0.1, em(1_150, 150)).unwrap();
        assert_eq!(c.salto.valor, Some(0.4), "0,2 + 0,1 + 0,1");
        c.saltar_relativo_em(-1.0, em(1_200, 200)).unwrap();
        assert_eq!(c.salto.valor, Some(0.0), "o alvo é limitado ao começo");
    }

    /// **O pedido de reenvio do texto morre quando o par mostra que já tem o texto.** Um estado
    /// do prompter que saiu antes de o roteiro chegar pede o reenvio; o seguinte, com o roteiro,
    /// tem de desfazer o pedido — senão 100 KB saem de novo 2 s depois. Achado pela `quall-probe`.
    #[test]
    fn o_texto_nao_e_reenviado_depois_que_o_par_mostra_que_o_tem() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        let estado_velho = mensagens(&mut p, em(1_000, 0)); // antes do texto
        c.definir_texto_em(&"roteiro ".repeat(10_000), em(1_001, 1)).unwrap();
        let do_controle = mensagens(&mut c, em(1_001, 1));
        assert_eq!(c.estado_em(em(0, 0)).contadores.textos_enviados, 1);
        for m in &estado_velho {
            c.receber_em(m, em(1_002, 2)); // chega depois: pede o texto
        }
        for m in &do_controle {
            p.receber_em(m, em(1_003, 3));
        }
        let estado_novo = mensagens(&mut p, em(1_004, 4));
        for m in &estado_novo {
            c.receber_em(m, em(1_005, 5)); // mostra que tem: desfaz o pedido
        }
        let depois = mensagens(&mut c, em(4_000, 3_000));
        assert!(
            depois.iter().all(|m| !m.contains("\"tipo\":\"texto\"")),
            "o roteiro saiu de novo: {} mensagens de texto",
            depois.iter().filter(|m| m.contains("\"tipo\":\"texto\"")).count()
        );
    }

    /// **Achado T2**: o texto só sai com o buffer de saída quase vazio; o estado sai sempre.
    #[test]
    fn o_texto_espera_o_buffer_esvaziar() {
        let mut c = nova("controle", C);
        c.definir_texto_em("roteiro", em(1_000, 0)).unwrap();
        let cheio = LIMITE_DO_BUFFER_PARA_TEXTO + 1;
        let (tipo, _) = c.proxima_devida(em(1_000, 0), cheio).expect("algo sai");
        assert_eq!(tipo, TipoDeMensagem::Estado, "com o buffer cheio, o texto espera");
        c.marcar_enviada(tipo, em(1_000, 0));
        assert!(c.proxima_devida(em(1_010, 10), cheio).is_none());
        let (tipo, _) = c.proxima_devida(em(1_020, 20), 0).expect("com o buffer vazio, sai");
        assert_eq!(tipo, TipoDeMensagem::Texto);
    }

    /// O salto dispara **uma vez** do outro lado, e não dispara de novo com o batimento — e não é
    /// salvo.
    #[test]
    fn o_salto_dispara_uma_vez_e_nao_volta() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        c.saltar_em(0.0, em(100_000, 0)).unwrap();
        let m = trocar(&mut c, &mut p, em(100_000, 0), em(100_000, 0));
        assert_ne!(m & mudou::SALTO, 0, "o salto tinha de chegar");
        assert_eq!(p.estado_em(em(0, 0)).salto, Some(0.0));
        let m = trocar(&mut c, &mut p, em(101_100, 1_100), em(101_100, 1_100));
        assert_eq!(m & mudou::SALTO, 0, "o batimento reaplicou o salto");
        c.saltar_em(0.0, em(103_000, 3_000)).unwrap();
        let m = trocar(&mut c, &mut p, em(103_000, 3_000), em(103_000, 3_000));
        assert_ne!(m & mudou::SALTO, 0, "o mesmo destino, pedido de novo, é salto novo");
        assert!(!c.salvo_json().contains("salto"), "{}", c.salvo_json());
    }

    /// **Persistência**: os seis campos voltam com os carimbos; `rolando`, `posicao` e `salto`
    /// não voltam. E o roteiro de hoje vence o de ontem depois de reabrir o app.
    #[test]
    fn o_salvo_leva_os_carimbos_e_deixa_de_fora_o_que_e_da_sessao() {
        let mut p0 = nova("prompter", P);
        p0.definir_texto_em("roteiro de ontem", em(1_000, 0)).unwrap();
        p0.definir_velocidade_em(3.0, em(1_001, 0)).unwrap();
        p0.definir_rolando_em(true, em(1_002, 0)).unwrap();
        p0.definir_posicao_em(0.5, em(1_003, 0)).unwrap();
        let salvo = p0.salvo_json();
        let mut volta = nova("prompter", P);
        volta.carregar(&salvo, em(2_000, 0)).unwrap();
        assert_eq!(volta.texto(), "roteiro de ontem");
        assert_eq!(volta.velocidade.carimbo, p0.velocidade.carimbo, "o carimbo tem de voltar");
        assert!(!volta.rolando.valor, "toda vida nova começa parada");
        assert_eq!(volta.posicao.valor, 0.0, "e no começo");
        assert!(volta.relogio() >= p0.velocidade.carimbo);

        let mut c = nova("controle", C);
        c.definir_texto_em("roteiro de hoje", em(86_400_000, 0)).unwrap();
        cruzar(&mut volta, &mut c, em(86_400_100, 10), 3);
        assert_eq!(volta.texto(), "roteiro de hoje", "o roteiro de ontem não pode vencer o de hoje");
        assert_eq!(c.texto(), "roteiro de hoje");
    }

    /// **O roteiro vazio é um roteiro**: apagar o texto é uma edição como outra, atravessa, e a
    /// mensagem que o leva não é vazia (é o JSON com `"valor":""`).
    #[test]
    fn apagar_o_roteiro_atravessa() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        c.definir_texto_em("algo", em(1_000, 0)).unwrap();
        cruzar(&mut c, &mut p, em(1_000, 0), 2);
        assert_eq!(p.texto(), "algo");
        c.definir_texto_em("", em(2_000, 1)).unwrap();
        let saiu = mensagens(&mut c, em(2_000, 1));
        assert!(saiu.iter().any(|m| m.contains("\"valor\":\"\"")), "{saiu:?}");
        for m in &saiu {
            p.receber_em(m, em(2_001, 2));
        }
        assert_eq!(p.texto(), "", "o roteiro apagado não chegou");
    }

    /// Os tetos do texto: o cru e o escapado.
    #[test]
    fn texto_acima_do_teto_e_recusado_e_o_anterior_fica() {
        let mut c = nova("controle", C);
        c.definir_texto_em("anterior", em(1, 0)).unwrap();
        let grande = "a".repeat(TETO_DO_TEXTO + 1);
        assert!(matches!(c.definir_texto_em(&grande, em(2, 0)), Err(Error::Invalid(_))));
        let escapa = "\u{1}".repeat(TETO_DO_TEXTO);
        assert!(matches!(c.definir_texto_em(&escapa, em(3, 0)), Err(Error::Invalid(_))));
        assert!(matches!(c.definir_texto_em("com\0nul", em(4, 0)), Err(Error::Invalid(_))));
        assert_eq!(c.texto(), "anterior");
        let no_teto = "b".repeat(TETO_DO_TEXTO);
        c.definir_texto_em(&no_teto, em(5, 0)).unwrap();
        let saiu = mensagens(&mut c, em(5, 0));
        assert!(saiu.iter().all(|m| m.len() <= TETO_DA_MENSAGEM));
        let mut p = nova("prompter", P);
        for m in &saiu {
            p.receber_em(m, em(6, 0));
        }
        assert_eq!(p.texto().len(), TETO_DO_TEXTO);
    }

    /// Valores fora da faixa: recusados na edição local, ignorados (e contados) na chegada.
    #[test]
    fn valor_fora_da_faixa_e_recusado_aqui_e_ignorado_la() {
        let mut c = nova("controle", C);
        assert!(c.definir_velocidade_em(f64::NAN, em(1, 0)).is_err());
        assert!(c.definir_velocidade_em(0.0, em(1, 0)).is_err());
        assert!(c.definir_fonte_em(1_000.0, em(1, 0)).is_err());
        assert!(c.definir_margem_em(0.5, em(1, 0)).is_err());
        assert!(c.saltar_em(1.5, em(1, 0)).is_err());
        let mut p = nova("prompter", P);
        let ruim = r#"{"app":"teleprompter","v":1,"tipo":"estado","relogio":9,
            "velocidade":{"valor":99.0,"carimbo":9,"autor":"x"},
            "espelho":{"valor":true,"carimbo":9,"autor":"x"}}"#;
        let m = p.receber_em(ruim, em(10, 0));
        assert_eq!(p.velocidade.valor, VELOCIDADE_PADRAO);
        assert!(p.espelho.valor, "o resto da mensagem entrou");
        assert_ne!(m & mudou::ESPELHO, 0);
        assert_eq!(p.estado_em(em(10, 0)).contadores.campos_recusados, 1);
    }

    /// O que não é do teleprompter é ignorado e contado.
    #[test]
    fn mensagem_que_nao_e_do_teleprompter_e_ignorada_e_contada() {
        let mut p = nova("prompter", P);
        assert_eq!(p.receber_em("não é json", em(1, 0)) & !mudou::PAR, 0);
        assert_eq!(p.receber_em(r#"{"app":"parede","v":1,"tipo":"estado"}"#, em(1, 0)) & !mudou::PAR, 0);
        assert_eq!(p.receber_em(r#"{"app":"teleprompter","v":2,"tipo":"estado"}"#, em(1, 0)) & !mudou::PAR, 0);
        assert_eq!(p.receber_em(r#"{"app":"teleprompter","v":1,"tipo":"xyz"}"#, em(1, 0)) & !mudou::PAR, 0);
        let k = p.estado_em(em(1, 0)).contadores;
        assert_eq!((k.invalidas, k.de_outro_app, k.de_outra_versao), (2, 1, 1));
    }

    /// **O formato no fio**, literal.
    #[test]
    fn o_formato_no_fio_e_o_do_contrato() {
        let mut c = nova("s24", C);
        c.definir_velocidade_em(1.5, em(1_757_799_990_000, 0)).unwrap();
        c.definir_texto_em("Boa noite.", em(1_757_799_990_001, 0)).unwrap();
        let saiu = mensagens(&mut c, em(1_757_799_990_001, 0));
        assert_eq!(saiu.len(), 2);
        assert_eq!(
            saiu[0],
            r#"{"app":"teleprompter","v":1,"tipo":"texto","relogio":1757799990001,"texto":{"valor":"Boa noite.","carimbo":1757799990001,"autor":"s24"}}"#
        );
        assert_eq!(
            saiu[1],
            format!(
                concat!(
                    r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":1757799990001,"#,
                    r#""rolando":{{"valor":false,"carimbo":0,"autor":""}},"#,
                    r#""velocidade":{{"valor":1.5,"carimbo":1757799990000,"autor":"s24"}},"#,
                    r#""fonte":{{"valor":48.0,"carimbo":0,"autor":""}},"#,
                    r#""margem":{{"valor":0.1,"carimbo":0,"autor":""}},"#,
                    r#""linha_de_leitura":{{"valor":0.3,"carimbo":0,"autor":""}},"#,
                    r#""espelho":{{"valor":false,"carimbo":0,"autor":""}},"#,
                    r#""posicao":{{"valor":0.0,"carimbo":0,"autor":""}},"#,
                    r#""salto":{{"valor":null,"carimbo":0,"autor":""}},"#,
                    r#""texto":{{"carimbo":1757799990001,"autor":"s24","bytes":10,"resumo":"{}"}}}}"#
                ),
                resumo("Boa noite.")
            )
        );
        assert_eq!(resumo("Boa noite.").len(), 16);
    }

    /// **A confirmação**: o controle sabe quando a edição ainda não foi vista, e a confirmação
    /// volta em uma ida e volta — não no batimento seguinte.
    #[test]
    fn a_confirmacao_volta_em_uma_ida_e_volta() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        c.definir_rolando_em(true, em(5_000, 0)).unwrap();
        assert_eq!(c.estado_em(em(5_000, 1_000)).sem_confirmacao_ha_ms, Some(1_000));
        // A mensagem se perde; o prompter manda o batimento dele sem a edição.
        let _perdida = mensagens(&mut c, em(5_000, 0));
        trocar(&mut p, &mut c, em(5_100, 1_500), em(5_100, 1_500));
        assert_eq!(c.estado_em(em(5_100, 1_600)).sem_confirmacao_ha_ms, Some(1_600));
        // O batimento do controle repara; a resposta do prompter sai **já**, sem esperar o dele.
        trocar(&mut c, &mut p, em(6_100, 2_100), em(6_100, 2_100));
        let m = trocar(&mut p, &mut c, em(6_110, 2_110), em(6_110, 2_110));
        assert_ne!(m & mudou::PAR, 0, "a confirmação muda o que a tela mostra");
        assert_eq!(c.estado_em(em(6_110, 2_110)).sem_confirmacao_ha_ms, None);
        assert_eq!(c.estado_em(em(6_110, 2_210)).par_visto_ha_ms, Some(100));
        // E a resposta não vira pingue-pongue.
        assert!(trocar(&mut c, &mut p, em(6_120, 2_120), em(6_120, 2_120)) & !mudou::PAR == 0);
        assert!(mensagens(&mut p, em(6_130, 2_130)).is_empty(), "o prompter respondeu à resposta");
    }

    /// A política sem par, num lugar só.
    #[test]
    fn perder_o_par_segue_a_politica() {
        let mut p = nova("prompter", P);
        p.definir_rolando_em(true, em(1, 0)).unwrap();
        let m = p.perdeu_o_par_em(em(2, 1));
        assert_ne!(m & mudou::PAR, 0);
        match POLITICA_SEM_PAR {
            PoliticaSemPar::ContinuaComoEsta => assert!(p.rolando.valor, "continua rolando"),
            PoliticaSemPar::Pausa => assert!(!p.rolando.valor, "pausou"),
        }
        assert_eq!(p.estado_em(em(2, 1)).par_visto_ha_ms, None);
    }

    #[test]
    fn autor_e_papel_sao_conferidos() {
        assert!(Replica::nova("", P).is_err());
        assert!(Replica::nova(&"x".repeat(TETO_DO_AUTOR + 1), P).is_err());
        assert!(Replica::nova("a", Papel::Desconhecido).is_err());
    }

    /// **De ponta a ponta, por uma sessão de verdade** (127.0.0.1): o prompter hospeda com o
    /// papel, o controle conecta, cada lado bombeia na sua thread, e os dois editam — o controle
    /// manda um roteiro de 100 KB e a velocidade, o prompter liga o espelho e relata a posição,
    /// e as edições da tela saem na hora, de outra thread. As duas réplicas têm de convergir.
    ///
    /// **Isto é loopback, não é a bancada**: prova a integração com o transporte e a sinalização,
    /// não a travessia por rádio.
    #[test]
    fn dois_aparelhos_pela_sessao_de_verdade() {
        use crate::cancel::Cancelamento;
        use crate::pairing::{PairedPeers, Pin};
        use crate::protocol::{Announcement, Capabilities, DeviceId, PROTOCOL_VERSION};
        use crate::session::{conectar, hospedar, EventoDeSessao, SessionConfig};
        use crate::signaling::SignalingServer;
        use crate::transport::TransportConfig;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let anuncio = |id: &str, papel: Papel| Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.into()),
            display_name: id.into(),
            capabilities: Capabilities { screen_source: false, camera_source: false, sink: false },
            screen: None,
            papel: Some(papel),
        };
        let config = |a: Announcement, pin: &Pin| SessionConfig {
            announcement: a,
            pin: Some(pin.clone()),
            known: PairedPeers::new(),
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(30),
            cancelamento: Cancelamento::novo(),
            silencio_do_caminho: None,
        };
        let pin = Pin::parse("717171").expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let (p, a_p) = (pin.clone(), anuncio("prompter-e2e", P));
        let lado_p = std::thread::spawn(move || hospedar(&servidor, config(a_p, &p)));
        let destino = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let mut sessao_c = conectar(destino, config(anuncio("controle-e2e", C), &pin)).expect("controle");
        let mut sessao_p = lado_p.join().expect("thread").expect("prompter");

        let tp = Arc::new(Teleprompter::nova("prompter-e2e", P).unwrap());
        let tc = Arc::new(Teleprompter::nova("controle-e2e", C).unwrap());
        let parar = Arc::new(AtomicBool::new(false));
        let bombas: Vec<_> = [(Arc::clone(&tp), sessao_p.session.mensageiro()), (Arc::clone(&tc), sessao_c.session.mensageiro())]
            .into_iter()
            .map(|(t, m)| {
                let parar = Arc::clone(&parar);
                std::thread::spawn(move || {
                    while !parar.load(Ordering::Relaxed) {
                        if t.bombear(&m, Duration::from_millis(50)).map(|b| b.fechada).unwrap_or(true) {
                            break;
                        }
                    }
                })
            })
            .collect();

        let roteiro: String = (0..2_000)
            .map(|i| format!("Linha {i} do roteiro, com acento: ação, emoção. 🎬\n"))
            .collect();
        assert!(roteiro.len() > 100_000);
        tc.definir_texto(&roteiro).unwrap();
        tc.definir_velocidade(2.5).unwrap();
        tc.definir_rolando(true).unwrap();
        tp.definir_espelho(true).unwrap();
        tp.definir_posicao(0.25).unwrap();

        let fim = std::time::Instant::now() + Duration::from_secs(15);
        let convergiu = loop {
            let (ep, ec) = (tp.estado().unwrap(), tc.estado().unwrap());
            let ok = tp.texto().unwrap() == roteiro
                && ep.velocidade == 2.5
                && ep.rolando
                && ec.espelho
                && ec.posicao == 0.25
                && ec.sem_confirmacao_ha_ms.is_none()
                && ep.sem_confirmacao_ha_ms.is_none();
            if ok || std::time::Instant::now() > fim {
                break ok;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let (ep, ec) = (tp.estado().unwrap(), tc.estado().unwrap());
        parar.store(true, Ordering::Relaxed);
        for b in bombas {
            let _ = b.join();
        }
        assert!(convergiu, "não convergiu em 15 s:\nprompter {ep:?}\ncontrole {ec:?}");
        assert_eq!(ec.contadores.textos_enviados, 1, "o texto saiu uma vez só: {:?}", ec.contadores);
        assert_eq!(ep.contadores.textos_enviados, 0, "o prompter não tinha texto para mandar");
        assert_eq!(sessao_p.proximo_evento(Duration::ZERO), EventoDeSessao::Nenhum);
        assert_eq!(sessao_c.proximo_evento(Duration::ZERO), EventoDeSessao::Nenhum);
        drop(sessao_c);
        drop(sessao_p);
    }

    // -----------------------------------------------------------------------------------------
    // A revisão do código pronto (13/09): um teste por defeito, escrito antes do conserto.
    // -----------------------------------------------------------------------------------------

    /// Um prompter e um controle numa sessão de verdade por 127.0.0.1, com o papel.
    fn sessoes(marca: &str, pin: &str) -> (crate::session::Ready, crate::session::Ready) {
        use crate::cancel::Cancelamento;
        use crate::pairing::{PairedPeers, Pin};
        use crate::protocol::{Announcement, Capabilities, DeviceId, PROTOCOL_VERSION};
        use crate::session::{hospedar, SessionConfig};
        use crate::signaling::SignalingServer;
        use crate::transport::TransportConfig;
        let anuncio = |id: String, papel: Papel| Announcement {
            protocol_version: PROTOCOL_VERSION,
            device_id: DeviceId(id.clone()),
            display_name: id,
            capabilities: Capabilities { screen_source: false, camera_source: false, sink: false },
            screen: None,
            papel: Some(papel),
        };
        let config = move |a: Announcement, pin: &Pin| SessionConfig {
            announcement: a,
            pin: Some(pin.clone()),
            known: PairedPeers::new(),
            transport: TransportConfig::default(),
            tracks: Vec::new(),
            timeout: Duration::from_secs(30),
            cancelamento: Cancelamento::novo(),
            silencio_do_caminho: None,
        };
        let pin = Pin::parse(pin).expect("pin");
        let servidor = SignalingServer::bind(0).expect("bind");
        let porta = servidor.port().expect("porta");
        let (p, a_p) = (pin.clone(), anuncio(format!("prompter-{marca}"), P));
        let lado_p = std::thread::spawn(move || hospedar(&servidor, config(a_p, &p)));
        let destino = format!("127.0.0.1:{porta}").parse().expect("endereço");
        let c = crate::session::conectar(destino, config(anuncio(format!("controle-{marca}"), C), &pin))
            .expect("controle");
        let p = lado_p.join().expect("thread").expect("prompter");
        (p, c)
    }

    /// **Defeito 1 da revisão**: o controle pausa e cai logo em seguida. A pausa já está na fila
    /// do prompter quando o canal fecha; a bombeada tem de fundi-la **e** dizer que a sessão
    /// acabou — nunca devolver só "fechou" e jogar fora o que fundiu, ou a tela seguiria rolando
    /// com a réplica dizendo parado.
    #[test]
    fn a_bombeada_nao_perde_o_ultimo_comando_quando_o_canal_fecha() {
        let (sessao_p, sessao_c) = sessoes("queda", "737373");
        let tp = Teleprompter::nova("prompter-queda", P).unwrap();
        let tc = Teleprompter::nova("controle-queda", C).unwrap();
        let (mp, mc) = (sessao_p.session.mensageiro(), sessao_c.session.mensageiro());
        tc.definir_rolando(true).unwrap();
        let fim = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < fim {
            let _ = tc.bombear(&mc, Duration::from_millis(10));
            let _ = tp.bombear(&mp, Duration::from_millis(10));
            if tp.estado().unwrap().rolando && tc.estado().unwrap().sem_confirmacao_ha_ms.is_none() {
                break;
            }
        }
        assert!(tp.estado().unwrap().rolando, "não rolou");

        // Pausa (sai na hora, da thread de quem edita) e cai.
        tc.definir_rolando(false).unwrap();
        drop(sessao_c);
        // Sem bombear o prompter: espera o canal fechar deste lado e a pausa estar na fila.
        let fim = std::time::Instant::now() + Duration::from_secs(10);
        while mp.enviar("{}").is_ok() && std::time::Instant::now() < fim {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(mp.enviar("{}"), Err(Error::Closed)), "o canal não fechou");
        assert!(mp.entregar_se(Duration::ZERO, |_| false).unwrap_or(None).is_some(), "a pausa não chegou");

        let mut bits = 0;
        let mut fechou = false;
        for _ in 0..5 {
            let b = tp.bombear(&mp, Duration::ZERO).expect("a bombeada só falha com o cadeado envenenado");
            bits |= b.mudancas;
            if b.fechada {
                fechou = true;
                break;
            }
        }
        assert!(fechou, "a bombeada tinha de dizer que a sessão acabou");
        assert_ne!(bits & mudou::ROLANDO, 0, "a pausa foi fundida e o bit se perdeu");
        assert!(!tp.estado().unwrap().rolando, "a pausa não entrou");
        drop(sessao_p);
    }

    /// **Defeito 4b, a outra metade**: uma mensagem impossível de mandar (um texto que, escapado,
    /// não cabe no canal) nunca pode travar a leitura. Forçado por dentro, para não depender de
    /// como ele entrou.
    #[test]
    fn mensagem_impossivel_de_mandar_nao_trava_a_leitura() {
        let (sessao_p, sessao_c) = sessoes("impossivel", "747474");
        let tp = Teleprompter::nova("prompter-imp", P).unwrap();
        let tc = Teleprompter::nova("controle-imp", C).unwrap();
        let (mp, mc) = (sessao_p.session.mensageiro(), sessao_c.session.mensageiro());
        {
            let mut i = tc.interno.lock().unwrap();
            i.replica.texto = Registro { valor: "\u{1}".repeat(50_000), carimbo: 5, autor: "controle-imp".into() };
            i.replica.envio.texto_local_devido = true;
        }
        tp.definir_espelho(true).unwrap();
        let fim = std::time::Instant::now() + Duration::from_secs(10);
        let mut viu_o_espelho = false;
        while std::time::Instant::now() < fim && !viu_o_espelho {
            let _ = tp.bombear(&mp, Duration::from_millis(10));
            let b = tc.bombear(&mc, Duration::from_millis(10)).expect("a bombeada do controle falhou");
            assert!(!b.fechada, "a sessão não fechou");
            viu_o_espelho = tc.estado().unwrap().espelho;
        }
        assert!(viu_o_espelho, "o controle parou de ler por causa de um texto que não cabe");
        assert!(tc.estado().unwrap().contadores.mensagens_impossiveis >= 1, "o texto impossível não foi contado");
        drop(sessao_c);
        drop(sessao_p);
    }

    /// **Defeito 3**: dois toques de "+0,1" com um relato de posição **velho** entre eles — gerado
    /// antes de o prompter aplicar o primeiro salto, mas com carimbo maior (o relógio dele está
    /// adiantado). Tem de dar dois pulos: a base só troca para o relato quando se sabe que ele é
    /// posterior à aplicação do salto.
    #[test]
    fn pular_com_um_relato_velho_no_meio_ainda_sao_dois_pulos() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        p.definir_posicao_em(0.2, em(1_000, 0)).unwrap();
        trocar(&mut p, &mut c, em(1_000, 0), em(1_000, 0));
        c.saltar_relativo_em(0.1, em(1_100, 100)).unwrap();
        assert_eq!(c.salto.valor, Some(0.3));
        // O relato velho: o prompter ainda não recebeu o salto, e o carimbo dele é maior.
        p.definir_posicao_em(0.21, em(1_200, 300)).unwrap();
        trocar(&mut p, &mut c, em(1_200, 300), em(1_200, 300));
        c.saltar_relativo_em(0.1, em(1_250, 350)).unwrap();
        assert_eq!(c.salto.valor, Some(0.4), "o relato velho virou base e os dois toques deram um pulo");

        // Quando o prompter mostra que aplicou o salto, a base passa a ser o relato dele.
        trocar(&mut c, &mut p, em(1_300, 400), em(1_300, 400));
        trocar(&mut p, &mut c, em(1_310, 410), em(1_310, 410));
        assert_eq!(c.posicao.valor, 0.4, "o prompter aplicou o salto e relatou a posição nova");
        c.saltar_relativo_em(0.1, em(1_400, 500)).unwrap();
        assert_eq!(c.salto.valor, Some(0.5));
    }

    /// **Defeito 4a**: um relógio salvo mais de 24 h à frente não pode arrastar a primeira edição
    /// para o futuro — ele é descartado como os carimbos, e não limitado a "agora + 24 h".
    #[test]
    fn relogio_salvo_do_futuro_nao_arrasta_a_primeira_edicao() {
        let agora = em(1_757_800_000_000, 0);
        let salvo = format!(r#"{{"v":1,"relogio":{}}}"#, agora.parede_ms + 48 * 3_600_000);
        let mut r = nova("eu", C);
        r.carregar(&salvo, agora).unwrap();
        assert!(r.relogio() <= agora.parede_ms, "o relógio salvo do futuro entrou: {}", r.relogio());
        r.definir_velocidade_em(2.0, agora).unwrap();
        assert_eq!(r.velocidade.carimbo, agora.parede_ms, "a primeira edição saiu no futuro");
    }

    /// **Defeito 4b**: o salvo passa pela mesma regra da edição. Um texto de 50 mil caracteres
    /// de controle tem 50 KB cru e 300 KB escapado — não cabe numa mensagem, e não pode entrar.
    /// E um `autor` acima de 256 bytes também não.
    #[test]
    fn o_salvo_passa_pela_regra_da_edicao() {
        let agora = em(1_757_800_000_000, 0);
        let texto = serde_json::to_string(&"\u{1}".repeat(50_000)).unwrap();
        let salvo = format!(r#"{{"v":1,"texto":{{"valor":{texto},"carimbo":5,"autor":"x"}}}}"#);
        let mut r = nova("eu", C);
        r.carregar(&salvo, agora).unwrap();
        assert_eq!(r.texto(), "", "o texto que não cabe numa mensagem entrou");
        let longo = "a".repeat(300);
        let salvo = format!(r#"{{"v":1,"fonte":{{"valor":60.0,"carimbo":5,"autor":"{longo}"}}}}"#);
        let mut r = nova("eu", C);
        r.carregar(&salvo, agora).unwrap();
        assert_eq!(r.fonte.valor, FONTE_PADRAO, "o autor de 300 bytes entrou pelo salvo");
    }

    /// E na chegada: um campo com `autor` acima de 256 bytes é recusado e contado.
    #[test]
    fn autor_longo_na_chegada_e_recusado() {
        let mut p = nova("prompter", P);
        let longo = "a".repeat(300);
        let msg = format!(
            r#"{{"app":"teleprompter","v":1,"tipo":"estado","espelho":{{"valor":true,"carimbo":9,"autor":"{longo}"}}}}"#
        );
        p.receber_em(&msg, em(10, 0));
        assert!(!p.espelho.valor, "o autor de 300 bytes entrou");
        assert_eq!(p.estado_em(em(10, 0)).contadores.campos_recusados, 1);
    }

    /// **Defeito 5**: com o relógio de um lado mais de 24 h adiantado, o outro recusa o texto (é
    /// carimbo do futuro) e continua mostrando a referência velha. O texto não pode ser reenviado a
    /// cada 2 s para sempre: 128 KiB sem fim, e o outro lado recusando cada um.
    #[test]
    fn texto_recusado_pelo_par_nao_e_reenviado_para_sempre() {
        const T: u64 = 1_757_800_000_000;
        const ADIANTADO: u64 = 48 * 3_600_000;
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        c.definir_texto_em("roteiro", em(T + ADIANTADO, 0)).unwrap();
        let mut textos = 0;
        for k in 0..20u64 {
            let t = k * 2_100;
            let mc = mensagens(&mut c, em(T + ADIANTADO + t, t));
            textos += mc.iter().filter(|m| m.contains("\"tipo\":\"texto\"")).count();
            for m in &mc {
                p.receber_em(m, em(T + t, t));
            }
            for m in &mensagens(&mut p, em(T + t, t)) {
                c.receber_em(m, em(T + ADIANTADO + t, t));
            }
        }
        assert_eq!(p.texto(), "", "o prompter recusa o texto do futuro");
        assert_eq!(textos, 1, "o texto foi mandado {textos} vezes em 42 s: só a edição, nenhum reenvio");
        assert_eq!(c.estado_em(em(0, 0)).contadores.reenvios_desistidos, 1, "a desistência é contada uma vez");
    }

    #[test]
    fn salvo_ilegivel_ou_de_outra_versao_e_recusado() {
        assert!(Replica::de_salvo("a", C, "{").is_err());
        assert!(Replica::de_salvo("a", C, r#"{"v":2}"#).is_err());
        assert!(Replica::de_salvo("a", C, r#"{"v":1}"#).is_ok(), "salvo sem campo nenhum é o padrão");
    }

    // -----------------------------------------------------------------------------------------
    // §11 do contrato: a pergunta do texto e as cópias. Os achados B1 a B11 das revisões de 14/09
    // têm um teste cada; a tabela está na §11.8.
    // -----------------------------------------------------------------------------------------

    fn par_de(r: &Replica) -> ParDaSessao {
        ParDaSessao { id: r.autor.clone(), nome: format!("aparelho {}", r.autor) }
    }

    /// Uma sessão nova nos dois lados, cada um sabendo quem é o outro — o que `session` faz.
    fn sessao(c: &mut Replica, p: &mut Replica, n: u64, agora: Agora) {
        let (do_c, do_p) = (par_de(p), par_de(c));
        c.nova_sessao_com_par(n, Some(&do_c), agora);
        p.nova_sessao_com_par(n, Some(&do_p), agora);
    }

    fn textos_em(ms: &[String]) -> usize {
        ms.iter().filter(|m| m.contains("\"tipo\":\"texto\"")).count()
    }

    fn pergunta(c: &Replica) -> PerguntaDoTexto {
        c.estado_em(em(0, 0)).pergunta_do_texto.expect("devia haver pergunta do texto")
    }

    fn resumo_do_prompter(c: &Replica) -> String {
        pergunta(c).do_prompter.expect("a pergunta devia estar aberta").resumo
    }

    fn copias(c: &Replica) -> Vec<CopiaDoTexto> {
        c.estado_em(em(0, 0)).copias_do_texto
    }

    /// Um controle e um prompter com roteiros diferentes, num primeiro encontro, com a pergunta
    /// aberta no controle. O do prompter é o mais velho; o relógio monotônico termina em 2 000.
    fn pergunta_aberta(t0: u64) -> (Replica, Replica) {
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p = nova("prompter", P);
        p.definir_texto_em("roteiro do prompter", em(t0, 0)).unwrap();
        c.definir_texto_em("roteiro do controle", em(t0 + 1_000, 0)).unwrap();
        sessao(&mut c, &mut p, 1, em(t0 + 2_000, 2_000));
        cruzar(&mut c, &mut p, em(t0 + 2_000, 2_000), 3);
        assert!(pergunta(&c).aberta, "a pergunta devia ter aberto");
        (c, p)
    }

    /// **A decisão do usuário (13/09)**: o controle que chega num prompter que não é o da última
    /// vez, com roteiro diferente, pergunta — e até a escolha nenhum roteiro some. Sem par (a regra
    /// de antes), o mais velho some em silêncio: é o defeito que a pergunta existe para fechar.
    #[test]
    fn controle_num_prompter_novo_com_roteiro_diferente_pergunta_e_nao_substitui() {
        let mut c0 = nova("controle", C);
        let mut p0 = nova("prompter", P);
        p0.definir_texto_em("roteiro do prompter", em(1_000, 0)).unwrap();
        c0.definir_texto_em("roteiro do controle", em(2_000, 0)).unwrap();
        c0.nova_sessao(1);
        p0.nova_sessao(1);
        cruzar(&mut c0, &mut p0, em(3_000, 1_000), 3);
        assert_eq!(p0.texto(), "roteiro do controle", "sem par, o do prompter some — o defeito");

        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p = nova("prompter", P);
        p.definir_texto_em("roteiro do prompter", em(1_000, 0)).unwrap();
        c.definir_texto_em("roteiro do controle", em(2_000, 0)).unwrap();
        sessao(&mut c, &mut p, 1, em(3_000, 1_000));
        let mut do_controle = 0;
        for k in 0..4u64 {
            let agora = em(3_000 + k * 100, 1_000 + k * 100);
            let mc = mensagens(&mut c, agora);
            do_controle += textos_em(&mc);
            for m in &mc {
                p.receber_em(m, agora);
            }
            for m in &mensagens(&mut p, agora) {
                c.receber_em(m, agora);
            }
        }
        assert_eq!(p.texto(), "roteiro do prompter", "o roteiro do prompter foi substituído antes da escolha");
        assert_eq!(c.texto(), "roteiro do controle", "o roteiro do controle foi substituído antes da escolha");
        assert_eq!(do_controle, 0, "o texto do controle saiu com a pergunta aberta");
        let q = pergunta(&c);
        assert!(q.aberta);
        assert_eq!((q.prompter_id.as_str(), q.prompter_nome.as_str()), ("prompter", "aparelho prompter"));
        assert_eq!(q.meu.bytes, "roteiro do controle".len());
        assert_eq!(q.do_prompter.map(|v| v.resumo), Some(resumo("roteiro do prompter")));
        assert_eq!(c.texto_da_pergunta(), Some("roteiro do prompter"));
        assert!(copias(&c).is_empty(), "nada saiu ainda: nenhuma cópia");
    }

    #[test]
    fn a_pergunta_nao_segura_os_outros_campos() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        c.definir_velocidade_em(3.5, em(20_000, 3_000)).unwrap();
        c.definir_rolando_em(true, em(20_001, 3_001)).unwrap();
        c.saltar_em(0.5, em(20_002, 3_002)).unwrap();
        p.definir_espelho_em(true, em(20_003, 3_003)).unwrap();
        cruzar(&mut c, &mut p, em(20_100, 3_100), 2);
        assert!(p.rolando.valor && p.velocidade.valor == 3.5, "o controle não comandou com a pergunta aberta");
        assert_eq!(p.salto.valor, Some(0.5), "o salto não atravessou");
        assert!(c.espelho.valor, "o espelho do prompter não chegou");
        assert!(pergunta(&c).aberta, "a pergunta fechou sozinha");
    }

    #[test]
    fn a_pergunta_nao_acusa_falta_de_confirmacao() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        cruzar(&mut c, &mut p, em(15_000, 4_000), 2);
        assert_eq!(c.estado_em(em(15_000, 9_000)).sem_confirmacao_ha_ms, None, "o controle acusa o texto retido");
        assert_eq!(p.estado_em(em(15_000, 9_000)).sem_confirmacao_ha_ms, None, "o prompter acusa o texto dele");
    }

    #[test]
    fn usar_o_do_prompter_adota_o_registro_e_guarda_copia_do_meu() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        let carimbo_do_prompter = p.texto.carimbo;
        c.resolver_texto_em(false, &resumo_do_prompter(&c), em(16_000, 3_000)).unwrap();
        assert_eq!(c.texto(), "roteiro do prompter");
        assert_eq!(c.texto.carimbo, carimbo_do_prompter, "adotou o texto, mas não o registro do prompter");
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none());
        let cs = copias(&c);
        assert_eq!(cs.len(), 1);
        assert_eq!((cs[0].origem, cs[0].prompter_id.as_str()), (OrigemDaCopia::Controle, "prompter"));
        assert_eq!(c.copia_do_texto(&cs[0].resumo), Some("roteiro do controle"));
        let mut do_controle = 0;
        for k in 0..3u64 {
            let agora = em(16_100 + k * 2_100, 3_100 + k * 2_100);
            let mc = mensagens(&mut c, agora);
            do_controle += textos_em(&mc);
            for m in &mc {
                p.receber_em(m, agora);
            }
            for m in &mensagens(&mut p, agora) {
                c.receber_em(m, agora);
            }
        }
        assert_eq!(do_controle, 0, "o controle devolveu ao prompter o próprio texto dele");
        igual(&c, &p);
        assert_eq!(c.ultimo_prompter_id.as_deref(), Some("prompter"), "a convergência não gravou o da última vez");
    }

    #[test]
    fn mandar_o_meu_vence_e_guarda_copia_do_prompter() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        c.resolver_texto_em(true, &resumo_do_prompter(&c), em(16_000, 3_000)).unwrap();
        assert!(c.texto.carimbo > p.texto.carimbo, "recarimbado abaixo do texto do prompter");
        cruzar(&mut c, &mut p, em(16_100, 3_100), 3);
        assert_eq!(p.texto(), "roteiro do controle");
        let cs = copias(&c);
        assert_eq!((cs.len(), cs[0].origem), (1, OrigemDaCopia::Prompter));
        assert_eq!(c.copia_do_texto(&cs[0].resumo), Some("roteiro do prompter"));
        igual(&c, &p);
        assert_eq!(c.ultimo_prompter_id.as_deref(), Some("prompter"));
    }

    /// **Achado B2**: a escolha só vale contra o que a pessoa viu e contra a **maior** referência
    /// de texto do prompter desta sessão — não a do último estado: com o canal sem ordem, o estado
    /// E1, atrasado, chega depois do E2 e diria que está tudo em dia.
    #[test]
    fn a_escolha_so_vale_com_a_pergunta_atualizada() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        let visto = resumo_do_prompter(&c);
        assert!(
            matches!(c.resolver_texto_em(true, "0000000000000000", em(16_000, 3_000)), Err(Error::Ocupado(_))),
            "valeu uma escolha sobre um texto que a pessoa não viu"
        );
        let e1 = mensagens(&mut p, em(16_000, 4_000));
        assert!(!e1.is_empty() && textos_em(&e1) == 0, "o batimento do prompter devia sair: {e1:?}");
        p.definir_texto_em("roteiro do prompter, versão 2", em(17_000, 5_000)).unwrap();
        let (t2, e2): (Vec<_>, Vec<_>) =
            mensagens(&mut p, em(17_000, 5_000)).into_iter().partition(|m| m.contains("\"tipo\":\"texto\""));
        // E2 chega; o texto T2 ainda não; e o E1 atrasado chega por último.
        for m in e2.iter().chain(e1.iter()) {
            c.receber_em(m, em(17_100, 5_100));
        }
        assert!(
            matches!(c.resolver_texto_em(true, &visto, em(17_200, 5_200)), Err(Error::Ocupado(_))),
            "a escolha passou com o prompter tendo um texto mais novo a caminho"
        );
        assert!(pergunta(&c).aberta, "a escolha recusada fechou a pergunta");
        let m = t2.iter().fold(0, |acc, x| acc | c.receber_em(x, em(17_300, 5_300)));
        assert_ne!(m & mudou::PERGUNTA_DO_TEXTO, 0, "a pergunta mudou sem o bit");
        let visto2 = resumo_do_prompter(&c);
        assert_eq!(visto2, resumo("roteiro do prompter, versão 2"));
        assert!(matches!(c.resolver_texto_em(true, &visto, em(17_400, 5_400)), Err(Error::Ocupado(_))));
        c.resolver_texto_em(true, &visto2, em(17_500, 5_500)).unwrap();
        assert_eq!(c.copia_do_texto(&visto2), Some("roteiro do prompter, versão 2"), "a cópia não é do que saiu");
    }

    /// **Achado B1 (alta)**: a pergunta vale por sessão. Volta "o mesmo prompter" — o mesmo
    /// `device_id` — com um roteiro **mais velho** (um iPad restaurado do backup de um iPhone): a
    /// sessão nova recomeça comparando, e é o roteiro dele agora que entra na pergunta. Mantida de
    /// uma sessão para a outra, o controle anunciaria o texto guardado (maior), o prompter não
    /// mandaria o dele, e "usar o do prompter" apagaria o roteiro que voltou, sem cópia.
    #[test]
    fn a_pergunta_vale_por_sessao() {
        let (mut c, p) = pergunta_aberta(10_000);
        c.perdeu_o_par_em(em(20_000, 3_000));
        assert!(pergunta(&c).aberta, "a pergunta sumiu da tela na queda");
        assert!(
            matches!(c.resolver_texto_em(false, &resumo_do_prompter(&c), em(20_100, 3_100)), Err(Error::Closed)),
            "a escolha passou sem o prompter conectado"
        );
        drop(p);
        let mut restaurado = nova("prompter", P);
        restaurado.definir_texto_em("roteiro de antes do backup", em(5_000, 0)).unwrap();
        sessao(&mut c, &mut restaurado, 2, em(21_000, 4_000));
        let primeiro = mensagens(&mut c, em(21_000, 4_000));
        assert!(
            primeiro.iter().any(|m| m.contains("\"texto\":{\"carimbo\":0,")),
            "a sessão nova não recomeçou comparando: {primeiro:?}"
        );
        for m in &primeiro {
            restaurado.receber_em(m, em(21_000, 4_000));
        }
        cruzar(&mut c, &mut restaurado, em(21_100, 4_100), 3);
        assert_eq!(c.texto_da_pergunta(), Some("roteiro de antes do backup"), "a pergunta ficou com o texto da sessão velha");
        c.resolver_texto_em(false, &resumo_do_prompter(&c), em(21_200, 4_200)).unwrap();
        cruzar(&mut c, &mut restaurado, em(21_300, 4_300), 3);
        assert_eq!(restaurado.texto(), "roteiro de antes do backup", "o roteiro do prompter que voltou sumiu");
        assert_eq!(c.texto(), "roteiro de antes do backup");
        assert!(c.copia_do_texto(&resumo("roteiro do controle")).is_some());
    }

    /// **Achado B3**: o primeiro encontro termina na convergência, não na escolha. Uma edição do
    /// prompter em voo depois da escolha é decidida por "vale o último" — e o perdedor, dos dois
    /// lados, fica guardado.
    #[test]
    fn o_primeiro_encontro_termina_na_convergencia() {
        // (a) Depois de "mandar o meu", o prompter confirma uma edição mais nova: o do controle perde.
        let (mut c, mut p) = pergunta_aberta(10_000);
        c.resolver_texto_em(true, &resumo_do_prompter(&c), em(16_000, 3_000)).unwrap();
        let em_voo = mensagens(&mut c, em(16_000, 3_000));
        assert_eq!(textos_em(&em_voo), 1);
        p.definir_texto_em("edição do prompter, em voo", em(16_500, 3_500)).unwrap();
        for m in &em_voo {
            p.receber_em(m, em(16_600, 3_600));
        }
        cruzar(&mut c, &mut p, em(16_700, 3_700), 3);
        igual(&c, &p);
        assert_eq!(c.texto(), "edição do prompter, em voo");
        assert!(c.copia_do_texto(&resumo("roteiro do controle")).is_some(), "o texto do controle sumiu sem cópia");

        // (b) A edição do prompter é mais velha que o texto recarimbado: ela perde, e fica guardada.
        let (mut c, mut p) = pergunta_aberta(10_000);
        p.definir_texto_em("edição do prompter, mais velha", em(15_900, 2_900)).unwrap();
        let do_prompter = mensagens(&mut p, em(15_900, 2_900));
        c.resolver_texto_em(true, &resumo_do_prompter(&c), em(16_000, 3_000)).unwrap();
        for m in &do_prompter {
            c.receber_em(m, em(16_100, 3_100));
        }
        cruzar(&mut c, &mut p, em(16_200, 3_200), 3);
        igual(&c, &p);
        assert_eq!(p.texto(), "roteiro do controle");
        assert!(
            c.copia_do_texto(&resumo("edição do prompter, mais velha")).is_some(),
            "a edição do prompter sumiu sem cópia"
        );
    }

    /// **Achado B4**: o prompter está com o editor aberto (texto vazio, carimbo 0) quando a sessão
    /// sobe; o controle solta e manda o dele; o prompter confirma o seu 50 ms depois. O do controle
    /// perde por "vale o último" — e fica guardado, em vez de sumir sem pergunta.
    #[test]
    fn o_prompter_que_nunca_escreveu_e_confirma_logo_depois_nao_apaga_o_do_controle() {
        // Nos dois modos: com a pergunta ligada (o "comparando" solta no estado de quem nunca
        // escreveu) e sem ela (vale "o último que mudou" desde o começo). Nos dois, a cópia.
        for com_pergunta in [true, false] {
            let mut c = nova("controle", C);
            if com_pergunta {
                c.ligar_pergunta_do_texto();
            }
            let mut p = nova("prompter", P);
            c.definir_texto_em("roteiro do controle", em(10_000, 0)).unwrap();
            sessao(&mut c, &mut p, 1, em(11_000, 1_000));
            for m in &mensagens(&mut p, em(11_000, 1_000)) {
                c.receber_em(m, em(11_000, 1_000));
            }
            assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none(), "o prompter que nunca escreveu devia soltar");
            let do_controle = mensagens(&mut c, em(11_010, 1_010));
            assert_eq!(textos_em(&do_controle), 1, "com a pergunta: {com_pergunta}");
            p.definir_texto_em("roteiro que o prompter digitou", em(11_060, 1_060)).unwrap();
            for m in &do_controle {
                p.receber_em(m, em(11_070, 1_070));
            }
            cruzar(&mut c, &mut p, em(11_100, 1_100), 3);
            igual(&c, &p);
            assert_eq!(c.texto(), "roteiro que o prompter digitou");
            assert!(
                c.copia_do_texto(&resumo("roteiro do controle")).is_some(),
                "o roteiro do controle sumiu sem cópia (com a pergunta: {com_pergunta})"
            );
            assert_eq!(c.ultimo_prompter_id.as_deref(), Some("prompter"));
        }
    }

    // -----------------------------------------------------------------------------------------
    // §12: "segurar para rolar" (o pedido do usuário de 14/09)
    // -----------------------------------------------------------------------------------------

    /// Um controle e um prompter cuja tela entende o "segurar", em sessão, já trocando estado.
    fn par_que_segura(t0: u64) -> (Replica, Replica) {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        p.ligar_segurar();
        sessao(&mut c, &mut p, 1, em(t0, 1_000));
        cruzar(&mut c, &mut p, em(t0, 1_000), 2);
        assert!(c.estado_em(em(0, 0)).par_entende_segurar, "o controle não viu que o prompter entende");
        (c, p)
    }

    fn estados_em(ms: &[String]) -> usize {
        ms.iter().filter(|m| m.contains("\"tipo\":\"estado\"")).count()
    }

    /// **O gesto** (§12): apertar é `rolando`, `para_tras` e `segurando` numa mensagem só; soltar é
    /// outra. E no canal sem ordem, o soltar que chega antes do apertar ganha pelo carimbo: o texto
    /// termina parado.
    #[test]
    fn segurar_aperta_numa_mensagem_so_e_soltar_para() {
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(true, em(11_000, 2_000)).unwrap();
        let aperta = mensagens(&mut c, em(11_000, 2_000));
        assert_eq!((aperta.len(), estados_em(&aperta)), (1, 1), "apertar saiu em mais de uma mensagem: {aperta:?}");
        for chave in [r#""rolando":{"valor":true"#, r#""para_tras":{"valor":true"#, r#""segurando":{"valor":true"#] {
            assert!(aperta[0].contains(chave), "{chave} fora do aperto: {}", aperta[0]);
        }
        let m = aperta.iter().fold(0, |acc, x| acc | p.receber_em(x, em(11_010, 2_010)));
        assert_ne!(m & mudou::ROLANDO, 0);
        assert_ne!(m & mudou::SEGURAR, 0);
        let e = p.estado_em(em(0, 0));
        assert!(e.rolando && e.para_tras && e.segurando, "{e:?}");

        c.soltar_em(em(11_500, 2_500)).unwrap();
        let solta = mensagens(&mut c, em(11_500, 2_500));
        assert_eq!((solta.len(), estados_em(&solta)), (1, 1));
        for m in &solta {
            p.receber_em(m, em(11_510, 2_510));
        }
        let e = p.estado_em(em(0, 0));
        assert!(!e.rolando && !e.para_tras && !e.segurando, "soltou e o texto não parou: {e:?}");
        // Soltar sem nada seguro não mexe em nada — nem num play normal.
        c.definir_rolando_em(true, em(11_600, 2_600)).unwrap();
        let carimbo = c.rolando.carimbo;
        c.soltar_em(em(11_700, 2_700)).unwrap();
        assert!(c.rolando.valor && c.rolando.carimbo == carimbo, "soltar sem segurar parou o play normal");

        // Fora de ordem: o soltar chega antes do apertar. O carimbo decide: parado.
        let (mut c, _) = par_que_segura(20_000);
        let mut p2 = nova("prompter", P);
        c.segurar_em(false, em(21_000, 2_000)).unwrap();
        let aperta = mensagens(&mut c, em(21_000, 2_000));
        c.soltar_em(em(21_100, 2_100)).unwrap();
        let solta = mensagens(&mut c, em(21_100, 2_100));
        for m in solta.iter().chain(aperta.iter()) {
            p2.receber_em(m, em(21_200, 2_200));
        }
        let e = p2.estado_em(em(0, 0));
        assert!(!e.rolando && !e.segurando, "o apertar atrasado religou o texto: {e:?}");
    }

    /// Para trás, na velocidade de sempre; e uma queda que **só o controle** percebeu (§12.4, reescrito
    /// com o achado 1 de 14/09): ele para o texto **só na réplica dele** — carimbo 0, nada vai ao fio —,
    /// e o prompter, que ainda rolava para trás, para sozinho pelo silêncio de `PAR_SUMIDO`. Na volta,
    /// o controle adota a parada do prompter; o play normal depois vai para a frente.
    #[test]
    fn a_queda_que_so_o_controle_viu_para_o_prompter_pelo_silencio_dele() {
        let (mut c, mut p) = par_que_segura(10_000);
        c.definir_velocidade_em(4.0, em(10_500, 1_500)).unwrap();
        c.segurar_em(true, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
        assert!(p.para_tras.valor && p.rolando.valor && p.velocidade.valor == 4.0);
        let m = c.perdeu_o_par_em(em(12_000, 3_000));
        assert_ne!(m & mudou::ROLANDO, 0, "a tela do controle não soube que parou");
        assert!(!c.rolando.valor && !c.segurando.valor && !c.para_tras.valor);
        assert_eq!(c.rolando.carimbo, 0, "a parada do controle foi carimbada: viajaria para a sessão seguinte");
        // O prompter ainda não percebeu a queda; o silêncio o para — 2,6 s depois da última mensagem
        // que ouviu (em 2 000 no relógio monotônico dele).
        assert!(p.segurando.valor);
        assert_ne!(p.vigiar_o_segurar_em(em(13_000, 4_600)) & mudou::ROLANDO, 0);
        assert!(!p.rolando.valor && !p.segurando.valor && !p.para_tras.valor, "o prompter não parou sozinho");
        sessao(&mut c, &mut p, 2, em(14_000, 5_000));
        let primeiras = mensagens(&mut c, em(14_000, 5_000));
        assert!(
            primeiras.iter().all(|m| !m.contains("segurando") && !m.contains("para_tras")),
            "a parada do controle saiu na sessão nova: {primeiras:?}"
        );
        for m in &primeiras {
            p.receber_em(m, em(14_000, 5_000));
        }
        cruzar(&mut c, &mut p, em(14_000, 5_000), 2);
        assert!(!c.rolando.valor && !c.segurando.valor, "o controle não adotou a parada do prompter");
        c.definir_rolando_em(true, em(15_000, 6_000)).unwrap();
        cruzar(&mut c, &mut p, em(15_000, 6_000), 2);
        assert!(p.rolando.valor && !p.para_tras.valor, "o play normal rolou para trás");
        igual(&c, &p);
    }

    /// **Achado 1 de 14/09, cenário A**: o controle segura em P1, e P1 fecha a sessão. O controle
    /// entra em P2, que já rolava por um play — e P2 segue rolando: a parada da queda era de P1.
    #[test]
    fn a_parada_da_queda_nao_para_o_proximo_prompter() {
        let (mut c, mut p1) = par_que_segura(10_000);
        c.segurar_em(false, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p1, em(11_000, 2_000), 2);
        p1.perdeu_o_par_em(em(12_000, 3_000));
        c.perdeu_o_par_em(em(12_000, 3_000));
        assert!(!p1.rolando.valor && !c.rolando.valor, "a queda segurando não parou");
        // P2 rola por um play dado nele antes da queda (o carimbo dele é mais velho que ela).
        let mut p2 = nova("prompter-2", P);
        p2.ligar_segurar();
        p2.definir_rolando_em(true, em(11_500, 500)).unwrap();
        sessao(&mut c, &mut p2, 2, em(13_000, 4_000));
        cruzar(&mut c, &mut p2, em(13_000, 4_000), 2);
        assert!(p2.rolando.valor, "a parada da queda em P1 parou P2");
        assert!(c.rolando.valor, "o controle não adotou o play de P2");
        igual(&c, &p2);
    }

    /// **Achado 1 de 14/09, cenário B**: o controle, com o relógio ~110 s adiantado, cai segurando. O
    /// prompter para sozinho; 20 s depois alguém dá play nele — e a volta do controle não desfaz o
    /// play (a parada carimbada do controle, na hora adiantada dele, venceria).
    #[test]
    fn a_parada_da_queda_nao_desfaz_o_play_dado_no_prompter() {
        const ADIANTADO: u64 = 110_000;
        const T: u64 = 1_000_000;
        // Cada troca com a hora de cada lado: o controle vê `T + ADIANTADO + t`, o prompter, `T + t`.
        let trocar_as_horas = |c: &mut Replica, p: &mut Replica, t: u64, mono: u64| {
            for _ in 0..2 {
                trocar(c, p, em(T + ADIANTADO + t, mono), em(T + t, mono));
                trocar(p, c, em(T + t, mono), em(T + ADIANTADO + t, mono));
            }
        };
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        p.ligar_segurar();
        sessao(&mut c, &mut p, 1, em(T, 1_000));
        trocar_as_horas(&mut c, &mut p, 0, 1_000);
        c.segurar_em(false, em(T + ADIANTADO + 1_000, 2_000)).unwrap();
        trocar_as_horas(&mut c, &mut p, 1_000, 2_000);
        assert!(p.rolando.valor && p.segurando.valor);
        c.perdeu_o_par_em(em(T + ADIANTADO + 2_000, 3_000));
        p.perdeu_o_par_em(em(T + 2_000, 3_000));
        assert!(!p.rolando.valor, "o prompter não parou sozinho");
        p.definir_rolando_em(true, em(T + 22_000, 23_000)).unwrap();
        sessao(&mut c, &mut p, 2, em(T + 30_000, 30_000));
        trocar_as_horas(&mut c, &mut p, 30_000, 30_000);
        assert!(p.rolando.valor, "a volta do controle desfez o play dado no prompter");
        assert!(c.rolando.valor);
        igual(&c, &p);
    }

    /// **O que o segurar do controle escreveu e não saiu não passa da sessão** (§12.4): o soltar no
    /// instante da queda (o canal já fechado o recusa) e a parada do silêncio numa bombeada que não
    /// chegou a mandar. Nenhum dos dois para o prompter seguinte — com `perdeu_o_par` ou sem ele.
    #[test]
    fn o_soltar_e_a_parada_que_nao_sairam_nao_passam_da_sessao() {
        for (qual, chama_perdeu) in [("soltar", true), ("soltar", false), ("silêncio", true), ("silêncio", false)] {
            let (mut c, mut p1) = par_que_segura(10_000);
            c.segurar_em(false, em(11_000, 2_000)).unwrap();
            cruzar(&mut c, &mut p1, em(11_000, 2_000), 2);
            if qual == "soltar" {
                c.soltar_em(em(11_900, 2_900)).unwrap();
            } else {
                assert_ne!(c.vigiar_o_segurar_em(em(11_900, 4_600)) & mudou::ROLANDO, 0);
            }
            assert!(!c.rolando.valor && c.rolando.carimbo > c.relogio_da_ultima_troca, "{qual}: devia estar por sair");
            if chama_perdeu {
                c.perdeu_o_par_em(em(12_000, 5_000));
            }
            let mut p2 = nova("prompter-2", P);
            p2.ligar_segurar();
            p2.definir_rolando_em(true, em(11_500, 500)).unwrap();
            sessao(&mut c, &mut p2, 2, em(13_000, 6_000));
            cruzar(&mut c, &mut p2, em(13_000, 6_000), 2);
            assert!(p2.rolando.valor, "{qual} (perdeu_o_par: {chama_perdeu}): o que não saiu parou o prompter seguinte");
            igual(&c, &p2);
        }
    }

    /// **A casca de prompter que troca de sessão sem `perdeu_o_par`** (§12.4, pedido do coordenador
    /// de 14/09). O prompter perdeu a sessão com o dedo no botão e começa outra ainda segurando; o
    /// primeiro estado do controle chega antes de qualquer volta do silêncio. A sessão nova o tira do
    /// segurar, só na réplica dele — antes, ele seguia rolando, e o controle adotava o `segurando`
    /// dele. E a parada não viaja: um play dado no controle durante a queda continua valendo.
    #[test]
    fn o_prompter_que_troca_de_sessao_segurando_para_sem_viajar() {
        // A proteção: para, e para antes do primeiro estado da sessão nova.
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(true, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
        assert!(p.rolando.valor && p.para_tras.valor && p.segurando.valor);
        c.perdeu_o_par_em(em(12_000, 3_000));
        // A casca do prompter não chama `perdeu_o_par`, e não bombeia entre as sessões.
        sessao(&mut c, &mut p, 2, em(13_000, 4_000));
        assert!(!p.rolando.valor && !p.segurando.valor && !p.para_tras.valor, "o prompter começou a sessão nova segurando");
        assert_eq!(p.rolando.carimbo, 0, "a parada do prompter foi carimbada: viajaria para o controle");
        let bits = std::mem::take(&mut p.mudancas_pendentes);
        assert_eq!(bits & (mudou::ROLANDO | mudou::SEGURAR), mudou::ROLANDO | mudou::SEGURAR, "a tela do prompter não soube");
        for m in mensagens(&mut c, em(13_000, 4_000)) {
            p.receber_em(&m, em(13_000, 4_000));
        }
        assert_eq!(p.vigiar_o_segurar_em(em(13_000, 4_000)), 0);
        let do_prompter = mensagens(&mut p, em(13_000, 4_000));
        assert!(do_prompter.iter().all(|m| !m.contains("segurando") && !m.contains("para_tras")), "{do_prompter:?}");
        for m in &do_prompter {
            c.receber_em(m, em(13_000, 4_000));
        }
        cruzar(&mut c, &mut p, em(13_000, 4_000), 2);
        assert!(!c.segurando.valor && !c.rolando.valor, "o controle adotou um segurar da sessão anterior");
        igual(&c, &p);

        // Sem viajar: o play dado no controle durante a queda (12 500) vence a saída do segurar, que
        // é da sessão nova (13 000) — carimbada, ela o desfaria.
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(true, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
        c.perdeu_o_par_em(em(12_000, 3_000));
        c.definir_rolando_em(true, em(12_500, 3_500)).unwrap();
        sessao(&mut c, &mut p, 2, em(13_000, 4_000));
        cruzar(&mut c, &mut p, em(13_000, 4_000), 2);
        assert!(p.rolando.valor && !p.para_tras.valor && !p.segurando.valor, "a saída do segurar desfez o play dado no controle na queda");
        igual(&c, &p);
    }

    /// **O reenvio rápido do grupo do segurar** (§12.3, pedido do coordenador de 14/09): o soltar que
    /// se perde sai de novo em +50, +150 e +350 ms, e só isso; com a confirmação, para; a parada do
    /// silêncio também agenda; uma mudança fora do grupo (a velocidade) não agenda nada; e a bombeada
    /// espera só até o próximo reenvio.
    #[test]
    fn o_grupo_do_segurar_sai_de_novo_ate_a_confirmacao() {
        let saidas_depois = |c: &mut Replica, de: u64, ate: u64| -> Vec<u64> {
            (de..=ate)
                .step_by(5)
                .filter(|&t| !mensagens(c, em(10_000 + t, t)).is_empty())
                .collect()
        };
        // Sem confirmação nenhuma: três reenvios, nos instantes, e mais nada até o batimento.
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(false, em(12_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(12_000, 2_000), 2);
        c.soltar_em(em(12_500, 2_500)).unwrap();
        assert_eq!(estados_em(&mensagens(&mut c, em(12_500, 2_500))), 1, "o soltar não saiu na hora");
        assert_eq!(
            c.espera_da_bombeada(em(12_510, 2_510), Duration::from_millis(100)),
            Duration::from_millis(40),
            "a bombeada não espera só até o reenvio"
        );
        assert_eq!(
            c.espera_da_bombeada(em(12_600, 2_600), Duration::from_millis(100)),
            Duration::from_millis(100),
            "um reenvio vencido e não mandado zerou a espera: a bombeada giraria sem parar com o canal fechado"
        );
        assert_eq!(saidas_depois(&mut c, 2_505, 3_400), vec![2_550, 2_650, 2_850], "os reenvios do soltar");
        assert_eq!(c.espera_da_bombeada(em(13_500, 3_500), Duration::from_millis(100)), Duration::from_millis(100));

        // Com a confirmação depois do primeiro reenvio, para.
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(true, em(12_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(12_000, 2_000), 2);
        c.soltar_em(em(12_500, 2_500)).unwrap();
        let _perdido = mensagens(&mut c, em(12_500, 2_500));
        let reenvio = mensagens(&mut c, em(12_550, 2_550));
        assert_eq!(estados_em(&reenvio), 1);
        for m in &reenvio {
            p.receber_em(m, em(12_560, 2_560));
        }
        assert!(!p.rolando.valor && !p.segurando.valor, "o reenvio não parou o prompter");
        for m in mensagens(&mut p, em(12_560, 2_560)) {
            c.receber_em(&m, em(12_570, 2_570));
        }
        assert_eq!(saidas_depois(&mut c, 2_575, 3_400), Vec::<u64>::new(), "reenviou depois da confirmação");

        // Fora do grupo, nada: a velocidade sai uma vez, e o resto espera o batimento.
        c.definir_velocidade_em(3.0, em(13_000, 3_000)).unwrap();
        assert_eq!(mensagens(&mut c, em(13_000, 3_000)).len(), 1);
        assert_eq!(saidas_depois(&mut c, 3_005, 3_900), Vec::<u64>::new(), "a velocidade agendou reenvio");

        // A parada do silêncio agenda também.
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(false, em(12_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(12_000, 2_000), 1);
        assert_ne!(c.vigiar_o_segurar_em(em(14_600, 4_600)) & mudou::ROLANDO, 0);
        let _parada = mensagens(&mut c, em(14_600, 4_600));
        assert_eq!(saidas_depois(&mut c, 4_605, 5_000), vec![4_650, 4_750, 4_950], "os reenvios da parada do silêncio");
    }

    /// Uma volta de "soltou → prompter parado" pelo canal simulado (§12.3). Cada transmissão leva de 5
    /// a 40 ms e se perde com `perda` %; perdida, `retransmite` a repete como o SCTP — depois do RTO
    /// mínimo do libdatachannel (200 ms), dobrando a cada perda —, e sem `retransmite` ela some e só o
    /// batimento repara. As réplicas bombeiam a cada 5 ms. O controle aperta; 500 ms depois de o
    /// prompter estar rolando, solta; devolve os ms do soltar até o prompter parar. `com_reenvio`
    /// falso tira o reenvio rápido (é o código de antes).
    fn soltou_ate_parar(semente: u64, perda: u64, retransmite: bool, com_reenvio: bool) -> u64 {
        let mut dado = Dado(semente.wrapping_mul(2_654_435_761).wrapping_add(12_345));
        let (mut c, mut p) = par_que_segura(1_000_000);
        // (quando chega, é para o prompter, a mensagem)
        let mut fila: Vec<(u64, bool, String)> = Vec::new();
        let mut transmitir = |fila: &mut Vec<(u64, bool, String)>, t: u64, para_p: bool, m: String| {
            let mut chega = t + 5 + dado.prox() % 36;
            let mut rto = 200;
            while dado.chance(perda) {
                if !retransmite {
                    return;
                }
                chega += rto;
                rto *= 2;
            }
            fila.push((chega, para_p, m));
        };
        let mut t = 1_000u64;
        let mut apertou = false;
        let mut rolando_desde = None;
        let mut soltou_em = None;
        loop {
            t += 5;
            assert!(t < 120_000, "semente {semente}: a volta não terminou");
            let agora = em(1_000_000 + t, t);
            let (chegam, ficam): (Vec<_>, Vec<_>) = fila.into_iter().partition(|(q, _, _)| *q <= t);
            fila = ficam;
            for (_, para_p, m) in chegam {
                if para_p {
                    p.receber_em(&m, agora);
                } else {
                    c.receber_em(&m, agora);
                }
            }
            if let Some(s) = soltou_em {
                if !p.rolando.valor || t > s + 10_000 {
                    return t - s;
                }
            } else if !apertou {
                c.segurar_em(false, agora).unwrap();
                apertou = true;
            } else if p.rolando.valor && p.segurando.valor && t >= *rolando_desde.get_or_insert(t) + 500 {
                c.soltar_em(agora).unwrap();
                soltou_em = Some(t);
            }
            if !com_reenvio {
                c.envio.reenvio_do_segurar = None;
            }
            for m in mensagens(&mut c, agora) {
                transmitir(&mut fila, t, true, m);
            }
            for m in mensagens(&mut p, agora) {
                transmitir(&mut fila, t, false, m);
            }
        }
    }

    /// p50, p95, p99 e máximo de `n` voltas de [`soltou_ate_parar`].
    fn cauda_do_soltar(n: u64, perda: u64, retransmite: bool, com_reenvio: bool) -> [u64; 4] {
        let mut v: Vec<u64> = (1..=n).map(|s| soltou_ate_parar(s, perda, retransmite, com_reenvio)).collect();
        v.sort_unstable();
        let q = |f: usize| v[(v.len() * f / 100).min(v.len() - 1)];
        [q(50), q(95), q(99), v[v.len() - 1]]
    }

    /// **O soltar que se perde chega pelo reenvio rápido** (§12.3): no canal simulado perdendo 20 e
    /// 30 %, com e sem a retransmissão do SCTP, o p95 do "soltou → prompter parado" com o reenvio fica
    /// abaixo de 250 ms — sem ele, ia à retransmissão (≥ 200 ms, dobrando) ou ao batimento (1 s) —, e o
    /// p99 e o máximo encurtam. 300 voltas por caso; a tabela da §12.3, com 2 000, é
    /// `tabela_do_soltou_ate_parar`.
    #[test]
    fn o_soltar_que_se_perde_chega_pelo_reenvio_rapido() {
        for perda in [20, 30] {
            for retransmite in [true, false] {
                let antes = cauda_do_soltar(300, perda, retransmite, false);
                let depois = cauda_do_soltar(300, perda, retransmite, true);
                let caso = format!(
                    "perda {perda} %, retransmissão {retransmite}: antes {antes:?}, depois {depois:?} (p50, p95, p99, máx)"
                );
                assert!(depois[1] < 250, "o p95 com o reenvio passou de 250 ms — {caso}");
                assert!(depois[2] < antes[2], "o reenvio não encurtou o p99 — {caso}");
                assert!(depois[3] <= antes[3], "o reenvio piorou o máximo — {caso}");
            }
        }
    }

    /// A tabela da §12.3, com 2 000 voltas por caso: `cargo test -p quall-core --lib
    /// tabela_do_soltou_ate_parar -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn tabela_do_soltou_ate_parar() {
        println!("| perda | retransmissão do SCTP | antes: p50 / p95 / p99 / máx (ms) | com o reenvio: p50 / p95 / p99 / máx (ms) |");
        for perda in [20, 30] {
            for retransmite in [true, false] {
                let [a50, a95, a99, amax] = cauda_do_soltar(2_000, perda, retransmite, false);
                let [d50, d95, d99, dmax] = cauda_do_soltar(2_000, perda, retransmite, true);
                println!(
                    "| {perda} % | {} | {a50} / {a95} / {a99} / {amax} | {d50} / {d95} / {d99} / {dmax} |",
                    if retransmite { "sim" } else { "não" }
                );
            }
        }
    }

    /// **O grupo do segurar ganha ou perde inteiro** (achado 2 de 14/09): a pausa no prompter no mesmo
    /// milissegundo, ou quase, de um novo aperto em "Rolar para cima". Com carimbos seguidos, o aperto
    /// 1 ms depois empatava só em `para_tras`, o `device_id` maior ("prompter") vencia, e o texto
    /// terminava rolando **para a frente** com o dedo em "para trás". Empate forçado, de -3 a +3 ms.
    #[test]
    fn o_grupo_do_segurar_ganha_ou_perde_inteiro() {
        const X: u64 = 20_000;
        for d in -3i64..=3 {
            let (mut c, mut p) = par_que_segura(10_000);
            // Um segurar para trás, que o prompter recebe; e o soltar, que se perde no caminho.
            c.segurar_em(true, em(11_000, 2_000)).unwrap();
            cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
            c.soltar_em(em(11_500, 2_500)).unwrap();
            let _perdido = mensagens(&mut c, em(11_500, 2_500));
            // A pausa no prompter, que ainda rola para trás, em X; o aperto para trás de novo em X + d.
            p.definir_rolando_em(false, em(X, 3_000)).unwrap();
            let x_mais_d = u64::try_from(i64::try_from(X).unwrap() + d).unwrap();
            c.segurar_em(true, em(x_mais_d, 3_000)).unwrap();
            cruzar(&mut c, &mut p, em(X + 10, 3_010), 2);
            igual(&c, &p);
            let e = p.estado_em(em(0, 0));
            let grupo = (e.rolando, e.segurando, e.para_tras);
            assert!(
                grupo == (false, false, false) || grupo == (true, true, true),
                "d = {d}: o grupo se partiu (rolando, segurando, para_tras) = {grupo:?}"
            );
        }
    }

    /// **Com um prompter de 13/09, o segurar não acende "o comando não chegou"** (achado 3 de 14/09):
    /// o controle tem `para_tras` e `segurando` escritos e ainda por sair — uma pausa dada sem sessão,
    /// depois de segurar num prompter novo — e entra num prompter de 13/09, que nunca os devolve. Sem
    /// `"entende_segurar"`, eles não contam na confirmação; `rolando`, que ele devolve, conta.
    #[test]
    fn com_o_prompter_de_13_09_o_segurar_nao_acende_o_aviso() {
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(false, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
        c.soltar_em(em(11_500, 2_500)).unwrap();
        c.definir_rolando_em(true, em(11_600, 2_600)).unwrap();
        cruzar(&mut c, &mut p, em(11_600, 2_600), 2);
        c.perdeu_o_par_em(em(12_000, 3_000));
        c.definir_rolando_em(false, em(12_500, 3_500)).unwrap();
        assert!(c.segurando.carimbo > c.relogio_da_ultima_troca, "a pausa sem sessão devia levar o grupo");
        c.nova_sessao_com_par(2, Some(&ParDaSessao { id: "ipad-13-09".into(), nome: "iPad".into() }), em(13_000, 4_000));
        let saiu = mensagens(&mut c, em(13_000, 4_000));
        assert!(saiu.iter().any(|m| m.contains("\"segurando\"")), "o teste não exercita o caso: {saiu:?}");
        // O prompter de 13/09 adota a pausa e a devolve — sem as chaves novas, que ele não conhece.
        let q = c.rolando.carimbo;
        let eco = format!(
            r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":{q},"rolando":{{"valor":false,"carimbo":{q},"autor":"controle"}},"texto":{{"carimbo":0,"autor":"","bytes":0,"resumo":"e3b0c44298fc1c14"}}}}"#
        );
        c.receber_em(&eco, em(13_010, 4_010));
        assert_eq!(
            c.estado_em(em(13_010, 9_000)).sem_confirmacao_ha_ms,
            None,
            "\"o comando não chegou\" aceso com o prompter de 13/09 tendo devolvido a pausa"
        );
    }

    /// **O play com o texto já rolando não tira do segurar** (achado 4 de 14/09): segurando, um
    /// `definir_rolando(true)` não muda nada — soltar para, e a queda para. E a pausa sempre para e sai
    /// do segurar, mesmo com `rolando` já parado (um grupo que chegou pela metade).
    #[test]
    fn o_play_com_o_texto_rolando_nao_tira_do_segurar() {
        // O play no meio do segurar, e depois soltar.
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(true, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
        let antes = (c.rolando.clone(), c.para_tras.clone(), c.segurando.clone());
        c.definir_rolando_em(true, em(11_200, 2_200)).unwrap();
        assert_eq!((c.rolando.clone(), c.para_tras.clone(), c.segurando.clone()), antes, "o play mexeu no segurar");
        c.soltar_em(em(11_500, 2_500)).unwrap();
        cruzar(&mut c, &mut p, em(11_500, 2_500), 2);
        let e = p.estado_em(em(0, 0));
        assert!(!e.rolando && !e.segurando && !e.para_tras, "o play virou play livre e soltar não parou: {e:?}");
        // O play no meio do segurar, e depois a queda.
        let (mut c, mut p) = par_que_segura(20_000);
        c.segurar_em(false, em(21_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(21_000, 2_000), 2);
        c.definir_rolando_em(true, em(21_200, 2_200)).unwrap();
        p.definir_rolando_em(true, em(21_200, 2_200)).unwrap();
        cruzar(&mut c, &mut p, em(21_200, 2_200), 2);
        assert!(p.segurando.valor && c.segurando.valor);
        p.perdeu_o_par_em(em(22_000, 3_000));
        c.perdeu_o_par_em(em(22_000, 3_000));
        assert!(!p.rolando.valor && !c.rolando.valor, "o play no meio do segurar deixou o texto rolando na queda");
        // A pausa com `rolando` já parado e o dedo ainda marcado: um lado que escreveu só `rolando`.
        let (mut c, mut p) = par_que_segura(30_000);
        c.segurar_em(true, em(31_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(31_000, 2_000), 2);
        let so_rolando = format!(
            r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":31500,"rolando":{{"valor":false,"carimbo":31500,"autor":"prompter"}},"entende_segurar":true,"texto":{{"carimbo":0,"autor":"","bytes":0,"resumo":"{}"}}}}"#,
            resumo("")
        );
        c.receber_em(&so_rolando, em(31_500, 2_500));
        assert!(!c.rolando.valor && c.segurando.valor && c.para_tras.valor, "o teste não montou o grupo partido");
        c.definir_rolando_em(false, em(31_600, 2_600)).unwrap();
        assert!(!c.segurando.valor && !c.para_tras.valor, "a pausa não saiu do segurar");
        assert!(c.rolando.carimbo == c.segurando.carimbo && c.segurando.carimbo == c.para_tras.carimbo);
    }

    /// **O play e a pausa de quem nunca segurou são o fio de antes** (§12.1): com o grupo de um carimbo
    /// só (achado 2), o play e a pausa só levam `para_tras` e `segurando` quando o segurar existe aqui.
    /// Um controle e um prompter que nunca o usaram — o prompter sem ligar o segurar — dão play e pausa
    /// dos dois lados, e nenhuma mensagem leva as chaves novas.
    #[test]
    fn o_play_e_a_pausa_de_quem_nunca_segurou_sao_o_fio_de_antes() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        sessao(&mut c, &mut p, 1, em(10_000, 1_000));
        let mut fio = Vec::new();
        for (k, (quem_c, rolando)) in [(true, true), (false, false), (false, true), (true, false)].into_iter().enumerate() {
            let t = 10_000 + 100 * k as u64;
            let r = if quem_c { &mut c } else { &mut p };
            r.definir_rolando_em(rolando, em(t, t)).unwrap();
            let (mc, mp) = (mensagens(&mut c, em(t, t)), mensagens(&mut p, em(t, t)));
            for m in &mc {
                p.receber_em(m, em(t, t));
            }
            for m in &mp {
                c.receber_em(m, em(t, t));
            }
            fio.extend(mc.into_iter().chain(mp));
        }
        assert!(fio.len() >= 4, "o teste não trocou mensagens: {fio:?}");
        assert!(
            fio.iter().all(|m| !m.contains("para_tras") && !m.contains("segurando") && !m.contains("entende_segurar")),
            "o play ou a pausa puseram o segurar no fio de quem nunca segurou: {fio:?}"
        );
        igual(&c, &p);
    }

    /// **Achado 5 de 14/09**: entre o fim da sessão e o `perdeu_o_par`, o aperto era aceito — e numa
    /// casca que não chamasse `perdeu_o_par`, ia para o prompter seguinte, mesmo um que não entende.
    /// Agora a bombeada que diz `fechada` fecha o segurar: `Closed` até a sessão seguinte. Por uma
    /// sessão de verdade (127.0.0.1).
    #[test]
    fn segurar_depois_de_a_sessao_acabar_e_recusado() {
        let (sessao_p, sessao_c) = sessoes("fim-do-segurar", "757575");
        let tp = Teleprompter::nova("prompter-fim-do-segurar", P).unwrap();
        let tc = Teleprompter::nova("controle-fim-do-segurar", C).unwrap();
        tp.ligar_segurar().unwrap();
        let (mp, mc) = (sessao_p.session.mensageiro(), sessao_c.session.mensageiro());
        let fim = std::time::Instant::now() + Duration::from_secs(10);
        while !tc.estado().unwrap().par_entende_segurar && std::time::Instant::now() < fim {
            let _ = tc.bombear(&mc, Duration::from_millis(10));
            let _ = tp.bombear(&mp, Duration::from_millis(10));
        }
        assert!(tc.estado().unwrap().par_entende_segurar, "o controle não viu o prompter dizer que entende");
        tc.segurar(false).unwrap();
        tc.soltar().unwrap();
        // O prompter some; a bombeada do controle diz que a sessão acabou.
        drop(sessao_p);
        let fim = std::time::Instant::now() + Duration::from_secs(10);
        let mut fechou = false;
        while !fechou && std::time::Instant::now() < fim {
            fechou = tc.bombear(&mc, Duration::from_millis(20)).unwrap().fechada;
        }
        assert!(fechou, "a bombeada não disse que a sessão acabou");
        // A casca ainda não chamou `perdeu_o_par`: o aperto é recusado, e nada fica seguro.
        assert!(matches!(tc.segurar(true), Err(Error::Closed)), "segurou depois de a sessão acabar");
        assert!(!tc.estado().unwrap().segurando);
        drop(sessao_c);
    }

    /// **Rolar e parar pelos botões de sempre saem do modo segurar**: a pessoa no prompter pausa no
    /// meio de um segurar para trás; depois, o play normal do controle rola **para a frente**.
    #[test]
    fn o_play_normal_sai_do_segurar() {
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(true, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
        p.definir_rolando_em(false, em(11_500, 2_500)).unwrap();
        cruzar(&mut c, &mut p, em(11_500, 2_500), 2);
        assert!(!c.rolando.valor && !c.segurando.valor, "a pausa do prompter não tirou o controle do segurar");
        c.definir_rolando_em(true, em(12_000, 3_000)).unwrap();
        cruzar(&mut c, &mut p, em(12_000, 3_000), 2);
        assert!(p.rolando.valor && !p.para_tras.valor, "o play normal rolou para trás");
        igual(&c, &p);
    }

    /// **O que só um prompter que entende recebe**: um prompter de 13/09 ignora `para_tras` e
    /// rolaria para a frente quando pedem para trás — e na queda seguiria rolando. O controle só
    /// segura quando o último estado do prompter diz `"entende_segurar": true`, o que só a tela que
    /// rola para trás liga.
    #[test]
    fn segurar_exige_o_prompter_que_entende() {
        // Um prompter novo cuja tela não liga o segurar.
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        sessao(&mut c, &mut p, 1, em(10_000, 1_000));
        cruzar(&mut c, &mut p, em(10_000, 1_000), 2);
        assert!(matches!(c.segurar_em(true, em(10_100, 1_100)), Err(Error::Protocol(_))));
        assert!(mensagens(&mut c, em(10_100, 1_100)).iter().all(|m| !m.contains("segurando")));
        // Um prompter de 13/09, pelos bytes dele (o formato da §4, sem as chaves novas).
        let de_13_09 = r#"{"app":"teleprompter","v":1,"tipo":"estado","relogio":10000,"rolando":{"valor":false,"carimbo":0,"autor":""},"texto":{"carimbo":0,"autor":"","bytes":0,"resumo":"e3b0c44298fc1c14"}}"#;
        let mut c = nova("controle", C);
        c.nova_sessao_com_par(1, Some(&ParDaSessao { id: "ipad-13-09".into(), nome: "iPad".into() }), em(10_000, 1_000));
        c.receber_em(de_13_09, em(10_000, 1_000));
        assert!(matches!(c.segurar_em(false, em(10_100, 1_100)), Err(Error::Protocol(_))));
        // Sem sessão, e no prompter.
        let mut sem = nova("controle", C);
        assert!(matches!(sem.segurar_em(false, em(1, 0)), Err(Error::Closed)));
        assert!(matches!(nova("prompter", P).segurar_em(false, em(1, 0)), Err(Error::Invalid(_))));
        // O estado de quem liga diz `entende_segurar`; o de quem não liga é o de antes.
        let mut que_liga = nova("prompter", P);
        que_liga.ligar_segurar();
        assert!(mensagens(&mut que_liga, em(1, 0))[0].contains(r#""entende_segurar":true,"texto":"#));
        assert!(mensagens(&mut nova("prompter", P), em(1, 0)).iter().all(|m| !m.contains("segurar")));
    }

    /// **A queda com o dedo no botão para o texto** (§12, decisão do usuário), nos dois lados, com
    /// o bit `ROLANDO` avisando a tela. Sem o segurar, a regra de sempre: continua como está.
    #[test]
    fn a_queda_segurando_para_o_texto_nos_dois_lados() {
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(false, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 2);
        assert!(p.rolando.valor && p.segurando.valor);
        let m = p.perdeu_o_par_em(em(12_000, 3_000));
        assert_ne!(m & mudou::ROLANDO, 0, "a tela do prompter não soube que parou");
        assert!(!p.rolando.valor && !p.segurando.valor, "caiu segurando e o prompter seguiu rolando");
        let m = c.perdeu_o_par_em(em(12_000, 3_000));
        assert_ne!(m & mudou::ROLANDO, 0);
        assert!(!c.rolando.valor && !c.segurando.valor);
        // Sem o segurar (o play normal), a política de sempre: continua rolando.
        let (mut c, mut p) = par_que_segura(20_000);
        c.definir_rolando_em(true, em(21_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(21_000, 2_000), 2);
        p.perdeu_o_par_em(em(22_000, 3_000));
        assert!(p.rolando.valor, "o play normal parou na queda");
    }

    /// **O silêncio com o dedo no botão** (§12): sem esperar a sessão cair (5 s), o prompter para
    /// depois de `PAR_SUMIDO` sem ouvir o controle — e o controle, sem ouvir o prompter.
    #[test]
    fn o_silencio_segurando_para_o_texto() {
        let (mut c, mut p) = par_que_segura(10_000);
        c.segurar_em(false, em(11_000, 2_000)).unwrap();
        cruzar(&mut c, &mut p, em(11_000, 2_000), 1);
        assert!(p.segurando.valor);
        assert_eq!(p.vigiar_o_segurar_em(em(12_000, 2_000 + 2_400)), 0, "parou antes de PAR_SUMIDO");
        let m = p.vigiar_o_segurar_em(em(12_000, 2_000 + 2_600));
        assert_ne!(m & mudou::ROLANDO, 0);
        assert!(!p.rolando.valor && !p.segurando.valor);
        let m = c.vigiar_o_segurar_em(em(12_000, 2_000 + 2_600));
        assert_ne!(m & mudou::ROLANDO, 0, "o controle seguiu mostrando o texto rolando");
    }

    /// **Apertar e soltar pelo canal que perde, duplica e embaralha** (§12): o carimbo decide a
    /// ordem, e o texto termina parado nos dois lados, com as réplicas iguais. Vinte sementes.
    #[test]
    fn apertar_e_soltar_embaralhados_terminam_parados() {
        for semente in 1..=20u64 {
            let mut dado = Dado(semente * 104_729);
            let (mut c, mut p) = par_que_segura(1_000_000);
            let mut para_p: Vec<String> = Vec::new();
            let mut para_c: Vec<String> = Vec::new();
            let base = 1_000_000u64;
            for passo in 0..400u64 {
                let t = passo * 50;
                let agora = em(base + 1_000 + t, 1_000 + t);
                if passo < 300 && dado.chance(15) {
                    if c.segurando.valor {
                        c.soltar_em(agora).unwrap();
                    } else if c.estado_em(agora).par_entende_segurar {
                        c.segurar_em(dado.chance(50), agora).unwrap();
                    }
                }
                if passo == 300 {
                    c.soltar_em(agora).unwrap();
                }
                for (de, fila) in [(&mut p, &mut para_c), (&mut c, &mut para_p)] {
                    for m in mensagens(de, agora) {
                        if dado.chance(30) {
                            continue;
                        }
                        if dado.chance(10) {
                            fila.push(m.clone());
                        }
                        fila.push(m);
                    }
                }
                for (fila, r) in [(&mut para_p, &mut p), (&mut para_c, &mut c)] {
                    let n = fila.len() / 2 + usize::from(!fila.is_empty());
                    for _ in 0..n {
                        let i = (dado.prox() as usize) % fila.len();
                        let m = fila.swap_remove(i);
                        r.receber_em(&m, agora);
                    }
                }
            }
            for m in para_p.drain(..) {
                p.receber_em(&m, em(base + 30_000, 30_000));
            }
            for m in para_c.drain(..) {
                c.receber_em(&m, em(base + 30_000, 30_000));
            }
            for k in 0..4u64 {
                cruzar(&mut p, &mut c, em(base + 31_000 + k * 1_100, 31_000 + k * 1_100), 1);
            }
            let e = p.estado_em(em(0, 0));
            assert!(!e.rolando && !e.segurando && !e.para_tras, "semente {semente}: o texto não terminou parado: {e:?}");
            igual(&p, &c);
        }
    }

    /// **A trava da pergunta** (§11.10): sem ela — o padrão, enquanto as cascas não têm a tela da
    /// pergunta —, o controle que chega num prompter novo segue a regra de hoje, **byte a byte no
    /// fio**: as mesmas mensagens de uma sessão sem par conhecido. O que sobra é local: o texto que
    /// perde a fusão vai para as cópias.
    #[test]
    fn sem_a_trava_o_fio_e_o_de_hoje_byte_a_byte() {
        let rodar = |com_par: bool| {
            let mut c = nova("controle", C);
            let mut p = nova("prompter", P);
            p.definir_texto_em("roteiro do prompter", em(1_000, 0)).unwrap();
            c.definir_texto_em("roteiro do controle", em(2_000, 0)).unwrap();
            if com_par {
                sessao(&mut c, &mut p, 1, em(3_000, 1_000));
            } else {
                c.nova_sessao_com_par(1, None, em(3_000, 1_000));
                p.nova_sessao_com_par(1, None, em(3_000, 1_000));
            }
            let mut fio = Vec::new();
            for k in 0..5u64 {
                let agora = em(3_000 + k * 2_100, 1_000 + k * 2_100);
                let (mc, mp) = (mensagens(&mut c, agora), mensagens(&mut p, agora));
                for m in &mc {
                    p.receber_em(m, agora);
                }
                for m in &mp {
                    c.receber_em(m, agora);
                }
                fio.extend(mc);
                fio.extend(mp);
            }
            assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none(), "sem a trava, nunca há pergunta");
            (fio, c, p)
        };
        let (hoje, c_hoje, _) = rodar(false);
        let (com_par, c, p) = rodar(true);
        assert_eq!(com_par, hoje, "sem a trava, o fio mudou");
        assert_eq!(p.texto(), "roteiro do controle", "a regra de hoje: vale o último que mudou");
        assert!(copias(&c_hoje).is_empty(), "sem par conhecido, não há cópia (a regra de antes)");
        // O que sobra sem a pergunta: o roteiro do prompter, que perdeu a fusão, fica guardado.
        let cs = copias(&c);
        assert_eq!((cs.len(), cs[0].origem), (1, OrigemDaCopia::Prompter), "{cs:?}");
        assert_eq!(c.copia_do_texto(&cs[0].resumo), Some("roteiro do prompter"));
        assert_eq!(c.ultimo_prompter_id.as_deref(), Some("prompter"));
        // E o nosso, quando é ele que perde.
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        c.definir_texto_em("roteiro do controle", em(1_000, 0)).unwrap();
        p.definir_texto_em("roteiro do prompter", em(2_000, 0)).unwrap();
        sessao(&mut c, &mut p, 1, em(3_000, 1_000));
        cruzar(&mut c, &mut p, em(3_000, 1_000), 3);
        assert_eq!(c.texto(), "roteiro do prompter");
        assert_eq!(c.copia_do_texto(&resumo("roteiro do controle")), Some("roteiro do controle"));
        assert!(matches!(c.resolver_texto_em(true, "x", em(4_000, 2_000)), Err(Error::Invalid(_))));
    }

    /// **Achado B5**: o salvo único do Android. O aparelho foi controle (e guardou o prompter da
    /// última vez); virou prompter, e outro controle mudou o roteiro dele. Ao voltar a ser controle,
    /// ele não pode empurrar esse roteiro em silêncio para o prompter da última vez.
    #[test]
    fn o_prompter_que_recebe_texto_de_outro_esquece_o_prompter_da_ultima_vez() {
        let mut como_controle = nova("tablet", C);
        como_controle.ultimo_prompter_id = Some("iphone".into());
        como_controle.referencia_convergida =
            Some(ReferenciaDoTexto { carimbo: 0, autor: String::new(), bytes: 0, resumo: resumo("") });
        let salvo = como_controle.salvo_json();
        let mut como_prompter = Replica::de_salvo("tablet", P, &salvo).unwrap();
        assert!(como_prompter.salvo_json().contains("\"ultimo_prompter_id\":\"iphone\""), "o prompter não carregou o campo");
        let mut x = nova("controle-x", C);
        x.definir_texto_em("roteiro do controle x", em(20_000, 0)).unwrap();
        trocar(&mut x, &mut como_prompter, em(20_000, 0), em(20_000, 0));
        assert_eq!(como_prompter.texto(), "roteiro do controle x");
        let salvo = como_prompter.salvo_json();
        assert!(
            !salvo.contains("ultimo_prompter_id") && !salvo.contains("referencia_convergida"),
            "o prompter manteve o da última vez depois de outro autor mudar o texto: {salvo}"
        );
        let mut de_volta = Replica::de_salvo("tablet", C, &salvo).unwrap();
        de_volta.ligar_pergunta_do_texto();
        let mut iphone = nova("iphone", P);
        iphone.definir_texto_em("roteiro do iphone", em(1_000, 0)).unwrap();
        sessao(&mut de_volta, &mut iphone, 1, em(30_000, 1_000));
        cruzar(&mut de_volta, &mut iphone, em(30_000, 1_000), 3);
        assert_eq!(iphone.texto(), "roteiro do iphone", "o roteiro que o controle x deixou foi empurrado em silêncio");
        assert!(pergunta(&de_volta).aberta);
    }

    /// **Achado B6** (e o risco 2 do rascunho): C1 converge com P e edita fora da sessão; C2 chega
    /// em P e manda o dele; C1 volta a P. Sem pergunta — é o da última vez —, e o texto de C1 perde
    /// para o de C2, mais novo: fica guardado em C1. Um texto que era o convergido, e que o prompter
    /// trocou à vista, não vira cópia.
    #[test]
    fn no_reencontro_o_nosso_texto_que_mudou_e_perdeu_fica_guardado() {
        let mut c1 = nova("c1", C);
        let mut p = nova("prompter", P);
        c1.definir_texto_em("roteiro v1", em(10_000, 0)).unwrap();
        sessao(&mut c1, &mut p, 1, em(11_000, 1_000));
        cruzar(&mut c1, &mut p, em(11_000, 1_000), 4);
        assert_eq!((c1.ultimo_prompter_id.as_deref(), p.texto()), (Some("prompter"), "roteiro v1"));
        c1.perdeu_o_par_em(em(12_000, 2_000));
        c1.definir_texto_em("roteiro v1, corrigido fora da sessão", em(20_000, 10_000)).unwrap();

        let mut c2 = nova("c2", C);
        c2.ligar_pergunta_do_texto();
        c2.definir_texto_em("roteiro do c2", em(25_000, 0)).unwrap();
        sessao(&mut c2, &mut p, 2, em(26_000, 1_000));
        cruzar(&mut c2, &mut p, em(26_000, 1_000), 3);
        c2.resolver_texto_em(true, &resumo_do_prompter(&c2), em(26_100, 1_100)).unwrap();
        cruzar(&mut c2, &mut p, em(26_200, 1_200), 3);
        assert_eq!(p.texto(), "roteiro do c2");

        sessao(&mut c1, &mut p, 3, em(30_000, 20_000));
        cruzar(&mut c1, &mut p, em(30_000, 20_000), 4);
        assert!(c1.estado_em(em(0, 0)).pergunta_do_texto.is_none(), "o reencontro com o da última vez perguntou");
        assert_eq!(c1.texto(), "roteiro do c2");
        let cs = copias(&c1);
        assert_eq!(cs.len(), 1, "{cs:?}");
        assert_eq!(cs[0].origem, OrigemDaCopia::Controle);
        assert_eq!(c1.copia_do_texto(&cs[0].resumo), Some("roteiro v1, corrigido fora da sessão"), "o texto de C1 sumiu");

        // O caso sem cópia: o texto do controle é o convergido, e a pessoa no prompter o trocou.
        let mut c3 = nova("c3", C);
        let mut p3 = nova("p3", P);
        c3.definir_texto_em("roteiro", em(10_000, 0)).unwrap();
        sessao(&mut c3, &mut p3, 1, em(11_000, 1_000));
        cruzar(&mut c3, &mut p3, em(11_000, 1_000), 4);
        c3.perdeu_o_par_em(em(12_000, 2_000));
        p3.definir_texto_em("roteiro trocado no prompter", em(20_000, 10_000)).unwrap();
        sessao(&mut c3, &mut p3, 2, em(21_000, 11_000));
        cruzar(&mut c3, &mut p3, em(21_000, 11_000), 3);
        assert_eq!(c3.texto(), "roteiro trocado no prompter");
        assert!(copias(&c3).is_empty(), "guardou cópia de um texto que o prompter tinha e trocou à vista");
    }

    /// **Achado B7**: a cópia só existe na memória até a casca gravar. Toda cópia nova — a da
    /// escolha, a de uma chegada — acende o bit na bombeada seguinte; `perdeu_o_par` também o
    /// entrega.
    #[test]
    fn a_copia_nova_pede_para_gravar_o_salvo() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        assert_eq!(c.mudancas_pendentes & mudou::COPIA_DO_TEXTO, 0);
        c.resolver_texto_em(false, &resumo_do_prompter(&c), em(16_000, 3_000)).unwrap();
        assert_ne!(c.mudancas_pendentes & mudou::COPIA_DO_TEXTO, 0, "a cópia da escolha não pede para gravar");
        let m = c.perdeu_o_par_em(em(16_100, 3_100));
        assert_ne!(m & mudou::COPIA_DO_TEXTO, 0, "perdeu_o_par não entregou o bit da cópia");
        assert_eq!(c.mudancas_pendentes, 0, "o bit foi entregue duas vezes");
        p.definir_texto_em("outro roteiro do prompter", em(17_000, 4_000)).unwrap();
        // Uma cópia nascida de uma chegada: o controle muda o próprio texto e perde para o prompter.
        sessao(&mut c, &mut p, 2, em(18_000, 5_000));
        c.definir_texto_em("roteiro do controle, de novo", em(17_500, 4_500)).unwrap();
        cruzar(&mut c, &mut p, em(18_000, 5_000), 3);
        let _ = c.resolver_texto_em(false, &resumo_do_prompter(&c), em(18_100, 5_100));
        assert_ne!(c.mudancas_pendentes & mudou::COPIA_DO_TEXTO, 0);
    }

    /// **Achado B8**: "usar o do prompter" com P2, queda antes da convergência, e o prompter
    /// anterior P1 — que era o da última vez — desfaria a escolha no reencontro. Entrar num primeiro
    /// encontro apaga o da última vez: a volta a P1 é outro primeiro encontro, e pergunta.
    #[test]
    fn entrar_num_primeiro_encontro_apaga_o_prompter_da_ultima_vez() {
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p1 = nova("p1", P);
        let mut p2 = nova("p2", P);
        p2.definir_texto_em("roteiro do p2, escrito antes", em(5_000, 0)).unwrap();
        p1.definir_texto_em("roteiro do p1", em(8_000, 0)).unwrap();
        sessao(&mut c, &mut p1, 1, em(10_000, 1_000));
        cruzar(&mut c, &mut p1, em(10_000, 1_000), 3);
        assert_eq!(c.ultimo_prompter_id.as_deref(), Some("p1"));
        c.perdeu_o_par_em(em(11_000, 2_000));

        sessao(&mut c, &mut p2, 2, em(12_000, 3_000));
        assert_eq!(c.ultimo_prompter_id, None, "entrar num primeiro encontro não apagou o da última vez");
        cruzar(&mut c, &mut p2, em(12_000, 3_000), 2);
        c.resolver_texto_em(false, &resumo_do_prompter(&c), em(12_100, 3_100)).unwrap();
        c.perdeu_o_par_em(em(12_200, 3_200));
        assert_eq!(c.texto(), "roteiro do p2, escrito antes");

        sessao(&mut c, &mut p1, 3, em(13_000, 4_000));
        cruzar(&mut c, &mut p1, em(13_000, 4_000), 3);
        assert_eq!(c.texto(), "roteiro do p2, escrito antes", "o prompter anterior desfez a escolha em silêncio");
        assert!(pergunta(&c).aberta);
    }

    /// **Achado B9**: com o relógio do controle mais de 24 h adiantado, a pergunta abre, a
    /// desistência não é contada enquanto o texto está retido (ele não está saindo), "mandar o meu"
    /// sai com um carimbo que o prompter recusa, e a pergunta volta na conexão seguinte.
    #[test]
    fn com_o_texto_retido_a_desistencia_nao_conta_e_o_relogio_adiantado_pergunta_de_novo() {
        const T: u64 = 1_757_800_000_000;
        const ADIANTADO: u64 = 48 * 3_600_000;
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p = nova("prompter", P);
        p.definir_texto_em("roteiro do prompter", em(T, 0)).unwrap();
        c.definir_texto_em("roteiro do controle", em(T + ADIANTADO, 0)).unwrap();
        let rodar = |c: &mut Replica, p: &mut Replica, de: u64, voltas: u64| {
            for k in 0..voltas {
                let t = de + k * 2_100;
                for m in &mensagens(c, em(T + ADIANTADO + t, t)) {
                    p.receber_em(m, em(T + t, t));
                }
                for m in &mensagens(p, em(T + t, t)) {
                    c.receber_em(m, em(T + ADIANTADO + t, t));
                }
            }
        };
        sessao(&mut c, &mut p, 1, em(T + ADIANTADO + 1_000, 1_000));
        rodar(&mut c, &mut p, 1_000, 6);
        assert!(pergunta(&c).aberta, "com o relógio do controle adiantado, a pergunta tinha de abrir");
        assert_eq!(c.estado_em(em(0, 0)).contadores.reenvios_desistidos, 0, "desistência contada com o texto retido");
        c.resolver_texto_em(true, &resumo_do_prompter(&c), em(T + ADIANTADO + 20_000, 20_000)).unwrap();
        rodar(&mut c, &mut p, 21_000, 4);
        assert_eq!(p.texto(), "roteiro do prompter", "o prompter aceitou um carimbo do futuro");
        assert!(p.estado_em(em(0, 0)).contadores.carimbos_do_futuro > 0);
        assert_eq!(c.ultimo_prompter_id, None, "sem convergência, não há prompter da última vez");
        c.perdeu_o_par_em(em(T + ADIANTADO + 40_000, 40_000));
        sessao(&mut c, &mut p, 2, em(T + ADIANTADO + 41_000, 41_000));
        rodar(&mut c, &mut p, 41_000, 3);
        assert!(pergunta(&c).aberta, "a pergunta devia voltar na conexão seguinte");
    }

    /// **Achado B10**: vazio no meio da pergunta fecha a pergunta **pela regra de hoje** — vale o
    /// último que mudou, o vazio — e guarda o outro texto como cópia. Dos dois lados.
    #[test]
    fn vazio_no_meio_da_pergunta_fecha_pela_regra_de_hoje_e_guarda_o_outro() {
        // (a) A pessoa no prompter apaga o roteiro com a pergunta aberta.
        let (mut c, mut p) = pergunta_aberta(10_000);
        p.definir_texto_em("", em(20_000, 3_000)).unwrap();
        let m = trocar(&mut p, &mut c, em(20_000, 3_000), em(20_000, 3_000));
        assert_ne!(m & mudou::PERGUNTA_DO_TEXTO, 0);
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none(), "o vazio não fechou a pergunta");
        assert_eq!(c.texto(), "", "o vazio mais novo não venceu");
        assert_eq!(c.copia_do_texto(&resumo("roteiro do controle")), Some("roteiro do controle"));
        cruzar(&mut c, &mut p, em(20_100, 3_100), 3);
        igual(&c, &p);

        // (b) A pessoa no controle apaga o próprio roteiro com a pergunta aberta.
        let (mut c, mut p) = pergunta_aberta(10_000);
        c.definir_texto_em("", em(20_000, 3_000)).unwrap();
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none(), "o vazio não fechou a pergunta");
        assert_eq!(c.copia_do_texto(&resumo("roteiro do prompter")), Some("roteiro do prompter"));
        cruzar(&mut c, &mut p, em(20_100, 3_100), 3);
        assert_eq!(p.texto(), "");
        igual(&c, &p);
    }

    /// **Decisão do coordenador**: vazio não substitui roteiro no primeiro encontro — nos dois
    /// sentidos, sem pergunta e sem cópia (não sai texto nenhum).
    #[test]
    fn vazio_de_um_lado_nao_pergunta() {
        // (a) O controle apagou o roteiro há um minuto: ele não apaga o do prompter novo.
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p = nova("prompter", P);
        p.definir_texto_em("roteiro do prompter", em(1_000, 0)).unwrap();
        c.definir_texto_em("algo", em(2_000, 0)).unwrap();
        c.definir_texto_em("", em(3_000, 0)).unwrap();
        sessao(&mut c, &mut p, 1, em(4_000, 1_000));
        cruzar(&mut c, &mut p, em(4_000, 1_000), 3);
        assert_eq!(p.texto(), "roteiro do prompter", "o vazio mais novo do controle apagou o do prompter");
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none() && copias(&c).is_empty());
        igual(&c, &p);

        // (b) O prompter apagou o roteiro (mais novo que o do controle): vai o do controle.
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p = nova("prompter", P);
        c.definir_texto_em("roteiro do controle", em(1_000, 0)).unwrap();
        p.definir_texto_em("algo", em(2_000, 0)).unwrap();
        p.definir_texto_em("", em(3_000, 0)).unwrap();
        sessao(&mut c, &mut p, 1, em(4_000, 1_000));
        cruzar(&mut c, &mut p, em(4_000, 1_000), 3);
        assert_eq!(p.texto(), "roteiro do controle", "o vazio do prompter apagou o do controle");
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none() && copias(&c).is_empty());
        igual(&c, &p);
    }

    #[test]
    fn mesmo_conteudo_nao_pergunta_e_o_texto_nao_volta() {
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p = nova("prompter", P);
        p.definir_texto_em("o mesmo roteiro", em(1_000, 0)).unwrap();
        c.definir_texto_em("o mesmo roteiro", em(2_000, 0)).unwrap();
        sessao(&mut c, &mut p, 1, em(3_000, 1_000));
        // Passos de 100 ms, como numa sessão de verdade: a resposta do controle depois de adotar sai
        // na bombeada seguinte, bem antes dos 2 s em que o prompter reenviaria.
        let (mut de_c, mut de_p) = (0, 0);
        for k in 0..30u64 {
            let agora = em(3_000 + k * 100, 1_000 + k * 100);
            let (mc, mp) = (mensagens(&mut c, agora), mensagens(&mut p, agora));
            de_c += textos_em(&mc);
            de_p += textos_em(&mp);
            for m in &mc {
                p.receber_em(m, agora);
            }
            for m in &mp {
                c.receber_em(m, agora);
            }
        }
        assert_eq!(de_c, 0, "o texto igual voltou ao prompter");
        assert!(de_p <= 1, "o prompter mandou o texto {de_p} vezes");
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none() && copias(&c).is_empty());
        igual(&c, &p);
        assert_eq!(c.ultimo_prompter_id.as_deref(), Some("prompter"));
    }

    /// Um controle **sem roteiro** também retém: uma edição confirmada logo depois de conectar,
    /// antes de o texto do prompter chegar, venceria o dele por carimbo; retida, vira a pergunta.
    #[test]
    fn a_edicao_logo_depois_de_conectar_nao_substitui_o_roteiro_do_prompter() {
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        let mut p = nova("prompter", P);
        p.definir_texto_em("roteiro do prompter", em(1_000, 0)).unwrap();
        sessao(&mut c, &mut p, 1, em(5_000, 1_000));
        let primeiro = mensagens(&mut c, em(5_000, 1_000));
        c.definir_texto_em("roteiro digitado no controle", em(5_010, 1_010)).unwrap();
        let depois = mensagens(&mut c, em(5_020, 1_020));
        assert_eq!(textos_em(&depois), 0, "a edição saiu antes de o texto do prompter chegar");
        for m in primeiro.iter().chain(depois.iter()) {
            p.receber_em(m, em(5_030, 1_030));
        }
        cruzar(&mut c, &mut p, em(5_100, 1_100), 3);
        assert_eq!(p.texto(), "roteiro do prompter");
        assert!(pergunta(&c).aberta);
    }

    /// **Compatível com os prompters de 13/09, sem campo novo no fio**: o controle recebe as
    /// mensagens de um prompter no formato literal da §4 e responde com o que um controle de 13/09
    /// mandaria — primeiro a referência de quem nunca escreveu, depois a do texto do prompter —, sem
    /// nunca mandar o dele.
    #[test]
    fn o_prompter_de_13_09_pelos_bytes_dele() {
        let estado = format!(
            concat!(
                r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":1757799990001,"#,
                r#""rolando":{{"valor":false,"carimbo":0,"autor":""}},"#,
                r#""velocidade":{{"valor":1.5,"carimbo":1757799990000,"autor":"ipad-13-09"}},"#,
                r#""fonte":{{"valor":48.0,"carimbo":0,"autor":""}},"#,
                r#""margem":{{"valor":0.1,"carimbo":0,"autor":""}},"#,
                r#""linha_de_leitura":{{"valor":0.3,"carimbo":0,"autor":""}},"#,
                r#""espelho":{{"valor":false,"carimbo":0,"autor":""}},"#,
                r#""posicao":{{"valor":0.0,"carimbo":0,"autor":""}},"#,
                r#""salto":{{"valor":null,"carimbo":0,"autor":""}},"#,
                r#""texto":{{"carimbo":1757799990001,"autor":"ipad-13-09","bytes":10,"resumo":"{}"}}}}"#
            ),
            resumo("Boa noite.")
        );
        let texto = r#"{"app":"teleprompter","v":1,"tipo":"texto","relogio":1757799990001,"texto":{"valor":"Boa noite.","carimbo":1757799990001,"autor":"ipad-13-09"}}"#;
        const T: u64 = 1_757_799_995_000;
        let mut c = nova("controle", C);
        c.ligar_pergunta_do_texto();
        c.definir_texto_em("Outro roteiro.", em(T, 0)).unwrap();
        c.nova_sessao_com_par(1, Some(&ParDaSessao { id: "ipad-13-09".into(), nome: "iPad".into() }), em(T, 1_000));
        let primeiro = mensagens(&mut c, em(T, 1_000));
        assert_eq!(textos_em(&primeiro), 0);
        assert!(
            primeiro.iter().any(|m| m.contains(r#""texto":{"carimbo":0,"autor":"","bytes":0,"resumo":"e3b0c44298fc1c14"}"#)),
            "{primeiro:?}"
        );
        c.receber_em(&estado, em(T + 10, 1_010));
        c.receber_em(texto, em(T + 20, 1_020));
        let depois = mensagens(&mut c, em(T + 30, 1_030));
        assert_eq!(textos_em(&depois), 0, "o controle mandou o texto dele com a pergunta aberta");
        let referencia_dele = format!(
            r#""texto":{{"carimbo":1757799990001,"autor":"ipad-13-09","bytes":10,"resumo":"{}"}}"#,
            resumo("Boa noite.")
        );
        assert!(depois.iter().any(|m| m.contains(&referencia_dele)), "{depois:?}");
        assert!(pergunta(&c).aberta);
        assert_eq!(c.velocidade.valor, 1.5, "o resto do estado de 13/09 não entrou");
    }

    /// O prompter manda o texto ao ver a referência de quem nunca escreveu, e **para** quando o
    /// estado do controle mostra o dele — mesmo com a pergunta aberta por um minuto. Nenhuma
    /// desistência (o teto de 20 fica longe).
    #[test]
    fn o_prompter_para_de_reenviar_com_a_pergunta_aberta() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        for k in 0..30u64 {
            let agora = em(12_000 + k * 2_100, 2_000 + k * 2_100);
            cruzar(&mut c, &mut p, agora, 1);
        }
        let ep = p.estado_em(em(0, 0)).contadores;
        assert!(ep.textos_enviados <= 2, "o prompter mandou o texto {} vezes", ep.textos_enviados);
        assert_eq!(ep.reenvios_desistidos, 0);
        assert_eq!(c.estado_em(em(0, 0)).contadores.reenvios_desistidos, 0);
        assert!(pergunta(&c).aberta);
    }

    #[test]
    fn o_salvo_leva_o_ultimo_prompter_e_as_copias() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        c.resolver_texto_em(true, &resumo_do_prompter(&c), em(16_000, 3_000)).unwrap();
        cruzar(&mut c, &mut p, em(16_100, 3_100), 3);
        let salvo = c.salvo_json();
        let volta = Replica::de_salvo("controle", C, &salvo).unwrap();
        assert_eq!(volta.ultimo_prompter_id.as_deref(), Some("prompter"));
        assert_eq!(volta.referencia_convergida, c.referencia_convergida);
        assert_eq!(copias(&volta).iter().map(|c| c.resumo.clone()).collect::<Vec<_>>(), vec![resumo("roteiro do prompter")]);
        assert_eq!(volta.copia_do_texto(&resumo("roteiro do prompter")), Some("roteiro do prompter"));
        // A réplica de prompter devolve os dois intocados (o salvo único do Android).
        let como_prompter = Replica::de_salvo("controle", P, &salvo).unwrap();
        assert_eq!(como_prompter.salvo_json(), salvo);
        // Sem nada disso, o salvo é o de antes, byte a byte.
        let limpo = nova("x", C).salvo_json();
        assert!(!limpo.contains("ultimo_prompter_id") && !limpo.contains("copias_do_texto") && !limpo.contains("convergida"));
    }

    #[test]
    fn as_copias_sao_tres_sem_repeticao_e_passam_pela_regra_do_texto() {
        let mut c = nova("controle", C);
        let p = par_de(&nova("prompter", P));
        for (i, t) in ["um", "dois", "três", "quatro", "dois"].iter().enumerate() {
            c.guardar_copia(t.to_string(), OrigemDaCopia::Prompter, &p, em(1_000 + i as u64, 0));
        }
        let cs = copias(&c);
        assert_eq!(cs.iter().map(|c| c.previa.as_str()).collect::<Vec<_>>(), vec!["dois", "quatro", "três"]);
        c.guardar_copia(String::new(), OrigemDaCopia::Controle, &p, em(2_000, 0));
        assert_eq!(copias(&c).len(), 3, "um texto vazio virou cópia");
        assert!(c.esquecer_copia_do_texto(&resumo("quatro")));
        assert!(!c.esquecer_copia_do_texto(&resumo("quatro")));
        // No salvo, a cópia que não passa pela regra do texto fica de fora e é contada; o resto
        // entra — e o roteiro também.
        let salvo = format!(
            r#"{{"v":1,"texto":{{"valor":"roteiro","carimbo":5,"autor":"controle"}},"copias_do_texto":[{},{},{},"lixo"]}}"#,
            r#"{"origem":"prompter","prompter_id":"p","prompter_nome":"P","quando_ms":1,"texto":"boa"}"#,
            r#"{"origem":"marciano","prompter_id":"p","texto":"origem que não existe"}"#,
            serde_json::json!({"origem":"controle","prompter_id":"p","texto":"a\u{0}b"})
        );
        let volta = Replica::de_salvo("controle", C, &salvo).unwrap();
        assert_eq!(volta.texto(), "roteiro", "uma cópia ruim derrubou o salvo e o roteiro");
        assert_eq!(copias(&volta).iter().map(|c| c.previa.as_str()).collect::<Vec<_>>(), vec!["boa"]);
        assert_eq!(volta.estado_em(em(0, 0)).contadores.campos_recusados, 3);
    }

    /// Uma sessão sem par conhecido (o transporte montado à mão) segue a regra de antes: sem
    /// retenção, sem pergunta e sem cópia.
    #[test]
    fn sessao_sem_par_segue_como_hoje() {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        c.definir_texto_em("roteiro do controle", em(1_000, 0)).unwrap();
        p.definir_texto_em("roteiro do prompter", em(2_000, 0)).unwrap();
        c.nova_sessao(1);
        p.nova_sessao(1);
        cruzar(&mut c, &mut p, em(3_000, 1_000), 3);
        assert_eq!(c.texto(), "roteiro do prompter");
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none() && copias(&c).is_empty());
        assert_eq!(c.ultimo_prompter_id, None);
    }

    #[test]
    fn o_prompter_da_ultima_vez_segue_o_reencontro() {
        let (mut c, mut p) = pergunta_aberta(10_000);
        c.resolver_texto_em(false, &resumo_do_prompter(&c), em(16_000, 3_000)).unwrap();
        cruzar(&mut c, &mut p, em(16_100, 3_100), 3);
        c.perdeu_o_par_em(em(17_000, 4_000));
        p.definir_texto_em("o prompter editou na queda", em(18_000, 5_000)).unwrap();
        sessao(&mut c, &mut p, 2, em(19_000, 6_000));
        let m = trocar(&mut c, &mut p, em(19_000, 6_000), em(19_000, 6_000));
        assert_eq!(m & mudou::PERGUNTA_DO_TEXTO, 0);
        cruzar(&mut c, &mut p, em(19_100, 6_100), 3);
        assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none(), "o reencontro com o da última vez perguntou");
        assert_eq!(c.texto(), "o prompter editou na queda");
        igual(&c, &p);
    }

    /// **Achado B11**: a simulação de perda, duplicata e desordem, **passando pela pergunta**: o
    /// controle chega com roteiro num prompter novo (o par injetado), a pergunta abre, a pessoa
    /// escolhe num instante sorteado (e insiste enquanto ouve "ocupado"), os dois seguem editando
    /// tudo — às vezes apagando o roteiro — e as réplicas convergem. Vinte sementes.
    #[test]
    fn converge_com_perda_passando_pela_pergunta() {
        for semente in 1..=20u64 {
            let mut dado = Dado(semente * 7_919);
            let mut p = nova("prompter", P);
            let mut c = nova("controle", C);
            c.ligar_pergunta_do_texto();
            let base = 1_000_000u64;
            p.definir_texto_em("roteiro inicial do prompter", em(base - 5_000, 0)).unwrap();
            c.definir_texto_em("roteiro inicial do controle", em(base - 2_000, 0)).unwrap();
            sessao(&mut c, &mut p, 1, em(base, 0));
            let (mut abriu, mut escolheu) = (false, false);
            let mut para_p: Vec<String> = Vec::new();
            let mut para_c: Vec<String> = Vec::new();
            for passo in 0..1_200u64 {
                let t = passo * 50;
                let agora = em(base + t, t);
                if passo < 600 {
                    for (r, alvo) in [(&mut p, 0u64), (&mut c, 1u64)] {
                        match dado.prox() % 14 {
                            0 => r.definir_velocidade_em(0.1 + (dado.prox() % 190) as f64 / 10.0, agora).unwrap(),
                            1 => r.definir_rolando_em(dado.chance(50), agora).unwrap(),
                            2 => r.definir_espelho_em(dado.chance(50), agora).unwrap(),
                            3 => r.saltar_em((dado.prox() % 100) as f64 / 100.0, agora).unwrap(),
                            // Apagar o roteiro só depois de a pergunta abrir: antes, é a regra do
                            // vazio no primeiro encontro (outro teste), e a pergunta não abriria.
                            4 if dado.chance(10) => {
                                let novo = if abriu && dado.chance(10) {
                                    String::new()
                                } else {
                                    format!("roteiro {alvo}-{passo}")
                                };
                                r.definir_texto_em(&novo, agora).unwrap();
                            }
                            5 if alvo == 0 => r.definir_posicao_em((passo % 100) as f64 / 100.0, agora).unwrap(),
                            _ => {}
                        }
                    }
                }
                if let Some(q) = c.estado_em(agora).pergunta_do_texto {
                    abriu |= q.aberta;
                    if let Some(dele) = q.do_prompter {
                        if passo >= 600 || dado.chance(5) {
                            escolheu |= c.resolver_texto_em(dado.chance(50), &dele.resumo, agora).is_ok();
                        }
                    }
                }
                for (de, fila) in [(&mut p, &mut para_c), (&mut c, &mut para_p)] {
                    for m in mensagens(de, agora) {
                        if dado.chance(30) {
                            continue;
                        }
                        if dado.chance(10) {
                            fila.push(m.clone());
                        }
                        fila.push(m);
                    }
                }
                for (fila, r) in [(&mut para_p, &mut p), (&mut para_c, &mut c)] {
                    let n = fila.len() / 2 + usize::from(!fila.is_empty());
                    for _ in 0..n {
                        let i = (dado.prox() as usize) % fila.len();
                        let m = fila.swap_remove(i);
                        r.receber_em(&m, agora);
                    }
                }
            }
            for m in para_p.drain(..) {
                p.receber_em(&m, em(base + 60_000, 60_000));
            }
            for m in para_c.drain(..) {
                c.receber_em(&m, em(base + 60_000, 60_000));
            }
            for k in 0..6u64 {
                cruzar(&mut p, &mut c, em(base + 61_000 + k * 2_100, 61_000 + k * 2_100), 1);
            }
            assert!(abriu, "semente {semente}: a pergunta nunca abriu");
            assert!(c.estado_em(em(0, 0)).pergunta_do_texto.is_none(), "semente {semente}: a pergunta não fechou");
            let _ = escolheu;
            assert!(copias(&c).len() <= COPIAS_DO_TEXTO);
            igual(&p, &c);
        }
    }

    /// **De ponta a ponta, por uma sessão de verdade** (127.0.0.1): o par vem do aperto de mão, o
    /// controle com o roteiro dele pergunta, "usar o do prompter" adota o do prompter — que nunca
    /// recebe texto nenhum —, e a convergência grava o prompter da última vez.
    #[test]
    fn a_pergunta_por_uma_sessao_de_verdade() {
        use crate::session::EventoDeSessao;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let (mut sessao_p, mut sessao_c) = sessoes("pergunta", "737373");
        let tp = Arc::new(Teleprompter::nova("prompter-pergunta", P).unwrap());
        let tc = Arc::new(Teleprompter::nova("controle-pergunta", C).unwrap());
        tc.ligar_pergunta_do_texto().unwrap();
        let do_prompter: String = (0..1_500).map(|i| format!("Prompter {i}: boa noite. 🎬\n")).collect();
        tp.definir_texto(&do_prompter).unwrap();
        tc.definir_texto("O roteiro do controle.").unwrap();
        let (mp, mc) = (sessao_p.session.mensageiro(), sessao_c.session.mensageiro());
        assert_eq!(mc.par().map(|p| p.id.as_str()), Some("prompter-pergunta"));
        let parar = Arc::new(AtomicBool::new(false));
        let bombas: Vec<_> = [(Arc::clone(&tp), mp), (Arc::clone(&tc), mc)]
            .into_iter()
            .map(|(t, m)| {
                let parar = Arc::clone(&parar);
                std::thread::spawn(move || {
                    while !parar.load(Ordering::Relaxed) {
                        if t.bombear(&m, Duration::from_millis(20)).map(|b| b.fechada).unwrap_or(true) {
                            break;
                        }
                    }
                })
            })
            .collect();
        let esperar = |ok: &dyn Fn() -> bool| {
            let fim = std::time::Instant::now() + Duration::from_secs(15);
            while !ok() && std::time::Instant::now() < fim {
                std::thread::sleep(Duration::from_millis(10));
            }
            ok()
        };
        let abriu = esperar(&|| tc.estado().unwrap().pergunta_do_texto.is_some_and(|q| q.aberta));
        let visto = tc.estado().unwrap().pergunta_do_texto.and_then(|q| q.do_prompter).map(|v| v.resumo);
        let escolha = visto.map(|v| tc.resolver_texto(false, &v));
        let convergiu = esperar(&|| {
            tc.texto().unwrap() == do_prompter && tc.salvo_json().unwrap().contains("\"ultimo_prompter_id\":\"prompter-pergunta\"")
        });
        let (ep, ec) = (tp.estado().unwrap(), tc.estado().unwrap());
        parar.store(true, Ordering::Relaxed);
        for b in bombas {
            let _ = b.join();
        }
        assert!(abriu, "a pergunta não abriu em 15 s: {ec:?}");
        assert!(matches!(escolha, Some(Ok(()))), "{escolha:?}");
        assert!(convergiu, "não convergiu em 15 s: controle {ec:?}");
        assert_eq!(tp.texto().unwrap(), do_prompter, "o roteiro do prompter foi substituído");
        assert_eq!(ec.contadores.textos_enviados, 0, "o controle mandou o texto dele");
        assert_eq!(ec.copias_do_texto.len(), 1);
        assert_eq!(tc.copia_do_texto(&resumo("O roteiro do controle.")).unwrap().as_deref(), Some("O roteiro do controle."));
        assert_eq!(ep.contadores.reenvios_desistidos, 0);
        assert_eq!(sessao_p.proximo_evento(Duration::ZERO), EventoDeSessao::Nenhum);
        assert_eq!(sessao_c.proximo_evento(Duration::ZERO), EventoDeSessao::Nenhum);
    }

    // -----------------------------------------------------------------------------------------
    // §13: a gravação — o controle começa, para e vê
    // -----------------------------------------------------------------------------------------

    /// Um controle e um prompter cuja tela grava, em sessão, já trocando estado.
    fn par_que_grava(t0: u64) -> (Replica, Replica) {
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        p.ligar_gravacao(true);
        sessao(&mut c, &mut p, 1, em(t0, 1_000));
        cruzar(&mut c, &mut p, em(t0, 1_000), 2);
        assert!(c.estado_em(em(t0, 1_000)).par_entende_gravar, "o controle não viu que o prompter grava");
        (c, p)
    }

    /// Entrega `msgs` em `a` e devolve as mudanças.
    fn entregar(msgs: &[String], a: &mut Replica, agora: Agora) -> Mudancas {
        msgs.iter().fold(0, |m, x| m | a.receber_em(x, agora))
    }

    /// **O caminho feliz** (§13.2): o controle pede, o prompter vê o pedido pelo bit, a casca começa
    /// a gravar e diz isso; o controle vê o pedido respondido e a duração contando. Depois, parar.
    #[test]
    fn o_controle_pede_e_o_prompter_decide() {
        let (mut c, mut p) = par_que_grava(10_000);
        c.pedir_em(true, em(10_100, 1_100)).unwrap();
        let pedido = c.estado_em(em(10_100, 1_150)).pedido_de_gravacao.expect("o pedido daqui");
        assert!(pedido.gravar && pedido.ha_ms == 50);
        let saiu = mensagens(&mut c, em(10_100, 1_100));
        assert_eq!(saiu.len(), 1, "o pedido saiu em mais de uma mensagem");
        assert!(
            saiu[0].contains(&format!(r#""pedido_de_gravacao":{{"n":{},"gravar":true,"autor":"controle"}}"#, pedido.n)),
            "{}",
            saiu[0]
        );
        let m = entregar(&saiu, &mut p, em(10_110, 1_110));
        assert_ne!(m & mudou::GRAVACAO, 0, "a casca do prompter não soube do pedido");
        let aberto = p.estado_em(em(10_110, 1_110)).pedido_de_gravacao.expect("o pedido aberto no prompter");
        assert_eq!((aberto.n, aberto.gravar), (pedido.n, true));
        assert_eq!(p.estado_em(em(10_110, 1_110)).gravando_ha_ms, None, "o núcleo começou a gravar pela casca");

        // A casca abre o arquivo e diz que grava: é a aceitação.
        p.definir_gravando_em(true, em(10_200, 1_200)).unwrap();
        assert!(p.estado_em(em(10_200, 1_200)).pedido_de_gravacao.is_none());
        let resposta = mensagens(&mut p, em(10_300, 1_300));
        assert!(resposta[0].contains(r#""gravando_ha_ms":{"valor":100,"#), "{}", resposta[0]);
        assert!(
            resposta[0].contains(&format!(r#""resposta_de_gravacao":{{"n":{},"autor":"controle"}}"#, pedido.n)),
            "{}",
            resposta[0]
        );
        let m = entregar(&resposta, &mut c, em(10_300, 1_300));
        assert_ne!(m & mudou::GRAVACAO, 0);
        let e = c.estado_em(em(10_300, 1_500));
        assert_eq!((e.pedido_de_gravacao, e.gravacao_recusada, e.gravando_ha_ms), (None, None, Some(300)));
        // Respondido, o pedido sai do fio; e a resposta não vira pingue-pongue.
        let depois = mensagens(&mut c, em(10_310, 1_310));
        assert!(depois.iter().all(|x| !x.contains("pedido_de_gravacao")), "{depois:?}");
        assert_eq!(entregar(&depois, &mut p, em(10_320, 1_320)) & mudou::GRAVACAO, 0);

        // Parar, pelo mesmo caminho. Um batimento do prompter que saiu antes de o "parar" chegar
        // ainda leva a resposta ao "gravar": ela não responde ao pedido novo.
        c.pedir_em(false, em(20_000, 11_000)).unwrap();
        let batimento = mensagens(&mut p, em(19_990, 10_990));
        assert!(batimento[0].contains("resposta_de_gravacao"), "{}", batimento[0]);
        entregar(&batimento, &mut c, em(20_000, 11_000));
        assert!(c.estado_em(em(20_000, 11_000)).pedido_de_gravacao.is_some(), "a resposta velha respondeu ao pedido novo");
        let m = trocar(&mut c, &mut p, em(20_000, 11_000), em(20_000, 11_000));
        assert_ne!(m & mudou::GRAVACAO, 0);
        assert_eq!(p.estado_em(em(20_000, 11_000)).pedido_de_gravacao.map(|x| x.gravar), Some(false));
        p.definir_gravando_em(false, em(20_100, 11_100)).unwrap();
        trocar(&mut p, &mut c, em(20_100, 11_100), em(20_100, 11_100));
        let e = c.estado_em(em(20_200, 11_200));
        assert_eq!((e.gravando_ha_ms, e.pedido_de_gravacao, e.gravacao_recusada), (None, None, None));
        assert_eq!(p.estado_em(em(20_200, 11_200)).gravando_ha_ms, None);
    }

    /// **O pedido velho que chega atrasado é ignorado** (§13.2): o canal é confiável e sem ordem, e
    /// "gravar" seguido de "parar" pode chegar como "parar" e depois "gravar". O número decide: o
    /// prompter termina parado. E o repetido (o pedido vai em todo estado até a resposta) não conta
    /// de novo.
    #[test]
    fn o_pedido_velho_que_chega_atrasado_e_ignorado() {
        let (mut c, mut p) = par_que_grava(10_000);
        c.pedir_em(true, em(10_100, 1_100)).unwrap();
        let gravar = mensagens(&mut c, em(10_100, 1_100));
        c.pedir_em(false, em(10_101, 1_101)).unwrap();
        let parar = mensagens(&mut c, em(10_101, 1_101));
        // "parar" primeiro: a casca decide — mesmo já parado, só ela sabe se o gravador está abrindo
        // o arquivo — e aceita.
        assert_ne!(entregar(&parar, &mut p, em(10_200, 1_200)) & mudou::GRAVACAO, 0, "o núcleo decidiu pela casca");
        assert_eq!(p.estado_em(em(10_200, 1_200)).pedido_de_gravacao.map(|x| x.gravar), Some(false));
        p.definir_gravando_em(false, em(10_200, 1_200)).unwrap();
        assert!(p.estado_em(em(10_200, 1_200)).pedido_de_gravacao.is_none(), "parar parado não respondeu");
        assert_eq!(p.gravacao.carimbo, 0, "responder sem mudar recarimbou a gravação");
        // "gravar", atrasado: velho, ignorado — nem abre pedido, nem acende o bit.
        assert_eq!(entregar(&gravar, &mut p, em(10_201, 1_201)) & mudou::GRAVACAO, 0, "o pedido velho acordou a casca");
        let e = p.estado_em(em(10_201, 1_201));
        assert!(e.pedido_de_gravacao.is_none() && e.gravando_ha_ms.is_none(), "{e:?}");
        // O repetido também.
        assert_eq!(entregar(&parar, &mut p, em(10_202, 1_202)) & mudou::GRAVACAO, 0);
        trocar(&mut p, &mut c, em(10_300, 1_300), em(10_300, 1_300));
        let e = c.estado_em(em(10_300, 1_300));
        assert!(e.pedido_de_gravacao.is_none() && e.gravando_ha_ms.is_none(), "o controle não viu a resposta: {e:?}");

        // Em ordem, mas a casca ainda decidindo o primeiro: o segundo o substitui, e a casca decide de
        // novo pelo mais novo — que é o que vale.
        let (mut c, mut p) = par_que_grava(20_000);
        c.pedir_em(true, em(20_100, 1_100)).unwrap();
        let gravar = mensagens(&mut c, em(20_100, 1_100));
        assert_ne!(entregar(&gravar, &mut p, em(20_110, 1_110)) & mudou::GRAVACAO, 0);
        c.pedir_em(false, em(20_120, 1_120)).unwrap();
        let parar = mensagens(&mut c, em(20_120, 1_120));
        assert_ne!(entregar(&parar, &mut p, em(20_130, 1_130)) & mudou::GRAVACAO, 0, "a troca do pedido não avisou a casca");
        assert_eq!(p.estado_em(em(20_130, 1_130)).pedido_de_gravacao.map(|x| x.gravar), Some(false));
        // A casca tinha começado pelo "gravar": começar não responde ao "parar"; parar, sim.
        p.definir_gravando_em(true, em(20_140, 1_140)).unwrap();
        assert!(p.estado_em(em(20_140, 1_140)).pedido_de_gravacao.is_some(), "gravar respondeu a um pedido de parar");
        p.definir_gravando_em(false, em(20_150, 1_150)).unwrap();
        assert!(p.estado_em(em(20_150, 1_150)).pedido_de_gravacao.is_none());
        // E o "gravar" duplicado que ainda estava no caminho não religa.
        assert_eq!(entregar(&gravar, &mut p, em(20_160, 1_160)) & mudou::GRAVACAO, 0);
        trocar(&mut p, &mut c, em(20_200, 1_200), em(20_200, 1_200));
        let (ep, ec) = (p.estado_em(em(20_200, 1_200)), c.estado_em(em(20_200, 1_200)));
        assert!(ep.gravando_ha_ms.is_none() && ec.gravando_ha_ms.is_none() && ec.pedido_de_gravacao.is_none());
    }

    /// **A recusa, com motivo legível** (§13.3): a casca recusa, e o controle mostra o motivo. O
    /// núcleo recusa sozinho o pedido que chega a uma tela que não grava. Um motivo longo demais que
    /// chega é cortado, e não faz a recusa sumir.
    #[test]
    fn a_recusa_tem_motivo_legivel() {
        let (mut c, mut p) = par_que_grava(10_000);
        assert!(matches!(p.recusar_gravacao(1, "sem pedido"), Err(Error::Invalid(_))), "recusou sem pedido aberto");
        c.pedir_em(true, em(10_100, 1_100)).unwrap();
        trocar(&mut c, &mut p, em(10_100, 1_100), em(10_100, 1_100));
        let n = p.estado_em(em(10_100, 1_100)).pedido_de_gravacao.unwrap().n;
        assert!(matches!(p.recusar_gravacao(n, ""), Err(Error::Invalid(_))));
        assert!(matches!(p.recusar_gravacao(n, &"x".repeat(TETO_DO_MOTIVO + 1)), Err(Error::Invalid(_))));
        assert!(matches!(p.recusar_gravacao(n, "com\0nul"), Err(Error::Invalid(_))));
        // M2: a recusa diz a qual pedido responde; um `n` que não é o aberto não recusa nada.
        assert!(matches!(p.recusar_gravacao(n - 1, "velho"), Err(Error::Ocupado(_))));
        assert!(p.estado_em(em(10_100, 1_100)).pedido_de_gravacao.is_some());
        p.recusar_gravacao(n, "sem espaço: sobram 312 MB").unwrap();
        let m = trocar(&mut p, &mut c, em(10_200, 1_200), em(10_200, 1_200));
        assert_ne!(m & mudou::GRAVACAO, 0);
        let e = c.estado_em(em(10_200, 1_200));
        let recusa = e.gravacao_recusada.clone().expect("o controle não viu a recusa");
        assert_eq!((recusa.gravar, recusa.motivo.as_str()), (true, "sem espaço: sobram 312 MB"));
        assert!(e.pedido_de_gravacao.is_none() && e.gravando_ha_ms.is_none());
        assert!(serde_json::to_string(&e).unwrap().contains(r#""gravacao_recusada":{"n":"#));
        // Um pedido novo apaga a recusa velha.
        c.pedir_em(true, em(10_300, 1_300)).unwrap();
        assert!(c.estado_em(em(10_300, 1_300)).gravacao_recusada.is_none());
        let _ = mensagens(&mut c, em(10_300, 1_300));

        // A tela do prompter deixou de gravar com o pedido já no fio: o núcleo recusa por ela.
        let (mut c, mut p) = par_que_grava(20_000);
        p.ligar_gravacao(false);
        c.pedir_em(true, em(20_100, 1_100)).unwrap();
        let m = trocar(&mut c, &mut p, em(20_100, 1_100), em(20_100, 1_100));
        assert_ne!(m & mudou::GRAVACAO, 0);
        assert!(p.estado_em(em(20_100, 1_100)).pedido_de_gravacao.is_none());
        trocar(&mut p, &mut c, em(20_200, 1_200), em(20_200, 1_200));
        let e = c.estado_em(em(20_200, 1_200));
        assert_eq!(e.gravacao_recusada.map(|r| r.motivo), Some(MOTIVO_SEM_A_TELA.to_string()));
        assert!(!e.par_entende_gravar, "o controle ainda acha que o prompter grava");
        assert!(matches!(c.pedir_em(true, em(20_300, 1_300)), Err(Error::Protocol(_))));

        // Um motivo de 600 bytes que chega é cortado em até 256, numa fronteira de caractere.
        let (mut c, _) = par_que_grava(30_000);
        c.pedir_em(true, em(30_100, 1_100)).unwrap();
        let n = c.estado_em(em(30_100, 1_100)).pedido_de_gravacao.unwrap().n;
        let longo = "ã".repeat(300);
        let msg = format!(
            r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":30100,"entende_gravar":true,"resposta_de_gravacao":{{"n":{n},"autor":"controle","gravacao_recusada":"{longo}"}}}}"#
        );
        c.receber_em(&msg, em(30_200, 1_200));
        let motivo = c.estado_em(em(30_200, 1_200)).gravacao_recusada.expect("a recusa longa sumiu").motivo;
        assert!(motivo.len() <= TETO_DO_MOTIVO && motivo.chars().all(|ch| ch == 'ã'), "{} bytes", motivo.len());
    }

    /// **A queda do controle não para a gravação** (§13.4): o prompter segue gravando, e o controle
    /// que volta vê a mesma gravação, com a duração de verdade. Um pedido sem resposta não passa da
    /// sessão, e com a sessão caída o controle não pede.
    #[test]
    fn a_queda_do_controle_nao_para_a_gravacao() {
        let (mut c, mut p) = par_que_grava(10_000);
        c.pedir_em(true, em(10_100, 1_100)).unwrap();
        trocar(&mut c, &mut p, em(10_100, 1_100), em(10_100, 1_100));
        p.definir_gravando_em(true, em(10_200, 1_200)).unwrap();
        trocar(&mut p, &mut c, em(10_200, 1_200), em(10_200, 1_200));
        let carimbo = p.gravacao.carimbo;
        // Um "parar" que sai no instante da queda e nunca chega.
        c.pedir_em(false, em(11_000, 2_000)).unwrap();
        let _perdido = mensagens(&mut c, em(11_000, 2_000));
        let mc = c.perdeu_o_par_em(em(12_000, 3_000));
        let mp = p.perdeu_o_par_em(em(12_000, 3_000));
        assert_ne!(mc & mudou::GRAVACAO, 0, "o pedido sem resposta ficou na tela do controle");
        assert!(c.estado_em(em(12_000, 3_000)).pedido_de_gravacao.is_none());
        assert_eq!(mp & mudou::ROLANDO, 0);
        assert_eq!(p.estado_em(em(12_000, 3_000)).gravando_ha_ms, Some(1_800), "a queda parou a gravação");
        assert!(matches!(c.pedir_em(false, em(12_100, 3_100)), Err(Error::Closed)), "pediu sem sessão");

        // A volta: o controle começa a sessão nova sem a gravação da velha, e adota a do prompter.
        sessao(&mut c, &mut p, 2, em(20_000, 11_000));
        assert_eq!(c.estado_em(em(20_000, 11_000)).gravando_ha_ms, None);
        cruzar(&mut c, &mut p, em(20_000, 11_000), 2);
        assert_eq!(p.gravacao.carimbo, carimbo, "a sessão nova recarimbou a gravação");
        assert_eq!(c.gravacao.carimbo, carimbo);
        assert_eq!(c.estado_em(em(20_000, 11_000)).gravando_ha_ms, Some(9_800));
        assert_eq!(p.estado_em(em(20_000, 11_000)).gravando_ha_ms, Some(9_800));
        // E o "parar" perdido não chegou por outro caminho.
        assert!(p.estado_em(em(20_000, 11_000)).pedido_de_gravacao.is_none());
    }

    /// **O tempo é duração, não hora** (revisão m-1): com o relógio de parede do prompter uma hora à
    /// frente, o controle mostra a duração que o prompter relatou, somada ao tempo desde a chegada. Um
    /// estado atrasado, com duração menor, não faz a contagem andar para trás.
    #[test]
    fn o_tempo_de_gravacao_e_duracao_e_nao_hora() {
        const HORA: u64 = 3_600_000;
        let mut c = nova("controle", C);
        let mut p = nova("prompter", P);
        p.ligar_gravacao(true);
        sessao(&mut c, &mut p, 1, em(10_000, 1_000));
        cruzar(&mut c, &mut p, em(10_000 + HORA, 1_000), 1);
        p.definir_gravando_em(true, em(10_000 + HORA, 1_000)).unwrap();
        let aos_5500 = mensagens(&mut p, em(15_500 + HORA, 5_500));
        p.envio.estado_devido = true;
        let aos_6000 = mensagens(&mut p, em(16_000 + HORA, 6_000));
        // O relógio monotônico do controle tem outra origem: 50 s.
        c.receber_em(&aos_6000[0], em(10_000, 50_000));
        assert_eq!(c.estado_em(em(10_000, 50_000)).gravando_ha_ms, Some(5_000));
        assert_eq!(c.estado_em(em(10_000, 51_000)).gravando_ha_ms, Some(6_000));
        // O de 4,5 s chega depois, atrasado: a contagem não volta.
        c.receber_em(&aos_5500[0], em(10_000, 52_000));
        assert_eq!(c.estado_em(em(10_000, 52_000)).gravando_ha_ms, Some(7_000));
        // Nenhum número de hora no fio da gravação: o valor é a duração.
        assert!(aos_6000[0].contains(r#""gravando_ha_ms":{"valor":5000,"#), "{}", aos_6000[0]);
    }

    /// **Só o prompter escreve a gravação** (§13.1), como a `posicao`: no controle, relatar é
    /// `INVALID`; o que chega ao prompter como `gravando_ha_ms` é ignorado; e o controle não pede a si
    /// mesmo nem recusa.
    #[test]
    fn so_o_prompter_escreve_a_gravacao() {
        let (mut c, mut p) = par_que_grava(10_000);
        assert!(matches!(c.definir_gravando_em(true, em(10_100, 1_100)), Err(Error::Invalid(_))));
        assert!(matches!(c.recusar_gravacao(1, "não"), Err(Error::Invalid(_))));
        assert!(matches!(p.pedir_em(true, em(10_100, 1_100)), Err(Error::Invalid(_))));
        c.ligar_gravacao(true);
        assert!(mensagens(&mut c, em(10_100, 1_100)).iter().all(|x| !x.contains("grav")));
        let forjado = r#"{"app":"teleprompter","v":1,"tipo":"estado","relogio":10100,"gravando_ha_ms":{"valor":99999,"carimbo":10100,"autor":"controle"}}"#;
        assert_eq!(p.receber_em(forjado, em(10_200, 1_200)) & mudou::GRAVACAO, 0);
        assert_eq!(p.estado_em(em(10_200, 1_200)).gravando_ha_ms, None, "o controle escreveu a gravação do prompter");
        // E o controle não ecoa a gravação que adotou.
        p.definir_gravando_em(true, em(10_300, 1_300)).unwrap();
        trocar(&mut p, &mut c, em(10_300, 1_300), em(10_300, 1_300));
        assert!(c.estado_em(em(10_300, 1_300)).gravando_ha_ms.is_some());
        c.definir_velocidade_em(3.0, em(10_400, 1_400)).unwrap();
        assert!(mensagens(&mut c, em(10_400, 1_400)).iter().all(|x| !x.contains("gravando")));
    }

    /// **A tela que fecha recusa o pedido que estava aberto** (revisão de 24/09, M1): sem isto, o
    /// controle ficava "esperando" até a sessão acabar — o reenvio do pedido é repetido, e pedir de novo
    /// dá `PROTOCOL`.
    #[test]
    fn a_tela_que_fecha_recusa_o_pedido_aberto() {
        let (mut c, mut p) = par_que_grava(10_000);
        c.pedir_em(true, em(10_100, 1_100)).unwrap();
        trocar(&mut c, &mut p, em(10_100, 1_100), em(10_100, 1_100));
        assert!(p.estado_em(em(10_100, 1_100)).pedido_de_gravacao.is_some());
        p.ligar_gravacao(false);
        assert!(p.estado_em(em(10_200, 1_200)).pedido_de_gravacao.is_none());
        trocar(&mut p, &mut c, em(10_200, 1_200), em(10_200, 1_200));
        let e = c.estado_em(em(10_200, 1_200));
        assert!(e.pedido_de_gravacao.is_none(), "o controle ficou esperando");
        assert_eq!(e.gravacao_recusada.map(|r| r.motivo), Some(MOTIVO_SEM_A_TELA.to_string()));
    }

    /// **O pedido aberto no prompter sobrevive à queda** (§13.4): chegou, é a vontade de quem pediu, e
    /// a casca decide depois. E o número do último pedido é esquecido na sessão nova (revisão de 24/09,
    /// m2), como o `n` do futuro é recusado na chegada.
    #[test]
    fn o_pedido_aberto_sobrevive_a_queda_e_o_numero_nao_passa_da_sessao() {
        let (mut c, mut p) = par_que_grava(10_000);
        c.pedir_em(true, em(10_100, 1_100)).unwrap();
        trocar(&mut c, &mut p, em(10_100, 1_100), em(10_100, 1_100));
        p.perdeu_o_par_em(em(11_000, 2_000));
        let aberto = p.estado_em(em(11_000, 2_000)).pedido_de_gravacao.expect("a queda levou o pedido aberto");
        p.definir_gravando_em(aberto.gravar, em(11_100, 2_100)).unwrap();
        assert!(p.estado_em(em(11_100, 2_100)).gravando_ha_ms.is_some());
        // Na sessão nova, o prompter não guarda o `n` da velha.
        sessao(&mut c, &mut p, 2, em(20_000, 11_000));
        assert!(p.ultimo_pedido.is_none(), "o número do último pedido atravessou a sessão");

        // O `n` de um relógio dias à frente é recusado e contado, e não trava os pedidos seguintes.
        let (mut c, mut p) = par_que_grava(30_000);
        let futuro = 30_000 + 3 * 24 * 3_600_000;
        let msg = format!(
            r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":30000,"pedido_de_gravacao":{{"n":{futuro},"gravar":true,"autor":"controle"}}}}"#
        );
        let antes = p.estado_em(em(0, 0)).contadores.carimbos_do_futuro;
        assert_eq!(p.receber_em(&msg, em(30_100, 1_100)) & mudou::GRAVACAO, 0);
        assert!(p.estado_em(em(0, 0)).contadores.carimbos_do_futuro > antes);
        c.pedir_em(true, em(30_200, 1_200)).unwrap();
        assert_ne!(trocar(&mut c, &mut p, em(30_200, 1_200), em(30_200, 1_200)) & mudou::GRAVACAO, 0);
    }

    /// **Uma duração absurda no fio é recusada** (revisão de 24/09, m3): pela regra do começo mais
    /// cedo, ela ficaria presa a gravação inteira.
    #[test]
    fn duracao_absurda_e_recusada() {
        let mut c = nova("controle", C);
        let absurdo = format!(
            r#"{{"app":"teleprompter","v":1,"tipo":"estado","relogio":10000,"gravando_ha_ms":{{"valor":{},"carimbo":10000,"autor":"prompter"}}}}"#,
            u64::MAX
        );
        c.receber_em(&absurdo, em(10_000, 1_000));
        let e = c.estado_em(em(10_000, 1_000));
        assert_eq!(e.gravando_ha_ms, None);
        assert_eq!(e.contadores.campos_recusados, 1);
    }

    /// **A gravação não vai no salvo** (§13.1): é da sessão; uma vida nova do app começa sem gravar.
    #[test]
    fn a_gravacao_nao_vai_no_salvo() {
        let mut p = nova("prompter", P);
        p.ligar_gravacao(true);
        p.definir_gravando_em(true, em(10_000, 1_000)).unwrap();
        let salvo = p.salvo_json();
        assert!(!salvo.contains("grav"), "{salvo}");
        let mut de_novo = Replica::de_salvo("prompter", P, &salvo).unwrap();
        assert_eq!(de_novo.estado_em(em(10_000, 2_000)).gravando_ha_ms, None);
        // A tela que grava liga de novo na vida nova: diz que grava, e não diz que está gravando.
        de_novo.ligar_gravacao(true);
        let fio = mensagens(&mut de_novo, em(10_000, 0));
        assert!(fio[0].contains(r#""entende_gravar":true"#) && !fio[0].contains("gravando_ha_ms"), "{}", fio[0]);
    }

    /// **A build anterior** (§13.5), dos dois lados:
    ///
    /// - quem nunca gravou nem pediu manda o `estado` de antes, byte a byte (as chaves só aparecem
    ///   quando existem; `o_formato_no_fio_e_o_do_contrato` segue igual);
    /// - um prompter de antes (sem `"entende_gravar"`) nunca recebe pedido: o controle ouve `PROTOCOL`
    ///   e nada vai ao fio — a tela não mostra o botão;
    /// - um controle de antes lê o estado de um prompter gravando campo a campo, e as chaves novas não
    ///   mudam nada do que ele lê;
    /// - o estado de um controle de antes não abre pedido nenhum.
    #[test]
    fn a_build_anterior_nao_ve_o_botao_e_nada_quebra() {
        let mut quieto = nova("prompter", P);
        quieto.definir_espelho_em(true, em(1, 0)).unwrap();
        assert!(mensagens(&mut quieto, em(1, 0)).iter().all(|x| !x.contains("grav")));

        let de_14_09 = r#"{"app":"teleprompter","v":1,"tipo":"estado","relogio":10000,"rolando":{"valor":false,"carimbo":0,"autor":""},"entende_segurar":true,"texto":{"carimbo":0,"autor":"","bytes":0,"resumo":"e3b0c44298fc1c14"}}"#;
        let mut c = nova("controle", C);
        c.nova_sessao_com_par(1, Some(&ParDaSessao { id: "ipad-14-09".into(), nome: "iPad".into() }), em(10_000, 1_000));
        c.receber_em(de_14_09, em(10_000, 1_000));
        assert!(!c.estado_em(em(10_000, 1_000)).par_entende_gravar);
        assert!(matches!(c.pedir_em(true, em(10_100, 1_100)), Err(Error::Protocol(_))));
        assert!(mensagens(&mut c, em(10_100, 1_100)).iter().all(|x| !x.contains("pedido")));

        // O estado de um prompter gravando, com e sem as chaves novas: o que um controle lê dos campos
        // de antes é o mesmo, e nada é recusado.
        let (mut c, mut p) = par_que_grava(20_000);
        c.pedir_em(true, em(20_100, 1_100)).unwrap();
        trocar(&mut c, &mut p, em(20_100, 1_100), em(20_100, 1_100));
        p.definir_gravando_em(true, em(20_200, 1_200)).unwrap();
        p.definir_velocidade_em(2.5, em(20_200, 1_200)).unwrap();
        let cheio = mensagens(&mut p, em(20_300, 1_300)).pop().unwrap();
        let mut v: Value = serde_json::from_str(&cheio).unwrap();
        for chave in ["gravando_ha_ms", "entende_gravar", "resposta_de_gravacao"] {
            assert!(v.as_object_mut().unwrap().remove(chave).is_some(), "faltou {chave}: {cheio}");
        }
        let de_antes = v.to_string();
        let (mut novo, mut antigo) = (nova("c1", C), nova("c2", C));
        novo.receber_em(&cheio, em(20_300, 1_300));
        antigo.receber_em(&de_antes, em(20_300, 1_300));
        igual(&novo, &antigo);
        assert_eq!(novo.estado_em(em(0, 0)).contadores.campos_recusados, 0);

        // Um controle de antes: o estado dele não traz pedido, e nada acorda a casca do prompter.
        let mut p = nova("prompter", P);
        p.ligar_gravacao(true);
        let do_controle_antigo = r#"{"app":"teleprompter","v":1,"tipo":"estado","relogio":10000,"rolando":{"valor":true,"carimbo":10000,"autor":"a10s"},"texto":{"carimbo":0,"autor":"","bytes":0,"resumo":"e3b0c44298fc1c14"}}"#;
        assert_eq!(p.receber_em(do_controle_antigo, em(10_000, 1_000)) & mudou::GRAVACAO, 0);
        assert!(p.estado_em(em(10_000, 1_000)).pedido_de_gravacao.is_none());
    }

    /// **Pedidos embaralhados terminam no último** (§13.2): o controle pede gravar e parar ao acaso,
    /// o canal perde 30 %, duplica 10 % e embaralha, e a casca do prompter obedece a cada bit. Em 20
    /// sementes, o prompter termina como o **último** pedido mandou, o controle vê isso, e nenhum
    /// pedido fica sem resposta.
    #[test]
    fn pedidos_embaralhados_terminam_no_ultimo() {
        // A casca decide **depois** do bit, de 0 a 5 passos (0 a 250 ms), como um gravador que leva
        // tempo para abrir o arquivo: pedidos chegam e substituem o aberto enquanto ela decide. Ela
        // decide pelo pedido que estiver aberto **na hora de decidir**, e às vezes (1 em 5) recusa.
        fn decidir(p: &mut Replica, dado: &mut Dado, agora: Agora) {
            if let Some(pedido) = p.estado_em(agora).pedido_de_gravacao {
                if dado.chance(20) {
                    p.recusar_gravacao(pedido.n, "a câmera não está entregando").unwrap();
                } else {
                    p.definir_gravando_em(pedido.gravar, agora).unwrap();
                }
            }
        }
        for semente in 1..=20u64 {
            let mut dado = Dado(semente);
            let (mut c, mut p) = par_que_grava(1_000_000);
            // O último pedido, e se a casca o recusou (então vale o estado de antes dele).
            let mut ultimo: Option<(u64, bool)> = None;
            let mut decidir_em: Option<u64> = None;
            let (mut para_p, mut para_c): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
            for passo in 0..1_200u64 {
                let t = 2_000 + passo * 50;
                let agora = em(1_000_000 + t, t);
                if passo < 600 && dado.chance(8) {
                    let gravar = dado.chance(50);
                    c.pedir_em(gravar, agora).unwrap();
                    ultimo = c.estado_em(agora).pedido_de_gravacao.map(|x| (x.n, gravar));
                }
                if decidir_em.is_some_and(|d| passo >= d) {
                    decidir_em = None;
                    decidir(&mut p, &mut dado, agora);
                }
                for (de, fila) in [(&mut p, &mut para_c), (&mut c, &mut para_p)] {
                    for m in mensagens(de, agora) {
                        if dado.chance(30) {
                            continue;
                        }
                        if dado.chance(10) {
                            fila.push(m.clone());
                        }
                        fila.push(m);
                    }
                }
                let n = para_p.len() / 2 + usize::from(!para_p.is_empty());
                for _ in 0..n {
                    let i = (dado.prox() as usize) % para_p.len();
                    let msg = para_p.swap_remove(i);
                    if p.receber_em(&msg, agora) & mudou::GRAVACAO != 0 && decidir_em.is_none() {
                        decidir_em = Some(passo + dado.prox() % 6);
                    }
                }
                let n = para_c.len() / 2 + usize::from(!para_c.is_empty());
                for _ in 0..n {
                    let i = (dado.prox() as usize) % para_c.len();
                    let msg = para_c.swap_remove(i);
                    c.receber_em(&msg, agora);
                }
            }
            // O fim: o canal entrega o que falta, sem perda, e a casca decide na hora.
            for k in 0..8u64 {
                let agora = em(1_071_000 + k * 2_100, 71_000 + k * 2_100);
                for msg in std::mem::take(&mut para_p).into_iter().chain(mensagens(&mut c, agora)) {
                    p.receber_em(&msg, agora);
                }
                decidir(&mut p, &mut dado, agora);
                for msg in std::mem::take(&mut para_c).into_iter().chain(mensagens(&mut p, agora)) {
                    c.receber_em(&msg, agora);
                }
            }
            let agora = em(1_090_000, 90_000);
            let (ep, ec) = (p.estado_em(agora), c.estado_em(agora));
            let (ultimo, gravar) = ultimo.expect("a semente não pediu nada");
            assert!(
                ep.pedido_de_gravacao.is_none() && ec.pedido_de_gravacao.is_none(),
                "semente {semente}: pedido sem resposta"
            );
            assert_eq!(ep.gravando_ha_ms.is_some(), ec.gravando_ha_ms.is_some(), "semente {semente}: o controle não viu");
            // O último pedido foi respondido — ao pedido certo — e o prompter está como ele mandou,
            // a menos que a casca o tenha recusado (e aí o controle sabe por quê).
            let resposta = p.resposta_de_gravacao.clone().expect("sem resposta");
            assert_eq!(resposta.n, ultimo, "semente {semente}: a última resposta não é a do último pedido");
            match ec.gravacao_recusada {
                Some(r) => assert_eq!(r.n, ultimo, "semente {semente}: a recusa de outro pedido"),
                None => assert_eq!(
                    ep.gravando_ha_ms.is_some(),
                    gravar,
                    "semente {semente}: o prompter não terminou como o último pedido mandou"
                ),
            }
        }
    }
}
