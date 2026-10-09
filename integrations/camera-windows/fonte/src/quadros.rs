//! De onde saem os quadros que a câmera virtual entrega.
//!
//! A fonte de mídia **não** roda no processo do Quall: o Frame Server a instancia dentro de
//! `svchost.exe -k Camera`, e o app consumidor pode instanciá-la dentro dele mesmo. O vídeo
//! precisa, portanto, atravessar uma fronteira de processo — e, no caso do Frame Server, também
//! de **conta** (`NT AUTHORITY\LocalService`) e de **sessão** (0, não a do usuário).
//!
//! ## Por que cano nomeado, e não memória compartilhada
//!
//! Memória compartilhada seria mais barata por quadro, mas o nome precisaria viver no espaço
//! `Global\` para atravessar a sessão — e **criar** um objeto em `Global\` exige o privilégio
//! `SeCreateGlobalPrivilege`, que um usuário interativo comum não tem. Isso obrigaria o app do
//! Quall a rodar elevado, o que é inaceitável para o produto.
//!
//! O espaço de nomes de canos (`\\.\pipe\...`) é global por construção, não é fatiado por sessão,
//! e um processo de usuário comum pode criar um cano com a DACL que quiser. Por isso o **host é o
//! servidor** e a fonte de mídia é **cliente**: o cano existe enquanto o Quall estiver rodando, e
//! cada instância da fonte (Frame Server, app consumidor) abre a sua própria instância do cano.
//!
//! O custo disso é medido, não suposto — ver a seção "Números medidos" do README.
//!
//! ## Formato fixo, de propósito
//!
//! A câmera anuncia **1920x1080 NV12 a 30 fps e nada mais** (era 1280x720 até 01/09/2026, e
//! subiu junto com o `PERFIL_H264` para nível 4.0 — enquanto publicasse 720p, todo 1080p que
//! chegasse pela rede era reduzido antes de chegar ao Zoom). A fonte de mídia é consultada pelo
//! sistema na geração do *sensor group*, que acontece **sem o Quall estar rodando** — não há como
//! anunciar a resolução do vídeo que vai chegar, porque nesse instante não chegou nada. Encaixar
//! o que vem da rede (inclusive retrato 720x1280 de um celular) nesse quadro é trabalho do host.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::diga;

pub const LARGURA: u32 = 1920;
pub const ALTURA: u32 = 1080;
pub const FPS: u32 = 30;
/// NV12: plano Y de w*h, seguido de plano UV entrelaçado de w*h/2.
pub const BYTES_NV12: usize = (LARGURA as usize) * (ALTURA as usize) * 3 / 2;
/// O cano de quem **não** tem nome: o de antes de 09/09/2026, e o que a sonda serve por padrão.
///
/// Continua existindo por dois motivos concretos, e nenhum é compatibilidade por gosto: uma fonte
/// instanciada **direto pelo CLSID** (o `ler --direto` da sonda, e qualquer app que faça o mesmo)
/// nunca passa pelo Frame Server e por isso **não recebe nome nenhum**; e todo roteiro de bancada
/// escrito até hoje alimenta este nome.
pub const CANO_SEM_NOME: &str = r"\\.\pipe\quall-camera-v1";

/// O cano de uma câmera, derivado do **nome dela**.
///
/// # Por que o nome, e não o CLSID
///
/// Porque o Frame Server entrega o nome, e foi medido (`docs/bancada.md` §8.59): dentro do
/// `svchost`, `IMFActivate::ActivateObject` traz oito atributos, e um deles é o nome com que a
/// câmera foi criada, sem a decoração "(Câmera Virtual do Windows)" que a enumeração acrescenta.
/// O plano anterior era registrar **N CLSIDs** em `HKLM`, um por vaga — irreversível na máquina de
/// quem instala. A medida apagou isso: um CLSID basta, e o teto de câmeras deixa de ser fixo em
/// tempo de compilação.
///
/// # Por que a correspondência é 1 para 1 de graça
///
/// O nó de dispositivo é chaveado por **CLSID + nome** — medido em 27/08/2026 ao investigar
/// duplicidade, e confirmado em §8.58. Duas câmeras de mesmo nome sobre o mesmo CLSID **não são
/// duas câmeras**: são uma. Então derivar o cano do nome não pode colidir mais do que o próprio
/// sistema já colide.
///
/// # O que a digestão faz, e o que ela não faz
///
/// FNV-1a de 64 bits sobre os bytes UTF-8 do nome, em hexadecimal. Não é criptografia e não
/// precisa ser: o que se quer é um nome de cano **legal** (sem barra, sem acento, tamanho fixo) e
/// **igual dos dois lados**. O nome cru não serve: `\\.\pipe\` não aceita `\` e a comparação de
/// nomes de cano do Windows tem regras próprias que ninguém aqui mediu.
///
/// **Diferença de caixa produz cano diferente**, de propósito: ninguém mediu se o sistema trata
/// `"Sala"` e `"sala"` como um nó ou dois, e supor errado aqui seria dar a duas câmeras o mesmo
/// vídeo em silêncio. Quem cria a câmera e quem serve o cano passam a **mesma** string.
pub fn cano_do_nome(nome: &str) -> String {
    format!(r"\\.\pipe\quall-camera-v1-{:016x}", digestao(nome))
}

