//! Pareamento v3 por OPAQUE-3DH (RFC 9807), seguido de confirmação bilateral
//! cifrada. Nenhuma identidade persistente participa das mensagens públicas.
//!
//! O registro OPAQUE é criado localmente no Host, que já conhece o PIN/segredo;
//! só o login atravessa a LAN. A biblioteca tem avaliação histórica de 2021,
//! que não equivale a auditoria desta versão ou desta composição.
//! PIN curto continua exigindo limite online por espera, inclusive quando um
//! candidato fecha depois de receber KE2. O protocolo antigo DH+PIN não é PAKE
//! e não é aceito como fallback. Vínculos antigos são preservados para novo PIN.

use crate::error::{hex_decode, hex_encode, Error, Result};
use crate::protocol::DeviceId;
use crate::secure_channel::{SecureChannel, SECURE_VERSION};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use opaque_ke::{
    CipherSuite, ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, Identifiers, ServerLogin, ServerLoginParameters, ServerRegistration,
    ServerSetup,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::collections::HashMap;
use std::sync::Mutex;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

type HmacSha256 = Hmac<Sha256>;
pub const PIN_DIGITS: usize = 6;
pub const PAIR_SECRET_LEN: usize = 32;
const NONCE_LEN: usize = 16;
const RESUME_SLOTS: usize = 128;
const MAX_HANDSHAKE_HEX: usize = 8192;
const MAX_ID_LEN: usize = 256;

struct Suite;
impl CipherSuite for Suite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, Sha512>;
    type Ksf = ModeKsf;
}

// Somente uma área de Argon2 por processo, inclusive entre hosts paralelos.
// Não há redução negociável de parâmetros e poison falha fechado.
static ARGON2_GATE: Mutex<()> = Mutex::new(());
#[derive(Default)]
enum ModeKsf {
    #[default]
    Pin,
    Resume,
}
impl opaque_ke::ksf::Ksf for ModeKsf {
    fn hash<L: opaque_ke::generic_array::ArrayLength<u8>>(
        &self,
        input: opaque_ke::generic_array::GenericArray<u8, L>,
    ) -> std::result::Result<
        opaque_ke::generic_array::GenericArray<u8, L>,
        opaque_ke::errors::InternalError,
    > {
        use opaque_ke::errors::InternalError;
        match self {
            Self::Resume => opaque_ke::ksf::Identity.hash(input),
            Self::Pin => {
                let _guard = ARGON2_GATE.lock().map_err(|_| InternalError::KsfError)?;
                let input = Zeroizing::new(input);
                let params = argon2::Params::new(19 * 1024, 2, 1, None)
                    .map_err(|_| InternalError::KsfError)?;
                let argon = argon2::Argon2::new(
                    argon2::Algorithm::Argon2id,
                    argon2::Version::V0x13,
                    params,
                );
                let mut blocks = Vec::new();
                blocks
                    .try_reserve_exact(19 * 1024)
                    .map_err(|_| InternalError::KsfError)?;
                blocks.resize(19 * 1024, argon2::Block::default());
                let mut blocks = Zeroizing::new(blocks);
                let mut output = Zeroizing::new(opaque_ke::generic_array::GenericArray::default());
                argon
                    .hash_password_into_with_memory(
                        &input,
                        &[0; argon2::RECOMMENDED_SALT_LEN],
                        &mut output,
                        blocks.as_mut_slice(),
                    )
                    .map_err(|_| InternalError::KsfError)?;
                Ok((*output).clone())
            }
        }
    }
}
impl PairMode {
    fn ksf(self) -> ModeKsf {
        match self {
            Self::Pin => ModeKsf::Pin,
            Self::Resume => ModeKsf::Resume,
        }
    }
}

/// PIN mostrado pelo emissor e digitado no receptor.
///
/// Guarda os dígitos, não o texto: `"012345"` e `"12345"` são coisas diferentes e confundir os
/// dois é o tipo de bug que só aparece um em dez pareamentos.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct Pin([u8; PIN_DIGITS]);

impl Pin {
    /// Sorteia um PIN novo com o gerador do sistema.
    ///
    /// A rejeição de amostra evita o viés de `% 10` sobre um byte: sem ela, os dígitos 0–5
    /// sairiam mais que 6–9, e um PIN enviesado é um PIN menor do que aparenta.
    pub fn generate() -> Result<Self> {
        let mut digitos = [0u8; PIN_DIGITS];
        let mut preenchidos = 0;
        let mut bruto = [0u8; PIN_DIGITS * 2];
        while preenchidos < PIN_DIGITS {
            getrandom::fill(&mut bruto)
                .map_err(|e| Error::Pairing(format!("sem entropia do sistema: {e}")))?;
            for b in bruto {
                if preenchidos == PIN_DIGITS {
                    break;
                }
                if b < 250 {
                    digitos[preenchidos] = b % 10;
                    preenchidos += 1;
                }
            }
        }
        Ok(Pin(digitos))
    }

    /// Lê o que o usuário digitou. Espaços e traços são ignorados — gente separa PIN em grupos.
    pub fn parse(entrada: &str) -> Result<Self> {
        let mut digitos = [0u8; PIN_DIGITS];
        let mut n = 0;
        for c in entrada.chars() {
            if c == ' ' || c == '-' || c == '.' {
                continue;
            }
            let Some(d) = c.to_digit(10) else {
                return Err(Error::Pairing(format!("'{c}' não é dígito")));
            };
            if n == PIN_DIGITS {
                return Err(Error::Pairing(format!("o PIN tem {PIN_DIGITS} dígitos")));
            }
            digitos[n] = d as u8;
            n += 1;
        }
        if n != PIN_DIGITS {
            return Err(Error::Pairing(format!("o PIN tem {PIN_DIGITS} dígitos")));
        }
        Ok(Pin(digitos))
    }

    /// Como mostrar na tela.
    pub fn to_display(&self) -> String {
        self.0.iter().map(|d| (b'0' + d) as char).collect()
    }

    fn bytes(&self) -> Zeroizing<[u8; PIN_DIGITS]> {
        Zeroizing::new(self.0)
    }
}

// `Debug` que não vaza o PIN em log. Um PIN em `logcat` é um PIN público.
impl core::fmt::Debug for Pin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Pin(******)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Guest,
}

#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct KnownPeer {
    #[zeroize(skip)]
    pub id: DeviceId,
    pub secret: [u8; PAIR_SECRET_LEN],
}
impl core::fmt::Debug for KnownPeer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("KnownPeer([oculto])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairMode {
    Pin,
    Resume,
}
impl PairMode {
    fn code(self) -> u8 {
        match self {
            Self::Pin => 0,
            Self::Resume => 1,
        }
    }
}

