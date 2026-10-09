//! **As regras do gravador local do R5** (`docs/teleprompter-com-camera.md` §5 e §8.10): aritmética
//! pura, sem Win32 e sem `quall-core`, para os testes rodarem no portão e em qualquer máquina
//! (`rustc --test` sobre este arquivo sozinho).
//!
//! - [`taxa_da_gravacao`]: 14 Mbit/s a 1080p30, escalada pela área e pelo fps (a regra do Android,
//!   §8.6, com os mesmos números nos testes);
//! - [`LinhaDoVideo`]: o PTS real de cada quadro, no zero do primeiro, e a duração de cada um pela
//!   distância até o seguinte (fps variável: o quadro fica na mão até o próximo chegar);
//! - [`ReguaDoSom`]: o PCM do microfone numa régua de amostras que começa no primeiro quadro, com
//!   silêncio onde não há som, corte onde o som sobrepõe, e **a comporta**: nada de som depois do
//!   último quadro escrito (o achado A do Android, §8.6: o AAC não tem volta);
//! - [`DecisaoDoPedido`]: o que a tela faz com o pedido de gravar/parar do controle (§13 do contrato);
//! - os nomes dos arquivos e o corte da caixa incompleta de um órfão (os pendentes, §5.4).

use std::time::Duration;

// =============================================================================================
// A taxa
// =============================================================================================

/// A referência: 1080p30 a 14 Mbit/s (Bruno, 24/09: a faixa de 12–16 do iPhone).
pub const REFERENCIA_BPS: u32 = 14_000_000;
pub const REFERENCIA_PIXELS: u64 = 1920 * 1080;
pub const REFERENCIA_FPS: u32 = 30;
/// 1,5× por dobra de fps: log2(1,5).
pub const EXPOENTE_DO_FPS: f64 = 0.585;
pub const TAXA_MINIMA: u32 = 2_000_000;
pub const TAXA_MAXIMA: u32 = 60_000_000;

/// **A taxa da gravação**, em bit/s: a referência escalada pela área (proporcional aos pixels) e
/// pelo fps ([`EXPOENTE_DO_FPS`]), entre [`TAXA_MINIMA`] e [`TAXA_MAXIMA`]. A mesma conta do
/// `ParametrosDaGravacao.taxa` do Android, sem os perfis do aparelho (o Windows não os tem).
pub fn taxa_da_gravacao(largura: u32, altura: u32, fps: u32) -> u32 {
    let px = u64::from(largura) * u64::from(altura);
    let fator = (f64::from(fps.max(1)) / f64::from(REFERENCIA_FPS)).powf(EXPOENTE_DO_FPS);
    let regra = f64::from(REFERENCIA_BPS) * px as f64 / REFERENCIA_PIXELS as f64 * fator;
    (regra as u64).clamp(u64::from(TAXA_MINIMA), u64::from(TAXA_MAXIMA)) as u32
}

/// O fps nominal que vai no tipo do escritor: o do tipo nativo da câmera, arredondado, entre 1 e 60.
/// É declaração: o arquivo tem os carimbos reais, e cada quadro a sua duração.
pub fn fps_nominal(fps: f64) -> u32 {
    if !fps.is_finite() || fps <= 0.0 {
        return 30;
    }
    (fps.round() as u32).clamp(1, 60)
}

// =============================================================================================
// O espaço
// =============================================================================================

/// Para começar a gravar, pelo menos isto livre no disco da pasta (o iOS usa o mesmo número).
pub const ESPACO_PARA_COMECAR: u64 = 500 * 1024 * 1024;
/// Gravando, abaixo disto a gravação para (e o arquivo fecha inteiro).
pub const ESPACO_PARA_SEGUIR: u64 = 300 * 1024 * 1024;

/// O texto da recusa por espaço, para o controle remoto e a tela (no idioma do app na hora).
pub fn texto_sem_espaco(livre: u64, minimo: u64) -> String {
    crate::idioma::tf("sem espaço: sobram {} MB (gravar pede {} MB livres)", &[&(livre / (1024 * 1024)), &(minimo / (1024 * 1024))])
}

/// A linha da gravação diz que grava sem som? O "SEM SOM" é o marcador que a legenda do Gravar
/// procura (`modelo_da_janela::legenda_do_gravar`); a linha nasce no idioma da hora, então o
/// marcador vale nos dois.
pub const MARCA_SEM_SOM: &str = "SEM SOM"; // i18n: chave
pub fn diz_sem_som(linha: &str) -> bool {
    linha.contains(MARCA_SEM_SOM) || crate::idioma::en_de(MARCA_SEM_SOM).is_some_and(|en| linha.contains(en))
}

// =============================================================================================
// O tempo do vídeo
// =============================================================================================

/// Um quadro pronto para o escritor: o PTS e a duração, em 100 ns, e o que veio junto.
#[derive(Debug, Clone, PartialEq)]
pub struct QuadroNoTempo<T> {
    pub pts_100ns: i64,
    pub duracao_100ns: i64,
    pub carga: T,
}

/// **O tempo do vídeo no arquivo (G3)**: o carimbo real de cada quadro (em µs, no zero do dono),
/// menos o do primeiro quadro gravado. A duração de um quadro é a distância até o seguinte, então
/// cada quadro fica na mão até o próximo chegar; o último sai no fim com a duração do anterior.
/// Um carimbo que não anda para a frente é recusado e contado (o arquivo nunca volta no tempo).
///
/// **A grade** (`em_grade`, o padrão do gravador desde a bancada de 27/09, §8.10.6): o PTS
/// vira a vaga mais próxima de uma grade de fps constante (`pts_da_vaga`, absoluta: não acumula), a
/// duração é a distância de vaga a vaga, e o quadro que cai na mesma vaga do anterior toma o lugar
/// dele (`na_mesma_vaga`; o velho volta em `Err`). O PTS se afasta do carimbo real até meia vaga.
#[derive(Debug)]
pub struct LinhaDoVideo<T> {
    zero_us: Option<u64>,
    na_mao: Option<(i64, T)>,
    ultima_duracao: i64,
    grade_fps: Option<u32>,
    ultimo_carimbo: Option<u64>,
    pub recusados_por_carimbo: u64,
    pub maior_buraco_100ns: i64,
    pub escritos: u64,
    pub na_mesma_vaga: u64,
}

/// A vaga da grade mais próxima de `rel_us` (µs desde o zero).
pub fn vaga_de(rel_us: u64, fps: u32) -> u64 {
    (rel_us * u64::from(fps.max(1)) + 500_000) / 1_000_000
}

/// O PTS da vaga `n`, em 100 ns, calculado do zero (sem somar durações).
pub fn pts_da_vaga(n: u64, fps: u32) -> i64 {
    let fps = u64::from(fps.max(1));
    ((n * 10_000_000 + fps / 2) / fps) as i64
}

