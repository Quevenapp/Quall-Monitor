//! **QR invertido: o computador mostra, o celular lê** (`docs/conexao.md` §10, refeito sobre o
//! protocolo v3 em `docs/v3-no-privado.md` §4).
//!
//! O desktop que vai **receber** (Mac, Windows, OBS) mostra um QR com os endereços dele, a porta do
//! chamado, o `DeviceId` e um segredo `S` de 128 bits. O celular que vai **emitir** — já esperando,
//! com o PIN na tela — lê o QR e manda um **chamado** por TCP dizendo onde está, **selado**
//! (XChaCha20-Poly1305, chave por direção, nonce sorteado no quadro): na rede não passa id nem nome.
//! O desktop abre o chamado, gasta o convite e conecta no celular como o convidado de sempre; o
//! pareamento é o OPAQUE do v3 com `K_par` como senha (`PairMode::Qr`, `pairing.rs`), e o celular
//! acha o convite lido por uma dica HMAC, não pelo id.
//!
//! Três peças, uma por lado e uma no meio:
//!
//! - [`Convite`] — o desktop: sorteia `S`, abre a escuta do chamado, monta o texto do QR e espera.
//! - [`chamar`] — o celular: lê o texto, guarda `K_par` na [caixa](tomar) do processo e chama.
//! - a caixa ([`lidos`]/[`tomar`]/[`concluir`]) — de onde a espera do anfitrião (`session.rs`) tira
//!   `K_par` quando chega o `Hello {qr}` com a dica dele.
//!
//! # Por que 128 bits mudam a conta
//!
//! Nada de baixa entropia trafega: o chamado é selado com chaves tiradas de `S`, e o pareamento é um
//! OPAQUE com `K_par` (256 bits tirados de `S`). Varrer 2¹²⁸ não é ataque.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::busca::{no_laco_local, origem_permitida, redes_locais};
use crate::cancel::Cancelamento;
use crate::error::{hex_decode, hex_decode_vec, hex_encode, Error, Result};
use crate::pairing::CHAVE_DO_QR_LEN;
use crate::protocol::DeviceId;

/// O começo do texto do QR. O resto é uma *query* `chave=valor&…` (`docs/conexao.md` §10.2).
pub const PREFIXO: &str = "quall://chamar?";
/// A versão do formato que esta build escreve e lê: **2**, o QR sobre o protocolo v3
/// (`docs/v3-no-privado.md` §4, decisão P5). Um QR de versão maior é "atualize"; de versão menor
/// (o v1 de 12/10, chamado em claro) é recusado.
pub const VERSAO: u32 = 2;
/// A porta do chamado, quando está livre. Ocupada (outro Quall, o plugin de OBS ao lado do app), a
/// escuta abre numa efêmera — o QR leva a porta, então tanto faz para o celular.
pub const PORTA_DO_CHAMADO: u16 = 7874;
/// Quanto o convite do desktop vale, se a casca não disser outro prazo.
pub const VALIDADE_DO_CONVITE: Duration = Duration::from_secs(180);
/// Quanto a chave lida fica na caixa do celular esperando o desktop.
pub const VALIDADE_DO_LIDO: Duration = Duration::from_secs(120);
/// Quantas tentativas de pareamento uma chave lida aguenta antes de sair da caixa.
pub const TENTATIVAS_DO_LIDO: u32 = 3;

const SEGREDO_LEN: usize = 16;
const NONCE_LEN: usize = 16;
const NONCE_AEAD_LEN: usize = 24;
const ENDERECOS_NO_MAXIMO: usize = 4;
const TETO_DO_ID: usize = 128;
const TETO_DO_NOME: usize = 63;
const TETO_DA_LINHA: usize = 1024;
/// Quanto a escuta espera a linha do chamado depois de aceitar a conexão (revisão M1: curto, para
/// uma conexão muda não segurar a vaga). O celular manda a linha logo que conecta.
const PRAZO_DA_LINHA: Duration = Duration::from_millis(300);
const PRAZO_DE_CONEXAO: Duration = Duration::from_millis(1500);
const CAIXA_NO_MAXIMO: usize = 8;
/// Conexões em tratamento ao mesmo tempo, no total (revisão M1) — e no máximo **uma** por IP.
const EM_TRATAMENTO_NO_MAXIMO: usize = 16;
/// Teto do selado (`x`) em dígitos hex, nos dois sentidos (revisão B3).
const TETO_DO_SELADO_HEX: usize = 1000;
const INTERVALO_POR_IP: Duration = Duration::from_millis(250);
const CHAMADOS_POR_SEGUNDO: u32 = 20;
/// Quantos `r` de chamados o convite guarda para recusar o reenvio (revisão M3).
const NONCES_VISTOS_NO_MAXIMO: usize = 64;
const FATIA: Duration = Duration::from_millis(50);
/// O formato da linha do chamado: 2, selada (o 1 de 12/10 ia em claro).
const FORMATO: u64 = 2;

const ROTULO_SAL: &[u8] = b"quall/v3/qr";
const ROTULO_IDA: &[u8] = b"quall/v3/qr/call/phone-to-desktop";
const ROTULO_VOLTA: &[u8] = b"quall/v3/qr/call/desktop-to-phone";
const ROTULO_K_PAR: &[u8] = b"quall/v3/qr/pair";
const ROTULO_AAD_CHAMADO: &[u8] = b"quall/v3/qr/call";
const ROTULO_AAD_RESPOSTA: &[u8] = b"quall/v3/qr/answer";

// =============================================================================================
// Segredo e chaves
// =============================================================================================

/// As três chaves que saem de `S` (`docs/v3-no-privado.md` §4.1): uma por direção do chamado
/// (revisão M3: o chamado e a resposta nunca dividem chave) e a do pareamento, que é a senha do
/// OPAQUE no modo `PairMode::Qr`.
#[derive(Clone)]
struct Chaves {
    ida: [u8; 32],
    volta: [u8; 32],
    par: [u8; CHAVE_DO_QR_LEN],
}

fn sortear<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| Error::Pairing(format!("sem entropia do sistema: {e}")))?;
    Ok(b)
}

