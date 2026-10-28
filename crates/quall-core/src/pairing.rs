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
/// Tamanho da chave do pareamento pelo QR (`K_par`, `docs/conexao.md` §10). O pareamento pelo QR
/// volta sobre o v3 na etapa 5 de `docs/v3-no-privado.md`; até lá só `qr.rs` usa a constante.
pub const CHAVE_DO_QR_LEN: usize = 32;
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
            // Sem segredo de baixa entropia a proteger: identidade, como a retomada.
            Self::Resume | Self::Codigo | Self::Qr => ModeKsf::Resume,
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
    /// **O código de 4 dígitos** (`docs/v3-no-privado.md` §3): OPAQUE com senha pública — um
    /// acordo de chaves anônimo e cifrado — e a comparação numérica com compromisso dentro dele.
    /// Só sai de um convidado que a casca ligou ao código, e só depois de nenhuma dica casar.
    Codigo,
    /// **O QR invertido** (`docs/v3-no-privado.md` §4): a senha é `K_par`, 256 bits tirados do
    /// segredo de 128 do QR. Código de modo e rótulo de KSF próprios (revisão B2).
    Qr,
}
impl PairMode {
    fn code(self) -> u8 {
        match self {
            Self::Pin => 0,
            Self::Resume => 1,
            Self::Codigo => 2,
            Self::Qr => 3,
        }
    }
}

/// A "senha" do modo [`PairMode::Codigo`]: pública e constante. Quem autentica é a pessoa,
/// comparando o código; o OPAQUE só dá a cifra e a chave da perna.
const SENHA_PUBLICA_DO_CODIGO: &[u8] = b"quall/v3/codigo/senha-publica";
/// Dígitos do código de comparação.
pub const CODE_DIGITS: usize = 4;
/// Teto do JSON selado num quadro do código.
const MAX_SELADO: usize = 1024;

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
    /// Modo código, anfitrião → convidado: `SHA-256(rótulo ‖ cb ‖ n_h)`, selado.
    CodigoCompromisso {
        sela: String,
    },
    /// Modo código, convidado → anfitrião: `n_g`, selado.
    CodigoNonce {
        sela: String,
    },
    /// Modo código, anfitrião → convidado: `n_h`, selado. Os dois têm o código.
    CodigoRevela {
        sela: String,
    },
    /// Modo código, anfitrião → convidado: `{"ok": bool}` depois do toque no anfitrião.
    CodigoDecisao {
        sela: String,
    },
    /// Modo código: o `DeviceId` (e, do anfitrião, o nome) só depois da decisão (M2).
    CodigoIdentidade {
        sela: String,
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
            Self::CodigoCompromisso { .. } => "CodigoCompromisso",
            Self::CodigoNonce { .. } => "CodigoNonce",
            Self::CodigoRevela { .. } => "CodigoRevela",
            Self::CodigoDecisao { .. } => "CodigoDecisao",
            Self::CodigoIdentidade { .. } => "CodigoIdentidade",
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
                PairMode::Codigo => b"ksf=identity-public-codigo-v3".as_slice(),
                PairMode::Qr => b"ksf=identity-qr-kpar32-v3".as_slice(),
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
    // O código (`docs/v3-no-privado.md` §3.3). Nenhum chega a `Done` antes das duas
    // `CodigoIdentidade` (M6).
    HostCodigoNonce,
    HostCodigoDecisao,
    HostCodigoIdentidade,
    GuestCodigoCompromisso,
    GuestCodigoRevela,
    GuestCodigoDecisao,
    GuestCodigoIdentidade,
    Done,
    Failed,
}

/// O que a troca pelo código guarda entre os quadros.
#[derive(Zeroize, ZeroizeOnDrop)]
struct TrocaDoCodigo {
    /// O `session_key` do OPAQUE desta perna (o `export_key` é zerado, revisão B1).
    k: Vec<u8>,
    cb: [u8; 32],
    n_g: [u8; NONCE_LEN],
    n_h: [u8; NONCE_LEN],
    compromisso: [u8; 32],
    #[zeroize(skip)]
    codigo: Option<String>,
    #[zeroize(skip)]
    opcoes: Vec<String>,
    #[zeroize(skip)]
    nome_do_par: Option<String>,
    /// O convidado já mandou `n_g` (revisão A1: perna que cai depois disto e antes do código conta).
    nonce_saiu: bool,
}

