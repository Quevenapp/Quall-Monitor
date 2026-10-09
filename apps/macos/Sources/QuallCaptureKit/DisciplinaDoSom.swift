import Foundation

// MARK: - O reamostrador

/// **O reamostrador do som**: sinc com janela de Kaiser (β = 8, 32 pontos) numa tabela polifásica
/// de 256 fases, com interpolação linear entre elas (`docs/som-no-receptor.md` §19.6.2). É a mesma
/// peça de `apps/windows/src/reamostrador_sinc.rs`, com os mesmos números.
///
/// - É o **atuador** da disciplina da deriva: a razão `ρ = 1 + u` (entrada por saída) faz o tempo
///   de mídia seguir o relógio do host, e o carimbo anda um quadro exato por pacote.
/// - **Sem normalizar pela soma dos pesos**: a soma varia com a fase fracionária, e normalizar
///   modula o ganho a ~24 Hz com 500 ppm (medido no Windows: 87,8 dB em vez de 95 a 1 kHz).
/// - O atraso de grupo é zero em relação à posição de entrada (L-c da segunda crítica): nada se
///   desconta da hora.
/// - No Mac ele roda **a 8 kHz, depois do conversor**. A 1 kHz, ~90 dB (o `Int16` limita); a
///   3,4 kHz, **60 dB medidos**: 0,425 da taxa cai na borda da banda de transição do núcleo (que
///   começa em ~0,42). Para o µ-law, que tem ~38 dB, sobra folga.
public struct ReamostradorSinc {
    public static let meiaLargura = 16
    public static let fases = 256
    public static let beta = 8.0

    private static let tabela: [Float] = {
        let n = meiaLargura * fases + 2
        let i0b = besselI0(beta)
        return (0..<n).map { i in
            let t = Double(i) / Double(fases)
            if t >= Double(meiaLargura) { return 0 }
            let s = t == 0 ? 1 : sin(Double.pi * t) / (Double.pi * t)
            let r = t / Double(meiaLargura)
            let w = besselI0(beta * max(0, 1 - r * r).squareRoot()) / i0b
            return Float(s * w)
        }
    }()

    static func besselI0(_ x: Double) -> Double {
        var soma = 1.0, termo = 1.0
        let q = x * x / 4
        for k in 1..<60 {
            termo *= q / Double(k * k)
            soma += termo
            if termo < 1e-17 * soma { break }
        }
        return soma
    }

    public let canais: Int
    let razaoNominal: Double
    public private(set) var u: Double = 0
    let corte: Double
    let soContar: Bool
    private var entrada: [Float] = []
    private var base: UInt64 = 0
    public private(set) var pos: Double = 0
    public private(set) var produzidas: UInt64 = 0
    private var guardadasContadas: UInt64 = 0

    public init(taxaEntrada: Double, taxaSaida: Double, canais: Int, soContar: Bool = false) {
        self.canais = max(1, canais)
        razaoNominal = taxaEntrada / taxaSaida
        corte = min(1, 1 / razaoNominal)
        self.soContar = soContar
    }

    public var passo: Double { razaoNominal * (1 + u) }
    public mutating func definirAjuste(_ u: Double) { self.u = u }
    var meiaLarguraDeEntrada: Double { Double(Self.meiaLargura) / corte }

    public var fimDaEntrada: UInt64 {
        soContar ? base + guardadasContadas : base + UInt64(entrada.count / canais)
    }

    /// O índice de saída (fracionário) cuja posição de entrada é `x`.
    public func indiceDeSaida(de x: Double) -> Double {
        Double(produzidas) + (x - pos) / passo
    }

    public mutating func empurrar(_ amostras: [Int16]) {
        if soContar {
            guardadasContadas += UInt64(amostras.count / canais)
        } else {
            entrada.append(contentsOf: amostras.map { Float($0) / 32768 })
        }
    }

    public mutating func empurrarContagem(_ quadros: UInt64) {
        if soContar {
            guardadasContadas += quadros
        } else {
            entrada.append(contentsOf: [Float](repeating: 0, count: Int(quadros) * canais))
        }
    }

