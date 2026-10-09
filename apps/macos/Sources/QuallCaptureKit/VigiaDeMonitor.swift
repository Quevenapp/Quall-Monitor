import AppKit
import CoreGraphics
import Foundation

/// Vigia o monitor escolhido e avisa quando ele **some**, para que a transmissão falhe em vez de
/// continuar viva sobre uma fonte que não existe mais.
///
/// # Por que não basta confiar no evento do sistema
///
/// Isto existe por desconfiança **medida**, e a medição é de outra plataforma. Em 2026-08-27 a
/// frente do Windows puxou o cabo HDMI do Dell no meio de uma transmissão e o evento oficial do
/// sistema — `GraphicsCaptureItem::Closed` — **não disparou**: treze leituras ao longo de três
/// segundos, todas falsas. A única testemunha da perda foi a reenumeração de monitores, e sem ela
/// o app teria ficado vivo, codificando nada, até alguém desistir.
///
/// No macOS há **três** candidatos a testemunha e nenhum deles tinha sido exercitado nesta casa.
/// Então este vigia não escolhe um: escuta os três ao mesmo tempo, registra **qual chegou
/// primeiro e em quanto tempo**, e só então derruba a sessão. A pergunta "o evento oficial
/// dispara?" é respondida por medição, não por leitura de documentação.
///
/// | testemunha | o que é |
/// |---|---|
/// | `CGDisplayReconfiguration` | o retorno oficial do CoreGraphics, com as bandeiras de remoção |
/// | `NSApplication.didChangeScreenParameters` | o aviso do AppKit, o mesmo que reordena `NSScreen.screens` |
/// | `reenumeração` | uma ronda de `CGGetActiveDisplayList` a cada 200 ms — a que salvou o Windows |
/// | `SCStream.didStopWithError` | o ScreenCaptureKit desistindo por conta própria (registrada por quem constrói) |
///
/// # Falhar, e não cair para outro monitor
///
/// A regra que este vigia serve é a mesma de `ScreenCapturer.discoverTarget`: monitor que sumiu
/// **não** vira "então manda o outro". Cair em silêncio para outro monitor mandaria para a rede
/// uma tela que ninguém escolheu — que é o defeito de privacidade, não de robustez.
///
/// # A janela de tolerância
///
/// Depois da primeira testemunha o vigia continua escutando por `janelaDeTolerancia` segundos
/// antes de derrubar, só para poder dizer **quais outras testemunhas chegaram e quando**. Isso é
/// instrumento de bancada e custa latência: em produto a janela é zero e a sessão cai na primeira
/// testemunha. A distinção importa porque "não disparou" e "não disparou dentro de 3 s" são
/// frases diferentes, e só a segunda é defensável.
public final class VigiaDeMonitor {
    /// Uma das testemunhas de que o monitor sumiu.
    public struct Testemunha: Sendable {
        /// Qual das quatro.
        public let nome: String
        /// Milissegundos depois da **primeira** testemunha. A primeira é sempre 0.
        public let msDesdeAPrimeira: Double
        /// O que ela trazia junto — bandeiras do CoreGraphics, o erro do SCStream, a contagem
        /// de telas vista pela ronda.
        public let detalhe: String
    }

    /// O monitor que esta sessão escolheu. Guardado como `CGDirectDisplayID`, e não como índice
    /// na lista, pela mesma razão que o seletor guarda: a lista se reordena.
    public let monitorada: CGDirectDisplayID
    /// O nome que a pessoa escolheu na lista, para a mensagem de erro dizer qual monitor caiu.
    public let nomeDoMonitor: String

    private let janelaDeTolerancia: Double
    private let periodoDaRonda: Double

    /// Chamado uma vez, com todas as testemunhas colhidas, quando a janela de tolerância fecha.
    public var aoSumir: (([Testemunha]) -> Void)?

    /// **A segunda olhada, no fim da janela.** Quando dado e verdadeiro, o monitor **voltou** dentro
    /// da janela de tolerância, e o vigia não derruba: esquece as testemunhas e segue vigiando.
    ///
    /// Existe pela tela estendida (`docs/tela-estendida.md`): o monitor dela sai da lista de telas
    /// ativas por instantes quando o macOS mexe nos monitores — o Sidecar reaplicando um espelho que
    /// o auxiliar desfaz em menos de 0,5 s, o iPad saindo e a disposição sendo refeita. Sem isto,
    /// cada um desses instantes encerrava a sessão. Nulo (o padrão) é o comportamento de sempre.
    public var revalidar: (() -> Bool)?

