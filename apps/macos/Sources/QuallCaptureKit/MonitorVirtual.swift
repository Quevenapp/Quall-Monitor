import CMonitorVirtual
import CoreGraphics
import Foundation
import QuallIdiomaKit

/// O monitor que a tela estendida cria: quantos pixels, quantos hertz, e em que escala o macOS
/// desenha nele.
///
/// # Pixels e pontos são perguntas diferentes
///
/// Os **pixels** são os do painel do aparelho que vai exibir — é o que atravessa a rede e o que o
/// receptor desenha 1:1. A **escala** é como o macOS usa esses pixels:
///
/// | escala | o macOS desenha | espaço de trabalho | letra |
/// |---|---|---|---|
/// | `.dobro` (2x, HiDPI) | 960 × 600 pontos sobre 1920 × 1200 pixels | pequeno | do tamanho da do Sidecar |
/// | `.umPraUm` (1x) | 1920 × 1200 pontos sobre 1920 × 1200 pixels | grande | metade do tamanho |
///
/// Os dois modos têm **os mesmos pixels por baixo**, então a captura sai 1:1 nos dois, e a pessoa
/// pode trocar de um para o outro em Ajustes > Monitores com a sessão aberta. O padrão é 2x porque
/// é o que a Apple escolhe para o iPad no Sidecar (exatamente o dobro), e porque 1x num painel de
/// ~206 ppi deixa o texto do tamanho que ninguém lê a um braço de distância. É escolha de olho, e a
/// bancada decide — `docs/tela-estendida.md`.
public struct ModoDoMonitorVirtual: Hashable, Sendable {
    public enum Escala: String, Hashable, Sendable, CaseIterable {
        case umPraUm = "1x"
        case dobro = "2x"
    }

    public let larguraEmPixels: Int
    public let alturaEmPixels: Int
    /// A taxa de atualização do monitor. **Não é o fps da transmissão**: o monitor nasce com o
    /// dobro dele (`hertzDoMonitor(paraFps:)`).
    public let hertz: Int
    /// O fps da transmissão feita deste monitor — nunca acima de `hertz`. Sem ele no `init`, é o
    /// próprio `hertz`, como era antes de os dois se separarem (11/09).
    public let fps: Int
    public let escala: Escala
    /// **Os pixels que vão para a rede** — os do painel do aparelho. São os do monitor, menos no 2x
    /// reduzido (`paraTela`): aí o monitor é maior que o painel e a captura o reduz para ele.
    public let larguraDaSaida: Int
    public let alturaDaSaida: Int

    public init(larguraEmPixels: Int, alturaEmPixels: Int, hertz: Int = 60, fps: Int? = nil,
                escala: Escala = .dobro, saida: (largura: Int, altura: Int)? = nil) {
        self.larguraEmPixels = larguraEmPixels
        self.alturaEmPixels = alturaEmPixels
        self.hertz = hertz
        self.fps = max(1, min(fps ?? hertz, hertz))
        self.escala = escala
        self.larguraDaSaida = saida?.largura ?? larguraEmPixels
        self.alturaDaSaida = saida?.altura ?? alturaEmPixels
    }

    /// O monitor é desenhado maior que o painel e reduzido na captura.
    public var reduzido: Bool { larguraDaSaida != larguraEmPixels || alturaDaSaida != alturaEmPixels }

    /// **O tamanho físico declarado é o que escolhe a escala com que o monitor nasce.**
    ///
    /// Medido em processos e identidades novas (`docs/tela-estendida.md`), nas duas receitas (modo
    /// em pixels em 10/09, em pontos em 11/09): com `hiDPI = 1` e o painel de 1920 × 1200, declarar
    /// 237 × 148 mm (~206 ppi, o tamanho real do painel do tablet) faz o macOS escolher 960 × 600 @2x;
    /// declarar 508 × 318 mm (~96 ppi) faz escolher 1920 × 1200 @1x. Em pontos, a 96 ppi, também
    /// 1920 × 1332, 1600 × 720, 1520 × 720 e 1334 × 750 nasceram 1x no painel.
    /// Nascer no modo certo evita **trocar** de modo — e troca de modo tem dois custos medidos: prende
    /// o monitor ao processo e deixa os outros processos sem ler o modo dele.
    ///
    /// Só os dois pontos foram medidos; a densidade de cada escala é a de cada ponto.
    ///
    /// **Isto decide só a primeira vez.** Com a mesma identidade (vendor, produto, série), o macOS
    /// lembra o último modo do monitor e ele vence o tamanho físico — medido. Quando a lembrança
    /// discorda da escala escolhida na tela inicial, `MonitorVirtual.criar` fixa a escolhida.
    ///
    /// **Com área mínima.** Um monitor declarado pequeno demais é criado e nunca fica online —
    /// medido em 11/09: o limite é a **área** (15.792 mm² não subiu, 16.643 subiu; 250 × 60 mm não
    /// subiu com a diagonal maior, 130 × 130 subiu com a menor). Foi o iPhone 7 de 10/09, 1334 × 750
    /// em 2x = 164 × 92 mm. Abaixo de `areaMinimaEmMm2`, os dois lados crescem na mesma proporção.
    public var milimetros: CGSize {
        let ppi = escala == .dobro ? 206.0 : 96.0
        let largura = Double(larguraEmPixels) * 25.4 / ppi, altura = Double(alturaEmPixels) * 25.4 / ppi
        guard largura * altura < ModoDoMonitorVirtual.areaMinimaEmMm2, largura > 0, altura > 0 else {
            return CGSize(width: largura.rounded(), height: altura.rounded())
        }
        let k = (ModoDoMonitorVirtual.areaMinimaEmMm2 / (largura * altura)).squareRoot()
        return CGSize(width: (largura * k).rounded(.up), height: (altura * k).rounded(.up))
    }

