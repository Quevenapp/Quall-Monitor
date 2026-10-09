import CQuall
import Foundation
import os
import QuallReceptorKit

/// **A porta puxada de uma track de som, com o decodificador junto** (`docs/contrato-som-puxado.md`).
///
/// É a metade da fronteira do som do receptor macOS; a outra metade, o motor, é ``Tocador``. Entre
/// as duas passa uma ``FonteDeSlots``: o motor pede um slot, esta classe puxa a porta
/// (`quall_audio_playout_pull`) e decodifica o slot no buffer do motor.
///
/// - **Opus** pela fronteira (`quall_audio_decoder_decode`), com as quatro ordens: `FRAME`
///   decodifica, `FEC` usa o LBRR do sucessor só com `fec_has_lbrr == 1`, `SILENCE` é a ocultação
///   do próprio decoder, `IDLE` são zeros (o montador escreve).
/// - **PCMU** na casca, pela ``MuLaw``: a fronteira recusa PCMU de propósito, e o emissor do Mac
///   manda PCMU (risco R6). O motor recebe **48 kHz**: a casca interpola por 6
///   (``InterpoladorPor6``). `SILENCE` desce em rampa de 5 ms até zero, e não num corte seco — o
///   corte era um estalo por pacote perdido (crítica 9, miúdo 3).
///
/// A porta abre com `shell_resamples = true`: quem acompanha a deriva é o Varispeed do motor, e o
/// núcleo nunca descarta nem insere slot por ela.
///
/// # Tempo real, e o fim
///
/// ``fonte`` roda na thread do dispositivo. Não aloca: o slot e o PCM de 16 bits são buffers fixos,
/// alocados no `init`. `pull` numa thread de cada vez — a do render, e só ela.
///
/// **A fonte segura esta porta com referência forte**, e o ponteiro da porta puxada fica atrás de um
/// cadeado: a puxada o tenta (`withLockIfAvailable`) e ``encerrar()`` o toma, espera a puxada em
/// curso acabar, zera e só então libera. Antes, a fonte era `unowned` e o ponteiro um `var` comum:
/// um render que escapasse do desmonte lia memória liberada (crítica 9, G1).
public final class PortaDeSom {

    public let especie: EspecieDeTrack
    public let codec: String
    public let formato: Tocador.Formato
    public let pcmu: Bool

    /// A porta puxada, do lado do render. `nil` depois de ``encerrar()``.
    private let porta: OSAllocatedUnfairLock<OpaquePointer?>
    /// A mesma porta, do lado do controle: `rate`, `stats` e `free` vêm todos da thread da sessão
    /// (`contrato-som-puxado.md` §2), então esta cópia não precisa de cadeado — e ler por ela não
    /// disputa o cadeado com o render, que numa disputa entregaria um slot ocioso.
    private var doControle: OpaquePointer?
    private var decodificador: OpaquePointer?
    private let slot: UnsafeMutablePointer<QuallAudioSlot>
    private let pcm: UnsafeMutablePointer<Int16>
    private let capacidade: Int
    /// PCMU: o slot a 8 kHz, antes de interpolar para 48.
    private let a8k: UnsafeMutablePointer<Float>
    private let porSlotNoFio: Int
    private var interpolador = InterpoladorPor6()

    /// A claquete (`--claquete`): o detector do estouro no som que sai, e, para cada estouro, a
    /// hora do host em que ele sai no DAC e onde ele cai no carimbo da track (`timestamp_us` do
    /// slot mais a posição dentro dele). Escritos pela thread do dispositivo com `trylock`.
    public struct Estouro: Equatable {
        public var noDacUs: UInt64
        public var carimboUs: Int64
    }
    private var detector: DetectorDeEstouro?
    private let estouros = OSAllocatedUnfairLock(initialState: [Estouro]())

