import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import QuartzCore

/// Põe o quadro decodificado na tela, e conta o que aconteceu com ele.
///
/// # `AVSampleBufferDisplayLayer`, e não Metal
///
/// A camada aceita `CMSampleBuffer` **já descomprimido**, então o caminho fica
/// `VTDecompressionSession` → `CVPixelBuffer` → camada, com o decode medido no meio. A alternativa
/// — mandar o H.264 direto para a camada e deixar que ela decodifique — é menos código e apaga
/// exatamente o número que o `docs/contrato-track.md` pede: sem sessão de decode própria não há
/// `decode p50/p95`.
///
/// # Sem fila, e o contador que diz isso
///
/// Cada quadro é oferecido à camada na hora e **descartado** se ela não quiser. Espelhamento ao
/// vivo não tem uso para quadro velho — é a mesma decisão do receptor de câmera do macOS, que
/// mediu 250 ms de fila no A07 justamente por não a ter tomado. `naoCouberam` é o preço aparecendo
/// como número em vez de virar latência silenciosa.
///
/// # O que "enfileirado" quer dizer aqui, exatamente
///
/// `enfileirados` conta os quadros **aceitos pela camada de exibição** com `DisplayImmediately`,
/// não pixels confirmados no vidro — nenhuma API do macOS confirma isso. O contador se chamava
/// `exibidos` no receptor iOS e foi renomeado em 2026-08-28 porque prometia vidro e entregava
/// enfileiramento. Nasce já com o nome honesto aqui.
///
/// A prova de que os pixels **certos** chegaram é outra e é independente: a régua de ``Marca``,
/// lida do buffer decodificado.
///
/// # E o *quando* de cada aceitação, que é a fluidez
///
/// `enfileirados` conta **quantos**; ``Fluidez`` mede o **intervalo entre um e o seguinte**, e é
/// esse intervalo que corresponde ao que o olho recebe. A marca é tirada aqui, no mesmo ponto e
/// sob a mesma trava do incremento de `enfileirados` — ver ``oferecer(_:)`` —, com a mesma
/// ressalva de nome: é a hora em que o quadro foi **entregue** à camada, não a hora em que ele
/// apareceu.
///
/// Ela existe porque `fps` médio não responde a pergunta que o usuário fez. Em 01/09/2026 a média
/// de `fila→tela` de uma corrida dizia 6,4 ms, estava certa, e o pior caso da mesma corrida era
/// 226 ms.
///
/// # Os três contadores de geometria, e o dia que custaram
///
/// Uma `AVSampleBufferDisplayLayer` de quadro zero, ou solta da árvore de camadas, **aceita todo
/// quadro e não desenha nada**: `enqueue` devolve normal, `isReadyForMoreMediaData` segue `true`,
/// `status` nunca vira `.failed`. Medido na bancada do iOS em 2026-08-27:
/// `recebidos 435 · exibidos 435 · marca ok 421 / erro 0`, com a tela **preta**. `largura x altura`
/// teria dito `0x0` na primeira leitura. No AppKit a armadilha é a mesma com uma porta a mais —
/// ver ``VistaDeVideo``.
public final class Exibidor {
    public let camada = AVSampleBufferDisplayLayer()

    private let trava = NSLock()
    private var _ofertados: UInt64 = 0
    private var _enfileirados: UInt64 = 0
    private var _naoCouberam: UInt64 = 0
    private var _semDescricao: UInt64 = 0
    private var _falhasDaCamada: UInt64 = 0
    /// Os intervalos entre entregas aceitas pela camada. Escrita e leitura **sob `trava`**: quem
    /// oferece é a thread de decode e quem lê é o laço de relato.
    private var _fluidez = Fluidez()
    private var descricao: CMFormatDescription?
    private var larguraDaDescricao: Int = 0
    private var alturaDaDescricao: Int = 0

    public struct Instantaneo {
        public var ofertados: UInt64 = 0
        public var enfileirados: UInt64 = 0
        public var naoCouberam: UInt64 = 0
        public var semDescricao: UInt64 = 0
        public var falhasDaCamada: UInt64 = 0
        /// A distribuição dos intervalos entre entregas aceitas, já formatada:
        /// `fluidez_ms=[n=… p50=… p95=… max=…] trancos=…`. Ver ``Fluidez``.
        ///
        /// Sai como texto e não como números soltos porque o formato **é** o contrato: quatro
        /// números na mesma ordem em toda casca do projeto, para que duas corridas de plataformas
        /// diferentes sejam lidas lado a lado sem ninguém traduzir nada.
        public var fluidez: String = "fluidez_ms=[n=0 p50=0 p95=0 max=0] trancos=0"
        /// Quantos intervalos passaram do corte de ``Fluidez/trancoMs``.
        public var trancos: UInt64 = 0
        /// Tamanho da camada **em pontos**, e se ela está pendurada em alguma árvore.
        public var larguraDaCamada: Double = 0
        public var alturaDaCamada: Double = 0
        public var camadaNaArvore = false

        /// `true` quando a camada não pode desenhar por geometria — a pergunta que faltava.
        public var camadaInvisivel: Bool { larguraDaCamada < 1 || alturaDaCamada < 1 || !camadaNaArvore }

        public init() {}
    }

    public func instantaneo() -> Instantaneo {
        trava.lock()
        var i = Instantaneo()
        i.ofertados = _ofertados
        i.enfileirados = _enfileirados
        i.naoCouberam = _naoCouberam
        i.semDescricao = _semDescricao
        i.falhasDaCamada = _falhasDaCamada
        i.fluidez = _fluidez.linha
        i.trancos = _fluidez.trancos
        trava.unlock()
        // Fora da trava: são propriedades da `CALayer`, com a trava própria do Core Animation.
        let caixa = camada.bounds
        i.larguraDaCamada = Double(caixa.width)
        i.alturaDaCamada = Double(caixa.height)
        i.camadaNaArvore = camada.superlayer != nil
        return i
    }

