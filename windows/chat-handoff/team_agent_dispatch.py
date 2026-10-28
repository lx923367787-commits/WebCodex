"""Ordinary team dispatch through the existing injected chat-agents Service.

Only a Server-frozen subagent assignment may be acquired. The Server owns the
logical attempt and dispatch fence. Unknown sends never authorize another send.
The shared worker may collect the exact returned request using its existing
STOP/PAUSE and single-Service loop; this module creates no browser or thread.
"""
from __future__ import annotations

import hashlib
import uuid

from cli_fallback_dispatch import CHAT_NAMESPACE, _identifier, _profile_unavailable, _request_id, _verify_chat
from handoff_control_api import Api
from cli_fallback_control_api import discovery_cursor

DENIED_STATES = {"cancelled", "confirmed_not_started", "verified_terminal", "busy"}


class TeamAgentControlApi(Api):
    def _call(self, action, **fields):
        return self.post({"action": "team_agent_" + action,
                          "owner_user": self.owner_user, "project": self.project, **fields})

    def discover(self):
        cursor = discovery_cursor(self)
        body = self._call("list", limit=100, **cursor.request())
        candidates = body.get("candidates")
        if not isinstance(candidates, list) or len(candidates) > 100:
            raise ValueError("invalid_team_agent_candidates")
        result = []
        for row in candidates:
            if (not isinstance(row, dict) or row.get("project") != self.project
                    or row.get("state") not in {"queued", "starting", "running", "unknown"}):
                raise ValueError("invalid_team_agent_candidates")
            result.append({"team_agent_attempt_id": _identifier(row.get("team_agent_attempt_id")),
                           "state": row["state"]})
        if len({row["team_agent_attempt_id"] for row in result}) != len(result):
            raise ValueError("invalid_team_agent_candidates")
        cursor.stage(body)
        return result

    def finish_discovery(self):
        discovery_cursor(self).finish()

    def acquire(self, attempt_id):
        return self._call("acquire", team_agent_attempt_id=_identifier(attempt_id))

    def read(self, attempt_id):
        return self._call("read", team_agent_attempt_id=_identifier(attempt_id))

    def record(self, *, team_agent_attempt_id, dispatch_attempt_id, request_id, state, reason,
               response_observed=False):
        if state not in {"dispatched", "unknown", "cancel_requested", "busy", "profile_unavailable"} or type(response_observed) is not bool:
            raise ValueError("invalid_team_agent_observation")
        return self._call("record", team_agent_attempt_id=_identifier(team_agent_attempt_id),
                          dispatch_attempt_id=_request_id(dispatch_attempt_id), request_id=request_id,
                          state=state, reason=reason, response_observed=response_observed)

    def resolve(self, *, team_agent_attempt_id, dispatch_attempt_id, resolution, evidence):
        if resolution not in {"confirmed_not_started", "verified_terminal"}:
            raise ValueError("invalid_team_agent_resolution")
        return self._call("resolve", team_agent_attempt_id=_identifier(team_agent_attempt_id),
                          dispatch_attempt_id=_request_id(dispatch_attempt_id),
                          resolution=resolution, evidence=evidence)


