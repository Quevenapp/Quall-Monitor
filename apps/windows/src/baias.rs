//! O registro de baias: **uma câmera virtual por aparelho pareado**.
//!
//! Guarda, em `%APPDATA%\Quall\baias.json`, o par `device_id → nome do aparelho`. É preciso porque
//! o pareamento **não guarda o nome**: `PairedPeers` é `device_id → segredo`, e o nome só existe no
//! instante da conexão (`pronto.peer.display_name`). Sem este arquivo, um aparelho já pareado não
//! teria como ganhar câmera antes de conectar de novo — e o pedido é justamente que a câmera dele
//! esteja lá **antes**, para quem abre o Zoom antes de mexer no celular.
//!
//! # Por que a baia é do pareamento e não da sessão
//!
//! Porque é isso que o consumidor vê. O Zoom lista as câmeras quando **ele** abre, não quando o
//! celular conecta; uma câmera que só existisse durante a sessão simplesmente não estaria na lista
//! na hora em que a pessoa procura. E o cano precisa existir antes por outra razão medida: sem
//! servidor, a fonte entrega o **padrão de bancada**, que quer dizer "o Quall nem está rodando" —
//! informação errada e mais alarmante que a placa de espera.
//!
//! # O que este registro não faz
//!
//! Não renomeia câmera. O nó é chaveado por CLSID **+ nome** (medido em 27/08/2026), então criar
//! com um nome novo **duplica** em vez de renomear. Quando um aparelho muda de nome, o que muda
//! aqui é o nome guardado, e a câmera nova só aparece na **próxima** abertura do app — com a
//! antiga já removida pela vida de sessão. Trocar isso a quente é derrubar e recriar, e não há
//! medida que autorize fazer isso no meio de uma transmissão.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::baia::Baia;
use crate::registro;

fn arquivo() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Quall").join("baias.json"))
}

fn ler() -> HashMap<String, String> {
    ler_com_estado().0
}

/// O mapa, e se o arquivo **que existe** pôde ser lido. `(vazio, true)` quando não há arquivo.
fn ler_com_estado() -> (HashMap<String, String>, bool) {
    let Some(p) = arquivo() else { return (HashMap::new(), true) };
    let Ok(texto) = std::fs::read_to_string(&p) else { return (HashMap::new(), !p.exists()) };
    // **O BOM, e o que ele custava.** Ferramenta de Windows escreve `EF BB BF` na frente de UTF-8
    // (o `Set-Content -Encoding UTF8` do PowerShell 5, o Bloco de Notas antigo), e `serde_json`
    // recusa o arquivo inteiro por causa dele. A recusa virava "nenhum aparelho conhecido" — e a
    // gravação seguinte, que relê e mescla, gravava por cima **só** o aparelho novo. Aconteceu em
    // 10/09/2026, por um roteiro de bancada meu; o arquivo foi salvo antes de o app gravar.
    let texto = texto.strip_prefix('\u{feff}').unwrap_or(&texto);
    match serde_json::from_str(texto) {
        Ok(mapa) => (mapa, true),
        Err(erro) => {
            registro::linha(format!("baias: {} ilegível ({erro})", p.display()));
            (HashMap::new(), false)
        }
    }
}

/// Grava relendo e mesclando, **nunca sobrescrevendo com o que está na memória**.
///
/// É a mesma regra de `identidade::guardar_pares` (dívida 23) e pelo mesmo motivo: duas instâncias
/// do app abertas ao mesmo tempo — que é caso real nesta bancada — apagariam uma o aparelho da
/// outra.
///
/// E **não grava por cima de um arquivo que não conseguiu ler**: mesclar com "vazio" seria apagar
/// os aparelhos que estão lá dentro. Melhor a câmera nova não persistir do que as antigas sumirem.
fn gravar(novos: &HashMap<String, String>) {
    let Some(p) = arquivo() else { return };
    let (mut todos, legivel) = ler_com_estado();
    if !legivel {
        registro::linha(format!(
            "baias: não regravo {} — ele está ilegível, e regravar apagaria os aparelhos dele",
            p.display()
        ));
        return;
    }
    for (k, v) in novos {
        todos.insert(k.clone(), v.clone());
    }
    if let Some(pasta) = p.parent() {
        let _ = std::fs::create_dir_all(pasta);
    }
    if let Ok(texto) = serde_json::to_string_pretty(&todos) {
        let _ = std::fs::write(&p, texto);
    }
}

pub struct Registro {
    /// Desligado, este registro não cria câmera nenhuma e não escreve arquivo nenhum — é como se
    /// a frente não existisse. Ver `Argumentos::sem_cameras_virtuais`.
    ligado: bool,
    abertas: Mutex<HashMap<String, Arc<Baia>>>,
    /// Nomes de câmera que **faltam criar**, deixados aqui pela thread da sessão para a thread da
    /// janela pegar.
    ///
    /// Existe porque `IMFVirtualCamera` é um ponteiro COM e não é `Send`: quem cria o nó tem de
    /// ser a thread que vai guardá-lo, e essa é a da janela. O `Baia` (cano e placa) **já** nasce
    /// na hora do pareamento, então a sessão publica desde o primeiro quadro; o que espera até o
    /// próximo pulso de 100 ms é só o nó aparecer na lista do Windows.
    ///
    /// Sem esta fila, a câmera de um aparelho recém-pareado só existia na **próxima abertura do
    /// app** — o que Bruno pegou em campo, com estas palavras: *"pareado s24 no dell não apareceu
    /// como camera virtual"*.
    pendentes: Mutex<Vec<String>>,
}

