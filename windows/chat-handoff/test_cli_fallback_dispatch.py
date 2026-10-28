"""Offline CLI fallback adapter tests; no browser, network, or model calls."""
import copy
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock

import cli_fallback_dispatch as adapter


ENVELOPE = {"state": "claimed", "fallback_id": "fallback-1", "root_task_id": "root-1",
            "source_attempt_id": "cli-attempt-1", "dispatch_attempt_id": "8cac1554-cb46-45ec-90db-e83c225949d2",
            "task": "核对已有 result.txt，完成剩余检查。", "thinking_profile_id": "profile-high",
            "request_id": None, "cancel_requested": False, "dispatch_permitted": True}


class Control:
    def __init__(self, envelope=None):
        self.envelope = copy.deepcopy(ENVELOPE if envelope is None else envelope)
        self.records = []
        self.acquire = Mock(side_effect=lambda fallback_id: copy.deepcopy(self.envelope))

    def record(self, **value):
        self.records.append(value)
        self.envelope["request_id"] = value["request_id"]
        return {"recorded": True}


def chat_state(envelope=None, **updates):
    envelope = ENVELOPE if envelope is None else envelope
    arguments, request_id = adapter._launch_arguments(envelope)
    return {"request_id": request_id, "parent_task_id": arguments["parent_task_id"],
            "requested_thinking_profile": {"profile_id": envelope["thinking_profile_id"]},
            "state": "running", "business_verified": False, **updates}


def profile_refusal(**updates):
    return {"state": "profile_unavailable", "reason": "thinking_profiles_expired_refresh_required",
            "request_created": False, "launched": False,
            "thinking_profile_id": ENVELOPE["thinking_profile_id"], **updates}


