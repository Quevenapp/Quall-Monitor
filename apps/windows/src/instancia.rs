//! **Um Quall de produto por sessão do Windows** (01/10/2026), só na abertura de produto
//! (`regras_da_bandeja::abertura_de_produto`: o exe aberto sem argumento nenhum).
//!
//! # Por que
//!
//! Com a bandeja, o Quall minimizado some da barra de tarefas, e a pessoa que o procura abre pelo
//! atalho de novo. Antes disto, isso abria um segundo Quall. E isso não é só confuso: **medido no
//! Dell em 01/10, duas instâncias abertas juntas brigaram pelas câmeras virtuais** — a segunda falhou
//! com "MFCreateVirtualCamera: um nó do mesmo nome de uma execução anterior não saiu a tempo", porque
//! cada uma cria um nó por aparelho pareado com o mesmo nome (`baias.rs`), e os dois disputariam
//! também a porta e o anúncio mDNS.
//!
//! # Como
//!
//! - Um mutex nomeado no espaço `Local\` (por sessão: outra conta logada no mesmo PC tem o seu
//!   Quall), com o identificador do app no nome. Quem o cria é a instância de produto, e o segura até
//!   o processo sair (a [`Guarda`] vive até o fim do `main`).
//! - Quem chega e o acha existindo procura a janela principal da outra (a classe da janela, e a
//!   propriedade [`PROPRIEDADE`], que só a janela da instância de produto tem — uma corrida de
//!   bancada aberta ao lado tem a mesma classe), dá a ela o direito de vir para a frente
//!   (`AllowSetForegroundWindow`: quem acabou de ser aberto pelo atalho tem esse direito, e o passa),
//!   manda a mensagem registrada [`MENSAGEM_MOSTRAR`] — que a janela trata como o clique no ícone da
//!   bandeja (`bandeja::restaurar`), escondida ou não —, escreve uma linha no registro e sai com
//!   código 0 **sem abrir nada**: tudo isto roda no começo do `main`, antes do COM, das câmeras
//!   virtuais, do mDNS e da janela.
//! - **A outra sem janela** está abrindo (a janela ainda não nasceu) ou fechando (a janela já se
//!   foi, e o desmonte do `main` ainda roda: até ~90 s de prazos somados). Quem chega espera, calado,
//!   até [`ESPERA_PELA_OUTRA`]: se a janela aparecer, chama-a; se o mutex for solto (a outra saiu),
//!   segue como a instância de produto e abre normalmente. Abrir por cima de uma que fecha era
//!   justamente a briga das câmeras. Passado o prazo, **diz** por que não abriu (uma caixa de
//!   mensagem: esperar calado mais que isso parecia um clique perdido) e sai.
//! - **A outra elevada** (aberta como administrador): o mutex dela não se deixa abrir daqui
//!   (`ERROR_ACCESS_DENIED`), o que também quer dizer "existe". A mensagem chega mesmo assim, porque
//!   a janela a libera da UIPI (`bandeja::Bandeja::nova`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE, HWND, LPARAM, WAIT_ABANDONED,
    WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowExW, GetPropW, GetWindowThreadProcessId, MessageBoxW, PostMessageW, RegisterWindowMessageW,
    MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
};

use crate::registro;

/// O mutex da instância de produto, por sessão (`Local\`), com o identificador do app.
const NOME_DO_MUTEX: PCWSTR = w!("Local\\br.com.queven.quall.monitor.instancia-de-produto");

/// A mensagem registrada que pede à janela da instância de produto que volte (`bandeja::restaurar`).
pub const MENSAGEM_MOSTRAR: PCWSTR = w!("QuallMonitor-MostrarJanela"); // i18n: fora (nome de mensagem registrada)

/// A propriedade (`SetPropW`) que marca a janela principal da instância de produto.
pub const PROPRIEDADE: PCWSTR = w!("QuallMonitor.InstanciaDeProduto"); // i18n: fora (nome de propriedade)

/// Quanto quem chega espera, calado, uma instância sem janela (abrindo ou fechando) antes de dizer
/// por que não abre. O desmonte comum (uma sessão, sem câmera gravando) cabe aqui; o pior caso do
/// `main` (~90 s de prazos somados) não, e esperar calado tudo isso parecia um clique perdido.
pub const ESPERA_PELA_OUTRA: Duration = Duration::from_secs(15);

/// O que a caixa diz quando a outra não mostrou janela nem saiu no prazo.
/// Em português, como chave da tabela; a caixa a mostra no idioma de agora (`idioma::t`).
const FRASE_DA_ESPERA: &str = "O Quall ainda está fechando a sessão anterior. Espere um instante e abra de novo."; // i18n: chave

/// Este processo é a instância de produto (a janela se marca com [`PROPRIEDADE`]).
static DE_PRODUTO: AtomicBool = AtomicBool::new(false);

pub fn de_produto() -> bool {
    DE_PRODUTO.load(Ordering::SeqCst)
}

/// O mutex, segurado até o fim do `main` (na thread dele: o dono de um mutex é a thread).
pub struct Guarda(Option<HANDLE>);

