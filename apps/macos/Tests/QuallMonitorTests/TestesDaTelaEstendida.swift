import XCTest
@testable import QuallCaptureKit

/// O que dá para segurar sem criar monitor nenhum.
///
/// Criar o monitor de verdade **não** entra aqui: faria a tela de quem roda `swift test` piscar a
/// cada corrida, e consultar o ScreenCaptureKit de dentro do `xctest` pediria Gravação de Tela ao
/// processo errado. A prova do monitor é `sonda-monitor-virtual`, que sobe e solta pelo caminho do
/// produto e dá veredito — ver `docs/tela-estendida.md`.
final class TestesDaTelaEstendida: XCTestCase {

    func testIdentidadeNaoColideComStudioLegado() {
        XCTAssertEqual(MonitorVirtual.produto, 0x0002)
        XCTAssertEqual(MonitorVirtualAuxiliar.nomeDoExecutavel, "quall-monitor-display")
    }

    func testOTabletEm2xTemMetadeDosPontosEOsMesmosPixels() {
        let modo = ModoDoMonitorVirtual.tabletDaBancada(escala: .dobro)
        XCTAssertEqual(modo.larguraEmPixels, 1920)
        XCTAssertEqual(modo.alturaEmPixels, 1200)
        XCTAssertEqual(modo.larguraEmPontos, 960)
        XCTAssertEqual(modo.alturaEmPontos, 600)
        // 30 fps por padrão, decisão do usuário em 10/09 depois da medida no `SM-X230`; 60 é opção.
        // O monitor, com o dobro (11/09).
        XCTAssertEqual(modo.fps, 30)
        XCTAssertEqual(modo.hertz, 60)
        XCTAssertEqual(ModoDoMonitorVirtual.fpsPadrao, 30)
        XCTAssertEqual(ModoDoMonitorVirtual.hertzPadrao, 60)
        XCTAssertEqual(ModoDoMonitorVirtual.tabletDaBancada(fps: 60).hertz, 120)
        XCTAssertEqual(modo.rotulo, "960 × 600 @2x")
    }

    /// Sem o Sidecar, o macOS pôs no monitor virtual o que o app atualiza a cada quadro na metade da
    /// taxa do monitor (11/09): com o monitor igual ao fps, a transmissão de 30 recebia 15–18 imagens
    /// novas por segundo; com o dobro, 29. Se alguém voltar o monitor para o fps, isto fica vermelho.
    func testOMonitorNasceComODobroDoFps() {
        XCTAssertEqual(ModoDoMonitorVirtual.hertzDoMonitor(paraFps: 30), 60)
        XCTAssertEqual(ModoDoMonitorVirtual.hertzDoMonitor(paraFps: 60), 120)
        XCTAssertEqual(ModoDoMonitorVirtual.hertzDoMonitor(paraFps: 90), 120, "o teto é 120 Hz")
        let iPhoneX = ModoDoMonitorVirtual.paraTela(larguraPx: 1125, alturaPx: 2436, hertz: 60, fps: 30)
        XCTAssertEqual(iPhoneX?.hertz, 60)
        XCTAssertEqual(iPhoneX?.fps, 30)
        // Sem fps, o do monitor — o auxiliar e as sondas, que só sabem o Hz.
        XCTAssertEqual(ModoDoMonitorVirtual(larguraEmPixels: 1920, alturaEmPixels: 1200, hertz: 30).fps, 30)
        // E nunca acima do monitor: a captura não tira mais imagens do que ele atualiza.
        XCTAssertEqual(ModoDoMonitorVirtual(larguraEmPixels: 1920, alturaEmPixels: 1200, hertz: 30, fps: 60).fps, 30)
        // O seletor de fontes mostra o fps, e não o Hz do monitor.
        XCTAssertEqual(FonteDeCaptura.telaEstendida(.tabletDaBancada()).detalhe, "960 × 600 @2x · 30 fps")
    }

