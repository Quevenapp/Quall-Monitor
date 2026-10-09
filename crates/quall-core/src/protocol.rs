//! Identidade e versionamento do protocolo Quall.
//!
//! O Quall não fala AirPlay, Cast nem Miracast. Este módulo define o mínimo que dois aparelhos
//! precisam concordar antes de qualquer coisa: quem são e qual versão falam.

use serde::{Deserialize, Serialize};

/// Versão do protocolo de sinalização. Sobe a cada mudança incompatível no formato das
/// mensagens trocadas antes da mídia começar a fluir.
///
/// ## Por que ela subiu para 2, em 2026-08-31
///
/// A adaptação de taxa (`docs/taxa-que-escuta.md`) acrescentou a mensagem `{"t":"enlace",…}`, que
/// o receptor manda ao emissor a cada janela com a perda medida. **Um par que fale a versão 1 morre
/// ao recebê-la**: uma etiqueta que a build não conhece falha no `serde_json::from_str`, e
/// `session.rs::olhar_sinalizacao` traduz esse `Err` em `EventoDeSessao::Falhou` — apesar de o
/// comentário de lá prometer que mensagem pós-negociação desconhecida é ignorada. Está medido no
/// teste `etiqueta_desconhecida_derruba_a_mensagem_e_nao_e_ignorada`, em `signaling.rs`.
///
/// Enquanto o controlador nascia desligado isso não mordia, porque o relato não saía. Ligar por
/// padrão exigia primeiro fechar essa porta, e o mecanismo que a fecha **já existia**:
/// [`Announcement::is_compatible`] recusa par de versão diferente **antes** de qualquer mensagem
/// pós-negociação, em `discovery.rs` (some da lista) e em `signaling.rs::conferir_anuncio` (recusa
/// no aperto de mão, dizendo as duas versões em prosa).
///
/// **A consequência é operacional e não tem meio-termo:** um aparelho com build de versão 1 deixa
/// de conectar com um de versão 2, e vice-versa, com mensagem legível em vez de sessão morta. Toda
/// a bancada tem de ser atualizada junto.
///
/// **O que continua aberto e não foi mexido aqui:** o desserializador continua **estrito** com
/// etiqueta desconhecida. Subir a versão resolve *este* caso; não resolve a família — a próxima
/// mensagem aditiva vai exigir outra subida. Tolerar etiqueta desconhecida depois da negociação é
/// a alternativa, e é mudança de comportamento com lado de segurança (falhar alto contra ignorar
/// calado) que ninguém decidiu. O teste que fixa o comportamento de hoje diz, em voz alta, que se
/// um dia ele passar a devolver `Ok` é o teste que está errado, não o código.
/// Revisão com PAKE autenticado e sinalização integralmente cifrada. Não aceita v1/v2.
pub const PROTOCOL_VERSION: u16 = 3;

/// Tipo de serviço anunciado por mDNS/Bonjour na LAN.
pub const SERVICE_TYPE: &str = "_quall._tcp";

/// O que um aparelho é capaz de fazer. Qualquer aparelho pode emitir, qualquer aparelho pode
/// exibir — mas nem todo aparelho pode tudo (a extension do iOS não exibe; o plugin de OBS não
/// emite).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Consegue capturar e enviar a tela.
    pub screen_source: bool,
    /// Consegue capturar e enviar a câmera.
    pub camera_source: bool,
    /// Consegue receber e exibir.
    pub sink: bool,
}

/// Como o vídeo será codificado. Tela e câmera têm características opostas — conteúdo estático
/// com mudanças bruscas contra ruído com movimento contínuo — e não toleram o mesmo preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodePreset {
    /// Tela: prioriza nitidez de texto e reage a mudanças bruscas de cena.
    Screen,
    /// Câmera: prioriza fluidez sob movimento contínuo e ruído de sensor.
    Camera,
}

/// Identificador estável de um aparelho, gerado na primeira execução e persistido pela casca
/// da plataforma. É o que o pareamento vincula — não o nome, que o usuário pode trocar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(pub String);

