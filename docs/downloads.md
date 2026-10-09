# Downloads pelo site oficial

O Quall Monitor é distribuído exclusivamente pelo site oficial Quéven, sem App Store, Microsoft Store ou MSIX. O repositório GitHub público oferece os fontes. A CI guarda pacotes de validação como artefatos de build; não publica releases de produto nem envia arquivos às lojas.

A frente do site deve oferecer os pacotes macOS e Windows com versão, arquitetura, requisitos e SHA-256, acompanhados de um link para a revisão de fonte correspondente. O pacote macOS contém o app e seu helper; o instalador Windows contém o app e o driver SudoVDA, com instalação e desinstalação integradas.

Um build de validação não deve ser apresentado como release assinada. Para a entrega pública final no site, executar os scripts de empacotamento com as identidades de assinatura de distribuição e, no Mac, notarizar o pacote. A compilação local e a CI não importam chaves privadas para o repositório.

A publicação da página e a transferência dos pacotes para a hospedagem do site são uma etapa separada. Nenhuma URL de download é declarada ativa por este documento.

## Página própria

Destino escolhido: **https://queven.com.br/quall-monitor/**. Os arquivos estáticos estão em `site/quall-monitor/`. Sem pacotes associados, a página mostra “Em preparação” e não oferece links quebrados.

Depois de reunir os pacotes da mesma revisão e versão:

```sh
python3 tools/prepare-site.py --version 0.1.0 \
  --mac dist/macos/Quall-Monitor-0.1.0-macos-arm64.zip \
  --windows dist/windows/Quall-Monitor-0.1.0-windows-x64.msi
```

O comando prepara `dist/site/quall-monitor/` com os pacotes reais, hashes SHA-256 e o endereço dos fontes. Copiar esse diretório para a rota `/quall-monitor/` da hospedagem oficial. Não executar upload de binários de outra revisão com um manifesto apontando para o HEAD atual.

A página destaca o requisito solicitado: **Quall Studio instalado no aparelho receptor**. O Quall Monitor fica no computador principal.
