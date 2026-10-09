import Foundation

/// O dano do enlace numa **janela**, e não desde o começo da sessão.
///
/// Todo contador de recepção deste projeto é acumulado desde o início — `packets_seen`,
/// `packets_lost_for_real`, `suspeitos`, `idrs_broken`. Isso é certo para o relato final e é
/// inútil para decidir alguma coisa **agora**: uma sessão que perdeu 8 % nos primeiros dez
/// segundos e nada depois continua dizendo 8 % meia hora adiante. Quem escuta o enlace precisa da
/// derivada, não da integral.
///
/// ## Por que esta peça existe no macOS, e o que a falta dela custava
///
/// O controlador de taxa do emissor (`crates/quall-core/src/taxa.rs`) só age sobre amostra que o
/// **receptor** manda de volta. Sem esta peça, uma casca receptora deixa o controlador
/// **inerte por construção**: o emissor nunca recebe janela e nunca mexe no bitrate.
///
/// Foi medido, e não deduzido. Em 31/08/2026, no par que o usuário usa — A10s espelhando para o
/// iPad —, o controlador estava ligado por padrão e fez **nada**: `trocas_de_bitrate=0` com
/// 2,95 % de perda e 881 quadros exibidos com a referência quebrada. O relato existia só na casca
/// Android. Portado para o iOS no mesmo dia, o **mesmo par** passou a `trocas_de_bitrate=9`,
/// `suspeitos` de 881 para 232 e perda de 2,95 % para 1,29 %. O macOS era a casca seguinte na
/// mesma lista.
///
/// É a mesma peça do iOS (`apps/ios/Quall/Receber/JanelaDoEnlace.swift`) e do Android
/// (`JanelaDoEnlace.kt`), com a mesma forma e os mesmos nomes, de propósito: um controlador
/// alimentado por um número diferente do que a bancada mediu é um controlador projetado contra
/// outra curva.
///
/// ## Por que ela mora em `QuallReceptorKit`, e não no app
///
/// Porque este alvo **tem testes** (`Tests/QuallReceptorKitTests`) e não depende de `CQuall`. A
/// peça é aritmética pura sobre números que o chamador já leu: ela nunca toca a fronteira C, e é
/// por isso que a âncora, a derivada, o denominador e a saturação podem ser provados sem rede,
/// sem `libquall.a` e sem aparelho. Quem fala com a fronteira é `NucleoReceptor.relatarEnlace`,
/// e quem decide *quando* é o laço de `Receptor.exibir`.
///
/// ## O denominador vem do emissor
///
/// `pacotes` é `vistos + perdidos`, que é **o que o emissor mandou** na janela. Dividir a perda
/// pelo que chegou responde outra pergunta, e o viés não é constante: numa medição desta bancada
/// ele inverteu a ordem entre dois braços, e o laudo já estava escrito. `ResumoDePerda`, no
/// arquivo ao lado, usa o mesmo denominador pelo mesmo motivo.
///
/// ## Contador que anda para trás é track recriada, não perda negativa
///
/// Os deltas são calculados com subtração **saturante**: regressão vira zero, nunca um número
/// gigante nem um negativo. Um delta negativo alimentando um controlador é como se sobe o bitrate
/// exatamente quando não se deve, e um `UInt64` que passa por baixo do zero publica dezoito
/// quintilhões — esta bancada já teve um contador imprimindo 296 pacotes numa origem cujo maior
/// quadro tem 115.
public struct JanelaDoEnlace {

    /// Uma janela fechada. Todos os campos são **deltas da janela**, exceto [`ms`].
    public struct Amostra: Equatable {
        /// Duração **real** da janela, em ms. Nunca a nominal: quem fecha é um laço que acorda
        /// quando acorda, e dividir pelo período nominal daria uma taxa sistematicamente alta.
        public let ms: UInt64
        /// O que o emissor mandou na janela: `vistos + perdidos`.
        public let pacotes: UInt64
        /// Perda **exata** (`packets_lost_for_real`), não o teto `packets_missing_upper_bound`.
        public let perdidos: UInt64
        /// Quadros que foram para a tela com a cadeia de referência condenada. É contador da
        /// própria casca, e não do núcleo.
        public let suspeitos: UInt64
        /// `idrs_broken` de `quall_track_stats_json`: o quadro de recuperação que chegou partido.
        public let idrsQuebrados: UInt64

        /// Perda da janela em por cento, com o denominador do emissor. Ver a doc do tipo.
        public var perdaPct: Double {
            pacotes == 0 ? 0.0 : Double(perdidos) * 100.0 / Double(pacotes)
        }

        /// A linha de diário, no **mesmo** formato do receptor iOS: as duas cascas põem a mesma
        /// janela no mesmo texto, para que duas corridas possam ser lidas lado a lado.
        public var linha: String {
            String(format: "janela_do_enlace ms=%llu pacotes=%llu perdidos=%llu (%.2f%%) "
                   + "suspeitos=%llu idrs_quebrados=%llu",
                   ms, pacotes, perdidos, perdaPct, suspeitos, idrsQuebrados)
        }
    }

