#!/usr/bin/env python3
"""Isolated Server + Runner + ACP goal acceptance harness (stdlib only).

Default: this script supplies a deterministic stdio ACP fixture. A real provider
is opt-in through --provider-executable and repeated --provider-arg/--env-pass.
Never connects to existing services or loads production WebCodex profiles.
"""
from __future__ import annotations

import argparse
import ctypes
import datetime as dt
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid


OS_ENV = {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "LANG", "LC_ALL", "TZ"}
BLOCKED_PASS = {"WEBCODEX_TOKEN", "WEBCODEX_RUNNER_TOKEN", "WEBCODEX_AGENT_TOKEN"}
MAX_RESPONSE = 2 * 1024 * 1024


class HarnessError(Exception):
    pass


def toml(value: object) -> str:
    # JSON string/array syntax is also valid TOML for these bounded values.
    return json.dumps(value, ensure_ascii=False)


def environment_value(name: str, environment: dict[str, str]) -> str | None:
    return next((value for key, value in environment.items() if key.upper() == name.upper()), None)


def isolated_environment(root: Path, original: dict[str, str]) -> dict[str, str]:
    environment = {key: value for key, value in original.items() if key.upper() in OS_ENV}
    for name, directory in {
        "LOCALAPPDATA": "state", "APPDATA": "config", "USERPROFILE": "profile",
        "XDG_STATE_HOME": "state", "XDG_CONFIG_HOME": "config",
        "TEMP": "tmp", "TMP": "tmp", "TMPDIR": "tmp",
    }.items():
        target = root / directory
        target.mkdir(exist_ok=True)
        environment[name] = str(target)
    environment["RUST_LOG"] = "warn"
    return environment


class WindowsProcessJob:
    """One private kill-on-close Windows Job owns all harness descendants."""

    def __init__(self) -> None:
        from ctypes import wintypes as w

        class BasicLimits(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_longlong), ("job_time", ctypes.c_longlong),
                        ("flags", w.DWORD), ("minimum", ctypes.c_size_t), ("maximum", ctypes.c_size_t),
                        ("active_limit", w.DWORD), ("affinity", ctypes.c_size_t),
                        ("priority", w.DWORD), ("scheduling", w.DWORD)]

        class IoCounters(ctypes.Structure):
            _fields_ = [(name, ctypes.c_ulonglong) for name in
                        ("read_ops", "write_ops", "other_ops", "read_bytes", "write_bytes", "other_bytes")]

        class ExtendedLimits(ctypes.Structure):
            _fields_ = [("basic", BasicLimits), ("io", IoCounters),
                        ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                        ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]

        class Accounting(ctypes.Structure):
            _fields_ = [(name, ctypes.c_longlong) for name in
                        ("user_time", "kernel_time", "period_user", "period_kernel")] + [
                        (name, w.DWORD) for name in ("faults", "total", "active", "terminated")]

        self.accounting_type = Accounting
        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.CreateJobObjectW.argtypes = [ctypes.c_void_p, w.LPCWSTR]
        self.api.CreateJobObjectW.restype = w.HANDLE
        self.api.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
        self.api.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
        self.api.TerminateJobObject.argtypes = [w.HANDLE, w.UINT]
        self.api.QueryInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD, ctypes.c_void_p]
        self.api.CloseHandle.argtypes = [w.HANDLE]
        self.ntdll = ctypes.WinDLL("ntdll")
        self.ntdll.NtResumeProcess.argtypes = [w.HANDLE]
        self.ntdll.NtResumeProcess.restype = ctypes.c_long
        self.handle = self.api.CreateJobObjectW(None, None)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = ExtendedLimits()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not self.api.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            error = ctypes.WinError(ctypes.get_last_error())
            self.api.CloseHandle(self.handle)
            raise error

    def assign(self, process: subprocess.Popen) -> None:
        if not self.api.AssignProcessToJobObject(self.handle, int(process._handle)):
            raise ctypes.WinError(ctypes.get_last_error())

    def resume(self, process: subprocess.Popen) -> None:
        status = self.ntdll.NtResumeProcess(int(process._handle))
        if status < 0:
            self.api.TerminateJobObject(self.handle, 1)
            raise HarnessError(f"NtResumeProcess failed with NTSTATUS 0x{status & 0xffffffff:08x}")

    def cleanup(self) -> bool:
        try:
            if not self.api.TerminateJobObject(self.handle, 1):
                raise ctypes.WinError(ctypes.get_last_error())
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                accounting = self.accounting_type()
                if not self.api.QueryInformationJobObject(self.handle, 1, ctypes.byref(accounting), ctypes.sizeof(accounting), None):
                    raise ctypes.WinError(ctypes.get_last_error())
                if accounting.active == 0:
                    return True
                time.sleep(0.05)
            return False
        finally:
            self.api.CloseHandle(self.handle)


