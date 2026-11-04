#!/usr/bin/env bash
#
# Frontend E2E smoke test for the Rust backend.
#
# 1. Builds the komari-web frontend into a temporary theme directory
#    (`npm run build -- --outDir <theme>/dist`).
# 2. Copies komari-theme.json / preview.png and rebuilds komari-web with
#    KOMARI_DEFAULT_THEME_DIR pointing at that theme.
# 3. Starts the Rust server with a fresh temporary SQLite database.
# 4. Verifies `/`, `/assets/*`, `/admin`, `/api/public`, `/api/version`,
#    `/ping`, `/favicon.ico`, the SPA fallback and the login API with curl.
# 5. When Chromium/Chrome is available, renders `/` and `/admin` headlessly
#    and checks the rendered DOM. Playwright/Puppeteer are not installed in
#    this environment, so the fallback is explicitly curl + HTML keywords.
#
# Usage:
#   scripts/frontend-e2e.sh
#   scripts/frontend-e2e.sh --keep
#   scripts/frontend-e2e.sh --web-dir /path/to/komari-web
#   scripts/frontend-e2e.sh --skip-frontend-build --dist /path/to/dist
#   scripts/frontend-e2e.sh --no-browser
#
# Environment:
#   KOMARI_WEB_DIR       komari-web checkout (auto-detected)
#   KOMARI_E2E_PORT      Listen port (default: an ephemeral free port)
#   KOMARI_E2E_WORKDIR   Use this directory instead of mktemp; implies --keep
#   KOMARI_ADMIN_USER    Test administrator name (default: admin)
#   KOMARI_ADMIN_PASS    Test administrator password (default: testpass)
#   CHROME_BIN           Chromium/Chrome binary override

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

WEB_DIR="${KOMARI_WEB_DIR:-}"
DIST_DIR=""
PORT="${KOMARI_E2E_PORT:-}"
ADMIN_USER="${KOMARI_ADMIN_USER:-admin}"
ADMIN_PASS="${KOMARI_ADMIN_PASS:-testpass}"
KEEP=0
SKIP_FRONTEND_BUILD=0
NO_BROWSER=0
REPORT_OUT=""
WORKDIR_OVERRIDE=""

usage() {
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --web-dir)
      WEB_DIR="${2:?--web-dir requires a path}"
      shift 2
      ;;
    --dist)
      DIST_DIR="${2:?--dist requires a path}"
      shift 2
      ;;
    --port)
      PORT="${2:?--port requires a value}"
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
    --skip-frontend-build)
      SKIP_FRONTEND_BUILD=1
      shift
      ;;
    --skip-build)
      # Deprecated alias: still rebuilds the Rust binary with the provided
      # theme, so the served assets are guaranteed to match --dist.
      SKIP_FRONTEND_BUILD=1
      shift
      ;;
    --no-browser)
      NO_BROWSER=1
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
if [[ "$SKIP_FRONTEND_BUILD" -eq 0 ]]; then
  require_command npm
  require_command tar
fi

detect_web_dir() {
  local candidate
  local -a candidates=()
  if [[ -n "${KOMARI_WEB_DIR:-}" ]]; then
    candidates+=("$KOMARI_WEB_DIR")
  fi
  candidates+=(
    "$ROOT_DIR/../komari-web"
    "$ROOT_DIR/../../komari/komari-web"
    "$ROOT_DIR/../../github/komari/komari-web"
    "/home/fearless/github/komari/komari-web"
  )
  for candidate in "${candidates[@]}"; do
    if [[ -f "$candidate/package.json" ]] \
      && { [[ "$SKIP_FRONTEND_BUILD" -eq 1 ]] || [[ -d "$candidate/node_modules" ]]; }; then
      (cd "$candidate" && pwd)
      return 0
    fi
  done
  return 1
}

