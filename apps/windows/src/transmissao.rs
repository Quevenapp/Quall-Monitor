//! Captura → encode → **rede**. Sem passar por disco, e sem ter para onde passar.
//!
//! # Por que isto não foi fundido com `main.rs`
//!
//! `main.rs` é a sonda do M1: captura → encode → **arquivo**, com sidecar por quadro para medir
//! com `ffprobe` por fora. Ela continua intacta e continua sendo o instrumento de bancada.
//!
//! Esta cadeia é o caminho de produto, e ela **não tem caminho para arquivo nem opção para criar
//! um**. A decisão é a mesma que o irmão do macOS tomou em `TransmissaoAoVivo`, pelo mesmo motivo,
//! e o motivo é uma regra desta casa: *"um vídeo de bancada pode conter a vida do usuário"*. Uma
//! frente já gravou um `.h264` do Dell e abriu um quadro dele com o WhatsApp Web do usuário à
//! mostra. A origem desta cadeia é a tela de trabalho de uma pessoa; a forma mais barata de nunca
//! vazar isso é não ter para onde gravar. Foi por isso que `prova-rede.ps1` perdeu o `--salvar`, e
//! é por isso que aqui não existe um.
//!
//! O preço são ~40 pontos de código parecidos com os de `main.rs`. As duas querem coisas
//! diferentes do mesmo encoder — uma quer o sidecar completo por quadro, a outra quer nunca
//! segurar um buffer — e fundi-las custaria mais do que a repetição.
//!
//! # O que esta cadeia conserta em relação à sonda
//!
//! 1. **Todo IDR sai com SPS e PPS.** O MFT do Windows entrega o conjunto de parâmetros uma vez,
//!    no tipo de saída, e não o repete em cada IDR — é o defeito que `quall-core` já nomeia num
//!    teste (`idrs_sem_parametros`). Numa sonda que grava tudo num arquivo desde o quadro zero,
//!    isso não dói. Neste produto o receptor **sempre** entra depois do começo.
//! 2. **O espaçamento real dos IDR é medido**, não pedido. Ver `medida_de_idr`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossbeam_channel::{select, Receiver};
use windows::core::Result;

use quall_core::protocol::EncodePreset;
use quall_core::track::QuadroCodificado;

use crate::capture::{Captura, CapturedFrame, ScreenCapture};
use crate::device;
use crate::encoder::{self, ChosenEncoder, EncoderConfig, FormatoDeEntrada, MftEvent, Preferencia};
use crate::escala;
use crate::fontes::Fonte;
use crate::oficina::{Oficina, Reserva};
use crate::registro;
use crate::sintetica::{Carga, OrigemSintetica, Ritmo};
use crate::sps::{self, ResumoSps};

/// De onde a cadeia tira os quadros.
pub enum OrigemDaCadeia<'a> {
    /// Um monitor de verdade, pelo WGC — o caminho de produto.
    Monitor {
        fonte: &'a Fonte,
        hmonitor: windows::Win32::Graphics::Gdi::HMONITOR,
    },
    /// Uma textura nossa, no tamanho pedido (`sintetica.rs`).
    Sintetica {
        largura: u32,
        altura: u32,
        fps: u32,
        carga: Carga,
        ritmo: Ritmo,
    },
    /// **Uma câmera do PC** (`captura_de_camera.rs`, fase 3 de `docs/camera-no-windows.md`). A placa
    /// é a do encoder que ativa, pelo LUID; o encoder recebe NV12.
    ///
    /// `parar` é o Parar da sessão: a abertura da câmera o olha na espera do primeiro quadro (a
    /// reconferência da fase 3). `bater` é o sinal de vida da sessão, dado a cada olhada no Parar
    /// durante a abertura (o cão de guarda do coordenador, M54); `None` sem coordenador.
    Camera {
        fonte: &'a crate::captura_de_camera::FonteDaCamera,
        parar: Option<&'a std::sync::atomic::AtomicBool>,
        bater: Option<&'a dyn Fn()>,
    },
    /// **A câmera do dono da captura do R5** (`dono_da_captura.rs`, `docs/teleprompter-com-camera.md`
    /// §8.10): a câmera já está aberta e é do dono. A cadeia ativa o encoder **na placa do dono** e
    /// herda o dispositivo e o gerenciador dele; passa **sempre** pelo conversor (a posição do anel da
    /// rede só vive até o `Blt` dele, a revisão do plano, B1); e parar solta a rede sem fechar a
    /// câmera.
    DoDono(crate::dono_da_captura::LeitorDoDono),
}

/// **Os padrões da tela estendida do Mac**, portados (`docs/handover-tela-estendida.md` §4). Só o
/// emissor com vários receptores os liga; `None` é o caminho de sempre, byte a byte.
#[derive(Clone, Copy, Debug)]
pub struct PadroesDaTelaEstendida {
    /// **Tela parada não gera quadro** — o WGC só entrega quando algo muda, e a origem sintética
    /// parada também não —, e um pedido de IDR só é atendido no próximo quadro que entra no
    /// encoder. Depois disto sem quadro novo, o último entra de novo; com IDR pedido, na hora.
    pub repetir_apos: Duration,
    /// O GOP **pedido**. No Mac é o IDR programado a cada 30 s. Aqui é só o pedido ao MFT, e **não**
    /// um IDR forçado a cada 30 s: no Windows todo IDR forçado é uma recriação de encoder (a quinta
    /// porta), que reinicia o controle de taxa e deixa +3 handles (`docs/troca-a-quente.md`) —
    /// revisão adversarial de 13/09/2026. O que o encoder de fato faz sai em `medida_de_idr`.
    pub gop_segundos: u32,
    /// O teto de quadro, em quadros médios da sessão (taxa ÷ 8 ÷ fps). `0` desliga.
    pub teto_de_quadro_em_medios: f64,
}

impl PadroesDaTelaEstendida {
    /// Os números do Mac: 500 ms, 30 s, 5 quadros médios.
    pub const DO_MAC: PadroesDaTelaEstendida = PadroesDaTelaEstendida {
        repetir_apos: Duration::from_millis(500),
        gop_segundos: 30,
        teto_de_quadro_em_medios: 5.0,
    };
}

/// Tudo o que `Cadeia::abrir` recebia em argumentos soltos, mais o que o emissor com vários
/// receptores acrescenta.
pub struct OpcoesDaCadeia {
    pub fps: u32,
    /// O zero da linha do tempo — ver `Cadeia::abrir`.
    pub origem_do_relogio: Instant,
    pub idr_por_flush: bool,
    pub idr_por_recriacao: bool,
    pub taxa_de_entrega: Option<u32>,
    pub piso_entre_recriacoes_ms: u64,
    pub bitrate_alvo: Option<u32>,
    pub troca_a_quente: bool,
    pub caixa_unica: bool,
    /// A escolha de MFT. `Produto` no caminho de sempre; `Intel` na bancada, para medir o Quick
    /// Sync mesmo onde a NVIDIA ativaria (a Sessão 0).
    pub preferencia: Preferencia,
    pub padroes: Option<PadroesDaTelaEstendida>,
}

/// Um quadro pronto para a track.
pub struct Quadro {
    pub bytes: Vec<u8>,
    pub idr: bool,
    /// Relógio monotônico desta cadeia, em microssegundos, tirado no instante da **captura** — não
    /// no da saída do encoder. É o que faz `timestamp_us` significar "quando isto estava na tela".
    pub timestamp_us: u64,
    pub latencia_us: u64,
    /// Quantos pacotes RTP a libdatachannel vai numerar para esta unidade de acesso, pela regra
    /// **exata** do pacotizador. Calculado uma vez, onde o buffer ainda está na mão, e carregado
    /// aqui para que `bombear` possa somar a **rajada de saída** sem recontar.
    pub pacotes: u64,
}

/// Um retrato dos quadros que saem numa dada posição depois de o encoder nascer.
///
/// Existe para responder a candidata que sobrou do laudo de 29/08 — **o controle de taxa
/// reiniciado** — e ela tem dois lados que são o mesmo fenômeno: a rampa que recomeça pode
/// **derrubar a média** (o emissor entrega 1,2 Mbps onde entregaria 3,7) e pode **produzir picos**
/// nos primeiros quadros. Um perfil por posição olha os dois de uma vez: se o tamanho **sobe** com
/// a posição, a rampa está subindo e a recriação a corta pela metade; se ele é **grande no começo**
/// e cai, os primeiros quadros são o pico.
#[derive(Default, Clone, Copy)]
pub struct PosicaoPosRecriacao {
    pub n: u64,
    pub soma_bytes: u64,
    pub max_bytes: u64,
    pub soma_pacotes: u64,
    pub max_pacotes: u64,
}

impl PosicaoPosRecriacao {
    fn registrar(&mut self, bytes: u64, pacotes: u64) {
        self.n += 1;
        self.soma_bytes += bytes;
        self.max_bytes = self.max_bytes.max(bytes);
        self.soma_pacotes += pacotes;
        self.max_pacotes = self.max_pacotes.max(pacotes);
    }
    fn media_bytes(&self) -> u64 {
        if self.n == 0 { 0 } else { self.soma_bytes / self.n }
    }
    fn media_pacotes_centesimos(&self) -> u64 {
        if self.n == 0 { 0 } else { self.soma_pacotes * 100 / self.n }
    }
}

/// Quantas posições depois da recriação são perfiladas antes de tudo cair no balde do resto.
///
/// Dezesseis porque a 28–31 fps e ~1,2 s entre recriações um encoder vive ~36 quadros: dezesseis
/// cobrem a primeira metade da vida dele, que é onde uma rampa que recomeça teria de aparecer.
const POSICOES_PERFILADAS: usize = 16;

/// As faixas do **penhasco da LAN**, medido em 30/08 por `tools/rajada-udp.py` sem nada nosso no
/// caminho: rajadas de 10, 20 e 40 pacotes colados perdem todas entre 0,24 % e 0,54 %, e a de 80
/// perde 30,82 %. O penhasco está **entre 40 e 80**, e é por isso que a faixa que interessa tem
/// fronteira em 40: um quadro que a cruze é um candidato a mecanismo; um que não a cruze não é.
const FRONTEIRAS_DE_RAJADA: [u64; 4] = [10, 20, 40, 80];

fn faixa_de_rajada(pacotes: u64) -> usize {
    FRONTEIRAS_DE_RAJADA
        .iter()
        .position(|&f| pacotes < f)
        .unwrap_or(FRONTEIRAS_DE_RAJADA.len())
}

fn faixas_em_texto(faixas: &[u64; 5]) -> String {
    format!(
        "<10:{} 10-19:{} 20-39:{} 40-79:{} >=80:{}",
        faixas[0], faixas[1], faixas[2], faixas[3], faixas[4]
    )
}

#[derive(Default, Clone, Copy)]
pub struct Contadores {
    /// Carimbos que a submissão empurrou para depois do anterior: o `t` do MFT e o `timestamp_us`
    /// do fio nunca voltam (a revisão do código da fase 3, M1). Zero é o esperado.
    pub carimbos_empurrados: u64,
    /// Voltas em que a câmera esperou um destino livre do conversor, com `destinos − 1` quadros no
    /// MFT (a revisão do código da fase 3, m6).
    pub conversor_sem_destino: u64,
    pub capturados: u64,
    pub encodados: u64,
    pub idrs: u64,
    /// Quantos IDR precisaram receber SPS/PPS por nossa conta. Se este número for igual ao de
    /// IDRs, o MFT nunca os repete sozinho — que é o defeito documentado.
    pub parametros_injetados: u64,
    /// Saídas do encoder que eram só conjunto de parâmetros, sem fatia de imagem. Não são quadro e
    /// não vão para a track.
    pub saidas_so_de_parametros: u64,
    /// A **soma** das três recusas do caminho de submissão, abaixo. Continua existindo com este
    /// nome porque `quall_quinta_porta.rs` o imprime desde o M1; o que ele nunca disse é de qual
    /// das três se tratava — e uma delas (`falhas_de_empacotamento`) nem sequer o incrementava.
    pub recusados_pelo_encoder: u64,
    /// O `Blt` do teto falhou. O quadro é perdido; nunca segue no tamanho errado.
    pub falhas_de_escala: u64,
    /// `sample_from_texture` falhou. **Este caminho não tinha contador nenhum até 31/08**: o
    /// quadro sumia com uma linha de registro e nada no relatório.
    pub falhas_de_empacotamento: u64,
    /// `ProcessInput` recusou. É aqui, e só aqui, que um `MF_E_NOTACCEPTING` apareceria.
    pub recusas_do_process_input: u64,
    /// Quadros entregues ao MFT com `ProcessInput` devolvendo `Ok`.
    ///
    /// Separado de `encodados` de propósito: `encodados` conta **saídas** do MFT, e a distância
    /// entre os dois é o que está em voo mais o que uma troca de encoder jogou fora. Sem ele a
    /// conta de `capturados` não fecha, e uma conta que não fecha não é medida.
    pub entregues_ao_mft: u64,
    /// **O degrau onde os quadros somem.**
    ///
    /// Um quadro que o laço tirou da caixa postal da captura fica em `pendente` até haver crédito
    /// do MFT para submetê-lo. Se a captura entregar outro antes disso, o anterior é
    /// **sobrescrito** — some sem erro, sem log e, até 31/08, sem contador. A corrida de 31/08
    /// (`capturados=1349 encodados=749`) tinha 600 quadros exatamente aqui e nada no relatório
    /// dizia onde.
    ///
    /// Sobrescrever é a política certa para espelhamento ao vivo — quem atravessa tem de ser o
    /// quadro mais novo. O defeito nunca foi descartar: foi **descartar calado**.
    pub sobrescritos_antes_de_submeter: u64,
    /// `METransformNeedInput` recebidos: quantas vezes o MFT pediu entrada.
    ///
    /// É a testemunha que separa as duas explicações de "encodados < capturados": se este número
    /// for igual a `entregues_ao_mft`, **todo crédito foi gasto** e quem limita a taxa é o
    /// encoder; se for muito maior, sobrou crédito e quem limita é o laço.
    pub creditos_recebidos: u64,
    /// Voltas em que havia quadro pendente e o MFT não tinha pedido entrada.
    pub voltas_sem_credito: u64,
    /// Voltas em que havia crédito sobrando e nenhum quadro pendente para gastá-lo.
    pub voltas_com_credito_sem_quadro: u64,
    /// Créditos que uma troca/recriação/`FLUSH` de encoder invalidou antes de serem gastos.
    pub creditos_perdidos_na_troca: u64,
    /// Quadros já dentro do MFT que uma troca/recriação/`FLUSH` fez nunca saírem.
    pub em_voo_perdidos_na_troca: u64,
    /// Quadro pendente jogado fora pelo `FLUSH` da terceira porta (`--idr-por-flush`).
    pub pendentes_perdidos_no_flush: u64,
    pub soma_latencia_us: u64,
    /// Voltas do laço em que a porta de taxa de entrega segurou o quadro pendente.
    ///
    /// **Não é contagem de quadros descartados**, e o primeiro nome que este contador teve
    /// (`descartados_pelo_ritmo`) dizia que era — trocado antes da primeira medição que o
    /// citasse, porque um contador com nome que mente é o formato de defeito que esta casa já
    /// pagou três vezes. O mesmo quadro é contado em todas as voltas em que continua pendente e a
    /// porta continua fechada, então este número é maior que o de quadros que deixaram de
    /// atravessar. Serve para uma coisa só, e é a que importa aqui: **sem `--taxa-de-entrega` ele
    /// é zero**, e é assim que quem lê o relatório confere de fora que o braço sem porta não teve
    /// porta. Quantos quadros atravessaram está em `encodados`.
    pub recusas_do_ritmo: u64,
    /// Quadros que entraram no encoder **repetidos** — a tela parada da tela estendida
    /// (`PadroesDaTelaEstendida::repetir_apos`) e, desde 22/09, a câmera parada
    /// (`Cadeia::talvez_repetir_a_camera`). Zero no caminho de sempre com a origem andando.
    pub repetidos: u64,
    /// A câmera parada pedia repetição e a posição do anel não estava intacta
    /// (`regras_da_camera::PosicaoRepetivel`): a repetição foi pulada.
    pub repeticoes_puladas: u64,
    /// Repetições postas em `pendente` (a tela estendida e a câmera parada): na conta do
    /// fechamento, são **origem** ao lado de `capturados` — sem isto a conta acusava "sobra −N"
    /// para N repetições (a revisão do código de 22/09, 1).
    pub repeticoes_postas: u64,
    /// Repetições em `pendente` que um quadro real sobrescreveu antes de entrar: destino na conta.
    pub repeticoes_sobrescritas: u64,
}

impl Contadores {
    pub fn latencia_media_ms(&self) -> f64 {
        if self.encodados == 0 {
            0.0
        } else {
            self.soma_latencia_us as f64 / self.encodados as f64 / 1000.0
        }
    }

    /// Onde cada quadro capturado foi parar, e a conta que **tem** de fechar.
    ///
    /// # Por que a conta é a aferição, e não um enfeite do relatório
    ///
    /// A regra desta bancada: instrumento não aferido contra caso conhecido não é instrumento.
    /// Um contador novo de descarte não tem como ser conferido de fora — ninguém sabe de
    /// antemão quantos quadros o encoder ia recusar. Mas há **uma** coisa sabida de antemão, em
    /// toda corrida, sem exceção: todo quadro que saiu da caixa postal da captura foi para
    /// exatamente um lugar. Ou foi codificado, ou está em voo, ou foi sobrescrito, ou uma das
    /// três recusas o pegou, ou ainda está pendente.
    ///
    /// Se a soma dessas parcelas não der `capturados`, o conjunto de contadores está errado — e
    /// isso aparece em **toda** corrida, não só na que alguém pensou em conferir. É o teste que
    /// acusa nas duas direções ao mesmo tempo: um caminho de descarte sem contador deixa sobra,
    /// e um contador que dispara onde não devia deixa falta.
    ///
    /// `pendente_agora` é estado vivo da `Cadeia`, por isso vem de fora. O que está **dentro** do
    /// MFT não entra aqui: ele já foi contado em `entregues_ao_mft`, e é a segunda conta
    /// ([`Contadores::diferenca_do_mft`]) que o cobra.
    pub fn diferenca_do_fechamento(&self, pendente_agora: u64) -> i64 {
        let destinos = self.entregues_ao_mft
            + self.sobrescritos_antes_de_submeter
            + self.falhas_de_escala
            + self.falhas_de_empacotamento
            + self.recusas_do_process_input
            + self.pendentes_perdidos_no_flush
            + self.repeticoes_sobrescritas
            + pendente_agora;
        (self.capturados + self.repeticoes_postas) as i64 - destinos as i64
    }

    /// A segunda conta: o que entrou no MFT contra o que saiu dele.
    pub fn diferenca_do_mft(&self, em_voo_agora: u64) -> i64 {
        self.entregues_ao_mft as i64
            - (self.encodados + self.em_voo_perdidos_na_troca + em_voo_agora) as i64
    }

