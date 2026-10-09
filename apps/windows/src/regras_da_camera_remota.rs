//! **O controle remoto da câmera, do lado de quem filma: a parte que é aritmética** (R9b,
//! `docs/controle-remoto-da-camera.md` §3.2, §6 e §12).
//!
//! Aqui não há Win32 nem núcleo, no molde de `regras_dos_controles.rs`: entram as faixas que a
//! câmera declarou, o registro e o lido; saem as **capacidades** que o filmador publica, o **pedido**
//! de um receptor traduzido em gestos do R9 ([`Acao`]) na ordem do contrato, o registro em JSON e o
//! **lido** em JSON. Quem fala com o `quall_core::camera_remota::Filmador` é `camera_remota.rs`, e
//! quem aplica é a thread dos ajustes (`ajustes_da_camera.rs`), que é a fila serial do dono da
//! câmera (§6, achado B1).
//!
//! # O que o Windows publica (§3.2, achado I7)
//!
//! | campo | descritor | por quê |
//! |---|---|---|
//! | `ev` | o **brilho** como deslocamento do padrão, `"unidade":"brilho"`, `"origem"` = o padrão | é o que o registro guarda (`regras_dos_controles.rs`, cabeçalho) |
//! | `iso` | o **ganho** do driver, `"unidade":"ganho"`, inteiro | idem |
//! | `obturadorNs` | de `2^min` ao teto de 1/fps, `"escala":"log2"`, inteiro | o driver anda em potências de 2 (R9 §3.1) |
//! | `kelvin` | a escala da tela (2000–10000 de 100 em 100, dentro da faixa do driver) | `faixa_do_kelvin` |
//! | `foco` / `focoPosicao` | só com `CameraControl_Focus`; sem ele, `camera_nao_oferece` | §12 |
//! | `toque` | nunca: `camera_nao_oferece` | as câmeras da bancada não têm ponto de interesse |
//!
//! **`Modo::Compartilhada`** (outro app abriu a câmera antes): `controles` vazio e todos os campos
//! em `limites` com `outro_app`. Os controles locais estão apagados por decisão do Bruno, e o
//! remoto recusa igual.

use serde_json::{json, Map, Value};

use crate::regras_dos_controles::{
    self as regras, faixa_do_kelvin, faixa_do_obturador, log2_dos_ns, ns_do_log2, Acao, AntiCintilacao, Balanco, Capacidades, Foco, Lidos,
    ModoDeExposicao, Propriedade, Registro,
};

/// Os campos (e a ação `toque`) que o contrato conhece, na ordem em que vão nas capacidades.
pub const CAMPOS: [&str; 12] =
    ["exposicao", "ev", "travaExposicao", "antiCintilacao", "iso", "obturadorNs", "balanco", "kelvin", "travaBalanco", "foco", "focoPosicao", "toque"]; // i18n: fora (o nome no fio)

/// Os códigos de "quem limita" que o Windows usa (§3.2).
pub const CAMERA_NAO_OFERECE: &str = "camera_nao_oferece";
pub const OUTRO_APP: &str = "outro_app";
/// O motivo de recusa do contrato quando a casca não consegue aplicar (§3.5).
pub const NAO_APLICADO: &str = "nao_aplicado";

/// O nome do campo do ajuste que cada propriedade do driver alimenta (para o `lido` e os
/// `divergentes`).
pub fn campo_da_propriedade(p: Propriedade) -> &'static str {
    match p {
        Propriedade::Exposicao => "obturadorNs",
        Propriedade::Ganho => "iso",
        Propriedade::Brilho => "ev",
        Propriedade::Balanco => "kelvin",
        Propriedade::Foco => "focoPosicao",
        Propriedade::AntiCintilacao => "antiCintilacao",
    }
}

fn valores(v: &[&str]) -> Value {
    json!({ "valores": v })
}

