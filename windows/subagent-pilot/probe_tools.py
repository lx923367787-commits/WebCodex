"""Experimental MCP facade for one operator-prepared, immutable chat task.

No production registration, caller credential forwarding, or arbitrary prompts.
The fixed request is prepared by this explicitly authorized local experiment.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "chat-handoff"))
from edge_tools import read_json, write_json, WORKER_ENV


def request(request_id):
    if str(uuid.UUID(request_id)) != request_id:
        raise ValueError("invalid_request_id")
    value = read_json(ROOT / "pilot-request.json")
    if value.get("launch_id") != request_id:
        raise ValueError("request_not_prepared_for_this_pilot")
    digest = hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
    return value, digest


def read_chat_task(request_id: str) -> dict:
    """Read this pilot's launch and observed answer; sent is not task success."""
    _, digest = request(request_id)
    path = ROOT / "dispatch" / (request_id + ".json")
    if not path.exists():
        return {"request_id": request_id, "state": "not_started"}
    record = read_json(path)
    if record["request_sha256"] != digest:
        raise ValueError("prepared_request_changed")
    result = ROOT / "evidence" / request_id / "result.json"
    if result.exists():
        record["observation"] = read_json(result)
    recovered = ROOT / "evidence" / request_id / "response.json"
    if recovered.exists():
        record["recovered_response"] = read_json(recovered)
    outcome = ROOT / "worker-results" / (request_id + ".json")
    if outcome.exists():
        record["worker_outcome"] = read_json(outcome)
    return record


def spawn_chat_task(request_id: str) -> dict:
    """Start the single prepared pilot asynchronously. Repeat the same ID to inspect;
    an issued/unknown launch is never sent again. No arbitrary task text is accepted.
    """
    _, digest = request(request_id)
    directory = ROOT / "dispatch"
    directory.mkdir(exist_ok=True)
    path = directory / (request_id + ".json")
    record = {"request_id": request_id, "request_sha256": digest,
              "state": "issued", "business_verified": False}
    try:
        with path.open("x", encoding="utf-8") as stream:
            json.dump(record, stream)
            stream.flush()
            os.fsync(stream.fileno())
    except FileExistsError:
        return read_chat_task(request_id)
    try:
        executable = Path(sys.executable)
        if os.name == "nt" and executable.with_name("pythonw.exe").is_file():
            executable = executable.with_name("pythonw.exe")
        environment = {key: value for key, value in os.environ.items() if key.upper() in WORKER_ENV}
        child = subprocess.Popen([str(executable), str(Path(__file__).resolve()), "--worker", request_id],
            cwd=str(ROOT), env=environment, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), close_fds=True)
        record.update(state="accepted", worker_pid=child.pid)
        write_json(path, record)
    except Exception:
        # Popen or persistence uncertainty is not evidence that no child started.
        record.update(state="unknown")
        write_json(path, record)
    return record


def worker(request_id):
    if str(uuid.UUID(request_id)) != request_id:
        raise ValueError("invalid_request_id")
    try:
        config, digest = request(request_id)
        dispatch = read_json(ROOT / "dispatch" / (request_id + ".json"))
        if dispatch["request_sha256"] != digest:
            raise ValueError("prepared_request_changed")
        import pilot_transport
        result = pilot_transport.run(config, ROOT / "evidence", collect_response=True)
        outcome = {"state": "finished", "result": result}
    except Exception as error:
        outcome = {"state": "unknown", "reason": type(error).__name__}
    # Separate ownership from the parent writer's accepted/PID receipt.
    write_json(ROOT / "worker-results" / (request_id + ".json"), outcome)
    return outcome


def build_mcp():
    from mcp.server.fastmcp import FastMCP
    from mcp.types import ToolAnnotations
    server = FastMCP("webcodex-subagent-pilot", instructions=
        "Experimental local pilot only. Spawn the one operator-prepared request, then read its state. "
        "No production Runner catalog activation or general autonomous delegation is implied.")
    server.tool(annotations=ToolAnnotations(readOnlyHint=False, destructiveHint=False,
        idempotentHint=True, openWorldHint=True))(spawn_chat_task)
    server.tool(annotations=ToolAnnotations(readOnlyHint=True, destructiveHint=False,
        idempotentHint=True, openWorldHint=False))(read_chat_task)
    return server


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker")
    args = parser.parse_args()
    if args.worker:
        worker(args.worker)
    else:
        build_mcp().run(transport="stdio")