    /// A linha que responde "onde morrem os quadros", em ordem de degrau.
    pub fn linha_dos_degraus(&self, pendente_agora: u64, em_voo_agora: u64) -> String {
        let dif = self.diferenca_do_fechamento(pendente_agora);
        let dif_mft = self.diferenca_do_mft(em_voo_agora);
        let pct = |n: u64| {
            if self.capturados == 0 {
                0.0
            } else {
                100.0 * n as f64 / self.capturados as f64
            }
        };
        format!(
            "degraus: capturados={}{} -> sobrescritos_antes_de_submeter={} ({:.1} %) \
             falhas_de_escala={} falhas_de_empacotamento={} recusas_do_process_input={} \
             pendentes_perdidos_no_flush={} pendente_agora={pendente_agora} \
             -> entregues_ao_mft={} -> em_voo_perdidos_na_troca={} em_voo_agora={em_voo_agora} \
             -> encodados={} ({:.1} % dos capturados) | \
             creditos_recebidos={} voltas_sem_credito={} voltas_com_credito_sem_quadro={} \
             creditos_perdidos_na_troca={} | contas: {} {}",
            self.capturados,
            // Só com repetição: a linha de sempre continua byte a byte.
            if self.repeticoes_postas > 0 {
                format!(" (+ repeticoes_postas={} repeticoes_sobrescritas={})", self.repeticoes_postas, self.repeticoes_sobrescritas)
            } else {
                String::new()
            },
            self.sobrescritos_antes_de_submeter,
            pct(self.sobrescritos_antes_de_submeter),
            self.falhas_de_escala,
            self.falhas_de_empacotamento,
            self.recusas_do_process_input,
            self.pendentes_perdidos_no_flush,
            self.entregues_ao_mft,
            self.em_voo_perdidos_na_troca,
            self.encodados,
            pct(self.encodados),
            self.creditos_recebidos,
            self.voltas_sem_credito,
            self.voltas_com_credito_sem_quadro,
            self.creditos_perdidos_na_troca,
            if dif == 0 { "captura FECHA".to_string() } else { format!("captura NÃO FECHA (sobra {dif})") },
            if dif_mft == 0 { "mft FECHA".to_string() } else { format!("mft NÃO FECHA (sobra {dif_mft})") },
        )
    }
}

/// Onde o tempo de uma volta do laço vai.
///
/// Existe porque a bancada mediu **11,7 fps** com `--fps 30` pedido e não havia como dizer de
/// quem era a culpa: da captura, do escalador que o teto introduziu, do `ProcessInput`, ou da
/// espera pela saída. "Meça onde o tempo vai antes de mexer" — e um número agregado de
/// `captura+encode` não separa nada disso.
///
/// Tudo em microssegundos somados, com o contador de amostras ao lado: a média é `soma/n` e não
/// depende de guardar histórico nenhum.
#[derive(Default, Clone, Copy)]
pub struct Perfil {
    /// Intervalo entre dois quadros **entregues pela captura**. Se isto for ~85 ms, a captura é o
    /// teto e o resto do laço é inocente.
    pub intervalo_de_captura_us: u64,
    pub n_intervalos: u64,
    pub intervalo_maximo_us: u64,
    /// Voltas de `bombear` em que a captura não tinha quadro novo para dar.
    pub voltas: u64,
    pub voltas_sem_quadro: u64,
    /// Qual ramo do `select!` ganhou a volta. Os três somados são `voltas`.
    ///
    /// Isto responde a pergunta que o número agregado de fps não responde: o `select!` do
    /// `crossbeam` escolhe **ao acaso** entre as operações prontas, e há duas — o quadro da
    /// captura e o evento do MFT. Se o MFT estiver sempre pronto, metade das voltas não olha para
    /// a captura, e o laço consome quadro na metade do ritmo em que a captura entrega, sem que
    /// nada no caminho pareça lento.
    pub ramo_captura: u64,
    pub ramo_evento: u64,
    pub ramo_ocioso: u64,
    /// Quadros que o WGC **entregou** (contados na callback) contra os que o laço consumiu. A
    /// diferença é quadro sobrescrito na caixa postal — ou seja, captura que existiu e o laço não
    /// pegou.
    pub chegados_do_wgc: u64,
    /// `Escalador::escalar` — o `Blt` que o teto introduziu.
    pub escala_us: u64,
    pub n_escala: u64,
    /// `ProcessInput` — entregar a amostra ao MFT.
    pub entrada_us: u64,
    pub n_entrada: u64,
    /// A espera síncrona pela saída do encoder, depois de submeter.
    pub espera_us: u64,
    pub n_espera: u64,
    /// Esperas que estouraram o prazo sem o quadro sair — cada uma é uma volta gasta sem produzir.
    pub esperas_estouradas: u64,
    /// `drain_output` + montagem do quadro de saída.
    pub drenagem_us: u64,
    pub n_drenagem: u64,
    /// **Quanto tempo o MFT leva, depois de aceitar um quadro, para pedir o próximo.**
    ///
    /// Medido do retorno do `ProcessInput` até o `METransformNeedInput` seguinte chegar ao canal.
    /// É a testemunha que decide de quem é o ritmo: se este número for da ordem de um intervalo
    /// de quadro, quem paga o tempo é o encoder e nenhum conserto no laço muda a taxa; se for
    /// ~0, o crédito estava lá e o laço não o gastou.
    pub ate_o_credito_seguinte_us: u64,
    pub n_ate_o_credito_seguinte: u64,
    pub ate_o_credito_seguinte_maximo_us: u64,
    /// Maior número de créditos acumulados ao mesmo tempo. Diz a profundidade de fila que o MFT
    /// oferece: 1 é um encoder que só aceita um quadro por vez, e nesse caso a taxa máxima do
    /// pipeline é o inverso do tempo de ida e volta dele.
    pub maximo_de_creditos: u64,
}

impl Perfil {
    fn media_ms(soma: u64, n: u64) -> f64 {
        if n == 0 { 0.0 } else { soma as f64 / n as f64 / 1000.0 }
    }

    pub fn linha(&self) -> String {
        format!(
            "voltas={} (captura={} evento={} ocioso={}) sem_quadro={} | chegados_do_wgc={} | \
             intervalo_de_captura media={:.1} ms max={:.1} ms n={} | \
             escala={:.2} ms n={} | process_input={:.2} ms n={} | \
             espera_da_saida={:.2} ms n={} estouradas={} | drenagem={:.2} ms n={} | \
             ate_o_credito_seguinte={:.2} ms max={:.1} ms n={} max_creditos={}",
            self.voltas,
            self.ramo_captura,
            self.ramo_evento,
            self.ramo_ocioso,
            self.voltas_sem_quadro,
            self.chegados_do_wgc,
            Self::media_ms(self.intervalo_de_captura_us, self.n_intervalos),
            self.intervalo_maximo_us as f64 / 1000.0,
            self.n_intervalos,
            Self::media_ms(self.escala_us, self.n_escala),
            self.n_escala,
            Self::media_ms(self.entrada_us, self.n_entrada),
            self.n_entrada,
            Self::media_ms(self.espera_us, self.n_espera),
            self.n_espera,
            self.esperas_estouradas,
            Self::media_ms(self.drenagem_us, self.n_drenagem),
            self.n_drenagem,
            Self::media_ms(self.ate_o_credito_seguinte_us, self.n_ate_o_credito_seguinte),
            self.ate_o_credito_seguinte_maximo_us as f64 / 1000.0,
            self.n_ate_o_credito_seguinte,
            self.maximo_de_creditos,
        )
    }
}

/// A cadeia montada e correndo.
pub struct Cadeia {
    captura: Captura,
    padroes: Option<PadroesDaTelaEstendida>,
    /// A textura do último quadro que entrou no encoder, **nossa** — para a repetição. Da origem
    /// sintética é a própria textura do anel; do WGC é uma cópia (`copia_para_repetir`), porque a
    /// do WGC volta ao pool de dois buffers no `frame.Close()` e pode ser reescrita pelo quadro
    /// seguinte no meio do encode (revisão adversarial de 13/09/2026).
    ultima_para_repetir: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
    copia_para_repetir: Option<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>,
    /// Quando o último quadro entrou no encoder.
    ultima_submissao: Option<Instant>,
    /// O carimbo do último quadro que entrou no encoder: o seguinte nunca vem antes dele (M1).
    ultimo_carimbo_submetido: Option<Instant>,
    /// Desde quando a câmera espera um destino livre do conversor (m6), para o prazo da
    /// reconferência: um MFT que segura os quadros sem devolver não trava a câmera para sempre.
    conversor_esperando_desde: Option<Instant>,
    /// Há um IDR pedido que ainda não saiu. Com a tela parada é ele que faz a repetição sair na
    /// hora, e não depois de `repetir_apos`.
    idr_pendente: bool,
    /// O quadro em `pendente` é uma repetição (não se guarda de novo, e conta em `repetidos`).
    proximo_e_repeticao: bool,
    /// **A câmera parada** (22/09): quando a última repetição dela foi posta em `pendente`. Espaça
    /// as tentativas também quando o encoder recusa a repetição (a `ultima_submissao` não anda).
    ultima_repeticao_da_camera: Option<Instant>,
    /// A cadeia não produz mais nada: a recriação do encoder falhou. Ver `motivo_de_morte`.
    morta: Option<String>,
    preferencia: Preferencia,
    /// A placa em que o encoder foi aberto **pelo LUID** (o monitor virtual; a câmera, na fase 3).
    /// A oficina e o recuo da recriação ativam o MFT dela, e não o primeiro da `preferencia` (a
    /// revisão adversarial de 18/09, M5). `None` no caminho de sempre.
    placa_do_encoder: Option<u64>,
    /// Reduz a textura capturada ao teto do núcleo. `None` quando a tela já cabia — o caminho
    /// sem reescala continua sendo o de antes, byte a byte.
    escalador: Option<escala::Escalador>,
    /// A câmera: NV12 ou YUY2 para o NV12 limitado no tamanho do teto (`conversor_de_camera.rs`).
    /// `None` na tela, e na câmera que já chega pronta.
    conversor: Option<crate::conversor_de_camera::ConversorDeCamera>,
    /// As trocas de aspecto da câmera que o conversor já encaixou (a fase 5, o DV 16:9 ↔ 4:3).
    trocas_de_aspecto_vistas: u64,
    enc: ChosenEncoder,
    eventos: Receiver<MftEvent>,
    creditos: u32,
    /// Uma caixa postal só, em vez de duas em série. Ver `Argumentos::caixa_unica`.
    caixa_unica: bool,
    /// Quando o último `ProcessInput` voltou. Serve a uma pergunta só: quanto o MFT demora, a
    /// partir daí, para pedir o quadro seguinte. Zerado assim que o crédito chega, para não
    /// contar o mesmo intervalo duas vezes quando o MFT manda dois créditos seguidos.
    entrada_aceita_em: Option<Instant>,
    pendente: Option<CapturedFrame>,
    submetidos: VecDeque<(u64, Instant)>,
    /// SPS+PPS em Annex-B, lidos do tipo de saída do MFT. `None` até o MFT publicá-los.
    parametros: Option<Vec<u8>>,
    /// O **primeiro** conjunto de parâmetros que esta sessão conheceu. Nunca muda depois de
    /// escrito: é a régua contra a qual todo conjunto publicado por um encoder recriado é
    /// comparado. Um receptor que entrou no começo montou o decodificador com este; se o encoder
    /// novo publicar outro, é este que o receptor vai continuar usando para decodificar o que
    /// vier — e a diferença, se houver, tem de aparecer no registro em vez de virar imagem
    /// quebrada sem explicação.
    parametros_de_referencia: Option<Vec<u8>>,
    resumo_sps: Option<ResumoSps>,
    inicio: Instant,
    indice: u64,
    duracao_100ns: i64,
    gop_frames: u32,
    /// Índices dos quadros em que um IDR de fato saiu — a prova, no fluxo, do que o encoder faz
    /// com os pedidos que a `ICodecAPI` aceita.
    idrs_em: Vec<u64>,
    pedidos_de_idr_em: Vec<u64>,
    /// Pedir IDR também reinicia o fluxo do MFT (a terceira porta). Atrás de flag porque errar
    /// a contabilidade de créditos **trava** o emissor — ver `pedir_idr`.
    idr_por_flush: bool,
    /// Quantas vezes o fluxo foi reiniciado. Ao lado de `idrs_em`, é o que separa "a porta
    /// funciona" de "a porta foi aberta".
    flushes: u64,

    // --- a quinta porta: derrubar e recriar o encoder -------------------------------------------
    //
    // O que precisa sobreviver a uma recriação, e por isso passou a morar aqui:
    //
    // - `dispositivo` e `gerenciador`: o MFT novo tem de nascer no **mesmo** adaptador D3D11 em
    //   que a captura já entrega textura. Recriar o dispositivo derrubaria a captura junto, que é
    //   exatamente o que esta porta não pode fazer.
    // - `cfg`: para o encoder novo ser o mesmo encoder, e não um encoder parecido.
    /// O dispositivo D3D11 compartilhado com a captura. **Não é recriado nunca.**
    ///
    /// Existiu para **manter o adaptador vivo** enquanto a cadeia existir; desde a F2b também é
    /// lido — a cópia do quadro para a repetição e o `GetDeviceRemovedReason` de
    /// `motivo_de_morte`. Hoje o `IMFDXGIDeviceManager` e a captura já seguram uma referência cada,
    /// mas as duas são de objetos que a quinta porta mexe ou pode vir a mexer; uma referência
    /// própria da `Cadeia` é o que garante que o dispositivo não some debaixo de uma recriação.
    #[allow(dead_code)]
    dispositivo: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    gerenciador: windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager,
    cfg: EncoderConfig,
    /// Pedir IDR derruba o transform e monta outro (a quinta porta). Atrás de flag pelo mesmo
    /// motivo da terceira: um caminho que pode travar o emissor não entra ligado por padrão antes
    /// de a bancada medir que ele não trava.
    idr_por_recriacao: bool,
    recriacoes: u64,
    /// Quanto tempo a chamada de recriação **bloqueou o laço** — derrubar o velho, enumerar,
    /// ativar, configurar e abrir o fluxo do novo. É o custo que o produto paga em imagem parada.
    custo_de_montagem_ms: Vec<u64>,
    /// Do instante da recriação até o IDR **sair no fio**. É a medida que decide se a porta vale:
    /// não o retorno da API, o quadro-chave de verdade.
    custo_ate_o_idr_ms: Vec<u64>,
    /// Instante da última recriação enquanto o IDR dela ainda não saiu.
    esperando_idr_desde: Option<Instant>,
    /// Quantos quadros o encoder **atual** já produziu. Zero significa que o próximo quadro dele
    /// vai ser IDR de qualquer jeito — recriar seria pagar 150 ms por algo que já vem.
    encodados_no_encoder_atual: u64,
    /// Pedidos de IDR que **não** viraram recriação, e por quê ver `pedir_idr`. Sem este número,
    /// "recriações=2 para 17 pedidos" pareceria a porta falhando quando é a porta se contendo.
    recriacoes_dispensadas: u64,

    // --- o piso de intervalo entre recriações ---------------------------------------------------
    //
    // Um limitador de taxa em cima da quinta porta. Ver `Argumentos::piso_entre_recriacoes_ms`
    // para o que ele troca por quê; aqui fica só o mecanismo.
    /// `None` é sem piso, que é o padrão.
    piso_entre_recriacoes: Option<Duration>,
    /// Instante da última recriação **concluída**. É contra ele que o piso mede.
    ultima_recriacao: Option<Instant>,
    /// Há um pedido de IDR guardado, esperando o piso vencer?
    ///
    /// **O piso adia, não recusa**, e este `bool` é a diferença entre as duas coisas. Um pedido
    /// recusado sumiria: o receptor ficaria com a imagem quebrada até pedir de novo, e o piso
    /// viraria um sorteio. Guardado, ele é atendido no instante em que o piso vence, e o custo do
    /// piso é exatamente a espera — que é o que a curva mede.
    pedido_adiado: bool,
    /// Desde quando o pedido guardado espera. Vira `espera_do_piso_ms` quando ele é atendido.
    pedido_adiado_desde: Option<Instant>,
    /// Quantos pedidos o piso adiou. Ao lado de `recriacoes`, separa "a porta se conteve" de "a
    /// porta não foi chamada".
    recriacoes_adiadas: u64,
    /// Quanto cada pedido adiado esperou até virar recriação. É o preço do piso em milissegundos,
    /// medido e não estimado — some com `custo_ate_o_idr_ms` para ter a recuperação total.
    espera_do_piso_ms: Vec<u64>,
    /// Intervalo entre recriações consecutivas. Com piso, é a testemunha de que ele pegou: sem
    /// piso esta lista tem valores abaixo de 1 s; com piso de N ms, nenhum abaixo de N.
    intervalos_entre_recriacoes_ms: Vec<u64>,
    // --- a troca a quente -----------------------------------------------------------------
    //
    // A quinta porta com a montagem fora do laço. Ver `oficina.rs` para os números que a
    // sustentam e `Argumentos::sem_troca_a_quente` para o que ela troca por quê.
    /// `None` quando a troca a quente está desligada — aí a porta é a de antes, e não há thread
    /// de oficina nenhuma no processo.
    oficina: Option<Oficina>,
    /// O encoder de reserva, montado e com o fluxo aberto, esperando a troca.
    reserva: Option<Reserva>,
    /// Quantas recriações foram trocas por ponteiro (custo ~0) em vez de derrubar e montar.
    trocas_a_quente: u64,
    /// Quantas vezes a porta pediu troca e a reserva **não estava pronta** — cada uma dessas caiu
    /// para o caminho antigo, com os ~150 ms de laço parado. É o número que diz se uma reserva
    /// só basta.
    trocas_sem_reserva: u64,
    /// Quanto cada reserva levou para ser montada **na thread de fundo**. Não é custo do laço; é
    /// o que diz se ela chega a tempo entre duas recriações.
    montagem_de_fundo_ms: Vec<u64>,
    /// O custo da troca **no laço**, em microssegundos — o número que a tarefa desta frente
    /// existia para derrubar. Em microssegundos e não em milissegundos porque em milissegundos
    /// ele seria uma coluna de zeros.
    custo_de_troca_us: Vec<u64>,

    /// Quantas recriações não conseguiram desligar o transform velho pelo `IMFShutdown` — cada
    /// uma dessas vaza uma thread de bombeamento de eventos e uma referência COM.
    desligamentos_sujos: u64,
    /// O conjunto de parâmetros mudou entre uma sessão de encoder e a seguinte? Um SPS/PPS novo no
    /// meio da sessão pode quebrar um receptor que já montou o decodificador.
    parametros_diferentes: u64,
    parametros_iguais: u64,

