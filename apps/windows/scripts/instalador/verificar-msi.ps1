# MPL-2.0. Read-only MSI inspection and cabinet extraction: no installation or custom action runs.
param(
    [Parameter(Mandatory=$true)][string]$Pacote,
    [Parameter(Mandatory=$true)][string]$Versao,
    [string]$Revisao,
    [string]$Estagio,
    [switch]$ExigirAssinatura,
    [switch]$ExigirFonteLimpa
)
$ErrorActionPreference = 'Stop'
$Pacote = (Resolve-Path -LiteralPath $Pacote).Path
function Com-Method($Objeto, [string]$Nome, [object[]]$Argumentos = @()) {
    return $Objeto.GetType().InvokeMember($Nome, [Reflection.BindingFlags]::InvokeMethod, $null, $Objeto, $Argumentos)
}
function Com-Property($Objeto, [string]$Nome, [object[]]$Argumentos = @()) {
    return $Objeto.GetType().InvokeMember($Nome, [Reflection.BindingFlags]::GetProperty, $null, $Objeto, $Argumentos)
}
function Linhas-Msi([string]$Sql, [int]$Colunas) {
    $view = Com-Method $script:banco 'OpenView' @($Sql)
    try {
        Com-Method $view 'Execute' | Out-Null
        while ($null -ne ($record = Com-Method $view 'Fetch')) {
            try {
                $linha = @()
                for ($i = 1; $i -le $Colunas; $i++) { $linha += [string](Com-Property $record 'StringData' @($i)) }
                Write-Output -NoEnumerate $linha
            } finally { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) | Out-Null }
        }
    } finally {
        Com-Method $view 'Close' | Out-Null
        [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view) | Out-Null
    }
}
$temporaria = Join-Path ([IO.Path]::GetTempPath()) ('QuallMonitor-verificar-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporaria | Out-Null
$installer = New-Object -ComObject WindowsInstaller.Installer
try {
    # Mode 0 is msiOpenDatabaseModeReadOnly. Never call InstallProduct or msiexec.
    $script:banco = Com-Method $installer 'OpenDatabase' @($Pacote, 0)
    $propriedades = @{}
    foreach ($linha in @(Linhas-Msi 'SELECT `Property`, `Value` FROM `Property`' 2)) { $propriedades[$linha[0]] = $linha[1] }
    if ($propriedades.ProductName -ne 'Quall Monitor' -or $propriedades.ProductVersion -ne $Versao) { throw 'ProductName/ProductVersion incorretos no MSI.' }
    if ($propriedades.UpgradeCode -ne '{CF7A1B50-649C-45A5-A330-5B11B72F1AD9}') { throw 'UpgradeCode de outro produto.' }
    if ($propriedades.ARPURLINFOABOUT -ne 'https://queven.com.br/quall-monitor/') { throw 'Página de produto incorreta no MSI.' }
    $esperados = @('quall-monitor.exe','driver-setup.ps1','LICENSE','LICENSE-SCOPE.md','NOTICE.txt','THIRD_PARTY_NOTICES.txt','SOURCE-REVISION.txt','SudoVDA-NOTICES.txt','WiX-NOTICES.txt')
    $arquivos = @{}
    foreach ($linha in @(Linhas-Msi 'SELECT `File`, `FileName` FROM `File`' 2)) {
        $nome = ($linha[1] -split '\|')[-1]
        if ($arquivos.ContainsKey($nome)) { throw "Arquivo duplicado no MSI: $nome" }
        $arquivos[$nome] = $linha[0]
    }
    if ($arquivos.Count -ne $esperados.Count) { throw 'O MSI contém arquivos fora do escopo do Quall Monitor.' }
    foreach ($nome in $esperados) { if (-not $arquivos.ContainsKey($nome)) { throw "Arquivo ausente no MSI: $nome" } }
    $acoes = @{}
    foreach ($linha in @(Linhas-Msi 'SELECT `Action`, `Type`, `Target` FROM `CustomAction`' 3)) { $acoes[$linha[0]] = $linha }
    foreach ($nome in @('InstallMonitorDriver','UninstallMonitorDriver','RollbackMonitorDriver')) {
        if (-not $acoes.ContainsKey($nome)) { throw "Custom action ausente: $nome" }
        if (([int]$acoes[$nome][1] -band 0x800) -eq 0 -or ([int]$acoes[$nome][1] -band 0x400) -eq 0) { throw "A ação $nome deve ser deferred/rollback e elevada." }
        if ($acoes[$nome][2] -notmatch 'driver-setup\.ps1|\[#DriverSetupScript\]' -or $acoes[$nome][2] -notmatch 'WindowsPowerShell') { throw "Wrapper do driver incorreto: $nome" }
    }
    $sequencia = @{}
    foreach ($linha in @(Linhas-Msi 'SELECT `Action`, `Condition` FROM `InstallExecuteSequence`' 2)) { $sequencia[$linha[0]] = $linha[1] }
    if ($sequencia.UninstallMonitorDriver -notmatch 'NOT UPGRADINGPRODUCTCODE' -or $sequencia.UninstallMonitorDriver -notmatch 'REMOVE') { throw 'A atualização deve preservar o driver.' }
    if ($sequencia.RollbackMonitorDriver -notmatch 'NOT DRIVER_OWNED_BEFORE') { throw 'Rollback não preserva a propriedade anterior.' }
    $controles = @(Linhas-Msi 'SELECT `Dialog_`, `Control`, `Type`, `Property`, `Text` FROM `Control`' 5)
    $consentimento = @($controles | Where-Object { $_[0] -eq 'MonitorDriverConsent' -and $_[2] -eq 'CheckBox' -and $_[3] -eq 'SUDOVDA_CONSENT' })
    $explicacao = @($controles | Where-Object { $_[0] -eq 'MonitorDriverConsent' -and $_[1] -eq 'Explanation' })
    if ($consentimento.Count -ne 1 -or $explicacao.Count -ne 1 -or $explicacao[0][4] -notmatch 'Root' -or $explicacao[0][4] -notmatch 'TrustedPublisher') { throw 'O diálogo deve explicar a confiança e exigir opção explícita.' }
    if ($propriedades.SUDOVDA_CONSENT) { throw 'O consentimento do driver não pode vir marcado por padrão.' }

    $extraidos = Join-Path $temporaria 'conteudo'
    & wix msi decompile $Pacote -x $extraidos -o (Join-Path $temporaria 'pacote.wxs')
    if ($LASTEXITCODE -ne 0) { throw 'Falha ao extrair o cabinet do MSI para inspeção.' }
    $hashes = @{}
    foreach ($nome in $esperados) {
        # WiX decompile extracts cabinet members into File/<MSI File identifier>.
        $caminho = Join-Path (Join-Path $extraidos 'File') $arquivos[$nome]
        if (-not (Test-Path -LiteralPath $caminho -PathType Leaf)) { throw "Payload não extraído: $nome" }
        $hashes[$nome] = (Get-FileHash -LiteralPath $caminho -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($Estagio -and $hashes[$nome] -ne (Get-FileHash -LiteralPath (Join-Path $Estagio $nome) -Algorithm SHA256).Hash.ToLowerInvariant()) { throw "O cabinet não corresponde ao estágio: $nome" }
    }
    $fonte = [IO.File]::ReadAllText((Join-Path (Join-Path $extraidos 'File') $arquivos['SOURCE-REVISION.txt']))
    if ($fonte -notmatch '(?m)^Repository: https://github\.com/Quevenapp/Quall-Monitor\r?$') { throw 'Origem dos fontes incorreta no payload.' }
    if ($Revisao -and $fonte -notmatch ('(?m)^Revision: ' + [regex]::Escape($Revisao) + '\r?$')) { throw 'O MSI não contém a revisão Git esperada.' }
    if ($ExigirFonteLimpa -and $fonte -notmatch '(?m)^Dirty: False\r?$') { throw 'O MSI veio de fontes com alterações locais.' }
    $exe = Join-Path (Join-Path $extraidos 'File') $arquivos['quall-monitor.exe']
    $bytes = [IO.File]::ReadAllBytes($exe)
    $pe = [BitConverter]::ToInt32($bytes, 0x3c)
    if ([BitConverter]::ToUInt16($bytes, $pe + 4) -ne 0x8664 -or [BitConverter]::ToUInt16($bytes, $pe + 24 + 68) -ne 2) { throw 'O executável extraído deve ser Windows GUI x64.' }
    if (-not [Text.Encoding]::ASCII.GetString($bytes).Contains('quall-monitor-distribuicao: desktop-v1')) { throw 'Produto incorreto no executável extraído.' }
    $exeComExtensao = Join-Path $temporaria 'quall-monitor.exe'
    Copy-Item -LiteralPath $exe -Destination $exeComExtensao
    $assinaturaExe = Get-AuthenticodeSignature -LiteralPath $exeComExtensao
    $assinaturaMsi = Get-AuthenticodeSignature -LiteralPath $Pacote
    if ($ExigirAssinatura -and ($assinaturaExe.Status -ne 'Valid' -or $assinaturaMsi.Status -ne 'Valid')) { throw 'O MSI e o executável devem ter assinaturas Authenticode válidas.' }
    [pscustomobject]@{
        ProductName = $propriedades.ProductName
        ProductVersion = $propriedades.ProductVersion
        SourceRevision = $fonte.Trim()
        MsiSha256 = (Get-FileHash -LiteralPath $Pacote -Algorithm SHA256).Hash.ToLowerInvariant()
        MsiAuthenticode = [string]$assinaturaMsi.Status
        ExeAuthenticode = [string]$assinaturaExe.Status
        FilesSha256 = $hashes
        DriverActionsInspected = $true
        DriverActionsExecuted = $false
    } | ConvertTo-Json -Depth 4
} finally {
    if ($script:banco) { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($script:banco) | Out-Null; $script:banco = $null }
    [Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer) | Out-Null
    Remove-Item -LiteralPath $temporaria -Recurse -Force
}
