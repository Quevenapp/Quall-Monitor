//! O painel de quem recebe, desenhado das capacidades de cada plataforma (R9b).

use super::*;
use crate::idioma::{com_idioma, Idioma};
use crate::modelo_dos_ajustes::{self as modelo, EstadoDosAjustes};
use crate::regras_dos_controles::{Capacidades, Faixa, Propriedade, FLAGS_AUTO, FLAGS_MANUAL};

/// As capacidades que um filmador Windows publica (a webcam de exemplo de `regras_dos_controles`).
fn capacidades_do_windows() -> Value {
    let f = |min, max, passo, padrao, b| Faixa::do_get_range(min, max, passo, padrao, b).unwrap();
    let mut c = Capacidades::new();
    c.insert(Propriedade::Exposicao, f(-11, -1, 1, -6, FLAGS_AUTO | FLAGS_MANUAL));
    c.insert(Propriedade::Ganho, f(0, 100, 1, 0, FLAGS_MANUAL));
    c.insert(Propriedade::Brilho, f(-64, 64, 1, 0, FLAGS_MANUAL));
    c.insert(Propriedade::Balanco, f(2800, 6500, 10, 4600, FLAGS_AUTO | FLAGS_MANUAL));
    c.insert(Propriedade::AntiCintilacao, f(0, 2, 1, 1, FLAGS_MANUAL));
    crate::regras_da_camera_remota::capacidades(&c, 30.0, false)
}

fn ajuste_manual() -> Value {
    json!({"exposicao":"manual","ev":0,"travaExposicao":false,"iso":400,"obturadorNs":16666666,"antiCintilacao":"60",
           "balanco":"kelvin","kelvin":5200,"travaBalanco":false,"foco":"manual","focoPosicao":0.42})
}

fn todos_os_estados() -> Vec<(String, EstadoRemoto)> {
    let mut v = Vec::new();
    for (plataforma, caps) in [("android", capacidades_do_android()), ("mac", capacidades_do_mac()), ("windows", capacidades_do_windows())] {
        for situacao in [PRONTO, NAO_PERMITIDO] {
            for (nome, ajuste) in [("auto", json!({})), ("manual", ajuste_manual()), ("travado", json!({"travaExposicao":true,"travaBalanco":true}))] {
                v.push((format!("{plataforma}-{situacao}-{nome}"), estado_de_exemplo(situacao, caps.clone(), ajuste)));
            }
        }
    }
    v
}

fn textos(q: &QuadroDosAjustes) -> Vec<String> {
    q.itens.iter().filter_map(|i| if let Item::Texto(t) = i { Some(t.texto_corrido()) } else { None }).collect()
}

fn no_estado(r: &EstadoRemoto, aba: Aba) -> EstadoDosAjustes {
    EstadoDosAjustes { aba, remoto: Some(r.clone()), ..Default::default() }
}

#[test]
fn cada_aba_cabe_sem_rolar_e_nenhum_controle_cobre_outro() {
    for (nome, r) in todos_os_estados() {
        for aba in Aba::TODAS {
            let e = no_estado(&r, aba);
            let (l, a) = modelo::tamanho(&e);
            assert_eq!((l, a), (lugar::largura(false), lugar::ALTURA), "o remoto é sem prévia e sem a faixa da opção");
            let janela = Ret::new(0.0, 0.0, l, a);
            let q = modelo::compor(&e);
            assert!(q.previa.is_none());
            assert!(q.lugar(ControleDosAjustes::PermitirRemoto).is_none(), "{nome}: a opção é de quem filma");
            for x in &q.controles {
                assert!(janela.contem(&x.ret), "{nome}/{aba:?}: {:?} fora da janela: {:?}", x.c, x.ret);
            }
            for (i, a_) in q.controles.iter().enumerate() {
                for b in q.controles.iter().skip(i + 1) {
                    assert!(!a_.ret.cruza(&b.ret), "{nome}/{aba:?}: {:?} cobre {:?}", a_.c, b.c);
                }
            }
            for item in &q.itens {
                if let Item::Texto(t) = item {
                    assert!(janela.contem(&t.ret), "{nome}/{aba:?}: texto fora: {:?}", t.texto_corrido());
                    for x in &q.controles {
                        assert!(!t.ret.cruza(&x.ret), "{nome}/{aba:?}: o texto {:?} fica debaixo de {:?}", t.texto_corrido(), x.c);
                    }
                }
            }
            // Os degraus de todo deslizante cabem no trackbar.
            for x in &q.controles {
                if let Some(d) = x.degraus {
                    assert!(d.max >= 0 && d.pos >= 0 && d.pos <= d.max, "{nome}/{aba:?}: {:?} {d:?}", x.c);
                }
            }
        }
    }
}

