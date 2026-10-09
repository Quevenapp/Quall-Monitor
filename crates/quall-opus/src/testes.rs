//! A prova de que existe um bitstream de Opus, e de que ele diz o que o `fmtp` promete.
//!
//! Tudo aqui parte de **tom sintético gerado neste arquivo**. Nenhum teste abre microfone, áudio
//! de sistema ou arquivo de mídia da máquina — a regra do `docs/regras-de-frente.md` sobre não
//! capturar o anfitrião vale para o som, e vale com força maior porque um buffer de áudio não
//! carrega no nome o que tem dentro.

use super::*;

/// Os dois presets de `quall_core::track`, repetidos aqui como números.
///
/// **Repetidos de propósito, e não importados.** Este crate não depende do `quall-core` — é o que
/// permite medi-lo sozinho na tabela de tamanho. Se os presets de lá mudarem, estes testes
/// continuam provando o que o Opus faz nestas taxas, que é o que interessa; a ligação entre os
/// dois números é feita na sonda, onde os dois crates se encontram.
const TAXA_HZ: u32 = 48_000;
const QUADRO_MS: u32 = 20;
const AMOSTRAS_POR_CANAL: usize = (TAXA_HZ as usize * QUADRO_MS as usize) / 1000; // 960

const MIC_CANAIS: u8 = 1;
const MIC_BITS: u32 = 32_000;
const SISTEMA_CANAIS: u8 = 2;
const SISTEMA_BITS: u32 = 128_000;

/// Um seno, intercalado em `canais`.
///
/// Determinístico e contínuo entre quadros: a fase sai do índice **absoluto** da amostra, como na
/// sonda. Sem isso haveria um salto de fase na emenda, e o encoder gastaria bits com um estalo
/// que nós mesmos teríamos criado.
fn tom(indice_do_quadro: u64, canais: u8, hz: f64) -> Vec<i16> {
    let base = indice_do_quadro * AMOSTRAS_POR_CANAL as u64;
    let mut v = Vec::with_capacity(AMOSTRAS_POR_CANAL * usize::from(canais));
    for i in 0..AMOSTRAS_POR_CANAL {
        let n = base + i as u64;
        let fase = 2.0 * std::f64::consts::PI * hz * (n as f64) / f64::from(TAXA_HZ);
        let a = (fase.sin() * 0.5 * f64::from(i16::MAX)) as i16;
        for _ in 0..canais {
            v.push(a);
        }
    }
    v
}

/// Codifica `quantos` quadros seguidos e devolve os pacotes.
///
/// Seguidos importa: o Opus é preditivo e o primeiro quadro não é representativo do fluxo — o
/// encoder ainda está convergindo, e é comum ele escolher outro modo no começo.
fn correr(
    canais: u8,
    bits: u32,
    aplicacao: Aplicacao,
    fec: bool,
    perda: u8,
    quantos: u64,
) -> Vec<Vec<u8>> {
    let mut enc = Codificador::novo(TAXA_HZ, canais, aplicacao).expect("criar codificador");
    enc.definir_taxa_de_bits(bits).expect("taxa");
    enc.definir_fec_embutido(fec).expect("fec");
    enc.definir_perda_esperada(perda).expect("perda");
    enc.definir_dtx(false).expect("dtx");

    let mut saida = Vec::new();
    let mut buf = vec![0u8; 4000];
    for i in 0..quantos {
        let pcm = tom(i, canais, 440.0);
        let n = enc.codificar(&pcm, &mut buf).expect("codificar");
        saida.push(buf[..n].to_vec());
    }
    saida
}

// -----------------------------------------------------------------------------------------
// O básico: a libopus vendorizada está viva
// -----------------------------------------------------------------------------------------

#[test]
fn a_libopus_vendorizada_responde_e_diz_a_versao() {
    let v = versao();
    assert!(
        v.contains("1.5.2"),
        "a libopus vendorizada devia ser a 1.5.2, e disse: {v}"
    );
}

