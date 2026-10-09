//! **Uma origem de quadros que não é a tela**: texturas D3D11 nossas, no tamanho que se pedir.
//!
//! # Por que existe
//!
//! Para o emissor com vários receptores ser provado sem capturar a tela de ninguém. A tela do Dell
//! é a máquina de trabalho do usuário, e a regra da casa é medir por contador, nunca pelos pixels
//! dela (`docs/regras-de-frente.md`). E sem `Windows.Graphics.Capture` a cadeia roda na **Sessão
//! 0**, que é onde o SSH cai — o mesmo raciocínio de `bin/quall_gemeos.rs`, que já provou o MFT
//! assim. Até a frente do driver trazer o monitor virtual, é também a implementação provisória do
//! "monitor da sessão" no formato da tela do receptor (`monitor.rs`).
//!
//! # A mesma forma da captura de tela, de propósito
//!
//! A `Cadeia` lê a origem por quatro coisas: um canal de aviso (`frame_ready`), `take_frame`,
//! `espiar_instante` e `item_fechado`. Esta origem expõe as quatro com a mesma semântica de caixa
//! postal de uma posição: o relógio só **avisa** que há quadro novo, e quem pinta é `take_frame`,
//! chamado na thread da cadeia. Pintar noutra thread poria duas threads no contexto imediato do
//! mesmo dispositivo — que é a thread do escalador e do `ProcessInput` —, e o contexto imediato do
//! D3D11 não é seguro entre threads.
//!
//! # O que ela desenha
//!
//! - [`Carga::Cor`]: a tela inteira numa cor que muda a cada quadro (a `quall-gemeos`). Quadro
//!   minúsculo; serve para contar mecanismo com oito sessões sem custo.
//! - [`Carga::Blocos`]: um fundo de blocos de 8 × 8 em cores sorteadas (desenhado uma vez, na CPU,
//!   com um sorteio fixo) e uma faixa de 256 px que anda sobre ele — o caso "uma janela se mexe
//!   sobre uma área de trabalho parada". O IDR tem tamanho de tela de verdade, que é o que o teto
//!   de quadro precisa para ser conferido.
//!
//! **Nenhum quadro daqui é gravado nem aberto por este módulo**; um receptor de bancada pode
//! gravar o `.h264` porque a origem é nossa (`docs/regras-de-frente.md`, "um vídeo de bancada pode
//! conter a vida do usuário" — a exceção é exatamente esta).

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver};
use windows::core::Result;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use crate::capture::CapturedFrame;

/// O que a origem desenha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carga {
    Cor,
    Blocos,
    /// Preto parado: o que a sessão do monitor virtual transmite enquanto o monitor dela nasce na
    /// fila do dono da topologia — o receptor recebe quadro já, e não desiste nos 10 s sem quadro
    /// do Android e do iOS (a revisão de 15/09, item 7). Nada de tela nenhuma.
    Preta,
}

/// Quando a origem produz quadro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ritmo {
    /// Um quadro a cada 1/fps, sempre.
    Movendo,
    /// Um segundo de quadros, e depois nada — a tela parada. É o caso que a repetição a cada
    /// 500 ms existe para cobrir.
    Parada,
    /// `movendo` segundos de quadros, `parada` segundos sem nenhum, e de novo.
    Alterna { movendo: u32, parada: u32 },
}

impl std::str::FromStr for Carga {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "cor" => Ok(Carga::Cor),
            "blocos" => Ok(Carga::Blocos),
            "preta" => Ok(Carga::Preta),
            _ => Err(format!("carga \"{s}\": use cor, blocos ou preta")),
        }
    }
}

impl std::str::FromStr for Ritmo {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        Ritmo::analisar(s).ok_or_else(|| format!("ritmo \"{s}\": use movendo, parada ou alterna:M/P"))
    }
}

impl Ritmo {
    /// Produz quadro neste instante da vida da origem?
    pub fn produz(&self, desde_o_comeco: Duration) -> bool {
        match *self {
            Ritmo::Movendo => true,
            Ritmo::Parada => desde_o_comeco < Duration::from_secs(1),
            Ritmo::Alterna { movendo, parada } => {
                let ciclo = u64::from(movendo.max(1) + parada);
                desde_o_comeco.as_secs() % ciclo < u64::from(movendo.max(1))
            }
        }
    }

    pub fn analisar(texto: &str) -> Option<Ritmo> {
        match texto {
            "movendo" => Some(Ritmo::Movendo),
            "parada" => Some(Ritmo::Parada),
            _ => {
                let resto = texto.strip_prefix("alterna:")?;
                let (m, p) = resto.split_once('/')?;
                Some(Ritmo::Alterna { movendo: m.parse().ok()?, parada: p.parse().ok()? })
            }
        }
    }
}

/// Quantas texturas no anel. O MFT segura a textura submetida até terminar de codificá-la, e
/// repintar a mesma na volta seguinte mudaria por baixo dele o quadro que ele ainda lê.
const TAMANHO_DO_ANEL: usize = 4;

