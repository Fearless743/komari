#!/usr/bin/env bash
#
# Build the `komari` binary with Go-compatible version injection.
#
# The Go backend injects `utils.CurrentVersion` and `utils.VersionHash` with
# `-ldflags "-X ..."`. The Rust port reads the same values from the
# `KOMARI_VERSION` / `KOMARI_VERSION_HASH` environment variables at compile
# time (see crates/komari-cli/src/version.rs).
#
# The default theme is embedded at compile time by `komari-web` (B14), which
# reads `KOMARI_DEFAULT_THEME_DIR`. Point that variable at a normalized theme
# directory (`dist/`, `komari-theme.json`, `preview.png`) for a release build;
# when it is unset B14 embeds its committed placeholder.
#
# Usage:
#   scripts/build.sh
#   KOMARI_VERSION=9.9.9 KOMARI_VERSION_HASH=abc scripts/build.sh
#   KOMARI_DEFAULT_THEME_DIR=/path/to/defaultTheme scripts/build.sh
#   TARGET=aarch64-unknown-linux-musl scripts/build.sh
#
# Environment:
#   KOMARI_VERSION             Version string (default: git describe)
#   KOMARI_VERSION_HASH        Commit hash (default: git rev-parse HEAD)
#   KOMARI_DEFAULT_THEME_DIR   Theme directory embedded by komari-web
#   PACKAGE                    Optional package; default builds --workspace
#   PROFILE                    Cargo profile: release (default) or dev
#   TARGET                     Optional Rust target triple
#   CROSS                      Optional build command override (e.g. cargo/cross)
#   OUT_DIR                    Output directory (default: dist)
#   CARGO                      Cargo executable (default: cargo)

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CARGO="${CARGO:-cargo}"
PACKAGE="${PACKAGE:-}"
PROFILE="${PROFILE:-release}"
TARGET="${TARGET:-}"
CROSS="${CROSS:-}"
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

# Match the Go release build's `-ldflags "-s -w"` (strip symbols).
if [[ "$PROFILE" == "release" ]]; then
  export CARGO_PROFILE_RELEASE_STRIP="${CARGO_PROFILE_RELEASE_STRIP:-symbols}"
fi

# `option_env!` values are not tracked by Cargo's fingerprint, so touch the
# file that reads them to make sure the new version is compiled in.
touch crates/komari-cli/src/version.rs

HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"

# cross is only needed for Linux musl targets. Same-OS cross builds (for
# example macOS x86_64 on Apple Silicon) use the native cargo/linker, matching
# the CI matrix's use_cross:false entries.
USE_CROSS=false
if [[ -n "$TARGET" && "$TARGET" != "$HOST_TARGET" ]]; then
  case "$TARGET" in
    *-unknown-linux-musl) USE_CROSS=true ;;
  esac
fi

if [[ -n "$CROSS" ]]; then
  BUILD_CMD="$CROSS"
elif [[ "$USE_CROSS" == true ]]; then
  if command -v cross >/dev/null 2>&1; then
    BUILD_CMD="cross"
  else
    echo "error: cross-compiling to $TARGET requires 'cross' (or set CROSS)." >&2
    echo "       Install it with: cargo install cross --locked" >&2
    exit 1
  fi
else
  BUILD_CMD="$CARGO"
fi
BUILD_CMD_NAME="$(basename "$BUILD_CMD")"

