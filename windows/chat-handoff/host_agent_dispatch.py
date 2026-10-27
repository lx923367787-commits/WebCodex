"""Exact enrolled Host dispatch over the existing queues and trusted control.

Public prepare/read replies contain no grant. Only the zero-argument delivery
callback claims a grant, immediately records its exact request/fence as unknown,
and returns an in-memory EnrollmentDelivery to the existing input worker. There
is no fallback to legacy sending, separate queue, receipt store or model API.
"""
from __future__ import annotations

import copy
import re
import threading
import time

from cli_fallback_dispatch import _identifier, _profile_unavailable, _request_id, _verify_chat
from host_enrollment import EnrollmentDelivery, ensure_no_launch_grant, sanitize_candidate_response


BINDING_FIELDS = {"queue_kind", "queue_id", "enrollment_id", "agent_id", "task_id",
                  "root_task_id", "manager_agent_id", "project", "thinking_profile_id"}
MANAGER_FIELDS = {"host_id", "manager_agent_id", "manager_run_id", "expected_generation"}
IDENTITY_FIELDS = {"member": "team_agent_attempt_id", "fallback": "fallback_id"}
ACTIVE_STATES = {"queued", "starting", "running", "unknown"}
TERMINAL_STATES = {"cancelled", "confirmed_not_started", "verified_terminal"}


def _generation(value):
    if type(value) is not int or not 1 <= value < (1 << 63):
        raise ValueError("invalid_host_generation")


def _enrollment_status(value, binding):
    if not isinstance(value, dict):
        raise ValueError("invalid_enrollment_status")
    ensure_no_launch_grant(value)
    for key in ("enrollment_id", "agent_id", "task_id"):
        if value.get(key) != binding[key]:
            raise ValueError("enrollment_status_binding_mismatch")
    _identifier(value.get("run_id"))
    _generation(value.get("generation"))
    if any(type(value.get(key)) is not bool for key in
           ("window_enabled", "principal_revoked", "claim_expired")):
        raise ValueError("invalid_enrollment_status")
    return value


def _attached(status):
    value = status["enrollment_status"]
    # Claim expiry does not undo a completed attach. Initial run expiry,
    # revocation and the exact enabled window still determine usable authority.
    return (value.get("state") == "claimed" and value.get("run_state") == "active"
            and value["window_enabled"] and not value["principal_revoked"]
            and isinstance(value.get("window_binding_ref"), str)
            and re.fullmatch(r"wc_host_binding_[0-9a-f]{64}", value["window_binding_ref"]) is not None
            and type(value.get("claimed_at_unix_ms")) is int
            and type(value.get("run_expires_at_unix_ms")) is int
            and value["run_expires_at_unix_ms"] > int(time.time() * 1000))


