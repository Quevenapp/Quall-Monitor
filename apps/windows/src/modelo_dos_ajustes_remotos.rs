//! **A janela "Ajustes da câmera" de quem recebe, sem Win32** (R9b,
//! `docs/controle-remoto-da-camera.md` §5 e §12): o painel das quatro abas do R9 desenhado **a
//! partir das capacidades que chegaram** do filmador, e não da câmera daqui.
//!
//! É a mesma janela (`janela_dos_ajustes.rs`), com os mesmos controles nativos
//! ([`ControleDosAjustes`]) e os mesmos lugares (`estilo::lugar::ajustes`, sem prévia: o vídeo está
//! na janela dele). O que muda é de onde vêm os números e para onde vão os gestos:
//!
//! - **o estado** é o JSON de `Controlador::estado_json` do núcleo (§11.1, "O estado do receptor"):
//!   a situação, as capacidades, o ajuste (o aplicado com o pendente por cima), o lido e a recusa;
//! - **os controles** saem dos descritores (§3.2): uma lista vira opções, um número vira um
//!   deslizante em degraus, `{}` vira um interruptor. As dicas `unidade`, `origem` e `escala`
//!   mudam o nome e o texto (o "Brilho" e o "Ganho" do Windows, o obturador em 2^v), e os `limites`
//!   viram as frases do R9 §3.5 pelos códigos;
//! - **um gesto** vira um pedido parcial, só com o campo mexido ([`gesto`]); "Restaurar
//!   automático" é a ação `restaurar`. O receptor Windows não tem prévia tocável, e o toque não se
//!   pede daqui.
//!
//! Nas situações sem controles (`esperando`, `sem_resposta`, `sem_camera`) o painel fica vazio, com
//! uma linha; com `nao_permitido`, tudo apagado **com os valores** e "O aparelho não permite controle
//! remoto da câmera".

use serde_json::{json, Map, Value};

use crate::estilo::lugar::ajustes as lugar;
use crate::estilo::*;
use crate::idioma::{t, tf, tr};
use crate::modelo_dos_ajustes::{ControleDosAjustes, Degraus, QuadroDosAjustes};
use crate::regras_dos_controles::{self as regras, Aba, Balanco};

// =============================================================================================
// O estado do receptor, lido do núcleo
// =============================================================================================

/// As situações do contrato (§5).
pub const PRONTO: &str = "pronto";
pub const NAO_PERMITIDO: &str = "nao_permitido";
pub const SEM_RESPOSTA: &str = "sem_resposta";
pub const SEM_CAMERA: &str = "sem_camera";

/// **O estado do controle remoto**, como o núcleo o publica.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EstadoRemoto {
    pub situacao: String,
    /// As capacidades (`controles`, `limites`); vazio antes de chegarem.
    pub capacidades: Map<String, Value>,
    /// O ajuste aplicado, com o pendente por cima (§5).
    pub ajuste: Map<String, Value>,
    pub lido: Map<String, Value>,
    /// O motivo e o campo da última recusa (o núcleo a apaga em 3 s).
    pub recusa: Option<(String, Option<String>)>,
}

fn objeto(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}

impl EstadoRemoto {
    /// Lê o JSON de `Controlador::estado_json`. Ilegível: `esperando`, sem nada.
    pub fn de_json(json: &str) -> EstadoRemoto {
        let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
        EstadoRemoto {
            situacao: v.get("situacao").and_then(Value::as_str).unwrap_or("esperando").to_string(),
            capacidades: objeto(v.get("capacidades")),
            ajuste: objeto(v.get("ajuste")),
            lido: objeto(v.get("lido")),
            recusa: v.get("recusa").and_then(Value::as_object).and_then(|r| {
                let m = r.get("motivo")?.as_str()?.to_string();
                Some((m, r.get("campo").and_then(Value::as_str).map(str::to_string)))
            }),
        }
    }

    /// O painel aparece (vivo ou apagado)? Só em `pronto` e `nao_permitido`, com as capacidades (§12).
    pub fn com_controles(&self) -> bool {
        (self.situacao == PRONTO || self.situacao == NAO_PERMITIDO) && self.capacidades.contains_key("controles")
    }

    /// Os controles respondem ao toque? Só em `pronto`.
    pub fn vivo(&self) -> bool {
        self.situacao == PRONTO && self.com_controles()
    }

    fn descritor(&self, campo: &str) -> Option<Descritor> {
        self.capacidades.get("controles").and_then(|c| c.get(campo)).and_then(Descritor::de)
    }

    fn numero(&self, campo: &str) -> Option<Numero> {
        match self.descritor(campo)? {
            Descritor::Numero(n) => Some(n),
            _ => None,
        }
    }

    fn lista(&self, campo: &str) -> Vec<String> {
        match self.descritor(campo) {
            Some(Descritor::Lista(v)) => v,
            _ => Vec::new(),
        }
    }

