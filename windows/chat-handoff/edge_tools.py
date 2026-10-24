"""Isolated stdio MCP facade for one-shot Edge handoff workers.

No browser, arbitrary JavaScript, shell, or native input is exposed here.
The separately reviewed edge_adapter owns browser behavior and its UI lock.
"""
from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import hashlib
import importlib
import importlib.util
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import time
import uuid
from host_enrollment import contains_launch_grant, sanitize_candidate_response


ROOT = Path(__file__).resolve().parent
DEFAULT_RUNTIME = ROOT / "runtime"
IDENTITY = ("launch_id", "project", "goal_id", "source_context_epoch", "checkpoint_sequence", "checkpoint_revision")
REQUIRED = set(IDENTITY) | {"recovery_prompt", "expires_at"}
ALLOWED = REQUIRED | {"schema_version", "deadline_at", "thinking_policy", "thinking_profile",
    "chat_task_request_id", "chat_task_id", "handoff_id", "attempt_id", "attempt_number", "agent_id"}
TERMINAL = {"sent", "blocked", "unknown"}
REASON_PATTERN = re.compile(r"^[a-zA-Z0-9_:-]{1,120}$")
PROTOCOL_VERSION = "webcodex-plugin-v1"
MAX_FRAME = 65536
WORKER_ENV = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "TEMP", "TMP", "USERPROFILE",
    "LOCALAPPDATA", "APPDATA", "PROGRAMDATA", "PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432",
    "HOMEDRIVE", "HOMEPATH", "HOME", "USERNAME", "COMPUTERNAME", "LANG", "LC_ALL", "PYTHONUTF8",
    "WEBCODEX_HANDOFF_CONTROL_ENDPOINT", "WEBCODEX_HANDOFF_CONTROL_TOKEN", "WEBCODEX_HANDOFF_CONTROL_TOKEN_FILE",
    "WEBCODEX_HANDOFF_OWNER_USER", "WEBCODEX_HANDOFF_PROJECT", "WEBCODEX_HANDOFF_AGENT_ID"}

TOOL_SPECS = [{"name": "handoff_" + name, "description": description,
    "inputSchema": {"type": "object", "properties": {"launch_id": {"type": "string", "minLength": 36, "maxLength": 36,
        "description": "Canonical lowercase UUID; validated by the dispatcher."}},
        "required": [] if name == "status" else ["launch_id"], "additionalProperties": False},
    "annotations": {"readOnlyHint": name != "launch", "destructiveHint": False,
        "idempotentHint": True, "openWorldHint": name == "launch"}}
    for name, description in (("status", "Read scheduler or one fixed handoff worker state."),
        ("inspect", "Read bounded backend state for one fixed local handoff request."),
        ("launch", "Start one hidden worker for a fixed local UUID request; poll status afterwards."))]


def launch_id(value):
    try:
        canonical = str(uuid.UUID(value))
    except (ValueError, AttributeError, TypeError):
        raise ValueError("invalid_launch_id") from None
    if canonical != value:
        raise ValueError("invalid_launch_id")
    return value


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate_json_key")
        result[key] = value
    return result


def read_json(path, limit=65536):
    with Path(path).open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise ValueError("local_file_too_large")
    value = json.loads(data.decode("utf-8-sig"), object_pairs_hook=unique_object)
    if not isinstance(value, dict):
        raise ValueError("invalid_local_record")
    # Legacy candidate text is untrusted output. Omit the entire sensitive
    # body; authority/config fields containing a grant still fail closed.
    return sanitize_candidate_response(value)


def write_json(path, value):
    from handoff_flow import ensure_no_launch_claim
    serialized = json.dumps(value, ensure_ascii=False, allow_nan=False)
    ensure_no_launch_claim(serialized)
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    try:
        with temporary.open("x", encoding="utf-8") as output:
            output.write(serialized)
            output.flush()
            os.fsync(output.fileno())
        temporary.replace(path)
    finally:
        if temporary.exists():
            temporary.unlink()


