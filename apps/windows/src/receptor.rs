//! A máquina de estados do **receptor** Windows: entrar numa sessão de outro aparelho e exibir.
//!
//! É a irmã de `emissor.rs`, e a assimetria entre as duas é imposta pelo protocolo, não por gosto.
//!
//! # Quem exibe é quem toca em "conectar" — e para este lado a dívida 1 é invisível
//!
//! `tracks` só vale em `hospedar` e não há renegociação (dívida 1): quem espelha anuncia e espera,
//! quem exibe escolhe na lista e conecta. Do lado do emissor isso exigiu discurso ("é assim que os
//! outros te encontram", nunca "escolha para onde mandar"). **Deste lado não exige nada**: tocar
//! num nome numa lista é exatamente o gesto que Chromecast e AirPlay já ensinaram, e é o gesto que
//! o `PROMPT.md` descreve. A dívida 1 não aparece aqui.
//!
//! # A mesma regra do `Ready` numa thread só, pelo mesmo motivo
//!
//! `docs/divida-do-nucleo.md` fixa a regra de plataforma que é do Windows e de mais ninguém: a
//! exceção da libdatachannel sobe de dentro do `lock_guard` do mutex global **sem soltá-lo**, e a
//! chamada seguinte trava o processo para sempre. Aqui a tradução é a mesma que `emissor.rs` faz e
//! ela é verificável por leitura: o `Ready` — e com ele a `Session`, o `Link` e as tracks — vive
//! numa variável só, na thread da sessão, e o laço de decode roda **nessa mesma thread**.
//!
//! O que **atravessa** a fronteira é a fatia do quadro, e ela atravessa como `Vec<u8>` copiado
//! dentro do tratador, nunca como handle. Isso não é escolha estética: `ao_receber_quadro` roda
//! numa thread da libdatachannel, a fatia vale só durante a chamada, e decodificar ali dentro
//! seguraria a recepção da sessão inteira, RTCP incluído.
//!
//! # Duas entradas, e a segunda é obrigatória
//!
//! mDNS **e** endereço digitado. O `PROMPT.md` fixa o fallback por IP como *obrigatório*, não
//! opcional — "rede com multicast bloqueado ou AP isolation, senão vira ticket de suporte". Nesta
//! própria bancada as quatro corridas do emissor conectaram todas por IP: `mdns=true` só disse que
//! o anúncio subiu.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};

use crate::idioma::{t, tf, tr};

use quall_core::cancel::Cancelamento;
use quall_core::discovery::{anuncio, endereco_manual};
use quall_core::error::Error;
use quall_core::pairing::{PairedPeers, Pin};
use quall_core::protocol::Capabilities;
use quall_core::rtp::Contadores;
use quall_core::session::{conectar, EventoDeSessao, SessionConfig};
use quall_core::signaling::RelatoDoEnlace;
use quall_core::track::{QuadroCodificado, TrackReceptor};
use quall_core::transport::TransportConfig;

use crate::argumentos::Argumentos;
use crate::cadeia::Condenacao;
use crate::camera_remota::ControleRemoto;
use crate::descoberta::{Aparelho, Busca};
use crate::exibicao::{Exibicao, QuadroRecebido};
use crate::identidade;
use crate::janela_do_enlace::{self, Acumulados, JanelaDoEnlace};
use crate::registro;
use crate::som_puxado::VontadeDoSom;
use crate::tocador::{ControleDoSom, Tocador};
use crate::sps;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FaseDoReceptor {
    /// A metade "Exibir" da tela inicial: lista, endereço e PIN.
    Parado,
    Conectando,
    Exibindo,
    Encerrando,
}

/// Tudo o que a janela lê deste lado. Publicado, nunca consultado ao vivo — mesma regra do
/// `Estado` do emissor.
pub struct EstadoDoReceptor {
    pub fase: FaseDoReceptor,
    pub versao: u64,
    pub aparelhos: Vec<Aparelho>,
    /// Sobe quando o **conteúdo** da lista muda. A janela remonta o controle só nessa hora.
    pub revisao_dos_aparelhos: u64,
    /// Índice na lista, ou `None` quando ninguém foi escolhido (ou o escolhido sumiu da rede).
    pub escolhido: Option<usize>,
    pub procurando: bool,
    pub aviso_da_busca: String,
    /// Para onde estamos indo, como texto — para a tela de conexão não dizer "conectando" sem
    /// dizer a quê.
    pub destino: String,
    pub par: String,
    pub conselho: String,
    pub resumo: String,
    /// **Os cinco contadores da cadeia de referência, na tela** — `rupturas`, `suspeitos`,
    /// `pior rajada`, `retidos` e `sem_referencia_ms`.
    ///
    /// `docs/contrato-track.md` tem uma cláusula só para isto, e ela nasceu de uma frase: em
    /// 31/08/2026, com os contadores já medindo certo na linha de relato do receptor iOS, o usuário
    /// olhou para o iPad no meio de uma corrida com **124 quadros suspeitos** e disse *"continua
    /// falhando e 0 falhas"*. Ele estava certo — a queixa que originou a medida sempre foi sobre a
    /// **tela**. Uma casca que publica os contadores só onde uma ferramenta os lê não cumpre o
    /// contrato.
    pub cadeia: String,
    /// `suspeitos > 0`: a janela pinta [`Self::cadeia`] em destaque. Mesma regra do
    /// `.foregroundColor(painel.suspeitos > 0 ? .orange : .primary)` do receptor iOS.
    pub cadeia_alerta: bool,
    /// A volta ao PIN: a janela põe o foco no campo e o destaca. Ver [`Receptor::ao_falhar`].
    pub pede_pin: bool,
    pub oferece_desparear: bool,
    /// **A linha do som, na tela** (S6): "som: tocando, volume 100 %", "som: mudo: a câmera do
    /// Quall está em uso", "sem som: …". Vazia sem track de som.
    pub som: String,
    /// As vontades da janela (D1 e D3), para os controles mostrarem o que vale.
    pub som_mudo: bool,
    pub som_volume: f32,
    pub som_com_camera: bool,
    /// **R9b**: o aparelho que filma respondeu ao controle remoto da câmera, com a câmera aberta
    /// (`pronto` ou `nao_permitido`): a janela mostra a engrenagem "Ajustes da câmera".
    pub camera_remota: bool,
}

impl EstadoDoReceptor {
    fn mudou(&mut self) {
        self.versao += 1;
    }
}

// ---------------------------------------------------------------------------------------------
// A política de pedido de IDR
// ---------------------------------------------------------------------------------------------

/// Piso do **primeiro** pedido por uma perda nova. Medido no Android (`docs/android-para-android.md`
/// §16) e adotado pelas outras cascas.
const PISO_DE_ABERTURA: Duration = Duration::from_millis(100);
/// Piso para **insistir** na mesma perda que ninguém atendeu. Foi 500 ms; a varredura de
/// `docs/android-para-android.md` §17 baixou para 250 ms — p95 quase igual ao do piso de 100 ms
/// com metade do chamado.
///
/// **Este é o número que a sonda `integrations/camera-windows/sonda` ainda não tem**: ela ficou em
/// 500 ms. Copiar de lá teria sido copiar o valor velho.
const PISO_DE_INSISTENCIA: Duration = Duration::from_millis(250);

/// Quanto tempo a fila precisa ficar **sem transbordar** antes de o receptor pedir o IDR que
/// conserta a cadeia.
///
/// Meio segundo é maior que o piso de insistência de propósito: o que se quer aqui não é espaçar
/// pedidos, é **não pedir enquanto o problema está acontecendo**. Ver o comentário no laço.
const CALMARIA_DA_FILA: Duration = Duration::from_millis(500);

/// Por que se está pedindo um IDR. **A causa não é enfeite de registro: ela escolhe o relógio.**
///
/// O defeito que essa separação conserta foi achado pela frente do desktop e o enunciado é dela:
/// *um PLI que saiu antes de a perda existir não pode consertá-la, e contá-lo como "já pedi" é
/// contar a resposta errada.* Com um relógio só, o pedido de abertura (que sai sempre, na entrada)
/// gastava o orçamento da perda que ainda nem tinha acontecido.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Causa {
    /// Entramos na sessão e ainda não montamos imagem nenhuma.
    Abertura,
    /// A fila entre a thread da rede e a do decoder transbordou, e um quadro foi jogado fora aqui
    /// dentro. É perda **nossa**, não da rede — e por isso não gasta o relógio da perda de rede.
    FilaCheia,
    /// Os contadores do núcleo acusaram pacote perdido ou quadro descartado.
    Perda,
}

#[derive(Default)]
struct PoliticaDeIdr {
    /// Relógio das causas que **não** são perda de rede.
    ultimo_pedido: Option<Instant>,
    /// Relógio próprio da perda. Separado, e é esse o conserto.
    ultimo_pedido_de_perda: Option<Instant>,
    perda_pendente: bool,
    pediu_por_esta_perda: bool,
    /// Última leitura da soma de anomalias do núcleo, para detectar **subida**.
    anomalias_vistas: u64,
    pub pedidos: u64,
    pub suprimidos: u64,
    pub falhos: u64,
    pub por_abertura: u64,
    pub por_fila: u64,
    pub por_perda: u64,
}

impl PoliticaDeIdr {
    /// Lê os contadores do núcleo e marca perda pendente quando eles **sobem**.
    fn notar_contadores(&mut self, anomalias: u64) {
        if anomalias > self.anomalias_vistas {
            self.anomalias_vistas = anomalias;
            // Uma rajada de dez quadros descartados vira **um** pedido: o piso é sobre o pedido,
            // não sobre a perda.
            if !self.perda_pendente {
                self.perda_pendente = true;
                self.pediu_por_esta_perda = false;
            }
        }
    }

    /// **Um IDR que chega sozinho apaga a perda pendente.** Não se pede o que o GOP do emissor já
    /// consertou — e sem isto o receptor continuaria insistindo depois de já ter se recuperado.
    fn notar_idr(&mut self) {
        self.perda_pendente = false;
        self.pediu_por_esta_perda = false;
    }

    /// Decide se este pedido pode sair agora. Devolve `false` quando o piso da causa segurou.
    fn liberar(&mut self, causa: Causa, agora: Instant) -> bool {
        let (relogio, piso) = match causa {
            Causa::Perda => {
                let piso = if self.pediu_por_esta_perda {
                    PISO_DE_INSISTENCIA
                } else {
                    PISO_DE_ABERTURA
                };
                (self.ultimo_pedido_de_perda, piso)
            }
            Causa::Abertura | Causa::FilaCheia => (self.ultimo_pedido, PISO_DE_INSISTENCIA),
        };
        let liberado = match relogio {
            None => true,
            Some(quando) => agora.duration_since(quando) >= piso,
        };
        if !liberado {
            self.suprimidos += 1;
            return false;
        }
        match causa {
            Causa::Perda => {
                self.ultimo_pedido_de_perda = Some(agora);
                self.pediu_por_esta_perda = true;
                self.por_perda += 1;
            }
            Causa::Abertura => {
                self.ultimo_pedido = Some(agora);
                self.por_abertura += 1;
            }
            Causa::FilaCheia => {
                self.ultimo_pedido = Some(agora);
                self.por_fila += 1;
            }
        }
        true
    }

