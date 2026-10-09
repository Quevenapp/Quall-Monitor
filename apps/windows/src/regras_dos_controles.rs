//! **Os controles de câmera do R9 no Windows: a parte que é aritmética**
//! (`docs/controles-de-camera.md` §2 e §3).
//!
//! Aqui não há Win32, no molde de `regras_da_camera.rs`: o que a câmera declara (`GetRange`) e o
//! que ela diz ter usado (`Get`) entram como números, e saem daqui o registro guardado, o que
//! mandar ao driver (o plano), o que devolver na soltura, os textos da tela e o que o roteiro de
//! bancada pede. Quem chama o `IAMCameraControl`, o `IAMVideoProcAmp` e o `IKsControl` é
//! `ajustes_da_camera.rs`, e ele só executa.
//!
//! # O que o Windows tem, e o nome que cada coisa leva (§1, §3.1–§3.4)
//!
//! | propriedade | interface e id | na tela | no registro (§2) |
//! |---|---|---|---|
//! | exposição | `CameraControl_Exposure` (4), log2 de segundos, só inteiros | "Obturador", `1/2^-v s` | `obturadorNs` (e `travaObturadorNs`) |
//! | ganho | `VideoProcAmp_Gain` (9), unidade do driver | "Ganho" | `iso` (e `travaIso`) |
//! | brilho | `VideoProcAmp_Brightness` (0), unidade do driver | "Brilho" | `ev`, **como deslocamento do padrão do driver** |
//! | balanço | `VideoProcAmp_WhiteBalance` (7), Kelvin | "Kelvin" | `kelvin` (e `travaGanhos = [K]`) |
//! | foco | `CameraControl_Focus` (6), unidade do driver | "Perto ↔ Longe" | `focoPosicao`, 0 = o primeiro valor da faixa |
//! | anti-cintilação | `IKsControl` id 13 de `PROPSETID_VIDCAP_VIDEOPROCAMP` | "Anti-cintilação" | `antiCintilacao` |
//!
//! **Os campos são os do §2, com a grafia literal**, e o Windows não inventa campo novo. Duas
//! leituras que a especificação deixa para a plataforma, escritas aqui para não serem adivinhadas:
//!
//! - `iso` guarda o **ganho** na unidade do driver: é o equivalente que o §3.2 nomeia;
//! - `ev` guarda o **brilho** como deslocamento a partir do padrão do `GetRange`, na unidade do
//!   driver: o `0` do §2 continua querendo dizer "não compensar" (e aqui, "não tocar no brilho"), e
//!   o "Restaurar automático" volta o brilho ao que a câmera tinha. O texto na tela é o número cru
//!   do driver, e nunca "EV" (§3.3: "Escrever 'EV' seria mentira").
//!
//! # "Auto" quer dizer "devolver", e não "mandar Auto"
//!
//! A câmera UVC guarda o ajuste no firmware, e ele vale para o próximo app (§2.2). Então o Quall
//! só manda ao driver o que o registro pede **fora do automático**, e devolve (`Flags_Auto`, ou o
//! valor que a câmera tinha quando abriu, para o que não tem Auto) só o que **ele mesmo** mudou
//! nesta abertura. Uma câmera que outro app deixou em manual, com o registro do Quall todo em Auto,
//! fica como estava. É o mesmo princípio do Android ("o Quall devolve a câmera como encontrou").

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

// =============================================================================================
// As propriedades do driver
// =============================================================================================

/// As seis propriedades que o R9 mexe no Windows, **na ordem em que são enviadas**: a exposição
/// antes do ganho (há driver que ignora o ganho com a exposição automática), o resto depois.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Propriedade {
    Exposicao,
    Ganho,
    Brilho,
    Balanco,
    Foco,
    AntiCintilacao,
}

/// Qual das duas interfaces do DirectShow tem a propriedade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interface {
    /// `IAMCameraControl` (`CameraControlProperty`).
    CameraControl,
    /// `IAMVideoProcAmp` (`VideoProcAmpProperty`). A anti-cintilação vai primeiro pelo
    /// `IKsControl`, com o mesmo id (§6).
    VideoProcAmp,
}

/// `CameraControl_Flags_Auto` e `VideoProcAmp_Flags_Auto` (os dois valem 1), e os `Manual` (2).
pub const FLAGS_AUTO: i32 = 1;
pub const FLAGS_MANUAL: i32 = 2;
/// `KSPROPERTY_VIDEOPROCAMP_POWERLINE_FREQUENCY` (`ksmedia.h`), o id da anti-cintilação.
pub const ID_DA_ANTI_CINTILACAO: i32 = 13;

impl Propriedade {
    pub const TODAS: [Propriedade; 6] =
        [Propriedade::Exposicao, Propriedade::Ganho, Propriedade::Brilho, Propriedade::Balanco, Propriedade::Foco, Propriedade::AntiCintilacao];

    /// A interface e o id da propriedade nela.
    pub fn onde(self) -> (Interface, i32) {
        match self {
            Propriedade::Exposicao => (Interface::CameraControl, 4),
            Propriedade::Foco => (Interface::CameraControl, 6),
            Propriedade::Brilho => (Interface::VideoProcAmp, 0),
            Propriedade::Balanco => (Interface::VideoProcAmp, 7),
            Propriedade::Ganho => (Interface::VideoProcAmp, 9),
            Propriedade::AntiCintilacao => (Interface::VideoProcAmp, ID_DA_ANTI_CINTILACAO),
        }
    }

    /// O nome curto, para o diário e para o roteiro de bancada.
    pub fn chave(self) -> &'static str {
        match self {
            Propriedade::Exposicao => "obturador",
            Propriedade::Ganho => "ganho",
            Propriedade::Brilho => "brilho",
            Propriedade::Balanco => "kelvin",
            Propriedade::Foco => "foco",
            Propriedade::AntiCintilacao => "cintilacao",
        }
    }

    pub fn da_chave(s: &str) -> Option<Propriedade> {
        Propriedade::TODAS.into_iter().find(|p| p.chave() == s)
    }

    /// **O `{controle}` das frases do §3.5**, com o artigo. Em português: é a chave da tradução, e
    /// [`frase_sem_controle`] o traduz (o diário fica com o português).
    pub fn nome(self) -> &'static str {
        match self {
            Propriedade::Exposicao => "o obturador", // i18n: chave
            Propriedade::Ganho => "o ganho", // i18n: chave
            Propriedade::Brilho => "o brilho", // i18n: chave
            Propriedade::Balanco => "o Kelvin", // i18n: chave
            Propriedade::Foco => "o foco manual", // i18n: chave
            Propriedade::AntiCintilacao => "a anti-cintilação", // i18n: chave
        }
    }
}

// =============================================================================================
// A faixa (o `GetRange`)
// =============================================================================================

/// **O que o `GetRange` declara**: mínimo, máximo, passo, padrão e as bandeiras que a propriedade
/// aceita (`Flags_Auto`, `Flags_Manual`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Faixa {
    pub min: i32,
    pub max: i32,
    pub passo: i32,
    pub padrao: i32,
    pub bandeiras: i32,
}

impl Faixa {
    /// A faixa de um `GetRange` que voltou com sucesso, ou `None` quando ela não faz sentido (o
    /// máximo abaixo do mínimo). Um passo de zero ou negativo vira 1: há driver que declara 0.
    pub fn do_get_range(min: i32, max: i32, passo: i32, padrao: i32, bandeiras: i32) -> Option<Faixa> {
        if max < min {
            return None;
        }
        Some(Faixa { min, max, passo: passo.max(1), padrao, bandeiras })
    }

    /// A propriedade aceita `Flags_Auto`?
    pub fn tem_auto(&self) -> bool {
        self.bandeiras & FLAGS_AUTO != 0
    }

    /// A propriedade aceita `Flags_Manual`? Uma faixa sem bandeira nenhuma é tratada como manual: o
    /// driver respondeu ao `GetRange` com números, e o `Set` é a prova.
    pub fn tem_manual(&self) -> bool {
        self.bandeiras & FLAGS_MANUAL != 0 || self.bandeiras & (FLAGS_AUTO | FLAGS_MANUAL) == 0
    }

    /// Quantos passos cabem da ponta de baixo à de cima.
    pub fn passos(&self) -> i32 {
        ((i64::from(self.max) - i64::from(self.min)) / i64::from(self.passo)) as i32
    }

    /// **O valor cortado pela faixa e arredondado ao passo** contado do mínimo (§2.2: "todo valor é
    /// cortado pela faixa daquele instante"). O último degrau é o maior que não passa do máximo.
    pub fn cortar(&self, v: i64) -> i32 {
        let min = i64::from(self.min);
        let passo = i64::from(self.passo);
        let v = v.clamp(min, i64::from(self.max));
        let k = ((v - min) as f64 / passo as f64).round() as i64;
        let k = k.clamp(0, i64::from(self.passos()));
        (min + k * passo) as i32
    }

    /// O degrau de um valor (0 é o mínimo).
    pub fn indice(&self, v: i64) -> i32 {
        ((i64::from(self.cortar(v)) - i64::from(self.min)) / i64::from(self.passo)) as i32
    }

    /// O valor do degrau `i`.
    pub fn do_indice(&self, i: i64) -> i32 {
        self.cortar(i64::from(self.min) + i * i64::from(self.passo))
    }

    /// A posição de `v` entre o mínimo (0) e o máximo (1).
    pub fn fracao(&self, v: i64) -> f64 {
        if self.max == self.min {
            return 0.0;
        }
        (self.cortar(v) - self.min) as f64 / (self.max - self.min) as f64
    }

    /// O valor na posição `f` (0 o mínimo, 1 o máximo), cortado ao passo.
    pub fn da_fracao(&self, f: f64) -> i32 {
        let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
        self.cortar(self.min as i64 + (f * (self.max as f64 - self.min as f64)).round() as i64)
    }
}

/// O que se leu de uma propriedade (`Get`): o valor e as bandeiras.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lido {
    pub valor: i32,
    pub bandeiras: i32,
}

impl Lido {
    pub fn em_auto(&self) -> bool {
        self.bandeiras & FLAGS_AUTO != 0
    }
}

/// As faixas que a câmera declarou, por propriedade. Uma propriedade ausente é uma que a câmera
/// não oferece (§3.5).
pub type Capacidades = BTreeMap<Propriedade, Faixa>;
/// O que se leu de cada propriedade.
pub type Lidos = BTreeMap<Propriedade, Lido>;

// =============================================================================================
// O obturador (§3.1, no Windows)
// =============================================================================================

/// **O texto do obturador** num valor de `CameraControl_Exposure` (log2 de segundos): `1/2^-v s`
/// abaixo de 1 s (`1/32 s`, `1/64 s`…), `2^v s` de 1 s para cima.
pub fn texto_do_obturador(log2: i32) -> String {
    if log2 < 0 {
        format!("1/{} s", 1u64 << (-i64::from(log2)).min(62))
    } else {
        format!("{} s", 1u64 << i64::from(log2).min(62))
    }
}

/// **O teto do obturador** no fps negociado: `floor(log2(1/fps))`. −5 a 30 fps, −6 a 60 fps. Sem
/// fps (zero ou absurdo), sem teto.
pub fn teto_do_obturador(fps: f64) -> i32 {
    if !(fps.is_finite() && fps > 0.0) {
        return i32::MAX;
    }
    // Um fps de 29,97 dá log2 = −4,905…: o piso é −5, como a 30.
    (1.0 / fps).log2().floor() as i32
}

/// A faixa do obturador **com o teto de 1/fps** (§3.1): o máximo é `min(máximo da câmera, teto)`.
/// Uma câmera cujo mínimo já passa do teto fica só com o mínimo (não há valor dentro dos dois).
pub fn faixa_do_obturador(f: &Faixa, fps: f64) -> Faixa {
    let teto = teto_do_obturador(fps);
    let max = f.max.min(teto).max(f.min);
    Faixa { max, ..*f }
}

/// A duração em ns de um valor em log2 de segundos.
pub fn ns_do_log2(v: i32) -> i64 {
    (1e9 * 2f64.powi(v)).round() as i64
}

/// O log2 de segundos mais perto de uma duração em ns. Zero ou negativo: o menor que existe.
pub fn log2_dos_ns(ns: i64) -> i32 {
    if ns <= 0 {
        return i32::MIN / 2;
    }
    (ns as f64 / 1e9).log2().round() as i32
}

// =============================================================================================
// Kelvin e anti-cintilação (§2, §3.4, §6)
// =============================================================================================

pub const KELVIN_MIN: i32 = 2000;
pub const KELVIN_MAX: i32 = 10000;
pub const KELVIN_PASSO: i32 = 100;

