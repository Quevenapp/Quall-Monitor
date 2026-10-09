//! **As regras puras do teleprompter no Windows**: sem Win32, sem rede, sem relógio — só contas,
//! e por isso provadas por teste de unidade (o molde de `EnderecoDoTeleprompterTest`,
//! `PoliticasTest` e `AvisosTest` do Android, e de `LacoDoTeleprompter.decidir` e `AvisosDaTela`
//! do Mac). As outras três telas já fixaram estas regras; aqui elas são portadas **igual**, para
//! que um Windows controlando um iPhone, ou um Android controlando o Windows, se comportem como
//! os aparelhos da mesma plataforma.
//!
//! O que mora aqui:
//!
//! - a **porta** do prompter e o **endereço** que o controle disca ([`porta_do_teleprompter`],
//!   [`destino_do_controle`], [`link`]);
//! - a **política do laço**: o que fazer depois de uma abertura que falhou ([`decidir`]);
//! - os **avisos** da tela ([`Avisos::calcular`]);
//! - o **rascunho** do editor e o texto que chega com ele aberto ([`Rascunho`]);
//! - os **passos** dos botões ([`passo`]) e as **ações de bancada** ([`AcoesDeBancada`]).

use quall_core::error::Error;
use quall_core::teleprompter::{Estado, PAR_SUMIDO};

// =============================================================================================
// A porta e o endereço
// =============================================================================================

/// **A porta do prompter: 7979.** A 7877 é a do espelhamento: um aparelho pode espelhar e
/// mostrar o texto ao mesmo tempo sem as duas disputarem a porta.
///
/// **Uma função só, de propósito**: nenhum outro lugar do Windows escreve 7979. Desde a §11.1 do
/// contrato a porta mora no núcleo (`discovery::PORTA_DO_TELEPROMPTER`), e esta função só a lê.
pub fn porta_do_teleprompter() -> u16 {
    quall_core::discovery::PORTA_DO_TELEPROMPTER
}

/// Quantas portas depois da preferida o prompter tenta antes de desistir de achar uma livre
/// (o mesmo número do Android, `EnderecoDoTeleprompter.PORTAS_A_TENTAR`).
pub const PORTAS_A_TENTAR: u16 = 10;

/// As portas que o prompter tenta, na ordem: a preferida e as seguintes, sem passar de 65535.
/// A casca tenta **abrir** cada uma (o `bind` do servidor de sinalização é o próprio teste) e
/// fica com a primeira que abriu — sem a corrida entre "testei" e "abri".
pub fn portas_candidatas(preferida: u16) -> Vec<u16> {
    let fim = (u32::from(preferida) + u32::from(PORTAS_A_TENTAR)).min(65_536);
    (u32::from(preferida)..fim).map(|p| p as u16).collect()
}

/// Para onde o controle disca: `host:porta` (IPv6 entre colchetes), e o PIN, se veio no link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destino {
    pub endereco: String,
    pub pin: Option<String>,
}

/// O que a pessoa digitou (ou colou) no campo do endereço → para onde discar. `None` se não dá
/// para entender (vazio, porta fora de 1..65535, link sem host). A regra das quatro cascas:
///
/// - `192.168.15.8` → `192.168.15.8:7979` (a porta do teleprompter, **não** a 7877);
/// - `192.168.15.8:8000` → como está;
/// - `quall-944d0e.local` → `quall-944d0e.local:7979`;
/// - `2804:1b1::1` → `[2804:1b1::1]:7979`; `[2804:1b1::1]:8000` → como está;
/// - `quall://424242@192.168.15.8:7979` → `192.168.15.8:7979` com o PIN 424242. **Só tolerância
///   de entrada**: nenhuma tela do Quall mostra mais esse link (o QR saiu das quatro cascas em
///   24/09), mas quem colar um link antigo continua sendo entendido. Não anuncie o formato em texto
///   nem em dica.
pub fn destino_do_controle(digitado: &str) -> Option<Destino> {
    let mut t = digitado.trim().to_string();
    if t.is_empty() {
        return None;
    }
    let mut pin = None;
    if t.len() >= 8 && t[..8].eq_ignore_ascii_case("quall://") {
        t = t[8..].trim_end_matches('/').to_string();
        if let Some(arroba) = t.rfind('@') {
            let p = t[..arroba].trim();
            if p.len() == 6 && p.chars().all(|c| c.is_ascii_digit()) {
                pin = Some(p.to_string());
            }
            t = t[arroba + 1..].to_string();
        }
        if t.is_empty() {
            return None;
        }
    }
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    Some(Destino { endereco: completar(&t)?, pin })
}

/// `host` ou `host:porta` → `host:porta`, com [`porta_do_teleprompter`] quando falta.
pub fn completar(host_ou_host_porta: &str) -> Option<String> {
    let t = host_ou_host_porta.trim();
    if t.is_empty() {
        return None;
    }
    let padrao = porta_do_teleprompter();
    if let Some(resto) = t.strip_prefix('[') {
        let fecha = resto.find(']')?;
        let host = &t[..fecha + 2];
        let depois = &resto[fecha + 1..];
        if depois.is_empty() {
            return Some(format!("{host}:{padrao}"));
        }
        let porta = porta_valida(depois.strip_prefix(':')?)?;
        return Some(format!("{host}:{porta}"));
    }
    match t.matches(':').count() {
        0 => Some(format!("{t}:{padrao}")),
        // Mais de um ":" sem colchetes é IPv6 puro: não tem como trazer porta junto.
        1 => {
            let (host, porta) = t.split_once(':')?;
            if host.is_empty() {
                return None;
            }
            Some(format!("{host}:{}", porta_valida(porta)?))
        }
        _ => Some(format!("[{t}]:{padrao}")),
    }
}

fn porta_valida(texto: &str) -> Option<u16> {
    texto.parse::<u16>().ok().filter(|p| *p >= 1)
}

// =============================================================================================
// A política do laço
// =============================================================================================

/// O papel desta tela. Não é o `Papel` do fio: é o lado da regra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lado {
    Prompter,
    Controle,
}

/// O código de uma falha, com os nomes da fronteira C (`QuallStatus`), para a regra ramificar
/// **pelo código** — nunca pelo texto, que o núcleo pode reescrever sem aviso (dívida 29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codigo {
    Invalido,
    Protocolo,
    Descoberta,
    Sinalizacao,
    Transporte,
    SemRota,
    Pareamento,
    PinErrado,
    PrecisaDePin,
    Prazo,
    Fechado,
    Cancelado,
    Io,
    Ocupado,
}

impl Codigo {
    pub fn de(e: &Error) -> Codigo {
        match e {
            Error::Invalid(_) => Codigo::Invalido,
            Error::Protocol(_) => Codigo::Protocolo,
            Error::Discovery(_) => Codigo::Descoberta,
            Error::Signaling(_) => Codigo::Sinalizacao,
            Error::Transport(_) => Codigo::Transporte,
            Error::NoRoute(_) => Codigo::SemRota,
            Error::Pairing(_) => Codigo::Pareamento,
            Error::WrongPin(_) => Codigo::PinErrado,
            Error::NeedsPin(_) => Codigo::PrecisaDePin,
            Error::Timeout(_) => Codigo::Prazo,
            Error::Closed => Codigo::Fechado,
            Error::Cancelled => Codigo::Cancelado,
            Error::Io(_) => Codigo::Io,
            Error::Ocupado(_) => Codigo::Ocupado,
        }
    }

    /// O nome que a fronteira C devolveria em `quall_last_status()` — o mesmo que a `quall-probe`
    /// imprime, para o registro das duas pontas falar a mesma língua.
    pub fn nome(self) -> &'static str {
        match self {
            Codigo::Invalido => "INVALID",
            Codigo::Protocolo => "PROTOCOL",
            Codigo::Descoberta => "DISCOVERY",
            Codigo::Sinalizacao => "SIGNALING",
            Codigo::Transporte => "TRANSPORT",
            Codigo::SemRota => "NO_ROUTE",
            Codigo::Pareamento => "PAIRING",
            Codigo::PinErrado => "WRONG_PIN",
            Codigo::PrecisaDePin => "NEEDS_PIN",
            Codigo::Prazo => "TIMEOUT",
            Codigo::Fechado => "CLOSED",
            Codigo::Cancelado => "CANCELLED",
            Codigo::Io => "IO",
            Codigo::Ocupado => "BUSY",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscolhaDoPin {
    /// O mesmo PIN — nenhuma tentativa de PIN foi gasta.
    Mesmo,
    /// PIN novo: depois de `WRONG_PIN` ou `PAIRING` numa espera. Repetir o PIN abriria força
    /// bruta — seis dígitos são segurados por uma tentativa por conexão (contrato §2).
    Novo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decisao {
    TentarDeNovo { pin: EscolhaDoPin, depois_ms: u64 },
    Parar,
}

/// Quantos erros de PIN seguidos o prompter aceita antes de parar de reabrir a espera sozinho.
pub const ERROS_DE_PIN_ATE_PARAR: u32 = 5;
/// Quantas falhas de sinalização seguidas antes de parar: é defeito que não se cura insistindo.
pub const FALHAS_DE_SINALIZACAO_ATE_PARAR: u32 = 3;
/// Por quanto tempo o controle insiste diante de `BUSY` **antes** de a primeira sessão subir.
/// Depois de uma queda insiste até entrar: é o controle de volta ouvindo "ocupado" enquanto o
/// prompter não percebe a queda (até 5 s, `SILENCIO_DO_TELEPROMPTER`).
pub const INSISTIR_NO_OCUPADO_MS: u64 = 12_000;

/// O que a regra precisa saber de uma abertura que falhou.
#[derive(Debug, Clone, Copy)]
pub struct Falha {
    pub codigo: Codigo,
    /// Esta tela já teve uma sessão de pé (a falha é da volta, e não da primeira conexão).
    pub ja_subiu: bool,
    /// Quanto a tentativa que falhou durou.
    pub durou_ms: u64,
    /// Há quanto tempo as tentativas vêm falhando, sem nenhuma subir.
    pub falhando_ha_ms: u64,
    pub falhas_seguidas: u32,
    pub erros_de_pin_seguidos: u32,
    /// A pessoa pediu para parar.
    pub parando: bool,
}

/// **A regra depois de uma abertura que falhou** — a tabela de `LacoDoTeleprompter.decidir` do
/// Mac, a mesma nas quatro telas:
///
/// | lado | código | o que fazer |
/// |---|---|---|
/// | os dois | `CANCELLED`, ou a pessoa parando | parar |
/// | prompter | `WRONG_PIN`, `PAIRING` | **PIN novo**, espera crescente 1, 2, 4, 8 s; no 5.º erro seguido, parar |
/// | prompter | `INVALID`, `NOT_UTF8`… | parar: é defeito, não rede |
/// | prompter | `SIGNALING` | mesmo PIN em 1 s; na 3.ª seguida, parar |
/// | prompter | outro (prazo, versão, porta…) | mesmo PIN, de novo |
/// | controle | `BUSY` | de novo em ~1 s (até 12 s na primeira vez; depois de uma queda, sempre) |
/// | controle | `WRONG_PIN`, `NEEDS_PIN`, `PAIRING`, `PROTOCOL`, `SIGNALING`, `INVALID` | parar: precisa da pessoa |
/// | controle | rede (`IO`, `TIMEOUT`, `NO_ROUTE`, `TRANSPORT`…) | na primeira vez, parar e dizer; depois de uma queda, de novo a cada ~1 s |
///
/// Uma falha em menos de 1 s espera o resto do segundo antes da próxima: sem isso uma porta
/// recusada vira laço quente. A espera crescente do PIN errado é o que segura seis dígitos: trocar
/// o PIN não muda a chance de cada tentativa; o que custa é cada tentativa custar uma conexão e
/// uma espera.
pub fn decidir(lado: Lado, f: Falha) -> Decisao {
    if f.parando || f.codigo == Codigo::Cancelado {
        return Decisao::Parar;
    }
    let resto = 1000u64.saturating_sub(f.durou_ms);
    match lado {
        Lado::Prompter => match f.codigo {
            Codigo::PinErrado | Codigo::Pareamento => {
                let n = f.erros_de_pin_seguidos.max(1);
                if n >= ERROS_DE_PIN_ATE_PARAR {
                    return Decisao::Parar;
                }
                let espera = (1000u64 << (n - 1).min(6)).min(60_000);
                Decisao::TentarDeNovo { pin: EscolhaDoPin::Novo, depois_ms: resto.max(espera) }
            }
            Codigo::Invalido => Decisao::Parar,
            Codigo::Sinalizacao => {
                if f.falhas_seguidas >= FALHAS_DE_SINALIZACAO_ATE_PARAR {
                    Decisao::Parar
                } else {
                    Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: resto.max(1000) }
                }
            }
            _ => Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: resto },
        },
        Lado::Controle => match f.codigo {
            Codigo::Ocupado => {
                if f.ja_subiu || f.falhando_ha_ms < INSISTIR_NO_OCUPADO_MS {
                    Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: resto }
                } else {
                    Decisao::Parar
                }
            }
            Codigo::PinErrado
            | Codigo::PrecisaDePin
            | Codigo::Pareamento
            | Codigo::Protocolo
            | Codigo::Sinalizacao
            | Codigo::Invalido => Decisao::Parar,
            _ if f.ja_subiu => Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: resto },
            _ => Decisao::Parar,
        },
    }
}

/// **A frase da tela para cada falha**, pelo código. `motivo` (o texto do núcleo) só entra como
/// complemento. Vazio: nada a dizer (um prazo que venceu, uma parada pedida).
pub fn conselho(lado: Lado, f: &Falha, decisao: Decisao, porta: u16, motivo: &str) -> String {
    use crate::idioma::{t, tf};
    let parou = decisao == Decisao::Parar;
    match lado {
        Lado::Prompter => match f.codigo {
            Codigo::PinErrado | Codigo::Pareamento => {
                if parou {
                    t("Cinco tentativas seguidas com o PIN errado: a espera parou, para ninguém ficar adivinhando o PIN. Clique em Esperar de novo.").into()
                } else {
                    t("Um aparelho tentou entrar com o PIN errado. O PIN mudou: passe os seis dígitos novos.").into()
                }
            }
            Codigo::Prazo | Codigo::Cancelado => String::new(),
            Codigo::PrecisaDePin => {
                t("Um aparelho tentou entrar com um pareamento que este computador não reconhece mais: ele precisa digitar o PIN.").into()
            }
            Codigo::Protocolo => t("Um aparelho que não é controle do Quall, ou de outra versão, tentou entrar e foi recusado.").into(),
            // O motivo é o texto do núcleo (o detalhe técnico): vai como veio, dentro da frase.
            Codigo::Io => tf("A porta {} falhou ({}). Tentando de novo.", &[&porta, &motivo]),
            _ if parou => tf("A espera parou: {}", &[&motivo]),
            _ => motivo.to_string(),
        },
        Lado::Controle => match f.codigo {
            Codigo::PinErrado => {
                t("PIN errado. Confira os seis dígitos na tela do prompter e conecte de novo — o PIN do prompter muda depois de um erro.").into()
            }
            Codigo::PrecisaDePin => t("O prompter não reconhece mais este computador. Digite o PIN que a tela dele mostra.").into(),
            Codigo::Pareamento => t("O pareamento não fechou. Confira o PIN na tela do prompter e tente de novo.").into(),
            Codigo::Protocolo | Codigo::Sinalizacao => tf("Esse aparelho não aceitou este computador como controle: {}", &[&motivo]),
            Codigo::Ocupado => {
                if parou {
                    t("O prompter já tem outro controle conectado.").into()
                } else {
                    t("O prompter ainda está com a sessão anterior — tentando de novo (ele percebe a queda em até 5 s).").into()
                }
            }
            Codigo::Cancelado => String::new(),
            _ if f.ja_subiu => t("Conexão perdida — tentando de novo. Os comandos vão quando a conexão voltar.").into(),
            Codigo::SemRota => t("Os dois aparelhos não acharam caminho um até o outro. Confira se estão na mesma rede.").into(),
            Codigo::Io | Codigo::Prazo => {
                t("Ninguém atendeu neste endereço. Confira se o prompter está aberto e se o endereço e a porta estão certos.").into()
            }
            _ => tf("Não deu para conectar: {}", &[&motivo]),
        },
    }
}

