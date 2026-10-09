//! A máquina de estados do emissor Windows.
//!
//! # Quem espelha anuncia e espera
//!
//! Dívida 1: `tracks` só vale em `hospedar`, não há renegociação, e quem chama `conectar` nunca
//! poderá emitir naquela sessão. **Não é preferência de interface, é imposto pelo protocolo.** A
//! consequência para este módulo é que não existe — e não pode existir — um método "mandar para
//! aquele aparelho ali". Existe [`Emissor::espelhar`], que sobe uma sessão e **espera**.
//!
//! O conserto disso na interface é discurso, não mecanismo: a tela nunca diz "escolha para onde
//! mandar", diz "é assim que os outros te encontram".
//!
//! # Uma thread, e o `Ready` mora só nela
//!
//! `docs/divida-do-nucleo.md` fixa uma regra de plataforma que é do Windows e de mais ninguém: a
//! exceção da libdatachannel sobe de dentro do `lock_guard` do mutex global **sem soltá-lo**, e a
//! chamada seguinte trava o processo para sempre. A tradução prática é simples e verificável: o
//! `Ready` (e portanto a `Session`, o `Link` e as tracks) vive numa variável só, na thread da
//! sessão, e nenhuma outra parte do programa guarda cópia. A interface fala com esta thread por
//! duas coisas que não são handles do núcleo: um sinalizador booleano e o [`Cancelamento`].
//!
//! É por isso que o laço de captura roda **nesta mesma thread** em vez de numa terceira: um laço
//! separado precisaria de uma referência à track.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use quall_core::cancel::Cancelamento;
use quall_core::discovery::{anuncio, Advertiser};
use quall_core::error::Error;
use quall_core::pairing::{PairedPeers, Pin};
use quall_core::protocol::Capabilities;
use quall_core::session::{hospedar, SessionConfig};
use quall_core::signaling::SignalingServer;
use quall_core::track::{TrackConfig, TrackKind};
use quall_core::transport::TransportConfig;

use crate::argumentos::Argumentos;
use crate::audio::{self, CadeiaDeAudio};
use crate::catalogo_de_cameras::Reescolha;
use crate::enderecos;
use crate::fontes::{self, Fonte};
use crate::identidade;
use crate::idioma;
use crate::registro;
use crate::sessao_de_emissao::{ContextoDoLaco, Laco, VigiaDoMonitor};
use crate::transmissao::Cadeia;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fase {
    Inicial,
    Esperando,
    Transmitindo,
    Encerrando,
}

/// Tudo o que a janela lê. A janela **nunca** olha o disco nem o núcleo: lê daqui.
///
/// `docs/ux-m6.md` §1.2, terceira regra: *"a manchete é publicada, não lida do disco a cada
/// redesenho"*. Ler `pares.json` dentro do `WM_PAINT` seria caro e, pior, poderia mudar de valor
/// no meio de um desenho — a manchete piscaria entre as duas formas.
pub struct Estado {
    pub fase: Fase,
    /// Sobe a cada mudança. A janela só redesenha quando ela muda.
    pub versao: u64,
    pub nome_do_aparelho: String,
    pub fontes: Vec<Fonte>,
    /// **A tela estendida sem o driver** (R10, 02/10): o adaptador do SudoVDA não está presente, e o
    /// seletor mostra o ladrilho "Tela estendida" apagado, **fora** de `fontes` (não se escolhe; o
    /// clique abre as instruções). Relido a cada enumeração do seletor
    /// (`regras_da_tela_estendida::no_seletor`).
    pub tela_estendida_sem_driver: bool,
    /// Sobe toda vez que a **lista** de monitores muda de conteúdo. A janela remonta o seletor
    /// quando ela muda; `versao` sozinha não serve, porque ela sobe a cada redesenho e remontar a
    /// lista a cada 100 ms fecharia a lista suspensa na cara de quem a abriu.
    pub revisao_das_fontes: u64,
    /// A geração da última lista aplicada (a revisão curta do `08af2cd`, A2): cada enumeração toma
    /// uma de `GERACAO_DAS_LISTAS` antes de começar, e só a mais nova que esta entra. A da abertura
    /// é a 0.
    pub lista_aplicada: u64,
    /// Um pedido de recarga chegou fora da tela inicial (a revisão do `ef71d20`, L2): a volta ao
    /// início o desliga **antes** de enumerar e, se ele voltou a ligar enquanto ela enumerava,
    /// enumera de novo depois de aplicar. Sem isto, a câmera que entrasse durante a enumeração da
    /// volta ficava fora do seletor até o evento seguinte.
    pub recarga_adiada: bool,
    /// A linha escolhida no seletor. **`None` é "nenhuma"**, e é o estado depois de a fonte
    /// escolhida sumir da lista: a escolha nunca cai sozinha para outra linha, porque a primeira é
    /// o monitor principal (`catalogo_de_cameras::reescolher`).
    pub escolhida: Option<usize>,
    /// O aviso da fonte que sumiu, **como foi posto** no `conselho`. Guardado à parte para sair
    /// sozinho: o conselho pode trazer junto o motivo do fim de uma sessão, e esse fica (a
    /// reconferência de 18/09). Sai quando a pessoa escolhe outra, ou quando a mesma volta.
    pub aviso_da_fonte: Option<String>,
    /// A fonte escolhida que saiu da lista (id e nome), enquanto a pessoa não escolher outra. Se a
    /// **mesma** voltar — o mesmo id **e** o mesmo nome —, ela volta escolhida
    /// (`catalogo_de_cameras::Reescolha::Voltou`): a TV que desliga e religa na tela inicial, ou no
    /// meio da sessão, volta a ser o que o Espelhar transmite, como no `main` (a revisão de código de
    /// 18/09, M1). O nome junto porque o id de monitor é da saída de vídeo (a reconferência).
    pub sumida: Option<(String, String)>,
    pub pin: String,
    pub endereco: Option<String>,
    pub ha_pares_conhecidos: bool,
    /// A caixa "Transmitir o som deste computador", como a pessoa a deixou. É **intenção**, não
    /// resultado.
    pub com_som: bool,
    /// O que de fato aconteceu com o som nesta sessão. Separado de [`Estado::com_som`] de
    /// propósito: a caixa marcada e o WASAPI recusando o endpoint são estados diferentes, e a tela
    /// de espera tem de contar o segundo, não repetir o primeiro.
    pub som_ativo: bool,
    /// Por que não houve som, quando não houve. Vazio quando houve ou quando ninguém pediu.
    pub som_recusado: String,
    pub par: String,
    pub conselho: String,
    pub oferece_desparear: bool,
    pub anunciando_por_mdns: bool,
    pub resumo: String,
    /// Corrida de bancada pediu para o processo sair.
    pub sair: bool,
    /// **Os receptores no ar**, com `--varias-sessoes`: um por linha na tela. Vazio no caminho de
    /// uma sessão só, que continua com `par` e `resumo`.
    pub receptores: Vec<ReceptorNaTela>,
    /// Transmitindo **e** com uma espera aberta para mais um aparelho (`pin` e `endereco` são os
    /// dela).
    pub esperando_mais_um: bool,
    /// A porta da espera aberta, crua — o que a bancada precisa para conectar um receptor por
    /// `127.0.0.1` sem ler o endereço de LAN que `endereco` mostra.
    pub porta_da_espera: Option<u16>,
    /// **A câmera comum pelo dono** (§8.10.3): a câmera está aberta com a espera, e o botão Gravar
    /// aparece.
    pub camera_pelo_dono: bool,
    pub gravando: bool,
    pub rotulo_gravar: String,
    /// "● GRAVANDO 1:23 · sobram 12,3 GB · Gravando SEM SOM — ligue o microfone", ou o recado.
    pub linha_da_gravacao: String,
}

/// Uma linha da lista de receptores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceptorNaTela {
    pub id: crate::sessoes::Id,
    pub nome: String,
    /// "2436 × 1124, índice 1" — o monitor desta sessão.
    pub monitor: String,
    pub resumo: String,
}

impl Estado {
    fn mudou(&mut self) {
        self.versao += 1;
    }
}

pub struct Emissor {
    estado: Mutex<Estado>,
    /// O botão Cancelar de verdade — `accept_cancelavel` por dentro. Sem ele a casca teria de
    /// abrir uma conexão descartável contra a própria porta para se destravar.
    cancelamento: Mutex<Option<Cancelamento>>,
    parar: AtomicBool,
    pub argumentos: Argumentos,
    ja_espelhou_sozinho: AtomicBool,
    /// O `--espelhar-ja` sem escolha já foi dito no registro: com `--repetir-espelhar` ele é tentado
    /// a cada volta, e a linha sairia a cada 100 ms (a revisão de código de 18/09, m2). Volta a
    /// valer depois de um Espelhar com escolha.
    avisou_sem_escolha: AtomicBool,
    /// Quantas sessões este **processo** já abriu. O número que separa "funcionou" de "funcionou
    /// duas vezes seguidas sem reiniciar".
    sessoes: std::sync::atomic::AtomicU64,
    /// O coordenador das várias sessões (`varias.rs`), com `--varias-sessoes`. Nasce no primeiro
    /// Espelhar, porque precisa do `Arc` deste emissor para publicar o estado.
    ///
    /// **Com a bandeira, `parar` e `cancelamento` acima não são usados**: cada sessão tem os seus,
    /// e a fase do app sai da tabela de sessões — o fim de uma sessão não toca nas outras (revisão
    /// adversarial de 13/09/2026, item 1).
    varias: std::sync::OnceLock<crate::varias::Coordenador>,
    /// **O coordenador é dono da tela** (R10, 02/10): a sessão em curso, ou a última, foi espelhada
    /// por ele. Com `--varias-sessoes` é sempre `true`, como antes; sem a bandeira, vira `true` no
    /// Espelhar da "Tela estendida" e `false` no Espelhar de qualquer outra fonte
    /// (`regras_da_tela_estendida::usa_o_coordenador`).
    ///
    /// O coordenador nasce uma vez e vive até o processo sair (o `OnceLock` acima), e o `publicar`
    /// dele reescreve a fase, o PIN e o conselho a cada 250 ms: **sem esta guarda, uma sessão só
    /// depois de uma tela estendida teria a fase devolvida a "inicial" pelo coordenador ocioso**, e
    /// o Parar, o conselho e o Desconectar iriam para ele. Trocada e lida sempre com o estado
    /// travado (`espelhar` e `varias::publicar`), para nenhum `publicar` passar entre a troca e a
    /// primeira escrita do caminho de uma sessão.
    coordenador_na_tela: AtomicBool,
    /// O botão do microfone da câmera comum (começa desligado).
    microfone_pedido: AtomicBool,
    /// O microfone da sessão de câmera no ar (nasce com a sessão, no zero de relógio dela).
    microfone: Mutex<Option<Arc<crate::microfone::Microfone>>>,
    /// A câmera comum pelo dono (§8.10.3), enquanto a espera existe.
    camera_comum: Mutex<Option<CameraComum>>,
    /// A thread dela: o `main` espera por ela, e não pelo campo acima (a revisão do código, 5).
    thread_da_camera_comum: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// **Os ajustes da câmera comum existem** (R9 §4.1): a câmera pelo link abriu, e não é a do
    /// próprio Quall. Escrito pela thread da câmera comum; a janela lê sem tomar cadeado nenhum.
    ajustes_disponiveis: AtomicBool,
    /// **R9b**: o aparelho que mexeu na câmera comum de longe, nos 4 s depois ("Controlado por").
    /// Escrito pela thread da câmera comum, do painel dos ajustes.
    controlada_por: Mutex<Option<String>>,
    /// **Pouca luz** (§3.1): o automático da câmera comum baixou o fps para clarear. Escrito pela
    /// thread da câmera comum, do painel dos ajustes, como o "Controlado por".
    pouca_luz: Mutex<Option<crate::regras_dos_controles::PoucaLuz>>,
}

