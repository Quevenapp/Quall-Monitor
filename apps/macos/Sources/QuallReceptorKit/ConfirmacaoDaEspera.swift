import Foundation

/// **O confirmador de uma espera, enquanto o PIN dela é o mesmo** (`docs/v3-no-privado.md` §3.4 e
/// P11; revisão da integração, baixo 2). O modelo é o do iOS (`ConfirmacaoDeCodigo.swift`).
///
/// As esperas do Mac são reabertas em laço com o mesmo PIN — o Espelhar depois de uma falha, a
/// câmera da R5 a cada queda, o prompter a cada volta — e o núcleo conta **no confirmador** as 3
/// tentativas do código e o bloqueio da escolha errada. Um confirmador novo a cada volta daria ao
/// atacante 3 tentativas por volta, e a escolha errada só "encerraria o código" até a próxima. Para a
/// pessoa, a espera é a do PIN que está na tela: o confirmador vive o mesmo tanto, e só é trocado quando
/// o PIN muda ou quando uma sessão sobe (a próxima espera é outra).
///
/// Genérico no confirmador para se testar sem o núcleo. Da thread de quem abre as esperas.
public final class ConfirmacaoDaEspera<C: AnyObject> {
    private let criar: () -> C?
    private var atual: (pin: String, confirmacao: C)?

    public init(criar: @escaping () -> C?) { self.criar = criar }

    /// O confirmador para esta volta: o mesmo da anterior se o PIN não mudou.
    public func para(pin: String) -> C? {
        if let atual, atual.pin == pin { return atual.confirmacao }
        guard let nova = criar() else { atual = nil; return nil }
        atual = (pin, nova)
        return nova
    }

    /// Uma sessão subiu (ou a espera acabou de vez): a próxima começa do zero.
    public func terminou() { atual = nil }
}
