import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

/// Decode H.264 no VideoToolbox do Mac, com o custo de cada quadro medido.
///
/// # De onde ele veio, e por que deste e não do outro
///
/// `docs/app-macos.md` deixou escrito que existem **dois** `VTDecompressionSession` na família
/// Apple deste repositório e que "o mais correto não é o mais óbvio":
/// `integrations/camera-macos/.../DecodificadorH264.swift` é o do macOS, e
/// `apps/ios/Receptor/Comum/DecodificadorH264.swift` é o **corrigido**. Este é porte do segundo,
/// e as três correções que ele carrega e o primeiro não tem são:
///
/// 1. **`recusados` separado de `semParametros`.** "Chegou quadro e o decodificador recusou" e
///    "chegou quadro antes de existir decodificador" são duas coisas, e somá-las esconde
///    exatamente a metade das corridas em que o primeiro quadro se perde (dívida 25).
/// 2. **`falhasDeDescricao` e `falhasDeSessao`, com o `OSStatus` guardado.** Sem eles, SPS e PPS
///    que chegam e são recusados pelo VideoToolbox aparecem como `semParametros` — que se lê como
///    "o emissor não mandou SPS/PPS" e manda quem investiga para o outro lado do fio. O sintoma
///    medido na bancada do iOS foi `frames_ready` subindo, 100% dos quadros `sem_parametros`,
///    `exibidos 0`, `decode p50 0,00 ms`.
/// 3. **O perfil é lido e guardado ANTES de qualquer tentativa.** Quando a montagem falha é
///    justamente quando se precisa saber que nível o emissor mandou; guardar só no caminho de
///    sucesso apagava a única pista no caso em que ela importa.
///
/// # O decode é síncrono
///
/// Ao contrário do original do macOS, que liga `_EnableAsynchronousDecompression`. O
/// `CMBlockBuffer` é criado com `kCFAllocatorNull` sobre um `[UInt8]` local; com decode assíncrono
/// a chamada volta antes de o VideoToolbox ter acabado com aquela memória, que é uso-após-liberação
/// esperando acontecer. O custo é que o decode entra no tempo do tratador — e o tratador não pode
/// bloquear, porque a barreira do `close` desiste em 2 s. No iPad A16 o p50 ficou em 2,39 ms, três
/// ordens de grandeza abaixo do teto; num M4 não há razão para ser pior.
public final class DecodificadorH264 {
    private var sessao: VTDecompressionSession?
    /// **Só para os testes**: as próximas N criações de sessão falham como se o VideoToolbox
    /// recusasse (`kVTVideoDecoderMalfunctionErr`). É o que permite provar, sem aparelho, que o IDR
    /// seguinte tenta de novo (o M9 de 21/09). Zero no produto, sempre.
    var falharAsProximasCriacoes = 0
    private var descricao: CMFormatDescription?
    private var sps: [UInt8] = []
    private var pps: [UInt8] = []
    private let aoQuadro: (CVPixelBuffer, UInt64) -> Void

    /// Contadores de instrumento, com trava própria — a regra da casa: *contador de instrumento é
    /// atômico ou tem trava própria; a leitura tem direito a número velho, jamais a espera*.
    private let travaDoRelato = NSLock()
    private var _recebidos: UInt64 = 0
    private var _decodificados: UInt64 = 0
    private var _recusados: UInt64 = 0
    private var _semParametros: UInt64 = 0
    private var _idrsRecebidos: UInt64 = 0
    private var _sessoesCriadas: UInt64 = 0
    private var _falhasDeDescricao: UInt64 = 0
    private var _falhasDeSessao: UInt64 = 0
    private var _ultimaFalha: OSStatus = 0
    private var custos: [UInt64] = []
    private var _largura: Int32 = 0
    private var _altura: Int32 = 0
    private var _perfil = ""

    public init(aoQuadro: @escaping (CVPixelBuffer, UInt64) -> Void) {
        self.aoQuadro = aoQuadro
        custos.reserveCapacity(4096)
    }

    deinit { fechar() }

