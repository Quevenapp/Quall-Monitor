import CoreVideo
import XCTest
@testable import QuallReceptorKit

/// A régua de blocos é a única testemunha, por contador, de que os **pixels certos** chegaram. Se
/// ela divergir do gerador, o sintoma é "decodifica e a marca nunca bate" — que aponta o dedo para
/// o decodificador, e manda quem investiga para o lugar errado.
///
/// Por isso os números abaixo são **literais**, e não `Marca.algumaCoisa`: um teste que lê a
/// constante que ele deveria estar conferindo não confere nada. Eles são os valores de
/// `apps/ios/Receptor/Ferramentas/gerar-fonte.swift`, que é quem desenha.
final class TestesDaMarca: XCTestCase {

    func testeOsValoresBatemComOsDoGerador() {
        XCTAssertEqual(Marca.digitos, 4, "o gerador desenha quatro blocos")
        XCTAssertEqual(Marca.modulo, 256, "4^4 — o gerador conta o quadro módulo isto")
        XCTAssertEqual(Marca.lado, 64, "o gerador desenha blocos de 64x64")
        for d in 0..<4 {
            XCTAssertEqual(Marca.luma(digito: d), UInt8(40 + d * 50),
                           "a luma do dígito \(d) é a mesma dos dois lados")
        }
    }

    /// O espaçamento de 50 níveis é o que faz a régua sobreviver à quantização do H.264. Este
    /// teste afirma a **margem**, não a fórmula: com erro de ±20 níveis os quatro dígitos
    /// continuam separáveis, e um espaçamento menor deixaria de valer sem nada acusar.
    func testeOsQuatroDigitosSeparamComErroDeVinteNiveis() {
        for d in 0..<4 {
            let alvo = Int(Marca.luma(digito: d))
            for erro in [-20, -10, 0, 10, 20] {
                XCTAssertEqual(Marca.digito(luma: alvo + erro), d,
                               "luma \(alvo + erro) devia ler como dígito \(d)")
            }
        }
    }

    func testeLumaLongeDeTodosNaoViraDigito() {
        // 40, 90, 140, 190 são os quatro níveis; 240 está a 50 do mais próximo.
        XCTAssertNil(Marca.digito(luma: 240))
        XCTAssertNil(Marca.digito(luma: 0))
    }

    // --- a leitura da janela recapturada (T1 da S7) -------------------------------------------

    /// Uma imagem BGRA como a que o ScreenCaptureKit entrega da janela: fundo de outra cor, a
    /// régua deslocada e reduzida, e os níveis **como a tela os mostra** (faixa cheia).
    static func janelaComMarca(valor: Int, largura: Int, altura: Int, x0: Int, y0: Int,
                               lado: Int, fundo: UInt8 = 12, desvio: Int = 0) -> [UInt8] {
        var p = [UInt8](repeating: fundo, count: largura * altura * 4)
        var v = valor
        for d in 0..<4 {
            let luma = 40 + (v % 4) * 50
            v /= 4
            // A tela leva a faixa de vídeo à faixa cheia: 16 → 0 e 235 → 255.
            let exibido = UInt8(max(0, min(255, Int((Double(luma - 16) * 255 / 219).rounded()) + desvio)))
            for y in y0..<(y0 + lado) {
                for x in (x0 + d * lado)..<(x0 + (d + 1) * lado) {
                    let i = (y * largura + x) * 4
                    p[i] = exibido; p[i + 1] = exibido; p[i + 2] = exibido; p[i + 3] = 255
                }
            }
        }
        return p
    }

    func testeLeAReguaDaJanelaRecapturadaEmVariasEscalas() {
        for (lado, x0, y0) in [(64, 0, 64), (32, 17, 57), (48, 5, 33), (21, 3, 70)] {
            for valor in [0, 1, 42, 137, 255] {
                let (l, a) = (x0 + 4 * lado + 30, y0 + lado + 20)
                let p = Self.janelaComMarca(valor: valor, largura: l, altura: a, x0: x0, y0: y0, lado: lado)
                let lido = p.withUnsafeBytes {
                    Marca.ler(bgra: $0.baseAddress!, bytesPorLinha: l * 4, largura: l, altura: a,
                              x0: Double(x0), y0: Double(y0), lado: Double(lado))
                }
                XCTAssertEqual(lido, valor, "lado \(lado), em (\(x0), \(y0))")
            }
        }
    }

