//! Canal autenticado de sinalização v3. O resultado de PAKE já autenticado é
//! a única origem das chaves. A instância não é clonável: clonar counters e
//! chaves permitiria reutilizar nonce. Falhas invalidam o canal inteiro.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Error, Result};
use crate::pairing::Role;

pub const SECURE_VERSION: u16 = 3;
pub const MAX_PLAINTEXT: usize = 1024 * 1024;
const HEADER: usize = 11;
const TAG: usize = 16;
// Limite conservador por chave, separado do espaço total do contador.
const MAX_MESSAGES: u64 = 1 << 32;
const MAX_BYTES: u64 = 1 << 36;

#[derive(Zeroize, ZeroizeOnDrop)]
struct TrafficKey {
    key: [u8; 32],
    prefix: [u8; 4],
}

pub struct SecureChannel {
    send: TrafficKey,
    receive: TrafficKey,
    context: [u8; 32],
    send_direction: u8,
    receive_direction: u8,
    send_counter: u64,
    receive_counter: u64,
    send_bytes: u64,
    receive_bytes: u64,
    failed: bool,
}

impl core::fmt::Debug for SecureChannel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecureChannel")
            .field("failed", &self.failed)
            .finish()
    }
}

impl SecureChannel {
    pub(crate) fn from_session(role: Role, session_key: &[u8], context: &[u8]) -> Result<Self> {
        let context: [u8; 32] = Sha256::digest(context).into();
        let hk = Hkdf::<Sha256>::new(Some(&context), session_key);
        let key = |label: &[u8]| -> Result<TrafficKey> {
            let mut material = [0u8; 36];
            hk.expand(label, &mut material)
                .map_err(|_| Error::Pairing("derivação do canal recusada".into()))?;
            let mut key = [0u8; 32];
            let mut prefix = [0u8; 4];
            key.copy_from_slice(&material[..32]);
            prefix.copy_from_slice(&material[32..]);
            material.zeroize();
            Ok(TrafficKey { key, prefix })
        };
        let guest_to_host = key(b"quall/v3/channel/guest-to-host")?;
        let host_to_guest = key(b"quall/v3/channel/host-to-guest")?;
        let (send, receive, send_direction, receive_direction) = match role {
            Role::Guest => (guest_to_host, host_to_guest, 0, 1),
            Role::Host => (host_to_guest, guest_to_host, 1, 0),
        };
        Ok(Self {
            send,
            receive,
            context,
            send_direction,
            receive_direction,
            send_counter: 0,
            receive_counter: 0,
            send_bytes: 0,
            receive_bytes: 0,
            failed: false,
        })
    }

    fn reject<T>(&mut self) -> Result<T> {
        self.failed = true;
        self.send.zeroize();
        self.receive.zeroize();
        Err(Error::Pairing("canal seguro inválido ou esgotado".into()))
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if self.failed
            || plaintext.len() > MAX_PLAINTEXT
            || self.send_counter >= MAX_MESSAGES
            || self.send_bytes.saturating_add(plaintext.len() as u64) > MAX_BYTES
        {
            return self.reject();
        }
        let header = header(self.send_direction, self.send_counter);
        let nonce = nonce(&self.send.prefix, self.send_counter);
        let aad = aad(&self.context, &header);
        let cipher = ChaCha20Poly1305::new((&self.send.key).into());
        let encrypted = match cipher.encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        ) {
            Ok(v) => v,
            Err(_) => return self.reject(),
        };
        let mut envelope = Vec::with_capacity(HEADER + encrypted.len());
        envelope.extend_from_slice(&header);
        envelope.extend_from_slice(&encrypted);
        self.send_counter += 1;
        self.send_bytes += plaintext.len() as u64;
        Ok(envelope)
    }

    pub fn open(&mut self, envelope: &[u8]) -> Result<Vec<u8>> {
        if self.failed
            || envelope.len() < HEADER + TAG
            || envelope.len() > HEADER + TAG + MAX_PLAINTEXT
            || self.receive_counter >= MAX_MESSAGES
        {
            return self.reject();
        }
        let expected = header(self.receive_direction, self.receive_counter);
        if envelope[..HEADER] != expected {
            return self.reject();
        }
        let length = (envelope.len() - HEADER - TAG) as u64;
        if self.receive_bytes.saturating_add(length) > MAX_BYTES {
            return self.reject();
        }
        let nonce = nonce(&self.receive.prefix, self.receive_counter);
        let aad = aad(&self.context, &expected);
        let cipher = ChaCha20Poly1305::new((&self.receive.key).into());
        let plaintext = match cipher.decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &envelope[HEADER..],
                aad: &aad,
            },
        ) {
            Ok(v) => v,
            Err(_) => return self.reject(),
        };
        self.receive_counter += 1;
        self.receive_bytes += length;
        Ok(plaintext)
    }
}

fn header(direction: u8, counter: u64) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..2].copy_from_slice(&SECURE_VERSION.to_le_bytes());
    h[2] = direction;
    h[3..].copy_from_slice(&counter.to_le_bytes());
    h
}

fn nonce(prefix: &[u8; 4], counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(prefix);
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

fn aad(context: &[u8; 32], header: &[u8; HEADER]) -> [u8; 32 + HEADER] {
    let mut aad = [0u8; 32 + HEADER];
    aad[..32].copy_from_slice(context);
    aad[32..].copy_from_slice(header);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;
    fn channels(context: &[u8]) -> (SecureChannel, SecureChannel) {
        (
            SecureChannel::from_session(Role::Guest, &[42; 64], context).unwrap(),
            SecureChannel::from_session(Role::Host, &[42; 64], context).unwrap(),
        )
    }

    #[test]
    fn direcoes_cifradas_independentes_e_counters() {
        let (mut guest, mut host) = channels(b"ctx");
        let g = guest.seal(b"private-id").unwrap();
        let h = host.seal(b"private-id").unwrap();
        assert_ne!(g, h);
        assert!(!g.windows(10).any(|s| s == b"private-id"));
        assert_eq!(host.open(&g).unwrap(), b"private-id");
        assert_eq!(guest.open(&h).unwrap(), b"private-id");
        let next = guest.seal(b"second").unwrap();
        assert_eq!(host.open(&next).unwrap(), b"second");
    }

    #[test]
    fn replay_tamper_reflection_e_downgrade_invalidam_canal() {
        for mode in 0..4 {
            let (mut guest, mut host) = channels(b"ctx");
            let mut frame = guest.seal(b"secret").unwrap();
            match mode {
                0 => {
                    host.open(&frame).unwrap();
                }
                1 => {
                    *frame.last_mut().unwrap() ^= 1;
                }
                2 => {
                    frame = host.seal(b"reflection").unwrap();
                }
                3 => {
                    frame[0] = 2;
                }
                _ => unreachable!(),
            }
            assert!(host.open(&frame).is_err());
            assert!(host.seal(b"after failure").is_err());
        }
    }

    #[test]
    fn outra_sessao_contexto_ordem_e_limites_falham() {
        let (mut guest, mut host) = channels(b"ctx");
        let _first = guest.seal(b"first").unwrap();
        let second = guest.seal(b"second").unwrap();
        assert!(host.open(&second).is_err());
        let (mut guest, _) = channels(b"ctx");
        let mut other = SecureChannel::from_session(Role::Host, &[42; 64], b"other").unwrap();
        assert!(other.open(&guest.seal(b"first").unwrap()).is_err());
        let (mut guest, _) = channels(b"ctx");
        guest.send_counter = MAX_MESSAGES;
        assert!(guest.seal(b"overflow").is_err());
    }
}
