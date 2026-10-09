import CQuall
import Foundation

/// Anuncia este Mac por mDNS (`_quall._tcp`) enquanto ele espera alguém conectar.
///
/// # Por que isto é uma peça separada de `NucleoDeRede`
///
/// `quall_host` **bloqueia** até alguém chegar, e a porta de sinalização só pode ser lida da
/// sessão *depois* que ela sobe. Se o anúncio dependesse disso, este Mac só apareceria na lista
/// dos outros aparelhos depois de já ter recebido a conexão — inútil. O núcleo separa as duas
/// coisas de propósito (o comentário de `quall_host` no header diz isso com todas as letras:
/// "anunciar por mDNS é separado, para que a casca possa oferecer só o caminho por IP em rede que
/// bloqueia multicast"), e a consequência para a casca é que **ela escolhe a porta**, anuncia, e
/// só então hospeda.
///
/// # O que o macOS pode e o iPhone não
///
/// Nenhum perfil de provisionamento do projeto tem `com.apple.developer.networking.multicast`,
/// e por isso o iPhone não pode usar **este** caminho — o do núcleo, que abre socket multicast
/// cru. **Ele passou a anunciar por outro:** `AnuncianteBonjour`, com `NetService`, pelo
/// `mDNSResponder` do sistema, sem entitlement (medido em 01/09/2026). No macOS não há o teto e
/// este caminho serve: este Mac aparece na lista. A tela de espera daqui mostra as duas coisas mesmo assim —
/// o nome, para quem vê a lista, e o endereço, para quem está numa rede que bloqueia multicast,
/// que é o fallback obrigatório do `PROMPT.md` e já se pagou nesta bancada.
public final class Anunciante {
    private let trava = NSLock()
    private var handle: OpaquePointer?
    private var vivas: [UnsafeMutablePointer<CChar>] = []

    public init() {}

    deinit {
        parar()
        for p in vivas { free(p) }
    }

    private func guardar(_ texto: String) -> UnsafeMutablePointer<CChar> {
        let copia = strdup(texto) ?? UnsafeMutablePointer<CChar>.allocate(capacity: 1)
        vivas.append(copia)
        return copia
    }

    /// Devolve `false` quando o daemon de mDNS recusou — em rede com multicast bloqueado isso é
    /// esperado, **não é erro de produto**, e a interface deve continuar mostrando o endereço em
    /// vez de dizer que algo falhou.
    @discardableResult
    public func comecar(deviceId: String, nome: String, porta: UInt16, emiteTela: Bool, emiteCamera: Bool) -> Bool {
        let idC = guardar(deviceId)
        let nomeC = guardar(nome)
        var eu = QuallDeviceDesc(
            device_id: UnsafePointer(idC),
            display_name: UnsafePointer(nomeC),
            screen_source: emiteTela,
            camera_source: emiteCamera,
            sink: false)
        let novo = withUnsafePointer(to: &eu) { quall_advertiser_start($0, porta) }
        guard let novo else { return false }
        trava.lock()
        handle = novo
        trava.unlock()
        return true
    }

    /// **Anuncia como teleprompter** (`quall_advertiser_start_with_role`, `docs/contrato-teleprompter.md`
    /// §2): a chave TXT `pa` = `teleprompter` e o papel no nome da instância — o mesmo Mac anunciando
    /// vídeo e teleprompter ao mesmo tempo não colide. Sem capacidade de vídeo nenhuma: é assim que as
    /// listas de vídeo o deixam de fora. Mesma regra de falha de `comecar`: multicast bloqueado não é
    /// erro de produto, e o endereço continua na tela.
    @discardableResult
    public func comecarComoTeleprompter(deviceId: String, nome: String, porta: UInt16) -> Bool {
        let idC = guardar(deviceId)
        let nomeC = guardar(nome)
        let papelC = guardar("teleprompter")
        var eu = QuallDeviceDesc(
            device_id: UnsafePointer(idC),
            display_name: UnsafePointer(nomeC),
            screen_source: false,
            camera_source: false,
            sink: false)
        let novo = withUnsafePointer(to: &eu) { quall_advertiser_start_with_role($0, porta, papelC) }
        guard let novo else { return false }
        trava.lock()
        handle = novo
        trava.unlock()
        return true
    }

    /// **Bloqueia** por até ~1 s esperando o adeus do mDNS sair (dívida 3: a versão antiga só
    /// soltava a caixa e deixava um anúncio fantasma na lista dos outros aparelhos até o TTL
    /// expirar). Chame de uma thread de trabalho, nunca da main.
    public func parar() {
        trava.lock()
        let atual = handle
        handle = nil
        trava.unlock()
        if let atual { quall_advertiser_stop(atual) }
    }
}

/// O cancelador da espera — o `quall_session_cancel` que a dívida 10 pediu.
///
/// Ele recebe o **cancelador**, e não a sessão, porque enquanto se espera ainda não existe sessão
/// nenhuma. É isso que faz o botão Cancelar da tela de espera funcionar de verdade em vez de
/// mentir: sem ele a casca teria de abrir uma conexão descartável contra a própria porta de
/// sinalização para destravar `quall_host`, que é o contorno que este projeto já carregou.
public final class Cancelador {
    private let trava = NSLock()
    private var handle: OpaquePointer?

    public init() {
        handle = quall_canceller_new()
    }

    deinit {
        trava.lock()
        let atual = handle
        handle = nil
        trava.unlock()
        if let atual { quall_canceller_free(atual) }
    }

    /// Pode ser chamado de qualquer thread, quantas vezes quiser.
    public func cancelar() {
        trava.lock()
        let atual = handle
        trava.unlock()
        if let atual { quall_session_cancel(atual) }
    }

    public var cancelado: Bool {
        trava.lock()
        let atual = handle
        trava.unlock()
        guard let atual else { return false }
        return quall_canceller_is_cancelled(atual)
    }

    /// Empresta o ponteiro cru para a chamada bloqueante. A chamada segura a **própria** cópia da
    /// bandeira, então o cancelador pode ser liberado assim que ela voltar.
    func comPonteiro<T>(_ corpo: (OpaquePointer?) -> T) -> T {
        trava.lock()
        let atual = handle
        trava.unlock()
        return corpo(atual)
    }
}
