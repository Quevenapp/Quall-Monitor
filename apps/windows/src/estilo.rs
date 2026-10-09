//! **As peças da tela** do app Windows: as cores, os tipos e o desenho de cada peça do "Estúdio de
//! bolso" (`docs/telas-estudio.md` §2–§4). A janela principal (`janela.rs`) só desenha com daqui.
//!
//! # Duas metades, e só a segunda é Windows
//!
//! - **As peças são valores**: cada função (`botao`, `ladrilho`, `pilula`, `letreiro`…) devolve uma
//!   lista de [`Item`] — caixas de cantos redondos, círculos, textos com trechos, ícones — em
//!   unidades de 96 dpi (DIP). Não há Win32 nessa metade, e é de propósito: a lista é o que os testes
//!   conferem em qualquer máquina, e é o que o retrato de bancada e a janela desenham igual.
//! - **O desenho** ([`d2d::Pintor`], só no Windows) põe a lista num DC pelo Direct2D e pelo
//!   DirectWrite, com antisserrilhado — a mesma técnica do texto do teleprompter
//!   (`teleprompter/texto.rs`: um `ID2D1DCRenderTarget` ligado ao DC que o Windows deu). O alvo é o
//!   **de software**: uma janela de controles redesenhada poucas vezes por segundo não ganha nada com a
//!   GPU, e o de software funciona igual na Sessão 0 do SSH, onde o retrato de bancada roda (o padrão
//!   falhou lá com `E_HANDLE`, medido pelo teleprompter em 14/09).
//!
//! # Os números, e a tabela de lugares
//!
//! As cores são os tokens do §2, com os nomes de lá. Os tamanhos do Windows são os do protótipo do
//! Windows onde o §4 dá um valor por plataforma (botão de 40 com cantos de 6, campo de 40): o §4 dá
//! um canto só para o ladrilho e o cartão (18/22, os do celular), e aqui eles ficam com os 8 do
//! Windows 11, como no protótipo — a nota está no relatório da rodada.
//!
//! **A janela é fixa, 880 × 580 DIP** (§11.5), e por isso cada lugar dela é uma constante do módulo
//! [`lugar`]: a **única** tabela de posições, que serve tanto a quem põe os controles nativos no
//! lugar (`janela.rs::posicionar`) quanto a quem pinta em volta deles. Antes desta rodada os y de
//! `posicionar` casavam à mão com os de `pintar_*`.

// =============================================================================================
// A cor
// =============================================================================================

/// Uma cor com alfa, em 8 bits por canal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Cor {
    /// `0xRRGGBB`, opaca.
    pub const fn rgb(hex: u32) -> Cor {
        Cor { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 255 }
    }

    /// `0xRRGGBB` com o alfa em 0–255.
    pub const fn rgba(hex: u32, a: u8) -> Cor {
        Cor { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a }
    }

    /// A mesma cor com a opacidade multiplicada (o botão apertado a 90 %, o desligado a 40 %).
    pub fn vezes(self, opacidade: f32) -> Cor {
        Cor { a: ((self.a as f32) * opacidade.clamp(0.0, 1.0)).round() as u8, ..self }
    }

    /// A cor composta sobre um fundo opaco. É o que o GDI precisa (ele não tem alfa): o fundo do
    /// campo de texto é pintado pelo controle nativo com uma cor opaca.
    pub fn sobre(self, fundo: Cor) -> Cor {
        let a = self.a as f32 / 255.0;
        let mistura = |c: u8, f: u8| ((c as f32) * a + (f as f32) * (1.0 - a)).round() as u8;
        Cor { r: mistura(self.r, fundo.r), g: mistura(self.g, fundo.g), b: mistura(self.b, fundo.b), a: 255 }
    }

    /// O `COLORREF` do GDI (`0x00BBGGRR`), sem o alfa.
    pub fn colorref(self) -> u32 {
        (self.b as u32) << 16 | (self.g as u32) << 8 | self.r as u32
    }
}

// =============================================================================================
// Os tokens (§2), com os nomes da especificação
// =============================================================================================

/// Fundo de toda tela.
pub const FUNDO: Cor = Cor::rgb(0x0B0B0F);
/// Cartões, campos, ladrilhos.
pub const SUPERFICIE: Cor = Cor::rgb(0x16161D);
/// Botão secundário, casa do PIN (a casa usa a `SUPERFICIE` com borda, como o §4 pede).
pub const SUPERFICIE_ALTA: Cor = Cor::rgb(0x20202A);
/// A borda de 1 DIP dos cartões: branco a 8 %.
pub const CONTORNO: Cor = Cor::rgba(0xFFFFFF, 20);
pub const TEXTO: Cor = Cor::rgb(0xF5F5F7);
/// Texto secundário (7,2:1 sobre a superfície).
pub const TEXTO2: Cor = Cor::rgb(0xA1A1AE);
/// Rótulo de seção e legenda (5,2:1) — nunca mais claro que isto em letra pequena.
pub const TEXTO3: Cor = Cor::rgb(0x8B8B99);
/// O violeta Quall: fundo do botão principal (texto branco: 4,7:1).
pub const ACENTO: Cor = Cor::rgb(0x6A5AF9);
/// Ícone e texto violeta sobre o escuro (7,9:1).
pub const ACENTO_CLARO: Cor = Cor::rgb(0xA99FFF);
/// O acento a 16 %: fundo do ícone dos ladrilhos e cartões.
pub const ACENTO_FUNDO: Cor = Cor::rgba(0x6A5AF9, 41);
/// O acento a 14 %: o ladrilho escolhido.
pub const ACENTO_ESCOLHIDO: Cor = Cor::rgba(0x6A5AF9, 36);
/// A luz vermelha: bolinha "no ar".
pub const NO_AR: Cor = Cor::rgb(0xFF453A);
/// Fundo da pílula NO AR (texto branco: 4,9:1).
pub const NO_AR_CHEIO: Cor = Cor::rgb(0xD93025);
/// A luz âmbar: aguardando, aviso.
pub const AGUARDANDO: Cor = Cor::rgb(0xFFB340);
pub const AGUARDANDO_TEXTO: Cor = Cor::rgb(0xFFC870);
/// A luz verde: conectado, rede presente.
pub const CONECTADO: Cor = Cor::rgb(0x32D74B);
pub const CONECTADO_TEXTO: Cor = Cor::rgb(0x6BE07F);
/// Parar, Esquecer: o texto…
pub const PERIGO_TEXTO: Cor = Cor::rgb(0xFF8A80);
/// …sobre o vermelho a 18 %.
pub const PERIGO_FUNDO: Cor = Cor::rgba(0xFF453A, 46);

// --- os do Windows (§7) ---

/// A barra lateral e a barra de título (a cor da legenda do DWM, no Windows 11).
pub const BARRA: Cor = SUPERFICIE;
/// O item escolhido da barra lateral: branco a 6 %…
pub const ITEM_ESCOLHIDO: Cor = Cor::rgba(0xFFFFFF, 15);
/// …com o traço vertical de 3 DIP à esquerda (o jeito do Windows 11).
pub const TRACO_ESCOLHIDO: Cor = Cor::rgb(0x8B7DFF);
/// O ícone do item escolhido.
pub const ICONE_ESCOLHIDO: Cor = Cor::rgb(0xC9C2FF);
/// O texto dos itens não escolhidos da barra.
pub const TEXTO_DA_BARRA: Cor = Cor::rgb(0xD6D6DE);
/// O "Parar" cheio, o vermelho do próprio Windows (§7.4).
pub const PARAR_WINDOWS: Cor = Cor::rgb(0xC42B1C);
/// A borda dos campos e das casas do PIN: branco a 10 %.
pub const BORDA_DE_CAMPO: Cor = Cor::rgba(0xFFFFFF, 26);
/// A borda de baixo do campo com o foco (o jeito do Windows 11), e a casa da vez.
pub const FOCO_DE_CAMPO: Cor = Cor::rgb(0x8B7DFF);
pub const BRANCO: Cor = Cor::rgb(0xFFFFFF);

// =============================================================================================
// Os tamanhos (DIP)
// =============================================================================================

/// A janela: 880 × 580 de área de cliente, no mínimo (§7). A mesma do Mac.
pub const LARGURA_MINIMA: f32 = 880.0;
pub const ALTURA_MINIMA: f32 = 580.0;
/// A barra lateral.
pub const LARGURA_DA_BARRA: f32 = 230.0;
/// As margens do painel: dos lados, em cima e embaixo (as do protótipo).
pub const MARGEM: f32 = 34.0;
pub const TOPO: f32 = 30.0;
pub const PE: f32 = 26.0;
/// O botão do Windows: 40 de altura, cantos de 6 (§4).
pub const ALTURA_DO_BOTAO: f32 = 40.0;
pub const RAIO_DO_BOTAO: f32 = 6.0;
/// Cartão, ladrilho, linha: os 8 do Windows 11.
pub const RAIO_DO_CARTAO: f32 = 8.0;
/// Campo de texto: 40 de altura, cantos de 6 (§4).
pub const ALTURA_DO_CAMPO: f32 = 40.0;
pub const RAIO_DO_CAMPO: f32 = 6.0;
/// O letreiro do PIN no computador: casas de 54 × 70 (§4).
pub const CASA_L: f32 = 54.0;
pub const CASA_A: f32 = 70.0;
pub const CASA_ESPACO: f32 = 7.0;
pub const CASA_ENTRE_GRUPOS: f32 = 10.0;
/// A pílula de estado: 28 de altura.
pub const ALTURA_DA_PILULA: f32 = 28.0;
/// O chip do endereço: a cápsula de 40 (§4).
pub const ALTURA_DO_CHIP: f32 = 40.0;
/// O interruptor desenhado (o nativo do Windows 11 tem 40 × 20).
pub const INTERRUPTOR_L: f32 = 40.0;
pub const INTERRUPTOR_A: f32 = 20.0;

// =============================================================================================
// Os tipos (§3)
// =============================================================================================

/// A família, pelo papel. O nome de verdade sai de [`nomes_da_familia`], na ordem de preferência.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Familia {
    Titulo,
    Corpo,
    Mono,
    Icones,
}