/// **As capacidades que o filmador publica** (§3.2), do mesmo mapa de faixas que monta o painel do
/// R9. `fps` é o do tipo nativo (o teto do obturador, R9 §3.1).
pub fn capacidades(caps: &Capacidades, fps: f64, compartilhada: bool) -> Value {
    let mut controles = Map::new();
    let mut limites = Map::new();
    if compartilhada {
        for c in CAMPOS {
            limites.insert(c.into(), Value::from(OUTRO_APP));
        }
        return json!({ "plataforma": "windows", "controles": controles, "limites": limites });
    }
    let mut falta = |campo: &str| {
        limites.insert(campo.into(), Value::from(CAMERA_NAO_OFERECE));
    };
    // A exposição manual (o obturador) é a porta do ganho e da trava: sem ela, a tela local também
    // não chega a eles (`modelo_dos_ajustes.rs`, as abas).
    // Um driver que declarasse a exposição numa unidade linear (e não em log2 de segundos) daria
    // durações absurdas: fora da janela plausível, o obturador não é oferecido (a revisão, achado 6).
    let exposicao = caps
        .get(&Propriedade::Exposicao)
        .filter(|f| f.tem_manual() && f.min >= LOG2_MINIMO && f.max <= LOG2_MAXIMO)
        .copied();
    match exposicao {
        Some(f) => {
            controles.insert("exposicao".into(), valores(&["auto", "manual"]));
            controles.insert("travaExposicao".into(), json!({}));
            let teto = faixa_do_obturador(&f, fps);
            // O piso e o teto **por fora** do arredondamento de `ns_do_log2`: o degrau 2^v que o
            // receptor monta (e arredonda) cai sempre dentro de `[min, max]` (achado 6).
            controles.insert(
                "obturadorNs".into(),
                json!({ "min": ns_por_baixo(teto.min), "max": ns_por_cima(teto.max), "inteiro": true, "escala": "log2" }),
            );
        }
        None => {
            falta("exposicao");
            falta("travaExposicao");
            falta("obturadorNs");
        }
    }
    match (exposicao, caps.get(&Propriedade::Ganho).filter(|f| f.tem_manual())) {
        (Some(_), Some(g)) => {
            controles.insert("iso".into(), json!({ "min": g.min, "max": g.max, "passo": g.passo, "inteiro": true, "unidade": "ganho" }));
        }
        _ => falta("iso"),
    }
    match caps.get(&Propriedade::Brilho).filter(|f| f.tem_manual()) {
        Some(b) => {
            controles.insert(
                "ev".into(),
                json!({
                    "min": i64::from(b.min) - i64::from(b.padrao),
                    "max": i64::from(b.max) - i64::from(b.padrao),
                    "passo": b.passo,
                    "inteiro": true,
                    "unidade": "brilho",
                    "origem": b.padrao,
                }),
            );
        }
        None => falta("ev"),
    }
    match caps.get(&Propriedade::AntiCintilacao) {
        Some(f) => {
            let v: Vec<&str> = AntiCintilacao::TODAS.into_iter().filter(|a| a.oferecida(f)).map(texto_da_cintilacao).collect();
            controles.insert("antiCintilacao".into(), valores(&v));
        }
        None => falta("antiCintilacao"),
    }
    match caps.get(&Propriedade::Balanco) {
        Some(f) if f.tem_manual() => {
            controles.insert("balanco".into(), valores(&["auto", "kelvin"]));
            let k = faixa_do_kelvin(f);
            controles.insert("kelvin".into(), json!({ "min": k.min, "max": k.max, "passo": k.passo, "inteiro": true }));
            controles.insert("travaBalanco".into(), json!({}));
        }
        Some(_) => {
            controles.insert("balanco".into(), valores(&["auto"]));
            falta("kelvin");
            falta("travaBalanco");
        }
        None => {
            falta("balanco");
            falta("kelvin");
            falta("travaBalanco");
        }
    }
    match caps.get(&Propriedade::Foco) {
        Some(f) if f.tem_manual() => {
            controles.insert("foco".into(), valores(&["auto", "travado", "manual"]));
            controles.insert("focoPosicao".into(), json!({ "min": 0, "max": 1, "passo": 0.01 }));
        }
        Some(_) => {
            controles.insert("foco".into(), valores(&["auto"]));
            falta("focoPosicao");
        }
        None => {
            falta("foco");
            falta("focoPosicao");
        }
    }
    falta("toque"); // i18n: fora (o nome no fio)
    json!({ "plataforma": "windows", "controles": controles, "limites": limites })
}

