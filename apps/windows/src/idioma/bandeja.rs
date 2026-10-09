//! A tabela de tradução: bandeja (ver `idioma.rs`). `(português, inglês)`, com os `{}` nas mesmas
//! posições de ordem; o português é o texto que está no código.
//!
//! O "Pronto" do menu ocioso não está aqui: na tabela ele é o botão ("Done", o glossário), e na
//! bandeja é o estado ("Ready", `regras_da_bandeja::ocioso`).

pub const TEXTOS: &[(&str, &str)] = &[
    // regras_da_bandeja.rs
    ("O Quall continua rodando aqui. Clique no ícone para abrir.", "Quall is still running here. Click the icon to open it."),
    ("Teleprompter aberto", "Teleprompter open"),
    ("Teleprompter: mostrando o texto", "Teleprompter: showing the text"),
    ("Teleprompter: controlando", "Teleprompter: remote"),
    ("Teleprompter: texto com a câmera", "Teleprompter: text with camera"),
    ("Câmera no ar", "Camera live"),
    ("{} para {}", "{} to {}"),
    ("Esperando um aparelho", "Waiting for a device"),
    ("Tela estendida: 1 aparelho", "Extended display: 1 device"),
    ("1 aparelho", "1 device"),
    ("{} aparelhos", "{} devices"),
    ("Tela estendida: esperando um aparelho", "Extended display: waiting for a device"),
    ("Tela estendida: {}", "Extended display: {}"),
    ("Conectando…", "Connecting…"),
    ("Conectando a {}", "Connecting to {}"),
    ("Exibindo", "Receiving"),
    ("Exibindo {}", "Receiving {}"),
    // bandeja.rs
    ("Abrir o Quall", "Open Quall"),
    ("Sair do Quall", "Quit Quall"),
];