    fn sim(&self, campo: &str) -> bool {
        matches!(self.descritor(campo), Some(Descritor::Sim))
    }

    fn limite(&self, campo: &str) -> Option<&str> {
        self.capacidades.get("limites").and_then(|l| l.get(campo)).and_then(Value::as_str)
    }

    /// O modo de um campo de modo (`exposicao`, `balanco`, `foco`), com o padrão "auto" (§6).
    fn modo(&self, campo: &str) -> &str {
        self.ajuste.get(campo).and_then(Value::as_str).unwrap_or("auto")
    }

    fn ligado(&self, campo: &str) -> bool {
        self.ajuste.get(campo).and_then(Value::as_bool).unwrap_or(false)
    }

    fn valor(&self, campo: &str) -> Option<f64> {
        self.ajuste.get(campo).and_then(Value::as_f64).or_else(|| self.lido.get(campo).and_then(Value::as_f64))
    }

    /// No Windows o `iso` é o ganho (`"unidade":"ganho"`).
    fn e_ganho(&self) -> bool {
        self.numero("iso").is_some_and(|n| n.unidade.as_deref() == Some("ganho"))
    }

    fn e_brilho(&self) -> bool {
        self.numero("ev").is_some_and(|n| n.unidade.as_deref() == Some("brilho"))
    }
}

// =============================================================================================
// Os descritores e as escalas
// =============================================================================================

/// Um descritor de controle (§3.2), com as dicas para a tela.
#[derive(Clone, Debug, PartialEq)]
pub enum Descritor {
    Lista(Vec<String>),
    Numero(Numero),
    Sim,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Numero {
    pub min: f64,
    pub max: f64,
    pub passo: Option<f64>,
    pub inteiro: bool,
    /// `"escala":"log2"`: os degraus são 2^v (o obturador do Windows).
    pub log2: bool,
    pub unidade: Option<String>,
    /// `"origem"`: o número mostrado é `origem + valor` (o brilho do Windows).
    pub origem: f64,
    /// `"calibrado"` (só no `focoPosicao`, de quem tem a lente calibrada, como o Android): as
    /// dioptrias da posição 1 (o mais perto). A tela mostra metros, `1 / (dioptrias × posição)`.
    pub calibrado: Option<f64>,
}

impl Descritor {
    pub fn de(v: &Value) -> Option<Descritor> {
        let o = v.as_object()?;
        if let Some(l) = o.get("valores") {
            return Some(Descritor::Lista(l.as_array()?.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()));
        }
        match (o.get("min").and_then(Value::as_f64), o.get("max").and_then(Value::as_f64)) {
            (Some(min), Some(max)) if min.is_finite() && max.is_finite() && min <= max => Some(Descritor::Numero(Numero {
                min,
                max,
                passo: o.get("passo").and_then(Value::as_f64).filter(|p| p.is_finite() && *p > 0.0),
                inteiro: o.get("inteiro").and_then(Value::as_bool).unwrap_or(false),
                log2: o.get("escala").and_then(Value::as_str) == Some("log2"),
                unidade: o.get("unidade").and_then(Value::as_str).map(str::to_string),
                origem: o.get("origem").and_then(Value::as_f64).filter(|x| x.is_finite()).unwrap_or(0.0),
                calibrado: o.get("calibrado").and_then(Value::as_f64).filter(|x| x.is_finite() && *x > 0.0),
            })),
            (None, None) => Some(Descritor::Sim),
            _ => None,
        }
    }
}

/// Os degraus de um deslizante, mais do que isto, são amostrados (a régua do trackbar não precisa
/// de mais posições do que pixels).
const TETO_DE_DEGRAUS: usize = 1000;

/// O ISO em terços de stop (R9 §3.2).
const ISO_EM_TERCOS: [f64; 22] = [
    50.0, 64.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 320.0, 400.0, 500.0, 640.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3200.0, 4000.0,
    5000.0, 6400.0,
];
/// As frações de cinema e vídeo do obturador (R9 §3.1), o denominador.
const FRACOES: [f64; 15] = [24.0, 25.0, 30.0, 48.0, 50.0, 60.0, 100.0, 120.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];

fn ns_de_log2(v: i32) -> f64 {
    (1e9 * 2f64.powi(v)).round()
}

/// Arredonda um valor à forma do fio: inteiro quando o descritor pede, e sem o ruído do ponto
/// flutuante (0,30000000000000004).
fn limpo(n: &Numero, v: f64) -> f64 {
    let v = v.clamp(n.min, n.max);
    if n.inteiro {
        v.round().clamp(n.min.ceil(), n.max.floor())
    } else {
        (v * 1e6).round() / 1e6
    }
}

/// **Os degraus de um campo numérico**, do menor ao maior, todos dentro de `[min, max]`.
pub fn degraus(campo: &str, n: &Numero) -> Vec<f64> {
    let mut v: Vec<f64> = if campo == "obturadorNs" && n.log2 {
        // 2^v dentro da faixa (§3.2: o receptor monta os degraus; a casca arredonda).
        let a = (n.min.max(1.0) / 1e9).log2().round() as i32;
        let b = (n.max.max(1.0) / 1e9).log2().round() as i32;
        (a..=b).map(ns_de_log2).collect()
    } else if campo == "obturadorNs" {
        // As frações que cabem, mais o próprio teto (o 1/fps), em ns.
        let mut v: Vec<f64> = FRACOES.iter().map(|d| (1e9 / d).round()).filter(|ns| *ns >= n.min && *ns <= n.max).collect();
        v.push(n.max);
        v
    } else if campo == "iso" && n.unidade.as_deref() != Some("ganho") {
        let mut v: Vec<f64> = ISO_EM_TERCOS.iter().copied().filter(|x| *x > n.min && *x < n.max).collect();
        v.push(n.min);
        v.push(n.max);
        v
    } else {
        let passo = n.passo.unwrap_or(if n.inteiro { 1.0 } else { (n.max - n.min) / 100.0 }).max(f64::EPSILON);
        let k = ((n.max - n.min) / passo + 1e-9).floor() as usize;
        if k <= TETO_DE_DEGRAUS {
            (0..=k).map(|i| n.min + i as f64 * passo).collect()
        } else {
            (0..=TETO_DE_DEGRAUS).map(|i| n.min + (i * k / TETO_DE_DEGRAUS) as f64 * passo).collect()
        }
    };
    let mut v: Vec<f64> = v.drain(..).map(|x| limpo(n, x)).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    if v.is_empty() {
        v.push(limpo(n, n.min));
    }
    v
}

/// O degrau mais perto de `x`.
pub fn degrau_de(v: &[f64], x: f64) -> usize {
    v.iter().enumerate().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())).map(|(i, _)| i).unwrap_or(0)
}