    /// Abre a porta na track. `nil` com o motivo em `motivo`.
    public static func abrir(track t: OpaquePointer) -> (porta: PortaDeSom?, motivo: String) {
        let tipo = quall_track_kind(t)
        let especie = EspecieDeTrack(bruto: tipo.rawValue)
        guard especie.eAudio else { return (nil, "a track não é de som (\(especie))") }
        let codec = quall_track_audio_codec(t)
        guard codec != QUALL_AUDIO_CODEC_DEFAULT else {
            return (nil, "a track não disse o codec: \(NucleoReceptor.ultimoErro())")
        }
        let json = NucleoDeRede.lerTexto { buf, cap in quall_audio_preset_json(tipo, codec, buf, cap) }
        guard let dados = json.data(using: .utf8),
              let p = try? JSONSerialization.jsonObject(with: dados) as? [String: Any],
              let taxa = p["sample_rate_hz"] as? Int, let canais = p["channels"] as? Int,
              let porQuadro = p["frame_samples"] as? Int, taxa > 0, canais > 0, porQuadro > 0 else {
            return (nil, "o preset da track não se leu: \(json)")
        }
        let nome = (p["codec"] as? String) ?? "?"
        // O atraso interno é o que o sinal que sai daqui tem contra o carimbo: o filtro por 6 do
        // PCMU, mais o do codec — no Opus, o lookahead do codificador do emissor (6,5 ms), que o
        // núcleo publica no preset (`content_delay_us`). Sem ele, o T0 media +6,6 ms no Opus e a
        // testemunha A saía 6,5 ms curta (`docs/som-no-receptor.md` §20.7 e §21). Lido antes de
        // abrir qualquer coisa: numa recusa, nada a soltar.
        guard let conteudoUs = (p["content_delay_us"] as? NSNumber)?.doubleValue, conteudoUs >= 0 else {
            return (nil, "o preset da track não diz content_delay_us: \(json)")
        }
        let pcmu = codec == QUALL_AUDIO_CODEC_PCMU
        var decodificador: OpaquePointer?
        if !pcmu {
            decodificador = quall_audio_decoder_new(tipo, codec)
            guard decodificador != nil else {
                return (nil, "o decodificador não abriu: \(NucleoReceptor.ultimoErro())")
            }
        }
        guard let porta = quall_audio_playout_new(t, true) else {
            let motivo = NucleoReceptor.ultimoErro()
            if let decodificador { quall_audio_decoder_free(decodificador) }
            return (nil, "a porta puxada não abriu: \(motivo)")
        }
        // PCMU vai ao motor a 48 kHz, mono, com o atraso do filtro.
        let formato = pcmu
            ? Tocador.Formato(canais: 1, taxaHz: Double(taxa * InterpoladorPor6.fator),
                              amostrasPorSlot: porQuadro * InterpoladorPor6.fator,
                              atrasoInternoUs: InterpoladorPor6.atrasoUs + conteudoUs)
            : Tocador.Formato(canais: canais, taxaHz: Double(taxa), amostrasPorSlot: porQuadro,
                              atrasoInternoUs: conteudoUs)
        return (PortaDeSom(especie: especie, codec: nome, formato: formato, pcmu: pcmu,
                           porSlotNoFio: porQuadro, porta: porta, decodificador: decodificador), "")
    }

    private init(especie: EspecieDeTrack, codec: String, formato: Tocador.Formato, pcmu: Bool,
                 porSlotNoFio: Int, porta: OpaquePointer, decodificador: OpaquePointer?) {
        self.especie = especie
        self.codec = codec
        self.formato = formato
        self.pcmu = pcmu
        // `unchecked`: o ponteiro opaco não é `Sendable`; quem garante o acesso é o próprio cadeado.
        self.porta = OSAllocatedUnfairLock(uncheckedState: porta)
        doControle = porta
        self.decodificador = decodificador
        self.porSlotNoFio = porSlotNoFio
        a8k = .allocate(capacity: porSlotNoFio)
        a8k.initialize(repeating: 0, count: porSlotNoFio)
        slot = .allocate(capacity: 1)
        slot.initialize(to: QuallAudioSlot())
        capacidade = formato.amostrasPorSlot * formato.canais
        pcm = .allocate(capacity: capacidade)
        pcm.initialize(repeating: 0, count: capacidade)
        estouros.withLock { $0.reserveCapacity(256) }
        // A tabela é um `static let`: a primeira leitura a inicializa, com alocação. Que seja aqui,
        // e não na thread do dispositivo.
        _ = MuLaw.tabela.count
    }

