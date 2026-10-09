//! O portão da sessão: o desregistro e a barreira que faltavam ao `quall_session_close`.
//!
//! # O problema, na forma em que ele morde
//!
//! Os tratadores da casca — `ao_pedir_idr` no emissor, `ao_receber_quadro` no receptor — rodam
//! em **threads da libdatachannel**. A fronteira C repassa a eles um `user_data` que é da casca:
//! um `id` de objeto Swift, um `jobject` global, o `obs_source_t*` do plugin. Fechar a sessão
//! desligava o caminho, mas não dizia nada sobre quem **já estava dentro** do código da casca
//! naquele instante. O header dizia a verdade sobre o perigo e passava a conta adiante:
//!
//! > Mantenha o `user_data` vivo por conta própria, ou proteja-o com um cadeado que o callback
//! > respeite.
//!
//! Medido aqui em 2026-08-26, MacBook Air M4, com o tratador de PLI comprovadamente dentro da
//! casca na hora do fechamento:
//!
//! | caminho | o `Drop` da sessão voltou em | tratador ainda dentro? |
//! |---|---|---|
//! | quadro recebido (`ao_receber_quadro`) | 2,00 s | não |
//! | pedido de IDR (`ao_pedir_idr`) | **0,49 ms** | **sim** |
//!
//! A primeira linha não é mérito nosso: `rtcDeleteTrack` espera o despacho de mensagem da
//! própria libdatachannel terminar, e isso é detalhe de implementação dela, num sistema
//! operacional só. A segunda é o defeito no estado puro — e é o lado do **emissor**, que é o
//! celular espelhando e a fonte do plugin de OBS.
//!
//! # O desenho: um portão por sessão, com contagem de quem está dentro
//!
//! [`Portao`] é uma porta que só fecha uma vez e que sabe quantas threads estão do lado de
//! dentro. Quem vai tocar a API C ou chamar a casca pede um [`Passe`]; quem fecha a sessão
//! fecha a porta e **espera esvaziar**.
//!
//! Isso dá três coisas de uma vez:
//!
//! 1. **Barreira.** Quando [`Portao::fechar_com_prazo`] volta com [`Barreira::Cumprida`], não há
//!    thread nenhuma dentro do código da casca e não haverá mais nenhuma. É o que autoriza
//!    liberar o `user_data`.
//! 2. **Desregistro com barreira.** [`Portao::esperar_vazio`] prova o mesmo sem fechar a porta:
//!    quem tirou o tratador do lugar espera o portão esvaziar **uma vez** e sabe que ninguém
//!    ficou dentro do tratador antigo.
//! 3. **A corrida do id morto, que a bandeira sozinha não fechava.** A `SessaoViva` anterior era
//!    um `AtomicBool`: conferir e usar eram dois passos, e entre eles o `Drop` da sessão podia
//!    chamar `rtcDeleteTrack`. A chamada seguinte à API C ia com um id morto — que no Windows
//!    **trava o processo**, não devolve erro. Com o portão, quem está dentro segura o
//!    fechamento, e o `rtcDeleteTrack` só acontece depois que todo mundo saiu.
//!
//! # O que foi descartado, e por quê
//!
//! **Tratadores atrás de `Weak`.** O tratador da casca ficaria num `Weak`; o fechamento soltaria
//! o `Arc` forte e o despacho desistiria quando o `upgrade` falhasse. Resolve *entrada nova* e
//! **não resolve barreira**: quem já fez `upgrade` está dentro com uma referência forte, e o
//! fechamento não tem como saber quando ele sai a não ser espiando `Arc::strong_count` num laço
//! — que é a mesma contagem deste módulo, escrita de um jeito que não dá para esperar sem girar.
//! Também não ajuda em nada com a corrida do id morto, que não passa por tratador nenhum.
//!
//! **Geração por sessão (epoch/token).** Cada sessão ganharia um número; o despacho compararia
//! antes de chamar. É elegante para "não entre mais", e é exatamente a mesma coisa que a
//! `AtomicBool` que já existia — porque uma sessão não renasce, o token só tem dois estados
//! úteis. E, de novo, **não é barreira**: comparar geração é um teste, não uma espera. Ganharia
//! algo se um handle de track pudesse ser reaproveitado entre sessões, e ele não pode.
//!
//! As duas ideias resolvem a metade fácil. A metade difícil — *esperar* — precisa de contagem de
//! ocupação, e é isso que está aqui.
//!
//! # Custo
//!
//! Caminho rápido de [`Portao::entrar`]: uma leitura, um `fetch_add`, outra leitura, e o par
//! simétrico na saída. Nenhum cadeado. O `Mutex`/`Condvar` só entram em cena quando alguém está
//! esperando o portão esvaziar, o que acontece uma vez por sessão.
//!
//! Com a porta já fechada, `entrar` sai na primeira leitura, **sem tocar na contagem**. Isso não
//! é economia: depois do fechamento os pacotes continuam chegando por algum tempo, e cada um
//! subindo e baixando o contador faz o portão parecer ocupado a quem espera esvaziar. Foi o
//! Galaxy A10s que mostrou isso; no M4 nunca apareceu.
//!
//! # Reentrância: a casca que fecha de dentro do próprio tratador
//!
//! Um plugin que descobre no `ao_receber_quadro` que o decoder morreu vai querer fechar a sessão
//! dali mesmo. Esperar esvaziar nesse caso é esperar por si próprio — travamento garantido, do
//! tipo que este repositório já pagou caro. Por isso cada thread carrega um contador de quantos
//! portões ela entrou, e quem fecha estando dentro recebe [`Barreira::DeDentroDoTratador`] **na
//! hora**, sem esperar.
//!
//! O contador é por thread e não por portão: é um `Cell<u32>` e nada mais. A consequência é uma
//! recusa conservadora — fechar a sessão A de dentro de um tratador da sessão B também devolve
//! `DeDentroDoTratador`. Erra para o lado seguro (a casca segura o `user_data` mais tempo) e
//! nunca para o lado que trava.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Quanto tempo o fechamento de uma sessão espera os tratadores da casca saírem.
///
/// O contrato manda que tratador não bloqueie: o normal é sair em microssegundos, e mesmo um
/// tratador que entrega o quadro a um decoder de hardware sai em milissegundos. Dois segundos
/// são quatro ordens de grandeza de folga.
///
/// Existe prazo, e não espera eterna, porque quem hospeda este código é o OBS, o Zoom ou uma
/// extension do iOS: uma casca com defeito tem de virar `QUALL_STATUS_TIMEOUT` e um `user_data`
/// que a casca segura mais tempo, nunca um processo pendurado. E estourar o prazo **não** é
/// perigoso do lado do núcleo — a porta já está fechada, então nenhuma chamada nossa chega mais
/// à API C com um id morto; o que fica em aberto é só o tempo de vida do `user_data`, que é da
/// casca.
pub const PRAZO_DA_BARREIRA: Duration = Duration::from_secs(2);

