//! A câmera como origem: **a parte que é aritmética** (`docs/camera-no-windows.md` §3.2, §3.3 e §5).
//!
//! Aqui não há Win32: é o que decide, a partir do que a câmera declara e do que chega dela, **que
//! tipo nativo pedir**, **que instante o quadro leva**, **se ele entra** e **se a câmera parou**. A
//! captura (`captura_de_camera.rs`) só executa. Separado pelo mesmo motivo de `catalogo_de_cameras.rs`:
//! uma regra que só roda com uma webcam na mão nunca é exercitada, e as duas regras de baixo são as
//! que a revisão adversarial de 18/09 achou erradas no desenho (M7 e M2).

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::idioma::{t, tf};

// =============================================================================================
// O tipo nativo
// =============================================================================================

/// O formato de pixel de um tipo que a câmera declara, no que importa aqui.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subtipo {
    Nv12,
    Yuy2,
    /// JPEG por quadro: o leitor insere o decodificador do Media Foundation.
    Mjpg,
    /// Três planos (Y, U, V) em memória: o único tipo da Canon pelo EOS Webcam Utility (a fase 5,
    /// passo 1: I420 1280×720 @30, sem faixa declarada). Não há textura DXGI I420: o leitor entrega
    /// o nativo em memória, e a cópia para o anel entrelaça U e V num NV12 ([`entrelacar_uv`]).
    I420,
    /// **DV** de definição padrão (`dvsd`, `dv25`, `dvsl`; o `dv50` e o HD ficam em `Outro` até alguém
    /// medir): o modo DV da Panasonic pelo USB (a
    /// fase 5: `dvsd` 720×480 @29,97). É compactado: o leitor insere o decodificador de DV do Windows
    /// (`mfdvdec.dll`) quando se pede YUY2 ou NV12, como faz com o MJPG, **sem** o processador
    /// avançado (o carimbo não muda de regime, M9). E é **entrelaçado** ([`entrelacamento`]).
    Dv,
    /// Qualquer outro (RGB24, H.264 da própria câmera…): fora da escolha.
    Outro,
}

impl Subtipo {
    /// A preferência entre formatos, só como desempate: o NV12 entra direto no encoder, o YUY2 pede
    /// uma conversão na GPU, o MJPG pede o decodificador **e** a conversão, o I420 sobe pela memória
    /// com o croma entrelaçado na CPU, e o DV pede o decodificador, a conversão **e** desentrelaçar.
    fn posto(self) -> u8 {
        match self {
            Subtipo::Nv12 => 5,
            Subtipo::Yuy2 => 4,
            Subtipo::Mjpg => 3,
            Subtipo::I420 => 2,
            Subtipo::Dv => 1,
            Subtipo::Outro => 0,
        }
    }
}

/// **Os campos de um quadro entrelaçado**: qual vem primeiro no tempo. O conversor desentrelaça
/// (`conversor_de_camera.rs`) e o quadro que sai é progressivo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entrelacamento {
    Progressivo,
    /// O campo de cima (as linhas pares, contando de 0) primeiro.
    CampoDeCimaPrimeiro,
    /// O campo de baixo (as linhas ímpares) primeiro.
    CampoDeBaixoPrimeiro,
}

/// `MFVideoInterlaceMode`, os valores que importam (documentação oficial do enum).
pub const INTERLACE_PROGRESSIVO: u32 = 2;
pub const INTERLACE_CIMA_PRIMEIRO: u32 = 3;
pub const INTERLACE_BAIXO_PRIMEIRO: u32 = 4;
/// O modo misto: cada amostra diz se é entrelaçada (`MFSampleExtension_Interlaced`) e a ordem
/// (`MFSampleExtension_BottomFieldFirst`). É o que a saída do decodificador de DV declara (M84).
pub const INTERLACE_MISTO: u32 = 7;

/// O que a primeira amostra diz dos campos (`MFSampleExtension_Interlaced` e
/// `MFSampleExtension_BottomFieldFirst`); `None` quando ela não traz o atributo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmostraDiz {
    pub entrelacada: Option<bool>,
    pub campo_de_baixo_primeiro: Option<bool>,
}

/// **O quadro é entrelaçado, e em que ordem?**
///
/// - **DV de definição padrão (480 ou 576 linhas) é campo de baixo primeiro, diga o tipo o que
///   disser**: a Panasonic no modo DV declara `MF_MT_INTERLACE_MODE = 2` (progressivo) no `dvsd`
///   720×480 (a fase 5, passo 1), e o DV-SD é entrelaçado com o campo de baixo primeiro. De onde:
///   conhecimento geral do formato (IEC 61834 e SMPTE 314M, o DV25 dos camcorders MiniDV; o
///   decodificador de DV do FFmpeg marca `top_field_first = 0`), **não** lido de documento deste
///   repositório nem medido: a prova é o Bruno olhar a janela com e sem.
/// - Fora do DV, **o tipo e a amostra**, cada um no seu papel (a revisão curta do `08af2cd`, A8,
///   corrigindo o L3 da revisão da fase 5):
///   - 3 (campo de cima primeiro) e 4 (campo de baixo) são modos **fixos**: o tipo manda, e a
///     amostra só veta com a contradição explícita (`Interlaced = 0`). A primeira versão exigia
///     `Interlaced = 1`, e uma placa que o declarasse só no tipo, que é o uso previsto, sairia com o
///     pente;
///   - 7 (misto): **a amostra decide**, `Interlaced = 1` com a ordem de `BottomFieldFirst` (sem ele,
///     o campo de cima, o padrão do Media Foundation);
///   - 2, nada ou os de campo único: progressivo — desentrelaçar uma webcam progressiva só apagaria
///     detalhe.
///
/// `altura` é a do **quadro** (a revisão, L2: um decodificador de DV que declare uma abertura de
/// 476 linhas não pode tirar o DV da regra).
pub fn entrelacamento(nativo: Subtipo, altura: u32, modo_declarado: Option<u32>, amostra: AmostraDiz) -> Entrelacamento {
    if nativo == Subtipo::Dv && (altura == 480 || altura == 576) {
        return Entrelacamento::CampoDeBaixoPrimeiro;
    }
    match modo_declarado {
        Some(INTERLACE_CIMA_PRIMEIRO | INTERLACE_BAIXO_PRIMEIRO) if amostra.entrelacada == Some(false) => {
            Entrelacamento::Progressivo
        }
        Some(INTERLACE_CIMA_PRIMEIRO) => Entrelacamento::CampoDeCimaPrimeiro,
        Some(INTERLACE_BAIXO_PRIMEIRO) => Entrelacamento::CampoDeBaixoPrimeiro,
        Some(INTERLACE_MISTO) if amostra.entrelacada == Some(true) => {
            if amostra.campo_de_baixo_primeiro == Some(true) {
                Entrelacamento::CampoDeBaixoPrimeiro
            } else {
                Entrelacamento::CampoDeCimaPrimeiro
            }
        }
        _ => Entrelacamento::Progressivo,
    }
}

/// Um tipo nativo da câmera: formato, tamanho e taxa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TipoNativo {
    pub subtipo: Subtipo,
    pub largura: u32,
    pub altura: u32,
    pub fps_num: u32,
    pub fps_den: u32,
}

impl TipoNativo {
    pub fn fps(&self) -> f64 {
        if self.fps_den == 0 {
            0.0
        } else {
            f64::from(self.fps_num) / f64::from(self.fps_den)
        }
    }

    /// A área em macroblocos de 16 × 16, a unidade do teto do núcleo (`quall_core::teto`).
    pub fn macroblocos(&self) -> u32 {
        self.largura.div_ceil(16) * self.altura.div_ceil(16)
    }

    pub fn descricao(&self) -> String {
        format!("{:?} {}x{} @{:.2}", self.subtipo, self.largura, self.altura, self.fps())
    }
}

/// O que o teto do núcleo deixa passar: a área (em macroblocos) e o fps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TetoDaCamera {
    pub max_macroblocos: u32,
    pub fps: u32,
}

/// **O fps que sai de fato** de uma câmera a `fps` num teto de `teto_fps`, depois do ritmo de
/// [`entregar`]: ele deixa passar um quadro a cada `k = ⌈0,75 · fps / teto⌉`. Uma câmera de 60 fps
/// num teto de 30 dá 30; uma de 50, 25; uma de 30 num teto de 24 continua 30 (e passa do teto).
pub fn fps_efetivo(fps: f64, teto_fps: u32) -> f64 {
    if fps <= 0.0 {
        return 0.0;
    }
    let k = (0.75 * fps / f64::from(teto_fps.max(1))).ceil().max(1.0);
    fps / k
}

/// **A escolha do tipo nativo, na ordem da revisão (M7), com o andar de baixo consertado.**
///
/// A primeira versão ordenava pelo subtipo primeiro, e isso escolhe 5 fps: YUY2 1080p a 30 fps
/// são ~124 MB/s contra ~24 MB/s de isócrono no USB 2, então toda webcam USB 2 oferece YUY2 1080p a
/// no máximo ~5 fps, e "NV12, depois YUY2" pegaria esse. O fps que conta é o **efetivo**
/// ([`fps_efetivo`]): um tipo de 60 fps num teto de 30 sai a 30 pelo ritmo, e não é excluído (a
/// revisão do código da fase 3, m4). A ordem, do que pesa mais para o que pesa menos:
///
/// 1. **não passar do fps do teto** depois do ritmo;
/// 2. **ser fluido**: pelo menos 30 fps (ou o fps do teto, se ele for menor);
/// 3. **fora do andar fluido, o fps antes da área**: sem nenhum tipo a 30, NV12 720p a 15 fps ganha
///    de YUY2 1080p a 5 (a versão anterior escolhia o de 5 fps: o M7 no andar de baixo, m4);
/// 4. **caber no teto** sem redução;
/// 5. a **maior área** entre os que cabem (e a menor entre os que não cabem: a mais perto do teto);
/// 6. o **maior fps** efetivo que resta;
/// 7. **sem dizimar**: entre iguais, o tipo que já vem no fps do teto;
/// 8. o subtipo: NV12, YUY2, MJPG, I420, DV (`Subtipo::posto`).
///
/// `None` só quando a câmera não declara nenhum desses formatos com tamanho (o `dvsd` sem tamanho da
/// Panasonic cai pelo filtro de tamanho).
pub fn escolher_tipo_nativo(tipos: &[TipoNativo], teto: TetoDaCamera) -> Option<usize> {
    let fluido = f64::from(teto.fps.min(30)) - 0.5;
    let limite = f64::from(teto.fps) + 0.5;
    let chave = |t: &TipoNativo| {
        let efetivo = fps_efetivo(t.fps(), teto.fps);
        let milifps = (efetivo * 1000.0).round() as u64;
        let nao_passa = efetivo <= limite;
        let e_fluido = efetivo >= fluido;
        // Em passos de meio fps: 15000/1001 e 15 são o mesmo andar, e a área decide entre eles (a
        // reconferência da fase 3).
        let fps_antes_da_area = if e_fluido { 0 } else { (efetivo * 2.0).round() as u64 };
        let cabe = t.macroblocos() <= teto.max_macroblocos;
        let area = u64::from(t.largura) * u64::from(t.altura);
        let area_na_ordem = if cabe { area } else { u64::MAX - area };
        let sem_dizimar = t.fps() <= limite;
        (nao_passa, e_fluido, fps_antes_da_area, cabe, area_na_ordem, milifps, sem_dizimar, t.subtipo.posto())
    };
    tipos
        .iter()
        .enumerate()
        .filter(|(_, t)| t.subtipo != Subtipo::Outro && t.largura > 0 && t.altura > 0)
        // `max_by` devolve o **último** entre iguais; a comparação invertida no índice devolve o
        // primeiro, que é o que a câmera declarou antes.
        .max_by(|(ia, a), (ib, b)| chave(a).cmp(&chave(b)).then(ib.cmp(ia)))
        .map(|(i, _)| i)
}

// =============================================================================================
// A geometria
// =============================================================================================

/// **O que o quadro tem, e o que dele é imagem** (a revisão do código da fase 3, M3). Um
/// decodificador de hardware pode entregar 1920×1088 com 1080 linhas de imagem: o tipo diz o
/// quadro inteiro em `MF_MT_FRAME_SIZE` e a imagem em `MF_MT_MINIMUM_DISPLAY_APERTURE`. O anel, o
/// conversor e o encoder ficam do tamanho da **abertura**, e a cópia leva só o retângulo dela.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometria {
    /// O quadro inteiro, como o tipo declara (`MF_MT_FRAME_SIZE`).
    pub quadro_largura: u32,
    pub quadro_altura: u32,
    /// O retângulo da imagem dentro dele, com coordenadas e tamanhos pares (NV12).
    pub x: u32,
    pub y: u32,
    pub largura: u32,
    pub altura: u32,
}

impl Geometria {
    /// O quadro inteiro é imagem.
    pub fn inteira(largura: u32, altura: u32) -> Self {
        Geometria { quadro_largura: largura, quadro_altura: altura, x: 0, y: 0, largura: largura & !1, altura: altura & !1 }
    }

    /// A mesma imagem, num quadro do tamanho da **superfície**: o plano UV de uma superfície travada
    /// pela memória começa depois das linhas dela, e não das do tipo (a reconferência da fase 3).
    pub fn na_superficie(&self, largura: u32, altura: u32) -> Geometria {
        Geometria { quadro_largura: largura, quadro_altura: altura, ..*self }
    }

    pub fn recortada(&self) -> bool {
        (self.x, self.y, self.largura, self.altura) != (0, 0, self.quadro_largura, self.quadro_altura)
    }

    pub fn descricao(&self) -> String {
        if self.recortada() {
            format!(
                "{}x{} de um quadro {}x{} (abertura em {},{})", // i18n: fora (diário e detalhe técnico)
                self.largura, self.altura, self.quadro_largura, self.quadro_altura, self.x, self.y
            )
        } else {
            format!("{}x{}", self.largura, self.altura)
        }
    }
}

/// O `MFVideoArea` de um blob de atributo (16 bytes): `OffsetX` e `OffsetY` como `MFOffset`
/// (`fract: u16`, `value: i16`), e `Area` como `SIZE` (`cx`, `cy`: `i32`). Devolve `(x, y, cx, cy)`;
/// a parte fracionária do deslocamento é ignorada.
pub fn abertura_do_blob(blob: &[u8]) -> Option<(i32, i32, i32, i32)> {
    if blob.len() < 16 {
        return None;
    }
    let i16_em = |i: usize| i16::from_le_bytes([blob[i], blob[i + 1]]);
    let i32_em = |i: usize| i32::from_le_bytes([blob[i], blob[i + 1], blob[i + 2], blob[i + 3]]);
    Some((i32::from(i16_em(2)), i32::from(i16_em(6)), i32_em(8), i32_em(12)))
}

/// A geometria de um tipo: a abertura, quando ela é válida (dentro do quadro, e com pelo menos
/// 16×16 depois de levada a coordenadas pares, para dentro); senão o quadro inteiro.
pub fn geometria(quadro_largura: u32, quadro_altura: u32, abertura: Option<(i32, i32, i32, i32)>) -> Geometria {
    let inteira = Geometria::inteira(quadro_largura, quadro_altura);
    let Some((x, y, cx, cy)) = abertura else {
        return inteira;
    };
    let (ql, qa) = (i64::from(quadro_largura), i64::from(quadro_altura));
    let (x, y, cx, cy) = (i64::from(x), i64::from(y), i64::from(cx), i64::from(cy));
    if x < 0 || y < 0 || cx <= 0 || cy <= 0 || x + cx > ql || y + cy > qa {
        return inteira;
    }
    let par_acima = |v: i64| (v + 1) & !1;
    let par_abaixo = |v: i64| v & !1;
    let (x0, y0, x1, y1) = (par_acima(x), par_acima(y), par_abaixo(x + cx), par_abaixo(y + cy));
    if x1 - x0 < 16 || y1 - y0 < 16 {
        return inteira;
    }
    Geometria {
        quadro_largura,
        quadro_altura,
        x: x0 as u32,
        y: y0 as u32,
        largura: (x1 - x0) as u32,
        altura: (y1 - y0) as u32,
    }
}

/// Quanto a superfície pode passar do quadro que o tipo declara: o alinhamento de um decodificador
/// (1088 para 1080, 736 para 720). Acima disso não é alinhamento, é um tamanho que mudou sem
/// `CURRENTMEDIATYPECHANGED`, e a cópia recortaria o canto de cima da imagem nova, calada (a
/// reconferência da fase 3).
pub const ALINHAMENTO_DA_SUPERFICIE: u32 = 64;

fn alinhar(v: u32, a: u32) -> u32 {
    v.div_ceil(a) * a
}

/// A superfície do leitor serve para a imagem `g`? `Ok(recorta)`: serve, e `recorta` diz que a cópia
/// não leva a superfície inteira (para o registro dizer uma vez). `Err` quando ela é menor que a
/// imagem, ou maior que o quadro alinhado a [`ALINHAMENTO_DA_SUPERFICIE`].
pub fn superficie_aceita(largura: u32, altura: u32, g: &Geometria) -> Result<bool, String> {
    let (x1, y1) = (g.x + g.largura, g.y + g.altura);
    if largura < x1 || altura < y1 {
        return Err(format!("a superfície {largura}x{altura} é menor que a imagem, que vai até {x1}x{y1}")); // i18n: fora (diário e detalhe técnico)
    }
    let limite_l = alinhar(g.quadro_largura.max(x1), ALINHAMENTO_DA_SUPERFICIE);
    let limite_a = alinhar(g.quadro_altura.max(y1), ALINHAMENTO_DA_SUPERFICIE);
    if largura > limite_l || altura > limite_a {
        return Err(format!(
            "a superfície {largura}x{altura} passa do quadro {}x{} alinhado a {ALINHAMENTO_DA_SUPERFICIE} ({limite_l}x{limite_a}): o tamanho mudou sem aviso", // i18n: fora (diário e detalhe técnico)
            g.quadro_largura, g.quadro_altura
        ));
    }
    Ok((g.x, g.y, x1, y1) != (0, 0, largura, altura))
}

/// Quantas cópias para o anel falhando **em seguida** acabam a captura, com o motivo: ~1 s a 30
/// fps. Antes disso a sessão ficava "Transmitindo" sem quadro nenhum, e a única pista era um
/// contador no relato do fim (a revisão do código da fase 3, M3).
pub const COPIAS_FALHAS_PARA_PARAR: u32 = 30;

// =============================================================================================
// A cor
// =============================================================================================

/// A faixa da entrada é completa (0–255)? O que o tipo declara manda; sem declaração, **completa
/// só para o que saiu do decodificador MJPEG** (JPEG é faixa completa por natureza) e limitada para
/// NV12, YUY2 e I420 vindos da câmera. É suposição (§3.3). A fase 5 viu as duas webcams UVC
/// declararem (a integrada: NV12 e MJPG 0-255, YUY2 16-235; a Panasonic: 0-255), e a Canon (I420)
/// não declarar nada: limitada é o padrão do Media Foundation para YUV sem
/// `MF_MT_VIDEO_NOMINAL_RANGE`, e é como os outros apps que leem a mesma fonte a mostram.
pub fn faixa_completa(nativo: Subtipo, declarada_completa: Option<bool>) -> bool {
    declarada_completa.unwrap_or(nativo == Subtipo::Mjpg)
}

/// **O leitor leva o gerenciador D3D?** Pelo tipo que a abertura vai usar (a fase 5, p9): com o
/// gerenciador, a fonte da Canon (I420) fez o primeiro `ReadSample` devolver `0xC00D36B4`
/// (`MF_E_INVALIDMEDIATYPE`), com e sem o tipo nativo devolvido como veio; sem ele, o quadro veio em
/// memória. O I420 não tem textura DXGI e sobe para o anel pela CPU de qualquer jeito
/// (`captura_de_camera::subir_da_memoria`), então o gerenciador não tem o que dar a ele. Os outros
/// continuam com o gerenciador: o NV12 e o YUY2 chegam como superfície da placa do encoder, e o MJPEG
/// e o DV passam pelo decodificador de hardware.
///
/// **O DV com o adapt2 (22/09) também vai sem o gerenciador** (`dv_pela_memoria`): o decodificador
/// de DV da Microsoft é de software, e sem o gerenciador o YUY2 vem em memória, onde o
/// desentrelaçador da CPU (`desentrelacador.rs`) roda na cópia para o anel
/// (`captura_de_camera::subir_da_memoria`). Com o gerenciador, o leitor subia o quadro para a GPU e o
/// processador de vídeo fazia o bob. Medido sem câmera: o leitor do Media Foundation sem o
/// gerenciador, sobre o DV da Panasonic num AVI, entrega o YUY2 com `Interlaced=1
/// BottomFieldFirst=1` (a sonda `desentrelacar --entrada x.avi`).
pub fn leitor_com_d3d(nativo: Subtipo, dv_pela_memoria: bool) -> bool {
    match nativo {
        Subtipo::I420 => false,
        Subtipo::Dv => !dv_pela_memoria,
        _ => true,
    }
}

/// **Refazer a abertura do DV com o gerenciador D3D?** (22/09) Só quando a tentativa com o leitor sem
/// o gerenciador (o adapt2) falhou por `Outra` (um `HRESULT` sem nome, o tipo recusado): outra
/// tentativa com o gerenciador pode dar outra resposta, e ela é rápida. **`Demorou` não refaz** (a
/// revisão do código, achado 1): seriam mais 5 s de espera e uma fonte nova pelo link (4,6–5,1 s,
/// M41) antes do recuo para a compartilhada, que passaria de 10 s; o recuo de modo segue como era,
/// e a tentativa seguinte já vai com o gerenciador (a captura lembra, no processo, que o DV sem ele
/// falhou). A câmera ocupada, negada, removida ou o Parar dariam o mesmo com o gerenciador.
pub fn refazer_o_dv_com_d3d(causa: CausaDaFalha) -> bool {
    matches!(causa, CausaDaFalha::Outra)
}

/// Se a falha do leitor do DV sem o gerenciador fica **lembrada no processo** (as sessões seguintes
/// vão direto ao bob). Só `Outra`, o erro que o leitor devolveu. **`Demorou` não** (23/09, com a
/// Panasonic no Dell): com a fita parada a câmera não manda quadro nenhum, e a tentativa com o
/// gerenciador também demorou (controladora e compartilhada, 5 s cada); lembrar disso punha o bob em
/// todas as sessões do processo, com a fita já tocando. Uma câmera muda não acusa o leitor.
pub fn lembrar_que_o_dv_sem_d3d_falhou(causa: CausaDaFalha) -> bool {
    matches!(causa, CausaDaFalha::Outra)
}