/// O bitstream existe. É o teste que a §13 do `docs/audio.md` dizia não existir.
#[test]
fn um_quadro_de_opus_nasce_com_tamanho_plausivel() {
    let pacotes = correr(MIC_CANAIS, MIC_BITS, Aplicacao::Voz, true, 5, 25);
    let ultimo = pacotes.last().expect("nenhum pacote");
    // 32 kbit/s × 20 ms = 640 bits = 80 bytes. O VBR passeia em volta disso; o que interessa é
    // não ser nem 1 byte (pacote vazio de DTX) nem centenas.
    assert!(
        (20..=200).contains(&ultimo.len()),
        "quadro de 20 ms a 32 kbit/s saiu com {} bytes",
        ultimo.len()
    );
}

// -----------------------------------------------------------------------------------------
// O TOC — a prova que esta frente veio buscar
// -----------------------------------------------------------------------------------------

/// **O preset do microfone opera em SILK.** Era afirmação de documentação; agora é o byte.
///
/// É esta linha que sustenta `useinbandfec=1` no `PRESET_MICROFONE`: o LBRR só existe em SILK e
/// híbrido, e sem esta medição a decisão do FEC era argumento, não fato.
#[test]
fn o_preset_do_microfone_opera_em_silk_e_permite_fec() {
    let pacotes = correr(MIC_CANAIS, MIC_BITS, Aplicacao::Voz, true, 10, 50);

    // O primeiro quadro fica de fora: o encoder ainda está convergindo.
    for (i, p) in pacotes.iter().enumerate().skip(5) {
        let toc = Toc::do_pacote(p).expect("TOC");
        assert!(
            toc.modo().permite_lbrr(),
            "quadro {i}: o preset de microfone caiu em {:?}, onde o LBRR não existe — \
             `useinbandfec=1` seria uma promessa vazia. TOC = {toc:?}",
            toc.modo()
        );
        assert!(!toc.estereo, "quadro {i}: o preset de microfone é mono");
        assert_eq!(
            toc.duracao_do_quadro_us(),
            20_000,
            "quadro {i}: o quadro tem de ser de 20 ms"
        );
    }
}

/// **O preset de áudio de sistema opera em CELT** — onde o LBRR não existe.
///
/// É o outro lado da mesma decisão: `useinbandfec=0` no `PRESET_AUDIO_DO_SISTEMA` não é
/// preferência, é o único valor honesto. Declarar 1 ali seria o defeito do SPS sem
/// `bitstream_restriction` do M4, num codec diferente.
#[test]
fn o_preset_de_audio_do_sistema_opera_em_celt_onde_nao_ha_fec() {
    let pacotes = correr(SISTEMA_CANAIS, SISTEMA_BITS, Aplicacao::Audio, false, 0, 50);

    for (i, p) in pacotes.iter().enumerate().skip(5) {
        let toc = Toc::do_pacote(p).expect("TOC");
        assert_eq!(
            toc.modo(),
            Modo::Celt,
            "quadro {i}: música em estéreo a 128 kbit/s devia cair em CELT; \
             veio {:?}. Se isto mudar, a decisão do FEC da §3 precisa ser relida. TOC = {toc:?}",
            toc.modo()
        );
        assert!(
            !toc.modo().permite_lbrr(),
            "quadro {i}: se houvesse LBRR aqui, `useinbandfec=0` estaria subdeclarando"
        );
        assert_eq!(
            toc.duracao_do_quadro_us(),
            20_000,
            "quadro {i}: o quadro tem de ser de 20 ms"
        );
    }
}

