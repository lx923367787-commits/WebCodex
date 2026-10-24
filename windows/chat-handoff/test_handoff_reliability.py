"""Offline transport simulations. No browser or production Server is used."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import uuid

import auto_handoff as ah
import handoff_controller as hc
import handoff_reliability as reliable

ORIGIN = "http://127.0.0.1:19876"
CANDIDATE = {"project": "project-test", "goal_id": "goal-test", "source_context_epoch": "epoch-A", "checkpoint_sequence": 1}


def evidence(candidate=None):
    c = candidate or CANDIDATE
    return {"driver": "host", "projection": "handoff",
        "goal": {"goal_id": c["goal_id"], "project": c["project"], "phase": "executing", "current_attempt_id": None, "fence": 1},
        "source_context": {"context_epoch": c["source_context_epoch"], "identity_source": "window_bound", "lifecycle_state": "draining", "retired_at": None},
        "checkpoint": {"sequence": c["checkpoint_sequence"], "revision": 5, "action": "handoff", "source_context_epoch": c["source_context_epoch"], "source_fence": 1, "created_at": 100000},
        "current_attempt": None, "unresolved_operation_count": 0, "business_verified": False, "business_evidence": None, "resume_started": None,
        "handoff": {"handoff_id": "handoff-1", **c, "plan_version": 5, "status": "waiting", "attempt_count": 0,
            "attempt_id": None, "candidate_context_epoch": None, "deadline_at": None, "timeout_secs": 300,
            "prepared_at": None, "execution_owner_epoch": "epoch-A", "failure_reason": None, "created_at": 100000, "committed_at": None}}


class FakeApi:
    """Persisted protocol fixture; Server atomicity has separate Rust tests."""
    def __init__(self, state_path=None):
        self.path = Path(state_path) if state_path else None
        self.raw = json.loads(self.path.read_text()) if self.path and self.path.exists() else evidence()
        self.candidates = [copy.deepcopy(CANDIDATE)]
        self.now, self.reads = 220, 0
        self.actions, self.invalidated = [], []
        self.on_read = self.on_action = self.drop_response = None

    def save(self):
        if self.path:
            self.path.write_text(json.dumps(self.raw))

    def discover(self):
        return copy.deepcopy(self.candidates)

    def read(self, candidate):
        self.reads += 1
        if self.on_read:
            self.on_read(self.reads)
        return copy.deepcopy(self.raw)

    def commit(self):
        self.raw["handoff"].update(status="committed", candidate_context_epoch="epoch-B", execution_owner_epoch="epoch-B", committed_at=int(self.now * 1000))
        self.raw["source_context"].update(lifecycle_state="retired", retired_at=int(self.now * 1000))
        self.raw["goal"]["fence"] += 1
        self.save()

    def call(self, tool, params):
        if tool != "goal_handoff":
            raise AssertionError(tool)
        self.actions.append(copy.deepcopy(params))
        if self.on_action:
            self.on_action(params)
        h, action = self.raw["handoff"], params["action"]
        if params["handoff_id"] != h["handoff_id"]:
            raise hc.ReadFailure()
        if h["status"] not in ("committed", "failed"):
            if action == "abort":
                h.update(status="failed", failure_reason=params["reason"], candidate_context_epoch=None)
            elif action == "fail_attempt" and params.get("attempt_id") == h["attempt_id"]:
                self.invalidated.append(h["attempt_id"])
                h.update(status="waiting", candidate_context_epoch=None, deadline_at=None)
            elif action == "begin_attempt" and (h["status"] == "waiting" or self.now * 1000 >= h["deadline_at"]):
                if h["attempt_id"] and h["attempt_id"] not in self.invalidated:
                    self.invalidated.append(h["attempt_id"])
                if h["attempt_count"] >= 4:
                    h.update(status="failed", failure_reason="attempts_exhausted", candidate_context_epoch=None)
                    self.raw["goal"]["phase"] = "blocked"
                else:
                    h.update(status="awaiting_ready", attempt_count=h["attempt_count"] + 1, attempt_id=str(uuid.uuid4()),
                        candidate_context_epoch=None, deadline_at=int(self.now * 1000 + h["timeout_secs"] * 1000))
        self.save()
        if action == self.drop_response:
            self.drop_response = None
            raise hc.ReadFailure()
        return {"handoff": copy.deepcopy(h), "execution_granted": False}


class SchedulerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.path = self.root / "scheduler.sqlite3"
        self.store = ah.Store(self.path)
        self.store.initialize(ORIGIN)
        self.api = FakeApi(self.root / "simulated-server.json")
        self.now, self.calls = 220, []
        self.policy = patch("chat_task_policy.resolve_goal_profile", return_value=None)
        self.policy.start()

    def tearDown(self):
        self.policy.stop()
        self.store.close()
        self.temp.cleanup()

    def launcher(self, path, stop):
        self.assertFalse(stop(force_refresh=True))
        self.calls.append(json.loads(path.read_text(encoding="utf-8")))
        return {"status": "sent"}

    def poll(self, launcher=None):
        self.api.now = self.now
        return ah.poll_once(self.store, self.api, launcher or self.launcher, clock=lambda: self.now, request_dir=self.root / "requests")

    def status(self):
        return self.store.entries()[0]["status"]

    def restart(self, server=False):
        self.store.close()
        self.store = ah.Store(self.path)
        self.store.initialize(ORIGIN)
        self.store.recover_interrupted()
        if server:
            self.api = FakeApi(self.root / "simulated-server.json")

    def expire(self):
        self.now = self.api.raw["handoff"]["deadline_at"] / 1000

    def test_launch_delay_separate_from_takeover_timeout_source_not_retired(self):
        self.now = 219.999
        self.poll()
        self.assertEqual([], self.calls)
        self.now = 220
        self.poll()
        self.assertEqual(1, len(self.calls))
        self.assertEqual("draining", self.api.raw["source_context"]["lifecycle_state"])
        self.assertEqual(520000, self.store.entries()[0]["ready_deadline_at"])
        self.assertEqual(ah.utc_iso(520), self.calls[0]["expires_at"])
        self.assertEqual("waiting_ready", self.status())

    def test_first_attempt_committed_original_goal_then_source_retired(self):
        def committed(path, stop):
            self.launcher(path, stop)
            self.api.commit()
            return {"status": "sent"}
        self.poll(committed)
        self.assertEqual("resumed", self.status())
        self.assertEqual("retired", self.api.raw["source_context"]["lifecycle_state"])
        self.assertEqual(CANDIDATE["goal_id"], self.calls[0]["goal_id"])
        self.assertIn("execution_granted=true", self.calls[0]["recovery_prompt"])
        self.assertIn("action=commit，携带同一goal_id/handoff_id/attempt_id/checkpoint_sequence", self.calls[0]["recovery_prompt"])
        self.assertNotIn("activate=true", self.calls[0]["recovery_prompt"])
        self.now = 10000
        self.poll()
        self.assertEqual(1, len(self.calls))

    def test_prepared_and_resume_started_not_committed(self):
        self.poll()
        self.api.raw["handoff"].update(status="prepared", candidate_context_epoch="epoch-B", prepared_at=230000)
        self.api.raw["resume_started"] = {"context_epoch": "epoch-B", "started_at": 230000, "identity_source": "window_bound"}
        self.now = 230
        self.poll()
        self.assertEqual("waiting_ready", self.status())
        self.assertEqual(1, len(self.calls))
        self.assertEqual("draining", self.api.raw["source_context"]["lifecycle_state"])

    def test_first_three_timeout_fourth_commits_four_adapter_calls(self):
        for number in range(1, 5):
            def launch(path, stop):
                result = self.launcher(path, stop)
                if number == 4:
                    self.api.commit()
                return result
            self.poll(launch)
            self.assertEqual(number, self.api.raw["handoff"]["attempt_count"])
            if number != 4:
                self.expire()
        self.assertEqual("resumed", self.status())
        self.assertEqual(4, len({call["launch_id"] for call in self.calls}))
        self.assertEqual([call["launch_id"] for call in self.calls[:3]], self.api.invalidated)

    def test_all_four_timeout_terminal_preserves_checkpoint_no_fifth(self):
        original = copy.deepcopy(self.api.raw["checkpoint"])
        for _ in range(4):
            self.poll()
            self.expire()
        self.poll()
        for _ in range(5):
            self.now += 10000
            self.poll()
        self.assertEqual(4, len(self.calls))
        self.assertEqual("failed", self.status())
        self.assertEqual(original, self.api.raw["checkpoint"])
        self.assertEqual("blocked", self.api.raw["goal"]["phase"])
        self.assertEqual("attempts_exhausted", self.store.entries()[0]["reason"])

    def test_positive_no_launch_failure_revokes_before_retry(self):
        def unavailable(path, stop):
            self.launcher(path, stop)
            return {"status": "retryable", "launched": False}
        for _ in range(4):
            self.poll(unavailable)
            self.now += 1
        self.poll(unavailable)
        self.assertEqual(4, len(self.calls))
        self.assertEqual("failed", self.status())
        self.assertTrue(all(call["launch_id"] in self.api.invalidated for call in self.calls))

    def test_duplicate_discovery_repeat_poll_no_duplicate_launch(self):
        self.api.candidates *= 2
        for _ in range(5):
            self.poll()
        self.assertEqual(1, len(self.calls))
        self.assertEqual(1, self.api.raw["handoff"]["attempt_count"])

    def test_restart_preserves_count_attempt_and_deadlines(self):
        self.poll()
        self.expire()
        self.poll()
        before = dict(self.store.entries()[0])
        self.restart(server=True)
        self.poll()
        self.assertEqual(2, len(self.calls))
        for key in ("attempt_id", "attempt_count", "ready_deadline_at", "deadline_at"):
            self.assertEqual(before[key], self.store.entries()[0][key])

    def test_interrupted_claim_never_replays_same_attempt(self):
        self.poll()
        with self.store.db:
            self.store.db.execute("UPDATE dispatch_attempt SET status='launch_started'")
        self.restart()
        self.poll()
        self.assertEqual(1, len(self.calls))
        self.assertEqual("unknown", reliable.attempt(self.store, self.calls[0]["attempt_id"])["status"])
        self.expire()
        self.poll()
        self.assertEqual(2, len(self.calls))

    def test_commit_response_lost_adapter_exception_queries_success(self):
        def lost(path, stop):
            self.launcher(path, stop)
            self.api.commit()
            raise RuntimeError("simulated lost response")
        self.poll(lost)
        self.assertEqual("resumed", self.status())
        self.expire()
        self.poll()
        self.assertEqual(1, len(self.calls))

    def test_begin_response_lost_queries_attempt_launches_once(self):
        self.api.drop_response = "begin_attempt"
        self.poll()
        self.poll()
        self.assertEqual(1, len(self.calls))
        actions = [call["action"] for call in self.api.actions]
        self.assertEqual("status", actions[actions.index("begin_attempt") + 1])

    def test_commit_racing_fail_no_next_launch(self):
        self.poll(lambda path, stop: (self.launcher(path, stop), {"status": "blocked", "launched": False})[1])
        def race(params):
            if params["action"] == "fail_attempt":
                self.api.commit()
        self.api.on_action = race
        self.poll()
        self.assertEqual("resumed", self.status())
        self.assertEqual(1, len(self.calls))

    def test_fresh_commit_before_adapter_prevents_send(self):
        def race(params):
            if params["action"] == "status" and self.api.raw["handoff"]["attempt_id"]:
                self.api.commit()
        self.api.on_action = race
        self.poll()
        self.assertEqual([], self.calls)
        self.assertEqual("resumed", self.status())

    def test_control_stop_racing_status_prevents_send(self):
        def stop(params):
            if params["action"] == "status":
                other = ah.Store(self.path)
                other.set_mode("stopped")
                other.close()
        self.api.on_action = stop
        self.poll()
        self.assertEqual("stopped", self.store.status()["mode"])
        self.assertEqual([], self.calls)

    def test_expired_candidate_cancels_adapter(self):
        def launch(path, stop):
            self.launcher(path, stop)
            self.expire()
            self.assertTrue(stop())
            return {"status": "unknown"}
        self.poll(launch)
        self.assertEqual("waiting_ready", self.status())

    def test_read_failure_does_not_allocate_replacement(self):
        self.poll()
        self.expire()
        actions = len(self.api.actions)
        with patch.object(self.api, "call", side_effect=hc.ReadFailure("connector_auth")):
            self.poll()
        self.assertEqual(actions, len(self.api.actions))
        self.assertEqual(1, len(self.calls))
        self.assertEqual(1, self.store.status()["failures"])

    def test_bounded_discovery_keeps_waiting_candidate(self):
        self.poll()
        self.api.candidates = []
        self.expire()
        self.poll()
        self.assertEqual(2, len(self.calls))

    def test_agent_binding_persists_beyond_discovery_and_restart(self):
        self.api.candidates[0]["agent_id"] = "agent-test"
        self.api.raw["handoff"]["agent_id"] = "agent-test"
        self.poll()
        self.restart(server=True)
        self.api.candidates = []
        read = self.api.read
        seen = []
        self.api.read = lambda candidate: (seen.append(candidate), read(candidate))[1]
        self.poll()
        self.assertEqual("agent-test", self.store.entries()[0]["agent_id"])
        self.assertEqual("agent-test", seen[0]["agent_id"])

    def test_private_data_not_copied_to_transport_or_prompt(self):
        self.api.raw["private_memory"] = "private-secret-marker"
        self.api.raw["handoff"]["private_paths"] = ["private-secret-marker"]
        self.api.raw["checkpoint"]["unrelated_history"] = "private-secret-marker"
        self.poll()
        self.assertNotIn("private-secret-marker", json.dumps(self.calls))
        self.assertNotIn("private-secret-marker", self.store.entries()[0]["snapshot"])

    def test_wrong_project_checkpoint_or_attempt_count_rejected(self):
        for key, invalid in (("project", "other"), ("checkpoint_sequence", 2), ("attempt_count", 5)):
            raw = copy.deepcopy(self.api.raw)
            raw["handoff"][key] = invalid
            with self.subTest(key=key), self.assertRaises(hc.ReadFailure):
                reliable.observation(raw, CANDIDATE)

    def test_same_source_claimed_as_success_rejected(self):
        self.api.raw["handoff"].update(status="committed", committed_at=220000, execution_owner_epoch="epoch-A")
        with self.assertRaises(hc.ReadFailure):
            ah.read_observation(self.api, CANDIDATE)

    def test_controls_survive_migration(self):
        self.store.set_mode("stopped")
        self.restart()
        self.assertEqual("stopped", self.store.status()["mode"])
        self.assertEqual(1, self.store.status()["generation"])

    def test_failed_survives_both_restarts_no_auto_restore(self):
        self.poll()
        self.api.raw["handoff"].update(status="failed", failure_reason="unsafe_old_execution", candidate_context_epoch=None)
        self.api.save()
        self.poll()
        self.restart(server=True)
        self.now = 10000
        self.poll()
        self.assertEqual("failed", self.status())
        self.assertEqual(1, len(self.calls))

    def test_failed_real_projection_source_stopped_is_terminal(self):
        self.poll()
        self.api.raw["source_context"]["lifecycle_state"] = "stopped"
        self.api.raw["goal"]["phase"] = "blocked"
        self.api.raw["handoff"].update(status="failed", failure_reason="attempts_exhausted", candidate_context_epoch=None)
        self.poll()
        self.assertEqual("failed", self.status())
        self.assertEqual(0, self.store.status()["failures"])
        self.assertEqual("HANDOFF_FAILED", hc.normalize(self.api.raw, CANDIDATE)["phase"])

    def test_planning_checkpoint_without_fence_is_observation_only(self):
        self.api.raw["goal"]["phase"] = "planning"
        self.api.raw["checkpoint"]["source_fence"] = None
        self.poll()
        self.assertEqual(1, len(self.calls))
        self.assertIsNone(json.loads(self.store.entries()[0]["snapshot"])["source_fence"])
        self.assertEqual("WAITING_NEW_CHAT", hc.normalize(self.api.raw, CANDIDATE)["phase"])

    def test_millisecond_initial_delay_never_fires_early(self):
        self.api.raw["handoff"]["created_at"] = 100001
        self.now = 220.999
        self.poll()
        self.assertEqual([], self.calls)
        self.now = 221
        self.poll()
        self.assertEqual(1, len(self.calls))

    def test_predates_activation_never_starts(self):
        with self.store.db:
            self.store.db.execute("UPDATE scheduler SET armed_at=101")
        self.poll()
        self.assertEqual([], self.calls)
        self.assertEqual("cancelled", self.status())

    def test_abort_nonactive_goal(self):
        self.api.raw["goal"]["phase"] = "cancelled"
        self.poll()
        self.assertEqual("failed", self.status())
        self.assertEqual([], self.calls)


if __name__ == "__main__":
    unittest.main()