impl Emissor {
    pub fn novo(argumentos: Argumentos) -> Arc<Self> {
        let nome = identidade::nome_do_aparelho();
        // **A câmera de bancada** (§7.3): com `--fonte` igual ao link liberado, a abertura espera
        // o link aparecer na enumeração antes de escolher, até 10 s, e não escolhe nada se ele não
        // aparecer (o `--fonte` sem casamento sai com código 2, como sempre).
        let ListaDoSeletor { fontes: lista, tela_estendida_sem_driver } = esperar_a_camera_de_bancada(&argumentos);
        registro::linha(format!(
            "fontes (monitores{}): {} — {}",
            if argumentos.com_cameras() { " e câmeras" } else { "" },
            lista.len(),
            lista
                .iter()
                .map(|f| format!("camera={} {}x{}", f.e_camera(), f.largura, f.altura))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        // **`--fonte` que não casa é erro, e o processo sai.** Antes daqui ele caía no índice 0 — o
        // monitor principal — e uma bancada que pedisse uma câmera ainda não enumerada transmitiria a
        // área de trabalho de quem está no Dell (a revisão adversarial de 18/09).
        // The only source is the virtual display. It creates nothing until Start is clicked.
        let escolhida = lista.iter().position(|f| f.especie == fontes::Especie::TelaEstendida);
        let varias_sessoes = argumentos.varias_sessoes;
        Arc::new(Emissor {
            estado: Mutex::new(Estado {
                fase: Fase::Inicial,
                versao: 1,
                nome_do_aparelho: nome,
                fontes: lista,
                tela_estendida_sem_driver,
                revisao_das_fontes: 1,
                lista_aplicada: 0,
                recarga_adiada: false,
                escolhida,
                aviso_da_fonte: None,
                sumida: None,
                pin: String::new(),
                endereco: None,
                ha_pares_conhecidos: identidade::ha_pares_conhecidos(),
                com_som: !argumentos.sem_som,
                som_ativo: false,
                som_recusado: String::new(),
                par: String::new(),
                conselho: String::new(),
                oferece_desparear: false,
                anunciando_por_mdns: false,
                resumo: String::new(),
                sair: false,
                receptores: Vec::new(),
                esperando_mais_um: false,
                porta_da_espera: None,
                camera_pelo_dono: false,
                gravando: false,
                rotulo_gravar: String::new(),
                linha_da_gravacao: String::new(),
            }),
            cancelamento: Mutex::new(None),
            parar: AtomicBool::new(false),
            argumentos,
            ja_espelhou_sozinho: AtomicBool::new(false),
            avisou_sem_escolha: AtomicBool::new(false),
            sessoes: std::sync::atomic::AtomicU64::new(0),
            varias: std::sync::OnceLock::new(),
            coordenador_na_tela: AtomicBool::new(varias_sessoes),
            microfone_pedido: AtomicBool::new(false),
            microfone: Mutex::new(None),
            camera_comum: Mutex::new(None),
            thread_da_camera_comum: Mutex::new(None),
            ajustes_disponiveis: AtomicBool::new(false),
            controlada_por: Mutex::new(None),
            pouca_luz: Mutex::new(None),
        })
    }

    /// **O botão do microfone da câmera comum** (`docs/audio.md` §8.2; `teleprompter-com-camera.md`
    /// §8.10, peça 9): começa desligado. Vale para a próxima sessão de câmera e para a que está no ar.
    pub fn alternar_microfone(&self, ligado: bool) {
        self.microfone_pedido.store(ligado, Ordering::SeqCst);
        // O `Arc` sai do cadeado antes de mexer no microfone (a revisão do código, B2): `desligar`
        // espera a thread dele, e ela acorda a janela.
        let m = self.microfone.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(m) = m {
            if ligado {
                m.ligar("o botão da janela principal"); // i18n: fora (diário)
            } else {
                m.pedir_desligar("o botão da janela principal"); // i18n: fora (diário)
            }
        }
        let mut e = self.estado();
        e.mudou();
    }

    /// O botão do microfone está ligado?
    pub fn microfone_pedido(&self) -> bool {
        self.microfone_pedido.load(Ordering::SeqCst)
    }

    /// A linha do microfone para a tela: aberto, ou por que não.
    ///
    /// **Estado, e não texto**: as três frases fixas ficam em português porque a janela as compara
    /// (`janela.rs`, `modelo_da_janela::legenda_do_microfone`) e as traduz ao mostrar (`idioma::tr`).
    /// A frase do microfone (`m.frase`) já nasce no idioma da hora, menos as chaves de
    /// `regras_r5` (a da privacidade e "O microfone não abriu."), que a janela também traduz.
    pub fn linha_do_microfone(&self) -> Option<String> {
        let m = self.microfone.lock().unwrap_or_else(|e| e.into_inner()).clone()?.estado();
        Some(if !m.frase.is_empty() {
            m.frase
        } else if m.aberto {
            "Com o som do microfone.".into() // i18n: chave
        } else if m.ligado {
            "Microfone abrindo…".into() // i18n: chave
        } else {
            "Microfone desligado: a câmera vai sem som (a track do microfone está na oferta, calada).".into() // i18n: chave
        })
    }

    /// Várias sessões: a bandeira de bancada `--varias-sessoes`.
    pub fn com_varias_sessoes(&self) -> bool {
        self.argumentos.varias_sessoes
    }

    /// **O coordenador é dono da tela** (R10): com a bandeira, sempre; sem ela, depois do Espelhar da
    /// "Tela estendida" e até o Espelhar de outra fonte. É o que decide a cena dos vários receptores,
    /// o Parar, o Desconectar e o conselho — e não mais a bandeira sozinha.
    pub fn coordenador_na_tela(&self) -> bool {
        self.coordenador_na_tela.load(Ordering::SeqCst)
    }

    /// O coordenador já nasceu neste processo (a saída espera o desmonte dele e solta os monitores
    /// virtuais que sobrarem).
    pub fn coordenador_de_pe(&self) -> bool {
        self.varias.get().is_some()
    }

    fn coordenador(self: &Arc<Self>) -> &crate::varias::Coordenador {
        self.varias.get_or_init(|| crate::varias::Coordenador::novo(Arc::clone(self)))
    }

    /// Desconecta **um** receptor (a linha dele na tela). Só existe com várias sessões.
    pub fn desconectar(self: &Arc<Self>, id: crate::sessoes::Id) {
        if self.coordenador_na_tela() {
            self.coordenador().desconectar(id);
        }
    }

    /// Espera o desmonte de todas as sessões e da fila do anúncio, com prazo — o que o `main` faz
    /// antes do `MFShutdown`. `true` quando tudo desmontou a tempo. Sem várias sessões, `true` na
    /// hora: o caminho de uma sessão só segue com a espera fixa de sempre.
    pub fn esperar_desmonte(&self, prazo: Duration) -> bool {
        match self.varias.get() {
            Some(c) => c.esperar_desmonte(prazo),
            None => true,
        }
    }

    pub fn estado(&self) -> MutexGuard<'_, Estado> {
        self.estado.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A pessoa escolheu a linha `indice` do seletor que mostrava a lista `revisao_da_tela`. Um
    /// clique de uma lista que já foi trocada (a recarga fora da thread da janela, antes do pulso
    /// que remonta o seletor) é descartado: o índice apontaria para outra linha (a revisão curta do
    /// `08af2cd`, A1).
    pub fn escolher_fonte(&self, indice: usize, revisao_da_tela: u64) {
        let mut e = self.estado();
        if revisao_da_tela != e.revisao_das_fontes {
            registro::linha(format!(
                "fontes: o clique na linha {indice} era da lista {revisao_da_tela}, e a lista agora é a {}: descartado, o seletor é remontado",
                e.revisao_das_fontes
            ));
            e.mudou();
            return;
        }
        let (linhas, escolhida, revisao) = (e.fontes.len(), e.escolhida, e.revisao_das_fontes);
        if let Some(indice) = crate::catalogo_de_cameras::escolha_do_clique(indice, revisao_da_tela, revisao, linhas, escolhida) {
            e.escolhida = Some(indice);
            // A pessoa escolheu: a que tinha sumido deixa de ser esperada de volta.
            e.sumida = None;
            self.tirar_aviso_da_fonte(&mut e);
            e.mudou();
        }
    }

    /// Tira do conselho **só** o aviso da fonte que sumiu, e deixa o resto (o motivo do fim de uma
    /// sessão, que pode ter chegado antes ou depois dele).
    fn tirar_aviso_da_fonte(&self, e: &mut Estado) {
        if let Some(aviso) = e.aviso_da_fonte.take() {
            let resto = crate::catalogo_de_cameras::sem_o_aviso(&e.conselho, &aviso);
            if resto != e.conselho {
                self.aconselhar(e, resto);
            }
        }
    }

    /// Põe (ou tira, com texto vazio) um conselho que o **emissor** escreve na tela. Com várias
    /// sessões e o coordenador já vivo, ele vai também pela tabela: o `publicar` do coordenador
    /// reescreve `e.conselho` com o da tabela a cada volta (≤ 250 ms), e um texto posto só aqui
    /// sumiria antes de alguém ler — o Espelhar pareceria não fazer nada (a revisão de código de
    /// 18/09, M3).
    fn aconselhar(&self, e: &mut Estado, texto: String) {
        // Só com o coordenador dono da tela (R10): ocioso depois de uma tela estendida, ele não
        // publica, e o conselho guardado na tabela dele reapareceria no Espelhar seguinte.
        if let (Some(c), true) = (self.varias.get(), self.coordenador_na_tela()) {
            c.aconselhar(texto.clone());
        }
        e.conselho = texto;
        e.mudou();
    }

    /// Um monitor ou uma câmera entrou ou saiu — reenumera e republica a lista.
    ///
    /// Chamada do `WM_DISPLAYCHANGE`, do `WM_DEVICECHANGE` das câmeras e da volta à tela inicial.
    /// Preserva a escolha **pelo id** (nome de dispositivo, link da câmera), não pelo índice: com
    /// dois monitores, desligar o primeiro faria o índice 1 virar o índice 0, e uma escolha guardada
    /// por posição passaria a apontar para outro monitor em silêncio. É o mesmo motivo pelo qual
    /// `Fonte::id` guarda `\\.\DISPLAY1` e não o `HMONITOR`.
    ///
    /// **A escolha que sumiu fica sem escolha, com o aviso na tela** — antes caía no índice 0, o
    /// monitor principal, sem dizer nada (a revisão adversarial de 18/09, achado 1 do catálogo).
    ///
    /// Não mexe em nada durante uma sessão: trocar a fonte debaixo de uma captura em curso não é
    /// coisa que a interface possa fazer sem renegociar.
    pub fn recarregar_fontes(&self) {
        // A fase **antes** de enumerar: um `WM_DEVICECHANGE` no meio de uma transmissão (uma baia
        // nascendo, o S24 entrando e saindo) não tem o que recompor, e enumerar câmeras e ler o
        // registro na thread da janela à toa enchia o registro do app (a revisão de código de 18/09).
        {
            let mut e = self.estado();
            if e.fase != Fase::Inicial {
                e.recarga_adiada = true;
                return;
            }
        }
        let (geracao, nova) = self.enumerar();
        let mut e = self.estado();
        if e.fase != Fase::Inicial {
            e.recarga_adiada = true;
            return;
        }
        self.aplicar_lista(&mut e, geracao, nova);
    }

    /// Toma a geração **e depois** enumera (a revisão curta do `08af2cd`, A2): uma enumeração que
    /// começou antes tem geração menor, termine quando terminar.
    fn enumerar(&self) -> (u64, ListaDoSeletor) {
        let geracao = GERACAO_DAS_LISTAS.fetch_add(1, Ordering::SeqCst) + 1;
        let nova = fontes_para_o_seletor(self.argumentos.com_cameras(), self.argumentos.camera_de_bancada.as_deref());
        (geracao, nova)
    }

    /// Troca a lista do seletor e refaz a escolha **pelo id**. Chamada com o estado travado, para a
    /// lista e a fase mudarem juntas na volta à tela inicial (`voltar_ao_inicio`). Só entra a lista
    /// mais nova que a última aplicada (A2).
    fn aplicar_lista(&self, e: &mut Estado, geracao: u64, lista: ListaDoSeletor) {
        if !crate::catalogo_de_cameras::lista_mais_nova(geracao, e.lista_aplicada) {
            registro::linha(format!(
                "fontes: a lista da enumeração {geracao} terminou depois da {}, e fica de fora",
                e.lista_aplicada
            ));
            return;
        }
        e.lista_aplicada = geracao;
        let ListaDoSeletor { fontes: nova, tela_estendida_sem_driver } = lista;
        // O ladrilho apagado não está em `fontes`: muda sozinho, sem mexer na escolha (R10, item 5:
        // o adaptador que aparece ou some com o app aberto entra na próxima recarga).
        if e.tela_estendida_sem_driver != tela_estendida_sem_driver {
            registro::linha(format!(
                "fontes: o adaptador do SudoVDA {} — a tela estendida {}",
                if tela_estendida_sem_driver { "sumiu" } else { "apareceu" },
                if tela_estendida_sem_driver { "fica apagada" } else { "pode ser escolhida" }
            ));
            e.tela_estendida_sem_driver = tela_estendida_sem_driver;
            e.mudou();
        }
        let iguais = nova.len() == e.fontes.len()
            && nova.iter().zip(e.fontes.iter()).all(|(a, b)| {
                a.id == b.id && a.nome == b.nome && a.largura == b.largura && a.altura == b.altura
            });
        if iguais {
            return;
        }
        let antes = e.escolhida.and_then(|i| e.fontes.get(i)).map(|f| (f.id.clone(), f.nome.clone()));
        registro::linha(format!(
            "fontes: a lista mudou — {} agora: {}",
            nova.len(),
            nova.iter()
                .map(|f| format!("camera={} {}x{}", f.e_camera(), f.largura, f.altura))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let reescolha = crate::catalogo_de_cameras::reescolher(
            antes.as_ref().map(|(id, nome)| (id.as_str(), nome.as_str())),
            e.sumida.as_ref().map(|(id, nome)| (id.as_str(), nome.as_str())),
            &candidatas(&nova),
        );
        match reescolha {
            Reescolha::Ficou(i) => e.escolhida = i,
            Reescolha::Sumiu { aviso } => {
                registro::linha(format!("fontes: a escolhida sumiu, sem escolha agora — {aviso}"));
                e.escolhida = None;
                e.sumida = antes;
                // O motivo do fim de uma sessão não é apagado pelo aviso: os dois ficam na tela
                // (a revisão de código de 18/09, m3), e o aviso é guardado para sair sozinho.
                self.tirar_aviso_da_fonte(e);
                let texto = crate::catalogo_de_cameras::com_o_aviso(&e.conselho, &aviso);
                e.aviso_da_fonte = Some(aviso);
                self.aconselhar(e, texto);
            }
            Reescolha::Voltou(i) => {
                registro::linha("fontes: a escolhida que tinha sumido voltou, escolhida de novo");
                e.escolhida = Some(i);
                e.sumida = None;
                self.tirar_aviso_da_fonte(e);
            }
            Reescolha::Reapareceu => {
                // O nome dela está na lista com outro id: não se escolhe (pode ser outro aparelho),
                // mas "não está mais disponível" seria falso.
                if e.aviso_da_fonte.is_some() {
                    registro::linha(
                        "fontes: uma fonte com o nome da que sumiu apareceu com outro id — sem escolha, e o aviso sai",
                    );
                }
                self.tirar_aviso_da_fonte(e);
            }
        }
        e.escolhida = nova.iter().position(|f| f.especie == fontes::Especie::TelaEstendida);
        e.fontes = nova;
        e.revisao_das_fontes += 1;
        e.mudou();
    }

    /// A caixa "Transmitir o som deste computador".
    ///
    /// Só vale antes do Espelhar, e isso não é preguiça de interface: `tracks` só existe em
    /// `hospedar` e o protocolo não renegocia (dívida 1). **Não dá para acender o som no meio de
    /// uma sessão de vídeo** — a track ou entrou na oferta, ou não existe. A janela esconde a caixa
    /// fora da tela inicial exatamente por isso.
    pub fn alternar_som(&self, ligado: bool) {
        let mut e = self.estado();
        if e.fase == Fase::Inicial && e.com_som != ligado {
            e.com_som = ligado;
            e.mudou();
        }
    }

    /// O toque em Espelhar que um agente de bancada não tem como dar.
    ///
    /// Com `--repetir-espelhar` ele volta a valer toda vez que o app retorna à tela inicial, que é
    /// como se exercita uma **segunda sessão sem reiniciar o processo**. `espelhar` já recusa
    /// sozinho quando a fase não é `Inicial`, então chamar em laço é seguro.
    pub fn talvez_espelhar_sozinho(self: &Arc<Self>) {
        if !self.argumentos.espelhar_ja {
            return;
        }
        if !self.argumentos.repetir_espelhar
            && self.ja_espelhou_sozinho.swap(true, Ordering::SeqCst)
        {
            return;
        }
        if self.estado().fase != Fase::Inicial {
            return;
        }
        // Sem escolha não há o que espelhar, e **não se escolhe por ela**: a escolha só existe se
        // `--fonte` casou ou se a pessoa (ou a abertura sem `--fonte`) escolheu. A exceção é a origem
        // sintética com várias sessões, que não captura monitor nenhum; numa sessão só ela ainda pede
        // uma fonte escolhida (`espelhar`), e sem esta distinção cada volta contaria uma "sessão"
        // que não abre (a revisão de código de 18/09, m2).
        let dispensa_escolha = (self.com_varias_sessoes() && self.argumentos.origem_sintetica.is_some())
            || self.argumentos.camera_sintetica;
        if self.estado().escolhida.is_none() && !dispensa_escolha {
            // Uma linha só, e não uma a cada volta do `--repetir-espelhar`.
            if !self.avisou_sem_escolha.swap(true, Ordering::SeqCst) {
                registro::linha("ERRO: --espelhar-ja sem fonte escolhida — nada é transmitido");
            }
            return;
        }
        self.avisou_sem_escolha.store(false, Ordering::SeqCst);
        let n = self.sessoes.fetch_add(1, Ordering::SeqCst) + 1;
        registro::linha(format!("espelhar automático: sessão número {n} deste processo"));
        self.espelhar();
    }

    // MARK: - espelhar

    pub fn espelhar(self: &Arc<Self>) {
        // **Com a tela R5 aberta, a janela principal não espelha nada** (`teleprompter-com-camera.md`
        // §8.10.1, M7): uma câmera por processo, e os dois anúncios teriam o mesmo nome de instância.
        let usa_o_coordenador = {
            let mut e = self.estado();
            if e.fase != Fase::Inicial {
                return;
            }
            if !crate::dono_da_captura::tomar_para_a_janela() {
                registro::linha(format!("espelhar recusado: {}", crate::dono_da_captura::FRASE_DA_CAMERA_OCUPADA_PELA_R5));
                self.aconselhar(&mut e, idioma::t(crate::dono_da_captura::FRASE_DA_CAMERA_OCUPADA_PELA_R5).to_string());
                return;
            }
            // **Quem espelha** (R10, 02/10): o coordenador com `--varias-sessoes`, como antes, ou com
            // a "Tela estendida" escolhida; qualquer outra fonte sem a bandeira, o caminho de uma
            // sessão só. A troca do dono da tela é feita com o estado travado: o `publicar` do
            // coordenador confere o dono debaixo da mesma trava.
            #[cfg(feature = "tela-estendida-futura")]
            let escolheu_tela_estendida = e
                .escolhida
                .and_then(|i| e.fontes.get(i))
                .is_some_and(|f| f.especie == fontes::Especie::TelaEstendida);
            #[cfg(feature = "tela-estendida-futura")]
            let usa = crate::regras_da_tela_estendida::usa_o_coordenador(
                self.com_varias_sessoes(),
                escolheu_tela_estendida,
                self.argumentos.camera_sintetica,
            );
            #[cfg(not(feature = "tela-estendida-futura"))]
            let usa = self.com_varias_sessoes();
            if usa != self.coordenador_na_tela.swap(usa, Ordering::SeqCst) {
                registro::linha(if usa {
                    "espelhar: a tela estendida — quem espelha é o coordenador de várias sessões"
                } else {
                    "espelhar: uma sessão só (o coordenador da tela estendida fica ocioso)"
                });
            }
            usa
        };
        // **Com várias sessões, quem espelha é o coordenador.** O caminho abaixo é o de uma sessão
        // só, o produto de hoje, e não foi tocado.
        if usa_o_coordenador {
            let (fonte, com_som) = {
                let mut e = self.estado();
                if e.fase != Fase::Inicial {
                    return;
                }
                // A origem sintética não precisa de monitor nenhum — e na Sessão 0 do SSH pode não
                // haver um enumerado. Sem ela, o monitor escolhido é o que cada sessão captura.
                let escolhida = e.escolhida.and_then(|i| e.fontes.get(i)).cloned();
                // **A câmera sintética da bancada** (`--camera-sintetica`): a fonte do Quall neste
                // processo, com o padrão de bancada nosso, no lugar de qualquer escolha.
                let escolhida = if self.argumentos.camera_sintetica { Some(fonte_da_camera_sintetica()) } else { escolhida };
                let fonte = match (escolhida, &self.argumentos.origem_sintetica) {
                    (Some(f), _) => f,
                    (None, Some(_)) => Fonte {
                        id: "sintetica".into(),
                        nome: "origem sintética".into(), // i18n: fora (bancada e diário)
                        largura: 0,
                        altura: 0,
                        primario: false,
                        especie: fontes::Especie::Monitor,
                    },
                    (None, None) => {
                        // Pela tabela também: o coordenador de uma rodada anterior apagaria o texto
                        // na volta seguinte (M3).
                        let texto = texto_sem_escolha(&e.fontes);
                        self.aconselhar(&mut e, texto);
                        crate::dono_da_captura::soltar_da_janela();
                        return;
                    }
                };
                // A câmera não leva o som do sistema. **O microfone da câmera no caminho de várias
                // sessões (bancada) fica fora da fase 4 do R5** (`teleprompter-com-camera.md` §8.10.1,
                // M10): só o caminho de uma sessão o tem.
                let com_som = e.com_som && !fonte.e_camera();
                (fonte, com_som)
            };
            self.coordenador().espelhar(fonte, com_som);
            return;
        }
        let (fonte, pin_texto, pin, com_som) = {
            let mut e = self.estado();
            if e.fase != Fase::Inicial {
                return;
            }
            // A câmera sintética (bancada, `--camera-sintetica`) no lugar da escolha, como com várias.
            let escolhida = if self.argumentos.camera_sintetica {
                Some(fonte_da_camera_sintetica())
            } else {
                e.escolhida.and_then(|i| e.fontes.get(i)).cloned()
            };
            let Some(fonte) = escolhida else {
                let texto = texto_sem_escolha(&e.fontes);
                self.aconselhar(&mut e, texto);
                crate::dono_da_captura::soltar_da_janela();
                return;
            };
            // O PIN sorteado pelo núcleo é o caminho normal: a qualidade do sorteio é o que segura
            // o pareamento. `--pin` existe só para a bancada, onde ninguém tem olhos para ler a
            // tela.
            let pin = match self.argumentos.pin.as_deref() {
                Some(p) => Pin::parse(p),
                None => Pin::generate(),
            };
            let pin = match pin {
                Ok(p) => p,
                Err(erro) => {
                    e.conselho = idioma::tf("Não consegui preparar o PIN: {}", &[&erro]);
                    e.mudou();
                    crate::dono_da_captura::soltar_da_janela();
                    return;
                }
            };
            let texto = pin.to_display();
            e.conselho.clear();
            e.oferece_desparear = false;
            e.resumo.clear();
            e.par.clear();
            e.pin = texto.clone();
            e.ha_pares_conhecidos = identidade::ha_pares_conhecidos();
            e.som_ativo = false;
            e.som_recusado.clear();
            e.fase = Fase::Esperando;
            e.mudou();
            // A câmera não leva o som do sistema: leva **o microfone** (a track sempre na oferta, o
            // botão, `docs/audio.md` §8.2; a regra "nunca abre o microfone" caiu em 24/09).
            let com_som = e.com_som && !fonte.e_camera();
            (fonte, texto, pin, com_som)
        };

        let cancelamento = Cancelamento::novo();
        *self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancelamento.clone());
        self.parar.store(false, Ordering::SeqCst);

        let eu = Arc::clone(self);
        // **A câmera comum pelo dono** (§8.10.3): a câmera abre com a espera, e grava sem receptor. O
        // caminho de antes (a câmera nascendo com a sessão, sem Gravar) fica atrás de
        // `--camera-comum-na-sessao`, o braço de controle da bancada.
        if fonte.e_camera() && !self.argumentos.camera_comum_na_sessao {
            drop(pin);
            let h = std::thread::Builder::new()
                .name("quall.camera.comum".into())
                .spawn(move || eu.correr_camera_comum(fonte, pin_texto));
            if let Ok(h) = h {
                *self.thread_da_camera_comum.lock().unwrap_or_else(|e| e.into_inner()) = Some(h);
            }
            return;
        }
        let _ = std::thread::Builder::new()
            .name("quall.sessao".into())
            .spawn(move || eu.correr_sessao(fonte, pin, pin_texto, com_som, cancelamento));
    }

    /// Roda inteira fora da thread da janela, do começo ao fim da sessão.
    #[allow(clippy::too_many_arguments)]
    fn correr_sessao(
        self: Arc<Self>,
        fonte: Fonte,
        pin: Pin,
        pin_texto: String,
        com_som: bool,
        cancelamento: Cancelamento,
    ) {
        let device_id = identidade::device_id();
        let nome_do_aparelho = self.estado().nome_do_aparelho.clone();

        // A porta vem do servidor, não de uma reserva prévia. `bind(0)` e depois `port()` fecha a
        // corrida entre "reservei a porta" e "hospedei nela".
        // `--so-local` (bancada): a espera só em 127.0.0.1, sem anúncio, e o ICE preso nele.
        let so_local = self.argumentos.so_local;
        let aberto = if so_local {
            SignalingServer::bind_em(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), self.argumentos.porta)
        } else {
            SignalingServer::bind(self.argumentos.porta)
        };
        let servidor = match aberto {
            Ok(s) => s,
            Err(erro) => {
                registro::linha(format!("bind falhou: status={}", crate::diagnostico_rede::status(&erro)));
                self.ao_falhar(Error::Io(erro.to_string()));
                return;
            }
        };
        let porta = servidor.port().unwrap_or(self.argumentos.porta);
        // Com `--so-local` a espera só existe em 127.0.0.1: o endereço da tela é esse.
        let endereco = if so_local { Some(format!("127.0.0.1:{porta}")) } else { enderecos::para_digitar(porta) };
        {
            let mut e = self.estado();
            e.endereco = endereco.clone();
            e.mudou();
        }

        let eu_anuncio = anuncio(
            &device_id,
            &nome_do_aparelho,
            // O anúncio diz o que esta sessão emite: a tela, ou a câmera.
            Capabilities {
                screen_source: !fonte.e_camera(),
                camera_source: fonte.e_camera(),
                sink: false,
            },
        );

        // Anunciar **antes** de hospedar, porque `hospedar` bloqueia: se o anúncio esperasse a
        // sessão subir, este computador só apareceria na lista dos outros **depois** de já ter
        // recebido a conexão. Falhar aqui é esperado em rede com multicast bloqueado e não é erro
        // de produto — o endereço continua na tela e continua funcionando.
        let anunciante = if so_local { None } else { Advertiser::start(&eu_anuncio, porta).ok() };
        {
            let mut e = self.estado();
            e.anunciando_por_mdns = anunciante.is_some();
            e.mudou();
        }
        registro::linha(format!(
            "espelhar: e_camera={} porta={porta} endereco_disponivel={} mdns={} pares_conhecidos={}",
            fonte.e_camera(),
            endereco.is_some(),
            anunciante.is_some(),
            self.estado().ha_pares_conhecidos,
        ));

        let conhecidos = identidade::pares_conhecidos();
        let rotulo = fonte.rotulo_da_track(&nome_do_aparelho);

        // **A track de áudio se decide agora, antes da oferta, ou não existe nesta sessão.**
        // `tracks` só vale em `hospedar` e o protocolo não renegocia (dívida 1) — não há como
        // acender o som depois. E declarar uma track que o WASAPI não vai conseguir alimentar é
        // pior que não declarar: do lado de quem recebe, uma track de áudio que nunca entrega um
        // pacote é indistinguível de silêncio, e a pessoa vai procurar o defeito no volume dela.
        //
        // Por isso o endpoint é **conferido** antes: `audio::conferir` abre o dispositivo e lê o
        // formato do mixador sem chamar `Initialize` nem `Start`, ou seja, sem capturar uma única
        // amostra do som de ninguém durante a espera.
        let config_de_audio = self.argumentos.config_de_audio();
        let audio_prometido = if com_som {
            match audio::conferir(&config_de_audio.alvo) {
                Ok(descricao) => {
                    registro::linha(format!("audio: endpoint conferido — {descricao}"));
                    true
                }
                Err(motivo) => {
                    registro::linha(format!(
                        "audio: NÃO vou declarar a track — o endpoint não abriu: {motivo}"
                    ));
                    let mut e = self.estado();
                    e.som_recusado = idioma::t("Este computador não deixou capturar o som da saída de áudio; a transmissão vai só com imagem.").into();
                    e.mudou();
                    false
                }
            }
        } else {
            false
        };

        // `TrackConfig::new` já traz `PRESET_AUDIO_DO_SISTEMA` junto com a espécie — o preset não é
        // escrito aqui de propósito: quem o define é o núcleo, e uma casca com `128_000` cravado
        // faria o `fmtp` do SDP e o encoder discordarem no dia em que ele mudar.
        let track_de_audio =
            || TrackConfig::new(TrackKind::SystemAudio, idioma::tf("Som de {}", &[&nome_do_aparelho]));
        let som_primeiro = audio_prometido && self.argumentos.som_primeiro;
        let mut tracks = Vec::new();
        if som_primeiro {
            tracks.push(track_de_audio());
        }
        // A espécie da track de vídeo sai da fonte: `Camera` para uma câmera, como no Mac.
        tracks.push(TrackConfig::new(
            if fonte.e_camera() { TrackKind::Camera } else { TrackKind::Screen },
            rotulo.clone(),
        ));
        if audio_prometido && !som_primeiro {
            tracks.push(track_de_audio());
        }
        // **O microfone da câmera, sempre na oferta** (§4.2: o botão pode ligar no meio, e não há
        // renegociação), calado com o botão desligado.
        if fonte.e_camera() {
            tracks.push(TrackConfig::new(TrackKind::Microphone, idioma::tf("Microfone de {}", &[&nome_do_aparelho])));
        }
        registro::linha(format!(
            "tracks na oferta: {}",
            tracks
                .iter()
                .map(|t| format!("{:?}", t.kind))
                .collect::<Vec<_>>()
                .join(" + ")
        ));

        // **Uma tentativa longa, não um laço de tentativas curtas.** A dívida 21 mostra que
        // `Session::offerer_com_tracks` nasce dentro do laço de tentativas e que a dívida 14 é
        // cobrada por tentativa: numa rede em que o pareamento fecha mas o ICE nunca fecha, um
        // laço acumularia uma `Track` vazada por volta. A dívida 4 tem de ser paga antes de
        // qualquer casca tirar o teto de tentativas.
        let resultado = hospedar(
            &servidor,
            SessionConfig {
                announcement: eu_anuncio,
                pin: Some(pin),
                known: conhecidos,
                transport: if so_local {
                    TransportConfig { bind_address: Some("127.0.0.1".into()), ..TransportConfig::default() }
                } else {
                    TransportConfig::default()
                },
                tracks,
                timeout: Duration::from_secs(5 * 60),
                // Detector de silêncio do caminho **desligado**, que é o padrão do núcleo:
                // tela parada legitimamente não produz quadro, e quem sabe se a origem produz
                // continuamente é a casca. Ver `SessionConfig::silencio_do_caminho`.
                silencio_do_caminho: None,
                cancelamento,
            },
        );

        // Conectado ou não, paramos de anunciar. Continuar anunciando durante a transmissão
        // convidaria um terceiro aparelho para uma porta que já não aceita ninguém.
        if let Some(a) = anunciante {
            let _ = a.stop();
        }
        {
            let mut e = self.estado();
            e.anunciando_por_mdns = false;
            e.mudou();
        }

        let mut pronto = match resultado {
            Ok(p) => p,
            Err(erro) => {
                registro::linha(format!("hospedar falhou: status={}", crate::diagnostico_rede::status(&erro)));
                self.ao_falhar(erro);
                return;
            }
        };

        // O pareamento fechou: grava **antes** de qualquer outra coisa. Se o app morrer no meio da
        // transmissão, o par continua conhecido e a próxima vez não pede PIN.
        let mut novos = PairedPeers::new();
        novos.insert(&pronto.outcome);
        identidade::guardar_pares(&novos);

        let nome_do_par = pronto.peer.display_name.clone();
        // `descartados` só é diferente de zero quando alguém conectou e sumiu no meio — o
        // receptor que fecha o app, o scanner de porta da LAN. Até 01/09/2026 cada um desses
        // fechava a porta em definitivo com o processo vivo, e era o "depois que sai não
        // conecta". Sai no registro para que o conserto continue visível.
        registro::linha(format!(
            "conectado: candidatos_descartados={} tela_do_par={}",
            pronto.descartados,
            // A tela que o receptor disse no aperto de mão (`Announcement::screen`). Este caminho
            // só a registra: o formato por receptor é do emissor com várias sessões.
            pronto
                .peer
                .screen
                .as_ref()
                .map(|s| format!("{}x{}", s.width_px, s.height_px))
                .unwrap_or_else(|| "não disse".into()),
        ));
        {
            let mut e = self.estado();
            e.par = nome_do_par;
            e.fase = Fase::Transmitindo;
            e.ha_pares_conhecidos = true;
            e.mudou();
        }

        // **A captura só começa agora, depois de alguém conectar.** Capturar durante a espera
        // gastaria GPU e comporia a tela de trabalho de uma pessoa para não mandar quadro a lugar
        // nenhum.
        // A câmera não tem `HMONITOR`: ela é conferida pela interface habilitada, e aberta pelo link.
        let sintetica = fonte.id == ID_DA_CAMERA_SINTETICA;
        let hmonitor = if fonte.e_camera() {
            if !sintetica && crate::cameras::interface_habilitada(&fonte.id) == Some(false) {
                self.encerrar_com(idioma::tf("A câmera \"{}\" não está mais conectada.", &[&fonte.nome]));
                return;
            }
            None
        } else {
            match fontes::achar_hmonitor(&fonte.id) {
                Some(h) => Some(h),
                None => {
                    // Falhar em vez de cair para outro monitor: transmitir a tela errada em silêncio é
                    // pior que não transmitir.
                    self.encerrar_com(idioma::tf("O monitor \"{}\" não está mais conectado.", &[&fonte.nome]));
                    return;
                }
            }
        };
        // **A track é achada pela espécie, nunca pela posição.** `Ready::tracks` de fato preserva
        // a ordem de `SessionConfig::tracks`, mas depender disso é o tipo de acoplamento que
        // sobrevive silencioso até alguém inserir uma linha no meio.
        let iv = pronto
            .tracks
            .iter()
            .position(|t| matches!(t.kind(), TrackKind::Screen | TrackKind::Camera))
            .unwrap_or(0);
        let ia = if fonte.e_camera() {
            pronto.tracks.iter().position(|t| t.kind() == TrackKind::Microphone)
        } else {
            pronto.tracks.iter().position(|t| t.kind() == TrackKind::SystemAudio)
        };

        // **Um zero de relógio para as duas cadeias.** `AmostraDeAudio::timestamp_us` e
        // `QuadroCodificado::timestamp_us` só alinham as duas tracks se falarem do mesmo instante
        // zero; duas cadeias chamando `Instant::now()` cada uma na sua abertura teriam zeros
        // separados pelo tempo de montar o encoder de vídeo — dezenas de milissegundos.
        let origem = Instant::now();

        let origem_da_camera = if sintetica {
            crate::captura_de_camera::FonteDaCamera::DoQuallNoProcesso { regua: true }
        } else {
            crate::captura_de_camera::FonteDaCamera::Link(fonte.id.clone())
        };
        let aberta = match hmonitor {
            Some(hmonitor) => Cadeia::abrir(
                &fonte,
                hmonitor,
                self.argumentos.fps,
                origem,
                self.argumentos.idr_por_flush,
                self.argumentos.idr_por_recriacao(),
                self.argumentos.taxa_de_entrega,
                self.argumentos.piso_entre_recriacoes_ms,
                self.argumentos.bitrate_alvo,
                self.argumentos.troca_a_quente(),
                self.argumentos.caixa_unica,
            ),
            // **A câmera** (fase 3 de `docs/camera-no-windows.md`): as mesmas opções da tela, pela
            // entrada que aceita origem; a placa sai do encoder que ativa, pelo LUID.
            None => Cadeia::abrir_com(
                crate::transmissao::OrigemDaCadeia::Camera { fonte: &origem_da_camera, parar: Some(&self.parar), bater: None },
                crate::transmissao::OpcoesDaCadeia {
                    fps: self.argumentos.fps,
                    origem_do_relogio: origem,
                    idr_por_flush: self.argumentos.idr_por_flush,
                    idr_por_recriacao: self.argumentos.idr_por_recriacao(),
                    taxa_de_entrega: self.argumentos.taxa_de_entrega,
                    piso_entre_recriacoes_ms: self.argumentos.piso_entre_recriacoes_ms,
                    bitrate_alvo: self.argumentos.bitrate_alvo,
                    troca_a_quente: self.argumentos.troca_a_quente(),
                    caixa_unica: self.argumentos.caixa_unica,
                    preferencia: if self.argumentos.preferir_intel {
                        crate::encoder::Preferencia::Intel
                    } else {
                        crate::encoder::Preferencia::Produto
                    },
                    padroes: None,
                },
            ),
        };
        let mut cadeia = match aberta {
            Ok(c) => c,
            Err(erro) => {
                // A câmera diz a frase dela ("A câmera está em uso por outro app", "O Windows negou o
                // acesso…"); o detalhe de cada tentativa vai só para o registro.
                let texto = erro.message().to_string();
                registro::linha(format!("a captura não abriu: {texto}"));
                if fonte.e_camera() {
                    self.encerrar_com(crate::regras_da_camera::so_a_frase(&texto).to_string());
                } else {
                    self.encerrar_com(idioma::tf("Não consegui iniciar a captura: {}", &[&erro]));
                }
                return;
            }
        };
        registro::linha(format!(
            "captura: {}x{} encoder=\"{}\" hardware={} adaptador={}",
            cadeia.largura, cadeia.altura, cadeia.nome_do_encoder, cadeia.encoder_e_hardware,
            cadeia.adaptador
        ));
        // O tamanho que sai, para a linha da tela de transmissão (a câmera diz o dela, §6.1).
        let (largura_da_camera, altura_da_camera) = (cadeia.largura, cadeia.altura);

        // O som só começa a ser capturado agora, junto com a imagem, e pelo mesmo motivo: capturar
        // durante a espera seria compor o som da máquina de uma pessoa para não mandar a lugar
        // nenhum.
        let mut cadeia_de_audio: Option<CadeiaDeAudio> = None;
        // **A câmera comum: o microfone**, no zero de relógio desta sessão, aberto se o botão já estava
        // ligado; o ramal dele é a `CadeiaDeAudio` da sessão.
        if let (true, Some(_)) = (fonte.e_camera(), ia) {
            let eu = Arc::clone(&self);
            let mic = crate::microfone::Microfone::novo(
                origem,
                self.argumentos.microfone_tom,
                // **Sem esperar o estado** (a revisão do código, B2): a janela pinta com o estado
                // travado e pergunta pelo microfone; a thread dele, ao acordar a janela, não pode
                // esperar esse cadeado. O pulso de 100 ms da janela cobre a volta perdida.
                Box::new(move || {
                    if let Ok(mut e) = eu.estado.try_lock() {
                        e.mudou();
                    }
                }),
            );
            cadeia_de_audio = Some(mic.ramal_da_rede());
            registro::linha("microfone: na oferta desta sessão de câmera (calado até o botão)");
            // **Guardado antes de ler o botão** (a revisão do código): um toque no meio acha o
            // microfone guardado, ou esta leitura acha o toque. Ligar duas vezes não abre duas.
            *self.microfone.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&mic));
            if self.microfone_pedido.load(Ordering::SeqCst) {
                mic.ligar("o botão já estava ligado ao começar"); // i18n: fora (diário)
            }
        } else if let Some(ia) = ia {
            let preset = pronto.tracks[ia].preset_de_audio();
            match preset {
                Some(preset) => {
                    match CadeiaDeAudio::abrir(config_de_audio.clone(), preset, origem) {
                        Ok(c) => {
                            registro::linha(format!("audio: capturando — {}", c.descricao()));
                            cadeia_de_audio = Some(c);
                            let mut e = self.estado();
                            e.som_ativo = true;
                            e.mudou();
                        }
                        Err(erro) => {
                            // A track já está na oferta e não dá para tirá-la. O que dá é dizer, no
                            // registro e na tela, que ela vai ficar muda — em vez de deixar a pessoa
                            // procurar o defeito no volume do outro aparelho.
                            registro::linha(format!(
                                "audio: a captura NÃO subiu depois de a track ser declarada: {erro}"
                            ));
                            let mut e = self.estado();
                            e.som_recusado = idioma::t("A track de som foi negociada, mas a captura não subiu: o outro aparelho vai receber imagem sem som.").into();
                            e.mudou();
                        }
                    }
                }
                None => registro::linha(
                    "audio: a track de sistema veio sem preset do núcleo — não há como codificar",
                ),
            }
        }

        // O receptor entrou no meio: pedir um IDR já, em vez de esperar o próximo do GOP.
        cadeia.pedir_idr();

        // **O laço mora em `sessao_de_emissao.rs`**, com as mesmas linhas de registro de quando
        // morava aqui: é o mesmo laço que as sessões do emissor com vários receptores rodam. Os
        // valores abaixo são os do caminho de uma sessão só — a bandeira de parada e o estado
        // deste emissor, a testemunha da reenumeração pelo nome do monitor, a régua de handles, e
        // a cadeia que morre sem encerrar a sessão (o comportamento de hoje).
        // A câmera: a interface habilitada, com memória e confirmação (`None` antes de lê-la
        // habilitada não derruba; cinco `None` seguidos depois dela são o nó que saiu, a revisão do
        // código da fase 3, m1, e a reconferência); o monitor: a reenumeração pelo nome GDI.
        let e_camera = fonte.e_camera();
        let testemunha = std::cell::Cell::new(crate::regras_da_camera::TestemunhaDaInterface::default());
        let ainda_existe = || {
            if e_camera && sintetica {
                // A sintética não tem nó: a testemunha é a captura.
                true
            } else if e_camera {
                // Com confirmação: um `None` passageiro não encerra (a reconferência da fase 3).
                let mut t = testemunha.get();
                let presente = t.presente(crate::cameras::interface_habilitada(&fonte.id));
                testemunha.set(t);
                presente
            } else {
                fontes::achar_hmonitor(&fonte.id).is_some()
            }
        };
        // A interface já foi lida habilitada? Sem isso a pausa da câmera tem teto (a sintética não
        // tem nó: conta como confirmada).
        let interface_confirmada = || sintetica || !e_camera || testemunha.get().ja_habilitada();
        let ctx = ContextoDoLaco {
            parar: &self.parar,
            espiada: Duration::from_millis(self.argumentos.espiada_ms),
            vigia: Some(VigiaDoMonitor {
                id: &fonte.id,
                nome: &fonte.nome,
                ainda_existe: &ainda_existe,
                e_camera,
                interface_confirmada: &interface_confirmada,
            }),
            regua: true,
            vigiar_morte: false,
            batimento: None,
            seguidor: None,
            // A câmera na sessão (`--camera-comum-na-sessao`) é bancada, sem dono: sem controle remoto.
            camera_remota: None,
        };
        let mut laco = Laco::novo();
        let fim = laco.correr(
            &mut pronto,
            &mut cadeia,
            cadeia_de_audio.as_ref(),
            None,
            iv,
            ia,
            &ctx,
            &mut |c, enviados, audio_enviados, parada| {
                let mut e = self.estado();
                // A câmera parada: a frase no lugar dos contadores, até ela voltar.
                if let Some(ha) = parada {
                    e.resumo = crate::regras_da_camera::texto_da_camera_parada(ha);
                    e.mudou();
                    return;
                }
                // No idioma da hora: o resumo é reescrito a cada relato, e a troca aparece no próximo.
                let camera = if e_camera { idioma::tf("câmera {}×{} · ", &[&largura_da_camera, &altura_da_camera]) } else { String::new() };
                let som = if e.som_ativo { idioma::tf(" · {} quadros de som", &[&audio_enviados]) } else { String::new() };
                let latencia = format!("{:.1}", c.latencia_media_ms());
                e.resumo = idioma::tf("{}{} quadros · {} IDR · captura+encode {} ms{}", &[&camera, &enviados, &c.idrs, &latencia, &som]);
                e.mudou();
            },
        );
        let motivo_do_fim = fim.motivo;

        // Parar a captura **antes** de fechar a sessão: mandar um quadro para uma sessão em
        // fechamento é pedir para ela esperar por si mesma. Vale igual para o som — e o som tem uma
        // razão a mais: parar a thread de áudio também para o tom de prova, e um tom que continua
        // tocando depois do fim da corrida é ruído na sala de quem está na bancada.
        cadeia.fechar();
        // O microfone da câmera fecha com a sessão (o botão continua como estava, para a próxima).
        let mic = self.microfone.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(m) = mic {
            m.soltar_ramal_da_rede();
            m.desligar("a sessão de câmera acabou"); // i18n: fora (bancada e diário)
        }
        if let Some(mut ca) = cadeia_de_audio.take() {
            ca.fechar();
            registro::linha(format!(
                "audio (fim da casca): {} | enviados={} recusados={}",
                ca.contadores.linha(),
                laco.audio_enviados,
                laco.audio_recusados
            ));
            if let Some(ia) = ia {
                registro::linha(format!(
                    "audio nucleo (final): quadros={} bytes={} mid={} viva={}",
                    pronto.tracks[ia].quadros_enviados(),
                    pronto.tracks[ia].bytes_enviados(),
                    pronto.tracks[ia].mid(),
                    pronto.tracks[ia].esta_viva(),
                ));
            }
        }
        laco.registrar_fim(&cadeia, &pronto, iv);

        pronto.link.close("transmissão encerrada"); // i18n: fora (o motivo do fio)
        drop(pronto);
        registro::linha("sessao encerrada");

        {
            let mut e = self.estado();
            if !motivo_do_fim.is_empty() {
                // Só a frase: o detalhe (o `HRESULT` do leitor da câmera) já foi para o registro
                // na linha "fim do laço".
                e.conselho = crate::regras_da_camera::so_a_frase(&motivo_do_fim).to_string();
            }
        }
        self.voltar_ao_inicio();
    }

