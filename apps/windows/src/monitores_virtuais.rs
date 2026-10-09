//! **`MonitoresVirtuais`**: um monitor virtual do SudoVDA por sessão do emissor com várias sessões,
//! no formato da tela do receptor — a implementação de [`FonteDeMonitor`] que a F2a pediu
//! (`docs/monitor-virtual-windows.md` §13.4 e §14). **Só a bancada**: o driver do produto é pergunta
//! aberta ao usuário (§4.3); chega-se aqui com `--varias-sessoes` e o monitor virtual pedido.
//!
//! # O desenho
//!
//! - **Um por processo** ([`MonitoresVirtuais::do_processo`]), com um **fio dono da topologia**.
//!   Tudo o que muda a arrumação dos monitores passa por ele, em ordem, numa fila: o `ADD` com a
//!   foto e o pedido limpo; o `REMOVE` com a testemunha; o recolher do Parar; e a resolução
//!   *alvo → nome GDI → `HMONITOR` → retângulo*, publicada num [`Mapa`] com uma **época** que sobe a
//!   cada mudança. **As solturas passam na frente das criações.** Os prazos (os 8 s da ativação, o
//!   da soltura) contam de quando o dono começa o pedido, não de quando ele entrou na fila.
//!   Oito sessões são oito threads, e cada pedido leva "todos os nossos ativos": dois pedidos ao
//!   mesmo tempo derrubariam o monitor um do outro.
//! - **A placa, uma vez.** Escolhida quando o processo abre isto, fora os adaptadores indiretos e de
//!   software (o do SudoVDA se diz "Intel" na DXGI): a primeira da ordem Intel → outras → NVIDIA que
//!   **anuncia** um H.264 de hardware próprio (`regras_da_tela_estendida::placa_do_monitor`; no Dell,
//!   a Intel), conferida ativando o H.264 dela antes de qualquer coisa ir ao driver. O
//!   `SET_RENDER_ADAPTER` sai antes do primeiro `ADD` e nunca muda (trocar com o driver vivo trava o
//!   adaptador até reiniciar, §12.3). A sessão pede o LUID antes de o monitor existir (`placa_do_processo`), abre o encoder e
//!   o dispositivo **daquela** placa e transmite uma origem preta até a captura do monitor entrar:
//!   placa → encoder → dispositivo → cadeia na origem preta → monitor → captura.
//! - **O fio de ping é global**, como o vigia do SudoVDA (§5.3): nasce com o primeiro monitor e
//!   morre sem nenhum, pinga a cada 500 ms num handle próprio — e **só** pinga com o dono vivo e o
//!   coordenador batendo (o `alimentar`) há menos de 5 s. Se o coordenador travar, o ping para e o
//!   vigia tira os monitores em 2–3 s: nada fica órfão na tela de ninguém.
//! - **A ativação é o pedido limpo, na hora** (`ativacao.rs` decide; aqui se lê e se aplica). A
//!   `SetDisplayConfig` bloqueia até ~1 s: corre no fio dono, nunca na janela nem no ping. **Nunca**
//!   `SDC_SAVE_TO_DATABASE`. Se a tela do usuário mudar depois de um pedido, o dono a devolve à foto
//!   e desiste do monitor.
//! - **Soltar**: `REMOVE` pelo GUID e a testemunha pelo caminho ativo **e** pelo nó PnP (§12.2 J).
//!   Depois de todo `REMOVE` o dono confere que os outros nossos seguem estendidos e, se não, pede de
//!   novo pela foto (a saída de um pode fazer o Windows clonar ou desligar o outro, §13.1). No Parar,
//!   **uma** `SetDisplayConfig` só com a tela do usuário antes dos `REMOVE` ([`FonteDeMonitor::recolher`]).
//! - **Ninguém fica órfão**: o GUID é lembrado antes do `ADD`; o monitor da sessão ([`Vivo`]) manda
//!   soltar no `Drop` (um pânico no meio também solta); o coordenador solta pela chave quando a
//!   sessão não confirma, some sem batimento ou passa do prazo do Parar; e a saída do processo chama
//!   [`encerrar_tudo`].
//! - **"Nosso" pelo adaptador ou pelo `monitorDevicePath` (`SMKD1CE`)**, nunca pelo nome GDI (E5):
//!   [`gdi_e_nosso`], que o seletor de fontes usa — e só depois de um `ADD` neste processo.
//! - **A cobertura** (`cobertura.rs`) é da bancada: com ela, o dono abre a janela sintética sobre o
//!   monitor antes de devolvê-lo, e a captura só deixa passar quadro coberto (o portão).

#![cfg(windows)]

use std::collections::{BTreeSet, VecDeque};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, RecvTimeoutError, Sender};
use windows::core::{BOOL, GUID, PCWSTR};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_TOPOLOGY_CLONE, DISPLAYCONFIG_TOPOLOGY_EXTEND, DISPLAYCONFIG_TOPOLOGY_EXTERNAL,
    DISPLAYCONFIG_TOPOLOGY_ID, DISPLAYCONFIG_TOPOLOGY_INTERNAL, QDC_ALL_PATHS, QDC_DATABASE_CURRENT,
    QDC_ONLY_ACTIVE_PATHS, QUERY_DISPLAY_CONFIG_FLAGS, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LPARAM, LUID, RECT};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsExW, GetMonitorInfoW, DEVMODEW, DISPLAYCONFIG_PATH_ACTIVE,
    DISPLAYCONFIG_PATH_MODE_IDX_INVALID, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};

use crate::ativacao::{self, Ausencias, Caminho, Decisao, Dono, Limites, Par, Tentativas};
use crate::capture::PortaoDeCobertura;
use crate::idioma;
use crate::cobertura::{self, Cobertura};
use crate::device;
use crate::fontes::Fonte;
use crate::monitor::{AlvoDoWindows, ContextoDoCriar, FonteDeMonitor, MonitorDaSessao, OrigemDoMonitor, PedidoDeMonitor, Soltura, TestemunhaDoMonitor};
use crate::registro;
use crate::sudovda::{self, pnp, Dispositivo};

/// O período do ping. O vigia do SudoVDA conta de segundo em segundo a partir de 3
/// (`Driver.cpp:44-48`): a 500 ms, um ping que atrasa 2 s ainda não derruba ninguém.
const PERIODO_DO_PING: Duration = Duration::from_millis(500);

/// Sem o coordenador bater há mais que isto, o ping para (e o vigia tira os monitores 2–3 s depois).
const SEM_COORDENADOR: Duration = Duration::from_secs(5);

/// O prazo de criar o item de captura (decisão do coordenador, §13.4 item 8), contado **com o dono
/// parado**: com monitores chegando, o `CreateForMonitor` espera as `SetDisplayConfig` deles (§14.5).
pub const PRAZO_DA_CAPTURA: Duration = Duration::from_secs(5);

/// O teto do item de captura com o dono mexendo o tempo todo: oito chegadas de ~2 s cabem.
pub const TETO_DA_CAPTURA_COM_O_DONO_MEXENDO: Duration = Duration::from_secs(30);

/// Sem o alvo aparecer disponível em caminho nenhum por isto, e sem clone nosso à vista, é a
/// assinatura do adaptador travado (§12.3). O normal é 1–7 ms (no primeiro `ADD` do processo, até
/// ~1,2 s, §13.3).
const ASSINATURA_DO_TRAVADO: Duration = Duration::from_secs(3);

pub const CANCELADO: &str = "cancelado: a sessão foi encerrada enquanto o monitor nascia";

/// O motivo que a sessão mostra quando o dono dos monitores caiu (a chave em português; quem o
/// entrega à sessão traduz com `idioma::t`).
pub const SEM_DONO: &str = "Os monitores virtuais pararam (o fio que cuida deles caiu); o SudoVDA os tira em 2–3 s."; // i18n: chave

/// O motivo que as outras sessões mostram quando o dono devolveu a tela da pessoa (item 17).
pub const TELA_DEVOLVIDA: &str = "A tela deste computador mudou enquanto um monitor novo entrava: os monitores virtuais saíram para não mexer nela."; // i18n: chave

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

fn de_utf16(bruto: &[u16]) -> String {
    let fim = bruto.iter().position(|c| *c == 0).unwrap_or(bruto.len());
    String::from_utf16_lossy(&bruto[..fim])
}

pub fn guid_texto(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
    )
}

fn chave(g: &GUID) -> u128 {
    g.to_u128()
}

// ================================================================================================
// O DisplayConfig: ler, fotografar, pedir e conferir
// ================================================================================================

pub mod ccd {
    use super::*;

    /// Os caminhos e modos do `QueryDisplayConfig`. **Erro é erro**, com o código Win32: quem
    /// testemunha a saída de um monitor não pode ler "não consegui perguntar" como "saiu" (com a
    /// tela bloqueada a API devolve `ERROR_ACCESS_DENIED`).
    pub fn ler(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), u32> {
        let mut ultimo = ERROR_INSUFFICIENT_BUFFER.0;
        for _ in 0..5 {
            let (mut np, mut nm) = (0u32, 0u32);
            let e = unsafe { GetDisplayConfigBufferSizes(flags, &mut np, &mut nm) };
            if e != ERROR_SUCCESS {
                return Err(e.0);
            }
            let mut p = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut m = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            let e = unsafe { QueryDisplayConfig(flags, &mut np, p.as_mut_ptr(), &mut nm, m.as_mut_ptr(), None) };
            if e == ERROR_INSUFFICIENT_BUFFER {
                ultimo = e.0;
                continue;
            }
            if e != ERROR_SUCCESS {
                return Err(e.0);
            }
            p.truncate(np as usize);
            m.truncate(nm as usize);
            return Ok((p, m));
        }
        Err(ultimo)
    }

    pub fn luid(l: LUID) -> u64 {
        sudovda::luid_u64(l)
    }

    /// `(nome amigável, monitorDevicePath)` de um alvo, pelo `GET_TARGET_NAME`.
    pub fn nome_do_alvo(adaptador: u64, alvo: u32) -> Result<(String, String), i32> {
        let mut t = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        t.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        t.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        t.header.adapterId = sudovda::luid_de(adaptador);
        t.header.id = alvo;
        let e = unsafe { DisplayConfigGetDeviceInfo(&mut t.header) };
        if e != ERROR_SUCCESS.0 as i32 {
            return Err(e);
        }
        Ok((de_utf16(&t.monitorFriendlyDeviceName), de_utf16(&t.monitorDevicePath)))
    }