    fn linha(&self) -> String {
        format!(
            "idr: pedidos={} (abertura={} fila={} perda={}) suprimidos={} falhos={} \
             perda_pendente={}",
            self.pedidos,
            self.por_abertura,
            self.por_fila,
            self.por_perda,
            self.suprimidos,
            self.falhos,
            self.perda_pendente,
        )
    }
}

// ---------------------------------------------------------------------------------------------
// O receptor
// ---------------------------------------------------------------------------------------------

/// Quanto cabe entre a thread da rede e a do decoder.
///
/// Oito quadros, e não "o que couber": uma fila grande esconde atraso em vez de mostrá-lo — o
/// receptor pareceria fluido enquanto acumula meio segundo de defasagem. Curta, ela transborda, e
/// o transbordo é **informação**: vira um pedido de IDR com causa própria e um contador.
const FILA_DE_QUADROS: usize = 8;

/// Quantas quedas do dispositivo D3D11 uma sessão reabre antes de desistir. Uma queda é acidente
/// (driver, troca de GPU); a quarta seguida é o computador dizendo que não vai, e reabrir para
/// sempre seria esconder isso atrás de uma janela piscando.
const QUEDAS_DA_GPU_TOLERADAS: u32 = 3;

/// O título da janela de vídeo: o nome do par, e o alarme da cadeia quando há alarme.
///
/// **Duas grandezas, e elas dizem coisas diferentes.** `agora` é quantos quadros suspeitos
/// entraram no último segundo — a derivada, que responde *"está quebrando neste instante"*.
/// `sessao` é o acumulado, que nunca desce. Um alarme preso à integral ficaria aceso para sempre
/// depois da primeira ruptura, e um alarme que nunca apaga não avisa nada; um que mostrasse só a
/// derivada apagaria o histórico de uma sessão que quebrou feio e sarou. Então: o aviso aparece
/// pela derivada, e o acumulado viaja junto como contexto.
///
/// O `⚠` é o mais forte que uma barra de título tem — ela não tem cor, negrito nem fundo.
/// `no_ar` é o que **está chegando**: largura, altura e os quadros apresentados por segundo.
///
/// É a metade do passo 1 de `docs/quem-limita-a-imagem.md` que o receptor consegue sozinho. O que
/// foi **pedido** no aparelho não chega aqui — não há, ainda, mensagem do emissor para o receptor —,
/// então o título diz o que está no ar e não finge saber se é o que se escolheu.
fn titulo_da_exibicao(par: &str, no_ar: Option<(u32, u32, u32)>, agora: u64, sessao: u64) -> String {
    let mut base = if par.is_empty() {
        "Quall".to_string()
    } else {
        format!("Quall — {par}") // i18n: fora (o nome do app e o do par)
    };
    if let Some((w, h, fps)) = no_ar {
        base.push_str(&tf(" · {}x{} a {} fps", &[&w, &h, &fps]));
    }
    match (agora, sessao) {
        (0, 0) => base,
        (0, s) => tf("{} · {} quadros suspeitos na sessão", &[&base, &s]),
        (a, s) => tf("{} · ⚠ {} quadros suspeitos agora · {} na sessão", &[&base, &a, &s]),
    }
}

pub struct Receptor {
    estado: Mutex<EstadoDoReceptor>,
    cancelamento: Mutex<Option<Cancelamento>>,
    parar: AtomicBool,
    pub busca: Arc<Busca>,
    pub argumentos: Argumentos,
    sessoes: AtomicU64,
    ja_exibiu_sozinho: AtomicBool,
    /// Revisão da lista de aparelhos já copiada para o [`EstadoDoReceptor`].
    revisao_publicada: AtomicU64,
    /// As câmeras virtuais, uma por aparelho pareado. Ver `crate::baias`.
    pub baias: Arc<crate::baias::Registro>,
    /// **As vontades do volume** (D1 e D3): a janela escreve mudo, volume e a chave da câmera; a
    /// sessão escreve se a câmera do Quall está em uso. O ganho que sai delas vai para o tocador
    /// da sessão de pé, se houver.
    vontade_do_som: Mutex<VontadeDoSom>,
    controle_do_som: Mutex<Option<Arc<ControleDoSom>>>,
    /// **R9b**: o controle da câmera de quem filma, nesta sessão (`camera_remota.rs`).
    camera_remota: Mutex<Option<Arc<ControleRemoto>>>,
}

impl Receptor {
    pub fn novo(argumentos: Argumentos) -> Arc<Self> {
        let baias = crate::baias::Registro::novo(false);
        let vontade = VontadeDoSom {
            mudo: argumentos.receptor_nasce_mudo(),
            volume: argumentos.volume.clamp(0.0, 1.0),
            camera_em_uso: false,
            tocar_com_a_camera: argumentos.som_com_camera,
        };
        Arc::new(Receptor {
            baias,
            vontade_do_som: Mutex::new(vontade),
            controle_do_som: Mutex::new(None),
            camera_remota: Mutex::new(None),
            estado: Mutex::new(EstadoDoReceptor {
                fase: FaseDoReceptor::Parado,
                versao: 1,
                aparelhos: Vec::new(),
                revisao_dos_aparelhos: 1,
                escolhido: None,
                procurando: false,
                aviso_da_busca: String::new(),
                destino: String::new(),
                par: String::new(),
                conselho: String::new(),
                resumo: String::new(),
                cadeia: String::new(),
                cadeia_alerta: false,
                pede_pin: false,
                oferece_desparear: false,
                som: String::new(),
                som_mudo: vontade.mudo,
                som_volume: vontade.volume,
                som_com_camera: vontade.tocar_com_a_camera,
                camera_remota: false,
            }),
            cancelamento: Mutex::new(None),
            parar: AtomicBool::new(false),
            busca: Busca::nova(),
            argumentos,
            sessoes: AtomicU64::new(0),
            ja_exibiu_sozinho: AtomicBool::new(false),
            revisao_publicada: AtomicU64::new(0),
        })
    }

    pub fn estado(&self) -> MutexGuard<'_, EstadoDoReceptor> {
        self.estado.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **R9b: abre a janela "Ajustes da câmera" da câmera de quem filma** (a engrenagem do Exibindo).
    pub fn abrir_ajustes_da_camera(&self) {
        let c = self.camera_remota.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match c {
            Some(c) => crate::janela_dos_ajustes::abrir_remota(c),
            None => registro::linha("câmera remota: sem sessão de recepção, nada a ajustar"),
        }
    }

    // MARK: - o som (D1 e D3)

    fn vontade(&self) -> VontadeDoSom {
        *self.vontade_do_som.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Muda uma vontade do som e leva o ganho novo ao tocador, se houver. Da janela e da sessão.
    fn mudar_o_som(&self, mudar: impl FnOnce(&mut VontadeDoSom)) {
        let v = {
            let mut g = self.vontade_do_som.lock().unwrap_or_else(|e| e.into_inner());
            mudar(&mut g);
            *g
        };
        if let Some(c) = self.controle_do_som.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.aplicar_ganho(v.ganho());
        }
        let mut e = self.estado();
        e.som_mudo = v.mudo;
        e.som_volume = v.volume;
        e.som_com_camera = v.tocar_com_a_camera;
        e.mudou();
    }

    /// D1: o mudo da janela.
    pub fn definir_mudo(&self, mudo: bool) {
        self.mudar_o_som(|v| v.mudo = mudo);
    }

    /// D1: o volume da janela, de 0 a 1.
    pub fn definir_volume(&self, volume: f32) {
        self.mudar_o_som(|v| v.volume = volume.clamp(0.0, 1.0));
    }

    /// D3: a chave "tocar mesmo com a câmera do Quall em uso".
    pub fn definir_som_com_camera(&self, tocar: bool) {
        self.mudar_o_som(|v| v.tocar_com_a_camera = tocar);
    }

    /// A linha do som na tela, escrita já (a abertura e a falha dela), sem esperar o relato.
    fn escrever_linha_do_som(&self, linha: String) {
        let mut e = self.estado();
        if e.som != linha {
            e.som = linha;
            e.mudou();
        }
    }

    /// A linha do som enquanto o tocador está de pé, pela vontade (D1, D3).
    fn linha_do_som_tocando(v: &VontadeDoSom) -> String {
        match v.por_que_calado() {
            // `por_que_calado` (som_puxado.rs) devolve a frase em português: a tabela a traduz.
            Some(m) => tf("som: {}", &[&tr(m)]),
            None => tf("som: tocando, volume {} %", &[&format!("{:.0}", v.ganho() * 100.0)]),
        }
    }

    /// **D3**: confere se alguma câmera do Quall está em uso e, se mudou, leva ao ganho. Barato (um
    /// cadeado curto e leituras atômicas): roda na abertura do tocador e a cada 50 ms do laço da
    /// sessão, e não a 1 Hz — um segundo de som no alto-falante é som na chamada (crítica 13, M1).
    fn conferir_a_camera(&self) {
        let em_uso = self.baias.alguma_em_uso();
        if self.vontade().camera_em_uso != em_uso {
            registro::linha(format!(
                "som: câmera do Quall {}",
                if em_uso { "em uso: o som cala (D3)" } else { "livre" }
            ));
            self.mudar_o_som(|v| v.camera_em_uso = em_uso);
        }
    }

    /// Abre o tocador na track de som adotada. `None` com o motivo no diário e na tela: a imagem
    /// segue sem som.
    fn abrir_o_som(&self, track: &TrackReceptor) -> Option<Tocador> {
        let sem_som = |motivo: String| {
            registro::linha(format!("som: {motivo}; sem som"));
            self.escrever_linha_do_som(tf("sem som: {}", &[&motivo]));
            None
        };
        let Some(codec) = track.codec_de_audio() else {
            return sem_som(t("a track de som não disse o codec").into());
        };
        let canais = track.kind().preset_de_audio().map(|p| p.canais).unwrap_or(2);
        let porta = match track.reproducao_puxada(true) {
            Ok(p) => p,
            Err(erro) => return sem_som(tf("a porta puxada não abriu: {}", &[&erro])),
        };
        // **A D3 antes de ligar** (crítica 13, M1; o Mac faz o mesmo em `montarSom`): com a câmera
        // do Quall já aberta numa chamada, o tocador nasce calado, e não calado um segundo depois.
        self.conferir_a_camera();
        let v = self.vontade();
        match Tocador::iniciar(porta, codec, canais, v.ganho()) {
            Ok(t) => {
                if self.argumentos.claquete {
                    t.ligar_claquete();
                }
                *self.controle_do_som.lock().unwrap_or_else(|e| e.into_inner()) = Some(t.controle());
                // A vontade é relida **depois** de publicar o controle: um "Mudo" que chegou entre a
                // leitura acima e a publicação não se perde (crítica 13, miúdo 7).
                let v = self.vontade();
                t.controle().aplicar_ganho(v.ganho());
                registro::linha(format!(
                    "som: {:?} \"{}\" {codec:?} x {canais}, porta puxada com RATEADJUST, ganho inicial {:.2}{}",
                    track.kind(),
                    track.label(),
                    v.ganho(),
                    v.por_que_calado().map(|m| format!(" ({m})")).unwrap_or_default()
                ));
                // Os controles aparecem já (eles só aparecem com a linha do som escrita).
                self.escrever_linha_do_som(Self::linha_do_som_tocando(&v));
                Some(t)
            }
            Err(erro) => sem_som(tf("o tocador não subiu: {}", &[&erro])),
        }
    }

