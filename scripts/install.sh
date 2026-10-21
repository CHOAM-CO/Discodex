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
    unsupported:en) printf 'No prebuilt Discodex release is available for %s %s.\n' "$@" ;;
    unsupported:ko) printf '감지한 환경(%s %s)에 사용할 Discodex 사전 빌드 릴리스가 없습니다.\n' "$@" ;;
    unsupported:zh) printf '没有适用于检测到的环境 %s %s 的 Discodex 预构建版本。\n' "$@" ;;
    unsupported:ja) printf '検出された環境 %s %s 用の Discodex ビルド済みリリースはありません。\n' "$@" ;;
    download:en) printf '%s\n' 'curl is required to download the official Discodex release.' ;;
    download:ko) printf '%s\n' '공식 Discodex 릴리스를 다운로드하려면 curl이 필요합니다.' ;;
    download:zh) printf '%s\n' '下载 Discodex 官方预构建版本需要 curl。' ;;
    download:ja) printf '%s\n' 'Discodex 公式ビルド済みリリースのダウンロードには curl が必要です。' ;;
    help:en) printf '%s\n' './install (new installation)' './update --config <profile-config> (update)' './install --update --config <profile-config> (update alias)' ;;
    help:ko) printf '%s\n' './install (새 설치)' './update --config <profile-config> (업데이트)' './install --update --config <profile-config> (업데이트 별칭)' ;;
    help:zh) printf '%s\n' './install（新安装）' './update --config <profile-config>（更新）' './install --update --config <profile-config>（更新别名）' ;;
    help:ja) printf '%s\n' './install（新規インストール）' './update --config <profile-config>（更新）' './install --update --config <profile-config>（更新別名）' ;;
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
  Linux:x86_64|Linux:amd64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux:aarch64|Linux:arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin:x86_64|Darwin:amd64) TARGET=x86_64-apple-darwin ;;
  Darwin:arm64|Darwin:aarch64) TARGET=aarch64-apple-darwin ;;
  *) msg unsupported "$(uname -s)" "$(uname -m)" >&2; exit 2 ;;
esac

case "${1:-}" in
  --help|-h) msg help; exit 0 ;;
  '') ;;
  *) msg unknown >&2; exit 2 ;;
esac
command -v curl >/dev/null 2>&1 || { msg download >&2; exit 2; }

DATA_ROOT=${XDG_DATA_HOME:-"${HOME:?HOME is not set}/.local/share"}/discodex
BIN_ROOT="$DATA_ROOT/bin"
mkdir -p "$BIN_ROOT"
chmod 700 "$DATA_ROOT" "$BIN_ROOT"
INSTALLED="$BIN_ROOT/discodex"
if [ -e "$INSTALLED" ]; then
  msg refuse "$INSTALLED" >&2
  exit 2
fi

TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/discodex-install.XXXXXX")
STAGED=$(mktemp "$BIN_ROOT/.discodex.installing.XXXXXX")
cleanup() { rm -f "$STAGED"; rm -rf "$TEMP_DIR"; }
trap cleanup EXIT HUP INT TERM
ASSET="discodex-${TARGET}"
BASE_URL="https://github.com/CHOAM-CO/Discodex/releases/latest/download"
BINARY="$TEMP_DIR/$ASSET"
CHECKSUM="$TEMP_DIR/$ASSET.sha256"
curl --fail --location --silent --show-error --retry 3 "$BASE_URL/$ASSET" --output "$BINARY"
curl --fail --location --silent --show-error --retry 3 "$BASE_URL/$ASSET.sha256" --output "$CHECKSUM"
EXPECTED=$(awk 'NR == 1 { print $1 }' "$CHECKSUM")
case "$EXPECTED" in
  ''|*[!0123456789abcdefABCDEF]*) printf '%s\n' 'The release checksum file is invalid.' >&2; exit 1 ;;
esac
[ "${#EXPECTED}" -eq 64 ] || { printf '%s\n' 'The release checksum file is invalid.' >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$BINARY" | awk '{ print $1 }')
else
  ACTUAL=$(shasum -a 256 "$BINARY" | awk '{ print $1 }')
fi
[ "$(printf '%s' "$EXPECTED" | tr '[:upper:]' '[:lower:]')" = "$(printf '%s' "$ACTUAL" | tr '[:upper:]' '[:lower:]')" ] || {
  printf '%s\n' 'The downloaded release binary failed SHA256 verification.' >&2
  exit 1
}
cp "$BINARY" "$STAGED"
chmod 700 "$STAGED"
if [ "$(uname -s)" = Darwin ]; then
  "$STAGED" mac-sign --binary "$STAGED"
fi
if ! ln "$STAGED" "$INSTALLED"; then
  msg raced "$INSTALLED" >&2
  exit 2
fi
rm -f "$STAGED"
rm -rf "$TEMP_DIR"
trap - EXIT HUP INT TERM
# The prebuilt installer selects shared or separate Codex data and optional AGY
# settings. It downloads owned runtimes and offers cancellable AGY sign-in;
# no global AGY installation or compiler is required on this host.
exec "$INSTALLED" install