/// **A escala do Kelvin na tela**: de 2000 a 10000, de 100 em 100 (§2), dentro da faixa do driver.
/// O passo é o do driver quando ele for maior (arredondado a 100). Um driver cuja faixa não encosta
/// em 2000–10000 fica com a faixa dele, cortada ao passo dele.
pub fn faixa_do_kelvin(f: &Faixa) -> Faixa {
    let passo = ((f.passo + KELVIN_PASSO - 1) / KELVIN_PASSO * KELVIN_PASSO).max(KELVIN_PASSO);
    let min = f.min.max(KELVIN_MIN);
    let min = (min + KELVIN_PASSO - 1) / KELVIN_PASSO * KELVIN_PASSO;
    let max = f.max.min(KELVIN_MAX) / KELVIN_PASSO * KELVIN_PASSO;
    if max < min {
        return *f;
    }
    let max = min + (max - min) / passo * passo;
    Faixa { min, max, passo, ..*f }
}

/// O Kelvin "estimado no momento de passar a Kelvin" (§2): o lido, na escala da tela.
pub fn kelvin_da_tela(v: i64) -> i32 {
    let v = v.clamp(i64::from(KELVIN_MIN), i64::from(KELVIN_MAX));
    (((v as f64) / f64::from(KELVIN_PASSO)).round() as i32) * KELVIN_PASSO
}

/// `antiCintilacao` (§2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AntiCintilacao {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "50")]
    Hz50,
    #[serde(rename = "60")]
    Hz60,
    #[serde(rename = "desligada")]
    Desligada,
}

impl AntiCintilacao {
    pub const TODAS: [AntiCintilacao; 4] = [AntiCintilacao::Auto, AntiCintilacao::Hz50, AntiCintilacao::Hz60, AntiCintilacao::Desligada];

    /// O rótulo em português (o diário o usa assim; a tela passa por `idioma::t`).
    pub fn rotulo(self) -> &'static str {
        match self {
            AntiCintilacao::Auto => "Auto", // i18n: chave
            AntiCintilacao::Hz50 => "50 Hz",
            AntiCintilacao::Hz60 => "60 Hz",
            AntiCintilacao::Desligada => "Desligada", // i18n: chave
        }
    }

    /// **O valor do `POWERLINE_FREQUENCY`** (§6): 0 desligada, 1 50 Hz, 2 60 Hz, 3 auto — este só
    /// em UVC 1.5 (a faixa vai até 3). Sem o 3, "Auto" quer dizer "não tocar": `None`.
    pub fn valor_do_driver(self, faixa: Option<&Faixa>) -> Option<i32> {
        match self {
            AntiCintilacao::Desligada => Some(0),
            AntiCintilacao::Hz50 => Some(1),
            AntiCintilacao::Hz60 => Some(2),
            AntiCintilacao::Auto => faixa.filter(|f| f.max >= 3 && f.min <= 3).map(|_| 3),
        }
    }

    /// O que um valor lido do driver quer dizer.
    pub fn do_valor(v: i32) -> Option<AntiCintilacao> {
        match v {
            0 => Some(AntiCintilacao::Desligada),
            1 => Some(AntiCintilacao::Hz50),
            2 => Some(AntiCintilacao::Hz60),
            3 => Some(AntiCintilacao::Auto),
            _ => None,
        }
    }

    /// A opção existe nesta câmera? "Auto" sempre (no pior caso, "não tocar"); as outras, se o
    /// valor cabe na faixa.
    pub fn oferecida(self, faixa: &Faixa) -> bool {
        match self.valor_do_driver(Some(faixa)) {
            None => self == AntiCintilacao::Auto,
            Some(v) => v >= faixa.min && v <= faixa.max,
        }
    }
}

// =============================================================================================
// O registro (§2)
// =============================================================================================

/// `exposicao`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ModoDeExposicao {
    #[default]
    Auto,
    Manual,
}

/// `balanco`. Os quatro presets existem no registro (o formato é o das quatro plataformas), e o
/// Windows não os oferece (§1): a tela os mostra apagados, e o plano não os manda.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Balanco {
    #[default]
    Auto,
    Incandescente,
    Fluorescente,
    LuzDoDia,
    Nublado,
    Kelvin,
}

impl Balanco {
    /// A grade de 2 × 3, na ordem da tela (§4.2).
    pub const GRADE: [Balanco; 6] = [Balanco::Auto, Balanco::Incandescente, Balanco::Fluorescente, Balanco::LuzDoDia, Balanco::Nublado, Balanco::Kelvin];

    /// O rótulo em português (a chave da tradução: a tela passa por `idioma::t`).
    pub fn rotulo(self) -> &'static str {
        match self {
            Balanco::Auto => "Auto", // i18n: chave
            Balanco::Incandescente => "Incandescente", // i18n: chave
            Balanco::Fluorescente => "Fluorescente", // i18n: chave
            Balanco::LuzDoDia => "Luz do dia", // i18n: chave
            Balanco::Nublado => "Nublado", // i18n: chave
            Balanco::Kelvin => "Kelvin", // i18n: chave
        }
    }

    pub fn e_preset(self) -> bool {
        matches!(self, Balanco::Incandescente | Balanco::Fluorescente | Balanco::LuzDoDia | Balanco::Nublado)
    }
}

/// `foco`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Foco {
    #[default]
    Auto,
    Travado,
    Manual,
}

impl Foco {
    pub const TODOS: [Foco; 3] = [Foco::Auto, Foco::Travado, Foco::Manual];

    /// O rótulo em português (a chave da tradução: a tela passa por `idioma::t`).
    pub fn rotulo(self) -> &'static str {
        match self {
            Foco::Auto => "Auto", // i18n: chave
            Foco::Travado => "Travado", // i18n: chave
            Foco::Manual => "Manual", // i18n: chave
        }
    }
}

/// **O registro de uma câmera** (§2), com os nomes literais em snake_case e o JSON em camelCase.
/// Ver o cabeçalho do módulo para o que `ev` e `iso` guardam no Windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Registro {
    pub exposicao: ModoDeExposicao,
    /// O brilho, como deslocamento do padrão do driver, na unidade dele (ver o cabeçalho).
    pub ev: f64,
    pub trava_exposicao: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trava_iso: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trava_obturador_ns: Option<i64>,
    /// O ganho, na unidade do driver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iso: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obturador_ns: Option<i64>,
    pub anti_cintilacao: AntiCintilacao,
    pub balanco: Balanco,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kelvin: Option<i32>,
    pub trava_balanco: bool,
    /// No Windows, um número só: o Kelvin lido ao travar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trava_ganhos: Option<Vec<f64>>,
    pub foco: Foco,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foco_posicao: Option<f64>,
}

impl Registro {
    /// Tudo no padrão da tabela do §2 ("Restaurar automático").
    pub fn e_padrao(&self) -> bool {
        *self == Registro::default()
    }
}

/// O arquivo inteiro: link → registro (§2, "mapa de link para registro").
pub type Mapa = BTreeMap<String, Registro>;

/// **Lê o mapa** do texto do arquivo. Tolerante: um arquivo ilegível vira mapa vazio, e uma entrada
/// ilegível é pulada sem levar as outras junto.
pub fn ler_mapa(texto: &str) -> Mapa {
    let texto = texto.trim_start_matches('\u{FEFF}');
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(texto) else {
        return Mapa::new();
    };
    obj.into_iter().filter_map(|(k, v)| serde_json::from_value::<Registro>(v).ok().map(|r| (k, r))).collect()
}

pub fn mapa_em_json(m: &Mapa) -> String {
    serde_json::to_string_pretty(m).unwrap_or_else(|_| "{}".into())
}

/// O registro de uma câmera (o link comparado sem caixa, como no resto do app), ou o padrão.
pub fn registro_de(m: &Mapa, link: &str) -> Registro {
    m.iter().find(|(k, _)| k.eq_ignore_ascii_case(link)).map(|(_, r)| r.clone()).unwrap_or_default()
}

/// Guarda o registro de uma câmera, trocando a entrada que já houver (sem caixa). **O registro no
/// padrão não é gravado** (07/10, "abrir no automático e lembrar o último manual"): a entrada guarda
/// "meus ajustes", e voltar ao automático não a apaga.
pub fn guardar_em(m: &mut Mapa, link: &str, r: &Registro) {
    if r.e_padrao() {
        return;
    }
    m.retain(|k, _| !k.eq_ignore_ascii_case(link));
    m.insert(link.to_string(), r.clone());
}

/// **Abrir no automático e lembrar o último manual** (decisão do Bruno, 07/10). Antes, o registro
/// guardado era reaplicado a cada abertura, e uma câmera deixada em manual abria escura no dia
/// seguinte. A abertura usa o padrão; o guardado vira "meus ajustes", oferecido no painel enquanto
/// for diferente do que vale.
pub fn meus_ajustes(guardado: Registro) -> Option<Registro> {
    (!guardado.e_padrao()).then_some(guardado)
}

/// O painel mostra "Usar meus ajustes"?
pub fn oferece_meus_ajustes(meus: Option<&Registro>, corrente: &Registro) -> bool {
    meus.is_some_and(|m| !m.e_padrao() && m != corrente)
}

// =============================================================================================
// O plano: o que mandar ao driver
// =============================================================================================

/// Um `Set`: a propriedade, o valor e as bandeiras.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Envio {
    pub prop: Propriedade,
    pub valor: i32,
    pub bandeiras: i32,
}

/// **O valor manual que o registro pede** para cada propriedade, já cortado pela faixa de agora
/// (§2.2), ou nada ("Auto": não tocar, ou devolver). Só as propriedades que a câmera declarou.
pub fn desejado(r: &Registro, caps: &Capacidades, fps: f64) -> BTreeMap<Propriedade, i32> {
    let mut d = BTreeMap::new();
    for (p, f) in caps {
        let v = match p {
            Propriedade::Exposicao => {
                let ns = match (r.exposicao, r.trava_exposicao) {
                    (ModoDeExposicao::Manual, _) => r.obturador_ns,
                    (ModoDeExposicao::Auto, true) => r.trava_obturador_ns,
                    _ => None,
                };
                ns.map(|ns| faixa_do_obturador(f, fps).cortar(i64::from(log2_dos_ns(ns))))
            }
            Propriedade::Ganho => {
                let g = match (r.exposicao, r.trava_exposicao) {
                    (ModoDeExposicao::Manual, _) => r.iso,
                    (ModoDeExposicao::Auto, true) => r.trava_iso,
                    _ => None,
                };
                g.map(|g| f.cortar(g.round() as i64))
            }
            Propriedade::Brilho => (r.ev.round() as i64 != 0).then(|| f.cortar(i64::from(f.padrao) + r.ev.round() as i64)),
            Propriedade::Balanco => match (r.balanco, r.trava_balanco) {
                (Balanco::Kelvin, _) => r.kelvin.map(|k| f.cortar(i64::from(k))),
                (Balanco::Auto, true) => r.trava_ganhos.as_ref().and_then(|g| g.first()).map(|k| f.cortar(k.round() as i64)),
                _ => None,
            },
            Propriedade::Foco => match r.foco {
                Foco::Manual | Foco::Travado => r.foco_posicao.map(|x| f.da_fracao(x)),
                Foco::Auto => None,
            },
            Propriedade::AntiCintilacao => r.anti_cintilacao.valor_do_driver(Some(f)).map(|v| f.cortar(i64::from(v))),
        };
        if let Some(v) = v {
            if *p == Propriedade::AntiCintilacao || f.tem_manual() {
                d.insert(*p, v);
            }
        }
    }
    d
}

/// **A devolução de uma propriedade** que o Quall mudou (§2.2): `Flags_Auto` onde a propriedade
/// tem Auto; senão, o valor e as bandeiras que a câmera tinha quando abriu (ou o padrão do driver,
/// se nada foi lido).
pub fn devolucao(p: Propriedade, caps: &Capacidades, originais: &Lidos) -> Option<Envio> {
    let f = caps.get(&p)?;
    let original = originais.get(&p);
    if f.tem_auto() && p != Propriedade::AntiCintilacao {
        return Some(Envio { prop: p, valor: original.map(|o| o.valor).unwrap_or(f.padrao), bandeiras: FLAGS_AUTO });
    }
    match original {
        Some(o) => Some(Envio { prop: p, valor: o.valor, bandeiras: if o.bandeiras == 0 { FLAGS_MANUAL } else { o.bandeiras } }),
        None => Some(Envio { prop: p, valor: f.padrao, bandeiras: FLAGS_MANUAL }),
    }
}