    public mutating func pularEntrada(_ quadros: UInt64) { pos += Double(quadros) }

    public mutating func produzir(_ saida: inout [Int16]) {
        let l = meiaLarguraDeEntrada
        let p = passo
        let fim = Double(fimDaEntrada)
        while (pos + l).rounded(.down) < fim {
            if soContar {
                saida.append(contentsOf: [Int16](repeating: 0, count: canais))
            } else {
                umaSaida(l, &saida)
            }
            pos += p
            produzidas += 1
        }
        descartarHistoria(l)
    }

    private func umaSaida(_ l: Double, _ saida: inout [Int16]) {
        let c = corte
        let i0 = max(UInt64((pos - l).rounded(.up).clamped(0)), base)
        let i1 = UInt64((pos + l).rounded(.down))
        var acc = [Double](repeating: 0, count: canais)
        let tab = Self.tabela
        if i0 <= i1 {
            for i in i0...i1 {
                let t = abs(Double(i) - pos) * c * Double(Self.fases)
                let k = Int(t)
                if k + 1 >= tab.count { continue }
                let fr = Float(t - Double(k))
                let w = Double(tab[k] + (tab[k + 1] - tab[k]) * fr)
                let j = Int(i - base) * canais
                for ch in 0..<canais { acc[ch] += Double(entrada[j + ch]) * w }
            }
        }
        for ch in 0..<canais {
            let v = acc[ch] * c * 32768
            saida.append(Int16(max(-32768, min(32767, v.rounded()))))
        }
    }

    private mutating func descartarHistoria(_ l: Double) {
        let primeira = (pos - l).rounded(.down) - 1
        guard primeira > Double(base) else { return }
        let descartar = min(UInt64(primeira) - base, fimDaEntrada - base)
        if soContar {
            guardadasContadas -= descartar
        } else {
            entrada.removeFirst(Int(descartar) * canais)
        }
        base += descartar
    }
}

private extension Double {
    func clamped(_ minimo: Double) -> Double { Swift.max(minimo, self) }
}

// MARK: - A disciplina

/// **O laço da disciplina da deriva** (`som-no-receptor.md` §19.6.4), o mesmo de
/// `apps/windows/src/disciplina.rs`. No Mac, uma atualização por janela de 10 s, sobre o erro que a
/// janela deu, com T = 60 s nos 5 primeiros minutos e 300 s depois.
public struct DisciplinaDaDeriva {
    public static let uMax = 1_000e-6
    public static let fMax = 500e-6
    public static let passoMaximoDeU = 20e-6

    public let tCurto: Double
    public let tLongo: Double
    public let partida: Double
    public let ligada: Bool
    public private(set) var f: Double = 0
    public private(set) var u: Double = 0
    private var inicio: Double?
    private var ultima: Double?
    public private(set) var atualizacoes = 0
    public private(set) var maiorDuPpm = 0.0

    /// O período das atualizações (a janela do Mac, 10 s).
    public let periodo: Double

    public init(tCurto: Double = 60, tLongo: Double = 300, partida: Double = 300, periodo: Double = 10,
                ligada: Bool = true) {
        self.tCurto = tCurto
        self.tLongo = tLongo
        self.partida = partida
        self.periodo = periodo
        self.ligada = ligada
    }

    public var fPpm: Double { f * 1e6 }

    /// Uma atualização com o erro `eUs` (µs) de uma janela que fechou em `agoraS` (s).
    public mutating func atualizar(agoraS: Double, eUs: Double) {
        let ini = inicio ?? agoraS
        inicio = ini
        // O `dt` tem teto de dois períodos: depois de uma lacuna longa, a atualização seguinte
        // multiplicaria o erro de uma janela pela lacuna inteira (a revisão do código, A: sob A,
        // `f` de 50 a 119 ppm e um socorro depois de 1 h).
        let dt = min(ultima.map { agoraS - $0 } ?? periodo, 2 * periodo)
        ultima = agoraS
        guard ligada else { return }
        let desde = agoraS - ini
        let naPartida = desde < partida
        let t = naPartida ? tCurto : tLongo
        let (kp, ki) = (2 / t, 1 / (t * t))
        let e = eUs * 1e-6
        f = min(Self.fMax, max(-Self.fMax, f - ki * e * dt))
        let alvo = min(Self.uMax, max(-Self.uMax, f - kp * e))
        let novo = naPartida ? alvo : min(u + Self.passoMaximoDeU, max(u - Self.passoMaximoDeU, alvo))
        if desde > 60 { maiorDuPpm = max(maiorDuPpm, abs(novo - u) * 1e6) }
        u = novo
        atualizacoes += 1
    }