// =============================================================================================
// Os avisos
// =============================================================================================

/// Em que pé está a ligação com o outro aparelho, **do ponto de vista da tela** (o `LigacaoDaTela`
/// do Mac). A diferença que importa é entre "nunca houve sessão" (o prompter esperando o primeiro
/// controle, com o PIN) e "houve, e caiu" — só a segunda é "controle sumido".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ligacao {
    SemSessao,
    /// Uma sessão de pé desde `desde_s` (segundos monotônicos). `depois_de_queda`: é a volta de
    /// um par que tinha caído — o aviso só some com a primeira mensagem dele (§2).
    Conectada { desde_s: f64, depois_de_queda: bool },
    Caiu,
}

/// A confirmação: acima disto sem voltar, a tela diz que o comando não chegou (contrato §3).
pub const SEM_CONFIRMACAO_MS: u64 = 1500;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Avisos {
    /// O outro lado sumiu: "controle sumido" no prompter; "o prompter não responde" ou
    /// "conexão perdida" no controle.
    pub par_sumido: bool,
    /// Uma edição daqui não voltou confirmada em 1,5 s: "o comando não chegou".
    pub sem_confirmacao: bool,
    /// O outro lado fala outra versão do contrato (`v` ≠ 1): "atualize o app".
    pub atualize_o_app: bool,
    /// Carimbos mais de 24 h à frente foram recusados: o relógio de um dos dois está errado.
    pub relogio_errado: bool,
    /// O roteiro foi reenviado 20 vezes e o outro lado continuou sem ele.
    pub texto_nao_passou: bool,
}

impl Avisos {
    /// # O intervalo que piscaria errado
    ///
    /// `par_visto_ha_ms` é nulo **antes da primeira mensagem de uma sessão nova** — e também
    /// depois de `perdeu_o_par`. Na primeira sessão da tela, nulo só vale como sumido depois de
    /// 2,5 s de sessão sem mensagem nenhuma; na volta depois de uma queda, o aviso aceso continua
    /// até a primeira mensagem do par, e não até o TCP subir (§2).
    pub fn calcular(e: &Estado, ligacao: Ligacao, agora_s: f64) -> Avisos {
        let sumido_ms = PAR_SUMIDO.as_millis() as u64;
        let mut a = Avisos::default();
        match ligacao {
            Ligacao::SemSessao => {}
            Ligacao::Caiu => a.par_sumido = true,
            Ligacao::Conectada { desde_s, depois_de_queda } => {
                a.par_sumido = match e.par_visto_ha_ms {
                    Some(visto) => visto > sumido_ms,
                    None => depois_de_queda || (agora_s - desde_s) * 1000.0 > sumido_ms as f64,
                };
                if let Some(pendente) = e.sem_confirmacao_ha_ms {
                    a.sem_confirmacao = pendente > SEM_CONFIRMACAO_MS;
                }
            }
        }
        a.atualize_o_app = e.contadores.de_outra_versao > 0;
        a.relogio_errado = e.contadores.carimbos_do_futuro > 0;
        a.texto_nao_passou = e.contadores.reenvios_desistidos > 0;
        a
    }
}

// =============================================================================================
// O rascunho do editor
// =============================================================================================

/// **O editor do roteiro, e o texto que chega do outro lado enquanto ele está aberto** — a regra
/// das telas (o contrato a deixa para elas, §6), igual à do Mac (`RascunhoDoTexto`) e à do
/// Android (`EdicaoDoTexto`):
///
/// 1. o rascunho **nunca** é tocado por fora;
/// 2. sem conflito (a pessoa não mudou nada), o texto novo entra no editor em silêncio;
/// 3. com conflito, aviso e escolha: **usar o texto novo** (o rascunho descartado vai para a área
///    de transferência) ou **manter o meu** (ao confirmar, o daqui vence nos dois, por ser o mais
///    recente);
/// 4. confirmar é o único envio (`definir_texto` só ao confirmar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rascunho {
    base: String,
    pub rascunho: String,
    texto_novo_do_outro_lado: Option<String>,
    atualizado_pelo_outro_lado: bool,
}

impl Rascunho {
    pub fn novo(texto_atual: &str) -> Rascunho {
        Rascunho {
            base: texto_atual.to_string(),
            rascunho: texto_atual.to_string(),
            texto_novo_do_outro_lado: None,
            atualizado_pelo_outro_lado: false,
        }
    }

    pub fn alterado(&self) -> bool {
        self.rascunho != self.base
    }

    pub fn em_conflito(&self) -> bool {
        self.texto_novo_do_outro_lado.is_some()
    }

    pub fn atualizado_pelo_outro_lado(&self) -> bool {
        self.atualizado_pelo_outro_lado
    }

    /// Chegou texto do outro lado com o editor aberto, e este é o texto da réplica agora.
    pub fn chegou(&mut self, texto_novo: &str) {
        if texto_novo == self.base && !self.em_conflito() {
            return;
        }
        if !self.alterado() && !self.em_conflito() {
            self.base = texto_novo.to_string();
            self.rascunho = texto_novo.to_string();
            self.atualizado_pelo_outro_lado = true;
            return;
        }
        // O outro lado chegou exatamente ao que a pessoa está escrevendo: nada a escolher.
        if texto_novo == self.rascunho {
            self.base = texto_novo.to_string();
            self.texto_novo_do_outro_lado = None;
            return;
        }
        self.texto_novo_do_outro_lado = Some(texto_novo.to_string());
    }

    /// **Usar o texto novo.** Devolve o rascunho descartado (para a área de transferência).
    pub fn usar_o_texto_novo(&mut self) -> Option<String> {
        let novo = self.texto_novo_do_outro_lado.take()?;
        let descartado = std::mem::replace(&mut self.rascunho, novo.clone());
        self.base = novo;
        self.atualizado_pelo_outro_lado = true;
        Some(descartado)
    }

    /// **Manter o meu.** O aviso some; confirmar manda o rascunho. A base passa a ser o texto do
    /// outro lado: um terceiro texto que chegue depois volta a perguntar.
    pub fn manter_o_meu(&mut self) {
        if let Some(novo) = self.texto_novo_do_outro_lado.take() {
            self.base = novo;
        }
    }

    /// O texto a mandar ao confirmar, ou `None` quando o rascunho já é o texto da réplica. O NUL
    /// sai: o núcleo recusaria o texto inteiro por ele.
    pub fn para_confirmar(&self, texto_da_replica: &str) -> Option<String> {
        let limpo = self.rascunho.replace('\0', "");
        (limpo != texto_da_replica).then_some(limpo)
    }

    pub fn bytes(&self) -> usize {
        self.rascunho.len()
    }
}

// =============================================================================================
// Os passos dos botões
// =============================================================================================

/// As faixas da §3 do contrato. Fora delas o núcleo recusa (`Invalid`) e nada muda — então o
/// botão para na borda em vez de pedir o impossível.
pub const FAIXA_DA_VELOCIDADE: (f64, f64) = (0.05, 20.0);
pub const FAIXA_DA_FONTE: (f64, f64) = (8.0, 400.0);
pub const FAIXA_DA_MARGEM: (f64, f64) = (0.0, 0.45);
pub const FAIXA_DA_LINHA: (f64, f64) = (0.0, 1.0);

/// Os passos do Android (`Ajustes`), os mesmos nas duas telas.
pub const PASSO_DA_VELOCIDADE: f64 = 0.1;
pub const PASSO_DA_FONTE: f64 = 4.0;
pub const PASSO_DA_MARGEM: f64 = 0.02;
/// "Os botões da linha andam 1 %" (`docs/teleprompter-ajustes-locais.md` §4).
pub const PASSO_DA_LINHA: f64 = 0.01;
/// Quanto "pular" anda: 5 % do percurso do prompter (a posição é fração, não linha).
pub const PULO: f64 = 0.05;

/// Um passo, arredondado ao próprio passo e preso na faixa: `0.1 + 0.2` não vira
/// `0.30000000004` na tela.
pub fn passo(atual: f64, passo: f64, sentido: i32, faixa: (f64, f64)) -> f64 {
    let alvo = atual + passo * f64::from(sentido);
    // `3 × 0,1` em `f64` é 0,30000000000000004: o último arredondamento, na resolução mais fina
    // do contrato (1/10000), é o que devolve 0,3.
    let no_passo = ((alvo / passo).round() * passo * 10_000.0).round() / 10_000.0;
    no_passo.clamp(faixa.0, faixa.1)
}

/// **O limite de envios de um arrasto** (as setas da linha de leitura, pedido do usuário de
/// 14/09): no máximo um envio a cada [`LimiteDeEnvio::INTERVALO_S`], e **o último sempre sai** — ao
/// soltar, ou no tique seguinte ao intervalo. Sem isto, um arrasto de meio segundo mandaria dezenas
/// de estados, e cada um pede ao prompter um redesenho e ao outro lado uma confirmação.
#[derive(Debug, Clone, Default)]
pub struct LimiteDeEnvio {
    ultimo_envio_s: Option<f64>,
    pendente: Option<f64>,
    pub envios: u32,
}

impl LimiteDeEnvio {
    pub const INTERVALO_S: f64 = 0.120;

    /// O ponteiro andou para `valor`: devolve o que mandar agora, se já pode.
    pub fn mover(&mut self, agora_s: f64, valor: f64) -> Option<f64> {
        if self.ultimo_envio_s.is_none_or(|t| agora_s - t >= Self::INTERVALO_S) {
            self.pendente = None;
            self.ultimo_envio_s = Some(agora_s);
            self.envios += 1;
            Some(valor)
        } else {
            self.pendente = Some(valor);
            None
        }
    }

    /// Sem movimento novo: o valor que ficou esperando sai quando o intervalo vence.
    pub fn tique(&mut self, agora_s: f64) -> Option<f64> {
        let v = self.pendente?;
        if self.ultimo_envio_s.is_none_or(|t| agora_s - t >= Self::INTERVALO_S) {
            self.pendente = None;
            self.ultimo_envio_s = Some(agora_s);
            self.envios += 1;
            Some(v)
        } else {
            None
        }
    }

    /// Soltou: o valor final sai **sempre**, dentro ou fora do intervalo.
    pub fn soltar(&mut self, agora_s: f64, valor: f64) -> f64 {
        self.pendente = None;
        self.ultimo_envio_s = Some(agora_s);
        self.envios += 1;
        valor
    }
}

// =============================================================================================
// Os ajustes locais do prompter (`docs/teleprompter-ajustes-locais.md`)
// =============================================================================================

/// **"Enquadramento"** (§2): duas setas laterais, **independentes**, **locais** (só neste
/// aparelho, fora do salvo do núcleo; o controle não as vê), com o texto **centralizado** entre
/// elas. A área entre as setas é a "vista do texto" do contrato: a `margem` sincronizada é fração
/// **dessa** largura, de cada lado.
///
/// Guardadas como fração da largura da vista, **por orientação** — o enquadramento em paisagem não é
/// o de retrato. No Windows a orientação é a da janela (um monitor girado pelo sistema a deixa em
/// retrato).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Enquadramento {
    pub paisagem: (f64, f64),
    pub retrato: (f64, f64),
}

impl Default for Enquadramento {
    /// Padrão: a largura inteira.
    fn default() -> Self {
        Enquadramento { paisagem: (0.0, 1.0), retrato: (0.0, 1.0) }
    }
}

/// A menor largura entre as setas: abaixo disto não cabe palavra nenhuma numa fonte legível.
pub const ENQUADRAMENTO_MINIMO: f64 = 0.2;

impl Enquadramento {
    fn e_paisagem(largura: f64, altura: f64) -> bool {
        largura >= altura
    }

    /// As duas setas para uma vista deste tamanho.
    pub fn par(&self, largura: f64, altura: f64) -> (f64, f64) {
        if Self::e_paisagem(largura, altura) {
            self.paisagem
        } else {
            self.retrato
        }
    }

    /// Move **uma** seta (`0` = esquerda, `1` = direita), sem passar da outra: as duas ficam em
    /// 0..1 e a pelo menos [`ENQUADRAMENTO_MINIMO`] uma da outra.
    pub fn mover(&mut self, largura: f64, altura: f64, qual: usize, fracao: f64) {
        let par = if Self::e_paisagem(largura, altura) { &mut self.paisagem } else { &mut self.retrato };
        let f = if fracao.is_finite() { fracao.clamp(0.0, 1.0) } else { return };
        if qual == 0 {
            par.0 = f.min(par.1 - ENQUADRAMENTO_MINIMO).max(0.0);
        } else {
            par.1 = f.max(par.0 + ENQUADRAMENTO_MINIMO).min(1.0);
        }
    }

    /// Onde o texto fica, em pixels da vista: `(x de onde a coluna de texto começa, largura dela)`.
    /// A coluna é a área entre as setas menos a `margem` de cada lado.
    pub fn coluna(par: (f64, f64), largura_px: f64, margem: f64) -> (f64, f64) {
        let esquerda = par.0 * largura_px;
        let entre = ((par.1 - par.0) * largura_px).max(1.0);
        let m = margem.clamp(0.0, 0.45) * entre;
        (esquerda + m, (entre - 2.0 * m).max(1.0))
    }
}

/// A partir de quantas **letras** ([`letras`]: só letras) uma palavra sozinha numa linha não conta
/// contra a fonte (§5, corrigido em 14/09: a exceção **não depende da fonte**).
pub const LETRAS_DA_PALAVRA_LONGA: usize = 12;

/// **"Fonte automática"** (§5): a regra de uma linha. Uma linha **com uma palavra só** é proibida,
/// com duas exceções que não contam: a **última linha de um parágrafo** (a palavra que sobra no fim)
/// e a linha cuja única palavra é **longa**, de [`LETRAS_DA_PALAVRA_LONGA`] letras ou mais
/// ("responsabilidade", "desenvolvimento"). A primeira versão (Mac) usava "mais de meia largura", e
/// em fonte grande quase toda palavra ocupa meia largura — "vamos" ficava sozinha numa linha.
pub fn linha_proibida(palavras: usize, ultima_do_paragrafo: bool, letras_da_palavra: usize) -> bool {
    palavras == 1 && !ultima_do_paragrafo && letras_da_palavra < LETRAS_DA_PALAVRA_LONGA
}

/// Quantas **letras** há num trecho (UTF-16): só letras — nem a pontuação grudada na palavra nem
/// algarismo contam (`docs/teleprompter-ajustes-locais.md` §5, a definição das quatro telas).
pub fn letras(trecho: &[u16]) -> usize {
    char::decode_utf16(trecho.iter().copied()).filter(|c| c.as_ref().is_ok_and(|c| c.is_alphabetic())).count()
}

