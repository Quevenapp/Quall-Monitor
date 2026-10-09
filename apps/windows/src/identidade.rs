//! Quem este computador é na rede, e de quem ele já se lembra.
//!
//! Espelha `apps/macos/Sources/QuallApp/Identidade.swift` em papel, não em forma: o macOS guarda
//! em `~/Library/Application Support/Quall`, aqui é `%APPDATA%\Quall`.
//!
//! # O pareamento é gravado com `merge`, mesmo havendo um escritor só
//!
//! Dívida 23. Hoje só o app escreve `pares.json`, então um `to_json` do que está em memória
//! bastaria. Mas o dia em que existir um segundo processo (o plugin de OBS, a câmera virtual, uma
//! sonda de bancada) o `to_json` puro apaga em silêncio o que o outro escreveu entre a leitura e a
//! escrita. `merge` custa uma releitura por sessão fechada — algo que acontece uma vez por
//! transmissão, não por quadro.

use std::path::PathBuf;

use quall_core::pairing::PairedPeers;

use crate::registro;

/// A variável que **desvia** a pasta de dados. Só a bancada a usa.
///
/// Uma sonda rodada pelo SSH do Dell cai no mesmo `%APPDATA%` do app do usuário: gravaria o
/// pareamento de receptores de sonda no `pares.json` dele, e as sondas dividiriam um `device-id`
/// só — com a regra "o mesmo aparelho reconectando encerra a sessão velha", duas sondas com a
/// mesma identidade nunca ficariam no ar juntas (revisão adversarial de 13/09/2026). O binário de
/// bancada (`quall-varias`) aponta isto para uma pasta descartável antes de qualquer outra coisa.
pub const VARIAVEL_DA_PASTA: &str = "QUALL_MONITOR_PASTA_DE_DADOS";

/// `%APPDATA%\Quall`, criada se não existir — ou a de [`VARIAVEL_DA_PASTA`], na bancada.
pub fn pasta_de_dados() -> PathBuf {
    if let Some(desviada) = std::env::var_os(VARIAVEL_DA_PASTA).filter(|v| !v.is_empty()) {
        let pasta = PathBuf::from(desviada);
        let _ = std::fs::create_dir_all(&pasta);
        return pasta;
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let pasta = base.join("Quall Monitor");
    let _ = std::fs::create_dir_all(&pasta);
    pasta
}

/// Quem lê-funde-grava `pares.json` entra por aqui, um de cada vez. Com várias sessões no mesmo
/// processo, duas pareando juntas liam o mesmo arquivo e a última gravação apagava o par da outra.
static ESCRITA_DOS_PARES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn caminho_dos_pares() -> PathBuf {
    pasta_de_dados().join("pares.json")
}

fn caminho_do_id() -> PathBuf {
    pasta_de_dados().join("device-id.txt")
}

/// O nome com que este computador aparece na lista dos outros aparelhos.
///
/// É o nome do computador que a pessoa já conhece (o mesmo de Configurações > Sistema > Sobre),
/// não um nome inventado por nós: quem está do outro lado da sala precisa reconhecer a máquina na
/// lista sem ter aprendido um segundo nome para ela.
pub fn nome_do_aparelho() -> String {
    use windows::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, GetComputerNameExW,
    };
    let mut tamanho: u32 = 0;
    unsafe {
        // Primeira chamada só para o tamanho: devolve erro e preenche `tamanho`.
        let _ = GetComputerNameExW(ComputerNameDnsHostname, None, &mut tamanho);
    }
    if tamanho == 0 {
        return format!("{} · Quall Monitor", fallback_de_nome());
    }
    let mut buffer = vec![0u16; tamanho as usize];
    let ok = unsafe {
        GetComputerNameExW(
            ComputerNameDnsHostname,
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            &mut tamanho,
        )
    }
    .is_ok();
    if !ok {
        return format!("{} · Quall Monitor", fallback_de_nome());
    }
    buffer.truncate(tamanho as usize);
    let nome = String::from_utf16_lossy(&buffer);
    if nome.trim().is_empty() {
        format!("{} · Quall Monitor", fallback_de_nome())
    } else {
        format!("{nome} · Quall Monitor")
    }
}

fn fallback_de_nome() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC com Windows".to_string())
}