/// A tela de quem exibe, em pixels do painel, sem orientação.
///
/// Existe para a **tela estendida** (`docs/tela-estendida.md`): o Mac cria um monitor virtual por
/// receptor, e o formato dele tem de ser o da tela do aparelho — até 10/09/2026 era 1920 × 1200 para
/// qualquer um, e um telefone mostrava um monitor de tablet com faixas pretas.
///
/// **Só inteiros.** `Announcement` deriva `Eq`, e a densidade da interface (que o iOS e o Android
/// medem de jeitos diferentes) não entra: o emissor decide o tamanho pelos pixels e pela proporção.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub width_px: u32,
    pub height_px: u32,
}

impl Screen {
    /// Um lado de 1 a 16384 px — o que um painel de verdade tem. Fora disso é lixo da casca.
    pub const LADO_MAXIMO: u32 = 16_384;

    pub fn nova(width_px: u32, height_px: u32) -> Option<Screen> {
        let valido = |l: u32| (1..=Screen::LADO_MAXIMO).contains(&l);
        (valido(width_px) && valido(height_px)).then_some(Screen {
            width_px,
            height_px,
        })
    }
}

/// **Para que é esta sessão**, quando não é vídeo. Ver `docs/contrato-teleprompter.md` §2.
///
/// Os valores no fio são literais do contrato: `"teleprompter"` e `"controle_remoto"`. Ausente é
/// o de sempre — vídeo —, e é o que toda build anterior manda.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Papel {
    /// Quem **hospeda** um teleprompter: mostra o texto e o PIN.
    Teleprompter,
    /// Quem **conecta** para controlar um teleprompter.
    ControleRemoto,
    /// Um valor que esta build não conhece — de uma build mais nova, com um papel que ainda não
    /// existe aqui. **Não derruba o anúncio**: o mesmo cuidado de `causa_do_fio`
    /// (`signaling.rs`), porque recusar um valor desconhecido faria o `serde` recusar o `Hello`
    /// inteiro e a sessão morrer sem motivo legível. Quem recebe um papel desconhecido o trata como
    /// um papel com que ele não sabe conversar.
    Desconhecido,
}

impl Papel {
    /// O valor no fio. `Desconhecido` nunca é mandado por esta build; se for reserializado (no JSON
    /// de `quall_session_peer_json`), sai como `"desconhecido"`.
    pub fn como_texto(self) -> &'static str {
        match self {
            Papel::Teleprompter => "teleprompter",
            Papel::ControleRemoto => "controle_remoto",
            Papel::Desconhecido => "desconhecido",
        }
    }

    /// Lê o valor do fio. Texto que esta build não conhece vira [`Papel::Desconhecido`], nunca
    /// erro.
    pub fn do_texto(texto: &str) -> Papel {
        match texto {
            "teleprompter" => Papel::Teleprompter,
            "controle_remoto" => Papel::ControleRemoto,
            _ => Papel::Desconhecido,
        }
    }
}

impl Serialize for Papel {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.como_texto())
    }
}

impl<'de> Deserialize<'de> for Papel {
    /// **Nunca falha**: texto desconhecido vira [`Papel::Desconhecido`], e um valor que nem texto
    /// é (um número, um objeto) também. Falhar aqui derrubaria o `Hello` inteiro — o defeito que
    /// `causa_do_fio` já evita para a causa da recusa.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Bruto {
            Texto(String),
            Outro(serde::de::IgnoredAny),
        }
        Ok(match Bruto::deserialize(d)? {
            Bruto::Texto(t) => Papel::do_texto(&t),
            Bruto::Outro(_) => Papel::Desconhecido,
        })
    }
}

