//! O teto do que qualquer emissor do Quall pode pôr na rede — **derivado do que o SDP anuncia**.
//!
//! # Por que isto mora no núcleo, e não em cada casca
//!
//! O núcleo anuncia, para todo receptor, `profile-level-id=42e034` ([`crate::track::PERFIL_H264`])
//! — Constrained Baseline, **nível 4.0**. Isso é uma **promessa**, e até 2026-08-28 nenhuma casca
//! a cumpria por acordo.
//!
//! Medido em 28/08, quando o anunciado ainda era 3.1, e cada linha é de uma casca diferente:
//!
//! | emissor | o que saiu | nível | cabia no 3.1? | cabe no 4.0? |
//! |---|---|---|---|---|
//! | iOS (tela do iPhone 7) | 720x1280 @30 | 3.1 | sim | sim |
//! | Android (A10s) | 720x1520 @30 | 3.2 | **não** | **sim** |
//! | Windows (Dell) | 1920x1080 | 4.0 | **não** | **sim** |
//! | macOS (MacBook) | 2560x1664 @60 | 5.2 | **não** | **não** |
//!
//! **Em 01/09/2026 o teto subiu de 3.1 para 4.0**, e a última coluna é a mudança: três das quatro
//! cascas passam a caber no que se anuncia, e a tela nativa do A10s e a do Dell atravessam
//! inteiras. A quarta continua fora — subir o teto não é tirar o teto.
//!
//! Três cascas, três formas diferentes de estourar o mesmo contrato, e nenhuma delas por
//! descuido isolado: **ninguém tinha onde perguntar qual era o teto.** Pôr a regra em cada casca
//! produziria quatro tetos e três deles errados — que é exatamente o padrão que este projeto já
//! pagou com o preset de áudio, com o piso do PLI e com a curva µ-law. A regra mora aqui, ao lado
//! da constante que ela serve, e as cascas perguntam.
//!
//! # O custo do excesso não é estético, foi medido
//!
//! `docs/tela-preta.md` §4, §6.1 e §9.3:
//!
//! - 2560x1664@60 rendeu **17,3% de perda** e **470 ms de decode** num A10s que faz 720p30 com
//!   **0,35% e 45 ms**. A perda **é consequência do que pedimos do enlace**, não condição do
//!   ambiente;
//! - o conjunto de parâmetros do Dell a 1080p ocupa **195.495 bytes = 163 pacotes RTP**, e um
//!   pacote perdido condena o quadro inteiro. A 8,6% de perda, `0,914^163 ≈ 5·10⁻⁷`: **nenhum
//!   conjunto chega**, o decodificador nunca começa, e a tela fica preta para sempre.
//!
//! **Esses dois números continuam verdadeiros, e o segundo volta a valer com o teto de 4.0** —
//! por isso ele fica escrito aqui e não some. O que `docs/joelho-da-perda.md` acrescentou depois
//! delimita o estrago:
//!
//! - o domínio dos 163 pacotes é a **abertura fria**, e ali eles reproduzem 69,33% de perda três
//!   vezes seguidas. Não é o regime;
//! - **o quadro-chave de regime tem 15 pacotes**, estável de 12 a 24 fps. A frase "o IDR é o
//!   único objeto grande o bastante para estourar o joelho" morreu com a medida;
//! - quem cobre a abertura é a **quinta porta**: 250 ms de recuperação contra 4,3–6,9 s sem ela.
//!
//! Ou seja: o custo de 1080p é concentrado no **primeiro** conjunto de parâmetros, não no fluxo.
//! Quem for medir regressão desta mudança mede a abertura, e mede num enlace ruim — no cabo, que
//! foi onde 1080p foi justificado, a abertura não tem com o que se preocupar.
//!
//! # Por que a conta é em macroblocos, e não numa caixa de 1280x720
//!
//! Porque **é assim que o nível é definido**. A norma H.264 (Anexo A) não limita largura e
//! altura: limita a **área em macroblocos** (`MaxFS`) e a **taxa de macroblocos por segundo**
//! (`MaxMBPS`). 1280x720 era só o retângulo 16:9 que saturava exatamente o `MaxFS` de 3600 do
//! nível 3.1 — e a 30 fps saturava exatamente o `MaxMBPS` de 108000. Nunca foi um número
//! escolhido: era o ponto em que aquelas duas contas fechavam.
//!
//! No 4.0 as mesmas duas contas fecham em **1080p30**, e por pouco: 1920x1080 são 120x68 = **8160
//! macroblocos** contra `MaxFS` 8192, e a 30 fps são **244800** contra `MaxMBPS` 245760. Sobram
//! 0,4% nas duas. **1080p31 não caberia**, e é por isso que 4.0 é o nível certo em vez de um
//! arredondamento para cima.
//!
//! A diferença aparece em tela alongada, que é precisamente o caso que nos mordeu. O A10s tem
//! 720x1520:
//!
//! - uma caixa de 1280x720 daria **606x1280** — espremido para caber num retângulo escrito para
//!   outra proporção;
//! - o teto em macroblocos dá **[`ajustar`]**, que preenchia os 3600 macroblocos disponíveis na
//!   proporção real da tela.
//!
//! Os dois cabiam em 3.1 e custavam o mesmo ao decodificador — a norma cobra por macrobloco. O
//! segundo entregava mais imagem pelo mesmo preço, e não precisava de exceção para tela em pé.
//!
//! **No 4.0 o A10s deixa de ser um caso**: 720x1520 são 4275 macroblocos, cabem nos 8192, e a
//! tela sai nativa sem passar por [`ajustar`]. O raciocínio em macroblocos continua valendo — é
//! ele que faz a próxima tela alongada caber sem exceção nenhuma.
//!
//! # 1080p exige recorte, e 720p não exigia
//!
//! 1080 / 16 = 67,5. O codificador codifica 68 linhas de macrobloco — **1088 px** — e sinaliza o
//! corte no SPS (`frame_cropping`). Quem não honrar isso mostra 8 linhas de lixo no rodapé. É um
//! caso que **não existia** enquanto o teto era 720p, onde 720 / 16 = 45 exato, e é o que
//! [`Saida::exige_recorte`] passou a marcar para o tamanho mais comum que existe.
//!
//! # O que este teto **não** é
//!
//! **Não é controle de banda.** Ele derruba bitrate, perda e o tamanho do conjunto de parâmetros
//! de uma vez, porque os três dependem do que entra no encoder — mas nada aqui mede o enlace nem
//! se adapta a ele. Teto adaptativo é outra frente.
//!
//! **Não negocia.** O nível sai de [`crate::track::PERFIL_H264`], que é o que **nós** anunciamos;
//! ninguém lê o `profile-level-id` do outro lado para subir o teto quando ele comportaria mais.
//! Esse é o desenho certo a prazo e é **decisão de protocolo** — mudaria o que o `fmtp` diz.
//! Ficou de fora de propósito, e não por dificuldade.

