import XCTest
import QuallMonitorKit
import QuallIdiomaKit

final class TestesDosMonitores: XCTestCase {
    func testOitoMonitoresIndependentesESomenteUmaEspera() {
        var state = ControleDeMonitores()
        var indices = Set<Int>()
        for id in 1...8 {
            XCTAssertTrue(state.abrirEspera(id: id))
            XCTAssertFalse(state.abrirEspera(id: 100))
            XCTAssertEqual(state.pareou(id: id, par: .init(id: "peer-\(id)", nome: "Display \(id)")), [])
            let index = state.preparar(id: id, agora: Double(id))!
            XCTAssertTrue(indices.insert(index).inserted)
            state.iniciou(id: id, mensagem: .init("Conectado"))
        }
        XCTAssertEqual(state.conectados.count, 8)
        XCTAssertFalse(state.abrirEspera(id: 9))
        state.encerrar(id: 4)
        XCTAssertFalse(state.podeAbrirEspera, "Closing helper still occupies its slot")
        state.remover(id: 4)
        XCTAssertTrue(state.abrirEspera(id: 9))
        XCTAssertEqual(state.conectados.count, 7)
        XCTAssertTrue(state.conectados.contains { $0.id == 1 && $0.indice == 0 })
    }
    func testReconexaoEsperaDesmonteEReutilizaIdentidade() {
        var state = ControleDeMonitores()
        XCTAssertTrue(state.abrirEspera(id: 1))
        _ = state.pareou(id: 1, par: .init(id: "tablet", nome: "Tablet"))
        let original = state.preparar(id: 1, agora: 1)
        state.iniciou(id: 1, mensagem: .init())
        XCTAssertTrue(state.abrirEspera(id: 2))
        XCTAssertEqual(state.pareou(id: 2, par: .init(id: "tablet", nome: "Tablet")), [1])
        XCTAssertNil(state.preparar(id: 2, agora: 2))
        state.remover(id: 1)
        XCTAssertEqual(state.preparar(id: 2, agora: 3), original)
        XCTAssertNil(state.preparar(id: 2, agora: 4), "Do not start a second helper for the same session")
    }
    func testSaidaNaoConfirmadaReservaIdentidadeEVaga() {
        var state = ControleDeMonitores()
        XCTAssertTrue(state.abrirEspera(id: 1))
        _ = state.pareou(id: 1, par: .init(id: "tablet", nome: "Tablet"))
        let original = state.preparar(id: 1, agora: 1)!
        state.encerrar(id: 1)
        state.remover(id: 1, monitorLiberado: false)
        XCTAssertEqual(state.indicesEmQuarentena, [original])
        XCTAssertTrue(state.novaRodada(), "A new round must not clear unconfirmed identities")
        for id in 2...8 {
            XCTAssertTrue(state.abrirEspera(id: id))
            _ = state.pareou(id: id, par: .init(id: id == 2 ? "tablet" : "peer-\(id)", nome: "Device"))
            XCTAssertNotEqual(state.preparar(id: id, agora: Double(id)), original)
        }
        XCTAssertFalse(state.abrirEspera(id: 9), "Seven sessions and an unconfirmed display fill eight slots")
        state.remover(id: 8, monitorLiberado: true)
        XCTAssertTrue(state.abrirEspera(id: 9))
        state.encerrarTodos()
        for id in Array(state.monitores.keys) { state.remover(id: id, monitorLiberado: false) }
        XCTAssertTrue(state.novaRodada())
        XCTAssertEqual(state.indicesEmQuarentena.count, 7)
        XCTAssertTrue(state.abrirEspera(id: 10))
        _ = state.pareou(id: 10, par: .init(id: "last", nome: "Device"))
        _ = state.preparar(id: 10, agora: 10)
        state.remover(id: 10, monitorLiberado: false)
        XCTAssertEqual(state.indicesEmQuarentena.count, 8)
        XCTAssertFalse(state.podeAbrirEspera)
    }
    func testParadaAntesDeParearNaoCriaMonitor() {
        var state = ControleDeMonitores()
        XCTAssertTrue(state.abrirEspera(id: 1))
        state.encerrar(id: 1)
        XCTAssertNil(state.pareou(id: 1, par: .init(id: "late", nome: "Late")))
        XCTAssertNil(state.preparar(id: 1, agora: 1))
        XCTAssertTrue(state.indices.entradas.isEmpty)
    }
    func testIdiomaDeMensagemAtivaMudaSemPerderDados() {
        let message = MensagemDoMonitor("Exibindo a tela de %@", "Tablet")
        XCTAssertEqual(message.texto(em: .pt), "Exibindo a tela de Tablet")
        XCTAssertEqual(message.texto(em: .en), "Displaying Tablet’s screen")
        XCTAssertFalse(Traducoes.ingles.isEmpty, "Monitor must load its own resource bundle")
    }
    func testPararTodosNaoAbreEsperaEnquantoAuxiliaresSaem() {
        var state = ControleDeMonitores()
        for id in 1...3 {
            _ = state.abrirEspera(id: id)
            _ = state.pareou(id: id, par: .init(id: "peer-\(id)", nome: "Device"))
            _ = state.preparar(id: id, agora: 1)
        }
        _ = state.abrirEspera(id: 4)
        state.encerrarTodos()
        for id in [4, 2, 1, 3] {
            state.remover(id: id)
            XCTAssertFalse(state.podeAbrirEspera)
            XCTAssertNil(state.preparar(id: 3, agora: 2))
        }
        XCTAssertTrue(state.novaRodada())
        XCTAssertTrue(state.abrirEspera(id: 5))
    }
    func testTodasAsMensagensDaCascaTemTraducao() throws {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let expression = try NSRegularExpression(pattern: #"(?:T|MensagemDoMonitor|\.init)\(\"([^\"\n]*)\""#)
        var keys = Set<String>()
        for file in ["Aplicativo.swift", "Sessoes.swift"] {
            let source = try String(contentsOf: root.appendingPathComponent("Sources/QuallMonitorApp/" + file))
            for match in expression.matches(in: source, range: NSRange(source.startIndex..., in: source)) {
                if let range = Range(match.range(at: 1), in: source) { keys.insert(String(source[range])) }
            }
        }
        // These keys are selected by an enum or a conditional rather than literal T calls.
        keys.formUnion(["Estender tela", "Exibir", "Parar", "Parar todos", "Conecte o outro aparelho",
            "Conecte mais um aparelho", "Encerrando…", "Preparando o monitor…",
            "O PIN não foi aceito. Inicie novamente para gerar outro PIN.",
            "Não foi possível conectar. Confira a rede e tente iniciar novamente.",
            "O acesso à rede local está bloqueado. Permita o Quall Monitor em Ajustes do Sistema › Privacidade e Segurança › Rede Local.",
            "Não consegui iniciar a descoberta na rede local. Confira a conexão e tente novamente."])
        for key in keys where !key.isEmpty {
            XCTAssertNotNil(Traducoes.ingles[key], "Missing English translation: \(key)")
        }
    }
}
