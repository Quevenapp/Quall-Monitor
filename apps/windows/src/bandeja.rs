//! **O ícone do Quall na área de notificação** (a "bandeja"), desde 01/10/2026.
//!
//! O pedido do dono, depois de usar a tela estendida por mais de uma hora com dois aparelhos: "deixar
//! o Quall minimizado no tray" — a janela fora do caminho, **com as sessões vivas**.
//!
//! # O desenho
//!
//! - O ícone existe enquanto o app roda, desde a abertura, com o ícone do próprio exe (o
//!   `RT_GROUP_ICON` 1 que `build.rs` embute a partir do `quall.ico`, no tamanho pequeno do sistema).
//!   A dica diz "Quall" e, com uma sessão de pé, uma linha de estado (`regras_da_bandeja::dica`).
//! - **Minimizar esconde a janela** (sai da barra de tarefas) e fica só o ícone. Nada é encerrado:
//!   transmissão, tela estendida, câmera, exibição e teleprompter seguem, porque nada disso depende
//!   de a janela estar à vista (o relógio dela continua batendo escondido).
//! - Clique esquerdo, duplo clique, Enter ou Espaço no ícone (ou clique no aviso) trazem a janela de
//!   volta, no lugar e no tamanho de antes, na frente. O direito abre o menu: a linha de estado
//!   (apagada), "Abrir o Quall", e "Sair do Quall" — que é o X: o mesmo `WM_CLOSE`.
//! - **O X não muda**: fecha encerrando as sessões (a decisão registrada no `WM_CLOSE` de
//!   `janela.rs`). Quem quer o Quall fora do caminho minimiza.
//! - Na primeira ida para a bandeja (uma vez por pasta de dados: a marca é `bandeja-avisou.txt` em
//!   `identidade::pasta_de_dados`, ao lado de `pares.json`), o ícone mostra um aviso. No Windows 11 um
//!   ícone novo cai no menu escondido (o "^"), e sem o aviso a pessoa não acha onde o Quall foi parar.
//!
//! # O que pode dar errado, e o que foi feito
//!
//! - **Sem área de notificação** (a Sessão 0 do SSH, o Explorer fora do ar) o `Shell_NotifyIconW`
//!   falha: vira uma linha de registro, e minimizar volta a ser o de sempre. Esconder a janela sem
//!   ícone seria fazê-la sumir sem caminho de volta (`regras_da_bandeja::ao_minimizar`).
//! - **O Explorer reiniciado** apaga os ícones de todo mundo e avisa com a mensagem registrada
//!   `TaskbarCreated`: o ícone volta por ela, e por ela entra também quando o app abriu antes da barra.
//!   Sem o ícone posto, cada troca de dica também tenta pô-lo de novo.
//! - **A janela escondida que perde o ícone** (o `TaskbarCreated` ou a troca de dica tentam repô-lo,
//!   e não dá): ela volta minimizada para a barra de tarefas, sem roubar o foco (`Bandeja::repor`).
//!   Nunca fica inalcançável com as sessões vivas.
//! - **Processo elevado**: a UIPI barraria o `TaskbarCreated` e a mensagem do ícone, que vêm do
//!   Explorer (não elevado), e a da instância única (`instancia.rs`), que vem de uma segunda abertura
//!   pelo atalho. As três são liberadas na janela (`ChangeWindowMessageFilterEx`).
//! - **O menu que não fecha** (o defeito clássico, KB135788): sem a janela em primeiro plano antes do
//!   `TrackPopupMenu`, clicar fora não fecha o menu; e sem um `WM_NULL` depois, ele reabre mal na
//!   segunda vez. Os dois estão em [`abrir_menu`].
//! - **O laço modal do menu despacha o relógio da janela**, e o `--sair-apos` da bancada pode
//!   destruí-la ali dentro (e soltar a `Janela`). Por isso o menu é função solta, que só recebe o
//!   `hwnd` e o texto, e quem a chama não toca na `Janela` depois que ela volta.
//! - **A janela escondida num monitor que saiu** (um monitor virtual da tela estendida, para onde a
//!   pessoa a arrastou): [`restaurar`] a traz para o monitor principal em vez de reabri-la no vazio.
//! - **O fantasma**: o ícone sai no `Drop` (o `WM_DESTROY` da janela solta a `Bandeja`). Um processo
//!   morto por fora (`TerminateProcess`) ainda deixa o ícone até o mouse passar por cima; isso não
//!   tem conserto do lado de cá.
//!
//! Duas instâncias do app põem dois ícones (o par `hWnd` + `uID` é diferente; não usamos `guidItem`,
//! que amarraria o ícone ao caminho do exe e faria as duas brigarem pelo mesmo).

