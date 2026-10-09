//! O controle remoto da câmera: **quem recebe também controla a câmera de quem filma**.
//!
//! O contrato está em `docs/controle-remoto-da-camera.md`; este módulo é a forma executável dele.
//! Mora no núcleo pelo motivo do teleprompter: a regra do "vence quem mexer por último", a
//! validação do que chega e o reenvio sobre um canal que perde são sutis, e cinco cascas (Swift,
//! Kotlin, o C do JNI, o Rust do Windows e o C do OBS) seriam cinco jeitos de errá-los.
//!
//! # O desenho, em uma tela
//!
//! - **O filmador é a autoridade.** Ele tem o ajuste da câmera em uso (o registro do R9,
//!   `docs/controles-de-camera.md` §2), valida o que chega, entrega à casca um pedido já validado,
//!   e **publica o estado aplicado** a todos os receptores. O receptor nunca muda nada sozinho.
//! - **Cada mudança sobe a `versao` do filmador.** O receptor manda no pedido a versão que viu
//!   (`vista`); um campo pedido só entra se ninguém **mais** o mudou depois dela (§4 do contrato).
//! - **As mensagens passam pelo canal de dados da sessão de vídeo**, que é **sem retransmissão**.
//!   Por isso tudo é estado inteiro e reenviado: o estado sai a cada mudança e a cada segundo, e o
//!   pedido é reenviado até o recibo (`seu`) do estado mostrar que foi tratado.
//! - **O núcleo nunca toca na câmera.** A casca aplica com o código do R9 (as travas que guardam
//!   o lido, os cortes de faixa) e devolve o registro que ficou valendo.
//!
//! As duas pontas são máquinas puras ([`NucleoDoFilmador`], [`NucleoDoReceptor`]) que recebem o
//! instante de fora, para os testes simularem perda, duplicata, desordem e o relógio; o
//! [`Filmador`] e o [`Controlador`] são as mesmas máquinas atrás de um cadeado, com a bombeada
//! sobre um [`Mensageiro`], e são o que a fronteira C embrulha e o Windows usa.

#![cfg(feature = "webrtc")]

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::transport::{Mensageiro, ParDaSessao, FILA_DE_DADOS};

// =============================================================================================
// O contrato, em constantes
// =============================================================================================

/// O valor da chave `"app"` de toda mensagem da câmera.
pub const APP: &str = "camera";
/// A versão **deste** contrato (`"v"`). Campo novo e aditivo não a sobe.
pub const VERSAO: u64 = 1;

/// **O teto de uma mensagem da câmera**, nos dois sentidos: 4 KiB. Acima dele, não sai (a chamada
/// que a faria crescer é recusada) e, na chegada, é descartada e contada.
pub const TETO_DA_MENSAGEM_DA_CAMERA: usize = 4096;
/// **O alvo** de uma mensagem: um pedaço SCTP. O canal da sessão de vídeo não retransmite, e uma
/// mensagem de um pedaço só chega inteira ou se perde inteira. Os testes medem o estado típico.
pub const ALVO_DA_MENSAGEM: usize = 1100;
/// O teto das capacidades que a casca do filmador publica, em bytes de JSON.
pub const TETO_DAS_CAPACIDADES: usize = 2048;
/// O teto do ajuste (o registro do R9), em bytes de JSON.
pub const TETO_DO_AJUSTE: usize = 1024;
/// O teto do lido, em bytes de JSON.
pub const TETO_DO_LIDO: usize = 512;
/// O teto do nome de um aparelho no estado (cortado numa fronteira de caractere).
pub const TETO_DO_NOME: usize = 64;
/// O teto de um código (de "quem limita" e de recusa): `[a-z0-9_]`, de 1 a 32 bytes.
pub const TETO_DO_CODIGO: usize = 32;
/// Quantos campos cabem num pedido.
pub const TETO_DE_CAMPOS: usize = 16;
/// Quantas sessões um filmador serve. Acima disso o `ola` é ignorado e contado.
pub const TETO_DE_SESSOES: usize = 16;
/// Quantos pedidos aceitos esperam a casca do filmador. Acima disso, `ocupado`.
pub const FILA_DE_PEDIDOS: usize = 8;

/// O batimento do estado, sem mudança nenhuma: o reparo de qualquer perda.
pub const BATIMENTO: Duration = Duration::from_millis(1000);
/// O intervalo mínimo entre dois estados quando só o lido mudou (4 por segundo, como a linha do
/// R9 §3.6).
pub const INTERVALO_DO_LIDO: Duration = Duration::from_millis(250);
/// O intervalo mínimo entre dois envios de pedido do receptor (15 por segundo, como os deslizantes
/// do R9 §2.2).
pub const INTERVALO_DOS_PEDIDOS: Duration = Duration::from_millis(67);
/// O reenvio do pedido ainda sem recibo.
pub const REENVIO_DO_PEDIDO: Duration = Duration::from_millis(250);
/// Sem recibo depois disto, contado da última mudança, o receptor desiste (`sem_resposta`).
pub const PRAZO_DO_PEDIDO: Duration = Duration::from_secs(3);
/// O reenvio do `ola` enquanto não há estado, ou enquanto as capacidades não batem.
pub const REENVIO_DO_OLA: Duration = Duration::from_millis(1000);
/// Sem estado nenhum depois disto, a situação do receptor é `sem_resposta`.
pub const SEM_FILMADOR: Duration = Duration::from_secs(5);
/// Por quanto tempo o filmador mostra "Controlado por <aparelho>".
pub const CONTROLADO_POR_DURA: Duration = Duration::from_secs(4);
/// Por quanto tempo a recusa fica no estado do receptor.
pub const RECUSA_DURA: Duration = Duration::from_secs(3);
/// O `ola` contra um filmador que não responde (`sem_resposta`) recua para isto: um filmador de
/// build anterior nunca vai responder, e a fila dele só enche.
pub const REENVIO_DO_OLA_SEM_RESPOSTA: Duration = Duration::from_secs(5);
/// As capacidades saem a uma sessão no máximo uma vez por isto, a não ser que mudem: um `ola`
/// repetido não vira 2 KB de resposta a cada vez.
pub const INTERVALO_DAS_CAPACIDADES: Duration = Duration::from_millis(1000);
/// Um pedido que a casca tirou e não respondeu em tanto tempo é recusado com `nao_aplicado`, e o
/// `set_settings` atrasado dele é recusado: a tabela do que está em aplicação não cresce sem fim.
pub const PRAZO_DA_CASCA: Duration = Duration::from_secs(5);

/// O `pedido` de [`NucleoDoFilmador::definir_ajuste`] que quer dizer **"a casca escreveu sozinha"**:
/// o valor lido que a trava guarda, o "Travado de novo depois de medir a cena", o foco lido ao
/// travar (R9 §2.1). O estado sai com o registro novo, mas nenhum campo muda de dono nem de versão
/// para o "vence o último", e o `autor` fica — escrita automática não é gente mexendo.
pub const AJUSTE_DO_SISTEMA: u64 = u64::MAX;

/// Os motivos de recusa no fio, literais (`docs/controle-remoto-da-camera.md` §3.5).
pub mod motivo {
    /// A opção "Permitir controle remoto da câmera" está desligada.
    pub const NAO_PERMITIDO: &str = "nao_permitido";
    /// A sessão não tem par conhecido (montada à mão, fora de `hospedar`/`conectar`).
    pub const NAO_PAREADO: &str = "nao_pareado";
    /// O filmador não tem câmera com controles agora.
    pub const SEM_CAMERA: &str = "sem_camera";
    /// A `epoca` ou o `cap` do pedido não são os atuais.
    pub const CAMERA_TROCADA: &str = "camera_trocada";
    /// Campo que as capacidades não listam.
    pub const CAMPO_DESCONHECIDO: &str = "campo_desconhecido";
    /// Valor de tipo errado, não finito, fora da faixa ou da lista.
    pub const FORA_DA_FAIXA: &str = "fora_da_faixa";
    /// O ajuste resultante quebra uma regra do R9.
    pub const INCOERENTE: &str = "incoerente";
    /// Todos os campos foram mudados por outro depois do que o receptor viu.
    pub const SUPERADO: &str = "superado";
    /// A fila de pedidos do filmador encheu.
    pub const OCUPADO: &str = "ocupado";
    /// O pedido não se lê.
    pub const INVALIDO: &str = "invalido";
    /// A casca do filmador não conseguiu aplicar.
    pub const NAO_APLICADO: &str = "nao_aplicado";
    /// O toque caiu fora da imagem que vai ao ar (numa tarja). Da casca do filmador.
    pub const FORA_DA_IMAGEM: &str = "fora_da_imagem";
    /// Só no receptor: sem recibo nem recusa no prazo.
    pub const SEM_RESPOSTA: &str = "sem_resposta";
}

/// A situação do receptor, para a tela (`"situacao"`).
pub mod situacao {
    /// Sessão nova, nada chegou ainda.
    pub const ESPERANDO: &str = "esperando";
    /// Nenhum estado em [`super::SEM_FILMADOR`]: filmador de build anterior, ou sem a casca nova.
    pub const SEM_RESPOSTA: &str = "sem_resposta";
    /// O filmador não tem câmera com controles (`cap` 0).
    pub const SEM_CAMERA: &str = "sem_camera";
    /// A opção do filmador está desligada: controles apagados, com os valores.
    pub const NAO_PERMITIDO: &str = "nao_permitido";
    /// Controles vivos.
    pub const PRONTO: &str = "pronto";
}

/// O que mudou no filmador, em bits (`QuallCameraHostChange`).
pub mod mudou_no_filmador {
    /// Há pedido aceito para a casca tirar ([`super::Filmador::proximo_pedido`]).
    pub const PEDIDO: u32 = 1;
    /// Um receptor disse `ola` ou saiu.
    pub const RECEPTORES: u32 = 2;
}

/// O que mudou no receptor **por causa do filmador**, em bits (`QuallCameraRemoteChange`).
pub mod mudou_no_receptor {
    /// Chegaram capacidades novas.
    pub const CAPACIDADES: u32 = 1;
    /// O ajuste aplicado, o autor, a permissão, ou o pendente que caiu.
    pub const AJUSTE: u32 = 2;
    /// O lido.
    pub const LIDO: u32 = 4;
    /// Uma recusa chegou (ou o receptor desistiu).
    pub const RECUSA: u32 = 8;
    /// A situação mudou.
    pub const SITUACAO: u32 = 16;
}

/// Um conjunto de bits de [`mudou_no_filmador`] ou [`mudou_no_receptor`].
pub type Mudancas = u32;

type Objeto = Map<String, Value>;

// =============================================================================================
// As regras de valor, que as duas pontas usam
// =============================================================================================

