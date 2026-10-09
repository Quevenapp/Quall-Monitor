import CoreMedia
import CoreVideo
import Foundation
import QuallIdiomaKit

/// Captura → encode → **rede**, sem passar por disco.
///
/// `CaptureSession` (a peça do M1) faz captura → encode → **arquivo**, e existe para medir com
/// `ffprobe` do lado de fora. Ela continua intacta e continua sendo o instrumento de bancada.
/// Esta aqui é a mesma cadeia com o outro fim: entrega cada quadro Annex-B a um sumidouro e não
/// guarda nada. Não é uma refatoração de `CaptureSession` porque as duas querem coisas diferentes
/// do mesmo encoder — uma quer o sidecar completo por quadro, a outra quer nunca segurar um
/// buffer — e fundi-las custaria mais do que os ~40 pontos de código que elas repetem.
///
/// **Nada aqui escreve pixel em lugar nenhum.** O quadro nasce no ScreenCaptureKit, vira H.264 no
/// bloco de mídia e sai pela fronteira C. Não há caminho para arquivo, nem opção para criar um:
/// a origem desta cadeia é a tela de trabalho de uma pessoa (`docs/regras-de-frente.md`, "Um
/// vídeo de bancada pode conter a vida do usuário"), e a forma mais barata de nunca vazar isso é
/// não ter para onde gravar.
public final class TransmissaoAoVivo {
    /// O que o quadro codificado encontra do outro lado. Devolve `true` se o núcleo aceitou.
    /// Roda na fila do VideoToolbox — **não bloqueie**.
    public typealias Sumidouro = @Sendable (_ annexb: Data, _ timestampUs: UInt64, _ idr: Bool) -> Bool

    /// O sumidouro do áudio. **Um quadro por chamada** — `docs/audio.md` §6: o pacotizador de
    /// áudio da libdatachannel não fragmenta, então uma chamada com dois quadros vira um pacote
    /// que o outro lado decodifica errado, sem erro em lugar nenhum no caminho.
    public typealias SumidouroDeAudio = @Sendable (_ quadro: Data, _ timestampUs: UInt64) -> Bool

    /// A tela estendida captura com `minimumFrameInterval` de **meio** período (ver
    /// `ScreenCapturer.intervaloMinimoDeMeioPeriodo`). **Padrão desde 11/09**, decisão do usuário depois
    /// da medida no iPhone X pela Ethernet (sem perda de rede no meio): com um período, 169 e 207
    /// trancos; com meio, 9 e 41 — a taxa igual (~16/s sem Sidecar) e os quadros chegando regulares
    /// (intervalo p95 81 ms contra 104). Duas comparações de cada lado. `--captura-periodo-inteiro`
    /// volta ao de antes, para comparar. Lido uma vez, na abertura do app.
    #if QUALL_TELA_ESTENDIDA_FUTURA
    nonisolated(unsafe) public static var capturaComMeioPeriodo = true

    #endif
    /// Teto de bytes por quadro pedido ao encoder (`H264Encoder`, `tetoDeQuadroBytes`). **`nil` é o
    /// automático**: na tela estendida, ``tetoDeQuadroEmQuadrosMedios`` quadros médios; fora dela,
    /// nenhum. `0` desliga, e um número fixa. `--teto-quadro-kb`; lido uma vez, na abertura do app.
    nonisolated(unsafe) public static var tetoDeQuadroBytes: Int?

    /// **O teto da tela estendida, em quadros médios** (taxa ÷ 8 ÷ fps). Padrão desde 11/09/2026,
    /// pelas corridas D2–D4 (`docs/tela-estendida.md`, "De onde vêm os trancos"). No iPhone X (11,9
    /// Mbps a 30 fps) isto dá ~250 KB: os IDR programados foram de 294–399 KB para 243–294 KB, e os
    /// trancos de IDR de 18 para 1 em 4 min, sem artefato novo que o usuário visse. Em múltiplo do
    /// quadro médio, e não em bytes, para acompanhar a resolução: o teto de uma tela 5K não pode ser
    /// o de um telefone. Medido na tela e não na mesa: com conteúdo de mesa o VideoToolbox não
    /// obedeceu o valor (ver o doc).
    #if QUALL_TELA_ESTENDIDA_FUTURA
    public static let tetoDeQuadroEmQuadrosMedios = 5.0

    /// De quantos em quantos segundos a tela estendida manda um IDR programado. **30 desde
    /// 11/09/2026** (antes, 10): agora que toda perda pede IDR — o quadro sem cabeça passou a contar
    /// no núcleo —, o programado é só rede de segurança, e cada um custava um tranco no iPhone X. D3 e
    /// D4: 10 IDR em 4 min contra 26. `--gop-tela-estendida`; lido uma vez, na abertura do app.
    nonisolated(unsafe) public static var gopDaTelaEstendida = 30.0

    #endif
    public struct Contadores: Sendable {
        public var quadrosCapturados = 0
        public var quadrosEnviados = 0
        public var quadrosRecusados = 0
        public var idrsEnviados = 0
        public var bytesEnviados = 0
        public var latenciaMediaUs: Double = 0
        /// Quantas vezes o receptor pediu um quadro-chave e nós forçamos um.
        public var idrsForcados = 0
        /// Quadros que **não** vieram da captura: o último quadro repetido porque a tela ficou
        /// parada. Só a tela estendida repete — ver `intervaloDeRepeticao`.
        public var quadrosRepetidos = 0
        /// **Paradas do lado de cá**: intervalos acima de 100 ms entre quadros capturados (e entre
        /// quadros aceitos pela track), com o maior visto. O corte é o do `trancos` dos receptores
        /// (`Fluidez`): em 11/09 o iPhone X parou ~15 vezes por minuto sem perder pacote, e sem
        /// esta régua não havia como dizer se a parada nascia na captura, no envio ou do outro lado.
        /// A diferença de um segundo para o outro na linha `casca:` alinha com os trancos dele.
        /// Com a tela parada a repetição é de 500 ms — lacuna por construção, não defeito.
        public var lacunasDeCaptura = 0
        public var maiorLacunaDeCapturaMs = 0
        public var lacunasDeEnvio = 0
        public var maiorLacunaDeEnvioMs = 0
        /// Amostras de tela do ScreenCaptureKit **por `SCFrameStatus`** (`rawValue`: 0 completo,
        /// 1 ocioso, 2 em branco, 3 suspenso, 4 começou, 5 parou), antes do filtro. Só a tela.
        public var estadosDaCaptura: [Int: Int] = [:]

        // MARK: - áudio de sistema

