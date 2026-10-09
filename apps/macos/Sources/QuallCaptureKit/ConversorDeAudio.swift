import AVFoundation
import CoreMedia
import Foundation

/// Leva o PCM que o ScreenCaptureKit entrega até o formato que o codec pede.
///
/// O ScreenCaptureKit entrega **Float32 a 48 kHz, estéreo, não intercalado**. O PCMU quer
/// **Int16 a 8 kHz, mono, intercalado**. São três conversões ao mesmo tempo — taxa, profundidade
/// e disposição — e a do meio é a que não se improvisa: reamostrar 48 kHz para 8 kHz jogando
/// fora cinco de cada seis amostras produz *aliasing*, que soa como um chiado metálico que ninguém
/// consegue atribuir à causa depois. Quem faz isso aqui é o `AVAudioConverter`, que já tem o
/// filtro anti-*aliasing* que eu teria de escrever à mão.
///
/// **Nada aqui grava em disco.** Ver a nota de `TransmissaoAoVivo`: a origem desta cadeia pode ser
/// o som da máquina de trabalho de uma pessoa, e a forma mais barata de nunca vazar isso é não ter
/// para onde gravar.
final class ConversorDeAudio {
    /// O formato de destino, montado a partir do preset — nunca escrito à mão em dois lugares.
    private let destino: AVAudioFormat
    private var origem: AVAudioFormat?
    private var conversor: AVAudioConverter?

    /// Quantas vezes a conversão falhou. Um número diferente de zero aqui explica um silêncio que
    /// de outra forma não teria explicação nenhuma.
    private(set) var falhas = 0

    /// Quantas vezes o `AVAudioConverter` foi **refeito** porque o formato da origem mudou.
    ///
    /// Existe porque este caminho foi escrito por raciocínio e nunca tinha sido executado. Sem um
    /// contador, "o formato mudou no meio da sessão e o conversor se refez" e "o formato nunca
    /// mudou" produzem exatamente o mesmo registro — e é o segundo caso que a bancada precisa
    /// conseguir afirmar, porque ele significa que a linha nunca foi provada.
    ///
    /// A primeira montagem **não** conta: ela é a construção normal, não uma remontagem.
    private(set) var reconstrucoes = 0

    /// Chamado a cada remontagem, com o formato de antes e o de agora. É a única testemunha de
    /// que a troca de dispositivo de áudio chegou até aqui.
    var aoRefazer: ((_ de: String, _ para: String) -> Void)?

    /// O formato que o ScreenCaptureKit está entregando agora, em uma linha.
    var formatoDaOrigem: String { origem.map(ConversorDeAudio.descrever) ?? "(nenhum bloco ainda)" }

    /// Uma linha legível de um formato de áudio. O `settings` do `AVAudioFormat` é um dicionário
    /// grande e ilegível num registro; o que importa para diagnosticar um silêncio são estes
    /// quatro campos.
    static func descrever(_ f: AVAudioFormat) -> String {
        String(format: "%.0f Hz, %u canal(is), %@, %@",
               f.sampleRate,
               f.channelCount,
               f.commonFormat == .pcmFormatFloat32 ? "float32"
                   : (f.commonFormat == .pcmFormatInt16 ? "int16" : "outro(\(f.commonFormat.rawValue))"),
               f.isInterleaved ? "intercalado" : "não intercalado")
    }

    init?(preset: PresetDeAudio) {
        guard let formato = AVAudioFormat(
            commonFormat: .pcmFormatInt16,
            sampleRate: preset.taxaDeAmostragem,
            channels: AVAudioChannelCount(preset.canais),
            interleaved: true
        ) else { return nil }
        self.destino = formato
    }

    /// Converte um bloco do ScreenCaptureKit em amostras intercaladas de 16 bits.
    ///
    /// Devolve vazio — e conta uma falha — em vez de lançar: isto roda na fila de amostras da
    /// captura, e derrubar a transmissão inteira porque um bloco de 10 ms não converteu seria
    /// trocar um estalo por uma queda.
    func converter(_ amostra: CMSampleBuffer) -> [Int16] {
        guard let entrada = ConversorDeAudio.paraBuffer(amostra) else {
            falhas += 1
            return []
        }
        return converter(entrada)
    }