class OwnedProcesses:
    def __init__(self) -> None:
        self.processes: list[tuple[str, subprocess.Popen]] = []
        self.logs: list[object] = []
        self.job = WindowsProcessJob()

    def start(self, name: str, argv: list[str], root: Path, environment: dict[str, str]) -> subprocess.Popen:
        log = (root / f"{name}.log").open("wb")
        self.logs.append(log)
        # Assign while suspended: no executable code can spawn a child outside
        # the private Job between CreateProcess and AssignProcessToJobObject.
        flags = subprocess.CREATE_NO_WINDOW | subprocess.CREATE_NEW_PROCESS_GROUP | 0x00000004  # CREATE_SUSPENDED
        process = subprocess.Popen(argv, cwd=root, env=environment, stdin=subprocess.PIPE,
                                   stdout=log, stderr=subprocess.STDOUT, creationflags=flags)
        self.processes.append((name, process))
        if self.job:
            try:
                self.job.assign(process)
                self.job.resume(process)
            except Exception:
                process.kill()
                process.wait(timeout=8)
                raise
        return process

    def check(self) -> None:
        for name, process in self.processes:
            if process.poll() is not None:
                raise HarnessError(f"owned {name} exited unexpectedly with code {process.returncode}")

    def cleanup(self) -> dict:
        result = {"method": "windows_job", "processes": []}
        try:
            result["tree_empty"] = self.job.cleanup()
            for name, process in self.processes:
                try:
                    process.wait(timeout=8)
                    result["processes"].append({"name": name, "exited": True})
                except subprocess.TimeoutExpired:
                    result["processes"].append({"name": name, "exited": False})
                    result["tree_empty"] = False
                if process.stdin:
                    process.stdin.close()
        except Exception as error:
            result["tree_empty"] = False
            result["error"] = type(error).__name__
        finally:
            for log in self.logs:
                log.close()
        return result


class Api:
    def __init__(self, port: int, token: str, deadline: float) -> None:
        self.base = f"http://127.0.0.1:{port}"
        self.token = token
        self.deadline = deadline
        # Loopback requests must never inherit the user's proxy routing.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def remaining(self) -> float:
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise HarnessError("overall acceptance deadline exceeded")
        return remaining

    def post(self, path: str, payload: dict) -> dict:
        request = urllib.request.Request(self.base + path, json.dumps(payload).encode(),
                                         {"Authorization": "Bearer " + self.token, "Content-Type": "application/json"})
        try:
            response = self.opener.open(request, timeout=min(15, self.remaining()))
        except urllib.error.HTTPError as error:
            response = error
        with response:
            data = response.read(MAX_RESPONSE + 1)
            if len(data) > MAX_RESPONSE:
                raise HarnessError("HTTP response exceeded harness evidence bound")
            body = json.loads(data)
            if response.status >= 400:
                kind = body.get("output", {}).get("error_kind", body.get("error", "http_error"))
                raise HarnessError(f"{path}: HTTP {response.status}: {str(kind)[:240]}")
            return body

    def tool(self, name: str, params: dict) -> dict:
        body = self.post("/api/tools/call", {"tool": name, "params": params})
        if body.get("success") is not True:
            raise HarnessError(f"{name} failed: {body.get('output', {}).get('error_kind', 'tool_error')}")
        return body["output"]


def wait_until(api: Api, processes: OwnedProcesses, predicate) -> object:
    while True:
        processes.check()
        api.remaining()
        try:
            value = predicate()
            if value:
                return value
        except (OSError, urllib.error.URLError, json.JSONDecodeError):
            pass
        time.sleep(min(0.2, api.remaining()))