/// As palavras de uma linha (UTF-16, como o DirectWrite a devolve): quantas são e onde está a
/// primeira (`início`, `comprimento`, em unidades UTF-16). Palavra é um trecho sem espaço **com pelo
/// menos uma letra ou algarismo** — um travessão ou reticências sozinhos não fazem uma linha "ter
/// duas palavras".
pub fn palavras_da_linha(linha: &[u16]) -> (usize, Option<(usize, usize)>) {
    let mut n = 0;
    let mut primeira = None;
    let mut inicio: Option<usize> = None;
    let mut tem_letra = false;
    let mut pos = 0usize;
    let mut fechar = |inicio: &mut Option<usize>, tem_letra: &mut bool, fim: usize| {
        if let Some(i) = inicio.take() {
            if *tem_letra {
                n += 1;
                if primeira.is_none() {
                    primeira = Some((i, fim - i));
                }
            }
        }
        *tem_letra = false;
    };
    for c in char::decode_utf16(linha.iter().copied()) {
        let c = c.unwrap_or('\u{FFFD}');
        let largura = c.len_utf16();
        if c.is_whitespace() {
            fechar(&mut inicio, &mut tem_letra, pos);
        } else {
            if inicio.is_none() {
                inicio = Some(pos);
            }
            if c.is_alphanumeric() {
                tem_letra = true;
            }
        }
        pos += largura;
    }
    fechar(&mut inicio, &mut tem_letra, pos);
    (n, primeira)
}

/// Uma linha como a quebra do DirectWrite a devolveu, para a regra da fonte automática.
#[derive(Debug, Clone, Copy)]
pub struct LinhaQuebrada<'a> {
    /// O texto da linha, com o espaço do fim e sem a quebra de parágrafo.
    pub texto: &'a [u16],
    /// A linha fecha um parágrafo (acaba em quebra) ou o roteiro.
    pub fim_de_paragrafo: bool,
}

fn letra_ou_algarismo(u: Option<&u16>) -> bool {
    u.and_then(|u| char::from_u32(u32::from(*u))).is_some_and(|c| c.is_alphanumeric())
}

/// **A regra da fonte automática no roteiro inteiro**: a primeira linha que reprova, ou `None` se
/// todas passam. Reprova:
///
/// - a linha **com uma palavra só** ([`linha_proibida`]), com as duas exceções (fim de parágrafo e
///   palavra de 12 letras ou mais);
/// - a linha que acaba **no meio de uma palavra** (letra no fim, letra no começo da seguinte, sem
///   espaço nem quebra entre elas): é a quebra de emergência de uma palavra que não cabe na coluna.
///   Não está escrita no pedido; é o que impede uma palavra longa de sair partida em fonte enorme
///   (o pedaço de 12 letras ou mais passaria pela exceção da palavra longa).
pub fn primeira_linha_reprovada(linhas: &[LinhaQuebrada]) -> Option<usize> {
    for (i, l) in linhas.iter().enumerate() {
        if !l.fim_de_paragrafo {
            if let Some(proxima) = linhas.get(i + 1) {
                if letra_ou_algarismo(l.texto.last()) && letra_ou_algarismo(proxima.texto.first()) {
                    return Some(i);
                }
            }
        }
        let (n, primeira) = palavras_da_linha(l.texto);
        if n == 1 && !l.fim_de_paragrafo {
            let (inicio, comprimento) = primeira.expect("uma palavra tem começo");
            if linha_proibida(1, false, letras(&l.texto[inicio..inicio + comprimento])) {
                return Some(i);
            }
        }
    }
    None
}

/// A faixa e a resolução da busca (§5): de 8 a 400 pontos, de 1 em 1.
pub const FONTE_AUTOMATICA_FAIXA: (f64, f64) = (8.0, 400.0);

/// **A maior fonte que passa**, por busca binária de 1 em 1 ponto entre 8 e 400: `passa(f)` diz se,
/// na fonte `f`, nenhuma linha reprova. Supõe o que a quebra faz — fonte maior, linhas mais curtas
/// em palavras —, e a fonte devolvida **passou de fato** (a busca só aceita o que foi testado).
/// `None` se nem 8 pontos passam (a fonte fica como está). Devolve também quantas vezes perguntou:
/// cada pergunta é uma quebra do roteiro inteiro.
pub fn maior_fonte_que_passa(mut passa: impl FnMut(f64) -> bool) -> (Option<f64>, u32) {
    let (mut baixo, mut alto) = (FONTE_AUTOMATICA_FAIXA.0 as i32, FONTE_AUTOMATICA_FAIXA.1 as i32);
    let mut perguntas = 1;
    if !passa(f64::from(baixo)) {
        return (None, perguntas);
    }
    // Invariante: `baixo` passa; procura a maior que passa em (baixo, alto].
    while baixo < alto {
        let meio = (baixo + alto + 1) / 2;
        perguntas += 1;
        if passa(f64::from(meio)) {
            baixo = meio;
        } else {
            alto = meio - 1;
        }
    }
    (Some(f64::from(baixo)), perguntas)
}

/// Os ajustes locais guardados no aparelho (fora do salvo do núcleo).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AjustesLocais {
    #[serde(default)]
    pub enquadramento: Enquadramento,
    #[serde(default)]
    pub fonte_automatica: bool,
    /// O controle no modo **"Segurar para rolar"** (§12 do contrato): ajuste local **do
    /// controle**, guardado no aparelho — o mesmo arquivo, porque é deste aparelho.
    #[serde(default)]
    pub segurar_para_rolar: bool,
    /// **"Inverter botões"** no modo segurar: local do controle, junto com a opção do modo; vale
    /// para qualquer prompter.
    #[serde(default)]
    pub inverter_botoes: bool,
    /// **A tela R5** (`docs/teleprompter-com-camera.md` §2.5): o lado do texto (`cima`, `baixo`,
    /// `esquerda`, `direita`; ausente é em cima), a fração dele (ausente é 50 %), a prévia como
    /// espelho (ausente é ligada) e a câmera lembrada (o link). Ajustes deste aparelho.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r5_lado: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r5_fracao: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r5_previa_espelhada: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r5_camera: Option<String>,
    /// A tela R5 desligou o espelho do texto ao abrir e o deve de volta (a dívida sobrevive à morte do
    /// processo: o prompter comum a paga na abertura seguinte, como no Android, §8.6 defeito 3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub r5_espelho_devido: bool,
    /// **A tela cheia lembrada**, uma por tela (o pedido do Bruno de 02/10): quem saiu do prompter
    /// ("texto") ou da tela R5 ("texto e câmera") em tela cheia volta a ela na abertura seguinte
    /// daquela tela. Ausente é em janela.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prompter_tela_cheia: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub r5_tela_cheia: bool,
}

impl AjustesLocais {
    /// Ilegível ou ausente: o padrão (a largura inteira, fonte automática desligada).
    pub fn de_json(texto: &str) -> AjustesLocais {
        serde_json::from_str::<AjustesLocais>(texto)
            .map(|mut a| {
                // Um arquivo mexido à mão não pode deixar as setas trocadas ou fora da faixa.
                for par in [&mut a.enquadramento.paisagem, &mut a.enquadramento.retrato] {
                    let ok = par.0.is_finite() && par.1.is_finite() && par.0 >= 0.0 && par.1 <= 1.0
                        && par.1 - par.0 >= ENQUADRAMENTO_MINIMO - 1e-9;
                    if !ok {
                        *par = (0.0, 1.0);
                    }
                }
                a
            })
            .unwrap_or_default()
    }

    pub fn para_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// A tela cheia lembrada da tela: o prompter (`com_camera = false`) ou a tela R5.
    pub fn tela_cheia(&self, com_camera: bool) -> bool {
        if com_camera {
            self.r5_tela_cheia
        } else {
            self.prompter_tela_cheia
        }
    }

    /// Guarda a tela cheia da tela; devolve se mudou (só então o arquivo é regravado).
    pub fn lembrar_tela_cheia(&mut self, com_camera: bool, ligada: bool) -> bool {
        let campo = if com_camera { &mut self.r5_tela_cheia } else { &mut self.prompter_tela_cheia };
        let mudou = *campo != ligada;
        *campo = ligada;
        mudou
    }
}

// =============================================================================================
// A tela cheia do prompter e da tela R5 (o pedido de 02/10)
// =============================================================================================

/// O nome do botão da tela cheia (o Narrador e a dica do mouse), pelo estado de agora: o botão diz
/// o que ele faz.
pub fn rotulo_da_tela_cheia(em_tela_cheia: bool) -> &'static str {
    if em_tela_cheia {
        "Sair da tela cheia" // i18n: chave (traduzido ao mostrar)
    } else {
        "Tela cheia" // i18n: chave
    }
}

/// O glifo do botão, pelo mesmo estado: as setas para fora entram, as para dentro saem.
pub fn icone_da_tela_cheia(em_tela_cheia: bool) -> crate::estilo::Icone {
    if em_tela_cheia {
        crate::estilo::Icone::SairDaTelaCheia
    } else {
        crate::estilo::Icone::TelaCheia
    }
}

/// **A tela abre em tela cheia?** Só a lembrada, e nunca na bancada: as provas medem a janela no
/// tamanho que pediram (`--tamanho`), e a escolha de quem usa o aparelho não pode mudar isso.
pub fn abre_em_tela_cheia(ajustes: &AjustesLocais, com_camera: bool, bancada: bool) -> bool {
    !bancada && ajustes.tela_cheia(com_camera)
}

/// **A escolha é guardada?** Fora da bancada: um F11 de prova não pode virar a preferência de quem
/// usa o aparelho.
pub fn guarda_a_tela_cheia(bancada: bool) -> bool {
    !bancada
}

// =============================================================================================
// "Segurar para rolar" (§12 do contrato; o pedido às telas de 14/09)
// =============================================================================================

/// Os dois botões grandes do modo "Segurar para rolar".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotaoDeSegurar {
    Cima,
    Baixo,
}

impl BotaoDeSegurar {
    /// **O mapeamento da §12.5, num lugar só**: "Rolar para cima" **volta** o texto
    /// (`segurar(true)`), "Rolar para baixo" o **avança** — e, com **"Inverter botões"** ligado (o
    /// pedido do usuário de 14/09, à tarde: no vidro a 45° o texto pode andar ao contrário da seta),
    /// o contrário. A inversão troca o sentido **aqui**, e em nenhum outro lugar: as teclas ↑ e ↓ e
    /// as legendas seguem daqui.
    pub fn para_tras(self, invertido: bool) -> bool {
        matches!(self, BotaoDeSegurar::Cima) != invertido
    }

    /// O rótulo e a seta **ficam no lugar** com a inversão: em cima é sempre "Rolar para cima".
    pub fn rotulo(self) -> &'static str {
        match self {
            BotaoDeSegurar::Cima => "Rolar para cima", // i18n: chave (o registro leva o português; a tela traduz)
            BotaoDeSegurar::Baixo => "Rolar para baixo", // i18n: chave
        }
    }

    /// A linha pequena embaixo do rótulo: **troca junto com a ação**, para a pessoa sempre ver o que
    /// o botão faz.
    pub fn legenda(self, invertido: bool) -> &'static str {
        if self.para_tras(invertido) {
            "volta o texto" // i18n: chave
        } else {
            "avança o texto" // i18n: chave
        }
    }

    pub fn nome(self) -> &'static str {
        match self {
            BotaoDeSegurar::Cima => "cima",
            BotaoDeSegurar::Baixo => "baixo",
        }
    }
}

/// O que a tela manda ao núcleo depois de um toque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComandoDeSegurar {
    Nada,
    /// `segurar(botao.para_tras())`.
    Segurar(BotaoDeSegurar),
    /// `soltar()`.
    Soltar,
}

/// Os identificadores dos contatos que não são dedos: o mouse e as duas teclas (no Mac e no
/// Windows, ↑ e ↓ também seguram). Os dedos usam o id do Windows (`WM_POINTER*`), pequeno.
pub const CONTATO_DO_MOUSE: u32 = 1_000_001;
pub const CONTATO_DA_SETA_PARA_CIMA: u32 = 1_000_002;
pub const CONTATO_DA_SETA_PARA_BAIXO: u32 = 1_000_003;

/// **Os contatos sobre os dois botões** — dedos, o mouse e as teclas ↑/↓ —, e o que mandar a cada
/// mudança. A regra é a do Mac (`SegurarParaRolar.swift`, a referência do coordenador), com contatos
/// em vez de "fontes", porque aqui há dedos:
///
/// - encostar num botão manda `segurar` na hora; com mais de um contato de pé, **vale o último
///   apertado**, e o `soltar` só sai quando **nenhum** sobrar;
/// - soltar o que valia com outro ainda seguro volta ao sentido do que sobrou — "segurando
///   pressionado e vai rolando" (*derivado*, como no Mac);
/// - sair do botão é soltar aquele contato (voltar para dentro arrastando não aperta: é o toque
///   cancelado das telas de toque);
/// - o mesmo contato de novo no mesmo botão (a repetição da tecla, um clique a mais) não faz nada;
/// - **o texto parou sozinho** — `segurando` voltou a `false` com um contato de pé (a queda, o
///   silêncio de 2,5 s, a pausa no prompter, o fim do texto): o aviso aparece e **nada aperta de novo
///   sozinho** — nem soltar um de dois contatos. Só um aperto novo, da pessoa, segura de novo. O aviso
///   fica até esse aperto, até sair do modo, ou até o texto voltar a rolar por outro caminho.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dedos {
    /// `(contato, botão)` dos contatos de pé, na ordem em que apertaram: o último vale.
    contatos: Vec<(u32, BotaoDeSegurar)>,
    /// O sentido do último `segurar` mandado, sem `soltar` depois.
    ativo: Option<BotaoDeSegurar>,
    /// O núcleo aceitou o `segurar`, e ele ainda não caiu nem foi solto.
    seguro: bool,
    /// O texto parou sozinho: o botão que valia na hora.
    texto_parou: Option<BotaoDeSegurar>,
}

/// O aviso do texto que parou com o dedo no botão, literal nas quatro telas.
pub const AVISO_DO_TEXTO_PARADO: &str = "O texto parou. Solte e aperte de novo."; // i18n: chave
/// O mesmo, quando quem parou foi o fim do texto com "Rolar para baixo" (o fim desliga `rolando`
/// pela regra de sempre, e "solte e aperte de novo" ali não adianta).
pub const AVISO_DO_FIM_DO_TEXTO: &str = "O texto chegou ao fim."; // i18n: chave

impl Dedos {
    /// O botão que vale agora (o último apertado de pé).
    pub fn ativo(&self) -> Option<BotaoDeSegurar> {
        self.contatos.last().map(|(_, b)| *b)
    }

    /// O texto está rolando por um `segurar` daqui (aceito, e não parou).
    pub fn seguro(&self) -> bool {
        self.seguro
    }

    /// **"Inverter botões" só troca sem contato de pé**: com um dedo num botão de rolar, a troca
    /// fica desligada até soltar (senão o mesmo dedo passaria a rolar para o outro lado).
    pub fn pode_inverter(&self) -> bool {
        self.contatos.is_empty()
    }

