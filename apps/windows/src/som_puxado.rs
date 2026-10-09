//! **O som do receptor do Windows, sem o Windows** (S6 do `docs/som-no-receptor.md`).
//!
//! A metade do tocador que não chama WASAPI, e por isso roda nos testes de qualquer máquina,
//! inclusive na Sessão 0 do Dell, que não tem saída de som. A outra metade, a que abre o dispositivo
//! e espera o evento dele, é `tocador.rs`.
//!
//! O que mora aqui, e as peças do Mac que cada uma espelha (`SomPuxado.swift`):
//!
//! - [`Montador`] (`MontadorDeSaida`): o WASAPI pede quantos quadros o buffer dele tem livres — 441,
//!   480, 1 056, o que o período do dispositivo der —, e a porta do núcleo entrega slots de 20 ms.
//!   O montador casa os dois, com a sobra de um slot guardada para o pedido seguinte, e calcula o
//!   atraso até o DAC de **cada** puxada: o que o dispositivo já tem na fila, mais o que este pedido
//!   já escreveu antes do slot começar.
//! - [`InterpoladorPor6`] (`InterpoladorPor6`): o PCMU de 8 kHz levado a 48 kHz antes do motor, pelo
//!   mesmo FIR de Kaiser (144 coeficientes, β = 7, corte em 4 kHz). O motor do Windows toca sempre
//!   48 kHz em estéreo; a conversão para o formato do mixador, se ele for outro, é do WASAPI
//!   (`AUTOCONVERTPCM`), e a razão do núcleo vai por `RATEADJUST`.
//! - [`mulaw_para_f32`] (`MuLaw`).
//! - [`aplicar_ganho`]: o volume em rampa, dentro de um pedido.
//! - [`VontadeDoSom`] (D1 e D3 do §12.1): mudo, volume, e a câmera do Quall em uso.
//! - [`DonoDoSom`] e [`toca_o_som_do_windows`] (D2): no emissor com várias sessões, quem manda o
//!   som.

/// A taxa do motor: o que o tocador entrega ao WASAPI, sempre.
pub const TAXA: u32 = 48_000;
/// Os canais do motor: estéreo, sempre. O PCMU mono é duplicado.
pub const CANAIS: usize = 2;
/// Um slot de 20 ms a 48 kHz.
pub const QUADROS_POR_SLOT: usize = 960;

// -------------------------------------------------------------------------------------------------
// O slot, e o montador
// -------------------------------------------------------------------------------------------------

/// A ordem do slot, como a casca a vê depois de decodificar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ordem {
    Quadro,
    Cura,
    Silencio,
    Ocioso,
}

/// O que uma puxada escreveu: a ordem, quantos quadros (por canal, a 48 kHz) e o carimbo do slot
/// na base da track (zero no ocioso).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotPuxado {
    pub ordem: Ordem,
    pub quadros: usize,
    pub carimbo_us: u64,
}

impl SlotPuxado {
    pub fn ocioso() -> Self {
        SlotPuxado { ordem: Ordem::Ocioso, quadros: 0, carimbo_us: 0 }
    }
}

/// O que o render viu, para quem relata.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Retrato {
    pub renders: u64,
    pub quadros_pedidos: u64,
    pub puxadas: u64,
    pub ociosas: u64,
    pub silencios: u64,
    pub curas: u64,
    pub maior_render: usize,
    pub menor_render: usize,
    /// O último slot de quadro tocado: o carimbo dele e a hora (µs, no relógio de quem chama o
    /// [`Montador::render`]) em que a primeira amostra dele sai no DAC.
    pub ultimo_carimbo_us: u64,
    pub ultimo_no_dac_us: u64,
    pub tem_ultimo: bool,
}

/// A fonte de slots: `(atraso até o DAC em µs, hora do DAC em µs, razão aplicada, destino)` → o
/// slot escrito em `destino` (estéreo intercalado, 48 kHz, até [`QUADROS_POR_SLOT`] quadros).
pub trait FonteDeSlots {
    fn puxar(&mut self, atraso_us: u32, no_dac_us: u64, razao: f64, destino: &mut [f32]) -> SlotPuxado;
}

impl<F> FonteDeSlots for F
where
    F: FnMut(u32, u64, f64, &mut [f32]) -> SlotPuxado,
{
    fn puxar(&mut self, atraso_us: u32, no_dac_us: u64, razao: f64, destino: &mut [f32]) -> SlotPuxado {
        self(atraso_us, no_dac_us, razao, destino)
    }
}

/// Casa os pedidos do dispositivo, de qualquer tamanho, com os slots de 20 ms da porta. Não aloca
/// depois de criado.
pub struct Montador<F: FonteDeSlots> {
    fonte: F,
    sobra: Vec<f32>,
    inicio: usize,
    fim: usize,
    pub retrato: Retrato,
}

impl<F: FonteDeSlots> Montador<F> {
    pub fn novo(fonte: F) -> Self {
        Montador {
            fonte,
            sobra: vec![0.0; QUADROS_POR_SLOT * CANAIS],
            inicio: 0,
            fim: 0,
            retrato: Retrato { menor_render: usize::MAX, ..Retrato::default() },
        }
    }

    /// Quadros ainda guardados, que tocam antes da próxima puxada.
    pub fn quadros_na_sobra(&self) -> usize {
        self.fim - self.inicio
    }

