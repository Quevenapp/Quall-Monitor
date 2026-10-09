//! **O texto do prompter**: a quebra em linhas, numa thread própria, e o desenho, quadro a quadro.
//!
//! # Por que DirectWrite e Direct2D, e por que sobre um DC
//!
//! O resto do app é GDI (`janela.rs`), e GDI desenharia o roteiro — mas só em pixel inteiro: a
//! 80 px por segundo (1 linha/s a 48 DIP) o texto andaria 1, 1, 2, 1, 1, 2 px por quadro, uma
//! batida de 20 Hz que o olho de quem lê em movimento percebe. O Direct2D desenha o texto em
//! **fração de pixel** (`D2D1_DRAW_TEXT_OPTIONS_NO_SNAP`, "recomendado para texto animado"), e o
//! DirectWrite dá o que o prompter precisa e o GDI não dá: linhas de **altura uniforme**
//! (`DWRITE_LINE_SPACING_METHOD_UNIFORM`, a unidade da velocidade), troca de fonte para emoji e
//! acento, e a quebra de um roteiro inteiro numa chamada.
//!
//! **Sobre um DC, e não numa swap chain.** Uma swap chain (`CreateSwapChainForHwnd`) daria o
//! compasso da tela de graça, mas **falha na Sessão 0** (`0x887A0022`, medido pela frente do
//! receptor) — e é na Sessão 0, pelo SSH, que esta frente prova sem mão humana. O
//! `ID2D1DCRenderTarget` desenha num DIB nosso (o quadro de trás, ver `tela.rs::desenhar_texto`),
//! que vai para a janela por `BitBlt` e que a captura de bancada (`WM_PRINT`) lê nas duas sessões. O
//! compasso vem do DWM (`DwmFlush`), quando ele existe; ver `tela.rs`.
//!
//! # O defeito do iPhone que não pode se repetir aqui
//!
//! No iPhone X, refazer o layout de um roteiro de 100 KB custa 250–340 ms **na thread principal**,
//! e o texto rolando dá um tranco quando o roteiro chega ou a fonte muda (handover §6, item 6).
//! Aqui a quebra roda no [`Diagramador`], numa thread só dela: a janela continua desenhando com o
//! layout velho até o novo ficar pronto, e troca num quadro. O que sobra na thread da janela é
//! criar o layout das **linhas visíveis** (uma dúzia de `IDWriteTextLayout` de uma linha cada),
//! e só delas.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget, ID2D1Factory, ID2D1PathGeometry, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, D2D1_DRAW_TEXT_OPTIONS, D2D1_DRAW_TEXT_OPTIONS_NO_SNAP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_MEDIUM,
    DWRITE_LINE_METRICS, DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows_numerics::{Matrix3x2, Vector2};

use super::geometria::GeometriaDoTexto;
use super::regras::{self, LinhaQuebrada};

/// O espaço entre linhas, como fração da altura natural da fonte: o 1,25 do Mac
/// (`TextoDiagramado.entrelinha`). Linhas muito juntas fazem o olho pular de linha em movimento.
pub const ENTRELINHA: f32 = 1.25;
const FAMILIA: PCWSTR = w!("Segoe UI");
const LOCALIDADE: PCWSTR = w!("pt-br");

/// `D2DERR_RECREATE_TARGET`: o dispositivo do Direct2D se perdeu (driver trocado, GPU
/// reiniciada). O alvo é refeito no quadro seguinte.
const D2DERR_RECREATE_TARGET: i32 = 0x8899_000Cu32 as i32;

/// Um pedido de quebra: o texto, a fonte e a largura em que ele vai ser desenhado.
#[derive(Clone)]
pub struct PedidoDeDiagrama {
    pub geracao: u64,
    pub texto: Arc<Vec<u16>>,
    pub bytes: usize,
    pub fonte_px: f32,
    pub largura_px: f32,
}

/// O roteiro quebrado em linhas. Leva o próprio texto: as linhas se referem **a ele**, e não ao
/// texto que a réplica tem agora (que pode ter mudado enquanto a quebra rodava).
#[derive(Clone, Debug)]
pub struct Diagramado {
    pub geracao: u64,
    pub texto: Arc<Vec<u16>>,
    pub bytes: usize,
    pub geometria: GeometriaDoTexto,
    pub fonte_px: f32,
    pub largura_px: f32,
    pub altura_px: f32,
    pub base_px: f32,
    /// Quanto a quebra levou, na thread do diagramador.
    pub custo_ms: f64,
}

impl Diagramado {
    pub fn vazio() -> Diagramado {
        Diagramado {
            geracao: 0,
            texto: Arc::new(Vec::new()),
            bytes: 0,
            geometria: GeometriaDoTexto::vazia(),
            fonte_px: 0.0,
            largura_px: 0.0,
            altura_px: 1.0,
            base_px: 0.0,
            custo_ms: 0.0,
        }
    }
}

