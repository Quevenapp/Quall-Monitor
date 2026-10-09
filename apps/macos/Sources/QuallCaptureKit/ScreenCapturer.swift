import Foundation
import QuallIdiomaKit
import ScreenCaptureKit
import CoreMedia
import CoreVideo

public enum CaptureError: Error, CustomStringConvertible {
    case noDisplay
    case permissionDenied(Error)
    case streamFailed(Error)
    /// Pediram `.somenteEsteApp` e não deu para isolar. **Falhar é o comportamento certo**: cair
    /// para a máquina inteira capturaria o som da vida do usuário sem ninguém perceber.
    case escopoNaoIsolavel(String)
    /// O monitor escolhido sumiu **no meio da transmissão** — cabo puxado, tampa fechada, dock
    /// trocado. Pelo mesmo motivo de `noDisplay`: cair para outro monitor mandaria para a rede
    /// uma tela que ninguém escolheu. Ver `VigiaDeMonitor`.
    case monitorSumiu(nome: String, testemunhas: String)
    /// O monitor **continua ligado**, mas entrou num conjunto espelhado — escolha da pessoa no menu
    /// Espelhamento de Tela ou em Ajustes > Monitores, ou lembrança do macOS de uma sessão anterior.
    /// Num conjunto espelhado ele sai da lista de telas ativas e o vigia o dá por sumido; a decisão
    /// (parar) é a mesma, mas a causa dita é outra, e a saída também. Achado na primeira sessão do
    /// usuário com a tela estendida, 10/09: "Tela estendida SUMIU" quando ela tinha virado espelho do
    /// iPad.
    case monitorVirouEspelho(nome: String, espelhoDe: String)

    public var description: String {
        switch self {
        case .noDisplay:
            return T("Nenhum display encontrado para captura.")
        case .permissionDenied(let underlying):
            return
                T("Permissão de Gravação de Tela (ScreenCaptureKit / TCC) negada ou ainda não "
                + "concedida para este binário. Abra Ajustes do Sistema > Privacidade e "
                + "Segurança > Gravação de Tela, habilite o binário que está rodando este "
                + "processo e rode de novo. Erro subjacente: %@", underlying)
        case .streamFailed(let underlying):
            return T("Falha no SCStream: %@", underlying)
        case .escopoNaoIsolavel(let motivo):
            return
                T("Não consegui limitar a captura a este processo: %@. A regra da casa manda "
                + "não capturar em vez de capturar demais — ver docs/regras-de-frente.md.", motivo)
        case .monitorSumiu(let nome, let testemunhas):
            return
                T("O monitor \"%@\" foi desconectado durante a transmissão, então ela parou. "
                + "Escolha outro monitor e comece de novo — o Quall Monitor não troca de tela sozinho, "
                + "porque isso mandaria para a rede uma tela que ninguém escolheu. "
                + "(testemunhas: %@)", nome, testemunhas)
        case .monitorVirouEspelho(let nome, let espelhoDe):
            return
                T("\"%@\" passou a espelhar %@, então a transmissão parou. Para usá-la como "
                + "mais uma tela, escolha \"Usar Como Tela Estendida\" no menu Espelhamento de Tela "
                + "(ou em Ajustes > Monitores) e comece de novo.", nome, espelhoDe)
        }
    }
}

/// A quem o conteúdo capturado pertence. **Existe por causa de uma regra, não de um recurso.**
///
/// `docs/regras-de-frente.md` proíbe capturar a tela do MacBook anfitrião, e `docs/audio.md` §8
/// estende a proibição ao som — com força maior, porque um `.wav` não carrega no nome o que tem
/// dentro. A saída é a mesma dos dois lados: **capturar só o que é nosso**.
///
/// A frente do vidro a vidro fez isso no vídeo com `SCContentFilter(desktopIndependentWindow:)`,
/// capturando só a própria janela do Quall. `.somenteEsteApp` é a irmã sonora: o filtro é limitado
/// ao processo do Quall, então o único som que pode entrar na captura é o que o próprio app está
/// tocando. Um tom que nós geramos atravessa o caminho inteiro — ScreenCaptureKit, conversão,
/// codec, RTP, receptor — e nada da vida do usuário pode entrar no artefato, porque nada da vida
/// dele passa por este processo.
public enum EscopoDaCaptura: String, Sendable {
    /// **O produto.** A tela escolhida, e o som que a máquina inteira está tocando.
    case aMaquinaInteira
    /// **A bancada.** Só a janela e o som deste processo. É o único escopo cujo áudio pode ser
    /// gravado, renderizado ou ouvido.
    case somenteEsteApp
}