    /// Depois de um degrau: a fase recomeça, a frequência fica.
    public mutating func reancorar() { u = f }
}

// MARK: - O portão (N4 da segunda crítica)

/// **O portão do Mac**: a disciplina só liga quando a deriva medida passa de 2 ppm com confiança.
/// No alto-falante embutido e no fone Bluetooth a deriva é zero (medido: ≤ 0,4 ppm por três vias),
/// e o laço só passaria ao carimbo a variação da latência de entrega (N4).
///
/// **A estimativa**:
/// 1. as diferenças entre janelas seguidas; a que se afasta da mediana delas mais de `4σ` (σ pelo
///    MAD) é um degrau na latência de entrega, e vira a mediana;
/// 2. a série refeita com essas diferenças, sem os degraus;
/// 3. a inclinação dela por mínimos quadrados, com o erro-padrão dos resíduos.
///
/// **Abre** quando `|b| > 2 ppm + 1,5 · 4σ / T` e o erro-padrão é no máximo `|b|/5`. O segundo
/// termo é o que um degrau abaixo do limiar (que fica na série) ainda poderia inclinar a reta
/// sobre a duração `T` (no meio, `1,5 · degrau / T`). A 300 ppm, abre em 80 s; a 5 ppm, com o
/// ruído do teste (~300 µs por janela), em ~15 min.
///
/// As duas versões anteriores, reprovadas nos testes:
/// - o Theil–Sen sobre todos os pares abriu a 0 ppm com um degrau de +5 ms na entrega: metade dos
///   pares atravessa um degrau no meio da série;
/// - a mediana das diferenças seguidas resiste ao degrau, mas o erro dela não cai com a duração
///   (cada diferença de 10 s tem dezenas de ppm de ruído): a ±5 ppm, não abriu em 40 min.
public struct PortaoDaDeriva {
    public static let limiarPpm = 2.0
    public static let janelasMinimas = 7
    public static let maximoDeJanelas = 361
    public private(set) var aberto = false
    public private(set) var inclinacaoPpm = 0.0
    public private(set) var erroPadraoPpm = Double.infinity
    /// Degraus de latência tirados da série (diferenças acima de 4σ), nos pontos guardados.
    public private(set) var degrausTirados = 0
    private var pontos: [(t: Double, e: Double)] = []

    public init(aberto: Bool = false) { self.aberto = aberto }

    /// Um ponto (a hora da janela em s, o erro dela em µs). Devolve `true` quando o portão abriu agora.
    public mutating func adicionar(tS: Double, eUs: Double) -> Bool {
        if let u = pontos.last, tS <= u.t { return false }
        pontos.append((tS, eUs))
        if pontos.count > Self.maximoDeJanelas { pontos.removeFirst() }
        guard !aberto, pontos.count >= Self.janelasMinimas else { return false }
        let n = pontos.count
        let d = (1..<n).map { pontos[$0].e - pontos[$0 - 1].e }
        let med = d.sorted()[d.count / 2]
        let mad = d.map { abs($0 - med) }.sorted()[d.count / 2]
        let limiar = 4 * max(1.0, 1.4826 * mad)
        var serie = [pontos[0].e]
        degrausTirados = 0
        for k in d {
            if abs(k - med) > limiar { degrausTirados += 1 }
            serie.append(serie[serie.count - 1] + (abs(k - med) > limiar ? med : k))
        }
        let tm = pontos.map(\.t).reduce(0, +) / Double(n)
        let em = serie.reduce(0, +) / Double(n)
        var stt = 0.0, ste = 0.0
        for k in 0..<n {
            stt += (pontos[k].t - tm) * (pontos[k].t - tm)
            ste += (pontos[k].t - tm) * (serie[k] - em)
        }
        guard stt > 0 else { return false }
        let b = ste / stt
        var sres = 0.0
        for k in 0..<n {
            let r = serie[k] - (em + b * (pontos[k].t - tm))
            sres += r * r
        }
        inclinacaoPpm = b
        erroPadraoPpm = (sres / Double(n - 2) / stt).squareRoot()
        let duracao = pontos[n - 1].t - pontos[0].t
        let vies = 1.5 * limiar / duracao
        if abs(b) > Self.limiarPpm + vies && erroPadraoPpm <= abs(b) / 5 {
            aberto = true
            return true
        }
        return false
    }