/// FNV-1a de 64 bits. Escrita à mão porque a alternativa é arrastar uma dependência para uma
/// dúzia de linhas — e porque `DefaultHasher` do `std` **não promete estabilidade entre versões**,
/// que é justamente a única coisa que esta função precisa prometer: os dois lados do cano podem
/// ser compilados em dias diferentes.
fn digestao(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// `"QUAL"` em little-endian.
const MAGICO: u32 = 0x4C41_5551;
/// Cabeçalho por quadro, no cano: magico, largura, altura, bytes, timestamp_us.
const CABECALHO: usize = 4 + 4 + 4 + 4 + 8;

pub struct Quadro {
    pub bytes: Vec<u8>,
    /// Relógio monotônico do host, em microssegundos.
    pub timestamp_us: u64,
    /// Quando este quadro foi posto aqui pelo fio leitor (para medir idade).
    pub chegada: std::time::Instant,
}

#[derive(Default)]
struct Estado {
    ultimo: Option<Quadro>,
    /// O último quadro **entregue**, guardado para ser repetido quando não chegar um novo a
    /// tempo. Ver `proximo`: é ele que evita o piscar de padrão de bancada no meio do vídeo.
    repeticao: Option<Vec<u8>>,
    /// Quantas vezes a repetição foi usada. Um número que cresce diz que a origem entrega menos
    /// que 30 fps — informação de diagnóstico, não defeito.
    repetidos: u64,
    /// Quantos quadros o fio leitor recebeu do cano desde que a fonte foi criada.
    recebidos: u64,
    /// Quantos quadros a fonte entregou ao consumidor.
    entregues: u64,
    /// Quantos quadros do cano nunca foram entregues (chegou outro por cima).
    descartados: u64,
    conectado: bool,
    /// Quanto tempo o último quadro entregue ficou parado aqui esperando ser pedido. Com a espera
    /// por condvar isto tem de ficar perto de zero; se subir, o Frame Server voltou a ditar o
    /// ritmo e a fase voltou ao caminho.
    idade_us: u64,
    /// Carimbo que o host pôs no cabeçalho do último quadro entregue.
    ultimo_ts_us: u64,
}

pub struct Contadores {
    pub recebidos: u64,
    pub entregues: u64,
    pub descartados: u64,
    pub repetidos: u64,
    pub conectado: bool,
    pub idade_us: u64,
    pub ultimo_ts_us: u64,
}

pub struct Provedor {
    estado: Arc<Mutex<Estado>>,
    chegou: Arc<Condvar>,
    parar: Arc<AtomicBool>,
    /// Contador de quadros sintéticos, para a animação de fundo.
    tique: AtomicU64,
    /// Já soube qual câmera é? Ver [`Provedor::identificar`].
    identificado: AtomicBool,
}

impl Provedor {
    /// Constrói **sem** abrir cano nenhum. Quem abre é [`Provedor::identificar`].
    ///
    /// A versão anterior subia o fio leitor aqui, na primeira linha de `criar()`. Isso deixou de
    /// ser possível quando o cano passou a depender do nome da câmera, e a medida diz por quê
    /// (`docs/bancada.md` §8.59): **em `criar()` o repositório de atributos tem zero itens**, nos
    /// dois processos. A identidade chega depois da construção — abrir o cano aqui seria abrir o
    /// cano errado, por construção, e o sintoma seria a câmera de um aparelho mostrando o vídeo
    /// de outro.
    pub fn novo() -> Arc<Self> {
        Arc::new(Provedor {
            estado: Arc::new(Mutex::new(Estado::default())),
            chegou: Arc::new(Condvar::new()),
            parar: Arc::new(AtomicBool::new(false)),
            tique: AtomicU64::new(0),
            identificado: AtomicBool::new(false),
        })
    }

    /// Diz a esta fonte **qual câmera ela é**, e com isso sobe o fio que lê o cano dela.
    ///
    /// `None` é o caminho de quem não passou pelo Frame Server (instanciação direta pelo CLSID):
    /// cai no [`CANO_SEM_NOME`], que é o comportamento de antes desta rodada.
    ///
    /// Idempotente de propósito: é chamada do `ActivateObject` **e** do `Start`, porque nem todo
    /// caminho passa pelos dois, e a segunda chamada não pode abrir um segundo fio.
    pub fn identificar(self: &Arc<Self>, nome: Option<&str>) {
        if self.identificado.swap(true, Ordering::SeqCst) {
            return;
        }
        let cano = match nome {
            Some(n) => cano_do_nome(n),
            None => CANO_SEM_NOME.to_string(),
        };
        diga!("cano desta câmera configurado (nome omitido)");
        let (estado, chegou, parar) = (self.estado.clone(), self.chegou.clone(), self.parar.clone());
        std::thread::spawn(move || fio_leitor(estado, chegou, parar, cano));
    }

    pub fn parar(&self) {
        self.parar.store(true, Ordering::Relaxed);
        self.chegou.notify_all();
    }

    /// Devolve o quadro mais novo, **esperando** até `prazo` por um quadro do cano.
    ///
    /// A espera não é conforto: sem ela, a fonte ritma no relógio dela e o host no dele, e dois
    /// relógios independentes de 30 Hz somam meia batida de fase ao caminho. Medido nesta
    /// bancada, era a maior parcela dos 52,8 ms de latência da primeira versão.
    ///
    /// Nunca falha, e a ordem do que ela devolve **é** o comportamento do produto:
    ///
    /// 1. o quadro novo que chegou pelo cano;
    /// 2. senão, **o último quadro entregue de novo**, enquanto o cano estiver conectado;
    /// 3. só sem cano é que sai o padrão de bancada.
    ///
    /// O passo 2 foi acrescentado depois de medido, e o defeito que ele conserta é grave. Com a
    /// câmera anunciando 30 fps e a origem da rede entregando menos — 27 fps na primeira corrida
    /// contra o MacBook, porque a captura de tela só emite quando a tela muda — o passo 3 entrava
    /// no lugar do quadro que faltava: o diário registrou `entregues=279` para **300** quadros
    /// pedidos, ou seja **21 quadros de padrão de bancada intercalados no vídeo**, cerca de duas
    /// piscadas por segundo dentro do Zoom. Repetir o último quadro é o que qualquer câmera faz
    /// quando o sensor não tem novidade, e é invisível.
    ///
    /// O padrão de bancada continua existindo para o caso em que ele é a informação certa: sem
    /// cano, o Quall não está rodando. Uma câmera que às vezes não entrega quadro é pior que uma
    /// que entrega barra de teste — o app do outro lado não distingue "sem imagem" de "travou".
    pub fn proximo(&self, prazo: std::time::Duration) -> (Vec<u8>, bool) {
        let mut e = self.estado.lock().unwrap();
        if e.conectado && e.ultimo.is_none() {
            let (g, _) = self.chegou.wait_timeout(e, prazo).unwrap();
            e = g;
        }
        if let Some(q) = e.ultimo.take() {
            e.entregues += 1;
            e.idade_us = q.chegada.elapsed().as_micros() as u64;
            e.ultimo_ts_us = q.timestamp_us;
            e.repeticao = Some(q.bytes.clone());
            return (q.bytes, true);
        }
        if e.conectado {
            if let Some(anterior) = &e.repeticao {
                let copia = anterior.clone();
                e.repetidos += 1;
                e.entregues += 1;
                return (copia, true);
            }
        } else {
            // Cano caiu: a repetição envelheceu e vira mentira. Soltar aqui também devolve
            // 3,11 MB (`BYTES_NV12`) enquanto ninguém está transmitindo.
            e.repeticao = None;
        }
        drop(e);
        let n = self.tique.fetch_add(1, Ordering::Relaxed);
        (padrao_de_bancada(n), false)
    }

    pub fn contadores(&self) -> Contadores {
        let e = self.estado.lock().unwrap();
        Contadores {
            recebidos: e.recebidos,
            entregues: e.entregues,
            descartados: e.descartados,
            repetidos: e.repetidos,
            conectado: e.conectado,
            idade_us: e.idade_us,
            ultimo_ts_us: e.ultimo_ts_us,
        }
    }
}

fn fio_leitor(estado: Arc<Mutex<Estado>>, chegou: Arc<Condvar>, parar: Arc<AtomicBool>, cano: String) {
    let mut avisou = false;
    while !parar.load(Ordering::Relaxed) {
        // `std::fs::File::open` num caminho `\\.\pipe\...` abre a ponta cliente do cano. Evita
        // um `CreateFileW` cru só para chegar no mesmo lugar.
        let f = std::fs::OpenOptions::new().read(true).open(&cano);
        let mut f = match f {
            Ok(f) => {
                diga!("cano conectado");
                avisou = false;
                estado.lock().unwrap().conectado = true;
                f
            }
            Err(e) => {
                if !avisou {
                    diga!("cano indisponível ({e}); entregando padrão de bancada");
                    avisou = true;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
        };

        let mut cab = [0u8; CABECALHO];
        let mut corpo = vec![0u8; BYTES_NV12];
        loop {
            if parar.load(Ordering::Relaxed) {
                return;
            }
            if f.read_exact(&mut cab).is_err() {
                break;
            }
            let magico = u32::from_le_bytes(cab[0..4].try_into().unwrap());
            let largura = u32::from_le_bytes(cab[4..8].try_into().unwrap());
            let altura = u32::from_le_bytes(cab[8..12].try_into().unwrap());
            let bytes = u32::from_le_bytes(cab[12..16].try_into().unwrap()) as usize;
            let ts = u64::from_le_bytes(cab[16..24].try_into().unwrap());
            if magico != MAGICO || largura != LARGURA || altura != ALTURA || bytes != BYTES_NV12 {
                diga!(
                    "cabeçalho fora do contrato (magico={magico:#x} {largura}x{altura} {bytes}B); \
                     fechando o cano em vez de tentar adivinhar"
                );
                break;
            }
            if f.read_exact(&mut corpo).is_err() {
                break;
            }
            let mut e = estado.lock().unwrap();
            if e.ultimo.is_some() {
                e.descartados += 1;
            }
            e.recebidos += 1;
            e.ultimo = Some(Quadro {
                bytes: corpo.clone(),
                timestamp_us: ts,
                chegada: std::time::Instant::now(),
            });
            drop(e);
            chegou.notify_one();
        }
        diga!("cano caiu; voltando a esperar");
        estado.lock().unwrap().conectado = false;
        chegou.notify_all();
    }
}

/// Padrão de bancada em NV12: fundo com gradiente, uma barra clara que anda e a cor girando.
///
/// Existe para uma coisa só: **ser inconfundivelmente vivo numa captura de tela**. Uma imagem
/// estática não distingue "a câmera virtual funciona" de "o app está mostrando o último quadro
/// preto que recebeu"; um padrão que se move em toda captura sucessiva distingue.
fn padrao_de_bancada(n: u64) -> Vec<u8> {
    let w = LARGURA as usize;
    let h = ALTURA as usize;
    let mut buf = vec![0u8; BYTES_NV12];

    let barra = ((n * 8) % (w as u64)) as usize;
    for y in 0..h {
        let base = y * w;
        let luz_linha = (y * 160 / h) as u8;
        for x in 0..w {
            let mut v = 16u16 + luz_linha as u16 + (x * 60 / w) as u16;
            // Barra vertical de 60 px que anda para a direita.
            if x >= barra && x < barra + 60 {
                v = 235;
            }
            // Grade a cada 128 px/linhas: dá referência de escala e mostra se o app recortou.
            if x % 128 == 0 || y % 128 == 0 {
                v = v.saturating_add(60);
            }
            buf[base + x] = v.min(235) as u8;
        }
    }

    // Plano UV: gira a cor devagar, para a mudança ser visível mesmo com a barra fora do quadro.
    let fase = (n as f32) * 0.05;
    let u = (128.0 + 60.0 * fase.sin()) as u8;
    let v = (128.0 + 60.0 * fase.cos()) as u8;
    let inicio_uv = w * h;
    for i in 0..(w * h / 4) {
        buf[inicio_uv + i * 2] = u;
        buf[inicio_uv + i * 2 + 1] = v;
    }
    buf
}
