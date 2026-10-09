import Foundation

/// **A distribuição dos intervalos entre apresentações** — o número que faltava para "sem fluidez"
/// deixar de ser impressão.
///
/// ## Por que a média não serve, e por que ela existia
///
/// Em 01/09/2026 o usuário olhou para a tela e disse que faltava fluidez. A média de `fila→tela`
/// da mesma corrida dizia **6,4 ms** e estava **certa** — e não respondia nada: o pior caso era
/// **226 ms**, e é nele que a pessoa vê a imagem parar. Média não vê tranco: um segundo com 29
/// quadros pontuais e um buraco de 200 ms tem a mesma média de um segundo regular.
///
/// O que se vê é o **intervalo entre um quadro e o seguinte na tela**, e o que descreve isso é a
/// distribuição dele, não o centro. Com o instrumento pronto, o mesmo A10s deu:
///
///     Wi-Fi 2,4 GHz : fluidez_ms=[n=2541 p50=32 p95=115 max=241]  trancos=208
///     cabo USB      : fluidez_ms=[n=2639 p50=34 p95=39  max=83 ]  trancos=0
///
/// `p50` idêntico nos dois. Toda a diferença está na cauda, e é ela que o olho vê.
///
/// ## O intervalo é entre **apresentações**, e isso é escolha
///
/// Não entre chegadas, não entre decodificações: entre os instantes em que um quadro de fato foi
/// entregue à camada de exibição. É o único ponto do caminho que corresponde ao que o olho recebe,
/// e é por isso que ele mede também o custo das políticas desta casca — a porta que segura o
/// quadro condenado aparece aqui como intervalo maior, que é exatamente o que ela custa e o que
/// precisava ficar visível. Medir chegadas esconderia a porta.
///
/// ## Honestidade obrigatória: "entregue", não "apareceu"
///
/// Quem põe o pixel no vidro é a camada do sistema, **depois** de nós entregarmos. O instante que
/// entra aqui é *"a hora em que este quadro foi entregue"*, não *"a hora em que ele apareceu"* —
/// nenhuma API do macOS confirma a segunda. É a mesma ressalva que já governa o nome
/// ``Exibidor/instantaneo()``.`enfileirados`, e ela está escrita aqui porque um número que se
/// apresenta como uma coisa e é outra já custou uma semana a este projeto (`packets_missing`).
///
/// ## `trancos` é convenção de comparação, não afirmação perceptual
///
/// A 30 fps o orçamento é 33 ms. `trancos` conta os intervalos acima de ``trancoMs`` — três tempos
/// de quadro. **Não** se está afirmando que 100 ms é o limiar em que uma pessoa percebe; o que se
/// afirma é que duas corridas com o mesmo emissor e a mesma origem podem ser comparadas por esse
/// número. Quem quiser outro corte tem os percentis ao lado.
///
/// ## Cópia declarada
///
/// A peça de referência é `apps/windows/src/fluidez.rs`, e o contrato — os nomes, o corte, a
/// âncora, o teto — está escrito lá. Esta é a mesma aritmética em Swift, e há uma terceira cópia em
/// `integrations/camera-macos/Fontes/App/Fluidez.swift`, que mede outro ponto (a entrega ao
/// sumidouro da câmera) e diz isso na própria doc. Mesma razão de `Medidas` e `JanelaDoEnlace`
/// serem cópias: os alvos de compilação não se encontram, e um pacote Swift para setenta linhas
/// custaria mais do que resolve. O que amarra as cópias é o formato da linha, que é o mesmo texto
/// em todas.
///
/// ## Por que ela mora em `QuallReceptorKit`
///
/// Porque este alvo **tem suíte** (`Tests/QuallReceptorKitTests`) e não depende de `CQuall`. A peça
/// é aritmética pura sobre instantes que o chamador já leu: ela nunca toca a fronteira C nem o
/// AVFoundation, e é por isso que a âncora, o corte, o teto e a forma da linha podem ser provados
/// sem rede, sem `libquall.a` e sem aparelho.
public struct Fluidez {

    /// O corte de `trancos`: três tempos de quadro a 30 fps. Ver a doc do tipo — é **convenção de
    /// comparação**, e a distribuição completa sai ao lado para quem quiser outro corte.
    public static let trancoMs: UInt64 = 100

