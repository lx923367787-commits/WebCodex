import unittest
from unittest.mock import Mock, patch

import cli_fallback_worker as worker
from cli_fallback_control_api import FallbackControlApi


class FallbackWorkerTests(unittest.TestCase):
    def setUp(self):
        self.api = Mock()
        self.api.discover.return_value = [{"fallback_id": "fallback-1", "state": "queued"}]
        self.service = Mock()
        self.service.browser_worker_busy.return_value = False
        self.state = lambda: {"mode": "running", "generation": 4}

    def test_pause_and_busy_do_not_acquire_or_discover(self):
        self.assertEqual(worker.advance(self.api, self.service,
                         lambda: {"mode": "paused"})["state"], "paused")
        self.api.discover.assert_not_called()
        self.service.browser_worker_busy.return_value = True
        with patch.object(worker, "dispatch") as dispatch:
            self.assertEqual(worker.advance(self.api, self.service, self.state)["observed"], 0)
            dispatch.assert_not_called()

    def test_generation_change_before_dispatch_stops_work(self):
        states = iter([self.state(), {"mode": "running", "generation": 5}])
        with patch.object(worker, "dispatch") as dispatch:
            self.assertEqual(worker.advance(self.api, self.service, lambda: next(states))["state"], "paused")
            dispatch.assert_not_called()

    def test_only_exact_existing_chat_is_collected(self):
        with patch.object(worker, "dispatch", return_value={
                "state": "dispatched", "request_id": "exact-request", "chat_state": {"worker_alive": False}}):
            self.assertEqual(worker.advance(self.api, self.service, self.state)["observed"], 1)
        self.service.collect_chat_task.assert_called_once_with("exact-request")
        self.service.spawn_chat_task.assert_not_called()

    def test_observed_result_and_cancellation_do_not_restart_collection(self):
        for chat in ({"response_observed": True}, {"cancel_requested": True}, {"worker_alive": True}):
            with patch.object(worker, "dispatch", return_value={"state": "dispatched", "request_id": "request", "chat_state": chat}):
                worker.advance(self.api, self.service, self.state)
        self.service.collect_chat_task.assert_not_called()

    def test_control_binding_is_fixed_and_catalog_is_bounded(self):
        api = object.__new__(FallbackControlApi)
        api.owner_user, api.project = "owner", "project"
        api.post = Mock(return_value={"candidates": [{"fallback_id": "fallback-1", "project": "project", "state": "queued"}], "next_cursor": None})
        self.assertEqual(api.discover(), [{"fallback_id": "fallback-1", "state": "queued"}])
        self.assertEqual(api.post.call_args.args[0], {"action": "cli_fallback_list", "owner_user": "owner", "project": "project", "limit": 100})
        api.post.return_value = {"candidates": [{"fallback_id": "fallback-1", "project": "foreign"}]}
        with self.assertRaises(Exception):
            api.discover()

    def test_busy_worker_still_checks_active_cancellation(self):
        self.service.browser_worker_busy.return_value = True
        self.api.discover.return_value = [{"fallback_id": "active", "state": "starting"}]
        with patch.object(worker, "dispatch", return_value={"state": "cancel_requested"}) as dispatch:
            worker.advance(self.api, self.service, self.state)
            dispatch.assert_called_once_with(self.api, self.service, "active")
        self.service.collect_chat_task.assert_not_called()

    def test_unknown_cancel_or_missing_chat_never_starts_collection(self):
        with patch.object(worker, "dispatch", return_value={"state": "unknown", "request_id": "request"}):
            worker.advance(self.api, self.service, self.state)
        self.service.collect_chat_task.assert_not_called()

    def test_ordinary_and_recovery_share_one_sender_in_recovery_first_order(self):
        ordinary = Mock()
        ordinary.discover.return_value = [{"team_agent_attempt_id": "ordinary-1", "state": "queued"}]
        calls = []
        with patch.object(worker, "dispatch", side_effect=lambda api, service, key: calls.append((service, key)) or {"state": "unknown"}), patch.object(worker, "dispatch_team_agent", side_effect=lambda api, service, key: calls.append((service, key)) or {"state": "unknown"}):
            result = worker.advance(self.api, self.service, self.state, ordinary)
        self.assertEqual(result["observed"], 2)
        self.assertEqual(calls, [(self.service, "fallback-1"), (self.service, "ordinary-1")])
        self.service.collect_chat_task.assert_not_called()

    def test_one_busy_sender_defers_ordinary_queued_work(self):
        ordinary = Mock()
        ordinary.discover.return_value = [{"team_agent_attempt_id": "ordinary-1", "state": "queued"}]
        def start_recovery(*_):
            self.service.browser_worker_busy.return_value = True
            return {"state": "dispatched"}
        with patch.object(worker, "dispatch", side_effect=start_recovery), patch.object(worker, "dispatch_team_agent") as ordinary_send:
            result = worker.advance(self.api, self.service, self.state, ordinary)
        self.assertEqual(result["observed"], 1)
        ordinary_send.assert_not_called()

    def test_pause_prevents_both_kinds_of_work(self):
        ordinary = Mock()
        worker.advance(self.api, self.service, lambda: {"mode": "paused"}, ordinary)
        self.api.discover.assert_not_called()
        ordinary.discover.assert_not_called()

    def test_failed_queue_does_not_prevent_other_queue_or_consume_failed_cursor(self):
        self.api.discover.side_effect = ValueError("cursor rejected")
        ordinary = Mock()
        ordinary.discover.return_value = [{"team_agent_attempt_id": "ordinary-1", "state": "queued"}]
        with patch.object(worker, "dispatch_team_agent", return_value={"state": "unknown"}) as send:
            with self.assertRaisesRegex(ValueError, "candidate_processing_failed"):
                worker.advance(self.api, self.service, self.state, ordinary)
            send.assert_called_once_with(ordinary, self.service, "ordinary-1")
        self.api.finish_discovery.assert_not_called()
        ordinary.finish_discovery.assert_called_once_with()

    def test_failed_candidate_does_not_pin_discovery_before_later_roots(self):
        self.api.discover.return_value = [{"fallback_id": "old", "state": "unknown"},
                                          {"fallback_id": "new-root", "state": "queued"}]
        with patch.object(worker, "dispatch", side_effect=[RuntimeError(), {"state": "unknown"}]) as send:
            with self.assertRaisesRegex(ValueError, "candidate_processing_failed"):
                worker.advance(self.api, self.service, self.state)
            self.assertEqual(send.call_args_list[-1].args[-1], "new-root")
        self.api.finish_discovery.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
