//! Onde a rede e a tela se encontram: track → decoder em hardware → Video Processor → janela.
//!
//! É o espelho de `transmissao.rs`, e as duas se parecem de propósito — mas o que atravessa a
//! fronteira aqui vai no sentido contrário.
//!
//! # O que este módulo não faz, e por que isso é o desenho
//!
//! **Não toca no núcleo.** Ele não conhece `TrackReceptor`, `Ready` nem `Session`: recebe
//! [`QuadroRecebido`] já copiado e devolve contadores. Quem fala com o núcleo é `receptor.rs`, numa
//! variável só, na thread da sessão — a mesma regra que `emissor.rs` segue pelo mesmo motivo (a
//! exceção da libdatachannel sobe de dentro do `lock_guard` do mutex global sem soltá-lo).
//!
//! **Não escreve arquivo nenhum.** Não há `--salvar`, não há caminho para `.bmp`, não há caminho
//! para `.h264`. A sonda `quall-receiver-probe` tem um (`--snapshot-out`) porque a origem dela é um
//! arquivo de bancada escolhido à mão; aqui a origem é a tela de outro aparelho, e a forma mais
//! barata de nunca vazar isso é **não ter para onde gravar**. É a mesma decisão que tirou o
//! `--salvar` do `prova-rede.ps1` e que fez `transmissao.rs` nascer sem caminho para disco.
//!
//! A única leitura de pixel que existe aqui é a régua de blocos (`regua.rs`), que devolve **um
//! inteiro por quadro** e fica atrás de um sinalizador de bancada.
//!
//! # A resolução vem do SPS, não de um palpite
//!
//! `decoder::configure` precisa de largura e altura **antes** do primeiro quadro, e o receptor não
//! as recebe pelo protocolo: o rótulo da track traz o nome do monitor, não o tamanho dele. A
//! resposta está no próprio fluxo — `sps.rs` já sabe ler `1920x1080` do conjunto de parâmetros, e é
//! o que este módulo usa. Chutar 1920x1080 funcionaria nesta bancada e quebraria calado no dia em
//! que um celular emitisse 720x1280 em pé.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::Context;
use crossbeam_channel::Receiver;

use windows::Win32::Media::MediaFoundation::MF_E_NOTACCEPTING;

use crate::etapas::Etapa;
use crate::fluidez::Fluidez;
use crate::regua;
use crate::registro;
use crate::{decoder, device, encoder, present};

/// Um quadro que chegou pela track, **já copiado** da fatia efêmera que o núcleo entrega.
///
/// A cópia não é escolha: `QuadroCodificado::annexb` vale só durante a chamada do tratador, que
/// roda numa thread da libdatachannel. Decodificar ali dentro seguraria a recepção da sessão
/// inteira — inclusive o RTCP —, então o tratador copia e vai embora.
pub struct QuadroRecebido {
    pub bytes: Vec<u8>,
    pub timestamp_us: u64,
    pub idr: bool,
    /// Quando este quadro entrou na fila, para medir fila→apresentado sem depender de os dois
    /// aparelhos terem o mesmo relógio.
    pub chegou_em: Instant,
    /// **A referência deste quadro foi condenada e a porta está ligada**: ele decodifica, e não vai
    /// para a tela. Ver [`crate::cadeia`] e a porta em [`Exibicao::bombear`].
    ///
    /// Nasce `false` no tratador da libdatachannel, que não tem como saber — quem classifica é a
    /// thread da sessão, no instante em que tira o quadro da fila. É por isso que a marca é um
    /// campo e não um parâmetro de [`Exibicao::submeter`]: um quadro que o MFT recusa volta pela
    /// variável `pendente` do laço e **não** pode ser classificado duas vezes.
    pub suspeito: bool,
}

/// Um quadro que entrou no decoder e ainda não saiu. É o trilho por onde a marca da condenação
/// viaja da entrada (`submeter`) até a saída (`bombear`), junto com o instante de chegada que mede
/// fila→tela.
struct Submetido {
    chegou_em: Instant,
    suspeito: bool,
    /// O carimbo do quadro (µs na base da track), para a claquete (S7) casar a imagem com a captura.
    timestamp_us: u64,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Contadores {
    pub submetidos: u64,
    pub decodificados: u64,
    pub apresentados: u64,
    pub recusados_pelo_decoder: u64,
    pub sem_correspondencia: u64,
    /// **Quantos a porta segurou** — decodificados e não apresentados porque a referência deles
    /// tinha sido condenada. É o quinto nome da tabela de `docs/contrato-track.md`, e o único dos
    /// cinco que depende de a porta estar ligada.
    pub retidos: u64,
    soma_latencia_us: u64,
    pub pior_latencia_us: u64,
}

impl Contadores {
    pub fn latencia_media_ms(&self) -> f64 {
        if self.apresentados == 0 {
            0.0
        } else {
            self.soma_latencia_us as f64 / self.apresentados as f64 / 1000.0
        }
    }

    pub fn linha(&self) -> String {
        format!(
            "submetidos={} decodificados={} apresentados={} recusados={} retidos={} \
             fila→tela media={:.1} ms pior={:.1} ms",
            self.submetidos,
            self.decodificados,
            self.apresentados,
            self.recusados_pelo_decoder,
            self.retidos,
            self.latencia_media_ms(),
            self.pior_latencia_us as f64 / 1000.0,
        )
    }
}

/// **O custo de cada parcela de um quadro dentro do laço**, em µs. Ver `crate::etapas` para a
/// atribuição errada que fez isto existir.
pub struct Custos {
    /// `ProcessInput` — empacotar e entregar o quadro comprimido ao MFT.
    pub entrada: Etapa,
    /// `ProcessOutput` que devolveu um quadro — é aqui que um decode síncrono espera a GPU.
    pub saida: Etapa,
    /// `present_frame`: `VideoProcessorBlt` para RGB e `Present(0)`.
    pub tela: Etapa,
    /// A câmera virtual, a metade que espera: `Map` da leitura e a cópia para o buffer.
    pub colher: Etapa,
    /// A câmera virtual, a metade que enfileira: `VideoProcessorBlt` para NV12 e `CopyResource`.
    pub submeter: Etapa,
    /// `colher` + `submeter` na mesma volta — a grandeza que §8.63 mediu. Só o braço de 09/09
    /// (`--camera-em-todo-quadro`) a preenche; no caminho de agora as duas metades não acontecem
    /// na mesma volta.
    pub escala: Etapa,
}

impl Custos {
    fn novos() -> Self {
        Custos {
            entrada: Etapa::nova("entrada_us"),
            saida: Etapa::nova("saida_us"),
            tela: Etapa::nova("tela_us"), // i18n: fora (diário e detalhe técnico)
            colher: Etapa::nova("colher_us"),
            submeter: Etapa::nova("submeter_us"),
            escala: Etapa::nova("escala_us"),
        }
    }

