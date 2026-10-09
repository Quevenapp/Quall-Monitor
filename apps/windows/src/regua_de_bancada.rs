//! **A fonte de bancada com a régua**: o quadro que a câmera de bancada e a câmera sintética
//! entregam na fase 4 de `docs/camera-no-windows.md` (§7.3), e quem serve o cano dela.
//!
//! O cano de uma câmera do Quall (`\\.\pipe\quall-camera-v1-<fnv64 do nome>`) é lido pela fonte de
//! mídia do Quall (`integrations/camera-windows/fonte`), dentro do Frame Server ou dentro do
//! processo. Quando ninguém o serve, a fonte entrega o padrão de bancada dela, sem régua. **Servido
//! por aqui**, ela entrega este quadro:
//!
//! - **a régua de blocos** (`regua::escrever`) no canto superior esquerdo, com o número do quadro:
//!   o receptor com `--regua` confere quadro a quadro que os pixels que saíram do decodificador são
//!   os que entraram na câmera — pixel conferido com conteúdo **nosso**, e o que se registra é um
//!   inteiro, nunca a imagem;
//! - **a barra que anda** 8 px por quadro, como a do padrão da fonte, **só abaixo da régua** (das
//!   linhas `regua::LADO` em diante), para a leitura de volta do anel da sonda
//!   (`quall_camera_local capturar --conferir-a-cada`) continuar valendo;
//! - um fundo em degradê, e o croma neutro.
//!
//! **O conteúdo é todo nosso**: nenhum quadro de câmera de ninguém passa por aqui, e o cano servido
//! é o de uma câmera nossa — a de bancada da sonda, com o nome dela, ou a sintética do app, com um
//! nome aleatório que ninguém serve (conferido antes, `captura_de_camera.rs`).

use crate::regua;

/// Largura e altura do quadro: as do contrato do cano (`quall_camera_fonte::quadros`).
pub const LARGURA: usize = 1920;
pub const ALTURA: usize = 1080;
/// Largura da barra que anda, e quanto ela anda por quadro (os do padrão da fonte).
pub const LARGURA_DA_BARRA: usize = 60;
pub const PASSO_DA_BARRA: usize = 8;

/// O fundo: o plano Y em degradê e o UV neutro, montado uma vez.
pub fn fundo() -> Vec<u8> {
    let mut q = vec![128u8; LARGURA * ALTURA * 3 / 2];
    for y in 0..ALTURA {
        let luz = 16 + (y * 160 / ALTURA);
        for x in 0..LARGURA {
            q[y * LARGURA + x] = (luz + x * 60 / LARGURA).min(234) as u8;
        }
    }
    q
}

/// Onde a barra começa no quadro `n`.
pub fn inicio_da_barra(n: u64) -> usize {
    ((n as usize).wrapping_mul(PASSO_DA_BARRA)) % LARGURA
}

/// **O quadro `n`**, em NV12 `LARGURA`×`ALTURA`, desenhado sobre uma cópia de `fundo`.
pub fn quadro(n: u64, fundo: &[u8]) -> Vec<u8> {
    let mut q = fundo.to_vec();
    let comeco = inicio_da_barra(n);
    let fim = (comeco + LARGURA_DA_BARRA).min(LARGURA);
    for y in regua::LADO..ALTURA {
        q[y * LARGURA + comeco..y * LARGURA + fim].fill(235);
    }
    regua::escrever((n % u64::from(regua::MODULO)) as u32, &mut q[..LARGURA * regua::LADO], LARGURA);
    q
}

// =============================================================================================
// Quem serve o cano (Windows)
// =============================================================================================

#[cfg(windows)]
mod servidor {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::baia::{self, Distribuidor};
    use crate::cano;

    /// **Serve o cano de uma câmera do Quall com o quadro de bancada com régua**, a 30 fps, até
    /// ser solto. Quatro instâncias do cano (as de `baia::subir_cano`: o Frame Server, um app
    /// consumidor e folga) e um fio que publica.
    pub struct FonteDeRegua {
        pub cano: String,
        parar: Arc<AtomicBool>,
        dist: Arc<Distribuidor>,
        fios: Vec<std::thread::JoinHandle<()>>,
        publicados: Arc<AtomicU64>,
        clientes: Arc<AtomicU64>,
        escritos: Arc<AtomicU64>,
    }

