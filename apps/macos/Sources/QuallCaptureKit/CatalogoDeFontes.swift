import CoreGraphics
import Foundation
import QuallIdiomaKit

/// Monitor has no camera source. A missing virtual display never falls back to a real desktop.
public enum TipoDeFonte: Hashable, Sendable {
    case tela(CGDirectDisplayID)
    case telaEstendida(ModoDoMonitorVirtual)
}
public struct FonteDeCaptura: Identifiable, Hashable, Sendable {
    public let id: String
    public let tipo: TipoDeFonte
    public let nome: String
    public let detalhe: String
    public init(id: String, tipo: TipoDeFonte, nome: String, detalhe: String) {
        self.id = id; self.tipo = tipo; self.nome = nome; self.detalhe = detalhe
    }
    public var ehTela: Bool { true }
    public var presetSugerido: CapturePreset { .screen }
    public var modoDaTelaEstendida: ModoDoMonitorVirtual? {
        if case .telaEstendida(let modo) = tipo { return modo }
        return nil
    }
    public static func telaEstendida(_ modo: ModoDoMonitorVirtual) -> Self {
        Self(id: "tela-estendida", tipo: .telaEstendida(modo), nome: T("Tela estendida"),
             detalhe: "\(modo.rotulo) · \(modo.fps) fps")
    }
}