def expiry(request):
    parsed = dt.datetime.fromisoformat(request["expires_at"].replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("expiry_timezone_required")
    return parsed.timestamp()


def load_request(runtime, request_id):
    request_id = launch_id(request_id)
    directory = (Path(runtime) / "requests").resolve()
    path = directory / (request_id + ".json")
    if path.is_symlink() or path.resolve().parent != directory:
        raise ValueError("request_path_rejected")
    value = read_json(path)
    if not REQUIRED <= set(value) or set(value) - ALLOWED or value["launch_id"] != request_id:
        raise ValueError("request_identity_invalid")
    for key in ("project", "goal_id", "source_context_epoch"):
        if not isinstance(value[key], str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,200}", value[key]):
            raise ValueError("request_identity_invalid")
    for key in ("checkpoint_sequence", "checkpoint_revision"):
        if type(value[key]) is not int or value[key] < 0:
            raise ValueError("request_identity_invalid")
    if not isinstance(value["recovery_prompt"], str) or not value["recovery_prompt"].strip() or len(value["recovery_prompt"]) > 32768:
        raise ValueError("request_prompt_invalid")
    if not isinstance(value["expires_at"], str):
        raise ValueError("request_expiry_invalid")
    expiry(value)
    from handoff_flow import validate_request
    validate_request(value, check_expiry=False)
    # A scheduler may refresh expiry only after a proven not-started result.
    # Identity, checkpoint, deadline and exact message stay bound.
    immutable = {key: item for key, item in value.items() if key != "expires_at"}
    digest = hashlib.sha256(json.dumps(immutable, sort_keys=True, ensure_ascii=False,
        separators=(",", ":"), allow_nan=False).encode("utf-8")).hexdigest()
    return path, value, digest


def scheduler_state(runtime):
    path = (Path(runtime) / "auto-handoff.sqlite3").resolve()
    with contextlib.closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=2)) as db:
        row = db.execute("SELECT mode,generation FROM scheduler WHERE id=1").fetchone()
    if row is None or row[0] not in ("running", "paused", "stopped") or type(row[1]) is not int:
        raise ValueError("scheduler_state_unavailable")
    return {"mode": row[0], "generation": row[1]}


def backend_available():
    return importlib.util.find_spec("edge_adapter") is not None


def load_backend():
    return importlib.import_module("edge_adapter")


def endpoint_available():
    try:
        importlib.import_module("playwright_extension_browser").discover_endpoint()
        return True
    except Exception:
        return False


def spawn_worker(runtime, request_id):
    executable = Path(sys.executable)
    windowless = executable.with_name("pythonw.exe")
    if os.name == "nt" and windowless.is_file():
        executable = windowless
    env = {key: value for key, value in os.environ.items() if key.upper() in WORKER_ENV}
    # Only this trusted control worker inherits the explicitly configured
    # control connection. The browser adapter has a separate OS-only allowlist
    # and never forwards these fields to the extension or a webpage.
    env["WEBCODEX_HANDOFF_RUNTIME"] = str(Path(runtime).resolve())
    process = subprocess.Popen([str(executable), str(Path(__file__).resolve()), "--worker", request_id,
        "--runtime", str(Path(runtime).resolve())], cwd=str(ROOT), env=env,
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), close_fds=True)
    return process.pid


def safe_reason(value, fallback):
    return value if isinstance(value, str) and not contains_launch_grant(value) and REASON_PATTERN.fullmatch(value) else fallback


def outcome(value):
    if not isinstance(value, dict):
        return {"state": "unknown", "reason": "invalid_backend_result"}
    state = value.get("status")
    if state == "retryable" and value.get("launched") is False:
        return {"state": "retryable", "reason": safe_reason(value.get("reason"), "not_started"), "launched": False}
    if state not in TERMINAL:
        state = "unknown"
    result = {"state": state, "reason": safe_reason(value.get("reason"), "backend_result"), "business_verified": False}
    url = value.get("target_url")
    if isinstance(url, str) and re.fullmatch(r"https://chatgpt\.com/(?:g/[A-Za-z0-9_-]+/)?c/[A-Za-z0-9-]{8,}", url):
        result["target_url"] = url
    for key in ("message_observed",):
        if isinstance(value.get(key), bool):
            result[key] = value[key]
    return result


