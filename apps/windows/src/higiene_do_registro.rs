//! Redação de diagnóstico, compartilhada pelo app e pela DLL da câmera sem dependências Win32.
//!
//! Não substitui escolher campos seguros no chamador. É a última barreira para erros externos e
//! callbacks: não grava PIN/credenciais rotuladas, URI de pareamento, IP ou digest longo. Não há
//! opção de bancada que desligue a redação. A UI de pareamento não passa por este módulo.

use std::net::{IpAddr, SocketAddr};

const OMITIDO: &str = "<omitido>";

/// Mesma barreira no stderr das sondas, que são crates separados da biblioteca.
#[macro_export]
macro_rules! diagnostico_eprintln {
    () => { std::eprintln!() };
    ($($t:tt)*) => { std::eprintln!("{}", $crate::higiene_do_registro::sanitizar(&format!($($t)*))) };
}

/// Uma linha segura para arquivo, stderr ou relato diagnóstico; neutraliza linhas injetadas.
pub fn sanitizar(texto: &str) -> String {
    // Redigir antes de dividir: uma credencial entre aspas pode conter quebras de linha.
    tokens(&campos(texto)).lines().collect::<Vec<_>>().join(" | ")
}

/// Localização útil sem o payload arbitrário do panic nem diretórios do build/usuário.
pub fn resumo_panico(falha: &std::panic::PanicHookInfo<'_>) -> String {
    falha.location().map(|local| {
        let arquivo = local.file().rsplit(['/', '\\']).next().unwrap_or("?");
        format!("panic local={arquivo}:{}:{}", local.line(), local.column())
    }).unwrap_or_else(|| "panic local=desconhecido".into())
}

/// Só para executáveis próprios. Uma DLL não deve substituir o hook global do consumidor.
pub fn instalar_hook_do_executavel() {
    std::panic::set_hook(Box::new(|falha| std::eprintln!("falha inesperada: {}", resumo_panico(falha))));
}

pub fn campo_sensivel(campo: &str) -> bool {
    matches!(campo.to_ascii_lowercase().as_str(),
        "pin" | "pin_texto" | "pin_fixo" | "pin_da_camera" | "password" | "passwd"
        | "senha" | "secret" | "shared_secret" | "pairing_secret" | "shared_key"
        | "pairing_key" | "private_key" | "key" | "token" | "authorization"
        | "credential" | "credentials" | "ice-pwd" | "ice-ufrag" | "ice_pwd"
        | "ice_ufrag" | "ufrag" | "pwd" | "device_id" | "display_name" | "nome"
        | "par" | "peer" | "destino" | "endereco" | "endereço" | "hostname"
        | "local_address" | "remote_address" | "local_candidate" | "remote_candidate"
        | "prompter_nome" | "emissor"
        | "camera" | "link" | "identidade" | "cano" | "texto" | "text" | "resumo"
        | "autor" | "author" | "label" | "fullname" | "moniker"
        | "motivo" | "mensagem" | "message" | "ultimo_motivo" | "last_error")
}

/// SDP de candidato pode incluir hostname, foundation e `ufrag` sem rótulo `=`/`:`, além de IP.
/// Extrai somente campos de vocabulário fechado; nunca devolve o SDP original.
pub fn candidato(texto: &str) -> String {
    let campos: Vec<_> = texto.split_whitespace().collect();
    let tipo = campos.windows(2).find(|v| v[0] == "typ").map(|v| v[1]);
    let tipo = match tipo { Some("host") => "host", Some("srflx") => "srflx", Some("prflx") => "prflx", Some("relay") => "relay", _ => "desconhecido" };
    let transporte = match campos.get(2).map(|s| s.to_ascii_uppercase()).as_deref() { Some("UDP") => "UDP", Some("TCP") => "TCP", _ => "desconhecido" };
    let familia = match campos.get(4).and_then(|s| s.parse::<IpAddr>().ok()) { Some(IpAddr::V4(_)) => "IPv4", Some(IpAddr::V6(_)) => "IPv6", _ => "não informada" };
    let porta = campos.get(5).and_then(|s| s.parse::<u16>().ok()).map_or("?".to_string(), |p| p.to_string());
    format!("tipo={tipo} transporte={transporte} familia={familia} porta={porta}")
}

