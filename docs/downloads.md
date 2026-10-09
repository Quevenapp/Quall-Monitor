# Downloads pelo site oficial

O Quall Monitor é distribuído exclusivamente pelo site oficial Quéven, sem App Store, Microsoft Store ou MSIX. O repositório GitHub público oferece os fontes. A CI guarda pacotes de validação como artefatos de build; não publica releases de produto nem envia arquivos às lojas.

A frente do site oferece pacotes macOS e Windows com versão, arquitetura, requisitos e SHA-256, acompanhados de um link para a revisão de fonte correspondente. O instalador macOS DMG contém o app, seu helper e um atalho para Aplicativos. O MSI Windows contém o app e o driver SudoVDA, com instalação e desinstalação integradas; há versões do instalador em português e inglês.

Um build de validação não deve ser apresentado como release assinada. A prévia Windows pode ser oferecida com o estado de assinatura informado: o MSI e o executável da CI não têm Authenticode. Para uma versão estável, assinar o instalador e validar instalação, remoção e tela estendida em aparelhos físicos. Para a entrega pública do Mac, usar Developer ID e notarizar o pacote. A compilação local e a CI não importam chaves privadas para o repositório.

A publicação da página e a transferência dos pacotes para a hospedagem do site são uma etapa separada. A disponibilidade pública da prévia 0.1.0 foi confirmada pelo usuário em um Samsung Galaxy S24 pela rede móvel.

## Publicação da prévia 0.1.0 em 09/10/2026

O [Build 37938552250](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37938552250) passou nos quatro jobs. Foram preparados três pacotes 0.1.0 da revisão `8f9f1f58f9a9391db6c40b478594f09d247d9995`: Mac Apple Silicon (arm64), Mac Intel (x64) e Windows x64 (MSI). Os dois pacotes Mac estão assinados, com notarização `Accepted`, tickets anexados e validados, e aceitação pelo Gatekeeper. O MSI e o executável Windows estão sem Authenticode; a distribuição Windows está preparada como prévia. Os testes de tela estendida em aparelhos físicos permanecem pendentes.

O `index.html`, com o requisito de **Quall Studio instalado no receptor** e o aviso de ausência de Authenticode no Windows, foi enviado por WebFTP. Após a falha do upload dos binários pelo WebFTP, os três pacotes foram publicados por SFTP em `/public_html/quall-monitor/downloads/`, e o manifesto final `downloads.json` foi enviado por SFTP com confirmação de envio concluído. Os links de download estão configurados no manifesto.

Os três binários foram baixados de volta por SFTP para `dist/hosted-check/`; seus SHA-256 coincidiram com os valores de `dist/site/quall-monitor/downloads.json`. O `index.html` e o manifesto também foram baixados de volta e são iguais, byte a byte, à preparação. Essa conferência valida os arquivos armazenados na hospedagem. Em 09/10/2026, o usuário confirmou que a página HTTPS abriu e que os downloads foram baixados em um Samsung Galaxy S24 pela rede móvel. O timeout observado na rede de desenvolvimento permanece como limitação dessa verificação local.

## Publicação da prévia 0.1.1 em 09/10/2026