    pub fn linha(&self) -> String {
        let mut partes = vec![self.entrada.linha(), self.saida.linha(), self.tela.linha()];
        if !self.colher.vazia() || !self.submeter.vazia() {
            partes.push(self.colher.linha());
            partes.push(self.submeter.linha());
        }
        partes.join(" ")
    }
}

/// A cadeia de exibição, inteira, numa struct. Vive na thread da sessão — a janela de vídeo é
/// criada aqui e por isso as mensagens dela são bombeadas aqui, na mesma thread que a criou.
pub struct Exibicao {
    janela: present::Window,
    apresentador: present::Presenter,
    decodificador: decoder::ChosenDecoder,
    eventos: Option<Receiver<encoder::MftEvent>>,
    creditos: u32,
    submetidos: VecDeque<Submetido>,
    duracao_100ns: i64,
    proximo_seq: i64,
    leitor: Option<regua::Leitor>,
    pub regua: regua::Contagem,
    pub largura: u32,
    pub altura: u32,
    /// O tamanho da **textura** que o decoder entrega, e o recorte dela que é imagem. Em 1080p
    /// são 1920x1088 e 1920x1080: `largura`/`altura` acima são a imagem; estes dois são o que a
    /// câmera virtual precisa para não escalar as linhas de enchimento. Ver
    /// [`decoder::abertura_de_saida`].
    codificada: (u32, u32),
    visivel: windows::Win32::Foundation::RECT,
    saida_largura: u32,
    saida_altura: u32,
    pub nome_do_decoder: String,
    pub decoder_e_hardware: bool,
    pub adaptador: String,
    pub primeira_imagem: Option<Instant>,
    pub contadores: Contadores,
    /// **A distribuição dos intervalos entre apresentações.** Mora aqui porque é aqui que o quadro
    /// vai para o vidro — ver `crate::fluidez` para por que o intervalo medido é entre
    /// apresentações e não entre chegadas.
    pub fluidez: Fluidez,
    /// A câmera virtual deste aparelho, quando o app está expondo uma. Ver `crate::baia`.
    ///
    /// **A publicação acontece depois do `present_frame`, e não antes**, por uma razão de
    /// prioridade: a janela é o que a pessoa está olhando, e a conversão para NV12 passa por um
    /// `Map` do contexto imediato — pôr isso na frente da apresentação seria dar à câmera a
    /// preferência que é do vidro. **Depois, mas não dependendo dele** (o G1 de 21/09): um
    /// `present_frame` que falha não tira o quadro da câmera.
    camera: Option<std::sync::Arc<crate::baia::Baia>>,
    escalador: Option<crate::escala_nv12::Escalador>,
    buffer_nv12: Vec<u8>,
    /// Buffers já publicados, para reaproveitar. Ver [`publicar_na_baia`].
    reserva_nv12: Vec<std::sync::Arc<Vec<u8>>>,
    /// Quantos quadros foram para o cano da câmera virtual. Comparável com `apresentados`.
    pub publicados_na_camera: u64,
    /// **O custo de cada parcela do quadro, em µs** — inclusive a conversão para NV12, que a
    /// revisão adversarial desta frente mandou medir **antes** de ligar a câmera por padrão, e eu
    /// liguei sem medir. Ver `Custos`.
    pub custos: Custos,
    /// **Braço de bancada**: o comportamento de 09/09 — converter **todo** quadro apresentado, com
    /// ou sem alguém lendo, colhendo o anterior **esperando** a GPU. Ver [`Exibicao::ligar_camera`].
    camera_em_todo_quadro: bool,
    /// Quadros apresentados que **não** foram convertidos, por motivo. Ver `linha_da_camera`.
    camera_sem_leitor: u64,
    /// `--claquete` (bancada, a S7, T2 do `docs/som-no-receptor.md` §9.4): o índice da régua de
    /// cada quadro apresentado, com a hora do `Present` no QPC (µs) e o carimbo do quadro. O lado
    /// da imagem do Δ é essa hora mais um refresh declarado — como a testemunha B do Mac, e **sem**
    /// as estatísticas de apresentação do DXGI.
    pub claquete: bool,
    imagens_da_claquete: Vec<(u32, u64, u64)>,
    camera_anel_cheio: u64,
    /// Tentativas de colher que acharam a GPU ainda escrevendo — cada uma é uma espera que o
    /// `Map` antigo teria feito dentro do laço.
    camera_ainda_na_gpu: u64,
    /// **A textura que o decoder devolve, descrita uma vez.** `D3D11_BIND_DECODER` nela é o que
    /// separa "o MFT decodificou por DXVA" de "decodificou em software e subiu para a GPU" — a
    /// revisão de 10/09/2026 apontou que a saída ser textura D3D11 não prova a primeira.
    textura_descrita: bool,
    /// Índices de subrecurso distintos vistos na saída (até 64). Um pool de superfícies de decode
    /// gira vários; uma textura só, sempre no índice 0, é outro caminho.
    subrecursos_vistos: u64,
    /// **O dispositivo D3D11 caiu**, com o motivo que o driver deu. Daqui em diante nenhuma chamada
    /// nele funciona, e esta exibição não volta: quem a abriu larga e abre outra. Ver
    /// [`Exibicao::gpu_caiu`].
    gpu_caiu: Option<windows::core::HRESULT>,
    /// `--simular-queda-da-gpu`: o instante em que esta exibição finge que o dispositivo caiu.
    queda_simulada_em: Option<Instant>,
}

/// Quantos buffers já publicados a exibição guarda para reaproveitar.
const RESERVA_NV12: usize = 4;

fn sim_nao(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "sim",
        Some(false) => "não", // i18n: fora (diário e detalhe técnico)
        None => "?",
    }
}

