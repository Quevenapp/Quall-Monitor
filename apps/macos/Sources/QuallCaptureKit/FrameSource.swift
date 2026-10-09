import Foundation
import CoreMedia

/// Origem de captura — tela (ScreenCaptureKit) ou câmera (AVCaptureSession). `CaptureSession`
/// não sabe qual das duas está por trás; só pede frames.
protocol FrameSource: AnyObject {
    /// Chamado a cada frame completo, na fila `sampleHandlerQueue` passada em `beginCapture`.
    var onFrame: ((CMSampleBuffer) -> Void)? { get set }
    /// Chamado a cada bloco de **áudio de sistema**, na mesma fila do vídeo.
    ///
    /// Está no protocolo, e não só no `ScreenCapturer`, para que quem orquestra a transmissão não
    /// precise saber qual fonte está por trás — a mesma razão de `onFrame` estar aqui. Mas só a
    /// captura de tela pode preenchê-lo: no macOS o áudio de sistema vem do ScreenCaptureKit, e
    /// uma `AVCaptureSession` de câmera não tem de onde tirá-lo. Ver `capturaAudioDeSistema`.
    var onAudio: ((CMSampleBuffer) -> Void)? { get set }
    /// Esta fonte pode entregar áudio de sistema? Falso na câmera, e a interface usa isto para
    /// não oferecer "com som" numa origem que nunca teria som.
    var capturaAudioDeSistema: Bool { get }
    /// Chamado se a captura parar sozinha (erro) depois de iniciada.
    var onStop: ((Error?) -> Void)? { get set }

    /// Resolve o dispositivo/display a usar e a resolução de saída. É aqui que a permissão TCC
    /// relevante (Gravação de Tela ou Câmera) é checada pelo sistema — se não concedida, lança.
    /// Separado de `beginCapture` para que quem orquestra possa criar o encoder com a resolução
    /// final antes de qualquer frame começar a chegar (evita perder os primeiros quadros numa
    /// corrida).
    func discoverTarget(width: Int?, height: Int?) async throws -> (width: Int, height: Int)

    /// Inicia a captura de fato.
    func beginCapture(fps: Int32, sampleHandlerQueue: DispatchQueue) async throws

    /// Pede à fonte que passe a entregar num tamanho **menor** que o resolvido em
    /// `discoverTarget`, para que o teto de `TetoDoEmissor` seja aplicado onde é barato.
    ///
    /// Devolve `false` quando a fonte não sabe fazer isso — e `false` **não é falha**: quem
    /// chama então aplica o mesmo teto no encoder, que é mais caro e igualmente correto. A
    /// distinção existe porque as duas fontes deste projeto são honestamente diferentes: o
    /// ScreenCaptureKit compõe no tamanho que se pedir, e uma `AVCaptureSession` de câmera
    /// entrega o formato ativo do dispositivo e pronto.
    ///
    /// Só reduz. Um pedido maior que o alvo corrente é recusado: ampliar gastaria banda para não
    /// acrescentar informação nenhuma.
    func reduzirDestino(largura: Int, altura: Int) -> Bool

    func stop() async

    /// Nome da API de captura, para o campo `capture_api` do sidecar — o nome real, não uma
    /// abreviação (ex.: "ScreenCaptureKit", "AVCaptureSession").
    var captureAPIName: String { get }

    /// Faixa de cor dos `CVPixelBuffer` que esta fonte entrega ao encoder — determinada pelo
    /// `pixelFormat` pedido à API de captura (ver `ColorRange`), não pelo encoder. É o que vai
    /// para o campo `color_range` do sidecar.
    var colorRange: ColorRange { get }
}

extension FrameSource {
    /// Padrão: a fonte não sabe reduzir. É o comportamento certo para a câmera, e faz o teto cair
    /// no encoder sem que ninguém precise escrever nada lá.
    func reduzirDestino(largura: Int, altura: Int) -> Bool { false }
}

/// Qual fonte de captura usar.
public enum CaptureSourceKind: String, Codable, CaseIterable {
    case screen
    case camera
}

/// Faixa de cor YCbCr do fluxo. Ver `docs/contrato-sidecar.md`: a primeira rodada do M1 saiu
/// divergente nisso (macOS em `full`, Windows em `limited`) e é o tipo de defeito que só aparece
/// como imagem lavada ou esmagada no receptor, depois de tudo "funcionar" — declarar é obrigatório.
public enum ColorRange: String, Codable {
    /// Luma [0, 255], chroma [1, 255] — `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`.
    case full
    /// Luma [16, 235], chroma [16, 240] — `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`.
    case limited
}
