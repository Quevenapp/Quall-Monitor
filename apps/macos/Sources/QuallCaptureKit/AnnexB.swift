import Foundation
import CoreMedia

/// VideoToolbox entrega os pacotes codificados em formato AVCC (cada NAL unit prefixado por um
/// comprimento big-endian de 4 bytes, SPS/PPS só na format description). O contrato desta frente
/// é Annex-B (start codes `00 00 00 01`, SPS/PPS embutidos no fluxo antes de cada IDR) — é o que
/// dá para escrever direto num `.h264` elementary stream e o formato mais universalmente aceito
/// entre plataformas. `AnnexB` faz essa conversão.
enum AnnexB {
    /// Um sample é keyframe (IDR) quando não carrega o attachment `NotSync` — convenção do
    /// VideoToolbox: ausência do attachment (ou presença com valor false) significa "sync sample".
    static func isKeyframe(_ sampleBuffer: CMSampleBuffer) -> Bool {
        guard
            let attachmentsArray = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[CFString: Any]],
            let attachments = attachmentsArray.first
        else {
            return true
        }
        let notSync = attachments[kCMSampleAttachmentKey_NotSync] as? Bool ?? false
        return !notSync
    }

    /// Converte um `CMSampleBuffer` codificado (AVCC) num `Data` Annex-B. Em quadros IDR, prefixa
    /// os parameter sets (SPS/PPS) extraídos da format description — sem isso um decoder que só
    /// entrou no meio do stream não consegue montar a primeira imagem.
    ///
    /// Com `remendo`, o SPS sai reescrito para **declarar** que este encoder não reordena quadro
    /// (`RemendoDeSPS`): sem essa declaração o decodificador do outro lado infere o teto do nível e
    /// empilha até nove quadros antes de entregar o primeiro. Sem `remendo`, o SPS vai como veio.
    static func convert(_ sampleBuffer: CMSampleBuffer,
                        remendo: RemendoDeSPS? = nil,
                        sinal: RemendoDeSPS.SinalDeVideo? = nil) -> Data? {
        guard let dataBuffer = CMSampleBufferGetDataBuffer(sampleBuffer) else { return nil }

        var totalLength = 0
        var dataPointer: UnsafeMutablePointer<Int8>?
        let status = CMBlockBufferGetDataPointer(
            dataBuffer,
            atOffset: 0,
            lengthAtOffsetOut: nil,
            totalLengthOut: &totalLength,
            dataPointerOut: &dataPointer
        )
        guard status == noErr, let dataPointer else { return nil }

        var output = Data()

        if isKeyframe(sampleBuffer), let formatDescription = CMSampleBufferGetFormatDescription(sampleBuffer) {
            var parameterSetCount = 0
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                formatDescription,
                parameterSetIndex: 0,
                parameterSetPointerOut: nil,
                parameterSetSizeOut: nil,
                parameterSetCountOut: &parameterSetCount,
                nalUnitHeaderLengthOut: nil
            )
            for index in 0..<parameterSetCount {
                var parameterSetPointer: UnsafePointer<UInt8>?
                var parameterSetSize = 0
                let psStatus = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    formatDescription,
                    parameterSetIndex: index,
                    parameterSetPointerOut: &parameterSetPointer,
                    parameterSetSizeOut: &parameterSetSize,
                    parameterSetCountOut: nil,
                    nalUnitHeaderLengthOut: nil
                )
                if psStatus == noErr, let parameterSetPointer {
                    output.append(contentsOf: [0, 0, 0, 1])
                    if let remendo, (parameterSetPointer[0] & 0x1F) == 7 {
                        let bruto = UnsafeRawBufferPointer(start: parameterSetPointer, count: parameterSetSize)
                        output.append(contentsOf: remendo.spsParaEnviar(bruto, sinal: sinal))
                    } else {
                        output.append(UnsafeBufferPointer(start: parameterSetPointer, count: parameterSetSize))
                    }
                }
            }
        }

        let lengthHeaderSize = 4
        dataPointer.withMemoryRebound(to: UInt8.self, capacity: totalLength) { bytes in
            var offset = 0
            while offset + lengthHeaderSize <= totalLength {
                let nalLength =
                    (Int(bytes[offset]) << 24) | (Int(bytes[offset + 1]) << 16)
                    | (Int(bytes[offset + 2]) << 8) | Int(bytes[offset + 3])
                offset += lengthHeaderSize
                guard nalLength >= 0, offset + nalLength <= totalLength else { break }
                output.append(contentsOf: [0, 0, 0, 1])
                output.append(UnsafeBufferPointer(start: bytes + offset, count: nalLength))
                offset += nalLength
            }
        }

        return output
    }
}
