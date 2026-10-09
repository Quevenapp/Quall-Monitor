//! Reduz a textura capturada ao tamanho que o teto do núcleo permite, **na GPU**.
//!
//! # Por que isto precisa existir no Windows e não no macOS
//!
//! No macOS o teto não custou código: o ScreenCaptureKit compõe o quadro já no tamanho da
//! `SCStreamConfiguration`, então pedir menos é literalmente pedir menos. Aqui não há esse botão.
//! O `Direct3D11CaptureFramePool` entrega a textura no tamanho do **item** capturado — o monitor
//! inteiro —, e o MFT de encode exige que o tipo de entrada tenha a mesma dimensão que ele
//! declarou. Alguém tem de reduzir no meio, e esse alguém é este arquivo.
//!
//! # Por que `ID3D11VideoProcessor` e não as alternativas
//!
//! - **Um shader próprio** (quad de tela cheia) faria o mesmo e obrigaria a carregar bytecode
//!   HLSL compilado no binário, mais estado de pipeline para salvar e restaurar. Mais código para
//!   o mesmo resultado.
//! - **O Video Processor MFT** (`CLSID_VideoProcessorMFT`) é a mesma máquina por uma porta mais
//!   alta — e seria **uma segunda máquina de estados de MFT** no mesmo laço. Este projeto já
//!   gastou uma frente inteira dentro da primeira (créditos de `METransformNeedInput`, `FLUSH`
//!   que invalida crédito, `E_UNEXPECTED` no lugar de `NEED_MORE_INPUT`); acrescentar outra ao
//!   lado, para escalar, seria pagar duas vezes pelo mesmo risco.
//! - **`CopySubresourceRegion`** não escala; corta. Pedir um `framePool` menor também corta, e
//!   corte silencioso é pior que erro.
//!
//! O `ID3D11VideoProcessor` é o escalador que o driver já usa para vídeo, roda no mesmo
//! dispositivo D3D11 que a captura e o encoder compartilham (`device.rs`), e não copia nada para
//! a CPU.
//!
//! # A regra que este arquivo obedece
//!
//! **Nada aqui abre, converte ou grava um pixel.** A origem é a tela de trabalho do usuário. O
//! quadro entra como textura, sai como textura, e vai direto para o encoder — o mesmo caminho que
//! já existia, com um `Blt` a mais. Nenhum caminho de código deste módulo lê a memória do quadro.

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Foundation::RECT;

/// Escala uma textura BGRA para um destino menor, preservando o que o chamador já decidiu.
///
/// **Não decide a proporção.** Quem decide é `quall_core::teto`, e este módulo só executa: recebe
/// as duas dimensões e faz o `Blt`. Um escalador que também escolhesse o tamanho seria uma
/// segunda fonte de verdade sobre o teto, que é exatamente o que esta rodada existe para acabar.
pub struct Escalador {
    contexto: ID3D11VideoContext,
    processador: ID3D11VideoProcessor,
    enumerador: ID3D11VideoProcessorEnumerator,
    dispositivo_de_video: ID3D11VideoDevice,
    /// A textura de saída, **reutilizada em todo quadro**. Alocar uma por quadro colocaria o
    /// alocador da GPU no caminho crítico de um laço que já persegue orçamento de milissegundos.
    destino: ID3D11Texture2D,
    vista_de_saida: ID3D11VideoProcessorOutputView,
    pub largura: u32,
    pub altura: u32,
}