    /// Uma reabertura: os pontos recomeçam, e o portão aberto fica aberto.
    public mutating func recomecar() { pontos.removeAll() }
}

// MARK: - A linha do som do Mac

/// **A linha do tempo do som do Mac**, que substitui o `RelogioDoSomCapturado` (§19.1): o carimbo
/// em tempo de mídia, a lacuna pelo PTS e a disciplina da deriva pela entrega
/// (`som-no-receptor.md` §19.6.4).
///
/// # A hora do host, nas duas hipóteses do N1
///
/// Com a saída embutida, o PTS de som do SCK anda exatamente com a contagem de amostras, e não se
/// sabe se ele é tempo de mídia (A) ou a hora do host (B) com um dispositivo travado no host. A
/// regra não depende disso:
/// - por bloco, `m = agora − S`: a hora da entrega contra o carimbo que a linha de saída dá à
///   primeira amostra dele. Sob A a entrega inclina com a deriva; sob B, o `PTS − contagem`
///   inclina. Nos dois casos `m` inclina, e o PTS não entra;
/// - o **mínimo por 10 s** tira o jitter e o dente de serra de 4 s da entrega (medido); o erro da
///   janela é esse mínimo menos o da primeira janela (o viés constante do caminho, os 750 µs do
///   conversor, fica no carimbo como hoje);
/// - **a referência é corrigida quando o portão abre**: o mínimo da primeira janela foi tirado com
///   a deriva correndo, e cai na ponta que o sinal dela escolhe (o fim, com a linha adiantando; o
///   começo, atrasando). Com a inclinação medida `b`, a referência volta ao que era na âncora:
///   `− b · (hora do mínimo − âncora)`. Sem isso, a +300 ppm o carimbo assentava 1,8 ms à frente, e
///   a −300 ppm em cima do host (medido no teste);
/// - o **PTS só acusa lacuna**: um salto de 2 ms ou mais contra a contagem é conteúdo que o SCK
///   perdeu (um bloco é de 10 ms ou mais; um passo de deriva entre dois blocos, a 500 ppm, é de
///   10 µs). A referência da entrega fica.
///
/// # O degrau de uma lacuna, nas duas hipóteses
///
/// O salto do PTS tem duas leituras, e só a entrega as separa, e só depois:
/// - **o PTS seguiu no tempo dele** (B; ou A sem reancorar): o degrau é o salto;
/// - **o SCK reancorou o tempo de mídia na hora do host** na pausa (A; não medido): o salto carrega
///   a correção que o laço acumulou desde a âncora, `c = (S − PTS) − (S − PTS)₀`, e o degrau é
///   `salto − c`. A 100 ppm por 1 h, c = ±360 ms.
///
/// A regra fica com **o menor dos dois** (a segunda só quando |c| passa de 2 ms, acima do jitter
/// do conversor, e nunca negativa), limitado pela entrega do bloco que volta: o carimbo nunca passa
/// de `agora − mínimo de referência + 1 ms`. Esse teto é a entrega, e não a hora do bloco: o
/// carimbo pode ficar à frente pelo excesso de entrega do bloco que volta (0 a 60 ms medidos), que
/// o laço tira em ~T; nunca pelo salto inteiro, que só se desfaria cortando som. Se ficou **atrás**
/// (a leitura escolhida era a errada), a primeira janela inteira depois da lacuna mostra o erro
/// contra o da janela de antes, e acima de 5 ms ele vira um degrau para a frente, sem perder som.
/// O custo, dito: até 10 s com o carimbo atrasado de `c`.
///
/// **Para trás**, o salto é lacuna quando a correção explica (`salto − c ≥ 0`, com c < −2 ms: sob A
/// reancorado, com o dispositivo mais rápido, uma pausa mais curta que a correção). Se não explica,
/// é som repetido e sai da entrada, até 1 s; acima disso, é o PTS que recomeçou de outra base, e a
/// referência recomeça sem degrau e sem corte. O PTS inválido (0) não entra na lacuna.
///
/// # O atuador
///
/// O sinc a 8 kHz, depois do conversor ([`ReamostradorSinc`]), com `ρ = 1 + u` quando o portão
/// está aberto (N4). O carimbo anda 20 ms exatos por quadro do fatiador.
///
/// Pura, sem relógio: quem chama passa as horas.
public struct LinhaDoSomDoMac {
    public static let limiarDeLacunaUs: Int64 = 2_000
    public static let janelaUs: UInt64 = 10_000_000
    public static let limiarDoSocorroUs = 40_000.0