    /// O aviso do texto que parou sozinho, com a posição que o prompter relatou.
    pub fn aviso_do_texto_parado(&self, posicao: f64) -> Option<&'static str> {
        match self.texto_parou {
            None => None,
            Some(BotaoDeSegurar::Baixo) if posicao >= 0.999 => Some(AVISO_DO_FIM_DO_TEXTO),
            Some(_) => Some(AVISO_DO_TEXTO_PARADO),
        }
    }

    /// Um contato encostou num botão (fora deles, quem chama nem pergunta).
    pub fn desceu(&mut self, contato: u32, botao: BotaoDeSegurar) -> ComandoDeSegurar {
        if self.contatos.contains(&(contato, botao)) {
            return ComandoDeSegurar::Nada;
        }
        self.contatos.retain(|(c, _)| *c != contato);
        self.contatos.push((contato, botao));
        self.texto_parou = None;
        self.ativo = Some(botao);
        ComandoDeSegurar::Segurar(botao)
    }

    /// Um contato se mexeu: fora do botão em que apertou (`em` diferente), é como se tivesse
    /// soltado. Entrar num botão arrastando não aperta.
    pub fn moveu(&mut self, contato: u32, em: Option<BotaoDeSegurar>) -> ComandoDeSegurar {
        match self.contatos.iter().find(|(c, _)| *c == contato) {
            Some((_, b)) if Some(*b) != em => self.subiu(contato),
            _ => ComandoDeSegurar::Nada,
        }
    }

    /// Um contato saiu (o dedo subiu, o toque foi cancelado, o botão do mouse subiu, a tecla subiu).
    pub fn subiu(&mut self, contato: u32) -> ComandoDeSegurar {
        let Some(i) = self.contatos.iter().position(|(c, _)| *c == contato) else {
            return ComandoDeSegurar::Nada;
        };
        let (_, botao) = self.contatos.remove(i);
        let era_o_que_valia = i == self.contatos.len();
        match self.contatos.last().copied() {
            None => {
                self.seguro = false;
                if self.ativo.take().is_some() {
                    ComandoDeSegurar::Soltar
                } else {
                    ComandoDeSegurar::Nada
                }
            }
            Some((_, sobrou)) if era_o_que_valia && self.seguro && self.texto_parou.is_none() && sobrou != botao => {
                self.ativo = Some(sobrou);
                ComandoDeSegurar::Segurar(sobrou)
            }
            Some(_) => ComandoDeSegurar::Nada,
        }
    }

    /// Todos soltam de uma vez: o segundo plano, um menu, a janela fechando, a desconexão.
    pub fn soltar_todos(&mut self) -> ComandoDeSegurar {
        let havia = !self.contatos.is_empty() || self.ativo.is_some();
        self.contatos.clear();
        self.seguro = false;
        self.ativo = None;
        if havia {
            ComandoDeSegurar::Soltar
        } else {
            ComandoDeSegurar::Nada
        }
    }

    /// Sair do modo: solta tudo e apaga o aviso.
    pub fn sair_do_modo(&mut self) -> ComandoDeSegurar {
        self.texto_parou = None;
        self.soltar_todos()
    }

    /// A resposta do núcleo ao `segurar`: aceito (`OK`) ou recusado (`PROTOCOL`, `CLOSED`…).
    pub fn segurou(&mut self, aceito: bool) {
        self.seguro = aceito && !self.contatos.is_empty();
    }

    /// O `segurando` e o `rolando` do estado, a cada leitura. Devolve `true` quando o texto **acabou
    /// de parar sozinho** com um contato de pé. O aviso se apaga quando o texto volta a rolar sem
    /// `segurar` daqui (alguém deu play no prompter): "o texto parou" deixou de ser verdade.
    pub fn observar(&mut self, segurando: bool, rolando: bool) -> bool {
        if self.texto_parou.is_some() && rolando && !self.seguro {
            self.texto_parou = None;
        }
        if !self.seguro || segurando {
            return false;
        }
        self.seguro = false;
        let Some(ativo) = self.ativo() else { return false };
        self.texto_parou = Some(ativo);
        true
    }
}

/// **Os botões funcionam?** Só com a sessão de pé e o prompter dizendo que entende
/// (`"par_entende_segurar": true`, §12.2) — a mesma tabela do Mac (`DisponibilidadeDoSegurar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisponibilidadeDoSegurar {
    /// Sem sessão (conectando, caiu, tentando de novo): a tela mostra o estado da conexão.
    SemSessao,
    /// A sessão subiu e o prompter ainda não mandou o primeiro estado (*derivado*: sem isto,
    /// "Atualize o app do prompter" piscaria a cada conexão).
    EsperandoOPrompter,
    /// O prompter não responde há mais de 2,5 s: um aperto agora pararia no silêncio antes de
    /// rolar (*derivado*).
    PrompterSumido,
    /// O prompter não diz que entende (13/09, ou uma tela que não liga o segurar), ou o núcleo
    /// recusou com `PROTOCOL`.
    PrompterAntigo,
    Pronto,
}

impl DisponibilidadeDoSegurar {
    pub fn calcular(conectado: bool, par_sumido: bool, par_visto: bool, par_entende: bool, recusou_por_protocolo: bool) -> Self {
        if !conectado {
            return DisponibilidadeDoSegurar::SemSessao;
        }
        if par_sumido {
            return DisponibilidadeDoSegurar::PrompterSumido;
        }
        if par_entende {
            return DisponibilidadeDoSegurar::Pronto;
        }
        if recusou_por_protocolo || par_visto {
            return DisponibilidadeDoSegurar::PrompterAntigo;
        }
        DisponibilidadeDoSegurar::EsperandoOPrompter
    }

    pub fn botoes_ligados(self) -> bool {
        self == DisponibilidadeDoSegurar::Pronto
    }

    /// Por que os botões estão desligados; `estado_da_conexao` é a frase de quem está sem sessão.
    pub fn aviso(self, estado_da_conexao: &str) -> Option<String> {
        use crate::idioma::t;
        match self {
            DisponibilidadeDoSegurar::Pronto => None,
            DisponibilidadeDoSegurar::SemSessao => Some(estado_da_conexao.to_string()),
            DisponibilidadeDoSegurar::EsperandoOPrompter => Some(t("Esperando o prompter responder…").into()),
            DisponibilidadeDoSegurar::PrompterSumido => {
                Some(t("O prompter não responde há mais de 2,5 s. Os botões voltam quando ele responder.").into())
            }
            DisponibilidadeDoSegurar::PrompterAntigo => Some(t("Atualize o app do prompter para usar este modo").into()),
        }
    }
}

// =============================================================================================
// A pergunta do texto e os roteiros guardados (§11 do contrato; o pedido de 14/09, fim da tarde)
// =============================================================================================

/// Os textos da caixa e da lista, **literais nas quatro telas**.
pub const PERGUNTA_NO_PROMPTER: &str = "No prompter"; // i18n: chave
pub const PERGUNTA_NESTE_APARELHO: &str = "Neste aparelho"; // i18n: chave
pub const PERGUNTA_USAR_O_DO_PROMPTER: &str = "Usar o do prompter"; // i18n: chave
pub const PERGUNTA_MANDAR_O_MEU: &str = "Mandar o meu"; // i18n: chave
pub const PERGUNTA_RODAPE: &str = "O roteiro que sair fica em Roteiros guardados."; // i18n: chave
pub const PERGUNTA_CONFERINDO: &str = "Conferindo o roteiro do prompter…"; // i18n: chave
pub const PERGUNTA_PROMPTER_SAIU: &str = "O prompter saiu. A pergunta volta quando ele voltar."; // i18n: chave
pub const PERGUNTA_MUDOU: &str = "O roteiro do prompter mudou. Confira de novo."; // i18n: chave
pub const ROTEIROS_GUARDADOS: &str = "Roteiros guardados"; // i18n: chave
pub const ROTEIROS_VAZIOS: &str = "Nenhum roteiro guardado."; // i18n: chave
pub const CONFIRMA_USAR_ESTE: &str = "Usar este roteiro? Ele substitui o roteiro atual, também no prompter conectado."; // i18n: chave
pub const CONFIRMA_APAGAR: &str = "Apagar este roteiro guardado?"; // i18n: chave

/// "O prompter {prompter_nome} tem outro roteiro."
pub fn titulo_da_pergunta(prompter_nome: &str) -> String {
    crate::idioma::tf("O prompter {} tem outro roteiro.", &[&prompter_nome])
}

/// Quantas palavras há no texto **inteiro** — a mesma palavra da fonte automática
/// ([`palavras_da_linha`]): um trecho sem espaço com pelo menos uma letra ou algarismo.
pub fn contar_palavras(texto: &str) -> usize {
    let u: Vec<u16> = texto.encode_utf16().collect();
    palavras_da_linha(&u).0
}

/// O tamanho de um texto, igual nas quatro telas: concordância e ponto de milhar do pt-BR — "1
/// palavra", "2 palavras", "1.234 palavras", e "0 palavras" para o texto vazio. Na caixa da pergunta
/// e em "Roteiros guardados".
pub fn rotulo_de_palavras(n: usize) -> String {
    use crate::idioma::{atual, t, tf, Idioma};
    if n == 1 {
        return t("1 palavra").to_string();
    }
    // O separador de milhar: o ponto do português, a vírgula do inglês ("1,234 words").
    let separador = if atual() == Idioma::En { ',' } else { '.' };
    let algarismos = n.to_string();
    let mut com_pontos = String::with_capacity(algarismos.len() + algarismos.len() / 3);
    for (i, c) in algarismos.chars().enumerate() {
        if i > 0 && (algarismos.len() - i) % 3 == 0 {
            com_pontos.push(separador);
        }
        com_pontos.push(c);
    }
    tf("{} palavras", &[&com_pontos])
}

/// De onde veio uma cópia: "Do prompter {nome}" (o texto que estava no prompter) ou "Deste
/// aparelho, antes de {nome}" (o texto que estava aqui e saiu).
pub fn origem_da_copia(do_prompter: bool, prompter_nome: &str) -> String {
    if do_prompter {
        crate::idioma::tf("Do prompter {}", &[&prompter_nome])
    } else {
        crate::idioma::tf("Deste aparelho, antes de {}", &[&prompter_nome])
    }
}

/// O que a caixa da pergunta mostra (o pedido, "Os estados da caixa").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstadoDaCaixa {
    /// Sem pergunta, ou comparando há pouco (sem piscar a cada conexão).
    Nenhuma,
    /// Comparando há mais de ~1 s: "Conferindo o roteiro do prompter…", sem botões.
    Conferindo,
    /// A pergunta aberta: os dois blocos, e os botões ligados ou não, com o aviso da vez.
    Aberta { botoes_ligados: bool, aviso: Option<&'static str> },
}

/// Quanto a comparação dura antes de a caixa dizer "Conferindo…".
pub const CONFERINDO_DEPOIS_MS: u64 = 1_000;

/// **O estado da caixa**: a pergunta do estado (`aberta`, `retido_ha_ms`), se o prompter está à vista
/// (`par_visto_ha_ms` não nulo) e a última recusa do `resolver_texto`. `CLOSED` e o prompter sumido
/// desligam os botões ("O prompter saiu…"); `BUSY` pede para conferir de novo, com os botões ligados
/// (a caixa já mostra o texto novo, pelo bit).
pub fn estado_da_caixa(pergunta: Option<(bool, u64)>, par_visto: bool, recusa: Option<Codigo>) -> EstadoDaCaixa {
    match pergunta {
        None => EstadoDaCaixa::Nenhuma,
        Some((false, retido_ha_ms)) if retido_ha_ms > CONFERINDO_DEPOIS_MS => EstadoDaCaixa::Conferindo,
        Some((false, _)) => EstadoDaCaixa::Nenhuma,
        Some((true, _)) if !par_visto || recusa == Some(Codigo::Fechado) => {
            EstadoDaCaixa::Aberta { botoes_ligados: false, aviso: Some(PERGUNTA_PROMPTER_SAIU) }
        }
        Some((true, _)) if recusa == Some(Codigo::Ocupado) => {
            EstadoDaCaixa::Aberta { botoes_ligados: true, aviso: Some(PERGUNTA_MUDOU) }
        }
        Some((true, _)) => EstadoDaCaixa::Aberta { botoes_ligados: true, aviso: None },
    }
}

/// A hora local de uma cópia, curta: "14/09 18:32".
pub fn hora_da_copia(dia: u16, mes: u16, hora: u16, minuto: u16) -> String {
    format!("{dia:02}/{mes:02} {hora:02}:{minuto:02}")
}

/// `12,3 KB de 128 KB` — o tamanho do roteiro contra o teto, em bytes de UTF-8.
pub fn tamanho_do_roteiro(bytes: usize, teto: usize) -> String {
    // A vírgula do português, o ponto do inglês ("12.3 KB of 128 KB").
    let kb = |b: usize| crate::idioma::decimal(b as f64 / 1024.0, 1);
    let teto_texto = kb(teto);
    let teto_texto = teto_texto.strip_suffix(",0").or_else(|| teto_texto.strip_suffix(".0")).unwrap_or(&teto_texto).to_string();
    crate::idioma::tf("{} KB de {} KB", &[&kb(bytes), &teto_texto])
}

/// **A prévia do roteiro** no bloco "Roteiro" do controle (como o cartão do Android, as primeiras
/// 160 letras): as quebras e os espaços repetidos viram um espaço só, e o corte, no meio de uma
/// palavra ou não, ganha "…". Vazio (ou só espaço) é "Sem roteiro.".
pub fn previa_do_roteiro(texto: &str, maximo: usize) -> String {
    let mut previa = String::new();
    let mut letras = 0;
    let mut cortou = false;
    for palavra in texto.split_whitespace() {
        if letras >= maximo {
            cortou = true;
            break;
        }
        if !previa.is_empty() {
            previa.push(' ');
            letras += 1;
        }
        for ch in palavra.chars() {
            if letras >= maximo {
                cortou = true;
                break;
            }
            previa.push(ch);
            letras += 1;
        }
    }
    if previa.is_empty() {
        return crate::idioma::t(SEM_ROTEIRO).into();
    }
    if cortou {
        previa = previa.trim_end().to_string();
        previa.push('…');
    }
    previa
}

/// **A frase do teto**: a de sempre do Confirmar ("O roteiro tem N bytes; o máximo é 131072…"),
/// que o "Abrir arquivo .txt…" também usa.
pub fn frase_do_teto(bytes: usize, teto: usize) -> String {
    crate::idioma::tf("O roteiro tem {} bytes; o máximo é {} (cerca de 20 mil palavras).", &[&bytes, &teto])
}

/// O maior arquivo que o "Abrir arquivo .txt…" chega a ler: o teto em UTF-16 (o dobro) com folga.
/// Acima disso o arquivo nem é lido, e a frase do teto sai com o tamanho do arquivo.
pub fn maior_arquivo_lido(teto: usize) -> usize {
    teto * 4 + 4
}

/// Por que o arquivo de roteiro não entrou no editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecusaDoArquivo {
    /// Passa do teto do núcleo: os bytes do texto em UTF-8 (ou do arquivo, quando nem foi lido).
    Grande(usize),
    /// Não é UTF-8, nem UTF-16 com BOM.
    Codificacao,
    /// Não abriu ou não leu (o motivo do sistema).
    Leitura(String),
}

impl RecusaDoArquivo {
    /// A frase para o editor.
    pub fn frase(&self, teto: usize) -> String {
        match self {
            RecusaDoArquivo::Grande(b) => frase_do_teto(*b, teto),
            RecusaDoArquivo::Codificacao => {
                crate::idioma::t("O arquivo não está em UTF-8 nem em UTF-16 com BOM; salve-o como UTF-8 e abra de novo.").into()
            }
            // O motivo é o do sistema (o detalhe técnico): vai como veio.
            RecusaDoArquivo::Leitura(e) => crate::idioma::tf("Não consegui ler o arquivo: {}", &[e]),
        }
    }
}