/// Campo de tamanho variável com o tamanho na frente (`u16` little-endian), para `("ab","c")` e
/// `("a","bc")` não darem a mesma AAD.
fn com_tamanho(saida: &mut Vec<u8>, campo: &[u8]) {
    let n = u16::try_from(campo.len()).unwrap_or(u16::MAX);
    saida.extend_from_slice(&n.to_le_bytes());
    saida.extend_from_slice(&campo[..usize::from(n)]);
}

fn chaves(segredo: &[u8; SEGREDO_LEN], id_desktop: &DeviceId) -> Chaves {
    let mut sal = Vec::with_capacity(ROTULO_SAL.len() + 2 + id_desktop.0.len());
    sal.extend_from_slice(ROTULO_SAL);
    com_tamanho(&mut sal, id_desktop.0.as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(&sal), segredo);
    let mut ida = [0u8; 32];
    let mut volta = [0u8; 32];
    let mut par = [0u8; CHAVE_DO_QR_LEN];
    // 32 bytes cabem sempre na HKDF-SHA256 (o teto é 255 × 32).
    let _ = hk.expand(ROTULO_IDA, &mut ida);
    let _ = hk.expand(ROTULO_VOLTA, &mut volta);
    let _ = hk.expand(ROTULO_K_PAR, &mut par);
    Chaves { ida, volta, par }
}

/// O texto do IP como os dois lados o escrevem na AAD: IPv4 puro, mesmo que chegue mapeado em IPv6
/// (`::ffff:a.b.c.d`, revisão B3).
fn ip_canonico(ip: IpAddr) -> String {
    ip.to_canonical().to_string()
}

fn aad_do_chamado(ip_do_celular: &str, r: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut aad = ROTULO_AAD_CHAMADO.to_vec();
    com_tamanho(&mut aad, ip_do_celular.as_bytes());
    aad.extend_from_slice(r);
    aad
}

fn aad_da_resposta(r: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut aad = ROTULO_AAD_RESPOSTA.to_vec();
    aad.extend_from_slice(r);
    aad
}

/// Sela com XChaCha20-Poly1305 e um nonce de 24 bytes **sorteado**, que vai no próprio quadro
/// (revisão M3): sem contador e sem nonce derivado que pudesse se repetir.
fn selar(chave: &[u8; 32], aad: &[u8], texto: &[u8]) -> Result<([u8; NONCE_AEAD_LEN], Vec<u8>)> {
    let nonce: [u8; NONCE_AEAD_LEN] = sortear()?;
    let cifra = XChaCha20Poly1305::new(chave.into());
    let selado = cifra
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: texto, aad })
        .map_err(|_| Error::Pairing("não consegui selar o chamado".into()))?;
    Ok((nonce, selado))
}

fn abrir(chave: &[u8; 32], nonce: &[u8; NONCE_AEAD_LEN], aad: &[u8], selado: &[u8]) -> Option<Vec<u8>> {
    XChaCha20Poly1305::new(chave.into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: selado, aad })
        .ok()
}

/// A dica que o desktop manda no `Hello {qr}` para o celular achar o convite lido sem id no fio
/// (`docs/v3-no-privado.md` §4.1): `HMAC(K_par, rótulo ‖ contexto)`, com os nonces da conexão.
pub(crate) fn dica_do_convite(chave_do_par: &[u8; CHAVE_DO_QR_LEN], contexto: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(chave_do_par)
        .expect("HMAC-SHA256 aceita chave de qualquer tamanho");
    mac.update(b"quall/v3/qr-hint");
    mac.update(contexto);
    mac.finalize().into_bytes().into()
}

// =============================================================================================
// O texto do QR
// =============================================================================================

/// O que o QR carrega. `Debug` não mostra o segredo: um QR em `logcat` é um pareamento público.
#[derive(Clone, PartialEq, Eq)]
pub struct ConteudoDoQr {
    /// IPv4 do desktop, a da rota padrão primeiro.
    pub enderecos: Vec<Ipv4Addr>,
    /// A porta TCP do chamado.
    pub porta: u16,
    /// O `DeviceId` do desktop — o mesmo que ele manda no `Hello`.
    pub id: DeviceId,
    /// O nome para mostrar ("Conectar a MacBook do Pessoa?"). Pode ser vazio.
    pub nome: String,
    segredo: [u8; SEGREDO_LEN],
}

impl core::fmt::Debug for ConteudoDoQr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConteudoDoQr")
            .field("enderecos", &self.enderecos)
            .field("porta", &self.porta)
            .field("segredo", &"[oculto]")
            .finish()
    }
}

/// Corta em `teto` bytes sem partir um caractere.
fn truncar(texto: &str, teto: usize) -> &str {
    if texto.len() <= teto {
        return texto;
    }
    let mut fim = teto;
    while !texto.is_char_boundary(fim) {
        fim -= 1;
    }
    &texto[..fim]
}

/// *Percent-encoding* de tudo que não é `A-Z a-z 0-9 - . _ ~`. Nunca escreve `+`.
fn codificar(texto: &str) -> String {
    let mut s = String::with_capacity(texto.len());
    for b in texto.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            s.push(char::from(b));
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn decodificar(texto: &str) -> Result<String> {
    let b = texto.as_bytes();
    let mut saida = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let par = texto
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| Error::Invalid("QR: '%' sem dois dígitos hex depois".into()))?;
            saida.push(par);
            i += 3;
        } else {
            saida.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(saida).map_err(|_| Error::Invalid("QR: texto que não é UTF-8".into()))
}

fn invalido(m: &str) -> Error {
    Error::Invalid(format!("QR do Quall ilegível: {m}"))
}

impl ConteudoDoQr {
    /// O texto que vai no QR (`docs/conexao.md` §10.2).
    pub fn para_texto(&self) -> String {
        let a: Vec<String> = self.enderecos.iter().map(Ipv4Addr::to_string).collect();
        format!(
            "{PREFIXO}v={VERSAO}&a={}&p={}&i={}&n={}&s={}",
            a.join(","),
            self.porta,
            codificar(&self.id.0),
            codificar(truncar(&self.nome, TETO_DO_NOME)),
            hex_encode(&self.segredo)
        )
    }

