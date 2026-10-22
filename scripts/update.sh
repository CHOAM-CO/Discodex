#!/bin/sh
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$SCRIPT_DIR/product.generated.sh"
DISCODEX_LANGUAGE=${DISCODEX_LANGUAGE:-en}
case " $DISCODEX_SUPPORTED_LANGUAGES " in
  *" $DISCODEX_LANGUAGE "*) ;;
  *) discodex_message install.language_environment_error en >&2; exit 2 ;;
esac

usage() {
  discodex_message update.usage "$DISCODEX_LANGUAGE"
}

CONFIG=
MIGRATE_ADHOC=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --config)
      [ "$#" -ge 2 ] || { discodex_message update.missing_config_value "$DISCODEX_LANGUAGE" >&2; usage >&2; exit 2; }
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
      discodex_message update.unknown_option "$DISCODEX_LANGUAGE" "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[ -n "$CONFIG" ] || { discodex_message update.missing_config "$DISCODEX_LANGUAGE" >&2; usage >&2; exit 2; }
[ -f "$CONFIG" ] || { discodex_message update.config_not_found "$DISCODEX_LANGUAGE" "$CONFIG" >&2; exit 2; }
command -v curl >/dev/null 2>&1 || { discodex_message install.curl_required "$DISCODEX_LANGUAGE" >&2; exit 2; }

OS=$(uname -s)
ARCH=$(uname -m)
TARGET=$(discodex_platform_target "$OS" "$ARCH") || {
  discodex_message update.unsupported_platform "$DISCODEX_LANGUAGE" "$OS" "$ARCH" >&2
  exit 2
}

ASSET=$(discodex_platform_asset "$OS" "$ARCH")
BASE_URL="$DISCODEX_RELEASE_DOWNLOAD_ROOT/latest/download"
TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/$DISCODEX_UPDATE_TEMP_PREFIX.XXXXXX")
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if ! rm -rf "$TEMP_DIR" && [ "$status" -eq 0 ]; then status=1; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
BINARY="$TEMP_DIR/$ASSET"
CHECKSUM="$TEMP_DIR/$ASSET.sha256"
curl --fail --location --silent --show-error --retry 3 "$BASE_URL/$ASSET" --output "$BINARY"
curl --fail --location --silent --show-error --retry 3 "$BASE_URL/$ASSET.sha256" --output "$CHECKSUM"
EXPECTED=$(awk 'NR == 1 { print $1 }' "$CHECKSUM")
case "$EXPECTED" in
  ''|*[!0123456789abcdefABCDEF]*) discodex_message install.invalid_checksum "$DISCODEX_LANGUAGE" >&2; exit 1 ;;
esac
[ "${#EXPECTED}" -eq 64 ] || { discodex_message install.invalid_checksum "$DISCODEX_LANGUAGE" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL=$(sha256sum "$BINARY" | awk '{ print $1 }')
else
  ACTUAL=$(shasum -a 256 "$BINARY" | awk '{ print $1 }')
fi
[ "$(printf '%s' "$EXPECTED" | tr '[:upper:]' '[:lower:]')" = "$(printf '%s' "$ACTUAL" | tr '[:upper:]' '[:lower:]')" ] || {
  discodex_message install.checksum_mismatch "$DISCODEX_LANGUAGE" >&2
  exit 1
}
chmod +x "$BINARY"
set -- update --source "$BINARY" --config "$CONFIG"
if [ "$MIGRATE_ADHOC" -eq 1 ]; then set -- "$@" --migrate-adhoc; fi
"$BINARY" "$@"