class HostQueueControlApi:
    """Wrap one legacy queue with an explicit trusted root→manager resolver."""
    enrollment_required = True

    def __init__(self, legacy_api, queue_kind, manager_bindings):
        if queue_kind not in IDENTITY_FIELDS:
            raise ValueError("invalid_host_queue_kind")
        self.legacy_api, self.queue_kind, self.manager_bindings = legacy_api, queue_kind, manager_bindings

    def discover(self):
        return self.legacy_api.discover()

    def finish_discovery(self):
        return self.legacy_api.finish_discovery()

    def _payload(self, operation, action):
        return {"action": "agent_host_dispatch_" + action,
                "owner_user": self.legacy_api.owner_user, "project": self.legacy_api.project,
                **operation["manager"], "queue_kind": self.queue_kind, "queue_id": operation["queue_id"]}

    def _validate_status(self, status, operation, expected_binding=None):
        if not isinstance(status, dict):
            raise ValueError("invalid_host_dispatch_status")
        ensure_no_launch_grant(status)
        binding = status.get("binding")
        if not isinstance(binding, dict) or set(binding) != BINDING_FIELDS:
            raise ValueError("invalid_host_dispatch_binding")
        for key in BINDING_FIELDS - {"queue_kind"}:
            _identifier(binding[key])
        record = operation["record"]
        if (binding["queue_kind"] != self.queue_kind or binding["queue_id"] != operation["queue_id"]
                or binding["project"] != self.legacy_api.project
                or binding["root_task_id"] != record["root_task_id"]
                or binding["manager_agent_id"] != operation["manager"]["manager_agent_id"]
                or binding["thinking_profile_id"] != record["thinking_profile_id"]
                or status.get("source_task_id") != record["source_task_id"]
                or re.fullmatch(r"wc_enrollment_[0-9a-f]{32}", binding["enrollment_id"]) is None):
            raise ValueError("host_dispatch_binding_mismatch")
        if expected_binding is not None and binding != expected_binding:
            raise ValueError("host_dispatch_binding_changed")
        if self.queue_kind == "member" and binding["task_id"] != record["source_task_id"]:
            raise ValueError("host_member_task_mismatch")
        if self.queue_kind == "fallback" and binding["task_id"] in (record["root_task_id"], record["source_task_id"]):
            raise ValueError("host_recovery_task_required")
        if (not isinstance(status.get("queue_state"), str)
                or status["queue_state"] not in ACTIVE_STATES | TERMINAL_STATES
                or type(status.get("cancel_requested")) is not bool
                or type(status.get("response_observed")) is not bool
                or type(status.get("dispatch_count")) is not int or not 0 <= status["dispatch_count"] <= 4
                or not isinstance(status.get("instruction"), str) or not status["instruction"].strip()
                or not isinstance(status.get("delivery_state"), str)):
            raise ValueError("invalid_host_dispatch_status")
        if status.get("dispatch_attempt_id") is not None:
            _request_id(status["dispatch_attempt_id"])
        if status.get("request_id") is not None:
            _request_id(status["request_id"])
        _enrollment_status(status.get("enrollment_status"), binding)
        if "status" in operation:
            previous = operation["status"]
            for key in ("dispatch_attempt_id", "request_id"):
                if previous.get(key) is not None and status.get(key) != previous[key]:
                    raise ValueError("host_dispatch_attempt_changed")
            old = previous["enrollment_status"]
            if any(status["enrollment_status"][key] != old[key] for key in ("run_id", "generation")):
                raise ValueError("enrollment_run_changed")
        return copy.deepcopy(status)

    def prepare(self, queue_id):
        _identifier(queue_id)
        record = self.legacy_api.read(queue_id)
        ensure_no_launch_grant(record)
        identity = IDENTITY_FIELDS[self.queue_kind]
        if (not isinstance(record, dict) or record.get(identity) != queue_id
                or record.get("project") != self.legacy_api.project):
            raise ValueError("invalid_host_queue_record")
        for key in ("root_task_id", "source_task_id", "thinking_profile_id"):
            _identifier(record.get(key))
        manager = self.manager_bindings.for_root(record["root_task_id"])
        if not isinstance(manager, dict) or set(manager) != MANAGER_FIELDS:
            raise ValueError("host_manager_binding_required")
        ensure_no_launch_grant(manager)
        for key in MANAGER_FIELDS - {"expected_generation"}:
            _identifier(manager[key])
        _generation(manager["expected_generation"])
        operation = {"queue_id": queue_id, "record": copy.deepcopy(record), "manager": copy.deepcopy(manager)}
        response = self.legacy_api.post(self._payload(operation, "prepare"))
        operation["status"] = self._validate_status(response, operation)
        return operation

    def read_status(self, operation):
        response = self.legacy_api.post(self._payload(operation, "read"))
        return self._validate_status(response, operation, operation["status"]["binding"])

    def record(self, operation, fence, request_id, state, reason, response_observed=False):
        _request_id(fence)
        if request_id is not None:
            _request_id(request_id)
        receipt = self.legacy_api.record(**{IDENTITY_FIELDS[self.queue_kind]: operation["queue_id"]},
            dispatch_attempt_id=fence, request_id=request_id, state=state, reason=reason,
            response_observed=response_observed)
        ensure_no_launch_grant(receipt)
        if not isinstance(receipt, dict) or receipt.get("recorded") is not True:
            raise ValueError("host_dispatch_record_failed")

    def delivery_provider(self, operation, request_id):
        # Freeze this operation now. A later sweep/map reload cannot retarget an
        # already scheduled worker to a newer manager run or different Agent.
        operation = copy.deepcopy(operation)
        _request_id(request_id)
        used = False
        lock = threading.Lock()

        def provide():
            nonlocal used
            with lock:
                if used:
                    raise ValueError("host_delivery_already_requested")
                used = True
                provide.claim_requested = True
            try:
                # Only a concrete resolver type may opt into the fresh guard;
                # dynamic Mock attributes and manual E2 readers are not APIs.
                validate = getattr(type(self.manager_bindings), "validate_for_delivery", None)
                if validate is not None:
                    validate(self.manager_bindings, operation["record"]["root_task_id"],
                             copy.deepcopy(operation["manager"]))
                reply = self.legacy_api.post(self._payload(operation, "claim"))
                if (not isinstance(reply, dict)
                        or set(reply) != {"status", "dispatch_permitted", "claim_grant", "expires_at_unix_ms"}
                        or reply["dispatch_permitted"] is not True):
                    raise ValueError("host_delivery_not_permitted")
                status = self._validate_status(reply["status"], operation, operation["status"]["binding"])
                if (status["queue_state"] != "starting" or status["request_id"] is not None
                        or status["cancel_requested"] or status["delivery_state"] != "delivery_unknown"
                        or status["dispatch_count"] != operation["status"]["dispatch_count"] + 1):
                    raise ValueError("host_delivery_not_permitted")
                delivery = EnrollmentDelivery(status["binding"]["enrollment_id"], reply["claim_grant"],
                                              reply["expires_at_unix_ms"])
                if delivery.expires_at_unix_ms <= int(time.time() * 1000):
                    raise ValueError("host_delivery_expired")
                self.record(operation, status["dispatch_attempt_id"], request_id,
                            "unknown", "enrollment_delivery_before_input")
                return delivery
            except Exception:
                # Grant/HTTP/transport details must never enter a thread error,
                # parent result, ordinary config, or a retried callback.
                raise ValueError("host_enrollment_delivery_unavailable") from None

        provide.claim_requested = False
        return provide


