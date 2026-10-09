import Foundation

/// Espelha `EncodePreset` de `crates/quall-core/src/protocol.rs` (propriedade da Frente 1 —
/// aquele arquivo não é editado por aqui). Tela e câmera têm características opostas: tela é
/// conteúdo estático com mudanças bruscas (prioriza nitidez de texto), câmera é ruído de sensor
/// com movimento contínuo (prioriza fluidez). Quando o app macOS ganhar uma ponte FFI real com o
/// núcleo Rust, este enum deve ser substituído pelo tipo vindo de lá — por ora é a cópia mínima
/// necessária para escolher a configuração de encode.
public enum CapturePreset: String, Codable, CaseIterable, CustomStringConvertible {
    case screen
    case camera

    public var description: String { rawValue }
}
