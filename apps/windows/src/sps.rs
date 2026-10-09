//! Leitor do conjunto de parâmetros que **este** emissor de fato põe no fio.
//!
//! # Por que um app de produto carrega um parser de SPS
//!
//! `docs/regras-de-frente.md`, "fazer a coisa certa e não declarar custa o mesmo que fazer
//! errado": o decodificador do outro lado não lê o nosso código-fonte, lê o conjunto de
//! parâmetros, e na dúvida assume o pior caso. Medido no M4 do projeto: um emissor que não declara
//! `bitstream_restriction` faz o decodificador da Microsoft segurar ~5 quadros — **169,5 ms de p50
//! contra 0,55 ms** — com 30 fps limpos nos dois casos. A vazão não denuncia; só a latência muda.
//!
//! `docs/bancada.md` mediu esse campo para o NVENC do Dell (declara `max_num_reorder_frames = 0` e
//! `max_dec_frame_buffering = 1`), para o VideoToolbox e para o MediaCodec. **Nunca mediu o Quick
//! Sync**, que é o MFT que de fato ativa em `apps/windows` (o MFT da NVIDIA falha em
//! `ActivateObject` neste Dell Optimus — achado 2 do `README.md`). Este módulo existe para essa
//! linha deixar de estar em branco, medida **no caminho de produção**, no bitstream que sai — não
//! no que a `ICodecAPI` respondeu.
//!
//! É a mesma escolha do irmão do macOS, que escreveu um teste sobre o SPS que o `H264Encoder`
//! emite em vez de confiar no `AllowFrameReordering = false` que ele pediu.
//!
//! Lê, e desde a S7 do som (21/09/2026) reescreve duas coisas no SPS Baseline que chega, antes do
//! decodificador: o `constraint_set1` ([`declarar_constrained_baseline`], para o DXVA) e a
//! `bitstream_restriction` ([`declarar_restricao_de_bitstream`], para o decodificador não segurar
//! quadros). A medida que diz que o segundo remendo é preciso está em
//! `docs/som-no-receptor.md` §20.15 e §20.19.

/// O que o SPS declara, nos campos que mudam o comportamento do decodificador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumoSps {
    pub bytes: usize,
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub largura: u32,
    pub altura: u32,
    /// Há VUI?
    pub tem_vui: bool,
    /// `video_signal_type_present_flag` — quem declara faixa de cor e primárias.
    pub tem_video_signal_type: bool,
    pub full_range: Option<bool>,
    /// (primárias, transferência, matriz), quando `colour_description_present_flag`.
    pub cor: Option<(u8, u8, u8)>,
    /// **O campo que decide a latência do outro lado.**
    pub tem_bitstream_restriction: bool,
    pub max_num_reorder_frames: Option<u32>,
    pub max_dec_frame_buffering: Option<u32>,
}

impl ResumoSps {
    /// Uma linha para o registro. Curta de propósito: ela sai uma vez por sessão, e o que importa
    /// é poder comparar com a tabela de `docs/bancada.md` sem abrir um JSON.
    pub fn linha(&self) -> String {
        let restricao = if self.tem_bitstream_restriction {
            format!(
                "reorder={} dpb={}",
                self.max_num_reorder_frames.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
                self.max_dec_frame_buffering.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            )
        } else {
            "AUSENTE (o decodificador vai assumir o teto do nível)".to_string()
        };
        let sinal = match (self.tem_video_signal_type, self.full_range, self.cor) {
            (false, _, _) => "video_signal_type AUSENTE".to_string(),
            (true, faixa, Some((p, t, m))) => format!(
                "full_range={} primarias={p} transfer={t} matriz={m}",
                faixa.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
            ),
            (true, faixa, None) => format!(
                "full_range={} sem colour_description",
                faixa.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
            ),
        };
        format!(
            "sps={}B perfil={} constraint={:06b} nivel={} {}x{} vui={} | bitstream_restriction: {restricao} | {sinal}",
            self.bytes,
            self.profile_idc,
            self.constraint_flags >> 2,
            self.level_idc,
            self.largura,
            self.altura,
            self.tem_vui,
        )
    }

    /// A pergunta que a regra da casa manda fazer — e ela é sobre o **campo**, não sobre a seção.
    ///
    /// O corolário que custou um desenho no M4: "tem VUI?" é a pergunta errada. O VideoToolbox com
    /// entrada `420f` **escreve** um VUI e mesmo assim deixa `bitstream_restriction_flag = 0`. A
    /// pergunta certa é se a restrição está declarada.
    pub fn declara_a_restricao(&self) -> bool {
        self.tem_bitstream_restriction
    }
}

