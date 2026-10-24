#!/usr/bin/env python3
"""Isolated real Server/Runner host handoff acceptance; no browser or model API.

The executor is an explicit-context test driver, not a real ChatGPT conversation.
Only this process's temporary profiles, server, runner and native file operations
are used. Requires Windows Job Objects for guaranteed descendant cleanup.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import socket
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

from e2e_goal_mode import Api, HarnessError, OwnedProcesses, assert_check, isolated_environment, toml, wait_until


def load_controller(path):
    spec = importlib.util.spec_from_file_location("local_handoff_controller", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def call(api, name, params, epoch=None, binding=None, operation=None):
    params = dict(params)
    if epoch:
        params["context_epoch"] = epoch
    if binding is not None:
        if not operation:
            raise HarnessError("Bound requests require an explicit operation id")
        params["goal_context"] = {**binding, "operation_id": operation, "context_epoch": epoch}
    return api.tool(name, params)


def current(api, goal_id, epoch):
    return call(api, "goal_read", {"goal_id": goal_id}, epoch)["goal"]


def denied_call(api, payload):
    """Expected guard denials may use HTTP 400 rather than a 200 tool envelope."""
    request = urllib.request.Request(api.base + "/api/tools/call", json.dumps(payload).encode(),
        {"Authorization": "Bearer " + api.token, "Content-Type": "application/json"})
    try:
        response = api.opener.open(request, timeout=min(10, api.remaining()))
    except urllib.error.HTTPError as error:
        response = error
    with response:
        data = response.read(1_048_577)
        if len(data) > 1_048_576:
            raise HarnessError("Guard denial response exceeded evidence bound")
        body = json.loads(data)
        if response.status >= 500:
            raise HarnessError("Guard denial returned a Server error")
        return body


def plan():
    final = {"kind": "file_contains", "id": "final-result", "description": "Successor writes verified result", "path": "b_result.txt", "text": "handoff-ready:"}
    acceptance = {**final, "id": "node-result"}
    return {
        "objective": "Recover one local host Goal and produce the original input hash",
        "constraints": ["Only this temporary project", "No external services or model API"],
        "deliverables": ["a_input.txt", "b_result.txt"], "final_criteria": [final],
        "nodes": [{"id": "recover", "title": "Recover and verify original input", "inputs": ["Original test input"],
            "work": "A writes input; B reads same input and records SHA-256 after atomic handoff",
            "outputs": ["b_result.txt"], "criterion_ids": ["final-result"], "acceptance": [acceptance],
            "failure_action": "Park and retain original artifacts"}],
        "state_management": {"facts": "Record input hash", "decisions": "Preserve Goal", "issues": "Keep unresolved operations",
            "artifacts": "Record exact paths", "checkpoint_triggers": ["Before changing epoch"],
            "resume_instructions": "Read original input and claim a new attempt before writing"},
    }


def run_acceptance(args, root, processes, report, sensitive):
    hc = load_controller(args.controller)
    project = root / "project"
    for directory in (project, root / "projects", root / "data"):
        directory.mkdir()
    (root / "empty.env").write_text("# Isolated host handoff acceptance\n", encoding="utf-8")
    (project / "AGENTS.md").write_text("Only this temporary project is authorized. No browser or external services.\n", encoding="utf-8")
    environment = isolated_environment(root, dict(os.environ))
    bootstrap = secrets.token_urlsafe(40)
    sensitive.append(bootstrap)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    deadline = time.monotonic() + args.timeout_secs
    server_environment = {**environment, "WEBCODEX_ADDR": f"127.0.0.1:{port}", "WEBCODEX_DATA": str(root / "data"),
        "WEBCODEX_TOKEN": bootstrap, "WEBCODEX_ENV_FILE": str(root / "empty.env")}
    server_argv = [str(Path(args.server_bin).resolve()), "--stop-on-stdin-eof"]
    server = processes.start("server", server_argv, root, server_environment)
    admin = Api(port, bootstrap, deadline)
    wait_until(admin, processes, lambda: admin.post("/api/runtime/status", {}))
    admin.post("/api/users/create", {"username": "handoff-e2e", "display_name": "Isolated Host Handoff Test"})
    writer = admin.post("/api/tokens/create", {"username": "handoff-e2e", "name": "test-writer", "scopes": ["runtime:read", "project:read", "project:write"]})["token"]
    observer = admin.post("/api/tokens/create", {"username": "handoff-e2e", "name": "read-only-controller", "scopes": ["project:read"]})["token"]
    runner_token = admin.post("/api/agent-tokens/create", {"username": "handoff-e2e", "client_id": "handoff-e2e-runner", "name": "isolated-runner"})["token"]
    sensitive.extend((writer, observer, runner_token))
    config = f"""server_url = {toml(f'http://127.0.0.1:{port}')}