    /// Teto de amostras guardadas. A 30 fps são ~5 minutos de sessão; passado isso a distribuição
    /// para de crescer em vez de a sessão longa comer memória. Mesmo teto da peça do Windows.
    ///
    /// O que passa do teto é **dito** — ver o sufixo em ``linha`` —, e não fingido: um `n` que se
    /// apresenta como a sessão inteira quando não é seria o mesmo defeito de sempre, com outra
    /// roupa.
    static let maximoDeAmostras = 10_000

    /// O instante da apresentação anterior, em microssegundos de `CLOCK_UPTIME_RAW`. `nil` até a
    /// primeira — ver ``apresentou(agoraUs:)``.
    private var anteriorUs: UInt64?
    private var intervalosUs: [UInt64] = []
    /// Quantas amostras foram descartadas por teto.
    private var descartadas: UInt64 = 0

    public init() {}

    /// Marca que um quadro foi **entregue** à camada de exibição agora.
    ///
    /// A primeira chamada **só ancora**: não existe intervalo antes do primeiro quadro, e contar o
    /// tempo desde a abertura da sessão como se fosse um intervalo poria a subida do ICE e o
    /// primeiro IDR dentro da distribuição da imagem — um `max` de segundos em toda corrida
    /// saudável, que é um instrumento que ninguém lê duas vezes.
    ///
    /// A subtração é **saturante**. O relógio é monotônico e não deveria andar para trás, mas uma
    /// casca que assume isso e erra publica dezoito quintilhões em vez de um zero.
    public mutating func apresentou(agoraUs: UInt64) {
        if let antes = anteriorUs {
            let us = agoraUs > antes ? agoraUs - antes : 0
            if intervalosUs.count < Fluidez.maximoDeAmostras {
                intervalosUs.append(us)
            } else {
                descartadas &+= 1
            }
        }
        anteriorUs = agoraUs
    }

    /// Zera para uma sessão nova, **âncora inclusive**.
    ///
    /// Existe pelo mesmo motivo que ``Exibidor/reiniciar()``: o exibidor vive o processo inteiro e
    /// as sessões não. Sem largar a âncora, o intervalo entre o último quadro de uma sessão e o
    /// primeiro da seguinte — que é o tempo de a pessoa digitar um endereço — entraria na
    /// distribuição como o maior tranco da corrida.
    public mutating func reiniciar() {
        anteriorUs = nil
        intervalosUs.removeAll(keepingCapacity: true)
        descartadas = 0
    }

    /// Quantos intervalos passaram de ``trancoMs``. Corte **estrito**: exatamente no limiar não
    /// conta. Fica fixado para que a comparação entre duas corridas não dependa de arredondamento.
    public var trancos: UInt64 {
        UInt64(intervalosUs.lazy.filter { $0 > Fluidez.trancoMs * 1_000 }.count)
    }

    /// `fluidez_ms=[n p50 p95 max] trancos=…`, em milissegundos.
    ///
    /// Quatro números e não um, de propósito, e na mesma forma de `sem_referencia_ms`: numa medida
    /// de dano visual **a cauda é o dano**, e este repositório já pagou por relatório que mostrava
    /// só o centro.
    ///
    /// O índice do percentil é `floor(q · (n-1))`, que é literalmente o da peça do Windows. É
    /// **diferente** do arredondamento de `Receptor.resumo` no app, e a diferença é deliberada:
    /// o número desta linha é comparado entre plataformas, então ele segue a referência do
    /// contrato e não a convenção local.
    public var linha: String {
        let sufixo = descartadas > 0 ? " (+\(descartadas) além do teto)" : ""
        guard !intervalosUs.isEmpty else {
            return "fluidez_ms=[n=0 p50=0 p95=0 max=0] trancos=0" + sufixo
        }
        let ordenados = intervalosUs.sorted()
        func q(_ p: Double) -> Double {
            let i = min(Int(p * Double(ordenados.count - 1)), ordenados.count - 1)
            return Double(ordenados[i]) / 1000
        }
        return String(format: "fluidez_ms=[n=%d p50=%.0f p95=%.0f max=%.0f] trancos=%llu%@",
                      ordenados.count, q(0.50), q(0.95),
                      Double(ordenados[ordenados.count - 1]) / 1000,
                      trancos, sufixo)
    }
}
