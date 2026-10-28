import Foundation
import QuallIdiomaKit

/// **O Mac como anfitrião do código de 4 dígitos** (`docs/conexao.md` §4), a parte sem rede e sem `.a`.
///
/// Quem hospeda neste Mac são três: a espera do Espelhar (tela e câmera, `Emissor`), a espera da
/// câmera da tela R5 (`EsperaDaCamera`) e o prompter (`Teleprompter`). Na tela R5 os dois últimos
/// esperam **ao mesmo tempo**, cada um com o seu confirmador. A janela mostra um cartão só, por cima
/// de tudo, e o toque precisa voltar ao confirmador certo, com a tentativa do cartão (revisão de
/// 12/10, M2). Esta peça decide, a cada leitura, **qual** cartão desenhar e **que** desfecho dizer
/// — uma vez por tentativa, e não a cada volta do relógio.
public struct AcompanhamentoDoCodigo {
    /// O cartão a desenhar: de qual espera (`chave`) e o estado dela.
    public struct NaTela: Equatable, Sendable {
        public let chave: String
        public let estado: EstadoDoCodigo

        public init(chave: String, estado: EstadoDoCodigo) {
            self.chave = chave
            self.estado = estado
        }
    }

    /// O último desfecho dito, por espera: a mesma tentativa terminada não é dita duas vezes.
    private var ditos: [String: (tentativa: UInt32, fase: EstadoDoCodigo.Fase)] = [:]

    public init() {}

    /// `leituras`: cada espera viva, na ordem em que começou a ser acompanhada, com o estado lido
    /// agora (`nil` quando a leitura falhou). Devolve o cartão (ou nenhum) e as frases dos desfechos
    /// que acabaram de acontecer.
    ///
    /// **Mostrando ganha de confirmado aqui**: com duas esperas pedindo ao mesmo tempo, quem ainda
    /// precisa do toque fica à frente de quem só espera o outro lado.
    public mutating func ler(_ leituras: [(chave: String, estado: EstadoDoCodigo?)])
        -> (naTela: NaTela?, desfechos: [String]) {
        var desfechos: [String] = []
        var vivas = Set<String>()
        for (chave, estado) in leituras {
            vivas.insert(chave)
            guard let estado, let frase = estado.desfecho else { continue }
            if let d = ditos[chave], d.tentativa == estado.tentativa, d.fase == estado.fase { continue }
            ditos[chave] = (estado.tentativa, estado.fase)
            desfechos.append(frase)
        }
        // Uma espera que saiu leva junto o que foi dito dela: a próxima com a mesma chave é outra.
        ditos = ditos.filter { vivas.contains($0.key) }

        let comEstado = leituras.compactMap { l in l.estado.map { NaTela(chave: l.chave, estado: $0) } }
        let naTela = comEstado.first { $0.estado.fase == .mostrando }
            ?? comEstado.first { $0.estado.fase == .confirmadoAqui }
        return (naTela, desfechos)
    }
}