/// **Quem desentrelaça o quadro da câmera** (22/09): o adapt2 na CPU, o bob do processador de vídeo
/// (o conversor), ou ninguém.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuemDesentrelaca {
    /// O quadro é progressivo, ou a bancada pediu `--sem-desentrelacar`.
    Ninguem,
    /// O conversor, com o índice do bob, sem quadros de referência (a fase 5).
    Processador(Entrelacamento),
    /// O adapt2 na cópia para o anel; o conversor recebe o quadro já progressivo.
    Cpu(Entrelacamento),
}

impl QuemDesentrelaca {
    /// O entrelaçamento **do quadro que fica no anel**: é o que o conversor tem de desentrelaçar.
    /// Com o adapt2, progressivo — senão o processador faria o bob de novo sobre a saída dele.
    pub fn no_anel(self) -> Entrelacamento {
        match self {
            QuemDesentrelaca::Processador(e) => e,
            QuemDesentrelaca::Ninguem | QuemDesentrelaca::Cpu(_) => Entrelacamento::Progressivo,
        }
    }
}

/// **Quem desentrelaça**, decidido depois do primeiro quadro, com o que a abertura sabe:
///
/// - `entrelacamento`: o que a regra decidiu (já `Progressivo` com o `--sem-desentrelacar`);
/// - `adapt2_pedido`: o `--desentrelacador` (o padrão é o adapt2);
/// - `pela_memoria`: o leitor nasceu **sem** o gerenciador D3D — só então o quadro passa pela CPU
///   na cópia para o anel. Com o gerenciador, a superfície é copiada na GPU, e o bob fica;
/// - `yuy2`: o formato do anel (o adapt2 só lê YUY2, o que o decodificador de DV entrega);
/// - `imagem_y`, `imagem_largura` e `imagem_altura`: a abertura dentro do quadro. O adapt2 conta os
///   campos a partir da primeira linha da imagem: com `y` ímpar a paridade dos campos se inverteria,
///   e com altura ímpar ou curta, ou largura ímpar (o par YUY2), ele não serve. Nesses casos, o bob.
///
/// O adapt2 nunca é condição para a câmera abrir: fora dessas condições, o bob de antes.
pub fn quem_desentrelaca(
    entrelacamento: Entrelacamento,
    adapt2_pedido: bool,
    pela_memoria: bool,
    yuy2: bool,
    imagem_y: u32,
    imagem_largura: u32,
    imagem_altura: u32,
) -> QuemDesentrelaca {
    if entrelacamento == Entrelacamento::Progressivo {
        return QuemDesentrelaca::Ninguem;
    }
    let geometria_serve = imagem_y % 2 == 0 && imagem_altura % 2 == 0 && imagem_altura >= 4 && imagem_largura % 2 == 0 && imagem_largura >= 2;
    if adapt2_pedido && pela_memoria && yuy2 && geometria_serve {
        QuemDesentrelaca::Cpu(entrelacamento)
    } else {
        QuemDesentrelaca::Processador(entrelacamento)
    }
}

/// O leitor que foi criado serve para o tipo em uso?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeitorServe {
    Sim,
    /// O leitor nasceu sem o gerenciador D3D e o tipo em uso é de GPU (o `SetNativeMediaType`
    /// recusado): serve, e o quadro sobe pela memória (`subir_da_memoria` aceita NV12 e YUY2).
    PelaMemoria,
    /// O leitor tem o gerenciador e o tipo em uso é o I420: o primeiro quadro daria o `0xC00D36B4`
    /// da Canon (p9).
    Nao,
}

/// **O leitor serve para o tipo em uso?** (a revisão do código da fase 5, L6, e a revisão curta do
/// `08af2cd`, A7). O defeito conhecido é um só: o gerenciador D3D com o I420 (p9). O outro sentido —
/// o leitor sem D3D e um NV12, YUY2 ou MJPG em uso — funcionava pela memória, e a primeira versão da
/// checagem o recusava.
///
/// O DV com o gerenciador (o descritor não disse o tipo, e o leitor nasceu com ele) serve: o quadro
/// vem pela GPU e o desentrelaçamento fica no bob do processador (`quem_desentrelaca`).
pub fn leitor_serve(com_d3d: bool, em_uso: Subtipo, dv_pela_memoria: bool) -> LeitorServe {
    match (com_d3d, em_uso) {
        (true, Subtipo::I420) => LeitorServe::Nao,
        (false, s) if leitor_com_d3d(s, dv_pela_memoria) => LeitorServe::PelaMemoria,
        _ => LeitorServe::Sim,
    }
}

// =============================================================================================
// O aspecto: a razão de pixel (a fase 5, o DV em 16:9)
// =============================================================================================

/// **A razão de pixel** (PAR, `MF_MT_PIXEL_ASPECT_RATIO`): a largura de um pixel sobre a altura.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Par {
    pub num: u32,
    pub den: u32,
}

impl Par {
    pub const QUADRADA: Par = Par { num: 1, den: 1 };

    /// Do `UINT64` do Media Foundation (numerador nos 32 bits de cima).
    pub fn do_mf(v: u64) -> Par {
        Par { num: (v >> 32) as u32, den: v as u32 }
    }

    /// Quadrada, ou sem sentido (um zero): o pixel sai como veio.
    pub fn quadrada(&self) -> bool {
        self.num == 0 || self.den == 0 || self.num == self.den
    }

    /// **Plausível**: entre 1:3 e 3:1 (a revisão curta do `08af2cd`, A5). Uma câmera virtual ruim que
    /// declare 1000:1 criaria uma imagem de 720.000 pixels de largura, e `u32::MAX`:1 estouraria as
    /// contas do tamanho e do teto. As PAR de verdade ficam longe dos limites: a do DV 16:9 é 32:27
    /// (1,19), a do DV 4:3 é 8:9 (0,89), a do HDV é 4:3 (1,33).
    pub fn plausivel(&self) -> bool {
        let (n, d) = (u64::from(self.num), u64::from(self.den));
        !self.quadrada() && n <= 3 * d && d <= 3 * n
    }
}

/// De onde veio a PAR que a câmera usa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrigemDaPar {
    /// O tipo de saída do leitor declara (o decodificador pode corrigir o nativo).
    DeclaradaNaSaida,
    /// O tipo nativo declara.
    DeclaradaNoNativo,
    /// DV-SD, pelo campo DISP do pacote VAUX de controle (`MF_MT_DV_VAUX_CTRL_PACK`).
    DvPeloVaux,
    /// DV-SD sem o pacote VAUX: 4:3 (o DV-SD nunca é de pixel quadrado).
    DvSemVaux,
    /// Ninguém declara: pixel quadrado, como antes da fase 5.
    Quadrada,
    /// A declarada estava fora de 1:3 a 3:1 ([`Par::plausivel`]) e foi ignorada: pixel quadrado. A
    /// PAR recusada fica aqui, para o registro.
    Absurda(Par),
}

/// O aspecto da câmera: a PAR e de onde ela veio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aspecto {
    pub par: Par,
    pub origem: OrigemDaPar,
}

/// **O DV em 16:9, pelo pacote VAUX de controle** (`MF_MT_DV_VAUX_CTRL_PACK`, os bytes PC1–PC4 do
/// pacote 0x61 num `UINT32`, PC1 no byte de baixo, como no `DVINFO` do DirectShow): o campo DISP
/// são os 3 bits de baixo do PC2. 2 e 7 são 16:9 — a regra do demultiplexador de DV do FFmpeg
/// (`dv_extract_video_info`), **não conferida** contra o que a Panasonic declara: o roteiro do
/// aspecto (`quall_camera_local aspecto`) imprime o pacote nos dois modos.
pub fn dv_em_16_9(vaux_ctrl: u32) -> bool {
    matches!((vaux_ctrl >> 8) & 0x7, 2 | 7)
}

/// **A PAR da câmera**, na ordem:
/// 1. a que o tipo de saída declara, se não for quadrada;
/// 2. a que o nativo declara, se não for quadrada;
/// 3. DV de definição padrão (720×480 ou 720×576), que **nunca** é de pixel quadrado — a Panasonic
///    declara 1:1 no `dvsd` (a fase 5, passo 1) —: 16:9 ou 4:3 pelo VAUX, e 4:3 sem ele. As PAR
///    são as do FFmpeg para a largura inteira de 720 (`dv_profiles`: 8:9 e 32:27 no 525/60, 16:15
///    e 64:45 no 625/50), que dão 640 e ~854 de largura a 480 linhas;
/// 4. quadrada: a fonte que não declara nada fica como hoje.
///
/// Uma declarada fora de 1:3 a 3:1 é ignorada (a revisão curta, A5): a regra segue para a próxima,
/// e, se nenhuma decidir, a origem é `Absurda`, com a PAR recusada, e o pixel fica quadrado.
pub fn aspecto_da_camera(
    nativo: Subtipo,
    largura: u32,
    altura: u32,
    declarada_na_saida: Option<Par>,
    declarada_no_nativo: Option<Par>,
    vaux_ctrl: Option<u32>,
) -> Aspecto {
    let absurda = [declarada_na_saida, declarada_no_nativo].into_iter().flatten().find(|p| !p.quadrada() && !p.plausivel());
    if let Some(p) = declarada_na_saida.filter(Par::plausivel) {
        return Aspecto { par: p, origem: OrigemDaPar::DeclaradaNaSaida };
    }
    if let Some(p) = declarada_no_nativo.filter(Par::plausivel) {
        return Aspecto { par: p, origem: OrigemDaPar::DeclaradaNoNativo };
    }
    if nativo == Subtipo::Dv && largura == 720 && (altura == 480 || altura == 576) {
        let dezesseis_por_nove = vaux_ctrl.is_some_and(dv_em_16_9);
        let par = match (altura, dezesseis_por_nove) {
            (480, false) => Par { num: 8, den: 9 },
            (480, true) => Par { num: 32, den: 27 },
            (_, false) => Par { num: 16, den: 15 },
            (_, true) => Par { num: 64, den: 45 },
        };
        let origem = if vaux_ctrl.is_some() { OrigemDaPar::DvPeloVaux } else { OrigemDaPar::DvSemVaux };
        return Aspecto { par, origem };
    }
    match absurda {
        Some(p) => Aspecto { par: Par::QUADRADA, origem: OrigemDaPar::Absurda(p) },
        None => Aspecto { par: Par::QUADRADA, origem: OrigemDaPar::Quadrada },
    }
}

/// **O aspecto da sessão, entre a abertura e as releituras** (a revisão curta do `08af2cd`, A4).
/// A abertura e a thread do Media Foundation (a troca de tipo) escrevem o mesmo aspecto, e nenhuma
/// das duas o calcula com a trava tomada: o cálculo chama o leitor, e a thread do leitor pode estar
/// esperando a trava. A ordem que não perde a troca:
/// 1. a abertura publica a **base** (o subtipo e a geometria) **antes** de calcular: toda troca
///    depois disso é relida;
/// 2. a abertura calcula e **fixa só se ninguém fixou** ([`fixar_na_abertura`]): uma releitura que
///    chegou entre a base e a gravação leu o leitor depois da troca, e ganha;
/// 3. a releitura guarda sem contar troca enquanto a abertura não fixou.
///
/// Na primeira versão a abertura calculava antes da base e gravava por cima: a troca que chegasse
/// entre as duas era descartada pela releitura (sem base) e apagada pela gravação.
///
/// [`fixar_na_abertura`]: AspectoDaSessao::fixar_na_abertura
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AspectoDaSessao {
    atual: Option<Aspecto>,
    fixado: bool,
    trocas: u64,
}

/// O que uma releitura fez.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Releitura {
    /// O mesmo aspecto.
    Igual,
    /// A abertura ainda não fixou: guardado, sem contar troca (a abertura vai usar este).
    AntesDaAbertura,
    /// Mudou com a sessão no ar: a `n`-ésima troca, e o aspecto de antes.
    Mudou { n: u64, antes: Aspecto },
}

impl AspectoDaSessao {
    /// A abertura fixa o aspecto que calculou, **se nenhuma releitura guardou antes**, e devolve o
    /// que ficou: é dele que saem o teto e a saída do conversor.
    pub fn fixar_na_abertura(&mut self, calculado: Aspecto) -> Aspecto {
        self.fixado = true;
        *self.atual.get_or_insert(calculado)
    }

    /// Uma troca de tipo releu o leitor.
    pub fn reler(&mut self, novo: Aspecto) -> Releitura {
        match self.atual {
            Some(a) if a == novo => Releitura::Igual,
            _ if !self.fixado => {
                self.atual = Some(novo);
                Releitura::AntesDaAbertura
            }
            antes => {
                self.atual = Some(novo);
                self.trocas += 1;
                // `fixado` implica um aspecto guardado.
                Releitura::Mudou { n: self.trocas, antes: antes.unwrap_or(novo) }
            }
        }
    }

    /// O aspecto em uso: o da abertura, ou o da última troca; quadrado antes de tudo.
    pub fn atual(&self) -> Aspecto {
        self.atual.unwrap_or(Aspecto { par: Par::QUADRADA, origem: OrigemDaPar::Quadrada })
    }

    /// Quantas vezes o aspecto mudou com a sessão no ar (depois de a abertura fixar).
    pub fn trocas(&self) -> u64 {
        self.trocas
    }
}

/// Um número arredondado ao **par** mais próximo, pelo menos 2 (o NV12 pede dimensões pares).
fn par_mais_proximo(x: f64) -> u32 {
    ((x / 2.0).round() as u32).max(1) * 2
}

/// **O tamanho em pixel quadrado** de uma imagem `largura`×`altura` com a PAR `par`: a largura
/// escalada pela PAR e arredondada ao par mais próximo; a altura fica. 720×480 a 32:27 dá 854×480;
/// a 8:9, 640×480. Quadrada, o tamanho não muda.
pub fn tamanho_exibido(largura: u32, altura: u32, par: Par) -> (u32, u32) {
    if par.quadrada() {
        return (largura, altura);
    }
    (par_mais_proximo(f64::from(largura) * f64::from(par.num) / f64::from(par.den)), altura)
}

/// **Onde a imagem entra numa saída de tamanho fixo**, sem deformar: o maior retângulo com o
/// aspecto de `exibida` dentro de `saida`, centrado, com as bordas em pixel par (as faixas pretas
/// ficam de fora). É a troca de aspecto no meio da sessão (o Bruno muda 16:9 ↔ 4:3 com a câmera
/// transmitindo): o encoder tem o tamanho da abertura, e o quadro novo entra com faixas.
/// Devolve (x, y, largura, altura).
pub fn encaixe(saida: (u32, u32), exibida: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sl, sa) = (f64::from(saida.0), f64::from(saida.1));
    let (el, ea) = (f64::from(exibida.0.max(1)), f64::from(exibida.1.max(1)));
    let escala = (sl / el).min(sa / ea);
    let l = par_mais_proximo(el * escala).min(saida.0);
    let a = par_mais_proximo(ea * escala).min(saida.1);
    let x = ((saida.0 - l) / 2) & !1;
    let y = ((saida.1 - a) / 2) & !1;
    (x, y, l, a)
}

/// **Os planos de um quadro I420 em memória**: o início do U, o início do V e o passo dos dois, para
/// um plano Y de `passo` bytes por linha e `quadro_altura` linhas. Os planos de croma têm metade das
/// linhas e metade do passo, um depois do outro (o arranjo do Media Foundation para I420 e IYUV,
/// também num buffer 2D: o passo que o `Lock2D` devolve é o do Y).
pub fn planos_i420(passo: usize, quadro_altura: usize) -> (usize, usize, usize) {
    let passo_uv = passo / 2;
    let inicio_u = passo * quadro_altura;
    let inicio_v = inicio_u + passo_uv * (quadro_altura / 2);
    (inicio_u, inicio_v, passo_uv)
}

/// Os bytes que um quadro I420 ocupa em memória, com o passo `passo` e `quadro_altura` linhas.
pub fn tamanho_i420(passo: usize, quadro_altura: usize) -> usize {
    let (_, inicio_v, passo_uv) = planos_i420(passo, quadro_altura);
    inicio_v + passo_uv * (quadro_altura / 2)
}

/// **Até onde a cópia de um quadro I420 lê**: o fim da última linha do V, `quadro_largura / 2`
/// amostras depois do começo dela. Com passo de sobra, passa do que a conta do NV12 cobre; sem
/// sobra, é o [`tamanho_i420`].
pub fn fim_i420(passo: usize, quadro_largura: usize, quadro_altura: usize) -> usize {
    let (_, inicio_v, passo_uv) = planos_i420(passo, quadro_altura);
    inicio_v + passo_uv * (quadro_altura / 2).saturating_sub(1) + quadro_largura / 2
}

/// **Uma linha de croma I420 vira a linha UV do NV12**: `U0 V0 U1 V1 …`. O destino tem o dobro das
/// amostras de cada plano; o que sobrar dele não é tocado.
pub fn entrelacar_uv(u: &[u8], v: &[u8], destino: &mut [u8]) {
    for ((par, a), b) in destino.chunks_exact_mut(2).zip(u).zip(v) {
        par[0] = *a;
        par[1] = *b;
    }
}

/// A matriz é a BT.709? O que o tipo declara manda; **sem declaração, BT.601**: é o padrão da UVC
/// (`bMatrixCoefficients` = SMPTE 170M no descritor de cor) e do JPEG (JFIF), e é o que o §3.3 diz.
/// A regra do Media Foundation para `MF_MT_YUV_MATRIX` ausente (BT.709 a partir de 720 linhas) é
/// de vídeo, e não de câmera (a revisão do código da fase 3, m9). Não mexe em pixel hoje: o
/// conversor mantém a matriz e troca só a faixa.
pub fn matriz_709(declarada_709: Option<bool>) -> bool {
    declarada_709.unwrap_or(false)
}

/// O quadro precisa passar pelo processador de vídeo? Para escalar, para converter YUY2 em NV12,
/// para levar a faixa completa à limitada do contrato — mesmo sem escala (a revisão, M6) —, ou para
/// desentrelaçar (o DV, a fase 5).
pub fn precisa_do_processador(saida_do_leitor: Subtipo, completa: bool, escala: bool, entrelacado: bool) -> bool {
    escala || completa || entrelacado || saida_do_leitor != Subtipo::Nv12
}

// =============================================================================================
// O carimbo
// =============================================================================================

/// De onde sai o instante da captura de um fluxo. Decidido **uma vez**, no primeiro quadro, e
/// escrito no registro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FonteDoCarimbo {
    /// `MFSampleExtension_DeviceTimestamp`: o carimbo do driver, no domínio do QPC (documentação
    /// oficial). Pelo Frame Server ele veio em 150 de 150 (M15).
    Dispositivo,
    /// O `GetSampleTime`, quando ele é QPC (a fonte do Quall no processo: M6).
    TempoDaAmostra,
    /// A hora da chegada: nenhum dos dois é plausível. A latência da fonte fica escondida.
    Chegada,
}

/// Idade acima disto não é carimbo de captura: é um carimbo relativo ao início do fluxo, ou lixo.
pub const IDADE_MAXIMA_PLAUSIVEL: Duration = Duration::from_secs(1);

/// Um carimbo que cairia **mais que isto antes** do anterior não é tremor nem o quadro a mais de
/// quando entra um leitor compartilhado (~33 ms, M16 e M36): é o relógio do aparelho que pulou
/// para trás (a revisão do código da fase 3, m3). ~7 quadros a 30 fps.
pub const SALTO_PARA_TRAS: Duration = Duration::from_millis(250);

/// A idade de um carimbo na chegada, em QPC de 100 ns, **se** ela for plausível (entre 0 e 1 s).
pub fn idade_plausivel(qpc_chegada_100ns: i64, carimbo_100ns: i64) -> Option<Duration> {
    let d = qpc_chegada_100ns.checked_sub(carimbo_100ns)?;
    if d < 0 || d > IDADE_MAXIMA_PLAUSIVEL.as_nanos() as i64 / 100 {
        return None;
    }
    Some(Duration::from_nanos(d as u64 * 100))
}

/// O que a chegada de um quadro diz sobre o tempo.
#[derive(Clone, Copy, Debug)]
pub struct Chegada {
    /// O `Instant` lido no retorno do leitor.
    pub instante: Instant,
    /// O QPC lido no mesmo ponto, em unidades de 100 ns.
    pub qpc_100ns: i64,
    pub dispositivo_100ns: Option<i64>,
    pub tempo_da_amostra_100ns: Option<i64>,
}

/// Converte o carimbo da câmera para o relógio do processo (o mesmo `Instant` da `origem` da
/// sessão, que o som também usa), **sem supor que o `Instant` é o QPC**: `captura = Instant(chegada)
/// − (QPC(chegada) − carimbo)`, uma subtração num domínio só (§5).
///
/// - A fonte do carimbo é decidida no primeiro quadro e **não muda** no fluxo: decidida quadro a
///   quadro, uma trava acima de ~1 s faria o carimbo andar para trás (a revisão, m3).
/// - **O quadro implausível leva a última idade plausível**, e não idade zero: com zero ele saía
///   uma idade inteira adiantado, os seguintes eram empurrados pela monotonia e o ritmo derrubava
///   dois (a revisão do código da fase 3, m2: um buraco de 133 ms e um quadro deslocado 67 ms).
/// - **O relógio que pula para trás** ([`SALTO_PARA_TRAS`]) é compensado, e a compensação é desfeita
///   quando ele volta (a reconferência da fase 3): a diferença de idade
///   passa a ser descontada dos quadros seguintes, e o carimbo continua de onde estava. Sem isso,
///   um salto de 500 ms parava a imagem por 500 ms (15 quadros forçados e derrubados pelo ritmo) e
///   deixava a idade errada para sempre (m3; teoria: nenhum salto assim foi visto). Um atraso de
///   entrega (quadros que chegam tarde) **não** dispara: o carimbo deles continua para a frente.
/// - **Monotonia forçada**: um carimbo que não anda vira o anterior mais 1 µs, e conta.
/// - Um quadro **anterior à origem** é descartado (`None`), e conta: senão ele sairia com carimbo 0.
/// - `Instant − Duration` que passaria do começo do relógio usa a chegada (`checked_sub`).
pub struct Carimbador {
    origem: Instant,
    fonte: Option<FonteDoCarimbo>,
    ultimo: Option<Instant>,
    /// A idade (já compensada) do último quadro plausível.
    ultima_idade: Option<Duration>,
    /// Quanto os saltos para trás do relógio do aparelho somam, em µs: descontado das idades.
    compensacao_us: i64,
    /// Saltos do relógio do aparelho compensados.
    pub saltos: u64,
    /// Voltas do relógio do aparelho depois de um salto: a compensação desfeita.
    pub voltas: u64,
    /// Carimbos empurrados para a frente pela monotonia.
    pub forcados: u64,
    /// Quadros anteriores à origem, descartados.
    pub antes_da_origem: u64,
    /// Quadros em que a fonte decidida veio implausível (e a chegada foi usada no lugar).
    pub implausiveis: u64,
    /// A idade na chegada de cada quadro, em µs, para o registro (a revisão, m6): com o carimbo do
    /// dispositivo, a latência da sessão passa a incluir a da fonte, e esta é a que separa as duas.
    idades_us: Vec<u32>,
    /// A idade do último quadro carimbado, também depois de [`IDADES_GUARDADAS`] (a janela de 5 s
    /// da fase 5, [`JanelaDaCamera`]).
    idade_do_ultimo: Option<Duration>,
}