/// Um macrobloco H.264 tem 16x16 pixels. Toda a aritmética de nível é nesta unidade.
pub const LADO_DO_MACROBLOCO: u32 = 16;

/// Taxa de quadros máxima que este produto emite, **independente do que o nível comportaria**.
///
/// É política de produto, não consequência da norma: num tamanho pequeno o nível 4.0 permitiria
/// mais de 200 fps (o 3.1 permitia 90), e nós não queremos.
///
/// # A evidência que estava escrita aqui confundia duas variáveis
///
/// Até 07/09/2026 esta linha dizia que **toda** medição a 60 fps desta bancada terminou em
/// desastre, citando `docs/tela-preta.md` §4 — 17,3 % de perda e 470 ms de decode num A10s.
/// **Aquele braço era 2560x1664**, isto é 16.640 macroblocos: **2,03 vezes** o `MaxFS` de 8192 do
/// nível que se anuncia hoje, e 4,6 vezes o do 3.1 que se anunciava então. A taxa de quadros e a
/// área estavam coladas, e o próprio cabeçalho deste módulo atribui aquele estrago ao que se
/// pediu do enlace. **Não existe nesta bancada uma corrida a 60 fps numa geometria que caiba no
/// nível.** O que está medido é "2560x1664@60 num A10s é desastre"; "60 fps é desastre" é o
/// confundidor falando.
///
/// # O que a aritmética diz, e por que ela sozinha não levanta o número
///
/// 1080p60 custa **18 Mbps** por [`teto_de_taxa`] — o dobro dos 9 de 1080p30 — e **não muda o
/// tamanho do quadro-chave**, que é a variável que quebra IDR nesta bancada: o joelho de
/// truncamento é ~50 pacotes (`docs/idr-que-sobrevive.md`) e o IDR de 1080p mede 102
/// (`docs/bancada.md` §8.47). É a diferença que separa 60 fps de 4K, cujo IDR escala com a área.
///
/// Mas a perda desta bancada cresce com ~carga^2,5 (§8.30: expoentes 2,8 no A07 e 2,4 no A10s),
/// então dobrar a carga prevê ~6x a perda **no rádio**, onde o joelho agregado está entre 32 e
/// 47 Mbps (§8.10). **No fio** §8.13 mediu 75,5 Mbps agregados a 0,023 % com 321 IDR e nenhum
/// quebrado: lá o dobro cabe. Ou seja, 60 fps é decisão de **transporte**, e não de codec.
///
/// # Por que o número é 30 e por que não se levanta *esta constante*
///
/// Porque subir aqui muda **todo** emissor de uma vez, e o quanto está medido: com
/// [`ajustar_com`] no nível 4.2 e teto de 60, a tela do MacBook (2560x1664@60) passa de
/// 1780x1156@30 a 8,93 Mbps para **1834x1192@60 a 18,98 Mbps** — 2,12x a carga, sem ninguém ter
/// pedido, e num emissor cuja captura já é de 60 fps. 720p30, 1080p30 e o 720x1520 do A10s ficam
/// byte a byte iguais, o que torna o salto do MacBook ainda mais fácil de não notar.
///
/// **60 fps tem de ser pedido por sessão**, e [`ajustar_com`] já aceita o teto como argumento:
/// falta um chamador, não um mecanismo. Ver `crates/quall-core/examples/transporte.rs` para a
/// conta completa contra os dois joelhos medidos.
pub const FPS_MAXIMO: u32 = 30;

/// O que o usuário escolheu emitir. **É a régua de cima; o nível é a de baixo.**
///
/// Pedido do usuário em 07/09/2026: *"o Quall tem que permitir o usuário escolher a resolução
/// câmera / tela 720p 1080p 2k 4k"*. Ver `docs/fluxo-de-uso.md`.
///
/// # Por que isto é um `MaxFS` e não um par de dimensões
///
/// Porque a escolha do usuário e o teto da norma são **a mesma grandeza**, e tratá-las como duas
/// coisas obrigaria a combiná-las na mão em cada casca — que é onde este projeto já perdeu um dia
/// (o teto de resolução subiu para 1080p em 01/09 e o de taxa não subiu junto; cinco cascas
/// tinham o número escrito à mão).
///
/// Um alvo em macroblocos entra direto em [`ajustar_com`] como `LimitesDoNivel`, e a regra vira
/// uma linha: **vale o menor dos dois**. Também é o que preserva a proporção — o usuário escolhe
/// "quanta área", e a geometria de saída continua a do aparelho, não um 16:9 forçado. Um telefone
/// em pé pedindo 1080p recebe 1080x1920, e não 1920x1080 deitado.
///
/// # O padrão é o de hoje, de propósito
///
/// [`Alvo::PADRAO`] é 1080p, que é exatamente o teto de 01/09/2026. Subir `PERFIL_H264` para 5.2
/// **sem** isto faria a tela do S24 saltar de 976x2116 para 1440x3120 nativos — 2,3x a carga, sem
/// ninguém ter pedido. A régua de cima existe para que abrir o cardápio não mexa em quem não
/// abrir o menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alvo {
    /// Macroblocos por quadro que o usuário aceita emitir.
    pub max_fs: u32,
    /// Quadros por segundo que ele pediu. **Teto, não piso** — o nível pode cortar.
    ///
    /// Está no mesmo tipo que `max_fs` porque as duas são a mesma decisão: "o que eu quero
    /// emitir". Separá-las levaria cada casca a combinar as duas na mão, e é aí que este projeto
    /// já perdeu um dia — o teto de resolução subiu em 01/09 e o de taxa não subiu junto, em
    /// cinco cascas de uma vez.
    pub fps: u32,
}

impl Alvo {
    /// 1280x720 — 3.600 macroblocos.
    pub const P720: Alvo = Alvo { max_fs: 3_600, fps: 30 };
    /// 1920x1080 — 8.160 macroblocos. **O padrão**, e o comportamento de antes do cardápio.
    pub const P1080: Alvo = Alvo { max_fs: 8_160, fps: 30 };
    /// 2560x1440 — 14.400 macroblocos.
    pub const P1440: Alvo = Alvo { max_fs: 14_400, fps: 30 };
    /// 3840x2160 — 32.400 macroblocos.
    pub const P2160: Alvo = Alvo { max_fs: 32_400, fps: 30 };

    /// O que vale quando ninguém escolheu. Ver a nota do tipo.
    pub const PADRAO: Alvo = Alvo::P1080;

    /// Todas as linhas do cardápio, da menor para a maior.
    pub const CARDAPIO: [Alvo; 4] = [Alvo::P720, Alvo::P1080, Alvo::P1440, Alvo::P2160];

