import Foundation
import CoreMedia

/// Utilitários de relógio monotônico. `DispatchTime.now().uptimeNanoseconds` é baseado em
/// `mach_absolute_time`, que não anda durante sono profundo mas é monotônico e é o que se usa
/// para medir latência dentro do processo (não é hora de parede, não sofre ajuste de NTP).
enum MonotonicClock {
    static func nowNanoseconds() -> UInt64 {
        DispatchTime.now().uptimeNanoseconds
    }

    /// Converte um `CMTime` (o `presentationTimeStamp` que o ScreenCaptureKit atribui, baseado no
    /// host time / mach continuous time — também monotônico) para microssegundos inteiros. Usada
    /// tanto para o timestamp gravado no sidecar quanto como chave de correlação entre a
    /// submissão ao encoder e a saída do pacote codificado.
    static func microseconds(from time: CMTime) -> Int64 {
        guard time.isValid, time.timescale != 0 else { return 0 }
        return Int64((time.seconds * 1_000_000).rounded())
    }
}
