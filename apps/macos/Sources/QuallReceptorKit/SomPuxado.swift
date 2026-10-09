import Foundation
import QuallIdiomaKit
import os

// =================================================================================================
// O som do receptor macOS, na parte que não precisa da fronteira C para ser testada
// (`docs/som-no-receptor.md` §7.0 e §7.2; S4).
//
// Aqui mora a lógica: que track é o quê, a tabela µ-law, o montador que transforma slots de 20 ms
// em buffers do tamanho que o dispositivo pede, a decisão do volume (mudo, volume, câmera do Quall
// em uso) e o resumo do Δ. O motor de áudio fica em `Tocador.swift`; a fronteira C (a porta puxada
// e o decodificador), em `QuallNetKit`. Mesma decisão que mantém este módulo sem `CQuall`: o que é
// lógica se testa sem o `.a` existir.
// =================================================================================================

/// A espécie de uma track, a partir do valor cru de `QuallTrackKind`. Os valores são ABI
/// (`quall.h`): 0 tela, 1 câmera, 2 microfone, 3 som do sistema.
public enum EspecieDeTrack: Equatable, CustomStringConvertible {
    case tela, camera, microfone, somDoSistema
    case desconhecida(UInt32)

    public init(bruto: UInt32) {
        switch bruto {
        case 0: self = .tela
        case 1: self = .camera
        case 2: self = .microfone
        case 3: self = .somDoSistema
        default: self = .desconhecida(bruto)
        }
    }

    public var eVideo: Bool { self == .tela || self == .camera }
    public var eAudio: Bool { self == .microfone || self == .somDoSistema }

    public var description: String {
        switch self {
        case .tela: return "tela"
        case .camera: return "câmera"
        case .microfone: return "microfone"
        case .somDoSistema: return "som do sistema"
        case .desconhecida(let v): return "desconhecida (\(v))"
        }
    }
}

/// O que fazer com uma track que chegou. **A espécie decide, e não a ordem de chegada**: a ordem
/// em que as tracks saem de `quall_session_next_track` não é contrato nenhum, e tomar a primeira
/// como vídeo deixava a imagem preta quando o som vinha primeiro (crítica 2, M1).
public enum DestinoDaTrack: Equatable {
    case video
    case audio
    /// Largar o handle e dizer por quê.
    case largar(String)
}

public func destinoDaTrack(_ especie: EspecieDeTrack, jaTemVideo: Bool, jaTemAudio: Bool) -> DestinoDaTrack {
    if especie.eVideo {
        return jaTemVideo ? .largar("segunda track de vídeo: a origem é fixa pela sessão") : .video
    }
    if especie.eAudio {
        return jaTemAudio ? .largar("segunda track de som: só uma é tocada") : .audio
    }
    return .largar("espécie \(especie) — nem vídeo nem som")
}

// -------------------------------------------------------------------------------------------------
// G.711 µ-law
// -------------------------------------------------------------------------------------------------

/// **A tabela de volta do G.711 µ-law.** O emissor do Mac manda PCMU (`SessaoDeEmissao.swift`), e
/// o decodificador da fronteira recusa PCMU de propósito: são vinte linhas na casca
/// (`quall_audio_decoder_new`). Sem esta tabela o Mac → Mac seria mudo (risco R6).
public enum MuLaw {
    /// ITU-T G.711, a expansão de 8 bits para 14 bits com sinal, escalada a 16 bits.
    public static func linear(_ u: UInt8) -> Int16 {
        let v = ~u
        var t = (Int32(v & 0x0F) << 3) + 0x84
        t <<= Int32((v & 0x70) >> 4)
        return Int16((v & 0x80) != 0 ? 0x84 - t : t - 0x84)
    }

    /// As 256 entradas, em `Float` de −1 a 1, calculadas uma vez. A thread do áudio só lê.
    public static let tabela: [Float] = (0...255).map { Float(linear(UInt8($0))) / 32768 }
}

// -------------------------------------------------------------------------------------------------
// De 8 kHz para 48 kHz, na casca
// -------------------------------------------------------------------------------------------------

