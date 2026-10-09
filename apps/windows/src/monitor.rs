//! **O monitor de cada sessão**, atrás de uma abstração: criar para a tela do receptor, alimentar,
//! soltar, e saber que soltou.
//!
//! # Por que existe
//!
//! No Mac cada receptor da tela estendida ganha um monitor virtual no formato da tela dele
//! (`QuallCaptureKit/MonitorVirtual.swift`). No Windows o monitor virtual depende de um driver, e o
//! driver é outra frente (F2a: `docs/monitor-virtual-windows.md`). Esta abstração é o lugar onde
//! ele entra: o emissor com várias sessões pede um monitor por sessão por [`FonteDeMonitor`], e
//! nada acima dela sabe se o que veio é um monitor virtual, o monitor físico ou uma textura nossa.
//!
//! # As implementações
//!
//! - **`MonitoresVirtuais`** (`monitores_virtuais.rs`, 15/09/2026): um monitor do SudoVDA por
//!   sessão, no formato da tela do receptor, com a regra do §13.4 de `monitor-virtual-windows.md`
//!   cumprida item a item (a lista abaixo diz onde). **Só a bancada**: o SudoVDA não é o driver do
//!   produto (§4.3, pergunta aberta ao usuário).
//! - [`MonitoresFisicos`] (provisório): toda sessão captura o monitor escolhido na tela inicial —
//!   sem formato por receptor. **Nada a soltar**; a testemunha de que ele existe é o nome GDI, como
//!   em `emissor.rs` (serve para o físico, que a pessoa escolheu por esse nome).
//! - [`MonitoresSinteticos`]: toda sessão ganha uma origem sintética (`sintetica.rs`) **no formato
//!   da tela do receptor** — a da prova de mecanismo, na Sessão 0 e sem capturar tela.
//!
//! # O contrato do monitor virtual (F2a, §5.3, §7 e §13.4 de `monitor-virtual-windows.md`)
//!
//! Valha o driver que valer (o SudoVDA na bancada; um nosso, provavelmente sobre o
//! libvirtualdisplay, no produto). Onde cada item mora hoje, com o SudoVDA:
//!
//! - identidade estável → `sudovda::guid_da_identidade` e `sudovda::textos_do_edid` (ASCII, até 13);
//! - **um dono da topologia por processo** → `MonitoresVirtuais::do_processo`: um fio que faz, em
//!   ordem e numa fila (as solturas antes das criações), o `ADD` com a ativação, o `REMOVE` com a
//!   testemunha, o recolher do Parar e o mapa *alvo → nome GDI → `HMONITOR` → retângulo* com época;
//! - a placa uma vez (a primeira de Intel → outras → NVIDIA que anuncia H.264; no Dell, a Intel) →
//!   escolhida quando o `MonitoresVirtuais` do processo abre
//!   ([`FonteDeMonitor::placa_do_processo`]) e fixada pelo dono antes do primeiro `ADD`; a sessão
//!   abre o encoder e o dispositivo **daquele** LUID (`encoder::ativar_h264_na_placa`,
//!   `device::create_device_por_luid`) **antes** de o monitor existir, sobe a cadeia numa origem
//!   preta nossa — o receptor recebe quadro já, e não desiste nos 10 s sem quadro do Android e do
//!   iOS — e troca pela captura do monitor quando ele chega de um fio auxiliar. A ordem é placa →
//!   encoder → dispositivo → cadeia na origem preta → monitor → captura;
//! - a ativação limpa, na hora → a decisão em `ativacao.rs` (pura, com testes), a leitura e o pedido
//!   em `monitores_virtuais::ccd`;
//! - o vigia global → um fio de ping por processo, que só pinga com o dono vivo e o coordenador
//!   batendo ([`FonteDeMonitor::alimentar`]);
//! - soltar e testemunhar → `REMOVE` pelo GUID, caminho ativo e nó PnP; ninguém órfão (a guarda
//!   no `Drop` do monitor, o coordenador pela chave, o `encerrar_tudo` na saída);
//! - seguir o alvo → a sessão lê o mapa do dono e reabre a captura no `HMONITOR` novo
//!   (`ativacao::reabertura`), sem recriar os encoders; o mesmo `HMONITOR` em outro lugar não
//!   reabre (o item segue o monitor);
//! - a captura com prazo → `capture::pedir_captura` (o item, o pool e a sessão do WGC num fio
//!   auxiliar; nada do WGC no fio da sessão), o prazo contado com o dono parado, e o aviso do
//!   usuário ([`crate::sessoes::AVISO_DA_CAPTURA_PRESA`]);
//! - na bancada, o portão da janela sintética decide cada quadro na chegada, e o que passa é
//!   copiado para uma textura nossa antes de o buffer voltar ao pool (`capture::PortaoDeCobertura`);
//! - fora do seletor → [`e_nosso`], pelo mapa do dono (o par, nunca o nome GDI), sem ler o
//!   `DisplayConfig` no fio da janela.
//!
//! O contrato, como a F2a o escreveu:
//!
//! - **criar** com largura, altura, Hz, **tamanho físico em mm** — o Windows escolhe a escala por
//!   ele — e uma **identidade estável de 64 bits por aparelho** ([`PedidoDeMonitor::identidade`]).
//!   O SudoVDA põe os 32 bits de baixo dela no EDID, e o modo preferido é achado comparando o EDID
//!   inteiro: dois monitores com os mesmos 32 bits herdariam o modo um do outro. Por isso os 32 de
//!   baixo carregam o **índice** do aparelho (único entre os monitores vivos, `TabelaDeIndices`);
//! - desenhado pela **mesma placa** que captura e codifica: sem `SET_RENDER_ADAPTER`, quem escolhe
//!   é o Windows, e pode sair outra. Com o monitor virtual a fonte diz a placa antes de a cadeia
//!   abrir ([`FonteDeMonitor::cadeia_na_placa_do_processo`]) e o monitor a confirma
//!   (`MonitorDaSessao::placa`); as outras fontes seguem encoder → adaptador → captura;
//! - **alimentar**: o vigia do SudoVDA é global e tira **todos** os monitores 2–3 s depois do último
//!   pedido ao driver. Quem pinga é um fio próprio; o coordenador bate ([`FonteDeMonitor::alimentar`]);
//! - **soltar**, e **testemunhar** que saiu por reenumeração — o `Closed` do `GraphicsCaptureItem`
//!   não disparou em 3 s com monitor desconectado (`docs/app-windows.md`, item 10);
//! - casar pelo que o driver devolve — `TargetId` + `AdapterLuid` ([`AlvoDoWindows`]) — e **não**
//!   pelo `\\.\DISPLAYn`, que é posição de saída e pode passar de um monitor que saiu para um que
//!   entrou (revisão adversarial de 13/09/2026, 7b). O nome GDI sai desse par por
//!   `QueryDisplayConfig` (`fontes::nome_gdi_do_alvo`) e pode demorar até ~1,3 s;
//! - **pôr na área de trabalho qualquer que seja o Win+P da pessoa**, pela regra do §13.4 de
//!   `monitor-virtual-windows.md` (medida no Dell em 14/09/2026 com o Win+P em "somente a tela do
//!   PC", 220 de 220; "Duplicar" e "Estender" não medidos): o Windows, na
//!   chegada, às vezes põe o monitor novo em **clone** com a tela integrada (a entrada que ele
//!   mesmo grava no banco para cada EDID novo); o pedido tem de sair **na hora** — os caminhos
//!   ativos **do usuário** (disponíveis, não nossos, com os modos deles) + o nosso, sem
//!   `SDC_SAVE_TO_DATABASE` —, e, se o clone vier mesmo assim, desfazê-lo e estender. Nunca
//!   devolver à `SetDisplayConfig` o que o `QDC_ALL_PATHS` marca ativo sem filtrar. Por isso também
//!   a identidade estável por aparelho: cada EDID novo é uma entrada a mais, para sempre, no banco
//!   de vídeo da pessoa;
//! - **fora do seletor de fontes**: um monitor nosso não é origem que a pessoa escolha
//!   ([`e_nosso`]);
//! - **soltar com prazo**: se o monitor não sumir, a sessão sai assim mesmo, e o índice fica em
//!   quarentena (`sessoes.rs`) — esperar para sempre prenderia o aparelho que reconecta.
//!
//! **O dobro do fps** foi medido no Mac (`docs/tela-estendida.md`, "Sem o Sidecar, o monitor anda
//! na metade"); no Windows **não vale por analogia**. O pedido carrega os dois números, e o SudoVDA
//! já oferece cada modo também com o dobro da frequência — a frente do driver mede.

