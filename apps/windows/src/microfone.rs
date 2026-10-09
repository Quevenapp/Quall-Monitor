//! **O microfone dos emissores de câmera** (`docs/teleprompter-com-camera.md` §4 e §8.10, peça 4;
//! `docs/audio.md` §8.2): a regra "este app nunca abre o microfone" **caiu para todo emissor de
//! câmera** em 24/09 (Bruno). A regra da bancada fica (`audio.md` §8.1): toda prova usa o tom, e o
//! microfone de verdade só com o sim do Bruno por corrida.
//!
//! - **Um botão, que começa desligado.** Ligado, a captura WASAPI **de captura** (`eCapture`, o
//!   microfone padrão, `eConsole`) abre de verdade — o ícone de microfone do Windows acende —;
//!   desligado, fecha. Não é mudo por software.
//! - **Som cru, como filmadora**: `AUDCLNT_STREAMOPTIONS_RAW` e a categoria `Other` pedidos por
//!   `IAudioClient2::SetClientProperties` (sem os efeitos do driver: supressor, eco, ganho). Recusado,
//!   segue sem, e o registro diz (hipótese: o driver do Dell não declara RAW).
//! - **A linha do tempo é a do loopback** (`linha_do_loopback.rs`): a hora de cada pacote pelo
//!   `u64QPCPosition` do `GetBuffer` (a hora da **captura**), o reamostrador para 48 kHz com a
//!   disciplina da deriva, o buraco pela posição do dispositivo. Todos os canais do microfone são
//!   misturados em mono (a média: um arranjo de microfones em RAW entrega vários), duplicados para os
//!   dois canais da linha, e o canal da esquerda sai: quadros de 20 ms, 48 kHz mono, carimbados **no
//!   zero do dono** (`origem`).
//! - **Duas saídas**: o **ramal da rede** (Opus mono 32 kbit/s com FEC pelo `PRESET_MICROFONE`, uma
//!   fila nova por sessão, que o laço drena como uma `CadeiaDeAudio`) e o **ramal do gravador** (PCM).
//! - **A Privacidade** (§4.4): a abertura que falha com `E_ACCESSDENIED`, ou o `ConsentStore` do
//!   registro dizendo `Deny`, viram a frase de `regras_r5::FRASE_DA_PRIVACIDADE`, e o botão volta a
//!   desligado. O registro é só lido.
//! - **Bancada** (`--microfone-tom HZ`): o conteúdo de cada quadro vira um seno **depois** do carimbo —
//!   o microfone abre (o ícone acende), o carimbo é o da captura, e o som da sala não sai do processo.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Sender};
use windows::core::{w, PCWSTR};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, AudioCategory_Other, AudioClientProperties, IAudioCaptureClient, IAudioClient, IAudioClient2,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMOPTIONS_RAW,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

use quall_core::track::PRESET_MICROFONE;

use crate::idioma::{t, tf};
use quall_opus::Codificador;

use crate::audio::{self, CadeiaDeAudio, ContadoresDeAudio, FormatoDoMixador, PacoteDeAudio};
use crate::gravador_local::QuadroDePcm;
use crate::linha_do_loopback::{hora_do_pacote, na_origem, LinhaDoLoopback, Pacote};
use crate::regras_r5::{self, Consentimento};
use crate::registro;

/// O que a tela lê.
#[derive(Clone, Debug, Default)]
pub struct EstadoDoMicrofone {
    /// O botão está ligado (a pessoa quer o microfone).
    pub ligado: bool,
    /// A captura está de pé agora.
    pub aberto: bool,
    /// Por que não abriu (ou caiu), para a tela; vazio quando está tudo bem.
    pub frase: String,
    /// O que abriu: o formato do mixador, se o RAW pegou, os canais.
    pub descricao: String,
    /// Quadros de 20 ms desde que abriu.
    pub quadros: u64,
    /// O maior intervalo entre dois pacotes do WASAPI na última janela de 10 s (o soluço da captura).
    pub buraco_maior_ms: u64,
}

