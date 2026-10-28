"""Trusted host adapter for one Server-reserved CLI fallback chat.

This module is not a model-facing tool and never accepts caller-supplied failure
evidence. ``control_api`` must be an authenticated Server control client:

* acquire(fallback_id) atomically reserves the fallback dispatch before returning
  a flat envelope with state="claimed", fallback_id, root_task_id,
  source_attempt_id, dispatch_attempt_id (canonical UUID fence), task,
  thinking_profile_id, dispatch_permitted (boolean), and optional request_id /
  cancel_requested. The Server verifies source termination, owns the
  separate fallback slot, and freezes this intent across retries/restarts.
  Only the first acquire of a queued claim permits dispatch. An existing claim
  returns dispatch_permitted=false with its original fence and bound request.
  Other states (blocked/busy/unknown/cancelled) do not authorize chat operations.
* record(fallback_id=..., dispatch_attempt_id=..., request_id=..., state=...,
  reason=...) durably records bounded transport facts for that exact claim.
  It returns {"recorded": true} only after durable acceptance; rejection raises
  or returns another value. States are dispatched/busy/profile_unavailable/unknown/cancel_requested,
  never business success.
  Unknown and local cancellation retain the fallback slot. Busy requires the
  exact chat state busy/launched=false; Server alone may requeue it under a new
  fence (at most four not-started attempts), preserving immutable intent/key.
  Stale-fence records must be rejected. Unknown never requeues. A claim that
  cannot dispatch may only read its bound or deterministic request identity;
  a missing request remains unknown, even if its original worker died early.
  profile_unavailable is accepted only from a first permitted spawn's explicit
  no-request/no-launch proof; it waits for catalog refresh with the same manager
  selection and key, and must not consume the Server's bounded busy retries.

The injected chat_service is the existing chat-agents Service: spawn/read/cancel
retain its one-shot @webcodex browser transport, STOP/PAUSE, and identity guards.
No HTTP, credentials, browser implementation, background loop, or model call is
created here. Candidate chat results remain unverified data.
"""
from __future__ import annotations

import hashlib
import re
import uuid


CHAT_NAMESPACE = uuid.UUID("4c993b18-0bc4-4fba-8cce-4be1d9bcb3af")
ID_PATTERN = re.compile(r"[A-Za-z0-9_.:-]{1,128}")
DENIED_STATES = {"blocked", "busy", "unknown", "cancelled"}
PROFILE_UNAVAILABLE_REASONS = {"discover_thinking_profiles_first",
                               "thinking_profiles_expired_refresh_required",
                               "thinking_profile_not_discovered"}


def _identifier(value):
    if not isinstance(value, str) or not ID_PATTERN.fullmatch(value):
        raise ValueError("invalid_fallback_identifier")
    return value


def _request_id(value):
    if not isinstance(value, str) or str(uuid.UUID(value)) != value:
        raise ValueError("invalid_fallback_request_id")
    return value


def _launch_arguments(envelope, *, require_fence=True):
    for field in ("fallback_id", "root_task_id", "source_attempt_id",
                  "thinking_profile_id"):
        _identifier(envelope.get(field))
    if require_fence:
        _request_id(envelope.get("dispatch_attempt_id"))
    if type(envelope.get("dispatch_permitted")) is not bool:
        raise ValueError("invalid_fallback_dispatch_permission")
    task = envelope.get("task")
    if not isinstance(task, str) or not task.strip():
        raise ValueError("invalid_fallback_task")
    task.encode("utf-8")
    task = (
        "你是因外部CLI不可用而启动的GPT保底子助手，不占常规Agent名额。"
        "仅执行本次已授权接管任务；此描述不授予额外权限。"
        "禁止调用外部Agent CLI，禁止递归委派或再创建保底助手。"
        "先核对已有文件、成果和验证记录，从剩余工作续接；不得重放结果未知的操作，"
        "不得覆盖、回滚或清理已有成果来重新开始。遇到原执行是否停止不明时停靠核对。"
        "按原验收条件验证结果，报告已验证与未验证部分；候选回答不是业务成功认证。\n"
        f"fallback_id={envelope['fallback_id']}\n"
        f"root_task_id={envelope['root_task_id']}\n"
        f"source_attempt_id={envelope['source_attempt_id']}\n"
        f"接管任务：\n{task}"
    )
    if len(task) > 12000:
        raise ValueError("fallback_task_too_large")
    key = "cli_fallback:" + hashlib.sha256(envelope["fallback_id"].encode()).hexdigest()
    arguments = {"task": task, "parent_task_id": envelope["root_task_id"],
                 "idempotency_key": key, "thinking_profile_id": envelope["thinking_profile_id"]}
    # Match Service.spawn_chat_task's existing UUID5 identity; the integration
    # test checks this contract without starting a browser worker.
    expected = str(uuid.uuid5(CHAT_NAMESPACE, arguments["parent_task_id"] + "\0" + key))
    if envelope.get("request_id") is not None and _request_id(envelope["request_id"]) != expected:
        raise ValueError("fallback_request_binding_mismatch")
    if type(envelope.get("cancel_requested", False)) is not bool:
        raise ValueError("invalid_fallback_cancellation")
    return arguments, expected


