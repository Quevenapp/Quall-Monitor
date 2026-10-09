# Verificações de 09/10/2026

## Prévia 0.1.0 — executado no Mac de desenvolvimento

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

## Prévia 0.1.0 — acesso público

Em 09/10/2026, o usuário confirmou que a página HTTPS abriu e que os downloads foram baixados em um Samsung Galaxy S24 pela rede móvel. A confirmação cobre a publicação da página e a transferência dos arquivos. O timeout observado na rede de desenvolvimento permanece como limitação da verificação local.

## Prévia 0.1.0 — validação nativa e limites

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
- Na primeira CI da 0.1.1, [Build 37973752384](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37973752384), os testes passaram, mas a embalagem MSI parou porque a extensão Firewall do WiX 5.0.2 não fornece sete mensagens em pt-BR. O arquivo de localização recebeu traduções dessas mensagens preservando seus parâmetros; a CI seguinte concluiu os dois instaladores.

## Versão 0.1.1 — CI e instaladores

O [Build 37975744590](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37975744590) passou nos quatro jobs para a revisão [`3ebcbe1be815e41e86352e529b526fad830c989b`](https://github.com/Quevenapp/Quall-Monitor/tree/3ebcbe1be815e41e86352e529b526fad830c989b), em 09/10/2026:

- Núcleo: **340 testes unitários e 1 teste de integração passaram; 2 foram ignorados**.
- Mac Apple Silicon (arm64) e Intel (x64): build Release do app e do helper, DMG e ZIP concluídos; **55 testes Swift passaram em cada arquitetura**.
- Windows x64: executável GUI e instaladores **pt-BR e en-US** concluídos; **557 de 557 testes nativos passaram**, sem falhas ou ignorados.
- Os avisos de distribuição e os payloads SudoVDA passaram pelas verificações da CI.

Os dois relatórios `.msi.validation.json` registram produto Quall Monitor, versão **0.1.1** no MSI e nos recursos PE do executável, culturas `pt-BR` (1046) e `en-US` (1033), a revisão exata acima e `Dirty: False`. A inspeção extraiu as nove cargas do cabinet e conferiu os hashes, o consentimento e as ações do driver. Os SHA-256 dos dois MSIs baixados coincidiram com seus relatórios; estão registrados em [downloads.md](downloads.md#publicação-da-prévia-011-em-09102026).

Ambos os relatórios têm `DriverActionsInspected: true` e `DriverActionsExecuted: false`: houve inspeção, sem executar instalação ou remoção do driver. O MSI e o executável têm `Authenticode: NotSigned`; os pacotes Windows permanecem uma prévia com essa condição informada na página.

## Versão 0.1.1 — assinatura Mac e conferência nativa

Os pacotes Mac finais arm64 e Intel (x86_64) foram assinados com **Developer ID Application**, equipe `A6AXA7CBU3`; app e helper usam hardened runtime e timestamp. Os quatro envios à Apple — app e DMG de cada arquitetura — retornaram **`Accepted`**. Os tickets foram anexados ao app e ao DMG e validados com `stapler validate`; `codesign --verify --deep --strict` passou nos apps e o Gatekeeper aceitou os dois apps e os dois DMGs com `source=Notarized Developer ID`.

A auditoria final registrada em `dist/final-artifact-audit-0.1.1.json` conferiu versão **0.1.1**, build **2**, requisito macOS **13.0**, identidade `br.com.queven.quall.monitor`, localizações PT/EN e revisão de fonte `3ebcbe1be815e41e86352e529b526fad830c989b`. Os DMGs foram montados somente para leitura: app e helper contidos mantêm a arquitetura e assinatura esperadas, o app mantém ticket e revisão válidos, e o atalho aponta para `/Applications`. Os hashes finais dos DMGs estão registrados em [downloads.md](downloads.md#publicação-da-prévia-011-em-09102026).

O app arm64 final foi instalado e conferido em `/Applications/Quall Monitor.app`. A interface alternou corretamente entre inglês e português e entrou em espera em **`192.168.0.9:7878`**, com PIN atual e orientação sobre até oito monitores. Essa conferência não concedeu novas permissões de privacidade nem estabeleceu uma sessão com receptor físico.

O endpoint `/quall/v3` do app instalado respondeu **`HTTP/1.1 101 Switching Protocols`** em `127.0.0.1:7878` e `192.168.0.9:7878`. Essa prova verifica a rota do handshake da versão final antes de PIN e pareamento; o erro 404 observado na 0.1.0 não ocorreu nesse teste.

## Versão 0.1.1 — publicação do site e limites

A página, o ícone, o manifesto e os quatro pacotes foram publicados por SFTP em `/public_html/quall-monitor/`. O manifesto indica versão **0.1.1**, canal **`preview`**, hashes finais e a mesma revisão exata `3ebcbe1be815e41e86352e529b526fad830c989b` para todos os arquivos. O download de volta dos sete arquivos confirmou página, ícone e manifesto byte a byte e os quatro SHA-256 dos instaladores; a prova está em `dist/hosted-check-0.1.1-proof.json`. A preparação foi conferida no navegador em PT e EN, com quatro links, ícone e requisito do Quall Studio no receptor. O HTTPS continua em timeout na rede de desenvolvimento; a confirmação de acesso público pela rede móvel registrada no início corresponde à **0.1.0**, e a da **0.1.1** permanece pendente.

Essas provas automáticas verificam protocolo, transporte, pareamento e retomada. Não comprovam oito telas físicas, captura e criação de monitores reais, sessões Android/iPad com a nova release ou instalação/desinstalação do driver em Windows.
