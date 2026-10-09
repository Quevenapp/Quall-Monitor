//! **O teleprompter no Windows, nos dois papéis** (`docs/contrato-teleprompter.md`; handover de
//! 13/09, §6 item 3). Decisão do usuário: todo aparelho mostra o texto e controla — este
//! computador hospeda como **prompter** (o texto rolando, o PIN, o endereço) ou conecta como
//! **controle** de outro aparelho. O texto se edita dos dois lados e vale o último que mudou (a
//! fusão é do núcleo: aqui só se edita, bombeia e lê). Se o controle cai, o prompter continua como
//! estava, com aviso nas duas telas.
//!
//! # Onde cada peça mora
//!
//! | peça | arquivo | Win32? |
//! |---|---|---|
//! | porta, endereço, política do laço, avisos, rascunho, passos, ações de bancada | `regras.rs` | não — testes de unidade |
//! | geometria do texto, rolagem, cadência dos quadros | `geometria.rs` | não — testes de unidade |
//! | a thread da sessão, nos dois papéis, com a ordem do fim | `sessao.rs` | não — testes com sessão de verdade por 127.0.0.1 |
//! | a quebra em linhas (DirectWrite, thread própria) e o desenho (Direct2D) | `texto.rs` | sim |
//! | as janelas | `tela.rs` | sim |
//! | o relato de bancada, a captura da própria janela, a área de transferência | `bancada.rs` | sim |
//!
//! # Threads
//!
//! - **a janela** tem thread própria (`quall.teleprompter.tela`), com o próprio laço de mensagens:
//!   a rolagem desenha um quadro por retraço, e isso não pode depender do relógio de 100 ms da
//!   janela principal;
//! - **a sessão** (`quall.teleprompter.sessao`) é dona exclusiva do `Ready` do núcleo — a regra de
//!   `janela.rs`: a janela nunca toca na sessão; ela chama os `definir_*` da mesma réplica (que é
//!   `Sync` e manda na hora) e lê o que a sessão publica;
//! - **a quebra** (`quall.teleprompter.diagrama`) roda fora da janela: o tranco do iPhone X
//!   (layout de 100 KB na thread principal) não se repete.
//!
//! # Uma réplica por papel, que vive mais que a sessão (§3)
//!
//! Criada na primeira vez que cada papel abre, guardada até o processo sair, cada uma no seu
//! arquivo (`teleprompter-prompter.json`, `teleprompter-controle.json`, na pasta de dados do app —
//! a de `QUALL_PASTA_DE_DADOS` na bancada), com o `device_id` do app como autor. Um arquivo só
//! para os dois papéis faria o espelho e a fonte que este computador usou **como prompter**
//! vencerem, pelo carimbo mais novo, no aparelho que ele passasse a **controlar** (a mesma
//! separação do Mac).
//!
//! # Os ajustes locais do prompter (`docs/teleprompter-ajustes-locais.md`)
//!
//! O **enquadramento** (duas setas laterais, o texto centralizado entre elas) e a **fonte
//! automática** são do aparelho no suporte: ficam em `teleprompter-ajustes.json`, fora do salvo do
//! núcleo. A "vista do texto" do contrato é a área entre as setas — a `margem` é fração dela. A
//! **linha de leitura** arrastável é campo do núcleo e vai pelo fio como sempre.

pub mod bancada;
// A tela R5 (`docs/teleprompter-com-camera.md` §8.10): a divisão (pura) e a sessão de vídeo.
pub mod camera;
pub mod divisao;
pub mod geometria;
pub mod regras;
pub mod sessao;
pub mod tela;
pub mod texto;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use quall_core::protocol::Papel;
use quall_core::teleprompter::Teleprompter;

use crate::{identidade, registro};

pub use regras::Lado;
pub use tela::{aberta, abrir, correr, fechar_se_aberta, papel_aberto, ConfigDaTela, ConfigDaTelaR5, PapelAberto};

static REPLICAS: OnceLock<Mutex<Vec<(Lado, Arc<Teleprompter>)>>> = OnceLock::new();

