//! A tabela de tradução: o driver da tela estendida que o Quall instala (02/10, noite; ver
//! `idioma.rs` e `regras_do_driver.rs`). `(português, inglês)`, com os `{}` nas mesmas posições de
//! ordem; o português é o texto que está no código.

pub const TEXTOS: &[(&str, &str)] = &[
    // o ladrilho apagado e o Narrador
    ("Instalar o driver da tela estendida", "Install the extended display driver"),
    (
        "Tela estendida, precisa de um driver. Instalar o driver da tela estendida: abre uma caixa que explica antes de instalar",
        "Extended display, needs a driver. Install the extended display driver: opens a box that explains before installing",
    ),
    ("Adaptador desligado — veja Ajustes", "Adapter turned off — see Settings"),
    ("Tela estendida, o adaptador está desligado. Abre os Ajustes", "Extended display, the adapter is turned off. Opens Settings"),
    // a caixa de instalar
    ("A tela estendida precisa de um driver de monitor virtual", "The extended display needs a virtual monitor driver"),
    (
        "Para o Windows ganhar um monitor novo para cada aparelho, o Quall instala o SudoVDA {}, um driver de monitor virtual gratuito feito pelo SudoMaker. Ele vem junto com o Quall; nada é baixado.",
        "So Windows gets a new monitor for each device, Quall installs SudoVDA {}, a free virtual monitor driver made by SudoMaker. It comes with Quall; nothing is downloaded.",
    ),
    ("O Windows vai pedir permissão de administrador.", "Windows will ask for administrator permission."),
    (
        "O certificado autoemitido do autor do SudoVDA ({}) será adicionado às lojas Root e TrustedPublisher deste computador. Isso torna esse certificado confiável na máquina. O setup remove somente o que o Quall Monitor instalou quando você desinstalar o aplicativo.",
        "SudoVDA's author's self-issued certificate ({}) will be added to this computer's Root and TrustedPublisher stores. This makes the certificate trusted on this machine. Setup removes only what Quall Monitor installed when you uninstall the app.",
    ),
    ("Instalar", "Install"),
    // a caixa de desinstalar e o cartão dos Ajustes
    ("Desinstalar o driver da tela estendida", "Uninstall the extended display driver"),
    ("Tirar o driver da tela estendida deste computador?", "Remove the extended display driver from this computer?"),
    (
        "O Quall tira o adaptador do SudoVDA, o pacote do driver e o certificado do autor, que ele mesmo pôs. A tela estendida fica apagada até você instalar de novo.",
        "Quall removes the SudoVDA adapter, the driver package and the author's certificate, which it put there itself. The extended display stays unavailable until you install it again.",
    ),
    ("Desinstalar", "Uninstall"),
    ("Driver da tela estendida", "Extended display driver"),
    ("Instalado pelo Quall Monitor (SudoVDA). Sai ao desinstalar o aplicativo.", "Installed by Quall Monitor (SudoVDA). Removed with the app."),
    (
        "Instalado pelo Quall, mas o adaptador está desligado no Gerenciador de Dispositivos.",
        "Installed by Quall, but the adapter is turned off in Device Manager.",
    ),
    (
        "Este computador já tem o SudoVDA, instalado por outro programa. A tela estendida do Quall já funciona com ele. Para instalar ou remover por aqui, remova antes o SudoVDA pelo programa que o instalou.",
        "This computer already has SudoVDA, installed by another program. Quall's extended display already works with it. To install or remove it here, first remove SudoVDA with the program that installed it.",
    ),
    ("Não instalado. Execute novamente o instalador do Quall Monitor ou clique em Tela estendida.", "Not installed. Run the Quall Monitor installer again or click Extended display."),
    ("Não instalado. Baixe o instalador do driver pelo ladrilho Tela estendida, em Espelhar.", "Not installed. Download the driver installer from the Extended display tile, in Mirror."),
    ("Instalado (SudoVDA). Para tirar, use o instalador do driver da tela estendida.", "Installed (SudoVDA). To remove it, use the extended display driver installer."),
    ("Desinstalar pelo instalador do driver", "Uninstall with the driver installer"),
    ("Baixe o driver da tela estendida", "Download the extended display driver"),
    (
        "Tela estendida, precisa do driver SudoVDA. Abre no navegador a página do instalador do driver",
        "Extended display, needs the SudoVDA driver. Opens the driver installer page in the browser",
    ),
    (
        "Tela estendida, precisa de um driver. Baixe o driver da tela estendida: abre no navegador a página do instalador",
        "Extended display, needs a driver. Download the extended display driver: opens the installer page in the browser",
    ),
    // o instalador avulso (quall-driver.exe)
    ("Instalador do driver da tela estendida do Quall Monitor", "Quall Monitor extended display driver installer"),
    ("Fechar", "Close"),
    ("O driver não está instalado neste computador.", "The driver isn't installed on this computer."),
    ("Pode fechar: o Quall já vê a tela estendida, mesmo aberto.", "You can close this: Quall already sees the extended display, even while open."),
    ("Este Windows não é x64: o driver da tela estendida não roda nele.", "This Windows isn't x64: the extended display driver doesn't run on it."),
    (
        "Há um adaptador SudoVDA desligado ou com erro no Gerenciador de Dispositivos. Ligue-o lá.",
        "There's a SudoVDA adapter turned off or with an error in Device Manager. Turn it on there.",
    ),
    // os passos
    ("Conferindo os arquivos do driver", "Checking the driver files"),
    ("Preparando a pasta de trabalho", "Preparing the work folder"),
    ("Confiando no certificado do SudoVDA", "Trusting the SudoVDA certificate"),
    ("Conferindo a assinatura do catálogo", "Checking the catalog signature"),
    ("Criando o adaptador", "Creating the adapter"),
    ("Instalando o driver", "Installing the driver"),
    ("Conferindo o adaptador", "Checking the adapter"),
    ("Tirando o adaptador", "Removing the adapter"),
    ("Tirando o pacote do driver", "Removing the driver package"),
    ("Tirando o certificado do SudoVDA", "Removing the SudoVDA certificate"),
    ("Conferindo que saiu", "Checking it's gone"),
    // o andamento e o resultado
    ("Esperando a permissão do Windows…", "Waiting for Windows permission…"),
    ("Instalando o driver da tela estendida — passo {} de {}: {}…", "Installing the extended display driver — step {} of {}: {}…"),
    ("Desinstalando o driver da tela estendida — passo {} de {}: {}…", "Uninstalling the extended display driver — step {} of {}: {}…"),
    ("Driver da tela estendida instalado. A Tela estendida já pode ser escolhida.", "Extended display driver installed. Extended display can be chosen now."),
    ("Driver da tela estendida desinstalado.", "Extended display driver uninstalled."),
    ("Pronto, mas o Windows pede para reiniciar o computador antes de usar.", "Done, but Windows asks to restart the computer before use."),
    ("Instalação cancelada: o Windows não recebeu a permissão. Nada foi instalado.", "Installation canceled: Windows didn't get permission. Nothing was installed."),
    ("Desinstalação cancelada: o Windows não recebeu a permissão. Nada foi tirado.", "Uninstall canceled: Windows didn't get permission. Nothing was removed."),
    ("Não deu para instalar o driver ({}): {}.", "Couldn't install the driver ({}): {}."),
    ("Não deu para desinstalar o driver ({}): {}.", "Couldn't uninstall the driver ({}): {}."),
    ("antes de começar", "before starting"),
    // os motivos
    ("os arquivos do driver dentro do Quall não conferem", "the driver files inside Quall don't match"),
    ("este Windows não é x64", "this Windows isn't x64"),
    ("já existe um adaptador SudoVDA neste computador", "there's already a SudoVDA adapter on this computer"),
    ("outra instalação do driver está em andamento", "another driver installation is in progress"),
    ("não deu para preparar a pasta de trabalho", "couldn't prepare the work folder"),
    ("não deu para guardar a marca da instalação", "couldn't save the installation record"),
    ("o Windows não aceitou o certificado", "Windows didn't accept the certificate"),
    ("a assinatura do catálogo não conferiu", "the catalog signature didn't check out"),
    ("o Windows não criou o adaptador", "Windows didn't create the adapter"),
    ("o Windows não instalou o driver", "Windows didn't install the driver"),
    ("o adaptador não ficou pronto em 10 s", "the adapter wasn't ready within 10 s"),
    ("o Quall não instalou este driver", "Quall didn't install this driver"),
    ("o adaptador não saiu", "the adapter wasn't removed"),
    ("o pacote do driver não saiu", "the driver package wasn't removed"),
    ("o certificado não saiu", "the certificate wasn't removed"),
    ("o pedido foi recusado", "the request was refused"),
    ("faltou a permissão de administrador", "administrator permission was missing"),
    ("o processo do driver terminou sem dizer como", "the driver process ended without saying how"),
    ("o Windows não abriu o processo do driver", "Windows didn't start the driver process"),
];
