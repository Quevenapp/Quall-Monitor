//! A paridade das tabelas e a varredura do código (o contrato comum, item 7).

use super::*;

/// Os arquivos com texto de interface: a varredura lê o código deles.
const ARQUIVOS: &[(&str, &str)] = &[
    ("janela.rs", include_str!("../janela.rs")),
    ("modelo_da_janela.rs", include_str!("../modelo_da_janela.rs")),
    ("estilo.rs", include_str!("../estilo.rs")),
    ("bandeja.rs", include_str!("../bandeja.rs")),
    ("regras_da_bandeja.rs", include_str!("../regras_da_bandeja.rs")),
    ("regras_da_tela_estendida.rs", include_str!("../regras_da_tela_estendida.rs")),
    ("regras_do_driver.rs", include_str!("../regras_do_driver.rs")),
    ("janela_dos_ajustes.rs", include_str!("../janela_dos_ajustes.rs")),
    ("modelo_dos_ajustes.rs", include_str!("../modelo_dos_ajustes.rs")),
    ("regras_dos_controles.rs", include_str!("../regras_dos_controles.rs")),
    ("regras_da_camera_remota.rs", include_str!("../regras_da_camera_remota.rs")),
    ("modelo_dos_ajustes_remotos.rs", include_str!("../modelo_dos_ajustes_remotos.rs")),
    ("camera_remota.rs", include_str!("../camera_remota.rs")),
    ("teleprompter/tela.rs", include_str!("../teleprompter/tela.rs")),
    ("teleprompter/tela_r5.rs", include_str!("../teleprompter/tela_r5.rs")),
    ("teleprompter/regras.rs", include_str!("../teleprompter/regras.rs")),
    ("teleprompter/camera.rs", include_str!("../teleprompter/camera.rs")),
    ("teleprompter/sessao.rs", include_str!("../teleprompter/sessao.rs")),
    ("teleprompter/divisao.rs", include_str!("../teleprompter/divisao.rs")),
    ("teleprompter/mod.rs", include_str!("../teleprompter/mod.rs")),
    ("receptor.rs", include_str!("../receptor.rs")),
    ("exibicao.rs", include_str!("../exibicao.rs")),
    ("placa.rs", include_str!("../placa.rs")),
    ("sessoes.rs", include_str!("../sessoes.rs")),
    ("regras_r5.rs", include_str!("../regras_r5.rs")),
    ("regras_da_gravacao.rs", include_str!("../regras_da_gravacao.rs")),
    ("regras_da_camera.rs", include_str!("../regras_da_camera.rs")),
    ("ajustes_da_camera.rs", include_str!("../ajustes_da_camera.rs")),
    ("teleprompter/texto.rs", include_str!("../teleprompter/texto.rs")),
    ("emissor.rs", include_str!("../emissor.rs")),
    ("varias.rs", include_str!("../varias.rs")),
    ("microfone.rs", include_str!("../microfone.rs")),
    ("dono_da_captura.rs", include_str!("../dono_da_captura.rs")),
    ("catalogo_de_cameras.rs", include_str!("../catalogo_de_cameras.rs")),
    ("cameras.rs", include_str!("../cameras.rs")),
    ("fontes.rs", include_str!("../fontes.rs")),
    ("instancia.rs", include_str!("../instancia.rs")),
];

#[test]
fn as_tabelas_tem_par_para_tudo_e_as_mesmas_lacunas() {
    let mut vistos: HashMap<&str, &str> = HashMap::new();
    let mut problemas = Vec::new();
    for (area, textos) in areas() {
        for (pt, en) in textos {
            if pt.trim().is_empty() || en.trim().is_empty() {
                problemas.push(format!("{area}: par vazio ({pt:?} → {en:?})"));
            }
            if lacunas(pt) != lacunas(en) {
                problemas.push(format!("{area}: {pt:?} tem {} `{{}}` e {en:?} tem {}", lacunas(pt), lacunas(en)));
            }
            for marca in ["%s", "%d", "{0}", "{1}", "{e}", "{n}"] {
                if pt.contains(marca) != en.contains(marca) {
                    problemas.push(format!("{area}: {marca} só de um lado em {pt:?}"));
                }
            }
            if let Some(outro) = vistos.insert(pt, en) {
                if outro != *en {
                    problemas.push(format!("{area}: {pt:?} traduzido de dois jeitos ({outro:?} e {en:?})"));
                }
            }
        }
    }
    assert!(problemas.is_empty(), "{}", problemas.join("\n"));
}