/// **Interpolação por 6, de 8 kHz para 48 kHz**, com um FIR polifásico de 144 coeficientes (24 por
/// fase), janela de Kaiser (β = 7), corte em 4 kHz.
///
/// # Por que a casca converte, e não o motor
///
/// Medido em 18/09/2026 neste MacBook, em tempo real: com o `AVAudioEngine` e a fonte a 8 kHz, o
/// motor pedia **exatamente 85 quadros por ciclo** de 512, e não 85,33 na média; a fonte era
/// consumida 0,39 % devagar (§17.3 do `som-no-receptor.md`). O motor de agora converte no
/// Varispeed, que carrega a fração, mas a porta continua entregando 48 kHz: é o formato da fonte
/// para o Opus também, e um formato só é um caminho só.
///
/// # O filtro
///
/// A primeira versão tinha 48 coeficientes (Blackman, corte em 3,6 kHz) e deixava a imagem de um tom
/// perto da borda só 17 dB abaixo dele a 3 150 Hz, e 12 dB a 3 400 Hz (crítica 9, miúdo 2). Com 144
/// coeficientes de Kaiser a transição é de ~1,4 kHz em volta de 4 kHz: o teste confere a imagem de
/// 3 150 Hz e a de 1 kHz. O atraso de grupo é de 71,5 amostras a 48 kHz, **1,49 ms**, e entra na
/// hora do DAC (`Tocador.Formato.atrasoInternoUs`).
public struct InterpoladorPor6 {
    public static let fator = 6
    public static let porFase = 24
    public static let atrasoUs: Double =
        Double(fator * porFase - 1) / 2 / 48_000 * 1_000_000

    private let coeficientes: UnsafeMutablePointer<Float>
    private let historia: UnsafeMutablePointer<Float>
    private var cabeca = 0

    /// A função de Bessel modificada de ordem zero, pela série.
    private static func i0(_ x: Double) -> Double {
        var soma = 1.0, termo = 1.0, k = 1.0
        while termo > 1e-12 * soma {
            termo *= (x / (2 * k)) * (x / (2 * k))
            soma += termo
            k += 1
        }
        return soma
    }

    public init() {
        let n = InterpoladorPor6.fator * InterpoladorPor6.porFase
        coeficientes = .allocate(capacity: n)
        let corte = 4_000.0 / 48_000.0
        let beta = 7.0
        var soma = 0.0
        var h = [Double](repeating: 0, count: n)
        let meio = Double(n - 1) / 2
        for m in 0..<n {
            let x = Double(m) - meio
            let sinc = x == 0 ? 2 * corte : sin(2 * .pi * corte * x) / (.pi * x)
            let r = x / meio
            let janela = InterpoladorPor6.i0(beta * (1 - r * r).squareRoot()) / InterpoladorPor6.i0(beta)
            h[m] = sinc * janela
            soma += h[m]
        }
        // Ganho de DC igual ao fator: a entrada é "esticada" com zeros, e cada fase soma um sexto.
        for m in 0..<n { coeficientes[m] = Float(h[m] / soma * Double(InterpoladorPor6.fator)) }
        historia = .allocate(capacity: InterpoladorPor6.porFase)
        historia.initialize(repeating: 0, count: InterpoladorPor6.porFase)
    }

    /// Libera os buffers. Chame uma vez, quando ninguém mais processa.
    public func liberar() {
        coeficientes.deallocate()
        historia.deallocate()
    }

    /// `n` amostras de 8 kHz em `entrada` → `6n` de 48 kHz em `saida` (mono). Não aloca.
    public mutating func processar(_ entrada: UnsafePointer<Float>, _ n: Int, _ saida: UnsafeMutablePointer<Float>) {
        let f = InterpoladorPor6.fator, k = InterpoladorPor6.porFase
        for i in 0..<n {
            cabeca = (cabeca + 1) % k
            historia[cabeca] = entrada[i]
            for p in 0..<f {
                var y: Float = 0
                var h = cabeca
                for j in 0..<k {
                    y += coeficientes[p + f * j] * historia[h]
                    h = h == 0 ? k - 1 : h - 1
                }
                saida[i * f + p] = y
            }
        }
    }

    /// A última amostra de 8 kHz que entrou, para a rampa do silêncio do PCMU.
    public var ultimaEntrada: Float { historia[cabeca] }
}

