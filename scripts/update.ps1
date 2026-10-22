[CmdletBinding()]
param(
    [string]$Config,
    [switch]$MigrateAdhoc
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. (Join-Path $PSScriptRoot 'product.generated.ps1')
$language = if ([string]::IsNullOrWhiteSpace($env:DISCODEX_LANGUAGE)) { 'en' } else { $env:DISCODEX_LANGUAGE.Trim().ToLowerInvariant() }
if ($language -notin $script:DiscodexProduct.SupportedLanguages) { throw (Get-DiscodexMessage 'install.language_environment_error' 'en') }

if ([string]::IsNullOrWhiteSpace($Config)) { throw (Get-DiscodexMessage 'update.missing_config' $language) }
if (-not (Test-Path -LiteralPath $Config -PathType Leaf)) {
    throw ((Get-DiscodexMessage 'update.config_not_found' $language) -f $Config)
}
$configPath = (Resolve-Path -LiteralPath $Config).Path
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or -not [Environment]::Is64BitOperatingSystem) {
    throw ((Get-DiscodexMessage 'update.unsupported_platform' $language) -f [Environment]::OSVersion.Platform, [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture)
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$targetInfo = $script:DiscodexProduct.Platforms[$architecture]
if ($null -eq $targetInfo) { throw ((Get-DiscodexMessage 'update.unsupported_platform' $language) -f 'Windows', $architecture) }
$asset = $targetInfo.Asset
$baseUrl = $script:DiscodexProduct.ReleaseDownloadRoot + '/latest/download'
$updateId = [Guid]::NewGuid().ToString('N')
$tempDirectory = Join-Path ([IO.Path]::GetTempPath()) $script:DiscodexProduct.UpdateTempTemplate.Replace('{id}', $updateId)
New-Item -ItemType Directory -Path $tempDirectory | Out-Null
$operationFailure = $null
$cliFailure = $null
try {
    $binaryPath = Join-Path $tempDirectory $asset
    $checksumPath = Join-Path $tempDirectory ($asset + '.sha256')
    Invoke-WebRequest -Uri "$baseUrl/$asset" -OutFile $binaryPath
    Invoke-WebRequest -Uri "$baseUrl/$asset.sha256" -OutFile $checksumPath

    $checksumText = [IO.File]::ReadAllText($checksumPath).Trim()
    $expected = ($checksumText -split '\s+')[0]
    if ($expected -notmatch '^[0-9a-fA-F]{64}$') { throw (Get-DiscodexMessage 'install.invalid_checksum' $language) }
    $actual = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    if (-not [string]::Equals($expected, $actual, [StringComparison]::OrdinalIgnoreCase)) {
        throw (Get-DiscodexMessage 'install.checksum_mismatch' $language)
    }

    # The new worker stages its matching prebuilt runtime before stopping services.
    # Profile settings, CODEX_HOME, and session data remain in their existing locations.
    $updateArguments = @('update', '--source', $binaryPath, '--config', $configPath)
    if ($MigrateAdhoc) { $updateArguments += '--migrate-adhoc' }
    $stderrPath = Join-Path $tempDirectory 'updater.stderr'
    & $binaryPath @updateArguments 2> $stderrPath
    $exitCode = $LASTEXITCODE
    $cliStderr = [IO.File]::ReadAllText($stderrPath)
    if ($exitCode -ne 0) {
        $cliFailure = [pscustomobject]@{ ExitCode = $exitCode; Stderr = $cliStderr }
    }
} catch {
    $operationFailure = $_
}

$cleanupFailures = @()
if (Test-Path -LiteralPath $tempDirectory) {
    try {
        Remove-Item -LiteralPath $tempDirectory -Recurse -Force -ErrorAction Stop
    } catch {
        $cleanupFailures += $_.Exception.Message
    }
}
if ($cliFailure -and $cleanupFailures.Count -gt 0) {
    $message = (Get-DiscodexMessage 'update.discodex_failed' $language) -f $cliFailure.ExitCode
    [Console]::Error.WriteLine("{0} stderr: {1} Cleanup failed: {2}" -f $message, $cliFailure.Stderr, ($cleanupFailures -join '; '))
    exit $cliFailure.ExitCode
}
if ($cliFailure) {
    $message = (Get-DiscodexMessage 'update.discodex_failed' $language) -f $cliFailure.ExitCode
    [Console]::Error.WriteLine("{0} stderr: {1}" -f $message, $cliFailure.Stderr)
    exit $cliFailure.ExitCode
}
if ($operationFailure -and $cleanupFailures.Count -gt 0) {
    throw ("{0} Cleanup failed: {1}" -f $operationFailure.Exception.Message, ($cleanupFailures -join '; '))
}
if ($operationFailure) { throw $operationFailure }
if ($cleanupFailures.Count -gt 0) {
    throw ("Update cleanup failed: {0}" -f ($cleanupFailures -join '; '))
}