/// O `id` da fonte "Tela estendida" no seletor: não é um monitor, é o pedido de um monitor virtual
/// por aparelho (só com `--varias-sessoes` e o adaptador do SudoVDA presente, `emissor.rs`).
pub const FONTE_TELA_ESTENDIDA: &str = "quall:tela-estendida";

/// O que uma sessão pede: um monitor para a tela deste receptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PedidoDeMonitor {
    /// Os pixels do monitor, deitado (largura ≥ altura), pares.
    pub largura: u32,
    pub altura: u32,
    /// A taxa do monitor: o dobro do fps, até 120 (a regra do Mac — ver o cabeçalho).
    pub hertz: u32,
    /// O fps da transmissão feita dele.
    pub fps: u32,
    /// O tamanho físico declarado, em mm — é por ele que o Windows escolhe a escala.
    pub milimetros: (u32, u32),
    /// A identidade do monitor deste aparelho (`TabelaDeIndices`).
    pub indice: usize,
    /// **A identidade estável do monitor, 64 bits.** Os 32 de cima vêm do `device_id` do par; os 32
    /// de baixo — os que o SudoVDA grava no EDID — levam o índice no byte de cima e 24 bits do
    /// `device_id` embaixo: únicos entre os monitores vivos (os índices são), e os mesmos toda vez
    /// que o mesmo aparelho volta (o índice é gravado por aparelho).
    pub identidade: u64,
    /// "Quall — iPhone X".
    pub nome: String,
    /// O formato veio da tela que o receptor disse no aperto de mão (`Announcement::screen`).
    /// `false` é o formato de sempre, de quem não diz a tela (o Windows e o OBS como receptores).
    pub da_tela_do_par: bool,
}