token = {toml(runner_token)}
client_id = "handoff-e2e-runner"
display_name = "Isolated host handoff Runner"
owner = "handoff-e2e"
project_registry_dir = {toml(str(root / 'projects'))}
poll_interval_ms = 200
transport = "websocket"

[policy]
allow_raw_shell = false
allow_cwd_anywhere = false
allowed_roots = [{toml(str(project))}]
max_timeout_secs = 30
max_output_bytes = 262144
"""
    (root / "runner.toml").write_text(config, encoding="utf-8")
    (root / "projects" / "test-project.toml").write_text(f'id = "test-project"\nname = "Handoff Test Project"\npath = {toml(str(project))}\nallow_patch = true\n', encoding="utf-8")
    processes.start("runner", [str(Path(args.runner_bin).resolve()), "--config", str(root / "runner.toml"), "--stop-on-stdin-eof"], root, environment)
    api = Api(port, writer, deadline)
    project_id = "agent:handoff-e2e-runner:test-project"
    ready = lambda: any(item.get("id") == project_id for item in api.tool("list_projects", {}).get("projects", []))
    wait_until(api, processes, ready)
    epoch_a, epoch_b, planner = "handoff-test-A", "handoff-test-B", "handoff-test-plan"
    started = call(api, "goal_mode_start", {"project": project_id, "objective": plan()["objective"], "idempotency_key": "start",
        "context_warning_tokens": 3000, "context_handoff_tokens": 4000}, epoch_a)
    goal_id = started["goal"]["goal_id"]
    call(api, "goal_bootstrap", {"goal_id": goal_id}, planner)
    goal = current(api, goal_id, planner)
    saved = call(api, "goal_plan_save", {"goal_id": goal_id, "expected_revision": goal["revision"], "idempotency_key": "save-plan", "plan": plan()}, planner)
    goal = saved["goal"]
    confirmed = call(api, "goal_confirm_plan", {"goal_id": goal_id, "expected_revision": goal["revision"], "plan_version": goal["plan_version"], "plan_sha256": goal["plan_hash"],
        "confirmation": "This is the user-authorized isolated test plan. Confirm only this temporary Goal.", "idempotency_key": "confirm-plan"}, planner)
    node_read = call(api, "goal_read", {"goal_id": goal_id, "view": "node", "node_id": "recover"}, epoch_a)
    goal = confirmed["goal"]
    claimed = call(api, "goal_claim_node", {"goal_id": goal_id, "node_id": "recover", "expected_revision": goal["revision"], "plan_version": goal["plan_version"], "idempotency_key": "claim-A"}, epoch_a)
    binding_a = claimed["binding"]
    goal = claimed["goal"]
    content = "original isolated handoff input"
    written = call(api, "write_project_file", {"project": project_id, "path": "a_input.txt", "content": content}, epoch_a, binding_a, "write-A")
    assert_check((project / "a_input.txt").read_text(encoding="utf-8") == content, "A wrote original input via real Runner", report)
    meter_trace = [result.get("context_meter", {}) for result in (started, node_read, claimed, written)]
    # These reads target only the isolated local Server. No website traffic.
    for _ in range(8):
        if meter_trace[-1].get("lifecycle_state") == "draining":
            break
        observed = call(api, "goal_read", {"goal_id": goal_id}, epoch_a)
        goal = observed["goal"]
        meter_trace.append(observed.get("context_meter", {}))
    report["a_meter_trace"] = [{key: meter.get(key) for key in ("lifecycle_state", "observed_tokens", "warning_tokens", "handoff_tokens")} for meter in meter_trace]
    assert_check(all(m.get("warning_tokens") == 3000 and m.get("handoff_tokens") == 4000 for m in meter_trace), "A keeps exact 3000/4000 test thresholds", report)
    assert_check(any(m.get("lifecycle_state") == "warning" for m in meter_trace), "A emits WARNING through real HTTP responses", report)
    assert_check(meter_trace[-1].get("lifecycle_state") == "draining", "A reaches persistent DRAINING within eight bounded reads", report)
    denied_draining = denied_call(api, {"tool": "write_project_file", "params": {"project": project_id, "path": "draining-forbidden.txt", "content": "must not exist", "context_epoch": epoch_a,
        "goal_context": {**binding_a, "operation_id": "write-draining-A", "context_epoch": epoch_a}}})
    assert_check(denied_draining.get("success") is False and denied_draining.get("output", {}).get("error_kind") == "goal_context_draining"
        and not (project / "draining-forbidden.txt").exists() and (project / "a_input.txt").read_text(encoding="utf-8") == content,
        "DRAINING rejects a new write without changing original input", report)
    handoff_params = {"goal_id": goal_id, "expected_revision": goal["revision"], "idempotency_key": "handoff-A",
        "patch": {"action": "handoff", "reason": "Transfer this test Goal to B", "state": {"completed_summary": "Original input written", "next_step": "Read original input and produce SHA-256"}}}
    call(api, "goal_checkpoint", handoff_params, epoch_a, binding_a, "handoff-A-request")
    replay = call(api, "goal_checkpoint", handoff_params, epoch_a, binding_a, "handoff-A-request")
    assert_check(replay.get("replayed") is True, "Lost handoff receipt replays without another checkpoint", report)
    observer_api = hc.Api(f"http://127.0.0.1:{port}", observer)
    store_path = root / "controller.sqlite3"
    store = hc.Store(store_path)
    observation_config = {"goal_id": goal_id, "project": project_id, "source_context_epoch": epoch_a, "endpoint": f"http://127.0.0.1:{port}"}
    try:
        store.initialize(observation_config)
        state = hc.poll_once(store, observer_api)
        report["first_observation"] = state
        assert_check(state["failures"] == 0 and state["mode"] == "running" and state["snapshot"]["lifecycle_state"] == "retired" and state["snapshot"]["checkpoint_sequence"] is not None, "Read-only credential observes authoritative A retirement", report)
        launch_id = state["snapshot"]["launch_id"]
        source_checkpoint = state["snapshot"]["checkpoint_sequence"]
        store.set_mode("stopped")
    finally:
        store.close()
    denied = denied_call(api, {"tool": "write_project_file", "params": {"project": project_id, "path": "forbidden.txt", "content": "must not exist", "context_epoch": epoch_a,
        "goal_context": {**binding_a, "operation_id": "write-retired-A", "context_epoch": epoch_a}}})
    report["retired_write_error_kind"] = denied.get("output", {}).get("error_kind")
    assert_check(denied.get("success") is False and denied.get("output", {}).get("error_kind") == "goal_chat_retired"
        and not (project / "forbidden.txt").exists(), "Retired A cannot dispatch a new write", report)
    # Gracefully stop only the owned Server; same temporary DB and Runner remain.
    server.stdin.close()
    server.wait(timeout=min(15, api.remaining()))
    assert_check(server.returncode == 0, "Only owned temporary Server exits cleanly before restart", report)
    processes.processes = [(name, process) for name, process in processes.processes if process is not server]
    processes.start("server-restarted", server_argv, root, server_environment)
    wait_until(admin, processes, lambda: admin.post("/api/runtime/status", {}))
    wait_until(api, processes, ready)
    store = hc.Store(store_path)
    try:
        assert_check(store.status()["mode"] == "stopped", "Controller STOP survives cold reopening", report)
        state = hc.poll_once(store, observer_api, resume=True)
        assert_check(state["mode"] == "running" and state["snapshot"]["launch_id"] == launch_id, "Resume freshly verifies same durable handoff after Server restart", report)
        recovered = call(api, "goal_bootstrap", {"goal_id": goal_id}, epoch_b)
        assert_check(recovered["goal"]["goal_id"] == goal_id, "B bootstrap preserves same Goal", report)
        b_meter = recovered.get("context_meter", {})
        report["b_meter"] = {key: b_meter.get(key) for key in ("context_epoch", "lifecycle_state", "warning_tokens", "handoff_tokens")}
        assert_check(b_meter.get("context_epoch") == epoch_b and b_meter.get("warning_tokens") == 600000 and b_meter.get("handoff_tokens") == 800000,
            "B receives normal 600000/800000 defaults independently of A", report)
        state = hc.poll_once(store, observer_api)
        assert_check(state["snapshot"]["phase"] == "WAITING_NEW_CHAT", "Bootstrap alone never counts as successor claim", report)
        call(api, "goal_read", {"goal_id": goal_id, "view": "node", "node_id": "recover"}, epoch_b)
        goal = current(api, goal_id, epoch_b)
        b_claim = call(api, "goal_claim_node", {"goal_id": goal_id, "node_id": "recover", "expected_revision": goal["revision"], "plan_version": goal["plan_version"],
            "idempotency_key": "claim-B", "takeover_attempt_id": binding_a["attempt_id"]}, epoch_b)
        binding_b = b_claim["binding"]
        assert_check(binding_b["fence"] > binding_a["fence"] and binding_b["attempt_id"] != binding_a["attempt_id"], "B obtains a new attempt and advances fence", report)
        read = call(api, "read_file", {"project": project_id, "path": "a_input.txt"}, epoch_b)
        original = read.get("text")
        assert_check(original == content, "B reads A's original input through real Runner", report)
        digest = hashlib.sha256(original.encode()).hexdigest()
        result = "handoff-ready:" + digest + "\n"
        call(api, "write_project_file", {"project": project_id, "path": "b_result.txt", "content": result}, epoch_b, binding_b, "write-B")
        goal = current(api, goal_id, epoch_b)
        checked = call(api, "goal_check_node", {"goal_id": goal_id, "node_id": "recover", "attempt_id": binding_b["attempt_id"], "expected_revision": goal["revision"],
            "plan_version": goal["plan_version"], "idempotency_key": "check-B"}, epoch_b, binding_b, "check-B-request")
        assert_check(checked["goal"]["phase"] == "completed", "Successor node and final native file criteria pass", report)
        state = hc.poll_once(store, observer_api)
        assert_check(state["mode"] == "running" and state["snapshot"]["phase"] == "BUSINESS_VERIFIED", "Controller verifies successor business and passed attempt", report)
        assert_check(state["snapshot"]["checkpoint_sequence"] == source_checkpoint and state["snapshot"]["launch_id"] == launch_id, "B cannot overwrite fixed A checkpoint evidence", report)
        assert_check((project / "a_input.txt").read_text(encoding="utf-8") == content and (project / "b_result.txt").read_text(encoding="utf-8") == result, "Original input unchanged and successor output hash exact", report)
        report.update(goal_id=goal_id, source_checkpoint=source_checkpoint, assurance="local_protocol_only", identity_source="explicit_context_only", web_unattended_verified=False)
    finally:
        store.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-bin", required=True, type=Path)
    parser.add_argument("--runner-bin", required=True, type=Path)
    parser.add_argument("--controller", required=True, type=Path)
    parser.add_argument("--timeout-secs", type=int, default=240)
    parser.add_argument("--output-dir", type=Path)
    args = parser.parse_args(argv)
    if os.name != "nt":
        parser.error("Windows Job Objects are required for isolated child cleanup")
    if not 30 <= args.timeout_secs <= 3600:
        parser.error("timeout must be 30..3600 seconds")
    for path in (args.server_bin, args.runner_bin, args.controller):
        if not path.is_file():
            parser.error("server, runner and controller must name existing files")
    report = {"mode": "explicit_context_test_driver", "started_at": dt.datetime.now(dt.timezone.utc).isoformat(), "passed": False, "checks": [], "web_unattended_verified": False}
    sensitive = []
    try:
        with tempfile.TemporaryDirectory(prefix="webcodex-host-handoff-e2e-") as directory:
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
    for secret in sorted(sensitive, key=len, reverse=True):
        serialized = serialized.replace(secret, "<redacted>")
    serialized = re.sub(r"Bearer\s+[^\s\"\\]+", "Bearer <redacted>", serialized)
    if args.output_dir:
        args.output_dir.mkdir(parents=True, exist_ok=True)
        name = "host-handoff-" + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:8] + ".json"
        with (args.output_dir / name).open("x", encoding="utf-8") as target:
            target.write(serialized + "\n")
    print(serialized)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
