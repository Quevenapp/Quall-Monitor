//! **O desentrelaçador `adapt2`** na CPU: o quadro entrelaçado da câmera (o DV da Panasonic) vira
//! um quadro progressivo, um por um, **antes** do conversor (desenho em
//! `quall-scratch/desentrelacar-windows/desenho.md`).
//!
//! É o `adapt2` da frente do S24 (`tools/espiao-dv-s24/bancada-dv/dv-bancada.c`, modo `ADAPT2` com
//! `-J 3 -P 3 -R 0` e limiar 10, medido em `mede-desentrelacado.py`), portado **bit a bit**:
//!
//! - as linhas do campo **mais novo** ficam como vieram;
//! - as do mais velho são refeitas pixel a pixel: **bob com ELA** (a média das vizinhas do campo
//!   novo na direção de menor diferença entre −1, 0 e +1) onde há movimento temporal (o pixel velho
//!   e as duas vizinhas novas contra o quadro anterior, o máximo das três diferenças, acima de 10)
//!   ou pente (a média, em x±3, do quanto o pixel velho sai do intervalo das vizinhas novas, acima
//!   de 3); **weave** (o pixel velho) no resto;
//! - a decisão é dilatada na vertical: a linha refeita y vai a bob onde y−2, y ou y+2 foi;
//! - o croma segue a máscara do luma: bob (a média simples das vizinhas) se algum dos pixels de luma
//!   que a amostra de croma cobre foi bob. `fator` é quantos pixels de luma uma amostra de croma
//!   cobre: 4 no yuv411p da referência, 2 no YUY2 que o decodificador de DV do Windows entrega;
//! - sem quadro anterior (o primeiro), tudo bob.
//!
//! **Campo de cima primeiro** é o espelho vertical do campo de baixo primeiro: a mesma rotina com as
//! linhas lidas de baixo para cima (a linha lógica y é a física `altura − 1 − y`, com a altura par).
//! Então `cima(x) == espelho(baixo(espelho(x)))`, bit a bit, por construção.
//!
//! O controle que decidiu trazer isto (o bob do processador de vídeo da Intel guarda o campo velho e
//! refaz o outro pela média das vizinhas, meia resolução vertical em todo quadro) está no desenho.
//!
//! Este arquivo não usa nada do Windows nem do resto do crate: o arnês de igualdade contra o C roda
//! no Mac com ele incluído por `#[path]`.

/// Qual campo veio primeiro no tempo. O adapt2 guarda o **outro** (o mais novo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ordem {
    /// O DV: o campo de baixo (as linhas ímpares, contando de 0) primeiro; ficam as pares.
    CampoDeBaixoPrimeiro,
    /// O campo de cima (as pares) primeiro; ficam as ímpares.
    CampoDeCimaPrimeiro,
}

/// O limiar do movimento temporal (em códigos de luma).
pub const LIMIAR_DE_MOVIMENTO: i32 = 10;
/// O limiar da média do pente na janela.
pub const LIMIAR_DE_PENTE: i32 = 3;
/// A meia janela do pente (x±3).
pub const JANELA: usize = 3;

/// O que um quadro deu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resultado {
    /// Pixels de luma das linhas refeitas que foram por interpolação (bob).
    pub pix_bob: u64,
    /// Pixels de luma das linhas refeitas.
    pub pix_total: u64,
    /// O quadro tinha anterior (senão, foi todo bob).
    pub tinha_anterior: bool,
}

impl Resultado {
    pub fn bob_por_mil(&self) -> f64 {
        if self.pix_total == 0 {
            0.0
        } else {
            1000.0 * self.pix_bob as f64 / self.pix_total as f64
        }
    }
}