thread_local! {
    /// Em quantos portões **esta thread** entrou e ainda não saiu. Ver a nota de reentrância no
    /// topo do módulo.
    static PORTOES_DESTA_THREAD: Cell<u32> = const { Cell::new(0) };
}

/// O que se conseguiu provar ao fechar ou ao esvaziar um portão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Barreira {
    /// Ninguém está dentro do código da casca, e ninguém mais entra. **A casca pode liberar o
    /// `user_data`.**
    Cumprida,
    /// Quem pediu a barreira está ele próprio dentro de um tratador. Esperar seria esperar por
    /// si mesmo; a porta ficou fechada, mas a barreira **não** vale.
    DeDentroDoTratador,
    /// O prazo estourou com alguém ainda dentro. A porta ficou fechada; a casca **não** pode
    /// liberar o `user_data`.
    Prazo,
}

impl Barreira {
    /// A casca pode liberar o `user_data` agora?
    pub fn cumprida(self) -> bool {
        matches!(self, Barreira::Cumprida)
    }
}

/// Uma porta de mão única com contagem de ocupação. Ver o topo do módulo.
#[derive(Debug)]
pub struct Portao {
    aberto: AtomicBool,
    /// Quantas threads pegaram um [`Passe`] e ainda não o largaram.
    dentro: AtomicUsize,
    /// Quantas threads estão esperando o portão esvaziar. Enquanto for zero, a saída de um
    /// [`Passe`] não toca em cadeado nenhum — é o que mantém o caminho do quadro barato.
    esperando: AtomicUsize,
    espera: Mutex<()>,
    sino: Condvar,
}