#[test]
fn o_android_desenha_tudo_o_que_anuncia() {
    use ControleDosAjustes as C;
    let r = estado_de_exemplo(PRONTO, capacidades_do_android(), json!({"ev":0.3}));
    let q = compor(Aba::Exposicao, &r);
    assert!(q.achar(C::Brilho).unwrap().habilitado, "o EV com Auto");
    assert_eq!(q.achar(C::Brilho).unwrap().degraus.unwrap().max, 40, "de −2 a 2, de 0,1 em 0,1");
    assert!(textos(&q).contains(&"+0,3 EV".to_string()));
    assert!(textos(&q).contains(&"EV".to_string()));
    assert!((0..4).all(|i| q.achar(C::Cintilacao(i)).unwrap().habilitado));
    // A linha do alto (§3.6).
    assert!(textos(&q).contains(&"ISO 400 · 1/60 s · 5150 K · f/1,7".to_string()));
    // ISO em terços de stop, de 50 a 3200, e as frações de cinema até o teto.
    let r = estado_de_exemplo(PRONTO, capacidades_do_android(), ajuste_manual());
    let q = compor(Aba::GanhoEObturador, &r);
    assert_eq!(q.achar(C::Ganho).unwrap().degraus.unwrap().max, 18, "19 degraus de ISO");
    let n = r.numero("obturadorNs").unwrap();
    let d = degraus("obturadorNs", &n);
    assert_eq!(*d.last().unwrap(), 33333333.0, "o teto (1/30) entra");
    assert!(d.contains(&(1e9f64 / 60.0).round()) && d.contains(&125000.0));
    assert!(textos(&q).contains(&"1/60 s".to_string()));
    // Balanço: a grade inteira, Kelvin com o deslizante.
    let q = compor(Aba::Balanco, &r);
    assert!((0..6).all(|i| q.achar(C::Balanco(i)).unwrap().habilitado));
    assert!(textos(&q).contains(&"5200 K".to_string()));
    // Foco manual com Perto ↔ Longe.
    let q = compor(Aba::Foco, &r);
    assert!(textos(&q).contains(&"0,42".to_string()));
    assert_eq!(q.achar(C::FocoPosicao).unwrap().degraus, Some(Degraus { max: 100, pos: 42 }));
}