/// O nosso leitor de TOC e o da libopus concordam, pacote a pacote, nos dois presets.
///
/// Duas implementações independentes lendo o mesmo byte. Se um dia divergirem, é achado.
#[test]
fn o_nosso_leitor_de_toc_concorda_com_a_libopus() {
    let mut conferidos = 0;
    for (canais, bits, app) in [
        (MIC_CANAIS, MIC_BITS, Aplicacao::Voz),
        (SISTEMA_CANAIS, SISTEMA_BITS, Aplicacao::Audio),
    ] {
        for p in correr(canais, bits, app, false, 0, 30) {
            Toc::conferir_contra_libopus(&p).expect("os dois leitores de TOC divergiram");
            conferidos += 1;
        }
    }
    assert_eq!(conferidos, 60, "nem todos os pacotes foram conferidos");
}

/// **Um pacote, um quadro.** A regra da §6 do `docs/audio.md`, conferida no bitstream.
///
/// O `AudioRtpPacketizer` da libdatachannel não fragmenta: uma mensagem entra, um pacote RTP sai.
/// Dois quadros de Opus concatenados numa chamada de `enviar_audio` viram um pacote que o outro
/// lado decodifica errado **sem erro nenhum no caminho**. Este teste garante que o nosso encoder
/// nunca produz a entrada desse defeito.
#[test]
fn todo_pacote_nosso_carrega_exatamente_um_quadro() {
    for p in correr(MIC_CANAIS, MIC_BITS, Aplicacao::Voz, true, 10, 30) {
        assert_eq!(
            quadros_no_pacote(&p).expect("contar quadros"),
            1,
            "um pacote com mais de um quadro atravessaria o RTP e seria decodificado errado \
             sem nada acusar"
        );
    }
}

/// Todos os 256 bytes são TOC válido, e a nossa tabela cobre os 32 valores de configuração.
///
/// Não há byte de TOC inválido na RFC 6716 — se houvesse `panic` ou faixa não coberta aqui, um
/// pacote de outro emissor derrubaria o leitor.
#[test]
fn a_tabela_de_toc_cobre_os_256_bytes_sem_buraco() {
    for b in 0u8..=255 {
        let toc = Toc::ler(b);
        assert_eq!(toc.configuracao, b >> 3);
        // Só de chamar já exercita as faixas; o que se prova é que nenhuma entra em `panic`.
        let _ = toc.modo();
        let _ = toc.largura_de_banda();
        let us = toc.duracao_do_quadro_us();
        assert!(
            [2_500, 5_000, 10_000, 20_000, 40_000, 60_000].contains(&us),
            "byte {b:#04x} deu uma duração que o Opus não tem: {us} µs"
        );
    }
}

// -----------------------------------------------------------------------------------------
// O FEC, medido e não prometido
// -----------------------------------------------------------------------------------------

/// Com o FEC ligado **e perda declarada**, o LBRR aparece de verdade no fluxo.
///
/// Este é o teste que transforma `useinbandfec=1` de texto de SDP em fato: `opus_packet_has_lbrr`
/// percorre o cabeçalho SILK com o decodificador de faixa e responde sobre o pacote que saiu.
#[test]
fn com_fec_ligado_e_perda_declarada_o_lbrr_aparece_no_fluxo() {
    let pacotes = correr(MIC_CANAIS, MIC_BITS, Aplicacao::Voz, true, 20, 60);
    let com_lbrr = pacotes
        .iter()
        .skip(5)
        .filter(|p| tem_lbrr(p).unwrap_or(false))
        .count();
    assert!(
        com_lbrr > 0,
        "o FEC estava ligado e 20% de perda declarada, e nenhum dos {} pacotes carregou LBRR — \
         `useinbandfec=1` estaria prometendo o que o encoder não entrega",
        pacotes.len() - 5
    );
}

/// Com o FEC desligado, **nenhum** pacote carrega LBRR.
///
/// O par do teste acima. Sem ele, "achou LBRR" poderia ser o encoder emitindo LBRR sempre, e a
/// medição não estaria medindo o `useinbandfec`.
#[test]
fn com_fec_desligado_nenhum_pacote_carrega_lbrr() {
    let pacotes = correr(MIC_CANAIS, MIC_BITS, Aplicacao::Voz, false, 20, 60);
    for (i, p) in pacotes.iter().enumerate() {
        assert!(
            !tem_lbrr(p).unwrap_or(false),
            "quadro {i} carregou LBRR com o FEC desligado"
        );
    }
}

