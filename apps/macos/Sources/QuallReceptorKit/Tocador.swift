import AudioToolbox
import CoreAudio
import Foundation
import os

/// **O motor que puxa a porta na cadência do DAC** (`docs/som-no-receptor.md` §7.2).
///
///     fonte (render nosso) → AUVarispeed (a razão, e a conversão para a taxa do dispositivo)
///                          → DefaultOutput (a saída padrão, só de saída)
///
/// # Por que não o `AVAudioEngine`
///
/// A primeira versão da S4 usava `AVAudioEngine` com `AVAudioSourceNode`. A troca **não** tirou a
/// checagem prévia de microfone do TCC que o app faz uma vez por processo: ela vem na primeira
/// chamada ao HAL do CoreAudio, em qualquer cliente, e foi medida num app de controle sem código de
/// entrada nenhum (`docs/som-no-receptor.md` §17.8; a suposição de que era do `AVAudioEngine` caiu
/// no controle). É prévia (`preflight=yes`), sem diálogo e sem acesso, e o `provar-som.sh` reprova
/// tudo o que passar disso.
///
/// Ganhos da troca:
/// - **a conversão para a taxa do dispositivo é do Varispeed**, que carrega a fração entre ciclos.
///   O conversor do misturador do motor pedia um número inteiro de quadros por ciclo: 85 em vez de
///   85,33 a 8 kHz (−3 906 ppm, §17.3), e numa saída a 44,1 kHz a conta dá −500,5 ppm, colada no
///   teto (crítica 9, M6);
/// - **o ganho é nosso**, aplicado no render: mudo e volume valem desde a primeira amostra, antes de
///   ligar (crítica 9, M5);
/// - **a hora do DAC vem da saída**: o `mHostTime` do ciclo da `DefaultOutput`, mais a latência do
///   dispositivo e do fluxo, mais a latência do Varispeed (crítica 9, M8).
///
/// # A ordem, e a troca de saída
///
/// Tudo o que liga, desliga ou reconfigura roda numa **fila serial** do `Tocador`. `parar()` roda
/// nela de forma síncrona: levanta `parado`, tira os ouvintes e para a saída. Uma troca de saída
/// que chegue depois vê `parado` e não religa — antes, ela podia religar o motor que o desmonte
/// tinha parado, sobre uma porta já solta (crítica 9, G1). Uma religação que falha tenta de novo,
/// com espera crescente, e a janela diz "sem som" enquanto isso (crítica 9, M7).
///
/// **O motor nunca abre entrada.** Não há unidade de entrada, nem `inputNode`, e há teste que
/// confere o texto dos fontes.
public final class Tocador {

    public struct Formato: Equatable {
        public var canais: Int
        public var taxaHz: Double
        public var amostrasPorSlot: Int
        /// Atraso que a porta põe antes do motor (o filtro do interpolador do PCMU), somado à hora
        /// do DAC.
        public var atrasoInternoUs: Double
        public init(canais: Int, taxaHz: Double, amostrasPorSlot: Int, atrasoInternoUs: Double = 0) {
            self.canais = canais
            self.taxaHz = taxaHz
            self.amostrasPorSlot = amostrasPorSlot
            self.atrasoInternoUs = atrasoInternoUs
        }
    }

    public let formato: Formato
    public let montador: MontadorDeSaida
    /// O motor entrega sempre dois canais ao Varispeed; o montador repete o último canal do slot.
    static let canaisDoMotor = 2

    // --- o que o controle escreve e o render lê ---------------------------------------------------

    struct ParaORender {
        var razao: Double = 1
        var ganho: Float = 1
        /// A latência da saída (dispositivo + fluxo) mais a do Varispeed, em µs.
        var latenciaUs: Double = 0
        /// A duração do buffer de E/S, para quando o ciclo não traz `mHostTime`.
        var bufferUs: Double = 0
    }
    private let paraORender = OSAllocatedUnfairLock(initialState: ParaORender())

    // --- o que o render publica -------------------------------------------------------------------

    struct DoRender {
        var horaValida: UInt64 = 0
        var horaEstimada: UInt64 = 0
        /// Pico absoluto da saída (depois do ganho) desde a última leitura.
        var picoNaSaida: Float = 0
        /// A testemunha de fora da deriva (``TestemunhaDaDeriva``): os quadros que a saída pediu
        /// antes do último ciclo com hora, e a hora que o HAL deu a esse ciclo (ticks do host).
        var quadrosDoDac: UInt64 = 0
        var horaDoCicloDoDac: UInt64 = 0
    }
    private let doRender = OSAllocatedUnfairLock(initialState: DoRender())