#[test]
fn o_windows_do_outro_lado_e_brilho_e_ganho() {
    use ControleDosAjustes as C;
    let r = estado_de_exemplo(PRONTO, capacidades_do_windows(), json!({"ev":12}));
    let q = compor(Aba::Exposicao, &r);
    // O brilho, com a origem (o padrão do driver) somada, e nunca "EV".
    assert!(textos(&q).contains(&"Brilho".to_string()) && textos(&q).contains(&"12".to_string()));
    assert!(!textos(&q).iter().any(|t| t.contains("EV")));
    assert!(textos(&q).iter().any(|t| t.starts_with("Ganho 400")), "{:?}", textos(&q));
    let e = no_estado(&r, Aba::GanhoEObturador);
    assert_eq!(modelo::texto_acessivel(C::Aba(1), &e), "Ganho e obturador, aba");
    // O obturador em 2^v: de 2^-11 ao teto de 1/30 (2^-5), sete degraus.
    let r = estado_de_exemplo(PRONTO, capacidades_do_windows(), json!({"exposicao":"manual","iso":32,"obturadorNs":15625000}));
    let q = compor(Aba::GanhoEObturador, &r);
    assert_eq!(q.achar(C::Obturador).unwrap().degraus, Some(Degraus { max: 6, pos: 5 }));
    assert!(textos(&q).contains(&"1/64 s".to_string()));
    // O degrau mais rápido sai dentro da faixa publicada (a revisão do plano, achado 6).
    let Some(GestoRemoto::Pedido(p)) = gesto(C::Obturador, &r, Some(0)) else { panic!() };
    let ns = p["obturadorNs"].as_f64().unwrap();
    let n = r.numero("obturadorNs").unwrap();
    assert!(ns >= n.min && ns <= n.max, "{ns} fora de [{}, {}]", n.min, n.max);
    assert!(p["obturadorNs"].is_i64(), "inteiro, sem .0");
    // O balanço do Windows: só Auto e Kelvin, e a linha dos presets.
    let q = compor(Aba::Balanco, &r);
    assert!(q.achar(C::Balanco(0)).unwrap().habilitado && q.achar(C::Balanco(5)).unwrap().habilitado);
    assert!(!q.achar(C::Balanco(1)).unwrap().habilitado);
    assert!(textos(&q).contains(&"Este aparelho não oferece os presets de balanço.".to_string()));
    // Sem foco na webcam da bancada (§12).
    assert!(textos(&compor(Aba::Foco, &r)).contains(&"Esta câmera não oferece o foco manual.".to_string()));
}

#[test]
fn o_mac_diz_quem_limita() {
    use ControleDosAjustes as C;
    let r = estado_de_exemplo(PRONTO, capacidades_do_mac(), json!({}));
    let q = compor(Aba::Exposicao, &r);
    assert!(textos(&q).contains(&"O macOS não oferece a compensação de exposição para câmeras.".to_string()));
    assert!(q.achar(C::TravarExposicao).unwrap().habilitado);
    assert!(!q.achar(C::Exposicao(1)).unwrap().habilitado);
    // Um grupo em que nada se aplica é uma linha só (R9 §3.5).
    let q = compor(Aba::GanhoEObturador, &r);
    assert_eq!(textos(&q).iter().filter(|t| t.contains("macOS")).count(), 1);
    assert!(textos(&q).contains(&"O macOS não oferece ISO e o obturador para câmeras.".to_string()));
    assert!(q.lugar(C::PassarParaManual).is_none());
    let q = compor(Aba::Balanco, &r);
    assert!(textos(&q).contains(&"O macOS não oferece o Kelvin para câmeras.".to_string()));
    assert!(q.achar(C::TravarBalanco).unwrap().habilitado);
    let q = compor(Aba::Foco, &r);
    assert!(q.achar(C::Foco(1)).unwrap().habilitado && !q.achar(C::Foco(2)).unwrap().habilitado);
    assert!(textos(&q).contains(&"O macOS não oferece o foco manual para câmeras.".to_string()));
}

#[test]
fn nao_permitido_apaga_com_os_valores() {
    use ControleDosAjustes as C;
    let r = estado_de_exemplo(NAO_PERMITIDO, capacidades_do_android(), ajuste_manual());
    for aba in Aba::TODAS {
        let q = compor(aba, &r);
        assert!(q.controles.iter().filter(|x| !matches!(x.c, C::Aba(_))).all(|x| !x.habilitado), "{aba:?}");
        assert!(textos(&q).contains(&"O aparelho não permite controle remoto da câmera".to_string()));
    }
    assert!(textos(&compor(Aba::Balanco, &r)).contains(&"5200 K".to_string()), "com os valores");
    com_idioma(Idioma::En, || {
        assert!(textos(&compor(Aba::Exposicao, &r)).contains(&"This device doesn't allow remote camera control".to_string()));
    });
}

