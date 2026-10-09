//! Os erros de bancada devem continuar visíveis sem ecoar os argumentos particulares.

pub fn interpretar<T: clap::Parser>() -> T {
    T::try_parse().unwrap_or_else(falhar_argumentos)
}

pub fn interpretar_de<T, I, S>(argumentos: I) -> T
where
    T: clap::Parser,
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString> + Clone,
{
    T::try_parse_from(argumentos).unwrap_or_else(falhar_argumentos)
}

fn falhar_argumentos<T>(erro: clap::Error) -> T {
    if erro.use_stderr() {
        std::eprintln!("{}", crate::higiene_do_registro::sanitizar_argumentos(&erro.to_string()));
    } else {
        // Ajuda/versão são texto estático e precisam manter o formato público da CLI.
        let _ = erro.print();
    }
    std::process::exit(erro.exit_code());
}

/// Não deixa a implementação automática de `Termination` imprimir o Debug do erro.
pub fn concluir<E: std::any::Any>(resultado: Result<(), E>) {
    if let Err(erro) = resultado {
        std::eprintln!("falha: {}", mensagem_de_falha(&erro));
        std::process::exit(1);
    }
}

/// Display/Debug desconhecido pode conter prosa remota sem rótulos. Só tipos reais fornecem
/// categorias/códigos; nenhum número é extraído de texto nem uma mensagem arbitrária é ecoada.
pub fn mensagem_de_falha(erro: &dyn std::any::Any) -> String {
    let encadeado = erro.downcast_ref::<anyhow::Error>();
    #[cfg(feature = "net")]
    if let Some(erro) = erro.downcast_ref::<quall_core::error::Error>()
        .or_else(|| encadeado.and_then(|e| e.downcast_ref::<quall_core::error::Error>()))
    { return format!("classe=NUCLEO status={}", crate::diagnostico_rede::status(erro)); }
    #[cfg(windows)]
    if let Some(erro) = erro.downcast_ref::<windows::core::Error>()
        .or_else(|| encadeado.and_then(|e| e.downcast_ref::<windows::core::Error>()))
    {
        // Uma falha HRESULT real tem severity=1. Código positivo não vira diagnóstico numérico.
        return if erro.code().is_err() {
            format!("classe=WINDOWS HRESULT=0x{:08X}", erro.code().0 as u32)
        } else { "classe=WINDOWS codigo_omitido".into() };
    }
    if let Some(erro) = erro.downcast_ref::<std::io::Error>()
        .or_else(|| encadeado.and_then(|e| e.downcast_ref::<std::io::Error>()))
    {
        let codigo = erro.raw_os_error().filter(|c| (1..=65535).contains(c));
        return format!("classe=IO tipo={:?}{}", erro.kind(), codigo.map_or(String::new(), |c| format!(" codigo={c}")));
    }
    "classe=OUTRA".into()
}

#[cfg(test)]
mod testes {
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Parametros { #[arg(long)] pin: u32 }

    #[test]
    fn erro_do_clap_preserva_codigo_e_opcao_sem_ecoa_valor() {
        let erro = Parametros::try_parse_from(["sonda", "--pin", "privado-invalido"]).unwrap_err();
        let texto = crate::higiene_do_registro::sanitizar_argumentos(&erro.to_string());
        assert_eq!(erro.exit_code(), 2);
        assert!(erro.use_stderr() && texto.contains("--pin"));
        assert!(!texto.contains("privado-invalido"));
    }

    #[test]
    fn ajuda_do_clap_continua_ajuda_sem_falha() {
        let ajuda = Parametros::try_parse_from(["sonda", "--help"]).unwrap_err();
        assert_eq!(ajuda.exit_code(), 0);
        assert!(!ajuda.use_stderr() && ajuda.to_string().contains("--pin"));
    }

    #[test]
    fn retorno_generico_ou_anyhow_livre_nao_ecoa_payload() {
        for privado in ["segredo-curto-livre 901234", "password privado sem rotulo", "PIN=901234"] {
            assert_eq!(super::mensagem_de_falha(&privado), "classe=OUTRA");
            assert_eq!(super::mensagem_de_falha(&anyhow::anyhow!(privado)), "classe=OUTRA");
        }
    }

    #[test]
    fn io_conserva_causa_tipada_sem_mensagem_ou_codigo_disfarcado() {
        let privado = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "segredo-curto-livre 901234");
        assert_eq!(super::mensagem_de_falha(&privado), "classe=IO tipo=PermissionDenied");
        assert!(super::mensagem_de_falha(&std::io::Error::from_raw_os_error(13)).contains("codigo=13"));
        assert!(!super::mensagem_de_falha(&std::io::Error::from_raw_os_error(901234)).contains("901234"));
        assert!(!super::mensagem_de_falha(&anyhow::Error::new(privado)).contains("901234"));
    }

    #[cfg(feature = "net")]
    #[test]
    fn erro_do_nucleo_em_anyhow_preserva_categoria_sem_payload() {
        let erro = quall_core::error::Error::Protocol("segredo-curto-livre 901234".into());
        assert_eq!(super::mensagem_de_falha(&erro), "classe=NUCLEO status=PROTOCOL");
        assert_eq!(super::mensagem_de_falha(&anyhow::Error::new(erro)), "classe=NUCLEO status=PROTOCOL");
    }
}