/// O áudio de sistema, com o preset real, **não** carrega LBRR — como o `fmtp` declara.
#[test]
fn o_audio_do_sistema_nao_carrega_lbrr() {
    for (i, p) in correr(SISTEMA_CANAIS, SISTEMA_BITS, Aplicacao::Audio, false, 0, 40)
        .iter()
        .enumerate()
    {
        assert!(
            !tem_lbrr(p).unwrap_or(false),
            "quadro {i} do áudio de sistema carregou LBRR, e o preset declara `useinbandfec=0`"
        );
    }
}

// -----------------------------------------------------------------------------------------
// Ida e volta
// -----------------------------------------------------------------------------------------

/// Codificar e decodificar devolve o número certo de amostras, e som de verdade.
///
/// **A conferência não é byte a byte, e não pode ser.** O Opus tem perda; o PCM que volta nunca é
/// o que entrou. O que se afirma aqui é o que dá para afirmar: o tamanho bate e a energia do
/// sinal sobreviveu.
#[test]
fn a_ida_e_volta_devolve_o_quadro_inteiro_com_energia() {
    let canais = MIC_CANAIS;
    let pacotes = correr(canais, MIC_BITS, Aplicacao::Voz, false, 0, 30);
    let mut dec = Decodificador::novo(TAXA_HZ, canais).expect("decodificador");

    let mut pcm = vec![0i16; AMOSTRAS_POR_CANAL * usize::from(canais)];
    let mut ultimo_rms = 0.0;
    for p in &pacotes {
        let n = dec.decodificar(p, &mut pcm).expect("decodificar");
        assert_eq!(
            n, AMOSTRAS_POR_CANAL,
            "um quadro de 20 ms a 48 kHz são 960 amostras por canal"
        );
        let soma: f64 = pcm.iter().map(|a| f64::from(*a) * f64::from(*a)).sum();
        ultimo_rms = (soma / pcm.len() as f64).sqrt();
    }
    // O tom entra com amplitude 0,5 do fundo de escala; o RMS de um seno é A/√2 ≈ 11 585.
    assert!(
        ultimo_rms > 5_000.0,
        "o áudio decodificado veio quase mudo (RMS {ultimo_rms:.0}) — o fluxo não é som"
    );
}

/// A ocultação de perda produz um quadro inteiro, sem pacote nenhum.
///
/// É a chamada que o jitter buffer da §4 vai fazer quando um pacote faltar.
#[test]
fn a_ocultacao_de_perda_entrega_um_quadro_inteiro() {
    let pacotes = correr(MIC_CANAIS, MIC_BITS, Aplicacao::Voz, false, 0, 10);
    let mut dec = Decodificador::novo(TAXA_HZ, MIC_CANAIS).expect("decodificador");
    let mut pcm = vec![0i16; AMOSTRAS_POR_CANAL];
    for p in &pacotes {
        dec.decodificar(p, &mut pcm).expect("decodificar");
    }
    let n = dec.ocultar_perda(&mut pcm).expect("ocultar");
    assert_eq!(n, AMOSTRAS_POR_CANAL);
}

/// O estéreo do preset de sistema atravessa como estéreo de verdade, e o TOC diz isso.
///
/// É a regra do M4 aplicada ao áudio: **um emissor tem de declarar no fio o que ele de fato faz.**
#[test]
fn o_preset_de_sistema_declara_estereo_no_proprio_bitstream() {
    for p in correr(SISTEMA_CANAIS, SISTEMA_BITS, Aplicacao::Audio, false, 0, 30)
        .iter()
        .skip(5)
    {
        let toc = Toc::do_pacote(p).expect("TOC");
        assert!(
            toc.estereo,
            "o preset de sistema declara `stereo=1` no SDP e o TOC saiu mono: {toc:?}"
        );
    }
}

