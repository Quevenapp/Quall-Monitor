import CoreVideo
import Foundation

/// Reescreve o SPS do VideoToolbox para que ele **declare** o que já faz.
///
/// O motivo tem número. Em 2026-08-24, medindo emissor por emissor contra a câmera virtual do
/// Windows (Media Foundation), o `decode` p50 saiu assim:
///
///   | emissor                  | SPS  | VUI      | max_dec_frame_buffering | decode p50 |
///   | macOS VideoToolbox       | 10 B | ausente  | —                       | 169,5 ms   |
///   | Samsung A07 (MediaCodec) | 18 B | presente | 1                       |   3,70 ms  |
///   | Dell (NVENC)             | 22 B | presente | 1                       |   0,55 ms  |
///
/// Sem `bitstream_restriction_flag`, a norma manda o decodificador **inferir** os limites pelo teto
/// do nível (H.264, E.2.1): `max_dec_frame_buffering = MaxDpbFrames`. Para 1280x720 (3600
/// macroblocos) no nível 3.1 (`MaxDpbMbs` = 34816) isso dava `min(34816/3600, 16) = 9` quadros;
/// hoje, com 1920x1080 (8160 macroblocos) no nível 4.0 (`MaxDpbMbs` = 32768), dá
/// `min(32768/8160, 16) = 4`. **O defeito não some com o número menor** — quatro quadros de
/// espera continuam sendo espera, e é por isso que o remendo continua existindo. O
/// decodificador da Microsoft segurou ~5 antes de entregar o primeiro — e `MF_LOW_LATENCY`, que o
/// MFT aceitou, não bastou.
///
/// O irônico é que o encoder já faz a coisa certa: `kVTCompressionPropertyKey_AllowFrameReordering`
/// está em `false` em todos os emissores deste projeto, e Baseline não tem quadro B. Reordenamento
/// é zero. O VideoToolbox só não conta isso a ninguém, e não expõe propriedade para contar — daí
/// reescrever o SPS antes de mandar.
///
/// ## Dois casos, medidos neste Mac em 2026-08-24 com a configuração exata dos emissores
///
/// O que o VideoToolbox emite depende do formato do pixel buffer de entrada:
///
///   * origem `420v` (faixa de vídeo): SPS de 10 B, **sem VUI nenhum**.
///   * origem `420f` (faixa cheia): SPS de 14 B, `2742001fab402802dd3702020202`, **com VUI** —
///     `video_full_range_flag = 1`, cor 2/2/2 (não especificada) e **`bitstream_restriction_flag =
///     0`**. Isto é: o defeito de latência existe nos dois casos, e uma regra do tipo "se já tem
///     VUI eu não encosto" deixaria metade dele de pé.
///
/// Daí os dois caminhos:
///
///   * **Sem VUI**: copia os bits até o `vui_parameters_present_flag`, liga o flag e escreve um VUI
///     inteiro (cor, se o chamador souber; restrição sempre).
///   * **Com VUI e sem restrição**: copia os bits até o `bitstream_restriction_flag`, liga o flag e
///     escreve só a restrição. Tudo o que o VideoToolbox já tinha escrito no VUI passa **bit a
///     bit**, intacto — inclusive a faixa de cor que ele mesmo declarou.
///   * **Com VUI e com restrição**: devolve o original. Quem escreveu sabia mais do que este
///     remendo.
///
/// Em nenhum dos casos o prefixo é reserializado: ele é **copiado bit a bit**. A análise só precisa
/// acertar um deslocamento; nenhuma sutileza de campo depende de eu ter entendido o campo.
///
/// ## O que é declarado, e por que cada valor
///
///   * `max_num_reorder_frames = 0` — verdade medida: sem reordenamento, sem quadro B.
///   * `max_dec_frame_buffering = max_num_ref_frames` — **lido do próprio SPS**, não chutado. A
///     norma exige `max_dec_frame_buffering >= max_num_ref_frames`; tirar o valor do bitstream faz
///     a desigualdade valer por construção. O VideoToolbox emite 1.
///   * `log2_max_mv_length_* = 16` — é o valor que a norma infere quando o campo falta (E.2.1),
///     isto é, não aperta nada; e é o mesmo que o A07 escreve, cujo fluxo já foi medido contra o
///     decodificador da Microsoft.
///   * `max_bytes_per_pic_denom = 0`, `max_bits_per_mb_denom = 0` — "sem limite". Os dois emissores
///     que funcionam escrevem 0. Escrever o default da norma (2 e 1) imporia ao nosso próprio fluxo
///     um limite que ninguém verifica.
///   * `video_signal_type` — só sai se o chamador souber; ver `SinalDeVideo.doPixelBuffer`.
///
/// ## Regras que este arquivo segue
///
///   * **Confere o que escreveu.** O resultado é lido de volta pelo mesmo analisador e comparado
///     campo a campo com o original antes de ser aceito. Qualquer divergência: devolve o original.
///   * **Nunca falha para o lado ruim.** Todo caminho de erro entrega o SPS que o VideoToolbox
///     produziu. Um SPS sem VUI atrasa; um SPS corrompido apaga a imagem.
///   * **Não declara o que não sabe.** Sem atributos de cor no pixel buffer, sai
///     `colour_description_present_flag = 0` — que é o que o NVENC do Dell faz, e é melhor do que
///     chutar 709.
///
/// ## Este arquivo existe em quatro cópias
///
/// `apps/macos/Sources/QuallCaptureKit` (a canônica, onde ficam os testes),
/// `integrations/camera-macos/Fontes/App`, `apps/ios/Quall/Comum` e `apps/ios/PortaoAppex/Comum`.
/// São alvos de build diferentes — pacote SwiftPM, app do Xcode, app do iOS e extensão de difusão —
/// e este projeto já decidiu copiar em vez de criar dependência entre frentes por uma peça pequena
/// (ver o cabeçalho de `integrations/camera-macos/Fontes/App/CodificadorH264.swift`). As quatro têm
/// que ser **byte a byte iguais**: `swift test` em `apps/macos` compara e falha se divergirem.
///
/// O trabalho é feito **uma vez por sessão**: `spsParaEnviar` guarda o resultado e só recalcula se
/// a entrada mudar. No caminho do quadro sobra uma comparação de bytes.
final class RemendoDeSPS {

