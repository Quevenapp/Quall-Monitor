//! A tela de "os dois aparelhos mostram o mesmo código?", do lado do núcleo.
//!
//! [`crate::session::hospedar_com_codigo`] e [`crate::session::conectar_com_codigo`] bloqueiam,
//! como as de sempre. Enquanto bloqueiam, a casca precisa de três coisas: **ver** o código quando
//! ele existe, **dizer** o que a pessoa tocou, e **saber** como terminou. É isto, no molde do
//! [`crate::cancel::Cancelamento`]: um estado compartilhado atrás de um `Arc`, sem canal e sem
//! thread — os laços do núcleo já acordam a cada 20 ms para olhar a sinalização, e passam a olhar
//! aqui também.
//!
//! Ver `docs/conexao.md` §4.

use std::sync::{Arc, Mutex};

/// Em que pé está a confirmação. Os valores no JSON da fronteira são os de [`Fase::como_texto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fase {
    /// Nada para mostrar ainda (esperando alguém tocar Conectar, ou a troca começar).
    Esperando,
    /// O código está pronto: mostre-o e os botões Confirmar e Recusar.
    Mostrando,
    /// A pessoa daqui confirmou; falta o outro lado.
    ConfirmadoAqui,
    /// Os dois confirmaram e o segredo do par foi derivado.
    Conferiu,
    /// A pessoa daqui recusou.
    RecusadoAqui,
    /// O outro lado recusou, ou caiu antes de confirmar.
    RecusadoLa,
    /// A confirmação não fechou: os dois lados chegaram a chaves diferentes (alguém no meio) ou
    /// o compromisso não foi cumprido.
    NaoConferiu,
    /// A pessoa não decidiu a tempo.
    Expirou,
}

impl Fase {
    pub fn como_texto(self) -> &'static str {
        match self {
            Fase::Esperando => "esperando",
            Fase::Mostrando => "mostrando",
            Fase::ConfirmadoAqui => "confirmado_aqui",
            Fase::Conferiu => "conferiu",
            Fase::RecusadoAqui => "recusado_aqui",
            Fase::RecusadoLa => "recusado_la",
            Fase::NaoConferiu => "nao_conferiu",
            Fase::Expirou => "expirou",
        }
    }
}

#[derive(Debug)]
struct Estado {
    fase: Fase,
    codigo: Option<String>,
    /// O nome que o outro aparelho anunciou, para a tela dizer **quem** está pedindo.
    par: Option<String>,
    /// O que a pessoa tocou, ainda não consumido pelo laço do núcleo.
    decisao: Option<bool>,
    /// Quantas trocas pelo código começaram nesta confirmação (o anfitrião conta para o limite).
    tentativas: u32,
    /// **Anfitrião**: a pessoa escolhe entre três números (`docs/v3-no-privado.md` §3.6); tocar
    /// "Confirmar" sem escolher não vale.
    anfitriao: bool,
    /// Os três números do anfitrião, em ordem sorteada; vazio no convidado.
    opcoes: Vec<String>,
    /// A pessoa escolheu um número que não era o do outro aparelho.
    escolha_errada: bool,
    /// **P11**: depois de uma escolha errada, o código desta espera acabou (o PIN continua).
    bloqueado: bool,
    /// Por que o núcleo recusou a última troca, quando é um motivo que a tela tem o que dizer
    /// (`"already_paired"`: já há vínculo com quem pediu, B2). `None` nos outros desfechos.
    recusa: Option<&'static str>,
    /// **O que a tela diz junto do cartão** (`docs/v3-no-privado.md` §8.6): `"previous_pairing_invalid"`
    /// quando o toque tentou reconectar e o outro lado não reconheceu o pareamento — este código é
    /// para parear de novo. Vale o toque inteiro (não some quando a troca do código começa).
    aviso: Option<&'static str>,
}

/// O estado da confirmação, compartilhado entre a thread que espera e a tela.
///
/// Clonar dá outra ponta do **mesmo** estado.
#[derive(Debug, Clone)]
pub struct ConfirmacaoPorCodigo(Arc<Mutex<Estado>>);

impl Default for ConfirmacaoPorCodigo {
    fn default() -> Self {
        Self::nova()
    }
}