if [[ -z "$WEB_DIR" ]]; then
  if ! WEB_DIR="$(detect_web_dir)"; then
    cat >&2 <<'EOF'
error: komari-web checkout not found.

Pass it explicitly:
  scripts/frontend-e2e.sh --web-dir /path/to/komari-web

or set KOMARI_WEB_DIR.
EOF
    exit 2
  fi
fi

if [[ "$SKIP_FRONTEND_BUILD" -eq 1 && -z "$DIST_DIR" ]]; then
  echo "error: --skip-frontend-build requires --dist <directory> (or use the normal build)" >&2
  exit 2
fi

if [[ -n "$WORKDIR_OVERRIDE" ]]; then
  WORK="$WORKDIR_OVERRIDE"
  mkdir -p "$WORK"
  if [[ -d "$WORK/theme" || -d "$WORK/server/data" ]]; then
    echo "warning: --workdir reuse: clearing previous theme/server data under $WORK" >&2
    rm -rf "$WORK/theme" "$WORK/server/data" "$WORK/web-build"
  fi
  KEEP=1
else
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/komari-e2e.XXXXXX")"
fi
THEME="$WORK/theme"
SERVER_DIR="$WORK/server"
BUILD_DIR="$WORK/web-build"
mkdir -p "$THEME/dist" "$SERVER_DIR"

SERVER_PID=""

cleanup() {
  local status=$?
  trap - EXIT INT TERM
  if [[ -n "$SERVER_PID" ]]; then
    stop_pid "$SERVER_PID"
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
# Build theme + backend
# ---------------------------------------------------------------------------

if [[ "$SKIP_FRONTEND_BUILD" -eq 0 ]]; then
  echo "building komari-web frontend in a temporary copy into $THEME/dist ..."
  mkdir -p "$BUILD_DIR"
  # Build in a temporary copy with a symlinked node_modules so Vite's
  # `bundle-analysis.html` and TypeScript build info never modify the user's
  # komari-web checkout.
  if ! (
    cd "$WEB_DIR"
    tar --exclude=./node_modules --exclude=./.git --exclude=./dist \
      --exclude=./bundle-analysis.html -cf - .
  ) | (
    cd "$BUILD_DIR"
    tar -xf -
  ); then
    echo "error: failed to copy komari-web into $BUILD_DIR" >&2
    exit 2
  fi
  ln -s "$WEB_DIR/node_modules" "$BUILD_DIR/node_modules"
  if ! (
    cd "$BUILD_DIR"
    npm run build -- --outDir "$THEME/dist"
  ); then
    echo "error: komari-web frontend build failed" >&2
    exit 2
  fi
else
  echo "reusing frontend dist from $DIST_DIR ..."
  if ! cp -a "$DIST_DIR/." "$THEME/dist/"; then
    echo "error: failed to copy --dist $DIST_DIR" >&2
    exit 2
  fi
fi

for name in komari-theme.json preview.png perview.png; do
  if [[ -f "$WEB_DIR/$name" ]]; then
    cp "$WEB_DIR/$name" "$THEME/$name"
  fi
done

if [[ ! -f "$THEME/dist/index.html" ]]; then
  echo "error: frontend build did not produce $THEME/dist/index.html" >&2
  exit 2
fi
if [[ ! -f "$THEME/komari-theme.json" ]]; then
  echo "error: missing komari-theme.json in $WEB_DIR" >&2
  exit 2
fi
if [[ ! -f "$THEME/preview.png" && ! -f "$THEME/perview.png" ]]; then
  echo "error: missing preview.png (or perview.png) in $WEB_DIR" >&2
  exit 2
fi

# Always rebuild with the selected theme, including --skip-frontend-build.
# This guarantees the served asset hashes come from $THEME/dist.
echo "rebuilding komari-web with the real theme ..."
if ! (
  cd "$ROOT_DIR"
  KOMARI_DEFAULT_THEME_DIR="$THEME" cargo build -p komari-web
  KOMARI_DEFAULT_THEME_DIR="$THEME" cargo build -p komari-cli
); then
  echo "error: Rust backend rebuild with KOMARI_DEFAULT_THEME_DIR failed" >&2
  exit 2
fi

RUST_BIN="$ROOT_DIR/target/debug/komari"
if [[ ! -x "$RUST_BIN" ]]; then
  echo "error: Rust binary not found: $RUST_BIN" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Start server
# ---------------------------------------------------------------------------

if [[ -z "$PORT" ]]; then
  PORT="$(python3 - <<'PY'
import socket
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
)"
fi

