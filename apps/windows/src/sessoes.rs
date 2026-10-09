//! **A tabela de sessões** do emissor com vários receptores: quem está esperando, quem está no ar,
//! quem está saindo, e o que fazer a cada aviso.
//!
//! # Por que existe, e por que é pura
//!
//! É o porte da metade "do app" de `apps/macos/Sources/QuallApp/Emissor.swift` (as sessões, a
//! espera seguinte, o limite, o índice por aparelho, a reconexão do mesmo aparelho, a espera que
//! falha). No Mac essa lógica mora na classe que também é a interface, e os avisos chegam pela
//! fila principal, que serializa tudo. Aqui ela é uma **máquina de estados sem Windows, sem rede e
//! sem thread**: recebe avisos (`espelhar`, `conectou`, `falhou_ao_hospedar`, `saiu`,
//! `desmontada`, `desconectar`, `encerrar`, `tique`) e pede efeitos por [`Efeitos`] (abrir uma
//! espera, anunciar, mandar transmitir, mandar encerrar, gravar os índices). Quem serializa é a
//! thread do coordenador (`crate::varias`), a única que chama estes métodos.
//!
//! O ganho é o que a revisão adversarial do Mac mostrou que faltava: **cada um dos dez achados
//! dela vira um teste** (`testes`, no fim do arquivo), sem aparelho e sem sessão de verdade.
//!
//! # A diferença para o Mac: nenhuma closure guardada
//!
//! O Mac guarda "quem espera o fim do desmonte" como closures na sessão. Aqui o fim de toda sessão
//! conectada chega como **um** aviso, `desmontada(id)`, mandado pela thread da sessão depois de ela
//! soltar o que tinha (a cadeia, a sessão do núcleo e o monitor). Quem esperava por ela está
//! escrito em `aguardando`, e é liberado ali. Não há lista para esquecer de avisar — o defeito do
//! Mac em que Parar durante um desmonte deixava o app preso em "Encerrando…" para sempre.
//!
//! # Identificadores nunca se repetem
//!
//! O Mac zera `proximoId` na volta ao começo e reconhece a sessão pela identidade do objeto. Aqui a
//! sessão é um número, e um número zerado faria o aviso atrasado de uma sessão de outra rodada cair
//! em cima de uma sessão nova com o mesmo número. Então o contador só sobe.

use std::collections::{BTreeMap, BTreeSet};

use crate::som_puxado::{toca_o_som_do_windows, DonoDoSom};
use crate::tabela_de_indices::TabelaDeIndices;

pub type Id = u64;

/// **8**, o mesmo do Mac (`Emissor.limiteDeMonitores`), onde foi medido no laço em 11/09:
/// monitor parado não custa nada, e o teto é o de pixels que mudam. No Windows **não foi medido**
/// com captura de verdade — ver a prova de mecanismo em `docs/app-windows.md`.
pub const LIMITE_DE_SESSOES: usize = 8;

/// Uma espera extra que falha antes disto não é reaberta: seria um laço de falhas (Mac).
pub const REABRIR_SO_DEPOIS_DE_MS: u64 = 3_000;

/// Quanto tempo o Parar espera as sessões desmontarem antes de voltar ao começo sem elas.
///
/// Não existe no Mac. Existe aqui porque a thread de uma sessão pode ficar presa num desmonte (a
/// armadilha do mutex global da libdatachannel no Windows, `docs/divida-do-nucleo.md`), e o
/// requisito é "parar no meio de um desmonte não pode deixar o app preso". Os avisos atrasados da
/// sessão presa são ignorados depois disso — ela é de outra rodada.
pub const PRAZO_DO_DESMONTE_MS: u64 = 15_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fase {
    Inicial,
    Esperando,
    Transmitindo,
    Encerrando,
}

/// `Encerrada` não existe: a sessão encerrada sai da tabela.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Estado {
    Esperando,
    /// Conectou. Pode estar esperando a sessão velha do mesmo aparelho desmontar antes de receber
    /// a ordem de transmitir — ver [`Sessao::ordem_dada`].
    Transmitindo,
    Encerrando,
}

/// O par, como o núcleo o devolveu depois do pareamento.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Par {
    pub nome: String,
    pub device_id: String,
    /// Os pixels do painel, quando o receptor os disse no aperto de mão (`Announcement::screen`).
    pub tela: Option<(u32, u32)>,
}

/// Por que uma espera acabou sem ninguém no ar. Espelha os braços de `emissor.rs::ao_falhar`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Falha {
    Cancelada,
    /// `Error::WrongPin` (dívida 29): o PIN não conferiu. **Achado pela prova de mecanismo de
    /// 13/09**: a primeira versão desta tabela o tratava como falha qualquer, e a espera reabria
    /// com o mesmo PIN — que o aparelho do outro lado já tinha errado.
    PinErrado,
    /// `Error::Pairing`: o outro lado recusou o pareamento, ou um dos lados o esqueceu (ver o
    /// comentário do braço em `emissor.rs`).
    Pareamento,
    PrecisaDePin,
    SemRota,
    Prazo,
    Outra(String),
}

/// **A frase do usuário** (decisão de 14/09/2026, `docs/monitor-virtual-windows.md` §13.4 item 8),
/// literal: o `CreateForMonitor` sobre o monitor virtual não voltou no prazo. No Dell isso
/// acompanhou o estado do Windows depois de ~490 monitores desde o boot, e o reinício destravou.
/// Em português: é comparado (`sessao_de_emissao.rs`) e a janela o traduz na hora de mostrar.
pub const AVISO_DA_CAPTURA_PRESA: &str = "A captura do Windows não respondeu. Reiniciar o Windows costuma resolver."; // i18n: chave

/// Por que uma sessão no ar acabou sozinha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saida {
    Saiu,
    Caiu,
    FonteSumiu(String),
    /// A captura nem subiu — frase própria, e não "parou sozinha" (revisão do Mac).
    NaoIniciou(String),
    /// A captura do Windows não voltou no prazo: a tela mostra [`AVISO_DA_CAPTURA_PRESA`], literal.
    CapturaPresa,
}

/// O que vale para todas as esperas de um Espelhar.
#[derive(Clone, Debug)]
pub struct Rodada {
    /// Vários receptores. Sem isto a tabela se comporta como o produto de hoje: uma espera, e a
    /// saída do receptor encerra tudo.
    pub varias: bool,
    pub limite: usize,
    pub com_som: bool,
    /// D2: quando o receptor que toca o som sai, o som passa ao próximo que toca
    /// (`--sem-passar-som` desliga). Ver [`DonoDoSom`].
    pub passar_som: bool,
    /// A porta da **primeira** espera (`--porta`; 0 é uma livre). As seguintes pegam uma livre.
    pub porta: u16,
    /// PIN fixo de bancada. `None` é o produto: cada espera sorteia o seu.
    pub pin: Option<String>,
}

/// O que a tabela pede ao mundo. Implementado de verdade pelo coordenador e de mentira nos testes.
pub trait Efeitos {
    /// Abre uma espera: o servidor na `porta` (0 = uma livre) e o PIN (`None` = sortear). Devolve
    /// a porta e o PIN de fato. Erro quando não há porta.
    fn abrir_espera(
        &mut self,
        id: Id,
        porta: u16,
        pin: Option<&str>,
        com_audio: bool,
    ) -> Result<(u16, String), String>;
    /// O anúncio mDNS passa a apontar para `porta`; `None` tira do ar.
    fn anunciar(&mut self, porta: Option<u16>);
    /// A sessão pode transmitir, com o monitor de índice `indice` e o formato da tela do par.
    fn transmitir(&mut self, id: Id, indice: usize, par: &Par);
    /// Encerra a sessão: cancela a espera, ou para a transmissão e desmonta. Ela avisa
    /// `falhou_ao_hospedar(Cancelada)` (se ainda esperava) ou `desmontada` quando acabar.
    fn encerrar(&mut self, id: Id);
    /// **D2**: a sessão `id` passa a mandar o som (`true`) ou para de mandar (`false`). Toda sessão
    /// com som oferece a track e captura; só a dona manda os pacotes.
    fn dar_o_som(&mut self, id: Id, mandar: bool);
    fn gravar_indices(&mut self, tabela: &TabelaDeIndices);
    fn registrar(&mut self, linha: String);
}