/// O que a tela lê de uma vez, para não ver o código de uma tentativa e a fase de outra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retrato {
    pub fase: Fase,
    pub codigo: Option<String>,
    pub par: Option<String>,
    pub tentativas: u32,
    /// Os três números do anfitrião (vazio no convidado).
    pub opcoes: Vec<String>,
    /// A última escolha foi errada (e o código desta espera acabou, P11).
    pub escolha_errada: bool,
    /// O motivo da recusa do núcleo, quando há um que a tela diga (`"already_paired"`).
    pub recusa: Option<&'static str>,
    /// O aviso do toque (`"previous_pairing_invalid"`), ver [`ConfirmacaoPorCodigo::avisar`].
    pub aviso: Option<&'static str>,
}

impl ConfirmacaoPorCodigo {
    pub fn nova() -> Self {
        Self(Arc::new(Mutex::new(Estado {
            fase: Fase::Esperando,
            codigo: None,
            par: None,
            decisao: None,
            tentativas: 0,
            anfitriao: false,
            opcoes: Vec::new(),
            escolha_errada: false,
            bloqueado: false,
            recusa: None,
            aviso: None,
        })))
    }

    /// Roda `f` com o estado. Um cadeado envenenado (panic noutra thread) não derruba ninguém:
    /// o estado é recuperado como está — é só a tela de confirmação.
    fn com<R>(&self, f: impl FnOnce(&mut Estado) -> R) -> R {
        let mut guarda = match self.0.lock() {
            Ok(g) => g,
            Err(envenenado) => envenenado.into_inner(),
        };
        f(&mut guarda)
    }

    /// O que mostrar agora.
    pub fn retrato(&self) -> Retrato {
        self.com(|e| Retrato {
            fase: e.fase,
            // **No anfitrião, o código não sai** (revisão, baixo 1): ele mostra os três números, e a
            // pessoa tem de achar o certo olhando o outro aparelho. Dar o código à tela permitiria
            // destacá-lo — e a escolha viraria um "Confirmar".
            codigo: if e.anfitriao { None } else { e.codigo.clone() },
            par: e.par.clone(),
            tentativas: e.tentativas,
            opcoes: e.opcoes.clone(),
            escolha_errada: e.escolha_errada,
            recusa: e.recusa,
            aviso: e.aviso,
        })
    }

    /// **Anfitrião**: a pessoa tocou o número `indice` (de [`Retrato::opcoes`]) no cartão da
    /// tentativa `tentativa`. O certo confirma; um falso recusa, e o código desta espera acaba (P11).
    /// Fora da tentativa e da fase certas, é ignorado e devolve `false`.
    pub fn escolher(&self, tentativa: u32, indice: usize) -> bool {
        self.com(|e| {
            if !e.anfitriao || e.fase != Fase::Mostrando || e.tentativas != tentativa {
                return false;
            }
            let (Some(codigo), Some(escolhido)) = (e.codigo.as_ref(), e.opcoes.get(indice)) else {
                return false;
            };
            let acertou = escolhido == codigo;
            e.decisao = Some(acertou);
            if acertou {
                e.fase = Fase::ConfirmadoAqui;
            } else {
                e.fase = Fase::RecusadoAqui;
                e.escolha_errada = true;
                e.bloqueado = true;
            }
            true
        })
    }

    /// O código desta espera acabou por uma escolha errada (P11).
    pub fn bloqueado(&self) -> bool {
        self.com(|e| e.bloqueado)
    }

    /// A pessoa tocou **Confirmar** no cartão da tentativa `tentativa` (a de [`Retrato::tentativas`]
    /// que a tela mostrava). Só vale se **essa** tentativa ainda está com o código na tela; fora
    /// disso é ignorado e devolve `false`.
    ///
    /// # Por que a tentativa vai junto (revisão adversarial de 12/10, M2)
    ///
    /// Sem ela, "Confirmar" valia para qualquer código que estivesse na tela **agora**: a tela lê o
    /// estado a cada 150–200 ms, e um toque dado olhando o cartão de uma tentativa que acabou de
    /// cair confirmava a tentativa seguinte — o código de outro aparelho, que a pessoa nunca viu.
    pub fn aceitar(&self, tentativa: u32) -> bool {
        self.decidir(tentativa, true)
    }

