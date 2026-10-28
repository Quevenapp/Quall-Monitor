import CQuall
import Foundation
import QuallReceptorKit

/// **A ponte da conexão sem dor** (`docs/conexao.md`): a lista do Exibir pelos três caminhos do
/// núcleo, a confirmação por código e o QR invertido. A sessão salva (`quall_connect_known`) mora em
/// `NucleoReceptor.conectarConhecido`, porque devolve a sessão, e não um endereço
/// (`docs/v3-no-privado.md` §5.2).

// MARK: - a lista do Exibir

/// **Os emissores de vídeo na rede**, no molde do `NavegadorDePrompters`: o `QuallBrowser*` é da
/// thread do navegador, e só dela; a interface só pede para parar.
public final class NavegadorDeAparelhos: @unchecked Sendable {
    private let trava = NSLock()
    private var _parar = false
    private var rodando = false
    private var tratador: (([AparelhoNaRede], DicaDaBusca?) -> Void)?
    /// Os pares a dar ao navegador (`quall_browser_set_known`), quando mudaram, e os nomes locais.
    private var conhecidosPendentes: (meuId: String, nome: String, pares: String)?
    private var nomesLocais: [String: String] = [:]
    private var dicasPendentes: [String] = []
    private var varrerDeNovo = false

    public init() {}

    /// **"Procurar de novo"** (`quall_browser_rescan`, `docs/v3-no-privado.md` §8.9): a varredura da /24
    /// recomeça na próxima volta da thread, com a lista cheia ou não. Só pelo botão.
    public func procurarDeNovo() {
        trava.lock(); varrerDeNovo = true; trava.unlock()
    }

    /// **As dicas** (`quall_browser_add_hint`, `docs/v3-no-privado.md` §8.9): os últimos endereços
    /// conhecidos (`"ip:porta"`), que a busca pergunta poucas vezes enquanto não estão na lista — no
    /// lugar de varrer a /24 inteira. A thread do navegador as entrega na próxima volta.
    public func darDicas(_ enderecos: [String]) {
        let validos = enderecos.map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        guard !validos.isEmpty else { return }
        trava.lock()
        dicasPendentes.append(contentsOf: validos)
        trava.unlock()
    }

    /// **Os pares conhecidos, para a lista dizer quem é quem** (`docs/v3-no-privado.md` §5.4): a thread
    /// do navegador os entrega ao núcleo (`quall_browser_set_known`) na próxima volta. Chame de novo ao
    /// parear e ao esquecer. `nomes`: `device_id` → nome guardado neste Mac.
    public func definirConhecidos(meuId: String, nome: String, pares: String, nomes: [String: String]) {
        trava.lock()
        conhecidosPendentes = (meuId, nome, pares)
        nomesLocais = nomes
        trava.unlock()
    }

    /// `aoAtualizar` roda na thread do navegador, quando a lista ou a dica mudam (a volta é de ~0,4 s).
    public func comecar(excluindo meuId: String, aoAtualizar: @escaping ([AparelhoNaRede], DicaDaBusca?) -> Void) {
        trava.lock()
        tratador = aoAtualizar
        _parar = false
        guard !rodando else { trava.unlock(); return }
        rodando = true
        trava.unlock()

        let t = Thread { [self] in
            guard let b = quall_browser_start() else {
                trava.lock(); rodando = false; let avisar = tratador; trava.unlock()
                avisar?([], nil)
                return
            }
            var anterior: ([AparelhoNaRede], DicaDaBusca?)?
            while true {
                trava.lock()
                if _parar { rodando = false; trava.unlock(); break }
                let avisar = tratador
                let conhecidos = conhecidosPendentes
                conhecidosPendentes = nil
                let nomes = nomesLocais
                let dicas = dicasPendentes
                dicasPendentes = []
                let varrer = varrerDeNovo
                varrerDeNovo = false
                trava.unlock()
                if let conhecidos { NavegadorDeAparelhos.darConhecidos(b, conhecidos) }
                NavegadorDeAparelhos.darDicas(b, dicas)
                if varrer { _ = quall_browser_rescan(b) }
                _ = quall_browser_collect(b, 400)
                let json = lerAteCaber { quall_browser_devices_json(b, $0, $1) } ?? "[]"
                let lista = AparelhoNaRede.emissores(doJSON: json, excluindo: meuId, nomesLocais: nomes)
                let dica = lerAteCaber { quall_browser_hint_json(b, $0, $1) }
                    .flatMap(DicaDaBusca.doJSON)
                if anterior.map({ $0.0 != lista || $0.1 != dica }) ?? true {
                    anterior = (lista, dica)
                    avisar?(lista, dica)
                }
            }
            quall_browser_stop(b)
        }
        t.name = "quall.exibir.navegador"
        t.start()
    }

