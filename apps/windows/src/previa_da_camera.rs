//! **A prévia da tela R5** (`docs/teleprompter-com-camera.md` §2.5, §6 e §8.10, peça 7): o quadro do
//! anel do dono numa janela filha, por swap chain e `VideoProcessorBlt`, **na thread do dono**.
//!
//! - **Sem cópia**: o `Blt` lê a posição do anel no mesmo contexto imediato em que o `take_frame`
//!   acabou de escrevê-la, e nesta mesma thread (a regra dos três leitores, `dono_da_captura.rs`).
//! - **O espelho é da prévia**: `VideoProcessorSetStreamMirror` na hora do `Blt` (ajuste local,
//!   ligado por padrão). O anel, a rede e o arquivo nunca são espelhados (§6).
//! - **Encaixada**: o quadro inteiro, com faixas pretas, como a rede o encaixa (a lição do Android,
//!   §8.6, defeito 1).
//! - **Nunca bloqueia o dono**: `Present(0, DXGI_PRESENT_DO_NOT_WAIT)`; com a fila cheia o quadro da
//!   prévia é pulado e contado, e a rede e o gravador seguem.
//! - **A janela é da thread da tela**; a swap chain, desta. A tela nunca espera o dono sem bombear
//!   mensagens (a DXGI pode mandar mensagem à janela no `Present` e no `ResizeBuffers`).
//! - **Na Sessão 0** a swap chain não nasce (`0x887A0022`, medido pela frente do receptor): a prévia
//!   diz uma vez e para de tentar; a câmera, a rede e o gravador não dependem dela.

#![cfg(windows)]

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::capture::CapturedFrame;
use crate::dono_da_captura::{PlacaDoDono, SaidaDoDono};
use crate::registro;

/// O que a tela controla e lê da prévia, sem tocar na swap chain.
#[derive(Default)]
pub struct ControleDaPrevia {
    /// Esconder a prévia **não fecha nada**: só para o `Blt` (a janela filha some na tela).
    pub visivel: AtomicBool,
    /// "Prévia como espelho" (ajuste local, ligado por padrão).
    pub espelho: AtomicBool,
    /// O tamanho do cliente da janela filha: `largura << 32 | altura`.
    pub tamanho: AtomicU64,
    pub desenhados: AtomicU64,
    pub pulados: AtomicU64,
    pub falha: Mutex<Option<String>>,
}

impl ControleDaPrevia {
    pub fn novo(espelho: bool) -> Arc<ControleDaPrevia> {
        let c = ControleDaPrevia::default();
        c.visivel.store(true, Ordering::SeqCst);
        c.espelho.store(espelho, Ordering::SeqCst);
        Arc::new(c)
    }

    pub fn definir_tamanho(&self, largura: u32, altura: u32) {
        self.tamanho.store((u64::from(largura) << 32) | u64::from(altura), Ordering::SeqCst);
    }

    fn tamanho(&self) -> (u32, u32) {
        let v = self.tamanho.load(Ordering::SeqCst);
        ((v >> 32) as u32, v as u32)
    }
}

struct Processador {
    dispositivo_de_video: ID3D11VideoDevice,
    contexto: ID3D11VideoContext1,
    enumerador: ID3D11VideoProcessorEnumerator,
    processador: ID3D11VideoProcessor,
    entrada: (u32, u32),
    saida: (u32, u32),
    espelho: Option<bool>,
}

/// A prévia, pendurada no dono.
pub struct PreviaDaCamera {
    hwnd: isize,
    controle: Arc<ControleDaPrevia>,
    faixa_completa: bool,
    matriz_709: bool,
    swapchain: Option<IDXGISwapChain1>,
    tamanho_atual: (u32, u32),
    vp: Option<Processador>,
    parou: bool,
    erros: u64,
}

// SAFETY: a swap chain e o processador são criados, usados e soltos só na thread do dono (a prévia
// é pendurada vazia, e tudo nasce no primeiro `quadro`). O `HWND` é um número.
unsafe impl Send for PreviaDaCamera {}