        /// Blocos que o ScreenCaptureKit entregou. **Não** é o mesmo que quadros: um bloco do
        /// SCK não tem a duração de um quadro do codec, e é justamente por isso que existe um
        /// fatiador no meio. Ver `tratarAudioCapturado`.
        public var blocosDeAudio = 0
        /// Amostras por canal que saíram do conversor. Divida pela taxa do preset e você tem os
        /// segundos de som que de fato atravessaram — o número que responde "o áudio parou?".
        public var amostrasDeAudio = 0
        public var quadrosDeAudioEnviados = 0
        public var quadrosDeAudioRecusados = 0
        public var bytesDeAudioEnviados = 0
        /// Blocos que o `AVAudioConverter` não converteu. Diferente de zero aqui explica um
        /// silêncio que de outra forma não teria explicação nenhuma.
        public var falhasDeConversao = 0
        /// Quantas vezes o `AVAudioConverter` foi refeito porque o formato da origem mudou no
        /// meio da sessão — o caminho que uma troca de dispositivo de saída percorre. **Zero é
        /// uma resposta**, e significa que o ScreenCaptureKit entregou o mesmo formato do começo
        /// ao fim; não significa que nada mudou na máquina.
        public var reconstrucoesDoConversor = 0
        /// **O nível do que foi capturado**, em RMS sobre o fundo de escala de 16 bits (32 767).
        ///
        /// Existe porque contar blocos **não prova que houve som**: uma sessão em silêncio entrega
        /// exatamente o mesmo número de blocos que uma tocando música, e todos os outros
        /// contadores sobem igual. Sem este número, "981 blocos capturados" é compatível com
        /// "981 blocos de zeros", que é o resultado que um filtro de escopo errado produziria — e
        /// é o defeito que parece funcionar.
        ///
        /// É contador, não conteúdo: um número escalar não reconstrói som nenhum, então ele pode
        /// ser relatado mesmo no escopo de produto, onde o áudio é da máquina do usuário. É a
        /// mesma disciplina de medir por contador em vez de por pixel.
        public var nivelRmsDoAudio: Double = 0
        /// O maior valor absoluto visto, no mesmo fundo de escala.
        public var picoDoAudio = 0
        /// **A frequência dominante do que foi capturado**, em Hz, por taxa de cruzamentos por
        /// zero.
        ///
        /// # Este contador existe para pegar o defeito que `docs/audio.md` §2 diz não ter contador
        ///
        /// A frase de lá é literal: o relógio do codec "não é escolha… errar por um fator de 6 não
        /// dá erro — dá áudio que *acelera* ou *arrasta* sem contador nenhum acusando". É o defeito
        /// mais fácil de cometer nesta cadeia, porque há **duas** taxas em jogo (48 kHz na captura,
        /// 8 kHz no codec) e um `AVAudioConverter` no meio cuja única função é converter entre elas.
        ///
        /// Nenhum outro número aqui o pegaria. Se o conversor entregasse as amostras sem reamostrar,
        /// a contagem de blocos ficaria igual, as amostras ficariam igual, o RMS ficaria igual e o
        /// pico ficaria igual — e o som sairia **seis vezes mais grave**. Este contador muda.
        ///
        /// Vale porque a origem de bancada é um tom **de frequência conhecida**: 440 Hz medidos
        /// contra 440 Hz emitidos fecham a conta. Sobre conteúdo real ele não significa quase nada
        /// (a "frequência dominante" de música é uma pergunta mal posta), e por isso ele é
        /// diagnóstico de bancada e não métrica de produto.
        ///
        /// Continua sendo **escalar, não conteúdo**: um número não reconstrói som nenhum.
        public var frequenciaEstimadaHz: Double = 0

        // MARK: - a linha do som (`LinhaDoSomDoMac`, §19.6 do som-no-receptor.md)

        /// Reaberturas da `SCStream` que reancoraram o carimbo do som.
        public var reancoragensDoSom = 0
        /// Lacunas de conteúdo: saltos do PTS de 2 ms ou mais contra a contagem de amostras.
        public var lacunasDoSom = 0
        public var maiorLacunaDoSomMs = 0.0
        /// Degraus do carimbo (lacunas, reaberturas, socorros): o carimbo nunca volta.
        public var degrausDoSom = 0
        public var socorrosDoSom = 0
        /// O erro da última janela de 10 s, em ms: a deriva desde a âncora, pela entrega.
        public var erroDoSomMs = 0.0
        public var maiorErroDoSomMs = 0.0
        /// A disciplina da deriva: a frequência estimada, o ajuste da razão e o portão (N4).
        public var fDoSomPpm = 0.0
        public var ajusteDoSomPpm = 0.0
        public var portaoDoSomAberto = false
    }

    public enum Falha: Error, CustomStringConvertible {
        case fonteNaoIniciou(String)

        public var description: String {
            switch self {
            case .fonteNaoIniciou(let motivo): return motivo
            }
        }
    }

    private let fonte: FonteDeCaptura
    private let fps: Int32
    private let larguraPedida: Int?
    private let alturaPedida: Int?
    /// Aplicar o teto de `TetoDoEmissor` ao que vai para o encoder e para a captura.
    ///
    /// **Verdadeiro por padrão, e é isto que muda o produto.** O parâmetro existe — em vez de o
    /// teto ser incondicional — por uma razão de bancada e só por ela: sem um jeito de desligá-lo
    /// no mesmo binário, o "antes" e o "depois" teriam de ser medidos em builds diferentes, e
    /// este projeto já pagou caro por comparar duas corridas que diferiam em mais coisas do que
    /// quem as comparou achava (`docs/tela-preta.md` §8.2: "só o nível muda" era falso, e eram
    /// cinco variáveis). Nenhum caminho de produto passa `false` aqui.
    private let aplicarTeto: Bool
    /// O teto a aplicar **no lugar da cópia local**, quando quem constrói tem um melhor. Hoje só a
    /// tela estendida passa: o `Emissor` pergunta ao núcleo (nível 5.2, 60 fps pedidos), porque a
    /// cópia local reduziria 1920 × 1200 a ~1830 × 1144 a 30 fps — letra borrada e mouse aos saltos.
    /// `QuallCaptureKit` não linka o núcleo, de propósito (`Package.swift`); quem linka injeta.
    private let tetoInjetado: ((Int, Int, Int32) -> TetoDoEmissor.Aplicado)?
    /// O capturador de tela **tipado**, só para a tela estendida: o monitor dela nasce em
    /// `iniciar()`, depois do capturador, e é por aqui que ele é apontado.
    #if QUALL_TELA_ESTENDIDA_FUTURA
    private let capturadorDeTela: ScreenCapturer?
    #endif
    /// O monitor da tela estendida, enquanto existir. Só tocado sob `travaDoMonitor`.
    #if QUALL_TELA_ESTENDIDA_FUTURA
    private var monitorAuxiliar: MonitorVirtualAuxiliar?
    #endif
    /// `parar()` já começou. Sob `travaDoMonitor`: é o que impede um monitor que termina de subir
    /// **depois** da parada de ficar vivo sem dono.
    private var paradaPedida = false
    private let travaDoMonitor = NSLock()
    /// O monitor da tela estendida, depois de nascer — para a captura saber se ele continua lá quando
    /// o ScreenCaptureKit a derruba. Sob `travaDoMonitor`.
    #if QUALL_TELA_ESTENDIDA_FUTURA
    private var displayIDDaTelaEstendida: CGDirectDisplayID?
    /// Quando as últimas reaberturas aconteceram. Três em 30 s é desistir: aí não é mexida de
    /// monitor, é captura que não fica de pé. Sob `travaDoMonitor`.
    private var reaberturas: [Date] = []
    /// As reaberturas **isentas** (mexida de monitor nosso) também têm teto — 10 em 30 s —, senão um
    /// aparelho em laço de conecta-e-cai manteria a janela aberta e um monitor que pisca de verdade
    /// reabriria sem fim (revisão de 10/09/2026). E uma reabertura por vez: o `onStop` e o vigia
    /// podiam chamar `reabrir` juntos, e ele mexe no stream entre `await`s.
    private var reaberturasIsentas: [Date] = []
    private var reabrindo = false
    #endif
    /// **Tela parada não gera quadro**, e isso derruba o receptor. O ScreenCaptureKit só entrega
    /// quadro quando algo muda (`ScreenCapturer` descarta os `.idle`); o receptor Android desiste
    /// depois de 10 s sem quadro (`ReceptorSessao.kt`, `SILENCIO_ATE_DESISTIR_MS`); e um pedido de
    /// IDR só é atendido no próximo quadro capturado. Um monitor que acabou de nascer é só papel de
    /// parede — parado por definição. Achado pela revisão adversarial de 10/09, antes da bancada.
    ///
    /// Com isto, depois de `intervaloDeRepeticao` sem quadro novo o último é codificado de novo — um
    /// P que custa quase nada, ou o IDR pedido, na hora. **Só a tela estendida repete**: nas outras
    /// fontes o fps segue o conteúdo, e medidas publicadas contam com isso (`docs/bancada.md`, "tela
    /// parada não gera quadro"). Estender às outras telas é decisão à parte.
    private let intervaloDeRepeticao: TimeInterval?
    /// O GOP da tela estendida: **10 s**, em vez do 1 s das outras telas. Com a repetição mantendo
    /// quadro no fio e o IDR atendido quando o receptor pede (`pedirIDR`), o IDR periódico só serve
    /// de rede de segurança — e a 1920 × 1200 cada um passou de 580 pacotes no laço local.
    private let gopSegundos: Double?
    /// O GOP desta transmissão, para a linha `captura:` do registro.
    public var gopDescrito: String { gopSegundos.map { "\($0)s" } ?? "do preset" }
    /// Só tocados em `fila`.
    private var ultimosPixels: CVPixelBuffer?
    private var ultimoEnvioNs: UInt64 = 0
    /// Para `Contadores.lacunasDeCaptura` e `lacunasDeEnvio`. Só em `fila`.
    /// O `minimumFrameInterval` pedido à captura, para a linha `captura:` do registro — em 11/09 uma
    /// comparação ficou sem prova de que a opção tinha sido aplicada. Vazio fora da tela estendida.
    public private(set) var intervaloMinimoDescrito = ""
    /// O que o encoder respondeu ao teto de bytes por quadro. Ver `H264Encoder.respostaDoTetoDeQuadro`.
    public private(set) var respostaDoTetoDeQuadro = "não pedido"
    private var ultimaCapturaNs: UInt64 = 0
    private var ultimoAceitoNs: UInt64 = 0
    private var repetidor: DispatchSourceTimer?
    private let sumidouro: Sumidouro
    private let sumidouroDeAudio: SumidouroDeAudio?

