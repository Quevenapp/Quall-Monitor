//! A oficina: monta o encoder **de reserva fora do laço**, para a quinta porta poder trocar por
//! ponteiro em vez de parar o fluxo.
//!
//! # O número que justifica este módulo
//!
//! Medido em 2026-08-30 no Dell, no MFT de Quick Sync, por `quall-gemeos` (n=10, mediana):
//!
//! | passo da recriação de hoje | custo |
//! |---|---|
//! | `desligar` o velho | 18,0 ms |
//! | `ActivateObject` do novo | 74,2 ms |
//! | `configure` (SET_D3D_MANAGER + tipos + ICodecAPI) | 45,1 ms |
//! | abrir o fluxo + subir o bombeador | 1,1 ms |
//! | **total, dentro do laço** | **138,6 ms** |
//!
//! Nenhum desses passos precisa do quadro que está passando. Se eles rodarem numa thread de
//! fundo, o que sobra no laço é trocar dois ponteiros — **0 µs medidos**.
//!
//! # As duas vagas, e por que não basta uma
//!
//! `IMFActivate::ActivateObject`, chamado de novo enquanto o objeto anterior **ainda está vivo**,
//! devolve o **mesmo** objeto em cache — medido, ponteiro a ponteiro: `0x1e0ebe858a0` nas duas
//! chamadas. É por isso que a quinta porta de hoje funciona (ela desliga antes de reativar) e é
//! por isso que a troca a quente **não pode** reusar um `IMFActivate` só: pedir a reserva ao
//! mesmo activate devolveria o encoder que está em uso.
//!
//! Duas vagas bastam, e se alternam sozinhas: quando o aposentado é desligado, o `IMFActivate`
//! dele fica livre para hospedar o sucessor da troca seguinte. Nunca é preciso reenumerar o
//! registro de MFTs no meio da sessão.
//!
//! # O que foi medido antes de escrever isto, e o que não foi
//!
//! Medido (`quall-gemeos`, Quick Sync, Sessão 0): dois MFTs de hardware **coexistem** no mesmo
//! `IMFDXGIDeviceManager` e no mesmo dispositivo D3D11, os dois produzindo ao mesmo tempo (60
//! quadros cada, nenhum `ProcessInput` recusado); o encoder de reserva ocioso custa **+16
//! handles** e ~12 MB; e vinte trocas a quente deixaram **+3,05 handles por troca** — o mesmo
//! número da recriação de hoje, ou seja a reserva **não dobra** o vazamento.
//!
//! Não medido ali, e por isso está escrito: a montagem numa **thread de fundo** enquanto a
//! principal codifica. É o que esta oficina faz, e quem a mede é o próprio `quall-app` em
//! corrida com receptor — `trocas_sem_reserva` e `montagem_de_fundo_ms` são as testemunhas.

#![cfg(windows)]

use std::thread::{self, JoinHandle};
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use windows::core::Result;
use windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::encoder::{self, ChosenEncoder, EncoderConfig, MftEvent};
use crate::registro;

/// Um encoder pronto para entrar em serviço: configurado, com o fluxo aberto e com o bombeador de
/// eventos já rodando.
pub struct Reserva {
    pub enc: ChosenEncoder,
    pub eventos: Receiver<MftEvent>,
    /// O desligamento do encoder que ocupava esta vaga saiu limpo? `None` quando não havia
    /// encoder anterior (a primeira reserva da sessão).
    pub desligamento_limpo: Option<bool>,
    /// `MF_MT_MAX_KEYFRAME_SPACING` foi aceito neste transform? A regra da casa é conferir no
    /// fluxo, e quem confere é `transmissao.rs`; aqui só se carrega o que a API respondeu.
    pub espacamento: bool,
    /// Quanto a montagem levou **na thread de fundo**. Não é custo do laço; é o que diz se a
    /// reserva chega a tempo entre duas recriações.
    pub montagem_ms: u64,
}

enum Pedido {
    /// A primeira reserva da sessão: não há vaga, então a oficina enumera o registro de MFTs.
    Semear,
    /// Um encoder aposentado. A oficina o desliga e monta o sucessor **no `IMFActivate` dele**.
    Reciclar(ChosenEncoder),
    Parar,
}

// --- as três travessias de thread, cada uma escopada a um tipo -------------------------------
//
// `windows-core` não implementa `Send` para interfaces COM de propósito: a segurança de cruzar
// thread depende do modelo de apartamento, que a interface por si não sabe. Nós sabemos — o
// processo inteiro roda em MTA (`CoInitializeEx(COINIT_MULTITHREADED)` em `main.rs`, e a thread
// desta oficina entra no mesmo apartamento), e em MTA chamar a mesma interface de threads
// diferentes sem marshaling é válido. É exatamente o raciocínio (e a nota) que
// `encoder::spawn_event_pump` já carrega desde o M1.
//
// Os três portadores são **concretos**, e não um `Carga<T>` genérico, para que o `unsafe impl`
// nomeie exatamente o que atravessa.