#[derive(Clone, Debug)]
pub struct Sessao {
    pub id: Id,
    pub estado: Estado,
    pub porta: u16,
    pub pin: String,
    pub criada_em_ms: u64,
    pub par: Par,
    pub indice: Option<usize>,
    /// A sessão oferece a track de som (todas, com o som ligado na rodada: D2). Quem **manda** é
    /// a dona, [`Tabela::dona_do_som`].
    pub com_audio: bool,
    /// Já recebeu a ordem de transmitir. Uma sessão conectada sem ela está esperando a velha do
    /// mesmo aparelho soltar o monitor.
    pub ordem_dada: bool,
    /// Pareou. Uma espera cancelada vai de `Esperando` a `Encerrando` sem nunca ter conectado — e
    /// sem nunca ter batido o laço, que é o que o cão de guarda do coordenador vigia.
    pub conectou: bool,
    /// D2: a sessão tem som de pé. Cai para `false` com o aviso de que ela ficou sem som
    /// ([`Tabela::sem_som`]), e então ela nunca é a dona.
    pub som_de_pe: bool,
}

/// Um receptor no ar, para a tela.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceptorNoAr {
    pub id: Id,
    pub nome: String,
    pub indice: Option<usize>,
    pub tela: Option<(u32, u32)>,
    /// Conectou e espera a sessão velha do mesmo aparelho sair.
    pub aguardando_a_velha: bool,
    /// D2: é o receptor que recebe o som.
    pub com_o_som: bool,
}

/// O que a interface lê.
#[derive(Clone, Debug)]
pub struct Retrato {
    pub fase: Fase,
    /// O PIN da espera aberta; vazio quando não há espera.
    pub pin: String,
    /// A porta da espera aberta, ou a do primeiro receptor no ar.
    pub porta: Option<u16>,
    /// Transmitindo **e** com uma espera aberta para mais um.
    pub esperando_mais_um: bool,
    pub receptores: Vec<ReceptorNoAr>,
    pub conselho: String,
    pub oferece_desparear: bool,
}

pub struct Tabela {
    sessoes: Vec<Sessao>,
    proximo_id: Id,
    rodada: Option<Rodada>,
    ja_abriu_espera_na_rodada: bool,
    encerrando_tudo: bool,
    encerrando_desde_ms: Option<u64>,
    fase: Fase,
    /// Sessão nova → as velhas do mesmo aparelho que ela espera desmontarem.
    aguardando: BTreeMap<Id, BTreeSet<Id>>,
    indices: TabelaDeIndices,
    /// Índices de monitores que não confirmaram a saída (`monitor_nao_soltou`): em quarentena
    /// pelo resto do processo. Um monitor virtual que ficou de pé com a identidade de um aparelho
    /// não pode ganhar um gêmeo.
    presos: BTreeSet<usize>,
    conselho: String,
    oferece_desparear: bool,
    /// D2: qual sessão manda o som.
    dono_do_som: DonoDoSom,
}

impl Tabela {
    pub fn nova(indices: TabelaDeIndices) -> Self {
        Tabela {
            sessoes: Vec::new(),
            proximo_id: 1,
            rodada: None,
            ja_abriu_espera_na_rodada: false,
            encerrando_tudo: false,
            encerrando_desde_ms: None,
            fase: Fase::Inicial,
            aguardando: BTreeMap::new(),
            indices,
            presos: BTreeSet::new(),
            conselho: String::new(),
            oferece_desparear: false,
            dono_do_som: DonoDoSom::default(),
        }
    }

    pub fn fase(&self) -> Fase {
        self.fase
    }

    pub fn sessoes(&self) -> &[Sessao] {
        &self.sessoes
    }

    pub fn sessao(&self, id: Id) -> Option<&Sessao> {
        self.sessoes.iter().find(|s| s.id == id)
    }

    pub fn indices(&self) -> &TabelaDeIndices {
        &self.indices
    }

    pub fn rodada(&self) -> Option<&Rodada> {
        self.rodada.as_ref()
    }

    /// D2: a sessão que manda o som agora.
    pub fn dona_do_som(&self) -> Option<Id> {
        self.dono_do_som.dono()
    }

    /// D2: leva ao mundo a troca de dona, se houve. `antes` é a dona de antes do aviso.
    fn aplicar_o_dono(&mut self, antes: Option<Id>, motivo: &str, fx: &mut impl Efeitos) {
        let agora = self.dono_do_som.dono();
        if agora == antes {
            return;
        }
        if let Some(a) = antes {
            fx.dar_o_som(a, false);
        }
        match agora {
            Some(n) => {
                fx.registrar(format!("som: a sessão #{n} passa a mandar o som — {motivo}")); // i18n: fora (diário)
                fx.dar_o_som(n, true);
            }
            None => {
                let porque = match self.dono_do_som.carencia() {
                    Some((aparelho, ate)) => format!(" (o som espera {aparelho} voltar até {ate} ms)"), // i18n: fora (diário)
                    None if !self.dono_do_som.passar => " (a passagem está desligada)".to_string(), // i18n: fora (diário)
                    None => String::new(),
                };
                fx.registrar(format!("som: ninguém manda o som agora — {motivo}{porque}")); // i18n: fora (diário)
            }
        }
    }

    /// D2: a sessão `id` deixou de estar no ar. Nada durante o Parar: a rodada inteira sai.
    fn som_saiu(&mut self, id: Id, fx: &mut impl Efeitos) {
        if self.encerrando_tudo {
            return;
        }
        let antes = self.dono_do_som.dono();
        self.dono_do_som.saiu(id);
        self.aplicar_o_dono(antes, &format!("a sessão #{id} saiu"), fx); // i18n: fora (diário)
    }

    /// Um conselho que não vem de uma sessão (a tela estendida que não sobe, por exemplo).
    pub fn definir_conselho(&mut self, texto: String) {
        self.conselho = texto;
    }

    pub fn limpar_conselho(&mut self) {
        self.conselho.clear();
        self.oferece_desparear = false;
    }

    fn pos(&self, id: Id) -> Option<usize> {
        self.sessoes.iter().position(|s| s.id == id)
    }

    fn vivas(&self) -> usize {
        self.sessoes.iter().filter(|s| s.estado == Estado::Transmitindo).count()
    }

    fn espera(&self) -> Option<&Sessao> {
        self.sessoes.iter().find(|s| s.estado == Estado::Esperando)
    }

    // MARK: - avisos

    /// O Espelhar. `false` quando nem a primeira espera abriu (o conselho diz por quê).
    pub fn espelhar(&mut self, rodada: Rodada, agora_ms: u64, fx: &mut impl Efeitos) -> bool {
        if self.fase != Fase::Inicial {
            return false;
        }
        self.conselho.clear();
        self.oferece_desparear = false;
        self.dono_do_som = DonoDoSom::novo(rodada.passar_som);
        self.rodada = Some(rodada);
        self.ja_abriu_espera_na_rodada = false;
        self.encerrando_tudo = false;
        if !self.abrir_espera(None, None, agora_ms, fx) {
            self.rodada = None;
            return false;
        }
        self.fase = Fase::Esperando;
        true
    }

