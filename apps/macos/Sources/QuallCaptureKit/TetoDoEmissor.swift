import Foundation

/// O teto do que este emissor põe na rede — **porte declarado de `crates/quall-core/src/teto.rs`**.
///
/// # Isto é uma cópia, e a cópia está aqui dita
///
/// A regra canônica mora no núcleo, ao lado do `profile-level-id` que ela serve, e a fronteira C
/// a expõe em `quall_teto_ajustar` para que as quatro cascas perguntem em vez de decidir. Esta
/// casca **não** pergunta, e o motivo é uma decisão de dependência que o projeto já tinha tomado:
/// `QuallCaptureKit` não depende de `CQuall`, de propósito — "captura+encode não precisa de rede
/// para ser testado, e não deveria passar a precisar" (`Package.swift`). Ligar o núcleo aqui
/// obrigaria `quall-capture`, `sonda-sps` e o alvo de testes a linkar `libquall.a`, e `swift test`
/// passaria a exigir um artefato de Rust construído antes.
///
/// **O que segura a cópia é `TestesDoTetoDoEmissor`**, que confere esta implementação contra a
/// mesma tabela de vetores que os testes do núcleo usam. Divergir passa a ser uma suíte vermelha,
/// e não uma descoberta de bancada seis meses depois — que é exatamente como o preset de áudio, o
/// piso do PLI e a curva µ-law cobraram o seu preço neste projeto.
///
/// **O acabamento certo é consultar a fronteira C**, e ele está no relatório como não feito.
///
/// # Por que a conta é em macroblocos
///
/// **Nível 4.0 desde 01/09/2026** (era 3.1). Os três números desta cópia — `maxFS`, `maxMBPS` e o
/// retângulo de entrada impossível — mudaram juntos, e `TestesDoTetoDoEmissor` é o que impede que
/// um deles fique para trás.
///
/// A norma H.264 (Anexo A) não limita largura e altura: limita a área em macroblocos (`MaxFS`) e
/// a taxa de macroblocos por segundo (`MaxMBPS`). 1920x1080 são 120x68 = 8160 macroblocos contra
/// o `MaxFS` de 8192 do nível 4.0, e a 30 fps são 244800 contra o `MaxMBPS` de 245760: cabe por
/// 0,4 % nas duas contas. (No 3.1 o par saturado era 1280x720 com 3600 e 108000.) Em tela
/// alongada a diferença importa: 720x1520 vira 652x1378 preservando a proporção, em vez de
/// 606x1280 espremido numa caixa 16:9 que ninguém pediu.
///
/// Ver o cabeçalho de `crates/quall-core/src/teto.rs` para o raciocínio completo e para os
/// números medidos que o justificam.
public enum TetoDoEmissor {
    /// Um macrobloco H.264 tem 16x16 pixels.
    public static let ladoDoMacrobloco = 16
    /// `MaxFS` do nível 4.0, que é o que `PERFIL_H264` anuncia.
    public static let maxFS = 8_192
    /// `MaxMBPS` do nível 4.0.
    public static let maxMBPS = 245_760
    /// `MaxBR` do nível 4.0, em kbps de VCL. Ver `teto_de_taxa` no núcleo.
    public static let maxBRkbps = 20_000
    /// `level_idc` anunciado: 40 é o nível 4.0.
    ///
    /// **Estava 31 até 02/09/2026**, quando `maxFS` e `maxMBPS` já eram os do 4.0 desde o dia
    /// anterior: os três mudam juntos e um ficou para trás. Só aparecia no texto de `relato`, que
    /// por isso imprimia "teto do nível 3.1" numa sessão de 4.0 — registro que mente sobre o
    /// próprio nível é pior que registro nenhum.
    public static let levelIdc = 40
    /// Política de produto, acima e além da norma. Ver `FPS_MAXIMO` no núcleo.
    public static let fpsMaximo: Int32 = 30

    /// Quantos macroblocos ocupa um quadro. Arredonda **para cima**: um quadro de 1281 px de
    /// largura ocupa 81 macroblocos e recorta o resto no SPS.
    public static func macroblocos(largura: Int, altura: Int) -> Int {
        let l = (largura + ladoDoMacrobloco - 1) / ladoDoMacrobloco
        let a = (altura + ladoDoMacrobloco - 1) / ladoDoMacrobloco
        return l * a
    }

    /// O que sai de `ajustar`.
    public struct Saida: Equatable {
        public let largura: Int
        public let altura: Int
        public let fps: Int32
        public let reduziuTamanho: Bool
        public let reduziuFps: Bool
        public let macroblocos: Int
        /// A saída não é múltipla de 16 nos dois lados, então o SPS **precisa** declarar
        /// `frame_cropping`. Não é defeito — é o caso comum. Existe para o relato poder dizê-lo:
        /// este projeto já quase reprovou uma corrida **por ela estar certa**, quando o iPhone X
        /// decodificou em 590x1280 e o roteiro de prova não sabia ler o recorte.
        public let exigeRecorte: Bool
        /// **Quantos bits por segundo pedir ao encoder.** Ver `tetoDeTaxa`.
        public let tetoDeTaxaBps: Int

