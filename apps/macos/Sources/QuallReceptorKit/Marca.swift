import CoreVideo
import Foundation

/// A régua de blocos que a fonte sintética desenha no canto superior esquerdo, lida de volta do
/// quadro **decodificado**.
///
/// # Por que ela existe neste app
///
/// "O Mac exibiu vídeo" é uma afirmação sobre pixels, e um contador de quadros não a sustenta: um
/// decodificador pode entregar mil buffers cinza com todos os contadores fechando. A régua resolve
/// isso **por contador**: o emissor escreve o número do quadro em quatro blocos chapados de 64x64,
/// e o receptor lê a média do miolo de cada bloco e reconstrói o número. Se ele bate com um quadro
/// plausível, os pixels que saíram do decodificador são os pixels que entraram no encoder.
///
/// # A terceira cópia, e a divergência que ela pode produzir
///
/// Os valores abaixo são idênticos aos de `apps/ios/Receptor/Ferramentas/gerar-fonte.swift` (o
/// gerador) e aos de `apps/ios/Receptor/Comum/Marca.swift` (o leitor do iPad). São três cópias
/// porque os três lados não compartilham alvo de compilação. O risco é real e tem sintoma
/// conhecido: uma divergência apareceria como "decodifica e a marca nunca bate", que aponta o dedo
/// para o decodificador. `TestesDaMarca` confere os valores desta cópia contra os números
/// literais do gerador, para que a divergência seja suíte vermelha e não corrida perdida.
///
/// # O que ela NÃO é
///
/// Não é um caminho de produto. Numa origem que não seja o gerador de bancada, [`ler`] devolve
/// `nil` — e isso é o caso **normal**, não erro. Nada nesta classe olha conteúdo: ela reduz um
/// retângulo de 64x64 a uma média e a compara com quatro níveis fixos. O que sai é um inteiro.
public enum Marca {
    /// Quantos blocos, e portanto quantos dígitos base 4.
    public static let digitos = 4
    /// 4^4 = 256 valores distintos. A 30 fps a régua dá a volta em 8,5 s.
    public static let modulo = 256
    /// Lado de cada bloco, em pixels do quadro codificado.
    public static let lado = 64

    /// Luma do dígito `d` (0..3). Espaçamento de 50 é o que sobrevive à quantização.
    public static func luma(digito: Int) -> UInt8 { UInt8(40 + digito * 50) }

    /// O dígito mais próximo de uma luma lida. `nil` se nenhum estiver a menos de 21 níveis.
    public static func digito(luma: Int) -> Int? {
        var melhor = -1
        var erro = 21
        for d in 0..<4 {
            let e = abs(luma - Int(Marca.luma(digito: d)))
            if e < erro { erro = e; melhor = d }
        }
        return melhor >= 0 ? melhor : nil
    }

    /// Lê a régua de um `CVPixelBuffer` decodificado. `nil` quando o quadro não a carrega.
    ///
    /// Lê só o **miolo** de cada bloco (a metade central), porque a borda é onde o filtro de
    /// desbloqueio do H.264 mistura o bloco com o vizinho.
    public static func ler(de imagem: CVPixelBuffer) -> Int? {
        let largura = CVPixelBufferGetWidth(imagem)
        let altura = CVPixelBufferGetHeight(imagem)
        guard largura >= digitos * lado, altura >= lado else { return nil }

        CVPixelBufferLockBaseAddress(imagem, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(imagem, .readOnly) }

        // Plano 0 de um buffer bi-planar 420v/420f é a luma. Um buffer de plano único não é o que
        // o VideoToolbox entrega aqui, e ler o plano 0 dele daria um número sem sentido.
        guard CVPixelBufferGetPlaneCount(imagem) >= 2,
              let base = CVPixelBufferGetBaseAddressOfPlane(imagem, 0) else { return nil }
        let passo = CVPixelBufferGetBytesPerRowOfPlane(imagem, 0)
        let y = base.assumingMemoryBound(to: UInt8.self)

        var valor = 0
        var peso = 1
        let borda = lado / 4
        for d in 0..<digitos {
            var soma = 0
            var quantos = 0
            for linha in borda..<(lado - borda) {
                let linhaBase = y + linha * passo + d * lado
                for x in borda..<(lado - borda) {
                    soma += Int(linhaBase[x])
                    quantos += 1
                }
            }
            guard quantos > 0, let digito = Marca.digito(luma: soma / quantos) else { return nil }
            valor += digito * peso
            peso *= 4
        }
        return valor
    }