/// Captura de tela via ScreenCaptureKit. Exige permissão TCC de Gravação de Tela concedida pelo
/// usuário no diálogo do sistema — não há como conceder isso via automação; ver `CaptureError`.
///
/// # O áudio de sistema mora aqui, e não numa segunda pilha
///
/// `SCStreamConfiguration.capturesAudio` liga uma **segunda saída na mesma `SCStream`** que já
/// carrega o vídeo. Não é uma segunda sessão de captura, não é um segundo pedido de permissão e
/// não é um segundo relógio: os dois fluxos saem do mesmo filtro de conteúdo, sob a mesma
/// concessão de TCC, com carimbos de tempo da mesma base — que é o que torna possível alinhá-los
/// do outro lado sem inventar uma correspondência entre dois relógios que ninguém ligou.
///
/// É também a razão de o áudio **não** ter um `FrameSource` próprio: uma segunda pilha teria um
/// segundo `SCStream`, e dois `SCStream` sobre o mesmo display é o desenho que faz o macOS acender
/// dois indicadores de gravação para uma coisa só.
final class ScreenCapturer: NSObject, FrameSource, SCStreamOutput, SCStreamDelegate {
    private struct Target {
        let display: SCDisplay
        let width: Int
        let height: Int
        /// Só preenchido em `.somenteEsteApp`: o Quall visto pelo ScreenCaptureKit.
        let esteApp: SCRunningApplication?
    }

    /// Chamado a cada frame completo, na fila `sampleHandlerQueue` passada em `beginCapture`.
    var onFrame: ((CMSampleBuffer) -> Void)?
    /// O `SCFrameStatus` de **toda** amostra de tela, antes do filtro de `.complete` — cru, pelo
    /// `rawValue`. Existe para saber se o quadro que falta à captura (29 por segundo com a carga
    /// desenhando 30, 11/09) chegou como "nada mudou" (`.idle`) ou nem chegou.
    var onEstado: ((Int) -> Void)?
    /// Chamado a cada bloco de áudio de sistema, na **mesma** fila. Só chega se
    /// `capturarAudio` for verdadeiro.
    var onAudio: ((CMSampleBuffer) -> Void)?
    /// Chamado se o stream parar sozinho (erro) depois de iniciado.
    var onStop: ((Error?) -> Void)?

    private var stream: SCStream?
    private var target: Target?
    /// `beginCapture` já foi chamado. Depois disso o alvo está congelado — ver `reduzirDestino`.
    private var iniciada = false

    /// Qual monitor capturar. `nil` mantém o comportamento antigo do binário de bancada (o
    /// primeiro que o ScreenCaptureKit listar). O app de produto **sempre** passa um id
    /// explícito, escolhido no seletor: num Mac com dois monitores "o primeiro da lista" não é
    /// uma escolha, é um sorteio, e a pessoa descobriria qual saiu só do outro lado da sala.
    private var displayIDDesejado: CGDirectDisplayID?
    private let capturarAudio: Bool
    private let escopo: EscopoDaCaptura

    /// **`minimumFrameInterval` de meio período**, para origem cuja atualização já é o fps pedido — o
    /// monitor da tela estendida, que nasce com a taxa da sessão. Ali o limite seria redundante (o
    /// ScreenCaptureKit não entrega mais que a atualização do monitor). Medido em 11/09 no iPhone X
    /// pela Ethernet: **não sobe a taxa** (a queda para ~16/s sem Sidecar é outra coisa, não
    /// resolvida), mas deixa os quadros regulares — trancos de 169 e 207 para 9 e 41. Ligado pela
    /// tela estendida (`TransmissaoAoVivo.capturaComMeioPeriodo`). Vale também em `reabrir`.
    var intervaloMinimoDeMeioPeriodo = false

