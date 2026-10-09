import Foundation
import QuallCaptureKit
import QuallIdiomaKit

/// Message arguments remain separate so changing PT/EN also translates an active session.
public struct MensagemDoMonitor: Equatable, Sendable {
    public let chave: String
    public let argumentos: [String]
    public init(_ chave: String = "", _ argumentos: String...) {
        self.chave = chave; self.argumentos = argumentos
    }
    public var texto: String { texto(em: Idioma.atual) }
    public func texto(em idioma: Idioma) -> String {
        let translated = T(chave, em: idioma)
        guard !argumentos.isEmpty else { return translated }
        return String(format: translated, arguments: argumentos.map { $0 as NSString })
    }
}

public struct ParDoMonitor: Sendable {
    public let id: String
    public let nome: String
    public let tela: (largura: Int, altura: Int)?
    public init(id: String, nome: String, tela: (Int, Int)? = nil) {
        self.id = id; self.nome = nome; self.tela = tela
    }
}

/// Main-thread policy, independent of TCC, networking, capture and UserDefaults.
/// A closing session keeps its slot and monitor identity until its helper has exited.
public struct ControleDeMonitores: Sendable {
    public static let limite = 8
    public enum Fase: Sendable { case esperando, preparando, ativo, encerrando }
    public struct Monitor: Identifiable, Sendable {
        public let id: Int
        public var fase: Fase
        public var aparelho = ""
        public var nome = ""
        public var indice: Int?
        public var mensagem = MensagemDoMonitor()
    }
    public private(set) var monitores: [Int: Monitor] = [:]
    public private(set) var indices: TabelaDeIndices
    /// Kept for the lifetime of this app when helper/display removal could not be confirmed.
    public private(set) var indicesEmQuarentena: Set<Int> = []
    public private(set) var finalizando = false
    public init(indices: TabelaDeIndices = .init()) { self.indices = indices }
    public var espera: Monitor? { monitores.values.first { $0.fase == .esperando } }
    public var podeAbrirEspera: Bool {
        !finalizando && espera == nil && monitores.count + indicesEmQuarentena.count < Self.limite
    }
    public var conectados: [Monitor] {
        monitores.values.filter { $0.fase != .esperando }.sorted { $0.id < $1.id }
    }
    @discardableResult public mutating func abrirEspera(id: Int) -> Bool {
        guard podeAbrirEspera, monitores[id] == nil else { return false }
        monitores[id] = Monitor(id: id, fase: .esperando)
        return true
    }
    @discardableResult public mutating func novaRodada() -> Bool {
        guard monitores.isEmpty else { return false }
        finalizando = false
        return true
    }
    /// Returns old sessions to stop before the same receiver can get its monitor identity back.
    public mutating func pareou(id: Int, par: ParDoMonitor) -> [Int]? {
        guard !finalizando, var current = monitores[id], current.fase == .esperando else { return nil }
        current.fase = .preparando; current.aparelho = par.id; current.nome = par.nome
        monitores[id] = current
        let old = monitores.values.filter {
            $0.id != id && !par.id.isEmpty && $0.aparelho == par.id
        }.map(\.id)
        for id in old { encerrar(id: id) }
        return old
    }
    /// An index is issued once, after older sessions of this receiver have fully closed.
    public mutating func preparar(id: Int, agora: Double) -> Int? {
        guard !finalizando, var current = monitores[id], current.fase == .preparando, current.indice == nil else { return nil }
        guard !monitores.values.contains(where: {
            $0.id != id && !current.aparelho.isEmpty && $0.aparelho == current.aparelho
        }) else { return nil }
        let inUse = Set(monitores.values.compactMap(\.indice)).union(indicesEmQuarentena)
        let key = current.aparelho.isEmpty ? "legado-\(id)" : current.aparelho
        let index = indices.indice(para: key, emUso: inUse, agora: agora)
        current.indice = index
        monitores[id] = current
        return index
    }
    public mutating func iniciou(id: Int, mensagem: MensagemDoMonitor) {
        guard var current = monitores[id], current.fase == .preparando, current.indice != nil else { return }
        current.fase = .ativo; current.mensagem = mensagem
        monitores[id] = current
    }
    public mutating func encerrar(id: Int) {
        guard var current = monitores[id] else { return }
        current.fase = .encerrando
        monitores[id] = current
    }
    public mutating func encerrarTodos() {
        finalizando = true
        for id in Array(monitores.keys) { encerrar(id: id) }
    }
    public mutating func remover(id: Int, monitorLiberado: Bool = true) {
        if !monitorLiberado, let index = monitores[id]?.indice { indicesEmQuarentena.insert(index) }
        monitores[id] = nil
    }
}
