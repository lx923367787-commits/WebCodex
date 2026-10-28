"""Synthetic control/Service checks; no HTTP, real grant, browser or model."""
import copy
import json
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import Mock
import uuid

import cli_fallback_dispatch as fallback
import host_agent_dispatch as host
from host_enrollment import EnrollmentDelivery
from manager_bindings import RootManagerBindings
import team_agent_dispatch as member


GRANT = "wc_en_" + "a" * 64
FENCE = "8cac1554-cb46-45ec-90db-e83c225949d2"
MANAGER = {"host_id": "host-1", "manager_agent_id": "manager-1", "manager_run_id": "manager-run-1",
           "expected_generation": 1}


class Legacy:
    owner_user, project = "owner-1", "project-1"

    def __init__(self, kind):
        self.kind = kind
        self.queue_id = kind + "-queue-1"
        self.record_value = {host.IDENTITY_FIELDS[kind]: self.queue_id, "root_task_id": "root-task-1",
            "source_task_id": "source-task-1", "source_attempt_id": "cli-attempt-1",
            "thinking_profile_id": "manager-selected", "project": self.project, "state": "queued",
            "cancel_requested": False, "host_request_id": None}
        binding = {"queue_kind": kind, "queue_id": self.queue_id,
            "enrollment_id": "wc_enrollment_" + "b" * 32, "agent_id": "child-agent-1",
            "task_id": "source-task-1" if kind == "member" else "recovery-task-1",
            "root_task_id": "root-task-1", "manager_agent_id": MANAGER["manager_agent_id"],
            "project": self.project, "thinking_profile_id": "manager-selected"}
        self.status = {"binding": binding, "queue_state": "queued", "dispatch_attempt_id": None,
            "dispatch_count": 0, "delivery_state": "reserved", "request_id": None,
            "response_observed": False, "target_task_state": "ready", "instruction": "Inspect existing artifacts and finish the authorized work.",
            "source_task_id": "source-task-1", "cancel_requested": False,
            "enrollment_status": {"enrollment_id": binding["enrollment_id"], "agent_id": binding["agent_id"],
                "task_id": binding["task_id"], "run_id": "child-run-1", "generation": 1,
                "state": "issued", "run_state": "pending", "window_binding_ref": None,
                "window_enabled": False, "principal_revoked": False, "claimed_at_unix_ms": None,
                "claim_expires_at_unix_ms": 1, "claim_expired": True, "delivery_state": "reserved",
                "run_expires_at_unix_ms": int(time.time() * 1000) + 3600000}}
        self.calls, self.records, self.events = [], [], []
        self.claim_change = None
        self.claim_error = False
        self.record_error = False
        self.public_change = None
        self.acquire = Mock(side_effect=AssertionError("legacy acquire forbidden"))
        self.discover = Mock(return_value=[{"state": "queued", host.IDENTITY_FIELDS[kind]: self.queue_id}])
        self.finish_discovery = Mock()

    def read(self, queue_id):
        if queue_id != self.queue_id:
            raise ValueError("wrong queue")
        return copy.deepcopy(self.record_value)

    def post(self, payload):
        self.calls.append(copy.deepcopy(payload))
        self.events.append(payload["action"])
        if payload["action"] == "agent_host_dispatch_claim":
            if self.status["queue_state"] != "queued":
                return {"status": copy.deepcopy(self.status), "dispatch_permitted": False,
                        "claim_grant": None, "expires_at_unix_ms": None}
            self.status.update(queue_state="starting", dispatch_attempt_id=FENCE,
                               dispatch_count=1, delivery_state="delivery_unknown")
            self.status["enrollment_status"]["delivery_state"] = "delivery_unknown"
            if self.claim_error:
                raise RuntimeError("private response " + GRANT)
            reply = {"status": copy.deepcopy(self.status), "dispatch_permitted": True,
                     "claim_grant": GRANT, "expires_at_unix_ms": int(time.time() * 1000) + 300000}
            if self.claim_change:
                self.claim_change(reply)
            return reply
        if payload["action"] not in {"agent_host_dispatch_prepare", "agent_host_dispatch_read"}:
            raise AssertionError("unexpected action")
        status = copy.deepcopy(self.status)
        if self.public_change:
            self.public_change(status)
        return status

    def record(self, **record):
        self.events.append("record:" + record["state"])
        self.records.append(copy.deepcopy(record))
        if record["dispatch_attempt_id"] != self.status["dispatch_attempt_id"]:
            raise ValueError("wrong fence")
        self.status["request_id"] = record["request_id"]
        self.status["queue_state"] = "running" if record["state"] == "dispatched" else "unknown"
        self.record_value["host_request_id"] = record["request_id"]
        if self.record_error:
            raise RuntimeError("private record " + GRANT)
        return {"recorded": True}

    def attach(self, **updates):
        self.status["delivery_state"] = "claimed"
        self.status["enrollment_status"].update(state="claimed", run_state="active",
            window_binding_ref="wc_host_binding_" + "c" * 64, window_enabled=True,
            claimed_at_unix_ms=int(time.time() * 1000), claim_expired=True, **updates)