    // MARK: - encerrar

    /// O Cancelar/Parar da tela de espera. **Funciona de verdade**, que aqui significa: a espera
    /// bloqueada destrava, o anúncio mDNS sai do ar, a captura para e a tela volta ao começo.
    pub fn encerrar(&self) {
        // O coordenador só recebe o Parar quando é dono da tela (R10): depois de uma tela estendida,
        // o Parar de uma sessão só é desta sessão.
        if let (Some(c), true) = (self.varias.get(), self.coordenador_na_tela()) {
            c.encerrar();
            registro::linha("cancelar pedido pela interface (várias sessões)");
            return;
        }
        {
            let e = self.estado();
            if e.fase != Fase::Esperando && e.fase != Fase::Transmitindo {
                return;
            }
        }
        self.estado().fase = Fase::Encerrando;
        self.estado().mudou();
        self.parar.store(true, Ordering::SeqCst);
        if let Some(c) = self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.cancelar();
        }
        registro::linha("cancelar pedido pela interface");
    }

    fn encerrar_com(&self, conselho: String) {
        registro::linha(format!("encerrando: {conselho}"));
        {
            let mut e = self.estado();
            e.conselho = conselho;
        }
        self.voltar_ao_inicio();
    }

    fn ao_falhar(&self, erro: Error) {
        let mut e = self.estado();
        // Cancelar é o caminho normal, não uma falha: a pessoa desistiu, e a tela volta ao começo
        // sem nenhum aviso vermelho.
        if matches!(erro, Error::Cancelled) || self.parar.load(Ordering::SeqCst) {
            drop(e);
            self.voltar_ao_inicio();
            return;
        }
        match erro {
            // Dívida 22: **funcionou ontem, hoje não funciona.** O núcleo não cai de volta para o
            // caminho do PIN sozinho, então quem oferece a saída é a casca — e só aqui, no caso em
            // que ela é a ação certa.
            Error::NeedsPin(_) => {
                e.conselho = idioma::t("Um aparelho tentou entrar com um pareamento que este computador não reconhece mais. Peça para ele digitar o PIN de novo; se continuar falhando, esqueça os pareamentos e comecem do zero.").into();
                e.oferece_desparear = true;
            }
            Error::NoRoute(_) => {
                e.conselho = idioma::t("O pareamento fechou, mas os dois aparelhos não acharam caminho um para o outro. Quase sempre é a rede: Wi-Fi de hóspede, isolamento entre aparelhos ou redes diferentes. Ponha os dois na mesma rede e tente de novo.").into();
            }
            Error::Timeout(_) => {
                e.conselho = idioma::t("Ninguém entrou em cinco minutos. Clique em Espelhar de novo quando o outro aparelho estiver pronto.").into();
            }
            // **Medido, e não era o que este braço dizia.** Ao exercitar a dívida 22 — este
            // computador esquece o par, o outro tenta retomar — o núcleo **não** devolveu
            // `Error::NeedsPin` ao emissor: devolveu `Error::Pairing`, embrulhado como *"o receptor
            // recusou: o outro aparelho não reconhece mais este pareamento"*. Ou seja, este braço
            // é quem atende os dois casos, e o texto antigo ("o PIN não conferiu") **mentia** na
            // metade das vezes: o PIN podia estar certíssimo e o pareamento ter sido esquecido.
            //
            // O conserto certo é um status do núcleo que separe as duas causas; enquanto ele não
            // existe, o texto nomeia as duas e dá a saída, que é a mesma. Adivinhar a causa por
            // comparação de string na mensagem de erro seria pior que não adivinhar.
            Error::Pairing(_) => {
                e.conselho = idioma::t("O pareamento não fechou. Ou o PIN não conferiu, ou o outro aparelho tentou entrar com um pareamento que este computador não reconhece mais. Nos dois casos a saída é a mesma: clique em Espelhar de novo e digite no outro aparelho o PIN novo que aparecer aqui.").into();
            }
            outro => e.conselho = outro.to_string(),
        }
        drop(e);
        self.voltar_ao_inicio();
    }

    fn voltar_ao_inicio(&self) {
        // A lista pode ter mudado durante a sessão (`recarregar_fontes` não mexe fora da tela
        // inicial): sem recompor, a pessoa voltaria a uma lista velha, com uma câmera que já saiu.
        // Enumerada **antes** de travar, e aplicada no mesmo trecho travado que publica a fase: a
        // janela nunca vê a fase inicial com a lista velha, e um clique nesse intervalo não leva um
        // índice da lista velha para a nova (a revisão de código de 18/09, m1).
        // O pedido que chegar daqui até a fase inicial é atendido de novo depois (L2); os de antes,
        // esta enumeração cobre.
        self.estado().recarga_adiada = false;
        let (geracao, nova) = self.enumerar();
        let mut e = self.estado();
        e.fase = Fase::Inicial;
        crate::dono_da_captura::soltar_da_janela();
        e.camera_pelo_dono = false;
        e.gravando = false;
        e.rotulo_gravar.clear();
        e.linha_da_gravacao.clear();
        e.pin.clear();
        e.par.clear();
        e.endereco = None;
        e.anunciando_por_mdns = false;
        e.resumo.clear();
        e.ha_pares_conhecidos = identidade::ha_pares_conhecidos();
        self.aplicar_lista(&mut e, geracao, nova);
        e.mudou();
        let adiada = std::mem::take(&mut e.recarga_adiada);
        drop(e);
        *self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.parar.store(false, Ordering::SeqCst);
        if adiada {
            // Na mesma thread que acabou de enumerar para a volta: nada de novo na thread da janela.
            registro::linha("fontes: um pedido de recarga chegou durante a volta à tela inicial; enumerando de novo");
            self.recarregar_fontes();
        }
    }

    /// **Bancada.** Manda a janela fechar, o que encerra a sessão pelo mesmo caminho que o
    /// Cancelar humano — sem `TerminateProcess`, que pularia o `Bye`, o adeus do mDNS e o registro
    /// final. Uma corrida que mata o processo prova o pipeline e não prova o desligamento.
    pub fn pedir_saida(&self) {
        registro::linha("--sair-apos atingido; fechando pelo mesmo caminho do Cancelar");
        self.encerrar();
        let mut e = self.estado();
        e.sair = true;
        e.mudou();
    }

    /// Só oferecido quando a retomada de fato falhou (`Error::NeedsPin`) — dívida 22.
    pub fn esquecer_pares(&self) {
        identidade::esquecer_pares();
        let mut e = self.estado();
        e.ha_pares_conhecidos = false;
        e.oferece_desparear = false;
        e.conselho = idioma::t("Pareamentos esquecidos. Da próxima vez o PIN será pedido.").into();
        e.mudou();
    }
}