/// Três planos, compactos (passo = largura do plano).
#[derive(Clone, Default)]
struct Planos {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Planos {
    fn novo(w: usize, h: usize, cw: usize) -> Planos {
        Planos { y: vec![0; w * h], u: vec![0; cw * h], v: vec![0; cw * h] }
    }
}

/// O desentrelaçador de uma captura: guarda o quadro cru anterior. Um por sessão de câmera.
pub struct Desentrelacador {
    largura: usize,
    altura: usize,
    fator: usize,
    ordem: Ordem,
    atual: Planos,
    anterior: Planos,
    saida: Planos,
    tem_anterior: bool,
    mascara: Vec<u8>,
    acum: Vec<i32>,
    /// Acumulados desde a criação.
    pub quadros: u64,
    pub pix_bob: u64,
    pub pix_total: u64,
}

impl Desentrelacador {
    /// `None` se a geometria não serve: altura ímpar ou menor que 4, largura que o `fator` não
    /// divide, ou `fator` fora de 1..=4.
    pub fn novo(largura: usize, altura: usize, fator: usize, ordem: Ordem) -> Option<Desentrelacador> {
        if altura < 4 || altura % 2 != 0 || largura < 2 || !(1..=4).contains(&fator) || largura % fator != 0 {
            return None;
        }
        let cw = largura / fator;
        Some(Desentrelacador {
            largura,
            altura,
            fator,
            ordem,
            atual: Planos::novo(largura, altura, cw),
            anterior: Planos::novo(largura, altura, cw),
            saida: Planos::novo(largura, altura, cw),
            tem_anterior: false,
            mascara: vec![0; largura * altura],
            acum: vec![0; largura + 1],
            quadros: 0,
            pix_bob: 0,
            pix_total: 0,
        })
    }

    /// Para o YUY2 (`fator` 2).
    pub fn novo_yuy2(largura: usize, altura: usize, ordem: Ordem) -> Option<Desentrelacador> {
        if largura % 2 != 0 {
            return None;
        }
        Self::novo(largura, altura, 2, ordem)
    }

    pub fn largura(&self) -> usize {
        self.largura
    }

    pub fn altura(&self) -> usize {
        self.altura
    }

    pub fn ordem(&self) -> Ordem {
        self.ordem
    }

    /// Esquece o quadro anterior: o próximo sai todo bob.
    pub fn esquecer(&mut self) {
        self.tem_anterior = false;
    }

    /// Um quadro YUY2 (`Y0 U Y1 V`), `entrada` com passo `passo_entrada` bytes, desentrelaçado em
    /// `saida` com passo `passo_saida`. Os dois têm de caber (`altura` linhas de `2·largura` bytes).
    pub fn yuy2(&mut self, entrada: &[u8], passo_entrada: usize, saida: &mut [u8], passo_saida: usize) -> Resultado {
        let (w, h) = (self.largura, self.altura);
        let lb = 2 * w;
        assert!(self.fator == 2, "yuy2 pede o fator 2");
        assert!(passo_entrada >= lb && entrada.len() >= passo_entrada * (h - 1) + lb, "entrada YUY2 curta");
        assert!(passo_saida >= lb && saida.len() >= passo_saida * (h - 1) + lb, "saída YUY2 curta");
        let cw = w / 2;
        for yy in 0..h {
            let l = &entrada[yy * passo_entrada..yy * passo_entrada + lb];
            let (py, pu, pv) = (&mut self.atual.y[yy * w..(yy + 1) * w], &mut self.atual.u[yy * cw..(yy + 1) * cw], &mut self.atual.v[yy * cw..(yy + 1) * cw]);
            for (i, q) in l.chunks_exact(4).enumerate() {
                py[2 * i] = q[0];
                pu[i] = q[1];
                py[2 * i + 1] = q[2];
                pv[i] = q[3];
            }
        }
        let r = self.processar();
        for yy in 0..h {
            let o = &mut saida[yy * passo_saida..yy * passo_saida + lb];
            let (py, pu, pv) = (&self.saida.y[yy * w..(yy + 1) * w], &self.saida.u[yy * cw..(yy + 1) * cw], &self.saida.v[yy * cw..(yy + 1) * cw]);
            for (i, q) in o.chunks_exact_mut(4).enumerate() {
                q[0] = py[2 * i];
                q[1] = pu[i];
                q[2] = py[2 * i + 1];
                q[3] = pv[i];
            }
        }
        r
    }

