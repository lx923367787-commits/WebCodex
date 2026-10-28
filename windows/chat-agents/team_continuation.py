"""Original-chat event delivery inside the existing native Host lifecycle.

Server owns logical events, claims and the one-shot send fence. Local records
are transport evidence only. No model input can choose a URL or an identity.
"""
from __future__ import annotations
import json
import hashlib
import re
from pathlib import Path
import time
import uuid

from edge_tools import read_json, write_json
from handoff_controller import watcher_lock
from host_enrollment import ensure_no_launch_grant
import approval_guard

TARGET_FIELDS = {"task_id", "agent_id", "run_id", "generation", "window_key"}


class Control:
    def __init__(self, api, host_provider):
        self.api, self.host_provider = api, host_provider

    def call(self, operation, *, target=None, attempt_id=None):
        host = self.host_provider()
        if not isinstance(host, str) or not host:
            raise ValueError("continuation_host_unavailable")
        value = self.api.post({"action": "team_continuation", "operation": operation,
            "owner_user": self.api.owner_user, "project": self.api.project, "host_id": host,
            **({"target": target} if target is not None else {}),
            **({"attempt_id": attempt_id} if attempt_id is not None else {})})
        ensure_no_launch_grant(value)
        return value


def validate_target(target):
    if (not isinstance(target, dict) or set(target) != TARGET_FIELDS
            or type(target["generation"]) is not int or target["generation"] < 1
            or any(not isinstance(target[k], str) or not target[k] or len(target[k]) > 256
                   for k in TARGET_FIELDS - {"generation"})):
        raise ValueError("invalid_continuation_target")
    for key,prefix in (("task_id","wc_agent_task_"),("agent_id","wc_iagent_"),("run_id","wc_agent_run_")):
        if re.fullmatch(prefix+r"[0-9a-f]{32}",target[key]) is None:
            raise ValueError("invalid_continuation_target")
    if re.fullmatch(r"[0-9a-f]{64}",target["window_key"]) is None:
        raise ValueError("invalid_continuation_target")


def validate_route(route):
    if isinstance(route, dict) and set(route) == {"url", "window_source"}:
        if (not isinstance(route["url"], str)
                or approval_guard.chat_url(route["url"]) != route["url"]
                or route["window_source"] != "openai-session"):
            raise ValueError("continuation_route_invalid")
        return
    if (not isinstance(route, dict)
            or set(route) != {"url", "anchor_launch_id", "anchor_task_id"}
            or not isinstance(route.get("url"), str)
            or approval_guard.chat_url(route["url"]) != route["url"]
            or any(not isinstance(route.get(key), str)
                   or re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", route[key]) is None
                   for key in ("anchor_launch_id", "anchor_task_id"))):
        raise ValueError("continuation_route_invalid")
    ensure_no_launch_grant(route)


def route_matches(route, target, proof):
    """URL identity is checked against the Server's existing window binding.

    A manager selected in an ordinary chat has no launch-message anchor. Its
    authenticated OpenAI session hash is sufficient without reading chat text.
    Never infer a binding from URL proximity, a title or a displayed task ID.
    """
    if (not isinstance(proof, dict) or proof.get("url") != route["url"]
            or proof.get("visible") is not True):
        return False
    if route.get("window_source") == "openai-session":
        conversation = route["url"].rsplit("/", 1)[-1]
        key = hashlib.sha256(b"webcodex.client-window.v1\0openai-session\0" + conversation.encode()).hexdigest()
        return key == target["window_key"]
    return proof.get("anchor_count") == 1


def register_original_route(service, control, target, route, *, observe):
    """Trusted Host/operator only; never exposed in native model tools.

    Observe returns counts/URL metadata only. It must not return conversation,
    Goal, Memory or recovery text. Every later delivery verifies the anchor again.
    A same-generation route is immutable; handoff registers its new run separately.
    """
    validate_target(target)
    validate_route(route)
    def current():
        value = control.call("validate_target", target=target)
        if value.get("target") != target or value.get("current") is not True:
            raise ValueError("continuation_route_target_unverified")
    current()
    proof = observe(route, target)
    if not route_matches(route, target, proof):
        raise ValueError("continuation_route_anchor_unverified")
    current()
    saved = service.runtime / "continuation-routes" / (target["run_id"] + ".json")
    value = {"target": target, "route": route}
    with watcher_lock(service.runtime / "registry"):
        if saved.exists():
            if read_json(saved) != value:
                raise ValueError("continuation_route_immutable")
            return {"registered": True, "replayed": True}
        if target["generation"] > 1:
            # Initial routes are retained as enrollment receipts rather than
            # registration files. A successor must not reuse those either.
            for request_path in (service.runtime / "tasks").glob("*/request.json"):
                request = read_json(request_path)
                binding = request.get("enrollment_binding", {})
                if (binding.get("task_id"), binding.get("agent_id")) != (target["task_id"], target["agent_id"]):
                    continue
                receipt_path = service.runtime / "evidence" / request_path.parent.name / "result.json"
                if receipt_path.exists():
                    receipt = read_json(receipt_path)
                    if approval_guard.chat_url(receipt.get("target_url")) == route["url"]:
                        raise ValueError("continuation_route_conversation_already_bound")
        for other in saved.parent.glob("*.json"):
            previous = read_json(other)
            if previous.get("route", {}).get("url") == route["url"]:
                raise ValueError("continuation_route_conversation_already_bound")
        write_json(saved, value)
    return {"registered": True, "replayed": False}


