//! Testes da busca. **Tudo em loopback e em portas próprias**: nos testes, broadcast e sondagem
//! vão só para `127.0.0.1` (`destinos_de_broadcast`, `alvos_da_sondagem`), para um `cargo test` no
//! MacBook da bancada não aparecer na lista dos aparelhos de verdade.

use super::*;
use crate::protocol::{Capabilities, Papel, PROTOCOL_VERSION};
use std::net::TcpListener;

fn anuncio_de(id: &str, nome: &str) -> Announcement {
    Announcement {
        protocol_version: PROTOCOL_VERSION,
        device_id: DeviceId(id.into()),
        display_name: nome.into(),
        capabilities: Capabilities { screen_source: false, camera_source: true, sink: false },
        screen: None,
        papel: None,
    }
}

/// Duas portas UDP livres agora (o sistema não as devolve de novo tão cedo).
fn portas_livres() -> (u16, u16) {
    let a = UdpSocket::bind("127.0.0.1:0").expect("a");
    let b = UdpSocket::bind("127.0.0.1:0").expect("b");
    (a.local_addr().expect("a").port(), b.local_addr().expect("b").port())
}

fn opcoes(pergunta: u16, escuta: u16) -> OpcoesDaBusca {
    OpcoesDaBusca {
        mdns: false,
        porta_da_pergunta: pergunta,
        porta_da_escuta: escuta,
        ..OpcoesDaBusca::default()
    }
}

/// Um token v3 de exemplo (32 hex) e o id efêmero que a lista dá a quem o anuncia.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";
fn visto(token: &str) -> DeviceId {
    DeviceId(format!("discovery-{token}"))
}