/// As famílias, da preferida para a de reserva. A primeira que o sistema tiver é a que vale (o
/// `Pintor` pergunta à coleção de fontes do DirectWrite).
pub fn nomes_da_familia(f: Familia) -> &'static [&'static str] {
    match f {
        Familia::Titulo => &["Segoe UI Variable Display", "Segoe UI"],
        Familia::Corpo => &["Segoe UI Variable Text", "Segoe UI"],
        Familia::Mono => &["Cascadia Mono", "Consolas"],
        // Windows 11 / Windows 10: os mesmos pontos de código nas duas.
        Familia::Icones => &["Segoe Fluent Icons", "Segoe MDL2 Assets"],
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fonte {
    pub familia: Familia,
    pub tamanho: f32,
    pub peso: u16,
    /// Espaço entre letras, em fração do tamanho (o rótulo de seção: +0,08 em).
    pub espaco: f32,
}

pub const fn fonte(familia: Familia, tamanho: f32, peso: u16) -> Fonte {
    Fonte { familia, tamanho, peso, espaco: 0.0 }
}

pub const NORMAL: u16 = 400;
pub const SEMINEGRITO: u16 = 600;
pub const NEGRITO: u16 = 700;

/// "Quall Monitor" na barra lateral.
pub const F_MARCA: Fonte = fonte(Familia::Titulo, 22.0, NEGRITO);
/// Título de tela (26–28).
pub const F_TITULO: Fonte = fonte(Familia::Titulo, 28.0, SEMINEGRITO);
/// "Pronto para espelhar", "Conectando": o título das telas de sessão.
pub const F_TITULO_GRANDE: Fonte = fonte(Familia::Titulo, 30.0, SEMINEGRITO);
/// O nome do par no ar.
pub const F_PAR: Fonte = fonte(Familia::Titulo, 34.0, SEMINEGRITO);
pub const F_TITULO_DE_CARTAO: Fonte = fonte(Familia::Corpo, 16.0, SEMINEGRITO);
pub const F_CORPO: Fonte = fonte(Familia::Corpo, 14.0, NORMAL);
pub const F_CORPO_FORTE: Fonte = fonte(Familia::Corpo, 14.0, SEMINEGRITO);
pub const F_CORPO_15: Fonte = fonte(Familia::Corpo, 15.0, NORMAL);
pub const F_LEGENDA: Fonte = fonte(Familia::Corpo, 12.0, NORMAL);
pub const F_LEGENDA_13: Fonte = fonte(Familia::Corpo, 13.0, NORMAL);
pub const F_LEGENDA_11: Fonte = fonte(Familia::Corpo, 11.0, NORMAL);
pub const F_BOTAO: Fonte = fonte(Familia::Corpo, 14.0, SEMINEGRITO);
pub const F_BOTAO_PEQUENO: Fonte = fonte(Familia::Corpo, 13.0, NORMAL);
pub const F_ROTULO: Fonte = Fonte { familia: Familia::Corpo, tamanho: 12.0, peso: SEMINEGRITO, espaco: 0.08 };
pub const F_ROTULO_11: Fonte = Fonte { familia: Familia::Corpo, tamanho: 11.0, peso: SEMINEGRITO, espaco: 0.06 };
pub const F_PILULA: Fonte = Fonte { familia: Familia::Corpo, tamanho: 12.0, peso: NEGRITO, espaco: 0.08 };
pub const F_PIN: Fonte = fonte(Familia::Mono, 34.0, NEGRITO);
pub const F_ENDERECO: Fonte = fonte(Familia::Mono, 15.0, NORMAL);
pub const F_MONO_13: Fonte = fonte(Familia::Mono, 13.0, NORMAL);
pub const F_MONO_12: Fonte = fonte(Familia::Mono, 12.0, NORMAL);
pub const F_MONO_11: Fonte = fonte(Familia::Mono, 11.0, NORMAL);
pub const F_VALOR: Fonte = fonte(Familia::Corpo, 16.0, SEMINEGRITO);
/// O valor mono dos cartões de número: 13, para "1920×1080 · 30" caber nos 115 de um cartão.
pub const F_VALOR_MONO: Fonte = fonte(Familia::Mono, 13.0, SEMINEGRITO);

/// A largura estimada de um texto, para as contas que não podem esperar o DirectWrite (o tamanho
/// do chip do endereço, que é uma janela filha posta antes do desenho). Mono é exato a menos do
/// arredondamento (Cascadia Mono e Consolas avançam 0,6 e 0,55 em); o resto é uma média folgada.
pub fn largura_estimada(texto: &str, f: Fonte) -> f32 {
    let n = texto.chars().count() as f32;
    let por_letra = match f.familia {
        Familia::Mono => 0.6,
        Familia::Icones => 1.0,
        _ if f.peso >= SEMINEGRITO => 0.56,
        _ => 0.52,
    };
    n * f.tamanho * (por_letra + f.espaco)
}

// =============================================================================================
// Os ícones
// =============================================================================================

/// Os ícones, pelo papel. No Windows são glifos da Segoe Fluent Icons (Windows 11) ou da Segoe
/// MDL2 Assets (Windows 10), que dividem os pontos de código — nunca emoji (§4). O quadradinho do
/// Parar e o miolo do Gravar não são glifo: são desenhados ([`Item::Caixa`], [`Item::Circulo`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icone {
    Espelhar,
    Exibir,
    Teleprompter,
    Ajustes,
    Monitor,
    TelaEstendida,
    Camera,
    Rosto,
    Microfone,
    Copiar,
    Marcado,
    Info,
    Aviso,
    Aparelho,
    Pasta,
    Controlar,
    Abrir,
    Fechar,
    Volume,
    Mudo,
    /// O botão da tela cheia do teleprompter (as duas telas): entrar e sair.
    TelaCheia,
    SairDaTelaCheia,
}

impl Icone {
    /// O glifo (ponto de código da área de uso privado).
    pub fn glifo(self) -> char {
        match self {
            Icone::Espelhar => '\u{E7F4}',      // TVMonitor
            Icone::Exibir => '\u{E714}',        // Video
            Icone::Teleprompter => '\u{E8E4}',  // AlignLeft
            Icone::Ajustes => '\u{E713}',       // Setting
            Icone::Monitor => '\u{E7F4}',       // TVMonitor
            Icone::TelaEstendida => '\u{E7C4}', // TaskView
            Icone::Camera => '\u{E722}',        // Camera
            Icone::Rosto => '\u{E77B}',         // Contact
            Icone::Microfone => '\u{E720}',     // Microphone
            Icone::Copiar => '\u{E8C8}',        // Copy
            Icone::Marcado => '\u{E73E}',       // CheckMark
            Icone::Info => '\u{E946}',          // Info
            Icone::Aviso => '\u{E7BA}',         // Warning
            Icone::Aparelho => '\u{E8EA}',      // CellPhone
            Icone::Pasta => '\u{E838}',         // FolderOpen
            Icone::Controlar => '\u{E9E9}',     // Equalizer
            Icone::Abrir => '\u{E70D}',         // ChevronDown
            Icone::Fechar => '\u{E70E}',        // ChevronUp
            Icone::Volume => '\u{E767}',        // Volume
            Icone::Mudo => '\u{E74F}',          // Mute
            Icone::TelaCheia => '\u{E740}',     // FullScreen
            Icone::SairDaTelaCheia => '\u{E73F}', // BackToWindow
        }
    }
}

// =============================================================================================
// A lista de desenho
// =============================================================================================

/// Um retângulo em DIP.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ret {
    pub x: f32,
    pub y: f32,
    pub l: f32,
    pub a: f32,
}

