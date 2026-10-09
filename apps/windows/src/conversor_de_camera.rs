//! O quadro da câmera levado ao que o encoder come: **NV12 de faixa limitada, no tamanho do teto**,
//! na GPU (`docs/camera-no-windows.md` §3.3 e §3.4).
//!
//! Só entra no caminho quando precisa (`regras_da_camera::precisa_do_processador`): para escalar
//! (sensor acima do teto, ou 720p pedido), para converter o YUY2 em NV12, ou para levar a faixa
//! completa (o que sai do decodificador MJPEG) à faixa limitada do contrato — **mesmo sem escala**.
//! Uma câmera NV12 de faixa limitada no tamanho do teto (a fonte do Quall, a webcam do Dell de
//! agosto) vai direto para o encoder, sem passar por aqui.
//!
//! # As três diferenças para `escala.rs`
//!
//! - **A cor é declarada nos dois lados** (a revisão, M6): `VideoProcessorSetStreamColorSpace1` na
//!   entrada e `VideoProcessorSetOutputColorSpace1` na saída. Declarar só a saída não converte nada.
//!   A matriz **não** é trocada (a saída declara a mesma da entrada); só a faixa. O driver é
//!   consultado (`CheckVideoProcessorFormatConversion`), e a resposta vai para o registro.
//! - **Um anel de destinos**, e não um só (a revisão, m5): o quadro seguinte não pode escrever no
//!   destino que o MFT assíncrono ainda está lendo.
//! - **A vista de entrada é solta depois do `Blt`**. `escala.rs` a embrulha num `ManuallyDrop` que
//!   ninguém solta; aqui ela sai com o quadro.
//!
//! # Pixel quadrado (a fase 5, o DV em 16:9)
//!
//! O DV-SD de 720×480 é anamórfico: em 16:9 o pixel é mais largo que alto (32:27), e o receptor
//! que mostra 720×480 como pixel quadrado comprime a imagem (o Bruno viu no S24). A captura decide a
//! razão de pixel (`regras_da_camera::aspecto_da_camera`) e a cadeia pede ao conversor a saída
//! **em pixel quadrado** (854×480 em 16:9, 640×480 em 4:3): vale para todo receptor, sem depender de
//! ninguém ler o `aspect_ratio_idc` do SPS. Se o aspecto muda com a sessão no ar, o tamanho do
//! encoder fica, e o quadro entra encaixado com faixas pretas (`encaixar`), do fundo em RGB (0, 0, 0)
//! (`Fundo::PretoRgb`).
//!
//! # Desentrelaçar (a fase 5, o DV da Panasonic)
//!
//! Com um quadro entrelaçado (`regras_da_camera::Entrelacamento`), o conteúdo e o fluxo são
//! declarados com os campos e a ordem deles (`InputFrameFormat`, `VideoProcessorSetStreamFrameFormat`)
//! e o processador é criado com o índice de conversão de taxa que desentrelaça **sem quadros de
//! referência** (o `bob`, que o anel não precisa guardar): um quadro sai por quadro que entra, do
//! primeiro campo no tempo. As capacidades de cada índice vão para o registro. O leitor não
//! desentrelaça: o processador avançado dele recarimba (M9).
//!
//! **Nada aqui lê um pixel.** Entra textura, sai textura.

#![cfg(windows)]

use std::cell::Cell;
use std::mem::ManuallyDrop;

use windows::core::{Interface, Result, BOOL};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_TYPE, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_FORMAT,
    DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};

use crate::regras_da_camera::Entrelacamento;

/// **O fundo das faixas** do quadro encaixado (a troca de aspecto no meio da sessão).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fundo {
    /// RGB (0, 0, 0) com `YCbCr = false`: o preto, que o processador leva ao espaço da saída (Y=16
    /// na faixa limitada). É o do produto (a revisão curta do `08af2cd`, A6).
    PretoRgb,
    /// O da primeira versão: YCbCr (16, 128, 128)/255 com `YCbCr = true`. A documentação não diz se
    /// o valor normalizado é o código (Y=16) ou a posição na faixa nominal (16/255 da faixa
    /// limitada, Y≈30); o Bruno viu as faixas pretas, mas o Y não foi medido. Fica para o `anel`
    /// medir como controle.
    YCbCrLimitado,
}

