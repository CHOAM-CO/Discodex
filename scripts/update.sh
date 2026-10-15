#!/bin/sh
set -eu

usage() {
  cat <<'USAGE'
Usage: ./update --config <existing-profile-config.json> [--migrate-adhoc]

Updates the source checkout with a fast-forward-only pull, builds the release binary,
and asks that binary to update the explicitly selected profile.
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

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPOSITORY_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
command -v git >/dev/null 2>&1 || { printf '%s\n' 'Git is required to update this source checkout.' >&2; exit 2; }
command -v cargo >/dev/null 2>&1 || { printf '%s\n' 'Rust Cargo is required to build Discodex.' >&2; exit 2; }

git -C "$REPOSITORY_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
  printf '%s\n' 'The update script must run from a Git source checkout.' >&2
  exit 2
}
if ! git -C "$REPOSITORY_ROOT" diff --quiet HEAD --; then
  printf '%s\n' 'Tracked source changes are present. Commit, stash, or otherwise preserve them before updating.' >&2
  exit 2
fi

# A fast-forward-only pull never rewrites a divergent local branch.
git -C "$REPOSITORY_ROOT" pull --ff-only

BUILD_ROOT=${CARGO_TARGET_DIR:-"$REPOSITORY_ROOT/target"}
cargo build --locked --release --manifest-path "$REPOSITORY_ROOT/Cargo.toml" --target-dir "$BUILD_ROOT"
SOURCE_BINARY="$BUILD_ROOT/release/discodex"
if [ ! -x "$SOURCE_BINARY" ]; then
  printf 'Release binary was not produced: %s\n' "$SOURCE_BINARY" >&2
  exit 1
fi

set -- update --source "$SOURCE_BINARY" --config "$CONFIG"
if [ "$MIGRATE_ADHOC" -eq 1 ]; then
  set -- "$@" --migrate-adhoc
fi
exec "$SOURCE_BINARY" "$@"
