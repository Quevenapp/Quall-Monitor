import CoreGraphics
import Foundation
import QuallCaptureKit

// O processo que **é** o monitor da tela estendida.
//
// # Por que um processo, e não um objeto dentro do app
//
// Duas limitações do `CGVirtualDisplay`, medidas neste Mac (macOS 26.6.2) em processos isolados e
// com o run loop principal rodando — ver `docs/tela-estendida.md`:
//
//  1. **Um processo só consegue um monitor virtual na vida.** O segundo, criado 3 s depois de o
//     primeiro ser solto ou ao mesmo tempo que ele, nunca ganha modo de exibição.
//  2. **Depois de uma troca de modo, soltar não tira o monitor.** Ele fica online até o processo
//     sair — seja a troca feita por código ou, presumivelmente, pela pessoa em Ajustes > Monitores.
//
// Dentro do app, a primeira limitação faria a segunda sessão de tela estendida falhar, e a segunda
// deixaria um monitor fantasma depois de toda sessão em que o modo mudou. O que funcionou **em
// todas** as medidas foi o processo sair: de forma limpa ou com `kill -9`, o monitor some em ~40 a
// ~80 ms. Então o monitor mora aqui, e a vida dele é a vida deste processo.
//
// # O protocolo com o app
//
//     quall-monitor-virtual --largura=1920 --altura=1200 --hertz=60 --escala=2x --nome=...
//                           [--indice=N]    # identidade de mais de um monitor; ver `MonitorVirtual.serie`
//                           [--saida=LxA]   # 2x reduzido: o painel para onde a captura reduz
//
// - Sobe o monitor e escreve **uma** linha JSON no stdout: `{"display_id": N, "relato": "..."}`
//   quando deu certo, `{"erro": "..."}` quando não (e sai com 1).
// - Depois fica lendo o stdin. **Quando o stdin fecha, sai** — e o monitor sai junto. É o app que
//   fecha o stdin no fim da sessão; se o app morrer, o sistema fecha o stdin por ele, e não sobra
//   monitor fantasma nem quando quem transmitia caiu.
// - Não captura nada, não pede permissão nenhuma. Criar monitor não é conteúdo.

func escrever(_ objeto: [String: Any]) {
    guard let dados = try? JSONSerialization.data(withJSONObject: objeto, options: [.sortedKeys]),
          var linha = String(data: dados, encoding: .utf8) else { return }
    linha += "\n"
    FileHandle.standardOutput.write(linha.data(using: .utf8)!)
}

var largura = 1920
var altura = 1200
var hertz = 60
var escala = ModoDoMonitorVirtual.Escala.dobro
var nome = "Quall"
var indice = 0
var saida: (largura: Int, altura: Int)?
for argumento in CommandLine.arguments.dropFirst() {
    let partes = argumento.split(separator: "=", maxSplits: 1).map(String.init)
    guard partes.count == 2 else {
        escrever(["erro": "argumento sem valor: \(argumento)"])
        exit(2)
    }
    switch partes[0] {
    case "--largura": largura = Int(partes[1]) ?? 0
    case "--altura": altura = Int(partes[1]) ?? 0
    case "--hertz": hertz = Int(partes[1]) ?? 0
    case "--escala": escala = ModoDoMonitorVirtual.Escala(rawValue: partes[1]) ?? escala
    case "--nome": nome = partes[1]
    case "--indice": indice = max(0, Int(partes[1]) ?? 0)
    // O 2x reduzido (`ModoDoMonitorVirtual.paraTela`): os pixels do painel, para onde a captura reduz.
    case "--saida":
        let n = partes[1].lowercased().split(separator: "x").compactMap { Int($0) }
        guard n.count == 2 else { escrever(["erro": "--saida=\(partes[1]): use LxA"]); exit(2) }
        saida = (n[0], n[1])
    default:
        escrever(["erro": "argumento desconhecido: \(argumento)"])
        exit(2)
    }
}

let modo = ModoDoMonitorVirtual(larguraEmPixels: largura, alturaEmPixels: altura,
                                hertz: hertz, escala: escala, saida: saida)
let nomeFinal = nome
let indiceFinal = indice

final class Caixa: @unchecked Sendable { var monitor: MonitorVirtual?; var erro: String? }
let caixa = Caixa()
let pronto = DispatchSemaphore(value: 0)

Task.detached {
    defer { pronto.signal() }
    do {
        caixa.monitor = try await MonitorVirtual.criar(
            modo: modo, nome: nomeFinal, indice: indiceFinal,
            // O sistema derrubou o monitor por conta própria. Sair é a única resposta honesta: o
            // app vê o stdout fechar, e o vigia de monitor dele vê o monitor sumir.
            aoTerminar: {
                FileHandle.standardError.write("quall-monitor-virtual: o sistema encerrou o monitor\n".data(using: .utf8)!)
                exit(3)
            })
    } catch {
        caixa.erro = "\(error)"
    }
}
pronto.wait()

guard let monitor = caixa.monitor else {
    escrever(["erro": caixa.erro ?? "falha sem motivo"])
    exit(1)
}
escrever(["display_id": Int(monitor.displayID), "relato": monitor.relato])

// O stdin fecha quando o app fecha o cano — ou morre. Numa thread própria, porque a principal
// agora gira o run loop do vigia de espelho abaixo.
Thread.detachNewThread {
    _ = FileHandle.standardInput.readDataToEndOfFile()
    exit(0)
}

// **O vigia de espelho, a sessão inteira.** Medido em 10/09: o monitor nasceu certo e, 18 s depois,
// o Sidecar reaplicou "Espelhar Quall — tela estendida" — o iPad passou a espelhar este monitor, e a
// pessoa viu "espelhado o iPad". A fonte se chama "Tela estendida"; espelho que envolva este monitor
// é desfeito assim que aparece. O run loop gira para o CoreGraphics deste processo receber os avisos
// de reconfiguração; a verificação é por relógio, e não pelo aviso, para não depender de ele chegar.
let idVigiado = monitor.displayID
let relogio = Timer(timeInterval: 0.5, repeats: true) { _ in
    do {
        if let desfeito = try MonitorVirtual.desfazerEspelhoSeHouver(idVigiado) {
            FileHandle.standardError.write("quall-monitor-virtual: espelho desfeito (\(desfeito))\n".data(using: .utf8)!)
        }
    } catch {
        FileHandle.standardError.write("quall-monitor-virtual: não consegui desfazer o espelho: \(error)\n".data(using: .utf8)!)
    }
}
RunLoop.main.add(relogio, forMode: .default)
RunLoop.main.run()