    /// A gama da composição pode puxar os níveis do meio uns pontos: com ±10 na faixa cheia os
    /// quatro dígitos continuam separáveis; fora da régua (fundo chapado) nada é lido.
    func testeAReguaDaJanelaAguentaDesvioELugarErradoNaoLe() {
        for desvio in [-10, -5, 5, 10] {
            let p = Self.janelaComMarca(valor: 201, largura: 300, altura: 120, x0: 10, y0: 20,
                                        lado: 40, desvio: desvio)
            let lido = p.withUnsafeBytes {
                Marca.ler(bgra: $0.baseAddress!, bytesPorLinha: 1200, largura: 300, altura: 120,
                          x0: 10, y0: 20, lado: 40)
            }
            XCTAssertEqual(lido, 201, "desvio \(desvio)")
        }
        let p = Self.janelaComMarca(valor: 201, largura: 300, altura: 120, x0: 10, y0: 20, lado: 40,
                                    fundo: 250)
        let fora = p.withUnsafeBytes {
            Marca.ler(bgra: $0.baseAddress!, bytesPorLinha: 1200, largura: 300, altura: 120,
                      x0: 100, y0: 70, lado: 40)
        }
        XCTAssertNil(fora, "o bloco fora da régua não pode virar número")
        let saindo = p.withUnsafeBytes {
            Marca.ler(bgra: $0.baseAddress!, bytesPorLinha: 1200, largura: 300, altura: 120,
                      x0: 200, y0: 20, lado: 40)
        }
        XCTAssertNil(saindo, "a régua que sai da imagem não é lida")
    }

    // --- a leitura de um buffer de verdade ----------------------------------------------------

    func testeLeODeUmBufferDesenhadoAqui() throws {
        for valor in [0, 1, 42, 137, 255] {
            let buffer = try Self.bufferComMarca(valor: valor, largura: 640, altura: 360)
            XCTAssertEqual(Marca.ler(de: buffer), valor)
        }
    }

    /// Um quadro chapado num nível **longe dos quatro** não é lido: é o caso normal de qualquer
    /// origem que não seja o gerador de bancada, e não é erro.
    func testeBufferChapadoLongeDosNiveisNaoEhLido() throws {
        let buffer = try Self.bufferChapado(luma: 240, largura: 640, altura: 360)
        XCTAssertNil(Marca.ler(de: buffer))
    }

    /// **A régua NÃO é uma soma de verificação, e este teste existe para dizer isso em voz alta.**
    ///
    /// Escrito esperando `nil` para um quadro cinza de luma 128 — e ele devolveu **170**. A causa é
    /// aritmética e está certa: 128 fica a 12 níveis de 140, que é o dígito 2, dentro da tolerância
    /// de 21 que a quantização exige. Quatro blocos lendo 2 dão 2+8+32+128 = 170.
    ///
    /// A consequência importa para quem lê o relatório: `marca_ausentes` **não** é a única saída
    /// para uma origem que não carrega a régua. Um fundo chapado num tom vizinho de um dos quatro
    /// níveis vira um número — mas vira **sempre o mesmo** número, e a classificação de três
    /// classes o chama de `.repetida` de todas as vezes depois da primeira. É por isso que
    /// `marca_certas` é o contador que se lê como prova, e não `marca_ausentes == 0`: só ele exige
    /// que a régua **avance**, e um quadro parado não avança.
    func testeUmQuadroChapadoViraUmNumeroConstanteEIssoNaoEhProgresso() throws {
        let buffer = try Self.bufferChapado(luma: 128, largura: 640, altura: 360)
        let primeira = Marca.ler(de: buffer)
        XCTAssertEqual(primeira, 170, "128 lê como dígito 2 nos quatro blocos")

        // A segunda leitura do mesmo quadro chapado dá o mesmo número, e a classificação o recusa
        // como avanço: nenhuma sequência de quadros chapados produz `.certa` mais de uma vez.
        let segunda = try XCTUnwrap(Marca.ler(de: buffer))
        XCTAssertEqual(Marca.classificar(anterior: primeira, atual: segunda), .repetida)
    }