    /// Este texto é um QR de chamado do Quall? Só olha o prefixo: serve para a casca escolher entre
    /// "QR do Quall" e "não é um código do Quall" antes de [`ConteudoDoQr::ler`] dizer o defeito.
    pub fn parece(texto: &str) -> bool {
        texto.trim().starts_with(PREFIXO)
    }

    /// Lê o texto de um QR. Chave desconhecida é ignorada; repetida, faltando ou malformada recusa o
    /// texto inteiro. Versão maior que [`VERSAO`] é "atualize o Quall", nunca palpite.
    pub fn ler(texto: &str) -> Result<ConteudoDoQr> {
        let texto = texto.trim();
        let query = texto
            .strip_prefix(PREFIXO)
            .ok_or_else(|| Error::Invalid("este QR não é um código de conexão do Quall".into()))?;
        // Um leitor genérico pode acrescentar `#…`: fica de fora.
        let query = query.split('#').next().unwrap_or("");
        let mut v = None;
        let mut a = None;
        let mut p = None;
        let mut i = None;
        let mut n = None;
        let mut s = None;
        for par in query.split('&').filter(|x| !x.is_empty()) {
            let (chave, valor) = par.split_once('=').unwrap_or((par, ""));
            let alvo = match chave {
                "v" => &mut v,
                "a" => &mut a,
                "p" => &mut p,
                "i" => &mut i,
                "n" => &mut n,
                "s" => &mut s,
                _ => continue,
            };
            if alvo.is_some() {
                return Err(invalido(&format!("'{chave}' repetido")));
            }
            *alvo = Some(valor);
        }

        let versao: u32 = v
            .ok_or_else(|| invalido("sem versão"))?
            .parse()
            .map_err(|_| invalido("versão não é número"))?;
        if versao == 0 {
            return Err(invalido("versão 0"));
        }
        if versao > VERSAO {
            return Err(Error::Invalid(
                "este QR é de uma versão mais nova do Quall: atualize o app".into(),
            ));
        }
        if versao < VERSAO {
            return Err(Error::Invalid(
                "este QR é de uma versão anterior do Quall: atualize o computador".into(),
            ));
        }

        let mut enderecos = Vec::new();
        for e in a.ok_or_else(|| invalido("sem endereço"))?.split(',') {
            let ip: Ipv4Addr = e.parse().map_err(|_| invalido(&format!("endereço '{e}'")))?;
            if !origem_permitida(IpAddr::V4(ip)) {
                return Err(invalido(&format!("{ip} não é endereço de rede local")));
            }
            if !enderecos.contains(&ip) {
                enderecos.push(ip);
            }
        }
        if enderecos.is_empty() || enderecos.len() > ENDERECOS_NO_MAXIMO {
            return Err(invalido("de 1 a 4 endereços"));
        }

        let porta: u16 = p
            .ok_or_else(|| invalido("sem porta"))?
            .parse()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| invalido("porta fora de 1..65535"))?;

        let id = decodificar(i.ok_or_else(|| invalido("sem o id do computador"))?)?;
        if id.is_empty() || id.len() > TETO_DO_ID || id.chars().any(char::is_control) {
            return Err(invalido("id do computador"));
        }
        let nome = match n {
            Some(n) => decodificar(n)?,
            None => String::new(),
        };
        let nome: String = truncar(&nome, TETO_DO_NOME).chars().filter(|c| !c.is_control()).collect();

        let segredo: [u8; SEGREDO_LEN] =
            hex_decode(s.ok_or_else(|| invalido("sem o segredo"))?).map_err(|_| invalido("segredo"))?;

        Ok(ConteudoDoQr { enderecos, porta, id: DeviceId(id), nome, segredo })
    }

    fn chaves(&self) -> Chaves {
        chaves(&self.segredo, &self.id)
    }
}

// =============================================================================================
// O fio do chamado
// =============================================================================================

/// Por que o desktop recusou o chamado. Vai no fio como texto e não é autenticado: é dica para a tela.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotivoDaRecusa {
    /// O convite passou do prazo: o desktop precisa mostrar um QR novo.
    Expirou,
    /// O convite já atendeu um chamado (este celular de novo, ou outro aparelho que leu antes).
    Usado,
    /// O chamado não conferiu: QR de outro convite, ou quem chamou não leu este QR.
    Invalido,
    /// O computador vê o celular por outro endereço que o celular diz ter (VPN, NAT, outra
    /// interface): o chamado não vale (revisão B3).
    Endereco,
}

impl MotivoDaRecusa {
    pub fn como_texto(self) -> &'static str {
        match self {
            MotivoDaRecusa::Expirou => "expirou",
            MotivoDaRecusa::Usado => "usado",
            MotivoDaRecusa::Invalido => "invalido",
            MotivoDaRecusa::Endereco => "endereco",
        }
    }

    fn do_texto(t: &str) -> MotivoDaRecusa {
        match t {
            "expirou" => MotivoDaRecusa::Expirou,
            "usado" => MotivoDaRecusa::Usado,
            "endereco" => MotivoDaRecusa::Endereco,
            _ => MotivoDaRecusa::Invalido,
        }
    }
}

fn base(tipo: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("q".into(), "quall".into());
    m.insert("f".into(), FORMATO.into());
    m.insert("t".into(), tipo.into());
    m
}

fn resposta_recusa(motivo: MotivoDaRecusa) -> String {
    let mut m = base("resposta");
    m.insert("ok".into(), false.into());
    m.insert("motivo".into(), motivo.como_texto().into());
    serde_json::Value::Object(m).to_string()
}

/// Lê uma linha (até `\n`) de no máximo [`TETO_DA_LINHA`] bytes, até o prazo.
fn ler_linha(s: &mut TcpStream, prazo: Instant) -> Result<String> {
    let mut linha = Vec::with_capacity(256);
    let mut b = [0u8; 256];
    loop {
        let falta = prazo.saturating_duration_since(Instant::now());
        if falta.is_zero() {
            return Err(Error::Timeout("a linha do chamado não chegou a tempo".into()));
        }
        s.set_read_timeout(Some(falta))?;
        let n = match s.read(&mut b) {
            Ok(0) => return Err(Error::Closed),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(Error::Timeout("a linha do chamado não chegou a tempo".into()))
            }
            Err(e) => return Err(e.into()),
        };
        linha.extend_from_slice(&b[..n]);
        if let Some(fim) = linha.iter().position(|x| *x == b'\n') {
            linha.truncate(fim);
            return String::from_utf8(linha).map_err(|_| Error::Protocol("chamado não é UTF-8".into()));
        }
        if linha.len() > TETO_DA_LINHA {
            return Err(Error::Protocol("chamado grande demais".into()));
        }
    }
}