    func testOTabletEm1xTemPontosIguaisAosPixels() {
        let modo = ModoDoMonitorVirtual.tabletDaBancada(escala: .umPraUm)
        XCTAssertEqual(modo.larguraEmPontos, 1920)
        XCTAssertEqual(modo.alturaEmPontos, 1200)
        XCTAssertEqual(modo.rotulo, "1920 × 1200")
    }

    /// Os dois pontos medidos, nas duas receitas (10/09 e 11/09): 237 × 148 mm nasce 2x, 508 × 318 mm
    /// nasce 1x. Se alguém mexer na densidade de uma escala, o monitor passa a nascer na escala
    /// errada — e isto fica vermelho.
    func testOTamanhoFisicoDeclaradoEOQueFoiMedido() {
        XCTAssertEqual(ModoDoMonitorVirtual.tabletDaBancada(escala: .dobro).milimetros,
                       CGSize(width: 237, height: 148))
        XCTAssertEqual(ModoDoMonitorVirtual.tabletDaBancada(escala: .umPraUm).milimetros,
                       CGSize(width: 508, height: 318))
    }

    /// Cada escala tem identidade própria, e nenhuma é a série 1 — a das sondas, que o macOS lembra
    /// num modo forçado.
    func testCadaEscalaTemIdentidadePropria() {
        let duas = MonitorVirtual.serie(para: .dobro)
        let uma = MonitorVirtual.serie(para: .umPraUm)
        XCTAssertNotEqual(duas, uma)
        XCTAssertNotEqual(duas, 1)
        XCTAssertNotEqual(uma, 1)
    }

    /// Mais de um monitor: cada índice tem identidade própria, em qualquer escala, e o índice 0 é a
    /// de sempre — o produto de um monitor só não pode mudar de identidade (o macOS perderia o que
    /// lembra dele).
    func testCadaIndiceTemIdentidadePropriaEOZeroEAdeSempre() {
        XCTAssertEqual(MonitorVirtual.serie(para: .dobro, indice: 0), MonitorVirtual.serie(para: .dobro))
        XCTAssertEqual(MonitorVirtual.serie(para: .umPraUm, indice: 0), MonitorVirtual.serie(para: .umPraUm))
        var vistas = Set<UInt32>()
        for indice in 0..<8 {
            for escala in [ModoDoMonitorVirtual.Escala.dobro, .umPraUm] {
                let serie = MonitorVirtual.serie(para: escala, indice: indice)
                XCTAssertNotEqual(serie, 1, "a série 1 é das sondas")
                XCTAssertTrue(vistas.insert(serie).inserted, "série \(serie) repetida")
            }
        }
        XCTAssertEqual(MonitorVirtual.serie(para: .dobro, indice: -3), MonitorVirtual.serie(para: .dobro))
    }

    // --- o monitor para a tela de cada aparelho ---------------------------------------------------

    /// O tablet da bancada continua exatamente como hoje: 1920 × 1200 a 2x.
    func testOTabletDaBancadaSaiIgualAHoje() {
        let m = ModoDoMonitorVirtual.paraTela(larguraPx: 1200, alturaPx: 1920, hertz: 60, fps: 30)
        XCTAssertEqual(m, ModoDoMonitorVirtual.tabletDaBancada(escala: .dobro))
    }