    pub fn gdi_da_fonte(adaptador: LUID, id: u32) -> Option<String> {
        let mut s = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        s.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        s.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        s.header.adapterId = adaptador;
        s.header.id = id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut s.header) } != ERROR_SUCCESS.0 as i32 {
            return None;
        }
        let g = de_utf16(&s.viewGdiDeviceName);
        (!g.is_empty()).then_some(g)
    }

    /// O `monitorDevicePath` é de um monitor do SudoVDA?
    pub fn caminho_do_sudovda(caminho_do_monitor: &str) -> bool {
        caminho_do_monitor.to_ascii_uppercase().contains(sudovda::MARCA_DOS_MONITORES)
    }

    fn dono(p: &DISPLAYCONFIG_PATH_INFO) -> Dono {
        match nome_do_alvo(luid(p.targetInfo.adapterId), p.targetInfo.id) {
            Ok((_, cam)) if !cam.is_empty() => {
                if caminho_do_sudovda(&cam) {
                    Dono::Nosso
                } else {
                    Dono::Usuario
                }
            }
            _ => Dono::Desconhecido,
        }
    }

    pub fn vista(p: &DISPLAYCONFIG_PATH_INFO, dono: Dono) -> Caminho {
        Caminho {
            fonte: Par::novo(luid(p.sourceInfo.adapterId), p.sourceInfo.id),
            alvo: Par::novo(luid(p.targetInfo.adapterId), p.targetInfo.id),
            disponivel: p.targetInfo.targetAvailable.as_bool(),
            dono,
        }
    }

    /// Os ativos de agora (`QDC_ONLY_ACTIVE_PATHS`), crus e vistos, com o dono de cada um.
    pub struct Ativos {
        pub caminhos: Vec<DISPLAYCONFIG_PATH_INFO>,
        pub modos: Vec<DISPLAYCONFIG_MODE_INFO>,
        pub vista: Vec<Caminho>,
    }

    pub fn ativos() -> Result<Ativos, u32> {
        let (caminhos, modos) = ler(QDC_ONLY_ACTIVE_PATHS)?;
        let vista = caminhos.iter().map(|p| vista(p, dono(p))).collect();
        Ok(Ativos { caminhos, modos, vista })
    }

    /// Todos os caminhos (`QDC_ALL_PATHS`), crus e vistos — só para achar o caminho livre.
    pub fn todos() -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<Caminho>), u32> {
        let (caminhos, _) = ler(QDC_ALL_PATHS)?;
        let vista = caminhos.iter().map(|p| vista(p, Dono::Desconhecido)).collect();
        Ok((caminhos, vista))
    }

    /// **O nome GDI de um alvo nosso**, só por um caminho ativo com a **fonte no mesmo adaptador**
    /// que o alvo (o virtual). Nunca pelo `monitorDevicePath`: no clone pela Intel o `SMKD1CE`
    /// aparece sobre a fonte da tela integrada (§13.1). `Ok(None)` = sem caminho ativo assim; `Err` =
    /// não deu para perguntar.
    pub fn gdi_do_alvo(adaptador: u64, id: u32) -> Result<Option<String>, String> {
        let (ps, _) = ler(QDC_ONLY_ACTIVE_PATHS).map_err(|e| format!("QueryDisplayConfig={e}"))?;
        let Some(p) = ps.iter().find(|p| luid(p.targetInfo.adapterId) == adaptador && p.targetInfo.id == id) else {
            return Ok(None);
        };
        if luid(p.sourceInfo.adapterId) != adaptador {
            return Ok(None);
        }
        gdi_da_fonte(p.sourceInfo.adapterId, p.sourceInfo.id).map(Some).ok_or_else(|| "GET_SOURCE_NAME falhou".to_string())
    }

    /// Copia um caminho para um pedido, trazendo junto os modos que ele usa (índices remapeados
    /// para `destino`). Sem `QDC_VIRTUAL_MODE_AWARE` o índice é o `modeInfoIdx` puro.
    pub fn copiar_com_modos(
        p: &DISPLAYCONFIG_PATH_INFO,
        origem: &[DISPLAYCONFIG_MODE_INFO],
        destino: &mut Vec<DISPLAYCONFIG_MODE_INFO>,
    ) -> DISPLAYCONFIG_PATH_INFO {
        let mut remapear = |i: u32| -> u32 {
            if i == DISPLAYCONFIG_PATH_MODE_IDX_INVALID || i as usize >= origem.len() {
                return DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            }
            let m = origem[i as usize];
            if let Some(j) = destino.iter().position(|x| x.infoType == m.infoType && x.id == m.id && luid(x.adapterId) == luid(m.adapterId)) {
                return j as u32;
            }
            destino.push(m);
            (destino.len() - 1) as u32
        };
        let mut c = *p;
        unsafe {
            c.sourceInfo.Anonymous.modeInfoIdx = remapear(p.sourceInfo.Anonymous.modeInfoIdx);
            c.targetInfo.Anonymous.modeInfoIdx = remapear(p.targetInfo.Anonymous.modeInfoIdx);
        }
        c
    }

    /// **A tela do usuário**: os caminhos, os modos, e a vista para a regra.
    #[derive(Clone)]
    pub struct TelaDoUsuario {
        pub caminhos: Vec<DISPLAYCONFIG_PATH_INFO>,
        pub modos: Vec<DISPLAYCONFIG_MODE_INFO>,
        pub vista: Vec<Caminho>,
        pub fora: String,
    }

    impl TelaDoUsuario {
        pub fn descrever(&self) -> String {
            format!(
                "[{}] modos={}{}",
                self.vista.iter().map(ativacao::curto).collect::<Vec<_>>().join(" | "),
                self.modos.len(),
                if self.fora.is_empty() { String::new() } else { format!(" fora=[{}]", self.fora) }
            )
        }

        /// Os alvos da tela da pessoa, para comparar duas fotos.
        pub fn alvos(&self) -> BTreeSet<Par> {
            self.vista.iter().map(|c| c.alvo).collect()
        }
    }

    pub fn foto(adaptador_virtual: u64) -> Result<TelaDoUsuario, String> {
        let a = ativos().map_err(|e| format!("QueryDisplayConfig(ativos)={e}"))?;
        let f = ativacao::separar_foto(&a.vista, adaptador_virtual);
        let mut modos = Vec::new();
        let caminhos: Vec<DISPLAYCONFIG_PATH_INFO> = f.usuario.iter().map(|&i| copiar_com_modos(&a.caminhos[i], &a.modos, &mut modos)).collect();
        let vista = f.usuario.iter().map(|&i| Caminho { dono: Dono::Usuario, ..a.vista[i] }).collect();
        let fora = f.fora.iter().map(|(i, m)| format!("{m:?}({})", ativacao::curto(&a.vista[*i]))).collect::<Vec<_>>().join(" ");
        Ok(TelaDoUsuario { caminhos, modos, vista, fora })
    }

    /// O que a conferência da tela do usuário achou.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum TelaConferida {
        Igual,
        Diferente(String),
        /// Não deu para ler (a tela bloqueada devolve `ERROR_ACCESS_DENIED`): **não** é tela
        /// diferente — ninguém desfaz nada por isso (a revisão de 15/09, item 9).
        Ilegivel(String),
    }

    impl TelaConferida {
        pub fn texto(&self) -> String {
            match self {
                TelaConferida::Igual => "igual".into(),
                TelaConferida::Diferente(d) => format!("DIFERENTE: {d}"),
                TelaConferida::Ilegivel(e) => format!("ilegível ({e})"),
            }
        }
    }

    /// A tela do usuário agora é a da foto? Caminho a caminho (pelos pares fonte/alvo): o modo da
    /// fonte (tamanho e posição) e o do alvo (tamanho ativo e frequência).
    pub fn conferir_usuario(foto: &TelaDoUsuario) -> TelaConferida {
        let (ps, ms) = match ler(QDC_ONLY_ACTIVE_PATHS) {
            Ok(x) => x,
            Err(e) => return TelaConferida::Ilegivel(format!("QueryDisplayConfig={e}")),
        };
        let modo = |arr: &[DISPLAYCONFIG_MODE_INFO], i: u32| -> Option<DISPLAYCONFIG_MODE_INFO> {
            (i != DISPLAYCONFIG_PATH_MODE_IDX_INVALID).then(|| arr.get(i as usize).copied()).flatten()
        };
        let fonte = |m: Option<DISPLAYCONFIG_MODE_INFO>| {
            m.map(|m| unsafe {
                let s = m.Anonymous.sourceMode;
                (s.width, s.height, s.position.x, s.position.y)
            })
        };
        let alvo = |m: Option<DISPLAYCONFIG_MODE_INFO>| {
            m.map(|m| unsafe {
                let v = m.Anonymous.targetMode.targetVideoSignalInfo;
                (v.activeSize.cx, v.activeSize.cy, v.vSyncFreq.Numerator, v.vSyncFreq.Denominator)
            })
        };
        let mut diferencas = Vec::new();
        for f in &foto.caminhos {
            let Some(a) = ps.iter().find(|p| {
                luid(p.sourceInfo.adapterId) == luid(f.sourceInfo.adapterId)
                    && p.sourceInfo.id == f.sourceInfo.id
                    && luid(p.targetInfo.adapterId) == luid(f.targetInfo.adapterId)
                    && p.targetInfo.id == f.targetInfo.id
            }) else {
                diferencas.push(format!("sumiu {}", ativacao::curto(&vista(f, Dono::Usuario))));
                continue;
            };
            let (fi, ai) = unsafe { (f.sourceInfo.Anonymous.modeInfoIdx, f.targetInfo.Anonymous.modeInfoIdx) };
            let (fa, aa) = unsafe { (a.sourceInfo.Anonymous.modeInfoIdx, a.targetInfo.Anonymous.modeInfoIdx) };
            let (f0, f1) = (fonte(modo(&foto.modos, fi)), fonte(modo(&ms, fa)));
            if f0 != f1 {
                diferencas.push(format!("fonte {:X}:{} {f0:?}→{f1:?}", f.sourceInfo.adapterId.LowPart, f.sourceInfo.id));
            }
            let (a0, a1) = (alvo(modo(&foto.modos, ai)), alvo(modo(&ms, aa)));
            if a0 != a1 {
                diferencas.push(format!("alvo {:X}:{} {a0:?}→{a1:?}", f.targetInfo.adapterId.LowPart, f.targetInfo.id));
            }
        }
        if diferencas.is_empty() {
            TelaConferida::Igual
        } else {
            TelaConferida::Diferente(diferencas.join("; "))
        }
    }

    /// Monta o pedido: a foto + os nossos já ativos (com os modos de agora) + os caminhos novos
    /// (índices de modo inválidos: o Windows escolhe modo e posição).
    pub fn montar(foto: &TelaDoUsuario, a: &Ativos, virtuais: &[usize], todos: &[DISPLAYCONFIG_PATH_INFO], novos: &[usize]) -> (Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>) {
        let mut modos = foto.modos.clone();
        let mut caminhos = foto.caminhos.clone();
        for &i in virtuais {
            caminhos.push(copiar_com_modos(&a.caminhos[i], &a.modos, &mut modos));
        }
        for &j in novos {
            let mut c = todos[j];
            c.flags = DISPLAYCONFIG_PATH_ACTIVE;
            c.sourceInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            c.targetInfo.Anonymous.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
            caminhos.push(c);
        }
        (caminhos, modos)
    }

    /// `SetDisplayConfig` com `SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES` e
    /// **sem** `SDC_SAVE_TO_DATABASE`: o Win+P da pessoa e a lembrança por combinação não mudam, e o
    /// que o banco guarda volta quando o monitor sai. Bloqueia até ~1 s.
    pub fn aplicar(caminhos: &[DISPLAYCONFIG_PATH_INFO], modos: &[DISPLAYCONFIG_MODE_INFO]) -> i32 {
        unsafe { SetDisplayConfig(Some(caminhos), Some(modos), SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES) }
    }

    /// A topologia do banco (o Win+P da pessoa): `QDC_DATABASE_CURRENT`. Só lê.
    pub fn topologia() -> String {
        let (mut np, mut nm) = (0u32, 0u32);
        let e = unsafe { GetDisplayConfigBufferSizes(QDC_DATABASE_CURRENT, &mut np, &mut nm) };
        if e != ERROR_SUCCESS {
            return format!("? (GetDisplayConfigBufferSizes={})", e.0);
        }
        let mut p = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut m = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        let mut t = DISPLAYCONFIG_TOPOLOGY_ID(0);
        let e = unsafe {
            QueryDisplayConfig(QDC_DATABASE_CURRENT, &mut np, p.as_mut_ptr(), &mut nm, m.as_mut_ptr(), Some(&mut t as *mut DISPLAYCONFIG_TOPOLOGY_ID))
        };
        if e != ERROR_SUCCESS {
            return format!("? (QueryDisplayConfig={})", e.0);
        }
        let nome = match t {
            DISPLAYCONFIG_TOPOLOGY_INTERNAL => "INTERNAL",
            DISPLAYCONFIG_TOPOLOGY_CLONE => "CLONE",
            DISPLAYCONFIG_TOPOLOGY_EXTEND => "EXTEND",
            DISPLAYCONFIG_TOPOLOGY_EXTERNAL => "EXTERNAL",
            _ => "?",
        };
        format!("{nome} (caminhos no banco={np})")
    }

    /// Os caminhos ativos de agora, como `\\.\DISPLAYn "nome" fonte>alvo` — a linha de base da tela.
    pub fn resumo_dos_ativos() -> String {
        let a = match ativos() {
            Ok(a) => a,
            Err(e) => return format!("(QueryDisplayConfig falhou: {e})"),
        };
        let v: Vec<String> = a
            .caminhos
            .iter()
            .zip(a.vista.iter())
            .map(|(p, c)| {
                let g = gdi_da_fonte(p.sourceInfo.adapterId, p.sourceInfo.id).unwrap_or_else(|| "?".into());
                let n = nome_do_alvo(c.alvo.adaptador, c.alvo.id).map(|x| x.0).unwrap_or_default();
                format!("{g} \"{n}\" {}", ativacao::curto(c))
            })
            .collect();
        format!("{} [{}]", v.len(), v.join(" | "))
    }
}

// --- o GDI ---------------------------------------------------------------------------------------

unsafe extern "system" fn junta(h: HMONITOR, _hdc: HDC, _r: *mut RECT, dados: LPARAM) -> BOOL {
    let lista = unsafe { &mut *(dados.0 as *mut Vec<(String, isize, RECT)>) };
    let mut i = MONITORINFOEXW::default();
    i.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(h, &mut i as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
        lista.push((de_utf16(&i.szDevice), h.0 as isize, i.monitorInfo.rcMonitor));
    }
    BOOL(1)
}

/// Os monitores do GDI: `(nome, HMONITOR como número, retângulo)`.
fn monitores_do_gdi() -> Vec<(String, isize, RECT)> {
    let mut v: Vec<(String, isize, RECT)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(junta), LPARAM(&mut v as *mut Vec<(String, isize, RECT)> as isize));
    }
    v
}

fn cruza(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// Algum monitor do SudoVDA está ativo por um adaptador que não é o virtual (o clone do Windows)?
fn clone_nosso_agora(virtual_: u64) -> bool {
    ccd::ler(QDC_ONLY_ACTIVE_PATHS).is_ok_and(|(ps, _)| {
        ps.iter().any(|p| {
            ccd::luid(p.targetInfo.adapterId) != virtual_
                && ccd::nome_do_alvo(ccd::luid(p.targetInfo.adapterId), p.targetInfo.id).is_ok_and(|(_, c)| ccd::caminho_do_sudovda(&c))
        })
    })
}

/// Onde um monitor nosso está na área de trabalho.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Onde {
    pub gdi: String,
    pub hmonitor: isize,
    /// `(esquerda, topo, direita, baixo)` em pixels físicos.
    pub rect: (i32, i32, i32, i32),
}

impl Onde {
    pub fn largura(&self) -> u32 {
        (self.rect.2 - self.rect.0).max(0) as u32
    }
    pub fn altura(&self) -> u32 {
        (self.rect.3 - self.rect.1).max(0) as u32
    }
}

/// O que o dono sabe de um alvo nosso.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Leitura {
    Ativo(Onde),
    /// Sem caminho ativo com a fonte no adaptador virtual, fora da enumeração do GDI, ou cruzando
    /// outro monitor: não é monitor que se capture agora.
    Inativo(String),
    /// Não deu para perguntar (a tela bloqueada, por exemplo): **não** é saída.
    Erro(String),
    /// A janela sintética da bancada acabou (o monitor dela sumiu por mais que a graça, mudou de
    /// tamanho ou cruzou outro): sem ela, nada deste monitor se captura.
    Descoberto(String),
    /// O fim, dito pelo dono, com o motivo que a sessão mostra: o dono caiu, ou devolveu a tela da
    /// pessoa (a revisão de 15/09, itens 5 e 17).
    Terminal(String),
}

/// **Alvo → nome GDI → `HMONITOR` → retângulo**, com as conferências antes de capturar: a fonte do
/// caminho no adaptador virtual, e o retângulo sem cruzar o de outro monitor.
pub fn ler_alvo(alvo: Par) -> Leitura {
    match ccd::gdi_do_alvo(alvo.adaptador, alvo.id) {
        Err(e) => Leitura::Erro(e),
        Ok(None) => Leitura::Inativo("sem caminho ativo com a fonte no adaptador virtual".into()),
        Ok(Some(gdi)) => {
            let todos = monitores_do_gdi();
            let Some((_, h, r)) = todos.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(&gdi)).cloned() else {
                return Leitura::Inativo(format!("{gdi} fora da enumeração do GDI"));
            };
            if let Some((outro, _, _)) = todos.iter().find(|(n, _, o)| !n.eq_ignore_ascii_case(&gdi) && cruza(o, &r)) {
                return Leitura::Inativo(format!("o retângulo de {gdi} cruza o de {outro}"));
            }
            Leitura::Ativo(Onde { gdi, hmonitor: h, rect: (r.left, r.top, r.right, r.bottom) })
        }
    }
}