// =============================================================================================
// Desktop: o convite
// =============================================================================================

/// O chamado que valeu: onde o celular espera e quem ele disse ser (autenticado pelo MAC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chamado {
    /// O IP de origem da conexão do chamado com a porta de sinalização que ele mandou.
    pub endpoint: SocketAddr,
    pub par: DeviceId,
    pub nome: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fase {
    Esperando,
    Chamado(Chamado),
    Expirou,
}

struct Comum {
    chaves: Chaves,
    expira: Instant,
    fase: Mutex<Fase>,
    mudou: Condvar,
    /// Os `r` dos chamados que abriram: um `r` repetido é recusado enquanto o convite vale
    /// (revisão M3).
    vistos: Mutex<Vec<[u8; NONCE_LEN]>>,
}

fn travar<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// O que a tela do desktop mostra (`quall_qr_invite_state_json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetratoDoConvite {
    /// `"esperando"`, `"chamado"` ou `"expirou"`.
    pub fase: &'static str,
    /// Quanto falta para expirar; zero depois de chamado ou expirado.
    pub expira_em: Duration,
    pub chamado: Option<Chamado>,
}

/// **O convite do desktop**: o segredo, a escuta do chamado e o texto do QR.
///
/// Vale [`VALIDADE_DO_CONVITE`] (ou o prazo dado) e **um** chamado válido; os seguintes ouvem
/// "usado". A escuta fecha quando o prazo acaba ou ao soltar o convite.
pub struct Convite {
    comum: Arc<Comum>,
    conteudo: ConteudoDoQr,
    parar: Arc<AtomicBool>,
    linha: Option<JoinHandle<()>>,
}

impl Convite {
    /// Sorteia o segredo e abre a escuta na [`PORTA_DO_CHAMADO`] (ou numa efêmera, se ocupada), com os
    /// endereços das redes locais deste aparelho. Sem rede local: [`Error::NoRoute`].
    ///
    /// Nos testes (`cfg(test)`, ou `busca::so_o_laco_local`) o QR leva só `127.0.0.1` e a escuta
    /// abre no laço local, pelo mesmo motivo da busca: um `cargo test` não abre nada para a LAN.
    pub fn novo(eu: &DeviceId, nome: &str, validade: Duration) -> Result<Convite> {
        let enderecos: Vec<Ipv4Addr> = if no_laco_local() {
            vec![Ipv4Addr::LOCALHOST]
        } else {
            redes_locais().into_iter().map(|r| r.ip).collect()
        };
        if enderecos.is_empty() {
            return Err(Error::NoRoute(
                "este aparelho não está em nenhuma rede local: o QR não teria para onde chamar".into(),
            ));
        }
        Self::novo_em(eu, nome, validade, enderecos, PORTA_DO_CHAMADO)
    }

    /// [`Convite::novo`] com os endereços e a porta preferida dados. `porta` 0 é efêmera direto.
    pub fn novo_em(
        eu: &DeviceId,
        nome: &str,
        validade: Duration,
        enderecos: Vec<Ipv4Addr>,
        porta: u16,
    ) -> Result<Convite> {
        if eu.0.is_empty() || eu.0.len() > TETO_DO_ID {
            return Err(Error::Invalid("o id deste aparelho não cabe no QR".into()));
        }
        let mut enderecos = enderecos;
        enderecos.truncate(ENDERECOS_NO_MAXIMO);
        if enderecos.is_empty() {
            return Err(Error::Invalid("o QR precisa de ao menos um endereço".into()));
        }
        let onde = if no_laco_local() { Ipv4Addr::LOCALHOST } else { Ipv4Addr::UNSPECIFIED };
        let escuta = TcpListener::bind((onde, porta)).or_else(|_| TcpListener::bind((onde, 0)))?;
        escuta.set_nonblocking(true)?;
        let porta = escuta.local_addr()?.port();

        let segredo: [u8; SEGREDO_LEN] = sortear()?;
        let conteudo = ConteudoDoQr {
            enderecos,
            porta,
            id: eu.clone(),
            nome: truncar(nome, TETO_DO_NOME).to_string(),
            segredo,
        };
        let validade = if validade.is_zero() { VALIDADE_DO_CONVITE } else { validade };
        let comum = Arc::new(Comum {
            chaves: conteudo.chaves(),
            expira: Instant::now() + validade,
            fase: Mutex::new(Fase::Esperando),
            mudou: Condvar::new(),
            vistos: Mutex::new(Vec::new()),
        });
        let parar = Arc::new(AtomicBool::new(false));
        let linha = {
            let comum = Arc::clone(&comum);
            let parar = Arc::clone(&parar);
            std::thread::Builder::new()
                .name("quall-qr-chamado".into())
                .spawn(move || laco_da_escuta(escuta, comum, parar))?
        };
        Ok(Convite { comum, conteudo, parar, linha: Some(linha) })
    }

    /// O texto do QR.
    pub fn texto(&self) -> String {
        self.conteudo.para_texto()
    }

    /// Para o diário: endereços e porta, sem o segredo.
    pub fn conteudo(&self) -> &ConteudoDoQr {
        &self.conteudo
    }

    pub fn retrato(&self) -> RetratoDoConvite {
        let mut fase = travar(&self.comum.fase);
        expirar_se_passou(&self.comum, &mut fase);
        match &*fase {
            Fase::Esperando => RetratoDoConvite {
                fase: "esperando",
                expira_em: self.comum.expira.saturating_duration_since(Instant::now()),
                chamado: None,
            },
            Fase::Chamado(c) => {
                RetratoDoConvite { fase: "chamado", expira_em: Duration::ZERO, chamado: Some(c.clone()) }
            }
            Fase::Expirou => RetratoDoConvite { fase: "expirou", expira_em: Duration::ZERO, chamado: None },
        }
    }

