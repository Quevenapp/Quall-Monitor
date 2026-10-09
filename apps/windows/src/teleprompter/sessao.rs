//! **A thread da sessão do teleprompter, nos dois papéis** — hospedar ou conectar, bombear,
//! vigiar a queda, e a ordem do fim do contrato (`docs/contrato-teleprompter.md` §2 e §6).
//!
//! # Quem é dono de quê
//!
//! O `Ready` do núcleo (sessão, link, atendente) vive **só** nesta thread: é a regra do mutex
//! global da libdatachannel no Windows (`docs/divida-do-nucleo.md`) e a regra de `janela.rs`. A
//! janela nunca toca na sessão. Ela edita a **réplica** (`quall_core::teleprompter::Teleprompter`,
//! que é `Sync` e manda na hora, da thread de quem edita) e lê o [`Painel`] que esta thread
//! publica. Os bits do que mudou por causa do outro lado vão por um atômico, e a janela é acordada
//! por quem a criou ([`Ambiente::acordar`]).
//!
//! # Portável de propósito
//!
//! Nada aqui é Win32: a identidade, o registro e o "acordar a janela" chegam por [`Ambiente`]. É o
//! que deixa esta thread ser provada **no Mac**, com sessões de verdade por 127.0.0.1, antes de o
//! Dell compilar (os testes do fim deste arquivo).
//!
//! # A ordem do fim (§6)
//!
//! 1. a bombeada que descobre o fim já vem com as mudanças — aplicadas; se o fim chegou pelo
//!    evento da sessão, **uma bombeada final com prazo zero**, aplicada;
//! 2. só então `perdeu_o_par`;
//! 3. a sessão velha cai (`drop` do `Ready`: fecha o link, a sessão e o atendente);
//! 4. no prompter, a espera volta **no mesmo servidor** — a mesma porta — com o mesmo PIN depois
//!    de uma queda e PIN novo depois de `WRONG_PIN`/`PAIRING`; no controle, conectar de novo a cada
//!    ~1 s pelo par conhecido, sem PIN.
//!
//! **O servidor fica aberto a vida inteira da tela.** A fronteira C abre um servidor por
//! `quall_host_with_role` e o contrato manda fechar a sessão antes de hospedar de novo porque é o
//! fechamento que solta a porta. Aqui, que o Windows fala Rust direto, o servidor é guardado
//! (`Arc<SignalingServer>`) e reusado: a porta nunca fica livre para outro processo pegar no meio
//! da troca, e o `bind` não depende de o sistema já ter soltado a porta (no Windows o `TcpListener`
//! não liga `SO_REUSEADDR`). O atendente da sessão velha é parado e esperado no `drop` do `Ready`
//! antes de a espera nova começar — nunca há dois `accept` no mesmo servidor.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use quall_core::cancel::Cancelamento;
use quall_core::discovery::{anuncio, endereco_manual, Advertiser, ESPERA_PELA_PORTA_DO_TELEPROMPTER};
use quall_core::pairing::{PairedPeers, Pin};
use quall_core::protocol::{Announcement, Capabilities, Papel};
use quall_core::session::{conectar, hospedar, EventoDeSessao, Ready, SessionConfig};
use quall_core::signaling::SignalingServer;
use quall_core::teleprompter::{mudou, resumo, Mudancas, Teleprompter};
use quall_core::transport::{ContadoresDeMensagens, TransportConfig};

use super::regras::{self, Codigo, Decisao, EscolhaDoPin, Falha, Lado, Ligacao};

/// 80 ms: dentro dos 50–100 que o contrato recomenda, e no máximo 250.
pub const PRAZO_DA_BOMBEADA: Duration = Duration::from_millis(80);
/// Uma espera do prompter: longa, e renovada com o mesmo PIN quando vence sem erro de PIN (§6).
pub const PRAZO_DA_ESPERA: Duration = Duration::from_secs(5 * 60);
/// Uma tentativa do controle: TCP, pareamento e o canal subir.
pub const PRAZO_DA_CONEXAO: Duration = Duration::from_secs(10);

/// O que a thread da sessão precisa do mundo, sem saber que mundo é esse.
pub struct Ambiente {
    pub device_id: String,
    pub nome: String,
    /// Os pares conhecidos, **relidos a cada tentativa**: o controle que pareou numa sessão volta
    /// pelo par gravado.
    pub pares: Box<dyn Fn() -> PairedPeers + Send + Sync>,
    /// Grava o pareamento que acabou de fechar, **antes** de qualquer outra coisa.
    pub guardar_pares: Box<dyn Fn(&PairedPeers) + Send + Sync>,
    pub registrar: Box<dyn Fn(&str) + Send + Sync>,
    /// Acorda a janela (no Windows, um `PostMessageW`). Pode ser chamado de qualquer thread.
    pub acordar: Box<dyn Fn() + Send + Sync>,
    /// O endereço desta máquina na LAN, para a tela do prompter.
    pub ip_local: Box<dyn Fn() -> Option<std::net::IpAddr> + Send + Sync>,
    /// Habilita instrumentos de bancada; PINs nunca vão para o diagnóstico.
    pub bancada: bool,
}

/// Em que pé está a ligação, para a tela.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fase {
    /// Controle: o laço acabou (a pessoa desconectou, ou uma falha que precisa dela). Prompter: a
    /// espera parou (cinco PINs errados, porta que não abre) — a tela oferece "Esperar de novo".
    Parada,
    /// Prompter: esperando o controle, com o PIN na tela. Controle: conectando.
    Abrindo,
    Conectada,
    /// Houve sessão e ela caiu: o prompter espera a volta (rolando como estava); o controle tenta
    /// de novo.
    SemPar,
    Encerrando,
}

/// O que a janela lê. Publicado por esta thread, sob trava, com uma versão.
#[derive(Debug, Clone)]
pub struct Painel {
    pub fase: Fase,
    pub pin: String,
    pub porta: u16,
    /// Prompter: o `ip:porta` para digitar no controle. Controle: para onde está discando.
    pub endereco: Option<String>,
    pub par: String,
    pub mensagem: String,
    pub anunciando: bool,
    pub tentativas: u32,
    pub ja_houve_sessao: bool,
    pub ligacao: Ligacao,
    pub versao: u64,
}

impl Painel {
    fn novo() -> Painel {
        Painel {
            fase: Fase::Abrindo,
            pin: String::new(),
            porta: 0,
            endereco: None,
            par: String::new(),
            mensagem: String::new(),
            anunciando: false,
            tentativas: 0,
            ja_houve_sessao: false,
            ligacao: Ligacao::SemSessao,
            versao: 1,
        }
    }
}

/// O que se mede para o relato de bancada.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Medidas {
    pub sessoes_de_pe: u32,
    pub quedas: u32,
    pub tentativas: u32,
    pub falhas: Vec<String>,
    pub pins_trocados: u32,
    pub ocupados_ouvidos: u32,
    /// Quanto cada edição daqui levou para voltar confirmada (o estado do outro lado mostrando o
    /// carimbo dela), em ms — a ida e volta que o contrato promete em §3.
    pub confirmacoes_ms: Vec<f64>,
    pub eventos: Vec<String>,
    pub mensageiro: Option<ContadoresDeMensagens>,
    pub entrega: Option<String>,
    pub atendidos_durante_a_sessao: u32,
    pub candidatos_descartados: u32,
}

const TETO_DE_EVENTOS: usize = 400;