/// Erros do parser CLI podem ecoar o valor digitado entre aspas, inclusive um PIN inválido.
/// Preserva os nomes de opções, mas nunca os respectivos valores ecoados pelo parser.
pub fn sanitizar_argumentos(texto: &str) -> String {
    let mut seguro = String::new();
    let mut resto = texto;
    while let Some(pos) = resto.find(['\'', '"']) {
        seguro.push_str(&resto[..pos]);
        let aspa = resto[pos..].chars().next().unwrap();
        resto = &resto[pos + 1..];
        let Some(fim) = resto.find(aspa) else { seguro.push_str(OMITIDO); return sanitizar(&seguro); };
        let valor = &resto[..fim];
        let opcao = valor.split_whitespace().next().unwrap_or("");
        if opcao.starts_with("--") && opcao.chars().all(|c| c.is_ascii_alphabetic() || matches!(c, '-' | '_')) {
            seguro.push(aspa); seguro.push_str(opcao); seguro.push(aspa);
        } else { seguro.push_str(OMITIDO); }
        resto = &resto[fim + 1..];
    }
    seguro.push_str(resto);
    sanitizar(&seguro)
}

fn caractere_de_campo(c: char) -> bool { c.is_alphanumeric() || matches!(c, '_' | '-') }

fn campos(texto: &str) -> String {
    let mut saida = String::new();
    let mut pos = 0;
    while pos < texto.len() {
        let c = texto[pos..].chars().next().unwrap();
        if !caractere_de_campo(c) {
            saida.push(c); pos += c.len_utf8(); continue;
        }
        let inicio = pos;
        while pos < texto.len() {
            let c = texto[pos..].chars().next().unwrap();
            if !caractere_de_campo(c) { break; }
            pos += c.len_utf8();
        }
        let campo = &texto[inicio..pos];
        let mut valor = pos;
        // JSON: fecha a aspa do nome do campo, antes do ':'; Rust/debug e texto usam '=' ou ':'.
        if texto[valor..].starts_with('"') { valor += 1; }
        while texto[valor..].starts_with(char::is_whitespace) { valor += texto[valor..].chars().next().unwrap().len_utf8(); }
        let separador = texto[valor..].starts_with(['=', ':']);
        let pin_na_frase = campo.eq_ignore_ascii_case("pin")
            && texto[valor..].starts_with(|c: char| c.is_ascii_digit());
        let citado = texto[valor..].starts_with(['"', '\'']) && valor > pos;
        if !campo_sensivel(campo) || !(separador || pin_na_frase || citado) {
            saida.push_str(campo); continue;
        }
        if separador { valor += 1; }
        while texto[valor..].starts_with(char::is_whitespace) { valor += texto[valor..].chars().next().unwrap().len_utf8(); }
        let fim = fim_do_valor(texto, valor, campo.eq_ignore_ascii_case("pin"));
        saida.push_str(&texto[inicio..valor]);
        saida.push_str(OMITIDO);
        pos = fim;
    }
    saida
}