use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_NOSOUND, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4, NOTIFY_ICON_DATA_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, ChangeWindowMessageFilterEx, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, GetSystemMetrics, GetWindowRect,
    IsIconic, IsWindowVisible, LoadIconW, SW_SHOWMINNOACTIVE, LoadImageW, PostMessageW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, SetWindowPos, ShowWindow,
    TrackPopupMenu, HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSGFLT_ALLOW, SM_CXSMICON,
    SM_CYSMICON, SM_MENUDROPALIGNMENT, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_RESTORE, SW_SHOW, TPM_LEFTALIGN, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

use crate::regras_da_bandeja as regras;
use crate::{identidade, registro};

/// **A mensagem do ícone**: o Explorer a manda para a janela a cada clique, tecla ou aviso. A
/// `WM_APP + 1` é o `WM_ATUALIZAR` da janela (`janela.rs`), que confere a diferença em tempo de
/// compilação.
pub const WM_BANDEJA: u32 = WM_APP + 2;

/// O id do ícone (o `uID`): um só por janela.
const ID_DO_ICONE: u32 = 1;

/// Os itens do menu. Com `TPM_RETURNCMD | TPM_NONOTIFY` nenhum `WM_COMMAND` sai do menu, então eles
/// não colidem com os ids dos controles; ficam longe deles mesmo assim.
const ID_ESTADO: usize = 0x7001;
const ID_ABRIR: usize = 0x7002;
const ID_SAIR: usize = 0x7003;

/// O que a pessoa escolheu no menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escolha {
    Abrir,
    Sair,
    Nada,
}

pub struct Bandeja {
    hwnd: HWND,
    icone: HICON,
    /// O ícone foi carregado do recurso do exe e é destruído no fim; o genérico do sistema
    /// (`IDI_APPLICATION`, a reserva) é compartilhado e não pode ser.
    icone_proprio: bool,
    /// O ícone está na área de notificação agora.
    posta: bool,
    /// O `NIM_SETVERSION` com `NOTIFYICON_VERSION_4` foi aceito (o formato dos avisos depende disto).
    versao_4: bool,
    /// A dica como foi posta (para só mandar `NIM_MODIFY` quando ela muda).
    dica: String,
    /// O número de `TaskbarCreated` (0 se o registro falhou).
    msg_da_barra: u32,
    /// O número de `instancia::MENSAGEM_MOSTRAR` (0 se o registro falhou): outra abertura do Quall
    /// pelo atalho pede a janela por ela.
    msg_mostrar: u32,
    /// O aviso da primeira vez já saiu (a marca existe na pasta de dados).
    ja_avisou: bool,
    /// A falha do `NIM_ADD` já foi dita no registro (até o próximo sucesso): sem área de
    /// notificação (a Sessão 0), cada troca de dica tenta de novo, e diria de novo.
    falha_dita: bool,
}

