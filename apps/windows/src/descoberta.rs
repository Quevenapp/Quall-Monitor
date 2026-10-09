//! "Os aparelhos da rede": a lista que a metade **Exibir** da tela inicial mostra.
//!
//! # Por que uma thread, e não uma varredura no clique
//!
//! `Browser::collect(duracao)` existe e é o que `connect.rs` usa na sonda — ele bloqueia pelo prazo
//! inteiro e devolve o que achou. Numa sonda isso está certo; numa janela, não: bloquear a thread
//! da interface por três segundos congela o app, e fazer a varredura só no clique significa que a
//! pessoa clica em "Exibir" antes de o nome do outro aparelho ter aparecido.
//!
//! O que esta thread faz é o que `Browser::next_event` foi desenhado para: um evento por vez, com
//! prazo curto, publicando numa lista sob trava que a janela lê. `Ok(None)` do `next_event` é o
//! caso **normal** numa rede parada, não erro — está escrito no próprio núcleo
//! (`discovery.rs:303`), e tratá-lo como falha encheria a tela de aviso vermelho numa rede que só
//! não tem ninguém transmitindo.
//!
//! # Quem entra na lista
//!
//! Na lista de **exibir**: só quem anuncia tela (`screen_source`)
//! e **nenhum papel**. Um aparelho que só sabe exibir (`sink`) não tem o que nos mandar, e listá-lo
//! seria oferecer um clique que só pode falhar — a mesma regra que fez o seletor de monitor não
//! listar a câmera que o app não sabe abrir. Um prompter do teleprompter anuncia `papel` e nenhuma
//! capacidade; ele já ficava fora pela capacidade, e o papel o tira de novo, explicitamente
//! (`docs/contrato-teleprompter.md` §7: "receptores de vídeo devem esconder da lista quem anuncia
//! `papel`").
//!
//! Na lista do **controle do teleprompter** ([`Busca::de_prompters`]): o contrário — só quem
//! anuncia `papel = teleprompter`, com endereço, e nunca este próprio aparelho.
//!
//! # O que esta lista **não** dispensa
//!
//! O campo de endereço digitado. O `PROMPT.md` fixa o fallback por IP como **obrigatório**, não
//! como conveniência: rede com multicast bloqueado e isolamento de AP são casos reais, e nesta
//! própria bancada as corridas do emissor conectaram todas por IP — `mdns=true` só diz que o
//! anúncio subiu. Uma lista vazia é uma resposta possível e prevista, e a tela precisa continuar
//! utilizável nela.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use quall_core::discovery::{Browser, DiscoveredDevice, DiscoveryEvent};
use quall_core::protocol::Papel;

use crate::registro;

/// Quem entra numa lista. Ver o cabeçalho do módulo.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Filtro {
    /// A metade Exibir da janela principal: fontes de vídeo, sem papel.
    Video,
    /// O controle do teleprompter: prompters, menos este aparelho.
    Prompters { meu_id: String },
}

impl Filtro {
    fn aceita(&self, achado: &DiscoveredDevice) -> bool {
        let a = &achado.announcement;
        match self {
            Filtro::Video => a.capabilities.screen_source && a.papel.is_none(),
            Filtro::Prompters { meu_id } => a.papel == Some(Papel::Teleprompter) && a.device_id.0 != *meu_id,
        }
    }
}

/// Um aparelho que anunciou uma fonte na rede.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aparelho {
    pub nome: String,
    pub device_id: String,
    pub endereco: SocketAddr,
    /// Nome completo mDNS — é por ele que o `DiscoveryEvent::Lost` casa, não pelo nome de tela
    /// (que pode repetir entre dois computadores com o mesmo nome de máquina).
    pub fullname: String,
    pub tem_tela: bool,
    pub tem_camera: bool,
}

impl Aparelho {
    /// Texto da linha da lista.
    pub fn linha_da_lista(&self) -> String {
        let o_que = match (self.tem_tela, self.tem_camera) {
            (true, true) => "tela e câmera",
            (true, false) => "tela",
            (false, true) => "câmera",
            (false, false) => "—",
        };
        format!("{} — {} · {}", self.nome, o_que, self.endereco)
    }