/// **Declara Constrained Baseline num SPS que diz só Baseline**, no lugar. Devolve `true` se mexeu.
///
/// # Por que isto existe: o decoder da Microsoft recusa DXVA para Baseline sem a marca
///
/// Medido em 10/09/2026 no Dell, com o mesmo app e o mesmo decoder:
///
/// | origem | SPS | textura de saída | `ProcessOutput` p50 |
/// |---|---|---|---|
/// | x264 sintético | `perfil=66 constraint=110000` | `bind=0x200` (DXVA), 8 superfícies | **0,23 ms** |
/// | câmera do S24 | `perfil=66 constraint=100000` | `bind=0x0`, 1 textura | **7,4 ms** |
///
/// A diferença é um bit: `constraint_set1_flag`. Sem ele o fluxo é Baseline "cheio" — que admite
/// FMO, ASO e fatias redundantes, que o DXVA de H.264 não implementa — e o MFT cai para software.
/// **Nenhum codificador deste projeto usa essas três ferramentas**, e o SDP já anuncia
/// `profile-level-id=42e0..`, que é Constrained Baseline: o S24 declara no SPS menos do que o
/// contrato promete. Marcar o bit é dizer ao decoder o que o fluxo já é.
///
/// Só o prefixo não-VCL do quadro é varrido (SPS, PPS, SEI, delimitador) — o SPS vem colado na
/// frente do IDR (`idr_com_csd_colado` no emissor Android), e parar na primeira fatia mantém o custo
/// em dezenas de bytes por quadro. O byte mexido é o de restrições, logo depois do `profile_idc`;
/// passar de `0x80` para `0xC0` não cria sequência `00 00 0x`, então não há prevenção de emulação a
/// refazer.
pub fn declarar_constrained_baseline(annexb: &mut [u8]) -> bool {
    let mut mexeu = false;
    let mut i = 0usize;
    while i + 3 < annexb.len() {
        let corpo = if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            i + 3
        } else if i + 4 < annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            i + 4
        } else {
            i += 1;
            continue;
        };
        if corpo >= annexb.len() {
            break;
        }
        match annexb[corpo] & 0x1f {
            7 => {
                // [cabeçalho][profile_idc][restrições][level_idc]
                if corpo + 3 < annexb.len() && annexb[corpo + 1] == 66 && annexb[corpo + 2] & 0x40 == 0
                {
                    annexb[corpo + 2] |= 0x40;
                    mexeu = true;
                }
            }
            // Parâmetros, SEI e delimitador vêm antes da imagem; a primeira fatia encerra a busca.
            6 | 8 | 9 => {}
            _ => break,
        }
        i = corpo + 1;
    }
    mexeu
}

/// O que [`declarar_restricao_de_bitstream`] fez num quadro: o SPS como chegou e como foi ao
/// decodificador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemendoDaRestricao {
    pub antes: ResumoSps,
    pub depois: ResumoSps,
}

/// **Declara `bitstream_restriction` com `max_num_reorder_frames = 0` num SPS Baseline que não a
/// declara**, trocando o SPS dentro do quadro. `None` quando não mexeu.
///
/// # Por que isto existe: o decodificador da Microsoft segura ~5 quadros sem ela
///
/// Sem a restrição, a norma (H.264, E.2.1) manda o decodificador inferir `max_dec_frame_buffering`
/// pelo teto do nível, e o *Microsoft H264 Video Decoder MFT* segura quadros antes de entregar — o
/// `MF_LOW_LATENCY`, que ele aceita, não basta, e o `CODECAPI_AVLowLatencyMode` ele recusa
/// (`plugins/obs/README.md`). Medido nesta casca em 21/09/2026 (a T2 da S7,
/// `docs/som-no-receptor.md` §20.15), com a origem da sonda (VideoToolbox, SPS de 10 bytes sem
/// VUI): fila→tela de **161 ms** e o som 81 ms **antes** da imagem; a mesma origem com o SPS
/// reescrito pelo `RemendoDeSPS` do Mac: fila→tela de **4,2 ms**, o som 77 ms depois, e nenhum
/// tranco. O emissor do Mac remenda o próprio SPS antes de mandar; este remendo faz o receptor não
/// depender disso.
///
/// # Por que só Baseline, e por que zero
///
/// Baseline não tem fatia B: a única reordenação possível seria um fluxo só de P com a ordem de
/// apresentação fora da de decodificação, que nenhum codificador deste projeto (nem o WebRTC)
/// produz, e o SDP negocia `profile-level-id=42e0..` (Constrained Baseline).
/// `max_dec_frame_buffering` recebe `max_num_ref_frames`, como no `RemendoDeSPS` do Mac. Main e
/// High não são tocados: lá a reordenação é possível, e só o emissor sabe.
///
/// O VUI que já existe é copiado bit a bit até o flag da restrição; sem VUI, ele é escrito do zero
/// **sem** `video_signal_type` (o receptor não sabe a faixa de cor, e não inventa). O SPS novo é
/// relido e conferido (perfil, nível, dimensões, a cor e a restrição); se não conferir, nada muda.
/// Só o prefixo não-VCL do quadro é varrido, como em [`declarar_constrained_baseline`].
pub fn declarar_restricao_de_bitstream(annexb: &mut Vec<u8>) -> Option<RemendoDaRestricao> {
    // (início do corpo da NAL, fim) de cada NAL antes da primeira fatia.
    let mut nals: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    let mut aberta: Option<usize> = None;
    while i + 2 < annexb.len() {
        let tamanho = if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            3
        } else if i + 3 < annexb.len()
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
        if let Some(ini) = aberta.take() {
            nals.push((ini, i));
        }
        let corpo = i + tamanho;
        if corpo >= annexb.len() || !matches!(annexb[corpo] & 0x1f, 6..=9) {
            // A primeira fatia (ou o que não for parâmetro) encerra a busca.
            break;
        }
        aberta = Some(corpo);
        i = corpo;
    }
    if let Some(ini) = aberta {
        nals.push((ini, annexb.len()));
    }

    let mut feito: Option<RemendoDaRestricao> = None;
    // De trás para a frente, para as posições das NALs anteriores continuarem valendo.
    for &(ini, fim) in nals.iter().rev() {
        if annexb[ini] & 0x1f != 7 {
            continue;
        }
        if let Some((novo, antes, depois)) = reescrever_com_restricao(&annexb[ini..fim]) {
            annexb.splice(ini..fim, novo);
            feito = Some(RemendoDaRestricao { antes, depois });
        }
    }
    feito
}

