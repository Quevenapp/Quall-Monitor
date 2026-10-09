//! Pareamento por PIN — uma vez por par de aparelhos, nunca por sessão.
//!
//! Sem conta, sem login: o vínculo é entre dois [`DeviceId`] e é o que dispensa o segundo
//! pareamento. O QR code do M6 vai carregar exatamente o mesmo PIN, só que sem digitação.
//!
//! # Por que não é só "manda o PIN e compara"
//!
//! Um PIN de seis dígitos tem cerca de 20 bits. Se a confirmação fosse `HMAC(pin, transcrição)`,
//! qualquer um na LAN capturaria a mensagem e testaria os 10⁶ PINs **offline**, em menos de um
//! segundo — e com o PIN na mão derivaria o segredo de longo prazo, que é o que dispensa o
//! pareamento seguinte. O ataque não é teórico: a sinalização é HTTP simples numa LAN que pode
//! ter um convidado no mesmo Wi-Fi.
//!
//! Por isso a chave de confirmação sai de **X25519 mais o PIN**, não do PIN sozinho:
//!
//! ```text
//! ikm       = X25519(sk_meu, pk_dele) || pin
//! prk       = HKDF-Extract(salt = transcrição, ikm)
//! confirmar = HKDF-Expand(prk, "quall/pair/confirm/v1", 32)
//! segredo   = HKDF-Expand(prk, "quall/pair/secret/v1",  32)
//! ```
//!
//! Um bisbilhoteiro passivo não tem o segredo X25519, então adivinhar o PIN não lhe dá nada.
//! Um atacante ativo, no meio da sinalização, tem **uma** tentativa: se errar o PIN, o MAC não
//! confere, a conexão morre e o emissor gera outro PIN. A chance é 1 em 10⁶ por tentativa.
//!
//! O segredo de longo prazo **nunca trafega**: os dois lados derivam o mesmo valor da mesma
//! transcrição. Não há transporte de chave para interceptar.
//!
//! # O que este módulo ainda não faz
//!
//! A transcrição não inclui a *fingerprint* DTLS do SDP. Enquanto o pareamento acontece antes da
//! oferta, e o mesmo canal WebSocket carrega os dois, um atacante que já esteja no meio da
//! sinalização é detectado no pareamento — mas amarrar as duas coisas explicitamente é mais
//! forte, e é o que fecha o caso quando a sinalização passar a aceitar reconexão. Anotado como
//! dívida, não como pronto.
//!
//! [`DeviceId`]: crate::protocol::DeviceId

use std::collections::HashMap;

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::{hex_decode, hex_encode, Error, Result};
use crate::protocol::{DeviceId, PROTOCOL_VERSION};

type HmacSha256 = Hmac<Sha256>;

/// Quantos dígitos o usuário digita. Seis é o limite do que se lê de uma tela e se digita em
/// outra sem errar; a segurança vem do X25519, não do tamanho do PIN.
pub const PIN_DIGITS: usize = 6;

/// Tamanho do segredo de longo prazo guardado por par de aparelhos.
pub const PAIR_SECRET_LEN: usize = 32;

const NONCE_LEN: usize = 16;
const MAC_LEN: usize = 32;

const ROTULO_CONFIRMAR: &[u8] = b"quall/pair/confirm/v1";
const ROTULO_SEGREDO: &[u8] = b"quall/pair/secret/v1";
const ROTULO_TRANSCRICAO: &[u8] = b"quall/pair/transcript/v1";
const ROTULO_RETOMADA: &[u8] = b"quall/pair/resume/v1";

/// PIN mostrado pelo emissor e digitado no receptor.
///
/// Guarda os dígitos, não o texto: `"012345"` e `"12345"` são coisas diferentes e confundir os
/// dois é o tipo de bug que só aparece um em dez pareamentos.
#[derive(Clone, PartialEq, Eq)]
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

    fn bytes(&self) -> [u8; PIN_DIGITS] {
        self.0
    }
}

// `Debug` que não vaza o PIN em log. Um PIN em `logcat` é um PIN público.
impl core::fmt::Debug for Pin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Pin(******)")
    }
}

/// Papel na troca. Quem hospeda a sinalização mostra o PIN; quem conecta digita.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Hospeda a sinalização e mostra o PIN na tela.
    Host,
    /// Conecta e digita o PIN.
    Guest,
}

impl Role {
    fn tag(self) -> u8 {
        match self {
            Role::Host => b'H',
            Role::Guest => b'G',
        }
    }
}

/// Par já conhecido: quem é e qual o segredo guardado para ele.
#[derive(Clone, PartialEq, Eq)]
pub struct KnownPeer {
    pub id: DeviceId,
    pub secret: [u8; PAIR_SECRET_LEN],
}

impl core::fmt::Debug for KnownPeer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KnownPeer")
            .field("id", &"[oculto]")
            .field("secret", &"[oculto]")
            .finish()
    }
}

/// Mensagens do pareamento, transportadas dentro da sinalização.
///
/// Bytes viajam em hex porque a sinalização é JSON, e porque assim dá para acompanhar a troca
/// com um cliente WebSocket qualquer quando algo não fecha na bancada.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "p", rename_all = "snake_case")]
pub enum PairFrame {
    /// Convidado abre: quem sou eu, minha chave efêmera, meu nonce.
    Hello {
        device_id: DeviceId,
        public_key: String,
        nonce: String,
    },
    /// Anfitrião responde com os dele.
    Ack {
        device_id: DeviceId,
        public_key: String,
        nonce: String,
    },
    /// Convidado prova que sabe o PIN.
    Confirm { mac: String },
    /// Anfitrião prova de volta. Só depois disso o convidado guarda o segredo.
    ConfirmAck { mac: String },

    /// Já pareado: convidado pede para retomar sem PIN.
    Resume { device_id: DeviceId, nonce: String },
    /// Anfitrião desafia com o segredo guardado.
    ResumeChallenge { nonce: String, mac: String },
    /// Convidado responde ao desafio.
    ResumeProof { mac: String },
    /// Anfitrião aceita.
    Ok,

    /// **Não reconheço este aparelho: comece de novo pelo PIN.** (dívida 22)
    ///
    /// É a diferença entre um beco sem saída e uma segunda chance. Antes, um [`PairFrame::Resume`]
    /// de aparelho desconhecido morria em "aparelho não está pareado aqui" e a conexão caía — o
    /// usuário via "funcionou ontem, hoje não funciona" sem nenhuma forma de digitar o PIN outra
    /// vez. Agora o anfitrião convida a recomeçar, na **mesma** conexão.
    ///
    /// Não é uma tentativa a mais contra o PIN: quem recebe isto ainda não provou nada, e o
    /// caminho do PIN que vem a seguir continua valendo uma tentativa por conexão.
    NeedsPin,

    /// Recusa, com motivo legível.
    Fail { motivo: String },
}