    /// Aponta para o monitor que só passou a existir depois de este capturador nascer — a tela
    /// estendida, cujo monitor é criado em `TransmissaoAoVivo.iniciar()`. Só vale antes de
    /// `discoverTarget`; depois disso o alvo já foi resolvido e mudar aqui mentiria sobre ele.
    ///
    /// Quem constrói para a tela estendida passa `kCGNullDirectDisplay` no `init`, e não `nil`: `nil`
    /// cai no "primeiro monitor da lista", que é a tela da pessoa. Esquecer de chamar isto tem de
    /// falhar com `noDisplay`, e não transmitir a tela errada.
    func apontar(para displayID: CGDirectDisplayID) {
        guard target == nil, !iniciada else { return }
        displayIDDesejado = displayID
    }

    init(displayID: CGDirectDisplayID? = nil,
         capturarAudio: Bool = false,
         escopo: EscopoDaCaptura = .aMaquinaInteira) {
        self.displayIDDesejado = displayID
        self.capturarAudio = capturarAudio
        self.escopo = escopo
        super.init()
    }

    let captureAPIName = "ScreenCaptureKit"
    /// Sempre verdadeiro para a tela: é a mesma API e a mesma permissão. Que a sessão de fato
    /// capture som depende de `capturarAudio`, que é escolha de quem construiu esta fonte.
    let capturaAudioDeSistema = true
    // Faixa limitada (16-235) de propósito — ver ColorRange e a nota em beginCapture sobre por
    // que não uso mais kCVPixelFormatType_420YpCbCr8BiPlanarFullRange.
    let colorRange: ColorRange = .limited

    /// Consulta os displays disponíveis. É aqui que a permissão de Gravação de Tela é checada
    /// pelo sistema — se não concedida, lança `.permissionDenied`.
    ///
    /// `width`/`height`, se passados, sobrescrevem a resolução de saída pedida ao
    /// ScreenCaptureKit (que escala a partir do display real) — por padrão o SCDisplay devolve
    /// a resolução lógica atual em pontos (que no aparelho da bancada, com escala "mais espaço",
    /// não bate com nenhum padrão redondo como 1080p). Passe `width`/`height` explícitos para
    /// forçar uma resolução de captura específica, como 1920x1080.
    func discoverTarget(width: Int? = nil, height: Int? = nil) async throws -> (width: Int, height: Int) {
        let content: SCShareableContent
        do {
            content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        } catch {
            throw CaptureError.permissionDenied(error)
        }
        let display: SCDisplay
        if let desejado = displayIDDesejado {
            // Monitor que sumiu entre a montagem do seletor e o toque em Espelhar (cabo puxado,
            // tampa fechada) **não** cai para outro monitor em silêncio: isso mandaria para a
            // rede uma tela que ninguém escolheu. Falha, e a interface manda escolher de novo.
            guard let achado = content.displays.first(where: { $0.displayID == desejado }) else {
                throw CaptureError.noDisplay
            }
            display = achado
        } else {
            guard let primeiro = content.displays.first else {
                throw CaptureError.noDisplay
            }
            display = primeiro
        }
        let resolvedWidth = width ?? display.width
        let resolvedHeight = height ?? display.height

        // Em `.somenteEsteApp` o filtro precisa do **nosso** processo visto pelo ScreenCaptureKit.
        // Achá-lo por `processID` e não por bundle id: o bundle id de um `.app` de bancada pode
        // repetir o de outra cópia aberta ao lado, e capturar o áudio da cópia errada seria
        // exatamente o tipo de engano que este escopo existe para tornar impossível.
        var esteApp: SCRunningApplication?
        if escopo == .somenteEsteApp {
            let meuPid = ProcessInfo.processInfo.processIdentifier
            esteApp = content.applications.first { $0.processID == meuPid }
            // Com `onScreenWindowsOnly`, um processo só aparece com uma janela na tela, e o
            // `--espelhar-ja` da bancada chega antes de a janela do app aparecer (medido em
            // 19/09/2026: "não lista este processo" logo depois do `open`). Até 3 s de espera,
            // perguntando de novo; o escopo continua o mesmo, só a pergunta se repete.
            var tentativas = 0
            while esteApp == nil && tentativas < 15 {
                tentativas += 1
                try await Task.sleep(nanoseconds: 200_000_000)
                if let deNovo = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true) {
                    esteApp = deNovo.applications.first { $0.processID == meuPid }
                }
            }
            guard esteApp != nil else {
                throw CaptureError.escopoNaoIsolavel(
                    "o ScreenCaptureKit não lista este processo (pid \(meuPid)) entre os "
                    + "aplicativos, então não há como limitar a captura a ele")
            }
        }