    /// Acima dos 16.643 mm² que subiram, com folga.
    static let areaMinimaEmMm2 = 17_000.0

    /// **O menor 2x que o macOS aceita como desktop**, em pontos. Medido em 11/09, com o modo
    /// declarado e oferecido: abaixo disto o 2x aparece na lista marcado como impróprio para
    /// desktop, o macOS não o escolhe, e fixá-lo dá `CGError 1001` — o erro de 10/09 com 1920 × 886.
    /// Largura: 768 não, 800 sim. Altura: 520 não, 530 sim (com 960 e com 1600 de largura). O
    /// tamanho físico declarado não muda isso (1920 × 884 a 96 e a 206 ppi, igual).
    public static let pisoDoDobroEmPontos = (largura: 800, altura: 530)

    /// Os pixels dão um 2x que o macOS aceita como desktop.
    public static func cabeEm2x(larguraPx: Int, alturaPx: Int) -> Bool {
        larguraPx / 2 >= pisoDoDobroEmPontos.largura && alturaPx / 2 >= pisoDoDobroEmPontos.altura
    }

    public var larguraEmPontos: Int { escala == .dobro ? larguraEmPixels / 2 : larguraEmPixels }
    public var alturaEmPontos: Int { escala == .dobro ? alturaEmPixels / 2 : alturaEmPixels }

    /// O tablet da bancada (`SM-X230`) deitado: painel de 1200 × 1920 em retrato natural
    /// (`docs/android-para-android.md`), 1920 × 1200 em paisagem.
    ///
    /// É o formato de quem não diz a tela no aperto de mão (Windows, OBS); quem diz ganha `paraTela`.
    /// Sem `hertz`, o monitor do fps (`hertzDoMonitor(paraFps:)`).
    public static func tabletDaBancada(escala: Escala = .dobro, fps: Int = ModoDoMonitorVirtual.fpsPadrao,
                                       hertz: Int? = nil) -> ModoDoMonitorVirtual {
        ModoDoMonitorVirtual(larguraEmPixels: 1920, alturaEmPixels: 1200,
                             hertz: hertz ?? hertzDoMonitor(paraFps: fps), fps: fps, escala: escala)
    }

    /// **30 fps por padrão, 60 por escolha.** Decisão do usuário em 10/09, depois da medida: a 60 fps
    /// o `SM-X230` descartou 31 quadros na caixa e mostrou 826 suspeitos em 4 min; a 30, mesma carga,
    /// nenhum (`docs/tela-estendida.md`). 60 fps fica para quem tem aparelho que aguenta. (O
    /// `decode_p50` do receptor Android é latência do pipeline, e **não** o custo de um quadro — a
    /// comparação dele com a verba de 16,7 ms, que estava aqui, caiu no mesmo dia.)
    public static let fpsPadrao = 30

    /// **O monitor nasce com o dobro do fps** (até 120 Hz). Medido em 11/09 (`docs/tela-estendida.md`,
    /// "Sem o Sidecar, o monitor anda na metade"): sem outra tela sendo composta sem parar — o iPad no
    /// Sidecar segurava —, o macOS põe no monitor virtual o que o **app** atualiza a cada quadro (um
    /// `draw`, uma camada movida, uma rolagem) na **metade** da taxa do monitor, em trechos ou a
    /// corrida inteira. Só o que o Core Animation anima sozinho passa inteiro. Com o monitor igual ao
    /// fps, a transmissão de 30 recebia 15–18 imagens novas por segundo; com o dobro, 29,2–29,4 — e
    /// 60 fps de um monitor de 120 Hz, 56,7. No iPhone X, às cegas, o usuário escolheu o de 60 Hz
    /// pela fluidez: 64 pausas acima de 50 ms contra 883. A causa do "metade" não é sabida.
    public static func hertzDoMonitor(paraFps fps: Int) -> Int { min(120, 2 * max(1, fps)) }