    /// Estado só da thread do render. Fica num buffer fixo, fora do Swift gerenciado.
    struct DoCiclo {
        var horaDaSaidaTicks: UInt64 = 0
        var quadrosDoDac: UInt64 = 0
        var horaValida = false
        var visto = ParaORender()
        var ganhoAtual: Float = 1
        var local = DoRender()
    }
    private let ciclo: UnsafeMutablePointer<DoCiclo>
    private let ponteiros: UnsafeMutablePointer<UnsafeMutablePointer<Float>>
    private let vazio: UnsafeMutablePointer<Float>
    private let capacidadeDoVazio = 16_384

    // --- o que só a fila toca ---------------------------------------------------------------------

    private let fila = DispatchQueue(label: "br.com.queven.quall.tocador")
    private let chaveDaFila = DispatchSpecificKey<Bool>()
    private var saida: AudioUnit?
    private var varispeed: AudioUnit?
    private var parado = true
    private var taxaDaSaida: Double
    private var ouvinteDoPadrao: AudioObjectPropertyListenerBlock?
    private var ouvinteDaTaxa: AudioObjectPropertyListenerBlock?
    private var dispositivoOuvido = AudioObjectID(kAudioObjectUnknown)
    private var tentativa = 0
    /// Cada montagem leva um número novo. Uma nova tentativa agendada só vale se, quando dispara,
    /// nenhuma montagem aconteceu depois dela: sem isto, a tentativa agendada antes de uma troca de
    /// saída que ligou desmontava a saída que já tocava (reconferência da S4, miúdo 1).
    private var geracao: UInt64 = 0

    // --- o que a janela e o diário leem -----------------------------------------------------------

    struct Situacao {
        var ligado = false
        var taxaDaSaida: Double = 0
        var religamentos: UInt64 = 0
        var tentativasFalhas: UInt64 = 0
        var ultimaFalha = ""
    }
    private let situacao = OSAllocatedUnfairLock(initialState: Situacao())

    private let manual: Bool
    private let timebase: mach_timebase_info_data_t = {
        var t = mach_timebase_info_data_t()
        mach_timebase_info(&t)
        return t
    }()

    /// Diário. Chamado da fila do `Tocador`, nunca do render.
    public var aoRegistrar: ((String) -> Void)?

    /// Só para os testes: quantas vezes seguidas ligar a saída deve falhar.
    var falhasForcadasAoLigar = 0

    /// `manual`: sem dispositivo nenhum. Só o Varispeed existe, e ``renderizarManual(quadros:)`` o
    /// puxa como se fosse a saída, a `taxaDaSaidaManual`. É o DAC simulado dos testes, e ele **não**
    /// cria unidade de E/S nenhuma.
    public init(formato: Formato, manual: Bool = false, taxaDaSaidaManual: Double = 48_000,
                agoraUs: @escaping () -> UInt64 = Medidas.agoraUs,
                fonte: @escaping FonteDeSlots) {
        self.formato = formato
        self.manual = manual
        taxaDaSaida = taxaDaSaidaManual
        montador = MontadorDeSaida(canais: formato.canais, taxaHz: formato.taxaHz,
                                   amostrasPorSlot: formato.amostrasPorSlot,
                                   atrasoInternoUs: formato.atrasoInternoUs,
                                   agoraUs: agoraUs, fonte: fonte)
        ciclo = .allocate(capacity: 1)
        ciclo.initialize(to: DoCiclo())
        ponteiros = .allocate(capacity: Tocador.canaisDoMotor)
        vazio = .allocate(capacity: capacidadeDoVazio)
        vazio.initialize(repeating: 0, count: capacidadeDoVazio)
        fila.setSpecific(key: chaveDaFila, value: true)
    }

    /// Roda na fila; direto, se já estiver nela (o `deinit` pode cair dentro de um bloco da fila).
    private func naFila<R>(_ bloco: () -> R) -> R {
        DispatchQueue.getSpecific(key: chaveDaFila) == true ? bloco() : fila.sync(execute: bloco)
    }

    deinit {
        parar()
        naFila { desmontarUnidades() }
        ciclo.deallocate()
        ponteiros.deallocate()
        vazio.deallocate()
    }

