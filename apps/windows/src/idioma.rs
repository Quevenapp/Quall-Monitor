//! **A tradução EN/PT do app Windows** (pedido do Bruno em 02/10/2026; o contrato comum das cinco
//! plataformas está em `docs/traducao.md`).
//!
//! # O mecanismo
//!
//! Uma tabela **português → inglês** num módulo puro, sem Win32: o português continua sendo o
//! texto-fonte e é a própria chave. Quem mostra um texto passa a escrever `t("Espelhar")` em vez de
//! `"Espelhar"`; com o app em português, `t` devolve a mesma `&'static str` e nada muda (os testes
//! que comparam frases em português continuam passando); em inglês, devolve a da tabela. Texto com
//! partes variáveis usa `tf("Conectado a {}", &[&nome])`: o molde é a chave, e os `{}` são trocados
//! em ordem nos dois idiomas.
//!
//! As tabelas ficam em `src/idioma/*.rs`, uma por área (a janela principal, a bandeja, o
//! teleprompter, os ajustes da câmera, o receptor, as mensagens), para cada área ser escrita sem
//! mexer nas outras.
//!
//! # O idioma de agora
//!
//! - **Padrão**: o idioma da interface do Windows (`GetUserDefaultUILanguage`): português (qualquer
//!   região) → PT; qualquer outro → EN ([`do_sistema`]).
//! - **A escolha do botão PT | EN** do cabeçalho da janela principal fica em `idioma.txt`, na pasta de
//!   dados (`%APPDATA%\Quall Monitor`), e vence o sistema dali em diante ([`ler_escolha`]).
//! - **A troca vale na hora**: [`definir`] muda o global e sobe a [`versao`]; cada janela que tem
//!   texto próprio (a principal, a do teleprompter, a dos ajustes da câmera, o menu da bandeja)
//!   reescreve os rótulos quando vê a versão mudar.
//! - Nos testes, [`com_idioma`] troca o idioma **só na thread do teste** (os testes rodam em
//!   paralelo, e o global de um mudaria as frases do outro).
//!
//! # Fora da tradução
//!
//! Os diários (`registro::linha`), as bandeiras e as telas de bancada, os nomes de arquivo e o
//! protocolo. E o **detalhe técnico** de um erro do sistema que chega à tela dentro de uma frase
//! (um `HRESULT`, o motivo do driver): a frase em volta é traduzida, o detalhe vai como veio.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::OnceLock;

mod ajustes;
mod bandeja;
#[cfg(any(test, feature = "tela-estendida-futura"))]
mod driver;
mod janela;
mod mensagens;
mod monitor;
mod receptor;
mod teleprompter;

/// Os dois idiomas do app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Idioma {
    #[default]
    Pt,
    En,
}

impl Idioma {
    /// Como fica no `idioma.txt`.
    pub fn codigo(self) -> &'static str {
        match self {
            Idioma::Pt => "pt",
            Idioma::En => "en",
        }
    }

    /// O nome para o Narrador do seletor do cabeçalho (o contrato comum, item 1).
    pub fn nome_acessivel(self) -> &'static str {
        match self {
            Idioma::Pt => "Idioma: Português",
            Idioma::En => "Language: English",
        }
    }

    /// A sigla do segmento no seletor.
    pub fn sigla(self) -> &'static str {
        match self {
            Idioma::Pt => "PT",
            Idioma::En => "EN",
        }
    }

    pub fn outro(self) -> Idioma {
        match self {
            Idioma::Pt => Idioma::En,
            Idioma::En => Idioma::Pt,
        }
    }
}

/// 0 = PT, 1 = EN. Começa em PT: os testes e as bancadas que não chamam [`iniciar`] veem o texto-fonte.
static ATUAL: AtomicU8 = AtomicU8::new(0);
/// Sobe a cada troca: as janelas comparam com a que viram por último.
static VERSAO: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static NESTA_THREAD: Cell<Option<Idioma>> = const { Cell::new(None) };
}

/// O idioma de agora (o desta thread, se um teste ou um retrato o trocou só nela).
pub fn atual() -> Idioma {
    if let Some(i) = NESTA_THREAD.with(|c| c.get()) {
        return i;
    }
    match ATUAL.load(Ordering::Relaxed) {
        1 => Idioma::En,
        _ => Idioma::Pt,
    }
}

/// Troca o idioma do processo (o seletor do cabeçalho). Devolve `true` quando mudou.
pub fn definir(i: Idioma) -> bool {
    let novo = match i {
        Idioma::Pt => 0,
        Idioma::En => 1,
    };
    let antes = ATUAL.swap(novo, Ordering::SeqCst);
    if antes != novo {
        VERSAO.fetch_add(1, Ordering::SeqCst);
        true
    } else {
        false
    }
}

/// A versão do idioma: muda a cada troca. Quem tem rótulos guardados os reescreve quando ela muda.
pub fn versao() -> u32 {
    VERSAO.load(Ordering::SeqCst)
}

/// Roda `f` com o idioma `i` **só nesta thread** (testes e retratos de bancada).
pub fn com_idioma<R>(i: Idioma, f: impl FnOnce() -> R) -> R {
    let antes = NESTA_THREAD.with(|c| c.replace(Some(i)));
    let r = f();
    NESTA_THREAD.with(|c| c.set(antes));
    r
}