class DispatchTests(unittest.TestCase):
    def setUp(self):
        self.control = Control()
        self.chat = Mock()
        self.chat.spawn_chat_task.return_value = chat_state()
        self.chat.read_chat_task.return_value = chat_state()

    def run_dispatch(self):
        return adapter.dispatch(self.control, self.chat, ENVELOPE["fallback_id"])

    def test_claim_precedes_spawn_and_bounded_record(self):
        events = []
        self.control.acquire.side_effect = lambda _: events.append("claim") or copy.deepcopy(ENVELOPE)
        self.chat.spawn_chat_task.side_effect = lambda **_: events.append("spawn") or chat_state()
        original = self.control.record
        self.control.record = lambda **value: events.append("record") or original(**value)
        result = self.run_dispatch()
        self.assertEqual(events, ["claim", "spawn", "record"])
        self.assertEqual(result["state"], "dispatched")
        self.assertTrue(result["slot_held"])
        self.assertFalse(result["business_verified"])
        self.assertEqual(set(self.control.records[0]),
                         {"fallback_id", "dispatch_attempt_id", "request_id", "state", "reason", "response_observed"})

    def test_prompt_forbids_cli_and_recursive_fallback_and_preserves_task(self):
        self.run_dispatch()
        arguments = self.chat.spawn_chat_task.call_args.kwargs
        for text in ("GPT保底", "禁止调用外部Agent CLI", "禁止递归委派", "从剩余工作续接", "不得重放",
                     ENVELOPE["task"], ENVELOPE["source_attempt_id"]):
            self.assertIn(text, arguments["task"])
        self.assertEqual(arguments["parent_task_id"], ENVELOPE["root_task_id"])
        self.assertEqual(arguments["thinking_profile_id"], ENVELOPE["thinking_profile_id"])

    def test_existing_binding_is_read_without_spawn(self):
        self.control.envelope["request_id"] = chat_state()["request_id"]
        self.chat.read_chat_task.return_value = chat_state(state="unknown")
        result = self.run_dispatch()
        self.chat.spawn_chat_task.assert_not_called()
        self.chat.read_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.assertEqual(result["state"], "unknown")
        self.assertTrue(result["slot_held"])

    def test_busy_retries_exact_same_arguments(self):
        self.chat.spawn_chat_task.return_value = {"state": "busy", "launched": False}
        result = self.run_dispatch()
        self.assertEqual(result["state"], "busy")
        self.assertFalse(result["launched"])
        self.assertTrue(result["retry_same_arguments"])
        self.assertFalse(result["slot_held"])  # Server acknowledged the safe requeue.
        self.control.envelope["dispatch_attempt_id"] = "9fefbca3-46fc-4fd4-85a8-75691cbaf405"
        self.run_dispatch()
        calls = self.chat.spawn_chat_task.call_args_list
        self.assertEqual(calls[0], calls[1])
        self.assertNotEqual(self.control.records[0]["dispatch_attempt_id"], self.control.records[1]["dispatch_attempt_id"])

    def test_busy_without_proven_no_launch_is_unknown(self):
        self.chat.spawn_chat_task.return_value = {"state": "busy"}
        self.assertEqual(self.run_dispatch()["state"], "unknown")

    def test_blocked_not_launched_does_not_requeue_as_busy(self):
        self.chat.spawn_chat_task.return_value = {"state": "blocked", "launched": False, "reason": "service_stopping"}
        self.assertEqual(self.run_dispatch()["state"], "unknown")
        self.assertEqual(self.control.records[0]["state"], "unknown")

    def test_explicit_profile_refusal_is_waiting_not_busy_or_unknown(self):
        self.chat.spawn_chat_task.return_value = profile_refusal()
        result = self.run_dispatch()
        self.assertEqual(result["state"], "profile_unavailable")
        self.assertTrue(result["retry_after_profile_refresh"])
        self.assertFalse(result["request_created"])
        self.assertFalse(result["slot_held"])  # No request exists; the acknowledged queue waits for refresh.
        self.assertNotIn("retry_same_arguments", result)
        self.assertEqual(self.control.records[0]["state"], "profile_unavailable")
        self.assertIsNone(self.control.records[0]["request_id"])
        self.assertFalse(self.control.records[0]["response_observed"])

    def test_profile_refusal_requires_complete_matching_not_created_proof(self):
        for change in ({"request_created": None}, {"request_created": 0}, {"launched": None},
                       {"thinking_profile_id": "other"}, {"reason": "arbitrary_failure"},
                       {"request_id": chat_state()["request_id"]}):
            with self.subTest(change=change):
                self.control = Control()
                self.chat.spawn_chat_task.return_value = profile_refusal(**change)
                self.assertEqual(self.run_dispatch()["state"], "unknown")
                self.assertEqual(self.control.records[0]["state"], "unknown")

    def test_read_cannot_supply_new_launch_profile_refusal(self):
        self.control.envelope["dispatch_permitted"] = False
        self.chat.read_chat_task.return_value = profile_refusal()
        self.assertEqual(self.run_dispatch()["state"], "unknown")
        self.chat.spawn_chat_task.assert_not_called()

    def test_profile_named_value_error_is_not_not_created_evidence(self):
        self.chat.spawn_chat_task.side_effect = ValueError("thinking_profiles_expired_refresh_required")
        self.assertEqual(self.run_dispatch()["state"], "unknown")
        self.assertEqual(self.control.records[0]["state"], "unknown")

    def test_unrecorded_profile_refusal_does_not_advertise_refresh_retry(self):
        self.chat.spawn_chat_task.return_value = profile_refusal()
        self.control.record = Mock(return_value={"recorded": False})
        result = self.run_dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertNotIn("retry_after_profile_refresh", result)

    def test_finished_worker_with_unconfirmed_send_preserves_request_without_retry(self):
        cases = [{}] + [{key: status} for key in ("status", "launch_status")
                        for status in ("blocked", "failed", "unknown")]
        for fields in cases:
            with self.subTest(fields=fields):
                self.control = Control()
                self.chat.reset_mock()
                observed = chat_state(state="finished", **fields)
                self.chat.spawn_chat_task.return_value = observed
                self.chat.read_chat_task.return_value = observed
                first = self.run_dispatch()
                self.assertEqual(first["state"], "unknown")
                self.assertEqual(first["request_id"], observed["request_id"])
                self.assertTrue(first["slot_held"])
                self.assertNotIn("retry_same_arguments", first)
                self.assertFalse(self.control.records[-1]["response_observed"])
                self.assertEqual(self.run_dispatch()["state"], "unknown")
                self.assertEqual(self.chat.spawn_chat_task.call_count, 1)
                self.chat.read_chat_task.assert_called_once_with(observed["request_id"])

    def test_uncertain_launch_requires_boolean_message_or_response_proof(self):
        for proof in ({"message_observed": "true"}, {"response_observed": 1},
                      {"message_observed": True}, {"response_observed": True}):
            with self.subTest(proof=proof):
                self.control = Control()
                self.chat.spawn_chat_task.return_value = chat_state(state="finished", status="unknown", **proof)
                result = self.run_dispatch()
                expected = "dispatched" if any(value is True for value in proof.values()) else "unknown"
                self.assertEqual(result["state"], expected)
                self.assertFalse(result["business_verified"])

    def test_finished_worker_with_sent_receipt_remains_dispatched(self):
        for key in ("status", "launch_status"):
            with self.subTest(key=key):
                self.control = Control()
                self.chat.spawn_chat_task.return_value = chat_state(state="finished", **{key: "sent"})
                self.assertEqual(self.run_dispatch()["state"], "dispatched")

    def test_repeated_claim_without_request_never_spawns(self):
        self.control.envelope["dispatch_permitted"] = False
        self.chat.read_chat_task.side_effect = FileNotFoundError("no persisted request")
        result = self.run_dispatch()
        self.chat.read_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.chat.spawn_chat_task.assert_not_called()
        self.assertEqual(result["state"], "unknown")
        self.assertTrue(result["slot_held"])

    def test_repeated_claim_recovers_existing_request_without_dispatch(self):
        self.control.envelope["dispatch_permitted"] = False
        result = self.run_dispatch()
        self.chat.read_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.chat.spawn_chat_task.assert_not_called()
        self.assertEqual(result["request_id"], chat_state()["request_id"])
        self.assertEqual(self.control.records[0]["request_id"], chat_state()["request_id"])

    def test_busy_from_read_cannot_requeue_claim(self):
        self.control.envelope["dispatch_permitted"] = False
        self.chat.read_chat_task.return_value = {"state": "busy", "launched": False}
        self.assertEqual(self.run_dispatch()["state"], "unknown")
        self.assertEqual(self.control.records[0]["state"], "unknown")

    def test_record_failure_keeps_unknown_and_does_not_resend(self):
        self.control.record = Mock(side_effect=RuntimeError("secret record failure"))
        result = self.run_dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["reason"], "control_record_failed")
        self.assertEqual(self.chat.spawn_chat_task.call_count, 1)
        self.assertNotIn("secret", str(result))

    def test_record_rejection_or_pending_is_not_durable_acceptance(self):
        for receipt in (None, {}, {"recorded": False}, {"recorded": "true"}, {"state": "pending"}):
            with self.subTest(receipt=receipt):
                self.control.record = Mock(return_value=receipt)
                result = self.run_dispatch()
                self.assertEqual(result["state"], "unknown")
                self.assertEqual(result["reason"], "control_record_failed")

    def test_unrecorded_busy_does_not_advertise_retry(self):
        self.chat.spawn_chat_task.return_value = {"state": "busy", "launched": False}
        self.control.record = Mock(side_effect=RuntimeError("record unavailable"))
        result = self.run_dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertTrue(result["slot_held"])
        self.assertNotIn("retry_same_arguments", result)

    def test_spawn_exception_is_sanitized_and_preserves_claim(self):
        self.chat.spawn_chat_task.side_effect = RuntimeError("private model output")
        result = self.run_dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertTrue(result["slot_held"])
        self.assertNotIn("private", str(result))
        self.assertEqual(self.control.records[0]["state"], "unknown")

    def test_bound_read_failure_never_calls_spawn(self):
        self.control.envelope["request_id"] = chat_state()["request_id"]
        self.chat.read_chat_task.side_effect = FileNotFoundError("missing")
        self.assertEqual(self.run_dispatch()["state"], "unknown")
        self.chat.spawn_chat_task.assert_not_called()

    def test_cancel_uses_only_local_cancel_and_preserves_slot(self):
        self.control.envelope.update(request_id=chat_state()["request_id"], cancel_requested=True)
        self.chat.cancel_chat_task.return_value = chat_state(cancel_requested=True)
        result = self.run_dispatch()
        self.chat.cancel_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.chat.spawn_chat_task.assert_not_called()
        self.assertEqual(result["state"], "cancel_requested")
        self.assertIn("may continue", result["cancellation_scope"])
        self.assertTrue(result["slot_held"])

    def test_cancel_before_binding_never_launches(self):
        self.control.envelope["cancel_requested"] = True
        self.assertEqual(self.run_dispatch()["state"], "cancel_requested")
        self.assertEqual(self.chat.mock_calls, [])

    def test_invalid_cancel_receipt_cannot_claim_local_cancellation(self):
        self.control.envelope.update(request_id=chat_state()["request_id"], cancel_requested=True)
        for receipt in (None, "bad", chat_state(parent_task_id="foreign")):
            with self.subTest(receipt=receipt):
                self.chat.cancel_chat_task.return_value = receipt
                result = self.run_dispatch()
                self.assertEqual(result["state"], "unknown")
                self.assertNotIn("cancellation_scope", result)
                self.chat.spawn_chat_task.assert_not_called()

    def test_cancel_recovers_unrecorded_request_only_by_read(self):
        self.control.envelope.update(dispatch_permitted=False, cancel_requested=True)
        self.chat.cancel_chat_task.return_value = chat_state(cancel_requested=True)
        result = self.run_dispatch()
        self.chat.read_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.chat.cancel_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.chat.spawn_chat_task.assert_not_called()
        self.assertEqual(result["state"], "cancel_requested")

    def test_candidate_answer_does_not_complete_fallback(self):
        self.chat.spawn_chat_task.return_value = chat_state(
            state="response_observed", response_observed=True, response_text="done", business_verified=True)
        result = self.run_dispatch()
        self.assertEqual(result["state"], "dispatched")
        self.assertFalse(result["business_verified"])
        self.assertFalse(result["chat_state"]["business_verified"])
        self.assertNotIn("response_text", self.control.records[0])

    def test_control_denial_prevents_all_chat_calls(self):
        for state in adapter.DENIED_STATES:
            with self.subTest(state=state):
                self.control.envelope["state"] = state
                self.assertEqual(self.run_dispatch()["state"], state)
        self.assertEqual(self.chat.mock_calls, [])
        self.assertEqual(self.control.records, [])

    def test_invalid_or_missing_envelope_values_never_launch(self):
        for updates in ({"thinking_profile_id": None}, {"thinking_profile_id": ""},
                        {"root_task_id": "../other"}, {"fallback_id": "foreign"},
                        {"source_attempt_id": None}, {"dispatch_attempt_id": None},
                        {"dispatch_attempt_id": "caller-label"}, {"dispatch_permitted": None},
                        {"dispatch_permitted": "true"},
                        {"task": "x" * 12000}, {"task": "\ud800"},
                        {"cancel_requested": "false"}, {"request_id": "00000000-0000-0000-0000-000000000000"},
                        {"state": "ready"}, {"state": []}):
            with self.subTest(updates=updates):
                self.control.envelope = {**ENVELOPE, **updates}
                self.assertEqual(self.run_dispatch()["state"], "unknown")
        self.assertEqual(self.chat.mock_calls, [])

    def test_returned_chat_identity_mismatch_is_not_bound(self):
        for updates in ({"request_id": "00000000-0000-0000-0000-000000000000"},
                        {"parent_task_id": "other"}, {"requested_thinking_profile": {"profile_id": "other"}},
                        {"state": "completed"}):
            with self.subTest(updates=updates):
                self.chat.spawn_chat_task.return_value = chat_state(**updates)
                result = self.run_dispatch()
                self.assertEqual(result["state"], "unknown")
                self.assertIsNone(result["request_id"])
                self.assertNotIn("chat_state", result)

    def test_acquire_failure_never_calls_chat(self):
        self.control.acquire.side_effect = RuntimeError("credential detail")
        result = self.run_dispatch()
        self.assertEqual(result["reason"], "control_acquire_failed")
        self.assertNotIn("credential", str(result))
        self.assertEqual(self.chat.mock_calls, [])


