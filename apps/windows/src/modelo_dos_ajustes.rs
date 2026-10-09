//! **A janela "Ajustes da câmera" sem Win32** (R9, `docs/controles-de-camera.md` §4.2 e §4.3), no
//! molde de `modelo_da_janela.rs`: o estado em valores simples (o painel que a thread dos ajustes
//! publica, a aba escolhida, com ou sem prévia), e a composição em peças de `estilo.rs` e em lugares
//! de controle de `estilo::lugar::ajustes`. A janela (`janela_dos_ajustes.rs`) só executa: põe os
//! controles nativos onde o quadro diz, desenha cada botão pela [`aparencia`] no `NM_CUSTOMDRAW`, e
//! transforma cada clique numa [`Acao`] pelo [`gesto`].
//!
//! Os testes rodam em qualquer máquina: cada aba, em cada estado de exemplo, cabe na janela sem
//! rolar e sem um controle cobrir outro, e os textos são os do §3.5.

use crate::estilo::lugar::ajustes as lugar;
use crate::estilo::*;
use crate::idioma::t;
use crate::modelo_da_janela::{Aparencia, EstadoDoControle};
use crate::modelo_dos_ajustes_remotos::{self as remotos, EstadoRemoto, GestoRemoto};
use crate::regras_dos_controles::{
    self as regras, faixa_do_kelvin, faixa_do_obturador, Aba, Acao, AntiCintilacao, Balanco, Faixa, FaseDosAjustes, Foco, ModoDeExposicao,
    PainelDosAjustes, Propriedade,
};

/// Os controles nativos da janela.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ControleDosAjustes {
    /// Uma das quatro abas (grupo de opções).
    Aba(usize),
    /// "Auto" (0) e "Manual" (1) da exposição.
    Exposicao(usize),
    Brilho,
    TravarExposicao,
    /// Auto, 50 Hz, 60 Hz, Desligada.
    Cintilacao(usize),
    PassarParaManual,
    Ganho,
    Obturador,
    /// A grade 2 × 3 do balanço, na ordem de `Balanco::GRADE`.
    Balanco(usize),
    Kelvin,
    TravarBalanco,
    /// Auto, Travado, Manual.
    Foco(usize),
    FocoPosicao,
    Restaurar,
    /// "Usar meus ajustes" (07/10): a câmera abre no automático e lembra o último manual.
    UsarMeusAjustes,
    /// "Permitir controle remoto da câmera" (R9b), só no aparelho que filma.
    PermitirRemoto,
}

/// Que controle nativo cada um é.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EspecieDosAjustes {
    /// `BS_PUSHBUTTON`, desenhado no `NM_CUSTOMDRAW`.
    Botao,
    /// `BS_AUTOCHECKBOX | BS_PUSHLIKE`.
    Alternar,
    /// `BS_AUTORADIOBUTTON | BS_PUSHLIKE`, num grupo.
    Opcao,
    /// `TRACKBAR_CLASS`, horizontal, em degraus.
    Deslizante,
}

impl ControleDosAjustes {
    pub fn id(self) -> usize {
        match self {
            ControleDosAjustes::Aba(i) => 300 + i,
            ControleDosAjustes::Exposicao(i) => 310 + i,
            ControleDosAjustes::Brilho => 320,
            ControleDosAjustes::TravarExposicao => 321,
            ControleDosAjustes::Cintilacao(i) => 330 + i,
            ControleDosAjustes::PassarParaManual => 340,
            ControleDosAjustes::Ganho => 341,
            ControleDosAjustes::Obturador => 342,
            ControleDosAjustes::Balanco(i) => 350 + i,
            ControleDosAjustes::Kelvin => 360,
            ControleDosAjustes::TravarBalanco => 361,
            ControleDosAjustes::Foco(i) => 370 + i,
            ControleDosAjustes::FocoPosicao => 380,
            ControleDosAjustes::Restaurar => 390,
            ControleDosAjustes::UsarMeusAjustes => 391,
            ControleDosAjustes::PermitirRemoto => 395,
        }
    }

    pub fn de_id(id: usize) -> Option<ControleDosAjustes> {
        Some(match id {
            300..=303 => ControleDosAjustes::Aba(id - 300),
            310..=311 => ControleDosAjustes::Exposicao(id - 310),
            320 => ControleDosAjustes::Brilho,
            321 => ControleDosAjustes::TravarExposicao,
            330..=333 => ControleDosAjustes::Cintilacao(id - 330),
            340 => ControleDosAjustes::PassarParaManual,
            341 => ControleDosAjustes::Ganho,
            342 => ControleDosAjustes::Obturador,
            350..=355 => ControleDosAjustes::Balanco(id - 350),
            360 => ControleDosAjustes::Kelvin,
            361 => ControleDosAjustes::TravarBalanco,
            370..=372 => ControleDosAjustes::Foco(id - 370),
            380 => ControleDosAjustes::FocoPosicao,
            390 => ControleDosAjustes::Restaurar,
            391 => ControleDosAjustes::UsarMeusAjustes,
            395 => ControleDosAjustes::PermitirRemoto,
            _ => return None,
        })
    }

    pub fn especie(self) -> EspecieDosAjustes {
        use ControleDosAjustes as C;
        match self {
            C::Aba(_) | C::Exposicao(_) | C::Cintilacao(_) | C::Balanco(_) | C::Foco(_) => EspecieDosAjustes::Opcao,
            C::TravarExposicao | C::TravarBalanco | C::PermitirRemoto => EspecieDosAjustes::Alternar,
            C::Brilho | C::Ganho | C::Obturador | C::Kelvin | C::FocoPosicao => EspecieDosAjustes::Deslizante,
            C::PassarParaManual | C::Restaurar | C::UsarMeusAjustes => EspecieDosAjustes::Botao,
        }
    }

    /// O primeiro de um grupo de opções (leva `WS_GROUP`).
    pub fn abre_grupo(self) -> bool {
        use ControleDosAjustes as C;
        match self {
            C::Aba(i) | C::Exposicao(i) | C::Cintilacao(i) | C::Balanco(i) | C::Foco(i) => i == 0,
            _ => true,
        }
    }

    /// Todos, na ordem de criação (a do Tab).
    pub fn todos() -> Vec<ControleDosAjustes> {
        use ControleDosAjustes as C;
        let mut v: Vec<C> = (0..4).map(C::Aba).collect();
        v.extend((0..2).map(C::Exposicao));
        v.extend([C::Brilho, C::TravarExposicao]);
        v.extend((0..4).map(C::Cintilacao));
        v.extend([C::PassarParaManual, C::Ganho, C::Obturador]);
        v.extend((0..6).map(C::Balanco));
        v.extend([C::Kelvin, C::TravarBalanco]);
        v.extend((0..3).map(C::Foco));
        v.extend([C::FocoPosicao, C::Restaurar, C::UsarMeusAjustes, C::PermitirRemoto]);
        v
    }
}

