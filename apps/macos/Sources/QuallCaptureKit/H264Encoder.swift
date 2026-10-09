import Foundation
import VideoToolbox
import CoreMedia

/// Qual encoder foi de fato usado — não o que foi pedido. VideoToolbox pode recusar o pedido de
/// hardware e cair silenciosamente para software; a única forma confiável de saber é perguntar à
/// sessão já criada.
public enum EncoderBackend: String, Codable {
    case hardware = "videotoolbox_hardware"
    case software = "videotoolbox_software"
}

public enum EncoderError: Error, CustomStringConvertible {
    case sessionCreationFailed(OSStatus)

    public var description: String {
        switch self {
        case .sessionCreationFailed(let status):
            return "VTCompressionSessionCreate falhou com status \(status)."
        }
    }
}

/// Envolve uma `VTCompressionSession` configurada para H.264 baseline, GOP curto e sem
/// reordenamento de quadros — zero filas, latência antes de taxa de bits, como pedido pelo
/// protocolo do projeto (ver `crates/quall-core/src/protocol.rs`, `EncodePreset`).
///
/// Tenta primeiro criar a sessão *exigindo* encoder de hardware
/// (`kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder`); se o VideoToolbox
/// recusar, cai para a criação sem essa exigência (que pode resultar em software) e, nos dois
/// casos, confirma o resultado real consultando
/// `kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder` na sessão já criada.
public final class H264Encoder {
    public typealias OutputHandler = (_ annexBData: Data, _ presentationTimeStamp: CMTime, _ isKeyframe: Bool) -> Void

    /// O que um preset resolve para, em termos que o VideoToolbox entende — e que o sidecar
    /// precisa reportar (`target_bitrate_bps`, `gop_frames`).
    private struct PresetTuning {
        let averageBitRateBps: Int
        let maxKeyFrameIntervalFrames: Int
        let maxKeyFrameIntervalDurationSeconds: Double
    }

    private let session: VTCompressionSession
    /// Reescreve o SPS para declarar que aqui não se reordena quadro. Ver `RemendoDeSPS`.
    private let remendoDeSPS = RemendoDeSPS()
    public let backend: EncoderBackend
    /// Nome de exibição do encoder que a sessão de fato escolheu (ex.: "Apple H.264"), resolvido
    /// via `kVTCompressionPropertyKey_EncoderID` + `VTCopyVideoEncoderList` — não um literal fixo,
    /// porque em outras máquinas (ex.: um Mac com GPU dedicada) o nome pode ser outro.
    public let encoderName: String
    public let targetBitrateBps: Int
    public let gopFrames: Int

    /// O que o VideoToolbox respondeu ao pedido de `kVTCompressionPropertyKey_MaxH264SliceBytes`,
    /// e o valor que ele devolveu quando relido. **`noErr` aqui não é prova de nada**: a regra da
    /// casa é conferir no artefato (`tools/fatias.py` conta as fatias por unidade de acesso). Esta
    /// propriedade existe para o relatório poder dizer *"a API aceitou e o bitstream desmentiu"*,
    /// que foi o que aconteceu cinco vezes neste projeto (`docs/quinta-porta.md`).
    public private(set) var respostaDoLimiteDeFatia: String = "não pedido"

    /// O que o VideoToolbox respondeu ao teto de bytes por quadro (`DataRateLimits`), e o que
    /// devolveu quando relido. Mesma regra de `respostaDoLimiteDeFatia`: aceitar não é cumprir —
    /// quem diz se o IDR encolheu é o tamanho do maior quadro no receptor.
    public private(set) var respostaDoTetoDeQuadro: String = "não pedido"

