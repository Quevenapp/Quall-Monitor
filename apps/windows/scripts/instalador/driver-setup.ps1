# MPL-2.0. Trusted wrapper installed beside the executable, invoked elevated by MSI.
param(
    [Parameter(Mandatory=$true)][ValidateSet('instalar','desinstalar')][string]$Acao,
    [string]$Consentimento = '',
    [int]$NivelUi = 2
)
$ErrorActionPreference = 'Stop'
$log = Join-Path $PSScriptRoot 'driver-setup.log'
try {
    $exe = Join-Path $PSScriptRoot 'quall-monitor.exe'
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw 'Executável Quall Monitor ausente.' }
    $argumentos = "--driver-tela-estendida $Acao --sem-cano"
    # A silent install never imports trust, even if a public MSI property was supplied.
    if ($Acao -eq 'instalar' -and $NivelUi -eq 5 -and $Consentimento -eq '1') {
        $argumentos += ' --confiar-certificado'
    }
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $exe
    $info.Arguments = $argumentos
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $processo = [System.Diagnostics.Process]::Start($info)
    $texto = $processo.StandardOutput.ReadToEnd()
    $erro = $processo.StandardError.ReadToEnd()
    $processo.WaitForExit()
    $codigo = $processo.ExitCode
    $processo.Dispose()
    Add-Content -LiteralPath $log -Encoding UTF8 -Value ("{0:o} {1}: saída {2}`r`n{3}{4}" -f [DateTime]::UtcNow, $Acao, $codigo, $texto, $erro)
    if ($codigo -eq 3010) {
        Add-Content -LiteralPath $log -Encoding UTF8 -Value 'Êxito. Reinicie o Windows antes de usar a tela estendida.'
        exit 0
    }
    if ($codigo -ne 0) { throw "O driver não terminou ($codigo). Consulte $log. Se a confiança no certificado estiver ausente, execute o instalador interativo e marque a confirmação do driver." }
    exit 0
} catch {
    Add-Content -LiteralPath $log -Encoding UTF8 -Value ("{0:o} FALHA: {1}" -f [DateTime]::UtcNow, $_.Exception.Message)
    Write-Error $_.Exception.Message
    exit 1
}