/// O modo atual de um monitor pelo nome GDI: `(largura, altura, Hz)`.
fn modo_atual(gdi: &str) -> Option<(u32, u32, u32)> {
    let l: Vec<u16> = gdi.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    let ok = unsafe { EnumDisplaySettingsExW(PCWSTR(l.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0)) };
    ok.as_bool().then_some((dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency))
}

/// As placas DXGI **de hardware**: `(descrição, fornecedor, LUID)`, sem os adaptadores indiretos
/// (o do SudoVDA aparece com o nome e o fornecedor da placa que o desenha) nem os de software. A que
/// não responde se é indireta fica de fora (o `device::placas_de_hardware` a deixa concorrer: aqui
/// ela poderia ser o próprio SudoVDA, que se diz Intel — a revisão de 02/10, item 8).
fn placas() -> Vec<(String, u32, u64)> {
    use windows::Win32::Graphics::Dxgi::DXGI_ADAPTER_FLAG_SOFTWARE;
    let Ok(f) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return Vec::new() };
    let mut v = Vec::new();
    let mut i = 0u32;
    while let Ok(a) = unsafe { f.EnumAdapters1(i) } {
        if let Some(d) = unsafe { a.GetDesc1() }.ok().filter(|d| d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0) {
            let luid = sudovda::luid_u64(d.AdapterLuid);
            // O mesmo `device::adaptador_indireto` que o `create_device` do app de uma sessão usa.
            match device::adaptador_indireto(luid) {
                Some(false) => v.push((de_utf16(&d.Description), d.VendorId, luid)),
                Some(true) => {}
                None => registro::linha(format!(
                    "monitor virtual: não consegui perguntar se o adaptador {luid:016X} ({}) é indireto — fica de fora",
                    de_utf16(&d.Description)
                )),
            }
        }
        i += 1;
    }
    v
}

/// **A testemunha da placa que desenha o monitor**: o adaptador DXGI sob o qual a saída `gdi` é
/// enumerada — descrição e LUID. No Dell, em 14/09, ela deu a placa pedida no `SET_RENDER_ADAPTER`
/// (§12.2 I).
fn placa_que_desenha(gdi: &str) -> Option<(String, u64)> {
    let f: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let mut i = 0u32;
    while let Ok(a) = unsafe { f.EnumAdapters1(i) } {
        let a: IDXGIAdapter1 = a;
        let mut j = 0u32;
        while let Ok(o) = unsafe { a.EnumOutputs(j) } {
            if let Ok(d) = unsafe { o.GetDesc() } {
                if de_utf16(&d.DeviceName).eq_ignore_ascii_case(gdi) {
                    let desc = unsafe { a.GetDesc1() }.ok()?;
                    return Some((de_utf16(&desc.Description), sudovda::luid_u64(desc.AdapterLuid)));
                }
            }
            j += 1;
        }
        i += 1;
    }
    None
}

/// **A placa do monitor virtual** (02/10: com ou sem Intel): a primeira da ordem que **anuncia** um
/// H.264 de hardware (`regras_da_tela_estendida::placa_do_monitor`), e a prova — o H.264 dela ativa
/// agora, e é desligado em seguida — antes de qualquer coisa ir ao driver. Se a escolhida não ativa,
/// recusa: cair para a próxima faria outro processo escolher outra placa e travar o SudoVDA (§12.3).
fn escolher_a_placa(sem_intel: bool) -> Result<(String, u64), String> {
    use crate::regras_da_tela_estendida::{placa_do_monitor, sem_placa, PlacaCandidata};
    let ps = placas();
    let candidatas: Vec<PlacaCandidata> =
        ps.iter().map(|(_, f, l)| PlacaCandidata { luid: *l, fornecedor: *f, tem_h264: crate::encoder::anuncia_h264_na_placa(*l) }).collect();
    registro::linha(format!(
        "monitor virtual: placas de hardware{}: {}",
        if sem_intel { " (--monitor-sem-intel: sem a Intel)" } else { "" },
        ps.iter()
            .zip(&candidatas)
            .map(|((d, f, l), c)| format!("{d} fornecedor={f:04X} luid={l:016X} h264={}", if c.tem_h264 { "sim" } else { "não" }))
            .collect::<Vec<_>>()
            .join("; ")
    ));
    let luid = placa_do_monitor(&candidatas, sem_intel).ok_or_else(|| sem_placa(ps.len(), sem_intel))?;
    let desc = ps.iter().find(|p| p.2 == luid).map(|p| p.0.clone()).unwrap_or_default();
    let enc = crate::encoder::ativar_h264_so_na_placa(luid).map_err(|e| format!("o codificador H.264 da placa {desc} não ativou ({e})"))?;
    let nome = enc.friendly_name.clone();
    crate::encoder::desligar(&enc);
    drop(enc);
    registro::linha(format!("monitor virtual: a placa é {desc} ({luid:016X}); o H.264 dela ativou (\"{nome}\") e foi desligado"));
    Ok((desc, luid))
}

// ================================================================================================
// O relato da bancada
// ================================================================================================

/// Um nascimento: do `ADD` ao caminho ativo.
#[derive(Clone, Debug, Default)]
pub struct Nascimento {
    pub indice: usize,
    pub alvo: u32,
    /// Quanto o pedido esperou na fila do dono (outra ativação ou soltura em curso).
    pub fila_ms: u64,
    /// Do `ADD` ao caminho ativo com todos os nossos na área de trabalho.
    pub ativo_ms: Option<u64>,
    pub pedidos: u32,
    pub clones_desfeitos: u32,
    pub esperas: u32,
    /// A linha de [`Ausencias`]: se algum dos outros saiu da área de trabalho, e por quanto.
    pub outros: String,
    pub algum_outro_saiu: bool,
    /// O nome GDI dos outros antes → depois (E5: o nome do 1º mudou quando o 2º chegou).
    pub nomes_dos_outros: String,
    pub modo: String,
    pub desenha: String,
    pub adotado: bool,
    pub falha: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Relato {
    pub nascimentos: Vec<Nascimento>,
    /// `(alvo, Some(ms) testemunhada | None não confirmada, fila em ms)`.
    pub solturas: Vec<(u32, Option<u64>, u64)>,
    /// `(alvo, buraco na captura em ms, motivo)` — a troca de `HMONITOR` seguida pela sessão.
    pub reaberturas: Vec<(u32, u64, String)>,
    /// Depois de um `REMOVE`: `(quantos dos outros estavam fora, pedidos para devolver)`.
    pub reparos: Vec<(usize, u32)>,
    /// Criações de item de captura que venceram o prazo.
    pub capturas_presas: u32,
    /// O ping: pings, falhas, maior intervalo entre dois pings bons (ms), pings pulados sem coordenador.
    pub ping: (u64, u64, u64, u64),
    /// Recolher (Parar): `SetDisplayConfig` só com a tela do usuário.
    pub recolhimentos: Vec<String>,
}

static RELATO: Mutex<Option<Relato>> = Mutex::new(None);

fn anotar(f: impl FnOnce(&mut Relato)) {
    if let Ok(mut r) = RELATO.lock() {
        f(r.get_or_insert_with(Relato::default));
    }
}

/// O que aconteceu com os monitores virtuais neste processo (a bancada imprime no fim).
pub fn relato() -> Relato {
    let mut r = RELATO.lock().ok().and_then(|r| r.clone()).unwrap_or_default();
    if let Some(m) = INSTANCIA.lock().ok().and_then(|i| i.clone()) {
        r.ping = m.c.ping_numeros();
    }
    r
}

/// A sessão seguiu o alvo e reabriu a captura: o buraco que isso custou.
pub fn anotar_reabertura(alvo: u32, buraco_ms: u64, motivo: &str) {
    anotar(|r| r.reaberturas.push((alvo, buraco_ms, motivo.to_string())));
}

// ================================================================================================
// O que o dono, as sessões e o ping dividem
// ================================================================================================

/// O que o dono publica depois de cada mudança de topologia (e a cada ~250 ms parado).
#[derive(Clone, Debug, Default)]
struct Mapa {
    epoca: u64,
    /// O dono está mudando a topologia agora: quem olha tolera ausência.
    mexendo: bool,
    alvos: Vec<(Par, Leitura)>,
}

enum Trabalho {
    Criar { pedido: PedidoDeMonitor, ficha: u64, cancelar: Arc<AtomicBool>, entrou: Instant, resposta: Sender<Result<Criado, String>> },
    Soltar { guid: GUID, prazo: Duration, entrou: Instant, resposta: Option<Sender<Soltura>> },
    Recolher,
    EncerrarTudo { resposta: Sender<usize> },
}

#[derive(Default)]
struct Fila {
    /// Solturas, recolher e o fim: passam na frente das criações.
    urgentes: VecDeque<Trabalho>,
    criacoes: VecDeque<Trabalho>,
}

struct Compartilhado {
    fila: Mutex<Fila>,
    sinal: Condvar,
    mapa: Mutex<Mapa>,
    base: Instant,
    /// ms desde `base` do último `alimentar` do coordenador.
    batimento_do_coordenador: AtomicU64,
    dono_vivo: AtomicBool,
    captura_travada: AtomicBool,
    /// O Parar pediu o recolher: as solturas que ainda estão na fila na frente dele não reparam os
    /// outros (cada reparo seria uma `SetDisplayConfig` a mais no meio do Parar — N = 8, 15/09).
    recolher_pedido: AtomicBool,
    adaptador_travado: Mutex<Option<String>>,
    /// Monitores vivos + GUIDs pendentes: o ping só pinga com algum.
    de_pe: AtomicUsize,
    proxima_ficha: AtomicU64,
    ping_pings: AtomicU64,
    ping_falhas: AtomicU64,
    ping_maior: AtomicU64,
    ping_pulados: AtomicU64,
    ping_ultimo_bom: AtomicU64,
    /// ms desde `base` (mais 1) de quando o dono caiu; 0 com ele de pé. É o relógio da espera antes
    /// de abrir outro ([`regras_da_tela_estendida::ESPERA_DEPOIS_DO_DONO_MS`]).
    morte_ms: AtomicU64,
}

impl Compartilhado {
    fn enfileirar(&self, t: Trabalho) {
        if let Ok(mut f) = self.fila.lock() {
            match t {
                Trabalho::Criar { .. } => f.criacoes.push_back(t),
                _ => f.urgentes.push_back(t),
            }
        }
        self.sinal.notify_all();
    }

    fn ler_mapa(&self, alvo: Par) -> (u64, bool, Leitura) {
        let m = self.mapa.lock().map(|m| m.clone()).unwrap_or_default();
        // Sem dono, o mapa congelou (talvez com `mexendo` ligado): a leitura é terminal, e a sessão
        // sai com o motivo em vez de repetir a imagem para sempre (a revisão de 15/09, item 5).
        if !self.dono_vivo.load(Ordering::SeqCst) {
            return (m.epoca, false, Leitura::Terminal(idioma::t(SEM_DONO).into()));
        }
        let l = m.alvos.iter().find(|(a, _)| *a == alvo).map(|(_, l)| l.clone()).unwrap_or_else(|| Leitura::Inativo("o dono não conhece o alvo".into()));
        (m.epoca, m.mexendo, l)
    }

    fn mexendo(&self, sim: bool) {
        if let Ok(mut m) = self.mapa.lock() {
            m.mexendo = sim;
        }
    }

    fn coordenador_batendo(&self) -> bool {
        let b = self.batimento_do_coordenador.load(Ordering::Relaxed);
        self.base.elapsed().saturating_sub(Duration::from_millis(b)) < SEM_COORDENADOR
    }

    fn ping_numeros(&self) -> (u64, u64, u64, u64) {
        (
            self.ping_pings.load(Ordering::Relaxed),
            self.ping_falhas.load(Ordering::Relaxed),
            self.ping_maior.load(Ordering::Relaxed),
            self.ping_pulados.load(Ordering::Relaxed),
        )
    }
}

// ================================================================================================
// O fio de ping
// ================================================================================================

struct Ping {
    parar: Arc<AtomicBool>,
    fio: Option<JoinHandle<()>>,
}

impl Ping {
    /// O fio abre o **seu** handle do dispositivo (um `ADD` demorado no mesmo handle o seguraria).
    fn iniciar(interface: &str, reserva: Arc<Dispositivo>, c: Arc<Compartilhado>) -> Ping {
        let disp = match Dispositivo::abrir(interface) {
            Ok(d) => Arc::new(d),
            Err(e) => {
                registro::linha(format!("monitor virtual: !! o fio de ping não abriu handle próprio ({e}); divide o do dono"));
                reserva
            }
        };
        let parar = Arc::new(AtomicBool::new(false));
        let p2 = parar.clone();
        let fio = std::thread::Builder::new()
            .name("quall.ping-sudovda".into())
            .spawn(move || {
                let mut anterior: Option<Instant> = None;
                let mut avisou = false;
                while !p2.load(Ordering::SeqCst) {
                    // **Só com o dono vivo e o coordenador batendo**: um processo travado não pode
                    // manter monitores de pé na tela de ninguém.
                    if c.dono_vivo.load(Ordering::SeqCst) && c.coordenador_batendo() && c.de_pe.load(Ordering::SeqCst) > 0 {
                        avisou = false;
                        match disp.ping() {
                            Ok(()) => {
                                let agora = Instant::now();
                                if let Some(a) = anterior {
                                    c.ping_maior.fetch_max(ms(agora - a), Ordering::Relaxed);
                                }
                                anterior = Some(agora);
                                c.ping_pings.fetch_add(1, Ordering::Relaxed);
                                c.ping_ultimo_bom.store(ms(c.base.elapsed()), Ordering::Relaxed);
                            }
                            Err(e) => {
                                if c.ping_falhas.fetch_add(1, Ordering::Relaxed) < 3 {
                                    registro::linha(format!("monitor virtual: !! ping falhou: {e}"));
                                }
                            }
                        }
                    } else {
                        c.ping_pulados.fetch_add(1, Ordering::Relaxed);
                        anterior = None;
                        if !avisou {
                            avisou = true;
                            registro::linha(format!(
                                "monitor virtual: ping suspenso (dono_vivo={} coordenador_batendo={} de_pe={}) — sem ping, o vigia tira os monitores em 2–3 s",
                                c.dono_vivo.load(Ordering::SeqCst),
                                c.coordenador_batendo(),
                                c.de_pe.load(Ordering::SeqCst)
                            ));
                        }
                    }
                    let t = Instant::now();
                    while t.elapsed() < PERIODO_DO_PING && !p2.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            })
            .ok();
        Ping { parar, fio }
    }

    fn parar(mut self) {
        self.parar.store(true, Ordering::SeqCst);
        if let Some(f) = self.fio.take() {
            let _ = f.join();
        }
    }
}

/// **O ping largado sem `parar`** (o dono caiu em pânico com ele de pé): o fio sai sozinho em até
/// 20 ms e fecha o handle próprio do dispositivo. Antes ele ficava vivo, ocioso, até o processo
/// sair — e a nova tentativa abriria um segundo fio de ping (a revisão de 02/10, item 7).
impl Drop for Ping {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::SeqCst);
    }
}

