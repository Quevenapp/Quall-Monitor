import CoreGraphics
import Foundation
import QuallIdiomaKit

/// Um processo auxiliar por monitor isola o ciclo de vida da API CGVirtualDisplay.
/// `subir` acontece depois do pareamento; `soltar` acontece depois de parar a captura.
/// Fechar o stdin solicita o encerramento. A saída do processo e a ausência do display são
/// verificadas separadamente; um prazo vencido não confirma a liberação da identidade.
public final class MonitorVirtualAuxiliar: @unchecked Sendable {
    public static let nomeDoExecutavel = "quall-monitor-display"

    public enum Falha: Error, CustomStringConvertible {
        case auxiliarAusente
        case naoSubiu(String)
        case recusou(String)
        case respostaIlegivel(String)

        public var description: String {
            switch self {
            case .auxiliarAusente:
                return T("O auxiliar %@ não está ao lado do app.", MonitorVirtualAuxiliar.nomeDoExecutavel)
            case .naoSubiu(let motivo): return T("O auxiliar do monitor virtual não subiu: %@", motivo)
            case .recusou(let motivo): return motivo
            case .respostaIlegivel(let linha): return T("O auxiliar do monitor virtual respondeu algo ilegível: %@", linha)
            }
        }
    }

    public let modo: ModoDoMonitorVirtual
    public let displayID: CGDirectDisplayID
    /// O relato do auxiliar (quanto custou cada etapa, o modo com que o monitor nasceu) mais o
    /// tempo de subir o processo.
    public let relato: String

    private let processo: Process
    private let entrada: Pipe
    private let trava = NSLock()
    private var solto = false
    private var resultadoDaSaida: ResultadoDaSaida?

    public struct ResultadoDaSaida: Sendable {
        public let processoSaiu: Bool
        public let monitorSumiu: Bool
        public let relato: String
        public var confirmado: Bool { processoSaiu && monitorSumiu }
    }

    private init(modo: ModoDoMonitorVirtual, displayID: CGDirectDisplayID, relato: String,
                 processo: Process, entrada: Pipe) {
        self.modo = modo
        self.displayID = displayID
        self.relato = relato
        self.processo = processo
        self.entrada = entrada
    }

    /// Rede de segurança, não caminho normal: quem transmite chama `soltar()`. Se este objeto
    /// morrer sem isso, o auxiliar é encerrado sem esperar.
    deinit {
        trava.lock()
        let jaSolto = solto
        trava.unlock()
        if !jaSolto {
            try? entrada.fileHandleForWriting.close()
            if processo.isRunning { processo.terminate() }
        }
    }

    /// Onde está o executável do auxiliar: dentro do `.app` (`Contents/MacOS`, ao lado do
    /// `quall-monitor-app`) ou, fora dele, ao lado do executável que está rodando — que é o caso de
    /// `.build/release/` para as sondas de bancada.
    public static func localizar() -> URL? {
        if let noBundle = Bundle.main.url(forAuxiliaryExecutable: nomeDoExecutavel),
           FileManager.default.isExecutableFile(atPath: noBundle.path) {
            return noBundle
        }
        let aqui = Bundle.main.executableURL
            ?? URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
        let aoLado = aqui.deletingLastPathComponent().appendingPathComponent(nomeDoExecutavel)
        return FileManager.default.isExecutableFile(atPath: aoLado.path) ? aoLado : nil
    }

    /// A tela estendida pode ser oferecida: a API existe **e** o auxiliar está onde deveria.
    public static var disponivel: Bool { MonitorVirtual.disponivel && localizar() != nil }