/// Uma recusa: o código e, quando há, o campo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recusa {
    pub motivo: String,
    pub campo: Option<String>,
}

impl Recusa {
    fn de(motivo: &str, campo: Option<&str>) -> Recusa {
        Recusa { motivo: motivo.to_string(), campo: campo.map(str::to_string) }
    }
}

/// Um código do contrato: `[a-z0-9_]`, de 1 a [`TETO_DO_CODIGO`] bytes.
pub fn codigo_valido(c: &str) -> bool {
    !c.is_empty()
        && c.len() <= TETO_DO_CODIGO
        && c.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// As três formas de descritor de um controle (`docs/controle-remoto-da-camera.md` §3.2).
#[derive(Debug, Clone, PartialEq)]
enum Descritor {
    /// `{"valores":[…]}`: texto, um dos valores.
    Lista(Vec<String>),
    /// `{"min":…,"max":…}`, com `"inteiro"` opcional.
    Numero { min: f64, max: f64, inteiro: bool },
    /// `{}` (ou só com dicas para a tela): sim/não.
    Sim,
}

/// Lê um descritor. `None` quando a forma não é nenhuma das três.
fn descritor(v: &Value) -> Option<Descritor> {
    let o = v.as_object()?;
    if let Some(valores) = o.get("valores") {
        let lista = valores.as_array()?;
        if lista.is_empty() || lista.len() > 32 {
            return None;
        }
        let mut saida = Vec::with_capacity(lista.len());
        for item in lista {
            let t = item.as_str()?;
            if t.is_empty() || t.len() > TETO_DO_CODIGO {
                return None;
            }
            saida.push(t.to_string());
        }
        return Some(Descritor::Lista(saida));
    }
    match (o.get("min"), o.get("max")) {
        (Some(a), Some(b)) => {
            let (min, max) = (a.as_f64()?, b.as_f64()?);
            if !min.is_finite() || !max.is_finite() || min > max {
                return None;
            }
            let inteiro = match o.get("inteiro") {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return None,
            };
            Some(Descritor::Numero { min, max, inteiro })
        }
        (None, None) => Some(Descritor::Sim),
        _ => None,
    }
}

/// O número na forma do fio: **inteiro sem `.0`** quando não tem parte fracionária, porque o
/// Android lê `iso` e `kelvin` como `Int` e `obturadorNs` como `Long`.
fn numero_no_fio(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9.0e15 {
        // `as` é exato aqui: inteiro, e dentro de 2^53.
        Value::from(x as i64)
    } else {
        serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
    }
}

/// Confere um valor contra o descritor e devolve a forma do fio. `Err` é `fora_da_faixa`.
fn conferir_valor(d: &Descritor, v: &Value) -> std::result::Result<Value, ()> {
    match d {
        Descritor::Lista(lista) => match v.as_str() {
            Some(t) if lista.iter().any(|x| x == t) => Ok(Value::String(t.to_string())),
            _ => Err(()),
        },
        Descritor::Numero { min, max, inteiro } => {
            let x = v.as_f64().ok_or(())?;
            if !x.is_finite() {
                return Err(());
            }
            // Uma folga relativa mínima, para o número que voltou do fio pelo outro lado.
            let folga = 1e-9 * (max - min).abs().max(1.0);
            if x < min - folga || x > max + folga {
                return Err(());
            }
            if *inteiro && x.fract() != 0.0 {
                return Err(());
            }
            Ok(numero_no_fio(x.clamp(*min, *max)))
        }
        Descritor::Sim => v.as_bool().map(Value::Bool).ok_or(()),
    }
}

/// O modo de um campo de modo no ajuste, com o padrão do R9 quando o registro não o grava (o Mac
/// grava só cinco campos; `docs/controles-de-camera.md` §2).
fn modo<'a>(ajuste: &'a Objeto, campo: &str) -> &'a str {
    ajuste.get(campo).and_then(Value::as_str).unwrap_or("auto")
}

/// As regras do R9 que um receptor pode quebrar (contrato §6, item 8): **pedir** o campo exige o
/// modo certo no ajuste resultante.
const COERENCIA: &[(&str, &str, &str)] = &[
    ("ev", "exposicao", "auto"),
    ("travaExposicao", "exposicao", "auto"),
    ("iso", "exposicao", "manual"),
    ("obturadorNs", "exposicao", "manual"),
    ("travaBalanco", "balanco", "auto"),
    ("kelvin", "balanco", "kelvin"),
    ("focoPosicao", "foco", "manual"),
];

/// Um toque na imagem que o receptor vê, de 0 a 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Toque {
    pub x: f64,
    pub y: f64,
    pub longo: bool,
}

/// O pedido depois de conferido: o ajuste parcial na forma do fio, e as ações.
#[derive(Debug, Clone, PartialEq, Default)]
struct PedidoConferido {
    ajuste: Objeto,
    restaurar: bool,
    toque: Option<Toque>,
}

/// Confere um pedido contra as capacidades e o ajuste vigente, **sem** o "vence o último" — que é
/// do filmador. As duas pontas usam esta função: o receptor para recusar na hora, sem rede; o
/// filmador porque nunca confia no receptor.
fn conferir_pedido(
    capacidades: &Objeto,
    vigente: &Objeto,
    ajuste: Option<&Value>,
    restaurar: Option<&Value>,
    toque: Option<&Value>,
) -> std::result::Result<PedidoConferido, Recusa> {
    let controles = capacidades.get("controles").and_then(Value::as_object);
    let mut saida = PedidoConferido::default();
    if let Some(v) = ajuste {
        let parcial = v.as_object().ok_or_else(|| Recusa::de(motivo::INVALIDO, None))?;
        if parcial.len() > TETO_DE_CAMPOS {
            return Err(Recusa::de(motivo::FORA_DA_FAIXA, None));
        }
        for (campo, valor) in parcial {
            let d = controles
                .and_then(|c| c.get(campo))
                .and_then(descritor)
                .ok_or_else(|| Recusa::de(motivo::CAMPO_DESCONHECIDO, Some(campo)))?;
            let valor = conferir_valor(&d, valor)
                .map_err(|()| Recusa::de(motivo::FORA_DA_FAIXA, Some(campo)))?;
            saida.ajuste.insert(campo.clone(), valor);
        }
    }
    match restaurar {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => saida.restaurar = true,
        Some(_) => return Err(Recusa::de(motivo::INVALIDO, Some("restaurar"))),
    }
    match toque {
        None | Some(Value::Null) => {}
        Some(v) => {
            if controles.and_then(|c| c.get("toque")).is_none() {
                return Err(Recusa::de(motivo::CAMPO_DESCONHECIDO, Some("toque")));
            }
            let o = v.as_object().ok_or_else(|| Recusa::de(motivo::INVALIDO, Some("toque")))?;
            let eixo = |k: &str| o.get(k).and_then(Value::as_f64).filter(|x| (0.0..=1.0).contains(x));
            let (Some(x), Some(y)) = (eixo("x"), eixo("y")) else {
                return Err(Recusa::de(motivo::FORA_DA_FAIXA, Some("toque")));
            };
            let longo = match o.get("longo") {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(Recusa::de(motivo::FORA_DA_FAIXA, Some("toque"))),
            };
            saida.toque = Some(Toque { x, y, longo });
        }
    }
    if saida.ajuste.is_empty() && !saida.restaurar && saida.toque.is_none() {
        return Err(Recusa::de(motivo::INVALIDO, None));
    }
    // Com `restaurar`, a casca restaura **antes** de aplicar os campos (contrato §6, a ordem): a
    // coerência é a do registro padrão, que é todo "auto".
    let vazio = Objeto::new();
    conferir_coerencia(if saida.restaurar { &vazio } else { vigente }, &saida.ajuste)?;
    Ok(saida)
}

/// A coerência do ajuste resultante (`vigente` com `parcial` por cima), só para os campos pedidos.
fn conferir_coerencia(vigente: &Objeto, parcial: &Objeto) -> std::result::Result<(), Recusa> {
    let mut resultado = vigente.clone();
    for (k, v) in parcial {
        resultado.insert(k.clone(), v.clone());
    }
    for (campo, modo_de, exigido) in COERENCIA {
        if parcial.contains_key(*campo) {
            // Uma trava desligada não exige modo nenhum.
            if parcial.get(*campo) == Some(&Value::Bool(false)) {
                continue;
            }
            if modo(&resultado, modo_de) != *exigido {
                return Err(Recusa::de(motivo::INCOERENTE, Some(campo)));
            }
        }
    }
    // O EV fica apagado com a trava de exposição ligada (R9 §3.3).
    if parcial.contains_key("ev") && resultado.get("travaExposicao") == Some(&Value::Bool(true)) {
        return Err(Recusa::de(motivo::INCOERENTE, Some("ev")));
    }
    Ok(())
}

/// Lê um objeto JSON da casca, com teto.
fn ler_objeto(json: &str, teto: usize, nome: &str) -> Result<Objeto> {
    if json.len() > teto {
        return Err(Error::Invalid(format!("{nome}: {} bytes passam do teto de {teto}", json.len())));
    }
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(Error::Invalid(format!("{nome}: não é um objeto JSON"))),
        Err(e) => Err(Error::Invalid(format!("{nome}: JSON ilegível: {e}"))),
    }
}

/// Confere a forma das capacidades que a casca do filmador publica (e que o receptor recebe).
fn conferir_capacidades(c: &Objeto) -> Result<()> {
    let controles = c
        .get("controles")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Invalid("capacidades: falta o objeto \"controles\"".into()))?;
    for (campo, d) in controles {
        if campo.is_empty() || campo.len() > TETO_DO_CODIGO || descritor(d).is_none() {
            return Err(Error::Invalid(format!("capacidades: o descritor de \"{campo}\" não tem forma conhecida")));
        }
    }
    if let Some(limites) = c.get("limites") {
        let limites = limites
            .as_object()
            .ok_or_else(|| Error::Invalid("capacidades: \"limites\" não é objeto".into()))?;
        for (campo, codigo) in limites {
            if !codigo.as_str().is_some_and(codigo_valido) {
                return Err(Error::Invalid(format!("capacidades: o limite de \"{campo}\" não é um código")));
            }
        }
    }
    if let Some(nome) = c.get("nomeDaCamera") {
        if !nome.as_str().is_some_and(|n| n.len() <= TETO_DO_NOME) {
            return Err(Error::Invalid("capacidades: \"nomeDaCamera\" não é texto de até 64 bytes".into()));
        }
    }
    Ok(())
}