/// O que um aparelho anuncia na rede.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    pub protocol_version: u16,
    pub device_id: DeviceId,
    /// Nome exibido na lista de aparelhos. Escolhido pelo usuário ou herdado do sistema.
    pub display_name: String,
    pub capabilities: Capabilities,
    /// A tela de quem vai exibir, quando a casca a disse (`quall_connect_with_screen`).
    ///
    /// **Aditivo, e sem subir [`PROTOCOL_VERSION`]**: ausente vira `None` e, `None`, o campo nem
    /// sai no JSON — um par da build anterior vê exatamente o anúncio de antes, e um que receba o
    /// campo o ignora (`Announcement` não recusa campo desconhecido). Não vai para o TXT do mDNS:
    /// `discovery.rs` monta aquele registro chave a chave. Ver os testes abaixo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<Screen>,
    /// Para que é esta sessão, quando não é vídeo. Ver [`Papel`] e `docs/contrato-teleprompter.md`.
    ///
    /// **Aditivo, e sem subir [`PROTOCOL_VERSION`]**, no molde de [`Announcement::screen`]: ausente
    /// vira `None` e, `None`, o campo nem sai no JSON — um par da build anterior vê o anúncio de
    /// antes, byte a byte, e um que receba o campo o ignora. **Não** é um campo de
    /// [`Capabilities`]: os de lá não têm `serde(default)`, e um campo novo ali quebraria quem não o
    /// manda.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub papel: Option<Papel>,
}

impl Announcement {
    /// Um anúncio de versão diferente da nossa não é utilizável.
    pub fn is_compatible(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
    }
}

/// **A regra do papel no aperto de mão**, do lado de **quem hospeda**, ao ler o `Hello`.
///
/// `Ok(())` segue; `Err(motivo)` é recusar o candidato **antes do PIN**, com o motivo em prosa.
///
/// | quem hospeda | quem conecta | |
/// |---|---|---|
/// | vídeo (sem papel) | qualquer um | segue — **exatamente como antes**: um anfitrião de vídeo não confere papel |
/// | `teleprompter` | `controle_remoto` | segue |
/// | `teleprompter` | qualquer outro, inclusive sem papel | recusa |
/// | `controle_remoto` ou desconhecido | qualquer um | recusa: quem hospeda não controla |
///
/// Ver `docs/contrato-teleprompter.md` §2.
pub fn papel_do_convidado_serve(
    anfitriao: Option<Papel>,
    convidado: Option<Papel>,
) -> Result<(), String> {
    match (anfitriao, convidado) {
        (None, _) => Ok(()),
        (Some(Papel::Teleprompter), Some(Papel::ControleRemoto)) => Ok(()),
        (Some(Papel::Teleprompter), None) => Err(
            "este aparelho é um teleprompter do Quall: ele não transmite vídeo, e só aceita o \
             controle remoto do teleprompter"
                .into(),
        ),
        (Some(Papel::Teleprompter), Some(outro)) => Err(format!(
            "este aparelho é um teleprompter do Quall e só aceita o controle remoto; o seu \
             aparelho conectou como \"{}\"",
            outro.como_texto()
        )),
        (Some(outro), _) => Err(format!(
            "um aparelho que hospeda não pode ter o papel \"{}\"",
            outro.como_texto()
        )),
    }
}