    deinit {
        _ = encerrar()
        slot.deallocate()
        pcm.deallocate()
        a8k.deallocate()
        interpolador.liberar()
        detector?.liberar()
    }

    // MARK: - a fonte (thread do dispositivo)

    /// A ``FonteDeSlots`` do motor. **Só o motor chama**, numa thread de cada vez. Segura esta porta
    /// com referência forte: ela vive enquanto o motor viver.
    public var fonte: FonteDeSlots {
        { [self] atraso, noDac, razao, destino, cap in self.puxar(atraso, noDac, razao, destino, cap) }
    }

    /// Liga o detector do estouro da claquete. **Antes** de o motor ligar.
    public func ligarClaquete() {
        if detector == nil { detector = DetectorDeEstouro(taxaHz: formato.taxaHz) }
    }

    /// Os estouros detectados desde a última leitura.
    public func tirarEstouros() -> [Estouro] {
        estouros.withLock { v in
            let copia = v
            v.removeAll(keepingCapacity: true)
            return copia
        }
    }

    private func puxar(_ atraso: UInt32, _ noDac: UInt64, _ razao: Double,
                       _ destino: UnsafeMutablePointer<Float>, _ cap: Int) -> SlotPuxado {
        let ocioso = SlotPuxado(ordem: .ocioso, amostrasPorCanal: 0, timestampUs: 0)
        // Com o cadeado ocupado (o `encerrar` em curso) ou a porta já solta, é ocioso.
        let feito = porta.withLockIfAvailableUnchecked { p -> SlotPuxado in
            guard let p else { return ocioso }
            return puxarEDecodificar(p, atraso, razao, destino, cap)
        }
        let s = feito ?? ocioso
        if detector != nil, s.ordem != .ocioso, s.amostrasPorCanal > 0,
           let o = detector!.processar(destino, s.amostrasPorCanal, salto: formato.canais) {
            let r = razao.isFinite && razao > 0 ? razao : 1
            // `o` é o índice no som que sai, que chega `atrasoInternoUs` atrás do carimbo (o FIR
            // do PCMU; o lookahead do codificador no Opus). O `noDac` já é a hora do DAC do **começo de mídia** do slot,
            // com esse atraso somado. Em tempo de mídia, o estouro está em `o − atraso`. Sem o
            // desconto, a testemunha B contava o filtro duas vezes (crítica 10, m7).
            let emMidiaUs = o / formato.taxaHz * 1_000_000 - formato.atrasoInternoUs
            let naSaida = Double(noDac) + emMidiaUs / r
            let noCarimbo = Double(s.timestampUs) + emMidiaUs
            _ = estouros.withLockIfAvailable { v in
                if v.count < v.capacity {
                    v.append(Estouro(noDacUs: UInt64(max(0, naSaida)), carimboUs: Int64(noCarimbo)))
                }
            }
        }
        return s
    }

    private func puxarEDecodificar(_ p: OpaquePointer, _ atraso: UInt32, _ razao: Double,
                                   _ destino: UnsafeMutablePointer<Float>, _ cap: Int) -> SlotPuxado {
        guard quall_audio_playout_pull(p, atraso, razao, slot) == QUALL_STATUS_OK else {
            return SlotPuxado(ordem: .ocioso, amostrasPorCanal: 0, timestampUs: 0)
        }
        let s = slot.pointee
        let ordem = OrdemPuxada(rawValue: s.order.rawValue) ?? .silencio
        switch ordem {
        case .ocioso:
            return SlotPuxado(ordem: .ocioso, amostrasPorCanal: 0, timestampUs: 0)
        case .quadro:
            let n = pcmu ? muLaw(s, destino, cap) : opus(s.payload, s.len, fec: false, destino, cap)
            return SlotPuxado(ordem: .quadro, amostrasPorCanal: n, timestampUs: s.timestamp_us)
        case .cura:
            // Só com LBRR de verdade: sem ele, `decode_fec` cai na ocultação e devolve sucesso.
            let n = pcmu ? silencio(destino, cap)
                : (s.fec_has_lbrr == 1 ? opus(s.payload, s.len, fec: true, destino, cap)
                                       : opus(nil, 0, fec: false, destino, cap))
            return SlotPuxado(ordem: .cura, amostrasPorCanal: n, timestampUs: s.timestamp_us)
        case .silencio:
            let n = pcmu ? silencio(destino, cap) : opus(nil, 0, fec: false, destino, cap)
            return SlotPuxado(ordem: .silencio, amostrasPorCanal: n, timestampUs: s.timestamp_us)
        }
    }