def fixture_provider(expected_project: Path) -> int:
    """Small real stdio ACP peer; no mock of the Server/Runner transport."""
    expected_project = expected_project.resolve(strict=True)
    session_id = "fixture-" + uuid.uuid4().hex
    current_project: Path | None = None
    mode = "agent"

    def send(value: dict) -> None:
        print(json.dumps(value, ensure_ascii=False), flush=True)

    def configs() -> list[dict]:
        return [{"id": "mode", "name": "Fixture mode", "type": "select", "currentValue": mode,
                 "options": [{"value": "agent", "name": "Agent"}, {"value": "read-only", "name": "Read only"}]}]

    for line in sys.stdin:
        message = json.loads(line)
        method, request_id = message.get("method"), message.get("id")
        params = message.get("params", {})
        result: dict = {}
        if method == "initialize":
            result = {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": [],
                      "agentInfo": {"name": "goal-e2e-fixture", "version": "1"}}
        elif method == "session/new":
            candidate = Path(params["cwd"]).resolve(strict=True)
            # Runner canonicalization can add the Windows verbatim prefix;
            # compare directory identity, not two spellings of the same path.
            if not candidate.samefile(expected_project):
                raise HarnessError("fixture refused unexpected project root")
            current_project = candidate
            result = {"sessionId": session_id, "configOptions": configs()}
        elif method == "session/set_config_option":
            mode = params["value"]
            result = {"configOptions": configs()}
        elif method == "session/cancel":
            if request_id is not None:
                send({"jsonrpc": "2.0", "id": request_id, "result": {}})
            continue
        elif method == "session/prompt":
            if current_project is None or params.get("sessionId") != session_id:
                raise HarnessError("fixture prompt has no admitted session")
            prompt = "\n".join(item.get("text", "") for item in params.get("prompt", []))
            if "CONTRACT:\n" not in prompt:
                raise HarnessError("fixture prompt missing acceptance contract")
            contract, _ = json.JSONDecoder().raw_decode(prompt.split("CONTRACT:\n", 1)[1])
            if prompt.startswith("Independently verify"):
                send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session_id,
                    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Provider transport warning.\n\n"}}}})
                send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session_id,
                    "update": {"sessionUpdate": "agent_message_chunk", "_meta": {"codex": {"phase": "commentary"}},
                               "content": {"type": "text", "text": "I will independently inspect the artifact."}}}})
                passed = (current_project / "result.txt").read_text(encoding="utf-8").strip() == "goal-ready"
                output = json.dumps({"checks": [{"criterion_id": item["id"], "passed": passed,
                    "evidence": "Inspected result.txt; exact goal-ready content " + ("present" if passed else "absent")}
                    for item in contract["criteria"] if item["kind"] == "review"]})
            else:
                counter_path = current_project / ".goal-fixture-executions"
                counter = int(counter_path.read_text()) + 1 if counter_path.exists() else 1
                counter_path.write_text(str(counter), encoding="utf-8")
                (current_project / "result.txt").write_text("draft\n" if counter == 1 else "goal-ready\n", encoding="utf-8")
                # Deliberately claim completion in round one; real checks must reject it.
                output = "Task complete. Fixture execution round " + str(counter)
            send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session_id,
                  "update": {"sessionUpdate": "agent_message_chunk", "_meta": {"codex": {"phase": "final_answer"}},
                             "content": {"type": "text", "text": output}}}})
            result = {"stopReason": "end_turn"}
        elif method is None:
            continue
        else:
            if request_id is not None:
                send({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "Unsupported fixture method"}})
            continue
        if request_id is not None:
            send({"jsonrpc": "2.0", "id": request_id, "result": result})
    return 0


def runner_config(root: Path, project: Path, port: int, runner_token: str,
                  executable: str, provider_args: list[str], mappings: dict[str, str], config_options: list[str] | None = None) -> str:
    text = f"""server_url = {toml(f'http://127.0.0.1:{port}')}
token = {toml(runner_token)}
client_id = "goal-e2e-runner"
display_name = "Isolated goal acceptance runner"
owner = "goal-e2e"
project_registry_dir = {toml(str(root / 'projects'))}
poll_interval_ms = 200
transport = "websocket"

[policy]
allow_raw_shell = true
allow_cwd_anywhere = false
allowed_roots = [{toml(str(project))}]
max_timeout_secs = 300
max_output_bytes = 262144

[acp]
max_concurrent_runs = 1
permission_timeout_secs = 5

[[acp.agents]]
id = "goal-e2e-provider"
name = "Isolated acceptance provider"
executable = {toml(executable)}
args = {toml(provider_args)}
allowed_config_options = {toml(config_options or [])}

[acp.agents.env_from_env]
"""
    return text + "".join(f"{toml(destination)} = {toml(source)}\n" for destination, source in mappings.items())