    /// Um quadro menor que a régua não pode ser lido, e não pode explodir.
    func testeBufferPequenoDemaisDaNil() throws {
        let buffer = try Self.bufferChapado(luma: 40, largura: 128, altura: 32)
        XCTAssertNil(Marca.ler(de: buffer))
    }

    // --- a classificação de três classes -------------------------------------------------------

    /// **Três classes, e a do meio existe porque a primeira medição do iPad a produziu.** Chamar
    /// a repetição de erro teria reprovado o produto por ele estar certo: o pedido de IDR de
    /// entrada faz o emissor reenviar o último IDR já emitido, que é um quadro *anterior*.
    func testeAsTresClasses() {
        XCTAssertEqual(Marca.classificar(anterior: nil, atual: 7), .certa, "a primeira de todas")
        XCTAssertEqual(Marca.classificar(anterior: 10, atual: 11), .certa)
        XCTAssertEqual(Marca.classificar(anterior: 250, atual: 3), .certa, "a volta do módulo")
        XCTAssertEqual(Marca.classificar(anterior: 10, atual: 10), .repetida, "passo zero")
        XCTAssertEqual(Marca.classificar(anterior: 10, atual: 9), .repetida, "um quadro para trás")
        XCTAssertEqual(Marca.classificar(anterior: 3, atual: 250), .repetida, "para trás pelo módulo")
        XCTAssertEqual(Marca.classificar(anterior: 10, atual: 120), .errada, "salto de 110")
    }

    /// A fronteira exata do passo plausível. Sem este teste, mudar `passoPlausivel` mudaria a
    /// classificação de corridas inteiras sem nada acusar.
    func testeAFronteiraDoPassoPlausivel() {
        XCTAssertEqual(Marca.classificar(anterior: 0, atual: 90), .certa, "90 ainda é plausível")
        XCTAssertEqual(Marca.classificar(anterior: 0, atual: 91), .errada, "91 já é perda")
    }

    // --- fábricas ------------------------------------------------------------------------------

    /// Desenha a régua do mesmo jeito que `gerar-fonte.swift`: dígito base 4, do menos ao mais
    /// significativo, da esquerda para a direita.
    static func bufferComMarca(valor: Int, largura: Int, altura: Int) throws -> CVPixelBuffer {
        let buffer = try bufferChapado(luma: 128, largura: largura, altura: altura)
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let base = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(buffer, 0))
            .assumingMemoryBound(to: UInt8.self)
        let passo = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        var resto = valor
        for d in 0..<Marca.digitos {
            let nivel = Marca.luma(digito: resto % 4)
            resto /= 4
            for linha in 0..<Marca.lado {
                let inicio = base + linha * passo + d * Marca.lado
                for x in 0..<Marca.lado { inicio[x] = nivel }
            }
        }
        return buffer
    }

    static func bufferChapado(luma: UInt8, largura: Int, altura: Int) throws -> CVPixelBuffer {
        var talvez: CVPixelBuffer?
        let estado = CVPixelBufferCreate(
            kCFAllocatorDefault, largura, altura,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary] as CFDictionary,
            &talvez)
        XCTAssertEqual(estado, kCVReturnSuccess)
        let buffer = try XCTUnwrap(talvez)
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let y = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(buffer, 0))
            .assumingMemoryBound(to: UInt8.self)
        let passoY = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        for linha in 0..<altura {
            memset(y + linha * passoY, Int32(luma), largura)
        }
        let cb = try XCTUnwrap(CVPixelBufferGetBaseAddressOfPlane(buffer, 1))
            .assumingMemoryBound(to: UInt8.self)
        let passoC = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        for linha in 0..<(altura / 2) {
            memset(cb + linha * passoC, 128, largura)
        }
        return buffer
    }
}