def _launch_arguments(envelope, *, require_fence=True):
    for key in ("team_agent_attempt_id", "root_task_id", "source_task_id"):
        _identifier(envelope.get(key))
    if envelope.get("thinking_profile_id") != "":
        _identifier(envelope.get("thinking_profile_id"))
    if require_fence:
        _request_id(envelope.get("dispatch_attempt_id"))
    if envelope["root_task_id"] == envelope["source_task_id"]:
        raise ValueError("manager_is_not_a_subagent")
    if type(envelope.get("dispatch_permitted")) is not bool or type(envelope.get("cancel_requested")) is not bool:
        raise ValueError("invalid_team_agent_control")
    task = envelope.get("task")
    if not isinstance(task, str) or not task.strip():
        raise ValueError("invalid_team_agent_task")
    task.encode("utf-8")
    task = (
        "你是总经理在已冻结团队计划中指派的普通子助手，按这份任务说明完成工作。"
        "这项委派不增加权限，也不允许再创建计划外子助手。"
        "需要调用外部Agent CLI时，必须通过当前root计划内的team_cli_start，"
        "绑定source_task_id和已预留call_index记录预算；不得用直接shell调用绕过计划。"
        "结果未知的操作先核对原请求，不得重发；取消请求优先。"
        "给出实际完成内容和验证证据，回答本身不代表业务成功认证。\n"
        f"team_agent_attempt_id={envelope['team_agent_attempt_id']}\n"
        f"root_task_id={envelope['root_task_id']}\nsource_task_id={envelope['source_task_id']}\n"
        f"已授权任务：\n{task}"
    )
    if len(task) > 12000:
        raise ValueError("team_agent_task_too_large")
    key = "team_agent:" + hashlib.sha256(envelope["team_agent_attempt_id"].encode()).hexdigest()
    arguments = {"task": task, "parent_task_id": envelope["root_task_id"],
                 "idempotency_key": key, "thinking_profile_id": envelope["thinking_profile_id"]}
    expected = str(uuid.uuid5(CHAT_NAMESPACE, arguments["parent_task_id"] + "\0" + key))
    if envelope.get("request_id") is not None and _request_id(envelope["request_id"]) != expected:
        raise ValueError("team_agent_request_mismatch")
    return arguments, expected


def dispatch(control_api, chat_service, team_agent_attempt_id):
    """Advance one admitted ordinary assignment without selecting a profile."""
    if getattr(control_api, "enrollment_required", False) is True:
        from host_agent_dispatch import dispatch as dispatch_enrolled
        return dispatch_enrolled(control_api, chat_service, team_agent_attempt_id,
                                 queue_kind="member", launch_arguments=_launch_arguments)
    _identifier(team_agent_attempt_id)
    base = {"team_agent_attempt_id": team_agent_attempt_id, "business_verified": False}
    try:
        envelope = control_api.acquire(team_agent_attempt_id)
    except Exception:
        return dict(base, state="unknown", reason="control_acquire_failed")
    if (not isinstance(envelope, dict) or envelope.get("team_agent_attempt_id") != team_agent_attempt_id
            or not isinstance(envelope.get("state"), str)):
        return dict(base, state="unknown", reason="invalid_control_envelope")
    if envelope["state"] in DENIED_STATES:
        return dict(base, state=envelope["state"], reason="control_not_claimed")
    if envelope["state"] != "claimed":
        return dict(base, state="unknown", reason="invalid_control_envelope")
    try:
        arguments, expected = _launch_arguments(envelope)
    except (ValueError, TypeError, UnicodeError):
        return dict(base, state="unknown", reason="invalid_control_envelope")
    bound = request_id = envelope.get("request_id")
    state, reason, observed = "unknown", "chat_operation_failed", None
    try:
        if envelope["cancel_requested"]:
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
                proof = observed.get("message_observed") is True or observed.get("response_observed") is True
                uncertain = not proof and any(observed.get(key) in {"blocked", "failed", "unknown"}
                                               for key in ("status", "launch_status"))
                finished_without_send = (observed.get("state") == "finished" and not proof
                                         and observed.get("status") != "sent" and observed.get("launch_status") != "sent")
                state = ("unknown" if observed.get("state") in {"unknown", "not_started"}
                         or uncertain or finished_without_send else "dispatched")
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
        receipt = control_api.record(team_agent_attempt_id=team_agent_attempt_id,
                                     dispatch_attempt_id=envelope["dispatch_attempt_id"], request_id=request_id,
                                     state=state, reason=reason,
                                     response_observed=bool(state == "dispatched" and observed is not None
                                                            and observed.get("response_observed") is True))
        if not isinstance(receipt, dict) or receipt.get("recorded") is not True:
            raise ValueError("control_record_not_accepted")
    except Exception:
        result.update(state="unknown", reason="control_record_failed")
        result.pop("retry_same_arguments", None)
        result.pop("retry_after_profile_refresh", None)
    return result