def dispatch(control_api, chat_service, queue_id, *, queue_kind, launch_arguments):
    """Dispatch an enrolled queue only; missing authority never uses legacy."""
    identity = IDENTITY_FIELDS[queue_kind]
    base = {identity: queue_id, "business_verified": False, "enrollment_verified": False}
    try:
        if control_api.queue_kind != queue_kind:
            raise ValueError("host_queue_kind_mismatch")
        operation = control_api.prepare(queue_id)
        status = operation["status"]
        binding = status["binding"]
        if status["queue_state"] in TERMINAL_STATES:
            return dict(base, state=status["queue_state"], reason="control_not_dispatchable")
        envelope = {**operation["record"], "task": status["instruction"],
                    "request_id": status["request_id"], "cancel_requested": status["cancel_requested"],
                    "dispatch_permitted": status["queue_state"] == "queued" and status["delivery_state"] == "reserved"}
        arguments, request_id = launch_arguments(envelope, require_fence=False)
        if status["request_id"] is not None and status["request_id"] != request_id:
            raise ValueError("host_request_binding_mismatch")
        if status["cancel_requested"]:
            if status["request_id"] is None:
                return dict(base, state="cancel_requested", reason="local_cancellation_only",
                            cancellation_scope="local browser actions only; an already sent model turn may continue")
            observed = chat_service.cancel_chat_task(request_id)
        elif envelope["dispatch_permitted"] and status["request_id"] is None:
            provider = control_api.delivery_provider(operation, request_id)
            observed = chat_service.spawn_enrolled_chat_task(**arguments,
                enrollment_binding=copy.deepcopy(binding), enrollment_delivery_provider=provider)
            if (not provider.claim_requested and isinstance(observed, dict)
                    and observed.get("state") == "busy" and observed.get("launched") is False):
                return dict(base, state="busy", queue_state="queued", launched=False,
                            reason="browser_worker_busy", retry_same_arguments=True)
            if not provider.claim_requested and isinstance(observed, dict) and _profile_unavailable(observed, arguments):
                return dict(base, state="profile_unavailable", queue_state="queued", request_created=False,
                            launched=False, reason=observed["reason"], retry_after_profile_refresh=True,
                            thinking_profile_id=binding["thinking_profile_id"])
        else:
            observed = chat_service.read_chat_task(status["request_id"] or request_id)
        observed = sanitize_candidate_response(observed)
        _verify_chat(observed, arguments, request_id)
        if observed.get("enrollment_binding") != binding:
            raise ValueError("local_enrollment_binding_mismatch")
        base["request_id"] = request_id
        latest = control_api.read_status(operation)
        if latest["request_id"] is not None and latest["request_id"] != request_id:
            raise ValueError("host_request_binding_mismatch")
        if latest["queue_state"] in TERMINAL_STATES:
            return dict(base, state=latest["queue_state"], reason="control_not_dispatchable", chat_state=observed)
        if latest["cancel_requested"] and not status["cancel_requested"]:
            observed = sanitize_candidate_response(chat_service.cancel_chat_task(request_id))
            _verify_chat(observed, arguments, request_id)
            if observed.get("enrollment_binding") != binding:
                raise ValueError("local_enrollment_binding_mismatch")
        attached = _attached(latest)
        state = "cancel_requested" if latest["cancel_requested"] else ("dispatched" if attached else "unknown")
        if latest["dispatch_attempt_id"] is not None:
            control_api.record(operation, latest["dispatch_attempt_id"], request_id, state,
                               "enrollment_attached" if attached else "enrollment_not_confirmed",
                               response_observed=bool(state == "dispatched" and observed.get("response_observed") is True))
        elif latest["queue_state"] == "queued" and observed.get("worker_alive") is True:
            state = "starting"
        result = dict(base, state=state, request_id=request_id, chat_state=observed,
                      enrollment_binding=copy.deepcopy(binding), enrollment_verified=attached,
                      queue_state=latest["queue_state"])
        if state == "cancel_requested":
            result["cancellation_scope"] = "local browser actions only; an already sent model turn may continue"
        return result
    except Exception:
        return dict(base, state="unknown", reason="enrolled_host_dispatch_unavailable")
