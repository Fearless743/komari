#!/usr/bin/env python3
"""HTTP differential runner for the Go and Rust Komari backends.

This script is the worker behind ``scripts/differential-test.sh``.  It:

* optionally seeds both temporary SQLite databases with the same client and
  record rows (so node/record serialization is actually exercised);
* logs in as the administrator on both backends;
* replays the same request matrix against both servers;
* normalizes volatile values (session tokens, admin UUIDs, build metadata,
  JSON object/array ordering, integer/float spelling);
* compares status, selected headers and JSON bodies; and
* reports ``一致`` / ``已知差异`` / ``差异`` / ``仅一侧`` and exits non-zero when
  an unexpected difference remains.

Known differences live in ``scripts/differential-known-differences.json``.
They are harness identifiers (``DF-xxx``); the authoritative ``D-xxx``
registry remains ``docs/parity-deviations.md``.
"""

from __future__ import annotations

import argparse
import http.client
import json
import re
import sqlite3
import sys
import urllib.parse
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Case matrix
# ---------------------------------------------------------------------------

VISIBLE_UUID_DEFAULT = "11111111-1111-4111-8111-111111111111"
HIDDEN_UUID_DEFAULT = "22222222-2222-4222-8222-222222222222"
MISSING_UUID = "00000000-0000-4000-8000-000000000000"


def build_cases(visible_uuid: str, hidden_uuid: str) -> list[dict[str, Any]]:
    """Return the request matrix in execution order.

    ``public`` must run before ``admin_settings``: the Rust backend persists
    the public-settings defaults lazily on the first ``/api/public`` call,
    whereas Go seeds every ``config.Legacy`` default during startup.
    """

    def rpc(
        case_id: str, method: str, params: Any | None = None, *, auth: bool = False
    ) -> dict[str, Any]:
        payload: dict[str, Any] = {"jsonrpc": "2.0", "id": 1, "method": method}
        if params is not None:
            payload["params"] = params
        case: dict[str, Any] = {
            "id": case_id,
            "method": "POST",
            "path": "/api/rpc2",
            "json": payload,
        }
        if auth:
            case["auth"] = True
        return case

    return [
        {"id": "ping", "method": "GET", "path": "/ping"},
        {"id": "version", "method": "GET", "path": "/api/version"},
        {"id": "public", "method": "GET", "path": "/api/public"},
        {"id": "nodes_guest", "method": "GET", "path": "/api/nodes"},
        {"id": "me_guest", "method": "GET", "path": "/api/me"},
        {"id": "recent_missing", "method": "GET", "path": f"/api/recent/{MISSING_UUID}"},
        {
            "id": "records_missing",
            "method": "GET",
            "path": f"/api/records/load?uuid={MISSING_UUID}",
        },
        rpc("rpc_ping", "rpc.ping", {}),
        rpc("rpc_public_info", "common:getPublicInfo", {}),
        rpc("rpc_nodes", "common:getNodes", {}),
        rpc("rpc_latest", "common:getNodesLatestStatus", {}),
        rpc("rpc_version", "common:getVersion", {}),
        {
            "id": "login_bad_password",
            "method": "POST",
            "path": "/api/login",
            "json": {"username": "admin", "password": "definitely-wrong"},
        },
        {
            "id": "login",
            "method": "POST",
            "path": "/api/login",
            "json": {"username": "{{admin_user}}", "password": "{{admin_pass}}"},
            "capture_session": True,
        },
        {"id": "me_admin", "method": "GET", "path": "/api/me", "auth": True},
        rpc("rpc_me_admin", "common:getMe", {}, auth=True),
        {"id": "nodes_admin", "method": "GET", "path": "/api/nodes", "auth": True},
        {"id": "admin_settings", "method": "GET", "path": "/api/admin/settings", "auth": True},
        {
            "id": "admin_settings_no_slash",
            "method": "GET",
            "path": "/api/admin/settings",
            "auth": True,
            "follow_redirects": False,
        },
        {"id": "recent_seeded", "method": "GET", "path": f"/api/recent/{visible_uuid}"},
        {
            "id": "records_seeded",
            "method": "GET",
            "path": f"/api/records/load?uuid={visible_uuid}&hours=24",
        },
        {
            "id": "records_seeded_cpu",
            "method": "GET",
            "path": f"/api/records/load?uuid={visible_uuid}&hours=24&load_type=cpu",
        },
        {
            "id": "records_seeded_gpu",
            "method": "GET",
            "path": f"/api/records/load?uuid={visible_uuid}&hours=24&load_type=gpu",
        },
        {
            "id": "recent_hidden_guest",
            "method": "GET",
            "path": f"/api/recent/{hidden_uuid}",
        },
        {
            "id": "records_hidden_guest",
            "method": "GET",
            "path": f"/api/records/load?uuid={hidden_uuid}",
        },
    ]