    /// Sobe o auxiliar e espera sua resposta. O prazo solicita encerramento com SIGTERM;
    /// uma falha antes da resposta não permite confirmar qual display chegou a ser criado.
    /// `indice` escolhe a identidade do monitor (ver `MonitorVirtual.serie`). O 0 — o de uma sessão
    /// só — sobe o auxiliar com os mesmos argumentos de sempre.
    public static func subir(modo: ModoDoMonitorVirtual, nome: String, indice: Int = 0,
                             prazo: TimeInterval = 8) async throws -> MonitorVirtualAuxiliar {
        guard let executavel = localizar() else { throw Falha.auxiliarAusente }

        let processo = Process()
        processo.executableURL = executavel
        processo.arguments = [
            "--largura=\(modo.larguraEmPixels)", "--altura=\(modo.alturaEmPixels)",
            "--hertz=\(modo.hertz)", "--escala=\(modo.escala.rawValue)", "--nome=\(nome)",
        ]
        if indice != 0 { processo.arguments?.append("--indice=\(indice)") }
        // O 2x reduzido: o auxiliar precisa saber, porque ali o 1x lembrado nunca é o que se quer.
        if modo.reduzido { processo.arguments?.append("--saida=\(modo.larguraDaSaida)x\(modo.alturaDaSaida)") }
        let entrada = Pipe()
        let saida = Pipe()
        processo.standardInput = entrada
        processo.standardOutput = saida
        processo.standardError = FileHandle.standardError

        let t0 = DispatchTime.now()
        do {
            try processo.run()
        } catch {
            throw Falha.naoSubiu("\(error)")
        }

        // O prazo solicita a saída do auxiliar para que a leitura de stdout termine.
        let vigiaDoPrazo = DispatchWorkItem { [processo] in
            if processo.isRunning { processo.terminate() }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + prazo, execute: vigiaDoPrazo)
        let linha = await primeiraLinha(de: saida.fileHandleForReading)
        vigiaDoPrazo.cancel()
        let msSubir = Int((DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1_000_000)

        func abortar(_ falha: Falha) -> Falha {
            try? entrada.fileHandleForWriting.close()
            if processo.isRunning { processo.terminate() }
            return falha
        }

        guard let linha, !linha.isEmpty else {
            throw abortar(.naoSubiu("nenhuma resposta em \(msSubir) ms (prazo \(Int(prazo)) s)"))
        }
        guard let dados = linha.data(using: .utf8),
              let objeto = try? JSONSerialization.jsonObject(with: dados) as? [String: Any] else {
            throw abortar(.respostaIlegivel(linha))
        }
        if let erro = objeto["erro"] as? String {
            throw abortar(.recusou(erro))
        }
        guard let numero = objeto["display_id"] as? Int, numero > 0 else {
            throw abortar(.respostaIlegivel(linha))
        }
        let relatoDoAuxiliar = objeto["relato"] as? String ?? ""
        return MonitorVirtualAuxiliar(
            modo: modo, displayID: CGDirectDisplayID(numero),
            relato: "\(relatoDoAuxiliar) | auxiliar pid \(processo.processIdentifier), resposta em \(msSubir) ms",
            processo: processo, entrada: entrada)
    }

    /// Encerra o auxiliar e espera o monitor sumir. Uma falha na consulta não confirma a saída.
    ///
    /// Fecha o stdin primeiro (o caminho limpo); se o auxiliar não sair em 2 s, `SIGTERM`; se nem
    /// assim, `SIGKILL`. Idempotente.
    @discardableResult
    public func soltar() async -> ResultadoDaSaida {
        let jaEstava = trava.withLock { () -> Bool in
            defer { solto = true }
            return solto
        }
        if jaEstava {
            return trava.withLock {
                resultadoDaSaida ?? ResultadoDaSaida(processoSaiu: false, monitorSumiu: false,
                    relato: "monitor virtual: encerramento ainda sem confirmação")
            }
        }

        let t0 = DispatchTime.now()
        try? entrada.fileHandleForWriting.close()
        var como = "stdin fechado"
        if !(await esperar(prazoMs: 2_000, { !self.processo.isRunning })) {
            processo.terminate()
            como = "SIGTERM"
            if !(await esperar(prazoMs: 1_000, { !self.processo.isRunning })) {
                kill(processo.processIdentifier, SIGKILL)
                como = "SIGKILL"
                _ = await esperar(prazoMs: 1_000, { !self.processo.isRunning })
            }
        }
        let processoSaiu = !processo.isRunning
        let msProcesso = ms(desde: t0)
        let sumiu = await esperar(prazoMs: 2_000, {
            MonitorVirtualAuxiliar.estaOnline(self.displayID) == false
        })
        let msMonitor = ms(desde: t0)
        let resultado = ResultadoDaSaida(processoSaiu: processoSaiu, monitorSumiu: sumiu,
            relato: "monitor virtual: id=\(displayID) \(como) em \(msProcesso) ms; processoSaiu=\(processoSaiu); "
                + (sumiu ? "monitor sumiu em \(msMonitor) ms" : "saída do monitor não confirmada depois de \(msMonitor) ms"))
        trava.withLock { resultadoDaSaida = resultado }
        return resultado
    }

    // MARK: - apoio

    static func estaOnline(_ id: CGDirectDisplayID) -> Bool? {
        var contagem: UInt32 = 0
        guard CGGetOnlineDisplayList(0, nil, &contagem) == .success else { return nil }
        guard contagem > 0 else { return false }
        var lista = [CGDirectDisplayID](repeating: 0, count: Int(contagem))
        guard CGGetOnlineDisplayList(contagem, &lista, &contagem) == .success else { return nil }
        return lista.prefix(Int(contagem)).contains(id)
    }

    /// Lê até o primeiro `\n` (ou até o fim do cano) numa thread própria: a leitura bloqueia, e
    /// bloquear uma thread do pool cooperativo por até `prazo` segundos seria pedir para travar o
    /// resto da cadeia.
    private static func primeiraLinha(de leitor: FileHandle) async -> String? {
        await withCheckedContinuation { continuacao in
            Thread.detachNewThread {
                var acumulado = Data()
                while true {
                    let pedaco = leitor.availableData
                    if pedaco.isEmpty { break }
                    acumulado.append(pedaco)
                    if acumulado.contains(0x0A) { break }
                }
                let texto = String(data: acumulado, encoding: .utf8)?
                    .split(separator: "\n", omittingEmptySubsequences: true).first.map(String.init)
                continuacao.resume(returning: texto)
            }
        }
    }

    private func esperar(prazoMs: Int, _ condicao: @escaping () -> Bool) async -> Bool {
        let t0 = DispatchTime.now()
        while !condicao() {
            if ms(desde: t0) >= prazoMs { return false }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return true
    }

    private func ms(desde t0: DispatchTime) -> Int {
        Int((DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1_000_000)
    }
}