struct Comum {
    origem: Instant,
    tom: Option<u32>,
    estado: Mutex<EstadoDoMicrofone>,
    rede: Mutex<Option<Sender<PacoteDeAudio>>>,
    contadores: Mutex<Arc<ContadoresDeAudio>>,
    gravador: Mutex<Option<Box<dyn Fn(QuadroDePcm) + Send + Sync>>>,
    acordar: Box<dyn Fn() + Send + Sync>,
}

/// O microfone de uma tela (a R5, ou a janela principal com câmera).
pub struct Microfone {
    comum: Arc<Comum>,
    thread: Mutex<Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>>,
}

impl Microfone {
    /// O microfone, **desligado**. `origem` é o zero de relógio do vídeo (o do dono, na tela R5).
    pub fn novo(origem: Instant, tom: Option<u32>, acordar: Box<dyn Fn() + Send + Sync>) -> Arc<Microfone> {
        Arc::new(Microfone {
            comum: Arc::new(Comum {
                origem,
                tom,
                estado: Mutex::new(EstadoDoMicrofone::default()),
                rede: Mutex::new(None),
                contadores: Mutex::new(Arc::new(ContadoresDeAudio::default())),
                gravador: Mutex::new(None),
                acordar,
            }),
            thread: Mutex::new(None),
        })
    }