    /// **Bloqueia** até um chamado valer, o convite expirar ([`Error::Timeout`]) ou a casca cancelar
    /// ([`Error::Cancelled`]).
    pub fn esperar_chamado(&self, cancelar: &Cancelamento) -> Result<Chamado> {
        let mut fase = travar(&self.comum.fase);
        loop {
            expirar_se_passou(&self.comum, &mut fase);
            match &*fase {
                Fase::Chamado(c) => return Ok(c.clone()),
                Fase::Expirou => {
                    return Err(Error::Timeout(
                        "o QR expirou sem nenhum celular chamar: mostre um novo".into(),
                    ))
                }
                Fase::Esperando => {}
            }
            if cancelar.cancelado() {
                return Err(Error::Cancelled);
            }
            fase = self
                .comum
                .mudou
                .wait_timeout(fase, FATIA)
                .map(|(g, _)| g)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }

    /// A chave do pareamento (`K_par`), para `session::conectar_pelo_qr`.
    pub fn chave_do_par(&self) -> [u8; CHAVE_DO_QR_LEN] {
        self.comum.chaves.par
    }
}

impl Drop for Convite {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(t) = self.linha.take() {
            let _ = t.join();
        }
    }
}

fn expirar_se_passou(comum: &Comum, fase: &mut Fase) {
    if *fase == Fase::Esperando && Instant::now() >= comum.expira {
        *fase = Fase::Expirou;
        comum.mudou.notify_all();
    }
}

/// O limite de chamados: uma conexão nova por IP a cada [`INTERVALO_POR_IP`] e
/// [`CHAMADOS_POR_SEGUNDO`] no total.
#[derive(Default)]
struct Limite {
    por_ip: Vec<(IpAddr, Instant)>,
    janela: Option<Instant>,
    na_janela: u32,
}

impl Limite {
    fn pode(&mut self, ip: IpAddr, agora: Instant) -> bool {
        self.por_ip.retain(|(_, t)| agora.duration_since(*t) < INTERVALO_POR_IP);
        if self.por_ip.iter().any(|(i, _)| *i == ip) {
            return false;
        }
        match self.janela {
            Some(t) if agora.duration_since(t) < Duration::from_secs(1) => {
                if self.na_janela >= CHAMADOS_POR_SEGUNDO {
                    return false;
                }
            }
            _ => {
                self.janela = Some(agora);
                self.na_janela = 0;
            }
        }
        self.na_janela += 1;
        self.por_ip.push((ip, agora));
        true
    }
}

/// As conexões do chamado em tratamento, por IP (revisão M1).
#[derive(Default)]
struct Vagas {
    ocupadas: Vec<IpAddr>,
}

/// Uma vaga ocupada; solta-se sozinha ao sair de escopo, **inclusive num pânico** — antes, a thread
/// que morria levava o contador junto (revisão A1).
struct Vaga {
    vagas: Arc<Mutex<Vagas>>,
    ip: IpAddr,
}

impl Drop for Vaga {
    fn drop(&mut self) {
        let mut v = travar(&self.vagas);
        if let Some(i) = v.ocupadas.iter().position(|ip| *ip == self.ip) {
            v.ocupadas.remove(i);
        }
    }
}

/// Uma vaga para `ip`, se ele não tem nenhuma em tratamento e o total cabe.
fn tomar_vaga(vagas: &Arc<Mutex<Vagas>>, ip: IpAddr) -> Option<Vaga> {
    let mut v = travar(vagas);
    if v.ocupadas.len() >= EM_TRATAMENTO_NO_MAXIMO || v.ocupadas.contains(&ip) {
        return None;
    }
    v.ocupadas.push(ip);
    Some(Vaga { vagas: Arc::clone(vagas), ip })
}