    /// `--claquete` (bancada, a S7, T2 do `docs/som-no-receptor.md` §9.4): as imagens apresentadas
    /// e os estouros tocados desde o último relato, em linhas que o juiz do Mac casa com a verdade
    /// da sonda (`apps/windows/scripts/julgar-claquete-t2.py`). As horas são QPC em µs, dos dois
    /// lados; as capturas, pelo relógio comum (`-` enquanto ele não vale).
    /// - imagem, `r:qpc:captura`: o índice da régua, a hora em que o `Present` voltou, e a captura;
    /// - som, `qpc:captura`: a hora em que o estouro sai no DAC, e a captura dele.
    fn relatar_a_claquete(
        &self,
        exibicao: Option<&mut Exibicao>,
        tocador: Option<&Tocador>,
        video: &TrackReceptor,
        som: Option<&TrackReceptor>,
    ) {
        if !self.argumentos.claquete {
            return;
        }
        let deslocamento = |t: &TrackReceptor| match t.deslocamento_de_captura() {
            quall_core::relogio::DeslocamentoDeCaptura::Valido { us } => Some(us),
            _ => None,
        };
        let captura = |ts: i64, off: Option<i64>| off.map(|o| (ts + o).to_string()).unwrap_or_else(|| "-".into());
        if let Some(ex) = exibicao {
            let imagens = ex.tirar_claquete();
            if !imagens.is_empty() {
                let ov = deslocamento(video);
                registro::linha(format!(
                    "receptor claquete imagem refresh_us={} {}",
                    refresh_da_tela_us(),
                    imagens
                        .iter()
                        .map(|(r, q, ts)| format!("{r}:{q}:{}", captura(*ts as i64, ov)))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
        }
        if let (Some(t), Some(s)) = (tocador, som) {
            let estouros = t.tirar_estouros();
            if !estouros.is_empty() {
                let os = deslocamento(s);
                registro::linha(format!(
                    "receptor claquete som {}",
                    estouros
                        .iter()
                        .map(|e| format!("{}:{}", e.no_dac_qpc_us, captura(e.carimbo_us, os)))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
        }
    }

    /// O relato do som de 1 Hz: a D3 conferida, a linha do diário, e a linha da tela.
    fn relatar_o_som(&self, tocador: &Tocador, track: &TrackReceptor, voltas_ociosas: &mut u32, puxadas_antes: &mut (u64, u64), inicio: Instant) {
        self.conferir_a_camera();
        let em_uso = self.vontade().camera_em_uso;
        let v = self.vontade();
        let s = tocador.situacao();
        let (pico_do_sinal, pico) = tocador.tirar_picos();
        let c = tocador.contadores_da_porta();
        let r = s.retrato;
        let (puxadas, ociosas) = (r.puxadas, r.ociosas);
        let na_volta = (puxadas - puxadas_antes.0, ociosas - puxadas_antes.1);
        *puxadas_antes = (puxadas, ociosas);
        *voltas_ociosas = if na_volta.0 > 0 && na_volta.0 == na_volta.1 { *voltas_ociosas + 1 } else { 0 };
        let db = |p: f32| if p > 0.0 { 20.0 * f64::from(p).log10() } else { f64::NEG_INFINITY };
        let relogio = track
            .retrato_do_relogio()
            .map(|x| format!("{:?} residuo_us={:?}", x.deslocamento, x.residuo_us))
            .unwrap_or_else(|| "null".into());
        registro::linha(format!(
            "receptor som t={:.1}s {:?} ligado={} ganho={:.2}{} camera_quall={} tocar_com_camera={} \
             razao={:.6} rateadjust={} mixador={} Hz x {} latencia={:.1}ms religamentos={} tentativas_falhas={} \
             ultima_falha=\"{}\" renders={} render=[{}..{}] puxadas={} ociosas={} silencios={} curas={} \
             pico_do_sinal={:.1}dBFS pico_na_saida={:.1}dBFS falhas_de_decodificar={} porta=[quadros={} subconsumos={} nivel={} k={} ancora_ms={:.1} \
             razao_sugerida={:?} deriva_ppm={:?}] relogio={relogio}",
            inicio.elapsed().as_secs_f64(),
            tocador.codec,
            if s.ligado { "sim" } else { "NAO" },
            v.ganho(),
            v.por_que_calado().map(|m| format!(" ({m})")).unwrap_or_default(),
            if em_uso { "em_uso" } else { "livre" },
            if v.tocar_com_a_camera { "sim" } else { "nao" },
            s.razao_aplicada,
            if s.com_rateadjust { "sim" } else { "nao" },
            s.taxa_do_mixador,
            s.canais_do_mixador,
            s.latencia_ms,
            s.religamentos,
            s.tentativas_falhas,
            s.ultima_falha,
            r.renders,
            if r.menor_render == usize::MAX { 0 } else { r.menor_render },
            r.maior_render,
            r.puxadas,
            r.ociosas,
            r.silencios,
            r.curas,
            db(pico_do_sinal),
            db(pico),
            tocador.falhas_de_decodificar(),
            c.quadros,
            c.subconsumos,
            c.nivel,
            c.profundidade_efetiva,
            c.latencia_na_ancoragem_us.map(|a| a as f64 / 1000.0).unwrap_or(-1.0),
            c.razao_sugerida,
            c.deriva_ed_ppm,
        ));
        let linha = if !s.ligado {
            let falha = if s.ultima_falha.is_empty() { t("a saída está sendo montada").to_string() } else { s.ultima_falha.clone() };
            tf("sem som: {}", &[&falha])
        } else if v.por_que_calado().is_none() && *voltas_ociosas >= 2 {
            // Daqui não se distingue o D2 (o som está com outro receptor) do emissor que parou de
            // mandar; a frase diz as duas coisas, como no Mac.
            t("som: nada chegando do emissor (o som pode estar com outro receptor)").to_string()
        } else {
            Self::linha_do_som_tocando(&v)
        };
        self.escrever_linha_do_som(linha);
    }

    /// Começa a procurar aparelhos na rede. **Com `--so-local` não procura**: a procura escuta o mDNS
    /// fora do laço local, e a bancada com `--so-local` promete não escutar fora dele.
    pub fn comecar_a_procurar(&self) {
        if self.argumentos.so_local {
            crate::registro::linha("receptor: --so-local, sem a procura por mDNS");
            return;
        }
        self.busca.comecar();
    }

    /// Copia a lista publicada pela thread de mDNS para o estado que a janela lê.
    ///
    /// Chamada do relógio da janela. Existe para a janela ler **uma** trava e não duas, e para a
    /// remontagem do controle depender de uma revisão só — a mesma razão pela qual o seletor de
    /// monitor tem `revisao_das_fontes`.
    pub fn pulsar(&self) {
        let (aparelhos, revisao, ativa, motivo) = {
            let l = self.busca.lista();
            (l.aparelhos.clone(), l.revisao, l.ativa, l.motivo.clone())
        };
        if self.revisao_publicada.swap(revisao, Ordering::SeqCst) == revisao {
            return;
        }
        let mut e = self.estado();
        // **A escolha é preservada pelo id do aparelho, não pelo índice.** Um aparelho que some da
        // rede faria o índice 1 virar 0, e uma escolha guardada por posição passaria a apontar
        // para outro aparelho em silêncio — é o mesmo defeito que `fontes.rs` evita guardando
        // `\\.\DISPLAY1` em vez do `HMONITOR`.
        let escolhido_antes = e.escolhido.and_then(|i| e.aparelhos.get(i)).map(|a| a.fullname.clone());
        e.escolhido = escolhido_antes
            .and_then(|f| aparelhos.iter().position(|a| a.fullname == f))
            .or_else(|| if aparelhos.is_empty() { None } else { Some(0) });
        e.aparelhos = aparelhos;
        e.revisao_dos_aparelhos = revisao;
        e.procurando = ativa;
        e.aviso_da_busca = motivo;
        e.mudou();
    }

    pub fn escolher_aparelho(&self, indice: usize) {
        let mut e = self.estado();
        if indice < e.aparelhos.len() && e.escolhido != Some(indice) {
            e.escolhido = Some(indice);
            e.mudou();
        }
    }

    /// **Bancada.** O clique em "Exibir" que um agente não tem como dar.
    pub fn talvez_exibir_sozinho(self: &Arc<Self>) {
        let Some(destino) = self.argumentos.exibir_ja.clone() else { return };
        if !self.argumentos.repetir_exibir && self.ja_exibiu_sozinho.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.estado().fase != FaseDoReceptor::Parado {
            return;
        }
        let n = self.sessoes.fetch_add(1, Ordering::SeqCst) + 1;
        registro::linha(format!("exibir automático: sessão número {n} deste processo"));
        self.exibir(destino, self.argumentos.pin.clone().unwrap_or_default());
    }

    // MARK: - exibir

    /// O gesto: "exibir o que aquele aparelho está mandando".
    ///
    /// `endereco_digitado` vazio quer dizer "use o aparelho escolhido na lista". Os dois caminhos
    /// existem e o segundo é obrigatório (`PROMPT.md`); nenhum é o "modo avançado" do outro.
    pub fn exibir(self: &Arc<Self>, endereco_digitado: String, pin_digitado: String) {
        let (destino_texto, endereco) = {
            let mut e = self.estado();
            if e.fase != FaseDoReceptor::Parado {
                return;
            }
            let digitado = endereco_digitado.trim().to_string();
            let alvo = if !digitado.is_empty() {
                match endereco_manual(&digitado) {
                    Ok(a) => (digitado.clone(), a),
                    Err(erro) => {
                        e.conselho = tf("Não entendi \"{}\". Escreva o endereço como aparece no outro aparelho — por exemplo 192.168.1.41:7877. ({})", &[&digitado, &erro]);
                        e.mudou();
                        return;
                    }
                }
            } else {
                match e.escolhido.and_then(|i| e.aparelhos.get(i)).cloned() {
                    Some(a) => (format!("{} ({})", a.nome, a.endereco), a.endereco),
                    None => {
                        e.conselho = t("Escolha um aparelho na lista, ou digite o endereço dele.").into();
                        e.mudou();
                        return;
                    }
                }
            };
            e.conselho.clear();
            e.resumo.clear();
            e.cadeia.clear();
            e.cadeia_alerta = false;
            e.par.clear();
            e.pede_pin = false;
            e.oferece_desparear = false;
            e.destino = alvo.0.clone();
            e.fase = FaseDoReceptor::Conectando;
            e.mudou();
            alvo
        };

        // **O PIN vazio não é erro.** Com o par já conhecido a retomada é sem PIN — é justamente o
        // estado que o emissor publica como `pares_conhecidos=true`. Um campo obrigatório aqui
        // obrigaria a pessoa a digitar seis dígitos que o protocolo vai ignorar.
        let pin = match pin_digitado.trim() {
            "" => None,
            texto => match Pin::parse(texto) {
                Ok(p) => Some(p),
                Err(erro) => {
                    let mut e = self.estado();
                    e.conselho = tf("Esse PIN não tem a forma certa: {}", &[&erro]);
                    e.fase = FaseDoReceptor::Parado;
                    e.pede_pin = true;
                    e.mudou();
                    return;
                }
            },
        };

        let cancelamento = Cancelamento::novo();
        *self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancelamento.clone());
        self.parar.store(false, Ordering::SeqCst);

        let eu = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("quall.receptor".into())
            .spawn(move || eu.correr_sessao(destino_texto, endereco, pin, cancelamento));
    }

    fn correr_sessao(
        self: Arc<Self>,
        destino_texto: String,
        endereco: std::net::SocketAddr,
        pin: Option<Pin>,
        cancelamento: Cancelamento,
    ) {
        let device_id = identidade::device_id();
        let nome = identidade::nome_do_aparelho();
        registro::linha(format!(
            "exibir: familia={} porta={} pin_digitado={} \
             pares_conhecidos={}",
            if endereco.is_ipv4() { "IPv4" } else { "IPv6" },
            endereco.port(),
            pin.is_some(),
            identidade::ha_pares_conhecidos()
        ));

        // **Anuncia-se como `sink` e nada mais.** Declarar `screen_source` aqui poria este
        // computador na lista de fontes dos outros aparelhos numa sessão em que ele não vai emitir
        // nada — e o app já sabe emitir, por outro botão, com outra sessão.
        let eu_anuncio = anuncio(
            &device_id,
            &nome,
            Capabilities { screen_source: false, camera_source: false, sink: true },
        );

        let resultado = conectar(
            endereco,
            SessionConfig {
                announcement: eu_anuncio,
                pin,
                known: identidade::pares_conhecidos(),
                // Ver `Argumentos::ligar_em`: sem isto o ICE escolhe o rádio quando o aparelho
                // é alcançável pelos dois caminhos, e uma corrida "pelo cabo" fecha pela Wi-Fi.
                transport: TransportConfig {
                    bind_address: self.argumentos.ligar_em.clone(),
                    ..TransportConfig::default()
                },
                // Quem conecta não declara tracks: `tracks` só vale em `hospedar` (dívida 1).
                tracks: Vec::new(),
                timeout: Duration::from_secs(60),
                // Detector de silêncio do caminho **desligado**, que é o padrão do núcleo:
                // tela parada legitimamente não produz quadro, e quem sabe se a origem produz
                // continuamente é a casca. Ver `SessionConfig::silencio_do_caminho`.
                silencio_do_caminho: None,
                cancelamento,
            },
        );

        let mut pronto = match resultado {
            Ok(p) => p,
            Err(erro) => {
                registro::linha(format!("conectar falhou: status={}", crate::diagnostico_rede::status(&erro)));
                self.ao_falhar(erro);
                return;
            }
        };

        // O pareamento fechou: grava **antes** de qualquer outra coisa, exatamente como o emissor.
        // Se o app morrer no meio da exibição, o par continua conhecido e a próxima vez não pede
        // PIN.
        let mut novos = PairedPeers::new();
        novos.insert(&pronto.outcome);
        identidade::guardar_pares(&novos);

        let nome_do_par = pronto.peer.display_name.clone();
        // A baia deste aparelho: pode já existir (o app subiu com ela) ou nascer agora, na
        // primeira vez que este aparelho conecta. `None` quando a frente está desligada.
        let baia = self
            .baias
            .obter_ou_criar(&pronto.peer.device_id.0, &nome_do_par);
        if let Some(b) = &baia {
            b.dizer(crate::placa::Estado::EsperandoImagem);
        }
        registro::linha(format!(
            "conectado: pareamento={} caminho={}",
            if pronto.outcome.novo { "novo, por PIN" } else { "retomado, sem PIN" },
            pronto
                .session
                .selected_pair()
                .map(|(l, r)| format!("{} <-> {}", crate::higiene_do_registro::candidato(&l), crate::higiene_do_registro::candidato(&r)))
                .unwrap_or_else(|| "(não informado)".into()),
        ));
        {
            let mut e = self.estado();
            e.par = nome_do_par.clone();
            e.fase = FaseDoReceptor::Exibindo;
            e.mudou();
        }

        // --- as tracks -------------------------------------------------------------------------
        //
        // **A track é achada pela espécie, nunca pela posição.** O achado 6 desta bancada mediu que
        // a ordem de chegada seguiu a da oferta nas duas vezes em que foi testada — e registrou que
        // isso **não é promessa do protocolo**: `session.rs` casa por `mid`, não por posição.
        // Depender daquele fato medido seria transformar uma observação em contrato.
        //
        // As tracks que não interessam ficam **vivas**, guardadas: largar uma `TrackReceptor` no
        // meio da sessão é mexer no que o núcleo montou, e nada aqui precisa disso.
        let mut tracks: Vec<TrackReceptor> = Vec::new();
        let mut indice_de_video: Option<usize> = None;
        let prazo_das_tracks = Instant::now() + Duration::from_secs(15);
        while indice_de_video.is_none() && Instant::now() < prazo_das_tracks {
            if self.parar.load(Ordering::SeqCst) {
                break;
            }
            let Some(track) = pronto.session.proxima_track(Duration::from_millis(250)) else {
                continue;
            };
            registro::linha(format!(
                "track: {:?} mid={}",
                track.kind(),
                track.mid()
            ));
            let e_video = track.kind() == quall_core::track::TrackKind::Screen;
            tracks.push(track);
            if e_video {
                indice_de_video = Some(tracks.len() - 1);
            }
            // O som é adotado mais abaixo, depois do vídeo (S6): a porta puxada e o tocador.
        }
        let Some(iv) = indice_de_video else {
            self.encerrar_com(tf("{} conectou, mas não mandou nenhuma track de vídeo em 15 segundos.", &[&nome_do_par]));
            pronto.link.close("nenhuma track de vídeo"); // i18n: fora (o motivo do fio)
            return;
        };

        // --- o tratador, e o que atravessa a fronteira -------------------------------------------
        let (envio, recebimento): (Sender<QuadroRecebido>, Receiver<QuadroRecebido>) =
            bounded(FILA_DE_QUADROS);
        let transbordos = Arc::new(AtomicU64::new(0));
        let recebidos = Arc::new(AtomicU64::new(0));
        {
            let transbordos = Arc::clone(&transbordos);
            let recebidos = Arc::clone(&recebidos);
            tracks[iv].ao_receber_quadro(move |q: QuadroCodificado<'_>| {
                recebidos.fetch_add(1, Ordering::Relaxed);
                let copia = QuadroRecebido {
                    bytes: q.annexb.to_vec(),
                    timestamp_us: q.timestamp_us,
                    idr: q.idr,
                    chegou_em: Instant::now(),
                    // **A condenação NÃO acontece aqui**, e o motivo é o mesmo que faz esta thread
                    // só copiar e ir embora: ela é da libdatachannel e o núcleo a chamou com o
                    // cadeado do depacotizador na mão. Perguntar `contadores()` daqui fecharia o
                    // ciclo ABBA que obriga as cascas Apple e a do OBS a condenar por amostragem.
                    // Quem classifica é a thread da sessão, no instante em que tira o quadro da
                    // fila — e é por isso que **nesta casca a condenação é exata, por quadro**.
                    suspeito: false,
                };
                // **Nunca bloquear aqui.** Esta thread é da libdatachannel e segura a sessão
                // inteira; um `send` bloqueante numa fila cheia pararia até o RTCP.
                if let Err(TrySendError::Full(_)) = envio.try_send(copia) {
                    transbordos.fetch_add(1, Ordering::Relaxed);
                }
            });
        }

        // --- o som (S6) --------------------------------------------------------------------------
        //
        // **A track de som, pela espécie**, e só uma. Ela pode ter chegado antes do vídeo (está em
        // `tracks`) ou chegar depois: a espiada de prazo zero do laço a pega, como no OBS e no Mac.
        // O tocador sai **antes** das tracks no fim, com barreira.
        // **A guarda da linha do som** (crítica 13, miúdo 1): em qualquer saída desta função —
        // inclusive o `return` do decoder que não reabre —, o controle do tocador e a linha da tela
        // são limpos, e a sessão seguinte não nasce com "som: tocando" de outra.
        let _limpa_o_som = LimpaOSom(&self);

        // --- R9b: o controle remoto da câmera de quem filma ----------------------------------------
        //
        // Pelo canal de dados desta sessão de vídeo, com o laço como o único leitor dele. O
        // `Controlador` manda o `ola`, recebe o estado e as capacidades, e manda os pedidos da janela
        // de ajustes remota; aqui ele só é bombeado, sem espera, a cada volta.
        let camera = ControleRemoto::novo(nome_do_par.clone());
        let mensageiro = pronto.session.mensageiro();
        *self.camera_remota.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&camera));
        let _limpa_a_camera = LimpaACamera(&self);
        let mut situacao_da_camera = String::new();
        let mut indice_do_som = tracks.iter().position(|t| t.kind().e_audio());
        let mut tocador: Option<Tocador> = indice_do_som.and_then(|i| self.abrir_o_som(&tracks[i]));
        let mut conferiu_o_som = Instant::now();
        let mut voltas_ociosas = 0u32;
        let mut puxadas_antes = (0u64, 0u64);
        let inicio_do_som = Instant::now();

        // O receptor entrou agora, quase sempre no meio do GOP: pedir IDR já.
        let mut politica = PoliticaDeIdr::default();
        self.pedir_idr(&tracks[iv], &mut politica, Causa::Abertura);

        // --- a resolução vem do SPS --------------------------------------------------------------
        //
        // O decoder precisa de largura e altura antes do primeiro quadro, e o protocolo não as
        // manda: o rótulo da track traz o nome do monitor, não o tamanho dele. Quem sabe é o
        // conjunto de parâmetros, e `sps.rs` já sabe lê-lo.
        let mut exibicao: Option<Exibicao> = None;
        let mut pendente: Option<QuadroRecebido> = None;
        let entrou_em = Instant::now();
        let mut ultimo_relato = Instant::now();
        let mut motivo_do_fim = String::new();
        let mut descartados_sem_sps: u64 = 0;
        let mut idrs_sem_parametros: u64 = 0;
        let mut ultimo_transbordo: u64 = 0;
        let mut transbordo_em: Option<Instant> = None;
        // O valor de `transbordou` na última vez que o relato saiu. Ver o uso, logo abaixo.
        let mut transbordo_relatado: u64 = 0;
        let mut devendo_idr_por_fila = false;

        // **A condenação da cadeia de referência**, e a porta que ela governa. Ver `crate::cadeia`.
        // A porta nasce desligada; o sinalizador existe para a bancada medir os dois braços.
        // **O caminho da mídia é dito quando existe, e não quando a sessão abre.**
        //
        // A linha `conectado: … caminho=` acima roda logo depois do pareamento, e nessa hora o ICE
        // **ainda não escolheu par** — ela imprimia `(não informado)` em toda corrida. Em
        // 01/09/2026 isso custou uma medição inteira: duas corridas A07 → Dell, uma pelo endereço
        // do cabo USB e outra pelo da Wi-Fi, deram números idênticos e **nenhuma das duas era
        // atribuível**, porque o Dell alcança aquele telefone pelos dois caminhos ao mesmo tempo e
        // ninguém sabia qual o ICE tinha escolhido.
        //
        // Uma corrida "pelo cabo" pode fechar pela Wi-Fi e parecer sucesso. Sem esta linha, o
        // rótulo mente.
        let mut caminho_dito = false;
        let mut condenacao = Condenacao::nova(self.argumentos.congelar_na_ruptura);
        // Quantos suspeitos havia no relatório anterior — a âncora da derivada que vai ao título.
        let mut suspeitos_no_relato: u64 = 0;
        // Quantos quadros tinham sido apresentados no relatório anterior — a âncora do fps do título.
        let mut apresentados_no_relato: u64 = 0;
        // **O caminho de volta do sinal.** Ver `crate::janela_do_enlace`. Sai sempre, sem
        // sinalizador de bancada: nos dois braços de um A/B o mesmo tráfego de relato tem de estar
        // no ar, ou a diferença entre eles pode ser explicada pelo próprio instrumento.
        let mut janela = JanelaDoEnlace::nova();
        let mut relatos_recusados: u64 = 0;
        // **O laço está dando conta?** `fila` é quantos quadros ainda esperavam quando este tirou o
        // próximo; `volta_us` é o que uma volta com quadro custa, da tirada ao fim da apresentação.
        // Ver `crate::etapas` para o teto que foi atribuído ao decodificador sem esta medida.
        let mut fila = crate::etapas::Etapa::nova("fila");
        let mut volta = crate::etapas::Etapa::nova("volta_us");
        // `--receptor-lento-fps`: a hora em que o laço pode tirar o próximo quadro. Ver o
        // sinalizador — é condição fabricada, para exercitar o controlador do outro lado.
        let lento = self
            .argumentos
            .receptor_lento_fps
            .filter(|f| *f > 0)
            .map(|f| Duration::from_secs_f64(1.0 / f64::from(f)));
        if let Some(p) = lento {
            registro::linha(format!(
                "BANCADA: receptor lento de propósito — no máximo {:.0} quadros por segundo saem da fila",
                1.0 / p.as_secs_f64()
            ));
        }
        let mut proxima_tirada: Option<Instant> = None;
        // Quantos SPS Baseline foram declarados Constrained nesta sessão. Ver
        // `sps::declarar_constrained_baseline` — é o que põe o S24 no DXVA.
        let mut sps_declarados: u64 = 0;
        // Quantos SPS Baseline sem `bitstream_restriction` ganharam a restrição nesta sessão. Ver
        // `sps::declarar_restricao_de_bitstream`.
        let mut sps_com_restricao: u64 = 0;
        // Quantas vezes o dispositivo D3D11 caiu nesta sessão e a exibição foi reaberta.
        let mut quedas_da_gpu: u32 = 0;

        loop {
            if self.parar.load(Ordering::SeqCst) {
                break;
            }
            // A D3 e a razão do núcleo, a cada 50 ms, fora da thread do render (crítica 13, M1 e M5).
            if let Some(t) = tocador.as_ref() {
                if conferiu_o_som.elapsed() >= Duration::from_millis(50) {
                    conferiu_o_som = Instant::now();
                    self.conferir_a_camera();
                    t.atualizar_razao();
                }
            }
            // A track de som que chega depois do vídeo, por espiada de prazo zero. Para de espiar
            // quando uma é adotada.
            if indice_do_som.is_none() {
                if let Some(t) = pronto.session.proxima_track(Duration::ZERO) {
                    registro::linha(format!("track: {:?} mid={}", t.kind(), t.mid()));
                    let e_audio = t.kind().e_audio();
                    tracks.push(t);
                    if e_audio {
                        indice_do_som = Some(tracks.len() - 1);
                        tocador = self.abrir_o_som(&tracks[tracks.len() - 1]);
                    }
                }
            }
            self.bombear_a_camera(&camera, &mensageiro, &mut situacao_da_camera);
            match pronto.proximo_evento(Duration::from_millis(0)) {
                EventoDeSessao::Desconectou => {
                    motivo_do_fim = tf("{} parou de transmitir.", &[&nome_do_par]);
                    registro::linha("sessão caiu: Desconectou");
                    break;
                }
                EventoDeSessao::Falhou => {
                    motivo_do_fim = t("A conexão caiu.").into();
                    registro::linha("sessão caiu: Falhou");
                    break;
                }
                EventoDeSessao::Nenhum => {}
            }

            // A janela de vídeo fechada pela pessoa encerra a sessão — e encerra de verdade, não
            // esconde. É a mesma decisão do `WM_CLOSE` da janela principal.
            if let Some(e) = exibicao.as_ref() {
                if !e.janela_viva() {
                    motivo_do_fim = t("Você fechou a janela do vídeo.").into();
                    registro::linha("janela de vídeo fechada pela pessoa");
                    break;
                }
            }

            // --- pega o próximo quadro (o que sobrou de uma recusa vem antes) ---
            //
            // Quando há um quadro pendente o laço **não** espera na fila, e é aí que ele poderia
            // girar em vazio: o MFT síncrono recusa a entrada até alguém drenar a saída, e a
            // drenagem só acontece mais abaixo nesta mesma volta. Um milissegundo de pausa é o que
            // separa "volta e drena" de "queima um núcleo perguntando". Não é um prazo: é um piso.
            let (mut quadro, e_novo) = match pendente.take() {
                Some(q) => {
                    std::thread::sleep(Duration::from_millis(1));
                    (Some(q), false)
                }
                // `--receptor-lento-fps`: antes da hora, a volta segue **sem** tirar quadro — e a
                // fila, que o tratador da rede continua enchendo, transborda como num receptor
                // que não dá conta. A espera é picada em 10 ms, como a do `recv_timeout`, para a
                // janela de vídeo e a sessão continuarem sendo atendidas.
                None if proxima_tirada.is_some_and(|p| Instant::now() < p) => {
                    let falta = proxima_tirada
                        .map(|p| p.saturating_duration_since(Instant::now()))
                        .unwrap_or_default();
                    std::thread::sleep(falta.min(Duration::from_millis(10)));
                    (None, true)
                }
                None => (recebimento.recv_timeout(Duration::from_millis(10)).ok(), true),
            };
            if let (Some(periodo), true, true) = (lento, e_novo, quadro.is_some()) {
                let agora = Instant::now();
                let mut p = proxima_tirada.unwrap_or(agora) + periodo;
                if p + periodo < agora {
                    p = agora + periodo;
                }
                proxima_tirada = Some(p);
            }
            // **O SPS antes do decoder, e antes do resumo que abre a exibição.** Dois remendos, só
            // no SPS Baseline e só no prefixo não-VCL do quadro, na thread da sessão:
            //  - sem `constraint_set1`, o MFT da Microsoft decodifica em software; o bit é marcado;
            //  - sem `bitstream_restriction`, ele segura ~5 quadros antes de entregar (161 ms de
            //    fila→tela e o som 81 ms antes da imagem, na T2 da S7 do som, 21/09); a restrição é
            //    declarada com `max_num_reorder_frames = 0`, que é o que um Baseline é.
            // `--sps-como-veio` desliga os dois: é o braço de antes.
            if let (Some(q), false) = (quadro.as_mut(), self.argumentos.sps_como_veio) {
                if e_novo {
                    let preparo = sps::preparar_para_o_decoder(&mut q.bytes);
                    if preparo.constrained {
                        sps_declarados += 1;
                        if sps_declarados == 1 {
                            registro::linha(
                                "sps: o emissor declara Baseline sem constraint_set1 — marcando \
                                 Constrained Baseline (o que o SDP já anuncia) para o decoder poder \
                                 usar DXVA",
                            );
                        }
                    }
                    if let Some(r) = preparo.restricao {
                        sps_com_restricao += 1;
                        if sps_com_restricao == 1 {
                            registro::linha(format!(
                                "sps: o emissor não declara bitstream_restriction — declarando \
                                 reorder=0 (Baseline não reordena), para o decoder não segurar \
                                 quadros | como chegou: {} | como vai ao decoder: {}",
                                r.antes.linha(),
                                r.depois.linha()
                            ));
                        }
                    }
                }
            }
            let volta_comecou = Instant::now();
            let houve_quadro = quadro.is_some();
            if e_novo && houve_quadro {
                fila.anotar(recebimento.len() as u64);
            }

            // --- UMA leitura de contadores por volta, e ela serve às três coisas -----------------
            //
            // A condenação da cadeia, a política de IDR e a janela do enlace leem os **mesmos**
            // números no **mesmo** instante. Ler três vezes daria três instantes, e a aritmética
            // que liga os números — `rupturas` contra `quadros_descartados`, `perdidos` contra
            // `pacotes` — deixaria de fechar. É para isso que `rtp::Contadores` existe.
            //
            // **E ela acontece aqui, depois de tirar o quadro da fila e antes de submetê-lo.** É o
            // que dá a esta casca a condenação **por quadro** em vez de por volta de laço de
            // amostragem: cada quadro é classificado contra uma leitura feita no instante em que
            // ele saiu da fila. Nas cascas Apple e na do OBS isso é impossível, porque lá a
            // leitura teria de sair de dentro do tratador de quadro, que roda com o cadeado do
            // depacotizador na mão.
            if !caminho_dito {
                // **Endereço primeiro, candidato depois — e a ordem é medida.**
                //
                // `selected_pair()` lê os objetos de candidato, e eles vêm **nulos** num par
                // *peer-reflexive*: foi o que o `quall_session_path_json` devolveu na corrida do
                // iPad pelo cabo em 01/09/2026, com `local_candidate: null` ao lado de
                // `local_address: "169.254.75.173:49564"`. Nas três corridas de câmera por cabo
                // desta mesma tarde a linha não saiu **nenhuma vez**, e a mídia estava correndo com
                // perda zero — instrumento mudo justamente no caso que ele existe para julgar.
                let par = pronto
                    .session
                    .local_address()
                    .zip(pronto.session.remote_address())
                    .map(|(l, r)| (crate::higiene_do_registro::sanitizar(&l), crate::higiene_do_registro::sanitizar(&r)))
                    .or_else(|| pronto.session.selected_pair().map(|(l, r)| (crate::higiene_do_registro::candidato(&l), crate::higiene_do_registro::candidato(&r))));
                if let Some((l, r)) = par {
                    caminho_dito = true;
                    registro::linha(format!("caminho da mídia: {l} <-> {r}"));
                }
            }

            let agora = Instant::now();
            let c = tracks[iv].contadores();
            let transbordou = transbordos.load(Ordering::Relaxed);
            condenacao.notar_contadores(c.quadros_descartados, transbordou, agora);
            // Só o quadro recém-tirado da fila é classificado: um quadro que o MFT recusou volta
            // por `pendente` já com o veredito dele, e reclassificá-lo contaria `suspeitos` duas
            // vezes para o mesmo quadro.
            if e_novo {
                if let Some(q) = quadro.as_mut() {
                    q.suspeito = condenacao.classificar(q.idr, agora);
                }
            }

            if let Some(q) = quadro {
                if q.idr {
                    politica.notar_idr();
                }
                match exibicao.as_mut() {
                    Some(ex) => {
                        if !ex.submeter(&q) {
                            pendente = Some(q);
                        }
                    }
                    None => match sps::resumir(&q.bytes) {
                        Some(resumo) => {
                            // Depois dos remendos (sem `--sps-como-veio`): é o que o decoder lê. O
                            // SPS como o emissor mandou está na linha "sps: o emissor não declara…".
                            registro::linha(format!("sps que vai ao decoder: {}", resumo.linha()));
                            if !resumo.declara_a_restricao() {
                                registro::linha(
                                    "sps: o emissor NÃO declara bitstream_restriction — este \
                                     decodificador vai assumir o teto do nível e segurar quadros \
                                     antes de entregar o primeiro (ver docs/regras-de-frente.md)",
                                );
                            }
                            match self.abrir_exibicao(&resumo, &nome_do_par) {
                                Ok(mut ex) => {
                                    registro::linha(format!(
                                        "exibição aberta: {}x{} decoder=\"{}\" adaptador={}",
                                        ex.largura, ex.altura, ex.nome_do_decoder, ex.adaptador
                                    ));
                                    if let Some(b) = &baia {
                                        if let Err(erro) = ex.ligar_camera(
                                            Arc::clone(b),
                                            self.argumentos.camera_em_todo_quadro,
                                        ) {
                                            registro::linha(format!(
                                                "camera virtual: não liguei ao vídeo — {erro:#}"
                                            ));
                                        }
                                    }
                                    if !ex.submeter(&q) {
                                        pendente = Some(q);
                                    }
                                    exibicao = Some(ex);
                                }
                                Err(erro) => {
                                    self.encerrar_com(tf("Não consegui abrir o vídeo: {}", &[&erro]));
                                    pronto.link.close("decoder não abriu"); // i18n: fora (o motivo do fio)
                                    let _ = tracks[iv].desregistrar_quadro();
                                    return;
                                }
                            }
                        }
                        None => {
                            // Quadro sem conjunto de parâmetros e sem decoder aberto: não há o que
                            // fazer com ele. É o caso normal de entrar no meio do GOP, e é por isso
                            // que o pedido de IDR de abertura sai antes.
                            descartados_sem_sps += 1;
                            // **Um IDR sem SPS/PPS é outra coisa, e é o defeito do M1 no Windows
                            // aparecendo no fio.** `crates/quall-core/src/track.rs` tem um teste
                            // com esse nome literal. Se acontecer, este receptor nunca abre o
                            // decoder — e pedir outro IDR não resolve, porque o próximo virá igual.
                            // Separar a linha é o que distingue "entrei no meio do GOP" (espera) de
                            // "este emissor não põe parâmetros no IDR" (não tem saída aqui).
                            if q.idr {
                                idrs_sem_parametros += 1;
                                if idrs_sem_parametros == 1 {
                                    registro::linha(
                                        "ATENÇÃO: chegou um IDR SEM SPS/PPS — é o defeito que o \
                                         teste `idr_sem_sps_pps` do núcleo nomeia. Este receptor \
                                         não tem como abrir o decoder sem os parâmetros, e pedir \
                                         outro IDR não resolve.",
                                    );
                                }
                            }
                            self.pedir_idr(&tracks[iv], &mut politica, Causa::Abertura);
                        }
                    },
                }
            }

            if let Some(ex) = exibicao.as_mut() {
                ex.bombear();
                if houve_quadro {
                    volta.desde(volta_comecou);
                }
            }

            // **A GPU caiu: larga a exibição e abre outra.** Um dispositivo D3D11 caído não volta
            // — toda chamada nele devolve `0x887A0005` —, e até 10/09/2026 a janela ficava no
            // último quadro pelo resto da sessão com a rede perfeita por baixo. Largar basta para
            // reabrir: sem exibição, o primeiro quadro sem SPS pede IDR (`Causa::Abertura`) e o IDR
            // abre dispositivo, decoder, janela e câmera virtual novos, pelo mesmo caminho da
            // abertura. A janela pisca — é o preço, e é visível de propósito.
            if let Some(motivo) = exibicao.as_ref().and_then(|ex| ex.gpu_caiu()) {
                if let Some(ex) = exibicao.take() {
                    quedas_da_gpu += 1;
                    registro::linha(format!(
                        "exibicao (largada na queda nº {quedas_da_gpu} da GPU, 0x{:08X}): {} | {}",
                        motivo.0 as u32,
                        ex.contadores.linha(),
                        ex.fluidez.linha()
                    ));
                    if let Some(linha) = ex.linha_da_camera() {
                        registro::linha(linha);
                    }
                    // Sem `fechar()`: drenar o MFT é mais chamada num dispositivo que não
                    // responde. O `Drop` destrói a janela.
                    drop(ex);
                }
                // A exibição nova conta do zero; a âncora do fps do título também.
                apresentados_no_relato = 0;
                if quedas_da_gpu > QUEDAS_DA_GPU_TOLERADAS {
                    registro::linha(format!(
                        "exibicao: a GPU caiu {quedas_da_gpu} vezes nesta sessão — desisto de reabrir"
                    ));
                    motivo_do_fim = tf("A placa de vídeo deste computador parou {} vezes durante a transmissão. Tente de novo; se voltar a acontecer, atualize o driver de vídeo.", &[&quedas_da_gpu]);
                    break;
                }
                registro::linha("exibicao: reabrindo no próximo IDR");
                self.pedir_idr(&tracks[iv], &mut politica, Causa::Abertura);
            }

            // --- a política de IDR, sobre a MESMA leitura de contadores ---
            //
            // A linha de base dela é outra — `pacotes_perdidos() + quadros_descartados`, e não só
            // o descarte — porque ela responde outra pergunta: "vale a pena pedir um IDR?", com
            // pisos próprios. A condenação responde "este quadro tinha como estar certo?". Os dois
            // relógios são separados de propósito, como no Android.
            politica.notar_contadores(c.pacotes_perdidos() + c.quadros_descartados);
            if politica.perda_pendente {
                self.pedir_idr(&tracks[iv], &mut politica, Causa::Perda);
            }
            // **O transbordo é um contador acumulado, e agir sobre o valor em vez de sobre a
            // subida seria pedir IDR a cada 250 ms para sempre depois do primeiro.** É o mesmo
            // cuidado que `notar_contadores` toma com os números do núcleo, e é fácil errar: o
            // sintoma seria uma tempestade de PLI que só aparece em corrida longa.
            //
            // **E o pedido espera a fila acalmar, medido em 09/09/2026.** Pedir IDR *enquanto* a
            // fila transborda alimenta o próprio transbordo: a fila enche porque o laço não dá
            // conta (decode, janela e câmera virtual numa thread só — ver `crate::etapas`), e um
            // IDR é o quadro mais caro que existe para decodificar. Na corrida da
            // câmera frontal do S24 — 56 quadros chegando por segundo, 39 decodificados — o app
            // pediu **358 IDRs em 105 s**, quase quatro por segundo, e a imagem virou sujeira de
            // movimento em vez de só atrasar. O plugin do OBS, que descarta e segue sem pedir
            // nada, mostrou a mesma transmissão sem defeito.
            //
            // Agora a subida só **anota a dívida**; o pedido sai quando o transbordo para por
            // [`CALMARIA_DA_FILA`] — que é quando um IDR tem chance de ser decodificado em vez de
            // entrar na fila que já estava cheia.
            if transbordou > ultimo_transbordo {
                ultimo_transbordo = transbordou;
                transbordo_em = Some(Instant::now());
                devendo_idr_por_fila = true;
            } else if devendo_idr_por_fila
                && transbordo_em.is_some_and(|q| q.elapsed() >= CALMARIA_DA_FILA)
            {
                devendo_idr_por_fila = false;
                self.pedir_idr(&tracks[iv], &mut politica, Causa::FilaCheia);
            }
            // Sem primeira imagem: insistir, com o piso da causa de abertura.
            if exibicao.as_ref().and_then(|e| e.primeira_imagem).is_none()
                && entrou_em.elapsed() >= Duration::from_millis(300)
            {
                self.pedir_idr(&tracks[iv], &mut politica, Causa::Abertura);
            }

            // -----------------------------------------------------------------------------------
            // **O caminho de volta do sinal**, a 2 Hz: o receptor conta ao emissor o que viu do
            // enlace na janela, e é isso que acorda o controlador de taxa do outro lado.
            //
            // **Sem esta casca relatar, aquele controlador é inerte por construção** — não
            // desligado, inerte: ele roda e não tem o que ler. Medido em 31/08/2026 no par
            // A10s → iPad, com o controlador ligado: `trocas_de_bitrate=0` com 2,95 % de perda,
            // porque o relato existia só no Android.
            //
            // **Daqui, e não do tratador de quadro.** `Ready::relatar_enlace` escreve na
            // sinalização na hora, sem passar por fila, e por isso tem de sair da **mesma thread**
            // que chama `proximo_evento` — a regra que o `Ready` inteiro carrega, e que neste
            // sistema operacional não é higiene: a exceção da libdatachannel sobe de dentro do
            // `lock_guard` do mutex global sem soltá-lo, e a chamada seguinte trava o processo para
            // sempre. Esta é aquela thread.
            //
            // **Sai sempre**, sem sinalizador, ao contrário da porta. Nos dois braços de um A/B o
            // mesmo tráfego de relato precisa estar no ar: uma diferença que possa ser explicada
            // pelo próprio instrumento não mede nada. Custa ~129 bytes por janela — 0,05 % de um
            // vídeo de 4 Mbps.
            //
            // `agora` é o instante da leitura desta volta, e `c` é aquela leitura: a janela mede
            // exatamente os números que a condenação e a política viram, e não outros meio
            // milissegundo depois.
            let leitura = Acumulados {
                vistos: c.pacotes_vistos,
                // A perda **exata**. `pacotes_faltando` é o teto, e cobra reordenação como perda
                // com erro medido de 1,3× a 44×: um controlador alimentado por ele reduziria o
                // bitrate por causa de pacotes que chegaram.
                perdidos: c.pacotes_perdidos_de_verdade,
                idrs_quebrados: c.idrs_quebrados,
                suspeitos: condenacao.suspeitos,
            };
            if let Some(amostra) = janela.fechar(agora, janela_do_enlace::PERIODO, leitura) {
                registro::linha(amostra.linha());
                // **O que esta casca não conseguiu entregar nesta janela.** É a diferença do
                // acumulado, não o acumulado: mandar o total faria o emissor descer para sempre
                // depois do primeiro transbordo. Foi o número que faltava em §8.63 — 17 quadros
                // por segundo jogados fora aqui, com perda de rede 0,00 %, e o emissor **subindo**
                // porque ninguém contava isto a ele.
                let nao_entregues = transbordou.saturating_sub(transbordo_relatado);
                transbordo_relatado = transbordou;
                let relato = RelatoDoEnlace {
                    ms: amostra.ms,
                    pacotes: amostra.pacotes,
                    perdidos: amostra.perdidos,
                    suspeitos: amostra.suspeitos,
                    idrs_quebrados: amostra.idrs_quebrados,
                    nao_decodificados: nao_entregues,
                };
                // Falhar não derruba nada, e não é este laço que decide parar: um emissor de versão
                // anterior não escuta por conta própria, e um socket morto aparece no detector de
                // queda que já existe, alguns milissegundos adiante. Três vezes e cala.
                if let Err(erro) = pronto.relatar_enlace(relato) {
                    if relatos_recusados < 3 {
                        relatos_recusados += 1;
                        registro::linha(format!("o relato do enlace não saiu: {erro}"));
                    }
                }
            }

            if ultimo_relato.elapsed() >= Duration::from_secs(1) {
                let decorrido = ultimo_relato.elapsed();
                ultimo_relato = Instant::now();
                self.relatar(
                    &c,
                    exibicao.as_ref(),
                    &politica,
                    &condenacao,
                    recebidos.load(Ordering::Relaxed),
                    transbordou,
                    descartados_sem_sps,
                );
                if let (Some(t), Some(i)) = (tocador.as_ref(), indice_do_som) {
                    self.relatar_o_som(t, &tracks[i], &mut voltas_ociosas, &mut puxadas_antes, inicio_do_som);
                }
                self.relatar_a_claquete(
                    exibicao.as_mut(),
                    tocador.as_ref(),
                    &tracks[iv],
                    indice_do_som.map(|i| &tracks[i]),
                );
                // **O alarme vai para a barra de título da janela do vídeo**, e não só para o
                // painel. O painel mora na janela principal, e numa corrida de 01/09/2026 o
                // usuário fotografou o rastro do quadro quebrado com a janela do painel fora da
                // tela: os cinco números mediam certo e não estavam onde os olhos estavam.
                //
                // **Duas grandezas, e a primeira é a que importa agora.** `agora` é a derivada —
                // quantos quadros suspeitos no último segundo — e é ela que responde "está
                // quebrando neste instante". `suspeitos` é a integral e nunca desce, então um
                // alarme preso a ela ficaria aceso para sempre depois da primeira ruptura, que é
                // o mesmo que não avisar. É a distinção que a `JanelaDoEnlace` já faz para o
                // controlador, aplicada aqui para o olho.
                let agora_suspeitos = condenacao.suspeitos.saturating_sub(suspeitos_no_relato);
                suspeitos_no_relato = condenacao.suspeitos;
                if let Some(ex) = exibicao.as_mut() {
                    // Quadros **apresentados** no último relato, e não chegados: é o que a pessoa
                    // vê. Um receptor que chega a 60 e mostra 40 tem de dizer 40.
                    let apresentados = ex.contadores.apresentados;
                    let fps = (apresentados.saturating_sub(apresentados_no_relato) as f64
                        / decorrido.as_secs_f64().max(0.001))
                        .round() as u32;
                    apresentados_no_relato = apresentados;
                    let no_ar = Some((ex.largura, ex.altura, fps));
                    let mut titulo = titulo_da_exibicao(&nome_do_par, no_ar, agora_suspeitos,
                                                        condenacao.suspeitos);
                    // A janela piscou porque foi reaberta; sem isto a pessoa vê o piscar e não
                    // sabe de quê. Fica até o fim da sessão, como o acumulado de suspeitos.
                    if quedas_da_gpu > 0 {
                        titulo.push_str(&tf(" · a placa de vídeo parou {}× e a imagem foi reaberta", &[&quedas_da_gpu]));
                    }
                    ex.titular(&titulo);
                }
            }
        }

        // --- fim ---------------------------------------------------------------------------------
        //
        // **Desregistrar com barreira antes de largar qualquer coisa.** `desregistrar_quadro`
        // devolve `Barreira::Cumprida` quando ninguém está dentro do tratador e ninguém mais entra
        // — é a única forma de saber que a thread da libdatachannel não vai tocar no `Sender`
        // depois que ele sumir. Ignorar isso é a classe de defeito que só aparece uma vez em cem
        // encerramentos.
        let barreira = tracks[iv].desregistrar_quadro();
        registro::linha(format!("desregistrar_quadro: {barreira:?}"));
        // O som sai antes das tracks: o render para, e a porta puxada é encerrada com barreira.
        *self.controle_do_som.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(t) = tocador.take() {
            if let Some(i) = indice_do_som {
                self.relatar_o_som(&t, &tracks[i], &mut voltas_ociosas, &mut puxadas_antes, inicio_do_som);
            }
            let barreira_do_som = t.parar();
            registro::linha(format!("som: tocador parado, porta encerrada: {barreira_do_som:?}"));
        }
        {
            let mut e = self.estado();
            e.som.clear();
            e.mudou();
        }

        let mut retidos_finais = 0u64;
        if let Some(mut ex) = exibicao.take() {
            ex.fechar();
            retidos_finais = ex.contadores.retidos;
            registro::linha(format!(
                "exibicao (final): {} | {}",
                ex.contadores.linha(),
                ex.fluidez.linha()
            ));
            if let Some(linha) = ex.linha_da_camera() {
                registro::linha(linha);
            }
            // **Onde o quadro gasta o tempo**, parcela por parcela. `fila` perto de zero é laço
            // folgado; `fila` no teto é laço afogado — e é a linha que diz qual parcela afoga.
            registro::linha(format!(
                "laço (final): {} {} {} subrecursos_distintos={}",
                fila.linha(),
                volta.linha(),
                ex.custos.linha(),
                ex.subrecursos_distintos()
            ));
            if ex.regua.lidas > 0 || ex.regua.ilegiveis > 0 {
                registro::linha(ex.regua.linha());
            }
        }
        let c = tracks[iv].contadores();
        // A linha que a bancada lê com o olho, **antes** do resto: até 01/09/2026 esta casca era a
        // quinta receptora mostrando só `pacotes_faltando`, que é o teto. Ver
        // `docs/contador-nas-cascas.md` §6.
        registro::linha(format!(
            "perda (final): {}",
            janela_do_enlace::resumo_de_perda(
                c.pacotes_perdidos_de_verdade,
                c.pacotes_faltando,
                c.pacotes_tarde_demais,
                c.pacotes_vistos,
            )
        ));
        registro::linha(format!(
            "nucleo (final): quadros_prontos={} quadros_descartados={} idrs_prontos={} \
             idrs_quebrados={} pacotes_vistos={} pacotes_perdidos_de_verdade={} \
             pacotes_faltando_teto={} pacotes_tarde_demais={} fora_de_ordem={} rtcp_ignorados={} \
             | recebidos_na_casca={} transbordos={} descartados_sem_sps={}",
            c.quadros_prontos,
            c.quadros_descartados,
            c.idrs_prontos,
            c.idrs_quebrados,
            c.pacotes_vistos,
            c.pacotes_perdidos_de_verdade,
            c.pacotes_faltando,
            c.pacotes_tarde_demais,
            c.eventos_fora_de_ordem,
            c.rtcp_ignorados,
            recebidos.load(Ordering::Relaxed),
            transbordos.load(Ordering::Relaxed),
            descartados_sem_sps,
        ));
        // **Uma prova de recepção reprova a corrida com `suspeitos > 0`**: não é linha discreta num
        // relatório, é falha. `docs/contrato-track.md`.
        registro::linha(condenacao.linha(retidos_finais));
        if janela.leituras_recusadas > 0 {
            registro::linha(format!(
                "janela_do_enlace: {} leitura(s) do núcleo recusada(s) por regredir — a âncora foi \
                 preservada em vez de zerada",
                janela.leituras_recusadas
            ));
        }
        if idrs_sem_parametros > 0 {
            registro::linha(format!(
                "idrs_sem_parametros (vistos no fio, do lado que recebe): {idrs_sem_parametros}"
            ));
        }
        registro::linha(politica.linha());

        pronto.link.close("exibição encerrada"); // i18n: fora (o motivo do fio)
        drop(tracks);
        drop(pronto);
        registro::linha("sessao encerrada (receptor)");

        {
            let mut e = self.estado();
            if !motivo_do_fim.is_empty() {
                e.conselho = motivo_do_fim;
            }
        }
        self.voltar_ao_inicio();
    }

    fn abrir_exibicao(&self, resumo: &sps::ResumoSps, par: &str) -> anyhow::Result<Exibicao> {
        let mut ex = Exibicao::abrir(
            resumo.largura,
            resumo.altura,
            self.argumentos.fps,
            &format!("Quall — {par}"), // i18n: fora (o nome do app e o do par)
            self.argumentos.escala_do_video,
            // A claquete (S7) lê a régua de cada quadro apresentado.
            self.argumentos.regua || self.argumentos.claquete,
            self.argumentos.buffers_da_janela,
            !self.argumentos.sem_protecao_multithread,
            self.argumentos.simular_queda_da_gpu.map(Duration::from_secs),
        )?;
        ex.claquete = self.argumentos.claquete;
        Ok(ex)
    }

    /// **Pedir, e conferir que o pedido saiu.**
    ///
    /// `pedir_idr` devolve `Result` de propósito — o núcleo diz que o pedido *falha de verdade*
    /// antes de a track abrir. Engolir isso seria repetir, do lado do receptor, o defeito que este
    /// projeto já mediu três vezes do lado do encoder: uma porta de IDR que aceita e ignora.
    /// Aqui o recusado é **contado** e o pendente continua de pé para a volta seguinte.
    fn pedir_idr(&self, track: &TrackReceptor, politica: &mut PoliticaDeIdr, causa: Causa) {
        if !politica.liberar(causa, Instant::now()) {
            return;
        }
        match track.pedir_idr() {
            Ok(()) => {
                politica.pedidos += 1;
                registro::linha(format!("pedido de IDR enviado ({causa:?})"));
            }
            Err(erro) => {
                politica.falhos += 1;
                registro::linha(format!("pedido de IDR ({causa:?}) NÃO saiu: status={}", crate::diagnostico_rede::status(&erro)));
            }
        }
    }

    /// A linha de 1 Hz do diário, **e** o que a janela do app mostra.
    ///
    /// `c` chega de fora, e isso não é estilo: é a **mesma** leitura que a condenação, a política de
    /// IDR e a janela do enlace usaram nesta volta. Um relatório montado com uma leitura própria
    /// misturaria dois instantes, e a aritmética que liga os números deixaria de fechar.
    fn relatar(
        &self,
        c: &Contadores,
        exibicao: Option<&Exibicao>,
        politica: &PoliticaDeIdr,
        condenacao: &Condenacao,
        recebidos: u64,
        transbordos: u64,
        descartados_sem_sps: u64,
    ) {
        let casca = exibicao
            .map(|e| e.contadores.linha())
            .unwrap_or_else(|| "(decoder ainda não abriu)".into()); // i18n: fora (diário)
        let retidos = exibicao.map(|e| e.contadores.retidos).unwrap_or(0);
        // **A linha mastigada da perda vem ANTES dos números crus**, e é a que se lê de relance.
        // Ver `janela_do_enlace::resumo_de_perda` para a dívida que ela paga: até hoje esta casca
        // imprimia só `pacotes_faltando`, que é o **teto** e não a perda.
        registro::linha(format!(
            "perda: {}",
            janela_do_enlace::resumo_de_perda(
                c.pacotes_perdidos_de_verdade,
                c.pacotes_faltando,
                c.pacotes_tarde_demais,
                c.pacotes_vistos,
            )
        ));
        registro::linha(format!(
            "nucleo: quadros_prontos={} descartados={} idrs_prontos={} idrs_quebrados={} \
             pacotes_vistos={} perdidos_de_verdade={} faltando_teto={} tarde_demais={} \
             fora_de_ordem={} | casca: recebidos={recebidos} transbordos={transbordos} \
             sem_sps={descartados_sem_sps} {casca} | {} | {} | {}",
            c.quadros_prontos,
            c.quadros_descartados,
            c.idrs_prontos,
            c.idrs_quebrados,
            c.pacotes_vistos,
            c.pacotes_perdidos_de_verdade,
            c.pacotes_faltando,
            c.pacotes_tarde_demais,
            c.eventos_fora_de_ordem,
            politica.linha(),
            condenacao.linha(retidos),
            // **O que a média de `fila→tela` escondia.** Ela dava 6,5 ms numa corrida em que a
            // imagem não estava fluida; o pior caso da mesma corrida era 226 ms. Ver
            // `crate::fluidez`.
            exibicao.map(|e| e.fluidez.linha()).unwrap_or_else(|| "fluidez_ms=(sem tela)".into()), // i18n: fora (diário)
        ));
        // **E os cinco chegam à tela, não só ao diário.** Publicado mesmo antes de o decoder abrir:
        // uma ruptura pode acontecer no primeiro segundo, e uma linha que só aparece depois da
        // primeira imagem esconde justamente a sessão que nunca montou imagem.
        let mut es = self.estado();
        es.cadeia = condenacao.linha_da_tela(retidos);
        es.cadeia_alerta = condenacao.suspeitos > 0;
        if let Some(e) = exibicao {
            let latencia = format!("{:.0}", e.contadores.latencia_media_ms());
            let mut resumo = tf("{} quadros · {} ms da fila à tela", &[&e.contadores.apresentados, &latencia]);
            if e.regua.lidas > 0 {
                // i18n: fora (a régua é de bancada)
                resumo.push_str(&format!(" · régua {}/{} em sequência", e.regua.em_sequencia, e.regua.lidas.saturating_sub(1)));
            }
            es.resumo = resumo;
        }
        es.mudou();
    }

    // MARK: - encerrar

    /// O Cancelar/Parar. Destrava a espera bloqueada, fecha o decoder e a janela de vídeo, e volta
    /// à tela inicial.
    pub fn encerrar(&self) {
        {
            let e = self.estado();
            if e.fase != FaseDoReceptor::Conectando && e.fase != FaseDoReceptor::Exibindo {
                return;
            }
        }
        {
            let mut e = self.estado();
            e.fase = FaseDoReceptor::Encerrando;
            e.mudou();
        }
        self.parar.store(true, Ordering::SeqCst);
        if let Some(c) = self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.cancelar();
        }
        registro::linha("cancelar exibição pedido pela interface");
    }

    fn encerrar_com(&self, conselho: String) {
        registro::linha(format!("encerrando exibição: {conselho}"));
        {
            let mut e = self.estado();
            e.conselho = conselho;
        }
        self.voltar_ao_inicio();
    }

    /// O conselho, por erro do núcleo.
    ///
    /// # A dívida 29, e por que este texto tem a forma que tem
    ///
    /// `Error::NeedsPin` é o caso **sem ambiguidade** e o texto dele pode ser direto: o outro
    /// aparelho não reconhece este pareamento e pediu para recomeçar pelo PIN.
    ///
    /// `Error::Pairing` **não** é. O núcleo de hoje devolve essa mesma variante para "o PIN não
    /// conferiu" (`pairing.rs:494`) e para "este aparelho não está pareado aqui" (`pairing.rs:563`),
    /// e os dois chegam aqui embrulhados por `session.rs:337` como *"o emissor recusou: …"*. Os
    /// dois conselhos são **opostos** — "digite o PIN de novo com cuidado" contra "peça um PIN novo
    /// ao outro aparelho" — e a interface não tem como escolher.
    ///
    /// **Não adivinho comparando string da mensagem de erro.** A frente anterior recusou fazer isso
    /// e estava certa: seria acoplar a interface ao texto de uma mensagem que o núcleo pode mudar
    /// sem aviso, e errar em silêncio quando mudasse. O texto abaixo nomeia as duas causas sem
    /// fingir saber qual é, e dá a saída — que por sorte é a mesma nos dois casos. O conserto de
    /// verdade é um status do núcleo que as separe: é o pedido registrado como dívida 29.
    fn ao_falhar(&self, erro: Error) {
        if matches!(erro, Error::Cancelled) || self.parar.load(Ordering::SeqCst) {
            self.voltar_ao_inicio();
            return;
        }
        let mut e = self.estado();
        match erro {
            Error::NeedsPin(_) => {
                e.conselho = t("O outro aparelho não reconhece mais este pareamento e pediu para recomeçar. Digite aqui o PIN que está aparecendo na tela dele.").into();
                e.pede_pin = true;
                e.oferece_desparear = true;
            }
            Error::Pairing(_) => {
                e.conselho = t("O pareamento não fechou. Ou o PIN não conferiu, ou o outro aparelho não reconhece mais este pareamento — deste lado as duas causas chegam iguais. Nos dois casos a saída é a mesma: peça o PIN que está na tela do outro aparelho e digite-o aqui.").into();
                e.pede_pin = true;
                e.oferece_desparear = true;
            }
            Error::NoRoute(_) => {
                e.conselho = t("O pareamento fechou, mas os dois aparelhos não acharam caminho um para o outro. Quase sempre é a rede: Wi-Fi de hóspede, isolamento entre aparelhos ou redes diferentes. Ponha os dois na mesma rede e tente de novo.").into();
            }
            Error::Timeout(_) => {
                e.conselho = t("O outro aparelho não respondeu. Confira se ele já clicou em Espelhar — quem exibe só entra depois de quem transmite estar esperando.").into();
            }
            Error::Signaling(_) | Error::Io(_) => {
                e.conselho = t("Não consegui falar com esse endereço. Confira o número e a porta, e se o outro aparelho está esperando.").into();
            }
            outro => e.conselho = outro.to_string(),
        }
        drop(e);
        self.voltar_ao_inicio();
    }

    fn voltar_ao_inicio(&self) {
        let mut e = self.estado();
        e.fase = FaseDoReceptor::Parado;
        e.par.clear();
        e.destino.clear();
        e.resumo.clear();
        e.cadeia.clear();
        e.cadeia_alerta = false;
        e.mudou();
        drop(e);
        *self.cancelamento.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.parar.store(false, Ordering::SeqCst);
    }

    /// O mesmo botão do emissor, e o mesmo arquivo: `pares.json` é um só por computador.
    pub fn esquecer_pares(&self) {
        identidade::esquecer_pares();
        let mut e = self.estado();
        e.oferece_desparear = false;
        e.pede_pin = true;
        e.conselho = t("Pareamentos esquecidos. Digite o PIN que está na tela do outro aparelho.").into();
        e.mudou();
    }
}


/// Limpa o controle do tocador e a linha do som quando a sessão acaba, por qualquer caminho.
struct LimpaOSom<'a>(&'a Receptor);