/// O estado da janela: a aba, o que a thread dos ajustes publicou, e se há prévia.
#[derive(Clone, Debug, Default)]
pub struct EstadoDosAjustes {
    pub aba: Aba,
    pub painel: PainelDosAjustes,
    pub com_previa: bool,
    /// **R9b, no aparelho que filma**: a opção "Permitir controle remoto da câmera" (`None`: a
    /// janela não a mostra, como na sonda sem rede).
    pub permitir: Option<bool>,
    /// **R9b, no aparelho que recebe**: o painel é o da câmera do outro lado
    /// (`modelo_dos_ajustes_remotos`), sem prévia e sem a opção.
    pub remoto: Option<EstadoRemoto>,
}

/// Um deslizante em degraus: `0..=max`, na posição `pos`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Degraus {
    pub max: i32,
    pub pos: i32,
}

/// Um controle visível: onde, se está habilitado e marcado, e os degraus de um deslizante.
#[derive(Clone, Debug, PartialEq)]
pub struct ControleNaTela {
    pub c: ControleDosAjustes,
    pub ret: Ret,
    pub habilitado: bool,
    pub marcado: bool,
    pub degraus: Option<Degraus>,
}

/// O que pintar, onde fica cada controle visível, e onde fica a prévia.
#[derive(Clone, Debug, Default)]
pub struct QuadroDosAjustes {
    pub itens: Vec<Item>,
    pub controles: Vec<ControleNaTela>,
    pub previa: Option<Ret>,
}

impl QuadroDosAjustes {
    pub fn achar(&self, c: ControleDosAjustes) -> Option<&ControleNaTela> {
        self.controles.iter().find(|x| x.c == c)
    }
    pub fn lugar(&self, c: ControleDosAjustes) -> Option<Ret> {
        self.achar(c).map(|x| x.ret)
    }
    pub(crate) fn controle(&mut self, c: ControleDosAjustes, ret: Ret, habilitado: bool, marcado: bool) {
        self.controles.push(ControleNaTela { c, ret, habilitado, marcado, degraus: None });
    }
    pub(crate) fn deslizante(&mut self, c: ControleDosAjustes, ret: Ret, habilitado: bool, degraus: Degraus) {
        self.controles.push(ControleNaTela { c, ret, habilitado, marcado: false, degraus: Some(degraus) });
    }
    pub(crate) fn texto(&mut self, r: Ret, s: impl Into<String>, f: Fonte, cor: Cor) {
        self.itens.push(texto(r, s, f, cor).meio().item());
    }
}

/// O tamanho da área de cliente, em DIP: o remoto é sem prévia e sem a faixa da opção.
pub fn tamanho(e: &EstadoDosAjustes) -> (f32, f32) {
    if e.remoto.is_some() {
        return (lugar::largura(false), lugar::ALTURA);
    }
    (lugar::largura(e.com_previa), lugar::altura(e.com_previa, e.permitir.is_some()))
}

// =============================================================================================
// Os valores na escala de cada deslizante
// =============================================================================================

fn faixa(e: &EstadoDosAjustes, p: Propriedade) -> Option<Faixa> {
    e.painel.caps.get(&p).copied()
}

fn lido(e: &EstadoDosAjustes, p: Propriedade) -> Option<i32> {
    e.painel.lidos.get(&p).map(|l| l.valor)
}

/// O brilho que a tela mostra: o do registro (padrão + deslocamento) ou o lido.
fn brilho(e: &EstadoDosAjustes, f: &Faixa) -> i32 {
    let r = &e.painel.registro;
    if r.ev.round() as i64 != 0 {
        f.cortar(i64::from(f.padrao) + r.ev.round() as i64)
    } else {
        f.cortar(i64::from(lido(e, Propriedade::Brilho).unwrap_or(f.padrao)))
    }
}

fn ganho(e: &EstadoDosAjustes, f: &Faixa) -> i32 {
    f.cortar(e.painel.registro.iso.map(|g| g.round() as i64).or(lido(e, Propriedade::Ganho).map(i64::from)).unwrap_or(i64::from(f.padrao)))
}

/// O obturador **aplicado** (cortado pelo teto do fps de agora); o guardado fica intacto (§3.1).
fn obturador(e: &EstadoDosAjustes, f: &Faixa) -> i32 {
    let v = e.painel.registro.obturador_ns.map(regras::log2_dos_ns).or(lido(e, Propriedade::Exposicao)).unwrap_or(f.padrao);
    f.cortar(i64::from(v))
}

fn kelvin(e: &EstadoDosAjustes, f: &Faixa) -> i32 {
    f.cortar(i64::from(e.painel.registro.kelvin.or(lido(e, Propriedade::Balanco)).unwrap_or(f.padrao)))
}

fn foco_posicao(e: &EstadoDosAjustes) -> f64 {
    match (e.painel.registro.foco_posicao, faixa(e, Propriedade::Foco), lido(e, Propriedade::Foco)) {
        (Some(x), _, _) => x,
        (None, Some(f), Some(v)) => f.fracao(i64::from(v)),
        _ => 0.0,
    }
}

// =============================================================================================
// A composição
// =============================================================================================