/// **Uma sessão da tela**: a thread dela e o que ela publica. A janela segura um `Arc` disto e
/// só lê, pede parada e anota o começo de uma edição.
pub struct Sessao {
    pub lado: Lado,
    pub teleprompter: Arc<Teleprompter>,
    ambiente: Arc<Ambiente>,
    painel: Mutex<Painel>,
    parar: AtomicBool,
    terminou: AtomicBool,
    cancelamento: Mutex<Option<Cancelamento>>,
    mudancas: AtomicU32,
    saltos_vistos: AtomicU64,
    /// Desde quando há uma edição daqui esperando confirmação (para medir, não para decidir).
    confirmacao_desde: Mutex<Option<Instant>>,
    medidas: Mutex<Medidas>,
    origem: Instant,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// O que o prompter precisa para abrir.
#[derive(Debug, Clone)]
pub struct ConfigDoPrompter {
    /// A porta pedida: [`regras::porta_do_teleprompter`], ou a da bancada.
    pub porta: u16,
    /// PIN dado pela bancada; `None` sorteia. Erro de PIN troca de qualquer jeito.
    pub pin: Option<String>,
    pub anunciar: bool,
}

/// O que o controle precisa para conectar.
#[derive(Debug, Clone)]
pub struct ConfigDoControle {
    pub destino: regras::Destino,
    /// PIN digitado (vale só para a primeira entrada; o do link vence).
    pub pin: Option<String>,
}

impl Sessao {
    fn nova(lado: Lado, teleprompter: Arc<Teleprompter>, ambiente: Arc<Ambiente>) -> Arc<Sessao> {
        Arc::new(Sessao {
            lado,
            teleprompter,
            ambiente,
            painel: Mutex::new(Painel::novo()),
            parar: AtomicBool::new(false),
            terminou: AtomicBool::new(false),
            cancelamento: Mutex::new(None),
            mudancas: AtomicU32::new(0),
            saltos_vistos: AtomicU64::new(0),
            confirmacao_desde: Mutex::new(None),
            medidas: Mutex::new(Medidas::default()),
            origem: Instant::now(),
            thread: Mutex::new(None),
        })
    }

    /// O prompter: abre a porta, anuncia, e espera o controle em laço.
    pub fn iniciar_prompter(t: Arc<Teleprompter>, amb: Arc<Ambiente>, cfg: ConfigDoPrompter) -> Arc<Sessao> {
        let s = Sessao::nova(Lado::Prompter, t, amb);
        let eu = Arc::clone(&s);
        let h = std::thread::Builder::new()
            .name("quall.teleprompter.sessao".into())
            .spawn(move || eu.correr_prompter(cfg))
            .ok();
        *trava(&s.thread) = h;
        s
    }

    /// O controle: conecta no destino e, depois de uma queda, tenta de novo até entrar.
    pub fn iniciar_controle(t: Arc<Teleprompter>, amb: Arc<Ambiente>, cfg: ConfigDoControle) -> Arc<Sessao> {
        let s = Sessao::nova(Lado::Controle, t, amb);
        let eu = Arc::clone(&s);
        let h = std::thread::Builder::new()
            .name("quall.teleprompter.sessao".into())
            .spawn(move || eu.correr_controle(cfg))
            .ok();
        *trava(&s.thread) = h;
        s
    }

    // -----------------------------------------------------------------------------------------
    // O que a janela usa
    // -----------------------------------------------------------------------------------------

    pub fn painel(&self) -> MutexGuard<'_, Painel> {
        trava(&self.painel)
    }

    /// Os bits do que mudou por causa do outro lado desde a última leitura.
    pub fn tirar_mudancas(&self) -> Mudancas {
        self.mudancas.swap(0, Ordering::SeqCst)
    }

    /// Quantos saltos esta thread já viu chegar. **Contado antes de acordar a janela**: a vista
    /// não relata posição enquanto não tiver aplicado todos (o relato velho passaria na frente do
    /// salto e o `saltar_relativo` seguinte do controle partiria dele — o defeito 3 da revisão de
    /// 13/09, que o Mac evita do mesmo jeito).
    pub fn saltos_vistos(&self) -> u64 {
        self.saltos_vistos.load(Ordering::SeqCst)
    }

    /// Pede a parada: a espera é cancelada, e a sessão de pé para na volta seguinte (≤ 80 ms),
    /// **pela ordem do fim** — a bombeada final entra antes de fechar.
    pub fn pedir_parada(&self) {
        self.parar.store(true, Ordering::SeqCst);
        if let Some(c) = trava(&self.cancelamento).as_ref() {
            c.cancelar();
        }
        let mut p = self.painel();
        if p.fase != Fase::Parada {
            p.fase = Fase::Encerrando;
            p.versao += 1;
        }
    }

    pub fn terminou(&self) -> bool {
        self.terminou.load(Ordering::SeqCst)
    }