/// A duração do último quadro quando não há anterior: um quadro a 30 fps.
pub const DURACAO_PADRAO_100NS: i64 = 333_333;

impl<T> Default for LinhaDoVideo<T> {
    fn default() -> Self {
        LinhaDoVideo {
            zero_us: None,
            na_mao: None,
            ultima_duracao: DURACAO_PADRAO_100NS,
            grade_fps: None,
            ultimo_carimbo: None,
            recusados_por_carimbo: 0,
            maior_buraco_100ns: 0,
            escritos: 0,
            na_mesma_vaga: 0,
        }
    }
}

impl<T> LinhaDoVideo<T> {
    /// A linha em grade de `fps` constante (o braço de comparação).
    pub fn em_grade(fps: u32) -> Self {
        LinhaDoVideo { grade_fps: Some(fps.max(1)), ultima_duracao: pts_da_vaga(1, fps), ..Default::default() }
    }

    pub fn em_grade_de(&self) -> Option<u32> {
        self.grade_fps
    }

    /// O zero do arquivo (o carimbo do primeiro quadro, em µs no relógio do dono), depois do primeiro.
    pub fn zero_us(&self) -> Option<u64> {
        self.zero_us
    }

    /// Um quadro chegou com o carimbo `carimbo_us`. Devolve o **anterior**, pronto para escrever,
    /// quando há um; e `Err(carga)` quando o quadro é recusado (carimbo que não anda).
    pub fn chegou(&mut self, carimbo_us: u64, carga: T) -> Result<Option<QuadroNoTempo<T>>, T> {
        let zero = *self.zero_us.get_or_insert(carimbo_us);
        if carimbo_us < zero {
            self.recusados_por_carimbo += 1;
            return Err(carga);
        }
        let pts = match self.grade_fps {
            None => ((carimbo_us - zero) as i64) * 10,
            Some(fps) => {
                if self.ultimo_carimbo.is_some_and(|u| carimbo_us <= u) {
                    self.recusados_por_carimbo += 1;
                    return Err(carga);
                }
                self.ultimo_carimbo = Some(carimbo_us);
                let pts = pts_da_vaga(vaga_de(carimbo_us - zero, fps), fps);
                if let Some((anterior, velho)) = self.na_mao.take() {
                    if pts <= anterior {
                        // A mesma vaga: o mais novo fica, o velho volta.
                        self.na_mao = Some((anterior, carga));
                        self.na_mesma_vaga += 1;
                        return Err(velho);
                    }
                    self.na_mao = Some((anterior, velho));
                }
                pts
            }
        };
        if let Some((anterior, _)) = &self.na_mao {
            if pts <= *anterior {
                self.recusados_por_carimbo += 1;
                return Err(carga);
            }
        }
        let saiu = self.na_mao.take().map(|(p, c)| {
            let d = pts - p;
            self.ultima_duracao = d;
            self.maior_buraco_100ns = self.maior_buraco_100ns.max(d);
            self.escritos += 1;
            QuadroNoTempo { pts_100ns: p, duracao_100ns: d, carga: c }
        });
        self.na_mao = Some((pts, carga));
        Ok(saiu)
    }

    /// O PTS do quadro na mão (o último que chegou), em 100 ns.
    pub fn pts_na_mao(&self) -> Option<i64> {
        self.na_mao.as_ref().map(|(p, _)| *p)
    }

    /// O fim, cobrindo até `fim_minimo_100ns` (o som já entregue numa pausa da câmera, a revisão do
    /// código, M4): o quadro na mão dura o que for maior, a duração do anterior ou até lá.
    pub fn terminar_ate(&mut self, fim_minimo_100ns: i64) -> Option<QuadroNoTempo<T>> {
        let d = self.ultima_duracao.max(1);
        self.na_mao.take().map(|(p, c)| {
            self.escritos += 1;
            QuadroNoTempo { pts_100ns: p, duracao_100ns: d.max(fim_minimo_100ns - p), carga: c }
        })
    }

    /// O fim: o quadro na mão sai com a duração do anterior.
    pub fn terminar(&mut self) -> Option<QuadroNoTempo<T>> {
        let d = self.ultima_duracao.max(1);
        self.na_mao.take().map(|(p, c)| {
            self.escritos += 1;
            QuadroNoTempo { pts_100ns: p, duracao_100ns: d, carga: c }
        })
    }
}

/// **A porta do começo (a bancada de 24/09, passo G)**: o escritor levou 4 s para abrir, e a entrada
/// já estava pendurada no dono — oito quadros de antes entraram na reserva, o zero caiu no pedido e o
/// nono quadro veio 3,9 s depois. O arquivo saiu sem esse buraco no vídeo e com ele no som (+4,65 s no
/// fim; no passo H, 1,5 s de abertura deram +1,29 s). A porta abre quando o escritor está pronto
/// (`aberta_em_us`, no relógio do dono; 0 = fechada), e **só passa quadro e som capturados dali em
/// diante**: o zero das duas trilhas é o primeiro quadro depois da porta.
pub fn porta_deixa(aberta_em_us: u64, carimbo_us: u64) -> bool {
    aberta_em_us != 0 && carimbo_us >= aberta_em_us
}

/// O teto das repetições de um quadro num buraco (um minuto a 30 fps); passando disso, os pedaços
/// ficam mais longos que um quadro.
pub const TETO_DE_REPETICOES: i64 = 1800;

/// O passo de um quadro no fps nominal, em 100 ns.
pub fn passo_do_fps(fps: u32) -> i64 {
    10_000_000 / i64::from(fps.max(1))
}

/// **Um buraco no vídeo vira o mesmo quadro repetido** (a bancada de 24/09): o arquivo de 10 min tinha
/// o maior intervalo de 48,4 ms na saída contra 3871 ms na entrada, e o vídeo terminou 4,65 s antes do
/// som (0,78 s além do buraco do começo). Hipótese (não medida isolada): o encoder da Intel, ou o
/// sink, aperta cada intervalo a ~1,45 quadro. Repartido em pedaços de ~1 quadro, o arquivo não
/// depende disso. Devolve `(pts, duração)` de cada pedaço, em 100 ns, emendados e somando `duracao`.
pub fn repartir(pts: i64, duracao: i64, passo: i64) -> Vec<(i64, i64)> {
    let passo = passo.max(1);
    if duracao <= passo + passo / 2 {
        return vec![(pts, duracao)];
    }
    let k = ((duracao + passo / 2) / passo).clamp(1, TETO_DE_REPETICOES);
    (0..k)
        .map(|i| {
            let a = pts + duracao * i / k;
            let b = pts + duracao * (i + 1) / k;
            (a, b - a)
        })
        .collect()
}