impl PedidoDeMonitor {
    /// O formato de quem não diz a tela: o tablet da bancada deitado — o mesmo do Mac
    /// (`ModoDoMonitorVirtual.tabletDaBancada`).
    pub const PADRAO: (u32, u32) = (1920, 1200);

    /// O maior quadro do nível que o núcleo anuncia (5.2), em macroblocos — o mesmo número do Mac
    /// (`ModoDoMonitorVirtual.macroblocosDoNivel`). Acima dele o núcleo reduziria na codificação e
    /// o monitor sairia maior que o vídeo.
    pub const MACROBLOCOS_DO_NIVEL: u32 = 36_864;

    /// O menor monitor de trabalho: abaixo disto, o formato de sempre (Mac: `paraTela` devolve nil).
    pub const MENOR: (u32, u32) = (1280, 720);

    /// **A densidade declarada**, em pixels por polegada: a do Mac para o 2x (206 ppi, o painel do
    /// tablet — `ModoDoMonitorVirtual.milimetros`). Que escala o Windows escolhe com ela, para cada
    /// formato de receptor, **não foi medido** (pergunta B da sonda da F2a). É um número a trocar
    /// pela medida, e está num lugar só por isso.
    pub const PPI_DECLARADO: f64 = 206.0;

    /// **O monitor para a tela de um aparelho**: deitado, com os pixels do painel (pares), e os dois
    /// lados encolhidos na mesma proporção até caber no nível. Porte de
    /// `ModoDoMonitorVirtual.paraTela` sem a escala 2x/1x do macOS — aqui a escala sai do tamanho
    /// físico declarado.
    pub fn para_tela(
        tela: Option<(u32, u32)>,
        fps: u32,
        indice: usize,
        nome_do_par: &str,
        device_id: &str,
    ) -> Self {
        let fps = fps.clamp(1, 120);
        let nome = if nome_do_par.is_empty() {
            "Quall — tela estendida".to_string()
        } else {
            format!("Quall — {nome_do_par}")
        };
        let identidade = identidade_do_monitor(device_id, indice);
        let montar = |largura: u32, altura: u32, da_tela_do_par: bool| PedidoDeMonitor {
            largura,
            altura,
            hertz: (2 * fps).min(120),
            fps,
            milimetros: milimetros(largura, altura),
            indice,
            identidade,
            nome: nome.clone(),
            da_tela_do_par,
        };
        let Some((l, a)) = tela.filter(|(l, a)| *l > 0 && *a > 0) else {
            return montar(Self::PADRAO.0, Self::PADRAO.1, false);
        };
        let (mut largura, mut altura) = (l.max(a) & !1, l.min(a) & !1);
        if macroblocos(largura, altura) > Self::MACROBLOCOS_DO_NIVEL {
            let k = (f64::from(Self::MACROBLOCOS_DO_NIVEL) / f64::from(macroblocos(largura, altura))).sqrt();
            let proporcao = f64::from(altura) / f64::from(largura);
            largura = ((f64::from(largura) * k) as u32) & !1;
            altura = ((f64::from(largura) * proporcao) as u32) & !1;
            while macroblocos(largura, altura) > Self::MACROBLOCOS_DO_NIVEL && largura > 16 {
                largura -= 16;
                altura = ((f64::from(largura) * proporcao) as u32) & !1;
            }
        }
        if largura < Self::MENOR.0 || altura < Self::MENOR.1 {
            return montar(Self::PADRAO.0, Self::PADRAO.1, false);
        }
        montar(largura, altura, true)
    }