    /// Espera a thread acabar, até `prazo`. Devolve se acabou.
    pub fn esperar(&self, prazo: Duration) -> bool {
        let fim = Instant::now() + prazo;
        while !self.terminou() {
            if Instant::now() >= fim {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(h) = trava(&self.thread).take() {
            let _ = h.join();
        }
        true
    }

    /// A janela vai editar: anota o começo, para medir a confirmação. Só com a sessão de pé —
    /// uma edição feita com o prompter fora mede a queda, não a ida e volta.
    pub fn antes_de_editar(&self) {
        if self.painel().fase != Fase::Conectada {
            return;
        }
        let mut desde = trava(&self.confirmacao_desde);
        if desde.is_none() {
            *desde = Some(Instant::now());
        }
    }

    /// Depois da edição: se nada ficou pendente (o valor era o mesmo), não há o que medir.
    pub fn depois_de_editar(&self) {
        let pendente = self
            .teleprompter
            .estado()
            .map(|e| e.sem_confirmacao_ha_ms.is_some())
            .unwrap_or(false);
        if !pendente {
            // Pode ser que a confirmação já tenha voltado (127.0.0.1): quem mede é a bombeada.
            let mut desde = trava(&self.confirmacao_desde);
            if let Some(t0) = desde.take() {
                self.anotar_confirmacao(t0.elapsed());
            }
        }
    }

    pub fn medidas(&self) -> Medidas {
        trava(&self.medidas).clone()
    }

    pub fn segundos(&self) -> f64 {
        self.origem.elapsed().as_secs_f64()
    }

    // -----------------------------------------------------------------------------------------
    // Por dentro
    // -----------------------------------------------------------------------------------------

    fn registrar(&self, linha: &str) {
        (self.ambiente.registrar)(&crate::higiene_do_registro::sanitizar(&format!("teleprompter: {linha}")));
    }

    fn evento(&self, texto: &str) {
        let t = self.segundos();
        let mut m = trava(&self.medidas);
        if m.eventos.len() < TETO_DE_EVENTOS {
            m.eventos.push(crate::higiene_do_registro::sanitizar(&format!("{t:.3}s {texto}")));
        }
    }

    fn publicar(&self, f: impl FnOnce(&mut Painel)) {
        {
            let mut p = self.painel();
            f(&mut p);
            p.versao += 1;
        }
        (self.ambiente.acordar)();
    }

    fn parando(&self) -> bool {
        self.parar.load(Ordering::SeqCst)
    }

    fn anotar_confirmacao(&self, d: Duration) {
        let mut m = trava(&self.medidas);
        if m.confirmacoes_ms.len() < 10_000 {
            m.confirmacoes_ms.push((d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0);
        }
    }

    /// Os bits que chegaram: contados, registrados e entregues à janela.
    fn aplicar(&self, m: Mudancas) {
        if m == 0 {
            return;
        }
        if m & mudou::SALTO != 0 && self.lado == Lado::Prompter {
            self.saltos_vistos.fetch_add(1, Ordering::SeqCst);
        }
        // O relato de posição chega a 4 Hz: fica fora do registro.
        if m & !(mudou::POSICAO) != 0 {
            let mut linha = format!("mudou [{}]", nomes(m));
            if m & mudou::TEXTO != 0 {
                if let Ok(t) = self.teleprompter.texto() {
                    linha.push_str(&format!(" texto={} bytes resumo={}", t.len(), resumo(&t)));
                }
            }
            if let Ok(e) = self.teleprompter.estado() {
                linha.push_str(&format!(
                    " | rolando={} para_tras={} segurando={} velocidade={} fonte={} margem={} linha={} espelho={} \
                     posicao={} salto={} par_visto_ha_ms={} par_entende_segurar={}",
                    e.rolando,
                    e.para_tras,
                    e.segurando,
                    e.velocidade,
                    e.fonte,
                    e.margem,
                    e.linha_de_leitura,
                    e.espelho,
                    e.posicao,
                    e.salto.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
                    e.par_visto_ha_ms.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
                    e.par_entende_segurar,
                ));
            }
            self.registrar(&linha);
        }
        self.mudancas.fetch_or(m, Ordering::SeqCst);
        (self.ambiente.acordar)();
    }

    /// A confirmação voltou? (Só mede; a regra do aviso é da tela.)
    fn medir_confirmacao(&self) {
        let pendente = self
            .teleprompter
            .estado()
            .map(|e| e.sem_confirmacao_ha_ms.is_some())
            .unwrap_or(true);
        if !pendente {
            let mut desde = trava(&self.confirmacao_desde);
            if let Some(t0) = desde.take() {
                drop(desde);
                self.anotar_confirmacao(t0.elapsed());
            }
        }
    }

    fn anuncio(&self, papel: Papel) -> Announcement {
        let mut eu = anuncio(
            &self.ambiente.device_id,
            &self.ambiente.nome,
            // Um teleprompter não emite nem recebe vídeo: sem capacidade nenhuma, e é assim que
            // as listas de vídeo o escondem (contrato §2).
            Capabilities { screen_source: false, camera_source: false, sink: false },
        );
        eu.papel = Some(papel);
        eu
    }

    fn novo_cancelamento(&self) -> Cancelamento {
        let c = Cancelamento::novo();
        if self.parando() {
            c.cancelar();
        }
        *trava(&self.cancelamento) = Some(c.clone());
        c
    }

    fn marcar_fim(&self) {
        let _ = self.teleprompter.estado();
        self.terminou.store(true, Ordering::SeqCst);
        (self.ambiente.acordar)();
    }

    /// Bombeia e vigia até a sessão acabar. Devolve por quê. **A ordem do fim** (§6) mora aqui:
    /// as mudanças da bombeada que fecha entram; pelo evento, uma bombeada final com prazo zero; e
    /// só depois `perdeu_o_par`.
    fn conduzir(&self, pronto: &mut Ready) -> String {
        let m = pronto.session.mensageiro();
        {
            let mut md = trava(&self.medidas);
            md.entrega = pronto.session.entrega_do_canal().map(|d| format!("{d:?}"));
        }
        let porque = loop {
            if self.parando() {
                // A pessoa parou com a sessão de pé: o que já chegou entra antes de fechar.
                if let Ok(b) = self.teleprompter.bombear(&m, Duration::ZERO) {
                    self.aplicar(b.mudancas);
                }
                break "parada".to_string();
            }
            let b = match self.teleprompter.bombear(&m, PRAZO_DA_BOMBEADA) {
                Ok(b) => b,
                Err(e) => break format!("a bombeada falhou: {e}"), // i18n: fora (diário)
            };
            // **Aplicar antes de qualquer outra coisa**: com a sessão fechada, as mudanças trazem
            // a última mensagem do outro lado (a pausa tocada logo antes da queda).
            self.aplicar(b.mudancas);
            self.medir_confirmacao();
            if b.fechada {
                break "a bombeada viu o canal fechar (CLOSED)".to_string(); // i18n: fora (diário)
            }
            let evento = pronto.proximo_evento(Duration::ZERO);
            if evento != EventoDeSessao::Nenhum {
                // A queda chegou pelo evento: a bombeada final lê a fila até o fim antes de o par
                // ser dado por perdido.
                if let Ok(fim) = self.teleprompter.bombear(&m, Duration::ZERO) {
                    self.aplicar(fim.mudancas);
                }
                break match evento {
                    EventoDeSessao::Desconectou => "o outro lado saiu (DISCONNECTED)".to_string(), // i18n: fora (diário)
                    _ => "a sessão falhou (FAILED)".to_string(), // i18n: fora (diário)
                };
            }
        };
        match self.teleprompter.perdeu_o_par() {
            Ok(mud) => self.aplicar(mud | mudou::PAR),
            Err(_) => self.registrar("!! perdeu_o_par falhou: classe=TELEPROMPTER"),
        }
        {
            let mut md = trava(&self.medidas);
            md.mensageiro = Some(m.contadores());
            md.atendidos_durante_a_sessao += pronto.atendidos_durante_a_sessao();
        }
        *trava(&self.confirmacao_desde) = None;
        porque
    }

    /// Grava o pareamento que acabou de fechar e registra quem entrou.
    fn subiu(&self, pronto: &Ready) {
        let mut novos = PairedPeers::new();
        novos.insert(&pronto.outcome);
        (self.ambiente.guardar_pares)(&novos);
        let par = if pronto.peer.display_name.is_empty() {
            "outro aparelho".to_string() // i18n: chave (o nome do par; a tela traduz ao mostrar)
        } else {
            pronto.peer.display_name.clone()
        };
        let caminho = pronto.session.caminho();
        let linha = format!(
            // i18n: fora (diário)
            "sessão de pé papel={} pareamento={} descartados={} entrega={:?} \
             caminho={} <-> {}",
            pronto.peer.papel.map(|p| p.como_texto()).unwrap_or("-"),
            if pronto.outcome.novo { "novo (PIN)" } else { "retomado" },
            pronto.descartados,
            pronto.session.entrega_do_canal(),
            caminho.local_address.as_deref().unwrap_or("?"),
            caminho.remote_address.as_deref().unwrap_or("?"),
        );
        self.registrar(&linha);
        self.evento(&format!("conectou pareamento={}", if pronto.outcome.novo { "novo" } else { "retomado" }));
        {
            let mut md = trava(&self.medidas);
            md.sessoes_de_pe += 1;
            md.candidatos_descartados += pronto.descartados;
        }
        let desde_s = self.segundos();
        self.publicar(|p| {
            p.ligacao = Ligacao::Conectada { desde_s, depois_de_queda: p.ja_houve_sessao };
            p.ja_houve_sessao = true;
            p.fase = Fase::Conectada;
            p.par = par;
            p.mensagem.clear();
            p.tentativas = 0;
        });
    }

    fn caiu(&self, porque: &str) {
        let estado = self
            .teleprompter
            .estado()
            .map(|e| serde_json::to_string(&e).unwrap_or_default())
            .unwrap_or_default();
        self.registrar(&format!(
            "a sessão acabou: {porque} — ordem do fim cumprida (bombeada final, perdeu_o_par, fechamento) | {estado}" // i18n: fora (diário)
        ));
        self.evento(&format!("caiu: {porque}"));
        trava(&self.medidas).quedas += 1;
        let parando = self.parando();
        self.publicar(|p| {
            p.ligacao = Ligacao::Caiu;
            if !parando {
                p.fase = Fase::SemPar;
            }
        });
    }

    fn anotar_falha(&self, codigo: Codigo, _motivo: &str, decisao: Decisao) {
        let t = self.segundos();
        let mut md = trava(&self.medidas);
        md.tentativas += 1;
        if codigo == Codigo::Ocupado {
            md.ocupados_ouvidos += 1;
        }
        if md.falhas.len() < 200 {
            md.falhas.push(format!("{t:.3}s {} {decisao:?}", codigo.nome()));
        }
    }

    // -----------------------------------------------------------------------------------------
    // O prompter
    // -----------------------------------------------------------------------------------------

    fn correr_prompter(self: Arc<Self>, cfg: ConfigDoPrompter) {
        let eu = self.anuncio(Papel::Teleprompter);

        // **A porta**: a pedida, ou a próxima que abrir, **escolhida uma vez, ao abrir a tela**, e o
        // servidor aberto a vida inteira dela (§11.7 do contrato). O `bind` é o teste — sem a
        // corrida entre "testei" e "abri". **Pela pedida, espera até 2 s**
        // (`ESPERA_PELA_PORTA_DO_TELEPROMPTER`, a mesma do `escolher_porta_do_teleprompter` do
        // núcleo): numa recriação da tela a sessão velha ainda a segura por uma bombeada, e sem a
        // espera a tela nova iria para a 7980 e o controle que caiu não a acharia mais.
        let mut servidor = None;
        let mut ultimo = String::new();
        for (i, p) in regras::portas_candidatas(cfg.porta).into_iter().enumerate() {
            let espera = if i == 0 { ESPERA_PELA_PORTA_DO_TELEPROMPTER } else { Duration::ZERO };
            let fim = Instant::now() + espera;
            loop {
                match SignalingServer::bind(p) {
                    Ok(s) => {
                        servidor = Some((Arc::new(s), p));
                        break;
                    }
                    Err(e) => {
                        ultimo = e.to_string();
                        if Instant::now() >= fim || self.parando() {
                            self.registrar(&format!(
                                "porta {p} não abriu: {e}{}", // i18n: fora (diário)
                                if espera.is_zero() { String::new() } else { format!(" (esperei {} ms por ela)", espera.as_millis()) }
                            ));
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            if servidor.is_some() {
                break;
            }
        }
        let Some((servidor, porta)) = servidor else {
            // O último erro do sistema vai como veio, dentro da frase.
            let ate = cfg.porta.saturating_add(regras::PORTAS_A_TENTAR - 1);
            let msg = crate::idioma::tf("Nenhuma porta de {} a {} abriu neste computador ({}).", &[&cfg.porta, &ate, &ultimo]);
            self.registrar(&format!("!! {msg}"));
            self.publicar(|p| {
                p.fase = Fase::Parada;
                p.mensagem = msg;
            });
            self.marcar_fim();
            return;
        };
        if porta != cfg.porta {
            self.registrar(&format!("a porta {} está ocupada — hospedando na {porta} (a tela mostra)", cfg.porta)); // i18n: fora (diário)
        }
        let ip = (self.ambiente.ip_local)();
        let endereco = ip.map(|ip| crate::enderecos::com_porta(ip, porta));

        // O anúncio vive a tela inteira: a porta não muda entre sessões.
        let anunciante = if cfg.anunciar {
            match Advertiser::start(&eu, porta) {
                Ok(a) => Some(a),
                Err(e) => {
                    self.registrar(&format!("mdns: o anúncio não subiu (status={}) — o endereço digitado continua valendo", Codigo::de(&e).nome())); // i18n: fora (diário)
                    None
                }
            }
        } else {
            self.registrar("mdns: desligado (--sem-mdns)");
            None
        };
        let anunciando = anunciante.is_some();
        self.registrar(&format!(
            "mdns: anunciou={anunciando} porta={porta} papel=teleprompter endereco={}",
            endereco.as_deref().unwrap_or("sem rede") // i18n: fora (diário)
        ));

        let mut pin = match cfg.pin.as_deref().map(Pin::parse) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                self.registrar(&format!("!! o PIN da bancada não vale (status={}); sorteando", Codigo::de(&e).nome())); // i18n: fora (diário)
                sortear_pin()
            }
            None => sortear_pin(),
        };
        let mut tentativa = 0u32;
        let mut falhando_desde: Option<Instant> = None;
        let mut falhas_seguidas = 0u32;
        let mut erros_de_pin_seguidos = 0u32;

        while !self.parando() {
            tentativa += 1;
            let pin_texto = pin.to_display();
            {
                let endereco = endereco.clone();
                self.publicar(|p| {
                    p.fase = if p.ja_houve_sessao { Fase::SemPar } else { Fase::Abrindo };
                    p.pin = pin_texto.clone();
                    p.porta = porta;
                    p.endereco = endereco;
                    p.anunciando = anunciando;
                    p.tentativas = tentativa;
                });
            }
            self.registrar(&format!(
                "prompter: esperando o controle porta={porta} endereco_disponivel={} (tentativa {tentativa})", // i18n: fora (diário)
                endereco.is_some(),
            ));
            let cancelamento = self.novo_cancelamento();
            let inicio = Instant::now();
            let resultado = hospedar(
                &servidor,
                SessionConfig {
                    announcement: eu.clone(),
                    pin: Some(pin.clone()),
                    known: (self.ambiente.pares)(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: PRAZO_DA_ESPERA,
                    cancelamento,
                    // O núcleo liga o detector de 5 s pelo papel (`SILENCIO_DO_TELEPROMPTER`).
                    silencio_do_caminho: None,
                },
            );
            match resultado {
                Ok(mut pronto) => {
                    falhando_desde = None;
                    falhas_seguidas = 0;
                    erros_de_pin_seguidos = 0;
                    // O prompter atende a porta enquanto a sessão dura: todo controle que bate
                    // ouve "ocupado" — inclusive o da sessão, voltando de uma queda — e nada que
                    // chega pela porta derruba a sessão.
                    pronto.atender_enquanto_dura(Arc::clone(&servidor), eu.clone());
                    self.subiu(&pronto);
                    let porque = self.conduzir(&mut pronto);
                    // Passo 3: a sessão velha cai aqui — link, sessão e atendente (o `drop` do
                    // atendente espera a thread dele sair) — **antes** de a espera nova começar.
                    drop(pronto);
                    self.caiu(&porque);
                    // O mesmo PIN depois de uma queda de uma sessão que subiu (§2).
                }
                Err(e) => {
                    let codigo = Codigo::de(&e);
                    let desde = *falhando_desde.get_or_insert(inicio);
                    falhas_seguidas += 1;
                    if matches!(codigo, Codigo::PinErrado | Codigo::Pareamento) {
                        erros_de_pin_seguidos += 1;
                    }
                    let falha = Falha {
                        codigo,
                        ja_subiu: self.painel().ja_houve_sessao,
                        durou_ms: inicio.elapsed().as_millis() as u64,
                        falhando_ha_ms: desde.elapsed().as_millis() as u64,
                        falhas_seguidas,
                        erros_de_pin_seguidos,
                        parando: self.parando(),
                    };
                    let decisao = regras::decidir(Lado::Prompter, falha);
                    let motivo = e.to_string();
                    self.anotar_falha(codigo, &motivo, decisao);
                    if codigo != Codigo::Cancelado {
                        self.registrar(&format!(
                            "não abriu: status={} decisao={decisao:?}", // i18n: fora (diário)
                            codigo.nome()
                        ));
                    }
                    let conselho = regras::conselho(Lado::Prompter, &falha, decisao, porta, &motivo);
                    if !conselho.is_empty() {
                        self.publicar(|p| p.mensagem = conselho);
                    }
                    match decisao {
                        Decisao::Parar => {
                            let parando = self.parando();
                            self.publicar(|p| p.fase = if parando { Fase::Encerrando } else { Fase::Parada });
                            break;
                        }
                        Decisao::TentarDeNovo { pin: escolha, depois_ms } => {
                            if escolha == EscolhaDoPin::Novo {
                                pin = sortear_pin();
                                trava(&self.medidas).pins_trocados += 1;
                                self.evento(&format!("pin trocado depois de {}", codigo.nome())); // i18n: fora (diário)
                            }
                            self.dormir(depois_ms);
                        }
                    }
                }
            }
        }

        if let Some(a) = anunciante {
            let _ = a.stop();
        }
        drop(servidor);
        self.registrar("prompter: o laço terminou; a porta foi solta"); // i18n: fora (diário)
        let parando = self.parando();
        self.publicar(|p| {
            p.anunciando = false;
            if parando {
                p.fase = Fase::Parada;
            }
        });
        self.marcar_fim();
    }

    // -----------------------------------------------------------------------------------------
    // O controle
    // -----------------------------------------------------------------------------------------

    fn correr_controle(self: Arc<Self>, cfg: ConfigDoControle) {
        let eu = self.anuncio(Papel::ControleRemoto);
        let destino_texto = cfg.destino.endereco.clone();
        self.publicar(|p| {
            p.fase = Fase::Abrindo;
            p.endereco = Some(destino_texto.clone());
        });
        // O endereço já vem com porta (a regra da 7979 é da casca, em `regras::completar`): o
        // `endereco_manual` do núcleo só resolve o nome, e nunca completa com a 7877.
        let destino: SocketAddr = match endereco_manual(&destino_texto) {
            Ok(a) => a,
            Err(e) => {
                let msg = crate::idioma::tf("Não entendi o endereço {}: {}", &[&destino_texto, &e]);
                self.registrar(&format!("!! {msg}"));
                self.publicar(|p| {
                    p.fase = Fase::Parada;
                    p.mensagem = msg;
                });
                self.marcar_fim();
                return;
            }
        };
        let pin_texto = cfg.destino.pin.clone().or(cfg.pin.clone()).filter(|p| !p.trim().is_empty());
        let mut pin = match pin_texto.as_deref().map(|p| Pin::parse(p.trim())) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                let msg = crate::idioma::tf("O PIN tem de ter seis dígitos ({}).", &[&e]);
                self.publicar(|p| {
                    p.fase = Fase::Parada;
                    p.mensagem = msg;
                });
                self.marcar_fim();
                return;
            }
            None => None,
        };

        let mut tentativa = 0u32;
        let mut falhando_desde: Option<Instant> = None;
        let mut falhas_seguidas = 0u32;
        let mut erros_de_pin_seguidos = 0u32;
        while !self.parando() {
            tentativa += 1;
            self.publicar(|p| {
                p.fase = if p.ja_houve_sessao { Fase::SemPar } else { Fase::Abrindo };
                p.tentativas = tentativa;
            });
            if tentativa == 1 || tentativa % 10 == 0 || self.ambiente.bancada {
                self.registrar(&format!(
                    "controle: conectando em {destino} (tentativa {tentativa}) {}", // i18n: fora (diário)
                    if pin.is_some() { "com PIN" } else { "sem PIN (par conhecido)" } // i18n: fora (diário)
                ));
            }
            let cancelamento = self.novo_cancelamento();
            let inicio = Instant::now();
            let resultado = conectar(
                destino,
                SessionConfig {
                    announcement: eu.clone(),
                    pin: pin.clone(),
                    known: (self.ambiente.pares)(),
                    transport: TransportConfig::default(),
                    tracks: Vec::new(),
                    timeout: PRAZO_DA_CONEXAO,
                    cancelamento,
                    silencio_do_caminho: None,
                },
            );
            match resultado {
                Ok(mut pronto) => {
                    falhando_desde = None;
                    falhas_seguidas = 0;
                    erros_de_pin_seguidos = 0;
                    // Daqui em diante o par é conhecido: a volta entra sem PIN. Repetir o PIN
                    // digitado faria um pareamento novo a cada queda — e o prompter troca de PIN
                    // depois de um erro, então a volta passaria a falhar.
                    pin = None;
                    self.subiu(&pronto);
                    let porque = self.conduzir(&mut pronto);
                    drop(pronto);
                    self.caiu(&porque);
                }
                Err(e) => {
                    let codigo = Codigo::de(&e);
                    let desde = *falhando_desde.get_or_insert(inicio);
                    falhas_seguidas += 1;
                    if matches!(codigo, Codigo::PinErrado | Codigo::Pareamento) {
                        erros_de_pin_seguidos += 1;
                    }
                    let falha = Falha {
                        codigo,
                        ja_subiu: self.painel().ja_houve_sessao,
                        durou_ms: inicio.elapsed().as_millis() as u64,
                        falhando_ha_ms: desde.elapsed().as_millis() as u64,
                        falhas_seguidas,
                        erros_de_pin_seguidos,
                        parando: self.parando(),
                    };
                    let decisao = regras::decidir(Lado::Controle, falha);
                    let motivo = e.to_string();
                    self.anotar_falha(codigo, &motivo, decisao);
                    if codigo != Codigo::Cancelado && (falhas_seguidas <= 3 || falhas_seguidas % 10 == 0 || self.ambiente.bancada) {
                        self.registrar(&format!(
                            "não abriu: status={} decisao={decisao:?}", // i18n: fora (diário)
                            codigo.nome()
                        ));
                    }
                    let conselho = regras::conselho(Lado::Controle, &falha, decisao, 0, &motivo);
                    self.publicar(|p| p.mensagem = conselho);
                    match decisao {
                        Decisao::Parar => {
                            self.publicar(|p| p.fase = Fase::Parada);
                            break;
                        }
                        Decisao::TentarDeNovo { depois_ms, .. } => self.dormir(depois_ms),
                    }
                }
            }
        }
        self.registrar("controle: o laço terminou"); // i18n: fora (diário)
        let parando = self.parando();
        self.publicar(|p| {
            if parando {
                p.fase = Fase::Parada;
                p.mensagem.clear();
            }
        });
        self.marcar_fim();
    }

    /// Dorme até `ms`, acordando antes se a tela pedir para parar.
    fn dormir(&self, ms: u64) {
        let fim = Instant::now() + Duration::from_millis(ms);
        while !self.parando() && Instant::now() < fim {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn sortear_pin() -> Pin {
    // `Pin::generate` só falha sem fonte de aleatoriedade do sistema; aí não há PIN seguro a
    // oferecer, e um PIN fixo seria pior que parar — mas parar aqui deixaria a tela sem nada.
    // O núcleo nunca falhou nisto em nenhuma plataforma; o registro diz se falhar.
    Pin::generate().unwrap_or_else(|_| Pin::parse("000000").expect("seis dígitos"))
}

fn trava<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Os nomes dos bits, para o registro: um bit que acende aparece por extenso.
pub fn nomes(m: Mudancas) -> String {
    let tabela = [
        (mudou::TEXTO, "texto"),
        (mudou::ROLANDO, "rolando"),
        (mudou::VELOCIDADE, "velocidade"),
        (mudou::FONTE, "fonte"),
        (mudou::MARGEM, "margem"),
        (mudou::LINHA_DE_LEITURA, "linha"),
        (mudou::ESPELHO, "espelho"),
        (mudou::POSICAO, "posicao"),
        (mudou::SALTO, "salto"),
        (mudou::PAR, "par"),
        (mudou::PERGUNTA_DO_TEXTO, "pergunta"),
        (mudou::COPIA_DO_TEXTO, "copia"),
        (mudou::SEGURAR, "segurar"),
        (mudou::GRAVACAO, "gravacao"),
    ];
    let mut v: Vec<String> = tabela.iter().filter(|(b, _)| m & b != 0).map(|(_, n)| n.to_string()).collect();
    let conhecidos: u32 = tabela.iter().map(|(b, _)| b).sum();
    if m & !conhecidos != 0 {
        v.push(format!("desconhecido({})", m & !conhecidos));
    }
    v.join(" ")
}

#[cfg(test)]
mod testes {
    //! **A thread da sessão de ponta a ponta**, com sessões de verdade por 127.0.0.1: o prompter e
    //! o controle deste módulo, um contra o outro, pelo núcleo e pela libdatachannel. Rodam em
    //! qualquer plataforma que compile o núcleo com transporte (no Mac, pelo rascunho da frente;
    //! no Windows, pelo portão).

    use super::*;
    use quall_core::teleprompter::Estado;
    use std::sync::atomic::AtomicUsize;

    static PROXIMA_PORTA: AtomicUsize = AtomicUsize::new(0);

    /// Uma porta por teste, alta, para os testes rodarem juntos sem disputar.
    fn porta() -> u16 {
        let base = 21_000 + (std::process::id() as usize % 2_000) * 10;
        (base + PROXIMA_PORTA.fetch_add(12, Ordering::SeqCst)) as u16
    }

    fn ambiente(id: &str, pares: Arc<Mutex<PairedPeers>>) -> Arc<Ambiente> {
        let p1 = Arc::clone(&pares);
        let p2 = Arc::clone(&pares);
        let rotulo = id.to_string();
        Arc::new(Ambiente {
            device_id: id.to_string(),
            nome: format!("Teste {id}"),
            pares: Box::new(move || trava(&p1).clone()),
            guardar_pares: Box::new(move |novos| trava(&p2).merge(novos)),
            registrar: Box::new(move |l| eprintln!("[{rotulo}] {l}")),
            acordar: Box::new(|| {}),
            ip_local: Box::new(|| Some(std::net::IpAddr::from([127, 0, 0, 1]))),
            bancada: true,
        })
    }

    fn replica(id: &str, papel: Papel) -> Arc<Teleprompter> {
        Arc::new(Teleprompter::nova(id, papel).expect("réplica"))
    }

    /// Espera uma condição, até `prazo`.
    fn ate(prazo: Duration, mut f: impl FnMut() -> bool) -> bool {
        let fim = Instant::now() + prazo;
        while Instant::now() < fim {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    fn estado(t: &Teleprompter) -> Estado {
        t.estado().expect("estado")
    }

    fn controle(t: &Arc<Teleprompter>, amb: &Arc<Ambiente>, porta: u16, pin: Option<&str>) -> Arc<Sessao> {
        Sessao::iniciar_controle(
            Arc::clone(t),
            Arc::clone(amb),
            ConfigDoControle {
                destino: regras::destino_do_controle(&format!("127.0.0.1:{porta}")).unwrap(),
                pin: pin.map(str::to_string),
            },
        )
    }

    #[test]
    fn o_controle_manda_o_roteiro_e_os_comandos_e_os_dois_convergem() {
        let p = porta();
        let tp = replica("prompter-a", Papel::Teleprompter);
        let tc = replica("controle-a", Papel::ControleRemoto);
        let pares_p = Arc::new(Mutex::new(PairedPeers::new()));
        let pares_c = Arc::new(Mutex::new(PairedPeers::new()));
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente("prompter-a", pares_p),
            ConfigDoPrompter { porta: p, pin: Some("424242".into()), anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta == p), "o prompter abriu a porta pedida");
        let amb_c = ambiente("controle-a", Arc::clone(&pares_c));
        let sc = controle(&tc, &amb_c, p, Some("424242"));
        assert!(ate(Duration::from_secs(15), || sc.painel().fase == Fase::Conectada), "o controle entrou");

        let roteiro: String = (0..2000).map(|i| format!("Linha {i}: ação e emoção 🎬\n")).collect();
        sc.antes_de_editar();
        tc.definir_texto(&roteiro).unwrap();
        tc.definir_velocidade(2.5).unwrap();
        tc.definir_rolando(true).unwrap();
        tc.saltar(0.25).unwrap();
        sc.depois_de_editar();
        assert!(ate(Duration::from_secs(10), || tp.texto().unwrap() == roteiro), "o roteiro atravessou");
        assert!(ate(Duration::from_secs(5), || {
            let e = estado(&tp);
            e.rolando && e.velocidade == 2.5 && e.salto == Some(0.25)
        }));
        // O prompter liga o espelho; o controle vê.
        tp.definir_espelho(true).unwrap();
        assert!(ate(Duration::from_secs(5), || estado(&tc).espelho));
        assert!(ate(Duration::from_secs(5), || estado(&tc).sem_confirmacao_ha_ms.is_none()));
        assert!(!sc.medidas().confirmacoes_ms.is_empty(), "a confirmação foi medida");
        // O salto chegou ao prompter como bit: a janela seria acordada para ir até ele.
        assert!(sp.saltos_vistos() >= 1);
        assert!(sp.tirar_mudancas() & mudou::SALTO != 0 || sp.saltos_vistos() >= 1);

        sc.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)));
        // **O controle saiu e o prompter continua como estava** (rolando segue rolando).
        assert!(ate(Duration::from_secs(10), || sp.painel().fase == Fase::SemPar), "o prompter viu a queda");
        assert!(estado(&tp).rolando, "a regra do usuário: rolando segue rolando");
        sp.pedir_parada();
        assert!(sp.esperar(Duration::from_secs(10)));
    }

    #[test]
    fn o_controle_volta_sem_pin_na_mesma_porta_e_o_prompter_mantem_o_pin() {
        let p = porta();
        let tp = replica("prompter-b", Papel::Teleprompter);
        let tc = replica("controle-b", Papel::ControleRemoto);
        let pares_p = Arc::new(Mutex::new(PairedPeers::new()));
        let pares_c = Arc::new(Mutex::new(PairedPeers::new()));
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente("prompter-b", pares_p),
            ConfigDoPrompter { porta: p, pin: Some("515151".into()), anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta == p));
        let amb_c = ambiente("controle-b", Arc::clone(&pares_c));
        let sc = controle(&tc, &amb_c, p, Some("515151"));
        assert!(ate(Duration::from_secs(15), || sc.painel().fase == Fase::Conectada));
        tc.definir_rolando(true).unwrap();
        assert!(ate(Duration::from_secs(5), || estado(&tp).rolando));
        sc.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)));
        assert!(ate(Duration::from_secs(10), || sp.painel().fase == Fase::SemPar));
        assert_eq!(sp.painel().pin, "515151", "depois de uma queda, o mesmo PIN");
        assert_eq!(sp.painel().porta, p, "e a mesma porta");

        // O controle volta **sem PIN**: o par ficou gravado.
        let sc2 = controle(&tc, &amb_c, p, None);
        assert!(ate(Duration::from_secs(15), || sc2.painel().fase == Fase::Conectada), "voltou sem PIN");
        // O aviso do prompter apaga na primeira mensagem do controle de volta.
        assert!(ate(Duration::from_secs(5), || estado(&tp).par_visto_ha_ms.is_some()));
        tc.definir_fonte(64.0).unwrap();
        assert!(ate(Duration::from_secs(5), || estado(&tp).fonte == 64.0));
        assert!(estado(&tc).rolando, "o controle adotou o rolando do prompter");
        sc2.pedir_parada();
        sp.pedir_parada();
        assert!(sc2.esperar(Duration::from_secs(10)) && sp.esperar(Duration::from_secs(10)));
    }

    #[test]
    fn pin_errado_troca_o_pin_do_prompter_e_o_novo_entra() {
        let p = porta();
        let tp = replica("prompter-c", Papel::Teleprompter);
        let pares_p = Arc::new(Mutex::new(PairedPeers::new()));
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente("prompter-c", pares_p),
            ConfigDoPrompter { porta: p, pin: Some("626262".into()), anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta == p));
        let intrusa = replica("intrusa-c", Papel::ControleRemoto);
        let amb_i = ambiente("intrusa-c", Arc::new(Mutex::new(PairedPeers::new())));
        let si = controle(&intrusa, &amb_i, p, Some("000000"));
        assert!(si.esperar(Duration::from_secs(15)), "o controle com PIN errado para (precisa da pessoa)");
        assert!(si.medidas().falhas.iter().any(|f| f.contains("WRONG_PIN") || f.contains("PAIRING")), "{:?}", si.medidas().falhas);
        assert!(ate(Duration::from_secs(5), || sp.painel().pin != "626262"), "o PIN mudou depois do erro");
        assert!(sp.medidas().pins_trocados >= 1);
        let novo = sp.painel().pin.clone();
        // O PIN velho não entra mais; o novo entra (depois da espera crescente de 1 s).
        let tc = replica("controle-c", Papel::ControleRemoto);
        let amb_c = ambiente("controle-c", Arc::new(Mutex::new(PairedPeers::new())));
        std::thread::sleep(Duration::from_millis(1200));
        let sc = controle(&tc, &amb_c, p, Some(&novo));
        assert!(ate(Duration::from_secs(15), || sc.painel().fase == Fase::Conectada), "o PIN novo entrou");
        sc.pedir_parada();
        sp.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)) && sp.esperar(Duration::from_secs(10)));
    }

    #[test]
    fn um_segundo_controle_ouve_ocupado_e_a_sessao_de_pe_nao_cai() {
        let p = porta();
        let tp = replica("prompter-d", Papel::Teleprompter);
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente("prompter-d", Arc::new(Mutex::new(PairedPeers::new()))),
            ConfigDoPrompter { porta: p, pin: Some("737373".into()), anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta == p));
        let tc = replica("controle-d", Papel::ControleRemoto);
        let sc = controle(&tc, &ambiente("controle-d", Arc::new(Mutex::new(PairedPeers::new()))), p, Some("737373"));
        assert!(ate(Duration::from_secs(15), || sc.painel().fase == Fase::Conectada));
        let t2 = replica("controle-d2", Papel::ControleRemoto);
        let s2 = controle(&t2, &ambiente("controle-d2", Arc::new(Mutex::new(PairedPeers::new()))), p, Some("737373"));
        assert!(ate(Duration::from_secs(10), || s2.medidas().ocupados_ouvidos >= 1), "o segundo ouviu BUSY");
        s2.pedir_parada();
        assert!(s2.esperar(Duration::from_secs(10)));
        tc.definir_margem(0.2).unwrap();
        assert!(ate(Duration::from_secs(5), || estado(&tp).margem == 0.2), "a sessão de pé seguiu viva");
        assert_eq!(sc.medidas().quedas, 0);
        sc.pedir_parada();
        sp.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)) && sp.esperar(Duration::from_secs(10)));
    }

    #[test]
    fn a_porta_ocupada_leva_a_proxima_livre() {
        let p = porta();
        let ocupa = std::net::TcpListener::bind(("0.0.0.0", p)).expect("ocupar a porta");
        let tp = replica("prompter-e", Papel::Teleprompter);
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente("prompter-e", Arc::new(Mutex::new(PairedPeers::new()))),
            ConfigDoPrompter { porta: p, pin: None, anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta != 0));
        assert_eq!(sp.painel().porta, p + 1);
        // O endereço que a tela mostra (quando há rede) já traz a porta que de fato abriu.
        let e = sp.painel().endereco.clone().unwrap_or_default();
        assert!(e.ends_with(&format!(":{}", p + 1)), "{e:?}");
        sp.pedir_parada();
        assert!(sp.esperar(Duration::from_secs(10)));
        drop(ocupa);
    }

    /// Um prompter com o roteiro dele, esperando por 127.0.0.1.
    fn prompter_com_roteiro(id: &str, pin: &str, roteiro: &str) -> (Arc<Teleprompter>, Arc<Sessao>, u16) {
        let p = porta();
        let tp = replica(id, Papel::Teleprompter);
        tp.definir_texto(roteiro).unwrap();
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente(id, Arc::new(Mutex::new(PairedPeers::new()))),
            ConfigDoPrompter { porta: p, pin: Some(pin.into()), anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta == p));
        (tp, sp, p)
    }

    #[test]
    fn a_pergunta_do_texto_pelas_duas_saidas_e_a_copia_do_que_saiu() {
        // O controle com a trava ligada e um roteiro dele (§11.10).
        let tc = replica("controle-q", Papel::ControleRemoto);
        tc.ligar_pergunta_do_texto().unwrap();
        tc.definir_texto("Roteiro do controle, primeira versão.").unwrap();
        let amb_c = ambiente("controle-q", Arc::new(Mutex::new(PairedPeers::new())));

        // 1. Um prompter novo com outro roteiro: pergunta; "usar o do prompter" guarda o meu.
        let (tp1, sp1, p1) = prompter_com_roteiro("prompter-q1", "717171", "Roteiro do prompter um.");
        let sc = controle(&tc, &amb_c, p1, Some("717171"));
        assert!(ate(Duration::from_secs(15), || estado(&tc).pergunta_do_texto.as_ref().is_some_and(|q| q.aberta)), "a pergunta abriu");
        let q = estado(&tc).pergunta_do_texto.unwrap();
        assert_eq!(q.prompter_nome, "Teste prompter-q1");
        let visto = q.do_prompter.as_ref().unwrap().resumo.clone();
        tc.resolver_texto(false, &visto).unwrap();
        assert!(ate(Duration::from_secs(5), || tc.texto().unwrap() == "Roteiro do prompter um."), "ficou com o do prompter");
        assert_eq!(tp1.texto().unwrap(), "Roteiro do prompter um.", "o prompter não mudou");
        let e = estado(&tc);
        assert!(e.pergunta_do_texto.is_none());
        assert_eq!(e.copias_do_texto.len(), 1);
        assert_eq!(e.copias_do_texto[0].origem, quall_core::teleprompter::OrigemDaCopia::Controle);
        let guardada = tc.copia_do_texto(&e.copias_do_texto[0].resumo).unwrap();
        assert_eq!(guardada.as_deref(), Some("Roteiro do controle, primeira versão."));
        sc.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)));

        // 2. Outro prompter novo: "mandar o meu" — o dele fica guardado.
        tc.definir_texto("Roteiro do controle, segunda versão.").unwrap();
        let (tp2, sp2, p2) = prompter_com_roteiro("prompter-q2", "727272", "Roteiro do prompter dois.");
        let sc2 = controle(&tc, &amb_c, p2, Some("727272"));
        assert!(ate(Duration::from_secs(15), || estado(&tc).pergunta_do_texto.as_ref().is_some_and(|q| q.aberta)));
        let visto = estado(&tc).pergunta_do_texto.unwrap().do_prompter.unwrap().resumo;
        tc.resolver_texto(true, &visto).unwrap();
        assert!(ate(Duration::from_secs(10), || tp2.texto().unwrap() == "Roteiro do controle, segunda versão."), "o prompter recebeu o meu");
        let copias = estado(&tc).copias_do_texto;
        assert_eq!(copias[0].origem, quall_core::teleprompter::OrigemDaCopia::Prompter, "o do prompter, a mais nova");
        assert_eq!(copias[0].prompter_nome, "Teste prompter-q2");
        sc2.pedir_parada();
        assert!(sc2.esperar(Duration::from_secs(10)));
        for s in [sp1, sp2] {
            s.pedir_parada();
            assert!(s.esperar(Duration::from_secs(10)));
        }
    }

    #[test]
    fn a_porta_pedida_que_solta_dentro_de_2_s_e_a_usada() {
        // A tela recriada: a sessão velha ainda segura a porta por um instante. A tela nova espera
        // por ela (até 2 s) em vez de ir para a seguinte (§11.7).
        let p = porta();
        let ocupa = std::net::TcpListener::bind(("0.0.0.0", p)).expect("ocupar a porta");
        let solta = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            drop(ocupa);
        });
        let tp = replica("prompter-e2", Papel::Teleprompter);
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente("prompter-e2", Arc::new(Mutex::new(PairedPeers::new()))),
            ConfigDoPrompter { porta: p, pin: None, anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta != 0));
        assert_eq!(sp.painel().porta, p, "esperou pela porta pedida");
        solta.join().unwrap();
        sp.pedir_parada();
        assert!(sp.esperar(Duration::from_secs(10)));
    }

    #[test]
    fn o_controle_sem_ninguem_no_endereco_para_e_diz() {
        let p = porta();
        let tc = replica("controle-f", Papel::ControleRemoto);
        let sc = controle(&tc, &ambiente("controle-f", Arc::new(Mutex::new(PairedPeers::new()))), p, Some("111111"));
        assert!(sc.esperar(Duration::from_secs(20)), "na primeira vez, rede recusada para");
        let painel = sc.painel().clone();
        assert_eq!(painel.fase, Fase::Parada);
        assert!(!painel.mensagem.is_empty());
    }

    #[test]
    fn os_nomes_dos_bits() {
        assert_eq!(nomes(mudou::TEXTO | mudou::PAR), "texto par");
        assert_eq!(nomes(mudou::SEGURAR | mudou::ROLANDO), "rolando segurar");
        assert_eq!(nomes(mudou::PERGUNTA_DO_TEXTO | mudou::COPIA_DO_TEXTO), "pergunta copia");
        assert_eq!(nomes(mudou::GRAVACAO | mudou::PAR), "par gravacao");
        assert_eq!(nomes(1 << 14), "desconhecido(16384)");
    }

    /// Um prompter (com a tela que entende o segurar) e um controle, conectados por 127.0.0.1.
    fn par_para_segurar(id: &str, pin: &str) -> (Arc<Teleprompter>, Arc<Teleprompter>, Arc<Sessao>, Arc<Sessao>) {
        let p = porta();
        let tp = replica(&format!("prompter-{id}"), Papel::Teleprompter);
        tp.ligar_segurar().unwrap();
        let tc = replica(&format!("controle-{id}"), Papel::ControleRemoto);
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente(&format!("prompter-{id}"), Arc::new(Mutex::new(PairedPeers::new()))),
            ConfigDoPrompter { porta: p, pin: Some(pin.into()), anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta == p));
        let amb_c = ambiente(&format!("controle-{id}"), Arc::new(Mutex::new(PairedPeers::new())));
        let sc = controle(&tc, &amb_c, p, Some(pin));
        assert!(ate(Duration::from_secs(15), || sc.painel().fase == Fase::Conectada), "o controle entrou");
        assert!(ate(Duration::from_secs(5), || estado(&tc).par_entende_segurar), "o prompter disse que entende");
        (tp, tc, sp, sc)
    }

    #[test]
    fn segurar_para_tras_e_soltar_atravessam_e_param() {
        let (tp, tc, sp, sc) = par_para_segurar("s1", "626262");
        tc.segurar(true).unwrap();
        assert!(ate(Duration::from_secs(5), || {
            let e = estado(&tp);
            e.rolando && e.para_tras && e.segurando
        }), "o aperto chegou numa mensagem: rolando, para trás, segurando");
        assert!(sp.tirar_mudancas() & mudou::SEGURAR != 0, "o prompter viu o bit do segurar");
        tc.soltar().unwrap();
        assert!(ate(Duration::from_secs(5), || {
            let e = estado(&tp);
            !e.rolando && !e.para_tras && !e.segurando
        }), "soltar parou o texto");
        sc.pedir_parada();
        sp.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)) && sp.esperar(Duration::from_secs(10)));
    }

    #[test]
    fn o_controle_que_sai_com_o_dedo_no_botao_para_o_prompter() {
        let (tp, tc, sp, sc) = par_para_segurar("s2", "636363");
        tc.segurar(false).unwrap();
        assert!(ate(Duration::from_secs(5), || estado(&tp).segurando));
        // O controle sai **sem soltar**: pela ordem do fim, `perdeu_o_par` dos dois lados para o
        // texto (a exceção do segurar à regra "continua como estava").
        sc.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)));
        assert!(!estado(&tc).rolando, "o controle parou o texto do lado dele");
        assert!(ate(Duration::from_secs(10), || sp.painel().fase == Fase::SemPar), "o prompter viu a queda");
        let e = estado(&tp);
        assert!(!e.rolando && !e.segurando, "o prompter parou com a queda: {e:?}");
        sp.pedir_parada();
        assert!(sp.esperar(Duration::from_secs(10)));
    }

    #[test]
    fn o_controle_nao_segura_um_prompter_que_nao_entende() {
        let p = porta();
        let tp = replica("prompter-s3", Papel::Teleprompter); // sem `ligar_segurar`: a tela de 13/09
        let tc = replica("controle-s3", Papel::ControleRemoto);
        let sp = Sessao::iniciar_prompter(
            Arc::clone(&tp),
            ambiente("prompter-s3", Arc::new(Mutex::new(PairedPeers::new()))),
            ConfigDoPrompter { porta: p, pin: Some("646464".into()), anunciar: false },
        );
        assert!(ate(Duration::from_secs(5), || sp.painel().porta == p));
        let sc = controle(&tc, &ambiente("controle-s3", Arc::new(Mutex::new(PairedPeers::new()))), p, Some("646464"));
        assert!(ate(Duration::from_secs(15), || sc.painel().fase == Fase::Conectada));
        assert!(ate(Duration::from_secs(5), || estado(&tc).par_visto_ha_ms.is_some()));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!estado(&tc).par_entende_segurar);
        let r = tc.segurar(false);
        assert_eq!(r.as_ref().map_err(regras::Codigo::de).err(), Some(regras::Codigo::Protocolo), "{r:?}");
        std::thread::sleep(Duration::from_millis(500));
        assert!(!estado(&tp).rolando, "o prompter que não entende nunca rolou");
        sc.pedir_parada();
        sp.pedir_parada();
        assert!(sc.esperar(Duration::from_secs(10)) && sp.esperar(Duration::from_secs(10)));
    }
}