    /// Onde o tempo do laço vai. Ver `Perfil`.
    perfil: Perfil,
    ultimo_capturado: Option<Instant>,
    /// Intervalo pedido pela porta de taxa de entrega (`--taxa-de-entrega`). `None` é o
    /// comportamento de produto: sem porta, tudo o que a captura der atravessa.
    intervalo_de_entrega: Option<Duration>,
    /// Tamanho, em bytes, de cada unidade de acesso IDR que foi para o fio. Ver o comentário no
    /// ponto onde é preenchido.
    bytes_de_idr: Vec<usize>,
    /// Pacotes RTP de cada IDR, pela regra **exata** do pacotizador
    /// ([`quall_core::track::pacotes_da_unidade`]). Guardado ao lado dos bytes porque
    /// `ceil(bytes/1188)` erra: a divisão é por NAL, e a `generateFragments` da libdatachannel
    /// desconta o cabeçalho FU-A **depois** de emparelhar os fragmentos.
    pacotes_de_idr: Vec<u64>,
    soma_bytes_nao_idr: u64,
    soma_pacotes_nao_idr: u64,
    n_nao_idr: u64,
    /// O **máximo** dos quadros não-IDR, em bytes e em pacotes. Até 30/08 só existia a média
    /// (5.329 B no braço de produto), e média não cruza penhasco nenhum: a pergunta do achado 8 é
    /// se **algum** quadro chega perto dos 40 pacotes onde a perda desta LAN multiplica por cem.
    max_bytes_nao_idr: u64,
    max_pacotes_nao_idr: u64,
    /// Todos os quadros (IDR e não-IDR) distribuídos pelas faixas do penhasco. É a régua que
    /// responde "sim ou não" sem depender de média nem de máximo isolado.
    faixas_de_pacotes_por_quadro: [u64; 5],
    /// O perfil por posição depois de cada encoder nascer. Índice 0 é o primeiro quadro do encoder
    /// novo (o IDR obrigatório), 1 é o seguinte, e assim por diante; o último balde é o resto.
    posicoes_apos_recriacao: [PosicaoPosRecriacao; POSICOES_PERFILADAS + 1],
    /// **A rajada de saída**, que é a rajada que de fato existe no fio.
    ///
    /// `bombear` devolve um vetor e `emissor.rs` manda o vetor inteiro ao pacotizador numa
    /// sequência sem pausa — então dois quadros na mesma volta são pacotes **colados**, e é a soma
    /// deles, não o tamanho de um quadro, que se compara com o penhasco de 40–80. Um IDR de 20
    /// pacotes que saia colado a dois quadros de 10 já é uma rajada de 40.
    faixas_de_rajada_de_saida: [u64; 5],
    max_pacotes_por_volta: u64,
    voltas_com_saida: u64,
    soma_quadros_por_volta: u64,
    /// Instante do próximo quadro devido. Ver a nota de `Argumentos::taxa_de_entrega` para por que
    /// isto avança de `1/taxa` em vez de reancorar no quadro que passou.
    proxima_entrega: Option<Instant>,
    pub contadores: Contadores,
    pub largura: u32,
    pub altura: u32,
    pub nome_do_encoder: String,
    pub encoder_e_hardware: bool,
    pub adaptador: String,
    pub espacamento_aceito: bool,
}

impl Cadeia {
    /// Monta tudo: encoder → adaptador → captura do monitor escolhido.
    ///
    /// A **ordem é obrigatória** e foi medida (achado 3 do `README.md`): um MFT de hardware só
    /// aceita `MFT_MESSAGE_SET_D3D_MANAGER` com um dispositivo criado no mesmo adaptador que ele.
    /// Criar o dispositivo primeiro e torcer para bater dá `E_INVALIDARG`.
    ///
    /// `origem` é o zero da linha do tempo, e ele vem **de fora**.
    ///
    /// Antes desta rodada era `Instant::now()` aqui dentro, e estava certo enquanto havia uma track
    /// só. Com áudio na mesma sessão deixou de estar: `AmostraDeAudio::timestamp_us` e
    /// `QuadroCodificado::timestamp_us` só alinham as duas tracks se falarem do **mesmo instante
    /// zero**, e duas cadeias que chamam `Instant::now()` cada uma na sua abertura têm zeros
    /// diferentes por quanto tempo levar entre as duas chamadas. Quem cria o zero é `emissor.rs`,
    /// uma vez, antes de abrir qualquer uma das duas.
    #[allow(clippy::too_many_arguments)]
    pub fn abrir(
        fonte: &Fonte,
        hmonitor: windows::Win32::Graphics::Gdi::HMONITOR,
        fps: u32,
        origem: Instant,
        idr_por_flush: bool,
        idr_por_recriacao: bool,
        taxa_de_entrega: Option<u32>,
        piso_entre_recriacoes_ms: u64,
        bitrate_alvo: Option<u32>,
        troca_a_quente: bool,
        caixa_unica: bool,
    ) -> Result<Self> {
        // O caminho de sempre: o monitor escolhido, a ordem de MFT do produto, e nenhum padrão da
        // tela estendida. `abrir_com` com estes valores é, linha a linha, o `abrir` de antes.
        Self::abrir_com(
            OrigemDaCadeia::Monitor { fonte, hmonitor },
            OpcoesDaCadeia {
                fps,
                origem_do_relogio: origem,
                idr_por_flush,
                idr_por_recriacao,
                taxa_de_entrega,
                piso_entre_recriacoes_ms,
                bitrate_alvo,
                troca_a_quente,
                caixa_unica,
                preferencia: Preferencia::Produto,
                padroes: None,
            },
        )
    }