/// **A thread que quebra o roteiro.** Recebe pedidos; quando chegam vários antes de ela acabar o
/// anterior, só o mais novo é feito (um controle deslizante de fonte manda dezenas).
pub struct Diagramador {
    envio: Sender<PedidoDeDiagrama>,
    pronto: Arc<Mutex<Option<Diagramado>>>,
    falha: Arc<Mutex<Option<String>>>,
}

impl Diagramador {
    pub fn novo(acordar: Box<dyn Fn() + Send>) -> Diagramador {
        let (envio, recebe) = channel::<PedidoDeDiagrama>();
        let pronto = Arc::new(Mutex::new(None));
        let falha = Arc::new(Mutex::new(None));
        let p = Arc::clone(&pronto);
        let f = Arc::clone(&falha);
        let _ = std::thread::Builder::new().name("quall.teleprompter.diagrama".into()).spawn(move || {
            let fabrica: Option<IDWriteFactory> = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok() };
            while let Ok(mut pedido) = recebe.recv() {
                while let Ok(mais_novo) = recebe.try_recv() {
                    pedido = mais_novo;
                }
                let r = match &fabrica {
                    Some(fab) => diagramar(fab, &pedido),
                    None => Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL)),
                };
                match r {
                    Ok(d) => *p.lock().unwrap_or_else(|e| e.into_inner()) = Some(d),
                    Err(e) => *f.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.to_string()),
                }
                acordar();
            }
        });
        Diagramador { envio, pronto, falha }
    }

    pub fn pedir(&self, p: PedidoDeDiagrama) {
        let _ = self.envio.send(p);
    }

    /// O último diagrama pronto, se houver um que ainda não foi pego.
    pub fn pegar(&self) -> Option<Diagramado> {
        self.pronto.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn pegar_falha(&self) -> Option<String> {
        self.falha.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// O formato do texto do prompter, com a altura uniforme das linhas já posta. Devolve a altura e a
/// linha de base, em pixels.
fn formato(fabrica: &IDWriteFactory, fonte_px: f32, quebra: bool) -> Result<(IDWriteTextFormat, f32, f32)> {
    unsafe {
        let formato = fabrica.CreateTextFormat(
            FAMILIA,
            None::<&IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT_MEDIUM,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            fonte_px.max(1.0),
            LOCALIDADE,
        )?;
        // A altura natural de uma linha nesta fonte, medida pelo próprio DirectWrite numa amostra
        // com ascendente e descendente ("Ág"), e a entrelinha de teleprompter por cima.
        let amostra: Vec<u16> = "Ág".encode_utf16().collect(); // i18n: fora (medida e diário)
        let medida = fabrica.CreateTextLayout(&amostra, &formato, 100_000.0, 100_000.0)?;
        let mut linhas = [DWRITE_LINE_METRICS::default(); 1];
        let mut n = 0u32;
        medida.GetLineMetrics(Some(&mut linhas[..]), &mut n)?;
        let natural = linhas[0].height.max(1.0);
        let altura = (natural * ENTRELINHA).ceil();
        let base = linhas[0].baseline + (altura - natural) / 2.0;
        formato.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, altura, base)?;
        formato.SetWordWrapping(if quebra { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
        Ok((formato, altura, base))
    }
}

/// **A quebra do roteiro inteiro** numa fonte e numa largura: o layout e as métricas de linha. É o
/// motor único de quebra — a vista ([`diagramar`]) e a fonte automática ([`passa_na_regra`]) usam
/// esta mesma chamada, então a fonte que a busca aprova é a quebra que a tela mostra.
fn quebrar(
    fabrica: &IDWriteFactory,
    texto: &[u16],
    formato: &IDWriteTextFormat,
    largura_px: f32,
) -> Result<(IDWriteTextLayout, Vec<DWRITE_LINE_METRICS>)> {
    unsafe {
        let layout = fabrica.CreateTextLayout(texto, formato, largura_px.max(1.0), 1.0e9)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        let mut linhas = vec![DWRITE_LINE_METRICS::default(); m.lineCount as usize];
        let mut n = 0u32;
        layout.GetLineMetrics(Some(linhas.as_mut_slice()), &mut n)?;
        linhas.truncate(n as usize);
        Ok((layout, linhas))
    }
}

/// **A quebra**: um `IDWriteTextLayout` do roteiro inteiro, na largura da coluna de texto (entre as
/// setas do enquadramento, menos as margens), e as métricas de linha dele. O layout grande é jogado
/// fora — o desenho refaz só as linhas visíveis, cada uma com o seu layout de uma linha.
fn diagramar(fabrica: &IDWriteFactory, p: &PedidoDeDiagrama) -> Result<Diagramado> {
    let comeco = Instant::now();
    let (formato, altura, base) = formato(fabrica, p.fonte_px, true)?;
    let mut inicios = Vec::new();
    if !p.texto.is_empty() {
        {
            let (_layout, linhas) = quebrar(fabrica, &p.texto, &formato, p.largura_px)?;
            let mut pos = 0usize;
            let total = linhas.len();
            for (i, l) in linhas.iter().enumerate() {
                // Um roteiro que termina em quebra **não** ganha a linha em branco depois dela:
                // com ela, a posição 1 ("o fim na linha de leitura", §3) deixaria a última linha
                // escrita uma linha acima da leitura — o mesmo corte do Mac.
                if l.length == 0 && i + 1 == total && i > 0 {
                    break;
                }
                inicios.push(pos);
                pos += l.length as usize;
            }
        }
    }
    Ok(Diagramado {
        geracao: p.geracao,
        texto: Arc::clone(&p.texto),
        bytes: p.bytes,
        geometria: GeometriaDoTexto::nova(f64::from(altura), inicios, p.texto.len()),
        fonte_px: p.fonte_px,
        largura_px: p.largura_px,
        altura_px: altura,
        base_px: base,
        custo_ms: comeco.elapsed().as_secs_f64() * 1000.0,
    })
}

// =============================================================================================
// A fonte automática (`docs/teleprompter-ajustes-locais.md` §5)
// =============================================================================================

/// Um pedido de fonte automática: o roteiro e a largura da coluna de texto em que ele vai ser
/// desenhado (que não depende da fonte: é a área entre as setas menos as margens).
#[derive(Clone)]
pub struct PedidoDeFonte {
    pub geracao: u64,
    pub texto: Arc<Vec<u16>>,
    pub bytes: usize,
    pub largura_px: f32,
    /// Pixels por DIP: a fonte do núcleo é em DIP, a quebra é em pixels.
    pub px_por_dip: f32,
}

/// A resposta: a maior fonte que passa (`None`: nem 8 passa), e o que custou.
#[derive(Clone, Debug)]
pub struct FonteCalculada {
    pub geracao: u64,
    pub fonte: Option<f64>,
    pub perguntas: u32,
    pub custo_ms: f64,
    pub bytes: usize,
    pub largura_px: f32,
    /// A quebra falhou (DirectWrite): a fonte fica como está.
    pub falha: Option<String>,
}

/// **A thread da fonte automática** (`quall.teleprompter.fonte`): a busca binária de 8 a 400 pt,
/// cada passo uma quebra do roteiro **inteiro** (sem amostra) pelo mesmo motor da vista. Como o
/// [`Diagramador`], só o pedido mais novo é feito; e um pedido que chega no meio corta a busca em
/// curso (entre um passo e outro).
pub struct FonteAutomatica {
    envio: Sender<PedidoDeFonte>,
    pronto: Arc<Mutex<Option<FonteCalculada>>>,
}

impl FonteAutomatica {
    pub fn nova(acordar: Box<dyn Fn() + Send>) -> FonteAutomatica {
        let (envio, recebe) = channel::<PedidoDeFonte>();
        let pronto = Arc::new(Mutex::new(None));
        let p = Arc::clone(&pronto);
        let _ = std::thread::Builder::new().name("quall.teleprompter.fonte".into()).spawn(move || {
            let fabrica: Option<IDWriteFactory> = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok() };
            let mut pendente: Option<PedidoDeFonte> = None;
            loop {
                let mut pedido = match pendente.take() {
                    Some(p) => p,
                    None => match recebe.recv() {
                        Ok(p) => p,
                        Err(_) => break,
                    },
                };
                while let Ok(mais_novo) = recebe.try_recv() {
                    pedido = mais_novo;
                }
                let comeco = Instant::now();
                let mut falha = None;
                // Um pedido mais novo no meio da busca (a pessoa arrastando uma seta) a corta: a
                // conta velha já não serve, e esperar por ela dobraria a demora da nova.
                let mut cortada: Option<PedidoDeFonte> = None;
                let (fonte, perguntas) = match &fabrica {
                    Some(fab) => regras::maior_fonte_que_passa(|f| {
                        if cortada.is_none() {
                            cortada = recebe.try_recv().ok();
                        }
                        if cortada.is_some() {
                            return false;
                        }
                        match passa_na_regra(fab, &pedido.texto, (f as f32) * pedido.px_por_dip, pedido.largura_px) {
                            Ok(passou) => passou,
                            Err(e) => {
                                falha.get_or_insert_with(|| e.to_string());
                                false
                            }
                        }
                    }),
                    None => {
                        falha = Some("DWriteCreateFactory falhou".into());
                        (None, 0)
                    }
                };
                if let Some(novo) = cortada {
                    pendente = Some(novo);
                    continue;
                }
                let r = FonteCalculada {
                    geracao: pedido.geracao,
                    fonte: if falha.is_some() { None } else { fonte },
                    perguntas,
                    custo_ms: comeco.elapsed().as_secs_f64() * 1000.0,
                    bytes: pedido.bytes,
                    largura_px: pedido.largura_px,
                    falha,
                };
                *p.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                acordar();
            }
        });
        FonteAutomatica { envio, pronto }
    }

    pub fn pedir(&self, p: PedidoDeFonte) {
        let _ = self.envio.send(p);
    }

    pub fn pegar(&self) -> Option<FonteCalculada> {
        self.pronto.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Numa fonte (em pixels), o roteiro inteiro passa na regra ([`regras::primeira_linha_reprovada`])?
fn passa_na_regra(fabrica: &IDWriteFactory, texto: &[u16], fonte_px: f32, largura_px: f32) -> Result<bool> {
    if texto.is_empty() {
        return Ok(true);
    }
    let (formato, _, _) = formato(fabrica, fonte_px, true)?;
    let (_layout, metricas) = quebrar(fabrica, texto, &formato, largura_px)?;
    let mut linhas = Vec::with_capacity(metricas.len());
    let mut pos = 0usize;
    let total = metricas.len();
    for (i, m) in metricas.iter().enumerate() {
        let inicio = pos.min(texto.len());
        let fim = (pos + m.length as usize).min(texto.len());
        let sem_quebra = fim.saturating_sub(m.newlineLength as usize).max(inicio);
        linhas.push(LinhaQuebrada { texto: &texto[inicio..sem_quebra], fim_de_paragrafo: m.newlineLength > 0 || i + 1 == total });
        pos = fim;
    }
    Ok(regras::primeira_linha_reprovada(&linhas).is_none())
}

/// O que um quadro desenha.
pub struct Cena<'a> {
    pub diagramado: &'a Diagramado,
    pub deslocamento: f64,
    /// Onde a linha de leitura fica, em pixels a partir do topo da área do texto.
    pub y_leitura: f64,
    /// **As setas do enquadramento**, em pixels a partir da esquerda da área, **na vista do texto**
    /// (antes do espelho): cada linha sai centrada entre elas, e a faixa e as setas da linha de
    /// leitura vão de uma à outra.
    pub setas_px: (f32, f32),
    /// Pixels por DIP (a escala do monitor), para o tamanho das marcas.
    pub px_por_dip: f32,
    pub espelho: bool,
    /// Sem roteiro: a frase que aparece no lugar dele.
    pub vazio: Option<&'a str>,
    /// Enquanto a pessoa arrasta a linha de leitura: o valor em % ao lado das setas.
    pub rotulo_da_linha: Option<&'a str>,
    /// Enquanto a pessoa arrasta uma seta do enquadramento: qual (`0` = a da esquerda do texto) e
    /// o valor em %.
    pub enquadrando: Option<(usize, &'a str)>,
    /// As guias verticais finas nas duas bordas do enquadramento: enquanto as faixas estão à mostra
    /// e durante o arrasto (a regra das quatro telas, `docs/teleprompter-ajustes-locais.md` §7).
    pub guias: bool,
    /// **A tela R5** (`docs/teleprompter-com-camera.md` §8.5, §8.10): a lente fica no alto do texto,
    /// e nada fica entre os dois — os triângulos do enquadramento vão ao **pé** do texto, apontando
    /// para cima. O prompter comum mantém as marcas no alto (a decisão de 14/09).
    pub marcas_no_pe: bool,
}

/// Onde as duas setas do enquadramento aparecem **na tela**, da esquerda para a direita: com
/// espelho, a da esquerda do texto aparece à direita (`largura − x`). Serve ao desenho e ao mouse.
pub fn setas_na_tela(setas_px: (f32, f32), largura: f32, espelho: bool) -> (f32, f32) {
    if espelho {
        (largura - setas_px.1, largura - setas_px.0)
    } else {
        setas_px
    }
}

/// **O desenho do texto**, na thread da janela. Os recursos do Direct2D são refeitos quando o
/// dispositivo se perde; o formato e o cache das linhas, quando o diagrama muda.
pub struct Desenho {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    alvo: Option<Alvo>,
    /// O alvo do Direct2D é o de software (o padrão falhou uma vez; ver [`Desenho::desenhar`]).
    software: bool,
    /// Por que o alvo padrão foi trocado pelo de software, para o registro e o relato.
    pub motivo_do_software: Option<String>,
    formato_das_linhas: Option<(u64, IDWriteTextFormat)>,
    formato_do_aviso: Option<IDWriteTextFormat>,
    /// A seta da linha de leitura (apontando para dentro, com a ponta em x = `lado`), e o tamanho
    /// para que foi feita.
    seta: Option<(f32, ID2D1PathGeometry)>,
    /// O triângulo das setas do enquadramento (no alto da borda, apontando para baixo).
    triangulo: Option<(f32, ID2D1PathGeometry)>,
    cache: HashMap<usize, IDWriteTextLayout>,
    geracao_do_cache: u64,
    /// Quantos layouts de linha foram criados (para o relato: é o custo que ficou na thread da
    /// janela).
    pub linhas_criadas: u64,
}

struct Alvo {
    rt: ID2D1DCRenderTarget,
    texto: ID2D1SolidColorBrush,
    faixa: ID2D1SolidColorBrush,
    marcador: ID2D1SolidColorBrush,
    fraco: ID2D1SolidColorBrush,
    /// O azul das setas do enquadramento: outra cor que o laranja da linha de leitura, para as duas
    /// marcas não se confundirem no vidro.
    enquadrador: ID2D1SolidColorBrush,
    /// A guia vertical das bordas do enquadramento: o mesmo azul, fraco.
    guia: ID2D1SolidColorBrush,
}

const PRETO: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

impl Desenho {
    pub fn novo() -> Result<Desenho> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Desenho {
                d2d,
                dwrite,
                alvo: None,
                software: false,
                motivo_do_software: None,
                formato_das_linhas: None,
                formato_do_aviso: None,
                seta: None,
                triangulo: None,
                cache: HashMap::new(),
                geracao_do_cache: u64::MAX,
                linhas_criadas: 0,
            })
        }
    }

    fn alvo(&mut self) -> Result<&Alvo> {
        if self.alvo.is_none() {
            unsafe {
                // 96 dpi no alvo: uma unidade do Direct2D é um pixel. A escala do monitor já entrou
                // na fonte (DIP × dpi ÷ 96) e nas margens, que são frações.
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: if self.software { D2D1_RENDER_TARGET_TYPE_SOFTWARE } else { D2D1_RENDER_TARGET_TYPE_DEFAULT },
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = self.d2d.CreateDCRenderTarget(&props)?;
                // Tons de cinza, e não ClearType: o ClearType presume subpixels RGB na horizontal,
                // e o espelho os inverte (o vidro do teleprompter mostraria franjas coloridas).
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let cor = |r: f32, g: f32, b: f32, a: f32| D2D1_COLOR_F { r, g, b, a };
                let texto = rt.CreateSolidColorBrush(&cor(1.0, 1.0, 1.0, 1.0), None)?;
                let faixa = rt.CreateSolidColorBrush(&cor(1.0, 1.0, 1.0, 0.08), None)?;
                let marcador = rt.CreateSolidColorBrush(&cor(1.0, 0.62, 0.10, 0.95), None)?;
                let fraco = rt.CreateSolidColorBrush(&cor(0.7, 0.7, 0.7, 1.0), None)?;
                let enquadrador = rt.CreateSolidColorBrush(&cor(0.35, 0.72, 1.0, 0.95), None)?;
                let guia = rt.CreateSolidColorBrush(&cor(0.35, 0.72, 1.0, 0.45), None)?;
                self.alvo = Some(Alvo { rt, texto, faixa, marcador, fraco, enquadrador, guia });
            }
        }
        Ok(self.alvo.as_ref().expect("acabou de ser criado"))
    }

    /// O layout de uma linha, do cache ou novo. O texto é o do diagrama, sem a quebra e sem o
    /// espaço do fim; o layout tem a largura da coluna em que o diagrama quebrou, com a linha
    /// **centralizada** nela (§3: fixo, não é opção) — o desenho põe a coluna entre as setas.
    fn linha(&mut self, i: usize, d: &Diagramado) -> Result<Option<IDWriteTextLayout>> {
        if self.geracao_do_cache != d.geracao {
            self.cache.clear();
            self.geracao_do_cache = d.geracao;
        }
        if let Some(l) = self.cache.get(&i) {
            return Ok(Some(l.clone()));
        }
        let inicio = d.geometria.inicios[i];
        let fim = d.geometria.inicios.get(i + 1).copied().unwrap_or(d.texto.len()).min(d.texto.len());
        let mut s = &d.texto[inicio.min(fim)..fim];
        // O espaço do fim sai também: a quebra o deixa na linha, e ele deslocaria o centro.
        while let Some(&c) = s.last() {
            if matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029 | 0x20 | 0x09 | 0x0B | 0x0C | 0x85 | 0x3000) {
                s = &s[..s.len() - 1];
            } else {
                break;
            }
        }
        if s.is_empty() {
            return Ok(None);
        }
        let precisa_formato = !matches!(&self.formato_das_linhas, Some((g, _)) if *g == d.geracao);
        if precisa_formato {
            let (f, _, _) = formato(&self.dwrite, d.fonte_px, false)?;
            // O formato das linhas tem de ter a mesma altura e base do diagrama: a altura veio da
            // mesma conta, mas é posta de novo explicitamente para as duas nunca divergirem.
            unsafe {
                f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, d.altura_px, d.base_px)?;
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            }
            self.formato_das_linhas = Some((d.geracao, f));
        }
        let (_, f) = self.formato_das_linhas.as_ref().expect("posto acima");
        // Sem quebra (`NO_WRAP`): a linha já veio quebrada; se uma fração de pixel de diferença na
        // forma a deixar mais larga que a coluna, ela transborda igual dos dois lados.
        let l = unsafe { self.dwrite.CreateTextLayout(s, f, d.largura_px.max(1.0), d.altura_px)? };
        self.linhas_criadas += 1;
        self.cache.insert(i, l.clone());
        Ok(Some(l))
    }

    /// Uma geometria fechada e cheia, pelos pontos: feita uma vez por tamanho (a geometria é da
    /// fábrica, não do alvo) e posta no lugar por transformação.
    fn figura(d2d: &ID2D1Factory, pontos: &[Vector2]) -> Result<ID2D1PathGeometry> {
        unsafe {
            let g = d2d.CreatePathGeometry()?;
            let pia = g.Open()?;
            pia.BeginFigure(pontos[0], D2D1_FIGURE_BEGIN_FILLED);
            pia.AddLines(&pontos[1..]);
            pia.EndFigure(D2D1_FIGURE_END_CLOSED);
            pia.Close()?;
            Ok(g)
        }
    }

    /// A seta laranja da linha de leitura: um triângulo com a base em x = 0 e a ponta em
    /// x = `1,3 × tamanho`, centrado em y = 0.
    fn seta(&mut self, tamanho: f32) -> Result<ID2D1PathGeometry> {
        if let Some((t, g)) = &self.seta {
            if (*t - tamanho).abs() < 0.5 {
                return Ok(g.clone());
            }
        }
        let g = Self::figura(
            &self.d2d,
            &[Vector2 { X: 0.0, Y: -tamanho }, Vector2 { X: tamanho * 1.3, Y: 0.0 }, Vector2 { X: 0.0, Y: tamanho }],
        )?;
        self.seta = Some((tamanho, g.clone()));
        Ok(g)
    }

    /// O triângulo de uma seta do enquadramento: a base em y = 0, de −`tamanho` a +`tamanho`, e a
    /// ponta para baixo em y = `1,3 × tamanho`, em x = 0.
    fn triangulo(&mut self, tamanho: f32) -> Result<ID2D1PathGeometry> {
        if let Some((t, g)) = &self.triangulo {
            if (*t - tamanho).abs() < 0.5 {
                return Ok(g.clone());
            }
        }
        let g = Self::figura(
            &self.d2d,
            &[Vector2 { X: -tamanho, Y: 0.0 }, Vector2 { X: tamanho, Y: 0.0 }, Vector2 { X: 0.0, Y: tamanho * 1.3 }],
        )?;
        self.triangulo = Some((tamanho, g.clone()));
        Ok(g)
    }

    /// Solta do cache as linhas longe da janela visível: um roteiro rolado inteiro não pode
    /// segurar dois mil layouts.
    fn podar(&mut self, visiveis: &std::ops::Range<usize>) {
        if self.cache.len() > 256 {
            let (a, b) = (visiveis.start.saturating_sub(64), visiveis.end + 64);
            self.cache.retain(|i, _| *i >= a && *i < b);
        }
    }

    /// Qual alvo do Direct2D está desenhando.
    pub fn tipo_do_alvo(&self) -> &'static str {
        if self.software {
            "software"
        } else {
            "padrão (hardware quando há)" // i18n: fora (medida e diário)
        }
    }

    /// **Um quadro.** Desenha a área do texto em `hdc`, no retângulo `area` (coordenadas do DC).
    ///
    /// Se o alvo padrão falhar (e não for o dispositivo perdido, que se refaz sozinho), o quadro é
    /// tentado de novo **uma vez** no alvo de software, e a janela fica nele: medido na Sessão 0 em
    /// 14/09, o padrão devolvia `E_HANDLE` em todo quadro. O motivo fica em
    /// [`Desenho::motivo_do_software`].
    pub fn desenhar(&mut self, hdc: HDC, area: RECT, cena: &Cena) -> std::result::Result<(), String> {
        match self.desenhar_uma_vez(hdc, area, cena) {
            Ok(()) => Ok(()),
            Err(e) if !self.software => {
                self.software = true;
                self.alvo = None;
                self.motivo_do_software = Some(e.clone());
                self.desenhar_uma_vez(hdc, area, cena).map_err(|e2| format!("{e}; e no alvo de software: {e2}")) // i18n: fora (medida e diário)
            }
            Err(e) => Err(e),
        }
    }

    fn desenhar_uma_vez(&mut self, hdc: HDC, area: RECT, cena: &Cena) -> std::result::Result<(), String> {
        let largura = (area.right - area.left).max(1) as f32;
        let altura = (area.bottom - area.top).max(1) as f32;
        let d = cena.diagramado;
        let g = &d.geometria;
        let visiveis = g.linhas_visiveis(cena.deslocamento, cena.y_leitura, f64::from(altura));
        // Os layouts das linhas saem antes do `BeginDraw`: criar recurso no meio do desenho não
        // é proibido, mas assim o custo fica separado e o `EndDraw` não espera por ele.
        let mut linhas = Vec::with_capacity(visiveis.len());
        for i in visiveis.clone() {
            if let Some(l) = self.linha(i, d).map_err(|e| format!("layout da linha {i}: {e}"))? { // i18n: fora (medida e diário)
                linhas.push((i, l));
            }
        }
        self.podar(&visiveis);
        let topo = g.topo_do_texto(cena.deslocamento, cena.y_leitura);
        let aviso = match cena.vazio {
            Some(t) if linhas.is_empty() => Some(t.encode_utf16().collect::<Vec<u16>>()),
            _ => None,
        };
        let rotulo: Option<Vec<u16>> = cena.rotulo_da_linha.map(|t| t.encode_utf16().collect());
        let rotulo_do_enquadramento: Option<(usize, Vec<u16>)> =
            cena.enquadrando.map(|(qual, t)| (qual, t.encode_utf16().collect()));
        let tamanho_da_seta = (d.altura_px * 0.28).clamp(9.0, 40.0);
        let seta = self.seta(tamanho_da_seta).map_err(|e| format!("a seta da linha: {e}"))?; // i18n: fora (medida e diário)
        let px = cena.px_por_dip.max(0.5);
        // Do tamanho das setas da linha de leitura (a regra das quatro telas depois de o usuário
        // não achar as primeiras, pequenas, no A10s).
        let tamanho_do_triangulo = tamanho_da_seta.max(9.0 * px);
        let triangulo = self.triangulo(tamanho_do_triangulo).map_err(|e| format!("a seta do enquadramento: {e}"))?; // i18n: fora (medida e diário)
        let formato_do_aviso = if aviso.is_some() || rotulo.is_some() || rotulo_do_enquadramento.is_some() {
            if self.formato_do_aviso.is_none() {
                let (f, _, _) = formato(&self.dwrite, 22.0, true).map_err(|e| format!("formato do aviso: {e}"))?; // i18n: fora (medida e diário)
                self.formato_do_aviso = Some(f);
            }
            self.formato_do_aviso.clone()
        } else {
            None
        };
        // A vista do texto (antes do espelho): as setas do enquadramento e a coluna centrada entre
        // elas. Com o diagrama atrasado (a pessoa acabou de mexer numa seta), a coluna velha fica
        // centrada no meio novo até a quebra nova chegar.
        let (esquerda, direita) = (cena.setas_px.0.clamp(0.0, largura), cena.setas_px.1.clamp(0.0, largura));
        let x_do_texto = (esquerda + direita) / 2.0 - d.largura_px / 2.0;
        let (esquerda_na_tela, direita_na_tela) = setas_na_tela((esquerda, direita), largura, cena.espelho);
        let espelho = if cena.espelho {
            // O espelho é da **vista do texto** (contrato §3): texto, faixa, setas da linha e setas
            // do enquadramento juntos, invertidos na horizontal em torno do centro da área.
            Matrix3x2 { M11: -1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: largura, M32: 0.0 }
        } else {
            Matrix3x2::identity()
        };
        let sem_pincel = None::<&ID2D1Brush>;

        let software = self.software;
        let resultado = {
            let alvo = self.alvo().map_err(|e| format!("criar o alvo ({}): {e}", if software { "software" } else { "padrão" }))?; // i18n: fora (medida e diário)
            unsafe {
                alvo.rt.BindDC(hdc, &area).map_err(|e| format!("BindDC: {e}"))?; // i18n: fora (medida e diário)
                alvo.rt.BeginDraw();
                alvo.rt.SetTransform(&Matrix3x2::identity());
                alvo.rt.Clear(Some(&PRETO as *const D2D1_COLOR_F));
                alvo.rt.SetTransform(&espelho);
                let y = cena.y_leitura as f32;
                let h = d.altura_px.max(1.0);
                alvo.rt.FillRectangle(
                    &D2D_RECT_F { left: esquerda, top: y - h / 2.0, right: direita, bottom: y + h / 2.0 },
                    &alvo.faixa,
                );
                let opcoes = D2D1_DRAW_TEXT_OPTIONS(D2D1_DRAW_TEXT_OPTIONS_NO_SNAP.0);
                for (i, l) in &linhas {
                    let y_da_linha = (topo + *i as f64 * g.altura_da_linha) as f32;
                    alvo.rt.DrawTextLayout(Vector2 { X: x_do_texto, Y: y_da_linha }, l, &alvo.texto, opcoes);
                }
                // **As duas setas laranjas** da linha de leitura, nas bordas do enquadramento,
                // apontando para dentro — é por elas (e pela faixa) que a pessoa arrasta a linha até
                // a altura dos olhos na câmera. Nas bordas do enquadramento, e não nas da janela: é
                // a área que o vidro mostra.
                let em = |m: Matrix3x2| m * espelho;
                alvo.rt.SetTransform(&em(Matrix3x2::translation(esquerda + 4.0, y)));
                alvo.rt.FillGeometry(&seta, &alvo.marcador, sem_pincel);
                alvo.rt.SetTransform(&em(Matrix3x2 { M11: -1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: direita - 4.0, M32: y }));
                alvo.rt.FillGeometry(&seta, &alvo.marcador, sem_pincel);
                // **As setas do enquadramento**: um triângulo azul no alto de cada borda, apontando
                // para baixo, **inteiro à vista** mesmo no padrão (nas bordas da janela ele entra o
                // bastante), e uma guia vertical fina em cada borda enquanto as faixas estão à
                // mostra e durante o arrasto — a da seta arrastada, mais forte. A área do texto
                // começa abaixo da faixa de cima: nada as cobre.
                for (qual, x) in [(0usize, esquerda), (1usize, direita)] {
                    // (`max`/`min`, e não `clamp`: numa área mais estreita que o triângulo, o
                    // `clamp` com o mínimo acima do máximo entra em pânico.)
                    let x_marca = x.min(largura - tamanho_do_triangulo - px).max(tamanho_do_triangulo + px);
                    let arrastada = rotulo_do_enquadramento.as_ref().is_some_and(|(q, _)| *q == qual);
                    if cena.guias || arrastada {
                        let x_guia = x.min(largura - 0.5 * px).max(0.5 * px);
                        alvo.rt.SetTransform(&espelho);
                        alvo.rt.DrawLine(
                            Vector2 { X: x_guia, Y: 0.0 },
                            Vector2 { X: x_guia, Y: altura },
                            if arrastada { &alvo.enquadrador } else { &alvo.guia },
                            if arrastada { 1.5 * px } else { px },
                            None::<&ID2D1StrokeStyle>,
                        );
                    }
                    if cena.marcas_no_pe {
                        // Virado para cima, encostado no pé da área.
                        alvo.rt.SetTransform(&em(Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: -1.0, M31: x_marca, M32: altura - 3.0 * px }));
                    } else {
                        alvo.rt.SetTransform(&em(Matrix3x2::translation(x_marca, 3.0 * px)));
                    }
                    alvo.rt.FillGeometry(&triangulo, &alvo.enquadrador, sem_pincel);
                }
                // Os rótulos saem **fora** do espelho: são para quem mexe na tela, e não para o
                // vidro.
                alvo.rt.SetTransform(&Matrix3x2::identity());
                if let (Some(t), Some(f)) = (&aviso, &formato_do_aviso) {
                    let caixa = D2D_RECT_F {
                        left: esquerda_na_tela + 24.0,
                        top: y + h,
                        right: (direita_na_tela - 24.0).max(esquerda_na_tela + 48.0),
                        bottom: altura,
                    };
                    alvo.rt.DrawText(t, f, &caixa, &alvo.fraco, D2D1_DRAW_TEXT_OPTIONS(0), Default::default());
                }
                if let (Some(t), Some(f)) = (&rotulo, &formato_do_aviso) {
                    // O valor em %, acima da seta da direita (na tela), enquanto a pessoa arrasta.
                    let topo = (y - h / 2.0 - 40.0).max(0.0);
                    let direita_do_rotulo = (direita_na_tela - 12.0).max(372.0).min(largura);
                    let caixa = D2D_RECT_F { left: direita_do_rotulo - 360.0, top: topo, right: direita_do_rotulo, bottom: topo + 36.0 };
                    alvo.rt.DrawText(t, f, &caixa, &alvo.marcador, D2D1_DRAW_TEXT_OPTIONS(0), Default::default());
                }
                if let (Some((qual, t)), Some(f)) = (&rotulo_do_enquadramento, &formato_do_aviso) {
                    // O valor em %, ao lado do triângulo que se arrasta, do lado de dentro.
                    let x_logico = if *qual == 0 { esquerda } else { direita };
                    let x_tela = if cena.espelho { largura - x_logico } else { x_logico };
                    let topo = 3.0 * px + 14.0 * px;
                    let caixa = if x_tela < largura / 2.0 {
                        D2D_RECT_F { left: x_tela + 16.0 * px, top: topo, right: x_tela + 16.0 * px + 320.0, bottom: topo + 36.0 }
                    } else {
                        D2D_RECT_F { left: x_tela - 16.0 * px - 320.0, top: topo, right: x_tela - 16.0 * px, bottom: topo + 36.0 }
                    };
                    alvo.rt.DrawText(t, f, &caixa, &alvo.enquadrador, D2D1_DRAW_TEXT_OPTIONS(0), Default::default());
                }
                alvo.rt.SetTransform(&Matrix3x2::identity());
                alvo.rt.EndDraw(None, None)
            }
        };
        if let Err(e) = resultado {
            if e.code().0 == D2DERR_RECREATE_TARGET {
                // O próximo quadro refaz o alvo e os pincéis; este se perde, e não é erro.
                self.alvo = None;
                return Ok(());
            }
            return Err(format!("EndDraw: {e}")); // i18n: fora (medida e diário)
        }
        Ok(())
    }
}