/// Quantas idades o carimbador guarda para o resumo (≈ 5 min a 30 fps).
const IDADES_GUARDADAS: usize = 9000;

impl Carimbador {
    pub fn novo(origem: Instant) -> Self {
        Carimbador {
            origem,
            fonte: None,
            ultimo: None,
            ultima_idade: None,
            compensacao_us: 0,
            saltos: 0,
            voltas: 0,
            forcados: 0,
            antes_da_origem: 0,
            implausiveis: 0,
            idades_us: Vec::new(),
            idade_do_ultimo: None,
        }
    }

    pub fn fonte(&self) -> Option<FonteDoCarimbo> {
        self.fonte
    }

    /// A idade na chegada do último quadro carimbado (descartado antes da origem inclusive).
    pub fn idade_do_ultimo(&self) -> Option<Duration> {
        self.idade_do_ultimo
    }

    fn decidir(c: &Chegada) -> FonteDoCarimbo {
        if c.dispositivo_100ns.and_then(|d| idade_plausivel(c.qpc_100ns, d)).is_some() {
            FonteDoCarimbo::Dispositivo
        } else if c.tempo_da_amostra_100ns.and_then(|t| idade_plausivel(c.qpc_100ns, t)).is_some() {
            FonteDoCarimbo::TempoDaAmostra
        } else {
            FonteDoCarimbo::Chegada
        }
    }

    /// O instante da captura no relógio do processo, ou `None` para descartar o quadro.
    pub fn carimbar(&mut self, c: &Chegada) -> Option<Instant> {
        let fonte = *self.fonte.get_or_insert_with(|| Self::decidir(c));
        let bruta = match fonte {
            FonteDoCarimbo::Dispositivo => c.dispositivo_100ns.and_then(|d| idade_plausivel(c.qpc_100ns, d)),
            FonteDoCarimbo::TempoDaAmostra => c.tempo_da_amostra_100ns.and_then(|t| idade_plausivel(c.qpc_100ns, t)),
            FonteDoCarimbo::Chegada => Some(Duration::ZERO),
        };
        let compensada = |b: Duration, comp: i64| Duration::from_micros((b.as_micros() as i64 - comp).max(0) as u64);
        let mut idade = match bruta {
            Some(b) => compensada(b, self.compensacao_us),
            None => {
                self.implausiveis += 1;
                self.ultima_idade.unwrap_or(Duration::ZERO)
            }
        };
        let mut t = c.instante.checked_sub(idade).unwrap_or(c.instante);
        // **O relógio voltou** depois de um salto compensado (a reconferência da fase 3): a idade
        // compensada ficaria negativa (e o `max(0)` a esconderia, deixando a idade em zero e o
        // carimbo na chegada para sempre). A diferença para a última idade sai da compensação.
        if let (Some(b), Some(referencia)) = (bruta, self.ultima_idade) {
            let assinada = b.as_micros() as i64 - self.compensacao_us;
            if self.compensacao_us != 0 && assinada < 0 {
                self.compensacao_us += assinada - referencia.as_micros() as i64;
                self.voltas += 1;
                idade = compensada(b, self.compensacao_us);
                t = c.instante.checked_sub(idade).unwrap_or(c.instante);
            }
        }
        // O relógio do aparelho pulou para trás: a diferença de idade vira compensação.
        if let (Some(b), Some(u), Some(referencia)) = (bruta, self.ultimo, self.ultima_idade) {
            if t + SALTO_PARA_TRAS < u {
                self.compensacao_us += idade.as_micros() as i64 - referencia.as_micros() as i64;
                self.saltos += 1;
                idade = compensada(b, self.compensacao_us);
                t = c.instante.checked_sub(idade).unwrap_or(c.instante);
            }
        }
        if bruta.is_some() {
            self.ultima_idade = Some(idade);
        }
        if self.idades_us.len() < IDADES_GUARDADAS {
            self.idades_us.push(idade.as_micros().min(u128::from(u32::MAX)) as u32);
        }
        self.idade_do_ultimo = Some(idade);
        if t < self.origem {
            self.antes_da_origem += 1;
            return None;
        }
        if let Some(u) = self.ultimo {
            if t <= u {
                t = u + Duration::from_micros(1);
                self.forcados += 1;
            }
        }
        self.ultimo = Some(t);
        Some(t)
    }

    /// A idade na chegada, em ms: mínimo, mediana, p95 e máximo.
    pub fn resumo_das_idades(&self) -> String {
        resumo_em_ms(&self.idades_us)
    }
}

/// Uma série em µs resumida em ms: `n=… min=… p50=… p95=… max=… ms`, ou "sem quadros".
pub fn resumo_em_ms(serie_us: &[u32]) -> String {
    if serie_us.is_empty() {
        return "sem quadros".into(); // i18n: fora (diário e detalhe técnico)
    }
    let mut v = serie_us.to_vec();
    v.sort_unstable();
    let p = |q: f64| f64::from(v[((v.len() - 1) as f64 * q).round() as usize]) / 1000.0;
    format!("n={} min={:.2} p50={:.2} p95={:.2} max={:.2} ms", v.len(), p(0.0), p(0.5), p(0.95), p(1.0))
}

/// O percentil `q` de uma série em µs, em ms; `None` sem amostras.
fn percentil_ms(serie_us: &[u32], q: f64) -> Option<f64> {
    if serie_us.is_empty() {
        return None;
    }
    let mut v = serie_us.to_vec();
    v.sort_unstable();
    Some(f64::from(v[((v.len() - 1) as f64 * q).round() as usize]) / 1000.0)
}

// =============================================================================================
// A fase 5: os relógios de uma câmera de verdade, e a janela de 5 s
// =============================================================================================

/// Um passo do relógio do aparelho maior que isto, para a frente, é um salto (o cabo que volta, a
/// fonte que recomeça), e não um quadro atrasado: ~30 quadros a 30 fps.
pub const SALTO_PARA_A_FRENTE: Duration = Duration::from_secs(1);

/// **Os relógios de um fluxo de câmera, como chegaram** (a fase 5, §5 de
/// `docs/camera-no-windows.md`). O [`Carimbador`] decide uma fonte e esconde a pergunta que só uma
/// câmera de verdade responde: numa webcam, o `GetSampleTime` é QPC ou é relativo ao começo do
/// fluxo? O `MFSampleExtension_DeviceTimestamp` vem, e é plausível? Isto só conta, sem decidir
/// nada, e dá o passo do relógio do aparelho entre quadros — a cadência do sensor, que é o que cai
/// em pouca luz.
#[derive(Default)]
pub struct RelogiosDoFluxo {
    pub quadros: u64,
    /// Quadros com `DeviceTimestamp`.
    pub com_dispositivo: u64,
    /// ... e com ele plausível como QPC (idade entre 0 e [`IDADE_MAXIMA_PLAUSIVEL`]).
    pub dispositivo_plausivel: u64,
    /// Quadros em que o `GetSampleTime` é plausível como QPC.
    pub tempo_plausivel: u64,
    /// Quadros em que o `GetSampleTime` é igual ao `DeviceTimestamp`.
    pub tempo_igual_ao_dispositivo: u64,
    /// O relógio do aparelho andou para trás.
    pub voltas_do_dispositivo: u64,
    /// O relógio do aparelho pulou mais que [`SALTO_PARA_A_FRENTE`], e o maior pulo.
    pub saltos_do_dispositivo: u64,
    pub maior_salto: Option<Duration>,
    primeiro: Option<Chegada>,
    ultimo_dispositivo: Option<i64>,
    ultimo_tempo: Option<i64>,
    /// Os passos do relógio do aparelho (ou do `GetSampleTime`, sem ele), em µs.
    passos_us: Vec<u32>,
}

impl RelogiosDoFluxo {
    pub fn novo() -> Self {
        Self::default()
    }

    /// Um quadro chegou. Devolve o passo do relógio do aparelho desde o anterior (o do
    /// `GetSampleTime` quando não há `DeviceTimestamp`), se ele andou para a frente.
    pub fn observar(&mut self, c: &Chegada) -> Option<Duration> {
        self.quadros += 1;
        if self.primeiro.is_none() {
            self.primeiro = Some(*c);
        }
        if let Some(d) = c.dispositivo_100ns {
            self.com_dispositivo += 1;
            if idade_plausivel(c.qpc_100ns, d).is_some() {
                self.dispositivo_plausivel += 1;
            }
        }
        if let Some(t) = c.tempo_da_amostra_100ns {
            if idade_plausivel(c.qpc_100ns, t).is_some() {
                self.tempo_plausivel += 1;
            }
            if c.dispositivo_100ns == Some(t) {
                self.tempo_igual_ao_dispositivo += 1;
            }
        }
        // O relógio do aparelho, e o `GetSampleTime` quando ele não vem: os dois do mesmo fluxo.
        let (atual, anterior) = match c.dispositivo_100ns {
            Some(d) => (Some(d), self.ultimo_dispositivo.replace(d)),
            None => (c.tempo_da_amostra_100ns, c.tempo_da_amostra_100ns.and_then(|t| self.ultimo_tempo.replace(t))),
        };
        let (Some(atual), Some(anterior)) = (atual, anterior) else {
            return None;
        };
        // **Andou para trás** é volta, caiba o passo num `i64` ou não (a revisão curta do `08af2cd`,
        // A9: o estouro para trás contava como salto). A comparação vem antes da conta.
        if atual < anterior {
            self.voltas_do_dispositivo += 1;
            return None;
        }
        // **Com conferência de estouro** (a revisão do código da fase 5, L7): um carimbo absurdo não
        // dá pânico; o passo para a frente que não cabe conta como salto, com o maior possível.
        let Some(passo) = atual.checked_sub(anterior) else {
            self.saltos_do_dispositivo += 1;
            self.maior_salto = Some(Duration::MAX);
            return None;
        };
        let passo = (passo as u64).checked_mul(100).map(Duration::from_nanos).unwrap_or(Duration::MAX);
        if passo > SALTO_PARA_A_FRENTE {
            self.saltos_do_dispositivo += 1;
            self.maior_salto = Some(self.maior_salto.map_or(passo, |m| m.max(passo)));
        }
        if self.passos_us.len() < IDADES_GUARDADAS {
            self.passos_us.push(passo.as_micros().min(u128::from(u32::MAX)) as u32);
        }
        Some(passo)
    }

    /// O primeiro quadro, com os três relógios em ms e as duas idades: é o que diz, de uma vez, se
    /// o `GetSampleTime` é relativo ao começo do fluxo (um número pequeno) ou QPC (horas desde o boot).
    pub fn primeiro_em_texto(&self) -> Option<String> {
        let c = self.primeiro?;
        let ms = |v: i64| v as f64 / 10_000.0;
        let idade = |v: Option<i64>| match v {
            Some(v) => c.qpc_100ns.checked_sub(v).map(|d| format!("{:.2} ms", ms(d))).unwrap_or_else(|| "estoura".into()),
            None => "—".into(),
        };
        Some(format!(
            "primeiro quadro: GetSampleTime={} DeviceTimestamp={} QPC da chegada={:.2} ms | QPC−DeviceTimestamp={} QPC−GetSampleTime={}", // i18n: fora (diário e detalhe técnico)
            c.tempo_da_amostra_100ns.map(|v| format!("{:.2} ms", ms(v))).unwrap_or_else(|| "—".into()),
            c.dispositivo_100ns.map(|v| format!("{:.2} ms", ms(v))).unwrap_or_else(|| "—".into()),
            ms(c.qpc_100ns),
            idade(c.dispositivo_100ns),
            idade(c.tempo_da_amostra_100ns),
        ))
    }

    /// Para o relato do fim da captura.
    pub fn resumo(&self) -> String {
        let fps = percentil_ms(&self.passos_us, 0.5).filter(|p| *p > 0.0).map(|p| 1000.0 / p);
        format!(
            // i18n: fora (diário e detalhe técnico)
            "relógios: quadros={} DeviceTimestamp={} (plausível em {}) GetSampleTime_como_QPC={} GetSampleTime_igual_ao_DeviceTimestamp={} \
             voltas={} saltos_acima_de_1s={}{} | passo do relógio do aparelho {}{}",
            self.quadros,
            self.com_dispositivo,
            self.dispositivo_plausivel,
            self.tempo_plausivel,
            self.tempo_igual_ao_dispositivo,
            self.voltas_do_dispositivo,
            self.saltos_do_dispositivo,
            self.maior_salto.map(|m| format!(" (o maior {:.0} ms)", m.as_secs_f64() * 1000.0)).unwrap_or_default(),
            resumo_em_ms(&self.passos_us),
            fps.map(|f| format!(" (≈{f:.1} fps)")).unwrap_or_default(),
        )
    }
}

/// De quanto em quanto a câmera diz ao registro o que chegou (a fase 5: o fps que cai em pouca luz,
/// o cabo que sai e volta). Uma linha por janela, só com câmera.
pub const JANELA_DO_RELATO: Duration = Duration::from_secs(5);

/// **A janela de 5 s da câmera** (a fase 5): quantos quadros chegaram do leitor e quantos foram
/// entregues ao encoder, o passo do relógio do aparelho e a idade na chegada, por janela. O relato
/// do fim da captura é um só para a sessão inteira, e em pouca luz o que importa é **quando** o fps
/// caiu e se voltou.
pub struct JanelaDaCamera {
    comeco: Option<Instant>,
    chegados_no_comeco: u64,
    entregues_no_comeco: u64,
    passos_us: Vec<u32>,
    idades_us: Vec<u32>,
    numero: u32,
}

impl Default for JanelaDaCamera {
    fn default() -> Self {
        Self::novo()
    }
}

impl JanelaDaCamera {
    pub fn novo() -> Self {
        JanelaDaCamera {
            comeco: None,
            chegados_no_comeco: 0,
            entregues_no_comeco: 0,
            passos_us: Vec::new(),
            idades_us: Vec::new(),
            numero: 0,
        }
    }

    /// Um quadro tomado da caixa, com o passo do relógio do aparelho e a idade na chegada.
    pub fn quadro(&mut self, passo: Option<Duration>, idade: Option<Duration>) {
        let us = |d: Duration| d.as_micros().min(u128::from(u32::MAX)) as u32;
        if let Some(p) = passo {
            self.passos_us.push(us(p));
        }
        if let Some(i) = idade {
            self.idades_us.push(us(i));
        }
    }

    /// Começa a primeira janela (na primeira chamada) ou, passada [`JANELA_DO_RELATO`], fecha a
    /// janela com a linha do registro e começa a seguinte. `chegados` e `entregues` são os totais.
    pub fn fechar_se_passou(&mut self, agora: Instant, chegados: u64, entregues: u64) -> Option<String> {
        let Some(comeco) = self.comeco else {
            self.comeco = Some(agora);
            self.chegados_no_comeco = chegados;
            self.entregues_no_comeco = entregues;
            return None;
        };
        let duracao = agora.saturating_duration_since(comeco);
        if duracao < JANELA_DO_RELATO {
            return None;
        }
        self.numero += 1;
        let s = duracao.as_secs_f64();
        let chegaram = chegados.saturating_sub(self.chegados_no_comeco);
        let entregues_na_janela = entregues.saturating_sub(self.entregues_no_comeco);
        let par = |v: &[u32]| match (percentil_ms(v, 0.5), percentil_ms(v, 1.0)) {
            (Some(p50), Some(max)) => format!("p50 {p50:.1} máx {max:.1} ms"), // i18n: fora (diário e detalhe técnico)
            _ => "—".to_string(),
        };
        let linha = format!(
            // i18n: fora (diário e detalhe técnico)
            "câmera: janela {} ({s:.1} s): chegaram {chegaram} ({:.1} fps), entregues {entregues_na_janela} ({:.1} fps) | \
             passo do relógio do aparelho {} | idade na chegada {}",
            self.numero,
            chegaram as f64 / s,
            entregues_na_janela as f64 / s,
            par(&self.passos_us),
            par(&self.idades_us),
        );
        self.comeco = Some(agora);
        self.chegados_no_comeco = chegados;
        self.entregues_no_comeco = entregues;
        self.passos_us.clear();
        self.idades_us.clear();
        Some(linha)
    }
}

// =============================================================================================
// O ritmo e a parada
// =============================================================================================

/// O quadro entra, ou vem cedo demais para o fps do teto? Uma câmera que só declara 60 fps num
/// teto de 30 teria o dobro de quadros para um encoder configurado a 30: a taxa passaria do teto.
/// Três quartos do intervalo, para não descartar o quadro que chegou um pouco adiantado.
pub fn entregar(ultimo_entregue: Option<Instant>, este: Instant, fps: u32) -> bool {
    match ultimo_entregue {
        None => true,
        Some(u) => este.saturating_duration_since(u) >= Duration::from_secs_f64(0.75 / f64::from(fps.max(1))),
    }
}

/// Sem quadro por este tempo, a câmera é dada como **parada**. Uma webcam em pouca luz cai para ~7
/// fps (143 ms), e a de 27/08 deu 63,8 ms de p50 (M17): 3 s é folga de sobra e ainda é rápido para
/// a pessoa.
///
/// **Até 22/09 era a quarta testemunha da desconexão e encerrava a sessão** (§6.2). A decisão do
/// Bruno de 21/09 (handover §4.2): **a câmera que pausa não encerra**. A Panasonic em DV parou
/// cinco vezes em 21/09 com a interface de pé (o diário do Dell: `interface_desabilitada=false`
/// nas cinco), e cada parada derrubou a gravação dele no OBS. Agora é só estado: a tela diz
/// [`texto_da_camera_parada`], a cadeia repete o último quadro, e a sessão segue. O que encerra é
/// o leitor que falha, a interface que some e, só para a câmera cuja interface nunca foi
/// confirmada, [`TETO_DA_PAUSA_SEM_INTERFACE`].
pub const SEM_QUADRO: Duration = Duration::from_secs(3);

/// A câmera parou de mandar quadro? Conta da chegada do último, ou da abertura se nenhum chegou.
pub fn parou(ultimo_quadro: Option<Instant>, aberta_em: Instant, agora: Instant) -> bool {
    parada_ha(ultimo_quadro, aberta_em, agora).is_some()
}

/// Há quanto tempo a câmera está parada, quando está (sem quadro por [`SEM_QUADRO`] ou mais).
pub fn parada_ha(ultimo_quadro: Option<Instant>, aberta_em: Instant, agora: Instant) -> Option<Duration> {
    let d = agora.saturating_duration_since(ultimo_quadro.unwrap_or(aberta_em));
    (d >= SEM_QUADRO).then_some(d)
}

/// **A câmera cuja interface nunca foi lida habilitada** (a revisão adversarial da crítica de
/// 22/09, 4): para ela, a única testemunha de que a câmera saiu é o leitor, e o leitor calado ao
/// puxar uma webcam USB não foi medido (R10). Parada por este tempo, a sessão encerra como antes
/// ("parou"). Com a interface confirmada, a pausa não tem teto: quem decide é a pessoa, no Parar.
pub const TETO_DA_PAUSA_SEM_INTERFACE: Duration = Duration::from_secs(60);

/// A frase da tela com a câmera parada (a linha do resumo da transmissão), no idioma da hora.
pub fn texto_da_camera_parada(ha: Duration) -> String {
    crate::idioma::tf(MOLDE_DA_CAMERA_PARADA, &[&ha.as_secs()])
}

/// O molde da frase: é a chave da tabela, e o começo dele (até o `{}`) é o que a reconhece.
const MOLDE_DA_CAMERA_PARADA: &str = "Câmera parada há {} s — esperando ela voltar."; // i18n: chave

/// O resumo é a frase da câmera parada? (as várias sessões não lhe acrescentam " · com o som").
/// Nos dois idiomas: a frase nasce no idioma da hora, e o idioma pode ter mudado depois.
pub fn e_texto_da_camera_parada(t: &str) -> bool {
    let comeco = |m: &str| m.split("{}").next().unwrap_or(m).to_string();
    let en = crate::idioma::en_de(MOLDE_DA_CAMERA_PARADA).unwrap_or(MOLDE_DA_CAMERA_PARADA);
    t.starts_with(&comeco(MOLDE_DA_CAMERA_PARADA)) || t.starts_with(&comeco(en))
}

/// **Depois de quanto tempo sem quadro real a cadeia começa a repetir o último quadro da câmera.**
/// Acima do pior intervalo medido em pouca luz (98 ms, M72) e do de uma webcam a 5 fps (200 ms),
/// para a repetição nunca se intercalar com os quadros de uma câmera lenta; abaixo dos 500 ms em
/// que a câmera virtual do receptor do Windows troca a imagem pela placa de espera (`baia.rs`).
pub const REPETIR_A_CAMERA_PARADA_APOS: Duration = Duration::from_millis(400);

/// **De quanto em quanto a câmera parada repete o último quadro.** Android, macOS e iOS desistem
/// da sessão com 10 s sem quadro de vídeo (`ReceptorSessao.kt`, `Receptor.swift`,
/// `SessaoDeRecepcao.swift`). E o anel de reordenação do núcleo, com um pacote perdido na borda
/// da pausa, segura os seguintes até juntar 16–64 pacotes, sem prazo por tempo (`quall-core`,
/// `rtp.rs`): um quadro repetido é ~1 pacote, então a 500 ms (o número do Mac) isso seriam até
/// 32 s, e a 100 ms são 6,4 s (a revisão adversarial da crítica, 1).
pub const INTERVALO_DA_REPETICAO_DA_CAMERA: Duration = Duration::from_millis(100);

/// A câmera está parada o bastante para a cadeia repetir o último quadro agora?
/// `ultimo_real` é a chegada do último quadro da câmera; `ultima_submissao`, a do último quadro
/// que entrou no encoder (real ou repetido). Sem quadro real ainda, não há o que repetir.
pub fn repetir_a_camera(ultimo_real: Option<Instant>, ultima_submissao: Option<Instant>, agora: Instant) -> bool {
    let Some(real) = ultimo_real else { return false };
    let parada = agora.saturating_duration_since(real) >= REPETIR_A_CAMERA_PARADA_APOS;
    let espacada = ultima_submissao.map_or(true, |s| agora.saturating_duration_since(s) >= INTERVALO_DA_REPETICAO_DA_CAMERA);
    parada && espacada
}

/// **A posição do anel que pode ser repetida** (a revisão adversarial da crítica, 3): a da última
/// cópia boa, **só enquanto nenhuma outra posição foi tomada depois dela**. Só a cópia que falha
/// invalida: a posição dela foi tomada (e talvez escrita pela metade) sem virar a última boa, e
/// a boa anterior fica a menos voltas de ser reescrita do que um quadro novo estaria; a
/// repetição é pulada até a próxima cópia boa. Um quadro perdido depois da cópia (na conversão,
/// no `ProcessInput`, no `FLUSH`) não invalida: a posição dele é a última boa, e ninguém a
/// escreveu depois. Uma memória, e não a conta `(boa + 1) % tamanho == próxima`, que volta a valer
/// depois de uma volta inteira de falhas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PosicaoRepetivel(Option<usize>);

