//! Os testes do QR invertido sem sessão: o texto, o chamado selado em loopback e os casos de ataque.
//! A sequência completa, com a sinalização e o pareamento, está em `session::testes_do_qr`.

use super::*;

/// Um id por teste: a caixa é do processo, e os testes rodam em paralelo.
fn id(nome: &str) -> DeviceId {
    DeviceId(format!("qr-teste-{nome}"))
}

fn convite(nome: &str) -> Convite {
    Convite::novo_em(&id(nome), "MacBook do Pessoa", Duration::from_secs(60), vec![Ipv4Addr::LOCALHOST], 0)
        .expect("convite em loopback")
}

/// Uma linha de chamado como o celular mandaria, selada para `ip` (a AAD), com o `r` dado.
fn linha_de_chamado_com(qr: &ConteudoDoQr, celular: &str, ip: &str, porta: u16, r: [u8; NONCE_LEN]) -> String {
    let dentro = serde_json::json!({ "id": celular, "n": "A07", "p": porta });
    let (nonce, selado) = selar(&qr.chaves().ida, &aad_do_chamado(ip, &r), dentro.to_string().as_bytes()).unwrap();
    serde_json::json!({
        "q": "quall", "f": 2, "t": "chamado", "r": hex_encode(&r), "ip": ip,
        "nonce": hex_encode(&nonce), "x": hex_encode(&selado),
    })
    .to_string()
}

fn linha_de_chamado(qr: &ConteudoDoQr, celular: &str, ip: &str, porta: u16) -> String {
    linha_de_chamado_com(qr, celular, ip, porta, sortear().unwrap())
}

fn ok_da_resposta(resposta: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(resposta).unwrap()["ok"] == true
}

fn motivo(resposta: &str) -> String {
    serde_json::from_str::<serde_json::Value>(resposta).unwrap()["motivo"].as_str().unwrap_or("").into()
}

// ---- o texto ----

#[test]
fn o_texto_vai_e_volta_e_o_debug_nao_mostra_o_segredo() {
    let c = convite("texto");
    let texto = c.texto();
    assert!(texto.starts_with("quall://chamar?v=2&a=127.0.0.1&p="), "{texto}");
    assert!(texto.contains("&n=MacBook%20do%20Pessoa&"), "{texto}");
    let lido = ConteudoDoQr::ler(&texto).unwrap();
    assert_eq!(lido, *c.conteudo());
    let hex = hex_encode(&lido.segredo);
    assert!(texto.ends_with(&hex));
    assert!(!format!("{lido:?}").contains(&hex));
    assert!(texto.len() < 200, "o QR tem de caber em versão pequena: {} bytes", texto.len());
}

#[test]
fn chave_desconhecida_e_ignorada_e_o_fragmento_cai() {
    let c = convite("desconhecida");
    let texto = c.texto().replacen("?v=2&", "?zz=9&v=2&", 1) + "#lixo";
    assert_eq!(ConteudoDoQr::ler(&texto).unwrap(), *c.conteudo());
}

#[test]
fn texto_quebrado_e_recusado_inteiro() {
    let bom = convite("quebrado").texto();
    let casos = [
        ("quall://123456@192.168.0.2:7877", "não é um código"),
        ("https://exemplo.com/?v=2", "não é um código"),
        (&*bom.replacen("v=2", "v=3", 1), "atualize o app"),
        // O v1 de 12/10 chamava em claro: recusado, com o porquê (P5).
        (&*bom.replacen("v=2", "v=1", 1), "versão anterior"),
        (&*bom.replacen("v=2", "v=0", 1), "versão 0"),
        (&*bom.replacen("&p=", "&v=2&p=", 1), "repetido"),
        (&*bom.replacen("a=127.0.0.1", "a=8.8.8.8", 1), "rede local"),
        (&*bom.replacen("a=127.0.0.1", "a=1.1.1.1,2.2.2.2", 1), "rede local"),
        (&*bom.replacen("a=127.0.0.1", "a=10.0.0.1,10.0.0.2,10.0.0.3,10.0.0.4,10.0.0.5", 1), "1 a 4"),
        (&*bom.replacen("&p=", "&p=0&x=", 1), "porta"),
        (&*bom.replacen("&s=", "&s=ab&x=", 1), "segredo"),
        (&*bom.replacen("&i=", "&x=", 1), "sem o id"),
    ];
    for (texto, pedaco) in casos {
        let e = ConteudoDoQr::ler(texto).expect_err(texto).to_string();
        assert!(e.contains(pedaco), "{texto}: {e}");
    }
}

