//! Os testes do controle remoto da câmera: as regras puras com o relógio na mão, o canal simulado
//! perdendo, duplicando e embaralhando, e uma sessão de verdade por 127.0.0.1.

use super::*;

const SESSAO_A: u64 = 101;
const SESSAO_B: u64 = 202;

fn par(nome: &str) -> ParDaSessao {
    ParDaSessao { id: format!("{nome}-id"), nome: nome.into() }
}

/// As capacidades de um Android com `MANUAL_SENSOR` (as do exemplo do contrato §3.2).
fn caps_android() -> String {
    json!({
        "plataforma": "android",
        "nomeDaCamera": "Traseira",
        "controles": {
            "exposicao": {"valores": ["auto", "manual"]},
            "ev": {"min": -2.0, "max": 2.0, "passo": 0.1},
            "travaExposicao": {},
            "antiCintilacao": {"valores": ["auto", "50", "60", "desligada"]},
            "iso": {"min": 50, "max": 3200, "inteiro": true, "analogicoMax": 800},
            "obturadorNs": {"min": 100000, "max": 33333333, "inteiro": true},
            "balanco": {"valores": ["auto", "incandescente", "fluorescente", "luzDoDia", "nublado", "kelvin"]},
            "kelvin": {"min": 2000, "max": 10000, "passo": 100, "inteiro": true},
            "travaBalanco": {},
            "foco": {"valores": ["auto", "travado", "manual"]},
            "focoPosicao": {"min": 0.0, "max": 1.0, "passo": 0.01},
            "toque": {}
        },
        "limites": {}
    })
    .to_string()
}

/// As do Mac: só as travas e o ponto (R9 §1).
fn caps_mac() -> String {
    json!({
        "plataforma": "macos",
        "nomeDaCamera": "Câmera FaceTime HD",
        "controles": {
            "travaExposicao": {},
            "travaBalanco": {},
            "foco": {"valores": ["auto", "travado"]},
            "toque": {}
        },
        "limites": {"ev": "macos", "iso": "macos", "obturadorNs": "macos", "kelvin": "macos",
                    "antiCintilacao": "macos", "focoPosicao": "macos"}
    })
    .to_string()
}

fn ajuste_padrao() -> String {
    json!({"exposicao": "auto", "ev": 0.0, "travaExposicao": false, "antiCintilacao": "auto",
           "balanco": "auto", "travaBalanco": false, "foco": "auto"})
    .to_string()
}

/// A casca do filmador, de mentira: aplica o parcial por cima do registro, completando o que o R9
/// manda ler da câmera ("Passar para Manual" parte do lido).
fn aplicar_como_a_casca(f: &mut NucleoDoFilmador, agora: Instant) -> Vec<Value> {
    let mut aplicados = Vec::new();
    while let Some(p) = f.proximo_pedido(agora) {
        let p: Value = serde_json::from_str(&p).unwrap();
        let mut reg = f.ajuste().clone();
        if p["restaurar"] == true {
            reg = serde_json::from_str::<Value>(&ajuste_padrao()).unwrap().as_object().unwrap().clone();
        }
        for (k, v) in p["ajuste"].as_object().unwrap() {
            reg.insert(k.clone(), v.clone());
        }
        if reg.get("exposicao") == Some(&json!("manual")) {
            reg.entry("iso").or_insert(json!(400));
            reg.entry("obturadorNs").or_insert(json!(16_666_666));
        }
        f.definir_ajuste(&Value::Object(reg).to_string(), p["n"].as_u64().unwrap(), agora).unwrap();
        aplicados.push(p);
    }
    aplicados
}

fn filmador_com_camera(t0: Instant) -> NucleoDoFilmador {
    let mut f = NucleoDoFilmador::novo();
    f.permitir(true);
    f.definir_camera(Some((&caps_android(), &ajuste_padrao()))).unwrap();
    let _ = t0;
    f
}

/// Entrega de verdade, sem perda: o que um lado deve vai direto ao outro.
fn trocar(f: &mut NucleoDoFilmador, sessao: u64, r: &mut NucleoDoReceptor, agora: Instant) {
    for _ in 0..4 {
        for t in r.devidas(agora) {
            f.receber(sessao, Some(&par(&format!("rx{sessao}"))), &t, agora);
        }
        for (_, t) in f.devidas(sessao, agora) {
            r.receber(&t, agora);
        }
    }
}

fn receptor_pronto(f: &mut NucleoDoFilmador, sessao: u64, t: Instant) -> NucleoDoReceptor {
    let mut r = NucleoDoReceptor::novo();
    r.comecar_sessao(sessao, t);
    trocar(f, sessao, &mut r, t);
    assert_eq!(r.situacao(), situacao::PRONTO, "{}", r.estado_json(t));
    r
}