/// Largura da faixa que anda na carga de blocos, e quanto ela anda por quadro.
const FAIXA_PX: u32 = 256;
const PASSO_PX: u32 = 12;

pub struct OrigemSintetica {
    contexto: ID3D11DeviceContext,
    anel: Vec<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    /// O fundo de blocos (só na carga de blocos).
    fundo: Option<ID3D11Texture2D>,
    carga: Carga,
    n: u64,
    /// O relógio avisou e o quadro ainda não foi tirado — a caixa postal de uma posição.
    ha_quadro: Arc<AtomicBool>,
    /// Instante do último aviso, em µs desde `inicio`.
    ultimo_aviso_us: Arc<AtomicU64>,
    chegados: Arc<AtomicU64>,
    inicio: Instant,
    parar: Arc<AtomicBool>,
    relogio: Option<JoinHandle<()>>,
    pub frame_ready: Receiver<()>,
    pub width: u32,
    pub height: u32,
}

impl OrigemSintetica {
    pub fn nova(
        dispositivo: &ID3D11Device,
        largura: u32,
        altura: u32,
        fps: u32,
        carga: Carga,
        ritmo: Ritmo,
    ) -> Result<Self> {
        // Par nos dois lados: é o que o encoder e o `teto` do núcleo esperam de qualquer origem.
        let largura = largura.max(16) & !1;
        let altura = altura.max(16) & !1;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: largura,
            Height: altura,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            // As mesmas bandeiras da textura de destino de `escala.rs`: alvo de desenho, e
            // consumível pelo MFT como superfície DXGI.
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let contexto = unsafe { dispositivo.GetImmediateContext()? };
        let mut anel = Vec::with_capacity(TAMANHO_DO_ANEL);
        for _ in 0..TAMANHO_DO_ANEL {
            let mut t: Option<ID3D11Texture2D> = None;
            unsafe { dispositivo.CreateTexture2D(&desc, None, Some(&mut t))? };
            let t = t.expect("CreateTexture2D não devolveu textura");
            let mut v: Option<ID3D11RenderTargetView> = None;
            unsafe { dispositivo.CreateRenderTargetView(&t, None, Some(&mut v))? };
            anel.push((t, v.expect("CreateRenderTargetView não devolveu vista")));
        }
        let fundo = match carga {
            Carga::Cor | Carga::Preta => None,
            Carga::Blocos => {
                let mut t: Option<ID3D11Texture2D> = None;
                unsafe { dispositivo.CreateTexture2D(&desc, None, Some(&mut t))? };
                let t = t.expect("CreateTexture2D não devolveu textura");
                let bytes = blocos_sorteados(largura, altura);
                unsafe {
                    contexto.UpdateSubresource(&t, 0, None, bytes.as_ptr() as *const _, largura * 4, 0);
                }
                Some(t)
            }
        };

        let (tx, rx) = bounded::<()>(1);
        let ha_quadro = Arc::new(AtomicBool::new(false));
        let ultimo_aviso_us = Arc::new(AtomicU64::new(0));
        let chegados = Arc::new(AtomicU64::new(0));
        let parar = Arc::new(AtomicBool::new(false));
        let inicio = Instant::now();
        let relogio = {
            let (ha_quadro, ultimo, chegados, parar) =
                (ha_quadro.clone(), ultimo_aviso_us.clone(), chegados.clone(), parar.clone());
            let periodo = Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
            std::thread::Builder::new()
                .name("quall.origem-sintetica".into())
                .spawn(move || {
                    // Avança por prazo acumulado, não por intervalo: o `sleep` do Windows acorda no
                    // tique de ~15,6 ms, e reancorar em cada volta daria 32 fps pedindo 30 — ou 21.
                    let mut proximo = Instant::now();
                    while !parar.load(Ordering::Relaxed) {
                        proximo += periodo;
                        let agora = Instant::now();
                        if proximo > agora {
                            std::thread::sleep(proximo - agora);
                        } else if agora - proximo > periodo * 4 {
                            proximo = agora; // atrasou demais (máquina ocupada): sem rajada
                        }
                        if !ritmo.produz(inicio.elapsed()) {
                            continue;
                        }
                        ultimo.store(inicio.elapsed().as_micros() as u64, Ordering::Relaxed);
                        ha_quadro.store(true, Ordering::Release);
                        chegados.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.try_send(());
                    }
                })
                .ok()
        };

        Ok(OrigemSintetica {
            contexto,
            anel,
            fundo,
            carga,
            n: 0,
            ha_quadro,
            ultimo_aviso_us,
            chegados,
            inicio,
            parar,
            relogio,
            frame_ready: rx,
            width: largura,
            height: altura,
        })
    }

