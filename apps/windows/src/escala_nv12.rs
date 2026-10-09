//! Do quadro decodificado ao quadro que o cano aceita: escala com letterbox e leitura de volta.
//!
//! O decoder da Frente 6 entrega uma **textura D3D11 NV12** no tamanho do vídeo que chegou pela
//! rede — 1920x1080 de um MacBook, 720x1280 (retrato!) de um celular, 1280x720 de outra coisa. O
//! cano só aceita **1920x1080 NV12**, e o motivo está em `fonte/src/quadros.rs`: a fonte de mídia
//! é consultada pelo sistema na geração do *sensor group*, que acontece **sem o Quall estar
//! rodando** — naquele instante não há vídeo nenhum para descrever, então o formato é fixo e
//! encaixar o que vem da rede nele é trabalho do host. É este arquivo.
//!
//! ## Por que Video Processor, e não um shader
//!
//! Mesmo argumento de `apps/windows/src/present.rs`, com uma diferença: lá o destino é RGB (uma
//! janela), aqui é **NV12** — ou seja, não há conversão de cor nenhuma, só escala e recorte. O
//! `VideoProcessorBlt` faz isso no mesmo dispositivo D3D11 que decodificou, sem descer o pixel
//! para a CPU no meio do caminho.
//!
//! ## A única cópia GPU→CPU do caminho, e por que ela é inevitável
//!
//! O cano é um `WriteFile`: ele precisa de bytes em memória de sistema. Então há exatamente uma
//! travessia GPU→CPU por quadro, na textura de leitura (`D3D11_USAGE_STAGING`). Ela está medida
//! no relatório, separada do resto, para ninguém ter de supor quanto custa.
//!
//! ## O plano UV, e a suposição que este arquivo declara
//!
//! Uma textura NV12 é **um** subrecurso: o `Map` devolve um ponteiro só, com o plano Y ocupando
//! `altura` linhas de `RowPitch` bytes e o plano UV logo em seguida. O deslocamento do UV é
//! tratado aqui como `RowPitch * altura` — que é o layout documentado e o que todo driver desta
//! bancada usa, mas **não** é uma garantia da API. Se um driver alinhar a altura do plano Y, a
//! cor sai deslocada e o sintoma é visível de longe (faixas verdes/rosa), não sutil. O
//! `DepthPitch` é conferido em tempo de execução e a divergência é dita no diário em vez de
//! virar imagem errada em silêncio.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use anyhow::{Context, Result};

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use crate::cano;

/// Retângulo que preserva o aspecto do conteúdo dentro da saída — as barras pretas do letterbox
/// (ou pillarbox, no caso do celular em retrato) preenchem o resto.
///
/// É a mesma função de `present.rs`, reescrita aqui em cinco linhas em vez de arrastar o módulo
/// de janela junto: `present::aspect_fit` é `pub`, mas usá-la obrigaria este pacote a compilar
/// `present.rs` inteiro, que cria janela Win32 e swap chain — coisas que uma câmera virtual não
/// tem.
pub fn encaixar(conteudo_w: u32, conteudo_h: u32, alvo_w: u32, alvo_h: u32) -> RECT {
    if conteudo_w == 0 || conteudo_h == 0 {
        return RECT { left: 0, top: 0, right: alvo_w as i32, bottom: alvo_h as i32 };
    }
    let ar_conteudo = conteudo_w as f64 / conteudo_h as f64;
    let ar_alvo = alvo_w as f64 / alvo_h as f64;
    let (w, h) = if ar_conteudo > ar_alvo {
        (alvo_w as f64, alvo_w as f64 / ar_conteudo)
    } else {
        (alvo_h as f64 * ar_conteudo, alvo_h as f64)
    };
    // Larguras e alturas ímpares num destino NV12 fazem o processador de vídeo arredondar por
    // conta própria; arredondar para par aqui deixa o recorte previsível.
    let w = (w.round() as i32) & !1;
    let h = (h.round() as i32) & !1;
    let x = ((alvo_w as i32 - w) / 2) & !1;
    let y = ((alvo_h as i32 - h) / 2) & !1;
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// Quantas cópias de leitura o anel tem. Três quadros em voo a 30 fps são 100 ms: se a GPU ficar
/// mais atrás que isso, o quadro da câmera é **saltado** em vez de o laço esperar por ela.
const VAGAS_DE_LEITURA: usize = 3;

/// O que uma tentativa de colher encontrou.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colheita {
    /// Nada submetido esperando.
    Nada,
    /// Há quadro submetido, e a GPU ainda não terminou de escrevê-lo. Tentar na volta seguinte.
    AindaNaGpu,
    /// Um quadro foi copiado para `saida`.
    Pronta,
}