fn v(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

// ---------------------------------------------------------------------------------------------
// O fio
// ---------------------------------------------------------------------------------------------

#[test]
fn o_formato_no_fio_e_o_do_contrato() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = NucleoDoReceptor::novo();
    r.comecar_sessao(SESSAO_A, t);
    let ola = r.devidas(t);
    assert_eq!(ola.len(), 1);
    assert_eq!(v(&ola[0]), json!({"app": "camera", "v": 1, "tipo": "ola", "cap": 0}));

    f.receber(SESSAO_A, Some(&par("OBS no Dell")), &ola[0], t);
    let saida = f.devidas(SESSAO_A, t);
    let caps = v(&saida[0].1);
    assert_eq!(caps["tipo"], "capacidades");
    assert_eq!(caps["cap"], 1);
    assert_eq!(caps["epoca"].as_str().unwrap().len(), 8);
    assert_eq!(caps["capacidades"]["controles"]["iso"]["max"], 3200);
    let est = v(&saida[1].1);
    let chaves: Vec<&str> = est.as_object().unwrap().keys().map(String::as_str).collect();
    for k in ["app", "v", "tipo", "epoca", "n", "versao", "camera", "cap", "permite", "ajuste", "lido", "autor", "seu"] {
        assert!(chaves.contains(&k), "falta {k} no estado: {est}");
    }
    assert_eq!(est["seu"], json!({"seq": 0, "recusa": null}));
    assert_eq!(est["autor"], Value::Null);

    for (_, m) in saida {
        r.receber(&m, t);
    }
    r.pedir(r#"{"exposicao":"manual","iso":800.0}"#, t).unwrap();
    let pedido = v(&r.devidas(t)[0]);
    assert_eq!(pedido["tipo"], "pedido");
    assert_eq!(pedido["seq"], 1);
    assert_eq!(pedido["vista"], est["versao"]);
    assert_eq!(pedido["camera"], 1);
    assert_eq!(pedido["vistas"], json!({"exposicao": est["versao"], "iso": est["versao"]}));
    // O inteiro sai sem `.0`: o Android lê `iso` como Int.
    assert_eq!(serde_json::to_string(&pedido["ajuste"]["iso"]).unwrap(), "800");
}

#[test]
fn o_estado_tipico_cabe_num_pedaco_sctp() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let cheio = json!({"exposicao": "manual", "ev": -0.7, "travaExposicao": false, "travaIso": 400,
        "travaObturadorNs": 16666666, "iso": 1600, "obturadorNs": 8333333, "antiCintilacao": "60",
        "balanco": "kelvin", "kelvin": 5600, "travaBalanco": false,
        "travaGanhos": [1.9921875, 1.0, 1.0, 2.34375], "foco": "manual", "focoPosicao": 0.37});
    f.definir_ajuste(&cheio.to_string(), 0, t).unwrap();
    f.definir_lido(r#"{"iso":1600,"obturadorNs":8333333,"kelvin":5550,"abertura":1.7,"focoPosicao":0.37,"divergentes":["kelvin"]}"#).unwrap();
    f.receber(SESSAO_A, Some(&par("Um nome de aparelho bem comprido, com acento ção")), &mensagem("ola", Objeto::new()), t);
    let saida = f.devidas(SESSAO_A, t);
    for (d, m) in &saida {
        eprintln!("{d:?}: {} bytes", m.len());
        assert!(m.len() <= ALVO_DA_MENSAGEM, "{d:?} tem {} bytes: {m}", m.len());
    }
}

#[test]
fn quem_nao_disse_ola_nao_recebe_nada() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    // Uma mensagem de outro app abre a sessão, mas não é `ola`.
    f.receber(SESSAO_A, Some(&par("antigo")), r#"{"app":"teleprompter","v":1,"tipo":"estado"}"#, t);
    assert!(f.devidas(SESSAO_A, t).is_empty());
    assert!(f.devidas(SESSAO_A, t + Duration::from_secs(5)).is_empty());
    assert_eq!(f.contadores().de_outro_app, 1);
}

#[test]
fn versao_tipo_e_lixo_sao_contados_sem_derrubar() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = receptor_pronto(&mut f, SESSAO_A, t);
    for lixo in [
        r#"{"app":"camera","v":2,"tipo":"estado"}"#,
        r#"{"app":"camera","v":1,"tipo":"novidade","x":1}"#,
        "não é json",
        r#"[1,2,3]"#,
        r#"{"app":"camera","v":1}"#,
    ] {
        r.receber(lixo, t);
        f.receber(SESSAO_A, Some(&par("x")), lixo, t);
    }
    let grande = format!(r#"{{"app":"camera","v":1,"tipo":"ola","x":"{}"}}"#, "a".repeat(5000));
    f.receber(SESSAO_A, Some(&par("x")), &grande, t);
    let cf = f.contadores();
    assert_eq!((cf.de_outra_versao, cf.tipo_desconhecido, cf.invalidas), (1, 1, 4));
    let cr = r.contadores();
    assert_eq!((cr.de_outra_versao, cr.tipo_desconhecido, cr.invalidas), (1, 1, 3));
    assert_eq!(r.situacao(), situacao::PRONTO);
}

// ---------------------------------------------------------------------------------------------
// O caminho feliz, a permissão e o "Controlado por"
// ---------------------------------------------------------------------------------------------

#[test]
fn o_pedido_vai_a_casca_volta_aplicado_e_mostra_quem_controlou() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = receptor_pronto(&mut f, SESSAO_A, t);
    r.pedir(r#"{"exposicao":"manual"}"#, t).unwrap();
    // O pendente aparece por cima do aplicado, na hora.
    let e = v(&r.estado_json(t));
    assert_eq!(e["ajuste"]["exposicao"], "manual");
    assert_eq!(e["aplicado"]["exposicao"], "auto");
    for m in r.devidas(t) {
        assert_eq!(f.receber(SESSAO_A, Some(&par("rx101")), &m, t), mudou_no_filmador::PEDIDO);
    }
    let aplicados = aplicar_como_a_casca(&mut f, t);
    assert_eq!(aplicados.len(), 1);
    assert_eq!(aplicados[0]["autor"], "rx101");
    assert_eq!(aplicados[0]["ajuste"], json!({"exposicao": "manual"}));
    let ef = v(&f.estado_json(t + Duration::from_secs(1)));
    assert_eq!(ef["controlado_por"]["nome"], "rx101");
    assert!(v(&f.estado_json(t + CONTROLADO_POR_DURA))["controlado_por"].is_null());

    trocar(&mut f, SESSAO_A, &mut r, t);
    let e = v(&r.estado_json(t));
    assert_eq!(e["aplicado"]["exposicao"], "manual");
    assert_eq!(e["aplicado"]["iso"], 400, "a casca partiu do lido");
    assert_eq!(e["pendente"], json!({}));
    assert_eq!(e["autor"], "rx101");
    assert!(e["recusa"].is_null());
}

#[test]
fn com_a_opcao_desligada_o_pedido_e_recusado_e_o_receptor_ve_apagado() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = receptor_pronto(&mut f, SESSAO_A, t);
    r.pedir(r#"{"ev":1.0}"#, t).unwrap();
    f.permitir(false);
    for m in r.devidas(t) {
        f.receber(SESSAO_A, Some(&par("rx")), &m, t);
    }
    assert!(f.proximo_pedido(t).is_none());
    let saida = f.devidas(SESSAO_A, t);
    let recusa = v(&saida[0].1);
    assert_eq!(recusa, json!({"app":"camera","v":1,"tipo":"recusa","seq":1,"motivo":"nao_permitido","campo":null}));
    let estado = v(&saida.last().unwrap().1);
    assert_eq!(estado["permite"], false);
    assert_eq!(estado["seu"]["recusa"]["motivo"], "nao_permitido");
    for (_, m) in saida {
        r.receber(&m, t);
    }
    let e = v(&r.estado_json(t));
    assert_eq!(e["situacao"], "nao_permitido");
    assert_eq!(e["recusa"]["motivo"], "nao_permitido");
    assert_eq!(e["aplicado"]["ev"], 0.0, "os valores continuam à vista");
    assert!(r.pedir(r#"{"ev":1.0}"#, t).is_err(), "apagado não pede");
}

#[test]
fn o_padrao_e_desligado() {
    let t = Instant::now();
    let mut f = NucleoDoFilmador::novo();
    f.definir_camera(Some((&caps_android(), &ajuste_padrao()))).unwrap();
    assert_eq!(v(&f.estado_json(t))["permite"], false);
}

#[test]
fn sessao_sem_par_nao_controla() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = NucleoDoReceptor::novo();
    r.comecar_sessao(SESSAO_A, t);
    for _ in 0..3 {
        for m in r.devidas(t) {
            f.receber(SESSAO_A, None, &m, t);
        }
        for (_, m) in f.devidas(SESSAO_A, t) {
            r.receber(&m, t);
        }
    }
    r.pedir(r#"{"ev":0.5}"#, t).unwrap();
    for m in r.devidas(t) {
        f.receber(SESSAO_A, None, &m, t);
    }
    assert!(f.proximo_pedido(t).is_none());
    assert_eq!(v(&f.devidas(SESSAO_A, t)[0].1)["motivo"], "nao_pareado");
}

// ---------------------------------------------------------------------------------------------
// O filmador nunca confia no receptor
// ---------------------------------------------------------------------------------------------

fn pedido_cru(f: &NucleoDoFilmador, seq: u64, corpo: Value) -> String {
    let mut o = corpo.as_object().cloned().unwrap_or_default();
    o.insert("epoca".into(), json!(f.epoca()));
    o.insert("camera".into(), json!(f.camera()));
    o.insert("seq".into(), json!(seq));
    o.entry("vista").or_insert(json!(f.versao()));
    mensagem("pedido", o)
}

fn motivo_da_recusa(f: &mut NucleoDoFilmador, t: Instant) -> (String, Value) {
    let saida = f.devidas(SESSAO_A, t);
    let r = saida.iter().map(|(_, m)| v(m)).find(|m| m["tipo"] == "recusa").expect("sem recusa");
    (r["motivo"].as_str().unwrap().to_string(), r["campo"].clone())
}

#[test]
fn o_filmador_recusa_o_que_as_capacidades_nao_permitem() {
    let t = Instant::now();
    let casos: Vec<(Value, &str, Value)> = vec![
        (json!({"ajuste": {"travaIso": 100}}), "campo_desconhecido", json!("travaIso")),
        (json!({"ajuste": {"zoom": 2}}), "campo_desconhecido", json!("zoom")),
        (json!({"ajuste": {"exposicao": "manual", "iso": 12800}}), "fora_da_faixa", json!("iso")),
        (json!({"ajuste": {"exposicao": "manual", "iso": 401.5}}), "fora_da_faixa", json!("iso")),
        (json!({"ajuste": {"ev": null}}), "fora_da_faixa", json!("ev")),
        (json!({"ajuste": {"ev": "1"}}), "fora_da_faixa", json!("ev")),
        (json!({"ajuste": {"balanco": "neon"}}), "fora_da_faixa", json!("balanco")),
        (json!({"ajuste": {"travaBalanco": 1}}), "fora_da_faixa", json!("travaBalanco")),
        (json!({"ajuste": {"iso": 400}}), "incoerente", json!("iso")),
        (json!({"ajuste": {"kelvin": 5000}}), "incoerente", json!("kelvin")),
        (json!({"ajuste": {"focoPosicao": 0.5}}), "incoerente", json!("focoPosicao")),
        (json!({"ajuste": {"exposicao": "manual", "travaExposicao": true}}), "incoerente", json!("travaExposicao")),
        (json!({"toque": {"x": 1.5, "y": 0.5}}), "fora_da_faixa", json!("toque")),
        (json!({"ajuste": "iso"}), "invalido", Value::Null),
        (json!({}), "invalido", Value::Null),
    ];
    for (i, (corpo, esperado, campo)) in casos.into_iter().enumerate() {
        let mut f = filmador_com_camera(t);
        let _ = receptor_pronto(&mut f, SESSAO_A, t);
        let _ = f.devidas(SESSAO_A, t);
        f.receber(SESSAO_A, Some(&par("rx")), &pedido_cru(&f, 1, corpo.clone()), t);
        assert!(f.proximo_pedido(t).is_none(), "caso {i} passou: {corpo}");
        assert_eq!(motivo_da_recusa(&mut f, t), (esperado.to_string(), campo), "caso {i}: {corpo}");
    }
    // Campos demais.
    let mut f = filmador_com_camera(t);
    let _ = receptor_pronto(&mut f, SESSAO_A, t);
    let muitos: Objeto = (0..=TETO_DE_CAMPOS).map(|i| (format!("c{i}"), json!(true))).collect();
    f.receber(SESSAO_A, Some(&par("rx")), &pedido_cru(&f, 1, json!({"ajuste": muitos})), t);
    assert_eq!(motivo_da_recusa(&mut f, t).0, "fora_da_faixa");
}

#[test]
fn passar_para_manual_com_iso_no_mesmo_pedido_e_coerente() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let _ = receptor_pronto(&mut f, SESSAO_A, t);
    f.receber(SESSAO_A, Some(&par("rx")), &pedido_cru(&f, 1, json!({"ajuste": {"exposicao": "manual", "iso": 800}})), t);
    let p = v(&f.proximo_pedido(t).expect("aceito"));
    assert_eq!(p["ajuste"], json!({"exposicao": "manual", "iso": 800}));
}

#[test]
fn no_mac_so_as_travas_e_o_ponto() {
    let t = Instant::now();
    let mut f = NucleoDoFilmador::novo();
    f.permitir(true);
    // O Mac grava só cinco campos (R9 §2): a coerência usa o padrão "auto" do que falta.
    f.definir_camera(Some((&caps_mac(), r#"{"exposicao":"auto","travaExposicao":false,"balanco":"auto","travaBalanco":false,"foco":"auto"}"#))).unwrap();
    let mut r = receptor_pronto(&mut f, SESSAO_A, t);
    assert!(r.pedir(r#"{"iso":400}"#, t).is_err());
    assert!(r.pedir(r#"{"foco":"manual"}"#, t).is_err());
    r.pedir(r#"{"travaExposicao":true}"#, t).unwrap();
    r.tocar(0.25, 0.75, true, t).unwrap();
    for m in r.devidas(t) {
        f.receber(SESSAO_A, Some(&par("rx")), &m, t);
    }
    let p = v(&f.proximo_pedido(t).unwrap());
    assert_eq!(p["ajuste"], json!({"travaExposicao": true}));
    assert_eq!(p["toque"], json!({"x": 0.25, "y": 0.75, "longo": true}));
}

#[test]
fn os_tetos_da_casca_sao_conferidos() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    assert!(f.definir_lido(&format!(r#"{{"x":"{}"}}"#, "a".repeat(600))).is_err());
    assert!(f.definir_ajuste(&format!(r#"{{"x":"{}"}}"#, "a".repeat(1100)), 0, t).is_err());
    assert!(f.definir_ajuste("[1]", 0, t).is_err());
    assert!(f.definir_camera(Some(("{}", "{}"))).is_err(), "sem controles");
    assert!(f.definir_camera(Some((r#"{"controles":{"iso":{"min":5}}}"#, "{}"))).is_err());
    assert!(f.definir_camera(Some((r#"{"controles":{},"limites":{"iso":"Não Libera"}}"#, "{}"))).is_err());
    assert!(f.definir_ajuste("{}", 99, t).is_err(), "pedido que não existe");
    assert!(f.recusar(99, "nao_aplicado").is_err());
    assert!(f.recusar(1, "Com Espaço").is_err());
    let mut sem = NucleoDoFilmador::novo();
    assert!(sem.definir_ajuste("{}", 0, t).is_err(), "sem câmera");
}

// ---------------------------------------------------------------------------------------------
// Vence quem mexer por último
// ---------------------------------------------------------------------------------------------

#[test]
fn reenvio_atrasado_perde_para_quem_mexeu_depois() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let mut b = receptor_pronto(&mut f, SESSAO_B, t);
    // A pede EV +1, e o pedido se perde.
    a.pedir(r#"{"ev":1.0}"#, t).unwrap();
    let perdido = a.devidas(t);
    // B pede EV -1 depois, chega e é aplicado.
    b.pedir(r#"{"ev":-1.0}"#, t).unwrap();
    for m in b.devidas(t) {
        f.receber(SESSAO_B, Some(&par("B")), &m, t);
    }
    aplicar_como_a_casca(&mut f, t);
    // O reenvio de A chega agora: A não viu a mudança de B, e B mexeu depois.
    for m in perdido {
        f.receber(SESSAO_A, Some(&par("A")), &m, t);
    }
    assert!(f.proximo_pedido(t).is_none());
    assert_eq!(motivo_da_recusa(&mut f, t), ("superado".into(), json!("ev")));
    assert_eq!(f.ajuste()["ev"], json!(-1));
    let depois = t + BATIMENTO;
    trocar(&mut f, SESSAO_A, &mut a, depois);
    let e = v(&a.estado_json(depois));
    assert_eq!(e["ajuste"]["ev"], json!(-1), "A vê o valor de B: {e}");
    assert_eq!(e["recusa"]["motivo"], "superado");
}

#[test]
fn quem_ja_viu_a_mudanca_do_outro_vence() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let mut b = receptor_pronto(&mut f, SESSAO_B, t);
    b.pedir(r#"{"ev":-1.0}"#, t).unwrap();
    trocar(&mut f, SESSAO_B, &mut b, t);
    aplicar_como_a_casca(&mut f, t);
    trocar(&mut f, SESSAO_A, &mut a, t);
    a.pedir(r#"{"ev":1.5}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    aplicar_como_a_casca(&mut f, t);
    assert_eq!(f.ajuste()["ev"], json!(1.5));
}

#[test]
fn o_deslizante_do_mesmo_receptor_nao_se_atrapalha() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let mut agora = t;
    // Arrastando: cada passo sai antes de o estado do anterior voltar.
    for i in 1..=10 {
        agora += INTERVALO_DOS_PEDIDOS;
        a.pedir(&format!(r#"{{"ev":{}}}"#, f64::from(i) / 10.0), agora).unwrap();
        for m in a.devidas(agora) {
            f.receber(SESSAO_A, Some(&par("A")), &m, agora);
        }
        aplicar_como_a_casca(&mut f, agora);
    }
    assert_eq!(f.ajuste()["ev"], json!(1));
    assert_eq!(f.contadores().campos_superados, 0);
}

#[test]
fn mudanca_local_enquanto_o_pedido_espera_vence() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"ev":1.0,"antiCintilacao":"60"}"#, t).unwrap();
    for m in a.devidas(t) {
        f.receber(SESSAO_A, Some(&par("A")), &m, t);
    }
    // Antes de a casca tirar o pedido, a pessoa no filmador mexe no EV.
    let mut reg = f.ajuste().clone();
    reg.insert("ev".into(), json!(-0.5));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    let p = v(&f.proximo_pedido(t).unwrap());
    assert_eq!(p["ajuste"], json!({"antiCintilacao": "60"}), "o EV local venceu, a anti-cintilação passou");
}

#[test]
fn reenvio_ja_tratado_nao_reaplica() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"ev":1.0}"#, t).unwrap();
    let m = a.devidas(t);
    for x in &m {
        f.receber(SESSAO_A, Some(&par("A")), x, t);
    }
    assert_eq!(aplicar_como_a_casca(&mut f, t).len(), 1);
    // O estado com o recibo se perdeu; o reenvio chega duplicado.
    for x in m.iter().chain(m.iter()) {
        f.receber(SESSAO_A, Some(&par("A")), x, t);
    }
    assert!(f.proximo_pedido(t).is_none());
    assert_eq!(f.contadores().reenvios, 2);
}

// ---------------------------------------------------------------------------------------------
// Câmera, época, desistência e sessões
// ---------------------------------------------------------------------------------------------

#[test]
fn troca_de_camera_recusa_o_que_estava_em_transito_e_refaz_o_receptor() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"ev":1.0}"#, t).unwrap();
    let em_transito = a.devidas(t);
    f.definir_camera(Some((&caps_mac(), &ajuste_padrao()))).unwrap();
    for m in em_transito {
        f.receber(SESSAO_A, Some(&par("A")), &m, t);
    }
    assert_eq!(motivo_da_recusa(&mut f, t).0, "camera_trocada");
    trocar(&mut f, SESSAO_A, &mut a, t + Duration::from_secs(2));
    let e = v(&a.estado_json(t + Duration::from_secs(2)));
    assert_eq!(e["situacao"], "pronto");
    assert_eq!(e["capacidades"]["plataforma"], "macos", "{e}");
    // Câmera fechada.
    f.definir_camera(None).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t + Duration::from_secs(4));
    assert_eq!(a.situacao(), situacao::SEM_CAMERA);
}

#[test]
fn pedido_em_aplicacao_quando_a_camera_troca_e_recusado() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"ev":1.0}"#, t).unwrap();
    for m in a.devidas(t) {
        f.receber(SESSAO_A, Some(&par("A")), &m, t);
    }
    let n = v(&f.proximo_pedido(t).unwrap())["n"].as_u64().unwrap();
    f.definir_camera(Some((&caps_android(), &ajuste_padrao()))).unwrap();
    assert!(f.definir_ajuste(&ajuste_padrao(), n, t).is_err(), "o pedido velho não vale na câmera nova");
    assert_eq!(motivo_da_recusa(&mut f, t).0, "camera_trocada");
}

#[test]
fn epoca_nova_esquece_o_filmador_de_antes() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = receptor_pronto(&mut f, SESSAO_A, t);
    r.pedir(r#"{"ev":1.0}"#, t).unwrap();
    let mut f2 = filmador_com_camera(t);
    trocar(&mut f2, SESSAO_A, &mut r, t + Duration::from_secs(2));
    let e = v(&r.estado_json(t + Duration::from_secs(2)));
    assert_eq!(e["situacao"], "pronto");
    assert_eq!(e["pendente"], json!({}), "o pendente da época velha caiu");
}

#[test]
fn sem_recibo_o_receptor_reenvia_e_depois_desiste() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = receptor_pronto(&mut f, SESSAO_A, t);
    r.pedir(r#"{"ev":1.0}"#, t).unwrap();
    assert_eq!(r.devidas(t).len(), 1);
    assert!(r.devidas(t + Duration::from_millis(100)).is_empty());
    assert_eq!(r.devidas(t + REENVIO_DO_PEDIDO).len(), 1);
    let depois = t + PRAZO_DO_PEDIDO;
    // O filmador continua mandando estado (sem recibo), e o receptor desiste.
    for (_, m) in f.devidas(SESSAO_A, depois) {
        r.receber(&m, depois);
    }
    let _ = r.devidas(depois);
    let e = v(&r.estado_json(depois));
    assert_eq!(e["recusa"]["motivo"], "sem_resposta");
    assert_eq!(e["pendente"], json!({}));
    assert_eq!(r.contadores().desistencias, 1);
}

#[test]
fn filmador_que_nao_responde_vira_sem_resposta() {
    let t = Instant::now();
    let mut r = NucleoDoReceptor::novo();
    r.comecar_sessao(SESSAO_A, t);
    assert_eq!(r.devidas(t).len(), 1);
    assert!(r.devidas(t + Duration::from_millis(500)).is_empty());
    assert_eq!(r.devidas(t + REENVIO_DO_OLA).len(), 1, "o ola se repete");
    let _ = r.devidas(t + SEM_FILMADOR);
    assert_eq!(r.situacao(), situacao::SEM_RESPOSTA);
    assert!(r.pedir(r#"{"ev":1}"#, t + SEM_FILMADOR).is_err());
}

#[test]
fn no_maximo_dezesseis_sessoes() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let ola = mensagem("ola", Objeto::new());
    for s in 1..=17 {
        f.receber(s, Some(&par("x")), &ola, t);
    }
    assert_eq!(f.contadores().sessoes_demais, 1);
    assert!(f.devidas(17, t).is_empty());
    f.esquecer_sessao(1);
    f.receber(17, Some(&par("x")), &ola, t);
    assert!(!f.devidas(17, t).is_empty());
}

#[test]
fn o_batimento_e_o_lido_tem_ritmo() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let _ = receptor_pronto(&mut f, SESSAO_A, t);
    assert!(f.devidas(SESSAO_A, t + Duration::from_millis(100)).is_empty());
    f.definir_lido(r#"{"iso":400}"#).unwrap();
    assert!(f.devidas(SESSAO_A, t + Duration::from_millis(100)).is_empty(), "o lido espera 250 ms");
    assert_eq!(f.devidas(SESSAO_A, t + INTERVALO_DO_LIDO).len(), 1);
    assert_eq!(f.devidas(SESSAO_A, t + INTERVALO_DO_LIDO + BATIMENTO).len(), 1, "o batimento");
}

// ---------------------------------------------------------------------------------------------
// O canal simulado: perda, duplicata e desordem
// ---------------------------------------------------------------------------------------------

struct Sorte(u64);

impl Sorte {
    fn proximo(&mut self) -> u64 {
        // xorshift64*: determinístico por semente, sem crate.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.proximo() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    fn ate(&mut self, n: usize) -> usize {
        (self.proximo() % n as u64) as usize
    }
}

/// Uma direção do canal: perde 30 %, duplica 10 %, e entrega fora de ordem.
#[derive(Default)]
struct Cano(Vec<String>);

impl Cano {
    fn por(&mut self, m: String, s: &mut Sorte) {
        if s.chance(0.3) {
            return;
        }
        if s.chance(0.1) {
            self.0.push(m.clone());
        }
        self.0.push(m);
    }
    fn tirar(&mut self, s: &mut Sorte) -> Vec<String> {
        let mut v = std::mem::take(&mut self.0);
        for i in (1..v.len()).rev() {
            v.swap(i, s.ate(i + 1));
        }
        v
    }
}

#[test]
fn converge_com_perda_duplicata_e_desordem() {
    let campos = ["ev", "antiCintilacao", "balanco", "foco"];
    let valores = |c: &str, k: usize| -> Value {
        match c {
            "ev" => json!((k % 41) as f64 / 10.0 - 2.0),
            "antiCintilacao" => json!(["auto", "50", "60", "desligada"][k % 4]),
            "balanco" => json!(["auto", "incandescente", "nublado"][k % 3]),
            _ => json!(["auto", "travado"][k % 2]),
        }
    };
    for semente in 1..=20u64 {
        let mut sorte = Sorte(semente.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let t0 = Instant::now();
        let mut f = filmador_com_camera(t0);
        let sessoes = [SESSAO_A, SESSAO_B];
        let mut rs: Vec<NucleoDoReceptor> = sessoes
            .iter()
            .map(|s| {
                let mut r = NucleoDoReceptor::novo();
                r.comecar_sessao(*s, t0);
                r
            })
            .collect();
        let mut ida: Vec<Cano> = vec![Cano::default(), Cano::default()];
        let mut volta: Vec<Cano> = vec![Cano::default(), Cano::default()];
        let mut agora = t0;
        for passo in 0..600u64 {
            agora += Duration::from_millis(20);
            // Nos primeiros 400 passos, todos mexem de vez em quando.
            if passo < 400 {
                for (i, r) in rs.iter_mut().enumerate() {
                    if sorte.chance(0.04) && r.situacao() == situacao::PRONTO {
                        let c = campos[sorte.ate(campos.len())];
                        let mut o = Objeto::new();
                        o.insert(c.into(), valores(c, sorte.ate(100) + i));
                        let _ = r.pedir(&Value::Object(o).to_string(), agora);
                    }
                }
                if sorte.chance(0.03) {
                    let c = campos[sorte.ate(campos.len())];
                    let mut reg = f.ajuste().clone();
                    reg.insert(c.into(), valores(c, sorte.ate(100)));
                    f.definir_ajuste(&Value::Object(reg).to_string(), 0, agora).unwrap();
                }
            }
            // Na reta final o canal fica limpo, para provar a convergência e não a sorte.
            let limpo = passo >= 500;
            for (i, s) in sessoes.iter().enumerate() {
                for m in rs[i].devidas(agora) {
                    if limpo { ida[i].0.push(m) } else { ida[i].por(m, &mut sorte) }
                }
                for m in ida[i].tirar(&mut sorte) {
                    f.receber(*s, Some(&par(&format!("rx{s}"))), &m, agora);
                }
                aplicar_como_a_casca(&mut f, agora);
                for (_, m) in f.devidas(*s, agora) {
                    if limpo { volta[i].0.push(m) } else { volta[i].por(m, &mut sorte) }
                }
                for m in volta[i].tirar(&mut sorte) {
                    rs[i].receber(&m, agora);
                }
            }
        }
        for (i, r) in rs.iter_mut().enumerate() {
            let e = v(&r.estado_json(agora));
            assert_eq!(e["situacao"], "pronto", "semente {semente}, receptor {i}: {e}");
            assert_eq!(e["pendente"], json!({}), "semente {semente}, receptor {i}: nada preso");
            assert_eq!(r.aplicado(), Some(f.ajuste()), "semente {semente}, receptor {i} convergiu");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// De ponta a ponta, por uma sessão de vídeo de verdade (127.0.0.1)
// ---------------------------------------------------------------------------------------------

/// **Uma sessão de vídeo de verdade** (sem papel, canal sem retransmissão): o filmador hospeda, o
/// receptor conecta, cada lado bombeia na sua thread, a casca do filmador aplica numa terceira, e o
/// pedido do receptor volta aplicado. Prova a integração com o transporte e a sinalização, não a
/// travessia por rádio.
#[test]
fn pela_sessao_de_video_de_verdade() {
    use crate::cancel::Cancelamento;
    use crate::pairing::{PairedPeers, Pin};
    use crate::protocol::{Announcement, Capabilities, DeviceId, PROTOCOL_VERSION};
    use crate::session::{conectar, hospedar, SessionConfig};
    use crate::signaling::SignalingServer;
    use crate::transport::{Delivery, TransportConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let anuncio = |id: &str| Announcement {
        protocol_version: PROTOCOL_VERSION,
        device_id: DeviceId(id.into()),
        display_name: format!("{id} nome"),
        capabilities: Capabilities { screen_source: false, camera_source: true, sink: true },
        screen: None,
        papel: None,
    };
    let config = |a: Announcement, pin: &Pin| SessionConfig {
        announcement: a,
        pin: Some(pin.clone()),
        known: PairedPeers::new(),
        transport: TransportConfig::default(),
        tracks: Vec::new(),
        timeout: Duration::from_secs(30),
        cancelamento: Cancelamento::novo(),
        silencio_do_caminho: None,
    };
    assert_eq!(TransportConfig::default().delivery, Delivery::Realtime, "a sessão de vídeo não muda");
    let pin = Pin::parse("525252").expect("pin");
    let servidor = SignalingServer::bind(0).expect("bind");
    let porta = servidor.port().expect("porta");
    let p = pin.clone();
    let lado_f = std::thread::spawn(move || hospedar(&servidor, config(anuncio("filmador-e2e"), &p)));
    let destino = format!("127.0.0.1:{porta}").parse().expect("endereço");
    let sessao_r = conectar(destino, config(anuncio("receptor-e2e"), &pin)).expect("receptor");
    let sessao_f = lado_f.join().expect("thread").expect("filmador");

    let filmador = Arc::new(Filmador::novo());
    filmador.permitir(true).unwrap();
    filmador.definir_camera(Some((&caps_android(), &ajuste_padrao()))).unwrap();
    let controle = Arc::new(Controlador::novo());
    let parar = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    {
        let (f, m, parar) = (Arc::clone(&filmador), sessao_f.session.mensageiro(), Arc::clone(&parar));
        threads.push(std::thread::spawn(move || {
            while !parar.load(Ordering::Relaxed) {
                let b = f.bombear(&m, Duration::from_millis(50)).unwrap();
                if b.fechada {
                    break;
                }
            }
        }));
    }
    {
        let (c, m, parar) = (Arc::clone(&controle), sessao_r.session.mensageiro(), Arc::clone(&parar));
        threads.push(std::thread::spawn(move || {
            while !parar.load(Ordering::Relaxed) {
                if c.bombear(&m, Duration::from_millis(50)).unwrap().fechada {
                    break;
                }
            }
        }));
    }
    {
        // A casca do filmador: tira os pedidos e aplica.
        let (f, parar) = (Arc::clone(&filmador), Arc::clone(&parar));
        threads.push(std::thread::spawn(move || {
            let mut reg: Objeto = serde_json::from_str(&ajuste_padrao()).unwrap();
            while !parar.load(Ordering::Relaxed) {
                while let Some(p) = f.proximo_pedido().unwrap() {
                    let p = v(&p);
                    for (k, x) in p["ajuste"].as_object().unwrap() {
                        reg.insert(k.clone(), x.clone());
                    }
                    f.definir_ajuste(&Value::Object(reg.clone()).to_string(), p["n"].as_u64().unwrap()).unwrap();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }));
    }

    let esperar = |cond: &dyn Fn(&Value) -> bool| {
        let fim = Instant::now() + Duration::from_secs(10);
        loop {
            let e = v(&controle.estado_json().unwrap());
            if cond(&e) || Instant::now() > fim {
                return e;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let e = esperar(&|e| e["situacao"] == "pronto");
    assert_eq!(e["situacao"], "pronto", "{e}");
    controle.pedir(r#"{"balanco":"kelvin","kelvin":5600}"#).unwrap();
    let e = esperar(&|e| e["aplicado"]["kelvin"] == 5600 && e["pendente"] == json!({}));
    assert_eq!(e["aplicado"]["kelvin"], 5600, "{e}");
    assert_eq!(e["autor"], "receptor-e2e nome");
    let ef = v(&filmador.estado_json().unwrap());
    assert_eq!(ef["controlado_por"]["nome"], "receptor-e2e nome", "{ef}");
    assert_eq!(ef["receptores"].as_array().map(Vec::len), Some(1));
    // Desligada, a opção chega ao receptor.
    filmador.permitir(false).unwrap();
    let e = esperar(&|e| e["situacao"] == "nao_permitido");
    assert_eq!(e["situacao"], "nao_permitido", "{e}");

    parar.store(true, Ordering::Relaxed);
    for t in threads {
        let _ = t.join();
    }
    drop(sessao_r);
    drop(sessao_f);
}

// ---------------------------------------------------------------------------------------------
// A revisão adversarial de 02/10: um teste por achado que mudou o código
// ---------------------------------------------------------------------------------------------

/// B2: juntar um campo velho num pedido novo não lhe dá a vista de agora.
#[test]
fn campo_velho_juntado_num_pedido_novo_nao_ressuscita() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let mut b = receptor_pronto(&mut f, SESSAO_B, t);
    // A mexe na anti-cintilação, e o pedido se perde.
    a.pedir(r#"{"antiCintilacao":"50"}"#, t).unwrap();
    let _perdido = a.devidas(t);
    // B muda a anti-cintilação depois; A recebe o estado novo e mexe no EV.
    b.pedir(r#"{"antiCintilacao":"60"}"#, t).unwrap();
    trocar(&mut f, SESSAO_B, &mut b, t);
    aplicar_como_a_casca(&mut f, t);
    let t1 = t + INTERVALO_DOS_PEDIDOS;
    for (_, m) in f.devidas(SESSAO_A, t1) {
        a.receber(&m, t1);
    }
    a.pedir(r#"{"ev":0.5}"#, t1).unwrap();
    for m in a.devidas(t1) {
        f.receber(SESSAO_A, Some(&par("A")), &m, t1);
    }
    let p = v(&f.proximo_pedido(t1).unwrap());
    assert_eq!(p["ajuste"], json!({"ev": 0.5}), "a anti-cintilação velha de A caiu: {p}");
    aplicar_como_a_casca(&mut f, t1);
    assert_eq!(f.ajuste()["antiCintilacao"], "60");
}

/// B3: `restaurar` com campos no mesmo pedido: a coerência é a do registro restaurado.
#[test]
fn restaurar_e_depois_os_campos() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let _ = receptor_pronto(&mut f, SESSAO_A, t);
    let mut reg = f.ajuste().clone();
    reg.insert("exposicao".into(), json!("manual"));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    // `ev` com a exposição manual é incoerente; depois de restaurar, não.
    f.receber(SESSAO_A, Some(&par("A")), &pedido_cru(&f, 1, json!({"restaurar": true, "ajuste": {"ev": 1.0}})), t);
    let p = v(&f.proximo_pedido(t).expect("aceito"));
    assert_eq!(p["restaurar"], true);
    assert_eq!(p["ajuste"], json!({"ev": 1}));
    f.receber(SESSAO_A, Some(&par("A")), &pedido_cru(&f, 2, json!({"ajuste": {"ev": 1.0}})), t);
    assert!(f.proximo_pedido(t).is_none(), "sem restaurar, incoerente");
}

/// I1: a escrita automática da casca não toma o campo de ninguém.
#[test]
fn a_escrita_do_sistema_nao_vira_mudanca_de_gente() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"foco":"manual","focoPosicao":0.4}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    aplicar_como_a_casca(&mut f, t);
    let antes = f.versao();
    let mut reg = f.ajuste().clone();
    reg.insert("focoPosicao".into(), json!(0.41));
    reg.insert("travaIso".into(), json!(400));
    f.definir_ajuste(&Value::Object(reg).to_string(), AJUSTE_DO_SISTEMA, t).unwrap();
    assert_eq!(f.versao(), antes + 1, "o estado sai com o registro novo");
    assert_eq!(v(&f.estado_json(t))["controlado_por"]["nome"], "rx101", "o autor fica");
    // Um pedido de B, que viu a versão de antes da escrita do sistema, ainda vale.
    let mut b = NucleoDoReceptor::novo();
    b.comecar_sessao(SESSAO_B, t);
    let ola = b.devidas(t);
    f.receber(SESSAO_B, Some(&par("B")), &ola[0], t);
    let mut estado_velho = None;
    for (_, m) in f.devidas(SESSAO_B, t) {
        b.receber(&m, t);
        estado_velho = Some(m);
    }
    assert!(estado_velho.is_some());
    b.pedir(r#"{"focoPosicao":0.2}"#, t).unwrap();
    let mut reg = f.ajuste().clone();
    reg.insert("focoPosicao".into(), json!(0.42));
    f.definir_ajuste(&Value::Object(reg).to_string(), AJUSTE_DO_SISTEMA, t).unwrap();
    for m in b.devidas(t) {
        f.receber(SESSAO_B, Some(&par("B")), &m, t);
    }
    assert!(f.proximo_pedido(t).is_some(), "a escrita do sistema não superou o pedido de B");
}

/// I2: `null` e ausente, `800` e `800.0`, não são mudança.
#[test]
fn o_diff_do_registro_e_normalizado() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let antes = f.versao();
    let mut reg = f.ajuste().clone();
    reg.insert("iso".into(), Value::Null);
    reg.insert("ev".into(), json!(0));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    assert_eq!(f.versao(), antes, "nada mudou");
}

/// I8: o campo igual ao aplicado não disputa nada e ganha recibo.
#[test]
fn pedido_igual_ao_aplicado_so_ganha_recibo() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"ev":0.0,"balanco":"auto"}"#, t).unwrap();
    for m in a.devidas(t) {
        f.receber(SESSAO_A, Some(&par("A")), &m, t);
    }
    assert!(f.proximo_pedido(t).is_none());
    let antes = f.versao();
    for (_, m) in f.devidas(SESSAO_A, t) {
        a.receber(&m, t);
    }
    assert_eq!(f.versao(), antes);
    assert_eq!(v(&a.estado_json(t))["pendente"], json!({}), "o recibo chegou");
}

/// I6: o toque que esperava na fila não some quando o pedido seguinte o substitui.
#[test]
fn a_substituicao_na_fila_nao_perde_o_toque() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let _ = receptor_pronto(&mut f, SESSAO_A, t);
    f.receber(SESSAO_A, Some(&par("A")), &pedido_cru(&f, 1, json!({"toque": {"x": 0.1, "y": 0.2}})), t);
    f.receber(SESSAO_A, Some(&par("A")), &pedido_cru(&f, 2, json!({"ajuste": {"ev": 0.3}})), t);
    let p = v(&f.proximo_pedido(t).unwrap());
    assert_eq!(p["toque"], json!({"x": 0.1, "y": 0.2, "longo": false}));
    assert_eq!(p["ajuste"], json!({"ev": 0.3}));
    assert!(f.proximo_pedido(t).is_none());
}

/// M7: faixas novas na mesma câmera não derrubam o que está em trânsito.
#[test]
fn faixa_nova_nao_e_camera_nova() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"exposicao":"manual","obturadorNs":20000000}"#, t).unwrap();
    let em_transito = a.devidas(t);
    // O fps foi a 60: o teto do obturador cai para 1/60.
    let caps = caps_android().replace("33333333", "16666666");
    f.definir_capacidades(&caps).unwrap();
    for m in &em_transito {
        f.receber(SESSAO_A, Some(&par("A")), m, t);
    }
    assert_eq!(motivo_da_recusa(&mut f, t), ("fora_da_faixa".into(), json!("obturadorNs")));
    let t1 = t + BATIMENTO;
    trocar(&mut f, SESSAO_A, &mut a, t1);
    let e = v(&a.estado_json(t1));
    assert_eq!(e["capacidades"]["controles"]["obturadorNs"]["max"], 16666666, "o receptor pediu as faixas novas");
    let t2 = t1 + INTERVALO_DOS_PEDIDOS;
    a.pedir(r#"{"exposicao":"manual","obturadorNs":16666666}"#, t2).unwrap();
    for m in a.devidas(t2) {
        f.receber(SESSAO_A, Some(&par("A")), &m, t2);
    }
    assert!(f.proximo_pedido(t2).is_some(), "a mesma câmera, sem camera_trocada");
}

/// M10: o pedido que a casca tirou e não respondeu vence.
#[test]
fn a_casca_que_nao_responde_vence_em_cinco_segundos() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"ev":1.0}"#, t).unwrap();
    for m in a.devidas(t) {
        f.receber(SESSAO_A, Some(&par("A")), &m, t);
    }
    let n = v(&f.proximo_pedido(t).unwrap())["n"].as_u64().unwrap();
    let depois = t + PRAZO_DA_CASCA;
    assert_eq!(motivo_da_recusa(&mut f, depois).0, "nao_aplicado");
    assert!(f.definir_ajuste(&ajuste_padrao(), n, depois).is_err());
}

/// M5: o EV fica apagado com a trava ligada.
#[test]
fn ev_com_a_trava_ligada_e_incoerente() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"travaExposicao":true}"#, t).unwrap();
    assert!(a.pedir(r#"{"ev":1.0}"#, t).is_err());
}

/// As capacidades de uma webcam no Windows: o `ev` é o brilho do driver, independente da exposição
/// (13/10, R9 §3.3).
fn caps_windows(independente: bool) -> String {
    let mut ev = json!({"min": -64, "max": 64, "passo": 1, "inteiro": true, "unidade": "brilho", "origem": 0});
    if independente {
        ev[DICA_DO_EV_INDEPENDENTE] = json!(true);
    }
    json!({
        "plataforma": "windows",
        "nomeDaCamera": "Integrated Webcam",
        "controles": {
            "exposicao": {"valores": ["auto", "manual"]},
            "ev": ev,
            "travaExposicao": {},
            "iso": {"min": 0, "max": 100, "inteiro": true, "unidade": "ganho"},
            "obturadorNs": {"min": 976563, "max": 62500000, "inteiro": true, "escala": "log2"}
        },
        "limites": {}
    })
    .to_string()
}

/// 13/10: o brilho do Windows vale com a exposição manual e com a trava, nas duas pontas; sem a
/// dica (o filmador de antes), a regra de sempre.
#[test]
fn o_brilho_independente_vale_em_manual_e_com_a_trava() {
    let t = Instant::now();
    for independente in [true, false] {
        let mut f = NucleoDoFilmador::novo();
        f.permitir(true);
        let manual = json!({"exposicao": "manual", "ev": 0, "travaExposicao": false, "iso": 10, "obturadorNs": 31250000});
        f.definir_camera(Some((&caps_windows(independente), &manual.to_string()))).unwrap();
        let mut a = receptor_pronto(&mut f, SESSAO_A, t);
        // O receptor confere com as capacidades do filmador.
        assert_eq!(a.pedir(r#"{"ev":12}"#, t).is_ok(), independente, "receptor, manual, independente={independente}");
        // O filmador também, com o pedido cru (o receptor de antes não sabe da dica).
        f.receber(SESSAO_A, Some(&par("A")), &pedido_cru(&f, 50, json!({"ajuste": {"ev": 20}})), t);
        let aceito = f.proximo_pedido(t);
        assert_eq!(aceito.is_some(), independente, "filmador, manual, independente={independente}");
        if let Some(p) = aceito {
            let p = v(&p);
            assert_eq!(p["ajuste"]["ev"], json!(20));
            let mut reg = f.ajuste().clone();
            reg.insert("ev".into(), json!(20));
            f.definir_ajuste(&Value::Object(reg).to_string(), p["n"].as_u64().unwrap(), t).unwrap();
        }
        // Em Auto com a trava ligada: o brilho continua livre só com a dica.
        let travado = json!({"exposicao": "auto", "ev": 0, "travaExposicao": true});
        f.definir_ajuste(&travado.to_string(), 0, t).unwrap();
        f.receber(SESSAO_A, Some(&par("A")), &pedido_cru(&f, 60, json!({"ajuste": {"ev": -8}})), t);
        assert_eq!(f.proximo_pedido(t).is_some(), independente, "filmador, trava, independente={independente}");
    }
}

/// A dica só vale no `ev`, e só com `true`: o Android (sem a dica) segue com o EV só no Auto.
#[test]
fn a_dica_do_ev_e_estrita() {
    let mut caps: Objeto = serde_json::from_str(&caps_windows(false)).unwrap();
    assert!(!ev_independente(&caps));
    caps["controles"]["ev"][DICA_DO_EV_INDEPENDENTE] = json!("sim");
    assert!(!ev_independente(&caps), "só o booleano");
    caps["controles"]["ev"][DICA_DO_EV_INDEPENDENTE] = json!(true);
    assert!(ev_independente(&caps));
    let manual: Objeto = serde_json::from_str(r#"{"exposicao":"manual"}"#).unwrap();
    let iso_em_auto: Objeto = serde_json::from_str(r#"{"exposicao":"auto","iso":10}"#).unwrap();
    // A dica não solta os outros campos.
    assert!(conferir_coerencia(&caps, &manual, &serde_json::from_str(r#"{"ev":3}"#).unwrap()).is_ok());
    assert!(conferir_coerencia(&caps, &Objeto::new(), &iso_em_auto).is_err());
}

/// M12: o nome do outro lado vai para a tela de todos sem caracteres de controle.
#[test]
fn o_nome_do_par_e_limpo_e_curto() {
    assert_eq!(nome_curto("OBS\u{1b}[31m no\nDell"), "OBS[31m noDell");
    let longo = "ç".repeat(40);
    let curto = nome_curto(&longo);
    assert!(curto.len() <= TETO_DO_NOME && curto.chars().all(|c| c == 'ç'));
}

/// B3 no receptor: com `restaurar` pendente, a coerência do pedido seguinte parte do padrão.
#[test]
fn no_receptor_restaurar_pendente_muda_a_coerencia() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut reg = f.ajuste().clone();
    reg.insert("exposicao".into(), json!("manual"));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    assert!(a.pedir(r#"{"ev":1.0}"#, t).is_err(), "manual: EV apagado");
    a.restaurar(t).unwrap();
    a.pedir(r#"{"ev":1.0}"#, t).unwrap();
}

// ---------------------------------------------------------------------------------------------
// "Travar fps" (R9 §3.1.1, 12/10)
// ---------------------------------------------------------------------------------------------

/// As capacidades do Android com o `travarFps` (sim/não).
fn caps_android_com_trava_de_fps() -> String {
    let mut c = v(&caps_android());
    c["controles"]["travarFps"] = json!({});
    c.to_string()
}

fn filmador_com_trava_de_fps() -> NucleoDoFilmador {
    let mut f = NucleoDoFilmador::novo();
    f.permitir(true);
    f.definir_camera(Some((&caps_android_com_trava_de_fps(), &ajuste_padrao()))).unwrap();
    f
}

/// O pedido do botão do aviso ("Travar em 30 fps") chega, é aplicado e volta no estado.
#[test]
fn travar_fps_vai_e_volta_pelo_controle_remoto() {
    let t = Instant::now();
    let mut f = filmador_com_trava_de_fps();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"travarFps":true}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    let aplicados = aplicar_como_a_casca(&mut f, t);
    assert_eq!(aplicados.len(), 1);
    assert_eq!(aplicados[0]["ajuste"], json!({"travarFps": true}));
    let t2 = t + BATIMENTO;
    trocar(&mut f, SESSAO_A, &mut a, t2);
    assert_eq!(a.aplicado().unwrap()["travarFps"], json!(true));
    // E o "Clarear" desliga.
    // (o `trocar` acima reenviou o primeiro pedido em t2: o próximo envio espera o intervalo)
    let t2 = t2 + INTERVALO_DOS_PEDIDOS;
    a.pedir(r#"{"travarFps":false}"#, t2).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t2);
    assert_eq!(aplicar_como_a_casca(&mut f, t2).len(), 1);
    let t3 = t2 + BATIMENTO;
    trocar(&mut f, SESSAO_A, &mut a, t3);
    assert_eq!(a.aplicado().unwrap()["travarFps"], json!(false));
}

/// As cascas Apple põem dicas no descritor (`{"fps":30,"piso":15}`): continua sendo sim/não.
#[test]
fn travar_fps_com_dicas_no_descritor() {
    let t = Instant::now();
    let mut c = v(&caps_android());
    c["controles"]["travarFps"] = json!({"fps": 30, "piso": 15});
    let mut f = NucleoDoFilmador::novo();
    f.permitir(true);
    f.definir_camera(Some((&c.to_string(), &ajuste_padrao()))).unwrap();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"travarFps":true}"#, t).unwrap();
    assert!(a.pedir(r#"{"travarFps":30}"#, t).is_err());
    trocar(&mut f, SESSAO_A, &mut a, t);
    assert_eq!(aplicar_como_a_casca(&mut f, t).len(), 1);
    assert_eq!(f.ajuste()["travarFps"], json!(true));
}

/// Booleano só: o número e o texto são `fora_da_faixa`.
#[test]
fn travar_fps_e_sim_ou_nao() {
    let t = Instant::now();
    let mut f = filmador_com_trava_de_fps();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    assert!(a.pedir(r#"{"travarFps":1}"#, t).is_err());
    assert!(a.pedir(r#"{"travarFps":"sim"}"#, t).is_err());
}

/// Vale com a exposição manual também: não é `incoerente` (fica guardado e vale ao voltar ao Auto).
#[test]
fn travar_fps_vale_em_qualquer_exposicao() {
    let t = Instant::now();
    let mut f = filmador_com_trava_de_fps();
    let mut reg = f.ajuste().clone();
    reg.insert("exposicao".into(), json!("manual"));
    reg.insert("iso".into(), json!(400));
    reg.insert("obturadorNs".into(), json!(16_666_666));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"travarFps":true}"#, t).unwrap();
    a.pedir(r#"{"exposicao":"auto","travarFps":true}"#, t).unwrap();
    a.pedir(r#"{"exposicao":"manual","travarFps":false}"#, t).unwrap();
}

/// Um filmador sem a chave nas capacidades (de antes de 12/10, ou sem a trava) faz o receptor
/// recusar na hora, com `campo_desconhecido`.
#[test]
fn travar_fps_sem_a_capacidade_e_campo_desconhecido() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    assert!(a.pedir(r#"{"travarFps":true}"#, t).is_err());
}

/// O registro antigo, sem a chave, vale `false`: a casca que passa a gravar o `false` explícito
/// não sobe a versão, e o pedido de `false` contra o ausente não disputa nada.
#[test]
fn travar_fps_ausente_vale_falso() {
    let t = Instant::now();
    let mut f = filmador_com_trava_de_fps();
    let antes = f.versao();
    let mut reg = f.ajuste().clone();
    assert!(!reg.contains_key("travarFps"));
    reg.insert("travarFps".into(), json!(false));
    f.definir_ajuste(&Value::Object(reg.clone()).to_string(), 0, t).unwrap();
    assert_eq!(f.versao(), antes, "false explícito = ausente");
    reg.insert("travarFps".into(), Value::Null);
    f.definir_ajuste(&Value::Object(reg.clone()).to_string(), 0, t).unwrap();
    assert_eq!(f.versao(), antes, "null = ausente = false");
    reg.insert("travarFps".into(), json!(true));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    assert_eq!(f.versao(), antes + 1, "true muda");

    // O pedido de false contra o ausente: recibo, sem pedido para a casca nem versão nova.
    let mut f = filmador_com_trava_de_fps();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let antes = f.versao();
    a.pedir(r#"{"travarFps":false}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    assert!(f.proximo_pedido(t).is_none());
    assert_eq!(f.versao(), antes);
}

/// O lido com a pouca luz (`"poucaLuz":{"fpsChegando":…,"piso":…}`) passa inteiro ao receptor, e o
/// estado de um registro cheio com ele ainda cabe num pedaço SCTP.
#[test]
fn o_lido_leva_a_pouca_luz() {
    let t = Instant::now();
    let mut f = filmador_com_trava_de_fps();
    let cheio = json!({"exposicao": "auto", "ev": -0.7, "travaExposicao": false, "travaIso": 400,
        "travaObturadorNs": 16666666, "iso": 1600, "obturadorNs": 8333333, "antiCintilacao": "60",
        "balanco": "kelvin", "kelvin": 5600, "travaBalanco": false,
        "travaGanhos": [1.9921875, 1.0, 1.0, 2.34375], "foco": "manual", "focoPosicao": 0.37,
        "travarFps": true});
    f.definir_ajuste(&cheio.to_string(), 0, t).unwrap();
    let lido = r#"{"iso":3200,"obturadorNs":33333333,"kelvin":5550,"abertura":1.7,"focoPosicao":0.37,"divergentes":["kelvin"],"poucaLuz":{"fpsChegando":30,"piso":15}}"#;
    f.definir_lido(lido).unwrap();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let e = v(&a.estado_json(t));
    assert_eq!(e["lido"]["poucaLuz"], json!({"fpsChegando": 30, "piso": 15}));
    f.receber(SESSAO_A, Some(&par("Um nome de aparelho bem comprido, com acento ção")), &mensagem("ola", Objeto::new()), t);
    for (d, m) in f.devidas(SESSAO_A, t) {
        assert!(m.len() <= ALVO_DA_MENSAGEM, "{d:?} tem {} bytes: {m}", m.len());
    }
    // A pouca luz que apaga volta como `null`.
    f.definir_lido(r#"{"iso":800,"poucaLuz":null}"#).unwrap();
    let t2 = t + BATIMENTO;
    trocar(&mut f, SESSAO_A, &mut a, t2);
    assert_eq!(v(&a.estado_json(t2))["lido"]["poucaLuz"], Value::Null);
}

// ---------------------------------------------------------------------------------------------
// Os ajustes de imagem (R9 §3.7, 13/10)
// ---------------------------------------------------------------------------------------------

/// As capacidades do Android com os quatro ajustes de imagem (o Quall processa) e a contraluz.
fn caps_android_com_imagem() -> String {
    let mut c = v(&caps_android_com_trava_de_fps());
    for campo in AJUSTES_DE_IMAGEM {
        c["controles"][campo] = json!({"min": -100, "max": 100, "passo": 1, "origem": "quall"});
    }
    c["controles"][CONTRALUZ] = json!({"origem": "camera"});
    c.to_string()
}

fn filmador_com_imagem() -> NucleoDoFilmador {
    let mut f = NucleoDoFilmador::novo();
    f.permitir(true);
    f.definir_camera(Some((&caps_android_com_imagem(), &ajuste_padrao()))).unwrap();
    f
}

/// Os quatro e a contraluz vão e voltam, vencendo o último como os outros campos.
#[test]
fn os_ajustes_de_imagem_vao_e_voltam() {
    let t = Instant::now();
    let mut f = filmador_com_imagem();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"gama":-20,"contraste":35,"saturacao":-100,"nitidez":100,"contraluz":true}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    let aplicados = aplicar_como_a_casca(&mut f, t);
    assert_eq!(aplicados.len(), 1);
    assert_eq!(
        aplicados[0]["ajuste"],
        json!({"gama": -20, "contraste": 35, "saturacao": -100, "nitidez": 100, "contraluz": true})
    );
    let t2 = t + BATIMENTO;
    trocar(&mut f, SESSAO_A, &mut a, t2);
    assert_eq!(a.aplicado().unwrap()["contraste"], json!(35));
    // O inteiro sai sem `.0` (o Android lê Double e trunca).
    assert_eq!(serde_json::to_string(&f.ajuste()["gama"]).unwrap(), "-20");

    // Um pedido cru com a vista de antes da mudança perde o campo (vence o último).
    let vista_velha = f.versao() - 1;
    let velho = pedido_cru(&f, 9, json!({"vista": vista_velha, "ajuste": {"contraste": -50}}));
    f.receber(SESSAO_B, Some(&par("B")), &velho, t2);
    assert!(f.proximo_pedido(t2).is_none(), "quem não viu a mudança não vence");
    // Quem viu, vence.
    let novo = pedido_cru(&f, 10, json!({"vista": f.versao(), "ajuste": {"contraste": -50}}));
    f.receber(SESSAO_B, Some(&par("B")), &novo, t2);
    assert_eq!(aplicar_como_a_casca(&mut f, t2).len(), 1);
    assert_eq!(f.ajuste()["contraste"], json!(-50));
}

/// Inteiros de -100 a 100, mesmo que o descritor não diga `"inteiro":true`.
#[test]
fn os_ajustes_de_imagem_sao_inteiros_na_faixa() {
    let t = Instant::now();
    let mut f = filmador_com_imagem();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    for ruim in [r#"{"gama":12.5}"#, r#"{"contraste":101}"#, r#"{"saturacao":-101}"#, r#"{"nitidez":"10"}"#, r#"{"nitidez":true}"#] {
        assert!(a.pedir(ruim, t).is_err(), "{ruim} passou no receptor");
    }
    // E o filmador, com o pedido cru.
    for (i, ruim) in [json!({"gama": 12.5}), json!({"contraste": 101}), json!({"contraluz": 1})].into_iter().enumerate() {
        f.receber(SESSAO_A, Some(&par("rx")), &pedido_cru(&f, 100 + i as u64, json!({"ajuste": ruim})), t);
        assert!(f.proximo_pedido(t).is_none(), "{ruim} passou no filmador");
        assert_eq!(motivo_da_recusa(&mut f, t).0, motivo::FORA_DA_FAIXA);
    }
}

/// Valem em qualquer exposição, com as travas, e logo depois de um `restaurar`.
#[test]
fn os_ajustes_de_imagem_nao_dependem_da_exposicao() {
    let t = Instant::now();
    let mut f = filmador_com_imagem();
    let mut reg = f.ajuste().clone();
    reg.insert("exposicao".into(), json!("manual"));
    reg.insert("iso".into(), json!(400));
    reg.insert("obturadorNs".into(), json!(16_666_666));
    reg.insert("balanco".into(), json!("kelvin"));
    reg.insert("kelvin".into(), json!(5600));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"gama":10,"nitidez":-10,"contraluz":false}"#, t).unwrap();
    a.restaurar(t).unwrap();
    a.pedir(r#"{"saturacao":40}"#, t).unwrap();
    let mut travado = f.ajuste().clone();
    travado.insert("exposicao".into(), json!("auto"));
    travado.insert("travaExposicao".into(), json!(true));
    let caps: Objeto = serde_json::from_str(&caps_android_com_imagem()).unwrap();
    let parcial: Objeto = serde_json::from_str(r#"{"contraste":5,"gama":-5}"#).unwrap();
    assert!(conferir_coerencia(&caps, &travado, &parcial).is_ok());
}

/// O ausente vale 0: a casca que passa a gravar o 0 explícito não sobe a versão, e o pedido de 0
/// contra o registro sem a chave só ganha recibo. A contraluz é a exceção: ausente é o padrão da
/// câmera, e `false` é "desligada".
#[test]
fn imagem_ausente_vale_zero_e_contraluz_ausente_nao_e_falso() {
    let t = Instant::now();
    let mut f = filmador_com_imagem();
    let antes = f.versao();
    let mut reg = f.ajuste().clone();
    for campo in AJUSTES_DE_IMAGEM {
        assert!(!reg.contains_key(campo));
        reg.insert(campo.into(), json!(0));
    }
    reg.insert("nitidez".into(), Value::Null);
    reg.insert("gama".into(), json!(0.0));
    f.definir_ajuste(&Value::Object(reg.clone()).to_string(), 0, t).unwrap();
    assert_eq!(f.versao(), antes, "0, 0.0 e null = ausente");
    reg.insert(CONTRALUZ.into(), json!(false));
    f.definir_ajuste(&Value::Object(reg.clone()).to_string(), 0, t).unwrap();
    assert_eq!(f.versao(), antes + 1, "contraluz false ≠ ausente");

    let mut f = filmador_com_imagem();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let antes = f.versao();
    a.pedir(r#"{"gama":0,"saturacao":0}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    assert!(f.proximo_pedido(t).is_none());
    assert_eq!(f.versao(), antes);
}

/// O filmador sem os campos novos (de antes de 13/10): o receptor recusa na hora.
#[test]
fn imagem_sem_a_capacidade_e_campo_desconhecido() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    for campo in AJUSTES_DE_IMAGEM {
        assert!(a.pedir(&json!({campo: 10}).to_string(), t).is_err());
    }
    assert!(a.pedir(r#"{"contraluz":true}"#, t).is_err());
}

/// A forma dos descritores de imagem é conferida ao publicar as capacidades.
#[test]
fn os_descritores_de_imagem_tem_a_forma_do_contrato() {
    let com = |campo: &str, d: Value| {
        let mut c = v(&caps_android());
        c["controles"][campo] = d;
        NucleoDoFilmador::novo().definir_camera(Some((&c.to_string(), &ajuste_padrao()))).is_ok()
    };
    assert!(com("gama", json!({"min": -100, "max": 100, "passo": 1, "origem": "camera"})));
    assert!(com("gama", json!({"min": -100, "max": 100})), "a origem é opcional");
    assert!(com("nitidez", json!({"min": 0, "max": 100, "origem": "quall"})), "uma faixa menor, com o 0 dentro");
    assert!(!com("gama", json!({"min": -200, "max": 200})), "a unidade do driver não vai no fio");
    assert!(!com("contraste", json!({"min": 10, "max": 100})), "sem o neutro");
    assert!(!com("saturacao", json!({"min": -100.5, "max": 100})));
    assert!(!com("saturacao", json!({"min": -100, "max": 100, "origem": "driver"})));
    assert!(!com("saturacao", json!({"min": -100, "max": 100, "origem": 0})));
    assert!(!com("nitidez", json!({})), "sim/não não é ajuste de imagem");
    assert!(!com("gama", json!({"valores": ["auto"]})));
    assert!(com(CONTRALUZ, json!({})));
    assert!(com(CONTRALUZ, json!({"min": 0, "max": 3, "origem": "camera"})));
    assert!(!com(CONTRALUZ, json!({"min": -1, "max": 3})));
    assert!(!com(CONTRALUZ, json!({"min": 0, "max": 2.5})));
    assert!(!com(CONTRALUZ, json!({"valores": ["sim", "nao"]})));
}

/// A contraluz em níveis do driver: inteiro na faixa; sim/não não serve.
#[test]
fn contraluz_em_niveis() {
    let t = Instant::now();
    let mut c = v(&caps_android());
    c["controles"][CONTRALUZ] = json!({"min": 0, "max": 3, "origem": "camera"});
    let mut f = NucleoDoFilmador::novo();
    f.permitir(true);
    f.definir_camera(Some((&c.to_string(), &ajuste_padrao()))).unwrap();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"contraluz":2}"#, t).unwrap();
    assert!(a.pedir(r#"{"contraluz":1.5}"#, t).is_err());
    assert!(a.pedir(r#"{"contraluz":4}"#, t).is_err());
    assert!(a.pedir(r#"{"contraluz":true}"#, t).is_err());
}

/// O estado com o registro cheio (os 20 campos) ainda cabe num pedaço SCTP; as capacidades
/// completas do Android, com a imagem e a contraluz, também.
#[test]
fn com_a_imagem_o_estado_ainda_cabe() {
    let t = Instant::now();
    let mut f = filmador_com_imagem();
    let cheio = json!({"exposicao": "manual", "ev": -0.7, "travaExposicao": false, "travaIso": 400,
        "travaObturadorNs": 16666666, "iso": 1600, "obturadorNs": 8333333, "antiCintilacao": "60",
        "balanco": "kelvin", "kelvin": 5600, "travaBalanco": false,
        "travaGanhos": [1.9921875, 1.0, 1.0, 2.34375], "foco": "manual", "focoPosicao": 0.37,
        "travarFps": true, "gama": -100, "contraste": -100, "saturacao": -100, "nitidez": -100,
        "contraluz": true});
    f.definir_ajuste(&cheio.to_string(), 0, t).unwrap();
    f.definir_lido(r#"{"iso":1600,"obturadorNs":8333333,"kelvin":5550,"abertura":1.7,"focoPosicao":0.37,"divergentes":["kelvin"]}"#).unwrap();
    f.receber(SESSAO_A, Some(&par("Um nome de aparelho bem comprido, com acento ção")), &mensagem("ola", Objeto::new()), t);
    let saida = f.devidas(SESSAO_A, t);
    for (d, m) in &saida {
        eprintln!("{d:?}: {} bytes", m.len());
        // 13/10: as capacidades mediram 1.022 bytes e o estado 655.
        assert!(m.len() <= ALVO_DA_MENSAGEM, "{d:?} tem {} bytes: {m}", m.len());
    }
}

// ---------------------------------------------------------------------------------------------
// O zoom (R9 §3.8, 13/10)
// ---------------------------------------------------------------------------------------------

/// O descritor de zoom de um S24 Ultra (a câmera lógica traseira): 0,6× a 100×, quatro lentes.
fn descritor_do_zoom_s24() -> Value {
    json!({"min": 0.6, "max": 100, "passo": 0.1, "paradas": [0.6, 1, 3, 5, 10],
           "opticas": [0.6, 1, 3, 5], "origem": "camera"})
}

fn filmador_com_zoom() -> NucleoDoFilmador {
    let mut c = v(&caps_android_com_imagem());
    c["controles"][ZOOM] = descritor_do_zoom_s24();
    let mut f = NucleoDoFilmador::novo();
    f.permitir(true);
    f.definir_camera(Some((&c.to_string(), &ajuste_padrao()))).unwrap();
    f
}

/// O zoom vai e volta, em qualquer exposição, e vence o último como os outros campos.
#[test]
fn o_zoom_vai_e_volta_em_qualquer_exposicao() {
    let t = Instant::now();
    let mut f = filmador_com_zoom();
    let mut reg = f.ajuste().clone();
    reg.insert("exposicao".into(), json!("manual"));
    reg.insert("iso".into(), json!(400));
    reg.insert("obturadorNs".into(), json!(16_666_666));
    f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    a.pedir(r#"{"zoom":3}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    let aplicados = aplicar_como_a_casca(&mut f, t);
    assert_eq!(aplicados.len(), 1);
    assert_eq!(aplicados[0]["ajuste"], json!({"zoom": 3}));
    assert_eq!(serde_json::to_string(&f.ajuste()["zoom"]).unwrap(), "3", "inteiro sem .0");
    let t2 = t + BATIMENTO;
    trocar(&mut f, SESSAO_A, &mut a, t2);
    assert_eq!(a.aplicado().unwrap()["zoom"], json!(3));

    // Fracionário vale (o zoom não é inteiro), dentro da faixa.
    a.pedir(r#"{"zoom":0.6}"#, t2).unwrap();
    a.pedir(r#"{"zoom":2.35}"#, t2).unwrap();
    for ruim in [r#"{"zoom":0.5}"#, r#"{"zoom":100.5}"#, r#"{"zoom":"3"}"#, r#"{"zoom":true}"#, r#"{"zoom":null}"#] {
        assert!(a.pedir(ruim, t2).is_err(), "{ruim} passou no receptor");
    }
    // O filmador também recusa o pedido cru fora da faixa.
    f.receber(SESSAO_A, Some(&par("rx")), &pedido_cru(&f, 50, json!({"ajuste": {"zoom": 0.1}})), t2);
    assert!(f.proximo_pedido(t2).is_none());
    assert_eq!(motivo_da_recusa(&mut f, t2).0, motivo::FORA_DA_FAIXA);

    // Vence o último: quem não viu a mudança perde.
    let vista_velha = f.versao() - 1;
    f.receber(SESSAO_B, Some(&par("B")), &pedido_cru(&f, 51, json!({"vista": vista_velha, "ajuste": {"zoom": 5}})), t2);
    assert!(f.proximo_pedido(t2).is_none(), "quem não viu o 3× não vence");
    f.receber(SESSAO_B, Some(&par("B")), &pedido_cru(&f, 52, json!({"vista": f.versao(), "ajuste": {"zoom": 5}})), t2);
    assert_eq!(aplicar_como_a_casca(&mut f, t2).len(), 1);
    assert_eq!(f.ajuste()["zoom"], json!(5));
}

/// O ausente vale 1: a casca que passa a gravar `"zoom":1` (ou `1.0`, ou `null`) não sobe a versão,
/// e o pedido de 1× contra o registro sem a chave só ganha recibo.
#[test]
fn zoom_ausente_vale_um() {
    let t = Instant::now();
    let mut f = filmador_com_zoom();
    let antes = f.versao();
    for um in [json!(1), json!(1.0), Value::Null] {
        let mut reg = f.ajuste().clone();
        reg.insert(ZOOM.into(), um);
        f.definir_ajuste(&Value::Object(reg).to_string(), 0, t).unwrap();
        assert_eq!(f.versao(), antes, "1, 1.0 e null = ausente");
    }
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    let antes = f.versao();
    a.pedir(r#"{"zoom":1.0}"#, t).unwrap();
    trocar(&mut f, SESSAO_A, &mut a, t);
    assert!(f.proximo_pedido(t).is_none());
    assert_eq!(f.versao(), antes);
}

/// **O gesto contínuo não inunda o canal.** Uma pinça a 60 Hz por 1 s no receptor (60 `pedir`)
/// sai em no máximo 15 pedidos por segundo, o filmador entrega à casca só o que juntou, e o último
/// vale.
#[test]
fn a_pinca_continua_sai_a_15_por_segundo_e_o_ultimo_vale() {
    let t0 = Instant::now();
    let mut f = filmador_com_zoom();
    let mut a = receptor_pronto(&mut f, SESSAO_A, t0);
    let mut enviados = 0;
    let mut na_casca = 0;
    let mut ultimo = 0.0;
    for i in 0..60u32 {
        let agora = t0 + Duration::from_millis(u64::from(i) * 1000 / 60);
        ultimo = 1.0 + f64::from(i) * 0.05;
        a.pedir(&json!({"zoom": ultimo}).to_string(), agora).unwrap();
        for texto in a.devidas(agora) {
            enviados += usize::from(texto.contains("\"tipo\":\"pedido\""));
            f.receber(SESSAO_A, Some(&par("rx")), &texto, agora);
        }
        na_casca += aplicar_como_a_casca(&mut f, agora).len();
        for (_, texto) in f.devidas(SESSAO_A, agora) {
            a.receber(&texto, agora);
        }
    }
    // O fim do gesto: o último ainda pendente sai no envio seguinte.
    for passo in 1..=4u32 {
        let agora = t0 + Duration::from_secs(1) + INTERVALO_DOS_PEDIDOS * passo;
        trocar(&mut f, SESSAO_A, &mut a, agora);
        na_casca += aplicar_como_a_casca(&mut f, agora).len();
    }
    assert!(enviados <= 16, "{enviados} pedidos num segundo de pinça");
    assert!(na_casca <= 17, "{na_casca} aplicações na casca");
    let aplicado = f.ajuste()["zoom"].as_f64().unwrap();
    assert!((aplicado - ultimo).abs() < 1e-9, "{aplicado} ≠ {ultimo}");
    trocar(&mut f, SESSAO_A, &mut a, t0 + Duration::from_secs(3));
    assert!((a.aplicado().unwrap()["zoom"].as_f64().unwrap() - ultimo).abs() < 1e-9);
}

/// A forma do descritor de zoom é conferida ao publicar as capacidades.
#[test]
fn o_descritor_do_zoom_tem_a_forma_do_contrato() {
    let com = |d: Value| {
        let mut c = v(&caps_android());
        c["controles"][ZOOM] = d;
        NucleoDoFilmador::novo().definir_camera(Some((&c.to_string(), &ajuste_padrao()))).is_ok()
    };
    assert!(com(descritor_do_zoom_s24()));
    assert!(com(json!({"min": 1, "max": 4})), "as dicas são opcionais");
    assert!(com(json!({"min": 1, "max": 4, "origem": "quall", "paradas": [1, 2, 4], "opticas": []})));
    assert!(com(json!({"min": 2, "max": 2})), "uma câmera de zoom fixo, fora do 1×");
    assert!(!com(json!({"min": 0, "max": 10})), "fator 0 não existe");
    assert!(!com(json!({"min": -1, "max": 10})));
    assert!(!com(json!({"min": 1, "max": 5000})));
    assert!(!com(json!({"min": 1, "max": 10, "paradas": [0.5, 1]})), "parada abaixo do min");
    assert!(!com(json!({"min": 1, "max": 10, "opticas": [1, 12]})), "óptica acima do max");
    assert!(!com(json!({"min": 1, "max": 10, "paradas": "1,2"})));
    assert!(!com(json!({"min": 1, "max": 10, "paradas": [1, "3"]})));
    assert!(!com(json!({"min": 1, "max": 10, "paradas": vec![1; 13]})));
    assert!(!com(json!({"min": 1, "max": 10, "passo": 0})));
    assert!(!com(json!({"min": 1, "max": 10, "origem": "driver"})));
    assert!(!com(json!({})), "sim/não não é zoom");
    assert!(!com(json!({"valores": ["1", "2"]})));
}

/// Sem o zoom nas capacidades (o filmador de antes, ou a câmera sem zoom): o receptor recusa na hora.
#[test]
fn zoom_sem_a_capacidade_e_campo_desconhecido() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut a = receptor_pronto(&mut f, SESSAO_A, t);
    assert!(a.pedir(r#"{"zoom":2}"#, t).is_err());
}

/// O registro cheio com o zoom ainda cabe num pedaço SCTP; as capacidades completas de um S24 (com a
/// imagem, a contraluz e o zoom) passam do alvo só um pouco (o batimento repara, contrato §8).
#[test]
fn com_o_zoom_o_estado_ainda_cabe() {
    let t = Instant::now();
    let mut f = filmador_com_zoom();
    let cheio = json!({"exposicao": "manual", "ev": -0.7, "travaExposicao": false, "travaIso": 400,
        "travaObturadorNs": 16666666, "iso": 1600, "obturadorNs": 8333333, "antiCintilacao": "60",
        "balanco": "kelvin", "kelvin": 5600, "travaBalanco": false,
        "travaGanhos": [1.9921875, 1.0, 1.0, 2.34375], "foco": "manual", "focoPosicao": 0.37,
        "travarFps": true, "gama": -100, "contraste": -100, "saturacao": -100, "nitidez": -100,
        "contraluz": true, "zoom": 12.3});
    f.definir_ajuste(&cheio.to_string(), 0, t).unwrap();
    f.definir_lido(r#"{"iso":1600,"obturadorNs":8333333,"kelvin":5550,"abertura":1.7,"focoPosicao":0.37,"zoom":12.3,"divergentes":["kelvin"]}"#).unwrap();
    f.receber(SESSAO_A, Some(&par("Um nome de aparelho bem comprido, com acento ção")), &mensagem("ola", Objeto::new()), t);
    for (d, m) in f.devidas(SESSAO_A, t) {
        eprintln!("{d:?}: {} bytes", m.len());
        match d {
            Devida::Estado => assert!(m.len() <= ALVO_DA_MENSAGEM, "o estado tem {} bytes: {m}", m.len()),
            _ => assert!(m.len() <= ALVO_DA_MENSAGEM + 160, "{d:?} tem {} bytes: {m}", m.len()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// §17: R9b pelo canal do teleprompter (13/10)
// ---------------------------------------------------------------------------------------------

/// §17.4: num canal confiável o pedido se reenvia a cada segundo, e não a cada 250 ms; o prazo de
/// desistir continua o mesmo. Uma sessão nova esquece a entrega.
#[test]
fn no_canal_confiavel_o_pedido_se_reenvia_a_cada_segundo() {
    let t = Instant::now();
    let mut f = filmador_com_camera(t);
    let mut r = receptor_pronto(&mut f, SESSAO_A, t);
    assert_eq!(r.entrega_confiavel(), None);
    r.definir_entrega_confiavel(true);
    r.pedir(r#"{"ev":1.0}"#, t).unwrap();
    assert_eq!(r.devidas(t).len(), 1, "o pedido sai na hora");
    assert!(r.devidas(t + REENVIO_DO_PEDIDO).is_empty(), "confiável, não reenvia aos 250 ms");
    assert_eq!(r.devidas(t + REENVIO_DO_PEDIDO_CONFIAVEL).len(), 1, "reenvia em 1 s");
    let depois = t + PRAZO_DO_PEDIDO;
    for (_, m) in f.devidas(SESSAO_A, depois) {
        r.receber(&m, depois);
    }
    let _ = r.devidas(depois);
    assert_eq!(v(&r.estado_json(depois))["recusa"]["motivo"], "sem_resposta", "o prazo não muda");
    r.comecar_sessao(SESSAO_B, depois);
    assert_eq!(r.entrega_confiavel(), None, "a entrega é da sessão");
}

/// **Uma sessão de teleprompter de verdade** (§17.7): o prompter hospeda com o papel `teleprompter`
/// (o canal `ReliableUnordered`), o controle conecta com `controle_remoto`, as duas réplicas do
/// teleprompter bombeiam os mensageiros comuns, e o filmador e o controlador bombeiam os da câmera
/// (`Mensageiro::da_camera`). O controle só bombeia a câmera depois de ver `par_entende_camera`. O
/// pedido volta aplicado com autor, o teleprompter segue convergindo, e nenhuma bombeada do
/// teleprompter vê mensagem da câmera.
#[test]
fn pela_sessao_do_teleprompter_de_verdade() {
    use crate::cancel::Cancelamento;
    use crate::pairing::{PairedPeers, Pin};
    use crate::protocol::{Announcement, Capabilities, DeviceId, Papel, PROTOCOL_VERSION};
    use crate::session::{conectar, hospedar, SessionConfig};
    use crate::signaling::SignalingServer;
    use crate::teleprompter::Teleprompter;
    use crate::transport::{Delivery, TransportConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let anuncio = |id: &str, papel: Papel| Announcement {
        protocol_version: PROTOCOL_VERSION,
        device_id: DeviceId(id.into()),
        display_name: format!("{id} nome"),
        capabilities: Capabilities { screen_source: false, camera_source: false, sink: false },
        screen: None,
        papel: Some(papel),
    };
    let config = |a: Announcement, pin: &Pin| SessionConfig {
        announcement: a,
        pin: Some(pin.clone()),
        known: PairedPeers::new(),
        transport: TransportConfig::default(),
        tracks: Vec::new(),
        timeout: Duration::from_secs(30),
        cancelamento: Cancelamento::novo(),
        silencio_do_caminho: None,
    };
    let pin = Pin::parse("535353").expect("pin");
    let servidor = SignalingServer::bind(0).expect("bind");
    let porta = servidor.port().expect("porta");
    let p = pin.clone();
    let lado_p =
        std::thread::spawn(move || hospedar(&servidor, config(anuncio("prompter-e2e", Papel::Teleprompter), &p)));
    let destino = format!("127.0.0.1:{porta}").parse().expect("endereço");
    let sessao_c = conectar(destino, config(anuncio("controle-e2e", Papel::ControleRemoto), &pin)).expect("controle");
    let sessao_p = lado_p.join().expect("thread").expect("prompter");
    assert_eq!(sessao_p.session.entrega_do_canal(), Some(Delivery::ReliableUnordered));

    let tp_p = Arc::new(Teleprompter::nova("prompter-e2e", Papel::Teleprompter).unwrap());
    let tp_c = Arc::new(Teleprompter::nova("controle-e2e", Papel::ControleRemoto).unwrap());
    tp_p.ligar_camera(true).unwrap();
    // No controle, ligar a câmera não muda nada.
    tp_c.ligar_camera(true).unwrap();
    let filmador = Arc::new(Filmador::novo());
    filmador.permitir(true).unwrap();
    filmador.definir_camera(Some((&caps_android(), &ajuste_padrao()))).unwrap();
    let controle = Arc::new(Controlador::novo());
    let parar = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    // O prompter: o teleprompter e a câmera no mesmo laço da sessão, como as cascas.
    {
        let (tp, f, parar) = (Arc::clone(&tp_p), Arc::clone(&filmador), Arc::clone(&parar));
        let m = sessao_p.session.mensageiro();
        let mc = m.da_camera();
        threads.push(std::thread::spawn(move || {
            while !parar.load(Ordering::Relaxed) {
                let a = tp.bombear(&m, Duration::from_millis(30)).unwrap();
                let b = f.bombear(&mc, Duration::ZERO).unwrap();
                if a.fechada || b.fechada {
                    break;
                }
            }
        }));
    }
    // O controle: a câmera só com `par_entende_camera`.
    {
        let (tp, c, parar) = (Arc::clone(&tp_c), Arc::clone(&controle), Arc::clone(&parar));
        let m = sessao_c.session.mensageiro();
        let mc = m.da_camera();
        threads.push(std::thread::spawn(move || {
            while !parar.load(Ordering::Relaxed) {
                let a = tp.bombear(&m, Duration::from_millis(30)).unwrap();
                if a.fechada {
                    break;
                }
                if tp.estado().unwrap().par_entende_camera && c.bombear(&mc, Duration::ZERO).unwrap().fechada {
                    break;
                }
            }
        }));
    }
    {
        let (f, parar) = (Arc::clone(&filmador), Arc::clone(&parar));
        threads.push(std::thread::spawn(move || {
            let mut reg: Objeto = serde_json::from_str(&ajuste_padrao()).unwrap();
            while !parar.load(Ordering::Relaxed) {
                while let Some(p) = f.proximo_pedido().unwrap() {
                    let p = v(&p);
                    for (k, x) in p["ajuste"].as_object().unwrap() {
                        reg.insert(k.clone(), x.clone());
                    }
                    f.definir_ajuste(&Value::Object(reg.clone()).to_string(), p["n"].as_u64().unwrap()).unwrap();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }));
    }
    let esperar = |cond: &dyn Fn(&Value) -> bool| {
        let fim = Instant::now() + Duration::from_secs(10);
        loop {
            let e = v(&controle.estado_json().unwrap());
            if cond(&e) || Instant::now() > fim {
                return e;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let e = esperar(&|e| e["situacao"] == "pronto");
    assert_eq!(e["situacao"], "pronto", "{e}");
    assert!(tp_c.estado().unwrap().par_entende_camera, "o controle viu que o prompter tem a câmera");
    assert!(!tp_p.estado().unwrap().par_entende_camera, "o controle nunca diz que tem a câmera");
    tp_c.definir_velocidade(2.5).unwrap();
    controle.pedir(r#"{"balanco":"kelvin","kelvin":5600}"#).unwrap();
    let e = esperar(&|e| e["aplicado"]["kelvin"] == 5600 && e["pendente"] == json!({}));
    assert_eq!(e["aplicado"]["kelvin"], 5600, "{e}");
    assert_eq!(e["autor"], "controle-e2e nome");
    let ef = v(&filmador.estado_json().unwrap());
    assert_eq!(ef["controlado_por"]["nome"], "controle-e2e nome", "{ef}");
    // O teleprompter segue: a velocidade chega ao prompter.
    let fim = Instant::now() + Duration::from_secs(5);
    while tp_p.estado().unwrap().velocidade != 2.5 && Instant::now() < fim {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(tp_p.estado().unwrap().velocidade, 2.5, "o teleprompter não convergiu");
    // A mudança local do prompter também vale, e chega ao controle (vence o último).
    let mut local: Objeto = serde_json::from_str(&ajuste_padrao()).unwrap();
    local.insert("balanco".into(), json!("kelvin"));
    local.insert("kelvin".into(), json!(4000));
    filmador.definir_ajuste(&Value::Object(local).to_string(), 0).unwrap();
    let e = esperar(&|e| e["aplicado"]["kelvin"] == 4000);
    assert_eq!(e["aplicado"]["kelvin"], 4000, "{e}");
    assert_eq!(e["autor"], Value::Null, "a mudança foi do próprio prompter");
    // Nenhuma bombeada do teleprompter viu mensagem da câmera.
    assert_eq!(tp_p.estado().unwrap().contadores.de_outro_app, 0, "a câmera vazou para o teleprompter do prompter");
    assert_eq!(tp_c.estado().unwrap().contadores.de_outro_app, 0, "a câmera vazou para o teleprompter do controle");

    // O prompter desliga a câmera: o controle vê `par_entende_camera` cair.
    tp_p.ligar_camera(false).unwrap();
    let fim = Instant::now() + Duration::from_secs(5);
    while tp_c.estado().unwrap().par_entende_camera && Instant::now() < fim {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!tp_c.estado().unwrap().par_entende_camera);

    parar.store(true, Ordering::Relaxed);
    for t in threads {
        let _ = t.join();
    }
    drop(sessao_c);
    drop(sessao_p);
}