    private let capturador: FrameSource
    private var encoder: H264Encoder?

    /// Nulos quando a transmissão é sem som. Os dois só são tocados dentro de `fila`, que é a
    /// mesma fila em que o ScreenCaptureKit entrega áudio e vídeo — nenhum dos dois precisa de
    /// trava própria por isso.
    private let conversorDeAudio: ConversorDeAudio?
    private let codificadorDeAudio: CodificadorDeAudio?
    /// **A linha do som** (`LinhaDoSomDoMac`, §19.6 do `som-no-receptor.md`): o carimbo do próximo
    /// quadro de áudio, ancorado no primeiro bloco capturado e andando de 20 em 20 ms exatos; um
    /// degrau em toda lacuna de conteúdo (um salto do PTS de 2 ms ou mais) e em toda reabertura da
    /// `SCStream`; e a disciplina da deriva, que reamostra o conteúdo quando a deriva medida pela
    /// entrega passa de 2 ppm. Antes de 19/09/2026 ele nunca reancorava, e cada reabertura deixava o
    /// som adiantado pelo tamanho da lacuna até o fim da sessão (crítica 3, grave 1b).
    ///
    /// # Por que não o carimbo do bloco que chegou
    ///
    /// Porque os blocos do ScreenCaptureKit não têm a duração de um quadro do codec, e o
    /// fatiador junta e reparte. Um quadro de 20 ms quase nunca começa onde um bloco começa, e
    /// carimbá-lo com o instante do bloco que o continha daria um carimbo errado por até um bloco
    /// inteiro.
    ///
    /// Pior: o carimbo de áudio vira o **carimbo RTP**, que para áudio tem de andar em passos
    /// exatos de `amostrasPorQuadro`. Repassar a irregularidade da captura para ele produziria um
    /// fluxo que o jitter buffer do outro lado leria como jitter da rede — culpando o rádio por
    /// uma conta que erramos aqui. Ancorar uma vez e andar em passo fixo é o que a natureza do
    /// áudio pede: o sumidouro do outro lado é um DAC, que consome exatamente 48 000 amostras por
    /// segundo, para sempre (`docs/audio.md` §4).
    private var linhaDoSom: LinhaDoSomDoMac?
    /// **Só bancada**: `QUALL_PROVA_LACUNA_NO_SOM_MS` joga fora os blocos de som por esse tanto, aos
    /// 10 s do primeiro — a lacuna de uma reabertura, sem reabrir nada nem mexer em monitor; e
    /// `QUALL_PROVA_SEM_REANCORAR=1` tira o degrau das lacunas (o controle, o comportamento de
    /// antes de 19/09; a reabertura continua reancorando). Os ganchos booleanos só ligam com "1":
    /// `=0` não liga. Sem as variáveis, nada disto existe.
    private let provaLacunaNoSomUs: UInt64? = ProcessInfo.processInfo.environment["QUALL_PROVA_LACUNA_NO_SOM_MS"]
        .flatMap { UInt64($0) }.map { $0 * 1000 }
    private let provaSemReancorar = ProcessInfo.processInfo.environment["QUALL_PROVA_SEM_REANCORAR"] == "1"
    /// **Só bancada** (o teste 12 do §19.6.6): `QUALL_PROVA_PPM_NO_SOM=300` faz o conteúdo andar
    /// 300 ppm mais depressa que o host — repete uma amostra a cada 1/ppm (tira, com ppm negativo),
    /// **depois** do detector de lacuna e antes do sinc. O PTS e a entrega não mudam, e a linha
    /// de saída anda diferente: o erro vê o gancho, e o laço tem de tirá-lo.
    /// `QUALL_PROVA_SEM_DISCIPLINA=1` desliga o laço (o controle). `QUALL_PROVA_PORTAO_ABERTO=1` abre
    /// o portão desde o começo. Só "1" liga.
    private let provaPpmNoSom: Double? = ProcessInfo.processInfo.environment["QUALL_PROVA_PPM_NO_SOM"].flatMap { Double($0) }
    private let provaSemDisciplina = ProcessInfo.processInfo.environment["QUALL_PROVA_SEM_DISCIPLINA"] == "1"
    private let provaPortaoAberto = ProcessInfo.processInfo.environment["QUALL_PROVA_PORTAO_ABERTO"] == "1"
    private var provaAcumuladoPpm = 0.0
    /// **Só bancada**: `QUALL_PROVA_SERIE_DO_SOM=<arquivo>` escreve uma linha por bloco de som — a
    /// hora do host agora, o PTS do bloco, as amostras dele (a 48 kHz), as pendentes do fatiador e
    /// o erro do relógio —, para medir o ruído da hora do SCK antes de desenhar o filtro da
    /// disciplina da deriva (`som-no-receptor.md` §19.6, G2 da crítica). Só horas e contagens:
    /// nenhuma amostra de som. Sem a variável, nada disto existe.
    private let provaSerieDoSom: FileHandle? = {
        guard let caminho = ProcessInfo.processInfo.environment["QUALL_PROVA_SERIE_DO_SOM"] else { return nil }
        FileManager.default.createFile(atPath: caminho, contents: Data("agora_us pts_us amostras pendentes erro_us\n".utf8))
        let h = FileHandle(forWritingAtPath: caminho)
        _ = try? h?.seekToEnd()
        return h
    }()
    private var provaSerieLinhas: [String] = []
    private var provaPrimeiroBlocoUs: UInt64?
    /// Soma dos quadrados e contagem, para o RMS. Guardados como soma corrente porque a
    /// alternativa — guardar as amostras — seria manter o som do usuário em memória.
    private var somaDosQuadradosDoAudio: Double = 0
    private var amostrasSomadasNoRms = 0
    /// Cruzamentos por zero e a última amostra vista, para a estimativa de frequência. A última
    /// amostra é guardada para que um cruzamento **na fronteira entre dois blocos** seja contado —
    /// sem isso, a estimativa erraria para menos em proporção ao número de blocos.
    private var cruzamentosPorZero = 0
    /// O lado em que o disparador de Schmitt está: `1` acima do limiar positivo, `-1` abaixo do
    /// negativo, `0` antes do primeiro sinal. Ver a nota sobre o viés em `frequenciaEstimadaHz`.
    private var ladoDoDisparador = 0

    /// Todo estado mutável só é tocado dentro desta fila. Os callbacks do ScreenCaptureKit e do
    /// VideoToolbox chegam de filas diferentes; sem um ponto único de serialização isto vira
    /// corrida de dados em cima de contadores que o SwiftUI lê.
    private let fila = DispatchQueue(label: "quall.transmissao")
    private var contadoresInternos = Contadores()
    private var somaLatenciaUs: Int64 = 0
    private var amostrasDeLatencia = 0
    private var submissoesPendentes: [Int64: UInt64] = [:]
    private let travaDeIDR = NSLock()
    private var idrPedido = false
    private var parado = false