impl Registro {
    pub fn novo(ligado: bool) -> Arc<Self> {
        Arc::new(Registro {
            ligado,
            abertas: Mutex::new(HashMap::new()),
            pendentes: Mutex::new(Vec::new()),
        })
    }

    /// Sobe uma câmera para cada aparelho que este computador já conhece.
    ///
    /// Erro numa não impede as outras: uma câmera que não sobe é uma linha no registro, não o app
    /// sem receptor.
    /// Devolve os objetos de câmera para **quem chamou segurar** — e quem chama é a thread
    /// principal. Largá-los aqui dentro seria pedir para um ponteiro COM atravessar thread; ver a
    /// nota em `baia.rs`. Enquanto o `Vec` devolvido viver, as câmeras existem.
    #[must_use = "as câmeras somem quando este Vec é largado — segure-o pela vida do app"]
    pub fn abrir_conhecidas(&self) -> Vec<crate::baia::CameraVirtual> {
        if !self.ligado {
            registro::linha("cameras virtuais: desligadas (--sem-cameras-virtuais)");
            return Vec::new();
        }
        let conhecidos = ler();
        if conhecidos.is_empty() {
            registro::linha(
                "cameras virtuais: nenhum aparelho conhecido ainda — a câmera de um aparelho \
                 aparece na PRÓXIMA abertura do app, depois da primeira conexão dele",
            );
            return Vec::new();
        }
        let mut cameras = Vec::new();
        for (id, nome) in conhecidos {
            match crate::baia::CameraVirtual::criar(&nome) {
                Ok(c) => {
                    self.abrir(&id, &nome);
                    cameras.push(c);
                }
                Err(erro) => {
                    registro::linha(format!("camera virtual (nome omitido): não subiu — {erro:#}"))
                }
            }
        }
        registro::linha(format!("cameras virtuais: {} de pé", cameras.len()));
        cameras
    }

    /// A baia deste aparelho, criando-a se ainda não existir. `None` quando desligado ou quando a
    /// criação falhou.
    pub fn obter_ou_criar(&self, device_id: &str, nome: &str) -> Option<Arc<Baia>> {
        if !self.ligado || nome.is_empty() {
            return None;
        }
        // O nome guardado é o **desta** conexão: se o aparelho mudou de nome, a câmera nova sai na
        // próxima abertura do app. Ver o cabeçalho.
        let mut um = HashMap::new();
        um.insert(device_id.to_string(), nome.to_string());
        gravar(&um);
        if let Some(b) = self.abertas.lock().unwrap().get(device_id) {
            if b.nome == nome {
                return Some(Arc::clone(b));
            }
        }
        // Aparelho novo (ou renomeado): o cano sobe **agora** — é Rust puro e pode nascer em
        // qualquer thread — e o nó fica na fila para a thread da janela criar no próximo pulso.
        let baia = self.abrir(device_id, nome);
        if baia.is_some() {
            self.pendentes.lock().unwrap().push(nome.to_string());
            registro::linha(format!(
                "camera virtual (nome omitido): cano de pé; o nó entra na lista no próximo pulso"
            ));
        }
        baia
    }

    /// **Roda na thread da janela**, a cada pulso: cria os nós que a thread da sessão pediu.
    ///
    /// O `Vec` que ela recebe é o mesmo que segura as câmeras vivas — a vida delas é
    /// `MFVirtualCameraLifetime_Session`, então largar o objeto é remover o nó.
    pub fn atender_pendentes(&self, cameras: &mut Vec<crate::baia::CameraVirtual>) {
        let nomes: Vec<String> = {
            let mut p = self.pendentes.lock().unwrap();
            if p.is_empty() {
                return;
            }
            p.drain(..).collect()
        };
        for nome in nomes {
            if cameras.iter().any(|c| c.nome == nome) {
                continue;
            }
            match crate::baia::CameraVirtual::criar(&nome) {
                Ok(c) => {
                    registro::linha(format!("camera virtual (nome omitido): nó criado, já na lista"));
                    cameras.push(c);
                }
                Err(erro) => {
                    registro::linha(format!("camera virtual (nome omitido): nó não subiu — {erro:#}"))
                }
            }
        }
    }

    /// Sobe **o cano e a placa** desta câmera (não o nó — ver `abrir_conhecidas`).
    fn abrir(&self, device_id: &str, nome: &str) -> Option<Arc<Baia>> {
        match Baia::abrir(nome) {
            Ok(b) => {
                let b = Arc::new(b);
                self.abertas
                    .lock()
                    .unwrap()
                    .insert(device_id.to_string(), Arc::clone(&b));
                Some(b)
            }
            Err(erro) => {
                registro::linha(format!("camera virtual (nome omitido): não subiu — {erro:#}"));
                None
            }
        }
    }

    pub fn quantas(&self) -> usize {
        self.abertas.lock().unwrap().len()
    }

    /// **Alguma câmera do Quall está em uso agora?** Alguém lendo o cano de qualquer baia — o
    /// servidor de quadros do Windows só abre o cano quando um app abre a câmera. É a D3 do
    /// `docs/som-no-receptor.md` §12.1 no Windows: o receptor cala o som enquanto isto for
    /// verdade, para o som do cômodo não vazar para a chamada, a menos que a pessoa marque "tocar
    /// mesmo com a câmera em uso". Desligado (`--sem-cameras-virtuais`), nunca.
    pub fn alguma_em_uso(&self) -> bool {
        self.ligado
            && self
                .abertas
                .lock()
                .map(|a| a.values().any(|b| b.alguem_lendo()))
                .unwrap_or(false)
    }
}