    pub fn estado(&self) -> EstadoDoMicrofone {
        self.comum.estado.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn ligado(&self) -> bool {
        self.estado().ligado
    }

    /// **Liga**: abre a captura numa thread própria. A frase da falha (a Privacidade) vai ao estado.
    pub fn ligar(&self, porque: &str) {
        let mut t = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((parar, h)) = t.as_ref() {
            if !h.is_finished() {
                if parar.load(Ordering::SeqCst) {
                    // **Nunca duas capturas ao mesmo tempo** (a revisão do código): a anterior ainda
                    // está fechando. O botão volta a desligado, com a frase.
                    let mut e = self.comum.estado.lock().unwrap_or_else(|x| x.into_inner());
                    e.ligado = false;
                    e.frase = crate::idioma::t("O microfone ainda está fechando; tente de novo em um instante.").into();
                    drop(e);
                    registro::linha(format!("microfone: ligar recusado ({porque}): a captura anterior ainda não saiu"));
                    (self.comum.acordar)();
                }
                return;
            }
        }
        if let Some((_, h)) = t.take() {
            let _ = h.join();
        }
        {
            let mut e = self.comum.estado.lock().unwrap_or_else(|x| x.into_inner());
            e.ligado = true;
            e.frase.clear();
        }
        registro::linha(format!("microfone: botão ligado ({porque}){}", if self.comum.tom.is_some() { " — bancada: o conteúdo vira o tom depois do carimbo" } else { "" }));
        let parar = Arc::new(AtomicBool::new(false));
        let p = Arc::clone(&parar);
        let c = Arc::clone(&self.comum);
        match std::thread::Builder::new().name("quall.microfone".into()).spawn(move || correr(c, p)) {
            Ok(h) => *t = Some((parar, h)),
            Err(e) => {
                let mut es = self.comum.estado.lock().unwrap_or_else(|x| x.into_inner());
                es.ligado = false;
                es.frase = tf("O microfone não abriu: a thread não subiu ({})", &[&e]);
            }
        }
        (self.comum.acordar)();
    }

    /// **Desliga sem esperar** (a thread da janela; a revisão do código, M1): a captura fecha na
    /// thread dela, e [`Microfone::terminou`] diz quando.
    pub fn pedir_desligar(&self, porque: &str) {
        let t = self.thread.lock().unwrap_or_else(|e| e.into_inner());
        let mut e = self.comum.estado.lock().unwrap_or_else(|x| x.into_inner());
        if !e.ligado && t.as_ref().is_none_or(|(_, h)| h.is_finished()) {
            return;
        }
        e.ligado = false;
        drop(e);
        if let Some((parar, _)) = t.as_ref() {
            parar.store(true, Ordering::SeqCst);
        }
        drop(t);
        registro::linha(format!("microfone: botão desligado ({porque})"));
        (self.comum.acordar)();
    }

    /// A captura saiu (ou nunca subiu)?
    pub fn terminou(&self) -> bool {
        self.thread.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_none_or(|(_, h)| h.is_finished())
    }

    /// **Desliga e espera** a thread (até 2 s): só de quem não é dono de janela (a sessão).
    pub fn desligar(&self, porque: &str) {
        self.pedir_desligar(porque);
        let fim = Instant::now() + Duration::from_secs(2);
        while !self.terminou() && Instant::now() < fim {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !self.terminou() {
            registro::linha("microfone: !! a thread não saiu em 2 s (fica para trás)");
        }
    }

    /// **O ramal da rede de uma sessão**: uma fila nova (nada de pacote velho de outra sessão), que o
    /// laço drena. Pendurar de novo solta o anterior.
    pub fn ramal_da_rede(&self) -> CadeiaDeAudio {
        let (tx, rx) = bounded::<PacoteDeAudio>(50);
        let contadores = Arc::new(ContadoresDeAudio::default());
        *self.comum.contadores.lock().unwrap_or_else(|e| e.into_inner()) = Arc::clone(&contadores);
        *self.comum.rede.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let d = self.estado().descricao;
        CadeiaDeAudio::de_ramal(rx, contadores, if d.is_empty() { "microfone (desligado)".into() } else { d })
    }

    pub fn soltar_ramal_da_rede(&self) {
        *self.comum.rede.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Pendura (ou solta) o ramal do gravador.
    pub fn pendurar_gravador(&self, f: Option<Box<dyn Fn(QuadroDePcm) + Send + Sync>>) {
        *self.comum.gravador.lock().unwrap_or_else(|e| e.into_inner()) = f;
    }
}

impl Drop for Microfone {
    fn drop(&mut self) {
        if let Some((parar, h)) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            parar.store(true, Ordering::SeqCst);
            let _ = h.join();
        }
    }
}

/// Um valor do `ConsentStore` (só leitura).
fn consentimento(raiz: HKEY, subchave: PCWSTR) -> Consentimento {
    // O par `(buf, cap)` do registro: primeiro o tamanho, depois o valor.
    let mut tam = 0u32;
    if unsafe { RegGetValueW(raiz, subchave, w!("Value"), RRF_RT_REG_SZ, None, None, Some(&mut tam)) }.is_err() || tam == 0 { // i18n: fora (registro do Windows e diário)
        return Consentimento::Desconhecido;
    }
    let mut valor: Vec<u16> = vec![0; (tam as usize).div_ceil(2)];
    let r = unsafe { RegGetValueW(raiz, subchave, w!("Value"), RRF_RT_REG_SZ, None, Some(valor.as_mut_ptr() as *mut _), Some(&mut tam)) }; // i18n: fora (registro do Windows e diário)
    if r.is_err() {
        return Consentimento::Desconhecido;
    }
    let n = (tam as usize / 2).min(valor.len());
    let texto = String::from_utf16_lossy(&valor[..n]);
    regras_r5::ler_consentimento(Some(texto.trim_end_matches('\0')))
}

/// `(a máquina, o usuário, os apps da área de trabalho)`.
pub fn consentimentos() -> (Consentimento, Consentimento, Consentimento) {
    let chave = w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone");
    let area = w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone\\NonPackaged");
    (consentimento(HKEY_LOCAL_MACHINE, chave), consentimento(HKEY_CURRENT_USER, chave), consentimento(HKEY_CURRENT_USER, area))
}

struct Captura {
    cliente: IAudioClient,
    captura: IAudioCaptureClient,
    formato: FormatoDoMixador,
    cru: String,
}

impl Drop for Captura {
    fn drop(&mut self) {
        unsafe {
            let _ = self.cliente.Stop();
        }
    }
}

fn abrir() -> windows::core::Result<Captura> {
    let e: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let dispositivo = unsafe { e.GetDefaultAudioEndpoint(eCapture, eConsole)? };
    let cliente: IAudioClient = unsafe { dispositivo.Activate(CLSCTX_ALL, None)? };
    // O som cru: RAW e a categoria `Other`, antes do `Initialize`.
    let cru = match windows::core::Interface::cast::<IAudioClient2>(&cliente) {
        Ok(c2) => {
            let p = AudioClientProperties {
                cbSize: std::mem::size_of::<AudioClientProperties>() as u32,
                bIsOffload: false.into(),
                eCategory: AudioCategory_Other,
                Options: AUDCLNT_STREAMOPTIONS_RAW,
            };
            match unsafe { c2.SetClientProperties(&p) } {
                Ok(()) => "RAW aceito (categoria Other)".to_string(),
                Err(e) => {
                    let p = AudioClientProperties { Options: Default::default(), ..p };
                    let _ = unsafe { c2.SetClientProperties(&p) };
                    format!("RAW recusado ({e}); segue com a categoria Other e os efeitos do driver") // i18n: fora (registro do Windows e diário)
                }
            }
        }
        Err(_) => "sem IAudioClient2: sem RAW".to_string(), // i18n: fora (registro do Windows e diário)
    };
    let formato = FormatoDoMixador::ler(&cliente)?;
    unsafe {
        let p = cliente.GetMixFormat()?;
        let r = cliente.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, p, None);
        CoTaskMemFree(Some(p as *const _));
        r?;
    }
    let captura: IAudioCaptureClient = unsafe { cliente.GetService()? };
    unsafe { cliente.Start()? };
    Ok(Captura { cliente, captura, formato, cru })
}

fn correr(comum: Arc<Comum>, parar: Arc<AtomicBool>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let (maquina, usuario, area) = consentimentos();
    let t0 = Instant::now();
    let falhar = |frase: String, detalhe: String| {
        registro::linha(format!("microfone: !! {detalhe} — \"{frase}\""));
        let mut e = comum.estado.lock().unwrap_or_else(|x| x.into_inner());
        e.ligado = false;
        e.aberto = false;
        e.frase = frase;
        drop(e);
        (comum.acordar)();
    };
    let captura = match abrir() {
        Ok(c) => c,
        Err(e) => {
            let codigo = e.code().0 as u32;
            falhar(
                regras_r5::frase_do_microfone(Some(codigo), maquina, usuario, area, &e.message()),
                format!(
                    "não abriu em {} ms: {e} (0x{codigo:08X}); ConsentStore máquina={maquina:?} usuário={usuario:?} área_de_trabalho={area:?}", // i18n: fora (registro do Windows e diário)
                    t0.elapsed().as_millis()
                ),
            );
            if com.is_ok() {
                unsafe { CoUninitialize() };
            }
            return;
        }
    };
    let mut codificador = match Codificador::novo(48_000, 1, audio::aplicacao_do(&PRESET_MICROFONE)) {
        Ok(mut c) => match audio::configurar(&mut c, &PRESET_MICROFONE) {
            Ok(()) => Some(c),
            Err(e) => {
                registro::linha(format!("microfone: !! a libopus recusou o preset: {e} (a rede fica sem som)"));
                None
            }
        },
        Err(e) => {
            registro::linha(format!("microfone: !! a libopus não abriu: {e} (a rede fica sem som)"));
            None
        }
    };
    let formato = captura.formato;
    let descricao = format!(
        "microfone padrão: {formato} | {} | saída 48 kHz mono 20 ms, Opus {} bit/s FEC={} | ConsentStore máquina={maquina:?} usuário={usuario:?} área_de_trabalho={area:?}", // i18n: fora (registro do Windows e diário)
        captura.cru, PRESET_MICROFONE.taxa_media_bits, PRESET_MICROFONE.fec
    );
    registro::linha(format!("microfone: aberto em {} ms — {descricao}", t0.elapsed().as_millis()));
    {
        let mut e = comum.estado.lock().unwrap_or_else(|x| x.into_inner());
        e.aberto = true;
        e.descricao = descricao;
        e.quadros = 0;
        // O registro diz "negado" e a abertura passou: o Windows pode entregar silêncio (hipótese do
        // §4.4). A tela diz o mesmo.
        e.frase = if regras_r5::negado_pelo_registro(maquina, usuario, area) { regras_r5::FRASE_DA_PRIVACIDADE.to_string() } else { String::new() };
    }
    (comum.acordar)();

    let par_origem_us = comum.origem.elapsed().as_micros() as u64;
    let par_qpc_us = audio::qpc_agora_us();
    let mut linha = LinhaDoLoopback::nova(formato.taxa_hz, true, false);
    linha.comecar(par_qpc_us);
    let mut para_f32: Vec<f32> = Vec::new();
    let mut estereo: Vec<f32> = Vec::new();
    let mut saida_opus = vec![0u8; 1500];
    let mut fase_do_tom = 0f64;
    let mut ultimo_pacote: Option<Instant> = None;
    let mut buraco_maior = Duration::ZERO;
    let mut ultimo_relato = Instant::now();
    let canais = formato.canais.max(1) as usize;
    let mut motivo_do_fim = String::new();

    'fora: while !parar.load(Ordering::SeqCst) {
        let contadores = Arc::clone(&comum.contadores.lock().unwrap_or_else(|e| e.into_inner()));
        let mut veio = false;
        loop {
            let disponivel = match unsafe { captura.captura.GetNextPacketSize() } {
                Ok(n) => n,
                Err(e) => {
                    motivo_do_fim = if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
                        t("O microfone foi desconectado (ou trocado nas configurações de som).").into()
                    } else {
                        tf("O microfone parou: {}", &[&e])
                    };
                    break 'fora;
                }
            };
            if disponivel == 0 {
                break;
            }
            veio = true;
            let agora = Instant::now();
            if let Some(u) = ultimo_pacote {
                buraco_maior = buraco_maior.max(agora - u);
            }
            ultimo_pacote = Some(agora);
            let mut dados: *mut u8 = std::ptr::null_mut();
            let mut quadros = 0u32;
            let mut bandeiras = 0u32;
            let mut posicao = 0u64;
            let mut qpc = 0u64;
            if let Err(e) = unsafe { captura.captura.GetBuffer(&mut dados, &mut quadros, &mut bandeiras, Some(&mut posicao), Some(&mut qpc)) } {
                motivo_do_fim = if e.code() == AUDCLNT_E_DEVICE_INVALIDATED {
                    t("O microfone foi desconectado (ou trocado nas configurações de som).").into()
                } else {
                    tf("O microfone parou: {}", &[&e])
                };
                break 'fora;
            }
            let hora_us = hora_do_pacote(bandeiras, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32, qpc);
            let descontinuidade = bandeiras & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
            contadores.blocos.fetch_add(1, Ordering::Relaxed);
            contadores.quadros_pcm.fetch_add(u64::from(quadros), Ordering::Relaxed);
            if descontinuidade {
                contadores.descontinuidades.fetch_add(1, Ordering::Relaxed);
            }
            para_f32.clear();
            if bandeiras & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                contadores.blocos_silenciosos.fetch_add(1, Ordering::Relaxed);
                para_f32.resize(quadros as usize * canais, 0.0);
            } else {
                let bytes = quadros as usize * formato.bytes_por_quadro as usize;
                let bruto = unsafe { std::slice::from_raw_parts(dados, bytes) };
                audio::amostras_para_f32(bruto, formato, &mut para_f32);
            }
            unsafe {
                let _ = captura.captura.ReleaseBuffer(quadros);
            }
            // Todos os canais em mono (a média), e o mono nos dois canais da linha.
            estereo.clear();
            for q in para_f32.chunks_exact(canais) {
                let m = q.iter().sum::<f32>() / canais as f32;
                estereo.push(m);
                estereo.push(m);
            }
            linha.pacote(Pacote { amostras: &estereo, quadros: u64::from(quadros), hora_us, posicao: Some(posicao), descontinuidade });
        }
        if !veio {
            linha.ocioso(audio::qpc_agora_us());
        }
        while let Some((amostras, carimbo_qpc_us)) = linha.proximo_quadro() {
            let carimbo_us = na_origem(par_origem_us, par_qpc_us, carimbo_qpc_us);
            let mut mono: Vec<i16> = amostras.chunks_exact(2).map(|p| audio::para_i16(p[0])).collect();
            if let Some(hz) = comum.tom {
                // Bancada: o conteúdo vira o tom **depois** do carimbo (a sala não sai do processo).
                for a in mono.iter_mut() {
                    *a = (0.1 * (fase_do_tom * std::f64::consts::TAU).sin() * 32767.0) as i16;
                    fase_do_tom = (fase_do_tom + f64::from(hz) / 48_000.0).fract();
                }
            }
            contadores.quadros_de_20ms.fetch_add(1, Ordering::Relaxed);
            comum.estado.lock().unwrap_or_else(|e| e.into_inner()).quadros += 1;
            if let Some(g) = comum.gravador.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                g(QuadroDePcm { carimbo_us, amostras: mono.clone() });
            }
            let rede = comum.rede.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let (Some(tx), Some(c)) = (rede, codificador.as_mut()) {
                match c.codificar(&mono, &mut saida_opus) {
                    Ok(n) => {
                        contadores.pacotes_opus.fetch_add(1, Ordering::Relaxed);
                        contadores.bytes_opus.fetch_add(n as u64, Ordering::Relaxed);
                        if tx.try_send(PacoteDeAudio { bytes: saida_opus[..n].to_vec(), timestamp_us: carimbo_us }).is_err() {
                            contadores.descartados_por_fila.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        contadores.falhas_de_encode.fetch_add(1, Ordering::Relaxed);
                        if contadores.falhas_de_encode.load(Ordering::Relaxed) <= 3 {
                            registro::linha(format!("microfone: codificar falhou: {e}"));
                        }
                    }
                }
            }
        }
        if ultimo_relato.elapsed() >= Duration::from_secs(10) {
            ultimo_relato = Instant::now();
            comum.estado.lock().unwrap_or_else(|e| e.into_inner()).buraco_maior_ms = buraco_maior.as_millis() as u64;
            registro::linha(format!(
                "microfone: quadros={} buraco_maior_da_captura={} ms | disciplina f_ppm={:.1} degraus={} | {}",
                comum.estado.lock().unwrap_or_else(|e| e.into_inner()).quadros,
                buraco_maior.as_millis(),
                linha.f_ppm(),
                linha.degraus,
                contadores.linha()
            ));
            buraco_maior = Duration::ZERO;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(captura);
    let quadros = comum.estado.lock().unwrap_or_else(|e| e.into_inner()).quadros;
    if motivo_do_fim.is_empty() {
        registro::linha(format!("microfone: fechado de verdade (a captura parou) depois de {quadros} quadros"));
        let mut e = comum.estado.lock().unwrap_or_else(|x| x.into_inner());
        e.aberto = false;
    } else {
        falhar(motivo_do_fim.clone(), format!("caiu depois de {quadros} quadros: {motivo_do_fim}")); // i18n: fora (registro do Windows e diário)
    }
    (comum.acordar)();
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
}