/// Identificador estável deste aparelho, persistido no primeiro uso.
///
/// Sorteado com `CoCreateGuid` — que é o gerador de identificador do próprio sistema e já está ao
/// alcance (o processo inicializa COM de qualquer forma). A alternativa considerada era ler o
/// `MachineGuid` do registro: rejeitada porque é o **mesmo** valor para todas as contas e todos os
/// programas da máquina, e um id de aparelho que vaza identidade de máquina para fora do produto
/// é mais do que este produto precisa saber.
pub fn device_id() -> String {
    let caminho = caminho_do_id();
    if let Ok(texto) = std::fs::read_to_string(&caminho) {
        let texto = texto.trim().to_string();
        if !texto.is_empty() {
            return texto;
        }
    }
    let novo = sortear_id();
    if let Err(e) = std::fs::write(&caminho, &novo) {
        // Não é fatal: um id que muda a cada abertura só faz o outro lado pedir PIN de novo.
        registro::linha(format!(
            "aviso: não consegui gravar o device-id em {}: {e}",
            caminho.display()
        ));
    }
    novo
}

fn sortear_id() -> String {
    use windows::Win32::System::Com::CoCreateGuid;
    match unsafe { CoCreateGuid() } {
        Ok(g) => format!(
            "monitor-win-{:08x}{:04x}{:04x}",
            g.data1, g.data2, g.data3
        ),
        Err(_) => {
            // COM sem inicializar não deveria acontecer neste processo, mas um id qualquer é
            // melhor que nenhum.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("monitor-win-{nanos:032x}")
        }
    }
}

/// Os pares já conhecidos, lidos do disco.
///
/// Arquivo ilegível ou corrompido devolve a lista vazia em vez de derrubar o app: o efeito para a
/// pessoa é digitar o PIN de novo, que é exatamente o que aconteceria se o arquivo não existisse.
pub fn pares_conhecidos() -> PairedPeers {
    let caminho = caminho_dos_pares();
    let Ok(texto) = std::fs::read_to_string(&caminho) else {
        return PairedPeers::new();
    };
    match PairedPeers::from_json(&texto) {
        Ok(p) => p,
        Err(e) => {
            registro::linha(format!("aviso: pares.json ilegível (status={}); começando vazio", crate::diagnostico_rede::status(&e)));
            PairedPeers::new()
        }
    }
}

/// Existe **algum** par salvo?
///
/// `docs/ux-m6.md` §1.2: é este estado, e não "reconheço quem está chegando agora", que decide a
/// manchete da tela de espera. A casca não tem como saber a segunda coisa antes de alguém tentar.
pub fn ha_pares_conhecidos() -> bool {
    !pares_conhecidos().is_empty()
}

/// Junta `novos` ao que está no disco e grava.
///
/// Um de cada vez ([`ESCRITA_DOS_PARES`]), e por troca de arquivo: grava ao lado e renomeia por
/// cima. Um `fs::write` direto trunca antes de escrever, e quem lesse naquele instante via JSON
/// inválido e começava vazio (`pares_conhecidos`) — o pareamento de verdade da pessoa sumia.
pub fn guardar_pares(novos: &PairedPeers) {
    let _vez = ESCRITA_DOS_PARES.lock().unwrap_or_else(|e| e.into_inner());
    let mut atual = pares_conhecidos();
    atual.merge(novos);
    match atual.to_json() {
        Ok(texto) => {
            if let Err(e) = gravar_por_troca(&caminho_dos_pares(), &texto) {
                registro::linha(format!("aviso: não consegui gravar pares.json: {e}"));
            } else {
                registro::linha(format!("pares gravados: {} conhecido(s)", atual.len()));
            }
        }
        Err(e) => registro::linha(format!("aviso: não consegui serializar os pares: status={}", crate::diagnostico_rede::status(&e))),
    }
}

/// Grava num arquivo ao lado e renomeia por cima (`MoveFileEx` com substituição, no Windows).
fn gravar_por_troca(destino: &std::path::Path, texto: &str) -> std::io::Result<()> {
    let provisorio = destino.with_extension("json.gravando");
    std::fs::write(&provisorio, texto)?;
    std::fs::rename(&provisorio, destino)
}

// --- os ajustes de câmera do R9 (`docs/controles-de-camera.md` §2) -------------------------------

/// Quem lê-funde-grava `camera-ajustes.json` entra por aqui, um de cada vez (a câmera comum e a da
/// tela R5 podem ser duas câmeras gravando juntas).
static ESCRITA_DOS_AJUSTES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn caminho_dos_ajustes_da_camera() -> PathBuf {
    pasta_de_dados().join("camera-ajustes.json")
}

/// **O registro de uma câmera**, pelo link (§2). Ausente ou ilegível: o padrão.
pub fn ajustes_da_camera(link: &str) -> crate::regras_dos_controles::Registro {
    let texto = std::fs::read_to_string(caminho_dos_ajustes_da_camera()).unwrap_or_default();
    crate::regras_dos_controles::registro_de(&crate::regras_dos_controles::ler_mapa(&texto), link)
}

