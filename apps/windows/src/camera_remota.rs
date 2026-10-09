//! **O controle remoto da câmera no Windows** (R9b, `docs/controle-remoto-da-camera.md` §11.2 e
//! §12): a ponte entre o núcleo (`quall_core::camera_remota`, Rust direto, sem FFI) e o app.
//!
//! # Quem filma
//!
//! - **Um `Filmador` por captura de câmera**, nascido com a thread dos ajustes dela
//!   (`ajustes_da_camera.rs`) e morto com ela. Pendurado na captura, e não num global com "vez": a
//!   captura compartilhada da mesma câmera tem o dela (com `outro_app`), a reabertura traz um novo, e
//!   a velha nunca escreve no filmador da nova (a revisão do plano, achados 1, 2 e 16).
//! - **A thread dos ajustes é a fila serial do dono da câmera** (§6, achado B1): ela publica a
//!   câmera, aplica os gestos locais e os pedidos remotos, nessa ordem, e publica o lido.
//! - **As sessões de vídeo bombeiam** ([`BombaDoFilmador`], no laço de `sessao_de_emissao.rs`): o
//!   filmador da câmera da sessão (conferido a cada meio segundo, porque a câmera pode reabrir), ou,
//!   numa sessão de tela, o **filmador sem câmera** do processo, que só diz `camera` 0 (§3.2, §9).
//! - **A opção "Permitir controle remoto da câmera"**, desligada por padrão, vale para o app: ela
//!   fica em `%APPDATA%\Quall\camera-controle-remoto.txt` (como a escolha do idioma, `idioma.txt`), e
//!   cada mudança chega a todos os filmadores vivos.
//!
//! # Quem recebe
//!
//! Um [`ControleRemoto`] por sessão de recepção (`receptor.rs`), bombeado no laço dela; a janela
//! "Ajustes da câmera" em modo remoto (`janela_dos_ajustes.rs`) o lê e pede por ele.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use quall_core::camera_remota::{mudou_no_filmador, Controlador, Filmador};
use quall_core::transport::Mensageiro;

use crate::ajustes_da_camera::PontaDosAjustes;
use crate::regras_da_camera_remota as regras;
use crate::registro;

// =============================================================================================
// A opção, e os filmadores vivos
// =============================================================================================

fn caminho_da_permissao() -> std::path::PathBuf {
    crate::identidade::pasta_de_dados().join(regras::ARQUIVO_DA_PERMISSAO)
}

fn permissao() -> &'static AtomicBool {
    static P: OnceLock<AtomicBool> = OnceLock::new();
    P.get_or_init(|| AtomicBool::new(std::fs::read_to_string(caminho_da_permissao()).map(|t| regras::ler_permissao(&t)).unwrap_or(false)))
}

static VIVOS: Mutex<Vec<Weak<Filmador>>> = Mutex::new(Vec::new());

/// "Permitir controle remoto da câmera" está ligada?
pub fn permite() -> bool {
    permissao().load(Ordering::SeqCst)
}

/// **Liga ou desliga a opção**: grava, e avisa todos os filmadores vivos (o núcleo recusa a fila com
/// `nao_permitido` ao desligar, achado M6).
pub fn definir_permissao(permite: bool) {
    permissao().store(permite, Ordering::SeqCst);
    if let Err(e) = std::fs::write(caminho_da_permissao(), regras::texto_da_permissao(permite)) {
        registro::linha(format!("câmera remota: !! não consegui gravar {}: {e}", regras::ARQUIVO_DA_PERMISSAO));
    }
    let vivos: Vec<Arc<Filmador>> = {
        let mut v = VIVOS.lock().unwrap_or_else(|e| e.into_inner());
        v.retain(|w| w.strong_count() > 0);
        v.iter().filter_map(Weak::upgrade).collect()
    };
    for f in &vivos {
        let _ = f.permitir(permite);
    }
    registro::linha(format!("câmera remota: \"Permitir controle remoto da câmera\" {} ({} filmador(es) vivo(s))", if permite { "ligada" } else { "desligada" }, vivos.len()));
}

/// Um filmador novo, com a opção de agora, na lista dos que a opção alcança.
pub fn novo_filmador() -> Arc<Filmador> {
    let f = Arc::new(Filmador::novo());
    let _ = f.permitir(permite());
    let mut v = VIVOS.lock().unwrap_or_else(|e| e.into_inner());
    v.retain(|w| w.strong_count() > 0);
    v.push(Arc::downgrade(&f));
    f
}