fn laco_da_escuta(escuta: TcpListener, comum: Arc<Comum>, parar: Arc<AtomicBool>) {
    let vagas = Arc::new(Mutex::new(Vagas::default()));
    let mut limite = Limite::default();
    loop {
        if parar.load(Ordering::Relaxed) {
            return;
        }
        {
            let mut fase = travar(&comum.fase);
            expirar_se_passou(&comum, &mut fase);
            // Vencido, a escuta fecha aqui, e a porta com ela. **Gasto, ela fica** até vencer ou a
            // casca soltar o convite: um segundo celular que leu o mesmo QR ouve "usado" em vez de
            // "o computador não respondeu" — e quem chamou antes fica visível.
            if *fase == Fase::Expirou || Instant::now() >= comum.expira {
                return;
            }
        }
        match escuta.accept() {
            Ok((conexao, origem)) => {
                let ip = origem.ip().to_canonical();
                if !origem_permitida(ip) || !limite.pode(ip, Instant::now()) {
                    drop(conexao);
                    continue;
                }
                let Some(vaga) = tomar_vaga(&vagas, ip) else {
                    drop(conexao);
                    continue;
                };
                let comum = Arc::clone(&comum);
                // Se a thread não nascer, a vaga volta com o fecho que a levava.
                let _ = std::thread::Builder::new().name("quall-qr-um-chamado".into()).spawn(move || {
                    let _vaga = vaga;
                    atender(conexao, ip, &comum);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(FATIA),
            Err(_) => std::thread::sleep(FATIA),
        }
    }
}

fn atender(mut conexao: TcpStream, origem: IpAddr, comum: &Comum) {
    let _ = conexao.set_nonblocking(false);
    let prazo = Instant::now() + PRAZO_DA_LINHA;
    let Ok(linha) = ler_linha(&mut conexao, prazo) else {
        return;
    };
    let resposta = decidir_chamado(comum, origem, &linha, Instant::now());
    let _ = conexao.set_write_timeout(Some(PRAZO_DA_LINHA));
    let _ = conexao.write_all(format!("{resposta}\n").as_bytes());
    let _ = conexao.flush();
}

/// **A decisão sobre um chamado**, sem rede: a origem, a linha e o relógio entram, a resposta sai, e
/// o convite muda de fase se valeu. Pura o bastante para os casos de ataque serem testados com uma
/// origem fora da LAN, que um teste em loopback não tem como produzir.
///
/// A linha: `{"q":"quall","f":2,"t":"chamado","r":<16 B>,"ip":<o IP que o celular diz ter>,
/// "nonce":<24 B>,"x":<selado>}`, com `x = XChaCha20-Poly1305(K_ida, nonce, aad = rótulo ‖ ip de
/// origem ‖ r, {"id","n","p"})`. O IP da AAD é o **de origem da conexão**, visto daqui: quem copia a
/// linha e a manda de outra máquina não abre o selo.
fn decidir_chamado(comum: &Comum, origem: IpAddr, linha: &str, agora: Instant) -> String {
    let origem = origem.to_canonical();
    if !origem_permitida(origem) {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    }
    let Ok(serde_json::Value::Object(m)) = serde_json::from_str::<serde_json::Value>(linha) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    let texto = |k: &str| m.get(k).and_then(serde_json::Value::as_str);
    if texto("q") != Some("quall")
        || m.get("f").and_then(serde_json::Value::as_u64) != Some(FORMATO)
        || texto("t") != Some("chamado")
    {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    }
    let (Some(r), Some(nonce), Some(x)) = (texto("r"), texto("nonce"), texto("x")) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    let (Ok(r), Ok(nonce)) = (hex_decode::<NONCE_LEN>(r), hex_decode::<NONCE_AEAD_LEN>(nonce)) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    // **A1**: hex decodificado por bytes, nunca por fatias de texto — um `x` com um caractere de
    // mais de um byte derrubava o processo ("not a char boundary").
    let Ok(selado) = hex_decode_vec(x, TETO_DO_SELADO_HEX) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    let ip_texto = ip_canonico(origem);
    // **B3**: o celular diz o IP que tem; se não é o que se vê daqui (VPN, NAT, outra interface), a
    // AAD não bateria de qualquer jeito — mas a tela do celular pode dizer o porquê.
    if let Some(dito) = texto("ip") {
        let dito = dito.parse::<IpAddr>().map(ip_canonico).unwrap_or_default();
        if dito != ip_texto {
            return resposta_recusa(MotivoDaRecusa::Endereco);
        }
    }
    // **M3**: um `r` que já abriu não abre de novo enquanto o convite vale.
    if travar(&comum.vistos).contains(&r) {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    }
    let Some(aberto) = abrir(&comum.chaves.ida, &nonce, &aad_do_chamado(&ip_texto, &r), &selado) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    {
        let mut vistos = travar(&comum.vistos);
        if vistos.len() >= NONCES_VISTOS_NO_MAXIMO {
            vistos.remove(0);
        }
        vistos.push(r);
    }
    let Ok(serde_json::Value::Object(dentro)) = serde_json::from_slice::<serde_json::Value>(&aberto) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    let campo = |k: &str| dentro.get(k).and_then(serde_json::Value::as_str);
    let (Some(id), Some(nome)) = (campo("id"), campo("n")) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    let porta = dentro
        .get("p")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0);
    let Some(porta) = porta else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    if id.is_empty() || id.len() > TETO_DO_ID || id.chars().any(char::is_control) || nome.len() > TETO_DO_NOME * 4 {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    }

    let mut fase = travar(&comum.fase);
    if *fase == Fase::Esperando && agora >= comum.expira {
        *fase = Fase::Expirou;
    }
    match &*fase {
        Fase::Esperando => {}
        Fase::Chamado(_) => return resposta_recusa(MotivoDaRecusa::Usado),
        Fase::Expirou => return resposta_recusa(MotivoDaRecusa::Expirou),
    }
    *fase = Fase::Chamado(Chamado {
        endpoint: SocketAddr::new(origem, porta),
        par: DeviceId(id.to_string()),
        // O nome vai para "Conectando a …" na tela do desktop: sem caracteres de controle, como na
        // leitura do QR.
        nome: truncar(nome, TETO_DO_NOME).chars().filter(|c| !c.is_control()).collect(),
    });
    comum.mudou.notify_all();
    drop(fase);

    let Ok((nonce, selado)) = selar(&comum.chaves.volta, &aad_da_resposta(&r), br#"{"ok":true}"#) else {
        return resposta_recusa(MotivoDaRecusa::Invalido);
    };
    let mut m = base("resposta");
    m.insert("ok".into(), true.into());
    m.insert("nonce".into(), hex_encode(&nonce).into());
    m.insert("x".into(), hex_encode(&selado).into());
    serde_json::Value::Object(m).to_string()
}

// =============================================================================================
// Celular: a caixa de convites lidos e o chamado
// =============================================================================================

struct Lido {
    id: DeviceId,
    chave: [u8; CHAVE_DO_QR_LEN],
    /// Os endereços que o QR deu para o desktop: só deles vale o pareamento pelo QR (revisão M2).
    enderecos: Vec<Ipv4Addr>,
    ate: Instant,
    tentativas: u32,
    /// O desktop já aceitou um chamado com esta chave: um chamado seguinte que falhe (o leitor que
    /// disparou duas vezes ouve "usado") não pode tirá-la da caixa — o desktop vem conectar com ela.
    aceito: bool,
}

/// **A caixa do processo** (`docs/conexao.md` §10.6): as chaves dos QR lidos, por id do desktop.
static CAIXA: Mutex<Vec<Lido>> = Mutex::new(Vec::new());

fn caixa() -> MutexGuard<'static, Vec<Lido>> {
    let mut c = travar(&CAIXA);
    let agora = Instant::now();
    c.retain(|l| l.ate > agora);
    c
}

fn guardar(id: &DeviceId, chave: [u8; CHAVE_DO_QR_LEN], enderecos: &[Ipv4Addr]) {
    let mut c = caixa();
    // O mesmo QR lido de novo depois de aceito: a entrada aceita fica como está.
    if c.iter().any(|l| l.id == *id && l.aceito && l.chave == chave) {
        return;
    }
    c.retain(|l| l.id != *id);
    if c.len() >= CAIXA_NO_MAXIMO {
        c.remove(0);
    }
    c.push(Lido {
        id: id.clone(),
        chave,
        enderecos: enderecos.to_vec(),
        ate: Instant::now() + VALIDADE_DO_LIDO,
        tentativas: 0,
        aceito: false,
    });
}

fn marcar_aceito(id: &DeviceId) {
    if let Some(l) = caixa().iter_mut().find(|l| l.id == *id) {
        l.aceito = true;
    }
}

/// O chamado com esta chave não valeu: ela sai da caixa, **a não ser** que um chamado anterior com
/// ela tenha sido aceito (revisão de 12/10 do QR, achado 1).
fn esquecer_se_nao_aceito(id: &DeviceId) {
    caixa().retain(|l| l.id != *id || l.aceito);
}

/// **Os convites lidos que valem para quem conectou de `origem`**, sem gastar tentativa: o id do
/// desktop e a chave. A espera do anfitrião procura entre eles a que casa a dica do `Hello {qr}`
/// (`docs/v3-no-privado.md` §4.1). **Revisão M2**: só convites que o desktop **aceitou** (o chamado
/// voltou com o aceite selado) e só de uma origem entre os endereços que o QR deu — quem viu o QR
/// na tela e conecta de outra máquina antes do desktop não sequestra o emissor.
pub fn lidos_de(origem: IpAddr) -> Vec<(DeviceId, [u8; CHAVE_DO_QR_LEN])> {
    let origem = origem.to_canonical();
    caixa()
        .iter()
        .filter(|l| l.tentativas < TENTATIVAS_DO_LIDO && l.aceito)
        .filter(|l| l.enderecos.iter().any(|e| IpAddr::V4(*e) == origem))
        .map(|l| (l.id.clone(), l.chave))
        .collect()
}

/// **A chave do QR lido para este desktop**, gastando uma tentativa. `None` sem QR lido, vencido ou
/// sem tentativas. A espera do anfitrião chama isto quando o `QrHello` chega.
pub fn tomar(id: &DeviceId) -> Option<[u8; CHAVE_DO_QR_LEN]> {
    let mut c = caixa();
    let i = c.iter().position(|l| l.id == *id)?;
    if c[i].tentativas >= TENTATIVAS_DO_LIDO {
        c.remove(i);
        return None;
    }
    c[i].tentativas += 1;
    Some(c[i].chave)
}

/// O pareamento pelo QR deste desktop fechou: a chave sai da caixa.
pub fn concluir(id: &DeviceId) {
    caixa().retain(|l| l.id != *id);
}

/// **O desfecho do pareamento pelo QR, visto do celular** (a espera que recebeu o desktop): a tela
/// que leu o QR fica em "Conectando a X…" até isto dizer como terminou. Por id do desktop, 2 min.
static DESFECHOS: Mutex<Vec<(DeviceId, &'static str, Instant)>> = Mutex::new(Vec::new());

/// `"connected"` (a sessão subiu), `"already_paired"` (já há vínculo com esse desktop aqui: o QR não
/// o substitui, B2), `"failed"` (a troca não fechou).
pub(crate) fn registrar_desfecho(desktop: &DeviceId, desfecho: &'static str) {
    let mut d = travar(&DESFECHOS);
    let agora = Instant::now();
    d.retain(|(i, _, t)| i != desktop && agora.duration_since(*t) < VALIDADE_DO_LIDO);
    if d.len() >= CAIXA_NO_MAXIMO {
        d.remove(0);
    }
    d.push((desktop.clone(), desfecho, agora));
}

/// Um chamado novo ao mesmo desktop começa outra tentativa: o desfecho da anterior ("connected" de
/// uma sessão que já caiu, um "failed" velho) não pode responder por ela (revisão das cascas).
fn apagar_desfecho(desktop: &DeviceId) {
    travar(&DESFECHOS).retain(|(i, _, _)| i != desktop);
}

/// Como terminou o pareamento pelo QR do desktop `desktop` neste processo, se já terminou.
pub fn desfecho(desktop: &DeviceId) -> Option<&'static str> {
    let agora = Instant::now();
    travar(&DESFECHOS)
        .iter()
        .rev()
        .find(|(i, _, t)| i == desktop && agora.duration_since(*t) < VALIDADE_DO_LIDO)
        .map(|(_, d, _)| *d)
}

/// O resultado de um chamado que chegou ao desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultadoDoChamado {
    /// O desktop aceitou (resposta autenticada) e vai conectar na porta de sinalização dada.
    Aceito { par: DeviceId, nome: String, via: SocketAddr },
    /// O desktop recusou. A chave lida já saiu da caixa.
    Recusado(MotivoDaRecusa),
}