/// A janela plausível do `CameraControl_Exposure` em log2 de segundos: de 1/65536 s a 1024 s.
pub const LOG2_MINIMO: i32 = -16;
pub const LOG2_MAXIMO: i32 = 10;

/// 2^v s em ns, arredondado para baixo e para cima.
pub fn ns_por_baixo(v: i32) -> i64 {
    (1e9 * 2f64.powi(v)).floor() as i64
}
pub fn ns_por_cima(v: i32) -> i64 {
    (1e9 * 2f64.powi(v)).ceil() as i64
}

/// O valor de `antiCintilacao` no fio.
pub fn texto_da_cintilacao(a: AntiCintilacao) -> &'static str {
    match a {
        AntiCintilacao::Auto => "auto",
        AntiCintilacao::Hz50 => "50",
        AntiCintilacao::Hz60 => "60",
        AntiCintilacao::Desligada => "desligada",
    }
}

/// O registro no JSON do R9 (o mesmo que vai ao disco), para o núcleo, **com os inteiros sem `.0`**:
/// o `Registro` guarda `iso` e `ev` em `f64`, e o descritor diz `"inteiro":true` (a revisão, achado
/// 7; o Android lê `Int`).
pub fn registro_em_json(r: &Registro) -> String {
    let Ok(Value::Object(mut o)) = serde_json::to_value(r) else { return "{}".into() };
    for v in o.values_mut() {
        if let Some(x) = v.as_f64().filter(|x| v.is_f64() && x.fract() == 0.0 && x.abs() < 9.0e15) {
            *v = Value::from(x as i64);
        }
    }
    Value::Object(o).to_string()
}

// =============================================================================================
// O pedido de um receptor
// =============================================================================================

/// Um pedido aceito pelo núcleo, como `proximo_pedido` o entrega (§6).
#[derive(Clone, Debug, PartialEq)]
pub struct PedidoRemoto {
    pub n: u64,
    pub autor: Option<String>,
    pub ajuste: Map<String, Value>,
    pub restaurar: bool,
    pub tem_toque: bool,
}

/// Lê o JSON de `proximo_pedido`. `None` quando ele não se lê (o núcleo nunca manda assim).
pub fn ler_pedido(json: &str) -> Option<PedidoRemoto> {
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(json) else { return None };
    Some(PedidoRemoto {
        n: o.get("n")?.as_u64()?,
        autor: o.get("autor").and_then(Value::as_str).map(str::to_string),
        ajuste: o.get("ajuste").and_then(Value::as_object).cloned().unwrap_or_default(),
        restaurar: o.get("restaurar").and_then(Value::as_bool).unwrap_or(false),
        tem_toque: o.get("toque").is_some_and(|t| !t.is_null()), // i18n: fora (o nome no fio)
    })
}

fn texto<'a>(a: &'a Map<String, Value>, campo: &str) -> Option<&'a str> {
    a.get(campo).and_then(Value::as_str)
}