def matching_entry(runtime, request):
    path = (Path(runtime) / "auto-handoff.sqlite3").resolve()
    with contextlib.closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=2)) as db:
        db.row_factory = sqlite3.Row
        row = db.execute("SELECT * FROM dispatch WHERE launch_id=?", (request["launch_id"],)).fetchone()
    if row is None or any(row[key] != request[key] for key in IDENTITY):
        raise ValueError("unregistered_request")
    if request.get("handoff_id") and (row["handoff_id"] != request["handoff_id"]
            or row["attempt_id"] != request["attempt_id"] or row["attempt_count"] != request["attempt_number"]
            or row["agent_id"] != request.get("agent_id")):
        raise ValueError("unregistered_attempt")
    return dict(row)


def run_scheduler_request(runtime, request_id, generation, loader=load_backend,
        credentials=None, api_factory=None):
    """Use the existing scheduler's authority; a JSON file is never consent."""
    import auto_handoff as scheduler
    import handoff_controller as controller
    import run_auto_handoff as runner
    runtime = Path(runtime).resolve()
    if credentials is None:
        from handoff_control_api import load_credentials
        credentials = load_credentials
    api_factory = api_factory or scheduler.Api
    store = None
    try:
        with controller.watcher_lock(runtime / "desktop-dispatch"):
            _, request, _ = load_request(runtime, request_id)
            matching_entry(runtime, request)
            store = scheduler.Store(runtime / "auto-handoff.sqlite3")
            control = store.status()
            if control["mode"] != "running" or control["generation"] != generation:
                return {"state": "retryable", "reason": "scheduler_control_changed", "launched": False}
            entry = store.entry(request_id)
            if entry.get("handoff_id"):
                import handoff_reliability as reliable
                origin, token, _ = credentials()
                if origin != control["endpoint"]:
                    return {"state": "blocked", "reason": "desktop_connection_changed"}
                api = api_factory(origin, token)
                del token
                candidate = {key: entry[key] for key in reliable.KEYS}
                if entry.get("agent_id"):
                    candidate["agent_id"] = entry["agent_id"]
                observed = scheduler.read_observation(api, candidate)
                if "handoff" not in observed or observed["handoff"]["attempt_id"] != request_id:
                    return {"state": "blocked", "reason": "attempt_no_longer_current"}
                if observed["handoff"]["status"] in ("committed", "failed"):
                    reliable.observe(store, candidate, observed)
                    return {"state": "blocked", "reason": "goal_no_longer_waiting"}
                if expiry(request) <= time.time():
                    return {"state": "blocked", "reason": "attempt_expired"}
                reliable.poll_candidate(store, api, lambda path, stop: loader().launch(path, should_stop=stop),
                    candidate, observed, time.time, runtime / "requests", allow_advance=False, expected_attempt_id=request_id)
                receipt = reliable.attempt(store, request_id)
                if receipt:
                    return outcome({"status": receipt["status"], "launched": bool(receipt["launched"]) if receipt["launched"] is not None else None})
                current = store.entry(request_id)
                if current and current["status"] in ("resumed", "failed"):
                    return {"state": "blocked", "reason": "goal_no_longer_waiting"}
                return {"state": "retryable", "reason": "scheduler_deferred", "launched": False}
            # Legacy requests remain inspectable; they do not have revocable
            # candidates or an execution-grant receipt and cannot be launched.
            return {"state": "blocked", "reason": "handoff_protocol_required"}
    except ValueError:
        # The long-running scheduler normally owns this lock. It remains the
        # authority and continues its normal checks; do not bypass its lease.
        return {"state": "retryable", "reason": "scheduler_unavailable_or_owned", "launched": False}
    except Exception:
        return {"state": "unknown", "reason": "scheduler_check_failed"}
    finally:
        if store is not None:
            store.close()


