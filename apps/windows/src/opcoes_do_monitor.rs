//! Public startup options for the Monitor shell; bench and Studio flags stay private.

use crate::idioma::t;
use clap::{Arg, ArgAction, Command, CommandFactory, FromArgMatches, Parser};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "quall-monitor", version, disable_help_flag = true, disable_version_flag = true)]
pub struct Opcoes {
    #[arg(long)]
    pub registro: Option<PathBuf>,
    #[arg(long, default_value_t = 0)]
    pub porta: u16,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=60))]
    pub fps: u32,
    #[arg(long)]
    pub sem_som: bool,
}

impl Opcoes {
    pub fn comando() -> Command {
        Self::command()
            .about(t("Quall Monitor — até 8 telas estendidas pela rede local"))
            .override_usage(t("quall-monitor [OPÇÕES]"))
            .help_template(t("{about}\n\nUso: {usage}\n\n{all-args}"))
            .mut_arg("registro", |a| a.help(t("Arquivo do diário; o padrão pertence apenas ao Quall Monitor.")))
            .mut_arg("porta", |a| a.help(t("Porta da sinalização; zero escolhe uma porta livre.")))
            .mut_arg("fps", |a| a.help(t("Taxa alvo do vídeo, de 1 a 60 quadros por segundo.")))
            .mut_arg("sem_som", |a| a.help(t("Estender sem transmitir o som deste computador.")))
            .arg(Arg::new("help").short('h').long("help").action(ArgAction::Help).help(t("Mostrar ajuda")))
            .arg(Arg::new("version").short('V').long("version").action(ArgAction::Version).help(t("Mostrar versão")))
            .next_help_heading(t("Opções"))
            .mut_args(|a| a.help_heading(t("Opções")))
    }

    pub fn ler() -> Result<Self, clap::Error> {
        let matches = Self::comando().try_get_matches()?;
        Self::from_arg_matches(&matches)
    }

    #[cfg(feature = "net")]
    pub fn argumentos(self) -> crate::argumentos::Argumentos {
        let mut args = crate::argumentos::Argumentos::parse_from(["quall-monitor"]);
        args.registro = self.registro;
        args.porta = self.porta;
        args.fps = self.fps;
        args.sem_som = self.sem_som;
        // Each receiver gets its own display and teardown through the inherited coordinator.
        args.varias_sessoes = true;
        args.sem_cameras = true;
        args.sem_cameras_virtuais = true;
        args
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::idioma::{com_idioma, Idioma};

    #[test]
    fn ajuda_e_rotulos_seguem_o_idioma_escolhido() {
        for (idioma, uso, ajuda, sobre) in [
            (Idioma::Pt, "Uso:", "Mostrar ajuda", "até 8 telas"),
            (Idioma::En, "Usage:", "Show help", "up to 8 extended displays"),
        ] {
            com_idioma(idioma, || {
                let texto = Opcoes::comando().render_help().to_string();
                assert!(texto.contains(uso) && texto.contains(ajuda) && texto.contains(sobre), "{texto}");
            });
        }
    }

    #[test]
    fn a_casca_recusa_bandeiras_de_camera_e_bancada() {
        for flag in ["--camera-sintetica", "--fonte", "--pin", "--varias-sessoes", "--monitor-virtual"] {
            assert!(Opcoes::comando().try_get_matches_from(["quall-monitor", flag]).is_err(), "{flag}");
        }
        assert!(Opcoes::comando().try_get_matches_from(["quall-monitor", "--fps", "61"]).is_err());
        assert!(Opcoes::comando().try_get_matches_from(["quall-monitor", "--fps", "0"]).is_err());
    }

    #[cfg(feature = "net")]
    #[test]
    fn abertura_do_monitor_usa_oito_sessoes_e_porta_livre_sem_cameras() {
        let matches = Opcoes::comando().try_get_matches_from(["quall-monitor"]).unwrap();
        let args = Opcoes::from_arg_matches(&matches).unwrap().argumentos();
        assert!(args.varias_sessoes && args.sem_cameras && args.sem_cameras_virtuais);
        assert_eq!(args.porta, 0);
        assert!(args.pin.is_none() && args.fonte.is_none());
        assert_eq!(crate::sessoes::LIMITE_DE_SESSOES, 8);
    }
}