impl PreviaDaCamera {
    /// A prévia da janela filha `hwnd`. Nada de DXGI aqui: a swap chain nasce no primeiro quadro, na
    /// thread do dono.
    pub fn nova(hwnd: HWND, controle: Arc<ControleDaPrevia>, faixa_completa: bool, matriz_709: bool) -> PreviaDaCamera {
        PreviaDaCamera {
            hwnd: hwnd.0 as isize,
            controle,
            faixa_completa,
            matriz_709,
            swapchain: None,
            tamanho_atual: (0, 0),
            vp: None,
            parou: false,
            erros: 0,
        }
    }

    fn parar(&mut self, motivo: String) {
        registro::linha(format!("r5 prévia: !! {motivo} — a prévia para (a câmera, a rede e a gravação seguem)"));
        *self.controle.falha.lock().unwrap_or_else(|e| e.into_inner()) = Some(motivo);
        self.parou = true;
        self.vp = None;
        self.swapchain = None;
    }

    fn desenhar(&mut self, q: &CapturedFrame, placa: &PlacaDoDono) -> windows::core::Result<()> {
        let (w, h) = self.controle.tamanho();
        if w < 2 || h < 2 {
            return Ok(());
        }
        if self.swapchain.is_none() {
            let dxgi: IDXGIDevice = placa.device.cast()?;
            let adaptador = unsafe { dxgi.GetAdapter()? };
            let fabrica: IDXGIFactory2 = unsafe { adaptador.GetParent()? };
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                ..Default::default()
            };
            let sc = unsafe { fabrica.CreateSwapChainForHwnd(&placa.device, HWND(self.hwnd as *mut core::ffi::c_void), &desc, None, None)? };
            registro::linha(format!("r5 prévia: swap chain {w}x{h} na placa \"{}\"", placa.descricao));
            self.swapchain = Some(sc);
            self.tamanho_atual = (w, h);
        }
        let Some(sc) = self.swapchain.clone() else { return Ok(()) };
        if self.tamanho_atual != (w, h) {
            unsafe { sc.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))? };
            self.tamanho_atual = (w, h);
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { q.texture.GetDesc(&mut desc) };
        let entrada = (desc.Width, desc.Height);
        if self.vp.as_ref().map(|v| (v.entrada, v.saida)) != Some((entrada, (w, h))) {
            self.vp = Some(self.processador(placa, entrada, (w, h))?);
        }
        let espelho = self.controle.espelho.load(Ordering::SeqCst);
        let Some(vp) = self.vp.as_mut() else { return Ok(()) };
        if vp.espelho != Some(espelho) {
            unsafe { vp.contexto.VideoProcessorSetStreamMirror(&vp.processador, 0, espelho, true, false) };
            vp.espelho = Some(espelho);
            registro::linha(format!("r5 prévia: espelho aplicado: {}", if espelho { "ligado (como espelho)" } else { "desligado" }));
        }
        let fundo: ID3D11Texture2D = unsafe { sc.GetBuffer(0)? };
        let desc_saida = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut vista_saida: Option<ID3D11VideoProcessorOutputView> = None;
        unsafe { vp.dispositivo_de_video.CreateVideoProcessorOutputView(&fundo, &vp.enumerador, &desc_saida, Some(&mut vista_saida))? };
        let desc_entrada = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut vista_entrada: Option<ID3D11VideoProcessorInputView> = None;
        unsafe { vp.dispositivo_de_video.CreateVideoProcessorInputView(&q.texture, &vp.enumerador, &desc_entrada, Some(&mut vista_entrada))? };
        let alvo = encaixe(entrada, (w, h));
        let cheio = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        let origem = RECT { left: 0, top: 0, right: entrada.0 as i32, bottom: entrada.1 as i32 };
        unsafe {
            vp.contexto.VideoProcessorSetOutputTargetRect(&vp.processador, true, Some(&cheio));
            vp.contexto.VideoProcessorSetStreamSourceRect(&vp.processador, 0, true, Some(&origem));
            vp.contexto.VideoProcessorSetStreamDestRect(&vp.processador, 0, true, Some(&alvo));
        }
        let fluxo = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL::from(true),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: std::ptr::null_mut(),
            pInputSurface: ManuallyDrop::new(vista_entrada),
            ppFutureSurfaces: std::ptr::null_mut(),
            ppPastSurfacesRight: std::ptr::null_mut(),
            pInputSurfaceRight: ManuallyDrop::new(None),
            ppFutureSurfacesRight: std::ptr::null_mut(),
        };
        let mut fluxos = [fluxo];
        let r = unsafe { vp.contexto.VideoProcessorBlt(&vp.processador, vista_saida.as_ref(), 0, &fluxos) };
        unsafe { ManuallyDrop::drop(&mut fluxos[0].pInputSurface) };
        r?;
        let hr = unsafe { sc.Present(0, DXGI_PRESENT_DO_NOT_WAIT) };
        if hr == DXGI_ERROR_WAS_STILL_DRAWING {
            self.controle.pulados.fetch_add(1, Ordering::Relaxed);
        } else {
            hr.ok()?;
            self.controle.desenhados.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn processador(&self, placa: &PlacaDoDono, entrada: (u32, u32), saida: (u32, u32)) -> windows::core::Result<Processador> {
        let dispositivo_de_video: ID3D11VideoDevice = placa.device.cast()?;
        let contexto: ID3D11VideoContext1 = unsafe { placa.device.GetImmediateContext()? }.cast()?;
        let d = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
            InputWidth: entrada.0,
            InputHeight: entrada.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
            OutputWidth: saida.0,
            OutputHeight: saida.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerador = unsafe { dispositivo_de_video.CreateVideoProcessorEnumerator(&d)? };
        let processador = unsafe { dispositivo_de_video.CreateVideoProcessor(&enumerador, 0)? };
        let espaco_de_entrada = match (self.faixa_completa, self.matriz_709) {
            (true, true) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
            (true, false) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601,
            (false, true) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            (false, false) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
        };
        unsafe {
            contexto.VideoProcessorSetStreamColorSpace1(&processador, 0, espaco_de_entrada);
            contexto.VideoProcessorSetOutputColorSpace1(&processador, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            contexto.VideoProcessorSetStreamAutoProcessingMode(&processador, 0, false);
            let preto = D3D11_VIDEO_COLOR { Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } } };
            contexto.VideoProcessorSetOutputBackgroundColor(&processador, false, &preto);
        }
        Ok(Processador { dispositivo_de_video, contexto, enumerador, processador, entrada, saida, espelho: None })
    }
}