impl Bandeja {
    /// Põe o ícone na área de notificação, com a dica dada. Nunca falha: sem área de notificação, a
    /// bandeja fica "não posta" (o registro diz), e o `TaskbarCreated` tenta de novo quando a barra
    /// aparecer.
    pub fn nova(hwnd: HWND, dica: &str) -> Bandeja {
        let msg_da_barra = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let msg_mostrar = unsafe { RegisterWindowMessageW(crate::instancia::MENSAGEM_MOSTRAR) };
        for m in [msg_da_barra, WM_BANDEJA, msg_mostrar] {
            if m != 0 {
                // Só pesa com o processo elevado; sem elevação a chamada não muda nada.
                let _ = unsafe { ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None) };
            }
        }
        let (icone, icone_proprio) = carregar_o_icone();
        let mut b = Bandeja {
            hwnd,
            icone,
            icone_proprio,
            posta: false,
            versao_4: false,
            dica: dica.to_string(),
            msg_da_barra,
            msg_mostrar,
            ja_avisou: marca().exists(),
            falha_dita: false,
        };
        b.por();
        b
    }

    pub fn posta(&self) -> bool {
        self.posta
    }

    pub fn versao_4(&self) -> bool {
        self.versao_4
    }

    pub fn ja_avisou(&self) -> bool {
        self.ja_avisou
    }

    /// O número da mensagem `TaskbarCreated` (0 se o registro falhou: aí não há o que esperar).
    pub fn msg_da_barra(&self) -> u32 {
        self.msg_da_barra
    }

    /// O número da mensagem que pede a janela de volta (`instancia.rs`; 0 se o registro falhou).
    pub fn msg_mostrar(&self) -> u32 {
        self.msg_mostrar
    }

    /// Os campos que todo pedido leva: o tamanho, a janela, o id, e a dica com `NIF_SHOWTIP` — na
    /// versão 4, um `NIM_MODIFY` sem ele pode trocar a dica comum pela "rica", que nós não desenhamos.
    fn dados(&self, bandeiras: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ID_DO_ICONE,
            uFlags: bandeiras | NIF_TIP | NIF_SHOWTIP,
            ..Default::default()
        };
        copiar(&mut d.szTip, &self.dica);
        d
    }

    /// `NIM_ADD` e `NIM_SETVERSION`. `true` se o ícone está na área de notificação.
    fn por(&mut self) -> bool {
        let mut d = self.dados(NIF_MESSAGE | NIF_ICON);
        d.uCallbackMessage = WM_BANDEJA;
        d.hIcon = self.icone;
        let mut ok = unsafe { Shell_NotifyIconW(NIM_ADD, &d) }.as_bool();
        if !ok {
            // Com o Explorer ocupado o `NIM_ADD` pode voltar falso por prazo vencido e o ícone entrar
            // assim mesmo: um `NIM_MODIFY` confere (e falha também se ele de fato não existe).
            let erro = unsafe { GetLastError() }.0;
            ok = unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) }.as_bool();
            if !ok && !self.falha_dita {
                self.falha_dita = true;
                registro::linha(format!(
                    "bandeja: !! o ícone não entrou na área de notificação (GetLastError {erro}); minimizar fica na barra de tarefas"
                ));
            }
        }
        if ok {
            self.falha_dita = false;
        }
        self.versao_4 = false;
        if ok {
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            self.versao_4 = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &d) }.as_bool();
            registro::linha(format!(
                "bandeja: ícone posto na área de notificação ({})",
                if self.versao_4 { "versão 4" } else { "!! versão 4 recusada; formato antigo dos avisos" }
            ));
        }
        self.posta = ok;
        ok
    }

    /// **Põe o ícone de novo depois da abertura**, e, se não der, não deixa a janela escondida sem
    /// ícone: sem ele, a janela escondida na bandeja ficaria inalcançável (nem ícone, nem botão na
    /// barra de tarefas) com as sessões vivas. Ela volta minimizada para a barra de tarefas, sem
    /// roubar o foco. Só depois da abertura: no `nova`, a janela ainda não apareceu, e o primeiro
    /// `ShowWindow` é o da janela.
    fn repor(&mut self) {
        if self.por() {
            return;
        }
        if !unsafe { IsWindowVisible(self.hwnd) }.as_bool() {
            registro::linha("bandeja: !! sem o ícone, a janela escondida volta minimizada para a barra de tarefas");
            // Por último: o `WM_SIZE` que isto manda já vê a bandeja não posta, e não a esconde de novo.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWMINNOACTIVE);
            }
        }
    }

    /// **O Explorer recriou a barra** (`TaskbarCreated`): os ícones de antes sumiram com ele.
    pub fn barra_recriada(&mut self) {
        registro::linha("bandeja: o Explorer recriou a barra de tarefas (TaskbarCreated); o ícone volta");
        self.posta = false;
        self.repor();
    }

    /// Troca a dica, só quando ela muda. Sem o ícone posto (ele não entrou na abertura, ou sumiu), é
    /// também a vez de tentar pô-lo de novo, já com a dica nova.
    pub fn mudar_dica(&mut self, dica: &str) {
        if dica == self.dica {
            return;
        }
        self.dica = dica.to_string();
        if !self.posta {
            self.repor();
            return;
        }
        let d = self.dados(NOTIFY_ICON_DATA_FLAGS(0));
        if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) }.as_bool() {
            // O ícone sumiu sem `TaskbarCreated` (o Explorer caiu e ainda não voltou).
            registro::linha("bandeja: !! a dica não mudou (o ícone sumiu?); pondo o ícone de novo");
            self.posta = false;
            self.repor();
        }
    }

    /// **O aviso da primeira vez.** A marca é gravada quando o Explorer aceita o pedido — e não
    /// quando o aviso aparece: com as notificações desligadas ou o "Não incomodar" ligado, o Windows
    /// aceita e não mostra, e esperar o `NIN_BALLOONSHOW` faria o aviso tentar de novo a cada
    /// minimização. O `NIN_BALLOONSHOW` vai para o registro, como prova de que ele saiu.
    pub fn avisar(&mut self) {
        let mut d = self.dados(NIF_INFO);
        copiar(&mut d.szInfoTitle, regras::TITULO_DO_AVISO);
        copiar(&mut d.szInfo, crate::idioma::t(regras::TEXTO_DO_AVISO));
        // `NIIF_USER`: o ícone do aviso é o do Quall (do Vista em diante ele vem de `hBalloonIcon`).
        // Sem som: quem minimizou não pediu um toque.
        d.dwInfoFlags = NIIF_USER | NIIF_NOSOUND;
        d.hBalloonIcon = self.icone;
        if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) }.as_bool() {
            registro::linha("bandeja: !! o aviso da primeira vez não foi aceito; ele tenta de novo na próxima minimização");
            return;
        }
        self.ja_avisou = true;
        // i18n: fora (o conteúdo do arquivo da marca, que ninguém vê na tela)
        let texto = "O aviso \"o Quall continua rodando aqui\" da bandeja já saiu nesta conta.\n\
                     Apagar este arquivo faz ele sair de novo na próxima minimização.\n";
        match std::fs::write(marca(), texto) {
            Ok(()) => registro::linha("bandeja: aviso da primeira vez pedido; a marca foi gravada"),
            Err(e) => registro::linha(format!("bandeja: !! aviso da primeira vez pedido, mas a marca não foi gravada ({e})")),
        }
    }
}

