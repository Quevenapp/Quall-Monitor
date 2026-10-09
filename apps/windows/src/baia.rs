//! Uma **baia**: a câmera virtual de um aparelho, com o cano que a alimenta.
//!
//! É a peça que faltava para o pedido *"o app cria uma fonte de dispositivo virtual para cada
//! aparelho pareado"*. Cada baia é, ao mesmo tempo:
//!
//! - um **nó de câmera** no Windows, com o nome do aparelho, criado por `MFCreateVirtualCamera`;
//! - um **servidor de cano nomeado**, `\\.\pipe\quall-camera-v1-<fnv64 do nome>`, que é onde a
//!   fonte de mídia daquele nó vai bater — ela deriva o mesmo nome do **nome da câmera**, medido
//!   em `docs/bancada.md` §8.59 e §8.60;
//! - uma **placa de espera**, para o que o Zoom mostra quando aquele aparelho não está
//!   transmitindo. Sem ela, escolher a câmera de um celular desligado dá preto — e preto não
//!   distingue "o aparelho não está transmitindo" de "o Quall travou".
//!
//! # As duas vidas, e por que elas são diferentes
//!
//! **A baia vive com o processo do app; a sessão vive com o aparelho.** É a correção mais
//! importante que a revisão adversarial desta frente pegou: com a baia amarrada à sessão, o cano
//! só existiria enquanto alguém estivesse transmitindo, e quem abrisse o Meet antes de mexer no
//! celular veria a **barra de bancada da fonte** — que quer dizer outra coisa ("o Quall nem está
//! rodando") — em vez da placa dizendo o que fazer.
//!
//! Por isso `Baia::abrir` é chamada uma vez por aparelho conhecido, quando o app sobe, e a sessão
//! só **publica** nela.
//!
//! # O que esta baia NÃO faz
//!
//! Não chama `IMFVirtualCamera::Remove()`. Esta bancada mediu três vezes que ele devolve `S_OK`
//! em 42 ms e **não remove nada** (`integrations/camera-windows/README.md`), e uma remoção parcial
//! é justamente o que fabrica o fantasma `AvStream Media Device`. O que faz a câmera sumir é a
//! vida `MFVirtualCameraLifetime_Session`, medida em §8.58: os dois nós de teste sumiram da
//! enumeração quando os processos morreram — e morreram por `Stop-Process`, não por saída limpa.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::{
    IMFVirtualCamera, MFCreateVirtualCamera, MFVirtualCameraAccess_CurrentUser,
    MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource,
    MF_E_INVALIDREQUEST,
};

use crate::cano;
use crate::placa::{Estado, Placa};

// ---------------------------------------------------------------------------------------------
// Distribuidor: o último quadro, e quem o espera
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Ultimo {
    /// **`Arc`, e não `Vec`.** Quatro fios servidores acordam no mesmo `notify_all` e cada um
    /// levava uma cópia de 3,11 MB **com o cadeado na mão**; o produtor ficava atrás dessa fila.
    /// Na sonda isso era inofensivo (um processo, um cano); no app o produtor é a thread da
    /// sessão de vídeo, e segurá-la é perder quadro na tela. Clonar o `Arc` é um incremento.
    bytes: Option<Arc<Vec<u8>>>,
    /// Sobe a cada publicação. É por ela que um fio do cano sabe que há quadro **novo**, sem
    /// comparar 3,11 MB de pixel.
    geracao: u64,
    /// Carimbo que vai no cabeçalho do cano (QPC em µs).
    ts_us: u64,
    /// Quando o último quadro **de vídeo** foi publicado.
    video_em: Option<Instant>,
}

pub struct Distribuidor {
    estado: Mutex<Ultimo>,
    chegou: Condvar,
    publicados_video: AtomicU64,
    publicados_placa: AtomicU64,
}

impl Distribuidor {
    pub fn novo() -> Arc<Self> {
        Arc::new(Distribuidor {
            estado: Mutex::new(Ultimo::default()),
            chegou: Condvar::new(),
            publicados_video: AtomicU64::new(0),
            publicados_placa: AtomicU64::new(0),
        })
    }