O [Build 37975744590](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37975744590) passou nos quatro jobs para a revisão [`3ebcbe1be815e41e86352e529b526fad830c989b`](https://github.com/Quevenapp/Quall-Monitor/tree/3ebcbe1be815e41e86352e529b526fad830c989b). Foram compilados app e helper Mac arm64 e x64, DMGs e ZIPs de validação, executável Windows x64 e dois instaladores MSI. O núcleo passou 340 testes unitários e 1 integração, com 2 ignorados; cada arquitetura Mac passou 55 testes Swift; Windows passou 557 de 557 testes nativos.

Os dois MSIs baixados da CI têm versão 0.1.1 no produto e no executável, revisão exata dos fontes e `Dirty: False`. Seus relatórios `.msi.validation.json` registram culturas `pt-BR` e `en-US`, `DriverActionsInspected: true` e `DriverActionsExecuted: false`; a inspeção do cabinet e das ações do driver ocorreu sem instalar ou remover o driver. Os hashes dos arquivos baixados foram conferidos contra esses relatórios:

| Pacote Windows x64 | Idioma | SHA-256 |
| --- | --- | --- |
| `Quall-Monitor-0.1.1-windows-x64.msi` | Português (`pt-BR`) | `8f88bafe99a54841346a0e509375a6b90e30f5bd7c2c99a7fc772fcebe131510` |
| `Quall-Monitor-0.1.1-windows-x64-en-US.msi` | Inglês (`en-US`) | `1589ea4089d413fadd6943e56b000b8f0af087b5fe075324e952d7d0bb4fc29d` |

Ambos os MSIs e o executável Windows estão `NotSigned`; sua distribuição permanece no canal de prévia com aviso na página.

Os pacotes Mac finais receberam **Developer ID Application**, com hardened runtime e timestamp no app e helper. A Apple retornou **`Accepted` nos quatro envios**: app e DMG arm64, app e DMG Intel. Os tickets dos dois apps e dos dois DMGs foram anexados e validados; assinaturas e Gatekeeper passaram com `Notarized Developer ID`. A montagem somente para leitura confirmou app e helper na arquitetura correta, versão 0.1.1, build 2, revisão `3ebcbe1be815e41e86352e529b526fad830c989b` e atalho para `/Applications`. A auditoria completa ficou em `dist/final-artifact-audit-0.1.1.json`.

| Instalador Mac final | Arquitetura | SHA-256 |
| --- | --- | --- |
| `Quall-Monitor-0.1.1-macos-arm64.dmg` | Apple Silicon (`arm64`) | `575ea04a34e19c0edb6c4b8e507e7a75b5f46ff7fd67f3f816f23f382f718a4f` |
| `Quall-Monitor-0.1.1-macos-x64.dmg` | Intel (`x86_64`) | `4d51c8c0eb188eb2e7841333e1c6a97685ba9d3a08756fc2819ba97486d4327b` |

A página, o ícone, o manifesto **0.1.1** e esses quatro instaladores foram publicados por SFTP em `/public_html/quall-monitor/`, no canal **`preview`**. Os sete arquivos foram baixados de volta para `dist/hosted-check-0.1.1/`: página, ícone e manifesto são iguais à preparação byte a byte, e os quatro instaladores coincidem com os SHA-256 do manifesto. A prova local está em `dist/hosted-check-0.1.1-proof.json`. A página foi conferida em português e inglês na preparação local. O HTTPS do domínio continua sem responder na rede de desenvolvimento; a confirmação pela rede móvel registrada anteriormente corresponde à **0.1.0**, e a da **0.1.1** permanece pendente.

O app arm64 final foi instalado e conferido em `/Applications/Quall Monitor.app`; a interface alternou entre PT e EN e mostrou o estado de espera em `192.168.0.9:7878`, PIN atual e orientação sobre até oito monitores. Seu endpoint `/quall/v3` respondeu `HTTP/1.1 101 Switching Protocols` tanto em loopback como no endereço LAN, antes do pareamento. Essa conferência não concedeu novas permissões de privacidade nem estabeleceu sessão com receptor físico.

Os testes automáticos verificam até oito sessões em loopback. A validação física com Android/iPad, oito telas reais e instalação/desinstalação do driver em Windows permanece pendente; o detalhamento das provas está em [validation.md](validation.md#versão-011--ci-e-instaladores).

## Página própria

Destino escolhido: **https://queven.com.br/quall-monitor/**. Os arquivos estáticos estão em `site/quall-monitor/`. Sem pacotes associados, a página mostra “Em preparação” e não oferece links quebrados.

Depois de reunir os pacotes da mesma revisão e versão, substituir `SHA40_DOS_PACOTES` pelo SHA completo dos fontes usados no build. Obter esse valor do registro do build e do `SOURCE-REVISION.txt` de cada pacote; ele pode ser diferente do checkout atual:

```sh
python3 tools/prepare-site.py --version 0.1.1 \
  --source-revision SHA40_DOS_PACOTES \
  --mac dist/macos/Quall-Monitor-0.1.1-macos-arm64.dmg \
  --mac dist/macos-intel/Quall-Monitor-0.1.1-macos-x64.dmg \
  --windows dist/windows/Quall-Monitor-0.1.1-windows-x64.msi \
  --windows dist/windows/Quall-Monitor-0.1.1-windows-x64-en-US.msi
```

O comando exige um SHA de 40 caracteres que exista localmente como commit Git; se necessário, buscar a revisão antes. Não escolhe o `HEAD` automaticamente. Os nomes seguem `Quall-Monitor-VERSAO-macos-ARQUITETURA.dmg` (ou `.zip` para validação) ou `Quall-Monitor-VERSAO-windows-x64.msi`, com a mesma versão `MAJOR.MINOR.PATCH`. O MSI inglês tem sufixo `-en-US`; o padrão é português. As arquiteturas Mac aceitas são `arm64`, `x64` e `universal`.

Para o Mac, o preparador aceita os DMGs de instalação e os ZIPs de validação. Confere versão e identidade no `Info.plist`, revisão e repositório no `SOURCE-REVISION.txt` e arquiteturas Mach-O do app **e do helper**. Recusa fontes com alterações não commitadas. O DMG é montado somente para leitura por `hdiutil`, sem abrir o app; o comando verifica sua assinatura interna com `codesign --verify --deep --strict` e o atalho para `/Applications`. A arquitetura universal precisa conter arm64 e x64 nos dois executáveis. Identidade Developer ID, notarização e ticket do app e do DMG são conferidos no Mac antes da publicação.

Para o Windows, `verificar-msi.ps1` inspeciona e extrai o cabinet pelo WiX, sem instalar ou executar ações do driver. Confere `SOURCE-REVISION.txt` (mesmo repositório e SHA, `Dirty: False`), versão do MSI e do executável PE x64, cultura do instalador, consentimento do driver e estado Authenticode. O arquivo `.msi.validation.json` deve acompanhar cada entrada. O preparador confere o hash desse relatório contra o MSI exato, a versão e a revisão dos fontes antes de gerar links. Seu campo `provenance: external` identifica essa inspeção nativa externa. O canal `stable` exige assinaturas válidas do MSI e do executável; a prévia informa a ausência de assinatura. Renomear um MSI antigo não comprova versão nem revisão.

Depois de validar todos os arquivos, o comando substitui `dist/site/quall-monitor/` por uma preparação nova, com os pacotes, hashes SHA-256, arquiteturas e o endereço da revisão comprovada dos fontes. Os pacotes de entrada devem ficar fora desse diretório gerado. Uma entrada recusada preserva a preparação anterior. Copiar o diretório resultante para a rota `/quall-monitor/` da hospedagem oficial.

O canal padrão é `preview`: a página identifica a versão como prévia. Usar `--channel stable` somente após as validações de assinatura e uso em aparelhos físicos descritas acima.

A página destaca o requisito solicitado: **Quall Studio instalado no aparelho receptor**. O Quall Monitor fica no computador principal.
