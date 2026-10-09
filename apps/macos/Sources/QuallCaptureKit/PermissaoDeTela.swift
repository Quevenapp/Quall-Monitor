import CoreGraphics
import Foundation
import QuallIdiomaKit

/// A permissão de **Gravação de Tela**, que no macOS também é a permissão de **áudio de sistema**.
///
/// # O ScreenCaptureKit pede uma concessão só para tela e som — mas o sistema tem duas
///
/// O ScreenCaptureKit captura tela e áudio de sistema sob a **mesma** concessão de TCC,
/// `kTCCServiceScreenCapture`. Quem a tem pode ligar `capturesAudio` e ouvir a máquina inteira;
/// quem não a tem não captura nem pixel nem amostra. É isso que este arquivo pede, e é o que o app
/// precisa.
///
/// **O que eu afirmei errado, e a correção.** Este comentário dizia que "não existe uma linha
/// separada para o som em Ajustes do Sistema". Isso valia para as versões antigas. No **macOS
/// 26.5.1 (build 25F80)** o painel se chama **"Gravação do Áudio do Sistema e da Tela"** e tem
/// **duas seções**: a de mesmo nome, onde o Quall está, e **"Apenas Gravação do Áudio do
/// Sistema"**, que é uma lista própria.
///
/// Não é só cosmético: os dois serviços de TCC existem de verdade, e estão nomeados no binário do
/// `SecurityPrivacyExtension.appex` deste sistema — `kTCCServiceScreenCapture` e
/// `kTCCServiceAudioCapture`, ao lado da string `SCREENANDAUDIOCAPTURE` que é o agrupamento do
/// painel.
///
/// **Qual API cai em `kTCCServiceAudioCapture` não foi medido.** A hipótese são os *process taps*
/// do CoreAudio (`AudioHardwareCreateProcessTap`, macOS 14.2+), que capturam áudio sem tocar em
/// tela — mas é hipótese, não fato: os frameworks vivem no dyld shared cache e não há arquivo para
/// inspecionar, e um experimento nesta máquina não distinguiria nada porque ela já tem
/// `ScreenCapture` concedido. Ver `docs/app-macos.md`.
///
/// Se isso for perseguido um dia, é **melhoria de privacidade real**: quem quer transmitir só o
/// som não deveria precisar conceder gravação de tela.
///
/// (A permissão de **Microfone** é outra coisa ainda, e não passa por aqui: o som de sistema não abre
/// microfone. Desde a R5 fase 4 (25/09) o microfone existe **só** junto da câmera, num botão que começa
/// desligado, pedido no primeiro toque pelo `DonoDaCamera` — `docs/audio.md` §8.2.)
///
/// # Ler não é pedir, e este arquivo existe por causa disso
///
/// `docs/regras-de-frente.md` fixa a regra depois que ela custou meia manhã na câmera virtual: o
/// painel de Privacidade **só lista quem pediu pelo menos uma vez**, então "o Quall não aparece
/// na lista" quer dizer *ninguém pediu*, e não *o usuário não ligou*. Um app que só consulta o
/// estado fica invisível no painel para sempre.
///
/// O comentário que estava em `CatalogoDeFontes` dizia que a Gravação de Tela "não tem
/// `requestAccess`" e que quem agenda o diálogo é a primeira consulta ao `SCShareableContent`.
/// Isso está incompleto: `CGRequestScreenCaptureAccess()` existe desde o macOS 10.15, é síncrona,
/// e é a chamada documentada que **cria a linha no painel**. Depender do efeito colateral de uma
/// consulta que falha é justamente a forma de pedir que a regra da casa desaconselha.
public enum PermissaoDeTela {
    public enum Estado: String, Sendable {
        case concedida
        case faltando
    }

    /// **Diagnóstico.** Lê o estado sem pedir nada e sem criar linha nenhuma no painel.
    ///
    /// Use para relatar, nunca para decidir que "falta um clique": se isto devolve `.faltando`, a
    /// pergunta seguinte é se alguém já **pediu** — e a resposta é `pedir()`.
    public static func estado() -> Estado {
        CGPreflightScreenCaptureAccess() ? .concedida : .faltando
    }

    /// **O pedido.** É esta chamada que faz o Quall aparecer em Ajustes do Sistema > Privacidade e
    /// Segurança > Gravação de Tela, e que mostra o diálogo na primeira vez.
    ///
    /// Só tem efeito no processo **responsável** — dentro de um `.app` aberto pelo LaunchServices.
    /// Chamada de um binário solto, o TCC atribui o pedido a quem abriu o shell e a linha sai com
    /// o nome errado (`docs/regras-de-frente.md`, "quem pede a permissão é o processo responsável").
    ///
    /// Devolve `true` se a permissão está concedida **agora**. Um `false` na primeira execução é o
    /// caso normal: o macOS pede que o app seja reaberto depois de a pessoa marcar a caixinha.
    @discardableResult
    public static func pedir() -> Bool {
        CGRequestScreenCaptureAccess()
    }

    /// As duas coisas de uma vez, com o texto que distingue os dois casos que antes se
    /// confundiam.
    ///
    /// A distinção importa porque as ações são opostas: "ninguém pediu ainda" se resolve sozinho
    /// no diálogo que acabou de aparecer; "o usuário desligou" só se resolve em Ajustes. Antes
    /// disto, o `SCStreamError.userDeclined` (-3801) chegava igual nos dois casos e a mensagem
    /// tinha de servir aos dois sem afirmar qual era.
    public static func conferirEPedir() -> (estado: Estado, jaTinha: Bool, texto: String) {
        let antes = estado()
        if antes == .concedida {
            return (.concedida, true, T("Gravação de Tela já concedida."))
        }
        let agora = pedir()
        if agora {
            return (.concedida, false, T("Gravação de Tela concedida agora, no diálogo do sistema."))
        }
        return (.faltando, false,
                T("Falta a permissão de Gravação de Tela — a mesma concessão cobre a tela **e** o "
                + "áudio do sistema. O pedido foi feito, então o Quall Monitor passa a aparecer em "
                + "Ajustes do Sistema > Privacidade e Segurança > Gravação do Áudio do Sistema e "
                + "da Tela. Marque a caixinha e abra o app de novo."))
    }
}