    /// O que dizer no `video_signal_type` do VUI. Os códigos são os da própria norma (Tabelas E-3,
    /// E-4 e E-5).
    struct SinalDeVideo: Equatable {
        /// `video_full_range_flag`. Não é chute: sai do formato do pixel buffer, onde `420f` **é**
        /// faixa cheia e `420v` **é** faixa de vídeo, por definição do formato.
        var faixaCheia: Bool
        /// `colour_primaries` / `transfer_characteristics` / `matrix_coefficients`. `nil` sai como
        /// `colour_description_present_flag = 0`.
        var cor: Cor?

        struct Cor: Equatable {
            var primarias: UInt32
            var transferencia: UInt32
            var matriz: UInt32
            static let bt709 = Cor(primarias: 1, transferencia: 1, matriz: 1)
        }

        /// Lê a verdade do pixel buffer que vai ser codificado — formato para a faixa, attachments
        /// para a cor. Devolve `nil` quando nem a faixa dá para afirmar (origem RGB, por exemplo,
        /// em que quem converte é o VideoToolbox).
        ///
        /// A tabela de cor é curta de propósito: só 709 e 2020, que são os dois casos em que o
        /// código da norma é certo sem ressalva. Qualquer outro vira "não especificada" em vez de
        /// virar um código plausível — este projeto já perdeu tempo com medição plausível e errada.
        static func doPixelBuffer(_ px: CVPixelBuffer) -> SinalDeVideo? {
            let faixaCheia: Bool
            switch CVPixelBufferGetPixelFormatType(px) {
            case kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                 kCVPixelFormatType_420YpCbCr8PlanarFullRange,
                 kCVPixelFormatType_422YpCbCr8FullRange:
                faixaCheia = true
            case kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                 kCVPixelFormatType_420YpCbCr8Planar,
                 kCVPixelFormatType_422YpCbCr8:
                faixaCheia = false
            default:
                return nil
            }
            // `as? String` porque CFString faz ponte com String em Swift; comparar texto evita a
            // dança de Unmanaged e falha para `nil` sozinho se o attachment vier de outro tipo.
            func atributo(_ chave: CFString) -> String? {
                CVBufferCopyAttachment(px, chave, nil) as? String
            }
            let p = atributo(kCVImageBufferColorPrimariesKey)
            let t = atributo(kCVImageBufferTransferFunctionKey)
            let m = atributo(kCVImageBufferYCbCrMatrixKey)
            var cor: Cor?
            if p == kCVImageBufferColorPrimaries_ITU_R_709_2 as String,
               t == kCVImageBufferTransferFunction_ITU_R_709_2 as String,
               m == kCVImageBufferYCbCrMatrix_ITU_R_709_2 as String {
                cor = .bt709
            } else if p == kCVImageBufferColorPrimaries_ITU_R_2020 as String,
                      m == kCVImageBufferYCbCrMatrix_ITU_R_2020 as String {
                cor = Cor(primarias: 9, transferencia: 2, matriz: 9)
            }
            return SinalDeVideo(faixaCheia: faixaCheia, cor: cor)
        }
    }