/// Reescreve uma NAL de SPS (com o byte de cabeçalho, sem start code). Ver
/// [`declarar_restricao_de_bitstream`].
fn reescrever_com_restricao(nal: &[u8]) -> Option<(Vec<u8>, ResumoSps, ResumoSps)> {
    let (antes, pos) = analisar_com_posicoes(nal)?;
    if antes.profile_idc != 66 || antes.tem_bitstream_restriction {
        return None;
    }
    let rbsp = desescapar(&nal[1..]);
    let mut e = Escritor::default();
    if antes.tem_vui {
        // O VUI existe e não tem a restrição: copia tudo até o flag dela.
        e.copiar(&rbsp, pos.bit_do_flag_de_restricao?);
    } else {
        e.copiar(&rbsp, pos.bit_do_flag_de_vui);
        e.u(1, 1); // vui_parameters_present_flag
        e.u(0, 1); // aspect_ratio_info_present_flag
        e.u(0, 1); // overscan_info_present_flag
        e.u(0, 1); // video_signal_type_present_flag: o receptor não sabe, e não inventa
        e.u(0, 1); // chroma_loc_info_present_flag
        e.u(0, 1); // timing_info_present_flag
        e.u(0, 1); // nal_hrd_parameters_present_flag
        e.u(0, 1); // vcl_hrd_parameters_present_flag
        e.u(0, 1); // pic_struct_present_flag
    }
    e.u(1, 1); // bitstream_restriction_flag
    e.u(1, 1); // motion_vectors_over_pic_boundaries_flag
    e.ue(0); // max_bytes_per_pic_denom (sem limite)
    e.ue(0); // max_bits_per_mb_denom (sem limite)
    e.ue(16); // log2_max_mv_length_horizontal
    e.ue(16); // log2_max_mv_length_vertical
    e.ue(0); // max_num_reorder_frames  <-- o conserto
    e.ue(pos.max_num_ref_frames); // max_dec_frame_buffering
    e.fechar_rbsp();

    let mut novo = vec![nal[0]];
    novo.extend(escapar(&e.bytes));
    // A releitura: um SPS errado não degrada a imagem, apaga.
    let depois = analisar(&novo)?;
    let confere = depois.profile_idc == antes.profile_idc
        && depois.constraint_flags == antes.constraint_flags
        && depois.level_idc == antes.level_idc
        && depois.largura == antes.largura
        && depois.altura == antes.altura
        && depois.tem_video_signal_type == antes.tem_video_signal_type
        && depois.full_range == antes.full_range
        && depois.cor == antes.cor
        && depois.tem_bitstream_restriction
        && depois.max_num_reorder_frames == Some(0)
        && depois.max_dec_frame_buffering == Some(pos.max_num_ref_frames);
    confere.then_some((novo, antes, depois))
}

/// O escritor de bits do remendo.
#[derive(Default)]
struct Escritor {
    bytes: Vec<u8>,
    bits: usize,
}

impl Escritor {
    fn bit(&mut self, b: bool) {
        if self.bits % 8 == 0 {
            self.bytes.push(0);
        }
        if b {
            let ultimo = self.bytes.len() - 1;
            self.bytes[ultimo] |= 0x80 >> (self.bits % 8);
        }
        self.bits += 1;
    }
    fn u(&mut self, v: u32, n: usize) {
        for k in (0..n).rev() {
            self.bit((v >> k) & 1 == 1);
        }
    }
    fn ue(&mut self, v: u32) {
        let x = u64::from(v) + 1;
        let n = 64 - x.leading_zeros() as usize;
        for _ in 0..n - 1 {
            self.bit(false);
        }
        for k in (0..n).rev() {
            self.bit((x >> k) & 1 == 1);
        }
    }
    fn copiar(&mut self, rbsp: &[u8], nbits: usize) {
        for p in 0..nbits {
            self.bit((rbsp[p / 8] >> (7 - (p % 8))) & 1 == 1);
        }
    }
    /// `rbsp_stop_one_bit` e o alinhamento.
    fn fechar_rbsp(&mut self) {
        self.bit(true);
        while self.bits % 8 != 0 {
            self.bit(false);
        }
    }
}

/// Refaz a prevenção de emulação de start code (`00 00 0x` com x ≤ 3 vira `00 00 03 0x`).
fn escapar(rbsp: &[u8]) -> Vec<u8> {
    let mut saida = Vec::with_capacity(rbsp.len() + 4);
    let mut zeros = 0usize;
    for &byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            saida.push(3);
            zeros = 0;
        }
        saida.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    saida
}

/// O que o receptor faz com o SPS de um quadro antes de entregá-lo ao decodificador: marca
/// Constrained Baseline e declara a restrição de bitstream, nessa ordem. É o único ponto que o
/// `receptor.rs` chama, e é o que os testes conferem de ponta a ponta.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Preparo {
    pub constrained: bool,
    pub restricao: Option<RemendoDaRestricao>,
}

pub fn preparar_para_o_decoder(annexb: &mut Vec<u8>) -> Preparo {
    let constrained = declarar_constrained_baseline(annexb);
    let restricao = declarar_restricao_de_bitstream(annexb);
    Preparo { constrained, restricao }
}

/// Acha o primeiro SPS num fluxo Annex-B e o resume. `None` se não houver SPS legível.
pub fn resumir(annexb: &[u8]) -> Option<ResumoSps> {
    let bruto = primeiro_sps(annexb)?;
    analisar(bruto)
}