/// **O buraco na grade**: o quadro que cobre várias vagas sai uma amostra por vaga, cada uma no PTS
/// exato da vaga (`pts_da_vaga`), até [`TETO_DE_REPETICOES`]; passando disso, a última cobre o resto.
/// `pts` e `duracao` já caem na grade (a `LinhaDoVideo::em_grade`).
pub fn fatiar_na_grade(pts: i64, duracao: i64, fps: u32) -> Vec<(i64, i64)> {
    let fim = pts + duracao;
    let primeira = vaga_de((pts.max(0) / 10) as u64, fps);
    let ultima = vaga_de((fim.max(0) / 10) as u64, fps).max(primeira + 1);
    let n = (ultima - primeira).min(TETO_DE_REPETICOES as u64);
    let mut v: Vec<(i64, i64)> = (0..n)
        .map(|i| {
            let a = pts_da_vaga(primeira + i, fps);
            let b = pts_da_vaga(primeira + i + 1, fps);
            (a, b - a)
        })
        .collect();
    if let Some(u) = v.last_mut() {
        u.1 = fim - u.0;
    }
    v
}

/// **A quarentena da reserva do gravador, no relógio** (a revisão do código, B1): a textura devolvida
/// (`devolvida_em_us`, 0 = nunca usada) volta a ser escrita `quarentena_us` depois. Contada em quadros
/// copiados, ela travava: com todas devolvidas juntas, nenhuma cópia a fazia andar.
pub fn fora_da_quarentena(devolvida_em_us: u64, agora_us: u64, quarentena_us: u64) -> bool {
    devolvida_em_us == 0 || agora_us.saturating_sub(devolvida_em_us) >= quarentena_us
}

// =============================================================================================
// A régua do som
// =============================================================================================

/// A taxa do PCM do gravador (a do preset do microfone, e a do AAC).
pub const TAXA_DO_SOM: u64 = 48_000;
/// Até esta distância (10 ms) entre onde o som chega e onde a régua está, ele é emendado como veio:
/// é o jitter da captura. Acima, silêncio (buraco) ou corte (sobreposição).
pub const TOLERANCIA_DA_REGUA: u64 = 480;
/// Sem som há mais do que isto atrás do vídeo (350 ms), a régua recebe silêncio até lá: é o que dá
/// ao arquivo a trilha de som desde o começo com o microfone desligado (o iOS, §8.7).
pub const FOLGA_ANTES_DO_SILENCIO: u64 = 16_800;
/// O som que espera o vídeo andar (a comporta) tem teto: 10 s. Além disso é descartado e contado.
pub const TETO_DA_ESPERA: usize = 480_000;

/// Um pedaço de PCM mono para o escritor: a posição da primeira amostra na régua e as amostras.
#[derive(Debug, Clone, PartialEq)]
pub struct PedacoDeSom {
    pub posicao: u64,
    pub amostras: Vec<i16>,
}

impl PedacoDeSom {
    /// O PTS em 100 ns.
    pub fn pts_100ns(&self) -> i64 {
        (self.posicao as i128 * 10_000_000 / TAXA_DO_SOM as i128) as i64
    }
    /// A duração em 100 ns (a diferença de dois PTS arredondados, para os pedaços emendarem).
    pub fn duracao_100ns(&self) -> i64 {
        let fim = ((self.posicao + self.amostras.len() as u64) as i128 * 10_000_000 / TAXA_DO_SOM as i128) as i64;
        fim - self.pts_100ns()
    }
}

/// **A régua**: a amostra *k* é o instante `zero + k/48000`. O som do microfone cai onde o carimbo
/// manda; o que já foi entregue ao escritor não volta (o AAC não tem volta), e **nada passa do
/// limite** que o gravador dá — o instante do último quadro já escrito.
#[derive(Debug, Default)]
pub struct ReguaDoSom {
    zero_us: u64,
    /// Amostras já entregues ao escritor: a posição do começo da espera.
    entregue: u64,
    /// O som contínuo a partir de `entregue`, esperando o vídeo andar.
    espera: Vec<i16>,
    pub silencio_inserido: u64,
    pub cortadas_por_sobreposicao: u64,
    pub antes_do_zero: u64,
    pub descartadas_pelo_teto: u64,
    pub lacunas: u64,
    pub recebidas: u64,
    pub cortadas_no_fim: u64,
}

impl ReguaDoSom {
    /// A régua começa no zero do arquivo (o carimbo do primeiro quadro, em µs no relógio do dono).
    pub fn nova(zero_us: u64) -> ReguaDoSom {
        ReguaDoSom { zero_us, ..Default::default() }
    }

    /// O zero da régua (o carimbo do primeiro quadro, em µs no relógio do dono).
    pub fn zero_us(&self) -> u64 {
        self.zero_us
    }

    /// Onde o som conhecido termina (posição na régua).
    pub fn fim_conhecido(&self) -> u64 {
        self.entregue + self.espera.len() as u64
    }

    /// A posição na régua de um instante em µs no relógio do dono (arredondada); negativa antes do zero.
    pub fn posicao_de(&self, carimbo_us: u64) -> i64 {
        let d = carimbo_us as i128 - self.zero_us as i128;
        (d * TAXA_DO_SOM as i128 + 500_000).div_euclid(1_000_000) as i64
    }

    /// Um quadro do microfone: `carimbo_us` é a hora da primeira amostra, no relógio do dono.
    pub fn som(&mut self, carimbo_us: u64, amostras: &[i16]) {
        self.recebidas += amostras.len() as u64;
        let mut p = self.posicao_de(carimbo_us);
        let mut a = amostras;
        if p < 0 {
            let fora = ((-p) as usize).min(a.len());
            self.antes_do_zero += fora as u64;
            a = &a[fora..];
            p = 0;
        }
        if a.is_empty() {
            return;
        }
        let p = p as u64;
        let fim = self.fim_conhecido();
        if p > fim + TOLERANCIA_DA_REGUA {
            // Um buraco: silêncio até onde o som novo começa.
            let n = (p - fim) as usize;
            self.silencio_inserido += n as u64;
            self.lacunas += 1;
            self.espera.extend(std::iter::repeat(0i16).take(n));
        } else if p + TOLERANCIA_DA_REGUA < fim {
            // Sobreposição: o começo do som novo cai em cima do que já existe, e sai.
            let sobra = ((fim - p) as usize).min(a.len());
            self.cortadas_por_sobreposicao += sobra as u64;
            a = &a[sobra..];
        }
        self.espera.extend_from_slice(a);
        if self.espera.len() > TETO_DA_ESPERA {
            let excesso = self.espera.len() - TETO_DA_ESPERA;
            self.espera.truncate(TETO_DA_ESPERA);
            self.descartadas_pelo_teto += excesso as u64;
        }
    }

