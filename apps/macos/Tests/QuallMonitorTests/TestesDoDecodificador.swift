import CoreMedia
import CoreVideo
import VideoToolbox
import XCTest
@testable import QuallReceptorKit

/// O caminho **decodificar → exibir** exercitado sem rede e sem aparelho.
///
/// Este é o teste que o receptor iOS não tem: lá a única prova do decodificador é a corrida contra
/// o iPad. Aqui o encoder e o decodificador rodam na mesma máquina, então dá para fechar o laço
/// inteiro dentro de `swift test` — origem sintética com a régua, encode em H.264, Annex-B, e a
/// mesma `alimentar(annexb:)` que o tratador de quadro do núcleo chama.
///
/// **O que ele NÃO é**: prova de que a imagem apareceu na tela. Nenhuma API do macOS confirma
/// apresentação, e este alvo não abre janela nenhuma. O que ele prova é que os pixels que saem do
/// decodificador são os que entraram no encoder — por contador, pela régua.
final class TestesDoDecodificador: XCTestCase {

    // --- o varredor de NALs --------------------------------------------------------------------

    /// Um erro aqui aparece como "não achou SPS", que se lê como defeito do **emissor** — e manda
    /// quem investiga para o outro lado do fio. É a razão de a função ser estática e testável.
    func testePercorreNalsComStartCodeDeTresEDeQuatroBytes() {
        // 00 00 00 01 [67 AA] 00 00 01 [68 BB] 00 00 00 01 [65 CC DD]
        let dados: [UInt8] = [0, 0, 0, 1, 0x67, 0xAA,
                              0, 0, 1, 0x68, 0xBB,
                              0, 0, 0, 1, 0x65, 0xCC, 0xDD]
        var achados: [[UInt8]] = []
        dados.withUnsafeBytes { p in
            DecodificadorH264.percorrerNals(p) { inicio, tamanho in
                achados.append(Array(dados[inicio..<(inicio + tamanho)]))
            }
        }
        XCTAssertEqual(achados, [[0x67, 0xAA], [0x68, 0xBB], [0x65, 0xCC, 0xDD]])
    }

    func testeUmNalSoEUmNalSo() {
        let dados: [UInt8] = [0, 0, 0, 1, 0x65, 1, 2, 3]
        var achados = 0
        dados.withUnsafeBytes { p in
            DecodificadorH264.percorrerNals(p) { inicio, tamanho in
                achados += 1
                XCTAssertEqual(Array(dados[inicio..<(inicio + tamanho)]), [0x65, 1, 2, 3])
            }
        }
        XCTAssertEqual(achados, 1)
    }

    func testeBufferVazioNaoVisitaNada() {
        var achados = 0
        let vazio = [UInt8]()
        vazio.withUnsafeBytes { p in
            DecodificadorH264.percorrerNals(p) { _, _ in achados += 1 }
        }
        XCTAssertEqual(achados, 0)
    }

    // --- o laço encode → decode → régua ---------------------------------------------------------

    /// **A prova de pixel desta suíte.** Sessenta quadros com a régua entram no encoder do
    /// VideoToolbox, saem como Annex-B, entram no decodificador do produto e voltam como
    /// `CVPixelBuffer` — e a régua lida de volta bate com o número que foi desenhado, quadro a
    /// quadro.
    ///
    /// A origem é sintética e desenhada aqui dentro: nenhum pixel de tela de ninguém atravessa
    /// este teste, e nada é gravado em disco.
    func testeDecodificaEOsPixelsSaoOsMesmos() throws {
        let largura = 640, altura = 360, quantos = 60
        let quadros = try Self.codificar(quadros: quantos, largura: largura, altura: altura)
        XCTAssertEqual(quadros.count, quantos, "o encoder devolveu todos os quadros")
        XCTAssertTrue(quadros[0].idr, "o primeiro quadro tem de ser IDR, com SPS e PPS junto")

        var lidas: [Int] = []
        let decodificador = DecodificadorH264 { imagem, _ in
            if let m = Marca.ler(de: imagem) { lidas.append(m) } else { lidas.append(-1) }
        }
        defer { decodificador.fechar() }

        for q in quadros {
            q.bytes.withUnsafeBytes { p in
                decodificador.alimentar(annexb: p, timestampUs: q.timestampUs, idr: q.idr)
            }
        }

        let i = decodificador.instantaneo()
        XCTAssertEqual(i.recebidos, UInt64(quantos))
        XCTAssertEqual(i.decodificados, UInt64(quantos), "todo quadro entregue saiu decodificado")
        XCTAssertEqual(i.recusados, 0)
        XCTAssertEqual(i.semParametros, 0, "o primeiro quadro já trouxe SPS e PPS")
        XCTAssertEqual(i.falhasDeDescricao, 0)
        XCTAssertEqual(i.falhasDeSessao, 0)
        XCTAssertEqual(i.sessoesCriadas, 1, "um par SPS/PPS, uma sessão de decode")
        XCTAssertEqual(i.largura, Int32(largura))
        XCTAssertEqual(i.altura, Int32(altura))
        XCTAssertTrue(i.perfil.hasPrefix("profile_idc="), "o perfil sai do SPS, não de suposição")

        XCTAssertEqual(lidas.count, quantos)
        XCTAssertEqual(lidas, Array(0..<quantos),
                       "a régua lida de volta é a régua desenhada, quadro a quadro")
        XCTAssertGreaterThan(i.n, 0, "o custo de decode foi medido")
    }