# ---------------------------------------------------------------------------
# SQLite seeding
# ---------------------------------------------------------------------------


def seed_database(path: Path, visible_uuid: str, hidden_uuid: str, now: str) -> None:
    """Insert one visible and one hidden client plus one record/GPU record.

    The same SQL and the same timestamp are used for both backends, so the
    differential test exercises real serialization instead of only empty
    collections.
    """

    connection = sqlite3.connect(str(path), timeout=15)
    try:
        connection.execute("PRAGMA busy_timeout=5000")
        client_sql = """
        INSERT OR REPLACE INTO clients (
            uuid, token, name, cpu_name, virtualization, arch, cpu_cores, os,
            kernel_version, gpu_name, ipv4, ipv6, region, remark,
            public_remark, mem_total, swap_total, disk_total, version, weight,
            price, billing_cycle, auto_renewal, currency, expired_at,
            `group`, tags, ddns_enabled, ddns_hostname, ddns_record_id,
            ddns_record_type, hidden, traffic_limit, traffic_limit_type,
            created_at, updated_at
        ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
        )
        """
        connection.execute(
            client_sql,
            (
                visible_uuid,
                "token-visible",
                "Visible Node",
                "Test CPU",
                "kvm",
                "x86_64",
                4,
                "Linux",
                "6.1.0",
                "",
                "192.0.2.10",
                "2001:db8::10",
                "Test",
                "private note",
                "public note",
                8589934592,
                2147483648,
                107374182400,
                "1.2.3",
                1,
                12.5,
                30,
                1,
                "$",
                "2030-01-01 00:00:00.0000000",
                "default",
                "a;b",
                0,
                "",
                "",
                "inherit",
                0,
                0,
                "max",
                "2026-01-01 00:00:00.0000000",
                "2026-01-01 00:00:00.0000000",
            ),
        )
        connection.execute(
            client_sql,
            (
                hidden_uuid,
                "token-hidden",
                "Hidden Node",
                "Test CPU",
                "kvm",
                "x86_64",
                2,
                "Linux",
                "6.1.0",
                "",
                "192.0.2.11",
                "",
                "Test",
                "hidden note",
                "",
                4294967296,
                0,
                53687091200,
                "",
                0,
                0,
                0,
                0,
                "$",
                None,
                "default",
                "",
                0,
                "",
                "",
                "inherit",
                1,
                0,
                "max",
                "2026-01-01 00:00:00.0000000",
                "2026-01-01 00:00:00.0000000",
            ),
        )
        connection.execute(
            """
            INSERT INTO records (
                client, time, cpu, gpu, ram, ram_total, swap, swap_total, load,
                temp, disk, disk_total, net_in, net_out, net_total_up,
                net_total_down, process, connections, connections_udp
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                visible_uuid,
                now,
                12.5,
                0,
                1024,
                8589934592,
                0,
                2147483648,
                0.5,
                40,
                2048,
                107374182400,
                100,
                200,
                1000,
                2000,
                10,
                5,
                1,
            ),
        )
        connection.execute(
            """
            INSERT INTO gpu_records (
                client, time, device_index, device_name, mem_total, mem_used,
                utilization, temperature
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (visible_uuid, now, 0, "Test GPU", 8589934592, 1024, 50, 60),
        )
        connection.commit()
    finally:
        connection.close()


# ---------------------------------------------------------------------------
# HTTP client
# ---------------------------------------------------------------------------


def request(
    base: str,
    method: str,
    path: str,
    *,
    body: bytes | None = None,
    headers: dict[str, str] | None = None,
    cookie: str | None = None,
    follow_redirects: bool = True,
) -> dict[str, Any]:
    """Perform one HTTP request and return status, headers and decoded body.

    ``http.client`` is used directly so that cookie handling and the
    no-redirect comparison case stay explicit and deterministic.
    """

    parsed = urllib.parse.urlsplit(base)
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or 80
    current_path = path
    current_method = method
    current_body = body
    chain: list[dict[str, Any]] = []

    for _ in range(6):
        connection = http.client.HTTPConnection(host, port, timeout=20)
        request_headers = {
            "Accept": "application/json, text/plain, */*",
            "User-Agent": "komari-differential-test/1",
        }
        if current_body is not None:
            request_headers["Content-Type"] = "application/json"
        if cookie:
            request_headers["Cookie"] = f"session_token={cookie}"
        if headers:
            request_headers.update(headers)

        try:
            connection.request(current_method, current_path, body=current_body, headers=request_headers)
            response = connection.getresponse()
            raw = response.read()
            response_headers = {key.lower(): value for key, value in response.getheaders()}
            status = response.status
        finally:
            connection.close()

        location = response_headers.get("location")
        chain.append({"path": current_path, "status": status, "location": location})

        if follow_redirects and status in (301, 302, 303, 307, 308) and location:
            if location.startswith(("http://", "https://")):
                parsed = urllib.parse.urlsplit(location)
                host = parsed.hostname or host
                port = parsed.port or (443 if parsed.scheme == "https" else 80)
                current_path = urllib.parse.urlunsplit(("", "", parsed.path or "/", parsed.query, ""))
            else:
                current_path = location
            if status in (301, 302, 303) and current_method not in ("GET", "HEAD"):
                current_method = "GET"
                current_body = None
            continue

        return {
            "status": status,
            "headers": response_headers,
            "body": raw,
            "chain": chain,
        }

    raise RuntimeError(f"too many redirects for {path}")


def decode_response(record: dict[str, Any]) -> dict[str, Any]:
    """Decode the body as JSON when possible and attach a content-type base."""

    content_type = record["headers"].get("content-type", "")
    base_type = content_type.split(";")[0].strip().lower()
    raw = record["body"]
    text: str | None
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = None

    parsed: Any | None = None
    is_json = base_type == "application/json"
    if text is not None and text.lstrip().startswith(("{", "[")):
        try:
            parsed = json.loads(text)
            is_json = True
        except json.JSONDecodeError:
            parsed = None

    return {
        "status": record["status"],
        "headers": record["headers"],
        "chain": record["chain"],
        "content_type": content_type,
        "content_type_base": base_type,
        "raw_text": text,
        "json": parsed,
        "is_json": is_json,
    }


# ---------------------------------------------------------------------------
# Normalization and diffing
# ---------------------------------------------------------------------------


def canonical(value: Any) -> Any:
    """Normalize JSON for comparison.

    * object keys are sorted by rebuilding the dict;
    * arrays are sorted by their canonical JSON (request/response ordering is
      explicitly out of scope for this harness);
    * integral floats become ints (Go emits ``80`` for ``float64(80)`` while
      Rust emits ``80.0``).
    """

    if isinstance(value, dict):
        return {key: canonical(value[key]) for key in sorted(value)}
    if isinstance(value, list):
        items = [canonical(item) for item in value]
        return sorted(items, key=lambda item: json.dumps(item, sort_keys=True, ensure_ascii=False))
    if isinstance(value, float):
        if value.is_integer():
            return int(value)
        return round(value, 6)
    return value


def canonical_json(value: Any) -> str:
    return json.dumps(canonical(value), sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def same_value(left: Any, right: Any) -> bool:
    return canonical_json(left) == canonical_json(right)


PATH_TOKEN = re.compile(r"[^.\[\]]+|\[\d+\]|\[\*\]")


def split_path(path: str) -> list[str]:
    return PATH_TOKEN.findall(path)


def path_matches(pattern: str, path: str) -> bool:
    """Match ``data.records[*].time`` against ``data.records[0].time``."""

    pattern_parts = split_path(pattern)
    path_parts = split_path(path)

    def match(pattern_index: int, path_index: int) -> bool:
        if pattern_index == len(pattern_parts):
            return path_index == len(path_parts)
        part = pattern_parts[pattern_index]
        if part == "**":
            return any(match(pattern_index + 1, next_index) for next_index in range(path_index, len(path_parts) + 1))
        if path_index >= len(path_parts):
            return False
        if part == "*":
            return match(pattern_index + 1, path_index + 1)
        if part == "[*]":
            if not path_parts[path_index].startswith("["):
                return False
            return match(pattern_index + 1, path_index + 1)
        if part != path_parts[path_index]:
            return False
        return match(pattern_index + 1, path_index + 1)

    return match(0, 0)


def rule_entry(value: Any) -> dict[str, str]:
    if isinstance(value, str):
        return {"id": "", "reason": value}
    if isinstance(value, dict):
        return {"id": str(value.get("id", "")), "reason": str(value.get("reason", ""))}
    return {"id": "", "reason": str(value)}


class DiffCollector:
    def __init__(self) -> None:
        self.diffs: list[dict[str, Any]] = []
        self.only: list[dict[str, Any]] = []
        self.known: list[dict[str, Any]] = []

    def add_diff(self, path: str, go_value: Any, rust_value: Any) -> None:
        self.diffs.append(
            {
                "path": path,
                "go": display(go_value),
                "rust": display(rust_value),
            }
        )

    def add_only(self, path: str, side: str, value: Any) -> None:
        self.only.append({"path": path, "side": side, "value": display(value)})

    def add_known(self, path: str, go_value: Any, rust_value: Any, rule: dict[str, str]) -> None:
        self.known.append(
            {
                "id": rule["id"],
                "path": path,
                "go": display(go_value),
                "rust": display(rust_value),
                "reason": rule["reason"],
            }
        )


def display(value: Any) -> str:
    text = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    if len(text) > 240:
        return text[:237] + "..."
    return text


def compare_values(
    go_value: Any,
    rust_value: Any,
    path: str,
    *,
    ignore_paths: list[str],
    known_paths: dict[str, Any],
    known_go_only_keys: set[str],
    known_rust_only_keys: set[str],
    object_path: str,
    collector: DiffCollector,
) -> None:
    if same_value(go_value, rust_value):
        return

    for pattern in ignore_paths:
        if path_matches(pattern, path):
            return

    for pattern, raw_rule in known_paths.items():
        if path_matches(pattern, path):
            collector.add_known(path, go_value, rust_value, rule_entry(raw_rule))
            return

    if isinstance(go_value, dict) and isinstance(rust_value, dict):
        for key in sorted(set(go_value) | set(rust_value)):
            child_path = f"{path}.{key}" if path else key
            if key not in rust_value:
                if path == object_path and key in known_go_only_keys:
                    collector.add_known(
                        child_path,
                        go_value[key],
                        "<missing>",
                        {
                            "id": "DF-002",
                            "reason": "Go seeds every config.Legacy default at startup; Rust persists defaults lazily.",
                        },
                    )
                else:
                    collector.add_only(child_path, "go", go_value[key])
                continue
            if key not in go_value:
                if path == object_path and key in known_rust_only_keys:
                    collector.add_known(
                        child_path,
                        "<missing>",
                        rust_value[key],
                        {
                            "id": "DF-002",
                            "reason": "Rust-only persisted key; verify against the Go config.Legacy set.",
                        },
                    )
                else:
                    collector.add_only(child_path, "rust", rust_value[key])
                continue
            compare_values(
                go_value[key],
                rust_value[key],
                child_path,
                ignore_paths=ignore_paths,
                known_paths=known_paths,
                known_go_only_keys=known_go_only_keys,
                known_rust_only_keys=known_rust_only_keys,
                object_path=object_path,
                collector=collector,
            )
        return

    if isinstance(go_value, list) and isinstance(rust_value, list):
        if len(go_value) != len(rust_value):
            collector.add_diff(path, go_value, rust_value)
            return
        for index, (go_item, rust_item) in enumerate(zip(go_value, rust_value)):
            compare_values(
                go_item,
                rust_item,
                f"{path}[{index}]",
                ignore_paths=ignore_paths,
                known_paths=known_paths,
                known_go_only_keys=known_go_only_keys,
                known_rust_only_keys=known_rust_only_keys,
                object_path=object_path,
                collector=collector,
            )
        return

    collector.add_diff(path, go_value, rust_value)


# ---------------------------------------------------------------------------
# Comparison
# ---------------------------------------------------------------------------


def compare_case(
    case: dict[str, Any],
    go_record: dict[str, Any],
    rust_record: dict[str, Any],
    rules: dict[str, Any],
    charset_cases: list[str],
) -> dict[str, Any]:
    case_rule = rules.get("cases", {}).get(case["id"], {})
    collector = DiffCollector()

    if case_rule.get("status_only"):
        expected = case_rule.get("known_status")
        if go_record["status"] != rust_record["status"]:
            if expected and {go_record["status"], rust_record["status"]} <= set(expected):
                collector.add_known(
                    "status",
                    go_record["status"],
                    rust_record["status"],
                    rule_entry(case_rule.get("reason", "Known status difference.")),
                )
            else:
                collector.add_diff("status", go_record["status"], rust_record["status"])
        return {
            "id": case["id"],
            "method": case["method"],
            "path": case["path"],
            "go_status": go_record["status"],
            "rust_status": rust_record["status"],
            "known": collector.known,
            "diffs": collector.diffs,
            "only": collector.only,
        }

    if go_record["status"] != rust_record["status"]:
        collector.add_diff("status", go_record["status"], rust_record["status"])
        return {
            "id": case["id"],
            "method": case["method"],
            "path": case["path"],
            "go_status": go_record["status"],
            "rust_status": rust_record["status"],
            "known": collector.known,
            "diffs": collector.diffs,
            "only": collector.only,
        }

    go_type = go_record["content_type_base"]
    rust_type = rust_record["content_type_base"]
    if go_type != rust_type:
        collector.add_diff("content-type", go_type, rust_type)
    elif go_record["content_type"] != rust_record["content_type"]:
        charset_cases.append(case["id"])

    for header in ("cache-control", "location"):
        go_value = go_record["headers"].get(header)
        rust_value = rust_record["headers"].get(header)
        if go_value != rust_value:
            known_header = case_rule.get("known_headers", {})
            if header in known_header:
                collector.add_known(header, go_value, rust_value, rule_entry(known_header[header]))
            else:
                collector.add_diff(header, go_value, rust_value)

    go_cookie = "set-cookie" in go_record["headers"]
    rust_cookie = "set-cookie" in rust_record["headers"]
    if go_cookie != rust_cookie:
        collector.add_diff("set-cookie", go_cookie, rust_cookie)

    if go_record["is_json"] and rust_record["is_json"]:
        compare_values(
            go_record["json"],
            rust_record["json"],
            "",
            ignore_paths=case_rule.get("ignore_paths", []),
            known_paths=case_rule.get("known_paths", {}),
            known_go_only_keys=set(case_rule.get("known_go_only_keys", [])),
            known_rust_only_keys=set(case_rule.get("known_rust_only_keys", [])),
            object_path=case_rule.get("object_path", "data"),
            collector=collector,
        )
    elif go_record["is_json"] != rust_record["is_json"]:
        collector.add_diff(
            "body",
            "<json>" if go_record["is_json"] else go_record["raw_text"],
            "<json>" if rust_record["is_json"] else rust_record["raw_text"],
        )
    elif go_record["raw_text"] != rust_record["raw_text"]:
        collector.add_diff("body", go_record["raw_text"], rust_record["raw_text"])

    return {
        "id": case["id"],
        "method": case["method"],
        "path": case["path"],
        "go_status": go_record["status"],
        "rust_status": rust_record["status"],
        "known": collector.known,
        "diffs": collector.diffs,
        "only": collector.only,
    }


def redact_record(record: dict[str, Any], case_rule: dict[str, Any]) -> dict[str, Any]:
    """Remove volatile values before writing the machine-readable report."""

    clone = json.loads(json.dumps(record, default=str))
    headers = clone.get("headers", {})
    if "set-cookie" in headers:
        headers["set-cookie"] = "<redacted>"
    if clone.get("is_json"):
        # The parsed `json` field below carries the (redacted) body. Drop the
        # raw text so a login token cannot leak through a second copy.
        clone["raw_text"] = "<json>"
    if clone.get("is_json") and isinstance(clone.get("json"), dict):
        value = clone["json"]
        for path in case_rule.get("ignore_paths", []):
            parts = split_path(path)
            node = value
            for part in parts[:-1]:
                if part.startswith("["):
                    break
                if isinstance(node, dict) and part in node:
                    node = node[part]
                else:
                    node = None
                    break
            if isinstance(node, dict) and parts and parts[-1] in node:
                node[parts[-1]] = "<redacted>"
    return clone


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--go-base", required=True)
    parser.add_argument("--rust-base", required=True)
    parser.add_argument("--go-db", type=Path)
    parser.add_argument("--rust-db", type=Path)
    parser.add_argument("--admin-user", default="admin")
    parser.add_argument("--admin-pass", default="testpass")
    parser.add_argument("--visible-uuid", default=VISIBLE_UUID_DEFAULT)
    parser.add_argument("--hidden-uuid", default=HIDDEN_UUID_DEFAULT)
    parser.add_argument("--rules", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--no-seed", action="store_true")
    parser.add_argument("--strict", action="store_true", help="Treat known differences as failures.")
    args = parser.parse_args(argv)

    rules = json.loads(args.rules.read_text(encoding="utf-8"))

    if not args.no_seed:
        if not args.go_db or not args.rust_db:
            print("error: --go-db/--rust-db are required unless --no-seed is used", file=sys.stderr)
            return 2
        now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S.0000000")
        for label, path in (("go", args.go_db), ("rust", args.rust_db)):
            if not path.is_file():
                print(f"error: {label} database does not exist: {path}", file=sys.stderr)
                return 2
            try:
                seed_database(path, args.visible_uuid, args.hidden_uuid, now)
            except sqlite3.Error as error:
                print(f"error: failed to seed {label} database {path}: {error}", file=sys.stderr)
                return 2
        print(f"seeded both databases (record time {now})")

    cases = build_cases(args.visible_uuid, args.hidden_uuid)
    results: list[dict[str, Any]] = []
    charset_cases: list[str] = []
    cookies: dict[str, str | None] = {"go": None, "rust": None}

    for case in cases:
        go_record = run_case(args.go_base, case, cookies, args)
        rust_record = run_case(args.rust_base, case, cookies, args)
        result = compare_case(case, go_record, rust_record, rules, charset_cases)
        case_rule = rules.get("cases", {}).get(case["id"], {})
        result["go"] = redact_record(go_record, case_rule)
        result["rust"] = redact_record(rust_record, case_rule)
        results.append(result)

        label = "一致"
        if result["diffs"] or result["only"]:
            label = "差异"
        elif result["known"]:
            label = "已知差异"
        print(f"  {label:<8} {case['id']} ({case['method']} {case['path']})")

    all_diffs = [item for result in results for item in result["diffs"]]
    all_only = [item for result in results for item in result["only"]]
    all_known = [item for result in results for item in result["known"]]

    if charset_cases:
        all_known.append(
            {
                "id": "DF-005",
                "path": "content-type (cases: " + ", ".join(sorted(set(charset_cases))) + ")",
                "go": "application/json; charset=utf-8",
                "rust": "application/json",
                "reason": "Gin appends `; charset=utf-8` to JSON responses; axum emits `application/json`.",
            }
        )

    summary = {
        "cases": len(results),
        "match": sum(1 for result in results if not result["diffs"] and not result["only"] and not result["known"]),
        "known": len(all_known),
        "diff": len(all_diffs),
        "only": len(all_only),
    }

    report = {
        "go_base": args.go_base,
        "rust_base": args.rust_base,
        "summary": summary,
        "unexpected_differences": all_diffs,
        "only_one_side": all_only,
        "known_differences": all_known,
        "cases": results,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print()
    print("=" * 72)
    print(
        f"cases={summary['cases']}  一致={summary['match']}  已知差异={summary['known']}  "
        f"差异={summary['diff']}  仅一侧={summary['only']}"
    )
    print(f"report: {args.report}")

    if all_known:
        print("\n已知差异:")
        for item in all_known:
            identifier = f"[{item['id']}] " if item.get("id") else ""
            print(f"  - {identifier}{item['path']}: go={item['go']} rust={item['rust']}")
            if item.get("reason"):
                print(f"    {item['reason']}")

    if all_diffs:
        print("\n未预期差异:")
        for item in all_diffs:
            print(f"  - {item['path']}: go={item['go']} rust={item['rust']}")

    if all_only:
        print("\n仅一侧:")
        for item in all_only:
            print(f"  - {item['path']} only in {item['side']}: {item['value']}")

    if all_diffs or all_only:
        return 1
    if args.strict and all_known:
        return 1
    return 0


def run_case(
    base: str,
    case: dict[str, Any],
    cookies: dict[str, str | None],
    args: argparse.Namespace,
) -> dict[str, Any]:
    body = None
    if "json" in case:
        payload = json.loads(
            json.dumps(case["json"]).replace("{{admin_user}}", args.admin_user).replace(
                "{{admin_pass}}", args.admin_pass
            )
        )
        body = json.dumps(payload).encode("utf-8")

    record = request(
        base,
        case["method"],
        case["path"],
        body=body,
        cookie=cookies["go"] if base == args.go_base and case.get("auth") else cookies["rust"] if case.get("auth") else None,
        follow_redirects=case.get("follow_redirects", True),
    )
    decoded = decode_response(record)

    if case.get("capture_session"):
        set_cookie = record["headers"].get("set-cookie", "")
        match = re.search(r"session_token=([^;]+)", set_cookie)
        side = "go" if base == args.go_base else "rust"
        cookies[side] = match.group(1) if match else None

    return decoded


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