    /// Espera os quadros saírem e invalida. Chamável só depois de o tratador de quadro do núcleo
    /// já estar desregistrado — senão um quadro em voo entraria numa sessão morta.
    public func fechar() {
        guard let s = sessao else { return }
        sessao = nil
        VTDecompressionSessionWaitForAsynchronousFrames(s)
        VTDecompressionSessionInvalidate(s)
    }

    public struct Instantaneo {
        public var recebidos: UInt64 = 0
        public var decodificados: UInt64 = 0
        public var recusados: UInt64 = 0
        public var semParametros: UInt64 = 0
        public var idrsRecebidos: UInt64 = 0
        public var sessoesCriadas: UInt64 = 0
        public var falhasDeDescricao: UInt64 = 0
        public var falhasDeSessao: UInt64 = 0
        public var ultimaFalha: OSStatus = 0
        public var largura: Int32 = 0
        public var altura: Int32 = 0
        public var perfil = ""
        public var n = 0
        public var p50Us: UInt64 = 0
        public var p95Us: UInt64 = 0
        public var maxUs: UInt64 = 0

        public init() {}
    }

    public func instantaneo() -> Instantaneo {
        travaDoRelato.lock(); defer { travaDoRelato.unlock() }
        let p = Medidas.percentis(custos)
        var i = Instantaneo()
        i.recebidos = _recebidos
        i.decodificados = _decodificados
        i.recusados = _recusados
        i.semParametros = _semParametros
        i.idrsRecebidos = _idrsRecebidos
        i.sessoesCriadas = _sessoesCriadas
        i.falhasDeDescricao = _falhasDeDescricao
        i.falhasDeSessao = _falhasDeSessao
        i.ultimaFalha = _ultimaFalha
        i.largura = _largura
        i.altura = _altura
        i.perfil = _perfil
        i.n = p.n
        i.p50Us = p.p50
        i.p95Us = p.p95
        i.maxUs = p.max
        return i
    }

    // --- entrada -----------------------------------------------------------------------------