    /// Escreve `saida.len() / CANAIS` quadros. `atraso_da_saida_us` é daqui a quanto a primeira
    /// amostra deste pedido sai no DAC (a fila do dispositivo e a latência do fluxo); `agora_us`,
    /// a hora do núcleo agora.
    pub fn render(&mut self, saida: &mut [f32], atraso_da_saida_us: f64, razao: f64, agora_us: u64) {
        let razao = if razao.is_finite() && razao > 0.0 { razao } else { 1.0 };
        let quadros = saida.len() / CANAIS;
        self.retrato.renders += 1;
        self.retrato.quadros_pedidos += quadros as u64;
        self.retrato.maior_render = self.retrato.maior_render.max(quadros);
        self.retrato.menor_render = self.retrato.menor_render.min(quadros);
        let mut escritos = 0usize;
        while escritos < quadros {
            if self.inicio == self.fim {
                // O slot novo começa depois de tudo o que este pedido já escreveu, lido à razão.
                let atraso =
                    (atraso_da_saida_us + escritos as f64 / (f64::from(TAXA) * razao) * 1e6).max(0.0);
                let no_dac = agora_us + atraso as u64;
                let s = self.fonte.puxar(atraso as u32, no_dac, razao, &mut self.sobra);
                self.retrato.puxadas += 1;
                let n = match s.ordem {
                    Ordem::Ocioso => {
                        self.retrato.ociosas += 1;
                        self.sobra.iter_mut().for_each(|v| *v = 0.0);
                        QUADROS_POR_SLOT
                    }
                    ordem => {
                        match ordem {
                            Ordem::Silencio => self.retrato.silencios += 1,
                            Ordem::Cura => self.retrato.curas += 1,
                            _ => {
                                self.retrato.ultimo_carimbo_us = s.carimbo_us;
                                self.retrato.ultimo_no_dac_us = no_dac;
                                self.retrato.tem_ultimo = true;
                            }
                        }
                        s.quadros.clamp(1, QUADROS_POR_SLOT)
                    }
                };
                self.inicio = 0;
                self.fim = n;
            }
            let k = (self.fim - self.inicio).min(quadros - escritos);
            saida[escritos * CANAIS..(escritos + k) * CANAIS]
                .copy_from_slice(&self.sobra[self.inicio * CANAIS..(self.inicio + k) * CANAIS]);
            self.inicio += k;
            escritos += k;
        }
    }
}

// -------------------------------------------------------------------------------------------------
// O ganho, em rampa
// -------------------------------------------------------------------------------------------------

/// Leva `saida` (estéreo intercalado) de `*atual` até `alvo` em rampa linear dentro do pedido, e
/// guarda o alvo em `*atual`. Com os dois iguais, só multiplica (e com 1, nem isso).
pub fn aplicar_ganho(saida: &mut [f32], atual: &mut f32, alvo: f32) {
    let alvo = alvo.clamp(0.0, 1.0);
    let quadros = saida.len() / CANAIS;
    if *atual == alvo {
        if alvo != 1.0 {
            saida.iter_mut().for_each(|v| *v *= alvo);
        }
        return;
    }
    // Multiplicado, e não somado quadro a quadro: a soma de 480 passos em `f32` erra o fim da
    // rampa em ~1e-5, e o mudo não chegaria a zero exato.
    let (de, n) = (*atual, quadros.max(1) as f32);
    for q in 0..quadros {
        let g = de + (alvo - de) * (q + 1) as f32 / n;
        for c in 0..CANAIS {
            saida[q * CANAIS + c] *= g;
        }
    }
    *atual = alvo;
}