        self.target = Target(display: display, width: resolvedWidth, height: resolvedHeight,
                             esteApp: esteApp)
        return (resolvedWidth, resolvedHeight)
    }

    /// Reduz o tamanho de saída pedido ao ScreenCaptureKit, antes de `beginCapture`.
    ///
    /// É por onde o teto de `TetoDoEmissor` chega à tela. O ScreenCaptureKit compõe o quadro **já
    /// no tamanho da `SCStreamConfiguration`**, no mesmo passe de GPU que ele faria de qualquer
    /// jeito — então reduzir aqui custa zero, enquanto reduzir no encoder obriga o VideoToolbox a
    /// abrir uma sessão de transferência para reescalar um quadro grande que nunca precisou
    /// existir.
    ///
    /// Só vale **antes** de `beginCapture`: depois disso a `SCStreamConfiguration` já foi
    /// entregue ao sistema, e mexer no alvo aqui mentiria sobre o que está saindo. Por isso a
    /// guarda de `iniciada` — e ela é a diferença entre um teto e um número bonito no registro.
    func reduzirDestino(largura: Int, altura: Int) -> Bool {
        guard let atual = target, !iniciada else { return false }
        guard largura > 0, altura > 0, largura <= atual.width, altura <= atual.height else {
            return false
        }
        target = Target(display: atual.display, width: largura, height: altura,
                        esteApp: atual.esteApp)
        return true
    }

    /// Inicia a captura de fato. Chamado só depois que quem orquestra já está pronto para
    /// receber frames (encoder já criado), para não perder os primeiros quadros numa corrida.
    func beginCapture(fps: Int32, sampleHandlerQueue: DispatchQueue) async throws {
        guard let target else {
            throw CaptureError.noDisplay
        }
        // O filtro é o que decide **de quem** é o conteúdo — do vídeo e do áudio ao mesmo tempo,
        // porque o ScreenCaptureKit aplica um filtro só à sessão inteira. É por isso que
        // `.somenteEsteApp` isola o som: não há como o áudio de outro processo entrar numa sessão
        // cujo filtro só inclui este.
        let filter: SCContentFilter
        switch escopo {
        case .aMaquinaInteira:
            filter = SCContentFilter(display: target.display, excludingWindows: [])
        case .somenteEsteApp:
            guard let esteApp = target.esteApp else {
                throw CaptureError.escopoNaoIsolavel("o alvo foi resolvido sem o aplicativo deste processo")
            }
            filter = SCContentFilter(display: target.display,
                                     including: [esteApp],
                                     exceptingWindows: [])
        }

        let config = SCStreamConfiguration()
        config.width = target.width
        config.height = target.height
        config.minimumFrameInterval = CMTime(value: 1, timescale: intervaloMinimoDeMeioPeriodo ? fps * 2 : fps)
        // Faixa limitada (video-range, 16-235), não full-range: é o que o resto do projeto
        // (Windows) já produz, e é o padrão que a maioria dos decoders H.264 assume quando não
        // confia (ou não lê) a sinalização VUI de faixa de cor — MediaCodec em aparelhos Android
        // mais simples é o exemplo clássico disso, e é justamente a plataforma mais frágil da
        // bancada (Galaxy A10s). Pedir o pixel format video-range aqui faz o ScreenCaptureKit
        // converter RGB->YCbCr já em faixa limitada; o VideoToolbox deriva a sinalização VUI do
        // pixel format de entrada, então nenhuma outra configuração é necessária no encoder — ver
        // `docs/contrato-sidecar.md` e o relato desta frente para o raciocínio completo.
        config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.showsCursor = true
        config.queueDepth = 5

        if capturarAudio {
            config.capturesAudio = true
            // 48 kHz e dois canais **na captura**, sempre — mesmo quando o codec do fio for mono a
            // 8 kHz. Reamostrar para baixo depois é conversão de qualidade conhecida
            // (`ConversorDeAudio`); pedir baixo aqui e precisar de alto depois não tem volta.
            config.sampleRate = 48_000
            config.channelCount = 2
            // **Nunca** excluir o próprio processo: em `.somenteEsteApp` ele é o único som que
            // pode existir na captura, e excluí-lo entregaria silêncio com todos os contadores
            // subindo — o pior tipo de defeito, o que parece funcionar.
            config.excludesCurrentProcessAudio = false
        }

        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        do {
            try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: sampleHandlerQueue)
            // A segunda saída, na **mesma** `SCStream` e na **mesma** fila. Mesma sessão, mesmo
            // filtro, mesma concessão de TCC, carimbos da mesma base — ver o comentário de tipo.
            if capturarAudio {
                try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: sampleHandlerQueue)
            }
            try await stream.startCapture()
        } catch {
            throw CaptureError.streamFailed(error)
        }
        self.stream = stream
        self.iniciada = true
    }

    func stop() async {
        try? await stream?.stopCapture()
    }

    /// Reabre a captura **no mesmo monitor e no mesmo tamanho**, com uma `SCStream` nova.
    ///
    /// Existe pela tela estendida: medido em 10/09, ao desligar o Sidecar o macOS refez a disposição
    /// dos monitores e o ScreenCaptureKit derrubou a captura do monitor do Quall com "falha ao
    /// encontrar telas ou janelas para capturar" — com o monitor ainda ligado e na lista. O `SCDisplay`
    /// antigo não serve depois de uma reorganização; ele é procurado de novo pelo id.
    func reabrir(fps: Int32, sampleHandlerQueue: DispatchQueue) async throws {
        guard let anterior = target else { throw CaptureError.noDisplay }
        try? await stream?.stopCapture()
        stream = nil
        iniciada = false
        target = nil
        _ = try await discoverTarget(width: anterior.width, height: anterior.height)
        try await beginCapture(fps: fps, sampleHandlerQueue: sampleHandlerQueue)
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard sampleBuffer.isValid else { return }

        if type == .audio {
            // Áudio não tem `SCFrameStatus`: não existe "quadro repetido" nem "quadro em branco"
            // num fluxo contínuo de amostras. O que chega, chega.
            onAudio?(sampleBuffer)
            return
        }

        guard type == .screen else { return }

        if let attachmentsArray = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
            let attachments = attachmentsArray.first,
            let statusRawValue = attachments[.status] as? Int,
            let frameStatus = SCFrameStatus(rawValue: statusRawValue)
        {
            onEstado?(statusRawValue)
            guard frameStatus == .complete else { return }
        }

        onFrame?(sampleBuffer)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        onStop?(error)
    }
}

/// **Sem isto, nenhuma das frases acima chegava à tela.** O `Emissor` mostra
/// `erro.localizedDescription`, e um `Error` do Swift que não é `LocalizedError` devolve ali
/// "A operação não pôde ser concluída (QuallCaptureKit.CaptureError erro 4.)" — a frase cuidadosa
/// sobre o monitor desconectado existia só no `description`, que ninguém mostrava. Achado em 10/09.
extension CaptureError: LocalizedError {
    public var errorDescription: String? { description }
}