    /// Abre uma espera. `porta`/`pin` pedidos só na reabertura de uma espera que falhou.
    fn abrir_espera(
        &mut self,
        porta: Option<u16>,
        pin: Option<String>,
        agora_ms: u64,
        fx: &mut impl Efeitos,
    ) -> bool {
        let Some(rodada) = self.rodada.clone() else {
            return false;
        };
        let porta = porta.unwrap_or(if self.ja_abriu_espera_na_rodada { 0 } else { rodada.porta });
        let pin = pin.or(rodada.pin.clone());
        // **Toda sessão oferece o som; só a dona manda** (D2 do `docs/som-no-receptor.md` §12.1).
        // O som do sistema é a mistura da máquina inteira, e mandá-lo junto de cada monitor seria
        // o mesmo som em vários aparelhos (revisão do Mac, 10/09). Até a S6 só a primeira espera
        // levava a track, e o som não tinha como passar a quem já estava conectado: o Quall não
        // renegocia, e a track tem de estar na oferta desde o começo.
        let com_audio = rodada.com_som;
        let id = self.proximo_id;
        self.proximo_id += 1;
        match fx.abrir_espera(id, porta, pin.as_deref(), com_audio) {
            Ok((porta, pin)) => {
                self.ja_abriu_espera_na_rodada = true;
                self.sessoes.push(Sessao {
                    id,
                    estado: Estado::Esperando,
                    porta,
                    pin,
                    criada_em_ms: agora_ms,
                    par: Par::default(),
                    indice: None,
                    com_audio,
                    ordem_dada: false,
                    conectou: false,
                    som_de_pe: com_audio,
                });
                // O anúncio aponta sempre para **a espera aberta**. Continuar anunciando uma porta
                // que já conectou convidaria um terceiro aparelho para um servidor que não aceita
                // mais ninguém.
                fx.anunciar(Some(porta));
                true
            }
            Err(motivo) => {
                self.conselho = crate::idioma::tf("Não consegui abrir a espera por mais um aparelho: {}", &[&motivo]);
                fx.registrar(format!("!! a espera #{id} não abriu: {motivo}")); // i18n: fora (diário)
                false
            }
        }
    }

    /// A espera `id` pareou. A sessão nova só transmite depois de a velha do mesmo aparelho sair.
    pub fn conectou(&mut self, id: Id, par: Par, agora_ms: u64, fx: &mut impl Efeitos) {
        let Some(i) = self.pos(id) else {
            // Uma sessão que já não é desta rodada (a pessoa parou e espelhou de novo no meio):
            // fecha e esquece — senão ela transmitiria por fora da lista, onde o Parar não alcança.
            fx.registrar(format!("[#{id}] conectou fora da rodada — fechando")); // i18n: fora (diário)
            fx.encerrar(id);
            return;
        };
        // Parar pode ter chegado enquanto o par fechava: a sessão já está sendo desmontada.
        if self.sessoes[i].estado != Estado::Esperando {
            return;
        }
        self.sessoes[i].par = par.clone();
        self.sessoes[i].estado = Estado::Transmitindo;
        self.sessoes[i].conectou = true;
        if self.encerrando_tudo || self.rodada.is_none() {
            return;
        }
        self.conselho.clear();
        self.oferece_desparear = false;

        // **O mesmo aparelho de novo, antes de a sessão velha cair** (o Wi-Fi piscou e ele
        // reconectou): a velha sai primeiro, e só depois a nova transmite — dois monitores do
        // mesmo aparelho ao mesmo tempo dividiriam identidade, e esperar a velha soltar o monitor é
        // o que deixa a nova herdar o índice do aparelho.
        let velhas: BTreeSet<Id> = if par.device_id.is_empty() {
            BTreeSet::new()
        } else {
            self.sessoes
                .iter()
                .filter(|s| {
                    s.id != id
                        && matches!(s.estado, Estado::Transmitindo | Estado::Encerrando)
                        && s.par.device_id == par.device_id
                })
                .map(|s| s.id)
                .collect()
        };
        if velhas.is_empty() {
            self.iniciar_transmissao(id, agora_ms, fx);
        } else {
            // D2: a sessão nova herda a vaga (e o som, se era dela) da velha **antes** de a velha
            // sair — senão o som passaria ao segundo receptor e a nova entraria calada (crítica 9,
            // M2, do Mac).
            if self.sessoes[i].com_audio {
                let toca = toca_o_som_do_windows(&par.device_id) && self.sessoes[i].som_de_pe;
                let antes = self.dono_do_som.dono();
                for v in &velhas {
                    self.dono_do_som.substituir(*v, id, toca, &par.device_id, agora_ms);
                }
                self.aplicar_o_dono(antes, &format!("o mesmo aparelho voltou pela sessão #{id}"), fx); // i18n: fora (diário)
            }
            for v in &velhas {
                fx.registrar(format!("[#{v}] o mesmo aparelho voltou pela sessão #{id} — esta sai antes")); // i18n: fora (diário)
                self.encerrar_uma(*v, fx);
            }
            self.aguardando.insert(id, velhas);
        }
        self.publicar();
    }

    fn iniciar_transmissao(&mut self, id: Id, agora_ms: u64, fx: &mut impl Efeitos) {
        if self.encerrando_tudo {
            return;
        }
        let Some(rodada) = self.rodada.clone() else {
            return;
        };
        let Some(i) = self.pos(id) else {
            return;
        };
        if self.sessoes[i].estado != Estado::Transmitindo || self.sessoes[i].ordem_dada {
            return;
        }
        // Os índices dos monitores ainda de pé — inclusive os que estão saindo — ficam de fora.
        let mut em_uso: BTreeSet<usize> = self
            .sessoes
            .iter()
            .filter(|s| s.id != id && matches!(s.estado, Estado::Transmitindo | Estado::Encerrando))
            .filter_map(|s| s.indice)
            .collect();
        em_uso.extend(self.presos.iter().copied());
        let device_id = self.sessoes[i].par.device_id.clone();
        let indice = if device_id.is_empty() {
            self.indices.indice_sem_identidade(&em_uso)
        } else {
            let n = self.indices.indice(&device_id, &em_uso, agora_ms as f64 / 1000.0);
            fx.gravar_indices(&self.indices);
            n
        };
        self.sessoes[i].indice = Some(indice);
        self.sessoes[i].ordem_dada = true;
        let par = self.sessoes[i].par.clone();
        fx.transmitir(id, indice, &par);

        // D2: o primeiro receptor **que toca** ganha o som; os seguintes oferecem e calam (M3 do
        // Mac: quem não toca nunca é dono). A sessão que substituiu a velha já está na fila.
        if self.sessoes[i].com_audio {
            let toca = toca_o_som_do_windows(&par.device_id) && self.sessoes[i].som_de_pe;
            let antes = self.dono_do_som.dono();
            self.dono_do_som.conectou(id, toca, &par.device_id, agora_ms);
            if !toca {
                fx.registrar(format!(
                    "[#{id}] som: {} {}; não recebe o som", // i18n: fora (diário)
                    if par.nome.is_empty() { "este receptor" } else { par.nome.as_str() }, // i18n: fora (diário)
                    if self.sessoes[i].som_de_pe {
                        format!("não toca o som do Windows (device_id {})", par.device_id) // i18n: fora (diário)
                    } else {
                        "está numa sessão sem som".to_string() // i18n: fora (diário)
                    }
                ));
            }
            self.aplicar_o_dono(antes, "o primeiro receptor que toca, com ninguém tocando", fx); // i18n: fora (diário)
        }

        // **A espera seguinte**: várias e abaixo do limite. Porta nova e PIN novo — a porta da
        // sessão que conectou continua presa ao servidor dela.
        if rodada.varias && self.vivas() < rodada.limite && self.espera().is_none() {
            if !self.abrir_espera(None, None, agora_ms, fx) {
                fx.anunciar(None);
            }
        } else if self.espera().is_none() {
            fx.anunciar(None);
        }
    }