#[test]
fn um_qr_quebrado_numa_build_antiga_nao_vira_endereco() {
    // `ler_destino` é quem lê o link colado nas builds de antes; o texto novo cai em recusa limpa.
    let texto = convite("antiga").texto();
    assert!(crate::discovery::ler_destino(&texto, 7877).is_err());
}

// ---- o chamado em loopback ----

#[test]
fn o_chamado_em_loopback_vale_e_a_chave_vai_para_a_caixa() {
    let c = convite("loopback");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let celular = id("loopback-celular");
    let r = chamar(&lido, &celular, "Galaxy A07", 7877, Duration::from_secs(3)).unwrap();
    match r {
        ResultadoDoChamado::Aceito { par, nome, via } => {
            assert_eq!(par, lido.id);
            assert_eq!(nome, "MacBook do Pessoa");
            assert_eq!(via.port(), lido.porta);
        }
        outro => panic!("esperava aceite, veio {outro:?}"),
    }
    let chamado = c.esperar_chamado(&Cancelamento::novo()).unwrap();
    assert_eq!(chamado.endpoint, SocketAddr::from((Ipv4Addr::LOCALHOST, 7877)));
    assert_eq!(chamado.par, celular);
    assert_eq!(chamado.nome, "Galaxy A07");
    assert_eq!(c.retrato().fase, "chamado");
    // O celular guardou a mesma chave de pareamento que o desktop tem.
    assert!(lidos_de(IpAddr::V4(Ipv4Addr::LOCALHOST)).iter().any(|(i, k)| *i == lido.id && *k == c.chave_do_par()));
    assert_eq!(tomar(&lido.id), Some(c.chave_do_par()));
    concluir(&lido.id);
    assert_eq!(tomar(&lido.id), None);
}

/// **Sentinela**: a linha do chamado não leva o id nem o nome do celular em claro.
#[test]
fn o_chamado_nao_leva_id_nem_nome_em_claro() {
    let c = convite("sentinela");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let linha = linha_de_chamado(&lido, "ID-SENTINELA-CELULAR", "127.0.0.1", 7877);
    assert!(!linha.contains("SENTINELA") && !linha.contains("A07"), "{linha}");
    let v: serde_json::Value = serde_json::from_str(&linha).unwrap();
    assert!(v.get("id").is_none() && v.get("n").is_none());
}

#[test]
fn segredo_errado_e_recusado_e_o_convite_continua_esperando() {
    let c = convite("errado");
    let mut falso = ConteudoDoQr::ler(&c.texto()).unwrap();
    falso.segredo[0] ^= 1;
    let r = chamar(&falso, &id("errado-celular"), "A07", 7877, Duration::from_secs(3)).unwrap();
    assert_eq!(r, ResultadoDoChamado::Recusado(MotivoDaRecusa::Invalido));
    assert_eq!(c.retrato().fase, "esperando");
    // A chave do QR falso não fica na caixa.
    assert_eq!(tomar(&falso.id), None);
}

#[test]
fn o_convite_e_de_uso_unico() {
    let c = convite("unico");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let primeiro = chamar(&lido, &id("unico-1"), "A07", 7877, Duration::from_secs(3)).unwrap();
    assert!(matches!(primeiro, ResultadoDoChamado::Aceito { .. }));
    // Outro celular que leu o mesmo QR (ou o mesmo, de novo) ouve "usado", e o chamado que valeu não muda.
    // Depois do intervalo por IP: em loopback os dois "celulares" têm o mesmo endereço.
    std::thread::sleep(INTERVALO_POR_IP + Duration::from_millis(50));
    let segundo = chamar(&lido, &id("unico-2"), "S24", 7999, Duration::from_secs(3)).unwrap();
    assert_eq!(segundo, ResultadoDoChamado::Recusado(MotivoDaRecusa::Usado));
    // O "usado" do segundo chamado (o leitor que disparou duas vezes) não tira da caixa a chave que o
    // primeiro deixou aceita: o desktop vem conectar com ela.
    assert_eq!(tomar(&lido.id), Some(c.chave_do_par()));
    concluir(&lido.id);
    let chamado = c.esperar_chamado(&Cancelamento::novo()).unwrap();
    assert_eq!(chamado.par, id("unico-1"));
    assert_eq!(chamado.endpoint.port(), 7877);
}