// =============================================================================================
// Os textos
// =============================================================================================

/// O texto de um valor, na unidade da tela.
pub fn texto_do_valor(campo: &str, n: Option<&Numero>, v: f64) -> String {
    match campo {
        "ev" if n.is_some_and(|n| n.unidade.as_deref() == Some("brilho")) => format!("{}", (n.map(|n| n.origem).unwrap_or(0.0) + v).round() as i64),
        "ev" => {
            // `+0,3 EV`, com o sinal sempre (R9 §3.3); o 0 é `0 EV`.
            let casas = match n.and_then(|n| n.passo) {
                Some(p) if p >= 1.0 => 0,
                Some(p) if p >= 0.1 => 1,
                _ => 2,
            };
            let r = (v * 100.0).round() / 100.0;
            if r == 0.0 {
                "0 EV".to_string()
            } else {
                format!("{}{} EV", if r > 0.0 { "+" } else { "" }, crate::idioma::decimal(r, casas))
            }
        }
        "obturadorNs" => texto_do_obturador_ns(v, n.is_some_and(|n| n.log2)),
        "kelvin" => format!("{} K", v.round() as i64),
        // Com a lente calibrada, metros (R9 §1): 1 / (dioptrias da posição 1 × posição); a
        // posição 0 é o infinito.
        "focoPosicao" => match n.and_then(|n| n.calibrado) {
            Some(_) if v <= 0.0 => "∞".to_string(),
            Some(d) => {
                let m = 1.0 / (d * v);
                format!("{} m", crate::idioma::decimal(m, if m < 10.0 { 2 } else { 1 }))
            }
            None => crate::idioma::decimal(v, 2),
        },
        _ => format!("{}", v.round() as i64),
    }
}

/// O obturador: `1/N s` (ou `2^v` no Windows, `1/2^-v s`).
pub fn texto_do_obturador_ns(ns: f64, log2: bool) -> String {
    if log2 {
        return regras::texto_do_obturador((ns.max(1.0) / 1e9).log2().round() as i32);
    }
    if ns >= 1e9 {
        format!("{} s", crate::idioma::decimal(ns / 1e9, if (ns / 1e9).fract() == 0.0 { 0 } else { 1 }))
    } else {
        format!("1/{} s", (1e9 / ns.max(1.0)).round() as i64)
    }
}

/// O `{controle}` das frases do §3.5, com o artigo, **já no idioma de agora**.
fn nome_do_controle(e: &EstadoRemoto, campo: &str) -> &'static str {
    match campo {
        "ev" if e.e_brilho() => t("o brilho"),
        "ev" => t("a compensação de exposição"),
        "iso" if e.e_ganho() => t("o ganho"),
        "iso" => t("ISO"),
        "obturadorNs" => t("o obturador"),
        "exposicao" => t("a exposição manual"),
        "kelvin" => t("o Kelvin"),
        "balanco" => t("os presets de balanço"),
        "antiCintilacao" => t("a anti-cintilação"),
        "focoPosicao" | "foco" => t("o foco manual"),
        "travaExposicao" => t("a trava de exposição"),
        "travaBalanco" => t("a trava de balanço"),
        "toque" => t("o toque para focar"), // i18n: fora (o nome no fio)
        _ => t("o ajuste"),
    }
}

