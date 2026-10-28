import CQuall
import Foundation
import QuallReceptorKit

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

/// Sessões salvas do Exibir, por identidade autenticada (`docs/conexao.md` §3), no diretório do
/// Monitor. O endereço é só dica: a retomada procura o aparelho pelo id quando o IP muda.
enum SessoesSalvas {
    static let limite = 8
    private static var arquivo: URL { Identidade.pasta.appendingPathComponent("sessoes.json") }
    static func ler() -> [SessaoSalva] {
        guard let data = try? Data(contentsOf: arquivo) else { return [] }
        return (try? JSONDecoder().decode([SessaoSalva].self, from: data)) ?? []
    }
    /// Só depois de autenticar. A mais recente primeiro; uma linha por aparelho.
    @discardableResult
    static func gravar(_ nova: SessaoSalva) -> [SessaoSalva] {
        var lista = ler().filter { $0.endereco != nova.endereco && (nova.deviceId.isEmpty || $0.deviceId != nova.deviceId) }
        lista.insert(nova, at: 0)
        return escrever(Array(lista.prefix(limite)))
    }
    @discardableResult
    static func esquecer(_ salva: SessaoSalva) -> [SessaoSalva] {
        escrever(ler().filter { $0 != salva })
    }
    private static func escrever(_ lista: [SessaoSalva]) -> [SessaoSalva] {
        try? FileManager.default.createDirectory(at: Identidade.pasta, withIntermediateDirectories: true,
                                                  attributes: [.posixPermissions: 0o700])
        if let data = try? JSONEncoder().encode(lista) {
            try? data.write(to: arquivo, options: .atomic)
            try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: arquivo.path)
        }
        return lista
    }
}
