import Foundation
import QuallIdiomaKit

/// **A lista do Exibir, sem rede e sem `.a`** (`docs/conexao.md` §2 e §4): o que se lê do JSON de
/// `quall_browser_devices_json`, a frase "visto há X s", a dica de quando nada aparece, o estado da
/// confirmação por código e a sessão salva por identidade. Mora aqui, e não em `QuallNetKit`, pela
/// mesma decisão do resto deste alvo: o que é lógica de tela se testa sem o núcleo existir.
///
/// **No v3** (`docs/v3-no-privado.md` §5.3–§5.4) o anúncio não leva id nem nome: `deviceId` é
/// `discovery-<token>` e `nome` é o alias `Quall 1a2b3c4d`, o mesmo que a espera do outro aparelho
/// mostra. Nenhum dos dois serve para guardar pareamento nem sessão — isso só com o id da sessão
/// autenticada (`quall_session_peer_json`).
public struct AparelhoNaRede: Equatable, Identifiable, Sendable {
    /// `discovery-<token>`: muda a cada espera do outro lado. Não é identidade.
    public let deviceId: String
    /// O alias da rede (`Quall 1a2b3c4d`).
    public let nome: String
    /// `"ip:porta"`. Nunca vazio: sem endereço utilizável o item não entra.
    public let endpoint: String
    /// Visto há no máximo 12 s. Falso é "visto há X s" — o item fica mais 30 s e sai sozinho.
    public let vivo: Bool
    /// Há quantos **segundos** veio o último sinal (arredondado, para a lista não mudar a cada volta).
    public let vistoHaS: Int
    /// Aceita a confirmação por código: tocar "Conectar" basta, sem PIN.
    public let aceitaCodigo: Bool
    /// **O par guardado que este item é** (`known_device_id`, `docs/v3-no-privado.md` §5.4): a sonda
    /// do núcleo achou a dica dele no `Challenge`. Pista, não prova — tocar conecta **com o alvo**
    /// (`quall_connect_known`), e a retomada é que decide; sem a dica, falha fechado (A2).
    public let idConhecido: String?
    /// O alias da rede, guardado quando `nome` passou a ser o nome local do par ou o anunciado.
    public let alias: String
    /// O nome que o aparelho anuncia (`announced_name`, `docs/v3-no-privado.md` §8.7). Não autentica
    /// nada: quando é o título, o alias vai embaixo (`aliasEmbaixo`).
    public let nomeAnunciado: String?
    /// O token do anúncio muda a cada espera e não é identidade (`docs/v3-no-privado.md` §5.3): a
    /// lista é chaveada por token, IP e porta, e a linha também.
    public var id: String { "\(deviceId)@\(endpoint)" }

    public init(deviceId: String, nome: String, endpoint: String, vivo: Bool, vistoHaS: Int, aceitaCodigo: Bool,
                idConhecido: String? = nil, alias: String? = nil, nomeAnunciado: String? = nil) {
        self.deviceId = deviceId
        self.nome = nome
        self.idConhecido = idConhecido
        self.alias = alias ?? nome
        self.nomeAnunciado = nomeAnunciado
        self.endpoint = endpoint
        self.vivo = vivo
        self.vistoHaS = vistoHaS
        self.aceitaCodigo = aceitaCodigo
    }

