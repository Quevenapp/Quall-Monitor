//! **A janela principal sem Win32**: o estado da tela em valores simples, e cada cena composta em
//! peças de `estilo.rs` e em lugares de controle (`docs/telas-estudio.md` §7 e §11.5).
//!
//! # Por que um módulo sem Windows
//!
//! A janela (`janela.rs`) monta um [`EstadoDaTela`] a partir do `Emissor` e do `Receptor` a cada
//! mudança de versão, e daí para a frente só lê dele: [`compor`] diz o que pintar e onde fica cada
//! controle nativo; [`aparencia`] diz como cada botão se desenha no `NM_CUSTOMDRAW`. O retrato de
//! bancada (`--retratos-de-bancada`) passa um estado de exemplo ([`exemplos`]) pelo mesmo caminho,
//! sem emissor nem receptor. E os testes daqui rodam em qualquer máquina: conferem que tudo cabe na
//! janela de 880 × 580, que nenhum controle cobre outro, e as frases.
//!
//! As posições vêm **todas** de `estilo::lugar` (a tabela única, §11.5).

use crate::estilo::{lugar, *};
use crate::idioma::{t, tf, tr};

// =============================================================================================
// Painéis, cenas e controles
// =============================================================================================

/// Os itens da barra lateral.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub enum Painel {
    #[default]
    Espelhar,
    Exibir,
    Teleprompter,
    Ajustes,
}

impl Painel {
    pub const TODOS: [Painel; 4] = [Painel::Espelhar, Painel::Exibir, Painel::Teleprompter, Painel::Ajustes];

    pub fn indice(self) -> usize {
        match self {
            Painel::Espelhar => 0,
            Painel::Exibir => 1,
            Painel::Teleprompter => 2,
            Painel::Ajustes => 3,
        }
    }

    pub fn rotulo(self) -> &'static str {
        match self {
            Painel::Espelhar => t("Estender"),
            Painel::Exibir => t("Exibir"),
            Painel::Teleprompter => t("Teleprompter"),
            Painel::Ajustes => t("Ajustes"),
        }
    }

    pub fn icone(self) -> Icone {
        match self {
            Painel::Espelhar => Icone::Espelhar,
            Painel::Exibir => Icone::Exibir,
            Painel::Teleprompter => Icone::Teleprompter,
            Painel::Ajustes => Icone::Ajustes,
        }
    }
}

/// O que o painel mostra: um dos quatro painéis, ou a tela de uma sessão de pé.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cena {
    Painel(Painel),
    Esperando,
    NoAr,
    Varios,
    Conectando,
    Exibindo,
}

impl Default for Cena {
    fn default() -> Cena {
        Cena::Painel(Painel::Espelhar)
    }
}

/// Os controles nativos da janela.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Controle {
    /// Um item da barra lateral (os quatro formam um grupo de opções).
    Item(Painel),
    /// Um ladrilho de origem (grupo de opções).
    Ladrilho(usize),
    /// "Mandar o som deste computador" (interruptor).
    Som,
    /// O microfone da câmera: a linha com interruptor em Espelhar, o redondo na sessão.
    Microfone,
    Espelhar,
    Lista,
    Endereco,
    Pin,
    Exibir,
    /// Um cartão do teleprompter: 0 mostrar o texto, 1 controlar, 2 texto com a câmera.
    Papel(usize),
    Esquecer,
    Diario,
    Licencas,
    Privacidade,
    Suporte,
    /// O letreiro do PIN: um `STATIC` desenhado, para o Narrador ler "PIN 4 8 2 7 1 9".
    Letreiro,
    /// O chip do endereço: copia.
    Chip,
    /// "Cancelar", ou o "Parar" (cheio, ou o redondo da câmera).
    Cancelar,
    Gravar,
    Mudo,
    SomComCamera,
    /// Um segmento do volume (grupo de opções): 100, 75, 50, 25 %.
    Volume(usize),
    /// "Detalhes": abre e fecha os números da exibição (interruptor).
    Detalhes,
    /// **Os ajustes da câmera** (R9, `docs/controles-de-camera.md` §4.1): o redondo da engrenagem
    /// (U+E713) na câmera pela espera e no ar; abre a janela "Ajustes da câmera".
    AjustesDaCamera,
    /// **A tela estendida sem o driver** (R10, 02/10): o ladrilho apagado, no lugar do escolhível,
    /// quando o adaptador do SudoVDA não está presente. É um botão, e não uma opção do grupo dos
    /// ladrilhos: não se escolhe (nem pelas setas). O clique abre a caixa de instalar o driver (02/10,
    /// noite), ou, na loja e no Windows sem suporte, as instruções do SudoVDA
    /// (`regras_do_driver::clique_no_apagado`).
    TelaEstendidaSemDriver,
    /// **"Desinstalar" o driver da tela estendida**, no cartão dele nos Ajustes: só com o driver que
    /// o Quall instalou.
    DriverDaTelaEstendida,
    /// **O seletor de idioma "PT | EN"** (a tradução, 02/10): um segmento por idioma (0 PT, 1 EN),
    /// num grupo de opções, no canto de cima à direita dos quatro painéis.
    Idioma(usize),
}

/// Que controle nativo cada um é.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Especie {
    /// `BS_PUSHBUTTON`, desenhado no `NM_CUSTOMDRAW`.
    Botao,
    /// `BS_AUTOCHECKBOX | BS_PUSHLIKE`: o estado é o do próprio controle (`BM_GETCHECK`).
    Alternar,
    /// `BS_AUTORADIOBUTTON | BS_PUSHLIKE`, num grupo.
    Opcao,
    /// `LISTBOX` com `LBS_OWNERDRAWFIXED | LBS_HASSTRINGS`.
    Lista,
    /// `EDIT`.
    Campo,
    /// `STATIC` desenhado (o letreiro).
    Letreiro,
}

pub const MAX_LADRILHOS: usize = 32;
pub const MAX_RECEPTORES: usize = 8;
/// Os volumes do segmentado, na ordem em que aparecem.
pub const VOLUMES: [(f32, &str); 4] = [(1.0, "100 %"), (0.75, "75 %"), (0.5, "50 %"), (0.25, "25 %")];
/// O aviso da espera enquanto o emissor desmonta a sessão (e o do Exibindo que acaba). Em português,
/// como chave da tabela: quem mostra passa por `t(ENCERRANDO)`. A bandeja reconhece o estado pelo
/// [`TelaEspera::encerrando`], e não por este texto (que muda com o idioma).
pub const ENCERRANDO: &str = "Encerrando…"; // i18n: chave
/// Os avisos do cartão do teleprompter (`janela::abrir_o_teleprompter`), em português: o cartão os
/// guarda assim, a janela os compara a cada pulso, e o [`compor`] os traduz.
pub const AVISO_FECHANDO_O_TELEPROMPTER: &str = "Fechando o teleprompter aberto para abrir no papel pedido…"; // i18n: chave
pub const AVISO_O_TELEPROMPTER_NAO_FECHOU: &str =
    "O teleprompter aberto não fechou em 45 s. Feche a janela dele e clique no cartão de novo."; // i18n: chave

impl Controle {
    /// O id do controle (o `HMENU` do filho, e o que chega no `WM_COMMAND`). Os que já existiam
    /// antes desta rodada mantêm o número.
    pub fn id(self) -> usize {
        match self {
            Controle::Espelhar => 102,
            Controle::Cancelar => 103,
            Controle::Esquecer => 104,
            Controle::Som => 105,
            Controle::Lista => 106,
            Controle::Endereco => 107,
            Controle::Pin => 108,
            Controle::Exibir => 109,
            Controle::Mudo => 111,
            Controle::SomComCamera => 113,
            Controle::Microfone => 114,
            Controle::Gravar => 115,
            Controle::AjustesDaCamera => 116,
            Controle::TelaEstendidaSemDriver => 117,
            Controle::Idioma(i) => 118 + i.min(1),
            Controle::Item(p) => 120 + p.indice(),
            Controle::Papel(i) => 130 + i,
            Controle::Diario => 140,
            Controle::Licencas => 145,
            Controle::Privacidade => 146,
            Controle::Suporte => 147,
            Controle::Chip => 141,
            Controle::Detalhes => 142,
            Controle::Letreiro => 143,
            Controle::DriverDaTelaEstendida => 144,
            Controle::Volume(i) => 150 + i,
            Controle::Ladrilho(i) => 200 + i,
        }
    }

    pub fn de_id(id: usize) -> Option<Controle> {
        Some(match id {
            102 => Controle::Espelhar,
            103 => Controle::Cancelar,
            104 => Controle::Esquecer,
            105 => Controle::Som,
            106 => Controle::Lista,
            107 => Controle::Endereco,
            108 => Controle::Pin,
            109 => Controle::Exibir,
            111 => Controle::Mudo,
            113 => Controle::SomComCamera,
            114 => Controle::Microfone,
            115 => Controle::Gravar,
            116 => Controle::AjustesDaCamera,
            117 => Controle::TelaEstendidaSemDriver,
            118 => Controle::Idioma(0),
            119 => Controle::Idioma(1),
            120..=123 => Controle::Item(Painel::TODOS[id - 120]),
            130..=132 => Controle::Papel(id - 130),
            140 => Controle::Diario,
            145 => Controle::Licencas,
            146 => Controle::Privacidade,
            147 => Controle::Suporte,
            141 => Controle::Chip,
            142 => Controle::Detalhes,
            143 => Controle::Letreiro,
            144 => Controle::DriverDaTelaEstendida,
            150..=153 => Controle::Volume(id - 150),
            i if (200..200 + MAX_LADRILHOS).contains(&i) => Controle::Ladrilho(i - 200),
            _ => return None,
        })
    }

    pub fn especie(self) -> Especie {
        match self {
            Controle::Item(_) | Controle::Ladrilho(_) | Controle::Volume(_) | Controle::Idioma(_) => Especie::Opcao,
            Controle::Som | Controle::Microfone | Controle::Mudo | Controle::SomComCamera | Controle::Detalhes => Especie::Alternar,
            Controle::Lista => Especie::Lista,
            Controle::Endereco | Controle::Pin => Especie::Campo,
            Controle::Letreiro => Especie::Letreiro,
            _ => Especie::Botao,
        }
    }

    /// Os controles fixos, na ordem de criação (que é a do Tab). Os ladrilhos entram logo depois dos
    /// itens da barra (a janela os põe no lugar da ordem quando os cria), e por isso antes da tela
    /// estendida apagada, que vem logo depois dos itens aqui.
    pub fn fixos() -> Vec<Controle> {
        let mut v: Vec<Controle> = [Painel::Espelhar, Painel::Exibir, Painel::Ajustes].iter().map(|p| Controle::Item(*p)).collect();
        #[cfg(feature = "tela-estendida-futura")]
        v.push(Controle::TelaEstendidaSemDriver);
        v.extend([Controle::Som, Controle::Espelhar, Controle::Lista, Controle::Endereco, Controle::Pin, Controle::Exibir]);

        // O chip antes do Esquecer e do microfone: é a ordem de cima para baixo da espera da câmera
        // (a janela põe o microfone depois do Diário quando a sessão é de câmera).
        v.extend([Controle::Letreiro, Controle::Chip, Controle::Esquecer, Controle::Diario, Controle::Licencas, Controle::Privacidade, Controle::Suporte, Controle::Mudo]);
        #[cfg(feature = "tela-estendida-futura")]
        v.push(Controle::DriverDaTelaEstendida);
        v.extend((0..4).map(Controle::Volume));
        v.extend([Controle::Detalhes, Controle::Cancelar]);
        v.extend([Controle::Idioma(0), Controle::Idioma(1)]);
        v
    }

    /// O primeiro de um grupo de opções (leva `WS_GROUP`; os seguintes do grupo, não).
    pub fn abre_grupo(self) -> bool {
        match self {
            Controle::Item(p) => p == Painel::Espelhar,
            Controle::Ladrilho(i) | Controle::Volume(i) | Controle::Idioma(i) => i == 0,
            _ => true,
        }
    }
}

/// Os destinos oficiais dos Ajustes, escolhidos pelo mesmo idioma da interface.
pub fn pagina_dos_ajustes(controle: Controle, idioma: crate::idioma::Idioma) -> Option<&'static str> {
    use crate::idioma::Idioma;
    Some(match (controle, idioma) {
        (Controle::Privacidade, Idioma::Pt) => "https://queven.com.br/quall/privacidade/",
        (Controle::Privacidade, Idioma::En) => "https://queven.com.br/en/quall/privacy/",
        (Controle::Suporte, Idioma::Pt) => "https://queven.com.br/quall/suporte/",
        (Controle::Suporte, Idioma::En) => "https://queven.com.br/en/quall/support/",
        _ => return None,
    })
}

// =============================================================================================
// O estado da tela
// =============================================================================================