/// **O plano**: o que mandar para o driver ficar como o registro pede, na ordem de
/// [`Propriedade::TODAS`]. O que o registro quer em manual vai com o valor; o que ele quer em Auto
/// e o Quall tinha mudado (`tocados`) é devolvido; o resto não é tocado.
pub fn plano(r: &Registro, caps: &Capacidades, fps: f64, originais: &Lidos, tocados: &BTreeSet<Propriedade>) -> Vec<Envio> {
    let d = desejado(r, caps, fps);
    let mut v = Vec::new();
    for p in Propriedade::TODAS {
        match d.get(&p) {
            Some(valor) => v.push(Envio { prop: p, valor: *valor, bandeiras: FLAGS_MANUAL }),
            None if tocados.contains(&p) => v.extend(devolucao(p, caps, originais)),
            None => {}
        }
    }
    v
}

/// **A abertura em automático de verdade** (§2.2, achado de 06/10): o que o registro quer em Auto e o
/// driver **não** está em Auto vai com `Flags_Auto` (com o valor lido, que o driver aceita); o brilho
/// e o ganho sem Auto, fora do padrão do driver, voltam ao padrão. A anti-cintilação fica como está.
///
/// Sem isto, o plano só devolvia o que o próprio Quall tinha mudado (`tocados`). Se outro app ou uma
/// sessão interrompida deixava a webcam em manual no mínimo, o Quall mostrava "Auto", não mandava nada
/// e a imagem saía escura (registro padrão + driver manual no mínimo = zero `Set`).
pub fn normalizacao_da_abertura(r: &Registro, caps: &Capacidades, fps: f64, originais: &Lidos) -> Vec<Envio> {
    let d = desejado(r, caps, fps);
    let mut v = Vec::new();
    for p in Propriedade::TODAS {
        if d.contains_key(&p) || p == Propriedade::AntiCintilacao {
            continue;
        }
        let Some(f) = caps.get(&p) else { continue };
        let o = originais.get(&p);
        if f.tem_auto() {
            if o.is_none_or(|o| o.bandeiras & FLAGS_AUTO == 0) {
                v.push(Envio { prop: p, valor: o.map(|o| o.valor).unwrap_or(f.padrao), bandeiras: FLAGS_AUTO });
            }
        } else if matches!(p, Propriedade::Brilho | Propriedade::Ganho) && o.is_some_and(|o| o.valor != f.padrao) {
            // O ganho sem Auto também (a webcam do Dell, 07/10): no mínimo, a imagem seguia escura com
            // o obturador já em Auto.
            v.push(Envio { prop: p, valor: f.padrao, bandeiras: FLAGS_MANUAL });
        }
    }
    v
}

/// O "como a câmera abriu" depois da [`normalizacao_da_abertura`]: é a ele que a devolução volta
/// (§2.2). Devolver a câmera ao manual escuro que outro app deixou seria refazer o defeito no fechar.
pub fn originais_depois_da_normalizacao(originais: &Lidos, n: &[Envio]) -> Lidos {
    let mut o = originais.clone();
    for e in n {
        o.insert(e.prop, Lido { valor: e.valor, bandeiras: e.bandeiras });
    }
    o
}

/// **Tudo o que a soltura devolve**: cada propriedade que o Quall mudou nesta abertura.
pub fn devolver_tudo(caps: &Capacidades, originais: &Lidos, tocados: &BTreeSet<Propriedade>) -> Vec<Envio> {
    Propriedade::TODAS.into_iter().filter(|p| tocados.contains(p)).filter_map(|p| devolucao(p, caps, originais)).collect()
}

/// Depois de um envio que deu certo: a propriedade passa a ser do Quall (manual) ou volta a não
/// ser (devolvida).
pub fn marcar_tocado(tocados: &mut BTreeSet<Propriedade>, e: &Envio, devolvida: bool) {
    if devolvida {
        tocados.remove(&e.prop);
    } else {
        tocados.insert(e.prop);
    }
}

// =============================================================================================
// O que a tela pede (§4.3), sobre o registro
// =============================================================================================

/// Um gesto da tela (ou da bancada) sobre o registro.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Acao {
    Exposicao(ModoDeExposicao),
    /// O brilho na unidade do driver (valor absoluto).
    Brilho(i32),
    TravarExposicao(bool),
    AntiCintilacao(AntiCintilacao),
    /// O ganho na unidade do driver.
    Ganho(i32),
    /// O obturador em log2 de segundos.
    Obturador(i32),
    Balanco(Balanco),
    Kelvin(i32),
    TravarBalanco(bool),
    Foco(Foco),
    /// 0,0 a 1,0, de 0,01 em 0,01.
    FocoPosicao(f64),
    Restaurar,
    /// "Usar meus ajustes" (07/10): a thread troca o registro pelo guardado; [`aplicar_acao`] não o
    /// conhece.
    UsarMeusAjustes,
}

/// O foco na escala do registro: de 0,01 em 0,01.
fn em_centesimos(f: f64) -> f64 {
    (f.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

/// **Aplica um gesto ao registro**, com o que a câmera diz agora (`lidos`) para os gestos que partem
/// do valor de agora (§2.1, §4.3). Devolve se o registro mudou.
pub fn aplicar_acao(r: &mut Registro, a: Acao, caps: &Capacidades, lidos: &Lidos) -> bool {
    let antes = r.clone();
    let lido = |p: Propriedade| lidos.get(&p).map(|l| l.valor);
    match a {
        Acao::Exposicao(ModoDeExposicao::Manual) => {
            if r.exposicao != ModoDeExposicao::Manual {
                // "Ao passar, ISO e obturador partem dos valores lidos naquele instante."
                r.exposicao = ModoDeExposicao::Manual;
                r.trava_exposicao = false;
                r.trava_iso = None;
                r.trava_obturador_ns = None;
                r.iso = lido(Propriedade::Ganho).map(f64::from).or(r.iso);
                r.obturador_ns = lido(Propriedade::Exposicao).map(ns_do_log2).or(r.obturador_ns);
            }
        }
        Acao::Exposicao(ModoDeExposicao::Auto) => r.exposicao = ModoDeExposicao::Auto,
        Acao::Brilho(v) => {
            if let Some(f) = caps.get(&Propriedade::Brilho) {
                r.ev = f64::from(f.cortar(i64::from(v)) - f.padrao);
            }
        }
        Acao::TravarExposicao(true) => {
            if r.exposicao == ModoDeExposicao::Auto {
                r.trava_exposicao = true;
                r.trava_iso = lido(Propriedade::Ganho).map(f64::from);
                r.trava_obturador_ns = lido(Propriedade::Exposicao).map(ns_do_log2);
            }
        }
        Acao::TravarExposicao(false) => {
            r.trava_exposicao = false;
            r.trava_iso = None;
            r.trava_obturador_ns = None;
        }
        Acao::AntiCintilacao(x) => r.anti_cintilacao = x,
        Acao::Ganho(v) => r.iso = Some(f64::from(v)),
        Acao::Obturador(v) => r.obturador_ns = Some(ns_do_log2(v)),
        Acao::Balanco(Balanco::Kelvin) => {
            if r.balanco != Balanco::Kelvin {
                r.balanco = Balanco::Kelvin;
                r.trava_balanco = false;
                r.trava_ganhos = None;
                r.kelvin = lido(Propriedade::Balanco).map(|k| kelvin_da_tela(i64::from(k))).or(r.kelvin);
            }
        }
        // Os presets não existem no Windows (§1): o gesto não muda nada.
        Acao::Balanco(b) if b.e_preset() => {}
        Acao::Balanco(b) => r.balanco = b,
        Acao::Kelvin(k) => r.kelvin = Some(k),
        Acao::TravarBalanco(true) => {
            if r.balanco == Balanco::Auto {
                r.trava_balanco = true;
                r.trava_ganhos = lido(Propriedade::Balanco).map(|k| vec![f64::from(k)]);
            }
        }
        Acao::TravarBalanco(false) => {
            r.trava_balanco = false;
            r.trava_ganhos = None;
        }
        Acao::Foco(Foco::Auto) => r.foco = Foco::Auto,
        Acao::Foco(x) => {
            // Travar guarda a posição lida (§2.1); passar a Manual parte dela.
            let pos = match (lido(Propriedade::Foco), caps.get(&Propriedade::Foco)) {
                (Some(v), Some(f)) => Some(em_centesimos(f.fracao(i64::from(v)))),
                _ => None,
            };
            if x == Foco::Travado || r.foco != Foco::Manual {
                r.foco_posicao = pos.or(r.foco_posicao);
            }
            r.foco = x;
        }
        Acao::FocoPosicao(f) => r.foco_posicao = Some(em_centesimos(f)),
        Acao::Restaurar => *r = Registro::default(),
        Acao::UsarMeusAjustes => {}
    }
    *r != antes
}

// =============================================================================================
// O que a tela lê
// =============================================================================================

/// Em que pé estão os ajustes de uma câmera aberta.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum FaseDosAjustes {
    /// A thread de trabalho ainda está lendo as faixas.
    #[default]
    Lendo,
    Pronto,
    /// **`Modo::Compartilhada`**: outro app controla a câmera, e os controles ficam apagados com o
    /// texto do §3.5 até se medir se o compartilhado aceita `Set` (§6).
    Compartilhada,
    /// **Pelo tipo da fonte** (§2.2): a câmera virtual do próprio Quall. A tela não oferece ajustes.
    SemControles,
}

/// **O que a thread dos ajustes publica para a tela**: a fase, o que a câmera declara e diz ter
/// usado, o registro de agora e as duas linhas do alto (§3.6).
#[derive(Clone, Debug, Default)]
pub struct PainelDosAjustes {
    pub fase: FaseDosAjustes,
    pub caps: Capacidades,
    pub lidos: Lidos,
    pub registro: Registro,
    /// O fps do tipo nativo (o teto do obturador, §3.1).
    pub fps: f64,
    pub linha_lida: String,
    pub divergencia: Option<String>,
    /// **R9b**: o aparelho que mexeu nesta câmera de longe, nos 4 s depois (o `controlado_por` do
    /// filmador do núcleo). A janela dos ajustes, a principal e a tela R5 o mostram.
    pub controlado_por: Option<String>,
    /// **Pouca luz** (§3.1): o automático baixou o fps para clarear, pelo vigia do fps que chega.
    /// Guardado como números, e não como frase: quem mostra a monta no idioma da hora, inteira (a
    /// janela dos ajustes e a tela R5) ou curta (a linha da gravação da janela principal).
    pub pouca_luz: Option<PoucaLuz>,
    /// "Meus ajustes" desta câmera (07/10): o último manual guardado, oferecido no painel.
    pub meus_ajustes: Option<Registro>,
}

// =============================================================================================
// O que a câmera diz ter usado (§3.6)
// =============================================================================================

/// O texto de um valor lido ou pedido, na unidade da tela.
pub fn texto_do_valor(p: Propriedade, v: i32) -> String {
    match p {
        Propriedade::Exposicao => texto_do_obturador(v),
        Propriedade::Balanco => format!("{v} K"),
        Propriedade::AntiCintilacao => AntiCintilacao::do_valor(v).map(|a| a.rotulo().to_string()).unwrap_or_else(|| v.to_string()),
        _ => v.to_string(),
    }
}

/// **A linha do alto do painel** (§3.6): só o que o Windows lê, separado por " · ". O ganho vai
/// com o nome ("Ganho 64"), porque não é ISO. No idioma de agora: a thread dos ajustes a refaz a
/// cada leitura (4 vezes por segundo), e uma troca de idioma aparece na leitura seguinte.
pub fn linha_lida(lidos: &Lidos) -> String {
    let mut partes = Vec::new();
    if let Some(l) = lidos.get(&Propriedade::Ganho) {
        partes.push(crate::idioma::tf("Ganho {}", &[&l.valor]));
    }
    if let Some(l) = lidos.get(&Propriedade::Exposicao) {
        partes.push(texto_do_obturador(l.valor));
    }
    if let Some(l) = lidos.get(&Propriedade::Balanco) {
        partes.push(format!("{} K", l.valor));
    }
    partes.join(" · ")
}

/// A linha se atualiza no máximo 4 vezes por segundo (§3.6).
pub const INTERVALO_DA_LINHA: Duration = Duration::from_millis(250);
/// Quanto tempo o pedido e o lido podem divergir antes de a tela dizer (§3.6).
pub const PRAZO_DA_DIVERGENCIA: Duration = Duration::from_secs(2);

/// As propriedades em que a divergência é vigiada: **só nos modos manuais e no Kelvin** (§3.6).
pub fn vigiadas(r: &Registro) -> Vec<Propriedade> {
    let mut v = Vec::new();
    if r.exposicao == ModoDeExposicao::Manual {
        v.extend([Propriedade::Exposicao, Propriedade::Ganho]);
    }
    if r.balanco == Balanco::Kelvin {
        v.push(Propriedade::Balanco);
    }
    if r.foco == Foco::Manual {
        v.push(Propriedade::Foco);
    }
    v
}

/// **A divergência de uma propriedade**: o pedido e o lido longe por mais de um passo durante
/// [`PRAZO_DA_DIVERGENCIA`] (§3.6). O tempo vem de fora (duração desde um zero qualquer).
#[derive(Clone, Debug, Default)]
pub struct Divergencia {
    desde: Option<(Duration, i32, i32)>,
}

impl Divergencia {
    /// Observa um par (pedido, lido); devolve `true` quando a divergência já dura o prazo.
    pub fn observar(&mut self, pedido: i32, lido: i32, passo: i32, agora: Duration) -> bool {
        let longe = (i64::from(pedido) - i64::from(lido)).abs() > i64::from(passo.max(1));
        if !longe {
            self.desde = None;
            return false;
        }
        match self.desde {
            // O pedido mudou: a contagem recomeça.
            Some((_, p, _)) if p != pedido => {
                self.desde = Some((agora, pedido, lido));
                false
            }
            Some((t, _, _)) => agora.saturating_sub(t) >= PRAZO_DA_DIVERGENCIA,
            None => {
                self.desde = Some((agora, pedido, lido));
                false
            }
        }
    }

    pub fn limpar(&mut self) {
        self.desde = None;
    }
}

/// "A câmera usou {lido} em vez de {pedido}." (§3.6), no idioma de agora.
pub fn frase_da_divergencia(p: Propriedade, lido: i32, pedido: i32) -> String {
    crate::idioma::tf("A câmera usou {} em vez de {}.", &[&texto_do_valor(p, lido), &texto_do_valor(p, pedido)])
}

// =============================================================================================
// Pouca luz (§3.1)
// =============================================================================================
//
// Com a exposição em Auto, o automático da webcam alonga o quadro para clarear a imagem, e o fps
// cai (o mesmo que o app de câmera nativo faz). As outras plataformas avisam; o Windows avisa pela
// regra do Mac (`PoucaLuz.Vigia`): nada garante que o `Get` da exposição em Auto diga o obturador
// que o driver está usando de fato (não medido em webcam nenhuma), e o fps baixo é o que a pessoa
// vê. Então o sinal é o **fps que chega**, contado pela captura (`chegados`), e não o obturador
// lido.

/// Abaixo desta fração do fps pedido, o quadro está lento (o Mac usa o mesmo: 87 %, isto é, o
/// quadro passou de 1/fps em ~15 %).
pub const FRACAO_LENTA: f64 = 0.87;
/// Quanto tempo seguido lento para acender, e de volta para apagar (a histerese do Mac: acende
/// rápido, apaga devagar, para o aviso não piscar numa luz no limite).
pub const ACENDE_A_POUCA_LUZ: Duration = Duration::from_secs(1);
pub const APAGA_A_POUCA_LUZ: Duration = Duration::from_secs(2);
/// A janela da medida do fps. O Mac mede em meio segundo, num relógio de 0,5 s; aqui a leitura é de
/// 250 ms (a dos ajustes), e um quarto de segundo a 30 fps são 7 ou 8 quadros: 7 / 0,25 = 28 e
/// 6 / 0,25 = 24 já cruzaria os 87 %. Um segundo deslizante tira esse serrilhado.
pub const JANELA_DO_FPS: Duration = Duration::from_secs(1);

/// **O fps medido**, pela contagem de quadros da captura (`chegados`), numa janela deslizante de
/// [`JANELA_DO_FPS`]. O tempo vem de fora (duração desde um zero qualquer), como na divergência.
#[derive(Clone, Debug, Default)]
pub struct MedidorDeFps {
    amostras: std::collections::VecDeque<(Duration, u64)>,
}

impl MedidorDeFps {
    /// Uma leitura do contador; devolve o fps da última janela, ou `None` enquanto não há um
    /// segundo inteiro de história.
    pub fn observar(&mut self, chegados: u64, agora: Duration) -> Option<f64> {
        // Um contador que voltou (outra captura, outro zero) não é uma queda de fps: recomeça.
        if self.amostras.back().is_some_and(|&(t, n)| chegados < n || agora < t) {
            self.amostras.clear();
        }
        self.amostras.push_back((agora, chegados));
        // Larga a mais velha só quando a seguinte já cobre a janela: a de trás fica sempre com
        // pelo menos [`JANELA_DO_FPS`] de idade.
        while self.amostras.len() > 2 && agora.saturating_sub(self.amostras[1].0) >= JANELA_DO_FPS {
            self.amostras.pop_front();
        }
        let &(t0, n0) = self.amostras.front()?;
        let dt = agora.saturating_sub(t0);
        if dt < JANELA_DO_FPS {
            return None;
        }
        Some((chegados - n0) as f64 / dt.as_secs_f64())
    }
}

/// **O vigia da pouca luz**, a regra do Mac (`PoucaLuz.Vigia`): acende depois de
/// [`ACENDE_A_POUCA_LUZ`] seguido com o fps medido abaixo de [`FRACAO_LENTA`] do pedido, e apaga
/// depois de [`APAGA_A_POUCA_LUZ`] seguidos de volta. Fps medido zero (a câmera parada, que tem a
/// frase dela) não acende.
#[derive(Clone, Debug, Default)]
pub struct VigiaDaPoucaLuz {
    acesa: bool,
    desde: Option<Duration>,
}

impl VigiaDaPoucaLuz {
    /// Devolve o fps medido (arredondado, entre 1 e o pedido) enquanto aceso, ou `None`.
    pub fn observar(&mut self, fps_medido: f64, fps: f64, agora: Duration) -> Option<u32> {
        let lento = fps > 0.0 && fps_medido > 0.0 && fps_medido < fps * FRACAO_LENTA;
        if lento != self.acesa {
            let desde = *self.desde.get_or_insert(agora);
            if agora.saturating_sub(desde) >= if lento { ACENDE_A_POUCA_LUZ } else { APAGA_A_POUCA_LUZ } {
                self.acesa = lento;
                self.desde = None;
            }
        } else {
            self.desde = None;
        }
        if !self.acesa {
            return None;
        }
        let teto = fps.round().max(1.0) as u32;
        Some((fps_medido.round().max(1.0) as u32).min(teto))
    }

    /// Apaga na hora, sem a histerese: a exposição passou a Manual, e o fps baixo deixou de ser
    /// coisa do automático.
    pub fn apagar(&mut self) {
        self.acesa = false;
        self.desde = None;
    }
}

/// **O vigia vale agora?** Só com o automático no comando da exposição: com o obturador manual o
/// teto é 1/fps (§3.1), e um fps baixo ali não é o automático clareando, nem se resolve pela
/// exposição manual. No modo compartilhado o registro do Quall não manda, e vale o que o driver diz
/// (sem leitura da exposição, vale: é o caso comum da webcam sem `CameraControl_Exposure`).
pub fn vigia_da_pouca_luz_vale(r: &Registro, lidos: &Lidos, compartilhada: bool) -> bool {
    if compartilhada {
        lidos.get(&Propriedade::Exposicao).is_none_or(|l| l.em_auto())
    } else {
        r.exposicao == ModoDeExposicao::Auto
    }
}

/// **O aviso aceso**: o fps de agora, o pedido, e se o conselho é a exposição manual (a câmera
/// declara obturador manual e o Quall pode mexer nela) ou a luz do ambiente.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoucaLuz {
    pub fps_agora: u32,
    pub fps: u32,
    pub com_manual: bool,
}

