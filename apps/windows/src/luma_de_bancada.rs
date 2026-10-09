//! **A média de luma de bancada** (`docs/controles-de-camera.md` §5, a bandeira `luma_media`).
//!
//! A prova "no fluxo" dos controles do R9: o brilho médio do quadro sobe e desce com o Brilho, o
//! Ganho e o obturador. **Sem salvar nem abrir quadro nenhum** (a câmera de verdade filma a sala):
//! um número por quadro medido, no diário.
//!
//! Como (§5, Windows): um quadro a cada 30, na thread do dono, o `VideoProcessorBlt` do quadro do
//! anel para uma textura BGRA de 64 × 36, a cópia dela para uma textura de leitura, e o `Map` **no
//! quadro seguinte** (a GPU já terminou; o `Map` não espera). A conta é a de
//! `regras_dos_controles::luma_de_bgra`. O custo das duas metades vai junto, em µs.
//!
//! Só existe com `--luma-media`: o produto nunca liga, e paga só a leitura de [`LIGADA`].

#![cfg(windows)]

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use crate::capture::CapturedFrame;
use crate::dono_da_captura::PlacaDoDono;
use crate::registro;
use crate::regras_dos_controles::luma_de_bgra;

/// `--luma-media`: a bancada liga, o produto não.
pub static LIGADA: AtomicBool = AtomicBool::new(false);

/// Um quadro medido a cada tantos.
pub const UM_A_CADA: u64 = 30;
const LARGURA: u32 = 64;
const ALTURA: u32 = 36;

/// A última medida: `(luma, fps desde a medida anterior)`, para as linhas dos ajustes.
static ULTIMA: Mutex<Option<(f64, f64)>> = Mutex::new(None);
static MEDIDAS: AtomicU64 = AtomicU64::new(0);

/// A última medida de luma e o fps medido entre as duas últimas, se a bandeira está ligada.
pub fn ultima() -> Option<(f64, f64)> {
    *ULTIMA.lock().unwrap_or_else(|e| e.into_inner())
}

struct Pecas {
    dispositivo_de_video: ID3D11VideoDevice,
    contexto: ID3D11VideoContext1,
    enumerador: ID3D11VideoProcessorEnumerator,
    processador: ID3D11VideoProcessor,
    saida: ID3D11Texture2D,
    vista_de_saida: ID3D11VideoProcessorOutputView,
    leitura: ID3D11Texture2D,
    entrada: (u32, u32),
}

/// O medidor, pendurado no bombeio do dono.
pub struct MedidorDeLuma {
    faixa_completa: bool,
    matriz_709: bool,
    pecas: Option<Pecas>,
    quadros: u64,
    /// A cópia feita, esperando o `Map` no quadro seguinte: o número do quadro e quando.
    pendente: Option<(u64, Instant)>,
    anterior: Option<(u64, Instant)>,
    custo_blt: (u64, u64, u64),
    custo_map: (u64, u64, u64),
    adiados: u64,
    falhou: bool,
    /// A medida desta vez montou o processador: o custo dela não entra na média.
    montado_agora: bool,
}

impl MedidorDeLuma {
    pub fn novo(faixa_completa: bool, matriz_709: bool) -> MedidorDeLuma {
        registro::linha(format!("luma: a média de luma de bancada está ligada (um quadro a cada {UM_A_CADA}, {LARGURA}x{ALTURA} BGRA)"));
        MedidorDeLuma {
            faixa_completa,
            matriz_709,
            pecas: None,
            quadros: 0,
            pendente: None,
            anterior: None,
            custo_blt: (0, 0, 0),
            custo_map: (0, 0, 0),
            adiados: 0,
            falhou: false,
            montado_agora: false,
        }
    }