#[test]
fn o_glossario_e_o_do_contrato() {
    // O glossário obrigatório (contrato comum, item 6): as cinco plataformas dizem o mesmo.
    let glossario = [
        ("Espelhar", "Mirror"),
        ("Receber", "Receive"),
        ("Tela estendida", "Extended display"),
        ("Teleprompter", "Teleprompter"),
        ("Roteiro", "Script"),
        ("Roteiros guardados", "Saved scripts"),
        ("Editar ou colar o roteiro", "Edit or paste the script"),
        ("Conectar", "Connect"),
        ("Desconectar", "Disconnect"),
        ("Parar", "Stop"),
        ("Câmera", "Camera"),
        ("Ajustes da câmera", "Camera settings"),
        ("Exposição", "Exposure"),
        ("Foco", "Focus"),
        ("Restaurar automático", "Reset to auto"),
        ("Anti-cintilação", "Anti-flicker"),
        ("Tela cheia", "Full screen"),
        ("Sair da tela cheia", "Exit full screen"),
        ("Microfone", "Microphone"),
        ("Pronto", "Done"),
        ("Fechar", "Close"),
        ("Ajustes", "Settings"),
    ];
    for (pt, en) in glossario {
        if let Some(achado) = en_de(pt) {
            assert_eq!(achado, en, "o glossário diz {pt:?} → {en:?}");
        }
    }
    for (pt, en) in [("Espelhar", "Mirror"), ("Fechar", "Close"), ("Ajustes", "Settings"), ("Tela estendida", "Extended display")] {
        assert_eq!(en_de(pt), Some(en), "{pt:?} tem de estar na tabela");
    }
}

#[test]
fn t_e_tf_seguem_o_idioma_da_thread() {
    assert_eq!(atual(), Idioma::Pt, "sem iniciar, o texto-fonte");
    assert_eq!(t("Espelhar"), "Espelhar");
    com_idioma(Idioma::En, || {
        assert_eq!(t("Espelhar"), "Mirror");
        assert_eq!(t("um texto que não está na tabela"), "um texto que não está na tabela");
        assert_eq!(decimal(12.25, 1), "12.2");
    });
    assert_eq!(decimal(12.25, 1), "12,2");
    assert_eq!(preencher("A {} e B {}", &[&1, &"dois"]), "A 1 e B dois");
    assert_eq!(preencher("só {}", &[]), "só {}");
    assert_eq!(atual(), Idioma::Pt, "o idioma da thread volta");
}

#[test]
fn o_padrao_e_o_sistema_e_a_escolha_vence() {
    assert_eq!(do_sistema(0x0416), Idioma::Pt, "pt-BR");
    assert_eq!(do_sistema(0x0816), Idioma::Pt, "pt-PT");
    assert_eq!(do_sistema(0x0409), Idioma::En, "en-US");
    assert_eq!(do_sistema(0x040C), Idioma::En, "fr-FR: inglês");
    assert_eq!(do_nome("pt-BR"), Idioma::Pt);
    assert_eq!(do_nome("es-ES"), Idioma::En);
    assert_eq!(ler_escolha("en\n"), Some(Idioma::En));
    assert_eq!(ler_escolha(" PT "), Some(Idioma::Pt));
    assert_eq!(ler_escolha("xx"), None);
    assert_eq!(inicial(None, Idioma::En), Idioma::En);
    assert_eq!(inicial(Some(Idioma::Pt), Idioma::En), Idioma::Pt);
    assert_eq!(Idioma::Pt.nome_acessivel(), "Idioma: Português");
    assert_eq!(Idioma::En.nome_acessivel(), "Language: English");
}

// =============================================================================================
// A varredura do código
// =============================================================================================

/// Os literais de uma linha (sem os de `r"…"`), com a coluna onde começam.
fn literais(linha: &str) -> Vec<(usize, String)> {
    let b = linha.as_bytes();
    let mut v = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            break;
        }
        if b[i] == b'\'' {
            // um char: '"' não abre literal
            if b.get(i + 2) == Some(&b'\'') {
                i += 3;
                continue;
            }
            if b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') {
                i += 4;
                continue;
            }
        }
        if b[i] == b'"' {
            let inicio = i;
            let mut s = String::new();
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    s.push(b[i + 1] as char);
                    i += 2;
                    continue;
                }
                let ch = linha[i..].chars().next().unwrap_or(' ');
                s.push(ch);
                i += ch.len_utf8();
            }
            v.push((inicio, s));
        }
        i += 1;
    }
    v
}

/// O literal parece português de interface? Uma letra acentuada, ou uma palavra comum do português.
fn parece_portugues(s: &str) -> bool {
    if s.chars().any(|c| "áàâãéêíóôõúçÁÀÂÃÉÊÍÓÔÕÚÇ".contains(c)) {
        return true;
    }
    let palavras = [
        " o ", " a ", " os ", " as ", " de ", " do ", " da ", " dos ", " das ", " em ", " no ", " na ", " um ", " uma ", " para ",
        " com ", " sem ", " que ", " e ", " ou ", " este ", " esta ", " aqui", " ainda", " tela", " roteiro", " aparelho", " toque",
    ];
    let m = format!(" {} ", s.to_lowercase());
    palavras.iter().any(|p| m.contains(p)) || parece_rotulo(s)
}

