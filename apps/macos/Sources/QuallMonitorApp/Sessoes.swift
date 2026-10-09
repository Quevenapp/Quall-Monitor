import CQuall
import CoreGraphics
import Foundation
import QuallCaptureKit
import QuallNetKit
import QuallReceptorKit
import QuallIdiomaKit
import QuallMonitorKit

protocol Sessao: AnyObject {
    func parar()
}

/// Bridge only on dedicated session threads. The main thread never waits for capture/network.
private final class ResultadoAssincrono<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<T, Error>?

    func concluir(_ value: Result<T, Error>) {
        lock.withLock { result = value }
    }

    func obter() throws -> T {
        guard let value = lock.withLock({ result }) else {
            preconditionFailure("A operação assíncrona terminou sem resultado")
        }
        return try value.get()
    }
}

private func esperar<T>(_ operation: @escaping () async throws -> T) throws -> T {
    let semaphore = DispatchSemaphore(value: 0)
    let result = ResultadoAssincrono<T>()
    Task.detached {
        do { result.concluir(.success(try await operation())) }
        catch { result.concluir(.failure(error)) }
        semaphore.signal()
    }
    semaphore.wait()
    return try result.obter()
}

final class SessaoEmissora: Sessao {
    let id: Int
    let porta: UInt16
    let pin: String
    let escala: ModoDoMonitorVirtual.Escala
    let fps: Int
    var aoParear: ((ParDoMonitor) -> Void)?
    var aoAnunciar: ((String) -> Void)?
    var aoEstado: ((MensagemDoMonitor) -> Void)?
    var aoTerminar: ((MensagemDoMonitor, Bool) -> Void)?
    private let lock = NSLock()
    private var stopping = false
    private var captureFailure = MensagemDoMonitor()
    private var indiceDoMonitor: Int?
    private let monitorPreparado = DispatchSemaphore(value: 0)
    private let cancelador = Cancelador()
    private let nucleo = NucleoDeRede()
    private let anunciante = Anunciante()
    private var transmissao: TransmissaoAoVivo?