pub struct Escalador {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerador: ID3D11VideoProcessorEnumerator,
    processador: ID3D11VideoProcessor,
    /// Destino do `VideoProcessorBlt`: NV12 1920x1080 na GPU.
    destino: ID3D11Texture2D,
    /// **Um anel** de cópias legíveis pela CPU, e não uma só. Ver [`Escalador::colher`].
    leituras: Vec<ID3D11Texture2D>,
    /// Quantas submissões já houve; a vaga da próxima é `submetidas % leituras.len()`.
    submetidas: Cell<u64>,
    /// As vagas submetidas e ainda não colhidas, **na ordem da submissão**.
    pendentes: RefCell<VecDeque<usize>>,
    entrada_w: u32,
    entrada_h: u32,
    /// O recorte da textura que é imagem de verdade. Ver o doc de `novo`.
    visivel: RECT,
    destino_rect: RECT,
    /// Avisa uma vez só se o `DepthPitch` não bater com a suposição do plano UV.
    avisou_uv: std::cell::Cell<bool>,
}

impl Escalador {
    /// `codificado` é o tamanho da textura que o decoder entrega; `visivel` é o retângulo dentro
    /// dela que é imagem de verdade.
    ///
    /// Os dois não são iguais, e a diferença já custou uma corrida: um vídeo 1920x**1080** chega
    /// numa textura de 1920x**1088**, porque o H.264 codifica em macroblocos de 16 e 1080 não é
    /// múltiplo de 16. Usar 1088 como se fosse a imagem deu `encaixe: 1920x1088 -> 1280x720 em
    /// (4,0)-(1274,720)`: quatro pixels de barra preta que não deviam existir, e as oito linhas
    /// de preenchimento do decoder espremidas dentro do quadro. Quem sabe o tamanho certo é o
    /// `MF_MT_MINIMUM_DISPLAY_APERTURE` do tipo de saída — ver `receber.rs::abertura_do_tipo`.
    pub fn novo(
        device: &ID3D11Device,
        codificado: (u32, u32),
        visivel: RECT,
        fps: u32,
    ) -> Result<Self> {
        let (entrada_w, entrada_h) = codificado;
        let vis_w = (visivel.right - visivel.left).max(1) as u32;
        let vis_h = (visivel.bottom - visivel.top).max(1) as u32;
        let context = unsafe { device.GetImmediateContext() }.context("GetImmediateContext")?;
        let video_device: ID3D11VideoDevice = device.cast().context("ID3D11VideoDevice")?;
        let video_context: ID3D11VideoContext = context.cast().context("ID3D11VideoContext")?;

        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 },
            InputWidth: entrada_w,
            InputHeight: entrada_h,
            OutputFrameRate: DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 },
            OutputWidth: cano::LARGURA,
            OutputHeight: cano::ALTURA,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerador = unsafe { video_device.CreateVideoProcessorEnumerator(&desc) }
            .context("CreateVideoProcessorEnumerator")?;

        // Confere que o driver aceita NV12 **na saída**. Sem isto, o `CreateVideoProcessorOutputView`
        // falha lá adiante com `E_INVALIDARG` e a mensagem não diz que o problema é o formato.
        let suporte = unsafe { enumerador.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
            .unwrap_or(0);
        let saida_ok = suporte & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 != 0;
        eprintln!(
            "  processador de vídeo: NV12 na entrada={} na saída={}",
            suporte & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 != 0,
            saida_ok
        );
        if !saida_ok {
            anyhow::bail!(
                "este adaptador não escreve NV12 pelo processador de vídeo; o caminho \
                 alternativo (sair em BGRA e converter na CPU) não está implementado"
            );
        }

        let processador = unsafe { video_device.CreateVideoProcessor(&enumerador, 0) }
            .context("CreateVideoProcessor")?;

        let desc_destino = D3D11_TEXTURE2D_DESC {
            Width: cano::LARGURA,
            Height: cano::ALTURA,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut destino: Option<ID3D11Texture2D> = None;
        unsafe { device.CreateTexture2D(&desc_destino, None, Some(&mut destino)) }
            .context("CreateTexture2D (destino NV12)")?;
        let destino = destino.context("CreateTexture2D não devolveu a textura de destino")?;

        let desc_leitura = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..desc_destino
        };
        let mut leituras = Vec::with_capacity(VAGAS_DE_LEITURA);
        for _ in 0..VAGAS_DE_LEITURA {
            let mut leitura: Option<ID3D11Texture2D> = None;
            unsafe { device.CreateTexture2D(&desc_leitura, None, Some(&mut leitura)) }
                .context("CreateTexture2D (leitura NV12)")?;
            leituras.push(leitura.context("CreateTexture2D não devolveu a textura de leitura")?);
        }

        let destino_rect = encaixar(vis_w, vis_h, cano::LARGURA, cano::ALTURA);
        eprintln!(
            "  encaixe: {vis_w}x{vis_h} visível de {entrada_w}x{entrada_h} codificados -> {}x{} em ({},{})-({},{})",
            cano::LARGURA,
            cano::ALTURA,
            destino_rect.left,
            destino_rect.top,
            destino_rect.right,
            destino_rect.bottom
        );

        // Fundo das barras: preto de **faixa limitada** (Y=16), não Y=0. A câmera anuncia
        // `MFNominalRange_16_235` (ver `fonte/src/fonte.rs`), e barras em Y=0 seriam "mais pretas
        // que o preto" — exatamente o defeito sutil que `docs/contrato-sidecar.md` existe para
        // evitar.
        let fundo = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                YCbCr: D3D11_VIDEO_COLOR_YCbCrA {
                    Y: 16.0 / 255.0,
                    Cb: 128.0 / 255.0,
                    Cr: 128.0 / 255.0,
                    A: 1.0,
                },
            },
        };
        unsafe {
            video_context.VideoProcessorSetOutputBackgroundColor(&processador, true, &fundo);
            // Sem interpolação temporal: cada quadro é independente, e um processador que
            // guardasse quadro anterior acrescentaria latência escondida ao caminho.
            video_context.VideoProcessorSetStreamOutputRate(
                &processador,
                0,
                D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                false,
                None,
            );
        }

        Ok(Escalador {
            device: device.clone(),
            context,
            video_device,
            video_context,
            enumerador,
            processador,
            destino,
            leituras,
            submetidas: Cell::new(0),
            pendentes: RefCell::new(VecDeque::with_capacity(VAGAS_DE_LEITURA)),
            entrada_w,
            entrada_h,
            visivel,
            destino_rect,
            avisou_uv: std::cell::Cell::new(false),
        })
    }

    /// O par (tamanho codificado, retângulo visível) com que este escalador foi montado — é por
    /// ele que `receber.rs` decide se precisa montar outro.
    pub fn entrada(&self) -> ((u32, u32), RECT) {
        ((self.entrada_w, self.entrada_h), self.visivel)
    }

    /// Escala o quadro decodificado para 1920x1080 NV12 e escreve os bytes em `saida`.
    ///
    /// `saida` tem de ter exatamente `cano::BYTES_NV12`.
    /// Submete **e** colhe, na mesma chamada. É o caminho simples, e é o que a sonda usa.
    ///
    /// Quem está dentro de um laço que também apresenta na tela deve usar [`Escalador::submeter`]
    /// e [`Escalador::colher`] em quadros diferentes — ver o porquê lá.
    pub fn converter(
        &self,
        textura: &ID3D11Texture2D,
        subrecurso: u32,
        saida: &mut [u8],
    ) -> Result<()> {
        // Quem converte de uma vez não deixa nada no anel; se ele estiver cheio de uma chamada
        // anterior, o que sobrou é descartado para a vaga desta conversão existir.
        while self.pendentes.borrow().len() >= VAGAS_DE_LEITURA {
            self.pendentes.borrow_mut().pop_front();
        }
        self.submeter(textura, subrecurso)?;
        while self.pendentes.borrow().len() > 1 {
            self.pendentes.borrow_mut().pop_front();
        }
        if !self.colher_esperando(saida)? {
            anyhow::bail!("converter: nada no anel depois de submeter");
        }
        Ok(())
    }

    /// Quantos quadros estão submetidos e ainda não colhidos.
    pub fn em_voo(&self) -> usize {
        self.pendentes.borrow().len()
    }

    /// **Enfileira** a escala e a cópia para a próxima vaga do anel. Não espera a GPU.
    ///
    /// `Ok(false)` quando o anel está cheio — as três vagas submetidas e nenhuma colhida. O quadro
    /// da câmera é **saltado**, e isso é o desenho: a câmera virtual é de 30 fps e aguenta perder
    /// um; a janela, que divide este laço com ela, não aguenta esperar.
    pub fn submeter(&self, textura: &ID3D11Texture2D, subrecurso: u32) -> Result<bool> {
        if self.pendentes.borrow().len() >= self.leituras.len() {
            return Ok(false);
        }
        let vaga = (self.submetidas.get() % self.leituras.len() as u64) as usize;
        let desc_entrada = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                // Decoders de hardware reciclam um pool de texturas em array: "o quadro" é sempre
                // o par (textura, índice), nunca só a textura.
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: subrecurso },
            },
        };
        let mut vista_entrada: Option<ID3D11VideoProcessorInputView> = None;
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                textura,
                &self.enumerador,
                &desc_entrada,
                Some(&mut vista_entrada),
            )
        }
        .context("CreateVideoProcessorInputView")?;
        let vista_entrada =
            vista_entrada.context("CreateVideoProcessorInputView não devolveu vista")?;

        let desc_saida = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut vista_saida: Option<ID3D11VideoProcessorOutputView> = None;
        unsafe {
            self.video_device.CreateVideoProcessorOutputView(
                &self.destino,
                &self.enumerador,
                &desc_saida,
                Some(&mut vista_saida),
            )
        }
        .context("CreateVideoProcessorOutputView")?;
        let vista_saida = vista_saida.context("CreateVideoProcessorOutputView não devolveu vista")?;

        let saida_cheia = RECT {
            left: 0,
            top: 0,
            right: cano::LARGURA as i32,
            bottom: cano::ALTURA as i32,
        };

        unsafe {
            self.video_context.VideoProcessorSetOutputTargetRect(
                &self.processador,
                true,
                Some(&saida_cheia),
            );
            self.video_context.VideoProcessorSetStreamSourceRect(
                &self.processador,
                0,
                true,
                // O recorte visível, **não** a textura inteira: é aqui que as linhas de
                // preenchimento do decoder ficam de fora.
                Some(&self.visivel),
            );
            self.video_context.VideoProcessorSetStreamDestRect(
                &self.processador,
                0,
                true,
                Some(&self.destino_rect),
            );
        }

        let fluxo = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: windows::core::BOOL::from(true),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: std::ptr::null_mut(),
            // Cópia sem `AddRef` — ver o mesmo campo em `present::Presenter::present_frame`: com
            // `clone()` o `ManuallyDrop` vazava uma vista por quadro.
            pInputSurface: unsafe { std::mem::transmute_copy(&vista_entrada) },
            ppFutureSurfaces: std::ptr::null_mut(),
            ppPastSurfacesRight: std::ptr::null_mut(),
            pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
            ppFutureSurfacesRight: std::ptr::null_mut(),
        };

        unsafe {
            self.video_context
                .VideoProcessorBlt(&self.processador, &vista_saida, 0, &[fluxo])
                .context("VideoProcessorBlt")?;
            self.context.CopyResource(&self.leituras[vaga], &self.destino);
            // **Mandar para a GPU agora, e não no próximo `Present`.** Sem isto a escala e a
            // cópia ficam no buffer de comandos do driver até alguém o esvaziar — e com o `Map`
            // em `DO_NOT_WAIT` a colheita seguinte acharia "ainda na GPU" um trabalho que a GPU
            // nem recebeu. Apontado pela revisão adversarial de 10/09/2026.
            self.context.Flush();
        }
        self.submetidas.set(self.submetidas.get() + 1);
        self.pendentes.borrow_mut().push_back(vaga);
        let _ = &self.device; // mantido vivo de propósito: as texturas pertencem a ele.
        Ok(true)
    }

    /// **Lê de volta** o quadro mais antigo do anel — **sem esperar a GPU**.
    ///
    /// # A história desta função, em três medidas
    ///
    /// 1. **Submeter e colher juntos** custavam **15,8 ms por quadro** (p50; p95 17,1; pior 25,9)
    ///    dentro do laço que apresenta na janela — o `Map` sem `DO_NOT_WAIT` no contexto imediato
    ///    força o CPU a esperar tudo o que a GPU tinha na fila, inclusive a apresentação. Medido em
    ///    09/09/2026 com a câmera frontal do S24 a 56 fps, que é mais que o orçamento inteiro.
    /// 2. **Colher uma volta depois** caiu para **8,4 ms** — e continuou sendo espera: com a fila
    ///    da rede cheia a volta seguinte começa na hora, e o `Map` alcança uma cópia que a GPU
    ///    ainda não fez. `docs/bancada.md` §8.63.
    /// 3. **Aqui o `Map` pede `DO_NOT_WAIT`**. Se a GPU ainda está escrevendo, a resposta é
    ///    [`Colheita::AindaNaGpu`] e o laço segue para a janela; a próxima volta tenta de novo. O
    ///    anel de [`VAGAS_DE_LEITURA`] cópias é o que deixa a submissão seguinte acontecer
    ///    enquanto esta ainda está na GPU.
    pub fn colher(&self, saida: &mut [u8]) -> Result<Colheita> {
        self.colher_com(saida, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32)
    }

    /// Como [`Escalador::colher`], **esperando** a GPU. É o caminho de quem converte um quadro de
    /// cada vez e não tem janela para servir (a sonda), e o braço de bancada que reproduz o
    /// comportamento de 09/09. `Ok(false)` quando não havia nada no anel.
    pub fn colher_esperando(&self, saida: &mut [u8]) -> Result<bool> {
        Ok(self.colher_com(saida, 0)? == Colheita::Pronta)
    }

    fn colher_com(&self, saida: &mut [u8], bandeiras: u32) -> Result<Colheita> {
        debug_assert_eq!(saida.len(), cano::BYTES_NV12);
        let Some(vaga) = self.pendentes.borrow().front().copied() else {
            return Ok(Colheita::Nada);
        };
        let leitura = &self.leituras[vaga];
        unsafe {
            let mut mapeado = D3D11_MAPPED_SUBRESOURCE::default();
            match self.context.Map(leitura, 0, D3D11_MAP_READ, bandeiras, Some(&mut mapeado)) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                    return Ok(Colheita::AindaNaGpu);
                }
                Err(e) => {
                    // A vaga sai do anel mesmo assim: um `Map` que falha por outro motivo não vai
                    // passar a funcionar na volta seguinte, e deixá-la presa travaria o anel.
                    self.pendentes.borrow_mut().pop_front();
                    return Err(e).context("Map da textura de leitura");
                }
            }

            let passo = mapeado.RowPitch as usize;
            let altura = cano::ALTURA as usize;
            let largura = cano::LARGURA as usize;
            let esperado = passo * altura * 3 / 2;
            if mapeado.DepthPitch != 0
                && (mapeado.DepthPitch as usize) < esperado
                && !self.avisou_uv.get()
            {
                self.avisou_uv.set(true);
                eprintln!(
                    "aviso: DepthPitch={} é menor que RowPitch*altura*3/2={esperado}; o plano UV \
                     pode não começar em RowPitch*altura nesta GPU",
                    mapeado.DepthPitch
                );
            }
            let total = passo * altura * 3 / 2;
            let origem = std::slice::from_raw_parts(mapeado.pData as *const u8, total);

            for y in 0..altura {
                let de = y * passo;
                let para = y * largura;
                saida[para..para + largura].copy_from_slice(&origem[de..de + largura]);
            }
            let inicio_uv_origem = passo * altura;
            let inicio_uv_saida = largura * altura;
            for y in 0..altura / 2 {
                let de = inicio_uv_origem + y * passo;
                let para = inicio_uv_saida + y * largura;
                saida[para..para + largura].copy_from_slice(&origem[de..de + largura]);
            }

            self.context.Unmap(leitura, 0);
        }
        self.pendentes.borrow_mut().pop_front();
        let _ = &self.device; // mantido vivo de propósito: as texturas pertencem a ele.
        Ok(Colheita::Pronta)
    }
}
