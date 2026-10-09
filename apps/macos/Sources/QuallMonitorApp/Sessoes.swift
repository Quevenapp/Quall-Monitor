import CQuall
import CoreGraphics
import Foundation
import QuallCaptureKit
import QuallNetKit
import QuallReceptorKit

protocol Sessao: AnyObject {
    func parar()
}

/// Bridge only on dedicated session threads. The main thread never waits for capture/network.
private func esperar<T>(_ operation: @escaping () async throws -> T) throws -> T {
    let semaphore = DispatchSemaphore(value: 0)
    var result: Result<T, Error>!
    Task.detached {
        do { result = .success(try await operation()) } catch { result = .failure(error) }
        semaphore.signal()
    }
    semaphore.wait()
    return try result.get()
}

final class SessaoEmissora: Sessao {
    let porta: UInt16
    let pin: String
    let escala: ModoDoMonitorVirtual.Escala
    let fps: Int
    var aoEstado: ((String) -> Void)?
    var aoTerminar: ((String) -> Void)?
    private let lock = NSLock()
    private var stopping = false
    private var captureFailure = ""
    private let cancelador = Cancelador()
    private let nucleo = NucleoDeRede()
    private let anunciante = Anunciante()
    private var transmissao: TransmissaoAoVivo?

    init(porta: UInt16, pin: String, escala: ModoDoMonitorVirtual.Escala, fps: Int) {
        self.porta = porta; self.pin = pin; self.escala = escala; self.fps = fps
    }
    func comecar() {
        let worker = Thread { [self] in correr() }
        worker.name = "quall.monitor.emissor"
        worker.qualityOfService = .userInitiated
        worker.start()
    }
    func parar() {
        lock.withLock { stopping = true }
        cancelador.cancelar()
    }
    private var parado: Bool { lock.withLock { stopping } }
    private func estado(_ text: String) {
        DispatchQueue.main.async { [self] in aoEstado?(text) }
    }
    private func correr() {
        var motivo = ""
        defer {
            anunciante.parar()
            if let transmissao { try? esperar { await transmissao.parar() } }
            nucleo.encerrar()
            Registro.linha("emissor: sessão desmontada; monitor e rede liberados")
            let final = motivo.isEmpty ? lock.withLock { captureFailure } : motivo
            DispatchQueue.main.async { [self] in aoTerminar?(final) }
        }
        let peers = Identidade.pares()
        guard !parado else { return }
        _ = anunciante.comecar(deviceId: Identidade.deviceId, nome: Identidade.nome, porta: porta,
                               emiteTela: true, emiteCamera: false)
        guard nucleo.hospedar(pin: pin, porta: porta, deviceId: Identidade.deviceId,
                             nome: Identidade.nome,
                             tracksPedidas: [.init(tipo: QUALL_TRACK_KIND_SCREEN, rotulo: "Quall Monitor · tela estendida")],
                             paresConhecidos: peers.isEmpty ? nil : peers, prazoMs: 300_000,
                             cancelador: cancelador) else {
            if !parado {
                motivo = nucleo.ultimoStatusDeFalha == QUALL_STATUS_WRONG_PIN
                    ? "O PIN não foi aceito. Inicie novamente para gerar outro PIN."
                    : "Não foi possível conectar. Confira a rede e tente iniciar novamente."
                Registro.linha("emissor: hospedagem falhou status=\(nucleo.ultimoStatusDeFalha.rawValue)")
            }
            return
        }
        anunciante.parar()
        Identidade.guardarPares(nucleo.paresParaGuardar(somandoA: peers.isEmpty ? nil : peers))
        guard !parado else { return }
        let peer = Self.par(nucleo.parJson())
        let hz = ModoDoMonitorVirtual.hertzDoMonitor(paraFps: fps)
        let mode = peer.tela.flatMap {
            ModoDoMonitorVirtual.paraTela(larguraPx: $0.0, alturaPx: $0.1, hertz: hz, fps: fps, escala: escala)
        } ?? ModoDoMonitorVirtual.tabletDaBancada(escala: escala, fps: fps, hertz: hz)
        estado("Preparando o monitor de \(peer.nome)…")
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
            nomeDoMonitor: "Quall Monitor — \(peer.nome)", indiceDoMonitor: Self.indice(para: peer.id),
            sumidouro: { [nucleo] bytes, timestamp, idr in
                bytes.withUnsafeBytes { nucleo.enviar(annexb: $0, timestampUs: timestamp, idr: idr) == QUALL_STATUS_OK }
            })
        transmissao = transmission
        transmission.aoRegistrarDiagnostico = Registro.linha
        transmission.aoPararSozinho = { [weak self] _ in
            self?.lock.withLock { self?.captureFailure = "A captura do monitor parou. Inicie novamente para reconectar." }
            self?.parar()
        }
        do {
            let size = try esperar { try await transmission.iniciar() }
            guard !parado else { return }
            transmission.pedirIDR()
            estado("Monitor estendido em \(peer.nome) · \(size.largura) × \(size.altura) · \(transmission.fpsDeSaida) fps")
            Registro.linha("emissor: monitor ativo \(size.largura)x\(size.altura) fps=\(transmission.fpsDeSaida)")
        } catch {
            if !parado { motivo = "Não consegui criar ou capturar o monitor: \(error)" }
            return
        }
        while !parado {
            let event = nucleo.proximoEvento()
            if event == QUALL_SESSION_EVENT_DISCONNECTED || event == QUALL_SESSION_EVENT_FAILED {
                motivo = "O outro aparelho desconectou. Inicie novamente para reconectar."
                return
            }
            if nucleo.precisaDeIDR() { transmission.pedirIDR() }
            Thread.sleep(forTimeInterval: 0.02)
        }
    }
    private static func par(_ json: String) -> (id: String, nome: String, tela: (Int, Int)?) {
        let d = json.data(using: .utf8).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
        let screen = d["screen"] as? [String: Any]
        let width = (screen?["width_px"] as? NSNumber)?.intValue ?? 0
        let height = (screen?["height_px"] as? NSNumber)?.intValue ?? 0
        return (d["device_id"] as? String ?? "", d["display_name"] as? String ?? "outro aparelho",
                width > 0 && height > 0 ? (width, height) : nil)
    }
    private static func indice(para peer: String) -> Int {
        var table = UserDefaults.standard.data(forKey: "monitor.indices")
            .flatMap { try? JSONDecoder().decode(TabelaDeIndices.self, from: $0) } ?? TabelaDeIndices()
        let index = table.indice(para: peer.isEmpty ? "legado" : peer, emUso: [], agora: Date().timeIntervalSince1970)
        if let data = try? JSONEncoder().encode(table) { UserDefaults.standard.set(data, forKey: "monitor.indices") }
        return index
    }
}