    /// A trava existe porque o callback do VideoToolbox não promete fila serial, e o SPS é lido lá
    /// dentro. Ela é tomada uma vez por IDR — não por quadro.
    private let trava = NSLock()
    private var origem: [UInt8] = []
    private var saida: [UInt8] = []
    private var sinalUsado: SinalDeVideo?
    private var _reescrito = false
    private var _recusa: String?
    /// O que o SPS de saída **de fato** declara, relido do resultado — e não o que o chamador
    /// pediu. No caminho 2 o remendo preserva o `video_signal_type` que já estava lá e ignora o
    /// sinal do chamador; dizer "cor=1/1/1" ali afirmaria como escrito o que foi só preservado.
    private var _corFinal: String = "?"

    init() {}

    /// Devolve os bytes do SPS a mandar no fluxo — reescrito, ou o original se não deu.
    /// `original` inclui o byte de cabeçalho do NAL (tipo 7); a saída também.
    func spsParaEnviar(_ original: UnsafeRawBufferPointer, sinal: SinalDeVideo?) -> [UInt8] {
        trava.lock(); defer { trava.unlock() }
        if !origem.isEmpty, sinal == sinalUsado, origem.count == original.count,
           let base = original.baseAddress,
           origem.withUnsafeBytes({ memcmp($0.baseAddress!, base, origem.count) == 0 }) {
            return saida
        }
        origem = [UInt8](original)
        sinalUsado = sinal
        switch Self.comVui(origem, sinal: sinal) {
        case .success(let novo):
            saida = novo; _reescrito = true; _recusa = nil
            let antes = Self.analisar(Self.desescapar(Array(origem.dropFirst())))
            let depois = Self.analisar(Self.desescapar(Array(novo.dropFirst())))
            let cor = depois?.sinal.map {
                "faixa=\($0.faixaCheia ? "cheia" : "vídeo") cor=" +
                ($0.cor.map { c in "\(c.primarias)/\(c.transferencia)/\(c.matriz)" } ?? "não especificada")
            } ?? "sem video_signal_type"
            // Quem já tinha VUI teve o sinal **preservado**; quem não tinha, recebeu o do chamador.
            _corFinal = (antes?.temVui == true) ? "\(cor) (preservado do emissor)" : "\(cor) (escrito aqui)"
        case .failure(let motivo):
            saida = origem; _reescrito = false; _recusa = motivo.description
        }
        return saida
    }

    /// Reescreveu de fato? Para o relatório da corrida — um remendo que desiste em silêncio é pior
    /// do que remendo nenhum.
    var reescrito: Bool { trava.lock(); defer { trava.unlock() }; return _reescrito }

    func resumo() -> String {
        trava.lock(); defer { trava.unlock() }
        guard !origem.isEmpty else { return "SPS: nenhum visto ainda" }
        if _reescrito { return "SPS reescrito com VUI: \(origem.count) -> \(saida.count) bytes, \(_corFinal)" }
        return "SPS original, sem remendo (\(origem.count) bytes): \(_recusa ?? "?")"
    }

    // MARK: - a reescrita

    enum Falha: Error, CustomStringConvertible {
        case naoEhSps, curtoDemais, jaTemRestricao, naoConfere(String)
        var description: String {
            switch self {
            case .naoEhSps: return "o NAL não é SPS (tipo != 7)"
            case .curtoDemais: return "o SPS acabou no meio da leitura"
            case .jaTemRestricao: return "o SPS já declara bitstream_restriction; não mexo"
            case .naoConfere(let o): return "a releitura do SPS reescrito não confere: \(o)"
            }
        }
    }