fn pintar_fundo(contexto: &ID3D11VideoContext1, processador: &ID3D11VideoProcessor, fundo: Fundo) {
    let (ycbcr, cor) = match fundo {
        Fundo::PretoRgb => (false, D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } }),
        Fundo::YCbCrLimitado => (
            true,
            D3D11_VIDEO_COLOR_0 { YCbCr: D3D11_VIDEO_COLOR_YCbCrA { Y: 16.0 / 255.0, Cb: 128.0 / 255.0, Cr: 128.0 / 255.0, A: 1.0 } },
        ),
    };
    unsafe { contexto.VideoProcessorSetOutputBackgroundColor(processador, ycbcr, &D3D11_VIDEO_COLOR { Anonymous: cor }) };
}

/// Quantos destinos o anel tem. A conversão acontece **na submissão** (`transmissao.rs`), então os
/// destinos são todos do MFT: a cadeia não converte com `DESTINOS − 1` quadros em voo (a revisão do
/// código da fase 3, m6), e conta quantas vezes esperou.
const DESTINOS: usize = 4;

/// O formato de quadro do processador para um entrelaçamento.
fn formato_de_quadro(e: Entrelacamento) -> D3D11_VIDEO_FRAME_FORMAT {
    match e {
        Entrelacamento::Progressivo => D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        Entrelacamento::CampoDeCimaPrimeiro => D3D11_VIDEO_FRAME_FORMAT_INTERLACED_TOP_FIELD_FIRST,
        Entrelacamento::CampoDeBaixoPrimeiro => D3D11_VIDEO_FRAME_FORMAT_INTERLACED_BOTTOM_FIELD_FIRST,
    }
}

/// Os nomes das capacidades de desentrelaçar de um índice de conversão de taxa.
fn capacidades(c: u32) -> String {
    let nomes = [
        (D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_BLEND.0 as u32, "blend"),
        (D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_BOB.0 as u32, "bob"),
        (D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_ADAPTIVE.0 as u32, "adaptive"),
        (D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_MOTION_COMPENSATION.0 as u32, "motion"),
        (D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_INVERSE_TELECINE.0 as u32, "ivtc"),
        (D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_FRAME_RATE_CONVERSION.0 as u32, "fps"),
    ];
    let v: Vec<&str> = nomes.iter().filter(|(b, _)| c & b != 0).map(|(_, n)| *n).collect();
    if v.is_empty() {
        "nenhuma".into()
    } else {
        v.join("+")
    }
}

/// O índice de conversão de taxa para desentrelaçar: o primeiro que faz `bob` sem pedir quadros de
/// referência; senão, o primeiro que faz `bob`; senão, o primeiro que desentrelaça de algum jeito.
/// Devolve o índice e a descrição de todos, para o registro.
unsafe fn indice_para_desentrelacar(enumerador: &ID3D11VideoProcessorEnumerator) -> Result<(u32, String)> {
    let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
    unsafe { enumerador.GetVideoProcessorCaps(&mut caps)? };
    let bob = D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_BOB.0 as u32;
    let qualquer = (D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_BLEND.0
        | D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_BOB.0
        | D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_ADAPTIVE.0
        | D3D11_VIDEO_PROCESSOR_PROCESSOR_CAPS_DEINTERLACE_MOTION_COMPENSATION.0) as u32;
    let mut lista = Vec::new();
    for i in 0..caps.RateConversionCapsCount {
        let mut r = D3D11_VIDEO_PROCESSOR_RATE_CONVERSION_CAPS::default();
        if unsafe { enumerador.GetVideoProcessorRateConversionCaps(i, &mut r) }.is_ok() {
            lista.push((i, r));
        }
    }
    let texto = lista
        .iter()
        .map(|(i, r)| format!("[{i}] {} passados={} futuros={}", capacidades(r.ProcessorCaps), r.PastFrames, r.FutureFrames))
        .collect::<Vec<_>>()
        .join(", ");
    let escolhido = lista
        .iter()
        .find(|(_, r)| r.ProcessorCaps & bob != 0 && r.PastFrames == 0 && r.FutureFrames == 0)
        .or_else(|| lista.iter().find(|(_, r)| r.ProcessorCaps & bob != 0))
        .or_else(|| lista.iter().find(|(_, r)| r.ProcessorCaps & qualquer != 0));
    match escolhido {
        Some((i, _)) => Ok((*i, texto)),
        None => Err(windows::core::Error::new(
            windows::Win32::Foundation::E_NOTIMPL,
            format!("nenhum índice de conversão de taxa desta placa desentrelaça ({texto})"),
        )),
    }
}