impl PosicaoRepetivel {
    /// Uma posição do anel foi tomada (antes da cópia): a anterior deixa de valer.
    pub fn tomada(&mut self) {
        self.0 = None;
    }
    /// A cópia para a posição `i` deu certo.
    pub fn copiada(&mut self, i: usize) {
        self.0 = Some(i);
    }
    pub fn posicao(&self) -> Option<usize> {
        self.0
    }
}

/// O que mudou na pausa desta volta do vigia ([`PausaDaCamera::olhar`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MudancaDaPausa {
    Nenhuma,
    /// A câmera acabou de ser dada como parada: sem quadro há `ha`.
    Parou { ha: Duration },
    /// Os quadros voltaram, depois de `depois_de` sem quadro (contado da chegada do último).
    Voltou { depois_de: Duration },
}

/// **A pausa da câmera, pura**: uma linha no registro por transição, e o número e a maior pausa
/// para o relato do fim. Olhada a cada volta do vigia (200 ms) com [`parada_ha`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PausaDaCamera {
    /// Desde quando está parada: a chegada do último quadro.
    desde: Option<Instant>,
    pub pausas: u64,
    pub maior: Duration,
}

impl PausaDaCamera {
    pub fn olhar(&mut self, parada: Option<Duration>, agora: Instant) -> MudancaDaPausa {
        match (self.desde, parada) {
            (None, Some(ha)) => {
                self.desde = Some(agora.checked_sub(ha).unwrap_or(agora));
                self.pausas += 1;
                MudancaDaPausa::Parou { ha }
            }
            (Some(d), None) => {
                self.desde = None;
                let depois_de = agora.saturating_duration_since(d);
                self.maior = self.maior.max(depois_de);
                MudancaDaPausa::Voltou { depois_de }
            }
            _ => MudancaDaPausa::Nenhuma,
        }
    }

    /// Há quanto tempo está parada, agora.
    pub fn parada_ha(&self, agora: Instant) -> Option<Duration> {
        self.desde.map(|d| agora.saturating_duration_since(d))
    }

    /// Encerrar agora? Só a câmera cuja interface nunca foi confirmada, parada por
    /// [`TETO_DA_PAUSA_SEM_INTERFACE`].
    pub fn encerrar(&self, agora: Instant, interface_confirmada: bool) -> bool {
        !interface_confirmada && self.parada_ha(agora).is_some_and(|h| h >= TETO_DA_PAUSA_SEM_INTERFACE)
    }

    /// Para o relato do fim: `pausas=N maior_ms=M` (a pausa em curso conta na maior).
    pub fn resumo(&self, agora: Instant) -> String {
        let maior = self.maior.max(self.parada_ha(agora).unwrap_or_default());
        format!("pausas={} maior_ms={}{}", self.pausas, maior.as_millis(), if self.desde.is_some() { " (parada no fim)" } else { "" }) // i18n: fora (diário e detalhe técnico)
    }
}

/// **Bancada** (`--pausar-camera`): as janelas em que a captura descarta o que o leitor entrega,
/// contadas da criação do estado da captura. `"20:15,60:40"` é "dos 20 s aos 35 s e dos 60 s aos
/// 100 s". Simula a câmera parada **na nossa fronteira**: o leitor continua entregando.
pub fn ler_pausas_de_bancada(texto: &str) -> std::result::Result<Vec<(Duration, Duration)>, String> {
    let mut v = Vec::new();
    for parte in texto.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = parte.split_once(':').ok_or_else(|| format!("\"{parte}\" não é início:duração em segundos"))?; // i18n: fora (diário e detalhe técnico)
        let ler = |s: &str| s.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x >= 0.0);
        match (ler(a), ler(b)) {
            (Some(inicio), Some(dura)) if dura > 0.0 => v.push((Duration::from_secs_f64(inicio), Duration::from_secs_f64(dura))),
            _ => return Err(format!("\"{parte}\" não é início:duração em segundos, com duração > 0")), // i18n: fora (diário e detalhe técnico)
        }
    }
    if v.is_empty() {
        return Err("nenhuma pausa".into());
    }
    Ok(v)
}

/// O instante `desde_a_criacao` cai numa das pausas de bancada?
pub fn na_pausa_de_bancada(pausas: &[(Duration, Duration)], desde_a_criacao: Duration) -> bool {
    pausas.iter().any(|(inicio, dura)| desde_a_criacao >= *inicio && desde_a_criacao < *inicio + *dura)
}

// =============================================================================================
// O carimbo na submissão ao encoder (toda origem)
// =============================================================================================

/// **O carimbo que vai ao encoder e ao fio nunca volta** (a revisão do código da fase 3, M1). A
/// cadeia pode pôr um quadro seu entre dois da origem — a repetição da tela estendida, carimbada com
/// "agora" — e um quadro da câmera chega com a idade dele (34 a 68 ms pelo Frame Server, 64 ms numa
/// webcam a 15,7 fps): o carimbo dele cai antes da repetição. Vale para **toda** origem; mora aqui
/// porque é aritmética e é testada sem `net`. Devolve o carimbo e se ele foi empurrado.
pub fn carimbo_da_submissao(anterior: Option<Instant>, este: Instant) -> (Instant, bool) {
    match anterior {
        Some(a) if este <= a => (a + Duration::from_micros(1), true),
        _ => (este, false),
    }
}

// =============================================================================================
// A abertura que falha, e o fim
// =============================================================================================

/// **Quanto cada tentativa espera o primeiro quadro, ou a falha** — contado depois do primeiro
/// `ReadSample`, sem a criação da fonte. O R1b mediu onde vão os 5,1 s de uma câmera recém-criada:
/// **fonte 5.132 ms, primeiro quadro 41 ms** (M41; no R3, 4.602 e 40 ms). A espera de 15 s da
/// primeira versão do conserto se apoiava na suposição errada e só aumentava o pior caso (a
/// reconferência da fase 3). Com duas tentativas, a espera soma no máximo 10 s, o prazo em que o
/// Android desiste; e ela olha o Parar ([`esperar_resposta`]).
pub const ESPERA_DO_PRIMEIRO_QUADRO: Duration = Duration::from_secs(5);

/// De quanto em quanto a espera olha o Parar.
pub const FATIA_DA_ESPERA: Duration = Duration::from_millis(50);

/// O que a espera pelo primeiro quadro deu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Espera<T> {
    Veio(T),
    /// O prazo passou sem quadro nem falha.
    Prazo,
    /// A pessoa (ou a sessão) pediu Parar.
    Parada,
}

/// **Espera a primeira resposta com prazo, olhando o Parar** a cada [`FATIA_DA_ESPERA`] (a
/// reconferência da fase 3: a espera no `Condvar` não olhava o Parar, e o pior caso de uma câmera
/// que não responde chegava a ~25 s mais a criação da fonte).
pub fn esperar_resposta<T: Clone>(caixa: &Mutex<Option<T>>, aviso: &Condvar, prazo: Duration, parar: &dyn Fn() -> bool) -> Espera<T> {
    let fim = Instant::now() + prazo;
    let mut g = caixa.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(r) = g.as_ref() {
            return Espera::Veio(r.clone());
        }
        if parar() {
            return Espera::Parada;
        }
        let agora = Instant::now();
        if agora >= fim {
            return Espera::Prazo;
        }
        let fatia = FATIA_DA_ESPERA.min(fim - agora);
        g = aviso.wait_timeout(g, fatia).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
    }
}

/// Quanto o encoder pode segurar os quadros da câmera com conversor sem devolver nenhum: com
/// `destinos − 1` em voo a câmera espera (m6), e um MFT que engole entradas sem saída a travaria
/// para sempre, sem fim de sessão, porque os quadros continuam chegando (a reconferência da fase 3,
/// teoria: até hoje o MFT foi 1:1). Passado o prazo, a captura acaba com o motivo.
pub const PRAZO_DO_CONVERSOR_CHEIO: Duration = Duration::from_secs(2);

/// O conversor está esperando destino livre há mais que o prazo?
pub fn conversor_travado(esperando_desde: Option<Instant>, agora: Instant) -> bool {
    esperando_desde.is_some_and(|d| agora.saturating_duration_since(d) >= PRAZO_DO_CONVERSOR_CHEIO)
}

/// `MF_E_HW_MFT_FAILED_START_STREAMING`: outro app controla a câmera (M16, M36).
pub const HR_OCUPADA: u32 = 0xC00D_3704;
/// `MF_E_VIDEO_RECORDING_DEVICE_PREEMPTED`: outro app tomou a câmera.
pub const HR_TOMADA: u32 = 0xC00D_3EA3;
/// `MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED`: "o dispositivo não está mais presente" (M36).
pub const HR_REMOVIDA: u32 = 0xC00D_3EA2;
/// `E_ACCESSDENIED`: a privacidade do Windows nega a câmera a este app.
pub const HR_NEGADA: u32 = 0x8007_0005;

/// Por que uma tentativa de abrir não deu o primeiro quadro.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CausaDaFalha {
    /// Outro app controla, ou tomou, a câmera.
    Ocupada,
    /// A câmera saiu.
    Removida,
    /// A privacidade do Windows nega o acesso.
    Negada,
    /// Nenhum quadro, e nenhuma falha, no prazo.
    Demorou,
    /// A pessoa (ou a sessão) pediu Parar durante a abertura.
    Cancelada,
    Outra,
}

pub fn causa_da_falha(codigo: Option<u32>, demorou: bool) -> CausaDaFalha {
    if demorou {
        return CausaDaFalha::Demorou;
    }
    match codigo {
        Some(HR_OCUPADA) | Some(HR_TOMADA) => CausaDaFalha::Ocupada,
        Some(HR_REMOVIDA) => CausaDaFalha::Removida,
        Some(HR_NEGADA) => CausaDaFalha::Negada,
        _ => CausaDaFalha::Outra,
    }
}

/// Vale tentar compartilhada depois desta falha da controladora? Não quando a câmera saiu ou o
/// Windows negou: a compartilhada falharia igual, e a mensagem certa se perderia.
pub fn recuar_para_compartilhada(causa: CausaDaFalha) -> bool {
    !matches!(causa, CausaDaFalha::Removida | CausaDaFalha::Negada | CausaDaFalha::Cancelada)
}

/// Uma tentativa que falhou: o texto para o registro e a causa.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FalhaDaTentativa {
    pub texto: String,
    pub causa: CausaDaFalha,
}

/// **O texto para a pessoa** quando a câmera não abriu (a revisão do código da fase 3, M2): "em
/// uso por outro app" **só** quando algum código diz isso. Nos outros casos, o motivo de cada
/// tentativa, sem culpar ninguém.
pub fn texto_da_abertura_que_falhou(controladora: &FalhaDaTentativa, compartilhada: Option<&FalhaDaTentativa>) -> String {
    let causas: Vec<CausaDaFalha> = std::iter::once(controladora.causa).chain(compartilhada.map(|f| f.causa)).collect();
    let detalhe = match compartilhada {
        Some(c) => format!("controladora: {}; compartilhada: {}", controladora.texto, c.texto),
        None => controladora.texto.clone(),
    };
    let tem = |c: CausaDaFalha| causas.contains(&c);
    // A frase nasce no idioma da hora (vai à tela por `so_a_frase`); o detalhe fica como veio.
    let frase = if tem(CausaDaFalha::Cancelada) {
        t("A abertura da câmera foi interrompida pelo Parar.")
    } else if tem(CausaDaFalha::Removida) {
        t("A câmera não está mais conectada.")
    } else if tem(CausaDaFalha::Negada) {
        t("O Windows negou o acesso à câmera: veja Privacidade e segurança > Câmera.")
    } else if tem(CausaDaFalha::Ocupada) {
        t("A câmera está em uso por outro app.")
    } else if causas.iter().all(|c| *c == CausaDaFalha::Demorou) {
        t("A câmera não mandou nenhum quadro.")
    } else {
        t("A câmera não abriu.")
    };
    format!("{frase}{SEPARADOR_DO_DETALHE}{detalhe}")
}

/// Entre a frase para a pessoa e o detalhe de cada tentativa, no texto da abertura que falhou.
pub const SEPARADOR_DO_DETALHE: &str = " — detalhe: ";

/// **Só a frase**, para a tela: o detalhe de cada tentativa (os `HRESULT`, os tempos) vai para o
/// registro, e a pessoa lê "A câmera está em uso por outro app." (a fase 4, `janela.rs` pela
/// `emissor.rs`). Um texto sem o separador sai inteiro.
pub fn so_a_frase(texto: &str) -> &str {
    texto.split(SEPARADOR_DO_DETALHE).next().unwrap_or(texto)
}

/// Como a captura acabou: o motivo, e se ele é uma desconexão (o texto do fim diz "foi
/// desconectada", e não "parou": a revisão do código da fase 3, m1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FimDaCamera {
    pub motivo: String,
    pub desconectada: bool,
    /// O leitor devolveu [`HR_TOMADA`]: outro app tomou a câmera. No R4 (M51) quem tomou foi a
    /// segunda sessão do mesmo processo; com a vez por câmera ([`AberturasDoProcesso`]) isso não
    /// acontece mais, e o que sobra é outro app.
    pub tomada: bool,
    /// O leitor devolveu [`HR_NEGADA`] no meio do fluxo: a privacidade de câmera foi desligada com a
    /// sessão no ar (a fase 5, p6d: 0x80070005 e o fim em 112 ms, com a frase "parou").
    pub negada: bool,
    /// O formato ou a geometria mudou no meio (`CURRENTMEDIATYPECHANGED`, R19): a configuração do
    /// encoder é fixa pela sessão. É também o caso da câmera que volta de uma pausa noutro formato.
    pub formato_mudou: bool,
}

impl FimDaCamera {
    /// O fim com o código do leitor, quando há um: desconexão, tomada ou privacidade.
    pub fn pelo_codigo(motivo: String, removida: bool, codigo: Option<u32>) -> Self {
        FimDaCamera {
            motivo,
            desconectada: removida,
            tomada: codigo == Some(HR_TOMADA),
            negada: codigo == Some(HR_NEGADA),
            formato_mudou: false,
        }
    }
}

/// **O texto do fim de uma sessão de câmera**, frase e detalhe separados por
/// [`SEPARADOR_DO_DETALHE`]: a tela recebe [`so_a_frase`], o registro o texto inteiro. A interface
/// que sumiu é desconexão, diga o leitor o que disser; sem ela, o leitor decide. Antes do R4 o
/// "parou" levava o `HRESULT` cru para a tela ("A câmera \"X\" parou: o leitor devolveu
/// HRESULT(0xC00D3EA3) (…)").
pub fn texto_do_fim_da_camera(nome: &str, fim: Option<&FimDaCamera>, interface_sumiu: bool) -> String {
    let detalhe = fim.map(|f| format!("{SEPARADOR_DO_DETALHE}{}", f.motivo)).unwrap_or_default();
    let frase = match fim {
        _ if interface_sumiu => tf("A câmera \"{}\" foi desconectada.", &[&nome]),
        Some(f) if f.desconectada => tf("A câmera \"{}\" foi desconectada.", &[&nome]),
        // A mesma frase da abertura negada (`texto_da_abertura_que_falhou`), com o nome.
        Some(f) if f.negada => tf("O Windows negou o acesso à câmera \"{}\": veja Privacidade e segurança > Câmera.", &[&nome]),
        Some(f) if f.tomada => tf("Outro app tomou a câmera \"{}\".", &[&nome]),
        Some(f) if f.formato_mudou => tf("A câmera \"{}\" mudou de formato: comece a transmissão de novo.", &[&nome]),
        Some(_) => tf("A câmera \"{}\" parou.", &[&nome]),
        None => tf("A câmera \"{}\" foi desconectada.", &[&nome]),
    };
    format!("{frase}{detalhe}")
}

// =============================================================================================
// A vez de abrir, no processo
// =============================================================================================

/// **Como a abertura de uma câmera começa**, pelo que o próprio processo já tem dela.
///
/// O R4 (M51) mediu: duas sessões do mesmo processo abrindo a mesma câmera como controladoras, e
/// a segunda **tomou** a câmera da primeira (`0xC00D3EA3`, "substituído por outro aplicativo
/// imersivo"). Entre processos o Frame Server recusa a segunda controladora (`0xC00D3704`, M36 e
/// M42), e o recuo para compartilhada resolve; **dentro do mesmo processo ele não recusa: ele
/// preempta**. Então quem já tem a câmera no processo decide o modo antes de tentar — e o que
/// decide é haver uma **controladora** viva, e não uma captura qualquer (a revisão do código da
/// fase 4, M1: a [#1] controladora sai, a [#2] compartilhada fica, e a [#3] abriria compartilhada
/// sem controladora nenhuma).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanoDaAbertura {
    /// Nenhuma **controladora** deste processo tem a câmera: controladora e, se ela falhar,
    /// compartilhada (§3.5, a decisão 1 do Bruno).
    ControladoraComRecuo,
    /// Uma controladora deste processo tem a câmera (e `outras` capturas ao todo): **compartilhada
    /// direto**, sem tentar a controladora. Custa +1 quadro de idade pela sessão inteira (M42, M54:
    /// 67,5 ms contra 34 ms) e o tipo é o que a controladora escolheu. Se a compartilhada falhar e a
    /// controladora já tiver saído, recua para controladora ([`AberturasDoProcesso::ha_controladora`]).
    SoCompartilhada { outras: u32 },
}

/// O papel de uma captura viva na câmera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Papel {
    Controladora,
    Compartilhada,
}

/// Quanto quem vai abrir espera pelas solturas da mesma câmera que ainda estão em curso (a sessão
/// que acabou de sair solta o leitor fora da thread dela). Sem esta espera, a pessoa que para e
/// recomeça na hora abriria compartilhada contra uma controladora que está saindo, e pagaria +1
/// quadro na sessão nova inteira. Uma soltura normal leva ~250 ms (M51, a [#1]); a de uma câmera
/// que o Frame Server segurou levou 20 s (M51, a [#2]), e por essa não se espera.
pub const ESPERA_PELAS_SOLTURAS: Duration = Duration::from_secs(3);

/// O que o processo tem de uma câmera.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NoLink {
    link: String,
    /// Capturas abertas e não soltas, por papel.
    controladoras: u32,
    compartilhadas: u32,
    /// Capturas cuja soltura (leitor e fonte) está em curso, fora da thread da sessão.
    soltando: u32,
    /// Uma abertura em curso: as outras esperam a vez.
    abrindo: bool,
}

/// **As aberturas de câmera do processo**, por link: quem abre primeiro, quantas estão vivas (e em
/// que papel) e quantas soltando. Puro; a captura guarda uma num `static` com um `Condvar`
/// (`captura_de_camera.rs`). O link é comparado sem caixa: o Windows não distingue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AberturasDoProcesso {
    links: Vec<NoLink>,
}

impl AberturasDoProcesso {
    pub const fn novo() -> Self {
        AberturasDoProcesso { links: Vec::new() }
    }

    fn achar(&mut self, link: &str) -> &mut NoLink {
        let chave = link.to_ascii_lowercase();
        let i = match self.links.iter().position(|n| n.link == chave) {
            Some(i) => i,
            None => {
                self.links.push(NoLink { link: chave, ..Default::default() });
                self.links.len() - 1
            }
        };
        &mut self.links[i]
    }

    fn arrumar(&mut self) {
        self.links.retain(|n| n.controladoras > 0 || n.compartilhadas > 0 || n.soltando > 0 || n.abrindo);
    }

    /// **A vez de abrir**: `None` enquanto outra abertura da mesma câmera está em curso, ou
    /// enquanto há soltura em curso e `esperar_solturas`; senão marca a abertura e diz o plano:
    /// compartilhada direto **só com controladora viva** no processo.
    pub fn pedir_a_vez(&mut self, link: &str, esperar_solturas: bool) -> Option<PlanoDaAbertura> {
        let n = self.achar(link);
        if n.abrindo || (esperar_solturas && n.soltando > 0) {
            self.arrumar();
            return None;
        }
        n.abrindo = true;
        Some(if n.controladoras == 0 {
            PlanoDaAbertura::ControladoraComRecuo
        } else {
            PlanoDaAbertura::SoCompartilhada { outras: n.controladoras + n.compartilhadas }
        })
    }

    /// A abertura acabou: com a câmera aberta num papel (uma viva a mais) ou não.
    pub fn fim_da_abertura(&mut self, link: &str, abriu: Option<Papel>) {
        let n = self.achar(link);
        n.abrindo = false;
        match abriu {
            Some(Papel::Controladora) => n.controladoras += 1,
            Some(Papel::Compartilhada) => n.compartilhadas += 1,
            None => {}
        }
        self.arrumar();
    }

    /// Uma captura viva começou a soltar.
    pub fn comecou_a_soltar(&mut self, link: &str, papel: Papel) {
        let n = self.achar(link);
        match papel {
            Papel::Controladora => n.controladoras = n.controladoras.saturating_sub(1),
            Papel::Compartilhada => n.compartilhadas = n.compartilhadas.saturating_sub(1),
        }
        n.soltando += 1;
        self.arrumar();
    }

    /// A soltura acabou: o leitor e a fonte foram soltos.
    pub fn terminou_de_soltar(&mut self, link: &str) {
        let n = self.achar(link);
        n.soltando = n.soltando.saturating_sub(1);
        self.arrumar();
    }

    /// Há uma controladora viva deste processo nesta câmera? A compartilhada que falhou **sem** ela
    /// recua para controladora (a revisão do código da fase 4, M1).
    pub fn ha_controladora(&mut self, link: &str) -> bool {
        let r = self.achar(link).controladoras > 0;
        self.arrumar();
        r
    }

    /// (vivas, soltando, abrindo), para o registro e os testes.
    pub fn estado(&mut self, link: &str) -> (u32, u32, bool) {
        let n = self.achar(link);
        let r = (n.controladoras + n.compartilhadas, n.soltando, n.abrindo);
        self.arrumar();
        r
    }
}

/// Vale tentar controladora depois desta falha da **compartilhada**, sem controladora viva no
/// processo? Pela mesma regra do recuo contrário: não quando a câmera saiu, o Windows negou ou a
/// pessoa pediu Parar.
pub fn recuar_para_controladora(causa: CausaDaFalha) -> bool {
    recuar_para_compartilhada(causa)
}

// =============================================================================================
// O vigia da fonte
// =============================================================================================