    /// A dimensão que **sai na rede** — já com o teto aplicado.
    public private(set) var largura = 0
    public private(set) var altura = 0
    /// A dimensão que a plataforma entregou, **antes** do teto. Guardada separada porque
    /// "capturou 2560x1664" e "codificou 1280x832" são frases diferentes, e até esta rodada o
    /// registro só sabia imprimir uma delas — o que é precisamente como um emissor sem teto
    /// passou meses sem que ninguém o nomeasse.
    public private(set) var larguraDaCaptura = 0
    public private(set) var alturaDaCaptura = 0
    /// A taxa de quadros que de fato foi pedida à captura e ao encoder, depois do teto.
    public private(set) var fpsDeSaida: Int32 = 0
    /// Quem reduziu: a fonte de captura (`true`) ou o encoder (`false`). Sem teto a aplicar, é
    /// `false` e não quer dizer nada.
    public private(set) var reduzidoPelaFonte = false
    public var preset: CapturePreset { fonte.presetSugerido }
    public private(set) var nomeDoEncoder = ""
    public private(set) var encoderEhHardware = false
    /// O que o `RemendoDeSPS` fez com o SPS desta sessão, em uma linha.
    ///
    /// **Guardado em `parar()`, e não lido do encoder na hora.** Antes disto esta propriedade lia
    /// `encoder?.resumoDoSPS`, e quem a consulta é o registro de encerramento — que roda **depois**
    /// de `parar()` ter anulado o encoder. O resultado é que a linha `sps:` do app dizia "encoder
    /// ainda não criado" em toda corrida, inclusive nas que codificaram 577 quadros em hardware.
    /// Era a única prova, de dentro do processo, de que a restrição de bitstream foi declarada —
    /// e ela nunca apareceu.
    public var resumoDoSPS: String {
        fila.sync { resumoDoSPSGuardado ?? encoder?.resumoDoSPS ?? "encoder ainda não criado" }
    }
    private var resumoDoSPSGuardado: String?

    public var contadores: Contadores {
        fila.sync { contadoresInternos }
    }

    /// O preset de áudio desta transmissão, ou `nil` quando ela é sem som.
    public let presetDeAudio: PresetDeAudio?
    /// O escopo do conteúdo. Ver `EscopoDaCaptura` — em `.somenteEsteApp` nada da vida do usuário
    /// pode entrar na captura, e é o único escopo cujo artefato pode ser gravado ou ouvido.
    public let escopo: EscopoDaCaptura

    public init(
        fonte: FonteDeCaptura,
        fps: Int32 = 30,
        largura: Int? = nil,
        altura: Int? = nil,
        aplicarTeto: Bool = true,
        teto: ((Int, Int, Int32) -> TetoDoEmissor.Aplicado)? = nil,
        presetDeAudio: PresetDeAudio? = nil,
        escopo: EscopoDaCaptura = .aMaquinaInteira,
        janelaDeToleranciaDoVigia: Double = 0,
        nomeDoMonitor: String = "",
        indiceDoMonitor: Int = 0,
        sumidouro: @escaping Sumidouro,
        sumidouroDeAudio: SumidouroDeAudio? = nil
    ) {
        self.fonte = fonte
        self.nomeDoMonitor = nomeDoMonitor
        self.indiceDoMonitor = indiceDoMonitor
        self.fps = fps
        self.larguraPedida = largura
        self.alturaPedida = altura
        self.aplicarTeto = aplicarTeto
        self.tetoInjetado = teto
    #if QUALL_TELA_ESTENDIDA_FUTURA
        if case .telaEstendida = fonte.tipo {
            self.intervaloDeRepeticao = 0.5
            self.gopSegundos = TransmissaoAoVivo.gopDaTelaEstendida
        } else {
            self.intervaloDeRepeticao = nil
            self.gopSegundos = nil
        }
    #else
        self.intervaloDeRepeticao = nil
        self.gopSegundos = nil
    #endif
        self.sumidouro = sumidouro
        self.escopo = escopo
        self.janelaDeToleranciaDoVigia = janelaDeToleranciaDoVigia

        // Som só existe se as três coisas existirem: o preset, o sumidouro e uma fonte que possa
        // capturá-lo. Faltando qualquer uma, a transmissão é sem som e os contadores de áudio
        // ficam em zero — que é uma resposta honesta, e não um silêncio inexplicado.
        let querSom = presetDeAudio != nil && sumidouroDeAudio != nil && fonte.ehTela
        self.presetDeAudio = querSom ? presetDeAudio : nil
        self.sumidouroDeAudio = querSom ? sumidouroDeAudio : nil
        if querSom, let preset = presetDeAudio {
            let conv = ConversorDeAudio(preset: preset)
            self.conversorDeAudio = conv
            self.codificadorDeAudio = CodificadorPCMU(preset: preset)
            self.linhaDoSom = LinhaDoSomDoMac(taxa: preset.taxaDeAmostragem, canais: preset.canais,
                                              disciplina: !provaSemDisciplina,
                                              portaoAberto: provaPortaoAberto,
                                              degrauNasLacunas: !provaSemReancorar)
        } else {
            self.conversorDeAudio = nil
            self.codificadorDeAudio = nil
        }

        switch fonte.tipo {
        case .tela(let displayID):
            self.capturador = ScreenCapturer(displayID: displayID,
                                             capturarAudio: querSom,
                                             escopo: escopo)
            #if QUALL_TELA_ESTENDIDA_FUTURA
            self.capturadorDeTela = nil
            #endif
    #if QUALL_TELA_ESTENDIDA_FUTURA
        case .telaEstendida:
            // **`kCGNullDirectDisplay`, e não `nil`.** `nil` quer dizer "o primeiro monitor da
            // lista" no `ScreenCapturer` — a tela da pessoa. Se o monitor virtual não subir, a
            // captura tem de falhar com `noDisplay`, e não mandar para a rede a tela errada.
            let deTela = ScreenCapturer(displayID: kCGNullDirectDisplay,
                                        capturarAudio: querSom,
                                        escopo: escopo)
            self.capturador = deTela
            self.capturadorDeTela = deTela
            deTela.onEstado = { [weak self] estado in
                guard let self else { return }
                self.fila.async { self.contadoresInternos.estadosDaCaptura[estado, default: 0] += 1 }
            }
    #endif
        }

        capturador.onFrame = { [weak self] amostra in
            guard let self else { return }
            self.fila.async { self.tratarQuadroCapturado(amostra) }
        }
        capturador.onAudio = { [weak self] bloco in
            guard let self else { return }
            self.fila.async { self.tratarAudioCapturado(bloco) }
        }
        capturador.onStop = { [weak self] erro in
            guard let self, let erro else { return }
            // **Tela estendida: o monitor continua ali, então a captura é reaberta** — o
            // ScreenCaptureKit a derrubou numa reorganização de monitores (o iPad saindo, 10/09), e
            // não porque o monitor sumiu. Se ele não estiver mais na lista, segue o caminho de sempre.
    #if QUALL_TELA_ESTENDIDA_FUTURA
            if let id = self.travaDoMonitor.withLock({ self.displayIDDaTelaEstendida }),
               VigiaDeMonitor.telasAtivas().contains(id) {
                Task.detached { await self.reabrirCaptura(motivo: "SCStream parou: \(erro.localizedDescription)") }
                return
            }
    #endif
            // Com um vigia de monitor ligado, o `didStopWithError` do ScreenCaptureKit deixa de
            // ser a decisão e passa a ser **uma das testemunhas**: o vigia colhe a hora dele
            // junto das outras e derruba a sessão quando a janela fechar. Sem vigia (câmera), o
            // comportamento é o de antes — este erro é a única notícia que existe.
            if let vigia = self.vigia {
                vigia.registrarTestemunhaExterna(
                    nome: "SCStream.didStopWithError",
                    detalhe: erro.localizedDescription)
                return
            }
            self.aoPararSozinho?(erro)
        }
        // A remontagem do conversor é anotada **na hora**: ela é o rastro de que a troca de
        // dispositivo de áudio chegou até a nossa cadeia, e um contador lido no fim não diz
        // *quando* aconteceu nem de que formato para qual.
        conversorDeAudio?.aoRefazer = { [weak self] de, para in
            self?.aoRegistrarDiagnostico?("conversor de áudio REFEITO: origem mudou de [\(de)] para [\(para)]")
        }
    }

    /// Chamado quando a captura para **sozinha** — o monitor foi desconectado, a Continuity
    /// Camera saiu de perto, o usuário revogou a permissão em Ajustes com o app aberto. Sem isto
    /// a tela de espera ficaria dizendo "Espelhando" sobre uma cadeia morta, que é exatamente o
    /// sintoma que `docs/fluxo-de-uso.md` manda evitar.
    public var aoPararSozinho: ((Error) -> Void)?

