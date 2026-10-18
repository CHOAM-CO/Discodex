#!/bin/sh
set -eu

usage() {
  cat <<'USAGE'
Usage: ./update --config <existing-profile-config.json> [--migrate-adhoc]

Downloads the official Discodex release for this operating system and architecture,
then asks that binary to update the explicitly selected profile.
USAGE
}

CONFIG=
MIGRATE_ADHOC=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --config)
      [ "$#" -ge 2 ] || { printf '%s\n' 'Missing value for --config.' >&2; usage >&2; exit 2; }
      CONFIG=$2
      shift 2
      ;;
    --migrate-adhoc)
      MIGRATE_ADHOC=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown update option: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[ -n "$CONFIG" ] || { printf '%s\n' 'An existing profile config is required with --config.' >&2; usage >&2; exit 2; }
[ -f "$CONFIG" ] || { printf 'Config file not found: %s\n' "$CONFIG" >&2; exit 2; }
command -v curl >/dev/null 2>&1 || { printf '%s\n' 'curl is required to download the official Discodex release.' >&2; exit 2; }

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS:$ARCH" in
  Linux:x86_64|Linux:amd64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux:aarch64|Linux:arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin:x86_64|Darwin:amd64) TARGET=x86_64-apple-darwin ;;
  Darwin:arm64|Darwin:aarch64) TARGET=aarch64-apple-darwin ;;
  *) printf 'No prebuilt Discodex release is available for %s/%s.\n' "$OS" "$ARCH" >&2; exit 2 ;;
esac

ASSET="discodex-${TARGET}"
BASE_URL="https://github.com/CHOAM-CO/Discodex/releases/latest/download"
TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/discodex-update.XXXXXX")
cleanup() { rm -rf "$TEMP_DIR"; }
trap cleanup EXIT HUP INT TERM
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
chmod +x "$BINARY"
set -- update --source "$BINARY" --config "$CONFIG"
if [ "$MIGRATE_ADHOC" -eq 1 ]; then set -- "$@" --migrate-adhoc; fi
"$BINARY" "$@"