/// **M3**: o mesmo `r` não abre duas vezes enquanto o convite vale — nem com outro selo.
#[test]
fn o_mesmo_nonce_do_chamado_e_recusado() {
    let c = Convite::novo_em(&id("nonce"), "Mac", Duration::from_secs(60), vec![Ipv4Addr::LOCALHOST], 0).unwrap();
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let ip: IpAddr = "192.168.15.20".parse().unwrap();
    let r = [5u8; NONCE_LEN];
    // O chamado que abre gasta o `r`.
    let primeira = linha_de_chamado_com(&lido, "cel", "192.168.15.20", 7877, r);
    assert!(ok_da_resposta(&decidir_chamado(&c.comum, ip, &primeira, Instant::now())));
    // O reenvio da mesma linha, e uma linha nova com o mesmo `r`, são recusados como inválidos.
    assert_eq!(motivo(&decidir_chamado(&c.comum, ip, &primeira, Instant::now())), "invalido");
    let outra = linha_de_chamado_com(&lido, "cel", "192.168.15.20", 7877, r);
    assert_eq!(motivo(&decidir_chamado(&c.comum, ip, &outra, Instant::now())), "invalido");
}

/// **M3**: o aceite usa a chave da volta, não a do chamado — e o nonce do selo é sorteado.
#[test]
fn chamado_e_resposta_nao_dividem_chave_nem_nonce() {
    let k = chaves(&[2u8; SEGREDO_LEN], &DeviceId("mac".into()));
    assert_ne!(k.ida, k.volta);
    assert_ne!(k.ida, k.par);
    assert_ne!(k.volta, k.par);
    let (n1, _) = selar(&k.ida, b"aad", b"x").unwrap();
    let (n2, _) = selar(&k.ida, b"aad", b"x").unwrap();
    assert_ne!(n1, n2, "nonce sorteado a cada selo");
    // O selo do chamado não abre com a chave da volta.
    let (n, s) = selar(&k.ida, b"aad", b"x").unwrap();
    assert!(abrir(&k.volta, &n, b"aad", &s).is_none());
    assert_eq!(abrir(&k.ida, &n, b"aad", &s).as_deref(), Some(&b"x"[..]));
}

#[test]
fn o_convite_expira() {
    let c = Convite::novo_em(&id("expira"), "Mac", Duration::from_millis(80), vec![Ipv4Addr::LOCALHOST], 0)
        .unwrap();
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    // Pela decisão, com o relógio adiante: "expirou", mesmo com o selo certo.
    let linha = linha_de_chamado(&lido, "expira-celular", "127.0.0.1", 7877);
    let resposta = decidir_chamado(&c.comum, IpAddr::V4(Ipv4Addr::LOCALHOST), &linha, Instant::now() + Duration::from_secs(1));
    assert_eq!(motivo(&resposta), "expirou");
    // E de verdade: a espera volta com Timeout, e a escuta fecha.
    let e = c.esperar_chamado(&Cancelamento::novo()).unwrap_err();
    assert!(matches!(e, Error::Timeout(_)), "{e:?}");
    assert_eq!(c.retrato().fase, "expirou");
    std::thread::sleep(Duration::from_millis(150));
    let r = chamar(&lido, &id("expira-celular"), "A07", 7877, Duration::from_secs(2));
    assert!(matches!(r, Err(Error::NoRoute(_))), "{r:?}");
}

#[test]
fn chamado_de_origem_fora_da_lan_e_recusado_mesmo_com_o_selo_certo() {
    let c = convite("fora");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    for ip in ["8.8.8.8", "100.64.0.1", "2001:db8::1"] {
        let origem: IpAddr = ip.parse().unwrap();
        let linha = linha_de_chamado(&lido, "fora-celular", &ip_canonico(origem), 7877);
        let resposta = decidir_chamado(&c.comum, origem, &linha, Instant::now());
        assert!(!ok_da_resposta(&resposta), "{ip}: {resposta}");
        assert_eq!(motivo(&resposta), "invalido");
    }
    assert_eq!(c.retrato().fase, "esperando");
}