impl PoucaLuz {
    /// O aviso a partir do vigia: `com_manual` só quando a câmera declara obturador manual e não está
    /// no modo compartilhado (aí os controles ficam apagados, e o conselho seria inútil).
    pub fn de(fps_agora: u32, fps: f64, caps: &Capacidades, compartilhada: bool) -> PoucaLuz {
        let com_manual = !compartilhada && caps.get(&Propriedade::Exposicao).is_some_and(|f| f.tem_manual());
        PoucaLuz { fps_agora, fps: fps.round().max(1.0) as u32, com_manual }
    }

    /// A frase inteira, no idioma de agora (a do Android, com "nos ajustes da câmera" no lugar da
    /// engrenagem: no Windows a janela se chama "Ajustes da câmera").
    pub fn texto(&self) -> String {
        if self.com_manual {
            crate::idioma::tf("Pouca luz: {} fps para clarear a imagem. Para {} fps, use a exposição manual nos ajustes da câmera.", &[&self.fps_agora, &self.fps])
        } else {
            crate::idioma::tf("Pouca luz: {} fps para clarear a imagem. Mais luz no ambiente devolve os {} fps.", &[&self.fps_agora, &self.fps])
        }
    }

    /// A primeira frase só, para a linha da gravação da janela principal quando ela está vazia: a
    /// inteira passa dos ~86 caracteres que cabem nos 582 px do painel (a conta de
    /// `estilo::altura_do_aviso`) e terminaria em reticências; o conselho fica na janela dos ajustes.
    pub fn sem_conselho(&self) -> String {
        crate::idioma::tf("Pouca luz: {} fps para clarear a imagem.", &[&self.fps_agora])
    }

    /// A forma curta, para dividir a linha da gravação da janela principal com a gravação ou o
    /// "Controlado por".
    pub fn curto(&self) -> String {
        crate::idioma::tf("Pouca luz: {} fps", &[&self.fps_agora])
    }
}

// =============================================================================================
// Os textos (§3.5, §4)
// =============================================================================================
//
// As constantes ficam em português: são as chaves da tradução (`idioma.rs`), e quem as mostra
// passa por `idioma::t` (o título, o nome do ícone para o Narrador, os botões, as frases).

pub const TITULO_DA_JANELA: &str = "Ajustes da câmera"; // i18n: chave
/// O rótulo de acessibilidade do ícone (§4.1). Quem o usa passa por `idioma::t(ROTULO_DO_ICONE)`.
pub const ROTULO_DO_ICONE: &str = "Ajustes da câmera"; // i18n: chave
pub const RESTAURAR_AUTOMATICO: &str = "Restaurar automático"; // i18n: chave
pub const USAR_MEUS_AJUSTES: &str = "Usar meus ajustes"; // i18n: chave
pub const PASSAR_PARA_MANUAL: &str = "Passar para Manual"; // i18n: chave
pub const FRASE_DESTRAVE: &str = "Destrave a exposição para compensar."; // i18n: chave
pub const FRASE_FOCO_FIXO: &str = "Esta câmera tem foco fixo."; // i18n: chave
pub const FRASE_OUTRO_APP: &str = "Outro app está controlando esta câmera. Feche-o para ajustar."; // i18n: chave
/// A frase da aba "Ganho e obturador" com a exposição em Auto (§4.3), com "ganho" no lugar de
/// "ISO": no Windows o controle se chama Ganho (§3.2), e a frase que nomeia um controle que a tela
/// não tem confundiria. Ver o relato da frente.
pub const FRASE_PASSE_PARA_MANUAL: &str = "Passe a exposição para Manual para escolher ganho e obturador."; // i18n: chave

/// "Esta câmera não oferece {controle}." (§3.5), no idioma de agora. O `nome` vem em português
/// (`Propriedade::nome`, ou um dos da tela) e é traduzido aqui.
pub fn frase_sem_controle(nome: &str) -> String {
    crate::idioma::tf("Esta câmera não oferece {}.", &[&crate::idioma::tr(nome)])
}

/// **A frase do §3.5 de uma propriedade que esta câmera não oferece** (ou `None`, se oferece): o
/// foco sem faixa é "foco fixo"; o foco só com Auto não tem o foco manual.
pub fn frase_do_limite(p: Propriedade, caps: &Capacidades) -> Option<String> {
    match (p, caps.get(&p)) {
        (Propriedade::Foco, None) => Some(crate::idioma::t(FRASE_FOCO_FIXO).to_string()),
        (_, None) => Some(frase_sem_controle(p.nome())),
        (Propriedade::Foco, Some(f)) if !f.tem_manual() => Some(frase_sem_controle(p.nome())),
        _ => None,
    }
}

/// As quatro abas (§4.2), com os nomes do Windows (§4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Aba {
    #[default]
    Exposicao,
    GanhoEObturador,
    Balanco,
    Foco,
}

impl Aba {
    pub const TODAS: [Aba; 4] = [Aba::Exposicao, Aba::GanhoEObturador, Aba::Balanco, Aba::Foco];

    /// O rótulo em português (a chave da tradução: a tela passa por `idioma::t`).
    pub fn rotulo(self) -> &'static str {
        match self {
            Aba::Exposicao => "Exposição", // i18n: chave
            Aba::GanhoEObturador => "Ganho e obturador", // i18n: chave
            Aba::Balanco => "Balanço", // i18n: chave
            Aba::Foco => "Foco", // i18n: chave
        }
    }

    pub fn indice(self) -> usize {
        match self {
            Aba::Exposicao => 0,
            Aba::GanhoEObturador => 1,
            Aba::Balanco => 2,
            Aba::Foco => 3,
        }
    }
}