        /// Público para que o teto do **núcleo** (`quall_teto_ajustar_para`, alcançável só por quem
        /// linka `CQuall`) chegue aqui com o mesmo tipo que a cópia local produz.
        public init(largura: Int, altura: Int, fps: Int32, reduziuTamanho: Bool, reduziuFps: Bool,
                    macroblocos: Int, exigeRecorte: Bool, tetoDeTaxaBps: Int) {
            self.largura = largura
            self.altura = altura
            self.fps = fps
            self.reduziuTamanho = reduziuTamanho
            self.reduziuFps = reduziuFps
            self.macroblocos = macroblocos
            self.exigeRecorte = exigeRecorte
            self.tetoDeTaxaBps = tetoDeTaxaBps
        }
    }

    /// Um teto e **de onde ele veio**: a saída, o nível que a limitou e quem fez a conta.
    ///
    /// Existe porque o relato imprimia sempre o nível desta cópia (4.0). Com a tela estendida o teto
    /// passa a vir do núcleo, que anuncia 5.2 — e um registro que dissesse "nível 4.0" numa sessão
    /// de 5.2 seria o mesmo defeito de 02/09 (`levelIdc` em 31 numa sessão de 4.0): registro que
    /// mente sobre o próprio nível é pior que registro nenhum.
    public struct Aplicado: Equatable {
        public let saida: Saida
        public let levelIdc: Int
        public let maxFS: Int
        /// "núcleo" ou "cópia local" — vai para o registro.
        public let origem: String

        public init(saida: Saida, levelIdc: Int, maxFS: Int, origem: String) {
            self.saida = saida
            self.levelIdc = levelIdc
            self.maxFS = maxFS
            self.origem = origem
        }

        /// O comportamento de antes: a cópia local, nível 4.0, 30 fps.
        public static func daCopiaLocal(largura: Int, altura: Int, fps: Int32) -> Aplicado {
            Aplicado(saida: TetoDoEmissor.ajustar(largura: largura, altura: altura, fps: fps),
                     levelIdc: TetoDoEmissor.levelIdc, maxFS: TetoDoEmissor.maxFS, origem: "cópia local")
        }
    }

    /// A taxa de referência: o valor de produto, e a resolução em que ele foi escolhido.
    /// 4 000 000 bps a 1280x720@30 são ~0,145 bit por pixel.
    public static let taxaDeReferenciaBps = 4_000_000
    public static let pixelsPorSegundoDaReferencia = 1280 * 720 * 30
    /// O piso do controlador de taxa (`quall_core::taxa::PISO_BPS`). Um teto abaixo do piso não é
    /// um teto: o controlador nasceria já "no piso", dizendo que o enlace não dá quando o
    /// problema seria a aritmética.
    public static let pisoDeTaxaBps = 400_000

    /// **Quantos bits por segundo pedir ao encoder para este quadro** — porte de
    /// `quall_core::teto::teto_de_taxa`.
    ///
    /// Mantém a densidade medida do produto e a aplica à taxa de pixels de **saída**. 720p30
    /// recebe exatamente 4 000 000 (a sessão que não cresceu é a mesma de antes); 1080p30 recebe
    /// 9 000 000, que é a taxa que faltou subir quando o teto de resolução subiu em 01/09/2026.
    ///
    /// Limitado por cima pelo `MaxBR` do nível — passar dele violaria o `profile-level-id` que o
    /// núcleo publica — e por baixo pelo piso do controlador.
    public static func tetoDeTaxa(largura: Int, altura: Int, fps: Int32,
                                  maxBRkbps: Int = TetoDoEmissor.maxBRkbps) -> Int {
        let pixelsPorSegundo = largura * altura * Int(max(1, fps))
        let bruto = pixelsPorSegundo * taxaDeReferenciaBps / max(1, pixelsPorSegundoDaReferencia)
        return max(pisoDeTaxaBps, min(bruto, maxBRkbps * 1_000))
    }