/// **O roteiro de um arquivo `.txt`** (o "Abrir arquivo .txt…" do editor): UTF-8 com ou sem BOM,
/// e UTF-16 (LE ou BE) **com** BOM; as quebras `\r\n` e `\r` viram `\n`, e os `\0` saem (como no
/// Confirmar). Acima do teto (em bytes de UTF-8, o que o núcleo conta), a recusa com o tamanho.
pub fn roteiro_do_arquivo(bruto: &[u8], teto: usize) -> Result<String, RecusaDoArquivo> {
    let texto = if let Some(r) = bruto.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8(r.to_vec()).map_err(|_| RecusaDoArquivo::Codificacao)?
    } else if let Some(r) = bruto.strip_prefix(&[0xFF, 0xFE]) {
        utf16(r, u16::from_le_bytes)?
    } else if let Some(r) = bruto.strip_prefix(&[0xFE, 0xFF]) {
        utf16(r, u16::from_be_bytes)?
    } else {
        String::from_utf8(bruto.to_vec()).map_err(|_| RecusaDoArquivo::Codificacao)?
    };
    let limpo = texto.replace("\r\n", "\n").replace('\r', "\n").replace('\0', "");
    if limpo.len() > teto {
        return Err(RecusaDoArquivo::Grande(limpo.len()));
    }
    Ok(limpo)
}

fn utf16(r: &[u8], de: fn([u8; 2]) -> u16) -> Result<String, RecusaDoArquivo> {
    if r.len() % 2 != 0 {
        return Err(RecusaDoArquivo::Codificacao);
    }
    let unidades: Vec<u16> = r.chunks_exact(2).map(|c| de([c[0], c[1]])).collect();
    String::from_utf16(&unidades).map_err(|_| RecusaDoArquivo::Codificacao)
}

/// A prévia de um roteiro vazio.
pub const SEM_ROTEIRO: &str = "Sem roteiro."; // i18n: chave
/// Quantas letras a prévia mostra.
pub const LETRAS_DA_PREVIA: usize = 160;

// =============================================================================================
// As ações de bancada
// =============================================================================================

/// Os botões do editor, para a bancada provar a regra do texto que chega com ele aberto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComandoDoEditor {
    Abrir,
    Confirmar,
    Cancelar,
    UsarNovo,
    ManterMeu,
}

/// Uma edição programada no tempo, feita pelo **mesmo caminho** dos botões da tela.
#[derive(Debug, Clone, PartialEq)]
pub enum Acao {
    Fonte(f64),
    Margem(f64),
    Linha(f64),
    Velocidade(f64),
    Espelho(bool),
    Rolando(bool),
    Salto(f64),
    Pular(f64),
    Texto(String),
    Acrescentar(String),
    Editor(ComandoDoEditor),
    Rascunho(String),
    /// O arquivo, pelo **mesmo caminho** do "Abrir arquivo .txt…" do editor, sem o diálogo: lido,
    /// decodificado e posto no editor aberto como se fosse colado.
    ArquivoNoEditor(String),
    /// Captura **a janela deste processo** para um BMP (e só ela: `WM_PRINT`).
    Captura(String),
    /// Arrasta as setas da linha de leitura até esta fração, **pelo mesmo caminho do mouse**
    /// (mensagens de botão e de movimento postadas à própria janela, meio segundo de arrasto).
    ArrastarLinha(f64),
    /// Arrasta uma seta do enquadramento (`0` = a da esquerda do texto, `1` = a da direita) até esta
    /// fração da largura, pelo mesmo caminho do mouse.
    ArrastarEnquadramento(usize, f64),
    /// Liga ou desliga a fonte automática, como o botão.
    FonteAutomatica(bool),
    /// Liga ou desliga o modo "Segurar para rolar" do controle, como os botões dele.
    ModoSegurar(bool),
    /// O botão do mouse desce no meio de um dos botões grandes (mensagem postada à própria janela,
    /// o mesmo caminho da mão).
    Apertar(BotaoDeSegurar),
    /// O botão do mouse sobe (onde desceu).
    SoltarBotao,
    /// A tecla ↑ (`Cima`) ou ↓ (`Baixo`) desce; com `true`, é a **repetição automática** do
    /// teclado (o bit 30 do `WM_KEYDOWN`), que tem de ser ignorada.
    TeclaDesce(BotaoDeSegurar, bool),
    /// A tecla sobe.
    TeclaSobe(BotaoDeSegurar),
    /// O botão "Inverter botões" do modo segurar, como o clique.
    InverterBotoes(bool),
    /// Responde à pergunta do texto pelo mesmo método do botão: `true` é "Mandar o meu". Se a
    /// caixa ainda não abriu, fica pendente e sai quando ela abrir com os botões ligados.
    Escolha(bool),
    /// Os botões de "Roteiros guardados", pelo mesmo caminho do clique.
    Roteiros(ComandoDosRoteiros),
}

/// Um botão de "Roteiros guardados". Os itens contam de 1, o mais novo primeiro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComandoDosRoteiros {
    Abrir,
    Fechar,
    VoltarALista,
    Ver(usize),
    Usar(usize),
    Apagar(usize),
    /// "Usar"/"Apagar" na confirmação.
    Sim,
    /// "Cancelar" na confirmação.
    Nao,
}

/// **As ações de bancada** de `--teleprompter-acoes`: `segundos:campo=valor`, separadas por
/// vírgula, contadas a partir de a tela abrir — a gramática do Mac (`AcoesDeBancada.swift`), mais
/// `captura`:
///
/// | campo | valor |
/// |---|---|
/// | `fonte`, `margem`, `linha`, `velocidade` | número (ponto decimal) |
/// | `espelho`, `rolando` | `0` ou `1` |
/// | `salto` | fração de 0 a 1 |
/// | `pular` | fração de −1 a 1 |
/// | `texto` | `@caminho` (o conteúdo do arquivo) ou o texto literal |
/// | `texto+` | acrescenta uma linha ao fim do texto |
/// | `editor` | `abrir`, `confirmar`, `cancelar`, `usar-novo`, `manter-meu` |
/// | `rascunho+` | acrescenta uma linha ao rascunho do editor aberto |
/// | `captura` | caminho de um `.bmp` com a janela deste processo |
/// | `arrastar` | fração de 0 a 1: arrasta as setas da linha de leitura até ela, pelo mouse |
/// | `seta_esquerda`, `seta_direita` | fração de 0 a 1: arrasta a seta do enquadramento até ela, pelo mouse |
/// | `fonte_auto` | `0` ou `1`: o botão "Fonte automática" |
/// | `modo_segurar` | `0` ou `1`: o modo "Segurar para rolar" do controle |
/// | `apertar`, `soltar` | `cima` ou `baixo` / `1`: o mouse desce num botão grande / sobe |
/// | `tecla`, `tecla_repete`, `tecla_solta` | `cima` ou `baixo`: ↑/↓ desce, repete sozinha, sobe |
/// | `inverter` | `0` ou `1`: o botão "Inverter botões" do modo segurar |
/// | `escolha` | `prompter` ou `meu`: responde à pergunta do texto, pelo método do botão (pendente até a caixa abrir) |
/// | `roteiros` | `abrir`, `fechar` ou `voltar`: a lista de "Roteiros guardados" |
/// | `roteiro_ver`, `roteiro_usar`, `roteiro_apagar` | o item, de 1 a 3 (o mais novo é o 1) |
/// | `confirmar` | `sim` ou `nao`: a confirmação de usar ou apagar |
///
/// O valor não pode ter vírgula. Uma ação ilegível vai para `recusadas`, e o app a escreve no
/// registro em vez de engoli-la.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AcoesDeBancada {
    pub acoes: Vec<(f64, Acao)>,
    pub recusadas: Vec<String>,
}

impl AcoesDeBancada {
    /// `ler_arquivo` existe para os testes não tocarem no disco.
    pub fn ler(texto: &str, ler_arquivo: impl Fn(&str) -> Option<String>) -> AcoesDeBancada {
        let botao = |v: &str| match v.trim() {
            "cima" => Some(BotaoDeSegurar::Cima),
            "baixo" => Some(BotaoDeSegurar::Baixo),
            _ => None,
        };
        let mut r = AcoesDeBancada::default();
        for item in texto.split(',').filter(|s| !s.trim().is_empty()) {
            let Some((antes, resto)) = item.split_once(':') else {
                r.recusadas.push(item.to_string());
                continue;
            };
            let Some(segundos) = antes.trim().parse::<f64>().ok().filter(|s| *s >= 0.0 && s.is_finite())
            else {
                r.recusadas.push(item.to_string());
                continue;
            };
            let Some((campo, valor)) = resto.split_once('=') else {
                r.recusadas.push(item.to_string());
                continue;
            };
            let campo = campo.trim();
            let numero = valor.trim().parse::<f64>().ok().filter(|n| n.is_finite());
            let acao = match campo {
                "fonte" => numero.map(Acao::Fonte),
                "margem" => numero.map(Acao::Margem),
                "linha" => numero.map(Acao::Linha),
                "velocidade" => numero.map(Acao::Velocidade),
                "salto" => numero.map(Acao::Salto),
                "pular" => numero.map(Acao::Pular),
                "arrastar" => numero.filter(|n| (0.0..=1.0).contains(n)).map(Acao::ArrastarLinha),
                "seta_esquerda" => numero.filter(|n| (0.0..=1.0).contains(n)).map(|n| Acao::ArrastarEnquadramento(0, n)),
                "seta_direita" => numero.filter(|n| (0.0..=1.0).contains(n)).map(|n| Acao::ArrastarEnquadramento(1, n)),
                "fonte_auto" => numero.map(|n| Acao::FonteAutomatica(n != 0.0)),
                "modo_segurar" => numero.map(|n| Acao::ModoSegurar(n != 0.0)),
                "apertar" => botao(valor).map(Acao::Apertar),
                "soltar" => Some(Acao::SoltarBotao),
                "tecla" => botao(valor).map(|b| Acao::TeclaDesce(b, false)),
                "tecla_repete" => botao(valor).map(|b| Acao::TeclaDesce(b, true)),
                "tecla_solta" => botao(valor).map(Acao::TeclaSobe),
                "inverter" => numero.map(|n| Acao::InverterBotoes(n != 0.0)),
                "escolha" => match valor.trim() {
                    "prompter" => Some(Acao::Escolha(false)),
                    "meu" => Some(Acao::Escolha(true)),
                    _ => None,
                },
                "roteiros" => match valor.trim() { // i18n: fora (ação de bancada)
                    "abrir" => Some(Acao::Roteiros(ComandoDosRoteiros::Abrir)),
                    "fechar" => Some(Acao::Roteiros(ComandoDosRoteiros::Fechar)),
                    "voltar" => Some(Acao::Roteiros(ComandoDosRoteiros::VoltarALista)),
                    _ => None,
                },
                "roteiro_ver" | "roteiro_usar" | "roteiro_apagar" => { // i18n: fora
                    let item = valor.trim().parse::<usize>().ok().filter(|n| (1..=3).contains(n));
                    item.map(|n| {
                        Acao::Roteiros(match campo {
                            "roteiro_ver" => ComandoDosRoteiros::Ver(n), // i18n: fora
                            "roteiro_usar" => ComandoDosRoteiros::Usar(n), // i18n: fora
                            _ => ComandoDosRoteiros::Apagar(n),
                        })
                    })
                }
                "confirmar" => match valor.trim() {
                    "sim" => Some(Acao::Roteiros(ComandoDosRoteiros::Sim)),
                    "nao" => Some(Acao::Roteiros(ComandoDosRoteiros::Nao)),
                    _ => None,
                },
                "espelho" => numero.map(|n| Acao::Espelho(n != 0.0)),
                "rolando" => numero.map(|n| Acao::Rolando(n != 0.0)),
                "texto" => match valor.strip_prefix('@') {
                    Some(caminho) => ler_arquivo(caminho).map(Acao::Texto),
                    None => Some(Acao::Texto(valor.to_string())),
                },
                "texto+" => Some(Acao::Acrescentar(valor.to_string())),
                "rascunho+" => Some(Acao::Rascunho(valor.to_string())),
                "arquivo-no-editor" if !valor.trim().is_empty() => Some(Acao::ArquivoNoEditor(valor.trim().to_string())),
                "captura" if !valor.trim().is_empty() => Some(Acao::Captura(valor.trim().to_string())),
                "editor" => match valor.trim() {
                    "abrir" => Some(Acao::Editor(ComandoDoEditor::Abrir)),
                    "confirmar" => Some(Acao::Editor(ComandoDoEditor::Confirmar)),
                    "cancelar" => Some(Acao::Editor(ComandoDoEditor::Cancelar)),
                    "usar-novo" => Some(Acao::Editor(ComandoDoEditor::UsarNovo)),
                    "manter-meu" => Some(Acao::Editor(ComandoDoEditor::ManterMeu)),
                    _ => None,
                },
                _ => None,
            };
            match acao {
                Some(a) => r.acoes.push((segundos, a)),
                None => r.recusadas.push(item.to_string()),
            }
        }
        // Estável: duas ações no mesmo segundo saem na ordem em que foram escritas.
        r.acoes.sort_by(|a, b| a.0.total_cmp(&b.0));
        r
    }
}

// =============================================================================================
// Testes
// =============================================================================================

#[cfg(test)]
mod testes {
    use super::*;
    use quall_core::teleprompter::ContadoresDoTeleprompter;