    /// A conversão de fato, já com o PCM desempacotado.
    ///
    /// **Separada da versão que recebe `CMSampleBuffer` para poder ser testada.** É aqui que mora
    /// a remontagem do `AVAudioConverter` quando o formato da origem muda no meio da sessão — o
    /// caminho que foi escrito por raciocínio, nunca tinha sido executado, e cujo modo de falhar é
    /// silêncio sem erro. Fabricar um `CMSampleBuffer` de áudio num teste é possível e frágil;
    /// fabricar dois `AVAudioPCMBuffer` de formatos diferentes é trivial e exercita a mesma linha.
    /// O que fica de fora é só o desempacotamento, que toda corrida real percorre.
    func converter(_ entrada: AVAudioPCMBuffer) -> [Int16] {
        // O formato da origem só é conhecido no primeiro bloco, e pode mudar no meio da sessão
        // se o dispositivo de saída padrão mudar (fone plugado, adaptador HDMI, a TV que some).
        // Refazer o conversor é mais barato do que descobrir isso como som errado.
        //
        // **A comparação é por campo, e não pelo `settings`.** O `settings` de um `AVAudioFormat`
        // é um dicionário com chaves que não são todas de valor — comparar dois deles como
        // `NSDictionary` responde "mudou" para formatos equivalentes e "não mudou" para formatos
        // que diferem só na disposição, e nos dois casos o erro é silencioso: no primeiro o
        // conversor é refeito à toa, no segundo ele continua convertendo do formato errado, que é
        // o defeito que soa como som acelerado sem contador nenhum acusando.
        let mudou = origem.map { !ConversorDeAudio.mesmoFormato($0, entrada.format) } ?? true
        if mudou {
            let anterior = origem
            let novo = AVAudioConverter(from: entrada.format, to: destino)
            novo?.sampleRateConverterQuality = AVAudioQuality.high.rawValue
            // **Só adota o formato novo se o conversor de fato nasceu.** Adotá-lo antes deixava
            // um buraco: com `AVAudioConverter` devolvendo `nil`, `origem` já apontava para o
            // formato novo, a condição de remontagem passava a ser falsa para sempre, e todos os
            // blocos seguintes caíam na falha sem nunca mais tentar. Silêncio permanente por uma
            // recusa momentânea do sistema — exatamente durante uma troca de dispositivo, que é
            // quando ela é mais provável.
            guard let novo else {
                falhas += 1
                return []
            }
            conversor = novo
            origem = entrada.format
            if let anterior {
                reconstrucoes += 1
                aoRefazer?(ConversorDeAudio.descrever(anterior), ConversorDeAudio.descrever(entrada.format))
            }
        }
        guard let conversor else {
            falhas += 1
            return []
        }

        // A capacidade da saída sai da razão entre as taxas, com uma folga: `AVAudioConverter`
        // não promete um número exato de quadros por chamada quando reamostra.
        let razao = destino.sampleRate / entrada.format.sampleRate
        let capacidade = AVAudioFrameCount(Double(entrada.frameLength) * razao) + 64
        guard capacidade > 0,
              let saida = AVAudioPCMBuffer(pcmFormat: destino, frameCapacity: capacidade) else {
            falhas += 1
            return []
        }

        var entregue = false
        var erro: NSError?
        let status = conversor.convert(to: saida, error: &erro) { _, situacao in
            // O padrão do `AVAudioConverter`: entregar o bloco uma vez e, na segunda pergunta,
            // dizer que não há mais dado **agora**. Devolver o mesmo buffer de novo o faria
            // reprocessar as mesmas amostras para sempre.
            if entregue {
                situacao.pointee = .noDataNow
                return nil
            }
            entregue = true
            situacao.pointee = .haveData
            return entrada
        }
        guard status != .error, saida.frameLength > 0, let canal = saida.int16ChannelData else {
            if status == .error { falhas += 1 }
            return []
        }

        let total = Int(saida.frameLength) * Int(destino.channelCount)
        return Array(UnsafeBufferPointer(start: canal[0], count: total))
    }

    /// Dois formatos são o mesmo quando as quatro coisas que a conversão usa são as mesmas.
    ///
    /// Taxa, canais, profundidade e disposição — são exatamente as entradas do `AVAudioConverter`.
    /// Comparar mais do que isso (o `settings` inteiro, que inclui coisas como o layout de canais
    /// em `NSData`) faz o conversor ser refeito por diferenças que não mudam a conversão em nada.
    static func mesmoFormato(_ a: AVAudioFormat, _ b: AVAudioFormat) -> Bool {
        a.sampleRate == b.sampleRate
            && a.channelCount == b.channelCount
            && a.commonFormat == b.commonFormat
            && a.isInterleaved == b.isInterleaved
    }

    /// `CMSampleBuffer` do ScreenCaptureKit → `AVAudioPCMBuffer`, sem copiar amostra.
    private static func paraBuffer(_ amostra: CMSampleBuffer) -> AVAudioPCMBuffer? {
        guard let descricao = CMSampleBufferGetFormatDescription(amostra),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(descricao) else { return nil }
        let formato = AVAudioFormat(streamDescription: asbd)
        guard let formato else { return nil }
        let quadros = AVAudioFrameCount(CMSampleBufferGetNumSamples(amostra))
        guard quadros > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: formato, frameCapacity: quadros) else { return nil }
        buffer.frameLength = quadros

        let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(
            amostra,
            at: 0,
            frameCount: Int32(quadros),
            into: buffer.mutableAudioBufferList)
        guard status == noErr else { return nil }
        return buffer
    }
}