    /// O vídeo chegou a `limite_us` (o instante do último quadro escrito, no relógio do dono):
    /// entrega o som até lá, completando com silêncio o que estiver mais de 350 ms atrás.
    pub fn liberar(&mut self, limite_us: u64) -> Option<PedacoDeSom> {
        let limite = self.posicao_de(limite_us).max(0) as u64;
        if limite > self.fim_conhecido() + FOLGA_ANTES_DO_SILENCIO {
            let n = (limite - FOLGA_ANTES_DO_SILENCIO - self.fim_conhecido()) as usize;
            self.silencio_inserido += n as u64;
            self.espera.extend(std::iter::repeat(0i16).take(n));
        }
        self.entregar_ate(limite)
    }

    /// O fim: silêncio até o fim do último quadro, tudo entregue até lá, e o que passou disso sai.
    pub fn terminar(&mut self, fim_do_video_us: u64) -> Option<PedacoDeSom> {
        let limite = self.posicao_de(fim_do_video_us).max(0) as u64;
        if limite > self.fim_conhecido() {
            let n = (limite - self.fim_conhecido()) as usize;
            self.silencio_inserido += n as u64;
            self.espera.extend(std::iter::repeat(0i16).take(n));
        }
        let pedaco = self.entregar_ate(limite);
        self.cortadas_no_fim += self.espera.len() as u64;
        self.espera.clear();
        pedaco
    }

    fn entregar_ate(&mut self, limite: u64) -> Option<PedacoDeSom> {
        if limite <= self.entregue || self.espera.is_empty() {
            return None;
        }
        let n = ((limite - self.entregue) as usize).min(self.espera.len());
        let amostras: Vec<i16> = self.espera.drain(..n).collect();
        let pedaco = PedacoDeSom { posicao: self.entregue, amostras };
        self.entregue += n as u64;
        Some(pedaco)
    }

    /// Quantas amostras já foram entregues ao escritor.
    pub fn entregue(&self) -> u64 {
        self.entregue
    }

    /// Uma linha para o registro.
    pub fn linha(&self) -> String {
        format!(
            "som: entregue={:.1} s silencio={:.1} s lacunas={} sobreposicao_cortada={} antes_do_zero={} teto={} cortadas_no_fim={}",
            self.entregue as f64 / TAXA_DO_SOM as f64,
            self.silencio_inserido as f64 / TAXA_DO_SOM as f64,
            self.lacunas,
            self.cortadas_por_sobreposicao,
            self.antes_do_zero,
            self.descartadas_pelo_teto,
            self.cortadas_no_fim
        )
    }
}

// =============================================================================================
// O pedido do controle (§13 do contrato)
// =============================================================================================

/// Onde o gravador está, para a decisão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaseDoGravador {
    Parado,
    /// Pedido, e o primeiro quadro ainda não entrou no arquivo.
    Abrindo,
    Gravando,
    /// O arquivo está fechando.
    Fechando,
}

/// O que a tela faz com o pedido aberto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisaoDoPedido {
    /// Nada agora (o mesmo pedido já tratado, ou o gravador no meio de uma mudança que responde
    /// sozinha quando acabar).
    Nada,
    /// Já está como se pede: responder com `definir_gravando(gravar)`.
    Aceitar(bool),
    /// Tentar começar (a falha vira recusa com o `n`).
    Comecar,
    /// Parar (a resposta é o arquivo fechando).
    Parar,
}

/// **A decisão**, relida a cada bit `GRAVACAO` e a cada mudança do gravador. `ja_tentado` é o `n`
/// do último pedido que a tela já mandou começar ou parar: não se tenta de novo em laço.
pub fn decidir_pedido(n: u64, gravar: bool, fase: FaseDoGravador, ja_tentado: Option<u64>) -> DecisaoDoPedido {
    use DecisaoDoPedido::*;
    use FaseDoGravador::*;
    match (gravar, fase) {
        (true, Gravando) => Aceitar(true),
        (false, Parado) => Aceitar(false),
        // O arquivo abrindo responde quando o primeiro quadro entrar; o fechando, quando fechar.
        (true, Abrindo) | (false, Fechando) => Nada,
        // Gravar com o arquivo fechando: espera fechar e então começa (o iOS, §8.7).
        (true, Fechando) => Nada,
        (true, Parado) => {
            if ja_tentado == Some(n) {
                Nada
            } else {
                Comecar
            }
        }
        (false, Gravando) | (false, Abrindo) => {
            if ja_tentado == Some(n) {
                Nada
            } else {
                Parar
            }
        }
    }
}

/// O motivo da recusa cortado a 256 bytes numa fronteira de caractere, e nunca vazio (o núcleo recusa
/// motivo vazio ou longo com `INVALID`, e o controle ficaria sem resposta).
pub fn motivo_para_o_fio(motivo: &str) -> String {
    const TETO: usize = 256;
    let limpo: String = motivo.chars().filter(|c| *c != '\0').collect();
    let limpo = if limpo.trim().is_empty() { crate::idioma::t("não deu para gravar").to_string() } else { limpo };
    if limpo.len() <= TETO {
        return limpo;
    }
    let mut corte = TETO;
    while !limpo.is_char_boundary(corte) {
        corte -= 1;
    }
    limpo[..corte].to_string()
}

// =============================================================================================
// Os nomes, e os órfãos
// =============================================================================================

/// O sufixo do arquivo enquanto grava: um órfão com ele é de um processo que morreu gravando.
pub const SUFIXO_GRAVANDO: &str = ".gravando.mp4";

/// A base do nome de uma gravação: `Quall-AAAAMMDD-HHMMSS`.
pub fn base_do_nome(ano: u32, mes: u32, dia: u32, hora: u32, minuto: u32, segundo: u32) -> String {
    format!("Quall-{ano:04}{mes:02}{dia:02}-{hora:02}{minuto:02}{segundo:02}") // i18n: fora (nome de arquivo)
}

/// O nome final de uma gravação que fechou: `base.mp4`, ou `base (2).mp4` se já existir.
pub fn nome_final(base: &str, existe: &dyn Fn(&str) -> bool) -> String {
    let primeiro = format!("{base}.mp4");
    if !existe(&primeiro) {
        return primeiro;
    }
    (2..1000).map(|i| format!("{base} ({i}).mp4")).find(|n| !existe(n)).unwrap_or_else(|| format!("{base} (mais).mp4"))
}

/// O nome de um órfão recuperado: `base (interrompido).mp4`.
pub fn nome_do_interrompido(nome_gravando: &str, existe: &dyn Fn(&str) -> bool) -> Option<String> {
    let base = nome_gravando.strip_suffix(SUFIXO_GRAVANDO)?;
    let primeiro = format!("{base} (interrompido).mp4");
    if !existe(&primeiro) {
        return Some(primeiro);
    }
    (2..1000).map(|i| format!("{base} (interrompido {i}).mp4")).find(|n| !existe(n))
}