/// O que aparece na tela quando não há fonte escolhida.
fn texto_sem_escolha(fontes: &[Fonte]) -> String {
    if fontes.is_empty() {
        idioma::t("Nenhum monitor foi encontrado neste computador.").into()
    } else {
        idioma::t("Escolha o que transmitir.").into()
    }
}

/// O que `catalogo_de_cameras` precisa de cada linha para escolher.
fn candidatas(lista: &[Fonte]) -> Vec<crate::catalogo_de_cameras::Candidata<'_>> {
    lista
        .iter()
        .map(|f| crate::catalogo_de_cameras::Candidata {
            id: &f.id,
            nome: &f.nome,
            monitor: f.especie == fontes::Especie::Monitor,
        })
        .collect()
}

/// O que uma enumeração do seletor devolve: as linhas escolhíveis e o ladrilho apagado da tela
/// estendida (R10).
struct ListaDoSeletor {
    fontes: Vec<Fonte>,
    tela_estendida_sem_driver: bool,
}

/// As linhas do seletor: os monitores, a tela estendida e, sem o `--sem-cameras`, as câmeras deste
/// PC — **depois** dos monitores, como no Mac. As câmeras do próprio Quall ficam de fora
/// (`catalogo_de_cameras`), e cada câmera, dentro ou fora, deixa uma linha no registro.
fn fontes_para_o_seletor(com_cameras: bool, camera_de_bancada: Option<&str>) -> ListaDoSeletor {
    let _ = (com_cameras, camera_de_bancada);
    let (lista, tela_estendida_sem_driver) = monitores_para_o_seletor();
    ListaDoSeletor { fontes: lista, tela_estendida_sem_driver }

}