/// **Dois valores do registro são o mesmo?** `null` e ausente são iguais (o Android grava `null`
/// explícito, o iOS e o Windows omitem), e número se compara por valor (`800` e `800.0`).
fn igual(a: Option<&Value>, b: Option<&Value>) -> bool {
    let a = a.filter(|v| !v.is_null());
    let b = b.filter(|v| !v.is_null());
    match (a, b) {
        (None, None) => true,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Corta um nome em [`TETO_DO_NOME`] bytes, numa fronteira de caractere, sem caracteres de
/// controle: o nome vem do anúncio do outro lado e vai para a tela de todos.
fn nome_curto(nome: &str) -> String {
    let nome: String = nome.chars().filter(|c| !c.is_control()).collect();
    if nome.len() <= TETO_DO_NOME {
        return nome;
    }
    let mut fim = TETO_DO_NOME;
    while !nome.is_char_boundary(fim) {
        fim -= 1;
    }
    nome[..fim].to_string()
}

/// Lê o envelope comum. `Err` diz em que contador a mensagem cai.
fn envelope(texto: &str) -> std::result::Result<(Objeto, String), Descarte> {
    if texto.len() > TETO_DA_MENSAGEM_DA_CAMERA {
        return Err(Descarte::Invalida);
    }
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(texto) else {
        return Err(Descarte::Invalida);
    };
    if o.get("app").and_then(Value::as_str) != Some(APP) {
        return Err(Descarte::DeOutroApp);
    }
    if o.get("v").and_then(Value::as_u64) != Some(VERSAO) {
        return Err(Descarte::DeOutraVersao);
    }
    let Some(tipo) = o.get("tipo").and_then(Value::as_str).map(str::to_string) else {
        return Err(Descarte::Invalida);
    };
    Ok((o, tipo))
}

/// Por que uma mensagem não entrou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Descarte {
    Invalida,
    DeOutroApp,
    DeOutraVersao,
}

/// Uma mensagem com o envelope, na ordem fixa das chaves (`serde_json` sem `preserve_order` ordena
/// as chaves; o formato é conferido pelos testes, não pela ordem).
fn mensagem(tipo: &str, corpo: Objeto) -> String {
    let mut o = Objeto::new();
    o.insert("app".into(), Value::from(APP));
    o.insert("v".into(), Value::from(VERSAO));
    o.insert("tipo".into(), Value::from(tipo));
    o.extend(corpo);
    Value::Object(o).to_string()
}

fn sorteia_epoca() -> String {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        // Sem entropia, a época ainda precisa mudar entre execuções: o relógio serve.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
            .unwrap_or(0);
        b = t.to_be_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

// =============================================================================================
// Os contadores
// =============================================================================================

/// O que passou pelo filmador. Os nomes são os do JSON de `quall_camera_host_state_json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ContadoresDoFilmador {
    pub recebidas: u64,
    pub invalidas: u64,
    pub de_outro_app: u64,
    pub de_outra_versao: u64,
    pub tipo_desconhecido: u64,
    pub pedidos_aceitos: u64,
    pub pedidos_recusados: u64,
    pub campos_superados: u64,
    pub reenvios: u64,
    pub estados_enviados: u64,
    pub mensagens_impossiveis: u64,
    pub sessoes_demais: u64,
}

/// O que passou pelo receptor. Os nomes são os do JSON de `quall_camera_remote_state_json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ContadoresDoReceptor {
    pub recebidas: u64,
    pub invalidas: u64,
    pub de_outro_app: u64,
    pub de_outra_versao: u64,
    pub tipo_desconhecido: u64,
    pub pedidos_enviados: u64,
    pub reenvios: u64,
    pub recusas: u64,
    pub desistencias: u64,
    pub olas_enviados: u64,
    pub mensagens_impossiveis: u64,
}

// =============================================================================================
// O filmador
// =============================================================================================

/// O que o filmador sabe de cada sessão.
#[derive(Debug, Clone, Default)]
struct SessaoNoFilmador {
    nome: String,
    id: String,
    pareada: bool,
    /// Disse `ola`: só a quem disse se manda alguma coisa.
    ouvindo: bool,
    capacidades_devidas: bool,
    /// O `cap` das últimas capacidades mandadas, e quando.
    cap_enviada: u64,
    ultimas_capacidades: Option<Instant>,
    estado_devido: bool,
    lido_devido: bool,
    /// Um reenvio pediu o recibo de novo: sai com o estado, no ritmo do lido.
    recibo_devido: bool,
    ultimo_estado: Option<Instant>,
    /// O maior `seq` que chegou desta sessão.
    seq_visto: u64,
    /// O recibo: o último `seq` tratado, e a recusa dele.
    recibo_seq: u64,
    recibo_recusa: Option<Recusa>,
    recusas_devidas: Vec<(u64, Recusa)>,
}

/// Um pedido aceito, esperando a casca.
#[derive(Debug, Clone)]
struct PedidoAceito {
    n: u64,
    sessao: u64,
    seq: u64,
    /// A `vista` de cada campo (contrato §4): a versão que o receptor tinha quando a pessoa mexeu
    /// **naquele** campo.
    vistas: BTreeMap<String, u64>,
    pedido: PedidoConferido,
}

/// Um pedido que a casca tirou e ainda não respondeu.
#[derive(Debug, Clone)]
struct EmAplicacao {
    sessao: u64,
    seq: u64,
    nome: String,
    desde: Instant,
}

/// O que o filmador deve mandar a uma sessão (para devolver a dívida se o envio falhar).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Devida {
    Capacidades,
    Estado,
    Recusa(u64, Recusa),
}

/// **O filmador, sem cadeado**: a máquina pura, que recebe o instante de fora. Ver o topo do
/// módulo e o [`Filmador`].
#[derive(Debug)]
pub struct NucleoDoFilmador {
    epoca: String,
    permite: bool,
    /// **Qual câmera** está em uso (sobe a cada troca); 0 = sem câmera.
    camera: u64,
    /// **A revisão das capacidades** (sobe a cada troca de câmera e a cada faixa nova).
    cap: u64,
    contador: u64,
    capacidades: Option<Objeto>,
    ajuste: Objeto,
    lido: Objeto,
    versao: u64,
    /// Por campo: a `versao` em que mudou pela última vez e quem mudou (0 = o filmador).
    campos: BTreeMap<String, (u64, u64)>,
    autor: Option<String>,
    n: u64,
    proximo_n: u64,
    pedidos: VecDeque<PedidoAceito>,
    em_aplicacao: BTreeMap<u64, EmAplicacao>,
    controlado_por: Option<(String, Instant)>,
    sessoes: BTreeMap<u64, SessaoNoFilmador>,
    contadores: ContadoresDoFilmador,
    mudancas: Mudancas,
}

impl Default for NucleoDoFilmador {
    fn default() -> Self {
        Self::novo()
    }
}

impl NucleoDoFilmador {
    /// Um filmador sem câmera, **com a permissão desligada** (o padrão do contrato).
    pub fn novo() -> NucleoDoFilmador {
        NucleoDoFilmador {
            epoca: sorteia_epoca(),
            permite: false,
            camera: 0,
            cap: 0,
            contador: 0,
            capacidades: None,
            ajuste: Objeto::new(),
            lido: Objeto::new(),
            versao: 0,
            campos: BTreeMap::new(),
            autor: None,
            n: 0,
            proximo_n: 1,
            pedidos: VecDeque::new(),
            em_aplicacao: BTreeMap::new(),
            controlado_por: None,
            sessoes: BTreeMap::new(),
            contadores: ContadoresDoFilmador::default(),
            mudancas: 0,
        }
    }

    /// A época deste filmador (8 dígitos hex).
    pub fn epoca(&self) -> &str {
        &self.epoca
    }

    /// A versão do ajuste.
    pub fn versao(&self) -> u64 {
        self.versao
    }

    /// Qual câmera está em uso (0 = nenhuma).
    pub fn camera(&self) -> u64 {
        self.camera
    }

    fn todos_devem_o_estado(&mut self) {
        for s in self.sessoes.values_mut() {
            s.estado_devido = true;
        }
    }

    fn recusar_na_sessao(&mut self, sessao: u64, seq: u64, r: Recusa) {
        self.contadores.pedidos_recusados += 1;
        if let Some(s) = self.sessoes.get_mut(&sessao) {
            if seq >= s.recibo_seq {
                s.recibo_seq = seq;
                s.recibo_recusa = Some(r.clone());
            }
            s.recusas_devidas.push((seq, r));
            s.estado_devido = true;
        }
    }

    /// O recibo sem recusa: o pedido `seq` desta sessão foi tratado.
    fn dar_recibo(&mut self, sessao: u64, seq: u64) {
        if let Some(s) = self.sessoes.get_mut(&sessao) {
            if seq >= s.recibo_seq {
                s.recibo_seq = seq;
                s.recibo_recusa = None;
            }
            s.estado_devido = true;
        }
    }

    /// Liga ou desliga a opção "Permitir controle remoto da câmera". Desligar recusa a fila.
    pub fn permitir(&mut self, permite: bool) {
        if self.permite == permite {
            return;
        }
        self.permite = permite;
        if !permite {
            for p in std::mem::take(&mut self.pedidos) {
                self.recusar_na_sessao(p.sessao, p.seq, Recusa::de(motivo::NAO_PERMITIDO, None));
            }
        }
        self.todos_devem_o_estado();
    }

    fn ler_camera(caps: &str, ajuste: &str) -> Result<(Objeto, Objeto)> {
        let c = ler_objeto(caps, TETO_DAS_CAPACIDADES, "capacidades")?;
        conferir_capacidades(&c)?;
        let a = ler_objeto(ajuste, TETO_DO_AJUSTE, "ajuste")?;
        Ok((c, a))
    }

    /// **A câmera em uso**: as capacidades e o registro dela, ou `None` (sem câmera). Sobe a
    /// câmera, o `cap` e a `versao`, e recusa com `camera_trocada` o que esperava a casca. Para as
    /// faixas que mudam **sem** trocar de câmera, use [`NucleoDoFilmador::definir_capacidades`].
    pub fn definir_camera(&mut self, camera: Option<(&str, &str)>) -> Result<()> {
        let novo = match camera {
            Some((caps, ajuste)) => Some(Self::ler_camera(caps, ajuste)?),
            None => None,
        };
        for p in std::mem::take(&mut self.pedidos) {
            self.recusar_na_sessao(p.sessao, p.seq, Recusa::de(motivo::CAMERA_TROCADA, None));
        }
        for (_, e) in std::mem::take(&mut self.em_aplicacao) {
            self.recusar_na_sessao(e.sessao, e.seq, Recusa::de(motivo::CAMERA_TROCADA, None));
        }
        self.versao += 1;
        self.campos.clear();
        self.autor = None;
        self.lido = Objeto::new();
        self.contador += 1;
        self.cap = self.contador;
        match novo {
            Some((c, a)) => {
                self.camera = self.contador;
                self.capacidades = Some(c);
                self.ajuste = a;
            }
            None => {
                self.camera = 0;
                self.capacidades = None;
                self.ajuste = Objeto::new();
            }
        }
        for s in self.sessoes.values_mut() {
            s.capacidades_devidas = true;
            s.estado_devido = true;
        }
        Ok(())
    }