/// **Onde cortar um MP4 fragmentado órfão**: o fim da última caixa de nível zero **inteira**, e um
/// `moof` que ficou sem o `mdat` dele também sai. `tamanho` é o do arquivo; `ler(posicao)` devolve os
/// 16 bytes do cabeçalho da caixa naquela posição (ou menos, no fim do arquivo). Devolve `None`
/// quando o arquivo não começa por uma caixa legível (não é MP4, ou morreu antes de qualquer caixa).
pub fn corte_do_orfao(tamanho: u64, ler: &mut dyn FnMut(u64) -> Vec<u8>) -> Option<u64> {
    let mut pos = 0u64;
    let mut ultimo_inteiro: Option<u64> = None;
    // O fim da última caixa que não é um `moof` à espera do `mdat`.
    let mut fim_seguro: Option<u64> = None;
    let mut moof_pendente = false;
    while pos + 8 <= tamanho {
        let h = ler(pos);
        if h.len() < 8 {
            break;
        }
        let tam32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        let tipo = [h[4], h[5], h[6], h[7]];
        let tam = match tam32 {
            0 => tamanho - pos, // até o fim do arquivo
            1 => {
                if h.len() < 16 {
                    break;
                }
                u64::from_be_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]])
            }
            t => t,
        };
        if tam < 8 || !tipo.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
            break;
        }
        let fim = pos + tam;
        if fim > tamanho {
            break;
        }
        ultimo_inteiro = Some(fim);
        match &tipo {
            b"moof" => moof_pendente = true,
            b"mdat" => {
                moof_pendente = false;
                fim_seguro = Some(fim);
            }
            _ => {
                if !moof_pendente {
                    fim_seguro = Some(fim);
                }
            }
        }
        pos = fim;
    }
    ultimo_inteiro?;
    fim_seguro
}