fn numero(a: &Map<String, Value>, campo: &str) -> Option<f64> {
    a.get(campo).and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// **O pedido em gestos do R9, na ordem do contrato** (§6, achado B3): `restaurar` → os modos
/// (`exposicao`, `balanco`, `foco`) → as travas → os valores (`ev`, `iso`, `obturadorNs`,
/// `kelvin`, `focoPosicao`, `antiCintilacao`). Aplicados em sequência por
/// [`regras::aplicar_acao`], "partir do lido" só vale para o que o pedido **não** trouxe: o modo
/// Manual parte do lido, e o `iso` pedido vem depois e fica.
///
/// Os valores chegam conferidos pelo núcleo contra as capacidades daqui; o corte pela faixa de agora
/// (R9 §2.2) acontece aqui mesmo. `Err` traz o motivo de recusa.
pub fn acoes_do_pedido(p: &PedidoRemoto, caps: &Capacidades, fps: f64) -> Result<Vec<Acao>, &'static str> {
    if p.tem_toque {
        // As capacidades nunca listam `toque`; o núcleo já recusaria. Por garantia.
        return Err(NAO_APLICADO);
    }
    let a = &p.ajuste;
    let mut v = Vec::new();
    if p.restaurar {
        v.push(Acao::Restaurar);
    }
    // Os modos.
    match texto(a, "exposicao") {
        Some("auto") => v.push(Acao::Exposicao(ModoDeExposicao::Auto)),
        Some("manual") => v.push(Acao::Exposicao(ModoDeExposicao::Manual)),
        Some(_) => return Err(NAO_APLICADO),
        None => {}
    }
    if let Some(b) = a.get("balanco") {
        let b: Balanco = serde_json::from_value(b.clone()).map_err(|_| NAO_APLICADO)?;
        v.push(Acao::Balanco(b));
    }
    if let Some(f) = a.get("foco") {
        let f: Foco = serde_json::from_value(f.clone()).map_err(|_| NAO_APLICADO)?;
        v.push(Acao::Foco(f));
    }
    // As travas.
    if let Some(t) = a.get("travaExposicao") {
        v.push(Acao::TravarExposicao(t.as_bool().ok_or(NAO_APLICADO)?));
    }
    if let Some(t) = a.get("travaBalanco") {
        v.push(Acao::TravarBalanco(t.as_bool().ok_or(NAO_APLICADO)?));
    }
    // Os valores, cortados pela faixa de agora.
    if let Some(ev) = numero(a, "ev") {
        let f = caps.get(&Propriedade::Brilho).ok_or(NAO_APLICADO)?;
        v.push(Acao::Brilho(f.cortar(i64::from(f.padrao) + ev.round() as i64)));
    }
    if let Some(g) = numero(a, "iso") {
        let f = caps.get(&Propriedade::Ganho).ok_or(NAO_APLICADO)?;
        v.push(Acao::Ganho(f.cortar(g.round() as i64)));
    }
    if let Some(ns) = numero(a, "obturadorNs") {
        let f = caps.get(&Propriedade::Exposicao).ok_or(NAO_APLICADO)?;
        // O log2 mais perto, cortado no teto de 1/fps (§3.2: "a casca do filmador arredonda").
        v.push(Acao::Obturador(faixa_do_obturador(f, fps).cortar(i64::from(log2_dos_ns(ns.round() as i64)))));
    }
    if let Some(k) = numero(a, "kelvin") {
        let f = caps.get(&Propriedade::Balanco).ok_or(NAO_APLICADO)?;
        v.push(Acao::Kelvin(faixa_do_kelvin(f).cortar(k.round() as i64)));
    }
    if let Some(x) = numero(a, "focoPosicao") {
        caps.get(&Propriedade::Foco).ok_or(NAO_APLICADO)?;
        v.push(Acao::FocoPosicao(x));
    }
    if let Some(c) = a.get("antiCintilacao") {
        let c: AntiCintilacao = serde_json::from_value(c.clone()).map_err(|_| NAO_APLICADO)?;
        v.push(Acao::AntiCintilacao(c));
    }
    Ok(v)
}

/// **Aplica um pedido ao registro**, sobre o lido de agora: o registro que fica valendo, ou o motivo
/// de recusa. "Sem lido (o Windows sem `Get`), recusa com `nao_aplicado` em vez de aplicar um manual
/// vazio" (§6): um modo manual que terminou sem o valor dele é recusado, e o registro não muda.
pub fn aplicar_pedido(r: &Registro, p: &PedidoRemoto, caps: &Capacidades, lidos: &Lidos, fps: f64) -> Result<Registro, &'static str> {
    let acoes = acoes_do_pedido(p, caps, fps)?;
    let mut novo = r.clone();
    for a in acoes {
        regras::aplicar_acao(&mut novo, a, caps, lidos);
    }
    if manual_vazio(&novo, caps) {
        return Err(NAO_APLICADO);
    }
    Ok(novo)
}

