"""Check or explicitly configure first-use enrollment for this Windows Desktop host.

The default only reads local state and read-only Server APIs. --apply sends one
Bootstrap control request. Credentials and existing chat identities are not copied.
"""
from __future__ import annotations

import argparse
from contextlib import closing
import ipaddress
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import urllib.error
import urllib.parse
import urllib.request


class SetupError(Exception):
    """Only fixed, credential-free error codes leave this tool."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def local_origin(value: str) -> str:
    parsed = urllib.parse.urlsplit(value)
    try:
        address = ipaddress.ip_address(parsed.hostname or "")
        valid = address.is_loopback and parsed.port is not None and parsed.port > 0
    except ValueError:
        valid = False
    if (not valid or parsed.scheme != "http" or parsed.username or parsed.password
            or parsed.path not in {"", "/"} or parsed.query or parsed.fragment):
        raise SetupError("server_url_must_be_numeric_loopback_http_origin_with_port")
    return value.rstrip("/")


def post(origin: str, path: str, token: str, body: dict) -> dict:
    request = urllib.request.Request(origin + path, data=json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open(request, timeout=15) as response:
            raw = response.read(1_048_577)
        if len(raw) > 1_048_576:
            raise SetupError("server_response_too_large")
        result = json.loads(raw)
        if not isinstance(result, dict):
            raise SetupError("invalid_server_response")
        return result
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
        raise SetupError("server_http_" + str(status)) from None
    except (urllib.error.URLError, TimeoutError):
        raise SetupError("server_unreachable_or_timed_out") from None
    except (ValueError, UnicodeError):
        raise SetupError("invalid_server_response") from None


def check_local(args, origin: str) -> tuple[Path, tuple | None]:
    state = json.loads(args.desktop_state.read_text(encoding="utf-8-sig"))
    runtime = state.get("runtime", {})
    if (runtime.get("server_url", "").rstrip("/") != origin
            or runtime.get("native_host_id") != args.host_id
            or runtime.get("runtime_project_id") != args.project):
        raise SetupError("desktop_target_mismatch")
    database = args.database or args.desktop_state.parent / "runtime/local/data/webcodex.db"
    with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as conn:
        host = conn.execute("SELECT enabled FROM wc_native_agent_hosts WHERE host_id=?",
                            (args.host_id,)).fetchone()
        if host != (1,):
            raise SetupError("native_host_missing_or_disabled")
        owner = conn.execute("SELECT id FROM users WHERE id=? AND disabled=0 AND disabled_at IS NULL",
                             (args.owner_user,)).fetchone()
        if owner is None:
            raise SetupError("owner_missing_or_disabled")
        previous = conn.execute(
            "SELECT owner_user,project,max_subagents,max_cli_calls "
            "FROM wc_native_host_auto_enrollment WHERE host_id=?", (args.host_id,)).fetchone()
    return database, previous


def configure(args) -> dict:
    origin = local_origin(args.server_url)
    if not re.fullmatch(r"wc_host_[0-9a-f]{32}", args.host_id):
        raise SetupError("invalid_host_id")
    for value in (args.owner_user, args.project):
        if not value.strip() or len(value) > 256 or any(ord(c) < 32 or ord(c) == 127 for c in value):
            raise SetupError("invalid_owner_or_project")
    database, previous = check_local(args, origin)
    token = args.bootstrap_token_file.read_text(encoding="utf-8-sig").strip()
    if not token or len(token) > 8192 or any(c.isspace() for c in token):
        raise SetupError("invalid_bootstrap_token_file")
    identity = post(origin, "/api/users/me", token, {})
    if (identity.get("success") is not True or identity.get("auth", {}).get("kind") != "bootstrap"
            or identity.get("auth", {}).get("is_bootstrap") is not True):
        raise SetupError("bootstrap_credential_required")
    users = post(origin, "/api/users/list", token, {})
    owners = [u for u in users.get("users", []) if u.get("id") == args.owner_user]
    if (users.get("success") is not True or len(owners) != 1
            or owners[0].get("disabled") is not False or owners[0].get("disabled_at") is not None):
        raise SetupError("server_owner_missing_or_disabled")
    dashboard = post(origin, "/api/admin/dashboard", token, {})
    projects = [p for p in dashboard.get("projects", []) if p.get("id") == args.project]
    if (len(projects) != 1 or projects[0].get("enabled") is not True
            or projects[0].get("readiness") != "online"):
        raise SetupError("server_project_missing_disabled_or_offline")
    expected = (args.owner_user, args.project, args.max_subagents, args.max_cli_calls)
    report = {"checked": True, "applied": False, "already_configured": previous == expected,
              "policy": {"max_subagents": args.max_subagents, "max_cli_calls": args.max_cli_calls}}
    if args.apply and previous != expected:
        body = {"action": "configure_native_host_auto_enrollment", "host_id": args.host_id,
                "owner_user": args.owner_user, "project": args.project, "policy": report["policy"]}
        result = post(origin, "/api/admin/agent-isolation", token, body)
        if (result.get("configured") is not True or result.get("host_id") != args.host_id
                or result.get("project") != args.project):
            raise SetupError("configuration_unconfirmed_check_before_retry")
        with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as conn:
            stored = conn.execute(
                "SELECT owner_user,project,max_subagents,max_cli_calls "
                "FROM wc_native_host_auto_enrollment WHERE host_id=?", (args.host_id,)).fetchone()
        if stored != expected:
            raise SetupError("configuration_readback_mismatch_check_before_retry")
        report["applied"] = True
    return report


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--server-url", required=True)
    result.add_argument("--bootstrap-token-file", type=Path, required=True,
                        help="Existing local token file; never pass the token itself")
    result.add_argument("--host-id", required=True, help="Verified Host ID from this machine")
    result.add_argument("--owner-user", required=True, help="Enabled user record ID, not username")
    result.add_argument("--project", required=True, help="Exact registered runtime project ID")
    local = Path(os.environ.get("LOCALAPPDATA", str(Path.home() / "AppData/Local")))
    result.add_argument("--desktop-state", type=Path,
                        default=local / "dev.webcodex.desktop/desktop-state.json")
    result.add_argument("--database", type=Path,
                        help="Default: runtime/local/data/webcodex.db beside Desktop state")
    result.add_argument("--max-subagents", type=int, choices=range(33), default=0, metavar="0..32")
    result.add_argument("--max-cli-calls", type=int, choices=range(33), default=0, metavar="0..32")
    mode = result.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="Read-only checks (the default)")
    mode.add_argument("--apply", action="store_true", help="Apply the checked local policy once")
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        print(json.dumps(configure(args), ensure_ascii=False))
        return 0
    except SetupError as error:
        print(json.dumps({"checked": False, "error": str(error)}), file=sys.stderr)
    except (OSError, ValueError, sqlite3.Error, TypeError, KeyError, AttributeError):
        print(json.dumps({"checked": False, "error": "local_or_server_data_unavailable_or_invalid"}),
              file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