    /// Zera os contadores para uma sessão nova, **sem** mexer na camada.
    ///
    /// Existe porque este objeto vive o processo inteiro e as sessões não: sem isto, todo número
    /// deste objeto tem escopo diferente dos números impressos ao lado dele. No receptor iOS a
    /// ausência disto produziu `recebidos 639 · exibidos 1749 · 77,4 fps` numa origem de 30 fps —
    /// um numerador de vida-do-processo ao lado de um denominador de vida-da-sessão.
    ///
    /// A `descricao` também é largada: ela é a geometria da sessão anterior, e mantê-la faria o
    /// primeiro quadro de uma origem com outra dimensão ser oferecido com a descrição errada.
    public func reiniciar() {
        trava.lock()
        _ofertados = 0
        _enfileirados = 0
        _naoCouberam = 0
        _semDescricao = 0
        _falhasDaCamada = 0
        // A âncora vai junto. Sem isto, o intervalo entre o último quadro de uma sessão e o
        // primeiro da seguinte — o tempo de a pessoa digitar um endereço — entraria na
        // distribuição como o maior tranco da corrida.
        _fluidez.reiniciar()
        descricao = nil
        larguraDaDescricao = 0
        alturaDaDescricao = 0
        trava.unlock()
    }

    public init() {
        camada.videoGravity = .resizeAspect
        // Sem isto a camada usa o próprio relógio para agendar a apresentação, e um carimbo de
        // captura vindo de outra máquina — que é o caso de qualquer receptor — a faria segurar ou
        // largar quadro por comparação de relógios que não estão sincronizados.
        camada.controlTimebase = nil
        // O fundo é da camada, e não da vista, para que ele exista mesmo antes de a vista ser
        // medida: uma faixa transparente no lugar do vídeo se lê como janela quebrada.
        camada.backgroundColor = CGColor(red: 0, green: 0, blue: 0, alpha: 1)
    }

    /// Oferece um quadro decodificado. Chamada da thread de decode; nada aqui bloqueia.
    ///
    /// Devolve `true` quando a camada aceitou.
    @discardableResult
    public func oferecer(_ imagem: CVPixelBuffer) -> Bool {
        trava.lock()
        _ofertados &+= 1
        let largura = CVPixelBufferGetWidth(imagem)
        let altura = CVPixelBufferGetHeight(imagem)
        if descricao == nil || largura != larguraDaDescricao || altura != alturaDaDescricao {
            var d: CMFormatDescription?
            CMVideoFormatDescriptionCreateForImageBuffer(allocator: kCFAllocatorDefault,
                                                         imageBuffer: imagem,
                                                         formatDescriptionOut: &d)
            descricao = d
            larguraDaDescricao = largura
            alturaDaDescricao = altura
        }
        let d = descricao
        trava.unlock()

        guard let d else {
            trava.lock(); _semDescricao &+= 1; trava.unlock()
            return false
        }

        // A camada pode entrar em falha (perda de contexto gráfico, app suspenso). Quando entra,
        // ela **para de aceitar quadros para sempre** até um `flush`, e sem esta guarda o sintoma
        // seria "recebidos sobe, enfileirados congela" sem nenhuma pista do motivo.
        if camada.status == .failed {
            trava.lock(); _falhasDaCamada &+= 1; trava.unlock()
            camada.flush()
        }

        guard camada.isReadyForMoreMediaData else {
            trava.lock(); _naoCouberam &+= 1; trava.unlock()
            return false
        }

        var tempo = CMSampleTimingInfo(duration: .invalid,
                                       presentationTimeStamp: .invalid,
                                       decodeTimeStamp: .invalid)
        var amostra: CMSampleBuffer?
        guard CMSampleBufferCreateReadyWithImageBuffer(allocator: kCFAllocatorDefault,
                                                       imageBuffer: imagem,
                                                       formatDescription: d,
                                                       sampleTiming: &tempo,
                                                       sampleBufferOut: &amostra) == noErr,
              let amostra else {
            trava.lock(); _naoCouberam &+= 1; trava.unlock()
            return false
        }

        // `DisplayImmediately`: mostre assim que puder, sem esperar relógio nenhum. É o que
        // espelhamento ao vivo quer, e é o que dispensa a `controlTimebase`.
        if let anexos = CMSampleBufferGetSampleAttachmentsArray(amostra, createIfNecessary: true),
           CFArrayGetCount(anexos) > 0 {
            let dicionario = unsafeBitCast(CFArrayGetValueAtIndex(anexos, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dicionario,
                                 Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }

        camada.enqueue(amostra)
        // **O ponto de apresentação desta casca**, e ele é aqui e não noutro lugar: é o instante
        // em que o quadro deixou de ser nosso e virou responsabilidade da camada de exibição. O
        // relógio é lido **depois** do `enqueue` e **antes** da trava, para que a espera pela
        // trava de contadores não vire cauda de imagem.
        //
        // Só os aceitos contam. Um quadro que não coube (`naoCouberam`) ou que ficou retido pela
        // porta não foi apresentado, e o custo dele aparece exatamente como deve: como um
        // intervalo maior entre os dois quadros que **foram**.
        let agoraUs = Medidas.agoraUs()
        trava.lock(); _enfileirados &+= 1; _fluidez.apresentou(agoraUs: agoraUs); trava.unlock()
        return true
    }

    /// Larga o quadro retido. É conteúdo da tela de outra pessoa e não tem por que continuar em
    /// memória depois da sessão.
    public func limpar() { camada.flushAndRemoveImage() }
}