    /// A pessoa tocou **Recusar**. Mesmo cuidado de [`ConfirmacaoPorCodigo::aceitar`].
    pub fn recusar(&self, tentativa: u32) -> bool {
        self.decidir(tentativa, false)
    }

    fn decidir(&self, tentativa: u32, aceito: bool) -> bool {
        self.com(|e| {
            if e.fase != Fase::Mostrando || e.tentativas != tentativa {
                return false;
            }
            // No anfitrião, confirmar é escolher o número certo (`escolher`); recusar vale.
            if e.anfitriao && aceito {
                return false;
            }
            e.decisao = Some(aceito);
            e.fase = if aceito { Fase::ConfirmadoAqui } else { Fase::RecusadoAqui };
            true
        })
    }

    // ---- do lado do núcleo ----

    /// Uma troca pelo código começou: limpa o que sobrou da anterior.
    pub(crate) fn comecar(&self, par: &str, anfitriao: bool) {
        self.com(|e| {
            e.fase = Fase::Esperando;
            e.codigo = None;
            e.decisao = None;
            e.opcoes.clear();
            e.escolha_errada = false;
            e.recusa = None;
            e.anfitriao = anfitriao;
            e.par = (!par.is_empty()).then(|| par.to_string());
            e.tentativas = e.tentativas.saturating_add(1);
        })
    }

    pub(crate) fn mostrar(&self, codigo: &str, opcoes: &[String]) {
        self.com(|e| {
            if e.fase == Fase::Esperando {
                e.codigo = Some(codigo.to_string());
                e.opcoes = opcoes.to_vec();
                e.fase = Fase::Mostrando;
            }
        })
    }

    /// O núcleo recusou a troca por um motivo que a tela tem o que dizer.
    pub(crate) fn recusar_por(&self, motivo: &'static str) {
        self.com(|e| e.recusa = Some(motivo))
    }

    /// **O pareamento anterior não vale mais** (a retomada do toque não foi reconhecida): a tela diz
    /// que o código que vem é para parear de novo.
    pub(crate) fn avisar(&self, aviso: &'static str) {
        self.com(|e| e.aviso = Some(aviso))
    }

    /// O nome do outro lado, quando ele chega (no convidado, só no fim: M2).
    pub(crate) fn nomear(&self, par: &str) {
        self.com(|e| e.par = (!par.is_empty()).then(|| par.to_string()))
    }

    /// Consome a decisão da pessoa, se houver.
    pub(crate) fn tomar_decisao(&self) -> Option<bool> {
        self.com(|e| e.decisao.take())
    }

    /// Fecha a tentativa. Uma fase que a própria pessoa já decidiu (recusado aqui) não é
    /// sobrescrita por um desfecho genérico que vem depois.
    pub(crate) fn encerrar(&self, fase: Fase) {
        self.com(|e| {
            e.decisao = None;
            if !(e.fase == Fase::RecusadoAqui && fase != Fase::Conferiu) {
                e.fase = fase;
            }
            if fase != Fase::Conferiu {
                e.codigo = None;
                e.opcoes.clear();
            }
        })
    }