/// O atraso algorítmico do encoder, medido em vez de citado.
///
/// O `docs/audio.md` §2 contabiliza 26,5 ms = 20 ms de quadro + 6,5 ms de lookahead. Os 6,5 ms
/// vinham da documentação do codec; aqui é a libopus compilada que responde.
#[test]
fn o_lookahead_do_encoder_bate_com_os_6_5_ms_do_orcamento() {
    let mut enc = Codificador::novo(TAXA_HZ, MIC_CANAIS, Aplicacao::Voz).expect("codificador");
    let amostras = enc.lookahead().expect("lookahead");
    let us = u64::from(amostras) * 1_000_000 / u64::from(TAXA_HZ);
    assert!(
        (6_000..=7_000).contains(&us),
        "o orçamento da §4 conta 6,5 ms de lookahead; a libopus disse {us} µs ({amostras} amostras)"
    );
}

/// **Pacote vazio não pode derrubar o processo.**
///
/// `opus_packet_has_lbrr` do upstream lê `packet[0]` sem olhar o `len` — ver a nota em
/// [`crate::tem_lbrr`]. Sem a guarda, este teste é um SIGSEGV, e não uma falha: foi assim que ele
/// apareceu, em 2026-08-27, ao escrever a porta de recepção de áudio da fronteira C.
///
/// **É alcançável da rede.** O socorro que o jitter buffer oferece é um pacote que chegou do fio,
/// e nada impede um par quebrado de mandar RTP com carga de zero byte.
#[test]
fn pacote_vazio_nao_derruba_o_leitor_de_lbrr() {
    assert!(
        crate::tem_lbrr(&[]).is_err(),
        "pacote vazio tem de virar erro, nunca uma leitura fora dos limites"
    );
}

/// E o vizinho continua respondendo o que deve: um pacote **legível** que simplesmente não tem
/// LBRR responde `false`, e não erro. Sem isto, a guarda acima poderia ter sido escrita larga
/// demais e apagado a diferença entre "não tem" e "não sei".
#[test]
fn pacote_de_celt_responde_que_nao_tem_lbrr_em_vez_de_erro() {
    // TOC 0xFF: config 31 (CELT, banda cheia), estéreo, code 3. Válido e sem LBRR possível.
    assert_eq!(crate::tem_lbrr(&[0xFF, 0xFF, 0xFF]), Ok(false));
}

/// Onde o conteúdo sai do decodificador: um estouro de 10 ms a 3 150 Hz (o da claquete) entra na
/// amostra `inicio` do codificador, e a correlação cruzada acha quantas amostras depois ele sai do
/// decodificador. Devolve `(medido, lookahead declarado)`.
fn atraso_medido_em_amostras(aplicacao: Aplicacao, canais: u8, bits: u32) -> (usize, u32) {
    let mut enc = Codificador::novo(TAXA_HZ, canais, aplicacao).expect("codificador");
    enc.definir_taxa_de_bits(bits).expect("taxa");
    let lookahead = enc.lookahead().expect("lookahead");
    let mut dec = Decodificador::novo(TAXA_HZ, canais).expect("decodificador");
    let c = usize::from(canais);
    let quadros = 50usize;
    // No meio de um quadro, de propósito: o atraso não pode ser a fronteira do quadro.
    let inicio = 20 * AMOSTRAS_POR_CANAL + 137;
    let dur = 480usize;
    let estouro = |n: usize| -> f64 {
        if n < inicio || n >= inicio + dur {
            return 0.0;
        }
        let k = (n - inicio) as f64;
        let janela = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k / dur as f64).cos();
        (2.0 * std::f64::consts::PI * 3_150.0 * n as f64 / f64::from(TAXA_HZ)).sin() * 0.5 * janela
    };
    let mut saida = Vec::new();
    let mut pacote = vec![0u8; 4_000];
    let mut pcm = vec![0i16; AMOSTRAS_POR_CANAL * c];
    for q in 0..quadros {
        let quadro: Vec<i16> = (0..AMOSTRAS_POR_CANAL)
            .flat_map(|i| {
                std::iter::repeat((estouro(q * AMOSTRAS_POR_CANAL + i) * 32_767.0) as i16).take(c)
            })
            .collect();
        let n = enc.codificar(&quadro, &mut pacote).expect("codificar");
        let m = dec.decodificar(&pacote[..n], &mut pcm).expect("decodificar");
        saida.extend(pcm[..m * c].iter().step_by(c).map(|v| f64::from(*v) / 32_767.0));
    }
    let mut melhor = (0usize, f64::MIN);
    for atraso in 0..2_000usize {
        let s: f64 = (inicio..inicio + dur)
            .filter(|n| n + atraso < saida.len())
            .map(|n| estouro(n) * saida[n + atraso])
            .sum();
        if s > melhor.1 {
            melhor = (atraso, s);
        }
    }
    (melhor.0, lookahead)
}