    // MARK: - ligar e parar

    /// Monta a cadeia e liga. Devolve `false` com o motivo — **nunca lança**: um receptor sem som
    /// ainda exibe a imagem. Se a saída não ligar, tenta de novo sozinho, com espera crescente.
    @discardableResult
    public func iniciar() -> Bool {
        naFila {
            guard parado else { return true }
            parado = false
            tentativa = 0
            if !manual { ouvirASaidaPadrao() }
            return montarELigar()
        }
    }

    /// Para, **de forma síncrona**, na fila: depois de voltar, nenhuma religação acontece e a saída
    /// está parada. Idempotente.
    public func parar() {
        naFila {
            guard !parado else { return }
            parado = true
            pararDeOuvir()
            if let saida { AudioOutputUnitStop(saida) }
            situacao.withLock { $0.ligado = false }
        }
    }

    /// Desmonta o que houver, monta a cadeia e liga a saída. Só na fila.
    ///
    /// **Entre desmontar e ligar, nenhum render roda**: é a única janela em que o estado do ciclo
    /// pode ser escrito daqui. É nela que o ganho e as latências entram, **antes** do primeiro
    /// ciclo (reconferência da S4, miúdos 2 e 3: a latência era lida depois de ligar, e o ganho
    /// escrevia no ciclo de outra thread).
    private func montarELigar() -> Bool {
        geracao &+= 1
        do {
            desmontarUnidades()
            if falhasForcadasAoLigar > 0 {
                falhasForcadasAoLigar -= 1
                throw FalhaDoTocador.forcada
            }
            try montarUnidades()
            lerLatencias()
            let p = paraORender.withLock { $0 }
            ciclo.pointee.visto = p
            ciclo.pointee.ganhoAtual = p.ganho
            if let saida { try verificar(AudioOutputUnitStart(saida), "ligar a saída") }
            situacao.withLock { $0.ligado = true; $0.ultimaFalha = "" }
            tentativa = 0
            aoRegistrar?("som: saída ligada — " + descricaoDaSaida())
            return true
        } catch {
            let texto = "\(error)"
            situacao.withLock {
                $0.ligado = false
                $0.tentativasFalhas &+= 1
                $0.ultimaFalha = texto
            }
            aoRegistrar?("som: !! a saída não ligou (tentativa \(tentativa + 1)): \(texto)")
            agendarNovaTentativa()
            return false
        }
    }

    /// Esperas de 0,2 · 0,5 · 1 · 2 · 4 s; depois disso, desiste e a janela continua dizendo "sem
    /// som".
    private func agendarNovaTentativa() {
        let esperas = [0.2, 0.5, 1, 2, 4]
        guard tentativa < esperas.count else {
            aoRegistrar?("som: !! desisti de ligar a saída depois de \(esperas.count) tentativas")
            return
        }
        let espera = esperas[tentativa]
        tentativa += 1
        let minha = geracao
        fila.asyncAfter(deadline: .now() + espera) { [weak self] in
            guard let self, !self.parado, self.geracao == minha else { return }
            _ = self.montarELigar()
        }
    }

    /// A saída padrão mudou, ou a taxa dela. Só na fila. Com `parado`, não faz nada — é o que
    /// fecha a corrida do desmonte (crítica 9, G1).
    func trocaDeSaida(motivo: String) {
        guard !parado else { return }
        situacao.withLock { $0.religamentos &+= 1; $0.ligado = false }
        aoRegistrar?("som: \(motivo); remontando a saída")
        if let saida { AudioOutputUnitStop(saida) }
        if !manual { ouvirATaxaDoDispositivo() }
        tentativa = 0
        _ = montarELigar()
    }

    /// Só para os testes: a troca de saída, como o ouvinte a dispararia.
    func simularTrocaDeSaida() {
        naFila { trocaDeSaida(motivo: "troca simulada") }
    }

    // MARK: - as unidades

    enum FalhaDoTocador: Error, CustomStringConvertible {
        case status(OSStatus, String)
        case semComponente(String)
        case forcada
        var description: String {
            switch self {
            case let .status(s, o): return "\(o): OSStatus \(s)"
            case let .semComponente(o): return "sem o componente \(o)"
            case .forcada: return "falha forçada pelo teste"
            }
        }
    }

    private func verificar(_ s: OSStatus, _ oque: String) throws {
        guard s == noErr else { throw FalhaDoTocador.status(s, oque) }
    }