    /// A luma de vídeo (faixa 16–235) de um cinza **exibido** `v` (0–255, faixa cheia).
    ///
    /// O VideoToolbox decodifica em faixa de vídeo, e a composição da janela leva a luma 16 ao
    /// preto (0) e a 235 ao branco (255). Os quatro níveis da régua (40, 90, 140, 190) saem na
    /// tela como ~28, 86, 145 e 203; esta conta os traz de volta à escala de `digito(luma:)`.
    public static func lumaDeVideo(exibida v: Double) -> Int {
        Int((16 + v * 219 / 255).rounded())
    }

    /// Lê a régua de uma imagem **BGRA recapturada da janela** (T1 do `docs/som-no-receptor.md`
    /// §9.4, a S7): a janela de vídeo do próprio app, pelo ScreenCaptureKit.
    ///
    /// Diferente de [`ler(de:)`], aqui a régua não está no pixel (0,0) nem em blocos de 64: o
    /// vídeo é exibido com `resizeAspect` numa vista dentro da janela, e a imagem capturada é a
    /// janela inteira, com a barra de título. Quem chama diz onde o canto de cima à esquerda do
    /// vídeo caiu (`x0`, `y0`, em pixels da imagem) e o lado de um bloco (`lado`, também em
    /// pixels). Lê o miolo de cada bloco (a metade central), como a leitura do quadro decodificado.
    /// `nil` quando algum bloco sai da imagem ou não está perto de nenhum dos quatro níveis.
    public static func ler(bgra base: UnsafeRawPointer, bytesPorLinha passo: Int, largura: Int,
                           altura: Int, x0: Double, y0: Double, lado: Double) -> Int? {
        guard lado >= 4 else { return nil }
        let p = base.assumingMemoryBound(to: UInt8.self)
        var valor = 0
        var peso = 1
        for d in 0..<digitos {
            let xa = Int((x0 + (Double(d) + 0.25) * lado).rounded())
            let xb = Int((x0 + (Double(d) + 0.75) * lado).rounded())
            let ya = Int((y0 + 0.25 * lado).rounded())
            let yb = Int((y0 + 0.75 * lado).rounded())
            guard xa >= 0, ya >= 0, xb <= largura, yb <= altura, xb > xa, yb > ya else { return nil }
            var soma = 0.0
            var quantos = 0
            for y in ya..<yb {
                let linha = p + y * passo
                for x in xa..<xb {
                    // BGRA: azul, verde, vermelho. Os blocos são cinza; a luma Rec. 709 dá o
                    // mesmo número num cinza e não inventa nível numa cor.
                    let b = Double(linha[x * 4]), g = Double(linha[x * 4 + 1])
                    let r = Double(linha[x * 4 + 2])
                    soma += 0.2126 * r + 0.7152 * g + 0.0722 * b
                    quantos += 1
                }
            }
            guard quantos > 0,
                  let digito = Marca.digito(luma: lumaDeVideo(exibida: soma / Double(quantos)))
            else { return nil }
            valor += digito * peso
            peso *= 4
        }
        return valor
    }

    /// Como classificar o passo entre a marca anterior e a atual.
    ///
    /// **Três classes, e não duas.** A segunda existe porque a primeira medição do receptor iOS a
    /// produziu, e chamá-la de erro teria reprovado o produto por ele estar certo: o pedido de IDR
    /// de entrada faz o emissor reenviar o **último IDR já emitido**, que é um quadro anterior ao
    /// da vez. Ver `docs/receptor-ios.md`.
    public enum Classe {
        /// Avança dentro de um passo plausível, ou é a primeira de todas.
        case certa
        /// Passo zero ou para trás. **Comportamento desenhado**, não defeito.
        case repetida
        /// Salto para a frente maior que o plausível: houve perda, e `packets_lost_for_real` é a
        /// segunda testemunha.
        case errada
    }

    /// O maior salto para a frente que ainda se lê como "a sessão andou": três segundos a 30 fps.
    public static let passoPlausivel = 90

    public static func classificar(anterior: Int?, atual: Int) -> Classe {
        guard let anterior else { return .certa }
        let passo = (atual - anterior + modulo) % modulo
        if passo >= 1 && passo <= passoPlausivel { return .certa }
        if passo == 0 || passo > modulo - passoPlausivel { return .repetida }
        return .errada
    }
}