    /// Cada linha de diagnóstico que a cadeia quer no registro **enquanto acontece**, e não no
    /// resumo do fim: o vigia de monitor e a remontagem do conversor de áudio.
    ///
    /// As duas coisas que passam por aqui têm a mesma natureza — são eventos raros, disparados
    /// por alguém mexendo num cabo, que não aparecem em contador nenhum se ninguém anotar a hora.
    public var aoRegistrarDiagnostico: ((String) -> Void)?

    /// O vigia da fonte, quando ela é um monitor. Câmera não tem: quem avisa que a Continuity
    /// Camera saiu de perto é a própria `AVCaptureSession`.
    public private(set) var vigia: VigiaDeMonitor?
    private let janelaDeToleranciaDoVigia: Double

    /// O nome e a identidade do monitor da tela estendida — um por receptor quando há mais de um
    /// (`MonitorVirtual.serie(para:indice:)`). Ignorados nas outras fontes.
    public let nomeDoMonitor: String
    public let indiceDoMonitor: Int

    // --- as reorganizações que nós mesmos causamos ---------------------------------------------
    //
    // Com vários receptores, cada monitor que nasce ou é solto reorganiza os monitores do Mac, e o
    // ScreenCaptureKit das **outras** sessões pode cair nessa hora (já caiu com o iPad saindo, 10/09).
    // Reabrir é o conserto, mas o limite de 3 reaberturas em 30 s existe para desistir de monitor que
    // não para quieto — e não para gastar o orçamento de uma sessão com os monitores das outras
    // (revisão adversarial de 10/09/2026). Então: o que acontece até 5 s depois de um monitor nosso
    // nascer ou sair reabre sem contar.
    #if QUALL_TELA_ESTENDIDA_FUTURA
    private static let travaDaReorganizacao = NSLock()
    private static var reorganizacaoNossaAte = Date.distantPast

    static func anotarReorganizacaoNossa(duracao: TimeInterval = 5) {
        travaDaReorganizacao.withLock { reorganizacaoNossaAte = Date().addingTimeInterval(duracao) }
    }

    static var reorganizacaoNossaRecente: Bool {
        travaDaReorganizacao.withLock { Date() < reorganizacaoNossaAte }
    }

    #endif
    /// Resolve a fonte, cria o encoder e começa a capturar. Devolve a resolução real.
    @discardableResult
    public func iniciar() async throws -> (largura: Int, altura: Int) {
        // --- a tela estendida: o monitor nasce agora -------------------------------------------
        //
        // Agora, e não no Espelhar: a captura só começa depois de alguém conectar (`Emissor`), e um
        // monitor criado na espera seria um lugar para janela cair sem ninguém olhando.
        var displayIDVigiado: CGDirectDisplayID?
        if case .tela(let id) = fonte.tipo { displayIDVigiado = id }
        let nativo: (width: Int, height: Int)
    #if QUALL_TELA_ESTENDIDA_FUTURA
        if let modo = fonte.modoDaTelaEstendida {
            Self.anotarReorganizacaoNossa()
            let monitor = try await MonitorVirtualAuxiliar.subir(modo: modo, nome: nomeDoMonitor,
                                                                 indice: indiceDoMonitor)
            Self.anotarReorganizacaoNossa()
            let adotado = travaDoMonitor.withLock { () -> Bool in
                guard !paradaPedida else { return false }
                monitorAuxiliar = monitor
                return true
            }
            guard adotado else {
                // `parar()` chegou enquanto o monitor subia: ninguém mais vai soltá-lo.
                let linha = await monitor.soltar()
                aoRegistrarDiagnostico?("tela estendida: a sessão acabou enquanto o monitor subia — \(linha)")
                throw Falha.fonteNaoIniciou("a sessão foi encerrada enquanto o monitor virtual subia")
            }
            aoRegistrarDiagnostico?(monitor.relato)
            travaDoMonitor.withLock { displayIDDaTelaEstendida = monitor.displayID }
            capturadorDeTela?.apontar(para: monitor.displayID)
            // Meio período, padrão desde 11/09 — ver `capturaComMeioPeriodo`. **Só quando o monitor
            // atualiza no fps pedido**: com o monitor a 60 Hz e a captura a 30 (`--tela-estendida-fps`),
            // meio período deixaria passar 60 quadros por segundo para um encoder de 30.
            let meioPeriodo = TransmissaoAoVivo.capturaComMeioPeriodo && modo.hertz <= Int(fps)
            capturadorDeTela?.intervaloMinimoDeMeioPeriodo = meioPeriodo
            intervaloMinimoDescrito = (meioPeriodo ? "1/\(fps * 2)" : "1/\(fps)") + " monitor=\(modo.hertz)Hz"
            displayIDVigiado = monitor.displayID
            nativo = try await descobrirMonitorNovo(modo: modo)
        } else {
            nativo = try await capturador.discoverTarget(width: larguraPedida, height: alturaPedida)
        }
    #else
        nativo = try await capturador.discoverTarget(width: larguraPedida, height: alturaPedida)
    #endif
        larguraDaCaptura = nativo.width
        alturaDaCaptura = nativo.height

        // --- o teto ---------------------------------------------------------------------------
        //
        // Aqui, e não dentro do `ScreenCapturer`, porque **a câmera também emite**: o
        // `CameraCapturer` ignora `width`/`height` de propósito (usa o formato ativo do
        // dispositivo), então um teto que morasse só na captura de tela deixaria metade das
        // origens sem limite nenhum. Este ponto é o único por onde as duas passam.
        let aplicado = tetoInjetado?(nativo.width, nativo.height, fps)
            ?? .daCopiaLocal(largura: nativo.width, altura: nativo.height, fps: fps)
        let teto = aplicado.saida
        let destino = aplicarTeto
            ? (largura: teto.largura, altura: teto.altura)
            : (largura: nativo.width, altura: nativo.height)
        let fpsDeSaida = aplicarTeto ? teto.fps : fps
        self.fpsDeSaida = fpsDeSaida

        // Quando a **fonte** sabe reduzir, é ela quem reduz. Não é otimização de gosto: o
        // ScreenCaptureKit compõe o quadro já no tamanho pedido, dentro do mesmo passe de GPU que
        // ele faria de qualquer jeito, enquanto deixar para o encoder obriga o VideoToolbox a
        // abrir uma sessão de transferência e reescalar um quadro grande que nunca precisou
        // existir. A câmera não sabe, e cai no caminho de baixo — que continua correto, só mais
        // caro.
        let precisaReduzir = destino.largura != nativo.width || destino.altura != nativo.height
        reduzidoPelaFonte = precisaReduzir
            && capturador.reduzirDestino(largura: destino.largura, altura: destino.altura)

        largura = destino.largura
        altura = destino.altura

        aoRegistrarDiagnostico?(
            TetoDoEmissor.relato(capturaLargura: nativo.width, capturaAltura: nativo.height,
                                 fpsPedido: fps, saida: teto, levelIdc: aplicado.levelIdc,
                                 maxFS: aplicado.maxFS)
            + " | teto do \(aplicado.origem)"
            + (precisaReduzir ? " | reduz em: \(reduzidoPelaFonte ? "captura" : "encoder")" : "")
            + (aplicarTeto ? "" : " | *** TETO DESLIGADO (corrida de bancada) ***"))

        // A taxa vem junto do teto **quando o teto é injetado**: é a mesma decisão (`quall.h`,
        // `teto_de_taxa_bps`). Sem injeção, o encoder pergunta à cópia local como sempre fez.
        #if QUALL_TELA_ESTENDIDA_FUTURA
        let tetoAutomaticoDeQuadro = TransmissaoAoVivo.tetoDeQuadroBytes == nil && fonte.modoDaTelaEstendida != nil
            ? TransmissaoAoVivo.tetoDeQuadroEmQuadrosMedios : 0
        #else
        let tetoAutomaticoDeQuadro = 0.0
        #endif
        let novo = try H264Encoder(width: Int32(destino.largura), height: Int32(destino.altura),
                                   fps: fpsDeSaida, preset: fonte.presetSugerido,
                                   taxaBps: tetoInjetado != nil && aplicarTeto ? teto.tetoDeTaxaBps : nil,
                                   gopSegundos: gopSegundos,
                                   tetoDeQuadroBytes: TransmissaoAoVivo.tetoDeQuadroBytes ?? 0,
                                   tetoDeQuadroEmQuadrosMedios:
                                       tetoAutomaticoDeQuadro)
        nomeDoEncoder = novo.encoderName
        respostaDoTetoDeQuadro = novo.respostaDoTetoDeQuadro
        encoderEhHardware = novo.backend == .hardware
        fila.sync { self.encoder = novo }

        // O encoder existe **antes** de o primeiro quadro poder chegar, de propósito: iniciar a
        // captura primeiro perderia os quadros da janela de corrida, e o primeiro quadro é
        // justamente o IDR que o receptor está esperando para montar a primeira imagem.
        try await capturador.beginCapture(fps: fpsDeSaida, sampleHandlerQueue: fila)

        // `parar()` pode ter chegado durante qualquer um dos `await` acima. Sem esta conferência a
        // captura ficaria ligada sem dono — indicador de gravação aceso e quadros para uma sessão que
        // já fechou —, e o vigia logo abaixo subiria vigiando um monitor que acabou de ser solto.
        if travaDoMonitor.withLock({ paradaPedida }) {
            await capturador.stop()
            throw Falha.fonteNaoIniciou("a sessão foi encerrada enquanto a captura subia")
        }

        if let intervalo = intervaloDeRepeticao {
            let relogio = DispatchSource.makeTimerSource(queue: fila)
            relogio.schedule(deadline: .now() + 0.1, repeating: 0.1)
            relogio.setEventHandler { [weak self] in self?.repetirSeParado(intervalo: intervalo) }
            fila.sync { self.repetidor = relogio }
            relogio.resume()
        }

        // O vigia sobe **depois** de a captura começar, e não antes: ligado antes, a ronda
        // registraria como "sumiço" um monitor que ainda nem tinha virado alvo.
        if let displayID = displayIDVigiado {
            #if QUALL_TELA_ESTENDIDA_FUTURA
            let ehEstendida = fonte.modoDaTelaEstendida != nil
            #else
            let ehEstendida = false
            #endif
            // Tela estendida: 1,5 s de janela e a segunda olhada. O monitor dela sai da lista por
            // instantes quando o macOS mexe nos monitores (ver `VigiaDeMonitor.revalidar`); um
            // monitor de verdade desplugado continua caindo na primeira testemunha.
            let novo = VigiaDeMonitor(monitorada: displayID,
                                      nomeDoMonitor: fonte.nome,
                                      janelaDeTolerancia: ehEstendida ? max(1.5, janelaDeToleranciaDoVigia)
                                                                      : janelaDeToleranciaDoVigia)
    #if QUALL_TELA_ESTENDIDA_FUTURA
            if ehEstendida {
                novo.revalidar = { VigiaDeMonitor.telasAtivas().contains(displayID) }
                novo.aoVoltar = { [weak self] in
                    guard let self else { return }
                    Task.detached { await self.reabrirCaptura(motivo: "o monitor voltou depois de uma mexida do macOS") }
                }
            }
    #endif
            novo.aoRegistrar = { [weak self] linha in self?.aoRegistrarDiagnostico?(linha) }
            novo.aoSumir = { [weak self] testemunhas in
                guard let self else { return }
                // Continua ligado e num conjunto espelhado: não sumiu, **virou espelho**. A decisão
                // é a mesma (parar), a frase não.
                if let origem = VigiaDeMonitor.espelhoDe(displayID) {
                    self.aoPararSozinho?(CaptureError.monitorVirouEspelho(nome: self.fonte.nome, espelhoDe: origem))
                    return
                }
                let resumo = testemunhas.isEmpty
                    ? "nenhuma"
                    : testemunhas.map { String(format: "%@ +%.1f ms", $0.nome, $0.msDesdeAPrimeira) }
                        .joined(separator: "; ")
                self.aoPararSozinho?(CaptureError.monitorSumiu(nome: self.fonte.nome, testemunhas: resumo))
            }
            novo.comecar()
            vigia = novo
        }

        return (largura: largura, altura: altura)
    }