/// **A frase de quem limita** (§3.2, R9 §3.5) para um código, no idioma de agora.
pub fn frase_do_limite(codigo: &str, controle: &str) -> String {
    let c = tr(controle);
    match codigo {
        "fabricante" => tf("O fabricante deste aparelho não libera {} para outros apps.", &[&c]),
        "macos" => tf("O macOS não oferece {} para câmeras.", &[&c]),
        "ios_cintilacao" => t("O iOS ajusta a cintilação sozinho.").to_string(),
        "camera_nao_oferece" => tf("Esta câmera não oferece {}.", &[&c]),
        "foco_fixo" => t(regras::FRASE_FOCO_FIXO).to_string(),
        "sem_calibracao" => t("Esta câmera não publica a calibração de cor que o Kelvin precisa.").to_string(),
        "outro_app" => t(regras::FRASE_OUTRO_APP).to_string(),
        _ => tf("Este aparelho não oferece {}.", &[&c]),
    }
}

/// A frase de um campo que não veio nas capacidades.
fn frase_de(e: &EstadoRemoto, campo: &str) -> String {
    frase_do_limite(e.limite(campo).unwrap_or(""), nome_do_controle(e, campo))
}

pub const FRASE_NAO_PERMITE: &str = "O aparelho não permite controle remoto da câmera"; // i18n: chave

/// **A linha de uma recusa** (§3.5), ou nada para os motivos que não se mostram.
pub fn frase_da_recusa(e: &EstadoRemoto, motivo: &str, campo: Option<&str>) -> Option<String> {
    let controle = || campo.map(|c| nome_do_controle(e, c)).unwrap_or_else(|| t("o ajuste"));
    Some(match motivo {
        "nao_permitido" => t(FRASE_NAO_PERMITE).to_string(),
        "campo_desconhecido" | "fora_da_faixa" | "incoerente" => tf("Este aparelho não aceitou {}.", &[&controle()]),
        "sem_resposta" => t("O aparelho não respondeu.").to_string(),
        "outro_app" => t(regras::FRASE_OUTRO_APP).to_string(),
        "nao_pareado" | "sem_camera" | "camera_trocada" | "superado" | "ocupado" | "invalido" | "fora_da_imagem" => return None,
        // `nao_aplicado`, e o código que esta build não conhece (§3.5).
        _ => t("O aparelho não conseguiu aplicar o ajuste.").to_string(),
    })
}

/// **A linha do alto** (§3.6): o que a câmera do outro lado diz estar usando.
pub fn linha_lida(e: &EstadoRemoto) -> String {
    let l = &e.lido;
    let mut partes = Vec::new();
    if let Some(v) = l.get("iso").and_then(Value::as_f64) {
        let v = v.round() as i64;
        partes.push(if e.e_ganho() { tf("Ganho {}", &[&v]) } else { tf("ISO {}", &[&v]) });
    }
    if let Some(v) = l.get("obturadorNs").and_then(Value::as_f64) {
        partes.push(texto_do_obturador_ns(v, e.numero("obturadorNs").is_some_and(|n| n.log2)));
    }
    if let Some(v) = l.get("kelvin").and_then(Value::as_f64) {
        partes.push(format!("{} K", v.round() as i64));
    }
    if let Some(v) = l.get("abertura").and_then(Value::as_f64) {
        partes.push(format!("f/{}", crate::idioma::decimal(v, 1)));
    }
    partes.join(" · ")
}

/// "A câmera usou {lido} em vez de {pedido}." do primeiro campo que o filmador diz divergir.
pub fn divergencia(e: &EstadoRemoto) -> Option<String> {
    let lista = e.lido.get("divergentes")?.as_array()?;
    lista.iter().filter_map(Value::as_str).find_map(|campo| {
        let lido = e.lido.get(campo)?.as_f64()?;
        let pedido = e.ajuste.get(campo)?.as_f64()?;
        let n = e.numero(campo);
        Some(tf("A câmera usou {} em vez de {}.", &[&texto_do_valor(campo, n.as_ref(), lido), &texto_do_valor(campo, n.as_ref(), pedido)]))
    })
}