    /// Um quadro em três planos (Y com `passo_y`; U e V de `largura / fator` com `passo_c`).
    /// Devolve o resultado; os planos de saída ficam em [`Desentrelacador::saida_planar`].
    #[allow(clippy::too_many_arguments)]
    pub fn planar(&mut self, y: &[u8], passo_y: usize, u: &[u8], v: &[u8], passo_c: usize) -> Resultado {
        let (w, h, cw) = (self.largura, self.altura, self.largura / self.fator);
        for yy in 0..h {
            self.atual.y[yy * w..(yy + 1) * w].copy_from_slice(&y[yy * passo_y..yy * passo_y + w]);
            self.atual.u[yy * cw..(yy + 1) * cw].copy_from_slice(&u[yy * passo_c..yy * passo_c + cw]);
            self.atual.v[yy * cw..(yy + 1) * cw].copy_from_slice(&v[yy * passo_c..yy * passo_c + cw]);
        }
        self.processar()
    }

    /// Os planos do último quadro desentrelaçado (compactos: Y `largura`, U e V `largura / fator`).
    pub fn saida_planar(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.saida.y, &self.saida.u, &self.saida.v)
    }

    /// O corpo: `atual` (e `anterior`, se houver) → `saida`; depois `atual` vira o `anterior`.
    fn processar(&mut self) -> Resultado {
        let (w, h, k) = (self.largura, self.altura, self.fator);
        let cw = w / k;
        // A linha lógica y é a física `fis(y)`: no campo de cima primeiro, o espelho vertical.
        let espelho = self.ordem == Ordem::CampoDeCimaPrimeiro;
        let fis = |y: usize| if espelho { h - 1 - y } else { y };
        let adapt = self.tem_anterior;
        let cur = &self.atual;
        let ant = &self.anterior;
        let m = &mut self.mascara;
        let acum = &mut self.acum;
        // A faixa da linha lógica y num plano compacto de `largura`.
        let linha = |y: usize, largura: usize| fis(y) * largura..fis(y) * largura + largura;

        // 1) a decisão crua por pixel das linhas lógicas ímpares (as refeitas)
        let mut yl = 1;
        while yl < h {
            let mr = yl * w..(yl + 1) * w;
            if !adapt {
                m[mr].fill(1);
                yl += 2;
                continue;
            }
            // Fatias de comprimento `w` exato: o compilador tira as conferências de índice do laço.
            let l = &cur.y[linha(yl, w)];
            let a = &cur.y[linha(yl - 1, w)];
            let b = if yl + 1 < h { &cur.y[linha(yl + 1, w)] } else { a };
            let pl = &ant.y[linha(yl, w)];
            let pa = &ant.y[linha(yl - 1, w)];
            let pb = if yl + 1 < h { &ant.y[linha(yl + 1, w)] } else { pa };
            let acum = &mut acum[..w + 1];
            acum[0] = 0;
            for x in 0..w {
                acum[x + 1] = acum[x] + pente(l[x] as i32, a[x] as i32, b[x] as i32);
            }
            let mm = &mut m[mr];
            for x in 0..w {
                let mov = (l[x] as i32 - pl[x] as i32).abs().max((a[x] as i32 - pa[x] as i32).abs()).max((b[x] as i32 - pb[x] as i32).abs());
                let mut d = mov > LIMIAR_DE_MOVIMENTO;
                if !d {
                    let x0 = x.saturating_sub(JANELA);
                    let x1 = (x + JANELA + 1).min(w);
                    d = acum[x1] - acum[x0] > LIMIAR_DE_PENTE * (x1 - x0) as i32;
                }
                mm[x] = d as u8;
            }
            yl += 2;
        }
        // 2) a dilatação vertical (só com anterior, como a referência): a linha lógica par da
        // máscara é o rascunho (a cópia da decisão da ímpar seguinte), e a ímpar y vira o OU dos
        // rascunhos de y−2, y e y+2.
        if adapt {
            let mut y = 1;
            while y < h {
                m.copy_within(y * w..(y + 1) * w, (y - 1) * w);
                y += 2;
            }
            let mut y = 1;
            while y < h {
                let r0 = (y - 1) * w;
                let rm = if y >= 3 { (y - 3) * w } else { r0 };
                let rp = if y + 2 < h { (y + 1) * w } else { r0 };
                for x in 0..w {
                    m[y * w + x] = m[r0 + x] | m[rm + x] | m[rp + x];
                }
                y += 2;
            }
        }
        // 3) o luma
        let mut pix_bob = 0u64;
        let mut pix_total = 0u64;
        {
            let out = &mut self.saida.y;
            for y in 0..h {
                let (fo, lo) = (fis(y) * w, fis(y) * w + w);
                if y & 1 == 0 {
                    out[fo..lo].copy_from_slice(&cur.y[fo..lo]);
                    continue;
                }
                let l = &cur.y[fo..lo];
                let a = &cur.y[linha(y - 1, w)];
                let b = if y + 1 < h { &cur.y[linha(y + 1, w)] } else { a };
                let mm = &m[y * w..(y + 1) * w];
                let o = &mut out[fo..lo];
                pix_total += w as u64;
                let n = mm.iter().filter(|v| **v != 0).count();
                pix_bob += n as u64;
                if n == 0 {
                    o.copy_from_slice(l);
                    continue;
                }
                // As pontas pelo ELA com a conferência de borda; o miolo sem ela.
                o[0] = if mm[0] != 0 { ela(a, b, 0, w) } else { l[0] };
                o[w - 1] = if mm[w - 1] != 0 { ela(a, b, w - 1, w) } else { l[w - 1] };
                for x in 1..w - 1 {
                    o[x] = if mm[x] != 0 { ela_miolo(a, b, x) } else { l[x] };
                }
            }
        }
        // 4) o croma: segue a máscara do luma (bob se algum dos `k` pixels de luma foi bob)
        for (entra, sai) in [(&cur.u, &mut self.saida.u), (&cur.v, &mut self.saida.v)] {
            for y in 0..h {
                let (fo, lo) = (fis(y) * cw, fis(y) * cw + cw);
                if y & 1 == 0 {
                    sai[fo..lo].copy_from_slice(&entra[fo..lo]);
                    continue;
                }
                let a = &entra[linha(y - 1, cw)];
                let b = if y + 1 < h { &entra[linha(y + 1, cw)] } else { a };
                let l = &entra[fo..lo];
                let mm = &m[y * w..(y + 1) * w];
                let o = &mut sai[fo..lo];
                for x in 0..cw {
                    let bob = mm[k * x..k * x + k].iter().any(|v| *v != 0);
                    o[x] = if bob { ((a[x] as u32 + b[x] as u32 + 1) >> 1) as u8 } else { l[x] };
                }
            }
        }
        // o cru deste quadro é o anterior do próximo
        std::mem::swap(&mut self.atual, &mut self.anterior);
        self.tem_anterior = true;
        self.quadros += 1;
        self.pix_bob += pix_bob;
        self.pix_total += pix_total;
        Resultado { pix_bob, pix_total, tinha_anterior: adapt }
    }
}

