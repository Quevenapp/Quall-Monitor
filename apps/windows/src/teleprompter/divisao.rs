//! **A divisão da tela R5** (`docs/teleprompter-com-camera.md` §2.5 e §8.10): o texto do lado da
//! lente, a prévia da câmera do outro, a borda arrastável entre os dois (20 % no mínimo de cada
//! lado), e a faixa dos estados e da barra **do lado longe da lente** — nada fica entre o texto e a
//! lente (o pedido do Bruno depois da prova do iPhone X, §8.5 e §8.6).
//!
//! Aritmética pura, sem Win32: os testes rodam no portão e em qualquer máquina (`rustc --test` sobre
//! este arquivo sozinho). A tela converte [`Ret`] em `RECT`.
//!
//! **O texto é fração da janela inteira, e só a prévia absorve a faixa** (a regra do iOS, §8.3.1): a
//! faixa que cresce (um aviso a mais) encolhe a prévia e nunca move a linha de leitura.

/// De que lado da janela o texto fica. O padrão é **em cima**: a webcam do notebook fica na borda de
/// cima, e uma webcam externa em cima do monitor também (§2.5). O ajuste local gira entre os quatro.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LadoDoTexto {
    #[default]
    Cima,
    Baixo,
    Esquerda,
    Direita,
}

impl LadoDoTexto {
    /// O seguinte no botão "Lado do texto".
    pub fn proximo(self) -> LadoDoTexto {
        match self {
            LadoDoTexto::Cima => LadoDoTexto::Baixo,
            LadoDoTexto::Baixo => LadoDoTexto::Esquerda,
            LadoDoTexto::Esquerda => LadoDoTexto::Direita,
            LadoDoTexto::Direita => LadoDoTexto::Cima,
        }
    }

    /// O nome no arquivo de ajustes.
    pub fn chave(self) -> &'static str {
        match self {
            LadoDoTexto::Cima => "cima",
            LadoDoTexto::Baixo => "baixo",
            LadoDoTexto::Esquerda => "esquerda",
            LadoDoTexto::Direita => "direita",
        }
    }

    pub fn da_chave(s: &str) -> Option<LadoDoTexto> {
        match s {
            "cima" => Some(LadoDoTexto::Cima),
            "baixo" => Some(LadoDoTexto::Baixo),
            "esquerda" => Some(LadoDoTexto::Esquerda),
            "direita" => Some(LadoDoTexto::Direita),
            _ => None,
        }
    }

    /// O rótulo do botão.
    pub fn rotulo(self) -> &'static str {
        match self {
            LadoDoTexto::Cima => "Texto: em cima", // i18n: chave (a tela traduz ao mostrar)
            LadoDoTexto::Baixo => "Texto: embaixo", // i18n: chave (a tela traduz ao mostrar)
            LadoDoTexto::Esquerda => "Texto: à esquerda", // i18n: chave (a tela traduz ao mostrar)
            LadoDoTexto::Direita => "Texto: à direita", // i18n: chave (a tela traduz ao mostrar)
        }
    }

    /// Lado a lado (a borda é vertical)?
    pub fn lado_a_lado(self) -> bool {
        matches!(self, LadoDoTexto::Esquerda | LadoDoTexto::Direita)
    }

    /// As marcas do enquadramento vão ao pé do texto (a lente está em cima dele)? Só com o texto
    /// embaixo a lente fica no pé, e então as marcas ficam no alto, como no prompter comum.
    pub fn marcas_no_pe(self) -> bool {
        self != LadoDoTexto::Baixo
    }
}

/// Um retângulo em pixels de cliente: `[esquerda, direita) × [topo, base)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Ret {
    pub esquerda: i32,
    pub topo: i32,
    pub direita: i32,
    pub base: i32,
}