impl Drop for Bandeja {
    /// O ícone sai junto com a janela (o `WM_DESTROY` solta a `Janela`, e com ela isto): sem o
    /// `NIM_DELETE` ficaria um fantasma na área de notificação até o mouse passar por cima. Pedido
    /// sempre, posto ou não: um `NIM_ADD` que "falhou" por prazo pode ter entrado.
    fn drop(&mut self) {
        let d = self.dados(NOTIFY_ICON_DATA_FLAGS(0));
        let tirou = unsafe { Shell_NotifyIconW(NIM_DELETE, &d) }.as_bool();
        if self.posta {
            registro::linha(if tirou { "bandeja: ícone tirado" } else { "bandeja: !! o NIM_DELETE falhou na saída" });
        }
        if self.icone_proprio {
            let _ = unsafe { DestroyIcon(self.icone) };
        }
    }
}

/// O arquivo da marca do aviso da primeira vez, na pasta de dados (`%APPDATA%\Quall`, ou a desviada
/// pela bancada com `--dados`: uma corrida de bancada não gasta o aviso do usuário).
fn marca() -> PathBuf {
    identidade::pasta_de_dados().join("bandeja-avisou.txt")
}

/// **O ícone do exe** no tamanho pequeno do sistema (`SM_CXSMICON` no DPI do sistema, que é o que o
/// `LoadIconMetric(LIM_SMALL)` usaria: 16 a 100 %, 20 a 125 %, 24 a 150 %, 32 a 200 % — o `quall.ico`
/// tem todos esses, então o Windows não estica nada). `LoadImageW`, do `user32`, e não o
/// `LoadIconMetric`, que só existe no `comctl32` versão 6 e amarraria o carregamento ao manifesto.
/// Sem o recurso (um binário que não é o `quall-app`), o genérico do sistema.
fn carregar_o_icone() -> (HICON, bool) {
    unsafe {
        let dpi = GetDpiForSystem().max(96);
        let (l, a) = (GetSystemMetricsForDpi(SM_CXSMICON, dpi), GetSystemMetricsForDpi(SM_CYSMICON, dpi));
        let erro = match GetModuleHandleW(None) {
            // `MAKEINTRESOURCE(1)`: o `RT_GROUP_ICON` 1 do `build.rs`.
            Ok(modulo) => match LoadImageW(Some(modulo.into()), PCWSTR(1usize as *const u16), IMAGE_ICON, l, a, LR_DEFAULTCOLOR) {
                Ok(h) if !h.is_invalid() => return (HICON(h.0), true),
                Ok(_) => "LoadImageW devolveu nulo".to_string(), // i18n: fora (diário)
                Err(e) => e.to_string(),
            },
            Err(e) => e.to_string(),
        };
        registro::linha(format!("bandeja: !! o ícone do exe não carregou ({l}x{a}: {erro}); vai o genérico do sistema"));
        (LoadIconW(None, IDI_APPLICATION).unwrap_or_default(), false)
    }
}