/// **O celular chama o desktop do QR** (`docs/conexao.md` §10.3).
///
/// Guarda `K_par` na caixa **antes** de chamar — o desktop conecta logo depois de responder —, tenta
/// todos os endereços do QR ao mesmo tempo e fica com o primeiro que conectar. `porta_de_sinalizacao`
/// é a da espera que já está de pé neste processo (`quall_host*`).
///
/// Ninguém respondeu: [`Error::NoRoute`] (outra rede, firewall). Resposta de aceite com MAC que não
/// confere: [`Error::Pairing`] — quem respondeu não é o desktop do QR.
pub fn chamar(
    qr: &ConteudoDoQr,
    eu: &DeviceId,
    nome: &str,
    porta_de_sinalizacao: u16,
    prazo: Duration,
) -> Result<ResultadoDoChamado> {
    if porta_de_sinalizacao == 0 {
        return Err(Error::Invalid("chamar: a porta de sinalização não pode ser 0".into()));
    }
    if eu.0.is_empty() || eu.0.len() > TETO_DO_ID {
        return Err(Error::Invalid("chamar: id deste aparelho fora de 1..128 bytes".into()));
    }
    let limite = Instant::now() + if prazo.is_zero() { Duration::from_secs(5) } else { prazo };
    let chaves = qr.chaves();
    guardar(&qr.id, chaves.par, &qr.enderecos);
    apagar_desfecho(&qr.id);

    let r = (|| -> Result<ResultadoDoChamado> {
        let (mut conexao, via) = conectar_no_primeiro(qr, limite)?;
        // **B3**: o IP que este celular tem na conexão (`getsockname`), normalizado; o desktop
        // compara com o de origem que ele vê (`getpeername`).
        let ip_local = ip_canonico(conexao.local_addr()?.ip());
        // **B3**: o nome sai limpo como o desktop o mostraria (sem controle nem formatação).
        let nome = crate::pairing::nome_limpo(truncar(nome, TETO_DO_NOME));
        let r: [u8; NONCE_LEN] = sortear()?;
        let dentro = serde_json::json!({ "id": eu.0, "n": nome, "p": porta_de_sinalizacao });
        let (nonce, selado) = selar(&chaves.ida, &aad_do_chamado(&ip_local, &r), dentro.to_string().as_bytes())?;
        if selado.len() * 2 > TETO_DO_SELADO_HEX {
            return Err(Error::Invalid("chamar: o chamado não cabe".into()));
        }
        let mut m = base("chamado");
        m.insert("r".into(), hex_encode(&r).into());
        m.insert("ip".into(), ip_local.clone().into());
        m.insert("nonce".into(), hex_encode(&nonce).into());
        m.insert("x".into(), hex_encode(&selado).into());
        conexao.set_write_timeout(Some(limite.saturating_duration_since(Instant::now()).max(FATIA)))?;
        conexao.write_all(format!("{}\n", serde_json::Value::Object(m)).as_bytes())?;
        conexao.flush()?;

        // **M1**: a conexão que cai sem resposta (o desktop com as vagas cheias, ou a rede) tem
        // mensagem própria, e a chave fica — o desktop pode ter aceitado.
        let linha = ler_linha(&mut conexao, limite).map_err(|e| match e {
            Error::Closed | Error::Io(_) | Error::Timeout(_) => Error::Ocupado(
                "o computador não respondeu ao chamado (ocupado com outra conexão?): tente de novo".into(),
            ),
            outro => outro,
        })?;
        if let Some(motivo) = ler_resposta(&linha, &chaves, &r)? {
            return Ok(ResultadoDoChamado::Recusado(motivo));
        }
        Ok(ResultadoDoChamado::Aceito { par: qr.id.clone(), nome: qr.nome.clone(), via })
    })();
    match &r {
        Ok(ResultadoDoChamado::Aceito { .. }) => marcar_aceito(&qr.id),
        // "Usado": este celular pode ter chamado antes (o leitor que disparou duas vezes); a chave
        // fica como está (revisão B1). Sem aceite, ela não vale mesmo (`lidos_de`).
        Ok(ResultadoDoChamado::Recusado(MotivoDaRecusa::Usado)) => {}
        // Recusa, nenhum endereço respondeu, ou um aceite que não provou nada: o desktop não vem.
        Ok(ResultadoDoChamado::Recusado(_)) | Err(Error::NoRoute(_) | Error::Pairing(_) | Error::Invalid(_)) => {
            esquecer_se_nao_aceito(&qr.id)
        }
        // A resposta se perdeu depois de a linha sair (prazo, conexão fechada): o desktop pode ter
        // aceitado e vir conectar. A chave fica; os 2 min da caixa dão conta do resto.
        Err(_) => {}
    }
    r
}