echo "starting Rust backend on 127.0.0.1:$PORT (workdir $SERVER_DIR) ..."
(
  cd "$SERVER_DIR"
  ADMIN_USERNAME="$ADMIN_USER" \
  ADMIN_PASSWORD="$ADMIN_PASS" \
  KOMARI_DB_FILE="data/komari.db" \
  KOMARI_LISTEN="127.0.0.1:$PORT" \
  KOMARI_LOG_LEVEL=error \
  TZ=UTC \
  exec "$RUST_BIN" server
) >"$SERVER_DIR/server.log" 2>&1 &
SERVER_PID=$!

ready=0
for _ in $(seq 1 150); do
  if curl -fsS --max-time 1 "http://127.0.0.1:$PORT/ping" >/dev/null 2>&1; then
    ready=1
    break
  fi
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    break
  fi
  sleep 0.2
done
if [[ "$ready" -ne 1 ]]; then
  echo "error: Rust server did not become ready on port $PORT" >&2
  tail -n 60 "$SERVER_DIR/server.log" >&2 || true
  exit 1
fi

# ---------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------

CHECKS_JSON="$WORK/checks.jsonl"
: >"$CHECKS_JSON"
FAILURES=0
CHECKS=0
SKIPPED=0

record_check() {
  local name="$1" ok="$2" detail="$3"
  local skipped=false
  if [[ "$ok" == "skipped" ]]; then
    skipped=true
    ok=false
  fi
  CHECKS=$((CHECKS + 1))
  python3 - "$name" "$ok" "$skipped" "$detail" >>"$CHECKS_JSON" <<'PY'
import json, sys
print(json.dumps({
    "name": sys.argv[1],
    "ok": sys.argv[2] == "true",
    "skipped": sys.argv[3] == "true",
    "detail": sys.argv[4],
}, ensure_ascii=False))
PY
  if [[ "$ok" == "true" ]]; then
    printf '  PASS  %s\n' "$name"
  elif [[ "$skipped" == "true" ]]; then
    printf '  SKIP  %s — %s\n' "$name" "$detail"
    SKIPPED=$((SKIPPED + 1))
  else
    printf '  FAIL  %s — %s\n' "$name" "$detail"
    FAILURES=$((FAILURES + 1))
  fi
}

http_get() {
  local path="$1"
  curl -sS --max-time 15 -o "$WORK/body" -D "$WORK/headers" -w '%{http_code}' "http://127.0.0.1:$PORT$path" || true
}

header_value() {
  python3 - "$WORK/headers" "$1" <<'PY'
import sys
wanted = sys.argv[2].lower()
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    if ":" in line:
        key, value = line.split(":", 1)
        if key.strip().lower() == wanted:
            print(value.strip())
            break
PY
}

check_status_and_type() {
  local name="$1" path="$2" expected_status="$3" expected_type="$4"
  local status
  status="$(http_get "$path")"
  local content_type
  content_type="$(header_value content-type)"
  local ok=true
  local detail="status=$status content-type=$content_type"
  if [[ "$status" != "$expected_status" ]]; then
    ok=false
  fi
  if [[ -n "$expected_type" && "${content_type,,}" != *"$expected_type"* ]]; then
    ok=false
  fi
  record_check "$name" "$ok" "$detail"
}

