# Instalador desktop Quall Monitor

O único pacote de distribuição Windows é um MSI x64, para download pelo site oficial. Não há pacote de loja, câmera virtual ou driver separado.

## Build

Instale WiX Toolset 5.0.2 e suas extensões:

```powershell
dotnet tool install --global wix --version 5.0.2
wix extension add -g WixToolset.Util.wixext/5.0.2
wix extension add -g WixToolset.Firewall.wixext/5.0.2
wix extension add -g WixToolset.UI.wixext/5.0.2
.\apps\windows\scripts\instalador\construir-msi.ps1 -Versao 0.1.1 -Destino "$PWD\dist\windows"
.\apps\windows\scripts\instalador\construir-msi.ps1 -Versao 0.1.1 -Idioma en-US -SoEmpacotar -Destino "$PWD\dist\windows"
```

A versão deve coincidir com `apps/windows/Cargo.toml`. O script compila `quall-monitor.exe` com o lockfile, verifica o marcador de produto e o PE GUI x64, e gera `Quall-Monitor-0.1.1-windows-x64.msi` e seu `.sha256`. `-Idioma pt-BR` é o padrão. `-Idioma en-US` gera `Quall-Monitor-0.1.1-windows-x64-en-US.msi`; `-SoEmpacotar` reutiliza o executável release existente para construir o segundo idioma sem recompilar. Os dois pacotes instalam o mesmo app bilíngue. A pasta `estagio` contém os arquivos de empacotamento, incluindo MPL, avisos próprios, de terceiros, SudoVDA e WiX e a revisão Git dos fontes.

As tabelas `QuallMonitor.pt-BR.wxl` e `QuallMonitor.en-US.wxl` traduzem o consentimento, mensagens de versão e sistema e a nota final. A cultura da UI padrão do WiX segue o mesmo idioma. O UpgradeCode é comum às duas culturas: instalar o outro idioma da mesma versão substitui o pacote anterior e preserva a propriedade do driver.

## Instalação, confiança e propriedade

`QuallMonitor.wxs` tem UpgradeCode, componentes, atalho e regras de firewall próprios. O sistema mínimo é lido de `CurrentBuildNumber` no registro nativo x64: Windows 11 build 22000. O MSI instala no Program Files e executa `driver-setup.ps1` elevado, chamando somente o helper do próprio executável.

O diálogo interativo explica o certificado autoemitido `CN=sudovda@su.mk`. Só a confirmação marcada em interface completa permite passar `--confiar-certificado`. Instalação silenciosa nunca importa confiança, mesmo com `SUDOVDA_CONSENT=1` na linha de comando. Ela pode prosseguir se o driver já existe ou se a confiança requerida já foi estabelecida.

A marca `HKLM\SOFTWARE\Quall Monitor\TelaEstendida` e a propriedade PnP exclusiva identificam efeitos do Monitor. O setup não adota um SudoVDA preexistente. Rollback remove efeitos de uma primeira instalação apenas quando não havia marca anterior. A remoção de uma versão antiga durante major upgrade não chama o desinstalador do driver. A desinstalação completa remove somente o que a marca registra; nós de monitor de outros produtos são preservados.

O helper devolve 3010 se um reinício for necessário. O wrapper registra esse resultado como sucesso e pede reinício no diário, evitando que o Windows Installer o interprete como falha de custom action. O resultado fica em `driver-setup.log`, ao lado do executável, que sai na desinstalação.

Para diagnóstico de setup:

```powershell
msiexec /i Quall-Monitor-0.1.1-windows-x64.msi /L*v monitor-install.log
```

Instalação real de driver, atualização, rollback, reinício, remoção e simultaneidade com Studio devem ser testados em uma máquina ou VM Windows apropriada antes de disponibilizar o MSI como lançamento estável.

## Conferência sem instalar

O build abre o MSI somente para leitura e decompila o cabinet com WiX, sem executar ações de instalação ou driver. Confere produto, versões do MSI e dos recursos PE, cultura e tradução exata do consentimento, arquivos, SHA-256 do payload, revisão dos fontes, PE x64 GUI e as ações do driver. O resultado fica no `.msi.validation.json`, com `Culture`, `ProductLanguage`, versões e o estado real das assinaturas Authenticode. Os avisos da decompilação aparecem no console e ficam fora do JSON.

Também é possível conferir um MSI externo:

```powershell
.\apps\windows\scripts\instalador\verificar-msi.ps1 -Pacote .\Quall-Monitor-0.1.1-windows-x64.msi -Versao 0.1.1 -Revisao <commit-Git> -ExigirFonteLimpa
```

Use `-ExigirAssinatura` para exigir assinaturas válidas no MSI e no executável, por exemplo ao conferir um lançamento assinado. O build de validação registra `NotSigned` quando os certificados de release ainda não foram fornecidos; não declara um arquivo sem assinatura como assinado.
