import CoreGraphics
import QuallIdiomaKit

/// Screen Recording access for this process. Monitor uses it only for video capture.
/// A missing Settings entry does not establish whether access was previously requested.
public enum PermissaoDeTela {
    public enum Estado: String, Sendable { case concedida, faltando }

    /// Reads current process access without requesting it. False is not a denial history.
    public static func estado() -> Estado {
        CGPreflightScreenCaptureAccess() ? .concedida : .faltando
    }

    /// Explicit synchronous request, invoked by a user action in the main application.
    /// Launch the signed .app through Finder/LaunchServices for the intended responsible app.
    /// False only means access is unavailable now; it does not confirm denial, a displayed
    /// dialog, or registration in Settings. Changing Settings may require reopening the app.
    @discardableResult public static func pedir() -> Bool { CGRequestScreenCaptureAccess() }

    /// Combines the diagnostic and request. jaTinha describes only this call's initial state.
    public static func conferirEPedir() -> (estado: Estado, jaTinha: Bool, texto: String) {
        if estado() == .concedida {
            return (.concedida, true, T("Gravação de Tela disponível neste processo."))
        }
        if pedir() {
            return (.concedida, false, T("Gravação de Tela disponível neste processo."))
        }
        return (.faltando, false,
            T("Permita o Quall Monitor em Gravação do Áudio do Sistema e da Tela. Se ele não aparecer, use + para adicionar Quall Monitor.app em Aplicativos. Depois, feche e abra o app."))
    }
}