struct PedidoEnviavel(Pedido);
unsafe impl Send for PedidoEnviavel {}
impl PedidoEnviavel {
    // Um método, e não o campo `.0` direto: a captura precisa (edition 2021) capturaria só o
    // campo, que não é `Send`, e o `unsafe impl` acima perderia o efeito.
    fn abrir(self) -> Pedido {
        self.0
    }
}

struct ReservaEnviavel(Result<Reserva>);
unsafe impl Send for ReservaEnviavel {}
impl ReservaEnviavel {
    fn abrir(self) -> Result<Reserva> {
        self.0
    }
}

struct BaseEnviavel {
    gerenciador: IMFDXGIDeviceManager,
    cfg: EncoderConfig,
}
unsafe impl Send for BaseEnviavel {}
impl BaseEnviavel {
    fn abrir(self) -> (IMFDXGIDeviceManager, EncoderConfig) {
        (self.gerenciador, self.cfg)
    }
}

pub struct Oficina {
    pedidos: Sender<PedidoEnviavel>,
    prontas: Receiver<ReservaEnviavel>,
    thread: Option<JoinHandle<()>>,
    /// Há um pedido em voo? Sem isto a oficina montaria uma reserva por volta do laço.
    ocupada: bool,
}

impl Oficina {
    /// Sobe a thread e já pede a primeira reserva.
    ///
    /// `preferencia` é a do encoder em serviço: a primeira reserva enumera o registro de MFTs, e
    /// sem ela uma cadeia aberta no Quick Sync por `Preferencia::Intel` semearia uma reserva da
    /// NVIDIA (a ordem de produto prefere NVIDIA), que o `configure` recusa com `E_INVALIDARG`
    /// contra o gerenciador do adaptador Intel (`device.rs`) — revisão adversarial de 13/09/2026.
    ///
    /// `placa` é o LUID quando a cadeia fixou a placa (`encoder::ativar_h264_na_placa`): aí a primeira
    /// reserva nasce no MFT dessa placa, e a `preferencia` não é consultada. `None` é o caminho de
    /// sempre (a revisão adversarial de 18/09, M5).
    ///
    /// `estrita` desliga o recuo "só Intel" da placa fixada: é a câmera (`encoder::ativar_h264_da_placa`,
    /// a revisão do código da fase 3, m7).
    pub fn abrir(
        gerenciador: &IMFDXGIDeviceManager,
        cfg: EncoderConfig,
        preferencia: encoder::Preferencia,
        placa: Option<u64>,
        estrita: bool,
    ) -> Oficina {
        let (tx_pedido, rx_pedido) = unbounded::<PedidoEnviavel>();
        let (tx_pronta, rx_pronta) = unbounded::<ReservaEnviavel>();
        let base = BaseEnviavel { gerenciador: gerenciador.clone(), cfg };
        // A thread da oficina escreve com o prefixo da sessão que a abriu (`registro.rs`).
        let prefixo = registro::prefixo_desta_thread();

        let thread = thread::spawn(move || {
            registro::prefixar_esta_thread(&prefixo);
            let (gerenciador, cfg) = base.abrir();
            // A thread precisa entrar no apartamento. `MFStartup` é por processo e já foi feito;
            // o apartamento é por thread.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if com.is_err() {
                registro::linha(format!(
                    "oficina: CoInitializeEx falhou ({com:?}); a troca a quente não vai funcionar"
                ));
            }
            while let Ok(pedido) = rx_pedido.recv() {
                let pedido = pedido.abrir();
                let resultado = match pedido {
                    Pedido::Parar => break,
                    Pedido::Semear => montar(None, &gerenciador, &cfg, preferencia, placa, estrita),
                    Pedido::Reciclar(velho) => montar(Some(velho), &gerenciador, &cfg, preferencia, placa, estrita),
                };
                if tx_pronta.send(ReservaEnviavel(resultado)).is_err() {
                    break;
                }
            }
            unsafe { CoUninitialize() };
        });