/// O aviso do alto: a permissão, a recusa, a divergência, ou a situação sem controles.
pub fn aviso_do_alto(e: &EstadoRemoto) -> Option<String> {
    if !e.com_controles() {
        return Some(match e.situacao.as_str() {
            SEM_RESPOSTA => t("O aparelho não respondeu.").to_string(),
            SEM_CAMERA => t("O aparelho não está mostrando uma câmera.").to_string(),
            _ => t("Lendo a câmera…").to_string(),
        });
    }
    if e.situacao == NAO_PERMITIDO {
        return Some(t(FRASE_NAO_PERMITE).to_string());
    }
    if let Some((m, c)) = &e.recusa {
        if let Some(f) = frase_da_recusa(e, m, c.as_deref()) {
            return Some(f);
        }
    }
    divergencia(e)
}

/// O rótulo da segunda aba: "ISO e obturador", ou "Ganho e obturador" com o ganho do Windows.
pub fn rotulo_da_aba(e: &EstadoRemoto, a: Aba) -> &'static str {
    match a {
        Aba::GanhoEObturador if !e.e_ganho() => "ISO e obturador", // i18n: chave
        _ => a.rotulo(),
    }
}

// =============================================================================================
// A composição
// =============================================================================================

/// O valor do campo na escala do deslizante: os degraus e a posição.
fn deslizante(e: &EstadoRemoto, campo: &str) -> Option<(Numero, Vec<f64>, usize, f64)> {
    let n = e.numero(campo)?;
    let v = degraus(campo, &n);
    let atual = e.valor(campo).unwrap_or(if campo == "ev" { 0.0 } else { n.min });
    let i = degrau_de(&v, atual);
    Some((n, v, i, atual))
}

const OPCOES_DA_CINTILACAO: [&str; 4] = ["auto", "50", "60", "desligada"];
const OPCOES_DO_FOCO: [&str; 3] = ["auto", "travado", "manual"];

/// O valor no fio de cada balanço da grade (`Balanco::GRADE`).
fn no_fio(b: Balanco) -> &'static str {
    match b {
        Balanco::Auto => "auto",
        Balanco::Incandescente => "incandescente",
        Balanco::Fluorescente => "fluorescente",
        Balanco::LuzDoDia => "luzDoDia",
        Balanco::Nublado => "nublado",
        Balanco::Kelvin => "kelvin",
    }
}