    /// **As faixas mudaram, a câmera é a mesma** (o fps que muda o teto do obturador, o degrau de
    /// calor do iOS). Sobe só o `cap`: os pedidos em trânsito continuam valendo, conferidos contra
    /// a faixa nova.
    pub fn definir_capacidades(&mut self, caps: &str) -> Result<()> {
        if self.camera == 0 {
            return Err(Error::Invalid("o filmador não tem câmera: chame definir_camera antes".into()));
        }
        let c = ler_objeto(caps, TETO_DAS_CAPACIDADES, "capacidades")?;
        conferir_capacidades(&c)?;
        if self.capacidades.as_ref() == Some(&c) {
            return Ok(());
        }
        self.contador += 1;
        self.cap = self.contador;
        self.capacidades = Some(c);
        for s in self.sessoes.values_mut() {
            s.capacidades_devidas = true;
            s.estado_devido = true;
        }
        Ok(())
    }

    /// **O registro que ficou valendo.** `pedido` é:
    /// - `0`: mudança local (o painel do filmador, o toque na prévia dele);
    /// - o `n` de um pedido que a casca tirou com [`NucleoDoFilmador::proximo_pedido_se`];
    /// - [`AJUSTE_DO_SISTEMA`]: a casca escreveu sozinha (o lido da trava).
    pub fn definir_ajuste(&mut self, json: &str, pedido: u64, agora: Instant) -> Result<()> {
        if self.camera == 0 {
            return Err(Error::Invalid("o filmador não tem câmera: chame definir_camera antes".into()));
        }
        let novo = ler_objeto(json, TETO_DO_AJUSTE, "ajuste")?;
        let quem = if pedido == 0 || pedido == AJUSTE_DO_SISTEMA {
            None
        } else {
            Some(self.em_aplicacao.remove(&pedido).ok_or_else(|| {
                Error::Invalid(format!(
                    "o pedido {pedido} não está em aplicação (já respondido, vencido, ou a câmera trocou)"
                ))
            })?)
        };
        let mut chaves: Vec<&String> = novo.keys().chain(self.ajuste.keys()).collect();
        chaves.sort();
        chaves.dedup();
        let mudados: Vec<String> = chaves
            .into_iter()
            .filter(|k| !igual(novo.get(*k), self.ajuste.get(*k)))
            .cloned()
            .collect();
        if !mudados.is_empty() {
            self.versao += 1;
            if pedido != AJUSTE_DO_SISTEMA {
                let autor_sessao = quem.as_ref().map(|e| e.sessao).unwrap_or(0);
                let controles = self
                    .capacidades
                    .as_ref()
                    .and_then(|c| c.get("controles"))
                    .and_then(Value::as_object);
                for k in mudados {
                    // Só os campos que um receptor pode pedir disputam o "vence o último"; os de
                    // leitura (`travaIso`, `travaGanhos`…) não têm dono.
                    if controles.is_some_and(|c| c.contains_key(&k)) {
                        self.campos.insert(k, (self.versao, autor_sessao));
                    }
                }
                self.autor = quem.as_ref().map(|e| e.nome.clone());
            }
            self.ajuste = novo;
            self.todos_devem_o_estado();
        }
        if let Some(e) = quem {
            self.dar_recibo(e.sessao, e.seq);
            self.controlado_por = Some((e.nome, agora));
        }
        Ok(())
    }

    /// O que a câmera diz estar usando (R9 §3.6), com `divergentes` dentro.
    pub fn definir_lido(&mut self, json: &str) -> Result<()> {
        let novo = ler_objeto(json, TETO_DO_LIDO, "lido")?;
        if novo != self.lido {
            self.lido = novo;
            for s in self.sessoes.values_mut() {
                s.lido_devido = true;
            }
        }
        Ok(())
    }

    /// A casca não conseguiu aplicar o pedido `n`. `codigo` é um código; um que o receptor não
    /// conheça ele mostra como `nao_aplicado`.
    pub fn recusar(&mut self, n: u64, codigo: &str) -> Result<()> {
        if !codigo_valido(codigo) {
            return Err(Error::Invalid(format!("\"{codigo}\" não é um código ([a-z0-9_], até 32 bytes)")));
        }
        let e = self
            .em_aplicacao
            .remove(&n)
            .ok_or_else(|| Error::Invalid(format!("o pedido {n} não está em aplicação")))?;
        self.recusar_na_sessao(e.sessao, e.seq, Recusa::de(codigo, None));
        Ok(())
    }

    /// Filtra os campos pelo "vence o último" (contrato §4): fica o campo que ninguém mais mudou
    /// depois da `vista` **dele**, ou que a própria sessão mudou por último. Devolve o primeiro que
    /// caiu e quantos caíram; quem conta é quem decide.
    fn filtrar_superados(&self, sessao: u64, vistas: &BTreeMap<String, u64>, ajuste: &mut Objeto) -> (Option<String>, u64) {
        let mut primeiro = None;
        let mut caidos = 0;
        ajuste.retain(|k, _| {
            let vista = vistas.get(k).copied().unwrap_or(0);
            let fica = match self.campos.get(k) {
                None => true,
                Some((versao, autor)) => *versao <= vista || (*autor == sessao && *autor != 0),
            };
            if !fica {
                caidos += 1;
                if primeiro.is_none() {
                    primeiro = Some(k.clone());
                }
            }
            fica
        });
        (primeiro, caidos)
    }

    /// O "vence o último" e a coerência de depois dele, juntos: um pedido cujos campos caíram e que
    /// por isso ficou incoerente é `superado`, não `incoerente` (o motivo é o outro ter mexido).
    fn decidir(
        &self,
        sessao: u64,
        vistas: &BTreeMap<String, u64>,
        p: &PedidoConferido,
    ) -> std::result::Result<(Objeto, u64), Recusa> {
        let mut ajuste = p.ajuste.clone();
        let tinha = ajuste.len();
        let (primeiro, caidos) = self.filtrar_superados(sessao, vistas, &mut ajuste);
        let sem_acoes = !p.restaurar && p.toque.is_none();
        if tinha > 0 && ajuste.is_empty() && sem_acoes {
            return Err(Recusa::de(motivo::SUPERADO, primeiro.as_deref()));
        }
        let vazio = Objeto::new();
        let base = if p.restaurar { &vazio } else { &self.ajuste };
        if let Err(r) = conferir_coerencia(base, &ajuste) {
            return Err(if caidos > 0 { Recusa::de(motivo::SUPERADO, primeiro.as_deref()) } else { r });
        }
        Ok((ajuste, caidos))
    }

    /// Recebe uma mensagem de uma sessão. `par` é quem é o outro lado (do aperto de mão).
    pub fn receber(&mut self, sessao: u64, par: Option<&ParDaSessao>, texto: &str, agora: Instant) -> Mudancas {
        let (o, tipo) = match envelope(texto) {
            Ok(x) => x,
            Err(d) => {
                match d {
                    Descarte::Invalida => self.contadores.invalidas += 1,
                    Descarte::DeOutroApp => self.contadores.de_outro_app += 1,
                    Descarte::DeOutraVersao => self.contadores.de_outra_versao += 1,
                }
                return 0;
            }
        };
        self.contadores.recebidas += 1;
        if !self.sessoes.contains_key(&sessao) {
            if self.sessoes.len() >= TETO_DE_SESSOES {
                self.contadores.sessoes_demais += 1;
                return 0;
            }
            let mut s = SessaoNoFilmador::default();
            if let Some(p) = par {
                s.nome = nome_curto(&p.nome);
                s.id = nome_curto(&p.id);
                s.pareada = true;
            }
            self.sessoes.insert(sessao, s);
        }
        match tipo.as_str() {
            "ola" => self.receber_ola(sessao, &o),
            "pedido" => self.receber_pedido(sessao, &o),
            _ => {
                self.contadores.tipo_desconhecido += 1;
                let _ = agora;
                0
            }
        }
    }

    fn receber_ola(&mut self, sessao: u64, o: &Objeto) -> Mudancas {
        let cap_que_tem = o.get("cap").and_then(Value::as_u64).unwrap_or(0);
        let cap = self.cap;
        let Some(s) = self.sessoes.get_mut(&sessao) else { return 0 };
        let novo = !s.ouvindo;
        s.ouvindo = true;
        if novo {
            s.estado_devido = true;
        } else {
            s.recibo_devido = true;
        }
        if cap_que_tem != cap {
            s.capacidades_devidas = true;
        }
        if novo {
            self.mudancas |= mudou_no_filmador::RECEPTORES;
            mudou_no_filmador::RECEPTORES
        } else {
            0
        }
    }