/// Entrega o buffer convertido à baia e deixa outro no lugar — **reaproveitado** quando algum dos
/// publicados antes já não está com ninguém.
///
/// *(Até 10/09/2026 o lugar era ocupado por um `vec![0u8; 3,11 MB]` novo a cada quadro — alocado,
/// zerado e com as páginas tocadas pela primeira vez dentro do laço que apresenta. O comentário
/// dizia "mesma alocação", e não era. Apontado pela revisão adversarial daquele dia.)*
///
/// Um buffer da reserva só é reaproveitado com `strong_count == 1`: a exibição é a única dona, o
/// distribuidor já passou para um quadro mais novo e nenhum fio do cano está escrevendo aquele.
/// Não é preciso zerá-lo — a colheita escreve todas as linhas de Y e de UV.
///
/// Função solta, e não método, por causa do empréstimo: quem chama já segura `&self.camera` e
/// `&self.escalador`, e um `&mut self` aqui não compilaria.
fn publicar_na_baia(
    baia: &crate::baia::Baia,
    buffer: &mut Vec<u8>,
    reserva: &mut Vec<std::sync::Arc<Vec<u8>>>,
) {
    use std::sync::Arc;
    let livre = reserva.iter().position(|a| Arc::strong_count(a) == 1);
    let proximo = match livre {
        Some(i) => Arc::try_unwrap(reserva.swap_remove(i)).unwrap_or_else(|a| (*a).clone()),
        None => vec![0u8; crate::cano::BYTES_NV12],
    };
    let pronto = Arc::new(std::mem::replace(buffer, proximo));
    reserva.push(Arc::clone(&pronto));
    if reserva.len() > RESERVA_NV12 {
        reserva.remove(0);
    }
    baia.publicar(pronto);
}

impl Exibicao {
    /// Abre decoder, janela e Video Processor. `com_regua` liga a leitura da régua de blocos —
    /// **bancada apenas**, e só faz sentido quando a origem é a fonte sintética.
    pub fn abrir(
        largura: u32,
        altura: u32,
        fps: u32,
        titulo: &str,
        escala: f64,
        com_regua: bool,
        buffers_da_janela: u32,
        protecao_multithread: bool,
        simular_queda_apos: Option<Duration>,
    ) -> anyhow::Result<Self> {
        let decodificador = decoder::find_and_activate_h264_decoder()
            .context("nenhum MFT de decode H.264 ativou")?; // i18n: fora (diário e detalhe técnico)
        registro::linha(format!(
            "decoder: \"{}\" assincrono={} d3d11_aware={}",
            decodificador.friendly_name, decodificador.is_async, decodificador.is_hardware
        ));

        // Mesma restrição do lado do encoder: `MFT_MESSAGE_SET_D3D_MANAGER` exige o dispositivo
        // D3D11 **no mesmo adaptador** do MFT escolhido, e a swap chain tem de nascer nesse mesmo
        // dispositivo (senão o DXGI copia entre adaptadores por baixo dos panos, 7,33 ms por
        // quadro medidos no M1).
        let palpite = if decodificador.friendly_name.to_uppercase().contains("NVIDIA") {
            device::VENDOR_NVIDIA
        } else {
            device::VENDOR_INTEL
        };
        let adaptador = device::create_device(palpite).context("criar dispositivo D3D11")?;
        registro::linha(format!(
            "adaptador de decode+exibição: {} (vendor 0x{:04X}) luid=0x{:016X}",
            adaptador.description, adaptador.vendor_id, adaptador.luid
        ));

        // **A proteção multithread entra antes de o decoder receber o dispositivo**: dali em
        // diante há threads do MFT no mesmo contexto. Ver `device::proteger_contexto`.
        let ao_criar = device::contexto_protegido(&adaptador.context);
        if protecao_multithread {
            if let Err(erro) = device::proteger_contexto(&adaptador.context, true) {
                registro::linha(format!(
                    "d3d11: não consegui ligar a proteção multithread — {erro}"
                ));
            }
        }

        let gerente = encoder::create_device_manager(&adaptador.device)
            .context("criar IMFDXGIDeviceManager")?;
        decoder::configure(
            &decodificador,
            &gerente,
            &decoder::DecoderConfig { width: largura, height: altura, fps },
        )
        .context("configurar o MFT de decode")?; // i18n: fora (diário e detalhe técnico)
        // O estado **depois** de o MFT receber o dispositivo é a leitura que decide a hipótese:
        // no braço sem proteção, `depois_do_mft=sim` quer dizer que o Media Foundation a liga
        // sozinho — e então a falta dela não explica a queda.
        registro::linha(format!(
            "d3d11: proteção multithread ao_criar={} depois_do_mft={}{}",
            sim_nao(ao_criar),
            sim_nao(device::contexto_protegido(&adaptador.context)),
            if protecao_multithread {
                " (ligada por este app)"
            } else {
                " — BANCADA: --sem-protecao-multithread, este app não liga"
            }
        ));
        decoder::start_stream(&decodificador.transform).context("iniciar o fluxo do decoder")?; // i18n: fora (diário e detalhe técnico)

        let eventos = decodificador
            .events
            .clone()
            .map(encoder::spawn_event_pump);

        let saida_largura = ((largura as f64) * escala).round().max(160.0) as u32;
        let saida_altura = ((altura as f64) * escala).round().max(90.0) as u32;
        let janela = present::Window::create(titulo, saida_largura, saida_altura)
            .context("criar a janela de vídeo")?; // i18n: fora (diário e detalhe técnico)
        let apresentador = present::Presenter::new(
            &adaptador.device,
            janela.hwnd,
            largura,
            altura,
            saida_largura,
            saida_altura,
            fps,
            buffers_da_janela,
        )
        .context("criar a swap chain e o Video Processor")?; // i18n: fora (diário e detalhe técnico)
        if buffers_da_janela != 2 {
            registro::linha(format!(
                "janela de vídeo: {buffers_da_janela} buffers na swap chain (braço de bancada; produto: 2)"
            ));
        }

        let leitor = if com_regua {
            match regua::Leitor::novo(&adaptador.device, largura, altura) {
                Ok(l) => {
                    registro::linha("regua: ligada (bancada) — lê 4 blocos e publica um inteiro");
                    Some(l)
                }
                Err(erro) => {
                    registro::linha(format!("regua: NÃO subiu ({erro}) — seguindo sem ela"));
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            janela,
            apresentador,
            eventos,
            creditos: 0,
            submetidos: VecDeque::new(),
            duracao_100ns: 10_000_000i64 / fps.max(1) as i64,
            proximo_seq: 0,
            leitor,
            regua: regua::Contagem::default(),
            largura,
            altura,
            codificada: (largura, altura),
            visivel: windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: largura as i32,
                bottom: altura as i32,
            },
            saida_largura,
            saida_altura,
            nome_do_decoder: decodificador.friendly_name.clone(),
            decoder_e_hardware: decodificador.is_hardware,
            adaptador: adaptador.description.clone(),
            primeira_imagem: None,
            contadores: Contadores::default(),
            fluidez: Fluidez::nova(),
            camera: None,
            escalador: None,
            buffer_nv12: Vec::new(),
            reserva_nv12: Vec::new(),
            publicados_na_camera: 0,
            custos: Custos::novos(),
            camera_em_todo_quadro: false,
            camera_sem_leitor: 0,
            claquete: false,
            imagens_da_claquete: Vec::new(),
            camera_anel_cheio: 0,
            camera_ainda_na_gpu: 0,
            textura_descrita: false,
            subrecursos_vistos: 0,
            gpu_caiu: None,
            queda_simulada_em: simular_queda_apos.map(|d| Instant::now() + d),
            decodificador,
        })
    }