/// Extrai a NAL de tipo 7 (sem o start code, com o byte de cabeçalho).
pub fn primeiro_sps(annexb: &[u8]) -> Option<&[u8]> {
    let mut i = 0usize;
    let mut inicio: Option<(usize, u8)> = None;
    let mut saida: Option<(usize, usize)> = None;
    while i + 2 < annexb.len() {
        let tamanho = if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            3
        } else if i + 3 < annexb.len()
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
        let corpo = i + tamanho;
        if corpo >= annexb.len() {
            break;
        }
        if let Some((ini, tipo)) = inicio.take() {
            if tipo == 7 {
                saida = Some((ini, i));
                break;
            }
        }
        inicio = Some((corpo, annexb[corpo] & 0x1f));
        i = corpo;
    }
    if saida.is_none() {
        if let Some((ini, 7)) = inicio {
            saida = Some((ini, annexb.len()));
        }
    }
    saida.map(|(a, b)| &annexb[a..b])
}

fn analisar(nal: &[u8]) -> Option<ResumoSps> {
    analisar_com_posicoes(nal).map(|(r, _)| r)
}

/// Onde, no RBSP desescapado, ficam os campos que o remendo da restrição precisa.
#[derive(Debug, Clone, Copy)]
struct Posicoes {
    /// O bit do `vui_parameters_present_flag`.
    bit_do_flag_de_vui: usize,
    /// O bit do `bitstream_restriction_flag`, quando o VUI foi lido até ele.
    bit_do_flag_de_restricao: Option<usize>,
    max_num_ref_frames: u32,
}

