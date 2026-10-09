# Quall Monitor

Casca simplificada do Quall para usar outro aparelho como **monitor estendido** pela rede local. Aplicativo para **macOS e Windows**, com downloads pelo site oficial Quéven e sem publicação nas lojas.

O Quall Monitor oferece **Estender tela** e **Exibir**, com até **oito monitores estendidos simultâneos** e interface em português e inglês. Ele utiliza o protocolo v3 do Quall Studio atual, com pareamento por PIN e sinalização cifrada. A interface não oferece câmera, gravação nem teleprompter.

## Instalação e uso

Os downloads ficam na [página Quall Monitor do site oficial Quéven](https://queven.com.br/quall-monitor/), para Mac Apple Silicon, Mac Intel e Windows x64. A revisão 0.1.1 substitui o protocolo antigo da prévia 0.1.0, que causava HTTP 404 ao conectar receptores Studio v3. A preparação e o estado das verificações de publicação estão descritos em [docs/downloads.md](docs/downloads.md). A página informa o requisito de **Quall Studio instalado no aparelho receptor**.

- **Mac:** macOS 13 ou superior, Apple Silicon ou Intel conforme a arquitetura do pacote. Abra o instalador DMG e arraste `Quall Monitor.app` para `Applications` (Aplicativos). Autorize o acesso à rede local ao conectar e a gravação de tela para estender. Se uma permissão estiver desligada, a interface oferece o ajuste correspondente. Cada monitor virtual é criado por um helper incluído no aplicativo e removido ao encerrar sua sessão.
- **Windows:** Windows 11 x64 (build 22000 ou superior). Execute o instalador MSI como administrador. O SudoVDA está embutido e é instalado junto. O desinstalador remove o driver quando ele pertence a esta instalação; um driver preexistente é preservado. Consulte [docs/sudovda.md](docs/sudovda.md) para a procedência e o estado de validação do pacote.

No computador principal, escolha **Estender tela**. No aparelho receptor, escolha **Exibir**, informe o endereço completo com a porta e o PIN apresentados pelo emissor. Repita em até oito receptores com os dados atuais do emissor. Use o Quall Monitor em outro Mac/Windows ou um receptor compatível do Quall Studio. Os aparelhos precisam estar na mesma rede local. O caminho manual atende redes que bloqueiam descoberta automática. Pareamentos da prévia 0.1.0 precisam ser refeitos por PIN no protocolo atual.

## Junto com Quall Studio

Os dois produtos têm identificadores, diretórios de dados, preferências, logs e atalhos separados. Cada um gera seu próprio ID de aparelho e mantém seus pareamentos. As portas de sessão são independentes e o protocolo de rede permanece compatível.

No Mac, o produto do monitor virtual também é distinto do Studio. No Windows, a trava global de acesso ao SudoVDA permanece compartilhada para evitar que dois donos removam os monitores um do outro. O Studio distribuído sem tela estendida e o Monitor podem executar simultaneamente; versões de desenvolvimento que também controlam o SudoVDA precisam respeitar essa exclusão no uso do driver.

## Compilar

Requisitos comuns: Rust stable, CMake, compilador C/C++, Perl e libclang para bindgen. O transporte é compilado do fonte, incluindo OpenSSL e libdatachannel. Os lockfiles e o fork em `vendor/datachannel-sys` fazem parte do build.

Mac, com Xcode Command Line Tools e CMake:

```sh
apps/macos/Empacotar/empacotar.sh
```

Windows, em terminal de desenvolvimento Visual Studio 2022 x64, com SDK do Windows, CMake, NASM, Perl/libclang e WiX 5:

```powershell
powershell -ExecutionPolicy Bypass -File apps/windows/scripts/instalador/construir-msi.ps1 -Versao 0.1.1 -Idioma pt-BR -Destino dist/windows
```

O workflow [Build](.github/workflows/build.yml) compila as duas cascas e guarda pacotes de validação, incluindo instaladores Windows em português e inglês. Nenhum workflow publica em lojas. Assinatura do instalador e assinatura/notarização de distribuição do Mac dependem das identidades da distribuidora.

## Fontes e licença

Código próprio autorizado sob **MPL-2.0**, a mesma licença open source do Quall Studio. Consulte [LICENSE](LICENSE), [LICENSE-SCOPE.md](LICENSE-SCOPE.md), [NOTICE.txt](NOTICE.txt), [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt) e os [avisos do driver](docs/SudoVDA-NOTICES.txt). Terceiros conservam seus termos.

Responsável pela distribuição: **Veneri & Quellis Ltda.** Origem e recorte: [SOURCE.md](SOURCE.md). Cada pacote deve incluir a revisão exata dos fontes em `SOURCE-REVISION.txt`, acessível no repositório público [Quevenapp/Quall-Monitor](https://github.com/Quevenapp/Quall-Monitor).

Compilação e testes automatizados não substituem uma prova de tela estendida em aparelhos físicos. As verificações executadas nesta versão estão em [docs/validation.md](docs/validation.md).