/// O item da sessão de pé, na barra lateral.
#[derive(Clone, Debug, PartialEq)]
pub struct SessaoNaBarra {
    pub item: Painel,
    pub luz: Luz,
    pub rotulo: String,
    pub nota: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ladrilho {
    pub titulo: String,
    pub detalhe: String,
    pub detalhe_mono: bool,
    pub icone: Icone,
    pub escolhido: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Aviso {
    pub tom: Tom,
    pub texto: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelaEspelhar {
    pub fontes: Vec<Ladrilho>,
    /// A escolhida é uma câmera (a linha do interruptor é a do microfone).
    pub camera: bool,
    /// Há uma escolhida (sem escolha, não há linha de interruptor).
    pub alguma_escolhida: bool,
    pub som: bool,
    pub microfone: bool,
    /// O endereço deste computador na rede, sem a porta. `None`: sem rede.
    pub ip: Option<String>,
    pub aviso: Option<Aviso>,
    pub pode_espelhar: bool,
    /// **A tela estendida apagada** (R10): a casa dela na grade dos ladrilhos (logo depois dos
    /// monitores, `regras_da_tela_estendida::lugar_do_apagado`), ou `None` com o adaptador presente
    /// (aí a tela estendida é um ladrilho de `fontes`).
    pub tela_estendida_sem_driver: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinhaDeAparelho {
    pub nome: String,
    /// "Tela ou câmera", "Tela", "Câmera".
    pub tipo: String,
    pub endereco: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelaExibir {
    pub aparelhos: Vec<LinhaDeAparelho>,
    pub escolhido: Option<usize>,
    pub procurando: bool,
    /// A frase no lugar da lista vazia (ou o aviso da busca).
    pub vazio: String,
    pub pede_pin: bool,
    pub aviso: Option<Aviso>,
    /// Qual campo tem o foco (a borda de baixo violeta).
    pub foco: Option<Controle>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelaAjustes {
    /// Há pares no disco (`Estado::ha_pares_conhecidos`): só a frase depende disto.
    pub tem_pares: bool,
    /// O primeiro clique no Esquecer já foi dado: o segundo, em 5 s, esquece de verdade (§11.1).
    pub confirmando: bool,
    pub pasta_do_diario: String,
    pub versao: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelaEspera {
    pub pin: String,
    pub endereco: String,
    pub ha_pares: bool,
    pub nome: String,
    pub anunciando: bool,
    pub origem: String,
    /// Com o som do computador (a intenção). `None`: câmera (o som é o do microfone).
    pub com_som: Option<bool>,
    /// O que aconteceu com o som (a recusa do WASAPI, a linha do microfone).
    pub frase: String,
    /// "Encerrando…", ou o aviso da espera da câmera.
    pub aviso: String,
    pub aviso_ambar: bool,
    /// O emissor está desmontando a sessão (o aviso é o [`ENCERRANDO`]): a bandeja lê daqui, e não
    /// do texto, que muda com o idioma.
    pub encerrando: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ControlesDaCamera {
    pub microfone: bool,
    pub legenda_do_microfone: String,
    /// O Gravar existe (a câmera comum pelo dono).
    pub gravar: bool,
    pub gravar_ativo: bool,
    pub gravando: bool,
    pub legenda_do_gravar: String,
    /// O nome do Gravar para o Narrador: o rótulo de hoje ("■ Parar a gravação (1:23)"…).
    pub nome_do_gravar: String,
    pub legenda_do_parar: String,
    pub linha_da_gravacao: String,
    /// "Esquecer pareamentos" à mão na espera da câmera: a retomada falhou agora
    /// (`Estado::oferece_desparear`), como antes desta rodada (a dívida 22).
    pub esquecer: bool,
    /// **Os ajustes da câmera** (R9): a câmera comum pelo dono, de verdade. A fonte do Quall no
    /// processo (a sintética) e a câmera virtual do próprio Quall ficam sem (§2.2, pelo tipo).
    pub ajustes: bool,
    /// **R9b**: "Controlado por <aparelho>" (o nome; vazio fora dos 4 s depois de um pedido remoto).
    pub controlado_por: String,
    /// **Pouca luz** (§3.1 dos controles): o automático baixou o fps para clarear. Na linha da
    /// gravação: a primeira frase sozinha, a curta ao lado da gravação ou do "Controlado por".
    pub pouca_luz: Option<crate::regras_dos_controles::PoucaLuz>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelaNoAr {
    pub par: String,
    pub origem: String,
    pub imagem: String,
    pub rede: String,
    pub som: String,
    pub som_indo: bool,
    pub resumo: String,
    pub frase: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinhaDeReceptor {
    pub nome: String,
    pub monitor: String,
    pub resumo: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelaVarios {
    pub nome_da_espera: String,
    pub receptores: Vec<LinhaDeReceptor>,
    /// PIN e endereço da espera aberta para mais um.
    pub mais_um: Option<(String, String)>,
    /// Sem espera aberta: o limite de 8 (o texto de hoje) ou "Abrindo a espera…".
    pub mais_um_texto: String,
    pub rodape: String,
    pub rodape_aviso: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelaExibindo {
    pub par: String,
    /// A linha do som ("som: tocando, volume 100 %"…). Vazia sem track de som.
    pub som: String,
    /// Mudo, volume e "tocar mesmo com a câmera": só com a track de som (como hoje).
    pub controles_do_som: bool,
    pub mudo: bool,
    pub volume: usize,
    pub som_com_camera: bool,
    pub detalhes_abertos: bool,
    pub resumo: String,
    pub cadeia: String,
    pub alerta: bool,
    pub encerrando: bool,
    /// **R9b**: o aparelho que filma aceita o controle remoto da câmera (está `pronto` ou
    /// `nao_permitido`): a engrenagem "Ajustes da câmera" abre a janela dos ajustes da câmera dele.
    pub ajustes_da_camera: bool,
}

/// A janela do teleprompter aberta (a de janela própria, `teleprompter::papel_aberto`), e em que
/// papel. A janela principal não a desenha; a bandeja a diz, porque "Sair do Quall" a fecha junto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeleprompterAberto {
    /// A janela aberta na escolha do papel.
    Escolha,
    Prompter,
    Controle,
    /// O prompter com a câmera (a tela R5).
    PrompterComCamera,
}

/// **Tudo o que a janela desenha**, em valores simples.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EstadoDaTela {
    pub cena: Cena,
    /// O item escolhido na barra (com sessão de pé, o da sessão).
    pub painel: Painel,
    pub nome_do_aparelho: String,
    pub sessao: Option<SessaoNaBarra>,
    pub espelhar: TelaEspelhar,
    pub exibir: TelaExibir,
    pub ajustes: TelaAjustes,
    pub espera: TelaEspera,
    pub no_ar: TelaNoAr,
    pub varios: TelaVarios,
    pub conectando: String,
    pub exibindo: TelaExibindo,
    /// Os controles redondos da câmera pela espera, quando a sessão é de câmera.
    pub camera: Option<ControlesDaCamera>,
    /// O chip acabou de copiar.
    pub copiado: bool,
    /// O que o cartão do teleprompter está fazendo ou por que não abriu ("Fechando o teleprompter
    /// aberto…"), no painel Teleprompter. **Em português**, como o cartão o escreveu: a janela o
    /// compara a cada pulso com o de agora, e [`compor`] o traduz na hora de pintar (`tr`).
    pub aviso_do_teleprompter: String,
    /// A janela do teleprompter aberta, e o papel (`None` sem ela). Só a bandeja lê.
    pub teleprompter: Option<TeleprompterAberto>,
    /// A revisão da lista de origens que `espelhar.fontes` mostra (o clique num ladrilho vai com
    /// ela, e o emissor descarta o de uma lista velha), e a da lista de aparelhos.
    pub revisao_das_fontes: u64,
    pub revisao_dos_aparelhos: u64,
    /// O idioma da tela (o segmento aceso do seletor "PT | EN").
    pub idioma: crate::idioma::Idioma,
    /// **O driver da tela estendida** (02/10, noite): a situação neste computador e o que está
    /// acontecendo com ele (o ladrilho apagado, o aviso do Espelhar e o cartão dos Ajustes).
    #[cfg(feature = "tela-estendida-futura")]
    pub driver: TelaDoDriver,
}

/// O driver da tela estendida, na tela (`regras_do_driver`).
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg(feature = "tela-estendida-futura")]
pub struct TelaDoDriver {
    pub situacao: crate::regras_do_driver::Situacao,
    pub andamento: crate::regras_do_driver::Andamento,
}

/// **A frase do andamento** do driver, no idioma de agora, e o tom. `None` parado.
#[cfg(feature = "tela-estendida-futura")]
pub fn texto_do_andamento(a: &crate::regras_do_driver::Andamento) -> Option<(Tom, String)> {
    use crate::regras_do_driver::{self as rd, Andamento, Resultado, TomDoAviso};
    let (tom, chave) = rd::frase(a)?;
    let tom = match tom {
        TomDoAviso::Info => Tom::Info,
        TomDoAviso::Ambar => Tom::Ambar,
        TomDoAviso::Vermelho => Tom::Vermelho,
    };
    let texto = match a {
        Andamento::Rodando { acao, passo } if *passo > 0 => {
            let nome = rd::nome_do_passo(*acao, *passo).map(t).unwrap_or("");
            tf(chave, &[passo, &rd::passos(*acao).len(), &nome])
        }
        Andamento::Acabou { acao, resultado: Resultado::Falha(n, motivo, tecnico) } => {
            let passo = rd::nome_do_passo(*acao, *n).map(t).unwrap_or(t(rd::ANTES_DE_COMECAR));
            let motivo = if tecnico.is_empty() { tr(motivo) } else { format!("{} ({tecnico})", tr(motivo)) };
            tf(chave, &[&passo, &motivo])
        }
        _ => t(chave).to_string(),
    };
    Some((tom, texto))
}

/// O aviso do painel Espelhar: o da instalação do driver, quando há, vence o do emissor.
fn aviso_do_espelhar(e: &EstadoDaTela) -> Option<Aviso> {
    #[cfg(not(feature = "tela-estendida-futura"))]
    { return e.espelhar.aviso.clone(); }
    #[cfg(feature = "tela-estendida-futura")]
    {
    use crate::regras_do_driver::{Acao, Andamento};
    let do_driver = matches!(
        e.driver.andamento,
        Andamento::Rodando { acao: Acao::Instalar, .. } | Andamento::Acabou { acao: Acao::Instalar, .. }
    );
    match texto_do_andamento(&e.driver.andamento) {
        Some((tom, texto)) if do_driver => Some(Aviso { tom, texto }),
        _ => e.espelhar.aviso.clone(),
    }
    }
}

// =============================================================================================
// As frases
// =============================================================================================

/// A label from a previous wait must never be shown next to the next wait's PIN/address.
pub fn rotulo_da_espera(porta: Option<u16>, anunciado: Option<&(u16, String)>) -> Option<&str> {
    anunciado.filter(|(p, nome)| Some(*p) == porta && !nome.is_empty()).map(|(_, nome)| nome.as_str())
}

/// A instrução da espera (§6.4 com a emenda da §11.5): o nome e "Exibir" em destaque, e a origem.
pub fn instrucao_da_espera(e: &TelaEspera) -> Vec<Trecho> {
    // Cada pedaço é uma chave da tabela: a ordem das partes é a mesma nos dois idiomas.
    let mut v = vec![Trecho::simples(t("No outro aparelho, abra o Quall em ")), Trecho::forte(t("Exibir"))];
    if e.anunciando {
        v.push(Trecho::simples(t(" e escolha ")));
        v.push(Trecho::forte(e.nome.clone()));
        v.push(Trecho::simples(t(" na lista, ou digite o endereço abaixo.")));
    } else {
        v.push(Trecho::simples(t(" e digite o endereço abaixo.")));
    }
    if !e.origem.is_empty() {
        match e.com_som {
            None => {
                v.push(Trecho::simples(t(" Vai a câmera ")));
                v.push(Trecho::forte(e.origem.clone()));
                v.push(Trecho::simples("."));
            }
            Some(com) => {
                v.push(Trecho::simples(t(" Vai ")));
                v.push(Trecho::forte(e.origem.clone()));
                v.push(Trecho::simples(if com { t(", com o som deste computador.") } else { t(", sem som.") }));
            }
        }
    }
    v
}

/// **A legenda curta do microfone redondo** (§6.5): "Mic desligado", "Mic ligado", "Ligando…",
/// "Sem acesso", "Não abriu". A frase longa de hoje continua na linha de baixo e no Narrador.
/// `linha` é a linha do microfone da sessão (`Emissor::linha_do_microfone`), `None` antes de a
/// sessão abrir o microfone; `sem_acesso`, a frase da privacidade do Windows.
///
/// A `linha` chega **em português** (é o estado do emissor, comparado aqui); a legenda sai no
/// idioma de agora.
pub fn legenda_do_microfone(pedido: bool, linha: Option<&str>, sem_acesso: bool) -> &'static str {
    if sem_acesso {
        return t("Sem acesso");
    }
    if !pedido {
        return t("Mic desligado");
    }
    match linha {
        None | Some("Com o som do microfone.") => t("Mic ligado"), // i18n: fora (o estado do emissor)
        Some("Microfone abrindo…") => t("Ligando…"), // i18n: fora (o estado do emissor)
        Some(l) if l.starts_with("Microfone desligado") => t("Mic desligado"), // i18n: fora (o estado do emissor)
        Some(_) => t("Não abriu"),
    }
}

/// **A legenda do Gravar redondo** (§6.5), a partir do rótulo de hoje (`Estado::rotulo_gravar`,
/// que continua sendo o nome do botão para o Narrador): gravando, o tempo ("1:23", com "· SEM SOM"
/// quando a linha da gravação diz); abrindo e fechando o arquivo, o que está acontecendo; senão,
/// "Gravar".
///
/// O `rotulo` de abrir e de fechar chega **em português** (o emissor o deixa assim para esta
/// comparação); o de gravar e a `linha` chegam no idioma da hora (o tempo entre parênteses e o
/// marcador do "SEM SOM" valem nos dois). A legenda sai no idioma de agora.
pub fn legenda_do_gravar(rotulo: &str, gravando: bool, linha: &str) -> String {
    if rotulo.contains("abrindo") {
        return t("Abrindo…").into();
    }
    // i18n: fora (o rótulo do emissor, comparado em português)
    if rotulo.starts_with("Fechando") {
        return t("Fechando…").into();
    }
    if !gravando {
        return t("Gravar").into();
    }
    let tempo = rotulo.rsplit_once('(').and_then(|(_, r)| r.strip_suffix(')')).filter(|x| x.contains(':')).unwrap_or("");
    let base = if tempo.is_empty() { t("Gravando").to_string() } else { tempo.to_string() };
    // A linha da gravação nasce no idioma da hora: o marcador "SEM SOM" vale nos dois.
    if crate::regras_da_gravacao::diz_sem_som(linha) {
        tf("{} · SEM SOM", &[&base])
    } else {
        base
    }
}

/// O PIN em dois grupos de três: "482 719".
pub fn pin_em_grupos(pin: &str) -> String {
    let c: Vec<char> = pin.chars().collect();
    if c.len() == 6 {
        format!("{} {}", c[..3].iter().collect::<String>(), c[3..].iter().collect::<String>())
    } else {
        pin.to_string()
    }
}

/// A acessibilidade do letreiro (§11.1): "PIN 4 8 2 7 1 9".
pub fn pin_para_ler(pin: &str) -> String {
    let digitos: Vec<String> = pin.chars().map(|c| c.to_string()).collect();
    format!("PIN {}", digitos.join(" "))
}

/// O volume do segmentado mais perto do volume pedido.
pub fn indice_do_volume(v: f32) -> usize {
    VOLUMES
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 .0 - v).abs().total_cmp(&(b.1 .0 - v).abs()))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// A pasta do diário para a tela: `%LOCALAPPDATA%` no lugar do caminho da conta.
pub fn pasta_curta(pasta: &str, localappdata: Option<&str>) -> String {
    if let Some(base) = localappdata.filter(|b| !b.is_empty()) {
        if let Some(resto) = pasta.strip_prefix(base) {
            return format!("%LOCALAPPDATA%{resto}");
        }
    }
    pasta.to_string()
}

/// O endereço sem a porta (o cartão "Rede" do no ar, a linha "Na rede").
pub fn sem_porta(endereco: &str) -> String {
    match endereco.rsplit_once(':') {
        Some((ip, porta)) if porta.chars().all(|c| c.is_ascii_digit()) && !ip.ends_with(']') && !ip.contains(':') => ip.to_string(),
        Some((ip, porta)) if porta.chars().all(|c| c.is_ascii_digit()) && ip.starts_with('[') && ip.ends_with(']') => ip.to_string(),
        _ => endereco.to_string(),
    }
}

// =============================================================================================
// A composição
// =============================================================================================

/// O que pintar (em coordenadas da área de cliente, DIP) e onde fica cada controle visível.
#[derive(Clone, Debug, Default)]
pub struct Quadro {
    pub itens: Vec<Item>,
    pub controles: Vec<(Controle, Ret)>,
}

impl Quadro {
    fn mais(&mut self, i: Item) {
        self.itens.push(i);
    }
    fn varios(&mut self, v: Vec<Item>) {
        self.itens.extend(v);
    }
    fn controle(&mut self, c: Controle, r: Ret) {
        self.controles.push((c, r));
    }
    pub fn lugar(&self, c: Controle) -> Option<Ret> {
        self.controles.iter().find(|(x, _)| *x == c).map(|(_, r)| *r)
    }
    /// Os textos pintados, corridos (para os testes lerem a tela).
    #[cfg(test)]
    pub fn textos(&self) -> Vec<String> {
        self.itens.iter().filter_map(|i| if let Item::Texto(t) = i { Some(t.texto_corrido()) } else { None }).collect()
    }
}

/// O controle está habilitado? (Visível é estar no [`Quadro`].)
pub fn habilitado(c: Controle, e: &EstadoDaTela) -> bool {
    match c {
        Controle::Item(p) => e.sessao.as_ref().is_none_or(|s| s.item == p),
        Controle::Espelhar => e.espelhar.pode_espelhar,
        Controle::Gravar => e.camera.as_ref().is_some_and(|c| c.gravar_ativo),
        #[cfg(feature = "tela-estendida-futura")]
        Controle::DriverDaTelaEstendida => crate::regras_do_driver::pode_comecar(&e.driver.andamento),
        #[cfg(not(feature = "tela-estendida-futura"))]
        Controle::DriverDaTelaEstendida | Controle::TelaEstendidaSemDriver => false,
        _ => true,
    }
}

/// **Compõe a janela** no estado dado. A janela é fixa (880 × 580 DIP), e os lugares são os de
/// `estilo::lugar`.
pub fn compor(e: &EstadoDaTela) -> Quadro {
    let mut q = Quadro::default();
    // O fundo: a barra, e o painel com o canto de cima à esquerda redondo e o contorno fino.
    q.mais(caixa(Ret::new(0.0, 0.0, LARGURA_MINIMA, ALTURA_MINIMA), 0.0, BARRA));
    let p = lugar::PAINEL;
    q.mais(caixa_com_borda(Ret::new(p.x, p.y, p.l + 16.0, p.a + 16.0), RAIO_DO_CARTAO, FUNDO, Cor::rgba(0xFFFFFF, 15), 1.0));
    barra(&mut q, e);
    // O seletor de idioma, no canto de cima à direita dos quatro painéis (não nas telas de sessão).
    if matches!(e.cena, Cena::Painel(_)) {
        q.mais(caixa(lugar::IDIOMA, 6.0, SUPERFICIE_ALTA));
        for (i, r) in lugar::segmentos_do_idioma().iter().enumerate() {
            q.controle(Controle::Idioma(i), *r);
        }
    }
    match e.cena {
        Cena::Painel(Painel::Espelhar) => painel_espelhar(&mut q, e),
        Cena::Painel(Painel::Exibir) => painel_exibir(&mut q, e),
        Cena::Painel(Painel::Teleprompter) => painel_teleprompter(&mut q, e),
        Cena::Painel(Painel::Ajustes) => painel_ajustes(&mut q, e),
        Cena::Esperando => cena_esperando(&mut q, e),
        Cena::NoAr => cena_no_ar(&mut q, e),
        Cena::Varios => cena_varios(&mut q, e),
        Cena::Conectando => cena_conectando(&mut q, e),
        Cena::Exibindo => cena_exibindo(&mut q, e),
    }
    q
}

fn barra(q: &mut Quadro, e: &EstadoDaTela) {
    q.varios(marca(lugar::MARCA_X, lugar::MARCA_Y, lugar::MARCA_LADO));
    q.mais(texto(lugar::MARCA_TEXTO, "Quall Monitor", F_MARCA, TEXTO).meio().item());
    for (i, painel) in [Painel::Espelhar, Painel::Exibir].iter().enumerate() {
        q.controle(Controle::Item(*painel), lugar::ITENS[i]);
    }
    if let Some(s) = &e.sessao {
        q.mais(texto(lugar::NOTA_DA_SESSAO, s.nota.clone(), F_LEGENDA, TEXTO3).quebra().item());
    }
    q.mais(texto(lugar::NOME_ROTULO, t("Nome deste computador"), F_LEGENDA_11, TEXTO3).meio().item());
    q.mais(texto(lugar::NOME, e.nome_do_aparelho.clone(), F_CORPO_FORTE, TEXTO).meio().item());
    q.controle(Controle::Item(Painel::Ajustes), lugar::ITEM_AJUSTES);
}

fn cabecalho(q: &mut Quadro, titulo_: &str, explicacao: &str) {
    q.mais(titulo(lugar::TITULO, titulo_));
    q.mais(texto(lugar::SUBTITULO, explicacao, F_CORPO, TEXTO2).quebra().item());
}

fn painel_espelhar(q: &mut Quadro, e: &EstadoDaTela) {
    let s = &e.espelhar;
    #[cfg(feature = "tela-estendida-futura")]
    let explicacao = t("Uma tela, a tela estendida ou uma câmera. Quem exibe escolhe este computador na lista.");
    #[cfg(not(feature = "tela-estendida-futura"))]
    let explicacao = t("Uma tela existente ou uma câmera. Quem exibe escolhe este computador na lista.");
    let _ = explicacao;
    cabecalho(q, t("Estender a área de trabalho"), t("Até 8 aparelhos, cada um com um monitor. No outro aparelho, escolha Exibir."));
    let com_interruptor = s.alguma_escolhida;
    let aviso_ = aviso_do_espelhar(e);
    let altura_do_aviso = aviso_.as_ref().map(|a| altura_do_aviso(&a.texto, lugar::L));
    let n = s.fontes.len().min(MAX_LADRILHOS);
    // A tela estendida apagada ocupa uma casa da grade, e os ladrilhos depois dela andam uma (R10).
    #[cfg(feature = "tela-estendida-futura")]
    let apagado = s.tela_estendida_sem_driver.map(|a| a.min(n));
    #[cfg(not(feature = "tela-estendida-futura"))]
    let apagado: Option<usize> = None;
    let casas = n + usize::from(apagado.is_some());
    let (ladrilhos, interruptor, aviso_r) = lugar::espelhar(casas, com_interruptor, altura_do_aviso);
    for i in 0..n {
        #[cfg(feature = "tela-estendida-futura")]
        let casa = crate::regras_da_tela_estendida::casa_do_ladrilho(i, apagado);
        #[cfg(not(feature = "tela-estendida-futura"))]
        let casa = i;
        q.controle(Controle::Ladrilho(i), ladrilhos[casa]);
    }
    if let Some(a) = apagado {
        q.controle(Controle::TelaEstendidaSemDriver, ladrilhos[a]);
    }
    if casas == 0 {
        q.mais(texto(lugar::SEM_FONTES, t("Nenhum monitor foi encontrado."), F_CORPO, AGUARDANDO_TEXTO).meio().item());
    }
    if let Some(r) = interruptor {
        q.controle(if s.camera { Controle::Microfone } else { Controle::Som }, r);
    }
    if let (Some(a), Some(r)) = (&aviso_, aviso_r) {
        q.varios(aviso(r, a.tom, &a.texto));
    }
    rede(q, lugar::RODAPE, s.ip.as_deref());
    q.controle(Controle::Espelhar, lugar::BOTAO_PRINCIPAL);
}

/// A linha da rede: bolinha verde e "Na rede · {ip}" (mono no ip), ou âmbar e "Sem rede".
fn rede(q: &mut Quadro, r: Ret, ip: Option<&str>) {
    let cy = r.y + r.a / 2.0;
    match ip {
        Some(ip) => {
            q.mais(Item::Circulo { cx: r.x + 4.0, cy, raio: 4.0, cor: CONECTADO });
            q.mais(rico(Ret::new(r.x + 16.0, r.y, r.l - 16.0, r.a), vec![Trecho::simples(t("Na rede · ")), Trecho::mono(ip)], F_LEGENDA_13, TEXTO2).meio().item());
        }
        None => {
            q.mais(Item::Circulo { cx: r.x + 4.0, cy, raio: 4.0, cor: AGUARDANDO });
            q.mais(texto(Ret::new(r.x + 16.0, r.y, r.l - 16.0, r.a), t("Sem rede"), F_LEGENDA_13, AGUARDANDO_TEXTO).meio().item());
        }
    }
}

fn painel_exibir(q: &mut Quadro, e: &EstadoDaTela) {
    let s = &e.exibir;
    cabecalho(q, t("Assistir outro aparelho"), t("Escolha quem está espelhando. Na primeira vez, digite o PIN que aparece lá."));
    q.mais(rotulo(lugar::ROTULO_DA_LISTA, t("Na rede agora")));
    if s.procurando {
        let r = lugar::PROCURANDO;
        // A bolinha violeta e "procurando" (§6.6), encostados à direita.
        let lt = 80.0;
        q.mais(Item::Circulo { cx: r.direita() - lt - 9.0, cy: r.y + r.a / 2.0, raio: 5.0, cor: TRACO_ESCOLHIDO.vezes(0.3) });
        q.mais(Item::Circulo { cx: r.direita() - lt - 9.0, cy: r.y + r.a / 2.0, raio: 3.0, cor: TRACO_ESCOLHIDO });
        q.mais(texto(Ret::new(r.direita() - lt, r.y, lt, r.a), t("procurando"), F_LEGENDA, TEXTO2).direita().meio().item());
    }
    if s.aparelhos.is_empty() {
        let r = lugar::LISTA_VAZIA;
        q.mais(cartao(r));
        q.mais(texto(r.dentro(16.0, 0.0), s.vazio.clone(), F_LEGENDA_13, TEXTO2).meio().item());
    } else {
        q.controle(Controle::Lista, lugar::LISTA);
    }
    q.mais(rotulo(lugar::ROTULO_DO_ENDERECO, t("Ou digite o endereço")));
    let r_pin = lugar::ROTULO_DO_PIN;
    q.mais(texto(r_pin, caixa_alta("PIN"), F_ROTULO, if s.pede_pin { ACENTO_CLARO } else { TEXTO3 }).meio().item());
    q.varios(campo(lugar::CAMPO_DO_ENDERECO, s.foco == Some(Controle::Endereco)));
    q.varios(campo(lugar::CAMPO_DO_PIN, s.foco == Some(Controle::Pin) || s.pede_pin));
    q.controle(Controle::Endereco, lugar::edit_no_campo(lugar::CAMPO_DO_ENDERECO));
    q.controle(Controle::Pin, lugar::edit_no_campo(lugar::CAMPO_DO_PIN));
    q.mais(texto(lugar::LEGENDA_DO_PIN, t("Deixe o PIN vazio se os dois já parearam."), F_LEGENDA, if s.pede_pin { ACENTO_CLARO } else { TEXTO3 }).meio().item());
    if let Some(a) = &s.aviso {
        let altura = altura_do_aviso(&a.texto, lugar::L).min(lugar::PE_Y - 12.0 - lugar::AVISO_DO_EXIBIR_Y);
        q.varios(aviso(Ret::new(lugar::X, lugar::AVISO_DO_EXIBIR_Y, lugar::L, altura), a.tom, &a.texto));
    }
    q.controle(Controle::Exibir, lugar::BOTAO_PRINCIPAL);
}

/// Os três cartões do teleprompter: ícone, título e explicação (§7.3). Em português, como chaves da
/// tabela: quem mostra passa por `t()`.
pub const PAPEIS: [(Icone, &str, &str); 3] = [
    (Icone::Teleprompter, "Mostrar o texto", "Este computador vira o prompter e espera o controle."), // i18n: chave
    (Icone::Controlar, "Controlar", "Comanda o texto que outro aparelho está mostrando."), // i18n: chave
    (Icone::Rosto, "Texto com a câmera", "O roteiro do lado da webcam. Só a câmera vai para a rede."), // i18n: chave
];

fn painel_teleprompter(q: &mut Quadro, e: &EstadoDaTela) {
    cabecalho(q, t("Teleprompter"), t("Um computador mostra o texto atrás do vidro; outro aparelho controla. O roteiro se edita dos dois lados."));
    let cartoes = lugar::cartoes_do_teleprompter();
    for (i, r) in cartoes.iter().enumerate() {
        q.controle(Controle::Papel(i), *r);
    }
    if !e.aviso_do_teleprompter.is_empty() {
        // O aviso do cartão está em português (é comparado pela janela a cada pulso): traduzido aqui.
        let dito = tr(&e.aviso_do_teleprompter);
        let y = cartoes[0].baixo() + 16.0;
        let a = altura_do_aviso(&dito, lugar::L);
        q.varios(aviso(Ret::new(lugar::X, y, lugar::L, a), Tom::Ambar, &dito));
    }
    q.mais(
        texto(lugar::NOTA_DO_TELEPROMPTER, t("Janela própria; tela cheia pelo botão da barra ou F11 (Esc sai); H esconde as faixas."), F_LEGENDA, TEXTO3)
            .meio()
            .item(),
    );
}

fn painel_ajustes(q: &mut Quadro, e: &EstadoDaTela) {
    let s = &e.ajustes;
    q.mais(titulo(lugar::TITULO, t("Ajustes")));
    let linhas = lugar::LINHAS_DOS_AJUSTES;
    let mut conteudo = vec![
        (
            t("Aparelhos pareados"),
            if s.confirmando {
                t("Clique de novo para esquecer todos.")
            } else if s.tem_pares {
                t("Os aparelhos pareados entram sem PIN.")
            } else {
                t("Nenhum aparelho pareado.")
            }
            .to_string(),
            false,
            lugar::ESQUECER.l,
        ),
        (t("Diário"), s.pasta_do_diario.clone(), true, lugar::DIARIO.l),
        (t("Versão"), s.versao.clone(), true, lugar::AJUSTES_LARGURA_DOS_LINKS),
    ];
    #[cfg(feature = "tela-estendida-futura")]
    conteudo.push((t(crate::regras_do_driver::AJUSTES_NOME), frase_do_driver(e).1, false, botao_do_driver(e).map_or(0.0, |(_, r)| r.l)));
    for (i, (r, (nome, d, mono, botao))) in linhas.iter().zip(conteudo).enumerate() {
        q.mais(cartao(*r));
        let largura = r.l - 32.0 - if botao > 0.0 { botao + 12.0 } else { 0.0 };
        q.mais(texto(Ret::new(r.x + 16.0, r.y + 11.0, largura, 20.0), nome, F_CORPO_FORTE, TEXTO).meio().item());
        // A primeira linha é a dos aparelhos pareados, e a quarta a do driver (pela posição: o texto
        // muda com o idioma).
        #[cfg(feature = "tela-estendida-futura")]
        let driver_vermelho = i == 3 && frase_do_driver(e).0 == Tom::Vermelho;
        #[cfg(not(feature = "tela-estendida-futura"))]
        let driver_vermelho = false;
        let cor = if (s.confirmando && i == 0) || driver_vermelho { PERIGO_TEXTO } else { TEXTO2 };
        if i == 3 {
            // O driver: a frase pode ter três linhas (o SudoVDA de outro programa, 02/10, noite).
            q.mais(texto(Ret::new(r.x + 16.0, r.y + 32.0, largura, r.a - 40.0), d, F_LEGENDA, cor).quebra().item());
        } else {
            q.mais(texto(Ret::new(r.x + 16.0, r.y + 32.0, largura, 18.0), d, if mono { F_MONO_12 } else { F_LEGENDA }, cor).meio().item());
        }
    }
    q.controle(Controle::Esquecer, lugar::ESQUECER);
    q.controle(Controle::Diario, lugar::DIARIO);
    q.controle(Controle::Licencas, lugar::LICENCAS);
    q.controle(Controle::Privacidade, lugar::PRIVACIDADE);
    q.controle(Controle::Suporte, lugar::SUPORTE);
    #[cfg(feature = "tela-estendida-futura")]
    if let Some((_, r)) = botao_do_driver(e) {
        q.controle(Controle::DriverDaTelaEstendida, r);
    }
}

/// O botão do cartão do driver nos Ajustes, e o lugar dele: "Desinstalar" com o driver que o Quall
/// instalou; na loja, com o SudoVDA presente, "Desinstalar pelo instalador do driver" (abre a página,
/// mais largo). Nunca no meio de uma instalação ou desinstalação (`regras_do_driver::botao_dos_ajustes`).
#[cfg(feature = "tela-estendida-futura")]
fn botao_do_driver(e: &EstadoDaTela) -> Option<(crate::regras_do_driver::BotaoDosAjustes, Ret)> {
    use crate::regras_do_driver::{botao_dos_ajustes, pode_comecar, BotaoDosAjustes};
    if !pode_comecar(&e.driver.andamento) {
        return None;
    }
    botao_dos_ajustes(e.driver.situacao).map(|b| {
        let r = match b {
            BotaoDosAjustes::Desinstalar => lugar::DRIVER,
            BotaoDosAjustes::PaginaDoInstalador => lugar::DRIVER_LARGO,
        };
        (b, r)
    })
}

/// A frase do cartão do driver: o andamento, quando há; senão, a situação.
#[cfg(feature = "tela-estendida-futura")]
fn frase_do_driver(e: &EstadoDaTela) -> (Tom, String) {
    texto_do_andamento(&e.driver.andamento)
        .unwrap_or_else(|| (Tom::Info, t(crate::regras_do_driver::frase_dos_ajustes(e.driver.situacao)).to_string()))
}

fn cena_esperando(q: &mut Quadro, e: &EstadoDaTela) {
    let s = &e.espera;
    q.mais(Item::Pilula { ancora: Ancora::Centro(lugar::CX), y: lugar::PILULA_Y, luz: Luz::Aguardando, texto: t("Aguardando").into() });
    q.mais(texto(lugar::TITULO_DA_SESSAO, t("Pronto para estender"), F_TITULO_GRANDE, TEXTO).centro().meio().item());
    q.mais(rico(lugar::INSTRUCAO, instrucao_da_espera(s), F_CORPO, TEXTO2).centro().quebra().item());
    let chip_l = largura_do_chip(&s.endereco);
    if !s.ha_pares {
        q.mais(texto(lugar::ROTULO_DO_PIN_DA_ESPERA, caixa_alta(t("Na primeira vez, o PIN")), F_ROTULO, TEXTO3).centro().meio().item());
        q.controle(Controle::Letreiro, lugar::letreiro());
        let (rotulo_r, chip_r) = lugar::chip(chip_l, true, lugar::CHIP_Y);
        if let Some(r) = rotulo_r {
            q.mais(texto(r, t("ou pelo endereço"), F_LEGENDA_13, TEXTO3).direita().meio().item());
        }
        q.controle(Controle::Chip, chip_r);
    } else {
        q.mais(texto(lugar::FRASE_DOS_PAREADOS, t("Aparelhos pareados entram direto."), F_CORPO_15, TEXTO).centro().meio().item());
        let (_, chip_r) = lugar::chip(chip_l, false, lugar::CHIP_Y_COM_PARES);
        q.controle(Controle::Chip, chip_r);
        q.mais(
            rico(lugar::PIN_NOVO, vec![Trecho::simples(t("Aparelho novo? PIN ")), Trecho::mono(pin_em_grupos(&s.pin))], F_LEGENDA_13, TEXTO2)
                .centro()
                .meio()
                .item(),
        );
    }
    if !s.frase.is_empty() {
        q.mais(texto(lugar::FRASE_DO_SOM, s.frase.clone(), F_LEGENDA_13, TEXTO3).centro().meio().item());
    }
    if !s.aviso.is_empty() {
        q.mais(texto(lugar::AVISO_DA_ESPERA, s.aviso.clone(), F_LEGENDA_13, if s.aviso_ambar { AGUARDANDO_TEXTO } else { TEXTO3 }).centro().meio().item());
    }
    match &e.camera {
        Some(c) => controles_da_camera(q, c),
        None => {
            let frase = if s.anunciando {
                t("Anunciando na rede — este nome aparece na lista dos outros aparelhos.")
            } else {
                t("Sem anúncio na rede: use o endereço acima.")
            };
            q.mais(texto(lugar::RODAPE_DA_SESSAO, frase, F_LEGENDA, TEXTO3).meio().item());
            q.controle(Controle::Cancelar, lugar::CANCELAR);
        }
    }
}

/// A câmera pela espera e no ar: a linha da gravação e os três redondos (microfone, Gravar, Parar).
fn controles_da_camera(q: &mut Quadro, c: &ControlesDaCamera) {
    // R9b: "Controlado por <aparelho>" divide a linha da gravação (a gravação primeiro).
    let controlado = if c.controlado_por.is_empty() { String::new() } else { tf("Controlado por {}", &[&c.controlado_por]) };
    let mut linha = match (c.linha_da_gravacao.is_empty(), controlado.is_empty()) {
        (false, false) => format!("{} · {controlado}", c.linha_da_gravacao),
        (false, true) => c.linha_da_gravacao.clone(),
        (true, _) => controlado,
    };
    // Pouca luz (§3.1) por último. A frase inteira (com o conselho) não cabe numa linha do painel:
    // sozinha vai a primeira frase, ao lado de outra coisa a curta ("Pouca luz: 15 fps"), e a
    // inteira fica na janela dos ajustes e na faixa da R5.
    if let Some(p) = &c.pouca_luz {
        linha = if linha.is_empty() { p.sem_conselho() } else { format!("{linha} · {}", p.curto()) };
    }
    if !linha.is_empty() {
        let cor = if c.gravando {
            PERIGO_TEXTO
        } else if c.linha_da_gravacao.is_empty() {
            AGUARDANDO_TEXTO
        } else {
            TEXTO2
        };
        q.mais(texto(lugar::LINHA_DA_GRAVACAO, linha, F_LEGENDA_13, cor).centro().meio().item());
    }
    if c.esquecer {
        q.controle(Controle::Esquecer, lugar::ESQUECER_NA_CAMERA);
    }
    let [m, g, p] = lugar::redondos();
    q.controle(Controle::Microfone, m);
    if c.gravar {
        q.controle(Controle::Gravar, g);
    }
    q.controle(Controle::Cancelar, p);
    if c.ajustes {
        q.controle(Controle::AjustesDaCamera, lugar::AJUSTES_DA_CAMERA);
    }
}

fn cena_no_ar(q: &mut Quadro, e: &EstadoDaTela) {
    let s = &e.no_ar;
    q.mais(Item::Pilula { ancora: Ancora::Esquerda(lugar::X), y: lugar::PILULA_Y, luz: Luz::NoAr, texto: t("No ar").into() });
    q.mais(texto(lugar::ESPELHANDO_PARA, t("Estendendo para"), F_CORPO, TEXTO2).meio().item());
    let par = if s.par.is_empty() { t("o outro aparelho").to_string() } else { s.par.clone() };
    q.mais(texto(lugar::PAR, par, F_PAR, TEXTO).meio().item());
    let cartoes = lugar::cartoes_de_numero();
    q.varios(cartao_de_numero(cartoes[0], t("Origem"), &s.origem, false, TEXTO));
    q.varios(cartao_de_numero(cartoes[1], t("Imagem"), &s.imagem, true, TEXTO));
    q.varios(cartao_de_numero(cartoes[2], t("Rede"), &s.rede, true, TEXTO));
    q.varios(cartao_de_numero(cartoes[3], t("Som"), &s.som, false, if s.som_indo { CONECTADO_TEXTO } else { TEXTO2 }));
    if !s.resumo.is_empty() {
        q.mais(texto(lugar::RESUMO_NO_AR, s.resumo.clone(), F_MONO_12, TEXTO3).meio().item());
    }
    q.mais(texto(lugar::FRASE_DO_PARAR, t("Parar encerra a sessão nos dois lados."), F_LEGENDA_13, TEXTO2).meio().item());
    if !s.frase.is_empty() {
        q.mais(texto(lugar::FRASE_DO_SOM_NO_AR, s.frase.clone(), F_LEGENDA_13, TEXTO3).meio().item());
    }
    match &e.camera {
        Some(c) => controles_da_camera(q, c),
        None => q.controle(Controle::Cancelar, lugar::PARAR),
    }
}

fn cena_varios(q: &mut Quadro, e: &EstadoDaTela) {
    let s = &e.varios;
    q.mais(Item::Pilula { ancora: Ancora::Esquerda(lugar::X), y: lugar::PILULA_Y, luz: Luz::NoAr, texto: t("No ar").into() });
    let n = s.receptores.len();
    let titulo_ = if n == 1 { t("Estendendo para 1 aparelho").to_string() } else { tf("Estendendo para {} aparelhos", &[&n]) };
    q.mais(texto(lugar::TITULO_DOS_VARIOS, titulo_, F_TITULO, TEXTO).meio().item());
    let linhas = lugar::linhas_dos_varios(n.min(MAX_RECEPTORES));
    for (r, x) in linhas.iter().zip(&s.receptores) {
        q.mais(cartao(*r));
        let lado = (r.a - 8.0).clamp(18.0, 28.0);
        let q_icone = Ret::new(r.x + 12.0, r.y + (r.a - lado) / 2.0, lado, lado);
        q.mais(caixa(q_icone, 6.0, ACENTO_FUNDO));
        q.mais(Item::Icone { ret: q_icone, icone: Icone::Monitor, tamanho: 15.0, cor: ACENTO_CLARO });
        let xt = q_icone.direita() + 12.0;
        let lt = r.direita() - 14.0 - xt;
        let duas = r.a >= 40.0 && !x.resumo.is_empty();
        let topo = if duas { Ret::new(xt, r.y + 4.0, lt, 20.0) } else { Ret::new(xt, r.y, lt, r.a) };
        q.mais(
            rico(topo, vec![Trecho::forte(x.nome.clone()), Trecho { texto: format!("  {}", x.monitor), cor: Some(TEXTO2), peso: None, familia: None, tamanho: Some(12.0) }], F_CORPO_FORTE, TEXTO)
                .meio()
                .item(),
        );
        if duas {
            q.mais(texto(Ret::new(xt, r.y + 24.0, lt, 16.0), x.resumo.clone(), F_MONO_11, TEXTO3).meio().item());
        }
    }
    if !s.rodape.is_empty() {
        q.mais(texto(lugar::RODAPE_DOS_VARIOS, s.rodape.clone(), F_LEGENDA_13, if s.rodape_aviso { AGUARDANDO_TEXTO } else { TEXTO2 }).meio().item());
    }
    let c = lugar::CARTAO_MAIS_UM;
    q.mais(cartao(c));
    q.mais(texto(Ret::new(c.x + 16.0, c.y + 10.0, c.l - 32.0, 16.0), caixa_alta(t("Mais um aparelho")), F_ROTULO, TEXTO3).meio().item());
    let valor = Ret::new(c.x + 16.0, c.y + 30.0, c.l - 32.0, 24.0);
    match &s.mais_um {
        Some((pin, endereco)) => q.mais(
            rico(valor, {
                let mut partes = vec![Trecho::simples("PIN "), Trecho::mono(pin_em_grupos(pin))];
                if !s.nome_da_espera.is_empty() {
                    partes.extend([Trecho::simples("  ·  "), Trecho::forte(s.nome_da_espera.clone())]);
                }
                partes.extend([Trecho::simples("  ·  "), Trecho::mono(endereco.clone())]);
                partes
            }, F_ENDERECO, TEXTO)
                .meio()
                .item(),
        ),
        None => q.mais(texto(valor, s.mais_um_texto.clone(), F_LEGENDA_13, AGUARDANDO_TEXTO).meio().item()),
    }
    q.controle(Controle::Cancelar, lugar::PARAR);
}

fn cena_conectando(q: &mut Quadro, e: &EstadoDaTela) {
    q.mais(Item::Pilula { ancora: Ancora::Centro(lugar::CX), y: lugar::PILULA_Y, luz: Luz::Aguardando, texto: t("Conectando").into() });
    q.mais(texto(lugar::TITULO_DA_SESSAO, t("Conectando"), F_TITULO_GRANDE, TEXTO).centro().meio().item());
    q.mais(texto(lugar::DESTINO, e.conectando.clone(), F_CORPO_15, TEXTO).centro().meio().item());
    q.mais(
        texto(
            lugar::PARAGRAFO_DA_CONEXAO,
            t("O outro aparelho precisa já ter clicado em Estender: quem exibe só entra depois de quem transmite estar esperando."),
            F_CORPO,
            TEXTO2,
        )
        .centro()
        .quebra()
        .item(),
    );
    q.controle(Controle::Cancelar, lugar::CANCELAR);
}

fn cena_exibindo(q: &mut Quadro, e: &EstadoDaTela) {
    let s = &e.exibindo;
    q.mais(Item::Pilula { ancora: Ancora::Esquerda(lugar::X), y: lugar::PILULA_Y, luz: Luz::Conectado, texto: t("Conectado").into() });
    let de = if s.par.is_empty() { t("do outro aparelho").to_string() } else { tf("de {}", &[&s.par]) };
    q.mais(texto(lugar::TITULO_DA_SESSAO, de, F_TITULO_GRANDE, TEXTO).meio().item());
    q.mais(texto(lugar::VIDEO_NA_OUTRA_JANELA, t("O vídeo está na outra janela."), F_CORPO, TEXTO2).meio().item());
    if !s.som.is_empty() {
        let c = lugar::CARTAO_DO_SOM;
        q.mais(cartao(c));
        q.mais(texto(Ret::new(c.x + 16.0, c.y + 10.0, 200.0, 20.0), t("Som"), F_CORPO_FORTE, TEXTO).meio().item());
        let linha = if s.controles_do_som { lugar::LINHA_DO_SOM } else { Ret::new(c.x + 16.0, lugar::LINHA_DO_SOM.y, c.l - 32.0, 18.0) };
        q.mais(texto(linha, s.som.clone(), F_LEGENDA, TEXTO2).meio().item());
        if s.controles_do_som {
            q.mais(caixa(lugar::SEGMENTADO, RAIO_DO_BOTAO, SUPERFICIE_ALTA));
            q.controle(Controle::Mudo, lugar::MUDO);
            for (i, r) in lugar::segmentos().iter().enumerate() {
                q.controle(Controle::Volume(i), *r);
            }
    
        }
    }
    q.controle(Controle::Detalhes, lugar::DETALHES);
    if s.ajustes_da_camera {
        q.controle(Controle::AjustesDaCamera, lugar::AJUSTES_DA_CAMERA_REMOTA);
    }
    if s.detalhes_abertos {
        let b = lugar::BLOCO_DOS_DETALHES;
        q.mais(cartao(b));
        let resumo = if s.resumo.is_empty() { "—".to_string() } else { s.resumo.clone() };
        q.mais(texto(Ret::new(b.x + 16.0, b.y + 10.0, b.l - 32.0, 36.0), resumo, F_MONO_12, TEXTO2).quebra().item());
        if !s.cadeia.is_empty() {
            q.mais(texto(Ret::new(b.x + 16.0, b.y + 50.0, b.l - 32.0, 36.0), s.cadeia.clone(), F_MONO_12, if s.alerta { AGUARDANDO_TEXTO } else { TEXTO2 }).quebra().item());
        }
    }
    if s.encerrando {
        q.mais(texto(lugar::ENCERRANDO, t(ENCERRANDO), F_LEGENDA_13, TEXTO3).meio().item());
    }
    q.mais(texto(lugar::RODAPE_DA_SESSAO, t("Parar encerra a sessão nos dois lados."), F_LEGENDA, TEXTO3).meio().item());
    q.controle(Controle::Cancelar, lugar::PARAR);
}

// =============================================================================================
// A aparência de cada controle (o que o NM_CUSTOMDRAW desenha)
// =============================================================================================

/// O estado de um controle na hora de desenhar: o do próprio controle nativo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EstadoDoControle {
    pub apertado: bool,
    pub foco: bool,
    pub desligado: bool,
    pub quente: bool,
    /// `BM_GETCHECK` (interruptores e opções).
    pub marcado: bool,
}

/// O desenho de um controle: a cor de trás (os cantos redondos mostram o que está atrás dele), a
/// lista de peças em `(0, 0, l, a)`, e a opacidade (apertado 90 %, desligado 40 %, §4).
#[derive(Clone, Debug, PartialEq)]
pub struct Aparencia {
    pub fundo: Cor,
    pub itens: Vec<Item>,
    pub opacidade: f32,
}

/// **Como cada controle se desenha**, no tamanho `l × a`.
pub fn aparencia(c: Controle, e: &EstadoDaTela, l: f32, a: f32, s: EstadoDoControle) -> Aparencia {
    let mut fundo = FUNDO;
    let mut raio = RAIO_DO_BOTAO;
    let mut itens = match c {
        Controle::Item(p) => {
            fundo = BARRA;
            let estado = e.sessao.as_ref().filter(|x| x.item == p).map(|x| (x.luz, x.rotulo.as_str()));
            item_da_barra(l, a, p.icone(), p.rotulo(), s.marcado, estado)
        }
        Controle::Ladrilho(i) => {
            raio = RAIO_DO_CARTAO;
            match e.espelhar.fontes.get(i) {
                Some(x) => ladrilho(l, a, x.icone, &x.titulo, &x.detalhe, x.detalhe_mono, s.marcado),
                None => vec![],
            }
        }
        #[cfg(feature = "tela-estendida-futura")]
        Controle::TelaEstendidaSemDriver => {
            raio = RAIO_DO_CARTAO;
            use crate::regras_da_tela_estendida as te;
            // O detalhe é o botão (02/10, noite): "Instalar o driver da tela estendida" sem o
            // adaptador; o texto de antes na loja e no Windows sem suporte.
            let detalhe = t(crate::regras_do_driver::detalhe_do_apagado(e.driver.situacao));
            ladrilho(l, a, Icone::TelaEstendida, t(te::TITULO), detalhe, false, false)
        }
        #[cfg(feature = "tela-estendida-futura")]
        Controle::DriverDaTelaEstendida => {
            fundo = SUPERFICIE;
            let rotulo = match crate::regras_do_driver::botao_dos_ajustes(e.driver.situacao) {
                Some(crate::regras_do_driver::BotaoDosAjustes::PaginaDoInstalador) => crate::regras_do_driver::AJUSTES_PAGINA_DO_INSTALADOR,
                _ => crate::regras_do_driver::AJUSTES_DESINSTALAR,
            };
            botao(l, a, TipoDeBotao::Secundario, t(rotulo), None, F_BOTAO_PEQUENO)
        }
        #[cfg(not(feature = "tela-estendida-futura"))]
        Controle::TelaEstendidaSemDriver | Controle::DriverDaTelaEstendida => vec![],
        Controle::Som => {
            raio = RAIO_DO_CARTAO;
            linha_de_interruptor(l, a, t("Mandar o som deste computador"), t("Vai o que o Windows estiver tocando."), s.marcado)
        }
        Controle::Microfone => match &e.camera {
            Some(cam) if e.cena != Cena::Painel(Painel::Espelhar) => {
                raio = l / 2.0;
                redondo(l, Redondo::Microfone { ligado: s.marcado }, &cam.legenda_do_microfone)
            }
            _ => {
                raio = RAIO_DO_CARTAO;
                linha_de_interruptor(l, a, t("Microfone"), t("Começa desligado. O som da câmera é o do microfone."), s.marcado)
            }
        },
        Controle::Espelhar => botao(l, a, TipoDeBotao::Principal, t("Estender"), Some(Icone::Espelhar), F_BOTAO),
        Controle::Exibir => botao(l, a, TipoDeBotao::Principal, t("Exibir"), None, F_BOTAO),
        Controle::Papel(i) => {
            raio = RAIO_DO_CARTAO;
            let (icone, titulo_, detalhe) = PAPEIS[i.min(2)];
            cartao_de_papel(l, a, icone, t(titulo_), t(detalhe))
        }
        Controle::Esquecer => {
            // Nos Ajustes fica dentro do cartão; na espera da câmera, sobre o fundo.
            fundo = if e.cena == Cena::Painel(Painel::Ajustes) { SUPERFICIE } else { FUNDO };
            if e.ajustes.confirmando && e.cena == Cena::Painel(Painel::Ajustes) {
                botao(l, a, TipoDeBotao::Perigo, t("Esquecer de verdade"), None, F_BOTAO_PEQUENO)
            } else {
                botao(l, a, TipoDeBotao::Secundario, t("Esquecer pareamentos"), None, F_BOTAO_PEQUENO)
            }
        }
        Controle::Diario => {
            fundo = SUPERFICIE;
            botao(l, a, TipoDeBotao::Secundario, t("Abrir a pasta"), Some(Icone::Pasta), F_BOTAO_PEQUENO)
        }
        Controle::Licencas => {
            fundo = SUPERFICIE;
            botao(l, a, TipoDeBotao::Secundario, t("Licenças"), None, F_BOTAO_PEQUENO)
        }
        Controle::Privacidade => {
            fundo = SUPERFICIE;
            botao(l, a, TipoDeBotao::Secundario, t("Privacidade"), None, F_BOTAO_PEQUENO)
        }
        Controle::Suporte => {
            fundo = SUPERFICIE;
            botao(l, a, TipoDeBotao::Secundario, t("Suporte"), None, F_BOTAO_PEQUENO)
        }
        Controle::Letreiro => {
            raio = 12.0;
            letreiro(l / 2.0, 0.0, &e.espera.pin)
        }
        Controle::Chip => {
            raio = a / 2.0;
            chip(l, a, &e.espera.endereco, e.copiado)
        }
        Controle::Cancelar => match (&e.camera, e.cena) {
            (Some(cam), Cena::Esperando | Cena::NoAr) => {
                raio = l / 2.0;
                redondo(l, Redondo::Parar, &cam.legenda_do_parar)
            }
            (_, Cena::Esperando | Cena::Conectando) => botao(l, a, TipoDeBotao::Secundario, t("Cancelar"), None, F_BOTAO),
            _ => botao(l, a, TipoDeBotao::PararCheio, t("Parar"), None, F_BOTAO),
        },
        Controle::Gravar => {
            raio = l / 2.0;
            let cam = e.camera.clone().unwrap_or_default();
            redondo(l, Redondo::Gravar { gravando: cam.gravando }, &cam.legenda_do_gravar)
        }
        // R9b: no Exibindo, um botão com a engrenagem ao lado de "Detalhes" (a câmera de quem filma).
        Controle::AjustesDaCamera if e.cena == Cena::Exibindo => {
            botao(l, a, TipoDeBotao::Secundario, t(crate::regras_dos_controles::ROTULO_DO_ICONE), Some(Icone::Ajustes), F_BOTAO_PEQUENO)
        }
        Controle::AjustesDaCamera => {
            raio = l / 2.0;
            redondo(l, Redondo::Ajustes, t("Ajustes"))
        }
        Controle::Mudo => {
            fundo = SUPERFICIE;
            let mut v = vec![texto(Ret::new(0.0, 0.0, l - INTERRUPTOR_L - 8.0, a), t("Mudo"), F_LEGENDA_13, TEXTO_DA_BARRA).direita().meio().item()];
            v.extend(interruptor(l - INTERRUPTOR_L, (a - INTERRUPTOR_A) / 2.0, s.marcado));
            v
        }
        Controle::SomComCamera => {
            raio = RAIO_DO_CARTAO;
            linha_de_interruptor(
                l,
                a,
                t("Tocar o som mesmo com a câmera do Quall em uso"),
                t("Numa chamada com a câmera virtual, o som continua tocando aqui."),
                s.marcado,
            )
        }
        Controle::Volume(i) => {
            fundo = SUPERFICIE_ALTA;
            raio = 4.0;
            segmento(l, a, VOLUMES[i.min(3)].1, s.marcado)
        }
        Controle::Idioma(i) => {
            fundo = SUPERFICIE_ALTA;
            raio = 4.0;
            segmento(l, a, idioma_do_segmento(i).sigla(), s.marcado)
        }
        Controle::Detalhes => {
            let rotulo_ = t("Detalhes");
            let mut v = vec![rico(
                Ret::new(0.0, 0.0, l, a),
                vec![Trecho::icone(if s.marcado { Icone::Fechar } else { Icone::Abrir }, 11.0), Trecho::simples(format!("\u{2002}{rotulo_}"))],
                F_LEGENDA_13,
                TEXTO2,
            )
            .meio()
            .item()];
            // A acusação (a linha laranja de hoje) põe um ponto âmbar ao lado (§6.6).
            if e.exibindo.alerta {
                let x = 11.0 + largura_estimada(&format!("  {rotulo_}"), F_LEGENDA_13) + 8.0;
                v.push(Item::Circulo { cx: x.min(l - 6.0), cy: a / 2.0, raio: 4.0, cor: AGUARDANDO });
            }
            v
        }
        Controle::Lista | Controle::Endereco | Controle::Pin => vec![],
    };
    if s.quente && !s.desligado && c.especie() != Especie::Letreiro {
        itens.push(caixa(Ret::new(0.0, 0.0, l, a), raio, Cor::rgba(0xFFFFFF, 10)));
    }
    if s.foco {
        itens.push(anel_de_foco(l, a, raio));
    }
    let opacidade = if s.desligado {
        0.4
    } else if c == Controle::TelaEstendidaSemDriver {
        // **Apagado, sem estar desligado** (R10): a mesma opacidade do desligado (§4), mas o botão
        // segue vivo para o clique abrir as instruções; apertado, um pouco mais escuro.
        if s.apertado { 0.36 } else { 0.4 }
    } else if s.apertado {
        0.9
    } else {
        1.0
    };
    Aparencia { fundo, itens, opacidade }
}

/// **Uma linha da lista** "NA REDE AGORA", no tamanho `l × a`.
pub fn aparencia_da_linha(i: usize, e: &EstadoDaTela, l: f32, a: f32, escolhida: bool, foco: bool) -> Aparencia {
    let mut itens = match e.exibir.aparelhos.get(i) {
        Some(x) => linha_de_aparelho(l, a, &x.nome, &x.tipo, &x.endereco, escolhida),
        None => vec![],
    };
    if foco {
        itens.push(Item::Caixa { ret: Ret::new(0.0, 0.0, l, a - 6.0), raio: RAIO_DO_CARTAO, fundo: Cor::rgba(0, 0), borda: Some((TEXTO.vezes(0.85), 2.0)) });
    }
    Aparencia { fundo: FUNDO, itens, opacidade: 1.0 }
}

/// O idioma de cada segmento do seletor: 0 PT, 1 EN.
pub fn idioma_do_segmento(i: usize) -> crate::idioma::Idioma {
    if i == 0 {
        crate::idioma::Idioma::Pt
    } else {
        crate::idioma::Idioma::En
    }
}

/// **O nome de cada controle para o Narrador** (o texto da janela do botão).
pub fn texto_acessivel(c: Controle, e: &EstadoDaTela) -> String {
    match c {
        // Cada segmento diz o idioma dele, na língua dele (o contrato comum, item 1).
        Controle::Idioma(i) => idioma_do_segmento(i).nome_acessivel().into(),
        Controle::Item(p) => match e.sessao.as_ref().filter(|s| s.item == p) {
            Some(s) => format!("{}, {}", p.rotulo(), s.rotulo),
            None => p.rotulo().to_string(),
        },
        Controle::Ladrilho(i) => e
            .espelhar
            .fontes
            .get(i)
            .map(|x| if x.detalhe.is_empty() { x.titulo.clone() } else { format!("{}, {}", x.titulo, x.detalhe) })
            .unwrap_or_default(),
        #[cfg(feature = "tela-estendida-futura")]
        Controle::TelaEstendidaSemDriver => t(crate::regras_do_driver::texto_acessivel_do_apagado(e.driver.situacao)).into(),
        #[cfg(feature = "tela-estendida-futura")]
        Controle::DriverDaTelaEstendida => match crate::regras_do_driver::botao_dos_ajustes(e.driver.situacao) {
            Some(crate::regras_do_driver::BotaoDosAjustes::PaginaDoInstalador) => t(crate::regras_do_driver::AJUSTES_PAGINA_DO_INSTALADOR).into(),
            _ => t(crate::regras_do_driver::CAIXA_DESINSTALAR_TITULO).into(),
        },
        #[cfg(not(feature = "tela-estendida-futura"))]
        Controle::TelaEstendidaSemDriver | Controle::DriverDaTelaEstendida => String::new(),
        Controle::Som => t("Mandar o som deste computador").into(),
        Controle::Microfone => match &e.camera {
            Some(cam) if e.cena != Cena::Painel(Painel::Espelhar) => tf("Microfone, {}", &[&cam.legenda_do_microfone]),
            _ => t("Microfone").into(),
        },
        Controle::Espelhar => t("Estender").into(),
        Controle::Exibir => t("Exibir").into(),
        Controle::Lista => t("Na rede agora").into(),
        Controle::Endereco => t("Endereço do outro aparelho").into(),
        Controle::Pin => t("PIN do outro aparelho; vazio se os dois já parearam").into(),
        Controle::Papel(i) => t(PAPEIS[i.min(2)].1).to_string(),
        Controle::Esquecer if e.ajustes.confirmando && e.cena == Cena::Painel(Painel::Ajustes) => {
            t("Esquecer de verdade: os aparelhos vão pedir o PIN outra vez").into()
        }
        Controle::Esquecer => t("Esquecer pareamentos").into(),
        Controle::Diario => t("Abrir a pasta do diário").into(),
        Controle::Licencas => t("Abrir as licenças de terceiros").into(),
        Controle::Privacidade => t("Abrir a política de privacidade").into(),
        Controle::Suporte => t("Abrir a página de suporte").into(),
        Controle::Letreiro => pin_para_ler(&e.espera.pin),
        // Copiado, o nome muda, e o Narrador anuncia a troca do nome do botão com o foco.
        Controle::Chip if e.copiado => tf("Copiado: {}", &[&e.espera.endereco]),
        Controle::Chip => tf("Copiar o endereço {}", &[&e.espera.endereco]),
        Controle::Cancelar => match (&e.camera, e.cena) {
            (Some(cam), Cena::Esperando | Cena::NoAr) => cam.legenda_do_parar.clone(),
            (_, Cena::Esperando | Cena::Conectando) => t("Cancelar").into(),
            _ => t("Parar").into(),
        },
        Controle::Gravar => e
            .camera
            .as_ref()
            .map(|c| if c.nome_do_gravar.is_empty() { c.legenda_do_gravar.clone() } else { c.nome_do_gravar.clone() })
            .unwrap_or_else(|| t("Gravar").into()),
        Controle::Mudo => t("Mudo").into(),
        Controle::SomComCamera => t("Tocar o som mesmo com a câmera do Quall em uso").into(),
        Controle::Volume(i) => tf("Volume {}", &[&VOLUMES[i.min(3)].1]),
        Controle::Detalhes => t("Detalhes").into(),
        Controle::AjustesDaCamera => t(crate::regras_dos_controles::ROTULO_DO_ICONE).into(),
    }
}

// =============================================================================================
// Os exemplos (o retrato de bancada e os testes)
// =============================================================================================

// Os exemplos vêm no idioma de agora (o retrato de bancada os chama dentro de `com_idioma`): os
// textos de interface por `t()` (os que nascem no emissor e no receptor, com as chaves das tabelas
// deles); os dados de exemplo (nomes de aparelho, de monitor e de câmera, endereços, números) como
// estão.

fn ladrilhos_de_exemplo(escolhido: usize) -> Vec<Ladrilho> {
    let base = [
        // i18n: fora (o nome do monitor é dado de exemplo)
        (tf("{} (principal)", &[&"Monitor 1"]), "1920 × 1080", true, Icone::Monitor),
        #[cfg(feature = "tela-estendida-futura")]
        (t(crate::regras_da_tela_estendida::TITULO).to_string(), t(crate::regras_da_tela_estendida::DETALHE), false, Icone::TelaEstendida),
        ("Integrated Webcam".to_string(), t("Câmera"), false, Icone::Camera),
        ("Canon EOS".to_string(), t("Câmera"), false, Icone::Camera),
    ];
    base.iter()
        .enumerate()
        .map(|(i, (nome, d, m, ic))| Ladrilho { titulo: nome.clone(), detalhe: d.to_string(), detalhe_mono: *m, icone: *ic, escolhido: i == escolhido })
        .collect()
}

fn base_de_exemplo() -> EstadoDaTela {
    EstadoDaTela {
        nome_do_aparelho: "DELL-G3".into(),
        espelhar: TelaEspelhar {
            fontes: ladrilhos_de_exemplo(0),
            camera: false,
            alguma_escolhida: true,
            som: true,
            microfone: false,
            ip: Some("192.168.15.2".into()),
            aviso: None,
            pode_espelhar: true,
            tela_estendida_sem_driver: None,
        },
        exibir: TelaExibir {
            aparelhos: vec![
                // i18n: fora (o nome do aparelho é dado de exemplo)
                LinhaDeAparelho { nome: "iPhone do Bruno".into(), tipo: t("Tela ou câmera").into(), endereco: "192.168.15.4:7877".into() },
                LinhaDeAparelho { nome: "Galaxy A07".into(), tipo: t("Tela ou câmera").into(), endereco: "192.168.15.11:7877".into() },
            ],
            escolhido: Some(0),
            procurando: true,
            vazio: t("Ninguém anunciando. Use o endereço que aparece no outro aparelho.").into(),
            pede_pin: false,
            aviso: None,
            foco: None,
        },
        ajustes: TelaAjustes { tem_pares: true, confirmando: false, pasta_do_diario: "%LOCALAPPDATA%\\Quall\\Logs".into(), versao: "0.1.0".into() },
        espera: TelaEspera {
            pin: "482719".into(),
            endereco: "192.168.15.2:7877".into(),
            ha_pares: false,
            nome: "DELL-G3".into(),
            anunciando: true,
            origem: "Monitor 1".into(), // i18n: fora (dado de exemplo)
            com_som: Some(true),
            frase: String::new(),
            aviso: String::new(),
            aviso_ambar: false,
            encerrando: false,
        },
        no_ar: TelaNoAr {
            par: "iPad do Bruno".into(), // i18n: fora (dado de exemplo)
            origem: "Monitor 1".into(), // i18n: fora (dado de exemplo)
            imagem: "1920×1080 · 30".into(),
            rede: "192.168.15.2".into(),
            som: t("Indo junto").into(),
            som_indo: true,
            // i18n: fora (os números do emissor, o diagnóstico de hoje)
            resumo: "1834 quadros · 3 IDR · captura+encode 4.2 ms · 2870 quadros de som".into(),
            frase: String::new(),
        },
        conectando: "iPhone do Bruno (192.168.15.4:7877)".into(), // i18n: fora (dado de exemplo)
        exibindo: TelaExibindo {
            par: "iPhone do Bruno".into(), // i18n: fora (dado de exemplo)
            som: tf("som: tocando, volume {} %", &[&100]),
            controles_do_som: true,
            mudo: false,
            volume: 0,
            som_com_camera: false,
            detalhes_abertos: false,
            resumo: "1920×1080 · 30,0 fps · 8,4 Mbps · 0 perdidos".into(),
            // i18n: fora (os números do receptor, o diagnóstico de hoje)
            cadeia: "rupturas 0 · suspeitos 0 · pior rajada 0 · retidos 0 · sem referência 0 ms".into(),
            alerta: false,
            encerrando: false,
            ajustes_da_camera: true,
        },
        ..Default::default()
    }
}

fn camera_de_exemplo(gravando: bool) -> ControlesDaCamera {
    ControlesDaCamera {
        microfone: true,
        legenda_do_microfone: t("Mic ligado").into(),
        gravar: true,
        gravar_ativo: true,
        gravando,
        legenda_do_gravar: if gravando { "1:23".into() } else { t("Gravar").into() },
        // O rótulo e a linha como o emissor os monta (`publicar_camera_comum`), com as chaves dele.
        nome_do_gravar: if gravando { tf("■ Parar a gravação ({})", &[&"1:23"]) } else { t("● Gravar neste computador").into() },
        legenda_do_parar: if gravando { t("Parar e salvar").into() } else { t("Cancelar").into() },
        linha_da_gravacao: if gravando {
            tf("● GRAVANDO {}", &[&"1:23"]) + &tf(" · sobram {} GB", &[&crate::idioma::decimal(41.2, 1)])
        } else {
            String::new()
        },
        esquecer: false,
        ajustes: true,
        controlado_por: String::new(),
        pouca_luz: None,
    }
}

fn sessao_de_exemplo(item: Painel, luz: Luz, rotulo: &str) -> Option<SessaoNaBarra> {
    let nota = if item == Painel::Exibir { t(NOTA_DO_RECEPTOR) } else { t(NOTA_DO_EMISSOR) };
    Some(SessaoNaBarra { item, luz, rotulo: rotulo.into(), nota: nota.into() })
}

/// A nota da barra com uma sessão de pé (as mesmas da janela).
pub const NOTA_DO_EMISSOR: &str = "Pare de estender para exibir outra tela ou abrir os Ajustes."; // i18n: chave
pub const NOTA_DO_RECEPTOR: &str = "Um papel por vez. Pare de exibir para usar os outros."; // i18n: chave

/// **Os estados de exemplo**, um por tela e estado da §7 (nome do arquivo, estado).
pub fn exemplos() -> Vec<(&'static str, EstadoDaTela)> {
    let mut v = Vec::new();
    let b = base_de_exemplo();

    v.push(("01-espelhar", EstadoDaTela { cena: Cena::Painel(Painel::Espelhar), painel: Painel::Espelhar, ..b.clone() }));
    let mut e = b.clone();
    e.espelhar.fontes = ladrilhos_de_exemplo(2);
    e.espelhar.camera = true;
    e.espelhar.microfone = true;
    e.espelhar.aviso = Some(Aviso { tom: Tom::Ambar, texto: t("O teleprompter com a câmera está aberto: feche-o para espelhar daqui.").into() });
    e.espelhar.pode_espelhar = false;
    v.push(("02-espelhar-camera-com-aviso", e));
    let mut e = b.clone();
    e.espelhar.fontes = (0..10)
        .map(|i| Ladrilho {
            titulo: format!("Monitor {}", i + 1), // i18n: fora (o nome do monitor é dado de exemplo)
            detalhe: "2560 × 1440".into(),
            detalhe_mono: true,
            icone: Icone::Monitor,
            escolhido: i == 3,
        })
        .collect();
    e.espelhar.ip = None;
    v.push(("03-espelhar-dez-fontes-sem-rede", e));
    #[cfg(feature = "tela-estendida-futura")]
    {
    // R10: sem o adaptador do SudoVDA, a tela estendida apagada depois do monitor.
    let mut e = b.clone();
    e.espelhar.fontes = ladrilhos_de_exemplo(0).into_iter().filter(|x| x.icone != Icone::TelaEstendida).collect();
    e.espelhar.tela_estendida_sem_driver = Some(1);
    v.push(("03b-espelhar-tela-estendida-sem-driver", e.clone()));
    // 02/10, noite: o Quall instalando o driver (o aviso diz o passo).
    e.driver.andamento = crate::regras_do_driver::Andamento::Rodando { acao: crate::regras_do_driver::Acao::Instalar, passo: 5 };
    v.push(("03c-espelhar-instalando-o-driver", e));
    }

    v.push(("04-exibir", EstadoDaTela { cena: Cena::Painel(Painel::Exibir), painel: Painel::Exibir, ..b.clone() }));
    let mut e = EstadoDaTela { cena: Cena::Painel(Painel::Exibir), painel: Painel::Exibir, ..b.clone() };
    e.exibir.aparelhos.clear();
    e.exibir.escolhido = None;
    e.exibir.pede_pin = true;
    e.exibir.aviso = Some(Aviso { tom: Tom::Vermelho, texto: t("O PIN não conferiu. Digite os seis dígitos que aparecem no outro aparelho.").into() });
    v.push(("05-exibir-vazio-pede-pin", e));

    v.push(("06-teleprompter", EstadoDaTela { cena: Cena::Painel(Painel::Teleprompter), painel: Painel::Teleprompter, ..b.clone() }));
    v.push(("07-ajustes", EstadoDaTela { cena: Cena::Painel(Painel::Ajustes), painel: Painel::Ajustes, ..b.clone() }));
    let mut e = EstadoDaTela { cena: Cena::Painel(Painel::Ajustes), painel: Painel::Ajustes, ..b.clone() };
    e.ajustes.confirmando = true;
    v.push(("07b-ajustes-confirmando", e));
    #[cfg(feature = "tela-estendida-futura")]
    {
    let mut e = EstadoDaTela { cena: Cena::Painel(Painel::Ajustes), painel: Painel::Ajustes, ..b.clone() };
    e.driver.situacao = crate::regras_do_driver::Situacao::DoQuall { presente: true };
    v.push(("07c-ajustes-driver-do-quall", e));
    }
    let mut e = EstadoDaTela { cena: Cena::Painel(Painel::Teleprompter), painel: Painel::Teleprompter, ..b.clone() };
    // Em português, como o cartão o escreve (o `compor` o traduz).
    e.aviso_do_teleprompter = AVISO_FECHANDO_O_TELEPROMPTER.into();
    v.push(("06b-teleprompter-fechando", e));

    let espera = EstadoDaTela {
        cena: Cena::Esperando,
        painel: Painel::Espelhar,
        sessao: sessao_de_exemplo(Painel::Espelhar, Luz::Aguardando, t("Aguardando")),
        ..b.clone()
    };
    v.push(("08-esperando", espera.clone()));
    let mut e = espera.clone();
    e.espera.ha_pares = true;
    e.espera.anunciando = false;
    e.espera.frase = t("O Windows recusou a saída de som padrão: vai sem som.").into();
    v.push(("09-esperando-pareados-sem-anuncio", e));
    let mut e = espera.clone();
    e.espera.origem = "Integrated Webcam".into();
    e.espera.com_som = None;
    e.espera.frase = t("O microfone abre quando o outro aparelho conectar.").into();
    e.espera.aviso = t("O aparelho não entrou com o pareamento guardado: esqueça os pareamentos e use o PIN.").into();
    e.espera.aviso_ambar = true;
    e.camera = Some(ControlesDaCamera { esquecer: true, ..camera_de_exemplo(false) });
    v.push(("10-esperando-camera", e));

    let no_ar = EstadoDaTela { cena: Cena::NoAr, painel: Painel::Espelhar, sessao: sessao_de_exemplo(Painel::Espelhar, Luz::NoAr, t("Espelhando")), ..b.clone() };
    v.push(("11-no-ar", no_ar.clone()));
    let mut e = no_ar.clone();
    e.sessao = sessao_de_exemplo(Painel::Espelhar, Luz::NoAr, t("No ar"));
    e.no_ar.origem = "Integrated Webcam".into();
    e.no_ar.imagem = t("Da câmera").into();
    e.no_ar.som = t("Microfone").into();
    // A linha do microfone fica em português no emissor; a janela a traduz (`tr`) ao montar.
    e.no_ar.frase = t("Com o som do microfone.").into();
    e.camera = Some(camera_de_exemplo(true));
    v.push(("12-no-ar-camera-gravando", e));

    let mut e = EstadoDaTela { cena: Cena::Varios, painel: Painel::Espelhar, sessao: sessao_de_exemplo(Painel::Espelhar, Luz::NoAr, t("Espelhando")), ..b.clone() };
    e.varios = TelaVarios {
        nome_da_espera: "Quall 12ab34cd".into(), // i18n: fora (ephemeral example label)
        receptores: vec![
            LinhaDeReceptor { nome: "iPad do Bruno".into(), monitor: "2360 × 1640, índice 1".into(), resumo: "912 quadros · 1 IDR · 3,9 ms".into() }, // i18n: fora (dados de exemplo)
            LinhaDeReceptor { nome: "iPhone X".into(), monitor: "2436 × 1124, índice 2".into(), resumo: "905 quadros · 1 IDR · 4,1 ms".into() }, // i18n: fora (dados de exemplo)
            LinhaDeReceptor { nome: "Galaxy S24".into(), monitor: "2340 × 1080, índice 3".into(), resumo: "899 quadros · 2 IDR · 4,4 ms".into() }, // i18n: fora (dados de exemplo)
        ],
        mais_um: Some(("591204".into(), "192.168.15.2:7878".into())),
        mais_um_texto: String::new(),
        rodape: t("Com o som deste computador, num aparelho só.").into(),
        rodape_aviso: false,
    };
    v.push(("13-varios", e.clone()));
    e.varios.receptores = (0..8)
        .map(|i| LinhaDeReceptor { nome: format!("Aparelho {}", i + 1), monitor: "1920 × 1080".into(), resumo: "900 quadros · 1 IDR".into() }) // i18n: fora (dados de exemplo)
        .collect();
    e.varios.mais_um = None;
    e.varios.mais_um_texto = tf("Limite de {} aparelhos: para entrar mais um, outro precisa sair.", &[&8]);
    v.push(("14-varios-no-limite", e));

    v.push((
        "15-conectando",
        EstadoDaTela { cena: Cena::Conectando, painel: Painel::Exibir, sessao: sessao_de_exemplo(Painel::Exibir, Luz::Aguardando, t("Conectando")), ..b.clone() },
    ));
    let exibindo = EstadoDaTela { cena: Cena::Exibindo, painel: Painel::Exibir, sessao: sessao_de_exemplo(Painel::Exibir, Luz::Conectado, t("Exibindo")), ..b.clone() };
    v.push(("16-exibindo", exibindo.clone()));
    let mut e = exibindo;
    e.exibindo.detalhes_abertos = true;
    e.exibindo.alerta = true;
    e.exibindo.cadeia = "rupturas 2 · suspeitos 124 · pior rajada 18 · retidos 3 · sem referência 640 ms".into(); // i18n: fora (o diagnóstico)
    e.exibindo.volume = 2;
    e.exibindo.mudo = true;
    v.push(("17-exibindo-detalhes-com-acusacao", e));
    v
}

/// O estado de cada controle num exemplo (o que o retrato põe nos controles nativos antes de
/// desenhar, e o que o teste confere).
pub fn marcado_no_exemplo(c: Controle, e: &EstadoDaTela) -> bool {
    match c {
        Controle::Item(p) => e.painel == p,
        Controle::Ladrilho(i) => e.espelhar.fontes.get(i).is_some_and(|x| x.escolhido),
        Controle::Som => e.espelhar.som,
        Controle::Microfone => e.camera.as_ref().map_or(e.espelhar.microfone, |c| c.microfone),
        Controle::Mudo => e.exibindo.mudo,
        Controle::SomComCamera => e.exibindo.som_com_camera,
        Controle::Volume(i) => e.exibindo.volume == i,
        Controle::Detalhes => e.exibindo.detalhes_abertos,
        Controle::Idioma(i) => idioma_do_segmento(i) == e.idioma,
        _ => false,
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn a_instrucao_usa_apenas_o_rotulo_do_anuncio_da_espera_atual() {
        let anterior = (40000, "Quall a1b2c3d4".to_string());
        let atual = (40001, "Quall b2c3d4e5".to_string());
        assert_eq!(rotulo_da_espera(Some(40000), Some(&anterior)), Some("Quall a1b2c3d4"));
        assert_eq!(rotulo_da_espera(Some(40001), Some(&anterior)), None, "a espera mudou antes do anúncio");
        assert_eq!(rotulo_da_espera(Some(40001), Some(&atual)), Some("Quall b2c3d4e5"));
        assert_eq!(rotulo_da_espera(None, Some(&atual)), None, "no limite de oito não há espera");
        assert_eq!(rotulo_da_espera(Some(40001), None), None, "multicast bloqueado usa endereço");
    }

    #[test]
    fn privacidade_e_suporte_seguem_o_idioma_e_ficam_nos_ajustes() {
        use crate::idioma::Idioma;
        for (idioma, privacidade, suporte) in [
            (Idioma::Pt, "https://queven.com.br/quall/privacidade/", "https://queven.com.br/quall/suporte/"),
            (Idioma::En, "https://queven.com.br/en/quall/privacy/", "https://queven.com.br/en/quall/support/"),
        ] {
            assert_eq!(pagina_dos_ajustes(Controle::Privacidade, idioma), Some(privacidade));
            assert_eq!(pagina_dos_ajustes(Controle::Suporte, idioma), Some(suporte));
            assert_eq!(pagina_dos_ajustes(Controle::Licencas, idioma), None);
            crate::idioma::com_idioma(idioma, || {
                for (_, e) in exemplos() {
                    let q = compor(&e);
                    for controle in [Controle::Privacidade, Controle::Suporte] {
                        assert_eq!(q.lugar(controle).is_some(), e.cena == Cena::Painel(Painel::Ajustes));
                    }
                }
            });
        }
    }

    #[test]
    fn monitor_oferece_apenas_estender_exibir_e_ajustes() {
        let controles = Controle::fixos();
        let paineis: Vec<_> = controles.iter().filter_map(|c| match c { Controle::Item(p) => Some(*p), _ => None }).collect();
        assert_eq!(paineis, [Painel::Espelhar, Painel::Exibir, Painel::Ajustes]);
        for ausente in [Controle::Microfone, Controle::Gravar, Controle::SomComCamera, Controle::AjustesDaCamera, Controle::Papel(0), Controle::Papel(1), Controle::Papel(2)] {
            assert!(!controles.contains(&ausente), "{ausente:?}");
        }
    }

    #[test]
    fn o_id_de_cada_controle_volta() {
        let mut todos = Controle::fixos();
        todos.extend((0..MAX_LADRILHOS).map(Controle::Ladrilho));
        let mut vistos = std::collections::HashSet::new();
        for c in todos {
            assert_eq!(Controle::de_id(c.id()), Some(c), "{c:?}");
            assert!(vistos.insert(c.id()), "id repetido: {c:?}");
        }
        assert_eq!(Controle::de_id(1), None, "o 1 é o IDOK do Enter, e não um controle");
        assert_eq!(Controle::de_id(2), None, "o 2 é o IDCANCEL do Esc");
    }

    #[test]
    fn cada_tela_cabe_na_janela_e_nenhum_controle_cobre_outro() {
        let janela = Ret::new(0.0, 0.0, LARGURA_MINIMA, ALTURA_MINIMA);
        for (nome, e) in exemplos() {
            let q = compor(&e);
            for (c, r) in &q.controles {
                assert!(janela.contem(r), "{nome}: {c:?} fora da janela: {r:?}");
                if !matches!(c, Controle::Item(_)) {
                    assert!(lugar::PAINEL.contem(r), "{nome}: {c:?} fora do painel: {r:?}");
                }
            }
            for (i, (a, ra)) in q.controles.iter().enumerate() {
                for (b, rb) in q.controles.iter().skip(i + 1) {
                    assert!(!ra.cruza(rb), "{nome}: {a:?} {ra:?} cobre {b:?} {rb:?}");
                }
            }
            for item in &q.itens {
                if let Item::Texto(t) = item {
                    assert!(janela.contem(&t.ret), "{nome}: texto fora da janela: {:?} {:?}", t.texto_corrido(), t.ret);
                    // Um texto pintado não pode ficar debaixo de um controle nativo (o controle o
                    // cobriria). Os controles pintam o próprio texto.
                    for (c, r) in &q.controles {
                        assert!(!t.ret.cruza(r), "{nome}: o texto {:?} fica debaixo de {c:?}", t.texto_corrido());
                    }
                }
            }
        }
    }

    /// **Em inglês** (a tradução, 02/10): cada exemplo composto com o idioma da thread em inglês
    /// continua cabendo, sem controle cobrindo controle nem texto debaixo de controle — e nenhum texto
    /// de interface do quadro, dos controles ou dos nomes para o Narrador ficou em português (igual a
    /// uma chave da tabela cujo inglês é outro, também em caixa alta, como saem os rótulos de seção).
    #[test]
    fn em_ingles_cabe_e_nada_fica_em_portugues() {
        use crate::idioma::{areas, com_idioma, Idioma};
        let chaves: Vec<(String, String)> = areas()
            .iter()
            .flat_map(|(_, v)| v.iter())
            .filter(|(pt, en)| pt != en)
            .map(|(pt, _)| (pt.to_string(), pt.to_uppercase()))
            .collect();
        let em_portugues = |s: &str| !s.trim().is_empty() && chaves.iter().any(|(pt, alta)| s == pt || s == alta);
        let janela = Ret::new(0.0, 0.0, LARGURA_MINIMA, ALTURA_MINIMA);
        com_idioma(Idioma::En, || {
            for (nome, e) in exemplos() {
                assert_eq!(compor(&e).controles.len(), crate::idioma::com_idioma(Idioma::Pt, || compor(&e).controles.len()), "{nome}");
                let q = compor(&e);
                for (i, (a, ra)) in q.controles.iter().enumerate() {
                    assert!(janela.contem(ra), "{nome} (en): {a:?} fora da janela: {ra:?}");
                    for (b, rb) in q.controles.iter().skip(i + 1) {
                        assert!(!ra.cruza(rb), "{nome} (en): {a:?} {ra:?} cobre {b:?} {rb:?}");
                    }
                }
                let mut textos: Vec<String> = Vec::new();
                let mut juntar = |itens: &[Item]| {
                    for item in itens {
                        match item {
                            Item::Texto(tx) => {
                                textos.push(tx.texto_corrido());
                                textos.extend(tx.trechos.iter().map(|p| p.texto.clone()));
                            }
                            Item::Pilula { texto, .. } => textos.push(texto.clone()),
                            _ => {}
                        }
                    }
                };
                for item in &q.itens {
                    if let Item::Texto(tx) = item {
                        assert!(janela.contem(&tx.ret), "{nome} (en): texto fora da janela: {:?}", tx.texto_corrido());
                        for (c, r) in &q.controles {
                            assert!(!tx.ret.cruza(r), "{nome} (en): o texto {:?} fica debaixo de {c:?}", tx.texto_corrido());
                        }
                    }
                }
                juntar(&q.itens);
                for (c, r) in &q.controles {
                    juntar(&aparencia(*c, &e, r.l, r.a, EstadoDoControle::default()).itens);
                    juntar(&aparencia(*c, &e, r.l, r.a, EstadoDoControle { marcado: true, ..Default::default() }).itens);
                }
                for i in 0..e.exibir.aparelhos.len() {
                    juntar(&aparencia_da_linha(i, &e, lugar::L, lugar::LINHA_DA_LISTA, false, false).itens);
                }
                let mut nomes: Vec<Controle> = Controle::fixos();
                nomes.extend((0..e.espelhar.fontes.len()).map(Controle::Ladrilho));
                textos.extend(nomes.into_iter().map(|c| texto_acessivel(c, &e)));
                for s in &textos {
                    assert!(!em_portugues(s), "{nome} (en): {s:?} ficou em português");
                }
            }
            // E um retrato do que muda: o botão, o item da barra e a instrução da espera.
            let e = exemplos().into_iter().find(|(n, _)| *n == "08-esperando").unwrap().1;
            assert_eq!(texto_acessivel(Controle::Item(Painel::Espelhar), &e), "Extend, Waiting");
            assert_eq!(texto_acessivel(Controle::Chip, &e), "Copy the address 192.168.15.2:7877");
            let instrucao: String = instrucao_da_espera(&e.espera).iter().map(|x| x.texto.as_str()).collect();
            assert_eq!(
                instrucao,
                "On the other device, open Quall in Receive and pick DELL-G3 from the list, or type the address below. Sending Monitor 1, with this computer's audio."
            );
            assert_eq!(legenda_do_microfone(true, Some("Com o som do microfone."), false), "Mic on", "a linha do emissor em português");
            assert_eq!(legenda_do_gravar("■ Stop recording (0:05)", true, "● RECORDING 0:05 · Recording with NO AUDIO — turn on the microphone"), "0:05 · NO AUDIO");
        });
    }

    #[test]
    fn a_sessao_apaga_os_outros_itens() {
        let e = exemplos().into_iter().find(|(n, _)| *n == "08-esperando").unwrap().1;
        assert!(habilitado(Controle::Item(Painel::Espelhar), &e));
        for p in [Painel::Exibir, Painel::Teleprompter, Painel::Ajustes] {
            assert!(!habilitado(Controle::Item(p), &e), "{p:?}");
        }
        let e = exemplos().into_iter().find(|(n, _)| *n == "01-espelhar").unwrap().1;
        for p in Painel::TODOS {
            assert!(habilitado(Controle::Item(p), &e));
        }
    }

    #[test]
    fn os_controles_de_cada_cena() {
        let achar = |n: &str| exemplos().into_iter().find(|(x, _)| *x == n).unwrap().1;
        let q = compor(&achar("01-espelhar"));
        assert!(q.lugar(Controle::Som).is_some() && q.lugar(Controle::Microfone).is_none());
        assert_eq!(q.controles.iter().filter(|(c, _)| matches!(c, Controle::Ladrilho(_))).count(), if cfg!(feature = "tela-estendida-futura") { 4 } else { 3 });
        let q = compor(&achar("02-espelhar-camera-com-aviso"));
        assert!(q.lugar(Controle::Microfone).is_some() && q.lugar(Controle::Som).is_none());
        let q = compor(&achar("08-esperando"));
        assert!(q.lugar(Controle::Letreiro).is_some() && q.lugar(Controle::Chip).is_some() && q.lugar(Controle::Cancelar).is_some());
        let q = compor(&achar("09-esperando-pareados-sem-anuncio"));
        assert!(q.lugar(Controle::Letreiro).is_none(), "com pares, o PIN vai na frase, sem o letreiro");
        let q = compor(&achar("10-esperando-camera"));
        assert!(q.lugar(Controle::Microfone).is_some() && q.lugar(Controle::Gravar).is_some());
        assert!(q.lugar(Controle::Esquecer).is_some(), "a dívida 22: o Esquecer na espera da câmera quando a retomada falhou");
        assert!(q.lugar(Controle::AjustesDaCamera).is_some(), "R9 §4.1: os ajustes na espera da câmera");
        let q = compor(&achar("12-no-ar-camera-gravando"));
        assert!(q.lugar(Controle::Esquecer).is_none());
        assert!(q.lugar(Controle::AjustesDaCamera).is_some(), "R9 §4.1: e no ar");
        let mut sem = achar("12-no-ar-camera-gravando");
        if let Some(c) = sem.camera.as_mut() {
            c.ajustes = false;
        }
        assert!(compor(&sem).lugar(Controle::AjustesDaCamera).is_none(), "a sintética fica sem (§2.2)");
        assert!(compor(&achar("11-no-ar")).lugar(Controle::AjustesDaCamera).is_none(), "a tela não tem ajustes de câmera");
        let q = compor(&achar("07-ajustes"));
        assert!(q.lugar(Controle::Esquecer).is_some());
        assert!(habilitado(Controle::Esquecer, &achar("07-ajustes")), "§11.1: nos Ajustes o Esquecer fica sempre à mão");
        let q = compor(&achar("16-exibindo"));
        assert!(q.lugar(Controle::SomComCamera).is_none(), "Quall Monitor não tem controles de câmera");
        assert_eq!(q.controles.iter().filter(|(c, _)| matches!(c, Controle::Volume(_))).count(), 4);
        // R9b: a engrenagem da câmera de quem filma, quando ele responde ao controle remoto.
        assert!(q.lugar(Controle::AjustesDaCamera).is_some(), "R9b: os ajustes da câmera remota no Exibindo");
        let mut sem = achar("16-exibindo");
        sem.exibindo.ajustes_da_camera = false;
        assert!(compor(&sem).lugar(Controle::AjustesDaCamera).is_none());
        let mut controlada = achar("12-no-ar-camera-gravando");
        if let Some(c) = controlada.camera.as_mut() {
            c.controlado_por = "Pixel do Bruno".into(); // i18n: fora (exemplo)
        }
        assert!(compor(&controlada).itens.iter().any(|i| matches!(i, Item::Texto(t) if t.texto_corrido().ends_with(" · Controlado por Pixel do Bruno"))));
        // Pouca luz (§3.1): ao lado da gravação, a curta; sozinha, a primeira frase, em âmbar.
        let luz = crate::regras_dos_controles::PoucaLuz { fps_agora: 15, fps: 30, com_manual: true };
        if let Some(c) = controlada.camera.as_mut() {
            c.pouca_luz = Some(luz);
        }
        assert!(compor(&controlada).itens.iter().any(|i| matches!(i, Item::Texto(t) if t.texto_corrido().ends_with(" · Controlado por Pixel do Bruno · Pouca luz: 15 fps"))));
        let mut so_luz = achar("12-no-ar-camera-gravando");
        if let Some(c) = so_luz.camera.as_mut() {
            c.linha_da_gravacao = String::new();
            c.gravando = false;
            c.pouca_luz = Some(luz);
        }
        assert!(compor(&so_luz).itens.iter().any(|i| matches!(i, Item::Texto(t) if t.texto_corrido() == "Pouca luz: 15 fps para clarear a imagem." && t.cor == AGUARDANDO_TEXTO)));
        let q = compor(&achar("07-ajustes"));
        assert!(q.lugar(Controle::SomComCamera).is_none(), "§11.5: saiu dos Ajustes");
        let q = compor(&achar("13-varios"));
        assert_eq!(q.controles.iter().filter(|(c, _)| !matches!(c, Controle::Item(_))).count(), 1, "§11.5: sem Desconectar");
    }

    #[cfg(not(feature = "tela-estendida-futura"))]
    #[test]
    fn o_release_nao_oferece_monitor_virtual_nem_instalador() {
        for idioma in [crate::idioma::Idioma::Pt, crate::idioma::Idioma::En] {
            crate::idioma::com_idioma(idioma, || {
                let fixos = Controle::fixos();
                assert!(!fixos.contains(&Controle::TelaEstendidaSemDriver));
                assert!(!fixos.contains(&Controle::DriverDaTelaEstendida));
                for (nome, e) in exemplos() {
                    assert!(!nome.contains("driver"), "{nome}");
                    assert!(e.espelhar.fontes.iter().all(|f| f.icone != Icone::TelaEstendida), "{nome}");
                    let q = compor(&e);
                    assert!(q.lugar(Controle::TelaEstendidaSemDriver).is_none(), "{nome}");
                    assert!(q.lugar(Controle::DriverDaTelaEstendida).is_none(), "{nome}");
                    assert!(q.textos().iter().all(|s| !s.contains("SudoVDA") && !s.contains("driver") && !s.contains("tela estendida") && !s.contains("extended display")), "{nome}");
                }
            });
        }
    }

    #[cfg(feature = "tela-estendida-futura")]
    #[test]
    fn a_tela_estendida_apagada_fica_depois_dos_monitores() {
        let achar = |n: &str| exemplos().into_iter().find(|(x, _)| *x == n).unwrap().1;
        let e = achar("03b-espelhar-tela-estendida-sem-driver");
        let q = compor(&e);
        let apagado = q.lugar(Controle::TelaEstendidaSemDriver).expect("o ladrilho apagado está na tela");
        let ladrilhos: Vec<Ret> = (0..3).map(|i| q.lugar(Controle::Ladrilho(i)).expect("os três ladrilhos")).collect();
        // A grade de quatro casas (duas colunas): o monitor na primeira, o apagado na segunda, as
        // duas câmeras embaixo — o mesmo lugar do escolhível no "01-espelhar".
        let antes = compor(&achar("01-espelhar"));
        assert_eq!(Some(apagado), antes.lugar(Controle::Ladrilho(1)));
        assert_eq!(Some(ladrilhos[0]), antes.lugar(Controle::Ladrilho(0)));
        assert_eq!(Some(ladrilhos[1]), antes.lugar(Controle::Ladrilho(2)));
        assert_eq!(Some(ladrilhos[2]), antes.lugar(Controle::Ladrilho(3)));
        // Não se escolhe: não é opção, e o "01-espelhar" (com o adaptador) não o mostra.
        assert_eq!(Controle::TelaEstendidaSemDriver.especie(), Especie::Botao);
        assert!(antes.lugar(Controle::TelaEstendidaSemDriver).is_none());
        assert!(!marcado_no_exemplo(Controle::TelaEstendidaSemDriver, &e));
        // O detalhe, o nome para o Narrador e o desenho apagado.
        // Sem o adaptador e fora da loja, o detalhe é o botão de instalar (02/10, noite).
        assert!(texto_acessivel(Controle::TelaEstendidaSemDriver, &e).contains("Instalar o driver da tela estendida"));
        let ap = aparencia(Controle::TelaEstendidaSemDriver, &e, 300.0, 62.0, EstadoDoControle::default());
        assert!(ap.opacidade < 0.5, "apagado: {}", ap.opacidade);
        let textos: Vec<String> = ap.itens.iter().filter_map(|i| if let Item::Texto(t) = i { Some(t.texto_corrido()) } else { None }).collect();
        assert!(textos.iter().any(|t| t == "Instalar o driver da tela estendida"), "{textos:?}");
        assert!(textos.iter().any(|t| t == "Tela estendida"), "{textos:?}");
        // Na loja, baixar o instalador avulso (02/10, noite).
        let mut loja = e.clone();
        loja.driver.situacao = crate::regras_do_driver::Situacao::Loja { presente: false };
        assert!(texto_acessivel(Controle::TelaEstendidaSemDriver, &loja).contains("Baixe o driver da tela estendida"));
        let ap = aparencia(Controle::TelaEstendidaSemDriver, &loja, 300.0, 62.0, EstadoDoControle::default());
        let textos: Vec<String> = ap.itens.iter().filter_map(|i| if let Item::Texto(t) = i { Some(t.texto_corrido()) } else { None }).collect();
        assert!(textos.iter().any(|t| t == "Baixe o driver da tela estendida"), "{textos:?}");
        // Sem fonte nenhuma, só ele, na primeira casa.
        let mut so = e.clone();
        so.espelhar.fontes.clear();
        so.espelhar.tela_estendida_sem_driver = Some(0);
        assert!(compor(&so).lugar(Controle::TelaEstendidaSemDriver).is_some());
    }

    /// **O driver da tela estendida** (02/10, noite): o aviso da instalação no Espelhar, o cartão e o
    /// botão de desinstalar nos Ajustes — só para o que o Quall instalou.
    #[cfg(feature = "tela-estendida-futura")]
    #[test]
    fn o_driver_no_espelhar_e_nos_ajustes() {
        use crate::regras_do_driver::{Acao, Andamento, Resultado, Situacao, MOTIVO_DRIVER};
        let achar = |n: &str| exemplos().into_iter().find(|(x, _)| *x == n).unwrap().1;
        // Instalando: o aviso do Espelhar diz o passo, e vence o do emissor.
        let e = achar("03c-espelhar-instalando-o-driver");
        let q = compor(&e);
        let textos = q.textos();
        assert!(textos.iter().any(|t| t.contains("passo 5 de 7: Criando o adaptador")), "{textos:?}");
        // A falha diz o passo, o motivo e o técnico.
        let mut f = e.clone();
        f.driver.andamento = Andamento::Acabou {
            acao: Acao::Instalar,
            resultado: Resultado::Falha(6, MOTIVO_DRIVER.into(), "UpdateDriverForPlugAndPlayDevicesW: 0xE0000247".into()),
        };
        let (tom, texto) = texto_do_andamento(&f.driver.andamento).unwrap();
        assert_eq!(tom, Tom::Vermelho);
        assert_eq!(
            texto,
            "Não deu para instalar o driver (Instalando o driver): o Windows não instalou o driver (UpdateDriverForPlugAndPlayDevicesW: 0xE0000247)."
        );
        let en = crate::idioma::com_idioma(crate::idioma::Idioma::En, || texto_do_andamento(&f.driver.andamento).unwrap().1);
        assert!(en.starts_with("Couldn't install the driver"), "{en}");
        assert!(en.contains("0xE0000247"), "o técnico vai como veio: {en}");
        let c = Andamento::Acabou { acao: Acao::Instalar, resultado: Resultado::CanceladoNoUac };
        assert_eq!(texto_do_andamento(&c).unwrap().0, Tom::Ambar);

        // Ajustes: o botão só com o driver do Quall, e nunca no meio de outra.
        let a = achar("07c-ajustes-driver-do-quall");
        assert!(compor(&a).lugar(Controle::DriverDaTelaEstendida).is_none(), "a desinstalação pertence ao setup");
        assert!(compor(&a).textos().iter().any(|t| t == "Instalado pelo Quall Monitor (SudoVDA). Sai ao desinstalar o aplicativo."));
        assert!(habilitado(Controle::DriverDaTelaEstendida, &a));
        for s in [Situacao::DeFora, Situacao::Ausente, Situacao::Loja { presente: false }, Situacao::Desligado, Situacao::SemSuporte] {
            let mut x = a.clone();
            x.driver.situacao = s;
            assert!(compor(&x).lugar(Controle::DriverDaTelaEstendida).is_none(), "{s:?}");
        }
        // Na loja com o SudoVDA presente: o estado, e o botão que abre a página do instalador avulso
        // (mais largo; nada se eleva dentro do app da loja).
        let mut loja = a.clone();
        loja.driver.situacao = Situacao::Loja { presente: true };
        let q = compor(&loja);
        assert_eq!(q.lugar(Controle::DriverDaTelaEstendida), Some(crate::estilo::lugar::DRIVER_LARGO));
        assert!(q.textos().iter().any(|t| t.contains("use o instalador do driver")));
        assert_eq!(texto_acessivel(Controle::DriverDaTelaEstendida, &loja), "Desinstalar pelo instalador do driver");
        let ap = aparencia(Controle::DriverDaTelaEstendida, &loja, 280.0, 32.0, EstadoDoControle::default());
        assert!(ap.itens.iter().any(|i| matches!(i, Item::Texto(t) if t.texto_corrido() == "Desinstalar pelo instalador do driver")));
        let mut rodando = a.clone();
        rodando.driver.andamento = Andamento::Rodando { acao: Acao::Desinstalar, passo: 2 };
        assert!(compor(&rodando).lugar(Controle::DriverDaTelaEstendida).is_none());
        assert!(compor(&rodando).textos().iter().any(|t| t.contains("Desinstalando o driver da tela estendida — passo 2 de 4")));
        // O Dell da bancada: instalado à mão, o Quall usa e diz que não desinstala.
        let mut dell = a.clone();
        dell.driver.situacao = Situacao::DeFora;
        assert!(compor(&dell).textos().iter().any(|t| t.contains("instalado por outro programa")));
    }

    #[test]
    fn a_grade_dos_ladrilhos() {
        assert_eq!(lugar::grade(4, 300.0), (2, 62.0));
        let (c, a) = lugar::grade(10, 300.0);
        assert_eq!(c, 2);
        assert!(a >= 44.0 && a < 62.0, "{a}");
        let (c, _) = lugar::grade(14, 250.0);
        assert!(c >= 3, "{c}");
    }

    #[test]
    fn as_frases() {
        let mut e = TelaEspera { nome: "DELL-G3".into(), anunciando: true, origem: "Monitor 1".into(), com_som: Some(true), ..Default::default() };
        let t: String = instrucao_da_espera(&e).iter().map(|x| x.texto.as_str()).collect();
        assert_eq!(
            t,
            "No outro aparelho, abra o Quall em Exibir e escolha DELL-G3 na lista, ou digite o endereço abaixo. Vai Monitor 1, com o som deste computador."
        );
        e.anunciando = false;
        e.com_som = None;
        e.origem = "Integrated Webcam".into();
        let t: String = instrucao_da_espera(&e).iter().map(|x| x.texto.as_str()).collect();
        assert_eq!(t, "No outro aparelho, abra o Quall em Exibir e digite o endereço abaixo. Vai a câmera Integrated Webcam.");
        assert_eq!(pin_em_grupos("482719"), "482 719");
        assert_eq!(pin_para_ler("482719"), "PIN 4 8 2 7 1 9");
        assert_eq!(indice_do_volume(1.0), 0);
        assert_eq!(indice_do_volume(0.6), 2);
        assert_eq!(indice_do_volume(0.1), 3);
        assert_eq!(pasta_curta("C:\\Users\\bruno\\AppData\\Local\\Quall\\Logs", Some("C:\\Users\\bruno\\AppData\\Local")), "%LOCALAPPDATA%\\Quall\\Logs");
        assert_eq!(pasta_curta("D:\\logs", Some("C:\\x")), "D:\\logs");
        assert_eq!(sem_porta("192.168.15.2:7877"), "192.168.15.2");
        assert_eq!(sem_porta("[fe80::1]:7877"), "[fe80::1]");
        assert_eq!(sem_porta("sem rede"), "sem rede");
        assert_eq!(legenda_do_gravar("■ Parar a gravação (1:23)", true, "● GRAVANDO 1:23 · sobram 12,3 GB"), "1:23");
        assert_eq!(legenda_do_gravar("■ Parar a gravação (0:05)", true, "● GRAVANDO 0:05 · Gravando SEM SOM — ligue o microfone"), "0:05 · SEM SOM");
        assert_eq!(legenda_do_gravar("Gravar (abrindo…)", true, ""), "Abrindo…");
        assert_eq!(legenda_do_gravar("■ Parar", true, ""), "Gravando");
        assert_eq!(legenda_do_gravar("Gravar (abrindo…)", false, ""), "Abrindo…");
        assert_eq!(legenda_do_gravar("Fechando o arquivo…", false, ""), "Fechando…");
        assert_eq!(legenda_do_gravar("● Gravar neste computador (sem som)", false, ""), "Gravar");
        assert_eq!(legenda_do_microfone(false, None, false), "Mic desligado");
        assert_eq!(legenda_do_microfone(true, None, false), "Mic ligado");
        assert_eq!(legenda_do_microfone(true, Some("Microfone abrindo…"), false), "Ligando…");
        assert_eq!(legenda_do_microfone(true, Some("Com o som do microfone."), false), "Mic ligado");
        assert_eq!(legenda_do_microfone(true, Some("O microfone não abriu: a thread não subiu (x)"), false), "Não abriu");
        assert_eq!(legenda_do_microfone(true, Some("O Windows não deixa…"), true), "Sem acesso");
    }

    #[test]
    fn o_estado_dos_controles_no_exemplo() {
        let e = exemplos().into_iter().find(|(n, _)| *n == "17-exibindo-detalhes-com-acusacao").unwrap().1;
        assert!(marcado_no_exemplo(Controle::Volume(2), &e));
        assert!(!marcado_no_exemplo(Controle::Volume(0), &e));
        assert!(marcado_no_exemplo(Controle::Mudo, &e));
        assert!(marcado_no_exemplo(Controle::Detalhes, &e));
        assert!(marcado_no_exemplo(Controle::Item(Painel::Exibir), &e));
    }

    #[test]
    fn os_nomes_para_o_narrador() {
        let e = exemplos().into_iter().find(|(n, _)| *n == "08-esperando").unwrap().1;
        assert_eq!(texto_acessivel(Controle::Letreiro, &e), "PIN 4 8 2 7 1 9");
        assert_eq!(texto_acessivel(Controle::Chip, &e), "Copiar o endereço 192.168.15.2:7877");
        assert_eq!(texto_acessivel(Controle::Item(Painel::Espelhar), &e), "Estender, Aguardando");
        assert_eq!(texto_acessivel(Controle::AjustesDaCamera, &e), "Ajustes da câmera");
        let copiado = EstadoDaTela { copiado: true, ..e.clone() };
        assert_eq!(texto_acessivel(Controle::Chip, &copiado), "Copiado: 192.168.15.2:7877");
        let a = exemplos().into_iter().find(|(n, _)| *n == "07b-ajustes-confirmando").unwrap().1;
        assert!(texto_acessivel(Controle::Esquecer, &a).starts_with("Esquecer de verdade"));
        for c in Controle::fixos() {
            if !matches!(c, Controle::Lista | Controle::Endereco | Controle::Pin) {
                assert!(!texto_acessivel(c, &e).is_empty(), "{c:?} sem nome");
            }
        }
    }

    #[test]
    fn a_marca_segue_a_especificacao() {
        let m = marca(0.0, 0.0, 32.0);
        assert_eq!(m[0], Item::Anel { cx: 15.0, cy: 15.0, raio: 10.5, espessura: 3.6, cor: TEXTO });
        assert_eq!(m[1], Item::Circulo { cx: 25.2, cy: 25.2, raio: 4.6, cor: NO_AR });
        assert!(largura_do_letreiro() > 6.0 * CASA_L);
    }
}
