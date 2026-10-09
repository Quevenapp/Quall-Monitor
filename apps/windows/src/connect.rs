//! Conexão do receptor com o núcleo: descoberta mDNS (com fallback por IP) e pareamento por PIN,
//! por cima de `quall_core::discovery`/`quall_core::session` — sem reinventar protocolo aqui.
//!
//! # O que este módulo não faz, e por quê
//!
//! O escopo desta frente pede consumo da track de mídia pelos nomes fixados em
//! `docs/contrato-track.md` (`ao_receber_quadro`, `pedir_idr`, `QuadroCodificado`). Neste
//! snapshot do repositório esse documento **não existe ainda** e `crates/quall-core` não expõe
//! nada com esses nomes — conferido em `crates/quall-core/src/{session,transport}.rs`: o que
//! existe hoje é um canal de dados genérico (`Session::send`/`Session::next_data`, bytes crus),
//! sem conceito de track de vídeo, sem `pedir_idr`. A Frente 1 está escrevendo essa API em
//! paralelo a esta entrega.
//!
//! O que **existe e já está provado em bancada** (M1, `quall-probe`) é a coreografia genérica:
//! descoberta mDNS com fallback por IP, sinalização, pareamento por PIN e canal de dados WebRTC
//! aberto — isso é `quall_core::session::conectar`, e é isso que este módulo usa de verdade, sem
//! inventar um segundo protocolo por cima. Inventar um framing próprio aqui seria repetir
//! exatamente o erro que `docs/contrato-sidecar.md` documenta: duas frentes escolhendo nomes
//! incompatíveis para a mesma coisa. Por isso este módulo para na borda do que o núcleo já
//! oferece, e devolve o [`Ready`] do núcleo pronto para o dia em que a track existir.
//!
//! Testado nesta entrega contra `quall-probe emitir` (macOS, hospedeiro) a partir do Dell G3 —
//! ver README.md, seção "Frente 6", para o resultado.

use std::net::SocketAddr;
use std::time::Duration;

use quall_core::discovery::{anuncio, endereco_manual, Browser};
use quall_core::error::{Error, Result};
use quall_core::pairing::{PairedPeers, Pin};
use quall_core::protocol::Capabilities;
use quall_core::session::{conectar, Ready, SessionConfig};
use quall_core::transport::TransportConfig;

/// Como achar o emissor.
pub enum Alvo {
    /// Endereço digitado — o fallback obrigatório para redes com mDNS bloqueado ou com AP
    /// isolation (o Firewall do Windows barrando UDP 5353 de entrada é exatamente esse caso,
    /// medido em `docs/bancada.md`).
    Ip(String),
    /// Procura por mDNS o primeiro aparelho que anuncia alguma fonte (tela ou câmera).
    Descobrir { timeout: Duration },
}

/// Conecta como receptor (`sink`): descobre ou usa o IP, pareia por PIN (ou retoma um par
/// conhecido) e sobe a sessão WebRTC até o canal de dados abrir. Devolve o [`Ready`] do núcleo —
/// o ponto exato em que, quando a track existir, o laço de decode passa a ler `QuadroCodificado`
/// dela em vez de ler bytes de um arquivo local.
pub fn conectar_como_receptor(
    alvo: Alvo,
    device_id: &str,
    display_name: &str,
    pin: Option<Pin>,
    known: PairedPeers,
) -> Result<Ready> {
    let destino: SocketAddr = match alvo {
        Alvo::Ip(texto) => endereco_manual(&texto)?,
        Alvo::Descobrir { timeout } => {
            let navegador = Browser::start()?;
            let achados = navegador.collect(timeout)?;
            navegador.stop();
            let escolhido = achados
                .into_iter()
                .find(|a| a.announcement.capabilities.screen_source || a.announcement.capabilities.camera_source)
                .ok_or_else(|| {
                    Error::Discovery(
                        "nenhum emissor apareceu por mDNS em tempo. Se a rede bloqueia mDNS \
                         (ex.: Firewall do Windows barrando UDP 5353 de entrada — ver \
                         docs/bancada.md), conecte pelo IP diretamente."
                            .into(),
                    )
                })?;
            escolhido
                .endpoint()
                .ok_or_else(|| Error::Discovery("o emissor não anunciou endereço utilizável".into()))?
        }
    };

    let eu = anuncio(
        device_id,
        display_name,
        Capabilities { screen_source: false, camera_source: false, sink: true },
    );

    conectar(
        destino,
        SessionConfig {
            announcement: eu,
            pin,
            known,
            transport: TransportConfig::default(),
            // Quem responde não declara tracks: as do outro lado chegam pela oferta, e `tracks` só
            // vale em `hospedar` (dívida 1 — não há renegociação, então quem conecta nunca emite
            // naquela sessão).
            tracks: Vec::new(),
            timeout: Duration::from_secs(60),
            // Detector de silêncio do caminho **desligado**, que é o padrão do núcleo:
            // tela parada legitimamente não produz quadro, e quem sabe se a origem produz
            // continuamente é a casca. Ver `SessionConfig::silencio_do_caminho`.
            silencio_do_caminho: None,
            // Esta sonda não tem botão de Cancelar; quem tem é o app (`emissor.rs`), e lá o
            // cancelamento é o `Cancelamento` de verdade. `Default` nasce sem cancelamento pedido.
            cancelamento: Default::default(),
        },
    )
}