/// **Compõe o painel remoto** na aba `aba` (sem prévia: o vídeo está na janela dele).
pub fn compor(aba: Aba, e: &EstadoRemoto) -> QuadroDosAjustes {
    use ControleDosAjustes as C;
    let mut q = QuadroDosAjustes::default();
    let (largura, altura) = (lugar::largura(false), lugar::ALTURA);
    q.itens.push(caixa(Ret::new(0.0, 0.0, largura, altura), 0.0, FUNDO));
    let linha = linha_lida(e);
    if !linha.is_empty() && e.com_controles() {
        q.itens.push(texto(lugar::LINHA_LIDA_SEM_PREVIA, linha, F_MONO_12, TEXTO2).meio().item());
    }
    if let Some(a) = aviso_do_alto(e) {
        let r = lugar::AVISO_SEM_PREVIA;
        let tom = if e.com_controles() { Tom::Ambar } else { Tom::Info };
        q.itens.extend(aviso(Ret::new(r.x, r.y, r.l, altura_do_aviso(&a, r.l).min(r.a)), tom, &a));
    }
    if !e.com_controles() {
        return q;
    }
    let vivo = e.vivo();
    for (i, r) in lugar::abas(false).iter().enumerate() {
        q.controle(C::Aba(i), *r, true, aba.indice() == i);
    }
    let (x, mut y) = lugar::inicio(false);
    let l = lugar::COLUNA_L;
    let nota = |q: &mut QuadroDosAjustes, y: f32, s: &str| {
        q.texto(Ret::new(x, y, l, lugar::NOTA_A), s.to_string(), F_LEGENDA, TEXTO3);
    };
    let rotulo_e_valor = |q: &mut QuadroDosAjustes, y: f32, rotulo_: &str, valor: &str| {
        q.texto(Ret::new(x, y, l - lugar::VALOR_L, lugar::ROTULO_A), rotulo_.to_string(), F_CORPO_FORTE, TEXTO);
        q.itens.push(texto(Ret::new(x + l - lugar::VALOR_L, y, lugar::VALOR_L, lugar::ROTULO_A), valor.to_string(), F_MONO_13, TEXTO2).direita().meio().item());
    };
    let altura_do_deslizante = lugar::ROTULO_A + 4.0 + lugar::DESLIZANTE_A + 2.0 + lugar::NOTA_A + 8.0;
    match aba {
        Aba::Exposicao => {
            let modos = e.lista("exposicao");
            let auto = e.modo("exposicao") == "auto";
            let ops = lugar::opcoes(x, y, 220.0, 2);
            q.controle(C::Exposicao(0), ops[0], vivo && modos.iter().any(|m| m == "auto"), auto);
            q.controle(C::Exposicao(1), ops[1], vivo && modos.iter().any(|m| m == "manual"), !auto);
            y += lugar::OPCAO_A + lugar::ESPACO;
            // O EV (ou o brilho do Windows), só com Auto e destravado (R9 §3.3).
            let rotulo_ev = if e.e_brilho() { t("Brilho") } else { "EV" };
            let travada = e.ligado("travaExposicao");
            match deslizante(e, "ev") {
                Some((n, v, i, atual)) => {
                    rotulo_e_valor(&mut q, y, rotulo_ev, &texto_do_valor("ev", Some(&n), v.get(i).copied().unwrap_or(atual)));
                    q.deslizante(C::Brilho, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), vivo && auto && !travada, Degraus { max: v.len() as i32 - 1, pos: i as i32 });
                    if travada {
                        nota(&mut q, y + lugar::ROTULO_A + 4.0 + lugar::DESLIZANTE_A + 2.0, t(regras::FRASE_DESTRAVE));
                    }
                }
                None => {
                    rotulo_e_valor(&mut q, y, rotulo_ev, "");
                    nota(&mut q, y + lugar::ROTULO_A + 4.0, &frase_de(e, "ev"));
                }
            }
            y += altura_do_deslizante;
            let tem_trava = e.sim("travaExposicao");
            q.controle(C::TravarExposicao, Ret::new(x, y, l, lugar::INTERRUPTOR_A), vivo && tem_trava && auto, travada);
            if !tem_trava {
                nota(&mut q, y + lugar::INTERRUPTOR_A + 2.0, &frase_de(e, "travaExposicao"));
            }
            y += lugar::INTERRUPTOR_A + 2.0 + lugar::NOTA_A + 8.0;
            q.texto(Ret::new(x, y, l, lugar::ROTULO_A), t("Anti-cintilação"), F_CORPO_FORTE, TEXTO);
            y += lugar::ROTULO_A + 4.0;
            let lista = e.lista("antiCintilacao");
            let atual = e.ajuste.get("antiCintilacao").and_then(Value::as_str).unwrap_or("auto");
            for (i, (ret, o)) in lugar::opcoes(x, y, l, 4).into_iter().zip(OPCOES_DA_CINTILACAO).enumerate() {
                q.controle(C::Cintilacao(i), ret, vivo && lista.iter().any(|v| v == o), atual == o);
            }
            if lista.is_empty() {
                nota(&mut q, y + lugar::OPCAO_A + 2.0, &frase_de(e, "antiCintilacao"));
            }
        }
        Aba::GanhoEObturador => {
            let (iso, obt) = (deslizante(e, "iso"), deslizante(e, "obturadorNs"));
            let rotulo_iso = if e.e_ganho() { t("Ganho") } else { "ISO" };
            if iso.is_none() && obt.is_none() {
                // §3.5: um grupo em que nada se aplica mostra uma linha só.
                let grupo = if e.e_ganho() { "o ganho e o obturador" } else { "ISO e o obturador" }; // i18n: chave
                nota(&mut q, y, &frase_do_limite(e.limite("iso").or(e.limite("obturadorNs")).unwrap_or(""), grupo));
            } else if e.modo("exposicao") != "manual" {
                let frase = if e.e_ganho() { regras::FRASE_PASSE_PARA_MANUAL } else { "Passe a exposição para Manual para escolher ISO e obturador." }; // i18n: chave
                q.itens.push(texto(Ret::new(x, y, l, lugar::FRASE_A), t(frase), F_LEGENDA_13, TEXTO2).quebra().item());
                y += lugar::FRASE_A + lugar::ESPACO;
                q.controle(C::PassarParaManual, Ret::new(x, y, 190.0, 36.0), vivo && e.lista("exposicao").iter().any(|m| m == "manual"), false);
            } else {
                match iso {
                    Some((n, v, i, atual)) => {
                        rotulo_e_valor(&mut q, y, rotulo_iso, &texto_do_valor("iso", Some(&n), v.get(i).copied().unwrap_or(atual)));
                        q.deslizante(C::Ganho, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), vivo, Degraus { max: v.len() as i32 - 1, pos: i as i32 });
                    }
                    None => {
                        rotulo_e_valor(&mut q, y, rotulo_iso, "");
                        nota(&mut q, y + lugar::ROTULO_A + 4.0, &frase_de(e, "iso"));
                    }
                }
                y += altura_do_deslizante;
                match obt {
                    Some((n, v, i, atual)) => {
                        rotulo_e_valor(&mut q, y, t("Obturador"), &texto_do_valor("obturadorNs", Some(&n), v.get(i).copied().unwrap_or(atual)));
                        q.deslizante(C::Obturador, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), vivo, Degraus { max: v.len() as i32 - 1, pos: i as i32 });
                    }
                    None => {
                        rotulo_e_valor(&mut q, y, t("Obturador"), "");
                        nota(&mut q, y + lugar::ROTULO_A + 4.0, &frase_de(e, "obturadorNs"));
                    }
                }
            }
        }
        Aba::Balanco => {
            let lista = e.lista("balanco");
            let atual = e.modo("balanco");
            if lista.is_empty() {
                nota(&mut q, y, &frase_de(e, if e.limite("kelvin").is_some() { "kelvin" } else { "balanco" }));
                y += lugar::NOTA_A + lugar::ESPACO;
            } else {
                let w = (l - 12.0) / 3.0;
                for (i, b) in Balanco::GRADE.iter().enumerate() {
                    let ret = Ret::new(x + (i % 3) as f32 * (w + 6.0), y + (i / 3) as f32 * (lugar::OPCAO_A + 6.0), w, lugar::OPCAO_A);
                    q.controle(C::Balanco(i), ret, vivo && lista.iter().any(|v| v == no_fio(*b)), atual == no_fio(*b));
                }
                y += 2.0 * lugar::OPCAO_A + 6.0 + 2.0;
                if !lista.iter().any(|v| Balanco::GRADE.iter().any(|b| b.e_preset() && no_fio(*b) == v)) {
                    nota(&mut q, y, &frase_de(e, "balanco"));
                }
                y += lugar::NOTA_A + lugar::ESPACO;
            }
            if atual == "kelvin" {
                match deslizante(e, "kelvin") {
                    Some((n, v, i, a)) => {
                        rotulo_e_valor(&mut q, y, t("Kelvin"), &texto_do_valor("kelvin", Some(&n), v.get(i).copied().unwrap_or(a)));
                        q.deslizante(C::Kelvin, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), vivo, Degraus { max: v.len() as i32 - 1, pos: i as i32 });
                    }
                    None => nota(&mut q, y, &frase_de(e, "kelvin")),
                }
            } else {
                // Com Kelvin a trava de balanço não se aplica e some (R9 §3.4).
                let tem = e.sim("travaBalanco");
                q.controle(C::TravarBalanco, Ret::new(x, y, l, lugar::INTERRUPTOR_A), vivo && tem && atual == "auto", e.ligado("travaBalanco"));
                if !tem {
                    nota(&mut q, y + lugar::INTERRUPTOR_A + 2.0, &frase_de(e, "travaBalanco"));
                }
            }
        }
        Aba::Foco => {
            let lista = e.lista("foco");
            if lista.is_empty() {
                nota(&mut q, y, &frase_de(e, if e.limite("foco").is_some() { "foco" } else { "focoPosicao" }));
            } else {
                let atual = e.modo("foco");
                for (i, (ret, o)) in lugar::opcoes(x, y, 300.0, 3).into_iter().zip(OPCOES_DO_FOCO).enumerate() {
                    q.controle(C::Foco(i), ret, vivo && lista.iter().any(|v| v == o), atual == o);
                }
                y += lugar::OPCAO_A + 2.0;
                let posicao = deslizante(e, "focoPosicao");
                if posicao.is_none() {
                    nota(&mut q, y, &frase_de(e, "focoPosicao"));
                }
                y += lugar::NOTA_A + lugar::ESPACO;
                if let (Some((n, v, i, a)), "manual") = (posicao, atual) {
                    rotulo_e_valor(&mut q, y, t("Perto ↔ Longe"), &texto_do_valor("focoPosicao", Some(&n), v.get(i).copied().unwrap_or(a)));
                    q.deslizante(C::FocoPosicao, Ret::new(x, y + lugar::ROTULO_A + 4.0, l, lugar::DESLIZANTE_A), vivo, Degraus { max: v.len() as i32 - 1, pos: i as i32 });
                }
            }
        }
    }
    q.controle(C::Restaurar, lugar::restaurar(false), vivo, false);
    q
}