check_status_and_type "GET /" "/" "200" "text/html"
cp "$WORK/body" "$WORK/index.html"
if grep -q 'id="root"' "$WORK/index.html"; then
  record_check "GET / contains SPA root" true ""
else
  record_check "GET / contains SPA root" false "missing id=\"root\""
fi
if grep -qi 'placeholder' "$WORK/index.html"; then
  record_check "GET / uses real embedded theme" false "placeholder theme is embedded; rebuild with KOMARI_DEFAULT_THEME_DIR"
else
  record_check "GET / uses real embedded theme" true ""
fi
if grep -qE 'src="[^"]*/assets/[^"]+\.js"' "$WORK/index.html"; then
  record_check "GET / references a hashed JS asset" true ""
else
  record_check "GET / references a hashed JS asset" false "no /assets/*.js in index.html"
fi

ASSET_JS="$(grep -oE 'src="[^"]*/assets/[^"]+\.js"' "$WORK/index.html" | head -1 | sed 's/^src="//;s/"$//' || true)"
ASSET_CSS="$(grep -oE 'href="[^"]*/assets/[^"]+\.css"' "$WORK/index.html" | head -1 | sed 's/^href="//;s/"$//' || true)"
THEME_ASSET_JS="$(grep -oE 'src="[^"]*/assets/[^"]+\.js"' "$THEME/dist/index.html" | head -1 | sed 's/^src="//;s/"$//' || true)"
if [[ -n "$ASSET_JS" && -n "$THEME_ASSET_JS" && "$ASSET_JS" == "$THEME_ASSET_JS" ]]; then
  record_check "served index uses the theme asset hash" true ""
else
  record_check "served index uses the theme asset hash" false "served=$ASSET_JS theme=$THEME_ASSET_JS"
fi
if [[ -n "$ASSET_JS" ]]; then
  check_status_and_type "GET $ASSET_JS" "$ASSET_JS" "200" "javascript"
  if [[ "$(wc -c <"$WORK/body")" -gt 1000 ]]; then
    record_check "asset JS body is non-trivial" true ""
  else
    record_check "asset JS body is non-trivial" false "body too small"
  fi
else
  record_check "index.html has a JS asset" false "not found"
fi
if [[ -n "$ASSET_CSS" ]]; then
  check_status_and_type "GET $ASSET_CSS" "$ASSET_CSS" "200" "css"
  if [[ "$(wc -c <"$WORK/body")" -gt 1000 ]]; then
    record_check "asset CSS body is non-trivial" true ""
  else
    record_check "asset CSS body is non-trivial" false "body too small"
  fi
else
  record_check "index.html has a CSS asset" false "not found"
fi

check_status_and_type "GET /admin" "/admin" "200" "text/html"
if grep -q 'id="root"' "$WORK/body"; then
  record_check "GET /admin contains SPA root" true ""
else
  record_check "GET /admin contains SPA root" false "missing id=\"root\""
fi

check_status_and_type "GET /ping" "/ping" "200" "text/plain"
if [[ "$(cat "$WORK/body")" == "pong" ]]; then
  record_check "GET /ping body is pong" true ""
else
  record_check "GET /ping body is pong" false "body=$(cat "$WORK/body")"
fi

check_status_and_type "GET /favicon.ico" "/favicon.ico" "200" "image"
check_status_and_type "GET /manifest.json" "/manifest.json" "200" "json"

check_status_and_type "GET /api/public" "/api/public" "200" "application/json"
if python3 - "$WORK/body" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
assert data["status"] == "success"
assert data["data"]["sitename"]
PY
then
  record_check "GET /api/public returns success + sitename" true ""
else
  record_check "GET /api/public returns success + sitename" false "unexpected JSON"
fi

check_status_and_type "GET /api/version" "/api/version" "200" "application/json"
if python3 - "$WORK/body" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
assert data["status"] == "success"
assert data["data"]["version"]
PY
then
  record_check "GET /api/version returns success + version" true ""