/// **O filmador sem câmera** do processo: o das sessões de tela, e o da sessão de câmera enquanto a
/// câmera não tem controles (fechada, reabrindo, ou a do próprio Quall). Ele só diz `camera` 0, e o
/// receptor esconde os controles (`sem_camera`).
fn sem_camera() -> Arc<Filmador> {
    static F: OnceLock<Arc<Filmador>> = OnceLock::new();
    Arc::clone(F.get_or_init(novo_filmador))
}

// =============================================================================================
// A bombeada, numa sessão de vídeo de quem filma
// =============================================================================================

/// De quanto em quanto tempo a sessão confere qual é a câmera dela (a câmera reabre, troca, fecha).
const CONFERIR_A_CAMERA: Duration = Duration::from_millis(500);

/// **A bombeada da câmera numa sessão de vídeo** de quem filma. Uma por sessão, na thread dela.
pub struct BombaDoFilmador {
    m: Mensageiro,
    atual: Arc<Filmador>,
    ponta: Option<PontaDosAjustes>,
    conferida: Option<Instant>,
    ola_dito: bool,
}

impl BombaDoFilmador {
    pub fn nova(m: Mensageiro) -> BombaDoFilmador {
        BombaDoFilmador { m, atual: sem_camera(), ponta: None, conferida: None, ola_dito: false }
    }

    /// Bombeia **sem esperar** (o laço de transmissão tem o ritmo dele). `camera` diz qual é a câmera
    /// da sessão agora (`None` numa sessão de tela).
    pub fn bombear(&mut self, camera: Option<&dyn Fn() -> Option<PontaDosAjustes>>) {
        if self.conferida.is_none_or(|t| t.elapsed() >= CONFERIR_A_CAMERA) {
            self.conferida = Some(Instant::now());
            let ponta = camera.and_then(|f| f());
            let alvo = ponta.as_ref().and_then(|p| p.filmador()).unwrap_or_else(sem_camera);
            if !Arc::ptr_eq(&alvo, &self.atual) {
                // O receptor vê a época nova e refaz o painel; o filmador velho esquece a sessão.
                let _ = self.atual.esquecer(&self.m);
                self.atual = alvo;
                registro::linha(format!(
                    "câmera remota: a sessão passa a bombear {}",
                    if ponta.as_ref().and_then(|p| p.filmador()).is_some() { "o filmador da câmera aberta" } else { "o filmador sem câmera" }
                ));
            }
            self.ponta = ponta;
        }
        match self.atual.bombear(&self.m, Duration::ZERO) {
            Ok(b) => {
                if b.mudancas & mudou_no_filmador::PEDIDO != 0 {
                    if let Some(p) = &self.ponta {
                        p.acordar();
                    }
                }
                if b.mudancas & mudou_no_filmador::RECEPTORES != 0 && !self.ola_dito {
                    self.ola_dito = true;
                    registro::linha("câmera remota: o receptor desta sessão disse ola (controle remoto da câmera)");
                }
            }
            Err(e) => registro::linha(format!("câmera remota: !! a bombeada falhou: {e}")),
        }
    }
}

impl Drop for BombaDoFilmador {
    fn drop(&mut self) {
        let _ = self.atual.esquecer(&self.m);
    }
}

// =============================================================================================
// Quem recebe
// =============================================================================================

/// **O controle da câmera do outro lado**, numa sessão de recepção.
pub struct ControleRemoto {
    pub controlador: Controlador,
    /// O nome do aparelho que filma (o título da janela).
    pub par: String,
    /// A sessão acabou: a janela de ajustes remota fecha.
    viva: AtomicBool,
}

impl ControleRemoto {
    pub fn novo(par: String) -> Arc<ControleRemoto> {
        Arc::new(ControleRemoto { controlador: Controlador::novo(), par, viva: AtomicBool::new(true) })
    }

    pub fn viva(&self) -> bool {
        self.viva.load(Ordering::SeqCst)
    }

    pub fn encerrar(&self) {
        self.viva.store(false, Ordering::SeqCst);
    }

    /// O estado para a tela (o JSON do núcleo); vazio se o cadeado foi envenenado.
    pub fn estado_json(&self) -> String {
        self.controlador.estado_json().unwrap_or_default()
    }

    /// Um pedido parcial (só de gesto, §5). Recusado na hora pelo núcleo: o diário diz por quê.
    pub fn pedir(&self, json: &str) {
        if let Err(e) = self.controlador.pedir(json) {
            registro::linha(format!("câmera remota: o pedido {json} não saiu: {e}"));
        }
    }

    pub fn restaurar(&self) {
        if let Err(e) = self.controlador.restaurar() {
            registro::linha(format!("câmera remota: o restaurar não saiu: {e}"));
        }
    }
}