/// **Compõe a janela** no estado dado.
pub fn compor(e: &EstadoDosAjustes) -> QuadroDosAjustes {
    use ControleDosAjustes as C;
    if let Some(r) = &e.remoto {
        return remotos::compor(e.aba, r);
    }
    let mut q = QuadroDosAjustes::default();
    let (largura, altura) = tamanho(e);
    q.itens.push(caixa(Ret::new(0.0, 0.0, largura, altura), 0.0, FUNDO));
    let p = &e.painel;
    let pronto = p.fase == FaseDosAjustes::Pronto;
    // A prévia e as duas linhas do §3.6.
    let (linha_r, aviso_r) = if e.com_previa {
        q.previa = Some(lugar::PREVIA);
        q.itens.push(caixa(lugar::PREVIA, RAIO_DO_CARTAO, Cor::rgb(0x000000)));
        (lugar::LINHA_LIDA_COM_PREVIA, lugar::AVISO_COM_PREVIA)
    } else {
        (lugar::LINHA_LIDA_SEM_PREVIA, lugar::AVISO_SEM_PREVIA)
    };
    let linha = match p.fase {
        FaseDosAjustes::Lendo => t("Lendo a câmera…").to_string(),
        _ => p.linha_lida.clone(),
    };
    if !linha.is_empty() {
        q.itens.push(texto(linha_r, linha, F_MONO_12, TEXTO2).meio().item());
    }
    // Uma linha de aviso só: o modo compartilhado, depois a divergência (o pedido da pessoa não
    // vingou), e por último a pouca luz (§3.1), que é informação e não falha: vai no tom neutro.
    let aviso_ = match p.fase {
        FaseDosAjustes::Compartilhada => Some((Tom::Ambar, t(regras::FRASE_OUTRO_APP).to_string())),
        _ => p.divergencia.clone().map(|d| (Tom::Ambar, d)).or_else(|| p.pouca_luz.map(|l| (Tom::Info, l.texto()))),
    };
    if let Some((tom, t)) = aviso_ {
        let a = altura_do_aviso(&t, aviso_r.l).min(aviso_r.a);
        q.itens.extend(aviso(Ret::new(aviso_r.x, aviso_r.y, aviso_r.l, a), tom, &t));
    }
    // As abas.
    for (i, r) in lugar::abas(e.com_previa).iter().enumerate() {
        q.controle(C::Aba(i), *r, true, e.aba.indice() == i);
    }
    let (x, mut y) = lugar::inicio(e.com_previa);
    let l = lugar::COLUNA_L;
    let r = &p.registro;
    let nota = |q: &mut QuadroDosAjustes, y: f32, t: &str| {
        q.texto(Ret::new(x, y, l, lugar::NOTA_A), t.to_string(), F_LEGENDA, TEXTO3);
    };
    // O rótulo de um deslizante, com o valor à direita.
    let rotulo_e_valor = |q: &mut QuadroDosAjustes, y: f32, rotulo_: &str, valor: &str| {
        q.texto(Ret::new(x, y, l - lugar::VALOR_L, lugar::ROTULO_A), rotulo_.to_string(), F_CORPO_FORTE, TEXTO);
        q.itens.push(texto(Ret::new(x + l - lugar::VALOR_L, y, lugar::VALOR_L, lugar::ROTULO_A), valor.to_string(), F_MONO_13, TEXTO2).direita().meio().item());
    };
    match e.aba {
        Aba::Exposicao => {
            let fx = faixa(e, Propriedade::Exposicao);
            let ops = lugar::opcoes(x, y, 220.0, 2);
            q.controle(C::Exposicao(0), ops[0], pronto && fx.is_some(), r.exposicao == ModoDeExposicao::Auto);
            q.controle(C::Exposicao(1), ops[1], pronto && fx.is_some_and(|f| f.tem_manual()), r.exposicao == ModoDeExposicao::Manual);
            y += lugar::OPCAO_A + lugar::ESPACO;
            // O brilho (§3.3: no Windows, no lugar do EV).
            match faixa(e, Propriedade::Brilho) {
                Some(f) => {
                    let v = brilho(e, &f);
                    rotulo_e_valor(&mut q, y, t("Brilho"), &v.to_string());
                    let livre = r.exposicao == ModoDeExposicao::Auto && !r.trava_exposicao;
                    q.deslizante(C::Brilho, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), pronto && livre, Degraus { max: f.passos(), pos: f.indice(i64::from(v)) });
                    if r.trava_exposicao {
                        nota(&mut q, y + lugar::ROTULO_A + 4.0 + lugar::DESLIZANTE_A + 2.0, t(regras::FRASE_DESTRAVE));
                    }
                }
                None => {
                    rotulo_e_valor(&mut q, y, t("Brilho"), "");
                    nota(&mut q, y + lugar::ROTULO_A + 4.0, &regras::frase_sem_controle(Propriedade::Brilho.nome()));
                }
            }
            y += lugar::ROTULO_A + 4.0 + lugar::DESLIZANTE_A + 2.0 + lugar::NOTA_A + 8.0;
            // A trava (só com Auto).
            let r_trava = Ret::new(x, y, l, lugar::INTERRUPTOR_A);
            q.controle(C::TravarExposicao, r_trava, pronto && fx.is_some_and(|f| f.tem_manual()) && r.exposicao == ModoDeExposicao::Auto, r.trava_exposicao);
            if fx.is_none_or(|f| !f.tem_manual()) {
                nota(&mut q, y + lugar::INTERRUPTOR_A + 2.0, &regras::frase_sem_controle("a trava de exposição")); // i18n: chave
            }
            y += lugar::INTERRUPTOR_A + 2.0 + lugar::NOTA_A + 8.0;
            // A anti-cintilação.
            q.texto(Ret::new(x, y, l, lugar::ROTULO_A), t("Anti-cintilação"), F_CORPO_FORTE, TEXTO);
            y += lugar::ROTULO_A + 4.0;
            let fc = faixa(e, Propriedade::AntiCintilacao);
            for (i, (ret, a)) in lugar::opcoes(x, y, l, 4).into_iter().zip(AntiCintilacao::TODAS).enumerate() {
                q.controle(C::Cintilacao(i), ret, pronto && fc.is_some_and(|f| a.oferecida(&f)), r.anti_cintilacao == a);
            }
            if fc.is_none() {
                nota(&mut q, y + lugar::OPCAO_A + 2.0, &regras::frase_sem_controle(Propriedade::AntiCintilacao.nome()));
            }
        }
        Aba::GanhoEObturador => {
            let (fg, fo) = (faixa(e, Propriedade::Ganho), faixa(e, Propriedade::Exposicao).filter(|f| f.tem_manual()));
            if fg.is_none() && fo.is_none() {
                // §3.5: um grupo em que nada se aplica mostra uma linha só.
                nota(&mut q, y, &regras::frase_sem_controle("o ganho e o obturador")); // i18n: chave
            } else if r.exposicao == ModoDeExposicao::Auto {
                q.itens.push(texto(Ret::new(x, y, l, lugar::FRASE_A), t(regras::FRASE_PASSE_PARA_MANUAL), F_LEGENDA_13, TEXTO2).quebra().item());
                y += lugar::FRASE_A + lugar::ESPACO;
                q.controle(C::PassarParaManual, Ret::new(x, y, 190.0, 36.0), pronto && fo.is_some(), false);
            } else {
                match fg {
                    Some(f) => {
                        let v = ganho(e, &f);
                        rotulo_e_valor(&mut q, y, t("Ganho"), &v.to_string());
                        q.deslizante(C::Ganho, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), pronto, Degraus { max: f.passos(), pos: f.indice(i64::from(v)) });
                    }
                    None => {
                        rotulo_e_valor(&mut q, y, t("Ganho"), "");
                        nota(&mut q, y + lugar::ROTULO_A + 4.0, &regras::frase_sem_controle(Propriedade::Ganho.nome()));
                    }
                }
                y += lugar::ROTULO_A + 4.0 + lugar::DESLIZANTE_A + 2.0 + lugar::NOTA_A + 8.0;
                match fo {
                    Some(f) => {
                        let f = faixa_do_obturador(&f, p.fps);
                        let v = obturador(e, &f);
                        rotulo_e_valor(&mut q, y, t("Obturador"), &regras::texto_do_obturador(v));
                        q.deslizante(C::Obturador, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), pronto, Degraus { max: f.passos(), pos: f.indice(i64::from(v)) });
                    }
                    None => {
                        rotulo_e_valor(&mut q, y, t("Obturador"), "");
                        nota(&mut q, y + lugar::ROTULO_A + 4.0, &regras::frase_sem_controle(Propriedade::Exposicao.nome()));
                    }
                }
            }
        }
        Aba::Balanco => match faixa(e, Propriedade::Balanco) {
            None => nota(&mut q, y, &regras::frase_sem_controle(Propriedade::Balanco.nome())),
            Some(f) => {
                let w = (l - 12.0) / 3.0;
                for (i, b) in Balanco::GRADE.iter().enumerate() {
                    let ret = Ret::new(x + (i % 3) as f32 * (w + 6.0), y + (i / 3) as f32 * (lugar::OPCAO_A + 6.0), w, lugar::OPCAO_A);
                    let habilitado = match b {
                        Balanco::Auto => true,
                        Balanco::Kelvin => f.tem_manual(),
                        _ => false,
                    };
                    q.controle(C::Balanco(i), ret, pronto && habilitado, r.balanco == *b);
                }
                y += 2.0 * lugar::OPCAO_A + 6.0 + 2.0;
                // Os presets não existem no Windows (§1), e a grade os mostra apagados com a linha.
                nota(&mut q, y, &regras::frase_sem_controle("os presets de balanço")); // i18n: chave
                y += lugar::NOTA_A + lugar::ESPACO;
                if r.balanco == Balanco::Kelvin {
                    let fk = faixa_do_kelvin(&f);
                    let v = kelvin(e, &fk);
                    rotulo_e_valor(&mut q, y, t("Kelvin"), &format!("{v} K"));
                    q.deslizante(C::Kelvin, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), pronto, Degraus { max: fk.passos(), pos: fk.indice(i64::from(v)) });
                } else {
                    // Com Kelvin a trava de balanço não se aplica e some (§3.4).
                    q.controle(C::TravarBalanco, Ret::new(x, y, l, lugar::INTERRUPTOR_A), pronto && f.tem_manual() && r.balanco == Balanco::Auto, r.trava_balanco);
                    if !f.tem_manual() {
                        nota(&mut q, y + lugar::INTERRUPTOR_A + 2.0, &regras::frase_sem_controle("a trava de balanço")); // i18n: chave
                    }
                }
            }
        },
        Aba::Foco => match faixa(e, Propriedade::Foco) {
            None => nota(&mut q, y, t(regras::FRASE_FOCO_FIXO)),
            Some(f) => {
                for (i, (ret, x_)) in lugar::opcoes(x, y, 300.0, 3).into_iter().zip(Foco::TODOS).enumerate() {
                    let habilitado = x_ == Foco::Auto || f.tem_manual();
                    q.controle(C::Foco(i), ret, pronto && habilitado, r.foco == x_);
                }
                y += lugar::OPCAO_A + 2.0;
                if !f.tem_manual() {
                    nota(&mut q, y, &regras::frase_sem_controle(Propriedade::Foco.nome()));
                }
                y += lugar::NOTA_A + lugar::ESPACO;
                if r.foco == Foco::Manual {
                    let pos = foco_posicao(e);
                    rotulo_e_valor(&mut q, y, t("Perto ↔ Longe"), &regras::com_virgula(pos, 2));
                    q.deslizante(C::FocoPosicao, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), pronto, Degraus { max: 100, pos: (pos * 100.0).round() as i32 });
                }
            }
        },
    }
    q.controle(C::Restaurar, lugar::restaurar(e.com_previa), pronto, false);
    // Ao lado, só quando há "meus ajustes" diferentes do que vale (07/10).
    if regras::oferece_meus_ajustes(p.meus_ajustes.as_ref(), &p.registro) {
        q.controle(C::UsarMeusAjustes, lugar::usar_meus_ajustes(e.com_previa), pronto, false);
    }
    // **R9b**: a opção vale para o app (e não para esta câmera), e fica viva mesmo lendo ou no
    // modo compartilhado; embaixo, quem mexeu de longe.
    if let Some(permite) = e.permitir {
        q.controle(C::PermitirRemoto, lugar::permitir_remoto(e.com_previa), true, permite);
        if let Some(nome) = &p.controlado_por {
            q.texto(lugar::controlado_por(e.com_previa), crate::idioma::tf("Controlado por {}", &[nome]), F_LEGENDA_13, AGUARDANDO_TEXTO);
        }
    }
    q
}