    /// O rótulo curto — o mesmo texto nas cinco cascas, para não haver cinco traduções.
    pub fn rotulo(self) -> &'static str {
        match self.max_fs {
            3_600 => "720p",
            8_160 => "1080p",
            14_400 => "2K",
            32_400 => "4K",
            _ => "personalizado",
        }
    }

    /// O alvo de um `max_fs` qualquer, para atravessar a fronteira C como um número só.
    pub fn de_max_fs(max_fs: u32) -> Alvo {
        Alvo { max_fs: max_fs.max(1), fps: 30 }
    }

    /// O mesmo alvo com outra taxa de quadros. `0` volta ao padrão de 30.
    pub fn a(self, fps: u32) -> Alvo {
        Alvo { fps: if fps == 0 { 30 } else { fps }, ..self }
    }
}

/// Os limites de um nível H.264 que importam para escolher o que emitir (norma H.264, Anexo A,
/// Tabela A-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitesDoNivel {
    /// `level_idc`, que é o nível vezes dez: 31 é o nível 3.1.
    pub level_idc: u8,
    /// `MaxFS` — área máxima do quadro, em macroblocos.
    pub max_fs: u32,
    /// `MaxMBPS` — macroblocos por segundo.
    pub max_mbps: u32,
    /// `MaxBR` — taxa máxima de bits do nível, em **kbps de VCL** (o fator do Constrained
    /// Baseline é 1000; o de NAL seria 1200).
    ///
    /// Entrou em 02/09/2026 com [`teto_de_taxa`], e entrou **na mesma tabela** de propósito: os
    /// três números saem da mesma Tabela A-1 e mudam juntos quando o nível muda. Repare no que
    /// ele diz sobre a história deste projeto: o 3.1 permitia **14 000 kbps** e o produto usava
    /// 4 000. O teto de taxa nunca foi a norma cobrando — sempre foi decisão de produto.
    pub max_br_kbps: u32,
}

/// Tabela A-1 da norma, nos níveis que aparecem nesta bancada.
///
/// Só os níveis medidos em aparelho real entram aqui. Um nível ausente faz [`LimitesDoNivel::do_idc`]
/// devolver `None`, e quem pergunta cai no comportamento conservador — **nunca** num teto
/// inventado por interpolação, que é o tipo de chute que parece funcionar até o dia em que não.
const TABELA: &[LimitesDoNivel] = &[
    LimitesDoNivel { level_idc: 10, max_fs: 99, max_mbps: 1_485, max_br_kbps: 64 },
    LimitesDoNivel { level_idc: 11, max_fs: 396, max_mbps: 3_000, max_br_kbps: 192 },
    LimitesDoNivel { level_idc: 12, max_fs: 396, max_mbps: 6_000, max_br_kbps: 384 },
    LimitesDoNivel { level_idc: 13, max_fs: 396, max_mbps: 11_880, max_br_kbps: 768 },
    LimitesDoNivel { level_idc: 20, max_fs: 396, max_mbps: 11_880, max_br_kbps: 2_000 },
    LimitesDoNivel { level_idc: 21, max_fs: 792, max_mbps: 19_800, max_br_kbps: 4_000 },
    LimitesDoNivel { level_idc: 22, max_fs: 1_620, max_mbps: 20_250, max_br_kbps: 4_000 },
    LimitesDoNivel { level_idc: 30, max_fs: 1_620, max_mbps: 40_500, max_br_kbps: 10_000 },
    LimitesDoNivel { level_idc: 31, max_fs: 3_600, max_mbps: 108_000, max_br_kbps: 14_000 },
    LimitesDoNivel { level_idc: 32, max_fs: 5_120, max_mbps: 216_000, max_br_kbps: 20_000 },
    LimitesDoNivel { level_idc: 40, max_fs: 8_192, max_mbps: 245_760, max_br_kbps: 20_000 },
    LimitesDoNivel { level_idc: 41, max_fs: 8_192, max_mbps: 245_760, max_br_kbps: 50_000 },
    LimitesDoNivel { level_idc: 42, max_fs: 8_704, max_mbps: 522_240, max_br_kbps: 50_000 },
    LimitesDoNivel { level_idc: 50, max_fs: 22_080, max_mbps: 589_824, max_br_kbps: 135_000 },
    LimitesDoNivel { level_idc: 51, max_fs: 36_864, max_mbps: 983_040, max_br_kbps: 240_000 },
    LimitesDoNivel { level_idc: 52, max_fs: 36_864, max_mbps: 2_073_600, max_br_kbps: 240_000 },
];

impl LimitesDoNivel {
    /// Os limites de um `level_idc`, ou `None` se ele não está na tabela.
    pub fn do_idc(level_idc: u8) -> Option<Self> {
        TABELA.iter().copied().find(|l| l.level_idc == level_idc)
    }

    /// Os limites do nível que **nós anunciamos** no SDP.
    ///
    /// Lê o `profile-level-id` de [`crate::track::PERFIL_H264`] em vez de repetir o número: as
    /// duas coisas têm de mudar juntas, e a única forma de garantir isso é uma delas não existir
    /// separada. Se o `fmtp` mudar para um nível fora da tabela, isto devolve o **3.1** e não um
    /// palpite — ver [`Self::do_sdp_ou_conservador`].
    pub fn do_sdp() -> Option<Self> {
        Self::do_idc(nivel_anunciado()?)
    }

    /// Como [`Self::do_sdp`], mas nunca falha: cai no nível 3.1, que é o denominador comum da
    /// matriz e o valor que o `fmtp` tem hoje.
    pub fn do_sdp_ou_conservador() -> Self {
        Self::do_sdp()
            .unwrap_or(LimitesDoNivel { level_idc: 31, max_fs: 3_600, max_mbps: 108_000, max_br_kbps: 14_000 })
    }

    /// Quantos macroblocos ocupa um quadro deste tamanho. Arredonda **para cima**, porque um
    /// quadro de 1281 px de largura ocupa 81 macroblocos e recorta o resto no SPS.
    pub fn macroblocos(largura: u32, altura: u32) -> u32 {
        largura.div_ceil(LADO_DO_MACROBLOCO) * altura.div_ceil(LADO_DO_MACROBLOCO)
    }
}

/// O `level_idc` que o `fmtp` do projeto anuncia, lido do próprio texto.
///
/// `profile-level-id` são seis dígitos hexadecimais: `profile_idc`, os bits de restrição, e o
/// `level_idc`. Em `42e034`, `34` = 52 = nível 5.2 (em `42e028`, `28` = 40; em `42e01f`, `1f` = 31).
pub fn nivel_anunciado() -> Option<u8> {
    let campo = crate::track::PERFIL_H264
        .split(';')
        .find_map(|p| p.trim().strip_prefix("profile-level-id="))?;
    if campo.len() != 6 {
        return None;
    }
    u8::from_str_radix(&campo[4..6], 16).ok()
}

