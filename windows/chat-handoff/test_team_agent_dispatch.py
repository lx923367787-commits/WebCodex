"""Offline ordinary-agent transport tests. No browser, network or model calls."""
import copy
import unittest
import uuid
from unittest.mock import Mock

import team_agent_dispatch as adapter

ENVELOPE = {"state": "claimed", "team_agent_attempt_id": "wc_team_agent_attempt_example",
            "project": "agent:runner:repo", "root_task_id": "wc_agent_task_root",
            "source_task_id": "wc_agent_task_child", "task": "Verify the existing feature and report evidence.",
            "thinking_profile_id": "manager-selected-deep", "dispatch_attempt_id": "8cac1554-cb46-45ec-90db-e83c225949d2",
            "dispatch_permitted": True, "request_id": None, "cancel_requested": False}


class Control:
    def __init__(self):
        self.envelope = copy.deepcopy(ENVELOPE)
        self.records = []

    def acquire(self, attempt_id):
        result = copy.deepcopy(self.envelope)
        self.envelope["dispatch_permitted"] = False
        return result

    def record(self, **record):
        self.records.append(record)
        if record["request_id"] is not None:
            self.envelope["request_id"] = record["request_id"]
        return {"recorded": True}


def chat_state(**updates):
    arguments, expected = adapter._launch_arguments(ENVELOPE)
    return {"request_id": expected, "parent_task_id": arguments["parent_task_id"],
            "requested_thinking_profile": {"profile_id": arguments["thinking_profile_id"]},
            "state": "running", "business_verified": False, **updates}