#[test]
fn chamado_reenviado_de_outro_ip_nao_abre() {
    // A AAD prende o IP de quem chamou: quem copia a linha e a manda de outra máquina não faz o
    // desktop conectar em lugar nenhum. Sem o campo `ip` (que diria "endereco"), o selo é que recusa.
    let c = convite("outro-ip");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&linha_de_chamado(&lido, "celular", "192.168.15.20", 7877)).unwrap();
    v.as_object_mut().unwrap().remove("ip");
    let linha = v.to_string();
    let atacante: IpAddr = "192.168.15.66".parse().unwrap();
    assert_eq!(motivo(&decidir_chamado(&c.comum, atacante, &linha, Instant::now())), "invalido");
    // A mesma linha, da origem certa, vale — e conecta **nela**, não onde o atacante quisesse.
    let certa: IpAddr = "192.168.15.20".parse().unwrap();
    assert!(ok_da_resposta(&decidir_chamado(&c.comum, certa, &linha, Instant::now())));
    assert_eq!(c.retrato().chamado.unwrap().endpoint, SocketAddr::new(certa, 7877));
}

/// **B3**: o celular atrás de VPN (ou NAT) diz um IP e o desktop vê outro: a recusa diz o porquê.
/// E o IPv4 mapeado em IPv6 é o mesmo endereço.
#[test]
fn ip_diferente_do_visto_e_endereco_e_o_mapeado_em_ipv6_vale() {
    let c = convite("vpn");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let linha = linha_de_chamado(&lido, "cel", "10.8.0.2", 7877);
    let visto: IpAddr = "192.168.15.20".parse().unwrap();
    assert_eq!(motivo(&decidir_chamado(&c.comum, visto, &linha, Instant::now())), "endereco");
    let linha = linha_de_chamado(&lido, "cel", "192.168.15.20", 7877);
    let mapeado: IpAddr = "::ffff:192.168.15.20".parse().unwrap();
    assert!(ok_da_resposta(&decidir_chamado(&c.comum, mapeado, &linha, Instant::now())));
    assert_eq!(c.retrato().chamado.unwrap().endpoint.ip(), visto);
}

#[test]
fn mexer_em_qualquer_campo_do_chamado_derruba_o_selo() {
    let c = convite("campos");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let ip: IpAddr = "192.168.15.20".parse().unwrap();
    let boa: serde_json::Value = serde_json::from_str(&linha_de_chamado(&lido, "cel", "192.168.15.20", 7877)).unwrap();
    let mut x_adulterado = boa["x"].as_str().unwrap().to_string();
    let primeiro = if x_adulterado.starts_with('0') { "1" } else { "0" };
    x_adulterado.replace_range(0..1, primeiro);
    for (campo, valor) in [
        ("x", serde_json::json!(x_adulterado)),
        ("nonce", serde_json::json!(hex_encode(&[9u8; NONCE_AEAD_LEN]))),
        ("r", serde_json::json!(hex_encode(&[8u8; NONCE_LEN]))),
        ("f", serde_json::json!(1)),
        ("t", serde_json::json!("outra")),
    ] {
        let mut v = boa.clone();
        v[campo] = valor;
        let resposta = decidir_chamado(&c.comum, ip, &v.to_string(), Instant::now());
        assert_eq!(motivo(&resposta), "invalido", "{campo}");
    }
    assert_eq!(motivo(&decidir_chamado(&c.comum, ip, "não é json", Instant::now())), "invalido");
    assert_eq!(c.retrato().fase, "esperando");
}