impl Default for Portao {
    fn default() -> Self {
        Portao::novo()
    }
}

impl Portao {
    /// Um portão aberto e vazio.
    pub fn novo() -> Self {
        Portao {
            aberto: AtomicBool::new(true),
            dentro: AtomicUsize::new(0),
            esperando: AtomicUsize::new(0),
            espera: Mutex::new(()),
            sino: Condvar::new(),
        }
    }

    /// A sessão deste portão ainda existe?
    ///
    /// Leitura solta, para quem só quer relatar estado. Quem vai **agir** — tocar a API C,
    /// chamar a casca — usa [`Portao::entrar`], porque entre conferir e usar cabe um
    /// `rtcDeleteTrack`.
    pub fn esta_aberto(&self) -> bool {
        self.aberto.load(Ordering::Acquire)
    }

    /// Entra, se ainda der. Enquanto o [`Passe`] viver, [`Portao::fechar_com_prazo`] espera.
    pub fn entrar(&self) -> Option<Passe<'_>> {
        // Porta já fechada: sai sem tocar na contagem. Não é só economia — depois do
        // fechamento os pacotes continuam chegando por algum tempo, e cada um deles subindo e
        // baixando o contador faz o portão parecer ocupado a quem está esperando esvaziar.
        // Achado no Galaxy A10s, onde a contagem transitória aparecia e no M4 não.
        if !self.aberto.load(Ordering::SeqCst) {
            return None;
        }
        // Contar **antes** de conferir a porta de novo. Na ordem inversa, o fechamento poderia
        // ver o portão vazio entre a conferência e a contagem, e voltar dizendo que a barreira
        // valeu com alguém prestes a entrar.
        self.dentro.fetch_add(1, Ordering::SeqCst);
        if !self.aberto.load(Ordering::SeqCst) {
            self.sair();
            return None;
        }
        PORTOES_DESTA_THREAD.with(|n| n.set(n.get().saturating_add(1)));
        Some(Passe { portao: self })
    }

    /// Fecha a porta e espera todo mundo sair, até `prazo`.
    ///
    /// Fechar duas vezes é barato e devolve [`Barreira::Cumprida`] na segunda: a porta já está
    /// fechada e o portão, vazio.
    pub fn fechar_com_prazo(&self, prazo: Duration) -> Barreira {
        self.aberto.store(false, Ordering::SeqCst);
        self.esperar_vazio(prazo)
    }

    /// Espera o portão esvaziar **uma vez**, sem fechar a porta.
    ///
    /// É a barreira do desregistro: quem já tirou o tratador do lugar e viu o portão vazio sabe
    /// que ninguém ficou dentro do tratador antigo — porque todo despacho acontece com um
    /// [`Passe`] na mão.
    pub fn esperar_vazio(&self, prazo: Duration) -> Barreira {
        if PORTOES_DESTA_THREAD.with(Cell::get) > 0 {
            return Barreira::DeDentroDoTratador;
        }
        // Anunciar a espera **antes** de olhar a contagem, e sair contando **antes** de olhar
        // quem espera (ver `sair`): um dos dois lados sempre enxerga o outro.
        self.esperando.fetch_add(1, Ordering::SeqCst);
        let resultado = self.esperar(prazo);
        self.esperando.fetch_sub(1, Ordering::SeqCst);
        resultado
    }

    fn esperar(&self, prazo: Duration) -> Barreira {
        let fim = Instant::now() + prazo;
        let Ok(mut guarda) = self.espera.lock() else {
            // Cadeado envenenado por um pânico em outra thread. Não dá para provar barreira
            // nenhuma daqui, e mentir seria pior que o prazo.
            return Barreira::Prazo;
        };
        while self.dentro.load(Ordering::SeqCst) > 0 {
            let agora = Instant::now();
            if agora >= fim {
                return Barreira::Prazo;
            }
            let Ok((proxima, _)) = self.sino.wait_timeout(guarda, fim - agora) else {
                return Barreira::Prazo;
            };
            guarda = proxima;
        }
        Barreira::Cumprida
    }

    fn sair(&self) {
        let antes = self.dentro.fetch_sub(1, Ordering::SeqCst);
        // O caso comum é ninguém esperando: uma leitura e pronto, sem cadeado no caminho do
        // quadro.
        if antes == 1 && self.esperando.load(Ordering::SeqCst) > 0 {
            if let Ok(_g) = self.espera.lock() {
                self.sino.notify_all();
            }
        }
    }
}