class OrdinaryDispatchTests(unittest.TestCase):
    def setUp(self):
        self.control = Control()
        self.service = Mock()
        self.service.spawn_chat_task.return_value = chat_state()
        self.service.read_chat_task.return_value = chat_state()

    def dispatch(self):
        return adapter.dispatch(self.control, self.service, ENVELOPE["team_agent_attempt_id"])

    def test_dispatch_uses_explicit_manager_profile_and_planned_cli_budget(self):
        result = self.dispatch()
        self.assertEqual(result["state"], "dispatched")
        arguments = self.service.spawn_chat_task.call_args.kwargs
        self.assertEqual(arguments["thinking_profile_id"], ENVELOPE["thinking_profile_id"])
        self.assertEqual(arguments["parent_task_id"], ENVELOPE["root_task_id"])
        for required in ("普通子助手", "team_cli_start", "source_task_id", ENVELOPE["task"]):
            self.assertIn(required, arguments["task"])
        self.assertNotIn("GPT保底", arguments["task"])
        self.assertNotIn("禁止调用外部Agent CLI", arguments["task"])
        self.assertFalse(result["business_verified"])
        self.assertEqual(set(self.control.records[0]), {"team_agent_attempt_id", "dispatch_attempt_id", "request_id", "state", "reason", "response_observed"})

    def test_replay_reads_original_identity_without_spawning_again(self):
        first = self.dispatch()
        self.control.envelope["request_id"] = None  # Lost Server receipt, retained claim.
        second = self.dispatch()
        self.assertEqual(second["request_id"], first["request_id"])
        self.assertEqual(self.service.spawn_chat_task.call_count, 1)
        self.service.read_chat_task.assert_called_once_with(first["request_id"])

    def test_unknown_spawn_or_missing_recovery_does_not_retry(self):
        self.service.spawn_chat_task.side_effect = RuntimeError("private exception")
        self.assertEqual(self.dispatch()["state"], "unknown")
        self.service.read_chat_task.side_effect = FileNotFoundError("missing")
        result = self.dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(self.service.spawn_chat_task.call_count, 1)
        self.assertNotIn("private", str(result))

    def test_only_explicit_busy_not_launched_can_requeue_same_logical_request(self):
        self.service.spawn_chat_task.return_value = {"state": "busy", "launched": False}
        first = self.dispatch()
        self.assertTrue(first["retry_same_arguments"])
        self.control.envelope.update(dispatch_permitted=True, dispatch_attempt_id=str(uuid.uuid4()))
        self.dispatch()
        self.assertEqual(self.service.spawn_chat_task.call_args_list[0], self.service.spawn_chat_task.call_args_list[1])
        self.control = Control()
        self.service.spawn_chat_task.return_value = {"state": "busy"}
        self.assertEqual(self.dispatch()["state"], "unknown")

    def test_profile_refusal_preserves_manager_selection_and_same_key_after_refresh(self):
        self.service.spawn_chat_task.return_value = {
            "state": "profile_unavailable", "reason": "thinking_profiles_expired_refresh_required",
            "request_created": False, "launched": False, "thinking_profile_id": ENVELOPE["thinking_profile_id"]}
        first = self.dispatch()
        self.assertEqual(first["state"], "profile_unavailable")
        self.assertTrue(first["retry_after_profile_refresh"])
        self.assertIsNone(self.control.records[0]["request_id"])
        self.assertEqual(self.control.records[0]["state"], "profile_unavailable")
        self.service.spawn_chat_task.return_value = chat_state()
        self.control.envelope.update(dispatch_permitted=True, dispatch_attempt_id=str(uuid.uuid4()))
        self.assertEqual(self.dispatch()["state"], "dispatched")
        self.assertEqual(self.service.spawn_chat_task.call_args_list[0], self.service.spawn_chat_task.call_args_list[1])

    def test_profile_refusal_cannot_be_inferred_from_error_or_read(self):
        self.service.spawn_chat_task.side_effect = ValueError("thinking_profiles_expired_refresh_required")
        self.assertEqual(self.dispatch()["state"], "unknown")
        self.service.read_chat_task.return_value = {
            "state": "profile_unavailable", "reason": "thinking_profiles_expired_refresh_required",
            "request_created": False, "launched": False, "thinking_profile_id": ENVELOPE["thinking_profile_id"]}
        self.assertEqual(self.dispatch()["state"], "unknown")
        self.assertEqual(self.service.spawn_chat_task.call_count, 1)

    def test_candidate_response_never_becomes_business_success(self):
        self.service.spawn_chat_task.return_value = chat_state(state="response_observed", response_observed=True, business_verified=True, response_text="Candidate answer")
        result = self.dispatch()
        self.assertEqual(result["state"], "dispatched")
        self.assertFalse(result["business_verified"])
        self.assertFalse(result["chat_state"]["business_verified"])
        self.assertTrue(self.control.records[0]["response_observed"])
        self.assertNotIn("response_text", self.control.records[0])

    def test_cancelled_queue_never_launches_and_active_cancel_only_requests_stop(self):
        self.control.envelope["state"] = "cancelled"
        self.assertEqual(self.dispatch()["state"], "cancelled")
        self.assertEqual(self.service.mock_calls, [])
        self.control = Control()
        self.control.envelope.update(request_id=chat_state()["request_id"], cancel_requested=True)
        self.service.cancel_chat_task.return_value = chat_state(cancel_requested=True)
        result = self.dispatch()
        self.service.spawn_chat_task.assert_not_called()
        self.service.cancel_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.assertEqual(result["state"], "cancel_requested")
        self.assertIn("may continue", result["cancellation_scope"])

    def test_cancel_can_recover_unrecorded_identity_without_resending(self):
        self.control.envelope.update(dispatch_permitted=False, cancel_requested=True)
        self.service.cancel_chat_task.return_value = chat_state(cancel_requested=True)
        result = self.dispatch()
        self.service.read_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.service.cancel_chat_task.assert_called_once_with(chat_state()["request_id"])
        self.service.spawn_chat_task.assert_not_called()
        self.assertEqual(result["state"], "cancel_requested")

    def test_invalid_profile_source_fence_and_identity_never_default_or_spawn(self):
        for updates in ({"thinking_profile_id": None}, {"thinking_profile_id": ""},
                        {"source_task_id": ENVELOPE["root_task_id"]}, {"dispatch_attempt_id": "not-a-fence"},
                        {"request_id": str(uuid.uuid4())}, {"dispatch_permitted": "true"},
                        {"task": "x" * 12000}, {"task": "\ud800"}):
            with self.subTest(updates=updates):
                self.control = Control()
                self.control.envelope.update(updates)
                self.assertEqual(self.dispatch()["state"], "unknown")
        self.service.spawn_chat_task.assert_not_called()

    def test_failed_durable_receipt_remains_unknown_and_does_not_advertise_retry(self):
        self.control.record = Mock(return_value={"recorded": False})
        self.service.spawn_chat_task.return_value = {"state": "busy", "launched": False}
        result = self.dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertNotIn("retry_same_arguments", result)

    def test_finished_worker_without_sent_receipt_remains_unknown(self):
        self.service.spawn_chat_task.return_value = chat_state(state="finished")
        result = self.dispatch()
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["request_id"], chat_state()["request_id"])


class ControlContractTests(unittest.TestCase):
    def test_scoped_control_list_and_actions_use_no_generic_tool_or_prompt(self):
        api = object.__new__(adapter.TeamAgentControlApi)
        api.owner_user, api.project = "owner", ENVELOPE["project"]
        api.post = Mock(return_value={"candidates": [{"project": api.project, "team_agent_attempt_id": ENVELOPE["team_agent_attempt_id"], "state": "queued"}], "next_cursor": None})
        self.assertEqual(api.discover(), [{"team_agent_attempt_id": ENVELOPE["team_agent_attempt_id"], "state": "queued"}])
        self.assertEqual(api.post.call_args.args[0], {"action": "team_agent_list", "owner_user": "owner", "project": api.project, "limit": 100})
        api.post.return_value = {"candidates": [{"project": "agent:other:repo", "team_agent_attempt_id": "id", "state": "queued"}]}
        with self.assertRaises(ValueError):
            api.discover()


if __name__ == "__main__":
    unittest.main()