// =============================================================================================
// Os gestos
// =============================================================================================

/// **O gesto de um controle**: a aba que ele abre (a janela troca sozinha), ou a [`Acao`] sobre o
/// registro. Um deslizante leva o degrau escolhido.
pub enum Gesto {
    Aba(Aba),
    Acao(Acao),
    /// Ligar ou desligar "Permitir controle remoto da câmera" (R9b).
    Permitir(bool),
    /// No aparelho que recebe: o pedido à câmera do outro lado.
    Remoto(GestoRemoto),
}

pub fn gesto(c: ControleDosAjustes, e: &EstadoDosAjustes, degrau: Option<i32>) -> Option<Gesto> {
    use ControleDosAjustes as C;
    if let Some(r) = &e.remoto {
        return match remotos::gesto(c, r, degrau)? {
            GestoRemoto::Aba(a) => Some(Gesto::Aba(a)),
            g => Some(Gesto::Remoto(g)),
        };
    }
    let p = &e.painel;
    let d = |f: Faixa| f.do_indice(i64::from(degrau.unwrap_or(0)));
    Some(match c {
        C::Aba(i) => Gesto::Aba(*Aba::TODAS.get(i)?),
        C::Exposicao(0) => Gesto::Acao(Acao::Exposicao(ModoDeExposicao::Auto)),
        C::Exposicao(_) => Gesto::Acao(Acao::Exposicao(ModoDeExposicao::Manual)),
        // "O Manual leva à aba ISO e obturador" (§4.3): quem chama troca a aba depois.
        C::PassarParaManual => Gesto::Acao(Acao::Exposicao(ModoDeExposicao::Manual)),
        C::Brilho => Gesto::Acao(Acao::Brilho(d(faixa(e, Propriedade::Brilho)?))),
        C::TravarExposicao => Gesto::Acao(Acao::TravarExposicao(!p.registro.trava_exposicao)),
        C::Cintilacao(i) => Gesto::Acao(Acao::AntiCintilacao(*AntiCintilacao::TODAS.get(i)?)),
        C::Ganho => Gesto::Acao(Acao::Ganho(d(faixa(e, Propriedade::Ganho)?))),
        C::Obturador => Gesto::Acao(Acao::Obturador(d(faixa_do_obturador(&faixa(e, Propriedade::Exposicao)?, p.fps)))),
        C::Balanco(i) => Gesto::Acao(Acao::Balanco(*Balanco::GRADE.get(i)?)),
        C::Kelvin => Gesto::Acao(Acao::Kelvin(d(faixa_do_kelvin(&faixa(e, Propriedade::Balanco)?)))),
        C::TravarBalanco => Gesto::Acao(Acao::TravarBalanco(!p.registro.trava_balanco)),
        C::Foco(i) => Gesto::Acao(Acao::Foco(*Foco::TODOS.get(i)?)),
        C::FocoPosicao => Gesto::Acao(Acao::FocoPosicao(f64::from(degrau.unwrap_or(0)) / 100.0)),
        C::Restaurar => Gesto::Acao(Acao::Restaurar),
        C::UsarMeusAjustes => Gesto::Acao(Acao::UsarMeusAjustes),
        C::PermitirRemoto => Gesto::Permitir(!e.permitir?),
    })
}