    /// A espera `id` acabou sem par. A thread dela já soltou o servidor.
    pub fn falhou_ao_hospedar(&mut self, id: Id, falha: Falha, agora_ms: u64, fx: &mut impl Efeitos) {
        let Some(i) = self.pos(id) else {
            return; // de outra rodada: nada a fazer, e nenhum aviso
        };
        let s = self.sessoes.remove(i);
        self.aguardando.remove(&id);
        // Cancelar é o caminho normal, não uma falha: a pessoa desistiu, ou foi este app que fechou
        // a espera. A tela volta sem aviso vermelho.
        if falha == Falha::Cancelada || self.encerrando_tudo {
            if self.sessoes.is_empty() {
                self.voltar_ao_inicio(fx);
            } else {
                self.publicar();
            }
            return;
        }
        let varias = self.rodada.as_ref().map(|r| r.varias).unwrap_or(false);
        let reabre = varias && self.vivas() > 0;
        let (texto, desparear) = conselho_da_falha(&falha, reabre);
        if !texto.is_empty() {
            self.conselho = texto;
        }
        if desparear {
            self.oferece_desparear = true;
        }
        if !reabre {
            self.voltar_ao_inicio(fx);
            return;
        }
        // **Só a espera caiu; as sessões no ar continuam** (revisão do Mac): um PIN errado na espera
        // de mais um aparelho não pode derrubar os monitores que já transmitem.
        if agora_ms.saturating_sub(s.criada_em_ms) < REABRIR_SO_DEPOIS_DE_MS {
            fx.registrar(format!("[#{id}] !! a espera falhou em menos de 3 s — não reabro")); // i18n: fora (diário)
            fx.anunciar(None);
            self.publicar();
            return;
        }
        // Na mesma porta (a espera que falhou já soltou o servidor dela), com PIN novo quando o
        // problema pode ter sido o PIN.
        let pin_novo = matches!(falha, Falha::PinErrado | Falha::Pareamento | Falha::PrecisaDePin);
        let pin = if pin_novo { None } else { Some(s.pin.clone()) };
        if !self.abrir_espera(Some(s.porta), pin, agora_ms, fx) {
            fx.anunciar(None);
        }
        self.publicar();
    }

    /// Uma sessão no ar acabou sozinha (o par saiu, a conexão caiu, a fonte sumiu, a captura não
    /// subiu). A thread dela já está desmontando; o `desmontada` vem depois.
    ///
    /// **D2, a queda**: se a conexão da dona do som **caiu**, o som espera o aparelho dela por 10 s
    /// (a carência, `DonoDoSom::caiu`); nas outras saídas, passa na hora.
    pub fn saiu(&mut self, id: Id, motivo: Saida, agora_ms: u64, fx: &mut impl Efeitos) {
        let Some(i) = self.pos(id) else {
            return;
        };
        if self.sessoes[i].estado != Estado::Transmitindo || self.encerrando_tudo {
            return;
        }
        if motivo == Saida::Caiu {
            let antes = self.dono_do_som.dono();
            self.dono_do_som.caiu(id, agora_ms);
            self.aplicar_o_dono(antes, &format!("a conexão da sessão #{id} caiu"), fx); // i18n: fora (diário)
        }
        let quem = if self.sessoes[i].par.nome.is_empty() {
            crate::idioma::t("O outro aparelho").to_string()
        } else {
            self.sessoes[i].par.nome.clone()
        };
        self.conselho = match motivo {
            Saida::Saiu => crate::idioma::tf("{} saiu.", &[&quem]),
            Saida::Caiu => crate::idioma::tf("A conexão com {} caiu.", &[&quem]),
            Saida::FonteSumiu(t) => t,
            Saida::NaoIniciou(t) => crate::idioma::tf("Não consegui iniciar a captura: {}", &[&t]),
            Saida::CapturaPresa => AVISO_DA_CAPTURA_PRESA.to_string(),
        };
        let restam = self
            .sessoes
            .iter()
            .any(|s| s.id != id && matches!(s.estado, Estado::Transmitindo | Estado::Esperando));
        let varias = self.rodada.as_ref().map(|r| r.varias).unwrap_or(false);
        if restam && varias {
            self.encerrar_uma(id, fx);
        } else {
            self.encerrar(fx);
        }
        self.publicar();
    }

    /// O monitor da sessão `id` não confirmou que saiu dentro do prazo. Chamado **antes** de
    /// `desmontada`: o índice dele fica em quarentena, e a sessão sai assim mesmo — esperar para
    /// sempre prenderia o aparelho que reconecta (revisão adversarial de 13/09/2026).
    pub fn monitor_nao_soltou(&mut self, id: Id, fx: &mut impl Efeitos) {
        if let Some(i) = self.sessao(id).and_then(|s| s.indice) {
            self.presos.insert(i);
            fx.registrar(format!(
                "[#{id}] !! o monitor de índice {i} não confirmou a saída — índice em quarentena" // i18n: fora (diário)
            ));
        }
    }

    /// A thread da sessão `id` terminou: soltou a cadeia, a sessão do núcleo e o monitor. É o
    /// último aviso de toda sessão que conectou.
    pub fn desmontada(&mut self, id: Id, agora_ms: u64, fx: &mut impl Efeitos) {
        let Some(i) = self.pos(id) else {
            return;
        };
        // D2: normalmente já saiu da fila quando começou a encerrar; isto cobre o resto.
        self.som_saiu(id, fx);
        self.sessoes.remove(i);
        self.aguardando.remove(&id);
        let mut liberadas = Vec::new();
        for (nova, velhas) in self.aguardando.iter_mut() {
            velhas.remove(&id);
            if velhas.is_empty() {
                liberadas.push(*nova);
            }
        }
        for nova in &liberadas {
            self.aguardando.remove(nova);
        }
        // O cinto do Parar: com tudo desmontado, a tela volta — venha o último aviso de onde vier.
        if self.encerrando_tudo {
            if self.sessoes.is_empty() {
                self.voltar_ao_inicio(fx);
            }
            return;
        }
        for nova in liberadas {
            self.iniciar_transmissao(nova, agora_ms, fx);
        }
        self.depois_de_uma_sair(agora_ms, fx);
        self.publicar();
    }

    fn depois_de_uma_sair(&mut self, agora_ms: u64, fx: &mut impl Efeitos) {
        if self.encerrando_tudo || self.fase == Fase::Inicial {
            return;
        }
        let saindo = self.sessoes.iter().any(|s| s.estado == Estado::Encerrando);
        if self.vivas() == 0 && self.espera().is_none() && !saindo {
            self.voltar_ao_inicio(fx);
            return;
        }
        // Tinha batido no limite (ou a espera tinha falhado rápido): com uma vaga, a espera volta.
        if let Some(r) = self.rodada.clone() {
            if r.varias && self.espera().is_none() && self.vivas() < r.limite {
                if !self.abrir_espera(None, None, agora_ms, fx) {
                    fx.anunciar(None);
                }
            }
        }
    }

    /// Desconecta **um** receptor (a linha dele na tela). O monitor dele sai junto.
    pub fn desconectar(&mut self, id: Id, fx: &mut impl Efeitos) {
        if self.sessao(id).map(|s| s.estado) == Some(Estado::Transmitindo) {
            fx.registrar(format!("[#{id}] desconectado pela pessoa"));
            self.encerrar_uma(id, fx);
            self.publicar();
        }
    }

    fn encerrar_uma(&mut self, id: Id, fx: &mut impl Efeitos) {
        if let Some(i) = self.pos(id) {
            if self.sessoes[i].estado != Estado::Encerrando {
                self.sessoes[i].estado = Estado::Encerrando;
                fx.encerrar(id);
                // D2: o som passa **já**, e não no fim do desmonte: quem está saindo não toca mais
                // nada, e o próximo não precisa esperar a cadeia da velha desligar.
                self.som_saiu(id, fx);
            }
        }
    }

    /// O Parar. **Funciona de verdade**: todas as esperas destravam, todas as transmissões param, o
    /// anúncio sai do ar, e a tela volta quando a última sessão avisar que desmontou — ou quando
    /// [`PRAZO_DO_DESMONTE_MS`] vencer.
    pub fn encerrar(&mut self, fx: &mut impl Efeitos) {
        if !matches!(self.fase, Fase::Esperando | Fase::Transmitindo) {
            return;
        }
        self.encerrando_tudo = true;
        self.fase = Fase::Encerrando;
        fx.anunciar(None);
        self.aguardando.clear();
        if let Some(d) = self.dono_do_som.dono() {
            fx.dar_o_som(d, false);
        }
        self.dono_do_som.zerar();
        if self.sessoes.is_empty() {
            self.voltar_ao_inicio(fx);
            return;
        }
        let ids: Vec<Id> = self.sessoes.iter().map(|s| s.id).collect();
        for id in ids {
            self.encerrar_uma(id, fx);
        }
    }

