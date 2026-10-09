//! Relatos de diagnóstico sem credenciais, identificação pessoal ou conteúdo de roteiro.
//! Não é usado para persistir estado funcional/pares, apenas para exportar diagnóstico.

use serde_json::Value;

pub fn redigir(valor: &mut Value) {
    match valor {
        Value::Object(campos) => {
            for (campo, valor) in campos.iter_mut() {
                if (crate::higiene_do_registro::campo_sensivel(campo)
                    || matches!(campo.as_str(), "caminho" | "capturas" | "device_path"))
                    && !matches!(valor, Value::Bool(_) | Value::Null)
                {
                    *valor = Value::Null;
                } else { redigir(valor); }
            }
        }
        Value::Array(valores) => valores.iter_mut().for_each(redigir),
        Value::String(texto) => *texto = crate::higiene_do_registro::sanitizar(texto),
        _ => {}
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn relato_aninhado_redige_credenciais_e_texto_mantendo_contadores() {
        let mut valor = serde_json::json!({
            "pin": "123456", "device_id": "identidade-pessoal", "nome": "Nome Particular", "camera":true,
            "sessao": {"pairing_secret": [1,2,3], "quadros": 120, "fps": 30.0,
                "eventos": ["caminho 192.168.1.42:7877 falha=TIMEOUT"]},
            "estado_final": {"texto":"roteiro particular", "prompter_nome":"Nome Particular", "posicao":3},
            "roteiro": {"bytes":18, "resumo":"resumo particular"}
        });
        redigir(&mut valor);
        let texto = serde_json::to_string(&valor).unwrap();
        for privado in ["123456", "identidade-pessoal", "Nome Particular", "192.168.1.42", "roteiro particular", "resumo particular"] {
            assert!(!texto.contains(privado), "{privado}");
        }
        assert_eq!(valor["sessao"]["quadros"], 120);
        assert_eq!(valor["camera"], true);
        assert_eq!(valor["sessao"]["fps"], 30.0);
        assert_eq!(valor["estado_final"]["posicao"], 3);
        assert!(texto.contains("TIMEOUT"));
        let linha = crate::higiene_do_registro::sanitizar(&texto);
        assert!(linha.contains("\"quadros\":120") && linha.contains("\"fps\":30.0"));
        let antes = valor.clone();
        redigir(&mut valor);
        assert_eq!(valor, antes);
    }
}