    /// Limite de bytes por fatia H.264, ou `0` para o padrão do VideoToolbox (sem limite).
    ///
    /// **Produto: 0.** Existe porque `docs/idr-que-sobrevive.md` mediu que o enlace de 2,4 GHz
    /// trunca a **cauda** de uma rajada acima de ~35 pacotes e entrega a cabeça inteira: um IDR
    /// de 60 pacotes fatiado em quatro chegaria com as primeiras fatias íntegras. Metade desse
    /// conserto é aqui; a outra metade é o depacotizador, que hoje descarta a unidade de acesso
    /// inteira ao primeiro buraco (`crates/quall-core/src/rtp.rs`, `abortar()`), e não é desta
    /// frente.
    ///
    /// Fatiar **não** encolhe a rajada — os bytes são os mesmos. Não confunda com refresh intra,
    /// que o VideoToolbox **não oferece**: não existe chave de refresh intra gradual na API
    /// pública, nem no SDK do macOS nem no do iOS (verificado nos cabeçalhos do Xcode 26.5).
    /// Fatias por quadro pela propriedade **privada** `NumberOfSlices`, ou `0` para não pedir.
    /// Produto: 0. Ver o comentário no corpo — ela não é pública, e o bitstream é quem decide.
    ///
    /// `taxaBps`, quando dado, é a taxa **que o teto já decidiu** para esta geometria — hoje só a
    /// tela estendida passa, com o teto do núcleo. Sem ela o encoder pergunta à cópia local
    /// (`TetoDoEmissor.tetoDeTaxa`), que é o comportamento de antes, byte a byte.
    ///
    /// `tetoDeQuadroBytes`, quando maior que zero, pede ao VideoToolbox que nenhum trecho de um
    /// tempo de quadro passe desse tanto de bytes — na prática, um teto por quadro, e é o IDR que
    /// ele morde. Existe pela medida de 11/09/2026 (`docs/tela-estendida.md`, "De onde vêm os
    /// trancos"): na tela estendida do iPhone X o IDR chegava a ~740 KB, e cada um levava ~100 ms
    /// para atravessar. `tetoDeQuadroEmQuadrosMedios` diz o mesmo em múltiplos do quadro médio da
    /// sessão (taxa ÷ 8 ÷ fps), e só vale quando `tetoDeQuadroBytes` é zero — é o que a tela
    /// estendida usa por padrão, para o teto acompanhar a resolução. Ver
    /// `TransmissaoAoVivo.tetoDeQuadroEmQuadrosMedios`.
    public init(width: Int32, height: Int32, fps: Int32, preset: CapturePreset,
                maxBytesPorFatia: Int32 = 0, fatiasPorQuadro: Int32 = 0, taxaBps: Int? = nil,
                gopSegundos: Double? = nil, tetoDeQuadroBytes: Int = 0,
                tetoDeQuadroEmQuadrosMedios: Double = 0) throws {
        let hardwareRequiredSpec: [CFString: Any] = [
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: true,
            kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true,
        ]

        var createdSession: VTCompressionSession?
        var status = VTCompressionSessionCreate(
            allocator: nil,
            width: width,
            height: height,
            codecType: kCMVideoCodecType_H264,
            encoderSpecification: hardwareRequiredSpec as CFDictionary,
            imageBufferAttributes: nil,
            compressedDataAllocator: nil,
            outputCallback: nil,
            refcon: nil,
            compressionSessionOut: &createdSession
        )

        if status != noErr || createdSession == nil {
            // VideoToolbox recusou exigir hardware nesta máquina/config — tenta de novo deixando
            // o VideoToolbox escolher livremente (pode resultar em software; confirmado abaixo).
            status = VTCompressionSessionCreate(
                allocator: nil,
                width: width,
                height: height,
                codecType: kCMVideoCodecType_H264,
                encoderSpecification: nil,
                imageBufferAttributes: nil,
                compressedDataAllocator: nil,
                outputCallback: nil,
                refcon: nil,
                compressionSessionOut: &createdSession
            )
        }

        guard status == noErr, let session = createdSession else {
            throw EncoderError.sessionCreationFailed(status)
        }
        self.session = session

        let tuning = H264Encoder.configure(
            session: session, largura: Int(width), altura: Int(height), fps: fps, preset: preset,
            taxaBps: taxaBps, gopSegundos: gopSegundos)
        if fatiasPorQuadro > 0 {
            // `NumberOfSlices` **não é uma constante pública**: ela não existe em
            // `VTCompressionProperties.h`, mas o encoder desta máquina a declara em
            // `VTSessionCopySupportedPropertyDictionary` — 133 propriedades com hardware, 70 sem,
            // e esta é a única com "slice" no nome nas duas listas. Foi achada perguntando à
            // sessão, e não lendo cabeçalho: `docs/regras-de-frente.md` é explícito que "não achei
            // a linha" não é "não existe".
            //
            // Sendo privada, ela pode sumir numa atualização do sistema sem aviso. Por isso o
            // `respostaDoLimiteDeFatia` guarda o status e o valor relido, e por isso quem decide se
            // ela funcionou é `tools/fatias.py` no bitstream.
            let chave = "NumberOfSlices" as CFString
            let st = VTSessionSetProperty(session, key: chave,
                                          value: NSNumber(value: fatiasPorQuadro))
            var lidoFatias: CFTypeRef?
            let stLeituraFatias = VTSessionCopyProperty(session, key: chave, allocator: nil,
                                                        valueOut: &lidoFatias)
            self.respostaDoLimiteDeFatia =
                "NumberOfSlices pedido \(fatiasPorQuadro) · set=\(st) · get=\(stLeituraFatias) "
                + "· relido=\((lidoFatias as? NSNumber)?.intValue.description ?? "nada")"
        }
        if maxBytesPorFatia > 0 {
            let st = VTSessionSetProperty(
                session,
                key: kVTCompressionPropertyKey_MaxH264SliceBytes,
                value: NSNumber(value: maxBytesPorFatia)
            )
            var lido: CFTypeRef?
            let stLeitura = VTSessionCopyProperty(
                session,
                key: kVTCompressionPropertyKey_MaxH264SliceBytes,
                allocator: nil,
                valueOut: &lido
            )
            let devolvido = (lido as? NSNumber)?.intValue
            self.respostaDoLimiteDeFatia =
                "pedido \(maxBytesPorFatia) B · set=\(st) · get=\(stLeitura) "
                + "· relido=\(devolvido.map(String.init) ?? "nada")"
        }
        let quadroMedio = Double(tuning.averageBitRateBps) / 8 / Double(max(1, fps))
        let tetoDeQuadroBytes = tetoDeQuadroBytes > 0
            ? tetoDeQuadroBytes
            : Int((quadroMedio * tetoDeQuadroEmQuadrosMedios).rounded())
        if tetoDeQuadroBytes > 0 {
            // `DataRateLimits` é uma lista de pares (bytes, segundos): nenhum trecho contíguo
            // daquela duração, em tempo de decode, pode passar daquele tamanho. Com a duração de
            // um quadro, o par vira teto por quadro. É **teto duro**, ao contrário do
            // `AverageBitRate`, que é meta de média — e foi a meta que deixou o tablet mandar
            // ~19 Mbps com 10 pedidos.
            let limites = [NSNumber(value: tetoDeQuadroBytes),
                           NSNumber(value: 1.0 / Double(max(1, fps)))] as CFArray
            let st = VTSessionSetProperty(session, key: kVTCompressionPropertyKey_DataRateLimits,
                                          value: limites)
            var lido: CFTypeRef?
            let stLeitura = VTSessionCopyProperty(session,
                                                  key: kVTCompressionPropertyKey_DataRateLimits,
                                                  allocator: nil, valueOut: &lido)
            let relido = (lido as? [NSNumber])?.map { $0.stringValue }.joined(separator: ",")
            self.respostaDoTetoDeQuadro =
                "pedido \(tetoDeQuadroBytes) B por 1/\(fps) s "
                + String(format: "(%.1f quadros médios) ", Double(tetoDeQuadroBytes) / max(1, quadroMedio))
                + "· set=\(st) · get=\(stLeitura) · relido=\(relido ?? "nada")"
        }
        self.targetBitrateBps = tuning.averageBitRateBps
        self.gopFrames = tuning.maxKeyFrameIntervalFrames
        VTCompressionSessionPrepareToEncodeFrames(session)

        var usesHardwareRef: CFTypeRef?
        VTSessionCopyProperty(
            session,
            key: kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
            allocator: nil,
            valueOut: &usesHardwareRef
        )
        let usesHardware = (usesHardwareRef as? Bool) ?? false
        self.backend = usesHardware ? .hardware : .software
        self.encoderName = H264Encoder.resolveEncoderName(session: session, fallback: self.backend.rawValue)
    }