    /// **A troca de tamanho no meio da sessão** (o controle de 21/09: o app do Mac acompanhou as
    /// trocas 854 ↔ 640 com `sessoes_criadas` subindo uma por troca). Um SPS de tamanho novo
    /// recria a sessão, e todo quadro dos dois tamanhos sai decodificado.
    func testeATrocaDeTamanhoRecriaASessaoEDecodificaOsDois() throws {
        let primeiro = try Self.codificar(quadros: 30, largura: 320, altura: 180)
        let segundo = try Self.codificar(quadros: 30, largura: 480, altura: 270)
        var tamanhos: [Int] = []
        let decodificador = DecodificadorH264 { imagem, _ in tamanhos.append(CVPixelBufferGetWidth(imagem)) }
        defer { decodificador.fechar() }
        var tempo: UInt64 = 0
        for q in primeiro + segundo {
            tempo += 33_333
            q.bytes.withUnsafeBytes { decodificador.alimentar(annexb: $0, timestampUs: tempo, idr: q.idr) }
        }
        let i = decodificador.instantaneo()
        XCTAssertEqual(i.decodificados, 60)
        XCTAssertEqual(i.sessoesCriadas, 2, "uma sessão por tamanho")
        XCTAssertEqual(i.falhasDeSessao, 0)
        XCTAssertEqual(i.largura, 480)
        XCTAssertEqual(i.altura, 270)
        XCTAssertEqual(Set(tamanhos), [320, 480])
    }

    /// **Uma recusa do VideoToolbox não congela a imagem** (o M9 da crítica de 21/09). A criação
    /// da sessão falha no primeiro IDR; os quadros até o IDR seguinte são descartados, e o IDR
    /// seguinte, com o **mesmo** SPS e PPS, tenta de novo. Antes, o par ficava guardado da tentativa
    /// que falhou, e nenhum IDR com o mesmo par tentava: nada mais saía até o SPS mudar.
    func testeUmaRecusaDaSessaoTentaDeNovoNoIdrSeguinte() throws {
        let quadros = try Self.codificar(quadros: 60, largura: 320, altura: 180)
        let idrs = quadros.enumerated().filter { $0.element.idr }.map(\.offset)
        XCTAssertGreaterThanOrEqual(idrs.count, 2, "o encoder de bancada pede um IDR a cada 30 quadros")
        let decodificador = DecodificadorH264 { _, _ in }
        defer { decodificador.fechar() }
        decodificador.falharAsProximasCriacoes = 1
        for q in quadros {
            q.bytes.withUnsafeBytes { decodificador.alimentar(annexb: $0, timestampUs: q.timestampUs, idr: q.idr) }
        }
        let i = decodificador.instantaneo()
        XCTAssertEqual(i.falhasDeSessao, 1)
        XCTAssertEqual(i.sessoesCriadas, 1, "o IDR seguinte montou a sessão")
        XCTAssertEqual(i.decodificados, UInt64(quadros.count - idrs[1]),
                       "tudo do segundo IDR em diante sai; antes, nada saía")
    }

