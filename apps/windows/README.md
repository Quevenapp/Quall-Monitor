# Quall Monitor para Windows

Casca desktop para criar até **oito monitores estendidos simultâneos**, um por aparelho receptor, e exibir uma tela recebida pela rede local. A interface tem **Estender**, **Exibir** e **Ajustes**, com português e inglês pelo seletor **PT | EN**. A escolha fica nos dados do Monitor e o padrão segue o idioma do Windows. O produto exige Windows 11 x64 (build 22000 ou posterior), captura WGC e codecs Media Foundation disponíveis no sistema.

O emissor mantém uma espera para o próximo aparelho enquanto há vaga. Ao atingir oito conexões, retira o anúncio; quando um aparelho sai, a espera e o anúncio voltam. Cada receptor recebe seu próprio monitor e reconecta com o mesmo índice, enquanto as demais sessões continuam. **Exibir** recebe uma tela por instância, como no Quall anterior.

O Quall Monitor e o Quall Studio podem ficar abertos simultaneamente: executáveis, mutex de instância, janelas, dados, diário, identidade de rede e preferências são independentes. A porta de sinalização padrão é escolhida livremente. O protocolo v3 permanece compatível com o Quall Studio 1.0.0. Na descoberta aparece um rótulo efêmero `Quall <código>`, mostrado junto ao PIN/endereço da espera atual. O nome local termina em ` · Quall Monitor` e só chega ao outro aparelho pelo canal autenticado. Instalações Monitor 0.1.0 precisam parear novamente por PIN ao atualizar.

O MSI cria regras de firewall próprias do executável Monitor, para TCP e UDP na rede privada e na sub-rede local. Elas permitem portas dinâmicas e permanecem separadas das regras do Studio. Para parear, os aparelhos precisam estar na mesma rede local, com o perfil de rede do Windows definido como Privado.

O Quall Monitor usa `%APPDATA%\Quall Monitor` e `%LOCALAPPDATA%\Quall Monitor\Logs\quall-monitor.log`. A variável de bancada `QUALL_MONITOR_PASTA_DE_DADOS` permite apontar os dados a uma pasta separada. Nenhum dado do Quall Studio é importado.

## Executar e compilar

```powershell
cargo run --manifest-path apps/windows/Cargo.toml --locked --bin quall-monitor
cargo test --manifest-path apps/windows/Cargo.toml --lib --locked
.\apps\windows\scripts\instalador\construir-msi.ps1 -Versao 0.1.1 -Destino "$PWD\dist\windows"
.\apps\windows\scripts\instalador\construir-msi.ps1 -Versao 0.1.1 -Idioma en-US -SoEmpacotar -Destino "$PWD\dist\windows"
```

Use Rust MSVC x64, Visual Studio Build Tools com SDK Windows, CMake, NASM, Perl e libclang. O instalador usa WiX Toolset **5.0.2** e as extensões Util, Firewall e UI da mesma versão. O MSI padrão é em português; o segundo comando empacota a interface em inglês usando o mesmo executável bilíngue. A [documentação do instalador](scripts/instalador/README.md) descreve os artefatos e a instalação do driver. O ícone `quall.ico`, que mostra a marca Quall na tela de um monitor, é embutido no executável e usado nos atalhos e na lista de aplicativos do Windows.

As opções públicas do executável são `--registro`, `--porta`, `--fps` (1–60), `--sem-som`, `--help` e `--version`. O app não oferece câmera, câmera virtual, gravação nem teleprompter. Os módulos compartilhados herdados do Studio continuam como código de biblioteca para manter os contratos do fluxo de vídeo; o pacote instala apenas o executável Monitor e seu wrapper de setup.

## Driver incluído

SudoVDA 1.10.9.289 vem embutido no executável. O setup pede elevação e instala o driver quando ele está ausente. Se o SudoVDA já existir, ele permanece sob a responsabilidade do programa que o instalou. O setup remove somente o adaptador, pacote e confiança em certificado registrados como instalados pelo Quall Monitor; uma atualização preserva essa propriedade.

O pacote atual contém um certificado autoemitido. Na instalação interativa, uma confirmação separada e inicialmente desmarcada informa que confiar nesse certificado o adiciona às lojas de máquina `Root` e `TrustedPublisher`. Uma instalação silenciosa não cria essa confiança: ela exige driver ou confiança preexistentes. Os hashes dos quatro arquivos embutidos são fixados e conferidos antes de qualquer instalação.

O mutex global de acesso ao adaptador virtual permanece compartilhado com o Quall e evita que duas implementações manipulem o driver ao mesmo tempo. Ele é diferente do mutex de instância do aplicativo. O Quall Studio distribuído sem a funcionalidade experimental de driver pode rodar ao lado do Monitor.

## Validação em hardware

Além dos testes automatizados, a liberação de binários deve conferir em Windows 11 x64: instalar e desinstalar com elevação nos dois idiomas, recusar confiança não aceita, preservar SudoVDA preexistente, atualizar e trocar idioma mantendo propriedade do driver, parear oito aparelhos, criar telas distintas, tentar o nono, reconectar um sem interromper os demais, retirar as telas ao encerrar e manter o Quall Studio aberto no mesmo computador. Compilar o MSI não substitui esses testes de driver e rede.