/// O que o vigia da sessão faz com as duas testemunhas desta volta ([`VigiaDaFonte::olhar`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Volta {
    /// O par de testemunhas mudou: uma linha no registro. **Só na transição**: o R4 mostrou 16
    /// linhas iguais por sessão, uma a cada 200 ms da tolerância.
    pub registrar: bool,
    /// Esta é a primeira testemunha: o motivo do fim se decide agora.
    pub primeira: bool,
    /// A testemunha do sistema (o `Closed` do item, ou o fim do leitor da câmera) foi a primeira.
    pub sistema_primeiro: bool,
    /// A testemunha do sistema chegou depois da primeira: há quanto tempo a primeira veio.
    pub sistema_depois: Option<Duration>,
    /// Encerrar a sessão agora.
    pub encerrar: bool,
}

/// **O vigia da fonte**, puro: as duas testemunhas de que a fonte sumiu, a janela de tolerância e
/// o que registrar.
///
/// No monitor, a primeira testemunha não encerra: a reenumeração perde o nome GDI por ~0,5 s quando
/// outro monitor chega (E5), e o `Closed` pode não disparar (`docs/app-windows.md`); a sessão
/// espera a tolerância. **Na câmera, a primeira testemunha encerra** (`definitiva`): as duas são
/// fins de verdade — o leitor que acabou não volta (erro, evento com falha, fim do fluxo, formato
/// que muda; **a câmera parada não é mais testemunha** desde 22/09, [`PausaDaCamera`]), e a
/// interface que some tem confirmação própria ([`TestemunhaDaInterface`]). O R4 (M51) mediu o
/// custo de esperar: a [#1] ficou 3,06 s transmitindo nada depois de o leitor ter acabado.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VigiaDaFonte {
    definitiva: bool,
    visto: (bool, bool),
    primeira: Option<Instant>,
    sistema_visto: bool,
}

impl VigiaDaFonte {
    pub fn novo(definitiva: bool) -> Self {
        VigiaDaFonte { definitiva, ..Default::default() }
    }

    /// Uma volta: `sistema` é a testemunha do sistema, `lista` a da enumeração (ou da interface).
    pub fn olhar(&mut self, sistema: bool, lista: bool, agora: Instant, tolerancia: Duration) -> Volta {
        let mut v = Volta::default();
        let alguma = sistema || lista;
        let mudou = (sistema, lista) != self.visto;
        self.visto = (sistema, lista);
        v.registrar = mudou && (alguma || self.primeira.is_some());
        if alguma && self.primeira.is_none() {
            self.primeira = Some(agora);
            v.primeira = true;
            if sistema {
                self.sistema_visto = true;
                v.sistema_primeiro = true;
            }
        } else if sistema && !self.sistema_visto {
            self.sistema_visto = true;
            v.sistema_depois = self.primeira.map(|p| agora.saturating_duration_since(p));
        }
        if let Some(p) = self.primeira {
            v.encerrar = if self.definitiva { true } else { agora.saturating_duration_since(p) >= tolerancia };
        }
        v
    }

    /// A testemunha do sistema chegou em algum momento?
    pub fn sistema_visto(&self) -> bool {
        self.sistema_visto
    }
}

// =============================================================================================
// A interface da câmera
// =============================================================================================

/// Quantas leituras `None` seguidas, depois de a interface ter sido lida habilitada, fazem um
/// sumiço: 5 leituras de 200 ms, ~1 s. Um `None` passageiro (um soluço do gerenciador de
/// configuração) não encerra a sessão (a reconferência da fase 3).
pub const AUSENCIAS_PARA_SUMICO: u32 = 5;

/// **A testemunha da interface, com memória e confirmação** (a revisão do código da fase 3, m1, e a
/// reconferência). Quando o nó da câmera sai, a leitura de `DEVPKEY_DeviceInterface_Enabled` passa
/// de `Some(true)` a **`None`** (a interface deixa de existir, M36), e não a `Some(false)`. `None`
/// sem nunca ter lido `Some(true)` é "não deu para perguntar", e não derruba; `None` **depois** de
/// `Some(true)` é sumiço quando se repete [`AUSENCIAS_PARA_SUMICO`] vezes seguidas. `Some(false)`
/// derruba na hora: é a interface desabilitada, ou a que o gerenciador diz que não existe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TestemunhaDaInterface {
    ja_habilitada: bool,
    ausencias: u32,
}

impl TestemunhaDaInterface {
    /// A interface já foi lida habilitada alguma vez? Sem isso, a testemunha é cega (um `None` não
    /// derruba), e a pausa da câmera tem teto ([`TETO_DA_PAUSA_SEM_INTERFACE`]).
    pub fn ja_habilitada(&self) -> bool {
        self.ja_habilitada
    }