    /// Chamado quando a segunda olhada salvou a sessão — para quem transmite reabrir a captura, que
    /// o ScreenCaptureKit pode ter derrubado no meio da mexida.
    public var aoVoltar: (() -> Void)?
    /// Cada linha que o vigia quer no registro, à medida que acontece.
    public var aoRegistrar: ((String) -> Void)?

    private let trava = NSLock()
    private var testemunhas: [Testemunha] = []
    private var instanteDaPrimeira: UInt64?
    private var encerrado = false
    private var registradoNoCG = false

    private var ronda: DispatchSourceTimer?
    private var observador: NSObjectProtocol?
    private let filaDaRonda = DispatchQueue(label: "br.com.queven.quall.vigia-de-monitor")

    /// - Parameters:
    ///   - janelaDeTolerancia: segundos que o vigia continua colhendo testemunhas depois da
    ///     primeira, antes de derrubar. **Zero em produto**; a bancada passa 3 para poder afirmar
    ///     "não disparou em 3 s" em vez do impreciso "não disparou".
    public init(monitorada: CGDirectDisplayID,
                nomeDoMonitor: String,
                janelaDeTolerancia: Double = 0,
                periodoDaRonda: Double = 0.2) {
        self.monitorada = monitorada
        self.nomeDoMonitor = nomeDoMonitor
        self.janelaDeTolerancia = max(janelaDeTolerancia, 0)
        self.periodoDaRonda = max(periodoDaRonda, 0.02)
    }

    deinit { pararSemAvisar() }

    // MARK: - as telas que existem agora

    /// A lista de telas ativas, direto do CoreGraphics.
    ///
    /// Deliberadamente **não** é o `SCShareableContent`: aquele passa pelo TCC, é assíncrono e
    /// custa caro demais para rodar a cada 200 ms. `CGGetActiveDisplayList` não pede permissão
    /// nenhuma — enumerar telas nunca foi conteúdo — e é o que torna a ronda barata o bastante
    /// para existir.
    public static func telasAtivas() -> [CGDirectDisplayID] {
        var contagem: UInt32 = 0
        guard CGGetActiveDisplayList(0, nil, &contagem) == .success, contagem > 0 else { return [] }
        var lista = [CGDirectDisplayID](repeating: 0, count: Int(contagem))
        guard CGGetActiveDisplayList(contagem, &lista, &contagem) == .success else { return [] }
        return Array(lista.prefix(Int(contagem)))
    }

    private func aindaExiste() -> Bool {
        VigiaDeMonitor.telasAtivas().contains(monitorada)
    }

    // MARK: - começar e parar

    public func comecar() {
        // 1. O retorno oficial do CoreGraphics.
        CGDisplayRegisterReconfigurationCallback(retornoDeReconfiguracao,
                                                 Unmanaged.passUnretained(self).toOpaque())
        registradoNoCG = true

        // 2. O aviso do AppKit. É o mesmo que faz `NSScreen.screens` mudar, e portanto o mesmo
        //    que a lista de fontes do seletor usaria para se recompor.
        observador = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil,
            queue: nil
        ) { [weak self] _ in
            guard let self else { return }
            guard !self.aindaExiste() else {
                self.aoRegistrar?("vigia: didChangeScreenParameters, mas \(self.nomeDoMonitor) continua na lista")
                return
            }
            self.registrar(nome: "NSApplication.didChangeScreenParameters",
                           detalhe: "\(VigiaDeMonitor.telasAtivas().count) tela(s) ativa(s)")
        }

        // 3. A ronda — a testemunha em que o Windows teve de confiar porque a oficial não veio.
        let t = DispatchSource.makeTimerSource(queue: filaDaRonda)
        t.schedule(deadline: .now() + periodoDaRonda, repeating: periodoDaRonda)
        t.setEventHandler { [weak self] in
            guard let self else { return }
            guard !self.aindaExiste() else { return }
            self.registrar(nome: "reenumeração (CGGetActiveDisplayList)",
                           detalhe: "ronda de \(Int(self.periodoDaRonda * 1000)) ms; "
                               + "\(VigiaDeMonitor.telasAtivas().count) tela(s) ativa(s)")
        }
        t.resume()
        ronda = t

