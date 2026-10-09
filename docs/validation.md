# Verificações de 09/10/2026

## Executado no Mac de desenvolvimento

- Núcleo Rust: `cargo test -p quall-core --no-default-features --locked`, **341 testes passaram** (340 unitários e 1 de integração).
- Núcleo nativo limpo: `MACOSX_DEPLOYMENT_TARGET=13.0 CARGO_PROFILE_RELEASE_LTO=false cargo build --release -p quall-ffi --locked`, compilação concluída, incluindo libdatachannel/OpenSSL/Opus.
- Casca macOS e helper: build Release concluído e bundle/ZIP arm64 montados. Assinatura ad-hoc verificada; não é assinatura Developer ID nem notarização pública.
- Duas aberturas pelo LaunchServices confirmaram identidade persistida do Monitor, helper próprio e produto de monitor virtual 2; o Studio já aberto permaneceu em execução. A prova usou o modo de verificação sem captura ou criação de monitor.
- Mac: **47 testes passaram**, incluindo encode/decode de H.264 sintético com conferência em pixels, recuperação/troca de resolução, escalas e identidade distinta do monitor virtual.
- Snapshot público: avisos de licença presentes e SHA-256 dos quatro payloads SudoVDA conferidos.
- Página `/quall-monitor/`: verificada no navegador local e enviada por WebFTP para `public_html/quall-monitor/`, incluindo o requisito do Quall Studio instalado no receptor. A abertura HTTPS pública não respondeu nesta rede durante a preparação.

## Validação nativa e limites

O workflow Build executa testes do núcleo, compila o Mac arm64/Intel e compila o app/instalador Windows x64 em runner Windows. Os pacotes são artefatos de validação, sem publicação em lojas. O resultado da CI deve ser conferido para a revisão exata antes de distribuir os downloads.

Não foi executada instalação/desinstalação do MSI em Windows limpo, importação de certificado, captura de tela, criação de monitor virtual nem sessão em aparelhos físicos nesta preparação. As regras de upgrade, ownership e consentimento do driver foram revisadas no código; rollback real, reboot e compatibilidade de drivers precisam de ensaio em Windows. A compilação e os testes sintéticos não são prova física de monitor estendido.

O `cargo fmt --all --check` do núcleo herdado encontrou diferenças de formatação preexistentes. Não foi feita uma reformatação geral dos fontes do Studio neste recorte.
