#!/usr/bin/env bash
# Verify that documented configuration names, CLI flags, provider fields,
# defaults and links agree with the code. Usage: ./docs/check-docs.sh
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ok=0
fail=0
section_name=""
section_ok=0
section_fail=0

section() {
  if [ -n "$section_name" ]; then
    printf '  %s: %d passed, %d failed\n' "$section_name" "$section_ok" "$section_fail"
  fi
  section_name="$1"
  section_ok=0
  section_fail=0
  echo "== $1 =="
}

pass() {
  ok=$((ok + 1))
  section_ok=$((section_ok + 1))
}

miss() {
  echo "MISSING: $*"
  fail=$((fail + 1))
  section_fail=$((section_fail + 1))
}

need() {
  local file="$1" needle="$2"
  if grep -Fq -- "$needle" "$root/$file"; then
    pass
  else
    miss "$needle in $file"
  fi
}

need_any() {
  local needle="$1"
  shift
  local file
  for file in "$@"; do
    if grep -Fq -- "$needle" "$root/$file"; then
      pass
      return
    fi
  done
  miss "$needle in any of: $*"
}

need_doc_alt() {
  local needle="$1" alt="$2"
  if grep -Fq -- "$needle" "$root/docs/CONFIGURATION.md" ||
    grep -Fq -- "$alt" "$root/docs/CONFIGURATION.md"; then
    pass
  else
    miss "$needle / $alt in docs/CONFIGURATION.md"
  fi
}

# Match a short flag as a standalone token, not as a substring of a long flag.
need_token() {
  local token="$1"
  if grep -Eq -- "(^|[^-A-Za-z0-9])${token}([^-A-Za-z0-9]|$)" \
    "$root/docs/CONFIGURATION.md"; then
    pass
  else
    miss "token $token in docs/CONFIGURATION.md"
  fi
}

section "environment variables mentioned in docs"
mapfile -t envs < <(grep -rhoE 'KOMARI_[A-Z0-9_]+' "$root/docs" "$root/README.md" | sort -u)
for var in "${envs[@]}"; do
  case "$var" in
    KOMARI_VERSION|KOMARI_VERSION_HASH)
      need_any "$var" crates/komari-cli/src/version.rs
      continue
      ;;
    KOMARI_COMPAT_DB)
      need_any "$var" crates/komari-db/tests/compat.rs
      continue
      ;;
    KOMARI_THEME_SRC)
      # B10 Docker build argument.
      need docs/DEPLOYMENT.md "$var"
      need docs/MIGRATION.md "$var"
      need Dockerfile "$var"
      continue
      ;;
    KOMARI_DEFAULT_THEME_DIR)
      # B14 build.rs compile-time input.
      need docs/DEPLOYMENT.md "$var"
      need docs/MIGRATION.md "$var"
      need crates/komari-web/build.rs "$var"
      continue
      ;;
    KOMARI_WEB_DIST|KOMARI_WEB_DIR)
      # Deprecated B10 variables; docs must mark them as such.
      need docs/CONFIGURATION.md "$var"
      need docs/DEPLOYMENT.md "$var"
      need docs/CONFIGURATION.md 废弃
      continue
      ;;
  esac
  need_any "$var" crates/komari-config/src/keys.rs
done

section "all ENV_* constants are documented"
mapfile -t env_consts < <(
  grep -oE 'pub const ENV_[A-Z0-9_]+: &str = "[^"]+"' \
    "$root/crates/komari-config/src/keys.rs" |
    sed -E 's/.*"([^"]+)".*/\1/'
)
for var in "${env_consts[@]}"; do
  need docs/CONFIGURATION.md "$var"
done

section "CLI flags: documented flags exist in cli.rs"
while IFS='|' read -r needle file; do
  need "$file" "$needle"
