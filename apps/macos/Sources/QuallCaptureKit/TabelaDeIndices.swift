import Foundation

/// **Qual identidade de monitor cada aparelho usa** na tela estendida — o índice de
/// `MonitorVirtual.serie(para:indice:)`.
///
/// O macOS guarda posição e modo por identidade de monitor. Com uma identidade por aparelho, ele
/// lembra onde a pessoa pôs **o monitor do tablet** e onde pôs **o do iPhone**, em vez de um lugar
/// só para todos. Por isso o índice é do aparelho (`device_id` do par) e fica gravado.
///
/// Aritmética pura, sem `UserDefaults`: quem grava é o app (ver `Emissor`), e isto se testa no
/// `swift test` sem mexer nas preferências de ninguém.
///
/// # Duas regras que vêm da revisão de 10/09/2026
///
/// - **Dois monitores vivos nunca dividem índice.** Se o índice de um aparelho estiver em uso por
///   outra sessão (o mesmo aparelho reconectando antes de a sessão velha cair, que o `Emissor` já
///   evita fechando a velha), sai um índice livre, só para esta vez, sem gravar.
/// - **A tabela tem teto** ([teto]). Aparelhos de sonda nascem com `device_id` novo a cada corrida;
///   sem teto, cada corrida de bancada deixaria uma identidade nova lembrada para sempre. Cheia, sai
///   o aparelho usado há mais tempo que não esteja no ar.
public struct TabelaDeIndices: Codable, Equatable, Sendable {
    public struct Entrada: Codable, Equatable, Sendable {
        public var indice: Int
        /// Segundos desde 1970 do último uso.
        public var ultimoUso: Double
    }

    public static let teto = 32

    public private(set) var entradas: [String: Entrada] = [:]

    public init() {}

    /// O índice deste aparelho agora. `emUso`: os índices dos monitores que estão no ar.
    public mutating func indice(para deviceId: String, emUso: Set<Int>, agora: Double) -> Int {
        if var e = entradas[deviceId] {
            guard !emUso.contains(e.indice) else {
                return menorLivre(fora: emUso.union(entradas.values.map(\.indice)))
            }
            e.ultimoUso = agora
            entradas[deviceId] = e
            return e.indice
        }
        if entradas.count >= Self.teto,
           let velho = entradas.filter({ !emUso.contains($0.value.indice) })
               .min(by: { $0.value.ultimoUso < $1.value.ultimoUso })?.key {
            entradas[velho] = nil
        }
        let novo = menorLivre(fora: emUso.union(entradas.values.map(\.indice)))
        entradas[deviceId] = Entrada(indice: novo, ultimoUso: agora)
        return novo
    }

    private func menorLivre(fora ocupados: Set<Int>) -> Int {
        var i = 0
        while ocupados.contains(i) { i += 1 }
        return i
    }
}
