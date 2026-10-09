//! Janela de exibição: cria a janela Win32, a swap chain D3D11 e usa o Video Processor do D3D11
//! (`ID3D11VideoDevice`/`ID3D11VideoContext`) para converter a textura NV12 do decoder para RGB e
//! escalar preservando o aspecto (barras pretas quando a janela não tem o mesmo aspecto do
//! conteúdo) — tudo em hardware, no mesmo dispositivo D3D11 que decodificou.
//!
//! Por que Video Processor e não um shader de conversão de cor escrito à mão: é o mesmo
//! mecanismo que o media engine do próprio Windows usa para NV12→RGB com escala. Evita compilar
//! HLSL e montar um pipeline de render só para um blit, e acerta a matriz de conversão de cor
//! (BT.601/BT.709, faixa limitada/completa) sem eu ter de codificar isso à mão — o que importa
//! aqui porque o contrato do Quall já declarou faixa limitada (`docs/contrato-sidecar.md`) e uma
//! conversão manual errada é exatamente o tipo de defeito visual sutil que esse documento cita.
//!
//! **Mesmo adaptador do início ao fim.** O dispositivo D3D11 passado para [`Presenter::new`] tem
//! de ser o mesmo registrado no `IMFDXGIDeviceManager` do decoder (`decoder.rs`) — é a mesma
//! restrição que o encoder mediu para `MFT_MESSAGE_SET_D3D_MANAGER` (`E_INVALIDARG` num
//! adaptador que não bate), e é o que evita a cópia entre adaptadores que o M1 mediu custar 7,33
//! ms por quadro. Se o decoder ativar na NVIDIA (que não serve a tela deste Dell — ver
//! `docs/windows-acesso.md`), a swap chain criada neste mesmo dispositivo força o DXGI a copiar
//! para o adaptador que serve o monitor por baixo dos panos no `Present` — não medido isolado
//! aqui de propósito nenhum, é o preço que reaparece se o decoder escolher a NVIDIA. Ver
//! README.md para qual adaptador decodificou de fato nesta bancada.

use std::mem::ManuallyDrop;

use windows::core::{w, Interface, Result, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Uma janela Win32 simples: sem menu, sem toolbar, sem nada além do necessário para hospedar a
/// swap chain — "sem firula", como pedido no escopo.
pub struct Window {
    pub hwnd: HWND,
    /// O que está escrito na barra de título agora. Guardado para não reescrever o mesmo texto:
    /// `SetWindowTextW` repinta a barra e pisca o botão da barra de tarefas, e esta janela é
    /// atualizada uma vez por segundo.
    ultimo_titulo: String,
}

/// **O sinal de "a pessoa fechou" sai do `WM_CLOSE`, e não do `WM_DESTROY`.**
///
/// `WM_CLOSE` só chega pelo X, pelo Alt+F4 ou pelo menu da janela — é a pessoa. `WM_DESTROY` chega
/// também quando **o próprio app** destrói a janela (`Drop` da `Exibicao`), e até 10/09/2026 era
/// dele que saía o `WM_QUIT`: um `WM_QUIT` é da thread inteira, e quando o receptor passou a largar
/// a exibição e abrir outra na mesma thread depois de uma queda da GPU, o `pump` da janela nova leu
/// o `WM_QUIT` da velha como "fechada pela pessoa" 70 ms depois de abrir. A janela não é destruída
/// aqui: quem a possui a destrói ao largar.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Window {
    /// Cria e mostra a janela, com o cliente do tamanho pedido (não a janela inteira — a borda e
    /// a barra de título do Windows são descontadas via `AdjustWindowRect`, senão o vídeo fica
    /// menor que a resolução declarada e ninguém percebe por quê).
    pub fn create(title: &str, client_width: u32, client_height: u32) -> Result<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let class_name = w!("QuallReceiverWindow");

            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(std::ptr::null_mut()),
                ..Default::default()
            };
            // `RegisterClassW` falha com `ERROR_CLASS_ALREADY_EXISTS` numa segunda janela no
            // mesmo processo — não é o caso desta sonda (uma janela por execução), mas não
            // derruba o processo à toa se um dia passar a ser.
            let _ = RegisterClassW(&wc);

            let mut rect = RECT { left: 0, top: 0, right: client_width as i32, bottom: client_height as i32 };
            let style = WS_OVERLAPPEDWINDOW;
            let _ = AdjustWindowRect(&mut rect, style, false);

            let mut title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let title_pcwstr = windows::core::PCWSTR(title_wide.as_mut_ptr());

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                title_pcwstr,
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rect.right - rect.left,
                rect.bottom - rect.top,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?;

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);

            Ok(Window { hwnd, ultimo_titulo: title.to_string() })
        }
    }

    /// **Reescreve a barra de título — que nesta casca é a única superfície que a janela do vídeo
    /// tem para dizer alguma coisa.**
    ///
    /// Não é enfeite. `docs/contrato-track.md` exige que os cinco contadores da cadeia de
    /// referência cheguem à **tela**, e esta casca cumpria a letra e não o espírito: o painel com
    /// eles vive na janela principal, e a janela principal **não é a que tem a imagem**. Numa
    /// corrida de 01/09/2026 o usuário fotografou o rastro do quadro quebrado dentro desta janela
    /// aqui, com a janela do painel fora da tela — os números existiam, mediam certo, e não
    /// estavam onde os olhos estavam. É a mesma queixa de 31/08 mudando de casca.
    ///
    /// Aqui não há compositor de sobreposição: a janela hospeda a swap chain e mais nada, e
    /// desenhar por cima do vídeo pediria um segundo passe de render só para isso. A barra de
    /// título é o que existe, custa uma chamada por segundo, e é lida sem tirar os olhos da imagem.
    pub fn titular(&mut self, texto: &str) {
        if self.ultimo_titulo == texto {
            return;
        }
        let mut largo: Vec<u16> = texto.encode_utf16().chain(std::iter::once(0)).collect();
        // Falhar aqui não é motivo para nada: é a barra de título de uma janela de vídeo, e o
        // diário continua tendo os mesmos números.
        let _ = unsafe { SetWindowTextW(self.hwnd, windows::core::PCWSTR(largo.as_mut_ptr())) };
        self.ultimo_titulo = texto.to_string();
    }

    /// Escoa a fila de mensagens sem bloquear. Devolve `false` quando a janela pediu para
    /// fechar (`WM_QUIT`) — é o sinal para o laço principal parar.
    pub fn pump(&self) -> bool {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return false;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        true
    }
}