impl TrocaDoCodigo {
    fn nova(k: &[u8], contexto: &[u8]) -> Result<Self> {
        let salt = Sha256::digest(contexto);
        let mut cb = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&salt), k)
            .expand(b"quall/v3/codigo/binding", &mut cb)
            .map_err(|_| invalid())?;
        Ok(Self {
            k: k.to_vec(),
            cb,
            n_g: [0; NONCE_LEN],
            n_h: [0; NONCE_LEN],
            compromisso: [0; 32],
            codigo: None,
            opcoes: Vec::new(),
            nome_do_par: None,
            nonce_saiu: false,
        })
    }
    fn compromisso_de(&self, n_h: &[u8; NONCE_LEN]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"quall/v3/codigo/commit");
        h.update(self.cb);
        h.update(n_h);
        h.finalize().into()
    }
    fn calcular_codigo(&mut self) -> Result<String> {
        let mut salt = [0u8; 2 * NONCE_LEN];
        salt[..NONCE_LEN].copy_from_slice(&self.n_g);
        salt[NONCE_LEN..].copy_from_slice(&self.n_h);
        let mut saida = [0u8; 4];
        Hkdf::<Sha256>::new(Some(&salt), &self.cb)
            .expand(b"quall/v3/codigo/sas", &mut saida)
            .map_err(|_| invalid())?;
        let codigo = format!("{:04}", u32::from_be_bytes(saida) % 10_000);
        self.codigo = Some(codigo.clone());
        Ok(codigo)
    }
}

/// O nome que vai ao cartão: sem caracteres de controle (Cc) **nem de formatação** (Cf: a
/// inversão de direção U+202E, os de largura zero U+200B…), que fariam o nome mostrado mentir
/// sobre quem pede (revisão, baixo 5). No máximo 63 caracteres.
pub(crate) fn nome_limpo(nome: &str) -> String {
    nome.chars().filter(|c| !c.is_control() && !de_formatacao(*c)).take(63).collect()
}