/// Um rótulo de uma palavra ("Editar", "Rolar", "Conectar"): maiúscula seguida de minúsculas. Fora os
/// nomes próprios que não se traduzem (fontes, classes de janela, marcas, exemplos de bancada).
fn parece_rotulo(s: &str) -> bool {
    let mut c = s.chars();
    let (Some(a), Some(b), Some(d)) = (c.next(), c.next(), c.next()) else { return false };
    if !(a.is_uppercase() && b.is_lowercase() && d.is_lowercase()) {
        return false;
    }
    let nomes = [
        "Segoe", "Consolas", "Cascadia", "DarkMode_", "TaskbarCreated", "Local\\", "Global\\", "Galaxy", "Canon", "Integrated",
        "ProcessOutput", "Microsoft", "Windows",
    ];
    if s == "Quall" || nomes.iter().any(|n| s.starts_with(n)) {
        return false;
    }
    // As classes de janela nossas: `QuallTeleprompter`, `QuallAppWindow`…
    !(s.starts_with("Quall") && s[5..].chars().next().is_some_and(|c| c.is_uppercase()))
}

/// As linhas que não são interface: diário, testes, asserções, atributos, comentários, e a marca
/// explícita `// i18n: fora` (bancada, nome de arquivo, protocolo).
fn linha_fora(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("//")
        || t.starts_with("#[")
        || [
            "registro::linha",
            "registro::",
            "eprintln!",
            "println!",
            "assert",
            "panic!",
            "unreachable!",
            ".expect(",
            "i18n: fora",
            "debug!",
            "anotar(",
        ]
        .iter()
        .any(|m| l.contains(m))
}

/// O argumento literal de uma chamada `t("…")`, `tf("…"`, `tr("…")` que começa na coluna `col`?
fn dentro_de_chamada(linha: &str, col: usize) -> bool {
    let Some(antes) = linha[..col].trim_end().strip_suffix('(') else { return false };
    let nome: String = antes.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<Vec<_>>().into_iter().rev().collect();
    matches!(nome.as_str(), "t" | "tf" | "tr")
}

/// O texto **do diário** que cruza várias linhas: um `registro::linha(format!(` aberto acima.
fn em_bloco_de_diario(linhas: &[&str], i: usize) -> bool {
    let mut profundidade = 0i32;
    for j in (i.saturating_sub(12)..i).rev() {
        let l = linhas[j];
        profundidade += l.matches(')').count() as i32 - l.matches('(').count() as i32;
        if ["registro::linha", "eprintln!", "println!", "anotar(", "assert", "panic!", ".expect("].iter().any(|m| l.contains(m)) {
            return profundidade < 0;
        }
        if l.trim_end().ends_with(';') || l.trim_end().ends_with('}') {
            return false;
        }
    }
    false
}

#[test]
fn a_varredura_nao_acha_texto_de_interface_literal() {
    let mut achados = Vec::new();
    let mut fora_da_tabela = Vec::new();
    for (nome, codigo) in ARQUIVOS {
        let linhas: Vec<&str> = codigo.lines().collect();
        let fim = linhas.iter().position(|l| l.trim_start().starts_with("#[cfg(test)]") || l.trim_start().starts_with("mod testes")).unwrap_or(linhas.len());
        for (i, l) in linhas[..fim].iter().enumerate() {
            for (col, s) in literais(l) {
                // `// i18n: chave`: uma constante em português que é chave da tabela (comparada ou
                // guardada em português, e traduzida na hora de mostrar com `t(CONSTANTE)`).
                let marca = |m: &str| l.contains(m) || (i > 0 && linhas[i - 1].trim_start().starts_with("//") && linhas[i - 1].contains(m));
                if dentro_de_chamada(l, col) || marca("i18n: chave") {
                    if en_de(&s).is_none() {
                        fora_da_tabela.push(format!("{nome}:{}: {s:?}", i + 1));
                    }
                    continue;
                }
                if linha_fora(l) || marca("i18n: fora") || em_bloco_de_diario(&linhas, i) || !parece_portugues(&s) {
                    continue;
                }
                achados.push(format!("{nome}:{}: {s:?}", i + 1));
            }
        }
    }
    assert!(fora_da_tabela.is_empty(), "{} chamadas com texto fora da tabela:\n{}", fora_da_tabela.len(), fora_da_tabela.join("\n"));
    assert!(achados.is_empty(), "{} textos de interface ainda literais (envolva em t()/tf(), ou marque `// i18n: fora`):\n{}", achados.len(), achados.join("\n"));
}