/// Mensagens públicas contêm material PAKE/aleatório. A identidade nas duas
/// confirmações está dentro de AEAD, com counters que são transferidos ao Link.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "p", rename_all = "snake_case", deny_unknown_fields)]
pub enum PairFrame {
    Probe {
        version: u16,
        guest_nonce: String,
        guest_role: u8,
    },
    Challenge {
        version: u16,
        token: String,
        host_nonce: String,
        host_role: u8,
        resume_hints: Vec<String>,
    },
    Hello {
        mode: PairMode,
        hint: Option<String>,
        ke1: String,
    },
    Ack {
        ke2: String,
    },
    Confirm {
        ke3: String,
        identity_ciphertext: String,
    },
    ConfirmAck {
        identity_ciphertext: String,
    },
    NeedsPin,
    Fail {
        motivo: String,
    },
}
impl core::fmt::Debug for PairFrame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Probe { .. } => "Probe",
            Self::Challenge { .. } => "Challenge",
            Self::Hello { .. } => "Hello",
            Self::Ack { .. } => "Ack",
            Self::Confirm { .. } => "Confirm",
            Self::ConfirmAck { .. } => "ConfirmAck",
            Self::NeedsPin => "NeedsPin",
            Self::Fail { .. } => "Fail",
        })
    }
}

#[derive(Debug)]
pub struct Step {
    pub reply: Option<PairFrame>,
    pub done: Option<PairOutcome>,
}
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct PairOutcome {
    #[zeroize(skip)]
    pub peer: DeviceId,
    pub secret: [u8; PAIR_SECRET_LEN],
    pub novo: bool,
}
impl core::fmt::Debug for PairOutcome {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PairOutcome")
            .field("peer", &"[oculto]")
            .field("secret", &"[oculto]")
            .field("novo", &self.novo)
            .finish()
    }
}

#[derive(Clone)]
struct Context {
    token: [u8; NONCE_LEN],
    guest_nonce: [u8; NONCE_LEN],
    host_nonce: [u8; NONCE_LEN],
    guest_role: u8,
    host_role: u8,
}
impl Context {
    fn bytes(&self, mode: Option<PairMode>) -> Vec<u8> {
        let mut b = b"quall/opaque-3dh-ristretto255-sha512/v3".to_vec();
        b.extend_from_slice(&SECURE_VERSION.to_le_bytes());
        b.extend_from_slice(&[self.guest_role, self.host_role]);
        b.extend_from_slice(&self.token);
        b.extend_from_slice(&self.guest_nonce);
        b.extend_from_slice(&self.host_nonce);
        if let Some(mode) = mode {
            b.push(mode.code());
            b.extend_from_slice(match mode {
                PairMode::Pin => b"ksf=argon2id-v19-m19456-t2-p1".as_slice(),
                PairMode::Resume => b"ksf=identity-secret32-v3".as_slice(),
            });
        }
        b
    }
}

enum State {
    Initial,
    GuestChallenge,
    HostHello,
    GuestAck(ClientLogin<Suite>),
    HostConfirm(ServerLogin<Suite>),
    GuestConfirmAck,
    Done,
    Failed,
}