/// **R9b**: em qualquer saída da sessão, o controle da câmera remota sai, a janela remota fecha
/// (ela vê o controle encerrado) e a engrenagem some.
struct LimpaACamera<'a>(&'a Receptor);

impl Drop for LimpaACamera<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.0.camera_remota.lock().unwrap_or_else(|e| e.into_inner()).take() {
            c.encerrar();
        }
        let mut e = self.0.estado();
        if e.camera_remota {
            e.camera_remota = false;
            e.mudou();
        }
    }
}

impl Receptor {
    /// Uma bombeada do controle da câmera, sem espera; a situação nova vai ao estado da janela.
    fn bombear_a_camera(&self, camera: &ControleRemoto, m: &quall_core::transport::Mensageiro, situacao: &mut String) {
        use quall_core::camera_remota::mudou_no_receptor;
        let Ok(b) = camera.controlador.bombear(m, Duration::ZERO) else { return };
        if b.mudancas & (mudou_no_receptor::SITUACAO | mudou_no_receptor::CAPACIDADES) == 0 && !situacao.is_empty() {
            return;
        }
        let estado = crate::modelo_dos_ajustes_remotos::EstadoRemoto::de_json(&camera.estado_json());
        if estado.situacao != *situacao {
            registro::linha(format!("câmera remota: situação {} → {}", if situacao.is_empty() { "-" } else { situacao.as_str() }, estado.situacao));
            *situacao = estado.situacao.clone();
        }
        let com = estado.com_controles();
        let mut e = self.estado();
        if e.camera_remota != com {
            e.camera_remota = com;
            e.mudou();
        }
    }
}