    /// "1920 × 1200 @60 Hz para 30 fps, 237 × 148 mm, índice 2 (da tela do par)" — para o registro.
    pub fn descricao(&self) -> String {
        format!(
            "{} × {} @{} Hz para {} fps, {} × {} mm, índice {}, identidade {:016x} ({})",
            self.largura,
            self.altura,
            self.hertz,
            self.fps,
            self.milimetros.0,
            self.milimetros.1,
            self.indice,
            self.identidade,
            if self.da_tela_do_par { "da tela do par" } else { "formato de sempre: o par não disse a tela" }
        )
    }
}

fn macroblocos(largura: u32, altura: u32) -> u32 {
    largura.div_ceil(16) * altura.div_ceil(16)
}

fn milimetros(largura: u32, altura: u32) -> (u32, u32) {
    let mm = |px: u32| (f64::from(px) * 25.4 / PedidoDeMonitor::PPI_DECLARADO).round() as u32;
    (mm(largura).max(1), mm(altura).max(1))
}

/// FNV-1a de 64 bits: estável entre execuções e plataformas (o `DefaultHasher` do Rust não promete
/// ser), e sem dependência nova.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A identidade de 64 bits do monitor de um aparelho — ver [`PedidoDeMonitor::identidade`].
pub fn identidade_do_monitor(device_id: &str, indice: usize) -> u64 {
    let h = fnv1a64(device_id.as_bytes());
    let de_cima = h >> 32;
    let de_baixo = ((indice as u64 & 0xFF) << 24) | (h & 0x00FF_FFFF);
    (de_cima << 32) | de_baixo
}

/// Como terminou o soltar de um monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Soltura {
    /// Não havia o que soltar (o monitor físico; a origem sintética, que morre com a cadeia).
    NadaASoltar,
    /// Soltou, e a testemunha confirmou em `ms`.
    Confirmada { ms: u64 },
    /// O prazo venceu sem a testemunha confirmar. A sessão sai assim mesmo.
    NaoConfirmada { prazo_ms: u64 },
}

impl Soltura {
    pub fn confirmou(&self) -> bool {
        !matches!(self, Soltura::NaoConfirmada { .. })
    }
}

/// Onde o Windows põe um monitor virtual: o par que o driver devolve ao criá-lo (`ADD` do SudoVDA).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlvoDoWindows {
    /// O `LUID` do adaptador, com `HighPart` nos 32 bits de cima.
    pub adapter_luid: u64,
    pub target_id: u32,
}

// --- os monitores nossos, fora do seletor --------------------------------------------------------

/// Este monitor (pelo nome GDI que o seletor mostra) é um monitor virtual nosso? A pergunta é pelo
/// nome, porque o seletor lista nomes; **a resposta não é**: ela vem do mapa do dono da topologia,
/// que resolve cada alvo nosso (o par `AdapterLuid + TargetId`) no nome GDI de agora
/// (`monitores_virtuais::gdi_e_nosso`). O nome GDI de um monitor nosso muda quando outro chega (E5,
/// §13.5), então um registro por nome ficaria errado; e ler o `DisplayConfig` aqui, no fio da janela,
/// a prenderia atrás da `SetDisplayConfig` de quem chega (a revisão de 15/09, item 15). Sem nenhum
/// `ADD` neste processo, `false` na hora, sem ler nada: o caminho de uma sessão só não muda.
#[cfg(all(windows, feature = "tela-estendida-futura"))]
pub fn e_nosso(nome_gdi: &str) -> bool {
    crate::monitores_virtuais::gdi_e_nosso(nome_gdi)
}

#[cfg(windows)]
pub use janelas::*;

#[cfg(windows)]
mod janelas {
    use std::time::{Duration, Instant};

    use super::{AlvoDoWindows, PedidoDeMonitor, Soltura};
    use crate::fontes::{self, Fonte};
    use crate::sintetica::{Carga, Ritmo};

    /// De onde a cadeia de uma sessão tira os quadros.
    #[derive(Clone, Debug)]
    pub enum OrigemDoMonitor {
        /// Um monitor que o Windows enumera e a pessoa escolheu pelo nome: o físico.
        Monitor(Fonte),
        Sintetico { largura: u32, altura: u32, fps: u32, carga: Carga, ritmo: Ritmo },
        /// **Um monitor virtual nosso.** A captura segue o `alvo`, não o nome GDI de `fonte` — que é
        /// só o do nascimento, e muda quando outro monitor chega (E5).
        #[cfg(feature = "tela-estendida-futura")]
        Virtual { alvo: AlvoDoWindows, fonte: Fonte },
        /// **Uma câmera** (`docs/camera-no-windows.md`, fase 3): toda sessão abre a mesma câmera. A
        /// segunda abre compartilhada, pelo recuo da captura (`captura_de_camera.rs`).
        Camera { fonte: Fonte, origem: crate::captura_de_camera::FonteDaCamera },
    }