    public func parar() async {
    #if QUALL_TELA_ESTENDIDA_FUTURA
        let monitor = travaDoMonitor.withLock { () -> MonitorVirtualAuxiliar? in
            paradaPedida = true
            defer { monitorAuxiliar = nil }
            return monitorAuxiliar
        }
    #else
        travaDoMonitor.withLock { paradaPedida = true }
    #endif
        vigia?.parar()
        vigia = nil
        await capturador.stop()
        fila.sync {
            parado = true
            repetidor?.cancel()
            repetidor = nil
            ultimosPixels = nil
            // O resumo é lido **antes** de o encoder morrer; ver `resumoDoSPS`.
            resumoDoSPSGuardado = encoder?.resumoDoSPS
            encoder?.finish()
            encoder = nil
            submissoesPendentes.removeAll()
        }
        // O monitor da tela estendida sai **por último**: com o vigia já parado e a captura já
        // desligada, o sumiço dele não vira testemunha de nada nem erro de stream.
    #if QUALL_TELA_ESTENDIDA_FUTURA
        if let monitor {
            Self.anotarReorganizacaoNossa()
            aoRegistrarDiagnostico?(await monitor.soltar())
            // E de novo depois: o auxiliar leva até 3 s para sair, e o sumiço do monitor é que mexe
            // nos outros.
            Self.anotarReorganizacaoNossa()
        }
    #endif
    }

    #if QUALL_TELA_ESTENDIDA_FUTURA
    /// Reabre a captura da tela estendida no mesmo monitor. Três vezes em 30 s é desistir: aí o
    /// motivo vira testemunha para o vigia, que encerra a sessão como sempre fez.
    private func reabrirCaptura(motivo: String) async {
        guard let capturadorDeTela else { return }
        let nossaAgora = Self.reorganizacaoNossaRecente
        var nossa = false
        let decisao = travaDoMonitor.withLock { () -> Bool? in
            if paradaPedida || reabrindo { return nil }
            let agora = Date()
            reaberturasIsentas = reaberturasIsentas.filter { agora.timeIntervalSince($0) < 30 }
            if nossaAgora && reaberturasIsentas.count < 10 {
                reaberturasIsentas.append(agora)
                nossa = true
                reabrindo = true
                return true
            }
            reaberturas = reaberturas.filter { agora.timeIntervalSince($0) < 30 }
            guard reaberturas.count < 3 else { return false }
            reaberturas.append(agora)
            reabrindo = true
            return true
        }
        guard let permitido = decisao else { return }
        defer { travaDoMonitor.withLock { reabrindo = false } }
        guard permitido else {
            aoRegistrarDiagnostico?("tela estendida: 3 reaberturas em 30 s — desisto (\(motivo))")
            vigia?.registrarTestemunhaExterna(nome: "SCStream.didStopWithError", detalhe: motivo)
            return
        }
        let t0 = Date()
        // A reabertura é uma descontinuidade do som: o próximo bloco reancora o carimbo, mesmo que
        // a lacuna tenha sido curta (§19.1). O pedido vai **antes** de reabrir: depois, o fluxo
        // novo podia entregar um bloco na `fila` antes dele, que virava lacuna, e o pedido vinha
        // em seguida (dois degraus; a revisão do código, leve 4). Um bloco velho que ainda chegue
        // depois do pedido reancora sem degrau, porque o PTS dele é contíguo.
        pedirReancoragemDoSom()
        do {
            try await capturadorDeTela.reabrir(fps: fpsDeSaida, sampleHandlerQueue: fila)
            pedirIDR()
            aoRegistrarDiagnostico?(String(format: "tela estendida: captura reaberta em %.0f ms (%@)%@",
                                           Date().timeIntervalSince(t0) * 1000, motivo,
                                           nossa ? " — mexida de monitor nosso, fora do limite" : ""))
        } catch {
            aoRegistrarDiagnostico?("tela estendida: a captura não reabriu (\(error)) — \(motivo)")
            vigia?.registrarTestemunhaExterna(nome: "SCStream.didStopWithError", detalhe: "\(motivo); reabrir falhou: \(error)")
        }
    }

    /// A reabertura pede ao relógio do som que reancore no próximo bloco, dentro de `fila`.
    private func pedirReancoragemDoSom() {
        fila.async { self.linhaDoSom?.pedirReancoragem() }
    }