    fn receber_pedido(&mut self, sessao: u64, o: &Objeto) -> Mudancas {
        let Some(seq) = o.get("seq").and_then(Value::as_u64).filter(|s| *s > 0) else {
            self.contadores.invalidas += 1;
            return 0;
        };
        let Some(s) = self.sessoes.get_mut(&sessao) else { return 0 };
        // Quem pede está ouvindo, mesmo que o `ola` dele tenha se perdido.
        if !s.ouvindo {
            s.ouvindo = true;
            s.capacidades_devidas = true;
            s.estado_devido = true;
            self.mudancas |= mudou_no_filmador::RECEPTORES;
        }
        if seq <= s.seq_visto {
            // Reenvio: só repete o recibo, no ritmo do lido (um reenvio de 50 bytes não compra um
            // estado inteiro na hora).
            s.recibo_devido = true;
            self.contadores.reenvios += 1;
            return 0;
        }
        s.seq_visto = seq;
        let pareada = s.pareada;
        let recusa = |m: &str| Recusa::de(m, None);
        if !pareada {
            self.recusar_na_sessao(sessao, seq, recusa(motivo::NAO_PAREADO));
            return 0;
        }
        if !self.permite {
            self.recusar_na_sessao(sessao, seq, recusa(motivo::NAO_PERMITIDO));
            return 0;
        }
        if self.camera == 0 {
            self.recusar_na_sessao(sessao, seq, recusa(motivo::SEM_CAMERA));
            return 0;
        }
        let epoca_ok = o.get("epoca").and_then(Value::as_str) == Some(self.epoca.as_str());
        let camera_ok = o.get("camera").and_then(Value::as_u64) == Some(self.camera);
        if !epoca_ok || !camera_ok {
            self.recusar_na_sessao(sessao, seq, recusa(motivo::CAMERA_TROCADA));
            return 0;
        }
        let caps = self.capacidades.clone().unwrap_or_default();
        let mut conferido =
            match conferir_pedido(&caps, &self.ajuste, o.get("ajuste"), o.get("restaurar"), o.get("toque")) {
                Ok(p) => p,
                Err(r) => {
                    self.recusar_na_sessao(sessao, seq, r);
                    return 0;
                }
            };
        let vista = o.get("vista").and_then(Value::as_u64).unwrap_or(0);
        let vistas_do_fio = o.get("vistas").and_then(Value::as_object);
        let mut vistas: BTreeMap<String, u64> = conferido
            .ajuste
            .keys()
            .map(|k| {
                let v = vistas_do_fio.and_then(|v| v.get(k)).and_then(Value::as_u64).unwrap_or(vista);
                (k.clone(), v)
            })
            .collect();
        // O campo igual ao aplicado não muda nada: sai do pedido sem disputar nem subir versão (o
        // OBS que reabre com os valores salvos na fonte não mexe na câmera de ninguém).
        if !conferido.restaurar {
            let vigente = &self.ajuste;
            conferido.ajuste.retain(|k, v| !igual(vigente.get(k), Some(v)));
            if conferido.ajuste.is_empty() && conferido.toque.is_none() {
                self.dar_recibo(sessao, seq);
                return 0;
            }
        }
        let caidos = match self.decidir(sessao, &vistas, &conferido) {
            Ok((ajuste, caidos)) => {
                conferido.ajuste = ajuste;
                caidos
            }
            Err(r) => {
                self.recusar_na_sessao(sessao, seq, r);
                return 0;
            }
        };
        self.contadores.campos_superados += caidos;
        // Um pedido novo da mesma sessão substitui o anterior dela na fila, juntando: o receptor
        // já manda tudo o que não foi confirmado, mas uma ação perdida no caminho não pode sumir.
        if let Some(i) = self.pedidos.iter().position(|p| p.sessao == sessao) {
            if let Some(velho) = self.pedidos.remove(i) {
                if !conferido.restaurar {
                    for (k, v) in velho.pedido.ajuste {
                        if !conferido.ajuste.contains_key(&k) {
                            if let Some(vv) = velho.vistas.get(&k) {
                                vistas.insert(k.clone(), *vv);
                            }
                            conferido.ajuste.insert(k, v);
                        }
                    }
                    conferido.restaurar |= velho.pedido.restaurar;
                }
                if conferido.toque.is_none() {
                    conferido.toque = velho.pedido.toque;
                }
            }
        }
        if self.pedidos.len() >= FILA_DE_PEDIDOS {
            self.recusar_na_sessao(sessao, seq, recusa(motivo::OCUPADO));
            return 0;
        }
        let n = self.proximo_n;
        self.proximo_n += 1;
        self.pedidos.push_back(PedidoAceito { n, sessao, seq, vistas, pedido: conferido });
        self.contadores.pedidos_aceitos += 1;
        self.mudancas |= mudou_no_filmador::PEDIDO;
        mudou_no_filmador::PEDIDO
    }

    /// **A espiada da fila de pedidos**: mostra o próximo pedido aceito a `aceitar`, e só o tira se
    /// ela devolver `true`. Antes de mostrar, confere de novo (a permissão e o "vence o último"):
    /// uma mudança local feita enquanto o pedido esperava na fila vence, porque foi depois. Devolve
    /// o tamanho do JSON mostrado, ou `None` com a fila vazia.
    pub fn proximo_pedido_se(&mut self, aceitar: impl FnOnce(&str) -> bool, agora: Instant) -> Option<usize> {
        loop {
            let p = self.pedidos.front()?.clone();
            let decidido = if self.permite {
                self.decidir(p.sessao, &p.vistas, &p.pedido)
            } else {
                Err(Recusa::de(motivo::NAO_PERMITIDO, None))
            };
            let (ajuste, caidos) = match decidido {
                Ok(x) => x,
                Err(r) => {
                    self.pedidos.pop_front();
                    self.recusar_na_sessao(p.sessao, p.seq, r);
                    continue;
                }
            };
            let s = self.sessoes.get(&p.sessao);
            let nome = s.map(|s| s.nome.clone()).unwrap_or_default();
            let id = s.map(|s| s.id.clone()).unwrap_or_default();
            let texto = json!({
                "n": p.n,
                "autor": nome,
                "autor_id": id,
                "ajuste": Value::Object(ajuste),
                "restaurar": p.pedido.restaurar,
                "toque": p.pedido.toque,
            })
            .to_string();
            let tamanho = texto.len();
            if aceitar(&texto) {
                self.pedidos.pop_front();
                self.contadores.campos_superados += caidos;
                self.em_aplicacao.insert(p.n, EmAplicacao { sessao: p.sessao, seq: p.seq, nome, desde: agora });
            }
            return Some(tamanho);
        }
    }

    /// O próximo pedido aceito, tirado da fila. Ver [`NucleoDoFilmador::proximo_pedido_se`].
    pub fn proximo_pedido(&mut self, agora: Instant) -> Option<String> {
        let mut saida = None;
        self.proximo_pedido_se(
            |t| {
                saida = Some(t.to_string());
                true
            },
            agora,
        );
        saida
    }

    /// A sessão acabou: esquece o que sabia dela. Os pedidos dela que esperam a casca ficam (o
    /// aparelho pediu antes de sair).
    pub fn esquecer_sessao(&mut self, sessao: u64) {
        if let Some(s) = self.sessoes.remove(&sessao) {
            if s.ouvindo {
                self.mudancas |= mudou_no_filmador::RECEPTORES;
            }
        }
    }

    /// Recusa o que a casca tirou e não respondeu em [`PRAZO_DA_CASCA`].
    fn vencer_a_casca(&mut self, agora: Instant) {
        let vencidos: Vec<u64> = self
            .em_aplicacao
            .iter()
            .filter(|(_, e)| agora.duration_since(e.desde) >= PRAZO_DA_CASCA)
            .map(|(n, _)| *n)
            .collect();
        for n in vencidos {
            if let Some(e) = self.em_aplicacao.remove(&n) {
                self.recusar_na_sessao(e.sessao, e.seq, Recusa::de(motivo::NAO_APLICADO, None));
            }
        }
    }

    fn corpo_das_capacidades(&self) -> Objeto {
        let mut o = Objeto::new();
        o.insert("epoca".into(), Value::from(self.epoca.as_str()));
        o.insert("camera".into(), Value::from(self.camera));
        o.insert("cap".into(), Value::from(self.cap));
        o.insert(
            "capacidades".into(),
            self.capacidades.clone().map(Value::Object).unwrap_or(Value::Null),
        );
        o
    }

    fn corpo_do_estado(&self, s: &SessaoNoFilmador) -> Objeto {
        let mut o = Objeto::new();
        o.insert("epoca".into(), Value::from(self.epoca.as_str()));
        o.insert("n".into(), Value::from(self.n));
        o.insert("versao".into(), Value::from(self.versao));
        o.insert("camera".into(), Value::from(self.camera));
        o.insert("cap".into(), Value::from(self.cap));
        o.insert("permite".into(), Value::from(self.permite));
        o.insert("ajuste".into(), Value::Object(self.ajuste.clone()));
        o.insert("lido".into(), Value::Object(self.lido.clone()));
        o.insert("autor".into(), self.autor.clone().map(Value::from).unwrap_or(Value::Null));
        o.insert("seu".into(), json!({"seq": s.recibo_seq, "recusa": s.recibo_recusa}));
        o
    }

    /// O que está devido a uma sessão agora, já como texto, e a dívida apagada. Se o envio falhar
    /// por "ainda não abriu", devolva com [`NucleoDoFilmador::devolver`].
    pub(crate) fn devidas(&mut self, sessao: u64, agora: Instant) -> Vec<(Devida, String)> {
        self.vencer_a_casca(agora);
        let mut saida = Vec::new();
        let cap = self.cap;
        let Some(s) = self.sessoes.get(&sessao) else { return saida };
        if !s.ouvindo {
            return saida;
        }
        let passou = |t: Option<Instant>, d: Duration| t.is_none_or(|t| agora.duration_since(t) >= d);
        let batimento = passou(s.ultimo_estado, BATIMENTO);
        let ritmo = (s.lido_devido || s.recibo_devido) && passou(s.ultimo_estado, INTERVALO_DO_LIDO);
        let quer_estado = s.estado_devido || ritmo || batimento;
        let quer_caps = s.capacidades_devidas
            && (s.cap_enviada != cap || passou(s.ultimas_capacidades, INTERVALO_DAS_CAPACIDADES));
        for (seq, r) in s.recusas_devidas.clone() {
            let mut corpo = Objeto::new();
            corpo.insert("seq".into(), Value::from(seq));
            corpo.insert("motivo".into(), Value::from(r.motivo.as_str()));
            corpo.insert("campo".into(), r.campo.clone().map(Value::from).unwrap_or(Value::Null));
            saida.push((Devida::Recusa(seq, r), mensagem("recusa", corpo)));
        }
        if quer_caps {
            saida.push((Devida::Capacidades, mensagem("capacidades", self.corpo_das_capacidades())));
        }
        if quer_estado {
            self.n += 1;
            let s = self.sessoes.get(&sessao).cloned().unwrap_or_default();
            saida.push((Devida::Estado, mensagem("estado", self.corpo_do_estado(&s))));
        }
        if let Some(s) = self.sessoes.get_mut(&sessao) {
            s.recusas_devidas.clear();
            if quer_caps {
                s.capacidades_devidas = false;
                s.cap_enviada = cap;
                s.ultimas_capacidades = Some(agora);
            }
            if quer_estado {
                s.estado_devido = false;
                s.lido_devido = false;
                s.recibo_devido = false;
                s.ultimo_estado = Some(agora);
            }
        }
        // Uma mensagem acima do teto nunca sai, e nunca trava as outras.
        let antes = saida.len();
        saida.retain(|(_, t)| t.len() <= TETO_DA_MENSAGEM_DA_CAMERA);
        self.contadores.mensagens_impossiveis += (antes - saida.len()) as u64;
        self.contadores.estados_enviados += saida.iter().filter(|(d, _)| *d == Devida::Estado).count() as u64;
        saida
    }

    /// Devolve uma dívida cujo envio falhou porque o canal ainda não abriu.
    pub(crate) fn devolver(&mut self, sessao: u64, d: Devida) {
        if let Some(s) = self.sessoes.get_mut(&sessao) {
            match d {
                Devida::Capacidades => {
                    s.capacidades_devidas = true;
                    s.cap_enviada = 0;
                }
                Devida::Estado => s.estado_devido = true,
                // A recusa perdida vai no recibo do estado seguinte.
                Devida::Recusa(..) => s.estado_devido = true,
            }
        }
    }