def _verify_chat(observed, arguments, expected):
    if (not isinstance(observed, dict) or _request_id(observed.get("request_id")) != expected
            or observed.get("parent_task_id") != arguments["parent_task_id"]
            or (observed.get("requested_thinking_profile") or {}).get("profile_id", "") != arguments["thinking_profile_id"]
            or observed.get("state") not in {"running", "finished", "response_observed", "unknown", "not_started"}):
        raise ValueError("chat_identity_or_state_mismatch")


def _profile_unavailable(observed, arguments):
    """Only the trusted Service's pre-creation refusal permits profile refresh."""
    return (observed.get("state") == "profile_unavailable"
            and observed.get("reason") in PROFILE_UNAVAILABLE_REASONS
            and observed.get("request_created") is False and observed.get("launched") is False
            and observed.get("request_id") is None
            and observed.get("thinking_profile_id") == arguments["thinking_profile_id"])


def dispatch(control_api, chat_service, fallback_id):
    """Advance one trusted claim; never infer failure from model-supplied text."""
    if getattr(control_api, "enrollment_required", False) is True:
        from host_agent_dispatch import dispatch as dispatch_enrolled
        return dispatch_enrolled(control_api, chat_service, fallback_id,
                                 queue_kind="fallback", launch_arguments=_launch_arguments)
    _identifier(fallback_id)
    base = {"fallback_id": fallback_id, "business_verified": False}
    try:
        envelope = control_api.acquire(fallback_id)
    except Exception:
        return dict(base, state="unknown", reason="control_acquire_failed")
    if (not isinstance(envelope, dict) or envelope.get("fallback_id") != fallback_id
            or not isinstance(envelope.get("state"), str)):
        return dict(base, state="unknown", reason="invalid_control_envelope")
    if envelope.get("state") in DENIED_STATES:
        result = dict(base, state=envelope["state"], reason="control_not_claimed")
        if envelope["state"] == "cancelled":
            result["cancellation_scope"] = "local browser actions only; an already sent model turn may continue"
        return result
    if envelope.get("state") != "claimed":
        return dict(base, state="unknown", reason="invalid_control_envelope")
    base["slot_held"] = True
    try:
        arguments, expected = _launch_arguments(envelope)
    except (ValueError, TypeError, UnicodeError):
        return dict(base, state="unknown", reason="invalid_control_envelope")
    bound = envelope.get("request_id")
    state, reason, observed = "unknown", "chat_operation_failed", None
    request_id = bound
    try:
        if envelope.get("cancel_requested", False):
            if bound is None and not envelope["dispatch_permitted"]:
                previous = chat_service.read_chat_task(expected)
                _verify_chat(previous, arguments, expected)
                bound = request_id = expected
            if bound is not None:
                observed = chat_service.cancel_chat_task(bound)
                _verify_chat(observed, arguments, expected)
            state, reason = "cancel_requested", "local_cancellation_only"
        else:
            spawning = bound is None and envelope["dispatch_permitted"]
            observed = (chat_service.spawn_chat_task(**arguments) if spawning
                        else chat_service.read_chat_task(bound or expected))
            if not isinstance(observed, dict):
                raise ValueError("invalid_chat_state")
            if observed.get("state") == "busy" and observed.get("launched") is False and spawning:
                state, reason = "busy", "browser_worker_busy"
            elif spawning and _profile_unavailable(observed, arguments):
                state, reason = "profile_unavailable", observed["reason"]
            else:
                _verify_chat(observed, arguments, expected)
                request_id = expected
                message_observed = (observed.get("message_observed") is True
                                    or observed.get("response_observed") is True)
                launch_uncertain = (not message_observed and any(
                    observed.get(key) in ("blocked", "failed", "unknown")
                    for key in ("status", "launch_status")))
                # A finished local worker may have been cancelled before send.
                # Retain its request, but do not treat worker completion as a
                # sent message or as the no-request busy retry case.
                finished_without_send = (observed.get("state") == "finished" and not message_observed
                                         and observed.get("status") != "sent"
                                         and observed.get("launch_status") != "sent")
                state = ("unknown" if observed.get("state") in {"unknown", "not_started"}
                         or launch_uncertain or finished_without_send else "dispatched")
                reason = "chat_state_observed"
    except Exception:
        observed = None
    result = dict(base, state=state, reason=reason, request_id=request_id)
    if state == "busy":
        result.update(launched=False, retry_same_arguments=True)
    if state == "profile_unavailable":
        result.update(request_created=False, launched=False, retry_after_profile_refresh=True,
                      thinking_profile_id=arguments["thinking_profile_id"])
    if state == "cancel_requested":
        result["cancellation_scope"] = "local browser actions only; an already sent model turn may continue"
    if observed is not None:
        result["chat_state"] = dict(observed, business_verified=False)
    try:
        receipt = control_api.record(fallback_id=fallback_id, dispatch_attempt_id=envelope["dispatch_attempt_id"],
                                     request_id=request_id, state=state, reason=reason,
                                     response_observed=bool(state == "dispatched" and observed is not None
                                                            and observed.get("response_observed") is True))
        if not isinstance(receipt, dict) or receipt.get("recorded") is not True:
            raise ValueError("control_record_not_accepted")
        if state in {"busy", "profile_unavailable"}:
            result["slot_held"] = False
    except Exception:
        result.update(state="unknown", reason="control_record_failed")
        result.pop("retry_same_arguments", None)
        result.pop("retry_after_profile_refresh", None)
    return result