/// Quanto o pixel do campo velho sai do intervalo das duas vizinhas do campo novo (0 se dentro).
#[inline]
fn pente(v: i32, a: i32, b: i32) -> i32 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    if v > hi {
        v - hi
    } else if v < lo {
        lo - v
    } else {
        0
    }
}

/// [`ela`] fora das pontas (`0 < x < w − 1`).
#[inline(always)]
fn ela_miolo(a: &[u8], b: &[u8], x: usize) -> u8 {
    let (ax, bx) = (a[x] as i32, b[x] as i32);
    let (a0, a2, b0, b2) = (a[x - 1] as i32, a[x + 1] as i32, b[x - 1] as i32, b[x + 1] as i32);
    let mut melhor = (ax - bx).abs();
    let mut r = (ax + bx + 1) >> 1;
    let d1 = (a0 - b2).abs();
    let d2 = (a2 - b0).abs();
    if d1 < melhor {
        melhor = d1;
        r = (a0 + b2 + 1) >> 1;
    }
    if d2 < melhor {
        r = (a2 + b0 + 1) >> 1;
    }
    r as u8
}

/// A interpolação com ELA, na ordem de desempate da referência: a vertical, depois (x−1, x+1) se
/// estritamente melhor, depois (x+1, x−1) se estritamente melhor que a melhor até ali.
#[inline]
fn ela(a: &[u8], b: &[u8], x: usize, w: usize) -> u8 {
    let (ax, bx) = (a[x] as i32, b[x] as i32);
    let mut melhor = (ax - bx).abs();
    let mut r = (ax + bx + 1) >> 1;
    if x > 0 && x < w - 1 {
        let d1 = (a[x - 1] as i32 - b[x + 1] as i32).abs();
        let d2 = (a[x + 1] as i32 - b[x - 1] as i32).abs();
        if d1 < melhor {
            melhor = d1;
            r = (a[x - 1] as i32 + b[x + 1] as i32 + 1) >> 1;
        }
        if d2 < melhor {
            r = (a[x + 1] as i32 + b[x - 1] as i32 + 1) >> 1;
        }
    }
    r as u8
}