class Service:
    def __init__(self, runtime=DEFAULT_RUNTIME, spawner=spawn_worker, loader=load_backend, available=backend_available,
            clock=time.time, coordinator=run_scheduler_request, endpoint_check=endpoint_available):
        self.runtime = Path(runtime).resolve()
        self.records = self.runtime / "edge-tools"
        self.spawner, self.loader, self.available, self.clock = spawner, loader, available, clock
        self.coordinator = coordinator
        self.endpoint_check = endpoint_check

    def path(self, request_id):
        return self.records / (launch_id(request_id) + ".json")

    def status(self, request_id=None):
        try:
            scheduler = scheduler_state(self.runtime)
        except (OSError, ValueError, sqlite3.Error):
            scheduler = {"mode": "unavailable"}
        if request_id is None:
            return {"backend_available": self.available(), "edge_endpoint_available": self.endpoint_check(),
                "scheduler": scheduler}
        try:
            path = self.path(request_id)
            if not path.exists():
                _, request, _ = load_request(self.runtime, request_id)
                entry = matching_entry(self.runtime, request)
                return {"launch_id": request_id, "state": entry["status"], "scheduler_state": entry["status"],
                    "backend": self.inspect(request_id).get("snapshot", {})}
            value = read_json(path)
            # Running is an observation, never a delivery guarantee. Once its
            # bounded request expires, an unfinished issued worker is unknown.
            if value.get("state") in ("issued", "accepted", "running") and self.clock() >= value.get("expires_at", 0):
                value = {**value, "state": "unknown", "reason": "worker_result_unknown"}
            return {key: value[key] for key in ("launch_id", "state", "reason", "pid", "updated_at", "result") if key in value}
        except (OSError, ValueError, TypeError, sqlite3.Error):
            return {"state": "blocked", "reason": "invalid_launch_record"}

    def inspect(self, request_id):
        try:
            _, request, digest = load_request(self.runtime, request_id)
            raw = self.loader().inspect(request)
            if not isinstance(raw, dict):
                raise ValueError("invalid_snapshot")
            raw = sanitize_candidate_response(raw)
            safe = {}
            for key in ("available", "endpoint_available", "bound", "document_visible", "document_focused", "message_observed"):
                if type(raw.get(key)) is bool:
                    safe[key] = raw[key]
            for key in ("backend", "state", "status", "reason", "snapshot_id", "target_id"):
                if isinstance(raw.get(key), str) and REASON_PATTERN.fullmatch(raw[key]):
                    safe[key] = raw[key]
            if isinstance(raw.get("target_url"), str):
                target = outcome({"status": "unknown", "target_url": raw["target_url"]}).get("target_url")
                if target:
                    safe["target_url"] = target
            if isinstance(raw.get("result"), dict):
                safe["result"] = outcome(raw["result"])
            return {"launch_id": request_id, "request_fingerprint": digest, "snapshot": safe}
        except ImportError:
            return {"state": "blocked", "reason": "backend_unavailable"}
        except Exception:
            return {"state": "blocked", "reason": "inspect_failed"}

    def launch(self, request_id):
        lock = None
        try:
            path, request, digest = load_request(self.runtime, request_id)
            self.records.mkdir(parents=True, exist_ok=True)
            record_path = self.path(request_id)
            lock = record_path.with_suffix(".lock")
            try:
                handle = os.open(str(lock), os.O_CREAT | os.O_EXCL | os.O_WRONLY)
                os.close(handle)
            except FileExistsError:
                lock = None
                if record_path.exists() and read_json(record_path).get("request_fingerprint") == digest:
                    return self.status(request_id)
                return {"launch_id": request_id, "state": "unknown", "reason": "dispatch_already_owned"}
            if record_path.exists():
                previous = read_json(record_path)
                if previous.get("request_fingerprint") != digest:
                    return {"state": "blocked", "reason": "request_changed"}
                if previous.get("state") != "retryable" or request.get("handoff_id"):
                    return self.status(request_id)
            scheduler = scheduler_state(self.runtime)
            entry = matching_entry(self.runtime, request)
            if not request.get("handoff_id"):
                return {"state": "blocked", "reason": "handoff_protocol_required"}
            if entry["status"] not in ("waiting", "waiting_ready", "launch_started"):
                return {"state": "blocked", "reason": "scheduler_not_waiting"}
            if scheduler["mode"] != "running" or expiry(request) <= self.clock():
                return {"launch_id": request_id, "state": "retryable", "reason": "not_ready_before_start", "launched": False}
            if not self.available():
                return {"state": "blocked", "reason": "backend_unavailable"}
            value = {"launch_id": request_id, "request_fingerprint": digest, "state": "issued",
                "generation": scheduler["generation"], "expires_at": expiry(request), "updated_at": self.clock()}
            write_json(record_path, value)
            try:
                pid = self.spawner(self.runtime, request_id)
                if type(pid) is not int or pid <= 0:
                    raise ValueError("invalid_worker_pid")
                value.update(state="accepted", pid=pid)
            except OSError:
                value.update(state="retryable", reason="worker_not_started", launched=False)
            except Exception:
                value.update(state="unknown", reason="worker_start_unknown")
            write_json(record_path, value)
            return self.status(request_id)
        except (OSError, ValueError, TypeError, sqlite3.Error):
            return {"state": "blocked", "reason": "launch_preflight_failed"}
        finally:
            if lock is not None:
                lock.unlink(missing_ok=True)

    def worker(self, request_id):
        record_path = self.path(request_id)
        lock = record_path.with_suffix(".lock")
        owned = False
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    handle = os.open(str(lock), os.O_CREAT | os.O_EXCL | os.O_WRONLY)
                    os.close(handle)
                    owned = True
                    break
                except FileExistsError:
                    time.sleep(0.05)
            if not owned:
                return 2
            record = read_json(record_path)
            if record.get("state") not in ("issued", "accepted"):
                return 2
            path, request, digest = load_request(self.runtime, request_id)

            if digest != record["request_fingerprint"]:
                result = {"state": "blocked", "reason": "request_changed"}
            elif (scheduler_state(self.runtime) != {"mode": "running", "generation": record["generation"]}
                    or expiry(request) <= self.clock()):
                result = {"state": "retryable", "reason": "stopped_before_worker", "launched": False}
            else:
                record.update(state="running", updated_at=self.clock())
                write_json(record_path, record)
                try:
                    result = self.coordinator(self.runtime, request_id, record["generation"], self.loader)
                    if not isinstance(result, dict) or result.get("state") not in TERMINAL | {"retryable"}:
                        result = {"state": "unknown", "reason": "invalid_coordinator_result"}
                    elif result["state"] == "retryable" and result.get("launched") is not False:
                        result = {"state": "unknown", "reason": "missing_not_started_proof"}
                except Exception:
                    result = {"state": "unknown", "reason": "scheduler_outcome_unknown"}
            record.update(state=result["state"], result=result, updated_at=self.clock())
            write_json(record_path, record)
            return 0
        except Exception:
            # Leave an issued record non-replayable if even result persistence
            # fails; never manufacture a known not-started receipt.
            return 2
        finally:
            if owned:
                lock.unlink(missing_ok=True)