    /// O `SCShareableContent` pode levar um instante para listar um monitor que acabou de nascer;
    /// tentar uma vez só transformaria esse instante em "nenhum display encontrado". Tenta por até
    /// 3 s, e para antes se a sessão acabar.
    ///
    /// Pede a captura **nos pixels do painel**, e não no tamanho que o `SCDisplay` reporta: a 2x ele
    /// reporta 960 × 600 **pontos**, e aceitar isso mandaria para a rede um quarto dos pixels. No 2x
    /// reduzido o painel é menor que o monitor, e o ScreenCaptureKit reduz na captura.
    private func descobrirMonitorNovo(modo: ModoDoMonitorVirtual) async throws -> (width: Int, height: Int) {
        let largura = larguraPedida ?? modo.larguraDaSaida
        let altura = alturaPedida ?? modo.alturaDaSaida
        let t0 = Date()
        while true {
            do {
                let alvo = try await capturador.discoverTarget(width: largura, height: altura)
                let ms = Int(Date().timeIntervalSince(t0) * 1000)
                if ms > 0 { aoRegistrarDiagnostico?("tela estendida: o ScreenCaptureKit listou o monitor em \(ms) ms") }
                return alvo
            } catch CaptureError.noDisplay {
                let parou = travaDoMonitor.withLock { paradaPedida }
                if parou || Date().timeIntervalSince(t0) >= 3 { throw CaptureError.noDisplay }
                try await Task.sleep(nanoseconds: 100_000_000)
            }
        }
    }

    #endif
    /// O receptor pediu um quadro-chave. Levanta a bandeira; o próximo quadro capturado sai IDR.
    public func pedirIDR() {
        travaDeIDR.lock()
        idrPedido = true
        travaDeIDR.unlock()
    }

    private func haPedidoDeIDR() -> Bool {
        travaDeIDR.lock()
        defer { travaDeIDR.unlock() }
        return idrPedido
    }

    private func consumirPedidoDeIDR() -> Bool {
        travaDeIDR.lock()
        defer { travaDeIDR.unlock() }
        guard idrPedido else { return false }
        idrPedido = false
        return true
    }

    // MARK: - o caminho do quadro (roda em `fila`)

    private func tratarQuadroCapturado(_ amostra: CMSampleBuffer) {
        guard !parado, let encoder, let pixels = CMSampleBufferGetImageBuffer(amostra) else { return }

        let pts = CMSampleBufferGetPresentationTimeStamp(amostra)
        let ptsUs = MonotonicClock.microseconds(from: pts)
        let agoraNs = MonotonicClock.nowNanoseconds()
        submissoesPendentes[ptsUs] = agoraNs
        contadoresInternos.quadrosCapturados += 1
        if ultimaCapturaNs != 0 {
            let ms = Int((agoraNs &- ultimaCapturaNs) / 1_000_000)
            if ms > 100 { contadoresInternos.lacunasDeCaptura += 1 }
            contadoresInternos.maiorLacunaDeCapturaMs = max(contadoresInternos.maiorLacunaDeCapturaMs, ms)
        }
        ultimaCapturaNs = agoraNs

        if intervaloDeRepeticao != nil {
            ultimosPixels = pixels
            ultimoEnvioNs = MonotonicClock.nowNanoseconds()
        }

        let forcar = consumirPedidoDeIDR()
        if forcar { contadoresInternos.idrsForcados += 1 }
        codificar(encoder: encoder, pixels: pixels, pts: pts, forcarIDR: forcar)
    }

    /// A tela está parada há `intervalo`, ou há um IDR esperando: codifica o último quadro de novo.
    /// Roda em `fila`, pelo relógio que `iniciar()` liga.
    private func repetirSeParado(intervalo: TimeInterval) {
        guard !parado, let encoder, let pixels = ultimosPixels else { return }
        let agora = MonotonicClock.nowNanoseconds()
        let parada = agora &- ultimoEnvioNs >= UInt64(intervalo * 1_000_000_000)
        guard parada || haPedidoDeIDR() else { return }
        ultimoEnvioNs = agora
        contadoresInternos.quadrosRepetidos += 1
        let forcar = consumirPedidoDeIDR()
        if forcar { contadoresInternos.idrsForcados += 1 }
        // Carimbo do relógio de host **agora**: é a mesma base dos carimbos do ScreenCaptureKit, e
        // um carimbo repetido faria o encoder e o RTP andarem para trás. Sem entrada em
        // `submissoesPendentes`: a latência de captura+encode é de quadro capturado.
        codificar(encoder: encoder, pixels: pixels, pts: CMClockGetTime(CMClockGetHostTimeClock()),
                  forcarIDR: forcar)
    }

    private func codificar(encoder: H264Encoder, pixels: CVPixelBuffer, pts: CMTime, forcarIDR forcar: Bool) {
        encoder.encode(
            pixelBuffer: pixels,
            presentationTimeStamp: pts,
            // `fpsDeSaida` e não `fps`: a duração declarada do quadro é o que o encoder usa para
            // fechar a conta de taxa, e declarar 60 num fluxo que sai a 30 pediria metade do
            // bitrate por quadro sem que ninguém percebesse.
            duration: CMTime(value: 1, timescale: fpsDeSaida),
            forcarIDR: forcar
        ) { [weak self] dados, ptsSaida, ehChave in
            guard let self else { return }
            let usSaida = MonotonicClock.microseconds(from: ptsSaida)
            // O sumidouro é chamado **fora** da fila, direto na thread do VideoToolbox: entrar na
            // fila aqui acrescentaria um salto de agendamento por quadro no caminho crítico, e o
            // envio já é `empacota e solta` (o núcleo não guarda o buffer). Só a contabilidade
            // volta para a fila.
            let aceito = self.sumidouro(dados, UInt64(max(0, usSaida)), ehChave)
            self.fila.async { self.contabilizar(bytes: dados.count, ptsUs: usSaida, idr: ehChave, aceito: aceito) }
        }
    }

