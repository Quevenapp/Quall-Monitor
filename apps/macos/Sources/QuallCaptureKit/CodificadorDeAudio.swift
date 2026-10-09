import Foundation

/// O que transforma PCM em quadros codificados prontos para `enviar_audio`.
///
/// **Um quadro por chamada, um pacote por quadro.** `docs/audio.md` §6 é explícito: o
/// `AudioRtpPacketizer` da libdatachannel **não fragmenta**, então dois quadros concatenados numa
/// chamada viram um pacote que o outro lado decodifica errado — e sem erro em lugar nenhum, porque
/// para o RTP é só um payload maior. Por isso a interface devolve `[Data]`, uma entrada por
/// quadro, e não um `Data` só.
public protocol CodificadorDeAudio: AnyObject {
    var preset: PresetDeAudio { get }
    /// Nome do codec como ele aparece no `a=rtpmap`, para o registro.
    var nome: String { get }
    /// Consome amostras **intercaladas** (`canais` valores por instante) e devolve os quadros
    /// completos que couberam. O resto fica guardado para a próxima chamada.
    func codificar(_ amostras: [Int16]) -> [Data]
    /// Quantas amostras **por canal** estão guardadas esperando completar um quadro. Numa lacuna, a
    /// linha do som (`LinhaDoSomDoMac`) soma esse tanto ao degrau, porque a sobra sai com
    /// `descartarSobra()` e o som de depois tem de ficar na hora dele.
    var pendentes: Int { get }
    /// Joga fora o que está guardado: numa reancoragem, a sobra é de antes da lacuna.
    func descartarSobra()
}

/// G.711 µ-law (RFC 3551), o **piso** de `docs/audio.md` §2.
///
/// # Por que este codec existe nesta casca, e por quanto tempo
///
/// Não por preferência: 8 kHz mono é um codec de telefone e áudio de sistema é música. Ele está
/// aqui porque, quando foi escrito, **não havia encoder de Opus alcançável do Swift** (desde então
/// a fronteira exporta `quall_audio_encoder_new`, e o microfone da câmera, R5 fase 4, já sai em Opus
/// por ele — `EncoderDeAudioDoNucleo`; o som de sistema continua aqui, que é de outra frente). Conferido no grafo de dependências:
/// só `quall-probe` depende de `quall-opus`, então `libquall.a` — o arquivo que este app linka
/// inteiro por caminho — não carrega símbolo nenhum da libopus. E o macOS não tem Opus no sistema:
/// o AudioToolbox tem AAC em hardware, não tem Opus.
///
/// A saída **não** é compilar uma libopus por conta própria aqui. Um encoder configurado na casca
/// faz o preset do fio e o preset do encoder virarem duas fontes de verdade, que é literalmente o
/// defeito medido em `docs/audio.md` §11 — `useinbandfec=1` no SDP com zero LBRR no byte. A saída
/// é a fronteira C exportar o encoder já configurado pela tabela do núcleo, e até lá o caminho
/// inteiro se prova com este aqui, que é o uso que a §2 reservou ao PCMU **textualmente**.
///
/// Trocar de codec é trocar esta classe. Nada mais no caminho sabe qual é.
public final class CodificadorPCMU: CodificadorDeAudio {
    public let preset: PresetDeAudio
    public let nome = "pcmu"

    /// O que sobrou de amostras que não completaram um quadro. G.711 **não tem estado** — cada
    /// amostra vira um byte independente — então isto é só o resto da divisão, e não memória de
    /// codec. (Em Opus não seria: ver `docs/audio.md` §12, "o Opus tem estado".)
    private var sobra: [Int16] = []

    public init(preset: PresetDeAudio = .audioDoSistema(codec: .pcmu)) {
        precondition(preset.codec == .pcmu, "CodificadorPCMU com preset de outro codec")
        self.preset = preset
    }

    public var pendentes: Int { sobra.count / max(1, preset.canais) }

    public func descartarSobra() {
        sobra.removeAll(keepingCapacity: true)
    }

