import Foundation

/// Espelha `CodecDeAudio` de `crates/quall-core/src/track.rs` (propriedade da frente do núcleo —
/// aquele arquivo não é editado por aqui), no mesmo espírito de `CapturePreset`.
///
/// A casca precisa deste enum por um motivo concreto e não por simetria: **o número de amostras
/// que cabe num quadro depende do relógio do codec**, e é a casca que fatia o PCM que sai do
/// ScreenCaptureKit. Errar isso não dá erro em lugar nenhum — dá áudio que acelera ou arrasta,
/// que é exatamente o defeito que `docs/audio.md` §2 usa para explicar por que o relógio do Opus
/// é 48 kHz "independentemente da taxa interna do encoder".
public enum CodecDeAudio: String, Sendable, CaseIterable, CustomStringConvertible {
    /// Opus a 48 kHz (RFC 7587). O codec do produto.
    case opus
    /// G.711 µ-law a 8 kHz (RFC 3551). Piso sem dependência.
    case pcmu

    public var description: String { rawValue }

    /// A taxa do relógio RTP, em Hz. **Não é a taxa do encoder**: para o Opus a RFC 7587 §4.1
    /// fixa 48 000 mesmo quando o encoder trabalha internamente em 16 kHz.
    public var relogioHz: Int {
        switch self {
        case .opus: return 48_000
        case .pcmu: return 8_000
        }
    }

    /// A taxa em que a casca precisa **entregar amostras ao encoder**.
    ///
    /// Para o Opus coincide com o relógio RTP. Para o PCMU também, e por isso a distinção não
    /// aparece aqui — ela existe como conceito separado porque um terceiro codec pode quebrá-la,
    /// e nesse dia o lugar de consertar é este, não o laço de captura.
    public var taxaDeAmostragem: Double { Double(relogioHz) }

    /// Quantas amostras por canal cabem num quadro da duração dada.
    /// A 48 kHz e 20 ms são 960; a 8 kHz e 20 ms, 160.
    public func amostrasPorQuadro(duracaoMs: Int) -> Int { relogioHz * duracaoMs / 1000 }

    /// Quantos canais este codec de fato transporta, que **nem sempre é o que o preset pede**.
    ///
    /// G.711 é mono por definição — a RFC 3551 §6 atribui o payload type 0 a `PCMU/8000/1`, e
    /// não há como transportar dois canais nele. Um preset de áudio de sistema pede estéreo; sob
    /// PCMU ele é rebaixado a mono aqui, num lugar só, em vez de o laço de captura descobrir
    /// sozinho que o que ele mandou não cabe.
    public func canaisNoFio(pedidos: Int) -> Int {
        switch self {
        case .opus: return pedidos
        case .pcmu: return 1
        }
    }
}

/// Espelha `PresetDeAudio` e a tabela `TrackKind::preset_de_audio` de
/// `crates/quall-core/src/track.rs`.
///
/// # Por que a casca tem uma cópia disto, e onde a cópia PARA
///
/// A casca precisa de três coisas para fatiar o PCM: **taxa, canais e duração do quadro**. Elas
/// estão aqui.
///
/// O que **não** está aqui, de propósito: `fec`, `perda_esperada_pct` e `conteudo_e_fala`. Esses
/// três configuram o **encoder**, e configurar o encoder na casca é o que `docs/audio.md` §11
/// mostra dar errado: `useinbandfec=1` foi anunciado no SDP por meses enquanto o byte no fio não
/// tinha LBRR nenhum, porque o preset do fio e o preset do encoder eram duas fontes de verdade.
/// Enquanto o encoder de Opus não vier da fronteira C configurado pela tabela do núcleo, esta
/// casca **não codifica Opus** — ela usa o PCMU, que não tem parâmetro nenhum para divergir.
///
/// Se você veio aqui para acrescentar um campo de encoder: não é aqui. É em `quall-ffi`.
public struct PresetDeAudio: Sendable, Equatable {
    public let codec: CodecDeAudio
    /// 1 para fala, 2 para som de sistema. É o que o preset **pede**; o que cabe no fio é
    /// `codec.canaisNoFio(pedidos:)`.
    public let canaisPedidos: Int
    public let duracaoDoQuadroMs: Int

    /// Canais que de fato saem daqui para o fio.
    public var canais: Int { codec.canaisNoFio(pedidos: canaisPedidos) }
    public var taxaDeAmostragem: Double { codec.taxaDeAmostragem }
    /// Amostras **por canal** num quadro.
    public var amostrasPorQuadro: Int { codec.amostrasPorQuadro(duracaoMs: duracaoDoQuadroMs) }
    /// Quantos microssegundos um quadro representa. É o passo do carimbo de tempo.
    public var duracaoDoQuadroUs: UInt64 { UInt64(duracaoDoQuadroMs) * 1000 }

    public init(codec: CodecDeAudio, canaisPedidos: Int, duracaoDoQuadroMs: Int = 20) {
        self.codec = codec
        self.canaisPedidos = canaisPedidos
        self.duracaoDoQuadroMs = duracaoDoQuadroMs
    }

    /// O preset de **áudio de sistema**: estéreo, 20 ms. Ver `docs/audio.md` §3.
    public static func audioDoSistema(codec: CodecDeAudio) -> PresetDeAudio {
        PresetDeAudio(codec: codec, canaisPedidos: 2)
    }
}