#[test]
fn resposta_de_aceite_forjada_nao_engana_o_celular() {
    // Um falso desktop no endereço do QR responde "ok" sem saber o segredo — com selo de outra chave.
    let escuta = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let porta = escuta.local_addr().unwrap().port();
    let falso = std::thread::spawn(move || {
        let (mut s, _) = escuta.accept().unwrap();
        let _ = ler_linha(&mut s, Instant::now() + Duration::from_secs(2));
        let (nonce, selado) = selar(&[7u8; 32], b"qualquer", br#"{"ok":true}"#).unwrap();
        let mut m = base("resposta");
        m.insert("ok".into(), true.into());
        m.insert("nonce".into(), hex_encode(&nonce).into());
        m.insert("x".into(), hex_encode(&selado).into());
        s.write_all(format!("{}\n", serde_json::Value::Object(m)).as_bytes()).unwrap();
    });
    let qr = ConteudoDoQr {
        enderecos: vec![Ipv4Addr::LOCALHOST],
        porta,
        id: id("forjada"),
        nome: String::new(),
        segredo: [3u8; SEGREDO_LEN],
    };
    let r = chamar(&qr, &id("forjada-celular"), "A07", 7877, Duration::from_secs(3));
    assert!(matches!(r, Err(Error::Pairing(_))), "{r:?}");
    assert_eq!(tomar(&qr.id), None, "a chave sai da caixa quando o chamado não vale");
    falso.join().unwrap();
}

#[test]
fn ninguem_no_endereco_e_sem_rota() {
    let escuta = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let porta = escuta.local_addr().unwrap().port();
    drop(escuta);
    let qr = ConteudoDoQr {
        enderecos: vec![Ipv4Addr::LOCALHOST],
        porta,
        id: id("ninguem"),
        nome: String::new(),
        segredo: [4u8; SEGREDO_LEN],
    };
    let r = chamar(&qr, &id("ninguem-celular"), "A07", 7877, Duration::from_secs(2));
    assert!(matches!(r, Err(Error::NoRoute(_))), "{r:?}");
    assert_eq!(tomar(&qr.id), None);
}

#[test]
fn esperar_o_chamado_obedece_ao_cancelar() {
    let c = convite("cancelar");
    let cancelar = Cancelamento::novo();
    let outro = cancelar.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        outro.cancelar();
    });
    let inicio = Instant::now();
    assert!(matches!(c.esperar_chamado(&cancelar), Err(Error::Cancelled)));
    assert!(inicio.elapsed() < Duration::from_secs(2));
    t.join().unwrap();
}

// ---- limites ----

#[test]
fn o_limite_segura_o_mesmo_ip_e_o_total() {
    let mut l = Limite::default();
    let t0 = Instant::now();
    let a: IpAddr = "192.168.15.20".parse().unwrap();
    assert!(l.pode(a, t0));
    assert!(!l.pode(a, t0 + Duration::from_millis(100)));
    assert!(l.pode(a, t0 + Duration::from_millis(300)));
    let mut l = Limite::default();
    let aceitos = (0..40u8)
        .filter(|i| l.pode(IpAddr::V4(Ipv4Addr::new(10, 0, 0, *i)), t0))
        .count();
    assert_eq!(aceitos, CHAMADOS_POR_SEGUNDO as usize);
}

#[test]
fn a_caixa_da_tres_tentativas_e_vence() {
    let desktop = id("caixa");
    guardar(&desktop, [9u8; CHAVE_DO_QR_LEN], &[Ipv4Addr::LOCALHOST]);
    marcar_aceito(&desktop);
    for _ in 0..TENTATIVAS_DO_LIDO {
        assert!(lidos_de(IpAddr::V4(Ipv4Addr::LOCALHOST)).iter().any(|(i, _)| *i == desktop));
        assert_eq!(tomar(&desktop), Some([9u8; CHAVE_DO_QR_LEN]));
    }
    assert!(!lidos_de(IpAddr::V4(Ipv4Addr::LOCALHOST)).iter().any(|(i, _)| *i == desktop), "sem tentativas, sai dos lidos");
    assert_eq!(tomar(&desktop), None);
    // Ler de novo o QR (o mesmo desktop) recomeça a contagem.
    guardar(&desktop, [8u8; CHAVE_DO_QR_LEN], &[Ipv4Addr::LOCALHOST]);
    assert_eq!(tomar(&desktop), Some([8u8; CHAVE_DO_QR_LEN]));
    concluir(&desktop);
    assert_eq!(tomar(&desktop), None);
}

#[test]
fn as_chaves_sao_diferentes_e_presas_ao_id() {
    let s = [1u8; SEGREDO_LEN];
    let a = chaves(&s, &DeviceId("mac-a".into()));
    let b = chaves(&s, &DeviceId("mac-b".into()));
    assert_ne!(a.ida, a.par);
    assert_ne!(a.par, b.par);
    assert_ne!(a.ida, b.ida);
}

