//! O registro do app: uma linha por evento, com carimbo de relógio, em arquivo **e** no stderr.
//!
//! # Por que o arquivo é obrigatório, e não um luxo
//!
//! A prova de qualquer coisa visual neste projeto passa pela **sessão interativa** do Dell
//! (`docs/windows-acesso.md`): o SSH cai na Sessão 0, que tem área de trabalho de serviço própria
//! e onde o `Windows.Graphics.Capture` sequer roda (`0x80070424`). O único caminho de automação é
//! Tarefa Agendada com `LogonType Interactive` — e uma tarefa agendada **não tem terminal
//! herdado**: o que for para o stdout vai para lugar nenhum.
//!
//! É a mesma razão que fez o irmão do macOS escrever em `~/Library/Logs/Quall` (o `open -n -W -a`
//! também não herda terminal). Sem arquivo, o app de produto é mudo exatamente na única sessão em
//! que ele consegue rodar.

use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

struct Destino {
    arquivo: Option<std::fs::File>,
    caminho: Option<PathBuf>,
}

static DESTINO: OnceLock<Mutex<Destino>> = OnceLock::new();

/// Abre o registro. Chamado uma vez, no começo do processo.
///
/// Falhar em abrir o arquivo **não** derruba o app: um produto que não sobe porque não conseguiu
/// escrever um log é pior que um produto sem log. Nesse caso sobra o stderr redirecionável;
/// na abertura normal, `quall-app` também avisa em uma mensagem visível. Reabrir devolve o
/// destino já configurado, sem trocar um `--registro` pelo caminho padrão.
pub fn abrir(caminho: Option<&Path>) -> Option<PathBuf> {
    if let Some(destino) = DESTINO.get() {
        return destino.lock().ok().and_then(|d| d.caminho.clone());
    }
    let caminho = match caminho {
        Some(c) => c.to_path_buf(),
        None => padrao(),
    };
    if let Some(pai) = caminho.parent() {
        let _ = create_dir_all(pai);
    }
    let arquivo = OpenOptions::new().create(true).append(true).open(&caminho).ok();
    let deu = arquivo.is_some();
    let _ = DESTINO.set(Mutex::new(Destino { arquivo, caminho: deu.then(|| caminho.clone()) }));
    if deu {
        Some(caminho)
    } else {
        None
    }
}

/// O arquivo de registro sem `--registro`: `%LOCALAPPDATA%\Quall\Logs\quall-app.log`. A janela mostra
/// a pasta dele nos Ajustes.
pub fn padrao() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Quall Monitor").join("Logs").join("quall-monitor.log")
}

thread_local! {
    /// O prefixo das linhas **desta thread**: `[#3] ` na thread da sessão 3 do emissor com vários
    /// receptores, vazio em todo o resto.
    ///
    /// Por thread, e não por chamada, porque quem escreve as linhas de uma sessão não é só o laço
    /// dela: a `Cadeia`, o encoder e a oficina escrevem pelo mesmo `linha`, sem saber de sessão
    /// nenhuma. Na thread da sessão elas saem com o prefixo sem que nenhum desses módulos mude. A
    /// forma `[#n] ` é a do Mac (`SessaoDeEmissao.swift`), e ainda casa com os `regex` dos
    /// roteiros de bancada, que procuram o texto depois do carimbo sem âncora.
    ///
    /// Sem prefixo — o caminho de uma sessão só, que é o produto de hoje — as linhas são as de
    /// antes, byte a byte.
    static PREFIXO: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Dá prefixo às linhas desta thread (vazio tira).
pub fn prefixar_esta_thread(prefixo: &str) {
    PREFIXO.with(|p| *p.borrow_mut() = prefixo.to_string());
}

/// O prefixo desta thread — para uma thread filha herdar o da mãe.
pub fn prefixo_desta_thread() -> String {
    PREFIXO.with(|p| p.borrow().clone())
}

/// Uma linha no registro. Segura de qualquer thread.
pub fn linha(texto: impl AsRef<str>) {
    let texto = crate::higiene_do_registro::sanitizar(texto.as_ref());
    let carimbo = carimbo_local();
    let prefixo = crate::higiene_do_registro::sanitizar(&prefixo_desta_thread());
    eprintln!("{carimbo}  {prefixo}{texto}");
    if let Some(trava) = DESTINO.get() {
        if let Ok(mut d) = trava.lock() {
            if let Some(arq) = d.arquivo.as_mut() {
                let _ = writeln!(arq, "{carimbo}  {prefixo}{texto}");
                let _ = arq.flush();
            }
        }
    }
}

/// A mesma coisa que [`linha`], mas com assinatura de ponteiro de função `fn(&str)`.
///
/// Existe só porque `quall_core::transport::ativar_registro_da_biblioteca` recebe um `fn(&str)`
/// (e não um `impl AsRef<str>`, que não tem representação de ponteiro). Uma linha de cola, e não
/// um segundo caminho de registro: ela chama `linha`.
pub fn linha_estatica(texto: &str) {
    linha(texto);
}

/// `HH:MM:SS.mmm` derivado do relógio de parede, sem dependência de formatação de data.
///
/// Só a hora do dia: a data não ajuda a casar duas máquinas numa corrida de bancada, e o que
/// importa é alinhar este registro com o do receptor do outro lado, que dura segundos.
fn carimbo_local() -> String {
    let agora = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_ms = agora.as_millis() as u64;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let h = (total_s / 3600) % 24;
    let m = (total_s / 60) % 60;
    let s = total_s % 60;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}Z")
}
