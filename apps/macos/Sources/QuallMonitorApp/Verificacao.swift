import CQuall
import Foundation
import QuallCaptureKit

/// A LaunchServices proof with no model, browser, capture, display creation, or TCC request.
enum Verificacao {
    static func executarSePedida() {
        guard let argument = CommandLine.arguments.first(where: { $0.hasPrefix("--verificar=") }) else { return }
        let path = String(argument.dropFirst("--verificar=".count))
        guard path.hasPrefix("/") else { exit(2) }
        let helper = MonitorVirtualAuxiliar.localizar()
        let report: [String: Any] = [
            "bundle_id": Bundle.main.bundleIdentifier ?? "",
            "device_id": Identidade.deviceId,
            "data_directory": Identidade.pasta.path,
            "name": Identidade.nome,
            "monitor_product": MonitorVirtual.produto,
            "helper": helper?.lastPathComponent ?? "",
            "protocol": quall_protocol_version(),
            "pid": ProcessInfo.processInfo.processIdentifier
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: URL(fileURLWithPath: path), options: .atomic)
            exit(Bundle.main.bundleIdentifier == Identidade.bundleId && helper != nil && MonitorVirtual.produto == 2 ? 0 : 3)
        } catch { exit(2) }
    }
}
