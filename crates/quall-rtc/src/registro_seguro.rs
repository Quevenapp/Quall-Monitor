//! Diagnóstico nativo recebe texto de terceiros, inclusive SDP, ICE e exceções livres.
//! Emitimos categorias conhecidas e errno, nunca o texto livre. Não muda o nível do logger.

pub(super) fn mensagem_nativa(texto: &str) -> String {
    // A lista guarda causas úteis. `contains` só seleciona uma constante: nenhum trecho do
    // texto original é copiado, mesmo se uma exceção simular um prefixo conhecido.
    const CAUSAS: &[&str] = &[
        "STUN integrity check failed",
        "STUN username invalid",
        "STUN local ufrag check failed",
        "STUN remote ufrag check failed",
        "STUN fingerprint check failed",
        "Failed to parse remote SDP candidate",
        "Failed to parse candidate",
        "Rejected ICE candidate",
        "Invalid fingerprint",
        "Unknown SDP fingerprint format",
        "STUN authentication failed",
        "No credentials for username",
        "No credentials for userhash",
        "Got STUN error code",
        "Uncaught exception in callback",
        "Exception in incoming media handler",
        "Send failed, buffer is full",
        "Send failed, datagram is too large",
        "Send failed",
        "recvfrom failed",
        "getnameinfo failed",
        "STUN message reading failed",
        "STUN message write failed",
        "STUN message send failed",
        "STUN message too short",
        "Invalid STUN message length",
        "Invalid STUN message",
        "Unexpected STUN message",
        "SCTP sending failed",
        "SCTP shutdown failed",
        "SCTP connection failed",
        "SCTP disconnected",
        "SCTP connected",
        "SCTP flush",
        "SCTP upcall",
        "SCTP write",
        "SCTP message is too large",
        "DTLS handshake failed",
        "DTLS handshake finished",
        "DTLS closed",
        "DTLS recv",
        "DTLS alert",
        "SRTP media sent before keys are derived",
        "Deriving SRTP keying material",
        "WebSocket connection timed out",
        "ICE-TCP is not supported",
        "ICE UDP mux is not available",
        "No local address found",
        "Specified external address is invalid",
        "Failed to resolve",
        "Failed to bind",
        "Failed to connect",
    ];
    let causa = CAUSAS
        .iter()
        .find(|causa| texto.contains(**causa))
        .copied()
        .unwrap_or("diagnóstico nativo (detalhes ocultos)");
    let errno = texto.split_once("errno=").and_then(|(_, restante)| {
        let codigo: String = restante
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .take(12)
            .collect();
        codigo.parse::<i32>().ok().filter(|codigo| {
            // Prosa externa não pode disfarçar um PIN de seis dígitos como errno.
            // Preserva errno POSIX e a faixa Winsock, que também aparece no vendor Windows.
            (-4095..=4095).contains(codigo) || (10000..=11999).contains(codigo)
        })
    });
    match errno {
        Some(codigo) => format!("{causa}; errno={codigo}"),
        None => causa.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credenciais_e_payloads_nativos_nao_sao_reproduzidos() {
        for texto in [
            "agent.c@1214: STUN integrity check failed, password=\"segredo-sem-rotulo\"",
            "STUN username invalid, username=\"usuario-privado\"",
            "STUN local ufrag check failed, expected=\"abc\", actual=\"def\"",
            "Rejected ICE candidate: candidate:1 UDP 192.0.2.10 12345 senha-livre",
            "Invalid fingerprint \"AA:BB:CC\", expected \"DD:EE:FF\"",
            "Uncaught exception in callback: o meu PIN pessoal era 901234",
            "mensagem nova desconhecida segredo-livre fd00::123%en0 /Users/pessoa/arquivo",
            "Send failed, buffer is full\npassword=901234",
        ] {
            let seguro = mensagem_nativa(texto);
            for proibido in [
                "segredo",
                "usuario-privado",
                "901234",
                "192.0.2.10",
                "fd00",
                "/Users",
                "AA:BB",
                "abc",
                "def",
            ] {
                assert!(!seguro.contains(proibido), "vazou {proibido}: {seguro}");
            }
        }
    }

    #[test]
    fn preserva_ocorrencia_causa_e_codigo_numerico() {
        assert_eq!(
            mensagem_nativa("send@63: Send failed, buffer is full"),
            "Send failed, buffer is full"
        );
        assert_eq!(
            mensagem_nativa("send@64: Send failed, errno=55"),
            "Send failed; errno=55"
        );
        assert_eq!(
            mensagem_nativa("SCTP sending failed, errno=-1 password=x"),
            "SCTP sending failed; errno=-1"
        );
        assert_eq!(
            mensagem_nativa("DTLS handshake failed: texto recebido"),
            "DTLS handshake failed"
        );
        assert_eq!(
            mensagem_nativa("errno=2147483648"),
            "diagnóstico nativo (detalhes ocultos)"
        );
        assert_eq!(
            mensagem_nativa("Send failed, errno=901234"),
            "Send failed"
        );
        assert_eq!(
            mensagem_nativa("Send failed, errno=10061"),
            "Send failed; errno=10061"
        );
    }
}