    /// O monitor do fps padrão.
    public static let hertzPadrao = hertzDoMonitor(paraFps: fpsPadrao)

    /// **O monitor para a tela de um aparelho** (os pixels que o receptor disse no aperto de mão,
    /// `quall_connect_with_screen`): deitado, com os pixels do painel (pares), na escala que a
    /// pessoa escolheu na tela inicial.
    ///
    /// - **1x**: o painel inteiro como espaço de trabalho, letra pequena.
    /// - **2x**: o 2x do painel quando o macOS o aceita como desktop (`cabeEm2x`). Quando não aceita
    ///   — telefone com painel de menos de 1060 px de altura —, o **2x reduzido**: o menor 2x que o
    ///   macOS aceita (`pisoDoDobroEmPontos`) na proporção do painel, e a captura reduz o monitor
    ///   para os pixels do painel. É o que o macOS faz nos modos "parece" das telas Retina: a letra
    ///   sai maior que em 1x, e a imagem deixa de ser 1:1.
    ///
    /// A regra saiu da medida de 11/09 (`docs/tela-estendida.md`, "A regra do 2x"), formato por
    /// formato:
    ///
    /// | aparelho | painel deitado | monitor em 2x |
    /// |---|---|---|
    /// | tablet `SM-X230` | 1920 × 1200 | 960 × 600 @2x |
    /// | iPad A16 | 2360 × 1640 | 1180 × 820 @2x |
    /// | S24 | 3120 × 1440 | 1560 × 720 @2x |
    /// | iPhone X | 2436 × 1125 | 1218 × 562 @2x (uma linha a menos, para ser par) |
    /// | A07 · A10s · iPhone 7 | 1600 × 720 · 1520 × 720 · 1334 × 750 | 2x reduzido: 1178 · 1119 · 943 × 530 @2x |
    ///
    /// **O teto é o do codificador, e não 1920 px.** O de 1920 vinha de "o macOS não gera 2x acima
    /// de 1920 px", frase que caiu em 11/09 — era a receita do monitor (o modo declarado em pixels), e
    /// não a plataforma: o `CGVirtualDisplay` subiu 5120 × 2880 @2x (medido). O que não cabe é o
    /// quadro no nível do núcleo (5.2, `macroblocosDoNivel`): acima dele o núcleo reduziria a imagem
    /// na codificação e o monitor sairia maior que o vídeo — o receptor do Mac com tela de 5K diz
    /// 5120 × 2880 (revisão de 11/09). Então os dois lados encolhem na mesma proporção até caber, e o
    /// monitor é exatamente o que se codifica; o receptor amplia.
    ///
    /// `nil` quando a tela não veio ou é pequena demais para um monitor de trabalho (menos de
    /// 1280 × 720 px) — quem chama usa o formato de sempre.
    public static func paraTela(larguraPx: Int, alturaPx: Int, hertz: Int, fps: Int? = nil,
                                escala: Escala = .dobro) -> ModoDoMonitorVirtual? {
        guard larguraPx > 0, alturaPx > 0 else { return nil }
        var largura = max(larguraPx, alturaPx) & ~1, altura = min(larguraPx, alturaPx) & ~1
        let quadro = TetoDoEmissor.macroblocos(largura: largura, altura: altura)
        if quadro > macroblocosDoNivel {
            let k = (Double(macroblocosDoNivel) / Double(quadro)).squareRoot()
            let proporcao = Double(altura) / Double(largura)
            largura = Int(Double(largura) * k) & ~1
            altura = Int(Double(largura) * proporcao) & ~1
            while TetoDoEmissor.macroblocos(largura: largura, altura: altura) > macroblocosDoNivel {
                largura -= 16
                altura = Int(Double(largura) * proporcao) & ~1
            }
        }
        guard largura >= 1280, altura >= 720 else { return nil }
        if escala == .umPraUm || cabeEm2x(larguraPx: largura, alturaPx: altura) {
            return ModoDoMonitorVirtual(larguraEmPixels: largura, alturaEmPixels: altura, hertz: hertz,
                                        fps: fps, escala: escala)
        }
        // O 2x reduzido: o piso em pontos, na proporção do painel (deitado, então a altura manda
        // quase sempre), e o dobro em pixels. A saída é o painel.
        let proporcao = Double(largura) / Double(altura)
        let alturaPt = max(pisoDoDobroEmPontos.altura, Int((Double(pisoDoDobroEmPontos.largura) / proporcao).rounded(.up)))
        let larguraPt = max(pisoDoDobroEmPontos.largura, Int((Double(alturaPt) * proporcao).rounded()))
        return ModoDoMonitorVirtual(larguraEmPixels: 2 * larguraPt, alturaEmPixels: 2 * alturaPt, hertz: hertz,
                                    fps: fps, escala: .dobro, saida: (largura, altura))
    }