    private func nova(_ tipo: OSType, _ sub: OSType, _ nome: String) throws -> AudioUnit {
        var d = AudioComponentDescription(componentType: tipo, componentSubType: sub,
                                          componentManufacturer: kAudioUnitManufacturer_Apple,
                                          componentFlags: 0, componentFlagsMask: 0)
        guard let c = AudioComponentFindNext(nil, &d) else { throw FalhaDoTocador.semComponente(nome) }
        var u: AudioUnit?
        try verificar(AudioComponentInstanceNew(c, &u), "criar \(nome)")
        guard let u else { throw FalhaDoTocador.semComponente(nome) }
        return u
    }

    private func formatoFloat(_ taxa: Double) -> AudioStreamBasicDescription {
        AudioStreamBasicDescription(
            mSampleRate: taxa, mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagsNativeFloatPacked | kAudioFormatFlagIsNonInterleaved,
            mBytesPerPacket: 4, mFramesPerPacket: 1, mBytesPerFrame: 4,
            mChannelsPerFrame: UInt32(Tocador.canaisDoMotor), mBitsPerChannel: 32, mReserved: 0)
    }

    /// Só na fila.
    private func montarUnidades() throws {
        if !manual { taxaDaSaida = Tocador.saidaPadrao().taxa }
        if taxaDaSaida <= 0 { taxaDaSaida = 48_000 }
        let t = taxaDaSaida
        situacao.withLock { $0.taxaDaSaida = t }
        let v = try nova(kAudioUnitType_FormatConverter, kAudioUnitSubType_Varispeed, "AUVarispeed")
        varispeed = v
        var entrada = formatoFloat(formato.taxaHz)
        var saidaDoVarispeed = formatoFloat(taxaDaSaida)
        let tam = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
        try verificar(AudioUnitSetProperty(v, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, 0,
                                           &entrada, tam), "formato da fonte")
        // **A conversão de taxa é do Varispeed**: entrada na taxa da fonte, saída na do dispositivo.
        try verificar(AudioUnitSetProperty(v, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Output, 0,
                                           &saidaDoVarispeed, tam), "formato da saída do Varispeed")
        var maximo: UInt32 = 8_192
        _ = AudioUnitSetProperty(v, kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitScope_Global, 0,
                                 &maximo, UInt32(MemoryLayout<UInt32>.size))
        var chamada = AURenderCallbackStruct(inputProc: Tocador.renderDaFonte,
                                             inputProcRefCon: Unmanaged.passUnretained(self).toOpaque())
        try verificar(AudioUnitSetProperty(v, kAudioUnitProperty_SetRenderCallback, kAudioUnitScope_Input, 0,
                                           &chamada, UInt32(MemoryLayout<AURenderCallbackStruct>.size)),
                      "fonte do Varispeed")
        try verificar(AudioUnitInitialize(v), "iniciar o Varispeed")
        try verificar(AudioUnitSetParameter(v, kVarispeedParam_PlaybackRate, kAudioUnitScope_Global, 0,
                                            Float(paraORender.withLock { $0.razao }), 0), "razão")

        guard !manual else { return }
        let s = try nova(kAudioUnitType_Output, kAudioUnitSubType_DefaultOutput, "DefaultOutput")
        saida = s
        try verificar(AudioUnitSetProperty(s, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, 0,
                                           &saidaDoVarispeed, tam), "formato da saída")
        var conexao = AudioUnitConnection(sourceAudioUnit: v, sourceOutputNumber: 0, destInputNumber: 0)
        try verificar(AudioUnitSetProperty(s, kAudioUnitProperty_MakeConnection, kAudioUnitScope_Input, 0,
                                           &conexao, UInt32(MemoryLayout<AudioUnitConnection>.size)),
                      "ligar o Varispeed à saída")
        try verificar(AudioUnitAddRenderNotify(s, Tocador.avisoDaSaida,
                                               Unmanaged.passUnretained(self).toOpaque()), "aviso da saída")
        try verificar(AudioUnitInitialize(s), "iniciar a saída")
    }

    /// Só na fila (ou no `deinit`, com a saída parada).
    private func desmontarUnidades() {
        if let s = saida {
            AudioOutputUnitStop(s)
            AudioUnitRemoveRenderNotify(s, Tocador.avisoDaSaida, Unmanaged.passUnretained(self).toOpaque())
            AudioUnitUninitialize(s)
            AudioComponentInstanceDispose(s)
            saida = nil
        }
        if let v = varispeed {
            AudioUnitUninitialize(v)
            AudioComponentInstanceDispose(v)
            varispeed = nil
        }
    }