        aoRegistrar?("vigia: ligado sobre \(nomeDoMonitor) (display \(monitorada)); "
            + "ronda a cada \(Int(periodoDaRonda * 1000)) ms; "
            + "janela de tolerância \(janelaDeTolerancia) s; "
            + "telas ativas agora: \(VigiaDeMonitor.telasAtivas().map(String.init).joined(separator: ", "))")
    }

    public func parar() { pararSemAvisar() }

    private func pararSemAvisar() {
        if registradoNoCG {
            CGDisplayRemoveReconfigurationCallback(retornoDeReconfiguracao,
                                                   Unmanaged.passUnretained(self).toOpaque())
            registradoNoCG = false
        }
        ronda?.cancel()
        ronda = nil
        if let observador {
            NotificationCenter.default.removeObserver(observador)
            self.observador = nil
        }
    }

    // MARK: - as testemunhas

    fileprivate func reconfigurou(display: CGDirectDisplayID, flags: CGDisplayChangeSummaryFlags) {
        // O retorno do CoreGraphics fala de **qualquer** mudança em **qualquer** tela — trocar de
        // resolução, mover a origem, ligar espelhamento. Só é testemunha do sumiço se falar da
        // nossa tela e trouxer uma bandeira de remoção ou desativação.
        guard display == monitorada else { return }
        let nomes = VigiaDeMonitor.nomesDasBandeiras(flags)
        let sumiu = flags.contains(.removeFlag) || flags.contains(.disabledFlag)
        guard sumiu else {
            aoRegistrar?("vigia: CGDisplayReconfiguration em \(nomeDoMonitor) sem remoção [\(nomes)]")
            return
        }
        registrar(nome: "CGDisplayReconfiguration", detalhe: "bandeiras [\(nomes)]")
    }

    /// Registra uma testemunha vinda de fora — hoje só o `SCStream.didStopWithError`, que quem
    /// constrói a transmissão liga ao capturador.
    public func registrarTestemunhaExterna(nome: String, detalhe: String) {
        registrar(nome: nome, detalhe: detalhe)
    }

    private func registrar(nome: String, detalhe: String) {
        let agora = DispatchTime.now().uptimeNanoseconds
        var primeira = false
        var decorridoMs = 0.0
        var repetida = false

        trava.lock()
        if encerrado {
            trava.unlock()
            return
        }
        if instanteDaPrimeira == nil {
            instanteDaPrimeira = agora
            primeira = true
        }
        decorridoMs = Double(agora - (instanteDaPrimeira ?? agora)) / 1_000_000
        // Uma testemunha por espécie: a ronda dispara a cada 200 ms e encheria a lista de linhas
        // idênticas, escondendo as outras três.
        repetida = testemunhas.contains { $0.nome == nome }
        if !repetida {
            testemunhas.append(Testemunha(nome: nome, msDesdeAPrimeira: decorridoMs, detalhe: detalhe))
        }
        trava.unlock()

        if !repetida {
            aoRegistrar?(String(format: "vigia: testemunha \"%@\" +%.1f ms — %@", nome, decorridoMs, detalhe))
        }

        guard primeira else { return }
        // **Sumir e virar espelho parecem iguais daqui**: num conjunto espelhado o monitor sai de
        // `CGGetActiveDisplayList` sem sair de `CGGetOnlineDisplayList`. A decisão não muda — quem
        // espelha não tem conteúdo próprio para capturar (medido: 0 quadros) —, mas o registro diz qual
        // dos dois foi. Ver `docs/tela-estendida.md`.
        if let origem = VigiaDeMonitor.espelhoDe(monitorada) {
            aoRegistrar?("vigia: \(nomeDoMonitor) continua ligado, mas VIROU ESPELHO de \(origem)")
        }
        aoRegistrar?("vigia: \(nomeDoMonitor) SUMIU — primeira testemunha foi \"\(nome)\"; "
            + (janelaDeTolerancia > 0
                ? "observando por mais \(janelaDeTolerancia) s antes de derrubar"
                : "derrubando agora"))
        filaDaRonda.asyncAfter(deadline: .now() + janelaDeTolerancia) { [weak self] in
            self?.fechar()
        }
    }

    private func fechar() {
        trava.lock()
        if encerrado {
            trava.unlock()
            return
        }
        let colhidas = testemunhas
        trava.unlock()

        if let revalidar, revalidar() {
            trava.lock()
            testemunhas = []
            instanteDaPrimeira = nil
            trava.unlock()
            aoRegistrar?("vigia: \(nomeDoMonitor) VOLTOU dentro da janela de \(janelaDeTolerancia) s — não derruba; vieram ["
                + colhidas.map { String(format: "%@ +%.1f ms", $0.nome, $0.msDesdeAPrimeira) }.joined(separator: ", ")
                + "]")
            aoVoltar?()
            return
        }

        trava.lock()
        encerrado = true
        trava.unlock()

        // O relato que sobrevive: quem veio, quem não veio, e em quanto tempo. É esta linha que
        // responde "o evento oficial do macOS dispara?".
        let ausentes = VigiaDeMonitor.todasAsTestemunhas.filter { esperada in
            !colhidas.contains { $0.nome == esperada }
        }
        aoRegistrar?("vigia: fim da janela — vieram ["
            + colhidas.map { String(format: "%@ +%.1f ms", $0.nome, $0.msDesdeAPrimeira) }.joined(separator: ", ")
            + "]; NÃO vieram em \(janelaDeTolerancia) s ["
            + ausentes.joined(separator: ", ") + "]")

        pararSemAvisar()
        aoSumir?(colhidas)
    }

    /// O monitor **continua ligado** e está num conjunto espelhado: devolve com quem, pelo nome que a
    /// pessoa lê ("iPad A16", "Tela Retina Integrada"). `nil` quando ele não está ligado ou não
    /// espelha nada — aí sumiu de verdade.
    public static func espelhoDe(_ id: CGDirectDisplayID) -> String? {
        // **A lista, e não `CGDisplayIsOnline`.** Medido pelo teste deste tipo em 10/09: para um id
        // que não existe, `CGDisplayIsOnline` e `CGDisplayIsInMirrorSet` respondem "sim" e
        // `CGDisplayMirrorsDisplay` responde `0xFFFFFFFF`. Um monitor que sumiu de verdade sairia
        // daqui como "virou espelho do monitor 4294967295".
        var contagem: UInt32 = 0
        CGGetOnlineDisplayList(0, nil, &contagem)
        var lista = [CGDirectDisplayID](repeating: 0, count: Int(contagem))
        CGGetOnlineDisplayList(contagem, &lista, &contagem)
        let ligados = Array(lista.prefix(Int(contagem)))
        guard id != kCGNullDirectDisplay, ligados.contains(id), CGDisplayIsInMirrorSet(id) != 0 else { return nil }
        // Num conjunto espelhado o macOS escolhe uma origem (a maior tela) e as outras a espelham.
        var outro = CGDisplayMirrorsDisplay(id)
        if !ligados.contains(outro) {
            // Nós somos a origem: quem nos espelha?
            outro = ligados.first { $0 != id && CGDisplayMirrorsDisplay($0) == id } ?? kCGNullDirectDisplay
        }
        guard outro != kCGNullDirectDisplay else { return "outra tela" }
        for tela in NSScreen.screens {
            if let numero = tela.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber,
               CGDirectDisplayID(truncating: numero) == outro {
                return "\"\(tela.localizedName)\""
            }
        }
        return "o monitor \(outro)"
    }

    /// As quatro que poderiam chegar. Serve para o registro dizer quais **não** chegaram — que é
    /// metade do achado, e a metade que se perde quando só se registra o que aconteceu.
    public static let todasAsTestemunhas = [
        "CGDisplayReconfiguration",
        "NSApplication.didChangeScreenParameters",
        "reenumeração (CGGetActiveDisplayList)",
        "SCStream.didStopWithError",
    ]

    private static func nomesDasBandeiras(_ flags: CGDisplayChangeSummaryFlags) -> String {
        var partes: [String] = []
        if flags.contains(.beginConfigurationFlag) { partes.append("beginConfiguration") }
        if flags.contains(.removeFlag) { partes.append("remove") }
        if flags.contains(.addFlag) { partes.append("add") }
        if flags.contains(.disabledFlag) { partes.append("disabled") }
        if flags.contains(.enabledFlag) { partes.append("enabled") }
        if flags.contains(.movedFlag) { partes.append("moved") }
        if flags.contains(.setModeFlag) { partes.append("setMode") }
        if flags.contains(.desktopShapeChangedFlag) { partes.append("desktopShapeChanged") }
        if flags.contains(.mirrorFlag) { partes.append("mirror") }
        if flags.contains(.unMirrorFlag) { partes.append("unMirror") }
        return partes.isEmpty ? "nenhuma" : partes.joined(separator: "|")
    }
}

/// O retorno do CoreGraphics é um ponteiro de função C: não captura contexto, então o vigia
/// atravessa pelo `userInfo`.
private let retornoDeReconfiguracao: CGDisplayReconfigurationCallBack = { display, flags, contexto in
    guard let contexto else { return }
    let vigia = Unmanaged<VigiaDeMonitor>.fromOpaque(contexto).takeUnretainedValue()
    vigia.reconfigurou(display: display, flags: flags)
}