    init(id: Int, porta: UInt16, pin: String, escala: ModoDoMonitorVirtual.Escala, fps: Int) {
        self.id = id; self.porta = porta; self.pin = pin; self.escala = escala; self.fps = fps
    }
    func comecar() {
        let worker = Thread { [self] in correr() }
        worker.name = "quall.monitor.emissor.\(id)"
        worker.qualityOfService = .userInitiated
        worker.start()
    }
    func parar() {
        lock.withLock { stopping = true }
        cancelador.cancelar()
        monitorPreparado.signal()
    }
    /// Issued on main only after older sessions of this receiver have finished their teardown.
    func preparar(indice: Int) {
        lock.withLock { if !stopping { indiceDoMonitor = indice } }
        monitorPreparado.signal()
    }
    private var parado: Bool { lock.withLock { stopping } }
    private func estado(_ text: MensagemDoMonitor) {
        DispatchQueue.main.async { [self] in aoEstado?(text) }
    }
    private func correr() {
        var motivo = MensagemDoMonitor()
        defer {
            anunciante.parar()
            let monitorLiberado = transmissao.map { transmission in
                (try? esperar { await transmission.parar() }) ?? false
            } ?? true
            nucleo.encerrar()
            Registro.linha("emissor[#\(id)]: sessão desmontada; rede liberada; monitorLiberado=\(monitorLiberado)")
            let final = motivo.chave.isEmpty ? lock.withLock { captureFailure } : motivo
            DispatchQueue.main.async { [self] in aoTerminar?(final, monitorLiberado) }
        }
        let peers = Identidade.pares()
        guard !parado else { return }
        if anunciante.comecar(deviceId: Identidade.deviceId, nome: Identidade.nome, porta: porta,
                              emiteTela: true, emiteCamera: false) {
            let label = anunciante.rotulo
            DispatchQueue.main.async { [self] in aoAnunciar?(label) }
        }
        guard nucleo.hospedar(pin: pin, porta: porta, deviceId: Identidade.deviceId,
                             nome: Identidade.nome,
                             tracksPedidas: [.init(tipo: QUALL_TRACK_KIND_SCREEN, rotulo: T("Quall Monitor · tela estendida"))],
                             paresConhecidos: peers.isEmpty ? nil : peers, prazoMs: 300_000,
                             cancelador: cancelador) else {
            if !parado {
                switch nucleo.ultimoStatusDeFalha {
                case QUALL_STATUS_WRONG_PIN, QUALL_STATUS_PAIRING:
                    motivo = MensagemDoMonitor("O pareamento não terminou. Inicie novamente para gerar outro PIN.")
                default:
                    motivo = MensagemDoMonitor("Não foi possível conectar. Confira a rede e tente iniciar novamente.")
                }
                Registro.linha("emissor: hospedagem falhou status=\(nucleo.ultimoStatusDeFalha.rawValue)")
            }
            return
        }
        anunciante.parar()
        Identidade.guardarPares(nucleo.paresParaGuardar(somandoA: peers.isEmpty ? nil : peers))
        guard !parado else { return }
        let peer = Self.par(nucleo.parJson())
        // Announcement stop completed above. The next waiting slot may now advertise the same
        // identity without an old mDNS goodbye deleting its new endpoint.
        DispatchQueue.main.async { [self] in aoParear?(peer) }
        while monitorPreparado.wait(timeout: .now() + 0.1) != .success {
            if parado { return }
        }
        guard !parado, let index = lock.withLock({ indiceDoMonitor }) else { return }
        let hz = ModoDoMonitorVirtual.hertzDoMonitor(paraFps: fps)
        let mode = peer.tela.flatMap {
            ModoDoMonitorVirtual.paraTela(larguraPx: $0.largura, alturaPx: $0.altura, hertz: hz, fps: fps, escala: escala)
        } ?? ModoDoMonitorVirtual.tabletDaBancada(escala: escala, fps: fps, hertz: hz)
        let transmission = TransmissaoAoVivo(
            fonte: .telaEstendida(mode), fps: Int32(fps),
            teto: { width, height, rate in
                if let t = NucleoDeRede.teto(largura: width, altura: height, fps: rate,
                                            alvoMaxFs: 36_864, alvoFps: rate) {
                    return TetoDoEmissor.Aplicado(
                        saida: .init(largura: t.largura, altura: t.altura, fps: t.fps,
                                     reduziuTamanho: t.reduziuTamanho, reduziuFps: t.reduziuFps,
                                     macroblocos: t.macroblocos, exigeRecorte: t.exigeRecorte,
                                     tetoDeTaxaBps: t.tetoDeTaxaBps),
                        levelIdc: t.levelIdc, maxFS: t.maxFS, origem: "núcleo")
                }
                return .daCopiaLocal(largura: width, altura: height, fps: rate)
            },
            nomeDoMonitor: "Quall Monitor — \(peer.nome)", indiceDoMonitor: index,
            sumidouro: { [nucleo] bytes, timestamp, idr in
                bytes.withUnsafeBytes { nucleo.enviar(annexb: $0, timestampUs: timestamp, idr: idr) == QUALL_STATUS_OK }
            })
        transmissao = transmission
        transmission.aoRegistrarDiagnostico = Registro.linha
        transmission.aoPararSozinho = { [weak self] _ in
            self?.lock.withLock { self?.captureFailure = MensagemDoMonitor("A captura do monitor parou. Inicie novamente para reconectar.") }
            self?.parar()
        }
        do {
            let size = try esperar { try await transmission.iniciar() }
            guard !parado else { return }
            transmission.pedirIDR()
            estado(MensagemDoMonitor("Monitor estendido em %@ · %@ × %@ · %@ fps", peer.nome,
                                     String(size.largura), String(size.altura), String(transmission.fpsDeSaida)))
            Registro.linha("emissor[#\(id)]: monitor ativo indice=\(index) \(size.largura)x\(size.altura) fps=\(transmission.fpsDeSaida)")
        } catch {
            if !parado { motivo = MensagemDoMonitor("Não consegui criar ou capturar o monitor: %@", String(describing: error)) }
            return
        }
        while !parado {
            let event = nucleo.proximoEvento()
            if event == QUALL_SESSION_EVENT_DISCONNECTED || event == QUALL_SESSION_EVENT_FAILED {
                motivo = MensagemDoMonitor("O outro aparelho desconectou. Inicie novamente para reconectar.")
                return
            }
            if nucleo.precisaDeIDR() { transmission.pedirIDR() }
            Thread.sleep(forTimeInterval: 0.02)
        }
    }
    private static func par(_ json: String) -> ParDoMonitor {
        let d = json.data(using: .utf8).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
        let screen = d["screen"] as? [String: Any]
        let width = (screen?["width_px"] as? NSNumber)?.intValue ?? 0
        let height = (screen?["height_px"] as? NSNumber)?.intValue ?? 0
        return ParDoMonitor(id: d["device_id"] as? String ?? "", nome: d["display_name"] as? String ?? T("outro aparelho"),
                            tela: width > 0 && height > 0 ? (width, height) : nil)
    }
}