    /// Texto da linha na lista do controle do teleprompter.
    pub fn linha_do_prompter(&self) -> String {
        format!("{} · {}", self.nome, self.endereco)
    }
}

/// O que a janela lê. Publicado, nunca consultado ao vivo — a mesma regra que `Estado` do emissor
/// segue: a janela não pergunta à rede dentro de um `WM_PAINT`.
pub struct ListaDeAparelhos {
    pub aparelhos: Vec<Aparelho>,
    /// Sobe quando o **conteúdo** da lista muda. A janela remonta o controle só nessa hora; um
    /// remonte a cada 100 ms fecharia a lista suspensa na cara de quem a abriu (é o mesmo defeito
    /// que `revisao_das_fontes` evita no seletor de monitor).
    pub revisao: u64,
    /// A busca está no ar? `false` com `motivo` preenchido é rede sem multicast — caso previsto.
    pub ativa: bool,
    pub motivo: String,
}

pub struct Busca {
    lista: Mutex<ListaDeAparelhos>,
    parar: AtomicBool,
    ligada: AtomicBool,
    filtro: Filtro,
}

impl Busca {
    /// A lista da metade **Exibir**: fontes de vídeo.
    pub fn nova() -> Arc<Self> {
        Busca::com_filtro(Filtro::Video)
    }

    /// A lista do **controle do teleprompter**: só prompters, menos este aparelho.
    pub fn de_prompters(meu_id: String) -> Arc<Self> {
        Busca::com_filtro(Filtro::Prompters { meu_id })
    }

    fn com_filtro(filtro: Filtro) -> Arc<Self> {
        Arc::new(Busca {
            lista: Mutex::new(ListaDeAparelhos {
                aparelhos: Vec::new(),
                revisao: 1,
                ativa: false,
                motivo: String::new(),
            }),
            parar: AtomicBool::new(false),
            ligada: AtomicBool::new(false),
            filtro,
        })
    }

    pub fn lista(&self) -> MutexGuard<'_, ListaDeAparelhos> {
        self.lista.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sobe a thread de navegação. Chamar duas vezes não sobe duas.
    pub fn comecar(self: &Arc<Self>) {
        if self.ligada.swap(true, Ordering::SeqCst) {
            return;
        }
        let eu = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("quall.mdns".into())
            .spawn(move || eu.correr());
    }

    /// Para a navegação. O `Browser` some junto com a thread.
    pub fn parar(&self) {
        self.parar.store(true, Ordering::SeqCst);
    }