/// Quanto tempo de vídeo, no mínimo, um pendente precisa ter para ser guardado (o resto é lixo de um
/// processo que morreu antes do primeiro fragmento).
pub const PENDENTE_MINIMO: Duration = Duration::from_millis(0);

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn a_taxa_e_a_do_android() {
        assert_eq!(taxa_da_gravacao(1920, 1080, 30), 14_000_000);
        assert_eq!(taxa_da_gravacao(1280, 720, 30), 6_222_222);
        assert_eq!(taxa_da_gravacao(3840, 2160, 30), 56_000_000);
        let t60 = taxa_da_gravacao(1920, 1080, 60);
        assert!((20_900_000..21_100_000).contains(&t60), "1080p60 ≈ 21 Mbit/s: {t60}");
        assert_eq!(taxa_da_gravacao(320, 240, 15), TAXA_MINIMA);
        assert_eq!(taxa_da_gravacao(3840, 2160, 60), TAXA_MAXIMA);
        assert_eq!(fps_nominal(29.97), 30);
        assert_eq!(fps_nominal(0.0), 30);
        assert_eq!(fps_nominal(120.0), 60);
    }

    #[test]
    fn a_quarentena_anda_com_o_relogio_e_nao_com_as_copias() {
        // B1: as oito devolvidas no mesmo instante, e nenhuma cópia depois — saem pelo relógio.
        let devolvidas = [5_000u64; 8];
        assert!(devolvidas.iter().all(|d| !fora_da_quarentena(*d, 5_000 + 69_999, 70_000)));
        assert!(devolvidas.iter().all(|d| fora_da_quarentena(*d, 5_000 + 70_000, 70_000)));
        assert!(fora_da_quarentena(0, 0, 70_000), "nunca usada: livre");
    }

    #[test]
    fn o_ultimo_quadro_cobre_o_som_da_pausa() {
        let mut l: LinhaDoVideo<u8> = LinhaDoVideo::default();
        let _ = l.chegou(0, 1);
        let _ = l.chegou(33_333, 2);
        // o som foi até 2 s numa pausa da câmera: o último quadro dura até lá
        let q = l.terminar_ate(20_000_000).unwrap();
        assert_eq!(q.pts_100ns + q.duracao_100ns, 20_000_000);
        let mut l: LinhaDoVideo<u8> = LinhaDoVideo::default();
        let _ = l.chegou(0, 1);
        let _ = l.chegou(33_333, 2);
        let q = l.terminar_ate(0).unwrap();
        assert_eq!(q.duracao_100ns, 333_330, "sem som além: a duração do anterior");
    }

    #[test]
    fn o_video_leva_o_carimbo_real_e_a_duracao_ate_o_seguinte() {
        let mut l: LinhaDoVideo<u32> = LinhaDoVideo::default();
        assert_eq!(l.chegou(1_000_000, 1), Ok(None), "o primeiro fica na mão");
        let q = l.chegou(1_033_333, 2).unwrap().unwrap();
        assert_eq!((q.pts_100ns, q.duracao_100ns, q.carga), (0, 333_330, 1));
        // um buraco de 133 ms fica do tamanho que teve
        let q = l.chegou(1_166_666, 3).unwrap().unwrap();
        assert_eq!((q.pts_100ns, q.duracao_100ns), (333_330, 1_333_330));
        assert_eq!(l.maior_buraco_100ns, 1_333_330);
        // um carimbo que não anda é recusado, e o na mão continua
        assert_eq!(l.chegou(1_166_666, 4), Err(4));
        assert_eq!(l.chegou(900_000, 5), Err(5), "antes do zero");
        assert_eq!(l.recusados_por_carimbo, 2);
        let q = l.terminar().unwrap();
        assert_eq!((q.pts_100ns, q.duracao_100ns, q.carga), (1_666_660, 1_333_330, 3));
        assert_eq!(l.escritos, 3);
        assert!(l.terminar().is_none());
    }

    fn tom(n: usize, v: i16) -> Vec<i16> {
        vec![v; n]
    }

    #[test]
    fn o_som_cai_onde_o_carimbo_manda_e_nunca_passa_do_video() {
        let mut r = ReguaDoSom::nova(1_000_000);
        // 20 ms de som a partir de 5 ms depois do zero: 240 amostras de antes viram... nada (dentro
        // da tolerância, emendado como veio)
        r.som(1_005_000, &tom(960, 7));
        assert_eq!(r.fim_conhecido(), 960, "dentro da tolerância, emenda em 0");
        // o vídeo está em 10 ms: sai só até lá (a comporta)
        let p = r.liberar(1_010_000).unwrap();
        assert_eq!((p.posicao, p.amostras.len()), (0, 480));
        // o vídeo em 30 ms: sai o resto conhecido (até 960), e nada inventado
        let p = r.liberar(1_030_000).unwrap();
        assert_eq!((p.posicao, p.amostras.len()), (480, 480));
        assert!(r.liberar(1_030_000).is_none());
        // um buraco de 100 ms vira silêncio
        r.som(1_120_000, &tom(960, 9));
        assert_eq!(r.lacunas, 1);
        assert_eq!(r.silencio_inserido, (120 * 48 - 960) as u64);
        // sobreposição: o som que cai em cima do que já existe perde o começo
        let antes = r.fim_conhecido();
        r.som(1_120_000, &tom(960, 3));
        assert_eq!(r.cortadas_por_sobreposicao, 960);
        assert_eq!(r.fim_conhecido(), antes);
    }

    #[test]
    fn sem_microfone_a_regua_recebe_silencio_com_folga() {
        let mut r = ReguaDoSom::nova(0);
        // 1 s de vídeo sem som nenhum: silêncio até 350 ms atrás, e entregue até lá
        let p = r.liberar(1_000_000).unwrap();
        assert_eq!(p.amostras.len() as u64, 48_000 - FOLGA_ANTES_DO_SILENCIO);
        assert!(p.amostras.iter().all(|a| *a == 0));
        // o microfone liga: o som que chega depois do silêncio entra (o que cai em cima sai)
        r.som(900_000, &tom(960, 5));
        let p = r.liberar(1_000_000).unwrap();
        assert!(p.amostras.iter().any(|a| *a == 5));
    }

    #[test]
    fn o_fim_completa_com_silencio_e_corta_o_que_passa() {
        let mut r = ReguaDoSom::nova(0);
        r.som(0, &tom(4800, 1)); // 100 ms
        let p = r.terminar(50_000).unwrap(); // o vídeo acaba em 50 ms
        assert_eq!(p.amostras.len(), 2400);
        assert_eq!(r.cortadas_no_fim, 2400);
        let mut r = ReguaDoSom::nova(0);
        r.som(0, &tom(960, 1));
        let p = r.terminar(100_000).unwrap();
        assert_eq!(p.amostras.len(), 4800, "completa com silêncio até o fim do vídeo");
        assert_eq!(r.entregue(), 4800);
    }

    #[test]
    fn o_som_antes_do_zero_cai() {
        let mut r = ReguaDoSom::nova(1_000_000);
        r.som(990_000, &tom(960, 1)); // 10 ms antes: 480 amostras caem
        assert_eq!(r.antes_do_zero, 480);
        assert_eq!(r.fim_conhecido(), 480);
    }

    #[test]
    fn os_pedacos_emendam_no_tempo() {
        let a = PedacoDeSom { posicao: 0, amostras: vec![0; 1024] };
        let b = PedacoDeSom { posicao: 1024, amostras: vec![0; 1024] };
        assert_eq!(a.pts_100ns() + a.duracao_100ns(), b.pts_100ns());
    }

    #[test]
    fn a_decisao_do_pedido() {
        use DecisaoDoPedido::*;
        use FaseDoGravador::*;
        assert_eq!(decidir_pedido(5, true, Parado, None), Comecar);
        assert_eq!(decidir_pedido(5, true, Parado, Some(5)), Nada, "não tenta de novo em laço");
        assert_eq!(decidir_pedido(6, true, Parado, Some(5)), Comecar);
        assert_eq!(decidir_pedido(5, true, Gravando, None), Aceitar(true));
        assert_eq!(decidir_pedido(5, true, Abrindo, None), Nada);
        assert_eq!(decidir_pedido(5, true, Fechando, None), Nada);
        assert_eq!(decidir_pedido(7, false, Gravando, None), Parar);
        assert_eq!(decidir_pedido(7, false, Abrindo, None), Parar);
        assert_eq!(decidir_pedido(7, false, Fechando, None), Nada);
        assert_eq!(decidir_pedido(7, false, Parado, None), Aceitar(false));
    }

    #[test]
    fn o_motivo_cabe_no_fio() {
        assert_eq!(motivo_para_o_fio(""), "não deu para gravar");
        assert_eq!(motivo_para_o_fio("a\0b"), "ab");
        let longo = "ç".repeat(200); // 400 bytes
        let m = motivo_para_o_fio(&longo);
        assert!(m.len() <= 256 && m.chars().all(|c| c == 'ç'));
    }

    /// Em inglês (a tradução EN/PT): a recusa por espaço e o marcador do "sem som" nos dois idiomas.
    #[test]
    fn os_textos_em_ingles() {
        crate::idioma::com_idioma(crate::idioma::Idioma::En, || {
            assert_eq!(texto_sem_espaco(100 * 1024 * 1024, 500 * 1024 * 1024), "not enough space: 100 MB left (recording needs 500 MB free)");
            assert_eq!(motivo_para_o_fio(""), "couldn't record");
        });
        assert_eq!(texto_sem_espaco(100 * 1024 * 1024, 500 * 1024 * 1024), "sem espaço: sobram 100 MB (gravar pede 500 MB livres)");
        assert!(diz_sem_som("● GRAVANDO 0:12 · Gravando SEM SOM — ligue o microfone"));
        assert!(diz_sem_som("● RECORDING 0:12 · Recording with NO AUDIO — turn on the microphone"));
        assert!(!diz_sem_som("● GRAVANDO 0:12 · sobram 41,2 GB"));
    }

    #[test]
    fn os_nomes() {
        let b = base_do_nome(2026, 9, 25, 7, 3, 9);
        assert_eq!(b, "Quall-20260925-070309");
        let nenhum = |_: &str| false;
        assert_eq!(nome_final(&b, &nenhum), "Quall-20260925-070309.mp4");
        let ja = |n: &str| n == "Quall-20260925-070309.mp4";
        assert_eq!(nome_final(&b, &ja), "Quall-20260925-070309 (2).mp4");
        assert_eq!(
            nome_do_interrompido("Quall-20260925-070309.gravando.mp4", &nenhum).as_deref(),
            Some("Quall-20260925-070309 (interrompido).mp4")
        );
        assert_eq!(nome_do_interrompido("outro.mp4", &nenhum), None);
    }

    fn caixa(tipo: &[u8; 4], corpo: usize) -> Vec<u8> {
        let mut v = ((corpo + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(tipo);
        v.extend(std::iter::repeat(0u8).take(corpo));
        v
    }

    fn cortar(arquivo: &[u8]) -> Option<u64> {
        let mut ler = |p: u64| arquivo[p as usize..(p as usize + 16).min(arquivo.len())].to_vec();
        corte_do_orfao(arquivo.len() as u64, &mut ler)
    }

    #[test]
    fn o_orfao_perde_so_o_fragmento_incompleto() {
        let mut a = caixa(b"ftyp", 16);
        a.extend(caixa(b"moov", 100));
        let depois_do_moov = a.len() as u64;
        a.extend(caixa(b"moof", 40));
        a.extend(caixa(b"mdat", 1000));
        let fragmento_1 = a.len() as u64;
        assert_eq!(cortar(&a), Some(fragmento_1), "inteiro: nada a cortar");
        // um moof sem o mdat dele sai
        a.extend(caixa(b"moof", 40));
        assert_eq!(cortar(&a), Some(fragmento_1));
        // um mdat cortado no meio sai, com o moof dele
        let mut mdat = caixa(b"mdat", 1000);
        mdat.truncate(300);
        a.extend(mdat);
        assert_eq!(cortar(&a), Some(fragmento_1));
        // um arquivo que morreu antes do primeiro fragmento guarda só as caixas de cabeçalho
        let mut b = caixa(b"ftyp", 16);
        b.extend(caixa(b"moov", 100));
        b.extend(caixa(b"moof", 40));
        assert_eq!(cortar(&b), Some(depois_do_moov));
        // lixo não é MP4
        assert_eq!(cortar(&[0xFF; 64]), None);
        assert_eq!(cortar(&[]), None);
    }

    // ---- a bancada de 24/09: um zero só para as duas trilhas ----

    /// O encoder da hipótese: cada intervalo de PTS na saída fica em no máximo `teto` (o maior da
    /// bancada foi 48,4 ms). Devolve o fim do vídeo no arquivo, em 100 ns.
    fn fim_no_encoder_que_aperta(entradas: &[(i64, i64)], teto: i64) -> i64 {
        let mut fim = 0i64;
        let mut saida = 0i64;
        for (i, (pts, dur)) in entradas.iter().enumerate() {
            if i > 0 {
                saida += (pts - entradas[i - 1].0).min(teto);
            } else {
                saida = *pts;
            }
            fim = saida + (*dur).min(teto);
        }
        fim
    }

    /// A thread do gravador, pura: quadros e som (carimbos em µs; som em blocos de 10 ms), o instante
    /// em que o escritor ficou pronto, a porta (sem ela, o comportamento de antes: a reserva de 8
    /// guardava os quadros de antes de o escritor abrir, e o som de antes passava), e se o buraco é
    /// repartido. Devolve (som − vídeo no fim do arquivo, em ms; o maior intervalo de entrada no
    /// encoder, em ms).
    fn gravar(pronto_us: u64, com_porta: bool, com_repartir: bool, quadros: &[u64], som: &[u64]) -> (f64, f64) {
        #[derive(Clone, Copy)]
        enum E {
            Q,
            S,
        }
        let mut eventos: Vec<(u64, E)> = quadros.iter().map(|c| (*c, E::Q)).chain(som.iter().map(|c| (*c, E::S))).collect();
        eventos.sort_by_key(|(c, _)| *c);
        let passo = passo_do_fps(30);
        let porta = if com_porta { pronto_us.max(1) } else { 0 };
        let mut antes_na_reserva = 0;
        let mut linha: LinhaDoVideo<()> = LinhaDoVideo::default();
        let mut regua: Option<ReguaDoSom> = None;
        let mut som_antes: Vec<u64> = Vec::new();
        let mut entradas: Vec<(i64, i64)> = Vec::new();
        let pedacos = |q: &QuadroNoTempo<()>| {
            if com_repartir {
                repartir(q.pts_100ns, q.duracao_100ns, passo)
            } else {
                vec![(q.pts_100ns, q.duracao_100ns)]
            }
        };
        for (c, e) in eventos {
            let passa = if com_porta {
                porta_deixa(porta, c)
            } else {
                match e {
                    E::Q if c < pronto_us => {
                        antes_na_reserva += 1;
                        antes_na_reserva <= 8
                    }
                    _ => true,
                }
            };
            if !passa {
                continue;
            }
            match e {
                E::Q => {
                    let primeiro = linha.zero_us().is_none();
                    let saiu = linha.chegou(c, ()).unwrap();
                    if primeiro {
                        let mut r = ReguaDoSom::nova(c);
                        for s in som_antes.drain(..) {
                            r.som(s, &[1; 480]);
                        }
                        regua = Some(r);
                    }
                    if let Some(q) = saiu {
                        let fim = q.pts_100ns + q.duracao_100ns;
                        entradas.extend(pedacos(&q));
                        let r = regua.as_mut().unwrap();
                        let limite = r.zero_us() + fim as u64 / 10;
                        let _ = r.liberar(limite);
                    }
                }
                E::S => match regua.as_mut() {
                    Some(r) => r.som(c, &[1; 480]),
                    None => som_antes.push(c),
                },
            }
        }
        let q = linha.terminar().unwrap();
        let fim = q.pts_100ns + q.duracao_100ns;
        entradas.extend(pedacos(&q));
        let r = regua.as_mut().unwrap();
        let limite = r.zero_us() + fim as u64 / 10;
        let _ = r.terminar(limite);
        let fim_do_som = (r.entregue() * 10_000_000 / TAXA_DO_SOM) as i64;
        let fim_do_video = fim_no_encoder_que_aperta(&entradas, 484_000);
        let maior = entradas.windows(2).map(|w| w[1].0 - w[0].0).max().unwrap_or(0);
        ((fim_do_som - fim_do_video) as f64 / 10_000.0, maior as f64 / 10_000.0)
    }

    fn quadros_a_30(de_us: u64, ate_us: u64) -> Vec<u64> {
        (0..).map(|i| de_us + i * 33_333).take_while(|c| *c < ate_us).collect()
    }

    fn som_a_cada_10ms(ate_us: u64) -> Vec<u64> {
        (0..).map(|i| i * 10_000).take_while(|c| *c < ate_us).collect()
    }

    #[test]
    fn um_zero_so_para_as_duas_trilhas() {
        // O passo G: a câmera entregando desde o pedido (0), o escritor pronto em 4 s, 20 s gravados.
        let quadros = quadros_a_30(0, 24_000_000);
        let som = som_a_cada_10ms(24_000_000);
        // Antes (sem porta, sem repartir): o som termina segundos depois da imagem — o defeito.
        let (dif, _) = gravar(4_000_000, false, false, &quadros, &som);
        assert!(dif > 3_500.0, "o comportamento de antes reproduz a bancada: {dif} ms");
        // Com a porta: um zero só, e o fim junto (a tolerância do fase3-ffprobe é 50 ms).
        let (dif, maior) = gravar(4_000_000, true, true, &quadros, &som);
        assert!(dif.abs() < 50.0, "som − vídeo no fim: {dif} ms");
        assert!(maior < 50.0, "nenhum intervalo acima de 1,5 quadro no encoder: {maior} ms");
        // A porta sozinha já resolve o começo.
        let (dif, _) = gravar(4_000_000, true, false, &quadros, &som);
        assert!(dif.abs() < 50.0, "só a porta: {dif} ms");
    }

    #[test]
    fn o_buraco_no_meio_vira_quadro_repetido() {
        // A câmera some 2 s no meio (a pausa do M4): sem repartir, o encoder que aperta come o buraco.
        let mut quadros = quadros_a_30(0, 5_000_000);
        quadros.extend(quadros_a_30(7_000_000, 12_000_000));
        let som = som_a_cada_10ms(12_000_000);
        let (dif, _) = gravar(0, true, false, &quadros, &som);
        assert!(dif > 1_500.0, "sem repartir, o vídeo encolhe: {dif} ms");
        let (dif, maior) = gravar(0, true, true, &quadros, &som);
        assert!(dif.abs() < 50.0, "repartido: {dif} ms");
        assert!(maior < 50.0, "{maior} ms");
    }

    #[test]
    fn o_quadro_que_a_camera_perde_nao_encolhe_o_video() {
        // O passo I, gravação #2: escritor quente (203 ms), 90 s sem buraco grande, e o som +130,8 ms.
        // Uma câmera que perde um quadro de vez em quando (66 ms) basta: cada intervalo apertado a
        // 48,4 ms tira ~18 ms do vídeo (hipótese). Sete perdas em 90 s.
        let quadros: Vec<u64> = quadros_a_30(0, 90_500_000).into_iter().enumerate().filter(|(i, _)| i % 385 != 200).map(|(_, c)| c).collect();
        let som = som_a_cada_10ms(90_500_000);
        let (dif, _) = gravar(203_000, false, false, &quadros, &som);
        assert!(dif > 100.0, "o comportamento de antes reproduz a bancada: {dif} ms");
        let (dif, maior) = gravar(203_000, true, true, &quadros, &som);
        assert!(dif.abs() < 50.0, "{dif} ms");
        assert!(maior < 50.0, "{maior} ms");
    }

    #[test]
    fn a_porta_e_o_repartir() {
        assert!(!porta_deixa(0, 5_000_000), "fechada");
        assert!(!porta_deixa(4_000_000, 3_999_999));
        assert!(porta_deixa(4_000_000, 4_000_000));
        let p = passo_do_fps(30);
        assert_eq!(repartir(100, p, p), vec![(100, p)], "um quadro fica um");
        assert_eq!(repartir(100, p * 3 / 2, p).len(), 1, "até 1,5 quadro fica um");
        let v = repartir(1000, 38_710_000, p);
        assert_eq!(v.len(), 116);
        assert_eq!(v[0].0, 1000);
        assert_eq!(v.iter().map(|x| x.1).sum::<i64>(), 38_710_000, "somam a duração");
        assert!(v.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0), "emendados");
        assert!(v.iter().all(|x| x.1 <= p + p / 2));
        assert_eq!(repartir(0, 10_000_000_000, p).len() as i64, TETO_DE_REPETICOES);
    }

    // ---- a bancada de 26/09: o tempo que entra no escritor não acumula ----

    /// A câmera do Dell: o DeviceTimestamp em passos de 16 ms (32/48 alternando), 29,85 fps reais.
    fn carimbos_de_16ms(fps_real: f64, segundos: f64) -> Vec<u64> {
        let n = (fps_real * segundos) as u64;
        (0..n).map(|i| ((i as f64 * 1e6 / fps_real) as u64 / 16_000) * 16_000 + 7_000_000).collect()
    }

    #[test]
    fn o_tempo_do_escritor_nao_acumula_em_dez_minutos() {
        // O carimbo real (o padrão): a soma das durações entregues é o PTS do último, exato, e o fim
        // bate com o último carimbo — a mesma conta que o diário do G mostrou (560,5 s em 560,5 s).
        let c = carimbos_de_16ms(29.85, 600.0);
        let mut l: LinhaDoVideo<()> = LinhaDoVideo::default();
        let mut soma = 0i64;
        let mut primeiro_pts = None;
        for x in &c {
            if let Some(q) = l.chegou(*x, ()).unwrap() {
                primeiro_pts.get_or_insert(q.pts_100ns);
                assert_eq!(q.pts_100ns, soma, "o PTS é a soma das durações anteriores");
                soma += q.duracao_100ns;
                assert!(q.duracao_100ns == 320_000 || q.duracao_100ns == 480_000, "{}", q.duracao_100ns);
            }
        }
        let real = ((c[c.len() - 1] - c[0]) * 10) as i64;
        assert_eq!(soma, real, "sem acúmulo: {soma} contra {real}");
        let q = l.terminar().unwrap();
        assert_eq!(q.pts_100ns, real);
    }

    #[test]
    fn a_grade_nao_acumula_e_fica_a_meia_vaga_do_carimbo() {
        for fps_real in [29.85, 30.2] {
            let c = carimbos_de_16ms(fps_real, 600.0);
            let mut l: LinhaDoVideo<u64> = LinhaDoVideo::em_grade(30);
            let meia = pts_da_vaga(1, 30) / 2 + 1;
            let mut fim = 0i64;
            let mut devolvidos = 0;
            for x in &c {
                match l.chegou(*x, *x) {
                    Ok(Some(q)) => {
                        let real = ((q.carga - c[0]) * 10) as i64;
                        assert!((q.pts_100ns - real).abs() <= meia, "a {} fps: {} contra {}", fps_real, q.pts_100ns, real);
                        let n = vaga_de((q.pts_100ns / 10) as u64, 30);
                        assert_eq!(q.pts_100ns, pts_da_vaga(n, 30), "na grade");
                        assert!(fim == 0 || q.pts_100ns == fim, "emendados");
                        fim = q.pts_100ns + q.duracao_100ns;
                    }
                    Ok(None) => {}
                    Err(_) => devolvidos += 1,
                }
            }
            let real = ((c[c.len() - 1] - c[0]) * 10) as i64;
            assert!((fim - real).abs() <= meia * 2 + pts_da_vaga(1, 30), "a {fps_real} fps o fim {fim} contra {real}");
            if fps_real > 30.0 {
                assert!(l.na_mesma_vaga > 0 && devolvidos as u64 == l.na_mesma_vaga, "mais rápida que a grade: troca");
            }
        }
    }

    #[test]
    fn o_buraco_na_grade_sai_uma_amostra_por_vaga() {
        let a = pts_da_vaga(10, 30);
        let b = pts_da_vaga(14, 30);
        let v = fatiar_na_grade(a, b - a, 30);
        assert_eq!(v.len(), 4);
        for (i, (p, d)) in v.iter().enumerate() {
            assert_eq!(*p, pts_da_vaga(10 + i as u64, 30));
            assert_eq!(p + d, pts_da_vaga(11 + i as u64, 30));
        }
        assert_eq!(fatiar_na_grade(a, pts_da_vaga(11, 30) - a, 30), vec![(a, pts_da_vaga(11, 30) - a)], "uma vaga, uma amostra");
        let v = fatiar_na_grade(0, pts_da_vaga(5000, 30), 30);
        assert_eq!(v.len() as i64, TETO_DE_REPETICOES);
        assert_eq!(v.last().map(|(p, d)| p + d), Some(pts_da_vaga(5000, 30)), "a última cobre o resto");
    }
}
