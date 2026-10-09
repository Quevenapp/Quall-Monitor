import Foundation

/// Relógio monotônico, pegada de memória e percentis — as três medidas que o laço de recepção
/// precisa e que não têm dono em `QuallCaptureKit` (lá o relógio existe em `MonotonicClock`, mas
/// em nanossegundos e amarrado ao `CMTime` da captura, que este lado não tem).
///
/// `CLOCK_UPTIME_RAW` e não `Date()`: um ajuste de horário no meio de uma corrida produziria
/// latência negativa, que num `UInt64` vira um número gigante. **Não há subtração de inteiro sem
/// sinal sem guarda em lugar nenhum deste arquivo** — é o que [`delta`] existe para garantir.
///
/// Cópia declarada de `apps/ios/Receptor/Comum/Medidas.swift`. Os dois receptores não
/// compartilham alvo de compilação (um é app iOS por XcodeGen, o outro é SwiftPM de macOS), e a
/// alternativa — um pacote Swift para trinta linhas — custaria mais do que resolve. O que amarra
/// as duas cópias é o formato do relato, que é o mesmo texto nos dois.
public enum Medidas {
    public static func agoraUs() -> UInt64 {
        var t = timespec()
        clock_gettime(CLOCK_UPTIME_RAW, &t)
        return UInt64(t.tv_sec) &* 1_000_000 &+ UInt64(t.tv_nsec) / 1_000
    }

    /// `a - b` que nunca estoura. Devolve 0 quando `b` é maior, em vez de 18 quintilhões.
    public static func delta(_ a: UInt64, _ b: UInt64) -> UInt64 { a > b ? a - b : 0 }

    /// `phys_footprint` — o mesmo número que o receptor iOS relata, para que as duas linhas sejam
    /// comparáveis quando alguém puser as corridas lado a lado.
    public static func pegadaDeMemoria() -> String {
        var info = task_vm_info_data_t()
        var contagem = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let estado = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(contagem)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &contagem)
            }
        }
        guard estado == KERN_SUCCESS else { return "?" }
        return String(format: "%.2f MB", Double(info.phys_footprint) / 1_048_576)
    }

    /// Percentis de uma amostra já coletada. Devolve zeros para amostra vazia — nunca `nil` e
    /// nunca um número inventado, porque "não medi" e "medi zero" precisam ser distinguíveis pelo
    /// `n` que sai junto.
    public static func percentis(_ amostra: [UInt64]) -> (n: Int, p50: UInt64, p95: UInt64, max: UInt64) {
        guard !amostra.isEmpty else { return (0, 0, 0, 0) }
        let o = amostra.sorted()
        return (o.count, o[o.count / 2], o[min(o.count - 1, Int(Double(o.count) * 0.95))], o[o.count - 1])
    }
}