/// Os caracteres de formatação (Cf) que um nome mostrado não leva. Também o do nome anunciado
/// (`discovery::sanear_nome_anunciado`).
pub(crate) fn de_formatacao(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890 | 0x0891 | 0x08E2
        | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Dois números falsos, distintos entre si e do código, para a escolha entre três no anfitrião
/// (`docs/v3-no-privado.md` §3.6), e os três em ordem sorteada.
fn tres_opcoes(codigo: &str) -> Result<Vec<String>> {
    let mut opcoes = vec![codigo.to_string()];
    while opcoes.len() < 3 {
        let n = u32::from_be_bytes(random::<4>()?) % 10_000;
        let falso = format!("{n:04}");
        if !opcoes.contains(&falso) {
            opcoes.push(falso);
        }
    }
    // Fisher-Yates com o sorteio do sistema.
    for i in (1..opcoes.len()).rev() {
        let j = (u32::from_be_bytes(random::<4>()?) as usize) % (i + 1);
        opcoes.swap(i, j);
    }
    Ok(opcoes)
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
    /// A casca ligou o código: o anfitrião aceita `Hello {codigo}`; o convidado o manda quando não
    /// tem PIN nem dica que case.
    codigo_ligado: bool,
    /// O nome de apresentação deste aparelho: no código, vai selado ao outro lado (o do convidado
    /// antes da decisão, para o cartão; o do anfitrião só no fim, M2).
    nome: String,
    troca: Option<Box<TrocaDoCodigo>>,
    /// **Convidado pelo QR**: o celular que chamou (o id que o chamado selado disse) e `K_par`.
    qr: Option<KnownPeer>,
    /// **Anfitrião**: os convites lidos neste processo (`qr::lidos`), entre os quais a dica do
    /// `Hello {qr}` escolhe.
    convites: Vec<KnownPeer>,
    /// O convite que a dica escolheu (para a espera gastar a tentativa e, no fim, tirá-lo da caixa).
    convite_usado: Option<DeviceId>,
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
            codigo_ligado: false,
            nome: String::new(),
            troca: None,
            qr: None,
            convites: Vec::new(),
            convite_usado: None,
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
    /// Uma tentativa **de PIN** passou do KE1 e gastou a espera. A retomada e o código não gastam:
    /// não há segredo de baixa entropia a proteger (`docs/v3-no-privado.md` §3.4, revisão B2).
    pub fn attempt_started(&self) -> bool {
        self.attempted
    }

    /// Liga o código de 4 dígitos (`docs/v3-no-privado.md` §3). Só antes de começar.
    pub fn com_codigo(&mut self, ligado: bool, nome: &str) -> Result<()> {
        if !matches!(self.state, State::Initial) {
            return Err(invalid());
        }
        self.codigo_ligado = ligado;
        self.nome = nome_limpo(nome);
        Ok(())
    }

    /// O convidado já recebeu o `Challenge` (a procura da sessão salva só dá o prazo cheio a quem
    /// chegou até aqui, revisão M-c).
    pub fn desafio_recebido(&self) -> bool {
        !matches!(self.state, State::Initial | State::GuestChallenge)
    }

    /// **Convidado**: pareia pelo QR com `celular`, a senha `chave` (`K_par`). Vem antes do PIN e da
    /// retomada: ler o QR é dizer "pareie agora".
    pub fn com_qr(&mut self, celular: DeviceId, chave: [u8; CHAVE_DO_QR_LEN]) -> Result<()> {
        if !matches!(self.state, State::Initial) || self.role != Role::Guest {
            return Err(invalid());
        }
        validate_id(&celular)?;
        self.qr = Some(KnownPeer { id: celular, secret: chave });
        Ok(())
    }

    /// **Anfitrião**: os convites lidos (o id do desktop e `K_par` de cada).
    pub fn com_convites(&mut self, convites: Vec<(DeviceId, [u8; CHAVE_DO_QR_LEN])>) -> Result<()> {
        if !matches!(self.state, State::Initial) || self.role != Role::Host {
            return Err(invalid());
        }
        self.convites = convites.into_iter().map(|(id, secret)| KnownPeer { id, secret }).collect();
        Ok(())
    }

    /// O convite que a dica do `Hello {qr}` escolheu.
    pub fn convite_do_qr(&self) -> Option<&DeviceId> {
        self.convite_usado.as_ref()
    }

    /// Esta troca está no modo código.
    pub fn e_por_codigo(&self) -> bool {
        self.mode == Some(PairMode::Codigo)
    }

    /// O código, quando os dois nonces já se conhecem.
    pub fn codigo_na_tela(&self) -> Option<&str> {
        self.troca.as_ref().and_then(|t| t.codigo.as_deref())
    }

    /// Os três números do anfitrião (o código e dois falsos), em ordem sorteada.
    pub fn opcoes_na_tela(&self) -> &[String] {
        self.troca.as_ref().map_or(&[], |t| t.opcoes.as_slice())
    }

    /// O nome do outro lado: o do convidado, no anfitrião, depois do `Confirm`; o do anfitrião, no
    /// convidado, só depois das duas decisões (M2).
    pub fn nome_do_par(&self) -> Option<&str> {
        self.troca.as_ref().and_then(|t| t.nome_do_par.as_deref())
    }

    /// O anfitrião está com o código na tela, esperando a escolha da pessoa.
    pub fn esperando_a_pessoa(&self) -> bool {
        matches!(self.state, State::HostCodigoDecisao)
    }

    /// O convidado mandou `Hello {codigo}` e o anfitrião ainda não respondeu: se a conexão cair aqui,
    /// é um anfitrião que não aceita o código (uma build das lojas, ou o código desligado ou gasto).
    pub fn hello_do_codigo_sem_resposta(&self) -> bool {
        self.mode == Some(PairMode::Codigo) && matches!(self.state, State::GuestAck(_))
    }

    /// O convidado já mandou o seu nonce do código (revisão A1).
    pub fn nonce_do_codigo_saiu(&self) -> bool {
        self.troca.as_ref().is_some_and(|t| t.nonce_saiu)
    }

    /// A decisão da pessoa no anfitrião. Com `false`, o quadro de recusa sai e a máquina falha.
    pub fn decidir(&mut self, aceito: bool) -> Result<Step> {
        if self.role != Role::Host || !matches!(self.state, State::HostCodigoDecisao) {
            return Err(invalid());
        }
        let sela = self.selar(&serde_json::json!({ "ok": aceito }))?;
        if aceito {
            self.state = State::HostCodigoIdentidade;
        } else {
            self.state = State::Failed;
            self.clear_sensitive();
        }
        Ok(Step {
            reply: Some(PairFrame::CodigoDecisao { sela }),
            done: None,
        })
    }

    fn selar(&mut self, valor: &serde_json::Value) -> Result<String> {
        let bytes = serde_json::to_vec(valor)?;
        if bytes.len() > MAX_SELADO {
            return Err(invalid());
        }
        Ok(hex_encode(&self.channel.as_mut().ok_or_else(invalid)?.seal(&bytes)?))
    }

    fn abrir(&mut self, sela: &str) -> Result<serde_json::Value> {
        let bytes = self.channel.as_mut().ok_or_else(invalid)?.open(&decode(sela)?)?;
        if bytes.len() > MAX_SELADO {
            return Err(invalid());
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    }

    fn abrir_bytes<const N: usize>(&mut self, sela: &str) -> Result<[u8; N]> {
        let bytes = self.channel.as_mut().ok_or_else(invalid)?.open(&decode(sela)?)?;
        bytes.try_into().map_err(|_| invalid())
    }

    fn derivar_segredo_do_codigo(&mut self) -> Result<()> {
        let contexto = self.context.as_ref().ok_or_else(invalid)?.bytes(Some(PairMode::Codigo));
        let t = self.troca.as_ref().ok_or_else(invalid)?;
        let mut h = Sha256::new();
        h.update(&contexto);
        h.update(t.n_g);
        h.update(t.n_h);
        let salt = h.finalize();
        Hkdf::<Sha256>::new(Some(&salt), &t.k)
            .expand(b"quall/v3/pair/code-secret", &mut *self.pair_secret)
            .map_err(|_| invalid())
    }

    fn id_selado(&mut self, sela: &str) -> Result<(DeviceId, Option<String>)> {
        let v = self.abrir(sela)?;
        let id = v.get("id").and_then(|x| x.as_str()).ok_or_else(invalid)?;
        let peer = DeviceId(id.to_string());
        validate_id(&peer)?;
        if peer == self.eu {
            return Err(invalid());
        }
        // **O código substitui o vínculo que não retomou** (bancada de 13/10, `docs/v3-no-privado.md`
        // §8.6; até então a B2 do código recusava aqui, nos dois lados). É a mesma peça do QR
        // (`read_identity`, §8.4): um convidado com o vínculo bom teria achado a dica no `Challenge` e
        // retomado, então a troca só chega ao código quando o vínculo que um lado guarda já não abre
        // nada do outro. E só chega até aqui depois da escolha certa da pessoa no anfitrião — é o
        // código que autentica o convidado; o segredo velho só é trocado com a sessão de pé, quando a
        // casca grava por cima.
        let nome = v.get("n").and_then(|x| x.as_str()).map(nome_limpo);
        Ok((peer, nome))
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
    /// **Qual par guardado tem a dica neste `Challenge`** (`crate::reconhecer`), sem andar a
    /// máquina: o convidado que só quer saber quem é o anfitrião não manda `Hello`. `None`: nenhum.
    pub fn quem_casa_no_desafio(&self, frame: &PairFrame) -> Result<Option<DeviceId>> {
        let PairFrame::Challenge { version, token, host_nonce, host_role, resume_hints } = frame
        else {
            return Err(invalid());
        };
        if self.role != Role::Guest || !matches!(self.state, State::GuestChallenge) {
            return Err(invalid());
        }
        validate_version(*version)?;
        validate_role(*host_role)?;
        if resume_hints.len() != RESUME_SLOTS {
            return Err(invalid());
        }
        let context = Context {
            token: hex_decode(token).map_err(|_| invalid())?,
            guest_nonce: self.nonce,
            host_nonce: hex_decode(host_nonce).map_err(|_| invalid())?,
            guest_role: self.local_role.ok_or_else(invalid)?,
            host_role: *host_role,
        };
        let hints: Vec<[u8; 32]> = resume_hints
            .iter()
            .map(|h| hex_decode(h).map_err(|_| invalid()))
            .collect::<Result<_>>()?;
        let mut achado = None;
        for known in &self.known {
            let expected = resume_hint(&known.secret, &context)?;
            let mut found = 0u8;
            for candidate in &hints {
                found |= expected.ct_eq(candidate).unwrap_u8();
            }
            if found == 1 && achado.is_none() {
                achado = Some(known.id.clone());
            }
        }
        Ok(achado)
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
        self.qr = None;
        self.convites.clear();
        if let Some(t) = self.troca.as_mut() {
            t.k.zeroize();
            t.cb.zeroize();
        }
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
                // **O QR com um vínculo bom dos dois lados reconecta** (bancada de 13/10): se o
                // anfitrião tem a dica do par que o QR chamou, é retomada — o QR só achou o
                // aparelho, e o vínculo fica como está. Só o par do QR: outro conhecido que responda
                // não é retomado aqui (M5).
                let retomada_do_qr = match &self.qr {
                    Some(convite) => {
                        let mut achado = None;
                        for known in self.known.iter().filter(|k| k.id == convite.id) {
                            let expected = resume_hint(&known.secret, &context)?;
                            let mut found = 0u8;
                            for candidate in &hints {
                                found |= expected.ct_eq(candidate).unwrap_u8();
                            }
                            if found == 1 && achado.is_none() {
                                achado = Some((known.clone(), expected));
                            }
                        }
                        achado
                    }
                    None => None,
                };
                let (mode, hint, password) = if let Some((known, hint)) = retomada_do_qr {
                    self.qr = None;
                    let password = Zeroizing::new(known.secret.to_vec());
                    self.selected = Some(known);
                    (PairMode::Resume, Some(hex_encode(&hint)), password)
                } else if let Some(convite) = self.qr.take() {
                    // O QR primeiro: ler é consentir, como digitar o PIN.
                    let hint = crate::qr::dica_do_convite(&convite.secret, &context.bytes(None));
                    let password = Zeroizing::new(convite.secret.to_vec());
                    self.selected = Some(convite);
                    (PairMode::Qr, Some(hex_encode(&hint)), password)
                } else if let Some(pin) = &self.pin {
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
                    match selected {
                        Some((known, hint)) => {
                            let password = Zeroizing::new(known.secret.to_vec());
                            self.selected = Some(known);
                            (PairMode::Resume, Some(hex_encode(&hint)), password)
                        }
                        // Sem PIN e sem dica que case: o código, se a casca o ligou.
                        None if self.codigo_ligado => (
                            PairMode::Codigo,
                            None,
                            Zeroizing::new(SENHA_PUBLICA_DO_CODIGO.to_vec()),
                        ),
                        None => {
                            return Err(Error::NeedsPin(
                                "novo PIN necessário para este vínculo".into(),
                            ))
                        }
                    }
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
                    PairMode::Qr => {
                        let recebida: [u8; 32] = hex_decode(hint.as_deref().ok_or_else(invalid)?)
                            .map_err(|_| invalid())?;
                        let mut escolhido = None;
                        for convite in &self.convites {
                            let dica = crate::qr::dica_do_convite(&convite.secret, &context.bytes(None));
                            if dica.ct_eq(&recebida).unwrap_u8() == 1 && escolhido.is_none() {
                                escolhido = Some(convite.clone());
                            }
                        }
                        let escolhido = escolhido
                            .ok_or_else(|| Error::NeedsPin("nenhum QR lido aqui casa com este".into()))?;
                        let password = Zeroizing::new(escolhido.secret.to_vec());
                        self.convite_usado = Some(escolhido.id.clone());
                        self.selected = Some(escolhido);
                        password
                    }
                    PairMode::Codigo => {
                        if hint.is_some() {
                            return Err(invalid());
                        }
                        if !self.codigo_ligado {
                            return Err(Error::NeedsPin("este aparelho não aceita o código".into()));
                        }
                        Zeroizing::new(SENHA_PUBLICA_DO_CODIGO.to_vec())
                    }
                };
                let ke1 = CredentialRequest::<Suite>::deserialize(&decode(&ke1)?)
                    .map_err(|_| invalid())?;
                let binding = context.bytes(Some(mode));
                // Só um KE1 de PIN gasta a espera (`docs/v3-no-privado.md` §3.4, B2).
                if mode == PairMode::Pin {
                    self.attempted = true;
                }
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
                if mode == PairMode::Codigo {
                    // Só o nome, para o cartão do anfitrião; o id vai depois da decisão (M2).
                    let nome = self.nome.clone();
                    let identity_ciphertext = self.selar(&serde_json::json!({ "n": nome }))?;
                    self.state = State::GuestCodigoCompromisso;
                    return Ok(Step {
                        reply: Some(PairFrame::Confirm {
                            ke3: hex_encode(&result.message.serialize()),
                            identity_ciphertext,
                        }),
                        done: None,
                    });
                }
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
                if mode == PairMode::Codigo {
                    let v = self.abrir(&identity_ciphertext)?;
                    let nome = nome_limpo(v.get("n").and_then(|x| x.as_str()).unwrap_or(""));
                    let n_h: [u8; NONCE_LEN] = random()?;
                    let troca = self.troca.as_mut().ok_or_else(invalid)?;
                    troca.n_h = n_h;
                    troca.nome_do_par = Some(nome);
                    let c = troca.compromisso_de(&n_h);
                    let sela = hex_encode(&self.channel.as_mut().ok_or_else(invalid)?.seal(&c)?);
                    self.state = State::HostCodigoNonce;
                    return Ok(Step {
                        reply: Some(PairFrame::CodigoCompromisso { sela }),
                        done: None,
                    });
                }
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
            (Role::Guest, State::GuestCodigoCompromisso, PairFrame::CodigoCompromisso { sela }) => {
                let c: [u8; 32] = self.abrir_bytes(&sela)?;
                let n_g: [u8; NONCE_LEN] = random()?;
                let troca = self.troca.as_mut().ok_or_else(invalid)?;
                troca.compromisso = c;
                troca.n_g = n_g;
                troca.nonce_saiu = true;
                let sela = hex_encode(&self.channel.as_mut().ok_or_else(invalid)?.seal(&n_g)?);
                self.state = State::GuestCodigoRevela;
                Ok(Step {
                    reply: Some(PairFrame::CodigoNonce { sela }),
                    done: None,
                })
            }
            (Role::Host, State::HostCodigoNonce, PairFrame::CodigoNonce { sela }) => {
                let n_g: [u8; NONCE_LEN] = self.abrir_bytes(&sela)?;
                let troca = self.troca.as_mut().ok_or_else(invalid)?;
                troca.n_g = n_g;
                let n_h = troca.n_h;
                let codigo = troca.calcular_codigo()?;
                troca.opcoes = tres_opcoes(&codigo)?;
                let sela = hex_encode(&self.channel.as_mut().ok_or_else(invalid)?.seal(&n_h)?);
                self.state = State::HostCodigoDecisao;
                Ok(Step {
                    reply: Some(PairFrame::CodigoRevela { sela }),
                    done: None,
                })
            }
            (Role::Guest, State::GuestCodigoRevela, PairFrame::CodigoRevela { sela }) => {
                let n_h: [u8; NONCE_LEN] = self.abrir_bytes(&sela)?;
                let troca = self.troca.as_mut().ok_or_else(invalid)?;
                // O compromisso tem de bater: é o que impede o relé de escolher `n_h` depois de
                // ver `n_g` (`docs/v3-no-privado.md` §3.3).
                if troca.compromisso_de(&n_h).ct_eq(&troca.compromisso).unwrap_u8() != 1 {
                    return Err(Error::Pairing("o compromisso do código não conferiu".into()));
                }
                troca.n_h = n_h;
                troca.calcular_codigo()?;
                self.state = State::GuestCodigoDecisao;
                Ok(Step {
                    reply: None,
                    done: None,
                })
            }
            (Role::Guest, State::GuestCodigoDecisao, PairFrame::CodigoDecisao { sela }) => {
                let v = self.abrir(&sela)?;
                if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
                    return Err(Error::Pairing("o outro aparelho recusou o código".into()));
                }
                let id = self.eu.0.clone();
                let sela = self.selar(&serde_json::json!({ "id": id }))?;
                self.state = State::GuestCodigoIdentidade;
                Ok(Step {
                    reply: Some(PairFrame::CodigoIdentidade { sela }),
                    done: None,
                })
            }
            (Role::Host, State::HostCodigoIdentidade, PairFrame::CodigoIdentidade { sela }) => {
                let (peer, _) = self.id_selado(&sela)?;
                let (id, nome) = (self.eu.0.clone(), self.nome.clone());
                let sela = self.selar(&serde_json::json!({ "id": id, "n": nome }))?;
                self.derivar_segredo_do_codigo()?;
                let outcome = self.finish(peer, PairMode::Codigo);
                Ok(Step {
                    reply: Some(PairFrame::CodigoIdentidade { sela }),
                    done: Some(outcome),
                })
            }
            (Role::Guest, State::GuestCodigoIdentidade, PairFrame::CodigoIdentidade { sela }) => {
                let (peer, nome) = self.id_selado(&sela)?;
                if let Some(t) = self.troca.as_mut() {
                    t.nome_do_par = nome;
                }
                self.derivar_segredo_do_codigo()?;
                let outcome = self.finish(peer, PairMode::Codigo);
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
        if mode == PairMode::Codigo {
            // O segredo do par só nasce depois das duas decisões (`derivar_segredo_do_codigo`).
            self.troca = Some(Box::new(TrocaDoCodigo::nova(session_key, context)?));
        } else if mode == PairMode::Resume {
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
        // **O QR substitui um vínculo que não retomou** (bancada de 13/10; até então a B2 do QR
        // recusava, como o código). Um convidado com o vínculo bom teria retomado (acima, no
        // `Challenge`): se a troca chegou ao `Qr`, o vínculo que um dos lados guarda já não abre
        // nada do outro — e o QR é consentimento nos dois (mostrar e ler, com 256 bits na tela),
        // como o PIN, que sempre substituiu. A B2 continua valendo para o código de 4 dígitos.
        Ok(peer)
    }
    fn finish(&mut self, peer: DeviceId, mode: PairMode) -> PairOutcome {
        let outcome = PairOutcome {
            peer,
            secret: *self.pair_secret,
            novo: mode != PairMode::Resume,
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
        PairMode::Resume | PairMode::Codigo | PairMode::Qr => invalid(),
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

    /// Os ids com vínculo v3 válido.
    pub fn ids_seguros(&self) -> Vec<DeviceId> {
        let mut ids: Vec<_> = self.pares.keys().map(|k| DeviceId(k.clone())).filter(|id| self.get(id).is_some()).collect();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        ids
    }

    /// **Só o vínculo v3 de `peer`** (revisão M5, `docs/v3-no-privado.md` §5.2): a procura da sessão
    /// salva retoma com o alvo e com mais ninguém, mesmo que outro par conhecido responda. Vazia se
    /// não há vínculo v3 válido com ele.
    pub fn so_de(&self, peer: &DeviceId) -> PairedPeers {
        let mut t = PairedPeers::new();
        if self.get(peer).is_some() {
            if let Some(e) = self.pares.get(&peer.0) {
                t.pares.insert(peer.0.clone(), e.clone());
            }
        }
        t
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

/// O código de 4 dígitos sobre o v3, na máquina (`docs/v3-no-privado.md` §3). A sequência com a
/// sinalização e a escolha na tela está em `session::testes_do_codigo`.
#[cfg(test)]
mod testes_do_codigo {
    use super::*;

    fn par(codigo_g: bool, codigo_h: bool) -> (Pairing, Pairing) {
        let mut g = Pairing::new(Role::Guest, DeviceId("convidado-id".into()), None, None).unwrap();
        let mut h = Pairing::new(Role::Host, DeviceId("anfitriao-id".into()), None, None).unwrap();
        g.bind_local_role(0).unwrap();
        h.bind_local_role(0).unwrap();
        g.com_codigo(codigo_g, "Galaxy A07").unwrap();
        h.com_codigo(codigo_h, "MacBook").unwrap();
        (g, h)
    }

    fn passar(de: &mut Pairing, quadro: PairFrame) -> Step {
        de.step(quadro).expect("passo")
    }

    /// Até o código na tela dos dois: Probe … Revela. Devolve os quadros públicos.
    fn ate_o_codigo(g: &mut Pairing, h: &mut Pairing) -> Vec<PairFrame> {
        let mut quadros = Vec::new();
        let probe = g.open().unwrap();
        quadros.push(probe.clone());
        let challenge = passar(h, probe).reply.unwrap();
        quadros.push(challenge.clone());
        let hello = passar(g, challenge).reply.unwrap();
        assert!(matches!(hello, PairFrame::Hello { mode: PairMode::Codigo, hint: None, .. }));
        quadros.push(hello.clone());
        let ack = passar(h, hello).reply.unwrap();
        quadros.push(ack.clone());
        let confirm = passar(g, ack).reply.unwrap();
        quadros.push(confirm.clone());
        let compromisso = passar(h, confirm).reply.unwrap();
        assert!(matches!(compromisso, PairFrame::CodigoCompromisso { .. }));
        assert_eq!(h.nome_do_par(), Some("Galaxy A07"), "o cartão do anfitrião diz quem pede");
        quadros.push(compromisso.clone());
        let nonce = passar(g, compromisso).reply.unwrap();
        assert!(g.nonce_do_codigo_saiu());
        quadros.push(nonce.clone());
        let revela = passar(h, nonce).reply.unwrap();
        quadros.push(revela.clone());
        let nada = passar(g, revela);
        assert!(nada.reply.is_none() && nada.done.is_none());
        quadros
    }

    #[test]
    fn o_codigo_e_o_mesmo_nos_dois_e_esta_entre_as_tres_opcoes() {
        let (mut g, mut h) = par(true, true);
        let quadros = ate_o_codigo(&mut g, &mut h);
        let codigo = g.codigo_na_tela().unwrap().to_string();
        assert_eq!(codigo.len(), CODE_DIGITS);
        assert_eq!(h.codigo_na_tela(), Some(codigo.as_str()));
        let opcoes = h.opcoes_na_tela().to_vec();
        assert_eq!(opcoes.len(), 3);
        assert!(opcoes.contains(&codigo));
        let mut distintas = opcoes.clone();
        distintas.dedup();
        distintas.sort();
        distintas.dedup();
        assert_eq!(distintas.len(), 3, "três números distintos");
        assert!(g.opcoes_na_tela().is_empty(), "o convidado só mostra o código");
        assert!(h.esperando_a_pessoa());
        // **Sentinela**: nenhum quadro público leva id nem nome.
        for q in &quadros {
            let json = serde_json::to_string(q).unwrap();
            for proibido in ["convidado-id", "anfitriao-id", "Galaxy", "MacBook"] {
                assert!(!json.contains(proibido), "{proibido} em {json}");
            }
        }
    }

    /// **M6**: nada sai de `take_secure_channel` e ninguém chega a `Done` antes das duas
    /// `CodigoIdentidade`. E **M2**: o nome do anfitrião só chega ao convidado no fim.
    #[test]
    fn so_depois_das_duas_identidades_e_o_nome_do_anfitriao_so_no_fim() {
        let (mut g, mut h) = par(true, true);
        ate_o_codigo(&mut g, &mut h);
        assert!(g.take_secure_channel().is_err() && h.take_secure_channel().is_err());
        assert_eq!(g.nome_do_par(), None, "M2: o nome do anfitrião não vem antes da decisão");
        let decisao = h.decidir(true).unwrap().reply.unwrap();
        assert!(h.take_secure_channel().is_err() && !h.is_done());
        let id_g = passar(&mut g, decisao).reply.unwrap();
        assert!(g.take_secure_channel().is_err() && !g.is_done());
        let fim_h = passar(&mut h, id_g);
        let rh = fim_h.done.unwrap();
        let rg = passar(&mut g, fim_h.reply.unwrap()).done.unwrap();
        assert_eq!(rg.secret, rh.secret);
        assert!(rg.novo && rh.novo, "o código é um pareamento novo (B2)");
        assert_eq!(rg.peer.0, "anfitriao-id");
        assert_eq!(rh.peer.0, "convidado-id");
        assert_eq!(g.nome_do_par(), Some("MacBook"));
        let mut cg = g.take_secure_channel().unwrap();
        let mut ch = h.take_secure_channel().unwrap();
        assert_eq!(ch.open(&cg.seal(b"oi").unwrap()).unwrap(), b"oi");
        // O segredo do código é um vínculo v3 como outro qualquer: a próxima vez é retomada.
        let mut tabela = PairedPeers::new();
        tabela.insert(&rg);
        assert!(tabela.has_secure_peers());
    }

    #[test]
    fn a_recusa_no_anfitriao_chega_ao_convidado() {
        let (mut g, mut h) = par(true, true);
        ate_o_codigo(&mut g, &mut h);
        let recusa = h.decidir(false).unwrap().reply.unwrap();
        assert!(h.take_secure_channel().is_err());
        let r = g.step(recusa);
        assert!(matches!(&r, Err(Error::Pairing(m)) if m.contains("recusou")), "{r:?}");
    }

    #[test]
    fn anfitriao_sem_codigo_recusa_o_hello_antes_do_ke1() {
        let (mut g, mut h) = par(true, false);
        let challenge = passar(&mut h, g.open().unwrap()).reply.unwrap();
        let hello = passar(&mut g, challenge).reply.unwrap();
        assert!(matches!(h.step(hello), Err(Error::NeedsPin(_))));
        assert!(!h.attempt_started(), "não gasta o PIN (M4)");
    }

    #[test]
    fn convidado_sem_codigo_continua_pedindo_pin() {
        let (mut g, mut h) = par(false, true);
        let challenge = passar(&mut h, g.open().unwrap()).reply.unwrap();
        assert!(matches!(g.step(challenge), Err(Error::NeedsPin(_))));
    }

    /// **O compromisso**: um anfitrião (ou um relé fazendo esse papel) que revela outro `n_h`
    /// depois de ver `n_g` é pego pelo convidado.
    #[test]
    fn revelar_outro_nonce_que_o_comprometido_falha() {
        let (mut g, mut h) = par(true, true);
        let challenge = passar(&mut h, g.open().unwrap()).reply.unwrap();
        let hello = passar(&mut g, challenge).reply.unwrap();
        let ack = passar(&mut h, hello).reply.unwrap();
        let confirm = passar(&mut g, ack).reply.unwrap();
        let compromisso = passar(&mut h, confirm).reply.unwrap();
        let nonce = passar(&mut g, compromisso).reply.unwrap();
        h.troca.as_mut().unwrap().n_h = random().unwrap(); // escolhe outro n_h depois do compromisso
        let revela = passar(&mut h, nonce).reply.unwrap();
        let r = g.step(revela);
        assert!(matches!(&r, Err(Error::Pairing(m)) if m.contains("compromisso")), "{r:?}");
        assert!(g.codigo_na_tela().is_none(), "o código nem chega à tela");
    }

    /// **Relé no meio** (duas pernas independentes): cada perna tem a sua chave, e os códigos das
    /// duas telas não coincidem — salvo 1 em 10⁴. Três rodadas: a chance de as três coincidirem é
    /// 10⁻¹².
    #[test]
    fn um_rele_no_meio_ve_codigos_diferentes_nas_duas_telas() {
        let mut algum_diferente = false;
        for _ in 0..3 {
            let (mut g, mut rele_h) = par(true, true);
            let (mut rele_g, mut h) = par(true, true);
            ate_o_codigo(&mut g, &mut rele_h);
            ate_o_codigo(&mut rele_g, &mut h);
            algum_diferente |= g.codigo_na_tela() != h.codigo_na_tela();
        }
        assert!(algum_diferente);
    }

    #[test]
    fn decidir_fora_de_hora_e_do_convidado_e_invalido() {
        let (mut g, mut h) = par(true, true);
        assert!(h.decidir(true).is_err());
        ate_o_codigo(&mut g, &mut h);
        assert!(g.decidir(true).is_err());
    }
}

#[cfg(test)]
mod testes_do_nome_no_cartao {
    use super::nome_limpo;

    /// **Baixo 5**: o nome que vai ao cartão perde os de controle (Cc) e os de formatação (Cf) —
    /// a inversão de direção e os de largura zero fariam o nome mostrado mentir sobre quem pede.
    #[test]
    fn o_nome_perde_controle_e_formatacao() {
        assert_eq!(nome_limpo("Galaxy\u{202E}70A"), "Galaxy70A");
        assert_eq!(nome_limpo("Mac\u{200B}Book\u{200D}\u{FEFF}"), "MacBook");
        assert_eq!(nome_limpo("A07\n\t\u{7}"), "A07");
        assert_eq!(nome_limpo("Câmera à esquerda"), "Câmera à esquerda");
        assert_eq!(nome_limpo(&"x".repeat(100)).chars().count(), 63);
    }
}
