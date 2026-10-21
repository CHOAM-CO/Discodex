[CmdletBinding()]
param(
    [switch]$Update,
    [string]$Config,
    [switch]$MigrateAdhoc
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if ($Update) {
    $updateScript = Join-Path $PSScriptRoot 'update.ps1'
    & $updateScript -Config $Config -MigrateAdhoc:$MigrateAdhoc
    return
}

if ([string]::IsNullOrWhiteSpace($env:DISCODEX_LANGUAGE)) {
    $choice = Read-Host 'Choose language: [en] English (default), [ko] 한국어, [zh] 简体中文, [ja] 日本語'
    if ([string]::IsNullOrWhiteSpace($choice)) { $choice = 'en' }
    $env:DISCODEX_LANGUAGE = $choice.Trim().ToLowerInvariant()
} else {
    $env:DISCODEX_LANGUAGE = $env:DISCODEX_LANGUAGE.Trim().ToLowerInvariant()
}
if ($env:DISCODEX_LANGUAGE -notin @('en', 'ko', 'zh', 'ja')) {
    throw 'Set DISCODEX_LANGUAGE to en, ko, zh, or ja.'
}

$messages = switch ($env:DISCODEX_LANGUAGE) {
    'ko' { @{ Windows = '이 설치 프로그램은 Windows 전용입니다.'; Architecture = '감지한 환경 {0}에 사용할 Discodex 사전 빌드 릴리스가 없습니다.'; Download = '공식 Discodex 릴리스를 다운로드할 수 없습니다.'; Refuse = '기존 {0} 파일을 덮어쓰지 않습니다. 소유 설치를 통해 재설치·제거하거나, 기존 설치 제거 후 파일을 정리하세요.'; Install = 'discodex install이 종료 코드 {0}을 반환했습니다.' } }
    'zh' { @{ Windows = '此安装程序仅适用于 Windows。'; Architecture = '没有适用于检测到的环境 {0} 的 Discodex 预构建版本。'; Download = '无法下载 Discodex 官方预构建版本。'; Refuse = '不会覆盖现有文件 {0}。请使用该安装执行重新安装或卸载，之后再清理文件。'; Install = 'discodex install 返回退出代码 {0}。' } }
    'ja' { @{ Windows = 'このインストーラーは Windows 専用です。'; Architecture = '検出された環境 {0} 用の Discodex ビルド済みリリースはありません。'; Download = 'Discodex 公式ビルド済みリリースをダウンロードできません。'; Refuse = '既存ファイル {0} は上書きしません。既存のインストールから再インストールまたは削除し、その後ファイルを整理してください。'; Install = 'discodex install が終了コード {0} を返しました。' } }
    default { @{ Windows = 'This installer is for Windows.'; Architecture = 'No prebuilt Discodex release is available for detected environment {0}.'; Download = 'Could not download the official Discodex release.'; Refuse = 'Refusing to replace {0}. Use the existing installation to reinstall or uninstall, then remove the file after uninstall.'; Install = 'discodex install returned exit code {0}.' } }
}

if ($env:OS -ne 'Windows_NT') { throw $messages.Windows }
$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$target = switch ($architecture) {
    'X64' { 'x86_64-pc-windows-msvc' }
    'Arm64' { 'aarch64-pc-windows-msvc' }
    default { throw ($messages.Architecture -f $architecture) }
}

$dataRoot = Join-Path $env:LOCALAPPDATA 'discodex'
$binRoot = Join-Path $dataRoot 'bin'
New-Item -ItemType Directory -Force -Path $binRoot | Out-Null
$installed = Join-Path $binRoot 'discodex.exe'
if (Test-Path -LiteralPath $installed) { throw ($messages.Refuse -f $installed) }

$asset = "discodex-$target.exe"
$baseUrl = 'https://github.com/CHOAM-CO/Discodex/releases/latest/download'
$tempDirectory = Join-Path ([IO.Path]::GetTempPath()) ('discodex-install-' + [Guid]::NewGuid().ToString('N'))
$staged = Join-Path $binRoot ('.discodex.installing-' + [Guid]::NewGuid().ToString('N') + '.exe')
New-Item -ItemType Directory -Path $tempDirectory | Out-Null
try {
    $binaryPath = Join-Path $tempDirectory $asset
    $checksumPath = Join-Path $tempDirectory ($asset + '.sha256')
    try {
        Invoke-WebRequest -Uri "$baseUrl/$asset" -OutFile $binaryPath
        Invoke-WebRequest -Uri "$baseUrl/$asset.sha256" -OutFile $checksumPath
    } catch {
        throw $messages.Download
    }

    $checksumText = [IO.File]::ReadAllText($checksumPath).Trim()
    $expected = ($checksumText -split '\s+')[0]
    if ($expected -notmatch '^[0-9a-fA-F]{64}$') { throw 'The release checksum file is invalid.' }
    $actual = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    if (-not [string]::Equals($expected, $actual, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The downloaded release binary failed SHA256 verification.'
    }

    Copy-Item -LiteralPath $binaryPath -Destination $staged
    [System.IO.File]::Move($staged, $installed)
    # The prebuilt installer selects an existing or separate Codex data home.
    # It fetches the matching runtime; no build toolchain is needed on this host.
    & $installed install
    if ($LASTEXITCODE -ne 0) { throw ($messages.Install -f $LASTEXITCODE) }
} finally {
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $tempDirectory -Recurse -Force -ErrorAction SilentlyContinue
}