    static func comVui(_ sps: [UInt8], sinal: SinalDeVideo?) -> Result<[UInt8], Falha> {
        guard let primeiro = sps.first, (primeiro & 0x1F) == 7 else { return .failure(.naoEhSps) }
        let rbsp = desescapar(Array(sps.dropFirst()))
        guard let a = analisar(rbsp) else { return .failure(.curtoDemais) }
        guard a.maxDecFrameBuffering == nil else { return .failure(.jaTemRestricao) }

        var e = Escritor()
        if a.temVui {
            // Caminho 2: o VUI existe (é o que o VideoToolbox faz com origem em faixa cheia) mas
            // não tem a restrição. Copia o VUI inteiro bit a bit e só liga o último flag.
            guard let bitDaRestricao = a.bitDoFlagDeRestricao else { return .failure(.curtoDemais) }
            e.copiar(bits: bitDaRestricao, de: rbsp)
        } else {
            // Caminho 1: não há VUI nenhum. Copia até o flag e escreve o VUI inteiro.
            e.copiar(bits: a.bitDoFlagDeVui, de: rbsp)
            e.flag(true)                              // vui_parameters_present_flag = 1
            e.flag(false)                             // aspect_ratio_info_present_flag
            e.flag(false)                             // overscan_info_present_flag
            if let s = sinal {
                e.flag(true)                          // video_signal_type_present_flag
                e.u(5, 3)                             // video_format = 5 (não especificado)
                e.flag(s.faixaCheia)                  // video_full_range_flag
                if let c = s.cor {
                    e.flag(true)                      // colour_description_present_flag
                    e.u(c.primarias, 8)
                    e.u(c.transferencia, 8)
                    e.u(c.matriz, 8)
                } else {
                    e.flag(false)
                }
            } else {
                e.flag(false)
            }
            e.flag(false)                             // chroma_loc_info_present_flag
            e.flag(false)                             // timing_info_present_flag
            e.flag(false)                             // nal_hrd_parameters_present_flag
            e.flag(false)                             // vcl_hrd_parameters_present_flag
            e.flag(false)                             // pic_struct_present_flag
        }
        e.flag(true)                                  // bitstream_restriction_flag
        e.flag(true)                                  // motion_vectors_over_pic_boundaries_flag
        e.ue(0)                                       // max_bytes_per_pic_denom (sem limite)
        e.ue(0)                                       // max_bits_per_mb_denom   (sem limite)
        e.ue(16)                                      // log2_max_mv_length_horizontal
        e.ue(16)                                      // log2_max_mv_length_vertical
        e.ue(0)                                       // max_num_reorder_frames  <-- o conserto
        e.ue(a.maxNumRefFrames)                       // max_dec_frame_buffering <-- o conserto
        e.fecharRbsp()

        var novo = [primeiro]
        novo.append(contentsOf: escapar(e.bytes))

        if let queixa = conferir(original: a, reescrito: novo, sinal: sinal) {
            return .failure(.naoConfere(queixa))
        }
        return .success(novo)
    }

    /// Lê de volta o que acabou de escrever e compara com o original. Isto não é zelo: o SPS entra
    /// no fluxo de todo IDR, e um erro aqui não degrada a imagem — apaga.
    private static func conferir(original a: Analise, reescrito: [UInt8], sinal: SinalDeVideo?) -> String? {
        guard let b = analisar(desescapar(Array(reescrito.dropFirst()))) else { return "não releu" }
        if !b.temVui { return "saiu sem VUI" }
        if b.perfil != a.perfil || b.nivel != a.nivel { return "perfil/nível mudou" }
        if b.largura != a.largura || b.altura != a.altura { return "dimensão mudou: \(b.largura)x\(b.altura)" }
        if b.maxNumRefFrames != a.maxNumRefFrames { return "max_num_ref_frames mudou" }
        if b.bitDoFlagDeVui != a.bitDoFlagDeVui { return "o prefixo mudou de tamanho" }
        if b.maxNumReorderFrames != 0 { return "max_num_reorder_frames = \(b.maxNumReorderFrames ?? -1)" }
        if b.maxDecFrameBuffering != Int(a.maxNumRefFrames) { return "max_dec_frame_buffering = \(b.maxDecFrameBuffering ?? -1)" }
        if a.temVui {
            // Caminho 2: o sinal de vídeo que já estava lá tem que ter passado intacto.
            if b.sinal != a.sinal { return "o video_signal_type original não sobreviveu" }
        } else {
            let esperado = sinal.map { SinalDeVideo(faixaCheia: $0.faixaCheia, cor: $0.cor) }
            if b.sinal != esperado { return "video_signal_type saiu \(String(describing: b.sinal)), pedido \(String(describing: esperado))" }
        }
        return nil
    }

    // MARK: - análise