    /// O maior quadro do nível que o núcleo anuncia para a tela estendida (5.2): `max_fs` em
    /// `crates/quall-core/src/teto.rs`. A 60 fps o `max_mbps` dele reduz um pouco o fps no tamanho
    /// máximo — o núcleo diz isso no registro.
    static let macroblocosDoNivel = 36_864

    /// "960 × 600 @2x" ou "1920 × 1200" — o que a pessoa reconhece em Ajustes > Monitores.
    public var rotulo: String {
        "\(larguraEmPontos) × \(alturaEmPontos)" + (escala == .dobro ? " @2x" : "")
    }

    /// Para o registro: o rótulo e o que ele custa em pixels (e para quanto a captura reduz). O fps
    /// só quando difere do Hz — o auxiliar recebe só o Hz e diria um fps que ninguém pediu.
    public var descricao: String {
        "\(rotulo) (\(larguraEmPixels)x\(alturaEmPixels) px"
            + (reduzido ? " reduzidos para \(larguraDaSaida)x\(alturaDaSaida)" : "")
            + ", \(hertz) Hz" + (fps != hertz ? " para \(fps) fps" : "") + ", escala \(escala.rawValue))"
    }
}

public enum FalhaDoMonitorVirtual: Error, CustomStringConvertible {
    case apiAusente
    case recusado(String)
    case naoFicouOnline(ms: Int)
    case semModo(ms: Int)
    case modoNaoOferecido(pedido: String, oferecidos: String)
    case naoFixou(etapa: String, erro: Int32)
    case modoErrado(pedido: String, obtido: String)

    public var description: String {
        switch self {
        case .apiAusente:
            return T("Este macOS não tem a API de monitor virtual (CGVirtualDisplay).")
        case .recusado(let motivo):
            return T("O macOS recusou criar o monitor virtual: %@", motivo)
        case .naoFicouOnline(let ms):
            return T("O monitor virtual foi criado mas não ficou online em %@ ms.", ms)
        case .semModo(let ms):
            return T("O monitor virtual ficou online mas não ganhou modo de exibição em %@ ms.", ms)
        case .modoNaoOferecido(let pedido, let oferecidos):
            return T("O monitor virtual não oferece o modo %@. Oferecidos: %@", pedido, oferecidos)
        case .naoFixou(let etapa, let erro):
            return T("Não consegui fixar o modo do monitor virtual (%@, CGError %@).", etapa, erro)
        case .modoErrado(let pedido, let obtido):
            return T("O monitor virtual ficou em %@, e não em %@.", obtido, pedido)
        }
    }
}

/// Um monitor criado **dentro deste processo**.
///
/// # Quem usa isto é o auxiliar, não o app
///
/// Duas limitações medidas neste Mac (`docs/tela-estendida.md`) tornam este tipo impróprio para
/// viver no app: **só o primeiro** monitor virtual de um processo ganha modo de exibição, e depois
/// de uma troca de modo `soltar()` **não** tira o monitor — só a saída do processo tira. Por isso o
/// app usa `MonitorVirtualAuxiliar`, que roda isto num processo `quall-monitor-virtual` por sessão e
/// o encerra no fim.
///
/// Custos medidos: criar ~90–350 ms, ficar online ~30–70 ms, fixar o modo ~300–520 ms; um processo
/// que sai leva o monitor em ~40 ms (limpo) a ~80 ms (`kill -9`).
public final class MonitorVirtual: @unchecked Sendable {
    /// "QLL" em PNP ID comprimido (5 bits por letra). Não é registro na UEFI: é o que faz o
    /// catálogo reconhecer o monitor como nosso e o macOS lembrar onde a pessoa o arrumou.
    public static let vendor: UInt32 = 0x458C
    public static let produto: UInt32 = 0x0002