// -------------------------------------------------------------------------------------------------
// O slot puxado
// -------------------------------------------------------------------------------------------------

/// A ordem que a porta puxada devolveu (os valores de `QuallAudioOrder`, que são ABI).
public enum OrdemPuxada: UInt32 {
    case quadro = 0, cura = 1, silencio = 2, ocioso = 3
}

/// O que a fonte fez numa puxada: a ordem, quantas amostras **por canal** escreveu no destino, e o
/// carimbo do slot (`timestamp_us`, 0 no ocioso).
public struct SlotPuxado: Equatable {
    public var ordem: OrdemPuxada
    public var amostrasPorCanal: Int
    public var timestampUs: UInt64

    public init(ordem: OrdemPuxada, amostrasPorCanal: Int, timestampUs: UInt64) {
        self.ordem = ordem
        self.amostrasPorCanal = amostrasPorCanal
        self.timestampUs = timestampUs
    }
}

/// A fonte de slots: puxa **um** slot de 20 ms da porta e o decodifica em `destino`, PCM
/// **intercalado** em `Float`, com `capacidade` amostras (todas as dos canais). Roda na thread de
/// tempo real do dispositivo: não pode alocar nem esperar cadeado (`docs/contrato-som-puxado.md`
/// §2).
///
/// `atrasoAteODacUs` e `razaoAplicada` vão direto para `quall_audio_playout_pull`.
///
/// `noDacUs` é a hora do host (µs) em que a primeira amostra do slot sai no DAC, lida **uma vez**
/// pelo montador: quem precisa da hora (o detector da claquete) usa esta, e não uma segunda leitura
/// do relógio (crítica 9, miúdo 13).
public typealias FonteDeSlots = (_ atrasoAteODacUs: UInt32, _ noDacUs: UInt64, _ razaoAplicada: Double,
                                 _ destino: UnsafeMutablePointer<Float>, _ capacidade: Int) -> SlotPuxado

// -------------------------------------------------------------------------------------------------
// O montador: slots de 20 ms → buffers do tamanho que o dispositivo pede
// -------------------------------------------------------------------------------------------------

/// **A casca do lado do render.** O dispositivo pede N quadros por ciclo (512, 441, 4 096…); a
/// porta entrega slots de 20 ms. O montador guarda a sobra do slot e só puxa quando ela acaba — é
/// isso que faz a porta ser puxada **na cadência do dispositivo**, que é o ponto de tudo
/// (`docs/som-no-receptor.md` §3).
///
/// # O atraso até o DAC
///
/// Quem chama diz, para cada render, quanto falta até a **primeira** amostra deste render sair no
/// DAC (`atrasoDaSaidaUs`). Uma puxada feita no meio do render soma a esse número o que ainda vai
/// tocar antes do slot novo: as amostras já escritas neste render. Com o Varispeed no meio, cada
/// amostra da fonte dura `1 / (taxa × razão)` — é a razão que faz a fonte andar mais depressa.
///
/// # Tempo real
///
/// `render` não aloca nem espera: a sobra é um buffer fixo, alocado no `init`, e o registro do
/// último slot para o Δ usa `withLockIfAvailable` — se o leitor estiver com o cadeado, o registro
/// fica para a puxada seguinte.
///
/// `atrasoInternoUs` é o que a porta põe entre o slot e o motor (o filtro do interpolador do
/// PCMU). Entra no atraso de toda puxada e na hora do DAC.
public final class MontadorDeSaida {

    public let canais: Int
    public let taxaHz: Double
    public let amostrasPorSlot: Int
    public let atrasoInternoUs: Double

    private let fonte: FonteDeSlots
    private let sobra: UnsafeMutablePointer<Float>
    private let capacidade: Int
    private var inicio = 0
    private var fim = 0

    /// O que o render viu, para quem relata. Protegido por `trava`.
    public struct Retrato: Equatable {
        public var renders: UInt64 = 0
        public var quadrosPedidos: UInt64 = 0
        public var puxadas: UInt64 = 0
        public var ociosas: UInt64 = 0
        public var silencios: UInt64 = 0
        public var curas: UInt64 = 0
        /// O maior pedido de quadros num render: é o quantum do dispositivo visto daqui.
        public var maiorRender = 0
        public var menorRender = Int.max
        /// O último slot de quadro tocado: o carimbo dele e a hora do host (µs) em que a primeira
        /// amostra dele sai no DAC. É o lado do som do Δ.
        public var ultimoCarimboUs: UInt64 = 0
        public var ultimoNoDacUs: UInt64 = 0
        public var temUltimo = false
        public init() {}
    }

