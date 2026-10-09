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
- Página `/quall-monitor/`: verificada no navegador local; `index.html` enviado por WebFTP para `public_html/quall-monitor/`, com o requisito do Quall Studio instalado no receptor e o aviso de ausência de Authenticode no Windows.
- Publicação por SFTP: após a falha do upload dos binários pelo WebFTP, os três pacotes foram enviados para `/public_html/quall-monitor/downloads/`. O manifesto final `downloads.json` foi enviado por SFTP com confirmação de envio concluído; os links de download estão configurados.
- Integridade na hospedagem: os três binários foram baixados de volta por SFTP para `dist/hosted-check/`, e seus SHA-256 coincidiram com `dist/site/quall-monitor/downloads.json`. A página e o manifesto baixados de volta são iguais, byte a byte, à preparação; os três botões foram conferidos no navegador usando essa preparação.

## Acesso público

Em 09/10/2026, o usuário confirmou que a página HTTPS abriu e que os downloads foram baixados em um Samsung Galaxy S24 pela rede móvel. A confirmação cobre a publicação da página e a transferência dos arquivos. O timeout observado na rede de desenvolvimento permanece como limitação da verificação local.

## Validação nativa e limites

O workflow [Build 37938552250](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37938552250) passou nos quatro jobs para a revisão `8f9f1f58f9a9391db6c40b478594f09d247d9995`: núcleo, Mac arm64, Mac Intel e Windows x64. Cada arquitetura Mac passou os 47 testes. Os três pacotes 0.1.0 preparados — Mac arm64, Mac Intel (x64) e Windows x64 (MSI) — usam essa revisão exata, sem publicação em lojas.

O verificador MSI inicial emitiu dois avisos WiX antes do JSON externo. A inspeção do MSI passou e os binários permaneceram intactos; o relatório externo foi normalizado, e o verificador foi corrigido para enviar avisos somente ao console. A sintaxe e o fluxo de saída foram verificados com PowerShell.

Não foi executada instalação/desinstalação do MSI em Windows limpo, importação de certificado, captura de tela, criação de monitor virtual nem sessão em aparelhos físicos nesta preparação. As regras de upgrade, ownership e consentimento do driver foram revisadas no código; rollback real, reboot e compatibilidade de drivers precisam de ensaio em Windows. A compilação e os testes sintéticos não são prova física de monitor estendido.

O `cargo fmt --all --check` do núcleo herdado encontrou diferenças de formatação preexistentes. Não foi feita uma reformatação geral dos fontes do Studio neste recorte.

## Versão 0.1.1 — validação automática do protocolo

O núcleo foi portado do baseline público [`5842bbc5d52c3be91d3b094fab10d81c0fddf842`](https://github.com/Quevenapp/Quall/tree/5842bbc5d52c3be91d3b094fab10d81c0fddf842), com protocolo 3, rota WebSocket `/quall/v3`, pareamento OPAQUE-3DH e canal de sinalização autenticado e cifrado. Não há fallback para as rotas ou o pareamento antigos. Os registros legados são preservados, mas o primeiro vínculo v3 exige digitar novamente o PIN; a retomada sem PIN usa o vínculo v3 salvo.

- Núcleo sem dependências nativas: **340 testes passaram e 2 foram ignorados**.
- Núcleo com transporte nativo, em Release: **541 testes passaram e 3 foram ignorados**.
- Fronteira C/FFI com transporte nativo, em Release: **99 testes passaram**, sem falhas.
- Regressão do HTTP 404: a incompatibilidade de rota foi reproduzida no host 0.1.0 antes do PIN; os testes conferiram a rota v3, a rejeição HTTP 404 de rotas legadas e o diagnóstico de um endpoint que recusa o handshake. As verificações direcionadas finais de rota legada e PIN incorreto também passaram.
- Oito sessões de Monitor e uma de Studio ficaram conectadas simultaneamente em **loopback**, com portas e transportes separados, dados isolados e oito receptores com identidades distintas. A regressão conferiu o armazenamento dos vínculos e a retomada sem PIN de cada receptor, mantendo os demais e o Studio conectados.
- Os testes do protocolo incluem rejeição de PIN incorreto, autenticação, adulteração e replay do canal cifrado.
- Casca e bibliotecas Mac: **55 testes passaram**, incluindo o limite de oito monitores, reconexão, cancelamento, tradução PT/EN e reserva de índice quando a saída do helper/monitor não é confirmada. A classificação da rede local foi conferida com testes puros; nenhum deles altera permissões do sistema.
- Preparação de downloads: o verificador rejeitou um MSI cujo relatório tinha SHA-256 diferente e rejeitou a promoção a canal estável dos pacotes Windows sem Authenticode. A preparação anterior foi preservada nos dois casos.
- Na primeira CI da 0.1.1, [Build 37973752384](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37973752384), núcleo e os dois Mac passaram; o núcleo teve 340 testes unitários aprovados, 2 ignorados e 1 integração aprovada. ARM e Intel tiveram 55 testes Swift aprovados cada. Windows teve **557 testes nativos aprovados**, sem falhas ou ignorados, e compilou o executável. A embalagem MSI parou porque a extensão Firewall do WiX 5.0.2 não fornece sete mensagens em pt-BR; o arquivo de localização recebeu traduções dessas mensagens preservando seus parâmetros. A geração dos pacotes será repetida após essa correção.

Essas provas automáticas verificam protocolo, transporte, pareamento e retomada. Não comprovam oito telas físicas, captura e criação de monitores reais, sessões Android/iPad com a nova release ou instalação do driver em Windows. Os builds, a CI, a assinatura/notarização dos novos pacotes e a publicação da 0.1.1 serão registrados após sua conclusão; a publicação e o acesso público descritos acima correspondem à **0.1.0**.