#[test]
fn sem_controles_nas_outras_situacoes() {
    for (situacao, frase) in [("esperando", "Lendo a câmera…"), (SEM_RESPOSTA, "O aparelho não respondeu."), (SEM_CAMERA, "O aparelho não está mostrando uma câmera.")] {
        let r = estado_de_exemplo(situacao, capacidades_do_android(), json!({}));
        let q = compor(Aba::Exposicao, &r);
        assert!(q.controles.is_empty(), "{situacao}");
        assert!(textos(&q).contains(&frase.to_string()), "{situacao}");
    }
    // Pronto sem capacidades (ainda não chegaram): também sem controles.
    let r = EstadoRemoto::de_json(r#"{"situacao":"pronto","capacidades":null}"#);
    assert!(!r.com_controles());
    assert!(EstadoRemoto::de_json("lixo").situacao == "esperando");
}

#[test]
fn as_recusas_e_a_divergencia() {
    let mut r = estado_de_exemplo(PRONTO, capacidades_do_android(), ajuste_manual());
    r.recusa = Some(("fora_da_faixa".into(), Some("iso".into())));
    assert_eq!(aviso_do_alto(&r).as_deref(), Some("Este aparelho não aceitou ISO."));
    r.recusa = Some(("nao_aplicado".into(), None));
    assert_eq!(aviso_do_alto(&r).as_deref(), Some("O aparelho não conseguiu aplicar o ajuste."));
    r.recusa = Some(("um_codigo_novo".into(), None));
    assert_eq!(aviso_do_alto(&r).as_deref(), Some("O aparelho não conseguiu aplicar o ajuste."), "código desconhecido é nao_aplicado (§3.5)");
    r.recusa = Some(("sem_resposta".into(), None));
    assert_eq!(aviso_do_alto(&r).as_deref(), Some("O aparelho não respondeu."));
    r.recusa = Some(("superado".into(), Some("iso".into())));
    assert_eq!(aviso_do_alto(&r), None, "superado não se mostra");
    r.recusa = None;
    r.lido.insert("divergentes".into(), json!(["iso"]));
    r.ajuste.insert("iso".into(), json!(800));
    assert_eq!(aviso_do_alto(&r).as_deref(), Some("A câmera usou 400 em vez de 800."));
    com_idioma(Idioma::En, || {
        let mut r = estado_de_exemplo(PRONTO, capacidades_do_android(), json!({}));
        r.recusa = Some(("incoerente".into(), Some("ev".into())));
        assert_eq!(aviso_do_alto(&r).as_deref(), Some("This device didn't accept exposure compensation."));
        assert_eq!(rotulo_da_aba(&r, Aba::GanhoEObturador), "ISO e obturador");
        assert_eq!(crate::idioma::t(rotulo_da_aba(&r, Aba::GanhoEObturador)), "ISO & shutter");
    });
}

#[test]
fn os_gestos_viram_pedidos_parciais() {
    use ControleDosAjustes as C;
    let r = estado_de_exemplo(PRONTO, capacidades_do_android(), json!({"travaExposicao":true}));
    let p = |c, d| match gesto(c, &r, d) {
        Some(GestoRemoto::Pedido(v)) => v,
        outro => panic!("{c:?}: {outro:?}"),
    };
    assert_eq!(p(C::Exposicao(1), None), json!({"exposicao":"manual"}));
    assert_eq!(p(C::PassarParaManual, None), json!({"exposicao":"manual"}), "só o modo: a casca parte do lido");
    assert_eq!(p(C::Exposicao(0), None), json!({"exposicao":"auto"}));
    assert_eq!(p(C::TravarExposicao, None), json!({"travaExposicao":false}));
    assert_eq!(p(C::Cintilacao(2), None), json!({"antiCintilacao":"60"}));
    assert_eq!(p(C::Balanco(3), None), json!({"balanco":"luzDoDia"}));
    assert_eq!(p(C::Foco(1), None), json!({"foco":"travado"}));
    assert_eq!(p(C::Brilho, Some(23)), json!({"ev":0.3}));
    assert_eq!(p(C::Ganho, Some(0)), json!({"iso":50}));
    assert_eq!(p(C::Ganho, Some(999)), json!({"iso":3200}), "o degrau além do fim é o último");
    assert_eq!(p(C::Kelvin, Some(32)), json!({"kelvin":5200}));
    assert_eq!(p(C::FocoPosicao, Some(42)), json!({"focoPosicao":0.42}));
    assert_eq!(gesto(C::Restaurar, &r, None), Some(GestoRemoto::Restaurar));
    assert_eq!(gesto(C::Aba(3), &r, None), Some(GestoRemoto::Aba(Aba::Foco)));
    assert_eq!(gesto(C::PermitirRemoto, &r, None), None);
    // Pelo modelo da janela: o mesmo gesto.
    let e = no_estado(&r, Aba::Exposicao);
    assert!(matches!(modelo::gesto(C::Exposicao(1), &e, None), Some(modelo::Gesto::Remoto(GestoRemoto::Pedido(_)))));
    assert!(matches!(modelo::gesto(C::Aba(2), &e, None), Some(modelo::Gesto::Aba(Aba::Balanco))));
}

#[test]
fn os_textos_de_valor() {
    let n = |v: Value| match Descritor::de(&v) {
        Some(Descritor::Numero(n)) => n,
        _ => panic!(),
    };
    let ev = n(json!({"min":-2,"max":2,"passo":0.1}));
    assert_eq!(texto_do_valor("ev", Some(&ev), 0.0), "0 EV");
    assert_eq!(texto_do_valor("ev", Some(&ev), -0.7), "-0,7 EV");
    assert_eq!(texto_do_valor("ev", Some(&n(json!({"min":-2,"max":2,"passo":0.5}))), 1.5), "+1,5 EV");
    let brilho = n(json!({"min":-128,"max":127,"inteiro":true,"unidade":"brilho","origem":128}));
    assert_eq!(texto_do_valor("ev", Some(&brilho), -28.0), "100");
    assert_eq!(texto_do_obturador_ns(1e9 / 250.0, false), "1/250 s");
    assert_eq!(texto_do_obturador_ns(2e9, false), "2 s");
    assert_eq!(texto_do_obturador_ns(1e9 / 128.0, true), "1/128 s");
    com_idioma(Idioma::En, || assert_eq!(texto_do_valor("ev", Some(&ev), 0.3), "+0.3 EV"));
    // O foco calibrado do Android: metros, e o infinito na posição 0.
    let foco = n(json!({"min":0,"max":1,"passo":0.01,"calibrado":10.0}));
    assert_eq!(foco.calibrado, Some(10.0));
    assert_eq!(texto_do_valor("focoPosicao", Some(&foco), 0.0), "∞");
    assert_eq!(texto_do_valor("focoPosicao", Some(&foco), 0.5), "0,20 m");
    assert_eq!(texto_do_valor("focoPosicao", Some(&foco), 0.005), "20,0 m");
    assert_eq!(texto_do_valor("focoPosicao", Some(&n(json!({"min":0,"max":1}))), 0.42), "0,42");
    // O ganho do Windows anda pelo passo do driver.
    let g = n(json!({"min":0,"max":100,"passo":5,"inteiro":true,"unidade":"ganho"}));
    assert_eq!(degraus("iso", &g).len(), 21);
    // Um descritor sem passo nem inteiro: cem degraus.
    assert_eq!(degraus("focoPosicao", &n(json!({"min":0,"max":1}))).len(), 101);
    // Faixas enormes são amostradas, e o último é o máximo.
    let grande = n(json!({"min":0,"max":1000000,"inteiro":true}));
    let d = degraus("iso", &n(json!({"min":0,"max":1000000,"inteiro":true,"unidade":"ganho"})));
    assert!(d.len() <= 1001 && *d.last().unwrap() == 1_000_000.0, "{}", d.len());
    assert!(degraus("kelvin", &grande).len() <= 1001);
}