/// O que sai de [`ajustar`]: a geometria a codificar, e o que foi preciso mexer para chegar nela.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Saida {
    pub largura: u32,
    pub altura: u32,
    pub fps: u32,
    /// A dimensão mudou.
    pub reduziu_tamanho: bool,
    /// A taxa de quadros mudou.
    pub reduziu_fps: bool,
    /// Quantos macroblocos o quadro de saída ocupa, contra o `MaxFS` do nível.
    pub macroblocos: u32,
    /// A saída não é múltipla de 16 nos dois lados, então o SPS **precisa** declarar
    /// `frame_cropping`. Não é defeito — é o caso comum, e existe aqui para o relato da corrida
    /// poder dizê-lo em vez de o parser da bancada tropeçar nele.
    pub exige_recorte: bool,
    /// **Quantos bits por segundo pedir ao encoder para este quadro.** Ver [`teto_de_taxa`].
    ///
    /// Vem junto com a geometria de propósito: as duas decisões são a mesma decisão, e separá-las
    /// foi exatamente o que produziu o defeito de 01/09/2026 — o teto de resolução subiu para
    /// 1080p e o de taxa ficou em 4 Mbps, literal em cada casca.
    pub teto_de_taxa_bps: u32,
}

/// A taxa de referência: **o valor de produto**, e a resolução em que ele foi escolhido.
///
/// 4 000 000 bps a 1280x720 e 30 fps são 27 648 000 pixels por segundo, ou ~0,145 bit por pixel.
/// Nenhum dos dois números é novo — o que é novo é eles estarem escritos **juntos**, que é o que
/// permite escalar um quando o outro muda.
const TAXA_DE_REFERENCIA_BPS: u64 = 4_000_000;
const PIXELS_POR_SEGUNDO_DA_REFERENCIA: u64 = 1280 * 720 * 30;

/// **Quantos bits por segundo pedir ao encoder para este quadro.**
///
/// # O defeito que ela fecha
///
/// Em 01/09/2026 o teto de resolução subiu de 720p para 1080p em cinco cascas. O de taxa **não
/// subiu**: continuou 4 000 000, cravado literalmente em cada uma delas — iOS, Android, macOS,
/// Windows e a câmera virtual. O resultado não é "1080p com a mesma qualidade": é 1080p com
/// **2,25 vezes menos bits por pixel** que os 720p que ele substituiu. Mais pixels pelo mesmo
/// orçamento é mais quantização, e a mudança que existia para melhorar a imagem podia piorá-la.
///
/// Era o mesmo padrão que produziu o teto de resolução: **ninguém tinha onde perguntar qual era o
/// teto de taxa**, então cinco cascas responderam sozinhas, com o mesmo literal, e o literal
/// envelheceu de uma vez só quando a resolução mudou.
///
/// # A regra
///
/// Mantém a densidade medida do produto — [`TAXA_DE_REFERENCIA_BPS`] sobre
/// [`PIXELS_POR_SEGUNDO_DA_REFERENCIA`] — e a aplica à taxa de pixels de saída. A consequência que
/// importa é o **braço de aferição negativo**: uma sessão de 720p30 recebe exatamente
/// 4 000 000 e é byte a byte a mesma de antes. Só quem de fato ficou maior ganha mais bits.
///
/// 1920x1080 a 30 fps dá 9 000 000 bps.
///
/// # Os dois limites, e nenhum é gosto
///
/// - **Teto**: o `MaxBR` do nível anunciado ([`LimitesDoNivel::max_br_kbps`]). Passar dele seria
///   violar o `profile-level-id` que este mesmo módulo publica no SDP — o receptor teria o
///   direito de recusar. No 4.0 são 20 000 kbps, e 1080p30 fica bem abaixo.
/// - **Piso**: [`crate::taxa::PISO_BPS`], o piso do controlador. Um teto abaixo do piso não é um
///   teto; é um erro de aritmética, e o controlador nasceria já "no piso" dizendo que o enlace não
///   dá quando o problema seria a conta.
///
/// # O que ela NÃO decide, e é preciso dizer
///
/// **Não decide o que sai no fio.** Quem decide é [`crate::taxa::ControleDeTaxa`], que nasce
/// aqui e só sabe descer. Num enlace limpo a sessão fica neste valor; num ruim ela desce, e
/// desce mais degraus do que descia — de 9 Mbps a 700 kbps são ~9 descidas multiplicativas
/// contra ~6 saindo de 4 Mbps. **Esse é o custo, e ele é real**: em 2,4 GHz são três janelas a
/// mais de bitrate alto antes de assentar. Quem responde à rajada em curso continua sendo o
/// pedido de IDR, não este número.
pub fn teto_de_taxa(largura: u32, altura: u32, fps: u32) -> u32 {
    teto_de_taxa_com(largura, altura, fps, LimitesDoNivel::do_sdp_ou_conservador())
}

/// [`teto_de_taxa`] com os limites explícitos — o que os testes usam para exercitar níveis que o
/// `fmtp` não anuncia hoje, sem mexer no `fmtp`.
pub fn teto_de_taxa_com(largura: u32, altura: u32, fps: u32, limites: LimitesDoNivel) -> u32 {
    let pixels_por_segundo =
        u64::from(largura) * u64::from(altura) * u64::from(fps.max(1));
    // Multiplica antes de dividir: a divisão primeiro jogaria fora a densidade inteira, que é um
    // número menor que 1.
    let bruto = pixels_por_segundo.saturating_mul(TAXA_DE_REFERENCIA_BPS)
        / PIXELS_POR_SEGUNDO_DA_REFERENCIA.max(1);
    let teto_do_nivel = u64::from(limites.max_br_kbps).saturating_mul(1_000);
    let com_teto = bruto.min(teto_do_nivel);
    let com_piso = com_teto.max(u64::from(crate::taxa::PISO_BPS));
    com_piso.min(u64::from(u32::MAX)) as u32
}

