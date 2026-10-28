import Foundation

/// **O nome que os outros aparelhos veem na lista** (`docs/v3-no-privado.md` §8.7, a P3 revista): o
/// campo dos Ajustes, guardado nas preferências, que o núcleo põe no anúncio (chave `nm`,
/// `quall_set_announced_name`). Nunca guardado é o padrão — o nome do Mac; guardado vazio é a pessoa
/// que apagou, e então os anúncios saem sem nome (a lista dos outros mostra o alias `Quall <8 hex>`).
///
/// O saneamento de verdade (40 bytes, sem controle) é o do núcleo; aqui só se aparam as pontas.
public enum NomeNaRede {
    /// A chave nas preferências.
    public static let chave = "nomeNaRede"

    /// O que anunciar: o guardado (vazio = sem nome), ou o padrão se nunca houve escolha.
    public static func efetivo(guardado: String?, padrao: String) -> String {
        (guardado ?? padrao).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Lê das preferências.
    public static func ler(_ preferencias: UserDefaults = .standard, padrao: String) -> String {
        efetivo(guardado: preferencias.string(forKey: chave), padrao: padrao)
    }

    /// **O nome da tela de espera** (bancada de 14/10, `docs/v3-no-privado.md` §8.7.1): o que os outros
    /// veem na lista (`nome`, o anunciado) e o alias pequeno para conferir (`alias`), como a linha da
    /// lista. Sem nome (apagado) → o alias sozinho, como antes (`alias` nulo).
    public struct DaEspera: Equatable, Sendable {
        public let nome: String
        public let alias: String?
        public init(nome: String, alias: String?) {
            self.nome = nome
            self.alias = alias
        }
    }

    /// `nil` sem anúncio (sem alias): a espera diz só o endereço.
    public static func daEspera(anunciado: String?, alias: String?) -> DaEspera? {
        guard let a = alias?.trimmingCharacters(in: .whitespacesAndNewlines), !a.isEmpty else { return nil }
        let n = (anunciado ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        return n.isEmpty || n == a ? DaEspera(nome: a, alias: nil) : DaEspera(nome: n, alias: a)
    }

    /// Numa linha só: "Galaxy S24 (Quall a71bca2b)", ou o alias sozinho.
    public static func emLinha(_ e: DaEspera) -> String {
        e.alias.map { "\(e.nome) (\($0))" } ?? e.nome
    }

    /// Guarda o que a pessoa escreveu — vazio inclusive, que é "não quero nome na rede".
    public static func guardar(_ nome: String, em preferencias: UserDefaults = .standard) {
        preferencias.set(nome.trimmingCharacters(in: .whitespacesAndNewlines), forKey: chave)
    }
}