    /// **Uma identidade por escala.** O macOS lembra o último modo de cada identidade de monitor, e
    /// essa lembrança vence o tamanho físico declarado (medido). Com uma identidade só, uma corrida
    /// de bancada em 1x faria a seguinte, pedida em 2x, nascer em 1x e ser aceita — e a comparação
    /// entre as duas sairia contaminada (revisão adversarial de 10/09). A série 1 ficou fora: foi a
    /// das sondas desta frente, e o macOS guarda dela o modo que eu forcei.
    ///
    /// **E uma por monitor, quando houver mais de um** (`indice`). O macOS guarda posição e modo por
    /// identidade; dois monitores iguais disputariam a mesma lembrança. O índice 0 é o de sempre —
    /// séries 2 e 3 —, então o produto de um monitor só não muda de identidade; o 1 fica com 4 e 5,
    /// o 2 com 6 e 7, e assim por diante.
    public static func serie(para escala: ModoDoMonitorVirtual.Escala, indice: Int = 0) -> UInt32 {
        (escala == .dobro ? 0x0002 : 0x0003) + UInt32(2 * max(0, indice))
    }

    public static var disponivel: Bool { QuallMonitorVirtualObjC.disponivel() }

    /// O monitor é um dos nossos — para o catálogo não oferecê-lo como um monitor comum.
    public static func ehNosso(_ id: CGDirectDisplayID) -> Bool {
        CGDisplayVendorNumber(id) == vendor && CGDisplayModelNumber(id) == produto
    }

    public let modo: ModoDoMonitorVirtual
    public let displayID: CGDirectDisplayID
    /// Uma linha para o registro: quanto custou cada etapa e o que o WindowServer aceitou.
    public let relato: String
    private let objc: QuallMonitorVirtualObjC

    private init(modo: ModoDoMonitorVirtual, objc: QuallMonitorVirtualObjC, relato: String) {
        self.modo = modo
        self.objc = objc
        self.displayID = objc.displayID
        self.relato = relato
    }

    deinit { objc.soltar() }

    /// Solta o objeto do CoreGraphics agora. Idempotente.
    ///
    /// **Não é garantia de o monitor sumir**: se o modo mudou depois de criado, ele fica online até
    /// o processo sair (medido). É por isso que o app não usa este caminho — ver o tipo.
    public func soltar() { objc.soltar() }