class ServiceIdentityTests(unittest.TestCase):
    def test_both_dispatchers_ignore_profile_expiry_and_keep_one_request(self):
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "chat-agents"))
        import chat_agents
        import team_agent_dispatch
        class Thread:
            def __init__(self, **kwargs):
                pass
            def start(self):
                pass
            def is_alive(self):
                return False
        ordinary = {**ENVELOPE, "team_agent_attempt_id": "ordinary-attempt-1", "source_task_id": "child-1"}
        for dispatcher, envelope, identifier in ((adapter, ENVELOPE, "fallback_id"),
                                                  (team_agent_dispatch, ordinary, "team_agent_attempt_id")):
            with self.subTest(dispatcher=dispatcher.__name__), tempfile.TemporaryDirectory() as temporary:
                runtime = Path(temporary)
                service = chat_agents.Service(runtime, thread_factory=Thread)
                def cache(observed_at):
                    chat_agents.write_json(runtime / "thinking-profiles.json", {
                        "state": "ready", "observed_at": observed_at,
                        "profiles": [{"profile_id": envelope["thinking_profile_id"]}]})
                cache(time.time())
                self.assertFalse(service.read_chat_profiles()["refresh_required"])
                service.selected_profile(envelope["thinking_profile_id"])
                control = Control(envelope)
                cache(1)  # The catalog expires after manager admission, before dispatch.
                started = dispatcher.dispatch(control, service, envelope[identifier])
                expected = dispatcher._launch_arguments(envelope)[1]
                self.assertEqual(started["request_id"], expected)
                self.assertEqual(started["state"], "unknown")  # Fake thread never executes a browser operation.
                self.assertEqual(started["chat_state"]["requested_thinking_profile"]["setting_error"], "thinking_profile_unavailable")
                control.envelope["dispatch_permitted"] = False
                cache(time.time())
                replayed = dispatcher.dispatch(control, service, envelope[identifier])
                self.assertEqual(replayed["request_id"], expected)
                self.assertEqual(replayed["state"], "unknown")
                self.assertEqual(replayed["chat_state"]["requested_thinking_profile"]["profile_id"], envelope["thinking_profile_id"])
                self.assertEqual(len(list((runtime / "tasks").glob("*/request.json"))), 1)
                self.assertEqual(len(list((runtime / "tasks").glob("*/jobs/*.json"))), 1)

    def test_real_service_replay_and_restart_use_one_persisted_request(self):
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "chat-agents"))
        import chat_agents
        class Thread:
            def __init__(self, **kwargs):
                self.kwargs = kwargs
            def start(self):
                pass
            def is_alive(self):
                return False
        with tempfile.TemporaryDirectory() as temporary:
            runtime = Path(temporary)
            service = chat_agents.Service(runtime, thread_factory=Thread)
            chat_agents.write_json(runtime / "thinking-profiles.json", {
                "state": "ready", "observed_at": time.time(),
                "profiles": [{"profile_id": ENVELOPE["thinking_profile_id"]}]})
            control = Control()
            first = adapter.dispatch(control, service, ENVELOPE["fallback_id"])
            self.assertEqual(first["request_id"], chat_state()["request_id"])
            self.assertEqual(first["state"], "unknown")
            # Simulate a crash after spawn but before Server stored its request.
            control.envelope["request_id"] = None
            control.envelope["dispatch_permitted"] = False
            service = chat_agents.Service(runtime, thread_factory=Thread)
            second = adapter.dispatch(control, service, ENVELOPE["fallback_id"])
            self.assertEqual(second["request_id"], first["request_id"])
            self.assertEqual(len(list((runtime / "tasks").glob("*/request.json"))), 1)
            self.assertEqual(len(list((runtime / "tasks").glob("*/jobs/*.json"))), 1)


if __name__ == "__main__":
    unittest.main()