def dispatch(service, name, arguments):
    if name not in {spec["name"] for spec in TOOL_SPECS} or not isinstance(arguments, dict):
        raise ValueError("unknown_tool_or_arguments")
    if set(arguments) - {"launch_id"} or (name != "handoff_status" and "launch_id" not in arguments):
        raise ValueError("invalid_tool_arguments")
    request_id = arguments.get("launch_id")
    if "launch_id" in arguments:
        launch_id(request_id)
    return getattr(service, name.removeprefix("handoff_"))(request_id)


def serve_native(service, source, destination):
    initialized = False

    def send(value):
        data = json.dumps(value, ensure_ascii=False, allow_nan=False).encode("utf-8")
        if len(data) > MAX_FRAME:
            raise ValueError("response_too_large")
        destination.write(data + b"\n")
        destination.flush()

    def error(request_id, code, reason):
        send({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": reason}})

    while True:
        line = source.readline(MAX_FRAME + 1)
        if not line:
            return
        if len(line) > MAX_FRAME:
            error(None, -32600, "request_frame_too_large")
            return
        try:
            request = json.loads(line.decode("utf-8"), object_pairs_hook=unique_object,
                parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
            if not isinstance(request, dict):
                raise ValueError()
        except (ValueError, UnicodeError, RecursionError):
            error(None, -32700, "invalid_json")
            continue
        request_id, params = request.get("id"), request.get("params", {})
        if request.get("jsonrpc") != "2.0" or type(request_id) not in (str, int) or not isinstance(params, dict):
            error(None, -32600, "invalid_request")
            continue
        method = request.get("method")
        if method == "initialize":
            if params.get("protocolVersion") != PROTOCOL_VERSION:
                error(request_id, -32602, "unsupported_protocol")
                continue
            initialized = True
            result = {"protocolVersion": PROTOCOL_VERSION}
        elif not initialized:
            error(request_id, -32000, "initialize_first")
            continue
        elif method == "tools/list":
            result = {"tools": TOOL_SPECS}
        elif method == "tools/call":
            try:
                value = dispatch(service, params.get("name"), params.get("arguments", {}))
                result = {"content": [{"type": "text", "text": "See structured handoff state."}],
                    "structuredContent": value, "isError": False}
            except (ValueError, TypeError):
                error(request_id, -32602, "invalid_tool_arguments")
                continue
            except Exception:
                error(request_id, -32603, "tool_failed")
                continue
        else:
            error(request_id, -32601, "method_not_found")
            continue
        send({"jsonrpc": "2.0", "id": request_id, "result": result})


def build_mcp(service):
    from mcp.server.fastmcp import FastMCP
    from mcp.types import ToolAnnotations
    server = FastMCP("edge-handoff", instructions="Only fixed local handoff requests are supported. No arbitrary browser or OS execution.")

    @server.tool(name="handoff_status", annotations=ToolAnnotations(readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False))
    def status(launch_id: str | None = None) -> dict:
        """Read scheduler and worker state; accepted does not mean sent."""
        return service.status(launch_id)

    @server.tool(name="handoff_inspect", annotations=ToolAnnotations(readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False))
    def inspect(launch_id: str) -> dict:
        """Read bounded backend state for one fixed local request."""
        return service.inspect(launch_id)

    @server.tool(name="handoff_launch", annotations=ToolAnnotations(readOnlyHint=False, destructiveHint=False, idempotentHint=True, openWorldHint=True))
    def launch(launch_id: str) -> dict:
        """Start one hidden worker for a fixed runtime/requests/<UUID>.json request, then poll status."""
        return service.launch(launch_id)

    return server


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, default=DEFAULT_RUNTIME)
    parser.add_argument("--worker", help=argparse.SUPPRESS)
    parser.add_argument("--native-plugin", action="store_true", help="Use WebCodex Runner native JSONL protocol")
    args = parser.parse_args(argv)
    os.environ["WEBCODEX_HANDOFF_RUNTIME"] = str(args.runtime.resolve())
    service = Service(args.runtime)
    if args.worker:
        return service.worker(args.worker)
    if args.native_plugin:
        serve_native(service, sys.stdin.buffer, sys.stdout.buffer)
        return 0
    build_mcp(service).run(transport="stdio")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