/// O pico absoluto de um pedido, depois do ganho.
pub fn pico(saida: &[f32]) -> f32 {
    saida.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

// -------------------------------------------------------------------------------------------------
// PCMU
// -------------------------------------------------------------------------------------------------

/// G.711 µ-law para `f32` em [-1, 1). Os mesmos valores do Mac e do plugin do OBS.
pub fn mulaw_para_f32(u: u8) -> f32 {
    let u = !u;
    let sinal = u & 0x80;
    let expoente = (u >> 4) & 0x07;
    let mantissa = i32::from(u & 0x0F);
    let amostra = (((mantissa << 3) + 0x84) << expoente) - 0x84;
    (if sinal != 0 { -amostra } else { amostra }) as f32 / 32768.0
}

/// PCM de 16 bits para G.711 µ-law — o codificador da G.711, com o viés de 0x84 e o teto de
/// 32 635. Só a sonda sem rede (`quall-som-local`) e os testes codificam: o receptor só decodifica.
pub fn mulaw_de_i16(pcm: i16) -> u8 {
    const VIES: i32 = 0x84;
    const TETO: i32 = 32_635;
    let mut x = i32::from(pcm);
    let sinal = if x < 0 {
        x = -x;
        0x80
    } else {
        0
    };
    x = x.min(TETO) + VIES;
    let mut expoente = 7;
    let mut mascara = 0x4000;
    while expoente > 0 && x & mascara == 0 {
        expoente -= 1;
        mascara >>= 1;
    }
    let mantissa = (x >> (expoente + 3)) & 0x0F;
    !((sinal | (expoente << 4) | mantissa) as u8)
}

// -------------------------------------------------------------------------------------------------
// O tom de prova, e a medida dele
// -------------------------------------------------------------------------------------------------

/// As quatro notas do tom da sonda (`crates/quall-probe/src/audio.rs`), meio segundo cada: 400,
/// 500, 800 e 1 000 Hz. Cada nota fecha um número inteiro de ciclos em meio segundo, então a troca
/// de nota cai num zero do seno, sem estalo.
pub const NOTAS_DE_PROVA_HZ: [u32; 4] = [400, 500, 800, 1_000];

/// A amostra `n` do tom de prova a `taxa` Hz, com amplitude 0,5 (a da sonda).
pub fn tom_de_prova(n: u64, taxa: u32) -> f32 {
    let meia = u64::from(taxa / 2).max(1);
    let nota = NOTAS_DE_PROVA_HZ[((n / meia) % NOTAS_DE_PROVA_HZ.len() as u64) as usize];
    let fase = 2.0 * std::f64::consts::PI * f64::from(nota) * (n % u64::from(taxa)) as f64 / f64::from(taxa);
    (0.5 * fase.sin()) as f32
}

/// A amplitude de `freq_hz` em `amostras` (lidas de `passo` em `passo`, para pegar um canal de um
/// intercalado), por Goertzel. Um seno de amplitude A dá ≈ A com um número inteiro de ciclos.
pub fn amplitude_em(freq_hz: f64, amostras: &[f32], passo: usize, taxa: u32) -> f32 {
    let passo = passo.max(1);
    let w = 2.0 * std::f64::consts::PI * freq_hz / f64::from(taxa);
    let coef = 2.0 * w.cos();
    let (mut s1, mut s2, mut n) = (0.0f64, 0.0f64, 0usize);
    for v in amostras.iter().step_by(passo) {
        let s0 = f64::from(*v) + coef * s1 - s2;
        s2 = s1;
        s1 = s0;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    let potencia = s1 * s1 + s2 * s2 - coef * s1 * s2;
    (2.0 * potencia.max(0.0).sqrt() / n as f64) as f32
}

/// Um estouro da claquete achado no som que sai (a S7, T2 do `docs/som-no-receptor.md` §9.4): a hora
/// em que ele sai no DAC, no QPC (µs), e o carimbo dele na base da track (µs, com o atraso interno
/// da decodificação descontado).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estouro {
    pub no_dac_qpc_us: u64,
    pub carimbo_us: i64,
}

/// O detector do estouro da claquete (`crates/quall-probe/src/claquete.rs`: 10 ms a 3 150 Hz, o tom
/// calado ±40 ms em volta). **Porte literal** do `DetectorDeEstouro` do Mac
/// (`apps/macos/Sources/QuallReceptorKit/SomPuxado.swift`), com os mesmos números e os mesmos
/// testes: Goertzel numa janela de Hann de 2 ms, passo de 1 ms, liga acima de 0,12 e só volta a
/// armar depois de 20 ms abaixo de 0,05.
///
/// Aloca só na criação: roda na thread do render.
pub struct DetectorDeEstouro {
    janela: usize,
    passo: usize,
    limiar_alto: f32,
    limiar_baixo: f32,
    acumulado: Vec<f32>,
    pesos: Vec<f32>,
    guardadas: usize,
    abaixo_ha: u32,
    ativo: bool,
    coef: f32,
    soma_dos_pesos: f32,
}

impl DetectorDeEstouro {
    pub const FREQUENCIA_HZ: f64 = 3150.0;
    /// Quantos passos depois do começo da janela o estouro costuma começar, quando a janela é a
    /// primeira acima do limiar (o número do Mac, medido lá em todas as fases do passo).
    pub const VIES_EM_PASSOS: f64 = 0.81;

    pub fn novo(taxa_hz: f64, maior_slot: usize) -> Self {
        let janela = (taxa_hz * 0.002) as usize;
        let passo = (taxa_hz * 0.001) as usize;
        let pesos: Vec<f32> = (0..janela)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (janela - 1) as f64).cos()) as f32)
            .collect();
        let soma_dos_pesos = pesos.iter().sum();
        DetectorDeEstouro {
            janela,
            passo,
            limiar_alto: 0.12,
            limiar_baixo: 0.05,
            acumulado: vec![0.0; janela + maior_slot],
            pesos,
            guardadas: 0,
            abaixo_ha: 1_000,
            ativo: false,
            coef: (2.0 * (2.0 * std::f64::consts::PI * Self::FREQUENCIA_HZ / taxa_hz).cos()) as f32,
            soma_dos_pesos,
        }
    }

    /// A amplitude de 3 150 Hz na janela que começa em `p`.
    fn amplitude(&self, p: usize) -> f32 {
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for i in 0..self.janela {
            let s0 = self.acumulado[p + i] * self.pesos[i] + self.coef * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let potencia = s1 * s1 + s2 * s2 - self.coef * s1 * s2;
        2.0 * potencia.max(0.0).sqrt() / self.soma_dos_pesos
    }

    /// Processa um slot: `x` intercalado com `salto` canais (lê o primeiro). Devolve onde o
    /// estouro começou, em amostras **a partir do começo deste slot** (negativo se começou na
    /// cauda do anterior), ou `None`. Não aloca.
    pub fn processar(&mut self, x: &[f32], salto: usize) -> Option<f64> {
        let salto = salto.max(1);
        let n = x.len() / salto;
        let capacidade = self.acumulado.len();
        let m = n.min(capacidade - self.guardadas);
        for i in 0..m {
            self.acumulado[self.guardadas + i] = x[i * salto];
        }
        let total = self.guardadas + m;
        let mut achado = None;
        let mut p = 0usize;
        while p + self.janela <= total {
            let a = self.amplitude(p);
            if self.ativo {
                if a < self.limiar_baixo {
                    self.abaixo_ha += 1;
                } else {
                    self.abaixo_ha = 0;
                }
                if self.abaixo_ha >= 20 {
                    self.ativo = false;
                }
            } else if a > self.limiar_alto {
                // Só arma depois de 20 ms abaixo do limiar baixo (o mesmo cuidado do Mac: as
                // janelas da borda de subida, entre os dois limiares, não desarmam).
                if self.abaixo_ha >= 20 && achado.is_none() {
                    achado = Some(p as f64 - self.guardadas as f64 + Self::VIES_EM_PASSOS * self.passo as f64);
                }
                self.ativo = true;
                self.abaixo_ha = 0;
            } else if a < self.limiar_baixo {
                self.abaixo_ha += 1;
            }
            p += self.passo;
        }
        // O que sobrou depois da última janela vai para o começo, para a próxima.
        let resto = total - p;
        self.acumulado.copy_within(p..total, 0);
        self.guardadas = resto;
        achado
    }
}

pub const INTERPOLADOR_FATOR: usize = 6;
pub const INTERPOLADOR_POR_FASE: usize = 24;
/// O atraso de grupo do filtro, em µs: (144 − 1) / 2 amostras a 48 kHz = 1,49 ms.
pub const INTERPOLADOR_ATRASO_US: f64 =
    (INTERPOLADOR_FATOR * INTERPOLADOR_POR_FASE - 1) as f64 / 2.0 / 48_000.0 * 1e6;

/// 8 → 48 kHz, mono. O mesmo filtro do Mac e do OBS.
pub struct InterpoladorPor6 {
    coeficientes: [f32; INTERPOLADOR_FATOR * INTERPOLADOR_POR_FASE],
    historia: [f32; INTERPOLADOR_POR_FASE],
    cabeca: usize,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut soma, mut termo, mut k) = (1.0f64, 1.0f64, 1.0f64);
    while termo > 1e-12 * soma {
        termo *= (x / (2.0 * k)) * (x / (2.0 * k));
        soma += termo;
        k += 1.0;
    }
    soma
}

impl Default for InterpoladorPor6 {
    fn default() -> Self {
        Self::novo()
    }
}