    /// Ajusta uma geometria de captura para caber no nível que o SDP anuncia, preservando a
    /// proporção. Nunca amplia; sempre devolve lados pares.
    ///
    /// Passo a passo idêntico ao do núcleo, **inclusive o laço de acerto no fim**: escalar por
    /// `sqrt(MaxFS/mbs)` e arredondar pode devolver um quadro que ainda ocupa um macrobloco a
    /// mais, porque a contagem arredonda para cima nos dois eixos. Conferir a conta depois de
    /// arredondar, e não confiar na fórmula, é a mesma regra de sempre — verificar no artefato.
    public static func ajustar(largura: Int, altura: Int, fps: Int32,
                               maxFS: Int = TetoDoEmissor.maxFS,
                               maxMBPS: Int = TetoDoEmissor.maxMBPS,
                               fpsMaximo: Int32 = TetoDoEmissor.fpsMaximo) -> Saida {
        guard largura > 0, altura > 0 else {
            // Entrada impossível (a API de captura mentiu): o retângulo que satura o nível, que é
            // a saída mais conservadora possível. Nunca `nil` — quem chama está abrindo uma
            // sessão e não tem o que fazer com uma ausência.
            return Saida(largura: 1792, altura: 1008, fps: min(fpsMaximo, max(1, fps)),
                         reduziuTamanho: true, reduziuFps: false,
                         macroblocos: macroblocos(largura: 1792, altura: 1008),
                         exigeRecorte: false,
                         tetoDeTaxaBps: tetoDeTaxa(largura: 1792, altura: 1008,
                                                   fps: min(fpsMaximo, max(1, fps))))
        }

        var l = largura
        var a = altura

        let mbs = macroblocos(largura: l, altura: a)
        if mbs > maxFS {
            let escala = (Double(maxFS) / Double(mbs)).squareRoot()
            l = max(2, Int((Double(largura) * escala).rounded()))
            a = max(2, Int((Double(altura) * escala).rounded()))
        }
        l -= l % 2; a -= a % 2
        l = max(2, l); a = max(2, a)

        var voltas = 0
        while macroblocos(largura: l, altura: a) > maxFS && voltas < 1_000 {
            if l >= a {
                let novo = max(2, l - ladoDoMacrobloco)
                a = max(2, Int((Double(novo) * Double(altura) / Double(largura)).rounded()))
                l = novo
            } else {
                let novo = max(2, a - ladoDoMacrobloco)
                l = max(2, Int((Double(novo) * Double(largura) / Double(altura)).rounded()))
                a = novo
            }
            l -= l % 2; a -= a % 2
            l = max(2, l); a = max(2, a)
            voltas += 1
        }

        let mbsFinal = macroblocos(largura: l, altura: a)
        let pedido = max(1, fps)
        let peloNivel = Int32(max(1, maxMBPS / max(1, mbsFinal)))
        let saidaFps = min(pedido, min(peloNivel, fpsMaximo))

        return Saida(largura: l, altura: a, fps: saidaFps,
                     reduziuTamanho: l != largura || a != altura,
                     reduziuFps: saidaFps != fps,
                     macroblocos: mbsFinal,
                     exigeRecorte: l % ladoDoMacrobloco != 0 || a % ladoDoMacrobloco != 0,
                     // **A taxa é da saída, nunca da entrada.**
                     tetoDeTaxaBps: tetoDeTaxa(largura: l, altura: a, fps: saidaFps))
    }

    /// Uma linha para o registro, dizendo o que foi limitado e o que passou intacto. Existe
    /// porque "capturou 2560x1664" e "codificou 1174x762" são frases diferentes, e o registro
    /// desta casa só sabia imprimir uma delas — que é precisamente como quatro emissores sem teto
    /// passaram meses sem que ninguém os nomeasse.
    public static func relato(capturaLargura: Int, capturaAltura: Int, fpsPedido: Int32,
                              saida: Saida, levelIdc: Int = TetoDoEmissor.levelIdc,
                              maxFS: Int = TetoDoEmissor.maxFS) -> String {
        let nivel = "\(levelIdc / 10).\(levelIdc % 10)"
        if !saida.reduziuTamanho && !saida.reduziuFps {
            return "teto: nada a limitar — \(capturaLargura)x\(capturaAltura) @\(saida.fps)fps "
                + "ocupa \(saida.macroblocos)/\(maxFS) macroblocos do nível \(nivel), "
                + "a \(saida.tetoDeTaxaBps / 1_000) kbps"
        }
        let tamanho = saida.reduziuTamanho
            ? "\(capturaLargura)x\(capturaAltura) -> \(saida.largura)x\(saida.altura)"
            : "\(saida.largura)x\(saida.altura) (intacto)"
        let taxa = saida.reduziuFps ? "\(fpsPedido) -> \(saida.fps) fps" : "\(saida.fps) fps (intacto)"
        let recorte = saida.exigeRecorte ? " | o SPS precisa declarar frame_cropping" : ""
        return "teto do nível \(nivel): \(tamanho), \(taxa) | "
            + "\(saida.macroblocos)/\(maxFS) macroblocos | "
            + "\(saida.tetoDeTaxaBps / 1_000) kbps\(recorte)"
    }
}