    /// **A placa da câmera**: a do encoder que ativa, pelo LUID, pulando as indiretas (o SudoVDA) e
    /// caindo para a próxima quando a ativação falha (`docs/camera-no-windows.md` §4.3, a revisão M4).
    /// Na ordem de produto a NVIDIA vem primeiro, como na tela; com `Preferencia::Intel`, só a Intel.
    /// O dispositivo nasce no **mesmo** LUID do encoder que ativou — nunca no primeiro da lista.
    ///
    /// **NV12 no NVENC não foi medido** (§4.1): no Dell a NVIDIA não ativa na sessão interativa, e a
    /// bancada pelo SSH usa `--preferir-intel`.
    fn encoder_e_placa_da_camera(preferencia: encoder::Preferencia) -> Result<(ChosenEncoder, device::ChosenAdapter)> {
        let mut placas = device::placas_de_hardware()?;
        match preferencia {
            encoder::Preferencia::Produto => placas.sort_by_key(|p| u8::from(p.vendor_id != device::VENDOR_NVIDIA)),
            encoder::Preferencia::Intel => placas.retain(|p| p.vendor_id == device::VENDOR_INTEL),
        }
        let mut tentativas: Vec<String> = Vec::new();
        for p in &placas {
            match encoder::ativar_h264_so_na_placa(p.luid) {
                Ok(enc) => match device::create_device_por_luid(p.luid) {
                    Ok(a) => {
                        registro::linha(format!(
                            "câmera: a placa é a do encoder que ativou — \"{}\" em {} (LUID {:016X}){}",
                            enc.friendly_name,
                            p.descricao,
                            p.luid,
                            if tentativas.is_empty() { String::new() } else { format!("; antes: {}", tentativas.join("; ")) }
                        ));
                        return Ok((enc, a));
                    }
                    Err(e) => {
                        let _ = encoder::desligar(&enc);
                        tentativas.push(format!("{} (LUID {:016X}): o dispositivo não abriu ({e})", p.descricao, p.luid));
                    }
                },
                Err(e) => tentativas.push(format!("{} (LUID {:016X}): {e}", p.descricao, p.luid)),
            }
        }
        Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            format!("nenhuma placa ativou um encoder H.264 para a câmera ({})", tentativas.join("; ")),
        ))
    }

    /// Monta a cadeia sobre qualquer origem, com os padrões da tela estendida quando pedidos.
    pub fn abrir_com(origem_da_cadeia: OrigemDaCadeia<'_>, opcoes: OpcoesDaCadeia) -> Result<Self> {
        let OpcoesDaCadeia {
            fps,
            origem_do_relogio: origem,
            idr_por_flush,
            idr_por_recriacao,
            taxa_de_entrega,
            piso_entre_recriacoes_ms,
            bitrate_alvo,
            troca_a_quente,
            caixa_unica,
            preferencia,
            padroes,
        } = opcoes;
        let e_camera = matches!(origem_da_cadeia, OrigemDaCadeia::Camera { .. } | OrigemDaCadeia::DoDono(_));
        let do_dono = matches!(origem_da_cadeia, OrigemDaCadeia::DoDono(_));
        // **Os padrões da tela estendida não valem para câmera** (a revisão do código da fase 3,
        // M1): a repetição do quadro parado entra carimbada com "agora", e o quadro seguinte da
        // câmera chega com a idade dele e cai antes dela. O IDR pedido sai no próximo quadro real
        // ou pela quinta porta, e o GOP de 30 s, o teto de quadro e o alvo "captura inteira" foram
        // pensados para tela. **A câmera parada tem a repetição dela** desde 22/09
        // (`talvez_repetir_a_camera`): só com ela parada há 400 ms, e sem cópia.
        let padroes = if e_camera {
            if padroes.is_some() {
                registro::linha("câmera: os padrões da tela estendida não valem para câmera (a repetição é só a da câmera parada; GOP de sempre)");
            }
            None
        } else {
            padroes
        };
        // A câmera escolhe a placa pelo LUID do encoder que ativa (M4); a tela, pelo encoder que a
        // ordem de produto ativou e o fabricante dele — o caminho de sempre, sem mudança.
        // **O dono escolheu a placa** (a Intel primeiro, §8.10): o encoder ativa nela, pelo LUID, e o
        // dispositivo é o dele — nenhum dispositivo novo.
        let placa_do_dono = match &origem_da_cadeia {
            OrigemDaCadeia::DoDono(l) => Some(l.placa.clone()),
            _ => None,
        };
        let (enc, adaptador, placa_do_encoder) = if let Some(p) = &placa_do_dono {
            let enc = encoder::ativar_h264_so_na_placa(p.luid)?;
            registro::linha(format!(
                "r5: a cadeia da rede ativou \"{}\" na placa do dono \"{}\" (LUID {:016X})",
                enc.friendly_name, p.descricao, p.luid
            ));
            (enc, p.como_adaptador(), Some(p.luid))
        } else if e_camera {
            let (enc, adaptador) = Self::encoder_e_placa_da_camera(preferencia)?;
            let luid = adaptador.luid;
            (enc, adaptador, Some(luid))
        } else {
            let enc = encoder::ativar_h264(preferencia)?;
            let vendor = if enc.friendly_name.to_uppercase().contains("NVIDIA") {
                device::VENDOR_NVIDIA
            } else {
                device::VENDOR_INTEL
            };
            let adaptador = device::create_device(vendor)?;
            (enc, adaptador, None)
        };
        registro::linha(format!(
            "encoder: \"{}\" hardware={}",
            enc.friendly_name, enc.is_hardware
        ));
        // O LUID vai junto porque o nome não separa as placas: o adaptador do SudoVDA se chama
        // "Intel(R) UHD Graphics 630", 0x8086, como a Intel de verdade (`device::create_device`).
        registro::linha(format!(
            "adaptador: {} (vendor 0x{:04X}) luid=0x{:016X}",
            adaptador.description, adaptador.vendor_id, adaptador.luid
        ));
        if e_camera && !do_dono {
            // **A proteção multithread, ligada** (§4.4): o leitor e o decodificador MJPEG usam o
            // dispositivo das threads deles, e a sessão copia e converte no mesmo contexto imediato.
            // O precedente no emissor é o monitor virtual (`sessao_de_emissao.rs`).
            match device::proteger_contexto(&adaptador.context, true) {
                Ok(antes) => registro::linha(format!("câmera: proteção multithread do contexto ligada (antes={antes})")),
                Err(e) => registro::linha(format!("câmera: a proteção multithread não ligou ({e})")),
            }
        }

        // O gerenciador DXGI: o do dono (o encoder da rede e o do gravador no mesmo), ou um novo.
        let gerenciador = match &placa_do_dono {
            Some(p) => p.gerenciador.0.clone(),
            None => encoder::create_device_manager(&adaptador.device)?,
        };
        let captura = match origem_da_cadeia {
            OrigemDaCadeia::Monitor { fonte, hmonitor } => {
                let c = ScreenCapture::start_for_monitor(&adaptador.device, hmonitor)?;
                registro::linha(format!(
                    "captura iniciada: {} ({}) {}x{}",
                    fonte.id, fonte.nome, c.width, c.height
                ));
                Captura::Tela(c)
            }
            OrigemDaCadeia::Sintetica { largura, altura, fps: fps_da_origem, carga, ritmo } => {
                let s = OrigemSintetica::nova(
                    &adaptador.device,
                    largura,
                    altura,
                    fps_da_origem,
                    carga,
                    ritmo,
                )?;
                registro::linha(format!(
                    "captura iniciada: origem SINTÉTICA (nenhuma tela é capturada) carga={carga:?} \
                     ritmo={ritmo:?} {}x{} @{fps_da_origem}",
                    s.width, s.height
                ));
                Captura::Sintetica(s)
            }
            OrigemDaCadeia::Camera { fonte, parar, bater } => {
                // O teto que a escolha do tipo nativo respeita: o alvo padrão do núcleo (1080p30),
                // com o fps da sessão por cima.
                let alvo = quall_core::teto::Alvo::PADRAO;
                let teto_da_camera = crate::regras_da_camera::TetoDaCamera { max_macroblocos: alvo.max_fs, fps: fps.min(alvo.fps) };
                match crate::captura_de_camera::CapturaDeCamera::abrir(&adaptador.device, &gerenciador, fonte, teto_da_camera, origem, parar, bater) {
                    Ok(c) => Captura::Camera(c),
                    Err(texto) => {
                        let _ = encoder::desligar(&enc);
                        return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, texto));
                    }
                }
            }
            OrigemDaCadeia::DoDono(l) => {
                registro::linha(format!(
                    "captura: a câmera do dono ({}x{} {:?}, {:?}) — a rede se pendura, a câmera não é aberta aqui",
                    l.info.largura, l.info.altura, l.info.formato, l.info.modo
                ));
                Captura::DoDono(l)
            }
        };
        // A partir daqui o resto da montagem só vê largura e altura, venham de onde vierem.
        let (larg_da_captura, alt_da_captura) = (captura.largura(), captura.altura());
        // **A câmera de pixel não quadrado** (a fase 5, o DV em 16:9, que o S24 mostrava comprimido):
        // o teto e a saída do conversor saem do tamanho **em pixel quadrado** (720×480 a 32:27 →
        // 854×480), e o conversor escala. Toda outra origem, e a câmera que não declara PAR, seguem
        // com o tamanho da captura, como antes.
        let (larg_exibida, alt_exibida) = match &captura {
            Captura::DoDono(l) => l.tamanho_exibido(),
            Captura::Camera(c) => {
                let exibida = c.tamanho_exibido();
                if exibida != (larg_da_captura, alt_da_captura) {
                    let a = c.aspecto();
                    registro::linha(format!(
                        "câmera: pixel não quadrado ({:?}, PAR {}:{}): {larg_da_captura}x{alt_da_captura} sai como {}x{} em pixel quadrado",
                        a.origem, a.par.num, a.par.den, exibida.0, exibida.1
                    ));
                }
                exibida
            }
            _ => (larg_da_captura, alt_da_captura),
        };

        // --- o teto ---------------------------------------------------------------------------
        //
        // **A decisão não é daqui.** `quall_core::teto::ajustar` lê o `profile-level-id` que o
        // próprio núcleo anuncia no SDP e devolve o que cabe nele. Até esta rodada este emissor
        // mandava a resolução nativa do monitor — 1920x1080, nível 4.0 medido no fio — contra os
        // 3.1 que o SDP prometia, e o custo não era a mentira no campo: era que o conjunto de
        // parâmetros de um IDR de 1080p ocupa 163 pacotes RTP, e um pacote perdido condena o
        // quadro inteiro. A 8,6% de perda, nenhum conjunto chega, e a tela fica preta para sempre
        // (`docs/tela-preta.md` §9).
        //
        // **Na tela estendida o alvo é o monitor inteiro**, como no Mac (`Emissor.tetoDoNucleo`):
        // o alvo padrão do núcleo (1080p) reduziria um monitor de 1920 × 1200 e borraria a letra.
        // Continua valendo o menor entre o alvo e o nível que o SDP anuncia.
        let teto = match padroes {
            None => quall_core::teto::ajustar(larg_exibida, alt_exibida, fps),
            Some(_) => quall_core::teto::ajustar_para(
                larg_exibida,
                alt_exibida,
                fps,
                Some(quall_core::teto::Alvo {
                    max_fs: quall_core::teto::LimitesDoNivel::macroblocos(
                        larg_exibida,
                        alt_exibida,
                    ),
                    fps,
                }),
            ),
        };
        registro::linha(teto.relato(larg_exibida, alt_exibida, fps));
        // Procedência do braço, na própria corrida. Duas frentes já concluíram coisa errada por
        // medir um aparelho no estado em que outro experimento o deixou; um braço que não diz em
        // que estado ele mesmo estava tem o mesmo defeito.
        registro::linha(match taxa_de_entrega.filter(|t| *t > 0) {
            Some(t) => format!(
                "porta de taxa de entrega: LIGADA em {t} fps (o encoder segue configurado \
                 para {fps})"
            ),
            None => "porta de taxa de entrega: DESLIGADA (padrão de produto)".to_string(),
        });
        // Procedência do outro braço desta bancada, pela mesma razão: um braço que não diz em que
        // caminho ele mesmo estava não pode ser comparado com outro.
        registro::linha(if caixa_unica {
            "caminho do quadro: CAIXA ÚNICA (o quadro fica no slot do WGC até haver crédito)"
        } else {
            "caminho do quadro: duas caixas em série (padrão de produto)"
        });

        // Reduzir custa um `Blt` na mesma GPU; não reduzir custava a transmissão. Se o escalador
        // não subir, a sessão **não** cai de volta para o tamanho nativo em silêncio: emitir
        // acima do contrato é o defeito que esta rodada existe para acabar, e um caminho de
        // exceção que o reintroduzisse calado seria pior que uma falha visível.
        // **A câmera passa pelo conversor, e não pelo escalador BGRA**: NV12 ou YUY2 entram, NV12 de
        // faixa limitada sai, no tamanho do teto — e só quando precisa (escala, YUY2 ou faixa
        // completa; `regras_da_camera::precisa_do_processador`).
        let conversor = match &captura {
            Captura::Camera(c)
                if crate::regras_da_camera::precisa_do_processador(
                    c.formato.subtipo(),
                    c.faixa_completa,
                    // Escalar: ao teto, ou ao pixel quadrado (o DV anamórfico).
                    teto.reduziu_tamanho || (larg_exibida, alt_exibida) != (larg_da_captura, alt_da_captura),
                    // O do quadro **no anel** (22/09): com o adapt2, já progressivo.
                    c.entrelacamento_no_anel != crate::regras_da_camera::Entrelacamento::Progressivo,
                ) =>
            {
                match crate::conversor_de_camera::ConversorDeCamera::novo(
                    &adaptador.device,
                    c.formato.dxgi(),
                    larg_da_captura,
                    alt_da_captura,
                    teto.largura,
                    teto.altura,
                    c.faixa_completa,
                    c.matriz_709,
                    c.entrelacamento_no_anel,
                ) {
                    Ok(conv) => {
                        registro::linha(format!("conversor da câmera: {}", conv.descricao));
                        Some(conv)
                    }
                    Err(e) => {
                        registro::linha(format!("conversor da câmera NÃO subiu: {e}"));
                        return Err(e);
                    }
                }
            }
            Captura::Camera(_) => {
                registro::linha("conversor da câmera: não precisa (NV12 de faixa limitada no tamanho do teto)");
                None
            }
            // **A câmera do dono passa sempre pelo conversor** (a revisão do plano, B1): o MFT nunca
            // segura uma posição do anel da rede, que o dono escreve na thread dele.
            Captura::DoDono(l) => {
                match crate::conversor_de_camera::ConversorDeCamera::novo(
                    &adaptador.device,
                    l.info.formato.dxgi(),
                    larg_da_captura,
                    alt_da_captura,
                    teto.largura,
                    teto.altura,
                    l.info.faixa_completa,
                    l.info.matriz_709,
                    l.info.entrelacamento_no_anel,
                ) {
                    Ok(conv) => {
                        registro::linha(format!("conversor da câmera do dono (sempre): {}", conv.descricao));
                        Some(conv)
                    }
                    Err(e) => {
                        registro::linha(format!("conversor da câmera do dono NÃO subiu: {e}"));
                        let _ = encoder::desligar(&enc);
                        return Err(e);
                    }
                }
            }
            _ => None,
        };
        let escalador = if teto.reduziu_tamanho && !e_camera {
            match escala::Escalador::novo(
                &adaptador.device,
                larg_da_captura,
                alt_da_captura,
                teto.largura,
                teto.altura,
            ) {
                Ok(e) => {
                    registro::linha(format!(
                        "escalador: {}x{} -> {}x{} no ID3D11VideoProcessor",
                        larg_da_captura, alt_da_captura, teto.largura, teto.altura
                    ));
                    Some(e)
                }
                Err(e) => {
                    registro::linha(format!("escalador NÃO subiu: {e}"));
                    return Err(e);
                }
            }
        } else {
            None
        };

        // Preset de tela: `docs/ux-m6.md` tarefa 3 — GOP de 1 s. Área plana comprime bem, mas
        // uma mudança brusca de cena precisa de IDR rápido. A câmera leva o dela
        // (`quall_core::media::preset_para`; a revisão do código da fase 3, m9). Hoje ele só vai
        // para o registro: nenhum parâmetro do encoder sai do preset.
        let preset = quall_core::media::preset_para(!e_camera);
        // **O alvo sai do mesmo teto que a geometria**, e é a segunda metade da decisão que ele
        // já tomava: `teto.teto_de_taxa_bps` acompanha o quadro que de fato vai sair (1080p30
        // pede 9 Mbps; 720p30 continua pedindo os 4 Mbps de sempre). Até 02/09/2026 este número
        // era `4_000_000` cravado no `Argumentos`, e não subiu quando a resolução subiu.
        //
        // `--bitrate-alvo` continua mandando quando é dado: é o que iguala os bits dos dois braços
        // do A/B da quinta porta — ver `Argumentos::bitrate_alvo`.
        let bitrate = bitrate_alvo.unwrap_or(teto.teto_de_taxa_bps);
        // Na tela estendida, o GOP pedido é o do Mac (30 s); é **pedido**, e o fluxo diz o que o
        // encoder fez com ele (`medida_de_idr`) — o Quick Sync já ignorou todo GOP pedido.
        let gop_frames = match padroes {
            None => teto.fps,
            Some(p) => teto.fps.saturating_mul(p.gop_segundos.max(1)),
        };
        // O teto de quadro, em bits: N quadros médios da sessão (`EncoderConfig::teto_de_quadro_bits`).
        let teto_de_quadro_bits = match padroes {
            Some(p) if p.teto_de_quadro_em_medios > 0.0 => {
                (p.teto_de_quadro_em_medios * f64::from(bitrate) / f64::from(teto.fps.max(1))) as u32
            }
            _ => 0,
        };

        let cfg = EncoderConfig {
            // A tela e a origem sintética entregam BGRA; a câmera, NV12 (do anel ou do conversor).
            entrada: if e_camera { FormatoDeEntrada::Nv12 } else { FormatoDeEntrada::Argb32 },
            width: teto.largura,
            height: teto.altura,
            fps: teto.fps,
            bitrate_bps: bitrate,
            gop_frames,
            // Os botões de `docs/idr-pequeno.md` nascem desligados; ver `EncoderConfig`.
            intra_refresh_frames: 0,
            slice_bytes: 0,
            teto_de_quadro_bits,
        };
        encoder::configure(&enc, &gerenciador, &cfg)?;
        let espacamento_aceito = encoder::tentar_espacamento_de_idr(&enc, gop_frames);
        registro::linha(format!(
            "preset={preset:?} bitrate={bitrate} gop_pedido={gop_frames} quadros | \
             MF_MT_MAX_KEYFRAME_SPACING: {}",
            if espacamento_aceito { "aceito" } else { "recusado" }
        ));
        if let Some(p) = padroes {
            registro::linha(format!(
                "padrões da tela estendida: repetir o quadro parado depois de {} ms | GOP pedido \
                 {} s ({gop_frames} quadros), sem IDR forçado a cada {} s (seria uma recriação) | \
                 teto de quadro {}",
                p.repetir_apos.as_millis(),
                p.gop_segundos,
                p.gop_segundos,
                if teto_de_quadro_bits > 0 {
                    format!(
                        "pedido {} B ({:.1} quadros médios de {} bps a {} fps) — {}",
                        teto_de_quadro_bits / 8,
                        p.teto_de_quadro_em_medios,
                        bitrate,
                        teto.fps,
                        encoder::teto_de_quadro_relido(&enc)
                    )
                } else {
                    "desligado".to_string()
                },
            ));
        }
        encoder::start_stream(&enc.transform)?;
        let eventos = encoder::spawn_event_pump(enc.events.clone());

        // A oficina sobe **por último**, com o encoder de serviço já montado e com o fluxo
        // aberto: a primeira coisa que ela faz é enumerar o registro de MFTs e ativar o segundo
        // transform, e fazer isso em paralelo com a montagem do primeiro seria medir uma corrida
        // que ninguém pediu.
        let oficina = if troca_a_quente && idr_por_recriacao {
            registro::linha(
                "troca a quente: LIGADA — a reserva é montada fora do laço e a troca é por \
                 ponteiro. Ver oficina.rs.",
            );
            Some(Oficina::abrir(&gerenciador, cfg, preferencia, placa_do_encoder, e_camera))
        } else {
            registro::linha(format!(
                "troca a quente: desligada ({})",
                if idr_por_recriacao { "por --sem-troca-a-quente" } else { "a quinta porta está desligada" }
            ));
            None
        };

        Ok(Cadeia {
            largura: teto.largura,
            altura: teto.altura,
            escalador,
            conversor,
            nome_do_encoder: enc.friendly_name.clone(),
            encoder_e_hardware: enc.is_hardware,
            adaptador: format!("{} (0x{:04X})", adaptador.description, adaptador.vendor_id),
            captura,
            padroes,
            ultima_para_repetir: None,
            copia_para_repetir: None,
            ultima_submissao: None,
            ultimo_carimbo_submetido: None,
            conversor_esperando_desde: None,
            trocas_de_aspecto_vistas: 0,
            idr_pendente: false,
            proximo_e_repeticao: false,
            ultima_repeticao_da_camera: None,
            morta: None,
            preferencia,
            placa_do_encoder,
            enc,
            eventos,
            creditos: 0,
            caixa_unica,
            entrada_aceita_em: None,
            pendente: None,
            submetidos: VecDeque::new(),
            parametros: None,
            parametros_de_referencia: None,
            resumo_sps: None,
            inicio: origem,
            indice: 0,
            duracao_100ns: 10_000_000i64 / teto.fps.max(1) as i64,
            gop_frames,
            idrs_em: Vec::new(),
            pedidos_de_idr_em: Vec::new(),
            contadores: Contadores::default(),
            idr_por_flush,
            flushes: 0,
            dispositivo: adaptador.device.clone(),
            gerenciador,
            cfg,
            idr_por_recriacao,
            oficina,
            reserva: None,
            trocas_a_quente: 0,
            trocas_sem_reserva: 0,
            montagem_de_fundo_ms: Vec::new(),
            custo_de_troca_us: Vec::new(),
            recriacoes: 0,
            custo_de_montagem_ms: Vec::new(),
            custo_ate_o_idr_ms: Vec::new(),
            esperando_idr_desde: None,
            encodados_no_encoder_atual: 0,
            recriacoes_dispensadas: 0,
            // Zero é sem piso, pela mesma leitura que `--taxa-de-entrega 0`: um piso de 0 ms não
            // é um piso, é a ausência dele.
            piso_entre_recriacoes: Some(piso_entre_recriacoes_ms)
                .filter(|p| *p > 0)
                .map(Duration::from_millis),
            ultima_recriacao: None,
            pedido_adiado: false,
            pedido_adiado_desde: None,
            recriacoes_adiadas: 0,
            espera_do_piso_ms: Vec::new(),
            intervalos_entre_recriacoes_ms: Vec::new(),
            desligamentos_sujos: 0,
            parametros_diferentes: 0,
            parametros_iguais: 0,
            perfil: Perfil::default(),
            ultimo_capturado: None,
            // Zero seria uma porta que nunca abre; o `filter` transforma isso em "sem porta", que
            // é a leitura que qualquer um faria de `--taxa-de-entrega 0`.
            intervalo_de_entrega: taxa_de_entrega
                .filter(|t| *t > 0)
                .map(|t| Duration::from_secs_f64(1.0 / f64::from(t))),
            proxima_entrega: None,
            bytes_de_idr: Vec::new(),
            pacotes_de_idr: Vec::new(),
            soma_bytes_nao_idr: 0,
            soma_pacotes_nao_idr: 0,
            n_nao_idr: 0,
            max_bytes_nao_idr: 0,
            max_pacotes_nao_idr: 0,
            faixas_de_pacotes_por_quadro: [0; 5],
            posicoes_apos_recriacao: [PosicaoPosRecriacao::default(); POSICOES_PERFILADAS + 1],
            faixas_de_rajada_de_saida: [0; 5],
            max_pacotes_por_volta: 0,
            voltas_com_saida: 0,
            soma_quadros_por_volta: 0,
            espacamento_aceito,
        })
    }

    /// O despachante da quinta porta: **troca por ponteiro se houver reserva**, e cai para o
    /// caminho de derrubar e montar se não houver.
    ///
    /// A queda não é cautela — é a única resposta certa. Um pedido de quadro-chave que não fosse
    /// atendido deixaria o receptor com a imagem quebrada até ele pedir de novo, e é exatamente
    /// esse o defeito que a quinta porta existe para não ter. Melhor pagar os 150 ms desta vez do
    /// que não consertar.
    pub fn recriar_encoder(&mut self) -> Result<()> {
        if self.oficina.is_some() {
            if let Some(reserva) = self.reserva.take() {
                return self.trocar_a_quente(reserva);
            }
            self.trocas_sem_reserva += 1;
        }
        self.recriar_derrubando()
    }

    /// **A troca a quente.** O encoder de reserva já está montado, configurado e com o fluxo
    /// aberto; aqui só se trocam ponteiros e se manda o aposentado para a oficina.
    ///
    /// # O que troca junto com o transform, e por quê
    ///
    /// É a mesma lista da recriação de hoje, e pelo mesmo motivo: os créditos de
    /// `METransformNeedInput` e a fila de `submetidos` pertenciam a um transform que saiu de
    /// serviço. Gastá-los seria submeter quadro ao encoder errado, e a conta de latência casaria
    /// entrada com saída trocadas, calada.
    ///
    /// A diferença é o que **não** acontece aqui: nada é destruído, nada é enumerado, nada é
    /// configurado. O `desligar` do aposentado e a montagem do sucessor acontecem na thread da
    /// oficina, depois que esta função já voltou.
    ///
    /// `pendente` fica, como no caminho antigo: é um quadro que veio da captura, não do encoder.
    fn trocar_a_quente(&mut self, reserva: Reserva) -> Result<()> {
        let comeco = Instant::now();

        let velho = std::mem::replace(&mut self.enc, reserva.enc);
        // O canal do bombeador velho morre aqui: sem receptor, o `send` da thread dele falha e
        // ela sai. Quem a solta de dentro do `GetEvent` é o `IMFShutdown` que a oficina vai
        // chamar; as duas coisas se somam e nenhuma depende da outra.
        let canal_velho = std::mem::replace(&mut self.eventos, reserva.eventos);
        drop(canal_velho);
        self.contar_o_que_a_troca_leva();
        self.creditos = 0;
        self.submetidos.clear();
        self.encodados_no_encoder_atual = 0;
        self.espacamento_aceito = reserva.espacamento;

        // O conjunto de parâmetros do encoder novo, pela mesma porta única de sempre.
        self.parametros = None;
        let novos = encoder::conjuntos_de_parametros(&self.enc);
        if let Some(novos) = novos {
            self.registrar_parametros(novos);
        }

        // O aposentado vai para a oficina, que o desliga e monta o sucessor no `IMFActivate`
        // dele. É o que mantém as duas vagas girando sem reenumerar.
        if let Some(of) = self.oficina.as_mut() {
            of.reciclar(velho);
        }

        self.recriacoes += 1;
        self.trocas_a_quente += 1;
        if let Some(anterior) = self.ultima_recriacao {
            self.intervalos_entre_recriacoes_ms
                .push(comeco.duration_since(anterior).as_millis() as u64);
        }
        self.ultima_recriacao = Some(comeco);
        self.esperando_idr_desde = Some(comeco);
        let custo = comeco.elapsed().as_micros() as u64;
        self.custo_de_troca_us.push(custo);
        // `custo_de_montagem_ms` continua sendo o custo **no laço**, para as duas portas se
        // compararem na mesma coluna: aqui ele é ~0.
        self.custo_de_montagem_ms.push(comeco.elapsed().as_millis() as u64);
        registro::linha(format!(
            "troca a quente: encoder trocado por ponteiro em {custo} µs (troca nº {}, recriação \
             nº {}) — montagem da reserva levou {} ms na thread de fundo | espaçamento {} | \
             conjunto de parâmetros do MFT novo: {}",
            self.trocas_a_quente,
            self.recriacoes,
            reserva.montagem_ms,
            if reserva.espacamento { "aceito" } else { "recusado" },
            match &self.parametros {
                Some(p) => format!("{} bytes", p.len()),
                None => "ainda não publicado".to_string(),
            },
        ));
        Ok(())
    }

    /// Colhe a reserva que a oficina terminou de montar. Chamada a cada volta do laço, e a
    /// primeira coisa que ela faz é a barata.
    fn colher_reserva(&mut self) {
        if self.reserva.is_some() {
            return;
        }
        let colhida = match self.oficina.as_mut() {
            Some(of) => of.colher(),
            None => return,
        };
        match colhida {
            None => {}
            Some(Ok(r)) => {
                if r.desligamento_limpo == Some(false) {
                    self.desligamentos_sujos += 1;
                    registro::linha(
                        "aviso: o MFT aposentado não expôs IMFShutdown — a thread de eventos dele \
                         não vai sair e a referência COM fica viva. Isto VAZA por troca.",
                    );
                }
                self.montagem_de_fundo_ms.push(r.montagem_ms);
                self.reserva = Some(r);
            }
            Some(Err(e)) => registro::linha(format!(
                "aviso: a oficina não conseguiu montar a reserva ({e}); a próxima recriação vai \
                 pelo caminho antigo"
            )),
        }
    }

    /// **A quinta porta: derrubar a sessão do encoder e montar outra.**
    ///
    /// As quatro portas anteriores pediam ao MFT que mudasse de comportamento —
    /// `AVEncMPVGOPSize` (recusado), `AVEncVideoForceKeyFrame` (aceito e ignorado),
    /// `MF_MT_MAX_KEYFRAME_SPACING` (aceito e ignorado), `COMMAND_FLUSH` (nocivo: destrói o IDR
    /// que ia atender o pedido). Esta é diferente em espécie: **não pede nada.** O transform é
    /// destruído e outro nasce no lugar. Um encoder recém-criado não tem cadeia de referência
    /// para apontar, então o primeiro quadro dele é obrigatoriamente IDR — é a única coisa que a
    /// norma garante sem depender de o driver querer.
    ///
    /// # O que NÃO é derrubado, e por quê
    ///
    /// A captura (`Windows.Graphics.Capture`), o dispositivo D3D11 e o `IMFDXGIDeviceManager`
    /// continuam de pé. Derrubar o dispositivo levaria a captura junto — o pool de quadros do WGC
    /// nasce nele — e o produto pararia de ver a tela para conseguir um quadro-chave, que é
    /// trocar o problema por um pior. O escalador também sobrevive: ele é um `Blt` no mesmo
    /// dispositivo.
    ///
    /// # A contabilidade de créditos, que é o que quase matou a porta 4
    ///
    /// O MFT é assíncrono e a `Cadeia` guarda créditos de `METransformNeedInput`. Os créditos são
    /// de um transform que **deixou de existir**: gastá-los seria submeter quadro a um objeto
    /// morto. Aqui isso é mais simples de acertar que no `FLUSH`, porque não há dúvida sobre o que
    /// sobrevive — nada sobrevive. `creditos`, `submetidos` e o bombeador de eventos são todos
    /// substituídos junto com o transform, num ponto só.
    ///
    /// `pendente` é o único que **fica**: é um quadro que veio da captura, não do encoder, e
    /// jogá-lo fora perderia imagem sem motivo.
    ///
    /// # A ordem, e o que ela protege
    ///
    /// O velho é desligado primeiro (`encoder::desligar`) e só então o novo é montado. Até
    /// 30/08 a razão escrita aqui era que *"dois MFTs de hardware vivos ao mesmo tempo no mesmo
    /// adaptador é estado que esta bancada nunca mediu"*. **A bancada mediu**: eles coexistem, os
    /// dois codificando ao mesmo tempo no mesmo `IMFDXGIDeviceManager` — é o achado que a troca a
    /// quente (`trocar_a_quente`) usa. A ordem daqui continua sendo esta porque este é o caminho
    /// **de queda**, que roda quando não há reserva pronta: se a montagem do novo falhar, a
    /// cadeia fica sem encoder e o erro sobe. Um caminho que "voltasse ao encoder antigo" não
    /// existe: ele já foi desligado.
    ///
    /// Deixou de ser o caminho normal em 30/08. Ele é o que roda com `--sem-troca-a-quente`
    /// (o negativo medido, que é o costume da casa) e quando a reserva não chegou a tempo.
    fn recriar_derrubando(&mut self) -> Result<()> {
        let comeco = Instant::now();

        // 1. O velho, e a testemunha de que ele morreu limpo.
        let limpo = encoder::desligar(&self.enc);
        if !limpo {
            self.desligamentos_sujos += 1;
            registro::linha(
                "aviso: o MFT não expôs IMFShutdown — a thread de eventos do transform velho não \
                 vai sair, e a referência COM dele fica viva. Isto VAZA por recriação.",
            );
        }

        // 2. O novo, com a mesma configuração — não uma parecida. E do **mesmo** `IMFActivate`:
        //    qual encoder é o certo já foi decidido na abertura da sessão, e reenumerar o registro
        //    de MFTs no meio dela custaria tempo para responder uma pergunta que não mudou. Se a
        //    reativação falhar, cai para a enumeração completa — porque ficar sem encoder é pior
        //    que gastar 20 ms.
        let enc = match encoder::reativar(&self.enc) {
            Ok(e) => e,
            Err(erro) => {
                registro::linha(format!(
                    "aviso: reativar o mesmo IMFActivate falhou ({erro}); reenumerando os MFTs"
                ));
                // Com a mesma preferência da abertura: reenumerar pela ordem de produto numa
                // cadeia aberta no Quick Sync poria um MFT da NVIDIA contra o gerenciador Intel.
                match self.placa_do_encoder {
                    Some(luid) => {
                        // A câmera não aceita o recuo "só Intel": o dispositivo dela é o da placa
                        // do LUID (m7).
                        let (enc, como) = encoder::ativar_h264_da_placa(luid, self.captura.e_camera())?;
                        // Por onde o MFT veio: o recuo "só Intel" de `ativar_h264_na_placa` pode
                        // pegar outra placa numa máquina com duas Intel (a revisão de código, m4).
                        registro::linha(format!(
                            "recriação: encoder da placa {luid:016X}: \"{}\" ({como})",
                            enc.friendly_name
                        ));
                        enc
                    }
                    None => encoder::ativar_h264(self.preferencia)?,
                }
            }
        };
        encoder::configure(&enc, &self.gerenciador, &self.cfg)?;
        let espacamento = encoder::tentar_espacamento_de_idr(&enc, self.gop_frames);
        encoder::start_stream(&enc.transform)?;
        let eventos = encoder::spawn_event_pump(enc.events.clone());

        // 3. O estado que pertencia ao transform velho, num ponto só.
        self.enc = enc;
        self.eventos = eventos;
        self.contar_o_que_a_troca_leva();
        self.creditos = 0;
        self.submetidos.clear();
        self.encodados_no_encoder_atual = 0;
        self.espacamento_aceito = espacamento;

        // 4. O conjunto de parâmetros. É a pergunta 3 da frente: um SPS/PPS diferente no meio da
        //    sessão pode quebrar um receptor que já montou o decodificador com o anterior. O blob
        //    é relido do MFT novo e comparado byte a byte com o do velho.
        //
        // O blob costuma só aparecer depois da renegociação de tipo de saída que segue o primeiro
        // `SetInputType` (achado 5) — então ele pode ainda não existir neste instante. Se não
        // existir agora, a comparação acontece de qualquer jeito, mais tarde, quando
        // `guardar_parametros`/`garantir_parametros` o encontrarem: as duas passam pelo mesmo
        // `registrar_parametros`. Nenhum caminho enche `self.parametros` sem comparar.
        self.parametros = None;
        if let Some(novos) = encoder::conjuntos_de_parametros(&self.enc) {
            self.registrar_parametros(novos);
        }

        self.recriacoes += 1;
        // O intervalo é medido de recriação a recriação, e não de pedido a pedido: é o número que
        // o piso governa, e a testemunha de que ele pegou.
        if let Some(anterior) = self.ultima_recriacao {
            self.intervalos_entre_recriacoes_ms
                .push(comeco.duration_since(anterior).as_millis() as u64);
        }
        self.ultima_recriacao = Some(comeco);
        self.esperando_idr_desde = Some(comeco);
        let custo = comeco.elapsed().as_millis() as u64;
        self.custo_de_montagem_ms.push(custo);
        registro::linha(format!(
            "quinta porta: encoder recriado em {custo} ms (recriação nº {}) — desligamento \
             {} | espaçamento {} | conjunto de parâmetros do MFT novo: {}",
            self.recriacoes,
            if limpo { "limpo" } else { "SUJO" },
            if espacamento { "aceito" } else { "recusado" },
            match &self.parametros {
                Some(p) => format!("{} bytes", p.len()),
                None => "ainda não publicado".to_string(),
            },
        ));
        Ok(())
    }

    /// Guarda um conjunto de parâmetros e **compara com o primeiro que esta sessão conheceu**.
    ///
    /// Ponto único: `guardar_parametros` (saída solta do encoder), `garantir_parametros` (releitura
    /// do blob do MFT) e `recriar_encoder` passam todos por aqui. Se um dia um quarto caminho
    /// escrever em `self.parametros` sem passar por este, a pergunta "o conjunto mudou?" volta a
    /// não ter resposta.
    fn registrar_parametros(&mut self, novos: Vec<u8>) {
        self.conferir_parametros(&novos, "blob do MFT");
        self.parametros = Some(novos);
    }

    /// Compara um conjunto de parâmetros com o de referência **sem** guardá-lo.
    ///
    /// Existe separado de `registrar_parametros` porque a comparação que importa mudou de fonte no
    /// meio desta frente. A hipótese era ler o blob `MF_MT_MPEG_SEQUENCE_HEADER` do MFT antes e
    /// depois da recriação — mas medido nesta bancada com o teto ligado, `parametros_injetados=0`
    /// e `saidas_so_de_parametros=0`: **este MFT já manda SPS e PPS dentro de cada IDR**, e o
    /// blob nunca chega a ser lido. Comparar o blob responderia uma pergunta que o receptor não
    /// faz.
    ///
    /// O que o receptor lê é o que vem no fio, então é isso que se compara: os NAL 7 e 8 extraídos
    /// de cada IDR que sai. Se um encoder recriado publicar um SPS diferente, é aqui que aparece.
    fn conferir_parametros(&mut self, novos: &[u8], origem: &str) {
        match &self.parametros_de_referencia {
            None => {
                registro::linha(format!(
                    "conjunto de parâmetros de referência ({origem}): {} bytes — {}",
                    novos.len(),
                    hexa(novos)
                ));
                self.parametros_de_referencia = Some(novos.to_vec());
            }
            Some(referencia) if referencia.as_slice() == novos => {
                // Igual é o caso comum e sai calado: uma linha por IDR encheria o registro de
                // ruído. O número aparece uma vez, no fim, em `relato_de_recriacao`.
                self.parametros_iguais += 1;
            }
            Some(referencia) => {
                self.parametros_diferentes += 1;
                registro::linha(format!(
                    "ATENÇÃO: o conjunto de parâmetros MUDOU — referência {} bytes, agora {} \
                     bytes.\n  referência: {}\n  agora:      {}",
                    referencia.len(),
                    novos.len(),
                    hexa(referencia),
                    hexa(&novos),
                ));
                let (antes, depois) = (sps::resumir(referencia), sps::resumir(&novos));
                if let Some(a) = antes {
                    registro::linha(format!("  sps de referência: {}", a.linha()));
                }
                if let Some(d) = depois {
                    registro::linha(format!("  sps agora:         {}", d.linha()));
                }
            }
        }
    }

    /// O que a quinta porta custou e produziu. Números crus, para a bancada ler sem interpretar.
    pub fn relato_de_recriacao(&self) -> String {
        format!(
            "recriações={} dispensadas={} | trocas_a_quente={} sem_reserva={} \
             custo_de_troca_us={:?} montagem_de_fundo_ms={:?} | montagem_ms={:?} | \
             ate_o_idr_ms={:?} | desligamentos_sujos={} | parametros iguais={} diferentes={} | \
             piso={} adiadas={} espera_do_piso_ms={:?} intervalos_ms={:?}",
            self.recriacoes,
            self.recriacoes_dispensadas,
            self.trocas_a_quente,
            self.trocas_sem_reserva,
            primeiros_u64(&self.custo_de_troca_us),
            primeiros_u64(&self.montagem_de_fundo_ms),
            primeiros_u64(&self.custo_de_montagem_ms),
            primeiros_u64(&self.custo_ate_o_idr_ms),
            self.desligamentos_sujos,
            self.parametros_iguais,
            self.parametros_diferentes,
            match self.piso_entre_recriacoes {
                Some(p) => format!("{} ms", p.as_millis()),
                None => "sem piso".to_string(),
            },
            self.recriacoes_adiadas,
            primeiros_u64(&self.espera_do_piso_ms),
            primeiros_u64(&self.intervalos_entre_recriacoes_ms),
        )
    }

    /// O perfil do laço, com o contador do WGC lido no instante da chamada.
    pub fn perfil(&self) -> Perfil {
        let mut p = self.perfil;
        p.chegados_do_wgc = self.captura.chegados();
        p
    }

    pub fn recriacoes(&self) -> u64 {
        self.recriacoes
    }

    pub fn contagem_de_idrs(&self) -> u64 {
        self.contadores.idrs
    }

    /// A origem desta captura deixou de existir — o monitor foi desconectado.
    ///
    /// É a testemunha do **sistema**. `emissor.rs` cruza com uma segunda, independente: reenumerar
    /// os monitores e não achar mais o nome de dispositivo escolhido. Duas porque nenhuma das duas
    /// tinha sido medida nesta bancada, e concordância entre testemunhas independentes é o que
    /// separa "medi" de "presumi".
    pub fn fonte_sumiu(&self) -> bool {
        self.captura.item_fechado()
    }

    /// A origem é uma câmera?
    pub fn e_camera(&self) -> bool {
        self.captura.e_camera()
    }

    /// Como a câmera acabou (o leitor, um evento, o fluxo, ou o formato que mudou), quando acabou,
    /// e se foi desconexão. A câmera parada não é fim: [`Cadeia::camera_parada_ha`].
    pub fn fim_da_camera(&self) -> Option<crate::regras_da_camera::FimDaCamera> {
        self.captura.fim_da_camera()
    }

    /// A câmera está parada (sem quadro por 3 s), e há quanto tempo. `None` nas outras origens.
    pub fn camera_parada_ha(&self, agora: Instant) -> Option<Duration> {
        self.captura.camera_parada_ha(agora)
    }

    /// O que o SPS deste emissor de fato declara. `None` até o primeiro conjunto de parâmetros
    /// aparecer.
    pub fn resumo_sps(&self) -> Option<&ResumoSps> {
        self.resumo_sps.as_ref()
    }

    /// Pede um IDR ao encoder — best effort, e dito assim de propósito.
    ///
    /// O achado 7 mediu que `SetValue` devolve sucesso e o driver ignora. Chamamos mesmo assim (é
    /// grátis, e outro driver pode obedecer) e **anotamos o pedido** para depois comparar com onde
    /// os IDR realmente saíram. É a regra "verifique no fluxo, não no retorno da API" aplicada ao
    /// mesmo `S_OK` que já enganou este projeto uma vez.
    pub fn pedir_idr(&mut self) {
        self.idr_pendente = true;
        self.pedidos_de_idr_em.push(self.indice);
        let _ = encoder::force_next_keyframe(&self.enc);

        // A terceira porta. Ver `encoder::reiniciar_fluxo` para por que ela é perigosa.
        //
        // **O estado que o `FLUSH` invalida é limpo aqui, e só aqui.** O MFT descarta o que
        // estava dentro dele e volta a emitir `METransformNeedInput` do zero:
        //
        // - `creditos = 0` porque os créditos velhos deixaram de valer. Gastá-los submeteria
        //   quadros que o MFT não pediu;
        // - `submetidos` some porque são quadros que nunca vão sair — mantê-los faria a conta de
        //   latência casar entrada com saída errada, calada;
        // - `pendente` some porque é um quadro de antes do corte.
        //
        // Se o MFT não voltar a pedir entrada, o emissor **para**. Por isso isto fica atrás de
        // uma flag: a regra da casa é conferir no fluxo, e o fluxo aqui é medido pelo
        // espaçamento real dos IDR contra os pedidos — `idrs_em` contra `pedidos_de_idr_em`, que
        // já existiam justamente para esta pergunta.
        if self.idr_por_flush {
            match encoder::reiniciar_fluxo(&self.enc) {
                Ok(()) => {
                    self.contar_o_que_a_troca_leva();
                    if self.pendente.take().is_some() {
                        self.contadores.pendentes_perdidos_no_flush += 1;
                    }
                    self.creditos = 0;
                    self.submetidos.clear();
                    self.flushes += 1;
                }
                Err(e) => registro::linha(format!("aviso: reiniciar_fluxo falhou: {e}")),
            }
        }

        // A quinta porta. Ver `recriar_encoder` — ela é quem trata todo o estado.
        //
        // **Duas recusas, e as duas existem para o produto e não para a bancada.** Recriar custa
        // ~150 ms de laço parado; um receptor numa rede ruim pede quadro-chave em rajada, e uma
        // porta que obedecesse a cada pedido derrubaria o encoder várias vezes por segundo — o
        // mesmo formato de defeito que matou a porta 3, por outro caminho: *pedir passaria a
        // impedir de receber*.
        //
        // 1. **Já há uma recriação cujo IDR não saiu.** O quadro-chave que este pedido quer está a
        //    caminho; derrubar o encoder de novo o destruiria.
        // 2. **O encoder atual ainda não produziu quadro nenhum.** O primeiro quadro de um encoder
        //    recém-criado é IDR por construção — não há o que forçar.
        if self.idr_por_recriacao {
            if self.esperando_idr_desde.is_some() || self.encodados_no_encoder_atual == 0 {
                self.recriacoes_dispensadas += 1;
                return;
            }
            // A terceira recusa, e é a única que **guarda** o pedido em vez de descartá-lo: o
            // piso de intervalo entre recriações. Ver `Argumentos::piso_entre_recriacoes_ms`.
            //
            // Um pedido que chega dentro do piso não some — ele fica em `pedido_adiado` e
            // `drenar_pedido_adiado` o atende assim que o piso vence. É o que torna o piso um
            // limitador de taxa e não um sorteio: com `--sem-idr-por-recriacao` a recuperação
            // depende de um IDR que o MFT não entrega quando pedem, e um piso que descartasse
            // pedidos teria o mesmo sintoma em doses.
            if self.dentro_do_piso() {
                if !self.pedido_adiado {
                    self.pedido_adiado = true;
                    self.pedido_adiado_desde = Some(Instant::now());
                    self.recriacoes_adiadas += 1;
                }
                return;
            }
            self.recriar_agora();
        }
    }

    /// O piso ainda não venceu desde a última recriação?
    ///
    /// `None` em `ultima_recriacao` é a primeira recriação da sessão, e ela nunca é adiada: o
    /// piso mede intervalo entre duas, e não há uma anterior de que se afastar.
    fn dentro_do_piso(&self) -> bool {
        match (self.piso_entre_recriacoes, self.ultima_recriacao) {
            (Some(piso), Some(quando)) => quando.elapsed() < piso,
            _ => false,
        }
    }

    /// Recria, e registra o erro sem propagá-lo — os dois chamadores (`pedir_idr` e
    /// `drenar_pedido_adiado`) tratam a falha do mesmo jeito, e ela não tem volta: o encoder
    /// velho já foi desligado antes de o novo falhar.
    fn recriar_agora(&mut self) {
        self.pedido_adiado = false;
        if let Some(desde) = self.pedido_adiado_desde.take() {
            self.espera_do_piso_ms
                .push(desde.elapsed().as_millis() as u64);
        }
        if let Err(e) = self.recriar_encoder() {
            registro::linha(format!(
                "ERRO: a recriação do encoder falhou ({e}). A cadeia ficou sem encoder novo e \
                 o velho já foi desligado: esta sessão acabou."
            ));
            // Até aqui a frase acima era só frase: a sessão seguia viva, sem encoder, e cada
            // quadro seguinte escrevia uma linha de recusa. Quem lê `motivo_de_morte` (o emissor
            // com várias sessões) encerra a sessão; o caminho de uma sessão só não o lê ainda.
            self.morta = Some(format!("a recriação do encoder falhou: {e}"));
        }
    }

    /// Por que esta cadeia não produz mais nada, quando não produz: a recriação do encoder falhou,
    /// ou o dispositivo D3D11 caiu (`GetDeviceRemovedReason`, que o emissor não lia — só o
    /// receptor, em `exibicao.rs`). `None` é "viva".
    pub fn motivo_de_morte(&self) -> Option<String> {
        if let Some(m) = &self.morta {
            return Some(m.clone());
        }
        device::motivo_da_queda(&self.dispositivo)
            .map(|m| format!("o dispositivo D3D11 caiu: {}", device::nome_do_motivo(m)))
    }

    /// **Tudo desligado**, e não só a captura: o encoder em serviço, a reserva colhida, e a
    /// oficina fechada com prazo — com as reservas que ela terminou depois do último `colher`.
    ///
    /// `fechar` (o de sempre) só para a captura, e o encoder em serviço e a reserva ficam vivos
    /// até o processo sair: o bombeador de eventos parado em `GetEvent`, o transform e o
    /// dispositivo presos (revisão adversarial de 13/09/2026). Numa sessão só por processo isso é
    /// um vazamento por sessão; com oito sessões que entram e saem, é um encoder de hardware
    /// sobrando a cada saída. O emissor com várias sessões chama isto; o de uma sessão, `fechar`.
    ///
    /// Com o tempo de cada etapa: no R4 (M51) a sessão ficou 16,9 s entre os contadores da câmera e
    /// esta linha, e só o diário da fonte disse onde.
    pub fn desligar_tudo(mut self, prazo_da_oficina: Duration) -> String {
        let t0 = Instant::now();
        self.captura.stop();
        let t_captura = t0.elapsed();
        let t1 = Instant::now();
        let sobras = match self.oficina.take() {
            Some(of) => of.fechar_e_esperar(prazo_da_oficina),
            None => Vec::new(),
        };
        let t_oficina = t1.elapsed();
        let t2 = Instant::now();
        let (mut limpos, mut sujos) = (0u32, 0u32);
        for r in sobras.into_iter().chain(self.reserva.take()) {
            if encoder::desligar(&r.enc) { limpos += 1 } else { sujos += 1 }
        }
        if encoder::desligar(&self.enc) { limpos += 1 } else { sujos += 1 }
        format!(
            "encoders desligados: {limpos} limpos, {sujos} sem IMFShutdown | captura {} ms, oficina {} ms, encoders {} ms",
            t_captura.as_millis(),
            t_oficina.as_millis(),
            t2.elapsed().as_millis()
        )
    }

    /// Guarda a textura de um quadro que vai entrar no encoder, **como nossa**, para a repetição.
    fn guardar_para_repetir(&mut self, textura: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D) {
        use windows::Win32::Graphics::Direct3D11::{
            ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
            D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
        };
        if self.captura.e_sintetica() {
            // A textura do anel já é nossa, e o anel só anda com quadro novo.
            self.ultima_para_repetir = Some(textura.clone());
            return;
        }
        unsafe {
            if self.copia_para_repetir.is_none() {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                textura.GetDesc(&mut desc);
                desc.Usage = D3D11_USAGE_DEFAULT;
                desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
                desc.CPUAccessFlags = 0;
                desc.MiscFlags = 0;
                let mut t: Option<ID3D11Texture2D> = None;
                if self.dispositivo.CreateTexture2D(&desc, None, Some(&mut t)).is_err() {
                    return;
                }
                self.copia_para_repetir = t;
            }
            let (Some(copia), Ok(ctx)) =
                (self.copia_para_repetir.as_ref(), self.dispositivo.GetImmediateContext())
            else {
                return;
            };
            ctx.CopyResource(copia, textura);
            self.ultima_para_repetir = Some(copia.clone());
        }
    }

    /// **A tela parada**, na tela estendida: sem quadro novo há `repetir_apos`, ou com um IDR
    /// pedido esperando, o último quadro entra de novo. É o `repetirSeParado` do Mac
    /// (`TransmissaoAoVivo.swift`): um P que custa quase nada, ou o IDR pedido, na hora — e o
    /// receptor não fica sem quadro (o Android desiste em 10 s) nem sem o IDR que pediu.
    fn talvez_repetir(&mut self) {
        if self.captura.e_camera() {
            self.talvez_repetir_a_camera();
            return;
        }
        let Some(p) = self.padroes else {
            return;
        };
        if self.pendente.is_some() || self.creditos == 0 {
            return;
        }
        let (Some(ultima), Some(desde)) = (self.ultima_para_repetir.clone(), self.ultima_submissao)
        else {
            return;
        };
        let parada = desde.elapsed() >= p.repetir_apos;
        // Com IDR pedido, na hora — mas não em rajada enquanto o IDR atravessa o encoder, e não
        // enquanto o piso segura a recriação (o quadro sairia P pelo encoder velho).
        let idr = self.idr_pendente
            && !self.pedido_adiado
            && desde.elapsed() >= Duration::from_millis(33);
        if !(parada || idr) {
            return;
        }
        // O carimbo é o de **agora**: um carimbo repetido faria o encoder e o RTP andarem para trás
        // (a mesma nota do Mac).
        self.pendente = Some(CapturedFrame { texture: ultima, captured_at: Instant::now(), posse: None });
        self.proximo_e_repeticao = true;
        self.contadores.repeticoes_postas += 1;
    }

    /// **A câmera parada** (a decisão do Bruno de 21/09: a câmera que pausa não encerra a sessão):
    /// sem quadro real há `REPETIR_A_CAMERA_PARADA_APOS` (400 ms), o último quadro entra de novo a
    /// cada `INTERVALO_DA_REPETICAO_DA_CAMERA` (100 ms), carimbado com "agora". É o que segura
    /// Android, macOS e iOS, que desistem com 10 s sem quadro de vídeo, e o anel de reordenação do
    /// núcleo depois de uma perda na borda da pausa (`regras_da_camera`, os dois números).
    ///
    /// Diferente da tela estendida: **sem cópia** (o quadro é a posição do anel da captura que ainda
    /// está intacta, `CapturaDeCamera::quadro_para_repetir`), e **só com a câmera parada** — nunca
    /// entre quadros reais de uma câmera lenta, nem para servir um IDR com ela andando (a revisão
    /// adversarial da crítica, 7). Um IDR pedido com ela parada sai na repetição seguinte. O M1 da
    /// fase 3 (a repetição "agora" e o quadro real mais velho logo depois) é o
    /// `carimbo_da_submissao`: o real é empurrado alguns ms e contado.
    fn talvez_repetir_a_camera(&mut self) {
        if self.pendente.is_some() || self.creditos == 0 {
            return;
        }
        // Com a caixa única, um quadro real na caixa passa na frente.
        if self.caixa_unica && self.captura.espiar_instante().is_some() {
            return;
        }
        let agora = Instant::now();
        let ultima = match (self.ultima_submissao, self.ultima_repeticao_da_camera) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if !crate::regras_da_camera::repetir_a_camera(self.captura.ultimo_quadro(), ultima, agora) {
            return;
        }
        self.ultima_repeticao_da_camera = Some(agora);
        let Some((textura, posse)) = self.captura.quadro_da_camera_para_repetir() else {
            self.contadores.repeticoes_puladas += 1;
            return;
        };
        self.pendente = Some(CapturedFrame { texture: textura, captured_at: agora, posse });
        self.proximo_e_repeticao = true;
        self.contadores.repeticoes_postas += 1;
    }

    /// Atende o pedido que o piso guardou, assim que ele puder ser atendido.
    ///
    /// Chamada a cada volta do laço, e por isso a primeira coisa que ela faz é a barata: sem
    /// pedido guardado, sai. As duas condições de `pedir_idr` são reconferidas aqui porque o
    /// mundo mudou desde que o pedido chegou — no meio da espera do piso pode ter havido uma
    /// recriação por outro caminho, e o quadro-chave que este pedido quer já estar a caminho.
    fn drenar_pedido_adiado(&mut self) {
        if !self.pedido_adiado {
            return;
        }
        if self.esperando_idr_desde.is_some() || self.encodados_no_encoder_atual == 0 {
            return;
        }
        if self.dentro_do_piso() {
            return;
        }
        self.recriar_agora();
    }

    /// O que uma troca de encoder (quinta porta, recriação ou `FLUSH`) leva junto.
    ///
    /// Os três caminhos zeram `creditos` e esvaziam `submetidos` pela mesma razão — pertenciam a
    /// um transform que saiu de serviço. O que faltava era **contar**: um crédito zerado é um
    /// pedido do MFT que nunca virou quadro, e uma entrada em `submetidos` é um quadro que já
    /// atravessou a escala e o `ProcessInput` e nunca vai sair. Sem estes dois números a conta de
    /// `capturados` não fecha numa sessão com recriação, e "não fecha" era indistinguível de
    /// "tem um caminho de descarte que ninguém achou".
    ///
    /// Chamada **antes** de `creditos = 0` e de `submetidos.clear()`, sempre.
    fn contar_o_que_a_troca_leva(&mut self) {
        self.contadores.creditos_perdidos_na_troca += u64::from(self.creditos);
        self.contadores.em_voo_perdidos_na_troca += self.submetidos.len() as u64;
        self.entrada_aceita_em = None;
    }

    /// Quantos quadros estão dentro do MFT agora, esperando saída.
    pub fn em_voo(&self) -> u64 {
        self.submetidos.len() as u64
    }

    /// Há quadro esperando crédito na entrada do encoder agora?
    pub fn pendente_agora(&self) -> u64 {
        u64::from(self.pendente.is_some())
    }

    /// A linha dos degraus, com o estado vivo já preenchido. Ver
    /// [`Contadores::linha_dos_degraus`].
    pub fn linha_dos_degraus(&self) -> String {
        self.contadores
            .linha_dos_degraus(self.pendente_agora(), self.em_voo())
    }

    /// Uma volta do laço, com orçamento de tempo. Devolve os quadros prontos para a track.
    pub fn bombear(&mut self, orcamento: Duration) -> Vec<Quadro> {
        let mut saida = Vec::new();
        let fim = Instant::now() + orcamento;
        self.perfil.voltas += 1;

        // Antes de qualquer coisa: o pedido que o piso guardou já pode ser atendido? Aqui, e não
        // em `pedir_idr`, porque quem faz o tempo passar é o laço — `pedir_idr` só é chamada
        // quando o receptor pede, e um pedido adiado que esperasse o pedido seguinte para ser
        // atendido não seria um piso, seria uma fila.
        // A reserva que a oficina terminou de montar entra em serviço aqui — antes do pedido
        // adiado, para que um pedido que vença o piso nesta mesma volta já ache reserva pronta.
        self.colher_reserva();
        self.drenar_pedido_adiado();

        // Clonado: o `take_frame` da origem sintética pede `&mut self.captura` dentro do braço.
        let avisos = self.captura.avisos().clone();
        select! {
            recv(avisos) -> _ => {
                self.perfil.ramo_captura += 1;
                // **Caixa única.** O aviso é consumido do mesmo jeito (senão o `select!` giraria
                // em falso sobre um canal sempre pronto), mas o quadro fica onde está: quem o
                // tira é `talvez_submeter`, na hora em que há crédito. O perfil de intervalo de
                // captura continua saindo, agora espiando o slot em vez de esvaziá-lo.
                if self.caixa_unica {
                    if let Some(agora) = self.captura.espiar_instante() {
                        if let Some(anterior) = self.ultimo_capturado {
                            if agora > anterior {
                                let d = (agora - anterior).as_micros() as u64;
                                self.perfil.intervalo_de_captura_us += d;
                                self.perfil.n_intervalos += 1;
                                self.perfil.intervalo_maximo_us =
                                    self.perfil.intervalo_maximo_us.max(d);
                            }
                        }
                        self.ultimo_capturado = Some(agora);
                    } else {
                        self.perfil.voltas_sem_quadro += 1;
                    }
                } else if let Some(quadro) = self.captura.take_frame() {
                    // Caixa postal de uma posição: o quadro velho é descartado, não enfileirado.
                    // "Zero filas" vale mais que "nenhum quadro perdido" num espelhamento ao vivo.
                    if let Some(anterior) = self.ultimo_capturado {
                        let d = (quadro.captured_at - anterior).as_micros() as u64;
                        self.perfil.intervalo_de_captura_us += d;
                        self.perfil.n_intervalos += 1;
                        self.perfil.intervalo_maximo_us = self.perfil.intervalo_maximo_us.max(d);
                    }
                    self.ultimo_capturado = Some(quadro.captured_at);
                    // **O degrau que não tinha contador.** A caixa postal de uma posição
                    // sobrescreve, e sobrescrever é a política certa — o que atravessa tem de ser
                    // o quadro mais novo. Mas até 31/08 ela sobrescrevia em silêncio, e a corrida
                    // que mediu `capturados=1349 encodados=749` não tinha como dizer que os 600
                    // que faltavam tinham morrido exatamente aqui.
                    if self.pendente.is_some() && !self.proximo_e_repeticao {
                        self.contadores.sobrescritos_antes_de_submeter += 1;
                    } else if self.pendente.is_some() {
                        self.contadores.repeticoes_sobrescritas += 1;
                    }
                    self.pendente = Some(quadro);
                    self.proximo_e_repeticao = false;
                    self.contadores.capturados += 1;
                } else {
                    self.perfil.voltas_sem_quadro += 1;
                }
            }
            recv(self.eventos) -> msg => {
                self.perfil.ramo_evento += 1;
                self.tratar_evento(msg.ok(), &mut saida);
            }
            default(Duration::from_millis(8)) => self.perfil.ramo_ocioso += 1,
        }

        // A tela parada (só na tela estendida; sem padrões, não faz nada).
        self.talvez_repetir();

        let submeteu = self.talvez_submeter();

        // **Esperar a saída, não pegá-la na volta seguinte.** A frente da câmera virtual mediu o
        // custo de não fazer isto: `encode_ms` deu 33,4 ms de mediana — exatamente um intervalo de
        // quadro — porque o quadro ficava pronto em milissegundos e só era enviado uma volta
        // depois. É latência da régua, não do sistema.
        if submeteu {
            let comeco_da_espera = Instant::now();
            let prazo = fim.min(comeco_da_espera + Duration::from_millis(25));
            let antes = self.contadores.encodados;
            while self.contadores.encodados == antes && Instant::now() < prazo {
                let msg = self.eventos.recv_timeout(Duration::from_millis(4)).ok();
                self.tratar_evento(msg, &mut saida);
            }
            self.perfil.espera_us += comeco_da_espera.elapsed().as_micros() as u64;
            self.perfil.n_espera += 1;
            if self.contadores.encodados == antes {
                self.perfil.esperas_estouradas += 1;
            }
        }

        // O que sobrou, sem esperar.
        while let Ok(evento) = self.eventos.try_recv() {
            self.tratar_evento(Some(evento), &mut saida);
        }

        // **A rajada de saída, medida onde ela existe.** Tudo o que está neste vetor sai para o
        // pacotizador numa sequência sem pausa em `emissor.rs`, então é a **soma** que se compara
        // com o penhasco de 40–80 pacotes desta LAN — não o tamanho de um quadro.
        if !saida.is_empty() {
            let pacotes: u64 = saida.iter().map(|q| q.pacotes).sum();
            self.faixas_de_rajada_de_saida[faixa_de_rajada(pacotes)] += 1;
            self.max_pacotes_por_volta = self.max_pacotes_por_volta.max(pacotes);
            self.voltas_com_saida += 1;
            self.soma_quadros_por_volta += saida.len() as u64;
        }
        saida
    }

    fn tratar_evento(&mut self, evento: Option<MftEvent>, saida: &mut Vec<Quadro>) {
        match evento {
            Some(MftEvent::NeedInput) => {
                self.creditos += 1;
                self.contadores.creditos_recebidos += 1;
                self.perfil.maximo_de_creditos =
                    self.perfil.maximo_de_creditos.max(u64::from(self.creditos));
                // O intervalo entre o MFT aceitar um quadro e pedir o próximo. Só o **primeiro**
                // crédito depois de cada `ProcessInput` conta: o segundo mediria o intervalo
                // errado, e a média ficaria mais curta do que o pipeline de fato é.
                if let Some(desde) = self.entrada_aceita_em.take() {
                    let us = desde.elapsed().as_micros() as u64;
                    self.perfil.ate_o_credito_seguinte_us += us;
                    self.perfil.n_ate_o_credito_seguinte += 1;
                    self.perfil.ate_o_credito_seguinte_maximo_us =
                        self.perfil.ate_o_credito_seguinte_maximo_us.max(us);
                }
            }
            Some(MftEvent::HaveOutput) => self.drenar(saida),
            _ => {}
        }
    }

    fn talvez_submeter(&mut self) -> bool {
        if self.creditos == 0 {
            // Um quadro à espera e nenhum crédito: ele **vai** ser descartado se a captura
            // entregar outro antes de o MFT pedir entrada. Contar as duas voltas — esta e a
            // simétrica, crédito sobrando sem quadro — é o que separa "o encoder não pede" de
            // "o laço não tem o que dar".
            let ha_quadro = self.pendente.is_some()
                || (self.caixa_unica && self.captura.espiar_instante().is_some());
            if ha_quadro {
                self.contadores.voltas_sem_credito += 1;
            }
            return false;
        }
        // **Caixa única: o quadro é buscado aqui, com o crédito já na mão.** No caminho de duas
        // caixas ele foi tirado do slot do WGC lá no `select!` e esperou em `pendente` até este
        // ponto.
        //
        // A previsão era que isso valesse ~11 ms de latência. **O A/B de 31/08 derrubou a
        // previsão**: 26,52 ms contra 27,00 ms, com 1,04 ms de espalhamento dentro do próprio
        // braço de produto. O motivo, depois de medido: o laço consome o aviso de `frame_ready`
        // prontamente, então `pendente` e o slot do WGC quase sempre contêm o **mesmo** quadro.
        // Ver `docs/quadros-que-nao-saem.md` §6.
        if self.caixa_unica && self.pendente.is_none() {
            if let Some(quadro) = self.captura.take_frame() {
                self.pendente = Some(quadro);
                self.proximo_e_repeticao = false;
                self.contadores.capturados += 1;
            }
        }
        if self.pendente.is_none() {
            self.contadores.voltas_com_credito_sem_quadro += 1;
            return false;
        }
        // **A câmera com conversor: um destino livre** (a revisão do código da fase 3, m6). A
        // conversão é aqui, na submissão, então os destinos do conversor são todos do MFT: com
        // `destinos − 1` quadros em voo, o próximo `Blt` escreveria num destino que o MFT pode
        // estar lendo. O quadro fica em `pendente` (e o mais novo o sobrescreve), como na falta de
        // crédito.
        if let Some(c) = &self.conversor {
            if self.submetidos.len() + 1 >= c.destinos() {
                self.contadores.conversor_sem_destino += 1;
                let agora = Instant::now();
                let desde = *self.conversor_esperando_desde.get_or_insert(agora);
                // **O prazo** (a reconferência da fase 3): um MFT que engole entradas sem devolver
                // saída travaria a câmera com conversor para sempre, sem fim de sessão. Passado o
                // prazo, a captura acaba com o motivo, e o vigia encerra a sessão com ele.
                if crate::regras_da_camera::conversor_travado(Some(desde), agora) {
                    self.captura.encerrar_camera(format!(
                        "o encoder segurou {} quadros da câmera sem devolver nenhum por {} s",
                        self.submetidos.len(),
                        crate::regras_da_camera::PRAZO_DO_CONVERSOR_CHEIO.as_secs()
                    ));
                }
                return false;
            }
            self.conversor_esperando_desde = None;
        }
        // **A porta de taxa de entrega, e ela é conferida antes do `take`.**
        //
        // Recusar sem tirar o quadro da caixa postal é o que mantém a semântica de caixa postal de
        // uma posição: o quadro recusado continua lá e é **sobrescrito** pelo próximo que a
        // captura entregar, de modo que quem atravessa é sempre o mais novo. Tirar e jogar fora
        // daria o mesmo número de quadros por segundo e entregaria imagem mais velha.
        if let Some(intervalo) = self.intervalo_de_entrega {
            let Some(quadro) = self.pendente.as_ref() else {
                return false;
            };
            let agora = quadro.captured_at;
            let devido = match self.proxima_entrega {
                None => agora,
                Some(d) => d,
            };
            if agora < devido {
                self.contadores.recusas_do_ritmo += 1;
                return false;
            }
            // Reancorar quando o atraso passou de um intervalo inteiro. Sem isto, uma tela parada
            // — que o WGC representa **não entregando quadro** — deixaria `proxima_entrega` no
            // passado, e na volta da tela o laço entregaria todos os quadros devidos de uma vez.
            // Uma rajada de recuperação é precisamente a doença que esta frente mede; a porta não
            // pode fabricá-la.
            self.proxima_entrega = Some(if agora > devido + intervalo {
                agora + intervalo
            } else {
                devido + intervalo
            });
        }
        let Some(quadro) = self.pendente.take() else {
            return false;
        };
        let repeticao = std::mem::take(&mut self.proximo_e_repeticao);
        // Só a tela estendida guarda (copia) o quadro para repetir; a câmera parada repete a posição
        // do anel sem cópia (`talvez_repetir_a_camera`), e o caminho de sempre não copia nada.
        if self.padroes.is_some() && !repeticao {
            self.guardar_para_repetir(&quadro.texture);
        }
        // **O carimbo nunca volta** (a revisão do código da fase 3, M1): para toda origem, o `t` do
        // MFT e o `timestamp_us` do fio (que sai de `submetidos`) andam para a frente, mesmo que a
        // cadeia tenha posto uma repetição carimbada com "agora" antes de um quadro mais velho.
        let (capturado_em, empurrado) =
            crate::regras_da_camera::carimbo_da_submissao(self.ultimo_carimbo_submetido, quadro.captured_at);
        if empurrado {
            self.contadores.carimbos_empurrados += 1;
        }
        let t = (capturado_em - self.inicio).as_nanos() as i64 / 100;

        // O teto é aplicado **aqui**, no caminho do quadro, e não só na configuração do encoder.
        // Configurar o MFT para 1274x716 e continuar entregando texturas de 1920x1080 seria
        // declarar um teto e não cumpri-lo — a mesma distância entre promessa e artefato que
        // este projeto vem pagando desde o `S_OK` que mentia. Se o `Blt` falhar, o quadro é
        // perdido e contado; nunca segue no tamanho errado.
        //
        // **O aspecto da câmera mudou com a sessão no ar** (a fase 5: o DV 16:9 ↔ 4:3): o encoder
        // fica no tamanho da abertura, e o quadro passa a entrar encaixado, com faixas pretas.
        // Só com quadro real: o repetido é da imagem de antes da troca, e sairia encaixado no
        // aspecto novo (a revisão adversarial da crítica de 22/09, 6).
        let aspecto_da_camera = match (&self.captura, repeticao) {
            (Captura::Camera(c), false) => Some((c.trocas_de_aspecto(), c.tamanho_exibido())),
            (Captura::DoDono(l), false) => Some((l.trocas_de_aspecto(), l.tamanho_exibido())),
            _ => None,
        };
        if let Some((trocas, exibida)) = aspecto_da_camera {
            if trocas != self.trocas_de_aspecto_vistas {
                match &self.conversor {
                    Some(conv) => {
                        let r = conv.encaixar(exibida);
                        registro::linha(format!(
                            "câmera: o aspecto mudou; o quadro {}x{} entra encaixado em {r:?} (x, y, l, a) na saída {}x{}",
                            exibida.0, exibida.1, conv.largura, conv.altura
                        ));
                    }
                    None => registro::linha(format!(
                        "câmera: o aspecto mudou para {}x{}, e esta câmera não passa pelo conversor: segue como veio",
                        exibida.0, exibida.1
                    )),
                }
                self.trocas_de_aspecto_vistas = trocas;
            }
        }
        let comeco_da_escala = Instant::now();
        let textura = match (&self.escalador, &self.conversor) {
            (_, Some(c)) => match c.converter(&quadro.texture) {
                Ok(convertida) => {
                    self.perfil.escala_us += comeco_da_escala.elapsed().as_micros() as u64;
                    self.perfil.n_escala += 1;
                    convertida
                }
                Err(erro) => {
                    self.contadores.recusados_pelo_encoder += 1;
                    self.contadores.falhas_de_escala += 1;
                    registro::linha(format!("aviso: não consegui converter o quadro da câmera: {erro}"));
                    return false;
                }
            },
            (None, None) => quadro.texture.clone(),
            (Some(e), None) => match e.escalar(&quadro.texture) {
                Ok(reduzida) => {
                    self.perfil.escala_us += comeco_da_escala.elapsed().as_micros() as u64;
                    self.perfil.n_escala += 1;
                    reduzida
                }
                Err(erro) => {
                    self.contadores.recusados_pelo_encoder += 1;
                    self.contadores.falhas_de_escala += 1;
                    registro::linha(format!("aviso: não consegui reduzir o quadro ao teto: {erro}"));
                    return false;
                }
            },
        };

        let amostra =
            // Subrecurso 0: a textura do WGC, a cópia do monitor virtual, a sintética, a saída do
            // escalador e a do anel e do conversor da câmera são todas de uma fatia só (a câmera
            // copia **a fatia certa** do leitor na chegada, `captura_de_camera.rs`).
            match encoder::sample_from_texture(&textura, 0, t, self.duracao_100ns) {
                Ok(a) => a,
                Err(e) => {
                    // Este caminho perdia quadro **sem incrementar contador nenhum** até 31/08:
                    // o único rastro era esta linha de registro, e nada no relatório final.
                    self.contadores.recusados_pelo_encoder += 1;
                    self.contadores.falhas_de_empacotamento += 1;
                    registro::linha(format!("aviso: não empacotei o quadro como amostra: {e}"));
                    return false;
                }
            };
        let comeco_da_entrada = Instant::now();
        match unsafe { self.enc.transform.ProcessInput(0, &amostra, 0) } {
            Ok(()) => {
                self.perfil.entrada_us += comeco_da_entrada.elapsed().as_micros() as u64;
                self.perfil.n_entrada += 1;
                self.submetidos.push_back((self.indice, capturado_em));
                self.ultimo_carimbo_submetido = Some(capturado_em);
                self.indice += 1;
                self.creditos -= 1;
                self.contadores.entregues_ao_mft += 1;
                self.entrada_aceita_em = Some(Instant::now());
                self.ultima_submissao = Some(Instant::now());
                if repeticao {
                    self.contadores.repetidos += 1;
                }
                true
            }
            Err(e) => {
                self.contadores.recusados_pelo_encoder += 1;
                self.contadores.recusas_do_process_input += 1;
                registro::linha(format!("aviso: ProcessInput recusou um quadro: {e}"));
                false
            }
        }
    }

    fn drenar(&mut self, saida: &mut Vec<Quadro>) {
        let comeco = Instant::now();
        let brutos = match encoder::drain_output(&self.enc.transform, encoder::OUTPUT_STREAM_ID) {
            Ok(b) => b,
            Err(e) => {
                registro::linha(format!("aviso: drain_output falhou: {e}"));
                return;
            }
        };
        self.perfil.drenagem_us += comeco.elapsed().as_micros() as u64;
        self.perfil.n_drenagem += 1;
        for bruto in brutos {
            // Uma saída sem nenhuma fatia de imagem é conjunto de parâmetros solto, não quadro.
            // Mandá-la pela track a contaria como quadro e faria o receptor esperar imagem de uma
            // coisa que não tem imagem.
            if !tem_fatia(&bruto.bytes) {
                self.contadores.saidas_so_de_parametros += 1;
                self.guardar_parametros(bruto.bytes);
                continue;
            }

            let agora = Instant::now();
            let (_numero, capturado_em) = match self.submetidos.pop_front() {
                Some(par) => par,
                // Sem correspondência na fila: sem B-frames a ordem é FIFO, então isto não deveria
                // acontecer. Se acontecer, o quadro segue (a imagem importa mais que a medida) com
                // a latência marcada como zero em vez de um número inventado.
                None => (0, agora),
            };

            let mut bytes = bruto.bytes;
            if bruto.is_idr {
                self.idr_pendente = false;
                self.contadores.idrs += 1;
                self.idrs_em.push(self.indice.saturating_sub(1));
                // **A medida que decide a quinta porta, e ela é aqui.** Não no retorno de
                // `recriar_encoder`, que só diz que a API aceitou: aqui, no primeiro NAL tipo 5
                // que o parser de `contains_idr_nal` reconheceu na saída do MFT novo. É a regra da
                // casa — verifique no fluxo, não no retorno da API — aplicada à porta que a
                // frente veio testar.
                if let Some(desde) = self.esperando_idr_desde.take() {
                    let ms = desde.elapsed().as_millis() as u64;
                    self.custo_ate_o_idr_ms.push(ms);
                    registro::linha(format!(
                        "quinta porta: IDR NO FLUXO {ms} ms depois da recriação nº {} \
                         (quadro {})",
                        self.recriacoes,
                        self.indice.saturating_sub(1),
                    ));
                }
                bytes = self.garantir_parametros(bytes);
                // **A pergunta "o conjunto de parâmetros muda quando o encoder é recriado?"
                // respondida onde ela importa: no fio.** Não no blob do MFT, que este encoder nem
                // chega a publicar quando o IDR já leva SPS e PPS dentro.
                if let Some(p) = parametros_do_quadro(&bytes) {
                    self.conferir_parametros(&p, "SPS+PPS do IDR");
                }
            }
            if self.resumo_sps.is_none() {
                self.resumo_sps = sps::resumir(&bytes);
                if let Some(r) = &self.resumo_sps {
                    registro::linha(format!("sps do emissor — {}", r.linha()));
                    if !r.declara_a_restricao() {
                        registro::linha(
                            "ATENÇÃO: este SPS não declara bitstream_restriction. O decodificador \
                             do outro lado é obrigado a assumir o teto do nível — medido no M4 em \
                             169,5 ms de p50 contra 0,55 ms, com 30 fps limpos nos dois casos.",
                        );
                    }
                }
            }

            // **O tamanho do quadro-chave, medido no artefato que vai para o fio.**
            //
            // `bytes` aqui já passou por `garantir_parametros`, ou seja, é exatamente a unidade de
            // acesso que `enviar_quadro` entrega à libdatachannel — não uma estimativa a partir do
            // bitrate pedido, nem o blob de parâmetros que este MFT sequer publica. É o número que
            // a conta de rajada precisa: o joelho desta LAN é de ~50 pacotes colados, e o único
            // objeto de uma sessão do Quall grande o bastante para atravessá-lo é o IDR.
            //
            // **A contagem de pacotes é feita aqui, sobre a unidade de acesso, e não depois a
            // partir dos bytes.** Medido em 29/08: `ceil(bytes/1188)` previa ~6 pacotes por
            // quadro não-IDR e o receptor contava 9,6. A regra de verdade precisa das fronteiras
            // de NAL, que só existem enquanto o buffer está na mão.
            let pacotes = quall_core::track::pacotes_da_unidade(&bytes);
            if bruto.is_idr {
                self.bytes_de_idr.push(bytes.len());
                self.pacotes_de_idr.push(pacotes);
            } else {
                self.soma_bytes_nao_idr += bytes.len() as u64;
                self.soma_pacotes_nao_idr += pacotes;
                self.n_nao_idr += 1;
                self.max_bytes_nao_idr = self.max_bytes_nao_idr.max(bytes.len() as u64);
                self.max_pacotes_nao_idr = self.max_pacotes_nao_idr.max(pacotes);
            }
            self.faixas_de_pacotes_por_quadro[faixa_de_rajada(pacotes)] += 1;

            // **O perfil por posição depois da recriação.** `encodados_no_encoder_atual` ainda não
            // foi incrementado, então ele é exatamente o índice 0-based deste quadro na vida do
            // encoder atual — 0 é o primeiro, que é o IDR que a quinta porta existe para produzir.
            let pos = (self.encodados_no_encoder_atual as usize).min(POSICOES_PERFILADAS);
            self.posicoes_apos_recriacao[pos].registrar(bytes.len() as u64, pacotes);

            let latencia_us = (agora - capturado_em).as_micros() as u64;
            self.contadores.encodados += 1;
            self.encodados_no_encoder_atual += 1;
            self.contadores.soma_latencia_us += latencia_us;
            saida.push(Quadro {
                timestamp_us: (capturado_em - self.inicio).as_micros() as u64,
                idr: bruto.is_idr,
                latencia_us,
                pacotes,
                bytes,
            });
        }
    }

    fn guardar_parametros(&mut self, bytes: Vec<u8>) {
        if self.parametros.is_none() && !bytes.is_empty() {
            registro::linha(format!(
                "conjunto de parâmetros veio como saída solta do encoder ({} bytes)",
                bytes.len()
            ));
            self.registrar_parametros(bytes);
        }
    }

    /// Garante que este IDR leva SPS e PPS junto, como o contrato do núcleo manda.
    fn garantir_parametros(&mut self, bytes: Vec<u8>) -> Vec<u8> {
        let ja_tem = QuadroCodificado {
            annexb: &bytes,
            timestamp_us: 0,
            idr: true,
        }
        .tem_parametros();
        if ja_tem {
            return bytes;
        }
        if self.parametros.is_none() {
            // O blob só costuma existir depois da renegociação de tipo de saída que segue o
            // primeiro `SetInputType` (achado 5) — por isso é relido, e não lido uma vez só.
            if let Some(p) = encoder::conjuntos_de_parametros(&self.enc) {
                registro::linha(format!("MF_MT_MPEG_SEQUENCE_HEADER lido do MFT: {} bytes", p.len()));
                self.registrar_parametros(p);
            }
        }
        let Some(parametros) = &self.parametros else {
            registro::linha(
                "ATENÇÃO: IDR sem SPS/PPS e o MFT ainda não publicou o conjunto de parâmetros. \
                 Um receptor que entrar agora não monta imagem.",
            );
            return bytes;
        };
        self.contadores.parametros_injetados += 1;
        let mut junto = Vec::with_capacity(parametros.len() + bytes.len());
        junto.extend_from_slice(parametros);
        junto.extend_from_slice(&bytes);
        junto
    }

    /// O que de fato aconteceu com o GOP — a conferência no fluxo que o `S_OK` não dá.
    /// Quantos pacotes RTP um quadro-chave ocupa **neste** enlace, hoje.
    ///
    /// A conta de pacotes é a regra do pacotizador da libdatachannel, e desde 29/08 ela é a
    /// regra **exata** ([`quall_core::track::pacotes_da_unidade`]), medida sobre a unidade de
    /// acesso enquanto ela ainda está na mão.
    ///
    /// Até essa data a conta era `ceil(bytes / MAX_FRAGMENTO)`, e ela subestimava em ~25 % por
    /// duas razões: a divisão é por **NAL** (AUD, SEI, SPS e PPS viram um pacote cada) e a
    /// `generateFragments` desconta os 2 bytes de cabeçalho FU-A **depois** de emparelhar os
    /// fragmentos, o que costuma acrescentar um. Medido no fio: 6 previstos contra 9,6 contados
    /// pelo receptor.
    ///
    /// O valor existe para ser comparado com **50**, o joelho de rajada desta LAN medido por
    /// `tools/rajada-udp.py` sem nada nosso no caminho.
    pub fn medida_de_quadro_chave(&self) -> String {
        if self.bytes_de_idr.is_empty() {
            return "nenhum IDR saiu".to_string();
        }
        let mut ordenados = self.bytes_de_idr.clone();
        ordenados.sort_unstable();
        let n = ordenados.len();
        let p50 = ordenados[n / 2];
        let menor = ordenados[0];
        let maior = ordenados[n - 1];
        let mut pac = self.pacotes_de_idr.clone();
        pac.sort_unstable();
        let media_nao_idr = if self.n_nao_idr == 0 {
            0
        } else {
            (self.soma_bytes_nao_idr / self.n_nao_idr) as usize
        };
        let pac_nao_idr = if self.n_nao_idr == 0 {
            0
        } else {
            self.soma_pacotes_nao_idr / self.n_nao_idr
        };
        format!(
            "n={n} | bytes p50={p50} min={menor} max={maior} | \
             pacotes RTP (regra exata do pacotizador): p50={} min={} max={} | \
             penhasco da LAN entre 40 e 80 pacotes | quadro não-IDR médio={media_nao_idr} B \
             ({pac_nao_idr} pacote(s)) MÁXIMO={} B ({} pacotes)",
            pac[pac.len() / 2],
            pac[0],
            pac[pac.len() - 1],
            self.max_bytes_nao_idr,
            self.max_pacotes_nao_idr,
        )
    }

    /// **A medida do controle de taxa reiniciado** — a última candidata de pé do laudo de 29/08.
    ///
    /// As outras duas caíram por medida: o **silêncio de ~150 ms** foi removido inteiro pela troca
    /// a quente e a perda não caiu (2,834 % contra 2,612 %); a **rajada do IDR** tem 19 a 20
    /// pacotes contra um penhasco entre 40 e 80. Sobra a rampa, e ela tem dois lados que são o
    /// mesmo fenômeno — a média derrubada (1,2 Mbps onde caberiam 3,7) e o pico dos primeiros
    /// quadros. Este relato olha os dois, e mais um terceiro que ninguém tinha olhado: a **rajada
    /// de saída**, que é a soma dos quadros que saem colados numa volta do laço.
    ///
    /// Tudo aqui é medido **no fio**, sobre a unidade de acesso que `enviar_quadro` entrega à
    /// libdatachannel, e com a regra **exata** do pacotizador — nunca sobre o que o encoder disse
    /// que ia fazer. É a regra da casa, e este MFT já a cobrou quatro vezes.
    pub fn medida_do_controle_de_taxa(&self) -> String {
        let media_bytes = if self.n_nao_idr == 0 {
            0
        } else {
            self.soma_bytes_nao_idr / self.n_nao_idr
        };
        let media_pac = if self.n_nao_idr == 0 {
            0
        } else {
            self.soma_pacotes_nao_idr * 100 / self.n_nao_idr
        };
        let mut linhas = vec![format!(
            "não-IDR: n={} bytes média={} MÁXIMO={} | pacotes média={:.2} MÁXIMO={}",
            self.n_nao_idr,
            media_bytes,
            self.max_bytes_nao_idr,
            media_pac as f64 / 100.0,
            self.max_pacotes_nao_idr,
        )];
        linhas.push(format!(
            "faixas de pacotes por quadro (penhasco da LAN entre 40 e 80): {}",
            faixas_em_texto(&self.faixas_de_pacotes_por_quadro)
        ));
        linhas.push(format!(
            "rajada de SAÍDA (quadros colados numa volta do laço): {} | máximo={} pacotes | \
             voltas com saída={} quadros/volta={:.3}",
            faixas_em_texto(&self.faixas_de_rajada_de_saida),
            self.max_pacotes_por_volta,
            self.voltas_com_saida,
            if self.voltas_com_saida == 0 {
                0.0
            } else {
                self.soma_quadros_por_volta as f64 / self.voltas_com_saida as f64
            },
        ));
        for (i, p) in self.posicoes_apos_recriacao.iter().enumerate() {
            if p.n == 0 {
                continue;
            }
            let rotulo = if i == POSICOES_PERFILADAS {
                format!("pos>={POSICOES_PERFILADAS}")
            } else {
                format!("pos={i}")
            };
            linhas.push(format!(
                "  {rotulo} n={} bytes média={} max={} | pacotes média={:.2} max={}",
                p.n,
                p.media_bytes(),
                p.max_bytes,
                p.media_pacotes_centesimos() as f64 / 100.0,
                p.max_pacotes,
            ));
        }
        linhas.join("\n")
    }

    pub fn medida_de_idr(&self) -> String {
        if self.idrs_em.is_empty() {
            return "nenhum IDR saiu".to_string();
        }
        let espacos: Vec<u64> = self
            .idrs_em
            .windows(2)
            .map(|par| par[1].saturating_sub(par[0]))
            .collect();
        let medio = if espacos.is_empty() {
            0
        } else {
            espacos.iter().sum::<u64>() / espacos.len() as u64
        };
        format!(
            "gop pedido={} quadros | idrs em {:?} | espaçamentos {:?} (médio {medio}) | \
             pedidos de IDR em {:?} | flushes={} ({}) | recriações={} ({})",
            self.gop_frames,
            primeiros(&self.idrs_em),
            primeiros(&espacos),
            primeiros(&self.pedidos_de_idr_em),
            self.flushes,
            if self.idr_por_flush { "porta 3 LIGADA" } else { "porta 3 desligada" },
            self.recriacoes,
            if self.idr_por_recriacao { "porta 5 LIGADA" } else { "porta 5 desligada" },
        )
    }

    pub fn fechar(&mut self) {
        self.captura.stop();
    }
}