fn arquivo_do_salvo(lado: Lado) -> PathBuf {
    identidade::pasta_de_dados().join(match lado {
        Lado::Prompter => "teleprompter-prompter.json",
        Lado::Controle => "teleprompter-controle.json",
    })
}

/// A réplica do papel: a mesma a cada sessão, criada na primeira vez a partir do salvo.
pub fn replica(lado: Lado) -> Result<Arc<Teleprompter>, String> {
    let mut v = REPLICAS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some((_, t)) = v.iter().find(|(l, _)| *l == lado) {
        return Ok(Arc::clone(t));
    }
    let autor = identidade::device_id();
    let papel = match lado {
        Lado::Prompter => Papel::Teleprompter,
        Lado::Controle => Papel::ControleRemoto,
    };
    let caminho = arquivo_do_salvo(lado);
    let t = match std::fs::read_to_string(&caminho) {
        Ok(json) => match Teleprompter::de_salvo(&autor, papel, &json) {
            Ok(t) => {
                registro::linha(format!("teleprompter: réplica do {lado:?} lida de {}", caminho.display()));
                t
            }
            Err(e) => {
                // O ilegível é **guardado de lado**, e não sobrescrito pela primeira gravação: é
                // o roteiro de alguém.
                let segundos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let de_lado = caminho.with_extension(format!("recusado-{segundos}.json"));
                let _ = std::fs::rename(&caminho, &de_lado);
                registro::linha(format!(
                    "teleprompter: !! o salvo do {lado:?} foi recusado ({e}); guardado em {} — a réplica começa do padrão",
                    de_lado.display()
                ));
                Teleprompter::nova(&autor, papel).map_err(|e| e.to_string())?
            }
        },
        Err(_) => Teleprompter::nova(&autor, papel).map_err(|e| e.to_string())?,
    };
    let t = Arc::new(t);
    v.push((lado, Arc::clone(&t)));
    Ok(t)
}

/// **Os ajustes locais do prompter** (`docs/teleprompter-ajustes-locais.md`: o enquadramento e a
/// fonte automática): num arquivo à parte, `teleprompter-ajustes.json`, fora do salvo do núcleo —
/// o controle não os vê, e nenhuma mensagem nova vai pelo fio.
fn arquivo_dos_ajustes() -> PathBuf {
    identidade::pasta_de_dados().join("teleprompter-ajustes.json")
}

/// Ausente ou ilegível: o padrão (a largura inteira, fonte automática desligada).
pub fn ler_ajustes() -> regras::AjustesLocais {
    std::fs::read_to_string(arquivo_dos_ajustes())
        .map(|t| regras::AjustesLocais::de_json(&t))
        .unwrap_or_default()
}

pub fn gravar_ajustes(a: &regras::AjustesLocais) {
    let destino = arquivo_dos_ajustes();
    let provisorio = destino.with_extension("json.gravando");
    let r = std::fs::write(&provisorio, a.para_json()).and_then(|_| std::fs::rename(&provisorio, &destino));
    if let Err(e) = r {
        registro::linha(format!("teleprompter: !! não consegui gravar {}: {e}", destino.display()));
    }
}

/// Grava o salvo da réplica do papel — por troca de arquivo, e **nunca vazio** (um salvo vazio
/// faria a próxima vida nascer sem o roteiro).
pub fn salvar(lado: Lado) {
    let t = {
        let v = REPLICAS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        v.iter().find(|(l, _)| *l == lado).map(|(_, t)| Arc::clone(t))
    };
    let Some(t) = t else { return };
    let json = match t.salvo_json() {
        Ok(j) if !j.is_empty() => j,
        Ok(_) => return registro::linha("teleprompter: !! salvo vazio — nada gravado"),
        Err(e) => return registro::linha(format!("teleprompter: !! salvo_json falhou: {e}")),
    };
    let destino = arquivo_do_salvo(lado);
    let provisorio = destino.with_extension("json.gravando");
    let r = std::fs::write(&provisorio, json).and_then(|_| std::fs::rename(&provisorio, &destino));
    if let Err(e) = r {
        registro::linha(format!("teleprompter: !! não consegui gravar {}: {e}", destino.display()));
    }
}
