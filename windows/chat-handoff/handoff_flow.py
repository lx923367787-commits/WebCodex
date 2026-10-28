"""Request validation, cancellation and durable receipts for the local Edge runner."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import uuid
from host_enrollment import ensure_no_launch_grant


_ACTIVE_REQUEST = None
_SHOULD_STOP = lambda: False
_ID = re.compile(r"^[A-Za-z0-9_-]{1,128}$")
_CLAIM = re.compile(r"^wc_hc_[0-9a-f]{64}$")


def ensure_no_launch_claim(value):
    """Keep the handoff guard entry point for both one-time grant formats."""
    ensure_no_launch_grant(value)


def valid_launch_claim(value):
    return isinstance(value, str) and _CLAIM.fullmatch(value) is not None


def runtime_dir():
    return Path(os.environ.get("WEBCODEX_HANDOFF_RUNTIME", str(Path(__file__).parent / "runtime")))


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc)


def timestamp():
    return utc_now().isoformat().replace("+00:00", "Z")


def parse_time(value):
    parsed = datetime.datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("expires_at must include a timezone")
    return parsed


def validate_request(request, check_expiry=True):
    if not isinstance(request, dict):
        raise ValueError("request must be a JSON object")
    ensure_no_launch_claim(request)
    if any(key in request for key in ("launch_claim", "claim_grant")):
        raise ValueError("launch_claim_must_not_be_in_request")
    for key in ("launch_id", "project", "goal_id", "source_context_epoch", "recovery_prompt", "expires_at"):
        if not isinstance(request.get(key), str) or not request[key].strip():
            raise ValueError("missing request field: " + key)
    if not _ID.fullmatch(request["launch_id"]):
        raise ValueError("invalid launch_id")
    for key in ("checkpoint_sequence", "checkpoint_revision"):
        if not isinstance(request.get(key), int) or isinstance(request[key], bool) or request[key] < 0:
            raise ValueError("invalid " + key)
    if any(key in request for key in ("handoff_id", "attempt_id", "attempt_number")):
        if (not isinstance(request.get("handoff_id"), str) or not _ID.fullmatch(request["handoff_id"])
                or request.get("attempt_id") != request["launch_id"]
                or str(uuid.UUID(request["attempt_id"])) != request["attempt_id"]
                or not isinstance(request.get("attempt_number"), int)
                or isinstance(request["attempt_number"], bool)
                or not 1 <= request["attempt_number"] <= 4):
            raise ValueError("invalid handoff attempt binding")
    if "agent_id" in request and (not isinstance(request["agent_id"], str)
            or not _ID.fullmatch(request["agent_id"]) or "handoff_id" not in request):
        raise ValueError("invalid native handoff identity")
    expires_at = parse_time(request["expires_at"])
    if check_expiry and expires_at <= utc_now():
        raise ValueError("request expired")
    if "thinking_policy" not in request and any(key in request for key in
            ("thinking_profile", "chat_task_request_id", "chat_task_id")):
        raise ValueError("explicit thinking policy required")
    if "thinking_policy" in request:
        policy = request["thinking_policy"]
        if policy not in ("main_pro_highest", "inherit_child"):
            raise ValueError("invalid thinking policy")
        if policy == "inherit_child":
            profile = request.get("thinking_profile")
            if "thinking_profile" not in request or (profile is not None and
                    (not isinstance(profile, dict) or not isinstance(profile.get("profile_id"), str))):
                raise ValueError("child thinking profile required")
            child_request = request.get("chat_task_request_id")
            if not isinstance(child_request, str) or str(uuid.UUID(child_request)) != child_request:
                raise ValueError("invalid child request id")
            if request.get("chat_task_id") != "wc_chat_" + uuid.UUID(child_request).hex:
                raise ValueError("child task identity mismatch")
        elif request.get("thinking_profile") is not None:
            raise ValueError("main handoff must use Pro highest")
        thinking_fingerprint(request)
    return request


def thinking_fingerprint(request):
    """Bind only explicit new thinking policy fields; retain legacy receipts."""
    keys = ("thinking_policy", "thinking_profile", "chat_task_request_id", "chat_task_id")
    if not any(key in request for key in keys):
        return None
    data = json.dumps({key: request.get(key) for key in keys}, ensure_ascii=False,
        sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
    if len(data) > 10000:
        raise ValueError("thinking policy too large")
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    serialized = json.dumps(value, ensure_ascii=False)
    ensure_no_launch_claim(serialized)
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    try:
        with temp.open("x", encoding="utf-8", newline="\n") as output:
            output.write(serialized)
            output.flush()
            os.fsync(output.fileno())
        os.replace(str(temp), str(path))
    finally:
        if temp.exists():
            temp.unlink()


def cancelled(request):
    return (_SHOULD_STOP() or (runtime_dir() / "cancellations" / (request["launch_id"] + ".json")).exists()
            or parse_time(request["expires_at"]) <= utc_now())


def result_path(request):
    return runtime_dir() / "results" / (request["launch_id"] + ".json")


def save_result(request, result):
    result = dict(result)
    if result.get("status") not in ("sent", "unknown", "blocked"):
        result = {"status": "unknown", "reason": "invalid_approval_guard_result"}
    result.update({"launch_id": request["launch_id"], "project": request["project"], "goal_id": request["goal_id"],
                   "source_context_epoch": request["source_context_epoch"],
                   "checkpoint_sequence": request["checkpoint_sequence"],
                   "checkpoint_revision": request["checkpoint_revision"], "recorded_at": timestamp()})
    for key in ("handoff_id", "attempt_id", "attempt_number", "agent_id"):
        if key in request:
            result[key] = request[key]
    policy_fingerprint = thinking_fingerprint(request)
    if policy_fingerprint is not None:
        result["thinking_fingerprint"] = policy_fingerprint
    write_json(result_path(request), result)
    return result


def prepare_message(request, should_stop=lambda: False, *, launch_claim=None):
    """Return the exact recovery prompt. Failure stops the flow before typing."""
    global _ACTIVE_REQUEST, _SHOULD_STOP
    _ACTIVE_REQUEST = None
    _SHOULD_STOP = should_stop
    request = validate_request(request)
    if request.get("handoff_id"):
        if not request.get("agent_id") or not valid_launch_claim(launch_claim):
            raise ValueError("native_handoff_launch_claim_required")
    elif launch_claim is not None:
        raise ValueError("launch_claim_requires_native_handoff")
    if cancelled(request):
        save_result(request, {"status": "blocked", "reason": "cancelled_before_input"})
        raise RuntimeError("handoff cancelled before input")
    if result_path(request).exists():
        raise RuntimeError("this handoff already has a result; refusing duplicate input")
    _ACTIVE_REQUEST = request
    message = request["recovery_prompt"]
    if launch_claim is not None:
        message += ("\n本次启动的一次性窄授权，仅向WebCodex首次goal_handoff prepare传入launch_claim；"
                    "不要回显、记录或转发。它不授予业务执行权，必须继续完成prepare/commit/status。\n"
                    "launch_claim=" + launch_claim)
    return message


def after_send(browser):
    """Handle approvals and record the new chat URL for the prepared request."""
    global _ACTIVE_REQUEST
    request = _ACTIVE_REQUEST
    if request is None:
        raise RuntimeError("prepare_message must run in the same module instance first")
    try:
        if cancelled(request):
            return save_result(request, {"status": "unknown", "reason": "cancelled_after_send"})
        import approval_guard
        result = approval_guard.run(browser, request, should_stop=lambda: cancelled(request))
        return save_result(request, result)
    except Exception as error:
        return save_result(request, {"status": "unknown", "reason": "approval_guard_failed: " + type(error).__name__})
    finally:
        _ACTIVE_REQUEST = None