fn primeiros(v: &[u64]) -> Vec<u64> {
    v.iter().copied().take(12).collect()
}

fn primeiros_u64(v: &[u64]) -> Vec<u64> {
    v.iter().copied().take(24).collect()
}

/// O conjunto de parâmetros em hexadecimal, para a comparação antes/depois caber numa linha de
/// registro. SPS+PPS de baseline a 720p cabem em ~30 bytes; o corte em 64 é folga.
///
/// **Isto não é pixel.** SPS e PPS descrevem o formato do fluxo — resolução, nível, restrições de
/// reordenação. Não há amostra de imagem em nenhum dos dois, e é por isso que este é o único blob
/// do caminho de vídeo que pode ir para um registro de bancada.
fn hexa(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in bytes.iter().take(64) {
        s.push_str(&format!("{b:02x}"));
    }
    if bytes.len() > 64 {
        s.push_str("...");
    }
    s
}

/// Os NAL de conjunto de parâmetros (SPS, tipo 7; PPS, tipo 8) deste Annex-B, na ordem em que
/// aparecem, cada um com o start code de 4 bytes.
///
/// Normalizar o start code para 4 bytes é de propósito: o que se quer comparar é o **conteúdo** do
/// conjunto de parâmetros entre um encoder e o seguinte, e um start code de 3 bytes num e de 4 no
/// outro faria dois conjuntos idênticos aparecerem como diferentes. O emparelhamento de bytes
/// depois disso é exato.
///
/// `None` quando não há nenhum dos dois — um quadro P, por exemplo.
fn parametros_do_quadro(annexb: &[u8]) -> Option<Vec<u8>> {
    let mut saida: Vec<u8> = Vec::new();
    for (tipo, corpo) in nals(annexb) {
        if tipo == 7 || tipo == 8 {
            saida.extend_from_slice(&[0, 0, 0, 1]);
            saida.extend_from_slice(corpo);
        }
    }
    if saida.is_empty() {
        None
    } else {
        Some(saida)
    }
}

