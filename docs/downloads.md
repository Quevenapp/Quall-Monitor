# Downloads pelo site oficial

O Quall Monitor é distribuído exclusivamente pelo site oficial Quéven, sem App Store, Microsoft Store ou MSIX. O repositório GitHub público oferece os fontes. A CI guarda pacotes de validação como artefatos de build; não publica releases de produto nem envia arquivos às lojas.

A frente do site deve oferecer os pacotes macOS e Windows com versão, arquitetura, requisitos e SHA-256, acompanhados de um link para a revisão de fonte correspondente. O pacote macOS contém o app e seu helper; o instalador Windows contém o app e o driver SudoVDA, com instalação e desinstalação integradas.

Um build de validação não deve ser apresentado como release assinada. A prévia Windows 0.1.0 pode ser oferecida com o estado de assinatura informado: o MSI da CI não tem Authenticode do aplicativo. Para uma versão estável, assinar o instalador e validar instalação, remoção e tela estendida em aparelhos físicos. Para a entrega pública do Mac, usar Developer ID e notarizar o pacote. A compilação local e a CI não importam chaves privadas para o repositório.

A publicação da página e a transferência dos pacotes para a hospedagem do site são uma etapa separada. A disponibilidade pública da prévia 0.1.0 foi confirmada pelo usuário em um Samsung Galaxy S24 pela rede móvel.

## Estado em 09/10/2026

O [Build 37938552250](https://github.com/Quevenapp/Quall-Monitor/actions/runs/37938552250) passou nos quatro jobs. Foram preparados três pacotes 0.1.0 da revisão `8f9f1f58f9a9391db6c40b478594f09d247d9995`: Mac Apple Silicon (arm64), Mac Intel (x64) e Windows x64 (MSI). Os dois pacotes Mac estão assinados, com notarização `Accepted`, tickets anexados e validados, e aceitação pelo Gatekeeper. O MSI e o executável Windows estão sem Authenticode; a distribuição Windows está preparada como prévia. Os testes de tela estendida em aparelhos físicos permanecem pendentes.

O `index.html`, com o requisito de **Quall Studio instalado no receptor** e o aviso de ausência de Authenticode no Windows, foi enviado por WebFTP. Após a falha do upload dos binários pelo WebFTP, os três pacotes foram publicados por SFTP em `/public_html/quall-monitor/downloads/`, e o manifesto final `downloads.json` foi enviado por SFTP com confirmação de envio concluído. Os links de download estão configurados no manifesto.

Os três binários foram baixados de volta por SFTP para `dist/hosted-check/`; seus SHA-256 coincidiram com os valores de `dist/site/quall-monitor/downloads.json`. O `index.html` e o manifesto também foram baixados de volta e são iguais, byte a byte, à preparação. Essa conferência valida os arquivos armazenados na hospedagem. Em 09/10/2026, o usuário confirmou que a página HTTPS abriu e que os downloads foram baixados em um Samsung Galaxy S24 pela rede móvel. O timeout observado na rede de desenvolvimento permanece como limitação dessa verificação local.

## Página própria

Destino escolhido: **https://queven.com.br/quall-monitor/**. Os arquivos estáticos estão em `site/quall-monitor/`. Sem pacotes associados, a página mostra “Em preparação” e não oferece links quebrados.

Depois de reunir os pacotes da mesma revisão e versão, substituir `SHA40_DOS_PACOTES` pelo SHA completo dos fontes usados no build. Obter esse valor do registro do build e do `SOURCE-REVISION.txt` de cada pacote; ele pode ser diferente do checkout atual:

```sh
python3 tools/prepare-site.py --version 0.1.0 \
  --source-revision SHA40_DOS_PACOTES \
  --mac dist/macos/Quall-Monitor-0.1.0-macos-arm64.zip \
  --windows dist/windows/Quall-Monitor-0.1.0-windows-x64.msi
```

O comando exige um SHA de 40 caracteres que exista localmente como commit Git; se necessário, buscar a revisão antes. Não escolhe o `HEAD` automaticamente. Os nomes devem seguir exatamente `Quall-Monitor-VERSAO-macos-arm64.zip`, `Quall-Monitor-VERSAO-macos-x64.zip`, `Quall-Monitor-VERSAO-macos-universal.zip` ou `Quall-Monitor-VERSAO-windows-x64.msi`, com a mesma versão `MAJOR.MINOR.PATCH` passada ao comando.

Para o Mac, o preparador aceita os ZIPs produzidos pelo empacotamento e pela notarização. Confere a versão e a identidade no `Info.plist`, a revisão e o repositório no `SOURCE-REVISION.txt`, e as arquiteturas Mach-O do app **e do helper**. Recusa metadados que indiquem alterações não commitadas. Um ZIP universal precisa conter arm64 e x64 nos dois executáveis e recebe o rótulo “Mac Universal (Apple Silicon + Intel)”. O comando não verifica assinatura nem notarização; conferir ambas no Mac antes da publicação.

Para o Windows, a conferência interna do MSI é externa ao preparador. Antes de passá-lo a `--windows`, extrair os arquivos com o decompilador do WiX, sem executar ações de instalação, e conferir o `SOURCE-REVISION.txt` incluído: mesmo repositório e SHA, com `Dirty: False`. Conferir também `ProductVersion`, o executável PE x64 e sua versão, e o estado exato da assinatura Authenticode. Na prévia, informar a ausência de assinatura; uma versão estável deve ter assinatura de distribuição válida. O preparador confere o nome e o cabeçalho do arquivo, mas não lê essas propriedades do MSI; seu campo `provenance: external` indica que a comprovação depende dessa conferência do responsável pela publicação. Renomear um MSI antigo não comprova versão nem revisão.

Depois de validar todos os arquivos, o comando substitui `dist/site/quall-monitor/` por uma preparação nova, com os pacotes, hashes SHA-256, arquiteturas e o endereço da revisão comprovada dos fontes. Os pacotes de entrada devem ficar fora desse diretório gerado. Uma entrada recusada preserva a preparação anterior. Copiar o diretório resultante para a rota `/quall-monitor/` da hospedagem oficial.

O canal padrão é `preview`: a página identifica a versão como prévia. Usar `--channel stable` somente após as validações de assinatura e uso em aparelhos físicos descritas acima.

A página destaca o requisito solicitado: **Quall Studio instalado no aparelho receptor**. O Quall Monitor fica no computador principal.