    /// **O dispositivo caiu?** `Some(motivo)` depois da primeira falha em que o driver confirmou a
    /// queda. O receptor larga a exibição e abre outra no IDR seguinte — dispositivo, decoder e
    /// janela novos. Até 10/09/2026 ninguém perguntava: a janela ficava no último quadro e o laço
    /// escrevia uma linha de falha por quadro, 5.970 numa sessão.
    pub fn gpu_caiu(&self) -> Option<windows::core::HRESULT> {
        self.gpu_caiu
    }

    /// Uma chamada na GPU falhou: pergunta ao driver se o dispositivo caiu, e registra o motivo
    /// **uma vez**. `true` quando caiu — o chamador para de tocar na GPU.
    fn conferir_gpu(&mut self, onde: &str, erro: &str) -> bool {
        if self.gpu_caiu.is_some() {
            return true;
        }
        let Some(motivo) = device::motivo_da_queda(self.apresentador.device()) else {
            return false;
        };
        self.gpu_caiu = Some(motivo);
        let protegido = unsafe { self.apresentador.device().GetImmediateContext() }
            .ok()
            .and_then(|c| device::contexto_protegido(&c));
        registro::linha(format!(
            "gpu: o dispositivo D3D11 caiu — {onde} falhou ({erro}); motivo do driver: 0x{:08X} {} \
             | proteção multithread={} | {} apresentados, {} decodificados até aqui",
            motivo.0 as u32,
            device::nome_do_motivo(motivo),
            sim_nao(protegido),
            self.contadores.apresentados,
            self.contadores.decodificados,
        ));
        true
    }

    /// **O alarme da cadeia na barra de título da janela do vídeo.** Ver
    /// [`present::Window::titular`] para por que ele mora aqui e não no painel: o painel está na
    /// outra janela, e a outra janela não é a que tem a imagem.
    pub fn titular(&mut self, texto: &str) {
        self.janela.titular(texto);
    }

    /// Escoa a fila de mensagens da janela de vídeo. `false` = a pessoa fechou a janela.
    pub fn janela_viva(&self) -> bool {
        self.janela.pump()
    }