# Resolve the theme to an absolute path for validation and marker checks.
THEME_DIR=""
if [[ -n "${KOMARI_DEFAULT_THEME_DIR:-}" ]]; then
  THEME_DIR="$KOMARI_DEFAULT_THEME_DIR"
  if [[ "$THEME_DIR" != /* ]]; then
    THEME_DIR="$ROOT_DIR/$THEME_DIR"
  fi
  if [[ ! -d "$THEME_DIR" ]]; then
    echo "error: KOMARI_DEFAULT_THEME_DIR is not a directory: $KOMARI_DEFAULT_THEME_DIR" >&2
    exit 1
  fi
  THEME_DIR="$(cd "$THEME_DIR" && pwd)"

  if [[ "$BUILD_CMD_NAME" == "cross" ]]; then
    # cross mounts the repository at /project and B14 resolves relative theme
    # paths against crates/komari-web. An absolute host path is not visible
    # inside the container, so pass a repo-relative path instead.
    case "$THEME_DIR" in
      "$ROOT_DIR"/*) ;;
      *)
        echo "error: cross builds require KOMARI_DEFAULT_THEME_DIR inside the repository." >&2
        echo "       Copy the theme into the repo (for example .komari-theme) and retry." >&2
        exit 1
        ;;
    esac
    if command -v python3 >/dev/null 2>&1; then
      REL_THEME="$(python3 -c 'import os, sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' \
        "$THEME_DIR" "$ROOT_DIR/crates/komari-web")"
    elif realpath --relative-to="$ROOT_DIR/crates/komari-web" "$THEME_DIR" >/dev/null 2>&1; then
      REL_THEME="$(realpath --relative-to="$ROOT_DIR/crates/komari-web" "$THEME_DIR")"
    else
      echo "error: cannot compute a relative theme path for cross; install python3 or realpath." >&2
      exit 1
    fi
    export KOMARI_DEFAULT_THEME_DIR="$REL_THEME"
  else
    export KOMARI_DEFAULT_THEME_DIR="$THEME_DIR"
  fi
fi

build_args=(build --locked --profile "$PROFILE")
if [[ -n "$PACKAGE" ]]; then
  build_args+=(-p "$PACKAGE")
else
  # Building the workspace compiles komari-web (B14) and embeds the theme.
  build_args+=(--workspace)
fi
if [[ -n "$TARGET" ]]; then
  build_args+=(--target "$TARGET")
fi

"$BUILD_CMD" "${build_args[@]}"

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

# Host strip only works for the host architecture. Cross builds rely on
# CARGO_PROFILE_RELEASE_STRIP (set above) and/or the cross container toolchain.
if [[ "$PROFILE" == "release" && "$BIN_NAME" != *.exe ]]; then
  if [[ -z "$TARGET" || "$TARGET" == "$HOST_TARGET" ]]; then
    if command -v strip >/dev/null 2>&1; then
      strip "$BINARY"
    else
      echo "warning: strip not found; binary may retain symbols" >&2
    fi
  else
    echo "note: not running host strip for cross target $TARGET" >&2
    echo "      relying on CARGO_PROFILE_RELEASE_STRIP=${CARGO_PROFILE_RELEASE_STRIP:-unset}" >&2
  fi
fi

mkdir -p "$OUT_DIR"
cp -f "$BINARY" "$OUT_DIR/$BIN_NAME"

echo "Built $OUT_DIR/$BIN_NAME"
echo "  version: $KOMARI_VERSION"
echo "  hash:    $KOMARI_VERSION_HASH"
if [[ -n "$THEME_DIR" ]]; then
  echo "  theme:   $THEME_DIR"
  MARKER=""
  if [[ -f "$THEME_DIR/dist/index.html" ]]; then
    MARKER="$(grep -oE 'assets/[A-Za-z0-9._-]+\.(js|css)' "$THEME_DIR/dist/index.html" | head -n1 || true)"
  fi
  if [[ -n "$MARKER" ]] && grep -a -q "$MARKER" "$BINARY"; then
    echo "  theme:   linked into the binary"
  else
    echo "  warning: theme is embedded in komari-web but not linked into komari-cli yet (B09);" >&2
    echo "           this binary does not contain the UI." >&2
  fi
else
  echo "  theme:   B14 placeholder (set KOMARI_DEFAULT_THEME_DIR for a real UI)"
  if [[ ! -f crates/komari-web/build.rs ]]; then
    echo "  warning: crates/komari-web/build.rs is missing (B14 not merged)" >&2
  else
    PLACEHOLDER_THEME="crates/komari-web/defaultTheme"
    MARKER=""
    if [[ -f "$PLACEHOLDER_THEME/dist/index.html" ]]; then
      MARKER="$(grep -oE 'assets/[A-Za-z0-9._-]+\.(js|css)' \
        "$PLACEHOLDER_THEME/dist/index.html" | head -n1 || true)"
    fi
    if [[ -n "$MARKER" ]] && grep -a -q "$MARKER" "$BINARY"; then
      echo "  theme:   placeholder linked into the binary"
    else
      echo "  warning: komari-cli does not link komari-web until B09;" >&2
      echo "           this binary does not contain the UI." >&2
    fi
  fi
fi