impl Ret {
    pub fn novo(esquerda: i32, topo: i32, direita: i32, base: i32) -> Ret {
        Ret { esquerda, topo, direita: direita.max(esquerda), base: base.max(topo) }
    }
    pub fn largura(&self) -> i32 {
        self.direita - self.esquerda
    }
    pub fn altura(&self) -> i32 {
        self.base - self.topo
    }
    pub fn contem(&self, x: i32, y: i32) -> bool {
        x >= self.esquerda && x < self.direita && y >= self.topo && y < self.base
    }
    pub fn vazio(&self) -> bool {
        self.largura() <= 0 || self.altura() <= 0
    }
}

/// A divisão calculada.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Divisao {
    pub texto: Ret,
    pub previa: Ret,
    /// Os estados, os avisos e a barra: do lado longe da lente.
    pub faixa: Ret,
    /// A borda arrastável entre o texto e a prévia (para o cursor e o arrasto).
    pub borda: Ret,
}

/// O mínimo de cada lado (§2.5, hipótese do desenho): o texto e a prévia nunca abaixo de 20 %.
pub const FRACAO_MINIMA: f64 = 0.2;
/// O padrão: 50/50.
pub const FRACAO_PADRAO: f64 = 0.5;

/// A fração do texto que cabe: pelo menos 20 %, e deixando pelo menos 20 % para a prévia depois da
/// faixa (quando a faixa divide a dimensão com a prévia). Numa janela pequena demais, 20 %.
pub fn limitar(fracao: f64, dimensao: i32, faixa_na_mesma_dimensao: i32) -> f64 {
    let f = if fracao.is_finite() { fracao } else { FRACAO_PADRAO };
    let d = f64::from(dimensao.max(1));
    let maximo = (1.0 - FRACAO_MINIMA - f64::from(faixa_na_mesma_dimensao.max(0)) / d).max(FRACAO_MINIMA);
    f.clamp(FRACAO_MINIMA, maximo)
}

/// **A divisão.** `faixa` é a altura da faixa dos estados e da barra; `pega` é a espessura da borda
/// arrastável.
pub fn dividir(cliente: Ret, lado: LadoDoTexto, fracao: f64, faixa: i32, pega: i32) -> Divisao {
    let (l, t, r, b) = (cliente.esquerda, cliente.topo, cliente.direita, cliente.base);
    let faixa = faixa.clamp(0, cliente.altura().max(0));
    let meia = (pega / 2).max(1);
    match lado {
        LadoDoTexto::Cima | LadoDoTexto::Baixo => {
            let h = cliente.altura();
            let f = limitar(fracao, h, faixa);
            let ht = (f * f64::from(h)).round() as i32;
            if lado == LadoDoTexto::Cima {
                let texto = Ret::novo(l, t, r, t + ht);
                let faixa_r = Ret::novo(l, b - faixa, r, b);
                let previa = Ret::novo(l, texto.base, r, faixa_r.topo);
                let borda = Ret::novo(l, texto.base - meia, r, texto.base + meia);
                Divisao { texto, previa, faixa: faixa_r, borda }
            } else {
                let texto = Ret::novo(l, b - ht, r, b);
                let faixa_r = Ret::novo(l, t, r, t + faixa);
                let previa = Ret::novo(l, faixa_r.base, r, texto.topo);
                let borda = Ret::novo(l, texto.topo - meia, r, texto.topo + meia);
                Divisao { texto, previa, faixa: faixa_r, borda }
            }
        }
        LadoDoTexto::Esquerda | LadoDoTexto::Direita => {
            let w = cliente.largura();
            let f = limitar(fracao, w, 0);
            let wt = (f * f64::from(w)).round() as i32;
            let (texto, coluna) = if lado == LadoDoTexto::Esquerda {
                (Ret::novo(l, t, l + wt, b), Ret::novo(l + wt, t, r, b))
            } else {
                (Ret::novo(r - wt, t, r, b), Ret::novo(l, t, r - wt, b))
            };
            let faixa_r = Ret::novo(coluna.esquerda, b - faixa, coluna.direita, b);
            let previa = Ret::novo(coluna.esquerda, t, coluna.direita, faixa_r.topo);
            let x = if lado == LadoDoTexto::Esquerda { texto.direita } else { texto.esquerda };
            let borda = Ret::novo(x - meia, t, x + meia, faixa_r.topo);
            Divisao { texto, previa, faixa: faixa_r, borda }
        }
    }
}