    /// Tira o quadro da caixa postal, se houver: pinta a próxima textura do anel **nesta thread**.
    pub fn take_frame(&mut self) -> Option<CapturedFrame> {
        if !self.ha_quadro.swap(false, Ordering::Acquire) {
            return None;
        }
        let i = (self.n as usize) % self.anel.len();
        self.n += 1;
        let (textura, vista) = &self.anel[i];
        unsafe {
            match (self.carga, &self.fundo) {
                (Carga::Blocos, Some(fundo)) => {
                    self.contexto.CopyResource(textura, fundo);
                    // A faixa: um pedaço do próprio fundo, de outro lugar, colado numa posição que
                    // anda. Muda o que o encoder vê numa área só, como uma janela arrastada.
                    let l = self.width;
                    let faixa = FAIXA_PX.min(l / 2).max(16);
                    let x = ((self.n as u32).wrapping_mul(PASSO_PX)) % (l - faixa);
                    let origem_x = (x + l / 2) % (l - faixa);
                    let caixa = D3D11_BOX {
                        left: origem_x,
                        top: 0,
                        front: 0,
                        right: origem_x + faixa,
                        bottom: self.height,
                        back: 1,
                    };
                    self.contexto.CopySubresourceRegion(textura, 0, x, 0, 0, fundo, 0, Some(&caixa));
                }
                (Carga::Preta, _) => {
                    self.contexto.ClearRenderTargetView(vista, &[0.0, 0.0, 0.0, 1.0]);
                }
                _ => {
                    let f = (self.n % 60) as f32 / 60.0;
                    let cor = [f, 1.0 - f, (f * 2.0) % 1.0, 1.0f32];
                    self.contexto.ClearRenderTargetView(vista, &cor);
                }
            }
        }
        let us = self.ultimo_aviso_us.load(Ordering::Relaxed);
        Some(CapturedFrame {
            texture: textura.clone(),
            captured_at: self.inicio + Duration::from_micros(us),
            posse: None,
        })
    }

    /// O instante do quadro na caixa postal, sem tirá-lo.
    pub fn espiar_instante(&self) -> Option<Instant> {
        if self.ha_quadro.load(Ordering::Acquire) {
            Some(self.inicio + Duration::from_micros(self.ultimo_aviso_us.load(Ordering::Relaxed)))
        } else {
            None
        }
    }

    /// Quantos avisos o relógio deu.
    pub fn chegados(&self) -> u64 {
        self.chegados.load(Ordering::Relaxed)
    }

    pub fn parar(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(t) = self.relogio.take() {
            let _ = t.join();
        }
    }
}

impl Drop for OrigemSintetica {
    fn drop(&mut self) {
        self.parar();
    }
}

/// O fundo da carga de blocos: blocos de 8 × 8 em cores sorteadas por um xorshift de semente fixa
/// — o mesmo fundo em toda corrida, para duas corridas serem comparáveis.
fn blocos_sorteados(largura: u32, altura: u32) -> Vec<u8> {
    let mut estado: u32 = 0x9E37_79B9;
    let mut sortear = || {
        estado ^= estado << 13;
        estado ^= estado >> 17;
        estado ^= estado << 5;
        estado
    };
    let (bl, ba) = (largura.div_ceil(8) as usize, altura.div_ceil(8) as usize);
    let cores: Vec<u32> = (0..bl * ba).map(|_| sortear() | 0xFF00_0000).collect();
    let mut bytes = vec![0u8; (largura * altura * 4) as usize];
    for y in 0..altura as usize {
        for x in 0..largura as usize {
            let c = cores[(y / 8) * bl + x / 8];
            let i = (y * largura as usize + x) * 4;
            bytes[i..i + 4].copy_from_slice(&c.to_le_bytes());
        }
    }
    bytes
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn ritmo_parada_so_produz_no_primeiro_segundo() {
        assert!(Ritmo::Parada.produz(Duration::from_millis(900)));
        assert!(!Ritmo::Parada.produz(Duration::from_millis(1100)));
        assert!(!Ritmo::Parada.produz(Duration::from_secs(60)));
    }

    #[test]
    fn ritmo_alterna_segue_o_ciclo() {
        let r = Ritmo::analisar("alterna:3/2").unwrap();
        assert_eq!(r, Ritmo::Alterna { movendo: 3, parada: 2 });
        let produz: Vec<bool> = (0..10).map(|s| r.produz(Duration::from_secs(s))).collect();
        assert_eq!(produz, [true, true, true, false, false, true, true, true, false, false]);
        assert_eq!(Ritmo::analisar("movendo"), Some(Ritmo::Movendo));
        assert_eq!(Ritmo::analisar("alterna:x/2"), None);
    }

    #[test]
    fn o_fundo_de_blocos_e_o_mesmo_em_toda_corrida() {
        let a = blocos_sorteados(64, 32);
        let b = blocos_sorteados(64, 32);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64 * 32 * 4);
        // Dentro de um bloco de 8 × 8 a cor é uma só.
        assert_eq!(a[0..4], a[4 * 7..4 * 8]);
        assert_ne!(a[0..4], a[4 * 8..4 * 9]);
    }
}