else
  record_check "GET /api/version returns success + version" false "unexpected JSON"
fi

check_status_and_type "GET /unknown/spa/route (SPA fallback)" "/unknown/spa/route" "200" "text/html"
if grep -q 'id="root"' "$WORK/body"; then
  record_check "SPA fallback contains root" true ""
else
  record_check "SPA fallback contains root" false "missing id=\"root\""
fi

check_status_and_type "GET /api/does-not-exist returns 404" "/api/does-not-exist" "404" ""

# Login API + authenticated /api/me
LOGIN_STATUS="$(curl -sS --max-time 15 -c "$WORK/cookies.txt" -o "$WORK/login.json" -w '%{http_code}' \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" \
  "http://127.0.0.1:$PORT/api/login" || true)"
if [[ "$LOGIN_STATUS" == "200" ]] && python3 - "$WORK/login.json" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
assert data["status"] == "success"
assert data["data"]["set-cookie"]["session_token"]
PY
then
  record_check "POST /api/login returns a session" true ""
else
  record_check "POST /api/login returns a session" false "status=$LOGIN_STATUS"
fi

ME_STATUS="$(curl -sS --max-time 15 -b "$WORK/cookies.txt" -o "$WORK/me.json" -w '%{http_code}' \
  "http://127.0.0.1:$PORT/api/me" || true)"
if [[ "$ME_STATUS" == "200" ]] && python3 - "$WORK/me.json" "$ADMIN_USER" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
assert data["logged_in"] is True
assert data["username"] == sys.argv[2]
PY
then
  record_check "GET /api/me is logged in as admin" true ""
else
  record_check "GET /api/me is logged in as admin" false "status=$ME_STATUS"
fi

SETTINGS_STATUS="$(curl -sS --max-time 15 -b "$WORK/cookies.txt" -o "$WORK/settings.json" -w '%{http_code}' \
  "http://127.0.0.1:$PORT/api/admin/settings/" || true)"
if [[ "$SETTINGS_STATUS" == "200" ]] && python3 - "$WORK/settings.json" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
assert data["status"] == "success"
assert isinstance(data["data"], dict)
PY
then
  record_check "GET /api/admin/settings with session" true ""
else
  record_check "GET /api/admin/settings with session" false "status=$SETTINGS_STATUS"
fi

# ---------------------------------------------------------------------------
# Headless browser smoke (optional)
# ---------------------------------------------------------------------------

BROWSER_JSON="$WORK/browser.json"
BROWSER_AVAILABLE=false
BROWSER_SKIP_REASON=""
CHROME_BIN="${CHROME_BIN:-}"
if [[ -z "$CHROME_BIN" ]]; then
  for candidate in chromium chromium-browser google-chrome google-chrome-stable; do
    if command -v "$candidate" >/dev/null 2>&1; then
      CHROME_BIN="$candidate"
      break
    fi
  done
fi

if [[ "$NO_BROWSER" -eq 1 ]]; then
  BROWSER_SKIP_REASON="disabled by --no-browser"
elif [[ -z "$CHROME_BIN" ]]; then
  BROWSER_SKIP_REASON="Chromium/Chrome not found"
else
  BROWSER_AVAILABLE=true
fi

