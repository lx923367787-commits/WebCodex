"""One operator-authorized activation using a ten-minute management-only PAT.

Not exposed as a model/provider tool. Never prints or persists credentials.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time
import urllib.error
import urllib.request
import urllib.parse
import uuid

from dotenv import dotenv_values

DEFAULT_STATE = Path(os.environ.get("LOCALAPPDATA", str(Path.home() / "AppData" / "Local"))) / "dev.webcodex.desktop" / "desktop-state.json"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def call(path, body, token, origin):
    request = urllib.request.Request(origin + path,
        json.dumps(body).encode(), {"Authorization": "Bearer " + token, "Content-Type": "application/json"})
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with opener.open(request, timeout=90) as response:
            raw = response.read(1048577)
        if len(raw) > 1048576:
            raise RuntimeError("response_too_large")
        value = json.loads(raw)
        if not isinstance(value, dict) or value.get("success") is not True:
            raise RuntimeError("operation_not_successful")
        return value
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
        raise RuntimeError("http_" + str(status)) from None


def tool(name, params, token, origin):
    return call("/api/tools/call", {"tool": name, "params": params}, token, origin)["output"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--desktop-state", type=Path, default=DEFAULT_STATE)
    parser.add_argument("--runner", required=True)
    parser.add_argument("--origin", required=True, help="Expected local Server origin, including port")
    parser.add_argument("--username", required=True, help="Owner of the temporary management permission")
    parser.add_argument("--expected-config-sha256", required=True)
    parser.add_argument("--output-directory", type=Path, required=True, help="New local report directory")
    args = parser.parse_args()
    origin = args.origin.rstrip("/")
    parsed = urllib.parse.urlsplit(origin)
    if (parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "localhost", "::1"}
            or parsed.username or parsed.password or parsed.path or parsed.query or parsed.fragment):
        parser.error("--origin must be a local HTTP Server origin without credentials or path")
    expected_hash = args.expected_config_sha256.lower()
    if len(expected_hash) != 64 or any(c not in "0123456789abcdef" for c in expected_hash):
        parser.error("--expected-config-sha256 must contain 64 hexadecimal characters")
    args.output_directory.mkdir(parents=True, exist_ok=False)
    report = {"runner": args.runner, "plugin": "chat-agents", "activated": False,
        "temporary_scopes": ["plugin:manage"], "temporary_ttl_seconds": 600,
        "service_restarted": False, "credential_persisted": False, "stage": "preflight"}
    token_id, admin = None, None
    try:
        runtime = json.loads(args.desktop_state.read_text(encoding="utf-8-sig"))["runtime"]
        if runtime["server_url"].rstrip("/") != origin or runtime["runner_client_id"] != args.runner:
            raise RuntimeError("desktop_target_changed")
        config = Path(runtime["runner_config"])
        if hashlib.sha256(config.read_bytes()).hexdigest() != expected_hash:
            raise RuntimeError("runner_configuration_changed")
        ordinary = Path(runtime["user_token_file"]).read_text(encoding="utf-8-sig").strip()
        status = tool("runtime_status", {"compact": True, "client_id": args.runner}, ordinary, origin)
        if status.get("focus", {}).get("active_jobs") != 0 or status.get("jobs", {}).get("active_count") != 0:
            raise RuntimeError("runner_has_active_jobs")
        admin = dotenv_values(runtime["server_env_file"], interpolate=False).get("WEBCODEX_TOKEN")
        if not admin:
            raise RuntimeError("admin_configuration_unavailable")
        report["stage"] = "create_temporary_permission"
        minted = call("/api/tokens/create", {"username": args.username,
            "name": "chat-agents-install", "scopes": ["plugin:manage"],
            "expires_at": int(time.time()) + 600}, admin, origin)
        token_id, management = minted["token_id"], minted["token"]
        report["temporary_token_id"] = token_id
        report["expires_at"] = minted["expires_at"]
        del minted
        report["checks"] = []
        for plugin in ("chat-agents", "edge-handoff"):
            report["stage"] = "check_" + plugin
            checked = tool("plugin_tool", {"action": "check", "runner": args.runner, "plugin": plugin}, management, origin)
            report["checks"].append({k: checked[k] for k in ("plugin", "ready", "phase", "toolCount", "code", "diagnostic") if k in checked})
            if checked.get("ready") is not True:
                raise RuntimeError("candidate_not_ready")
        report["stage"] = "reload"
        reloaded = tool("plugin_tool", {"action": "reload", "runner": args.runner}, management, origin)
        report["plugins"] = [{k: item.get(k) for k in ("plugin", "name", "status", "errorCode")}
            for item in reloaded.get("plugins", [])]
        if reloaded.get("failures") or not any(p["plugin"] == "chat-agents" and p["status"] == "ready" for p in report["plugins"]):
            raise RuntimeError("reload_not_confirmed")
        report.update(activated=True, stage="activated")
        del management
    except Exception as error:
        # No raw response, headers, env, or exception detail is logged.
        report["error"] = str(error) if isinstance(error, RuntimeError) else type(error).__name__
    finally:
        if token_id and admin:
            try:
                revoked = call("/api/tokens/revoke", {"username": args.username, "token_id": token_id}, admin, origin)
                report["temporary_permission_revoked"] = revoked.get("success") is True
            except Exception:
                report["temporary_permission_revoked"] = False
        history = args.output_directory / "activation-history"
        history.mkdir(exist_ok=True)
        encoded = json.dumps(report, ensure_ascii=False, indent=2)
        (history / ((token_id or str(uuid.uuid4())) + ".json")).write_text(encoded, encoding="utf-8")
        (args.output_directory / "activation.json").write_text(encoded, encoding="utf-8")
        print(json.dumps(report, ensure_ascii=False))


if __name__ == "__main__":
    main()