    /// Como saber que o monitor ainda está lá (e que saiu).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum TestemunhaDoMonitor {
        /// Pelo nome GDI (`\\.\DISPLAYn`): serve para o monitor físico que a pessoa escolheu por
        /// esse nome. **Não** serve para um monitor nosso — ver o cabeçalho.
        PorNomeGdi(String),
        /// Pelo alvo que o driver devolveu — o monitor virtual.
        #[cfg(feature = "tela-estendida-futura")]
        PorAlvo(AlvoDoWindows),
        /// Pela interface da câmera habilitada (`cameras::interface_habilitada`), pelo link, com a
        /// memória de já ter lido "habilitada": `None` depois disso é sumiço (m1).
        PorInterfaceDeCamera(String, MemoriaDaInterface),
        /// A origem sintética: não há o que testemunhar.
        Nenhuma,
    }

    /// A testemunha da interface (`regras_da_camera::TestemunhaDaInterface`: "já li habilitada" e as
    /// ausências seguidas), dividida entre as cópias da testemunha. Toda memória é igual para
    /// `PartialEq`: a testemunha é a mesma pelo link.
    #[derive(Clone, Debug, Default)]
    pub struct MemoriaDaInterface(std::sync::Arc<std::sync::Mutex<crate::regras_da_camera::TestemunhaDaInterface>>);

    impl PartialEq for MemoriaDaInterface {
        fn eq(&self, _: &Self) -> bool {
            true
        }
    }

    impl Eq for MemoriaDaInterface {}

    impl TestemunhaDoMonitor {
        pub fn presente(&self) -> bool {
            match self {
                TestemunhaDoMonitor::PorNomeGdi(id) => fontes::achar_hmonitor(id).is_some(),
                #[cfg(feature = "tela-estendida-futura")]
                TestemunhaDoMonitor::PorAlvo(a) => fontes::nome_gdi_do_alvo(a.adapter_luid, a.target_id).is_some(),
                // `None` antes de ler "habilitada" (não deu para perguntar) não derruba a sessão;
                // cinco `None` seguidos depois dela são o nó que saiu (M36; a revisão do código da
                // fase 3, m1, e a reconferência: um `None` passageiro não encerra).
                TestemunhaDoMonitor::PorInterfaceDeCamera(link, memoria) => {
                    let leitura = crate::cameras::interface_habilitada(link);
                    memoria.0.lock().unwrap_or_else(|e| e.into_inner()).presente(leitura)
                }
                TestemunhaDoMonitor::Nenhuma => true,
            }
        }

        /// A interface da câmera já foi lida habilitada? `true` em toda testemunha que não é de
        /// câmera (a pausa sem teto é só da câmera cuja interface nunca foi confirmada).
        pub fn interface_confirmada(&self) -> bool {
            match self {
                TestemunhaDoMonitor::PorInterfaceDeCamera(_, memoria) => {
                    memoria.0.lock().unwrap_or_else(|e| e.into_inner()).ja_habilitada()
                }
                _ => true,
            }
        }
    }

    /// O monitor de uma sessão, enquanto ela existir.
    pub struct MonitorDaSessao {
        pub origem: OrigemDoMonitor,
        pub pedido: PedidoDeMonitor,
        pub testemunha: TestemunhaDoMonitor,
        /// O que foi de fato criado, para o registro.
        pub descricao: String,
        /// A placa (LUID) em que a cadeia desta sessão tem de abrir o encoder e o dispositivo: a
        /// que desenha o monitor. `None` nas fontes que deixam a cadeia escolher.
        pub placa: Option<u64>,
        /// O que só o monitor virtual tem (a ficha de posse, o mapa do dono, o portão da
        /// cobertura). `None` nas outras fontes.
        #[cfg(feature = "tela-estendida-futura")]
        pub virtual_: Option<crate::monitores_virtuais::Vivo>,
    }

    /// O que a sessão dá a quem cria o monitor: o Parar (para cancelar sem esperar os 8 s da
    /// ativação) e o batimento (a espera na fila do dono não é uma sessão presa).
    pub struct ContextoDoCriar<'a> {
        pub parar: &'a std::sync::atomic::AtomicBool,
        pub bater: &'a dyn Fn(),
    }

    impl MonitorDaSessao {
        /// O monitor continua lá?
        pub fn ainda_existe(&self) -> bool {
            self.testemunha.presente()
        }

        /// A interface da câmera já foi lida habilitada ([`TestemunhaDoMonitor::interface_confirmada`])?
        pub fn interface_confirmada(&self) -> bool {
            self.testemunha.interface_confirmada()
        }
    }

    /// Quem cria, alimenta e solta o monitor de cada sessão.
    pub trait FonteDeMonitor: Send + Sync {
        /// Para o registro: "monitor físico \\.\DISPLAY1 (provisório)".
        fn descricao(&self) -> String;

        /// Cria o monitor de `pedido`, desenhado pela placa `placa` (o LUID do adaptador que
        /// captura e codifica — `SET_RENDER_ADAPTER` no SudoVDA). `None` quando a fonte não precisa
        /// da placa ([`FonteDeMonitor::precisa_da_placa`]).
        fn criar(&self, pedido: &PedidoDeMonitor, placa: Option<u64>) -> Result<MonitorDaSessao, String>;

        /// Como [`FonteDeMonitor::criar`], com o Parar e o batimento da sessão. As fontes que não
        /// esperam nada ignoram o contexto.
        fn criar_com(&self, pedido: &PedidoDeMonitor, _ctx: &ContextoDoCriar<'_>) -> Result<MonitorDaSessao, String> {
            self.criar(pedido, None)
        }

        /// **A ordem da sessão com monitor virtual**: a placa do processo (fixada uma vez), o encoder
        /// e o dispositivo dela, a cadeia numa origem preta nossa, e o monitor num fio auxiliar — a
        /// captura dele entra no lugar da preta quando ele chega (a revisão de 15/09, itens B e 7).
        /// As outras fontes seguem encoder → adaptador → captura.
        fn cadeia_na_placa_do_processo(&self) -> bool {
            false
        }

        /// A fonte é uma câmera: a track sai com a espécie `Camera`, sem som, e o anúncio diz
        /// `camera_source`.
        fn e_camera(&self) -> bool {
            false
        }

        /// A placa (LUID) em que o monitor virtual é desenhado, fixada uma vez no processo — a
        /// sessão abre o encoder e o dispositivo nela **antes** de o monitor nascer, para mandar
        /// quadro preto ao receptor enquanto espera a fila do dono (a revisão, item 7). `None` nas
        /// outras fontes.
        fn placa_do_processo(&self) -> Option<Result<u64, String>> {
            None
        }

        /// O batimento do coordenador, a cada volta do laço dele. **Não** é o sinal de vida do
        /// SudoVDA: esse é um fio próprio, que só pinga com este batimento recente (um coordenador
        /// travado não pode manter monitores de pé).
        fn alimentar(&self) {}

        /// Solta o monitor e espera a testemunha, com prazo ([`confirmar_ausencia`]).
        fn soltar(&self, _monitor: &MonitorDaSessao, _prazo: Duration) -> Soltura {
            Soltura::NadaASoltar
        }

        /// O Parar: tira os monitores da área de trabalho de uma vez, antes de cada sessão soltar o
        /// seu (em vez de o Windows passar por um conjunto intermediário a cada saída).
        fn recolher(&self) {}

        /// O coordenador solta um monitor pela chave (o GUID), quando a sessão dele não confirmou,
        /// sumiu sem batimento ou passou do prazo do Parar.
        fn soltar_pela_chave(&self, _chave: u128) {}

        /// A saída do processo: solta tudo o que sobrou, com prazo. Devolve quantos.
        fn encerrar_tudo(&self, _prazo: Duration) -> usize {
            0
        }
    }

    /// **Provisório**: toda sessão captura o monitor escolhido na tela inicial.
    pub struct MonitoresFisicos {
        pub fonte: Fonte,
    }

    impl FonteDeMonitor for MonitoresFisicos {
        fn descricao(&self) -> String {
            format!("monitor físico {} ({}) — provisório, sem formato por receptor", self.fonte.id, self.fonte.nome)
        }

        fn criar(&self, pedido: &PedidoDeMonitor, _placa: Option<u64>) -> Result<MonitorDaSessao, String> {
            if fontes::achar_hmonitor(&self.fonte.id).is_none() {
                return Err(format!("o monitor \"{}\" não está mais conectado", self.fonte.nome));
            }
            Ok(MonitorDaSessao {
                origem: OrigemDoMonitor::Monitor(self.fonte.clone()),
                pedido: pedido.clone(),
                testemunha: TestemunhaDoMonitor::PorNomeGdi(self.fonte.id.clone()),
                descricao: format!(
                    "{} ({} {}x{}) no lugar de um monitor {} — provisório até o driver",
                    self.fonte.id, self.fonte.nome, self.fonte.largura, self.fonte.altura,
                    pedido.descricao()
                ),
                placa: None,
                #[cfg(feature = "tela-estendida-futura")]
                virtual_: None,
            })
        }
    }

    /// **A câmera, em toda sessão** (`docs/camera-no-windows.md`, fase 3): a escolhida na tela inicial
    /// (pelo link), ou a fonte do Quall no processo com `--camera-sintetica` (bancada). Não há formato
    /// por receptor: a câmera tem o dela, e o teto do núcleo decide o que sai.
    pub struct CamerasDaSessao {
        pub fonte: Fonte,
        pub origem: crate::captura_de_camera::FonteDaCamera,
    }

    impl FonteDeMonitor for CamerasDaSessao {
        fn descricao(&self) -> String {
            format!("{} — {}", self.fonte.nome, self.origem.descricao())
        }

        fn e_camera(&self) -> bool {
            true
        }

        fn criar(&self, pedido: &PedidoDeMonitor, _placa: Option<u64>) -> Result<MonitorDaSessao, String> {
            let testemunha = match &self.origem {
                crate::captura_de_camera::FonteDaCamera::Link(link) => {
                    if crate::cameras::interface_habilitada(link) == Some(false) {
                        return Err(format!("a câmera \"{}\" não está mais conectada", self.fonte.nome));
                    }
                    TestemunhaDoMonitor::PorInterfaceDeCamera(link.clone(), MemoriaDaInterface::default())
                }
                crate::captura_de_camera::FonteDaCamera::DoQuallNoProcesso { .. } => TestemunhaDoMonitor::Nenhuma,
            };
            Ok(MonitorDaSessao {
                origem: OrigemDoMonitor::Camera { fonte: self.fonte.clone(), origem: self.origem.clone() },
                pedido: pedido.clone(),
                testemunha,
                descricao: format!("{} (a câmera tem o formato dela; o pedido era {})", self.descricao(), pedido.descricao()),
                placa: None,
                #[cfg(feature = "tela-estendida-futura")]
                virtual_: None,
            })
        }
    }

    /// **Provisório, e o da prova**: toda sessão ganha uma origem sintética no formato pedido.
    pub struct MonitoresSinteticos {
        pub carga: Carga,
        pub ritmo: Ritmo,
    }

    impl FonteDeMonitor for MonitoresSinteticos {
        fn descricao(&self) -> String {
            format!("origem sintética carga={:?} ritmo={:?} — nenhuma tela é capturada", self.carga, self.ritmo)
        }

        fn criar(&self, pedido: &PedidoDeMonitor, _placa: Option<u64>) -> Result<MonitorDaSessao, String> {
            Ok(MonitorDaSessao {
                origem: OrigemDoMonitor::Sintetico {
                    largura: pedido.largura,
                    altura: pedido.altura,
                    fps: pedido.fps,
                    carga: self.carga,
                    ritmo: self.ritmo,
                },
                pedido: pedido.clone(),
                testemunha: TestemunhaDoMonitor::Nenhuma,
                descricao: format!("sintético {}", pedido.descricao()),
                placa: None,
                #[cfg(feature = "tela-estendida-futura")]
                virtual_: None,
            })
        }
    }

    /// **A testemunha que funcionou no Windows**: reenumerar até `presente` dizer que o monitor saiu,
    /// com prazo. O monitor virtual solta e confirma com isto, pelo alvo (`TestemunhaDoMonitor::PorAlvo`).
    pub fn confirmar_ausencia(presente: impl Fn() -> bool, prazo: Duration) -> Soltura {
        let comeco = Instant::now();
        loop {
            if !presente() {
                return Soltura::Confirmada { ms: comeco.elapsed().as_millis() as u64 };
            }
            if comeco.elapsed() >= prazo {
                return Soltura::NaoConfirmada { prazo_ms: prazo.as_millis() as u64 };
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// **O nome GDI de um monitor recém-criado**, pelo alvo que o driver devolveu: ele pode demorar
    /// a aparecer (o Apollo espera 20 ms e vai dobrando, e desiste com ~1,3 s somados —
    /// `docs/monitor-virtual-windows.md` §7). Mesmo molde aqui.
    pub fn esperar_nome_gdi(alvo: AlvoDoWindows) -> Option<String> {
        let mut espera = Duration::from_millis(20);
        let mut total = Duration::ZERO;
        loop {
            if let Some(nome) = fontes::nome_gdi_do_alvo(alvo.adapter_luid, alvo.target_id) {
                return Some(nome);
            }
            if total >= Duration::from_millis(1_300) {
                return None;
            }
            std::thread::sleep(espera);
            total += espera;
            espera = (espera * 2).min(Duration::from_millis(640));
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn p(l: u32, a: u32) -> PedidoDeMonitor {
        PedidoDeMonitor::para_tela(Some((l, a)), 30, 0, "x", "aparelho-x")
    }

    #[test]
    fn os_formatos_da_bancada_do_mac() {
        // Tablet, iPad e S24 cabem no nível e ficam como são (deitados).
        assert_eq!((p(1200, 1920).largura, p(1200, 1920).altura), (1920, 1200));
        assert_eq!((p(1640, 2360).largura, p(1640, 2360).altura), (2360, 1640));
        assert_eq!((p(1440, 3120).largura, p(1440, 3120).altura), (3120, 1440));
        // iPhone X: uma linha a menos, para ser par.
        assert_eq!((p(1125, 2436).largura, p(1125, 2436).altura), (2436, 1124));
        assert!(p(1125, 2436).da_tela_do_par);
    }

    #[test]
    fn tela_maior_que_o_nivel_encolhe_nos_dois_lados_na_mesma_proporcao() {
        let p = PedidoDeMonitor::para_tela(Some((5120, 2880)), 30, 0, "Mac 5K", "mac");
        assert!(macroblocos(p.largura, p.altura) <= PedidoDeMonitor::MACROBLOCOS_DO_NIVEL);
        assert_eq!((p.largura, p.altura), (4096, 2304));
        assert_eq!(p.largura % 2, 0);
        assert_eq!(p.altura % 2, 0);
    }

    #[test]
    fn sem_tela_ou_tela_pequena_e_o_formato_de_sempre() {
        let sem = PedidoDeMonitor::para_tela(None, 30, 3, "", "");
        assert_eq!((sem.largura, sem.altura), PedidoDeMonitor::PADRAO);
        assert!(!sem.da_tela_do_par);
        assert_eq!(sem.nome, "Quall — tela estendida");
        let pequena = PedidoDeMonitor::para_tela(Some((800, 600)), 30, 0, "velho", "v");
        assert_eq!((pequena.largura, pequena.altura), PedidoDeMonitor::PADRAO);
        assert!(!pequena.da_tela_do_par);
    }

    #[test]
    fn o_monitor_pede_o_dobro_do_fps_ate_120() {
        assert_eq!(PedidoDeMonitor::para_tela(None, 30, 0, "", "").hertz, 60);
        assert_eq!(PedidoDeMonitor::para_tela(None, 60, 0, "", "").hertz, 120);
        assert_eq!(PedidoDeMonitor::para_tela(None, 90, 0, "", "").hertz, 120);
    }

    #[test]
    fn o_tamanho_fisico_declarado_sai_da_densidade_do_mac() {
        // O tablet a 206 ppi: os 237 × 148 mm do painel real (`ModoDoMonitorVirtual.milimetros`).
        assert_eq!(p(1200, 1920).milimetros, (237, 148));
    }

    #[test]
    fn a_identidade_e_estavel_por_aparelho_e_os_32_de_baixo_nao_se_repetem_entre_vivos() {
        let a = identidade_do_monitor("iphone-x", 1);
        assert_eq!(a, identidade_do_monitor("iphone-x", 1), "a mesma toda vez");
        // Oito monitores vivos têm oito índices diferentes: os 32 de baixo nunca coincidem, mesmo
        // com aparelhos cujos 24 bits do hash coincidissem.
        let de_baixo: std::collections::BTreeSet<u32> =
            (0..8).map(|i| identidade_do_monitor("mesmo-hash", i) as u32).collect();
        assert_eq!(de_baixo.len(), 8);
        // Aparelhos diferentes no mesmo índice diferem nos 32 de cima.
        assert_ne!(identidade_do_monitor("tablet", 0) >> 32, identidade_do_monitor("s24", 0) >> 32);
    }

    #[cfg(all(windows, feature = "tela-estendida-futura"))]
    #[test]
    fn sem_monitor_virtual_criado_nenhum_monitor_e_nosso() {
        // O processo de teste nunca mandou um ADD: a resposta é `false` na hora, sem ler o
        // `DisplayConfig` — é o que garante o caminho de uma sessão só igual ao de antes.
        assert!(!e_nosso("\\\\.\\DISPLAY1"));
        assert!(!e_nosso("\\\\.\\DISPLAY9"));
    }

    #[cfg(all(windows, feature = "tela-estendida-futura"))]
    #[test]
    fn o_nosso_se_reconhece_pelo_caminho_do_monitor_e_nao_pelo_nome() {
        use crate::monitores_virtuais::ccd::caminho_do_sudovda;
        assert!(caminho_do_sudovda(r"\\?\DISPLAY#SMKD1CE#5&2b5e3a1&0&UID256#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}"));
        assert!(caminho_do_sudovda(r"\\?\display#smkd1ce#x"));
        assert!(!caminho_do_sudovda(r"\\?\DISPLAY#LGD05F2#4&1ab2c996&0&UID265988#{e6f07b5f}"));
        assert!(!caminho_do_sudovda(""));
    }
}