/// Um catálogo de câmeras que leva mais que isto vai ao registro com `!!` (a revisão do código da
/// fase 5, M2). O normal medido é ~19 ms por leitura (M58).
const CATALOGO_LENTO: Duration = Duration::from_millis(250);

/// Uma thread de recarga do seletor por vez, e os pedidos que chegam com ela rodando viram **mais
/// uma volta** dela (a revisão curta do `08af2cd`, A3: antes, uma thread por `WM_DEVICECHANGE`,
/// enfileiradas numa trava).
static RECARGA_DAS_FONTES: crate::catalogo_de_cameras::Juntador = crate::catalogo_de_cameras::Juntador::novo();

/// A geração das enumerações do seletor (A2): tomada antes de enumerar.
static GERACAO_DAS_LISTAS: AtomicU64 = AtomicU64::new(0);

impl Emissor {
    /// **A recarga do seletor fora da thread da janela** (a revisão do código da fase 5, M2): o
    /// `WM_DEVICECHANGE` e o `WM_DISPLAYCHANGE` chegam na thread da janela, e a recarga enumera as
    /// câmeras pelo Media Foundation e lê o registro de cada uma. Um driver de câmera virtual ruim, um
    /// Frame Server preso ou uma câmera USB em mau estado não podem congelar a janela de um app que é,
    /// antes de tudo, de tela. A lista nova entra no estado sob a trava, como antes, e a janela a vê
    /// no pulso seguinte (a versão do estado sobe). Se a thread não subir, a recarga é feita aqui.
    pub fn recarregar_fontes_fora(self: &Arc<Self>, porque: &'static str) {
        // Com uma rodando, o pedido vira mais uma volta dela (A3).
        if !RECARGA_DAS_FONTES.pedir() {
            return;
        }
        let emissor = Arc::clone(self);
        let subiu = std::thread::Builder::new().name("quall-recarga-das-fontes".into()).spawn(move || {
            let com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
            // Um pânico no meio da volta solta a vez (a revisão do `ef71d20`, L1): sem a guarda, o
            // juntador ficava em "rodando" e o `WM_DEVICECHANGE` nunca mais recompunha o seletor.
            let _vez = RECARGA_DAS_FONTES.vez();
            let mut voltas = 0u32;
            loop {
                RECARGA_DAS_FONTES.comecar_volta();
                voltas += 1;
                let t0 = Instant::now();
                emissor.recarregar_fontes();
                let levou = t0.elapsed();
                if levou >= CATALOGO_LENTO {
                    registro::linha(format!(
                        "!! fontes: a recarga ({porque}, volta {voltas}) levou {} ms, fora da thread da janela",
                        levou.as_millis()
                    ));
                }
                if !RECARGA_DAS_FONTES.mais_uma_volta() {
                    break;
                }
            }
            if com.is_ok() {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
            }
        });
        if let Err(e) = subiu {
            registro::linha(format!("fontes: a thread da recarga não subiu ({e}); recarregando na thread da janela"));
            RECARGA_DAS_FONTES.soltar();
            self.recarregar_fontes();
        }
    }
}