fn analisar_com_posicoes(nal: &[u8]) -> Option<(ResumoSps, Posicoes)> {
    if nal.len() < 4 || nal[0] & 0x1f != 7 {
        return None;
    }
    let rbsp = desescapar(&nal[1..]);
    let mut b = Bits::new(&rbsp);

    let profile_idc = b.u(8)? as u8;
    let constraint_flags = b.u(8)? as u8;
    let level_idc = b.u(8)? as u8;
    b.ue()?; // seq_parameter_set_id

    let mut chroma_format_idc = 1u32;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = b.ue()?;
        if chroma_format_idc == 3 {
            b.u(1)?; // separate_colour_plane_flag
        }
        b.ue()?; // bit_depth_luma_minus8
        b.ue()?; // bit_depth_chroma_minus8
        b.u(1)?; // qpprime_y_zero_transform_bypass_flag
        if b.u(1)? == 1 {
            let listas = if chroma_format_idc != 3 { 8 } else { 12 };
            for i in 0..listas {
                if b.u(1)? == 1 {
                    pular_lista_de_escala(&mut b, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }

    b.ue()?; // log2_max_frame_num_minus4
    let pic_order_cnt_type = b.ue()?;
    if pic_order_cnt_type == 0 {
        b.ue()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if pic_order_cnt_type == 1 {
        b.u(1)?; // delta_pic_order_always_zero_flag
        b.se()?; // offset_for_non_ref_pic
        b.se()?; // offset_for_top_to_bottom_field
        let ciclo = b.ue()?;
        for _ in 0..ciclo {
            b.se()?;
        }
    }
    let max_num_ref_frames = b.ue()?;
    b.u(1)?; // gaps_in_frame_num_value_allowed_flag

    // Contas com checagem (revisão da S7, B1): o parser roda em todo quadro com SPS, e um SPS
    // malformado de um emissor pareado não pode derrubar a thread da sessão numa compilação de debug.
    let largura_mbs = b.ue()?.checked_add(1)?;
    let altura_map_units = b.ue()?.checked_add(1)?;
    let frame_mbs_only = b.u(1)?;
    if frame_mbs_only == 0 {
        b.u(1)?; // mb_adaptive_frame_field_flag
    }
    b.u(1)?; // direct_8x8_inference_flag

    let (mut esq, mut dir, mut topo, mut base) = (0u32, 0u32, 0u32, 0u32);
    if b.u(1)? == 1 {
        esq = b.ue()?;
        dir = b.ue()?;
        topo = b.ue()?;
        base = b.ue()?;
    }

    // O recorte é contado em unidades de croma, e a unidade depende do formato — a conta com o
    // fator errado dá uma largura plausível e ligeiramente errada, que é o tipo de erro que
    // ninguém percebe.
    let (sub_w, sub_h) = match chroma_format_idc {
        0 => (1u32, 1u32),
        2 => (2, 1),
        3 => (1, 1),
        _ => (2, 2),
    };
    let mult_v = if frame_mbs_only == 1 { 1 } else { 2 };
    let largura = largura_mbs
        .checked_mul(16)?
        .checked_sub(esq.checked_add(dir)?.checked_mul(sub_w)?)?;
    let altura = altura_map_units
        .checked_mul(mult_v)?
        .checked_mul(16)?
        .checked_sub(topo.checked_add(base)?.checked_mul(sub_h)?.checked_mul(mult_v)?)?;

    let mut resumo = ResumoSps {
        bytes: nal.len(),
        profile_idc,
        constraint_flags,
        level_idc,
        largura,
        altura,
        tem_vui: false,
        tem_video_signal_type: false,
        full_range: None,
        cor: None,
        tem_bitstream_restriction: false,
        max_num_reorder_frames: None,
        max_dec_frame_buffering: None,
    };

    let mut posicoes =
        Posicoes { bit_do_flag_de_vui: b.pos, bit_do_flag_de_restricao: None, max_num_ref_frames };
    if b.u(1)? == 1 {
        resumo.tem_vui = true;
        ler_vui(&mut b, &mut resumo, &mut posicoes.bit_do_flag_de_restricao);
    }
    Some((resumo, posicoes))
}

/// Lê o VUI **sem** poder falhar para fora: um VUI truncado ou com um campo que este parser não
/// entende deve deixar o resumo com o que já foi lido, não apagar o SPS inteiro. Os campos que
/// interessam (`video_signal_type`, `bitstream_restriction`) só valem se tiverem sido alcançados,
/// e é isso que os `Option`/`bool` dizem.
fn ler_vui(b: &mut Bits, r: &mut ResumoSps, bit_da_restricao: &mut Option<usize>) -> Option<()> {
    if b.u(1)? == 1 {
        let idc = b.u(8)?;
        if idc == 255 {
            b.u(16)?;
            b.u(16)?;
        }
    }
    if b.u(1)? == 1 {
        b.u(1)?; // overscan_appropriate_flag
    }
    if b.u(1)? == 1 {
        r.tem_video_signal_type = true;
        b.u(3)?; // video_format
        r.full_range = Some(b.u(1)? == 1);
        if b.u(1)? == 1 {
            let p = b.u(8)? as u8;
            let t = b.u(8)? as u8;
            let m = b.u(8)? as u8;
            r.cor = Some((p, t, m));
        }
    }
    if b.u(1)? == 1 {
        b.ue()?;
        b.ue()?;
    }
    if b.u(1)? == 1 {
        b.u(32)?; // num_units_in_tick
        b.u(32)?; // time_scale
        b.u(1)?; // fixed_frame_rate_flag
    }
    let nal_hrd = b.u(1)? == 1;
    if nal_hrd {
        pular_hrd(b)?;
    }
    let vcl_hrd = b.u(1)? == 1;
    if vcl_hrd {
        pular_hrd(b)?;
    }
    if nal_hrd || vcl_hrd {
        b.u(1)?; // low_delay_hrd_flag
    }
    b.u(1)?; // pic_struct_present_flag
    // A posição só vale se o flag couber no RBSP: um SPS que acaba antes dele não é remendado (o
    // mesmo que o C do plugin faz; revisão da S7, N2).
    let pos_do_flag = b.pos;
    let flag = b.u(1)?;
    *bit_da_restricao = Some(pos_do_flag);
    if flag == 1 {
        r.tem_bitstream_restriction = true;
        b.u(1)?; // motion_vectors_over_pic_boundaries_flag
        b.ue()?; // max_bytes_per_pic_denom
        b.ue()?; // max_bits_per_mb_denom
        b.ue()?; // log2_max_mv_length_horizontal
        b.ue()?; // log2_max_mv_length_vertical
        r.max_num_reorder_frames = Some(b.ue()?);
        r.max_dec_frame_buffering = Some(b.ue()?);
    }
    Some(())
}

fn pular_hrd(b: &mut Bits) -> Option<()> {
    let cpb = b.ue()?.checked_add(1)?;
    b.u(4)?;
    b.u(4)?;
    for _ in 0..cpb {
        b.ue()?;
        b.ue()?;
        b.u(1)?;
    }
    b.u(5)?;
    b.u(5)?;
    b.u(5)?;
    b.u(5)?;
    Some(())
}

fn pular_lista_de_escala(b: &mut Bits, tamanho: usize) -> Option<()> {
    let mut ultimo = 8i32;
    let mut proximo = 8i32;
    for _ in 0..tamanho {
        if proximo != 0 {
            let delta = b.se()?;
            proximo = (i64::from(ultimo) + i64::from(delta) + 256).rem_euclid(256) as i32;
        }
        if proximo != 0 {
            ultimo = proximo;
        }
    }
    Some(())
}

/// Desfaz a prevenção de emulação de start code (`00 00 03` → `00 00`).
///
/// Sem isto, um SPS que contenha o padrão — o que acontece de verdade em campos de recorte e em
/// VUI — é lido a partir de um byte deslocado, e o parser devolve números plausíveis e errados.
fn desescapar(bruto: &[u8]) -> Vec<u8> {
    let mut saida = Vec::with_capacity(bruto.len());
    let mut zeros = 0usize;
    for &byte in bruto {
        if zeros >= 2 && byte == 0x03 {
            zeros = 0;
            continue;
        }
        if byte == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        saida.push(byte);
    }
    saida
}

struct Bits<'a> {
    dados: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(dados: &'a [u8]) -> Self {
        Bits { dados, pos: 0 }
    }

    fn u(&mut self, n: usize) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut v: u32 = 0;
        for _ in 0..n {
            let byte = *self.dados.get(self.pos / 8)?;
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | bit as u32;
            self.pos += 1;
        }
        Some(v)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0usize;
        loop {
            let bit = self.u(1)?;
            if bit == 1 {
                break;
            }
            zeros += 1;
            // Um `ue(v)` legítimo em 32 bits tem no máximo 31 zeros à frente; com 32, o `1 << zeros`
            // abaixo transbordaria (revisão da S7, B1). E um fluxo corrompido não laça até o fim.
            if zeros >= 32 {
                return None;
            }
        }
        if zeros == 0 {
            return Some(0);
        }
        let resto = self.u(zeros)?;
        Some((1u32 << zeros) - 1 + resto)
    }

    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        let magnitude = ((k + 1) / 2) as i32;
        Some(if k % 2 == 1 { magnitude } else { -magnitude })
    }
}

#[cfg(test)]
mod testes {

    /// O quadro de abertura do S24 como ele chega: SPS Baseline sem `constraint_set1`, PPS, IDR.
    fn idr_do_s24() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, 0x67, 66, 0x80, 42, 0xAA, 0xBB];
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]);
        v.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x33]);
        v
    }

    #[test]
    fn baseline_sem_marca_ganha_constraint_set1_e_so_isso_muda() {
        let original = idr_do_s24();
        let mut q = original.clone();
        assert!(super::declarar_constrained_baseline(&mut q));
        assert_eq!(q[6], 0xC0, "o byte de restrições");
        let diferentes: Vec<usize> = (0..q.len()).filter(|&i| q[i] != original[i]).collect();
        assert_eq!(diferentes, vec![6], "um byte só, e é o de restrições");
    }

    #[test]
    fn ja_constrained_nao_mexe() {
        let mut q = idr_do_s24();
        q[6] = 0xC0; // o x264: constraint=110000
        let antes = q.clone();
        assert!(!super::declarar_constrained_baseline(&mut q));
        assert_eq!(q, antes);
    }

    #[test]
    fn main_e_high_nao_sao_tocados() {
        for perfil in [77u8, 100] {
            let mut q = idr_do_s24();
            q[5] = perfil;
            let antes = q.clone();
            assert!(!super::declarar_constrained_baseline(&mut q));
            assert_eq!(q, antes, "perfil {perfil}");
        }
    }

    /// A busca para na primeira fatia: um `0x67` dentro dos dados de uma fatia não é SPS.
    #[test]
    fn quadro_p_nao_e_varrido_alem_da_fatia() {
        let mut q = vec![0, 0, 0, 1, 0x41, 0x9A, 0x00, 0x00, 0x01, 0x67, 66, 0x80, 42];
        let antes = q.clone();
        assert!(!super::declarar_constrained_baseline(&mut q));
        assert_eq!(q, antes);
    }

    use super::*;

    /// SPS de 1920x1080, Constrained Baseline nível 4.0, com VUI que **declara** a restrição de
    /// bitstream (reorder=2, dpb=4) e **não** declara `video_signal_type`.
    ///
    /// É o caso mais útil que existe para este parser, e por acidente feliz: ele é exatamente o
    /// corolário que custou um desenho no M4 — "tem VUI?" e "declara a restrição?" são perguntas
    /// diferentes, e aqui as duas seções se separam em direções opostas (a restrição está lá, a
    /// faixa de cor não). Os valores foram conferidos byte a byte contra um segundo parser,
    /// escrito à parte em Python, antes de virarem asserção.
    const SPS_1080P: &[u8] = &[
        0x67, 0x42, 0xc0, 0x28, 0xd9, 0x00, 0x78, 0x02, 0x27, 0xe5, 0x84, 0x00, 0x00, 0x03, 0x00,
        0x04, 0x00, 0x00, 0x03, 0x00, 0xf0, 0x3c, 0x60, 0xc6, 0x58,
    ];

    #[test]
    fn le_perfil_nivel_e_dimensoes() {
        let r = analisar(SPS_1080P).expect("SPS legível");
        assert_eq!(r.profile_idc, 66, "Constrained Baseline");
        assert_eq!(r.level_idc, 40, "nível 4.0");
        assert_eq!(r.largura, 1920);
        assert_eq!(r.altura, 1080);
    }

    #[test]
    fn le_a_restricao_de_bitstream_quando_ela_existe() {
        let r = analisar(SPS_1080P).expect("SPS legível");
        assert!(r.declara_a_restricao());
        assert_eq!(r.max_num_reorder_frames, Some(2));
        assert_eq!(r.max_dec_frame_buffering, Some(4));
    }

    /// A pergunta certa é sobre o campo, não sobre a seção — `docs/regras-de-frente.md`.
    #[test]
    fn ter_vui_nao_e_o_mesmo_que_declarar_a_faixa_de_cor() {
        let r = analisar(SPS_1080P).expect("SPS legível");
        assert!(r.tem_vui, "este SPS tem VUI");
        assert!(
            !r.tem_video_signal_type,
            "e mesmo assim não declara faixa de cor nem primárias — é o caso que a regra \
             conservadora do M4 teria pulado em silêncio"
        );
        assert_eq!(r.full_range, None);
        assert_eq!(r.cor, None);
    }

    #[test]
    fn acha_o_sps_dentro_de_annexb_com_pps_junto() {
        let mut fluxo = vec![0, 0, 0, 1];
        fluxo.extend_from_slice(SPS_1080P);
        fluxo.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xcb, 0x83, 0xcb, 0x20]);
        let r = resumir(&fluxo).expect("acha o SPS entre as NALs");
        assert_eq!(r.largura, 1920);
        assert!(r.declara_a_restricao());
    }

    #[test]
    fn desescapa_antes_de_ler() {
        // `00 00 03 01` tem de virar `00 00 01` na leitura.
        assert_eq!(desescapar(&[0x00, 0x00, 0x03, 0x01]), vec![0x00, 0x00, 0x01]);
        // Um `03` que não vem depois de dois zeros é dado, não escape.
        assert_eq!(desescapar(&[0x01, 0x03, 0x00]), vec![0x01, 0x03, 0x00]);
    }

    #[test]
    fn fluxo_sem_sps_nao_inventa_resposta() {
        let so_pps = [0u8, 0, 0, 1, 0x68, 0xcb, 0x83];
        assert!(resumir(&so_pps).is_none());
    }

    // --- a restrição de bitstream (a S7 do som, §20.15 e §20.19) --------------------------------

    /// O SPS da origem da sonda (`gerar-fonte.swift`, VideoToolbox): 10 bytes, Baseline nível 3.1,
    /// 1280x720, `max_num_ref_frames = 1`, **sem VUI**. É o SPS da T2 de 21/09 (o som 81 ms antes
    /// da imagem), lido pelo `tools/ler-sps.py`.
    const SPS_DA_SONDA: &[u8] = &[0x27, 0x42, 0x00, 0x1f, 0xab, 0x40, 0x28, 0x02, 0xdc, 0x80];

    /// O mesmo SPS depois do remendo desta casca, **conferido à parte** pelo `tools/ler-sps.py`
    /// (outro parser, em Python): `vui_parameters_present = True`, sem `video_signal_type`,
    /// `bitstream_restriction_flag = 1`, `max_num_reorder_frames = 0`, `max_dec_frame_buffering = 1`,
    /// 1280x720, perfil 66, nível 31.
    const SPS_DA_SONDA_REMENDADO: &[u8] = &[
        0x27, 0x42, 0x00, 0x1f, 0xab, 0x40, 0x28, 0x02, 0xdd, 0x00, 0xf0, 0x88, 0x46, 0xa0,
    ];

    /// O quadro de abertura como a sonda manda: SPS, PPS e o começo de um IDR.
    fn idr_da_sonda() -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1];
        v.extend_from_slice(SPS_DA_SONDA);
        v.extend_from_slice(&[0, 0, 0, 1, 0x28, 0xce, 0x3c, 0x80]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x25, 0x88, 0x84, 0x00, 0x33, 0x67, 0x42]);
        v
    }

    /// **O teste que reprova no código de antes**: o quadro que vai ao decodificador tem de
    /// declarar a restrição. Até 21/09 o `preparar` era só o `declarar_constrained_baseline`, e o
    /// SPS ia sem ela (conferido desligando o passo novo: este teste reprova).
    #[test]
    fn o_quadro_que_vai_ao_decoder_declara_a_restricao() {
        let mut q = idr_da_sonda();
        let p = preparar_para_o_decoder(&mut q);
        assert!(p.constrained, "o constraint_set1 continua sendo marcado");
        let r = resumir(&q).expect("o SPS continua legível");
        assert!(r.declara_a_restricao(), "o SPS que vai ao decoder: {}", r.linha());
        assert_eq!(r.max_num_reorder_frames, Some(0));
        assert_eq!(r.max_dec_frame_buffering, Some(1));
        assert_eq!((r.profile_idc, r.level_idc, r.largura, r.altura), (66, 31, 1280, 720));
        assert_eq!(r.constraint_flags, 0x40, "Constrained Baseline, e só isso");
        let remendo = p.restricao.expect("o remendo diz o que fez");
        assert!(!remendo.antes.declara_a_restricao());
        assert_eq!(remendo.depois, r);
    }

    /// O SPS novo é, byte a byte, o que o parser independente leu; o resto do quadro não muda.
    #[test]
    fn so_o_sps_muda_e_ele_sai_como_o_parser_independente_leu() {
        let mut q = idr_da_sonda();
        assert!(declarar_restricao_de_bitstream(&mut q).is_some());
        let mut esperado = vec![0, 0, 0, 1];
        esperado.extend_from_slice(SPS_DA_SONDA_REMENDADO);
        esperado.extend_from_slice(&idr_da_sonda()[4 + SPS_DA_SONDA.len()..]);
        assert_eq!(q, esperado);
        // Uma segunda passada não mexe: a restrição já está lá.
        let antes = q.clone();
        assert!(declarar_restricao_de_bitstream(&mut q).is_none());
        assert_eq!(q, antes);
    }

    #[test]
    fn sps_que_ja_declara_a_restricao_nao_e_tocado() {
        let mut fluxo = vec![0, 0, 0, 1];
        fluxo.extend_from_slice(SPS_1080P); // reorder=2 dpb=4: quem decidiu foi o emissor
        fluxo.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xcb, 0x83, 0xcb, 0x20]);
        let antes = fluxo.clone();
        assert!(declarar_restricao_de_bitstream(&mut fluxo).is_none());
        assert_eq!(fluxo, antes);
    }

    #[test]
    fn main_e_high_sem_restricao_nao_sao_tocados() {
        for perfil in [77u8, 100] {
            let mut q = idr_da_sonda();
            q[5] = perfil;
            let antes = q.clone();
            assert!(declarar_restricao_de_bitstream(&mut q).is_none(), "perfil {perfil}");
            assert_eq!(q, antes, "perfil {perfil}");
        }
    }

    /// O caminho 2: o VUI existe (faixa cheia declarada, como o VideoToolbox faz com entrada
    /// `420f`) e não tem a restrição. O VUI é copiado, e a faixa de cor continua a mesma.
    #[test]
    fn vui_sem_restricao_guarda_a_cor_e_ganha_a_restricao() {
        let rbsp = desescapar(&SPS_DA_SONDA[1..]);
        let (_, pos) = analisar_com_posicoes(SPS_DA_SONDA).expect("SPS da sonda");
        let mut e = Escritor::default();
        e.copiar(&rbsp, pos.bit_do_flag_de_vui);
        e.u(1, 1); // VUI
        e.u(0, 1); // aspect
        e.u(0, 1); // overscan
        e.u(1, 1); // video_signal_type
        e.u(5, 3); // video_format
        e.u(1, 1); // full_range
        e.u(1, 1); // colour_description
        e.u(1, 8);
        e.u(1, 8);
        e.u(1, 8);
        e.u(0, 1); // chroma_loc
        e.u(0, 1); // timing
        e.u(0, 1); // nal_hrd
        e.u(0, 1); // vcl_hrd
        e.u(0, 1); // pic_struct
        e.u(0, 1); // bitstream_restriction_flag = 0
        e.fechar_rbsp();
        let mut nal = vec![0x27];
        nal.extend(escapar(&e.bytes));
        let antes = analisar(&nal).expect("o SPS de teste é legível");
        assert!(antes.tem_vui && !antes.declara_a_restricao());
        assert_eq!((antes.full_range, antes.cor), (Some(true), Some((1, 1, 1))));

        let mut q = vec![0, 0, 1];
        q.extend_from_slice(&nal);
        q.extend_from_slice(&[0, 0, 1, 0x65, 0x88]);
        let r = declarar_restricao_de_bitstream(&mut q).expect("remendou");
        assert_eq!((r.depois.full_range, r.depois.cor), (Some(true), Some((1, 1, 1))));
        assert_eq!(r.depois.max_num_reorder_frames, Some(0));
        assert!(q.ends_with(&[0, 0, 1, 0x65, 0x88]), "a fatia não muda");
    }

    /// O caminho 2 com bytes literais, os mesmos que a prova do plugin do OBS confere
    /// (`plugins/obs/bancada/prova-remendo-sps.c`): o SPS da sonda com VUI de faixa cheia e cor
    /// 709 e sem a restrição, antes e depois. Os dois conferidos à parte pelo `tools/ler-sps.py`.
    #[test]
    fn o_sps_com_vui_sai_como_o_parser_independente_leu() {
        const ANTES: &[u8] =
            &[0x27, 0x42, 0x00, 0x1f, 0xab, 0x40, 0x28, 0x02, 0xdd, 0x37, 0x01, 0x01, 0x01, 0x02];
        const DEPOIS: &[u8] = &[
            0x27, 0x42, 0x00, 0x1f, 0xab, 0x40, 0x28, 0x02, 0xdd, 0x37, 0x01, 0x01, 0x01, 0x07, 0x84,
            0x42, 0x35,
        ];
        let mut q = vec![0, 0, 0, 1];
        q.extend_from_slice(ANTES);
        q.extend_from_slice(&[0, 0, 0, 1, 0x25, 0x88]);
        assert!(declarar_restricao_de_bitstream(&mut q).is_some());
        let mut esperado = vec![0, 0, 0, 1];
        esperado.extend_from_slice(DEPOIS);
        esperado.extend_from_slice(&[0, 0, 0, 1, 0x25, 0x88]);
        assert_eq!(q, esperado);
    }

    /// A busca para na primeira fatia: um `0x67` dentro dos dados de uma fatia não é SPS.
    #[test]
    fn quadro_p_nao_ganha_restricao() {
        let mut q = vec![0, 0, 0, 1, 0x41, 0x9A, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1f];
        let antes = q.clone();
        assert!(declarar_restricao_de_bitstream(&mut q).is_none());
        assert_eq!(q, antes);
    }

    fn de_hex(h: &str) -> Vec<u8> {
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex")).collect()
    }

    /// Os dois SPS malformados da revisão (B1): numa compilação de debug (a do `cargo test`), o
    /// parser antigo entrava em pânico — `attempt to subtract with overflow` (o recorte de 1 000
    /// macroblocos, maior que a largura) e `attempt to shift left with overflow` (um `ue` com 32
    /// zeros no `chroma_loc`). Agora dão `None`, e o quadro sai intocado.
    #[test]
    fn sps_malformado_nao_entra_em_panico_e_fica_como_veio() {
        for hex in ["2742001ff402802df007d3a0", "6742c028d900780227e58c00000300020004000003000810"] {
            let nal = de_hex(hex);
            let mut q = vec![0, 0, 0, 1];
            q.extend_from_slice(&nal);
            q.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
            let antes = q.clone();
            assert!(declarar_restricao_de_bitstream(&mut q).is_none(), "{hex}");
            assert_eq!(q, antes, "{hex}");
            let mut p = q.clone();
            let _ = preparar_para_o_decoder(&mut p);
            let _ = resumir(&p);
        }
    }

    /// Dois casos que a revisão conferiu com o leitor do `ffmpeg` (`trace_headers`) e com a
    /// decodificação de verdade (N1): a releitura do remendo é pelo mesmo parser, e estes fixam o
    /// caminho do VUI por fora dele. Um 1080p com recorte, SAR 255, cor, `chroma_loc`, timing,
    /// `nal_hrd` e `vcl_hrd`; e um com escape de emulação na entrada e na saída.
    #[test]
    fn os_sps_que_o_ffmpeg_conferiu_saem_iguais() {
        let casos = [
            (
                "27420028ab403c0113f2ffe000200036e020203e000007d20001d4c1a460026980058bc004d4000b186f7be3460026980058bd7bdf02",
                "27420028ab403c0113f2ffe000200036e020203e000007d20001d4c1a460026980058bc004d4000b186f7be3460026980058bd7bdf07844235",
            ),
            (
                "2742001fab402802dd82880000030008000003001020",
                "2742001fab402802dd82880000030008000003001078442350",
            ),
        ];
        for (antes, depois) in casos {
            let mut q = vec![0, 0, 0, 1];
            q.extend_from_slice(&de_hex(antes));
            q.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
            assert!(declarar_restricao_de_bitstream(&mut q).is_some(), "{antes}");
            let mut esperado = vec![0, 0, 0, 1];
            esperado.extend_from_slice(&de_hex(depois));
            esperado.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
            assert_eq!(q, esperado, "{antes}");
        }
    }

    #[test]
    fn o_escape_de_emulacao_volta_ao_que_era() {
        for bruto in [vec![0u8, 0, 1, 0, 0, 0, 0, 3, 7], vec![0, 0, 0, 0, 2], vec![1, 2, 3]] {
            assert_eq!(desescapar(&escapar(&bruto)), bruto);
            let e = escapar(&bruto);
            assert!(!e.windows(3).any(|w| w[0] == 0 && w[1] == 0 && w[2] <= 2), "{e:?}");
        }
    }
}
