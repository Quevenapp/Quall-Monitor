import Combine
import Foundation

/// **Os dois idiomas do Quall** (`docs/traducao.md`, "macOS"): o português, que é o texto-fonte, e o
/// inglês.
///
/// # O texto em português é a chave
///
/// `T("Espelhar")` devolve "Espelhar" em português e "Mirror" em inglês. A tabela inglesa mora em
/// `Recursos/en.lproj/Localizable.strings`, com o texto português como chave — o padrão da Apple para
/// texto-fonte —, e o português não tem tabela: é o próprio código. Assim nenhum texto PT muda de
/// sentido, e os testes que comparam frases em português continuam valendo.
///
/// # A troca vale na hora
///
/// O idioma da vez mora aqui, e não no `Bundle.main`: o seletor PT | EN da tela inicial troca sem
/// reabrir o app. Quem desenha observa `TrocaDeIdioma.compartilhada` (as raízes das janelas se
/// redesenham inteiras com o `.id` do idioma); quem monta frase lê `Idioma.atual` na hora de montar.
public enum Idioma: String, CaseIterable, Sendable {
    case pt
    case en

    /// O nome do idioma no próprio idioma, para o leitor de tela do seletor.
    public var nome: String {
        switch self {
        case .pt: return "Português"
        case .en: return "English"
        }
    }

    /// "PT" e "EN", como o seletor mostra.
    public var sigla: String { rawValue.uppercased() }

    /// O idioma da vez. Lido por `T` de qualquer thread (o núcleo chama de volta fora da principal).
    public static var atual: Idioma {
        get { trava.lock(); defer { trava.unlock() }; return _atual }
        set { trava.lock(); _atual = newValue; trava.unlock() }
    }

    private static let trava = NSLock()
    private static var _atual: Idioma = resolvido()

    /// A chave do `UserDefaults` com a escolha do seletor.
    public static let chaveDaEscolha = "quall.idioma"

    /// **A ordem** (contrato, item 2): `QUALL_IDIOMA` no ambiente (bancada e retratos); sob o XCTest,
    /// o texto-fonte, para os testes que comparam frases em português não dependerem do idioma do Mac;
    /// a escolha guardada do seletor; e por fim o idioma do sistema — qualquer `pt-*` é português, o
    /// resto é inglês.
    static func resolvido(ambiente: [String: String] = ProcessInfo.processInfo.environment,
                          guardado: String? = UserDefaults.standard.string(forKey: chaveDaEscolha),
                          preferidos: [String] = Locale.preferredLanguages,
                          sobXCTest: Bool = NSClassFromString("XCTestCase") != nil) -> Idioma {
        if let v = ambiente["QUALL_IDIOMA"], let i = Idioma(rawValue: v.lowercased()) { return i }
        if sobXCTest { return .pt }
        if let guardado, let i = Idioma(rawValue: guardado) { return i }
        return doSistema(preferidos)
    }

    /// O primeiro idioma preferido do sistema decide: `pt`, `pt-BR`, `pt-PT` → português.
    public static func doSistema(_ preferidos: [String]) -> Idioma {
        guard let primeiro = preferidos.first?.lowercased() else { return .pt }
        return primeiro == "pt" || primeiro.hasPrefix("pt-") || primeiro.hasPrefix("pt_") ? .pt : .en
    }
}

/// **Quem a tela observa** para se redesenhar quando o seletor troca o idioma.
public final class TrocaDeIdioma: ObservableObject {
    public static let compartilhada = TrocaDeIdioma()

    @Published public private(set) var atual: Idioma = Idioma.atual

    /// Troca o idioma da vez e o guarda (contrato, item 2: a escolha vence o sistema dali em diante).
    ///
    /// Guarda também `AppleLanguages` no domínio do app — o mesmo que Ajustes do Sistema > Geral >
    /// Idioma e Região > Aplicativos grava —, para o que o próprio macOS escreve (o menu do app, Editar,
    /// Janela, e a frase dos diálogos de permissão, do `InfoPlist.strings`) vir no idioma escolhido
    /// **na próxima abertura**. Esse texto não troca na hora: é o sistema que o desenha.
    public func escolher(_ idioma: Idioma, guardar: Bool = true) {
        Idioma.atual = idioma
        if guardar {
            UserDefaults.standard.set(idioma.rawValue, forKey: Idioma.chaveDaEscolha)
            UserDefaults.standard.set([idioma == .pt ? "pt-BR" : "en"], forKey: "AppleLanguages")
        }
        if atual != idioma { atual = idioma }
    }