    private var local = Retrato()
    private let publicado = OSAllocatedUnfairLock(initialState: Retrato())

    /// `agoraUs` é o relógio do host em µs (o mesmo de `Medidas.agoraUs`), injetado para o teste.
    private let agoraUs: () -> UInt64

    public init(canais: Int, taxaHz: Double, amostrasPorSlot: Int, atrasoInternoUs: Double = 0,
                agoraUs: @escaping () -> UInt64 = Medidas.agoraUs,
                fonte: @escaping FonteDeSlots) {
        precondition(canais > 0 && amostrasPorSlot > 0)
        self.canais = canais
        self.taxaHz = taxaHz
        self.amostrasPorSlot = amostrasPorSlot
        self.atrasoInternoUs = atrasoInternoUs
        self.fonte = fonte
        self.agoraUs = agoraUs
        capacidade = amostrasPorSlot * canais
        sobra = .allocate(capacity: capacidade)
        sobra.initialize(repeating: 0, count: capacidade)
    }

    deinit {
        sobra.deallocate()
    }

    /// Escreve `quadros` quadros em `saida`: um ponteiro por canal, PCM não intercalado em `Float`
    /// (o formato do Varispeed). Com menos ponteiros que canais, os canais a mais do
    /// slot são descartados; com mais, os de sobra recebem o último canal.
    public func render(quadros: Int, saida: UnsafeMutableBufferPointer<UnsafeMutablePointer<Float>>,
                       atrasoDaSaidaUs: Double, razaoAplicada: Double) {
        let razao = razaoAplicada.isFinite && razaoAplicada > 0 ? razaoAplicada : 1
        local.renders &+= 1
        local.quadrosPedidos &+= UInt64(quadros)
        local.maiorRender = max(local.maiorRender, quadros)
        local.menorRender = min(local.menorRender, quadros)
        var escritos = 0
        while escritos < quadros {
            if inicio == fim {
                let atraso = max(0, atrasoDaSaidaUs + atrasoInternoUs
                                     + Double(escritos) / (taxaHz * razao) * 1_000_000)
                let noDac = agoraUs() &+ UInt64(atraso)
                let slot = fonte(UInt32(clamping: Int64(atraso)), noDac, razaoAplicada, sobra, capacidade)
                local.puxadas &+= 1
                var n = min(max(slot.amostrasPorCanal, 0), amostrasPorSlot)
                switch slot.ordem {
                case .ocioso:
                    local.ociosas &+= 1
                    // Zeros, e não ocultação: não há fluxo tocando (`contrato-som-puxado.md` §1).
                    sobra.update(repeating: 0, count: capacidade)
                    n = amostrasPorSlot
                case .silencio:
                    local.silencios &+= 1
                case .cura:
                    local.curas &+= 1
                case .quadro:
                    local.ultimoCarimboUs = slot.timestampUs
                    local.ultimoNoDacUs = noDac
                    local.temUltimo = true
                }
                if n == 0 {
                    // Uma fonte que não escreveu nada ainda ocupa o slot: 20 ms de silêncio, para
                    // a cadência das puxadas não mudar por causa de uma falha do decodificador.
                    sobra.update(repeating: 0, count: capacidade)
                    n = amostrasPorSlot
                }
                inicio = 0
                fim = n
            }
            let k = min(quadros - escritos, fim - inicio)
            for c in 0..<saida.count {
                let canalDoSlot = min(c, canais - 1)
                let destino = saida[c].advanced(by: escritos)
                var origem = inicio * canais + canalDoSlot
                for i in 0..<k {
                    destino[i] = sobra[origem]
                    origem += canais
                }
            }
            inicio += k
            escritos += k
        }
        let copia = local
        _ = publicado.withLockIfAvailable { $0 = copia }
    }

