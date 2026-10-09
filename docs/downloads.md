# Downloads pelo site oficial

O Quall Monitor é distribuído exclusivamente pelo site oficial Quéven, sem App Store, Microsoft Store ou MSIX. O repositório GitHub público oferece os fontes. A CI guarda pacotes de validação como artefatos de build; não publica releases de produto nem envia arquivos às lojas.

A frente do site oferece pacotes macOS e Windows com versão, arquitetura, requisitos e SHA-256, acompanhados de um link para a revisão de fonte correspondente. O instalador macOS DMG contém o app, seu helper e um atalho para Aplicativos. O MSI Windows contém o app e o driver SudoVDA, com instalação e desinstalação integradas; há versões do instalador em português e inglês.

Um build de validação não deve ser apresentado como release assinada. A prévia Windows 0.1.0 pode ser oferecida com o estado de assinatura informado: o MSI da CI não tem Authenticode do aplicativo. Para uma versão estável, assinar o instalador e validar instalação, remoção e tela estendida em aparelhos físicos. Para a entrega pública do Mac, usar Developer ID e notarizar o pacote. A compilação local e a CI não importam chaves privadas para o repositório.

A publicação da página e a transferência dos pacotes para a hospedagem do site são uma etapa separada. A disponibilidade pública da prévia 0.1.0 foi confirmada pelo usuário em um Samsung Galaxy S24 pela rede móvel.

## Publicação da prévia 0.1.0 em 09/10/2026

O [Build 37938552250](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37938552250) passou nos quatro jobs. Foram preparados três pacotes 0.1.0 da revisão `8f9f1f58f9a9391db6c40b478594f09d247d9995`: Mac Apple Silicon (arm64), Mac Intel (x64) e Windows x64 (MSI). Os dois pacotes Mac estão assinados, com notarização `Accepted`, tickets anexados e validados, e aceitação pelo Gatekeeper. O MSI e o executável Windows estão sem Authenticode; a distribuição Windows está preparada como prévia. Os testes de tela estendida em aparelhos físicos permanecem pendentes.

O `index.html`, com o requisito de **Quall Studio instalado no receptor** e o aviso de ausência de Authenticode no Windows, foi enviado por WebFTP. Após a falha do upload dos binários pelo WebFTP, os três pacotes foram publicados por SFTP em `/public_html/quall-monitor/downloads/`, e o manifesto final `downloads.json` foi enviado por SFTP com confirmação de envio concluído. Os links de download estão configurados no manifesto.

Os três binários foram baixados de volta por SFTP para `dist/hosted-check/`; seus SHA-256 coincidiram com os valores de `dist/site/quall-monitor/downloads.json`. O `index.html` e o manifesto também foram baixados de volta e são iguais, byte a byte, à preparação. Essa conferência valida os arquivos armazenados na hospedagem. Em 09/10/2026, o usuário confirmou que a página HTTPS abriu e que os downloads foram baixados em um Samsung Galaxy S24 pela rede móvel. O timeout observado na rede de desenvolvimento permanece como limitação dessa verificação local.

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