def original_route(service, target):
    """Match an existing Host enrollment receipt, never the latest chat."""
    validate_target(target)
    routes = []
    for path in (service.runtime / "tasks").glob("*/request.json"):
        if target["generation"]!=1:
            break
        request = read_json(path)
        binding = request.get("enrollment_binding", {})
        if (binding.get("task_id"), binding.get("agent_id")) != (target["task_id"], target["agent_id"]):
            continue
        receipt_path = service.runtime / "evidence" / path.parent.name / "result.json"
        if not receipt_path.exists():
            continue
        receipt = read_json(receipt_path)
        config = request["config"]
        url = approval_guard.chat_url(receipt.get("target_url"))
        if (url and receipt.get("message_observed") is True
                and receipt.get("launch_id") == config.get("launch_id")
                and receipt.get("task_id") == config.get("task_id")):
            routes.append({"url": url, "anchor_launch_id": config["launch_id"],
                           "anchor_task_id": config["task_id"]})
    # An explicitly registered original manager/successor route is Host-only
    # metadata. Its exact run/generation fence prevents a handoff from reusing
    # a predecessor's conversation. Registration is not a model tool.
    saved = service.runtime / "continuation-routes" / (target["run_id"] + ".json")
    if saved.exists():
        route = read_json(saved)
        if route.get("target") != target:
            raise ValueError("continuation_route_target_mismatch")
        routes.append(route["route"])
    # Formal handoff receipts carry identity/URL metadata. A successor route
    # must independently match its current authenticated window; no recovery
    # prompt, private checkpoint or Goal body is read here.
    if target["generation"] > 1:
        for path in (service.transport_runtime_root / "results").glob("*.json"):
            receipt = read_json(path)
            url = approval_guard.chat_url(receipt.get("target_url"))
            if (receipt.get("agent_id") != target["agent_id"] or not url
                    or receipt.get("message_observed") is not True):
                continue
            route = {"url": url, "window_source": "openai-session"}
            if route_matches(route, target, {"url": url, "visible": True}):
                routes.append(route)
    unique = {json.dumps(route, sort_keys=True) for route in routes}
    if len(unique) != 1:
        return None
    route = json.loads(unique.pop())
    validate_route(route)
    return route


def schedule(service, api, host_provider, state_reader, *, cursor_path=None):
    """Reserve the same browser worker; all authority is checked again there."""
    if service._stopping.is_set() or state_reader().get("mode") != "running":
        return False
    control = Control(api, host_provider)
    candidates = control.call("list").get("candidates")
    if not isinstance(candidates, list) or len(candidates) > 100:
        raise ValueError("invalid_continuation_candidates")
    cursor_path=Path(cursor_path) if cursor_path is not None else service.runtime/"continuation-cursor.json"
    last=read_json(cursor_path).get("task_id","") if cursor_path.exists() else ""
    for candidate in candidates:
        validate_target(candidate.get("target"))
    candidates.sort(key=lambda c:c["target"]["task_id"])
    candidates=[c for c in candidates if c["target"]["task_id"]>last]+[c for c in candidates if c["target"]["task_id"]<=last]
    for candidate in candidates:
        target = candidate.get("target")
        validate_target(target)
        route = original_route(service, target)
        if route is None:
            control.call("route_missing",target=target)
            continue
        with watcher_lock(service.runtime / "registry"):
            if service._stopping.is_set() or service.browser_worker_busy():
                return False
            thread = service.thread_factory(target=worker,
                args=(service, control, candidate, route, state_reader),
                name="webcodex-team-continuation", daemon=True)
            service._thread, service._active_job_id = thread, str(uuid.uuid4())
            write_json(cursor_path,{"task_id":target["task_id"]})
            thread.start()
            return True
    return False


def worker(service, control, candidate, route, state_reader):
    import team_continuation_transport as transport
    target = candidate["target"]
    attempt = candidate.get("attempt")
    result = {"target": target, "state": "not_started", "business_verified": False}
    try:
        result = transport.run(control, target, route, attempt, state_reader,
            cancelled=service._stopping.is_set, runtime_root=service.transport_runtime_root)
    except Exception as error:
        # Server keeps any crossed send fence unknown. Never retry here.
        result.update(state="unknown", reason=type(error).__name__)
    ensure_no_launch_grant(result)
    folder = service.runtime / "continuation-observations" / target["task_id"]
    digest=hashlib.sha256(json.dumps(result,sort_keys=True,ensure_ascii=False).encode()).hexdigest()
    path=folder/(digest+".json")
    if not path.exists():
        write_json(path,result)