/// Prova de que uma thread está dentro do portão. Segura o fechamento da sessão até sair.
#[derive(Debug)]
pub struct Passe<'a> {
    portao: &'a Portao,
}

impl Drop for Passe<'_> {
    fn drop(&mut self) {
        PORTOES_DESTA_THREAD.with(|n| n.set(n.get().saturating_sub(1)));
        self.portao.sair();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    #[test]
    fn portao_novo_deixa_entrar() {
        let p = Portao::novo();
        assert!(p.esta_aberto());
        assert!(p.entrar().is_some());
    }

    #[test]
    fn portao_fechado_nao_deixa_entrar() {
        let p = Portao::novo();
        assert_eq!(p.fechar_com_prazo(PRAZO_DA_BARREIRA), Barreira::Cumprida);
        assert!(!p.esta_aberto());
        assert!(
            p.entrar().is_none(),
            "entrou num portão fechado: é o tratador disparando depois do `close`"
        );
    }

    #[test]
    fn fechar_duas_vezes_continua_valendo() {
        let p = Portao::novo();
        assert_eq!(p.fechar_com_prazo(PRAZO_DA_BARREIRA), Barreira::Cumprida);
        assert_eq!(p.fechar_com_prazo(PRAZO_DA_BARREIRA), Barreira::Cumprida);
    }

    /// **A barreira.** Quem está dentro segura o fechamento.
    #[test]
    fn fechar_espera_quem_esta_dentro_sair() {
        let p = Arc::new(Portao::novo());
        let entrou = Arc::new(AtomicBool::new(false));
        let saiu = Arc::new(AtomicBool::new(false));

        let trabalhador = {
            let (p, entrou, saiu) = (Arc::clone(&p), Arc::clone(&entrou), Arc::clone(&saiu));
            std::thread::spawn(move || {
                let passe = p.entrar().expect("o portão estava aberto");
                entrou.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
                saiu.store(true, Ordering::SeqCst);
                drop(passe);
            })
        };

        while !entrou.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(p.fechar_com_prazo(PRAZO_DA_BARREIRA), Barreira::Cumprida);
        assert!(
            saiu.load(Ordering::SeqCst),
            "o fechamento voltou com alguém ainda dentro"
        );
        let _ = trabalhador.join();
    }

    /// O prazo existe para que uma casca com defeito vire status, e não processo pendurado.
    #[test]
    fn quem_nao_sai_a_tempo_vira_prazo_e_nao_travamento() {
        let p = Arc::new(Portao::novo());
        let solta = Arc::new(AtomicBool::new(false));
        let entrou = Arc::new(AtomicBool::new(false));

        let teimoso = {
            let (p, solta, entrou) = (Arc::clone(&p), Arc::clone(&solta), Arc::clone(&entrou));
            std::thread::spawn(move || {
                let passe = p.entrar().expect("aberto");
                entrou.store(true, Ordering::SeqCst);
                while !solta.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                drop(passe);
            })
        };
        while !entrou.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }

        let comecou = Instant::now();
        let barreira = p.fechar_com_prazo(Duration::from_millis(120));
        let levou = comecou.elapsed();
        assert_eq!(barreira, Barreira::Prazo);
        assert!(
            levou >= Duration::from_millis(100) && levou < Duration::from_secs(5),
            "esperou {levou:?}, fora do prazo pedido"
        );
        // A porta fica fechada mesmo com o prazo estourado: ninguém mais entra.
        assert!(p.entrar().is_none());

        solta.store(true, Ordering::SeqCst);
        let _ = teimoso.join();
    }

    /// **Reentrância.** A casca que fecha de dentro do próprio tratador não pode pendurar o
    /// processo.
    #[test]
    fn fechar_de_dentro_do_tratador_volta_na_hora() {
        let p = Portao::novo();
        let passe = p.entrar().expect("aberto");

        let comecou = Instant::now();
        let barreira = p.fechar_com_prazo(Duration::from_secs(30));
        let levou = comecou.elapsed();

        assert_eq!(barreira, Barreira::DeDentroDoTratador);
        assert!(
            levou < Duration::from_secs(1),
            "esperou {levou:?} por si mesma — é o travamento que este caso existe para evitar"
        );
        assert!(
            !p.esta_aberto(),
            "a porta tinha de ficar fechada mesmo assim"
        );
        drop(passe);
    }

    #[test]
    fn sair_do_tratador_devolve_a_thread_ao_estado_normal() {
        let p = Portao::novo();
        {
            let _passe = p.entrar().expect("aberto");
        }
        // Fora do tratador, a barreira volta a ser possível.
        assert_eq!(p.fechar_com_prazo(PRAZO_DA_BARREIRA), Barreira::Cumprida);
    }

    /// `esperar_vazio` prova a mesma coisa sem fechar a porta: é a barreira do desregistro.
    #[test]
    fn esperar_vazio_nao_fecha_a_porta() {
        let p = Arc::new(Portao::novo());
        let entrou = Arc::new(AtomicBool::new(false));
        let saiu = Arc::new(AtomicU64::new(0));

        let trabalhador = {
            let (p, entrou, saiu) = (Arc::clone(&p), Arc::clone(&entrou), Arc::clone(&saiu));
            std::thread::spawn(move || {
                let passe = p.entrar().expect("aberto");
                entrou.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(200));
                saiu.fetch_add(1, Ordering::SeqCst);
                drop(passe);
            })
        };
        while !entrou.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(p.esperar_vazio(PRAZO_DA_BARREIRA), Barreira::Cumprida);
        assert_eq!(saiu.load(Ordering::SeqCst), 1);
        assert!(p.esta_aberto(), "esperar esvaziar não pode fechar a porta");
        assert!(p.entrar().is_some());
        let _ = trabalhador.join();
    }

    /// Muitas threads entrando e saindo enquanto o portão fecha: ninguém pode ficar dentro, e
    /// ninguém pode entrar depois.
    ///
    /// # O contador que a casca vê, e não o de dentro do portão
    ///
    /// A primeira versão deste teste olhava o `dentro` do próprio portão logo depois de fechar.
    /// **O Galaxy A10s a derrubou**, e com razão: `entrar` conta antes de conferir a porta pela
    /// segunda vez, então uma thread que vai ser recusada faz o contador subir e descer. Ler
    /// `dentro` de fora é ler um número que oscila por desenho.
    ///
    /// O invariante que interessa é outro, e é o da casca: **quantas threads estão dentro da
    /// seção crítica**. Ele só sobe depois de o passe ter sido concedido — e o passe concedido é
    /// exatamente o que segura o fechamento —, então zero aqui prova a barreira sem depender de
    /// detalhe interno.
    #[test]
    fn fechamento_sob_transito_nao_deixa_ninguem_dentro() {
        let p = Arc::new(Portao::novo());
        let parar = Arc::new(AtomicBool::new(false));
        let ocupados = Arc::new(AtomicU64::new(0));

        let mut threads = Vec::new();
        for _ in 0..4 {
            let (p, parar, ocupados) = (Arc::clone(&p), Arc::clone(&parar), Arc::clone(&ocupados));
            threads.push(std::thread::spawn(move || {
                while !parar.load(Ordering::Relaxed) {
                    if let Some(passe) = p.entrar() {
                        ocupados.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_micros(50));
                        ocupados.fetch_sub(1, Ordering::SeqCst);
                        drop(passe);
                    }
                }
            }));
        }

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(p.fechar_com_prazo(PRAZO_DA_BARREIRA), Barreira::Cumprida);
        assert_eq!(
            ocupados.load(Ordering::SeqCst),
            0,
            "o fechamento voltou com alguém dentro da seção crítica"
        );

        // E com as threads ainda martelando `entrar`, ninguém entra de novo.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            ocupados.load(Ordering::SeqCst),
            0,
            "alguém entrou depois de a porta fechar"
        );

        parar.store(true, Ordering::Relaxed);
        for t in threads {
            let _ = t.join();
        }
    }
}