fn esperar<T>(limite: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let fim = Instant::now() + limite;
    while Instant::now() < fim {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

// ---- o datagrama ----

#[test]
fn o_anuncio_vai_e_volta_com_as_chaves_do_txt_v3() {
    let mut a = anuncio_de("android-1a2b", "Galaxy A07");
    a.papel = Some(Papel::Teleprompter);
    let bytes = anuncio(&a, 7979, TOKEN, true, Some("9f2c01d4")).expect("publicável");
    let texto = String::from_utf8(bytes.clone()).expect("utf-8");
    // As mesmas chaves do TXT v3, e os valores como texto: um leitor só para os dois caminhos.
    for chave in ["\"v\":\"3\"", "\"t\":\"0123456789abcdef0123456789abcdef\"", "\"c\":\"c\"", "\"p\":\"7979\"", "\"pa\":\"teleprompter\"", "\"cc\":\"1\""] {
        assert!(texto.contains(chave), "{chave} em {texto}");
    }
    let Some(Lido::Anuncio { anuncio: lido, porta, aceita_codigo, nome, r }) = ler_datagrama(&bytes) else {
        panic!("não leu: {texto}")
    };
    assert_eq!((porta, aceita_codigo, r.as_deref()), (7979, true, Some("9f2c01d4")));
    assert_eq!(nome, None, "sem nome definido, o anúncio não leva `nm`");
    assert_eq!(lido.device_id, visto(TOKEN), "a lista só conhece o token");
    assert_eq!(lido.display_name, "Quall 01234567");
    assert_eq!(lido.papel, Some(Papel::Teleprompter));
}

/// **Sentinela do v3** (`docs/v3-no-privado.md` §5.3): nem o id nem o nome do aparelho saem no
/// datagrama, por mais que a casca os tenha posto no anúncio.
#[test]
fn o_id_e_o_nome_nunca_saem_no_datagrama() {
    let a = anuncio_de("ID-SENTINELA-PERSISTENTE", "NOME-SENTINELA-PESSOAL");
    for r in [None, Some("ab")] {
        let bytes = anuncio(&a, 7877, TOKEN, false, r).expect("publicável");
        let texto = String::from_utf8(bytes).expect("utf-8");
        assert!(!texto.contains("SENTINELA"), "{texto}");
        assert!(!texto.contains("\"id\"") && !texto.contains("\"n\""), "{texto}");
    }
}

#[test]
fn sem_codigo_a_chave_cc_nem_sai() {
    let bytes = anuncio(&anuncio_de("x", "X"), 7877, TOKEN, false, None).expect("publicável");
    let texto = String::from_utf8(bytes.clone()).expect("utf-8");
    assert!(!texto.contains("cc") && !texto.contains("\"r\""), "{texto}");
    assert!(matches!(ler_datagrama(&bytes), Some(Lido::Anuncio { aceita_codigo: false, r: None, .. })));
}

/// §8.7: o nome vai em `nm`, saneado na saída e de novo na chegada; o alias não muda.
#[test]
fn o_datagrama_leva_o_nome_saneado() {
    let a = anuncio_de("x", "NOME-DA-SESSAO");
    let bytes = anuncio_com_nome(&a, 7877, TOKEN, false, Some(" iPhone\n 7 "), None).expect("publicável");
    let texto = String::from_utf8(bytes.clone()).expect("utf-8");
    assert!(texto.contains("\"nm\":\"iPhone 7\""), "{texto}");
    assert!(!texto.contains("NOME-DA-SESSAO") && !texto.contains("\"n\""), "o nome da sessão não sai: {texto}");
    let Some(Lido::Anuncio { anuncio: lido, nome, .. }) = ler_datagrama(&bytes) else { panic!("não leu") };
    assert_eq!(nome.as_deref(), Some("iPhone 7"));
    assert_eq!(lido.display_name, "Quall 01234567", "o alias continua o do token");
    // Um forjado com nome longo e quebra chega cortado e numa linha só.
    let forjado = format!(
        r#"{{"q":"quall","f":2,"m":"anuncio","v":"3","t":"{TOKEN}","c":"c","p":"7877","nm":"MacBook do Pessoa\n{}"}}"#,
        "x".repeat(200)
    )
    .replace('\n', "\\n");
    let Some(Lido::Anuncio { nome, .. }) = ler_datagrama(forjado.as_bytes()) else { panic!("não leu o forjado") };
    let nome = nome.unwrap_or_default();
    assert!(nome.len() <= crate::discovery::TETO_DO_NOME_ANUNCIADO && !nome.contains('\n'), "{nome}");
    assert!(nome.starts_with("MacBook do Pessoa x"));
    // Nome que o saneamento esvazia não sai.
    let vazio = anuncio_com_nome(&a, 7877, TOKEN, false, Some("\u{200B}\n"), None).expect("publicável");
    assert!(!String::from_utf8(vazio).expect("utf-8").contains("\"nm\""));
}

/// §8.7, ordem da linha: nome guardado (par reconhecido) > nome anunciado (com o alias embaixo) >
/// alias.
#[test]
fn a_linha_prefere_o_par_guardado_depois_o_nome_e_por_fim_o_alias() {
    let alias = "Quall 46eb4d0a";
    let par = rotulo_da_linha(Some("MacBook do Pessoa"), Some("Impostor"), alias);
    assert_eq!(par, RotuloDaLinha { titulo: "MacBook do Pessoa".into(), alias_embaixo: None, pareado: true });
    let nomeado = rotulo_da_linha(None, Some("iPhone 7"), alias);
    assert_eq!(
        nomeado,
        RotuloDaLinha { titulo: "iPhone 7".into(), alias_embaixo: Some(alias.into()), pareado: false }
    );
    // Um nome guardado vazio não conta; um anunciado vazio ou igual ao alias também não.
    assert_eq!(rotulo_da_linha(Some("  "), Some("iPhone 7"), alias), nomeado);
    let so_alias = RotuloDaLinha { titulo: alias.into(), alias_embaixo: None, pareado: false };
    assert_eq!(rotulo_da_linha(None, None, alias), so_alias);
    assert_eq!(rotulo_da_linha(None, Some(" \n "), alias), so_alias);
    assert_eq!(rotulo_da_linha(None, Some(alias), alias), so_alias);
    // Quem se chama como um par na rede continua com o alias embaixo.
    let impostor = rotulo_da_linha(None, Some("MacBook do Pessoa"), "Quall 9f8e7d6c");
    assert_eq!(impostor.alias_embaixo.as_deref(), Some("Quall 9f8e7d6c"));
    assert!(!impostor.pareado);
}

#[test]
fn token_invalido_nao_vira_datagrama() {
    assert_eq!(anuncio(&anuncio_de("x", "X"), 7877, "curto", false, None), None);
    assert_eq!(anuncio(&anuncio_de("x", "X"), 0, TOKEN, false, None), None);
}

#[test]
fn datagrama_com_id_ou_nome_e_recusado() {
    for velho in [
        br#"{"q":"quall","f":2,"m":"anuncio","v":"3","t":"0123456789abcdef0123456789abcdef","id":"a","c":"c","p":"7877"}"#.as_slice(),
        br#"{"q":"quall","f":2,"m":"anuncio","v":"2","id":"a","n":"A","c":"c","p":"7877"}"#,
        br#"{"q":"quall","f":1,"t":"anuncio","v":"2","id":"a","n":"A","c":"c","p":"7877"}"#,
    ] {
        assert_eq!(ler_datagrama(velho), None, "{}", String::from_utf8_lossy(velho));
    }
}

#[test]
fn a_pergunta_vai_e_volta_dirigida_ou_nao() {
    assert_eq!(ler_datagrama(&pergunta("ab", None)), Some(Lido::Pergunta { r: "ab".into(), id: None }));
    let id = DeviceId("mac-1".into());
    assert_eq!(
        ler_datagrama(&pergunta("cd", Some(&id))),
        Some(Lido::Pergunta { r: "cd".into(), id: Some(id) })
    );
}

#[test]
fn o_que_nao_e_nosso_e_ignorado_em_silencio() {
    for lixo in [
        &b"oi"[..],
        b"{}",
        br#"{"q":"outro","f":1,"t":"pergunta","r":"a"}"#,
        br#"{"q":"quall","f":2,"t":"pergunta","r":"a"}"#,
        br#"{"q":"quall","f":1,"t":"coisa_nova","r":"a"}"#,
        br#"{"q":"quall","f":1,"t":"anuncio","v":"2"}"#,
        br#"{"q":"quall","f":1,"t":"pergunta"}"#,
        br#"{"q":"quall","f":1,"t":"pergunta","r":"a"}"#, // a pergunta de uma build v2
        br#"{"q":"quall","f":3,"m":"pergunta","r":"a"}"#, // um formato mais novo
        br#"{"q":"quall","f":2,"m":"coisa_nova","r":"a"}"#,
    ] {
        assert_eq!(ler_datagrama(lixo), None, "{}", String::from_utf8_lossy(lixo));
    }
    let grande = vec![b' '; TETO_DO_DATAGRAMA + 1];
    assert_eq!(ler_datagrama(&grande), None);
}

// ---- redes e sondagem ----

#[test]
fn a_sondagem_e_a_24_do_proprio_endereco_sem_ele_rede_e_broadcast() {
    let r = RedeLocal { ip: Ipv4Addr::new(192, 168, 15, 8), prefixo: 24 };
    let alvos = r.alvos_da_sondagem();
    assert_eq!(alvos.len(), 253);
    assert!(!alvos.contains(&r.ip));
    assert!(!alvos.contains(&Ipv4Addr::new(192, 168, 15, 0)));
    assert!(!alvos.contains(&Ipv4Addr::new(192, 168, 15, 255)));
    assert_eq!(r.broadcast(), Ipv4Addr::new(192, 168, 15, 255));
    assert_eq!(r.como_texto(), "192.168.15.0/24");

    // Máscara mais larga: só a /24 do endereço, nunca a /16 inteira.
    let larga = RedeLocal { ip: Ipv4Addr::new(10, 0, 7, 20), prefixo: 16 };
    let alvos = larga.alvos_da_sondagem();
    assert_eq!(alvos.len(), 253);
    assert!(alvos.iter().all(|ip| ip.octets()[..3] == [10, 0, 7]));
    assert_eq!(larga.broadcast(), Ipv4Addr::new(10, 0, 255, 255));

    // Mais estreita: só a própria sub-rede.
    let estreita = RedeLocal { ip: Ipv4Addr::new(192, 168, 1, 1), prefixo: 30 };
    assert_eq!(estreita.alvos_da_sondagem(), vec![Ipv4Addr::new(192, 168, 1, 2)]);
}

#[test]
fn as_causas_seguem_a_regra_do_desenho() {
    let casa = [RedeLocal { ip: Ipv4Addr::new(192, 168, 15, 8), prefixo: 24 }];
    assert_eq!(causas_provaveis(&[], None, true, 0, true, false), vec![Causa::SemRedeLocal]);
    assert_eq!(
        causas_provaveis(&casa, Some(IpAddr::from([192, 168, 0, 9])), false, 0, true, false),
        vec![Causa::SubRedeDiferente]
    );
    assert!(causas_provaveis(&casa, Some(IpAddr::from([192, 168, 15, 9])), false, 0, true, false).is_empty());
    assert!(causas_provaveis(&casa, None, true, 0, true, false).is_empty(), "cedo demais para dizer");
    assert_eq!(causas_provaveis(&casa, None, true, 1, true, false), vec![Causa::Isolamento]);
    assert_eq!(causas_provaveis(&casa, None, true, 1, true, true), vec![Causa::Firewall, Causa::Isolamento]);
    assert_eq!(causas_provaveis(&casa, None, true, 0, false, false), vec![Causa::Firewall]);
}

#[test]
fn o_limite_de_respostas_segura_origem_e_total() {
    let mut l = Limite::default();
    let t0 = Instant::now();
    let a = SocketAddr::from(([192, 168, 1, 2], 50000));
    assert!(l.pode(a, t0));
    assert!(!l.pode(a, t0 + Duration::from_millis(100)), "a mesma origem espera");
    assert!(l.pode(a, t0 + INTERVALO_POR_ORIGEM));
    let mut l = Limite::default();
    let aceitas = (0..100u8).filter(|i| l.pode(SocketAddr::from(([10, 0, 0, *i], 50000)), t0)).count();
    assert_eq!(aceitas, RESPOSTAS_POR_SEGUNDO as usize);
}

#[test]
fn so_responde_a_lan() {
    assert!(origem_permitida(IpAddr::from([192, 168, 0, 5])));
    assert!(origem_permitida(IpAddr::from([169, 254, 1, 1])));
    assert!(origem_permitida(IpAddr::from([127, 0, 0, 1])));
    assert!(!origem_permitida(IpAddr::from([8, 8, 8, 8])));
    assert!(!origem_permitida(IpAddr::from([100, 64, 0, 1])));
}

// ---- a lista que não pisca ----

#[test]
fn quem_some_fica_visto_ha_x_s_e_so_depois_sai() {
    let agora = Instant::now();
    let e = Entrada {
        desde: agora,
        anuncio: anuncio_de("x", "X"),
        porta: 7877,
        aceita_codigo: false,
        nome_anunciado: None,
        via: BTreeSet::from(["udp"]),
        mdns: None,
        udp: Some((IpAddr::from([192, 168, 15, 9]), agora - Duration::from_secs(20))),
        ultimo: agora - Duration::from_secs(20),
    };
    let r = e.retrato(agora, "x|192.168.15.9|7877");
    assert!(!r.vivo);
    assert!(!r.proprio && r.conhecido.is_none());
    assert_eq!(r.chave, "x|192.168.15.9|7877");
    assert!(r.visto_ha >= Duration::from_secs(20));
    // O endereço velho continua lá: é o que a pessoa tenta se tocar no item.
    assert_eq!(r.endpoint, Some("192.168.15.9:7877".parse().expect("e")));
    assert!(VIVO_POR + LEMBRADO_POR > Duration::from_secs(20), "e ainda não saiu da lista");
}

// ---- ponta a ponta, em loopback ----

/// §8.7, ponta a ponta: o anúncio com nome chega à lista com `nome_anunciado`; o sem nome (uma build
/// das lojas, ou o campo apagado) fica com o alias.
#[test]
fn anuncio_sem_nome_fica_com_o_alias() {
    let (p, e) = portas_livres();
    let outro = "fedcba9876543210fedcba9876543210";
    let _com = Respondedor::iniciar_em_com_nome(
        &anuncio_de("cam-nome", "x"),
        7877,
        false,
        TOKEN,
        Some("Câmera\n 2".into()),
        p,
        e,
    );
    let _sem = Respondedor::iniciar_em(&anuncio_de("cam-sem", "y"), 7878, false, outro, p, e);
    let busca = Busca::com(OpcoesDaBusca { sondar: false, ..opcoes(p, e) });
    let com = esperar(Duration::from_secs(3), || busca.aparelho(&visto(TOKEN))).expect("o com nome");
    let sem = esperar(Duration::from_secs(3), || busca.aparelho(&visto(outro))).expect("o sem nome");
    assert_eq!(com.nome_anunciado.as_deref(), Some("Câmera 2"));
    assert_eq!(com.anuncio.display_name, "Quall 01234567");
    assert_eq!(sem.nome_anunciado, None);
    assert_eq!(sem.anuncio.display_name, "Quall fedcba98");
}

#[test]
fn o_anuncio_por_broadcast_chega_e_o_respondedor_sai_ao_soltar() {
    let (p, e) = portas_livres();
    let a = anuncio_de("cam-udp", "Câmera UDP");
    let r = Respondedor::iniciar_em(&a, 7877, true, TOKEN, p, e);
    assert_eq!(anuncios_no_ar(p), 1);
    let busca = Busca::com(OpcoesDaBusca { sondar: false, ..opcoes(p, e) });
    let achado = esperar(Duration::from_secs(3), || busca.aparelho(&visto(TOKEN))).expect("achou");
    assert!(achado.vivo && achado.aceita_codigo);
    // O anunciante é deste processo: o item se diz próprio, para a casca não listar a si mesma.
    assert!(achado.proprio, "o próprio anúncio tinha de se reconhecer");
    assert_eq!(achado.via, vec!["udp"]);
    assert_eq!(achado.endpoint.map(|x| x.port()), Some(7877));
    drop(r);
    assert_eq!(anuncios_no_ar(p), 0);
}

#[test]
fn sem_broadcast_a_sondagem_acha_depois_de_dois_segundos() {
    let (p, e) = portas_livres();
    let a = anuncio_de("cam-sonda", "Câmera da sondagem");
    let _r = Respondedor::iniciar_em(&a, 7900, false, TOKEN, p, e);
    let inicio = Instant::now();
    let busca = Busca::com(OpcoesDaBusca { broadcast: false, ..opcoes(p, e) });
    let achado = esperar(Duration::from_secs(6), || busca.aparelho(&visto(TOKEN))).expect("a sondagem achou");
    assert!(inicio.elapsed() >= SONDAR_DEPOIS_DE, "a sondagem não começa antes de 2 s");
    assert_eq!(achado.via, vec!["sondagem"]);
    assert_eq!(busca.diagnostico().sondagens, 1);
}

/// No v3 a pergunta dirigida por id **não é respondida** (seria um oráculo de presença por id para
/// qualquer um da LAN, `docs/v3-no-privado.md` §5.3); os dois anúncios do processo seguem no ar.
#[test]
fn dois_anuncios_no_mesmo_processo_e_a_pergunta_dirigida_nao_tem_resposta() {
    let (p, e) = portas_livres();
    let video = anuncio_de("mesmo-aparelho-video", "Vídeo");
    let mut prompter = anuncio_de("mesmo-aparelho-prompter", "Prompter");
    prompter.papel = Some(Papel::Teleprompter);
    let token_do_prompter = "fedcba9876543210fedcba9876543210";
    let _r1 = Respondedor::iniciar_em(&video, 7877, false, TOKEN, p, e);
    let _r2 = Respondedor::iniciar_em(&prompter, 7979, false, token_do_prompter, p, e);
    assert_eq!(anuncios_no_ar(p), 2, "um respondedor só, dois anúncios");

    // Sem escuta (broadcast desligado): só as respostas às perguntas dirigidas — a da dica e a da
    // sondagem do alvo, as duas aos 1 s — entrariam.
    let busca = Busca::com(OpcoesDaBusca {
        alvo: Some(prompter.device_id.clone()),
        dica: Some("127.0.0.1:7979".parse().expect("dica")),
        broadcast: false,
        ..opcoes(p, e)
    });
    std::thread::sleep(Duration::from_millis(2000));
    assert!(busca.aparelho(&visto(token_do_prompter)).is_none(), "a pergunta dirigida foi respondida");
    assert!(busca.aparelho(&visto(TOKEN)).is_none(), "a pergunta dirigida foi respondida");

    // A pergunta comum (sondagem) acha os dois, cada um com o seu token.
    let busca = Busca::com(OpcoesDaBusca { broadcast: false, ..opcoes(p, e) });
    let achado = esperar(Duration::from_secs(6), || busca.aparelho(&visto(token_do_prompter))).expect("o prompter");
    assert_eq!(achado.anuncio.papel, Some(Papel::Teleprompter));
    assert!(esperar(Duration::from_secs(2), || busca.aparelho(&visto(TOKEN))).is_some(), "o vídeo");
}

/// `localizar` pelo id fica desligado até a etapa 3 de `docs/v3-no-privado.md`.
#[test]
fn localizar_publico_e_invalid_ate_a_etapa_3() {
    let r = localizar(&DeviceId("qualquer".into()), None, Duration::from_millis(100));
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[test]
#[ignore = "localizar volta pelo Challenge do v3 na etapa 3 de docs/v3-no-privado.md"]
fn localizar_acha_o_ip_novo_pelo_id_quando_o_antigo_nao_atende() {
    let (p, e) = portas_livres();
    let a = anuncio_de("a07-dhcp", "A07");
    let _r = Respondedor::iniciar_em(&a, 7877, true, TOKEN, p, e);
    // O endereço antigo: uma porta TCP fechada (o DHCP trocou o IP). A 1, e não uma efêmera que este
    // teste abre e solta: o `bind(0)` de outro teste em paralelo poderia pegar a mesma porta, e o TCP
    // desta procura cairia na sinalização dele. (Foi a primeira suspeita para a falha do teste do
    // atendente no Dell em 12/10; o controle na base `93b45a82` falhou igual, 2 em 3 — a causa é outra
    // e anterior a esta frente. A precaução fica.)
    let antigo: SocketAddr = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 1));
    let achado = localizar_com(&a.device_id, Some(antigo), Duration::from_secs(5), opcoes(p, e)).expect("achou");
    assert_eq!(achado.endpoint.port(), 7877);
    assert!(achado.mudou, "a casca grava o endereço novo");
    assert!(achado.aceita_codigo);
    assert_ne!(achado.via, "endereco_antigo");
}

#[test]
#[ignore = "localizar volta pelo Challenge do v3 na etapa 3 de docs/v3-no-privado.md"]
fn localizar_aceita_o_endereco_antigo_de_um_anfitriao_sem_udp() {
    let (p, e) = portas_livres();
    // Um anfitrião de build anterior: atende TCP e não responde UDP.
    let antigo = TcpListener::bind("127.0.0.1:0").expect("l");
    let endereco = antigo.local_addr().expect("a");
    let achado = localizar_com(&DeviceId("velho".into()), Some(endereco), Duration::from_secs(3), opcoes(p, e))
        .expect("o endereço antigo atendeu");
    assert_eq!(achado, Localizado { endpoint: endereco, via: "endereco_antigo", mudou: false, aceita_codigo: false });
}

#[test]
#[ignore = "localizar volta pelo Challenge do v3 na etapa 3 de docs/v3-no-privado.md"]
fn localizar_recusa_o_endereco_antigo_quando_outro_quall_responde_dali() {
    let (p, e) = portas_livres();
    // O DHCP deu o IP antigo a outro aparelho com Quall: o TCP atende, mas o anúncio dali é de outro.
    let antigo = TcpListener::bind("127.0.0.1:0").expect("l");
    let endereco = antigo.local_addr().expect("a");
    let outro = anuncio_de("outro-aparelho", "Outro");
    let _r = Respondedor::iniciar_em(&outro, endereco.port(), false, TOKEN, p, e);
    let busca_previa = Busca::com(OpcoesDaBusca { sondar: false, ..opcoes(p, e) });
    esperar(Duration::from_secs(3), || busca_previa.aparelho(&outro.device_id)).expect("o outro aparece");
    drop(busca_previa);
    let r = localizar_com(&DeviceId("sumido".into()), Some(endereco), Duration::from_millis(2500), opcoes(p, e));
    assert!(matches!(r, Err(Error::Timeout(_))), "{r:?}");
}

#[test]
#[ignore = "localizar volta pelo Challenge do v3 na etapa 3 de docs/v3-no-privado.md"]
fn localizar_sem_resposta_nenhuma_esgota_o_prazo_com_motivo() {
    let (p, e) = portas_livres();
    let r = localizar_com(&DeviceId("ninguem".into()), None, Duration::from_millis(300), opcoes(p, e));
    let Err(Error::Timeout(m)) = r else { panic!("{r:?}") };
    assert!(m.contains("ninguem"), "{m}");
}

// ---- M4 e B3 da revisão de 12/10 ----

fn rede(a: u8, b: u8, c: u8, d: u8, prefixo: u8) -> RedeLocal {
    RedeLocal { ip: Ipv4Addr::new(a, b, c, d), prefixo }
}

/// O Dell tem `vEthernet (WSL (Hyper-V firewall))` antes da placa de verdade na enumeração.
#[test]
fn a_rede_da_rota_padrao_vem_antes_das_virtuais() {
    let candidatas = vec![
        ("vEthernet (WSL (Hyper-V firewall))".to_string(), rede(172, 23, 16, 1, 20)),
        ("vEthernet (Default Switch)".to_string(), rede(172, 30, 0, 1, 20)),
        ("Ethernet".to_string(), rede(192, 168, 15, 8, 24)),
        ("Wi-Fi".to_string(), rede(192, 168, 15, 6, 24)),
    ];
    let escolhidas = escolher_redes(candidatas.clone(), Some(Ipv4Addr::new(192, 168, 15, 8)));
    assert_eq!(escolhidas, vec![rede(192, 168, 15, 8, 24)], "a /24 da LAN uma vez, e as virtuais de fora");

    // Sem rota padrão: a real ainda vence, e as virtuais continuam de fora.
    let sem_rota = escolher_redes(candidatas, None);
    assert_eq!(sem_rota, vec![rede(192, 168, 15, 8, 24)]);

    // Só virtuais (a máquina sem LAN): melhor que nada.
    let so_virtuais = escolher_redes(
        vec![("vEthernet (WSL)".to_string(), rede(172, 23, 16, 1, 20))],
        None,
    );
    assert_eq!(so_virtuais, vec![rede(172, 23, 16, 1, 20)]);
}

#[test]
fn virtualbox_docker_e_vpn_ficam_para_depois() {
    let candidatas = vec![
        ("docker0".to_string(), rede(172, 17, 0, 1, 16)),
        ("vboxnet0".to_string(), rede(192, 168, 56, 1, 24)),
        ("en0".to_string(), rede(10, 0, 0, 23, 24)),
        ("utun4".to_string(), rede(10, 8, 0, 2, 24)),
        ("lo0".to_string(), rede(127, 0, 0, 1, 8)),
        ("publica".to_string(), rede(8, 8, 8, 8, 24)),
    ];
    let e = escolher_redes(candidatas, None);
    assert_eq!(e[0], rede(10, 0, 0, 23, 24));
    assert!(!e.iter().any(|r| r.ip.is_loopback() || r.ip == Ipv4Addr::new(8, 8, 8, 8)));
}

fn anuncio_udp_de(id: &str) -> Lido {
    Lido::Anuncio { anuncio: anuncio_de(id, id), porta: 7877, aceita_codigo: false, nome: None, r: None }
}

/// B3: quem manda mil ids do mesmo IP não enche a lista; e a lista tem teto.
#[test]
fn a_lista_tem_teto_e_limite_por_ip() {
    let tabela = Mutex::new(Tabela::default());
    let um_ip = SocketAddr::from(([192, 168, 15, 66], 7875));
    // Na mesma porta, cada token novo substitui o anterior (B5): fica um só.
    for i in 0..100 {
        anotar_udp(&tabela, anuncio_udp_de(&format!("mesma-porta-{i}")), um_ip, "udp");
    }
    assert_eq!(travar(&tabela).entradas.len(), 1);
    travar(&tabela).entradas.clear();
    // Em portas diferentes, o teto por IP.
    let em_porta =
        |id: &str, porta: u16| Lido::Anuncio { anuncio: anuncio_de(id, id), porta, aceita_codigo: false, nome: None, r: None };
    for i in 0..100u16 {
        anotar_udp(&tabela, em_porta(&format!("forjado-{i}"), 7000 + i), um_ip, "udp");
    }
    assert_eq!(travar(&tabela).entradas.len(), APARELHOS_POR_IP);
    // Quem já está continua sendo atualizado.
    anotar_udp(&tabela, em_porta("forjado-0", 7000), um_ip, "udp");
    assert!(travar(&tabela).entradas.values().any(|e| e.anuncio.device_id.0 == "forjado-0"));

    for i in 0..300u32 {
        let ip = SocketAddr::from(([10, 0, (i / 250) as u8, (i % 250) as u8 + 1], 7875));
        anotar_udp(&tabela, anuncio_udp_de(&format!("muitos-{i}")), ip, "udp");
    }
    assert_eq!(travar(&tabela).entradas.len(), TETO_DE_APARELHOS);
}

/// B5: a chave é token + IP + porta. O mesmo token vindo de outro IP é outro item (não muda o
/// endereço de quem já está); um token novo no mesmo IP, porta e papel substitui o anterior.
#[test]
fn a_lista_e_por_token_ip_e_porta_e_o_token_novo_substitui_o_velho() {
    let tabela = Mutex::new(Tabela::default());
    let celular = SocketAddr::from(([192, 168, 15, 9], 7875));
    let intruso = SocketAddr::from(([192, 168, 15, 66], 7875));
    anotar_udp(&tabela, anuncio_udp_de("discovery-aaa"), celular, "udp");
    anotar_udp(&tabela, anuncio_udp_de("discovery-aaa"), intruso, "udp");
    {
        let t = travar(&tabela);
        assert_eq!(t.entradas.len(), 2, "o mesmo token de outro IP é outro item");
        let do_celular = t.entradas.get(&chave_da_entrada("discovery-aaa", Some(celular.ip()), 7877)).expect("o do celular");
        assert_eq!(do_celular.udp.map(|(ip, _)| ip), Some(celular.ip()), "o endereço do celular não mudou");
    }
    // O celular recomeçou a espera: token novo, mesmo IP, porta e papel.
    anotar_udp(&tabela, anuncio_udp_de("discovery-bbb"), celular, "udp");
    let t = travar(&tabela);
    assert!(!t.entradas.contains_key(&chave_da_entrada("discovery-aaa", Some(celular.ip()), 7877)), "o velho saiu");
    assert!(t.entradas.contains_key(&chave_da_entrada("discovery-bbb", Some(celular.ip()), 7877)));
    assert!(t.entradas.contains_key(&chave_da_entrada("discovery-aaa", Some(intruso.ip()), 7877)), "o do outro IP fica");
}

/// Um teleprompter e um vídeo no mesmo aparelho (portas diferentes) não se substituem.
#[test]
fn papel_ou_porta_diferente_nao_substitui() {
    let tabela = Mutex::new(Tabela::default());
    let ip = SocketAddr::from(([192, 168, 15, 9], 7875));
    anotar_udp(&tabela, anuncio_udp_de("discovery-video"), ip, "udp");
    let mut prompter = anuncio_de("discovery-prompter", "p");
    prompter.papel = Some(Papel::Teleprompter);
    anotar_udp(
        &tabela,
        Lido::Anuncio { anuncio: prompter, porta: 7979, aceita_codigo: false, nome: None, r: None },
        ip,
        "udp",
    );
    assert_eq!(travar(&tabela).entradas.len(), 2);
}

fn do_mdns(token: &str, fullname: &str, ips: &[&str]) -> DiscoveredDevice {
    DiscoveredDevice {
        announcement: anuncio_de(token, token),
        addresses: ips.iter().map(|e| e.parse::<IpAddr>().expect("ip").into()).collect(),
        signaling_port: 7877,
        fullname: fullname.into(),
        nome_anunciado: None,
    }
}

/// **M-d**: o mesmo aparelho pelo mDNS e pelo UDP é **um** item, venha qual vier primeiro; e
/// quando o melhor endereço do mDNS muda, o item é o mesmo (a chave é a instância).
#[test]
fn mdns_e_udp_do_mesmo_aparelho_sao_um_item_e_o_endereco_novo_nao_duplica() {
    for udp_primeiro in [false, true] {
        let tabela = Mutex::new(Tabela::default());
        let celular = SocketAddr::from(([192, 168, 15, 9], 7875));
        if udp_primeiro {
            anotar_udp(&tabela, anuncio_udp_de("discovery-ccc"), celular, "udp");
        }
        anotar_mdns(&tabela, do_mdns("discovery-ccc", "Quall ccc._quall._tcp.local.", &["192.168.15.9", "fe80::1"]), false);
        if !udp_primeiro {
            anotar_udp(&tabela, anuncio_udp_de("discovery-ccc"), celular, "udp");
        }
        {
            let t = travar(&tabela);
            assert_eq!(t.entradas.len(), 1, "um aparelho, um item (UDP primeiro: {udp_primeiro})");
            let e = t.entradas.values().next().expect("item");
            assert!(e.via.contains("mdns") && e.via.contains("udp"));
        }
        // O DHCP trocou o IP: o mDNS revê a mesma instância noutro endereço.
        anotar_mdns(&tabela, do_mdns("discovery-ccc", "Quall ccc._quall._tcp.local.", &["192.168.15.40"]), false);
        assert_eq!(travar(&tabela).entradas.len(), 1, "a chave velha não fica para trás");
    }
}

/// **Baixo 6**: com a lista cheia, um forjado no IP de um aparelho de verdade não o tira; e
/// `Busca::aparelho` com o mesmo token em dois endereços devolve o que entrou primeiro.
#[test]
fn lista_cheia_nao_deixa_forjado_tirar_o_verdadeiro_e_o_primeiro_token_vale() {
    let tabela = Mutex::new(Tabela::default());
    let verdadeiro = SocketAddr::from(([192, 168, 15, 9], 7875));
    anotar_udp(&tabela, anuncio_udp_de("discovery-real"), verdadeiro, "udp");
    for i in 0..(TETO_DE_APARELHOS as u32 + 5) {
        let ip = SocketAddr::from(([10, 1, (i / 250) as u8, (i % 250) as u8 + 1], 7875));
        anotar_udp(&tabela, anuncio_udp_de(&format!("discovery-enche-{i}")), ip, "udp");
    }
    assert_eq!(travar(&tabela).entradas.len(), TETO_DE_APARELHOS);
    anotar_udp(&tabela, anuncio_udp_de("discovery-forjado"), verdadeiro, "udp");
    assert!(
        travar(&tabela).entradas.values().any(|e| e.anuncio.device_id.0 == "discovery-real"),
        "o forjado tirou o verdadeiro com a lista cheia"
    );

    // O mesmo token em dois endereços: vale o primeiro.
    let busca_tabela = Arc::new(Mutex::new(Tabela::default()));
    anotar_udp(&busca_tabela, anuncio_udp_de("discovery-dup"), verdadeiro, "udp");
    std::thread::sleep(Duration::from_millis(5));
    anotar_udp(&busca_tabela, anuncio_udp_de("discovery-dup"), SocketAddr::from(([192, 168, 15, 66], 7875)), "udp");
    let busca = Busca { tabela: busca_tabela, parar: Arc::new(AtomicBool::new(true)), thread: None, dica: None, varredura: Varredura::nova(), numero: 0 };
    let achado = busca.aparelho(&DeviceId("discovery-dup".into())).expect("achado");
    assert_eq!(achado.endpoint.map(|e| e.ip()), Some(verdadeiro.ip()));
}

/// O teto por IP vale também para o mDNS.
#[test]
fn o_teto_por_ip_vale_para_o_mdns() {
    let tabela = Mutex::new(Tabela::default());
    for i in 0..10u16 {
        let mut d = do_mdns(&format!("discovery-m{i}"), &format!("Quall m{i}._quall._tcp.local."), &["192.168.15.77"]);
        d.signaling_port = 7000 + i;
        anotar_mdns(&tabela, d, false);
    }
    assert_eq!(travar(&tabela).entradas.len(), APARELHOS_POR_IP);
}


/// **A varredura não se repete em laço** (`docs/v3-no-privado.md` §8.9): o aparelho que abre depois
/// dela só entra pela sondagem quando a pessoa toca "Procurar de novo" — o caminho do caso de 13/10
/// (a lista já tinha um, e o outro só a sondagem acharia).
#[test]
fn a_varredura_nao_se_repete_e_procurar_de_novo_acha_o_que_abriu_depois() {
    let (p, e) = portas_livres();
    let a = anuncio_de("cam-primeira", "Primeira");
    let _r1 = Respondedor::iniciar_em(&a, 7901, false, TOKEN, p, e);
    let busca = Busca::com(OpcoesDaBusca { broadcast: false, ..opcoes(p, e) });
    esperar(Duration::from_secs(6), || busca.aparelho(&visto(TOKEN))).expect("a varredura achou");
    assert_eq!(busca.diagnostico().sondagens, 1);
    // O segundo aparelho abre depois: sem o toque, nada o pergunta.
    let outro = "fedcba9876543210fedcba9876543210";
    let b = anuncio_de("cam-segunda", "Segunda");
    let _r2 = Respondedor::iniciar_em(&b, 7902, false, outro, p, e);
    std::thread::sleep(Duration::from_secs(3));
    assert!(busca.aparelho(&visto(outro)).is_none(), "a varredura se repetiu sozinha");
    busca.procurar_de_novo();
    let achado = esperar(Duration::from_secs(3), || busca.aparelho(&visto(outro))).expect("procurar de novo");
    assert_eq!(achado.via, vec!["sondagem"]);
    assert_eq!(busca.diagnostico().sondagens, 2);
}

/// A dica é perguntada aos 1 s, sem varredura nenhuma (a lista não fica vazia: o alvo é outro).
#[test]
fn a_dica_e_perguntada_sem_varredura() {
    let (p, e) = portas_livres();
    let a = anuncio_de("cam-dica", "Dica");
    let _r = Respondedor::iniciar_em(&a, 7903, false, TOKEN, p, e);
    let busca = Busca::com(OpcoesDaBusca { broadcast: false, varredura: Varredura::nova(), ..opcoes(p, e) });
    busca.dar_dica(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let inicio = Instant::now();
    let achado = esperar(Duration::from_secs(3), || busca.aparelho(&visto(TOKEN))).expect("a dica");
    assert!(inicio.elapsed() < SONDAR_DEPOIS_DE, "veio pela dica, antes da varredura");
    assert_eq!(achado.via, vec!["sondagem"]);
    assert_eq!(busca.diagnostico().sondagens, 0);
}

#[test]
fn falta_alguem_sem_alvo_e_so_os_proprios_com_alvo_e_ele() {
    let mut item = AparelhoVisto {
        anuncio: anuncio_de("discovery-x", "Quall x"),
        endpoint: None,
        via: vec!["mdns"],
        visto_ha: Duration::ZERO,
        vivo: true,
        aceita_codigo: false,
        proprio: true,
        conhecido: None,
        chave: "k".into(),
        nome_anunciado: None,
    };
    assert!(falta_alguem(&[], None));
    assert!(falta_alguem(std::slice::from_ref(&item), None), "só o próprio é lista vazia");
    item.proprio = false;
    assert!(!falta_alguem(std::slice::from_ref(&item), None));
    assert!(falta_alguem(std::slice::from_ref(&item), Some(&DeviceId("outro".into()))));
    assert!(!falta_alguem(std::slice::from_ref(&item), Some(&DeviceId("discovery-x".into()))));
}

#[test]
fn os_vizinhos_ficam_na_rede_sem_o_proprio_rede_e_broadcast() {
    let r = rede(192, 168, 15, 9, 24);
    let crus = vec![
        Ipv4Addr::new(192, 168, 15, 1),
        Ipv4Addr::new(192, 168, 15, 9),
        Ipv4Addr::new(192, 168, 15, 0),
        Ipv4Addr::new(192, 168, 15, 255),
        Ipv4Addr::new(10, 0, 0, 2),
        Ipv4Addr::new(224, 0, 0, 251),
        Ipv4Addr::new(192, 168, 15, 8),
        Ipv4Addr::new(192, 168, 15, 1),
    ];
    assert_eq!(
        filtrar_vizinhos(crus, &[r]),
        vec![Ipv4Addr::new(192, 168, 15, 1), Ipv4Addr::new(192, 168, 15, 8)]
    );
}

#[test]
fn o_diario_diz_quem_entrou_saiu_e_por_onde_sem_nome_pessoal() {
    let item = |token: &str, ip: &str, via: Vec<&'static str>| AparelhoVisto {
        anuncio: Announcement {
            display_name: "iPhone do Pessoa".into(),
            ..anuncio_de(&format!("discovery-{token}"), "x")
        },
        endpoint: Some(ip.parse().expect("e")),
        via,
        visto_ha: Duration::ZERO,
        vivo: true,
        aceita_codigo: false,
        proprio: false,
        conhecido: None,
        chave: format!("{token}|{ip}"),
        nome_anunciado: Some("iPhone do Pessoa".into()),
    };
    let a = item(TOKEN, "192.168.15.7:7877", vec!["mdns"]);
    let b = item("fedcba9876543210fedcba9876543210", "192.168.15.9:7877", vec!["udp", "mdns"]);
    let entrou = mudancas_da_lista(std::slice::from_ref(&a), &[a.clone(), b.clone()]);
    assert_eq!(entrou, vec!["entrou Quall fedcba98 em 192.168.15.9:7877 por mdns+udp".to_string()]);
    let mut a2 = a.clone();
    a2.via = vec!["mdns", "sondagem"];
    assert_eq!(mudancas_da_lista(std::slice::from_ref(&a), &[a2]), vec!["Quall 01234567 agora em 192.168.15.7:7877 por mdns+sondagem".to_string()]);
    let saiu = mudancas_da_lista(&[a.clone(), b], std::slice::from_ref(&a));
    assert_eq!(saiu, vec!["saiu Quall fedcba98 (era 192.168.15.9:7877 por mdns+udp)".to_string()]);
    assert!(mudancas_da_lista(std::slice::from_ref(&a), std::slice::from_ref(&a)).is_empty());
    for l in entrou.iter().chain(&saiu) {
        assert!(!l.contains("Pessoa"), "nome pessoal no diário: {l}");
    }
}