impl Ret {
    pub const fn new(x: f32, y: f32, l: f32, a: f32) -> Ret {
        Ret { x, y, l, a }
    }
    pub fn direita(&self) -> f32 {
        self.x + self.l
    }
    pub fn baixo(&self) -> f32 {
        self.y + self.a
    }
    /// Encolhido `dx` de cada lado e `dy` em cima e embaixo.
    pub fn dentro(&self, dx: f32, dy: f32) -> Ret {
        Ret { x: self.x + dx, y: self.y + dy, l: (self.l - 2.0 * dx).max(0.0), a: (self.a - 2.0 * dy).max(0.0) }
    }
    /// Os dois se cruzam (com área; encostar não conta).
    pub fn cruza(&self, o: &Ret) -> bool {
        self.x < o.direita() - 0.01 && o.x < self.direita() - 0.01 && self.y < o.baixo() - 0.01 && o.y < self.baixo() - 0.01
    }
    /// `o` cabe inteiro neste.
    pub fn contem(&self, o: &Ret) -> bool {
        o.x >= self.x - 0.01 && o.y >= self.y - 0.01 && o.direita() <= self.direita() + 0.01 && o.baixo() <= self.baixo() + 0.01
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alinha {
    Esquerda,
    Centro,
    Direita,
}

/// Um pedaço de texto com cor, peso ou família próprios (o **nome** em negrito na instrução da
/// espera, o endereço em mono no meio de uma frase, o glifo do ícone antes do rótulo de um botão).
#[derive(Clone, Debug, PartialEq)]
pub struct Trecho {
    pub texto: String,
    pub cor: Option<Cor>,
    pub peso: Option<u16>,
    pub familia: Option<Familia>,
    pub tamanho: Option<f32>,
}

impl Trecho {
    pub fn simples(texto: impl Into<String>) -> Trecho {
        Trecho { texto: texto.into(), cor: None, peso: None, familia: None, tamanho: None }
    }
    /// Em seminegrito e na cor principal: o destaque de uma frase (`<b>` do protótipo).
    pub fn forte(texto: impl Into<String>) -> Trecho {
        Trecho { texto: texto.into(), cor: Some(TEXTO), peso: Some(SEMINEGRITO), familia: None, tamanho: None }
    }
    pub fn mono(texto: impl Into<String>) -> Trecho {
        Trecho { texto: texto.into(), cor: None, peso: None, familia: Some(Familia::Mono), tamanho: None }
    }
    /// O glifo de um ícone, no tamanho dado, no meio de um texto.
    pub fn icone(i: Icone, tamanho: f32) -> Trecho {
        Trecho { texto: i.glifo().to_string(), cor: None, peso: Some(NORMAL), familia: Some(Familia::Icones), tamanho: Some(tamanho) }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Texto {
    pub ret: Ret,
    pub trechos: Vec<Trecho>,
    pub fonte: Fonte,
    pub cor: Cor,
    pub alinha: Alinha,
    /// Centrado na vertical do retângulo (senão, encostado em cima).
    pub meio: bool,
    /// Quebra em linhas; sem quebra, o que não cabe termina em reticências.
    pub quebra: bool,
}

impl Texto {
    pub fn texto_corrido(&self) -> String {
        self.trechos.iter().map(|t| t.texto.as_str()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Luz {
    /// Vermelho: no ar.
    NoAr,
    /// Âmbar: aguardando.
    Aguardando,
    /// Verde: conectado.
    Conectado,
}

impl Luz {
    pub fn cor(self) -> Cor {
        match self {
            Luz::NoAr => NO_AR,
            Luz::Aguardando => AGUARDANDO,
            Luz::Conectado => CONECTADO,
        }
    }
}

/// Onde a pílula se ancora: pela esquerda, ou pelo centro (a largura dela depende do texto, e só o
/// desenho mede).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ancora {
    Esquerda(f32),
    Centro(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// Retângulo de cantos redondos, cheio, com borda opcional (a borda fica **dentro** do
    /// retângulo).
    Caixa { ret: Ret, raio: f32, fundo: Cor, borda: Option<(Cor, f32)> },
    Circulo { cx: f32, cy: f32, raio: f32, cor: Cor },
    /// Circunferência (o anel da marca, o anel do Gravar).
    Anel { cx: f32, cy: f32, raio: f32, espessura: f32, cor: Cor },
    Linha { x0: f32, y0: f32, x1: f32, y1: f32, espessura: f32, cor: Cor },
    Texto(Texto),
    /// Um glifo de ícone, centrado no retângulo.
    Icone { ret: Ret, icone: Icone, tamanho: f32, cor: Cor },
    /// A pílula de estado (§4): altura 28, cápsula, bolinha de 8 e a palavra em caixa alta.
    Pilula { ancora: Ancora, y: f32, luz: Luz, texto: String },
}

// =============================================================================================
// Construtores
// =============================================================================================

/// Um texto de uma cor, à esquerda, encostado em cima, sem quebra.
pub fn texto(ret: Ret, s: impl Into<String>, f: Fonte, cor: Cor) -> Texto {
    Texto { ret, trechos: vec![Trecho::simples(s)], fonte: f, cor, alinha: Alinha::Esquerda, meio: false, quebra: false }
}

impl Texto {
    pub fn centro(mut self) -> Texto {
        self.alinha = Alinha::Centro;
        self
    }
    pub fn direita(mut self) -> Texto {
        self.alinha = Alinha::Direita;
        self
    }
    pub fn meio(mut self) -> Texto {
        self.meio = true;
        self
    }
    pub fn quebra(mut self) -> Texto {
        self.quebra = true;
        self
    }
    pub fn item(self) -> Item {
        Item::Texto(self)
    }
}

/// Um texto feito de trechos.
pub fn rico(ret: Ret, trechos: Vec<Trecho>, f: Fonte, cor: Cor) -> Texto {
    Texto { ret, trechos, fonte: f, cor, alinha: Alinha::Esquerda, meio: false, quebra: false }
}

pub fn caixa(ret: Ret, raio: f32, fundo: Cor) -> Item {
    Item::Caixa { ret, raio, fundo, borda: None }
}

pub fn caixa_com_borda(ret: Ret, raio: f32, fundo: Cor, borda: Cor, espessura: f32) -> Item {
    Item::Caixa { ret, raio, fundo, borda: Some((borda, espessura)) }
}

/// A caixa de caixa alta do rótulo de seção e da pílula. `to_uppercase` do Rust, e não o do
/// DirectWrite (que não tem): "ç" e "ã" viram "Ç" e "Ã" certo.
pub fn caixa_alta(s: &str) -> String {
    s.to_uppercase()
}

// =============================================================================================
// As peças (§4)
// =============================================================================================

/// **A marca**: o "Q" é uma lente (anel) com a luz de estúdio acesa (a bolinha vermelha no lugar
/// do rabo). Anel de raio 10,5/32 do lado com traço 3,6/32, cor `texto`; bolinha `noAr` de raio
/// 4,6/32 com centro em (25,2; 25,2)/32. O centro do anel é o do protótipo, (15; 15)/32.
pub fn marca(x: f32, y: f32, lado: f32) -> Vec<Item> {
    let u = lado / 32.0;
    vec![
        Item::Anel { cx: x + 15.0 * u, cy: y + 15.0 * u, raio: 10.5 * u, espessura: 3.6 * u, cor: TEXTO },
        Item::Circulo { cx: x + 25.2 * u, cy: y + 25.2 * u, raio: 4.6 * u, cor: NO_AR },
    ]
}

/// O tipo do botão (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipoDeBotao {
    /// Fundo `acento`, texto branco.
    Principal,
    /// Fundo `superficieAlta`, texto `texto`.
    Secundario,
    /// Vermelho a 18 %, texto `#FF8A80`, quadradinho de parar à esquerda.
    Perigo,
    /// O "Parar" do Windows: vermelho cheio `#C42B1C`, texto branco, quadradinho (§7.4).
    PararCheio,
}

/// O anel do foco do teclado, por cima da peça (o `ODS_FOCUS` do dono do desenho).
pub fn anel_de_foco(l: f32, a: f32, raio: f32) -> Item {
    Item::Caixa { ret: Ret::new(0.0, 0.0, l, a), raio, fundo: Cor::rgba(0, 0), borda: Some((TEXTO.vezes(0.85), 2.0)) }
}

/// **O botão**, no retângulo `(0, 0, l, a)`: fundo, rótulo seminegrito de 14 e o ícone opcional à
/// esquerda (como um trecho do mesmo texto, para o conjunto centrar junto).
pub fn botao(l: f32, a: f32, tipo: TipoDeBotao, rotulo: &str, icone: Option<Icone>, fonte_do_rotulo: Fonte) -> Vec<Item> {
    let (fundo, tinta) = match tipo {
        TipoDeBotao::Principal => (ACENTO, BRANCO),
        TipoDeBotao::Secundario => (SUPERFICIE_ALTA, TEXTO),
        TipoDeBotao::Perigo => (PERIGO_FUNDO, PERIGO_TEXTO),
        TipoDeBotao::PararCheio => (PARAR_WINDOWS, BRANCO),
    };
    let r = Ret::new(0.0, 0.0, l, a);
    let mut itens = vec![caixa(r, RAIO_DO_BOTAO, fundo)];
    let parar = matches!(tipo, TipoDeBotao::Perigo | TipoDeBotao::PararCheio);
    if parar {
        // O quadradinho de parar: desenhado, e posto antes do texto centrado pela conta da largura
        // estimada (o texto vai centrado no que sobra).
        let lt = largura_estimada(rotulo, fonte_do_rotulo);
        let grupo = 12.0 + 10.0 + lt;
        let x0 = ((l - grupo) / 2.0).max(10.0);
        itens.push(caixa(Ret::new(x0, (a - 12.0) / 2.0, 12.0, 12.0), 3.0, tinta));
        itens.push(texto(Ret::new(x0 + 22.0, 0.0, (l - x0 - 22.0 - 6.0).max(1.0), a), rotulo, fonte_do_rotulo, tinta).meio().item());
        return itens;
    }
    let mut trechos = Vec::new();
    if let Some(i) = icone {
        trechos.push(Trecho::icone(i, 16.0));
        trechos.push(Trecho::simples("\u{2002}"));
    }
    trechos.push(Trecho::simples(rotulo));
    itens.push(Item::Texto(Texto {
        ret: r.dentro(8.0, 0.0),
        trechos,
        fonte: fonte_do_rotulo,
        cor: tinta,
        alinha: Alinha::Centro,
        meio: true,
        quebra: false,
    }));
    itens
}

/// **O ladrilho** de uma origem (§4, §7.1), em `(0, 0, l, a)`: ícone num quadrado de 34 (cantos de
/// 6, `acentoFundo`, ícone `acentoClaro`), título 14 seminegrito, legenda 12 `texto2`. Escolhido:
/// fundo `acento` a 14 %, borda 2 `acento` e o selo redondo com ✓ no canto.
pub fn ladrilho(l: f32, a: f32, icone: Icone, titulo: &str, detalhe: &str, detalhe_mono: bool, escolhido: bool) -> Vec<Item> {
    let r = Ret::new(0.0, 0.0, l, a);
    let mut itens = Vec::new();
    if escolhido {
        itens.push(caixa_com_borda(r, RAIO_DO_CARTAO, ACENTO_ESCOLHIDO.sobre(FUNDO), ACENTO, 2.0));
    } else {
        itens.push(caixa_com_borda(r, RAIO_DO_CARTAO, SUPERFICIE, CONTORNO, 1.0));
    }
    let q = 34.0_f32.min(a - 12.0);
    let quadrado = Ret::new(14.0, (a - q) / 2.0, q, q);
    let (fundo_do_icone, tinta_do_icone) =
        if escolhido { (Cor::rgba(0x6A5AF9, 77), Cor::rgb(0xE0DBFF)) } else { (ACENTO_FUNDO, ACENTO_CLARO) };
    itens.push(caixa(quadrado, 6.0, fundo_do_icone));
    itens.push(Item::Icone { ret: quadrado, icone, tamanho: 18.0, cor: tinta_do_icone });
    let x = quadrado.direita() + 12.0;
    let selo = if escolhido { 20.0 + 12.0 } else { 0.0 };
    let largura = (l - x - 12.0 - selo).max(1.0);
    let com_detalhe = !detalhe.is_empty() && a >= 50.0;
    if com_detalhe {
        let meio = a / 2.0;
        itens.push(texto(Ret::new(x, meio - 20.0, largura, 20.0), titulo, F_CORPO_FORTE, TEXTO).meio().item());
        let f = if detalhe_mono { F_MONO_12 } else { F_LEGENDA };
        itens.push(texto(Ret::new(x, meio, largura, 18.0), detalhe, f, TEXTO2).meio().item());
    } else {
        itens.push(texto(Ret::new(x, 0.0, largura, a), titulo, F_CORPO_FORTE, TEXTO).meio().item());
    }
    if escolhido {
        let cx = l - 14.0 - 10.0;
        let cy = a / 2.0;
        itens.push(Item::Circulo { cx, cy, raio: 10.0, cor: ACENTO });
        itens.push(Item::Icone { ret: Ret::new(cx - 10.0, cy - 10.0, 20.0, 20.0), icone: Icone::Marcado, tamanho: 11.0, cor: BRANCO });
    }
    itens
}

/// As cores da pílula: fundo, bolinha e palavra.
pub fn cores_da_pilula(luz: Luz) -> (Cor, Cor, Cor) {
    match luz {
        Luz::NoAr => (NO_AR_CHEIO, BRANCO, BRANCO),
        Luz::Aguardando => (Cor::rgba(0xFFB340, 41), AGUARDANDO, AGUARDANDO_TEXTO),
        Luz::Conectado => (Cor::rgba(0x32D74B, 36), CONECTADO, CONECTADO_TEXTO),
    }
}

/// A largura da pílula para uma palavra de largura `lt` (medida pelo desenho, ou estimada).
pub fn largura_da_pilula(lt: f32) -> f32 {
    12.0 + 8.0 + 8.0 + lt + 12.0
}

/// **O letreiro do PIN** (§4): seis casas de 54 × 70, cantos de 12, `superficie` com borda branca
/// a 10 %, dígito mono negrito, 7 de espaço entre as casas e 10 a mais entre os dois grupos.
/// Centrado em `cx`.
pub fn letreiro(cx: f32, y: f32, pin: &str) -> Vec<Item> {
    let digitos: Vec<char> = pin.chars().collect();
    let largura = largura_do_letreiro();
    let mut x = cx - largura / 2.0;
    let mut itens = Vec::new();
    for i in 0..6 {
        let casa = Ret::new(x, y, CASA_L, CASA_A);
        itens.push(caixa_com_borda(casa, 12.0, SUPERFICIE, BORDA_DE_CAMPO, 1.0));
        let d = digitos.get(i).map(|c| c.to_string()).unwrap_or_default();
        itens.push(texto(casa, d, F_PIN, TEXTO).centro().meio().item());
        x += CASA_L + CASA_ESPACO + if i == 2 { CASA_ENTRE_GRUPOS } else { 0.0 };
    }
    itens
}

pub fn largura_do_letreiro() -> f32 {
    6.0 * CASA_L + 5.0 * CASA_ESPACO + CASA_ENTRE_GRUPOS
}

/// **O interruptor** desenhado, com o canto de cima à esquerda em `(x, y)`: 40 × 20. Ligado: a
/// cápsula cheia de `acento` e a bolinha à direita; desligado: só o contorno e a bolinha à esquerda.
pub fn interruptor(x: f32, y: f32, ligado: bool) -> Vec<Item> {
    let r = Ret::new(x, y, INTERRUPTOR_L, INTERRUPTOR_A);
    if ligado {
        vec![
            caixa(r, INTERRUPTOR_A / 2.0, ACENTO),
            Item::Circulo { cx: x + INTERRUPTOR_L - 10.0, cy: y + 10.0, raio: 6.0, cor: BRANCO },
        ]
    } else {
        vec![
            Item::Caixa { ret: r, raio: INTERRUPTOR_A / 2.0, fundo: Cor::rgba(0, 0), borda: Some((TEXTO2, 1.0)) },
            Item::Circulo { cx: x + 10.0, cy: y + 10.0, raio: 6.0, cor: TEXTO2 },
        ]
    }
}

/// **A linha com interruptor** (o som, o microfone, "tocar mesmo com a câmera"), em `(0, 0, l, a)`:
/// cartão, título e explicação à esquerda, "Ligado"/"Desligado" e o interruptor à direita. A linha
/// inteira é o botão: clicar em qualquer lugar troca.
pub fn linha_de_interruptor(l: f32, a: f32, titulo: &str, detalhe: &str, ligado: bool) -> Vec<Item> {
    let r = Ret::new(0.0, 0.0, l, a);
    let mut itens = vec![caixa_com_borda(r, RAIO_DO_CARTAO, SUPERFICIE, CONTORNO, 1.0)];
    let direita = 16.0 + INTERRUPTOR_L + 12.0 + 76.0;
    let largura = (l - 16.0 - direita).max(1.0);
    if detalhe.is_empty() {
        itens.push(texto(Ret::new(16.0, 0.0, largura, a), titulo, F_CORPO_FORTE, TEXTO).meio().item());
    } else {
        itens.push(texto(Ret::new(16.0, a / 2.0 - 20.0, largura, 20.0), titulo, F_CORPO_FORTE, TEXTO).meio().item());
        itens.push(texto(Ret::new(16.0, a / 2.0, largura, 18.0), detalhe, F_LEGENDA, TEXTO2).meio().item());
    }
    let xi = l - 16.0 - INTERRUPTOR_L;
    itens.push(
        texto(Ret::new(xi - 12.0 - 76.0, 0.0, 76.0, a), if ligado { crate::idioma::t("Ligado") } else { crate::idioma::t("Desligado") }, F_LEGENDA_13, TEXTO_DA_BARRA)
            .direita()
            .meio()
            .item(),
    );
    itens.extend(interruptor(xi, (a - INTERRUPTOR_A) / 2.0, ligado));
    itens
}

/// **O item da barra lateral** (§7), em `(0, 0, l, a)`: escolhido, fundo branco a 6 % e o traço de
/// 3 DIP em `#8B7DFF` à esquerda. Com a sessão de pé, o item dela mostra a bolinha do estado e diz
/// o estado à direita.
pub fn item_da_barra(l: f32, a: f32, icone: Icone, rotulo: &str, escolhido: bool, estado: Option<(Luz, &str)>) -> Vec<Item> {
    let mut itens = Vec::new();
    if escolhido {
        itens.push(caixa(Ret::new(0.0, 0.0, l, a), RAIO_DO_BOTAO, ITEM_ESCOLHIDO.sobre(BARRA)));
        itens.push(caixa(Ret::new(0.0, 10.0, 3.0, (a - 20.0).max(4.0)), 1.5, TRACO_ESCOLHIDO));
    }
    let icone_r = Ret::new(12.0, (a - 20.0) / 2.0, 20.0, 20.0);
    itens.push(Item::Icone { ret: icone_r, icone, tamanho: 16.0, cor: if escolhido { ICONE_ESCOLHIDO } else { TEXTO2 } });
    let mut direita = 12.0;
    if let Some((luz, dito)) = estado {
        let ld = largura_estimada(dito, F_LEGENDA) + 4.0;
        itens.push(texto(Ret::new(l - 12.0 - ld, 0.0, ld, a), dito, F_LEGENDA, TEXTO2).direita().meio().item());
        itens.push(Item::Circulo { cx: l - 12.0 - ld - 8.0, cy: a / 2.0, raio: 4.0, cor: luz.cor() });
        direita = 12.0 + ld + 16.0;
    }
    itens.push(
        texto(Ret::new(42.0, 0.0, (l - 42.0 - direita).max(1.0), a), rotulo, F_CORPO, if escolhido { TEXTO } else { TEXTO_DA_BARRA })
            .meio()
            .item(),
    );
    itens
}

/// **O chip do endereço** (§4), em `(0, 0, l, a)`: cápsula `superficie` com contorno, o endereço
/// em mono e o ícone de copiar `acentoClaro`. Copiado: o ✓ e "Copiado".
pub fn chip(l: f32, a: f32, endereco: &str, copiado: bool) -> Vec<Item> {
    let r = Ret::new(0.0, 0.0, l, a);
    let mut itens = vec![caixa_com_borda(r, a / 2.0, SUPERFICIE, CONTORNO, 1.0)];
    let icone_r = Ret::new(l - 16.0 - 18.0, (a - 18.0) / 2.0, 18.0, 18.0);
    if copiado {
        itens.push(texto(Ret::new(18.0, 0.0, (l - 18.0 - 44.0).max(1.0), a), crate::idioma::t("Copiado"), F_CORPO_FORTE, ACENTO_CLARO).meio().item());
        itens.push(Item::Icone { ret: icone_r, icone: Icone::Marcado, tamanho: 14.0, cor: ACENTO_CLARO });
    } else {
        itens.push(texto(Ret::new(18.0, 0.0, (l - 18.0 - 44.0).max(1.0), a), endereco, F_ENDERECO, TEXTO).meio().item());
        itens.push(Item::Icone { ret: icone_r, icone: Icone::Copiar, tamanho: 14.0, cor: ACENTO_CLARO });
    }
    itens
}

/// A largura do chip para um endereço.
pub fn largura_do_chip(endereco: &str) -> f32 {
    (18.0 + largura_estimada(endereco, F_ENDERECO) + 12.0 + 18.0 + 16.0).max(150.0)
}

/// O tom de um aviso (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tom {
    Ambar,
    Vermelho,
    Info,
}

/// **O aviso** (§4): cantos de 8 (Windows), ícone e texto de 13.
pub fn aviso(r: Ret, tom: Tom, s: &str) -> Vec<Item> {
    let (fundo, tinta, icone, cor_do_icone) = match tom {
        Tom::Ambar => (Cor::rgba(0xFFB340, 31).sobre(FUNDO), AGUARDANDO_TEXTO, Icone::Aviso, AGUARDANDO),
        Tom::Vermelho => (Cor::rgba(0xFF453A, 41).sobre(FUNDO), PERIGO_TEXTO, Icone::Aviso, PERIGO_TEXTO),
        Tom::Info => (SUPERFICIE, TEXTO2, Icone::Info, ACENTO_CLARO),
    };
    vec![
        caixa(r, RAIO_DO_CARTAO, fundo),
        Item::Icone { ret: Ret::new(r.x + 12.0, r.y + 11.0, 18.0, 18.0), icone, tamanho: 15.0, cor: cor_do_icone },
        texto(Ret::new(r.x + 40.0, r.y + 10.0, (r.l - 52.0).max(1.0), (r.a - 20.0).max(1.0)), s, F_LEGENDA_13, tinta).quebra().item(),
    ]
}

/// A altura de um aviso com `s` na largura `l` (estimada: até quatro linhas de 18).
pub fn altura_do_aviso(s: &str, l: f32) -> f32 {
    let por_linha = ((l - 52.0) / (13.0 * 0.52)).max(10.0);
    let linhas = ((s.chars().count() as f32) / por_linha).ceil().clamp(1.0, 4.0);
    20.0 + linhas * 18.0
}

/// **O campo** (§4) desenhado em volta do `EDIT` nativo: cantos de 6, `superficie`, borda branca
/// a 10 %; com o foco (ou pedindo atenção, a volta ao PIN), a borda de baixo de 2 em violeta.
pub fn campo(r: Ret, destaque: bool) -> Vec<Item> {
    let mut itens = vec![caixa_com_borda(r, RAIO_DO_CAMPO, SUPERFICIE, BORDA_DE_CAMPO, 1.0)];
    if destaque {
        itens.push(caixa(Ret::new(r.x + 1.0, r.baixo() - 2.0, r.l - 2.0, 2.0), 1.0, FOCO_DE_CAMPO));
    }
    itens
}

/// Um cartão: `superficie` com contorno, cantos de 8.
pub fn cartao(r: Ret) -> Item {
    caixa_com_borda(r, RAIO_DO_CARTAO, SUPERFICIE, CONTORNO, 1.0)
}

/// **O rótulo de seção** (§4): 12 seminegrito, caixa alta, +0,08 em, `texto3`.
pub fn rotulo(r: Ret, s: &str) -> Item {
    texto(r, caixa_alta(s), F_ROTULO, TEXTO3).meio().item()
}

/// O título de uma tela.
pub fn titulo(r: Ret, s: &str) -> Item {
    texto(r, s, F_TITULO, TEXTO).meio().item()
}

/// **O cartão do teleprompter** (§7.3), em `(0, 0, l, a)`: ícone num quadrado de 44, título 16
/// seminegrito, explicação de 13 em até três linhas (16 de margem: "Texto com a câmera" cabe numa linha).
pub fn cartao_de_papel(l: f32, a: f32, icone: Icone, titulo: &str, detalhe: &str) -> Vec<Item> {
    let r = Ret::new(0.0, 0.0, l, a);
    let q = Ret::new(16.0, 18.0, 44.0, 44.0);
    vec![
        caixa_com_borda(r, RAIO_DO_CARTAO, SUPERFICIE, CONTORNO, 1.0),
        caixa(q, RAIO_DO_CARTAO, ACENTO_FUNDO),
        Item::Icone { ret: q, icone, tamanho: 20.0, cor: ACENTO_CLARO },
        texto(Ret::new(16.0, 76.0, l - 32.0, 22.0), titulo, F_TITULO_DE_CARTAO, TEXTO).meio().item(),
        texto(Ret::new(16.0, 104.0, l - 32.0, (a - 104.0 - 14.0).max(1.0)), detalhe, F_LEGENDA_13, TEXTO2).quebra().item(),
    ]
}

/// O que um controle redondo da câmera é (§6.5, §7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redondo {
    /// O microfone: 56; ligado, `noAr` a 85 %; desligado, branco a 16 %.
    Microfone { ligado: bool },
    /// Gravar: 78, anel branco de 4 e miolo `noAr`; gravando, o miolo vira quadrado arredondado.
    Gravar { gravando: bool },
    /// Parar: 56, vermelho a 24 %, quadradinho `#FF8A80`.
    Parar,
    /// **Os ajustes da câmera** (R9, `docs/controles-de-camera.md` §4.1): 56, branco a 16 %, a
    /// engrenagem (U+E713).
    Ajustes,
}

/// A altura da peça redonda com a legenda: o círculo maior (78), 6 e a legenda de 16.
pub const ALTURA_DO_REDONDO: f32 = 78.0 + 6.0 + 16.0;

/// **Um controle redondo** com a legenda embaixo (12, branco a 80 %), em `(0, 0, l, a)`: o
/// círculo centrado na faixa de 78 de cima.
pub fn redondo(l: f32, tipo: Redondo, legenda: &str) -> Vec<Item> {
    let cx = l / 2.0;
    let cy = 39.0;
    let mut itens = Vec::new();
    match tipo {
        Redondo::Microfone { ligado } => {
            itens.push(Item::Circulo { cx, cy, raio: 28.0, cor: if ligado { NO_AR.vezes(0.85) } else { Cor::rgba(0xFFFFFF, 41) } });
            itens.push(Item::Icone { ret: Ret::new(cx - 14.0, cy - 14.0, 28.0, 28.0), icone: Icone::Microfone, tamanho: 20.0, cor: BRANCO });
            if !ligado {
                itens.push(Item::Linha { x0: cx - 11.0, y0: cy - 11.0, x1: cx + 11.0, y1: cy + 11.0, espessura: 2.0, cor: BRANCO });
            }
        }
        Redondo::Gravar { gravando } => {
            itens.push(Item::Anel { cx, cy, raio: 37.0, espessura: 4.0, cor: BRANCO });
            if gravando {
                itens.push(caixa(Ret::new(cx - 14.0, cy - 14.0, 28.0, 28.0), 6.0, NO_AR));
            } else {
                itens.push(Item::Circulo { cx, cy, raio: 29.0, cor: NO_AR });
            }
        }
        Redondo::Parar => {
            itens.push(Item::Circulo { cx, cy, raio: 28.0, cor: Cor::rgba(0xFF453A, 61) });
            itens.push(caixa(Ret::new(cx - 8.0, cy - 8.0, 16.0, 16.0), 3.0, PERIGO_TEXTO));
        }
        Redondo::Ajustes => {
            itens.push(Item::Circulo { cx, cy, raio: 28.0, cor: Cor::rgba(0xFFFFFF, 41) });
            itens.push(Item::Icone { ret: Ret::new(cx - 14.0, cy - 14.0, 28.0, 28.0), icone: Icone::Ajustes, tamanho: 20.0, cor: BRANCO });
        }
    }
    // Gravando, a legenda é o tempo, em mono (§6.5).
    let f = if matches!(tipo, Redondo::Gravar { gravando: true }) { F_MONO_12 } else { F_LEGENDA };
    itens.push(texto(Ret::new(0.0, 84.0, l, 16.0), legenda, f, BRANCO.vezes(0.8)).centro().meio().item());
    itens
}

/// **Um segmento** do segmentado (o volume, §7.2), em `(0, 0, l, a)`: o escolhido cheio de
/// `acento` com texto branco; os outros, `texto2` sobre o fundo do grupo (pintado atrás pela janela).
pub fn segmento(l: f32, a: f32, rotulo: &str, escolhido: bool) -> Vec<Item> {
    let r = Ret::new(0.0, 0.0, l, a);
    let mut itens = vec![caixa(r, 0.0, SUPERFICIE_ALTA)];
    if escolhido {
        itens.push(caixa(r.dentro(2.0, 2.0), 4.0, ACENTO));
    }
    itens.push(texto(r, rotulo, F_BOTAO_PEQUENO, if escolhido { BRANCO } else { TEXTO2 }).centro().meio().item());
    itens
}

/// **A linha de um aparelho** na lista "NA REDE AGORA" (§7.2), em `(0, 0, l, a)`: o cartão tem 6 a
/// menos embaixo (o espaço entre as linhas da lista, que é uma só janela).
pub fn linha_de_aparelho(l: f32, a: f32, nome: &str, tipo: &str, endereco: &str, escolhida: bool) -> Vec<Item> {
    let r = Ret::new(0.0, 0.0, l, (a - 6.0).max(1.0));
    let mut itens = Vec::new();
    if escolhida {
        itens.push(caixa_com_borda(r, RAIO_DO_CARTAO, ACENTO_ESCOLHIDO.sobre(FUNDO), ACENTO, 1.0));
    } else {
        itens.push(caixa_com_borda(r, RAIO_DO_CARTAO, SUPERFICIE, CONTORNO, 1.0));
    }
    let q = Ret::new(14.0, (r.a - 34.0) / 2.0, 34.0, 34.0);
    let (fundo_do_icone, tinta_do_icone) =
        if escolhida { (Cor::rgba(0x6A5AF9, 77), Cor::rgb(0xE0DBFF)) } else { (ACENTO_FUNDO, ACENTO_CLARO) };
    itens.push(caixa(q, 6.0, fundo_do_icone));
    itens.push(Item::Icone { ret: q, icone: Icone::Aparelho, tamanho: 17.0, cor: tinta_do_icone });
    let x = q.direita() + 12.0;
    let largura = (l - x - 14.0).max(1.0);
    itens.push(texto(Ret::new(x, r.a / 2.0 - 19.0, largura, 19.0), nome, F_CORPO_FORTE, TEXTO).meio().item());
    let mut trechos = vec![Trecho::simples(format!("{tipo} · "))];
    trechos.push(Trecho::mono(endereco));
    itens.push(rico(Ret::new(x, r.a / 2.0 + 1.0, largura, 17.0), trechos, F_LEGENDA, TEXTO2).meio().item());
    itens
}

/// **O cartão de número** do "no ar" (§7.4): rótulo em caixa alta e o valor.
pub fn cartao_de_numero(r: Ret, chave: &str, valor: &str, mono: bool, cor_do_valor: Cor) -> Vec<Item> {
    vec![
        cartao(r),
        texto(Ret::new(r.x + 12.0, r.y + 12.0, r.l - 24.0, 16.0), caixa_alta(chave), F_ROTULO_11, TEXTO3).meio().item(),
        texto(Ret::new(r.x + 12.0, r.y + 32.0, r.l - 24.0, 22.0), valor, if mono { F_VALOR_MONO } else { F_VALOR }, cor_do_valor)
            .meio()
            .item(),
    ]
}

// =============================================================================================
// A tabela de lugares (a janela fixa de 880 × 580 DIP)
// =============================================================================================

/// **Todos os lugares da janela**, em DIP, com a origem no canto de cima à esquerda da área de
/// cliente. Os que dependem de quantos itens há (ladrilhos, receptores) são funções; o resto é
/// constante. Nada fora daqui escreve uma coordenada.
pub mod lugar {
    use super::{Ret, ALTURA_DO_BOTAO, ALTURA_MINIMA, LARGURA_DA_BARRA, LARGURA_MINIMA, MARGEM, PE, TOPO};

    // --- a barra lateral ---

    /// A marca: o canto do ícone de 26 e o nome "Quall Monitor".
    pub const MARCA_X: f32 = 20.0;
    pub const MARCA_Y: f32 = 17.0;
    pub const MARCA_LADO: f32 = 26.0;
    pub const MARCA_TEXTO: Ret = Ret::new(56.0, 14.0, 160.0, 32.0);
    /// Espelhar, Exibir e Teleprompter.
    pub const ITENS: [Ret; 3] = [
        Ret::new(8.0, 62.0, 214.0, 36.0),
        Ret::new(8.0, 100.0, 214.0, 36.0),
        Ret::new(8.0, 138.0, 214.0, 36.0),
    ];
    /// "Um papel por vez…", com a sessão de pé.
    pub const NOTA_DA_SESSAO: Ret = Ret::new(20.0, 186.0, 190.0, 66.0);
    /// "Este computador aparece como" e o nome.
    pub const NOME_ROTULO: Ret = Ret::new(20.0, 474.0, 190.0, 16.0);
    pub const NOME: Ret = Ret::new(20.0, 490.0, 190.0, 24.0);
    /// O item Ajustes, no pé.
    pub const ITEM_AJUSTES: Ret = Ret::new(8.0, ALTURA_MINIMA - 12.0 - 36.0, 214.0, 36.0);

    // --- o painel ---

    /// A área do painel (à direita da barra), com o canto de cima à esquerda redondo.
    pub const PAINEL: Ret = Ret::new(LARGURA_DA_BARRA, 0.0, LARGURA_MINIMA - LARGURA_DA_BARRA, ALTURA_MINIMA);
    /// A coluna útil do painel.
    pub const X: f32 = LARGURA_DA_BARRA + MARGEM;
    pub const L: f32 = LARGURA_MINIMA - LARGURA_DA_BARRA - 2.0 * MARGEM;
    pub const DIREITA: f32 = X + L;
    /// O centro do painel (as telas de espera e de conexão são centradas nele).
    pub const CX: f32 = LARGURA_DA_BARRA + (LARGURA_MINIMA - LARGURA_DA_BARRA) / 2.0;
    pub const TITULO: Ret = Ret::new(X, TOPO, L - IDIOMA_L - 12.0, 36.0);
    /// **O seletor de idioma "PT | EN"** (a tradução, 02/10): no canto de cima à direita do painel,
    /// na linha do título (que encolhe para ele), nos quatro painéis.
    pub const IDIOMA_L: f32 = 92.0;
    pub const IDIOMA: Ret = Ret::new(DIREITA - IDIOMA_L, TOPO + 4.0, IDIOMA_L, 28.0);
    /// Os dois segmentos do seletor: PT e EN.
    pub fn segmentos_do_idioma() -> [Ret; 2] {
        let l = (IDIOMA_L - 4.0) / 2.0;
        [0, 1].map(|i| Ret::new(IDIOMA.x + 2.0 + i as f32 * l, IDIOMA.y + 2.0, l, IDIOMA.a - 4.0))
    }
    /// A explicação sob o título: até duas linhas de 14.
    pub const SUBTITULO: Ret = Ret::new(X, TOPO + 42.0, L, 40.0);
    /// Onde o conteúdo começa, sob o título e a explicação.
    pub const CONTEUDO_Y: f32 = TOPO + 92.0;
    /// O pé: o botão principal à direita (Espelhar, Exibir), e a linha da esquerda (a rede).
    pub const PE_Y: f32 = ALTURA_MINIMA - PE - ALTURA_DO_BOTAO;
    pub const BOTAO_PRINCIPAL: Ret = Ret::new(DIREITA - 150.0, PE_Y, 150.0, ALTURA_DO_BOTAO);
    pub const RODAPE: Ret = Ret::new(X, PE_Y, L - 150.0 - 20.0, ALTURA_DO_BOTAO);
    /// "Cancelar" (secundário, 36 de altura) e "Parar" (cheio, 40).
    pub const CANCELAR: Ret = Ret::new(DIREITA - 120.0, ALTURA_MINIMA - PE - 36.0, 120.0, 36.0);
    pub const PARAR: Ret = Ret::new(DIREITA - 130.0, PE_Y, 130.0, ALTURA_DO_BOTAO);

    // --- Espelhar ---

    pub const LADRILHO_A: f32 = 62.0;
    pub const LADRILHO_ESPACO: f32 = 8.0;
    /// A linha do interruptor (o som ou o microfone), sob os ladrilhos.
    pub const INTERRUPTOR_A: f32 = 62.0;

    /// Os ladrilhos, o interruptor e o aviso de Espelhar. `n` ladrilhos em 2 colunas (3 com mais de
    /// 8, 4 com mais de 12), da altura que couber entre o título e o pé — nunca menos de 40, e é
    /// para isso que as colunas crescem. O aviso fica logo acima do pé, com `altura_do_aviso`.
    pub fn espelhar(n: usize, com_interruptor: bool, altura_do_aviso: Option<f32>) -> (Vec<Ret>, Option<Ret>, Option<Ret>) {
        let mut fim = PE_Y - 16.0;
        let aviso = altura_do_aviso.map(|a| {
            let r = Ret::new(X, fim - a, L, a);
            fim = r.y - 12.0;
            r
        });
        if com_interruptor {
            fim -= INTERRUPTOR_A + 16.0;
        }
        let disponivel = (fim - CONTEUDO_Y).max(40.0);
        let (colunas, altura) = grade(n, disponivel);
        let largura = (L - (colunas as f32 - 1.0) * LADRILHO_ESPACO) / colunas as f32;
        let mut ladrilhos = Vec::with_capacity(n);
        for i in 0..n {
            let (c, l) = (i % colunas, i / colunas);
            ladrilhos.push(Ret::new(
                X + c as f32 * (largura + LADRILHO_ESPACO),
                CONTEUDO_Y + l as f32 * (altura + LADRILHO_ESPACO),
                largura,
                altura,
            ));
        }
        let fim_dos_ladrilhos = ladrilhos.last().map(|r| r.baixo()).unwrap_or(CONTEUDO_Y + 24.0);
        let interruptor = com_interruptor.then(|| Ret::new(X, fim_dos_ladrilhos + 16.0, L, INTERRUPTOR_A));
        (ladrilhos, interruptor, aviso)
    }

    /// Quantas colunas e que altura de ladrilho, para `n` ladrilhos numa altura `disponivel`.
    pub fn grade(n: usize, disponivel: f32) -> (usize, f32) {
        let n = n.max(1);
        for colunas in [2usize, 3, 4] {
            let linhas = n.div_ceil(colunas) as f32;
            let altura = ((disponivel - (linhas - 1.0) * LADRILHO_ESPACO) / linhas).min(LADRILHO_A);
            if altura >= 44.0 || colunas == 4 {
                return (colunas, altura.max(30.0));
            }
        }
        unreachable!("o laço devolve na quarta volta")
    }

    /// "Nenhum monitor foi encontrado.", sem ladrilho nenhum.
    pub const SEM_FONTES: Ret = Ret::new(X, CONTEUDO_Y, L, 24.0);

    // --- Exibir ---

    pub const ROTULO_DA_LISTA: Ret = Ret::new(X, CONTEUDO_Y, L - 130.0, 18.0);
    pub const PROCURANDO: Ret = Ret::new(DIREITA - 120.0, CONTEUDO_Y, 120.0, 18.0);
    /// A lista: **altura fixa de três linhas**, que nunca empurra os campos (a mesma regra do
    /// Android, §11.3). Uma linha tem 60 (o cartão de 54 e 6 de espaço).
    pub const LINHA_DA_LISTA: f32 = 60.0;
    pub const LISTA: Ret = Ret::new(X, CONTEUDO_Y + 26.0, L, 3.0 * LINHA_DA_LISTA);
    /// A frase da lista vazia, no lugar da primeira linha.
    pub const LISTA_VAZIA: Ret = Ret::new(X, CONTEUDO_Y + 26.0, L, 54.0);
    pub const CAMPOS_Y: f32 = CONTEUDO_Y + 26.0 + 3.0 * LINHA_DA_LISTA + 18.0;
    /// O PIN tem 170: os seis dígitos em mono de 15 e a dica "se for a 1ª vez" cabem.
    pub const LARGURA_DO_PIN: f32 = 170.0;
    pub const ROTULO_DO_ENDERECO: Ret = Ret::new(X, CAMPOS_Y, L - LARGURA_DO_PIN - 12.0, 18.0);
    pub const CAMPO_DO_ENDERECO: Ret = Ret::new(X, CAMPOS_Y + 24.0, L - LARGURA_DO_PIN - 12.0, super::ALTURA_DO_CAMPO);
    pub const ROTULO_DO_PIN: Ret = Ret::new(DIREITA - LARGURA_DO_PIN, CAMPOS_Y, LARGURA_DO_PIN, 18.0);
    pub const CAMPO_DO_PIN: Ret = Ret::new(DIREITA - LARGURA_DO_PIN, CAMPOS_Y + 24.0, LARGURA_DO_PIN, super::ALTURA_DO_CAMPO);
    pub const LEGENDA_DO_PIN: Ret = Ret::new(X, CAMPOS_Y + 24.0 + 40.0 + 6.0, L, 18.0);
    pub const AVISO_DO_EXIBIR_Y: f32 = CAMPOS_Y + 24.0 + 40.0 + 6.0 + 18.0 + 8.0;
    /// O `EDIT` nativo dentro do campo desenhado: 12 de cada lado, 22 de altura, centrado.
    pub fn edit_no_campo(campo: Ret) -> Ret {
        Ret::new(campo.x + 12.0, campo.y + (campo.a - 22.0) / 2.0, campo.l - 24.0, 22.0)
    }

    // --- Teleprompter ---

    pub fn cartoes_do_teleprompter() -> [Ret; 3] {
        let l = (L - 2.0 * 10.0) / 3.0;
        [0, 1, 2].map(|i| Ret::new(X + i as f32 * (l + 10.0), CONTEUDO_Y, l, 176.0))
    }
    pub const NOTA_DO_TELEPROMPTER: Ret = Ret::new(X, ALTURA_MINIMA - PE - 18.0, L, 18.0);

    // --- Ajustes ---

    /// Os cartões dos Ajustes: aparelhos pareados, diário, versão e o driver da tela estendida
    /// (02/10, noite).
    pub const LINHAS_DOS_AJUSTES: [Ret; 4] = [
        Ret::new(X, TOPO + 50.0, L, 62.0),
        Ret::new(X, TOPO + 50.0 + 70.0, L, 62.0),
        Ret::new(X, TOPO + 50.0 + 140.0, L, 62.0),
        // O do driver é mais alto: a frase do SudoVDA de outro programa quebra em até três linhas.
        Ret::new(X, TOPO + 50.0 + 210.0, L, 100.0),
    ];
    pub const ESQUECER: Ret = Ret::new(DIREITA - 16.0 - 176.0, TOPO + 50.0 + 15.0, 176.0, 32.0);
    pub const DIARIO: Ret = Ret::new(DIREITA - 16.0 - 140.0, TOPO + 50.0 + 70.0 + 15.0, 140.0, 32.0);
    /// Licenças, privacidade e suporte, no cartão da versão (PT e EN).
    pub const AJUSTES_LARGURA_DOS_LINKS: f32 = 104.0 + 8.0 + 120.0 + 8.0 + 104.0;
    pub const LICENCAS: Ret = Ret::new(DIREITA - 16.0 - AJUSTES_LARGURA_DOS_LINKS, TOPO + 50.0 + 140.0 + 15.0, 104.0, 32.0);
    pub const PRIVACIDADE: Ret = Ret::new(LICENCAS.x + LICENCAS.l + 8.0, LICENCAS.y, 120.0, 32.0);
    pub const SUPORTE: Ret = Ret::new(PRIVACIDADE.x + PRIVACIDADE.l + 8.0, LICENCAS.y, 104.0, 32.0);
    /// "Desinstalar" no cartão do driver.
    pub const DRIVER: Ret = Ret::new(DIREITA - 16.0 - 140.0, TOPO + 50.0 + 210.0 + 15.0, 140.0, 32.0);
    /// "Desinstalar pelo instalador do driver" (a loja): o mesmo lugar, mais largo.
    pub const DRIVER_LARGO: Ret = Ret::new(DIREITA - 16.0 - 280.0, TOPO + 50.0 + 210.0 + 15.0, 280.0, 32.0);

    // --- Esperando e Conectando (centrados) ---

    pub const PILULA_Y: f32 = TOPO;
    pub const TITULO_DA_SESSAO: Ret = Ret::new(X, TOPO + 40.0, L, 40.0);
    pub const INSTRUCAO: Ret = Ret::new(CX - 240.0, TOPO + 86.0, 480.0, 62.0);
    pub const ROTULO_DO_PIN_DA_ESPERA: Ret = Ret::new(X, 190.0, L, 16.0);
    pub const LETREIRO_Y: f32 = 214.0;
    pub fn letreiro() -> Ret {
        let l = super::largura_do_letreiro();
        Ret::new(CX - l / 2.0, LETREIRO_Y, l, super::CASA_A)
    }
    /// A linha "ou pelo endereço" + chip, sem pares; o chip sozinho, com pares.
    pub const CHIP_Y: f32 = 300.0;
    pub const CHIP_Y_COM_PARES: f32 = 224.0;
    pub const OU_PELO_ENDERECO_L: f32 = 118.0;
    /// O chip centrado, com a largura dada; sem pares, com "ou pelo endereço" à esquerda (o par
    /// inteiro centrado).
    pub fn chip(largura: f32, com_rotulo: bool, y: f32) -> (Option<Ret>, Ret) {
        if com_rotulo {
            let total = OU_PELO_ENDERECO_L + 10.0 + largura;
            let x0 = CX - total / 2.0;
            (Some(Ret::new(x0, y, OU_PELO_ENDERECO_L, super::ALTURA_DO_CHIP)), Ret::new(x0 + OU_PELO_ENDERECO_L + 10.0, y, largura, super::ALTURA_DO_CHIP))
        } else {
            (None, Ret::new(CX - largura / 2.0, y, largura, super::ALTURA_DO_CHIP))
        }
    }
    pub const FRASE_DOS_PAREADOS: Ret = Ret::new(X, 190.0, L, 24.0);
    pub const PIN_NOVO: Ret = Ret::new(X, 274.0, L, 20.0);
    pub const FRASE_DO_SOM: Ret = Ret::new(X, 352.0, L, 18.0);
    pub const AVISO_DA_ESPERA: Ret = Ret::new(X, 374.0, L, 18.0);
    pub const RODAPE_DA_SESSAO: Ret = Ret::new(X, ALTURA_MINIMA - PE - 36.0, L - 120.0 - 20.0, 36.0);
    pub const DESTINO: Ret = Ret::new(X, TOPO + 88.0, L, 24.0);
    pub const PARAGRAFO_DA_CONEXAO: Ret = Ret::new(CX - 240.0, TOPO + 124.0, 480.0, 62.0);

    // --- a câmera pela espera: os três controles redondos (§7.4) ---

    pub const REDONDO_L: f32 = 110.0;
    pub const REDONDOS_Y: f32 = ALTURA_MINIMA - PE - super::ALTURA_DO_REDONDO;
    /// Microfone, Gravar e Parar, centrados no painel.
    pub fn redondos() -> [Ret; 3] {
        [-1.0f32, 0.0, 1.0].map(|k| Ret::new(CX + k * 134.0 - REDONDO_L / 2.0, REDONDOS_Y, REDONDO_L, super::ALTURA_DO_REDONDO))
    }
    pub const LINHA_DA_GRAVACAO: Ret = Ret::new(X, REDONDOS_Y - 28.0, L, 18.0);
    /// "Esquecer pareamentos" na câmera pela espera, quando a retomada falhou (a dívida 22): entre o
    /// aviso da espera e a linha da gravação.
    pub const ESQUECER_NA_CAMERA: Ret = Ret::new(CX - 90.0, REDONDOS_Y - 28.0 - 4.0 - 26.0, 180.0, 26.0);
    /// **Os ajustes da câmera** (R9, `docs/controles-de-camera.md` §4.1): um redondo menor, na
    /// fileira dos três, encostado à direita do painel (fora do conjunto centrado, que não muda).
    pub const AJUSTES_DA_CAMERA_L: f32 = 84.0;
    pub const AJUSTES_DA_CAMERA: Ret = Ret::new(DIREITA - AJUSTES_DA_CAMERA_L, REDONDOS_Y, AJUSTES_DA_CAMERA_L, super::ALTURA_DO_REDONDO);

    // --- No ar ---

    pub const ESPELHANDO_PARA: Ret = Ret::new(X, TOPO + 42.0, L, 20.0);
    pub const PAR: Ret = Ret::new(X, TOPO + 62.0, L, 44.0);
    pub fn cartoes_de_numero() -> [Ret; 4] {
        let l = (L - 3.0 * 8.0) / 4.0;
        [0, 1, 2, 3].map(|i| Ret::new(X + i as f32 * (l + 8.0), TOPO + 122.0, l, 66.0))
    }
    pub const RESUMO_NO_AR: Ret = Ret::new(X, TOPO + 202.0, L, 18.0);
    pub const FRASE_DO_PARAR: Ret = Ret::new(X, TOPO + 226.0, L, 20.0);
    pub const FRASE_DO_SOM_NO_AR: Ret = Ret::new(X, TOPO + 250.0, L, 18.0);

    // --- Vários ---

    pub const TITULO_DOS_VARIOS: Ret = Ret::new(X, TOPO + 40.0, L, 36.0);
    pub const LISTA_DOS_VARIOS_Y: f32 = TOPO + 88.0;
    pub const CARTAO_MAIS_UM: Ret = Ret::new(X, PE_Y - 16.0 - 64.0, L, 64.0);
    pub const RODAPE_DOS_VARIOS: Ret = Ret::new(X, PE_Y - 16.0 - 64.0 - 26.0, L, 18.0);
    /// As linhas dos receptores, até 8, da altura que couber (52 no máximo).
    pub fn linhas_dos_varios(n: usize) -> Vec<Ret> {
        let fim = RODAPE_DOS_VARIOS.y - 10.0;
        let a = ((fim - LISTA_DOS_VARIOS_Y) / n.max(1) as f32).min(52.0);
        (0..n).map(|i| Ret::new(X, LISTA_DOS_VARIOS_Y + i as f32 * a, L, a - 6.0)).collect()
    }

    // --- Exibindo ---

    pub const VIDEO_NA_OUTRA_JANELA: Ret = Ret::new(X, TOPO + 84.0, L, 20.0);
    pub const CARTAO_DO_SOM: Ret = Ret::new(X, TOPO + 120.0, L, 62.0);
    pub const SEGMENTADO: Ret = Ret::new(DIREITA - 16.0 - 212.0, TOPO + 120.0 + 15.0, 212.0, 32.0);
    pub fn segmentos() -> [Ret; 4] {
        [0, 1, 2, 3].map(|i| Ret::new(SEGMENTADO.x + 2.0 + i as f32 * 52.0, SEGMENTADO.y + 2.0, 52.0, 28.0))
    }
    pub const MUDO: Ret = Ret::new(SEGMENTADO.x - 12.0 - 100.0, SEGMENTADO.y, 100.0, 32.0);
    pub const LINHA_DO_SOM: Ret = Ret::new(X + 16.0, TOPO + 120.0 + 32.0, MUDO.x - 12.0 - X - 16.0, 18.0);
    /// "Tocar o som mesmo com a câmera do Quall em uso" (§11.5: fica aqui, e não nos Ajustes).
    pub const SOM_COM_CAMERA: Ret = Ret::new(X, TOPO + 192.0, L, 56.0);
    pub const DETALHES: Ret = Ret::new(X, TOPO + 262.0, 130.0, 28.0);
    /// **R9b**: "Ajustes da câmera" de quem filma, ao lado de "Detalhes".
    pub const AJUSTES_DA_CAMERA_REMOTA: Ret = Ret::new(X + 130.0 + 12.0, TOPO + 262.0, 190.0, 28.0);
    pub const BLOCO_DOS_DETALHES: Ret = Ret::new(X, TOPO + 296.0, L, 96.0);
    pub const ENCERRANDO: Ret = Ret::new(X, TOPO + 400.0, L, 18.0);

    /// **A janela "Ajustes da câmera"** (R9, `docs/controles-de-camera.md` §4.2–§4.3): fixa, com a
    /// prévia à esquerda na câmera comum, e só a coluna dos controles na tela R5. Os lugares de
    /// cada linha das abas saem daqui; quem escolhe quais aparecem é `modelo_dos_ajustes`.
    pub mod ajustes {
        use super::super::Ret;

        pub const MARGEM: f32 = 20.0;
        pub const ALTURA: f32 = 500.0;
        pub const COLUNA_L: f32 = 400.0;
        pub const LARGURA_COM_PREVIA: f32 = MARGEM + 400.0 + MARGEM + COLUNA_L + MARGEM;
        pub const LARGURA_SEM_PREVIA: f32 = MARGEM + COLUNA_L + MARGEM;
        /// A prévia (16:9) e as duas linhas do §3.6 embaixo dela.
        pub const PREVIA: Ret = Ret::new(MARGEM, MARGEM, 400.0, 225.0);
        pub const LINHA_LIDA_COM_PREVIA: Ret = Ret::new(MARGEM, MARGEM + 225.0 + 10.0, 400.0, 18.0);
        pub const AVISO_COM_PREVIA: Ret = Ret::new(MARGEM, MARGEM + 225.0 + 34.0, 400.0, 52.0);
        /// Sem prévia, as duas linhas ficam no alto, e a coluna desce.
        pub const LINHA_LIDA_SEM_PREVIA: Ret = Ret::new(MARGEM, 14.0, COLUNA_L, 18.0);
        pub const AVISO_SEM_PREVIA: Ret = Ret::new(MARGEM, 36.0, COLUNA_L, 52.0);

        pub fn largura(com_previa: bool) -> f32 {
            if com_previa {
                LARGURA_COM_PREVIA
            } else {
                LARGURA_SEM_PREVIA
            }
        }

        /// O canto de cima à esquerda da coluna dos controles.
        pub fn coluna(com_previa: bool) -> (f32, f32) {
            if com_previa {
                (MARGEM + 400.0 + MARGEM, MARGEM)
            } else {
                (MARGEM, 100.0)
            }
        }

        pub const ABA_A: f32 = 32.0;
        /// As larguras das quatro abas ("Exposição", "Ganho e obturador", "Balanço", "Foco"). Em
        /// inglês "White balance" é mais longo que "Balanço": a terceira cresce, a segunda e a
        /// quarta cedem (o total é o mesmo, 382).
        pub const ABAS_L: [f32; 4] = [92.0, 136.0, 100.0, 54.0];
        pub const ESPACO_DAS_ABAS: f32 = 6.0;

        pub fn abas(com_previa: bool) -> [Ret; 4] {
            let (x0, y0) = coluna(com_previa);
            let mut x = x0;
            ABAS_L.map(|l| {
                let r = Ret::new(x, y0, l, ABA_A);
                x += l + ESPACO_DAS_ABAS;
                r
            })
        }

        /// Onde o conteúdo da aba começa.
        pub fn inicio(com_previa: bool) -> (f32, f32) {
            let (x, y) = coluna(com_previa);
            (x, y + ABA_A + 18.0)
        }

        pub const OPCAO_A: f32 = 32.0;
        pub const ROTULO_A: f32 = 18.0;
        pub const DESLIZANTE_A: f32 = 30.0;
        pub const INTERRUPTOR_A: f32 = 48.0;
        pub const NOTA_A: f32 = 18.0;
        pub const FRASE_A: f32 = 36.0;
        pub const ESPACO: f32 = 12.0;
        pub const VALOR_L: f32 = 120.0;

        /// `n` opções lado a lado na largura `l`, a partir de `(x, y)`.
        pub fn opcoes(x: f32, y: f32, l: f32, n: usize) -> Vec<Ret> {
            let e = 6.0;
            let w = (l - e * (n as f32 - 1.0)) / n as f32;
            (0..n).map(|i| Ret::new(x + i as f32 * (w + e), y, w, OPCAO_A)).collect()
        }

        /// O "Restaurar automático", no pé da coluna.
        pub fn restaurar(com_previa: bool) -> Ret {
            let (x, _) = coluna(com_previa);
            Ret::new(x, ALTURA - MARGEM - 36.0, 190.0, 36.0)
        }

        /// "Usar meus ajustes" (07/10), ao lado do "Restaurar automático".
        pub fn usar_meus_ajustes(com_previa: bool) -> Ret {
            let (x, _) = coluna(com_previa);
            Ret::new(x + 200.0, ALTURA - MARGEM - 36.0, 190.0, 36.0)
        }

        /// **A faixa do controle remoto** (R9b): "Permitir controle remoto da câmera" e, embaixo,
        /// "Controlado por …". Com a prévia, ela cabe na coluna da prévia, embaixo do aviso; sem
        /// ela (a tela R5), a janela cresce [`FAIXA_DO_REMOTO_A`] no pé.
        pub const FAIXA_DO_REMOTO_A: f32 = 76.0;
        pub const PERMITIR_A: f32 = 48.0;

        /// A altura da janela: a de sempre, mais a faixa do remoto quando ela vai no pé.
        pub fn altura(com_previa: bool, com_faixa: bool) -> f32 {
            if com_faixa && !com_previa {
                ALTURA + FAIXA_DO_REMOTO_A
            } else {
                ALTURA
            }
        }

        pub fn permitir_remoto(com_previa: bool) -> Ret {
            if com_previa {
                Ret::new(MARGEM, AVISO_COM_PREVIA.y + AVISO_COM_PREVIA.a + 12.0, 400.0, PERMITIR_A)
            } else {
                Ret::new(MARGEM, ALTURA, COLUNA_L, PERMITIR_A)
            }
        }

        pub fn controlado_por(com_previa: bool) -> Ret {
            let p = permitir_remoto(com_previa);
            Ret::new(p.x, p.y + p.a + 4.0, p.l, 18.0)
        }
    }
}

// =============================================================================================
// O desenho no Windows: Direct2D e DirectWrite sobre um DC
// =============================================================================================

#[cfg(windows)]
pub mod d2d {
    //! O pintor das listas de [`Item`](super::Item).

    use std::collections::HashMap;

    use windows::core::{Interface, Result, BOOL, PCWSTR};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F};
    use windows::Win32::Graphics::Direct2D::{
        D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush, ID2D1StrokeStyle,
        D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
        D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
        D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    };
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteInlineObject, IDWriteTextFormat,
        IDWriteTextLayout, IDWriteTextLayout1, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
        DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
        DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS,
        DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
        DWRITE_WORD_WRAPPING_WRAP,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Gdi::HDC;
    use windows_numerics::{Matrix3x2, Vector2};

    use super::{
        cores_da_pilula, largura_da_pilula, nomes_da_familia, Alinha, Ancora, Cor, Familia, Fonte, Item, Texto,
        ALTURA_DA_PILULA, F_PILULA,
    };

    /// `D2DERR_RECREATE_TARGET` (o alvo se perdeu). No alvo de software não deveria acontecer; se
    /// acontecer, o próximo desenho refaz o alvo.
    const D2DERR_RECREATE_TARGET: i32 = 0x8899_000Cu32 as i32;
    const LOCALIDADE: PCWSTR = windows::core::w!("pt-br");

    fn cor(c: Cor, opacidade: f32) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: c.r as f32 / 255.0,
            g: c.g as f32 / 255.0,
            b: c.b as f32 / 255.0,
            a: (c.a as f32 / 255.0) * opacidade,
        }
    }

    fn largo(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// O pintor: a fábrica, o alvo sobre DC e os formatos de texto guardados.
    pub struct Pintor {
        d2d: ID2D1Factory,
        dwrite: IDWriteFactory,
        alvo: Option<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)>,
        /// A família de verdade de cada papel (a primeira que o sistema tem).
        familias: HashMap<Familia, String>,
        formatos: HashMap<(Familia, u32, u16), IDWriteTextFormat>,
    }

    impl Pintor {
        pub fn novo() -> Result<Pintor> {
            unsafe {
                let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
                let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
                let mut colecao: Option<IDWriteFontCollection> = None;
                let _ = dwrite.GetSystemFontCollection(&mut colecao, false);
                let mut familias = HashMap::new();
                for f in [Familia::Titulo, Familia::Corpo, Familia::Mono, Familia::Icones] {
                    let nomes = nomes_da_familia(f);
                    let mut escolhida = nomes[nomes.len() - 1].to_string();
                    if let Some(c) = &colecao {
                        for n in nomes {
                            let w: Vec<u16> = n.encode_utf16().chain(std::iter::once(0)).collect();
                            let mut indice = 0u32;
                            let mut existe = BOOL(0);
                            if c.FindFamilyName(PCWSTR(w.as_ptr()), &mut indice, &mut existe).is_ok() && existe.as_bool() {
                                escolhida = n.to_string();
                                break;
                            }
                        }
                    }
                    familias.insert(f, escolhida);
                }
                Ok(Pintor { d2d, dwrite, alvo: None, familias, formatos: HashMap::new() })
            }
        }

        /// O nome da família que vale para o papel (o GDI dos campos usa o mesmo).
        pub fn familia(&self, f: Familia) -> &str {
            self.familias.get(&f).map(|s| s.as_str()).unwrap_or("Segoe UI")
        }

        fn formato(&mut self, f: Fonte) -> Result<IDWriteTextFormat> {
            let chave = (f.familia, (f.tamanho * 10.0).round() as u32, f.peso);
            if let Some(x) = self.formatos.get(&chave) {
                return Ok(x.clone());
            }
            let nome: Vec<u16> = self.familia(f.familia).encode_utf16().chain(std::iter::once(0)).collect();
            let x = unsafe {
                self.dwrite.CreateTextFormat(
                    PCWSTR(nome.as_ptr()),
                    None::<&IDWriteFontCollection>,
                    DWRITE_FONT_WEIGHT(f.peso as i32),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    f.tamanho.max(1.0),
                    LOCALIDADE,
                )?
            };
            self.formatos.insert(chave, x.clone());
            Ok(x)
        }

        fn alvo(&mut self) -> Result<(ID2D1DCRenderTarget, ID2D1SolidColorBrush)> {
            if let Some((rt, p)) = &self.alvo {
                return Ok((rt.clone(), p.clone()));
            }
            unsafe {
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                };
                let rt = self.d2d.CreateDCRenderTarget(&props)?;
                // Tons de cinza: o fundo é opaco, mas o texto em cima de cores com alfa e as
                // opacidades do botão apertado ficam iguais nas duas sessões (a Sessão 0 não tem
                // ClearType).
                rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                let p = rt.CreateSolidColorBrush(&cor(super::TEXTO, 1.0), None)?;
                self.alvo = Some((rt.clone(), p.clone()));
                Ok((rt, p))
            }
        }

        /// **Mede** a largura de um texto de uma linha, em DIP.
        pub fn medir(&mut self, s: &str, f: Fonte) -> f32 {
            let Ok(formato) = self.formato(f) else { return super::largura_estimada(s, f) };
            let w = largo(s);
            unsafe {
                let Ok(l) = self.dwrite.CreateTextLayout(&w, &formato, 100_000.0, 1_000.0) else {
                    return super::largura_estimada(s, f);
                };
                if f.espaco > 0.0 {
                    if let Ok(l1) = l.cast::<IDWriteTextLayout1>() {
                        let _ = l1.SetCharacterSpacing(0.0, f.espaco * f.tamanho, 0.0, DWRITE_TEXT_RANGE { startPosition: 0, length: w.len() as u32 });
                    }
                }
                let mut m = DWRITE_TEXT_METRICS::default();
                if l.GetMetrics(&mut m).is_ok() {
                    m.widthIncludingTrailingWhitespace
                } else {
                    super::largura_estimada(s, f)
                }
            }
        }

        /// **Desenha a lista** no DC, no retângulo `area` (pixels do DC; a origem da lista é o canto
        /// dele), a `dpi` pontos por polegada. `fundo`, se houver, limpa a área antes; `opacidade`
        /// multiplica o alfa de tudo (o botão apertado a 0,9, o desligado a 0,4).
        pub fn desenhar(&mut self, hdc: HDC, area: RECT, dpi: u32, fundo: Option<Cor>, itens: &[Item], opacidade: f32) -> std::result::Result<(), String> {
            match self.desenhar_uma_vez(hdc, area, dpi, fundo, itens, opacidade) {
                Err(e) if e.code().0 == D2DERR_RECREATE_TARGET => {
                    self.alvo = None;
                    self.desenhar_uma_vez(hdc, area, dpi, fundo, itens, opacidade).map_err(|e| format!("{e}"))
                }
                r => r.map_err(|e| format!("{e}")),
            }
        }

        fn desenhar_uma_vez(&mut self, hdc: HDC, area: RECT, dpi: u32, fundo: Option<Cor>, itens: &[Item], opacidade: f32) -> Result<()> {
            // Os layouts de texto saem antes do `BeginDraw` (o mesmo cuidado de `texto.rs`).
            let mut layouts: Vec<Option<(IDWriteTextLayout, Vec<(DWRITE_TEXT_RANGE, Cor)>)>> = Vec::with_capacity(itens.len());
            let mut pilulas: Vec<Option<f32>> = Vec::with_capacity(itens.len());
            for item in itens {
                match item {
                    Item::Texto(t) => {
                        layouts.push(self.layout(t).ok());
                        pilulas.push(None);
                    }
                    Item::Pilula { texto, .. } => {
                        let lt = self.medir(&super::caixa_alta(texto), F_PILULA);
                        let t = Texto {
                            ret: super::Ret::new(0.0, 0.0, lt + 2.0, ALTURA_DA_PILULA),
                            trechos: vec![super::Trecho::simples(super::caixa_alta(texto))],
                            fonte: F_PILULA,
                            cor: super::BRANCO,
                            alinha: Alinha::Esquerda,
                            meio: true,
                            quebra: false,
                        };
                        layouts.push(self.layout(&t).ok());
                        pilulas.push(Some(lt));
                    }
                    Item::Icone { icone, tamanho, ret, .. } => {
                        let t = Texto {
                            ret: *ret,
                            trechos: vec![super::Trecho::simples(icone.glifo().to_string())],
                            fonte: super::fonte(Familia::Icones, *tamanho, super::NORMAL),
                            cor: super::TEXTO,
                            alinha: Alinha::Centro,
                            meio: true,
                            quebra: false,
                        };
                        layouts.push(self.layout(&t).ok());
                        pilulas.push(None);
                    }
                    _ => {
                        layouts.push(None);
                        pilulas.push(None);
                    }
                }
            }
            let (rt, pincel) = self.alvo()?;
            unsafe {
                rt.BindDC(hdc, &area)?;
                rt.SetDpi(dpi as f32, dpi as f32);
                let mut erro: Option<windows::core::Error> = None;
                rt.BeginDraw();
                rt.SetTransform(&Matrix3x2::identity());
                if let Some(f) = fundo {
                    rt.Clear(Some(&cor(f, 1.0)));
                }
                let pinta = |c: Cor| {
                    pincel.SetColor(&cor(c, opacidade));
                };
                for (i, item) in itens.iter().enumerate() {
                    match item {
                        Item::Caixa { ret, raio, fundo, borda } => {
                            let d = D2D_RECT_F { left: ret.x, top: ret.y, right: ret.x + ret.l, bottom: ret.y + ret.a };
                            if fundo.a > 0 {
                                pinta(*fundo);
                                if *raio > 0.0 {
                                    rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: d, radiusX: *raio, radiusY: *raio }, &pincel);
                                } else {
                                    rt.FillRectangle(&d, &pincel);
                                }
                            }
                            if let Some((c, e)) = borda {
                                pinta(*c);
                                let m = e / 2.0;
                                let d2 = D2D_RECT_F { left: d.left + m, top: d.top + m, right: d.right - m, bottom: d.bottom - m };
                                let r2 = (raio - m).max(0.0);
                                rt.DrawRoundedRectangle(
                                    &D2D1_ROUNDED_RECT { rect: d2, radiusX: r2, radiusY: r2 },
                                    &pincel,
                                    *e,
                                    None::<&ID2D1StrokeStyle>,
                                );
                            }
                        }
                        Item::Circulo { cx, cy, raio, cor: c } => {
                            pinta(*c);
                            rt.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: *cx, Y: *cy }, radiusX: *raio, radiusY: *raio }, &pincel);
                        }
                        Item::Anel { cx, cy, raio, espessura, cor: c } => {
                            pinta(*c);
                            rt.DrawEllipse(
                                &D2D1_ELLIPSE { point: Vector2 { X: *cx, Y: *cy }, radiusX: *raio, radiusY: *raio },
                                &pincel,
                                *espessura,
                                None::<&ID2D1StrokeStyle>,
                            );
                        }
                        Item::Linha { x0, y0, x1, y1, espessura, cor: c } => {
                            pinta(*c);
                            rt.DrawLine(Vector2 { X: *x0, Y: *y0 }, Vector2 { X: *x1, Y: *y1 }, &pincel, *espessura, None::<&ID2D1StrokeStyle>);
                        }
                        Item::Texto(t) => {
                            if let Some(Some((l, cores))) = layouts.get(i) {
                                // Sem `?` aqui dentro: sair entre o `BeginDraw` e o `EndDraw` deixaria o
                                // alvo preso para sempre. O erro é guardado e devolvido depois.
                                if let Err(e) = self.pintar_layout(&rt, l, cores, t.cor, t.ret.x, t.ret.y, opacidade) {
                                    erro.get_or_insert(e);
                                }
                            }
                        }
                        Item::Icone { ret, cor: c, .. } => {
                            if let Some(Some((l, _))) = layouts.get(i) {
                                pinta(*c);
                                rt.DrawTextLayout(Vector2 { X: ret.x, Y: ret.y }, l, &pincel, D2D1_DRAW_TEXT_OPTIONS_CLIP);
                            }
                        }
                        Item::Pilula { ancora, y, luz, .. } => {
                            let lt = pilulas.get(i).copied().flatten().unwrap_or(60.0);
                            let largura = largura_da_pilula(lt);
                            let x = match ancora {
                                Ancora::Esquerda(x) => *x,
                                Ancora::Centro(cx) => cx - largura / 2.0,
                            };
                            let (fundo, bolinha, palavra) = cores_da_pilula(*luz);
                            pinta(fundo);
                            let d = D2D_RECT_F { left: x, top: *y, right: x + largura, bottom: y + ALTURA_DA_PILULA };
                            let r = ALTURA_DA_PILULA / 2.0;
                            rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: d, radiusX: r, radiusY: r }, &pincel);
                            pinta(bolinha);
                            rt.FillEllipse(
                                &D2D1_ELLIPSE { point: Vector2 { X: x + 12.0 + 4.0, Y: y + r }, radiusX: 4.0, radiusY: 4.0 },
                                &pincel,
                            );
                            if let Some(Some((l, _))) = layouts.get(i) {
                                pinta(palavra);
                                rt.DrawTextLayout(Vector2 { X: x + 12.0 + 8.0 + 8.0, Y: *y }, l, &pincel, D2D1_DRAW_TEXT_OPTIONS_CLIP);
                            }
                        }
                    }
                }
                let r = rt.EndDraw(None, None);
                if let Err(e) = &r {
                    if e.code().0 == D2DERR_RECREATE_TARGET {
                        self.alvo = None;
                    }
                }
                match erro {
                    Some(e) => Err(e),
                    None => r,
                }
            }
        }

        /// O layout de um texto e as cores de cada trecho (aplicadas no desenho, pelo pincel de
        /// cada faixa).
        fn layout(&mut self, t: &Texto) -> Result<(IDWriteTextLayout, Vec<(DWRITE_TEXT_RANGE, Cor)>)> {
            let formato = self.formato(t.fonte)?;
            let mut w: Vec<u16> = Vec::new();
            let mut faixas = Vec::new();
            for tr in &t.trechos {
                let inicio = w.len() as u32;
                w.extend(tr.texto.encode_utf16());
                faixas.push((DWRITE_TEXT_RANGE { startPosition: inicio, length: w.len() as u32 - inicio }, tr));
            }
            unsafe {
                let l = self.dwrite.CreateTextLayout(&w, &formato, t.ret.l.max(1.0), t.ret.a.max(1.0))?;
                l.SetTextAlignment(match t.alinha {
                    Alinha::Esquerda => DWRITE_TEXT_ALIGNMENT_LEADING,
                    Alinha::Centro => DWRITE_TEXT_ALIGNMENT_CENTER,
                    Alinha::Direita => DWRITE_TEXT_ALIGNMENT_TRAILING,
                })?;
                l.SetParagraphAlignment(if t.meio { DWRITE_PARAGRAPH_ALIGNMENT_CENTER } else { DWRITE_PARAGRAPH_ALIGNMENT_NEAR })?;
                l.SetWordWrapping(if t.quebra { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
                if !t.quebra {
                    let sinal: IDWriteInlineObject = self.dwrite.CreateEllipsisTrimmingSign(&formato)?;
                    let corte = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                    l.SetTrimming(&corte, &sinal)?;
                }
                let mut cores = Vec::new();
                for (faixa, tr) in &faixas {
                    if faixa.length == 0 {
                        continue;
                    }
                    if let Some(p) = tr.peso {
                        l.SetFontWeight(DWRITE_FONT_WEIGHT(p as i32), *faixa)?;
                    }
                    if let Some(f) = tr.familia {
                        let nome: Vec<u16> = self.familia(f).encode_utf16().chain(std::iter::once(0)).collect();
                        l.SetFontFamilyName(PCWSTR(nome.as_ptr()), *faixa)?;
                    }
                    if let Some(s) = tr.tamanho {
                        l.SetFontSize(s, *faixa)?;
                    }
                    if let Some(c) = tr.cor {
                        cores.push((*faixa, c));
                    }
                }
                if t.fonte.espaco > 0.0 {
                    if let Ok(l1) = l.cast::<IDWriteTextLayout1>() {
                        l1.SetCharacterSpacing(0.0, t.fonte.espaco * t.fonte.tamanho, 0.0, DWRITE_TEXT_RANGE { startPosition: 0, length: w.len() as u32 })?;
                    }
                }
                Ok((l, cores))
            }
        }

        /// Um texto: a cor de base no pincel de sempre, e cada trecho com cor própria desenhado de
        /// novo por cima, recortado à caixa dele. Mais simples que um `IDWriteTextRenderer`
        /// nosso, e o custo é um desenho a mais por trecho colorido (dois ou três por tela).
        #[allow(clippy::too_many_arguments)]
        fn pintar_layout(
            &self,
            rt: &ID2D1DCRenderTarget,
            l: &IDWriteTextLayout,
            cores: &[(DWRITE_TEXT_RANGE, Cor)],
            base: Cor,
            x: f32,
            y: f32,
            opacidade: f32,
        ) -> Result<()> {
            unsafe {
                let pincel = rt.CreateSolidColorBrush(&cor(base, opacidade), None)?;
                if cores.is_empty() {
                    rt.DrawTextLayout(Vector2 { X: x, Y: y }, l, &pincel, D2D1_DRAW_TEXT_OPTIONS_CLIP);
                    return Ok(());
                }
                // A cor de cada trecho vai como "efeito de desenho" da faixa: o Direct2D usa o
                // pincel dado ali no lugar do de base.
                let mut guardados = Vec::new();
                for (faixa, c) in cores {
                    let p = rt.CreateSolidColorBrush(&cor(*c, opacidade), None)?;
                    l.SetDrawingEffect(&p.cast::<windows::core::IUnknown>()?, *faixa)?;
                    guardados.push(p);
                }
                rt.DrawTextLayout(Vector2 { X: x, Y: y }, l, &pincel, D2D1_DRAW_TEXT_OPTIONS_CLIP);
                Ok(())
            }
        }
    }
}
