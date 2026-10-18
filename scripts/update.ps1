[CmdletBinding()]
param(
    [string]$Config,
    [switch]$MigrateAdhoc
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if ([string]::IsNullOrWhiteSpace($Config)) { throw 'An existing profile config is required with -Config.' }
if (-not (Test-Path -LiteralPath $Config -PathType Leaf)) {
    throw "Config file not found: $Config"
}
$configPath = (Resolve-Path -LiteralPath $Config).Path
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or -not [Environment]::Is64BitOperatingSystem) {
    throw 'No prebuilt Discodex release is available for this operating system or architecture.'
}

$target = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()) {
    'X64' { 'x86_64-pc-windows-msvc' }
    'Arm64' { 'aarch64-pc-windows-msvc' }
    default { throw 'No prebuilt Discodex release is available for this architecture.' }
}
$asset = "discodex-$target.exe"
$baseUrl = 'https://github.com/CHOAM-CO/Discodex/releases/latest/download'
$tempDirectory = Join-Path ([IO.Path]::GetTempPath()) ('discodex-update-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempDirectory | Out-Null
try {
    $binaryPath = Join-Path $tempDirectory $asset
    $checksumPath = Join-Path $tempDirectory ($asset + '.sha256')
    Invoke-WebRequest -Uri "$baseUrl/$asset" -OutFile $binaryPath
    Invoke-WebRequest -Uri "$baseUrl/$asset.sha256" -OutFile $checksumPath

    $checksumText = [IO.File]::ReadAllText($checksumPath).Trim()
    $expected = ($checksumText -split '\s+')[0]
    if ($expected -notmatch '^[0-9a-fA-F]{64}$') { throw 'The release checksum file is invalid.' }
    $actual = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    if (-not [string]::Equals($expected, $actual, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The downloaded release binary failed SHA256 verification.'
    }

    $updateArguments = @('update', '--source', $binaryPath, '--config', $configPath)
    if ($MigrateAdhoc) { $updateArguments += '--migrate-adhoc' }
    & $binaryPath @updateArguments
    if ($LASTEXITCODE -ne 0) { throw "Discodex update returned exit code $LASTEXITCODE." }
} finally {
    Remove-Item -LiteralPath $tempDirectory -Recurse -Force -ErrorAction SilentlyContinue
}