/// Copia o texto para um campo de tamanho fixo do `NOTIFYICONDATAW`, sempre com o nulo no fim. O
/// texto já chega cortado (`regras_da_bandeja::cortar`); o corte daqui é só a última guarda.
fn copiar(destino: &mut [u16], texto: &str) {
    let mut i = 0;
    for u in texto.encode_utf16() {
        if i + 1 >= destino.len() {
            break;
        }
        destino[i] = u;
        i += 1;
    }
    if let Some(fim) = destino.get_mut(i) {
        *fim = 0;
    }
}

fn largo(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Esconde a janela: sai da barra de tarefas e do Alt+Tab, e fica só o ícone.
pub fn esconder(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// **Traz a janela de volta**, no lugar e no tamanho de antes, e na frente. Escondida pela
/// interceptação do `SC_MINIMIZE` ela não chegou a minimizar, e o `SW_SHOW` basta; escondida depois
/// de minimizada (o `WM_SIZE` de um Win+M), o `SW_RESTORE` a desminimiza para o retângulo de antes.
pub fn restaurar(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        trazer_para_um_monitor(hwnd);
        // O clique no ícone dá a este processo o direito de vir para a frente (o Explorer o concede a
        // quem é dono do ícone clicado); fora disso, o Windows só pisca o botão.
        let _ = SetForegroundWindow(hwnd);
    }
}

/// A janela que voltou fora de qualquer monitor (o monitor em que ela estava saiu enquanto ela
/// estava escondida) vai para o meio da área de trabalho do principal. O `WM_DPICHANGED` cuida do
/// tamanho, se o DPI de lá for outro.
fn trazer_para_um_monitor(hwnd: HWND) {
    unsafe {
        if !MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL).is_invalid() {
            return;
        }
        let principal = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut r = RECT::default();
        if !GetMonitorInfoW(principal, &mut info).as_bool() || GetWindowRect(hwnd, &mut r).is_err() {
            return;
        }
        let t = info.rcWork;
        let (l, a) = (r.right - r.left, r.bottom - r.top);
        let x = t.left + ((t.right - t.left) - l).max(0) / 2;
        let y = t.top + ((t.bottom - t.top) - a).max(0) / 2;
        let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        registro::linha("bandeja: a janela voltou fora de qualquer monitor; foi para o meio do principal");
    }
}

/// **O menu do ícone**: a linha de estado (apagada), "Abrir o Quall" (o padrão, em negrito),
/// separador, "Sair do Quall". `ponto` é a âncora que o Explorer mandou (versão 4), ou `None` para o
/// cursor.
///
/// Função solta, de propósito: o laço modal do `TrackPopupMenu` despacha as mensagens da janela
/// (o relógio, o `--sair-apos` que a destrói), e daqui para a frente só o `hwnd` é usado.
pub fn abrir_menu(hwnd: HWND, ponto: Option<(i32, i32)>, linha: &str) -> Escolha {
    unsafe {
        let menu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(e) => {
                registro::linha(format!("bandeja: !! o menu não nasceu ({e})"));
                return Escolha::Nada;
            }
        };
        let linha = largo(linha);
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, ID_ESTADO, PCWSTR(linha.as_ptr()));
        // O menu nasce a cada clique: sai no idioma de agora, sem nada a refazer na troca.
        let abrir = largo(crate::idioma::t("Abrir o Quall"));
        let sair = largo(crate::idioma::t("Sair do Quall"));
        let _ = AppendMenuW(menu, MF_STRING, ID_ABRIR, PCWSTR(abrir.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_SAIR, PCWSTR(sair.as_ptr()));
        let _ = SetMenuDefaultItem(menu, ID_ABRIR as u32, 0);
        let (x, y) = ponto.unwrap_or_else(|| {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            (p.x, p.y)
        });
        // O lado para onde o menu abre segue a preferência do sistema (a dos canhotos), como o
        // exemplo da Microsoft para o ícone de notificação.
        let lado = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
        // KB135788: sem a janela em primeiro plano, clicar fora não fecha o menu.
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(menu, lado | TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, x, y, None, hwnd, None).0 as usize;
        // KB135788, a outra metade: sem uma mensagem depois, o menu da vez seguinte abre e fecha.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match id {
            ID_ABRIR => Escolha::Abrir,
            ID_SAIR => Escolha::Sair,
            _ => Escolha::Nada,
        }
    }
}