/// **O conteúdo decodificado sai atrasado exatamente o lookahead do codificador**, e não o quadro
/// nem outra coisa: é o que o T0 do Mac mediu no Opus (+6,6 ms, e ~0 no PCMU;
/// `docs/som-no-receptor.md` §20.7). **O controle** é o `RESTRICTED_LOWDELAY`, de 2,5 ms: se o
/// atraso medido é do codificador, ele anda junto com o lookahead declarado — a previsão do §17.4.
#[test]
fn o_conteudo_decodificado_sai_atrasado_o_lookahead_do_codificador() {
    for (nome, app, canais, bits) in [
        ("voz", Aplicacao::Voz, MIC_CANAIS, MIC_BITS),
        ("audio", Aplicacao::Audio, SISTEMA_CANAIS, SISTEMA_BITS),
        ("atraso baixo", Aplicacao::AtrasoBaixo, SISTEMA_CANAIS, SISTEMA_BITS),
    ] {
        let (medido, lookahead) = atraso_medido_em_amostras(app, canais, bits);
        eprintln!("{nome}: medido {medido} amostras, lookahead declarado {lookahead}");
        assert!(
            (i64::try_from(medido).unwrap() - i64::from(lookahead)).abs() <= 2,
            "{nome}: o estouro saiu {medido} amostras depois, e o lookahead é {lookahead}"
        );
    }
}

// -----------------------------------------------------------------------------------------
// A complexidade (27/09, `docs/teleprompter-com-camera.md` §8.12.17)
// -----------------------------------------------------------------------------------------

#[test]
#[ignore]
fn varredura_lbrr_por_complexidade() {
    for complexidade in (0u8..=10).rev() {
        let mut enc = Codificador::novo(TAXA_HZ, MIC_CANAIS, Aplicacao::Voz).expect("criar");
        enc.definir_taxa_de_bits(MIC_BITS).expect("taxa");
        enc.definir_fec_embutido(true).expect("fec");
        enc.definir_perda_esperada(5).expect("perda");
        enc.definir_dtx(false).expect("dtx");
        enc.definir_sinal(Sinal::Voz).expect("sinal");
        enc.definir_complexidade(complexidade).expect("complexidade");
        let mut buf = vec![0u8; 4000];
        let mut com_lbrr = 0;
        for i in 0..60u64 {
            let n = enc.codificar(&tom(i, MIC_CANAIS, 220.0), &mut buf).expect("codificar");
            if i >= 5 && tem_lbrr(&buf[..n]).unwrap_or(false) { com_lbrr += 1; }
        }
        println!("complexidade {complexidade:2}: {com_lbrr} de 55 com LBRR");
    }
}