    /// Cada aparelho da bancada em 2x (o padrão), com o monitor que nasceu na medida de 11/09 (receita
    /// em pontos, identidade nova, um formato por vez): o 2x do painel quando a metade passa do piso de
    /// desktop do macOS; nos telefones pequenos, o 2x reduzido — o piso na proporção do painel, e a
    /// saída no painel.
    func testEm2xCadaAparelhoGanhaO2xDoPainelOuO2xReduzido() {
        let casos: [(nome: String, tela: (Int, Int), monitor: [Int], saida: [Int])] = [
            ("tablet", (1200, 1920), [1920, 1200], [1920, 1200]),
            ("iPad A16", (1640, 2360), [2360, 1640], [2360, 1640]),
            ("S24", (1440, 3120), [3120, 1440], [3120, 1440]),
            ("iPhone X", (1125, 2436), [2436, 1124], [2436, 1124]),
            ("A07", (720, 1600), [2356, 1060], [1600, 720]),
            ("A10s", (720, 1520), [2238, 1060], [1520, 720]),
            ("iPhone 7", (750, 1334), [1886, 1060], [1334, 750]),
        ]
        for c in casos {
            let m = ModoDoMonitorVirtual.paraTela(larguraPx: c.tela.0, alturaPx: c.tela.1, hertz: 30)
            XCTAssertEqual(m.map { [$0.larguraEmPixels, $0.alturaEmPixels] }, c.monitor, c.nome)
            XCTAssertEqual(m.map { [$0.larguraDaSaida, $0.alturaDaSaida] }, c.saida, c.nome)
            XCTAssertEqual(m?.escala, .dobro, c.nome)
            XCTAssertEqual(m?.reduzido, c.monitor != c.saida, c.nome)
            // O reduzido fica no piso: aceito como desktop, e nunca menor que o painel.
            if let m, m.reduzido {
                XCTAssertTrue(ModoDoMonitorVirtual.cabeEm2x(larguraPx: m.larguraEmPixels, alturaPx: m.alturaEmPixels), c.nome)
                XCTAssertGreaterThanOrEqual(m.larguraEmPixels, m.larguraDaSaida, c.nome)
                XCTAssertGreaterThanOrEqual(m.alturaEmPixels, m.alturaDaSaida, c.nome)
                XCTAssertEqual(Double(m.larguraEmPixels) / Double(m.alturaEmPixels),
                               Double(m.larguraDaSaida) / Double(m.alturaDaSaida), accuracy: 0.005, c.nome)
            }
        }
        XCTAssertEqual(ModoDoMonitorVirtual.paraTela(larguraPx: 1440, alturaPx: 3120, hertz: 60)?.hertz, 60)
    }

    /// Em 1x, todo aparelho da bancada ganha o painel inteiro, sem redução — o iPhone X com uma linha
    /// a menos (1125 é ímpar).
    func testEm1xCadaAparelhoGanhaOPainelInteiro() {
        let casos: [(tela: (Int, Int), monitor: [Int])] = [
            ((1200, 1920), [1920, 1200]), ((1640, 2360), [2360, 1640]), ((1440, 3120), [3120, 1440]),
            ((1125, 2436), [2436, 1124]), ((720, 1600), [1600, 720]), ((720, 1520), [1520, 720]),
            ((750, 1334), [1334, 750]),
        ]
        for c in casos {
            let m = ModoDoMonitorVirtual.paraTela(larguraPx: c.tela.0, alturaPx: c.tela.1, hertz: 30, escala: .umPraUm)
            XCTAssertEqual(m.map { [$0.larguraEmPixels, $0.alturaEmPixels] }, c.monitor, "\(c.tela)")
            XCTAssertEqual(m?.escala, .umPraUm)
            XCTAssertEqual(m?.reduzido, false)
        }
    }

    /// O registro diz que o monitor é reduzido, e para quanto; a tela de espera mostra o que se vê em
    /// Ajustes > Monitores.
    func testO2xReduzidoSeDizNoRegistro() {
        let a10s = ModoDoMonitorVirtual.paraTela(larguraPx: 720, alturaPx: 1520, hertz: 30)!
        XCTAssertTrue(a10s.descricao.contains("reduzidos para 1520x720"), a10s.descricao)
        XCTAssertEqual(a10s.rotulo, "1119 × 530 @2x")
        XCTAssertFalse(ModoDoMonitorVirtual.tabletDaBancada().descricao.contains("reduzidos"))
    }

