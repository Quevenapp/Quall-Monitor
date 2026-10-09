//! O payload de pânico pode conter chaves ou prosa recebida do par sem qualquer rótulo.
//! A ocorrência e a localização do código bastam para diagnosticar sem copiá-lo aos sinks.

pub(super) fn mensagem_panico(info: &std::panic::PanicHookInfo<'_>) -> String {
    mensagem_da_origem(info.location().map(|l| (l.file(), l.line(), l.column())))
}

fn mensagem_da_origem(origem: Option<(&str, u32, u32)>) -> String {
    match origem {
        Some((arquivo, linha, coluna)) => {
            let nome = arquivo.rsplit(['/', '\\']).next().unwrap_or("origem");
            format!("pânico no núcleo do Quall: conteúdo oculto; origem={nome}:{linha}:{coluna}")
        }
        None => "pânico no núcleo do Quall: conteúdo oculto; origem indisponível".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostico_de_panico_preserva_local_sem_caminho_pessoal() {
        assert_eq!(
            mensagem_da_origem(Some(("/Users/pessoa/crates/quall-ffi/src/lib.rs", 4315, 7))),
            "pânico no núcleo do Quall: conteúdo oculto; origem=lib.rs:4315:7"
        );
        assert_eq!(
            mensagem_da_origem(Some(("C:\\Users\\pessoa\\src\\lib.rs", 42, 3))),
            "pânico no núcleo do Quall: conteúdo oculto; origem=lib.rs:42:3"
        );
        assert!(mensagem_da_origem(None).contains("origem indisponível"));
    }
}