    struct Analise {
        var perfil: UInt32 = 0
        var nivel: UInt32 = 0
        var maxNumRefFrames: UInt32 = 0
        var largura = 0
        var altura = 0
        var bitDoFlagDeVui = 0
        var temVui = false
        var sinal: SinalDeVideo?
        var bitDoFlagDeRestricao: Int?
        var maxNumReorderFrames: Int?
        var maxDecFrameBuffering: Int?
    }

    /// Percorre `seq_parameter_set_data()`. Só precisa acertar **deslocamentos em bits**: tudo antes
    /// do ponto de emenda é copiado bit a bit, não reserializado.
    static func analisar(_ rbsp: [UInt8]) -> Analise? {
        var l = Leitor(rbsp)
        var a = Analise()
        guard let perfil = l.u(8), l.u(8) != nil, let nivel = l.u(8), l.ue() != nil else { return nil }
        a.perfil = perfil; a.nivel = nivel
        if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(perfil) {
            guard let cf = l.ue() else { return nil }
            if cf == 3 { guard l.u(1) != nil else { return nil } }
            guard l.ue() != nil, l.ue() != nil, l.u(1) != nil, let escala = l.u(1) else { return nil }
            if escala == 1 {
                for i in 0..<(cf == 3 ? 12 : 8) {
                    guard let presente = l.u(1) else { return nil }
                    if presente == 1 {
                        var ultimo = 8, proximo = 8
                        for _ in 0..<(i < 6 ? 16 : 64) {
                            if proximo != 0 {
                                guard let d = l.se() else { return nil }
                                proximo = ((ultimo + Int(d)) % 256 + 256) % 256
                            }
                            ultimo = proximo != 0 ? proximo : ultimo
                        }
                    }
                }
            }
        }
        guard l.ue() != nil, let poc = l.ue() else { return nil }
        if poc == 0 { guard l.ue() != nil else { return nil } }
        else if poc == 1 {
            guard l.u(1) != nil, l.se() != nil, l.se() != nil, let n = l.ue() else { return nil }
            for _ in 0..<n { guard l.se() != nil else { return nil } }
        }
        guard let refs = l.ue(), l.u(1) != nil, let mbsW = l.ue(), let mbsH = l.ue(), let fmo = l.u(1)
        else { return nil }
        a.maxNumRefFrames = refs
        a.largura = (Int(mbsW) + 1) * 16
        a.altura = (Int(mbsH) + 1) * 16 * (fmo == 1 ? 1 : 2)
        if fmo == 0 { guard l.u(1) != nil else { return nil } }
        guard l.u(1) != nil, let corte = l.u(1) else { return nil }
        if corte == 1 {
            guard let ce = l.ue(), let cd = l.ue(), let ct = l.ue(), let cb = l.ue() else { return nil }
            a.largura -= (Int(ce) + Int(cd)) * 2
            a.altura -= (Int(ct) + Int(cb)) * (fmo == 1 ? 2 : 4)
        }
        a.bitDoFlagDeVui = l.i
        guard let vui = l.u(1) else { return nil }
        a.temVui = vui == 1
        guard a.temVui else { return a }

        guard let aspecto = l.u(1) else { return nil }
        if aspecto == 1 {
            guard let idc = l.u(8) else { return nil }
            if idc == 255 { guard l.u(16) != nil, l.u(16) != nil else { return nil } }
        }
        guard let over = l.u(1) else { return nil }
        if over == 1 { guard l.u(1) != nil else { return nil } }
        guard let vst = l.u(1) else { return nil }
        if vst == 1 {
            guard l.u(3) != nil, let faixa = l.u(1), let temCor = l.u(1) else { return nil }
            var cor: SinalDeVideo.Cor?
            if temCor == 1 {
                guard let p = l.u(8), let t = l.u(8), let m = l.u(8) else { return nil }
                cor = SinalDeVideo.Cor(primarias: p, transferencia: t, matriz: m)
            }
            a.sinal = SinalDeVideo(faixaCheia: faixa == 1, cor: cor)
        }
        guard let croma = l.u(1) else { return nil }
        if croma == 1 { guard l.ue() != nil, l.ue() != nil else { return nil } }
        guard let tempo = l.u(1) else { return nil }
        if tempo == 1 { guard l.u(32) != nil, l.u(32) != nil, l.u(1) != nil else { return nil } }
        guard let nhrd = l.u(1) else { return nil }
        if nhrd == 1 { guard l.hrd() else { return nil } }
        guard let vhrd = l.u(1) else { return nil }
        if vhrd == 1 { guard l.hrd() else { return nil } }
        if nhrd == 1 || vhrd == 1 { guard l.u(1) != nil else { return nil } }
        guard l.u(1) != nil else { return nil }        // pic_struct_present_flag
        a.bitDoFlagDeRestricao = l.i
        guard let restricao = l.u(1) else { return nil }
        if restricao == 1 {
            guard l.u(1) != nil, l.ue() != nil, l.ue() != nil, l.ue() != nil, l.ue() != nil,
                  let reorder = l.ue(), let dpb = l.ue() else { return nil }
            a.maxNumReorderFrames = Int(reorder)
            a.maxDecFrameBuffering = Int(dpb)
        }
        return a
    }