    public let taxa: Double
    public private(set) var sinc: ReamostradorSinc
    public private(set) var disciplina: DisciplinaDaDeriva
    public private(set) var portao: PortaoDaDeriva
    /// `false`: o controle da lacuna (a regra de antes de 19/09): nenhum degrau.
    public let degrauNasLacunas: Bool

    public private(set) var ancoraUs: Double?
    private var degrausUs = 0.0
    private var descartadas: UInt64 = 0
    private var quadrosFeitos: UInt64 = 0
    private var ptsEsperadoUs: Double?
    private var reancorarNoProximo = false
    private var inicioDaJanelaUs: UInt64?
    private var minimoDaJanela = Double.infinity
    private var horaDoMinimoDaJanela: UInt64 = 0
    private var minimoDeReferencia: Double?
    /// A hora (da entrega) do mínimo da janela de referência, e a da âncora (ou da reabertura).
    private var horaDoMinimoDeReferencia: UInt64 = 0
    private var horaDaAncora: UInt64?
    private var janelasAcimaDoSocorro = 0
    private var corteDevido: UInt64 = 0
    /// `(S − PTS)` no primeiro bloco contíguo depois da âncora: o viés do conversor.
    private var folgaNaAncora: Double?
    /// O erro da janela de antes de uma lacuna, para conferir a primeira janela inteira depois dela.
    private var conferirContra: Double?
    public static let limiarDaFolgaUs = 2_000.0
    /// O degrau de lacuna a partir do qual a sobra do fatiador é jogada fora.
    public static let limiarDeDescarteUs = 10_000.0
    /// O teto do corte quando o PTS volta: som repetido é no máximo um bloco; acima de 1 s, é o PTS
    /// que recomeçou de outra base.
    public static let maximoDeCorteUs: Int64 = 1_000_000
    public static let limiarDaConferenciaUs = 5_000.0
    public static let toleranciaDaEntregaUs = 1_000.0

    public private(set) var lacunas = 0
    public private(set) var maiorLacunaUs: Int64 = 0
    public private(set) var ptsParaTras = 0
    public private(set) var degraus = 0
    public private(set) var socorros = 0
    public private(set) var reancoragens = 0
    /// Degraus para a frente dados pela conferência da primeira janela depois de uma lacuna.
    public private(set) var conferenciasDeLacuna = 0
    /// O que a referência da entrega andou quando o portão abriu (µs).
    public private(set) var correcaoDaReferenciaUs = 0.0
    /// Amostras de entrada cortadas (o PTS que voltou, o socorro para trás).
    public private(set) var amostrasCortadas: UInt64 = 0
    /// Blocos com PTS inválido (0).
    public private(set) var ptsInvalidos = 0
    /// O erro da última janela (µs): a deriva desde a âncora, medida pela entrega.
    public private(set) var erroUs = 0.0
    public private(set) var maiorErroUs = 0.0
    /// O carimbo que a linha deu à primeira amostra do último bloco (µs do host).
    public private(set) var ultimoSUs = 0.0