class Service:
    def __init__(self, directory, legacy):
        self.directory, self.legacy = directory, legacy
        self.requests = {}
        self.provider = None
        self.preflight = None
        self.spawn_calls, self.read_calls, self.cancel_calls, self.sends = 0, 0, 0, 0
        self.spawn_chat_task = Mock(side_effect=AssertionError("legacy spawn forbidden"))

    def spawn_enrolled_chat_task(self, *, task, parent_task_id, idempotency_key, thinking_profile_id,
                                 enrollment_binding, enrollment_delivery_provider):
        self.spawn_calls += 1
        if self.preflight is not None:
            return copy.deepcopy(self.preflight)
        request_id = str(uuid.uuid5(fallback.CHAT_NAMESPACE, parent_task_id + "\0" + idempotency_key))
        if request_id not in self.requests:
            self.provider = enrollment_delivery_provider
            self.requests[request_id] = {"request_id": request_id, "parent_task_id": parent_task_id,
                "requested_thinking_profile": {"profile_id": thinking_profile_id}, "state": "running",
                "worker_alive": True, "enrollment_binding": copy.deepcopy(enrollment_binding),
                "business_verified": False}
            # Only public ordinary data is retained by this fake Service.
            (self.directory / (request_id + ".json")).write_text(json.dumps({"task": task,
                "idempotency_key": idempotency_key, **self.requests[request_id]}), encoding="utf-8")
        return copy.deepcopy(self.requests[request_id])

    def input(self, attach=True):
        delivery = self.provider()
        if not isinstance(delivery, EnrollmentDelivery):
            raise AssertionError("invalid delivery")
        self.legacy.events.append("private_input")
        self.sends += 1
        for request in self.requests.values():
            request.update(state="finished", status="sent", worker_alive=False, message_observed=True)
        if attach:
            self.legacy.attach()
        return delivery

    def read_chat_task(self, request_id):
        self.read_calls += 1
        if request_id not in self.requests:
            raise FileNotFoundError("no local request")
        return copy.deepcopy(self.requests[request_id])

    def cancel_chat_task(self, request_id):
        self.cancel_calls += 1
        self.requests[request_id]["cancel_requested"] = True
        return copy.deepcopy(self.requests[request_id])


class FreshBindings:
    def __init__(self):
        self.current = copy.deepcopy(MANAGER)
        self.validations = []

    def for_root(self, root):
        if self.current is None:
            raise ValueError("host context unavailable")
        return copy.deepcopy(self.current)

    def validate_for_delivery(self, root, expected):
        self.validations.append((root, copy.deepcopy(expected)))
        if self.for_root(root) != expected:
            raise ValueError("manager binding changed before delivery")


class HostDispatchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.configure("fallback")

    def configure(self, kind):
        self.legacy = Legacy(kind)
        self.managers = Mock()
        self.managers.for_root.return_value = copy.deepcopy(MANAGER)
        self.control = host.HostQueueControlApi(self.legacy, kind, self.managers)
        self.service = Service(self.root, self.legacy)
        self.route = fallback if kind == "fallback" else member

    def dispatch(self):
        return self.route.dispatch(self.control, self.service, self.legacy.queue_id)

    def test_member_without_profile_keeps_real_enrollment_and_one_send(self):
        self.configure("member")
        self.legacy.record_value["thinking_profile_id"] = ""
        self.legacy.status["binding"]["thinking_profile_id"] = ""
        first = self.dispatch()
        self.assertEqual(first["state"], "starting")
        self.service.input()
        for request in self.service.requests.values():
            request.pop("requested_thinking_profile")
            request["thinking_adjustment"] = {"status": "not_requested"}
        repeated = self.dispatch()
        self.assertEqual(repeated["state"], "dispatched")
        self.assertEqual(self.service.sends, 1)
        self.assertEqual(self.service.spawn_calls, 1)

    def test_both_routes_claim_only_at_input_record_before_send_and_confirm_real_attach(self):
        for kind in ("member", "fallback"):
            with self.subTest(kind=kind):
                self.configure(kind)
                first = self.dispatch()
                self.assertEqual(first["state"], "starting")
                self.assertEqual(self.legacy.status["queue_state"], "queued")
                self.assertEqual(self.legacy.records, [])
                self.assertNotIn("agent_host_dispatch_claim", self.legacy.events)
                delivery = self.service.input()
                self.assertNotIn(GRANT, repr(delivery))
                self.assertLess(self.legacy.events.index("agent_host_dispatch_claim"), self.legacy.events.index("record:unknown"))
                self.assertLess(self.legacy.events.index("record:unknown"), self.legacy.events.index("private_input"))
                second = self.dispatch()
                self.assertEqual(second["state"], "dispatched")
                self.assertTrue(second["enrollment_verified"])
                self.assertFalse(second["business_verified"])
                self.assertEqual(self.service.spawn_calls, 1)
                self.assertEqual(self.service.sends, 1)
                self.legacy.acquire.assert_not_called()
                self.service.spawn_chat_task.assert_not_called()
                self.assertNotIn(GRANT, json.dumps(second))
                self.assertNotIn(GRANT, json.dumps(self.legacy.records))
        for path in self.root.glob("*.json"):
            self.assertNotIn(GRANT, path.read_text(encoding="utf-8"))

    def test_preflight_busy_and_profile_refusal_leave_original_queue_unclaimed(self):
        for preflight in ({"state": "busy", "launched": False},
                          {"state": "profile_unavailable", "reason": "thinking_profiles_expired_refresh_required",
                           "request_created": False, "launched": False, "thinking_profile_id": "manager-selected"}):
            with self.subTest(state=preflight["state"]):
                self.service.preflight = preflight
                result = self.dispatch()
                self.assertEqual(result["state"], preflight["state"])
                self.assertEqual(result["queue_state"], "queued")
                self.assertNotIn("agent_host_dispatch_claim", self.legacy.events)
                self.assertEqual(self.legacy.records, [])
                self.assertIsNone(self.service.provider)

    def test_callback_freezes_exact_manager_authority_and_is_single_use(self):
        self.dispatch()
        self.managers.for_root.return_value.update(manager_run_id="manager-run-new", expected_generation=2)
        self.service.input()
        claim = next(call for call in self.legacy.calls if call["action"] == "agent_host_dispatch_claim")
        self.assertEqual({key: claim[key] for key in host.MANAGER_FIELDS}, MANAGER)
        self.assertEqual(claim["owner_user"], self.legacy.owner_user)
        self.assertEqual(claim["project"], self.legacy.project)
        self.managers.validate_for_delivery.assert_not_called()
        with self.assertRaisesRegex(ValueError, "^host_delivery_already_requested$"):
            self.service.provider()
        self.assertEqual(self.legacy.events.count("agent_host_dispatch_claim"), 1)

    def test_cleared_host_blocks_prepared_provider_before_claim(self):
        managers = FreshBindings()
        self.control.manager_bindings = managers
        self.dispatch()
        managers.current = None
        with self.assertRaisesRegex(ValueError, "^host_enrollment_delivery_unavailable$"):
            self.service.input()
        self.assertEqual(managers.validations, [("root-task-1", MANAGER)])
        self.assertNotIn("agent_host_dispatch_claim", self.legacy.events)
        self.assertEqual(self.legacy.records, [])
        self.assertEqual(self.service.sends, 0)

    def test_manager_handoff_blocks_old_provider_without_retargeting(self):
        managers = FreshBindings()
        self.control.manager_bindings = managers
        self.dispatch()
        managers.current.update(manager_run_id="manager-run-successor", expected_generation=2)
        with self.assertRaisesRegex(ValueError, "^host_enrollment_delivery_unavailable$"):
            self.service.input()
        self.assertEqual(managers.validations, [("root-task-1", MANAGER)])
        self.assertNotIn("agent_host_dispatch_claim", self.legacy.events)
        self.assertEqual(self.legacy.records, [])
        self.assertEqual(self.service.sends, 0)

    def test_fresh_guard_allows_only_the_prepared_exact_manager(self):
        managers = FreshBindings()
        self.control.manager_bindings = managers
        self.dispatch()
        self.service.input()
        self.assertEqual(managers.validations, [("root-task-1", MANAGER)])
        claim = next(call for call in self.legacy.calls if call["action"] == "agent_host_dispatch_claim")
        self.assertEqual({key: claim[key] for key in host.MANAGER_FIELDS}, MANAGER)
        self.assertEqual(self.service.sends, 1)

    def test_explicit_manual_e2_reader_retains_existing_delivery_contract(self):
        self.control.manager_bindings = RootManagerBindings({"owner_user": self.legacy.owner_user,
            "project": self.legacy.project, "roots": [{"root_task_id": "root-task-1", **MANAGER}]},
            owner_user=self.legacy.owner_user, project=self.legacy.project)
        self.dispatch()
        self.service.input()
        self.assertEqual(self.service.sends, 1)

    def test_lost_claim_or_record_response_never_reaches_private_input_or_reclaims(self):
        for flag in ("claim_error", "record_error"):
            with self.subTest(flag=flag):
                self.configure("fallback")
                self.dispatch()
                setattr(self.legacy, flag, True)
                with self.assertRaisesRegex(ValueError, "^host_enrollment_delivery_unavailable$") as raised:
                    self.service.input()
                self.assertNotIn(GRANT, str(raised.exception))
                self.assertEqual(self.service.sends, 0)
                with self.assertRaises(ValueError):
                    self.service.provider()
                self.assertEqual(self.legacy.events.count("agent_host_dispatch_claim"), 1)
                result = self.dispatch()
                self.assertEqual(result["state"], "unknown")
                self.assertEqual(self.service.spawn_calls, 1)

    def test_missing_manager_mapping_stops_before_prepare_and_never_uses_legacy(self):
        self.managers.for_root.side_effect = ValueError("no root mapping")
        self.assertEqual(self.dispatch()["state"], "unknown")
        self.assertEqual(self.legacy.calls, [])
        self.assertEqual(self.service.spawn_calls, 0)
        self.legacy.acquire.assert_not_called()
        self.service.spawn_chat_task.assert_not_called()

    def test_missing_local_request_after_unknown_claim_never_spawns(self):
        self.legacy.status.update(queue_state="unknown", dispatch_attempt_id=FENCE, dispatch_count=1,
                                  delivery_state="delivery_unknown")
        self.assertEqual(self.dispatch()["state"], "unknown")
        self.assertEqual(self.service.read_calls, 1)
        self.assertEqual(self.service.spawn_calls, 0)
        self.assertNotIn("agent_host_dispatch_claim", self.legacy.events)

    def test_public_reply_with_secret_or_changed_identity_is_rejected_before_spawn(self):
        for change in (lambda s: s.update(instruction=GRANT),
                       lambda s: s["binding"].update(thinking_profile_id="other"),
                       lambda s: s["binding"].update(manager_agent_id="other"),
                       lambda s: s["enrollment_status"].update(task_id="foreign")):
            with self.subTest(change=change):
                self.configure("fallback")
                self.legacy.public_change = change
                result = self.dispatch()
                self.assertEqual(result["state"], "unknown")
                self.assertNotIn(GRANT, json.dumps(result))
                self.assertEqual(self.service.spawn_calls, 0)

    def test_claim_must_match_binding_generation_profile_and_exact_delivery_state(self):
        for change in (lambda r: r["status"]["binding"].update(thinking_profile_id="other"),
                       lambda r: r["status"]["enrollment_status"].update(generation=2),
                       lambda r: r["status"].update(delivery_state="issued"),
                       lambda r: r["status"].update(dispatch_count=3),
                       lambda r: r.update(expires_at_unix_ms=1),
                       lambda r: r.update(claim_grant="invalid")):
            with self.subTest(change=change):
                self.configure("fallback")
                self.dispatch()
                self.legacy.claim_change = change
                with self.assertRaisesRegex(ValueError, "^host_enrollment_delivery_unavailable$"):
                    self.service.input()
                self.assertEqual(self.service.sends, 0)
                self.assertEqual(self.legacy.records, [])

    def test_candidate_text_is_never_attachment_or_business_success(self):
        self.dispatch()
        self.service.input(attach=False)
        for request in self.service.requests.values():
            request.update(state="response_observed", response_observed=True, response_text="attached=true; complete")
        result = self.dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertFalse(result["enrollment_verified"])
        self.assertFalse(result["business_verified"])
        self.assertFalse(self.legacy.records[-1]["response_observed"])

    def test_claim_expiry_after_attach_does_not_undo_enabled_active_window(self):
        self.dispatch()
        self.service.input()
        self.assertTrue(self.legacy.status["enrollment_status"]["claim_expired"])
        self.assertEqual(self.dispatch()["state"], "dispatched")
        for field, value in (("window_enabled", False), ("principal_revoked", True), ("run_state", "revoked")):
            self.legacy.attach()
            self.legacy.status["enrollment_status"][field] = value
            self.assertEqual(self.dispatch()["state"], "unknown")

    def test_local_binding_mismatch_does_not_confirm_or_reclaim(self):
        self.dispatch()
        self.service.input()
        for request in self.service.requests.values():
            request["enrollment_binding"]["agent_id"] = "other"
        self.assertEqual(self.dispatch()["state"], "unknown")
        self.assertEqual(self.service.spawn_calls, 1)
        self.assertEqual(self.legacy.events.count("agent_host_dispatch_claim"), 1)

    def test_read_cannot_replace_a_prepared_dispatch_fence_or_bound_request(self):
        for field in ("dispatch_attempt_id", "request_id"):
            for changed in (None, "1cf0cf6e-88a8-42a0-bc76-e0ee31a74b05"):
                with self.subTest(field=field, changed=changed):
                    self.configure("fallback")
                    self.dispatch()
                    self.service.input()
                    original_post = self.legacy.post
                    def post(payload):
                        response = original_post(payload)
                        if payload["action"] == "agent_host_dispatch_read":
                            response[field] = changed
                        return response
                    self.legacy.post = post
                    before = len(self.legacy.records)
                    result = self.dispatch()
                    self.assertEqual(result["state"], "unknown")
                    self.assertEqual(len(self.legacy.records), before)
                    self.assertEqual(self.service.spawn_calls, 1)
                    self.assertEqual(self.service.sends, 1)

    def test_active_cancel_is_local_only_and_keeps_original_request(self):
        first = self.dispatch()
        self.service.input()
        self.legacy.status["cancel_requested"] = True
        result = self.dispatch()
        self.assertEqual(result["state"], "cancel_requested")
        self.assertEqual(result["request_id"], first["request_id"])
        self.assertEqual(self.service.cancel_calls, 1)
        self.assertIn("may continue", result["cancellation_scope"])

    def test_existing_cursor_is_delegated_without_parallel_discovery_state(self):
        self.assertEqual(self.control.discover(), self.legacy.discover.return_value)
        self.control.finish_discovery()
        self.legacy.discover.assert_called_once_with()
        self.legacy.finish_discovery.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