/// Máquina sem I/O. A aplicação deve consumir a tentativa após KE1 aceito,
/// impor deadline/teto de conexões e encerrar em qualquer erro de autenticação.
pub struct Pairing {
    role: Role,
    eu: DeviceId,
    pin: Option<Pin>,
    known: Vec<KnownPeer>,
    selected: Option<KnownPeer>,
    password: Option<Zeroizing<Vec<u8>>>,
    local_role: Option<u8>,
    nonce: [u8; NONCE_LEN],
    token: [u8; NONCE_LEN],
    context: Option<Context>,
    mode: Option<PairMode>,
    state: State,
    channel: Option<SecureChannel>,
    pair_secret: Zeroizing<[u8; PAIR_SECRET_LEN]>,
    attempted: bool,
}
impl Pairing {
    pub fn new(
        role: Role,
        eu: DeviceId,
        pin: Option<Pin>,
        conhecido: Option<KnownPeer>,
    ) -> Result<Self> {
        validate_id(&eu)?;
        Ok(Self {
            role,
            eu,
            pin,
            known: conhecido.into_iter().collect(),
            selected: None,
            password: None,
            local_role: None,
            nonce: random()?,
            token: random()?,
            context: None,
            mode: None,
            state: State::Initial,
            channel: None,
            pair_secret: Zeroizing::new([0; PAIR_SECRET_LEN]),
            attempted: false,
        })
    }
    pub fn new_with_store(
        role: Role,
        eu: DeviceId,
        pin: Option<Pin>,
        store: &PairedPeers,
    ) -> Result<Self> {
        let mut p = Self::new(role, eu, pin, None)?;
        p.known = store.eligible();
        Ok(p)
    }
    pub fn with_store(
        role: Role,
        eu: DeviceId,
        pin: Option<Pin>,
        store: &PairedPeers,
        _primeiro: &PairFrame,
    ) -> Result<Self> {
        Self::new_with_store(role, eu, pin, store)
    }
    pub fn guest(
        eu: DeviceId,
        pin: Option<Pin>,
        store: &PairedPeers,
        local_role: u8,
    ) -> Result<Self> {
        let mut p = Self::new_with_store(Role::Guest, eu, pin, store)?;
        p.bind_local_role(local_role)?;
        Ok(p)
    }
    pub fn host(
        eu: DeviceId,
        pin: Option<Pin>,
        store: &PairedPeers,
        token: &str,
        local_role: u8,
    ) -> Result<Self> {
        let mut p = Self::new_with_store(Role::Host, eu, pin, store)?;
        p.token = hex_decode(token).map_err(|_| invalid())?;
        p.bind_local_role(local_role)?;
        Ok(p)
    }
    pub fn bind_local_role(&mut self, role: u8) -> Result<()> {
        if !matches!(self.state, State::Initial) || self.local_role.is_some() {
            return Err(invalid());
        }
        validate_role(role)?;
        self.local_role = Some(role);
        Ok(())
    }
    pub fn peer_role(&self) -> Result<u8> {
        if !self.is_done() {
            return Err(invalid());
        }
        let ctx = self.context.as_ref().ok_or_else(invalid)?;
        Ok(match self.role {
            Role::Host => ctx.guest_role,
            Role::Guest => ctx.host_role,
        })
    }
    pub fn attempt_started(&self) -> bool {
        self.attempted
    }
    pub fn is_done(&self) -> bool {
        matches!(self.state, State::Done)
    }
    pub fn take_secure_channel(&mut self) -> Result<SecureChannel> {
        if !self.is_done() {
            return Err(invalid());
        }
        self.channel.take().ok_or_else(invalid)
    }
    pub fn open(&mut self) -> Result<PairFrame> {
        if self.role != Role::Guest || !matches!(self.state, State::Initial) {
            return Err(invalid());
        }
        let guest_role = self.local_role.ok_or_else(invalid)?;
        self.state = State::GuestChallenge;
        Ok(PairFrame::Probe {
            version: SECURE_VERSION,
            guest_nonce: hex_encode(&self.nonce),
            guest_role,
        })
    }
    pub fn step(&mut self, frame: PairFrame) -> Result<Step> {
        let result = self.step_inner(frame);
        if result.is_err() {
            self.state = State::Failed;
            self.clear_sensitive();
            self.channel = None;
        }
        result
    }
    fn clear_sensitive(&mut self) {
        self.pin = None;
        self.password = None;
        self.known.clear();
        self.selected = None;
        self.pair_secret.zeroize();
    }
    fn step_inner(&mut self, frame: PairFrame) -> Result<Step> {
        let local_role = self.local_role.ok_or_else(invalid)?;
        if matches!(frame, PairFrame::NeedsPin) {
            return Err(Error::NeedsPin(
                "pareamento atualizado requer PIN explícito".into(),
            ));
        }
        if matches!(frame, PairFrame::Fail { .. }) {
            return Err(invalid());
        }
        let state = core::mem::replace(&mut self.state, State::Failed);
        match (self.role, state, frame) {
            (
                Role::Host,
                State::Initial,
                PairFrame::Probe {
                    version,
                    guest_nonce,
                    guest_role,
                },
            ) => {
                validate_version(version)?;
                validate_role(guest_role)?;
                let context = Context {
                    token: self.token,
                    guest_nonce: hex_decode(&guest_nonce).map_err(|_| invalid())?,
                    host_nonce: self.nonce,
                    guest_role,
                    host_role: local_role,
                };
                let mut hints = Vec::with_capacity(RESUME_SLOTS);
                for known in &self.known {
                    hints.push(hex_encode(&resume_hint(&known.secret, &context)?));
                }
                while hints.len() < RESUME_SLOTS {
                    hints.push(hex_encode(&random::<32>()?));
                }
                self.context = Some(context);
                self.state = State::HostHello;
                Ok(Step {
                    reply: Some(PairFrame::Challenge {
                        version: SECURE_VERSION,
                        token: hex_encode(&self.token),
                        host_nonce: hex_encode(&self.nonce),
                        host_role: local_role,
                        resume_hints: hints,
                    }),
                    done: None,
                })
            }
            (
                Role::Guest,
                State::GuestChallenge,
                PairFrame::Challenge {
                    version,
                    token,
                    host_nonce,
                    host_role,
                    resume_hints,
                },
            ) => {
                validate_version(version)?;
                validate_role(host_role)?;
                if resume_hints.len() != RESUME_SLOTS {
                    return Err(invalid());
                }
                let context = Context {
                    token: hex_decode(&token).map_err(|_| invalid())?,
                    guest_nonce: self.nonce,
                    host_nonce: hex_decode(&host_nonce).map_err(|_| invalid())?,
                    guest_role: local_role,
                    host_role,
                };
                let hints: Vec<[u8; 32]> = resume_hints
                    .iter()
                    .map(|h| hex_decode(h).map_err(|_| invalid()))
                    .collect::<Result<_>>()?;
                let (mode, hint, password) = if let Some(pin) = &self.pin {
                    (PairMode::Pin, None, Zeroizing::new(pin.bytes().to_vec()))
                } else {
                    let mut selected = None;
                    for known in &self.known {
                        let expected = resume_hint(&known.secret, &context)?;
                        let mut found = 0u8;
                        for candidate in &hints {
                            found |= expected.ct_eq(candidate).unwrap_u8();
                        }
                        if found == 1 && selected.is_none() {
                            selected = Some((known.clone(), expected));
                        }
                    }
                    let (known, hint) = selected.ok_or_else(|| {
                        Error::NeedsPin("novo PIN necessário para este vínculo".into())
                    })?;
                    let password = Zeroizing::new(known.secret.to_vec());
                    self.selected = Some(known);
                    (PairMode::Resume, Some(hex_encode(&hint)), password)
                };
                let login =
                    ClientLogin::<Suite>::start(&mut OsRng, &password).map_err(|_| invalid())?;
                self.context = Some(context);
                self.mode = Some(mode);
                self.password = Some(password);
                self.pin = None;
                self.known.clear();
                self.attempted = true;
                self.state = State::GuestAck(login.state);
                Ok(Step {
                    reply: Some(PairFrame::Hello {
                        mode,
                        hint,
                        ke1: hex_encode(&login.message.serialize()),
                    }),
                    done: None,
                })
            }
            (Role::Host, State::HostHello, PairFrame::Hello { mode, hint, ke1 }) => {
                let context = self.context.as_ref().ok_or_else(invalid)?;
                let password = match mode {
                    PairMode::Pin => {
                        if hint.is_some() {
                            return Err(invalid());
                        }
                        Zeroizing::new(
                            self.pin
                                .as_ref()
                                .ok_or_else(|| Error::NeedsPin("PIN não ativo".into()))?
                                .bytes()
                                .to_vec(),
                        )
                    }
                    PairMode::Resume => {
                        let received: [u8; 32] = hex_decode(hint.as_deref().ok_or_else(invalid)?)
                            .map_err(|_| invalid())?;
                        let mut selected = None;
                        for known in &self.known {
                            if resume_hint(&known.secret, context)?
                                .ct_eq(&received)
                                .unwrap_u8()
                                == 1
                                && selected.is_none()
                            {
                                selected = Some(known.clone());
                            }
                        }
                        let selected = selected
                            .ok_or_else(|| Error::NeedsPin("vínculo não reconhecido".into()))?;
                        let password = Zeroizing::new(selected.secret.to_vec());
                        self.selected = Some(selected);
                        password
                    }
                };
                let ke1 = CredentialRequest::<Suite>::deserialize(&decode(&ke1)?)
                    .map_err(|_| invalid())?;
                let binding = context.bytes(Some(mode));
                self.attempted = true;
                let setup = ServerSetup::<Suite>::new(&mut OsRng);
                // Registro somente local: seus dados nunca entram em PairFrame.
                let registration = ClientRegistration::<Suite>::start(&mut OsRng, &password)
                    .map_err(|_| invalid())?;
                let response =
                    ServerRegistration::<Suite>::start(&setup, registration.message, &binding)
                        .map_err(|_| invalid())?;
                let mut record = registration
                    .state
                    .finish(
                        &mut OsRng,
                        &password,
                        response.message,
                        ClientRegistrationFinishParameters {
                            identifiers: identifiers(),
                            ksf: Some(&mode.ksf()),
                        },
                    )
                    .map_err(|_| invalid())?;
                record.export_key.zeroize();
                let file = ServerRegistration::<Suite>::finish(record.message);
                let login = ServerLogin::<Suite>::start(
                    &mut OsRng,
                    &setup,
                    Some(file),
                    ke1,
                    &binding,
                    ServerLoginParameters {
                        context: Some(&binding),
                        identifiers: identifiers(),
                    },
                )
                .map_err(|_| invalid())?;
                self.mode = Some(mode);
                self.pin = None;
                self.known.clear();
                self.state = State::HostConfirm(login.state);
                Ok(Step {
                    reply: Some(PairFrame::Ack {
                        ke2: hex_encode(&login.message.serialize()),
                    }),
                    done: None,
                })
            }
            (Role::Guest, State::GuestAck(login), PairFrame::Ack { ke2 }) => {
                let mode = self.mode.ok_or_else(invalid)?;
                let binding = self.context.as_ref().ok_or_else(invalid)?.bytes(Some(mode));
                let password = self.password.take().ok_or_else(invalid)?;
                let response = CredentialResponse::<Suite>::deserialize(&decode(&ke2)?)
                    .map_err(|_| invalid())?;
                let mut result = login
                    .finish(
                        &mut OsRng,
                        &password,
                        response,
                        ClientLoginFinishParameters {
                            context: Some(&binding),
                            identifiers: identifiers(),
                            ksf: Some(&mode.ksf()),
                        },
                    )
                    .map_err(|_| authentication_error(mode))?;
                result.export_key.zeroize();
                let session_key = Zeroizing::new(result.session_key);
                self.install_keys(&session_key, &binding, mode)?;
                let identity_ciphertext = hex_encode(
                    &self
                        .channel
                        .as_mut()
                        .ok_or_else(invalid)?
                        .seal(self.eu.0.as_bytes())?,
                );
                self.state = State::GuestConfirmAck;
                Ok(Step {
                    reply: Some(PairFrame::Confirm {
                        ke3: hex_encode(&result.message.serialize()),
                        identity_ciphertext,
                    }),
                    done: None,
                })
            }
            (
                Role::Host,
                State::HostConfirm(login),
                PairFrame::Confirm {
                    ke3,
                    identity_ciphertext,
                },
            ) => {
                let mode = self.mode.ok_or_else(invalid)?;
                let binding = self.context.as_ref().ok_or_else(invalid)?.bytes(Some(mode));
                let finalization = CredentialFinalization::<Suite>::deserialize(&decode(&ke3)?)
                    .map_err(|_| invalid())?;
                let result = login
                    .finish(
                        finalization,
                        ServerLoginParameters {
                            context: Some(&binding),
                            identifiers: identifiers(),
                        },
                    )
                    .map_err(|_| authentication_error(mode))?;
                let session_key = Zeroizing::new(result.session_key);
                self.install_keys(&session_key, &binding, mode)?;
                let peer = self.read_identity(&identity_ciphertext)?;
                let identity_ciphertext = hex_encode(
                    &self
                        .channel
                        .as_mut()
                        .ok_or_else(invalid)?
                        .seal(self.eu.0.as_bytes())?,
                );
                let outcome = self.finish(peer, mode);
                Ok(Step {
                    reply: Some(PairFrame::ConfirmAck {
                        identity_ciphertext,
                    }),
                    done: Some(outcome),
                })
            }
            (
                Role::Guest,
                State::GuestConfirmAck,
                PairFrame::ConfirmAck {
                    identity_ciphertext,
                },
            ) => {
                let peer = self.read_identity(&identity_ciphertext)?;
                let mode = self.mode.ok_or_else(invalid)?;
                let outcome = self.finish(peer, mode);
                Ok(Step {
                    reply: None,
                    done: Some(outcome),
                })
            }
            _ => Err(invalid()),
        }
    }
    fn install_keys(&mut self, session_key: &[u8], context: &[u8], mode: PairMode) -> Result<()> {
        self.channel = Some(SecureChannel::from_session(
            self.role,
            session_key,
            context,
        )?);
        if mode == PairMode::Resume {
            *self.pair_secret = self.selected.as_ref().ok_or_else(invalid)?.secret;
        } else {
            let salt = Sha256::digest(context);
            Hkdf::<Sha256>::new(Some(&salt), session_key)
                .expand(b"quall/v3/pair/long-term-secret", &mut *self.pair_secret)
                .map_err(|_| invalid())?;
        }
        Ok(())
    }
    fn read_identity(&mut self, ciphertext: &str) -> Result<DeviceId> {
        let bytes = self
            .channel
            .as_mut()
            .ok_or_else(invalid)?
            .open(&decode(ciphertext)?)?;
        let text = String::from_utf8(bytes).map_err(|_| invalid())?;
        let peer = DeviceId(text);
        validate_id(&peer)?;
        if peer == self.eu {
            return Err(invalid());
        }
        if let Some(selected) = &self.selected {
            if peer != selected.id {
                return Err(invalid());
            }
        }
        Ok(peer)
    }
    fn finish(&mut self, peer: DeviceId, mode: PairMode) -> PairOutcome {
        let outcome = PairOutcome {
            peer,
            secret: *self.pair_secret,
            novo: mode == PairMode::Pin,
        };
        self.clear_sensitive();
        self.state = State::Done;
        outcome
    }
}