/// A resposta do desktop a um chamado: `Ok(None)` é o aceite provado (selado com a chave da volta
/// sobre o `r`), `Ok(Some(motivo))` a recusa (só dica), `Err` o resto. Pura, para o teste de fuzz.
fn ler_resposta(linha: &str, chaves: &Chaves, r: &[u8; NONCE_LEN]) -> Result<Option<MotivoDaRecusa>> {
    let resposta: serde_json::Value = serde_json::from_str(linha)
        .map_err(|_| Error::Protocol("a resposta do computador não é JSON".into()))?;
    if resposta.get("t").and_then(serde_json::Value::as_str) != Some("resposta") {
        return Err(Error::Protocol("o computador respondeu outra coisa".into()));
    }
    if resposta.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let motivo = resposta.get("motivo").and_then(serde_json::Value::as_str).unwrap_or("");
        return Ok(Some(MotivoDaRecusa::do_texto(motivo)));
    }
    let sem_prova = || Error::Pairing("quem respondeu não é o computador deste QR".into());
    let nonce: [u8; NONCE_AEAD_LEN] = resposta
        .get("nonce")
        .and_then(serde_json::Value::as_str)
        .and_then(|t| hex_decode(t).ok())
        .ok_or_else(sem_prova)?;
    // **A1**: por bytes, nunca por fatias de texto.
    let selado = resposta
        .get("x")
        .and_then(serde_json::Value::as_str)
        .and_then(|t| hex_decode_vec(t, TETO_DO_SELADO_HEX).ok())
        .ok_or_else(sem_prova)?;
    let aberto = abrir(&chaves.volta, &nonce, &aad_da_resposta(r), &selado).ok_or_else(sem_prova)?;
    if aberto != br#"{"ok":true}"# {
        return Err(sem_prova());
    }
    Ok(None)
}

/// Conecta em todos os endereços do QR ao mesmo tempo; vale o primeiro.
fn conectar_no_primeiro(qr: &ConteudoDoQr, limite: Instant) -> Result<(TcpStream, SocketAddr)> {
    let prazo = limite.saturating_duration_since(Instant::now()).min(PRAZO_DE_CONEXAO);
    if prazo.is_zero() {
        return Err(Error::Timeout("sem tempo para chamar o computador".into()));
    }
    let (tx, rx) = mpsc::channel();
    for ip in &qr.enderecos {
        let alvo = SocketAddr::from((*ip, qr.porta));
        let tx = tx.clone();
        let _ = std::thread::Builder::new().name("quall-qr-chamar".into()).spawn(move || {
            let _ = tx.send(TcpStream::connect_timeout(&alvo, prazo).map(|s| (s, alvo)));
        });
    }
    drop(tx);
    let fim = Instant::now() + prazo + FATIA;
    while let Ok(r) = rx.recv_timeout(fim.saturating_duration_since(Instant::now())) {
        if let Ok(ok) = r {
            return Ok(ok);
        }
    }
    let lista: Vec<String> = qr.enderecos.iter().map(Ipv4Addr::to_string).collect();
    Err(Error::NoRoute(format!(
        "o computador não respondeu em {} (porta {}): os dois estão na mesma rede?",
        lista.join(", "),
        qr.porta
    )))
}

#[cfg(test)]
mod tests;
