# Quall Monitor para Windows

Casca desktop para criar telas estendidas e exibir uma tela recebida pela rede local. A interface tem **Estender**, **Exibir** e **Ajustes**. O produto exige Windows 11 x64 (build 22000 ou posterior), captura WGC e codecs Media Foundation disponíveis no sistema.

O Quall Monitor e o Quall Studio podem ficar abertos simultaneamente: executáveis, mutex de instância, janelas, dados, diário, identidade de rede e preferências são independentes. A porta de sinalização padrão é escolhida livremente. O protocolo de descoberta permanece compatível com o Quall; o nome anunciado termina em ` · Quall Monitor`.

O Quall Monitor usa `%APPDATA%\Quall Monitor` e `%LOCALAPPDATA%\Quall Monitor\Logs\quall-monitor.log`. A variável de bancada `QUALL_MONITOR_PASTA_DE_DADOS` permite apontar os dados a uma pasta separada. Nenhum dado do Quall Studio é importado.

## Executar e compilar

```powershell
cargo run --manifest-path apps/windows/Cargo.toml --locked --bin quall-monitor
cargo test --manifest-path apps/windows/Cargo.toml --lib --locked
.\apps\windows\scripts\instalador\construir-msi.ps1 -Versao 0.1.0 -Destino "$PWD\dist\windows"
```

Use Rust MSVC x64, Visual Studio Build Tools com SDK Windows, CMake, NASM, Perl e libclang. O instalador usa WiX Toolset **5.0.2** e as extensões Util, Firewall e UI da mesma versão. A [documentação do instalador](scripts/instalador/README.md) descreve os artefatos e a instalação do driver.

As opções públicas do executável são `--registro`, `--porta`, `--fps` (1–60), `--sem-som`, `--help` e `--version`. O app não oferece câmera, câmera virtual, gravação nem teleprompter. Os módulos compartilhados herdados do Studio continuam como código de biblioteca para manter os contratos do fluxo de vídeo; o pacote instala apenas o executável Monitor e seu wrapper de setup.

## Driver incluído

SudoVDA 1.10.9.289 vem embutido no executável. O setup pede elevação e instala o driver quando ele está ausente. Se o SudoVDA já existir, ele permanece sob a responsabilidade do programa que o instalou. O setup remove somente o adaptador, pacote e confiança em certificado registrados como instalados pelo Quall Monitor; uma atualização preserva essa propriedade.

O pacote atual contém um certificado autoemitido. Na instalação interativa, uma confirmação separada e inicialmente desmarcada informa que confiar nesse certificado o adiciona às lojas de máquina `Root` e `TrustedPublisher`. Uma instalação silenciosa não cria essa confiança: ela exige driver ou confiança preexistentes. Os hashes dos quatro arquivos embutidos são fixados e conferidos antes de qualquer instalação.

O mutex global de acesso ao adaptador virtual permanece compartilhado com o Quall e evita que duas implementações manipulem o driver ao mesmo tempo. Ele é diferente do mutex de instância do aplicativo. O Quall Studio distribuído sem a funcionalidade experimental de driver pode rodar ao lado do Monitor.

## Validação em hardware

Além dos testes automatizados, a liberação de binários deve conferir em Windows 11 x64: instalar e desinstalar com elevação, recusar confiança não aceita, preservar SudoVDA preexistente, atualizar mantendo propriedade do driver, parear dois aparelhos, criar telas distintas, reconectar, retirar as telas ao encerrar e manter o Quall Studio aberto no mesmo computador. Compilar o MSI não substitui esses testes de driver e rede.