/// O espaço de cor de um YCbCr: faixa e matriz.
fn espaco(completa: bool, matriz_709: bool) -> DXGI_COLOR_SPACE_TYPE {
    match (completa, matriz_709) {
        (true, true) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
        (true, false) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601,
        (false, true) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        (false, false) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
    }
}

pub struct ConversorDeCamera {
    contexto: ID3D11VideoContext1,
    processador: ID3D11VideoProcessor,
    enumerador: ID3D11VideoProcessorEnumerator,
    dispositivo_de_video: ID3D11VideoDevice,
    destinos: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    proximo: Cell<usize>,
    pub largura: u32,
    pub altura: u32,
    /// Para o registro: o que entra, o que sai, e o que o driver disse da conversão de cor.
    pub descricao: String,
}

impl ConversorDeCamera {
    #[allow(clippy::too_many_arguments)]
    pub fn novo(
        dispositivo: &ID3D11Device,
        formato_de_entrada: DXGI_FORMAT,
        entrada_largura: u32,
        entrada_altura: u32,
        saida_largura: u32,
        saida_altura: u32,
        entrada_completa: bool,
        matriz_709: bool,
        entrelacamento: Entrelacamento,
    ) -> Result<Self> {
        unsafe {
            let dispositivo_de_video: ID3D11VideoDevice = dispositivo.cast()?;
            let contexto: ID3D11VideoContext1 = dispositivo.GetImmediateContext()?.cast()?;
            let descricao = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: formato_de_quadro(entrelacamento),
                InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                InputWidth: entrada_largura,
                InputHeight: entrada_altura,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 },
                OutputWidth: saida_largura,
                OutputHeight: saida_altura,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerador = dispositivo_de_video.CreateVideoProcessorEnumerator(&descricao)?;
            let entrada_ok = enumerador.CheckVideoProcessorFormat(formato_de_entrada)?
                & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32
                != 0;
            let saida_ok = enumerador.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?
                & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32
                != 0;
            if !entrada_ok || !saida_ok {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_NOTIMPL,
                    format!(
                        "o processador de vídeo desta placa não aceita {formato_de_entrada:?} na entrada ({entrada_ok}) ou NV12 na saída ({saida_ok})"
                    ),
                ));
            }
            let espaco_de_entrada = espaco(entrada_completa, matriz_709);
            let espaco_de_saida = espaco(false, matriz_709);
            let conversao = match enumerador.cast::<ID3D11VideoProcessorEnumerator1>() {
                Ok(e1) => match e1.CheckVideoProcessorFormatConversion(
                    formato_de_entrada,
                    espaco_de_entrada,
                    DXGI_FORMAT_NV12,
                    espaco_de_saida,
                ) {
                    Ok(b) if b.as_bool() => "o driver confirma a conversão".to_string(),
                    Ok(_) => "o driver NÃO confirma a conversão de cor pedida: a faixa pode sair errada".to_string(),
                    Err(e) => format!("o driver não respondeu sobre a conversão ({e})"),
                },
                Err(_) => "sem ID3D11VideoProcessorEnumerator1: a conversão de cor não foi consultada".to_string(),
            };
            let (indice, desentrelaca) = if entrelacamento == Entrelacamento::Progressivo {
                (0, String::new())
            } else {
                match indice_para_desentrelacar(&enumerador) {
                    Ok((i, todos)) => {
                        (i, format!("; desentrelaça {entrelacamento:?} pelo índice {i} (os da placa: {todos}), sem quadros de referência"))
                    }
                    // **A placa que não desentrelaça não derruba a sessão** (a revisão do código da
                    // fase 5, L4): o conversor é refeito progressivo, e o quadro segue com os dois
                    // campos, como antes da fase 5, com o motivo no registro.
                    Err(e) => {
                        let mut c = Self::novo(
                            dispositivo,
                            formato_de_entrada,
                            entrada_largura,
                            entrada_altura,
                            saida_largura,
                            saida_altura,
                            entrada_completa,
                            matriz_709,
                            Entrelacamento::Progressivo,
                        )?;
                        c.descricao.push_str(&format!(
                            "; NÃO desentrelaça {entrelacamento:?} ({e}): o quadro segue com os dois campos, e a sessão não cai"
                        ));
                        return Ok(c);
                    }
                }
            };
            let processador = dispositivo_de_video.CreateVideoProcessor(&enumerador, indice)?;
            contexto.VideoProcessorSetStreamFrameFormat(&processador, 0, formato_de_quadro(entrelacamento));
            contexto.VideoProcessorSetStreamColorSpace1(&processador, 0, espaco_de_entrada);
            contexto.VideoProcessorSetOutputColorSpace1(&processador, espaco_de_saida);
            // Sem auto-processamento: nitidez e redução de ruído do driver não têm o que fazer aqui.
            contexto.VideoProcessorSetStreamAutoProcessingMode(&processador, 0, false);
            let origem = RECT { left: 0, top: 0, right: entrada_largura as i32, bottom: entrada_altura as i32 };
            let alvo = RECT { left: 0, top: 0, right: saida_largura as i32, bottom: saida_altura as i32 };
            contexto.VideoProcessorSetStreamSourceRect(&processador, 0, true, Some(&origem));
            contexto.VideoProcessorSetStreamDestRect(&processador, 0, true, Some(&alvo));
            contexto.VideoProcessorSetOutputTargetRect(&processador, true, Some(&alvo));
            // O fundo é o que aparece nas faixas quando o aspecto muda no meio da sessão e o quadro
            // entra encaixado (`encaixar`): o preto em RGB (a revisão curta do `08af2cd`, A6).
            pintar_fundo(&contexto, &processador, Fundo::PretoRgb);

            let mut destinos = Vec::with_capacity(DESTINOS);
            for _ in 0..DESTINOS {
                let destino = textura_nv12_de_saida(dispositivo, saida_largura, saida_altura)?;
                let desc_vista = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut vista: Option<ID3D11VideoProcessorOutputView> = None;
                dispositivo_de_video.CreateVideoProcessorOutputView(&destino, &enumerador, &desc_vista, Some(&mut vista))?;
                let vista = vista.ok_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_POINTER, "sem vista de saída"))?;
                destinos.push((destino, vista));
            }
            let descricao = format!(
                "{formato_de_entrada:?} {entrada_largura}x{entrada_altura} faixa {} matriz {} → NV12 {saida_largura}x{saida_altura} faixa limitada, {DESTINOS} destinos; {conversao}{desentrelaca}",
                if entrada_completa { "completa" } else { "limitada" },
                if matriz_709 { "BT.709" } else { "BT.601" },
            );
            Ok(ConversorDeCamera {
                contexto,
                processador,
                enumerador,
                dispositivo_de_video,
                destinos,
                proximo: Cell::new(0),
                largura: saida_largura,
                altura: saida_altura,
                descricao,
            })
        }
    }

    /// Quantos destinos o anel tem: a cadeia não converte com `destinos() − 1` quadros no MFT.
    pub fn destinos(&self) -> usize {
        self.destinos.len()
    }

    /// **O aspecto mudou no meio da sessão** (a fase 5: o DV da Panasonic em 16:9 ↔ 4:3): o
    /// encoder tem o tamanho da abertura, e o quadro passa a entrar **encaixado** nele, sem
    /// deformar, com faixas pretas (`regras_da_camera::encaixe`). Devolve o retângulo.
    pub fn encaixar(&self, exibida: (u32, u32)) -> (u32, u32, u32, u32) {
        let (x, y, l, a) = crate::regras_da_camera::encaixe((self.largura, self.altura), exibida);
        let r = RECT { left: x as i32, top: y as i32, right: (x + l) as i32, bottom: (y + a) as i32 };
        unsafe { self.contexto.VideoProcessorSetStreamDestRect(&self.processador, 0, true, Some(&r)) };
        (x, y, l, a)
    }

    /// Troca o fundo das faixas. O produto usa [`Fundo::PretoRgb`] desde a criação; o `anel` mede o
    /// outro como controle.
    pub fn pintar_fundo(&self, fundo: Fundo) {
        pintar_fundo(&self.contexto, &self.processador, fundo);
    }

    /// **Bancada** (a sonda `desentrelacar --modo gpu-referencias`): o mesmo `Blt`, com quadros de
    /// referência passados e futuros entregues ao processador (o índice da Intel declara
    /// `adaptive+motion` com 1 passado e 1 futuro). O produto não usa: com o futuro, a saída atrasa um
    /// quadro. A ordem de `passados` é do mais novo para o mais velho.
    pub fn converter_com_referencias(
        &self,
        origem: &ID3D11Texture2D,
        passados: &[&ID3D11Texture2D],
        futuros: &[&ID3D11Texture2D],
        quadro: u32,
    ) -> Result<ID3D11Texture2D> {
        let i = self.proximo.get();
        self.proximo.set((i + 1) % self.destinos.len());
        let (destino, vista_de_saida) = &self.destinos[i];
        let vista = |t: &ID3D11Texture2D| -> Result<Option<ID3D11VideoProcessorInputView>> {
            let desc_entrada = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut v: Option<ID3D11VideoProcessorInputView> = None;
            unsafe { self.dispositivo_de_video.CreateVideoProcessorInputView(t, &self.enumerador, &desc_entrada, Some(&mut v))? };
            Ok(v)
        };
        let mut vp: Vec<Option<ID3D11VideoProcessorInputView>> = passados.iter().map(|t| vista(t)).collect::<Result<_>>()?;
        let mut vf: Vec<Option<ID3D11VideoProcessorInputView>> = futuros.iter().map(|t| vista(t)).collect::<Result<_>>()?;
        let atual = vista(origem)?;
        unsafe {
            let fluxo = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL::from(true),
                OutputIndex: 0,
                InputFrameOrField: quadro,
                PastFrames: vp.len() as u32,
                FutureFrames: vf.len() as u32,
                ppPastSurfaces: if vp.is_empty() { std::ptr::null_mut() } else { vp.as_mut_ptr() },
                pInputSurface: ManuallyDrop::new(atual),
                ppFutureSurfaces: if vf.is_empty() { std::ptr::null_mut() } else { vf.as_mut_ptr() },
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let mut fluxos = [fluxo];
            let r = self.contexto.VideoProcessorBlt(&self.processador, vista_de_saida, quadro, &fluxos);
            ManuallyDrop::drop(&mut fluxos[0].pInputSurface);
            r?;
        }
        drop(vp);
        drop(vf);
        Ok(destino.clone())
    }

    /// Converte um quadro para o **próximo** destino do anel e o devolve.
    pub fn converter(&self, origem: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let i = self.proximo.get();
        self.proximo.set((i + 1) % self.destinos.len());
        let (destino, vista_de_saida) = &self.destinos[i];
        unsafe {
            let desc_entrada = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                // A textura que chega é do anel da captura: uma fatia só, copiada da fatia certa na
                // chegada (`captura_de_camera.rs`).
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
            };
            let mut vista_de_entrada: Option<ID3D11VideoProcessorInputView> = None;
            self.dispositivo_de_video.CreateVideoProcessorInputView(
                origem,
                &self.enumerador,
                &desc_entrada,
                Some(&mut vista_de_entrada),
            )?;
            let fluxo = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL::from(true),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(vista_de_entrada),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let mut fluxos = [fluxo];
            let r = self.contexto.VideoProcessorBlt(&self.processador, vista_de_saida, 0, &fluxos);
            // A vista é nossa: solta aqui, com o quadro, dê o `Blt` certo ou errado.
            ManuallyDrop::drop(&mut fluxos[0].pInputSurface);
            r?;
        }
        Ok(destino.clone())
    }
}

/// Uma textura NV12 que o processador pode escrever e o MFT pode ler.
fn textura_nv12_de_saida(dispositivo: &ID3D11Device, largura: u32, altura: u32) -> Result<ID3D11Texture2D> {
    let mut ultimo = None;
    for bind in [
        (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        D3D11_BIND_RENDER_TARGET.0 as u32,
    ] {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: largura,
            Height: altura,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut t: Option<ID3D11Texture2D> = None;
        match unsafe { dispositivo.CreateTexture2D(&desc, None, Some(&mut t)) } {
            Ok(()) => {
                if let Some(t) = t {
                    return Ok(t);
                }
            }
            Err(e) => ultimo = Some(e),
        }
    }
    Err(ultimo.unwrap_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_POINTER, "sem textura NV12")))
}