    /// **D2, a sessão sem som** (crítica 13, M3): a sessão `id` avisou que não tem som — o endpoint
    /// não abriu e a track nem foi oferecida, ou a captura não subiu. Ela não toca, e se era a dona,
    /// o som passa na hora. O aviso pode chegar antes de ela conectar: a marca fica na sessão.
    pub fn sem_som(&mut self, id: Id, fx: &mut impl Efeitos) {
        let Some(i) = self.pos(id) else {
            return;
        };
        if !self.sessoes[i].som_de_pe {
            return;
        }
        self.sessoes[i].som_de_pe = false;
        if self.encerrando_tudo {
            return;
        }
        let antes = self.dono_do_som.dono();
        self.dono_do_som.nao_toca(id);
        self.aplicar_o_dono(antes, &format!("a sessão #{id} ficou sem som"), fx); // i18n: fora (diário)
    }

    /// Relógio: a carência do dono do som que caiu, e o prazo do desmonte.
    pub fn tique(&mut self, agora_ms: u64, fx: &mut impl Efeitos) {
        if !self.encerrando_tudo && self.dono_do_som.carencia().is_some() {
            let antes = self.dono_do_som.dono();
            self.dono_do_som.tique(agora_ms);
            self.aplicar_o_dono(antes, "a carência de 10 s do dono que caiu acabou", fx); // i18n: fora (diário)
        }
        if !self.encerrando_tudo {
            self.encerrando_desde_ms = None;
            return;
        }
        let desde = *self.encerrando_desde_ms.get_or_insert(agora_ms);
        if agora_ms.saturating_sub(desde) >= PRAZO_DO_DESMONTE_MS && !self.sessoes.is_empty() {
            let presas: Vec<String> = self.sessoes.iter().map(|s| format!("#{}", s.id)).collect();
            fx.registrar(format!(
                "!! {} não desmontaram em {} s — voltando ao começo sem elas", // i18n: fora (diário)
                presas.join(", "),
                PRAZO_DO_DESMONTE_MS / 1000
            ));
            self.voltar_ao_inicio(fx);
        }
    }

    fn voltar_ao_inicio(&mut self, fx: &mut impl Efeitos) {
        self.fase = Fase::Inicial;
        self.encerrando_tudo = false;
        self.encerrando_desde_ms = None;
        self.sessoes.clear();
        self.rodada = None;
        self.aguardando.clear();
        self.dono_do_som.zerar();
        // O anúncio sai aqui também: uma espera que falhou sem ninguém no ar deixaria o computador
        // na lista dos outros apontando para uma porta morta (revisão do Mac).
        fx.anunciar(None);
    }

    /// A fase que a interface mostra, derivada das sessões.
    fn publicar(&mut self) {
        if self.encerrando_tudo || (self.fase == Fase::Inicial && self.sessoes.is_empty()) {
            return;
        }
        if self.vivas() > 0 {
            self.fase = Fase::Transmitindo;
        } else if self.espera().is_some() {
            self.fase = Fase::Esperando;
        }
    }

    pub fn retrato(&self) -> Retrato {
        let espera = self.espera();
        let no_ar: Vec<&Sessao> =
            self.sessoes.iter().filter(|s| s.estado == Estado::Transmitindo).collect();
        Retrato {
            fase: self.fase,
            pin: espera.map(|s| s.pin.clone()).unwrap_or_default(),
            porta: espera.map(|s| s.porta).or_else(|| no_ar.first().map(|s| s.porta)),
            esperando_mais_um: !no_ar.is_empty() && espera.is_some(),
            receptores: no_ar
                .iter()
                .map(|s| ReceptorNoAr {
                    id: s.id,
                    nome: s.par.nome.clone(),
                    indice: s.indice,
                    tela: s.par.tela,
                    aguardando_a_velha: !s.ordem_dada,
                    com_o_som: self.dono_do_som.dono() == Some(s.id),
                })
                .collect(),
            conselho: self.conselho.clone(),
            oferece_desparear: self.oferece_desparear,
        }
    }
}

// **Os conselhos fixos ficam em português**, como chaves da tabela (`idioma/janela.rs`): a janela os
// traduz na hora de mostrar (`idioma::tr`), e a troca de idioma vale também para um conselho que já
// está na tela. Uma linha cada (a varredura da tradução lê o literal inteiro numa linha só).
const CONSELHO_PIN_ERRADO_REABRE: &str = "Um aparelho tentou entrar com o PIN errado. O PIN mudou: passe os seis dígitos novos — cada PIN vale uma tentativa por conexão."; // i18n: chave
const CONSELHO_PIN_ERRADO: &str = "Um aparelho tentou entrar com o PIN errado. Clique em Estender de novo e passe os seis dígitos novos — cada PIN vale uma tentativa por conexão."; // i18n: chave
const CONSELHO_PRECISA_DE_PIN: &str = "Um aparelho tentou entrar com um pareamento que este computador não reconhece mais. Peça para ele digitar o PIN de novo; se continuar falhando, esqueça os pareamentos e comecem do zero."; // i18n: chave
const CONSELHO_SEM_ROTA: &str = "O pareamento fechou, mas os dois aparelhos não acharam caminho um para o outro. Quase sempre é a rede: Wi-Fi de hóspede, isolamento entre aparelhos ou redes diferentes. Ponha os dois na mesma rede e tente de novo."; // i18n: chave
const CONSELHO_PRAZO: &str = "Ninguém entrou em cinco minutos. Clique em Estender de novo quando o outro aparelho estiver pronto."; // i18n: chave
const CONSELHO_PAREAMENTO_REABRE: &str = "O pareamento de mais um aparelho não fechou. Ou o PIN não conferiu, ou o outro aparelho tentou entrar com um pareamento que este computador não reconhece mais. O PIN mudou: digite no outro aparelho o PIN novo que aparece aqui."; // i18n: chave
const CONSELHO_PAREAMENTO: &str = "O pareamento não fechou. Ou o PIN não conferiu, ou o outro aparelho tentou entrar com um pareamento que este computador não reconhece mais. Nos dois casos a saída é a mesma: clique em Estender de novo e digite no outro aparelho o PIN novo que aparecer aqui."; // i18n: chave