    /// O último retrato publicado pelo render. De qualquer thread.
    public func retrato() -> Retrato {
        publicado.withLock { $0 }
    }

    /// Quadros ainda guardados, que vão tocar antes da próxima puxada.
    public var quadrosNaSobra: Int { fim - inicio }
}

// -------------------------------------------------------------------------------------------------
// O volume: D1 e D3 do §12.1
// -------------------------------------------------------------------------------------------------

/// O que se sabe da câmera do Quall, pelo CoreMediaIO.
///
/// # A leitura da D3 (§12.1; decisão do coordenador de 18/09, noite)
///
/// A D3 do Bruno é "mudo enquanto um app usa a webcam do Quall". O CoreMediaIO só diz se o
/// **dispositivo** está rodando (`kCMIODevicePropertyDeviceIsRunningSomewhere`), e o app que
/// alimenta a câmera (`QuallCamera.app`, `CMIODeviceStartStream` no fluxo de entrada,
/// `ClienteDoSumidouro.swift`) também o põe para rodar: daqui não dá para separar o alimentador de
/// quem assiste. A primeira resposta calava só com o app da câmera fechado — o que inverte o caso
/// realista, porque uma chamada que **mostra imagem** pela câmera do Quall sempre tem o app aberto
/// (reconferência da S4, crítica 11). A leitura agora é a conservadora: **rodando é em uso, e
/// cala**, com o app da câmera aberto ou não. A chave da janela, "tocar mesmo com a câmera do
/// Quall numa chamada" (`--som-com-camera`), desfaz. Quando a extensão publicar quem consome
/// (`clientesNaSaida`), a leitura pode afinar.
public enum EstadoDaCameraDoQuall: Equatable {
    /// Não instalada, ou não se deixou ler.
    case ausente
    /// Instalada e parada.
    case livre
    /// Rodando: um app a usa, ou o app da câmera a alimenta. O D3 cala.
    case emUso

    /// `rodando` é o `IsRunningSomewhere` do dispositivo, `nil` quando a câmera não foi achada.
    public init(rodando: Bool?) {
        switch rodando {
        case nil: self = .ausente
        case false?: self = .livre
        case true?: self = .emUso
        }
    }

    /// Para o diário.
    public var descricao: String {
        switch self {
        case .ausente: return "ausente"
        case .livre: return "livre"
        case .emUso: return "em_uso"
        }
    }
}

/// **O volume que sai, a partir das vontades.** D1: som ligado por padrão, com mudo e volume na
/// janela. D3: mudo automático enquanto a câmera do Quall estiver rodando (a leitura acima), para o
/// som do aparelho não vazar para a chamada pelo microfone. Mudo não para de puxar: a porta
/// continua ancorada, e voltar o som não reancora.
///
/// **O D3 tem saída na janela** (`tocarComACamera`): a primeira versão calava sem volta, e o botão
/// de mudo não desfazia (crítica 9, M4).
public struct VontadeDoSom: Equatable {
    public var mudo: Bool
    public var volume: Float
    public var camera: EstadoDaCameraDoQuall
    /// Tocar mesmo com a câmera do Quall em uso. A chave da janela, e o `--som-com-camera` da
    /// bancada.
    public var tocarComACamera: Bool

    public init(mudo: Bool = false, volume: Float = 1, camera: EstadoDaCameraDoQuall = .livre,
                tocarComACamera: Bool = false) {
        self.mudo = mudo
        self.volume = volume
        self.camera = camera
        self.tocarComACamera = tocarComACamera
    }

    /// O D3 está calando agora.
    public var caladoPelaCamera: Bool { camera == .emUso && !tocarComACamera }

    /// O ganho que vai para a saída, de 0 a 1.
    public var ganho: Float {
        if mudo || caladoPelaCamera { return 0 }
        return min(max(volume, 0), 1)
    }

    /// Por que está calado, para a janela e o diário; `nil` quando toca.
    public var porQueCalado: String? {
        if mudo { return T("mudo") }
        if caladoPelaCamera { return T("mudo: a câmera do Quall está ligada (numa chamada, ou pelo app dela)") }
        if volume <= 0 { return T("volume zero") }
        return nil
    }
}