    #[test]
    fn o_roteiro_do_arquivo_le_utf8_e_utf16_e_respeita_o_teto() {
        let teto = 131_072;
        assert_eq!(roteiro_do_arquivo("Olá\r\nmundo\rfim".as_bytes(), teto), Ok("Olá\nmundo\nfim".into()), "UTF-8 sem BOM, quebras");
        let mut com_bom = vec![0xEF, 0xBB, 0xBF];
        com_bom.extend_from_slice("ação\n".as_bytes());
        assert_eq!(roteiro_do_arquivo(&com_bom, teto), Ok("ação\n".into()), "o BOM do UTF-8 sai");
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain("Câmera\r\n1".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(roteiro_do_arquivo(&le, teto), Ok("Câmera\n1".into()), "UTF-16 LE com BOM");
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain("é".encode_utf16().flat_map(|u| u.to_be_bytes())).collect();
        assert_eq!(roteiro_do_arquivo(&be, teto), Ok("é".into()), "UTF-16 BE com BOM");
        assert_eq!(roteiro_do_arquivo(&[0x61, 0x00, 0x62], teto), Ok("ab".into()), "o \\0 sai, como no Confirmar");
        assert_eq!(roteiro_do_arquivo(&[0xC3, 0x28], teto), Err(RecusaDoArquivo::Codificacao), "Latin-1/UTF-8 quebrado");
        assert_eq!(roteiro_do_arquivo(&[0xFF, 0xFE, 0x41], teto), Err(RecusaDoArquivo::Codificacao), "UTF-16 de tamanho ímpar");
        assert_eq!(roteiro_do_arquivo(b"", teto), Ok(String::new()));
        // O teto conta bytes de UTF-8 depois de normalizar: `\r\n` vira 1 byte.
        let no_teto = "a".repeat(teto);
        assert_eq!(roteiro_do_arquivo(no_teto.as_bytes(), teto).map(|s| s.len()), Ok(teto));
        assert_eq!(roteiro_do_arquivo(format!("{no_teto}é").as_bytes(), teto), Err(RecusaDoArquivo::Grande(teto + 2)));
        let quebras = "a\r\n".repeat(teto / 2);
        assert!(roteiro_do_arquivo(quebras.as_bytes(), teto).is_ok(), "{} bytes no disco, {} no texto", quebras.len(), teto);
        assert_eq!(
            RecusaDoArquivo::Grande(200_000).frase(teto),
            "O roteiro tem 200000 bytes; o máximo é 131072 (cerca de 20 mil palavras).",
            "a mesma frase do Confirmar"
        );
        assert!(maior_arquivo_lido(teto) > teto * 4, "um roteiro no teto em UTF-16 com BOM cabe na leitura");
    }

    #[test]
    fn a_tela_cheia_e_lembrada_por_tela_e_sobrevive_ao_arquivo() {
        let mut a = AjustesLocais::default();
        assert!(!a.tela_cheia(false) && !a.tela_cheia(true), "ausente é em janela");
        assert!(a.lembrar_tela_cheia(true, true), "a R5 entrou: mudou");
        assert!(!a.lembrar_tela_cheia(true, true), "de novo: não mudou, não regrava");
        assert!(a.tela_cheia(true));
        assert!(!a.tela_cheia(false), "a R5 não leva o prompter junto");
        let mut lido = AjustesLocais::de_json(&a.para_json());
        assert!(lido.r5_tela_cheia && !lido.prompter_tela_cheia);
        assert!(lido.lembrar_tela_cheia(true, false), "saiu da tela cheia: mudou");
        assert!(lido.lembrar_tela_cheia(false, true));
        assert!(lido.tela_cheia(false) && !lido.tela_cheia(true));
        // Em janela, o arquivo não ganha os campos (o de antes desta rodada continua igual).
        assert!(!AjustesLocais::default().para_json().contains("tela_cheia"));
        // Um arquivo velho, sem os campos, lê em janela.
        let velho = r#"{"enquadramento":{"paisagem":[0.0,1.0],"retrato":[0.0,1.0]},"fonte_automatica":true}"#;
        let v = AjustesLocais::de_json(velho);
        assert!(!v.tela_cheia(false) && !v.tela_cheia(true) && v.fonte_automatica);
    }

    #[test]
    fn a_bancada_nao_abre_nem_guarda_a_tela_cheia() {
        let mut a = AjustesLocais::default();
        a.lembrar_tela_cheia(false, true);
        assert!(abre_em_tela_cheia(&a, false, false));
        assert!(!abre_em_tela_cheia(&a, false, true), "a bancada mede a janela do tamanho pedido");
        assert!(!abre_em_tela_cheia(&a, true, false), "a R5 tem a dela");
        assert!(guarda_a_tela_cheia(false));
        assert!(!guarda_a_tela_cheia(true));
    }

    #[test]
    fn a_previa_do_roteiro_junta_as_linhas_e_corta_com_reticencias() {
        assert_eq!(previa_do_roteiro("", 160), "Sem roteiro.");
        assert_eq!(previa_do_roteiro(" \r\n\t ", 160), "Sem roteiro.");
        assert_eq!(previa_do_roteiro("Boa noite.\r\n\r\nHoje   falamos", 160), "Boa noite. Hoje falamos");
        assert_eq!(previa_do_roteiro("abc def", 7), "abc def", "cabe exato: sem reticências");
        assert_eq!(previa_do_roteiro("abc def ghi", 7), "abc def…");
        assert_eq!(previa_do_roteiro("abcdefghij", 4), "abcd…");
        // Conta letras, não bytes: acento e emoji não partem.
        assert_eq!(previa_do_roteiro("ação é 👍 sim", 8), "ação é 👍…");
        let longo = "palavra ".repeat(100);
        let p = previa_do_roteiro(&longo, LETRAS_DA_PREVIA);
        assert!(p.ends_with('…') && p.chars().count() <= LETRAS_DA_PREVIA + 1);
    }

    #[test]
    fn o_botao_da_tela_cheia_diz_o_que_faz() {
        assert_eq!(rotulo_da_tela_cheia(false), "Tela cheia");
        assert_eq!(rotulo_da_tela_cheia(true), "Sair da tela cheia");
        assert_eq!(icone_da_tela_cheia(false).glifo(), '\u{E740}');
        assert_eq!(icone_da_tela_cheia(true).glifo(), '\u{E73F}');
    }

    #[test]
    fn a_porta_do_teleprompter_e_7979_e_nao_a_do_video() {
        assert_eq!(porta_do_teleprompter(), 7979);
        assert_ne!(porta_do_teleprompter(), quall_core::discovery::DEFAULT_SIGNALING_PORT);
    }

    #[test]
    fn as_portas_candidatas_sao_dez_e_nao_passam_de_65535() {
        assert_eq!(portas_candidatas(7979), (7979..7989).collect::<Vec<_>>());
        assert_eq!(portas_candidatas(65_530), (65_530..=65_535).collect::<Vec<_>>());
    }

    #[test]
    fn ip_puro_completa_com_7979() {
        let d = destino_do_controle("192.168.15.8").unwrap();
        assert_eq!(d.endereco, "192.168.15.8:7979");
        assert_eq!(d.pin, None);
        assert_eq!(destino_do_controle("  192.168.15.8:8000 ").unwrap().endereco, "192.168.15.8:8000");
        assert_eq!(destino_do_controle("quall-944d0e.local").unwrap().endereco, "quall-944d0e.local:7979");
    }

    #[test]
    fn ipv6_ganha_colchetes_e_porta() {
        assert_eq!(destino_do_controle("2804:1b1::1").unwrap().endereco, "[2804:1b1::1]:7979");
        assert_eq!(destino_do_controle("[2804:1b1::1]").unwrap().endereco, "[2804:1b1::1]:7979");
        assert_eq!(destino_do_controle("[2804:1b1::1]:8000").unwrap().endereco, "[2804:1b1::1]:8000");
        assert_eq!(destino_do_controle("[2804:1b1::1]x"), None);
    }

    /// O `quall://` colado (de uma tela antiga) ainda é entendido, embora nenhuma tela o mostre mais.
    #[test]
    fn o_link_antigo_colado_traz_o_pin_e_a_porta() {
        let d = destino_do_controle("quall://424242@192.168.15.8:7979").unwrap();
        assert_eq!(d.endereco, "192.168.15.8:7979");
        assert_eq!(d.pin.as_deref(), Some("424242"));
        // Maiúsculas no esquema e barra no fim: o link colado de outro lugar.
        let d = destino_do_controle("QUALL://424242@192.168.15.8/").unwrap();
        assert_eq!(d.endereco, "192.168.15.8:7979");
        assert_eq!(d.pin.as_deref(), Some("424242"));
        // PIN que não é de seis dígitos não vale — o endereço vale.
        let d = destino_do_controle("quall://12ab@192.168.15.8:7979").unwrap();
        assert_eq!(d.pin, None);
        assert_eq!(destino_do_controle("quall://424242@"), None);
    }

    #[test]
    fn enderecos_que_nao_se_entendem() {
        assert_eq!(destino_do_controle(""), None);
        assert_eq!(destino_do_controle("   "), None);
        assert_eq!(destino_do_controle("192.168.15.8:0"), None);
        assert_eq!(destino_do_controle("192.168.15.8:70000"), None);
        assert_eq!(destino_do_controle(":7979"), None);
    }

    fn falha(codigo: Codigo) -> Falha {
        Falha {
            codigo,
            ja_subiu: false,
            durou_ms: 5_000,
            falhando_ha_ms: 0,
            falhas_seguidas: 1,
            erros_de_pin_seguidos: 0,
            parando: false,
        }
    }

    #[test]
    fn prompter_troca_o_pin_depois_de_erro_de_pin_com_espera_crescente() {
        for (n, espera) in [(1, 1000), (2, 2000), (3, 4000), (4, 8000)] {
            let d = decidir(
                Lado::Prompter,
                Falha { erros_de_pin_seguidos: n, ..falha(Codigo::PinErrado) },
            );
            assert_eq!(d, Decisao::TentarDeNovo { pin: EscolhaDoPin::Novo, depois_ms: espera });
        }
        let d = decidir(Lado::Prompter, Falha { erros_de_pin_seguidos: 5, ..falha(Codigo::Pareamento) });
        assert_eq!(d, Decisao::Parar, "no quinto erro seguido a espera para");
    }

    #[test]
    fn prompter_mantem_o_pin_em_prazo_versao_e_porta() {
        for c in [Codigo::Prazo, Codigo::Protocolo, Codigo::Io, Codigo::PrecisaDePin, Codigo::Fechado] {
            let d = decidir(Lado::Prompter, falha(c));
            assert_eq!(d, Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: 0 }, "{c:?}");
        }
        // Uma falha rápida espera o resto do segundo: sem laço quente.
        let d = decidir(Lado::Prompter, Falha { durou_ms: 200, ..falha(Codigo::Io) });
        assert_eq!(d, Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: 800 });
        assert_eq!(decidir(Lado::Prompter, falha(Codigo::Invalido)), Decisao::Parar);
        let s = decidir(Lado::Prompter, Falha { falhas_seguidas: 3, ..falha(Codigo::Sinalizacao) });
        assert_eq!(s, Decisao::Parar);
        let s = decidir(Lado::Prompter, Falha { falhas_seguidas: 1, ..falha(Codigo::Sinalizacao) });
        assert_eq!(s, Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: 1000 });
    }

    #[test]
    fn controle_insiste_no_ocupado_e_para_no_que_precisa_da_pessoa() {
        let ocupado = Falha { durou_ms: 50, falhando_ha_ms: 3_000, ..falha(Codigo::Ocupado) };
        assert_eq!(
            decidir(Lado::Controle, ocupado),
            Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: 950 }
        );
        // Na primeira conexão, ocupado por mais de 12 s: é outro controle, não a volta.
        let longo = Falha { falhando_ha_ms: 13_000, ..ocupado };
        assert_eq!(decidir(Lado::Controle, longo), Decisao::Parar);
        // Depois de uma queda, ocupado é sempre "tente de novo".
        let volta = Falha { ja_subiu: true, falhando_ha_ms: 60_000, ..ocupado };
        assert!(matches!(decidir(Lado::Controle, volta), Decisao::TentarDeNovo { .. }));
        for c in [Codigo::PinErrado, Codigo::PrecisaDePin, Codigo::Pareamento, Codigo::Protocolo] {
            let depois_de_queda = Falha { ja_subiu: true, ..falha(c) };
            assert_eq!(decidir(Lado::Controle, depois_de_queda), Decisao::Parar, "{c:?}");
        }
    }

    #[test]
    fn controle_so_insiste_na_rede_depois_de_uma_queda() {
        for c in [Codigo::Io, Codigo::Prazo, Codigo::SemRota, Codigo::Transporte] {
            assert_eq!(decidir(Lado::Controle, falha(c)), Decisao::Parar, "{c:?} na primeira vez");
            let volta = Falha { ja_subiu: true, durou_ms: 10, ..falha(c) };
            assert_eq!(
                decidir(Lado::Controle, volta),
                Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: 990 },
                "{c:?} depois de uma queda"
            );
        }
    }

    #[test]
    fn parar_e_cancelar_param_nos_dois_lados() {
        for lado in [Lado::Prompter, Lado::Controle] {
            assert_eq!(decidir(lado, falha(Codigo::Cancelado)), Decisao::Parar);
            assert_eq!(decidir(lado, Falha { parando: true, ..falha(Codigo::Ocupado) }), Decisao::Parar);
        }
    }

    #[test]
    fn o_codigo_vem_da_variante_e_nao_do_texto() {
        assert_eq!(Codigo::de(&Error::WrongPin("qualquer".into())), Codigo::PinErrado);
        assert_eq!(Codigo::de(&Error::Ocupado("x".into())), Codigo::Ocupado);
        assert_eq!(Codigo::de(&Error::Cancelled), Codigo::Cancelado);
        assert_eq!(Codigo::PinErrado.nome(), "WRONG_PIN");
        assert_eq!(Codigo::Ocupado.nome(), "BUSY");
    }

    fn estado() -> Estado {
        Estado {
            rolando: false,
            velocidade: 1.0,
            fonte: 48.0,
            margem: 0.1,
            linha_de_leitura: 0.3,
            espelho: false,
            posicao: 0.0,
            salto: None,
            texto_bytes: 0,
            par_visto_ha_ms: None,
            sem_confirmacao_ha_ms: None,
            contadores: ContadoresDoTeleprompter::default(),
            pergunta_do_texto: None,
            copias_do_texto: Vec::new(),
            para_tras: false,
            segurando: false,
            par_entende_segurar: false,
            gravando_ha_ms: None,
            pedido_de_gravacao: None,
            gravacao_recusada: None,
            par_entende_gravar: false,
        }
    }

    #[test]
    fn a_caixa_da_pergunta_e_os_seus_estados() {
        assert_eq!(estado_da_caixa(None, true, None), EstadoDaCaixa::Nenhuma);
        // Comparando: nada no primeiro segundo (sem piscar a cada conexão); depois, "Conferindo…".
        assert_eq!(estado_da_caixa(Some((false, 300)), true, None), EstadoDaCaixa::Nenhuma);
        assert_eq!(estado_da_caixa(Some((false, 1_200)), true, None), EstadoDaCaixa::Conferindo);
        assert_eq!(
            estado_da_caixa(Some((true, 50)), true, None),
            EstadoDaCaixa::Aberta { botoes_ligados: true, aviso: None }
        );
        // O prompter sumiu, ou o `resolver_texto` deu CLOSED: botões desligados, com o texto.
        let saiu = EstadoDaCaixa::Aberta { botoes_ligados: false, aviso: Some(PERGUNTA_PROMPTER_SAIU) };
        assert_eq!(estado_da_caixa(Some((true, 50)), false, None), saiu);
        assert_eq!(estado_da_caixa(Some((true, 50)), true, Some(Codigo::Fechado)), saiu);
        // BUSY: confira de novo, com os botões ligados.
        assert_eq!(
            estado_da_caixa(Some((true, 50)), true, Some(Codigo::Ocupado)),
            EstadoDaCaixa::Aberta { botoes_ligados: true, aviso: Some(PERGUNTA_MUDOU) }
        );
    }

    #[test]
    fn os_textos_da_pergunta_e_dos_roteiros_guardados() {
        assert_eq!(titulo_da_pergunta("iPad da Maria"), "O prompter iPad da Maria tem outro roteiro.");
        assert_eq!(contar_palavras("Bom dia a todos.\n— Boa noite, 2026!"), 7, "o travessão sozinho não é palavra");
        assert_eq!(contar_palavras(""), 0);
        assert_eq!(rotulo_de_palavras(0), "0 palavras", "o texto vazio");
        assert_eq!(rotulo_de_palavras(1), "1 palavra");
        assert_eq!(rotulo_de_palavras(2), "2 palavras");
        assert_eq!(rotulo_de_palavras(999), "999 palavras");
        assert_eq!(rotulo_de_palavras(1_000), "1.000 palavras");
        assert_eq!(rotulo_de_palavras(1_234), "1.234 palavras");
        assert_eq!(rotulo_de_palavras(14_044), "14.044 palavras");
        assert_eq!(rotulo_de_palavras(123_456), "123.456 palavras");
        assert_eq!(rotulo_de_palavras(1_234_567), "1.234.567 palavras");
        assert_eq!(origem_da_copia(true, "A10s"), "Do prompter A10s");
        assert_eq!(origem_da_copia(false, "A10s"), "Deste aparelho, antes de A10s");
        assert_eq!(hora_da_copia(4, 9, 8, 5), "04/09 08:05");
    }

    #[test]
    fn o_mapeamento_do_segurar_mora_num_lugar_so() {
        assert!(BotaoDeSegurar::Cima.para_tras(false), "\"Rolar para cima\" volta o texto (§12.5)");
        assert!(!BotaoDeSegurar::Baixo.para_tras(false));
        assert_eq!(BotaoDeSegurar::Cima.legenda(false), "volta o texto");
        assert_eq!(BotaoDeSegurar::Baixo.legenda(false), "avança o texto");
    }

    #[test]
    fn inverter_botoes_troca_o_sentido_e_a_legenda_e_nao_o_rotulo() {
        // Com a inversão, o de cima manda `para_tras=false` (avança) e o de baixo `true` (volta).
        assert!(!BotaoDeSegurar::Cima.para_tras(true));
        assert!(BotaoDeSegurar::Baixo.para_tras(true));
        // As legendas trocam junto com a ação; o rótulo e a seta ficam no lugar.
        assert_eq!(BotaoDeSegurar::Cima.legenda(true), "avança o texto");
        assert_eq!(BotaoDeSegurar::Baixo.legenda(true), "volta o texto");
        assert_eq!(BotaoDeSegurar::Cima.rotulo(), "Rolar para cima");
        assert_eq!(BotaoDeSegurar::Baixo.rotulo(), "Rolar para baixo");
    }

    #[test]
    fn com_um_dedo_num_botao_a_troca_nao_vale() {
        let mut d = Dedos::default();
        assert!(d.pode_inverter());
        apertar(&mut d, CONTATO_DA_SETA_PARA_BAIXO, BotaoDeSegurar::Baixo);
        assert!(!d.pode_inverter(), "a tecla ↓ segura: a troca fica desligada");
        d.subiu(CONTATO_DA_SETA_PARA_BAIXO);
        assert!(d.pode_inverter(), "soltou: a troca volta");
        // O texto que parou sozinho com o dedo no botão também segura a troca até soltar.
        apertar(&mut d, 1, BotaoDeSegurar::Cima);
        assert!(d.observar(false, false));
        assert!(!d.pode_inverter());
        d.subiu(1);
        assert!(d.pode_inverter());
    }

    /// Aperta e diz ao modelo que o núcleo aceitou (o caminho feliz da tela).
    fn apertar(d: &mut Dedos, contato: u32, b: BotaoDeSegurar) -> ComandoDeSegurar {
        let c = d.desceu(contato, b);
        if matches!(c, ComandoDeSegurar::Segurar(_)) {
            d.segurou(true);
        }
        c
    }

    #[test]
    fn um_dedo_segura_ao_encostar_e_solta_ao_tirar() {
        use BotaoDeSegurar::*;
        use ComandoDeSegurar::*;
        let mut d = Dedos::default();
        assert_eq!(apertar(&mut d, 7, Baixo), Segurar(Baixo));
        assert!(d.seguro());
        assert_eq!(d.desceu(7, Baixo), Nada, "o mesmo contato de novo (a repetição) não faz nada");
        assert_eq!(d.moveu(7, Some(Baixo)), Nada, "mexer dentro do botão não manda nada");
        assert_eq!(d.subiu(7), Soltar);
        assert!(!d.seguro());
        assert_eq!(d.subiu(7), Nada, "soltar duas vezes não manda duas");
        assert_eq!(d.ativo(), None);
    }

    #[test]
    fn sair_do_botao_solta_e_entrar_no_outro_arrastando_nao_aperta() {
        use BotaoDeSegurar::*;
        use ComandoDeSegurar::*;
        let mut d = Dedos::default();
        assert_eq!(apertar(&mut d, 1, Cima), Segurar(Cima));
        assert_eq!(d.moveu(1, None), Soltar, "saiu do botão");
        assert_eq!(d.moveu(1, Some(Baixo)), Nada, "entrou no outro arrastando: não aperta");
        assert_eq!(d.moveu(1, Some(Cima)), Nada, "voltar ao primeiro arrastando também não");
        assert_eq!(d.subiu(1), Nada);
    }

    #[test]
    fn dois_dedos_vale_o_ultimo_e_solta_so_quando_nenhum_sobra() {
        use BotaoDeSegurar::*;
        use ComandoDeSegurar::*;
        let mut d = Dedos::default();
        assert_eq!(apertar(&mut d, 1, Baixo), Segurar(Baixo));
        assert_eq!(apertar(&mut d, 2, Cima), Segurar(Cima), "vale o último botão apertado");
        assert_eq!(d.subiu(2), Segurar(Baixo), "sobrou o dedo no de baixo: é ele que vale");
        assert_eq!(d.subiu(1), Soltar, "nenhum dedo sobrou");
        // Soltar o que **não** valia não muda nada.
        assert_eq!(apertar(&mut d, 3, Baixo), Segurar(Baixo));
        assert_eq!(apertar(&mut d, 4, Cima), Segurar(Cima));
        assert_eq!(d.subiu(3), Nada, "o de baixo não valia");
        assert_eq!(d.subiu(4), Soltar);
        // O mouse e as teclas são contatos como os dedos.
        assert_eq!(apertar(&mut d, CONTATO_DA_SETA_PARA_CIMA, Cima), Segurar(Cima));
        assert_eq!(apertar(&mut d, CONTATO_DO_MOUSE, Baixo), Segurar(Baixo));
        assert_eq!(d.soltar_todos(), Soltar, "segundo plano / um menu / a janela fechando");
        assert_eq!(d.soltar_todos(), Nada, "sem nada seguro, não manda nada");
    }

    #[test]
    fn o_texto_que_parou_com_o_dedo_no_botao_nao_aperta_de_novo_sozinho() {
        use BotaoDeSegurar::*;
        use ComandoDeSegurar::*;
        let mut d = Dedos::default();
        assert_eq!(apertar(&mut d, 1, Baixo), Segurar(Baixo));
        assert_eq!(apertar(&mut d, 2, Cima), Segurar(Cima));
        assert!(!d.observar(true, true), "segurando: nada parou");
        assert!(d.observar(false, false), "o segurando voltou a false com os dois de pé");
        assert_eq!(d.aviso_do_texto_parado(0.4), Some(AVISO_DO_TEXTO_PARADO));
        assert_eq!(d.subiu(2), Nada, "sobrou um dedo, mas não aperta de novo sozinho");
        assert_eq!(d.aviso_do_texto_parado(0.4), Some(AVISO_DO_TEXTO_PARADO), "o aviso fica");
        assert_eq!(apertar(&mut d, 3, Cima), Segurar(Cima), "um aperto novo, da pessoa, segura de novo");
        assert_eq!(d.aviso_do_texto_parado(0.4), None);
        // Soltos todos com o texto parado, o aviso fica até o próximo aperto…
        assert!(d.observar(false, false));
        assert_eq!(d.subiu(1), Nada);
        assert_eq!(d.subiu(3), Soltar, "o último contato sai: o soltar vai (no núcleo, sem efeito)");
        assert_eq!(d.aviso_do_texto_parado(0.4), Some(AVISO_DO_TEXTO_PARADO));
        // … ou até o texto voltar a rolar por outro caminho (play no prompter).
        assert!(!d.observar(false, true));
        assert_eq!(d.aviso_do_texto_parado(0.4), None);
    }

    #[test]
    fn o_fim_do_texto_segurando_para_baixo_tem_o_seu_aviso() {
        use BotaoDeSegurar::*;
        let mut d = Dedos::default();
        apertar(&mut d, 1, Baixo);
        assert!(d.observar(false, false));
        assert_eq!(d.aviso_do_texto_parado(1.0), Some(AVISO_DO_FIM_DO_TEXTO));
        assert_eq!(d.aviso_do_texto_parado(0.5), Some(AVISO_DO_TEXTO_PARADO));
        // Para cima, o começo não para o texto (fica rolando em 0); se parar, é o aviso de sempre.
        let mut d = Dedos::default();
        apertar(&mut d, 1, Cima);
        assert!(d.observar(false, false));
        assert_eq!(d.aviso_do_texto_parado(1.0), Some(AVISO_DO_TEXTO_PARADO));
        assert_eq!(d.sair_do_modo(), ComandoDeSegurar::Soltar);
        assert_eq!(d.aviso_do_texto_parado(1.0), None, "sair do modo apaga o aviso");
    }

    #[test]
    fn o_segurar_recusado_nao_vira_texto_parado() {
        use BotaoDeSegurar::*;
        let mut d = Dedos::default();
        assert_eq!(d.desceu(1, Baixo), ComandoDeSegurar::Segurar(Baixo));
        d.segurou(false); // PROTOCOL
        assert!(!d.observar(false, false), "nunca rolou: não parou");
        assert_eq!(d.aviso_do_texto_parado(0.2), None);
        assert_eq!(d.subiu(1), ComandoDeSegurar::Soltar);
    }

    #[test]
    fn o_modo_so_liga_os_botoes_com_o_prompter_que_entende() {
        use DisponibilidadeDoSegurar::*;
        assert_eq!(DisponibilidadeDoSegurar::calcular(false, false, true, true, false), SemSessao);
        assert_eq!(DisponibilidadeDoSegurar::calcular(true, false, false, false, false), EsperandoOPrompter);
        assert_eq!(DisponibilidadeDoSegurar::calcular(true, false, true, false, false), PrompterAntigo);
        assert_eq!(DisponibilidadeDoSegurar::calcular(true, false, false, false, true), PrompterAntigo, "PROTOCOL");
        assert_eq!(DisponibilidadeDoSegurar::calcular(true, true, true, true, false), PrompterSumido);
        assert_eq!(DisponibilidadeDoSegurar::calcular(true, false, true, true, true), Pronto, "a marca vale mais que a recusa velha");
        assert!(Pronto.botoes_ligados() && !PrompterAntigo.botoes_ligados() && !SemSessao.botoes_ligados());
        assert_eq!(PrompterAntigo.aviso("").as_deref(), Some("Atualize o app do prompter para usar este modo"));
        assert_eq!(SemSessao.aviso("Conexão perdida").as_deref(), Some("Conexão perdida"));
        assert_eq!(Pronto.aviso(""), None);
    }

    #[test]
    fn o_aviso_de_par_sumido_nao_pisca_na_primeira_sessao() {
        let e = estado();
        // Sessão nova, sem mensagem ainda, há 1 s: não é sumido.
        let a = Avisos::calcular(&e, Ligacao::Conectada { desde_s: 10.0, depois_de_queda: false }, 11.0);
        assert!(!a.par_sumido);
        // Há 3 s sem mensagem nenhuma: é.
        let a = Avisos::calcular(&e, Ligacao::Conectada { desde_s: 10.0, depois_de_queda: false }, 13.0);
        assert!(a.par_sumido);
        // Na volta depois de uma queda, o aviso fica até a primeira mensagem.
        let a = Avisos::calcular(&e, Ligacao::Conectada { desde_s: 10.0, depois_de_queda: true }, 10.1);
        assert!(a.par_sumido);
        let visto = Estado { par_visto_ha_ms: Some(100), ..estado() };
        let a = Avisos::calcular(&visto, Ligacao::Conectada { desde_s: 10.0, depois_de_queda: true }, 10.2);
        assert!(!a.par_sumido);
        let velho = Estado { par_visto_ha_ms: Some(2_600), ..estado() };
        let a = Avisos::calcular(&velho, Ligacao::Conectada { desde_s: 0.0, depois_de_queda: false }, 30.0);
        assert!(a.par_sumido);
        assert!(Avisos::calcular(&e, Ligacao::Caiu, 0.0).par_sumido);
        assert!(!Avisos::calcular(&e, Ligacao::SemSessao, 99.0).par_sumido);
    }

    #[test]
    fn o_comando_que_nao_chegou_e_o_app_de_outra_versao() {
        let pendente = Estado { sem_confirmacao_ha_ms: Some(1_600), ..estado() };
        let a = Avisos::calcular(&pendente, Ligacao::Conectada { desde_s: 0.0, depois_de_queda: false }, 1.0);
        assert!(a.sem_confirmacao);
        let recente = Estado { sem_confirmacao_ha_ms: Some(1_400), ..estado() };
        let a = Avisos::calcular(&recente, Ligacao::Conectada { desde_s: 0.0, depois_de_queda: false }, 1.0);
        assert!(!a.sem_confirmacao);
        let mut outra = estado();
        outra.contadores.de_outra_versao = 1;
        outra.contadores.carimbos_do_futuro = 2;
        outra.contadores.reenvios_desistidos = 1;
        let a = Avisos::calcular(&outra, Ligacao::SemSessao, 0.0);
        assert!(a.atualize_o_app && a.relogio_errado && a.texto_nao_passou);
    }

    #[test]
    fn o_rascunho_sem_mudanca_acompanha_o_outro_lado() {
        let mut r = Rascunho::novo("um");
        r.chegou("dois");
        assert_eq!(r.rascunho, "dois");
        assert!(!r.em_conflito());
        assert!(r.atualizado_pelo_outro_lado());
        assert_eq!(r.para_confirmar("dois"), None);
    }

    #[test]
    fn o_rascunho_mudado_pergunta_e_nunca_e_tocado() {
        let mut r = Rascunho::novo("um");
        r.rascunho.push_str(" e meu");
        r.chegou("dois");
        assert!(r.em_conflito());
        assert_eq!(r.rascunho, "um e meu", "o que a pessoa digitou fica");
        // Manter o meu: confirmar manda o daqui.
        r.manter_o_meu();
        assert!(!r.em_conflito());
        assert_eq!(r.para_confirmar("dois").as_deref(), Some("um e meu"));
        // Um terceiro texto volta a perguntar.
        r.chegou("três");
        assert!(r.em_conflito());
        // Usar o novo: o descartado sai para a área de transferência.
        assert_eq!(r.usar_o_texto_novo().as_deref(), Some("um e meu"));
        assert_eq!(r.rascunho, "três");
        assert_eq!(r.para_confirmar("três"), None);
    }

    #[test]
    fn o_rascunho_tira_o_nul_ao_confirmar() {
        let mut r = Rascunho::novo("");
        r.rascunho = "a\0b".into();
        assert_eq!(r.para_confirmar("").as_deref(), Some("ab"));
    }

    #[test]
    fn os_passos_param_na_borda_e_nao_acumulam_erro() {
        assert_eq!(passo(0.1, PASSO_DA_VELOCIDADE, 1, FAIXA_DA_VELOCIDADE), 0.2);
        assert_eq!(passo(0.2, PASSO_DA_VELOCIDADE, 1, FAIXA_DA_VELOCIDADE), 0.3);
        assert_eq!(passo(0.7, PASSO_DA_VELOCIDADE, 1, FAIXA_DA_VELOCIDADE), 0.8);
        assert_eq!(passo(0.05, PASSO_DA_VELOCIDADE, -1, FAIXA_DA_VELOCIDADE), 0.05);
        assert_eq!(passo(398.0, PASSO_DA_FONTE, 1, FAIXA_DA_FONTE), 400.0);
        assert_eq!(passo(0.44, PASSO_DA_MARGEM, 1, FAIXA_DA_MARGEM), 0.45);
        assert_eq!(passo(0.0, PASSO_DA_LINHA, -1, FAIXA_DA_LINHA), 0.0);
        assert_eq!(passo(0.3, PASSO_DA_LINHA, 1, FAIXA_DA_LINHA), 0.31, "a linha anda 1 %");
        assert_eq!(tamanho_do_roteiro(12_595, 131_072), "12,3 KB de 128 KB");
    }

    #[test]
    fn o_arrasto_manda_no_maximo_um_a_cada_120_ms_e_o_ultimo_sempre_sai() {
        let mut l = LimiteDeEnvio::default();
        // Um arrasto de meio segundo, um movimento a cada 16 ms.
        let mut mandados = Vec::new();
        let mut t = 0.0;
        let mut v = 0.30;
        while t < 0.5 {
            if let Some(x) = l.mover(t, v) {
                mandados.push((t, x));
            }
            if let Some(x) = l.tique(t) {
                mandados.push((t, x));
            }
            t += 0.016;
            v += 0.004;
        }
        let final_ = l.soltar(t, v);
        assert!((final_ - v).abs() < 1e-12, "o último valor sai ao soltar");
        // Entre dois envios do arrasto, nunca menos de 120 ms.
        for par in mandados.windows(2) {
            assert!(par[1].0 - par[0].0 >= LimiteDeEnvio::INTERVALO_S - 1e-9, "{par:?}");
        }
        assert!(mandados.len() <= 5, "{} envios em 0,5 s", mandados.len());
        assert_eq!(l.envios as usize, mandados.len() + 1);
        // Parado depois de um movimento dentro do intervalo: o pendente sai no tique seguinte.
        let mut l = LimiteDeEnvio::default();
        assert_eq!(l.mover(0.0, 0.5), Some(0.5));
        assert_eq!(l.mover(0.05, 0.6), None);
        assert_eq!(l.tique(0.10), None);
        assert_eq!(l.tique(0.13), Some(0.6));
        assert_eq!(l.tique(0.30), None, "nada pendente, nada sai");
    }

    #[test]
    fn o_enquadramento_e_por_orientacao_e_as_setas_nao_se_cruzam() {
        let mut e = Enquadramento::default();
        assert_eq!(e.par(1280.0, 720.0), (0.0, 1.0));
        e.mover(1280.0, 720.0, 0, 0.1);
        e.mover(1280.0, 720.0, 1, 0.85);
        assert_eq!(e.par(1280.0, 720.0), (0.1, 0.85));
        // Retrato (o monitor girado): o enquadramento é outro.
        assert_eq!(e.par(720.0, 1280.0), (0.0, 1.0));
        // Uma seta não passa da outra: fica a pelo menos 20 % dela.
        e.mover(1280.0, 720.0, 0, 0.9);
        assert!((e.paisagem.0 - 0.65).abs() < 1e-9, "{:?}", e.paisagem);
        e.mover(1280.0, 720.0, 1, 0.0);
        assert!((e.paisagem.1 - 0.85).abs() < 1e-9, "{:?}", e.paisagem);
        e.mover(1280.0, 720.0, 0, -3.0);
        assert_eq!(e.paisagem.0, 0.0);
        e.mover(1280.0, 720.0, 1, f64::NAN);
        assert!((e.paisagem.1 - 0.85).abs() < 1e-9);
    }

    #[test]
    fn a_margem_e_fracao_da_area_entre_as_setas_e_o_texto_fica_no_meio() {
        // Largura inteira, margem de 10 %: a coluna começa a 10 % e tem 80 %.
        let (x, w) = Enquadramento::coluna((0.0, 1.0), 1000.0, 0.1);
        assert!((x - 100.0).abs() < 1e-9 && (w - 800.0).abs() < 1e-9);
        // Setas em 20 % e 80 %: a área tem 600 px, a margem de 10 % é de 60 px de cada lado.
        let (x, w) = Enquadramento::coluna((0.2, 0.8), 1000.0, 0.1);
        assert!((x - 260.0).abs() < 1e-9 && (w - 480.0).abs() < 1e-9);
        // O centro da coluna é o centro entre as setas.
        assert!((x + w / 2.0 - 500.0).abs() < 1e-9);
    }

    #[test]
    fn os_ajustes_locais_voltam_do_arquivo_e_o_arquivo_ruim_vira_padrao() {
        let mut a = AjustesLocais::default();
        a.enquadramento.mover(1280.0, 720.0, 0, 0.15);
        a.fonte_automatica = true;
        a.segurar_para_rolar = true;
        a.inverter_botoes = true;
        assert_eq!(AjustesLocais::de_json(&a.para_json()), a);
        // Um arquivo de antes do segurar (sem as chaves novas) lê com as duas desligadas.
        let velho = r#"{"enquadramento":{"paisagem":[0.1,0.9],"retrato":[0.0,1.0]},"fonte_automatica":true}"#;
        let lido = AjustesLocais::de_json(velho);
        assert!(lido.fonte_automatica && !lido.segurar_para_rolar && !lido.inverter_botoes);
        assert_eq!(AjustesLocais::de_json("não é json"), AjustesLocais::default());
        let trocado = r#"{"enquadramento":{"paisagem":[0.9,0.1],"retrato":[0.0,1.0]},"fonte_automatica":false}"#;
        assert_eq!(AjustesLocais::de_json(trocado).enquadramento.paisagem, (0.0, 1.0));
    }

    #[test]
    fn a_regra_da_palavra_sozinha_e_as_duas_excecoes() {
        assert!(linha_proibida(1, false, 5), "\"vamos\" sozinha no meio do parágrafo");
        assert!(!linha_proibida(2, false, 5));
        assert!(!linha_proibida(1, true, 5), "a última do parágrafo não conta");
        assert!(linha_proibida(1, false, 11), "11 letras ainda não é palavra longa");
        assert!(!linha_proibida(1, false, 12), "12 letras ou mais não conta");
        assert!(!linha_proibida(0, false, 0), "linha em branco não é palavra sozinha");
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(letras(&u("responsabilidade,")), 16, "a vírgula não é letra");
        assert_eq!(letras(&u("ação")), 4);
        assert_eq!(letras(&u("2026")), 0, "algarismo não é letra");
        assert_eq!(letras(&u("R$1.500.000,00")), 1);
    }

    #[test]
    fn a_busca_acha_a_maior_fonte_que_passa_de_1_em_1() {
        // Um roteiro que passa até 73 pontos.
        let (f, perguntas) = maior_fonte_que_passa(|f| f <= 73.0);
        assert_eq!(f, Some(73.0));
        assert!(perguntas <= 10, "{perguntas} perguntas");
        assert_eq!(maior_fonte_que_passa(|_| true).0, Some(400.0));
        assert_eq!(maior_fonte_que_passa(|_| false).0, None, "nem 8 passa: a fonte fica como está");
        assert_eq!(maior_fonte_que_passa(|f| f <= 8.0).0, Some(8.0));
        // O que a busca devolve sempre passou: com uma regra que não é monótona (passa em 8–60 e
        // em 90–100), ela devolve uma fonte testada, e não o palpite entre as duas faixas.
        let mut testadas = Vec::new();
        let (f, _) = maior_fonte_que_passa(|f| {
            testadas.push(f);
            f <= 60.0 || (90.0..=100.0).contains(&f)
        });
        let f = f.unwrap();
        assert!(testadas.contains(&f) && (f <= 60.0 || (90.0..=100.0).contains(&f)), "{f}");
    }

    #[test]
    fn a_regra_no_roteiro_inteiro_pega_a_palavra_sozinha_e_a_palavra_partida() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        fn linha(t: &[u16], fim: bool) -> LinhaQuebrada<'_> {
            // O texto da linha sem a quebra do fim, como o DirectWrite devolve `newlineLength`.
            let sem = if t.last() == Some(&0x0A) { &t[..t.len() - 1] } else { t };
            LinhaQuebrada { texto: sem, fim_de_paragrafo: fim }
        }
        // "todos" fecha o parágrafo: passa.
        let (a, b) = (u("Bom dia a "), u("todos\n"));
        assert_eq!(primeira_linha_reprovada(&[linha(&a, false), linha(&b, true)]), None);
        // "vamos " sozinha no meio do parágrafo: reprova, em qualquer fonte.
        let (c, d) = (u("vamos "), u("começar agora"));
        assert_eq!(primeira_linha_reprovada(&[linha(&c, false), linha(&d, true)]), Some(0));
        // "responsabilidade " sozinha: palavra longa, não conta.
        let (e, f) = (u("responsabilidade "), u("de todos"));
        assert_eq!(primeira_linha_reprovada(&[linha(&e, false), linha(&f, true)]), None);
        // "anticonstitu" | "cionalissimamente": o pedaço tem 12 letras, mas a palavra partida
        // reprova.
        let (p1, p2) = (u("anticonstitu"), u("cionalissimamente"));
        assert_eq!(primeira_linha_reprovada(&[linha(&p1, false), linha(&p2, true)]), Some(0));
        // A última linha do roteiro, sem quebra no fim, é fim de parágrafo.
        let g = u("Tchau");
        assert_eq!(primeira_linha_reprovada(&[linha(&g, true)]), None);
        // Um número de 12 algarismos sozinho é uma palavra (tem algarismo), mas de zero letras:
        // reprova.
        let (n1, n2) = (u("123456789012 "), u("segue aqui"));
        assert_eq!(primeira_linha_reprovada(&[linha(&n1, false), linha(&n2, true)]), Some(0));
    }

    #[test]
    fn as_acoes_de_bancada_leem_a_gramatica_do_mac() {
        let a = AcoesDeBancada::ler(
            "3:fonte=64,1:espelho=1,1:rolando=0,8:texto+=Linha nova,2:texto=@roteiro.txt,\
             4:pular=-0.1,5:editor=manter-meu,6:captura=C:\\x\\a.bmp,7:rascunho+=oi,9:salto=0,10:arrastar=0.42,\
             11:seta_esquerda=0.1,12:seta_direita=0.9,13:fonte_auto=1,14:modo_segurar=1,15:apertar=cima,\
             16:soltar=1,17:tecla=baixo,17.1:tecla_repete=baixo,18:tecla_solta=baixo,19:inverter=1,\
             20:escolha=prompter,21:escolha=meu,22:roteiros=abrir,23:roteiro_ver=1,24:roteiros=voltar,\
             25:roteiro_usar=2,26:confirmar=sim,27:roteiro_apagar=3,28:confirmar=nao,29:roteiros=fechar,\
             30:arquivo-no-editor=C:\\r\\utf16.txt",
            |c| (c == "roteiro.txt").then(|| "conteúdo".to_string()),
        );
        assert!(a.recusadas.is_empty(), "{:?}", a.recusadas);
        let esperado = vec![
            (1.0, Acao::Espelho(true)),
            (1.0, Acao::Rolando(false)),
            (2.0, Acao::Texto("conteúdo".into())),
            (3.0, Acao::Fonte(64.0)),
            (4.0, Acao::Pular(-0.1)),
            (5.0, Acao::Editor(ComandoDoEditor::ManterMeu)),
            (6.0, Acao::Captura("C:\\x\\a.bmp".into())),
            (7.0, Acao::Rascunho("oi".into())),
            (8.0, Acao::Acrescentar("Linha nova".into())),
            (9.0, Acao::Salto(0.0)),
            (10.0, Acao::ArrastarLinha(0.42)),
            (11.0, Acao::ArrastarEnquadramento(0, 0.1)),
            (12.0, Acao::ArrastarEnquadramento(1, 0.9)),
            (13.0, Acao::FonteAutomatica(true)),
            (14.0, Acao::ModoSegurar(true)),
            (15.0, Acao::Apertar(BotaoDeSegurar::Cima)),
            (16.0, Acao::SoltarBotao),
            (17.0, Acao::TeclaDesce(BotaoDeSegurar::Baixo, false)),
            (17.1, Acao::TeclaDesce(BotaoDeSegurar::Baixo, true)),
            (18.0, Acao::TeclaSobe(BotaoDeSegurar::Baixo)),
            (19.0, Acao::InverterBotoes(true)),
            (20.0, Acao::Escolha(false)),
            (21.0, Acao::Escolha(true)),
            (22.0, Acao::Roteiros(ComandoDosRoteiros::Abrir)),
            (23.0, Acao::Roteiros(ComandoDosRoteiros::Ver(1))),
            (24.0, Acao::Roteiros(ComandoDosRoteiros::VoltarALista)),
            (25.0, Acao::Roteiros(ComandoDosRoteiros::Usar(2))),
            (26.0, Acao::Roteiros(ComandoDosRoteiros::Sim)),
            (27.0, Acao::Roteiros(ComandoDosRoteiros::Apagar(3))),
            (28.0, Acao::Roteiros(ComandoDosRoteiros::Nao)),
            (29.0, Acao::Roteiros(ComandoDosRoteiros::Fechar)),
            (30.0, Acao::ArquivoNoEditor("C:\\r\\utf16.txt".into())),
        ];
        assert_eq!(a.acoes, esperado);
    }

    #[test]
    fn a_acao_ilegivel_e_recusada_e_nao_engolida() {
        let a = AcoesDeBancada::ler(
            "x:fonte=1,2:fonte=grande,3:nada=1,4fonte=2,5:texto=@falta.txt,6:arrastar=1.5,7:seta_direita=2,\
             8:apertar=lado,9:tecla=esquerda,10:escolha=talvez,11:roteiro_ver=4,12:confirmar=depois",
            |_| None,
        );
        assert!(a.acoes.is_empty());
        assert_eq!(a.recusadas.len(), 12);
    }

    #[test]
    fn as_palavras_da_linha_sao_as_que_tem_letra_ou_numero() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(palavras_da_linha(&u("Bom dia a todos ")), (4, Some((0, 3))));
        assert_eq!(palavras_da_linha(&u("  sozinha\n")), (1, Some((2, 7))));
        // O travessão sozinho não é palavra: "— palavra" é uma linha de uma palavra só.
        assert_eq!(palavras_da_linha(&u("— palavra")), (1, Some((2, 7))));
        assert_eq!(palavras_da_linha(&u("... \u{2029}")), (0, None));
        assert_eq!(palavras_da_linha(&u("2026, ação")), (2, Some((0, 5))));
        assert_eq!(palavras_da_linha(&u("olá👋")), (1, Some((0, 5))), "o emoji conta como parte da palavra");
    }

    /// **As frases montadas em inglês** (a tradução EN/PT): o molde vem da tabela, as partes entram
    /// em ordem, e os números ganham o ponto do inglês. Em português, as mesmas de sempre.
    #[test]
    fn frases_montadas_em_ingles() {
        use crate::idioma::{com_idioma, Idioma};
        com_idioma(Idioma::En, || {
            assert_eq!(
                conselho(Lado::Controle, &falha(Codigo::PinErrado), Decisao::Parar, 0, ""),
                "Wrong PIN. Check the six digits on the prompter's screen and connect again — the prompter's PIN changes after a mistake."
            );
            assert_eq!(
                conselho(Lado::Prompter, &falha(Codigo::Io), Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: 0 }, 7979, "os error 10048"),
                "Port 7979 failed (os error 10048). Trying again."
            );
            assert_eq!(conselho(Lado::Controle, &falha(Codigo::Invalido), Decisao::Parar, 0, "x"), "Couldn't connect: x");
            assert_eq!(tamanho_do_roteiro(12_595, 131_072), "12.3 KB of 128 KB");
            assert_eq!(frase_do_teto(140_000, 131_072), "The script is 140000 bytes; the maximum is 131072 (about 20,000 words).");
            assert_eq!(rotulo_de_palavras(1), "1 word");
            assert_eq!(rotulo_de_palavras(1_234), "1,234 words");
            assert_eq!(titulo_da_pergunta("iPad"), "The prompter iPad has a different script.");
            assert_eq!(previa_do_roteiro("  ", LETRAS_DA_PREVIA), "No script.");
        });
        com_idioma(Idioma::Pt, || {
            assert_eq!(
                conselho(Lado::Prompter, &falha(Codigo::Io), Decisao::TentarDeNovo { pin: EscolhaDoPin::Mesmo, depois_ms: 0 }, 7979, "os error 10048"),
                "A porta 7979 falhou (os error 10048). Tentando de novo."
            );
            assert_eq!(frase_do_teto(140_000, 131_072), "O roteiro tem 140000 bytes; o máximo é 131072 (cerca de 20 mil palavras).");
        });
    }
}