def assert_check(condition: bool, description: str, report: dict) -> None:
    report["checks"].append({"name": description, "passed": bool(condition)})
    if not condition:
        raise HarnessError(description)


def run_acceptance(args: argparse.Namespace, root: Path, processes: OwnedProcesses, report: dict, sensitive: list[str]) -> None:
    project = root / "project"
    project.mkdir()
    (root / "projects").mkdir()
    (root / "data").mkdir()
    (root / "empty.env").write_text("# Isolated goal acceptance; no production environment files.\n", encoding="utf-8")
    (project / "AGENTS.md").write_text("Work only in this temporary project. Do not access other projects, accounts, services, or configuration.\n", encoding="utf-8")
    original = dict(os.environ)
    environment = isolated_environment(root, original)
    bootstrap = secrets.token_urlsafe(40)
    sensitive.append(bootstrap)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    deadline = time.monotonic() + args.timeout_secs
    server_environment = {**environment, "WEBCODEX_ADDR": f"127.0.0.1:{port}", "WEBCODEX_DATA": str(root / "data"),
                          "WEBCODEX_TOKEN": bootstrap, "WEBCODEX_ENV_FILE": str(root / "empty.env")}
    processes.start("server", [str(Path(args.server_bin).resolve()), "--stop-on-stdin-eof"], root, server_environment)
    admin = Api(port, bootstrap, deadline)
    wait_until(admin, processes, lambda: admin.post("/api/runtime/status", {}))
    admin.post("/api/users/create", {"username": "goal-e2e", "display_name": "Isolated Goal E2E"})
    token_result = admin.post("/api/tokens/create", {"username": "goal-e2e", "name": "goal-e2e",
        "scopes": ["runtime:read", "project:read", "project:write", "job:run", "job:detach", "coding_agent:run"]})
    runner_result = admin.post("/api/agent-tokens/create", {"username": "goal-e2e", "client_id": "goal-e2e-runner", "name": "goal-e2e"})
    token, runner_token = token_result["token"], runner_result["token"]
    sensitive.extend([token, runner_token])
    api = Api(port, token, deadline)
    mappings = {key.upper(): key for key in environment if key.upper() in OS_ENV | {"TEMP", "TMP", "TMPDIR", "USERPROFILE", "LOCALAPPDATA", "APPDATA"}}
    for index, name in enumerate(args.env_pass):
        value = environment_value(name, original)
        if value is None:
            raise HarnessError(f"explicit provider environment variable is unavailable: {name}")
        alias = f"GOAL_E2E_PROVIDER_ENV_{index}"
        environment[alias] = value
        mappings[name.upper()] = alias
        if len(value) >= 4 and name.upper() not in {"INITIAL_AGENT_MODE", "NO_BROWSER"}:
            sensitive.append(value)
    if args.provider_executable:
        executable, provider_args = str(Path(args.provider_executable).resolve()), args.provider_arg
    else:
        executable = sys.executable
        provider_args = [str(Path(__file__).resolve()), "--fixture-provider", "--fixture-project", str(project)]
    (root / "projects" / "goal-project.toml").write_text(
        f'id = "goal-project"\nname = "Isolated Goal Project"\npath = {toml(str(project))}\nallow_patch = true\n', encoding="utf-8")
    config_path = root / "runner.toml"
    config_path.write_text(runner_config(root, project, port, runner_token, executable, provider_args, mappings,
        ["reasoning_effort"] if args.reasoning_effort else []), encoding="utf-8")
    if os.name != "nt":
        config_path.chmod(0o600)
    processes.start("runner", [str(Path(args.runner_bin).resolve()), "--config", str(config_path), "--stop-on-stdin-eof"], root, environment)
    project_id = "agent:goal-e2e-runner:goal-project"
    wait_until(api, processes, lambda: any(item.get("id") == project_id for item in api.tool("list_projects", {}).get("projects", [])))
    criteria = [
        {"kind": "file_exists", "id": "result_exists", "description": "result.txt exists", "path": "result.txt"},
        {"kind": "file_contains", "id": "result_ready", "description": "result.txt contains goal-ready", "path": "result.txt", "text": "goal-ready"},
        {"kind": "command", "id": "result_command", "description": "An independent native process verifies the complete file", "program": sys.executable,
         "args": ["-c", "from pathlib import Path; import sys; text=Path('result.txt').read_text(encoding='utf-8').strip(); print('native checker:', text); sys.exit(0 if text == 'goal-ready' else 1)"], "timeout_secs": 30},
    ]
    if args.review:
        criteria.append({"kind": "review", "id": "result_review", "description": "Read result.txt and verify that its complete trimmed content is exactly goal-ready."})
    params = {"project": project_id, "provider_id": "goal-e2e-provider", "objective":
        "Work only inside this temporary project. Create result.txt containing exactly goal-ready followed by a newline. "
        "Do not access or modify files outside this project; do not access accounts, network services, user configuration, "
        "or production resources. Do not modify acceptance criteria. Report only the result.",
        "criteria": criteria, "artifact_paths": ["result.txt"], "idempotency_key": "goal-e2e-" + uuid.uuid4().hex,
        "limits": {"max_rounds": 3, "total_timeout_secs": min(args.timeout_secs, 86400),
                   "max_no_progress_rounds": 3, "run_timeout_secs": min(300, args.timeout_secs)}}
    if args.review:
        params["reviewer_provider_id"] = "goal-e2e-provider"
    if args.reasoning_effort:
        params["config"] = {"reasoning_effort": args.reasoning_effort}
    started = api.tool("goal_start", params)
    goal_id = started["goal"]["goal_id"]
    report["goal_id"] = goal_id
    replayed = api.tool("goal_start", params)
    assert_check(replayed["goal"]["goal_id"] == goal_id and replayed.get("replayed") is True, "same start key replays one goal", report)

    def completed():
        result = api.tool("goal_read", {"goal_id": goal_id, "round_offset": 0, "round_limit": 5})
        state = result["goal"]["state"]
        if state in {"paused", "blocked", "cancelled"}:
            report["stopped_goal"] = {"state": state, "reason": result["goal"].get("reason")}
            for round_record in result["goal"].get("rounds", []):
                if round_record.get("review_output"):
                    report.setdefault("review_output_excerpt", []).append(round_record["review_output"][:2000])
                if round_record.get("active_check_job_id"):
                    report.setdefault("check_job_status", []).append(api.post("/api/jobs/status", {"job_id": round_record["active_check_job_id"]}))
                    report.setdefault("check_job_observations", []).append(api.tool("observe_jobs", {
                        "items": [{"job_id": round_record["active_check_job_id"]}], "tail_lines": 20}))
                for field in ("execution_run_id", "review_run_id"):
                    if round_record.get(field):
                        observation = api.tool("coding_agent_observe", {"run_id": round_record[field], "wait_secs": 0})
                        report.setdefault("run_terminal", []).append({"kind": field, "state": observation.get("state"), "terminal": observation.get("terminal"),
                            "event_kinds": [event.get("kind") for event in observation.get("events", [])],
                            "activity_labels": [event.get("label") for event in observation.get("events", []) if event.get("label")]})
            raise HarnessError("goal stopped before acceptance: " + state + ": " + str(result["goal"].get("reason", ""))[:400])
        return result if state == "completed" else None

    result = wait_until(api, processes, completed)
    rounds = result["goal"]["rounds"]
    count = result.get("total_rounds", len(rounds))
    report["round_count"] = count
    report["criterion_ids"] = [check["criterion_id"] for check in rounds[-1]["checks"]]
    assert_check(result.get("delivery_ready") is True and result.get("delivery_freshness") == "current", "completed artifacts are verified for delivery", report)
    assert_check((project / "result.txt").read_text(encoding="utf-8").strip() == "goal-ready", "actual result.txt has required content", report)
    if not args.provider_executable:
        assert_check(count == 2 and int((project / ".goal-fixture-executions").read_text()) == 2, "first failed round automatically advances exactly once", report)
        assert_check(any(check["criterion_id"] == "result_ready" and not check["passed"] for check in rounds[0]["checks"]), "executor completion claim does not bypass failed checks", report)
    assert_check(bool(rounds[-1]["checks"]) and all(check["passed"] and check["evidence"] for check in rounds[-1]["checks"]), "all required final checks retain evidence", report)
    (project / "result.txt").write_text("changed-after-acceptance\n", encoding="utf-8")
    changed = api.tool("goal_read", {"goal_id": goal_id})
    assert_check(changed["goal"]["state"] == "completed" and changed.get("delivery_ready") is False
                 and changed.get("delivery_freshness") == "changed", "changed artifacts invalidate delivery readiness", report)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--server-bin", help="path to this checkout's webcodex-server binary")
    result.add_argument("--runner-bin", help="path to this checkout's webcodex-runner binary")
    result.add_argument("--provider-executable", help="opt in to a real ACP executable (default: deterministic Python fixture)")
    result.add_argument("--provider-arg", action="append", default=[], help="repeat for each provider argument; use = for leading dashes")
    result.add_argument("--env-pass", action="append", default=[], help="explicit existing environment variable to pass only to the ACP provider")
    result.add_argument("--review", action="store_true", help="also exercise a separate ACP reviewer run")
    result.add_argument("--reasoning-effort", choices=["low", "medium", "high"], help="explicit live-provider reasoning setting for the bounded smoke")
    result.add_argument("--timeout-secs", type=int, default=240, help="single absolute deadline for startup and acceptance (default: 240)")
    result.add_argument("--output-dir", type=Path, help="preserve only the redacted JSON report in this directory")
    result.add_argument("--fixture-provider", action="store_true", help=argparse.SUPPRESS)
    result.add_argument("--fixture-project", type=Path, help=argparse.SUPPRESS)
    return result