// ================================================================================================
// O dono da topologia
// ================================================================================================

/// Um monitor do processo de pé, do ponto de vista do dono.
struct VivoNoDono {
    guid: GUID,
    alvo: Par,
    instancia: Option<String>,
    /// Quem o segura agora; `None` = órfão (a soltura não confirmou), adotável pelo mesmo aparelho.
    ficha: Option<u64>,
    cobertura: Option<Cobertura>,
    /// O dono decidiu o fim deste monitor (a tela da pessoa devolvida): o motivo vai no mapa.
    terminal: Option<String>,
}

/// O que o dono devolve a quem pediu um monitor.
pub struct Criado {
    guid: GUID,
    alvo: Par,
    placa: u64,
    onde: Onde,
    modo: Option<(u32, u32, u32)>,
    portao: Option<Arc<PortaoDeCobertura>>,
    descricao: String,
}

/// A referência da tela da pessoa na primeira criação da rodada: o dono só repara os nossos sozinho
/// (sem ter mandado um `REMOVE`) se a topologia do banco e os alvos da pessoa continuam os mesmos —
/// nunca contra o Win+P dela.
struct Referencia {
    topologia: String,
    alvos: BTreeSet<Par>,
    /// A foto da tela da pessoa tirada antes do primeiro `ADD` (sem monitor nosso): os caminhos **e
    /// os modos** que o reparo e o recolher pedem, e contra os quais conferem. Fotografar "agora", no
    /// meio de um clone nosso, traria o modo do clone (a revisão de 15/09, item 8; §13.6).
    foto: ccd::TelaDoUsuario,
}

struct DonoDaTopologia {
    c: Arc<Compartilhado>,
    interface: String,
    disp: Arc<Dispositivo>,
    /// A placa do processo (descrição e LUID), escolhida na abertura.
    placa_escolhida: (String, u64),
    /// A placa já mandada ao driver (`SET_RENDER_ADAPTER`) por este dono.
    placa: Option<u64>,
    adaptador_virtual: Option<u64>,
    ping: Option<Ping>,
    vivos: Vec<VivoNoDono>,
    pendentes: Vec<GUID>,
    cobrir: Option<cobertura::Modo>,
    referencia: Option<Referencia>,
    /// Depois do Parar: não reparar sozinho até a próxima criação.
    recolhido: bool,
    ultimo_reparo: Option<Instant>,
}

impl DonoDaTopologia {
    fn correr(mut self) {
        loop {
            let t = {
                let Ok(mut f) = self.c.fila.lock() else { break };
                if f.urgentes.is_empty() && f.criacoes.is_empty() {
                    f = match self.c.sinal.wait_timeout(f, Duration::from_millis(250)) {
                        Ok((g, _)) => g,
                        Err(_) => break,
                    };
                }
                f.urgentes.pop_front().or_else(|| f.criacoes.pop_front())
            };
            match t {
                Some(t) => {
                    self.c.mexendo(true);
                    self.fazer(t);
                    self.publicar_mapa();
                    self.c.mexendo(false);
                }
                None => self.parado(),
            }
            self.cuidar_do_ping();
        }
    }

    fn fazer(&mut self, t: Trabalho) {
        match t {
            Trabalho::Criar { pedido, ficha, cancelar, entrou, resposta } => {
                let fila = ms(entrou.elapsed());
                let r = self.criar(&pedido, ficha, &cancelar, fila);
                if let Err(Ok(criado)) = resposta.send(r).map_err(|e| e.into_inner()) {
                    // Quem pediu já foi embora: o monitor não pode ficar de pé sem dono.
                    registro::linha(format!("monitor virtual: a sessão que pediu o alvo {} não esperou a resposta — solto", criado.alvo.id));
                    let _ = self.soltar(criado.guid, Duration::from_secs(5), 0);
                }
            }
            Trabalho::Soltar { guid, prazo, entrou, resposta } => {
                let s = self.soltar(guid, prazo, ms(entrou.elapsed()));
                if let Some(r) = resposta {
                    let _ = r.send(s);
                }
            }
            Trabalho::Recolher => self.recolher(),
            Trabalho::EncerrarTudo { resposta } => {
                let n = self.encerrar_tudo();
                let _ = resposta.send(n);
            }
        }
    }

    /// Os monitores com dono (os órfãos não contam: sem sessão, o vigia do SudoVDA os tira quando o
    /// ping para — a revisão de 15/09, item 6).
    fn com_dono(&self) -> usize {
        self.vivos.iter().filter(|v| v.ficha.is_some()).count()
    }

    fn atualizar_de_pe(&self) {
        self.c.de_pe.store(self.com_dono() + self.pendentes.len(), Ordering::SeqCst);
    }

    fn cuidar_do_ping(&mut self) {
        self.atualizar_de_pe();
        let alguem = self.com_dono() > 0 || !self.pendentes.is_empty();
        if alguem && self.ping.is_none() {
            self.ping = Some(Ping::iniciar(&self.interface, self.disp.clone(), self.c.clone()));
            registro::linha(format!("monitor virtual: fio de ping de pé, a cada {} ms", ms(PERIODO_DO_PING)));
        } else if !alguem {
            if let Some(p) = self.ping.take() {
                p.parar();
                let n = self.c.ping_numeros();
                registro::linha(format!(
                    "monitor virtual: fio de ping parado (nenhum monitor de pé): {} pings, {} falhas, maior intervalo {} ms, {} pulados",
                    n.0, n.1, n.2, n.3
                ));
            }
        }
    }

    /// O mapa: onde cada alvo nosso está agora. A época sobe quando algo muda.
    fn publicar_mapa(&mut self) {
        let alvos: Vec<(Par, Leitura)> = self
            .vivos
            .iter()
            .map(|v| match (v.terminal.as_ref(), v.cobertura.as_ref().and_then(|c| c.acabou())) {
                (Some(t), _) => (v.alvo, Leitura::Terminal(t.clone())),
                (None, Some(m)) => (v.alvo, Leitura::Descoberto(m)),
                (None, None) => (v.alvo, ler_alvo(v.alvo)),
            })
            .collect();
        if let Ok(mut m) = self.c.mapa.lock() {
            if m.alvos != alvos {
                m.epoca += 1;
                m.alvos = alvos;
            }
        }
    }

    /// Parado: o mapa, as solturas que não confirmaram, e o reparo guardado.
    fn parado(&mut self) {
        self.publicar_mapa();
        // Os órfãos (soltura sem confirmação): um REMOVE de novo a cada volta parada (a cada ~250 ms),
        // até o PnP confirmar — ou até o mesmo aparelho voltar e adotá-lo.
        let orfaos: Vec<GUID> = self.vivos.iter().filter(|v| v.ficha.is_none()).map(|v| v.guid).collect();
        for g in orfaos {
            let _ = self.soltar(g, Duration::from_millis(500), 0);
        }
        // Reparar sozinho só se nada da pessoa mudou (topologia do banco e alvos dela iguais aos da
        // primeira criação): nunca contra o Win+P dela. E no máximo uma vez a cada 5 s.
        if self.recolhido || self.c.recolher_pedido.load(Ordering::SeqCst) || self.vivos.is_empty() || self.ultimo_reparo.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        let fora: Vec<Par> = self
            .vivos
            .iter()
            .filter(|v| v.ficha.is_some())
            .filter(|v| matches!(ler_alvo(v.alvo), Leitura::Inativo(_)))
            .map(|v| v.alvo)
            .collect();
        if fora.is_empty() {
            return;
        }
        let Some(r) = self.referencia.as_ref() else { return };
        let (Some(virtual_), Ok(foto)) = (self.adaptador_virtual, ccd::foto(self.adaptador_virtual.unwrap_or(0))) else { return };
        if ccd::topologia() != r.topologia || foto.alvos() != r.alvos {
            return;
        }
        self.ultimo_reparo = Some(Instant::now());
        self.c.mexendo(true);
        let n = self.reparar(&fora, virtual_, "parado");
        self.publicar_mapa();
        self.c.mexendo(false);
        anotar(|rel| rel.reparos.push((fora.len(), n)));
    }

    /// **A foto para pedir e conferir**: a da referência, quando os alvos da pessoa agora são os
    /// dela (os modos da referência, não os de um clone nosso de agora — item 8); senão a de agora,
    /// porque a pessoa mudou a tela e a referência envelheceu.
    fn foto_para_pedir(&self, virtual_: u64) -> Result<(ccd::TelaDoUsuario, &'static str), String> {
        let agora = ccd::foto(virtual_)?;
        match self.referencia.as_ref() {
            Some(r) if r.foto.alvos() == agora.alvos() && !r.foto.caminhos.is_empty() => Ok((r.foto.clone(), "a foto de referência")),
            _ => Ok((agora, "a foto de agora (os alvos da pessoa não são os da referência)")),
        }
    }

    /// Devolve à área de trabalho os nossos em `fora` (e mantém os outros com dono), pela foto de
    /// referência (item 8).
    fn reparar(&mut self, fora: &[Par], virtual_: u64, porque: &str) -> u32 {
        let mut alvos: Vec<Par> = fora.to_vec();
        // Só os com dono: o órfão não tem janela nem sessão, e pedi-lo o poria na área de trabalho da
        // pessoa sem ninguém vendo (a revisão de 15/09, item 6).
        alvos.extend(self.vivos.iter().filter(|v| v.ficha.is_some()).map(|v| v.alvo).filter(|a| !fora.contains(a)));
        let mut n = Nascimento::default();
        let t = Instant::now();
        let nunca = AtomicBool::new(false);
        let r = match self.foto_para_pedir(virtual_) {
            Ok((f, qual)) => {
                registro::linha(format!("monitor virtual: reparo ({porque}) pede com {qual}"));
                self.ativar(&alvos, virtual_, t, &mut n, &nunca, false, f)
            }
            Err(e) => Err(format!("a foto da tela do usuário falhou ({e})")),
        };
        registro::linha(format!(
            "monitor virtual: reparo ({porque}) de {} alvo(s) fora da área de trabalho: {} em {} ms, {} pedidos",
            fora.len(),
            match &r {
                Ok(()) => "de volta".to_string(),
                Err(e) => format!("NÃO voltou ({e})"),
            },
            ms(t.elapsed()),
            n.pedidos
        ));
        n.pedidos
    }

    /// **A placa, uma vez.** A primeira criação manda o `SET_RENDER_ADAPTER` com a placa do processo;
    /// as seguintes não mandam nada. Nunca troca.
    fn fixar_placa(&mut self) -> Result<u64, String> {
        let (desc, luid) = self.placa_escolhida.clone();
        if self.placa.is_none() {
            self.disp.fixar_placa(sudovda::luid_de(luid)).map_err(|e| format!("SET_RENDER_ADAPTER falhou: {e}"))?;
            self.placa = Some(luid);
            if let Ok(mut f) = PLACA_FIXADA.lock() {
                *f = Some((desc.clone(), luid));
            }
            registro::linha(format!(
                "monitor virtual: SET_RENDER_ADAPTER = {desc} ({luid:016X}), uma vez neste processo (o retorno não prova nada; a testemunha é o desenha= de cada monitor)"
            ));
        }
        Ok(luid)
    }