/// O quadro `entrada` inteiro dentro de `saida`, centrado, sem deformar (a mesma conta de
/// `teleprompter::divisao::encaixar`, aqui em `RECT`).
fn encaixe(entrada: (u32, u32), saida: (u32, u32)) -> RECT {
    let (w, h) = (f64::from(saida.0.max(1)), f64::from(saida.1.max(1)));
    if entrada.0 == 0 || entrada.1 == 0 {
        return RECT { left: 0, top: 0, right: saida.0 as i32, bottom: saida.1 as i32 };
    }
    let a = f64::from(entrada.0) / f64::from(entrada.1);
    let (ew, eh) = if w / h > a { (h * a, h) } else { (w, w / a) };
    let x = ((w - ew) / 2.0).round() as i32;
    let y = ((h - eh) / 2.0).round() as i32;
    RECT { left: x, top: y, right: x + ew.round() as i32, bottom: y + eh.round() as i32 }
}

impl SaidaDoDono for PreviaDaCamera {
    fn quadro(&mut self, q: &CapturedFrame, placa: &PlacaDoDono) {
        if self.parou || !self.controle.visivel.load(Ordering::SeqCst) {
            return;
        }
        if let Err(e) = self.desenhar(q, placa) {
            self.erros += 1;
            // A swap chain que não nasce (a Sessão 0) ou uma janela que sumiu: para de vez.
            if self.swapchain.is_none() || self.erros >= 30 {
                self.parar(format!("a prévia não desenha: {e}"));
            } else if self.erros <= 3 {
                registro::linha(format!("r5 prévia: um quadro não foi desenhado: {e}"));
                // O processador é refeito no próximo quadro (a janela pode ter mudado de tamanho).
                self.vp = None;
            }
        }
    }

    fn nome(&self) -> &'static str {
        "prévia"
    }

    fn relato(&mut self) -> Option<String> {
        Some(format!(
            "desenhados={} pulados={} visivel={} espelho={} erros={}{}",
            self.controle.desenhados.load(Ordering::Relaxed),
            self.controle.pulados.load(Ordering::Relaxed),
            self.controle.visivel.load(Ordering::Relaxed),
            self.controle.espelho.load(Ordering::Relaxed),
            self.erros,
            if self.parou { " (parada)" } else { "" }
        ))
    }
}