/// Ajusta uma geometria de captura para caber no nível que o SDP anuncia, **preservando a
/// proporção**.
///
/// # As decisões, e por que cada uma
///
/// **Preserva proporção.** Deformar a tela de alguém para caber num retângulo é pior que reduzir:
/// o receptor não tem como desfazer.
///
/// **Nunca amplia.** Uma câmera de 640x480 sai 640x480 e não esticada até o teto — ampliar
/// gastaria banda para não acrescentar informação nenhuma.
///
/// **Arredonda para par, para baixo.** Dimensão ímpar não existe em 4:2:0. É obrigatório, não
/// higiene.
///
/// **Não força múltiplo de 16, de propósito.** O encoder codifica o retângulo arredondado para
/// cima em macroblocos e declara no SPS quantos pixels recortar (`frame_cropping`). Forçar
/// múltiplo de 16 aqui deformaria a proporção para agradar um parser nosso. Este projeto já foi
/// mordido pelo lado oposto: o iPhone X decodificou em **590x1280** — que é o certo, porque ele é
/// mais alongado — e o `provar.sh` quase reprovou a corrida **por ela estar certa**. A saída
/// certa é o instrumento aprender a ler o recorte, e [`Saida::exige_recorte`] é o aviso de que
/// ele vai precisar.
///
/// **Corrige depois de arredondar, e não antes.** Escalar por `sqrt(MaxFS/mbs)` e arredondar pode
/// devolver um quadro que ainda ocupa um macrobloco a mais, porque a contagem arredonda para
/// cima nos dois eixos. Por isso o laço no fim: encolhe 16 px do lado maior até a conta fechar de
/// verdade. **Conferir no artefato, não no retorno da fórmula.**
pub fn ajustar(largura: u32, altura: u32, fps: u32) -> Saida {
    // **O padrão é [`Alvo::PADRAO`], e não o nível.** Quando `PERFIL_H264` subiu para 5.2 em
    // 07/09/2026 para abrir o cardápio de resolução, esta função — que é a que todas as cascas
    // chamam — passaria a deixar **tudo** subir de uma vez: a tela do S24 saltaria de 976x2116
    // para os 1440x3120 nativos, 2,3x a carga, sem ninguém ter pedido. Quatro testes desta
    // seção pegaram isso na hora, e é para isso que eles existem.
    //
    // Quem quiser mais pede por [`ajustar_para`]. Abrir o cardápio não pode mexer em quem não
    // abriu o menu.
    ajustar_para(largura, altura, fps, None)
}

/// Como [`ajustar`], mas respeitando **o que o usuário escolheu** — ver [`Alvo`].
///
/// A regra é uma linha e está aqui em vez de em cinco cascas: **vale o menor entre a escolha e o
/// nível.** Escolher 4K num binário que anuncia 4.0 devolve 1080p, e não um erro: o usuário pediu
/// o máximo que o aparelho dele permitir, e é isso que ele recebe.
///
/// `None` é "não escolheu", e cai em [`Alvo::PADRAO`] — que é o comportamento de antes do
/// cardápio, byte a byte.
pub fn ajustar_para(largura: u32, altura: u32, fps: u32, alvo: Option<Alvo>) -> Saida {
    let nivel = LimitesDoNivel::do_sdp_ou_conservador();
    let escolhido = alvo.unwrap_or(Alvo::PADRAO);
    let limites = LimitesDoNivel {
        max_fs: nivel.max_fs.min(escolhido.max_fs),
        ..nivel
    };
    // **O teto de fps também é do usuário.** `FPS_MAXIMO` deixou de ser a última palavra em
    // 07/09/2026: ele é o padrão de quem não escolheu, e um número global calibrado pelo pior
    // receptor da bancada — ver a nota da constante. Quem escolhe passa o dele, e o nível ainda
    // corta por cima.
    ajustar_com(largura, altura, fps, limites, escolhido.fps.max(1))
}

/// [`ajustar`] com os limites explícitos — é o que os testes usam para exercitar níveis que o
/// `fmtp` não anuncia hoje, sem mexer no `fmtp`.
pub fn ajustar_com(largura: u32, altura: u32, fps: u32, limites: LimitesDoNivel, fps_maximo: u32) -> Saida {
    // Entrada impossível (a API de captura mentiu, ou ninguém preencheu): devolve o retângulo
    // 16:9 que satura o nível, que é a saída mais conservadora que existe. Nunca um erro: quem
    // chama está no caminho de abrir uma sessão e não tem o que fazer com uma ausência.
    if largura == 0 || altura == 0 {
        let (l, a) = retangulo_16_9(limites.max_fs);
        return Saida {
            largura: l,
            altura: a,
            fps: fps_maximo.min(fps.max(1)),
            reduziu_tamanho: true,
            reduziu_fps: false,
            macroblocos: LimitesDoNivel::macroblocos(l, a),
            exige_recorte: l % LADO_DO_MACROBLOCO != 0 || a % LADO_DO_MACROBLOCO != 0,
            teto_de_taxa_bps: teto_de_taxa_com(l, a, fps_maximo.min(fps.max(1)), limites),
        };
    }

    let mut l = largura;
    let mut a = altura;

    // 1. A área. `sqrt(MaxFS / mbs)` é a escala que faria o quadro ocupar exatamente o teto.
    let mbs = LimitesDoNivel::macroblocos(l, a);
    if mbs > limites.max_fs {
        let escala = (f64::from(limites.max_fs) / f64::from(mbs)).sqrt();
        l = ((f64::from(largura) * escala).round() as u32).max(2);
        a = ((f64::from(altura) * escala).round() as u32).max(2);
    }

    l -= l % 2;
    a -= a % 2;
    l = l.max(2);
    a = a.max(2);

    // 2. O acerto fino. O arredondamento para cima da contagem de macroblocos pode ter deixado o
    //    quadro um macrobloco acima do teto; encolhe o lado maior até caber. O laço é limitado
    //    pela própria aritmética (cada volta tira 16 px de um lado), mas o teto de voltas está
    //    escrito assim mesmo — um laço que depende de aritmética de ponto flutuante para
    //    terminar não é um laço, é uma aposta.
    let mut voltas = 0;
    while LimitesDoNivel::macroblocos(l, a) > limites.max_fs && voltas < 1_000 {
        if l >= a {
            let novo = l.saturating_sub(LADO_DO_MACROBLOCO);
            a = ((f64::from(novo) * f64::from(altura) / f64::from(largura)).round() as u32).max(2);
            l = novo.max(2);
        } else {
            let novo = a.saturating_sub(LADO_DO_MACROBLOCO);
            l = ((f64::from(novo) * f64::from(largura) / f64::from(altura)).round() as u32).max(2);
            a = novo.max(2);
        }
        l -= l % 2;
        a -= a % 2;
        l = l.max(2);
        a = a.max(2);
        voltas += 1;
    }

    // 3. A taxa. Duas cobranças independentes: o `MaxMBPS` da norma e a política de produto.
    let mbs_final = LimitesDoNivel::macroblocos(l, a);
    let pedido = fps.max(1);
    let pelo_nivel = (limites.max_mbps / mbs_final.max(1)).max(1);
    let saida_fps = pedido.min(pelo_nivel).min(fps_maximo);

    Saida {
        largura: l,
        altura: a,
        fps: saida_fps,
        reduziu_tamanho: l != largura || a != altura,
        reduziu_fps: saida_fps != fps,
        macroblocos: mbs_final,
        exige_recorte: l % LADO_DO_MACROBLOCO != 0 || a % LADO_DO_MACROBLOCO != 0,
        // **A taxa é da saída, nunca da entrada.** Um monitor 5K reduzido para 1080p tem de pedir
        // o orçamento de 1080p; pedir o de 5K seria mandar para a rede o que o quadro nem carrega.
        teto_de_taxa_bps: teto_de_taxa_com(l, a, saida_fps, limites),
    }
}

