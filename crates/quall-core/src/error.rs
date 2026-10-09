//! Erro único do núcleo.
//!
//! O workspace usa `panic = "abort"` em release: um panic aqui derruba o processo hospedeiro,
//! que pode ser o Zoom ou uma Broadcast Upload Extension de 50 MB. Então nada de `unwrap` em
//! caminho de execução — tudo vira [`Error`].
//!
//! Os erros carregam `String` já formatada em vez do erro original das dependências de
//! propósito: assim o tipo público do núcleo não vaza `mdns_sd::Error`, `tungstenite::Error` nem
//! `datachannel::Error` para as cascas, e a superfície de FFI não precisa acompanhar a versão
//! dessas dependências. O custo é uma alocação — só no caminho de erro.

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Entrada malformada vinda do usuário ou da rede (IP digitado inválido, TXT sem campo).
    Invalid(String),
    /// A outra ponta fala outra versão do protocolo, ou mandou algo fora de ordem.
    Protocol(String),
    /// Falha de mDNS: anúncio ou navegação.
    Discovery(String),
    /// Falha do canal de sinalização (TCP, handshake WebSocket, JSON).
    Signaling(String),
    /// Falha do transporte WebRTC/libdatachannel.
    Transport(String),
    /// **O ICE não achou caminho entre os dois aparelhos.**
    ///
    /// Separado de [`Error::Transport`] porque é o caso mais provável do produto e tem uma causa
    /// que a casca sabe consertar: no iOS, permissão de **Rede Local** negada; em qualquer
    /// plataforma, isolamento de AP ou Wi-Fi de hóspede. A frente iOS distinguia isso comparando
    /// prefixo de string em português — o que quebra na primeira vez que alguém reescreve a
    /// mensagem. Agora é um código.
    NoRoute(String),
    /// Pareamento recusado por um motivo que **não** é nenhum dos dois abaixo: mensagem fora de
    /// ordem, MAC de retomada que não confere, segredo guardado do aparelho errado.
    ///
    /// **Dívida 29.** Antes, este era o balaio de tudo — e as cascas, sem ter como separar,
    /// escreviam "O PIN não conferiu" em cima dele. A frente do Windows viu esse texto **com o
    /// PIN certo**, porque o caso real era outro. Os dois casos que saíram daqui têm conselhos
    /// **opostos**, e por isso viraram variantes próprias: [`Error::WrongPin`] e
    /// [`Error::NeedsPin`].
    ///
    /// O que sobrou aqui é de propósito o que **não** deve virar convite a recomeçar. Ver a nota
    /// de segurança em [`Error::NeedsPin`].
    Pairing(String),
    /// **O PIN digitado não conferiu. O caminho é digitar de novo.**
    ///
    /// **Dívida 29.** Separado de [`Error::Pairing`] porque o conselho é o oposto do de
    /// [`Error::NeedsPin`]: aqui existe um PIN válido do outro lado e quem errou foi a digitação;
    /// lá não existe PIN nenhum, e insistir na digitação não leva a lugar algum.
    ///
    /// Continua valendo **uma tentativa por conexão** — a conexão cai de qualquer jeito, e é isso
    /// que segura o PIN de seis dígitos. "Digitar de novo" quer dizer reconectar.
    WrongPin(String),
    /// **O aparelho não está pareado aqui, e o caminho é pedir o PIN de novo.**
    ///
    /// Separado de [`Error::Pairing`] porque não é recusa: é o convite a recomeçar. A casca mostra
    /// a tela de PIN em vez de "falhou". Ver a dívida 22.
    ///
    /// **Dívida 29.** Até 2026-08-27 esta variante existia e era **inalcançável pelo lado que
    /// precisava dela**: o anfitrião que não reconhece o par a produzia corretamente, mas a
    /// sinalização achatava o erro em `SignalMessage::Error { motivo }` — texto — e a outra ponta
    /// reconstruía tudo como [`Error::Pairing`]. É a mesma forma da dívida 28: um status que
    /// existe e não chega, porque a camada de baixo perde a causa. Agora a causa viaja no fio.
    ///
    /// **Nota de segurança, e ela decidiu onde a linha fica.** MAC de retomada inválido **não**
    /// vira `NeedsPin`, e sim [`Error::Pairing`]. Um anfitrião que falha a prova de retomada pode
    /// ser um impostor; convidar o convidado a recomeçar por PIN ali seria oferecer um caminho de
    /// rebaixamento em que o PIN a digitar é o **do impostor**. Só quem admite não conhecer o par
    /// convida a recomeçar.
    NeedsPin(String),
    /// Esgotou o prazo esperando a outra ponta.
    Timeout(String),
    /// A outra ponta fechou.
    Closed,
    /// A casca pediu para cancelar a espera. Ver [`crate::cancel::Cancelamento`].
    Cancelled,
    /// Erro de E/S do sistema.
    Io(String),
    /// **O outro aparelho está ocupado com outra sessão: tente de novo daqui a pouco.**
    ///
    /// Nasce no teleprompter (`docs/contrato-teleprompter.md` §2): um prompter com um controle já
    /// conectado responde isto a um segundo controle — e ao **mesmo** controle que volta depois de
    /// uma queda, enquanto o prompter solta a sessão velha. Separado de [`Error::Signaling`] porque
    /// o conselho é outro: não é endereço errado, é "espere e tente de novo".
    Ocupado(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(m) => write!(f, "entrada inválida: {m}"),
            Error::Protocol(m) => write!(f, "protocolo: {m}"),
            Error::Discovery(m) => write!(f, "descoberta: {m}"),
            Error::Signaling(m) => write!(f, "sinalização: {m}"),
            Error::Transport(m) => write!(f, "transporte: {m}"),
            // Mantém o prefixo `transporte:` de propósito: as cascas em produção hoje casam por
            // ele, e o código de status novo é o caminho de saída — não uma quebra na integração.
            Error::NoRoute(m) => write!(f, "transporte: {m}"),
            // Os três mantêm o prefixo `pareamento:` de propósito, pela mesma razão que o
            // `NoRoute` mantém o `transporte:`: as cascas em produção hoje casam por ele, e o
            // código de status é o caminho de saída — não uma quebra na integração.
            Error::Pairing(m) => write!(f, "pareamento: {m}"),
            Error::WrongPin(m) => write!(f, "pareamento: {m}"),
            Error::NeedsPin(m) => write!(f, "pareamento: {m}"),
            Error::Timeout(m) => write!(f, "tempo esgotado: {m}"),
            Error::Closed => write!(f, "a outra ponta fechou a conexão"),
            Error::Cancelled => write!(f, "a espera foi cancelada"),
            Error::Io(m) => write!(f, "e/s: {m}"),
            Error::Ocupado(m) => write!(f, "ocupado: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Signaling(format!("JSON: {e}"))
    }
}