// =============================================================================================
// A aparência e os nomes
// =============================================================================================

/// O rótulo de cada controle, no idioma de agora (os de `regras_dos_controles` são as chaves em
/// português). A janela o reescreve quando o idioma muda. No remoto, a segunda aba e os deslizantes
/// do EV e do ISO levam o nome da câmera do outro lado.
pub fn rotulo(c: ControleDosAjustes, e: &EstadoDosAjustes) -> String {
    use ControleDosAjustes as C;
    if let Some(r) = &e.remoto {
        if let C::Aba(i) = c {
            if let Some(a) = Aba::TODAS.get(i) {
                return t(remotos::rotulo_da_aba(r, *a)).to_string();
            }
        }
        if let Some(s) = remotos::rotulo_do_deslizante(c, r) {
            return t(s).to_string();
        }
    }
    let s: &'static str = match c {
        C::Aba(i) => Aba::TODAS.get(i).map(|a| a.rotulo()).unwrap_or_default(),
        C::Exposicao(0) => "Auto", // i18n: chave
        C::Exposicao(_) => "Manual", // i18n: chave
        C::Brilho => "Brilho", // i18n: chave
        C::TravarExposicao => "Travar exposição", // i18n: chave
        C::Cintilacao(i) => AntiCintilacao::TODAS.get(i).map(|a| a.rotulo()).unwrap_or_default(),
        C::PassarParaManual => regras::PASSAR_PARA_MANUAL,
        C::Ganho => "Ganho", // i18n: chave
        C::Obturador => "Obturador", // i18n: chave
        C::Balanco(i) => Balanco::GRADE.get(i).map(|b| b.rotulo()).unwrap_or_default(),
        C::Kelvin => "Kelvin", // i18n: chave
        C::TravarBalanco => "Travar balanço", // i18n: chave
        C::Foco(i) => Foco::TODOS.get(i).map(|f| f.rotulo()).unwrap_or_default(),
        C::FocoPosicao => "Perto ↔ Longe", // i18n: chave
        C::Restaurar => regras::RESTAURAR_AUTOMATICO,
        C::UsarMeusAjustes => regras::USAR_MEUS_AJUSTES,
        C::PermitirRemoto => FRASE_PERMITIR,
    };
    t(s).to_string()
}

/// A opção do aparelho que filma (R9b, `docs/controle-remoto-da-camera.md`), literal.
pub const FRASE_PERMITIR: &str = "Permitir controle remoto da câmera"; // i18n: chave

/// **O nome de cada controle para o Narrador.** Os deslizantes levam o valor.
pub fn texto_acessivel(c: ControleDosAjustes, e: &EstadoDosAjustes) -> String {
    use ControleDosAjustes as C;
    let r = rotulo(c, e);
    if let Some(remoto) = &e.remoto {
        return match (c, remotos::valor_acessivel(c, remoto)) {
            (C::Aba(_), _) => format!("{r}, {}", t("aba")),
            (_, Some(v)) => format!("{r}, {v}"),
            _ => r,
        };
    }
    let valor = match c {
        C::Brilho => faixa(e, Propriedade::Brilho).map(|f| brilho(e, &f).to_string()),
        C::Ganho => faixa(e, Propriedade::Ganho).map(|f| ganho(e, &f).to_string()),
        C::Obturador => faixa(e, Propriedade::Exposicao).map(|f| regras::texto_do_obturador(obturador(e, &faixa_do_obturador(&f, e.painel.fps)))),
        C::Kelvin => faixa(e, Propriedade::Balanco).map(|f| format!("{} K", kelvin(e, &faixa_do_kelvin(&f)))),
        C::FocoPosicao => Some(regras::com_virgula(foco_posicao(e), 2)),
        C::Aba(_) => Some(t("aba").into()),
        _ => None,
    };
    match valor {
        Some(v) => format!("{r}, {v}"),
        None => r,
    }
}