// -------------------------------------------------------------------------------------------------
// O Δ: som contra imagem, para o mesmo instante capturado
// -------------------------------------------------------------------------------------------------

/// **Δ = t_som − t_imagem para o mesmo instante capturado** (`docs/som-no-receptor.md` §9.1).
/// Positivo é som atrasado.
///
/// Cada lado é uma latência "da captura até a saída": `saída no host − captura no relógio da
/// sessão`. As duas têm a mesma constante desconhecida (a diferença entre o relógio do host e o do
/// emissor), que some na subtração. A captura vem do relógio comum
/// (`quall_track_capture_offset_us`), e só vale com o par válido.
public enum Delta {
    /// `(captura µs, saída no host µs)` de cada lado → Δ em µs.
    public static func us(som: (captura: Int64, saida: UInt64), imagem: (captura: Int64, saida: UInt64)) -> Int64 {
        let latenciaDoSom = Int64(bitPattern: som.saida) - som.captura
        let latenciaDaImagem = Int64(bitPattern: imagem.saida) - imagem.captura
        return latenciaDoSom - latenciaDaImagem
    }

    /// `n`, p05, p50 e p95 em ms, no formato do diário. O critério do §9.1 é p05 ≥ −45 ms e
    /// p95 ≤ +125 ms.
    public static func resumo(_ amostrasUs: [Int64]) -> String {
        var a = Acumulador()
        for v in amostrasUs { a.somar(v) }
        return a.resumo
    }

    /// **O Δ de uma sessão inteira em memória fixa**: um histograma de 0,2 ms de −2 s a +3 s, mais
    /// o mínimo e o máximo exatos. A primeira versão guardava toda amostra numa lista e a ordenava
    /// inteira a cada segundo, em toda sessão de produto (crítica 9, miúdo 9).
    public struct Acumulador {
        static let passoUs: Int64 = 200
        static let deUs: Int64 = -2_000_000
        static let caixas = 25_000
        private var contagem = [UInt32](repeating: 0, count: Acumulador.caixas)
        public private(set) var n = 0
        public private(set) var minimoUs = Int64.max
        public private(set) var maximoUs = Int64.min
        public private(set) var foraDaFaixa = 0

        public init() {}

        public mutating func somar(_ us: Int64) {
            n += 1
            minimoUs = min(minimoUs, us)
            maximoUs = max(maximoUs, us)
            var k = Int((us - Acumulador.deUs) / Acumulador.passoUs)
            if k < 0 || k >= Acumulador.caixas {
                foraDaFaixa += 1
                k = min(max(k, 0), Acumulador.caixas - 1)
            }
            contagem[k] &+= 1
        }

        /// O percentil `p` (0 a 1), em ms, pelo centro da caixa.
        public func percentilMs(_ p: Double) -> Double {
            guard n > 0 else { return .nan }
            let alvo = Int((p * Double(n - 1)).rounded()) + 1
            var soma = 0
            for (k, c) in contagem.enumerated() {
                soma += Int(c)
                if soma >= alvo {
                    return Double(Acumulador.deUs + Int64(k) * Acumulador.passoUs + Acumulador.passoUs / 2) / 1000
                }
            }
            return Double(maximoUs) / 1000
        }

        public var resumo: String {
            guard n > 0 else { return "[n=0]" }
            return String(format: "[n=%d p05=%.1f p50=%.1f p95=%.1f min=%.1f max=%.1f%@]",
                          n, percentilMs(0.05), percentilMs(0.5), percentilMs(0.95),
                          Double(minimoUs) / 1000, Double(maximoUs) / 1000,
                          foraDaFaixa > 0 ? " fora=\(foraDaFaixa)" : "")
        }
    }
}

// -------------------------------------------------------------------------------------------------
// A claquete no som que sai
// -------------------------------------------------------------------------------------------------