/// Hex minúsculo, sem dependência externa.
///
/// Existe porque as mensagens de sinalização são JSON e chaves públicas, nonces e MACs são
/// arrays de bytes de tamanho fixo — que o `serde` não serializa direto sem mais uma dependência.
/// Hex também deixa o tráfego legível quando alguém for depurar a sinalização com um cliente
/// WebSocket qualquer.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut saida = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        saida.push(DIGITS[usize::from(b >> 4)] as char);
        saida.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    saida
}

/// Decodifica hex para um array de tamanho fixo. Tamanho errado é erro, não truncamento.
pub(crate) fn hex_decode<const N: usize>(texto: &str) -> Result<[u8; N]> {
    let bytes = texto.as_bytes();
    if bytes.len() != N * 2 {
        return Err(Error::Invalid(format!(
            "hex de {} dígitos, esperados {}",
            bytes.len(),
            N * 2
        )));
    }
    let mut saida = [0u8; N];
    for (i, par) in bytes.chunks_exact(2).enumerate() {
        let alto = digito(par[0])?;
        let baixo = digito(par[1])?;
        saida[i] = (alto << 4) | baixo;
    }
    Ok(saida)
}

fn digito(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Error::Invalid(format!(
            "caractere hex inválido: {:?}",
            c as char
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_ida_e_volta() {
        let original = [0x00u8, 0x0f, 0xa5, 0xff];
        let texto = hex_encode(&original);
        assert_eq!(texto, "000fa5ff");
        assert_eq!(hex_decode::<4>(&texto).expect("decodifica"), original);
    }

    #[test]
    fn hex_de_tamanho_errado_e_erro() {
        assert!(hex_decode::<4>("000fa5").is_err());
        assert!(hex_decode::<4>("000fa5ffff").is_err());
    }

    #[test]
    fn hex_com_lixo_e_erro() {
        assert!(hex_decode::<4>("000fa5zz").is_err());
    }
}