    /// O piso medido: 768 × 600 e 960 × 520 pt ficaram de fora; 800 × 600 e 960 × 530 passaram.
    func testOPisoDoDobroEOQueFoiMedido() {
        XCTAssertFalse(ModoDoMonitorVirtual.cabeEm2x(larguraPx: 1536, alturaPx: 1200))
        XCTAssertFalse(ModoDoMonitorVirtual.cabeEm2x(larguraPx: 1920, alturaPx: 1040))
        XCTAssertFalse(ModoDoMonitorVirtual.cabeEm2x(larguraPx: 3200, alturaPx: 1040))
        XCTAssertTrue(ModoDoMonitorVirtual.cabeEm2x(larguraPx: 1600, alturaPx: 1200))
        XCTAssertTrue(ModoDoMonitorVirtual.cabeEm2x(larguraPx: 1920, alturaPx: 1060))
        XCTAssertTrue(ModoDoMonitorVirtual.cabeEm2x(larguraPx: 3200, alturaPx: 1060))
    }

    /// Declarado pequeno demais, o monitor nunca fica online (iPhone 7 em 2x: 164 × 92 mm, 10/09).
    /// A área cresce até o piso, na mesma proporção; acima dele, nada muda.
    func testOTamanhoFisicoTemAreaMinima() {
        let pequeno = ModoDoMonitorVirtual(larguraEmPixels: 1334, alturaEmPixels: 750, escala: .dobro).milimetros
        XCTAssertGreaterThanOrEqual(pequeno.width * pequeno.height, 17_000)
        XCTAssertEqual(pequeno.width / pequeno.height, 1334.0 / 750.0, accuracy: 0.02)
        XCTAssertEqual(ModoDoMonitorVirtual(larguraEmPixels: 1334, alturaEmPixels: 750, escala: .umPraUm).milimetros,
                       CGSize(width: 353, height: 198))
    }

    /// Tela que não veio, ou pequena demais para trabalhar, cai no formato de sempre (quem chama).
    func testTelaAusenteOuPequenaDemaisNaoDaModo() {
        XCTAssertNil(ModoDoMonitorVirtual.paraTela(larguraPx: 0, alturaPx: 0, hertz: 30))
        XCTAssertNil(ModoDoMonitorVirtual.paraTela(larguraPx: 480, alturaPx: 800, hertz: 30))
        XCTAssertNil(ModoDoMonitorVirtual.paraTela(larguraPx: 720, alturaPx: 1278, hertz: 30))
        // A borda entra: em 1x no painel, em 2x reduzida (a metade, 640 × 360, fica abaixo do piso).
        let borda = ModoDoMonitorVirtual.paraTela(larguraPx: 720, alturaPx: 1280, hertz: 30, escala: .umPraUm)
        XCTAssertEqual(borda.map { [$0.larguraEmPixels, $0.alturaEmPixels] }, [1280, 720])
        XCTAssertEqual(borda?.escala, .umPraUm)
        let bordaEm2x = ModoDoMonitorVirtual.paraTela(larguraPx: 720, alturaPx: 1280, hertz: 30)
        XCTAssertEqual(bordaEm2x.map { [$0.larguraDaSaida, $0.alturaDaSaida] }, [1280, 720])
        XCTAssertEqual(bordaEm2x?.reduzido, true)
    }

    /// O teto é o quadro do nível do núcleo (5.2), e não 1920 px: tela maior encolhe na mesma
    /// proporção até caber, e o monitor é o que se codifica. Quem cabe fica como está.
    func testTelaMaiorQueONivelEncolheNaMesmaProporcao() {
        let cincoK = ModoDoMonitorVirtual.paraTela(larguraPx: 5120, alturaPx: 2880, hertz: 30)
        XCTAssertEqual(cincoK.map { [$0.larguraEmPixels, $0.alturaEmPixels] }, [4096, 2304])
        XCTAssertEqual(cincoK?.escala, .dobro)
        let xdr = ModoDoMonitorVirtual.paraTela(larguraPx: 6016, alturaPx: 3384, hertz: 30)!
        XCTAssertLessThanOrEqual(TetoDoEmissor.macroblocos(largura: xdr.larguraEmPixels, altura: xdr.alturaEmPixels),
                                 ModoDoMonitorVirtual.macroblocosDoNivel)
        XCTAssertEqual(Double(xdr.larguraEmPixels) / Double(xdr.alturaEmPixels), 6016.0 / 3384.0, accuracy: 0.01)
        XCTAssertEqual(xdr.larguraEmPixels % 2, 0)
        XCTAssertEqual(xdr.alturaEmPixels % 2, 0)
        for (l, a) in [(2560, 1664), (5120, 1440), (1440, 3120)] {
            let m = ModoDoMonitorVirtual.paraTela(larguraPx: l, alturaPx: a, hertz: 30)
            XCTAssertEqual(m.map { [$0.larguraEmPixels, $0.alturaEmPixels] }, [max(l, a), min(l, a)], "\(l)x\(a)")
        }
    }

