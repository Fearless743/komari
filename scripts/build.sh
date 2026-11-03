#!/usr/bin/env bash
#
# Build the `komari` binary with Go-compatible version injection.
#
# The Go backend injects `utils.CurrentVersion` and `utils.VersionHash` with
# `-ldflags "-X ..."`. The Rust port reads the same values from the
# `KOMARI_VERSION` / `KOMARI_VERSION_HASH` environment variables at compile
# time (see crates/komari-cli/src/version.rs).
#
# Usage:
#   scripts/build.sh                       # release build into dist/
#   KOMARI_VERSION=9.9.9 \
#   KOMARI_VERSION_HASH=abc scripts/build.sh
#   TARGET=aarch64-unknown-linux-musl scripts/build.sh
#
# Environment:
#   KOMARI_VERSION       Version string (default: git describe)
#   KOMARI_VERSION_HASH  Commit hash (default: git rev-parse HEAD)
#   PACKAGE              Cargo package (default: komari-cli)
#   PROFILE              Cargo profile: release (default) or dev
#   TARGET               Optional Rust target triple for cross builds
#   OUT_DIR              Output directory (default: dist)
#   CARGO                Cargo executable (default: cargo)

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CARGO="${CARGO:-cargo}"
PACKAGE="${PACKAGE:-komari-cli}"
PROFILE="${PROFILE:-release}"
TARGET="${TARGET:-}"
OUT_DIR="${OUT_DIR:-dist}"

if [[ -n "${KOMARI_VERSION:-}" ]]; then
  VERSION="$KOMARI_VERSION"
elif command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
  VERSION="$(git describe --tags --always --dirty 2>/dev/null || echo dev)"
else
  VERSION="dev"
fi

if [[ -n "${KOMARI_VERSION_HASH:-}" ]]; then
  VERSION_HASH="$KOMARI_VERSION_HASH"
elif command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
  VERSION_HASH="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
else
  VERSION_HASH="unknown"
fi

export KOMARI_VERSION="$VERSION"
export KOMARI_VERSION_HASH="$VERSION_HASH"

# `option_env!` values are not tracked by Cargo's fingerprint, so touch the
# file that reads them to make sure the new version is compiled in.
touch crates/komari-cli/src/version.rs

build_args=(build --locked -p "$PACKAGE" --profile "$PROFILE")
if [[ -n "$TARGET" ]]; then
  build_args+=(--target "$TARGET")
fi

"$CARGO" "${build_args[@]}"

case "$PROFILE" in
  dev | debug) PROFILE_DIR="debug" ;;
  release) PROFILE_DIR="release" ;;
  *) PROFILE_DIR="$PROFILE" ;;
esac

BIN_NAME="komari"
case "$TARGET" in
  *windows*) BIN_NAME="komari.exe" ;;
esac

TARGET_DIR="target"
if [[ -n "$TARGET" ]]; then
  TARGET_DIR="target/$TARGET"
fi
BINARY="$TARGET_DIR/$PROFILE_DIR/$BIN_NAME"

if [[ ! -f "$BINARY" ]]; then
  echo "error: expected binary not found: $BINARY" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"
cp -f "$BINARY" "$OUT_DIR/$BIN_NAME"

echo "Built $OUT_DIR/$BIN_NAME"
echo "  version: $KOMARI_VERSION"
echo "  hash:    $KOMARI_VERSION_HASH"
