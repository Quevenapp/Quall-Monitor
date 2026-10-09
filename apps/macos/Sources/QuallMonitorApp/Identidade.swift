import CQuall
import Foundation

/// Monitor owns its identity and pairings; Studio's files remain independent.
enum Identidade {
    static let bundleId = "br.com.queven.quall.monitor"
    private static let fila = DispatchQueue(label: "quall.monitor.identidade")
    static let pasta: URL = {
        if let isolated = ProcessInfo.processInfo.environment["QUALL_MONITOR_DADOS"], isolated.hasPrefix("/") {
            return URL(fileURLWithPath: isolated, isDirectory: true)
        }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Quall Monitor", isDirectory: true)
    }()
    static let nome = "\(Host.current().localizedName ?? "Mac") · Quall Monitor"
    static let deviceId: String = fila.sync {
        try? FileManager.default.createDirectory(at: pasta, withIntermediateDirectories: true,
                                                  attributes: [.posixPermissions: 0o700])
        let file = pasta.appendingPathComponent("aparelho.json")
        if let data = try? Data(contentsOf: file),
           let object = try? JSONSerialization.jsonObject(with: data) as? [String: String],
           let id = object["device_id"], !id.isEmpty { return id }
        let id = "monitor-mac-" + UUID().uuidString.lowercased()
        if let data = try? JSONSerialization.data(withJSONObject: ["device_id": id]) {
            try? data.write(to: file, options: .atomic)
            try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
        }
        return id
    }
    static func pares() -> String {
        fila.sync { (try? String(contentsOf: pasta.appendingPathComponent("pares.json"), encoding: .utf8)) ?? "" }
    }
    static func guardarPares(_ novos: String) {
        guard !novos.isEmpty else { return }
        fila.sync {
            let file = pasta.appendingPathComponent("pares.json")
            let old = (try? String(contentsOf: file, encoding: .utf8)) ?? ""
            let merged = old.withCString { a in novos.withCString { b in
                lerTexto { buffer, count in Int(quall_known_peers_merge(a, b, buffer, count)) }
            }}
            guard !merged.isEmpty else { Registro.linha("pares: falha ao fundir; arquivo preservado"); return }
            try? merged.data(using: .utf8)?.write(to: file, options: .atomic)
            try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
        }
    }
}

/// C JSON calls may grow between their size query and read.
func lerTexto(_ call: (UnsafeMutablePointer<CChar>?, UInt) -> Int) -> String {
    var count = call(nil, 0)
    for _ in 0..<8 {
        guard count > 0, count <= 16 * 1024 * 1024 else { return "" }
        var buffer = [CChar](repeating: 0, count: count)
        let written = buffer.withUnsafeMutableBufferPointer { call($0.baseAddress, UInt($0.count)) }
        if written > 0, written <= count { return String(cString: buffer) }
        if written <= 0 { return "" }
        count = written
    }
    return ""
}

enum Registro {
    private static let lock = NSLock()
    static let arquivo: URL = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("Logs/Quall Monitor/quall-monitor.log")
    static func linha(_ text: String) {
        let safe = text.replacingOccurrences(of: "\n", with: " ").replacingOccurrences(of: "\r", with: " ")
        lock.lock(); defer { lock.unlock() }
        try? FileManager.default.createDirectory(at: arquivo.deletingLastPathComponent(), withIntermediateDirectories: true)
        if !FileManager.default.fileExists(atPath: arquivo.path) {
            _ = FileManager.default.createFile(atPath: arquivo.path, contents: nil, attributes: [.posixPermissions: 0o600])
        }
        guard let file = try? FileHandle(forWritingTo: arquivo) else { return }
        defer { try? file.close() }
        _ = try? file.seekToEnd()
        try? file.write(contentsOf: "\(ISO8601DateFormatter().string(from: Date())) \(safe)\n".data(using: .utf8)!)
    }
}