// =============================================================================================
// Os gestos
// =============================================================================================

/// **O que um controle pede**: a aba (a janela troca sozinha), um ajuste parcial com só o campo
/// mexido (§12, item 3), ou "Restaurar automático".
#[derive(Clone, Debug, PartialEq)]
pub enum GestoRemoto {
    Aba(Aba),
    Pedido(Value),
    Restaurar,
}

/// O número na forma do fio: inteiro sem `.0` quando não tem parte fracionária.
fn no_fio_numero(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9.0e15 {
        Value::from(x as i64)
    } else {
        json!(x)
    }
}

pub fn gesto(c: ControleDosAjustes, e: &EstadoRemoto, degrau: Option<i32>) -> Option<GestoRemoto> {
    use ControleDosAjustes as C;
    let pedido = |campo: &str, v: Value| Some(GestoRemoto::Pedido(json!({ campo: v })));
    let numero = |campo: &str| {
        let n = e.numero(campo)?;
        let v = degraus(campo, &n);
        let i = (degrau.unwrap_or(0).max(0) as usize).min(v.len().saturating_sub(1));
        Some(GestoRemoto::Pedido(json!({ campo: no_fio_numero(*v.get(i)?) })))
    };
    match c {
        C::Aba(i) => Some(GestoRemoto::Aba(*Aba::TODAS.get(i)?)),
        C::Exposicao(0) => pedido("exposicao", json!("auto")),
        // "O 'Passar para Manual' manda `exposicao` sozinho" (§12): a casca do filmador parte do lido.
        C::Exposicao(_) | C::PassarParaManual => pedido("exposicao", json!("manual")),
        C::Brilho => numero("ev"),
        C::TravarExposicao => pedido("travaExposicao", json!(!e.ligado("travaExposicao"))),
        C::Cintilacao(i) => pedido("antiCintilacao", json!(*OPCOES_DA_CINTILACAO.get(i)?)),
        C::Ganho => numero("iso"),
        C::Obturador => numero("obturadorNs"),
        C::Balanco(i) => pedido("balanco", json!(no_fio(*Balanco::GRADE.get(i)?))),
        C::Kelvin => numero("kelvin"),
        C::TravarBalanco => pedido("travaBalanco", json!(!e.ligado("travaBalanco"))),
        C::Foco(i) => pedido("foco", json!(*OPCOES_DO_FOCO.get(i)?)),
        C::FocoPosicao => numero("focoPosicao"),
        C::Restaurar => Some(GestoRemoto::Restaurar),
        // "Usar meus ajustes" é do filmador: o receptor não o oferece (07/10).
        C::PermitirRemoto | C::UsarMeusAjustes => None,
    }
}

