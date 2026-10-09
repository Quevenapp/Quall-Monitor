import XCTest
import QuallMonitorKit

final class TestesDaRedeLocal: XCTestCase {
    func testSomentePolicyDeniedIndicaBloqueioDePrivacidade() {
        XCTAssertEqual(EstadoDaRedeLocal.deBonjour(codigoDNS: -65570), .bloqueado)
        XCTAssertEqual(EstadoDaRedeLocal.deBonjour(codigoDNS: -65570, falhou: true), .bloqueado)
        XCTAssertEqual(EstadoDaRedeLocal.deBonjour(codigoDNS: -65563), .aguardando)
        XCTAssertEqual(EstadoDaRedeLocal.deBonjour(codigoDNS: -65563, falhou: true), .falha)
        XCTAssertEqual(EstadoDaRedeLocal.deBonjour(), .aguardando)
        XCTAssertEqual(EstadoDaRedeLocal.deBonjour(pronto: true), .navegando)
    }
}