    /// Os bits do que mudou desde a última vez, apagados.
    pub fn tirar_mudancas(&mut self) -> Mudancas {
        std::mem::take(&mut self.mudancas)
    }

    /// O estado para a tela do filmador (`quall_camera_host_state_json`).
    pub fn estado_json(&self, agora: Instant) -> String {
        let controlado = self
            .controlado_por
            .as_ref()
            .filter(|(_, t)| agora.duration_since(*t) < CONTROLADO_POR_DURA)
            .map(|(nome, t)| json!({"nome": nome, "ha_ms": ms(agora.duration_since(*t))}));
        let receptores: Vec<Value> = self
            .sessoes
            .values()
            .filter(|s| s.ouvindo)
            .map(|s| json!({"nome": s.nome, "id": s.id}))
            .collect();
        json!({
            "permite": self.permite,
            "camera": self.camera,
            "cap": self.cap,
            "versao": self.versao,
            "controlado_por": controlado,
            "receptores": receptores,
            "pedidos_na_fila": self.pedidos.len(),
            "contadores": self.contadores,
        })
        .to_string()
    }

    /// Os contadores.
    pub fn contadores(&self) -> ContadoresDoFilmador {
        self.contadores
    }

    /// O ajuste vigente.
    pub fn ajuste(&self) -> &Map<String, Value> {
        &self.ajuste
    }
}

// =============================================================================================
// O receptor
// =============================================================================================

/// O estado que chegou do filmador.
#[derive(Debug, Clone)]
struct EstadoRecebido {
    n: u64,
    versao: u64,
    camera: u64,
    cap: u64,
    permite: bool,
    ajuste: Objeto,
    lido: Objeto,
    autor: Option<String>,
}

/// **O receptor, sem cadeado**: a máquina pura. Ver o topo do módulo e o [`Controlador`].
#[derive(Debug, Default)]
pub struct NucleoDoReceptor {
    sessao: Option<u64>,
    comecou: Option<Instant>,
    epoca: Option<String>,
    capacidades: Option<Objeto>,
    cap_das_capacidades: u64,
    estado: Option<EstadoRecebido>,
    ouvido: Option<Instant>,
    pendente: Objeto,
    /// A `vista` de cada campo pendente: a versão que havia quando a pessoa mexeu **nele**.
    vistas: BTreeMap<String, u64>,
    restaurar: bool,
    toque: Option<Toque>,
    seq: u64,
    seq_pendente: u64,
    vista_pendente: u64,
    ultima_edicao: Option<Instant>,
    ultimo_envio: Option<Instant>,
    enviado_seq: u64,
    ultimo_ola: Option<Instant>,
    ola_devido: bool,
    recusa: Option<(Recusa, Instant)>,
    situacao: &'static str,
    contadores: ContadoresDoReceptor,
    mudancas: Mudancas,
}

impl NucleoDoReceptor {
    /// Um receptor sem sessão.
    pub fn novo() -> NucleoDoReceptor {
        NucleoDoReceptor { situacao: situacao::ESPERANDO, ..Default::default() }
    }

    /// Começa (ou recomeça) para a sessão `sessao`: esquece tudo o que sabia de outra.
    pub fn comecar_sessao(&mut self, sessao: u64, agora: Instant) {
        if self.sessao == Some(sessao) {
            return;
        }
        let contadores = self.contadores;
        let seq = self.seq;
        *self = NucleoDoReceptor::novo();
        self.contadores = contadores;
        // O `seq` só sobe, mesmo entre sessões: um filmador que de algum modo visse as duas nunca
        // tomaria o pedido novo por reenvio.
        self.seq = seq;
        self.sessao = Some(sessao);
        self.comecou = Some(agora);
        self.ola_devido = true;
        self.mudancas |= mudou_no_receptor::CAPACIDADES | mudou_no_receptor::AJUSTE | mudou_no_receptor::LIDO;
    }

    fn esquecer_o_filmador(&mut self) {
        self.capacidades = None;
        self.cap_das_capacidades = 0;
        self.estado = None;
        self.largar_pendente();
        self.mudancas |= mudou_no_receptor::CAPACIDADES | mudou_no_receptor::AJUSTE | mudou_no_receptor::LIDO;
    }

    fn largar_pendente(&mut self) {
        self.pendente.clear();
        self.vistas.clear();
        self.restaurar = false;
        self.toque = None;
        self.seq_pendente = 0;
        self.ultima_edicao = None;
    }

    fn tem_pendente(&self) -> bool {
        self.seq_pendente != 0
    }