/// **O LBRR e a complexidade** (27/09, §8.12.17): com o preset do microfone (voz, sinal em voz, FEC ligado,
/// 5 % de perda declarada), um encoder que **nasce** em 10 emite LBRR, e um que nasce em 6 não emite — é o
/// que proíbe baixar o padrão (`PresetDeAudio::complexidade_do_encoder`). Se a libopus mudar isso, este
/// teste avisa, e a nota de lá fica velha.
#[test]
fn o_lbrr_depende_da_complexidade_com_que_o_encoder_nasce() {
    let contar = |complexidade: u8| {
        let mut enc = Codificador::novo(TAXA_HZ, MIC_CANAIS, Aplicacao::Voz).expect("criar");
        enc.definir_taxa_de_bits(MIC_BITS).expect("taxa");
        enc.definir_fec_embutido(true).expect("fec");
        enc.definir_perda_esperada(5).expect("perda");
        enc.definir_dtx(false).expect("dtx");
        enc.definir_sinal(Sinal::Voz).expect("sinal");
        enc.definir_complexidade(complexidade).expect("complexidade");
        let mut buf = vec![0u8; 4000];
        (0..60u64)
            .filter(|&i| {
                let n = enc.codificar(&tom(i, MIC_CANAIS, 220.0), &mut buf).expect("codificar");
                i >= 5 && tem_lbrr(&buf[..n]).unwrap_or(false)
            })
            .count()
    };
    let (dez, seis) = (contar(10), contar(6));
    assert!(dez >= 15, "complexidade 10: o LBRR sumiu ({dez} de 55)");
    assert_eq!(seis, 0, "complexidade 6: o LBRR passou a sair — a nota de `complexidade_do_encoder` está velha");
}

/// **A medida do custo** (não roda no portão): `cargo test -p quall-opus --release -- --ignored
/// --nocapture custo_por_complexidade`. Codifica 30 s de voz sintética mono a 32 kbit/s em cada
/// complexidade e imprime o tempo médio por quadro de 20 ms.
#[test]
#[ignore]
fn custo_por_complexidade() {
    for complexidade in [10u8, 9, 8, 7, 6, 5, 3, 2, 0] {
        let mut enc = Codificador::novo(TAXA_HZ, MIC_CANAIS, Aplicacao::Voz).expect("criar");
        enc.definir_taxa_de_bits(MIC_BITS).expect("taxa");
        enc.definir_fec_embutido(true).expect("fec");
        enc.definir_perda_esperada(5).expect("perda");
        enc.definir_dtx(false).expect("dtx");
        enc.definir_sinal(Sinal::Voz).expect("sinal");
        enc.definir_complexidade(complexidade).expect("complexidade");
        let quadros: Vec<Vec<i16>> = (0..1500u64)
            .map(|i| {
                // Uma "voz" grosseira: uma fundamental que varia, com um harmônico e ruído.
                let hz = 120.0 + 40.0 * ((i as f64) / 25.0).sin();
                let mut q = tom(i, MIC_CANAIS, hz);
                for (k, a) in q.iter_mut().enumerate() {
                    let t = (i as usize * 960 + k) as f64;
                    let h = (t * 2.0 * std::f64::consts::PI * hz * 3.0 / 48_000.0).sin();
                    let ruido = ((k * 7919 + i as usize * 104_729) % 601) as i16 - 300;
                    *a = a.saturating_add((h * 3000.0) as i16).saturating_add(ruido);
                }
                q
            })
            .collect();
        let mut buf = vec![0u8; 4000];
        let inicio = std::time::Instant::now();
        let mut bytes = 0usize;
        for q in &quadros {
            bytes += enc.codificar(q, &mut buf).expect("codificar");
        }
        let por_quadro = inicio.elapsed().as_secs_f64() * 1000.0 / quadros.len() as f64;
        println!(
            "complexidade {complexidade:2}: {por_quadro:.3} ms por quadro de 20 ms, {:.1} kbit/s",
            bytes as f64 * 8.0 / 30.0 / 1000.0
        );
    }
}