/// O idioma pelo da interface do Windows: o `LANGID` de `GetUserDefaultUILanguage`. Português é a
/// língua primária `0x16` (pt-BR `0x0416`, pt-PT `0x0816`); qualquer outra é inglês.
pub fn do_sistema(langid: u16) -> Idioma {
    if langid & 0x3FF == 0x16 {
        Idioma::Pt
    } else {
        Idioma::En
    }
}

/// O idioma por um nome de região (`pt-BR`, `pt_PT`, `en-US`…): `pt` no começo é português.
pub fn do_nome(nome: &str) -> Idioma {
    let n = nome.trim().to_ascii_lowercase();
    if n == "pt" || n.starts_with("pt-") || n.starts_with("pt_") {
        Idioma::Pt
    } else {
        Idioma::En
    }
}

/// A escolha guardada no `idioma.txt` (`pt` ou `en`); outra coisa, ou nada, é "sem escolha".
pub fn ler_escolha(conteudo: &str) -> Option<Idioma> {
    match conteudo.trim().to_ascii_lowercase().as_str() {
        "pt" => Some(Idioma::Pt),
        "en" => Some(Idioma::En),
        _ => None,
    }
}

/// O idioma inicial: a escolha guardada vence o sistema.
pub fn inicial(escolha: Option<Idioma>, sistema: Idioma) -> Idioma {
    escolha.unwrap_or(sistema)
}

/// O nome do arquivo da escolha, na pasta de dados.
pub const ARQUIVO_DA_ESCOLHA: &str = "idioma.txt";

/// Lê a escolha guardada em `pasta` (ou o idioma do Windows) e define o idioma do processo. Chamado
/// uma vez, no começo do app.
#[cfg(windows)]
pub fn iniciar(pasta: &std::path::Path) -> Idioma {
    let escolha = std::fs::read_to_string(pasta.join(ARQUIVO_DA_ESCOLHA)).ok().and_then(|s| ler_escolha(&s));
    let sistema = do_sistema(unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() });
    let i = inicial(escolha, sistema);
    definir(i);
    i
}

/// Guarda a escolha do seletor em `pasta`.
pub fn guardar(pasta: &std::path::Path, i: Idioma) -> std::io::Result<()> {
    std::fs::write(pasta.join(ARQUIVO_DA_ESCOLHA), i.codigo())
}

// =============================================================================================
// A tabela
// =============================================================================================

/// Todas as áreas juntas: `(português, inglês)`.
pub fn areas() -> Vec<(&'static str, &'static [(&'static str, &'static str)])> {
    vec![
        ("janela", janela::TEXTOS),
        ("bandeja", bandeja::TEXTOS),
        ("teleprompter", teleprompter::TEXTOS),
        ("ajustes", ajustes::TEXTOS),
        ("receptor", receptor::TEXTOS),
        ("mensagens", mensagens::TEXTOS),
        ("monitor", monitor::TEXTOS),
        #[cfg(any(test, feature = "tela-estendida-futura"))]
        ("driver", driver::TEXTOS),
    ]
}

fn tabela() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        for (_, area) in areas() {
            for (pt, en) in area {
                m.insert(*pt, *en);
            }
        }
        m
    })
}

/// O inglês de um texto em português, se a tabela o tem.
pub fn en_de(pt: &str) -> Option<&'static str> {
    tabela().get(pt).copied()
}

/// **O texto no idioma de agora.** Sem tradução na tabela, o português (o teste de paridade acusa).
pub fn t(pt: &'static str) -> &'static str {
    match atual() {
        Idioma::Pt => pt,
        Idioma::En => en_de(pt).unwrap_or(pt),
    }
}

/// [`t`] para um texto que não é `'static` (um aviso guardado numa `String`, que pode ser uma das
/// frases da tabela): o mesmo texto quando não está nela.
pub fn tr(pt: &str) -> String {
    match atual() {
        Idioma::Pt => pt.to_string(),
        Idioma::En => en_de(pt).map(str::to_string).unwrap_or_else(|| pt.to_string()),
    }
}

/// **Um molde com partes variáveis**: `tf("Conectado a {}", &[&nome])`. O molde em português é a
/// chave; os `{}` são trocados em ordem, nos dois idiomas.
pub fn tf(pt: &'static str, partes: &[&dyn Display]) -> String {
    preencher(t(pt), partes)
}

/// Troca os `{}` de `molde`, em ordem, por `partes` (as que sobrarem ficam de fora; `{}` sem parte
/// fica como está).
pub fn preencher(molde: &str, partes: &[&dyn Display]) -> String {
    let mut s = String::with_capacity(molde.len() + 16);
    let mut resto = molde;
    let mut i = 0;
    while let Some(p) = resto.find("{}") {
        s.push_str(&resto[..p]);
        match partes.get(i) {
            Some(v) => s.push_str(&v.to_string()),
            None => s.push_str("{}"),
        }
        i += 1;
        resto = &resto[p + 2..];
    }
    s.push_str(resto);
    s
}

/// Um número com `casas` decimais, com a vírgula do português ou o ponto do inglês.
pub fn decimal(v: f64, casas: usize) -> String {
    let s = format!("{v:.casas$}");
    match atual() {
        Idioma::Pt => s.replace('.', ","),
        Idioma::En => s,
    }
}

/// Quantos `{}` um texto tem.
pub fn lacunas(s: &str) -> usize {
    s.matches("{}").count()
}

#[cfg(test)]
mod testes;