    public func parar() {
        trava.lock(); _parar = true; tratador = nil; trava.unlock()
    }

    /// `quall_browser_add_hint`, uma por endereço. Na thread do navegador.
    private static func darDicas(_ b: OpaquePointer, _ enderecos: [String]) {
        for e in enderecos {
            if e.withCString({ quall_browser_add_hint(b, $0) }) != 0 {
                NSLog("quall: quall_browser_add_hint falhou: status %d", quall_last_status().rawValue)
            }
        }
    }

    /// `quall_browser_set_known`: só `me` e `known_peers_json` são lidos. Na thread do navegador.
    private static func darConhecidos(_ b: OpaquePointer, _ c: (meuId: String, nome: String, pares: String)) {
        let r: Int32 = c.meuId.withCString { id in
            c.nome.withCString { n in
                c.pares.withCString { pares in
                    var opcoes = QuallSessionOptions(
                        me: QuallDeviceDesc(device_id: id, display_name: n,
                                            screen_source: false, camera_source: false, sink: true),
                        pin: nil, known_peers_json: pares, signaling_port: 0, timeout_ms: 0,
                        tracks: nil, track_count: 0, bind_address: nil)
                    return withUnsafePointer(to: &opcoes) { quall_browser_set_known(b, $0, nil) }
                }
            }
        }
        if r != 0 { NSLog("quall: quall_browser_set_known falhou: status %d", quall_last_status().rawValue) }
    }
}

// MARK: - a confirmação por código

/// O `QuallCodeConfirm*` de uma espera ou de uma conexão. A tela lê `estado` e chama
/// `aceitar`/`recusar`; o núcleo segura a própria cópia, então soltar isto depois da chamada é seguro.
public final class ConfirmacaoDeCodigo: @unchecked Sendable {
    let ponteiro: OpaquePointer

    public init?() {
        guard let p = quall_code_confirm_new() else { return nil }
        ponteiro = p
    }

    deinit { quall_code_confirm_free(ponteiro) }

    public var estado: EstadoDoCodigo? {
        lerAteCaber { quall_code_confirm_state_json(ponteiro, $0, $1) }
            .flatMap(EstadoDoCodigo.doJSON)
    }

    /// O toque vai com a tentativa do cartão que a pessoa via; outra tentativa não é confirmada (M2).
    /// **Só no convidado**: no anfitrião, aceitar não confirma mais — é preciso `escolher`.
    @discardableResult public func aceitar(tentativa: UInt32) -> Bool { quall_code_confirm_accept(ponteiro, tentativa) }
    @discardableResult public func recusar(tentativa: UInt32) -> Bool { quall_code_confirm_reject(ponteiro, tentativa) }
    /// **Anfitrião** (`docs/v3-no-privado.md` §3.6): a pessoa tocou o número `indice` de
    /// `estado.opcoes`. O certo confirma; um falso recusa e encerra o código desta espera (P11).
    @discardableResult public func escolher(tentativa: UInt32, indice: Int) -> Bool {
        guard indice >= 0, let i = UInt32(exactly: indice) else { return false }
        return quall_code_confirm_choose(ponteiro, tentativa, i)
    }
}

// MARK: - o QR invertido (`docs/conexao.md` §10)

/// **O convite que este Mac mostra como QR** para o celular ler (`quall_qr_invite_*`). Vale 3 min e um
/// chamado. O núcleo abre a escuta do chamado (TCP 7874) ao criar e a fecha ao liberar.
///
/// Dono de quem o cria (a thread da espera); a tela o lê ao mesmo tempo (`estado`, `texto`), o que a
/// fronteira autoriza. Liberado no `deinit`, quando ninguém mais o segura — a espera já voltou.
public final class ConviteQr: @unchecked Sendable {
    let ponteiro: OpaquePointer

