//! A puxada promete não alocar nem liberar memória (`docs/contrato-som-puxado.md`,
//! `crates/quall-core/src/reproducao.rs`). Um alocador que conta só na thread armada confere.
//!
//! Trazido da revisão do código da S1 (achado A7): no Mac e no iOS o `Mutex` da std aloca na
//! primeira trava, e a **primeira** puxada alocava uma vez. O conserto aciona os cadeados na
//! criação.
//!
//! Este arquivo é um binário de teste próprio, porque troca o alocador global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use quall_core::jitter::Politica;
use quall_core::media::Clock;
use quall_core::reproducao::{OpcoesDeReproducao, ReproducaoPuxada};
use quall_core::rtp::QuadroDeAudio;

struct Conta;
static N: AtomicUsize = AtomicUsize::new(0);
thread_local! { static ARMADA: Cell<bool> = const { Cell::new(false) }; }

// SAFETY: repassa ao alocador do sistema sem mudar nada; só conta.
unsafe impl GlobalAlloc for Conta {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ARMADA.with(|a| a.get()) {
            N.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ARMADA.with(|a| a.get()) {
            N.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static G: Conta = Conta;

fn armada<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let antes = N.load(Ordering::SeqCst);
    ARMADA.with(|a| a.set(true));
    let r = f();
    ARMADA.with(|a| a.set(false));
    (r, N.load(Ordering::SeqCst) - antes)
}

#[test]
fn a_puxada_nao_aloca_nem_libera() {
    let (alimentador, mut rep) = ReproducaoPuxada::nova(
        OpcoesDeReproducao {
            politica: Politica::MICROFONE,
            casca_reamostra: false,
        },
        Arc::new(Clock::new()),
    );
    let leitor = rep.leitor();
    let q = 20_000u64;
    let chegar = |i: u64| {
        let p = [0xfcu8, i as u8, 0, 0];
        alimentador.entregar(
            &QuadroDeAudio {
                payload: &p,
                timestamp_us: i * q,
                sequencia: i as u16,
                marca: true,
            },
            i * q + 5_000,
        );
    };
    // A primeira puxada, sem pacote nenhum: era a que alocava.
    let (_, primeira) = armada(|| {
        let _ = rep.puxar_em(0, 10_000, f64::NAN);
    });
    // Com fluxo, uma parada de 300 ms (salto), um transbordo (2 s sem puxar), e rajadas de 5.
    let mut depois = 0;
    let mut i = 0u64;
    for n in 1..3_000u64 {
        let h = n * q + 7_000;
        while i * q + 5_000 <= h {
            chegar(i);
            i += 1;
        }
        let parada = (1_000..1_015).contains(&n) || (2_000..2_100).contains(&n);
        if parada {
            continue;
        }
        let rajada = if (2_500..2_600).contains(&n) { 5 } else { 1 };
        for _ in 0..rajada {
            let (_, a) = armada(|| {
                let _ = rep.puxar_em(h, 10_000, f64::NAN);
            });
            depois += a;
        }
    }
    let c = leitor.contadores();
    eprintln!(
        "alocações+liberações na 1ª puxada: {primeira}; nas seguintes: {depois}; saltos={} \
         transbordos={}",
        c.saltos, c.transbordos
    );
    assert!(c.saltos >= 1 && c.transbordos >= 1, "o cenário tinha de passar pelos dois: {c:?}");
    assert_eq!(depois, 0, "as puxadas seguintes alocaram");
    assert_eq!(primeira, 0, "a PRIMEIRA puxada alocou {primeira} vez(es) nesta plataforma");
}