    /// **Os emissores de vídeo** do JSON: quem anuncia tela ou câmera, **sem papel** (um
    /// teleprompter não é para o Exibir, §7 do contrato), com endereço, e nunca este aparelho. As
    /// chaves novas (`alive`, `seen_ms_ago`, `accepts_code`) faltando — um núcleo anterior — valem
    /// "vivo", "agora" e "não aceita".
    ///
    /// `own` verdadeiro é um anúncio deste mesmo processo: não entra. `nomesLocais` é o nome que este
    /// Mac guardou de cada par (`device_id` → nome, gravado depois de cada sessão autenticada).
    ///
    /// **O título da linha** (`docs/v3-no-privado.md` §8.7): o nome guardado do par reconhecido >
    /// o nome anunciado (`announced_name`, com o alias embaixo) > o alias.
    public static func emissores(doJSON texto: String, excluindo meuId: String,
                                 nomesLocais: [String: String] = [:]) -> [AparelhoNaRede] {
        guard let dados = texto.data(using: .utf8),
              let itens = (try? JSONSerialization.jsonObject(with: dados)) as? [[String: Any]] else { return [] }
        var saida: [AparelhoNaRede] = []
        for item in itens {
            guard item["papel"] == nil, (item["own"] as? Bool) != true,
                  let id = item["device_id"] as? String, id != meuId,
                  let endpoint = item["endpoint"] as? String, !endpoint.isEmpty else { continue }
            let caps = item["capabilities"] as? [String: Any] ?? [:]
            guard (caps["screen_source"] as? Bool) == true || (caps["camera_source"] as? Bool) == true else { continue }
            let alias = (item["display_name"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? endpoint
            let conhecido = (item["known_device_id"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            let anunciado = (item["announced_name"] as? String)
                .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                .flatMap { $0.isEmpty ? nil : $0 }
            let nome = conhecido.flatMap { nomesLocais[$0] }.flatMap { $0.isEmpty ? nil : $0 } ?? anunciado ?? alias
            let ms = (item["seen_ms_ago"] as? NSNumber)?.intValue ?? 0
            saida.append(AparelhoNaRede(deviceId: id, nome: nome, endpoint: endpoint,
                                        vivo: (item["alive"] as? Bool) ?? true,
                                        vistoHaS: ms / 1000,
                                        aceitaCodigo: (item["accepts_code"] as? Bool) ?? false,
                                        idConhecido: conhecido, alias: alias, nomeAnunciado: anunciado))
        }
        // Vivos primeiro; dentro de cada grupo, por nome. A ordem não depende da volta do navegador.
        return saida.sorted {
            if $0.vivo != $1.vivo { return $0.vivo }
            return $0.nome.localizedCaseInsensitiveCompare($1.nome) == .orderedAscending
        }
    }

    /// A linha de baixo do item que a sonda reconheceu (§5.4): "pareado", e o alias da rede quando o
    /// título já é o nome local ("pareado · Quall 1a2b3c4d"). `nil` para os outros.
    public var rotuloDoPareado: String? {
        guard idConhecido != nil else { return nil }
        return nome != alias ? T("pareado · %@", alias) : T("pareado")
    }

    /// **O alias pequeno embaixo do nome anunciado** (§8.7): o nome que vem da rede não autentica nada,
    /// e o alias é o que a tela de espera do outro aparelho mostra para conferir. `nil` quando o título
    /// já é o alias, e no item reconhecido (lá a linha diz `rotuloDoPareado`).
    public var aliasEmbaixo: String? {
        idConhecido == nil && nome != alias ? alias : nil
    }

    /// "visto há 14 s", só quando o aparelho deixou de ser visto.
    public var rotuloDoVisto: String? {
        vivo ? nil : T("visto há %@ s", vistoHaS)
    }
}

/// **Por que a lista pode estar vazia**, do JSON de `quall_browser_hint_json` (`docs/conexao.md` §2.5).
public struct DicaDaBusca: Equatable, Sendable {
    public let causas: [String]
    public let redes: [String]

    public init(causas: [String], redes: [String]) {
        self.causas = causas
        self.redes = redes
    }

    public static func doJSON(_ texto: String) -> DicaDaBusca? {
        guard let dados = texto.data(using: .utf8),
              let j = (try? JSONSerialization.jsonObject(with: dados)) as? [String: Any] else { return nil }
        return DicaDaBusca(causas: j["causes"] as? [String] ?? [], redes: j["networks"] as? [String] ?? [])
    }

    /// A frase da tela, a causa mais provável primeiro, e a rede deste Mac no fim — é olhando as duas
    /// telas que se vê o 2,4 GHz contra o 5 GHz em sub-redes separadas. Vazio é "ainda procurando".
    public var texto: String {
        var partes: [String] = []
        for causa in causas {
            switch causa {
            case "sem_rede_local":
                partes.append(T("Este Mac não está numa rede local. Ligue o Wi-Fi ou o cabo na mesma rede do outro aparelho."))
            case "sub_rede_diferente":
                partes.append(T("O aparelho salvo estava noutra rede. Os dois precisam estar na mesma rede (o 2,4 GHz e o 5 GHz do roteador às vezes são redes separadas)."))
            case "firewall":
                partes.append(T("Um firewall pode estar barrando a busca. Permita o Quall nas redes privadas e públicas."))
            case "isolamento":
                partes.append(T("Ninguém respondeu na rede. Redes de convidados e roteadores com isolamento escondem os aparelhos uns dos outros."))
            default:
                continue
            }
        }
        if !partes.isEmpty, let rede = redes.first {
            partes.append(T("Este Mac está na rede %@.", rede))
        }
        return partes.joined(separator: " ")
    }
}

/// **A confirmação por código**, do JSON de `quall_code_confirm_state_json` (`docs/conexao.md` §4).
public struct EstadoDoCodigo: Equatable, Sendable {
    public enum Fase: String, Sendable {
        case esperando, mostrando, confirmadoAqui = "confirmado_aqui", conferiu
        case recusadoAqui = "recusado_aqui", recusadoLa = "recusado_la", naoConferiu = "nao_conferiu", expirou
    }

    public let fase: Fase
    public let codigo: String?
    public let par: String?
    /// A tentativa a que este código pertence (`"attempts"`): o toque vai com ela, e um toque dado no
    /// cartão de uma tentativa que já caiu não confirma a seguinte (revisão de 12/10, M2).
    public let tentativa: UInt32
    /// **Anfitrião** (`docs/v3-no-privado.md` §3.6): os três números, na ordem do núcleo — sorteada a
    /// cada tentativa e estável enquanto o cartão está aberto. Vazio no convidado. O anfitrião **não**
    /// recebe o código (`codigo` é `nil`): destacar o certo transformaria a escolha num "Confirmar".
    public let opcoes: [String]
    /// A pessoa tocou um número que não era o do outro aparelho.
    public let escolhaErrada: Bool
    /// O código desta espera acabou (P11): depois de uma escolha errada, só o PIN serve.
    public let codigoBloqueado: Bool
    /// Por que a troca caiu, quando o núcleo sabe (`"refusal"`, §8.3): `"already_paired"` é o vínculo
    /// que já existe (B2), nos dois lados.
    public let recusa: String?
    /// O aviso do toque (`"notice"`, `docs/v3-no-privado.md` §8.6): `"previous_pairing_invalid"` quando a
    /// reconexão não foi reconhecida e este código é para parear de novo.
    public let aviso: String?

    /// O cartão do convidado diz que o pareamento anterior não vale mais (§8.6).
    public var pareamentoAnteriorInvalido: Bool { aviso == "previous_pairing_invalid" }

    public init(fase: Fase, codigo: String?, par: String?, tentativa: UInt32 = 0,
                opcoes: [String] = [], escolhaErrada: Bool = false, codigoBloqueado: Bool = false,
                recusa: String? = nil, aviso: String? = nil) {
        self.recusa = recusa
        self.aviso = aviso
        self.tentativa = tentativa
        self.fase = fase
        self.codigo = codigo
        self.par = par
        self.opcoes = opcoes
        self.escolhaErrada = escolhaErrada
        self.codigoBloqueado = codigoBloqueado
    }

    public static func doJSON(_ texto: String) -> EstadoDoCodigo? {
        guard let dados = texto.data(using: .utf8),
              let j = (try? JSONSerialization.jsonObject(with: dados)) as? [String: Any],
              let fase = (j["phase"] as? String).flatMap(Fase.init(rawValue:)) else { return nil }
        return EstadoDoCodigo(fase: fase, codigo: j["code"] as? String, par: j["peer"] as? String,
                              tentativa: (j["attempts"] as? NSNumber)?.uint32Value ?? 0,
                              opcoes: (j["choices"] as? [String]) ?? [],
                              escolhaErrada: (j["wrong_choice"] as? Bool) ?? false,
                              codigoBloqueado: (j["code_blocked"] as? Bool) ?? false,
                              recusa: j["refusal"] as? String,
                              aviso: j["notice"] as? String)
    }

    /// O cartão é o do anfitrião (escolher entre três), e não o do convidado (mostrar o código).
    public var escolhaEntreTres: Bool { !opcoes.isEmpty }

    /// O que dizer quando a tentativa pelo código não fechou; `nil` nas outras fases.
    public var desfecho: String? {
        if recusa == "already_paired" { return RecusaDoVinculo.jaPareado.frase }
        // **O outro não aceita o código** (bancada de 13/10): ninguém recusou nada; o PIN de lá.
        if recusa == "code_not_accepted" { return T("Este aparelho não aceita o código. Digite o PIN que aparece na tela dele.") }
        if escolhaErrada || (codigoBloqueado && fase == .recusadoAqui) {
            return T("Esse não era o número do outro aparelho. O código ficou desligado nesta espera; o PIN continua valendo.")
        }
        switch fase {
        case .recusadoAqui: return T("Você recusou o código.")
        case .recusadoLa: return T("O outro aparelho recusou o código, ou desistiu.")
        case .naoConferiu: return T("Os códigos não conferiram. Tente de novo; se repetir, alguém pode estar no meio da rede.")
        case .expirou: return T("O código expirou sem confirmação.")
        default: return nil
        }
    }

    /// O código com um espaço no meio, como as duas telas mostram: "04 27".
    public var codigoEmGrupos: String { EstadoDoCodigo.emGrupos(codigo ?? "") }

    /// "0427" → "04 27". Outro tamanho volta como veio.
    public static func emGrupos(_ c: String) -> String {
        guard c.count == 4 else { return c }
        return String(c.prefix(2)) + " " + String(c.suffix(2))
    }

    /// **Para o leitor de tela, dígito a dígito**: "0427" → "0, 4, 2, 7". Lido como número, "0427"
    /// vira "quatrocentos e vinte e sete" — e o zero da frente some.
    public static func paraOLeitor(_ c: String) -> String {
        c.map(String.init).joined(separator: ", ")
    }
}

/// **A frase de um toque em Conectar que não subiu**, quando ela é do código ou do vínculo, e não do
/// status de rede (`docs/v3-no-privado.md` §8.5 e §8.6). `status` é o nome do `QuallStatus`; `estado`, o
/// confirmador do toque no fim. `nil`: a casca diz a frase do status, como sempre.
///
/// A ordem é a do Exibir (`Receptor.explicarFalhaDeConexao`): as recusas do vínculo; o desfecho do
/// código que entrou em cena (inclusive "não aceita o código"); e o `NEEDS_PIN` com o código à mão que
/// não chegou a aparecer — o outro lado não aceita o código, e o PIN dele resolve.
public enum FraseDoToque {
    public static func de(status: String, estado: EstadoDoCodigo?) -> String? {
        if let recusa = RecusaDoVinculo.de(status: status) { return recusa.frase }
        guard let estado else { return nil }
        if let frase = estado.desfecho { return frase }
        if status == "NEEDS_PIN", estado.fase == .esperando {
            return T("Este aparelho não aceita o código. Digite o PIN que aparece na tela dele.")
        }
        return nil
    }
}

/// **Por que uma conexão sem PIN parou antes de começar ou no fim do código**, a parte que a tela
/// precisa separar (`docs/v3-no-privado.md` §3.4, §3.5, §8.1 e §8.3). Cada uma tem status próprio na
/// fronteira desde §8.3: `INTERFERENCE` (17), `PAIR_NOT_RECOGNIZED` (18), `ALREADY_PAIRED` (19).
public enum RecusaDoVinculo: Equatable, Sendable {
    /// A1: pernas demais caíram antes de o código chegar à tela. "Use o PIN."
    case interferencia
    /// A2: há vínculo com o alvo e o outro lado não mostrou a dica dele. Falha fechado: nem código
    /// nem QR; a saída é "Esquecer este aparelho" (ou um PIN).
    case naoReconheceu
    /// B2: o código (ou o QR) chegou ao fim, mas um dos dois lados já tem vínculo com o outro.
    case jaPareado

    /// `status`: o nome do `QuallStatus` (`NucleoReceptor.nome`), para esta peça não depender do `.a`.
    public static func de(status: String) -> RecusaDoVinculo? {
        switch status {
        case "INTERFERENCE": return .interferencia
        case "PAIR_NOT_RECOGNIZED": return .naoReconheceu
        case "ALREADY_PAIRED": return .jaPareado
        default: return nil
        }
    }

    /// **B2 com o par conhecido** (revisão da integração, baixo 8): a frase que oferece esquecer aquele
    /// par aqui. `nome` é o que a tela sabe dele (o nome do chamado, ou o guardado).
    public static func jaPareadoCom(_ nome: String) -> String {
        T("Já há um pareamento com %@ (neste Mac ou no outro aparelho), e o código ou o QR não o substitui. Toque em Esquecer este aparelho para tirá-lo deste Mac, ou use o PIN.", nome)
    }

    public var frase: String {
        switch self {
        case .interferencia:
            return T("Algo está interferindo na conexão com este aparelho. Use o PIN.")
        case .naoReconheceu:
            return T("O outro aparelho não reconheceu este pareamento. Para parear de novo, toque em Esquecer este aparelho, ou digite o PIN.")
        case .jaPareado:
            return T("Um dos dois aparelhos ainda guarda um pareamento antigo com o outro, e o código não o substitui. Esqueça o pareamento no aparelho que ainda o guarda, ou use o PIN.")
        }
    }
}

/// **Onde a sessão do convidado conectou**, do JSON de `quall_session_endpoint_json`
/// (`docs/v3-no-privado.md` §5.2): `{"endpoint":"192.168.15.9:7877","via":"udp"}`. `via` só vem da
/// sessão salva (`quall_connect_known`).
public struct EnderecoDaSessao: Equatable, Sendable {
    public let endpoint: String
    public let via: String?

    public init(endpoint: String, via: String?) {
        self.endpoint = endpoint
        self.via = via
    }

    public static func doJSON(_ texto: String) -> EnderecoDaSessao? {
        guard let dados = texto.data(using: .utf8),
              let j = (try? JSONSerialization.jsonObject(with: dados)) as? [String: Any],
              let e = j["endpoint"] as? String, !e.isEmpty else { return nil }
        return EnderecoDaSessao(endpoint: e, via: j["via"] as? String)
    }
}

/// **A sessão salva, por identidade** (`docs/conexao.md` §3): quem é o outro aparelho, e o último
/// endereço só como dica. O "token" é o segredo do par, que já fica no `pares.json` e nunca viaja.
public struct SessaoSalva: Codable, Equatable, Sendable {
    public var deviceId: String
    public var nome: String
    public var endereco: String

    public init(deviceId: String, nome: String, endereco: String) {
        self.deviceId = deviceId
        self.nome = nome
        self.endereco = endereco
    }

    /// Lê a sessão salva. Aceita também o formato de antes — só o endereço, sem identidade —, que
    /// continua servindo como endereço (sem a procura pelo id).
    public static func ler(json: String?, enderecoAntigo: String?) -> SessaoSalva? {
        if let json, let dados = json.data(using: .utf8),
           let s = try? JSONDecoder().decode(SessaoSalva.self, from: dados), !s.endereco.isEmpty {
            return s
        }
        guard let e = enderecoAntigo, !e.isEmpty else { return nil }
        return SessaoSalva(deviceId: "", nome: "", endereco: e)
    }

    public var json: String {
        (try? JSONEncoder().encode(self)).flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
    }

    /// **A sessão a gravar depois que a conexão fechou** (revisão de 12/10, M3): o id é o que a
    /// sessão **autenticada** diz (`quall_session_peer_json`), e o endereço é o que de fato conectou.
    /// É a única porta de escrita: a resposta da procura pelo id não grava nada, porque qualquer um na
    /// LAN responde com um id. Sem id do par (núcleo antigo), mantém o id que havia se o endereço é
    /// o mesmo; senão grava sem id — e a próxima vez não procura.
    public static func depoisDeConectar(antes: SessaoSalva?, idDoPar: String, nome: String,
                                        endereco: String) -> SessaoSalva {
        let id = !idDoPar.isEmpty ? idDoPar : (antes?.endereco == endereco ? antes?.deviceId ?? "" : "")
        return SessaoSalva(deviceId: id, nome: nome, endereco: endereco)
    }

    /// A procura pelo id só vale para quem tem id e para o endereço que a sessão salvou.
    public func procuraPeloId(para endereco: String) -> Bool {
        !deviceId.isEmpty && endereco == self.endereco
    }

    /// **Depois de "Esquecer este aparelho"** (`docs/v3-no-privado.md` §3.5): o endereço fica como
    /// conveniência de digitação, mas sem o id a próxima conexão não é mais retomada — é PIN ou código.
    public var semIdentidade: SessaoSalva { SessaoSalva(deviceId: "", nome: "", endereco: endereco) }
}