    fn montar(&self, placa: &PlacaDoDono, entrada: (u32, u32)) -> windows::core::Result<Pecas> {
        let dispositivo_de_video: ID3D11VideoDevice = placa.device.cast()?;
        let contexto: ID3D11VideoContext1 = unsafe { placa.device.GetImmediateContext()? }.cast()?;
        let d = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
            InputWidth: entrada.0,
            InputHeight: entrada.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
            OutputWidth: LARGURA,
            OutputHeight: ALTURA,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerador = unsafe { dispositivo_de_video.CreateVideoProcessorEnumerator(&d)? };
        let processador = unsafe { dispositivo_de_video.CreateVideoProcessor(&enumerador, 0)? };
        let espaco = match (self.faixa_completa, self.matriz_709) {
            (true, true) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
            (true, false) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601,
            (false, true) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
            (false, false) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
        };
        unsafe {
            contexto.VideoProcessorSetStreamColorSpace1(&processador, 0, espaco);
            contexto.VideoProcessorSetOutputColorSpace1(&processador, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            contexto.VideoProcessorSetStreamAutoProcessingMode(&processador, 0, false);
        }
        let desc = |uso: D3D11_USAGE, bind: u32, cpu: u32| D3D11_TEXTURE2D_DESC {
            Width: LARGURA,
            Height: ALTURA,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: uso,
            BindFlags: bind,
            CPUAccessFlags: cpu,
            MiscFlags: 0,
        };
        let mut saida: Option<ID3D11Texture2D> = None;
        unsafe { placa.device.CreateTexture2D(&desc(D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0), None, Some(&mut saida))? };
        let saida = saida.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))?;
        let mut leitura: Option<ID3D11Texture2D> = None;
        unsafe { placa.device.CreateTexture2D(&desc(D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32), None, Some(&mut leitura))? };
        let leitura = leitura.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))?;
        let desc_saida = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut vista: Option<ID3D11VideoProcessorOutputView> = None;
        unsafe { dispositivo_de_video.CreateVideoProcessorOutputView(&saida, &enumerador, &desc_saida, Some(&mut vista))? };
        let vista_de_saida = vista.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))?;
        Ok(Pecas { dispositivo_de_video, contexto, enumerador, processador, saida, vista_de_saida, leitura, entrada })
    }

    /// O `Blt` do quadro para 64 × 36 e a cópia para a textura de leitura.
    fn medir(&mut self, q: &CapturedFrame, placa: &PlacaDoDono) -> windows::core::Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { q.texture.GetDesc(&mut desc) };
        let entrada = (desc.Width, desc.Height);
        if self.pecas.as_ref().map(|p| p.entrada) != Some(entrada) {
            // A montagem (o processador e as texturas) fica fora do custo por medida: é uma vez.
            let t = Instant::now();
            self.pecas = Some(self.montar(placa, entrada)?);
            registro::linha(format!("luma: o processador {}x{} -> {LARGURA}x{ALTURA} montado em {} us", entrada.0, entrada.1, t.elapsed().as_micros()));
            self.montado_agora = true;
        }
        let Some(p) = self.pecas.as_ref() else { return Ok(()) };
        let desc_entrada = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let mut vista_entrada: Option<ID3D11VideoProcessorInputView> = None;
        unsafe { p.dispositivo_de_video.CreateVideoProcessorInputView(&q.texture, &p.enumerador, &desc_entrada, Some(&mut vista_entrada))? };
        let cheio = RECT { left: 0, top: 0, right: LARGURA as i32, bottom: ALTURA as i32 };
        let origem = RECT { left: 0, top: 0, right: entrada.0 as i32, bottom: entrada.1 as i32 };
        unsafe {
            p.contexto.VideoProcessorSetOutputTargetRect(&p.processador, true, Some(&cheio));
            p.contexto.VideoProcessorSetStreamSourceRect(&p.processador, 0, true, Some(&origem));
            p.contexto.VideoProcessorSetStreamDestRect(&p.processador, 0, true, Some(&cheio));
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
        let r = unsafe { p.contexto.VideoProcessorBlt(&p.processador, &p.vista_de_saida, 0, &fluxos) };
        unsafe { ManuallyDrop::drop(&mut fluxos[0].pInputSurface) };
        r?;
        unsafe {
            placa.context.CopyResource(&p.leitura, &p.saida);
            // A cópia vai para a GPU agora: o `Map` do quadro seguinte a encontra pronta.
            placa.context.Flush();
        }
        Ok(())
    }

    /// O `Map` da textura de leitura (um quadro depois da cópia). `Ok(None)`: a GPU ainda não
    /// terminou, tenta no próximo.
    fn ler(&mut self, placa: &PlacaDoDono) -> windows::core::Result<Option<f64>> {
        let Some(p) = self.pecas.as_ref() else { return Ok(None) };
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        let r = unsafe { placa.context.Map(&p.leitura, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut m)) };
        if let Err(e) = r {
            if e.code() == DXGI_ERROR_WAS_STILL_DRAWING {
                return Ok(None);
            }
            return Err(e);
        }
        let passo = m.RowPitch as usize;
        let bytes = unsafe { std::slice::from_raw_parts(m.pData as *const u8, passo * ALTURA as usize) };
        let luma = luma_de_bgra(bytes, passo, LARGURA as usize, ALTURA as usize);
        unsafe { placa.context.Unmap(&p.leitura, 0) };
        Ok(luma)
    }

    /// Um quadro do anel, na thread do dono.
    pub fn quadro(&mut self, q: &CapturedFrame, placa: &PlacaDoDono) {
        if self.falhou {
            return;
        }
        self.quadros += 1;
        if let Some((n, quando)) = self.pendente {
            let t = Instant::now();
            match self.ler(placa) {
                Ok(Some(luma)) => {
                    let us = t.elapsed().as_micros() as u64;
                    let c = &mut self.custo_map;
                    *c = (c.0 + 1, c.1 + us, c.2.max(us));
                    self.pendente = None;
                    let fps = self.anterior.map(|(n0, t0)| (n - n0) as f64 / quando.duration_since(t0).as_secs_f64().max(1e-6));
                    self.anterior = Some((n, quando));
                    *ULTIMA.lock().unwrap_or_else(|e| e.into_inner()) = Some((luma, fps.unwrap_or(0.0)));
                    let k = MEDIDAS.fetch_add(1, Ordering::Relaxed) + 1;
                    let media = |c: (u64, u64, u64)| if c.0 == 0 { 0.0 } else { c.1 as f64 / c.0 as f64 };
                    registro::linha(format!(
                        "luma: media={luma:.2} quadro={n} fps={} medida={k} custo: blt+copia={:.0}us (max {}us) map={:.0}us (max {}us) adiados={}",
                        fps.map(|f| format!("{f:.2}")).unwrap_or_else(|| "-".into()),
                        media(self.custo_blt),
                        self.custo_blt.2,
                        media(self.custo_map),
                        self.custo_map.2,
                        self.adiados
                    ));
                }
                Ok(None) => self.adiados += 1,
                Err(e) => {
                    registro::linha(format!("luma: !! o Map falhou ({e}); a luma para"));
                    self.falhou = true;
                    return;
                }
            }
        }
        if self.pendente.is_none() && self.quadros % UM_A_CADA == 0 {
            let t = Instant::now();
            match self.medir(q, placa) {
                Ok(()) => {
                    let us = t.elapsed().as_micros() as u64;
                    if !std::mem::take(&mut self.montado_agora) {
                        let c = &mut self.custo_blt;
                        *c = (c.0 + 1, c.1 + us, c.2.max(us));
                    }
                    self.pendente = Some((self.quadros, Instant::now()));
                }
                Err(e) => {
                    registro::linha(format!("luma: !! o Blt falhou ({e}); a luma para"));
                    self.falhou = true;
                }
            }
        }
    }
}