fn identifiers() -> Identifiers<'static> {
    Identifiers {
        client: Some(b"quall/v3/guest"),
        server: Some(b"quall/v3/host"),
    }
}
fn invalid() -> Error {
    Error::Pairing("autenticação segura inválida".into())
}
fn authentication_error(mode: PairMode) -> Error {
    match mode {
        PairMode::Pin => Error::WrongPin("PIN ou autenticação não conferiu".into()),
        PairMode::Resume => invalid(),
    }
}
fn validate_version(version: u16) -> Result<()> {
    if version != SECURE_VERSION {
        Err(Error::Protocol(
            "atualize o Quall nos dois aparelhos".into(),
        ))
    } else {
        Ok(())
    }
}
fn validate_role(role: u8) -> Result<()> {
    if role <= 2 {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn validate_id(id: &DeviceId) -> Result<()> {
    if id.0.is_empty() || id.0.len() > MAX_ID_LEN || id.0.chars().any(char::is_control) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut value = [0u8; N];
    getrandom::fill(&mut value).map_err(|_| Error::Pairing("sem entropia do sistema".into()))?;
    Ok(value)
}
fn resume_hint(secret: &[u8; 32], context: &Context) -> Result<[u8; 32]> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret).map_err(|_| invalid())?;
    mac.update(b"quall/v3/resume-hint");
    mac.update(&context.bytes(None));
    Ok(mac.finalize().into_bytes().into())
}
fn decode(text: &str) -> Result<Vec<u8>> {
    if text.len() > MAX_HANDSHAKE_HEX || text.len() % 2 != 0 {
        return Err(invalid());
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for bytes in text.as_bytes().chunks_exact(2) {
        let digit = |b: u8| -> Result<u8> {
            match b {
                b'0'..=b'9' => Ok(b - b'0'),
                b'a'..=b'f' => Ok(b - b'a' + 10),
                b'A'..=b'F' => Ok(b - b'A' + 10),
                _ => Err(invalid()),
            }
        };
        out.push((digit(bytes[0])? << 4) | digit(bytes[1])?);
    }
    Ok(out)
}

/// Uma entrada da tabela de pares.
///
/// # Dois formatos, e o velho continua sendo lido
///
/// Até o M3 a entrada era só o segredo em hex. O carimbo entrou por causa da dívida 23: sem ele
/// não há como [`PairedPeers::merge`] decidir qual de dois segredos para a **mesma** chave é o
/// bom, e é exatamente esse o caso que quebra o produto.
///
/// `untagged` faz um arquivo antigo — string pura — continuar carregando sem conversão nenhuma.
/// O que **não** acontece é o contrário: um binário do núcleo anterior a esta mudança não lê o
/// formato com carimbo. Não é problema em campo, porque a casca e o núcleo vão no mesmo pacote,
/// mas está dito aqui para ninguém descobrir isso num downgrade.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(untagged)]
enum Entrada {
    /// Formato do M0 ao M3: só o segredo em hex.
    Simples(String),
    /// Formato com carimbo de quando o pareamento foi feito, em milissegundos desde a época.
    Datada {
        secret: String,
        updated_ms: u64,
        #[serde(default)]
        security_version: u16,
    },
}

impl Entrada {
    fn secret(&self) -> &str {
        match self {
            Entrada::Simples(s) => s,
            Entrada::Datada { secret, .. } => secret,
        }
    }

    /// Quando foi gravada. Entrada sem carimbo conta como a mais velha possível — que é o certo:
    /// ela vem de antes de existir carimbo.
    fn quando(&self) -> u64 {
        match self {
            Entrada::Simples(_) => 0,
            Entrada::Datada { updated_ms, .. } => *updated_ms,
        }
    }
}

/// Milissegundos desde a época, ou `0` se o relógio do sistema estiver antes dela.
fn agora_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Tabela de aparelhos pareados. A casca persiste; o núcleo não toca em disco.
///
/// Serializa como JSON com o segredo em hex. O arquivo é material de chave: quem persistir tem
/// de guardá-lo com permissão restrita (Keychain no Apple, Keystore no Android, DPAPI no
/// Windows) — o núcleo não tem como garantir isso e não finge que tem.
///
/// # A chave é só o `DeviceId`, e isso cobra uma fusão (dívida 23)
///
/// Um segredo por aparelho, sem sessão, sem origem e sem porta na chave. É o comportamento
/// **certo** e é o que dá de graça a promessa do fluxo: quem pareou por uma origem retoma por
/// outra sem digitar nada — no iOS, parear pela tela e depois usar a câmera não pede PIN.
///
/// O preço é que duas origens do mesmo aparelho — a extension e o app, no iOS — escrevem o mesmo
/// arquivo. Enquanto o núcleo só oferecia ler-modificar-escrever, uma atualização perdida
/// bastava para os dois lados ficarem com segredos diferentes sob a mesma chave, e a retomada
/// seguinte falhava duro. [`PairedPeers::merge`] existe para que a casca escreva **fundindo** o
/// que está em disco com o que ela tem na mão, em vez de sobrescrever.
///
/// Isto não dispensa a trava entre processos (`NSFileCoordinator` no iOS): reduz o estrago de
/// uma corrida perdida de "perde uma entrada" para "converge na mais recente". O que fecha o
/// caso é a volta ao PIN da dívida 22, que existe justamente para quando a convergência falhar.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PairedPeers {
    pares: HashMap<String, Entrada>,
}

