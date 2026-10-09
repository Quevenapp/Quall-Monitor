//! **As regras do R5 no Windows que não são da gravação** (`docs/teleprompter-com-camera.md`
//! §2.4, §2.5, §4.4 e §8.10): a ordem das placas do dono da captura, a câmera "frontal" do
//! catálogo, e a leitura do consentimento do microfone. Aritmética pura, sem Win32, para os testes
//! rodarem no portão e em qualquer máquina (`rustc --test` sobre este arquivo sozinho).

/// `0x8086`.
pub const VENDOR_INTEL: u32 = 0x8086;
/// `0x10DE`.
pub const VENDOR_NVIDIA: u32 = 0x10DE;

/// **A ordem em que o dono tenta as placas** (§2.4, item 2, e a S-W1 de §8.2): a Intel primeiro — o
/// Quick Sync fez dois H.264 a 30 fps juntos, e o MFT da NVIDIA falhou com `0x8000FFFF` até sozinho
/// —, depois as outras na ordem da DXGI, e a NVIDIA por último. Recebe `(luid, fabricante)` e
/// devolve os LUIDs na ordem.
pub fn ordem_das_placas(placas: &[(u64, u32)]) -> Vec<u64> {
    let posto = |v: u32| match v {
        VENDOR_INTEL => 0u8,
        VENDOR_NVIDIA => 2,
        _ => 1,
    };
    let mut v: Vec<(usize, u64, u32)> = placas.iter().enumerate().map(|(i, (l, f))| (i, *l, *f)).collect();
    v.sort_by_key(|(i, _, f)| (posto(*f), *i));
    v.into_iter().map(|(_, l, _)| l).collect()
}

/// A câmera parece a integrada do notebook? **Pelo nome, e é hipótese** (§8.10): o catálogo não
/// distingue a embutida (o `EnclosureLocation` é do WinRT, e o app não o lê). "Integrated Webcam"
/// (Dell), "Integrated Camera" (Lenovo), "HP TrueVision", "Built-in", "Internal".
pub fn parece_integrada(nome: &str) -> bool {
    let n = nome.to_lowercase();
    ["integrated", "integrada", "built-in", "builtin", "internal", "interna", "truevision", "embutida"]
        .iter()
        .any(|p| n.contains(p))
}

/// **A câmera da tela R5** (§2.5): a lembrada, se ainda está no catálogo; senão a que parece
/// integrada; senão a primeira. Recebe `(id, nome)` das câmeras do seletor.
pub fn camera_inicial(cameras: &[(String, String)], lembrada: Option<&str>) -> Option<usize> {
    if let Some(l) = lembrada {
        if let Some(i) = cameras.iter().position(|(id, _)| id.eq_ignore_ascii_case(l)) {
            return Some(i);
        }
    }
    cameras.iter().position(|(_, nome)| parece_integrada(nome)).or(if cameras.is_empty() { None } else { Some(0) })
}

/// O que o registro diz do consentimento (`...\CapabilityAccessManager\ConsentStore\microphone`,
/// valor `Value`: `Allow` ou `Deny`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consentimento {
    Permitido,
    Negado,
    /// A chave ou o valor não existem, ou o texto é outro.
    Desconhecido,
}

pub fn ler_consentimento(valor: Option<&str>) -> Consentimento {
    match valor.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "allow" => Consentimento::Permitido,
        Some(v) if v == "deny" => Consentimento::Negado,
        _ => Consentimento::Desconhecido,
    }
}

/// A frase da tela quando o Windows não deixa usar o microfone. **Em português, sempre**: a janela
/// a compara (`janela.rs`, `tela_r5.rs`) e a traduz na hora de mostrar (`idioma::tr`).
pub const FRASE_DA_PRIVACIDADE: &str = "O Windows não deixa este app usar o microfone. Ligue em Configurações → Privacidade e segurança → Microfone: \"Acesso ao microfone\" e \"Permitir que aplicativos da área de trabalho acessem o microfone\"."; // i18n: chave

/// `E_ACCESSDENIED`.
pub const HR_ACESSO_NEGADO: u32 = 0x8007_0005;