/// Os conselhos de quando a espera falha — os textos de `emissor.rs::ao_falhar`, e os do Mac para
/// a espera de mais um aparelho (`reabre`): ela volta sozinha, e o conselho não pode mandar a
/// pessoa clicar em Espelhar.
///
/// **Duplicados de propósito**: o caminho de uma sessão só (`emissor.rs`) é o produto de hoje e não
/// foi tocado por esta frente. Quando os dois caminhos virarem um, os textos viram um.
pub fn conselho_da_falha(falha: &Falha, reabre: bool) -> (String, bool) {
    match falha {
        Falha::Cancelada => (String::new(), false),
        // Os textos do Mac para `QUALL_STATUS_WRONG_PIN` (`Emissor.conselho(para:)`).
        Falha::PinErrado => (if reabre { CONSELHO_PIN_ERRADO_REABRE } else { CONSELHO_PIN_ERRADO }.into(), false),
        Falha::PrecisaDePin => (CONSELHO_PRECISA_DE_PIN.into(), true),
        Falha::SemRota => (CONSELHO_SEM_ROTA.into(), false),
        Falha::Prazo => (if reabre { String::new() } else { CONSELHO_PRAZO.into() }, false),
        Falha::Pareamento => (if reabre { CONSELHO_PAREAMENTO_REABRE } else { CONSELHO_PAREAMENTO }.into(), false),
        Falha::Outra(t) => (t.clone(), false),
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Efeitos de mentira: grava o que foi pedido, sorteia porta e PIN previsíveis.
    #[derive(Default)]
    struct Registro {
        abertas: Vec<(Id, u16, String, bool)>,
        anuncios: Vec<Option<u16>>,
        /// D2: a ordem dos `dar_o_som`.
        som: Vec<(Id, bool)>,
        transmitir: Vec<(Id, usize)>,
        encerrar: Vec<Id>,
        linhas: Vec<String>,
        gravacoes: usize,
        proxima_porta: u16,
        falhar_proxima_abertura: bool,
    }

    impl Efeitos for Registro {
        fn abrir_espera(
            &mut self,
            id: Id,
            porta: u16,
            pin: Option<&str>,
            com_audio: bool,
        ) -> Result<(u16, String), String> {
            if self.falhar_proxima_abertura {
                self.falhar_proxima_abertura = false;
                return Err("sem porta".into());
            }
            let porta = if porta != 0 {
                porta
            } else {
                self.proxima_porta += 1;
                50_000 + self.proxima_porta
            };
            let pin = pin.map(str::to_string).unwrap_or_else(|| format!("{:06}", 100_000 + id));
            self.abertas.push((id, porta, pin.clone(), com_audio));
            Ok((porta, pin))
        }
        fn anunciar(&mut self, porta: Option<u16>) {
            self.anuncios.push(porta);
        }
        fn transmitir(&mut self, id: Id, indice: usize, _par: &Par) {
            self.transmitir.push((id, indice));
        }
        fn encerrar(&mut self, id: Id) {
            self.encerrar.push(id);
        }
        fn dar_o_som(&mut self, id: Id, mandar: bool) {
            self.som.push((id, mandar));
        }
        fn gravar_indices(&mut self, _t: &TabelaDeIndices) {
            self.gravacoes += 1;
        }
        fn registrar(&mut self, linha: String) {
            self.linhas.push(linha);
        }
    }

    fn rodada(varias: bool) -> Rodada {
        Rodada { varias, limite: LIMITE_DE_SESSOES, com_som: false, passar_som: true, porta: 0, pin: None }
    }

    fn par(id: &str) -> Par {
        Par { nome: format!("aparelho {id}"), device_id: id.into(), tela: Some((1920, 1200)) }
    }

    /// A espera aberta agora.
    fn espera(t: &Tabela) -> Id {
        t.sessoes().iter().find(|s| s.estado == Estado::Esperando).expect("espera aberta").id
    }

    #[test]
    fn cada_espera_tem_porta_e_pin_novos_e_o_anuncio_segue_a_espera() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        assert!(t.espelhar(rodada(true), 0, &mut fx));
        for i in 0..3 {
            let e = espera(&t);
            t.conectou(e, par(&format!("a{i}")), 1_000, &mut fx);
        }
        let portas: BTreeSet<u16> = fx.abertas.iter().map(|a| a.1).collect();
        let pins: BTreeSet<String> = fx.abertas.iter().map(|a| a.2.clone()).collect();
        assert_eq!(fx.abertas.len(), 4, "a primeira e mais uma por receptor");
        assert_eq!(portas.len(), 4);
        assert_eq!(pins.len(), 4);
        assert_eq!(fx.anuncios.last().copied().flatten(), Some(fx.abertas[3].1));
        let r = t.retrato();
        assert_eq!(r.fase, Fase::Transmitindo);
        assert!(r.esperando_mais_um);
        assert_eq!(r.receptores.len(), 3);
        assert_eq!(r.pin, fx.abertas[3].2);
    }

    #[test]
    fn limite_de_oito_e_a_espera_volta_quando_abre_vaga() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let mut ids = Vec::new();
        for i in 0..LIMITE_DE_SESSOES {
            let e = espera(&t);
            ids.push(e);
            t.conectou(e, par(&format!("a{i}")), 1_000, &mut fx);
        }
        assert_eq!(fx.transmitir.len(), 8);
        let indices: BTreeSet<_> = fx.transmitir.iter().map(|(_, indice)| *indice).collect();
        assert_eq!(indices, (0..8).collect(), "cada aparelho captura seu próprio monitor");
        assert!(t.sessoes().iter().all(|s| s.estado == Estado::Transmitindo), "nenhuma espera");
        assert_eq!(fx.anuncios.last(), Some(&None), "no limite o anúncio sai do ar");
        assert!(!t.retrato().esperando_mais_um);
        assert!(t.retrato().pin.is_empty());
        // Um sai: a espera volta.
        t.saiu(ids[3], Saida::Saiu, 2_000, &mut fx);
        assert_eq!(fx.encerrar, vec![ids[3]]);
        t.desmontada(ids[3], 5_000, &mut fx);
        assert!(t.sessoes().iter().any(|s| s.estado == Estado::Esperando));
        assert_eq!(t.retrato().receptores.len(), 7);
        assert!(t.retrato().esperando_mais_um);
        // The returning device gets its old display index without ending the other seven.
        let volta = espera(&t);
        t.conectou(volta, par("a3"), 6_000, &mut fx);
        assert_eq!(fx.transmitir.last(), Some(&(volta, 3)));
        assert_eq!(fx.encerrar, vec![ids[3]], "as outras sete sessões continuam");
        assert_eq!(t.retrato().receptores.len(), 8);
        assert!(!t.retrato().esperando_mais_um, "nono aparelho não ganha uma espera");
    }

    #[test]
    fn indice_por_aparelho_gravado_e_devolvido_quando_ele_volta() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("iphone"), 2_000, &mut fx);
        assert_eq!(fx.transmitir, vec![(a, 0), (b, 1)]);
        t.saiu(a, Saida::Saiu, 2_000, &mut fx);
        t.desmontada(a, 3_000, &mut fx);
        // Um aparelho novo não toma o 0 do tablet.
        let c = espera(&t);
        t.conectou(c, par("s24"), 4_000, &mut fx);
        assert_eq!(fx.transmitir.last(), Some(&(c, 2)));
        // O tablet volta: o 0 é dele.
        let d = espera(&t);
        t.conectou(d, par("tablet"), 5_000, &mut fx);
        assert_eq!(fx.transmitir.last(), Some(&(d, 0)));
        assert_eq!(t.indices().gravado("tablet"), Some(0));
        assert!(fx.gravacoes >= 4);
    }

    #[test]
    fn o_mesmo_aparelho_so_transmite_depois_de_a_velha_desmontar_e_herda_o_indice() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let velha = espera(&t);
        t.conectou(velha, par("tablet"), 1_000, &mut fx);
        let outra = espera(&t);
        t.conectou(outra, par("iphone"), 1_500, &mut fx);
        let nova = espera(&t);
        let esperas_antes = fx.abertas.len();
        // O Wi-Fi piscou: o tablet volta pela espera aberta antes de a sessão velha cair.
        t.conectou(nova, par("tablet"), 2_000, &mut fx);
        assert_eq!(fx.encerrar, vec![velha], "a velha sai antes");
        assert!(!fx.transmitir.iter().any(|(id, _)| *id == nova), "a nova ainda não transmite");
        assert!(t.retrato().receptores.iter().any(|r| r.id == nova && r.aguardando_a_velha));
        // Enquanto a velha desmonta, o índice 0 continua em uso — nenhuma outra o toma.
        t.desmontada(velha, 3_000, &mut fx);
        assert_eq!(fx.transmitir.last(), Some(&(nova, 0)), "herda o índice do aparelho");
        assert_eq!(fx.abertas.len(), esperas_antes + 1, "e só então a espera seguinte abre");
        assert_eq!(t.retrato().receptores.len(), 2);
    }

    #[test]
    fn espera_extra_que_falha_e_reaberta_sem_derrubar_quem_esta_no_ar() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        let (e, porta, pin, _) = fx.abertas.last().cloned().unwrap();
        // PIN errado dez segundos depois: reabre na mesma porta, com PIN novo.
        t.falhou_ao_hospedar(e, Falha::PinErrado, 11_000, &mut fx);
        assert!(fx.encerrar.is_empty(), "ninguém no ar foi derrubado");
        let (e2, porta2, pin2, _) = fx.abertas.last().cloned().unwrap();
        assert_ne!(e2, e);
        assert_eq!(porta2, porta);
        assert_ne!(pin2, pin);
        assert!(t.retrato().conselho.contains("O PIN mudou"));
        assert_eq!(t.retrato().fase, Fase::Transmitindo);
        // O prazo de cinco minutos venceu: renova com o mesmo PIN, sem conselho.
        t.limpar_conselho();
        t.falhou_ao_hospedar(e2, Falha::Prazo, 400_000, &mut fx);
        let (_, porta3, pin3, _) = fx.abertas.last().cloned().unwrap();
        assert_eq!((porta3, pin3), (porta2, pin2));
        assert!(t.retrato().conselho.is_empty());
    }

    #[test]
    fn espera_que_falha_em_menos_de_3_s_nao_reabre_ate_abrir_vaga() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("iphone"), 1_000, &mut fx);
        let e = espera(&t);
        let n = fx.abertas.len();
        t.falhou_ao_hospedar(e, Falha::SemRota, 2_000, &mut fx);
        assert_eq!(fx.abertas.len(), n, "não reabre");
        assert_eq!(fx.anuncios.last(), Some(&None));
        assert_eq!(t.retrato().fase, Fase::Transmitindo);
        // O iPhone sai: com uma vaga e nenhuma espera, a espera volta.
        t.saiu(b, Saida::Caiu, 2_000, &mut fx);
        t.desmontada(b, 10_000, &mut fx);
        assert_eq!(fx.abertas.len(), n + 1);
    }

    #[test]
    fn d2_toda_sessao_oferece_o_som_e_so_a_dona_manda() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        t.espelhar(r, 0, &mut fx);
        // O OBS primeiro: não toca, não é dono (M3).
        let obs = espera(&t);
        t.conectou(obs, par("9e7b34b1-obs"), 1_000, &mut fx);
        assert_eq!(t.dona_do_som(), None);
        let a = espera(&t);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("ios-iphone"), 1_000, &mut fx);
        let com_som: Vec<bool> = fx.abertas.iter().map(|x| x.3).collect();
        assert_eq!(com_som, vec![true, true, true, true], "toda espera oferece a track");
        assert_eq!(t.dona_do_som(), Some(a), "o primeiro que toca");
        assert_eq!(fx.som, vec![(a, true)]);
        let r = t.retrato();
        assert_eq!(r.receptores.iter().filter(|x| x.com_o_som).map(|x| x.id).collect::<Vec<_>>(), vec![a]);
        // A dona sai: o som passa **já** ao próximo que toca, sem esperar o desmonte.
        t.saiu(a, Saida::Saiu, 2_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(b));
        assert_eq!(fx.som, vec![(a, true), (a, false), (b, true)]);
        t.desmontada(a, 2_000, &mut fx);
        assert_eq!(fx.som.len(), 3, "o desmonte da velha não mexe no som de novo");
        // Uma sessão nova depois disso oferece e cala.
        let c = espera(&t);
        t.conectou(c, par("mac-3916"), 3_000, &mut fx);
        assert_eq!(fx.abertas.last().map(|x| x.3), Some(true));
        assert_eq!(t.dona_do_som(), Some(b));
    }

    #[test]
    fn d2_sem_passar_o_som_fica_com_ninguem_ate_chegar_alguem() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        r.passar_som = false;
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("win-dell"), 1_000, &mut fx);
        t.desconectar(a, &mut fx);
        assert_eq!(t.dona_do_som(), None, "b já estava conectado e calado");
        assert_eq!(fx.som, vec![(a, true), (a, false)]);
        let c = espera(&t);
        t.conectou(c, par("mac-3916"), 2_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(c), "quem chega sem dono no ar vira dono");
    }

    #[test]
    fn d2_o_dono_que_reconecta_leva_o_som_antes_de_a_velha_sair() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("mac-3916"), 1_000, &mut fx);
        // O tablet volta por outra espera antes de a velha cair.
        let a2 = espera(&t);
        t.conectou(a2, par("android-tablet"), 2_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(a2), "a nova herda o som, e não o Mac");
        assert_eq!(fx.som, vec![(a, true), (a, false), (a2, true)]);
        assert!(fx.encerrar.contains(&a));
        t.desmontada(a, 3_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(a2));
        assert_eq!(fx.som.len(), 3);
    }

    /// **O M2 da crítica 13, na ordem que o derrubava**: o emissor vê a queda do dono **antes** de
    /// ele voltar. Com a carência de 10 s (decisão do Bruno de 18/09), o som espera o aparelho, o
    /// segundo receptor não toca, e a sessão nova do mesmo aparelho pega o som de volta.
    #[test]
    fn d2_o_dono_que_cai_antes_de_voltar_mantem_o_som() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("mac-3916"), 1_000, &mut fx);
        t.saiu(a, Saida::Caiu, 2_000, &mut fx);
        assert_eq!(t.dona_do_som(), None, "nos 10 s ninguém toca");
        assert_eq!(fx.som, vec![(a, true), (a, false)], "o Mac não pega o som na queda");
        let a2 = espera(&t);
        t.conectou(a2, par("android-tablet"), 5_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(a2), "o tablet voltou em 3 s e o som é dele");
        assert_eq!(fx.som, vec![(a, true), (a, false), (a2, true)]);
        t.desmontada(a, 6_000, &mut fx);
        t.tique(20_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(a2));
        assert_eq!(fx.som.len(), 3);
    }

    #[test]
    fn d2_o_dono_que_cai_e_nao_volta_passa_o_som_em_10_s() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("mac-3916"), 1_000, &mut fx);
        t.saiu(a, Saida::Caiu, 2_000, &mut fx);
        t.desmontada(a, 2_500, &mut fx);
        t.tique(11_999, &mut fx);
        assert_eq!(t.dona_do_som(), None, "ainda na carência");
        t.tique(12_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(b), "10 s depois da queda, o som passa");
        assert_eq!(fx.som, vec![(a, true), (a, false), (b, true)]);
    }

    #[test]
    fn d2_quem_sai_ou_e_desconectado_passa_o_som_na_hora() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("mac-3916"), 1_000, &mut fx);
        t.saiu(a, Saida::Saiu, 2_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(b), "o receptor fechou: sem carência");
        let c = espera(&t);
        t.conectou(c, par("ios-iphone"), 3_000, &mut fx);
        t.desconectar(b, &mut fx);
        assert_eq!(t.dona_do_som(), Some(c), "a pessoa desconectou: sem carência");
    }

    /// **O M3 da crítica 13**: a sessão que ficou sem som não segura o som — nem a que avisou antes
    /// de conectar (o endpoint não abriu), nem a dona cuja captura caiu depois.
    #[test]
    fn d2_a_sessao_sem_som_nao_segura_o_som() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.sem_som(a, &mut fx);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        assert_eq!(t.dona_do_som(), None, "a sessão sem track de som não é dona");
        let b = espera(&t);
        t.conectou(b, par("mac-3916"), 1_000, &mut fx);
        assert_eq!(t.dona_do_som(), Some(b));
        let c = espera(&t);
        t.conectou(c, par("ios-iphone"), 1_000, &mut fx);
        t.sem_som(b, &mut fx);
        assert_eq!(t.dona_do_som(), Some(c), "a dona que perdeu a captura passa o som na hora");
        assert!(!t.retrato().receptores.iter().any(|x| x.id == b && x.com_o_som));
    }

    #[test]
    fn d2_o_parar_tira_o_som_de_todos_uma_vez() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.com_som = true;
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("android-tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("mac-3916"), 1_000, &mut fx);
        let e = espera(&t);
        t.encerrar(&mut fx);
        assert_eq!(fx.som, vec![(a, true), (a, false)], "sem passar o som a quem também está saindo");
        t.falhou_ao_hospedar(e, Falha::Cancelada, 2_000, &mut fx);
        t.desmontada(a, 2_000, &mut fx);
        t.desmontada(b, 2_000, &mut fx);
        assert_eq!(t.fase(), Fase::Inicial);
        assert_eq!(t.dona_do_som(), None);
    }

    #[test]
    fn parar_durante_um_desmonte_nao_deixa_o_app_preso() {
        for desmonta_antes_do_parar in [false, true] {
            let mut t = Tabela::nova(TabelaDeIndices::nova());
            let mut fx = Registro::default();
            t.espelhar(rodada(true), 0, &mut fx);
            let a = espera(&t);
            t.conectou(a, par("tablet"), 1_000, &mut fx);
            let b = espera(&t);
            t.conectou(b, par("iphone"), 1_000, &mut fx);
            let e = espera(&t);
            t.desconectar(a, &mut fx);
            if desmonta_antes_do_parar {
                t.desmontada(a, 2_000, &mut fx);
            }
            t.encerrar(&mut fx);
            assert_eq!(t.fase(), Fase::Encerrando);
            // O desmonte de "a" já corria: nenhuma ordem repetida para ele.
            assert_eq!(fx.encerrar.iter().filter(|id| **id == a).count(), 1);
            t.falhou_ao_hospedar(e, Falha::Cancelada, 2_100, &mut fx);
            if !desmonta_antes_do_parar {
                t.desmontada(a, 2_200, &mut fx);
            }
            assert_eq!(t.fase(), Fase::Encerrando);
            t.desmontada(b, 2_300, &mut fx);
            assert_eq!(t.fase(), Fase::Inicial, "com tudo desmontado a tela volta");
            assert_eq!(fx.anuncios.last(), Some(&None));
        }
    }

    #[test]
    fn desmonte_que_nao_termina_nao_prende_para_sempre() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        t.encerrar(&mut fx);
        t.tique(10_000, &mut fx);
        t.tique(10_000 + PRAZO_DO_DESMONTE_MS - 1, &mut fx);
        assert_eq!(t.fase(), Fase::Encerrando);
        t.tique(10_000 + PRAZO_DO_DESMONTE_MS, &mut fx);
        assert_eq!(t.fase(), Fase::Inicial);
        // O aviso atrasado da sessão presa é de outra rodada.
        t.desmontada(a, 60_000, &mut fx);
        assert_eq!(t.fase(), Fase::Inicial);
        assert!(t.espelhar(rodada(true), 61_000, &mut fx));
    }

    #[test]
    fn aviso_de_sessao_de_outra_rodada_e_fechado_e_ignorado() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let velha = espera(&t);
        t.encerrar(&mut fx);
        t.falhou_ao_hospedar(velha, Falha::Cancelada, 100, &mut fx);
        assert_eq!(t.fase(), Fase::Inicial);
        t.espelhar(rodada(true), 200, &mut fx);
        let nova = espera(&t);
        assert_ne!(nova, velha, "o número não se repete entre rodadas");
        // A velha tinha pareado no instante do Parar: é fechada, e não entra na lista.
        t.conectou(velha, par("tablet"), 300, &mut fx);
        assert_eq!(fx.encerrar.last(), Some(&velha));
        assert!(t.sessao(velha).is_none());
        t.desmontada(velha, 400, &mut fx);
        assert_eq!(t.fase(), Fase::Esperando);
    }

    #[test]
    fn a_captura_presa_mostra_a_frase_do_usuario_literal_e_nao_derruba_as_outras() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        let b = espera(&t);
        t.conectou(b, par("s24"), 2_000, &mut fx);
        t.saiu(b, Saida::CapturaPresa, 2_000, &mut fx);
        assert_eq!(t.retrato().conselho, "A captura do Windows não respondeu. Reiniciar o Windows costuma resolver.");
        assert_eq!(t.retrato().conselho, AVISO_DA_CAPTURA_PRESA);
        assert!(t.sessao(a).is_some_and(|s| s.estado == Estado::Transmitindo), "a outra sessão segue no ar");
    }

    #[test]
    fn sem_varias_e_o_produto_de_hoje_uma_espera_e_a_saida_encerra_tudo() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(false), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        assert_eq!(fx.abertas.len(), 1, "nenhuma espera seguinte");
        assert_eq!(fx.anuncios.last(), Some(&None), "parou de anunciar quando conectou");
        t.saiu(a, Saida::Saiu, 2_000, &mut fx);
        assert_eq!(t.fase(), Fase::Encerrando);
        t.desmontada(a, 2_000, &mut fx);
        assert_eq!(t.fase(), Fase::Inicial);
        assert_eq!(t.retrato().conselho, "aparelho tablet saiu.");
    }

    #[test]
    fn espera_que_falha_sem_ninguem_no_ar_volta_ao_inicio_com_conselho() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let e = espera(&t);
        t.falhou_ao_hospedar(e, Falha::Prazo, 300_000, &mut fx);
        assert_eq!(t.fase(), Fase::Inicial);
        assert!(t.retrato().conselho.contains("cinco minutos"));
        assert_eq!(fx.anuncios.last(), Some(&None), "o anúncio não fica apontando para porta morta");
    }

    #[test]
    fn espera_seguinte_que_nao_abre_tira_o_anuncio_e_nao_derruba_ninguem() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        fx.falhar_proxima_abertura = true;
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        assert_eq!(fx.transmitir, vec![(a, 0)]);
        assert_eq!(fx.anuncios.last(), Some(&None));
        assert!(fx.encerrar.is_empty());
        assert_eq!(t.fase(), Fase::Transmitindo);
    }

    #[test]
    fn pin_errado_sem_ninguem_no_ar_volta_ao_inicio_com_o_conselho_do_pin() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let e = espera(&t);
        t.falhou_ao_hospedar(e, Falha::PinErrado, 10_000, &mut fx);
        assert_eq!(t.fase(), Fase::Inicial);
        assert!(t.retrato().conselho.contains("PIN errado"));
        assert!(t.retrato().conselho.contains("Clique em Estender"));
    }

    #[test]
    fn a_espera_cancelada_nunca_conta_como_conectada() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        let e = espera(&t);
        t.encerrar(&mut fx);
        assert!(t.sessao(a).unwrap().conectou);
        assert!(!t.sessao(e).unwrap().conectou, "a espera não pareou: o cão de guarda não a vigia");
    }

    #[test]
    fn monitor_que_nao_confirma_a_saida_deixa_o_indice_em_quarentena() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        t.espelhar(rodada(true), 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        t.saiu(a, Saida::Caiu, 2_000, &mut fx);
        t.monitor_nao_soltou(a, &mut fx);
        t.desmontada(a, 2_000, &mut fx);
        // O tablet volta: o 0 dele está preso, então ele ganha outro, e ninguém ganha o 0.
        let b = espera(&t);
        t.conectou(b, par("tablet"), 3_000, &mut fx);
        assert_eq!(fx.transmitir.last(), Some(&(b, 1)));
        let c = espera(&t);
        t.conectou(c, par("iphone"), 4_000, &mut fx);
        assert_ne!(fx.transmitir.last().map(|x| x.1), Some(0));
    }

    #[test]
    fn a_primeira_espera_usa_a_porta_pedida_e_as_seguintes_uma_livre() {
        let mut t = Tabela::nova(TabelaDeIndices::nova());
        let mut fx = Registro::default();
        let mut r = rodada(true);
        r.porta = 7959;
        r.pin = Some("141421".into());
        t.espelhar(r, 0, &mut fx);
        let a = espera(&t);
        t.conectou(a, par("tablet"), 1_000, &mut fx);
        assert_eq!(fx.abertas[0].1, 7959);
        assert_ne!(fx.abertas[1].1, 7959);
        // O PIN de bancada vale para todas as esperas.
        assert!(fx.abertas.iter().all(|x| x.2 == "141421"));
    }
}