    /// Cria o monitor, espera ele ficar online, confere o modo com que nasceu e, **só se o sistema
    /// desobedeceu**, fixa o modo pedido e relê.
    ///
    /// # Por que conferir, e não confiar
    ///
    /// Porque o modo de nascença já saiu errado nesta bancada: com `hiDPI = 0` o monitor nasceu em
    /// **960 × 600 a 1x** — um quarto dos pixels, e a captura sairia borrada sem nenhum erro. O
    /// pedido é feito pelo tamanho físico (ver `ModoDoMonitorVirtual.milimetros`), e o macOS também
    /// lembra o último modo de cada monitor; quando os dois discordam, vale o que o app pediu.
    ///
    /// Qualquer falha depois de criar solta o monitor antes de lançar: nenhum caminho de erro deixa
    /// um monitor para trás.
    public static func criar(modo: ModoDoMonitorVirtual, nome: String, indice: Int = 0,
                             aoTerminar: (@Sendable () -> Void)? = nil) async throws -> MonitorVirtual {
        guard disponivel else { throw FalhaDoMonitorVirtual.apiAusente }
        // Pares nas duas escalas: o modo é declarado em pontos, a metade (`QuallMonitorVirtual.m`).
        guard modo.larguraEmPixels > 0, modo.alturaEmPixels > 0,
              modo.larguraEmPixels % 2 == 0, modo.alturaEmPixels % 2 == 0
        else {
            throw FalhaDoMonitorVirtual.recusado("modo impossível: \(modo.descricao)")
        }

        let t0 = DispatchTime.now()
        guard let objc = QuallMonitorVirtualObjC(
            nome: nome,
            larguraPixels: UInt32(modo.larguraEmPixels),
            alturaPixels: UInt32(modo.alturaEmPixels),
            hertz: Double(modo.hertz),
            milimetros: modo.milimetros,
            vendor: vendor,
            produto: produto,
            serie: serie(para: modo.escala, indice: indice),
            aoTerminar: aoTerminar.map { bloco in { bloco() } })
        else {
            throw FalhaDoMonitorVirtual.recusado("initWithDescriptor/applySettings recusou \(modo.descricao)")
        }
        let id = objc.displayID
        let msCriar = ms(desde: t0)

        do {
            guard let msOnline = try await esperar(prazoMs: 3_000, { estaOnline(id) }) else {
                throw FalhaDoMonitorVirtual.naoFicouOnline(ms: 3_000)
            }
            guard let msModo = try await esperar(prazoMs: 3_000, { CGDisplayCopyDisplayMode(id) != nil }) else {
                throw FalhaDoMonitorVirtual.semModo(ms: 3_000)
            }

            let nascido = CGDisplayCopyDisplayMode(id).map(descrever) ?? "sem modo"

            // **Nascer dentro de um espelho.** O macOS lembra, por identidade de monitor, se ele fazia
            // parte de um conjunto espelhado. Medido em 10/09: depois de o usuário escolher "Espelhar
            // Quall — tela estendida" no menu do iPad, a sessão seguinte nasceu espelhando o iPad,
            // com o modo dele (2360 × 1640) e **0 quadros capturados** — quem espelha não tem conteúdo
            // próprio. A fonte se chama "Tela estendida": estender é o pedido, e o espelho que envolve
            // este monitor é desfeito. Os outros monitores não são tocados.
            var espelho = ""
            if let desfeito = try desfazerEspelhoSeHouver(id) {
                guard try await esperar(prazoMs: 2_000, { CGDisplayIsInMirrorSet(id) == 0 }) != nil else {
                    throw FalhaDoMonitorVirtual.recusado("nasceu num conjunto espelhado e o espelho não se desfez em 2 s")
                }
                _ = try await esperar(prazoMs: 2_000, { CGDisplayCopyDisplayMode(id) != nil })
                espelho = " | nasceu num espelho (\(desfeito)), desfeito"
            }

            let t1 = DispatchTime.now()
            // **A escala é a que a pessoa escolheu na tela inicial** (`Emissor.escalaDaTelaEstendida`),
            // e não a que o macOS lembra. O macOS lembra o último modo de cada identidade (medido: a
            // lembrança vence o tamanho físico), e antes do seletor era por Ajustes > Monitores que a
            // pessoa escolhia — então aceitava-se a lembrança. Com o seletor, ela o faria parecer
            // quebrado: 1x escolhido, 2x nascido; e no 2x reduzido um 1x lembrado seria o pior dos dois,
            // letra menor que a do 1x do painel e sem 1:1 (revisão de 11/09). Então pixels errados **ou
            // escala errada** fixam o pedido. Trocar em Ajustes durante a sessão vale até ela acabar.
            var escalaCaiu = ""
            var motivoDaTroca: String?
            let nasceuEm = CGDisplayCopyDisplayMode(id)
            if !bate(nasceuEm, com: modo) {
                let todos = modosOferecidos(id)
                // **Só se fixa modo de desktop.** Abaixo de `pisoDoDobroEmPontos` o 2x aparece na
                // lista marcado como impróprio para desktop, e fixá-lo dá `CGError 1001` — foi o que
                // derrubou 1920 × 886 em 10/09 (medido em 11/09). `paraTela` já pede 1x nesses
                // formatos; isto cobre o pedido de bancada (`--tela-estendida-tamanho`) e o que o
                // macOS mudar. O que se quer são os **pixels** do painel, e 1x nos mesmos pixels os
                // entrega — com a letra pequena, dito no registro.
                let oferecidos = todos.filter { $0.isUsableForDesktopGUI() }
                let umPraUm = oferecidos.first {
                    $0.width == modo.larguraEmPixels && $0.height == modo.alturaEmPixels
                        && $0.pixelWidth == modo.larguraEmPixels && $0.pixelHeight == modo.alturaEmPixels
                }
                guard let alvo = oferecidos.first(where: { bate($0, com: modo) }) ?? umPraUm else {
                    throw FalhaDoMonitorVirtual.modoNaoOferecido(
                        pedido: modo.descricao,
                        oferecidos: todos.map(descrever).joined(separator: ", "))
                }
                if !bate(alvo, com: modo) {
                    // Um 2x pedido abaixo do piso nasce direto no 1x dos mesmos pixels (medido: 1520 ×
                    // 720 a 206 ppi) — e a causa é essa, não lembrança (revisão de 11/09).
                    escalaCaiu = " | !! 2x não existe como desktop em \(modo.larguraEmPixels)x\(modo.alturaEmPixels) — 1x nos mesmos pixels"
                }
                func noAlvo(_ m: CGDisplayMode?) -> Bool {
                    pixelsBatem(m, com: modo) && m?.width == alvo.width && m?.height == alvo.height
                }
                if !noAlvo(nasceuEm) {
                    motivoDaTroca = pixelsBatem(nasceuEm, com: modo) ? "escala lembrada pelo macOS" : "pixels errados"
                    try fixar(alvo, em: id)
                    guard try await esperar(prazoMs: 2_000, { noAlvo(CGDisplayCopyDisplayMode(id)) }) != nil else {
                        throw FalhaDoMonitorVirtual.modoErrado(
                            pedido: modo.descricao,
                            obtido: CGDisplayCopyDisplayMode(id).map(descrever) ?? "sem modo")
                    }
                }
            }
            let msFixar = ms(desde: t1)

            let emUso = CGDisplayCopyDisplayMode(id).map(descrever) ?? "sem modo"
            // **Principal ou espelho são lembrados como o modo**: se numa sessão a pessoa arrastou a
            // barra de menus para este monitor, ou escolheu espelhar, as seguintes nascem assim — e o
            // Dock, as janelas novas ou a tela inteira vão para o tablet. Não é falha (foi escolha
            // dela), mas tem de estar no registro com destaque.
            let principal = CGDisplayIsMain(id) != 0
            let espelhoDe = CGDisplayMirrorsDisplay(id)
            let aviso = (principal ? " | !! o macOS pôs este monitor como PRINCIPAL" : "")
                + (espelhoDe != kCGNullDirectDisplay ? " | !! este monitor ESPELHA o \(espelhoDe), não estende" : "")
            let desfecho = motivoDaTroca.map { ", \($0): fixado em \(emUso) em \(msFixar) ms" }
                ?? (bate(CGDisplayCopyDisplayMode(id), com: modo) ? ", já no modo pedido" : ", nos pixels pedidos")
            let relato = "monitor virtual: id=\(id) pedido \(modo.descricao) — criado em \(msCriar) ms, "
                + "online em \(msOnline) ms, modo em \(msModo) ms, nasceu \(nascido)" + desfecho
                + escalaCaiu + espelho + aviso
                + " | posição (\(Int(CGDisplayBounds(id).minX)),\(Int(CGDisplayBounds(id).minY)))"
            return MonitorVirtual(modo: modo, objc: objc, relato: relato)
        } catch {
            objc.soltar()
            throw error
        }
    }