impl Drop for LimpaOSom<'_> {
    fn drop(&mut self) {
        *self.0.controle_do_som.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let mut e = self.0.estado();
        if !e.som.is_empty() {
            e.som.clear();
            e.mudou();
        }
    }
}

#[cfg(test)]
mod testes {
    use super::titulo_da_exibicao;

    /// **Sem alarme, o título é só o nome do par.** Uma sessão limpa não pode parecer suja: um
    /// aviso permanente é ruído, e ruído permanente é a mesma coisa que silêncio.
    #[test]
    fn sessao_limpa_nao_ganha_alarme() {
        assert_eq!(titulo_da_exibicao("SM-A107M", None, 0, 0), "Quall — SM-A107M");
    }

    /// **O aviso aparece pela derivada.** O que decide o `⚠` é o último segundo, não o acumulado.
    #[test]
    fn o_aviso_vem_do_ultimo_segundo() {
        assert_eq!(
            titulo_da_exibicao("SM-A107M", None, 8, 27),
            "Quall — SM-A107M · ⚠ 8 quadros suspeitos agora · 27 na sessão"
        );
    }

    /// **E ele apaga quando o segundo passa limpo, sem apagar o histórico.** Uma sessão que
    /// quebrou feio e sarou continua dizendo que quebrou — sem gritar que está quebrando.
    #[test]
    fn o_alarme_apaga_e_o_acumulado_fica() {
        assert_eq!(
            titulo_da_exibicao("SM-A107M", None, 0, 27),
            "Quall — SM-A107M · 27 quadros suspeitos na sessão"
        );
    }