impl Drop for Guarda {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            unsafe {
                let _ = ReleaseMutex(h);
                let _ = CloseHandle(h);
            }
        }
    }
}

pub enum Instancia {
    /// Esta é a instância de produto (sem mutex, se ele não nasceu: o registro diz, e o app abre).
    Primeira(Guarda),
    /// Outra instância de produto está aberta, e foi chamada (ou não respondeu a tempo): sair.
    OutraAberta,
}

/// **Toma a instância de produto, ou chama a que já existe.** Só para a abertura de produto.
pub fn tomar() -> Instancia {
    unsafe {
        // O `CreateMutexW` diz "já existia" pelo `GetLastError` de um retorno bem-sucedido: zerado
        // antes, para um erro velho da thread não passar por esse aviso.
        SetLastError(ERROR_SUCCESS);
        let criado = CreateMutexW(None, true, NOME_DO_MUTEX);
        let erro = GetLastError();
        match criado {
            Ok(h) if erro != ERROR_ALREADY_EXISTS => {
                DE_PRODUTO.store(true, Ordering::SeqCst);
                registro::linha("instância única: esta é a instância de produto");
                Instancia::Primeira(Guarda(Some(h)))
            }
            Ok(h) => esperar_ou_chamar(Some(h)),
            Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => esperar_ou_chamar(None),
            Err(e) => {
                registro::linha(format!("instância única: !! o mutex não nasceu ({e}); o app abre sem a guarda"));
                Instancia::Primeira(Guarda(None))
            }
        }
    }
}

/// Outra instância de produto existe: chama a janela dela, ou espera ela aparecer ou sair.
fn esperar_ou_chamar(mutex: Option<HANDLE>) -> Instancia {
    let fechar = |h: Option<HANDLE>| {
        if let Some(h) = h {
            let _ = unsafe { CloseHandle(h) };
        }
    };
    let fim = Instant::now() + ESPERA_PELA_OUTRA;
    let mut avisou_a_espera = false;
    loop {
        if let Some(janela) = janela_da_outra() {
            chamar(janela);
            fechar(mutex);
            return Instancia::OutraAberta;
        }
        if !avisou_a_espera {
            avisou_a_espera = true;
            registro::linha(format!(
                "instância única: outra instância de produto está aberta sem janela (abrindo ou fechando); esperando até {} s",
                ESPERA_PELA_OUTRA.as_secs()
            ));
        }
        match mutex {
            Some(h) => {
                let r = unsafe { WaitForSingleObject(h, 100) };
                if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
                    DE_PRODUTO.store(true, Ordering::SeqCst);
                    registro::linha("instância única: a instância anterior saiu; esta é a instância de produto agora");
                    return Instancia::Primeira(Guarda(Some(h)));
                }
            }
            // A outra é elevada e o mutex dela não se abre daqui: só a janela pode aparecer.
            None => std::thread::sleep(Duration::from_millis(100)),
        }
        if Instant::now() >= fim {
            registro::linha(format!(
                "instância única: !! a outra instância não mostrou janela nem saiu em {} s; esta avisa e sai sem abrir",
                ESPERA_PELA_OUTRA.as_secs()
            ));
            fechar(mutex);
            let frase = windows::core::HSTRING::from(crate::idioma::t(FRASE_DA_ESPERA));
            unsafe {
                MessageBoxW(None, PCWSTR(frase.as_ptr()), w!("Quall Monitor"), MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND); // i18n: fora (a marca)
            }
            return Instancia::OutraAberta;
        }
    }
}

/// A janela principal da instância de produto: as janelas de topo da classe do app (escondidas
/// também — a da bandeja está), a que tem a [`PROPRIEDADE`], de outro processo.
fn janela_da_outra() -> Option<HWND> {
    let eu = unsafe { GetCurrentProcessId() };
    let mut depois: Option<HWND> = None;
    // Um teto, contra uma lista que mudasse debaixo do laço.
    for _ in 0..64 {
        let h = unsafe { FindWindowExW(None, depois, crate::janela::CLASSE, PCWSTR::null()) }.ok()?;
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut pid));
        }
        if pid != eu && !unsafe { GetPropW(h, PROPRIEDADE) }.is_invalid() {
            return Some(h);
        }
        depois = Some(h);
    }
    None
}

/// Dá à outra o direito de vir para a frente e pede que a janela volte.
fn chamar(janela: HWND) {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(janela, Some(&mut pid));
        let deu_a_frente = AllowSetForegroundWindow(pid).is_ok();
        let msg = RegisterWindowMessageW(MENSAGEM_MOSTRAR);
        let postou = msg != 0 && PostMessageW(Some(janela), msg, WPARAM(0), LPARAM(0)).is_ok();
        registro::linha(format!(
            "instância única: o Quall já está aberto (processo {pid}); {} — esta abertura sai sem abrir nada{}",
            if postou { "a janela dele foi chamada" } else { "!! a mensagem para a janela dele não foi" },
            if deu_a_frente { "" } else { " (sem AllowSetForegroundWindow: ela pode só piscar na barra)" }
        ));
    }
}