    pub fn publicar(&self, bytes: Arc<Vec<u8>>, ts_us: u64, e_video: bool) {
        let mut e = self.estado.lock().unwrap();
        if !e_video {
            // A placa nunca passa na frente de vídeo recente. Sem esta guarda, um soluço de
            // meio segundo na rede faria a placa piscar por cima do vídeo — o que é pior que a
            // imagem congelar, porque parece defeito do Quall e não da rede.
            if let Some(quando) = e.video_em {
                if quando.elapsed() < Duration::from_millis(500) {
                    return;
                }
            }
            self.publicados_placa.fetch_add(1, Ordering::Relaxed);
        } else {
            e.video_em = Some(Instant::now());
            self.publicados_video.fetch_add(1, Ordering::Relaxed);
        }
        e.bytes = Some(bytes);
        e.ts_us = ts_us;
        e.geracao += 1;
        drop(e);
        self.chegou.notify_all();
    }

    /// Espera um quadro com geração maior que `vista`. Devolve `None` no fim do prazo — o que não
    /// é erro: é o fio do cano acordando para conferir se é hora de parar.
    pub fn esperar(&self, vista: u64, prazo: Duration) -> Option<(Arc<Vec<u8>>, u64, u64)> {
        let mut e = self.estado.lock().unwrap();
        if e.geracao <= vista {
            let (g, _) = self.chegou.wait_timeout(e, prazo).unwrap();
            e = g;
        }
        if e.geracao > vista {
            e.bytes.as_ref().map(|b| (Arc::clone(b), e.geracao, e.ts_us))
        } else {
            None
        }
    }

    pub fn ha_video_recente(&self) -> bool {
        let e = self.estado.lock().unwrap();
        e.video_em
            .map(|q| q.elapsed() < Duration::from_millis(500))
            .unwrap_or(false)
    }

    /// Acorda todo mundo que está esperando quadro, **sem publicar nada**.
    ///
    /// É o que se chama na hora de parar: os fios do cano dormem em `esperar` até 500 ms, e sem
    /// este empurrão o encerramento pagaria essa espera por fio.
    pub fn acordar(&self) {
        self.chegou.notify_all();
    }