/// Um modo manual sem o valor que ele precisa (o lido faltou ao passar).
pub fn manual_vazio(r: &Registro, caps: &Capacidades) -> bool {
    let tem = |p: Propriedade| caps.contains_key(&p);
    (r.exposicao == ModoDeExposicao::Manual
        && ((tem(Propriedade::Ganho) && r.iso.is_none()) || (tem(Propriedade::Exposicao) && r.obturador_ns.is_none())))
        || (r.balanco == Balanco::Kelvin && r.kelvin.is_none())
        || (r.foco == Foco::Manual && r.foco_posicao.is_none())
}

// =============================================================================================
// O lido e a permissão
// =============================================================================================

/// **O lido** (§3.3), só o que o Windows lê, na unidade do registro: o ganho em `iso`, o obturador
/// em ns, o balanço em Kelvin, o foco de 0 a 1; e os campos em que a tela daqui diz "A câmera usou
/// {lido} em vez de {pedido}." (`divergentes`).
pub fn lido(lidos: &Lidos, caps: &Capacidades, divergentes: &[Propriedade]) -> Value {
    let mut o = Map::new();
    if let Some(l) = lidos.get(&Propriedade::Ganho) {
        o.insert("iso".into(), Value::from(l.valor));
    }
    if let Some(l) = lidos.get(&Propriedade::Exposicao) {
        o.insert("obturadorNs".into(), Value::from(ns_do_log2(l.valor)));
    }
    if let Some(l) = lidos.get(&Propriedade::Balanco) {
        o.insert("kelvin".into(), Value::from(l.valor));
    }
    if let (Some(l), Some(f)) = (lidos.get(&Propriedade::Foco), caps.get(&Propriedade::Foco)) {
        let x = (f.fracao(i64::from(l.valor)) * 100.0).round() / 100.0;
        o.insert("focoPosicao".into(), json!(x));
    }
    o.insert("divergentes".into(), Value::from(divergentes.iter().map(|p| campo_da_propriedade(*p)).collect::<Vec<_>>()));
    Value::Object(o)
}

/// O nome do arquivo da opção "Permitir controle remoto da câmera", na pasta de dados.
pub const ARQUIVO_DA_PERMISSAO: &str = "camera-controle-remoto.txt";

/// A opção guardada: só `sim` liga. Ausente, ilegível ou outra coisa: desligada (o padrão).
pub fn ler_permissao(conteudo: &str) -> bool {
    conteudo.trim_start_matches('\u{FEFF}').trim().eq_ignore_ascii_case("sim")
}

pub fn texto_da_permissao(permite: bool) -> &'static str {
    if permite {
        "sim"
    } else {
        "nao"
    }
}

