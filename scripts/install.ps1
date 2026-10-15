[CmdletBinding()]
param(
    [switch]$Update,
    [string]$Config,
    [switch]$MigrateAdhoc
)

$ErrorActionPreference = 'Stop'
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
    'ko' { @{ Windows = '이 설치 bootstrap은 Windows 전용입니다.'; Architecture = '이 설치 bootstrap은 Windows x86_64/aarch64를 지원합니다. 감지한 환경: {0}.'; Cargo = 'Rust Cargo가 필요합니다. 공식 rustup 안내에 따라 Rust를 설치하고 PowerShell을 다시 연 뒤 이 스크립트를 실행하세요.'; Build = '패키지 빌드에 실패했습니다 (종료 코드 {0}).'; Refuse = '기존 {0} 파일을 덮어쓰지 않습니다. 소유 설치를 통해 재설치·제거하거나, 기존 설치 제거 후 파일을 정리하세요.'; Install = 'discodex install이 종료 코드 {0}을 반환했습니다.' } }
    'zh' { @{ Windows = '此安装引导程序仅适用于 Windows。'; Architecture = '此安装引导程序支持 Windows x86_64/aarch64；检测到：{0}。'; Cargo = '需要 Rust Cargo。请按照 rustup 官方说明安装 Rust，重新打开 PowerShell，然后再次运行此脚本。'; Build = '软件包构建失败，退出代码为 {0}。'; Refuse = '不会覆盖现有文件 {0}。请使用该安装执行重新安装或卸载，之后再清理文件。'; Install = 'discodex install 返回退出代码 {0}。' } }
    'ja' { @{ Windows = 'このインストール用ブートストラップは Windows 専用です。'; Architecture = 'このブートストラップは Windows x86_64/aarch64 に対応しています。検出値: {0}。'; Cargo = 'Rust Cargo が必要です。rustup の公式手順で Rust をインストールし、PowerShell を開き直してから再実行してください。'; Build = 'パッケージのビルドに失敗しました（終了コード {0}）。'; Refuse = '既存ファイル {0} は上書きしません。既存のインストールから再インストールまたは削除し、その後ファイルを整理してください。'; Install = 'discodex install が終了コード {0} を返しました。' } }
    default { @{ Windows = 'This source bootstrap is for Windows.'; Architecture = 'This source bootstrap supports Windows x86_64/aarch64; detected {0}.'; Cargo = 'Rust Cargo is required. Install Rust with the official rustup instructions, reopen PowerShell, and rerun this script.'; Build = 'The package build failed with exit code {0}.'; Refuse = 'Refusing to replace {0}. Use the existing installation to reinstall or uninstall, then remove the file after uninstall.'; Install = 'discodex install returned exit code {0}.' } }
}

if ($env:OS -ne 'Windows_NT') {
    throw $messages.Windows
}
$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($architecture -notin @('X64', 'Arm64')) {
    throw ($messages.Architecture -f $architecture)
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw $messages.Cargo
}

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$manifest = Join-Path $repositoryRoot 'Cargo.toml'
cargo build --locked --release --manifest-path $manifest --target-dir (Join-Path $repositoryRoot 'target')
if ($LASTEXITCODE -ne 0) {
    throw ($messages.Build -f $LASTEXITCODE)
}

$dataRoot = Join-Path $env:LOCALAPPDATA 'discodex'
$binRoot = Join-Path $dataRoot 'bin'
New-Item -ItemType Directory -Force -Path $binRoot | Out-Null
$staged = Join-Path $binRoot ("discodex-" + [guid]::NewGuid().ToString('N') + '.exe')
$sourceBinary = Join-Path $repositoryRoot 'target\release\discodex.exe'
$installed = Join-Path $binRoot 'discodex.exe'
if (Test-Path -LiteralPath $installed) {
    throw ($messages.Refuse -f $installed)
}
Copy-Item -LiteralPath $sourceBinary -Destination $staged
[System.IO.File]::Move($staged, $installed)
& $installed install
if ($LASTEXITCODE -ne 0) {
    throw ($messages.Install -f $LASTEXITCODE)
}
