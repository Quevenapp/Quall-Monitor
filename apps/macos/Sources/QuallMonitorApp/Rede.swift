import CQuall
import Foundation

enum Rede {
    /// Prefer Monitor's port, then an ephemeral port. A bind race is reported by the host.
    static func portaLivre(preferida: UInt16 = 7878) -> UInt16 {
        for requested in [preferida, 0] {
            let descriptor = socket(AF_INET, SOCK_STREAM, 0)
            guard descriptor >= 0 else { continue }
            defer { close(descriptor) }
            var address = sockaddr_in()
            address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
            address.sin_family = sa_family_t(AF_INET)
            address.sin_port = requested.bigEndian
            address.sin_addr.s_addr = INADDR_ANY.bigEndian
            let result = withUnsafePointer(to: &address) { p in
                p.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
            guard result == 0 else { continue }
            var length = socklen_t(MemoryLayout<sockaddr_in>.size)
            let read = withUnsafeMutablePointer(to: &address) { p in
                p.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(descriptor, $0, &length) }
            }
            if read == 0 { return UInt16(bigEndian: address.sin_port) }
        }
        return 0
    }
    static func enderecos(porta: UInt16) -> [String] {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0 else { return [] }
        defer { freeifaddrs(head) }
        var current = head
        var found: [String] = []
        while let item = current {
            defer { current = item.pointee.ifa_next }
            guard let address = item.pointee.ifa_addr else { continue }
            let flags = Int32(item.pointee.ifa_flags)
            let name = String(cString: item.pointee.ifa_name)
            guard flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0,
                  name.hasPrefix("en"), address.pointee.sa_family == UInt8(AF_INET)
                    || address.pointee.sa_family == UInt8(AF_INET6) else { continue }
            var buffer = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            guard getnameinfo(address, socklen_t(address.pointee.sa_len), &buffer,
                              socklen_t(buffer.count), nil, 0, NI_NUMERICHOST) == 0 else { continue }
            let ip = String(cString: buffer)
            guard !ip.hasPrefix("169.254."), !ip.hasPrefix("fe80:"), !ip.contains("%") else { continue }
            found.append(ip.contains(":") ? "[\(ip)]:\(porta)" : "\(ip):\(porta)")
        }
        return Array(Set(found)).sorted { a, b in
            if a.hasPrefix("[") != b.hasPrefix("[") { return !a.hasPrefix("[") }
            return a < b
        }
    }
}

struct Aparelho: Identifiable, Equatable {
    let id: String
    let nome: String
    let endereco: String
}

/// One dedicated thread owns browser collect/read/free for its lifetime.
final class Descoberta {
    private let lock = NSLock()
    private var stopping = false
    var aoAtualizar: (([Aparelho]) -> Void)?
    func comecar() {
        let worker = Thread { [self] in
            guard let browser = quall_browser_start() else { return }
            defer { quall_browser_stop(browser) }
            var previous: [Aparelho] = []
            while !lock.withLock({ stopping }) {
                _ = quall_browser_collect(browser, 500)
                let json = lerTexto { p, size in Int(quall_browser_devices_json(browser, p, size)) }
                guard let data = json.data(using: .utf8),
                      let list = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else { continue }
                let devices = list.compactMap { d -> Aparelho? in
                    guard let id = d["device_id"] as? String, id != Identidade.deviceId,
                          let endpoint = d["endpoint"] as? String,
                          let capabilities = d["capabilities"] as? [String: Any],
                          capabilities["screen_source"] as? Bool == true else { return nil }
                    return Aparelho(id: id, nome: d["display_name"] as? String ?? "Quall", endereco: endpoint)
                }.sorted { $0.nome < $1.nome }
                if devices != previous {
                    previous = devices
                    DispatchQueue.main.async { [weak self] in self?.aoAtualizar?(devices) }
                }
            }
        }
        worker.name = "quall.monitor.descoberta"
        worker.start()
    }
    func parar() { lock.withLock { stopping = true } }
    deinit { parar() }
}