/// A lista da abertura. Com `--camera-de-bancada` e `--fonte` igual a ela, espera o link entrar na
/// lista, até 10 s: a sonda pode ter criado a câmera um instante antes (§7.3, a revisão, 1).
fn esperar_a_camera_de_bancada(argumentos: &Argumentos) -> ListaDoSeletor {
    let lista = || fontes_para_o_seletor(argumentos.com_cameras(), argumentos.camera_de_bancada.as_deref());
    let (Some(link), Some(pedida)) = (argumentos.camera_de_bancada.as_deref(), argumentos.fonte.as_deref()) else {
        return lista();
    };
    if !link.eq_ignore_ascii_case(pedida) {
        return lista();
    }
    let comeco = Instant::now();
    let mut voltas = 0u32;
    loop {
        let l = lista();
        voltas += 1;
        if l.fontes.iter().any(|f| f.id.eq_ignore_ascii_case(link)) {
            registro::linha(format!("câmera de bancada: na lista em {} ms ({voltas} leitura(s) do catálogo)", comeco.elapsed().as_millis()));
            return l;
        }
        if voltas == 1 {
            registro::linha("câmera de bancada: ainda não está na lista; relendo o catálogo a cada 250 ms por até 10 s (o registro só diz o que mudar)");
        }
        if comeco.elapsed() >= Duration::from_secs(10) {
            registro::linha(format!(
                "câmera de bancada: não apareceu na lista em 10 s ({voltas} leituras do catálogo) — nada é escolhido"
            ));
            return l;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A câmera sintética da bancada (`--camera-sintetica`): a linha que a sessão transmite no lugar
/// de uma escolha.
fn fonte_da_camera_sintetica() -> Fonte {
    Fonte {
        id: ID_DA_CAMERA_SINTETICA.into(),
        nome: "Câmera sintética".into(), // i18n: fora (bancada)
        largura: 0,
        altura: 0,
        primario: false,
        especie: fontes::Especie::Camera,
    }
}

/// O id da câmera sintética: não é link de câmera nenhuma.
const ID_DA_CAMERA_SINTETICA: &str = "camera-sintetica";

/// Quall Monitor oferece somente a tela estendida. O bool indica o ladrilho sem driver.
fn monitores_para_o_seletor() -> (Vec<Fonte>, bool) {
    let mut lista: Vec<Fonte> = Vec::new();
    #[cfg(not(feature = "tela-estendida-futura"))]
    { return (lista, false); }
    #[cfg(feature = "tela-estendida-futura")]
    {
    // **A tela estendida** (`docs/monitor-virtual-windows.md` §14 e a nota de 02/10 no §7): um
    // monitor virtual novo para cada aparelho que entrar, pelo SudoVDA **que a pessoa instalou**. Com
    // o adaptador presente (o PnP diz; nada é aberto aqui) ela entra na lista, com ou sem
    // `--varias-sessoes`; sem ele, o ladrilho aparece apagado e fica fora da lista (R10, 02/10).
    use crate::regras_da_tela_estendida::{no_seletor, NoSeletor};
    match no_seletor(crate::monitores_virtuais::adaptador_presente()) {
        NoSeletor::Escolhivel => {
            lista.push(Fonte {
                id: crate::monitor::FONTE_TELA_ESTENDIDA.to_string(),
                // Em português sempre: o `reescolher` acha a fonte que voltou pelo nome, e o idioma
                // pode trocar no meio (a janela mostra o título da tela estendida, não este nome).
                nome: "Tela estendida (um monitor novo para cada aparelho)".to_string(), // i18n: chave
                largura: 0,
                altura: 0,
                primario: false,
                especie: fontes::Especie::TelaEstendida,
            });
            (lista, false)
        }
        NoSeletor::Apagada => (lista, true),
    }
    }
}

// =============================================================================================
// A câmera comum pelo dono da captura (`docs/teleprompter-com-camera.md` §8.10.3)
// =============================================================================================

/// **O que a câmera comum guarda enquanto a espera existe**: o dono (a câmera aberta com a espera, e
/// não com o pareamento), a gravação, a pasta e o recado dela.
struct CameraComum {
    dono: Arc<crate::dono_da_captura::DonoDaCaptura>,
    gravador: Option<Arc<crate::gravador_local::Gravador>>,
    pasta: Option<std::path::PathBuf>,
    recado: Option<(String, Instant)>,
    /// O desmonte começou: nada de gravação nova (a revisão do código, 4).
    fechando: bool,
}

/// A espera da câmera comum sem receptor e sem gravação desiste depois disto (a revisão do código, 8).
const ESPERA_OCIOSA: Duration = Duration::from_secs(10 * 60);

/// Quanto o recado da gravação ("salva em …", "não gravou: …") fica na tela.
const RECADO_DA_GRAVACAO: Duration = Duration::from_secs(15);

impl Emissor {
    /// **Os ajustes da câmera comum** (R9 §4.1) estão disponíveis agora? (A janela pinta o ícone.)
    pub fn ajustes_da_camera_disponiveis(&self) -> bool {
        self.ajustes_disponiveis.load(Ordering::SeqCst)
    }

    /// **R9b**: quem está controlando a câmera comum de longe ("Controlado por <aparelho>").
    pub fn camera_controlada_por(&self) -> Option<String> {
        self.controlada_por.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// **Pouca luz** na câmera comum (§3.1): o fps de agora e o pedido, para a linha da gravação.
    pub fn camera_com_pouca_luz(&self) -> Option<crate::regras_dos_controles::PoucaLuz> {
        *self.pouca_luz.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **Abre a janela "Ajustes da câmera"** da câmera comum, com a prévia dentro (§4.2).
    pub fn abrir_ajustes_da_camera(&self) {
        let dono = self.camera_comum.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|c| Arc::clone(&c.dono));
        match dono {
            Some(d) if d.ajustes().is_some() => crate::janela_dos_ajustes::abrir(d, true),
            _ => registro::linha("câmera comum: os ajustes da câmera ainda não estão prontos (a câmera abrindo, ou sem controles)"),
        }
    }

    /// Acorda a janela sem esperar o estado (a janela pinta com ele travado).
    fn acordador(self: &Arc<Self>) -> Box<dyn Fn() + Send + Sync> {
        let eu = Arc::clone(self);
        Box::new(move || {
            if let Ok(mut e) = eu.estado.try_lock() {
                e.mudou();
            }
        })
    }

    /// **O botão Gravar da câmera comum** (§8.10.3): começa ou para. Não liga o microfone — a pessoa o
    /// liga antes (Bruno, 24/09); sem ele, a tela diz "Gravando SEM SOM" por extenso.
    pub fn alternar_gravacao(self: &Arc<Self>) {
        let acordar = self.acordador();
        let mic = self.microfone.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut g = self.camera_comum.lock().unwrap_or_else(|e| e.into_inner());
        let Some(c) = g.as_mut() else { return };
        if c.fechando {
            return;
        }
        if let Some(gr) = &c.gravador {
            if !matches!(gr.estado().fase, crate::gravador_local::FaseDaGravacao::Fechada { .. }) {
                gr.parar("o botão da janela principal"); // i18n: fora (diário)
                let _ = c.dono.pendurar_gravador(None);
                if let Some(m) = &mic {
                    m.pendurar_gravador(None);
                }
                drop(g);
                self.estado().mudou();
                return;
            }
        }
        match crate::gravador_local::comecar_no_dono(&c.dono, c.pasta.clone(), "o botão da janela principal", acordar) { // i18n: fora (diário)
            Ok(gr) => {
                if let Some(m) = &mic {
                    m.pendurar_gravador(Some(gr.ramal_do_som()));
                }
                c.gravador = Some(gr);
                c.recado = None;
            }
            Err(m) => {
                registro::linha(format!("câmera comum: não gravou: {m}"));
                c.recado = Some((idioma::tf("Não gravou: {}", &[&m]), Instant::now()));
            }
        }
        drop(g);
        self.estado().mudou();
    }

    /// **A câmera comum pelo dono** (§8.10.3; o Android §8.6 e o iOS §8.8 fizeram o mesmo): a câmera
    /// abre **com a espera**, o receptor que pareia pendura a transmissão (a sessão de vídeo da tela
    /// R5, `teleprompter/camera.rs`), o que cai a solta e a espera volta **com o mesmo PIN**, e a
    /// gravação segue sem receptor. Acaba no Parar, na câmera que acabou, ou na espera que desistiu.
    fn correr_camera_comum(self: Arc<Self>, fonte: Fonte, pin_texto: String) {
        registro::prefixar_esta_thread("");
        let sintetica = fonte.id == ID_DA_CAMERA_SINTETICA;
        let fonte_da_camera = if sintetica {
            crate::captura_de_camera::FonteDaCamera::DoQuallNoProcesso { regua: true }
        } else {
            crate::captura_de_camera::FonteDaCamera::Link(fonte.id.clone())
        };
        // O teto de sempre da câmera comum (o alvo do núcleo com o fps pedido): o espelhamento comum
        // não muda de formato nem de carga (o iOS, §8.8, decidiu o mesmo).
        let alvo = quall_core::teto::Alvo::PADRAO;
        let teto = crate::regras_da_camera::TetoDaCamera { max_macroblocos: alvo.max_fs, fps: self.argumentos.fps.min(alvo.fps) };
        registro::linha(format!(
            "câmera comum pelo dono: \"{}\" abre com a espera (o receptor se pendura, e a gravação não depende dele)",
            fonte.nome
        ));
        let dono = crate::dono_da_captura::DonoDaCaptura::abrir(fonte_da_camera, fonte.nome.clone(), fonte.id.clone(), teto, self.acordador());
        let mic = crate::microfone::Microfone::novo(dono.origem, self.argumentos.microfone_tom, self.acordador());
        // Guardado antes de ler o botão: um toque no meio acha o microfone, ou esta leitura acha o toque.
        *self.microfone.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&mic));
        if self.microfone_pedido.load(Ordering::SeqCst) {
            mic.ligar("o botão já estava ligado ao começar"); // i18n: fora (diário)
        }
        let pasta = self.argumentos.pasta_das_gravacoes.clone().or_else(|| crate::gravador_local::pasta_padrao().ok());
        if let Some(p) = pasta.clone() {
            let _ = std::thread::Builder::new().name("quall.camera.pendentes".into()).spawn(move || {
                for l in crate::gravador_local::recuperar_pendentes(&p) {
                    registro::linha(format!("câmera comum, pendentes: {l}"));
                }
            });
        }
        *self.camera_comum.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(CameraComum { dono: Arc::clone(&dono), gravador: None, pasta, recado: None, fechando: false });
        let video = crate::teleprompter::camera::SessaoDeVideo::iniciar(
            Arc::clone(&dono),
            Arc::clone(&mic),
            crate::teleprompter::camera::ConfigDoVideo {
                porta: self.argumentos.porta,
                pin: Some(pin_texto.chars().filter(|c| c.is_ascii_digit()).collect()),
                pin_de_bancada: self.argumentos.pin.is_some(),
                anunciar: !self.argumentos.so_local,
                so_local: self.argumentos.so_local,
                fps: self.argumentos.fps,
            },
            self.acordador(),
        );
        let mut motivo = String::new();
        let mut transmitindo_antes = false;
        // **A bancada** (as bandeiras da tela R5, valendo aqui): o microfone só com o tom, o Gravar, o
        // parar e a morte gravando, contados da câmera aberta.
        let mut bancada = (0u8, 0u8);
        let mut aberta_em: Option<Instant> = None;
        let mut gravando_desde: Option<Instant> = None;
        let mut ajustes_abertos = false;
        // **A espera ociosa acaba** (a revisão do código, 8): a câmera (e o microfone) não ficam
        // abertos sem receptor e sem gravação para sempre.
        let mut ocioso_desde = Instant::now();
        loop {
            if self.parar.load(Ordering::SeqCst) {
                break;
            }
            {
                let e = self.estado();
                if e.gravando || e.fase == Fase::Transmitindo {
                    ocioso_desde = Instant::now();
                }
            }
            if ocioso_desde.elapsed() >= ESPERA_OCIOSA {
                motivo = idioma::tf("Ninguém entrou em {} minutos, e nada estava gravando: a câmera fechou. Clique em Espelhar de novo quando o outro aparelho estiver pronto.", &[&(ESPERA_OCIOSA.as_secs() / 60)]);
                break;
            }
            if aberta_em.is_none() && dono.fase() == crate::dono_da_captura::FaseDoDono::Aberto {
                aberta_em = Some(Instant::now());
            }
            // Os ajustes (R9): o ícone aparece quando a thread dos ajustes publicou que a câmera tem
            // controles (a sintética e a câmera do Quall nunca têm).
            let disponiveis = !sintetica
                && dono.ajustes().is_some_and(|p| p.fase() != crate::regras_dos_controles::FaseDosAjustes::SemControles);
            if self.ajustes_disponiveis.swap(disponiveis, Ordering::SeqCst) != disponiveis {
                self.estado().mudou();
            }
            // R9b: "Controlado por <aparelho>" na tela da câmera, do painel dos ajustes.
            let controlada = dono.ajustes().and_then(|p| p.painel_controlado_por());
            let mudou = {
                let mut g = self.controlada_por.lock().unwrap_or_else(|e| e.into_inner());
                let mudou = *g != controlada;
                *g = controlada;
                mudou
            };
            // Pouca luz (§3.1), do mesmo painel e do mesmo jeito.
            let pouca_luz = dono.ajustes().and_then(|p| p.painel_pouca_luz());
            let mudou_a_luz = {
                let mut g = self.pouca_luz.lock().unwrap_or_else(|e| e.into_inner());
                let mudou = *g != pouca_luz;
                *g = pouca_luz;
                mudou
            };
            if mudou || mudou_a_luz {
                self.estado().mudou();
            }
            if let Some(a) = aberta_em {
                self.bancada_da_camera_comum(a.elapsed().as_secs_f64(), &mut bancada, &mut gravando_desde);
            }
            // **Bancada, R9**: a janela "Ajustes da câmera" por argumento (`--abrir-ajustes-apos`).
            if let (Some(apos), Some(a), false) = (self.argumentos.abrir_ajustes_apos, aberta_em, ajustes_abertos) {
                if a.elapsed().as_secs_f64() >= apos && self.ajustes_disponiveis.load(Ordering::SeqCst) {
                    ajustes_abertos = true;
                    registro::linha("câmera comum: bancada: abrindo os ajustes da câmera (--abrir-ajustes-apos)");
                    self.abrir_ajustes_da_camera();
                }
            }
            match dono.fase() {
                crate::dono_da_captura::FaseDoDono::Falhou(f) | crate::dono_da_captura::FaseDoDono::Acabou(f) => {
                    motivo = f;
                    break;
                }
                _ => {}
            }
            let p = video.painel();
            if let crate::teleprompter::camera::FaseDoVideo::Parada(f) = &p.fase {
                motivo = f.clone();
                break;
            }
            let transmitindo = p.fase == crate::teleprompter::camera::FaseDoVideo::Transmitindo;
            if transmitindo != transmitindo_antes {
                transmitindo_antes = transmitindo;
                registro::linha(if transmitindo {
                    format!("câmera comum: transmitindo para {} (a câmera não reabriu)", p.par)
                } else {
                    "câmera comum: o receptor saiu; a espera volta com o mesmo PIN, e a câmera (e a gravação) seguem".to_string()
                });
            }
            self.publicar_camera_comum(&p, transmitindo);
            std::thread::sleep(Duration::from_millis(100));
        }
        // O fim, na ordem: a gravação fecha (o arquivo inteiro), o vídeo, o microfone, a câmera. **A
        // tela vê o desmonte** (a revisão do código, 4 e 6): Encerrando, o Gravar sem efeito, e
        // "Fechando o arquivo…" enquanto ele fecha.
        let gravador = {
            let mut g = self.camera_comum.lock().unwrap_or_else(|e| e.into_inner());
            g.as_mut().and_then(|c| {
                c.fechando = true;
                c.gravador.take()
            })
        };
        {
            let mut e = self.estado();
            e.fase = Fase::Encerrando;
            if gravador.is_some() {
                e.linha_da_gravacao = idioma::t("Fechando o arquivo da gravação…").into();
                // Em português: a legenda do Gravar o reconhece pelo começo (`legenda_do_gravar`).
                e.rotulo_gravar = "Fechando o arquivo…".into(); // i18n: chave
            }
            e.mudou();
        }
        if let Some(g) = gravador {
            g.parar(if motivo.is_empty() { "a transmissão da câmera foi encerrada" } else { "a câmera acabou" }); // i18n: fora (diário)
            let _ = dono.pendurar_gravador(None);
            mic.pendurar_gravador(None);
            let fim = Instant::now() + Duration::from_secs(15);
            while !g.terminou() && Instant::now() < fim {
                std::thread::sleep(Duration::from_millis(20));
            }
            if let crate::gravador_local::FaseDaGravacao::Fechada { arquivo, segundos, motivo: m, .. } = g.estado().fase {
                registro::linha(format!("câmera comum: a gravação fechou com a sessão ({m}): {:?} {segundos:.1} s", arquivo));
            }
        }
        video.pedir_parada();
        let fim = Instant::now() + Duration::from_secs(6);
        while !video.terminou() && Instant::now() < fim {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mic = self.microfone.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(m) = mic {
            m.desligar("a sessão de câmera acabou"); // i18n: fora (bancada e diário)
        }
        self.ajustes_disponiveis.store(false, Ordering::SeqCst);
        *self.controlada_por.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.pouca_luz.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // A janela dos ajustes solta a prévia antes de a câmera fechar.
        crate::janela_dos_ajustes::fechar_se_aberta(Duration::from_secs(4));
        dono.pedir_fechar();
        if !dono.esperar(Duration::from_secs(5)) {
            registro::linha("câmera comum: !! o dono da captura não fechou em 5 s");
        }
        *self.camera_comum.lock().unwrap_or_else(|e| e.into_inner()) = None;
        registro::linha(format!(
            "câmera comum: fechada ({}) — a câmera entregou {} quadros, maior buraco {} ms",
            if motivo.is_empty() { "Parar".to_string() } else { motivo.clone() },
            dono.entregues(),
            dono.buraco_maior().as_millis()
        ));
        if !motivo.is_empty() {
            self.estado().conselho = crate::regras_da_camera::so_a_frase(&motivo).to_string();
        }
        self.voltar_ao_inicio();
    }

    /// O que a janela lê da câmera comum: a fase, o PIN, o endereço, o par, e a gravação (o botão, o
    /// rótulo e a linha, com o "SEM SOM" por extenso).
    fn publicar_camera_comum(&self, p: &crate::teleprompter::camera::PainelDoVideo, transmitindo: bool) {
        use crate::gravador_local::FaseDaGravacao;
        // **O som é o do microfone aberto**, não o do botão (a revisão do código, 3).
        let m = self.microfone.lock().unwrap_or_else(|e| e.into_inner()).clone().map(|m| m.estado()).unwrap_or_default();
        let mic_ligado = m.aberto;
        // A linha nasce no idioma da hora; o "SEM SOM" (e o "NO AUDIO" do inglês) é o que a legenda do
        // Gravar procura: `regras_da_gravacao::diz_sem_som`.
        let sem_som = if m.aberto {
            String::new()
        } else if !m.frase.is_empty() {
            idioma::tf("Gravando SEM SOM — {}", &[&idioma::tr(&m.frase)])
        } else if m.ligado {
            idioma::t("Gravando SEM SOM — o microfone está abrindo").to_string()
        } else {
            idioma::t("Gravando SEM SOM — ligue o microfone").to_string()
        };
        let (rotulo, linha, gravando) = {
            let mut g = self.camera_comum.lock().unwrap_or_else(|e| e.into_inner());
            let Some(c) = g.as_mut() else { return };
            // A gravação que fechou sozinha (espaço, câmera, erro): solta, com o recado.
            if let Some(gr) = c.gravador.clone() {
                if let FaseDaGravacao::Fechada { arquivo, segundos, motivo, inteira } = gr.estado().fase {
                    let _ = c.dono.pendurar_gravador(None);
                    if let Some(m) = self.microfone.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                        m.pendurar_gravador(None);
                    }
                    c.gravador = None;
                    let t = match (arquivo, inteira) {
                        (Some(a), true) => idioma::tf("Gravação salva ({} s) em {}", &[&format!("{segundos:.0}"), &a.display()]),
                        (Some(_), false) => idioma::tf("A gravação fechou com defeito ({}); o arquivo é recuperado na próxima vez", &[&motivo]),
                        (None, _) => idioma::tf("Não gravou: {}", &[&motivo]),
                    };
                    registro::linha(format!("câmera comum: {t}"));
                    c.recado = Some((t, Instant::now()));
                }
            }
            match c.gravador.as_ref().map(|g| g.estado()) {
                Some(e) => match e.fase {
                    FaseDaGravacao::Gravando => {
                        let s = e.desde.map(|d| d.elapsed().as_secs()).unwrap_or(0);
                        let tempo = format!("{}:{:02}", s / 60, s % 60);
                        let mut l = idioma::tf("● GRAVANDO {}", &[&tempo]);
                        if let Some(livre) = e.livre {
                            l.push_str(&idioma::tf(" · sobram {} GB", &[&idioma::decimal(livre as f64 / (1024.0 * 1024.0 * 1024.0), 1)]));
                        }
                        if !mic_ligado {
                            l.push_str(" · ");
                            l.push_str(&sem_som);
                        }
                        // O rótulo é também o nome do botão para o Narrador; a legenda tira o tempo
                        // dos parênteses, que ficam nos dois idiomas.
                        (idioma::tf("■ Parar a gravação ({})", &[&tempo]), l, true)
                    }
                    // Os rótulos de abrir e fechar ficam em português (a legenda os reconhece pelo
                    // texto, `legenda_do_gravar`); a janela os traduz ao mostrar. A linha nasce traduzida.
                    FaseDaGravacao::Abrindo => ("Gravar (abrindo…)".to_string(), idioma::t("Abrindo o arquivo…").to_string(), true), // i18n: chave
                    FaseDaGravacao::Fechando => ("Fechando o arquivo…".to_string(), idioma::t("Fechando o arquivo…").to_string(), true), // i18n: chave
                    FaseDaGravacao::Fechada { .. } => (idioma::t("● Gravar").to_string(), String::new(), false),
                },
                None => {
                    let recado = c.recado.as_ref().filter(|(_, q)| q.elapsed() < RECADO_DA_GRAVACAO).map(|(t, _)| t.clone());
                    let rotulo = if mic_ligado { idioma::t("● Gravar neste computador") } else { idioma::t("● Gravar neste computador (sem som)") };
                    (rotulo.to_string(), recado.unwrap_or_default(), false)
                }
            }
        };
        let mut e = self.estado();
        if self.parar.load(Ordering::SeqCst) || e.fase == Fase::Encerrando {
            return;
        }
        let fase = if transmitindo { Fase::Transmitindo } else { Fase::Esperando };
        let pin = p.pin.clone();
        let mudou = e.fase != fase
            || e.pin != pin
            || e.oferece_desparear != p.oferece_desparear
            || e.endereco != p.endereco
            || e.par != p.par
            || e.resumo != p.resumo
            || e.anunciando_por_mdns != p.anunciando
            || e.rotulo_gravar != rotulo
            || e.linha_da_gravacao != linha
            || e.gravando != gravando
            || !e.camera_pelo_dono;
        if mudou {
            e.fase = fase;
            e.pin = pin;
            e.oferece_desparear = p.oferece_desparear;
            e.endereco = p.endereco.clone();
            e.par = p.par.clone();
            e.resumo = if transmitindo { p.resumo.clone() } else { p.aviso.clone() };
            e.anunciando_por_mdns = p.anunciando;
            e.rotulo_gravar = rotulo;
            e.linha_da_gravacao = linha;
            e.gravando = gravando;
            e.camera_pelo_dono = true;
            if transmitindo {
                e.ha_pares_conhecidos = true;
            }
            e.mudou();
        }
    }

    /// A bancada da câmera comum: `--microfone-apos` (só com `--microfone-tom`, `docs/audio.md` §8.1),
    /// `--gravar-apos`, `--gravar-por` e `--matar-gravando-apos`, pelo caminho dos botões.
    fn bancada_da_camera_comum(self: &Arc<Self>, s: f64, estado: &mut (u8, u8), gravando_desde: &mut Option<Instant>) {
        let a = &self.argumentos;
        if let Some(apos) = a.microfone_apos {
            if estado.0 == 0 && s >= apos {
                estado.0 = 1;
                if a.microfone_tom.is_some() {
                    self.alternar_microfone(true);
                } else {
                    registro::linha("câmera comum: !! --microfone-apos sem --microfone-tom: recusado (a bancada só liga o microfone com o tom)");
                }
            } else if estado.0 == 1 {
                if let Some(por) = a.microfone_por {
                    if s >= apos + por {
                        estado.0 = 2;
                        self.alternar_microfone(false);
                    }
                }
            }
        }
        if let Some(apos) = a.gravar_apos {
            if estado.1 == 0 && s >= apos {
                estado.1 = 1;
                registro::linha("câmera comum: bancada: gravar (--gravar-apos)");
                self.alternar_gravacao();
            }
        }
        let gravando = self.estado().gravando;
        if estado.1 == 1 && gravando && gravando_desde.is_none() {
            *gravando_desde = Some(Instant::now());
        }
        if let (1, Some(d)) = (estado.1, *gravando_desde) {
            if let Some(m) = a.matar_gravando_apos {
                if d.elapsed().as_secs_f64() >= m {
                    registro::linha("câmera comum: bancada: TerminateProcess agora, gravando (--matar-gravando-apos)");
                    unsafe {
                        let _ = windows::Win32::System::Threading::TerminateProcess(windows::Win32::System::Threading::GetCurrentProcess(), 9);
                    }
                }
            }
            if let Some(por) = a.gravar_por {
                if d.elapsed().as_secs_f64() >= por {
                    estado.1 = 2;
                    // Só se ainda grava: o botão começaria outra se ela já tivesse fechado sozinha.
                    if gravando {
                        registro::linha("câmera comum: bancada: parar a gravação (--gravar-por)");
                        self.alternar_gravacao();
                    }
                }
            }
        }
    }

    /// Espera a câmera comum fechar (a gravação inteira), até `prazo`: o `main` chama antes do
    /// `MFShutdown`, com a janela já fechada.
    pub fn esperar_a_camera_comum(&self, prazo: Duration) -> bool {
        let fim = Instant::now() + prazo;
        loop {
            let acabou = self.thread_da_camera_comum.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_none_or(|h| h.is_finished());
            if acabou {
                if let Some(h) = self.thread_da_camera_comum.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = h.join();
                }
                return true;
            }
            if Instant::now() >= fim {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