/// Percorre um Annex-B e devolve `(tipo, corpo)` de cada NAL, sem o start code.
fn nals(annexb: &[u8]) -> Vec<(u8, &[u8])> {
    // Onde cada NAL começa (índice do primeiro byte DEPOIS do start code).
    let mut inicios: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            inicios.push(i + 3);
            i += 3;
        } else if i + 4 <= annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            inicios.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut saida = Vec::with_capacity(inicios.len());
    for (n, &comeco) in inicios.iter().enumerate() {
        if comeco >= annexb.len() {
            continue;
        }
        // O fim é o começo do start code seguinte. Achá-lo pelo início do próximo NAL menos o
        // tamanho do start code dele exigiria guardar esse tamanho; recuar sobre os zeros do
        // final dá o mesmo resultado sem guardar nada.
        let fim = match inicios.get(n + 1) {
            Some(&proximo) => {
                let mut f = proximo.saturating_sub(3);
                while f > comeco && annexb[f - 1] == 0 {
                    f -= 1;
                }
                f
            }
            None => annexb.len(),
        };
        saida.push((annexb[comeco] & 0x1f, &annexb[comeco..fim]));
    }
    saida
}

/// Este fluxo Annex-B tem alguma NAL de fatia (tipo 1 ou 5)?
fn tem_fatia(annexb: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 3 < annexb.len() {
        let salto = if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            3
        } else if i + 4 < annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            4
        } else {
            i += 1;
            continue;
        };
        let tipo = annexb[i + salto] & 0x1f;
        if tipo == 1 || tipo == 5 {
            return true;
        }
        i += salto;
    }
    false
}