        let oficina = Oficina {
            pedidos: tx_pedido,
            prontas: rx_pronta,
            thread: Some(thread),
            ocupada: false,
        };
        oficina.pedir(Pedido::Semear);
        oficina
    }

    fn pedir(&self, p: Pedido) {
        let _ = self.pedidos.send(PedidoEnviavel(p));
    }

    /// Manda o encoder aposentado para a oficina: ela o desliga e monta o sucessor no
    /// `IMFActivate` dele.
    pub fn reciclar(&mut self, velho: ChosenEncoder) {
        self.ocupada = true;
        self.pedir(Pedido::Reciclar(velho));
    }

    /// A reserva já ficou pronta? Não bloqueia nunca — é chamada a cada volta do laço.
    pub fn colher(&mut self) -> Option<Result<Reserva>> {
        match self.prontas.try_recv() {
            Ok(r) => {
                self.ocupada = false;
                Some(r.abrir())
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.ocupada = false;
                None
            }
        }
    }

    pub fn ocupada(&self) -> bool {
        self.ocupada
    }

    /// **Fecha e espera**, com prazo, e devolve as reservas que ficaram prontas depois do último
    /// `colher` — quem chama as desliga.
    ///
    /// O `Drop` de baixo não espera, por um motivo que vale para o fim de **uma** sessão: esperar
    /// um `ActivateObject` de ~75 ms no fechamento seria uma espera que o usuário sente. Com vários
    /// receptores o fim de uma sessão não é o fim do processo, e a thread largada com um MFT dentro
    /// é um encoder de hardware vivo sem dono, somado a cada "sai e volta" (revisão adversarial de
    /// 13/09/2026). Então o emissor com várias sessões chama isto; o caminho de uma sessão só segue
    /// com o `Drop` de sempre.
    ///
    /// Quando o prazo vence a thread segue sozinha, e o registro diz.
    pub fn fechar_e_esperar(mut self, prazo: std::time::Duration) -> Vec<Reserva> {
        self.pedir(Pedido::Parar);
        let comeco = Instant::now();
        let thread = self.thread.take();
        if let Some(t) = &thread {
            while !t.is_finished() && comeco.elapsed() < prazo {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let terminou = thread.as_ref().map(|t| t.is_finished()).unwrap_or(true);
        if terminou {
            if let Some(t) = thread {
                let _ = t.join();
            }
        } else {
            registro::linha(format!(
                "!! a oficina não terminou em {} ms — a thread dela segue sozinha",
                prazo.as_millis()
            ));
        }
        let mut sobras = Vec::new();
        while let Ok(r) = self.prontas.try_recv() {
            if let Ok(r) = r.abrir() {
                sobras.push(r);
            }
        }
        sobras
    }
}

impl Drop for Oficina {
    fn drop(&mut self) {
        self.pedir(Pedido::Parar);
        // Não damos `join`: a thread pode estar no meio de um `ActivateObject` de ~75 ms, e
        // esperar por ela no caminho de fechamento da sessão trocaria um vazamento que não existe
        // por uma espera que o usuário sente. Ela sai sozinha ao ver o `Parar` ou o canal fechado.
        let _ = self.thread.take();
    }
}

/// Desliga o velho (se houver) e monta o sucessor. Roda **na thread da oficina**.
fn montar(
    velho: Option<ChosenEncoder>,
    gerenciador: &IMFDXGIDeviceManager,
    cfg: &EncoderConfig,
    preferencia: encoder::Preferencia,
    placa: Option<u64>,
    estrita: bool,
) -> Result<Reserva> {
    let comeco = Instant::now();
    let (base, limpo) = match velho {
        Some(v) => {
            let limpo = encoder::desligar(&v);
            (Some(v), Some(limpo))
        }
        None => (None, None),
    };
    // Com a vaga desocupada, `ActivateObject` no mesmo `IMFActivate` cria um objeto novo — é o
    // que a quinta porta já faz hoje, e o que a medição de ponteiro de `quall-gemeos` confirma
    // pelo avesso (com o objeto **vivo**, a mesma chamada devolve o de cache).
    let enc = match &base {
        Some(v) => encoder::reativar(v)?,
        // **A placa, quando a cadeia a fixou pelo LUID** (o monitor virtual; a câmera, na fase 3): a
        // primeira reserva nasce no MFT daquela placa, e não no primeiro da ordem de produto — que na
        // Sessão 0 é a NVIDIA e o `configure` recusaria com `E_INVALIDARG` contra o gerenciador da
        // Intel (a revisão adversarial de 18/09, M5).
        None => match placa {
            Some(luid) => {
                let (enc, como) = encoder::ativar_h264_da_placa(luid, estrita)?;
                // Por onde o MFT da reserva veio. Sem isto, uma reserva que viesse do recuo "só
                // Intel" (outra placa, numa máquina com duas Intel) e o `configure` recusasse só
                // deixaria "a oficina não conseguiu montar a reserva" (a revisão de código, m4).
                // O LUID no mesmo formato de "encoder da placa …" (`sessao_de_emissao.rs`): a
                // conferência "a reserva nasce na placa do encoder em serviço" é casar texto.
                registro::linha(format!("oficina: reserva da placa {luid:016X}: \"{}\" ({como})", enc.friendly_name));
                enc
            }
            None => encoder::ativar_h264(preferencia)?,
        },
    };
    encoder::configure(&enc, gerenciador, cfg)?;
    let espacamento = encoder::tentar_espacamento_de_idr(&enc, cfg.gop_frames);
    encoder::start_stream(&enc.transform)?;
    let eventos = encoder::spawn_event_pump(enc.events.clone());
    Ok(Reserva {
        enc,
        eventos,
        desligamento_limpo: limpo,
        espacamento,
        montagem_ms: comeco.elapsed().as_millis() as u64,
    })
}