final class SessaoReceptora: Sessao {
    let endereco: String
    let pin: String
    let tela: (largura: UInt32, altura: UInt32)
    let exibidor: Exibidor
    var aoEstado: ((String) -> Void)?
    var aoTerminar: ((String) -> Void)?
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
        var reason = ""
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
                case QUALL_STATUS_NEEDS_PIN: reason = "Digite o PIN de seis dígitos mostrado no outro aparelho."
                case QUALL_STATUS_WRONG_PIN: reason = "PIN incorreto. Confira o PIN no outro aparelho."
                default: reason = "Não consegui conectar. Confira endereço, porta e rede local."
                }
                Registro.linha("receptor: conexão falhou status=\(nucleo.ultimoStatus.rawValue)")
            }
            return
        }
        Identidade.guardarPares(nucleo.paresConhecidos(somandoA: peers.isEmpty ? nil : peers))
        guard let track = nucleo.esperarTrackDeVideo(prazoMs: 10_000), track.tipo == QUALL_TRACK_KIND_SCREEN else {
            if !nucleo.parou { reason = "O aparelho não ofereceu uma tela. Use Estender tela ou Espelhar uma tela no Quall Studio." }
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
        guard status == QUALL_STATUS_OK else { reason = "Não consegui iniciar a recepção do vídeo."; return }
        let peer = nucleo.nomeDoPar
        DispatchQueue.main.async { [self] in aoEstado?("Exibindo a tela de \(peer)") }
        var dropped: UInt64 = 0
        var lastRequest: UInt64 = 0
        var lastLink: UInt64 = 0
        var lastDecodeFailures: UInt64 = 0
        var link = JanelaDoEnlace()
        while !nucleo.parou {
            let event = nucleo.evento(prazoMs: 0)
            if event == QUALL_SESSION_EVENT_DISCONNECTED || event == QUALL_SESSION_EVENT_FAILED {
                reason = "O emissor desconectou. Conecte novamente quando ele estiver disponível."
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
                reason = "O vídeo parou por 15 segundos. Confira a conexão e tente novamente."
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