browser_check() {
  local name="$1" url="$2" marker_regex="$3" output="$4"
  local stderr="$WORK/chrome.err"
  local static_size dom_size
  if timeout 90 "$CHROME_BIN" \
      --headless --no-sandbox --disable-gpu --disable-dev-shm-usage \
      --user-data-dir="$WORK/chrome-profile" --virtual-time-budget=8000 \
      --dump-dom "$url" >"$output" 2>"$stderr"; then
    if [[ ! -s "$output" ]]; then
      record_check "$name" false "headless browser produced an empty DOM"
      return
    fi
    # A static title/root is not enough: require Radix to have mounted inside
    # #root, and require the rendered DOM to be larger than the static index.
    if ! grep -q 'data-is-root-theme="true"' "$output"; then
      record_check "$name" false "rendered DOM lacks data-is-root-theme (JS did not render)"
      return
    fi
    if ! grep -qE '<div id="root"><[^>]' "$output"; then
      record_check "$name" false "rendered #root has no child element"
      return
    fi
    static_size="$(wc -c <"$WORK/index.html")"
    dom_size="$(wc -c <"$output")"
    if (( dom_size <= static_size )); then
      record_check "$name" false "rendered DOM ($dom_size B) is not larger than static index ($static_size B)"
      return
    fi
    if grep -Eqi "$marker_regex" "$output"; then
      record_check "$name" true ""
    else
      record_check "$name" false "rendered DOM lacks /$marker_regex/"
    fi
  else
    local stderr_text
    stderr_text="$(tail -n 3 "$stderr" 2>/dev/null || true)"
    if grep -Eqi \
        'error while loading shared libraries|No usable sandbox|Failed to move to new namespace|Operation not permitted|cannot open shared object|zygote|DevToolsActivePort|Failed to launch' \
        <<<"$stderr_text"; then
      # Known environment limitation (sandbox, missing shared library, ...):
      # the curl checks above already cover the HTTP contract. Mark the
      # browser unavailable so the second render check is not attempted.
      BROWSER_AVAILABLE=false
      BROWSER_SKIP_REASON="headless browser unavailable: $stderr_text"
      record_check "$name" skipped "$BROWSER_SKIP_REASON"
    else
      record_check "$name" false "headless browser failed: $stderr_text"
    fi
  fi
}

if [[ "$BROWSER_AVAILABLE" == true ]]; then
  browser_check "browser: / renders the dashboard" "http://127.0.0.1:$PORT/" "登录|Login|暂无|No nodes|No data" "$WORK/dom-index.html"
fi
if [[ "$BROWSER_AVAILABLE" == true ]]; then
  browser_check "browser: /admin renders the login UI" "http://127.0.0.1:$PORT/admin" "login|登录|用户名|username|密码|password" "$WORK/dom-admin.html"
fi

# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

python3 - "$CHECKS_JSON" "$BROWSER_AVAILABLE" "$BROWSER_SKIP_REASON" "$FAILURES" "$CHECKS" "$SKIPPED" \
  "$THEME" "$RUST_BIN" "$PORT" "$REPORT_OUT" <<'PY'
import json, os, sys

checks_path, available, skip_reason, failures, total_checks, skipped, theme, binary, port, report_out = sys.argv[1:]
rows = [json.loads(line) for line in open(checks_path, encoding="utf-8") if line.strip()]
report = {
    "ok": int(failures) == 0,
    "checks": rows,
    "summary": {
        "checks": int(total_checks),
        "failures": int(failures),
        "skipped": int(skipped),
    },
    "browser": {
        "available": available == "true",
        "skipped_reason": skip_reason or None,
    },
    "theme_dir": theme,
    "rust_bin": binary,
    "port": int(port),
}
text = json.dumps(report, indent=2, ensure_ascii=False)
if report_out:
    parent = os.path.dirname(os.path.abspath(report_out))
    os.makedirs(parent, exist_ok=True)
    open(report_out, "w", encoding="utf-8").write(text + "\n")
print()
print("=" * 72)
print(f"frontend E2E: checks={total_checks} failures={failures} skipped={skipped} browser_available={available}")
print(f"report: {report_out or '(not written; use --report-out)'}")
if skip_reason:
    print(f"browser note: {skip_reason}")
PY

if [[ "$FAILURES" -ne 0 ]]; then
  echo "---- server log (tail) ----" >&2
  tail -n 40 "$SERVER_DIR/server.log" >&2 || true
  exit 1
fi
exit 0