def main(argv: list[str] | None = None) -> int:
    arguments = parser()
    args = arguments.parse_args(argv)
    if args.fixture_provider:
        if args.fixture_project is None:
            arguments.error("fixture provider requires --fixture-project")
        return fixture_provider(args.fixture_project)
    if os.name != "nt":
        arguments.error("this harness requires Windows Job Objects for verified descendant cleanup; use the existing Unix E2E harnesses on other systems")
    for name in ("server_bin", "runner_bin"):
        value = getattr(args, name)
        if not value or not Path(value).is_file():
            arguments.error("--" + name.replace("_", "-") + " must name an existing binary")
    if args.provider_executable and not Path(args.provider_executable).is_file():
        arguments.error("--provider-executable must name an existing file")
    if args.provider_arg and not args.provider_executable:
        arguments.error("--provider-arg requires --provider-executable")
    if args.reasoning_effort and not args.provider_executable:
        arguments.error("--reasoning-effort requires --provider-executable")
    if not 10 <= args.timeout_secs <= 86400:
        arguments.error("--timeout-secs must be 10..86400")
    for name in args.env_pass:
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) or name.upper().startswith("WEBCODEX_") or name.upper() in BLOCKED_PASS:
            arguments.error("--env-pass accepts provider environment names, never WebCodex transport configuration")
    if len({name.upper() for name in args.env_pass}) != len(args.env_pass):
        arguments.error("--env-pass names must be unique")
    report = {"mode": "live_provider" if args.provider_executable else "deterministic_fixture", "review": args.review,
              "reasoning_effort": args.reasoning_effort,
              "started_at": dt.datetime.now(dt.timezone.utc).isoformat(), "passed": False, "checks": []}
    sensitive: list[str] = []
    try:
        # tempfile owns the exact absolute root; no caller-provided path is ever
        # recursively deleted. User-requested report directories are not profiles.
        with tempfile.TemporaryDirectory(prefix="webcodex-goal-e2e-") as directory:
            root = Path(directory).resolve()
            processes = OwnedProcesses()
            try:
                run_acceptance(args, root, processes, report, sensitive)
                report["passed"] = True
            except Exception as error:
                report["error"] = str(error)[:1200]
            finally:
                report["cleanup"] = processes.cleanup()
                report["passed"] = report["passed"] and report["cleanup"].get("tree_empty") is True
        report["temporary_profile_removed"] = True
    except (Exception, KeyboardInterrupt) as error:
        report["passed"] = False
        report["error"] = str(error)[:1200] or type(error).__name__
    report["finished_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
    serialized = json.dumps(report, ensure_ascii=False, indent=2)
    for value in sorted(sensitive, key=len, reverse=True):
        serialized = serialized.replace(value, "<redacted>")
    serialized = re.sub(r"Bearer\s+[^\s\"\\]+", "Bearer <redacted>", serialized)
    if args.output_dir:
        args.output_dir.mkdir(parents=True, exist_ok=True)
        filename = "goal-e2e-" + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:8] + ".json"
        with (args.output_dir / filename).open("x", encoding="utf-8") as target:
            target.write(serialized + "\n")
    print(serialized)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
