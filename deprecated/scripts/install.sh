#!/bin/sh
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$SCRIPT_DIR/product.generated.sh"

if [ "${1:-}" = --update ]; then
  shift
  exec sh "$SCRIPT_DIR/update.sh" "$@"
fi

if [ -z "${DISCODEX_LANGUAGE:-}" ]; then
  discodex_message install.language_prompt en
  IFS= read -r DISCODEX_LANGUAGE || DISCODEX_LANGUAGE=
  DISCODEX_LANGUAGE=${DISCODEX_LANGUAGE:-en}
fi
case " $DISCODEX_SUPPORTED_LANGUAGES " in
  *" $DISCODEX_LANGUAGE "*) export DISCODEX_LANGUAGE ;;
  *) discodex_message install.language_environment_error en >&2; exit 2 ;;
esac

msg() {
  key=$1
  shift
  case "$key" in
    unsupported) key=install.unsupported_platform ;;
    download) key=install.curl_required ;;
    help) key=install.help ;;
    unknown) key=install.unknown_option ;;
    refuse) key=install.refuse_replace ;;
    raced) key=install.destination_appeared ;;
  esac
  discodex_message "$key" "$DISCODEX_LANGUAGE" "$@"
}

OS=$(uname -s)
ARCH=$(uname -m)
TARGET=$(discodex_platform_target "$OS" "$ARCH") || {
  msg unsupported "$OS" "$ARCH" >&2
  exit 2
}

case "${1:-}" in
  --help|-h) msg help; exit 0 ;;
  '') ;;
  *) msg unknown >&2; exit 2 ;;
esac
command -v curl >/dev/null 2>&1 || { msg download >&2; exit 2; }

DATA_ROOT=${XDG_DATA_HOME:-"${HOME:?HOME is not set}/.local/share"}/$DISCODEX_DATA_DIRECTORY
BIN_ROOT="$DATA_ROOT/$DISCODEX_BINARY_DIRECTORY"
mkdir -p "$BIN_ROOT"
chmod 700 "$DATA_ROOT" "$BIN_ROOT"
INSTALLED="$BIN_ROOT/$DISCODEX_PRODUCT_NAME"
if [ -e "$INSTALLED" ]; then
  msg refuse "$INSTALLED" >&2
  exit 2
fi

TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/$DISCODEX_INSTALL_TEMP_PREFIX.XXXXXX")
STAGED=
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  cleanup_failed=0
  if [ -n "$STAGED" ]; then rm -f "$STAGED" || cleanup_failed=1; fi
  rm -rf "$TEMP_DIR" || cleanup_failed=1
  if [ "$status" -eq 0 ] && [ "$cleanup_failed" -ne 0 ]; then status=1; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
STAGED=$(mktemp "$BIN_ROOT/$DISCODEX_INSTALL_STAGING_PREFIX.XXXXXX")
ASSET=$(discodex_platform_asset "$OS" "$ARCH")
BASE_URL="$DISCODEX_RELEASE_DOWNLOAD_ROOT/latest/download"
BINARY="$TEMP_DIR/$ASSET"
CHECKSUM="$TEMP_DIR/$ASSET.sha256"
curl --fail --location --silent --show-error --retry 3 "$BASE_URL/$ASSET" --output "$BINARY"
curl --fail --location --silent --show-error --retry 3 "$BASE_URL/$ASSET.sha256" --output "$CHECKSUM"
EXPECTED=$(awk 'NR == 1 { print $1 }' "$CHECKSUM")
case "$EXPECTED" in
  ''|*[!0123456789abcdefABCDEF]*) msg install.invalid_checksum >&2; exit 1 ;;
esac
[ "${#EXPECTED}" -eq 64 ] || { msg install.invalid_checksum >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$BINARY" | awk '{ print $1 }')
else
  ACTUAL=$(shasum -a 256 "$BINARY" | awk '{ print $1 }')
fi
[ "$(printf '%s' "$EXPECTED" | tr '[:upper:]' '[:lower:]')" = "$(printf '%s' "$ACTUAL" | tr '[:upper:]' '[:lower:]')" ] || {
  msg install.checksum_mismatch >&2
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
# The prebuilt installer selects shared or separate Codex data and optional AGY
# settings. It downloads owned runtimes and offers cancellable AGY sign-in;
# no global AGY installation or compiler is required on this host.
rm -f "$STAGED"
rm -rf "$TEMP_DIR"
trap - EXIT HUP INT TERM
exec "$INSTALLED" install