    /// Recebe um quadro Annex-B completo do núcleo. **O ponteiro vale só durante a chamada** e esta
    /// função roda numa thread da libdatachannel: nada aqui pode bloquear.
    public func alimentar(annexb: UnsafeRawBufferPointer, timestampUs: UInt64, idr: Bool) {
        travaDoRelato.lock()
        _recebidos &+= 1
        if idr { _idrsRecebidos &+= 1 }
        travaDoRelato.unlock()

        var vcl: [(Int, Int)] = []   // (deslocamento, tamanho) de cada NAL de imagem
        var novoSps: [UInt8]?
        var novoPps: [UInt8]?

        DecodificadorH264.percorrerNals(annexb) { inicio, tamanho in
            let tipo = annexb[inicio] & 0x1F
            switch tipo {
            case 7: novoSps = Array(UnsafeRawBufferPointer(rebasing: annexb[inicio..<(inicio + tamanho)]))
            case 8: novoPps = Array(UnsafeRawBufferPointer(rebasing: annexb[inicio..<(inicio + tamanho)]))
            case 1, 5: vcl.append((inicio, tamanho))
            default: break
            }
        }

        // **Sem sessão, todo IDR tenta de novo** (o M9 da crítica de 21/09): o par é guardado antes
        // de a sessão nascer, e uma recusa do VideoToolbox deixava os IDR seguintes com o mesmo
        // par sem tentar — a imagem parava até o SPS mudar. Com a troca de tamanho no meio da
        // sessão virando rotina, uma recusa única na troca congelaria a tela até a troca seguinte.
        if let novoSps, let novoPps, novoSps != sps || novoPps != pps || sessao == nil {
            sps = novoSps
            pps = novoPps
            recriarSessao()
        }

        guard let sessao, let descricao else {
            // Quadro antes de qualquer SPS. **Descartar é a única coisa correta**: alimentar o
            // decodificador com um quadro P sem parâmetros é o caminho conhecido para travá-lo de
            // vez (achado do receptor Windows no M2, repetido pelo Android).
            travaDoRelato.lock(); _semParametros &+= 1; travaDoRelato.unlock()
            return
        }
        guard !vcl.isEmpty else { return }

        // Annex-B -> AVCC: os start codes viram prefixos de tamanho de 4 bytes, big-endian.
        var carga = [UInt8]()
        carga.reserveCapacity(annexb.count + 8)
        for (inicio, tamanho) in vcl {
            let n = UInt32(tamanho).bigEndian
            withUnsafeBytes(of: n) { carga.append(contentsOf: $0) }
            carga.append(contentsOf: UnsafeRawBufferPointer(rebasing: annexb[inicio..<(inicio + tamanho)]))
        }

        carga.withUnsafeMutableBytes { p in
            guard let base = p.baseAddress else { return }
            var bloco: CMBlockBuffer?
            let criou = CMBlockBufferCreateWithMemoryBlock(
                allocator: kCFAllocatorDefault, memoryBlock: base, blockLength: p.count,
                blockAllocator: kCFAllocatorNull, customBlockSource: nil,
                offsetToData: 0, dataLength: p.count, flags: 0, blockBufferOut: &bloco)
            guard criou == noErr, let bloco else {
                travaDoRelato.lock(); _recusados &+= 1; travaDoRelato.unlock()
                return
            }

            var amostra: CMSampleBuffer?
            var tamanho = p.count
            var tempo = CMSampleTimingInfo(
                duration: .invalid,
                presentationTimeStamp: CMTime(value: CMTimeValue(timestampUs), timescale: 1_000_000),
                decodeTimeStamp: .invalid)
            let fez = CMSampleBufferCreateReady(
                allocator: kCFAllocatorDefault, dataBuffer: bloco, formatDescription: descricao,
                sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &tempo,
                sampleSizeEntryCount: 1, sampleSizeArray: &tamanho, sampleBufferOut: &amostra)
            guard fez == noErr, let amostra else {
                travaDoRelato.lock(); _recusados &+= 1; travaDoRelato.unlock()
                return
            }

            // O instante da submissão viaja como **valor** no `sourceFrameRefCon`, que nunca é
            // dereferenciado. Evita uma alocação por quadro só para medir latência de decode.
            let submissao = Medidas.agoraUs()
            let marca = UnsafeMutableRawPointer(bitPattern: UInt(submissao))
            var saida = VTDecodeInfoFlags()
            let estado = VTDecompressionSessionDecodeFrame(
                sessao, sampleBuffer: amostra, flags: [._1xRealTimePlayback],
                frameRefcon: marca, infoFlagsOut: &saida)
            if estado != noErr {
                travaDoRelato.lock(); _recusados &+= 1; _ultimaFalha = estado; travaDoRelato.unlock()
            }
        }
    }

    // --- sessão ------------------------------------------------------------------------------