/// **Acha o começo do estouro de 3 150 Hz da claquete** (`crates/quall-probe/src/claquete.rs`) no
/// som que a porta entrega ao motor. Goertzel em janelas de 2 ms (Hann, 96 amostras a 48 kHz) com
/// passo de 1 ms, e histerese: o estouro só conta depois de 20 ms abaixo do limiar baixo.
///
/// É o lado do som da claquete **dentro do app**: com a hora do DAC que a puxada já sabe, dá a hora
/// em que o estouro sai. Junto com a hora em que o quadro marcado entra na camada, mede o Δ por
/// evento — sem passar pelo relógio comum, que é o que ele confere.
///
/// Resolução: meio passo (±0,5 ms). O viés da janela (o estouro cruza o limiar antes de encher a
/// janela) é descontado por uma constante medida no teste.
public struct DetectorDeEstouro {
    public static let frequenciaHz = 3150.0
    public let taxaHz: Double
    public let janela: Int
    public let passo: Int
    public var limiarAlto: Float = 0.12
    public var limiarBaixo: Float = 0.05
    /// Amostras da cauda do slot anterior que ainda não viraram janela.
    private let acumulado: UnsafeMutablePointer<Float>
    private let pesos: UnsafeMutablePointer<Float>
    private let capacidade: Int
    private var guardadas = 0
    private var abaixoHa = 1_000
    private var ativo = false
    private let coef: Float
    private let somaDosPesos: Float
    /// Quantas amostras depois do começo da janela o estouro costuma começar, quando a janela é a
    /// primeira acima do limiar. Medido no teste com estouros em todas as fases do passo.
    public static let viesEmPassos: Double = 0.81

    public init(taxaHz: Double = 48_000, maiorSlot: Int = 1920) {
        self.taxaHz = taxaHz
        janela = Int(taxaHz * 0.002)
        passo = Int(taxaHz * 0.001)
        capacidade = janela + maiorSlot
        acumulado = .allocate(capacity: capacidade)
        acumulado.initialize(repeating: 0, count: capacidade)
        pesos = .allocate(capacity: janela)
        var soma: Float = 0
        for i in 0..<janela {
            let w = Float(0.5 - 0.5 * cos(2 * .pi * Double(i) / Double(janela - 1)))
            pesos[i] = w
            soma += w
        }
        somaDosPesos = soma
        coef = Float(2 * cos(2 * .pi * DetectorDeEstouro.frequenciaHz / taxaHz))
    }

    public func liberar() {
        acumulado.deallocate()
        pesos.deallocate()
    }

    /// A amplitude de 3 150 Hz na janela que começa em `p`.
    private func amplitude(_ p: Int) -> Float {
        var s1: Float = 0, s2: Float = 0
        for i in 0..<janela {
            let s0 = acumulado[p + i] * pesos[i] + coef * s1 - s2
            s2 = s1
            s1 = s0
        }
        let potencia = s1 * s1 + s2 * s2 - coef * s1 * s2
        return 2 * sqrt(max(potencia, 0)) / somaDosPesos
    }

    /// Processa um slot (`n` amostras de um canal, com `salto` entre elas: 1 para mono, o número de
    /// canais para intercalado). Devolve onde o estouro começou, em amostras **a partir do começo
    /// deste slot** (negativo se começou na cauda do anterior), ou `nil`. Não aloca.
    public mutating func processar(_ x: UnsafePointer<Float>, _ n: Int, salto: Int = 1) -> Double? {
        let m = min(n, capacidade - guardadas)
        for i in 0..<m { acumulado[guardadas + i] = x[i * salto] }
        let total = guardadas + m
        var achado: Double?
        var p = 0
        while p + janela <= total {
            let a = amplitude(p)
            if ativo {
                if a < limiarBaixo { abaixoHa += 1 } else { abaixoHa = 0 }
                if abaixoHa >= 20 { ativo = false }
            } else if a > limiarAlto {
                // Só arma depois de 20 ms abaixo do limiar baixo. As janelas da borda de subida,
                // entre os dois limiares, não desarmam: sem isto, o estouro que começava no meio
                // de uma janela passava despercebido (achado pelo teste, em 4 fases de 24).
                if abaixoHa >= 20, achado == nil {
                    achado = Double(p - guardadas) + DetectorDeEstouro.viesEmPassos * Double(passo)
                }
                ativo = true
                abaixoHa = 0
            } else if a < limiarBaixo {
                abaixoHa += 1
            }
            p += passo
        }
        // O que sobrou depois da última janela vai para o começo, para a próxima.
        let resto = total - p
        // Cópia para a frente, amostra a amostra: as regiões se sobrepõem.
        for i in 0..<max(resto, 0) { acumulado[i] = acumulado[p + i] }
        guardadas = resto
        return achado
    }
}