// =================================================================================================
// A entrada do monitor virtual (`docs/monitor-virtual-windows.md` §14)
// =================================================================================================
//
// **Ao lado de `abrir`/`abrir_com`, que não mudam**: o caminho de uma sessão só continua passando
// por eles, linha a linha. Esta entrada recebe o encoder, o dispositivo e a captura já abertos — na
// ordem do monitor virtual: placa do processo → encoder daquela placa → dispositivo daquele LUID →
// origem preta; a captura do monitor entra depois, por `trocar_captura` — e monta o resto. A cauda (teto, escalador, configuração do encoder, oficina) é a de `abrir_com`,
// copiada: mudou uma, mude a outra (o compilador cobra os campos da `Cadeia`, não a lógica).

impl Cadeia {
    /// Monta a cadeia do monitor virtual no dispositivo `adaptador`, com o encoder `enc` da mesma
    /// placa, sobre a `captura` do começo — a origem preta do tamanho do pedido, enquanto o monitor
    /// nasce na fila do dono; a captura de tela entra depois, por [`Cadeia::trocar_captura`]. **Todo
    /// caminho de erro desliga o MFT**: o `ChosenEncoder` não tem `Drop`, e um `?` no meio o
    /// largaria ativado (a revisão, item 8).
    pub fn abrir_no_monitor_virtual(
        enc: ChosenEncoder,
        adaptador: device::ChosenAdapter,
        captura: Captura,
        opcoes: OpcoesDaCadeia,
    ) -> Result<Self> {
        let reserva = enc.clone();
        let r = Self::montar_no_monitor_virtual(enc, adaptador, captura, opcoes);
        if r.is_err() {
            let limpo = encoder::desligar(&reserva);
            registro::linha(format!("a cadeia do monitor virtual não subiu: o encoder foi desligado (limpo={limpo})"));
        }
        r
    }