    fn correr(self: Arc<Self>) {
        let navegador = match Browser::start() {
            Ok(b) => b,
            Err(erro) => {
                registro::linha(format!(
                    "mdns: a navegação não subiu ({erro}) — a lista fica vazia e o endereço \
                     digitado continua sendo o caminho"
                ));
                let mut l = self.lista();
                l.ativa = false;
                l.motivo = "Não consegui procurar na rede. Digite o endereço.".into(); // i18n: chave (traduzida ao mostrar)
                l.revisao += 1;
                return;
            }
        };
        {
            let mut l = self.lista();
            l.ativa = true;
            l.motivo.clear();
            l.revisao += 1;
        }
        registro::linha("mdns: navegando (_quall._tcp)");

        while !self.parar.load(Ordering::SeqCst) {
            // Prazo curto: é o que dá à thread a chance de ver o pedido de parada. `Ok(None)` é o
            // caso normal — rede parada não é erro.
            match navegador.next_event(Duration::from_millis(250)) {
                Ok(Some(DiscoveryEvent::Found(achado))) => {
                    let caps = &achado.announcement.capabilities;
                    if !self.filtro.aceita(&achado) {
                        continue;
                    }
                    let Some(endereco) = achado.endpoint() else {
                        registro::linha("mdns: aparelho anunciou sem endereço utilizável — fora da lista");
                        continue;
                    };
                    let novo = Aparelho {
                        nome: achado.announcement.display_name.clone(),
                        device_id: achado.announcement.device_id.0.clone(),
                        endereco,
                        fullname: achado.fullname.clone(),
                        tem_tela: caps.screen_source,
                        tem_camera: caps.camera_source,
                    };
                    let mut l = self.lista();
                    match l.aparelhos.iter().position(|a| a.fullname == novo.fullname) {
                        // Um anúncio repetido com o **mesmo** conteúdo não é mudança: subir a
                        // revisão aqui remontaria a lista sem motivo, e o mDNS reanuncia sozinho.
                        Some(i) if l.aparelhos[i] == novo => {}
                        Some(i) => {
                            registro::linha(format!("mdns: aparelho atualizado tela={} e_camera={}", novo.tem_tela, novo.tem_camera));
                            l.aparelhos[i] = novo;
                            l.revisao += 1;
                        }
                        None => {
                            registro::linha(format!("mdns: aparelho achado tela={} e_camera={}", novo.tem_tela, novo.tem_camera));
                            l.aparelhos.push(novo);
                            l.revisao += 1;
                        }
                    }
                }
                Ok(Some(DiscoveryEvent::Lost(fullname))) => {
                    let mut l = self.lista();
                    if let Some(i) = l.aparelhos.iter().position(|a| a.fullname == fullname) {
                        registro::linha("mdns: aparelho saiu da lista");
                        l.aparelhos.remove(i);
                        l.revisao += 1;
                    }
                }
                Ok(None) => {}
                Err(erro) => {
                    registro::linha(format!("mdns: navegação parou: {erro}"));
                    let mut l = self.lista();
                    l.ativa = false;
                    l.motivo = "A procura na rede parou. Digite o endereço.".into();
                    l.revisao += 1;
                    break;
                }
            }
        }
        navegador.stop();
        registro::linha("mdns: navegação encerrada");
        self.ligada.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use quall_core::discovery::anuncio;
    use quall_core::protocol::Capabilities;

    fn achado(id: &str, fonte: bool, papel: Option<Papel>) -> DiscoveredDevice {
        let mut a = anuncio(id, "Aparelho", Capabilities { screen_source: fonte, camera_source: false, sink: !fonte });
        a.papel = papel;
        DiscoveredDevice {
            announcement: a,
            addresses: vec!["192.168.15.8".parse::<std::net::IpAddr>().unwrap().into()],
            signaling_port: 7979,
            fullname: format!("{id}._quall._tcp.local."),
        }
    }

    #[test]
    fn a_lista_de_exibir_mostra_fontes_e_esconde_quem_tem_papel() {
        let f = Filtro::Video;
        assert!(f.aceita(&achado("emissor", true, None)));
        assert!(!f.aceita(&achado("so-exibe", false, None)));
        let camera = DiscoveredDevice {
            announcement: anuncio("camera", "Camera", Capabilities { screen_source: false, camera_source: true, sink: false }),
            ..achado("camera", false, None)
        };
        assert!(!f.aceita(&camera), "Monitor não lista uma fonte que só transmite câmera");
        assert!(!f.aceita(&achado("prompter", false, Some(Papel::Teleprompter))));
        // Mesmo que um dia um prompter anuncie fonte, o papel o tira da lista de vídeo.
        assert!(!f.aceita(&achado("prompter-com-fonte", true, Some(Papel::Teleprompter))));
    }

    #[test]
    fn a_lista_do_controle_so_mostra_prompters_e_nunca_este_aparelho() {
        let f = Filtro::Prompters { meu_id: "eu".into() };
        assert!(f.aceita(&achado("tablet", false, Some(Papel::Teleprompter))));
        assert!(!f.aceita(&achado("eu", false, Some(Papel::Teleprompter))));
        assert!(!f.aceita(&achado("emissor", true, None)));
        assert!(!f.aceita(&achado("outro-controle", false, Some(Papel::ControleRemoto))));
        assert!(!f.aceita(&achado("futuro", false, Some(Papel::Desconhecido))));
    }
}