// Debug é diagnóstico, não exportação do quadro de autenticação. Mesmo chave pública,
// nonce/prova e texto recebido do par não precisam aparecer num diário do produto.
impl core::fmt::Debug for PairFrame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let tipo = match self {
            Self::Hello { .. } => "Hello",
            Self::Ack { .. } => "Ack",
            Self::Confirm { .. } => "Confirm",
            Self::ConfirmAck { .. } => "ConfirmAck",
            Self::Resume { .. } => "Resume",
            Self::ResumeChallenge { .. } => "ResumeChallenge",
            Self::ResumeProof { .. } => "ResumeProof",
            Self::Ok => "Ok",
            Self::NeedsPin => "NeedsPin",
            Self::Fail { .. } => "Fail",
        };
        f.write_str(tipo)
    }
}

/// Resultado de um passo da máquina de estados.
#[derive(Debug)]
pub struct Step {
    /// O que mandar para a outra ponta, se houver.
    pub reply: Option<PairFrame>,
    /// Se o pareamento terminou com sucesso, o segredo do par e quem é o par.
    pub done: Option<PairOutcome>,
}

/// O que sobra de um pareamento bem-sucedido.
#[derive(Clone, PartialEq, Eq)]
pub struct PairOutcome {
    pub peer: DeviceId,
    /// Segredo de longo prazo. A casca persiste; o núcleo não escreve em disco.
    pub secret: [u8; PAIR_SECRET_LEN],
    /// `true` quando este pareamento nasceu de um PIN, `false` quando foi retomada.
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

/// Máquina de estados do pareamento, sem rede e sem relógio — por isso testável inteira.
///
/// Um lado é `Host`, o outro é `Guest`. Quem chama alimenta [`Pairing::step`] com o que chegou
/// e manda o que sair. Uma falha aqui derruba a sinalização: pareamento é uma tentativa só, e
/// deixar tentar de novo na mesma conexão transformaria 1 em 10⁶ em força bruta.
pub struct Pairing {
    role: Role,
    eu: DeviceId,
    pin: Option<Pin>,
    /// Par já guardado, quando existe. É o que habilita a retomada sem PIN.
    conhecido: Option<KnownPeer>,
    sk: StaticSecret,
    pk: PublicKey,
    nonce: [u8; NONCE_LEN],
    estado: Estado,
}

#[derive(Debug, PartialEq, Eq)]
enum Estado {
    Inicio,
    /// Convidado mandou `Hello` e espera `Ack`.
    EsperandoAck,
    /// Anfitrião mandou `Ack` e espera `Confirm`.
    EsperandoConfirm {
        chave: Chaves,
        par: DeviceId,
    },
    /// Convidado mandou `Confirm` e espera `ConfirmAck`.
    EsperandoConfirmAck {
        chave: Chaves,
        par: DeviceId,
    },
    /// Convidado mandou `Resume` e espera o desafio.
    EsperandoDesafio,
    /// Anfitrião mandou o desafio e espera a prova.
    EsperandoProva {
        par: DeviceId,
        segredo: [u8; PAIR_SECRET_LEN],
        nonce_guest: [u8; NONCE_LEN],
        nonce_host: [u8; NONCE_LEN],
    },
    /// Convidado mandou a prova e espera o `Ok`.
    EsperandoOk {
        par: DeviceId,
        segredo: [u8; PAIR_SECRET_LEN],
    },
    Pronto,
    Falhou,
}

#[derive(PartialEq, Eq)]
struct Chaves {
    confirmar: [u8; 32],
    segredo: [u8; PAIR_SECRET_LEN],
    transcricao: Vec<u8>,
}

impl core::fmt::Debug for Chaves {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Chaves([oculto])")
    }
}

impl Pairing {
    /// Cria a máquina de estados.
    ///
    /// `pin` é obrigatório quando não há segredo guardado; com segredo, ele é ignorado e a
    /// troca vira retomada. `conhecido` é o que a casca leu do disco para este par — no lado
    /// anfitrião, o `DeviceId` do par só é conhecido quando o `Hello` chega, então passe a
    /// tabela inteira por [`Pairing::with_store`].
    pub fn new(
        role: Role,
        eu: DeviceId,
        pin: Option<Pin>,
        conhecido: Option<KnownPeer>,
    ) -> Result<Self> {
        let mut semente = [0u8; 32];
        getrandom::fill(&mut semente)
            .map_err(|e| Error::Pairing(format!("sem entropia do sistema: {e}")))?;
        let sk = StaticSecret::from(semente);
        let pk = PublicKey::from(&sk);

        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|e| Error::Pairing(format!("sem entropia do sistema: {e}")))?;

