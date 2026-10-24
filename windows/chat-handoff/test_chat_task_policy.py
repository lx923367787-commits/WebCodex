"""Offline local policy checks; no Server, browser or credential access."""
from __future__ import annotations

import contextlib
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import uuid

import chat_task_policy as policy


class ChatTaskPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="chat-task-policy-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.lock = patch.object(policy, "watcher_lock", side_effect=lambda path: contextlib.nullcontext())
        self.lock_mock = self.lock.start()
        self.addCleanup(self.lock.stop)
        self.project, self.goal = "agent:runner-test:project-test", "goal-test-not-a-uuid"
        self.request_id = str(uuid.uuid4())
        self.profile = {"profile_id": "pro_medium", "intelligence": "Pro",
                        "reasoning_effort": "medium", "metadata": {"ticks": [1, 2], "verified": True}}
        self.prepare(self.request_id)

    def prepare(self, request_id, profile=None):
        profile = self.profile if profile is None else profile
        config = {"launch_id": request_id, "task_id": "wc_chat_" + uuid.UUID(request_id).hex,
                  "thinking_profile": profile, "prompt": "An authorized child task.",
                  "parent_session_id": "parent-test"}
        policy.write_json(self.root / "tasks" / request_id / "request.json", {"config": config, "intent_sha256": "test-intent"})
        policy.write_json(self.root / "evidence" / request_id / "browser.json",
                          {key: config[key] for key in ("launch_id", "task_id", "thinking_profile")})

    def register(self, request_id=None, goal=None):
        return policy.register_goal_binding(request_id or self.request_id, self.project,
                                            goal or self.goal, runtime=self.root)

    def resolve(self, goal=None):
        return policy.resolve_goal_profile(self.project, goal or self.goal, runtime=self.root)

    def request_path(self):
        return self.root / "tasks" / self.request_id / "request.json"

    def browser_path(self):
        return self.root / "evidence" / self.request_id / "browser.json"

    def binding_path(self):
        name = hashlib.sha256((self.project + "\0" + self.goal).encode()).hexdigest()
        return self.root / "goal-bindings" / (name + ".json")

    def edit(self, path, change):
        value = policy.read_json(path)
        change(value)
        policy.write_json(path, value)

    def test_unbound_goal_uses_main_default_without_creating_binding(self):
        self.assertIsNone(self.resolve())
        self.assertFalse((self.root / "goal-bindings").exists())
        self.assertIsNone(policy.resolve_goal_profile("project", "goal", runtime=self.root / "absent"))

    def test_registration_is_explicitly_unvalidated_and_exactly_idempotent(self):
        first = self.register()
        initial = self.binding_path().read_bytes()
        second = self.register()
        self.assertEqual(first["registration_state"], "awaiting_real_goal_handoff_validation")
        self.assertFalse(first["replayed"])
        self.assertTrue(second["replayed"])
        self.assertEqual(first["thinking_profile"], self.profile)
        self.assertEqual(self.binding_path().read_bytes(), initial)
        self.assertNotIn("validated", {key: value for key, value in first.items() if key != "registration_state"})
        self.lock_mock.assert_called_with(self.root / "registry")

    def test_two_epoch_handoff_reads_keep_same_goal_policy(self):
        self.register()
        original = self.binding_path().read_bytes()
        # These are simulated outputs of the caller's separate Server gate;
        # the resolver deliberately keys the policy by Goal, never by epoch.
        handoffs = [{"project": self.project, "goal_id": self.goal,
                     "source_context_epoch": "epoch-A", "checkpoint_sequence": 1},
                    {"project": self.project, "goal_id": self.goal,
                     "source_context_epoch": "epoch-B", "checkpoint_sequence": 2}]
        results = [policy.resolve_goal_profile(item["project"], item["goal_id"], runtime=self.root)
                   for item in handoffs]
        self.assertEqual(results[0], results[1])
        self.assertEqual(results[1]["thinking_profile"], self.profile)
        self.assertEqual(results[1]["request_id"], self.request_id)
        self.assertEqual(self.binding_path().read_bytes(), original)

    def test_other_task_cannot_rebind_same_goal_even_with_same_profile(self):
        self.register()
        original = self.binding_path().read_bytes()
        other = str(uuid.uuid4())
        self.prepare(other)
        with self.assertRaisesRegex(policy.PolicyError, "goal_binding_conflict"):
            self.register(other)
        self.assertEqual(self.binding_path().read_bytes(), original)

    def test_same_task_changed_profile_is_not_a_retry(self):
        self.register()
        changed = {"profile_id": "pro_high", "reasoning_effort": "high"}
        self.prepare(self.request_id, changed)
        for operation in (self.register, self.resolve):
            with self.assertRaisesRegex(policy.PolicyError, "goal_binding_evidence_changed"):
                operation()

    def test_request_change_with_unchanged_profile_fails_closed(self):
        self.register()
        self.edit(self.request_path(), lambda value: value["config"].update(prompt="Different task."))
        with self.assertRaisesRegex(policy.PolicyError, "goal_binding_evidence_changed"):
            self.resolve()

    def test_missing_request_after_registration_never_falls_back_to_main(self):
        self.register()
        self.request_path().unlink()
        with self.assertRaisesRegex(policy.PolicyError, "chat_task_request_missing"):
            self.resolve()

    def test_missing_browser_proof_prevents_registration_and_resolution(self):
        self.browser_path().unlink()
        with self.assertRaisesRegex(policy.PolicyError, "chat_task_browser_evidence_missing"):
            self.register()
        self.assertFalse(self.binding_path().exists())
        self.prepare(self.request_id)
        self.register()
        self.browser_path().unlink()
        with self.assertRaisesRegex(policy.PolicyError, "chat_task_browser_evidence_missing"):
            self.resolve()

    def test_browser_profile_mismatch_is_rejected(self):
        self.edit(self.browser_path(), lambda value: value["thinking_profile"].update(profile_id="wrong-profile"))
        with self.assertRaisesRegex(policy.PolicyError, "chat_task_profile_not_confirmed"):
            self.register()

    def test_request_and_browser_task_identity_must_match(self):
        for filename, error in ((self.request_path(), "chat_task_identity_mismatch"),
                                (self.browser_path(), "chat_task_browser_identity_mismatch")):
            with self.subTest(file=filename.name):
                self.prepare(self.request_id)
                self.edit(filename, lambda value: (value.get("config") or value).update(task_id="wc_chat_wrong"))
                with self.assertRaisesRegex(policy.PolicyError, error):
                    self.register()

    def test_cancel_prevents_registration_and_existing_resolution(self):
        self.register()
        policy.write_json(self.root / "tasks" / self.request_id / "cancel.json", {"cancel_requested": True})
        for operation in (self.register, self.resolve):
            with self.assertRaisesRegex(policy.PolicyError, "chat_task_cancelled"):
                operation()

    def test_binding_tampering_is_rejected(self):
        self.register()
        self.edit(self.binding_path(), lambda value: value.update(request_sha256="0" * 64))
        with self.assertRaisesRegex(policy.PolicyError, "goal_binding_evidence_changed"):
            self.resolve()

    def test_nonfinite_profile_is_rejected_without_binding(self):
        request = policy.read_json(self.request_path())
        request["config"]["thinking_profile"]["invalid"] = float("nan")
        self.request_path().write_text(json.dumps(request), encoding="utf-8")
        with self.assertRaisesRegex(policy.PolicyError, "invalid_thinking_profile"):
            self.register()
        self.assertFalse(self.binding_path().exists())

    def test_profile_size_depth_and_node_count_are_bounded(self):
        deep = {"profile_id": "profile"}
        child = deep
        for _ in range(policy.MAX_PROFILE_DEPTH + 1):
            child["nested"] = {}
            child = child["nested"]
        profiles = [dict(self.profile, large="x" * policy.MAX_PROFILE_BYTES),
                    dict(self.profile, nodes=[None] * policy.MAX_PROFILE_NODES), deep]
        for profile in profiles:
            with self.subTest(profile_keys=list(profile)):
                self.prepare(self.request_id, profile)
                with self.assertRaisesRegex(policy.PolicyError, "thinking_profile_too_large"):
                    self.register()

    def test_profile_id_is_required(self):
        for profile in ({}, {"profile_id": ""}, {"profile_id": 1}, {"profile_id": "x" * 129}):
            self.prepare(self.request_id, profile)
            with self.assertRaisesRegex(policy.PolicyError, "invalid_thinking_profile_id"):
                self.register()

    def test_unsafe_project_goal_and_request_ids_are_rejected(self):
        for project, goal in (("../escape", "goal"), ("project", "goal/escape"),
                              ("project", "x" * 257), ("x" * 513, "goal")):
            with self.assertRaises(policy.PolicyError):
                policy.register_goal_binding(self.request_id, project, goal, runtime=self.root)
        with self.assertRaisesRegex(policy.PolicyError, "invalid_request_id"):
            policy.register_goal_binding("../escape", self.project, self.goal, runtime=self.root)

    def test_canonical_request_key_order_does_not_change_frozen_intent(self):
        self.register()
        request = policy.read_json(self.request_path())
        self.request_path().write_text(json.dumps(dict(reversed(list(request.items()))), indent=4), encoding="utf-8")
        self.assertEqual(self.resolve()["thinking_profile"], self.profile)


if __name__ == "__main__":
    unittest.main()
