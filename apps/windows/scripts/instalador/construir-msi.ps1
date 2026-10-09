# MPL-2.0. Build Quall Monitor x64 for direct distribution; no Store or virtual-camera package.
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')][string]$Versao = '0.1.1',
    [string]$Destino,
    [ValidateSet('pt-BR','en-US')][string]$Idioma = 'pt-BR',
    [switch]$SoEmpacotar
)
$ErrorActionPreference = 'Stop'
$raiz = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
if (-not $Destino) { $Destino = Join-Path $raiz 'dist\windows' }
$Destino = [IO.Path]::GetFullPath($Destino)
$estagio = Join-Path $Destino 'estagio'
New-Item -ItemType Directory -Force -Path $estagio | Out-Null
$manifest = Join-Path $raiz 'apps\windows\Cargo.toml'
$versaoCargo = Select-String -LiteralPath $manifest -Pattern '^version = "([^"]+)"' | Select-Object -First 1
if (-not $versaoCargo -or $versaoCargo.Matches[0].Groups[1].Value -ne $Versao) {
    throw 'A versão solicitada deve coincidir com apps/windows/Cargo.toml e os recursos do executável.'
}
$target = Join-Path $raiz 'apps\windows\target'
$exe = Join-Path $target 'release\quall-monitor.exe'
if (-not $SoEmpacotar) {
    Push-Location (Split-Path $manifest)
    try {
        & cargo build --release --locked --bin quall-monitor --features net,tela-estendida-futura --target-dir $target
        if ($LASTEXITCODE -ne 0) { throw 'Falha ao compilar Quall Monitor para Windows.' }
    } finally { Pop-Location }
}
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "Executável não encontrado: $exe" }
$bytes = [IO.File]::ReadAllBytes($exe)
if (-not [Text.Encoding]::ASCII.GetString($bytes).Contains('quall-monitor-distribuicao: desktop-v1')) {
    throw 'O executável não contém o marcador do Quall Monitor. Recusando um app de outro produto.'
}
$pe = [BitConverter]::ToInt32($bytes, 0x3c)
if ([BitConverter]::ToUInt16($bytes, $pe + 4) -ne 0x8664) { throw 'O pacote exige PE x64.' }
if ([BitConverter]::ToUInt16($bytes, $pe + 24 + 68) -ne 2) { throw 'O app deve usar o subsistema Windows GUI.' }
foreach ($nome in @('SudoVDA.inf','SudoVDA.cat','SudoVDA.dll','SudoVDA.cer')) {
    $payload = Join-Path $raiz "apps\windows\terceiros\sudovda\$nome"
    if (-not (Test-Path -LiteralPath $payload -PathType Leaf)) { throw "Payload SudoVDA ausente: $nome" }
}
Copy-Item -LiteralPath $exe -Destination (Join-Path $estagio 'quall-monitor.exe') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'driver-setup.ps1') -Destination $estagio -Force
foreach ($nome in @('LICENSE','LICENSE-SCOPE.md','NOTICE.txt','THIRD_PARTY_NOTICES.txt')) {
    Copy-Item -LiteralPath (Join-Path $raiz $nome) -Destination $estagio -Force
}
Copy-Item -LiteralPath (Join-Path $raiz 'docs\SudoVDA-NOTICES.txt') -Destination $estagio -Force
Copy-Item -LiteralPath (Join-Path $raiz 'docs\WiX-NOTICES.txt') -Destination $estagio -Force
$rev = (& git -C $raiz rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Não foi possível identificar a revisão Git dos fontes.' }
$sujo = [bool](& git -C $raiz status --porcelain)
[IO.File]::WriteAllText((Join-Path $estagio 'SOURCE-REVISION.txt'), "Repository: https://github.com/Quevenapp/Quall-Monitor`nRevision: $rev`nDirty: $sujo`n", [Text.Encoding]::ASCII)
$texto = [IO.File]::ReadAllText((Join-Path $raiz 'LICENSE'))
$texto = $texto.Replace('\','\\').Replace('{','\{').Replace('}','\}').Replace("`r",'').Replace("`n",'\par ')
[IO.File]::WriteAllText((Join-Path $estagio 'license.rtf'), ('{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\f0\fs18 ' + $texto + '}'), [Text.Encoding]::ASCII)
$sufixoIdioma = if ($Idioma -eq 'en-US') { '-en-US' } else { '' }
$msi = Join-Path $Destino "Quall-Monitor-$Versao-windows-x64$sufixoIdioma.msi"
& wix build (Join-Path $PSScriptRoot 'QuallMonitor.wxs') -arch x64 -culture $Idioma `
    -loc (Join-Path $PSScriptRoot "QuallMonitor.$Idioma.wxl") `
    -ext WixToolset.Util.wixext -ext WixToolset.Firewall.wixext -ext WixToolset.UI.wixext `
    -d "Versao=$Versao" -d "Bin=$estagio" -o $msi
if ($LASTEXITCODE -ne 0) { throw 'Falha ao construir MSI.' }
& (Join-Path $PSScriptRoot 'verificar-msi.ps1') -Pacote $msi -Versao $Versao -Idioma $Idioma -Revisao $rev -Estagio $estagio | Set-Content -LiteralPath "$msi.validation.json" -Encoding UTF8
$hash = (Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$msi.sha256", "$hash  $([IO.Path]::GetFileName($msi))`n", [Text.Encoding]::ASCII)
Write-Output $msi