/// O nome de um deslizante para o Narrador, com o valor (os botões usam o rótulo).
pub fn valor_acessivel(c: ControleDosAjustes, e: &EstadoRemoto) -> Option<String> {
    use ControleDosAjustes as C;
    let campo = match c {
        C::Brilho => "ev",
        C::Ganho => "iso",
        C::Obturador => "obturadorNs",
        C::Kelvin => "kelvin",
        C::FocoPosicao => "focoPosicao",
        _ => return None,
    };
    let (n, v, i, a) = deslizante(e, campo)?;
    Some(texto_do_valor(campo, Some(&n), v.get(i).copied().unwrap_or(a)))
}

/// O rótulo de um deslizante remoto (o "EV" e o "ISO" de quem não é Windows).
pub fn rotulo_do_deslizante(c: ControleDosAjustes, e: &EstadoRemoto) -> Option<&'static str> {
    match c {
        ControleDosAjustes::Brilho if !e.e_brilho() => Some("EV"),
        ControleDosAjustes::Ganho if !e.e_ganho() => Some("ISO"), // i18n: chave
        _ => None,
    }
}

// =============================================================================================
// Os exemplos (os testes)
// =============================================================================================

/// As capacidades do exemplo do contrato (§3.2): um Android com `MANUAL_SENSOR`.
pub fn capacidades_do_android() -> Value {
    json!({"plataforma":"android","nomeDaCamera":"Traseira", // i18n: fora (o exemplo do contrato)
      "controles":{
       "exposicao":{"valores":["auto","manual"]},
       "ev":{"min":-2.0,"max":2.0,"passo":0.1},
       "travaExposicao":{},
       "antiCintilacao":{"valores":["auto","50","60","desligada"]},
       "iso":{"min":50,"max":3200,"inteiro":true,"analogicoMax":800},
       "obturadorNs":{"min":100000,"max":33333333,"inteiro":true},
       "balanco":{"valores":["auto","incandescente","fluorescente","luzDoDia","nublado","kelvin"]},
       "kelvin":{"min":2000,"max":10000,"passo":100,"inteiro":true},
       "travaBalanco":{},
       "foco":{"valores":["auto","travado","manual"]},
       "focoPosicao":{"min":0.0,"max":1.0,"passo":0.01},
       "toque":{}}, // i18n: fora (o nome no fio)
      "limites":{}})
}

/// As do Mac (§3.2): só as travas e o ponto.
pub fn capacidades_do_mac() -> Value {
    json!({"plataforma":"macos","controles":{"travaExposicao":{},"travaBalanco":{},"foco":{"valores":["auto","travado"]},"toque":{}}, // i18n: fora (o nome no fio)
      "limites":{"ev":"macos","iso":"macos","obturadorNs":"macos","kelvin":"macos","antiCintilacao":"macos","focoPosicao":"macos"}})
}

/// Um estado de exemplo com estas capacidades e este ajuste.
pub fn estado_de_exemplo(situacao: &str, capacidades: Value, ajuste: Value) -> EstadoRemoto {
    EstadoRemoto::de_json(
        &json!({"situacao": situacao, "capacidades": capacidades, "ajuste": ajuste,
                "lido": {"iso": 400, "obturadorNs": 16666666, "kelvin": 5150, "abertura": 1.7, "divergentes": []}})
        .to_string(),
    )
}

#[cfg(test)]
mod testes;