final class SessaoReceptora: Sessao {
    let endereco: String
    let pin: String
    let tela: (largura: UInt32, altura: UInt32)
    let exibidor: Exibidor
    var aoEstado: ((MensagemDoMonitor) -> Void)?
    var aoTerminar: ((MensagemDoMonitor) -> Void)?
    private let nucleo = NucleoReceptor()
    private let lock = NSLock()
    private var waitingIdr = true
    private var lastFrame: UInt64 = Medidas.agoraUs()
    private var hidden: UInt64 = 0

    init(endereco: String, pin: String, tela: (UInt32, UInt32), exibidor: Exibidor) {
        self.endereco = endereco; self.pin = pin; self.tela = tela; self.exibidor = exibidor
    }
    func parar() { nucleo.parar() }
    func comecar() {
        let worker = Thread { [self] in correr() }
        worker.name = "quall.monitor.receptor"
        worker.qualityOfService = .userInitiated
        worker.stackSize = 1 << 20
        worker.start()
    }
    private func correr() {
        var reason = MensagemDoMonitor()
        let decoder = DecodificadorH264 { [exibidor] image, _ in exibidor.oferecer(image) }
        defer {
            let closed = nucleo.encerrar() // barrier removes callbacks before decoder/session teardown
            if closed.caixaLiberada {
                decoder.fechar()
            } else if closed.desregistro != nil {
                // The nucleus deliberately retained its callback box after a failed barrier.
                // That box owns this decoder; closing it now would invalidate an in-flight call.
                Registro.linha("receptor: barreira de callbacks não confirmada; decoder preservado")
            }
            Registro.linha("receptor: desmontado barreira=\(String(describing: closed.desregistro)) fechamento=\(String(describing: closed.fechamento))")
            let final = reason
            DispatchQueue.main.async { [self] in
                exibidor.camada.flushAndRemoveImage()
                aoTerminar?(final)
            }
        }
        let peers = Identidade.pares()
        guard nucleo.conectar(endereco: endereco, pin: pin.isEmpty ? nil : pin,
                             deviceId: Identidade.deviceId, nome: Identidade.nome,
                             paresConhecidos: peers.isEmpty ? nil : peers, prazoMs: 30_000, tela: tela) else {
            if !nucleo.parou {
                switch nucleo.ultimoStatus {
                case QUALL_STATUS_NEEDS_PIN: reason = MensagemDoMonitor("Digite o PIN de seis dígitos mostrado no outro aparelho.")
                case QUALL_STATUS_WRONG_PIN: reason = MensagemDoMonitor("PIN incorreto. Confira o PIN no outro aparelho.")
                default: reason = MensagemDoMonitor("Não consegui conectar. Confira endereço, porta e rede local.")
                }
                Registro.linha("receptor: conexão falhou status=\(nucleo.ultimoStatus.rawValue)")
            }
            return
        }
        Identidade.guardarPares(nucleo.paresConhecidos(somandoA: peers.isEmpty ? nil : peers))
        guard let track = nucleo.esperarTrackDeVideo(prazoMs: 10_000), track.tipo == QUALL_TRACK_KIND_SCREEN else {
            if !nucleo.parou { reason = MensagemDoMonitor("O aparelho não ofereceu uma tela. Use Estender tela ou Espelhar uma tela no Quall Studio.") }
            return
        }
        guard !nucleo.parou else { return }
        // The connection/track handshake may take longer than the media silence deadline.
        lock.withLock { lastFrame = Medidas.agoraUs() }
        let status = nucleo.ouvirQuadros { [self, decoder] bytes, timestamp, idr in
            let keep = lock.withLock { () -> Bool in
                lastFrame = Medidas.agoraUs()
                if idr { waitingIdr = false }
                if waitingIdr { hidden &+= 1; return false }
                return true
            }
            if keep { decoder.alimentar(annexb: bytes, timestampUs: timestamp, idr: idr) }
        }
        guard status == QUALL_STATUS_OK else { reason = MensagemDoMonitor("Não consegui iniciar a recepção do vídeo."); return }
        let peer = nucleo.nomeDoPar
        DispatchQueue.main.async { [self] in aoEstado?(MensagemDoMonitor("Exibindo a tela de %@", peer)) }
        var dropped: UInt64 = 0
        var lastRequest: UInt64 = 0
        var lastLink: UInt64 = 0
        var lastDecodeFailures: UInt64 = 0
        var link = JanelaDoEnlace()
        while !nucleo.parou {
            let event = nucleo.evento(prazoMs: 0)
            if event == QUALL_SESSION_EVENT_DISCONNECTED || event == QUALL_SESSION_EVENT_FAILED {
                reason = MensagemDoMonitor("O emissor desconectou. Conecte novamente quando ele estiver disponível.")
                break
            }
            let now = Medidas.agoraUs()
            let lost = nucleo.quadrosDescartados() // never read core stats inside its frame callback
            let decoding = decoder.instantaneo()
            // An IDR can arrive while its parameters or VT session still fail to initialize.
            // Every failure path must re-arm recovery rather than wait for the 30-second GOP.
            let failures = decoding.recusados &+ decoding.semParametros
                &+ decoding.falhasDeDescricao &+ decoding.falhasDeSessao
            if lost > dropped || failures > lastDecodeFailures { lock.withLock { waitingIdr = true } }
            dropped = lost; lastDecodeFailures = failures
            let needs = lock.withLock { waitingIdr }
            if needs, Medidas.delta(now, lastRequest) >= 500_000 {
                _ = nucleo.pedirIdr()
                lastRequest = now
            }
            if Medidas.delta(now, lock.withLock { lastFrame }) > 15_000_000 {
                reason = MensagemDoMonitor("O vídeo parou por 15 segundos. Confira a conexão e tente novamente.")
                break
            }
            if Medidas.delta(now, lastLink) >= 500_000 {
                lastLink = now
                if let c = JanelaDoEnlace.acumulados(nucleo.contadores()),
                   let sample = link.fechar(agoraUs: now, periodoMs: 500, vistosAcum: c.vistos,
                                             perdidosAcum: c.perdidos, suspeitosAcum: 0, idrsQuebradosAcum: c.idrsQuebrados) {
                    _ = nucleo.relatarEnlace(ms: sample.ms, pacotes: sample.pacotes, perdidos: sample.perdidos,
                                         suspeitos: 0, idrsQuebrados: sample.idrsQuebrados)
                }
            }
            Thread.sleep(forTimeInterval: 0.02)
        }
        Registro.linha("receptor: recebidos=\(decoder.instantaneo().recebidos) decodificados=\(decoder.instantaneo().decodificados) retidos=\(lock.withLock { hidden })")
    }
}