    pub fn publicados(&self) -> (u64, u64) {
        (
            self.publicados_video.load(Ordering::Relaxed),
            self.publicados_placa.load(Ordering::Relaxed),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Fios do cano e da placa
// ---------------------------------------------------------------------------------------------

/// Mantém `leitores` contando **quem está com o cano aberto agora** — e desconta sozinho em
/// qualquer saída do fio, inclusive a do `return` de parada, que um `fetch_sub` à mão esqueceria.
struct Presenca(Arc<AtomicU64>);

impl Presenca {
    fn entrar(leitores: &Arc<AtomicU64>) -> Self {
        leitores.fetch_add(1, Ordering::Relaxed);
        Presenca(Arc::clone(leitores))
    }
}

impl Drop for Presenca {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `clientes` conta **quantas vezes** alguém abriu (acumulado, para o relatório); `leitores` conta
/// **quem está lendo agora** — e é este que diz ao laço de exibição se converter para NV12 serve
/// para alguma coisa.
pub fn subir_cano(
    dist: Arc<Distribuidor>,
    parar: Arc<AtomicBool>,
    clientes: Arc<AtomicU64>,
    leitores: Arc<AtomicU64>,
    escritos: Arc<AtomicU64>,
    custos: Arc<Mutex<Vec<f64>>>,
    alguem_abriu: Arc<AtomicBool>,
    cano_servido: String,
) -> Vec<std::thread::JoinHandle<()>> {
    // Quatro instâncias porque há mais de um cliente possível ao mesmo tempo: o Frame Server abre
    // uma, o processo do app consumidor pode abrir outra, e sobra folga para a troca de app sem
    // janela de "cano ocupado".
    (0..4)
        .map(|_| {
            let (dist, parar, clientes, leitores, escritos, custos, alguem_abriu) = (
                Arc::clone(&dist),
                Arc::clone(&parar),
                Arc::clone(&clientes),
                Arc::clone(&leitores),
                Arc::clone(&escritos),
                Arc::clone(&custos),
                Arc::clone(&alguem_abriu),
            );
            let cano_servido = cano_servido.clone();
            std::thread::spawn(move || {
                while !parar.load(Ordering::Relaxed) {
                    let servidor = match cano::Servidor::esperar_cliente(&cano_servido) {
                        Ok(s) => s,
                        Err(e) => {
                            crate::registro::linha(format!("cano: {e}"));
                            std::thread::sleep(Duration::from_millis(300));
                            continue;
                        }
                    };
                    if parar.load(Ordering::Relaxed) {
                        // Cliente de mentira, aberto por `soltar_o_cano` só para destravar o
                        // `ConnectNamedPipe`. Não conta como app que abriu a câmera.
                        return;
                    }
                    clientes.fetch_add(1, Ordering::Relaxed);
                    let _presente = Presenca::entrar(&leitores);
                    alguem_abriu.store(true, Ordering::Relaxed);
                    let mut vista = 0u64;
                    loop {
                        if parar.load(Ordering::Relaxed) {
                            return;
                        }
                        // **Esperar o quadro**, nunca ritmar.
                        let Some((buf, geracao, ts)) =
                            dist.esperar(vista, Duration::from_millis(500))
                        else {
                            continue;
                        };
                        vista = geracao;
                        match servidor.escrever(&buf, ts) {
                            Ok(us) => {
                                custos.lock().unwrap().push(us as f64);
                                escritos.fetch_add(1, Ordering::Relaxed);
                            }
                            // O cliente foi embora (o app fechou a câmera). Volta a esperar outro.
                            Err(_) => break,
                        }
                    }
                }
            })
        })
        .collect()
}

/// Destrava os fios que estão parados em `ConnectNamedPipe`.
///
/// `ConnectNamedPipe` síncrono **não tem prazo**: um fio esperando um cliente que nunca vem fica
/// lá para sempre, e um `join` nele pendura o processo. Abrir a ponta cliente e fechar em
/// seguida devolve o fio ao laço, onde ele vê a bandeira de parada e sai. É o mesmo truque que a
/// casca Android usa para destravar o `quall_host` (dívida 10) — e, como lá, o fato de ser
/// preciso já é o sintoma: a espera devia ser cancelável.
pub fn soltar_o_cano(quantos: usize, cano_servido: &str) {
    for _ in 0..quantos {
        // Falha é o caso normal quando não há mais instância esperando; não é erro.
        let _ = std::fs::OpenOptions::new().read(true).open(cano_servido);
    }
}

pub fn subir_placa(
    dist: Arc<Distribuidor>,
    parar: Arc<AtomicBool>,
    estado: Arc<Mutex<Estado>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut placa: Option<Placa> = None;
        let mut tique = 0u64;
        while !parar.load(Ordering::Relaxed) {
            let alvo = *estado.lock().unwrap();
            // `serve_para`: o estado, e o idioma com que a placa foi desenhada (a troca PT | EN).
            if !placa.as_ref().is_some_and(|p| p.serve_para(alvo)) {
                match Placa::nova(alvo) {
                    Ok(p) => placa = Some(p),
                    Err(e) => {
                        crate::registro::linha(format!("placa de espera: {e:#}"));
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }
            if let Some(p) = &placa {
                if !dist.ha_video_recente() {
                    dist.publicar(Arc::new(p.quadro(tique)), cano::qpc_us(), false);
                }
            }
            tique += 1;
            // 30 fps, o mesmo que a câmera **anuncia**. Anunciar 30 e entregar 15 é a mesma
            // família de defeito que anunciar faixa completa e entregar limitada: o app do outro
            // lado acredita no anúncio.
            std::thread::sleep(Duration::from_millis(33));
        }
    })
}

// ---------------------------------------------------------------------------------------------
// A câmera virtual
// ---------------------------------------------------------------------------------------------

pub struct CameraVirtual {
    cam: IMFVirtualCamera,
    pub nome: String,
}

impl CameraVirtual {
    /// Cria o nó e o liga, **com uma segunda tentativa quando um nó do mesmo nome ficou para
    /// trás**.
    ///
    /// # O defeito que esta segunda tentativa conserta, medido em 09/09/2026
    ///
    /// Bruno pareou o S24 e relatou: *"pareado s24 no dell não apareceu como camera virtual"*, e
    /// depois: *"tive que fechar e abrir ai apareceu"*. O registro do app diz por quê:
    ///
    /// ```text
    /// camera virtual "SM-S928B": não subiu — IMFVirtualCamera::Start:
    ///     A solicitação é inválida no estado atual. (0xC00D36B2)
    /// ```
    ///
    /// `MF_E_INVALIDREQUEST`. Um nó com **o mesmo nome** já existia — sobra de uma execução
    /// anterior — e o `Start` recusa. E o detalhe que fecha o mecanismo: **a tentativa que falha
    /// limpa a sobra**. O `MFCreateVirtualCamera` já tinha se prendido àquele nó, e o nosso objeto
    /// morre com vida de sessão ao sair do `?`, levando o nó junto. Por isso a abertura **seguinte**
    /// funcionava — era a segunda tentativa, feita pela mão do usuário, um dia depois.
    ///
    /// Aqui ela é imediata.
    pub fn criar(nome: &str) -> Result<Self> {
        let mut ultimo = match Self::tentar(nome) {
            Ok(c) => return Ok(c),
            Err(e) if e.code() == MF_E_INVALIDREQUEST => e,
            Err(e) => return Err(anyhow::Error::from(e).context("MFCreateVirtualCamera")),
        };
        // **A segunda tentativa imediata não resolve, e isto está medido.** A primeira versão deste
        // conserto (§8.62) supunha que a tentativa recusada levava a sobra embora na hora, porque
        // era o que explicava o "fechar e abrir funciona" do usuário. Em 09/09/2026, no campo, o
        // registro mostrou as duas seguidas falhando com o mesmo `0xC00D36B2` e o app subindo com
        // **zero** câmeras. O que fazia o "fechar e abrir" funcionar era o **tempo**, não a segunda
        // chamada: o nó de vida de sessão é retirado pelo sistema algum tempo depois de o processo
        // que o criou morrer.
        //
        // Então espera-se por ele, em vez de supor. As esperas são curtas e somam ~3,5 s no pior
        // caso: é o que custa a câmera de um aparelho aparecer, uma vez, contra ela **não**
        // aparecer.
        for espera_ms in [200u64, 400, 800, 1000, 1000] {
            std::thread::sleep(std::time::Duration::from_millis(espera_ms));
            match Self::tentar(nome) {
                Ok(c) => {
                    crate::registro::linha(format!(
                        "camera virtual: o nó da execução anterior saiu; subiu na \
                         tentativa depois de esperar"
                    ));
                    return Ok(c);
                }
                Err(e) => ultimo = e,
            }
        }
        Err(anyhow::Error::from(ultimo).context(
            "MFCreateVirtualCamera: um nó do mesmo nome de uma execução anterior não saiu a tempo",
        ))
    }

    /// **`Start` exige `IMFActivate`** — sem ela dá `E_NOINTERFACE`, câmera criada e morta; a nossa
    /// fonte implementa a interface, e é por isso que ela existe lá.
    fn tentar(nome: &str) -> windows::core::Result<Self> {
        let cam: IMFVirtualCamera = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                // Vida de **sessão**: some quando este processo morre, medido em §8.58 inclusive
                // com morte à força. É o que impede uma câmera de aparelho esquecido de ficar na
                // lista do Zoom para sempre — e o que dispensa o `Remove()`, que não remove.
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &HSTRING::from(nome),
                &HSTRING::from(quall_camera_fonte::CLSID_TEXTO),
                None,
            )
        }?;
        unsafe { cam.Start(None) }?;
        Ok(CameraVirtual { cam, nome: nome.to_string() })
    }
}

impl Drop for CameraVirtual {
    fn drop(&mut self) {
        // `Stop` e `Shutdown`, nesta ordem, e **sem `Remove`**. Ver o cabeçalho do módulo.
        unsafe {
            let _ = self.cam.Stop();
            let _ = self.cam.Shutdown();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A baia
// ---------------------------------------------------------------------------------------------

pub struct Baia {
    pub nome: String,
    pub cano_servido: String,
    dist: Arc<Distribuidor>,
    parar: Arc<AtomicBool>,
    estado_placa: Arc<Mutex<Estado>>,
    alguem_abriu: Arc<AtomicBool>,
    clientes: Arc<AtomicU64>,
    leitores: Arc<AtomicU64>,
    escritos: Arc<AtomicU64>,
    custos: Arc<Mutex<Vec<f64>>>,
    fios: Vec<std::thread::JoinHandle<()>>,
    fio_da_placa: Option<std::thread::JoinHandle<()>>,
}

// **A câmera não mora aqui, e o compilador é quem decidiu.** `IMFVirtualCamera` é um ponteiro COM
// e não é `Send`/`Sync`; a `Baia` é compartilhada com a thread da sessão, que publica quadros
// nela. Guardar as duas coisas juntas obrigaria a marcar um ponteiro COM como seguro para
// atravessar thread — afirmação que ninguém aqui mediu e que depende do apartamento em que o
// objeto nasceu.
//
// Então o objeto da câmera fica com **quem a criou** (a thread principal, em `bin/quall_app.rs`),
// e a `Baia` — cano, placa e distribuidor, tudo Rust puro — é a parte que viaja.

impl Baia {
    /// Sobe o cano e a placa desta câmera. **Não cria a câmera** — ver a nota acima.
    pub fn abrir(nome: &str) -> Result<Self> {
        let cano_servido = cano::cano_do_nome(nome);
        let dist = Distribuidor::novo();
        let parar = Arc::new(AtomicBool::new(false));
        let estado_placa = Arc::new(Mutex::new(Estado::SemAparelho));
        let alguem_abriu = Arc::new(AtomicBool::new(false));
        let clientes = Arc::new(AtomicU64::new(0));
        let leitores = Arc::new(AtomicU64::new(0));
        let escritos = Arc::new(AtomicU64::new(0));
        let custos: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
        let fios = subir_cano(
            Arc::clone(&dist),
            Arc::clone(&parar),
            Arc::clone(&clientes),
            Arc::clone(&leitores),
            Arc::clone(&escritos),
            Arc::clone(&custos),
            Arc::clone(&alguem_abriu),
            cano_servido.clone(),
        );
        let fio_da_placa = subir_placa(
            Arc::clone(&dist),
            Arc::clone(&parar),
            Arc::clone(&estado_placa),
        );
        crate::registro::linha("camera virtual: de pé, servindo o cano");
        Ok(Baia {
            nome: nome.to_string(),
            cano_servido,
            dist,
            parar,
            estado_placa,
            alguem_abriu,
            clientes,
            leitores,
            escritos,
            custos,
            fios,
            fio_da_placa: Some(fio_da_placa),
        })
    }

    /// Publica um quadro de **vídeo** (já em 1920x1080 NV12) para quem estiver consumindo.
    pub fn publicar(&self, bytes: Arc<Vec<u8>>) {
        self.dist.publicar(bytes, cano::qpc_us(), true);
    }

    /// Muda a frase da placa de espera.
    pub fn dizer(&self, estado: Estado) {
        *self.estado_placa.lock().unwrap() = estado;
    }

    /// **Borda, não nível**: devolve `true` uma vez por abertura de câmera, e é assim que o
    /// pedido de IDR sai uma vez em vez de a cada volta do laço. Consultar como nível era um dos
    /// achados da revisão desta frente.
    pub fn tomar_abertura(&self) -> bool {
        self.alguem_abriu.swap(false, Ordering::Relaxed)
    }

    /// **Há alguém com a câmera aberta agora?**
    ///
    /// A fonte, dentro do Frame Server, abre o cano quando é ativada por um app e o solta quando o
    /// sistema a desliga. Sem leitor, cada quadro convertido para NV12 é uma escala na GPU e uma
    /// leitura de 3,1 MB de volta para a CPU que ninguém vai ver — e esse trabalho estava **dentro**
    /// do laço que apresenta na janela, em toda sessão, desde que a câmera por aparelho nasceu.
    pub fn alguem_lendo(&self) -> bool {
        self.leitores.load(Ordering::Relaxed) > 0
    }

    /// (clientes que abriram, quadros escritos, publicados de vídeo, publicados de placa)
    pub fn contadores(&self) -> (u64, u64, u64, u64) {
        let (v, p) = self.dist.publicados();
        (
            self.clientes.load(Ordering::Relaxed),
            self.escritos.load(Ordering::Relaxed),
            v,
            p,
        )
    }

    /// Custo de escrita no cano, em µs, para o relatório de quem quiser.
    pub fn custos_de_escrita(&self) -> Vec<f64> {
        self.custos.lock().unwrap().clone()
    }
}

impl Drop for Baia {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        soltar_o_cano(self.fios.len(), &self.cano_servido);
        for f in self.fios.drain(..) {
            let _ = f.join();
        }
        if let Some(f) = self.fio_da_placa.take() {
            let _ = f.join();
        }
        crate::registro::linha("cano da camera: encerrado");
    }
}