impl InterpoladorPor6 {
    pub fn novo() -> Self {
        let n = INTERPOLADOR_FATOR * INTERPOLADOR_POR_FASE;
        let corte = 4_000.0 / 48_000.0;
        let beta = 7.0;
        let meio = (n - 1) as f64 / 2.0;
        let mut h = [0.0f64; INTERPOLADOR_FATOR * INTERPOLADOR_POR_FASE];
        let mut soma = 0.0;
        for (m, v) in h.iter_mut().enumerate() {
            let x = m as f64 - meio;
            let sinc = if x == 0.0 {
                2.0 * corte
            } else {
                (2.0 * std::f64::consts::PI * corte * x).sin() / (std::f64::consts::PI * x)
            };
            let r = x / meio;
            let janela = bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta);
            *v = sinc * janela;
            soma += *v;
        }
        let mut coeficientes = [0.0f32; INTERPOLADOR_FATOR * INTERPOLADOR_POR_FASE];
        for (c, v) in coeficientes.iter_mut().zip(h.iter()) {
            *c = (v / soma * INTERPOLADOR_FATOR as f64) as f32;
        }
        InterpoladorPor6 { coeficientes, historia: [0.0; INTERPOLADOR_POR_FASE], cabeca: 0 }
    }

    /// `entrada` (8 kHz) → `saida` (48 kHz, `6 × entrada.len()` amostras).
    pub fn processar(&mut self, entrada: &[f32], saida: &mut [f32]) {
        let (f, k) = (INTERPOLADOR_FATOR, INTERPOLADOR_POR_FASE);
        for (i, &x) in entrada.iter().enumerate() {
            self.cabeca = (self.cabeca + 1) % k;
            self.historia[self.cabeca] = x;
            for p in 0..f {
                let mut y = 0.0f32;
                let mut h = self.cabeca;
                for j in 0..k {
                    y += self.coeficientes[p + f * j] * self.historia[h];
                    h = if h == 0 { k - 1 } else { h - 1 };
                }
                saida[i * f + p] = y;
            }
        }
    }

    /// A última amostra de 8 kHz que entrou.
    pub fn ultima(&self) -> f32 {
        self.historia[self.cabeca]
    }
}

// -------------------------------------------------------------------------------------------------
// D1 e D3: o volume que sai
// -------------------------------------------------------------------------------------------------

/// **As vontades do volume** (D1 e D3 do `docs/som-no-receptor.md` §12.1), as mesmas do Mac.
///
/// - D1: som ligado por padrão; mudo e volume na janela.
/// - D3: mudo enquanto a câmera do Quall estiver em uso. No Windows a câmera do Quall é a baia de
///   cada aparelho (`baia.rs`), e "em uso" é **alguém lendo o cano dela** (`Baia::alguem_lendo`):
///   o servidor de quadros só abre o cano quando um app abre a câmera. A leitura do coordenador
///   (18/09) vale igual: em uso cala, e a chave da janela desfaz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VontadeDoSom {
    pub mudo: bool,
    /// 0 a 1.
    pub volume: f32,
    pub camera_em_uso: bool,
    pub tocar_com_a_camera: bool,
}

impl Default for VontadeDoSom {
    fn default() -> Self {
        VontadeDoSom { mudo: false, volume: 1.0, camera_em_uso: false, tocar_com_a_camera: false }
    }
}

impl VontadeDoSom {
    pub fn calado_pela_camera(&self) -> bool {
        self.camera_em_uso && !self.tocar_com_a_camera
    }

    pub fn ganho(&self) -> f32 {
        if self.mudo || self.calado_pela_camera() {
            0.0
        } else {
            self.volume.clamp(0.0, 1.0)
        }
    }

    /// Por que está calado, para a janela e o diário; `None` quando toca.
    pub fn por_que_calado(&self) -> Option<&'static str> {
        if self.mudo {
            Some("mudo")
        } else if self.calado_pela_camera() {
            Some("mudo: a câmera do Quall está em uso")
        } else if self.volume <= 0.0 {
            Some("volume zero")
        } else {
            None
        }
    }
}

// -------------------------------------------------------------------------------------------------
// D2: quem manda o som, no emissor com várias sessões
// -------------------------------------------------------------------------------------------------

/// **Quem toca o som que o Windows manda** (Opus, `PRESET_AUDIO_DO_SISTEMA`), pelo `device_id` do
/// receptor. A tabela do Mac (`QuemTocaOSomDoMac`) é do PCMU que o Mac manda, e nela o iOS fica de
/// fora (só toca Opus, risco R6); o Windows manda **Opus**, e o iOS toca (`SaidaDeAudio.swift`):
///
/// | prefixo | toca o Opus do Windows? |
/// |---|---|
/// | `mac-` | sim, desde a S4 |
/// | `android-` | sim |
/// | `ios-` | sim (`apps/ios/Quall/Receber/SaidaDeAudio.swift`) |
/// | `win-` | sim, desde a S6 |
/// | outro (OBS, o app da câmera, a sonda) | não |
///
/// **É uma tabela que envelhece**: o jeito certo é o receptor declarar no aperto de mão se toca e
/// quais codecs (registrado no §17.7). O OBS publica o som desde a S5, mas o `device_id` dele não
/// tem prefixo; continua fora da tabela, e o som vai para quem toca no cômodo.
pub fn toca_o_som_do_windows(device_id: &str) -> bool {
    let id = device_id.to_ascii_lowercase();
    id.starts_with("mac-") || id.starts_with("android-") || id.starts_with("ios-") || id.starts_with("win-")
}

/// **A carência do dono que cai** (decisão do Bruno, 18/09/2026, ~22h30): se o dono do som cai e
/// volta em até 10 s, o som continua nele; depois disso, passa ao próximo. Nos 10 s ninguém toca.
pub const CARENCIA_DO_DONO_MS: u64 = 10_000;

/// **D2**: só o primeiro receptor **que toca** manda o som; quando ele sai, o som passa ao próximo
/// que toca, na ordem de conexão (se a passagem estiver ligada). O mesmo desenho do Mac
/// (`DonoDoSom.swift`), com o conserto do M2 (a sessão que reconecta mantém o som) e do M3 (quem não
/// toca não é dono). Os números são os `Id` da tabela de sessões (`sessoes.rs`).
///
/// - **Quem sai** (o receptor fechou, a pessoa desconectou, a sessão perdeu a fonte) passa o som
///   **na hora**.
/// - **Quem cai** (a conexão caiu) abre a [`CARENCIA_DO_DONO_MS`]: o som fica com o **aparelho**
///   (o `device_id`), ninguém toca, e se ele voltar por uma sessão nova dentro do prazo, o som é
///   dela. Vencido o prazo, passa ao próximo. É a resposta ao M2 da crítica 13: o dono cuja rede
///   pisca, e cuja queda o emissor vê **antes** da volta, não perde o som para o segundo receptor.
/// - **A sessão que perde o som** (a captura não subiu) deixa de tocar e não segura o som (crítica
///   13, M3).
///
/// Com a passagem desligada, quem **chega** sem dono no ar ainda vira dono (como no Mac): a chave
/// só impede o som de ir para quem já estava conectado e calado.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DonoDoSom {
    /// As sessões conectadas, na ordem de conexão: o id, se o receptor toca, e o aparelho.
    conectadas: Vec<(u64, bool, String)>,
    dono: Option<u64>,
    /// O aparelho do dono que caiu, e até quando (ms) o som espera por ele.
    carencia: Option<(String, u64)>,
    pub passar: bool,
}