/// O nome de quem está controlando, do estado do filmador do núcleo (`controlado_por`), ou `None`.
pub fn controlado_por(estado_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(estado_json).ok()?;
    let nome = v.get("controlado_por")?.get("nome")?.as_str()?.trim();
    (!nome.is_empty()).then(|| nome.to_string())
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::regras_dos_controles::{Faixa, Lido, FLAGS_AUTO, FLAGS_MANUAL};

    fn f(min: i32, max: i32, passo: i32, padrao: i32, bandeiras: i32) -> Faixa {
        Faixa::do_get_range(min, max, passo, padrao, bandeiras).unwrap()
    }

    /// A webcam de exemplo de `regras_dos_controles.rs`.
    fn caps() -> Capacidades {
        let mut c = Capacidades::new();
        c.insert(Propriedade::Exposicao, f(-11, -1, 1, -6, FLAGS_AUTO | FLAGS_MANUAL));
        c.insert(Propriedade::Ganho, f(0, 100, 1, 0, FLAGS_MANUAL));
        c.insert(Propriedade::Brilho, f(-64, 64, 1, 0, FLAGS_MANUAL));
        c.insert(Propriedade::Balanco, f(2800, 6500, 10, 4600, FLAGS_AUTO | FLAGS_MANUAL));
        c.insert(Propriedade::AntiCintilacao, f(0, 2, 1, 1, FLAGS_MANUAL));
        c
    }

    fn lidos() -> Lidos {
        let mut l = Lidos::new();
        l.insert(Propriedade::Exposicao, Lido { valor: -6, bandeiras: FLAGS_AUTO });
        l.insert(Propriedade::Ganho, Lido { valor: 32, bandeiras: FLAGS_MANUAL });
        l.insert(Propriedade::Brilho, Lido { valor: 0, bandeiras: FLAGS_MANUAL });
        l.insert(Propriedade::Balanco, Lido { valor: 5230, bandeiras: FLAGS_AUTO });
        l
    }

    fn pedido(ajuste: Value) -> PedidoRemoto {
        PedidoRemoto { n: 7, autor: Some("Pixel".into()), ajuste: ajuste.as_object().cloned().unwrap(), restaurar: false, tem_toque: false }
    }

    #[test]
    fn as_capacidades_da_webcam_da_bancada() {
        let c = capacidades(&caps(), 30.0, false);
        let ctl = &c["controles"];
        assert_eq!(c["plataforma"], "windows");
        assert_eq!(ctl["exposicao"]["valores"], json!(["auto", "manual"]));
        // O brilho como deslocamento do padrão, com a origem (achado I7).
        assert_eq!(ctl["ev"], json!({"min": -64, "max": 64, "passo": 1, "inteiro": true, "unidade": "brilho", "origem": 0}));
        assert_eq!(ctl["iso"]["unidade"], "ganho");
        assert_eq!(ctl["iso"]["inteiro"], true);
        // O obturador de 2^-11 ao teto de 1/30 (2^-5), em log2.
        assert_eq!(ctl["obturadorNs"]["escala"], "log2");
        assert_eq!(ctl["obturadorNs"]["min"], json!(ns_por_baixo(-11)));
        assert_eq!(ctl["obturadorNs"]["max"], json!(ns_por_cima(-5)));
        // O degrau arredondado (o que o receptor manda) cai dentro da faixa publicada.
        for v in -11..=-5 {
            let ns = ns_do_log2(v);
            assert!(ns >= ns_por_baixo(-11) && ns <= ns_por_cima(-5), "2^{v}");
        }
        assert_eq!(ns_por_baixo(-12), 244140);
        assert_eq!(ns_por_cima(-12), 244141);
        // Um driver com a exposição numa unidade que não é log2 fica sem obturador.
        let mut linear = caps();
        linear.insert(Propriedade::Exposicao, f(1, 10000, 1, 100, FLAGS_AUTO | FLAGS_MANUAL));
        assert_eq!(capacidades(&linear, 30.0, false)["limites"]["obturadorNs"], CAMERA_NAO_OFERECE);
        assert_eq!(ctl["kelvin"], json!({"min": 2800, "max": 6500, "passo": 100, "inteiro": true}));
        assert_eq!(ctl["balanco"]["valores"], json!(["auto", "kelvin"]));
        assert_eq!(ctl["antiCintilacao"]["valores"], json!(["auto", "50", "60", "desligada"]));
        // Sem foco na câmera da bancada (§12), e nunca o toque.
        assert!(ctl.get("foco").is_none() && ctl.get("toque").is_none());
        assert_eq!(c["limites"]["foco"], CAMERA_NAO_OFERECE);
        assert_eq!(c["limites"]["focoPosicao"], CAMERA_NAO_OFERECE);
        assert_eq!(c["limites"]["toque"], CAMERA_NAO_OFERECE);
        // Cabe no teto do núcleo (2.048 bytes) com folga.
        assert!(c.to_string().len() < 1100, "{} bytes", c.to_string().len());
        // A 60 fps o teto do obturador desce para 2^-6.
        assert_eq!(capacidades(&caps(), 60.0, false)["controles"]["obturadorNs"]["max"], json!(ns_do_log2(-6)));
    }

    #[test]
    fn o_modo_compartilhado_nao_oferece_nada() {
        let c = capacidades(&caps(), 30.0, true);
        assert_eq!(c["controles"], json!({}));
        for campo in CAMPOS {
            assert_eq!(c["limites"][campo], OUTRO_APP, "{campo}");
        }
    }

    #[test]
    fn a_camera_pobre_diz_o_que_falta() {
        let mut c = Capacidades::new();
        c.insert(Propriedade::Brilho, f(0, 255, 1, 128, FLAGS_MANUAL));
        c.insert(Propriedade::Foco, f(0, 10, 1, 0, FLAGS_AUTO));
        c.insert(Propriedade::Ganho, f(0, 100, 1, 0, FLAGS_MANUAL));
        let j = capacidades(&c, 30.0, false);
        assert_eq!(j["controles"]["ev"]["origem"], 128);
        assert_eq!(j["controles"]["ev"]["min"], -128);
        assert_eq!(j["controles"]["foco"]["valores"], json!(["auto"]));
        for campo in ["exposicao", "iso", "obturadorNs", "balanco", "kelvin", "antiCintilacao", "focoPosicao"] {
            assert_eq!(j["limites"][campo], CAMERA_NAO_OFERECE, "{campo}");
        }
    }

    #[test]
    fn passar_para_manual_com_iso_fica_com_o_iso_pedido() {
        // §6: `{"exposicao":"manual","iso":800}` fica com o ISO pedido, e o obturador parte do lido.
        let r = aplicar_pedido(&Registro::default(), &pedido(json!({"exposicao":"manual","iso":64})), &caps(), &lidos(), 30.0).unwrap();
        assert_eq!(r.exposicao, ModoDeExposicao::Manual);
        assert_eq!(r.iso, Some(64.0));
        assert_eq!(r.obturador_ns, Some(ns_do_log2(-6)), "do lido");
        // Só o modo: os dois do lido.
        let r = aplicar_pedido(&Registro::default(), &pedido(json!({"exposicao":"manual"})), &caps(), &lidos(), 30.0).unwrap();
        assert_eq!(r.iso, Some(32.0));
    }

    #[test]
    fn a_ordem_do_contrato() {
        let mut p = pedido(json!({"iso": 10, "antiCintilacao": "50", "exposicao": "manual", "travaBalanco": true, "balanco": "auto"}));
        p.restaurar = true;
        let v = acoes_do_pedido(&p, &caps(), 30.0).unwrap();
        assert_eq!(
            v,
            vec![
                Acao::Restaurar,
                Acao::Exposicao(ModoDeExposicao::Manual),
                Acao::Balanco(Balanco::Auto),
                Acao::TravarBalanco(true),
                Acao::Ganho(10),
                Acao::AntiCintilacao(AntiCintilacao::Hz50),
            ]
        );
    }

    #[test]
    fn os_valores_voltam_a_unidade_do_driver() {
        let mut r = Registro { exposicao: ModoDeExposicao::Manual, iso: Some(1.0), obturador_ns: Some(ns_do_log2(-7)), ..Default::default() };
        // O obturador: o log2 mais perto, cortado no teto de 1/30.
        r = aplicar_pedido(&r, &pedido(json!({"obturadorNs": ns_do_log2(-9)})), &caps(), &lidos(), 30.0).unwrap();
        assert_eq!(r.obturador_ns, Some(ns_do_log2(-9)));
        r = aplicar_pedido(&r, &pedido(json!({"obturadorNs": 1_000_000_000})), &caps(), &lidos(), 30.0).unwrap();
        assert_eq!(r.obturador_ns, Some(ns_do_log2(-5)), "o teto de 1/30");
        // O brilho: o deslocamento do padrão.
        let r = aplicar_pedido(&Registro::default(), &pedido(json!({"ev": 12})), &caps(), &lidos(), 30.0).unwrap();
        assert_eq!(r.ev, 12.0);
        // O Kelvin na escala da tela.
        let r = aplicar_pedido(&Registro { balanco: Balanco::Kelvin, kelvin: Some(5000), ..Default::default() }, &pedido(json!({"kelvin": 9000})), &caps(), &lidos(), 30.0).unwrap();
        assert_eq!(r.kelvin, Some(6500));
    }

    #[test]
    fn manual_sem_lido_e_recusado() {
        let mut sem = lidos();
        sem.remove(&Propriedade::Ganho);
        let e = aplicar_pedido(&Registro::default(), &pedido(json!({"exposicao":"manual"})), &caps(), &sem, 30.0);
        assert_eq!(e, Err(NAO_APLICADO));
        // Com o ganho no pedido, o lido do ganho não faz falta.
        assert!(aplicar_pedido(&Registro::default(), &pedido(json!({"exposicao":"manual","iso":5})), &caps(), &sem, 30.0).is_ok());
    }

    #[test]
    fn o_pedido_do_nucleo_se_le() {
        let p = ler_pedido(r#"{"n":5,"autor":"OBS no Dell","autor_id":"dell-7f2a","ajuste":{"exposicao":"manual","iso":800},"restaurar":false,"toque":null}"#).unwrap();
        assert_eq!(p.n, 5);
        assert_eq!(p.autor.as_deref(), Some("OBS no Dell"));
        assert_eq!(p.ajuste["iso"], 800);
        assert!(!p.restaurar && !p.tem_toque);
        assert!(ler_pedido("lixo").is_none());
        assert!(acoes_do_pedido(&PedidoRemoto { tem_toque: true, ..p }, &caps(), 30.0).is_err());
    }

    #[test]
    fn o_lido_e_a_permissao() {
        let mut c = caps();
        c.insert(Propriedade::Foco, f(0, 250, 5, 0, FLAGS_AUTO | FLAGS_MANUAL));
        let mut l = lidos();
        l.insert(Propriedade::Foco, Lido { valor: 125, bandeiras: FLAGS_AUTO });
        let j = lido(&l, &c, &[Propriedade::Exposicao]);
        assert_eq!(j["iso"], 32);
        assert_eq!(j["obturadorNs"], json!(ns_do_log2(-6)));
        assert_eq!(j["kelvin"], 5230);
        assert_eq!(j["focoPosicao"], json!(0.5));
        assert_eq!(j["divergentes"], json!(["obturadorNs"]));
        assert!(ler_permissao("sim\r\n") && ler_permissao("\u{FEFF}SIM"));
        assert!(!ler_permissao("") && !ler_permissao("nao") && !ler_permissao("talvez"));
        assert!(ler_permissao(texto_da_permissao(true)) && !ler_permissao(texto_da_permissao(false)));
        assert_eq!(controlado_por(r#"{"controlado_por":{"nome":"OBS no Dell","ha_ms":10}}"#).as_deref(), Some("OBS no Dell"));
        assert_eq!(controlado_por(r#"{"controlado_por":null}"#), None);
    }

    #[test]
    fn o_registro_em_json_e_o_do_disco() {
        let r = Registro { exposicao: ModoDeExposicao::Manual, iso: Some(64.0), ev: -3.0, ..Default::default() };
        let texto = registro_em_json(&r);
        assert!(texto.contains("\"iso\":64") && !texto.contains("64.0"), "{texto}");
        assert!(texto.contains("\"ev\":-3") && !texto.contains("-3.0"), "{texto}");
        let j: Value = serde_json::from_str(&texto).unwrap();
        assert_eq!(serde_json::from_value::<Registro>(j.clone()).unwrap(), r, "volta igual");
        assert_eq!(j["exposicao"], "manual");
        assert_eq!(j["antiCintilacao"], "auto");
        assert_eq!(j["balanco"], "auto");
        assert_eq!(j["travaExposicao"], false);
    }
}
