#!/bin/sh
set -eu

if [ "${1:-}" = --update ]; then
  SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
  shift
  exec sh "$SCRIPT_DIR/update.sh" "$@"
fi

if [ -z "${DISCODEX_LANGUAGE:-}" ]; then
  printf '%s' 'Choose language: [en] English (default), [ko] 한국어, [zh] 简体中文, [ja] 日本語: '
  IFS= read -r DISCODEX_LANGUAGE || DISCODEX_LANGUAGE=
  DISCODEX_LANGUAGE=${DISCODEX_LANGUAGE:-en}
fi
case "$DISCODEX_LANGUAGE" in
  en|ko|zh|ja) export DISCODEX_LANGUAGE ;;
  *) printf '%s\n' 'Set DISCODEX_LANGUAGE to en, ko, zh, or ja.' >&2; exit 2 ;;
esac

msg() {
  key=$1
  shift
  case "$key:$DISCODEX_LANGUAGE" in
    unsupported:en) printf 'This source bootstrap supports Linux and macOS x86_64/aarch64; detected %s %s.\n' "$@" ;;
    unsupported:ko) printf '이 설치 bootstrap은 Linux/macOS x86_64/aarch64를 지원합니다. 감지한 환경: %s %s.\n' "$@" ;;
    unsupported:zh) printf '此安装引导程序支持 Linux 和 macOS x86_64/aarch64；检测到：%s %s。\n' "$@" ;;
    unsupported:ja) printf 'このインストール用ブートストラップは Linux/macOS x86_64/aarch64 に対応しています。検出値: %s %s。\n' "$@" ;;
    cargo:en) printf '%s\n' 'Rust Cargo is required. Install Rust with the official rustup instructions, reopen the shell, and rerun this script.' ;;
    cargo:ko) printf '%s\n' 'Rust Cargo가 필요합니다. 공식 rustup 안내에 따라 Rust를 설치하고 셸을 다시 연 뒤 이 스크립트를 실행하세요.' ;;
    cargo:zh) printf '%s\n' '需要 Rust Cargo。请按照 rustup 官方说明安装 Rust，重新打开 shell，然后再次运行此脚本。' ;;
    cargo:ja) printf '%s\n' 'Rust Cargo が必要です。rustup の公式手順で Rust をインストールし、シェルを開き直してから再実行してください。' ;;
    help:en) printf '%s\n' './install (new installation)' './update --config <profile-config> (update)' './install --update --config <profile-config> (update alias)' ;;
    help:ko) printf '%s\n' './install (새 설치)' './update --config <profile-config> (업데이트)' './install --update --config <profile-config> (업데이트 별칭)' ;;
    help:zh) printf '%s\n' './install（新安装）' './update --config <profile-config>（更新）' './install --update --config <profile-config>（更新别名）' ;;
    help:ja) printf '%s\n' './install（新規インストール）' './update --config <profile-config>（更新）' './install --update --config <profile-config>（更新エイリアス）' ;;
    unknown:en) printf '%s\n' 'Unknown install option; use ./install --help.' ;;
    unknown:ko) printf '%s\n' '알 수 없는 설치 옵션입니다. ./install --help를 사용하세요.' ;;
    unknown:zh) printf '%s\n' '未知的安装选项；请使用 ./install --help。' ;;
    unknown:ja) printf '%s\n' '不明なインストールオプションです。./install --help を使用してください。' ;;
    refuse:en) printf 'Refusing to replace %s. Use the existing binary for install/reinstall/uninstall, or remove it after its owned installation is uninstalled.\n' "$1" ;;
    refuse:ko) printf '%s 파일을 덮어쓰지 않습니다. 기존 소유 설치로 재설치·제거하거나 해당 설치를 제거한 뒤 파일을 정리하세요.\n' "$1" ;;
    refuse:zh) printf '不会覆盖文件 %s。请使用现有安装执行重新安装或卸载，之后再清理文件。\n' "$1" ;;
    refuse:ja) printf 'ファイル %s は上書きしません。既存のインストールから再インストールまたは削除し、その後ファイルを整理してください。\n' "$1" ;;
    raced:en) printf 'Refusing to replace %s; the destination appeared during installation.\n' "$1" ;;
    raced:ko) printf '설치 중 대상 경로 %s가 생겨 덮어쓰지 않습니다.\n' "$1" ;;
    raced:zh) printf '安装过程中目标路径 %s 已出现，不会覆盖。\n' "$1" ;;
    raced:ja) printf 'インストール中に保存先 %s が作成されたため、上書きしません。\n' "$1" ;;
  esac
}

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:aarch64|Linux:arm64|Darwin:x86_64|Darwin:arm64|Darwin:aarch64) ;;
  *) msg unsupported "$(uname -s)" "$(uname -m)" >&2; exit 2 ;;
esac

case "${1:-}" in
  --help|-h) msg help; exit 0 ;;
  '') ;;
  *) msg unknown >&2; exit 2 ;;
esac
command -v cargo >/dev/null 2>&1 || {
  msg cargo >&2
  exit 2
}

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPOSITORY_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
BUILD_ROOT=${CARGO_TARGET_DIR:-"$REPOSITORY_ROOT/target"}
cargo build --locked --release --manifest-path "$REPOSITORY_ROOT/Cargo.toml" --target-dir "$BUILD_ROOT"

DATA_ROOT=${XDG_DATA_HOME:-"${HOME:?HOME is not set}/.local/share"}/discodex
BIN_ROOT="$DATA_ROOT/bin"
mkdir -p "$BIN_ROOT"
chmod 700 "$DATA_ROOT" "$BIN_ROOT"
INSTALLED="$BIN_ROOT/discodex"
if [ -e "$INSTALLED" ]; then
  msg refuse "$INSTALLED" >&2
  exit 2
fi
STAGED="$BIN_ROOT/.discodex.installing"
cp "$BUILD_ROOT/release/discodex" "$STAGED"
chmod 700 "$STAGED"
trap 'rm -f "$STAGED"' EXIT HUP INT TERM
if [ "$(uname -s)" = Darwin ]; then
  "$BUILD_ROOT/release/discodex" mac-sign --binary "$STAGED"
fi
if ! ln "$STAGED" "$INSTALLED"; then
  rm -f "$STAGED"
  msg raced "$INSTALLED" >&2
  exit 2
fi
rm "$STAGED"
exec "$INSTALLED" install