impl DonoDoSom {
    pub fn novo(passar: bool) -> Self {
        DonoDoSom { conectadas: Vec::new(), dono: None, carencia: None, passar }
    }

    pub fn dono(&self) -> Option<u64> {
        self.dono
    }

    /// O aparelho que o som espera, e até quando, se há carência.
    pub fn carencia(&self) -> Option<(&str, u64)> {
        self.carencia.as_ref().map(|(a, ate)| (a.as_str(), *ate))
    }

    /// O primeiro conectado que toca, se a passagem estiver ligada.
    fn proximo(&self) -> Option<u64> {
        if !self.passar {
            return None;
        }
        self.conectadas.iter().find(|(_, toca, _)| *toca).map(|(i, _, _)| *i)
    }

    /// Vence a carência, se passou do prazo: o som vai ao próximo que toca.
    fn vencer(&mut self, agora_ms: u64) {
        if let Some((_, ate)) = &self.carencia {
            if agora_ms >= *ate {
                self.carencia = None;
                if self.dono.is_none() {
                    self.dono = self.proximo();
                }
            }
        }
    }

    /// A sessão `id`, do `aparelho`, entrou na fila: é dona se o som espera por este aparelho, ou
    /// se ninguém é dono e não há carência.
    fn entrou(&mut self, id: u64, toca: bool, aparelho: &str) {
        let e_o_esperado = self.carencia.as_ref().is_some_and(|(a, _)| !aparelho.is_empty() && a == aparelho);
        if e_o_esperado {
            self.carencia = None;
            // Voltou por um app que não toca: é como se tivesse saído, e o som passa já.
            self.dono = if toca { Some(id) } else { self.proximo() };
            return;
        }
        if self.dono.is_none() && self.carencia.is_none() && toca {
            self.dono = Some(id);
        }
    }

    /// Uma sessão conectou. Devolve o dono novo, se mudou. Quem já está na fila (a sessão que
    /// substituiu a velha do mesmo aparelho) não muda nada.
    pub fn conectou(&mut self, id: u64, toca: bool, aparelho: &str, agora_ms: u64) -> Option<u64> {
        let antes = self.dono;
        self.vencer(agora_ms);
        if !self.conectadas.iter().any(|(i, _, _)| *i == id) {
            self.conectadas.push((id, toca, aparelho.to_string()));
            self.entrou(id, toca, aparelho);
        }
        (self.dono != antes).then_some(self.dono).flatten()
    }

    /// Uma sessão saiu por gesto ou por falha que não é queda: o som passa **na hora**. Devolve o
    /// dono novo, se mudou.
    pub fn saiu(&mut self, id: u64) -> Option<u64> {
        self.conectadas.retain(|(i, _, _)| *i != id);
        if self.dono != Some(id) {
            return None;
        }
        self.dono = if self.carencia.is_none() { self.proximo() } else { None };
        self.dono
    }

    /// A conexão da sessão `id` caiu. Se ela era a dona, o som espera o aparelho dela por
    /// [`CARENCIA_DO_DONO_MS`], e ninguém toca nesse tempo. Senão, é uma saída qualquer.
    pub fn caiu(&mut self, id: u64, agora_ms: u64) -> Option<u64> {
        if self.dono != Some(id) {
            return self.saiu(id);
        }
        let aparelho = self
            .conectadas
            .iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, _, a)| a.clone())
            .unwrap_or_default();
        self.conectadas.retain(|(i, _, _)| *i != id);
        self.dono = None;
        if aparelho.is_empty() {
            // Sem identidade não há como reconhecer a volta: passa na hora.
            self.dono = self.proximo();
            return self.dono;
        }
        self.carencia = Some((aparelho, agora_ms + CARENCIA_DO_DONO_MS));
        None
    }

    /// O mesmo aparelho voltou por outra sessão: a nova herda o lugar (e o som, se era da velha, ou
    /// se o som espera por este aparelho) **antes** de a velha sair.
    pub fn substituir(&mut self, velha: u64, nova: u64, nova_toca: bool, aparelho: &str, agora_ms: u64) -> Option<u64> {
        let antes = self.dono;
        self.vencer(agora_ms);
        self.conectadas.retain(|(i, _, _)| *i != nova);
        if let Some(p) = self.conectadas.iter().position(|(i, _, _)| *i == velha) {
            self.conectadas[p] = (nova, nova_toca, aparelho.to_string());
        } else {
            self.conectadas.push((nova, nova_toca, aparelho.to_string()));
        }
        if self.dono == Some(velha) {
            // O aparelho voltou por um app que não toca: é como se o dono tivesse saído.
            self.dono = if nova_toca { Some(nova) } else { self.proximo() };
        } else {
            self.entrou(nova, nova_toca, aparelho);
        }
        (self.dono != antes).then_some(self.dono).flatten()
    }

    /// A sessão `id` ficou sem som (a captura não subiu, ou a track nem foi oferecida): ela não
    /// toca, e se era a dona, o som passa na hora (crítica 13, M3).
    pub fn nao_toca(&mut self, id: u64) -> Option<u64> {
        if let Some(c) = self.conectadas.iter_mut().find(|(i, _, _)| *i == id) {
            c.1 = false;
        }
        if self.dono != Some(id) {
            return None;
        }
        self.dono = if self.carencia.is_none() { self.proximo() } else { None };
        self.dono
    }

    /// O relógio: vence a carência. Devolve o dono novo, se mudou.
    pub fn tique(&mut self, agora_ms: u64) -> Option<u64> {
        let antes = self.dono;
        self.vencer(agora_ms);
        (self.dono != antes).then_some(self.dono).flatten()
    }

    pub fn zerar(&mut self) {
        self.conectadas.clear();
        self.dono = None;
        self.carencia = None;
    }
}