/// A fração do texto para um ponto do arrasto da borda (limitada como em [`dividir`]).
pub fn fracao_do_arrasto(cliente: Ret, lado: LadoDoTexto, faixa: i32, x: i32, y: i32) -> f64 {
    match lado {
        LadoDoTexto::Cima => limitar(f64::from(y - cliente.topo) / f64::from(cliente.altura().max(1)), cliente.altura(), faixa),
        LadoDoTexto::Baixo => limitar(f64::from(cliente.base - y) / f64::from(cliente.altura().max(1)), cliente.altura(), faixa),
        LadoDoTexto::Esquerda => limitar(f64::from(x - cliente.esquerda) / f64::from(cliente.largura().max(1)), cliente.largura(), 0),
        LadoDoTexto::Direita => limitar(f64::from(cliente.direita - x) / f64::from(cliente.largura().max(1)), cliente.largura(), 0),
    }
}

/// A prévia encaixada (sem cortar e sem deformar) num retângulo: o quadro inteiro da câmera, com
/// faixas pretas (a rede encaixa igual; a prévia mostra o mesmo enquadramento, a lição do Android,
/// §8.6 defeito 1).
pub fn encaixar(largura: u32, altura: u32, dentro: Ret) -> Ret {
    if largura == 0 || altura == 0 || dentro.vazio() {
        return dentro;
    }
    let (w, h) = (f64::from(dentro.largura()), f64::from(dentro.altura()));
    let a = f64::from(largura) / f64::from(altura);
    let (ew, eh) = if w / h > a { (h * a, h) } else { (w, w / a) };
    let x = dentro.esquerda + ((w - ew) / 2.0).round() as i32;
    let y = dentro.topo + ((h - eh) / 2.0).round() as i32;
    Ret::novo(x, y, x + ew.round() as i32, y + eh.round() as i32)
}

#[cfg(test)]
mod testes {
    use super::*;

    const LADOS: [LadoDoTexto; 4] = [LadoDoTexto::Cima, LadoDoTexto::Baixo, LadoDoTexto::Esquerda, LadoDoTexto::Direita];

    fn intersecta(a: Ret, b: Ret) -> bool {
        a.esquerda < b.direita && b.esquerda < a.direita && a.topo < b.base && b.topo < a.base
    }

    #[test]
    fn o_padrao_e_texto_em_cima_meio_a_meio() {
        let c = Ret::novo(0, 0, 1280, 720);
        let d = dividir(c, LadoDoTexto::default(), FRACAO_PADRAO, 100, 8);
        assert_eq!(d.texto, Ret::novo(0, 0, 1280, 360));
        assert_eq!(d.faixa, Ret::novo(0, 620, 1280, 720));
        assert_eq!(d.previa, Ret::novo(0, 360, 1280, 620), "só a prévia absorve a faixa");
    }