/// Recorte, dentro de uma área de saída `target_w`×`target_h`, que preserva o aspecto de um
/// conteúdo `content_w`×`content_h` — barras pretas (letterbox/pillarbox) preenchem o resto.
/// Nesta sonda a janela nasce do tamanho exato do conteúdo (ver `main`), então o caso comum é
/// `content` == `target` e isto devolve o retângulo cheio; a função existe para o caso de a
/// janela ser redimensionada, e para não fingir que "aspecto preservado" só vale por acidente.
pub fn aspect_fit(content_w: u32, content_h: u32, target_w: u32, target_h: u32) -> RECT {
    if content_w == 0 || content_h == 0 || target_w == 0 || target_h == 0 {
        return RECT { left: 0, top: 0, right: target_w as i32, bottom: target_h as i32 };
    }
    let content_ar = content_w as f64 / content_h as f64;
    let target_ar = target_w as f64 / target_h as f64;
    let (w, h) = if content_ar > target_ar {
        (target_w as f64, target_w as f64 / content_ar)
    } else {
        (target_h as f64 * content_ar, target_h as f64)
    };
    let x = ((target_w as f64 - w) / 2.0).round() as i32;
    let y = ((target_h as f64 - h) / 2.0).round() as i32;
    RECT { left: x, top: y, right: x + w.round() as i32, bottom: y + h.round() as i32 }
}

/// Uma falha do caminho da apresentação, com a etapa no texto. O `0x80070057` do controle de
/// 21/09 saía só como "Parâmetro incorreto", sem dizer se foi a vista de entrada ou o `Blt`.
fn na_etapa(etapa: &str) -> impl Fn(windows::core::Error) -> windows::core::Error + '_ {
    move |e| windows::core::Error::new(e.code(), format!("{etapa}: {}", e.message()))
}

/// Swap chain + Video Processor D3D11: converte e apresenta um quadro NV12 por vez.
pub struct Presenter {
    device: ID3D11Device,
    swapchain: IDXGISwapChain1,
    context: ID3D11DeviceContext,
    video_context: ID3D11VideoContext,
    video_device: ID3D11VideoDevice,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// A entrada para a qual o **processador** foi montado (o enumerador pede o tamanho). Até
    /// 21/09 era também a origem de todo quadro, e é isso que o G1 consertou: a origem é pedida por
    /// quadro (`present_frame`).
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    fps: u32,
}