/// **Por que o microfone não abriu**, para a tela (§4.4): a Privacidade quando a abertura deu
/// acesso negado **ou** o registro diz `Deny` em qualquer das três chaves (a do computador, a do
/// usuário e a dos apps da área de trabalho); senão, a falha como veio. A hipótese do §4.4 é que a
/// abertura falhe com acesso negado; se ela abrir e entregar silêncio, o registro é a testemunha.
pub fn frase_do_microfone(codigo: Option<u32>, maquina: Consentimento, usuario: Consentimento, area_de_trabalho: Consentimento, falha: &str) -> String {
    let negado = [maquina, usuario, area_de_trabalho].contains(&Consentimento::Negado);
    if codigo == Some(HR_ACESSO_NEGADO) || negado {
        FRASE_DA_PRIVACIDADE.to_string()
    } else if falha.is_empty() {
        // Em português: a frase fica guardada no estado do microfone; a janela traduz ao mostrar.
        "O microfone não abriu.".to_string() // i18n: chave
    } else {
        crate::idioma::tf("O microfone não abriu: {}", &[&falha])
    }
}

/// O registro diz, de antemão, que o microfone vai ser negado?
pub fn negado_pelo_registro(maquina: Consentimento, usuario: Consentimento, area_de_trabalho: Consentimento) -> bool {
    [maquina, usuario, area_de_trabalho].contains(&Consentimento::Negado)
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn a_intel_primeiro_e_a_nvidia_por_ultimo() {
        let placas = [(1, VENDOR_NVIDIA), (2, VENDOR_INTEL), (3, 0x1002)];
        assert_eq!(ordem_das_placas(&placas), vec![2, 3, 1]);
        assert_eq!(ordem_das_placas(&[(9, VENDOR_NVIDIA)]), vec![9], "só a NVIDIA: ela mesma");
        assert!(ordem_das_placas(&[]).is_empty());
        // duas Intel (o SudoVDA não entra: `placas_de_hardware` já pula as indiretas) mantêm a ordem
        assert_eq!(ordem_das_placas(&[(5, VENDOR_INTEL), (4, VENDOR_INTEL)]), vec![5, 4]);
    }

    #[test]
    fn a_camera_da_tela() {
        let c = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let lista = c(&[("usb#canon", "Canon EOS"), ("usb#int", "Integrated Webcam"), ("usb#gs", "Panasonic GS500")]);
        assert_eq!(camera_inicial(&lista, None), Some(1), "a integrada, sem lembrada");
        assert_eq!(camera_inicial(&lista, Some("USB#GS")), Some(2), "a lembrada, sem caixa");
        assert_eq!(camera_inicial(&lista, Some("sumiu")), Some(1), "a lembrada que saiu cai na integrada");
        let sem = c(&[("a", "Canon EOS"), ("b", "Logitech C920")]);
        assert_eq!(camera_inicial(&sem, None), Some(0));
        assert_eq!(camera_inicial(&[], None), None);
        assert!(parece_integrada("HP TrueVision HD Camera"));
        assert!(!parece_integrada("Logitech BRIO"));
    }

    #[test]
    fn o_consentimento_e_a_frase() {
        use Consentimento::*;
        assert_eq!(ler_consentimento(Some("Allow")), Permitido);
        assert_eq!(ler_consentimento(Some(" deny ")), Negado);
        assert_eq!(ler_consentimento(Some("Prompt")), Desconhecido);
        assert_eq!(ler_consentimento(None), Desconhecido);
        assert_eq!(frase_do_microfone(Some(HR_ACESSO_NEGADO), Desconhecido, Desconhecido, Desconhecido, "x"), FRASE_DA_PRIVACIDADE);
        assert_eq!(frase_do_microfone(None, Permitido, Permitido, Negado, "x"), FRASE_DA_PRIVACIDADE);
        assert_eq!(frase_do_microfone(Some(1), Permitido, Permitido, Permitido, "x"), "O microfone não abriu: x");
        assert!(negado_pelo_registro(Negado, Permitido, Permitido));
        assert!(!negado_pelo_registro(Desconhecido, Permitido, Desconhecido));
    }

    /// Em inglês: a frase com a falha nasce traduzida; a da privacidade fica a chave em português
    /// (comparada pela janela) e tem o par na tabela.
    #[test]
    fn a_frase_do_microfone_em_ingles() {
        use Consentimento::*;
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert_eq!(frase_do_microfone(Some(1), Permitido, Permitido, Permitido, "x"), "The microphone didn't open: x");
            assert_eq!(frase_do_microfone(None, Negado, Permitido, Permitido, "x"), FRASE_DA_PRIVACIDADE);
            assert!(crate::idioma::tr(FRASE_DA_PRIVACIDADE).starts_with("Windows isn't letting this app use the microphone."));
        });
    }
}