    /// A latência da saída (dispositivo + fluxo) e a do Varispeed. Só na fila.
    private func lerLatencias() {
        var latencia = 0.0
        if let v = varispeed {
            var s: Float64 = 0
            var tam = UInt32(MemoryLayout<Float64>.size)
            if AudioUnitGetProperty(v, kAudioUnitProperty_Latency, kAudioUnitScope_Global, 0, &s, &tam) == noErr {
                latencia += s
            }
        }
        var buffer = 0.0
        if !manual {
            let d = Tocador.saidaPadrao()
            latencia += d.latenciaS
            buffer = d.taxa > 0 ? Double(d.buffer) / d.taxa : 0
        }
        let latenciaUs = latencia * 1_000_000, bufferUs = buffer * 1_000_000
        paraORender.withLock {
            $0.latenciaUs = latenciaUs
            $0.bufferUs = bufferUs
        }
    }

    // MARK: - os ouvintes da saída padrão (só leitura do CoreAudio)

    private func ouvirASaidaPadrao() {
        var end = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                                             mScope: kAudioObjectPropertyScopeGlobal,
                                             mElement: kAudioObjectPropertyElementMain)
        let bloco: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.trocaDeSaida(motivo: "a saída padrão mudou")
        }
        if AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &end, fila, bloco) == noErr {
            ouvinteDoPadrao = bloco
        }
        ouvirATaxaDoDispositivo()
    }

    private func ouvirATaxaDoDispositivo() {
        pararDeOuvirATaxa()
        let id = Tocador.idDaSaidaPadrao()
        guard id != kAudioObjectUnknown else { return }
        var end = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyNominalSampleRate,
                                             mScope: kAudioObjectPropertyScopeGlobal,
                                             mElement: kAudioObjectPropertyElementMain)
        let bloco: AudioObjectPropertyListenerBlock = { [weak self] _, _ in
            self?.trocaDeSaida(motivo: "a taxa da saída mudou")
        }
        if AudioObjectAddPropertyListenerBlock(id, &end, fila, bloco) == noErr {
            ouvinteDaTaxa = bloco
            dispositivoOuvido = id
        }
    }

    private func pararDeOuvirATaxa() {
        guard let b = ouvinteDaTaxa, dispositivoOuvido != kAudioObjectUnknown else { return }
        var end = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyNominalSampleRate,
                                             mScope: kAudioObjectPropertyScopeGlobal,
                                             mElement: kAudioObjectPropertyElementMain)
        AudioObjectRemovePropertyListenerBlock(dispositivoOuvido, &end, fila, b)
        ouvinteDaTaxa = nil
        dispositivoOuvido = AudioObjectID(kAudioObjectUnknown)
    }

    private func pararDeOuvir() {
        if let b = ouvinteDoPadrao {
            var end = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                                                 mScope: kAudioObjectPropertyScopeGlobal,
                                                 mElement: kAudioObjectPropertyElementMain)
            AudioObjectRemovePropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &end, fila, b)
            ouvinteDoPadrao = nil
        }
        pararDeOuvirATaxa()
    }

    // MARK: - razão e ganho (do controle)

    /// A razão que o núcleo sugeriu. `NaN` (não medida) vira 1. Limitada a ±500 ppm, o limite do
    /// próprio núcleo.
    public func ajustarRazao(_ sugerida: Double) {
        let r = sugerida.isFinite ? min(max(sugerida, 1 - 500e-6), 1 + 500e-6) : 1
        // O parâmetro é `Float`: a razão que o render informa é a que o Varispeed recebe.
        let f = Float(r)
        paraORender.withLock { $0.razao = Double(f) }
        fila.async { [weak self] in
            guard let self, let v = self.varispeed else { return }
            AudioUnitSetParameter(v, kVarispeedParam_PlaybackRate, kAudioUnitScope_Global, 0, f, 0)
        }
    }

    public var razaoAplicada: Double { paraORender.withLock { $0.razao } }

    /// O ganho, de 0 a 1 (``VontadeDoSom/ganho``). **Vale antes de ligar**: o `montarELigar` põe
    /// o ganho no ciclo antes do primeiro render. Com a saída ligada, o render vai até ele em rampa,
    /// dentro de um ciclo. Daqui só se escreve o que o render lê por `trylock`; o estado do ciclo é
    /// dele. Calar não para de puxar.
    public func aplicarGanho(_ g: Float) {
        let alvo = min(max(g, 0), 1)
        paraORender.withLock { $0.ganho = alvo }
    }

    // MARK: - o render (tempo real)

    /// A fonte do Varispeed: o montador puxa a porta. Tempo real: sem alocação, só `trylock`.
    private static let renderDaFonte: AURenderCallback = { refCon, _, _, _, quadros, lista in
        let eu = Unmanaged<Tocador>.fromOpaque(refCon).takeUnretainedValue()
        guard let lista else { return noErr }
        let abl = UnsafeMutableAudioBufferListPointer(lista)
        let n = min(abl.count, Tocador.canaisDoMotor)
        let q = Int(quadros)
        for i in 0..<n {
            eu.ponteiros[i] = abl[i].mData?.assumingMemoryBound(to: Float.self) ?? eu.vazio
        }
        let c = eu.ciclo
        if let v = eu.paraORender.withLockIfAvailable({ $0 }) { c.pointee.visto = v }
        let visto = c.pointee.visto
        // A hora do DAC: o ciclo da saída, mais as latências. Sem hora do ciclo (modo manual), o
        // buffer de E/S no lugar dela.
        var atraso = visto.latenciaUs
        if c.pointee.horaValida {
            let agora = mach_absolute_time()
            let t = c.pointee.horaDaSaidaTicks
            let adiante = t > agora ? t - agora : 0
            atraso += Double(adiante) * Double(eu.timebase.numer) / Double(eu.timebase.denom) / 1_000
            c.pointee.local.horaValida &+= 1
        } else {
            atraso += visto.bufferUs
            c.pointee.local.horaEstimada &+= 1
        }
        eu.montador.render(quadros: q, saida: UnsafeMutableBufferPointer(start: eu.ponteiros, count: max(n, 1)),
                           atrasoDaSaidaUs: atraso, razaoAplicada: visto.razao)
        // O ganho, em rampa de um ciclo até o alvo.
        let de = c.pointee.ganhoAtual, para = visto.ganho
        if de == para {
            if para != 1 {
                for i in 0..<n { let p = eu.ponteiros[i]; for k in 0..<q { p[k] *= para } }
            }
        } else {
            let passo = (para - de) / Float(max(q, 1))
            for i in 0..<n {
                let p = eu.ponteiros[i]
                var g = de
                for k in 0..<q { g += passo; p[k] *= g }
            }
            c.pointee.ganhoAtual = para
        }
        let local = c.pointee.local
        _ = eu.doRender.withLockIfAvailable { d in
            d.horaValida = local.horaValida
            d.horaEstimada = local.horaEstimada
        }
        return noErr
    }

    /// O aviso da saída: antes do ciclo, a hora dele; depois, o pico do que saiu.
    private static let avisoDaSaida: AURenderCallback = { refCon, flags, carimbo, _, quadros, lista in
        let eu = Unmanaged<Tocador>.fromOpaque(refCon).takeUnretainedValue()
        let c = eu.ciclo
        if flags.pointee.contains(.unitRenderAction_PreRender) {
            let valida = carimbo.pointee.mFlags.contains(.hostTimeValid)
            c.pointee.horaValida = valida
            c.pointee.horaDaSaidaTicks = carimbo.pointee.mHostTime
            if valida {
                let q = c.pointee.quadrosDoDac, h = carimbo.pointee.mHostTime
                _ = eu.doRender.withLockIfAvailable { d in
                    d.quadrosDoDac = q
                    d.horaDoCicloDoDac = h
                }
            }
            c.pointee.quadrosDoDac &+= UInt64(quadros)
        } else if flags.pointee.contains(.unitRenderAction_PostRender), let lista {
            let abl = UnsafeMutableAudioBufferListPointer(lista)
            if let p = abl.first?.mData?.assumingMemoryBound(to: Float.self) {
                var pico = c.pointee.local.picoNaSaida
                for k in 0..<Int(quadros) { pico = max(pico, abs(p[k])) }
                c.pointee.local.picoNaSaida = pico
                let visto = pico
                if eu.doRender.withLockIfAvailable({ $0.picoNaSaida = max($0.picoNaSaida, visto) }) != nil {
                    c.pointee.local.picoNaSaida = 0
                }
            }
        }
        return noErr
    }

    // MARK: - o DAC simulado

    /// Modo manual: puxa `quadros` quadros da cadeia (fonte → Varispeed), na taxa da saída
    /// simulada. Devolve os dois canais, ou `nil` se o motor não está ligado.
    public func renderizarManual(quadros: Int) -> [[Float]]? {
        naFila {
            guard manual, !parado, let v = varispeed else { return nil }
            let canais = Tocador.canaisDoMotor
            let buffers = (0..<canais).map { _ in UnsafeMutablePointer<Float>.allocate(capacity: quadros) }
            defer { buffers.forEach { $0.deallocate() } }
            let lista = AudioBufferList.allocate(maximumBuffers: canais)
            defer { free(lista.unsafeMutablePointer) }
            for i in 0..<canais {
                lista[i] = AudioBuffer(mNumberChannels: 1, mDataByteSize: UInt32(quadros * 4),
                                       mData: UnsafeMutableRawPointer(buffers[i]))
            }
            var carimbo = AudioTimeStamp()
            carimbo.mSampleTime = amostrasManuais
            carimbo.mFlags = .sampleTimeValid
            var flags = AudioUnitRenderActionFlags()
            let s = AudioUnitRender(v, &flags, &carimbo, 0, UInt32(quadros), lista.unsafeMutablePointer)
            amostrasManuais += Double(quadros)
            guard s == noErr else { return nil }
            return buffers.map { Array(UnsafeBufferPointer(start: $0, count: quadros)) }
        }
    }
    private var amostrasManuais = 0.0

    // MARK: - o que relatar

    public struct Retrato {
        public var montador: MontadorDeSaida.Retrato
        public var razao: Double
        public var ganho: Float
        public var latenciaDeSaidaMs: Double
        public var bufferDeSaidaMs: Double
        public var horaDoRenderValida: UInt64
        public var horaDoRenderEstimada: UInt64
        /// O pico desde a última ``Tocador/tirarPicoNaSaida()``, sem zerar.
        public var picoNaSaida: Float
        public var religamentos: UInt64
        public var tentativasFalhas: UInt64
        public var ultimaFalha: String
        public var ligado: Bool
        public var taxaDaSaida: Double
        /// Quadros que a saída pediu antes do último ciclo com hora, e a hora do host (µs) que o
        /// HAL deu a esse ciclo: um ponto da ``TestemunhaDaDeriva``. Zero antes do primeiro ciclo.
        public var quadrosDoDac: UInt64
        public var horaDoCicloDoDacUs: UInt64
    }

    /// De qualquer thread. **Não** zera o pico: a janela também chama isto, e zerar daqui encolhia o
    /// `pico_na_saida` do relato de 1 Hz (reconferência da S4, miúdo 4). Quem relata usa
    /// ``tirarPicoNaSaida()``.
    public func retrato() -> Retrato {
        let p = paraORender.withLock { $0 }
        let r = doRender.withLock { $0 }
        let s = situacao.withLock { $0 }
        return Retrato(montador: montador.retrato(), razao: p.razao, ganho: p.ganho,
                       latenciaDeSaidaMs: p.latenciaUs / 1000, bufferDeSaidaMs: p.bufferUs / 1000,
                       horaDoRenderValida: r.horaValida, horaDoRenderEstimada: r.horaEstimada,
                       picoNaSaida: r.picoNaSaida, religamentos: s.religamentos,
                       tentativasFalhas: s.tentativasFalhas, ultimaFalha: s.ultimaFalha,
                       ligado: s.ligado, taxaDaSaida: s.taxaDaSaida,
                       quadrosDoDac: r.quadrosDoDac,
                       horaDoCicloDoDacUs: r.horaDoCicloDoDac
                           * UInt64(timebase.numer) / UInt64(timebase.denom) / 1_000)
    }

    public var ligado: Bool { situacao.withLock { $0.ligado } }

    /// O pico absoluto da saída desde a última chamada, e zera. **Só o relato de 1 Hz chama.**
    public func tirarPicoNaSaida() -> Float {
        doRender.withLock { v -> Float in let pico = v.picoNaSaida; v.picoNaSaida = 0; return pico }
    }

    public func descricaoDaSaida() -> String {
        if manual { return String(format: "modo manual (DAC simulado a %.0f Hz)", taxaDaSaida) }
        let s = Tocador.saidaPadrao()
        return String(format: "saída \"%@\" %.0f Hz buffer %d quadros, latência declarada %.1f ms, "
                      + "fonte %.0f Hz × %d, conversão no Varispeed",
                      s.nome, s.taxa, s.buffer, s.latenciaS * 1000, formato.taxaHz, formato.canais)
    }

    // MARK: - o dispositivo padrão, pelo CoreAudio (só leitura)

    static func idDaSaidaPadrao() -> AudioObjectID {
        var id = AudioObjectID(kAudioObjectUnknown)
        var tam = UInt32(MemoryLayout<AudioObjectID>.size)
        var end = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                                             mScope: kAudioObjectPropertyScopeGlobal,
                                             mElement: kAudioObjectPropertyElementMain)
        _ = AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &end, 0, nil, &tam, &id)
        return id
    }

    /// Nome, taxa nominal, buffer de E/S e latência (dispositivo + primeiro fluxo de saída).
    public static func saidaPadrao() -> (nome: String, taxa: Double, buffer: Int, latenciaS: Double) {
        let id = idDaSaidaPadrao()
        guard id != kAudioObjectUnknown else { return ("?", 0, 0, 0) }
        var end = AudioObjectPropertyAddress(mSelector: kAudioObjectPropertyName,
                                             mScope: kAudioObjectPropertyScopeGlobal,
                                             mElement: kAudioObjectPropertyElementMain)
        var nome: Unmanaged<CFString>?
        var tam = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        let temNome = AudioObjectGetPropertyData(id, &end, 0, nil, &tam, &nome) == noErr
        var taxa = Float64(0)
        tam = UInt32(MemoryLayout<Float64>.size)
        end.mSelector = kAudioDevicePropertyNominalSampleRate
        _ = AudioObjectGetPropertyData(id, &end, 0, nil, &tam, &taxa)
        var buffer = UInt32(0)
        tam = UInt32(MemoryLayout<UInt32>.size)
        end.mSelector = kAudioDevicePropertyBufferFrameSize
        end.mScope = kAudioObjectPropertyScopeOutput
        _ = AudioObjectGetPropertyData(id, &end, 0, nil, &tam, &buffer)
        var latDisp = UInt32(0)
        end.mSelector = kAudioDevicePropertyLatency
        _ = AudioObjectGetPropertyData(id, &end, 0, nil, &tam, &latDisp)
        // O primeiro fluxo de saída, e a latência dele.
        var latFluxo = UInt32(0)
        end.mSelector = kAudioDevicePropertyStreams
        var tamFluxos: UInt32 = 0
        if AudioObjectGetPropertyDataSize(id, &end, 0, nil, &tamFluxos) == noErr, tamFluxos > 0 {
            var fluxos = [AudioObjectID](repeating: 0, count: Int(tamFluxos) / MemoryLayout<AudioObjectID>.size)
            if AudioObjectGetPropertyData(id, &end, 0, nil, &tamFluxos, &fluxos) == noErr, let f = fluxos.first {
                var e2 = AudioObjectPropertyAddress(mSelector: kAudioStreamPropertyLatency,
                                                    mScope: kAudioObjectPropertyScopeGlobal,
                                                    mElement: kAudioObjectPropertyElementMain)
                var t2 = UInt32(MemoryLayout<UInt32>.size)
                _ = AudioObjectGetPropertyData(f, &e2, 0, nil, &t2, &latFluxo)
            }
        }
        let texto = temNome ? (nome?.takeRetainedValue() as String? ?? "?") : "?"
        let latencia = taxa > 0 ? Double(latDisp + latFluxo) / taxa : 0
        return (texto, taxa, Int(buffer), latencia)
    }

    /// `kAudioDevicePropertyActualSampleRate` da saída padrão: a taxa **medida** pelos carimbos do
    /// dispositivo (SDK, `AudioHardware.h`). Zero quando o dispositivo não está rodando.
    public static func taxaMedidaDaSaidaPadrao() -> Double {
        let id = idDaSaidaPadrao()
        guard id != kAudioObjectUnknown else { return 0 }
        var end = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyActualSampleRate,
                                             mScope: kAudioObjectPropertyScopeGlobal,
                                             mElement: kAudioObjectPropertyElementMain)
        var taxa = Float64(0)
        var tam = UInt32(MemoryLayout<Float64>.size)
        guard AudioObjectGetPropertyData(id, &end, 0, nil, &tam, &taxa) == noErr else { return 0 }
        return taxa
    }
}