fn fim_do_valor(texto: &str, inicio: usize, pin: bool) -> usize {
    let mut pos = inicio;
    let Some(primeiro) = texto[pos..].chars().next() else { return pos; };
    if texto[pos..].starts_with(OMITIDO) { return pos + OMITIDO.len(); }
    // JSON já redigido usa null/booleanos. Reconhece a fronteira estrutural, sem consumir
    // as métricas seguintes quando o logger recebe o relato seguro serializado.
    for literal in ["null", "false", "true"] {
        if let Some(resto) = texto[pos..].strip_prefix(literal) {
            if resto.is_empty() || resto.starts_with([',', '}', ']', ')']) { return pos + literal.len(); }
        }
    }
    if texto[pos..].starts_with("Some(") { return fim_do_grupo(texto, pos + 4); }
    if matches!(primeiro, '"' | '\'') {
        pos += 1;
        let mut escape = false;
        for c in texto[pos..].chars() {
            pos += c.len_utf8();
            if !escape && c == primeiro { return pos; }
            escape = !escape && c == '\\';
        }
        return pos;
    }
    if matches!(primeiro, '[' | '{' | '(') {
        let mut fim = fim_do_grupo(texto, pos);
        if texto[fim..].starts_with(':') {
            fim += 1;
            while texto[fim..].starts_with(|c: char| c.is_ascii_digit()) { fim += 1; }
        }
        return fim;
    }
    // PINs de tela também vêm separados por espaço/hífen. Não deixa a segunda metade no log.
    if pin && primeiro.is_ascii_digit() {
        let mut fim = pos;
        for c in texto[pos..].chars() {
            if !(c.is_ascii_digit() || matches!(c, ' ' | '-')) { break; }
            pos += c.len_utf8();
            if c.is_ascii_digit() { fim = pos; }
        }
        return fim;
    }
    // Texto livre não tem fronteira confiável. Melhor omitir o restante do diagnóstico que
    // deixar metade da credencial ou mensagem remota no arquivo. Os chamadores registram
    // categorias e métricas separadas, sem depender de conteúdo arbitrário para a causa.
    texto.len()
}

fn fim_do_grupo(texto: &str, inicio: usize) -> usize {
    let mut esperados = Vec::new();
    let mut aspa = None;
    let mut escape = false;
    for (offset, c) in texto[inicio..].char_indices() {
        if let Some(atual) = aspa {
            if !escape && c == atual { aspa = None; }
            escape = !escape && c == '\\';
            continue;
        }
        if matches!(c, '"' | '\'') { aspa = Some(c); escape = false; continue; }
        match c {
            '[' => esperados.push(']'), '{' => esperados.push('}'), '(' => esperados.push(')'),
            ']' | '}' | ')' => {
                if esperados.pop() != Some(c) { return texto.len(); }
                if esperados.is_empty() { return inicio + offset + c.len_utf8(); }
            }
            _ => {}
        }
    }
    texto.len()
}

fn tokens(texto: &str) -> String {
    let mut saida = String::new();
    let mut inicio = 0;
    let mut dentro = false;
    for (pos, c) in texto.char_indices().chain(std::iter::once((texto.len(), ' '))) {
        let token = c.is_alphanumeric() || matches!(c, '.' | ':' | '%' | '[' | ']' | '/' | '\\' | '@' | '-' | '_');
        if token && !dentro { inicio = pos; dentro = true; }
        if !token {
            if dentro { saida.push_str(&token_seguro(&texto[inicio..pos])); dentro = false; }
            if pos < texto.len() { saida.push(c); }
        }
    }
    saida
}