    /// O seletor: um toque troca para o outro.
    public func alternar() {
        escolher(atual == .pt ? .en : .pt)
    }
}

/// **O texto no idioma da vez.** A chave é a frase em português, do jeito que o código sempre a
/// escreveu; sem tradução na tabela, volta o português (e o teste de paridade acusa a falta).
public func T(_ pt: String) -> String {
    switch Idioma.atual {
    case .pt: return pt
    case .en: return Traducoes.ingles[pt] ?? pt
    }
}

/// O mesmo, com lacunas `%@` preenchidas na ordem: `T("Conectado a %@", nome)`. Qualquer valor serve
/// (vira texto como na interpolação), e por isso a única lacuna é `%@`; a tradução pode trocar a ordem
/// com `%1$@`/`%2$@`, e um `%` de verdade numa frase com lacuna se escreve `%%`.
public func T(_ pt: String, _ argumentos: Any...) -> String {
    String(format: T(pt), arguments: argumentos.map { "\($0)" as NSString })
}

/// A frase num idioma dado, sem olhar o da vez (os retratos e os testes).
public func T(_ pt: String, em idioma: Idioma) -> String {
    switch idioma {
    case .pt: return pt
    case .en: return Traducoes.ingles[pt] ?? pt
    }
}

/// **Uma frase já pronta, no idioma da vez.** Os avisos guardados nos modelos (o bloqueio da permissão,
/// o conselho do emissor) nascem no idioma da hora em que aconteceram; a tela que os mostra passa por
/// aqui para o seletor valer também para eles. Só a frase inteira da tabela volta — uma frase com dado
/// dentro (o nome do aparelho, o código do erro) fica como nasceu até a próxima.
public func retraduzido(_ texto: String) -> String {
    switch Idioma.atual {
    case .pt: return Traducoes.portugues[texto] ?? texto
    case .en: return Traducoes.ingles[texto] ?? texto
    }
}

/// **A frase começa como esta chave, em qualquer das duas línguas?** Para a lógica que reconhece um
/// aviso pelo começo (`"sem som: …"`, `"Conexão perdida…"`): o começo é o da chave até a primeira lacuna,
/// e a frase pode ter nascido antes de o seletor trocar o idioma.
public func comecaComo(_ texto: String, _ pt: String) -> Bool {
    Idioma.allCases.contains { i in
        let modelo = T(pt, em: i)
        let comeco = modelo.range(of: "%").map { String(modelo[..<$0.lowerBound]) } ?? modelo
        return !comeco.isEmpty && texto.hasPrefix(comeco)
    }
}

/// **A tabela inglesa**, lida uma vez do `Localizable.strings` do pacote de recursos.
public enum Traducoes {
    public static let ingles: [String: String] = carregar("en")
    /// O caminho de volta (o inglês para o português), para `retraduzido`.
    static let portugues: [String: String] = Dictionary(ingles.map { ($1, $0) }, uniquingKeysWith: { a, _ in a })

    /// O pacote de recursos deste módulo. Procurado à mão, e não pelo `Bundle.module`: o acessor
    /// gerado pelo SwiftPM derruba o processo (`fatalError`) quando não acha o pacote, e aqui a falta
    /// tem de virar "o app em português", não "o app que não abre". Os lugares: dentro do `.app`
    /// (`Contents/Resources`, onde `Empacotar/empacotar.sh` o copia), ao lado do executável
    /// (`swift build`), e ao lado do `.xctest` (`swift test`).
    public static let pacote: Bundle? = {
        let nome = "QuallCapture_QuallIdiomaKit.bundle"
        var lugares: [URL?] = [
            Bundle.main.resourceURL,
            Bundle.main.bundleURL,
            Bundle.main.executableURL?.deletingLastPathComponent(),
            Bundle(for: Marcador.self).resourceURL,
            Bundle(for: Marcador.self).bundleURL.deletingLastPathComponent(),
        ]
        for b in Bundle.allBundles where b.bundlePath.hasSuffix(".xctest") {
            lugares.append(b.bundleURL.deletingLastPathComponent())
        }
        for lugar in lugares.compactMap({ $0 }) {
            if let b = Bundle(url: lugar.appendingPathComponent(nome)) { return b }
        }
        return nil
    }()

    /// O `Localizable.strings` de um idioma, como dicionário.
    static func carregar(_ codigo: String) -> [String: String] {
        guard let url = pacote?.url(forResource: "Localizable", withExtension: "strings",
                                    subdirectory: nil, localization: codigo),
              let d = NSDictionary(contentsOf: url) as? [String: String]
        else { return [:] }
        return d
    }

    private final class Marcador {}
}