    pub(crate) fn tentativas(&self) -> u32 {
        self.com(|e| e.tentativas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn so_aceita_toque_com_o_codigo_na_tela() {
        let c = ConfirmacaoPorCodigo::nova();
        assert!(!c.aceitar(0));
        assert_eq!(c.tomar_decisao(), None, "sem código, o toque não vale");
        c.comecar("MacBook", false);
        c.mostrar("0427", &[]);
        let r = c.retrato();
        assert_eq!(r.fase, Fase::Mostrando);
        assert_eq!(r.codigo.as_deref(), Some("0427"));
        assert_eq!(r.par.as_deref(), Some("MacBook"));
        assert!(c.aceitar(1));
        assert!(!c.recusar(1)); // depois de decidido, o segundo toque não muda nada
        assert_eq!(c.tomar_decisao(), Some(true));
        assert_eq!(c.retrato().fase, Fase::ConfirmadoAqui);
    }

    #[test]
    fn a_tentativa_seguinte_comeca_limpa_e_conta() {
        let c = ConfirmacaoPorCodigo::nova();
        c.comecar("a", false);
        c.mostrar("1111", &[]);
        c.recusar(1);
        c.encerrar(Fase::RecusadoLa);
        assert_eq!(c.retrato().fase, Fase::RecusadoAqui, "quem recusou foi a pessoa daqui");
        c.comecar("b", false);
        let r = c.retrato();
        assert_eq!((r.fase, r.codigo, r.tentativas), (Fase::Esperando, None, 2));
    }

    /// **M2 da revisão de 12/10.** A tela mostrava o código da tentativa 1; ela caiu, a 2 começou
    /// com outro código, e o toque da pessoa (dado olhando o cartão da 1) chega agora. Não confirma.
    #[test]
    fn toque_no_cartao_da_tentativa_anterior_nao_confirma_a_seguinte() {
        let c = ConfirmacaoPorCodigo::nova();
        c.comecar("aparelho certo", false);
        c.mostrar("1111", &[]);
        let visto = c.retrato();
        c.encerrar(Fase::RecusadoLa);
        c.comecar("outro aparelho", false);
        c.mostrar("2222", &[]);
        assert!(!c.aceitar(visto.tentativas), "o toque era para a tentativa {}", visto.tentativas);
        assert_eq!(c.tomar_decisao(), None);
        assert_eq!(c.retrato().fase, Fase::Mostrando);
        assert!(c.aceitar(c.retrato().tentativas), "o toque desta tentativa vale");
    }

    #[test]
    fn a_copia_e_o_mesmo_estado() {
        let c = ConfirmacaoPorCodigo::nova();
        let tela = c.clone();
        c.comecar("x", false);
        c.mostrar("9999", &[]);
        std::thread::spawn(move || tela.aceitar(1)).join().expect("thread");
        assert_eq!(c.tomar_decisao(), Some(true));
    }
}

#[cfg(test)]
mod testes_da_escolha {
    use super::*;

    fn tres() -> Vec<String> {
        vec!["8812".into(), "0427".into(), "5530".into()]
    }

    /// No anfitrião, confirmar é **escolher** o número que o outro aparelho mostra
    /// (`docs/v3-no-privado.md` §3.6): "Confirmar" sozinho não vale.
    #[test]
    fn no_anfitriao_so_a_escolha_certa_confirma() {
        let c = ConfirmacaoPorCodigo::nova();
        c.comecar("Galaxy A07", true);
        c.mostrar("0427", &tres());
        let r = c.retrato();
        assert_eq!(r.opcoes, tres());
        assert_eq!(r.codigo, None, "baixo 1: o anfitrião não recebe o código, só os três números");
        assert!(!c.aceitar(r.tentativas), "sem escolher, não confirma");
        assert!(!c.escolher(r.tentativas, 7), "índice fora");
        assert!(c.escolher(r.tentativas, 1));
        assert_eq!(c.tomar_decisao(), Some(true));
        assert_eq!(c.retrato().fase, Fase::ConfirmadoAqui);
        assert!(!c.bloqueado());
    }

    /// **P11**: um erro na escolha recusa e encerra o código daquela espera (o PIN continua).
    #[test]
    fn escolha_errada_recusa_e_bloqueia_o_codigo_da_espera() {
        let c = ConfirmacaoPorCodigo::nova();
        c.comecar("Intruso", true);
        c.mostrar("0427", &tres());
        assert!(c.escolher(1, 0));
        assert_eq!(c.tomar_decisao(), Some(false));
        let r = c.retrato();
        assert_eq!(r.fase, Fase::RecusadoAqui);
        assert!(r.escolha_errada);
        assert!(c.bloqueado());
    }

    /// No convidado não há escolha: ele só mostra o código.
    #[test]
    fn no_convidado_nao_ha_escolha() {
        let c = ConfirmacaoPorCodigo::nova();
        c.comecar("", false);
        c.mostrar("0427", &[]);
        assert!(!c.escolher(1, 0));
        assert!(c.retrato().opcoes.is_empty());
    }
}