/// A sequência sintética determinística dos testes (e do arnês contra o C, que grava o FNV-1a
/// da saída da referência em C para ela): bordas diagonais que andam, uma faixa parada com
/// detalhe fino, baixo contraste andando, e ruído de um xorshift. yuv411p 720×480, `n` quadros.
/// Não é do produto: serve ao teste de igualdade e ao arnês que roda a referência em C.
#[doc(hidden)]
pub fn sequencia_sintetica_411(n: usize) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let mut s: u32 = 0x1234_5678;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let (w, h) = (720usize, 480usize);
    let cw = w / 4;
    (0..n)
        .map(|q| {
            let mut y = vec![0u8; w * h];
            let mut u = vec![0u8; cw * h];
            let mut v = vec![0u8; cw * h];
            for yy in 0..h {
                // o campo (a linha ímpar é o campo de baixo, 1/60 s antes do de cima)
                let t = 2 * q as i64 + if yy % 2 == 1 { 0 } else { 1 };
                for x in 0..w {
                    let xi = x as i64;
                    let yi = yy as i64;
                    let mut val: i64 = if yy < 120 {
                        // parado, detalhe fino: xadrez de 3 colunas por 4 linhas
                        if (xi / 3 + yi / 4) % 2 == 0 { 60 } else { 190 }
                    } else if (xi + 2 * yi - 7 * t).rem_euclid(160) < 80 {
                        // diagonal que anda
                        200
                    } else {
                        40
                    };
                    if yy >= 360 {
                        // baixo contraste andando (o pente fraco)
                        val = if (xi - 3 * t).rem_euclid(50) < 25 { 90 } else { 96 };
                    }
                    val += (rnd() % 7) as i64 - 3;
                    y[yy * w + x] = val.clamp(16, 235) as u8;
                }
                for x in 0..cw {
                    u[yy * cw + x] = (100 + (x + yy + q) % 50) as u8;
                    v[yy * cw + x] = (150 - (x * 2 + q) % 60) as u8;
                }
            }
            (y, u, v)
        })
        .collect()
}

// =============================================================================================
// Testes
// =============================================================================================

#[cfg(test)]
mod testes {
    use super::*;

    const W: usize = 720;
    const H: usize = 480;