    /// **Quadro P antes de qualquer SPS é descartado, e contado como `semParametros`.**
    ///
    /// Alimentar o decodificador com um quadro P sem parâmetros é o caminho conhecido para travá-lo
    /// de vez — achado do receptor Windows no M2, repetido pelo Android. E o contador tem de ser
    /// `semParametros`, não `recusados`: os dois nomes mandam a investigação para lados opostos.
    func testeQuadroAntesDoSpsEDescartadoENomeadoCerto() {
        let decodificador = DecodificadorH264 { _, _ in
            XCTFail("não pode sair quadro nenhum sem SPS")
        }
        defer { decodificador.fechar() }

        // NAL tipo 1 (não-IDR) com carga qualquer: nunca chega ao VideoToolbox.
        let p: [UInt8] = [0, 0, 0, 1, 0x41, 0x9A, 0x00, 0x11]
        p.withUnsafeBytes { buf in
            decodificador.alimentar(annexb: buf, timestampUs: 1000, idr: false)
        }

        let i = decodificador.instantaneo()
        XCTAssertEqual(i.recebidos, 1)
        XCTAssertEqual(i.semParametros, 1)
        XCTAssertEqual(i.recusados, 0, "não foi o decodificador que recusou: não havia decodificador")
        XCTAssertEqual(i.decodificados, 0)
        XCTAssertEqual(i.sessoesCriadas, 0)
    }

    /// Um instantâneo de decodificador que nunca viu nada tem de ser todo zero — e o `n` tem de
    /// ser 0, e não 1 com um percentil inventado. "Não medi" e "medi zero" precisam ser
    /// distinguíveis por quem lê o relatório.
    func testeInstantaneoVazioNaoInventaPercentil() {
        let decodificador = DecodificadorH264 { _, _ in }
        let i = decodificador.instantaneo()
        XCTAssertEqual(i.n, 0)
        XCTAssertEqual(i.p50Us, 0)
        XCTAssertEqual(i.recebidos, 0)
        XCTAssertEqual(i.perfil, "")
    }

    // --- o Exibidor ----------------------------------------------------------------------------

    /// A camada solta da árvore é **invisível**, e o instantâneo tem de dizer isso.
    ///
    /// Uma `AVSampleBufferDisplayLayer` de quadro zero, ou fora de qualquer árvore de camadas,
    /// aceita todo quadro e não desenha nada: `enqueue` devolve normal, `isReadyForMoreMediaData`
    /// segue `true`, `status` nunca vira `.failed`. Foi o defeito que custou o dia inteiro no
    /// receptor iOS — `recebidos 435 · exibidos 435` com a tela **preta**. Este teste afirma que o
    /// contador que teria respondido na primeira leitura existe e responde.
    func testeCamadaForaDaArvoreSeDeclaraInvisivel() throws {
        let exibidor = Exibidor()
        let imagem = try TestesDaMarca.bufferComMarca(valor: 3, largura: 320, altura: 180)
        exibidor.oferecer(imagem)

        let i = exibidor.instantaneo()
        XCTAssertEqual(i.ofertados, 1)
        XCTAssertFalse(i.camadaNaArvore, "não há vista nenhuma neste alvo de teste")
        XCTAssertTrue(i.camadaInvisivel,
                      "sem árvore e sem área, a camada não desenha — e o contador tem de acusar")
    }

    /// `reiniciar()` zera os contadores e **larga a descrição de formato**. A descrição é a
    /// geometria da sessão anterior: mantê-la faria o primeiro quadro de uma origem com outra
    /// dimensão ser oferecido com a descrição errada.
    func testeReiniciarZeraOEscopoDaSessao() throws {
        let exibidor = Exibidor()
        let imagem = try TestesDaMarca.bufferComMarca(valor: 1, largura: 320, altura: 180)
        exibidor.oferecer(imagem)
        XCTAssertEqual(exibidor.instantaneo().ofertados, 1)

        exibidor.reiniciar()
        XCTAssertEqual(exibidor.instantaneo().ofertados, 0)
        XCTAssertEqual(exibidor.instantaneo().enfileirados, 0)

        // Uma origem de outra dimensão depois do reinício não pode reaproveitar a descrição velha.
        let outra = try TestesDaMarca.bufferComMarca(valor: 2, largura: 640, altura: 360)
        exibidor.oferecer(outra)
        XCTAssertEqual(exibidor.instantaneo().semDescricao, 0)
    }

    // --- o encoder de bancada -------------------------------------------------------------------

    struct QuadroCodificado {
        let bytes: [UInt8]
        let idr: Bool
        let timestampUs: UInt64
    }