/// O maior retângulo 16:9, em pixels pares, que cabe em `max_fs` macroblocos. Para o nível 3.1 dá
/// exatamente 1280x720 — que é de onde vem o número que todo mundo decorou.
fn retangulo_16_9(max_fs: u32) -> (u32, u32) {
    let mut melhor = (2u32, 2u32);
    // 16:9 em macroblocos é 16n x 9n. `n` até 64 cobre com folga qualquer nível da tabela.
    for n in 1..=64u32 {
        let (l, a) = (n * 16 * LADO_DO_MACROBLOCO, n * 9 * LADO_DO_MACROBLOCO);
        if LimitesDoNivel::macroblocos(l, a) <= max_fs {
            melhor = (l, a);
        } else {
            break;
        }
    }
    melhor
}

impl Saida {
    /// Uma linha para o registro da corrida. Existe porque "capturou 2560x1664" e "codificou
    /// 1280x832" são frases diferentes, e o registro desta casa só sabia imprimir uma delas — que
    /// é precisamente como quatro emissores sem teto passaram meses sem que ninguém os nomeasse.
    pub fn relato(&self, largura_de_entrada: u32, altura_de_entrada: u32, fps_pedido: u32) -> String {
        let limites = LimitesDoNivel::do_sdp_ou_conservador();
        if !self.reduziu_tamanho && !self.reduziu_fps {
            return format!(
                "teto: nada a limitar — {}x{} @{}fps ocupa {}/{} macroblocos do nível {}.{}, \
                 a {} kbps",
                largura_de_entrada, altura_de_entrada, self.fps, self.macroblocos, limites.max_fs,
                limites.level_idc / 10, limites.level_idc % 10, self.teto_de_taxa_bps / 1_000
            );
        }
        let tamanho = if self.reduziu_tamanho {
            format!("{}x{} -> {}x{}", largura_de_entrada, altura_de_entrada, self.largura, self.altura)
        } else {
            format!("{}x{} (intacto)", self.largura, self.altura)
        };
        let taxa = if self.reduziu_fps {
            format!("{} -> {} fps", fps_pedido, self.fps)
        } else {
            format!("{} fps (intacto)", self.fps)
        };
        format!(
            "teto do nível {}.{}: {}, {} | {}/{} macroblocos | {} kbps{}",
            limites.level_idc / 10, limites.level_idc % 10, tamanho, taxa,
            self.macroblocos, limites.max_fs, self.teto_de_taxa_bps / 1_000,
            if self.exige_recorte { " | o SPS precisa declarar frame_cropping" } else { "" }
        )
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn o_nivel_sai_do_fmtp_e_nao_de_um_numero_repetido() {
        // Se alguém mudar `PERFIL_H264` sem mudar o teto, é aqui que aparece.
        assert_eq!(nivel_anunciado(), Some(52));
        assert_eq!(LimitesDoNivel::do_sdp().unwrap().max_fs, 36_864);

        // **E o que o produto emite não é isto.** O nível é a régua de baixo; a de cima é o
        // [`Alvo`] que o usuário escolheu, e sem escolha vale [`Alvo::PADRAO`]. Este par de
        // asserções é o que guarda a promessa de 07/09/2026: abrir o cardápio para 4K não pode
        // mexer em quem não abriu o menu.
        assert_eq!(Alvo::PADRAO, Alvo::P1080);
        let sem_escolha = ajustar(3840, 2160, 30);
        assert!(
            LimitesDoNivel::macroblocos(sem_escolha.largura, sem_escolha.altura)
                <= Alvo::P1080.max_fs,
            "sem escolha, 4K tem de cair no padrão de 1080p, e veio {}x{}",
            sem_escolha.largura, sem_escolha.altura
        );
        let com_escolha = ajustar_para(3840, 2160, 30, Some(Alvo::P2160));
        assert_eq!(
            (com_escolha.largura, com_escolha.altura), (3840, 2160),
            "escolhendo 4K no nível 5.2, 4K passa intacto"
        );
    }

    /// **60 fps é escolha, e o nível 5.2 comporta o cardápio inteiro nas duas taxas.**
    ///
    /// Este teste guarda as duas metades da promessa de 07/09/2026: quem não escolher continua
    /// em 30, e quem escolher recebe. O `FPS_MAXIMO` deixou de ser a última palavra e virou o
    /// padrão de quem não abriu o menu.
    #[test]
    fn sessenta_quadros_sao_escolha_e_o_nivel_comporta_o_cardapio() {
        for alvo in Alvo::CARDAPIO {
            let a30 = ajustar_para(3840, 2160, 60, Some(alvo));
            assert_eq!(a30.fps, 30, "sem pedir 60, {} continua em 30", alvo.rotulo());

            let a60 = ajustar_para(3840, 2160, 60, Some(alvo.a(60)));
            assert_eq!(
                a60.fps, 60,
                "{} a 60 tem de passar no nível 5.2 — veio {}x{}@{}",
                alvo.rotulo(), a60.largura, a60.altura, a60.fps
            );
            assert!(
                LimitesDoNivel::macroblocos(a60.largura, a60.altura) <= alvo.max_fs,
                "{} a 60 estourou a própria escolha", alvo.rotulo()
            );
        }

        // E a régua de baixo continua existindo: 4K60 são 1.944.000 MB/s contra os 2.073.600 do
        // 5.2. Cabe com 6 % de folga, e é o modo mais caro que este cardápio oferece.
        let quatro_k60 = ajustar_para(3840, 2160, 60, Some(Alvo::P2160.a(60)));
        assert_eq!((quatro_k60.largura, quatro_k60.altura, quatro_k60.fps), (3840, 2160, 60));
        assert!(quatro_k60.macroblocos * 60 <= LimitesDoNivel::do_sdp().unwrap().max_mbps);
    }

    #[test]
    fn setecentos_e_vinte_p_trinta_passa_inteiro_e_agora_com_folga() {
        // 1280x720 = 80x45 = 3600 macroblocos, que era o `MaxFS` **exato** do 3.1. No 4.0 sobra
        // mais que o dobro, e a saída continua idêntica: subir o teto não mexe em quem já cabia.
        assert_eq!(LimitesDoNivel::macroblocos(1280, 720), 3_600);
        let s = ajustar(1280, 720, 30);
        assert_eq!((s.largura, s.altura, s.fps), (1280, 720, 30));
        assert!(!s.reduziu_tamanho, "720p30 já cabe: não há o que reduzir");
        assert!(!s.exige_recorte, "1280 e 720 são múltiplos de 16");
    }

    #[test]
    fn o_mesmo_tamanho_a_sessenta_quadros_e_cortado_pela_taxa() {
        // A linha que isola o fps. **Mudou de dono com o 4.0**: no 3.1 quem cortava 720p60 era
        // o `MaxMBPS` da norma (108000 / 3600 = 30). No 4.0 a norma permitiria 68 fps
        // (245760 / 3600), e quem corta passa a ser a política de produto, `FPS_MAXIMO`. O
        // resultado é o mesmo 30, e é de propósito que o teste continua aqui: o dia em que
        // alguém subir `FPS_MAXIMO` sem pensar, é neste ponto que a conta muda.
        // No 5.2 a norma permitiria 576 fps para este tamanho (2073600 / 3600); quem corta
        // continua sendo o alvo do usuário, que sem escolha é 30. O número da norma mudou quatro
        // vezes (30 no 3.1, 68 no 4.0, 273 no 5.1, 576 no 5.2) e o resultado sem escolha nunca
        // mudou — é essa invariância que o teste guarda.
        assert_eq!(LimitesDoNivel::do_sdp().unwrap().max_mbps / 3_600, 576);
        let s = ajustar(1280, 720, 60);
        assert_eq!((s.largura, s.altura), (1280, 720), "a área já cabia");
        assert_eq!(s.fps, FPS_MAXIMO);
        assert!(s.reduziu_fps);
    }

    #[test]
    fn a_tela_do_dell_de_1080p_agora_passa_inteira() {
        // **O teste que a mudança de 01/09/2026 existe para fazer passar.** No 3.1 este quadro
        // encolhia; no 4.0 ele atravessa sem tocar em nada.
        let s = ajustar(1920, 1080, 30);
        assert_eq!((s.largura, s.altura, s.fps), (1920, 1080, 30));
        assert!(!s.reduziu_tamanho, "1080p cabe no 4.0");
        assert!(!s.reduziu_fps);
        // **E 1080p exige recorte, ao contrário de 720p.** 1080 / 16 = 67,5: o codificador
        // codifica 68 linhas de macrobloco (1088 px) e sinaliza o corte no SPS. Quem não honrar
        // `frame_cropping` mostra 8 linhas de lixo no rodapé. Era um caso que não existia
        // enquanto o teto era 720p, onde 720 / 16 = 45 exato.
        assert!(s.exige_recorte, "1080 não é múltiplo de 16 e o recorte é obrigatório");
        assert_eq!(1080 % 16, 8, "sobram 8 px: o quadro real é 1088");
    }

    #[test]
    fn mil_e_oitenta_p_a_trinta_cabe_no_4_0_por_zero_virgula_quatro_por_cento() {
        // A margem é fina e o número tem de estar escrito: 1920x1080 são 120x68 = 8160
        // macroblocos contra `MaxFS` 8192 (32 de folga, 0,4 %), e a 30 fps são 244800 contra
        // `MaxMBPS` 245760 (960 de folga, 0,4 %). **1080p31 não caberia.** É por isso que 4.0 é
        // o nível certo e não um arredondamento para cima.
        let l = LimitesDoNivel::do_idc(40).unwrap();
        let mbs = LimitesDoNivel::macroblocos(1920, 1080);
        assert_eq!(mbs, 8_160);
        assert!(mbs <= l.max_fs);
        assert_eq!(l.max_fs - mbs, 32);
        assert_eq!(mbs * 30, 244_800);
        assert!(mbs * 30 <= l.max_mbps);
        assert!(mbs * 31 > l.max_mbps, "31 fps a 1080p estouraria o MaxMBPS do 4.0");
    }

    #[test]
    fn a_tela_alongada_do_a10s_que_abriu_esta_correcao() {
        // 720x1520: 45x95 = 4275 macroblocos. Era o caso que **abriu** a correção do teto: no
        // 3.1 estourava os 3600 e tinha de encolher. No 4.0 os 4275 cabem nos 8192 e a tela do
        // A10s atravessa **na resolução nativa**, sem encolher e sem ser espremida numa caixa
        // 16:9. O aparelho não ganhou codificador nenhum — ganhou permissão.
        assert_eq!(LimitesDoNivel::macroblocos(720, 1520), 4_275);
        assert!(4_275 > LimitesDoNivel::do_idc(31).unwrap().max_fs, "não cabia no 3.1");
        let s = ajustar(720, 1520, 30);
        assert_eq!((s.largura, s.altura, s.fps), (720, 1520, 30));
        assert!(!s.reduziu_tamanho, "no 4.0 a tela do A10s passa inteira");
    }

    #[test]
    fn a_tela_deste_macbook_estoura_nos_dois_eixos() {
        // 2560x1664 = 160x104 = 16640 macroblocos: estoura mesmo o 4.0 (8192), e continua sendo
        // o caso que prova que subir o teto não é o mesmo que tirar o teto.
        assert_eq!(LimitesDoNivel::macroblocos(2560, 1664), 16_640);
        let s = ajustar(2560, 1664, 60);
        assert!(s.macroblocos <= 8_192);
        assert_eq!(s.fps, 30);
        assert!(s.reduziu_tamanho && s.reduziu_fps);
    }

    #[test]
    fn nunca_amplia() {
        // Uma câmera pequena sai como entrou. Ampliar gastaria banda para não acrescentar nada.
        let s = ajustar(640, 480, 30);
        assert_eq!((s.largura, s.altura, s.fps), (640, 480, 30));
        assert!(!s.reduziu_tamanho);
    }

    #[test]
    fn a_taxa_e_cortada_pela_politica_mesmo_quando_o_nivel_permitiria() {
        // 640x480 = 40x30 = 1200 macroblocos; o `MaxMBPS` de 4.0 permitiria 204 fps (o de 3.1
        // permitia 90). A política de produto diz 30, e é ela que manda — em qualquer nível.
        let s = ajustar(640, 480, 90);
        assert_eq!(s.fps, FPS_MAXIMO);
    }

    #[test]
    fn a_saida_e_sempre_par_e_sempre_cabe() {
        // Varredura sobre formas reais e absurdas. O invariante é o contrato inteiro em três
        // linhas, e é o que impede um caso de borda de voltar em silêncio.
        let limites = LimitesDoNivel::do_sdp_ou_conservador();
        for largura in [2u32, 320, 640, 720, 750, 1080, 1280, 1334, 1512, 1920, 2560, 3840, 5120] {
            for altura in [2u32, 240, 480, 720, 1080, 1280, 1520, 1664, 2160, 2880] {
                let s = ajustar(largura, altura, 60);
                assert_eq!(s.largura % 2, 0, "{largura}x{altura} -> largura ímpar");
                assert_eq!(s.altura % 2, 0, "{largura}x{altura} -> altura ímpar");
                assert!(s.largura >= 2 && s.altura >= 2, "{largura}x{altura} degenerou");
                assert!(
                    s.macroblocos <= limites.max_fs,
                    "{largura}x{altura} -> {}x{} = {} macroblocos, acima de {}",
                    s.largura, s.altura, s.macroblocos, limites.max_fs
                );
                assert!(s.fps >= 1 && s.fps <= FPS_MAXIMO);
                assert!(s.largura <= largura && s.altura <= altura, "{largura}x{altura} ampliou");
                assert!(
                    u64::from(s.macroblocos) * u64::from(s.fps) <= u64::from(limites.max_mbps),
                    "{largura}x{altura} estourou o MaxMBPS"
                );
            }
        }
    }

    #[test]
    fn entrada_impossivel_cai_no_conservador_em_vez_de_estourar() {
        // O maior 16:9 inteiro que cabe no `MaxFS` do 4.0: n=7 dá 1792x1008 = 112x63 = 7056
        // macroblocos; n=8 daria 2048x1152 = 9216 e estouraria. Não é 1920x1080 porque o
        // gerador anda de macrobloco em macrobloco na razão 16:9 exata, e 1080 não é múltiplo
        // de 16 — a diferença é justamente o `exige_recorte` que 1080p carrega.
        let s = ajustar(0, 0, 30);
        assert_eq!((s.largura, s.altura), (1792, 1008));
        assert!(s.macroblocos <= 8_192);
    }

    #[test]
    fn a_tabela_de_niveis_bate_com_a_norma_nos_pontos_conhecidos() {
        // Três âncoras verificáveis fora daqui: 3.1 comporta 720p, 4.0 comporta 1080p, e 5.2 é o
        // que o MacBook produzia a 2560x1664.
        assert!(LimitesDoNivel::macroblocos(1280, 720) <= LimitesDoNivel::do_idc(31).unwrap().max_fs);
        assert!(LimitesDoNivel::macroblocos(1920, 1080) > LimitesDoNivel::do_idc(31).unwrap().max_fs);
        assert!(LimitesDoNivel::macroblocos(1920, 1080) <= LimitesDoNivel::do_idc(40).unwrap().max_fs);
        assert!(LimitesDoNivel::macroblocos(2560, 1664) <= LimitesDoNivel::do_idc(52).unwrap().max_fs);
    }

    #[test]
    fn o_retangulo_16_9_de_cada_nivel() {
        assert_eq!(retangulo_16_9(3_600), (1280, 720), "o do 3.1, que era o teto antigo");
        assert_eq!(retangulo_16_9(8_192), (1792, 1008), "o do 4.0, que é o teto de hoje");
    }

    /// **O braço de aferição negativo, e ele passa por construção.** 720p30 recebe exatamente o
    /// valor de produto de sempre: a sessão que não cresceu é byte a byte a mesma.
    #[test]
    fn setecentos_e_vinte_p_trinta_recebe_o_valor_de_produto_de_sempre() {
        assert_eq!(teto_de_taxa(1280, 720, 30), 4_000_000);
    }

    /// A frente inteira num número: 1080p tem 2,25 vezes os pixels de 720p, e passa a ter 2,25
    /// vezes os bits. Até 02/09/2026 tinha os mesmos 4 Mbps — mais pixels pelo mesmo orçamento,
    /// que é imagem pior, não melhor.
    #[test]
    fn mil_e_oitenta_p_trinta_ganha_a_taxa_que_faltou_subir() {
        assert_eq!(teto_de_taxa(1920, 1080, 30), 9_000_000);
        let densidade_720 = 4_000_000.0 / (1280.0 * 720.0 * 30.0);
        let densidade_1080 = f64::from(teto_de_taxa(1920, 1080, 30)) / (1920.0 * 1080.0 * 30.0);
        assert!(
            (densidade_720 - densidade_1080).abs() < 1e-9,
            "a densidade tem de ser a mesma: {densidade_720} contra {densidade_1080}"
        );
    }

    /// O `MaxBR` do nível é um limite de **norma**, não de gosto: passar dele violaria o
    /// `profile-level-id` que este módulo publica, e o receptor teria o direito de recusar.
    #[test]
    fn a_taxa_nunca_passa_do_max_br_do_nivel() {
        // 4K a 30 fps pela densidade daria ~36 Mbps; o 4.0 permite 20 000 kbps.
        let quatro_k = teto_de_taxa_com(3840, 2160, 30, LimitesDoNivel::do_idc(40).unwrap());
        assert_eq!(quatro_k, 20_000_000);
        // E o 3.1, que era o nível anunciado até 01/09, para em 14 000 kbps.
        let no_3_1 = teto_de_taxa_com(3840, 2160, 30, LimitesDoNivel::do_idc(31).unwrap());
        assert_eq!(no_3_1, 14_000_000);
    }

    /// Um teto abaixo do piso do controlador não é um teto: o controlador nasceria já `no_piso`,
    /// dizendo que o enlace não dá quando o problema seria a aritmética.
    #[test]
    fn a_taxa_nunca_fica_abaixo_do_piso_do_controlador() {
        // Uma câmera de 160x120 a 15 fps daria ~41 kbps pela densidade.
        assert_eq!(teto_de_taxa(160, 120, 15), crate::taxa::PISO_BPS);
        assert!(teto_de_taxa(2, 2, 1) >= crate::taxa::PISO_BPS);
    }

    /// A taxa acompanha a **saída**, e não a entrada. Um MacBook de 2560x1664 é reduzido a
    /// 1280x832 pelo teto de área; pedir o orçamento dos 2560 seria mandar para a rede bits que o
    /// quadro nem carrega.
    #[test]
    fn a_taxa_e_da_saida_e_nao_da_entrada() {
        let s = ajustar(2560, 1664, 60);
        assert_eq!(s.teto_de_taxa_bps, teto_de_taxa(s.largura, s.altura, s.fps));
        assert!(
            s.teto_de_taxa_bps < teto_de_taxa(2560, 1664, 60),
            "o orçamento tem de encolher junto com o quadro"
        );
    }

    /// O `MaxBR` entrou na mesma tabela que o `MaxFS` e o `MaxMBPS` porque os três saem da mesma
    /// Tabela A-1 e mudam juntos. E ele conta uma coisa sobre a história deste projeto: o 3.1
    /// permitia 14 000 kbps enquanto o produto usava 4 000. O teto de taxa nunca foi a norma
    /// cobrando.
    #[test]
    fn o_max_br_da_tabela_bate_com_a_norma() {
        assert_eq!(LimitesDoNivel::do_idc(31).unwrap().max_br_kbps, 14_000);
        assert_eq!(LimitesDoNivel::do_idc(40).unwrap().max_br_kbps, 20_000);
        assert_eq!(LimitesDoNivel::do_idc(52).unwrap().max_br_kbps, 240_000);
        // Monotônica: nível maior nunca permite menos bits que um menor.
        let mut anterior = 0;
        for l in TABELA {
            assert!(l.max_br_kbps >= anterior, "o nível {} regrediu", l.level_idc);
            anterior = l.max_br_kbps;
        }
    }
}