    private func recriarSessao() {
        fechar()

        var nova: CMFormatDescription?
        let estado = sps.withUnsafeBufferPointer { s -> OSStatus in
            pps.withUnsafeBufferPointer { p -> OSStatus in
                let conjuntos = [s.baseAddress!, p.baseAddress!]
                let tamanhos = [s.count, p.count]
                return conjuntos.withUnsafeBufferPointer { cp in
                    tamanhos.withUnsafeBufferPointer { tp in
                        CMVideoFormatDescriptionCreateFromH264ParameterSets(
                            allocator: kCFAllocatorDefault, parameterSetCount: 2,
                            parameterSetPointers: cp.baseAddress!, parameterSetSizes: tp.baseAddress!,
                            nalUnitHeaderLength: 4, formatDescriptionOut: &nova)
                    }
                }
            }
        }
        // O perfil/nível saem do próprio SPS: byte 1 é `profile_idc`, byte 3 é `level_idc`.
        // **Lido e guardado antes de qualquer tentativa** — ver o cabeçalho do tipo.
        let perfil = sps.count >= 4
            ? "profile_idc=\(sps[1]) level_idc=\(sps[3])"
            : "?"
        travaDoRelato.lock(); _perfil = perfil; travaDoRelato.unlock()

        guard estado == noErr, let nova else {
            travaDoRelato.lock()
            _falhasDeDescricao &+= 1
            _ultimaFalha = estado
            travaDoRelato.unlock()
            return
        }
        descricao = nova

        let dim = CMVideoFormatDescriptionGetDimensions(nova)

        let atributos: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            // Sem `IOSurface` a `AVSampleBufferDisplayLayer` copia o buffer para poder compor, e a
            // cópia por quadro a 30 fps aparece na CPU. No macOS ele também é o que permite ao
            // WindowServer compor o quadro sem passar pela memória do processo.
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var callback = VTDecompressionOutputCallbackRecord(
            decompressionOutputCallback: { eu, refcon, estado, _, imagem, _, _ in
                guard let eu else { return }
                let quem = Unmanaged<DecodificadorH264>.fromOpaque(eu).takeUnretainedValue()
                quem.saiu(estado: estado, imagem: imagem, marca: refcon)
            },
            decompressionOutputRefCon: Unmanaged.passUnretained(self).toOpaque())

        if falharAsProximasCriacoes > 0 {
            falharAsProximasCriacoes -= 1
            travaDoRelato.lock()
            _falhasDeSessao &+= 1
            _ultimaFalha = kVTVideoDecoderMalfunctionErr
            travaDoRelato.unlock()
            return
        }
        var criada: VTDecompressionSession?
        let r = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: nova, decoderSpecification: nil,
            imageBufferAttributes: atributos as CFDictionary, outputCallback: &callback,
            decompressionSessionOut: &criada)
        guard r == noErr, let criada else {
            // Aqui mora o caso que a bancada do iOS procurou o dia inteiro: SPS e PPS **chegaram**
            // e foram aceitos pela descrição, e mesmo assim não há decodificador. Sem este
            // contador, o sintoma vira "sem parâmetros" e a investigação sobe para o emissor.
            travaDoRelato.lock()
            _falhasDeSessao &+= 1
            _ultimaFalha = r
            _largura = dim.width
            _altura = dim.height
            travaDoRelato.unlock()
            return
        }
        VTSessionSetProperty(criada, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        sessao = criada

        travaDoRelato.lock()
        _largura = dim.width
        _altura = dim.height
        _perfil = perfil
        _sessoesCriadas &+= 1
        travaDoRelato.unlock()
    }

    private func saiu(estado: OSStatus, imagem: CVImageBuffer?, marca: UnsafeMutableRawPointer?) {
        guard estado == noErr, let imagem else {
            travaDoRelato.lock(); _recusados &+= 1; _ultimaFalha = estado; travaDoRelato.unlock()
            return
        }
        let submissao = UInt64(UInt(bitPattern: marca))
        let custo = submissao > 0 ? Medidas.delta(Medidas.agoraUs(), submissao) : 0
        travaDoRelato.lock()
        // Teto na amostra: a lista existe para dar percentil, e uma corrida longa não pode fazer o
        // instrumento crescer sem limite dentro do processo que ele mede.
        if custos.count < 100_000 { custos.append(custo) }
        _decodificados &+= 1
        travaDoRelato.unlock()
        aoQuadro(imagem, custo)
    }

    /// Varre os NALs de um quadro Annex-B, aceitando start code de 3 e de 4 bytes.
    ///
    /// Estática e `public` de propósito: é a única parte do decodificador que dá para exercitar
    /// sem VideoToolbox, e um erro aqui aparece como "não achou SPS" — que se lê como defeito do
    /// emissor. Ver `TestesDoDecodificador`.
    public static func percorrerNals(_ dados: UnsafeRawBufferPointer, _ visitar: (Int, Int) -> Void) {
        let n = dados.count
        var i = 0
        var inicioDoNal = -1
        while i + 2 < n {
            if dados[i] == 0, dados[i + 1] == 0, dados[i + 2] == 1 {
                if inicioDoNal >= 0 {
                    var fim = i
                    if fim > inicioDoNal, dados[fim - 1] == 0 { fim -= 1 }  // start code de 4 bytes
                    if fim > inicioDoNal { visitar(inicioDoNal, fim - inicioDoNal) }
                }
                i += 3
                inicioDoNal = i
                continue
            }
            i += 1
        }
        if inicioDoNal >= 0, inicioDoNal < n { visitar(inicioDoNal, n - inicioDoNal) }
    }
}