/// **Como cada botão se desenha**, no tamanho `l × a` (os deslizantes são do sistema), com o rótulo
/// de [`rotulo`].
pub fn aparencia(c: ControleDosAjustes, rotulo_: &str, l: f32, a: f32, s: EstadoDoControle) -> Aparencia {
    use ControleDosAjustes as C;
    let rotulo = |_c: ControleDosAjustes| rotulo_.to_string();
    let mut fundo = FUNDO;
    let mut raio = RAIO_DO_BOTAO;
    let mut itens = match c.especie() {
        // O título longo da opção do remoto (R9b) quebra em duas linhas, em vez de cortar.
        EspecieDosAjustes::Alternar if c == C::PermitirRemoto => {
            raio = RAIO_DO_CARTAO;
            let mut v = vec![caixa_com_borda(Ret::new(0.0, 0.0, l, a), RAIO_DO_CARTAO, SUPERFICIE, CONTORNO, 1.0)];
            let xi = l - 16.0 - INTERRUPTOR_L;
            v.push(texto(Ret::new(16.0, 4.0, (xi - 28.0).max(1.0), a - 8.0), rotulo_, F_CORPO_FORTE, TEXTO).quebra().meio().item());
            v.extend(interruptor(xi, (a - INTERRUPTOR_A) / 2.0, s.marcado));
            v
        }
        EspecieDosAjustes::Opcao => {
            fundo = FUNDO;
            raio = 4.0;
            let mut v = vec![caixa(Ret::new(0.0, 0.0, l, a), 6.0, SUPERFICIE_ALTA)];
            v.extend(segmento(l, a, &rotulo(c), s.marcado).into_iter().skip(1));
            v
        }
        EspecieDosAjustes::Alternar => {
            raio = RAIO_DO_CARTAO;
            linha_de_interruptor(l, a, &rotulo(c), "", s.marcado)
        }
        EspecieDosAjustes::Botao => {
            let tipo = if c == C::PassarParaManual { TipoDeBotao::Principal } else { TipoDeBotao::Secundario };
            botao(l, a, tipo, &rotulo(c), None, F_BOTAO_PEQUENO)
        }
        EspecieDosAjustes::Deslizante => vec![],
    };
    if s.quente && !s.desligado {
        itens.push(caixa(Ret::new(0.0, 0.0, l, a), raio, Cor::rgba(0xFFFFFF, 10)));
    }
    if s.foco {
        itens.push(anel_de_foco(l, a, raio));
    }
    let opacidade = if s.desligado {
        0.4
    } else if s.apertado {
        0.9
    } else {
        1.0
    };
    Aparencia { fundo, itens, opacidade }
}

// =============================================================================================
// Os exemplos (os testes e o retrato de bancada)
// =============================================================================================

fn faixa_(min: i32, max: i32, passo: i32, padrao: i32, bandeiras: i32) -> Faixa {
    Faixa { min, max, passo, padrao, bandeiras }
}

fn painel_de_exemplo() -> PainelDosAjustes {
    use regras::{Lido, FLAGS_AUTO as A, FLAGS_MANUAL as M};
    let mut p = PainelDosAjustes { fase: FaseDosAjustes::Pronto, fps: 30.0, ..Default::default() };
    p.caps.insert(Propriedade::Exposicao, faixa_(-11, -1, 1, -6, A | M));
    p.caps.insert(Propriedade::Ganho, faixa_(0, 100, 1, 0, M));
    p.caps.insert(Propriedade::Brilho, faixa_(-64, 64, 1, 0, M));
    p.caps.insert(Propriedade::Balanco, faixa_(2800, 6500, 10, 4600, A | M));
    p.caps.insert(Propriedade::Foco, faixa_(0, 250, 5, 0, A | M));
    p.caps.insert(Propriedade::AntiCintilacao, faixa_(0, 2, 1, 1, M));
    for (prop, v, b) in [
        (Propriedade::Exposicao, -6, A),
        (Propriedade::Ganho, 32, M),
        (Propriedade::Brilho, 0, M),
        (Propriedade::Balanco, 5230, A),
        (Propriedade::Foco, 125, A),
        (Propriedade::AntiCintilacao, 2, M),
    ] {
        p.lidos.insert(prop, Lido { valor: v, bandeiras: b });
    }
    p.linha_lida = regras::linha_lida(&p.lidos);
    p
}