    /// **O que está no ar vai no título** — tamanho e quadros apresentados por segundo. É a metade
    /// do "pedido × entregue" que o receptor sabe sozinho (`docs/quem-limita-a-imagem.md`, passo 1).
    #[test]
    fn o_titulo_diz_o_que_esta_no_ar() {
        assert_eq!(
            titulo_da_exibicao("SM-S928B", Some((1920, 1080, 60)), 0, 0),
            "Quall — SM-S928B · 1920x1080 a 60 fps"
        );
        assert_eq!(
            titulo_da_exibicao("SM-S928B", Some((1920, 1080, 40)), 8, 27),
            "Quall — SM-S928B · 1920x1080 a 40 fps · ⚠ 8 quadros suspeitos agora · 27 na sessão"
        );
    }

    /// Sem nome de par — a sessão pode reportar antes de o nome chegar —, o título ainda é o do
    /// produto e não um travessão solto.
    #[test]
    fn sem_nome_do_par_o_titulo_nao_fica_manco() {
        assert_eq!(titulo_da_exibicao("", None, 0, 0), "Quall");
        assert_eq!(titulo_da_exibicao("", None, 3, 3), "Quall · ⚠ 3 quadros suspeitos agora · 3 na sessão");
    }

    /// Em inglês, o mesmo título pela tabela (a tradução EN/PT, 02/10).
    #[test]
    fn o_titulo_em_ingles() {
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert_eq!(
                titulo_da_exibicao("SM-S928B", Some((1920, 1080, 40)), 8, 27),
                "Quall — SM-S928B · 1920x1080 at 40 fps · ⚠ 8 suspect frames now · 27 this session"
            );
            assert_eq!(titulo_da_exibicao("SM-S928B", None, 0, 27), "Quall — SM-S928B · 27 suspect frames this session");
        });
    }
}

/// O refresh da tela principal, em µs (a S7: o lado da imagem da claquete é a hora do `Present`
/// mais um refresh). 60 Hz quando o GDI não diz.
fn refresh_da_tela_us() -> u64 {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        EnumDisplaySettingsExW, DEVMODEW, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
    };
    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    let ok = unsafe {
        EnumDisplaySettingsExW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut dm, ENUM_DISPLAY_SETTINGS_FLAGS(0))
    };
    let hz = if ok.as_bool() && dm.dmDisplayFrequency > 1 { dm.dmDisplayFrequency } else { 60 };
    1_000_000 / u64::from(hz)
}