done <<'EOF'
long = "listen"|crates/komari-cli/src/cli.rs
short = 'l'|crates/komari-cli/src/cli.rs
long = "db-type"|crates/komari-cli/src/cli.rs
short = 't'|crates/komari-cli/src/cli.rs
long = "database"|crates/komari-cli/src/cli.rs
short = 'd'|crates/komari-cli/src/cli.rs
db-file|crates/komari-cli/src/cli.rs
long = "db-host"|crates/komari-cli/src/cli.rs
long = "db-port"|crates/komari-cli/src/cli.rs
long = "db-user"|crates/komari-cli/src/cli.rs
long = "db-pass"|crates/komari-cli/src/cli.rs
long = "db-name"|crates/komari-cli/src/cli.rs
long = "log-level"|crates/komari-cli/src/cli.rs
long = "theme"|crates/komari-cli/src/cli.rs
long = "allow-cors"|crates/komari-cli/src/cli.rs
long = "enable-cloudflared"|crates/komari-cli/src/cli.rs
long = "ws-disable-origin"|crates/komari-cli/src/cli.rs
long = "username"|crates/komari-cli/src/cli.rs
short = 'u'|crates/komari-cli/src/cli.rs
long = "password"|crates/komari-cli/src/cli.rs
short = 'p'|crates/komari-cli/src/cli.rs
EOF

section "CLI flags: every clap long/short/alias is documented"
mapfile -t longs < <(
  grep -oE 'long = "[^"]+"' "$root/crates/komari-cli/src/cli.rs" |
    sed -E 's/long = "([^"]+)"/\1/'
)
for flag in "${longs[@]}"; do
  need docs/CONFIGURATION.md "--$flag"
done
mapfile -t shorts < <(
  grep -oE "short = '[^']+'" "$root/crates/komari-cli/src/cli.rs" |
    sed -E "s/short = '([^']+)'/\1/"
)
for flag in "${shorts[@]}"; do
  need_token "-$flag"
done
mapfile -t aliases < <(
  grep -oE 'visible_alias = "[^"]+"' "$root/crates/komari-cli/src/cli.rs" |
    sed -E 's/visible_alias = "([^"]+)"/\1/'
)
for alias in "${aliases[@]}"; do
  need_doc_alt "--$alias" "$alias"
done

section "subcommands"
for needle in 'Server(ServerArgs)' 'Chpasswd(ChpasswdArgs)' 'Disable2fa(Disable2faArgs)' 'PermitLogin' \
  'name = "disable-2fa"' 'visible_alias = "disable2FA"' \
  'name = "permit-login"' 'visible_alias = "permitPasswordLogin"'; do
  need crates/komari-cli/src/cli.rs "$needle"
done
for subcommand in server chpasswd disable-2fa disable2FA permit-login permitPasswordLogin; do
  need docs/CONFIGURATION.md "$subcommand"
done

section "persisted config keys"
mapfile -t keys < <(
  grep -oE 'pub const [A-Z0-9_]+_KEY: &str = "[^"]+"' \
    "$root/crates/komari-config/src/keys.rs" |
    sed -E 's/.*"([^"]+)".*/\1/'
)
for key in "${keys[@]}"; do
  need docs/CONFIGURATION.md "$key"
done

section "auth-only keys"
for key in tempory_share_token tempory_share_token_expire_at session_token api_key private_site; do
  need_any "$key" crates/komari-auth/src/middleware.rs crates/komari-config/src/keys.rs
  need docs/CONFIGURATION.md "$key"
done

section "provider names and fields"
for name in telegram bark webhook email 'Server酱³' 'Server酱Turbo' empty Javascript; do
  need crates/komari-notify/src/registry.rs "\"$name\""
  need docs/CONFIGURATION.md "$name"