    public func codificar(_ amostras: [Int16]) -> [Data] {
        sobra.append(contentsOf: amostras)
        // `amostrasPorQuadro` é por canal; o buffer é intercalado.
        let porQuadro = preset.amostrasPorQuadro * preset.canais
        guard porQuadro > 0, sobra.count >= porQuadro else { return [] }

        var quadros: [Data] = []
        var lidas = 0
        while sobra.count - lidas >= porQuadro {
            var bytes = Data(count: porQuadro)
            bytes.withUnsafeMutableBytes { destino in
                let saida = destino.bindMemory(to: UInt8.self)
                for i in 0..<porQuadro {
                    saida[i] = CodificadorPCMU.paraMuLaw(sobra[lidas + i])
                }
            }
            quadros.append(bytes)
            lidas += porQuadro
        }
        sobra.removeFirst(lidas)
        return quadros
    }

    // MARK: - a tabela

    /// Os limites superiores dos oito segmentos da curva µ-law, no domínio de 13 bits.
    private static let fimDoSegmento: [Int32] = [0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF]

    /// Um valor linear de 16 bits vira um byte µ-law (ITU-T G.711).
    ///
    /// **Conferido exaustivamente contra o AudioToolbox**, nos 65 536 valores possíveis de
    /// `Int16` — ver `TestesDoCodificadorDeAudio`. Escrever a curva à mão e depois compará-la com
    /// uma implementação que não é nossa é o corolário do M4 aplicado a um codec: um µ-law errado
    /// **não dá erro em lugar nenhum** — os contadores sobem, o pacote atravessa, o som sai
    /// distorcido, e nenhuma medição de vazão denuncia.
    ///
    /// # A comparação achou duas coisas, e só uma delas era defeito nosso
    ///
    /// **O defeito:** a primeira versão negava o valor **depois** de deslocá-lo. Como o
    /// deslocamento aritmético de um negativo arredonda para baixo, a curva negativa ficava
    /// deslocada meio passo, errando por um código em 126 fronteiras de segmento. O conserto é
    /// dobrar o negativo com `-x - 1` **antes** do deslocamento, que é o complemento de dois
    /// espelhado em torno de −½ em vez de 0. É o mesmo tropeço que o spandsp marca no código-fonte
    /// com um aviso de "isto já esteve no lugar errado".
    ///
    /// **O que não é defeito nosso:** o AudioToolbox **nunca emite o código 0x00** — 255 códigos
    /// distintos contra os nossos 256 —, e por isso satura os 1 157 valores mais negativos em
    /// 0x02. É a supressão do octeto todo-zeros das linhas T1, onde ela existe para manter
    /// densidade de pulsos no fio. **Aqui não há linha T1**: isto é RTP sobre SRTP, todo
    /// decodificador de µ-law aceita os 256 códigos, e a RFC 3551 não reserva nenhum.
    ///
    /// Medido, e é o número que decidiu: passando os dois pelo **decodificador do próprio
    /// AudioToolbox**, a nossa curva erra menos — erro quadrático médio **226,5 contra 360,7**, e
    /// pior caso **644 contra 2 692**. Copiar a supressão nos deixaria byte a byte iguais à Apple
    /// e mensuravelmente piores. Então não copiamos, e o teste afirma a diferença em vez de
    /// escondê-la.
    public static func paraMuLaw(_ amostra: Int16) -> UInt8 {
        // Em `Int32` do começo ao fim: `-Int16.min` estoura, e o domínio da curva com o viés
        // somado também não cabe em `Int16` no caso extremo.
        var valor = Int32(amostra)
        let mascara: Int32
        if valor < 0 {
            // `-x - 1`, e **antes** do deslocamento. Ver o comentário acima: com `-x` depois do
            // deslocamento, a curva negativa erra por um código em 126 fronteiras.
            valor = -valor - 1
            mascara = 0x7F // negativos zeram o bit de sinal depois do XOR
        } else {
            mascara = 0xFF
        }
        valor >>= 2 // 16 bits -> o domínio de 14 bits da curva
        if valor > 8159 { valor = 8159 } // saturação da curva
        valor += 33 // o viés (0x84 >> 2), que é o que torna a curva contínua em zero

        var segmento = 0
        while segmento < 8 && valor > CodificadorPCMU.fimDoSegmento[segmento] { segmento += 1 }
        if segmento >= 8 { return UInt8(0x7F ^ mascara) }

        let mantissa = (valor >> Int32(segmento + 1)) & 0x0F
        let bruto = Int32(segmento << 4) | mantissa
        return UInt8(bruto ^ mascara)
    }
}