    /// **Nada entre o texto e a lente, nem sobre as primeiras linhas do lado dela**: numa varredura
    /// de lados, frações, faixas e janelas, o texto encosta na borda da lente, e a faixa e a prévia
    /// nunca o cobrem.
    #[test]
    fn nada_entre_o_texto_e_a_lente() {
        for lado in LADOS {
            for &(w, h) in &[(1280, 720), (720, 1280), (400, 300), (1920, 1080)] {
                for faixa in [0, 60, 140] {
                    for f in [0.0, 0.1, 0.2, 0.5, 0.8, 0.95, f64::NAN] {
                        let c = Ret::novo(0, 0, w, h);
                        let d = dividir(c, lado, f, faixa, 8);
                        assert!(!intersecta(d.faixa, d.texto), "{lado:?} {w}x{h} faixa={faixa} f={f}: a faixa cobre o texto");
                        assert!(!intersecta(d.previa, d.texto), "{lado:?}: a prévia cobre o texto");
                        match lado {
                            LadoDoTexto::Cima | LadoDoTexto::Esquerda | LadoDoTexto::Direita => {
                                assert_eq!(d.texto.topo, 0, "{lado:?}: o texto encosta na borda de cima (a lente)")
                            }
                            LadoDoTexto::Baixo => assert_eq!(d.texto.base, h),
                        }
                        // lado a lado, a faixa fica só na coluna da prévia
                        if lado.lado_a_lado() {
                            assert!(d.faixa.esquerda >= d.previa.esquerda && d.faixa.direita <= d.previa.direita);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn o_minimo_de_vinte_por_cento() {
        let c = Ret::novo(0, 0, 1000, 1000);
        for lado in LADOS {
            let d = dividir(c, lado, 0.05, 100, 8);
            let (t, dim) = if lado.lado_a_lado() { (d.texto.largura(), 1000) } else { (d.texto.altura(), 1000) };
            assert_eq!(t, dim / 5, "{lado:?}: o texto não fica abaixo de 20 %");
            let d = dividir(c, lado, 0.99, 100, 8);
            let p = if lado.lado_a_lado() { d.previa.largura() } else { d.previa.altura() };
            assert!(p >= 200, "{lado:?}: a prévia não fica abaixo de 20 % ({p})");
        }
    }

    #[test]
    fn o_arrasto_da_borda() {
        let c = Ret::novo(0, 0, 1000, 800);
        assert!((fracao_do_arrasto(c, LadoDoTexto::Cima, 100, 0, 400) - 0.5).abs() < 1e-9);
        assert!((fracao_do_arrasto(c, LadoDoTexto::Baixo, 100, 0, 600) - 0.25).abs() < 1e-9);
        assert_eq!(fracao_do_arrasto(c, LadoDoTexto::Cima, 100, 0, 10), FRACAO_MINIMA);
        assert!((fracao_do_arrasto(c, LadoDoTexto::Direita, 100, 700, 0) - 0.3).abs() < 1e-9);
        // a borda fica onde o arrasto a deixou
        for lado in LADOS {
            let d = dividir(c, lado, 0.4, 100, 8);
            let (x, y) = ((d.borda.esquerda + d.borda.direita) / 2, (d.borda.topo + d.borda.base) / 2);
            let f = fracao_do_arrasto(c, lado, 100, x, y);
            assert!((f - 0.4).abs() < 0.01, "{lado:?}: {f}");
        }
    }

    #[test]
    fn o_lado_gira_e_volta_do_arquivo() {
        let mut l = LadoDoTexto::Cima;
        for _ in 0..4 {
            assert_eq!(LadoDoTexto::da_chave(l.chave()), Some(l));
            l = l.proximo();
        }
        assert_eq!(l, LadoDoTexto::Cima);
        assert!(LadoDoTexto::Cima.marcas_no_pe() && !LadoDoTexto::Baixo.marcas_no_pe());
    }

    #[test]
    fn a_previa_encaixa_o_quadro_inteiro() {
        let r = encaixar(1920, 1080, Ret::novo(0, 360, 1280, 620));
        assert_eq!(r.altura(), 260);
        assert!((f64::from(r.largura()) / f64::from(r.altura()) - 16.0 / 9.0).abs() < 0.02);
        assert_eq!((r.esquerda + r.direita) / 2, 640, "centrada");
        let r = encaixar(640, 480, Ret::novo(0, 0, 400, 1000));
        assert_eq!(r.largura(), 400);
        assert_eq!(r.altura(), 300);
    }
}