    /// A situação para a tela.
    fn calcular_situacao(&self, agora: Instant) -> &'static str {
        match &self.estado {
            None => {
                let passou = self.comecou.is_some_and(|c| agora.duration_since(c) >= SEM_FILMADOR);
                if passou { situacao::SEM_RESPOSTA } else { situacao::ESPERANDO }
            }
            Some(e) => {
                if self.ouvido.is_some_and(|t| agora.duration_since(t) >= SEM_FILMADOR) {
                    situacao::SEM_RESPOSTA
                } else if e.camera == 0 {
                    situacao::SEM_CAMERA
                } else if self.capacidades.is_none() || self.cap_das_capacidades != e.cap {
                    situacao::ESPERANDO
                } else if !e.permite {
                    situacao::NAO_PERMITIDO
                } else {
                    situacao::PRONTO
                }
            }
        }
    }

    fn olhar_situacao(&mut self, agora: Instant) {
        let nova = self.calcular_situacao(agora);
        if nova != self.situacao {
            self.situacao = nova;
            self.mudancas |= mudou_no_receptor::SITUACAO;
        }
        if let Some((_, t)) = &self.recusa {
            if agora.duration_since(*t) >= RECUSA_DURA {
                self.recusa = None;
                self.mudancas |= mudou_no_receptor::RECUSA;
            }
        }
        if self.tem_pendente() && self.ultima_edicao.is_some_and(|t| agora.duration_since(t) >= PRAZO_DO_PEDIDO) {
            self.largar_pendente();
            self.contadores.desistencias += 1;
            self.recusa = Some((Recusa::de(motivo::SEM_RESPOSTA, None), agora));
            self.mudancas |= mudou_no_receptor::RECUSA | mudou_no_receptor::AJUSTE;
        }
    }

    /// Recebe uma mensagem do filmador.
    pub fn receber(&mut self, texto: &str, agora: Instant) -> Mudancas {
        let antes = self.mudancas;
        self.mudancas = 0;
        let (o, tipo) = match envelope(texto) {
            Ok(x) => x,
            Err(d) => {
                match d {
                    Descarte::Invalida => self.contadores.invalidas += 1,
                    Descarte::DeOutroApp => self.contadores.de_outro_app += 1,
                    Descarte::DeOutraVersao => self.contadores.de_outra_versao += 1,
                }
                self.mudancas = antes;
                return 0;
            }
        };
        self.contadores.recebidas += 1;
        match tipo.as_str() {
            "capacidades" => self.receber_capacidades(&o),
            "estado" => self.receber_estado(&o, agora),
            "recusa" => self.receber_recusa(&o, agora),
            _ => self.contadores.tipo_desconhecido += 1,
        }
        self.olhar_situacao(agora);
        let agora_mudou = self.mudancas;
        self.mudancas |= antes;
        agora_mudou
    }

    /// `true` quando a época é outra: esquece o filmador de antes.
    fn conferir_epoca(&mut self, o: &Objeto) -> bool {
        let Some(epoca) = o.get("epoca").and_then(Value::as_str) else { return false };
        if self.epoca.as_deref() != Some(epoca) {
            if self.epoca.is_some() {
                self.esquecer_o_filmador();
            }
            self.epoca = Some(epoca.to_string());
        }
        true
    }

    fn receber_capacidades(&mut self, o: &Objeto) {
        if !self.conferir_epoca(o) {
            self.contadores.invalidas += 1;
            return;
        }
        let Some(cap) = o.get("cap").and_then(Value::as_u64) else {
            self.contadores.invalidas += 1;
            return;
        };
        if cap < self.cap_das_capacidades {
            return; // mais velha que a que já temos
        }
        let caps = match o.get("capacidades") {
            Some(Value::Object(c)) if conferir_capacidades(c).is_ok() => Some(c.clone()),
            Some(Value::Null) | None if o.get("camera").and_then(Value::as_u64) == Some(0) => None,
            _ => {
                self.contadores.invalidas += 1;
                return;
            }
        };
        if cap != self.cap_das_capacidades || caps != self.capacidades {
            self.cap_das_capacidades = cap;
            self.capacidades = caps;
            self.mudancas |= mudou_no_receptor::CAPACIDADES;
        }
    }

    fn receber_estado(&mut self, o: &Objeto, agora: Instant) {
        if !self.conferir_epoca(o) {
            self.contadores.invalidas += 1;
            return;
        }
        let num = |k: &str| o.get(k).and_then(Value::as_u64);
        let (Some(n), Some(versao), Some(camera), Some(cap)) = (num("n"), num("versao"), num("camera"), num("cap")) else {
            self.contadores.invalidas += 1;
            return;
        };
        if self.estado.as_ref().is_some_and(|e| n <= e.n) {
            return; // mais velho que o que já temos (o canal é sem ordem)
        }
        let novo = EstadoRecebido {
            n,
            versao,
            camera,
            cap,
            permite: o.get("permite").and_then(Value::as_bool).unwrap_or(false),
            ajuste: o.get("ajuste").and_then(Value::as_object).cloned().unwrap_or_default(),
            lido: o.get("lido").and_then(Value::as_object).cloned().unwrap_or_default(),
            autor: o.get("autor").and_then(Value::as_str).map(nome_curto),
        };
        match &self.estado {
            None => self.mudancas |= mudou_no_receptor::AJUSTE | mudou_no_receptor::LIDO,
            Some(e) => {
                if e.versao != novo.versao
                    || e.ajuste != novo.ajuste
                    || e.permite != novo.permite
                    || e.autor != novo.autor
                    || e.camera != novo.camera
                {
                    self.mudancas |= mudou_no_receptor::AJUSTE;
                }
                if e.lido != novo.lido {
                    self.mudancas |= mudou_no_receptor::LIDO;
                }
            }
        }
        if cap != self.cap_das_capacidades {
            self.ola_devido = true;
        }
        self.estado = Some(novo);
        self.ouvido = Some(agora);
        // O recibo.
        if let Some(seu) = o.get("seu").and_then(Value::as_object) {
            let seq = seu.get("seq").and_then(Value::as_u64).unwrap_or(0);
            if self.tem_pendente() && seq >= self.seq_pendente {
                let recusa = seu.get("recusa").and_then(Value::as_object).and_then(ler_recusa);
                if seq == self.seq_pendente {
                    if let Some(r) = recusa {
                        self.contadores.recusas += 1;
                        self.recusa = Some((r, agora));
                        self.mudancas |= mudou_no_receptor::RECUSA;
                    }
                }
                self.largar_pendente();
                self.mudancas |= mudou_no_receptor::AJUSTE;
            }
        }
    }

    fn receber_recusa(&mut self, o: &Objeto, agora: Instant) {
        let Some(seq) = o.get("seq").and_then(Value::as_u64) else {
            self.contadores.invalidas += 1;
            return;
        };
        if !self.tem_pendente() || seq != self.seq_pendente {
            return;
        }
        let Some(r) = ler_recusa(o) else {
            self.contadores.invalidas += 1;
            return;
        };
        self.contadores.recusas += 1;
        self.recusa = Some((r, agora));
        self.largar_pendente();
        self.mudancas |= mudou_no_receptor::RECUSA | mudou_no_receptor::AJUSTE;
    }

    /// O ajuste aplicado com o pendente por cima.
    fn vigente(&self) -> Objeto {
        // Com `restaurar` pendente, o que vale é o registro padrão (todo "auto"), como no filmador.
        let mut a = if self.restaurar {
            Objeto::new()
        } else {
            self.estado.as_ref().map(|e| e.ajuste.clone()).unwrap_or_default()
        };
        for (k, v) in &self.pendente {
            a.insert(k.clone(), v.clone());
        }
        a
    }

    fn pronto(&self) -> Result<(&Objeto, u64)> {
        if self.situacao != situacao::PRONTO {
            return Err(Error::Invalid(format!("a câmera do outro lado não está pronta: {}", self.situacao)));
        }
        match (&self.capacidades, &self.estado) {
            (Some(c), Some(e)) => Ok((c, e.versao)),
            _ => Err(Error::Invalid("a câmera do outro lado não está pronta".into())),
        }
    }

    fn novo_seq(&mut self, vista: u64, agora: Instant) {
        self.seq += 1;
        self.seq_pendente = self.seq;
        self.vista_pendente = vista;
        self.ultima_edicao = Some(agora);
    }

    /// Pede um ajuste parcial. Confere contra as capacidades que chegaram (as regras do filmador,
    /// menos o "vence o último"): o que o filmador recusaria é recusado aqui, sem rede.
    pub fn pedir(&mut self, json: &str, agora: Instant) -> Result<()> {
        let parcial = ler_objeto(json, TETO_DO_AJUSTE, "pedido")?;
        let (caps, vista) = self.pronto()?;
        let vigente = self.vigente();
        let conferido = conferir_pedido(caps, &vigente, Some(&Value::Object(parcial)), None, None)
            .map_err(|r| Error::Invalid(texto_da_recusa(&r)))?;
        if self.pendente.len() + conferido.ajuste.len() > TETO_DE_CAMPOS {
            // Os campos pendentes e os novos não cabem num pedido: vale o mais novo.
            self.pendente.clear();
            self.vistas.clear();
        }
        for (k, v) in conferido.ajuste {
            // A vista é **por campo**: juntar um campo velho num pedido novo não pode dar a ele a
            // vista de agora (achado B2 da revisão de 02/10).
            self.vistas.insert(k.clone(), vista);
            self.pendente.insert(k, v);
        }
        self.novo_seq(vista, agora);
        Ok(())
    }

    /// "Restaurar automático" na câmera do outro lado.
    pub fn restaurar(&mut self, agora: Instant) -> Result<()> {
        let (_, vista) = self.pronto()?;
        self.pendente.clear();
        self.vistas.clear();
        self.restaurar = true;
        self.novo_seq(vista, agora);
        Ok(())
    }

    /// Um toque na imagem mostrada, de 0 a 1.
    pub fn tocar(&mut self, x: f64, y: f64, longo: bool, agora: Instant) -> Result<()> {
        let (caps, vista) = self.pronto()?;
        let vigente = self.vigente();
        let toque = json!({"x": x, "y": y, "longo": longo});
        let conferido = conferir_pedido(caps, &vigente, None, None, Some(&toque))
            .map_err(|r| Error::Invalid(texto_da_recusa(&r)))?;
        self.toque = conferido.toque;
        self.novo_seq(vista, agora);
        Ok(())
    }

    /// O que está devido ao filmador agora, já como texto.
    pub(crate) fn devidas(&mut self, agora: Instant) -> Vec<String> {
        self.olhar_situacao(agora);
        let mut saida = Vec::new();
        let reenvio_do_ola = if self.situacao == situacao::SEM_RESPOSTA { REENVIO_DO_OLA_SEM_RESPOSTA } else { REENVIO_DO_OLA };
        let ola_vencido = self.ultimo_ola.is_none_or(|t| agora.duration_since(t) >= reenvio_do_ola);
        let sem_estado = self.estado.is_none();
        if self.sessao.is_some() && ola_vencido && (self.ola_devido || sem_estado) {
            let mut corpo = Objeto::new();
            corpo.insert("cap".into(), Value::from(self.cap_das_capacidades));
            saida.push(mensagem("ola", corpo));
            self.ultimo_ola = Some(agora);
            self.ola_devido = false;
            self.contadores.olas_enviados += 1;
        }
        if self.tem_pendente() {
            let intervalo_ok = self.ultimo_envio.is_none_or(|t| agora.duration_since(t) >= INTERVALO_DOS_PEDIDOS);
            let novo = self.enviado_seq != self.seq_pendente;
            let reenvio = !novo && self.ultimo_envio.is_some_and(|t| agora.duration_since(t) >= REENVIO_DO_PEDIDO);
            if intervalo_ok && (novo || reenvio) {
                let e = self.estado.as_ref();
                let mut corpo = Objeto::new();
                corpo.insert("epoca".into(), self.epoca.clone().map(Value::from).unwrap_or(Value::Null));
                corpo.insert("camera".into(), Value::from(e.map(|e| e.camera).unwrap_or(0)));
                corpo.insert("seq".into(), Value::from(self.seq_pendente));
                corpo.insert("vista".into(), Value::from(self.vista_pendente));
                if !self.pendente.is_empty() {
                    corpo.insert("ajuste".into(), Value::Object(self.pendente.clone()));
                    let vistas: Objeto = self.vistas.iter().map(|(k, v)| (k.clone(), Value::from(*v))).collect();
                    corpo.insert("vistas".into(), Value::Object(vistas));
                }
                if self.restaurar {
                    corpo.insert("restaurar".into(), Value::Bool(true));
                }
                if let Some(t) = &self.toque {
                    corpo.insert("toque".into(), json!(t));
                }
                let texto = mensagem("pedido", corpo);
                if texto.len() > TETO_DA_MENSAGEM_DA_CAMERA {
                    self.contadores.mensagens_impossiveis += 1;
                    self.largar_pendente();
                } else {
                    if novo {
                        self.contadores.pedidos_enviados += 1;
                    } else {
                        self.contadores.reenvios += 1;
                    }
                    self.enviado_seq = self.seq_pendente;
                    self.ultimo_envio = Some(agora);
                    saida.push(texto);
                }
            }
        }
        saida
    }

    /// O `ola` ou o pedido cujo envio falhou porque o canal ainda não abriu: tenta de novo.
    pub(crate) fn devolver(&mut self, texto: &str) {
        if texto.contains("\"tipo\":\"ola\"") {
            self.ultimo_ola = None;
            self.ola_devido = true;
        } else {
            self.enviado_seq = 0;
            self.ultimo_envio = None;
        }
    }

    /// Os bits do que mudou desde a última vez, apagados.
    pub fn tirar_mudancas(&mut self) -> Mudancas {
        std::mem::take(&mut self.mudancas)
    }

    /// A situação (`situacao::*`).
    pub fn situacao(&self) -> &'static str {
        self.situacao
    }

    /// O ajuste aplicado no filmador, como chegou (sem o pendente).
    pub fn aplicado(&self) -> Option<&Map<String, Value>> {
        self.estado.as_ref().map(|e| &e.ajuste)
    }

    /// O estado para a tela (`quall_camera_remote_state_json`).
    pub fn estado_json(&mut self, agora: Instant) -> String {
        self.olhar_situacao(agora);
        let e = self.estado.as_ref();
        let recusa = self.recusa.as_ref().map(|(r, t)| {
            json!({"motivo": r.motivo, "campo": r.campo, "ha_ms": ms(agora.duration_since(*t))})
        });
        json!({
            "situacao": self.situacao,
            "capacidades": self.capacidades.clone().map(Value::Object),
            "ajuste": e.map(|_| Value::Object(self.vigente())),
            "aplicado": e.map(|e| Value::Object(e.ajuste.clone())),
            "pendente": Value::Object(self.pendente.clone()),
            "lido": e.map(|e| Value::Object(e.lido.clone())),
            "autor": e.and_then(|e| e.autor.clone()),
            "versao": e.map(|e| e.versao),
            "recusa": recusa,
            "contadores": self.contadores,
        })
        .to_string()
    }

    /// Os contadores.
    pub fn contadores(&self) -> ContadoresDoReceptor {
        self.contadores
    }
}

fn ler_recusa(o: &Objeto) -> Option<Recusa> {
    let m = o.get("motivo").and_then(Value::as_str).filter(|m| codigo_valido(m))?;
    let campo = o.get("campo").and_then(Value::as_str).map(nome_curto);
    Some(Recusa { motivo: m.to_string(), campo })
}

fn texto_da_recusa(r: &Recusa) -> String {
    match &r.campo {
        Some(c) => format!("{} ({c})", r.motivo),
        None => r.motivo.clone(),
    }
}

// =============================================================================================
// Atrás de um cadeado, com a bombeada
// =============================================================================================

/// O resultado de uma bombeada: o que mudou, **e** se a sessão acabou (a fila já foi lida até o
/// fim). As duas coisas juntas, como na do teleprompter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bombeada {
    pub mudancas: Mudancas,
    pub fechada: bool,
}

#[derive(Debug)]
struct InternoDoFilmador {
    nucleo: NucleoDoFilmador,
    mensageiros: BTreeMap<u64, Mensageiro>,
}

/// **O filmador**, um por câmera em uso, compartilhado por todas as sessões de vídeo que a
/// transmitem. As mudanças da casca podem vir de qualquer thread e saem na hora para as sessões
/// já bombeadas; cada sessão bombeia na thread dela.
#[derive(Debug)]
pub struct Filmador {
    interno: Mutex<InternoDoFilmador>,
}