    /// Entrega um quadro ao decoder. `false` quando o MFT recusou por estar cheio — o chamador
    /// **não** deve descartar o quadro: a próxima volta drena e tenta de novo.
    pub fn submeter(&mut self, quadro: &QuadroRecebido) -> bool {
        // Com o dispositivo caído o quadro não tem para onde ir; "consumido" é o que deixa o
        // receptor seguir até largar esta exibição.
        if self.gpu_caiu.is_some() {
            return true;
        }
        if self.decodificador.is_async && self.creditos == 0 {
            return false;
        }
        let tempo = self.proximo_seq * self.duracao_100ns;
        let amostra = match decoder::sample_from_bytes(&quadro.bytes, tempo, self.duracao_100ns) {
            Ok(a) => a,
            Err(erro) => {
                registro::linha(format!("exibicao: não empacotei o quadro como amostra: {erro}"));
                return true; // não adianta tentar de novo com o mesmo quadro
            }
        };
        let comecou = Instant::now();
        let entregue = unsafe { self.decodificador.transform.ProcessInput(0, &amostra, 0) };
        self.custos.entrada.desde(comecou);
        match entregue {
            Ok(()) => {
                self.submetidos.push_back(Submetido {
                    chegou_em: quadro.chegou_em,
                    suspeito: quadro.suspeito,
                    timestamp_us: quadro.timestamp_us,
                });
                self.proximo_seq += 1;
                self.contadores.submetidos += 1;
                if self.decodificador.is_async {
                    self.creditos -= 1;
                }
                true
            }
            // Normal no MFT síncrono: ele está pedindo um `ProcessOutput` antes de aceitar mais.
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                self.contadores.recusados_pelo_decoder += 1;
                false
            }
            Err(e) => {
                registro::linha(format!("exibicao: ProcessInput recusou o quadro: {e}"));
                self.contadores.recusados_pelo_decoder += 1;
                true
            }
        }
    }

    /// Uma volta do laço: colhe eventos do MFT, drena o que já decodificou e apresenta.
    ///
    /// Devolve quantos quadros foram apresentados nesta volta — o número que responde "chegou
    /// imagem?" sem olhar um pixel.
    pub fn bombear(&mut self) -> u32 {
        if self.gpu_caiu.is_some() {
            return 0;
        }
        if self.queda_simulada_em.is_some_and(|t| Instant::now() >= t) {
            self.queda_simulada_em = None;
            self.gpu_caiu = Some(windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_REMOVED);
            registro::linha(format!(
                "BANCADA: queda da GPU simulada (--simular-queda-da-gpu) — o dispositivo está de \
                 pé; {} apresentados até aqui",
                self.contadores.apresentados
            ));
            return 0;
        }
        let mut pronto_para_drenar = !self.decodificador.is_async;
        if let Some(rx) = &self.eventos {
            // 5 ms: no caminho assíncrono é aqui que a thread dorme em vez de girar. No síncrono
            // (o único medido nesta bancada) `eventos` é `None` e quem regula o giro é o chamador.
            match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(encoder::MftEvent::NeedInput) => self.creditos += 1,
                Ok(encoder::MftEvent::HaveOutput) => pronto_para_drenar = true,
                _ => {}
            }
        }
        if !pronto_para_drenar {
            return 0;
        }

        // A câmera colhe **antes** de tudo, e sem esperar: o quadro submetido na volta anterior
        // quase sempre já saiu da GPU, e colhê-lo aqui é o que mantém o anel vazio para o próximo.
        self.colher_camera();
        if self.gpu_caiu.is_some() {
            return 0;
        }

        // **Um quadro por vez, consumido antes de pedir o próximo.** Juntar um lote e olhar
        // depois é o defeito que a régua achou: o MFT recicla a superfície no `ProcessOutput`
        // seguinte, e as primeiras entradas do lote chegam ao chamador já sobrescritas pelas
        // últimas. Ver `decoder::DecodedFrame`.
        let mut apresentados = 0u32;
        loop {
            let pediu_em = Instant::now();
            let quadro = match decoder::proximo_quadro_com_mudanca(
                &self.decodificador.transform,
                decoder::OUTPUT_STREAM_ID,
            ) {
                Ok((q, mudou)) => {
                    // Só a volta que devolveu quadro entra na conta: a que devolve "nada pronto"
                    // é o fim normal da drenagem, e contá-la puxaria o centro para perto de zero.
                    if q.is_some() {
                        self.custos.saida.desde(pediu_em);
                    }
                    // **A geometria do fluxo mudou no meio da sessão.** A janela e o escalador da
                    // câmera foram montados para o tamanho anterior; seguir com eles é desenhar o
                    // plano UV no passo errado — a tela inteira verde com listras verticais que
                    // Bruno fotografou. O escalador é **remontado aqui**, na geometria nova; a
                    // janela passa a encaixar pelo tamanho novo.
                    //
                    // *(Até 10/09/2026 o comentário dizia que ele "renascia no `ligar_camera`
                    // seguinte" — e não havia chamada seguinte: `ligar_camera` roda uma vez, na
                    // abertura. Uma troca de geometria deixava a câmera virtual na placa de espera
                    // pelo resto da sessão. Nunca aconteceu em campo, e é por isso que ninguém viu.)*
                    //
                    // **Textura e imagem são coisas diferentes aqui**, e a primeira versão deste
                    // ramo as confundia: um 1080p do x264 abre o decoder e troca o tipo para
                    // 1920x1088 na primeira saída — a textura alinhada, com a mesma imagem de
                    // 1920x1080 dentro. A imagem só "mudou" se o recorte visível mudou.
                    if let Some((w, h)) = mudou {
                        let (codificada, visivel) = decoder::abertura_de_saida(
                            &self.decodificador.transform,
                        )
                        .unwrap_or((
                            (w, h),
                            windows::Win32::Foundation::RECT {
                                left: 0,
                                top: 0,
                                right: w as i32,
                                bottom: h as i32,
                            },
                        ));
                        let (vw, vh) = (
                            (visivel.right - visivel.left).max(0) as u32,
                            (visivel.bottom - visivel.top).max(0) as u32,
                        );
                        if vw > 0 && vh > 0 && (codificada, visivel) != (self.codificada, self.visivel)
                        {
                            registro::linha(format!(
                                "exibicao: textura do decoder {}x{} -> {}x{}, imagem {}x{} -> \
                                 {vw}x{vh}{}",
                                self.codificada.0,
                                self.codificada.1,
                                codificada.0,
                                codificada.1,
                                self.largura,
                                self.altura,
                                if (vw, vh) == (self.largura, self.altura) {
                                    " (só o alinhamento; a imagem é a mesma)"
                                } else {
                                    " — rearmando o encaixe e a câmera virtual"
                                }
                            ));
                            let imagem_mudou = (vw, vh) != (self.largura, self.altura);
                            self.largura = vw;
                            self.altura = vh;
                            self.codificada = codificada;
                            self.visivel = visivel;
                            // **A janela acompanha** (o G1 de 21/09): a origem de cada quadro sai da
                            // abertura de agora (abaixo). Até aqui só o destino acompanhava, e a
                            // origem do primeiro SPS passava da textura quando o fluxo descia de
                            // 854 para 640: o `VideoProcessorBlt` recusava a origem maior que a
                            // textura (o passo 11 do `anel`, 21/09) e a janela parava na última
                            // imagem de 854. O processador é refeito **só quando a imagem muda**
                            // (a revisão do `23eb480`, B1): o enumerador do tamanho antigo aceita a
                            // textura nova (o mesmo passo 11), e o alinhamento de toda sessão 1080p
                            // (1920x1088) não paga a recriação.
                            if imagem_mudou {
                                if let Err(erro) = self.apresentador.ajustar_entrada(codificada.0, codificada.1) {
                                    registro::linha(format!(
                                        "exibicao: não refiz o processador da janela para {}x{} — {erro}; a origem por quadro segue valendo",
                                        codificada.0, codificada.1
                                    ));
                                }
                            }
                            self.escalador = None;
                            if self.camera.is_some() {
                                if let Err(erro) = self.montar_escalador() {
                                    registro::linha(format!(
                                        "camera virtual: não remontei o escalador em {vw}x{vh} — {erro:#}"
                                    ));
                                }
                            }
                        }
                    }
                    match q {
                        Some(q) => q,
                        None => break,
                    }
                }
                Err(erro) => {
                    if !self.conferir_gpu("ProcessOutput", &erro.to_string()) {
                        registro::linha(format!("exibicao: ProcessOutput falhou: {erro}"));
                    }
                    break;
                }
            };
            self.contadores.decodificados += 1;
            self.descrever_saida(&quadro);

            // A régua **antes** de apresentar, e sobre a textura que saiu do decoder — é essa a
            // afirmação que ela sustenta: "os pixels que saíram do decodificador são os que
            // entraram no encoder". Depois do Video Processor já houve conversão de cor e escala.
            let valor_da_regua =
                if self.leitor.is_some() { self.ler_a_regua(&quadro) } else { None };

            // **A saída casa com a entrada aqui, e não depois de apresentar.**
            //
            // Este `pop_front` é a outra ponta do trilho: a marca da condenação entrou em
            // `submeter` e sai aqui, no quadro correspondente. Ele acontece **antes** do
            // `present_frame` por duas razões, e a primeira decide:
            //
            //  1. é a marca que diz se este quadro pode ser apresentado — perguntar depois de
            //     apresentar seria perguntar tarde;
            //  2. o MFT consumiu a entrada independentemente de a apresentação dar certo, então
            //     casar só no sucesso desalinharia a fila no primeiro `present_frame` que falhasse
            //     e inflaria `sem_correspondencia` daí para a frente.
            let submetido = self.submetidos.pop_front();
            if submetido.is_none() {
                self.contadores.sem_correspondencia += 1;
            }

            // **A porta.** O quadro condenado foi decodificado — parar de alimentar o decodificador
            // dessincroniza a sessão e faz o IDR seguinte chegar num decoder com buraco — e
            // **não** é apresentado: a janela segura o último quadro bom até a cadeia se curar, ou
            // até a válvula de 2 s desistir de esperar. Ver `crate::cadeia`.
            if submetido.as_ref().is_some_and(|s| s.suspeito) {
                self.contadores.retidos += 1;
                continue;
            }

            let destino = present::aspect_fit(
                self.largura,
                self.altura,
                self.saida_largura,
                self.saida_altura,
            );
            // **A origem, quadro a quadro** (o G1 de 21/09): a abertura visível de agora, recortada
            // à textura que chegou. Um `GetDesc` por quadro, e nenhuma origem passa da textura,
            // qualquer que seja a ordem em que o tipo e a textura mudarem.
            let textura = {
                let mut d = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                unsafe { quadro.texture.GetDesc(&mut d) };
                (d.Width, d.Height)
            };
            let v = self.visivel;
            let origem = crate::geometria_da_exibicao::origem_na_textura((v.left, v.top, v.right, v.bottom), textura)
                .map(|(left, top, right, bottom)| windows::Win32::Foundation::RECT { left, top, right, bottom });
            // `None` no último parâmetro **não é preguiça**: é o caminho de gravação de imagem da
            // sonda, e ele não existe aqui. Ver a nota no topo do módulo.
            let apresentou_em = Instant::now();
            let apresentado = match origem {
                Some(origem) => self.apresentador.present_frame(
                    &quadro.texture,
                    quadro.subresource_index,
                    origem,
                    destino,
                    None,
                ),
                None => Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_INVALIDARG,
                    format!(
                        "a abertura {:?} não tem área dentro da textura {}x{}", // i18n: fora (diário e detalhe técnico)
                        (v.left, v.top, v.right, v.bottom),
                        textura.0,
                        textura.1
                    ),
                )),
            };
            self.custos.tela.desde(apresentou_em);
            // A hora do `Present` para a claquete (S7), antes de a câmera virtual ocupar a thread.
            let apresentou_qpc = if self.claquete { crate::cano::qpc_us() } else { 0 };
            let apresentou = match apresentado {
                Ok(()) => true,
                Err(erro) => {
                    if self.conferir_gpu("present_frame", &erro.to_string()) {
                        break;
                    }
                    registro::linha(format!("exibicao: present_frame falhou: {erro}"));
                    false
                }
            };

            // **A câmera virtual recebe o mesmo quadro que a janela** — quando há quem leia, e no
            // ritmo dela, e só quando há baia: sem câmera ligada, este caminho não existe e nenhum
            // pixel sai do processo. **Apresentado ou não** (o G1 de 21/09): a janela que falha não
            // congela a câmera junto; o escalador dela já foi remontado na geometria nova.
            if self.gpu_caiu.is_none() {
                self.submeter_camera(&quadro);
            }
            if !apresentou {
                continue;
            }
            self.contadores.apresentados += 1;
            apresentados += 1;
            // A claquete (S7): a hora em que o `Present` voltou, no QPC (lida logo depois dele,
            // antes da câmera virtual), com o índice da régua e o carimbo. Com o limite, uma sessão
            // esquecida aberta não cresce sem fim.
            if self.claquete && self.imagens_da_claquete.len() < 100_000 {
                if let (Some(v), Some(s)) = (valor_da_regua, submetido.as_ref()) {
                    self.imagens_da_claquete.push((v, apresentou_qpc, s.timestamp_us));
                }
            }
            // **O instante em que o quadro foi para o vidro.** É este ponto do caminho, e não a
            // chegada nem a decodificação, que corresponde ao que o olho recebe — e é por isso que
            // a porta logo acima aparece nesta distribuição como intervalo maior, que é o que ela
            // custa. Ver `crate::fluidez`.
            self.fluidez.apresentou(Instant::now());

            if let Some(s) = submetido {
                let us = s.chegou_em.elapsed().as_micros() as u64;
                self.contadores.soma_latencia_us += us;
                self.contadores.pior_latencia_us = self.contadores.pior_latencia_us.max(us);
            }
            if self.primeira_imagem.is_none() {
                self.primeira_imagem = Some(Instant::now());
            }
        }
        apresentados
    }

    /// Liga a câmera virtual desta sessão: daqui em diante os quadros apresentados também vão
    /// para o cano da baia — **quando alguém estiver lendo, e no ritmo da câmera**.
    ///
    /// O `Escalador` nasce **aqui**, e não na abertura, porque ele precisa da geometria do vídeo
    /// que de fato chegou — que só se conhece depois do primeiro SPS — e do **mesmo** dispositivo
    /// D3D11 da apresentação.
    ///
    /// `em_todo_quadro` é o braço de bancada `--camera-em-todo-quadro`: o comportamento de 09/09,
    /// para medir contra o de agora no mesmo binário.
    pub fn ligar_camera(
        &mut self,
        baia: std::sync::Arc<crate::baia::Baia>,
        em_todo_quadro: bool,
    ) -> anyhow::Result<()> {
        self.montar_escalador()?;
        self.buffer_nv12 = vec![0u8; crate::cano::BYTES_NV12];
        self.camera_em_todo_quadro = em_todo_quadro;
        registro::linha(format!(
            "camera virtual \"{}\": ligada ao vídeo de {}x{} ({})",
            baia.nome,
            self.largura,
            self.altura,
            if em_todo_quadro {
                "braço de bancada: todo quadro, esperando a GPU"
            } else {
                "todo quadro com leitor, sem esperar a GPU"
            }
        ));
        self.camera = Some(baia);
        Ok(())
    }

    fn montar_escalador(&mut self) -> anyhow::Result<()> {
        self.escalador = Some(crate::escala_nv12::Escalador::novo(
            self.apresentador.device(),
            self.codificada,
            self.visivel,
            crate::cano::FPS,
        )?);
        Ok(())
    }

    /// Colhe, **sem esperar a GPU**, o que a câmera submeteu nas voltas anteriores.
    fn colher_camera(&mut self) {
        if self.camera_em_todo_quadro {
            return; // o braço de 09/09 colhe esperando, no próprio passo de submeter
        }
        let (Some(baia), Some(escalador)) = (self.camera.as_deref(), self.escalador.as_ref())
        else {
            return;
        };
        let mut falha = None;
        loop {
            let comecou = Instant::now();
            match escalador.colher(&mut self.buffer_nv12) {
                Ok(crate::escala_nv12::Colheita::Pronta) => {
                    self.custos.colher.desde(comecou);
                    publicar_na_baia(baia, &mut self.buffer_nv12, &mut self.reserva_nv12);
                    self.publicados_na_camera += 1;
                }
                Ok(crate::escala_nv12::Colheita::AindaNaGpu) => {
                    self.camera_ainda_na_gpu += 1;
                    break;
                }
                Ok(crate::escala_nv12::Colheita::Nada) => break,
                Err(erro) => {
                    falha = Some(format!("{erro:#}"));
                    break;
                }
            }
        }
        if let Some(erro) = falha {
            if !self.conferir_gpu("a colheita da câmera virtual", &erro) { // i18n: fora (diário e detalhe técnico)
                registro::linha(format!("camera virtual: colheita falhou: {erro}"));
            }
        }
    }

    /// Submete o quadro que acabou de ir para a janela à conversão da câmera — se alguém lê a
    /// câmera, se é a hora dela, e se o anel tem vaga. Nenhum dos três casos espera a GPU.
    fn submeter_camera(&mut self, quadro: &decoder::DecodedFrame) {
        let (Some(baia), Some(escalador)) = (self.camera.as_deref(), self.escalador.as_ref())
        else {
            return;
        };

        let mut falha: Option<(&str, String)> = None;
        if self.camera_em_todo_quadro {
            // O braço de 09/09, fiel: colher o de trás **esperando**, submeter o da vez, em todo
            // quadro apresentado e com ou sem leitor.
            let comecou = Instant::now();
            if escalador.em_voo() > 0 {
                match escalador.colher_esperando(&mut self.buffer_nv12) {
                    Ok(true) => {
                        self.custos.colher.desde(comecou);
                        publicar_na_baia(baia, &mut self.buffer_nv12, &mut self.reserva_nv12);
                        self.publicados_na_camera += 1;
                    }
                    Ok(false) => {}
                    Err(erro) => falha = Some(("colheita", format!("{erro:#}"))),
                }
            }
            let submeteu_em = Instant::now();
            if falha.is_none() {
                if let Err(erro) = escalador.submeter(&quadro.texture, quadro.subresource_index) {
                    falha = Some(("escala", format!("{erro:#}")));
                }
            }
            self.custos.submeter.desde(submeteu_em);
            self.custos.escala.desde(comecou);
            self.notar_falha_da_camera(falha);
            return;
        }

        if !baia.alguem_lendo() {
            self.camera_sem_leitor += 1;
            return;
        }
        // **Todo quadro apresentado vai para a câmera**, e não 30 por segundo.
        //
        // Por algumas horas de 10/09/2026 este ponto dizimava para os 30 fps que a fonte anuncia,
        // pela hora de chegada. O usuário viu na hora, olhando a câmera virtual com o S24 a 60:
        // *"exibindo menos quadros, a imagem tem pequenos trancos"* — o tremor do Wi-Fi escolhia
        // quadros a 16, 33 e 50 ms um do outro. A dizimação nasceu da premissa de que a conversão
        // era o gargalo do laço, e o mesmo dia a desmentiu: com o decode em DXVA ela custa ~1 ms
        // por quadro (`escala_us` p50 857 µs no braço antigo). O que fica do conserto é o que não
        // custa imagem: a colheita sem esperar a GPU e o buffer reaproveitado.
        let agora = Instant::now();
        match escalador.submeter(&quadro.texture, quadro.subresource_index) {
            Ok(true) => {}
            Ok(false) => self.camera_anel_cheio += 1,
            Err(erro) => falha = Some(("escala", format!("{erro:#}"))),
        }
        self.custos.submeter.desde(agora);
        self.notar_falha_da_camera(falha);
    }

    /// A falha da câmera virtual vai para o registro como sempre foi — a menos que seja a GPU que
    /// caiu, e aí quem registra é [`Exibicao::conferir_gpu`], uma vez.
    fn notar_falha_da_camera(&mut self, falha: Option<(&str, String)>) {
        let Some((etapa, erro)) = falha else { return };
        if !self.conferir_gpu(&format!("a {etapa} da câmera virtual"), &erro) { // i18n: fora (diário e detalhe técnico)
            registro::linha(format!("camera virtual: {etapa} falhou: {erro}"));
        }
    }

    /// Registra, uma vez, como é a textura que o decoder entrega; e conta os subrecursos.
    fn descrever_saida(&mut self, quadro: &decoder::DecodedFrame) {
        if quadro.subresource_index < 64 {
            self.subrecursos_vistos |= 1u64 << quadro.subresource_index;
        }
        if self.textura_descrita {
            return;
        }
        self.textura_descrita = true;
        let mut d = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
        unsafe { quadro.texture.GetDesc(&mut d) };
        let decoder = d.BindFlags
            & windows::Win32::Graphics::Direct3D11::D3D11_BIND_DECODER.0 as u32
            != 0;
        registro::linha(format!(
            "decoder: textura de saída {}x{} formato={} array={} bind=0x{:X} misc=0x{:X} \
             uso={} -> BIND_DECODER={}",
            d.Width,
            d.Height,
            d.Format.0,
            d.ArraySize,
            d.BindFlags,
            d.MiscFlags,
            d.Usage.0,
            if decoder { "sim (superfície de DXVA)" } else { "NÃO" }
        ));
    }

    /// Quantos índices de subrecurso distintos a saída do decoder usou (dos primeiros 64).
    pub fn subrecursos_distintos(&self) -> u32 {
        self.subrecursos_vistos.count_ones()
    }

    /// A linha de fim de sessão da câmera virtual. `None` quando esta sessão não tinha câmera.
    pub fn linha_da_camera(&self) -> Option<String> {
        let baia = self.camera.as_ref()?;
        let mut linha = format!(
            "camera virtual \"{}\": modo={} publicados={} sem_leitor={} anel_cheio={} \
             ainda_na_gpu={}",
            baia.nome,
            if self.camera_em_todo_quadro { "todo-quadro" } else { "por-leitor" },
            self.publicados_na_camera,
            self.camera_sem_leitor,
            self.camera_anel_cheio,
            self.camera_ainda_na_gpu,
        );
        if !self.custos.escala.vazia() {
            linha.push(' ');
            linha.push_str(&self.custos.escala.linha());
        }
        Some(linha)
    }

    /// A ordem aqui não é estilo: `leitor` é um `&mut` de um campo de `self`, e `self.regua` e
    /// `self.leitor` são outros dois. Todo o uso do `leitor` acontece **antes** de tocar nos
    /// outros dois, para o empréstimo terminar — escrito ao contrário, isto não compila.
    /// Devolve o valor lido (para a claquete), além de contá-lo.
    fn ler_a_regua(&mut self, quadro: &decoder::DecodedFrame) -> Option<u32> {
        let (largura, altura) = (self.largura, self.altura);
        let Some(leitor) = self.leitor.as_mut() else { return None };
        let ja_no_piso = leitor.no_caminho_largo();
        let resultado = leitor.ler(&quadro.texture, quadro.subresource_index);
        let segue_viva = match &resultado {
            Ok(_) => true,
            // Já no piso e ainda falhando: não há terceiro caminho a tentar.
            Err(_) if ja_no_piso => false,
            Err(_) => leitor.cair_para_o_caminho_largo(largura, altura).is_ok(),
        };
        // O empréstimo de `leitor` acaba aqui.
        match resultado {
            Ok(valor) => {
                self.regua.registrar(valor, true);
                valor
            }
            Err(erro) => {
                if !segue_viva {
                    registro::linha(format!("regua: desligada — a cópia da GPU falhou: {erro}"));
                    self.leitor = None;
                }
                None
            }
        }
    }

    /// As imagens da claquete desde a última chamada: `(índice da régua, QPC do Present em µs,
    /// carimbo do quadro)`. **Só o relato de 1 Hz chama.**
    pub fn tirar_claquete(&mut self) -> Vec<(u32, u64, u64)> {
        std::mem::take(&mut self.imagens_da_claquete)
    }

    /// Drena o que ficou dentro do MFT e fecha o fluxo. Não apresenta nada: a janela já vai sumir.
    pub fn fechar(&mut self) {
        match decoder::end_stream_and_drain(&self.decodificador.transform, self.eventos.as_ref()) {
            Ok(q) => {
                if !q.is_empty() {
                    registro::linha(format!("exibicao: {} quadro(s) na drenagem final", q.len()));
                }
            }
            Err(erro) => registro::linha(format!("exibicao: drenagem final falhou: {erro}")),
        }
    }
}

/// **A janela de vídeo tem de sumir quando a sessão acaba.**
///
/// `present::Window` não destrói a janela sozinha — na sonda isso nunca importou, porque o
/// processo termina logo depois. Aqui importa: o app continua vivo e volta à tela inicial, e uma
/// janela de vídeo abandonada mostrando o último quadro do outro aparelho ficaria na tela sem nada
/// por trás dela. É a irmã da decisão de `WM_CLOSE` na janela principal — não deixar coisa viva
/// sem quem a mostre.
impl Drop for Exibicao {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.janela.hwnd);
        }
    }
}