impl core::fmt::Debug for PairedPeers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PairedPeers")
            .field("quantidade", &self.pares.len())
            .finish()
    }
}

impl PairedPeers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, peer: &DeviceId) -> Option<[u8; PAIR_SECRET_LEN]> {
        let entrada = self.pares.get(&peer.0)?;
        if !matches!(
            entrada,
            Entrada::Datada {
                security_version: SECURE_VERSION,
                ..
            }
        ) {
            return None;
        }
        hex_decode::<PAIR_SECRET_LEN>(entrada.secret()).ok()
    }

    /// O vínculo antigo permanece no arquivo, mas exige novo PIN explícito.
    pub fn requires_repair(&self, peer: &DeviceId) -> bool {
        self.pares.contains_key(&peer.0) && self.get(peer).is_none()
    }

    /// Há pelo menos um vínculo v3 válido que pode tentar retomada segura.
    /// Não conta entradas antigas, futuras ou com material malformado.
    pub fn has_secure_peers(&self) -> bool {
        self.pares.values().any(|entrada| {
            matches!(
                entrada,
                Entrada::Datada {
                    security_version: SECURE_VERSION,
                    ..
                }
            ) && hex_decode::<PAIR_SECRET_LEN>(entrada.secret()).is_ok()
        })
    }

    fn eligible(&self) -> Vec<KnownPeer> {
        let mut ids: Vec<_> = self.pares.keys().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| {
                let id = DeviceId(id.clone());
                self.get(&id).map(|secret| KnownPeer { id, secret })
            })
            .take(RESUME_SLOTS)
            .collect()
    }

    pub fn insert(&mut self, resultado: &PairOutcome) {
        self.pares.insert(
            resultado.peer.0.clone(),
            Entrada::Datada {
                secret: hex_encode(&resultado.secret),
                updated_ms: agora_ms(),
                security_version: SECURE_VERSION,
            },
        );
    }

    /// Esquece um par. É o que a casca oferece como "parear de novo" — ver a dívida 22.
    pub fn remove(&mut self, peer: &DeviceId) {
        self.pares.remove(&peer.0);
    }

    /// Funde `outro` nesta tabela: **união**, e na colisão vence o carimbo mais recente.
    ///
    /// # Por que "mais recente vence" é o certo, e não uma escolha qualquer
    ///
    /// O outro lado guarda **um** segredo por `DeviceId` e o último pareamento sobrescreve os
    /// anteriores. Então o segredo que o receptor tem é sempre o do pareamento mais recente —
    /// e é exatamente esse que esta regra preserva. Escolher o mais antigo, ou o "meu", faria as
    /// duas origens divergirem do receptor em vez de convergirem para ele.
    ///
    /// O relógio de parede pode andar para trás (NTP corrigindo o aparelho). Quando isso
    /// acontecer, a fusão escolhe errado e a retomada falha — caindo, de propósito, no caminho
    /// do PIN da dívida 22, que é a rede de segurança de tudo isto.
    pub fn merge(&mut self, outro: &PairedPeers) {
        for (id, entrada) in &outro.pares {
            let manter = match self.pares.get(id) {
                Some(minha) => {
                    let secure = |e: &Entrada| {
                        matches!(
                            e,
                            Entrada::Datada {
                                security_version: SECURE_VERSION,
                                ..
                            }
                        )
                    };
                    (secure(entrada) && !secure(minha))
                        || (secure(entrada) == secure(minha) && entrada.quando() > minha.quando())
                }
                None => true,
            };
            if manter {
                self.pares.insert(id.clone(), entrada.clone());
            }
        }
    }

    pub fn len(&self) -> usize {
        self.pares.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pares.is_empty()
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(texto: &str) -> Result<Self> {
        Ok(serde_json::from_str(texto)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn machines(
        pin_g: Option<Pin>,
        pin_h: Option<Pin>,
        known_g: Option<KnownPeer>,
        known_h: Option<KnownPeer>,
    ) -> (Pairing, Pairing) {
        let mut g = Pairing::new(
            Role::Guest,
            DeviceId("guest-private".into()),
            pin_g,
            known_g,
        )
        .unwrap();
        let mut h =
            Pairing::new(Role::Host, DeviceId("host-private".into()), pin_h, known_h).unwrap();
        g.bind_local_role(2).unwrap();
        h.bind_local_role(1).unwrap();
        (g, h)
    }
    fn exchange(g: &mut Pairing, h: &mut Pairing) -> Result<(PairOutcome, PairOutcome)> {
        let probe = g.open()?;
        let challenge = h.step(probe)?.reply.ok_or_else(invalid)?;
        let hello = g.step(challenge)?.reply.ok_or_else(invalid)?;
        let ack = h.step(hello)?.reply.ok_or_else(invalid)?;
        let confirm = g.step(ack)?.reply.ok_or_else(invalid)?;
        let done_h = h.step(confirm)?;
        let done_g = g.step(done_h.reply.ok_or_else(invalid)?)?;
        Ok((
            done_g.done.ok_or_else(invalid)?,
            done_h.done.ok_or_else(invalid)?,
        ))
    }
    fn pin() -> Pin {
        Pin::parse("012345").unwrap()
    }
    #[test]
    fn pin_mutuo_confirma_identidades_apenas_na_cifra_e_canal_transferido() {
        let (mut g, mut h) = machines(Some(pin()), Some(pin()), None, None);
        let probe = g.open().unwrap();
        let mut frames = vec![probe.clone()];
        let challenge = h.step(probe).unwrap().reply.unwrap();
        frames.push(challenge.clone());
        let hello = g.step(challenge).unwrap().reply.unwrap();
        frames.push(hello.clone());
        let ack = h.step(hello).unwrap().reply.unwrap();
        frames.push(ack.clone());
        let confirm = g.step(ack).unwrap().reply.unwrap();
        frames.push(confirm.clone());
        assert!(!g.is_done());
        let rh = h.step(confirm).unwrap();
        frames.push(rh.reply.clone().unwrap());
        let rg = g.step(rh.reply.unwrap()).unwrap().done.unwrap();
        let rh = rh.done.unwrap();
        assert_eq!(rg.secret, rh.secret);
        assert!(rg.novo && rh.novo);
        assert_eq!(rg.peer.0, "host-private");
        assert_eq!(rh.peer.0, "guest-private");
        assert_eq!(g.peer_role().unwrap(), 1);
        assert_eq!(h.peer_role().unwrap(), 2);
        for frame in frames {
            let json = serde_json::to_string(&frame).unwrap();
            assert!(!json.contains("private"));
            assert!(!json.contains("device_id"));
        }
        let mut cg = g.take_secure_channel().unwrap();
        let mut ch = h.take_secure_channel().unwrap();
        assert_eq!(
            ch.open(&cg.seal(b"announcement").unwrap()).unwrap(),
            b"announcement"
        );
        assert_eq!(cg.open(&ch.seal(b"welcome").unwrap()).unwrap(), b"welcome");
        assert!(g.take_secure_channel().is_err());
    }
    #[test]
    fn pin_errado_e_tamper_ke2_falham_sem_confirm_e_sem_chaves() {
        for tamper in [false, true] {
            let (mut g, mut h) = machines(
                Some(if tamper {
                    pin()
                } else {
                    Pin::parse("999999").unwrap()
                }),
                Some(pin()),
                None,
                None,
            );
            let challenge = h.step(g.open().unwrap()).unwrap().reply.unwrap();
            let hello = g.step(challenge).unwrap().reply.unwrap();
            let mut ack = h.step(hello).unwrap().reply.unwrap();
            assert!(h.attempt_started());
            if tamper {
                if let PairFrame::Ack { ke2 } = &mut ack {
                    let mut bytes = decode(ke2).unwrap();
                    *bytes.last_mut().unwrap() ^= 1;
                    *ke2 = hex_encode(&bytes);
                }
            }
            assert!(g.step(ack).is_err());
            assert!(!g.is_done());
            assert!(g.take_secure_channel().is_err());
        }
    }
    #[test]
    fn retomada_usa_pake_e_conserva_segredo_com_chaves_novas() {
        let secret = [42; 32];
        let known_g = KnownPeer {
            id: DeviceId("host-private".into()),
            secret,
        };
        let known_h = KnownPeer {
            id: DeviceId("guest-private".into()),
            secret,
        };
        let mut encrypted = Vec::new();
        for _ in 0..2 {
            let (mut g, mut h) = machines(None, None, Some(known_g.clone()), Some(known_h.clone()));
            let (rg, rh) = exchange(&mut g, &mut h).unwrap();
            assert_eq!(rg.secret, secret);
            assert_eq!(rh.secret, secret);
            assert!(!rg.novo && !rh.novo);
            encrypted.push(g.take_secure_channel().unwrap().seal(b"same").unwrap());
        }
        assert_ne!(encrypted[0], encrypted[1]);
    }
    #[test]
    fn desafio_vincula_roles_noncess_e_versao_e_reflexao() {
        for attack in 0..4 {
            let (mut g, mut h) = machines(Some(pin()), Some(pin()), None, None);
            let probe = g.open().unwrap();
            let mut challenge = h.step(probe.clone()).unwrap().reply.unwrap();
            if attack == 3 {
                assert!(g.step(probe).is_err());
                continue;
            }
            if let PairFrame::Challenge {
                version,
                host_role,
                host_nonce,
                ..
            } = &mut challenge
            {
                match attack {
                    0 => *version = 2,
                    1 => *host_role = 0,
                    2 => *host_nonce = hex_encode(&[9; 16]),
                    _ => unreachable!(),
                }
            }
            match g.step(challenge) {
                Err(_) => assert_eq!(attack, 0),
                Ok(step) => {
                    let ack = h.step(step.reply.unwrap()).unwrap().reply.unwrap();
                    assert!(g.step(ack).is_err());
                }
            }
        }
    }
    #[test]
    fn confirmacao_corrompida_nao_finaliza_e_replay_nao_reabre() {
        let (mut g, mut h) = machines(Some(pin()), Some(pin()), None, None);
        let challenge = h.step(g.open().unwrap()).unwrap().reply.unwrap();
        let hello = g.step(challenge).unwrap().reply.unwrap();
        let ack = h.step(hello).unwrap().reply.unwrap();
        let mut confirm = g.step(ack).unwrap().reply.unwrap();
        if let PairFrame::Confirm {
            identity_ciphertext,
            ..
        } = &mut confirm
        {
            let mut b = decode(identity_ciphertext).unwrap();
            *b.last_mut().unwrap() ^= 1;
            *identity_ciphertext = hex_encode(&b);
        }
        assert!(h.step(confirm).is_err());
        assert!(h.take_secure_channel().is_err());
        let (mut g, mut h) = machines(Some(pin()), Some(pin()), None, None);
        exchange(&mut g, &mut h).unwrap();
        assert!(g.open().is_err());
        assert!(h.step(PairFrame::NeedsPin).is_err());
    }
    #[test]
    fn pin_explicito_tem_precedencia_e_segredos_novos_nao_dependem_apenas_do_pin() {
        let mut secrets = Vec::new();
        for _ in 0..2 {
            let (mut g, mut h) = machines(
                Some(pin()),
                Some(pin()),
                Some(KnownPeer {
                    id: DeviceId("wrong".into()),
                    secret: [1; 32],
                }),
                None,
            );
            let (rg, _) = exchange(&mut g, &mut h).unwrap();
            assert!(rg.novo);
            secrets.push(rg.secret);
        }
        assert_ne!(secrets[0], secrets[1]);
    }
    #[test]
    fn store_preserva_legado_mas_exige_pin_e_nao_rebaixa_por_merge() {
        let old=serde_json::json!({"pares":{"old-id":"11".repeat(32),"dated":{"secret":"22".repeat(32),"updated_ms":9999999999999u64}}}).to_string();
        let mut store = PairedPeers::from_json(&old).unwrap();
        assert_eq!(store.len(), 2);
        assert!(!store.has_secure_peers());
        assert!(store.requires_repair(&DeviceId("old-id".into())));
        assert!(store.get(&DeviceId("dated".into())).is_none());
        assert!(store.to_json().unwrap().contains("old-id"));
        store.insert(&PairOutcome {
            peer: DeviceId("dated".into()),
            secret: [3; 32],
            novo: true,
        });
        assert!(store.has_secure_peers());
        store.merge(&PairedPeers::from_json(&old).unwrap());
        assert_eq!(store.get(&DeviceId("dated".into())), Some([3; 32]));
        let round = PairedPeers::from_json(&store.to_json().unwrap()).unwrap();
        assert_eq!(round.get(&DeviceId("dated".into())), Some([3; 32]));
        assert!(!format!("{round:?}").contains("old-id"));
    }
    #[test]
    fn maquinas_exigem_binding_e_pin_valido() {
        let mut g = Pairing::new(Role::Guest, DeviceId("guest".into()), Some(pin()), None).unwrap();
        assert!(g.open().is_err());
        assert!(g.bind_local_role(3).is_err());
        assert!(Pin::parse("12345").is_err());
        assert_eq!(Pin::parse("012 345").unwrap(), pin());
        assert_eq!(format!("{:?}", pin()), "Pin(******)");
        let generated = Pin::generate().unwrap();
        assert_eq!(generated.to_display().len(), 6);
    }

    fn secure_store(entries: &[(&str, u8, u64)]) -> PairedPeers {
        let mut peers = serde_json::Map::new();
        for (id, byte, timestamp) in entries {
            peers.insert(
                (*id).into(),
                serde_json::json!({
                    "secret": hex_encode(&[*byte; PAIR_SECRET_LEN]),
                    "updated_ms": timestamp,
                    "security_version": SECURE_VERSION,
                }),
            );
        }
        PairedPeers::from_json(&serde_json::json!({"pares": peers}).to_string()).unwrap()
    }

    #[test]
    fn a_fusao_converge_para_o_segredo_mais_recente() {
        let peer = DeviceId("peer-fixture".into());
        // Carimbos explícitos evitam sleep e dependência do relógio da máquina.
        let extension = secure_store(&[("peer-fixture", 1, 100)]);
        let app = secure_store(&[("peer-fixture", 2, 200)]);
        let mut first = extension.clone();
        first.merge(&app);
        let mut reverse = app;
        reverse.merge(&extension);
        assert_eq!(first.get(&peer), Some([2; PAIR_SECRET_LEN]));
        assert_eq!(reverse.get(&peer), first.get(&peer));
        assert_eq!(first.len(), 1);
    }

    #[test]
    fn a_fusao_nao_perde_par_que_so_um_lado_conhece() {
        let original = secure_store(&[("peer-a", 1, 100), ("common", 2, 100)]);
        let other = secure_store(&[("peer-b", 3, 200), ("common", 4, 200)]);
        let mut first = original.clone();
        first.merge(&other);
        let mut reverse = other;
        reverse.merge(&original);
        for merged in [first, reverse] {
            assert_eq!(merged.len(), 3);
            for (id, byte) in [("peer-a", 1), ("peer-b", 3), ("common", 4)] {
                assert_eq!(
                    merged.get(&DeviceId(id.into())),
                    Some([byte; PAIR_SECRET_LEN])
                );
            }
        }
    }

    #[test]
    fn esquecer_um_par_apaga_so_ele() {
        let mut store = secure_store(&[("peer-a", 1, 100), ("peer-b", 2, 100)]);
        store.remove(&DeviceId("peer-a".into()));
        store.remove(&DeviceId("missing".into()));
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
        assert!(store.get(&DeviceId("peer-a".into())).is_none());
        assert!(!store.requires_repair(&DeviceId("peer-a".into())));
        assert_eq!(
            store.get(&DeviceId("peer-b".into())),
            Some([2; PAIR_SECRET_LEN])
        );
        store.remove(&DeviceId("peer-b".into()));
        assert!(store.is_empty());
        assert!(!store.has_secure_peers());
    }

    #[test]
    fn store_sobrevive_ida_e_volta_por_json() {
        let mut store = secure_store(&[("peer-a", 1, 0), ("peer-b", 2, u64::MAX)]);
        store.insert(&PairOutcome {
            peer: DeviceId("peer-c".into()),
            secret: [3; PAIR_SECRET_LEN],
            novo: true,
        });
        let serialized = store.to_json().unwrap();
        let restored = PairedPeers::from_json(&serialized).unwrap();
        assert_eq!(restored.len(), 3);
        assert!(restored.has_secure_peers());
        for (id, byte) in [("peer-a", 1), ("peer-b", 2), ("peer-c", 3)] {
            assert_eq!(
                restored.get(&DeviceId(id.into())),
                Some([byte; PAIR_SECRET_LEN])
            );
            assert!(!restored.requires_repair(&DeviceId(id.into())));
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&serialized).unwrap(),
            serde_json::from_str::<serde_json::Value>(&restored.to_json().unwrap()).unwrap()
        );
    }

    #[test]
    fn arquivo_do_formato_antigo_continua_preservado_mas_exige_novo_pin() {
        for entry in [
            serde_json::json!("01".repeat(32)),
            serde_json::json!({"secret":"01".repeat(32),"updated_ms":100}),
        ] {
            let old = serde_json::json!({"pares":{"peer-old":entry}});
            let read = PairedPeers::from_json(&old.to_string()).unwrap();
            let peer = DeviceId("peer-old".into());
            assert_eq!(read.len(), 1);
            assert_eq!(read.get(&peer), None);
            assert!(read.requires_repair(&peer));
            assert!(!read.has_secure_peers());
            let saved: serde_json::Value = serde_json::from_str(&read.to_json().unwrap()).unwrap();
            let before = &old["pares"]["peer-old"];
            let after = &saved["pares"]["peer-old"];
            if before.is_string() {
                assert_eq!(before, after);
            } else {
                assert_eq!(before["secret"], after["secret"]);
                assert_eq!(before["updated_ms"], after["updated_ms"]);
            }
            let mut modern = secure_store(&[("peer-old", 9, 0)]);
            modern.merge(&read);
            assert_eq!(modern.get(&peer), Some([9; PAIR_SECRET_LEN]));
            let mut opposite = read;
            opposite.merge(&modern);
            assert_eq!(opposite.get(&peer), modern.get(&peer));
        }
    }

    #[test]
    fn fusao_carimbo_zero_rollback_e_empate_preservam_regra_sem_rebaixar_v3() {
        let peer = DeviceId("peer".into());
        let zero = secure_store(&[("peer", 1, 0)]);
        assert_eq!(zero.get(&peer), Some([1; PAIR_SECRET_LEN]));
        let later = secure_store(&[("peer", 2, 200)]);
        let rollback = secure_store(&[("peer", 3, 100)]);
        let mut merged = zero;
        merged.merge(&later);
        merged.merge(&rollback);
        assert_eq!(merged.get(&peer), Some([2; PAIR_SECRET_LEN]));
        // Relógio que retrocede não prova recência: continua vencendo o maior carimbo.
        // Empate mantém a entrada existente; não alegamos convergência para secrets divergentes.
        merged.merge(&secure_store(&[("peer", 4, 200)]));
        assert_eq!(merged.get(&peer), Some([2; PAIR_SECRET_LEN]));
        let legacy = PairedPeers::from_json(
            &serde_json::json!({"pares":{"peer":{
                "secret":"05".repeat(32), "updated_ms":u64::MAX
            }}})
            .to_string(),
        )
        .unwrap();
        merged.merge(&legacy);
        assert_eq!(merged.get(&peer), Some([2; PAIR_SECRET_LEN]));
        let mut reverse = legacy;
        reverse.merge(&secure_store(&[("peer", 1, 0)]));
        assert_eq!(reverse.get(&peer), Some([1; PAIR_SECRET_LEN]));
    }

    #[test]
    fn has_secure_peers_nao_confunde_legado_futuro_ou_material_invalido() {
        for entry in [
            serde_json::json!("01".repeat(32)),
            serde_json::json!({"secret":"01".repeat(32),"updated_ms":0,"security_version":4}),
            serde_json::json!({"secret":"invalid","updated_ms":0,"security_version":3}),
        ] {
            let store =
                PairedPeers::from_json(&serde_json::json!({"pares":{"peer":entry}}).to_string())
                    .unwrap();
            assert!(!store.has_secure_peers());
            assert!(store.get(&DeviceId("peer".into())).is_none());
            assert!(store.requires_repair(&DeviceId("peer".into())));
            assert_eq!(store.len(), 1);
        }
        assert!(secure_store(&[("peer", 7, 0)]).has_secure_peers());
    }

    #[test]
    fn pin_gerado_tem_digitos_validos() {
        for _ in 0..64 {
            let generated = Pin::generate().unwrap();
            let text = generated.to_display();
            assert_eq!(text.len(), PIN_DIGITS);
            assert!(text.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(Pin::parse(&text).unwrap(), generated);
        }
    }

    #[test]
    fn pin_aceita_separadores_e_recusa_tamanho_errado() {
        assert_eq!(Pin::parse("012 345").unwrap(), pin());
        assert_eq!(Pin::parse("0.1-2 3.4-5").unwrap(), pin());
        for malformed in ["", "12345", "1234567", "12a456", "12345💡"] {
            assert!(Pin::parse(malformed).is_err());
        }
        assert!(
            Pin::parse("12345").is_err(),
            "zero inicial não pode desaparecer"
        );
    }

    #[test]
    fn debug_de_pareamento_nao_expoe_segredos_nem_identidade() {
        let secret = "a1".repeat(PAIR_SECRET_LEN);
        for entry in [
            serde_json::json!(secret),
            serde_json::json!({"secret":secret,"updated_ms":123}),
            serde_json::json!({"secret":secret,"updated_ms":123,"security_version":3}),
        ] {
            let store = PairedPeers::from_json(
                &serde_json::json!({"pares":{"private-person-id":entry}}).to_string(),
            )
            .unwrap();
            assert_eq!(format!("{store:?}"), "PairedPeers { quantidade: 1 }");
        }
        let frames = [
            PairFrame::Probe {
                version: 3,
                guest_nonce: secret.clone(),
                guest_role: 0,
            },
            PairFrame::Challenge {
                version: 3,
                token: secret.clone(),
                host_nonce: secret.clone(),
                host_role: 1,
                resume_hints: vec![secret.clone()],
            },
            PairFrame::Hello {
                mode: PairMode::Pin,
                hint: Some(secret.clone()),
                ke1: secret.clone(),
            },
            PairFrame::Ack {
                ke2: secret.clone(),
            },
            PairFrame::Confirm {
                ke3: secret.clone(),
                identity_ciphertext: secret.clone(),
            },
            PairFrame::ConfirmAck {
                identity_ciphertext: secret.clone(),
            },
            PairFrame::NeedsPin,
            PairFrame::Fail {
                motivo: "private-person-id PIN901234".into(),
            },
        ];
        for frame in frames {
            let log = format!("{frame:?}");
            assert!(!log.contains(&secret));
            assert!(!log.contains("private-person-id"));
            assert!(!log.contains("901234"));
        }
        let result = PairOutcome {
            peer: DeviceId("private-person-id".into()),
            secret: [0xa1; 32],
            novo: true,
        };
        let known = KnownPeer {
            id: result.peer.clone(),
            secret: result.secret,
        };
        let log = format!("{result:?} {known:?}");
        assert!(!log.contains("private-person-id"));
        assert!(!log.contains(&secret));
        assert_eq!(
            format!("{:?}", Pin::parse("901234").unwrap()),
            "Pin(******)"
        );
    }
    #[test]
    fn ksf_pin_equivale_ao_argon2_publicado_e_resume_so_usa_identity_no_modo_forte() {
        use opaque_ke::ksf::Ksf;
        let input: opaque_ke::generic_array::GenericArray<
            u8,
            opaque_ke::generic_array::typenum::U64,
        > = opaque_ke::generic_array::GenericArray::clone_from_slice(&[7u8; 64]);
        let expected = argon2::Argon2::default().hash(input.clone()).unwrap();
        assert_eq!(ModeKsf::Pin.hash(input.clone()).unwrap(), expected);
        assert_eq!(ModeKsf::Resume.hash(input.clone()).unwrap(), input);
    }
    #[test]
    fn mutacao_pin_para_resume_e_hint_copiado_nao_autenticam() {
        let secret = [42; 32];
        let (mut g, mut h) = machines(
            Some(pin()),
            Some(pin()),
            None,
            Some(KnownPeer {
                id: DeviceId("guest-private".into()),
                secret,
            }),
        );
        let challenge = h.step(g.open().unwrap()).unwrap().reply.unwrap();
        let copied = match &challenge {
            PairFrame::Challenge { resume_hints, .. } => resume_hints[0].clone(),
            _ => unreachable!(),
        };
        let mut hello = g.step(challenge).unwrap().reply.unwrap();
        if let PairFrame::Hello { mode, hint, .. } = &mut hello {
            *mode = PairMode::Resume;
            *hint = Some(copied);
        }
        let ack = h.step(hello).unwrap().reply.unwrap();
        assert!(g.step(ack).is_err());
        assert!(g.take_secure_channel().is_err());
        assert!(h.take_secure_channel().is_err());
    }
    #[test]
    #[ignore = "benchmark local solicitado; executar com time -l e test-threads=2"]
    fn benchmark_pake_memoria_e_latencia() {
        for label in ["pin", "wrong_pin", "resume"] {
            let start = std::time::Instant::now();
            let (mut g, mut h) = match label {
                "pin" => machines(Some(pin()), Some(pin()), None, None),
                "wrong_pin" => {
                    machines(Some(Pin::parse("999999").unwrap()), Some(pin()), None, None)
                }
                _ => machines(
                    None,
                    None,
                    Some(KnownPeer {
                        id: DeviceId("host-private".into()),
                        secret: [42; 32],
                    }),
                    Some(KnownPeer {
                        id: DeviceId("guest-private".into()),
                        secret: [42; 32],
                    }),
                ),
            };
            let result = exchange(&mut g, &mut h);
            assert_eq!(result.is_ok(), label != "wrong_pin");
            println!(
                "{{\"case\":\"{label}\",\"elapsed_ms\":{}}}",
                start.elapsed().as_millis()
            );
        }
    }
    #[test]
    #[ignore = "benchmark concorrente solicitado; executar com time -l"]
    fn benchmark_pake_duas_sessoes_pin() {
        std::thread::scope(|scope| {
            for worker in 0..2 {
                scope.spawn(move || {
                    let start = std::time::Instant::now();
                    let (mut g, mut h) = machines(Some(pin()), Some(pin()), None, None);
                    exchange(&mut g, &mut h).unwrap();
                    println!(
                        "{{\"case\":\"pin_parallel\",\"worker\":{worker},\"elapsed_ms\":{}}}",
                        start.elapsed().as_millis()
                    );
                });
            }
        });
    }
}