/// **Os estados de exemplo**: cada aba, em Auto e em Manual, travada, compartilhada, lendo, sem
/// controles, e com e sem prévia.
pub fn exemplos() -> Vec<(String, EstadoDosAjustes)> {
    let base = EstadoDosAjustes { aba: Aba::Exposicao, painel: painel_de_exemplo(), com_previa: true, permitir: Some(false), remoto: None };
    let mut v = Vec::new();
    let mut manual = base.painel.clone();
    manual.registro.exposicao = ModoDeExposicao::Manual;
    manual.registro.iso = Some(64.0);
    manual.registro.obturador_ns = Some(regras::ns_do_log2(-7));
    manual.registro.balanco = Balanco::Kelvin;
    manual.registro.kelvin = Some(5600);
    manual.registro.foco = Foco::Manual;
    manual.registro.foco_posicao = Some(0.42);
    manual.divergencia = Some(regras::frase_da_divergencia(Propriedade::Exposicao, -6, -7));
    manual.controlado_por = Some("Pixel do Bruno".into()); // i18n: fora (exemplo)
    let mut travado = base.painel.clone();
    travado.registro.trava_exposicao = true;
    travado.registro.trava_balanco = true;
    let mut compartilhada = base.painel.clone();
    compartilhada.fase = FaseDosAjustes::Compartilhada;
    let lendo = PainelDosAjustes::default();
    let mut pobre = PainelDosAjustes { fase: FaseDosAjustes::Pronto, fps: 30.0, ..Default::default() };
    pobre.caps.insert(Propriedade::Brilho, faixa_(0, 255, 1, 128, regras::FLAGS_MANUAL));
    pobre.caps.insert(Propriedade::Foco, faixa_(0, 10, 1, 0, regras::FLAGS_AUTO));
    for com_previa in [true, false] {
        for aba in Aba::TODAS {
            for (nome, painel) in [("auto", &base.painel), ("manual", &manual), ("travado", &travado), ("compartilhada", &compartilhada), ("lendo", &lendo), ("pobre", &pobre)] {
                v.push((
                    format!("{}-{}-{nome}", if com_previa { "comum" } else { "r5" }, aba.indice()),
                    EstadoDosAjustes { aba, painel: painel.clone(), com_previa, permitir: Some(nome == "manual"), remoto: None },
                ));
            }
        }
    }
    v
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn o_id_de_cada_controle_volta() {
        let mut vistos = std::collections::HashSet::new();
        for c in ControleDosAjustes::todos() {
            assert_eq!(ControleDosAjustes::de_id(c.id()), Some(c), "{c:?}");
            assert!(vistos.insert(c.id()), "id repetido: {c:?}");
        }
        assert_eq!(ControleDosAjustes::de_id(1), None);
    }

    #[test]
    fn cada_aba_cabe_sem_rolar_e_nenhum_controle_cobre_outro() {
        for (nome, e) in exemplos() {
            let (l, a) = tamanho(&e);
            let janela = Ret::new(0.0, 0.0, l, a);
            let q = compor(&e);
            for x in &q.controles {
                assert!(janela.contem(&x.ret), "{nome}: {:?} fora da janela: {:?}", x.c, x.ret);
                if let Some(p) = q.previa {
                    assert!(!p.cruza(&x.ret), "{nome}: {:?} em cima da prévia", x.c);
                }
            }
            for (i, a_) in q.controles.iter().enumerate() {
                for b in q.controles.iter().skip(i + 1) {
                    assert!(!a_.ret.cruza(&b.ret), "{nome}: {:?} cobre {:?}", a_.c, b.c);
                }
            }
            for item in &q.itens {
                if let Item::Texto(t) = item {
                    assert!(janela.contem(&t.ret), "{nome}: texto fora: {:?}", t.texto_corrido());
                    for x in &q.controles {
                        assert!(!t.ret.cruza(&x.ret), "{nome}: o texto {:?} fica debaixo de {:?}", t.texto_corrido(), x.c);
                    }
                }
            }
        }
    }

    fn achar(nome: &str) -> EstadoDosAjustes {
        exemplos().into_iter().find(|(n, _)| n == nome).unwrap().1
    }

    fn textos(q: &QuadroDosAjustes) -> Vec<String> {
        q.itens.iter().filter_map(|i| if let Item::Texto(t) = i { Some(t.texto_corrido()) } else { None }).collect()
    }

    #[test]
    fn o_que_cada_aba_mostra() {
        use ControleDosAjustes as C;
        // Exposição em Auto: o brilho livre, a trava, a anti-cintilação.
        let q = compor(&achar("comum-0-auto"));
        assert!(q.achar(C::Brilho).unwrap().habilitado);
        assert!(q.achar(C::TravarExposicao).unwrap().habilitado);
        assert!(q.achar(C::Exposicao(0)).unwrap().marcado);
        assert!(q.achar(C::Cintilacao(0)).unwrap().marcado, "auto é o padrão");
        assert!(q.previa.is_some(), "a câmera comum tem a prévia dentro (§4.2)");
        // Travada: o brilho apagado, com a linha.
        let q = compor(&achar("comum-0-travado"));
        assert!(!q.achar(C::Brilho).unwrap().habilitado);
        assert!(textos(&q).contains(&"Destrave a exposição para compensar.".to_string()));
        // Ganho e obturador em Auto: a frase e o botão; em Manual, os dois deslizantes.
        let q = compor(&achar("comum-1-auto"));
        assert!(q.lugar(C::PassarParaManual).is_some() && q.lugar(C::Ganho).is_none());
        let q = compor(&achar("comum-1-manual"));
        assert!(q.lugar(C::Ganho).is_some() && q.lugar(C::Obturador).is_some());
        assert!(textos(&q).contains(&"1/128 s".to_string()));
        let d = q.achar(C::Obturador).unwrap().degraus.unwrap();
        assert_eq!(d.max, 6, "de −11 ao teto de 1/30 (−5)");
        // Balanço: os presets apagados com a linha; Kelvin com o deslizante e sem a trava.
        let q = compor(&achar("comum-2-auto"));
        assert!(!q.achar(C::Balanco(1)).unwrap().habilitado && q.achar(C::Balanco(5)).unwrap().habilitado);
        assert!(textos(&q).contains(&"Esta câmera não oferece os presets de balanço.".to_string()));
        assert!(q.lugar(C::TravarBalanco).is_some() && q.lugar(C::Kelvin).is_none());
        let q = compor(&achar("comum-2-manual"));
        assert!(q.lugar(C::Kelvin).is_some() && q.lugar(C::TravarBalanco).is_none(), "§3.4: com Kelvin a trava some");
        assert!(textos(&q).contains(&"5600 K".to_string()));
        // Foco manual: "Perto ↔ Longe" com 0,42.
        let q = compor(&achar("comum-3-manual"));
        assert!(textos(&q).contains(&"0,42".to_string()));
        assert_eq!(q.achar(C::FocoPosicao).unwrap().degraus, Some(Degraus { max: 100, pos: 42 }));
        // A divergência na linha do alto.
        assert!(textos(&q).iter().any(|t| t == "A câmera usou 1/64 s em vez de 1/128 s."));
        // Compartilhada: tudo apagado, com a frase do §3.5.
        let q = compor(&achar("comum-0-compartilhada"));
        assert!(q.controles.iter().filter(|x| !matches!(x.c, C::Aba(_) | C::PermitirRemoto)).all(|x| !x.habilitado));
        // A opção do remoto vale para o app: viva até no modo compartilhado (R9b).
        assert!(q.achar(C::PermitirRemoto).unwrap().habilitado);
        assert!(textos(&q).iter().any(|t| t == regras::FRASE_OUTRO_APP));
        // A câmera pobre: uma linha só onde nada se aplica.
        let q = compor(&achar("r5-1-pobre"));
        assert!(textos(&q).contains(&"Esta câmera não oferece o ganho e o obturador.".to_string()));
        assert!(q.previa.is_none(), "a R5 não tem prévia na janela (§4.2)");
        let q = compor(&achar("r5-2-pobre"));
        assert!(textos(&q).contains(&"Esta câmera não oferece o Kelvin.".to_string()));
        assert!(q.lugar(C::Balanco(0)).is_none(), "sem balanço, nada da grade");
        let q = compor(&achar("r5-3-pobre"));
        assert!(textos(&q).contains(&"Esta câmera não oferece o foco manual.".to_string()));
        let mut fixo = achar("r5-3-pobre");
        fixo.painel.caps.remove(&Propriedade::Foco);
        assert!(textos(&compor(&fixo)).contains(&"Esta câmera tem foco fixo.".to_string()));
        let q = compor(&achar("r5-0-pobre"));
        assert!(textos(&q).contains(&"Esta câmera não oferece a anti-cintilação.".to_string()));
        assert!(textos(&q).contains(&"Esta câmera não oferece a trava de exposição.".to_string()));
        assert!(q.achar(C::Restaurar).unwrap().habilitado);
        // "Usar meus ajustes" (07/10): só com lembrança diferente do que vale, ao lado do Restaurar.
        assert!(q.achar(C::UsarMeusAjustes).is_none(), "sem meus ajustes, sem botão");
        let mut com = achar("r5-0-pobre");
        com.painel.meus_ajustes = Some(crate::regras_dos_controles::Registro { ev: 1.0, ..Default::default() });
        let q2 = compor(&com);
        assert!(q2.achar(C::UsarMeusAjustes).is_some());
        com.painel.registro = com.painel.meus_ajustes.clone().unwrap();
        assert!(compor(&com).achar(C::UsarMeusAjustes).is_none(), "já vale: some");
        assert!(!compor(&achar("r5-0-lendo")).achar(C::Restaurar).unwrap().habilitado);
    }

    #[test]
    fn a_opcao_do_remoto_e_quem_controla() {
        use ControleDosAjustes as C;
        let q = compor(&achar("comum-0-manual"));
        let x = q.achar(C::PermitirRemoto).unwrap();
        assert!(x.marcado && x.habilitado);
        assert!(textos(&q).contains(&"Controlado por Pixel do Bruno".to_string()));
        let e = achar("comum-0-auto");
        assert!(!compor(&e).achar(C::PermitirRemoto).unwrap().marcado);
        assert!(matches!(gesto(C::PermitirRemoto, &e, None), Some(Gesto::Permitir(true))));
        // Sem prévia (a R5) a janela cresce a faixa; com ela, a faixa cabe embaixo do aviso.
        assert_eq!(tamanho(&achar("r5-0-auto")).1, lugar::ALTURA + lugar::FAIXA_DO_REMOTO_A);
        assert_eq!(tamanho(&e).1, lugar::ALTURA);
        // Sem a opção (a sonda sem rede), nada.
        let mut sem = e.clone();
        sem.permitir = None;
        assert!(compor(&sem).lugar(C::PermitirRemoto).is_none());
        assert_eq!(texto_acessivel(C::PermitirRemoto, &e), "Permitir controle remoto da câmera");
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert_eq!(texto_acessivel(C::PermitirRemoto, &e), "Allow remote camera control");
            assert!(textos(&compor(&achar("r5-1-manual"))).contains(&"Controlled by Pixel do Bruno".to_string()));
        });
    }

    #[test]
    fn a_pouca_luz_na_linha_de_aviso() {
        use crate::regras_dos_controles::PoucaLuz;
        let luz = PoucaLuz { fps_agora: 15, fps: 30, com_manual: true };
        let mut e = achar("comum-0-auto");
        assert!(!textos(&compor(&e)).iter().any(|t| t.starts_with("Pouca luz")), "sem pouca luz, sem aviso");
        e.painel.pouca_luz = Some(luz);
        let q = compor(&e);
        assert!(textos(&q).contains(&"Pouca luz: 15 fps para clarear a imagem. Para 30 fps, use a exposição manual nos ajustes da câmera.".to_string()));
        assert!(q.itens.iter().any(|i| matches!(i, Item::Icone { icone: Icone::Info, .. })), "informação, no tom neutro");
        // Na R5 (sem prévia) também, e sem obturador manual o conselho é a luz.
        let mut r5 = achar("r5-0-pobre");
        r5.painel.pouca_luz = Some(PoucaLuz { com_manual: false, ..luz });
        assert!(textos(&compor(&r5)).contains(&"Pouca luz: 15 fps para clarear a imagem. Mais luz no ambiente devolve os 30 fps.".to_string()));
        // A divergência e o modo compartilhado vêm antes: uma linha de aviso só.
        let mut manual = achar("comum-1-manual");
        manual.painel.pouca_luz = Some(luz);
        let t_ = textos(&compor(&manual));
        assert!(t_.iter().any(|t| t.starts_with("A câmera usou")) && !t_.iter().any(|t| t.starts_with("Pouca luz")));
        let mut compartilhada = achar("comum-0-compartilhada");
        compartilhada.painel.pouca_luz = Some(PoucaLuz { com_manual: false, ..luz });
        let t_ = textos(&compor(&compartilhada));
        assert!(t_.iter().any(|t| t == regras::FRASE_OUTRO_APP) && !t_.iter().any(|t| t.starts_with("Pouca luz")));
        // O aviso não cobre controle nenhum, com e sem prévia.
        for e in [e, r5] {
            let q = compor(&e);
            for item in &q.itens {
                if let Item::Texto(t) = item {
                    for x in &q.controles {
                        assert!(!t.ret.cruza(&x.ret), "o texto {:?} fica debaixo de {:?}", t.texto_corrido(), x.c);
                    }
                }
            }
        }
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            let mut e = achar("comum-0-auto");
            e.painel.pouca_luz = Some(luz);
            assert!(textos(&compor(&e)).contains(&"Low light: 15 fps to brighten the picture. For 30 fps, use manual exposure in Camera settings.".to_string()));
        });
    }

    #[test]
    fn os_gestos() {
        use ControleDosAjustes as C;
        let e = achar("comum-1-manual");
        let a = |c, d| match gesto(c, &e, d) {
            Some(Gesto::Acao(a)) => a,
            _ => panic!("{c:?}"),
        };
        assert_eq!(a(C::Obturador, Some(0)), Acao::Obturador(-11));
        assert_eq!(a(C::Obturador, Some(99)), Acao::Obturador(-5), "o teto");
        assert_eq!(a(C::Ganho, Some(50)), Acao::Ganho(50));
        assert_eq!(a(C::Kelvin, Some(0)), Acao::Kelvin(2800));
        assert_eq!(a(C::FocoPosicao, Some(42)), Acao::FocoPosicao(0.42));
        assert_eq!(a(C::Cintilacao(1), None), Acao::AntiCintilacao(AntiCintilacao::Hz50));
        assert_eq!(a(C::TravarExposicao, None), Acao::TravarExposicao(true));
        assert_eq!(a(C::Restaurar, None), Acao::Restaurar);
        assert!(matches!(gesto(C::Aba(2), &e, None), Some(Gesto::Aba(Aba::Balanco))));
        assert_eq!(texto_acessivel(C::Obturador, &e), "Obturador, 1/128 s");
        assert_eq!(texto_acessivel(C::Restaurar, &e), "Restaurar automático");
    }

    #[test]
    fn em_ingles() {
        use ControleDosAjustes as C;
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            let e = achar("comum-1-manual");
            assert_eq!(texto_acessivel(C::Obturador, &e), "Shutter, 1/128 s");
            assert_eq!(texto_acessivel(C::Aba(1), &e), "Gain & shutter, tab");
            assert_eq!(texto_acessivel(C::Restaurar, &e), "Reset to auto");
            let q = compor(&achar("comum-3-manual"));
            assert!(textos(&q).contains(&"Near ↔ Far".to_string()));
            assert!(textos(&q).contains(&"0.42".to_string()));
            assert!(textos(&compor(&achar("r5-1-pobre"))).contains(&"This camera doesn't support gain and shutter.".to_string()));
        });
    }
}