impl Escalador {
    /// Monta o escalador para um par (entrada, saída) fixo.
    ///
    /// O par é fixo de propósito: a resolução do monitor não muda no meio de uma sessão sem que a
    /// captura inteira seja refeita (é o que `VigiaDeMonitor` e `item_fechado` vigiam), então um
    /// escalador que se reconfigurasse sozinho estaria resolvendo um caso que não chega até aqui.
    pub fn novo(
        dispositivo: &ID3D11Device,
        entrada_largura: u32,
        entrada_altura: u32,
        saida_largura: u32,
        saida_altura: u32,
    ) -> Result<Self> {
        unsafe {
            let dispositivo_de_video: ID3D11VideoDevice = dispositivo.cast()?;
            let contexto_imediato = dispositivo.GetImmediateContext()?;
            let contexto: ID3D11VideoContext = contexto_imediato.cast()?;

            let descricao = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: entrada_largura,
                InputHeight: entrada_altura,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: saida_largura,
                OutputHeight: saida_altura,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerador = dispositivo_de_video.CreateVideoProcessorEnumerator(&descricao)?;
            let processador = dispositivo_de_video.CreateVideoProcessor(&enumerador, 0)?;

            // A textura de destino precisa de `RENDER_TARGET` para poder ser saída do processador
            // e de `SHADER_RESOURCE` para o MFT poder consumi-la como superfície DXGI.
            let desc_destino = D3D11_TEXTURE2D_DESC {
                Width: saida_largura,
                Height: saida_altura,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut destino: Option<ID3D11Texture2D> = None;
            dispositivo.CreateTexture2D(&desc_destino, None, Some(&mut destino))?;
            let destino = destino.expect("CreateTexture2D não devolveu textura");

            let desc_vista = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut vista_de_saida: Option<ID3D11VideoProcessorOutputView> = None;
            dispositivo_de_video.CreateVideoProcessorOutputView(
                &destino,
                &enumerador,
                &desc_vista,
                Some(&mut vista_de_saida),
            )?;
            let vista_de_saida = vista_de_saida.expect("CreateVideoProcessorOutputView não devolveu vista");

            // Sem auto-processamento: nitidez, redução de ruído e desentrelaçamento do driver não
            // têm o que fazer num espelhamento de tela, e cada um deles é latência e consumo que
            // ninguém pediu. **Espelhar é reproduzir, não melhorar.**
            contexto.VideoProcessorSetStreamAutoProcessingMode(&processador, 0, false);
            let origem = RECT { left: 0, top: 0, right: entrada_largura as i32, bottom: entrada_altura as i32 };
            let alvo = RECT { left: 0, top: 0, right: saida_largura as i32, bottom: saida_altura as i32 };
            contexto.VideoProcessorSetStreamSourceRect(&processador, 0, true, Some(&origem));
            contexto.VideoProcessorSetStreamDestRect(&processador, 0, true, Some(&alvo));
            contexto.VideoProcessorSetOutputTargetRect(&processador, true, Some(&alvo));

            Ok(Escalador {
                contexto,
                processador,
                enumerador,
                dispositivo_de_video,
                destino,
                vista_de_saida,
                largura: saida_largura,
                altura: saida_altura,
            })
        }
    }

    /// Escala um quadro. Devolve a **mesma** textura de destino a cada chamada — quem chama tem de
    /// entregá-la ao encoder antes da volta seguinte, que é exatamente o que o laço de
    /// `transmissao.rs` já faz com a caixa postal de uma posição.
    pub fn escalar(&self, origem: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        unsafe {
            let desc_entrada = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                },
            };
            // A vista de entrada é criada por quadro porque a textura muda por quadro: o
            // `framePool` do WGC gira entre os seus buffers e não há garantia de qual volta. Criar
            // vista é barato; guardá-la para a textura errada seria mostrar o quadro anterior, que
            // é o tipo de defeito que aparece como rastro e não como erro.
            let mut vista_de_entrada: Option<ID3D11VideoProcessorInputView> = None;
            self.dispositivo_de_video.CreateVideoProcessorInputView(
                origem,
                &self.enumerador,
                &desc_entrada,
                Some(&mut vista_de_entrada),
            )?;
            let vista_de_entrada = vista_de_entrada
                .expect("CreateVideoProcessorInputView não devolveu vista");

            let fluxo = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(vista_de_entrada)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let fluxos = [fluxo];
            self.contexto
                .VideoProcessorBlt(&self.processador, &self.vista_de_saida, 0, &fluxos)?;
            Ok(self.destino.clone())
        }
    }
}
