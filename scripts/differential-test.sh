#!/usr/bin/env bash
#
# Differential test: Go reference backend vs Rust backend.
#
# Starts both servers in isolated temporary directories with a fresh SQLite
# database, seeds identical client/record rows, logs in as the administrator,
# replays the same HTTP/RPC matrix against both, normalizes volatile values and
# reports 一致 / 已知差异 / 差异 / 仅一侧.
#
# Usage:
#   scripts/differential-test.sh
#   scripts/differential-test.sh --keep
#   scripts/differential-test.sh --go-repo /path/to/go/komari
#   scripts/differential-test.sh --go-bin /path/to/komari --rust-bin target/debug/komari
#   scripts/differential-test.sh --skip-build --go-port 18080 --rust-port 18081
#
# Environment:
#   KOMARI_GO_REPO       Go reference checkout (auto-detected)
#   KOMARI_GO_BIN        Prebuilt Go binary (skips `go build`)
#   KOMARI_RUST_BIN      Prebuilt Rust binary (skips `cargo build`)
#   KOMARI_DIFF_WORKDIR  Use this directory instead of mktemp; implies --keep
#   KOMARI_ADMIN_USER    Test administrator name (default: admin)
#   KOMARI_ADMIN_PASS    Test administrator password (default: testpass)
#
# Exit status is non-zero when an unexpected difference or a one-sided endpoint
# remains. Known differences (DF-xxx) are documented in docs/DIFFERENTIAL.md
# and whitelisted in scripts/differential-known-differences.json.

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

GO_REPO="${KOMARI_GO_REPO:-}"
GO_BIN="${KOMARI_GO_BIN:-}"
RUST_BIN="${KOMARI_RUST_BIN:-}"
GO_PORT=""
RUST_PORT=""
ADMIN_USER="${KOMARI_ADMIN_USER:-admin}"
ADMIN_PASS="${KOMARI_ADMIN_PASS:-testpass}"
KEEP=0
SKIP_BUILD=0
NO_SEED=0
STRICT=0
REPORT_OUT=""
WORKDIR_OVERRIDE=""

usage() {
  sed -n '2,36p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --go-repo)
      GO_REPO="${2:?--go-repo requires a path}"
      shift 2
      ;;
    --go-bin)
      GO_BIN="${2:?--go-bin requires a path}"
      shift 2
      ;;
    --rust-bin)
      RUST_BIN="${2:?--rust-bin requires a path}"
      shift 2
      ;;
    --go-port)
      GO_PORT="${2:?--go-port requires a port}"
      shift 2
      ;;
    --rust-port)
      RUST_PORT="${2:?--rust-port requires a port}"
      shift 2
      ;;
    --admin-user)
      ADMIN_USER="${2:?--admin-user requires a value}"
      shift 2
      ;;
    --admin-pass)
      ADMIN_PASS="${2:?--admin-pass requires a value}"
      shift 2
      ;;
    --report-out)
      REPORT_OUT="${2:?--report-out requires a path}"
      shift 2
      ;;
    --workdir)
      WORKDIR_OVERRIDE="${2:?--workdir requires a path}"
      shift 2
      ;;
    --keep)
      KEEP=1
      shift
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --no-seed)
      NO_SEED=1
      shift
      ;;
    --strict)
      STRICT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 2
  fi
}

require_command curl
require_command python3
require_command cargo
if [[ -z "$GO_BIN" && "$SKIP_BUILD" -eq 0 ]]; then
  require_command go
fi

# ---------------------------------------------------------------------------
# Locate the Go reference checkout / binaries
# ---------------------------------------------------------------------------