done
mapfile -t notify_fields < <(
  grep -hoE 'ConfigField::new\("[^"]+"' "$root"/crates/komari-notify/src/providers/*.rs |
    sed -E 's/ConfigField::new\("([^"]+)"/\1/' | sort -u
)
for field in "${notify_fields[@]}"; do
  need docs/CONFIGURATION.md "$field"
done

mapfile -t ddns_fields < <(
  grep -hoE 'ConfigurationItem::new\("[^"]+"' "$root"/crates/komari-ddns/src/providers/*.rs |
    sed -E 's/ConfigurationItem::new\("([^"]+)"/\1/' | sort -u
)
for field in "${ddns_fields[@]}"; do
  need docs/CONFIGURATION.md "$field"
done
need crates/komari-ddns/src/providers/cloudflare.rs '"cloudflare"'
need crates/komari-ddns/src/providers/empty.rs '"empty"'
need docs/CONFIGURATION.md cloudflare
need docs/CONFIGURATION.md webhook
need docs/CONFIGURATION.md empty

section "GeoIP provider keys"
for provider in empty geojs ipapi ipinfo mmdb; do
  need crates/komari-geoip/src/factory.rs "crate::providers::$provider::register"
  case "$provider" in
    ipapi) need docs/CONFIGURATION.md 'ip-api' ;;
    *) need docs/CONFIGURATION.md "$provider" ;;
  esac
done
need crates/komari-geoip/src/service.rs 'DEFAULT_CACHE_CAPACITY: usize = 10_000'

section "defaults extracted from config/site/service/providers"
mapfile -t defaults < <(
  {
    grep -oE 'pub const DEFAULT_(LISTEN|DB_FILE): &str = "[^"]+"' \
      "$root/crates/komari-config/src/config.rs" |
      sed -E 's/.*"([^"]+)"/\1/'
    grep -oE '(host|user|name|theme): "[^"]+"' \
      "$root/crates/komari-config/src/config.rs" |
      sed -E 's/.*"([^"]+)"/\1/'
    grep -oE 'port: [0-9]+' "$root/crates/komari-config/src/config.rs" |
      sed -E 's/port: //'
    sed -n '/impl Default for SiteConfig/,/^}/p' \
      "$root/crates/komari-config/src/site.rs" |
      grep -oE '"[^"]+"' | tr -d '"'
    sed -n '/impl Default for SiteConfig/,/^}/p' \
      "$root/crates/komari-config/src/site.rs" |
      grep -oE ':\s*(true|false|[0-9]+(\.[0-9]+)?)' |
      sed -E 's/^:\s*//'
    grep -rhoE 'default_value\("[^"]*"\)' \
      "$root/crates/komari-notify/src/providers" \
      "$root/crates/komari-ddns/src/providers" |
      sed -E 's/default_value\("([^"]*)"\)/\1/'
  } | sed -E 's/^[[:space:]]+|[[:space:]]+$//g' | grep -v '^$' | sort -u
)
for value in "${defaults[@]}"; do
  need docs/CONFIGURATION.md "${value//_/,}"
done
need docs/CONFIGURATION.md sqlite
need docs/CONFIGURATION.md info
# GeoIP service constants
ttl_line=$(grep -E 'DEFAULT_CACHE_TTL: Duration' "$root/crates/komari-geoip/src/service.rs")
ttl_hours=$(printf '%s\n' "$ttl_line" | grep -oE '[0-9]+' | head -1)
need docs/CONFIGURATION.md "$ttl_hours"
capacity=$(grep -oE 'DEFAULT_CACHE_CAPACITY: usize = [0-9_]+' \
  "$root/crates/komari-geoip/src/service.rs" | grep -oE '[0-9_]+$')
need docs/CONFIGURATION.md "${capacity//_/,}"

section "paths, schema and constants"
need crates/komari-config/src/config.rs 'DEFAULT_DB_FILE'
need crates/komari-config/src/config.rs 'data/komari.db'
need crates/komari-config/src/config.rs '0.0.0.0:25774'
need crates/komari-db/src/db.rs 'data/komari.db'
need crates/komari-geoip/src/providers/mmdb.rs 'GeoLite2-Country.mmdb'
need crates/komari-geoip/src/providers/mmdb.rs 'https://raw.githubusercontent.com/Loyalsoldier/geoip/release/GeoLite2-Country.mmdb'
need docs/MIGRATION.md 'command_audits'
need docs/MIGRATION.md 'ping_records'
need docs/MIGRATION.md 'task_results'

section "relative markdown links"
python3 - "$root" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
files = [root / "README.md", *sorted((root / "docs").glob("*.md"))]
bad = []
count = 0
for md in files:
    text = md.read_text(encoding="utf-8")
    for match in re.finditer(r"\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)", text):
        link = match.group(1)
        if link.startswith(("http://", "https://", "mailto:", "#")):
            continue
        target = link.split("#", 1)[0]
        if not target:
            continue
        count += 1
        path = (md.parent / target).resolve()
        if not path.exists():
            bad.append(f"{md.relative_to(root)} -> {link}")
if bad:
    for item in bad:
        print(f"MISSING LINK: {item}")
    sys.exit(1)
print(f"checked {count} relative links")
PY
link_status=$?
if [ "$link_status" -eq 0 ]; then
  pass
else
  fail=$((fail + 1))
fi

printf '  %s: %d passed, %d failed\n' "$section_name" "$section_ok" "$section_fail"
echo
echo "checks passed: $ok, failures: $fail"
[ "$fail" -eq 0 ]