fn token_seguro(token: &str) -> String {
    if token.to_ascii_lowercase().starts_with("quall://") { return "quall://<omitido>".into(); }
    if token.len() >= 32 && token.bytes().all(|b| b.is_ascii_hexdigit()) { return "<digest omitido>".into(); }
    // Aceita ponto final da frase, SocketAddr, IPv6 com zona e endereços sem porta.
    let ip = token.trim_end_matches('.');
    if let Ok(socket) = ip.parse::<SocketAddr>() {
        return format!("<{}>:{}", if socket.is_ipv4() { "IPv4" } else { "IPv6" }, socket.port());
    }
    let sem_colchetes = ip.trim_matches(['[', ']']);
    let sem_zona = sem_colchetes.split('%').next().unwrap_or(sem_colchetes);
    if let Ok(endereco) = sem_zona.parse::<IpAddr>() {
        return format!("<{}>", if endereco.is_ipv4() { "IPv4" } else { "IPv6" });
    }
    if token.contains(":\\") || token.starts_with("\\\\") { return "<caminho omitido>".into(); }
    token.into()
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn pin_em_todos_os_formatos_fica_fora_do_registro() {
        for linha in ["pin=123456", "PIN: 123 456", "PIN 123456", "pin: \"123456\"", "{\"pin\":\"123456\"}", "pin_fixo=123456", "pin: Some(\"123 456\")"] {
            let seguro = sanitizar(linha);
            assert!(!seguro.contains("123") && !seguro.contains("456"), "{seguro}");
        }
        assert_eq!(sanitizar("WRONG_PIN: tentativas=3 código=0x80004005"), "WRONG_PIN: tentativas=3 código=0x80004005");
    }

    #[test]
    fn credenciais_json_rust_e_sdp_sao_redigidas() {
        for campo in ["shared_secret", "pairing_key", "password", "token", "ice-pwd", "ice-ufrag"] {
            for valor in ["segredo-unico", "\"segredo com espaço\"", "[1,2,3,4]"] {
                let seguro = sanitizar(&format!("{campo}: {valor} falha=PAIRING tentativas=2"));
                assert!(!seguro.contains(valor), "{seguro}");
                if valor.starts_with(['"', '[']) {
                    assert!(seguro.contains("falha=PAIRING tentativas=2"), "{seguro}");
                }
            }
        }
        let sdp = sanitizar("a=ice-pwd:segredo\na=ice-ufrag:usuario");
        assert!(!sdp.contains("segredo") && !sdp.contains("usuario") && !sdp.contains('\n'));
    }

    #[test]
    fn ip_uri_e_identidade_ficam_fora_mas_porta_e_metricas_continuam() {
        let seguro = sanitizar("192.168.1.42:7877 -> [fd12::42]:4567 nome=\"Nome Particular\" device_id=\"meu-id\" frames=305 fps=30.0");
        assert_eq!(seguro, "<IPv4>:7877 -> <IPv6>:4567 nome=<omitido> device_id=<omitido> frames=305 fps=30.0");
        for endereco in ["quall://123456@192.168.1.42:7979", "fe80::42%11", "192.168.1.42."] {
            let seguro = sanitizar(endereco);
            assert!(!seguro.contains("123456") && !seguro.contains("42"), "{seguro}");
        }
    }

    #[test]
    fn digests_longos_e_caminho_pessoal_nao_persistem() {
        let chave = "01".repeat(32);
        assert!(!sanitizar(&format!("chave {chave}" )).contains(&chave));
        assert_eq!(sanitizar(r"arquivo C:\Users\Particular\pares.json recusado HRESULT=0x80070005"), "arquivo <caminho omitido> recusado HRESULT=0x80070005");
    }

    #[test]
    fn texto_unicode_sem_dados_sensiveis_e_idempotente() {
        for texto in ["falha: parâmetro recusado; quadros=120 p50_ms=3.70", "PIN errado; PIN mudou", "som=sim codec=Opus taxa=48000", ""] {
            assert_eq!(sanitizar(texto), texto);
        }
        let seguro = sanitizar("PIN=123456 par=\"José\" endereço=[fd12::1]:7979");
        assert_eq!(sanitizar(&seguro), seguro);
    }

    #[test]
    fn erro_cli_nao_ecoa_pin_invalido_ou_endpoint_do_usuario() {
        let seguro = sanitizar_argumentos("error: invalid value '12345' for '--pin': PIN precisa de seis dígitos\nUsage: quall --pin <PIN>");
        assert!(!seguro.contains("12345"));
        assert!(seguro.contains("'--pin'") && seguro.contains("PIN precisa de seis dígitos"));
        assert!(!sanitizar_argumentos("unexpected argument 'quall://123456@192.168.1.42:7979'").contains("123456"));
    }

    #[test]
    fn credencial_livre_omite_ate_o_fim() {
        for campo in ["secret", "password", "pairing_key", "token", "nome", "motivo"] {
            let seguro = sanitizar(&format!("frames=120 {campo}=NAO RETER VALOR LIVRE 901234"));
            assert_eq!(seguro, format!("frames=120 {campo}=<omitido>"));
        }
    }

    #[test]
    fn arrays_aninhados_nao_vazam_cauda() {
        let seguro = sanitizar("secret=[1,[\"SEGREDO\"],{\"x\":[3,4]}] fps=30");
        assert_eq!(seguro, "secret=<omitido> fps=30");
    }

    #[test]
    fn arrays_truncados_omitem_restante() {
        for fragmento in ["[1,[\"SEGREDO\"]", "[1,[SEGREDO", "[1,[\"SEGREDO\"", "[1,[\"SEGREDO\"]} cauda"] {
            assert_eq!(sanitizar(&format!("password={fragmento} segredo solto 901234")), "password=<omitido>");
        }
    }

    #[test]
    fn objeto_credencial_aninhado_e_redigido() {
        assert_eq!(sanitizar("key={\"privada\":{\"dados\":[7,8]}} HRESULT=0x80070005"), "key=<omitido> HRESULT=0x80070005");
    }

    #[test]
    fn opcao_aninhada_e_truncada_e_redigida() {
        assert_eq!(sanitizar("pin: Some(\"123 456\") frames=12"), "pin: <omitido> frames=12");
        assert_eq!(sanitizar("shared_secret=Some([1,[\"SEGREDO\"]]) frames=12"), "shared_secret=<omitido> frames=12");
        assert_eq!(sanitizar("shared_secret=Some([1,[\"SEGREDO\"]] resto"), "shared_secret=<omitido>");
    }

    #[test]
    fn aspas_truncadas_e_escapadas_nao_vazam() {
        assert_eq!(sanitizar("secret=\"NAO RETER 901234"), "secret=<omitido>");
        assert_eq!(sanitizar(r#"secret="parte \"parte\" privada" frames=4"#), "secret=<omitido> frames=4");
    }

    #[test]
    fn credencial_multilinha_e_redigida_antes_da_neutralizacao() {
        assert_eq!(sanitizar("password=\"NAO\nRETER\n901234\" frames=3"), "password=<omitido> frames=3");
        assert_eq!(sanitizar("frames=3\npassword=NAO\nRETER 901234"), "frames=3 | password=<omitido>");
    }

    #[test]
    fn dados_estruturados_redigidos_preservam_metricas_e_idempotencia() {
        let seguro = sanitizar("{\"secret\":[1,[\"SEGREDO\"]],\"frames\":120} password=\"privado\" codigo=WRONG_PIN");
        assert!(seguro.contains("\"frames\":120") && seguro.contains("codigo=WRONG_PIN"));
        assert!(!seguro.contains("SEGREDO") && !seguro.contains("privado"));
        assert_eq!(sanitizar(&seguro), seguro);
    }

    #[test]
    fn candidato_ice_nao_imprime_ufrag_hostname_ou_ip() {
        assert_eq!(candidato("candidate:privado 1 UDP 2130706431 192.168.1.42 7877 typ host ufrag segredo"), "tipo=host transporte=UDP familia=IPv4 porta=7877");
        assert_eq!(candidato("candidate:privado 1 TCP 9 fd12::42 5000 typ host tcptype passive ufrag segredo"), "tipo=host transporte=TCP familia=IPv6 porta=5000");
        assert_eq!(candidato("candidate:privado 1 UDP 9 pessoa.local 5000 typ host ufrag segredo"), "tipo=host transporte=UDP familia=não informada porta=5000");
        assert!(!candidato("nome-livre secreto 123456").contains("secreto"));
    }
}