detect_go_repo() {
  local candidate
  local -a candidates=()
  if [[ -n "${KOMARI_GO_REPO:-}" ]]; then
    candidates+=("$KOMARI_GO_REPO")
  fi
  candidates+=(
    "$ROOT_DIR/../komari"
    "$ROOT_DIR/../komari/komari"
    "$ROOT_DIR/../../komari/komari"
    "$ROOT_DIR/../../github/komari/komari"
    "/home/fearless/github/komari/komari"
  )
  for candidate in "${candidates[@]}"; do
    if [[ -f "$candidate/go.mod" && -f "$candidate/main.go" ]]; then
      (cd "$candidate" && pwd)
      return 0
    fi
  done
  return 1
}

if [[ -z "$GO_BIN" && -z "$GO_REPO" ]]; then
  if ! GO_REPO="$(detect_go_repo)"; then
    cat >&2 <<'EOF'
error: Go reference checkout not found.

Pass it explicitly:
  scripts/differential-test.sh --go-repo /path/to/go/komari

or set KOMARI_GO_REPO. A prebuilt binary can be supplied with
KOMARI_GO_BIN / --go-bin instead.
EOF
    exit 2
  fi
fi

if [[ -z "$RUST_BIN" ]]; then
  RUST_BIN="$ROOT_DIR/target/debug/komari"
fi

# ---------------------------------------------------------------------------
# Temporary workspace
# ---------------------------------------------------------------------------

if [[ -n "$WORKDIR_OVERRIDE" ]]; then
  WORK="$WORKDIR_OVERRIDE"
  mkdir -p "$WORK"
  KEEP=1
else
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/komari-diff.XXXXXX")"
fi
mkdir -p "$WORK/bin" "$WORK/go" "$WORK/rust"

GO_PID=""
RUST_PID=""

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n "$GO_PID" ]]; then
    stop_pid "$GO_PID"
  fi
  if [[ -n "$RUST_PID" ]]; then
    stop_pid "$RUST_PID"
  fi
  if [[ "$KEEP" -eq 1 ]]; then
    echo "workdir kept: $WORK" >&2
  else
    rm -rf "$WORK"
  fi
  exit "$status"
}

stop_pid() {
  local pid="$1"
  kill -0 "$pid" 2>/dev/null || return 0
  kill -TERM "$pid" 2>/dev/null || true
  local i
  for i in $(seq 1 50); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 0.1
  done
  kill -KILL "$pid" 2>/dev/null || true
}

trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

if [[ -z "$GO_BIN" ]]; then
  if [[ "$SKIP_BUILD" -eq 1 ]]; then
    GO_BIN="$GO_REPO/komari"
    if [[ ! -x "$GO_BIN" ]]; then
      echo "error: --skip-build requires $GO_BIN to exist, or pass --go-bin" >&2
      exit 2
    fi
  else
    echo "building Go reference backend from $GO_REPO ..."
    (
      cd "$GO_REPO"
      go build -o "$WORK/bin/komari-go" .
    )
    GO_BIN="$WORK/bin/komari-go"
  fi
fi

if [[ "$SKIP_BUILD" -eq 0 ]]; then
  echo "building Rust backend (cargo build -p komari-cli) ..."
  cargo build -p komari-cli
fi

if [[ ! -x "$RUST_BIN" ]]; then
  echo "error: Rust binary not executable: $RUST_BIN" >&2
  exit 2
fi
if [[ ! -x "$GO_BIN" ]]; then
  echo "error: Go binary not executable: $GO_BIN" >&2
  exit 2
fi

# Servers run with a temporary working directory; make both binary paths
# absolute so a relative --rust-bin/--go-bin still works.
RUST_BIN="$(cd -- "$(dirname -- "$RUST_BIN")" && pwd)/$(basename -- "$RUST_BIN")"
GO_BIN="$(cd -- "$(dirname -- "$GO_BIN")" && pwd)/$(basename -- "$GO_BIN")"

# ---------------------------------------------------------------------------
# Ports and readiness
# ---------------------------------------------------------------------------

pick_port() {
  python3 - <<'PY'
import socket
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
}

if [[ -z "$GO_PORT" ]]; then
  GO_PORT="$(pick_port)"