    private func muLaw(_ s: QuallAudioSlot, _ destino: UnsafeMutablePointer<Float>, _ cap: Int) -> Int {
        guard let p = s.payload else { return silencio(destino, cap) }
        let n = min(Int(s.len), porSlotNoFio, cap / InterpoladorPor6.fator)
        MuLaw.tabela.withUnsafeBufferPointer { tabela in
            for i in 0..<n { a8k[i] = tabela[Int(p[i])] }
        }
        interpolador.processar(a8k, n, destino)
        return n * InterpoladorPor6.fator
    }

    /// PCMU: 5 ms de rampa da última amostra até zero, e zeros, pelo filtro. Opus: nunca chega
    /// aqui com a ordem de silêncio (a ocultação é do decoder); só na falha do decode.
    private func silencio(_ destino: UnsafeMutablePointer<Float>, _ cap: Int) -> Int {
        if pcmu {
            let ultima = interpolador.ultimaEntrada
            let rampa = min(40, porSlotNoFio)
            for i in 0..<porSlotNoFio {
                a8k[i] = i < rampa ? ultima * Float(rampa - 1 - i) / Float(rampa) : 0
            }
            interpolador.processar(a8k, min(porSlotNoFio, cap / InterpoladorPor6.fator), destino)
            return formato.amostrasPorSlot
        }
        destino.update(repeating: 0, count: min(cap, capacidade))
        return formato.amostrasPorSlot
    }

    private func opus(_ pacote: UnsafePointer<UInt8>?, _ len: UInt, fec: Bool,
                      _ destino: UnsafeMutablePointer<Float>, _ cap: Int) -> Int {
        guard let decodificador else { return silencio(destino, cap) }
        let n = quall_audio_decoder_decode(decodificador, pacote, len, fec, pcm, UInt(capacidade))
        guard n > 0 else { return silencio(destino, cap) }
        let total = min(Int(n) * formato.canais, cap, capacidade)
        let escala: Float = 1 / 32768
        for i in 0..<total { destino[i] = Float(pcm[i]) * escala }
        return Int(n)
    }

    // MARK: - o controle (qualquer thread)

    /// A razão sugerida, ou `NaN` quando não medida. Da thread da sessão.
    public func razaoSugerida() -> Double {
        doControle.map { quall_audio_playout_rate($0) } ?? .nan
    }

    /// `quall_audio_playout_stats_json`. Da thread da sessão.
    public func contadores() -> String {
        guard let p = doControle else { return "{}" }
        return NucleoDeRede.lerTexto { buf, cap in quall_audio_playout_stats_json(p, buf, cap) }
    }

    /// Solta a porta e o decodificador. Toma o cadeado: espera a puxada em curso acabar, e as
    /// seguintes veem a porta solta. **Chame depois de parar o motor**; se um render escapar, ele
    /// só vê ocioso. Devolve o status da barreira, ou `nil` se já estava solta.
    @discardableResult
    public func encerrar() -> QuallStatus? {
        doControle = nil
        guard let p = porta.withLockUnchecked({ v -> OpaquePointer? in let antes = v; v = nil; return antes }) else {
            return nil
        }
        let st = quall_audio_playout_free(p)
        if let d = decodificador {
            decodificador = nil
            quall_audio_decoder_free(d)
        }
        return st
    }
}