/// Um número com a vírgula decimal do português (§3), ou o ponto do inglês (`idioma::decimal`).
pub fn com_virgula(v: f64, casas: usize) -> String {
    crate::idioma::decimal(v, casas)
}

// =============================================================================================
// Os envios agrupados (§2.2)
// =============================================================================================

/// "Os envios à câmera são agrupados a no máximo 15 por segundo."
pub const INTERVALO_DOS_ENVIOS: Duration = Duration::from_millis(67);

/// Pode mandar de novo, `agora`, depois do último envio em `ultimo`?
pub fn pode_enviar(ultimo: Option<Duration>, agora: Duration) -> bool {
    ultimo.is_none_or(|u| agora.saturating_sub(u) >= INTERVALO_DOS_ENVIOS)
}

// =============================================================================================
// A bancada: o roteiro dos ajustes e a luma
// =============================================================================================

/// Um alvo de bancada, em palavras que não pedem saber a faixa da câmera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlvoDeBancada {
    Min,
    Max,
    Meio,
    Padrao,
    /// O obturador no teto de 1/fps (§3.1).
    Teto,
    Auto,
    Manual,
    Hz50,
    Hz60,
    Desligada,
    Sim,
    Nao,
    Valor(i64),
    /// Sem valor: `restaurar`.
    Nenhum,
}

/// Um passo do roteiro: a hora (desde a câmera pronta para ajustes) e os ajustes.
#[derive(Clone, Debug, PartialEq)]
pub struct PassoDeBancada {
    pub em: Duration,
    pub ajustes: Vec<(String, AlvoDeBancada)>,
}

const CHAVES_DE_BANCADA: [&str; 11] =
    ["brilho", "ganho", "obturador", "kelvin", "foco", "cintilacao", "exposicao", "balanco", "travar-exposicao", "travar-balanco", "restaurar"];

/// **Lê `--ajustes-camera`**: passos `segundos:chave=valor,chave=valor` separados por `;`. As
/// chaves: `brilho`, `ganho`, `obturador`, `kelvin` e `foco` (com `min`, `max`, `meio`, `padrao`,
/// `teto` só no obturador, ou um número do driver); `cintilacao` (`auto`, `50`, `60`, `desligada`);
/// `exposicao`, `balanco` e `foco` com `auto`/`manual`; `travar-exposicao` e `travar-balanco` com
/// `sim`/`nao`; e `restaurar`, sem valor. Os passos voltam em ordem de hora.
pub fn ler_roteiro_de_bancada(t: &str) -> Result<Vec<PassoDeBancada>, String> {
    let mut v = Vec::new();
    for passo in t.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (s, resto) = passo.split_once(':').ok_or_else(|| format!("o passo \"{passo}\" não tem \"segundos:\""))?; // i18n: fora (bancada)
        let s: f64 = s.trim().replace(',', ".").parse().map_err(|_| format!("\"{s}\" não é um número de segundos"))?; // i18n: fora (bancada)
        if !(s.is_finite() && s >= 0.0) {
            return Err(format!("{s} não é uma hora de passo")); // i18n: fora (bancada)
        }
        let mut ajustes = Vec::new();
        for item in resto.split(',').map(str::trim).filter(|x| !x.is_empty()) {
            let (chave, valor) = match item.split_once('=') {
                Some((c, x)) => (c.trim().to_string(), x.trim()),
                None => (item.to_string(), ""),
            };
            if !CHAVES_DE_BANCADA.contains(&chave.as_str()) {
                return Err(format!("chave desconhecida: \"{chave}\""));
            }
            let alvo = match valor {
                "" if chave == "restaurar" => AlvoDeBancada::Nenhum,
                "" => return Err(format!("\"{chave}\" sem valor")), // i18n: fora (bancada)
                "min" => AlvoDeBancada::Min,
                "max" => AlvoDeBancada::Max,
                "meio" => AlvoDeBancada::Meio,
                "padrao" => AlvoDeBancada::Padrao,
                "teto" => AlvoDeBancada::Teto,
                "auto" => AlvoDeBancada::Auto,
                "manual" => AlvoDeBancada::Manual,
                "50" if chave == "cintilacao" => AlvoDeBancada::Hz50,
                "60" if chave == "cintilacao" => AlvoDeBancada::Hz60,
                "desligada" => AlvoDeBancada::Desligada,
                "sim" => AlvoDeBancada::Sim,
                "nao" => AlvoDeBancada::Nao,
                x => AlvoDeBancada::Valor(x.parse().map_err(|_| format!("\"{x}\" não é valor de \"{chave}\""))?), // i18n: fora (bancada)
            };
            ajustes.push((chave, alvo));
        }
        if ajustes.is_empty() {
            return Err(format!("o passo \"{passo}\" não tem ajuste")); // i18n: fora (bancada)
        }
        v.push(PassoDeBancada { em: Duration::from_secs_f64(s), ajustes });
    }
    v.sort_by_key(|p| p.em);
    Ok(v)
}

/// **Os gestos de um ajuste de bancada**, com a faixa de agora. `Err` quando a câmera não tem a
/// propriedade, ou o alvo não serve para a chave: a bancada diz isso no diário, e segue.
pub fn acoes_de_bancada(chave: &str, alvo: AlvoDeBancada, caps: &Capacidades, fps: f64) -> Result<Vec<Acao>, String> {
    use AlvoDeBancada as A;
    let faixa = |p: Propriedade| caps.get(&p).copied().ok_or_else(|| format!("a câmera não declara {}", p.chave())); // i18n: fora (bancada)
    let na_faixa = |f: &Faixa, alvo: A| -> Result<i32, String> {
        Ok(match alvo {
            A::Min => f.min,
            A::Max => f.max,
            A::Meio => f.do_indice(i64::from(f.passos() / 2)),
            A::Padrao => f.cortar(i64::from(f.padrao)),
            A::Valor(v) => f.cortar(v),
            outro => return Err(format!("{outro:?} não é valor de {chave}")), // i18n: fora (bancada)
        })
    };
    Ok(match (chave, alvo) {
        ("brilho", a) => vec![Acao::Brilho(na_faixa(&faixa(Propriedade::Brilho)?, a)?)],
        ("ganho", a) => vec![Acao::Exposicao(ModoDeExposicao::Manual), Acao::Ganho(na_faixa(&faixa(Propriedade::Ganho)?, a)?)],
        ("obturador", a) => {
            let f = faixa_do_obturador(&faixa(Propriedade::Exposicao)?, fps);
            let v = if a == A::Teto { f.max } else { na_faixa(&f, a)? };
            vec![Acao::Exposicao(ModoDeExposicao::Manual), Acao::Obturador(v)]
        }
        ("kelvin", a) => vec![Acao::Balanco(Balanco::Kelvin), Acao::Kelvin(na_faixa(&faixa_do_kelvin(&faixa(Propriedade::Balanco)?), a)?)],
        ("foco", A::Auto) => vec![Acao::Foco(Foco::Auto)],
        ("foco", a) => {
            let f = faixa(Propriedade::Foco)?;
            let v = na_faixa(&f, a)?;
            vec![Acao::Foco(Foco::Manual), Acao::FocoPosicao(f.fracao(i64::from(v)))]
        }
        ("cintilacao", a) => {
            faixa(Propriedade::AntiCintilacao)?;
            vec![Acao::AntiCintilacao(match a {
                A::Auto => AntiCintilacao::Auto,
                A::Hz50 => AntiCintilacao::Hz50,
                A::Hz60 => AntiCintilacao::Hz60,
                A::Desligada => AntiCintilacao::Desligada,
                outro => return Err(format!("{outro:?} não é valor de cintilacao")), // i18n: fora (bancada)
            })]
        }
        ("exposicao", A::Auto) => vec![Acao::Exposicao(ModoDeExposicao::Auto)],
        ("exposicao", A::Manual) => vec![Acao::Exposicao(ModoDeExposicao::Manual)],
        ("balanco", A::Auto) => vec![Acao::Balanco(Balanco::Auto)],
        ("travar-exposicao", A::Sim | A::Nao) => vec![Acao::TravarExposicao(alvo == A::Sim)],
        ("travar-balanco", A::Sim | A::Nao) => vec![Acao::TravarBalanco(alvo == A::Sim)],
        ("restaurar", A::Nenhum) => vec![Acao::Restaurar],
        (c, a) => return Err(format!("{a:?} não é valor de {c}")), // i18n: fora (bancada)
    })
}

/// **A média de luma** de um quadro BGRA pequeno (a bandeira de bancada `luma_media`, §5): Y' pelos
/// coeficientes do BT.709 sobre os valores de 0 a 255, linha a linha (`passo` em bytes). Sem
/// pixel, `None`.
pub fn luma_de_bgra(px: &[u8], passo: usize, largura: usize, altura: usize) -> Option<f64> {
    if largura == 0 || altura == 0 || passo < largura * 4 {
        return None;
    }
    let mut soma = 0.0f64;
    let mut n = 0u64;
    for y in 0..altura {
        let linha = px.get(y * passo..y * passo + largura * 4)?;
        for p in linha.chunks_exact(4) {
            soma += 0.0722 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.2126 * f64::from(p[2]);
            n += 1;
        }
    }
    Some(soma / n as f64)
}

/// A linha do diário de uma medida de bancada (o roteiro de prova a lê): chave=valor separados por
/// espaço, sem acento nas chaves. O pedido é o último envio: um envio com `Flags_Auto` (a devolução)
/// sai como `pedido=auto`, porque o valor dele não é pedido nenhum — e o `Get` em Auto, em muitos
/// drivers, devolve o último valor manual (medido na Integrated Webcam do Dell, 01/10: o Kelvin em
/// Auto lia 6500 depois de um manual de 6500).
#[allow(clippy::too_many_arguments)]
pub fn linha_da_medida(passo: usize, chave: &str, pedido: Option<Envio>, lido: Option<Lido>, luma: Option<f64>, fps: Option<f64>, hr: &str) -> String {
    let n = |x: Option<i32>| x.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
    let pedido = match pedido {
        Some(e) if e.bandeiras == FLAGS_AUTO => "auto".to_string(),
        Some(e) => e.valor.to_string(),
        None => "-".to_string(),
    };
    format!(
        "ajustes: medida passo={passo} prop={chave} pedido={} lido={} flags={} luma={} fps={} hr={hr}",
        pedido,
        n(lido.map(|l| l.valor)),
        n(lido.map(|l| l.bandeiras)),
        luma.map(|l| format!("{l:.2}")).unwrap_or_else(|| "-".into()),
        fps.map(|f| format!("{f:.2}")).unwrap_or_else(|| "-".into()),
    )
}

#[cfg(test)]
mod testes {
    use super::*;

    fn f(min: i32, max: i32, passo: i32, padrao: i32, bandeiras: i32) -> Faixa {
        Faixa::do_get_range(min, max, passo, padrao, bandeiras).unwrap()
    }

    /// Uma webcam UVC típica (a Integrated Webcam do Dell declara faixas assim; os números exatos
    /// saem da prova).
    fn caps_de_exemplo() -> Capacidades {
        let mut c = Capacidades::new();
        c.insert(Propriedade::Exposicao, f(-11, -1, 1, -6, FLAGS_AUTO | FLAGS_MANUAL));
        c.insert(Propriedade::Ganho, f(0, 100, 1, 0, FLAGS_MANUAL));
        c.insert(Propriedade::Brilho, f(-64, 64, 1, 0, FLAGS_MANUAL));
        c.insert(Propriedade::Balanco, f(2800, 6500, 10, 4600, FLAGS_AUTO | FLAGS_MANUAL));
        c.insert(Propriedade::Foco, f(0, 250, 5, 0, FLAGS_AUTO | FLAGS_MANUAL));
        c.insert(Propriedade::AntiCintilacao, f(0, 2, 1, 1, FLAGS_MANUAL));
        c
    }

    fn lidos_de_exemplo() -> Lidos {
        let mut l = Lidos::new();
        l.insert(Propriedade::Exposicao, Lido { valor: -6, bandeiras: FLAGS_AUTO });
        l.insert(Propriedade::Ganho, Lido { valor: 32, bandeiras: FLAGS_MANUAL });
        l.insert(Propriedade::Brilho, Lido { valor: 0, bandeiras: FLAGS_MANUAL });
        l.insert(Propriedade::Balanco, Lido { valor: 5230, bandeiras: FLAGS_AUTO });
        l.insert(Propriedade::Foco, Lido { valor: 125, bandeiras: FLAGS_AUTO });
        l.insert(Propriedade::AntiCintilacao, Lido { valor: 2, bandeiras: FLAGS_MANUAL });
        l
    }