    // MARK: - CoreGraphics

    private static func estaOnline(_ id: CGDirectDisplayID) -> Bool {
        var contagem: UInt32 = 0
        guard CGGetOnlineDisplayList(0, nil, &contagem) == .success, contagem > 0 else { return false }
        var lista = [CGDirectDisplayID](repeating: 0, count: Int(contagem))
        guard CGGetOnlineDisplayList(contagem, &lista, &contagem) == .success else { return false }
        return lista.prefix(Int(contagem)).contains(id)
    }

    /// Os modos que o monitor oferece, **inclusive os HiDPI**: sem
    /// `kCGDisplayShowDuplicateLowResolutionModes` o 2x não aparece na lista.
    private static func modosOferecidos(_ id: CGDirectDisplayID) -> [CGDisplayMode] {
        let opcoes = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
        return (CGDisplayCopyAllDisplayModes(id, opcoes) as? [CGDisplayMode]) ?? []
    }

    /// Os pixels por baixo são os pedidos — em qualquer escala.
    static func pixelsBatem(_ atual: CGDisplayMode?, com modo: ModoDoMonitorVirtual) -> Bool {
        guard let atual else { return false }
        return atual.pixelWidth == modo.larguraEmPixels && atual.pixelHeight == modo.alturaEmPixels
    }

    static func bate(_ atual: CGDisplayMode?, com modo: ModoDoMonitorVirtual) -> Bool {
        guard let atual else { return false }
        return atual.width == modo.larguraEmPontos && atual.height == modo.alturaEmPontos
            && atual.pixelWidth == modo.larguraEmPixels && atual.pixelHeight == modo.alturaEmPixels
    }

    private static func descrever(_ m: CGDisplayMode) -> String {
        "\(m.width)x\(m.height) pt/\(m.pixelWidth)x\(m.pixelHeight) px @\(Int(m.refreshRate.rounded())) Hz"
            + (m.isUsableForDesktopGUI() ? "" : " (não-desktop)")
    }