    /// Codifica `quadros` quadros com a régua em H.264 Annex-B, do mesmo jeito que
    /// `apps/ios/Receptor/Ferramentas/gerar-fonte.swift`: todo IDR leva SPS e PPS junto, que é
    /// exigência do `docs/contrato-sidecar.md` e é o que permite a quem entra no meio da sessão
    /// montar a primeira imagem.
    static func codificar(quadros: Int, largura: Int, altura: Int) throws -> [QuadroCodificado] {
        var sessao: VTCompressionSession?
        let criou = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: Int32(largura), height: Int32(altura),
            codecType: kCMVideoCodecType_H264,
            encoderSpecification: nil, imageBufferAttributes: nil,
            compressedDataAllocator: nil, outputCallback: nil, refcon: nil,
            compressionSessionOut: &sessao)
        XCTAssertEqual(criou, noErr)
        let s = try XCTUnwrap(sessao)
        defer { VTCompressionSessionInvalidate(s) }

        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel,
                             value: kVTProfileLevel_H264_Baseline_AutoLevel)
        // Sem reordenação: é o que os quatro emissores do projeto configuram, e é o que faz a
        // ordem de saída ser a ordem de entrada — sem isso a régua sairia fora de ordem e o teste
        // culparia o decodificador.
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering,
                             value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameInterval,
                             value: NSNumber(value: 30))
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate,
                             value: NSNumber(value: 4_000_000))

        var saida: [QuadroCodificado] = []
        let trava = NSLock()

        for n in 0..<quadros {
            let pixel = try TestesDaMarca.bufferComMarca(valor: n % Marca.modulo,
                                                         largura: largura, altura: altura)
            let tempo = CMTime(value: CMTimeValue(n) * 1000, timescale: 30_000)
            var bandeiras = VTEncodeInfoFlags()
            let estado = VTCompressionSessionEncodeFrame(
                s, imageBuffer: pixel, presentationTimeStamp: tempo,
                duration: CMTime(value: 1000, timescale: 30_000),
                frameProperties: nil, infoFlagsOut: &bandeiras
            ) { _, _, amostra in
                guard let amostra, let q = Self.annexbDe(amostra) else { return }
                trava.lock()
                saida.append(QuadroCodificado(
                    bytes: q.0, idr: q.1,
                    timestampUs: UInt64(max(0, CMSampleBufferGetPresentationTimeStamp(amostra)
                        .convertScale(1_000_000, method: .default).value))))
                trava.unlock()
            }
            XCTAssertEqual(estado, noErr)
        }
        VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid)
        trava.lock(); let resultado = saida; trava.unlock()
        return resultado
    }

    /// AVCC → Annex-B, com SPS e PPS na frente de todo quadro-chave.
    static func annexbDe(_ amostra: CMSampleBuffer) -> ([UInt8], Bool)? {
        guard let bloco = CMSampleBufferGetDataBuffer(amostra) else { return nil }
        var total = 0
        var ponteiro: UnsafeMutablePointer<CChar>?
        guard CMBlockBufferGetDataPointer(bloco, atOffset: 0, lengthAtOffsetOut: nil,
                                          totalLengthOut: &total,
                                          dataPointerOut: &ponteiro) == noErr,
              let ponteiro else { return nil }

        var chave = true
        if let anexos = CMSampleBufferGetSampleAttachmentsArray(amostra, createIfNecessary: false)
            as? [[CFString: Any]], let primeiro = anexos.first {
            chave = !((primeiro[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
        }

        let start: [UInt8] = [0, 0, 0, 1]
        var out = [UInt8]()

        if chave, let formato = CMSampleBufferGetFormatDescription(amostra) {
            var quantos = 0
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                formato, parameterSetIndex: 0, parameterSetPointerOut: nil,
                parameterSetSizeOut: nil, parameterSetCountOut: &quantos,
                nalUnitHeaderLengthOut: nil)
            for i in 0..<quantos {
                var p: UnsafePointer<UInt8>?
                var tamanho = 0
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    formato, parameterSetIndex: i, parameterSetPointerOut: &p,
                    parameterSetSizeOut: &tamanho, parameterSetCountOut: nil,
                    nalUnitHeaderLengthOut: nil) == noErr, let p {
                    out.append(contentsOf: start)
                    out.append(contentsOf: UnsafeBufferPointer(start: p, count: tamanho))
                }
            }
        }

        var i = 0
        ponteiro.withMemoryRebound(to: UInt8.self, capacity: total) { bytes in
            while i + 4 <= total {
                let n = Int(bytes[i]) << 24 | Int(bytes[i + 1]) << 16
                    | Int(bytes[i + 2]) << 8 | Int(bytes[i + 3])
                i += 4
                if n <= 0 || i + n > total { break }
                out.append(contentsOf: start)
                out.append(contentsOf: UnsafeBufferPointer(start: bytes + i, count: n))
                i += n
            }
        }
        return (out, chave)
    }
}