impl Default for Filmador {
    fn default() -> Self {
        Self::novo()
    }
}

fn envenenado() -> Error {
    Error::Transport("o controle da câmera foi envenenado por um panic".into())
}

/// Manda o que está devido a uma sessão. `true` quando a sessão acabou.
fn despachar_sessao(i: &mut InternoDoFilmador, sessao: u64, agora: Instant) -> bool {
    let Some(m) = i.mensageiros.get(&sessao).cloned() else { return false };
    for (d, texto) in i.nucleo.devidas(sessao, agora) {
        match m.enviar(&texto) {
            Ok(()) => {}
            Err(Error::Closed) => return true,
            Err(Error::Invalid(_)) => i.nucleo.contadores.mensagens_impossiveis += 1,
            Err(_) => i.nucleo.devolver(sessao, d),
        }
    }
    false
}

/// Esquece as sessões cujo mensageiro diz que acabaram.
fn podar(i: &mut InternoDoFilmador) {
    let mortas: Vec<u64> = i.mensageiros.iter().filter(|(_, m)| m.acabou()).map(|(s, _)| *s).collect();
    for s in mortas {
        i.mensageiros.remove(&s);
        i.nucleo.esquecer_sessao(s);
    }
}

fn despachar_todas(i: &mut InternoDoFilmador, agora: Instant) {
    let sessoes: Vec<u64> = i.mensageiros.keys().copied().collect();
    for s in sessoes {
        if despachar_sessao(i, s, agora) {
            i.mensageiros.remove(&s);
            i.nucleo.esquecer_sessao(s);
        }
    }
}

impl Filmador {
    /// Um filmador sem câmera, com a permissão desligada.
    pub fn novo() -> Filmador {
        Filmador {
            interno: Mutex::new(InternoDoFilmador { nucleo: NucleoDoFilmador::novo(), mensageiros: BTreeMap::new() }),
        }
    }

    fn com<R>(&self, f: impl FnOnce(&mut InternoDoFilmador) -> R) -> Result<R> {
        let mut g = self.interno.lock().map_err(|_| envenenado())?;
        Ok(f(&mut g))
    }

    /// Liga ou desliga "Permitir controle remoto da câmera".
    pub fn permitir(&self, permite: bool) -> Result<()> {
        self.com(|i| {
            i.nucleo.permitir(permite);
            despachar_todas(i, Instant::now());
        })
    }

    /// A câmera em uso (`None` = sem câmera). Ver [`NucleoDoFilmador::definir_camera`].
    pub fn definir_camera(&self, camera: Option<(&str, &str)>) -> Result<()> {
        self.com(|i| {
            i.nucleo.definir_camera(camera)?;
            despachar_todas(i, Instant::now());
            Ok(())
        })?
    }

    /// As faixas novas da mesma câmera. Ver [`NucleoDoFilmador::definir_capacidades`].
    pub fn definir_capacidades(&self, caps: &str) -> Result<()> {
        self.com(|i| {
            i.nucleo.definir_capacidades(caps)?;
            despachar_todas(i, Instant::now());
            Ok(())
        })?
    }

    /// Esquece a sessão deste mensageiro (a casca a largou sem bombear até o fim).
    pub fn esquecer(&self, m: &Mensageiro) -> Result<()> {
        self.com(|i| {
            i.mensageiros.remove(&m.sessao());
            i.nucleo.esquecer_sessao(m.sessao());
        })
    }

    /// O registro que ficou valendo; `pedido` 0 é mudança local.
    pub fn definir_ajuste(&self, json: &str, pedido: u64) -> Result<()> {
        self.com(|i| {
            i.nucleo.definir_ajuste(json, pedido, Instant::now())?;
            despachar_todas(i, Instant::now());
            Ok(())
        })?
    }

    /// O lido. Sai com o estado seguinte, no máximo a cada 250 ms (na bombeada).
    pub fn definir_lido(&self, json: &str) -> Result<()> {
        self.com(|i| i.nucleo.definir_lido(json))?
    }

    /// A casca não conseguiu aplicar o pedido `n`.
    pub fn recusar(&self, n: u64, codigo: &str) -> Result<()> {
        self.com(|i| {
            i.nucleo.recusar(n, codigo)?;
            despachar_todas(i, Instant::now());
            Ok(())
        })?
    }

    /// O próximo pedido aceito, tirado da fila (`None` com a fila vazia).
    pub fn proximo_pedido(&self) -> Result<Option<String>> {
        self.com(|i| {
            let p = i.nucleo.proximo_pedido(Instant::now());
            despachar_todas(i, Instant::now());
            p
        })
    }

    /// A espiada da fila, para a fronteira C. Ver [`NucleoDoFilmador::proximo_pedido_se`].
    pub fn proximo_pedido_se(&self, aceitar: impl FnOnce(&str) -> bool) -> Result<Option<usize>> {
        self.com(|i| {
            let p = i.nucleo.proximo_pedido_se(aceitar, Instant::now());
            despachar_todas(i, Instant::now());
            p
        })
    }

    /// O estado para a tela do filmador.
    pub fn estado_json(&self) -> Result<String> {
        self.com(|i| i.nucleo.estado_json(Instant::now()))
    }

    /// **A bombeada** de uma sessão: manda o que está devido a ela, espera até `limite` pela
    /// primeira mensagem, trata o que chegou, e manda de novo. Não segura o cadeado enquanto
    /// espera. Com a sessão acabada, lê a fila até o fim e esquece a sessão.
    pub fn bombear(&self, m: &Mensageiro, limite: Duration) -> Result<Bombeada> {
        let sessao = m.sessao();
        let mut fechada = self.com(|i| {
            if !i.mensageiros.contains_key(&sessao) {
                // Uma sessão nova: antes, esquece as que acabaram sem ninguém bombear até o fim,
                // para elas não ocuparem as 16 vagas.
                podar(i);
                i.mensageiros.insert(sessao, m.clone());
            }
            despachar_sessao(i, sessao, Instant::now())
        })?;
        let mut espera = if fechada { Duration::ZERO } else { limite };
        for _ in 0..=FILA_DE_DADOS {
            match m.proxima(espera) {
                Ok(Some(texto)) => {
                    self.com(|i| i.nucleo.receber(sessao, m.par(), &texto, Instant::now()))?;
                    espera = Duration::ZERO;
                }
                Ok(None) => break,
                Err(_) => {
                    fechada = true;
                    break;
                }
            }
        }
        self.com(|i| {
            if !fechada {
                fechada = despachar_sessao(i, sessao, Instant::now());
            }
            if fechada {
                i.mensageiros.remove(&sessao);
                i.nucleo.esquecer_sessao(sessao);
            }
            Bombeada { mudancas: i.nucleo.tirar_mudancas(), fechada }
        })
    }
}

#[derive(Debug)]
struct InternoDoReceptor {
    nucleo: NucleoDoReceptor,
    mensageiro: Option<Mensageiro>,
}

/// **O controle da câmera do outro lado**, um por sessão de recepção (recomeça sozinho numa sessão
/// nova). Os pedidos da tela podem vir de qualquer thread e saem na hora; a sessão bombeia na
/// thread dela.
#[derive(Debug)]
pub struct Controlador {
    interno: Mutex<InternoDoReceptor>,
}

impl Default for Controlador {
    fn default() -> Self {
        Self::novo()
    }
}

fn despachar_receptor(i: &mut InternoDoReceptor, agora: Instant) -> bool {
    let Some(m) = i.mensageiro.clone() else { return false };
    for texto in i.nucleo.devidas(agora) {
        match m.enviar(&texto) {
            Ok(()) => {}
            Err(Error::Closed) => return true,
            Err(Error::Invalid(_)) => i.nucleo.contadores.mensagens_impossiveis += 1,
            Err(_) => i.nucleo.devolver(&texto),
        }
    }
    false
}

impl Controlador {
    /// Um controle sem sessão.
    pub fn novo() -> Controlador {
        Controlador { interno: Mutex::new(InternoDoReceptor { nucleo: NucleoDoReceptor::novo(), mensageiro: None }) }
    }

    fn com<R>(&self, f: impl FnOnce(&mut InternoDoReceptor) -> R) -> Result<R> {
        let mut g = self.interno.lock().map_err(|_| envenenado())?;
        Ok(f(&mut g))
    }

    /// Pede um ajuste parcial (JSON objeto, só os campos mexidos).
    pub fn pedir(&self, json: &str) -> Result<()> {
        self.com(|i| {
            i.nucleo.pedir(json, Instant::now())?;
            despachar_receptor(i, Instant::now());
            Ok(())
        })?
    }

    /// "Restaurar automático".
    pub fn restaurar(&self) -> Result<()> {
        self.com(|i| {
            i.nucleo.restaurar(Instant::now())?;
            despachar_receptor(i, Instant::now());
            Ok(())
        })?
    }

    /// Um toque na imagem mostrada, de 0 a 1.
    pub fn tocar(&self, x: f64, y: f64, longo: bool) -> Result<()> {
        self.com(|i| {
            i.nucleo.tocar(x, y, longo, Instant::now())?;
            despachar_receptor(i, Instant::now());
            Ok(())
        })?
    }

    /// O estado para a tela.
    pub fn estado_json(&self) -> Result<String> {
        self.com(|i| i.nucleo.estado_json(Instant::now()))
    }

    /// **A bombeada**: numa sessão nova, esquece a anterior; manda o `ola` e o pedido devidos,
    /// espera até `limite`, trata o que chegou, e manda de novo.
    pub fn bombear(&self, m: &Mensageiro, limite: Duration) -> Result<Bombeada> {
        let mut fechada = self.com(|i| {
            let agora = Instant::now();
            if i.mensageiro.as_ref().map(Mensageiro::sessao) != Some(m.sessao()) {
                i.mensageiro = Some(m.clone());
            }
            i.nucleo.comecar_sessao(m.sessao(), agora);
            despachar_receptor(i, agora)
        })?;
        let mut espera = if fechada { Duration::ZERO } else { limite };
        for _ in 0..=FILA_DE_DADOS {
            match m.proxima(espera) {
                Ok(Some(texto)) => {
                    self.com(|i| i.nucleo.receber(&texto, Instant::now()))?;
                    espera = Duration::ZERO;
                }
                Ok(None) => break,
                Err(_) => {
                    fechada = true;
                    break;
                }
            }
        }
        self.com(|i| {
            if !fechada {
                fechada = despachar_receptor(i, Instant::now());
            } else {
                i.nucleo.olhar_situacao(Instant::now());
            }
            Bombeada { mudancas: i.nucleo.tirar_mudancas(), fechada }
        })
    }
}

#[cfg(test)]
mod testes;
