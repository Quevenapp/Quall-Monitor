//! "Os aparelhos da rede": a lista que a metade **Exibir** da tela inicial mostra.
//!
//! # Por que uma thread, e não uma varredura no clique
//!
//! `Browser::collect(duracao)` existe e é o que `connect.rs` usa na sonda — ele bloqueia pelo prazo
//! inteiro e devolve o que achou. Numa sonda isso está certo; numa janela, não: bloquear a thread
//! da interface por três segundos congela o app, e fazer a varredura só no clique significa que a
//! pessoa clica em "Exibir" antes de o nome do outro aparelho ter aparecido.
//!
//! O que esta thread faz é ler, a cada 250 ms, a tabela que `quall_core::busca::Busca` mantém numa
//! thread do núcleo (mDNS, broadcast UDP e sondagem da /24, `docs/conexao.md` §2), e publicá-la
//! numa lista sob trava que a janela lê. Lista vazia é o caso **normal** numa rede parada, não erro:
//! tratá-la como falha encheria a tela de aviso vermelho numa rede que só não tem ninguém
//! transmitindo.
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

#[cfg(test)]
use quall_core::discovery::DiscoveredDevice;
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
    #[cfg(test)]
    fn aceita(&self, achado: &DiscoveredDevice) -> bool {
        self.aceita_anuncio(&achado.announcement)
    }

    fn aceita_anuncio(&self, a: &quall_core::protocol::Announcement) -> bool {
        match self {
            Filtro::Video => a.capabilities.screen_source && a.papel.is_none(),
            Filtro::Prompters { meu_id } => a.papel == Some(Papel::Teleprompter) && a.device_id.0 != *meu_id,
        }
    }
}