/// **Guarda o registro de uma câmera** (o padrão tira a entrada): lê o arquivo, troca a entrada
/// daquele link e grava pela troca de arquivo — as outras câmeras ficam como estavam.
pub fn guardar_ajustes_da_camera(link: &str, r: &crate::regras_dos_controles::Registro) {
    let _vez = ESCRITA_DOS_AJUSTES.lock().unwrap_or_else(|e| e.into_inner());
    let caminho = caminho_dos_ajustes_da_camera();
    let texto = std::fs::read_to_string(&caminho).unwrap_or_default();
    let mut mapa = crate::regras_dos_controles::ler_mapa(&texto);
    crate::regras_dos_controles::guardar_em(&mut mapa, link, r);
    if let Err(e) = gravar_por_troca(&caminho, &crate::regras_dos_controles::mapa_em_json(&mapa)) {
        registro::linha(format!("aviso: não consegui gravar camera-ajustes.json: {e}"));
    }
}

fn caminho_dos_indices() -> PathBuf {
    pasta_de_dados().join("indices-de-monitor.json")
}

/// Os índices de monitor por aparelho (`TabelaDeIndices`). Ilegível ou ausente: vazia.
pub fn indices_de_monitor() -> crate::tabela_de_indices::TabelaDeIndices {
    std::fs::read_to_string(caminho_dos_indices())
        .map(|t| crate::tabela_de_indices::TabelaDeIndices::de_json(&t))
        .unwrap_or_default()
}

pub fn gravar_indices_de_monitor(tabela: &crate::tabela_de_indices::TabelaDeIndices) {
    if let Err(e) = gravar_por_troca(&caminho_dos_indices(), &tabela.para_json()) {
        registro::linha(format!("aviso: não consegui gravar indices-de-monitor.json: {e}"));
    }
}

/// O que [`esquecer_par`] fez.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Esquecimento {
    Esquecido,
    /// O par não estava no `pares.json` (ou o arquivo não existe): o resultado que se queria.
    NaoEstava,
    /// O `pares.json` existe e não se leu, ou a gravação falhou: **nada foi esquecido**. Distinto
    /// de "não estava" (crítica 15, N3): o `pares_conhecidos` devolve vazio num arquivo ilegível, e
    /// a primeira versão dizia "não estava" com o par lá dentro.
    Falhou(String),
}

/// **Bancada: esquece um par só** (crítica 13, miúdo 10). A prova da S6 grava a identidade fixa da
/// sonda (`probe-som-s6`) no `pares.json` do usuário, e o mesmo arquivo serve aos dois papéis: sem
/// isto, a sonda do Mac retomaria sem PIN contra o emissor do Dell para sempre. Lê-funde-grava pelo
/// mesmo cadeado e pela mesma troca de arquivo de [`guardar_pares`].
pub fn esquecer_par(device_id: &str) -> Esquecimento {
    let _vez = ESCRITA_DOS_PARES.lock().unwrap_or_else(|e| e.into_inner());
    let caminho = caminho_dos_pares();
    let texto = match std::fs::read_to_string(&caminho) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Esquecimento::NaoEstava,
        Err(e) => return Esquecimento::Falhou(format!("não consegui ler {}: {e}", caminho.display())),
    };
    let mut atual = match PairedPeers::from_json(&texto) {
        Ok(p) => p,
        Err(e) => return Esquecimento::Falhou(format!("{} ilegível: {e}", caminho.display())),
    };
    let id = quall_core::protocol::DeviceId(device_id.to_string());
    let antes = atual.len();
    atual.remove(&id);
    if atual.len() == antes {
        return Esquecimento::NaoEstava;
    }
    match atual.to_json() {
        Ok(texto) => match gravar_por_troca(&caminho, &texto) {
            Ok(()) => {
                registro::linha(format!("par esquecido; {} conhecido(s)", atual.len()));
                Esquecimento::Esquecido
            }
            Err(e) => Esquecimento::Falhou(format!("não consegui gravar {}: {e}", caminho.display())),
        },
        Err(e) => Esquecimento::Falhou(format!("não consegui serializar os pares: {e}")),
    }
}

/// Apaga todos os pareamentos.
///
/// Só oferecido pela interface quando a retomada de fato falhou (`Error::NeedsPin`) — dívida 22.
/// Como botão permanente seria uma armadilha: apagar pareamento é a ação que **causa** o sintoma
/// que ela conserta.
pub fn esquecer_pares() {
    let caminho = caminho_dos_pares();
    match std::fs::remove_file(&caminho) {
        Ok(()) => registro::linha("pares esquecidos"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => registro::linha(format!("aviso: não consegui apagar pares.json: {e}")),
    }
}