    // --- a identidade de cada aparelho -------------------------------------------------------------

    func testCadaAparelhoGuardaOSeuIndice() {
        var t = TabelaDeIndices()
        XCTAssertEqual(t.indice(para: "tablet", emUso: [], agora: 1), 0)
        XCTAssertEqual(t.indice(para: "iphone", emUso: [0], agora: 2), 1)
        // Voltando depois, cada um volta com o seu — é o que faz o macOS lembrar a arrumação.
        XCTAssertEqual(t.indice(para: "iphone", emUso: [], agora: 3), 1)
        XCTAssertEqual(t.indice(para: "tablet", emUso: [1], agora: 4), 0)
    }

    /// O mesmo aparelho duas vezes no ar (reconexão antes de a sessão velha cair): a segunda sessão
    /// ganha um índice livre só dela, sem mexer no que está gravado.
    func testDoisMonitoresVivosNuncaDividemIndice() {
        var t = TabelaDeIndices()
        XCTAssertEqual(t.indice(para: "tablet", emUso: [], agora: 1), 0)
        let segundo = t.indice(para: "tablet", emUso: [0], agora: 2)
        XCTAssertNotEqual(segundo, 0)
        XCTAssertEqual(t.entradas["tablet"]?.indice, 0)
    }

    /// Cheia, sai o aparelho usado há mais tempo que não esteja no ar.
    func testATabelaTemTetoESaiOMaisAntigoForaDoAr() {
        var t = TabelaDeIndices()
        for i in 0..<TabelaDeIndices.teto { _ = t.indice(para: "a\(i)", emUso: [], agora: Double(i + 10)) }
        XCTAssertEqual(t.entradas.count, TabelaDeIndices.teto)
        // a0 é o mais antigo, mas está no ar: quem sai é a1.
        let novo = t.indice(para: "novo", emUso: [0], agora: 100)
        XCTAssertEqual(t.entradas.count, TabelaDeIndices.teto)
        XCTAssertNil(t.entradas["a1"])
        XCTAssertNotNil(t.entradas["a0"])
        XCTAssertEqual(novo, 1)
    }

    func testAFonteDaTelaEstendidaETelaEMantemOIdQualquerQueSejaAEscala() {
        let dobro = FonteDeCaptura.telaEstendida(.tabletDaBancada(escala: .dobro))
        let uma = FonteDeCaptura.telaEstendida(.tabletDaBancada(escala: .umPraUm))
        XCTAssertTrue(dobro.ehTela)
        XCTAssertEqual(dobro.presetSugerido, .screen)
        XCTAssertEqual(dobro.modoDaTelaEstendida?.escala, .dobro)
        // O id é o que o seletor usa para dizer "a sua escolha sumiu"; a escala não pode mudá-lo.
        XCTAssertEqual(dobro.id, "tela-estendida")
        XCTAssertEqual(uma.id, dobro.id)
    }

    func testMonitorDeVerdadeNaoETelaEstendida() {
        let tela = FonteDeCaptura(id: "tela:1", tipo: .tela(1), nome: "Tela interna", detalhe: "")
        XCTAssertTrue(tela.ehTela)
        XCTAssertNil(tela.modoDaTelaEstendida)
    }