// -------------------------------------------------------------------------------------------------
// Testes
// -------------------------------------------------------------------------------------------------

#[cfg(test)]
mod testes {
    use super::*;

    /// Uma porta falsa: slots de 960 quadros numerados, e o registro de cada puxada.
    struct PortaFalsa {
        proximo: u64,
        puxadas: Vec<(u32, u64)>,
        ociosas_primeiro: usize,
    }

    impl FonteDeSlots for PortaFalsa {
        fn puxar(&mut self, atraso_us: u32, no_dac_us: u64, _razao: f64, destino: &mut [f32]) -> SlotPuxado {
            self.puxadas.push((atraso_us, no_dac_us));
            if self.ociosas_primeiro > 0 {
                self.ociosas_primeiro -= 1;
                return SlotPuxado::ocioso();
            }
            for q in 0..QUADROS_POR_SLOT {
                let v = (self.proximo * QUADROS_POR_SLOT as u64 + q as u64) as f32;
                destino[q * CANAIS] = v;
                destino[q * CANAIS + 1] = -v;
            }
            let s = SlotPuxado { ordem: Ordem::Quadro, quadros: QUADROS_POR_SLOT, carimbo_us: self.proximo * 20_000 };
            self.proximo += 1;
            s
        }
    }

    #[test]
    fn o_montador_puxa_na_cadencia_do_dispositivo_sem_perder_amostra() {
        // Um período de 441 quadros (o que um mixador a 44,1 kHz com 10 ms pediria, levado a 48):
        // não casa com 960, e nenhuma amostra pode sumir nem repetir.
        let mut m = Montador::novo(PortaFalsa { proximo: 0, puxadas: Vec::new(), ociosas_primeiro: 0 });
        let mut saida = vec![0.0f32; 441 * CANAIS];
        let mut esperado = 0.0f32;
        let mut ok = true;
        for r in 0..200u64 {
            m.render(&mut saida, 10_000.0, 1.0, r * 9_188);
            for q in 0..441 {
                if saida[q * CANAIS] != esperado || saida[q * CANAIS + 1] != -esperado {
                    ok = false;
                }
                esperado += 1.0;
            }
        }
        assert!(ok, "a saída é a sequência dos slots, sem buraco nem repetição");
        // 200 × 441 = 88 200 quadros = 91,875 slots: 92 puxadas.
        assert_eq!(m.retrato.puxadas, 92);
        assert_eq!(m.retrato.quadros_pedidos, 88_200);
        assert_eq!((m.retrato.menor_render, m.retrato.maior_render), (441, 441));
    }