    /// `nil` com o status do núcleo (`NO_ROUTE`: este Mac não está em rede local nenhuma).
    public init?(deviceId: String, nome: String, prazoMs: UInt32 = 0) {
        let p: OpaquePointer? = deviceId.withCString { id in
            nome.withCString { n in
                var eu = QuallDeviceDesc(device_id: id, display_name: n,
                                         screen_source: false, camera_source: false, sink: true)
                return withUnsafePointer(to: &eu) { quall_qr_invite_new($0, prazoMs) }
            }
        }
        guard let p else { return nil }
        ponteiro = p
    }

    deinit { quall_qr_invite_free(ponteiro) }

    /// **Leva o segredo** (§10.2): vai para o desenho do QR e para mais nada — nem diário, nem área de
    /// transferência.
    public var texto: String? {
        lerAteCaber { quall_qr_invite_text(ponteiro, $0, $1) }
    }

    public var estado: EstadoDoConvite? {
        lerAteCaber { quall_qr_invite_state_json(ponteiro, $0, $1) }
            .flatMap(EstadoDoConvite.doJSON)
    }
}

/// Uma sessão que subiu pelo QR e ainda não tem dono: a espera a entrega à tela, que a passa ao
/// `NucleoReceptor` (`adotar`) ou, se a tela já foi para outro lado, a fecha (`deinit`).
public final class SessaoDoConvite: @unchecked Sendable {
    private let trava = NSLock()
    private var ponteiro: OpaquePointer?

    init(_ p: OpaquePointer) { ponteiro = p }

    func tomar() -> OpaquePointer? {
        trava.lock(); defer { trava.unlock() }
        let p = ponteiro
        ponteiro = nil
        return p
    }

    deinit { if let p = tomar() { quall_session_close(p) } }
}

public enum EsperaPeloConvite {
    /// **Bloqueia** até um celular chamar e a sessão subir, o convite expirar ou o cancelador disparar
    /// (`quall_connect_with_invite`). O pareamento novo sai da sessão como sempre
    /// (`quall_session_known_peers_json`), e quem a adota grava. Falha: o nome do status
    /// (`NucleoReceptor.nome`) e o motivo.
    public static func esperar(_ convite: ConviteQr, deviceId: String, nome: String, paresConhecidos: String?,
                               prazoMs: UInt32, cancelador: Cancelador)
        -> Result<SessaoDoConvite, FalhaDoConvite> {
        let nova: OpaquePointer? = deviceId.withCString { id in
            nome.withCString { n in
                comOpcionalC(paresConhecidos) { pares in
                    var opcoes = QuallSessionOptions(
                        me: QuallDeviceDesc(device_id: id, display_name: n,
                                            screen_source: false, camera_source: false, sink: true),
                        pin: nil, known_peers_json: pares, signaling_port: 0, timeout_ms: prazoMs,
                        tracks: nil, track_count: 0, bind_address: nil)
                    return withUnsafePointer(to: &opcoes) { o in
                        cancelador.comPonteiro { c in quall_connect_with_invite(convite.ponteiro, o, c) }
                    }
                }
            }
        }
        guard let nova else {
            let status = quall_last_status()
            return .failure(FalhaDoConvite(status: NucleoReceptor.nome(status),
                                           cancelado: status == QUALL_STATUS_CANCELLED,
                                           motivo: NucleoReceptor.ultimoErro()))
        }
        return .success(SessaoDoConvite(nova))
    }
}

public struct FalhaDoConvite: Error, Sendable {
    public let status: String
    public let cancelado: Bool
    public let motivo: String
}

private func comOpcionalC<R>(_ texto: String?, _ corpo: (UnsafePointer<CChar>?) -> R) -> R {
    guard let texto else { return corpo(nil) }
    return texto.withCString { corpo($0) }
}

/// Lê um texto da fronteira C no padrão `(buf, cap)`, crescendo o buffer até caber. É o mesmo
/// contrato do helper do Studio, sem depender do teleprompter, que o Monitor não inclui.
func lerAteCaber(_ chamada: (UnsafeMutablePointer<CChar>?, UInt) -> Int) -> String? {
    var precisa = chamada(nil, 0)
    for _ in 0..<16 {
        guard precisa > 0 else { return nil }
        let capacidade = precisa + 64
        var buffer = [CChar](repeating: 0, count: capacidade)
        let escritos = buffer.withUnsafeMutableBufferPointer { p in chamada(p.baseAddress, UInt(p.count)) }
        if escritos < 0 { return nil }
        if escritos > 0 && escritos <= capacidade { return String(cString: buffer) }
        precisa = escritos
    }
    return nil
}