    /// `disciplina`: o laço (o controle o desliga). `portaoAberto`: força o portão (os testes).
    public init(taxa: Double, canais: Int, disciplina: Bool = true, portaoAberto: Bool = false,
                degrauNasLacunas: Bool = true, soContar: Bool = false) {
        self.taxa = taxa
        sinc = ReamostradorSinc(taxaEntrada: taxa, taxaSaida: taxa, canais: canais, soContar: soContar)
        self.disciplina = DisciplinaDaDeriva(ligada: disciplina)
        portao = PortaoDaDeriva(aberto: portaoAberto)
        self.degrauNasLacunas = degrauNasLacunas
    }

    public var fPpm: Double { disciplina.fPpm }
    public var ajustePpm: Double { sinc.u * 1e6 }

    /// A captura foi reaberta: o próximo bloco reancora (para a frente), e a referência da entrega
    /// recomeça.
    public mutating func pedirReancoragem() { reancorarNoProximo = true }

    private func s(de y: Double) -> Double {
        (ancoraUs ?? 0) + (y - Double(descartadas)) / taxa * 1e6 + degrausUs
    }

    /// Um bloco do SCK: o PTS e as amostras dele a 48 kHz (a contagem), a hora da entrega, as
    /// amostras já convertidas (a `taxa` da linha) e as pendentes do fatiador. Devolve a saída do
    /// sinc e se o fatiador tem de jogar fora a sobra (um degrau entrou).
    public mutating func bloco(ptsUs: UInt64, amostras48: Int, agoraUs: UInt64, convertidas: [Int16],
                               pendentesDoFatiador: Int) -> (saida: [Int16], descartarSobra: Bool) {
        var descartar = false
        var entrada = convertidas
        // **O PTS inválido** chega como 0 (`MonotonicClock.microseconds(from:)`). Ele não entra na
        // lacuna nem na âncora: o bloco é tomado como contíguo, e o esperado anda o tamanho dele.
        // Antes, ele virava um salto para trás do tamanho da sessão, cortado sem teto (a revisão
        // do código: nenhuma saída nos 10 min seguintes).
        let ptsValido = ptsUs > 0
        if !ptsValido { ptsInvalidos += 1 }
        let pts = Double(ptsUs)
        let duracaoUs = Double(amostras48) / 48_000 * 1e6
        if ancoraUs == nil {
            guard ptsValido else { return ([], false) }
            ancoraUs = pts
            horaDaAncora = agoraUs
        } else if reancorarNoProximo && ptsValido {
            // A reabertura: o carimbo nunca volta. Degrau para a frente até o PTS novo.
            reancorarNoProximo = false
            descartadas += UInt64(pendentesDoFatiador)
            descartar = true
            let degrau = max(0, (pts - s(de: sinc.indiceDeSaida(de: Double(sinc.fimDaEntrada)))).rounded())
            degrausUs += degrau
            degraus += 1
            recomecarAReferencia(agoraUs)
        } else if let esperado = ptsEsperadoUs, ptsValido {
            // A lacuna: o PTS contra a contagem.
            let salto = Int64((pts - esperado).rounded())
            if abs(salto) >= Self.limiarDeLacunaUs {
                // O carimbo que a primeira amostra de depois teria sem degrau, e a correção que o
                // laço acumulou desde a âncora (a leitura "o SCK reancorou o tempo de mídia na hora
                // do host": o degrau seria `salto − c`).
                let sFim = s(de: sinc.indiceDeSaida(de: Double(sinc.fimDaEntrada)))
                let c = folgaNaAncora.map { (sFim - esperado) - $0 } ?? 0
                let reancorado = Double(salto) - c
                let explica = abs(c) > Self.limiarDaFolgaUs && reancorado >= 0
                if salto > 0 || (explica && degrauNasLacunas) {
                    // **Lacuna**. Para a frente, o menor das duas leituras (o carimbo não fica à
                    // frente por ela); para trás, só quando a correção acumulada explica o salto
                    // (a revisão do código, C: sob A reancorado, com o dispositivo mais rápido,
                    // uma pausa mais curta que a correção dá salto negativo, que antes cortava
                    // ~270 ms de som e deixava o socorro puxar o `f`).
                    var degrau = salto > 0 ? (explica ? min(Double(salto), reancorado) : Double(salto)) : reancorado
                    lacunas += 1
                    if degrauNasLacunas {
                        if let referencia = minimoDeReferencia {
                            degrau = min(degrau, Double(agoraUs) - referencia + Self.toleranciaDaEntregaUs - sFim)
                        }
                        degrau = max(0, degrau.rounded())
                        maiorLacunaUs = max(maiorLacunaUs, Int64(degrau))
                        if degrau >= Self.limiarDeDescarteUs {
                            // A sobra do fatiador é de antes da lacuna e sai; o degrau soma o que
                            // ela ocupava, para o som de depois ficar na hora dele (L-d).
                            descartadas += UInt64(pendentesDoFatiador)
                            descartar = true
                            degrausUs += degrau + Double(pendentesDoFatiador) / taxa * 1e6
                        } else {
                            // Um degrau pequeno (um PTS ruidoso passa por aqui): a sobra fica, e
                            // sai com a hora de depois, errada por menos de 10 ms (leve 2).
                            degrausUs += degrau
                        }
                        degraus += 1
                        // A primeira janela inteira depois da lacuna confere o degrau.
                        if minimoDeReferencia != nil {
                            conferirContra = erroUs
                            inicioDaJanelaUs = nil
                            minimoDaJanela = .infinity
                        }
                        // O som voltou depois de mais que uma janela: a fase recomeça, e a
                        // frequência fica (a revisão do código, A).
                        if Double(salto) >= Double(Self.janelaUs) || reancorado >= Double(Self.janelaUs) {
                            disciplina.reancorar()
                        }
                    } else {
                        maiorLacunaUs = max(maiorLacunaUs, salto)
                    }
                } else if -salto <= Self.maximoDeCorteUs {
                    // Nunca visto: o SCK repetiu som. Corta o repetido da entrada.
                    ptsParaTras += 1
                    let n = UInt64(Double(-salto) * taxa / 1e6)
                    corteDevido += n
                    amostrasCortadas += n
                } else {
                    // **Para trás além do teto**: não é som repetido, é o PTS que recomeçou de
                    // outra base. O som é contínuo; a referência recomeça, como numa reabertura,
                    // sem degrau e sem corte (a revisão do código, B).
                    ptsParaTras += 1
                    recomecarAReferencia(agoraUs)
                }
            }
        }
        if ptsValido {
            ptsEsperadoUs = pts + duracaoUs
        } else if let e = ptsEsperadoUs {
            ptsEsperadoUs = e + duracaoUs
        }

        // O corte devido sai do começo.
        if corteDevido > 0 {
            let n = min(Int(corteDevido), entrada.count / sinc.canais)
            entrada.removeFirst(n * sinc.canais)
            corteDevido -= UInt64(n)
        }

        // O erro: a entrega contra a linha de saída, pelo mínimo da janela.
        let x = Double(sinc.fimDaEntrada)
        let sBloco = s(de: sinc.indiceDeSaida(de: x))
        ultimoSUs = sBloco
        if folgaNaAncora == nil && x > 0 && !descartar { folgaNaAncora = sBloco - pts }
        let m = Double(agoraUs) - sBloco
        let inicio = inicioDaJanelaUs ?? agoraUs
        inicioDaJanelaUs = inicio
        if m < minimoDaJanela {
            minimoDaJanela = m
            horaDoMinimoDaJanela = agoraUs
        }
        if agoraUs >= inicio + Self.janelaUs {
            fecharJanela(agoraUs)
        }

        sinc.empurrar(entrada)
        var saida: [Int16] = []
        sinc.produzir(&saida)
        return (saida, descartar)
    }