    @discardableResult
    private static func configure(session: VTCompressionSession, largura: Int, altura: Int,
                                  fps: Int32, preset: CapturePreset, taxaBps: Int? = nil,
                                  gopSegundos: Double? = nil) -> PresetTuning {
        func set(_ key: CFString, _ value: CFTypeRef) {
            VTSessionSetProperty(session, key: key, value: value)
        }

        // Zero filas: tempo real, sem reordenamento (sem B-frames), baseline é o denominador
        // comum entre todas as plataformas do projeto.
        set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
        set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse)
        set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Baseline_AutoLevel)
        set(kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: fps))

        // **O teto de taxa não é escolhido aqui, é perguntado.** Até 02/09/2026 estes dois
        // literais eram 4 000 000 e 6 000 000, e não sabiam nada sobre o tamanho do quadro: a
        // mesma tela em 720p e em 1080p pedia os mesmos bits, ou seja, 1080p com 2,25 vezes menos
        // bits por pixel. Ver `TetoDoEmissor.tetoDeTaxa`.
        //
        // A diferença entre os dois presets sobrevive como **razão**, e não como número: a
        // referência do núcleo é densidade de tela, e a câmera continua pedindo metade a mais
        // para absorver o ruído de sensor sem esborratar. 4 e 6 Mbps a 720p continuam sendo 4 e
        // 6 Mbps a 720p.
        let daResolucao = taxaBps ?? TetoDoEmissor.tetoDeTaxa(largura: largura, altura: altura, fps: fps)
        let tuning: PresetTuning
        switch preset {
        case .screen:
            // Conteúdo estático com mudanças bruscas: GOP curto (~1s) para que uma mudança de
            // cena não fique presa esperando o próximo IDR agendado, e um teto de bitrate mais
            // baixo porque telas comprimem bem (grandes áreas planas, pouco ruído).
            //
            // `gopSegundos` sobrepõe isso só para quem pede — hoje a tela estendida. Medido em
            // 10/09 no laço local, 1920 × 1200 a 60 fps com a tela parada: **31 IDR em 104 quadros**,
            // o maior com 698.937 bytes (~583 pacotes RTP). Com poucos quadros por segundo o controle
            // de taxa gasta a verba em IDR, e o GOP de 1 s pede um por segundo — uma rajada de
            // centenas de pacotes por segundo numa tela onde nada mudou. Ver `docs/tela-estendida.md`.
            let gop = gopSegundos ?? 1.0
            tuning = PresetTuning(averageBitRateBps: daResolucao,
                                  maxKeyFrameIntervalFrames: max(1, Int((gop * Double(fps)).rounded())),
                                  maxKeyFrameIntervalDurationSeconds: gop)
        case .camera:
            // Movimento contínuo e ruído de sensor: quadro a quadro já é redundante o bastante
            // para não precisar de refresh total tão frequente; GOP um pouco mais largo (~2s) e
            // bitrate maior para absorver o ruído sem esborratar.
            tuning = PresetTuning(averageBitRateBps: daResolucao * 3 / 2, maxKeyFrameIntervalFrames: Int(fps) * 2, maxKeyFrameIntervalDurationSeconds: 2.0)
        }

        set(kVTCompressionPropertyKey_MaxKeyFrameInterval, NSNumber(value: tuning.maxKeyFrameIntervalFrames))
        set(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, NSNumber(value: tuning.maxKeyFrameIntervalDurationSeconds))
        set(kVTCompressionPropertyKey_AverageBitRate, NSNumber(value: tuning.averageBitRateBps))

        return tuning
    }

    /// Resolve o `EncoderID` que a sessão de fato escolheu para um nome de exibição, consultando
    /// `VTCopyVideoEncoderList` (a mesma lista que `kVTVideoEncoderList_EncoderName` alimenta no
    /// Painel de Sistema). Se por algum motivo a consulta falhar, cai para o nome do backend
    /// (`videotoolbox_hardware`/`videotoolbox_software`) em vez de inventar um nome.
    private static func resolveEncoderName(session: VTCompressionSession, fallback: String) -> String {
        var encoderIdRef: CFTypeRef?
        VTSessionCopyProperty(session, key: kVTCompressionPropertyKey_EncoderID, allocator: nil, valueOut: &encoderIdRef)
        guard let encoderId = encoderIdRef as? String else { return fallback }

        var encoderListRef: CFArray?
        guard VTCopyVideoEncoderList(nil, &encoderListRef) == noErr, let encoderList = encoderListRef as? [[CFString: Any]] else {
            return fallback
        }
        for entry in encoderList {
            if let id = entry[kVTVideoEncoderList_EncoderID] as? String, id == encoderId {
                return (entry[kVTVideoEncoderList_EncoderName] as? String) ?? encoderId
            }
        }
        return encoderId
    }

    /// O que o remendo de SPS fez nesta sessão, em uma linha — para o relatório da corrida.
    public var resumoDoSPS: String { remendoDeSPS.resumo() }

    /// Submete um `CVPixelBuffer` capturado para encode. O `outputHandler` pode ser chamado
    /// de forma assíncrona, numa fila diferente da que chamou este método.
    /// - Parameter forcarIDR: quando `true`, pede um quadro-chave **neste** quadro em vez de
    ///   esperar o próximo do GOP. É o que fecha o caminho do `pedir_idr` do contrato: o receptor
    ///   que entrou no meio do fluxo emite PLI, o núcleo levanta a bandeira
    ///   (`quall_track_take_idr_request`) e a casca a consome aqui. Sem isso o receptor esperaria
    ///   até um segundo inteiro pela primeira imagem — e a regra do projeto é conferir o pedido no
    ///   artefato, não no retorno da API: quem confirma que o IDR saiu é o `idr` do quadro
    ///   entregue ao `outputHandler`, não esta bandeira.
    public func encode(
        pixelBuffer: CVPixelBuffer,
        presentationTimeStamp: CMTime,
        duration: CMTime,
        forcarIDR: Bool = false,
        outputHandler: @escaping OutputHandler
    ) {
        // O sinal de vídeo é lido **aqui**, do buffer que está sendo submetido: é o único ponto em
        // que a verdade sobre faixa e cor da origem está à mão. O callback não recebe o pixel
        // buffer, e a format description comprimida do VideoToolbox não propaga esses atributos —
        // medido neste Mac em 2026-08-24.
        let remendo = remendoDeSPS
        let sinal = RemendoDeSPS.SinalDeVideo.doPixelBuffer(pixelBuffer)
        let propriedades: CFDictionary? = forcarIDR
            ? [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue!] as CFDictionary
            : nil
        let status = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: pixelBuffer,
            presentationTimeStamp: presentationTimeStamp,
            duration: duration,
            frameProperties: propriedades,
            infoFlagsOut: nil
        ) { status, infoFlags, sampleBuffer in
            guard status == noErr, !infoFlags.contains(.frameDropped), let sampleBuffer else { return }
            guard CMSampleBufferGetNumSamples(sampleBuffer) > 0 else { return }
            guard let annexB = AnnexB.convert(sampleBuffer, remendo: remendo, sinal: sinal) else { return }
            outputHandler(annexB, presentationTimeStamp, AnnexB.isKeyframe(sampleBuffer))
        }
        if status != noErr {
            FileHandle.standardError.write(
                "quall-capture: VTCompressionSessionEncodeFrameWithOutputHandler falhou (status \(status))\n".data(using: .utf8)!
            )
        }
    }

    public func finish() {
        VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
        VTCompressionSessionInvalidate(session)
    }
}
