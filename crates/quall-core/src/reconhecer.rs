//! **Quem da lista é um par conhecido** (`docs/v3-no-privado.md` §5.4, P3).
//!
//! No v3 o anúncio não diz o id de ninguém: o `device_id` da lista é o token do anunciante. Para a
//! casca mostrar o nome local de um par que ela guardou — e conectar com o alvo, para a A2 valer a
//! partir da lista —, o núcleo abre a sinalização com o item, manda um `Probe` e lê as dicas de
//! retomada do `Challenge`: se a dica de um par guardado está lá, é ele. A conexão fecha **antes
//! do `Hello`**: nada de PAKE, nenhum PIN gasto lá (M4), e o anfitrião só vê um candidato que
//! desistiu.
//!
//! # O que vaza, e para quem
//!
//! O `Probe` leva só um nonce e o papel funcional; as dicas são HMAC do segredo do par sobre o
//! contexto da troca. Um terceiro na rede não aprende quem é par de quem (as dicas mudam a cada
//! nonce), e o anfitrião não aprende quem sondou. O resultado fica **neste processo**.
//!
//! # Custo
//!
//! Uma conexão por item **novo** da lista que serve ao papel deste aparelho (chave: token com endereço), no máximo
//! [`SONDAS_DE_UMA_VEZ`] ao mesmo tempo, cada uma com prazo de [`PRAZO_DA_SONDA`]. Um item já
//! sondado não é sondado de novo; o que não respondeu volta depois de [`RECUO_DA_SONDA`]. Sem par
//! guardado, não sonda nada.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::busca::AparelhoVisto;
use crate::cancel::Cancelamento;
use crate::error::{Error, Result};
use crate::pairing::{PairFrame, PairedPeers, Pairing};
use crate::protocol::{papel_do_anfitriao_serve, DeviceId, Papel};
use crate::signaling::{connect_de, SignalMessage};

/// O prazo de uma sonda, da conexão ao `Challenge`.
pub const PRAZO_DA_SONDA: Duration = Duration::from_millis(2500);
/// Um item que não respondeu a sonda é tentado de novo depois disto.
pub const RECUO_DA_SONDA: Duration = Duration::from_secs(30);
/// Quantas sondas andam ao mesmo tempo.
pub const SONDAS_DE_UMA_VEZ: usize = 2;
/// Teto do cache (uma rede de casa não tem tantos itens; isto só segura o crescimento).
const CACHE_NO_MAXIMO: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Estado {
    Sondando,
    Conhecido(DeviceId),
    Desconhecido,
    /// Não respondeu: tenta de novo depois do recuo.
    Falhou(Instant),
}

/// O reconhecimento dos itens da lista contra a tabela de pares da casca.
pub struct Reconhecedor {
    eu: DeviceId,
    papel: u8,
    /// O papel deste aparelho, para não sondar quem não serve a ele (um prompter, para o vídeo).
    papel_local: Option<Papel>,
    pares: PairedPeers,
    cache: Arc<Mutex<HashMap<String, Estado>>>,
    em_voo: Arc<AtomicUsize>,
    cancelamento: Cancelamento,
}