/// Um aparelho que anunciou uma fonte na rede.
///
/// **No v3 o anúncio é anônimo** (`docs/v3-no-privado.md` §5.3, §5.4): o nome que vem da rede é o
/// alias `Quall 1a2b3c4d` (o mesmo que a espera do outro aparelho mostra), e o `device_id` é
/// `discovery-<token>`, que muda a cada espera. O nome guardado de um pareado vem da sessão salva
/// (a linha "Pareado", posta pelo `receptor.rs`), nunca da rede.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aparelho {
    pub nome: String,
    pub device_id: String,
    pub endereco: SocketAddr,
    /// A chave da escolha na lista: o `device_id` da busca (o token do anúncio), e não o nome de
    /// tela. Na linha "Pareado", o id guardado.
    pub fullname: String,
    pub tem_tela: bool,
    pub tem_camera: bool,
    /// **A sessão salva do Exibir** (`docs/v3-no-privado.md` §5.2): a linha "Pareado", posta pelo
    /// `receptor.rs`. Escolhida, o núcleo acha o aparelho e retoma com ele na mesma conexão
    /// (`conectar_ao_par`). Nunca vem da busca.
    pub lembrado: bool,
    /// **O par guardado que este item é**, pela sonda do núcleo (`reconhecer::Reconhecedor`, §5.4):
    /// o id dele. A lista mostra o nome local e o Exibir conecta com o alvo (a A2 vale a partir da
    /// lista). Pista, não prova: quem decide é a retomada só com o vínculo dele.
    pub conhecido: Option<String>,
    /// **O alias embaixo do nome anunciado** (`docs/v3-no-privado.md` §8.7): quando [`Aparelho::nome`] é
    /// o nome que o aparelho escolheu para a rede, que não autentica nada, a linha leva também o alias
    /// `Quall 1a2b3c4d` da espera do outro lado. `None` quando o nome já é o alias, e quando ele vira o
    /// nome guardado de um par (`receptor.rs`).
    pub alias: Option<String>,
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
        match &self.alias {
            Some(alias) => format!("{} ({alias}) — {} · {}", self.nome, o_que, self.endereco),
            None => format!("{} — {} · {}", self.nome, o_que, self.endereco),
        }
    }

    /// Texto da linha na lista do controle do teleprompter.
    pub fn linha_do_prompter(&self) -> String {
        match &self.alias {
            Some(alias) => format!("{} ({alias}) · {}", self.nome, self.endereco),
            None => format!("{} · {}", self.nome, self.endereco),
        }
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
    /// As dicas e o "Procurar de novo" (`docs/v3-no-privado.md` §8.9), para a thread entregar ao núcleo.
    dicas: Mutex<Vec<String>>,
    varrer_de_novo: AtomicBool,
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
            dicas: Mutex::new(Vec::new()),
            varrer_de_novo: AtomicBool::new(false),
        })
    }

    /// **Uma dica** (`docs/v3-no-privado.md` §8.9): o último endereço de um vínculo (`"ip:porta"`), que a
    /// busca pergunta poucas vezes enquanto ele não está na lista — no lugar de varrer a /24.
    pub fn dar_dica(&self, endereco: &str) {
        let e = endereco.trim();
        if !e.is_empty() {
            self.dicas.lock().unwrap_or_else(|e| e.into_inner()).push(e.to_string());
        }
    }

    /// **"Procurar de novo"** (§8.9): a varredura da /24 recomeça, com a lista cheia ou não.
    pub fn procurar_de_novo(&self) {
        self.varrer_de_novo.store(true, Ordering::SeqCst);
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

    /// Para a navegação. A busca do núcleo some junto com a thread.
    pub fn parar(&self) {
        self.parar.store(true, Ordering::SeqCst);
    }

    /// **Os três caminhos do núcleo** (`docs/conexao.md` §2): mDNS, broadcast UDP e a sondagem — as
    /// dicas, os vizinhos da tabela do Windows ([`crate::vizinhos`]) e, só com a lista vazia (ou pelo
    /// "Procurar de novo"), a /24, uma vez e devagar (`docs/v3-no-privado.md` §8.9). Até 12/10 era só o mDNS, e o Firewall do Windows barra o 5353 de
    /// entrada até no perfil Privado — a lista vazia era o caso comum. A sondagem passa por ele: a
    /// resposta é unicast a uma pergunta que **saiu** deste aparelho, e o firewall com estado a deixa
    /// entrar.
    ///
    /// A lista não pisca: quem some fica até 30 s (`quall_core::busca::LEMBRADO_POR`), e a revisão só
    /// sobe quando muda quem está na lista ou para onde se conecta — nunca pelo relógio do "visto há".
    fn correr(self: Arc<Self>) {
        crate::vizinhos::instalar();
        let busca = quall_core::busca::Busca::iniciar();
        // **O nome local dos pares** (`docs/v3-no-privado.md` §5.4): o núcleo sonda cada item novo uma
        // vez (um `Probe`, e fecha antes do `Hello`: nada é gasto do outro lado) e diz qual par
        // guardado ele é. Refeito quando o `pares.json` muda (parear, esquecer): o cache recomeça.
        let papel = match self.filtro {
            Filtro::Video => None,
            Filtro::Prompters { .. } => Some(Papel::ControleRemoto),
        };
        let mut pares_vistos = String::new();
        // Os nomes guardados dos pares, relidos de tempos em tempos (o controle do teleprompter mostra o
        // prompter reconhecido pelo nome; no Exibir quem faz isso é o `receptor.rs`).
        let mut nomes = crate::conexao::NomesDosPares::default();
        let mut reconhecedor: Option<quall_core::reconhecer::Reconhecedor> = None;
        let mut voltas: u32 = 0;
        // O que o diário já contou (bancada de 13/10: "lista com 1 aparelho(s)" não dizia quem).
        let mut vistos_antes: Vec<quall_core::busca::AparelhoVisto> = Vec::new();
        {
            let mut l = self.lista();
            l.ativa = true;
            l.motivo.clear();
            l.revisao += 1;
        }
        registro::linha("busca: procurando (mDNS, broadcast UDP e sondagem da /24)");

        while !self.parar.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(250));
            for d in std::mem::take(&mut *self.dicas.lock().unwrap_or_else(|e| e.into_inner())) {
                let ip = d.parse::<std::net::SocketAddr>().map(|e| e.ip()).or_else(|_| d.parse());
                if let Ok(ip) = ip {
                    busca.dar_dica(ip);
                }
            }
            if self.varrer_de_novo.swap(false, Ordering::SeqCst) {
                registro::linha("busca: procurar de novo");
                busca.procurar_de_novo();
            }
            if voltas % 20 == 0 {
                if matches!(self.filtro, Filtro::Prompters { .. }) {
                    nomes = crate::identidade::nomes_dos_pares();
                }
                let pares = crate::identidade::pares_conhecidos();
                let json = pares.to_json().unwrap_or_default();
                if json != pares_vistos {
                    pares_vistos = json;
                    let eu = quall_core::protocol::DeviceId(crate::identidade::device_id());
                    reconhecedor = match quall_core::reconhecer::Reconhecedor::novo(eu, papel, pares) {
                        Ok(r) => Some(r),
                        Err(e) => {
                            registro::linha(format!("busca: sem o nome local dos pares (status={})", crate::diagnostico_rede::status(&e)));
                            None
                        }
                    };
                }
            }
            voltas = voltas.wrapping_add(1);
            let mut vistos = busca.aparelhos();
            if let Some(r) = &reconhecedor {
                r.rotular(&mut vistos);
            }
            // Quem entrou, saiu ou mudou de caminho — antes do filtro, para o diário dizer também
            // quem a busca viu e a lista não mostra (outro papel, o próprio anúncio). Só o alias.
            for linha in quall_core::busca::mudancas_da_lista(&vistos_antes, &vistos) {
                registro::linha(format!("busca: {linha}"));
            }
            vistos_antes = vistos.clone();
            let novos: Vec<Aparelho> = vistos
                .into_iter()
                // O próprio anúncio (a espera deste mesmo processo) nunca entra na lista.
                .filter(|v| !v.proprio)
                .filter(|v| self.filtro.aceita_anuncio(&v.anuncio))
                .filter_map(|v| {
                    let caps = v.anuncio.capabilities;
                    // O nome anunciado, com o alias embaixo (§8.7). O guardado: na lista do controle, aqui
                    // (o prompter que a sonda reconheceu, §8.8); no Exibir, no `receptor.rs`.
                    let guardado = match self.filtro {
                        Filtro::Prompters { .. } => v.conhecido.as_ref().and_then(|d| nomes.nome(&d.0)),
                        Filtro::Video => None,
                    };
                    let (nome, alias) =
                        crate::conexao::rotulo_da_linha(guardado, v.nome_anunciado.as_deref(), &v.anuncio.display_name);
                    Some(Aparelho {
                        nome,
                        alias,
                        device_id: v.anuncio.device_id.0.clone(),
                        endereco: v.endpoint?,
                        // O id faz o papel do nome mDNS: é por ele que a escolha da lista sobrevive
                        // a uma atualização (`receptor.rs`), e ele é único por aparelho.
                        fullname: v.anuncio.device_id.0.clone(),
                        tem_tela: caps.screen_source,
                        tem_camera: caps.camera_source,
                        lembrado: false,
                        conhecido: v.conhecido.as_ref().map(|d| d.0.clone()),
                    })
                })
                .collect();
            let mut l = self.lista();
            if l.aparelhos != novos {
                registro::linha(format!("busca: lista com {} aparelho(s)", novos.len()));
                l.aparelhos = novos;
                l.revisao += 1;
            }
        }
        busca.parar();
        registro::linha("busca: encerrada");
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
            nome_anunciado: None,
        }
    }

    #[test]
    fn a_lista_de_exibir_mostra_fontes_e_esconde_quem_tem_papel() {
        let f = Filtro::Video;
        assert!(f.aceita(&achado("emissor", true, None)));
        assert!(!f.aceita(&achado("so-exibe", false, None)));
        assert!(!f.aceita(&achado("prompter", false, Some(Papel::Teleprompter))));
        // Mesmo que um dia um prompter anuncie fonte, o papel o tira da lista de vídeo.
        assert!(!f.aceita(&achado("prompter-com-fonte", true, Some(Papel::Teleprompter))));
        let camera = DiscoveredDevice {
            announcement: anuncio("camera", "Camera", Capabilities { screen_source: false, camera_source: true, sink: false }),
            ..achado("camera", false, None)
        };
        assert!(!f.aceita(&camera), "Monitor não lista uma fonte que só transmite câmera");
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