    #[test]
    fn o_atraso_de_cada_puxada_conta_o_que_o_pedido_ja_escreveu() {
        // Um pedido grande (2 400 quadros = 50 ms) puxa três slots: o segundo começa 20 ms depois do
        // primeiro, o terceiro 40 ms.
        let mut m = Montador::novo(PortaFalsa { proximo: 0, puxadas: Vec::new(), ociosas_primeiro: 0 });
        let mut saida = vec![0.0f32; 2_400 * CANAIS];
        m.render(&mut saida, 5_000.0, 1.0, 1_000_000);
        let p = &m.fonte.puxadas;
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], (5_000, 1_005_000));
        assert_eq!(p[1], (25_000, 1_025_000));
        assert_eq!(p[2], (45_000, 1_045_000));
        // O último quadro tocado é o terceiro slot, e a hora dele no DAC é a da terceira puxada.
        assert_eq!(m.retrato.ultimo_carimbo_us, 40_000);
        assert_eq!(m.retrato.ultimo_no_dac_us, 1_045_000);
    }

    #[test]
    fn a_razao_entra_no_atraso() {
        let mut m = Montador::novo(PortaFalsa { proximo: 0, puxadas: Vec::new(), ociosas_primeiro: 0 });
        let mut saida = vec![0.0f32; 1_920 * CANAIS];
        m.render(&mut saida, 0.0, 1.0005, 0);
        // O segundo slot começa 960 quadros depois, lidos a 1,0005 da taxa.
        let esperado = 960.0 / (48_000.0 * 1.0005) * 1e6;
        assert!((f64::from(m.fonte.puxadas[1].0) - esperado).abs() <= 1.0);
    }

    #[test]
    fn ocioso_sao_zeros_e_ocupam_um_slot() {
        let mut m = Montador::novo(PortaFalsa { proximo: 0, puxadas: Vec::new(), ociosas_primeiro: 2 });
        let mut saida = vec![7.0f32; 960 * 3 * CANAIS];
        m.render(&mut saida, 0.0, 1.0, 0);
        assert!(saida[..960 * 2 * CANAIS].iter().all(|v| *v == 0.0), "dois slots ociosos: zeros");
        assert_eq!(saida[960 * 2 * CANAIS], 0.0, "o primeiro quadro de verdade começa em 0");
        assert_eq!(saida[960 * 2 * CANAIS + 2], 1.0);
        assert_eq!(m.retrato.ociosas, 2);
        assert!(m.retrato.tem_ultimo);
    }

    #[test]
    fn o_ganho_desce_em_rampa_e_depois_fica() {
        let mut saida = vec![1.0f32; 480 * CANAIS];
        let mut g = 1.0;
        aplicar_ganho(&mut saida, &mut g, 0.0);
        assert!(saida[0] > 0.99 && saida[0] < 1.0, "começa perto de 1");
        assert!(saida[479 * CANAIS].abs() < 1e-6, "termina em 0");
        assert!(saida.windows(CANAIS * 2).step_by(CANAIS).all(|w| w[CANAIS] <= w[0]), "sem degrau para cima");
        let mut outra = vec![1.0f32; 480 * CANAIS];
        aplicar_ganho(&mut outra, &mut g, 0.0);
        assert!(outra.iter().all(|v| *v == 0.0), "mudo fica mudo");
        assert_eq!(pico(&outra), 0.0);
    }

    #[test]
    fn mulaw_bate_com_a_g711() {
        assert_eq!(mulaw_para_f32(0xFF), 0.0);
        assert_eq!(mulaw_para_f32(0x7F), 0.0);
        assert_eq!(mulaw_para_f32(0x80) * 32768.0, 32124.0);
        assert_eq!(mulaw_para_f32(0x00) * 32768.0, -32124.0);
        assert_eq!(mulaw_para_f32(0xFE) * 32768.0, 8.0);
    }

    #[test]
    fn mulaw_de_i16_volta_pela_tabela_dentro_do_degrau() {
        for &x in &[0i16, 1, -1, 7, 100, -100, 1_000, -1_000, 8_000, -8_000, 16_384, 32_000, -32_000, i16::MAX, i16::MIN] {
            let u = mulaw_de_i16(x);
            let volta = mulaw_para_f32(u) * 32768.0;
            // O degrau do µ-law no módulo |x| é (|x| + 132) / 16; o erro fica em meio degrau.
            let degrau = (f32::from(x).abs() + 132.0) / 16.0;
            assert!(
                (volta - f32::from(x).clamp(-32_635.0, 32_635.0)).abs() <= degrau,
                "{x} -> {u:#04x} -> {volta}"
            );
        }
        assert_eq!(mulaw_de_i16(0), 0xFF, "o zero da G.711");
    }

    #[test]
    fn o_tom_de_prova_tem_as_quatro_notas_e_nao_estala_na_troca() {
        let taxa = 48_000;
        let tom: Vec<f32> = (0..(2 * taxa) as u64).map(|n| tom_de_prova(n, taxa)).collect();
        for (i, nota) in NOTAS_DE_PROVA_HZ.iter().enumerate() {
            let trecho = &tom[i * 24_000..(i + 1) * 24_000];
            let a = amplitude_em(f64::from(*nota), trecho, 1, taxa);
            assert!((a - 0.5).abs() < 0.01, "nota {nota}: {a}");
        }
        let maior_salto = tom.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        // 1 kHz a 48 kHz com amplitude 0,5: o maior passo de um seno é 0,5 × 2π × 1000/48000 ≈ 0,065.
        assert!(maior_salto < 0.07, "salto de {maior_salto} numa troca de nota");
    }

    #[test]
    fn o_interpolador_leva_o_tom_de_8_a_48_khz_com_ganho_1() {
        let mut it = InterpoladorPor6::novo();
        let entrada: Vec<f32> = (0..1_600)
            .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 8_000.0).sin() as f32)
            .collect();
        let mut saida = vec![0.0f32; 1_600 * 6];
        it.processar(&entrada, &mut saida);
        let pico_no_meio = saida[2_000..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((pico_no_meio - 0.5).abs() < 0.01, "ganho 1: {pico_no_meio}");
        // Uma constante sai constante.
        let mut dc = InterpoladorPor6::novo();
        let mut s = vec![0.0f32; 60 * 6];
        dc.processar(&[0.25; 60], &mut s);
        assert!(s[200..].iter().all(|v| (v - 0.25).abs() < 1e-3));
        assert!((INTERPOLADOR_ATRASO_US - 1_489.583).abs() < 0.01);
    }

    #[test]
    fn a_vontade_do_som_d1_e_d3() {
        assert_eq!(VontadeDoSom::default().ganho(), 1.0, "D1: ligado por padrão");
        assert_eq!(VontadeDoSom { mudo: true, ..Default::default() }.ganho(), 0.0);
        assert_eq!(VontadeDoSom { volume: 0.4, ..Default::default() }.ganho(), 0.4);
        let em_uso = VontadeDoSom { camera_em_uso: true, ..Default::default() };
        assert_eq!(em_uso.ganho(), 0.0, "D3: a câmera do Quall em uso cala");
        assert_eq!(em_uso.por_que_calado(), Some("mudo: a câmera do Quall está em uso"));
        let com_chave = VontadeDoSom { tocar_com_a_camera: true, ..em_uso };
        assert_eq!(com_chave.ganho(), 1.0, "a chave da janela desfaz");
        assert_eq!(VontadeDoSom { mudo: true, ..com_chave }.ganho(), 0.0, "o mudo continua valendo");
    }

    #[test]
    fn d2_o_dono_e_o_primeiro_que_toca_e_passa_ao_proximo() {
        let mut d = DonoDoSom::novo(true);
        assert_eq!(d.conectou(1, false, "obs", 0), None, "quem não toca não é dono");
        assert_eq!(d.conectou(2, true, "android-a", 0), Some(2));
        assert_eq!(d.conectou(3, true, "mac-b", 0), None);
        assert_eq!(d.dono(), Some(2));
        assert_eq!(d.saiu(1), None, "sair quem não é dono não muda nada");
        assert_eq!(d.saiu(2), Some(3), "o som passa ao próximo que toca");
        let mut sem = DonoDoSom::novo(false);
        sem.conectou(1, true, "android-a", 0);
        sem.conectou(2, true, "mac-b", 0);
        assert_eq!(sem.saiu(1), None, "sem a passagem, o som não vai a quem já estava");
        assert_eq!(sem.dono(), None);
    }

    #[test]
    fn d2_o_dono_que_reconecta_mantem_o_som() {
        let mut d = DonoDoSom::novo(true);
        d.conectou(1, true, "android-a", 0);
        d.conectou(2, true, "mac-b", 0);
        assert_eq!(d.substituir(1, 3, true, "android-a", 1_000), Some(3), "a sessão nova herda o som da velha");
        assert_eq!(d.saiu(1), None, "a saída da velha já não acha o dono");
        assert_eq!(d.dono(), Some(3));
    }

    // --- a carência de 10 s (decisão do Bruno de 18/09): os mesmos casos em `TestesDoDonoDoSom.swift`

    #[test]
    fn d2_o_dono_que_cai_e_volta_em_10_s_mantem_o_som() {
        let mut d = DonoDoSom::novo(true);
        d.conectou(1, true, "android-a", 0);
        d.conectou(2, true, "mac-b", 0);
        assert_eq!(d.caiu(1, 1_000), None, "o som não passa na queda");
        assert_eq!(d.dono(), None, "nos 10 s ninguém toca");
        assert_eq!(d.carencia(), Some(("android-a", 11_000)));
        assert_eq!(d.tique(10_999), None);
        assert_eq!(d.conectou(3, true, "android-a", 10_999), Some(3), "o mesmo aparelho volta e o som é dele");
        assert_eq!(d.carencia(), None);
        assert_eq!(d.tique(20_000), None, "sem carência, o relógio não mexe");
        assert_eq!(d.dono(), Some(3));
    }

    #[test]
    fn d2_depois_de_10_s_o_som_passa_ao_proximo() {
        let mut d = DonoDoSom::novo(true);
        d.conectou(1, true, "android-a", 0);
        d.conectou(2, true, "mac-b", 0);
        d.caiu(1, 1_000);
        assert_eq!(d.tique(11_000), Some(2), "vencido o prazo, passa ao próximo que toca");
        assert_eq!(d.conectou(3, true, "android-a", 12_000), None, "o dono antigo volta tarde e fica calado");
        assert_eq!(d.dono(), Some(2));
    }

    #[test]
    fn d2_nos_10_s_ninguem_toca_nem_quem_chega() {
        let mut d = DonoDoSom::novo(true);
        d.conectou(1, true, "android-a", 0);
        d.conectou(2, true, "mac-b", 0);
        d.caiu(1, 1_000);
        assert_eq!(d.conectou(3, true, "ios-c", 2_000), None, "quem chega na carência não pega o som");
        assert_eq!(d.saiu(2), None, "quem sai na carência não passa nada");
        assert_eq!(d.dono(), None);
        assert_eq!(d.conectou(4, true, "win-d", 11_500), Some(3), "a chegada depois do prazo vence a carência primeiro");
    }

    #[test]
    fn d2_quem_sai_por_gesto_passa_na_hora() {
        let mut d = DonoDoSom::novo(true);
        d.conectou(1, true, "android-a", 0);
        d.conectou(2, true, "mac-b", 0);
        assert_eq!(d.saiu(1), Some(2));
        assert_eq!(d.carencia(), None);
    }

    #[test]
    fn d2_o_dono_que_volta_sem_tocar_passa_na_hora() {
        let mut d = DonoDoSom::novo(true);
        d.conectou(1, true, "android-a", 0);
        d.conectou(2, true, "mac-b", 0);
        d.caiu(1, 1_000);
        assert_eq!(d.conectou(3, false, "android-a", 2_000), Some(2), "voltou por um app que não toca");
        assert_eq!(d.carencia(), None);
    }

    #[test]
    fn d2_a_sessao_sem_som_nao_segura_o_som() {
        let mut d = DonoDoSom::novo(true);
        d.conectou(1, true, "android-a", 0);
        d.conectou(2, true, "mac-b", 0);
        assert_eq!(d.nao_toca(1), Some(2), "a dona que perdeu o som passa na hora");
        assert_eq!(d.nao_toca(2), None, "sem ninguém que toque, fica sem dono");
        assert_eq!(d.dono(), None);
    }

    #[test]
    fn d2_a_tabela_de_quem_toca() {
        for (id, toca) in [
            ("mac-3916", true),
            ("android-a07", true),
            ("win-g3", true),
            ("WIN-G3", true),
            ("ios-iphone", true),
            ("9e7b34b1-6c0a", false),
            ("probe-1234", false),
        ] {
            assert_eq!(toca_o_som_do_windows(id), toca, "{id}");
        }
    }

    // --- o detector do estouro da claquete (S7): os testes do Mac, portados ---------------------

    /// `slots` slots de 960 amostras, **estéreo intercalado** como o render escreve: tom de 1 kHz
    /// (calado ±40 ms em volta) e um estouro de 10 ms a 3 150 Hz começando em `comeco`.
    fn sinal_com_estouro(comeco: usize, amplitude: f32, slots: usize) -> Vec<f32> {
        let n = slots * 960;
        let mut x = vec![0.0f32; n * 2];
        for i in 0..n {
            let centro = comeco + 240;
            let v = if i >= comeco && i < comeco + 480 {
                amplitude * (2.0 * std::f32::consts::PI * 3150.0 * (i - comeco) as f32 / 48_000.0).sin()
            } else if i.abs_diff(centro) < 1920 {
                0.0
            } else {
                0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin()
            };
            x[2 * i] = v;
            x[2 * i + 1] = v;
        }
        x
    }

    fn detectar(x: &[f32]) -> Vec<f64> {
        let mut d = DetectorDeEstouro::novo(48_000.0, QUADROS_POR_SLOT);
        let mut achados = Vec::new();
        for (k, slot) in x.chunks(960 * 2).enumerate() {
            if let Some(o) = d.processar(slot, 2) {
                achados.push((k * 960) as f64 + o);
            }
        }
        achados
    }

    /// Em todas as fases do passo de 1 ms, e dos dois lados da emenda entre slots: o começo sai a
    /// menos de meio passo, e o tom de fundo não dispara nada (os limites do teste do Mac).
    #[test]
    fn o_comeco_do_estouro_sai_a_menos_de_meio_milissegundo() {
        let mut erros = Vec::new();
        for comeco in (3000..3096).step_by(4).chain([4790, 4800, 4810, 5750]) {
            let achados = detectar(&sinal_com_estouro(comeco, 0.5, 10));
            assert_eq!(achados.len(), 1, "um estouro em {comeco}: {achados:?}");
            erros.push(achados[0] - comeco as f64);
        }
        let media = erros.iter().sum::<f64>() / erros.len() as f64;
        let pior = erros.iter().map(|e| (e - media).abs()).fold(0.0, f64::max);
        eprintln!("detector: viés médio {media:.1} amostras, pior desvio {pior:.1} amostras");
        assert!(media.abs() < 6.0, "viés médio {media}");
        assert!(pior < 26.0, "pior desvio {pior}");
    }

    #[test]
    fn o_tom_sozinho_nao_dispara() {
        let notas = [400.0f32, 500.0, 800.0, 1000.0];
        let n = 960 * 20;
        let mut x = vec![0.0f32; n * 2];
        for i in 0..n {
            let f = notas[(i / 12_000) % 4];
            let v = 0.5 * (2.0 * std::f32::consts::PI * f * i as f32 / 48_000.0).sin();
            x[2 * i] = v;
            x[2 * i + 1] = v;
        }
        assert!(detectar(&x).is_empty(), "o tom de fundo não é estouro");
    }

    /// Mais fraco (o PCMU interpolado atenua 3 150 Hz) ainda é achado.
    #[test]
    fn o_estouro_atenuado_ainda_e_achado() {
        assert_eq!(detectar(&sinal_com_estouro(3100, 0.25, 10)).len(), 1);
    }
}