fn travar<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Reconhecedor {
    /// `eu` é o id deste aparelho, `papel` o funcional dele (o mesmo da conexão), `pares` a tabela
    /// que a casca guarda.
    pub fn novo(eu: DeviceId, papel: Option<Papel>, pares: PairedPeers) -> Result<Self> {
        let papel_local = papel;
        let papel = match papel {
            None => 0,
            Some(Papel::Teleprompter) => 1,
            Some(Papel::ControleRemoto) => 2,
            Some(Papel::Desconhecido) => return Err(Error::Invalid("papel desconhecido".into())),
        };
        // Sem id válido, o `Pairing` recusaria cada sonda; melhor dizer agora.
        Pairing::guest(eu.clone(), None, &PairedPeers::new(), papel)?;
        Ok(Self {
            eu,
            papel,
            papel_local,
            pares,
            cache: Arc::default(),
            em_voo: Arc::default(),
            cancelamento: Cancelamento::novo(),
        })
    }

    /// Preenche [`AparelhoVisto::conhecido`] pelo cache e põe para sondar os itens novos (vivos,
    /// com endereço, que não são deste processo). Não bloqueia.
    pub fn rotular(&self, itens: &mut [AparelhoVisto]) {
        if !self.pares.has_secure_peers() {
            return;
        }
        let agora = Instant::now();
        for item in itens.iter_mut() {
            let estado = travar(&self.cache).get(&item.chave).cloned();
            match estado {
                Some(Estado::Conhecido(id)) => item.conhecido = Some(id),
                Some(Estado::Sondando) | Some(Estado::Desconhecido) => {}
                Some(Estado::Falhou(quando)) if agora < quando + RECUO_DA_SONDA => {}
                _ => {
                    let Some(destino) = item.endpoint else { continue };
                    // Nem o endereço que a pessoa acabou de tocar (`docs/v3-no-privado.md` §8.9): a
                    // sonda disputaria o toque, e com o celular cochilando envenenaria o ARP dele.
                    if !item.vivo
                        || item.proprio
                        || crate::busca::tocado_ha_pouco(destino.ip())
                        || papel_do_anfitriao_serve(self.papel_local, item.anuncio.papel).is_err()
                    {
                        continue;
                    }
                    self.sondar(item.chave.clone(), destino);
                }
            }
        }
    }

    fn sondar(&self, chave: String, destino: SocketAddr) {
        // Reserva uma vaga; sem vaga, o item espera a próxima atualização da lista.
        if self
            .em_voo
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < SONDAS_DE_UMA_VEZ).then_some(n + 1)
            })
            .is_err()
        {
            return;
        }
        {
            let mut cache = travar(&self.cache);
            if cache.len() >= CACHE_NO_MAXIMO {
                cache.retain(|_, e| matches!(e, Estado::Sondando | Estado::Conhecido(_)));
            }
            cache.insert(chave.clone(), Estado::Sondando);
        }
        let (eu, papel, pares) = (self.eu.clone(), self.papel, self.pares.clone());
        let chave_da_vaga = chave.clone();
        let (cache, em_voo, cancelamento) =
            (self.cache.clone(), self.em_voo.clone(), self.cancelamento.clone());
        let disparou = std::thread::Builder::new().name("quall-reconhecer".into()).spawn(move || {
            let estado = match quem_e(destino, eu, papel, &pares, PRAZO_DA_SONDA, &cancelamento) {
                Ok(Some(id)) => Estado::Conhecido(id),
                Ok(None) => Estado::Desconhecido,
                Err(_) => Estado::Falhou(Instant::now()),
            };
            travar(&cache).insert(chave, estado);
            em_voo.fetch_sub(1, Ordering::AcqRel);
        });
        if disparou.is_err() {
            self.em_voo.fetch_sub(1, Ordering::AcqRel);
            travar(&self.cache).remove(&chave_da_vaga);
        }
    }
}

impl Drop for Reconhecedor {
    fn drop(&mut self) {
        // As sondas em voo desistem; o resultado delas cai num cache que ninguém mais lê.
        self.cancelamento.cancelar();
    }
}

/// **A sonda**: conecta em `destino`, manda o `Probe`, lê o `Challenge` e diz qual par guardado
/// tem a dica nele. Fecha antes do `Hello`.
pub fn quem_e(
    destino: SocketAddr,
    eu: DeviceId,
    papel: u8,
    pares: &PairedPeers,
    prazo: Duration,
    cancelamento: &Cancelamento,
) -> Result<Option<DeviceId>> {
    let fim = Instant::now() + prazo;
    let mut maquina = Pairing::guest(eu, None, pares, papel)?;
    let mut link = connect_de(destino, prazo, None, cancelamento)?;
    let resposta = (|| {
        link.send(&SignalMessage::Pair(maquina.open()?))?;
        loop {
            if cancelamento.cancelado() {
                return Err(Error::Cancelled);
            }
            if Instant::now() >= fim {
                return Err(Error::Timeout("a sonda não recebeu o desafio".into()));
            }
            match link.poll()? {
                Some(SignalMessage::Pair(quadro @ PairFrame::Challenge { .. })) => {
                    return maquina.quem_casa_no_desafio(&quadro);
                }
                Some(SignalMessage::Error { motivo, causa }) => return Err(causa.erro(motivo)),
                Some(SignalMessage::Bye { motivo }) => return Err(Error::Signaling(motivo)),
                Some(_) => return Err(Error::Protocol("esperava o desafio".into())),
                None => {}
            }
        }
    })();
    link.close("só reconhecendo");
    resposta
}