/// **A regra do papel no aperto de mão**, do lado de **quem conecta**, ao ler o `Welcome`.
///
/// | quem conecta | quem hospeda | |
/// |---|---|---|
/// | vídeo (sem papel) | vídeo (sem papel) | segue — como antes |
/// | vídeo (sem papel) | `teleprompter` ou outro | recusa: não há imagem ali |
/// | `controle_remoto` | `teleprompter` | segue |
/// | `controle_remoto` | qualquer outro, inclusive sem papel | recusa: não é um teleprompter |
/// | `teleprompter` ou desconhecido | qualquer um | recusa: quem conecta não mostra |
pub fn papel_do_anfitriao_serve(
    convidado: Option<Papel>,
    anfitriao: Option<Papel>,
) -> Result<(), String> {
    match (convidado, anfitriao) {
        (None, None) => Ok(()),
        (None, Some(p)) => Err(format!(
            "o outro aparelho é um {} do Quall, não um emissor de vídeo",
            if p == Papel::Teleprompter {
                "teleprompter"
            } else {
                "aparelho de outro papel"
            }
        )),
        (Some(Papel::ControleRemoto), Some(Papel::Teleprompter)) => Ok(()),
        (Some(Papel::ControleRemoto), _) => {
            Err("o outro aparelho não é um teleprompter do Quall".into())
        }
        (Some(outro), _) => Err(format!(
            "um aparelho que conecta não pode ter o papel \"{}\"",
            outro.como_texto()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announcement(protocol_version: u16) -> Announcement {
        Announcement {
            protocol_version,
            device_id: DeviceId("a10s-teste".into()),
            display_name: "Galaxy A10s".into(),
            capabilities: Capabilities {
                screen_source: true,
                camera_source: true,
                sink: true,
            },
            screen: None,
            papel: None,
        }
    }

    #[test]
    fn anuncio_da_versao_atual_e_compativel() {
        assert!(announcement(PROTOCOL_VERSION).is_compatible());
    }

    #[test]
    fn anuncio_de_outra_versao_nao_e_compativel() {
        assert!(!announcement(PROTOCOL_VERSION + 1).is_compatible());
    }

    #[test]
    fn anuncio_sobrevive_ida_e_volta_por_json() {
        let original = announcement(PROTOCOL_VERSION);
        let texto = serde_json::to_string(&original).expect("serializa");
        let voltou: Announcement = serde_json::from_str(&texto).expect("desserializa");
        assert_eq!(original, voltou);
    }

    #[test]
    fn anuncio_com_tela_sobrevive_ida_e_volta() {
        let mut original = announcement(PROTOCOL_VERSION);
        original.screen = Screen::nova(1125, 2436);
        let texto = serde_json::to_string(&original).expect("serializa");
        assert!(
            texto.contains("\"screen\":{\"width_px\":1125,\"height_px\":2436}"),
            "{texto}"
        );
        let voltou: Announcement = serde_json::from_str(&texto).expect("desserializa");
        assert_eq!(original, voltou);
    }

    /// **Sem tela, o JSON é o de antes, byte a byte**: o campo nem sai. É o que deixa um par da build
    /// anterior ver exatamente o anúncio que ele já conhecia.
    #[test]
    fn anuncio_sem_tela_nao_escreve_o_campo() {
        let texto = serde_json::to_string(&announcement(PROTOCOL_VERSION)).expect("serializa");
        // A chave exata: `screen_source`, nas capacidades, também tem "screen" dentro.
        assert!(!texto.contains("\"screen\""), "{texto}");
    }

    /// O anúncio de uma build anterior — sem a chave — desserializa, com a tela vazia.
    #[test]
    fn anuncio_antigo_sem_a_chave_desserializa() {
        let antigo = r#"{"protocol_version":2,"device_id":"x","display_name":"X",
            "capabilities":{"screen_source":false,"camera_source":false,"sink":true}}"#;
        let a: Announcement = serde_json::from_str(antigo).expect("desserializa");
        assert_eq!(a.screen, None);
        assert!(
            !a.is_compatible(),
            "desserializar não autoriza a versão antiga"
        );
    }

    /// O outro sentido: uma build **anterior** lendo um anúncio **com** a tela. A struct de lá não
    /// tem o campo; o que se prova aqui é que o serde dela ignora chave desconhecida — que é o que
    /// torna o campo aditivo sem subir a versão.
    #[test]
    fn build_anterior_le_anuncio_com_tela() {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct AnuncioDaBuildAnterior {
            protocol_version: u16,
            device_id: DeviceId,
            display_name: String,
            capabilities: Capabilities,
        }
        let mut novo = announcement(PROTOCOL_VERSION);
        novo.screen = Screen::nova(1920, 1200);
        let texto = serde_json::to_string(&novo).expect("serializa");
        let lido: AnuncioDaBuildAnterior =
            serde_json::from_str(&texto).expect("a build anterior aceita");
        assert_eq!(lido.display_name, "Galaxy A10s");
    }

    /// O receptor de vídeo omite o papel; sua identidade só circula no canal cifrado v3.
    #[test]
    fn anuncio_sem_papel_omite_a_chave_e_declara_v3() {
        let texto = serde_json::to_string(&announcement(PROTOCOL_VERSION)).expect("serializa");
        assert_eq!(
            texto,
            r#"{"protocol_version":3,"device_id":"a10s-teste","display_name":"Galaxy A10s","capabilities":{"screen_source":true,"camera_source":true,"sink":true}}"#
        );
    }

    #[test]
    fn anuncio_com_papel_sobrevive_ida_e_volta_e_escreve_o_valor_do_contrato() {
        for (papel, fio) in [
            (Papel::Teleprompter, "\"papel\":\"teleprompter\""),
            (Papel::ControleRemoto, "\"papel\":\"controle_remoto\""),
        ] {
            let mut a = announcement(PROTOCOL_VERSION);
            a.papel = Some(papel);
            let texto = serde_json::to_string(&a).expect("serializa");
            assert!(texto.ends_with(&format!(",{fio}}}")), "{texto}");
            let voltou: Announcement = serde_json::from_str(&texto).expect("desserializa");
            assert_eq!(voltou, a);
        }
    }

    /// Uma build **anterior** lendo um anúncio com papel: a struct de lá não tem o campo, e o
    /// serde dela ignora a chave. Igual ao teste da tela.
    #[test]
    fn build_anterior_le_anuncio_com_papel() {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct AnuncioDaBuildAnterior {
            protocol_version: u16,
            device_id: DeviceId,
            display_name: String,
            capabilities: Capabilities,
            #[serde(default)]
            screen: Option<Screen>,
        }
        let mut novo = announcement(PROTOCOL_VERSION);
        novo.papel = Some(Papel::Teleprompter);
        let texto = serde_json::to_string(&novo).expect("serializa");
        let lido: AnuncioDaBuildAnterior =
            serde_json::from_str(&texto).expect("a build anterior aceita");
        assert_eq!(lido.display_name, "Galaxy A10s");
    }

    /// Um papel que esta build não conhece — texto novo, número, objeto — não derruba o anúncio.
    #[test]
    fn papel_desconhecido_ou_de_tipo_errado_nao_derruba_o_anuncio() {
        for bruto in [r#""parede""#, "7", r#"{"x":1}"#, "true"] {
            let texto = format!(
                r#"{{"protocol_version":2,"device_id":"x","display_name":"X",
                "capabilities":{{"screen_source":false,"camera_source":false,"sink":true}},
                "papel":{bruto}}}"#
            );
            let a: Announcement = serde_json::from_str(&texto)
                .unwrap_or_else(|e| panic!("papel {bruto} derrubou o anúncio: {e}"));
            assert_eq!(a.papel, Some(Papel::Desconhecido), "{bruto}");
        }
    }

    /// A tabela da regra do papel, do lado de quem hospeda e do lado de quem conecta.
    #[test]
    fn a_regra_do_papel_e_a_tabela_do_contrato() {
        use Papel::*;
        // Quem hospeda vídeo não confere nada: o vídeo de sempre não muda.
        for convidado in [
            None,
            Some(Teleprompter),
            Some(ControleRemoto),
            Some(Desconhecido),
        ] {
            assert!(papel_do_convidado_serve(None, convidado).is_ok());
        }
        assert!(papel_do_convidado_serve(Some(Teleprompter), Some(ControleRemoto)).is_ok());
        for convidado in [None, Some(Teleprompter), Some(Desconhecido)] {
            assert!(papel_do_convidado_serve(Some(Teleprompter), convidado).is_err());
        }
        assert!(papel_do_convidado_serve(Some(ControleRemoto), Some(ControleRemoto)).is_err());

        assert!(papel_do_anfitriao_serve(None, None).is_ok());
        assert!(papel_do_anfitriao_serve(None, Some(Teleprompter)).is_err());
        assert!(papel_do_anfitriao_serve(Some(ControleRemoto), Some(Teleprompter)).is_ok());
        assert!(papel_do_anfitriao_serve(Some(ControleRemoto), None).is_err());
        assert!(papel_do_anfitriao_serve(Some(Teleprompter), Some(Teleprompter)).is_err());
    }

    #[test]
    fn tela_fora_da_faixa_nao_existe() {
        assert_eq!(Screen::nova(0, 1200), None);
        assert_eq!(Screen::nova(1920, 0), None);
        assert_eq!(Screen::nova(Screen::LADO_MAXIMO + 1, 1200), None);
        assert!(Screen::nova(Screen::LADO_MAXIMO, 1).is_some());
    }
}