    fn fnv(dados: &[&[u8]]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for d in dados {
            for b in d.iter() {
                h = (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }

    /// **Bit a bit contra a referência em C** (`dv-bancada.c`, `-d adapt2 -J 3 -P 3 -R 0`, limiar 10)
    /// sobre a sequência sintética: o FNV-1a dos três planos de cada quadro, gravado pelo arnês do
    /// Mac (`quall-scratch/desentrelacar-windows/arnes`) rodando o C sobre a mesma sequência.
    #[test]
    fn igual_a_referencia_em_c_na_sequencia_sintetica() {
        let esperado: [u64; 8] = GOLDEN_C;
        let seq = sequencia_sintetica_411(esperado.len());
        let mut d = Desentrelacador::novo(W, H, 4, Ordem::CampoDeBaixoPrimeiro).unwrap();
        for (q, (y, u, v)) in seq.iter().enumerate() {
            d.planar(y, W, u, v, W / 4);
            let (sy, su, sv) = d.saida_planar();
            assert_eq!(fnv(&[sy, su, sv]), esperado[q], "quadro {q}");
        }
    }

    /// O FNV-1a da saída da referência em C na `sequencia_sintetica_411(8)`, quadro a quadro (os três
    /// planos em sequência), gravado pelo arnês do Mac (`arnes/compara.sh`, 22/09) com o `dv-bancada.c`
    /// de SHA-1 `b96228a6`. O mesmo arnês deu igualdade byte a byte nas cinco amostras DV da fase A.
    const GOLDEN_C: [u64; 8] = [
        0xd5e3f28cbc1114cd,
        0xced0bce31c5bbef9,
        0x87d6c293d7d22fa9,
        0xf8e7ec6c9a6ff62c,
        0x642ce0120c22bfdc,
        0xf6bfdcc50da442d8,
        0x1e1a276ac5369917,
        0xd42a24c2df72756b,
    ];

    fn espelhar(p: &[u8], largura: usize) -> Vec<u8> {
        p.chunks_exact(largura).rev().flatten().copied().collect()
    }

    #[test]
    fn campo_de_cima_primeiro_e_o_espelho() {
        let seq = sequencia_sintetica_411(4);
        let mut baixo = Desentrelacador::novo(W, H, 4, Ordem::CampoDeBaixoPrimeiro).unwrap();
        let mut cima = Desentrelacador::novo(W, H, 4, Ordem::CampoDeCimaPrimeiro).unwrap();
        let cw = W / 4;
        for (y, u, v) in &seq {
            let (ey, eu, ev) = (espelhar(y, W), espelhar(u, cw), espelhar(v, cw));
            let rb = baixo.planar(&ey, W, &eu, &ev, cw);
            let rc = cima.planar(y, W, u, v, cw);
            assert_eq!(rb, rc);
            let (by, bu, bv) = baixo.saida_planar();
            let (cy, cu, cv) = cima.saida_planar();
            assert_eq!(espelhar(by, W), cy);
            assert_eq!(espelhar(bu, cw), cu);
            assert_eq!(espelhar(bv, cw), cv);
        }
    }

    /// No campo de cima primeiro ficam as linhas **ímpares** (o campo de baixo, o mais novo) e são
    /// refeitas as pares; no de baixo primeiro, o contrário. Com movimento (anterior diferente),
    /// algumas linhas refeitas mudam.
    #[test]
    fn cada_ordem_guarda_o_campo_mais_novo() {
        let seq = sequencia_sintetica_411(3);
        for (ordem, guardada) in [(Ordem::CampoDeBaixoPrimeiro, 0usize), (Ordem::CampoDeCimaPrimeiro, 1usize)] {
            let mut d = Desentrelacador::novo(W, H, 4, ordem).unwrap();
            for (y, u, v) in &seq {
                d.planar(y, W, u, v, W / 4);
                let (sy, _, _) = d.saida_planar();
                for yy in (guardada..H).step_by(2) {
                    assert_eq!(&sy[yy * W..(yy + 1) * W], &y[yy * W..(yy + 1) * W], "{ordem:?}: a linha {yy} é do campo guardado");
                }
                let refeitas_mudadas = (1 - guardada..H).step_by(2).filter(|yy| sy[yy * W..(yy + 1) * W] != y[yy * W..(yy + 1) * W]).count();
                assert!(refeitas_mudadas > 0, "{ordem:?}: as linhas do outro campo são refeitas");
            }
        }
    }

    /// O YUY2 é a rotina planar com fator 2: desempacota, desentrelaça, empacota.
    #[test]
    fn yuy2_igual_ao_planar_com_fator_2() {
        let seq = sequencia_sintetica_411(4);
        let cw = W / 2;
        let mut p = Desentrelacador::novo(W, H, 2, Ordem::CampoDeBaixoPrimeiro).unwrap();
        let mut e = Desentrelacador::novo_yuy2(W, H, Ordem::CampoDeBaixoPrimeiro).unwrap();
        let passo = 2 * W + 64; // passo maior que a linha, dos dois lados
        for (y, u4, v4) in &seq {
            // croma 4:2:2 a partir do 4:1:1 (duplicado), com uma perturbação para não ser simétrico
            let u: Vec<u8> = (0..cw * H).map(|i| u4[(i / cw) * (W / 4) + (i % cw) / 2].wrapping_add((i % 3) as u8)).collect();
            let v: Vec<u8> = (0..cw * H).map(|i| v4[(i / cw) * (W / 4) + (i % cw) / 2].wrapping_sub((i % 5) as u8)).collect();
            let mut ent = vec![0xAAu8; passo * H];
            for yy in 0..H {
                for i in 0..cw {
                    let o = yy * passo + 4 * i;
                    ent[o] = y[yy * W + 2 * i];
                    ent[o + 1] = u[yy * cw + i];
                    ent[o + 2] = y[yy * W + 2 * i + 1];
                    ent[o + 3] = v[yy * cw + i];
                }
            }
            let mut sai = vec![0x55u8; passo * H];
            let re = e.yuy2(&ent, passo, &mut sai, passo);
            let rp = p.planar(y, W, &u, &v, cw);
            assert_eq!(re, rp);
            let (py, pu, pv) = p.saida_planar();
            for yy in 0..H {
                for i in 0..cw {
                    let o = yy * passo + 4 * i;
                    assert_eq!(
                        [sai[o], sai[o + 1], sai[o + 2], sai[o + 3]],
                        [py[yy * W + 2 * i], pu[yy * cw + i], py[yy * W + 2 * i + 1], pv[yy * cw + i]],
                        "linha {yy}, par {i}"
                    );
                }
                // o que passa da linha no passo de saída não é tocado
                assert!(sai[yy * passo + 2 * W..(yy + 1) * passo].iter().all(|b| *b == 0x55));
            }
        }
    }

    #[test]
    fn o_primeiro_quadro_e_todo_bob_e_o_parado_e_weave() {
        let seq = sequencia_sintetica_411(1);
        let (y, u, v) = &seq[0];
        let mut d = Desentrelacador::novo(W, H, 4, Ordem::CampoDeBaixoPrimeiro).unwrap();
        let r = d.planar(y, W, u, v, W / 4);
        assert!(!r.tinha_anterior);
        assert_eq!(r.pix_bob, r.pix_total);
        // o mesmo quadro de novo: sem movimento; a faixa parada de cima (linhas 8..112), sem pente
        // fora do ruído, sai weave: as linhas refeitas iguais às de entrada em quase tudo
        let r2 = d.planar(y, W, u, v, W / 4);
        assert!(r2.tinha_anterior);
        let (sy, _, _) = d.saida_planar();
        let iguais = (9..112).step_by(2).flat_map(|yy| (0..W).map(move |x| (yy, x))).filter(|&(yy, x)| sy[yy * W + x] == y[yy * W + x]).count();
        let total = (9..112).step_by(2).count() * W;
        assert!(iguais * 10 > total * 9, "weave no parado: {iguais} de {total}");
        // as linhas guardadas (pares) nunca mudam
        for yy in (0..H).step_by(2) {
            assert_eq!(&sy[yy * W..(yy + 1) * W], &y[yy * W..(yy + 1) * W]);
        }
    }

    #[test]
    fn geometria_recusada() {
        assert!(Desentrelacador::novo(720, 481, 2, Ordem::CampoDeBaixoPrimeiro).is_none());
        assert!(Desentrelacador::novo(721, 480, 2, Ordem::CampoDeBaixoPrimeiro).is_none());
        assert!(Desentrelacador::novo(720, 2, 2, Ordem::CampoDeBaixoPrimeiro).is_none());
        assert!(Desentrelacador::novo(720, 480, 5, Ordem::CampoDeBaixoPrimeiro).is_none());
        assert!(Desentrelacador::novo_yuy2(720, 576, Ordem::CampoDeCimaPrimeiro).is_some());
    }
}