        Ok(Pairing {
            role,
            eu,
            pin,
            conhecido,
            sk,
            pk,
            nonce,
            estado: Estado::Inicio,
        })
    }

    /// Primeira mensagem, que só o `Guest` manda. O `Host` fica esperando.
    pub fn open(&mut self) -> Result<PairFrame> {
        if self.role != Role::Guest {
            return Err(Error::Pairing("só o convidado abre o pareamento".into()));
        }
        if self.estado != Estado::Inicio {
            return Err(Error::Pairing("pareamento já começou".into()));
        }
        // **PIN digitado tem precedência sobre a retomada, e isso é o conserto de um beco sem
        // saída medido em 31/08/2026.**
        //
        // Antes, a existência de um segredo guardado decidia sozinha: havia segredo, retomava, e o
        // PIN nunca era olhado. Isso mata o par para sempre quando os dois lados **discordam** do
        // segredo — cada um tem um, e são diferentes. O convidado recebe um `ResumeChallenge` com
        // MAC que não confere, cai em "o aparelho do outro lado não é o que foi pareado", e a
        // conexão morre. Com o PIN certo na mão e sem nenhum efeito, porque este `if` nunca chegava
        // nele.
        //
        // Acontece de verdade e não é caso de laboratório: o receptor guarda **um** segredo por
        // par, então espelhar do PC, depois do celular, e voltar para o PC basta. Foi assim que
        // isto apareceu — o iPad guardou o segredo do pareamento com o Dell e o Android de bancada
        // ainda tinha o antigo. A dívida 22 já tinha consertado a **outra** metade do beco ("o
        // anfitrião não me conhece", que vira convite a recomeçar); esta é a metade que faltava:
        // "nós dois nos conhecemos e discordamos".
        //
        // **Não é enfraquecimento.** O PIN só está aqui porque uma pessoa o digitou nesta
        // tentativa, olhando para a tela do outro aparelho: digitar o PIN *é* dizer "pareie agora".
        // O caminho contrário — MAC de retomada inválido virar convite a digitar PIN — continua
        // proibido de propósito, e tem teste que falha se alguém o abrir
        // (`mac_de_retomada_invalido_continua_sendo_recusa_e_nao_convite`): lá quem falha é o outro
        // lado, aqui quem decide é o dono do aparelho.
        //
        // Custo: uma reconexão que carregue um PIN antigo num campo de texto vira pareamento novo
        // em vez de retomada — um aperto de mão a mais, medido em ~94 ms (1502 contra 1408 ms), e
        // nenhuma perda de função.
        if self.pin.is_some() {
            self.estado = Estado::EsperandoAck;
            return Ok(PairFrame::Hello {
                device_id: self.eu.clone(),
                public_key: hex_encode(self.pk.as_bytes()),
                nonce: hex_encode(&self.nonce),
            });
        }
        if self.conhecido.is_some() {
            self.estado = Estado::EsperandoDesafio;
            Ok(PairFrame::Resume {
                device_id: self.eu.clone(),
                nonce: hex_encode(&self.nonce),
            })
        } else {
            if self.pin.is_none() {
                return Err(Error::Pairing(
                    "aparelho desconhecido e sem PIN: não dá para parear".into(),
                ));
            }
            self.estado = Estado::EsperandoAck;
            Ok(PairFrame::Hello {
                device_id: self.eu.clone(),
                public_key: hex_encode(self.pk.as_bytes()),
                nonce: hex_encode(&self.nonce),
            })
        }
    }

    /// Consome uma mensagem da outra ponta.
    pub fn step(&mut self, quadro: PairFrame) -> Result<Step> {
        let r = self.step_interno(quadro);
        if r.is_err() {
            self.estado = Estado::Falhou;
        }
        r
    }

    fn step_interno(&mut self, quadro: PairFrame) -> Result<Step> {
        if let PairFrame::Fail { motivo } = quadro {
            return Err(Error::Pairing(format!("a outra ponta recusou: {motivo}")));
        }

        match (
            self.role,
            core::mem::replace(&mut self.estado, Estado::Falhou),
            quadro,
        ) {
            // ---- caminho do PIN ----
            (
                Role::Host,
                Estado::Inicio,
                PairFrame::Hello {
                    device_id,
                    public_key,
                    nonce,
                },
            ) => {
                let pin = self
                    .pin
                    .clone()
                    .ok_or_else(|| Error::Pairing("o anfitrião não tem PIN ativo".into()))?;
                let pk_guest: [u8; 32] = hex_decode(&public_key)?;
                let nonce_guest: [u8; NONCE_LEN] = hex_decode(&nonce)?;

                let chave = derivar(
                    &self.sk,
                    &pk_guest,
                    &pin,
                    // A transcrição é sempre na mesma ordem, independente do papel: convidado
                    // primeiro. Sem isso, os dois lados derivariam chaves diferentes.
                    &Partes {
                        guest_id: &device_id,
                        host_id: &self.eu,
                        pk_guest: &pk_guest,
                        pk_host: self.pk.as_bytes(),
                        nonce_guest: &nonce_guest,
                        nonce_host: &self.nonce,
                    },
                )?;

                self.estado = Estado::EsperandoConfirm {
                    chave,
                    par: device_id,
                };
                Ok(Step {
                    reply: Some(PairFrame::Ack {
                        device_id: self.eu.clone(),
                        public_key: hex_encode(self.pk.as_bytes()),
                        nonce: hex_encode(&self.nonce),
                    }),
                    done: None,
                })
            }

            (
                Role::Guest,
                Estado::EsperandoAck,
                PairFrame::Ack {
                    device_id,
                    public_key,
                    nonce,
                },
            ) => {
                let pin = self
                    .pin
                    .clone()
                    .ok_or_else(|| Error::Pairing("sem PIN para confirmar".into()))?;
                let pk_host: [u8; 32] = hex_decode(&public_key)?;
                let nonce_host: [u8; NONCE_LEN] = hex_decode(&nonce)?;

                let chave = derivar(
                    &self.sk,
                    &pk_host,
                    &pin,
                    &Partes {
                        guest_id: &self.eu,
                        host_id: &device_id,
                        pk_guest: self.pk.as_bytes(),
                        pk_host: &pk_host,
                        nonce_guest: &self.nonce,
                        nonce_host: &nonce_host,
                    },
                )?;

                let mac = confirmacao(&chave, Role::Guest);
                self.estado = Estado::EsperandoConfirmAck {
                    chave,
                    par: device_id,
                };
                Ok(Step {
                    reply: Some(PairFrame::Confirm {
                        mac: hex_encode(&mac),
                    }),
                    done: None,
                })
            }

            (Role::Host, Estado::EsperandoConfirm { chave, par }, PairFrame::Confirm { mac }) => {
                let recebido: [u8; MAC_LEN] = hex_decode(&mac)?;
                let esperado = confirmacao(&chave, Role::Guest);
                if recebido.ct_eq(&esperado).unwrap_u8() != 1 {
                    // Sem "tente de novo": uma tentativa por conexão é o que mantém a chance
                    // em 1/10⁶. Quem chama derruba a sinalização e gera outro PIN.
                    //
                    // **Dívida 29.** Este é o **único** ponto do núcleo em que o PIN de fato não
                    // confere, e agora é o único que produz `Error::WrongPin`. Todo o resto do
                    // módulo que devolvia `Error::Pairing` continua devolvendo — e é isso que
                    // torna o status confiável: quem receber `WrongPin` sabe que foi o PIN.
                    return Err(Error::WrongPin("o PIN não conferiu".into()));
                }
                let resposta = confirmacao(&chave, Role::Host);
                let segredo = chave.segredo;
                self.estado = Estado::Pronto;
                Ok(Step {
                    reply: Some(PairFrame::ConfirmAck {
                        mac: hex_encode(&resposta),
                    }),
                    done: Some(PairOutcome {
                        peer: par,
                        secret: segredo,
                        novo: true,
                    }),
                })
            }

            (
                Role::Guest,
                Estado::EsperandoConfirmAck { chave, par },
                PairFrame::ConfirmAck { mac },
            ) => {
                let recebido: [u8; MAC_LEN] = hex_decode(&mac)?;
                let esperado = confirmacao(&chave, Role::Host);
                if recebido.ct_eq(&esperado).unwrap_u8() != 1 {
                    return Err(Error::Pairing(
                        "o outro aparelho não provou saber o PIN".into(),
                    ));
                }
                let segredo = chave.segredo;
                self.estado = Estado::Pronto;
                Ok(Step {
                    reply: None,
                    done: Some(PairOutcome {
                        peer: par,
                        secret: segredo,
                        novo: true,
                    }),
                })
            }

            // ---- caminho da retomada ----
            //
            // **Dívida 22.** Um `Resume` de aparelho que este anfitrião não conhece não é
            // recusa: é o caso em que o segredo se perdeu de um lado só — troca de aparelho,
            // reinstalação, ou a dessincronia do `pares.json` da dívida 23. Cair aqui deixava o
            // usuário sem saída, porque o produto não oferece "digitar o PIN de novo".
            //
            // Então o anfitrião convida a recomeçar pelo PIN, **na mesma conexão**, e volta ao
            // estado inicial para receber o `Hello` que vem a seguir.
            (Role::Host, Estado::Inicio, PairFrame::Resume { device_id, nonce })
                if self.conhecido.is_none() =>
            {
                let _ = nonce;
                if self.pin.is_none() {
                    return Err(Error::NeedsPin(format!(
                        "o aparelho {} não está pareado aqui e não há PIN ativo para recomeçar",
                        device_id.0
                    )));
                }
                self.estado = Estado::Inicio;
                Ok(Step {
                    reply: Some(PairFrame::NeedsPin),
                    done: None,
                })
            }

            (Role::Host, Estado::Inicio, PairFrame::Resume { device_id, nonce }) => {
                let conhecido = self.conhecido.clone().ok_or_else(|| {
                    Error::Pairing(format!("aparelho {} não está pareado aqui", device_id.0))
                })?;
                if conhecido.id != device_id {
                    return Err(Error::Pairing(format!(
                        "segredo carregado é do aparelho {}, não de {}",
                        conhecido.id.0, device_id.0
                    )));
                }
                let segredo = conhecido.secret;
                let nonce_guest: [u8; NONCE_LEN] = hex_decode(&nonce)?;
                let mac = retomada(&segredo, Role::Host, &nonce_guest, &self.nonce);
                self.estado = Estado::EsperandoProva {
                    par: device_id,
                    segredo,
                    nonce_guest,
                    nonce_host: self.nonce,
                };
                Ok(Step {
                    reply: Some(PairFrame::ResumeChallenge {
                        nonce: hex_encode(&self.nonce),
                        mac: hex_encode(&mac),
                    }),
                    done: None,
                })
            }

            (Role::Guest, Estado::EsperandoDesafio, PairFrame::ResumeChallenge { nonce, mac }) => {
                let conhecido = self
                    .conhecido
                    .clone()
                    .ok_or_else(|| Error::Pairing("sem segredo guardado".into()))?;
                let segredo = conhecido.secret;
                let nonce_host: [u8; NONCE_LEN] = hex_decode(&nonce)?;
                let recebido: [u8; MAC_LEN] = hex_decode(&mac)?;
                let esperado = retomada(&segredo, Role::Host, &self.nonce, &nonce_host);
                if recebido.ct_eq(&esperado).unwrap_u8() != 1 {
                    return Err(Error::Pairing(
                        "o aparelho do outro lado não é o que foi pareado".into(),
                    ));
                }
                let prova = retomada(&segredo, Role::Guest, &self.nonce, &nonce_host);
                self.estado = Estado::EsperandoOk {
                    par: conhecido.id,
                    segredo,
                };
                Ok(Step {
                    reply: Some(PairFrame::ResumeProof {
                        mac: hex_encode(&prova),
                    }),
                    done: None,
                })
            }

            (
                Role::Host,
                Estado::EsperandoProva {
                    par,
                    segredo,
                    nonce_guest,
                    nonce_host,
                },
                PairFrame::ResumeProof { mac },
            ) => {
                let recebido: [u8; MAC_LEN] = hex_decode(&mac)?;
                let esperado = retomada(&segredo, Role::Guest, &nonce_guest, &nonce_host);
                if recebido.ct_eq(&esperado).unwrap_u8() != 1 {
                    return Err(Error::Pairing("prova de retomada inválida".into()));
                }
                self.estado = Estado::Pronto;
                Ok(Step {
                    reply: Some(PairFrame::Ok),
                    done: Some(PairOutcome {
                        peer: par,
                        secret: segredo,
                        novo: false,
                    }),
                })
            }

            // **Dívida 22, o outro lado.** O anfitrião não nos conhece mais. Se o usuário já tem
            // o PIN na mão, a retomada vira pareamento novo sem que ninguém precise reconectar;
            // se não tem, o erro é [`Error::NeedsPin`] — que a casca traduz em "peça o PIN",
            // não em "falhou".
            (Role::Guest, Estado::EsperandoDesafio, PairFrame::NeedsPin) => {
                if self.pin.is_none() {
                    self.conhecido = None;
                    return Err(Error::NeedsPin(
                        "o outro aparelho não reconhece mais este pareamento; peça o PIN de novo"
                            .into(),
                    ));
                }
                // O segredo guardado não vale mais nada: seguir com ele seria tentar a retomada
                // outra vez, no meio do caminho do PIN.
                self.conhecido = None;
                self.estado = Estado::EsperandoAck;
                Ok(Step {
                    reply: Some(PairFrame::Hello {
                        device_id: self.eu.clone(),
                        public_key: hex_encode(self.pk.as_bytes()),
                        nonce: hex_encode(&self.nonce),
                    }),
                    done: None,
                })
            }

            (Role::Guest, Estado::EsperandoOk { par, segredo }, PairFrame::Ok) => {
                self.estado = Estado::Pronto;
                Ok(Step {
                    reply: None,
                    done: Some(PairOutcome {
                        peer: par,
                        secret: segredo,
                        novo: false,
                    }),
                })
            }

            (_, estado, quadro) => Err(Error::Pairing(format!(
                "mensagem fora de ordem: {quadro:?} em {estado:?}"
            ))),
        }
    }

    pub fn is_done(&self) -> bool {
        self.estado == Estado::Pronto
    }
}