    /// Só para o modo "só contar" dos testes: um bloco de `quadros` amostras sem conteúdo.
    public mutating func blocoContado(ptsUs: UInt64, amostras48: Int, agoraUs: UInt64, quadros: Int,
                                      pendentesDoFatiador: Int = 0) -> (saidas: Int, descartarSobra: Bool) {
        let r = bloco(ptsUs: ptsUs, amostras48: amostras48, agoraUs: agoraUs,
                      convertidas: [Int16](repeating: 0, count: quadros * sinc.canais),
                      pendentesDoFatiador: pendentesDoFatiador)
        return (r.saida.count / sinc.canais, r.descartarSobra)
    }

    private mutating func fecharJanela(_ agoraUs: UInt64) {
        let minimo = minimoDaJanela
        minimoDaJanela = .infinity
        inicioDaJanelaUs = agoraUs
        guard var referencia = minimoDeReferencia else {
            minimoDeReferencia = minimo
            horaDoMinimoDeReferencia = horaDoMinimoDaJanela
            return
        }
        var e = minimo - referencia
        erroUs = e
        if abs(e) > abs(maiorErroUs) { maiorErroUs = e }
        let t = Double(agoraUs) / 1e6
        if let antes = conferirContra {
            conferirContra = nil
            let atras = e - antes
            if atras > Self.limiarDaConferenciaUs {
                // O degrau da lacuna ficou curto: o resto, para a frente, sem passar pelo laço.
                degrausUs += atras.rounded()
                degraus += 1
                conferenciasDeLacuna += 1
                erroUs = antes
                return
            }
        }
        if portao.adicionar(tS: t, eUs: e), let ancora = horaDaAncora {
            // O portão abriu agora: a referência volta ao que era na âncora.
            let correcao = portao.inclinacaoPpm * (Double(horaDoMinimoDeReferencia) - Double(ancora)) / 1e6
            referencia -= correcao
            minimoDeReferencia = referencia
            correcaoDaReferenciaUs = correcao
            e = minimo - referencia
            erroUs = e
        }
        guard portao.aberto, disciplina.ligada else { return }
        // O socorro: duas janelas seguidas acima de 40 ms.
        if abs(e) > Self.limiarDoSocorroUs {
            janelasAcimaDoSocorro += 1
            if janelasAcimaDoSocorro >= 2 {
                janelasAcimaDoSocorro = 0
                socorros += 1
                degraus += 1
                // A referência fica: o degrau (ou o corte) leva o carimbo de volta a ela, e a
                // janela seguinte vê o erro zerado. (A primeira versão a movia junto, e a janela
                // seguinte via −e: 86 socorros em 30 min a −1 500 ppm, erro de 1,2 s, no teste.)
                if e > 0 {
                    // A linha atrás do host: degrau para a frente.
                    degrausUs += e.rounded()
                } else {
                    // Nunca para trás: corta a entrada.
                    sinc.pularEntrada(UInt64(-e * taxa / 1e6))
                    amostrasCortadas += UInt64(-e * taxa / 1e6)
                }
                disciplina.reancorar()
                sinc.definirAjuste(disciplina.u)
                return
            }
        } else {
            janelasAcimaDoSocorro = 0
        }
        disciplina.atualizar(agoraS: t, eUs: e)
        sinc.definirAjuste(disciplina.u)
    }

    /// O carimbo do próximo quadro do fatiador (µs do host).
    public mutating func carimboDoProximoQuadro(duracaoUs: UInt64) -> UInt64 {
        let c = UInt64(ancoraUs ?? 0) + quadrosFeitos * duracaoUs + UInt64(degrausUs)
        quadrosFeitos += 1
        return c
    }

    /// A referência da entrega recomeça (uma reabertura, ou o PTS que recomeçou de outra base): a
    /// janela, a folga da âncora, a conferência e o portão; a frequência fica.
    private mutating func recomecarAReferencia(_ agoraUs: UInt64) {
        reancoragens += 1
        minimoDeReferencia = nil
        horaDaAncora = agoraUs
        inicioDaJanelaUs = nil
        minimoDaJanela = .infinity
        folgaNaAncora = nil
        conferirContra = nil
        portao.recomecar()
        disciplina.reancorar()
    }

    /// Quantas amostras o fatiador jogou fora num degrau (já contadas em `descartadas` pela linha).
    public var amostrasDescartadas: UInt64 { descartadas }
}