impl Presenter {
    /// O dispositivo D3D11 desta apresentação.
    ///
    /// Existe porque a câmera virtual precisa converter **a mesma textura** que vai para a janela,
    /// e o `ID3D11VideoProcessor` dela tem de nascer no mesmo dispositivo — a armadilha do
    /// `IMFDXGIDeviceManager` do M1 vale aqui igual: dois dispositivos, e o `CreateVideoProcessor
    /// InputView` recusa a textura com `E_INVALIDARG` sem dizer por quê.
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }
    pub fn new(
        device: &ID3D11Device,
        hwnd: HWND,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        fps: u32,
        buffers: u32,
    ) -> Result<Self> {
        let context = unsafe { device.GetImmediateContext()? };

        let dxgi_device: IDXGIDevice = device.cast()?;
        let adapter = unsafe { dxgi_device.GetAdapter()? };
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent()? };

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: output_width,
            Height: output_height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            // **Dois é o mínimo do modelo *flip*, e pode ser o teto do laço inteiro.** Com um
            // buffer na tela (o DWM o segura até o retraço seguinte) e outro na fila, o
            // `VideoProcessorBlt` do quadro seguinte não tem onde escrever e a GPU espera o vsync
            // — e tudo o que vem atrás dele no contexto imediato espera junto, inclusive a cópia
            // da câmera virtual. Apontado pela revisão adversarial de 10/09/2026 como suspeito do
            // teto de 39 a 51 fps; o número vem de quem chama para o A/B ser no mesmo binário.
            BufferCount: buffers.clamp(2, 4),
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let swapchain = unsafe { factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None)? };

        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;

        let (enumerator, processor) =
            Self::processador(&video_device, input_width, input_height, output_width, output_height, fps)?;

        Ok(Self {
            device: device.clone(),
            swapchain,
            context,
            video_context,
            video_device,
            enumerator,
            processor,
            input_width,
            input_height,
            output_width,
            output_height,
            fps,
        })
    }

    fn processador(
        video_device: &ID3D11VideoDevice,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        fps: u32,
    ) -> Result<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor)> {
        let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 },
            InputWidth: input_width,
            InputHeight: input_height,
            OutputFrameRate: DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 },
            OutputWidth: output_width,
            OutputHeight: output_height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content_desc) }
            .map_err(na_etapa("CreateVideoProcessorEnumerator"))?;
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }
            .map_err(na_etapa("CreateVideoProcessor"))?;
        Ok((enumerator, processor))
    }

    /// **A textura do decoder mudou de tamanho no meio da sessão** (o G1 de 21/09): o enumerador e
    /// o processador são refeitos para a entrada nova. A janela e a swap chain ficam: a imagem nova
    /// entra encaixada na janela de sempre. Custa um enumerador e um processador, uma vez por troca.
    pub fn ajustar_entrada(&mut self, input_width: u32, input_height: u32) -> Result<()> {
        if (input_width, input_height) == (self.input_width, self.input_height) {
            return Ok(());
        }
        let (enumerator, processor) = Self::processador(
            &self.video_device,
            input_width,
            input_height,
            self.output_width,
            self.output_height,
            self.fps,
        )?;
        self.enumerator = enumerator;
        self.processor = processor;
        self.input_width = input_width;
        self.input_height = input_height;
        Ok(())
    }

    /// Converte (NV12→RGB) e apresenta um quadro decodificado: o retângulo `origem` da textura,
    /// dentro de `dest_rect` (o retângulo de [`aspect_fit`]). Limpa o backbuffer antes: fora de
    /// `dest_rect` fica preto, as barras do letterbox/pillarbox.
    ///
    /// **A origem vem de quem chama, quadro a quadro** (o G1 de 21/09): é a abertura visível do
    /// tipo de saída de agora, recortada à textura (`geometria_da_exibicao::origem_na_textura`).
    /// Fixa no primeiro SPS, ela passava da textura quando o fluxo descia de 854 para 640, e o
    /// `present_frame` falhava com `0x80070057` em todo quadro.
    pub fn present_frame(
        &self,
        texture: &ID3D11Texture2D,
        subresource_index: u32,
        origem: RECT,
        dest_rect: RECT,
        snapshot: Option<&std::path::Path>,
    ) -> Result<()> {
        let backbuffer: ID3D11Texture2D =
            unsafe { self.swapchain.GetBuffer(0) }.map_err(na_etapa("GetBuffer"))?;

        // `CreateRenderTargetView` é método do *dispositivo*, não do contexto — o contexto só
        // consome a view já criada (`ClearRenderTargetView`). Erro do primeiro `cargo check`
        // nesta bancada: cheguei a escrever `self.context.CreateRenderTargetView`, que nem
        // existe (o compilador sugeriu `ClearRenderTargetView` por semelhança de nome).
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        unsafe { self.device.CreateRenderTargetView(&backbuffer, None, Some(&mut rtv)) }
            .map_err(na_etapa("CreateRenderTargetView"))?;
        let rtv = rtv.ok_or_else(|| {
            windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "CreateRenderTargetView não devolveu view")
        })?;
        let preto: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
        unsafe { self.context.ClearRenderTargetView(&rtv, &preto) };

        let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: subresource_index },
            },
        };
        // `CreateVideoProcessorInputView`/`CreateVideoProcessorOutputView` também não ganharam o
        // invólucro `Result<T>` nesta versão do `windows-rs` (mesmo padrão do
        // `IMFDXGIBuffer::GetResource` em `decoder.rs`) — o último parâmetro é o ponteiro de
        // saída, explícito.
        let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                texture,
                &self.enumerator,
                &input_view_desc,
                Some(&mut input_view),
            )
        }
        .map_err(na_etapa("CreateVideoProcessorInputView"))?;
        let input_view = input_view.ok_or_else(|| {
            windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "CreateVideoProcessorInputView não devolveu view")
        })?;

        let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
        unsafe {
            self.video_device.CreateVideoProcessorOutputView(
                &backbuffer,
                &self.enumerator,
                &output_view_desc,
                Some(&mut output_view),
            )
        }
        .map_err(na_etapa("CreateVideoProcessorOutputView"))?;
        let output_view = output_view.ok_or_else(|| {
            windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "CreateVideoProcessorOutputView não devolveu view")
        })?;

        let full_output = RECT { left: 0, top: 0, right: self.output_width as i32, bottom: self.output_height as i32 };

        unsafe {
            self.video_context.VideoProcessorSetOutputTargetRect(&self.processor, true, Some(&full_output));
            self.video_context.VideoProcessorSetStreamSourceRect(&self.processor, 0, true, Some(&origem));
            self.video_context.VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&dest_rect));
            self.video_context.VideoProcessorSetStreamOutputRate(
                &self.processor,
                0,
                D3D11_VIDEO_PROCESSOR_OUTPUT_RATE_NORMAL,
                false,
                None,
            );
        }

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL::from(true),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: std::ptr::null_mut(),
            // **Cópia sem `AddRef`**, e não `clone()`: o campo é `ManuallyDrop` e nunca solta o
            // que recebe. Com `clone()` cada quadro vazava uma referência à vista — e a vista
            // segura a textura do decoder —, 60 por segundo, até 10/09/2026. `input_view` é solta
            // no fim desta função, e a cópia aqui não conta.
            pInputSurface: unsafe { std::mem::transmute_copy(&input_view) },
            ppFutureSurfaces: std::ptr::null_mut(),
            ppPastSurfacesRight: std::ptr::null_mut(),
            pInputSurfaceRight: ManuallyDrop::new(None),
            ppFutureSurfacesRight: std::ptr::null_mut(),
        };

        unsafe { self.video_context.VideoProcessorBlt(&self.processor, &output_view, 0, &[stream]) }
            .map_err(na_etapa("VideoProcessorBlt"))?;

        // Prova visual, se pedida: lê o **mesmo objeto** `backbuffer` que acabamos de escrever,
        // *antes* do `Present`. Achado desta bancada, não escondido: a primeira versão disto
        // chamava `GetBuffer(0)` de novo, numa função separada, depois do `Present` — e o
        // resultado ora saía preto (a troca de buffers do modo *flip* faz `GetBuffer(0)` devolver
        // o buffer que ainda **não** foi desenhado nesta rodada, não o que acabou de ir pra tela),
        // ora saía com conteúdo de outra janela do desktop (uma composição do DWM capturada por
        // acidente, não o vídeo). Seguro só existe reaproveitando a referência que este método já
        // tem, escrita por ele mesmo, sem outra chamada a `GetBuffer` no meio.
        if let Some(path) = snapshot {
            if let Err(e) = self.copy_texture_to_bmp(&backbuffer, path) {
                eprintln!("aviso: não salvou o snapshot em {}: {e}", path.display());
            }
        }

        // `Present(0, ...)`: sem espera de vsync — este número mede o custo do próprio pipeline
        // (decode + blt + apresentar o backbuffer), não quanto falta para o próximo retraço. O
        // tempo até o fóton de fato sair da tela inclui até um intervalo de quadro a mais que
        // isto não captura — dito no README.md, não escondido atrás de "medi a latência".
        unsafe { self.swapchain.Present(0, DXGI_PRESENT(0)) }.ok().map_err(na_etapa("Present"))?;
        Ok(())
    }

    /// Copia o backbuffer (o que acabou de ir pro `Present`) pra um `.bmp` em disco — prova do
    /// pixel de verdade, sem depender de captura de tela do Windows.
    ///
    /// Existiu porque a captura de tela por GDI (`CopyFromScreen`/`BitBlt`, o jeito óbvio de
    /// "printar a tela" por script) devolveu a **mesma imagem, byte a byte, em quatro tentativas
    /// separadas** nesta bancada — sinal de que o DWM não está recompondo quadro novo nenhum
    /// nesta sessão (sessão "Interativa" via Tarefa Agendada, sem ninguém de fato olhando o
    /// monitor físico ou conectado por RDP; o compositor aparenta ficar parado num quadro
    /// congelado quando não há consumidor). Não investiguei a causa raiz a fundo — só medi que
    /// `SetForegroundWindow`/`SetWindowPos(HWND_TOPMOST)` devolveram sucesso (a janela real
    /// mudou de lugar na ordem Z) e mesmo assim a "foto" continuou idêntica, o que aponta pra
    /// captura de tela, não pra apresentação, como a parte quebrada. Ler o backbuffer direto da
    /// GPU contorna o problema inteiro: é o mesmo dado que o `VideoProcessorBlt` acabou de
    /// escrever, half a dependência de o Windows "fotografar a tela" certo.
    fn copy_texture_to_bmp(&self, texture: &ID3D11Texture2D, path: &std::path::Path) -> Result<()> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            let staging = staging.ok_or_else(|| {
                windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "CreateTexture2D (staging) não devolveu textura")
            })?;

            self.context.CopyResource(&staging, texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

            let width = desc.Width;
            let height = desc.Height;
            let row_pitch = mapped.RowPitch as usize;
            let total = row_pitch * height as usize;
            let src = std::slice::from_raw_parts(mapped.pData as *const u8, total);

            let resultado = write_bmp_top_down_bgra(path, width, height, src, row_pitch);

            self.context.Unmap(&staging, 0);

            resultado.map_err(|e| {
                windows::core::Error::new(windows::Win32::Foundation::E_FAIL, format!("gravar .bmp: {e}"))
            })?;
        }
        Ok(())
    }
}

