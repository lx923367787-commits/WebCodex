"""Durable, single-flight Edge execution shared by the watcher and internal tools."""
import contextlib
import json
from pathlib import Path

import handoff_controller as hc
import handoff_flow as flow

IDENTITY_KEYS = ("launch_id", "project", "goal_id", "source_context_epoch", "checkpoint_sequence", "checkpoint_revision")


def _read(path):
    try:
        with Path(path).open("rb") as source:
            data = source.read(262145)
        if len(data) > 262144:
            return None
        value = json.loads(data.decode("utf-8-sig"))
        return value if isinstance(value, dict) else None
    except (OSError, ValueError):
        return None


def _identity(request):
    value = {key: request[key] for key in IDENTITY_KEYS}
    for key in ("handoff_id", "attempt_id", "attempt_number", "agent_id"):
        if key in request:
            value[key] = request[key]
    fingerprint = flow.thinking_fingerprint(request)
    if fingerprint is not None:
        value["thinking_fingerprint"] = fingerprint
    return value


def _result(request):
    for path in (flow.result_path(request), flow.runtime_dir()/"adapter-results"/(request["launch_id"]+".json")):
        value = _read(path)
        try:
            flow.ensure_no_launch_claim(value)
        except ValueError:
            value = None
        if (value and all(value.get(key) == request[key] for key in IDENTITY_KEYS)
                and all(value.get(key) == request.get(key) for key in ("handoff_id", "attempt_id", "attempt_number", "agent_id"))
                and value.get("thinking_fingerprint") == flow.thinking_fingerprint(request)
                and value.get("status") in ("sent", "unknown", "blocked")):
            return value
    return None


def _control_api():
    from handoff_control_api import Api, load_credentials
    origin, token, _ = load_credentials()
    return Api(origin, token)


def _launch_claim(request, control_api_factory=None):
    """Keep the raw grant separate from every persisted request and receipt."""
    try:
        api = _control_api() if control_api_factory is None else control_api_factory(request)
    except Exception:
        return None, {"status": "blocked", "reason": "launch_claim_control_unavailable", "launched": False}
    try:
        result = api.issue_launch_claim(request)
        if (not isinstance(result, dict)
                or any(result.get(key) != request[key] for key in ("project", "goal_id", "agent_id", "handoff_id", "attempt_id"))
                or type(result.get("expires_at_ms")) is not int
                or result["expires_at_ms"] != round(flow.parse_time(request["expires_at"]).timestamp() * 1000)
                or result["expires_at_ms"] <= flow.utc_now().timestamp() * 1000
                or not flow.valid_launch_claim(result.get("claim_grant"))):
            raise ValueError("invalid_launch_claim_response")
        return result["claim_grant"], None
    except Exception:
        # Issuance or a previous commit may have succeeded before its response
        # was lost. Never reissue or rotate a grant for this attempt.
        try:
            observed = api.call("goal_handoff", {"action": "status", "goal_id": request["goal_id"],
                "handoff_id": request["handoff_id"], "attempt_id": request["attempt_id"]})
            h = observed.get("handoff", {})
            if (h.get("status") == "committed" and h.get("handoff_id") == request["handoff_id"]
                    and h.get("goal_id") == request["goal_id"] and h.get("project") == request["project"]):
                return None, {"status": "blocked", "reason": "handoff_already_committed", "launched": False}
        except Exception:
            pass
        return None, {"status": "unknown", "reason": "launch_claim_outcome_unknown", "launched": False}


def inspect(request):
    """Read only bounded local evidence; never open a page or attach to Edge."""
    request = flow.validate_request(request, check_expiry=False)
    import playwright_extension_browser as edge_browser
    try:
        edge_browser.discover_endpoint()
        ready = True
    except Exception:
        ready = False
    result = _result(request)
    attempt = flow.runtime_dir()/"edge-launches"/(request["launch_id"]+".json")
    return dict(_identity(request), backend="playwright_extension", endpoint_available=ready,
                state=result["status"] if result else ("unknown" if attempt.exists() else "not_started"),
                result=result)


def launch(request_file, should_stop=lambda **_: False, *, control_api_factory=None):
    """Before opening a tab persist intent. An uncertain intent is never replayed."""
    try:
        request = flow.validate_request(_read(request_file), check_expiry=False)
        if request.get("handoff_id") and not request.get("agent_id"):
            raise ValueError("native_handoff_agent_required")
    except (ValueError, TypeError):
        return {"status":"blocked", "reason":"invalid_request", "launched":False}
    directory = flow.runtime_dir()
    directory.mkdir(parents=True, exist_ok=True)
    with contextlib.ExitStack() as stack:
        try:
            stack.enter_context(hc.watcher_lock(directory/"edge-dispatch"))
        except ValueError:
            return {"status":"retryable", "reason":"edge_runner_busy", "launched":False}
        receipt = _result(request)
        if receipt:
            return receipt
        if any(path.exists() for path in (flow.result_path(request),
                directory/"adapter-results"/(request["launch_id"]+".json"))):
            return {"status":"blocked", "reason":"receipt_identity_or_format_mismatch", "launched":False}
        attempt = directory/"edge-launches"/(request["launch_id"]+".json")
        legacy = directory/"launches"/(request["launch_id"]+".json")
        # Retain old transport evidence across migrations. Unknown old outcomes
        # cannot be turned into permission for a new browser submission.
        if (attempt.exists() or legacy.exists()
                or (directory/"send-intents"/(request["launch_id"]+".json")).exists()):
            return dict(_identity(request), status="unknown", reason="previous_launch_outcome_unknown")
        if should_stop() or flow.parse_time(request["expires_at"]) <= flow.utc_now():
            return {"status":"retryable", "reason":"stopped_or_expired_before_launch", "launched":False}
        if (directory/"cancellations"/(request["launch_id"]+".json")).exists():
            return {"status":"blocked", "reason":"request_cancelled", "launched":False}
        try:
            import playwright_extension_browser as edge_browser
            edge = stack.enter_context(edge_browser.persistent_session())
        except Exception:
            # No page has been created and no input dispatched. A later poll
            # may retry when the user starts Edge or permits the connection.
            return {"status":"retryable", "reason":"edge_connection_unavailable", "launched":False}
        if should_stop() or flow.parse_time(request["expires_at"]) <= flow.utc_now():
            return {"status":"retryable", "reason":"stopped_or_expired_before_launch", "launched":False}
        launch_claim = None
        if request.get("agent_id"):
            # Before issuance, record only the public attempt identity. A crash
            # here cannot lead another worker to issue/deliver a second grant.
            flow.write_json(attempt, dict(_identity(request), status="claim_requested", backend="playwright_extension", recorded_at=flow.timestamp()))
            launch_claim, outcome = (_launch_claim(request) if control_api_factory is None else
                                     _launch_claim(request, control_api_factory))
            if launch_claim is None:
                return flow.save_result(request, outcome)
            if should_stop() or flow.parse_time(request["expires_at"]) <= flow.utc_now():
                return flow.save_result(request, {"status":"blocked", "reason":"stopped_after_claim", "launched":False})
        flow.write_json(attempt, dict(_identity(request), status="issued", backend="playwright_extension", recorded_at=flow.timestamp()))
        try:
            browser = edge.create("https://chatgpt.com/")
            import edge_handoff
            if request.get("agent_id"):
                return edge_handoff.run(browser, request, should_stop, launch_claim=launch_claim)
            return edge_handoff.run(browser, request, should_stop)
        except Exception as error:
            return flow.save_result(request, {"status":"unknown", "reason":"edge_execution_"+type(error).__name__})