    #[test]
    fn a_faixa_corta_e_arredonda_ao_passo() {
        assert_eq!(Faixa::do_get_range(5, 1, 1, 0, 0), None, "máximo abaixo do mínimo");
        let x = f(0, 255, 0, 128, FLAGS_MANUAL);
        assert_eq!(x.passo, 1, "passo zero vira 1");
        let k = f(2800, 6500, 10, 4600, 3);
        assert_eq!(k.cortar(1000), 2800);
        assert_eq!(k.cortar(9999), 6500);
        assert_eq!(k.cortar(5234), 5230);
        assert_eq!(k.cortar(5235), 5240);
        // O último degrau é o maior que não passa do máximo.
        let torta = f(0, 10, 3, 0, FLAGS_MANUAL);
        assert_eq!(torta.cortar(10), 9);
        assert_eq!(torta.passos(), 3);
        assert_eq!(torta.do_indice(5), 9);
        assert_eq!(torta.indice(7), 2);
        assert!(k.tem_auto() && k.tem_manual());
        assert!(f(0, 1, 1, 0, 0).tem_manual(), "sem bandeira, manual");
        assert!(!f(0, 1, 1, 0, FLAGS_AUTO).tem_manual());
        assert_eq!(f(0, 250, 5, 0, 3).da_fracao(0.5), 125);
        assert_eq!(f(0, 250, 5, 0, 3).fracao(125), 0.5);
        assert_eq!(f(7, 7, 1, 7, 2).fracao(7), 0.0);
    }

    #[test]
    fn o_obturador_em_log2_e_o_teto_de_1_por_fps() {
        assert_eq!(texto_do_obturador(-5), "1/32 s");
        assert_eq!(texto_do_obturador(-6), "1/64 s");
        assert_eq!(texto_do_obturador(-7), "1/128 s");
        assert_eq!(texto_do_obturador(0), "1 s");
        assert_eq!(texto_do_obturador(2), "4 s");
        assert_eq!(teto_do_obturador(30.0), -5, "§3.1: −5 a 30 fps");
        assert_eq!(teto_do_obturador(60.0), -6, "§3.1: −6 a 60 fps");
        assert_eq!(teto_do_obturador(29.97), -5);
        assert_eq!(teto_do_obturador(15.0), -4);
        assert_eq!(teto_do_obturador(32.0), -5, "1/32 cabe em 32 fps");
        assert_eq!(teto_do_obturador(0.0), i32::MAX);
        let faixa = f(-11, -1, 1, -6, 3);
        assert_eq!(faixa_do_obturador(&faixa, 30.0).max, -5);
        assert_eq!(faixa_do_obturador(&faixa, 60.0).max, -6);
        assert_eq!(faixa_do_obturador(&f(-3, 0, 1, -2, 3), 30.0).max, -3, "o mínimo já passa do teto: fica o mínimo");
        assert_eq!(ns_do_log2(-5), 31_250_000);
        assert_eq!(log2_dos_ns(31_250_000), -5);
        assert_eq!(log2_dos_ns(33_333_333), -5);
        assert_eq!(log2_dos_ns(16_666_667), -6);
        assert_eq!(log2_dos_ns(ns_do_log2(-9)), -9);
    }

    #[test]
    fn o_kelvin_de_100_em_100_dentro_do_driver() {
        let k = faixa_do_kelvin(&f(2800, 6500, 10, 4600, 3));
        assert_eq!((k.min, k.max, k.passo), (2800, 6500, 100));
        let k = faixa_do_kelvin(&f(1000, 12000, 1, 4600, 3));
        assert_eq!((k.min, k.max, k.passo), (2000, 10000, 100));
        let k = faixa_do_kelvin(&f(2850, 6450, 250, 4600, 3));
        assert_eq!((k.min, k.passo), (2900, 300));
        assert!(k.max <= 6450 && (k.max - k.min) % 300 == 0);
        // Fora de 2000–10000: a faixa do driver.
        let estranha = f(0, 100, 1, 50, 3);
        assert_eq!(faixa_do_kelvin(&estranha), estranha);
        assert_eq!(kelvin_da_tela(5234), 5200);
        assert_eq!(kelvin_da_tela(5250), 5300);
        assert_eq!(kelvin_da_tela(500), 2000);
        assert_eq!(kelvin_da_tela(20000), 10000);
    }

    #[test]
    fn a_anti_cintilacao_e_o_auto_do_uvc_1_5() {
        let uvc11 = f(0, 2, 1, 1, FLAGS_MANUAL);
        let uvc15 = f(0, 3, 1, 3, FLAGS_MANUAL);
        assert_eq!(AntiCintilacao::Auto.valor_do_driver(Some(&uvc11)), None, "sem o 3, Auto é não tocar");
        assert_eq!(AntiCintilacao::Auto.valor_do_driver(Some(&uvc15)), Some(3));
        assert_eq!(AntiCintilacao::Hz50.valor_do_driver(None), Some(1));
        assert_eq!(AntiCintilacao::Hz60.valor_do_driver(None), Some(2));
        assert_eq!(AntiCintilacao::Desligada.valor_do_driver(None), Some(0));
        assert_eq!(AntiCintilacao::do_valor(2), Some(AntiCintilacao::Hz60));
        assert_eq!(AntiCintilacao::do_valor(9), None);
        assert!(AntiCintilacao::Auto.oferecida(&uvc11));
        assert!(!AntiCintilacao::Desligada.oferecida(&f(1, 2, 1, 1, 2)), "o 0 fora da faixa");
        assert!(AntiCintilacao::Hz60.oferecida(&uvc11));
    }

    #[test]
    fn o_registro_em_json_com_os_nomes_literais() {
        let r = Registro {
            exposicao: ModoDeExposicao::Manual,
            iso: Some(64.0),
            obturador_ns: Some(31_250_000),
            anti_cintilacao: AntiCintilacao::Hz60,
            balanco: Balanco::LuzDoDia,
            foco: Foco::Travado,
            foco_posicao: Some(0.42),
            trava_ganhos: Some(vec![5200.0]),
            ..Default::default()
        };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["exposicao"], "manual");
        assert_eq!(j["obturadorNs"], 31_250_000);
        assert_eq!(j["antiCintilacao"], "60");
        assert_eq!(j["balanco"], "luzDoDia");
        assert_eq!(j["foco"], "travado");
        assert_eq!(j["focoPosicao"], 0.42);
        assert_eq!(j["travaExposicao"], false);
        assert_eq!(j["travaGanhos"][0], 5200.0);
        assert!(j.get("travaIso").is_none(), "o que não existe não é escrito");
        let de_volta: Registro = serde_json::from_value(j).unwrap();
        assert_eq!(de_volta, r);
        // Um registro parcial: o resto no padrão da tabela (§2).
        let parcial: Registro = serde_json::from_str(r#"{"balanco":"kelvin","kelvin":5600}"#).unwrap();
        assert_eq!(parcial.exposicao, ModoDeExposicao::Auto);
        assert_eq!(parcial.anti_cintilacao, AntiCintilacao::Auto);
        assert_eq!(parcial.kelvin, Some(5600));
        assert!(Registro::default().e_padrao());
    }

    #[test]
    fn abre_no_automatico_e_lembra_o_ultimo_manual() {
        let manual = Registro { exposicao: ModoDeExposicao::Manual, iso: Some(64.0), obturador_ns: Some(ns_do_log2(-7)), ..Default::default() };
        assert_eq!(meus_ajustes(manual.clone()), Some(manual.clone()));
        assert_eq!(meus_ajustes(Registro::default()), None);
        assert!(oferece_meus_ajustes(Some(&manual), &Registro::default()));
        assert!(!oferece_meus_ajustes(Some(&manual), &manual), "já vale: some");
        assert!(!oferece_meus_ajustes(None, &Registro::default()));
        // "Usar meus ajustes" não é gesto que aplicar_acao conheça: a thread troca o registro.
        let mut r = Registro::default();
        assert!(!aplicar_acao(&mut r, Acao::UsarMeusAjustes, &Capacidades::new(), &Lidos::new()));
    }

