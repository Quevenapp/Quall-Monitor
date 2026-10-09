//! Cancelamento de uma espera bloqueante — a dívida 10.
//!
//! [`crate::session::hospedar`] e [`crate::session::conectar`] bloqueiam até a sessão fechar ou o
//! prazo estourar. Sem uma forma de interromper, o botão **Cancelar** da tela de espera não tem o
//! que chamar, e as cascas inventaram cada uma o seu contorno: Android e iOS abrem uma **conexão
//! TCP descartável para o próprio endereço**, só para destravar o `accept`.
//!
//! O contorno funciona — a frente iOS mediu: sem cutucada, `quall_host` segura 120,16 s; com
//! cutucada aos 3 s, sai em 3,09 s — mas ele depende de **dois defeitos se cancelarem**. Era a
//! ausência de prazo no handshake (dívida 18) que fazia o `close` da conexão descartável ser
//! necessário na hora certa. Consertada a 18, o contorno fica mais frágil, não menos.
//!
//! # O desenho
//!
//! Uma bandeira atômica compartilhada, e nada além disso. Sem canal, sem thread, sem sinal: os
//! laços de espera do núcleo já acordam a cada fatia de 5 a 20 ms para revezar sinalização e
//! transporte, e a única coisa que falta é eles **perguntarem**.
//!
//! Cancelar é **irreversível e idempotente**: um [`Cancelamento`] cancelado não volta atrás. Uma
//! tela de espera que cancela e tenta de novo cria outro; reaproveitar um cancelado seria a
//! forma mais barata de fazer a segunda tentativa nascer morta.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Bandeira de cancelamento compartilhada entre a thread que espera e quem toca em Cancelar.
///
/// Clonar dá outra ponta da **mesma** bandeira: é `Arc` por dentro, e é isso que permite entregar
/// uma cópia à casca antes de a chamada bloqueante começar.
#[derive(Debug, Clone, Default)]
pub struct Cancelamento(Arc<AtomicBool>);

impl Cancelamento {
    /// Um cancelamento ainda não pedido.
    pub fn novo() -> Self {
        Self::default()
    }

    /// Pede o cancelamento. Pode ser chamado de qualquer thread, quantas vezes quiser.
    pub fn cancelar(&self) {
        // `Release` casa com o `Acquire` de `cancelado`: quem vê a bandeira levantada vê também
        // tudo o que a thread que cancelou escreveu antes.
        self.0.store(true, Ordering::Release);
    }

    /// Já pediram para cancelar?
    pub fn cancelado(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nasce_sem_cancelamento_e_nao_volta_atras() {
        let c = Cancelamento::novo();
        assert!(!c.cancelado());
        c.cancelar();
        assert!(c.cancelado());
        c.cancelar();
        assert!(c.cancelado());
    }

    #[test]
    fn a_copia_e_a_mesma_bandeira() {
        let c = Cancelamento::novo();
        let copia = c.clone();
        std::thread::spawn(move || copia.cancelar())
            .join()
            .expect("thread");
        assert!(c.cancelado(), "cancelar na cópia tinha de aparecer aqui");
    }
}