    /// Captura → conversão → fatiamento → codec → rede. Roda em `fila`, a mesma do vídeo.
    ///
    /// O ScreenCaptureKit entrega blocos de duração própria (tipicamente ~10 ms), em Float32 a
    /// 48 kHz não intercalado. O codec quer quadros de **20 ms exatos**. Entre uma coisa e outra
    /// há duas peças, e nenhuma das duas é opcional: o `ConversorDeAudio`, que reamostra e
    /// rebaixa sem *aliasing*, e o fatiador dentro do codificador, que guarda o resto de um bloco
    /// para completar o quadro com o começo do próximo.
    private func tratarAudioCapturado(_ bloco: CMSampleBuffer) {
        guard !parado,
              let conversor = conversorDeAudio,
              let codificador = codificadorDeAudio,
              let preset = presetDeAudio,
              let sumidouroDeAudio else { return }

        let ptsUs = UInt64(max(0, MonotonicClock.microseconds(from: CMSampleBufferGetPresentationTimeStamp(bloco))))
        // A lacuna de bancada: os blocos desta janela não existem.
        if let lacuna = provaLacunaNoSomUs {
            if provaPrimeiroBlocoUs == nil { provaPrimeiroBlocoUs = ptsUs }
            let desde = ptsUs &- (provaPrimeiroBlocoUs ?? ptsUs)
            if desde >= 10_000_000 && desde < 10_000_000 + lacuna { return }
        }

        contadoresInternos.blocosDeAudio += 1
        let agoraUs = MonotonicClock.nowNanoseconds() / 1000
        let amostras48 = CMSampleBufferGetNumSamples(bloco)

        let antes = conversor.falhas
        var amostras = conversor.converter(bloco)
        contadoresInternos.falhasDeConversao += conversor.falhas - antes
        contadoresInternos.reconstrucoesDoConversor = conversor.reconstrucoes
        // Uma falha do conversor não entra na linha: o bloco seguinte chega com o PTS adiantado
        // deste, e a linha vê a lacuna. Antes, o bloco entrava vazio com o PTS contíguo, e o
        // carimbo ficava 20 ms atrás por falha, sem nada que corrigisse com o portão fechado (a
        // revisão do código, leve 3).
        if conversor.falhas > antes { return }
        if let ppm = provaPpmNoSom, !amostras.isEmpty {
            amostras = aplicarDerivaDeBancada(amostras, ppm: ppm, canais: preset.canais)
        }

        // **A linha do som**: a lacuna pelo PTS, a deriva pela entrega, o sinc (§19.6.4). O que sai
        // dela é o que o fatiador corta em quadros; num degrau, a sobra de antes é jogada fora.
        // A linha muda no lugar: uma cópia (`guard var`) duplicaria a cada bloco a entrada do sinc e
        // os pontos do portão, que o original ainda segura (a revisão do código, leve 7).
        guard linhaDoSom != nil else { return }
        let degrausAntes = linhaDoSom!.degraus
        let r = linhaDoSom!.bloco(ptsUs: ptsUs, amostras48: amostras48, agoraUs: agoraUs,
                                  convertidas: amostras, pendentesDoFatiador: codificador.pendentes)
        let linha = linhaDoSom!
        if r.descartarSobra { codificador.descartarSobra() }
        if let serie = provaSerieDoSom {
            provaSerieLinhas.append("\(agoraUs) \(ptsUs) \(amostras48) \(codificador.pendentes) \(Int64(linha.erroUs))")
            if provaSerieLinhas.count >= 200 {
                serie.write(Data((provaSerieLinhas.joined(separator: "\n") + "\n").utf8))
                provaSerieLinhas.removeAll(keepingCapacity: true)
            }
        }
        contadoresInternos.reancoragensDoSom = linha.reancoragens
        contadoresInternos.lacunasDoSom = linha.lacunas
        contadoresInternos.maiorLacunaDoSomMs = Double(linha.maiorLacunaUs) / 1000
        contadoresInternos.degrausDoSom = linha.degraus
        contadoresInternos.socorrosDoSom = linha.socorros
        contadoresInternos.erroDoSomMs = linha.erroUs / 1000
        contadoresInternos.maiorErroDoSomMs = linha.maiorErroUs / 1000
        contadoresInternos.fDoSomPpm = linha.fPpm
        contadoresInternos.ajusteDoSomPpm = linha.ajustePpm
        if linha.portao.aberto && !contadoresInternos.portaoDoSomAberto {
            aoRegistrarDiagnostico?(String(format: "som: o portão da disciplina abriu (deriva medida de %.1f ppm)",
                                           linha.portao.inclinacaoPpm))
        }
        contadoresInternos.portaoDoSomAberto = linha.portao.aberto
        if linha.degraus > degrausAntes {
            aoRegistrarDiagnostico?(String(format: "som: degrau no carimbo (%d lacuna(s), %d reabertura(s), %d socorro(s))",
                                           linha.lacunas, linha.reancoragens, linha.socorros))
        }
        guard !amostras.isEmpty else { return }
        contadoresInternos.amostrasDeAudio += amostras.count / max(1, preset.canais)

        // Nível e frequência do que chegou. Só somas correntes são guardadas — nunca as amostras.
        //
        // **Um disparador de Schmitt, e não uma troca de sinal com limiar.** A primeira versão
        // contava a troca de sinal entre duas amostras vizinhas e só se **as duas** estivessem
        // acima de um limiar de silêncio. Isso mede baixo, e mede baixo de um jeito que depende da
        // frequência: perto de um cruzamento as amostras de uma senoide são pequenas por
        // definição, então o próprio limiar descarta justamente os cruzamentos que deveria contar.
        // A fração descartada é ~2·limiar/(passo de fase), o que dá 23% a 440 Hz e 10% a 1 kHz —
        // e foi exatamente o que apareceu quando medi as duas.
        //
        // A histerese não tem esse viés: conta uma vez cada vez que o sinal **atravessa inteiro**
        // de um lado ao outro, o que dá dois por ciclo para qualquer senoide de amplitude acima do
        // limiar, independentemente de onde as amostras calharem de cair.
        let limiar: Int16 = 327 // 1% do fundo de escala
        for amostra in amostras {
            let v = Double(amostra)
            somaDosQuadradosDoAudio += v * v
            let absoluto = abs(Int(amostra))
            if absoluto > contadoresInternos.picoDoAudio { contadoresInternos.picoDoAudio = absoluto }

            if amostra > limiar, ladoDoDisparador != 1 {
                if ladoDoDisparador != 0 { cruzamentosPorZero += 1 }
                ladoDoDisparador = 1
            } else if amostra < -limiar, ladoDoDisparador != -1 {
                if ladoDoDisparador != 0 { cruzamentosPorZero += 1 }
                ladoDoDisparador = -1
            }
        }
        amostrasSomadasNoRms += amostras.count
        if amostrasSomadasNoRms > 0 {
            contadoresInternos.nivelRmsDoAudio =
                (somaDosQuadradosDoAudio / Double(amostrasSomadasNoRms)).squareRoot()
            // Dois cruzamentos por ciclo. A duração vem da contagem de amostras **por canal** e da
            // taxa do preset — que é justamente a taxa cuja correção se quer conferir, e por isso
            // um erro nela aparece aqui como frequência errada em vez de passar batido.
            let segundos = Double(amostrasSomadasNoRms / max(1, preset.canais))
                / preset.taxaDeAmostragem
            if segundos > 0 {
                contadoresInternos.frequenciaEstimadaHz =
                    Double(cruzamentosPorZero) / 2 / Double(max(1, preset.canais)) / segundos
            }
        }

        for quadro in codificador.codificar(r.saida) {
            let carimbo = linhaDoSom?.carimboDoProximoQuadro(duracaoUs: preset.duracaoDoQuadroUs) ?? 0
            // Como no vídeo: o envio é `empacota e solta`, o núcleo não guarda o buffer, e o
            // resultado é só contabilidade.
            if sumidouroDeAudio(quadro, carimbo) {
                contadoresInternos.quadrosDeAudioEnviados += 1
                contadoresInternos.bytesDeAudioEnviados += quadro.count
            } else {
                contadoresInternos.quadrosDeAudioRecusados += 1
            }
        }
    }

    /// O gancho de bancada `QUALL_PROVA_PPM_NO_SOM`: repete uma amostra (ppm > 0) ou tira uma
    /// (ppm < 0) a cada 1/|ppm| amostras, com o acumulado entre blocos. Só roda com a variável.
    private func aplicarDerivaDeBancada(_ amostras: [Int16], ppm: Double, canais: Int) -> [Int16] {
        let quadros = amostras.count / max(1, canais)
        var saida: [Int16] = []
        saida.reserveCapacity(amostras.count + canais * 2)
        for q in 0..<quadros {
            provaAcumuladoPpm += abs(ppm) * 1e-6
            let quadro = amostras[(q * canais)..<((q + 1) * canais)]
            if provaAcumuladoPpm >= 1 {
                provaAcumuladoPpm -= 1
                if ppm > 0 {
                    saida.append(contentsOf: quadro)
                    saida.append(contentsOf: quadro)
                }
                continue
            }
            saida.append(contentsOf: quadro)
        }
        return saida
    }

    private func contabilizar(bytes: Int, ptsUs: Int64, idr: Bool, aceito: Bool) {
        if let submissao = submissoesPendentes.removeValue(forKey: ptsUs) {
            let latencia = Int64((MonotonicClock.nowNanoseconds() - submissao) / 1000)
            somaLatenciaUs += latencia
            amostrasDeLatencia += 1
            contadoresInternos.latenciaMediaUs = Double(somaLatenciaUs) / Double(amostrasDeLatencia)
        }
        if aceito {
            contadoresInternos.quadrosEnviados += 1
            contadoresInternos.bytesEnviados += bytes
            if idr { contadoresInternos.idrsEnviados += 1 }
            let agoraNs = MonotonicClock.nowNanoseconds()
            if ultimoAceitoNs != 0 {
                let ms = Int((agoraNs &- ultimoAceitoNs) / 1_000_000)
                if ms > 100 { contadoresInternos.lacunasDeEnvio += 1 }
                contadoresInternos.maiorLacunaDeEnvioMs = max(contadoresInternos.maiorLacunaDeEnvioMs, ms)
            }
            ultimoAceitoNs = agoraNs
        } else {
            contadoresInternos.quadrosRecusados += 1
        }
    }
}