    /// A câmera está presente, com esta leitura?
    pub fn presente(&mut self, leitura: Option<bool>) -> bool {
        match leitura {
            Some(true) => {
                self.ja_habilitada = true;
                self.ausencias = 0;
                true
            }
            Some(false) => false,
            None if !self.ja_habilitada => true,
            None => {
                self.ausencias += 1;
                self.ausencias < AUSENCIAS_PARA_SUMICO
            }
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    const TETO_1080P30: TetoDaCamera = TetoDaCamera { max_macroblocos: 8160, fps: 30 };

    fn t(subtipo: Subtipo, largura: u32, altura: u32, fps_num: u32, fps_den: u32) -> TipoNativo {
        TipoNativo { subtipo, largura, altura, fps_num, fps_den }
    }

    #[test]
    fn a_webcam_usb2_nao_sai_em_yuy2_a_5_fps() {
        // O caso da revisão (M7): YUY2 1080p só a 5 fps; o 1080p30 existe em MJPG.
        let tipos = [
            t(Subtipo::Yuy2, 1920, 1080, 5, 1),
            t(Subtipo::Yuy2, 1280, 720, 10, 1),
            t(Subtipo::Mjpg, 1920, 1080, 30, 1),
            t(Subtipo::Nv12, 640, 480, 30, 1),
        ];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(2));
    }

    #[test]
    fn nv12_ganha_no_empate() {
        let tipos = [t(Subtipo::Mjpg, 1280, 720, 30, 1), t(Subtipo::Yuy2, 1280, 720, 30, 1), t(Subtipo::Nv12, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(2));
    }

    #[test]
    fn nada_acima_do_fps_do_teto() {
        let tipos = [t(Subtipo::Nv12, 1920, 1080, 60, 1), t(Subtipo::Nv12, 1920, 1080, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(1));
        // 29,97 é 30.
        let ntsc = [t(Subtipo::Nv12, 1920, 1080, 30000, 1001), t(Subtipo::Nv12, 1280, 720, 60, 1)];
        assert_eq!(escolher_tipo_nativo(&ntsc, TETO_1080P30), Some(0));
    }

    #[test]
    fn o_4k_cede_ao_1080p_que_cabe() {
        let tipos = [t(Subtipo::Nv12, 3840, 2160, 30, 1), t(Subtipo::Nv12, 1920, 1080, 30, 1), t(Subtipo::Nv12, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(1));
        // Só 4K: é ele (e a cadeia reduz).
        assert_eq!(escolher_tipo_nativo(&tipos[..1], TETO_1080P30), Some(0));
    }

    #[test]
    fn a_webcam_do_dell_de_agosto() {
        // O padrão do driver em 27/08: NV12 1280x720 a ~15,7 fps medidos. Se houver MJPG a 30,
        // a fluidez pesa mais que o formato.
        let tipos = [t(Subtipo::Nv12, 1280, 720, 15, 1), t(Subtipo::Mjpg, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(1));
        // Sem nada a 30, o mais rápido que resta: fora do andar fluido, o fps pesa antes da área
        // (a revisão do código da fase 3, m4: este teste afirmava o contrário do comentário).
        let lentos = [t(Subtipo::Nv12, 1280, 720, 10, 1), t(Subtipo::Nv12, 640, 480, 15, 1)];
        assert_eq!(escolher_tipo_nativo(&lentos, TETO_1080P30), Some(1), "o fps pesa antes da área");
    }

    #[test]
    fn so_formatos_que_a_cadeia_sabe() {
        assert_eq!(escolher_tipo_nativo(&[t(Subtipo::Outro, 1920, 1080, 30, 1)], TETO_1080P30), None);
        assert_eq!(escolher_tipo_nativo(&[], TETO_1080P30), None);
    }

    #[test]
    fn a_fonte_do_quall_e_escolhida() {
        // O único tipo que a fonte do Quall declara (M6): NV12 1920x1080 @30.
        assert_eq!(escolher_tipo_nativo(&[t(Subtipo::Nv12, 1920, 1080, 30, 1)], TETO_1080P30), Some(0));
    }

    #[test]
    fn a_faixa_e_o_processador() {
        assert!(!faixa_completa(Subtipo::Nv12, None));
        assert!(faixa_completa(Subtipo::Mjpg, None));
        assert!(faixa_completa(Subtipo::Nv12, Some(true)));
        assert!(!faixa_completa(Subtipo::Mjpg, Some(false)));
        assert!(!precisa_do_processador(Subtipo::Nv12, false, false, false));
        assert!(precisa_do_processador(Subtipo::Nv12, true, false, false));
        assert!(precisa_do_processador(Subtipo::Yuy2, false, false, false));
        assert!(precisa_do_processador(Subtipo::Nv12, false, true, false));
    }

    fn chegada(base: Instant, ms: u64, qpc: i64, dispositivo: Option<i64>, tempo: Option<i64>) -> Chegada {
        Chegada { instante: base + Duration::from_millis(ms), qpc_100ns: qpc, dispositivo_100ns: dispositivo, tempo_da_amostra_100ns: tempo }
    }

    #[test]
    fn o_carimbo_do_dispositivo_ganha_e_leva_a_idade() {
        let origem = Instant::now();
        let mut c = Carimbador::novo(origem);
        // Pelo Frame Server (M15): o `DeviceTimestamp` com ~34 ms de idade, e o `GetSampleTime`
        // igual a ele.
        let q = 1_000_000_000;
        let t = c.carimbar(&chegada(origem, 1000, q, Some(q - 340_000), Some(q - 340_000))).unwrap();
        assert_eq!(c.fonte(), Some(FonteDoCarimbo::Dispositivo));
        assert_eq!(t, origem + Duration::from_millis(1000) - Duration::from_millis(34));
    }

    #[test]
    fn o_tempo_relativo_da_webcam_cai_na_chegada() {
        let origem = Instant::now();
        let mut c = Carimbador::novo(origem);
        // Uma webcam sem `DeviceTimestamp` e com o `GetSampleTime` relativo ao início do fluxo:
        // a diferença para o QPC passa de 1 s, e o carimbo é a chegada.
        let t = c.carimbar(&chegada(origem, 500, 3_000_000_000, None, Some(0))).unwrap();
        assert_eq!(c.fonte(), Some(FonteDoCarimbo::Chegada));
        assert_eq!(t, origem + Duration::from_millis(500));
    }

    #[test]
    fn a_fonte_e_decidida_uma_vez_e_nao_anda_para_tras() {
        let origem = Instant::now();
        let mut c = Carimbador::novo(origem);
        let q = 1_000_000_000;
        let a = c.carimbar(&chegada(origem, 100, q, None, Some(q - 10_000))).unwrap();
        assert_eq!(c.fonte(), Some(FonteDoCarimbo::TempoDaAmostra));
        // Uma trava: o quadro seguinte chega 1,5 s depois com o carimbo de 1,5 s atrás. A fonte não
        // troca para a chegada no meio; o carimbo implausível usa a chegada **deste** quadro.
        let b = c.carimbar(&chegada(origem, 1600, q + 15_000_000, None, Some(q))).unwrap();
        assert_eq!(c.fonte(), Some(FonteDoCarimbo::TempoDaAmostra));
        assert_eq!(c.implausiveis, 1);
        assert!(b > a);
        // Um carimbo que volta no tempo é empurrado para depois do anterior.
        let d = c.carimbar(&chegada(origem, 1601, q + 15_010_000, None, Some(q + 14_000_000))).unwrap();
        assert!(d > b);
        assert_eq!(c.forcados, 1);
    }

    #[test]
    fn o_quadro_anterior_a_origem_e_descartado() {
        let base = Instant::now();
        let origem = base + Duration::from_millis(100);
        let mut c = Carimbador::novo(origem);
        let q = 1_000_000_000;
        // Chegou 10 ms depois da origem com 34 ms de idade: capturado antes dela.
        assert!(c.carimbar(&chegada(base, 110, q, Some(q - 340_000), None)).is_none());
        assert_eq!(c.antes_da_origem, 1);
        assert!(c.carimbar(&chegada(base, 200, q + 900_000, Some(q + 900_000 - 340_000), None)).is_some());
        assert!(c.resumo_das_idades().starts_with("n=2"));
    }

    #[test]
    fn idade_so_entre_zero_e_um_segundo() {
        assert_eq!(idade_plausivel(1_000, 1_000), Some(Duration::ZERO));
        assert_eq!(idade_plausivel(1_000, 2_000), None, "carimbo no futuro");
        assert_eq!(idade_plausivel(20_000_000, 0), None, "relativo ao início do fluxo");
        assert_eq!(idade_plausivel(i64::MIN, 1), None, "estouro");
    }

    #[test]
    fn o_ritmo_e_a_parada() {
        let base = Instant::now();
        assert!(entregar(None, base, 30));
        assert!(!entregar(Some(base), base + Duration::from_millis(16), 30), "60 fps num teto de 30");
        assert!(entregar(Some(base), base + Duration::from_millis(30), 30), "um pouco adiantado passa");
        assert!(!parou(None, base, base + Duration::from_millis(2900)));
        assert!(parou(None, base, base + SEM_QUADRO));
        assert!(!parou(Some(base + Duration::from_secs(2)), base, base + Duration::from_secs(4)));
    }

    // ---------------------------------------------------------------------------------------------
    // A revisão do código da fase 3
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn m4_sem_trinta_o_fps_pesa_antes_da_area() {
        // A sonda do revisor: sem nenhum tipo a 30, a versão anterior escolhia YUY2 1080p a 5 fps.
        let tipos = [
            t(Subtipo::Yuy2, 1920, 1080, 5, 1),
            t(Subtipo::Yuy2, 1280, 720, 10, 1),
            t(Subtipo::Nv12, 1280, 720, 15, 1),
            t(Subtipo::Nv12, 640, 480, 15, 1),
        ];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(2), "NV12 720p a 15 fps");
    }

    #[test]
    fn m4_sessenta_contra_quinze() {
        // Só MJPG 1080p60 e NV12 720p15: o de 60 sai a 30 pelo ritmo, e ganha.
        let tipos = [t(Subtipo::Mjpg, 1920, 1080, 60, 1), t(Subtipo::Nv12, 1280, 720, 15, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(0));
        assert_eq!(fps_efetivo(60.0, 30), 30.0);
        assert_eq!(fps_efetivo(50.0, 30), 25.0);
        assert_eq!(fps_efetivo(30.0, 30), 30.0);
        assert_eq!(fps_efetivo(30.0, 24), 30.0, "o ritmo de 3/4 não reduz 30 a 24");
        // O ritmo de fato entrega a metade de uma câmera a 60.
        let base = Instant::now();
        let mut ultimo = None;
        let mut n = 0;
        for k in 0..60u64 {
            let q = base + Duration::from_micros(k * 16_667);
            if entregar(ultimo, q, 30) {
                ultimo = Some(q);
                n += 1;
            }
        }
        assert_eq!(n, 30);
    }

    #[test]
    fn m4_entre_iguais_sem_dizimar() {
        // 1080p60 e 1080p30: os dois saem a 30, e o que já vem a 30 ganha.
        let tipos = [t(Subtipo::Nv12, 1920, 1080, 60, 1), t(Subtipo::Nv12, 1920, 1080, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(1));
    }

    fn chegada_x10(base: Instant, ms_x10: u64, qpc: i64, disp: Option<i64>) -> Chegada {
        Chegada { instante: base + Duration::from_micros(ms_x10 * 100), qpc_100ns: qpc, dispositivo_100ns: disp, tempo_da_amostra_100ns: disp }
    }

    #[test]
    fn m2_o_implausivel_leva_a_ultima_idade() {
        // A sonda do revisor: compartilhada (67 ms), 30 fps, o quadro 3 sem `DeviceTimestamp`.
        let base = Instant::now();
        let mut c = Carimbador::novo(base);
        let q0: i64 = 1_000_000_000;
        let mut ultimo: Option<Instant> = None;
        let mut carimbos = Vec::new();
        for k in 0..8u64 {
            let qpc = q0 + (k as i64) * 333_333;
            let disp = if k == 3 { None } else { Some(qpc - 670_000) };
            let q = c.carimbar(&chegada_x10(base, 1000 + k * 333, qpc, disp)).unwrap();
            assert!(entregar(ultimo, q, 30), "o quadro {k} entra");
            ultimo = Some(q);
            carimbos.push(q);
        }
        assert_eq!(c.implausiveis, 1);
        assert_eq!(c.forcados, 0, "ninguém empurrado");
        for par in carimbos.windows(2) {
            let d = par[1].duration_since(par[0]).as_micros();
            assert!((33_000..=34_000).contains(&d), "passo de {d} µs");
        }
    }

    #[test]
    fn m3_o_salto_de_meio_segundo_e_compensado() {
        // A sonda do revisor: o `DeviceTimestamp` recua 500 ms de uma vez e fica assim.
        let base = Instant::now();
        let mut c = Carimbador::novo(base);
        let q0: i64 = 1_000_000_000;
        let mut ultimo = None;
        let mut perdidos = 0;
        let mut anterior: Option<Instant> = None;
        for k in 0..40u64 {
            let qpc = q0 + (k as i64) * 333_333;
            let desvio = if k >= 5 { 5_000_000 } else { 0 };
            let q = c.carimbar(&chegada_x10(base, 10_000 + k * 333, qpc, Some(qpc - 340_000 - desvio))).unwrap();
            if let Some(a) = anterior {
                let d = q.duration_since(a).as_micros();
                assert!((33_000..=34_000).contains(&d), "o quadro {k} anda {d} µs");
            }
            anterior = Some(q);
            if entregar(ultimo, q, 30) {
                ultimo = Some(q);
            } else {
                perdidos += 1;
            }
        }
        assert_eq!(perdidos, 0);
        assert_eq!(c.saltos, 1);
        assert_eq!(c.forcados, 0);
    }

    #[test]
    fn m3_o_quadro_a_mais_do_compartilhado_nao_e_salto() {
        // M16 e M36: quando entra um leitor compartilhado, a controladora passa de 34 a 67 ms.
        let base = Instant::now();
        let mut c = Carimbador::novo(base);
        let q0: i64 = 1_000_000_000;
        for k in 0..20u64 {
            let qpc = q0 + (k as i64) * 333_333;
            let idade = if k >= 10 { 670_000 } else { 340_000 };
            c.carimbar(&chegada_x10(base, 10_000 + k * 333, qpc, Some(qpc - idade))).unwrap();
        }
        assert_eq!(c.saltos, 0, "33 ms a mais é idade de verdade");
        assert!(c.resumo_das_idades().contains("max=67"), "{}", c.resumo_das_idades());
    }

    #[test]
    fn m3_a_entrega_atrasada_nao_e_salto() {
        // Três quadros chegam 400 ms atrasados (uma trava na entrega), com o carimbo certo: eles
        // continuam para a frente, e nada é compensado.
        let base = Instant::now();
        let mut c = Carimbador::novo(base);
        let q0: i64 = 1_000_000_000;
        let mut anterior: Option<Instant> = None;
        for k in 0..12u64 {
            let captura = q0 + (k as i64) * 333_333;
            let atraso = if (4..7).contains(&k) { 4_000_000 } else { 340_000 };
            let chegada = captura + atraso;
            let ms_x10 = 10_000 + ((chegada - q0) / 1_000) as u64;
            let q = c.carimbar(&chegada_x10(base, ms_x10, chegada, Some(captura))).unwrap();
            if let Some(a) = anterior {
                assert!(q > a);
            }
            anterior = Some(q);
        }
        assert_eq!(c.saltos, 0);
    }

    #[test]
    fn m1_a_repeticao_nao_faz_o_carimbo_voltar() {
        // A sequência do revisor: webcam a 15,7 fps (63,7 ms, M17) com 64 ms de idade, e a
        // repetição da tela estendida entrando 33 ms depois de cada quadro, carimbada com "agora".
        let base = Instant::now();
        let mut c = Carimbador::novo(base);
        let q0: i64 = 1_000_000_000;
        let mut submetido: Option<Instant> = None;
        let mut bruto: Option<Instant> = None;
        let (mut recuos_sem_defesa, mut empurrados) = (0, 0);
        let mut submeter = |q: Instant, submetido: &mut Option<Instant>, bruto: &mut Option<Instant>| {
            if bruto.is_some_and(|b| q < b) {
                recuos_sem_defesa += 1;
            }
            *bruto = Some(q);
            let (s, empurrado) = carimbo_da_submissao(*submetido, q);
            if empurrado {
                empurrados += 1;
            }
            assert!(submetido.is_none_or(|u| s > u), "o carimbo submetido voltou");
            *submetido = Some(s);
        };
        for k in 0..20u64 {
            let ms_x10 = 1000 + k * 637;
            let qpc = q0 + (k as i64) * 637_000;
            let q = c.carimbar(&chegada_x10(base, ms_x10, qpc, Some(qpc - 640_000))).unwrap();
            submeter(q, &mut submetido, &mut bruto);
            let repeticao = base + Duration::from_micros(ms_x10 * 100) + Duration::from_millis(33);
            submeter(repeticao, &mut submetido, &mut bruto);
        }
        assert_eq!(recuos_sem_defesa, 19, "sem a defesa, cada quadro depois de uma repetição volta: é o M1");
        assert_eq!(empurrados, 19);
        assert_eq!(c.forcados, 0, "o carimbador não vê a repetição: a defesa tem de ser na submissão");
    }

    #[test]
    fn m3_a_geometria_pela_abertura() {
        // 1920x1088 com 1080 linhas de imagem: a abertura manda.
        let g = geometria(1920, 1088, Some((0, 0, 1920, 1080)));
        assert_eq!((g.x, g.y, g.largura, g.altura), (0, 0, 1920, 1080));
        assert!(g.recortada());
        // Sem abertura, o quadro inteiro.
        assert!(!geometria(1280, 720, None).recortada());
        // Fora do quadro, ou pequena demais: o quadro inteiro.
        assert_eq!(geometria(1280, 720, Some((0, 0, 1280, 736))), Geometria::inteira(1280, 720));
        assert_eq!(geometria(1280, 720, Some((-2, 0, 1280, 720))), Geometria::inteira(1280, 720));
        assert_eq!(geometria(1280, 720, Some((0, 0, 8, 8))), Geometria::inteira(1280, 720));
        // Ímpar vira par, para dentro.
        let g = geometria(1280, 720, Some((1, 3, 1277, 715)));
        assert_eq!((g.x, g.y, g.largura, g.altura), (2, 4, 1276, 714));
        // O blob do `MFVideoArea`: OffsetX {fract, value}, OffsetY {fract, value}, cx, cy.
        let mut blob = Vec::new();
        blob.extend_from_slice(&0u16.to_le_bytes());
        blob.extend_from_slice(&8i16.to_le_bytes());
        blob.extend_from_slice(&0u16.to_le_bytes());
        blob.extend_from_slice(&4i16.to_le_bytes());
        blob.extend_from_slice(&1904i32.to_le_bytes());
        blob.extend_from_slice(&1080i32.to_le_bytes());
        assert_eq!(abertura_do_blob(&blob), Some((8, 4, 1904, 1080)));
        assert_eq!(abertura_do_blob(&blob[..12]), None);
    }

    #[test]
    fn m2_o_texto_so_culpa_outro_app_com_o_codigo() {
        let ocupada = FalhaDaTentativa { texto: "0xC00D3704".into(), causa: causa_da_falha(Some(HR_OCUPADA), false) };
        let troca = FalhaDaTentativa { texto: "o tipo mudou".into(), causa: causa_da_falha(None, false) };
        let lenta = FalhaDaTentativa { texto: "nenhum quadro em 15 s".into(), causa: causa_da_falha(None, true) };
        assert!(texto_da_abertura_que_falhou(&ocupada, Some(&troca)).starts_with("A câmera está em uso por outro app"));
        assert!(texto_da_abertura_que_falhou(&troca, Some(&troca)).starts_with("A câmera não abriu"));
        assert!(texto_da_abertura_que_falhou(&lenta, Some(&lenta)).starts_with("A câmera não mandou nenhum quadro"));
        let removida = FalhaDaTentativa { texto: "0xC00D3EA2".into(), causa: causa_da_falha(Some(HR_REMOVIDA), false) };
        assert!(texto_da_abertura_que_falhou(&removida, None).starts_with("A câmera não está mais conectada"));
        let negada = FalhaDaTentativa { texto: "E_ACCESSDENIED".into(), causa: causa_da_falha(Some(HR_NEGADA), false) };
        assert!(texto_da_abertura_que_falhou(&negada, None).contains("Privacidade"));
        // Só recua quando a compartilhada pode dar outra resposta.
        assert!(recuar_para_compartilhada(CausaDaFalha::Ocupada));
        assert!(recuar_para_compartilhada(CausaDaFalha::Demorou));
        assert!(recuar_para_compartilhada(CausaDaFalha::Outra));
        assert!(!recuar_para_compartilhada(CausaDaFalha::Removida));
        assert!(!recuar_para_compartilhada(CausaDaFalha::Negada));
    }

    #[test]
    fn m9_a_espera_e_curta_e_olha_o_parar() {
        // A premissa medida (M41): a fonte levou 5.132 ms, **antes** da espera; o primeiro quadro, 41 ms.
        assert!(ESPERA_DO_PRIMEIRO_QUADRO >= Duration::from_millis(41) * 100, "folga de 100 vezes o medido");
        assert!(ESPERA_DO_PRIMEIRO_QUADRO * 2 <= Duration::from_secs(10), "as duas tentativas cabem nos 10 s do Android");
        // A resposta que vem.
        let caixa = std::sync::Arc::new((Mutex::new(None::<u32>), Condvar::new()));
        let c2 = caixa.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            *c2.0.lock().unwrap() = Some(7);
            c2.1.notify_all();
        });
        assert_eq!(esperar_resposta(&caixa.0, &caixa.1, Duration::from_secs(5), &|| false), Espera::Veio(7));
        t.join().unwrap();
        // O prazo.
        let vazia = (Mutex::new(None::<u32>), Condvar::new());
        let comeco = Instant::now();
        assert_eq!(esperar_resposta(&vazia.0, &vazia.1, Duration::from_millis(150), &|| false), Espera::Prazo);
        assert!(comeco.elapsed() >= Duration::from_millis(150));
        // **O Parar**, no meio de um prazo de 5 s: sai em uma fatia.
        let parar = std::sync::atomic::AtomicBool::new(false);
        let comeco = Instant::now();
        std::thread::scope(|e| {
            e.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                parar.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            let r = esperar_resposta(&vazia.0, &vazia.1, ESPERA_DO_PRIMEIRO_QUADRO, &|| parar.load(std::sync::atomic::Ordering::SeqCst));
            assert_eq!(r, Espera::Parada);
        });
        assert!(comeco.elapsed() < Duration::from_millis(100) + FATIA_DA_ESPERA * 4, "{:?}", comeco.elapsed());
        // Cancelada não recua, e o texto diz o Parar.
        assert!(!recuar_para_compartilhada(CausaDaFalha::Cancelada));
        let f = FalhaDaTentativa { texto: "Parar".into(), causa: CausaDaFalha::Cancelada };
        assert!(texto_da_abertura_que_falhou(&f, None).contains("interrompida pelo Parar"));
    }

    #[test]
    fn fase4_a_tela_recebe_so_a_frase() {
        let ocupada = FalhaDaTentativa { texto: "o leitor devolveu HRESULT(0xC00D3704) [fonte 24 ms]".into(), causa: CausaDaFalha::Ocupada };
        let lenta = FalhaDaTentativa { texto: "nenhum quadro nem falha em 5 s".into(), causa: CausaDaFalha::Demorou };
        let t = texto_da_abertura_que_falhou(&ocupada, Some(&lenta));
        assert_eq!(so_a_frase(&t), "A câmera está em uso por outro app.");
        assert!(t.contains("0xC00D3704"), "o detalhe fica no texto inteiro, para o registro");
        assert_eq!(so_a_frase("A câmera \"X\" foi desconectada."), "A câmera \"X\" foi desconectada.");
    }

    #[test]
    fn m9_a_matriz_padrao_e_a_bt601() {
        assert!(!matriz_709(None), "UVC e JFIF são BT.601");
        assert!(matriz_709(Some(true)));
        assert!(!matriz_709(Some(false)));
    }

    #[test]
    fn m1_none_depois_de_habilitada_e_sumico_confirmado() {
        let mut t = TestemunhaDaInterface::default();
        assert!(t.presente(None), "None antes de ver habilitada não derruba");
        assert!(t.presente(Some(true)));
        // Um `None` passageiro, e outro: a câmera continua (a reconferência).
        for _ in 0..AUSENCIAS_PARA_SUMICO - 1 {
            assert!(t.presente(None));
        }
        assert!(t.presente(Some(true)), "a leitura que volta zera a conta");
        for _ in 0..AUSENCIAS_PARA_SUMICO - 1 {
            assert!(t.presente(None));
        }
        assert!(!t.presente(None), "o quinto None seguido é sumiço (~1 s)");
        assert!(!TestemunhaDaInterface::default().presente(Some(false)), "Some(false) derruba na hora");
    }

    #[test]
    fn reconferencia_a_superficie_tem_limite() {
        let g = geometria(1920, 1080, None);
        assert_eq!(superficie_aceita(1920, 1080, &g), Ok(false), "a superfície inteira, sem recorte");
        assert_eq!(superficie_aceita(1920, 1088, &g), Ok(true), "o alinhamento de um decodificador");
        assert!(superficie_aceita(1920, 1072, &g).is_err(), "menor que a imagem");
        assert!(superficie_aceita(3840, 2160, &g).is_err(), "um tamanho que mudou sem aviso");
        assert!(superficie_aceita(1920, 1152, &g).is_err(), "72 linhas a mais passam do alinhamento de 64");
        // A abertura em (16, 8) de um quadro 1296x736: a superfície do quadro serve e recorta.
        let g = geometria(1296, 736, Some((16, 8, 1280, 720)));
        assert_eq!(superficie_aceita(1296, 736, &g), Ok(true));
        // O tipo 1280x720 numa superfície 1280x736 (o ramo (ii) do M3).
        assert_eq!(superficie_aceita(1280, 736, &Geometria::inteira(1280, 720)), Ok(true));
        // A geometria na superfície: o quadro passa a ser o da superfície, a imagem fica.
        let s = Geometria::inteira(1280, 720).na_superficie(1280, 736);
        assert_eq!((s.quadro_largura, s.quadro_altura, s.largura, s.altura), (1280, 736, 1280, 720));
    }

    #[test]
    fn reconferencia_o_relogio_que_volta_desfaz_a_compensacao() {
        // A sonda do revisor (`r_salto_que_volta`): o relógio recua 500 ms e, 15 quadros depois, volta.
        let base = Instant::now();
        let mut c = Carimbador::novo(base);
        let q0: i64 = 1_000_000_000;
        let mut ultimo = None;
        let mut perdidos = 0;
        let mut anterior: Option<Instant> = None;
        for k in 0..40u64 {
            let qpc = q0 + (k as i64) * 333_333;
            let desvio = if (5..20).contains(&k) { 5_000_000 } else { 0 };
            let q = c.carimbar(&chegada_x10(base, 10_000 + k * 333, qpc, Some(qpc - 340_000 - desvio))).unwrap();
            if let Some(a) = anterior {
                let d = q.duration_since(a).as_micros();
                assert!((33_000..=34_000).contains(&d), "o quadro {k} anda {d} µs");
            }
            anterior = Some(q);
            if entregar(ultimo, q, 30) {
                ultimo = Some(q);
            } else {
                perdidos += 1;
            }
        }
        assert_eq!((perdidos, c.saltos, c.voltas, c.forcados), (0, 1, 1, 0));
        // A idade ficou nos 34 ms do começo ao fim, e não foi a zero depois da volta.
        assert!(c.resumo_das_idades().contains("min=34.00"), "{}", c.resumo_das_idades());
        assert!(c.resumo_das_idades().contains("max=34.00"), "{}", c.resumo_das_idades());
    }

    #[test]
    fn reconferencia_o_conversor_cheio_tem_prazo() {
        let agora = Instant::now();
        assert!(!conversor_travado(None, agora));
        assert!(!conversor_travado(Some(agora), agora + Duration::from_millis(1_999)));
        assert!(conversor_travado(Some(agora), agora + PRAZO_DO_CONVERSOR_CHEIO));
    }

    #[test]
    fn r4_a_segunda_sessao_do_processo_abre_compartilhada_depois_da_vez() {
        // O R4 (M51): duas sessões do mesmo processo, a mesma câmera, ao mesmo tempo.
        let mut a = AberturasDoProcesso::novo();
        let link = r"\\?\swd#vcamdevapi#a5de#{e5323777}\{fceb}";
        assert_eq!(a.pedir_a_vez(link, true), Some(PlanoDaAbertura::ControladoraComRecuo), "a primeira controla");
        // A segunda chega com a primeira ainda abrindo (a fonte de um nó novo leva ~5 s): espera.
        assert_eq!(a.pedir_a_vez(link, true), None);
        // O link vem com outra caixa numa enumeração: é a mesma câmera.
        assert_eq!(a.pedir_a_vez(&link.to_ascii_uppercase(), true), None);
        a.fim_da_abertura(link, Some(Papel::Controladora));
        assert_eq!(a.estado(link), (1, 0, false));
        assert_eq!(a.pedir_a_vez(link, true), Some(PlanoDaAbertura::SoCompartilhada { outras: 1 }), "compartilhada direto");
        a.fim_da_abertura(link, Some(Papel::Compartilhada));
        assert_eq!(a.estado(link), (2, 0, false));
        // Outra câmera não espera por esta.
        assert_eq!(a.pedir_a_vez("outra", true), Some(PlanoDaAbertura::ControladoraComRecuo));
        a.fim_da_abertura("outra", None);
        assert_eq!(a.estado("outra"), (0, 0, false));
    }

    #[test]
    fn r4_a_abertura_que_falha_devolve_a_vez_sem_contar() {
        let mut a = AberturasDoProcesso::novo();
        assert_eq!(a.pedir_a_vez("c", true), Some(PlanoDaAbertura::ControladoraComRecuo));
        a.fim_da_abertura("c", None);
        // A seguinte tenta controladora de novo: ninguém do processo tem a câmera.
        assert_eq!(a.pedir_a_vez("c", true), Some(PlanoDaAbertura::ControladoraComRecuo));
        a.fim_da_abertura("c", Some(Papel::Controladora));
        assert_eq!(a.estado("c"), (1, 0, false));
    }

    #[test]
    fn r4_quem_recomeca_espera_a_soltura_da_controladora_que_sai() {
        let mut a = AberturasDoProcesso::novo();
        assert!(a.pedir_a_vez("c", true).is_some());
        a.fim_da_abertura("c", Some(Papel::Controladora));
        // A sessão para: a soltura vai para outra thread.
        a.comecou_a_soltar("c", Papel::Controladora);
        assert_eq!(a.estado("c"), (0, 1, false));
        // Quem recomeça na hora espera a soltura (dentro de `ESPERA_PELAS_SOLTURAS`)...
        assert_eq!(a.pedir_a_vez("c", true), None);
        // ...e, passado o prazo, abre sem ela: controladora, porque nenhuma está viva.
        assert_eq!(a.pedir_a_vez("c", false), Some(PlanoDaAbertura::ControladoraComRecuo));
        a.fim_da_abertura("c", Some(Papel::Controladora));
        a.terminou_de_soltar("c");
        assert_eq!(a.estado("c"), (1, 0, false));
        // Com a controladora viva e uma compartilhada soltando, a nova é compartilhada, esperando ou não.
        assert!(a.pedir_a_vez("c", true).is_some());
        a.fim_da_abertura("c", Some(Papel::Compartilhada));
        a.comecou_a_soltar("c", Papel::Compartilhada);
        assert_eq!(a.estado("c"), (1, 1, false));
        assert_eq!(a.pedir_a_vez("c", false), Some(PlanoDaAbertura::SoCompartilhada { outras: 1 }));
        a.fim_da_abertura("c", None);
        a.terminou_de_soltar("c");
        a.comecou_a_soltar("c", Papel::Controladora);
        a.terminou_de_soltar("c");
        assert_eq!(a.estado("c"), (0, 0, false));
        assert!(a.links.is_empty(), "o link sem nada sai da lista");
        // Soltar a mais não faz conta negativa.
        a.terminou_de_soltar("c");
        a.comecou_a_soltar("c", Papel::Compartilhada);
        assert_eq!(a.estado("c"), (0, 1, false));
    }

    /// **A revisão do código da fase 4, M1**: o que decide a compartilhada direto é haver uma
    /// **controladora** viva, e não uma captura qualquer.
    #[test]
    fn c4_m1_sem_controladora_viva_a_terceira_tenta_controladora() {
        let mut a = AberturasDoProcesso::novo();
        let l = r"\\?\swd#cam";
        // A sonda `b_a_terceira_depois_da_controladora_sair` do revisor.
        assert_eq!(a.pedir_a_vez(l, true), Some(PlanoDaAbertura::ControladoraComRecuo)); // [#1]
        a.fim_da_abertura(l, Some(Papel::Controladora));
        assert_eq!(a.pedir_a_vez(l, true), Some(PlanoDaAbertura::SoCompartilhada { outras: 1 })); // [#2]
        a.fim_da_abertura(l, Some(Papel::Compartilhada));
        a.comecou_a_soltar(l, Papel::Controladora); // a [#1] sai
        a.terminou_de_soltar(l);
        assert_eq!(a.estado(l), (1, 0, false), "a [#2] compartilhada ficou");
        assert!(!a.ha_controladora(l));
        assert_eq!(a.pedir_a_vez(l, true), Some(PlanoDaAbertura::ControladoraComRecuo), "a [#3] tenta controladora");
        a.fim_da_abertura(l, Some(Papel::Controladora));
        assert!(a.ha_controladora(l));
        // E a quarta, com a [#3] controladora viva, volta a ser compartilhada direto.
        assert_eq!(a.pedir_a_vez(l, true), Some(PlanoDaAbertura::SoCompartilhada { outras: 2 }));
        a.fim_da_abertura(l, None);
    }

    /// A controladora sai **no meio** da abertura da segunda (a sonda
    /// `b_a_controladora_sai_no_meio_da_abertura_da_segunda`): o plano era compartilhada, e, se ela
    /// falhar, o recuo para controladora vale porque não há mais controladora viva.
    #[test]
    fn c4_m1_a_compartilhada_que_falha_sem_controladora_recua() {
        let mut a = AberturasDoProcesso::novo();
        let l = "c";
        a.pedir_a_vez(l, true);
        a.fim_da_abertura(l, Some(Papel::Controladora));
        assert_eq!(a.pedir_a_vez(l, true), Some(PlanoDaAbertura::SoCompartilhada { outras: 1 }));
        assert!(a.ha_controladora(l), "com a controladora viva, a falha da compartilhada não recua");
        a.comecou_a_soltar(l, Papel::Controladora); // sai enquanto a segunda abre
        assert!(!a.ha_controladora(l), "sem ela, recua");
        a.terminou_de_soltar(l);
        a.fim_da_abertura(l, Some(Papel::Controladora)); // o recuo abriu como controladora
        assert!(a.ha_controladora(l));
        assert_eq!(a.estado(l), (1, 0, false));
        // O recuo segue a regra do recuo contrário.
        assert!(recuar_para_controladora(CausaDaFalha::Demorou));
        assert!(recuar_para_controladora(CausaDaFalha::Outra));
        assert!(!recuar_para_controladora(CausaDaFalha::Removida));
        assert!(!recuar_para_controladora(CausaDaFalha::Negada));
        assert!(!recuar_para_controladora(CausaDaFalha::Cancelada));
    }

    #[test]
    fn r4_o_vigia_da_camera_encerra_na_primeira_testemunha() {
        let t0 = Instant::now();
        let tol = Duration::from_secs(3);
        // A [#1] do R4: o fim do leitor (0xC00D3EA3) foi a primeira testemunha.
        let mut v = VigiaDaFonte::novo(true);
        assert_eq!(v.olhar(false, false, t0, tol), Volta::default());
        let r = v.olhar(true, false, t0 + Duration::from_millis(200), tol);
        assert!(r.registrar && r.primeira && r.sistema_primeiro && r.encerrar, "{r:?}");
        assert_eq!(r.sistema_depois, None, "e não \"disparou 0 ms depois da primeira testemunha\"");
        // A [#2]: a interface foi a primeira, e a sessão sai na mesma volta.
        let mut v = VigiaDaFonte::novo(true);
        let r = v.olhar(false, true, t0, tol);
        assert!(r.registrar && r.primeira && !r.sistema_primeiro && r.encerrar, "{r:?}");
    }

    #[test]
    fn r4_o_vigia_do_monitor_registra_so_as_transicoes() {
        let t0 = Instant::now();
        let tol = Duration::from_secs(3);
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut v = VigiaDaFonte::novo(false);
        let mut linhas = 0;
        // A enumeração perde o monitor; o `Closed` vem 400 ms depois; 3 s de tolerância em voltas de 200 ms.
        let mut fim = None;
        for k in 0..=16u64 {
            let r = v.olhar(k >= 2, true, ms(k * 200), tol);
            if r.registrar {
                linhas += 1;
            }
            if k == 0 {
                assert!(r.primeira && !r.sistema_primeiro && !r.encerrar);
            }
            if k == 2 {
                assert_eq!(r.sistema_depois, Some(Duration::from_millis(400)));
            } else {
                assert_eq!(r.sistema_depois, None);
            }
            if r.encerrar && fim.is_none() {
                fim = Some(k);
            }
        }
        assert_eq!(linhas, 2, "a primeira testemunha e a chegada do Closed; no R4 eram 16 linhas");
        assert_eq!(fim, Some(15), "a tolerância conta da primeira testemunha");
        assert!(v.sistema_visto());
        // Uma testemunha que volta atrás é uma transição, e a tolerância continua contando.
        let mut v = VigiaDaFonte::novo(false);
        assert!(v.olhar(false, true, ms(0), tol).registrar);
        let r = v.olhar(false, false, ms(200), tol);
        assert!(r.registrar && !r.encerrar);
        assert!(!v.olhar(false, false, ms(400), tol).registrar);
        assert!(v.olhar(false, false, ms(3000), tol).encerrar);
        assert!(!v.sistema_visto());
    }

    #[test]
    fn r4_o_fim_da_camera_da_a_frase_e_guarda_o_detalhe() {
        let nome = "Quall-bancada sonda 11060 (Câmera Virtual do Windows)";
        let tomada = FimDaCamera {
            motivo: "o leitor devolveu HRESULT(0xC00D3EA3) (O dispositivo…)".into(),
            desconectada: false,
            tomada: true,
            negada: false,
            formato_mudou: false,
        };
        let t = texto_do_fim_da_camera(nome, Some(&tomada), false);
        assert_eq!(so_a_frase(&t), format!("Outro app tomou a câmera \"{nome}\"."));
        assert!(t.contains("0xC00D3EA3"), "o HRESULT fica no registro");
        // A interface que sumiu é desconexão, diga o leitor o que disser.
        assert_eq!(so_a_frase(&texto_do_fim_da_camera(nome, Some(&tomada), true)), format!("A câmera \"{nome}\" foi desconectada."));
        // O R4, a [#2]: a interface primeiro, o leitor sem fim ainda.
        assert_eq!(texto_do_fim_da_camera(nome, None, true), format!("A câmera \"{nome}\" foi desconectada."));
        let removida = FimDaCamera { motivo: "0xC00D3EA2".into(), desconectada: true, tomada: false, negada: false, formato_mudou: false };
        assert_eq!(so_a_frase(&texto_do_fim_da_camera(nome, Some(&removida), false)), format!("A câmera \"{nome}\" foi desconectada."));
        let parada = FimDaCamera { motivo: "nenhum quadro da câmera há 3 s".into(), desconectada: false, tomada: false, negada: false, formato_mudou: false };
        let t = texto_do_fim_da_camera(nome, Some(&parada), false);
        assert_eq!(so_a_frase(&t), format!("A câmera \"{nome}\" parou."));
        assert!(t.ends_with("nenhum quadro da câmera há 3 s"));
    }

    // ---------------------------------------------------------------------------------------------
    // A câmera que pausa (22/09): não encerra, repete, e diz na tela
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn pausa_a_parada_e_estado_e_nao_fim() {
        let base = Instant::now();
        assert_eq!(parada_ha(None, base, base + Duration::from_millis(2900)), None);
        assert_eq!(parada_ha(None, base, base + SEM_QUADRO), Some(SEM_QUADRO));
        let ultimo = base + Duration::from_secs(10);
        assert_eq!(parada_ha(Some(ultimo), base, ultimo + Duration::from_secs(2)), None);
        assert_eq!(parada_ha(Some(ultimo), base, ultimo + Duration::from_secs(95)), Some(Duration::from_secs(95)));
        // O relógio que anda para trás não dá pânico nem parada.
        assert_eq!(parada_ha(Some(ultimo), base, base), None);
        assert_eq!(texto_da_camera_parada(Duration::from_millis(12_900)), "Câmera parada há 12 s — esperando ela voltar.");
        assert!(e_texto_da_camera_parada(&texto_da_camera_parada(Duration::from_secs(3))));
        assert!(!e_texto_da_camera_parada("câmera 854×480 · 120 quadros · 2 IDR"));
    }

    /// Em inglês (a tradução EN/PT): as frases da tela nascem no idioma da hora, o detalhe vai como
    /// veio, e a câmera parada é reconhecida nos dois idiomas (o idioma pode mudar com ela no ar).
    #[test]
    fn as_frases_da_camera_em_ingles() {
        use crate::idioma::{com_idioma, Idioma};
        let ocupada = FalhaDaTentativa { texto: "o leitor devolveu HRESULT(0xC00D3704)".into(), causa: CausaDaFalha::Ocupada };
        let (parada_en, aberta_en, fim_en) = com_idioma(Idioma::En, || {
            (
                texto_da_camera_parada(Duration::from_secs(12)),
                texto_da_abertura_que_falhou(&ocupada, None),
                texto_do_fim_da_camera("Video Edit", None, false),
            )
        });
        assert_eq!(parada_en, "Camera paused for 12 s — waiting for it to come back.");
        assert_eq!(so_a_frase(&aberta_en), "The camera is in use by another app.");
        assert!(aberta_en.contains("0xC00D3704"));
        assert_eq!(fim_en, "The camera \"Video Edit\" was disconnected.");
        // Reconhecida em português também com o texto montado em inglês, e vice-versa.
        assert!(e_texto_da_camera_parada(&parada_en));
        assert!(com_idioma(Idioma::En, || e_texto_da_camera_parada(&texto_da_camera_parada(Duration::from_secs(4)))));
        assert!(com_idioma(Idioma::En, || e_texto_da_camera_parada("Câmera parada há 4 s — esperando ela voltar.")));
    }

    #[test]
    fn pausa_parar_voltar_e_parar_de_novo() {
        let t0 = Instant::now();
        let mut p = PausaDaCamera::default();
        assert_eq!(p.olhar(None, t0), MudancaDaPausa::Nenhuma);
        // Parou: o último quadro chegou 3 s antes desta volta.
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(p.olhar(Some(Duration::from_secs(3)), t1), MudancaDaPausa::Parou { ha: Duration::from_secs(3) });
        // Uma linha por transição: as voltas seguintes não dizem nada.
        assert_eq!(p.olhar(Some(Duration::from_millis(3200)), t1 + Duration::from_millis(200)), MudancaDaPausa::Nenhuma);
        assert_eq!(p.parada_ha(t1 + Duration::from_secs(20)), Some(Duration::from_secs(23)));
        // Voltou 45 s depois de detectada: 48 s sem quadro.
        let t2 = t1 + Duration::from_secs(45);
        assert_eq!(p.olhar(None, t2), MudancaDaPausa::Voltou { depois_de: Duration::from_secs(48) });
        assert_eq!(p.parada_ha(t2), None);
        // Parou de novo, mais curta.
        let t3 = t2 + Duration::from_secs(30);
        assert!(matches!(p.olhar(Some(Duration::from_secs(3)), t3), MudancaDaPausa::Parou { .. }));
        assert!(matches!(p.olhar(None, t3 + Duration::from_secs(2)), MudancaDaPausa::Voltou { .. }));
        assert_eq!(p.pausas, 2);
        assert_eq!(p.maior, Duration::from_secs(48));
        assert_eq!(p.resumo(t3 + Duration::from_secs(3)), "pausas=2 maior_ms=48000");
    }

    #[test]
    fn pausa_que_nunca_comecou_e_a_do_fim() {
        // A câmera que nunca mandou quadro é contada da abertura (a abertura já esperou o primeiro).
        let t0 = Instant::now();
        let mut p = PausaDaCamera::default();
        let agora = t0 + Duration::from_secs(4);
        assert!(matches!(p.olhar(parada_ha(None, t0, agora), agora), MudancaDaPausa::Parou { .. }));
        assert_eq!(p.resumo(agora + Duration::from_secs(6)), "pausas=1 maior_ms=10000 (parada no fim)");
    }

    #[test]
    fn pausa_so_tem_teto_sem_a_interface_confirmada() {
        let t0 = Instant::now();
        let mut p = PausaDaCamera::default();
        p.olhar(Some(SEM_QUADRO), t0);
        let quase = t0 + TETO_DA_PAUSA_SEM_INTERFACE - SEM_QUADRO - Duration::from_millis(1);
        assert!(!p.encerrar(quase, false));
        let passou = t0 + TETO_DA_PAUSA_SEM_INTERFACE - SEM_QUADRO;
        assert!(p.encerrar(passou, false), "sem a interface confirmada, 60 s parada encerram");
        assert!(!p.encerrar(passou + Duration::from_secs(3600), true), "com a interface confirmada, nunca");
        // Sem pausa, nada a encerrar.
        assert!(!PausaDaCamera::default().encerrar(passou, false));
    }

    #[test]
    fn repeticao_so_com_a_camera_parada_e_espacada() {
        let t0 = Instant::now();
        // Sem quadro real ainda: nada a repetir.
        assert!(!repetir_a_camera(None, None, t0 + Duration::from_secs(5)));
        // Uma câmera lenta (5 fps, 200 ms entre quadros) nunca recebe repetição.
        assert!(!repetir_a_camera(Some(t0), Some(t0), t0 + Duration::from_millis(200)));
        assert!(!repetir_a_camera(Some(t0), Some(t0), t0 + Duration::from_millis(399)));
        // Parada há 400 ms: repete.
        assert!(repetir_a_camera(Some(t0), Some(t0), t0 + REPETIR_A_CAMERA_PARADA_APOS));
        // E de 100 em 100 ms, contados da última submissão (a repetição conta).
        let rep = t0 + Duration::from_millis(450);
        assert!(!repetir_a_camera(Some(t0), Some(rep), rep + Duration::from_millis(99)));
        assert!(repetir_a_camera(Some(t0), Some(rep), rep + INTERVALO_DA_REPETICAO_DA_CAMERA));
        // Os receptores de 10 s: com repetição a cada 100 ms, o maior buraco no fio é ~400 ms.
        assert!(REPETIR_A_CAMERA_PARADA_APOS < Duration::from_millis(500), "abaixo da placa da câmera virtual");
        assert!(INTERVALO_DA_REPETICAO_DA_CAMERA * 64 < Duration::from_secs(10), "64 pacotes do anel antes dos 10 s");
    }

    #[test]
    fn repeticao_so_da_posicao_intacta_do_anel() {
        let mut r = PosicaoRepetivel::default();
        assert_eq!(r.posicao(), None, "nenhuma cópia boa ainda");
        r.tomada();
        r.copiada(2);
        assert_eq!(r.posicao(), Some(2));
        // Uma posição tomada depois (a cópia que falha): pula até a próxima cópia boa.
        r.tomada();
        assert_eq!(r.posicao(), None);
        // Uma volta inteira de falhas não ressuscita a 2 (o furo da conta pela próxima posição).
        for _ in 0..6 {
            r.tomada();
        }
        assert_eq!(r.posicao(), None);
        r.tomada();
        r.copiada(3);
        assert_eq!(r.posicao(), Some(3));
    }

    #[test]
    fn pausas_de_bancada() {
        let p = ler_pausas_de_bancada("20:15, 60:40").unwrap();
        assert_eq!(p, vec![(Duration::from_secs(20), Duration::from_secs(15)), (Duration::from_secs(60), Duration::from_secs(40))]);
        assert!(!na_pausa_de_bancada(&p, Duration::from_secs(19)));
        assert!(na_pausa_de_bancada(&p, Duration::from_secs(20)));
        assert!(na_pausa_de_bancada(&p, Duration::from_millis(34_999)));
        assert!(!na_pausa_de_bancada(&p, Duration::from_secs(35)));
        assert!(na_pausa_de_bancada(&p, Duration::from_secs(99)));
        assert!(ler_pausas_de_bancada("20").is_err());
        assert!(ler_pausas_de_bancada("20:0").is_err());
        assert!(ler_pausas_de_bancada("-1:5").is_err());
        assert!(ler_pausas_de_bancada("").is_err());
    }

    #[test]
    fn o_formato_que_muda_tem_frase_propria() {
        let nome = "Video Edit";
        let mut f = FimDaCamera::pelo_codigo("o formato da câmera mudou no meio (Yuy2 720x480 → Yuy2 320x240)".into(), false, None);
        assert_eq!(so_a_frase(&texto_do_fim_da_camera(nome, Some(&f), false)), "A câmera \"Video Edit\" parou.");
        f.formato_mudou = true;
        let t = texto_do_fim_da_camera(nome, Some(&f), false);
        assert_eq!(so_a_frase(&t), "A câmera \"Video Edit\" mudou de formato: comece a transmissão de novo.");
        assert!(t.ends_with("320x240)"), "o detalhe fica no registro");
        // A interface que sumiu continua sendo desconexão.
        assert_eq!(so_a_frase(&texto_do_fim_da_camera(nome, Some(&f), true)), "A câmera \"Video Edit\" foi desconectada.");
    }

    #[test]
    fn a_testemunha_diz_se_ja_leu_habilitada() {
        let mut t = TestemunhaDaInterface::default();
        assert!(!t.ja_habilitada());
        assert!(t.presente(None));
        assert!(!t.ja_habilitada(), "None não confirma");
        assert!(t.presente(Some(true)));
        assert!(t.ja_habilitada());
    }

    #[test]
    fn reconferencia_o_milesimo_de_fps_nao_passa_na_frente_da_area() {
        // A sonda do revisor (`r_milesimo_de_fps`): 1080p a 15000/1001 perdia para 480p a 15.
        let tipos = [t(Subtipo::Nv12, 1920, 1080, 15000, 1001), t(Subtipo::Nv12, 640, 480, 15, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(0));
        // E meio fps ainda separa: 720p a 15 ganha de 1080p a 10.
        let tipos = [t(Subtipo::Nv12, 1920, 1080, 10, 1), t(Subtipo::Nv12, 1280, 720, 15, 1)];
        assert_eq!(escolher_tipo_nativo(&tipos, TETO_1080P30), Some(1));
    }

    /// **Os relógios de uma webcam** (a fase 5, §5): o `DeviceTimestamp` em QPC e o `GetSampleTime`
    /// relativo ao começo do fluxo — o caso que a documentação descreve e que só uma câmera de
    /// verdade mostra. A contagem separa os dois, e o passo é o do aparelho.
    #[test]
    fn fase5_os_relogios_de_uma_webcam() {
        let base = Instant::now();
        let qpc0: i64 = 2_165_052_422_713; // ~60 h desde o boot, em 100 ns (M6)
        let mut r = RelogiosDoFluxo::novo();
        // Três quadros a 15 fps (66,7 ms): o aparelho carimba 40 ms antes da chegada; o tempo da
        // amostra conta do começo do fluxo.
        for k in 0..3i64 {
            let disp = qpc0 + k * 666_667;
            let c = chegada(base, (k * 67) as u64, disp + 400_000, Some(disp), Some(k * 666_667));
            let passo = r.observar(&c);
            if k == 0 {
                assert_eq!(passo, None, "o primeiro não tem passo");
            } else {
                assert_eq!(passo, Some(Duration::from_nanos(66_666_700)));
            }
        }
        assert_eq!((r.quadros, r.com_dispositivo, r.dispositivo_plausivel), (3, 3, 3));
        assert_eq!(r.tempo_plausivel, 0, "o GetSampleTime relativo não é QPC");
        assert_eq!(r.tempo_igual_ao_dispositivo, 0);
        let p = r.primeiro_em_texto().unwrap();
        assert!(p.contains("GetSampleTime=0.00 ms"), "{p}");
        assert!(p.contains("QPC−DeviceTimestamp=40.00 ms"), "{p}");
        let resumo = r.resumo();
        assert!(resumo.contains("DeviceTimestamp=3 (plausível em 3) GetSampleTime_como_QPC=0"), "{resumo}");
        assert!(resumo.contains("p50=66.67"), "{resumo}");
        assert!(resumo.contains("≈15.0 fps"), "{resumo}");
    }

    /// Sem `DeviceTimestamp` (a fonte do Quall no processo, M6), o passo sai do `GetSampleTime`; o
    /// relógio que volta conta como volta, e o que pula mais de 1 s (o cabo da Canon que volta)
    /// conta como salto, com o maior.
    #[test]
    fn fase5_os_relogios_sem_o_do_aparelho_e_os_saltos() {
        let base = Instant::now();
        let mut r = RelogiosDoFluxo::novo();
        let q = 1_000_000_000i64;
        assert_eq!(r.observar(&chegada(base, 0, q, None, Some(q))), None);
        assert_eq!(r.observar(&chegada(base, 33, q + 333_333, None, Some(q + 333_333))), Some(Duration::from_nanos(33_333_300)));
        assert_eq!(r.tempo_plausivel, 2, "o GetSampleTime da fonte do Quall é QPC");
        assert_eq!(r.com_dispositivo, 0);
        // O relógio volta: não é passo, e conta.
        assert_eq!(r.observar(&chegada(base, 66, q + 666_666, None, Some(q))), None);
        assert_eq!(r.voltas_do_dispositivo, 1);
        // Um pulo de 2,5 s para a frente.
        assert_eq!(r.observar(&chegada(base, 2600, q + 25_000_000, None, Some(q + 25_000_000))), Some(Duration::from_millis(2500)));
        assert_eq!(r.saltos_do_dispositivo, 1);
        assert_eq!(r.maior_salto, Some(Duration::from_millis(2500)));
        assert!(r.resumo().contains("saltos_acima_de_1s=1 (o maior 2500 ms)"), "{}", r.resumo());
    }

    /// **O carimbo absurdo** (a revisão do código da fase 5, L7): nenhum pânico, nenhuma volta; o
    /// passo que não cabe conta como salto, e o primeiro quadro diz "estoura".
    #[test]
    fn fase5_os_relogios_com_carimbo_absurdo() {
        let base = Instant::now();
        let mut r = RelogiosDoFluxo::novo();
        assert_eq!(r.observar(&chegada(base, 0, i64::MAX, Some(i64::MIN), Some(i64::MIN))), None);
        assert!(r.primeiro_em_texto().unwrap().contains("estoura"));
        // De i64::MIN a i64::MAX: a subtração estoura, e conta como salto.
        assert_eq!(r.observar(&chegada(base, 33, i64::MAX, Some(i64::MAX), Some(i64::MAX))), None);
        assert_eq!(r.saltos_do_dispositivo, 1);
        // Um passo que cabe no i64 mas não em nanossegundos de u64: o maior possível, e salto.
        let mut r = RelogiosDoFluxo::novo();
        r.observar(&chegada(base, 0, 0, Some(0), None));
        assert_eq!(r.observar(&chegada(base, 33, 0, Some(i64::MAX), None)), Some(Duration::MAX));
        assert_eq!(r.saltos_do_dispositivo, 1);
        assert!(!r.resumo().is_empty());
        // A revisão curta do `08af2cd`, A9: de i64::MAX a i64::MIN a subtração também estoura, mas o
        // relógio andou para trás: é volta, e não salto.
        let mut r = RelogiosDoFluxo::novo();
        r.observar(&chegada(base, 0, 0, Some(i64::MAX), None));
        assert_eq!(r.observar(&chegada(base, 33, 0, Some(i64::MIN), None)), None);
        assert_eq!((r.voltas_do_dispositivo, r.saltos_do_dispositivo, r.maior_salto), (1, 0, None));
        // E a volta que cabe continua volta.
        r.observar(&chegada(base, 66, 0, Some(1_000), None));
        assert_eq!(r.observar(&chegada(base, 99, 0, Some(500), None)), None);
        assert_eq!(r.voltas_do_dispositivo, 2);
    }

    /// **A janela de 5 s** (a fase 5): a primeira chamada só começa; antes de 5 s não diz nada;
    /// passados 5 s, diz o que chegou e o que foi entregue naquela janela, e recomeça do zero.
    #[test]
    fn fase5_a_janela_de_5_s() {
        let t0 = Instant::now();
        let mut j = JanelaDaCamera::novo();
        assert_eq!(j.fechar_se_passou(t0, 10, 9), None, "a primeira só começa");
        for _ in 0..75 {
            j.quadro(Some(Duration::from_micros(66_667)), Some(Duration::from_millis(40)));
        }
        assert_eq!(j.fechar_se_passou(t0 + Duration::from_millis(4_900), 80, 79), None);
        let l = j.fechar_se_passou(t0 + Duration::from_secs(5), 85, 84).expect("a janela fecha em 5 s");
        assert!(l.starts_with("câmera: janela 1 (5.0 s): chegaram 75 (15.0 fps), entregues 75 (15.0 fps)"), "{l}");
        assert!(l.contains("passo do relógio do aparelho p50 66.7 máx 66.7 ms"), "{l}");
        assert!(l.contains("idade na chegada p50 40.0 máx 40.0 ms"), "{l}");
        // A seguinte recomeça do zero: em pouca luz, 7 quadros em 5 s, sem passo nem idade.
        let l2 = j.fechar_se_passou(t0 + Duration::from_secs(10), 92, 91).unwrap();
        assert!(l2.starts_with("câmera: janela 2 (5.0 s): chegaram 7 (1.4 fps), entregues 7 (1.4 fps)"), "{l2}");
        assert!(l2.contains("passo do relógio do aparelho — | idade na chegada —"), "{l2}");
    }

    /// **A Canon (I420) passa a ser escolhida** (a fase 5, passo 1: o EOS Webcam Utility só declara
    /// I420 1280×720 @30), e nos empates o I420 perde para os outros três: ele sobe pela memória.
    #[test]
    fn fase5_o_i420_da_canon_e_escolhido_e_perde_os_empates() {
        let canon = [t(Subtipo::I420, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&canon, TETO_1080P30), Some(0));
        let empate = [t(Subtipo::I420, 1280, 720, 30, 1), t(Subtipo::Mjpg, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&empate, TETO_1080P30), Some(1), "o MJPG ganha do I420 no empate");
        let empate = [t(Subtipo::I420, 1280, 720, 30, 1), t(Subtipo::Nv12, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&empate, TETO_1080P30), Some(1));
        // O fps ainda pesa antes do formato: I420 a 30 ganha de NV12 a 15.
        let fluido = [t(Subtipo::Nv12, 1280, 720, 15, 1), t(Subtipo::I420, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&fluido, TETO_1080P30), Some(1));
        // Um formato que a cadeia não sabe (`Outro`) continua fora; o `dvsd` entra como `Dv` (o teste
        // seguinte).
        assert_eq!(escolher_tipo_nativo(&[t(Subtipo::Outro, 720, 480, 30000, 1001)], TETO_1080P30), None);
        // Sem faixa declarada, limitada (o padrão do Media Foundation para YUV): o conversor não roda
        // só pela faixa, e o anel é NV12.
        assert!(!faixa_completa(Subtipo::I420, None));
        assert!(!precisa_do_processador(Subtipo::Nv12, faixa_completa(Subtipo::I420, None), false, false));
        assert!(faixa_completa(Subtipo::I420, Some(true)), "a declarada manda");
    }

    /// **Os planos do I420 e o entrelaçamento**: um quadro 4×4 com passo 6 (dois bytes de
    /// enchimento por linha), U e V conferidos byte a byte na linha UV do NV12.
    #[test]
    fn fase5_os_planos_do_i420_e_o_nv12_entrelacado() {
        // 1280×720 compacto (a Canon: DEFAULT_STRIDE 1280, SAMPLE_SIZE 1.382.400).
        assert_eq!(planos_i420(1280, 720), (921_600, 921_600 + 230_400, 640));
        assert_eq!(tamanho_i420(1280, 720), 1_382_400);
        assert_eq!(fim_i420(1280, 1280, 720), 1_382_400, "compacto: lê até o último byte");
        // Com passo de sobra (1536 para 1280), o fim do V passa da conta do NV12 (a M da revisão:
        // a conferência do buffer usa esta, e não a do NV12).
        let conta_do_nv12 = 1536 * (720 + 360 - 1) + 1280;
        assert!(fim_i420(1536, 1280, 720) > conta_do_nv12);
        assert_eq!(fim_i420(1536, 1280, 720), 1536 * 720 + 768 * 360 + 768 * 359 + 640);
        // 4×4 com passo 6: Y 24 bytes, U 3×2 = 6, V 6.
        let (passo, altura) = (6usize, 4usize);
        let (iu, iv, puv) = planos_i420(passo, altura);
        assert_eq!((iu, iv, puv, tamanho_i420(passo, altura)), (24, 30, 3, 36));
        let mut quadro = vec![0u8; tamanho_i420(passo, altura)];
        for linha in 0..altura / 2 {
            for x in 0..2 {
                quadro[iu + linha * puv + x] = 100 + (linha * 2 + x) as u8; // U
                quadro[iv + linha * puv + x] = 200 + (linha * 2 + x) as u8; // V
            }
        }
        let mut uv = vec![0xEEu8; 2 * 4];
        for linha in 0..altura / 2 {
            let u = &quadro[iu + linha * puv..iu + linha * puv + 2];
            let v = &quadro[iv + linha * puv..iv + linha * puv + 2];
            entrelacar_uv(u, v, &mut uv[linha * 4..linha * 4 + 4]);
        }
        assert_eq!(uv, vec![100, 200, 101, 201, 102, 202, 103, 203]);
        // O destino maior que o croma: o resto não é tocado.
        let mut largo = vec![0xEEu8; 6];
        entrelacar_uv(&[1, 2], &[3, 4], &mut largo);
        assert_eq!(largo, vec![1, 3, 2, 4, 0xEE, 0xEE]);
    }

    /// **O aspecto do DV** (a fase 5: a Panasonic em 16:9 saía comprimida no S24). O DV-SD nunca é de
    /// pixel quadrado, diga o tipo o que disser; o VAUX decide 16:9 ou 4:3; a PAR declarada manda; e
    /// **a fonte que não declara PAR fica quadrada, como hoje**.
    #[test]
    fn fase5_o_aspecto_do_dv_e_o_tamanho_exibido() {
        // A Panasonic declara 1:1 no `dvsd` 720x480: o DV-SD nunca é quadrado.
        let q = Some(Par::QUADRADA);
        let a = aspecto_da_camera(Subtipo::Dv, 720, 480, q, q, None);
        assert_eq!(a, Aspecto { par: Par { num: 8, den: 9 }, origem: OrigemDaPar::DvSemVaux }, "sem VAUX, 4:3");
        assert_eq!(tamanho_exibido(720, 480, a.par), (640, 480));
        // O VAUX de controle com DISP = 2 no PC2 (o byte 1 do UINT32): 16:9.
        let vaux_16_9 = 0x0000_0200u32 | 0x00FF_00F8;
        assert!(dv_em_16_9(vaux_16_9) && dv_em_16_9(0x0000_0700) && !dv_em_16_9(0x0000_0000) && !dv_em_16_9(0x0000_0100));
        let a = aspecto_da_camera(Subtipo::Dv, 720, 480, q, q, Some(vaux_16_9));
        assert_eq!(a, Aspecto { par: Par { num: 32, den: 27 }, origem: OrigemDaPar::DvPeloVaux });
        assert_eq!(tamanho_exibido(720, 480, a.par), (854, 480), "853,33 arredonda ao par 854");
        assert_eq!(aspecto_da_camera(Subtipo::Dv, 720, 480, q, q, Some(0)).par, Par { num: 8, den: 9 }, "VAUX 4:3");
        // O PAL: 16:15 e 64:45 -> 768 e 1024.
        assert_eq!(tamanho_exibido(720, 576, aspecto_da_camera(Subtipo::Dv, 720, 576, q, q, None).par), (768, 576));
        assert_eq!(tamanho_exibido(720, 576, aspecto_da_camera(Subtipo::Dv, 720, 576, q, q, Some(0x200)).par), (1024, 576));
        // O que a saída declara manda (o decodificador pode corrigir), depois o nativo.
        let p40_33 = Par { num: 40, den: 33 };
        assert_eq!(aspecto_da_camera(Subtipo::Dv, 720, 480, Some(p40_33), q, Some(0)).origem, OrigemDaPar::DeclaradaNaSaida);
        assert_eq!(
            aspecto_da_camera(Subtipo::Nv12, 1280, 720, q, Some(Par { num: 4, den: 3 }), None).origem,
            OrigemDaPar::DeclaradaNoNativo
        );
        // **A fonte que não declara PAR fica quadrada, como hoje**: a integrada, a Canon.
        let integrada = aspecto_da_camera(Subtipo::Nv12, 1280, 720, q, q, None);
        assert_eq!(integrada, Aspecto { par: Par::QUADRADA, origem: OrigemDaPar::Quadrada });
        assert_eq!(tamanho_exibido(1280, 720, integrada.par), (1280, 720));
        assert_eq!(aspecto_da_camera(Subtipo::I420, 1280, 720, None, None, None).origem, OrigemDaPar::Quadrada);
        // Um DV que não é SD não entra na regra; uma PAR com zero é quadrada.
        assert_eq!(aspecto_da_camera(Subtipo::Dv, 1440, 1080, q, q, None).origem, OrigemDaPar::Quadrada);
        assert!(Par { num: 0, den: 0 }.quadrada());
        assert_eq!(Par::do_mf((32u64 << 32) | 27), Par { num: 32, den: 27 });
    }

    /// **A PAR absurda** (a revisão curta do `08af2cd`, A5): uma câmera virtual ruim que declare
    /// 1000:1 criava uma imagem de 720 mil pixels de largura, e `u32::MAX`:1 estourava. Fora de 1:3 a
    /// 3:1, a declarada é ignorada: a regra segue para a próxima, e sem nenhuma fica quadrada, com a
    /// recusada na origem para o registro.
    #[test]
    fn revisao_curta_a5_a_par_fora_de_1_3_a_3_1_e_ignorada() {
        let q = Some(Par::QUADRADA);
        let mil = Par { num: 1000, den: 1 };
        let a = aspecto_da_camera(Subtipo::Nv12, 1280, 720, Some(mil), q, None);
        assert_eq!(a, Aspecto { par: Par::QUADRADA, origem: OrigemDaPar::Absurda(mil) });
        assert_eq!(tamanho_exibido(1280, 720, a.par), (1280, 720));
        let enorme = Par { num: u32::MAX, den: 1 };
        let a = aspecto_da_camera(Subtipo::I420, 1280, 720, None, Some(enorme), None);
        assert_eq!(a.origem, OrigemDaPar::Absurda(enorme));
        assert_eq!(tamanho_exibido(1280, 720, a.par), (1280, 720));
        let fina = Par { num: 1, den: 4 };
        assert_eq!(aspecto_da_camera(Subtipo::Yuy2, 640, 480, Some(fina), None, None).origem, OrigemDaPar::Absurda(fina));
        // A absurda na saída não esconde a boa do nativo, e o DV-SD cai na regra dele.
        let quatro_tercos = Par { num: 4, den: 3 };
        assert_eq!(
            aspecto_da_camera(Subtipo::Nv12, 1440, 1080, Some(mil), Some(quatro_tercos), None),
            Aspecto { par: quatro_tercos, origem: OrigemDaPar::DeclaradaNoNativo }
        );
        assert_eq!(
            aspecto_da_camera(Subtipo::Dv, 720, 480, Some(mil), q, None),
            Aspecto { par: Par { num: 8, den: 9 }, origem: OrigemDaPar::DvSemVaux }
        );
        // Os limites entram, e as PAR de verdade ficam longe deles.
        assert!(Par { num: 3, den: 1 }.plausivel() && Par { num: 1, den: 3 }.plausivel());
        assert!(!Par { num: 301, den: 100 }.plausivel() && !Par { num: 100, den: 301 }.plausivel());
        for p in [(8, 9), (32, 27), (16, 15), (64, 45), (4, 3), (10, 11), (40, 33)] {
            assert!(Par { num: p.0, den: p.1 }.plausivel(), "{p:?}");
        }
        assert!(!Par::QUADRADA.plausivel() && !Par { num: 0, den: 7 }.plausivel(), "a quadrada não é declaração");
        assert!(Par { num: u32::MAX, den: u32::MAX / 2 }.plausivel(), "a conta não estoura perto do limite");
        // O M91 e o M88 não mudam: 8:9 e 32:27 na saída do decodificador.
        assert_eq!(aspecto_da_camera(Subtipo::Dv, 720, 480, Some(Par { num: 8, den: 9 }), q, None).origem, OrigemDaPar::DeclaradaNaSaida);
        assert_eq!(tamanho_exibido(720, 480, Par { num: 8, den: 9 }), (640, 480));
        assert_eq!(tamanho_exibido(720, 480, Par { num: 32, den: 27 }), (854, 480));
    }

    /// **A troca que chega durante a abertura** (a revisão curta do `08af2cd`, A4). A abertura leu
    /// o leitor antes da troca; a thread do Media Foundation releu depois dela e guardou antes de a
    /// abertura gravar. Na primeira versão a gravação da abertura vinha por cima, e o aspecto novo
    /// sumia sem troca contada: a sessão ficava com o tamanho velho e sem encaixe.
    #[test]
    fn revisao_curta_a4_a_troca_durante_a_abertura_nao_se_perde() {
        let quatro_tercos = Aspecto { par: Par { num: 8, den: 9 }, origem: OrigemDaPar::DeclaradaNaSaida };
        let dezesseis_nonos = Aspecto { par: Par { num: 32, den: 27 }, origem: OrigemDaPar::DeclaradaNaSaida };
        // A troca entre o cálculo e a gravação da abertura: fica a da releitura, sem troca contada.
        let mut s = AspectoDaSessao::default();
        let calculado_na_abertura = quatro_tercos;
        assert_eq!(s.reler(dezesseis_nonos), Releitura::AntesDaAbertura);
        assert_eq!(s.fixar_na_abertura(calculado_na_abertura), dezesseis_nonos, "a releitura é a mais nova");
        assert_eq!((s.atual(), s.trocas()), (dezesseis_nonos, 0));
        // Depois de fixado, a troca conta, e a mesma releitura repetida não.
        assert_eq!(s.reler(quatro_tercos), Releitura::Mudou { n: 1, antes: dezesseis_nonos });
        assert_eq!(s.reler(quatro_tercos), Releitura::Igual);
        assert_eq!(s.reler(dezesseis_nonos), Releitura::Mudou { n: 2, antes: quatro_tercos });
        assert_eq!(s.trocas(), 2);
        // Sem troca nenhuma: o da abertura; e o M91 (4:3 desde antes de começar, 8:9 na saída do
        // decodificador): a releitura do mesmo aspecto não conta.
        let mut s = AspectoDaSessao::default();
        assert_eq!(s.atual(), Aspecto { par: Par::QUADRADA, origem: OrigemDaPar::Quadrada });
        assert_eq!(s.fixar_na_abertura(quatro_tercos), quatro_tercos);
        assert_eq!(s.reler(quatro_tercos), Releitura::Igual);
        assert_eq!((s.atual(), s.trocas()), (quatro_tercos, 0));
        assert_eq!(tamanho_exibido(720, 480, s.atual().par), (640, 480));
    }

    /// **A troca de aspecto no meio da sessão**: o encoder tem o tamanho da abertura, e o quadro
    /// novo entra encaixado, com faixas, sem deformar.
    #[test]
    fn fase5_o_encaixe_da_troca_de_aspecto_no_meio() {
        // Começou 4:3 (640x480) e virou 16:9 (854x480): faixas em cima e embaixo.
        assert_eq!(encaixe((640, 480), (854, 480)), (0, 60, 640, 360));
        // Começou 16:9 (854x480) e virou 4:3 (640x480): faixas dos lados.
        assert_eq!(encaixe((854, 480), (640, 480)), (106, 0, 640, 480));
        // O mesmo aspecto: a saída inteira.
        assert_eq!(encaixe((854, 480), (854, 480)), (0, 0, 854, 480));
        assert_eq!(encaixe((640, 480), (720, 540)), (0, 0, 640, 480));
        // Tudo par, e nada passa da saída.
        for (s, e) in [((854, 480), (720, 480)), ((640, 480), (1024, 576)), ((1280, 720), (768, 576))] {
            let (x, y, l, a) = encaixe(s, e);
            assert!(x % 2 == 0 && y % 2 == 0 && l % 2 == 0 && a % 2 == 0, "{s:?} {e:?} -> {:?}", (x, y, l, a));
            assert!(x + l <= s.0 && y + a <= s.1);
        }
    }

    /// **O leitor sem o gerenciador D3D para o I420** (a fase 5, p9: com ele, a Canon devolveu
    /// `0xC00D36B4` no primeiro quadro; sem ele, o quadro veio em memória). Os outros tipos mantêm o
    /// caminho da GPU.
    #[test]
    fn fase5_o_tipo_decide_se_o_leitor_leva_d3d() {
        for dv_pela_memoria in [false, true] {
            assert!(!leitor_com_d3d(Subtipo::I420, dv_pela_memoria), "a Canon: sem o gerenciador");
            for s in [Subtipo::Nv12, Subtipo::Yuy2, Subtipo::Mjpg] {
                assert!(leitor_com_d3d(s, dv_pela_memoria), "{s:?} continua com o gerenciador");
            }
        }
        // O DV: com o adapt2, pela memória; com o bob (ou `--sem-desentrelacar`), como antes.
        assert!(!leitor_com_d3d(Subtipo::Dv, true));
        assert!(leitor_com_d3d(Subtipo::Dv, false));
    }

    /// **Quem desentrelaça** (22/09): o adapt2 só com o leitor sem D3D, YUY2, e a imagem com os
    /// campos no lugar; senão o bob do processador, e nunca a câmera sem abrir. O conversor recebe
    /// progressivo quando a CPU desentrelaçou (senão o bob correria de novo sobre o adapt2).
    #[test]
    fn desentrelacar_o_recuo_do_dv_com_d3d() {
        assert!(refazer_o_dv_com_d3d(CausaDaFalha::Outra));
        for c in [CausaDaFalha::Demorou, CausaDaFalha::Ocupada, CausaDaFalha::Removida, CausaDaFalha::Negada, CausaDaFalha::Cancelada] {
            assert!(!refazer_o_dv_com_d3d(c), "{c:?}");
        }
        // A fita parada (nenhum quadro em 5 s) não condena o leitor sem D3D pelo resto do processo.
        assert!(lembrar_que_o_dv_sem_d3d_falhou(CausaDaFalha::Outra));
        for c in [CausaDaFalha::Demorou, CausaDaFalha::Ocupada, CausaDaFalha::Removida, CausaDaFalha::Negada, CausaDaFalha::Cancelada] {
            assert!(!lembrar_que_o_dv_sem_d3d_falhou(c), "{c:?}");
        }
    }

    #[test]
    fn desentrelacar_quem_faz() {
        use Entrelacamento::*;
        let b = CampoDeBaixoPrimeiro;
        assert_eq!(quem_desentrelaca(Progressivo, true, true, true, 0, 720, 480), QuemDesentrelaca::Ninguem);
        assert_eq!(quem_desentrelaca(b, true, true, true, 0, 720, 480), QuemDesentrelaca::Cpu(b));
        assert_eq!(quem_desentrelaca(CampoDeCimaPrimeiro, true, true, true, 0, 720, 576), QuemDesentrelaca::Cpu(CampoDeCimaPrimeiro));
        assert_eq!(quem_desentrelaca(b, false, true, true, 0, 720, 480), QuemDesentrelaca::Processador(b), "o --desentrelacador bob");
        assert_eq!(quem_desentrelaca(b, true, false, true, 0, 720, 480), QuemDesentrelaca::Processador(b), "o leitor com D3D: a superfície vai pela GPU");
        assert_eq!(quem_desentrelaca(b, true, true, false, 0, 720, 480), QuemDesentrelaca::Processador(b), "NV12 entrelaçado: o bob");
        assert_eq!(quem_desentrelaca(b, true, true, true, 1, 720, 478), QuemDesentrelaca::Processador(b), "a abertura em y ímpar trocaria os campos");
        assert_eq!(quem_desentrelaca(b, true, true, true, 0, 720, 479), QuemDesentrelaca::Processador(b));
        assert_eq!(quem_desentrelaca(b, true, true, true, 0, 719, 480), QuemDesentrelaca::Processador(b), "largura ímpar: o par YUY2");
        assert_eq!(QuemDesentrelaca::Cpu(b).no_anel(), Progressivo);
        assert_eq!(QuemDesentrelaca::Processador(b).no_anel(), b);
        assert_eq!(QuemDesentrelaca::Ninguem.no_anel(), Progressivo);
    }

    /// **O leitor serve para o tipo em uso?** (a revisão curta do `08af2cd`, A7). O descritor
    /// escolheu I420, o leitor nasceu sem D3D, e o `SetNativeMediaType` foi recusado: o tipo em uso
    /// é outro, e o quadro sobe pela memória, como antes da checagem. Falha só o gerenciador D3D com
    /// o I420 (o `0xC00D36B4` da Canon, p9).
    #[test]
    fn revisao_curta_a7_so_o_d3d_com_o_i420_falha() {
        assert_eq!(leitor_serve(true, Subtipo::I420, false), LeitorServe::Nao);
        for s in [Subtipo::Nv12, Subtipo::Yuy2, Subtipo::Mjpg, Subtipo::Dv] {
            assert_eq!(leitor_serve(false, s, false), LeitorServe::PelaMemoria, "{s:?}: sem D3D, pela memória");
            assert_eq!(leitor_serve(true, s, false), LeitorServe::Sim, "{s:?}");
        }
        assert_eq!(leitor_serve(false, Subtipo::I420, false), LeitorServe::Sim, "a Canon, como no p7");
        // O DV com o adapt2 (22/09): o leitor sem D3D é o esperado, e não um recuo.
        assert_eq!(leitor_serve(false, Subtipo::Dv, true), LeitorServe::Sim);
        assert_eq!(leitor_serve(true, Subtipo::Dv, true), LeitorServe::Sim, "o descritor não disse o tipo: com D3D, e o DV serve (o bob)");
    }

    /// **O DV da Panasonic em controladora** (a fase 5, o pedido do Bruno): o `dvsd` passa a ser
    /// escolhido (antes caía em `Outro`, a controladora recusava, e só o recuo compartilhado
    /// transmitia), perde todos os empates, e o fps e a área continuam pesando antes do formato.
    #[test]
    fn fase5_o_dv_e_escolhido_e_perde_os_empates() {
        let dv = [t(Subtipo::Dv, 720, 480, 30000, 1001), t(Subtipo::Outro, 0, 0, 0, 0)];
        assert_eq!(escolher_tipo_nativo(&dv, TETO_1080P30), Some(0), "o dvsd 720x480 da Panasonic em DV");
        let empate = [t(Subtipo::Dv, 720, 480, 30, 1), t(Subtipo::I420, 720, 480, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&empate, TETO_1080P30), Some(1), "o I420 ganha do DV no empate");
        // Uma câmera com DV e NV12 720p: a área decide antes do formato.
        let com_nv12 = [t(Subtipo::Dv, 720, 480, 30000, 1001), t(Subtipo::Nv12, 1280, 720, 30, 1)];
        assert_eq!(escolher_tipo_nativo(&com_nv12, TETO_1080P30), Some(1));
    }

    /// **O entrelaçamento**: o DV-SD é campo de baixo primeiro mesmo quando o tipo diz progressivo
    /// (a Panasonic declara `MF_MT_INTERLACE_MODE = 2` no `dvsd`, a fase 5, passo 1); fora do DV,
    /// manda o que o tipo declara, e o que não diz é progressivo.
    #[test]
    fn fase5_o_entrelacamento_do_dv_e_das_outras() {
        use Entrelacamento::*;
        let nada = AmostraDiz::default();
        let diz = |entrelacada: bool, baixo: Option<bool>| AmostraDiz { entrelacada: Some(entrelacada), campo_de_baixo_primeiro: baixo };
        assert_eq!(entrelacamento(Subtipo::Dv, 480, Some(INTERLACE_PROGRESSIVO), nada), CampoDeBaixoPrimeiro, "a Panasonic mente");
        assert_eq!(entrelacamento(Subtipo::Dv, 576, None, diz(false, None)), CampoDeBaixoPrimeiro, "o PAL também, diga a amostra o que disser");
        assert_eq!(
            entrelacamento(Subtipo::Dv, 1080, Some(INTERLACE_CIMA_PRIMEIRO), diz(true, None)),
            CampoDeCimaPrimeiro,
            "DV em HD: o tipo e a amostra mandam"
        );
        assert_eq!(entrelacamento(Subtipo::Nv12, 720, Some(INTERLACE_PROGRESSIVO), diz(true, None)), Progressivo);
        assert_eq!(entrelacamento(Subtipo::Nv12, 720, None, nada), Progressivo, "a webcam que não diz é progressiva");
        assert_eq!(entrelacamento(Subtipo::Yuy2, 480, Some(INTERLACE_BAIXO_PRIMEIRO), diz(true, None)), CampoDeBaixoPrimeiro);
        assert_eq!(entrelacamento(Subtipo::Mjpg, 1080, Some(INTERLACE_CIMA_PRIMEIRO), diz(true, None)), CampoDeCimaPrimeiro);
        // A revisão curta do `08af2cd`, A8: nos modos fixos (3 e 4), o tipo manda, e a amostra só
        // veta com a contradição explícita. A placa que declara só no tipo é desentrelaçada.
        assert_eq!(entrelacamento(Subtipo::Yuy2, 480, Some(INTERLACE_BAIXO_PRIMEIRO), nada), CampoDeBaixoPrimeiro);
        assert_eq!(entrelacamento(Subtipo::Yuy2, 576, Some(INTERLACE_CIMA_PRIMEIRO), nada), CampoDeCimaPrimeiro);
        assert_eq!(entrelacamento(Subtipo::Yuy2, 480, Some(INTERLACE_BAIXO_PRIMEIRO), diz(false, None)), Progressivo, "a amostra contradiz");
        // No misto (7), a amostra decide, e a ordem vem do `BottomFieldFirst`.
        assert_eq!(entrelacamento(Subtipo::Nv12, 1080, Some(INTERLACE_MISTO), nada), Progressivo, "o misto sem a amostra dizer");
        assert_eq!(entrelacamento(Subtipo::Nv12, 1080, Some(INTERLACE_MISTO), diz(false, Some(true))), Progressivo);
        assert_eq!(entrelacamento(Subtipo::Nv12, 1080, Some(INTERLACE_MISTO), diz(true, None)), CampoDeCimaPrimeiro);
        assert_eq!(entrelacamento(Subtipo::Nv12, 1080, Some(INTERLACE_MISTO), diz(true, Some(false))), CampoDeCimaPrimeiro);
        assert_eq!(entrelacamento(Subtipo::Nv12, 480, Some(INTERLACE_MISTO), diz(true, Some(true))), CampoDeBaixoPrimeiro);
        // Os de campo único (5, 6) e o que não se conhece seguem progressivos.
        assert_eq!(entrelacamento(Subtipo::Nv12, 480, Some(5), diz(true, Some(true))), Progressivo);
        assert_eq!(entrelacamento(Subtipo::Nv12, 480, Some(99), diz(true, None)), Progressivo);
        // A revisão, L2: a regra do DV olha a altura do quadro (a captura passa `quadro_altura`).
        assert_eq!(entrelacamento(Subtipo::Dv, 476, Some(INTERLACE_PROGRESSIVO), nada), Progressivo, "476 é abertura, e não quadro");
        // O entrelaçado passa pelo processador mesmo em NV12 limitado no tamanho do teto.
        assert!(precisa_do_processador(Subtipo::Nv12, false, false, true));
        assert!(!precisa_do_processador(Subtipo::Nv12, false, false, false));
    }

    /// **A privacidade desligada com a sessão no ar** (a fase 5, p6d): o leitor devolveu
    /// `0x80070005`, a sessão acabou em 112 ms, e a tela dizia "parou". A frase é a da abertura
    /// negada, com o nome; o código fica no detalhe. Os outros fins não mudam.
    #[test]
    fn fase5_a_privacidade_desligada_no_meio_da_sessao() {
        let nome = "Integrated Webcam";
        let motivo = "o leitor devolveu HRESULT(0x80070005) (Acesso negado.)".to_string();
        let negada = FimDaCamera::pelo_codigo(motivo.clone(), false, Some(HR_NEGADA));
        assert!(negada.negada && !negada.tomada && !negada.desconectada);
        let t = texto_do_fim_da_camera(nome, Some(&negada), false);
        assert_eq!(
            so_a_frase(&t),
            "O Windows negou o acesso à câmera \"Integrated Webcam\": veja Privacidade e segurança > Câmera."
        );
        assert!(t.ends_with(&motivo), "o código fica no registro: {t}");
        // A interface que some continua ganhando: desconexão é desconexão.
        assert_eq!(so_a_frase(&texto_do_fim_da_camera(nome, Some(&negada), true)), "A câmera \"Integrated Webcam\" foi desconectada.");
        // Os outros códigos: tomada, removida e o resto como antes.
        assert!(FimDaCamera::pelo_codigo(String::new(), false, Some(HR_TOMADA)).tomada);
        assert!(FimDaCamera::pelo_codigo(String::new(), true, Some(HR_REMOVIDA)).desconectada);
        let outro = FimDaCamera::pelo_codigo("o leitor devolveu HRESULT(0x887A0005)".into(), false, Some(0x887A_0005));
        assert!(!outro.negada && !outro.tomada && !outro.desconectada);
        assert_eq!(so_a_frase(&texto_do_fim_da_camera(nome, Some(&outro), false)), "A câmera \"Integrated Webcam\" parou.");
    }

    #[test]
    fn fase5_o_resumo_em_ms_e_o_mesmo_das_idades() {
        assert_eq!(resumo_em_ms(&[]), "sem quadros");
        assert_eq!(resumo_em_ms(&[34_000, 33_000, 35_000]), "n=3 min=33.00 p50=34.00 p95=35.00 max=35.00 ms");
    }
}