    private var abertaEmUs: UInt64 = 0
    private var vistos: UInt64 = 0
    private var perdidos: UInt64 = 0
    private var suspeitos: UInt64 = 0
    private var idrsQuebrados: UInt64 = 0
    private var primeira = true

    public init() {}

    /// Fecha a janela se ela já durou `periodoMs`, e devolve a **derivada**.
    ///
    /// A primeira chamada só ancora e devolve `nil`: os acumulados de uma sessão que já rodou
    /// meio segundo antes de a primeira janela abrir não são dano desta janela — mediriam o
    /// arranque (o primeiro IDR, a subida do ICE) como se fosse regime.
    ///
    /// Enquanto a janela não completou `periodoMs`, devolve `nil` **sem mexer na âncora**: o
    /// laço chama a cada ~100 ms e é esta função que decide o que é uma janela.
    public mutating func fechar(agoraUs: UInt64, periodoMs: UInt64,
                                vistosAcum: UInt64, perdidosAcum: UInt64,
                                suspeitosAcum: UInt64, idrsQuebradosAcum: UInt64) -> Amostra? {
        if primeira {
            primeira = false
            ancorar(agoraUs, vistosAcum, perdidosAcum, suspeitosAcum, idrsQuebradosAcum)
            return nil
        }
        let decorridoMs = Medidas.delta(agoraUs, abertaEmUs) / 1000
        guard periodoMs > 0, decorridoMs >= periodoMs else { return nil }

        let dv = JanelaDoEnlace.derivada(vistosAcum, vistos)
        let dp = JanelaDoEnlace.derivada(perdidosAcum, perdidos)
        let ds = JanelaDoEnlace.derivada(suspeitosAcum, suspeitos)
        let di = JanelaDoEnlace.derivada(idrsQuebradosAcum, idrsQuebrados)
        ancorar(agoraUs, vistosAcum, perdidosAcum, suspeitosAcum, idrsQuebradosAcum)

        return Amostra(ms: decorridoMs, pacotes: dv &+ dp, perdidos: dp,
                       suspeitos: ds, idrsQuebrados: di)
    }

    /// `agora - antes` **saturante**. Ver a doc do tipo: regressão é track recriada, e o zero é a
    /// única resposta que não mente para o controlador do outro lado.
    private static func derivada(_ agora: UInt64, _ antes: UInt64) -> UInt64 {
        agora > antes ? agora - antes : 0
    }

    private mutating func ancorar(_ agoraUs: UInt64, _ v: UInt64, _ p: UInt64,
                                  _ s: UInt64, _ i: UInt64) {
        abertaEmUs = agoraUs
        vistos = v
        perdidos = p
        suspeitos = s
        idrsQuebrados = i
    }

    /// Os três acumulados do núcleo que a janela precisa, do JSON de `quall_track_stats_json`.
    ///
    /// `packets_lost_for_real` e **não** `packets_missing_upper_bound`: o segundo cobra
    /// reordenação como perda, e foi lido como perda em todas as medições desta bancada até
    /// 29/08/2026 — de 1,3× a 44× de inflação (`docs/contador-nas-cascas.md`). Um controlador
    /// alimentado por ele reduziria o bitrate por causa de pacotes que chegaram.
    ///
    /// # Leitura falha devolve `nil`, e a janela **não** é fechada
    ///
    /// A primeira versão desta função devolvia zeros, nas três cascas Swift. Parecia inofensivo:
    /// o delta de um acumulado que não se moveu é zero, e uma janela de `pacotes=0` é descartada
    /// pelo controlador, que exige `pacotes_minimos`. O dano não está nessa janela — está na
    /// **seguinte**. Zerar aqui zera a âncora, e a janela seguinte entrega como dano de 500 ms
    /// tudo o que a sessão acumulou desde o começo. O controlador soma `pacotes` e `perdidos` ao
    /// longo de um trecho (`ControladorDeTaxa::pacotes_desde_a_mudanca`), e uma janela dessas
    /// domina o trecho inteiro: é a integral entrando pela porta que existe para entregar a
    /// derivada.
    ///
    /// `nil` custa **um tique**. A âncora fica de pé, a janela seguinte mede o intervalo maior e
    /// o `ms` real diz isso em voz alta. É a política que a casca do OBS já tinha
    /// (`ler_acumulados_do_enlace`), e agora as quatro concordam.
    public static func acumulados(_ json: String) -> (vistos: UInt64, perdidos: UInt64,
                                                      idrsQuebrados: UInt64)? {
        guard let dados = json.data(using: .utf8),
              let d = try? JSONSerialization.jsonObject(with: dados) as? [String: Any]
        else { return nil }
        // Contador de núcleo não é negativo. Se vier assim é lixo, e zero é o único valor que não
        // inventa dano nem o esconde debaixo de um `UInt64` gigante.
        func inteiro(_ chave: String) -> UInt64? {
            guard let n = d[chave] as? NSNumber else { return nil }
            let v = n.int64Value
            return v > 0 ? UInt64(v) : 0
        }
        guard let vistos = inteiro("packets_seen"),
              let perdidos = inteiro("packets_lost_for_real"),
              let idrs = inteiro("idrs_broken")
        else { return nil }
        return (vistos, perdidos, idrs)
    }
}
