import Foundation
import QuallIdiomaKit

/// **O QR invertido, do lado do Mac que recebe** (`docs/conexao.md` §10): o estado do convite, do
/// JSON de `quall_qr_invite_state_json`, e as frases da tela. Sem rede e sem `.a`, como o resto deste
/// alvo. O texto do QR não passa por aqui: ele leva o segredo, e vai do núcleo direto para o desenho.
public struct EstadoDoConvite: Equatable, Sendable {
    public enum Fase: String, Sendable {
        /// O QR está na tela, esperando o celular ler e chamar.
        case esperando
        /// Um celular chamou: tire o QR e diga "Conectando a …".
        case chamado
        /// Passaram os 3 min sem chamado: troque por um QR novo.
        case expirou
    }

    public let fase: Fase
    public let expiraEmMs: Int
    public let nomeDoPar: String?
    /// O `device_id` que o chamado selado disse (`peer_id`): com ele, um `ALREADY_PAIRED` pode oferecer
    /// esquecer **este** par (revisão da integração, baixo 8).
    public let idDoPar: String?
    /// `ip:porta` em que o Mac vai conectar, depois do chamado.
    public let endpoint: String?

    public init(fase: Fase, expiraEmMs: Int, nomeDoPar: String?, endpoint: String?, idDoPar: String? = nil) {
        self.idDoPar = idDoPar
        self.fase = fase
        self.expiraEmMs = expiraEmMs
        self.nomeDoPar = nomeDoPar
        self.endpoint = endpoint
    }

    public static func doJSON(_ texto: String) -> EstadoDoConvite? {
        guard let dados = texto.data(using: .utf8),
              let j = (try? JSONSerialization.jsonObject(with: dados)) as? [String: Any],
              let fase = (j["phase"] as? String).flatMap(Fase.init(rawValue:)) else { return nil }
        let nome = (j["peer_name"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        return EstadoDoConvite(fase: fase, expiraEmMs: (j["expires_in_ms"] as? NSNumber)?.intValue ?? 0,
                               nomeDoPar: nome, endpoint: j["endpoint"] as? String,
                               idDoPar: (j["peer_id"] as? String).flatMap { $0.isEmpty ? nil : $0 })
    }

    /// A frase debaixo do QR: quanto falta, ou quem chamou.
    public var frase: String {
        switch fase {
        case .esperando:
            let s = max(0, expiraEmMs / 1000)
            return T("Vale por mais %@", String(format: "%d:%02d", s / 60, s % 60))
        case .chamado:
            return T("Conectando a %@…", nomeDoPar ?? T("outro aparelho"))
        case .expirou:
            return T("O QR expirou. Gerando outro…")
        }
    }
}

/// **Quantos convites seguidos o Exibir mostra** antes de parar sozinho (3 min cada: 15 min), como o
/// OBS (`QR_RENOVACOES`). Sem teto, um Exibir esquecido aberto — a janela escondida, o Mac noutra
/// coisa — manteria a escuta do chamado (TCP 7874) aberta para sempre (revisão de 12/10).
public enum TetoDoConvite {
    public static let convites = 5

    /// `usados` convites já foram mostrados nesta espera: pode vir mais um?
    public static func podeMostrarMaisUm(usados: Int) -> Bool { usados < convites }
}

/// **Por que a espera pelo QR não fechou**, separado pelo estado do convite (`docs/conexao.md` §10.9):
/// `TIMEOUT` tem dois sentidos — o QR expirou sem ninguém ler (troca-se em silêncio), ou o celular
/// chamou e a conexão não fechou (isso se diz).
public enum DesfechoDoConvite: Equatable, Sendable {
    /// Um QR novo, sem frase: ninguém leu este a tempo.
    case renovar
    /// Um QR novo, com a frase do que deu errado.
    case renovarDizendo(String)

    /// `status` é o nome do `QuallStatus` (`NucleoReceptor.nome`), para esta peça não depender do `.a`.
    public static func de(status: String, fase: EstadoDoConvite.Fase?) -> DesfechoDoConvite {
        switch status {
        case "TIMEOUT":
            return fase == .chamado ? .renovarDizendo(T("O celular leu o QR, mas a conexão não fechou. Leia o QR novo."))
                : .renovar
        // O QR não substitui um vínculo que já existe (`docs/v3-no-privado.md` §8.2–§8.3).
        case "ALREADY_PAIRED":
                return .renovarDizendo(T("Este Mac ou o celular ainda guarda um pareamento antigo com o outro, e o QR não o substitui. Conecte pela lista, ou esqueça o pareamento no aparelho que ainda o guarda e leia o QR novo."))
        case "PAIRING":
            return .renovarDizendo(T("Quem atendeu não é o celular que leu o QR. Leia o QR novo."))
        case "NO_ROUTE":
            return .renovarDizendo(T("O celular chamou, mas este Mac não o alcançou na rede."))
        default:
            return .renovarDizendo(T("A conexão pelo QR não fechou (%@). Leia o QR novo.", status))
        }
    }
}