    /// 1920 × 1200 não cabe no nível da cópia local — é o motivo de a tela estendida perguntar ao
    /// núcleo. Se um dia couber, o motivo acabou e este teste avisa.
    func testACopiaLocalReduziriaOTabletInteiro() {
        let local = TetoDoEmissor.Aplicado.daCopiaLocal(largura: 1920, altura: 1200, fps: 60)
        XCTAssertTrue(local.saida.reduziuTamanho)
        XCTAssertTrue(local.saida.reduziuFps)
        XCTAssertLessThan(local.saida.largura, 1920)
        XCTAssertEqual(local.saida.fps, 30)
        XCTAssertEqual(local.origem, "cópia local")
        XCTAssertEqual(local.saida, TetoDoEmissor.ajustar(largura: 1920, altura: 1200, fps: 60))
    }

    /// O relato diz o nível **de quem fez a conta**. Com o teto do núcleo (5.2) ele não pode dizer
    /// 4.0 — foi exatamente o defeito de 02/09 com o `levelIdc` em 31.
    func testORelatoDizONivelDoTetoInjetado() {
        let saida = TetoDoEmissor.Saida(largura: 1920, altura: 1200, fps: 60, reduziuTamanho: false,
                                        reduziuFps: false, macroblocos: 9_000, exigeRecorte: false,
                                        tetoDeTaxaBps: 20_000_000)
        let linha = TetoDoEmissor.relato(capturaLargura: 1920, capturaAltura: 1200, fpsPedido: 60,
                                         saida: saida, levelIdc: 52, maxFS: 36_864)
        XCTAssertTrue(linha.contains("nível 5.2"), linha)
        XCTAssertTrue(linha.contains("9000/36864"), linha)
        XCTAssertFalse(linha.contains("4.0"), linha)
    }

    /// Sem parâmetros, o relato é o de antes — nível e `maxFS` da cópia local.
    func testORelatoSemNivelContinuaODeAntes() {
        let saida = TetoDoEmissor.ajustar(largura: 1280, altura: 720, fps: 30)
        let linha = TetoDoEmissor.relato(capturaLargura: 1280, capturaAltura: 720, fpsPedido: 30, saida: saida)
        XCTAssertTrue(linha.contains("nível 4.0"), linha)
        XCTAssertTrue(linha.contains("/8192"), linha)
    }

    /// A frase escrita para a pessoa chega à tela: o `Emissor` mostra `localizedDescription`, e sem
    /// `LocalizedError` ali saía "a operação não pôde ser concluída (… erro 4)".
    func testAMensagemDoErroDeCapturaChegaAoLocalizedDescription() {
        let erro: Error = CaptureError.monitorSumiu(nome: "DELL U2415", testemunhas: "nenhuma")
        XCTAssertTrue(erro.localizedDescription.contains("DELL U2415"), erro.localizedDescription)
        XCTAssertTrue(erro.localizedDescription.contains("desconectado"), erro.localizedDescription)
    }

    func testVirarEspelhoDizOQueFazer() {
        let erro: Error = CaptureError.monitorVirouEspelho(nome: "Tela estendida", espelhoDe: "\"iPad A16\"")
        XCTAssertTrue(erro.localizedDescription.contains("iPad A16"), erro.localizedDescription)
        XCTAssertTrue(erro.localizedDescription.contains("Usar Como Tela Estendida"), erro.localizedDescription)
        XCTAssertFalse(erro.localizedDescription.contains("desconectado"), erro.localizedDescription)
    }

    /// Um monitor que não existe não está "num espelho": sumiu de verdade.
    func testMonitorInexistenteNaoEEspelho() {
        XCTAssertNil(VigiaDeMonitor.espelhoDe(kCGNullDirectDisplay))
        XCTAssertNil(VigiaDeMonitor.espelhoDe(0xDEAD_BEEF))
    }
}