fi
if [[ -z "$RUST_PORT" ]]; then
  RUST_PORT="$(pick_port)"
fi
while [[ "$RUST_PORT" == "$GO_PORT" ]]; do
  RUST_PORT="$(pick_port)"
done

wait_ready() {
  local name="$1" port="$2" pid="$3" log="$4"
  local i
  for i in $(seq 1 150); do
    if curl -fsS --max-time 1 "http://127.0.0.1:$port/ping" >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "error: $name server exited before becoming ready" >&2
      echo "---- $log (tail) ----" >&2
      tail -n 40 "$log" >&2 || true
      return 1
    fi
    sleep 0.2
  done
  echo "error: $name server did not become ready on port $port" >&2
  echo "---- $log (tail) ----" >&2
  tail -n 40 "$log" >&2 || true
  return 1
}

echo "starting Go backend on 127.0.0.1:$GO_PORT (workdir $WORK/go) ..."
(
  cd "$WORK/go"
  ADMIN_USERNAME="$ADMIN_USER" \
  ADMIN_PASSWORD="$ADMIN_PASS" \
  KOMARI_DB_FILE="data/komari.db" \
  KOMARI_LISTEN="127.0.0.1:$GO_PORT" \
  KOMARI_LOG_LEVEL=error \
  TZ=UTC \
  "$GO_BIN" server
) >"$WORK/go/server.log" 2>&1 &
GO_PID=$!

echo "starting Rust backend on 127.0.0.1:$RUST_PORT (workdir $WORK/rust) ..."
(
  cd "$WORK/rust"
  ADMIN_USERNAME="$ADMIN_USER" \
  ADMIN_PASSWORD="$ADMIN_PASS" \
  KOMARI_DB_FILE="data/komari.db" \
  KOMARI_LISTEN="127.0.0.1:$RUST_PORT" \
  KOMARI_LOG_LEVEL=error \
  TZ=UTC \
  "$RUST_BIN" server
) >"$WORK/rust/server.log" 2>&1 &
RUST_PID=$!

wait_ready "Go" "$GO_PORT" "$GO_PID" "$WORK/go/server.log"
wait_ready "Rust" "$RUST_PORT" "$RUST_PID" "$WORK/rust/server.log"

# ---------------------------------------------------------------------------
# Compare
# ---------------------------------------------------------------------------

COMPARE_ARGS=(
  --go-base "http://127.0.0.1:$GO_PORT"
  --rust-base "http://127.0.0.1:$RUST_PORT"
  --go-db "$WORK/go/data/komari.db"
  --rust-db "$WORK/rust/data/komari.db"
  --admin-user "$ADMIN_USER"
  --admin-pass "$ADMIN_PASS"
  --rules "$ROOT_DIR/scripts/differential-known-differences.json"
  --report "$WORK/report.json"
)
if [[ "$NO_SEED" -eq 1 ]]; then
  COMPARE_ARGS+=(--no-seed)
fi
if [[ "$STRICT" -eq 1 ]]; then
  COMPARE_ARGS+=(--strict)
fi

echo
if python3 "$ROOT_DIR/scripts/differential_compare.py" "${COMPARE_ARGS[@]}"; then
  STATUS=0
else
  STATUS=$?
fi

if [[ -n "$REPORT_OUT" && -f "$WORK/report.json" ]]; then
  mkdir -p "$(dirname -- "$REPORT_OUT")"
  cp "$WORK/report.json" "$REPORT_OUT"
  echo "report copied to: $REPORT_OUT"
fi

if [[ "$STATUS" -ne 0 ]]; then
  echo >&2
  echo "---- Go server log (tail) ----" >&2
  tail -n 30 "$WORK/go/server.log" >&2 || true
  echo "---- Rust server log (tail) ----" >&2
  tail -n 30 "$WORK/rust/server.log" >&2 || true
fi

exit "$STATUS"
