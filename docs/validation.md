# Verificações de 09/10/2026

## Executado no Mac de desenvolvimento

- Núcleo Rust: `cargo test -p quall-core --no-default-features --locked`, **341 testes passaram** (340 unitários e 1 de integração).
- Núcleo nativo limpo: `MACOSX_DEPLOYMENT_TARGET=13.0 CARGO_PROFILE_RELEASE_LTO=false cargo build --release -p quall-ffi --locked`, compilação concluída, incluindo libdatachannel/OpenSSL/Opus.
- Casca macOS e helper: builds Release arm64 e Intel concluídos. Os dois pacotes foram assinados com Developer ID Application do titular, com hardened runtime e timestamp; a Apple retornou `Accepted` nas duas notarizações, os tickets foram anexados e validados, e o Gatekeeper retornou `accepted / Notarized Developer ID`.
- Duas aberturas pelo LaunchServices confirmaram identidade persistida do Monitor, helper próprio e produto de monitor virtual 2; o Studio já aberto permaneceu em execução. A prova usou o modo de verificação sem captura ou criação de monitor.
- Mac: **47 testes passaram**, incluindo encode/decode de H.264 sintético com conferência em pixels, recuperação/troca de resolução, escalas e identidade distinta do monitor virtual.
- A diferença de diagnóstico de concorrência do Swift no runner arm64 da CI foi corrigida com armazenamento do resultado assíncrono protegido por trava. Build Release e os 47 testes locais passaram após a correção.
- Windows: **553 testes passaram** no runner Windows, executável GUI x64 e MSI compilados. A inspeção somente leitura conferiu as nove cargas do cabinet, hashes, ações elevadas do driver, diálogo de consentimento e revisão `8f9f1f58f9a9391db6c40b478594f09d247d9995`, com `Dirty: False`. Os recursos PE foram conferidos externamente: Quall Monitor, versão 0.1.0. O MSI e o executável da prévia estão sem Authenticode.
- Snapshot público: avisos de licença presentes e SHA-256 dos quatro payloads SudoVDA conferidos.
- Página `/quall-monitor/`: verificada no navegador local; `index.html` e `downloads.json` com lista de arquivos vazia foram enviados por WebFTP para `public_html/quall-monitor/`, incluindo o requisito do Quall Studio instalado no receptor. O upload dos binários pelo WebFTP falhou, e a abertura HTTPS apresentou timeout nesta rede. Os downloads de produto permanecem “Em preparação”, sem downloads ativos.

## Validação nativa e limites

O workflow [Build 37938552250](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37938552250) passou nos quatro jobs para a revisão `8f9f1f58f9a9391db6c40b478594f09d247d9995`: núcleo, Mac arm64, Mac Intel e Windows x64. Cada arquitetura Mac passou os 47 testes. Os três pacotes 0.1.0 preparados — Mac arm64, Mac Intel (x64) e Windows x64 (MSI) — usam essa revisão exata, sem publicação em lojas.

O verificador MSI inicial emitiu dois avisos WiX antes do JSON externo. A inspeção do MSI passou e os binários permaneceram intactos; o relatório externo foi normalizado, e o verificador foi corrigido para enviar avisos somente ao console. A sintaxe e o fluxo de saída foram verificados com PowerShell.

Não foi executada instalação/desinstalação do MSI em Windows limpo, importação de certificado, captura de tela, criação de monitor virtual nem sessão em aparelhos físicos nesta preparação. As regras de upgrade, ownership e consentimento do driver foram revisadas no código; rollback real, reboot e compatibilidade de drivers precisam de ensaio em Windows. A compilação e os testes sintéticos não são prova física de monitor estendido.

O `cargo fmt --all --check` do núcleo herdado encontrou diferenças de formatação preexistentes. Não foi feita uma reformatação geral dos fontes do Studio neste recorte.