    /// A ativação: o pedido limpo, na hora, até todos os `alvos` (o novo primeiro) estarem na área de
    /// trabalho, com prazo pelo relógio a partir de `t0`.
    fn ativar(
        &mut self,
        alvos: &[Par],
        virtual_: u64,
        t0: Instant,
        n: &mut Nascimento,
        cancelar: &AtomicBool,
        novo_monitor: bool,
        foto: ccd::TelaDoUsuario,
    ) -> Result<(), String> {
        let outros: Vec<Par> = alvos[1..].to_vec();
        // **Os outros, amostrados num fio à parte** a cada 10 ms: o fio dono fica bloqueado dentro
        // da `SetDisplayConfig` (até ~1 s), e é justamente aí que a chegada de um tira o outro da
        // área de trabalho (E5; na N = 2 de 15/09 a janela do 1º se escondeu 480 ms). Amostrar só no
        // laço do dono seria cego nesse trecho.
        let aus = Mutex::new(Ausencias::para(&outros));
        let parar_amostra = AtomicBool::new(false);
        let r = std::thread::scope(|escopo| {
            if !outros.is_empty() {
                escopo.spawn(|| {
                    while !parar_amostra.load(Ordering::SeqCst) {
                        let agora = ms(t0.elapsed());
                        let lidos: Vec<(Par, bool)> = outros.iter().map(|&o| (o, matches!(ler_alvo(o), Leitura::Ativo(_)))).collect();
                        if let Ok(mut a) = aus.lock() {
                            a.observar(agora, |p| lidos.iter().any(|(o, at)| *o == p && *at));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                });
            }
            let r = self.ativar_por_dentro(alvos, virtual_, t0, n, cancelar, novo_monitor, &foto);
            parar_amostra.store(true, Ordering::SeqCst);
            r
        });
        let mut a = aus.into_inner().unwrap_or_default();
        a.fechar(ms(t0.elapsed()));
        n.outros = a.linha();
        n.algum_outro_saiu = a.algum_saiu();
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn ativar_por_dentro(
        &mut self,
        alvos: &[Par],
        virtual_: u64,
        t0: Instant,
        n: &mut Nascimento,
        cancelar: &AtomicBool,
        novo_monitor: bool,
        foto: &ccd::TelaDoUsuario,
    ) -> Result<(), String> {
        let l = Limites::DA_RECEITA;
        let novo = alvos[0];
        registro::linha(format!("monitor virtual: foto do usuário (alvo {}): {}", novo.id, foto.descrever()));
        let mut tent = Tentativas::default();
        let mut ultimo_motivo = String::from("nenhuma leitura ainda");
        let mut novo_ativo = false;
        let (mut ja_disponivel, mut clone_visto) = (false, false);
        // O tempo com leituras boas: é só ele que conta para a assinatura do adaptador travado. Com a
        // tela bloqueada (Win+L) as leituras falham, e isso não é o adaptador (a revisão de 15/09, 9).
        let (mut leituras_boas_ms, mut ultima_boa): (u64, Option<Instant>) = (0, None);
        loop {
            let agora = ms(t0.elapsed());
            if cancelar.load(Ordering::SeqCst) {
                return Err(CANCELADO.into());
            }
            match (ccd::ativos(), ccd::todos()) {
                (Ok(a), Ok((todos_crus, todos))) => {
                    if let Some(u) = ultima_boa {
                        leituras_boas_ms += ms(u.elapsed());
                    }
                    ultima_boa = Some(Instant::now());
                    novo_ativo = a.vista.iter().any(|p| p.alvo == novo && p.disponivel);
                    ja_disponivel |= ativacao::alvo_disponivel(&todos, novo) || novo_ativo;
                    let em_algum_caminho = todos.iter().any(|p| p.alvo == novo);
                    let decisao = ativacao::decidir(alvos, virtual_, &foto.vista, &a.vista, &todos);
                    if matches!(decisao, Decisao::DesfazerClone { .. }) {
                        clone_visto = true;
                    }
                    let pedido = match decisao {
                        Decisao::Pronto => {
                            n.ativo_ms = Some(agora);
                            return Ok(());
                        }
                        Decisao::Esperar(m) => {
                            n.esperas += 1;
                            if n.esperas <= 3 || n.esperas % 100 == 0 {
                                registro::linha(format!("monitor virtual: alvo {} espera {} em +{agora} ms: {m}", novo.id, n.esperas));
                            }
                            ultimo_motivo = m;
                            None
                        }
                        // Com o novo ativo e os outros sem caminho livre, não há o que pedir.
                        Decisao::Estender { ref novos, .. } if novos.is_empty() => {
                            ultimo_motivo = "o novo está ativo; um dos outros está fora e sem caminho disponível".into();
                            None
                        }
                        Decisao::DesfazerClone { .. } | Decisao::Estender { .. } if em_algum_caminho && tent.pode_pedir(agora, &l) => Some(decisao),
                        _ => None,
                    };
                    if let Some(d) = pedido {
                        tent.pediu(agora);
                        n.pedidos += 1;
                        let (desfazer, (caminhos, modos)) = match &d {
                            Decisao::DesfazerClone { virtuais, .. } => (true, ccd::montar(foto, &a, virtuais, &todos_crus, &[])),
                            Decisao::Estender { virtuais, novos } => (false, ccd::montar(foto, &a, virtuais, &todos_crus, novos)),
                            _ => unreachable!("só os dois pedidos chegam aqui"),
                        };
                        let tt = Instant::now();
                        let rc = ccd::aplicar(&caminhos, &modos);
                        let dur = ms(tt.elapsed());
                        let tela = ccd::conferir_usuario(foto);
                        registro::linha(format!(
                            "monitor virtual: pedido {} em +{agora} ms alvo {} {}: SetDisplayConfig={rc} ({dur} ms) caminhos={} tela_do_usuario={}",
                            n.pedidos,
                            novo.id,
                            if desfazer { "desfaz o clone do Windows" } else { "estende" },
                            caminhos.len(),
                            tela.texto()
                        ));
                        // Só a tela **lida** diferente desfaz; ilegível segue (item 9).
                        if let ccd::TelaConferida::Diferente(dif) = tela {
                            return Err(self.devolver_a_tela(foto, &dif));
                        }
                        if desfazer && rc == 0 {
                            n.clones_desfeitos += 1;
                        }
                        if rc != 0 {
                            ultimo_motivo = format!("SetDisplayConfig={rc}");
                        }
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    ultima_boa = None;
                    ultimo_motivo = format!("QueryDisplayConfig={e}");
                }
            }
            let agora = t0.elapsed();
            // A assinatura do adaptador travado (§12.3): nenhum caminho disponível para o alvo novo,
            // e nenhum clone nosso à vista, por 3 s **de leituras boas**. Não gastar 8 s por sessão: o
            // processo inteiro passa a recusar, com o motivo.
            if novo_monitor && leituras_boas_ms >= ms(ASSINATURA_DO_TRAVADO) && !ja_disponivel && !clone_visto {
                let m = idioma::t("o SudoVDA não põe o monitor novo em caminho disponível nenhum (targetAvailable=0 em todos por 3 s): é a assinatura do adaptador travado, §12.3. Reiniciar o Windows costuma resolver.").to_string();
                if let Ok(mut t) = self.c.adaptador_travado.lock() {
                    *t = Some(m.clone());
                }
                return Err(m);
            }
            if ativacao::prazo_vencido(ms(agora), &l) {
                if novo_ativo {
                    // O novo está na área de trabalho, mas um dos outros não voltou: o novo vale, e o
                    // registro diz quem ficou fora (a sessão dele percebe e decide).
                    n.ativo_ms = Some(ms(agora));
                    registro::linha(format!("monitor virtual: !! alvo {} ativo, mas {ultimo_motivo}", novo.id));
                    return Ok(());
                }
                return Err(idioma::tf("o monitor não ficou pronto em {} s ({})", &[&(l.prazo_ms / 1000), &ultimo_motivo]));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A tela do usuário mudou depois de um pedido nosso: volta à foto **sem os nossos** e desiste
    /// (§13.4; a revisão, item 22). Nunca se reativa contra o Win+P da pessoa.
    fn devolver_a_tela(&mut self, foto: &ccd::TelaDoUsuario, diferenca: &str) -> String {
        let rc = ccd::aplicar(&foto.caminhos, &foto.modos);
        let agora = ccd::conferir_usuario(foto);
        registro::linha(format!(
            "monitor virtual: !! a tela do usuário mudou depois do pedido ({diferenca}); devolvida à foto sem os nossos: SetDisplayConfig={rc}, tela_do_usuario={}",
            agora.texto()
        ));
        self.recolhido = true;
        // As outras sessões saem com o que aconteceu, e não com "o monitor foi desconectado" depois
        // dos 3 s fora (a revisão de 15/09, item 17).
        for v in self.vivos.iter_mut().filter(|v| v.ficha.is_some()) {
            v.terminal = Some(idioma::t(TELA_DEVOLVIDA).into());
        }
        idioma::tf("a tela do usuário mudou depois do pedido ({}); ela voltou à foto sem os monitores virtuais, e este não nasce", &[&diferenca])
    }

    fn criar(&mut self, pedido: &PedidoDeMonitor, ficha: u64, cancelar: &AtomicBool, fila_ms: u64) -> Result<Criado, String> {
        if cancelar.load(Ordering::SeqCst) {
            return Err(CANCELADO.into());
        }
        if self.c.captura_travada.load(Ordering::SeqCst) {
            return Err(crate::sessoes::AVISO_DA_CAPTURA_PRESA.into());
        }
        if let Some(m) = self.c.adaptador_travado.lock().ok().and_then(|t| t.clone()) {
            return Err(m);
        }
        let placa = self.fixar_placa()?;
        self.recolhido = false;
        self.c.recolher_pedido.store(false, Ordering::SeqCst);
        let guid = sudovda::guid_da_identidade(pedido.identidade);
        let mut nasc = Nascimento { indice: pedido.indice, fila_ms, ..Default::default() };

        // **O mesmo aparelho que volta herda o monitor vivo** (o ADD com o mesmo GUID devolveria o
        // mesmo monitor, §5.3): não soltar e recriar.
        if let Some(i) = self.vivos.iter().position(|v| v.guid == guid) {
            if self.vivos[i].ficha.is_some() {
                return Err(idioma::t("o monitor deste aparelho ainda está com outra sessão").into());
            }
            self.vivos[i].ficha = Some(ficha);
            let alvo = self.vivos[i].alvo;
            if !matches!(ler_alvo(alvo), Leitura::Ativo(_)) {
                if let Some(v) = self.adaptador_virtual {
                    self.reparar(&[alvo], v, "adoção");
                }
            }
            nasc.alvo = alvo.id;
            nasc.adotado = true;
            return self.entregar(i, placa, pedido, nasc);
        }

        let (nome, serie) = sudovda::textos_do_edid(pedido.identidade, pedido.indice);
        let p = sudovda::AddParams {
            width: pedido.largura,
            height: pedido.altura,
            refresh_rate: pedido.hertz,
            monitor_guid: guid,
            device_name: sudovda::texto_edid(&nome)?,
            serial_number: sudovda::texto_edid(&serie)?,
        };
        // Os outros **com dono** (item 6): o órfão fora dos alvos — o pedido não o reativa, e o `Pronto`
        // não espera por ele.
        let outros: Vec<Par> = self.vivos.iter().filter(|v| v.ficha.is_some()).map(|v| v.alvo).collect();
        let nomes_antes: Vec<(u32, Option<String>)> =
            outros.iter().map(|a| (a.id, ccd::gdi_do_alvo(a.adaptador, a.id).ok().flatten())).collect();
        // **A foto da tela do usuário antes do ADD**: depois dele, as leituras do `DisplayConfig` ficam
        // presas atrás da chegada do monitor (1,3 s no primeiro ADD da N = 1 de 15/09), e o Windows
        // clona o monitor nesse meio tempo. A foto de antes é a tela da pessoa sem o nosso — o que a
        // receita quer do "instante do ADD".
        let foto = ccd::foto(self.adaptador_virtual.unwrap_or(0)).map_err(|e| format!("a foto da tela do usuário falhou ({e}); sem ela não se pede nada"))?;
        // A referência: a primeira foto — ou a de agora, quando nenhum monitor nosso tem dono (a
        // pessoa pode ter mudado a tela entre uma rodada e outra).
        if self.referencia.is_none() || self.com_dono() == 0 {
            self.referencia = Some(Referencia { topologia: ccd::topologia(), alvos: foto.alvos(), foto: foto.clone() });
        }
        // Lembrado **antes** do ADD: se o `IddCxMonitorArrival` falhar, o contexto já entrou na lista
        // do driver (`Driver.cpp:906-911`) e só um REMOVE com este GUID o tira.
        self.pendentes.push(guid);
        self.atualizar_de_pe();
        self.cuidar_do_ping();
        let t_add = Instant::now();
        let saida = self.disp.adicionar(&p);
        let limpar = |eu: &mut DonoDaTopologia, motivo: String| -> String {
            let r = eu.disp.soltar(guid);
            eu.pendentes.retain(|g| *g != guid);
            registro::linha(format!("monitor virtual: !! {motivo} — REMOVE de limpeza: {r:?}"));
            motivo
        };
        let (o, _) = match saida {
            Ok(x) => x,
            Err(e) => return Err(limpar(self, format!("o ADD do SudoVDA falhou: {e}"))),
        };
        let alvo = Par::novo(sudovda::luid_u64(o.adapter_luid), o.target_id);
        if alvo.adaptador == 0 && alvo.id == 0 {
            return Err(limpar(self, "o SudoVDA devolveu LUID e alvo zerados: o monitor não chegou ao Windows".into()));
        }
        if let Ok(mut a) = ADAPTADORES_NOSSOS.lock() {
            a.insert(alvo.adaptador);
        }
        self.adaptador_virtual = Some(alvo.adaptador);
        nasc.alvo = alvo.id;
        registro::linha(format!(
            "monitor virtual: ADD em {} ms alvo={:X}:{} guid={{{}}} edid=\"{nome}\"/\"{serie}\" {}x{}@{} (outros de pé: {}; esperou na fila {fila_ms} ms)",
            ms(t_add.elapsed()),
            alvo.adaptador as u32,
            alvo.id,
            guid_texto(&guid),
            pedido.largura,
            pedido.altura,
            pedido.hertz,
            outros.len()
        ));
        let mut alvos = vec![alvo];
        alvos.extend_from_slice(&outros);
        if let Err(motivo) = self.ativar(&alvos, alvo.adaptador, t_add, &mut nasc, cancelar, true, foto) {
            nasc.falha = Some(motivo.clone());
            let s = self.remover(guid, alvo, None, Duration::from_secs(5));
            self.pendentes.retain(|g| *g != guid);
            registro::linha(format!("monitor virtual: !! alvo {} não nasceu: {motivo} — solto ({s:?})", alvo.id));
            anotar(|r| r.nascimentos.push(nasc));
            return Err(motivo);
        }
        let caminho_do_monitor = ccd::nome_do_alvo(alvo.adaptador, alvo.id).map(|x| x.1).unwrap_or_default();
        let instancia = (!caminho_do_monitor.is_empty()).then(|| pnp::instancia_da_interface(&caminho_do_monitor)).flatten();
        nasc.nomes_dos_outros = nomes_antes
            .iter()
            .map(|(id, antes)| {
                let depois = ccd::gdi_do_alvo(alvo.adaptador, *id).ok().flatten();
                format!("{id}: {}→{}", antes.as_deref().unwrap_or("?"), depois.as_deref().unwrap_or("?"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.pendentes.retain(|g| *g != guid);
        self.vivos.push(VivoNoDono { guid, alvo, instancia, ficha: Some(ficha), cobertura: None, terminal: None });
        let i = self.vivos.len() - 1;
        self.entregar(i, placa, pedido, nasc)
    }

    /// O monitor de `vivos[i]` está ativo: acha onde, confere, abre a cobertura (bancada) e entrega.
    fn entregar(&mut self, i: usize, placa: u64, pedido: &PedidoDeMonitor, mut nasc: Nascimento) -> Result<Criado, String> {
        let alvo = self.vivos[i].alvo;
        let guid = self.vivos[i].guid;
        // Os outros já estão onde a ativação os deixou: o mapa sai agora, e não só no fim do
        // trabalho (as sessões deles reabrem a captura ~0,1–1 s mais cedo).
        self.publicar_mapa();
        // O HMONITOR pode chegar um pouco depois do caminho: até 1 s.
        let t1 = Instant::now();
        let mut leitura = ler_alvo(alvo);
        while !matches!(leitura, Leitura::Ativo(_)) && t1.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
            leitura = ler_alvo(alvo);
        }
        let onde = match leitura {
            Leitura::Ativo(o) => o,
            outra => {
                let motivo = format!("o monitor ficou ativo mas não é capturável: {outra:?}");
                let _ = self.soltar(guid, Duration::from_secs(5), 0);
                nasc.falha = Some(motivo.clone());
                anotar(|r| r.nascimentos.push(nasc));
                return Err(motivo);
            }
        };
        let modo = modo_atual(&onde.gdi);
        nasc.modo = modo.map_or("?".into(), |(l, a, f)| {
            let ok = (l, a) == (pedido.largura, pedido.altura) && f.abs_diff(pedido.hertz) <= 1;
            format!("{l}x{a}@{f}{}", if ok { " (o pedido)" } else { " (DIFERENTE do pedido)" })
        });
        // **O tamanho diferente do pedido, já no nascimento** (a revisão de 15/09, item 18): a cadeia
        // abriu no tamanho do pedido, e a captura só troca com o tamanho igual — descobrir na troca é
        // descobrir tarde, com o receptor esperando.
        let (l, a) = (onde.largura(), onde.altura());
        if (l, a) != (pedido.largura, pedido.altura) {
            let motivo = idioma::tf("o Windows pôs o monitor em {}x{}, e o pedido era {}x{}: a sessão não abre num tamanho que não é o da tela do aparelho", &[&l, &a, &pedido.largura, &pedido.altura]);
            let _ = self.soltar(guid, Duration::from_secs(5), 0);
            nasc.falha = Some(motivo.clone());
            anotar(|r| r.nascimentos.push(nasc));
            return Err(motivo);
        }
        nasc.desenha = placa_que_desenha(&onde.gdi).map_or("?".into(), |(d, l)| {
            format!("{d}{}", if l == placa { "" } else { " (!! não é a placa do processo)" })
        });
        // A cobertura da bancada, com o monitor parado debaixo dela (o dono não mexe em mais nada
        // enquanto ela nasce), e antes de a sessão abrir a captura.
        if self.cobrir.is_some() && self.vivos[i].cobertura.is_none() {
            match Cobertura::iniciar(self.cobrir.unwrap_or(cobertura::Modo::Camadas), (alvo.adaptador, alvo.id), placa) {
                Ok((c, linha)) => {
                    registro::linha(linha);
                    self.vivos[i].cobertura = Some(c);
                }
                Err(e) => {
                    let motivo = format!("a janela sintética não cobriu o monitor ({e}); sem ela não se captura");
                    let _ = self.soltar(guid, Duration::from_secs(5), 0);
                    nasc.falha = Some(motivo.clone());
                    anotar(|r| r.nascimentos.push(nasc));
                    return Err(motivo);
                }
            }
        }
        registro::linha(format!(
            "monitor virtual: alvo {} {} em {} ms como {} modo={} desenha={} pedidos={} clones_desfeitos={} esperas={} fila={} ms | outros: {} | nomes dos outros: [{}] | pnp={}",
            alvo.id,
            if nasc.adotado { "ADOTADO" } else { "ativo" },
            nasc.ativo_ms.unwrap_or(0),
            onde.gdi,
            nasc.modo,
            nasc.desenha,
            nasc.pedidos,
            nasc.clones_desfeitos,
            nasc.esperas,
            nasc.fila_ms,
            nasc.outros,
            nasc.nomes_dos_outros,
            self.vivos[i].instancia.as_deref().unwrap_or("?")
        ));
        let descricao = format!(
            "monitor virtual {} alvo={:X}:{} {} ({} ms do ADD ao ativo, desenhado por {})",
            onde.gdi,
            alvo.adaptador as u32,
            alvo.id,
            nasc.modo,
            nasc.ativo_ms.unwrap_or(0),
            nasc.desenha
        );
        anotar(|r| r.nascimentos.push(nasc));
        Ok(Criado {
            guid,
            alvo,
            placa,
            onde,
            modo,
            portao: self.vivos[i].cobertura.as_ref().map(|c| c.portao()),
            descricao,
        })
    }

    /// `REMOVE` e a testemunha (o caminho ativo e o nó PnP), com prazo. Leitura que falha não é saída.
    fn remover(&mut self, guid: GUID, alvo: Par, instancia: Option<&str>, prazo: Duration) -> Soltura {
        let t = Instant::now();
        let r = self.disp.soltar(guid);
        if let Err(e) = &r {
            registro::linha(format!("monitor virtual: REMOVE {{{}}}: {e}", guid_texto(&guid)));
        }
        loop {
            let caminho = ccd::ler(QDC_ONLY_ACTIVE_PATHS)
                .ok()
                .map(|(ps, _)| ps.iter().any(|p| ccd::luid(p.targetInfo.adapterId) == alvo.adaptador && p.targetInfo.id == alvo.id));
            let presente = match instancia {
                Some(i) => pnp::presenca(i).ok(),
                // Sem a instância (o monitorDevicePath não levou a um nó), só o caminho testemunha.
                None => Some(false),
            };
            if ativacao::saiu(caminho, presente) {
                return Soltura::Confirmada { ms: ms(t.elapsed()) };
            }
            if t.elapsed() >= prazo {
                return Soltura::NaoConfirmada { prazo_ms: ms(prazo) };
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn soltar(&mut self, guid: GUID, prazo: Duration, fila_ms: u64) -> Soltura {
        let Some(i) = self.vivos.iter().position(|v| v.guid == guid) else {
            // Só pendente (um ADD sem monitor vivo), ou já saiu: o REMOVE limpa, NOT_FOUND é o esperado.
            if self.pendentes.contains(&guid) {
                let _ = self.disp.soltar(guid);
                self.pendentes.retain(|g| *g != guid);
            }
            return Soltura::NadaASoltar;
        };
        // A janela sai antes do monitor (a sonda: "fechada antes do REMOVE").
        if let Some(c) = self.vivos[i].cobertura.take() {
            let r = c.parar();
            registro::linha(format!("monitor virtual: alvo {} {}", self.vivos[i].alvo.id, r.linha()));
            // Um respiro para o DWM tirar a janela antes de o monitor sair.
            std::thread::sleep(Duration::from_millis(100));
        }
        let (alvo, instancia) = (self.vivos[i].alvo, self.vivos[i].instancia.clone());
        let s = self.remover(guid, alvo, instancia.as_deref(), prazo);
        match s {
            Soltura::NaoConfirmada { .. } => {
                // Órfão: fica na lista sem dono, o dono refaz o REMOVE parado, e o mesmo aparelho que
                // volta o adota (a revisão, item 17).
                self.vivos[i].ficha = None;
            }
            _ => {
                self.vivos.remove(i);
            }
        }
        anotar(|rel| {
            rel.solturas.push((
                alvo.id,
                match s {
                    Soltura::Confirmada { ms } => Some(ms),
                    _ => None,
                },
                fila_ms,
            ))
        });
        registro::linha(format!(
            "monitor virtual: alvo {} solto: {s:?} (caminho ativo e nó PnP; fila {fila_ms} ms) topologia={} ativos={}",
            alvo.id,
            ccd::topologia(),
            ccd::resumo_dos_ativos()
        ));
        // **Depois de todo REMOVE**: os outros nossos seguem estendidos? A saída de um pode fazer o
        // Windows clonar ou desligar o outro (a revisão, item 2) — e o clone chega ~484 ms depois do
        // `REMOVE` (G1/G2), quando a testemunha já fechou em 0,1–1 s. Por isso o dono **observa por
        // 1,5 s**, de 20 em 20 ms, como o recolher faz; se algum sair, ou um clone nosso aparecer, de
        // volta pela foto de referência (a revisão de 15/09, item 7). Com o Parar a caminho, não: o
        // recolher tira todos.
        if !self.recolhido && !self.c.recolher_pedido.load(Ordering::SeqCst) && self.com_dono() > 0 {
            if let Some(v) = self.adaptador_virtual {
                let t = Instant::now();
                let mut reparado = false;
                while t.elapsed() < Duration::from_millis(1500) && !self.c.recolher_pedido.load(Ordering::SeqCst) {
                    let fora: Vec<Par> =
                        self.vivos.iter().filter(|x| x.ficha.is_some()).filter(|x| matches!(ler_alvo(x.alvo), Leitura::Inativo(_))).map(|x| x.alvo).collect();
                    if !fora.is_empty() || clone_nosso_agora(v) {
                        registro::linha(format!(
                            "monitor virtual: {} ms depois do REMOVE: {} dos nossos fora{}",
                            ms(t.elapsed()),
                            fora.len(),
                            if clone_nosso_agora(v) { ", com clone nosso à vista" } else { "" }
                        ));
                        let n = self.reparar(&fora, v, "depois do REMOVE");
                        anotar(|rel| rel.reparos.push((fora.len(), n)));
                        reparado = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                if !reparado {
                    anotar(|rel| rel.reparos.push((0, 0)));
                }
            }
        }
        s
    }

    /// **O Parar**: fecha as janelas da bancada, uma `SetDisplayConfig` só com a tela do usuário, e
    /// então os `REMOVE` de todos, **em sequência, sem esperar a testemunha de um para mandar o do
    /// outro** — a testemunha de todos vem depois. Medido na N = 2 de 15/09: com o recolher e os
    /// `REMOVE` um a um, quando o 2º saiu o Windows pôs o 1º **em clone** com a tela integrada (o
    /// banco dele guarda `Recent = Clone` para cada conjunto {tela + monitor nosso}, §13.1) até o
    /// `REMOVE` do 1º; a G com a sonda mostrou o mesmo. O recolher observa, de 5 em 5 ms, se algum
    /// monitor do SudoVDA aparece ativo por outro adaptador (o clone) enquanto os monitores saem.
    fn recolher(&mut self) {
        self.recolhido = true;
        let Some(v) = self.adaptador_virtual else { return };
        if self.vivos.is_empty() {
            return;
        }
        let t = Instant::now();
        for x in self.vivos.iter_mut() {
            if let Some(c) = x.cobertura.take() {
                let r = c.parar();
                registro::linha(format!("monitor virtual: alvo {} {}", x.alvo.id, r.linha()));
            }
        }
        // Um respiro para o DWM tirar as janelas antes de os monitores saírem.
        std::thread::sleep(Duration::from_millis(100));
        let algum_ativo = self.vivos.iter().any(|x| matches!(ler_alvo(x.alvo), Leitura::Ativo(_)));
        let mut linha = String::new();
        if algum_ativo {
            match self.foto_para_pedir(v) {
                Ok((f, qual)) if !f.caminhos.is_empty() => {
                    let ts = Instant::now();
                    let rc = ccd::aplicar(&f.caminhos, &f.modos);
                    linha = format!(
                        "SetDisplayConfig só com a tela do usuário ({qual}) = {rc} em {} ms (tela_do_usuario={})",
                        ms(ts.elapsed()),
                        ccd::conferir_usuario(&f).texto()
                    );
                }
                Ok(_) => linha = "a foto do usuário veio vazia — não pedi".into(),
                Err(e) => linha = format!("a foto falhou ({e})"),
            }
        }
        // Os REMOVE, todos, em sequência.
        let saindo: Vec<(GUID, Par, Option<String>)> = self.vivos.iter().map(|x| (x.guid, x.alvo, x.instancia.clone())).collect();
        let tr = Instant::now();
        for (g, _, _) in &saindo {
            if let Err(e) = self.disp.soltar(*g) {
                registro::linha(format!("monitor virtual: recolher: REMOVE {{{}}}: {e}", guid_texto(g)));
            }
        }
        // As testemunhas de todos, e o clone observado enquanto eles saem.
        let (mut clone_ms, mut clone_desde): (u64, Option<Instant>) = (0, None);
        let mut faltam: Vec<(GUID, Par, Option<String>)> = saindo.clone();
        let mut confirmados: Vec<(u32, u64)> = Vec::new();
        while !faltam.is_empty() && tr.elapsed() < Duration::from_secs(5) {
            let lidos = ccd::ler(QDC_ONLY_ACTIVE_PATHS).ok();
            let clone_agora = lidos.as_ref().is_some_and(|(ps, _)| {
                ps.iter().any(|p| {
                    ccd::luid(p.targetInfo.adapterId) != v
                        && ccd::nome_do_alvo(ccd::luid(p.targetInfo.adapterId), p.targetInfo.id).is_ok_and(|(_, c)| ccd::caminho_do_sudovda(&c))
                })
            });
            match (clone_agora, clone_desde) {
                (true, None) => clone_desde = Some(Instant::now()),
                (false, Some(d)) => {
                    clone_ms += ms(d.elapsed());
                    clone_desde = None;
                }
                _ => {}
            }
            faltam.retain(|(_, alvo, inst)| {
                let caminho = lidos
                    .as_ref()
                    .map(|(ps, _)| ps.iter().any(|p| ccd::luid(p.targetInfo.adapterId) == alvo.adaptador && p.targetInfo.id == alvo.id));
                let presente = match inst {
                    Some(i) => pnp::presenca(i).ok(),
                    None => Some(false),
                };
                if ativacao::saiu(caminho, presente) {
                    confirmados.push((alvo.id, ms(tr.elapsed())));
                    false
                } else {
                    true
                }
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(d) = clone_desde {
            clone_ms += ms(d.elapsed());
        }
        for (_, alvo, _) in &faltam {
            anotar(|rel| rel.solturas.push((alvo.id, None, 0)));
        }
        for (alvo, t_ms) in &confirmados {
            anotar(|rel| rel.solturas.push((*alvo, Some(*t_ms), 0)));
        }
        // Os confirmados saem da lista; os que não confirmaram ficam órfãos (o dono refaz o REMOVE).
        let fora: Vec<GUID> = saindo.iter().filter(|(_, a, _)| confirmados.iter().any(|(id, _)| *id == a.id)).map(|(g, _, _)| *g).collect();
        self.vivos.retain(|x| !fora.contains(&x.guid));
        for x in self.vivos.iter_mut() {
            x.ficha = None;
        }
        let linha = format!(
            "recolher em {} ms: {linha}; {} REMOVE em sequência, {} testemunhados em {:?} ms, {} sem confirmar; clone de monitor nosso visto por {clone_ms} ms enquanto saíam; topologia={} ativos={}",
            ms(t.elapsed()),
            saindo.len(),
            confirmados.len(),
            confirmados.iter().map(|c| c.1).collect::<Vec<_>>(),
            faltam.len(),
            ccd::topologia(),
            ccd::resumo_dos_ativos()
        );
        registro::linha(format!("monitor virtual: {linha}"));
        anotar(|r| r.recolhimentos.push(linha));
    }

    /// A saída do processo: recolhe, solta todos (vivos e pendentes) e para o ping.
    fn encerrar_tudo(&mut self) -> usize {
        self.recolher();
        let guids: Vec<GUID> = self.vivos.iter().map(|v| v.guid).collect();
        let mut n = 0;
        for g in guids {
            let _ = self.soltar(g, Duration::from_secs(2), 0);
            n += 1;
        }
        // Órfãos que continuam sem confirmar: o REMOVE já saiu; o vigia termina o serviço.
        for v in self.vivos.drain(..) {
            let _ = self.disp.soltar(v.guid);
        }
        for g in std::mem::take(&mut self.pendentes) {
            let _ = self.disp.soltar(g);
            n += 1;
        }
        self.cuidar_do_ping();
        n
    }
}

// ================================================================================================
// O lado de fora: MonitoresVirtuais, um por processo
// ================================================================================================

pub struct MonitoresVirtuais {
    c: Arc<Compartilhado>,
    pub cobrir: Option<cobertura::Modo>,
    /// A placa do processo (sem os adaptadores indiretos): descrição e LUID.
    placa: (String, u64),
}

/// **Os monitores virtuais do processo, só quando abriram.** Antes era um `OnceLock` com o
/// `Result`: a primeira falha ficava guardada, e a tela estendida só voltava reiniciando o app.
/// Agora a falha não é guardada e o próximo Espelhar tenta de novo
/// ([`regras_da_tela_estendida::decidir_abertura`]). Quem abre é sempre o fio do coordenador; o
/// trinco não fica preso durante o `abrir` (a janela lê daqui no `WM_DISPLAYCHANGE`).
static INSTANCIA: Mutex<Option<Arc<MonitoresVirtuais>>> = Mutex::new(None);

/// A placa que o `SET_RENDER_ADAPTER` fixou **neste processo** (descrição e LUID). Uma abertura
/// depois de o dono cair usa esta, e nunca outra: trocar a placa com o driver vivo trava o
/// adaptador até reiniciar (§12.3).
static PLACA_FIXADA: Mutex<Option<(String, u64)>> = Mutex::new(None);

/// O handle do mutex nomeado `Global\QuallMonitoresVirtuais` que este processo segura (0 = nenhum).
static POSSE_DO_MUTEX: Mutex<isize> = Mutex::new(0);

/// Os adaptadores virtuais que este processo já viu num `ADD`: é por eles (e pelo `SMKD1CE`) que
/// um monitor é "nosso". Vazio = este processo nunca criou monitor, e [`gdi_e_nosso`] responde na
/// hora, sem ler o `DisplayConfig` — o caminho de uma sessão só não muda em nada.
static ADAPTADORES_NOSSOS: Mutex<BTreeSet<u64>> = Mutex::new(BTreeSet::new());

/// O adaptador do SudoVDA está presente e publica a interface de controle? Só lê o PnP (não abre o
/// dispositivo: qualquer IOCTL recarregaria o vigia). É o que decide se a janela oferece a fonte
/// "Tela estendida".
pub fn adaptador_presente() -> bool {
    matches!(pnp::interface_do_sudovda(), Ok(Some(_)))
}

/// Um mutex nomeado para o modo do monitor virtual: o vigia do SudoVDA é de todos os processos, e
/// duas instâncias pingando manteriam os monitores uma da outra (a revisão, item 13).
///
/// Devolve `true` quando o mutex foi pego **agora** (e então quem chamou o solta se a abertura
/// falhar, com [`soltar_o_mutex`]); `false` quando este processo já o segurava de uma abertura
/// anterior. Sem isto, a nova tentativa daria de cara com o mutex do próprio processo e se acusaria
/// de "outro processo".
fn mutex_da_instancia() -> Result<bool, String> {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    let mut posse = POSSE_DO_MUTEX.lock().map_err(|_| "o trinco do mutex da instância envenenou".to_string())?;
    if *posse != 0 {
        return Ok(false);
    }
    // `Global\`: dois Quall em sessões diferentes do Windows pegariam os dois um `Local\`, e a
    // varredura do segundo soltaria os monitores **vivos** do primeiro (a revisão de 15/09, item 11).
    let h = unsafe { CreateMutexW(None, true, windows::core::w!("Global\\QuallMonitoresVirtuais")) }.map_err(|e| format!("CreateMutexW: {e}"))?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };
        return Err(idioma::t("outro Quall aberto neste computador já está usando a tela estendida (o vigia do SudoVDA é de todos): um de cada vez").into());
    }
    // O handle fica aberto enquanto os monitores virtuais deste processo existirem: é ele que marca
    // a posse. Só uma abertura que falha o solta.
    *posse = h.0 as isize;
    Ok(true)
}

/// Solta o mutex nomeado (uma abertura que falhou): outro Quall pode usar a tela estendida, e a nova
/// tentativa deste o pega de novo. Mesmo fio que o pegou (o do coordenador).
fn soltar_o_mutex() {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::ReleaseMutex;
    if let Ok(mut posse) = POSSE_DO_MUTEX.lock() {
        if *posse != 0 {
            let h = HANDLE(*posse as *mut core::ffi::c_void);
            unsafe {
                let _ = ReleaseMutex(h);
                let _ = CloseHandle(h);
            }
            *posse = 0;
        }
    }
}

impl MonitoresVirtuais {
    /// **O único do processo.** O primeiro chamado abre o dispositivo, confere o protocolo, escolhe
    /// a placa, varre os monitores do SudoVDA presentes (os do espaço de nomes do app, órfãos de uma
    /// corrida anterior, são soltos; de outro programa, recusa) e põe o dono de pé.
    ///
    /// **Uma falha não fica guardada** (02/10): o próximo Espelhar tenta de novo. Um dono que caiu
    /// (pânico) é trocado por outro quando ninguém mais segura nada dele e o vigia do SudoVDA já teve
    /// tempo de tirar os monitores; o novo herda a placa e as travas do processo.
    pub fn do_processo(cobrir: Option<cobertura::Modo>, sem_intel: bool) -> Result<Arc<MonitoresVirtuais>, String> {
        use crate::regras_da_tela_estendida::{decidir_abertura, Abertura, Instancia};
        let atual = INSTANCIA.lock().map_err(|_| "o trinco dos monitores virtuais envenenou".to_string())?.clone();
        let estado = match &atual {
            None => Instancia::Nenhuma,
            Some(m) if m.c.dono_vivo.load(Ordering::SeqCst) => Instancia::Viva,
            Some(m) => Instancia::DonoMorto {
                // `c` é segurado pela própria instância; qualquer outro é uma sessão com monitor, o
                // fio do dono ou o do ping ainda saindo.
                segurada: Arc::strong_count(&m.c) > 1,
                ms_desde_a_morte: ms(m.c.base.elapsed()).saturating_sub(m.c.morte_ms.load(Ordering::SeqCst).saturating_sub(1)),
            },
        };
        match decidir_abertura(estado) {
            Abertura::Reusar => {
                let m = atual.ok_or("a instância sumiu")?;
                if m.cobrir != cobrir {
                    registro::linha(format!(
                        "monitor virtual: a cobertura deste processo ficou {:?} (pedida {:?}: é uma por processo)",
                        m.cobrir, cobrir
                    ));
                }
                Ok(m)
            }
            Abertura::Esperar(motivo) => {
                registro::linha(format!("monitor virtual: o dono anterior caiu e ainda não dá para abrir outro ({estado:?})"));
                Err(motivo.into())
            }
            Abertura::Abrir { herdar } => {
                let heranca = if herdar {
                    atual.as_ref().map(|m| (m.c.captura_travada.load(Ordering::SeqCst), m.c.adaptador_travado.lock().ok().and_then(|t| t.clone())))
                } else {
                    None
                };
                drop(atual);
                if herdar {
                    registro::linha("monitor virtual: o dono anterior caiu — abrindo outro (a placa e as travas do processo vêm junto)");
                }
                let m = Arc::new(Self::abrir(cobrir, sem_intel, heranca)?);
                if let Ok(mut i) = INSTANCIA.lock() {
                    *i = Some(m.clone());
                }
                Ok(m)
            }
        }
    }

    /// A abertura, com o mutex nomeado solto de novo se ela falhar depois de pegá-lo.
    fn abrir(cobrir: Option<cobertura::Modo>, sem_intel: bool, heranca: Option<(bool, Option<String>)>) -> Result<MonitoresVirtuais, String> {
        let interface = pnp::interface_do_sudovda()?.ok_or(idioma::t("o driver SudoVDA não está instalado ou o adaptador dele está desligado"))?;
        let pegou_agora = mutex_da_instancia()?;
        let r = Self::abrir_com_o_mutex(interface, cobrir, sem_intel, heranca);
        if let Err(e) = &r {
            registro::linha(format!("monitor virtual: a abertura falhou ({e}); o próximo Espelhar tenta de novo"));
            if pegou_agora {
                soltar_o_mutex();
            }
        }
        r
    }

    fn abrir_com_o_mutex(
        interface: String,
        cobrir: Option<cobertura::Modo>,
        sem_intel: bool,
        heranca: Option<(bool, Option<String>)>,
    ) -> Result<MonitoresVirtuais, String> {
        let disp = Dispositivo::abrir(&interface).map_err(|e| idioma::tf("o driver SudoVDA não respondeu ({})", &[&e]))?;
        let v = disp.versao().map_err(|e| idioma::tf("o driver SudoVDA não disse a versão (GET_PROTOCOL_VERSION: {})", &[&e]))?;
        if (v.major, v.minor) != (sudovda::PROTOCOLO.0, sudovda::PROTOCOLO.1) {
            let deste = format!("{}.{}.{}", v.major, v.minor, v.incremental);
            let conhecido = format!("{}.{}.{}", sudovda::PROTOCOLO.0, sudovda::PROTOCOLO.1, sudovda::PROTOCOLO.2);
            return Err(idioma::tf("esta versão do driver SudoVDA (protocolo {}) não é a que o Quall conhece ({})", &[&deste, &conhecido]));
        }
        // A placa já fixada neste processo (um dono anterior caiu) vale, e nunca outra (§12.3); sem
        // ela, a escolha — antes dos `REMOVE` da varredura, para uma placa que não ativa não mexer
        // em nada.
        let fixada = PLACA_FIXADA.lock().ok().and_then(|f| f.clone());
        let placa = match fixada {
            Some(p) => p,
            None => escolher_a_placa(sem_intel)?,
        };
        let vigia_s = disp.vigia().map(|w| w.timeout).unwrap_or(3);
        // A varredura: os monitores do SudoVDA presentes. Um de outro programa: não mandar
        // `SET_RENDER_ADAPTER` (ele pode estar numa placa que não é a nossa) e recusar.
        let presentes = pnp::monitores_presentes()?;
        let (nossos, alheios): (Vec<_>, Vec<_>) = presentes.iter().partition(|(_, g)| g.is_some_and(|g| sudovda::guid_e_do_app(&g)));
        if !alheios.is_empty() {
            let quais = alheios.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>().join(", ");
            return Err(idioma::tf("outro programa está usando o SudoVDA agora, com um monitor ligado ({}); feche-o e tente de novo", &[&quais]));
        }
        for (inst, g) in &nossos {
            if let Some(g) = g {
                let r = disp.soltar(*g);
                registro::linha(format!("monitor virtual: varredura — órfão de uma corrida anterior {inst} {{{}}}: REMOVE {r:?}", guid_texto(g)));
            }
        }
        registro::linha(format!(
            "monitor virtual: SudoVDA presente, protocolo {}.{}.{}, vigia de {vigia_s} s; placa para desenhar: {} luid={:016X}; {} órfão(s) soltos na varredura; cobertura: {}",
            v.major,
            v.minor,
            v.incremental,
            placa.0,
            placa.1,
            nossos.len(),
            cobrir.map_or("nenhuma", |m| m.nome())
        ));
        let c = Arc::new(Compartilhado {
            fila: Mutex::new(Fila::default()),
            sinal: Condvar::new(),
            mapa: Mutex::new(Mapa::default()),
            base: Instant::now(),
            batimento_do_coordenador: AtomicU64::new(0),
            dono_vivo: AtomicBool::new(true),
            captura_travada: AtomicBool::new(false),
            recolher_pedido: AtomicBool::new(false),
            adaptador_travado: Mutex::new(None),
            de_pe: AtomicUsize::new(0),
            proxima_ficha: AtomicU64::new(1),
            ping_pings: AtomicU64::new(0),
            ping_falhas: AtomicU64::new(0),
            ping_maior: AtomicU64::new(0),
            ping_pulados: AtomicU64::new(0),
            ping_ultimo_bom: AtomicU64::new(0),
            morte_ms: AtomicU64::new(0),
        });
        // **O que a vez anterior deixou** (o dono dela caiu): a captura presa e o adaptador travado são
        // estado do Windows, e continuam valendo para o processo (§13.5, §12.3).
        if let Some((presa, travado)) = heranca {
            c.captura_travada.store(presa, Ordering::SeqCst);
            if let Ok(mut t) = c.adaptador_travado.lock() {
                *t = travado;
            }
        }
        let placa_do_processo = placa.clone();
        let dono = DonoDaTopologia {
            c: c.clone(),
            interface,
            disp: Arc::new(disp),
            placa_escolhida: placa,
            placa: None,
            adaptador_virtual: None,
            ping: None,
            vivos: Vec::new(),
            pendentes: Vec::new(),
            cobrir,
            referencia: None,
            recolhido: false,
            ultimo_reparo: None,
        };
        let c2 = c.clone();
        std::thread::Builder::new()
            .name("quall.dono-da-topologia".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dono.correr()));
                // Morto o dono, o ping para de pingar e o vigia tira os monitores em 2–3 s. O mapa
                // não pode ficar "mexendo" para sempre (a sessão repetiria a imagem congelada): a
                // leitura passa a ser terminal (`ler_mapa`), e quem espera acorda (item 5).
                c2.morte_ms.store(ms(c2.base.elapsed()) + 1, Ordering::SeqCst);
                c2.dono_vivo.store(false, Ordering::SeqCst);
                c2.mexendo(false);
                c2.sinal.notify_all();
                if r.is_err() {
                    registro::linha("monitor virtual: !! o dono da topologia entrou em pânico — o ping para e o vigia tira os monitores");
                }
            })
            .map_err(|e| format!("não consegui criar o fio dono da topologia: {e}"))?;
        Ok(MonitoresVirtuais { c, cobrir, placa: placa_do_processo })
    }

    /// Depois que a abertura de uma captura vence o prazo, o processo recusa `ADD` novo: o
    /// `CreateForMonitor` preso é o estado do Windows (§13.5), e cada monitor a mais piora.
    pub fn marcar_captura_travada(&self) {
        self.c.captura_travada.store(true, Ordering::SeqCst);
        anotar(|r| r.capturas_presas += 1);
    }
}

impl FonteDeMonitor for MonitoresVirtuais {
    fn descricao(&self) -> String {
        format!(
            "monitores virtuais do SudoVDA (um por sessão, na placa {}, pelo fio dono da topologia){}",
            self.placa.0,
            self.cobrir.map_or(String::new(), |m| format!(", cobertos pela janela sintética da bancada ({})", m.nome()))
        )
    }

    fn cadeia_na_placa_do_processo(&self) -> bool {
        true
    }

    fn placa_do_processo(&self) -> Option<Result<u64, String>> {
        Some(Ok(self.placa.1))
    }

    fn criar(&self, pedido: &PedidoDeMonitor, _placa: Option<u64>) -> Result<MonitorDaSessao, String> {
        let parar = AtomicBool::new(false);
        self.criar_com(pedido, &ContextoDoCriar { parar: &parar, bater: &|| {} })
    }

    /// Entra na fila do dono e espera a resposta, batendo o coração da sessão e olhando o Parar: com
    /// ele, o pedido é cancelado (o dono pula, ou para a ativação e solta) — sem esperar os 8 s.
    fn criar_com(&self, pedido: &PedidoDeMonitor, ctx: &ContextoDoCriar<'_>) -> Result<MonitorDaSessao, String> {
        if !self.c.dono_vivo.load(Ordering::SeqCst) {
            // A espera aberta continua na instância do dono que caiu: a nova só vem no próximo
            // Espelhar (a revisão de 02/10, item 6).
            return Err(idioma::tf("{} Toque em Parar e depois em Espelhar para abrir de novo.", &[&idioma::t(SEM_DONO)]));
        }
        let (tx, rx) = bounded(1);
        let cancelar = Arc::new(AtomicBool::new(false));
        let ficha = self.c.proxima_ficha.fetch_add(1, Ordering::SeqCst);
        self.c.enfileirar(Trabalho::Criar { pedido: pedido.clone(), ficha, cancelar: cancelar.clone(), entrou: Instant::now(), resposta: tx });
        let r = loop {
            (ctx.bater)();
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(r) => break r,
                Err(RecvTimeoutError::Timeout) => {
                    if ctx.parar.load(Ordering::SeqCst) {
                        cancelar.store(true, Ordering::SeqCst);
                    }
                    if !self.c.dono_vivo.load(Ordering::SeqCst) {
                        break Err("o dono da topologia morreu".into());
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break Err("o dono da topologia largou o pedido".into()),
            }
        };
        let criado = r?;
        let alvo_w = AlvoDoWindows { adapter_luid: criado.alvo.adaptador, target_id: criado.alvo.id };
        let (largura, altura) = criado.modo.map_or((criado.onde.largura(), criado.onde.altura()), |(l, a, _)| (l, a));
        let fonte = Fonte {
            id: criado.onde.gdi.clone(),
            nome: pedido.nome.clone(),
            largura,
            altura,
            primario: false,
            especie: crate::fontes::Especie::Monitor,
        };
        Ok(MonitorDaSessao {
            descricao: criado.descricao.clone(),
            origem: OrigemDoMonitor::Virtual { alvo: alvo_w, fonte },
            pedido: pedido.clone(),
            testemunha: TestemunhaDoMonitor::PorAlvo(alvo_w),
            placa: Some(criado.placa),
            virtual_: Some(Vivo {
                guid: criado.guid,
                alvo: criado.alvo,
                c: self.c.clone(),
                solto: AtomicBool::new(false),
                portao: criado.portao,
                onde_inicial: criado.onde,
            }),
        })
    }

    /// **O batimento do coordenador**, que o fio de ping exige (ver o cabeçalho): quem pinga é o fio
    /// próprio; este laço só diz que está vivo.
    fn alimentar(&self) {
        self.c.batimento_do_coordenador.store(ms(self.c.base.elapsed()), Ordering::Relaxed);
    }

    fn soltar(&self, monitor: &MonitorDaSessao, prazo: Duration) -> Soltura {
        let Some(v) = monitor.virtual_.as_ref() else { return Soltura::NadaASoltar };
        if v.solto.swap(true, Ordering::SeqCst) {
            return Soltura::NadaASoltar;
        }
        // Sem dono, a fila está morta: não esperar 20 s por ela. O ping já parou, e o vigia do SudoVDA
        // tira o monitor em 2–3 s (a revisão de 15/09, item 5).
        if !self.c.dono_vivo.load(Ordering::SeqCst) {
            registro::linha("monitor virtual: soltar sem o dono de pé — o vigia do SudoVDA tira o monitor em 2–3 s");
            return Soltura::NaoConfirmada { prazo_ms: 0 };
        }
        let (tx, rx) = bounded(1);
        self.c.enfileirar(Trabalho::Soltar { guid: v.guid, prazo, entrou: Instant::now(), resposta: Some(tx) });
        // O prazo conta no dono; aqui só um teto folgado para a fila (as solturas passam na frente) —
        // e o dono vivo, olhado a cada 100 ms.
        let teto = Instant::now() + prazo + Duration::from_secs(15);
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(s) => return s,
                Err(RecvTimeoutError::Disconnected) => return Soltura::NaoConfirmada { prazo_ms: ms(prazo) },
                Err(RecvTimeoutError::Timeout) => {
                    if !self.c.dono_vivo.load(Ordering::SeqCst) || Instant::now() >= teto {
                        return Soltura::NaoConfirmada { prazo_ms: ms(prazo) };
                    }
                }
            }
        }
    }

    fn recolher(&self) {
        self.c.recolher_pedido.store(true, Ordering::SeqCst);
        self.c.enfileirar(Trabalho::Recolher);
    }

    fn soltar_pela_chave(&self, chave_do_monitor: u128) {
        self.c.enfileirar(Trabalho::Soltar {
            guid: GUID::from_u128(chave_do_monitor),
            prazo: Duration::from_secs(5),
            entrou: Instant::now(),
            resposta: None,
        });
    }

    fn encerrar_tudo(&self, prazo: Duration) -> usize {
        if !self.c.dono_vivo.load(Ordering::SeqCst) {
            return 0;
        }
        let (tx, rx) = bounded(1);
        self.c.enfileirar(Trabalho::EncerrarTudo { resposta: tx });
        let teto = Instant::now() + prazo;
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(n) => return n,
                Err(RecvTimeoutError::Disconnected) => return 0,
                Err(RecvTimeoutError::Timeout) => {
                    if !self.c.dono_vivo.load(Ordering::SeqCst) || Instant::now() >= teto {
                        return 0;
                    }
                }
            }
        }
    }
}

/// A saída do processo: se este processo teve monitores virtuais, recolhe e solta todos, e para o
/// ping. Chamado depois do `esperar_desmonte` e antes do `MFShutdown`.
pub fn encerrar_tudo(prazo: Duration) -> usize {
    let m = INSTANCIA.lock().ok().and_then(|i| i.clone());
    match m {
        Some(m) => m.encerrar_tudo(prazo),
        None => 0,
    }
}

// ================================================================================================
// O monitor de uma sessão
// ================================================================================================

/// O que só o monitor virtual tem: a ficha de posse, a chave para o coordenador e o mapa.
pub struct Vivo {
    guid: GUID,
    alvo: Par,
    c: Arc<Compartilhado>,
    solto: AtomicBool,
    portao: Option<Arc<PortaoDeCobertura>>,
    onde_inicial: Onde,
}

impl Vivo {
    /// A chave do monitor (o GUID), que a sessão dá ao coordenador para ele poder soltar sozinho.
    pub fn chave(&self) -> u128 {
        chave(&self.guid)
    }

    pub fn alvo(&self) -> AlvoDoWindows {
        AlvoDoWindows { adapter_luid: self.alvo.adaptador, target_id: self.alvo.id }
    }

    /// `(época, o dono está mexendo, onde o alvo está)`.
    pub fn onde_agora(&self) -> (u64, bool, Leitura) {
        self.c.ler_mapa(self.alvo)
    }

    pub fn onde_inicial(&self) -> &Onde {
        &self.onde_inicial
    }

    /// O portão da cobertura (bancada); `None` sem cobertura.
    pub fn portao(&self) -> Option<Arc<PortaoDeCobertura>> {
        self.portao.clone()
    }

    /// Com cobertura, o portão está aberto agora (a janela visível sobre o monitor)?
    pub fn coberto_agora(&self) -> bool {
        self.portao.as_ref().is_none_or(|p| p.aberto_ha_folga())
    }

    pub fn captura_travada(&self) -> bool {
        self.c.captura_travada.load(Ordering::SeqCst)
    }

    pub fn marcar_captura_travada(&self) {
        self.c.captura_travada.store(true, Ordering::SeqCst);
        anotar(|r| r.capturas_presas += 1);
    }
}

impl Drop for Vivo {
    /// **A guarda**: o monitor que a sessão não soltou (um pânico, um caminho de erro esquecido) é
    /// solto aqui — o `REMOVE` vai para a fila do dono.
    fn drop(&mut self) {
        if !self.solto.swap(true, Ordering::SeqCst) {
            registro::linha(format!("monitor virtual: o monitor do alvo {} caiu sem ser solto — soltando pela guarda", self.alvo.id));
            self.c.enfileirar(Trabalho::Soltar { guid: self.guid, prazo: Duration::from_secs(5), entrou: Instant::now(), resposta: None });
        }
    }
}

// ================================================================================================
// "Nosso", pelo adaptador ou pelo monitorDevicePath — nunca pelo nome GDI
// ================================================================================================

/// O monitor com este nome GDI é um monitor virtual nosso? **Pelo mapa do dono**, que resolve cada
/// alvo nosso (o par `AdapterLuid + TargetId`, nunca o nome) no nome GDI de agora: quem pergunta é a
/// janela, a cada `WM_DISPLAYCHANGE`, e ler o `DisplayConfig` ali prenderia a janela atrás da
/// `SetDisplayConfig` de quem está chegando (a revisão de 15/09, item 15). Sem nenhum `ADD` neste
/// processo, `false` na hora — o caminho de uma sessão só não muda em nada.
pub fn gdi_e_nosso(gdi: &str) -> bool {
    if ADAPTADORES_NOSSOS.lock().map(|a| a.is_empty()).unwrap_or(true) {
        return false;
    }
    // `try_lock`: a janela não espera o coordenador (ele só segura o trinco para ler ou publicar).
    let Some(m) = INSTANCIA.try_lock().ok().and_then(|i| i.clone()) else { return false };
    m.c.mapa
        .lock()
        .map(|mapa| mapa.alvos.iter().any(|(_, l)| matches!(l, Leitura::Ativo(o) if o.gdi.eq_ignore_ascii_case(gdi))))
        .unwrap_or(false)
}

// ================================================================================================
// Diagnóstico da bancada: o dwm e os WUDFHost, sem abrir processo nenhum
// ================================================================================================

/// Os handles e a memória do `dwm` e dos `WUDFHost` (um deles hospeda o SudoVDA), pela mesma fonte do
/// `Get-Process` — `NtQuerySystemInformation(SystemProcessInformation)` —, sem abrir processo nenhum
/// (o `WUDFHost` é do LocalService). Porte de `bin/receita_monitor.rs`, módulo `sistema`. O leiaute é
/// o de `SYSTEM_PROCESS_INFORMATION` no x64: `NextEntryOffset` em 0, `ImageName` em 0x38,
/// `UniqueProcessId` em 0x50, `HandleCount` em 0x60, `WorkingSetSize` em 0x90.
pub fn sistema() -> String {
    use std::ffi::c_void;
    #[link(name = "ntdll")]
    extern "system" {
        fn NtQuerySystemInformation(classe: u32, buf: *mut c_void, tam: u32, devolvido: *mut u32) -> i32;
    }
    fn ler<const N: usize>(b: &[u8], i: usize) -> Option<[u8; N]> {
        b.get(i..i + N).and_then(|s| s.try_into().ok())
    }
    let mut tam = 1usize << 20;
    loop {
        let mut buf = vec![0u64; tam / 8];
        let mut devolvido = 0u32;
        let st = unsafe { NtQuerySystemInformation(5, buf.as_mut_ptr() as *mut c_void, tam as u32, &mut devolvido) };
        if st == 0xC000_0004u32 as i32 && tam < (64 << 20) {
            tam *= 2;
            continue;
        }
        if st != 0 {
            return "? (NtQuerySystemInformation falhou)".into();
        }
        let b: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, tam) };
        let (mut dwm, mut wudf) = (Vec::new(), Vec::new());
        let mut off = 0usize;
        while let (Some(prox), Some(len), Some(ptr), Some(pid), Some(h), Some(ws)) = (
            ler::<4>(b, off).map(u32::from_le_bytes),
            ler::<2>(b, off + 0x38).map(u16::from_le_bytes),
            ler::<8>(b, off + 0x40).map(usize::from_le_bytes),
            ler::<8>(b, off + 0x50).map(usize::from_le_bytes),
            ler::<4>(b, off + 0x60).map(u32::from_le_bytes),
            ler::<8>(b, off + 0x90).map(usize::from_le_bytes),
        ) {
            let base = b.as_ptr() as usize;
            let nome = if ptr >= base && ptr + len as usize <= base + tam && len > 0 {
                String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr as *const u16, len as usize / 2) })
            } else {
                String::new()
            };
            if nome.eq_ignore_ascii_case("dwm.exe") {
                dwm.push(format!("{h}h/{:.0}MB", ws as f64 / 1048576.0));
            } else if nome.eq_ignore_ascii_case("WUDFHost.exe") {
                wudf.push(format!("{pid}:{h}"));
            }
            if prox == 0 {
                break;
            }
            off += prox as usize;
        }
        return format!("dwm={} wudf=[{}]", dwm.join(","), wudf.join(" "));
    }
}