/// Escreve um `.bmp` de 24 bpp (sem canal alfa — a janela não usa transparência) a partir de um
/// buffer BGRA de origem (o formato do backbuffer, `DXGI_FORMAT_B8G8R8A8_UNORM`), respeitando o
/// `RowPitch` de origem (que quase sempre é maior que `width * 4` por alinhamento de GPU) e
/// escrevendo de cima para baixo (`biHeight` negativo — variante padrão de BMP "top-down", assim
/// as linhas não precisam ser invertidas na cópia).
fn write_bmp_top_down_bgra(
    path: &std::path::Path,
    width: u32,
    height: u32,
    src: &[u8],
    src_row_pitch: usize,
) -> std::io::Result<()> {
    use std::io::Write;

    let dst_row_size = ((width * 3 + 3) / 4) * 4;
    let pixel_data_size = dst_row_size * height;
    let file_size = 14 + 40 + pixel_data_size;

    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);

    // BITMAPFILEHEADER (14 bytes)
    f.write_all(b"BM")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&0u16.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?;

    // BITMAPINFOHEADER (40 bytes)
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&(width as i32).to_le_bytes())?;
    f.write_all(&(-(height as i64) as i32).to_le_bytes())?; // negativo: top-down
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?; // BI_RGB
    f.write_all(&pixel_data_size.to_le_bytes())?;
    f.write_all(&0i32.to_le_bytes())?;
    f.write_all(&0i32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;

    let mut row_buf = vec![0u8; dst_row_size as usize];
    for y in 0..height as usize {
        let src_row = &src[y * src_row_pitch..y * src_row_pitch + (width as usize * 4)];
        for x in 0..width as usize {
            let px = &src_row[x * 4..x * 4 + 4];
            row_buf[x * 3] = px[0]; // B
            row_buf[x * 3 + 1] = px[1]; // G
            row_buf[x * 3 + 2] = px[2]; // R
        }
        for b in row_buf[(width as usize * 3)..].iter_mut() {
            *b = 0;
        }
        f.write_all(&row_buf)?;
    }
    f.flush()
}