// ---- a revisão adversarial do QR (7234e8ac) ----

/// Um gerador simples e determinístico (xorshift), para o fuzz não depender de crate nem de sorte.
struct Sorteio(u64);
impl Sorteio {
    fn proximo(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Texto com ASCII, hex, acentos, emoji, controle e formatação — os que quebravam fatias.
    fn texto(&mut self) -> String {
        const PECAS: &[&str] = &["0", "a", "f", "é", "ç", "😀", "\u{202E}", "\u{200B}", "\n", "\"", "\\", "+", "-", "z", "00", "ff", "%", "{", "}"];
        let n = (self.proximo() % 80) as usize;
        (0..n).map(|_| PECAS[(self.proximo() % PECAS.len() as u64) as usize]).collect()
    }
    fn valor(&mut self) -> serde_json::Value {
        match self.proximo() % 6 {
            0 => serde_json::Value::Null,
            1 => serde_json::json!(self.proximo() % 70_000),
            2 => serde_json::json!(true),
            3 => serde_json::json!([self.texto()]),
            _ => serde_json::json!(self.texto()),
        }
    }
}

/// **A1**: bytes e UTF-8 aleatórios em todos os campos do chamado e da resposta não derrubam
/// ninguém — antes, um `x` com `é` dava pânico (e, em release com `panic = "abort"`, matava o app).
#[test]
fn fuzz_do_chamado_e_da_resposta_sem_panico() {
    let c = convite("fuzz");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let ip: IpAddr = "192.168.15.20".parse().unwrap();
    let boa: serde_json::Value = serde_json::from_str(&linha_de_chamado(&lido, "cel", "192.168.15.20", 7877)).unwrap();
    let mut s = Sorteio(0x9e37_79b9_7f4a_7c15);
    // O caso exato da revisão.
    let exato = r#"{"q":"quall","f":2,"t":"chamado","r":"00000000000000000000000000000000","nonce":"000000000000000000000000000000000000000000000000","x":"aéa"}"#;
    assert_eq!(motivo(&decidir_chamado(&c.comum, ip, exato, Instant::now())), "invalido");
    for _ in 0..3000 {
        let mut v = boa.clone();
        for campo in ["q", "f", "t", "r", "ip", "nonce", "x", "extra"] {
            if s.proximo() % 3 == 0 {
                v[campo] = s.valor();
            }
        }
        let _ = decidir_chamado(&c.comum, ip, &v.to_string(), Instant::now());
        let _ = decidir_chamado(&c.comum, ip, &s.texto(), Instant::now());
        // A resposta, do lado do celular.
        let mut resp = serde_json::json!({"q": "quall", "f": 2, "t": "resposta", "ok": true, "nonce": "00", "x": "00"});
        for campo in ["t", "ok", "motivo", "nonce", "x"] {
            if s.proximo() % 2 == 0 {
                resp[campo] = s.valor();
            }
        }
        let _ = ler_resposta(&resp.to_string(), &lido.chaves(), &[1u8; NONCE_LEN]);
        let _ = ler_resposta(&s.texto(), &lido.chaves(), &[1u8; NONCE_LEN]);
    }
    let resp = r#"{"t":"resposta","ok":true,"nonce":"000000000000000000000000000000000000000000000000","x":"aéa"}"#;
    assert!(matches!(ler_resposta(resp, &lido.chaves(), &[1u8; NONCE_LEN]), Err(Error::Pairing(_))));
}

/// **M1**: no máximo uma conexão em tratamento por IP e um teto no total; a vaga volta ao sair —
/// inclusive quando a thread entra em pânico.
#[test]
fn as_vagas_sao_uma_por_ip_e_voltam_mesmo_num_panico() {
    let vagas = Arc::new(Mutex::new(Vagas::default()));
    let a: IpAddr = "192.168.15.20".parse().unwrap();
    let primeira = tomar_vaga(&vagas, a).expect("vaga");
    assert!(tomar_vaga(&vagas, a).is_none(), "o mesmo IP não pega a segunda vaga");
    drop(primeira);
    let v2 = Arc::clone(&vagas);
    let t = std::thread::spawn(move || {
        let _vaga = tomar_vaga(&v2, a).expect("vaga");
        panic!("a thread do chamado morreu");
    });
    assert!(t.join().is_err());
    assert!(tomar_vaga(&vagas, a).is_some(), "a vaga voltou depois do pânico");
    let muitas: Vec<_> = (0..40u8)
        .filter_map(|i| tomar_vaga(&vagas, IpAddr::V4(Ipv4Addr::new(10, 0, 1, i))))
        .collect();
    assert_eq!(muitas.len(), EM_TRATAMENTO_NO_MAXIMO);
}

/// **M1**: uma conexão muda de um IP não segura a escuta: o chamado legítimo de outro IP passa, e a
/// conexão muda sai no prazo curto da linha.
#[test]
fn uma_conexao_muda_nao_tira_a_vaga_do_chamado_legitimo() {
    let c = convite("mudo");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    // Quatro conexões mudas do mesmo IP (loopback): só a primeira ganha vaga, e por pouco tempo.
    let mudas: Vec<_> = (0..4)
        .filter_map(|_| TcpStream::connect((Ipv4Addr::LOCALHOST, lido.porta)).ok())
        .collect();
    std::thread::sleep(PRAZO_DA_LINHA + Duration::from_millis(400));
    let r = chamar(&lido, &id("mudo-celular"), "A07", 7877, Duration::from_secs(3)).unwrap();
    assert!(matches!(r, ResultadoDoChamado::Aceito { .. }), "{r:?}");
    drop(mudas);
}

/// **M2**: o celular só aceita o pareamento pelo QR de um convite que o desktop aceitou, e só de
/// quem conecta de um dos endereços que o QR deu.
#[test]
fn so_convite_aceito_e_so_dos_enderecos_do_qr() {
    let desktop = id("m2");
    guardar(&desktop, [6u8; CHAVE_DO_QR_LEN], &["192.168.15.9".parse().unwrap()]);
    let do_desktop: IpAddr = "192.168.15.9".parse().unwrap();
    assert!(lidos_de(do_desktop).iter().all(|(i, _)| *i != desktop), "sem aceite não vale");
    marcar_aceito(&desktop);
    assert!(lidos_de(do_desktop).iter().any(|(i, _)| *i == desktop));
    assert!(lidos_de("::ffff:192.168.15.9".parse().unwrap()).iter().any(|(i, _)| *i == desktop));
    assert!(lidos_de("192.168.15.66".parse().unwrap()).iter().all(|(i, _)| *i != desktop), "outra origem não vale");
    concluir(&desktop);
}

/// **B1**: o "usado" (o leitor que disparou duas vezes) não tira a chave da caixa.
#[test]
fn usado_nao_esquece_a_chave() {
    let c = convite("b1");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    // O primeiro chamado vale; a chave fica aceita. Um QR igual relido e chamado de novo ouve
    // "usado", e a chave continua lá, aceita.
    assert!(matches!(chamar(&lido, &id("b1-cel"), "A07", 7877, Duration::from_secs(3)).unwrap(), ResultadoDoChamado::Aceito { .. }));
    std::thread::sleep(INTERVALO_POR_IP + Duration::from_millis(50));
    let r = chamar(&lido, &id("b1-cel"), "A07", 7877, Duration::from_secs(3)).unwrap();
    assert_eq!(r, ResultadoDoChamado::Recusado(MotivoDaRecusa::Usado));
    assert!(lidos_de(IpAddr::V4(Ipv4Addr::LOCALHOST)).iter().any(|(i, _)| *i == lido.id));
    concluir(&lido.id);
}

/// **B3**: o celular manda o nome limpo (sem controle nem formatação), e o selado fica abaixo do teto.
#[test]
fn o_chamado_leva_o_nome_limpo() {
    let c = convite("b3");
    let lido = ConteudoDoQr::ler(&c.texto()).unwrap();
    let longo = format!("Galaxy\u{202E}{}", "😀".repeat(40));
    let r = chamar(&lido, &id("b3-cel"), &longo, 7877, Duration::from_secs(3)).unwrap();
    assert!(matches!(r, ResultadoDoChamado::Aceito { .. }));
    let chamado = c.esperar_chamado(&Cancelamento::novo()).unwrap();
    assert!(chamado.nome.starts_with("Galaxy") && !chamado.nome.contains('\u{202E}'), "{:?}", chamado.nome);
    concluir(&lido.id);
}