impl Pairing {
    /// Versão para o anfitrião, que só descobre com quem fala quando o `Hello`/`Resume` chega.
    ///
    /// Espia o primeiro quadro para achar o segredo guardado do par antes de decidir se a troca
    /// é por PIN ou por retomada.
    pub fn with_store(
        role: Role,
        eu: DeviceId,
        pin: Option<Pin>,
        store: &PairedPeers,
        primeiro: &PairFrame,
    ) -> Result<Self> {
        let par = match primeiro {
            PairFrame::Hello { device_id, .. } | PairFrame::Resume { device_id, .. } => {
                Some(device_id)
            }
            _ => None,
        };
        let conhecido = par.and_then(|id| {
            store.get(id).map(|secret| KnownPeer {
                id: id.clone(),
                secret,
            })
        });
        Pairing::new(role, eu, pin, conhecido)
    }
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum Entrada {
    /// Formato do M0 ao M3: só o segredo em hex.
    Simples(String),
    /// Formato com carimbo de quando o pareamento foi feito, em milissegundos desde a época.
    Datada { secret: String, updated_ms: u64 },
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
        hex_decode::<PAIR_SECRET_LEN>(entrada.secret()).ok()
    }

    pub fn insert(&mut self, resultado: &PairOutcome) {
        self.pares.insert(
            resultado.peer.0.clone(),
            Entrada::Datada {
                secret: hex_encode(&resultado.secret),
                updated_ms: agora_ms(),
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
                Some(minha) => entrada.quando() > minha.quando(),
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

struct Partes<'a> {
    guest_id: &'a DeviceId,
    host_id: &'a DeviceId,
    pk_guest: &'a [u8; 32],
    pk_host: &'a [u8; 32],
    nonce_guest: &'a [u8; NONCE_LEN],
    nonce_host: &'a [u8; NONCE_LEN],
}

/// Transcrição: tudo que os dois lados viram, na mesma ordem dos dois lados.
///
/// Os ids entram com o tamanho na frente (`u16` little-endian). Sem isso, `("ab","c")` e
/// `("a","bc")` dariam a mesma transcrição — a ambiguidade clássica de concatenar campos de
/// tamanho variável, e um caminho para um atacante casar duas trocas diferentes.
fn transcricao(partes: &Partes<'_>) -> Vec<u8> {
    let mut t = Vec::with_capacity(160);
    t.extend_from_slice(ROTULO_TRANSCRICAO);
    t.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    for id in [partes.guest_id, partes.host_id] {
        let bytes = id.0.as_bytes();
        // Truncar em 65535 é seguro: um `DeviceId` maior que isso já é entrada absurda, e o
        // `as u16` sem checagem esconderia o problema em vez de mostrá-lo.
        let n = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&bytes[..usize::from(n)]);
    }
    t.extend_from_slice(partes.pk_guest);
    t.extend_from_slice(partes.pk_host);
    t.extend_from_slice(partes.nonce_guest);
    t.extend_from_slice(partes.nonce_host);
    t
}

fn derivar(
    sk: &StaticSecret,
    pk_outro: &[u8; 32],
    pin: &Pin,
    partes: &Partes<'_>,
) -> Result<Chaves> {
    let compartilhado = sk.diffie_hellman(&PublicKey::from(*pk_outro));
    // Chave pública de ordem baixa faz o X25519 devolver zero: o "segredo" seria público e o
    // pareamento inteiro cairia para a força bruta do PIN. Recusar é o comportamento certo.
    if !compartilhado.was_contributory() {
        return Err(Error::Pairing(
            "chave pública inválida (resultado X25519 degenerado)".into(),
        ));
    }

    let t = transcricao(partes);

    let mut ikm = Vec::with_capacity(32 + PIN_DIGITS);
    ikm.extend_from_slice(compartilhado.as_bytes());
    ikm.extend_from_slice(&pin.bytes());

    let hk = Hkdf::<Sha256>::new(Some(&t), &ikm);

    let mut confirmar = [0u8; 32];
    let mut segredo = [0u8; PAIR_SECRET_LEN];
    hk.expand(ROTULO_CONFIRMAR, &mut confirmar)
        .map_err(|_| Error::Pairing("HKDF recusou o tamanho da chave".into()))?;
    hk.expand(ROTULO_SEGREDO, &mut segredo)
        .map_err(|_| Error::Pairing("HKDF recusou o tamanho da chave".into()))?;

    Ok(Chaves {
        confirmar,
        segredo,
        transcricao: t,
    })
}

fn confirmacao(chave: &Chaves, quem: Role) -> [u8; MAC_LEN] {
    let mut mac = HmacSha256::new_from_slice(&chave.confirmar)
        .expect("HMAC-SHA256 aceita chave de qualquer tamanho");
    mac.update(&[quem.tag()]);
    mac.update(&chave.transcricao);
    mac.finalize().into_bytes().into()
}

fn retomada(
    segredo: &[u8; PAIR_SECRET_LEN],
    quem: Role,
    nonce_guest: &[u8; NONCE_LEN],
    nonce_host: &[u8; NONCE_LEN],
) -> [u8; MAC_LEN] {
    let mut mac =
        HmacSha256::new_from_slice(segredo).expect("HMAC-SHA256 aceita chave de qualquer tamanho");
    mac.update(ROTULO_RETOMADA);
    mac.update(&[quem.tag()]);
    mac.update(nonce_guest);
    mac.update(nonce_host);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_de_pareamento_nao_expoe_segredos_nem_identidade() {
        let secreto = "a1".repeat(PAIR_SECRET_LEN);
        for entrada in [
            serde_json::json!(secreto),
            serde_json::json!({"secret": secreto, "updated_ms": 123}),
        ] {
            let json = serde_json::json!({"pares": {"pessoa-device": entrada}}).to_string();
            let pares: PairedPeers = serde_json::from_str(&json).expect("formato de armazenamento");
            assert_eq!(format!("{pares:?}"), "PairedPeers { quantidade: 1 }");
            // Diagnóstico não modifica o formato persistido: hex é representação, não cifra.
            assert_eq!(
                serde_json::to_value(&pares).unwrap(),
                serde_json::from_str::<serde_json::Value>(&json).unwrap()
            );
        }
        let quadro = PairFrame::Hello {
            device_id: DeviceId("pessoa-device".into()),
            public_key: secreto.clone(),
            nonce: "nonce-privado".into(),
        };
        assert_eq!(format!("{quadro:?}"), "Hello");
        assert!(serde_json::to_string(&quadro).unwrap().contains(&secreto));
        for quadro in [
            PairFrame::Confirm {
                mac: secreto.clone(),
            },
            PairFrame::Fail {
                motivo: "PIN livre 901234".into(),
            },
        ] {
            let log = format!("{quadro:?}");
            assert!(!log.contains(&secreto) && !log.contains("901234"));
        }
        let resultado = PairOutcome {
            peer: DeviceId("pessoa-device".into()),
            secret: [0xa1; PAIR_SECRET_LEN],
            novo: true,
        };
        let conhecido = KnownPeer {
            id: resultado.peer.clone(),
            secret: resultado.secret,
        };
        assert!(!format!("{resultado:?} {conhecido:?}").contains("pessoa-device"));
        assert_eq!(
            format!("{:?}", Pin::parse("901234").unwrap()),
            "Pin(******)"
        );
    }

    fn ids() -> (DeviceId, DeviceId) {
        (
            DeviceId("guest-macbook".into()),
            DeviceId("host-dell-g3".into()),
        )
    }

    /// Roda a troca inteira entre duas máquinas de estado, sem rede.
    fn trocar(
        guest: &mut Pairing,
        host: &mut Pairing,
    ) -> Result<(Option<PairOutcome>, Option<PairOutcome>)> {
        let mut do_guest = Some(guest.open()?);
        let mut do_host: Option<PairFrame> = None;
        let mut r_guest = None;
        let mut r_host = None;

        for _ in 0..8 {
            if let Some(q) = do_guest.take() {
                let passo = host.step(q)?;
                if let Some(o) = passo.done {
                    r_host = Some(o);
                }
                do_host = passo.reply;
            }
            if let Some(q) = do_host.take() {
                let passo = guest.step(q)?;
                if let Some(o) = passo.done {
                    r_guest = Some(o);
                }
                do_guest = passo.reply;
            }
            if do_guest.is_none() && do_host.is_none() {
                break;
            }
        }
        Ok((r_guest, r_host))
    }

    #[test]
    fn pin_correto_deriva_o_mesmo_segredo_dos_dois_lados() {
        let (g, h) = ids();
        let pin = Pin::generate().expect("pin");
        let mut guest = Pairing::new(Role::Guest, g, Some(pin.clone()), None).expect("guest");
        let mut host = Pairing::new(Role::Host, h, Some(pin), None).expect("host");

        let (rg, rh) = trocar(&mut guest, &mut host).expect("troca");
        let rg = rg.expect("convidado conclui");
        let rh = rh.expect("anfitrião conclui");

        assert_eq!(
            rg.secret, rh.secret,
            "os dois lados derivam o mesmo segredo"
        );
        assert!(rg.novo && rh.novo);
        assert_eq!(rg.peer.0, "host-dell-g3");
        assert_eq!(rh.peer.0, "guest-macbook");
        assert!(guest.is_done() && host.is_done());
    }

    #[test]
    fn pin_errado_e_recusado() {
        let (g, h) = ids();
        let mut guest = Pairing::new(
            Role::Guest,
            g,
            Some(Pin::parse("123456").expect("pin")),
            None,
        )
        .expect("guest");
        let mut host = Pairing::new(
            Role::Host,
            h,
            Some(Pin::parse("654321").expect("pin")),
            None,
        )
        .expect("host");

        let erro = trocar(&mut guest, &mut host).expect_err("tem de falhar");
        // **Dívida 29.** Era `Error::Pairing`, o balaio. Agora é o único caso do núcleo em que o
        // PIN de fato não confere, e é o único que pode dizer "digite de novo".
        assert!(matches!(erro, Error::WrongPin(_)), "erro foi {erro:?}");
    }

    /// **Dívida 29, o caso que a frente do Windows exercitou.**
    ///
    /// O convidado pede retomada com o segredo guardado; o anfitrião já esqueceu o par **e não
    /// tem PIN ativo** para convidar a recomeçar. O conselho certo é "peça um PIN novo no outro
    /// aparelho" — o **oposto** de "digite o PIN de novo".
    ///
    /// Repare no que este teste fixa: o erro do anfitrião **não** é `WrongPin` nem `Pairing`. O
    /// PIN nunca foi digitado nesta troca; dizer que ele não conferiu seria falso, e foi
    /// exatamente o texto que a frente do Windows viu com o PIN certo.
    #[test]
    fn par_esquecido_sem_pin_ativo_nao_e_pin_errado() {
        let (g, h) = ids();
        let fantasma = KnownPeer {
            id: h.clone(),
            secret: [7u8; PAIR_SECRET_LEN],
        };
        let mut guest = Pairing::new(Role::Guest, g, None, Some(fantasma)).expect("guest");
        // Anfitrião sem par guardado e **sem PIN ativo**.
        let mut host = Pairing::new(Role::Host, h, None, None).expect("host");

        let erro = trocar(&mut guest, &mut host).expect_err("tem de falhar");
        assert!(matches!(erro, Error::NeedsPin(_)), "erro foi {erro:?}");
        assert!(
            !matches!(erro, Error::WrongPin(_) | Error::Pairing(_)),
            "o PIN não foi digitado nesta troca; chamá-lo de errado é o defeito da dívida 29"
        );
    }

    /// **Dívida 29, nota de segurança.** MAC de retomada inválido **não** vira convite a
    /// recomeçar por PIN.
    ///
    /// Um anfitrião que falha a prova de retomada pode ser um impostor. Devolver `NeedsPin` ali
    /// ofereceria um caminho de rebaixamento: o convidado passaria a digitar um PIN mostrado pelo
    /// **impostor**. Só quem admite não conhecer o par convida a recomeçar.
    #[test]
    fn mac_de_retomada_invalido_continua_sendo_recusa_e_nao_convite() {
        let (g, h) = ids();
        // Os dois lados "se conhecem", com segredos diferentes: a prova não vai fechar.
        let do_guest = KnownPeer {
            id: h.clone(),
            secret: [1u8; PAIR_SECRET_LEN],
        };
        let do_host = KnownPeer {
            id: g.clone(),
            secret: [2u8; PAIR_SECRET_LEN],
        };
        let mut guest = Pairing::new(Role::Guest, g, None, Some(do_guest)).expect("guest");
        let mut host = Pairing::new(Role::Host, h, None, Some(do_host)).expect("host");

        let erro = trocar(&mut guest, &mut host).expect_err("tem de falhar");
        assert!(
            matches!(erro, Error::Pairing(_)),
            "esperava recusa genérica, veio {erro:?}"
        );
    }

    /// **Dívida 22.** Retomada falhada não pode ser beco sem saída.
    ///
    /// O convidado acha que está pareado; o anfitrião perdeu o segredo (troca de aparelho,
    /// reinstalação, ou a dessincronia do `pares.json` da dívida 23). Antes, isto morria em
    /// "aparelho não está pareado aqui" e o usuário ficava sem forma de digitar o PIN de novo.
    #[test]
    fn retomada_de_aparelho_desconhecido_cai_de_volta_no_pin() {
        let (g, h) = ids();
        let pin = Pin::parse("246810").expect("pin");
        // O convidado tem um segredo que o anfitrião nunca viu.
        let fantasma = KnownPeer {
            id: h.clone(),
            secret: [7u8; PAIR_SECRET_LEN],
        };

        let mut guest =
            Pairing::new(Role::Guest, g, Some(pin.clone()), Some(fantasma)).expect("guest");
        let mut host = Pairing::new(Role::Host, h, Some(pin), None).expect("host");

        let (rg, rh) = trocar(&mut guest, &mut host).expect("a troca tinha de se recuperar");
        let rg = rg.expect("convidado conclui");
        let rh = rh.expect("anfitrião conclui");

        assert_eq!(
            rg.secret, rh.secret,
            "depois da volta ao PIN os dois lados têm de derivar o mesmo segredo"
        );
        assert!(
            rg.novo && rh.novo,
            "o pareamento nasceu de um PIN, então é novo dos dois lados — e é isso que faz a \
             casca gravar por cima do segredo velho"
        );
        assert_ne!(
            rg.secret, [7u8; PAIR_SECRET_LEN],
            "o segredo velho não podia sobreviver"
        );
    }

    /// Sem PIN na mão, o convidado não tem como recomeçar sozinho — mas o erro precisa dizer
    /// **o que fazer**, e não só que falhou. É o que a casca usa para abrir a tela do PIN.
    #[test]
    fn retomada_falhada_sem_pin_pede_o_pin_em_vez_de_so_falhar() {
        let (g, h) = ids();
        let fantasma = KnownPeer {
            id: h.clone(),
            secret: [7u8; PAIR_SECRET_LEN],
        };

        let mut guest = Pairing::new(Role::Guest, g, None, Some(fantasma)).expect("guest");
        let mut host = Pairing::new(
            Role::Host,
            h,
            Some(Pin::parse("135791").expect("pin")),
            None,
        )
        .expect("host");

        let erro = trocar(&mut guest, &mut host).expect_err("não tinha como fechar");
        assert!(
            matches!(erro, Error::NeedsPin(_)),
            "esperava NeedsPin, veio {erro:?}"
        );
    }

    /// A retomada que **funciona** continua funcionando: o caminho novo não pode virar desvio
    /// para quem já está pareado dos dois lados.
    /// **O beco sem saída dos dois segredos que discordam, medido em bancada em 31/08/2026.**
    ///
    /// O iPad guardava o segredo do pareamento com o Dell; o Android de bancada ainda tinha o
    /// segredo antigo do iPad. Os dois lados se conheciam, e discordavam. A corrida morria em
    /// "o aparelho do outro lado não é o que foi pareado" — **com `--pin` na linha de comando**,
    /// porque `open()` decidia pela existência do segredo e nunca olhava o PIN.
    ///
    /// Não é caso de laboratório: o receptor guarda um segredo por par, então espelhar do PC,
    /// depois do celular, e voltar para o PC é suficiente para produzir isto.
    #[test]
    fn segredos_que_discordam_nao_matam_o_par_quando_ha_pin() {
        let (g, h) = ids();
        let pin = Pin::parse("314159").expect("pin");

        let mut guest = Pairing::new(
            Role::Guest,
            g.clone(),
            Some(pin.clone()),
            Some(KnownPeer {
                id: h.clone(),
                secret: [1u8; PAIR_SECRET_LEN],
            }),
        )
        .expect("guest");
        let mut host = Pairing::new(
            Role::Host,
            h,
            Some(pin),
            Some(KnownPeer {
                id: g,
                // **Diferente do que o convidado guardou.** É o caso real.
                secret: [2u8; PAIR_SECRET_LEN],
            }),
        )
        .expect("host");

        let (rg, rh) = trocar(&mut guest, &mut host).expect("o PIN tem de resgatar o par");
        let rg = rg.expect("guest");
        let rh = rh.expect("host");
        assert_eq!(
            rg.secret, rh.secret,
            "os dois lados saem com o mesmo segredo"
        );
        assert!(rh.novo, "é pareamento novo, não retomada");
        assert_ne!(
            rg.secret, [1u8; PAIR_SECRET_LEN],
            "o segredo velho do convidado foi substituído"
        );
        assert_ne!(
            rg.secret, [2u8; PAIR_SECRET_LEN],
            "o segredo velho do anfitrião foi substituído"
        );
    }

    /// PIN digitado significa "pareie agora", e por isso o primeiro quadro é `Hello` e não
    /// `Resume`, mesmo havendo segredo guardado. Se alguém inverter a precedência outra vez, este
    /// teste falha antes de a bancada perder um dia.
    #[test]
    fn pin_digitado_tem_precedencia_sobre_a_retomada() {
        let (g, h) = ids();
        let mut guest = Pairing::new(
            Role::Guest,
            g,
            Some(Pin::parse("314159").expect("pin")),
            Some(KnownPeer {
                id: h,
                secret: [42u8; PAIR_SECRET_LEN],
            }),
        )
        .expect("guest");

        match guest.open().expect("abre") {
            PairFrame::Hello { .. } => {}
            outro => panic!("com PIN digitado o convidado tem de mandar Hello, veio {outro:?}"),
        }
    }

    #[test]
    fn retomada_com_segredo_certo_continua_dispensando_o_pin() {
        let (g, h) = ids();
        let segredo = [42u8; PAIR_SECRET_LEN];

        let mut guest = Pairing::new(
            Role::Guest,
            g.clone(),
            None,
            Some(KnownPeer {
                id: h.clone(),
                secret: segredo,
            }),
        )
        .expect("guest");
        let mut host = Pairing::new(
            Role::Host,
            h,
            None,
            Some(KnownPeer {
                id: g,
                secret: segredo,
            }),
        )
        .expect("host");

        let (rg, rh) = trocar(&mut guest, &mut host).expect("retomada");
        assert_eq!(rg.expect("guest").secret, segredo);
        let rh = rh.expect("host");
        assert_eq!(rh.secret, segredo);
        assert!(!rh.novo, "retomada não é pareamento novo");
    }

    /// **Dívida 23, o caso ruim inteiro.** Duas origens do mesmo aparelho pareiam com o mesmo
    /// receptor, derivam segredos diferentes, e a atualização de uma se perde.
    ///
    /// O receptor guarda **um** segredo por `DeviceId`, o do pareamento mais recente. A fusão
    /// tem de convergir para ele — senão a origem que perdeu a corrida fica com um segredo que
    /// ninguém mais reconhece.
    #[test]
    fn a_fusao_converge_para_o_segredo_mais_recente() {
        let par = DeviceId("receptor-macbook".into());

        // A extension pareou primeiro.
        let mut extension = PairedPeers::new();
        extension.insert(&PairOutcome {
            peer: par.clone(),
            secret: [1u8; PAIR_SECRET_LEN],
            novo: true,
        });
        // ...e o app pareou depois, que é o segredo que o receptor guardou.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut app = PairedPeers::new();
        app.insert(&PairOutcome {
            peer: par.clone(),
            secret: [2u8; PAIR_SECRET_LEN],
            novo: true,
        });

        // A extension escreve fundindo, em vez de sobrescrever.
        let mut fundido = extension.clone();
        fundido.merge(&app);
        assert_eq!(
            fundido.get(&par),
            Some([2u8; PAIR_SECRET_LEN]),
            "a fusão tinha de ficar com o pareamento mais recente"
        );

        // E na ordem contrária dá o mesmo resultado: fusão não pode depender de quem escreve.
        let mut ao_contrario = app;
        ao_contrario.merge(&extension);
        assert_eq!(ao_contrario.get(&par), Some([2u8; PAIR_SECRET_LEN]));
    }

    /// A parte fácil, e que a casca perdia toda vez: chaves **diferentes** têm de sobreviver às
    /// duas escritas.
    #[test]
    fn a_fusao_nao_perde_par_que_so_um_lado_conhece() {
        let mut a = PairedPeers::new();
        a.insert(&PairOutcome {
            peer: DeviceId("mac".into()),
            secret: [1u8; PAIR_SECRET_LEN],
            novo: true,
        });
        let mut b = PairedPeers::new();
        b.insert(&PairOutcome {
            peer: DeviceId("dell".into()),
            secret: [2u8; PAIR_SECRET_LEN],
            novo: true,
        });

        a.merge(&b);
        assert_eq!(a.len(), 2);
        assert_eq!(a.get(&DeviceId("mac".into())), Some([1u8; PAIR_SECRET_LEN]));
        assert_eq!(
            a.get(&DeviceId("dell".into())),
            Some([2u8; PAIR_SECRET_LEN])
        );
    }

    /// Um `pares.json` gravado antes do carimbo tem de continuar carregando — é o arquivo que
    /// está no aparelho de todo mundo hoje.
    #[test]
    fn arquivo_do_formato_antigo_continua_sendo_lido() {
        let antigo = r#"{"pares":{"mac":"0101010101010101010101010101010101010101010101010101010101010101"}}"#;
        let lido = PairedPeers::from_json(antigo).expect("formato antigo");
        assert_eq!(
            lido.get(&DeviceId("mac".into())),
            Some([1u8; PAIR_SECRET_LEN])
        );

        // E perde para qualquer entrada com carimbo, porque veio de antes de existir carimbo.
        let mut novo = PairedPeers::new();
        novo.insert(&PairOutcome {
            peer: DeviceId("mac".into()),
            secret: [9u8; PAIR_SECRET_LEN],
            novo: true,
        });
        let mut fundido = lido;
        fundido.merge(&novo);
        assert_eq!(
            fundido.get(&DeviceId("mac".into())),
            Some([9u8; PAIR_SECRET_LEN])
        );
    }

    /// Esquecer um par é o que a casca oferece como "parear de novo" (dívida 22).
    #[test]
    fn esquecer_um_par_apaga_so_ele() {
        let mut p = PairedPeers::new();
        for (id, s) in [("mac", 1u8), ("dell", 2u8)] {
            p.insert(&PairOutcome {
                peer: DeviceId(id.into()),
                secret: [s; PAIR_SECRET_LEN],
                novo: true,
            });
        }
        p.remove(&DeviceId("mac".into()));
        assert_eq!(p.len(), 1);
        assert!(p.get(&DeviceId("mac".into())).is_none());
        assert_eq!(
            p.get(&DeviceId("dell".into())),
            Some([2u8; PAIR_SECRET_LEN])
        );
    }

    #[test]
    fn dois_pareamentos_com_o_mesmo_pin_dao_segredos_diferentes() {
        // Se não dessem, um segredo vazado comprometeria toda sessão futura com o mesmo PIN.
        let (g, h) = ids();
        let pin = Pin::parse("111111").expect("pin");

        let mut a1 = Pairing::new(Role::Guest, g.clone(), Some(pin.clone()), None).expect("g1");
        let mut b1 = Pairing::new(Role::Host, h.clone(), Some(pin.clone()), None).expect("h1");
        let (r1, _) = trocar(&mut a1, &mut b1).expect("troca 1");

        let mut a2 = Pairing::new(Role::Guest, g, Some(pin.clone()), None).expect("g2");
        let mut b2 = Pairing::new(Role::Host, h, Some(pin), None).expect("h2");
        let (r2, _) = trocar(&mut a2, &mut b2).expect("troca 2");

        assert_ne!(r1.expect("r1").secret, r2.expect("r2").secret);
    }

    #[test]
    fn retomada_dispensa_o_pin() {
        let (g, h) = ids();
        let pin = Pin::generate().expect("pin");
        let mut guest = Pairing::new(Role::Guest, g.clone(), Some(pin.clone()), None).expect("g");
        let mut host = Pairing::new(Role::Host, h.clone(), Some(pin), None).expect("h");
        let (rg, rh) = trocar(&mut guest, &mut host).expect("primeiro pareamento");
        let segredo = rg.expect("rg").secret;
        assert_eq!(segredo, rh.expect("rh").secret);

        // Segunda sessão: nenhum dos dois tem PIN, os dois têm o segredo.
        let mut guest2 = Pairing::new(
            Role::Guest,
            g.clone(),
            None,
            Some(KnownPeer {
                id: h.clone(),
                secret: segredo,
            }),
        )
        .expect("g2");
        let mut host2 = Pairing::new(
            Role::Host,
            h,
            None,
            Some(KnownPeer {
                id: g,
                secret: segredo,
            }),
        )
        .expect("h2");
        let (rg2, rh2) = trocar(&mut guest2, &mut host2).expect("retomada");
        let rg2 = rg2.expect("rg2");
        let rh2 = rh2.expect("rh2");
        assert!(!rg2.novo && !rh2.novo);
        assert_eq!(rg2.secret, segredo);
        assert_eq!(rh2.secret, segredo);
    }

    #[test]
    fn retomada_com_segredo_errado_e_recusada() {
        let (g, h) = ids();
        let mut guest = Pairing::new(
            Role::Guest,
            g.clone(),
            None,
            Some(KnownPeer {
                id: h.clone(),
                secret: [1u8; 32],
            }),
        )
        .expect("g");
        let mut host = Pairing::new(
            Role::Host,
            h,
            None,
            Some(KnownPeer {
                id: g,
                secret: [2u8; 32],
            }),
        )
        .expect("h");
        assert!(trocar(&mut guest, &mut host).is_err());
    }

    #[test]
    fn convidado_sem_pin_e_sem_segredo_nao_abre() {
        let (g, _) = ids();
        let mut guest = Pairing::new(Role::Guest, g, None, None).expect("g");
        assert!(guest.open().is_err());
    }

    #[test]
    fn anfitriao_nao_abre_a_troca() {
        let (_, h) = ids();
        let mut host =
            Pairing::new(Role::Host, h, Some(Pin::generate().expect("pin")), None).expect("h");
        assert!(host.open().is_err());
    }

    #[test]
    fn mensagem_fora_de_ordem_falha() {
        let (g, _) = ids();
        let mut guest =
            Pairing::new(Role::Guest, g, Some(Pin::generate().expect("pin")), None).expect("g");
        // `ConfirmAck` antes de qualquer coisa.
        assert!(guest
            .step(PairFrame::ConfirmAck {
                mac: hex_encode(&[0u8; 32])
            })
            .is_err());
    }

    #[test]
    fn fail_da_outra_ponta_vira_erro() {
        let (g, _) = ids();
        let mut guest =
            Pairing::new(Role::Guest, g, Some(Pin::generate().expect("pin")), None).expect("g");
        let erro = guest
            .step(PairFrame::Fail {
                motivo: "sem PIN ativo".into(),
            })
            .expect_err("tem de falhar");
        assert!(format!("{erro}").contains("sem PIN ativo"));
    }

    #[test]
    fn chave_publica_de_ordem_baixa_e_recusada() {
        let (g, h) = ids();
        let pin = Pin::generate().expect("pin");
        let mut host = Pairing::new(Role::Host, h, Some(pin), None).expect("h");
        // Todo-zeros é o ponto de ordem baixa clássico: o X25519 devolve zero.
        let erro = host
            .step(PairFrame::Hello {
                device_id: g,
                public_key: hex_encode(&[0u8; 32]),
                nonce: hex_encode(&[0u8; NONCE_LEN]),
            })
            .expect_err("tem de recusar");
        assert!(format!("{erro}").contains("degenerado"), "erro: {erro}");
    }

    #[test]
    fn pin_gerado_tem_digitos_validos() {
        for _ in 0..64 {
            let p = Pin::generate().expect("pin");
            let texto = p.to_display();
            assert_eq!(texto.len(), PIN_DIGITS);
            assert!(texto.chars().all(|c| c.is_ascii_digit()));
            assert_eq!(Pin::parse(&texto).expect("reparse"), p);
        }
    }

    #[test]
    fn pin_aceita_separadores_e_recusa_tamanho_errado() {
        assert_eq!(
            Pin::parse("123 456").expect("com espaço"),
            Pin::parse("123456").expect("sem")
        );
        assert!(Pin::parse("12345").is_err());
        assert!(Pin::parse("1234567").is_err());
        assert!(Pin::parse("12a456").is_err());
    }

    #[test]
    fn pin_nao_vaza_no_debug() {
        let p = Pin::parse("987654").expect("pin");
        assert_eq!(format!("{p:?}"), "Pin(******)");
        assert!(!format!("{p:?}").contains("987654"));
    }

    #[test]
    fn transcricao_nao_e_ambigua_entre_ids() {
        let pk = [7u8; 32];
        let n = [9u8; NONCE_LEN];
        let a = transcricao(&Partes {
            guest_id: &DeviceId("ab".into()),
            host_id: &DeviceId("c".into()),
            pk_guest: &pk,
            pk_host: &pk,
            nonce_guest: &n,
            nonce_host: &n,
        });
        let b = transcricao(&Partes {
            guest_id: &DeviceId("a".into()),
            host_id: &DeviceId("bc".into()),
            pk_guest: &pk,
            pk_host: &pk,
            nonce_guest: &n,
            nonce_host: &n,
        });
        assert_ne!(a, b);
    }

    #[test]
    fn store_sobrevive_ida_e_volta_por_json() {
        let mut store = PairedPeers::new();
        store.insert(&PairOutcome {
            peer: DeviceId("dell".into()),
            secret: [42u8; PAIR_SECRET_LEN],
            novo: true,
        });
        let texto = store.to_json().expect("json");
        let voltou = PairedPeers::from_json(&texto).expect("volta");
        assert_eq!(
            voltou.get(&DeviceId("dell".into())),
            Some([42u8; PAIR_SECRET_LEN])
        );
        assert_eq!(voltou.len(), 1);
    }

    #[test]
    fn anfitriao_recusa_retomada_de_outro_aparelho() {
        let (g, h) = ids();
        let mut host = Pairing::new(
            Role::Host,
            h,
            None,
            Some(KnownPeer {
                id: DeviceId("outro-aparelho".into()),
                secret: [3u8; 32],
            }),
        )
        .expect("h");
        let erro = host
            .step(PairFrame::Resume {
                device_id: g,
                nonce: hex_encode(&[0u8; NONCE_LEN]),
            })
            .expect_err("tem de recusar");
        assert!(format!("{erro}").contains("outro-aparelho"), "erro: {erro}");
    }
}
