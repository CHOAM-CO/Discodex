[CmdletBinding()]
param(
    [string]$Config,
    [switch]$MigrateAdhoc
)

$ErrorActionPreference = 'Stop'

function Invoke-ExternalCommand {
    param(
        [Parameter(Mandatory = $true)] [string]$FilePath,
        [Parameter(Mandatory = $true)] [string[]]$Arguments,
        [switch]$Quiet
    )

    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        if ($Quiet) {
            & $FilePath @Arguments *> $null
        } else {
            & $FilePath @Arguments 2>&1 | Out-Host
        }
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousPreference
    }
    return $exitCode
}

if ([string]::IsNullOrWhiteSpace($Config)) { throw 'An existing profile config is required with -Config.' }
if (-not (Test-Path -LiteralPath $Config -PathType Leaf)) {
    throw "Config file not found: $Config"
}
$configPath = (Resolve-Path -LiteralPath $Config).Path
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw 'Git is required to update this source checkout.'
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Rust Cargo is required to build Discodex.'
}

$gitCheck = Invoke-ExternalCommand -FilePath 'git' -Arguments @('-C', $repositoryRoot, 'rev-parse', '--is-inside-work-tree') -Quiet
if ($gitCheck -ne 0) {
    throw 'The update script must run from a Git source checkout.'
}
$gitDiff = Invoke-ExternalCommand -FilePath 'git' -Arguments @('-C', $repositoryRoot, 'diff', '--quiet', 'HEAD', '--') -Quiet
if ($gitDiff -ne 0) {
    throw 'Tracked source changes are present. Commit, stash, or otherwise preserve them before updating.'
}

$gitPull = Invoke-ExternalCommand -FilePath 'git' -Arguments @('-C', $repositoryRoot, 'pull', '--ff-only')
if ($gitPull -ne 0) {
    throw 'Could not fast-forward the source checkout. Resolve the branch state without discarding local work, then retry.'
}

$manifest = Join-Path $repositoryRoot 'Cargo.toml'
$targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repositoryRoot 'target' }
$buildResult = Invoke-ExternalCommand -FilePath 'cargo' -Arguments @('build', '--locked', '--release', '--manifest-path', $manifest, '--target-dir', $targetDirectory)
if ($buildResult -ne 0) {
    throw "The release build failed with exit code $buildResult."
}

$sourceBinary = Join-Path $targetDirectory 'release\discodex.exe'
if (-not (Test-Path -LiteralPath $sourceBinary -PathType Leaf)) {
    throw "Release binary was not produced: $sourceBinary"
}
$updateArgs = @('update', '--source', $sourceBinary, '--config', $configPath)
if ($MigrateAdhoc) { $updateArgs += '--migrate-adhoc' }
$updateResult = Invoke-ExternalCommand -FilePath $sourceBinary -Arguments $updateArgs
if ($updateResult -ne 0) {
    throw "Discodex update returned exit code $updateResult."
}