    /// `.forSession`, e não `.permanently`: o modo vale até o monitor sumir ou a sessão de login
    /// acabar, e não fica gravado nas preferências de ninguém.
    private static func fixar(_ alvo: CGDisplayMode, em id: CGDirectDisplayID) throws {
        var config: CGDisplayConfigRef?
        let e1 = CGBeginDisplayConfiguration(&config)
        guard e1 == .success, let config else {
            throw FalhaDoMonitorVirtual.naoFixou(etapa: "CGBeginDisplayConfiguration", erro: e1.rawValue)
        }
        let e2 = CGConfigureDisplayWithDisplayMode(config, id, alvo, nil)
        guard e2 == .success else {
            CGCancelDisplayConfiguration(config)
            throw FalhaDoMonitorVirtual.naoFixou(etapa: "CGConfigureDisplayWithDisplayMode", erro: e2.rawValue)
        }
        let e3 = CGCompleteDisplayConfiguration(config, .forSession)
        guard e3 == .success else {
            throw FalhaDoMonitorVirtual.naoFixou(etapa: "CGCompleteDisplayConfiguration", erro: e3.rawValue)
        }
    }

    // MARK: - espelho

    /// Se este monitor está num conjunto espelhado, tira-o de lá — e tira quem o espelhava — e diz
    /// o que havia. `nil` quando não havia espelho nenhum.
    ///
    /// **Não é só no nascimento.** Medido em 10/09: o monitor nasceu certo, e **18 s depois** o
    /// Sidecar reaplicou a escolha lembrada "Espelhar Quall — tela estendida" — o **iPad** passou a
    /// espelhar o monitor do Quall. O auxiliar chama isto a sessão inteira
    /// (`quall-monitor-virtual/main.swift`).
    public static func desfazerEspelhoSeHouver(_ id: CGDirectDisplayID) throws -> String? {
        guard CGDisplayIsInMirrorSet(id) != 0 else { return nil }
        let origem = CGDisplayMirrorsDisplay(id)
        let espelhos = quemEspelha(id)
        try desfazerEspelho(de: id, espelhos: espelhos)
        if origem != kCGNullDirectDisplay, origem != 0xFFFF_FFFF {
            return "espelhando o \(origem)"
        }
        return espelhos.isEmpty ? "conjunto espelhado" : "espelhado por \(espelhos.map(String.init).joined(separator: ", "))"
    }

    /// Quem espelha este monitor — os que o têm como origem.
    private static func quemEspelha(_ id: CGDirectDisplayID) -> [CGDirectDisplayID] {
        var contagem: UInt32 = 0
        guard CGGetOnlineDisplayList(0, nil, &contagem) == .success, contagem > 0 else { return [] }
        var lista = [CGDirectDisplayID](repeating: 0, count: Int(contagem))
        guard CGGetOnlineDisplayList(contagem, &lista, &contagem) == .success else { return [] }
        return lista.prefix(Int(contagem)).filter { $0 != id && CGDisplayMirrorsDisplay($0) == id }
    }

    /// Tira este monitor do conjunto espelhado, e tira dele quem o espelhava.
    ///
    /// `.permanently`, e não `.forSession`: o que se desfaz aqui é justamente uma **lembrança** do
    /// macOS, e com `.forSession` ela voltaria na sessão seguinte. É o mesmo que a pessoa escolher
    /// "Usar Como Tela Estendida" no menu.
    private static func desfazerEspelho(de id: CGDirectDisplayID, espelhos: [CGDirectDisplayID]) throws {
        var config: CGDisplayConfigRef?
        let e1 = CGBeginDisplayConfiguration(&config)
        guard e1 == .success, let config else {
            throw FalhaDoMonitorVirtual.naoFixou(etapa: "CGBeginDisplayConfiguration (espelho)", erro: e1.rawValue)
        }
        var e2 = CGConfigureDisplayMirrorOfDisplay(config, id, kCGNullDirectDisplay)
        for outro in espelhos where e2 == .success {
            e2 = CGConfigureDisplayMirrorOfDisplay(config, outro, kCGNullDirectDisplay)
        }
        guard e2 == .success else {
            CGCancelDisplayConfiguration(config)
            throw FalhaDoMonitorVirtual.naoFixou(etapa: "CGConfigureDisplayMirrorOfDisplay", erro: e2.rawValue)
        }
        let e3 = CGCompleteDisplayConfiguration(config, .permanently)
        guard e3 == .success else {
            throw FalhaDoMonitorVirtual.naoFixou(etapa: "CGCompleteDisplayConfiguration (espelho)", erro: e3.rawValue)
        }
    }

    // MARK: - espera

    /// Volta os milissegundos até a condição valer, ou `nil` se o prazo acabar. Cancelável.
    private static func esperar(prazoMs: Int, _ condicao: () -> Bool) async throws -> Int? {
        let t0 = DispatchTime.now()
        while true {
            if condicao() { return ms(desde: t0) }
            if ms(desde: t0) >= prazoMs { return nil }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
    }

    private static func ms(desde t0: DispatchTime) -> Int {
        Int((DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1_000_000)
    }
}