    #[test]
    fn o_mapa_por_link_tolera_o_ilegivel() {
        let link = r"\\?\usb#vid_0c45&pid_6a09&mi_00#6&abc#{e5323777-f976-4f5b-9b55-b94699c46e44}\global";
        let mut m = Mapa::new();
        let r = Registro { ev: 12.0, ..Default::default() };
        guardar_em(&mut m, link, &r);
        assert_eq!(registro_de(&m, &link.to_uppercase()), r, "o link sem caixa");
        let texto = mapa_em_json(&m);
        assert_eq!(ler_mapa(&texto), m);
        assert_eq!(ler_mapa(&format!("\u{FEFF}{texto}")), m, "com BOM");
        // Restaurar automático (07/10): a entrada fica, porque ela é "meus ajustes".
        guardar_em(&mut m, &link.to_uppercase(), &Registro::default());
        assert_eq!(registro_de(&m, link), r);
        assert!(ler_mapa("não é json").is_empty());
        assert!(ler_mapa("[1,2]").is_empty());
        let misto = ler_mapa(r#"{"a":{"ev":3},"b":{"exposicao":"lua"},"c":7}"#);
        assert_eq!(misto.len(), 1, "a entrada ilegível é pulada sem levar as outras");
        assert_eq!(misto["a"].ev, 3.0);
        assert_eq!(registro_de(&misto, "z"), Registro::default());
    }

    #[test]
    fn o_plano_manda_so_o_manual_e_devolve_so_o_tocado() {
        let caps = caps_de_exemplo();
        let originais = lidos_de_exemplo();
        let nada = BTreeSet::new();
        // O registro no padrão não toca em nada: a câmera fica como estava (§2.2).
        assert!(plano(&Registro::default(), &caps, 30.0, &originais, &nada).is_empty());
        // Manual: o obturador cortado no teto de 1/30 e o ganho na faixa, nesta ordem.
        let r = Registro { exposicao: ModoDeExposicao::Manual, iso: Some(500.0), obturador_ns: Some(ns_do_log2(-2)), ..Default::default() };
        let p = plano(&r, &caps, 30.0, &originais, &nada);
        assert_eq!(
            p,
            vec![
                Envio { prop: Propriedade::Exposicao, valor: -5, bandeiras: FLAGS_MANUAL },
                Envio { prop: Propriedade::Ganho, valor: 100, bandeiras: FLAGS_MANUAL },
            ]
        );
        // A 60 fps o mesmo registro vai a −6; e o guardado continua −2 (§3.1).
        assert_eq!(plano(&r, &caps, 60.0, &originais, &nada)[0].valor, -6);
        assert_eq!(r.obturador_ns, Some(ns_do_log2(-2)));
        // De volta a Auto, com os dois tocados: Auto no obturador (tem Auto), o valor de antes no
        // ganho (não tem Auto).
        let tocados: BTreeSet<_> = [Propriedade::Exposicao, Propriedade::Ganho].into();
        let p = plano(&Registro::default(), &caps, 30.0, &originais, &tocados);
        assert_eq!(
            p,
            vec![
                Envio { prop: Propriedade::Exposicao, valor: -6, bandeiras: FLAGS_AUTO },
                Envio { prop: Propriedade::Ganho, valor: 32, bandeiras: FLAGS_MANUAL },
            ]
        );
        assert_eq!(devolver_tudo(&caps, &originais, &tocados), p, "a soltura devolve o mesmo");
        let mut t = BTreeSet::new();
        marcar_tocado(&mut t, &p[0], false);
        assert!(t.contains(&Propriedade::Exposicao));
        marcar_tocado(&mut t, &p[0], true);
        assert!(t.is_empty());
    }

    #[test]
    fn a_abertura_poe_em_auto_o_que_outro_app_deixou_em_manual() {
        let caps = caps_de_exemplo();
        // Já no automático, com o brilho e o ganho no padrão: nada a mandar (§2.2).
        let mut no_padrao = lidos_de_exemplo();
        no_padrao.insert(Propriedade::Ganho, Lido { valor: 0, bandeiras: FLAGS_MANUAL });
        assert!(normalizacao_da_abertura(&Registro::default(), &caps, 30.0, &no_padrao).is_empty());
        // Outro app deixou exposição e foco em manual e o brilho no mínimo: a "câmera escura" de 06/10.
        let mut escura = lidos_de_exemplo();
        escura.insert(Propriedade::Exposicao, Lido { valor: -11, bandeiras: FLAGS_MANUAL });
        escura.insert(Propriedade::Foco, Lido { valor: 0, bandeiras: FLAGS_MANUAL });
        escura.insert(Propriedade::Brilho, Lido { valor: -64, bandeiras: FLAGS_MANUAL });
        let n = normalizacao_da_abertura(&Registro::default(), &caps, 30.0, &escura);
        assert_eq!(
            n,
            vec![
                Envio { prop: Propriedade::Exposicao, valor: -11, bandeiras: FLAGS_AUTO },
                Envio { prop: Propriedade::Ganho, valor: 0, bandeiras: FLAGS_MANUAL },
                Envio { prop: Propriedade::Brilho, valor: 0, bandeiras: FLAGS_MANUAL },
                Envio { prop: Propriedade::Foco, valor: 0, bandeiras: FLAGS_AUTO },
            ]
        );
        // A anti-cintilação fica como está; o ganho de exemplo (32, padrão 0) volta ao padrão.
        assert!(n.iter().all(|e| e.prop != Propriedade::AntiCintilacao));
        // O que o registro quer em manual não é normalizado: o plano cuida dele.
        let manual = Registro { exposicao: ModoDeExposicao::Manual, iso: Some(50.0), obturador_ns: Some(ns_do_log2(-8)), ..Default::default() };
        assert!(normalizacao_da_abertura(&manual, &caps, 30.0, &escura).iter().all(|e| e.prop != Propriedade::Exposicao));
        // E a devolução volta ao normalizado, não ao manual escuro.
        let o = originais_depois_da_normalizacao(&escura, &n);
        let tocados: BTreeSet<_> = [Propriedade::Brilho].into();
        assert_eq!(devolver_tudo(&caps, &o, &tocados), vec![Envio { prop: Propriedade::Brilho, valor: 0, bandeiras: FLAGS_MANUAL }]);
    }

    #[test]
    fn o_brilho_e_um_deslocamento_do_padrao() {
        let caps = caps_de_exemplo();
        let mut r = Registro::default();
        assert!(aplicar_acao(&mut r, Acao::Brilho(20), &caps, &Lidos::new()));
        assert_eq!(r.ev, 20.0);
        let d = desejado(&r, &caps, 30.0);
        assert_eq!(d.get(&Propriedade::Brilho), Some(&20));
        // Fora da faixa: cortado no registro e no envio.
        aplicar_acao(&mut r, Acao::Brilho(500), &caps, &Lidos::new());
        assert_eq!(r.ev, 64.0);
        // Voltar ao padrão é "não tocar" (ou devolver o que a câmera tinha).
        aplicar_acao(&mut r, Acao::Brilho(0), &caps, &Lidos::new());
        assert!(r.e_padrao());
        let tocados: BTreeSet<_> = [Propriedade::Brilho].into();
        let p = plano(&r, &caps, 30.0, &lidos_de_exemplo(), &tocados);
        assert_eq!(p, vec![Envio { prop: Propriedade::Brilho, valor: 0, bandeiras: FLAGS_MANUAL }]);
    }

    #[test]
    fn as_travas_guardam_o_lido_e_reaplicam_como_manual() {
        let caps = caps_de_exemplo();
        let lidos = lidos_de_exemplo();
        let mut r = Registro::default();
        aplicar_acao(&mut r, Acao::TravarExposicao(true), &caps, &lidos);
        assert!(r.trava_exposicao);
        assert_eq!(r.trava_iso, Some(32.0));
        assert_eq!(r.trava_obturador_ns, Some(ns_do_log2(-6)));
        // §2.1: onde há manual, a trava volta como manual com os valores guardados.
        let p = plano(&r, &caps, 30.0, &lidos, &BTreeSet::new());
        assert_eq!(p[0], Envio { prop: Propriedade::Exposicao, valor: -6, bandeiras: FLAGS_MANUAL });
        assert_eq!(p[1], Envio { prop: Propriedade::Ganho, valor: 32, bandeiras: FLAGS_MANUAL });
        // A trava só com Auto.
        let mut m = Registro { exposicao: ModoDeExposicao::Manual, ..Default::default() };
        assert!(!aplicar_acao(&mut m, Acao::TravarExposicao(true), &caps, &lidos));
        aplicar_acao(&mut r, Acao::TravarExposicao(false), &caps, &lidos);
        assert!(r.e_padrao());
        // Balanço: trava guarda o Kelvin lido; com Kelvin ela some.
        aplicar_acao(&mut r, Acao::TravarBalanco(true), &caps, &lidos);
        assert_eq!(r.trava_ganhos, Some(vec![5230.0]));
        assert_eq!(desejado(&r, &caps, 30.0)[&Propriedade::Balanco], 5230);
        aplicar_acao(&mut r, Acao::Balanco(Balanco::Kelvin), &caps, &lidos);
        assert!(!r.trava_balanco && r.trava_ganhos.is_none());
        assert_eq!(r.kelvin, Some(5200), "o estimado no momento de passar a Kelvin");
        // Foco travado guarda a posição lida.
        aplicar_acao(&mut r, Acao::Foco(Foco::Travado), &caps, &lidos);
        assert_eq!(r.foco_posicao, Some(0.5));
        assert_eq!(desejado(&r, &caps, 30.0)[&Propriedade::Foco], 125);
    }

    #[test]
    fn passar_para_manual_parte_do_lido() {
        let caps = caps_de_exemplo();
        let lidos = lidos_de_exemplo();
        let mut r = Registro { trava_exposicao: true, trava_iso: Some(1.0), ..Default::default() };
        aplicar_acao(&mut r, Acao::Exposicao(ModoDeExposicao::Manual), &caps, &lidos);
        assert_eq!(r.exposicao, ModoDeExposicao::Manual);
        assert_eq!(r.iso, Some(32.0));
        assert_eq!(r.obturador_ns, Some(ns_do_log2(-6)));
        assert!(!r.trava_exposicao && r.trava_iso.is_none());
        // Já em manual, passar de novo não apaga o que a pessoa escolheu.
        aplicar_acao(&mut r, Acao::Ganho(80), &caps, &lidos);
        assert!(!aplicar_acao(&mut r, Acao::Exposicao(ModoDeExposicao::Manual), &caps, &lidos));
        assert_eq!(r.iso, Some(80.0));
        // Os presets não existem no Windows.
        assert!(!aplicar_acao(&mut r, Acao::Balanco(Balanco::Nublado), &caps, &lidos));
        assert!(aplicar_acao(&mut r, Acao::Restaurar, &caps, &lidos));
        assert!(r.e_padrao());
        aplicar_acao(&mut r, Acao::FocoPosicao(0.4249), &caps, &lidos);
        assert_eq!(r.foco_posicao, Some(0.42), "de 0,01 em 0,01");
    }

    #[test]
    fn so_o_que_a_camera_declara_entra_no_plano() {
        let mut caps = Capacidades::new();
        caps.insert(Propriedade::Brilho, f(0, 255, 1, 128, FLAGS_MANUAL));
        let r = Registro { exposicao: ModoDeExposicao::Manual, iso: Some(10.0), obturador_ns: Some(1), ev: -28.0, ..Default::default() };
        assert_eq!(plano(&r, &caps, 30.0, &Lidos::new(), &BTreeSet::new()), vec![Envio { prop: Propriedade::Brilho, valor: 100, bandeiras: FLAGS_MANUAL }]);
        // Uma propriedade só com Auto não recebe valor manual.
        caps.insert(Propriedade::Foco, f(0, 10, 1, 0, FLAGS_AUTO));
        let r = Registro { foco: Foco::Manual, foco_posicao: Some(0.3), ..Default::default() };
        assert!(desejado(&r, &caps, 30.0).is_empty());
        // A anti-cintilação: 60 vai; Auto sem UVC 1.5 não vai.
        caps.insert(Propriedade::AntiCintilacao, f(0, 2, 1, 1, 0));
        let r = Registro { anti_cintilacao: AntiCintilacao::Hz60, ..Default::default() };
        assert_eq!(desejado(&r, &caps, 30.0)[&Propriedade::AntiCintilacao], 2);
        assert!(desejado(&Registro::default(), &caps, 30.0).is_empty());
        // Devolver a anti-cintilação é voltar o valor de antes, nunca Flags_Auto.
        let originais: Lidos = [(Propriedade::AntiCintilacao, Lido { valor: 1, bandeiras: 0 })].into();
        assert_eq!(
            devolucao(Propriedade::AntiCintilacao, &caps, &originais),
            Some(Envio { prop: Propriedade::AntiCintilacao, valor: 1, bandeiras: FLAGS_MANUAL })
        );
    }

    #[test]
    fn a_linha_lida_e_a_divergencia() {
        let lidos = lidos_de_exemplo();
        assert_eq!(linha_lida(&lidos), "Ganho 32 · 1/64 s · 5230 K");
        assert_eq!(linha_lida(&Lidos::new()), "");
        let s = Duration::from_secs;
        let mut d = Divergencia::default();
        assert!(!d.observar(-5, -5, 1, s(0)));
        assert!(!d.observar(-5, -7, 1, s(1)), "longe, mas há menos de 2 s");
        assert!(!d.observar(-5, -7, 1, Duration::from_millis(2900)));
        assert!(d.observar(-5, -7, 1, s(3)));
        assert!(!d.observar(-5, -6, 1, s(4)), "um passo só não é divergência");
        assert!(!d.observar(-4, -7, 1, s(5)));
        assert!(!d.observar(-3, -7, 1, s(6)), "o pedido mudou: recomeça");
        assert!(d.observar(-3, -7, 1, s(8)));
        assert_eq!(frase_da_divergencia(Propriedade::Exposicao, -6, -5), "A câmera usou 1/64 s em vez de 1/32 s.");
        assert_eq!(frase_da_divergencia(Propriedade::Balanco, 5000, 5600), "A câmera usou 5000 K em vez de 5600 K.");
        let r = Registro { exposicao: ModoDeExposicao::Manual, balanco: Balanco::Kelvin, ..Default::default() };
        assert_eq!(vigiadas(&r), vec![Propriedade::Exposicao, Propriedade::Ganho, Propriedade::Balanco]);
        assert!(vigiadas(&Registro::default()).is_empty(), "só nos modos manuais e no Kelvin");
    }

    #[test]
    fn o_fps_medido_na_janela_de_um_segundo() {
        let ms = Duration::from_millis;
        let mut m = MedidorDeFps::default();
        // 30 fps lidos a cada 250 ms: 7 ou 8 quadros por leitura, e a janela de 1 s diz 30.
        let mut n = 0u64;
        let mut ultimo = None;
        for i in 0..=8u64 {
            if i > 0 {
                n += if i % 2 == 0 { 8 } else { 7 };
            }
            ultimo = m.observar(n, ms(250 * i));
            if i < 4 {
                assert_eq!(ultimo, None, "menos de 1 s de história");
            }
        }
        assert_eq!(ultimo, Some(30.0));
        // O contador que volta recomeça a história, sem um fps negativo.
        assert_eq!(m.observar(3, ms(2250)), None);
        // A leitura atrasada (a thread acordou tarde) mede na janela que de fato passou.
        let mut m = MedidorDeFps::default();
        assert_eq!(m.observar(0, ms(0)), None);
        assert_eq!(m.observar(15, ms(1000)), Some(15.0));
        assert_eq!(m.observar(30, ms(2000)), Some(15.0));
    }

    #[test]
    fn o_vigia_da_pouca_luz_tem_a_histerese_do_mac() {
        let ms = Duration::from_millis;
        let mut v = VigiaDaPoucaLuz::default();
        assert_eq!(v.observar(30.0, 30.0, ms(0)), None);
        assert_eq!(v.observar(26.5, 30.0, ms(250)), None, "26,5 está acima de 87 % de 30 (26,1)");
        assert_eq!(v.observar(15.2, 30.0, ms(500)), None, "lento, mas há menos de 1 s");
        assert_eq!(v.observar(15.2, 30.0, ms(1250)), None);
        assert_eq!(v.observar(15.2, 30.0, ms(1500)), Some(15), "1 s seguido lento: acende");
        // Uma volta curta não apaga: são 2 s seguidos.
        assert_eq!(v.observar(30.0, 30.0, ms(1750)), Some(30));
        assert_eq!(v.observar(14.6, 30.0, ms(2000)), Some(15), "voltou a ficar lento: a contagem recomeça");
        assert_eq!(v.observar(30.0, 30.0, ms(2250)), Some(30));
        assert_eq!(v.observar(30.0, 30.0, ms(4000)), Some(30), "menos de 2 s de volta");
        assert_eq!(v.observar(30.0, 30.0, ms(4250)), None, "2 s de volta: apaga");
        // Câmera parada (0 fps) não acende; fps pedido desconhecido também não.
        let mut v = VigiaDaPoucaLuz::default();
        assert_eq!(v.observar(0.0, 30.0, ms(0)), None);
        assert_eq!(v.observar(0.0, 30.0, ms(5000)), None);
        assert_eq!(v.observar(10.0, 0.0, ms(6000)), None);
        assert_eq!(v.observar(10.0, 0.0, ms(9000)), None);
        // O fps devolvido fica entre 1 e o pedido; apagar não espera a histerese.
        let mut v = VigiaDaPoucaLuz::default();
        v.observar(0.4, 30.0, ms(0));
        assert_eq!(v.observar(0.4, 30.0, ms(1000)), Some(1));
        v.apagar();
        assert_eq!(v.observar(0.4, 30.0, ms(1100)), None, "apagado, conta de novo");
    }

    #[test]
    fn quando_o_vigia_vale_e_a_frase_da_pouca_luz() {
        let auto = Registro::default();
        let manual = Registro { exposicao: ModoDeExposicao::Manual, ..Default::default() };
        let lidos = lidos_de_exemplo();
        assert!(vigia_da_pouca_luz_vale(&auto, &lidos, false));
        assert!(!vigia_da_pouca_luz_vale(&manual, &lidos, false), "com o obturador manual o fps não é coisa do automático");
        // Compartilhada: vale o que o driver diz, e o registro não manda.
        assert!(vigia_da_pouca_luz_vale(&manual, &lidos, true), "o driver lê Auto");
        let mut em_manual = lidos.clone();
        em_manual.insert(Propriedade::Exposicao, Lido { valor: -4, bandeiras: FLAGS_MANUAL });
        assert!(!vigia_da_pouca_luz_vale(&auto, &em_manual, true));
        assert!(vigia_da_pouca_luz_vale(&auto, &Lidos::new(), true), "sem exposição lida, vale");
        // A frase: com obturador manual, o conselho é a exposição manual; sem ele, a luz.
        let caps = caps_de_exemplo();
        let p = PoucaLuz::de(15, 30.0, &caps, false);
        assert_eq!(p, PoucaLuz { fps_agora: 15, fps: 30, com_manual: true });
        assert_eq!(p.texto(), "Pouca luz: 15 fps para clarear a imagem. Para 30 fps, use a exposição manual nos ajustes da câmera.");
        assert_eq!(p.curto(), "Pouca luz: 15 fps");
        assert_eq!(p.sem_conselho(), "Pouca luz: 15 fps para clarear a imagem.");
        let sem = PoucaLuz::de(15, 30.0, &Capacidades::new(), false);
        assert_eq!(sem.texto(), "Pouca luz: 15 fps para clarear a imagem. Mais luz no ambiente devolve os 30 fps.");
        assert!(!PoucaLuz::de(15, 30.0, &caps, true).com_manual, "compartilhada: os controles ficam apagados");
        let mut so_auto = caps.clone();
        so_auto.insert(Propriedade::Exposicao, f(-11, -1, 1, -6, FLAGS_AUTO));
        assert!(!PoucaLuz::de(15, 30.0, &so_auto, false).com_manual);
        assert_eq!(PoucaLuz::de(25, 29.97, &caps, false).fps, 30);
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert_eq!(p.texto(), "Low light: 15 fps to brighten the picture. For 30 fps, use manual exposure in Camera settings.");
            assert_eq!(sem.texto(), "Low light: 15 fps to brighten the picture. More light in the room brings back 30 fps.");
            assert_eq!(p.curto(), "Low light: 15 fps");
            assert_eq!(p.sem_conselho(), "Low light: 15 fps to brighten the picture.");
        });
    }

    #[test]
    fn os_textos_literais() {
        assert_eq!(frase_sem_controle(Propriedade::Brilho.nome()), "Esta câmera não oferece o brilho.");
        assert_eq!(frase_sem_controle(Propriedade::Ganho.nome()), "Esta câmera não oferece o ganho.");
        assert_eq!(frase_sem_controle(Propriedade::AntiCintilacao.nome()), "Esta câmera não oferece a anti-cintilação.");
        assert_eq!(frase_sem_controle(Propriedade::Balanco.nome()), "Esta câmera não oferece o Kelvin.");
        assert_eq!(FRASE_OUTRO_APP, "Outro app está controlando esta câmera. Feche-o para ajustar.");
        assert_eq!(FRASE_FOCO_FIXO, "Esta câmera tem foco fixo.");
        assert_eq!(Aba::GanhoEObturador.rotulo(), "Ganho e obturador");
        assert_eq!(Aba::TODAS.map(|a| a.rotulo()), ["Exposição", "Ganho e obturador", "Balanço", "Foco"]);
        assert_eq!(Balanco::GRADE.map(|b| b.rotulo()), ["Auto", "Incandescente", "Fluorescente", "Luz do dia", "Nublado", "Kelvin"]);
        assert_eq!(com_virgula(0.42, 2), "0,42");
        assert_eq!(com_virgula(-1.5, 1), "-1,5");
        assert_eq!(texto_do_valor(Propriedade::AntiCintilacao, 1), "50 Hz");
        for p in Propriedade::TODAS {
            assert_eq!(Propriedade::da_chave(p.chave()), Some(p));
        }
    }

    #[test]
    fn os_textos_em_ingles() {
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert_eq!(linha_lida(&lidos_de_exemplo()), "Gain 32 · 1/64 s · 5230 K");
            assert_eq!(frase_da_divergencia(Propriedade::Balanco, 5000, 5600), "The camera used 5000 K instead of 5600 K.");
            let mut caps = Capacidades::new();
            assert_eq!(frase_do_limite(Propriedade::Foco, &caps).as_deref(), Some("This camera has fixed focus."));
            caps.insert(Propriedade::Foco, f(0, 10, 1, 0, FLAGS_AUTO));
            assert_eq!(frase_do_limite(Propriedade::Foco, &caps).as_deref(), Some("This camera doesn't support manual focus."));
            assert_eq!(com_virgula(0.42, 2), "0.42");
            // As chaves continuam em português (o diário e o registro as usam assim).
            assert_eq!(AntiCintilacao::Desligada.rotulo(), "Desligada");
        });
    }

    #[test]
    fn a_frase_de_quem_nao_oferece() {
        let mut caps = Capacidades::new();
        assert_eq!(frase_do_limite(Propriedade::Foco, &caps).as_deref(), Some("Esta câmera tem foco fixo."));
        assert_eq!(frase_do_limite(Propriedade::Balanco, &caps).as_deref(), Some("Esta câmera não oferece o Kelvin."));
        caps.insert(Propriedade::Foco, f(0, 10, 1, 0, FLAGS_AUTO));
        assert_eq!(frase_do_limite(Propriedade::Foco, &caps).as_deref(), Some("Esta câmera não oferece o foco manual."));
        caps.insert(Propriedade::Foco, f(0, 10, 1, 0, FLAGS_AUTO | FLAGS_MANUAL));
        assert_eq!(frase_do_limite(Propriedade::Foco, &caps), None);
    }

    #[test]
    fn a_devolucao_em_auto_nao_e_divergencia() {
        // Medido na Integrated Webcam (01/10): o balanço devolvido a Auto lê o último manual. A
        // divergência só se vigia em manual e em Kelvin; com o balanço em Auto, nada.
        let r = Registro::default();
        assert!(!vigiadas(&r).contains(&Propriedade::Balanco));
        // E o plano em Auto não escreve nada, salvo a devolução do que o Quall tocou (Flags_Auto).
        let caps: Capacidades = [(Propriedade::Balanco, f(2800, 6500, 10, 4600, FLAGS_AUTO | FLAGS_MANUAL))].into();
        assert!(plano(&r, &caps, 30.0, &Lidos::new(), &BTreeSet::new()).is_empty());
        let tocados: BTreeSet<_> = [Propriedade::Balanco].into();
        let p = plano(&r, &caps, 30.0, &Lidos::new(), &tocados);
        assert_eq!(p, vec![Envio { prop: Propriedade::Balanco, valor: 4600, bandeiras: FLAGS_AUTO }]);
    }

    #[test]
    fn os_envios_agrupados_a_15_por_segundo() {
        let ms = Duration::from_millis;
        assert!(pode_enviar(None, ms(0)));
        assert!(!pode_enviar(Some(ms(100)), ms(150)));
        assert!(pode_enviar(Some(ms(100)), ms(167)));
        assert!(1000 / INTERVALO_DOS_ENVIOS.as_millis() <= 15);
    }

    #[test]
    fn o_roteiro_de_bancada() {
        let r = ler_roteiro_de_bancada("8:brilho=max; 4:brilho=min ;12:obturador=teto,ganho=meio;16:cintilacao=50;20:restaurar").unwrap();
        assert_eq!(r.len(), 5);
        assert_eq!(r[0].em, Duration::from_secs(4), "em ordem de hora");
        assert_eq!(r[0].ajustes, vec![("brilho".to_string(), AlvoDeBancada::Min)]);
        assert_eq!(r[2].ajustes, vec![("obturador".to_string(), AlvoDeBancada::Teto), ("ganho".to_string(), AlvoDeBancada::Meio)]);
        assert_eq!(r[3].ajustes[0].1, AlvoDeBancada::Hz50);
        assert_eq!(r[4].ajustes[0], ("restaurar".to_string(), AlvoDeBancada::Nenhum));
        assert_eq!(ler_roteiro_de_bancada("1,5:foco=120").unwrap()[0].ajustes[0].1, AlvoDeBancada::Valor(120));
        assert!(ler_roteiro_de_bancada("4:zoom=max").is_err());
        assert!(ler_roteiro_de_bancada("brilho=max").is_err());
        assert!(ler_roteiro_de_bancada("4:brilho").is_err());
        assert!(ler_roteiro_de_bancada("4:brilho=muito").is_err());
        assert!(ler_roteiro_de_bancada("").unwrap().is_empty());

        let caps = caps_de_exemplo();
        assert_eq!(acoes_de_bancada("brilho", AlvoDeBancada::Max, &caps, 30.0).unwrap(), vec![Acao::Brilho(64)]);
        assert_eq!(
            acoes_de_bancada("obturador", AlvoDeBancada::Teto, &caps, 30.0).unwrap(),
            vec![Acao::Exposicao(ModoDeExposicao::Manual), Acao::Obturador(-5)]
        );
        assert_eq!(acoes_de_bancada("obturador", AlvoDeBancada::Max, &caps, 60.0).unwrap()[1], Acao::Obturador(-6), "o máximo é o teto");
        assert_eq!(acoes_de_bancada("ganho", AlvoDeBancada::Meio, &caps, 30.0).unwrap()[1], Acao::Ganho(50));
        assert_eq!(acoes_de_bancada("kelvin", AlvoDeBancada::Min, &caps, 30.0).unwrap()[1], Acao::Kelvin(2800));
        assert_eq!(acoes_de_bancada("foco", AlvoDeBancada::Max, &caps, 30.0).unwrap()[1], Acao::FocoPosicao(1.0));
        assert_eq!(acoes_de_bancada("cintilacao", AlvoDeBancada::Desligada, &caps, 30.0).unwrap(), vec![Acao::AntiCintilacao(AntiCintilacao::Desligada)]);
        assert_eq!(acoes_de_bancada("restaurar", AlvoDeBancada::Nenhum, &caps, 30.0).unwrap(), vec![Acao::Restaurar]);
        assert!(acoes_de_bancada("brilho", AlvoDeBancada::Hz50, &caps, 30.0).is_err());
        assert!(acoes_de_bancada("brilho", AlvoDeBancada::Max, &Capacidades::new(), 30.0).is_err(), "a câmera sem brilho");
    }

    #[test]
    fn a_luma_de_um_quadro_bgra() {
        // 2×2, com uma linha de enchimento: branco, preto, vermelho puro, verde puro.
        let mut px = vec![0u8; 2 * 12];
        px[0..4].copy_from_slice(&[255, 255, 255, 255]);
        px[4..8].copy_from_slice(&[0, 0, 0, 255]);
        px[12..16].copy_from_slice(&[0, 0, 255, 255]);
        px[16..20].copy_from_slice(&[0, 255, 0, 255]);
        let l = luma_de_bgra(&px, 12, 2, 2).unwrap();
        let esperado = (255.0 + 0.0 + 0.2126 * 255.0 + 0.7152 * 255.0) / 4.0;
        assert!((l - esperado).abs() < 1e-9, "{l} {esperado}");
        assert_eq!(luma_de_bgra(&px, 4, 2, 2), None, "passo menor que a linha");
        assert_eq!(luma_de_bgra(&px[..10], 12, 2, 2), None, "buffer curto");
        let e = |valor, bandeiras| Some(Envio { prop: Propriedade::Brilho, valor, bandeiras });
        assert_eq!(
            linha_da_medida(3, "brilho", e(64, FLAGS_MANUAL), Some(Lido { valor: 64, bandeiras: 2 }), Some(101.234), Some(30.0), "ok"),
            "ajustes: medida passo=3 prop=brilho pedido=64 lido=64 flags=2 luma=101.23 fps=30.00 hr=ok"
        );
        assert!(
            linha_da_medida(4, "kelvin", e(4600, FLAGS_AUTO), Some(Lido { valor: 6500, bandeiras: 1 }), None, None, "ok").contains("pedido=auto lido=6500 flags=1"),
            "a devolução não é um pedido de valor"
        );
    }
}
