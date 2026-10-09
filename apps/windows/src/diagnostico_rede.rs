//! Só categorias tipadas: uma mensagem remota pode conter qualquer texto, inclusive segredos.
pub fn status(erro: &quall_core::error::Error) -> &'static str {
    use quall_core::error::Error;
    match erro {
        Error::Invalid(_) => "INVALID", Error::Protocol(_) => "PROTOCOL",
        Error::Discovery(_) => "DISCOVERY", Error::Signaling(_) => "SIGNALING",
        Error::Transport(_) => "TRANSPORT", Error::NoRoute(_) => "NO_ROUTE",
        Error::Pairing(_) => "PAIRING", Error::WrongPin(_) => "WRONG_PIN",
        Error::NeedsPin(_) => "NEEDS_PIN", Error::Timeout(_) => "TIMEOUT",
        Error::Closed => "CLOSED", Error::Cancelled => "CANCELLED",
        Error::Io(_) => "IO", Error::Ocupado(_) => "BUSY",
    }
}

#[cfg(test)]
mod testes {
    #[test]
    fn erro_remoto_livre_vira_somente_categoria() {
        use quall_core::error::Error;
        let privado = "NAO RETER TEXTO LIVRE 901234";
        for erro in [Error::Protocol(privado.into()), Error::Pairing(privado.into()), Error::Invalid(privado.into()), Error::Io(privado.into())] {
            let codigo = super::status(&erro);
            assert!(codigo.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'));
            assert!(!codigo.contains("901234") && !codigo.contains("RETER"));
        }
    }
}