    impl FonteDeRegua {
        pub fn servir(cano: String) -> FonteDeRegua {
            let parar = Arc::new(AtomicBool::new(false));
            let dist = Distribuidor::novo();
            let clientes = Arc::new(AtomicU64::new(0));
            let leitores = Arc::new(AtomicU64::new(0));
            let escritos = Arc::new(AtomicU64::new(0));
            let custos = Arc::new(Mutex::new(Vec::new()));
            let alguem_abriu = Arc::new(AtomicBool::new(false));
            let mut fios = baia::subir_cano(
                Arc::clone(&dist),
                Arc::clone(&parar),
                Arc::clone(&clientes),
                leitores,
                Arc::clone(&escritos),
                custos,
                alguem_abriu,
                cano.clone(),
            );
            let publicados = Arc::new(AtomicU64::new(0));
            {
                let (dist, parar, publicados) = (Arc::clone(&dist), Arc::clone(&parar), Arc::clone(&publicados));
                fios.push(std::thread::spawn(move || {
                    let fundo = super::fundo();
                    let comeco = Instant::now();
                    let mut n = 0u64;
                    while !parar.load(Ordering::Relaxed) {
                        dist.publicar(Arc::new(super::quadro(n, &fundo)), cano::qpc_us(), true);
                        publicados.fetch_add(1, Ordering::Relaxed);
                        n += 1;
                        // 30 fps pelo relógio, e não por soneca: o atraso de uma volta não se acumula.
                        let alvo = comeco + Duration::from_micros(n * 33_333);
                        let agora = Instant::now();
                        if alvo > agora {
                            std::thread::sleep(alvo - agora);
                        }
                    }
                }));
            }
            crate::registro::linha(format!("régua de bancada: servindo {cano} a 30 fps"));
            FonteDeRegua { cano, parar, dist, fios, publicados, clientes, escritos }
        }

        /// Publicados, clientes que abriram o cano, quadros escritos no cano.
        pub fn contadores(&self) -> (u64, u64, u64) {
            (
                self.publicados.load(Ordering::Relaxed),
                self.clientes.load(Ordering::Relaxed),
                self.escritos.load(Ordering::Relaxed),
            )
        }
    }

    impl Drop for FonteDeRegua {
        fn drop(&mut self) {
            self.parar.store(true, Ordering::SeqCst);
            self.dist.acordar();
            baia::soltar_o_cano(8, &self.cano);
            for f in self.fios.drain(..) {
                let _ = f.join();
            }
            let (p, c, e) = (
                self.publicados.load(Ordering::Relaxed),
                self.clientes.load(Ordering::Relaxed),
                self.escritos.load(Ordering::Relaxed),
            );
            crate::registro::linha(format!(
                "régua de bancada: solta ({}) — publicados={p} clientes={c} escritos={e}",
                self.cano
            ));
        }
    }
}

#[cfg(windows)]
pub use servidor::FonteDeRegua;

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn o_quadro_leva_a_regua_e_a_barra_abaixo_dela() {
        let f = fundo();
        for n in [0u64, 1, 255, 256, 237, 238, 239, 1000] {
            let q = quadro(n, &f);
            assert_eq!(q.len(), LARGURA * ALTURA * 3 / 2);
            assert_eq!(regua::ler(&q, LARGURA, LARGURA as u32, ALTURA as u32), Some((n % 256) as u32), "n={n}");
            // A barra: 235 a partir do início, só das linhas da régua para baixo.
            let comeco = inicio_da_barra(n);
            let linha = &q[100 * LARGURA..101 * LARGURA];
            let corrida = linha[comeco..].iter().take_while(|&&v| v == 235).count();
            assert_eq!(corrida, LARGURA_DA_BARRA.min(LARGURA - comeco), "n={n}");
            // Acima da régua, nas colunas depois dela, a barra não entra.
            if comeco >= regua::DIGITOS * regua::LADO {
                assert_ne!(q[10 * LARGURA + comeco], 235, "n={n}");
            }
        }
        // O fundo nunca chega a 235: só a barra é 235.
        assert!(f[..LARGURA * ALTURA].iter().all(|&v| v < 235));
        // O croma é neutro.
        assert!(f[LARGURA * ALTURA..].iter().all(|&v| v == 128));
    }
}
