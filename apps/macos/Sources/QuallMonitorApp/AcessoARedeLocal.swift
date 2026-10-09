import Foundation
import Network
import QuallMonitorKit

/// Browses the service the app actually uses. Network.framework keeps waiting through the
/// first consent alert; only DNS PolicyDenied is classified as a privacy block (Apple TN3179).
final class AcessoARedeLocal {
    private let fila = DispatchQueue(label: "quall.monitor.acesso-rede")
    private var browser: NWBrowser?
    private var geracao = 0
    var aoAtualizar: ((EstadoDaRedeLocal) -> Void)?
    func iniciar(repetir: Bool = false) {
        if browser != nil && !repetir { return }
        parar()
        geracao += 1
        let generation = geracao
        let parameters = NWParameters.tcp
        let current = NWBrowser(for: .bonjour(type: "_quall._tcp", domain: "local."), using: parameters)
        browser = current
        aoAtualizar?(.aguardando)
        current.stateUpdateHandler = { [weak self] state in
            let next: EstadoDaRedeLocal
            switch state {
            case .ready: next = .navegando
            case .waiting(let error), .failed(let error):
                let failed: Bool
                if case .failed = state { failed = true } else { failed = false }
                if case .dns(let code) = error {
                    next = .deBonjour(codigoDNS: code, falhou: failed)
                } else { next = .deBonjour(falhou: failed) }
            default: return
            }
            DispatchQueue.main.async { [weak self] in
                guard let self, self.geracao == generation else { return }
                self.aoAtualizar?(next)
            }
        }
        current.start(queue: fila)
    }
    func parar() {
        geracao += 1
        browser?.stateUpdateHandler = nil
        browser?.cancel()
        browser = nil
    }
}
