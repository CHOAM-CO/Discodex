[CmdletBinding()]
param(
    [switch]$Update,
    [string]$Config,
    [switch]$MigrateAdhoc
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. (Join-Path $PSScriptRoot 'product.generated.ps1')
if ($Update) {
    $updateScript = Join-Path $PSScriptRoot 'update.ps1'
    & $updateScript -Config $Config -MigrateAdhoc:$MigrateAdhoc
    return
}

if ([string]::IsNullOrWhiteSpace($env:DISCODEX_LANGUAGE)) {
    $choice = Read-Host (Get-DiscodexMessage 'install.language_prompt' 'en')
    if ([string]::IsNullOrWhiteSpace($choice)) { $choice = 'en' }
    $env:DISCODEX_LANGUAGE = $choice.Trim().ToLowerInvariant()
} else {
    $env:DISCODEX_LANGUAGE = $env:DISCODEX_LANGUAGE.Trim().ToLowerInvariant()
}
if ($env:DISCODEX_LANGUAGE -notin $script:DiscodexProduct.SupportedLanguages) {
    throw (Get-DiscodexMessage 'install.language_environment_error' 'en')
}

if ($env:OS -ne 'Windows_NT') { throw (Get-DiscodexMessage 'install.windows_only' $env:DISCODEX_LANGUAGE) }
$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$targetInfo = $script:DiscodexProduct.Platforms[$architecture]
if ($null -eq $targetInfo) {
    throw ((Get-DiscodexMessage 'install.unsupported_platform' $env:DISCODEX_LANGUAGE) -f 'Windows', $architecture)
}
$target = $targetInfo.Target

$dataRoot = Join-Path $env:LOCALAPPDATA $script:DiscodexProduct.DataDirectory
$binRoot = Join-Path $dataRoot $script:DiscodexProduct.BinaryDirectory
New-Item -ItemType Directory -Force -Path $binRoot | Out-Null
$installed = Join-Path $binRoot ($script:DiscodexProduct.Name + $targetInfo.Extension)
if (Test-Path -LiteralPath $installed) { throw ((Get-DiscodexMessage 'install.refuse_replace' $env:DISCODEX_LANGUAGE) -f $installed) }

$asset = $targetInfo.Asset
$baseUrl = $script:DiscodexProduct.ReleaseDownloadRoot + '/latest/download'
$installId = [Guid]::NewGuid().ToString('N')
$tempDirectory = Join-Path ([IO.Path]::GetTempPath()) $script:DiscodexProduct.InstallTempTemplate.Replace('{id}', $installId)
$staged = Join-Path $binRoot ($script:DiscodexProduct.InstallStagingTemplate.Replace('{id}', $installId) + $targetInfo.Extension)
New-Item -ItemType Directory -Path $tempDirectory | Out-Null
$operationFailure = $null
$cliFailure = $null
try {
    $binaryPath = Join-Path $tempDirectory $asset
    $checksumPath = Join-Path $tempDirectory ($asset + '.sha256')
    try {
        Invoke-WebRequest -Uri "$baseUrl/$asset" -OutFile $binaryPath
        Invoke-WebRequest -Uri "$baseUrl/$asset.sha256" -OutFile $checksumPath
    } catch { throw }

    $checksumText = [IO.File]::ReadAllText($checksumPath).Trim()
    $expected = ($checksumText -split '\s+')[0]
    if ($expected -notmatch '^[0-9a-fA-F]{64}$') { throw (Get-DiscodexMessage 'install.invalid_checksum' $env:DISCODEX_LANGUAGE) }
    $actual = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    if (-not [string]::Equals($expected, $actual, [StringComparison]::OrdinalIgnoreCase)) {
        throw (Get-DiscodexMessage 'install.checksum_mismatch' $env:DISCODEX_LANGUAGE)
    }

    Copy-Item -LiteralPath $binaryPath -Destination $staged
    [System.IO.File]::Move($staged, $installed)
    # The prebuilt installer selects shared/separate Codex data and optional AGY
    # settings. It downloads owned runtimes and offers cancellable AGY sign-in;
    # no global AGY installation or build toolchain is needed on this host.
    $stderrPath = Join-Path $tempDirectory 'installer.stderr'
    & $installed install 2> $stderrPath
    $exitCode = $LASTEXITCODE
    $cliStderr = [IO.File]::ReadAllText($stderrPath)
    if ($exitCode -ne 0) {
        $cliFailure = [pscustomobject]@{ ExitCode = $exitCode; Stderr = $cliStderr }
    }
} catch {
    $operationFailure = $_
}

$cleanupFailures = @()
foreach ($path in @($staged, $tempDirectory)) {
    if (-not (Test-Path -LiteralPath $path)) { continue }
    try {
        Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction Stop
    } catch {
        $cleanupFailures += $_.Exception.Message
    }
}
if ($cliFailure -and $cleanupFailures.Count -gt 0) {
    $message = (Get-DiscodexMessage 'install.discodex_failed' $env:DISCODEX_LANGUAGE) -f $cliFailure.ExitCode
    [Console]::Error.WriteLine("{0} stderr: {1} Cleanup failed: {2}" -f $message, $cliFailure.Stderr, ($cleanupFailures -join '; '))
    exit $cliFailure.ExitCode
}
if ($cliFailure) {
    $message = (Get-DiscodexMessage 'install.discodex_failed' $env:DISCODEX_LANGUAGE) -f $cliFailure.ExitCode
    [Console]::Error.WriteLine("{0} stderr: {1}" -f $message, $cliFailure.Stderr)
    exit $cliFailure.ExitCode
}
if ($operationFailure -and $cleanupFailures.Count -gt 0) {
    throw ("{0} Cleanup failed: {1}" -f $operationFailure.Exception.Message, ($cleanupFailures -join '; '))
}
if ($operationFailure) { throw $operationFailure }
if ($cleanupFailures.Count -gt 0) {
    throw ("Installer cleanup failed: {0}" -f ($cleanupFailures -join '; '))
}