    // MARK: - bits

    struct Leitor {
        let b: [UInt8]
        var i = 0
        init(_ b: [UInt8]) { self.b = b }
        mutating func u(_ n: Int) -> UInt32? {
            guard n <= 32, i + n <= b.count * 8 else { return nil }
            var v: UInt32 = 0
            for _ in 0..<n {
                v = (v << 1) | UInt32((b[i >> 3] >> (7 - (i & 7))) & 1)
                i += 1
            }
            return v
        }
        mutating func ue() -> UInt32? {
            var zeros = 0
            while true {
                guard let bit = u(1) else { return nil }
                if bit == 1 { break }
                zeros += 1
                if zeros > 31 { return nil }
            }
            if zeros == 0 { return 0 }
            guard let resto = u(zeros) else { return nil }
            return (1 << UInt32(zeros)) - 1 + resto
        }
        mutating func se() -> Int32? {
            guard let k = ue() else { return nil }
            return k % 2 == 1 ? Int32((k + 1) / 2) : -Int32(k / 2)
        }
        mutating func hrd() -> Bool {
            guard let n = ue(), u(4) != nil, u(4) != nil else { return false }
            for _ in 0...n { guard ue() != nil, ue() != nil, u(1) != nil else { return false } }
            return u(5) != nil && u(5) != nil && u(5) != nil && u(5) != nil
        }
    }

    struct Escritor {
        private(set) var bytes: [UInt8] = []
        private var usados = 0   // bits já ocupados no último byte, 0..7
        mutating func bit(_ v: UInt32) {
            if usados == 0 { bytes.append(0) }
            if v & 1 == 1 { bytes[bytes.count - 1] |= UInt8(1 << (7 - usados)) }
            usados = (usados + 1) & 7
        }
        mutating func flag(_ v: Bool) { bit(v ? 1 : 0) }
        mutating func u(_ v: UInt32, _ n: Int) {
            var k = n - 1
            while k >= 0 { bit((v >> UInt32(k)) & 1); k -= 1 }
        }
        mutating func ue(_ v: UInt32) {
            let c = v &+ 1
            var n = 0
            while (c >> UInt32(n)) != 0 { n += 1 }   // n = bits de c
            for _ in 0..<(n - 1) { bit(0) }
            u(c, n)
        }
        mutating func copiar(bits n: Int, de origem: [UInt8]) {
            for k in 0..<n { bit(UInt32((origem[k >> 3] >> (7 - (k & 7))) & 1)) }
        }
        /// `rbsp_trailing_bits()`: um 1 e zeros até fechar o byte.
        mutating func fecharRbsp() {
            bit(1)
            while usados != 0 { bit(0) }
        }
    }

    /// Tira os bytes 0x03 de anti-emulação: `00 00 03` -> `00 00`.
    static func desescapar(_ b: [UInt8]) -> [UInt8] {
        var saida: [UInt8] = []
        saida.reserveCapacity(b.count)
        var i = 0
        while i < b.count {
            if i + 2 < b.count, b[i] == 0, b[i + 1] == 0, b[i + 2] == 3 {
                saida.append(0); saida.append(0); i += 3
            } else {
                saida.append(b[i]); i += 1
            }
        }
        return saida
    }

    /// Põe de volta: `00 00 00|01|02|03` vira `00 00 03 xx`.
    static func escapar(_ b: [UInt8]) -> [UInt8] {
        var saida: [UInt8] = []
        saida.reserveCapacity(b.count + 8)
        var zeros = 0
        for x in b {
            if zeros == 2 && x <= 3 { saida.append(3); zeros = 0 }
            saida.append(x)
            zeros = x == 0 ? zeros + 1 : 0
        }
        return saida
    }
}