    fn montar_no_monitor_virtual(
        enc: ChosenEncoder,
        adaptador: device::ChosenAdapter,
        captura: Captura,
        opcoes: OpcoesDaCadeia,
    ) -> Result<Self> {
        let OpcoesDaCadeia {
            fps,
            origem_do_relogio: origem,
            idr_por_flush,
            idr_por_recriacao,
            taxa_de_entrega,
            piso_entre_recriacoes_ms,
            bitrate_alvo,
            troca_a_quente,
            caixa_unica,
            preferencia,
            padroes,
        } = opcoes;
        registro::linha(format!("encoder: \"{}\" hardware={}", enc.friendly_name, enc.is_hardware));
        registro::linha(format!(
            "adaptador: {} (vendor 0x{:04X}) luid={:016X} — o do monitor virtual",
            adaptador.description, adaptador.vendor_id, adaptador.luid
        ));
        // A placa foi fixada pelo LUID: a oficina e o recuo da recriação ficam nela.
        let placa_luid = adaptador.luid;
        let gerenciador = encoder::create_device_manager(&adaptador.device)?;
        registro::linha(format!(
            "captura iniciada: monitor virtual {} {}x{}",
            match captura.hmonitor() {
                Some(h) => format!("hmonitor={h:X}"),
                None => "(origem preta enquanto o monitor nasce: nenhuma tela é capturada)".to_string(),
            },
            captura.largura(),
            captura.altura()
        ));
        let (larg_da_captura, alt_da_captura) = (captura.largura(), captura.altura());

        // O teto: na tela estendida o alvo é o monitor inteiro (ver `abrir_com`).
        let teto = match padroes {
            None => quall_core::teto::ajustar(larg_da_captura, alt_da_captura, fps),
            Some(_) => quall_core::teto::ajustar_para(
                larg_da_captura,
                alt_da_captura,
                fps,
                Some(quall_core::teto::Alvo {
                    max_fs: quall_core::teto::LimitesDoNivel::macroblocos(larg_da_captura, alt_da_captura),
                    fps,
                }),
            ),
        };
        registro::linha(teto.relato(larg_da_captura, alt_da_captura, fps));
        registro::linha(match taxa_de_entrega.filter(|t| *t > 0) {
            Some(t) => format!("porta de taxa de entrega: LIGADA em {t} fps (o encoder segue configurado para {fps})"),
            None => "porta de taxa de entrega: DESLIGADA (padrão de produto)".to_string(),
        });
        registro::linha(if caixa_unica {
            "caminho do quadro: CAIXA ÚNICA (o quadro fica no slot do WGC até haver crédito)"
        } else {
            "caminho do quadro: duas caixas em série (padrão de produto)"
        });
        let escalador = if teto.reduziu_tamanho {
            match escala::Escalador::novo(&adaptador.device, larg_da_captura, alt_da_captura, teto.largura, teto.altura) {
                Ok(e) => {
                    registro::linha(format!(
                        "escalador: {}x{} -> {}x{} no ID3D11VideoProcessor",
                        larg_da_captura, alt_da_captura, teto.largura, teto.altura
                    ));
                    Some(e)
                }
                Err(e) => {
                    registro::linha(format!("escalador NÃO subiu: {e}"));
                    return Err(e);
                }
            }
        } else {
            None
        };
        let preset = EncodePreset::Screen;
        let bitrate = bitrate_alvo.unwrap_or(teto.teto_de_taxa_bps);
        let gop_frames = match padroes {
            None => teto.fps,
            Some(p) => teto.fps.saturating_mul(p.gop_segundos.max(1)),
        };
        let teto_de_quadro_bits = match padroes {
            Some(p) if p.teto_de_quadro_em_medios > 0.0 => {
                (p.teto_de_quadro_em_medios * f64::from(bitrate) / f64::from(teto.fps.max(1))) as u32
            }
            _ => 0,
        };
        let cfg = EncoderConfig {
            // A tela e a origem sintética entregam BGRA. A câmera (NV12) é a fase 3.
            entrada: FormatoDeEntrada::Argb32,
            width: teto.largura,
            height: teto.altura,
            fps: teto.fps,
            bitrate_bps: bitrate,
            gop_frames,
            intra_refresh_frames: 0,
            slice_bytes: 0,
            teto_de_quadro_bits,
        };
        encoder::configure(&enc, &gerenciador, &cfg)?;
        let espacamento_aceito = encoder::tentar_espacamento_de_idr(&enc, gop_frames);
        registro::linha(format!(
            "preset={preset:?} bitrate={bitrate} gop_pedido={gop_frames} quadros | MF_MT_MAX_KEYFRAME_SPACING: {}",
            if espacamento_aceito { "aceito" } else { "recusado" }
        ));
        if let Some(p) = padroes {
            registro::linha(format!(
                "padrões da tela estendida: repetir o quadro parado depois de {} ms | GOP pedido {} s ({gop_frames} quadros), sem IDR forçado a cada {} s (seria uma recriação) | teto de quadro {}",
                p.repetir_apos.as_millis(),
                p.gop_segundos,
                p.gop_segundos,
                if teto_de_quadro_bits > 0 {
                    format!(
                        "pedido {} B ({:.1} quadros médios de {} bps a {} fps) — {}",
                        teto_de_quadro_bits / 8,
                        p.teto_de_quadro_em_medios,
                        bitrate,
                        teto.fps,
                        encoder::teto_de_quadro_relido(&enc)
                    )
                } else {
                    "desligado".to_string()
                },
            ));
        }
        encoder::start_stream(&enc.transform)?;
        let eventos = encoder::spawn_event_pump(enc.events.clone());
        let oficina = if troca_a_quente && idr_por_recriacao {
            registro::linha("troca a quente: LIGADA — a reserva é montada fora do laço e a troca é por ponteiro. Ver oficina.rs.");
            Some(Oficina::abrir(&gerenciador, cfg, preferencia, Some(placa_luid), false))
        } else {
            registro::linha(format!(
                "troca a quente: desligada ({})",
                if idr_por_recriacao { "por --sem-troca-a-quente" } else { "a quinta porta está desligada" }
            ));
            None
        };
        Ok(Cadeia {
            largura: teto.largura,
            altura: teto.altura,
            escalador,
            conversor: None,
            nome_do_encoder: enc.friendly_name.clone(),
            encoder_e_hardware: enc.is_hardware,
            adaptador: format!("{} (0x{:04X})", adaptador.description, adaptador.vendor_id),
            captura,
            padroes,
            ultima_para_repetir: None,
            copia_para_repetir: None,
            ultima_submissao: None,
            ultimo_carimbo_submetido: None,
            conversor_esperando_desde: None,
            trocas_de_aspecto_vistas: 0,
            idr_pendente: false,
            proximo_e_repeticao: false,
            ultima_repeticao_da_camera: None,
            morta: None,
            preferencia,
            placa_do_encoder: Some(placa_luid),
            enc,
            eventos,
            creditos: 0,
            caixa_unica,
            entrada_aceita_em: None,
            pendente: None,
            submetidos: VecDeque::new(),
            parametros: None,
            parametros_de_referencia: None,
            resumo_sps: None,
            inicio: origem,
            indice: 0,
            duracao_100ns: 10_000_000i64 / teto.fps.max(1) as i64,
            gop_frames,
            idrs_em: Vec::new(),
            pedidos_de_idr_em: Vec::new(),
            contadores: Contadores::default(),
            idr_por_flush,
            flushes: 0,
            dispositivo: adaptador.device.clone(),
            gerenciador,
            cfg,
            idr_por_recriacao,
            oficina,
            reserva: None,
            trocas_a_quente: 0,
            trocas_sem_reserva: 0,
            montagem_de_fundo_ms: Vec::new(),
            custo_de_troca_us: Vec::new(),
            recriacoes: 0,
            custo_de_montagem_ms: Vec::new(),
            custo_ate_o_idr_ms: Vec::new(),
            esperando_idr_desde: None,
            encodados_no_encoder_atual: 0,
            recriacoes_dispensadas: 0,
            piso_entre_recriacoes: Some(piso_entre_recriacoes_ms).filter(|p| *p > 0).map(Duration::from_millis),
            ultima_recriacao: None,
            pedido_adiado: false,
            pedido_adiado_desde: None,
            recriacoes_adiadas: 0,
            espera_do_piso_ms: Vec::new(),
            intervalos_entre_recriacoes_ms: Vec::new(),
            desligamentos_sujos: 0,
            parametros_diferentes: 0,
            parametros_iguais: 0,
            perfil: Perfil::default(),
            ultimo_capturado: None,
            intervalo_de_entrega: taxa_de_entrega.filter(|t| *t > 0).map(|t| Duration::from_secs_f64(1.0 / f64::from(t))),
            proxima_entrega: None,
            bytes_de_idr: Vec::new(),
            pacotes_de_idr: Vec::new(),
            soma_bytes_nao_idr: 0,
            soma_pacotes_nao_idr: 0,
            n_nao_idr: 0,
            max_bytes_nao_idr: 0,
            max_pacotes_nao_idr: 0,
            faixas_de_pacotes_por_quadro: [0; 5],
            posicoes_apos_recriacao: [PosicaoPosRecriacao::default(); POSICOES_PERFILADAS + 1],
            faixas_de_rajada_de_saida: [0; 5],
            max_pacotes_por_volta: 0,
            voltas_com_saida: 0,
            soma_quadros_por_volta: 0,
            espacamento_aceito,
        })
    }

    /// O dispositivo D3D11 da cadeia — o da captura. A reabertura da captura do monitor virtual
    /// monta o pool novo nele.
    pub fn dispositivo(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Device {
        &self.dispositivo
    }

    /// O `HMONITOR` da captura de agora, como número (`None` na origem sintética).
    pub fn hmonitor_da_captura(&self) -> Option<isize> {
        self.captura.hmonitor()
    }

    pub fn primeiro_quadro_da_captura(&self) -> Option<Instant> {
        self.captura.primeiro_quadro()
    }

    pub fn ultimo_quadro_da_captura(&self) -> Option<Instant> {
        self.captura.ultimo_quadro()
    }

    /// O maior intervalo entre dois quadros tirados da captura, na sessão toda, em ms.
    pub fn maior_intervalo_de_captura_ms(&self) -> f64 {
        self.perfil.intervalo_maximo_us as f64 / 1000.0
    }

    /// **Troca a captura por outra do mesmo monitor** — o `HMONITOR` mudou (o nome GDI do monitor
    /// mudou quando outro chegou, E5) e a sessão seguiu o alvo. O encoder, o escalador, a cópia da
    /// repetição e o SPS continuam: por isso **só com o tamanho igual**; tamanho diferente é erro, e
    /// a sessão encerra com motivo (a revisão, item 9). O quadro pendente, da captura velha, cai.
    ///
    /// **Sem chamada do WGC aqui**: a captura velha volta para quem chamou, que a fecha fora do fio
    /// da sessão (`capture::fechar_em_segundo_plano`); com o tamanho errado, a nova volta com o motivo
    /// (a revisão de 15/09, item 10).
    pub fn trocar_captura(&mut self, nova: ScreenCapture) -> std::result::Result<Captura, (ScreenCapture, String)> {
        let (l, a) = (self.captura.largura(), self.captura.altura());
        if (nova.width, nova.height) != (l, a) {
            let motivo = format!(
                "o monitor voltou com outro tamanho ({l}x{a} → {}x{}): o pool, a cópia da repetição, o escalador e o SPS são do tamanho velho",
                nova.width, nova.height
            );
            return Err((nova, motivo));
        }
        self.pendente = None;
        Ok(std::mem::replace(&mut self.captura, Captura::Tela(nova)))
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn sps_e_pps_sozinhos_nao_sao_quadro() {
        let so_parametros = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xcb];
        assert!(!tem_fatia(&so_parametros));
    }

    /// A câmera parada (22/09): as repetições são origem na conta, e a sobrescrita é destino. A
    /// prova de bancada sem isto dizia "captura NÃO FECHA (sobra -333)" com 333 repetições.
    #[test]
    fn a_conta_fecha_com_as_repeticoes() {
        let mut c = Contadores { capturados: 447, entregues_ao_mft: 447, ..Default::default() };
        assert_eq!(c.diferenca_do_fechamento(0), 0);
        let sem_repeticao = c.linha_dos_degraus(0, 0);
        assert!(sem_repeticao.starts_with("degraus: capturados=447 -> "), "a linha de sempre, byte a byte");
        // 226 repetições postas: 225 entraram, 1 foi sobrescrita pelo quadro real que voltou.
        c.repeticoes_postas = 226;
        c.entregues_ao_mft += 225;
        c.repeticoes_sobrescritas = 1;
        assert_eq!(c.diferenca_do_fechamento(0), 0);
        let linha = c.linha_dos_degraus(0, 0);
        assert!(linha.contains("(+ repeticoes_postas=226 repeticoes_sobrescritas=1)"));
        assert!(linha.contains("captura FECHA"));
        // Uma repetição sem destino continua acusando.
        c.repeticoes_postas += 1;
        assert_eq!(c.diferenca_do_fechamento(0), 1);
        assert_eq!(c.diferenca_do_fechamento(1), 0, "a que ainda está pendente fecha");
    }

    #[test]
    fn idr_e_quadro() {
        let com_idr = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88, 0x84];
        assert!(tem_fatia(&com_idr));
    }

    #[test]
    fn quadro_p_e_quadro() {
        assert!(tem_fatia(&[0, 0, 1, 0x41, 0x9a, 0x00]));
    }

    #[test]
    fn parametros_saem_do_idr_sem_a_fatia() {
        // SPS (0x67), PPS (0x68) e a fatia de IDR (0x65). Só os dois primeiros são conjunto de
        // parâmetros; a fatia não pode entrar na comparação, ou dois IDR iguais em formato e
        // diferentes em imagem apareceriam como "o conjunto mudou".
        let idr = [
            0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, //
            0, 0, 0, 1, 0x68, 0xcb, 0x83, 0xcb, //
            0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x21,
        ];
        let p = parametros_do_quadro(&idr).expect("há SPS e PPS neste quadro");
        assert_eq!(
            p,
            vec![0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0, 0, 0, 1, 0x68, 0xcb, 0x83, 0xcb]
        );
    }

    #[test]
    fn quadro_p_nao_tem_conjunto_de_parametros() {
        assert!(parametros_do_quadro(&[0, 0, 0, 1, 0x41, 0x9a, 0x00]).is_none());
    }

    #[test]
    fn o_tamanho_do_start_code_nao_muda_a_comparacao() {
        // O mesmo conjunto de parâmetros escrito com start code de 3 e de 4 bytes tem de comparar
        // igual: é o **conteúdo** que diz se o receptor precisa remontar o decodificador.
        let quatro = [0, 0, 0, 1, 0x67, 0x42, 0xc0, 0, 0, 0, 1, 0x68, 0xcb, 0, 0, 0, 1, 0x65, 0x88];
        let tres = [0, 0, 1, 0x67, 0x42, 0xc0, 0, 0, 1, 0x68, 0xcb, 0, 0, 1, 0x65, 0x88];
        assert_eq!(parametros_do_quadro(&quatro), parametros_do_quadro(&tres));
    }
}