// MARK: - A testemunha de fora da deriva

/// **A taxa do DAC pelo relógio do host**, com barra de erro (crítica 10, M9).
///
/// O `ed_drift_ppm` da porta é a inclinação do nível **mais** a razão que nós mesmos passamos ao
/// Varispeed: é o estimador do produto, e não testemunha dele (§9.6). Esta é de fora: os quadros
/// que a saída pediu até o começo de cada ciclo, contra a hora que o **HAL** dá ao ciclo
/// (`mHostTime`). Nem o estimador nem a razão entram nela.
///
/// Numa bancada em que o emissor anda no relógio do host (a sonda em `lo0`, com `--ppm-no-audio N`
/// injetando N), a deriva emissor → DAC esperada é `N − dac_vs_host`, e o `ed_drift_ppm` tem de
/// cair perto dela.
public struct TestemunhaDaDeriva {
    private var pontos: [(horaUs: Double, quadros: Double)] = []
    public init() {}

    public mutating func zerar() { pontos.removeAll(keepingCapacity: true) }

    /// Um par (hora do host do ciclo em µs, quadros pedidos antes dele). O mesmo ciclo lido duas
    /// vezes é ignorado; uma volta para trás (a saída religou) recomeça a série.
    public mutating func adicionar(horaUs: UInt64, quadros: UInt64) {
        guard horaUs > 0 else { return }
        if let u = pontos.last {
            if Double(horaUs) == u.horaUs { return }
            if Double(horaUs) < u.horaUs || Double(quadros) < u.quadros { pontos.removeAll() }
        }
        pontos.append((Double(horaUs), Double(quadros)))
        // Um ponto por segundo cresceria sem teto numa sessão longa (reconferência da S4, miúdo 7).
        // Passando do teto, fica um ponto a cada dois: a série mantém o começo e o fim, que é o que
        // dá a inclinação.
        if pontos.count > TestemunhaDaDeriva.maximoDePontos {
            pontos = pontos.enumerated().filter { $0.offset % 2 == 0 || $0.offset == pontos.count - 1 }
                .map(\.element)
        }
    }

    /// O teto da série: mais de uma hora a um ponto por segundo.
    public static let maximoDePontos = 4_096

    public var n: Int { pontos.count }

    /// `(ppm, erro padrão em ppm)` da taxa medida contra `taxaNominalHz`. `nil` com menos de 5
    /// pontos. Mínimos quadrados de quadros × hora; o erro é o da inclinação.
    public func dacContraHost(taxaNominalHz: Double) -> (ppm: Double, erro: Double)? {
        guard pontos.count >= 5, taxaNominalHz > 0 else { return nil }
        let x0 = pontos[0].horaUs, y0 = pontos[0].quadros
        let xs = pontos.map { ($0.horaUs - x0) / 1_000_000 }
        let ys = pontos.map { $0.quadros - y0 }
        let n = Double(pontos.count)
        let mx = xs.reduce(0, +) / n, my = ys.reduce(0, +) / n
        var sxx = 0.0, sxy = 0.0
        for i in 0..<pontos.count {
            sxx += (xs[i] - mx) * (xs[i] - mx)
            sxy += (xs[i] - mx) * (ys[i] - my)
        }
        guard sxx > 0 else { return nil }
        let b = sxy / sxx
        var ss = 0.0
        for i in 0..<pontos.count {
            let r = ys[i] - my - b * (xs[i] - mx)
            ss += r * r
        }
        let erroB = (ss / max(n - 2, 1) / sxx).squareRoot()
        return ((b / taxaNominalHz - 1) * 1e6, erroB / taxaNominalHz * 1e6)
    }

    /// Para o diário: `+3.91±0.02(n=30)`, ou `-` sem pontos bastantes.
    public func descricao(taxaNominalHz: Double) -> String {
        guard let r = dacContraHost(taxaNominalHz: taxaNominalHz) else { return "-" }
        return String(format: "%+.2f±%.2f(n=%d)", r.ppm, r.erro, n)
    }
}
