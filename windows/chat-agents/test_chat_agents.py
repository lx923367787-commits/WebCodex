"""Service regression tests; no live browser, network or child processes."""
from __future__ import annotations

import contextlib
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, patch
import uuid

import chat_agents as chat

PROFILE = {"profile_id": "fixture-standard", "model_label": "Fixture model", "thinking_label": "Standard"}
OTHER_PROFILE = {"profile_id": "fixture-extended", "model_label": "Fixture model", "thinking_label": "Extended"}


class ControlledThread:
    def __init__(self, *, target, args, name, daemon, start_error=None):
        self.target, self.args, self.name, self.daemon = target, args, name, daemon
        self.start_error = start_error
        self.alive = False
        self.join_calls = []
        self.finish_on_join = False

    def start(self):
        if self.start_error:
            raise self.start_error
        self.alive = True

    def is_alive(self):
        return self.alive

    def run(self):
        try:
            self.target(*self.args)
        finally:
            self.alive = False

    def join(self, timeout):
        self.join_calls.append(timeout)
        if self.finish_on_join:
            self.run()


class ChatAgentServiceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="webcodex-chat-agents-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.scheduler = self.root / "scheduler"
        self.threads = []
        self.next_start_error = None
        self.thread_factory = Mock(side_effect=self.new_thread)
        self.stamps = {12345: "process-instance-A"}
        self.service = chat.Service(self.root, thread_factory=self.thread_factory,
                                    stamp=lambda pid: self.stamps.get(pid), transport_runtime_root=self.scheduler)
        self.lock_patch = patch.object(chat, "watcher_lock", side_effect=lambda path: contextlib.nullcontext())
        self.lock_patch.start()
        self.addCleanup(self.lock_patch.stop)
        self.transport = types.ModuleType("chat_transport")
        self.transport.send = Mock()
        self.transport.observe = Mock()
        self.transport.discover_profiles = Mock(return_value={"profiles": [PROFILE, OTHER_PROFILE]})
        self.module_patch = patch.dict(sys.modules, {"chat_transport": self.transport})
        self.module_patch.start()
        self.addCleanup(self.module_patch.stop)
        self.save_profiles()

    def save_profiles(self, *, profiles=None, observed_at=None, state="ready"):
        chat.write_json(self.root / "thinking-profiles.json", {
            "state": state, "observed_at": chat.time.time() if observed_at is None else observed_at,
            "profiles": [PROFILE, OTHER_PROFILE] if profiles is None else profiles})

    def new_thread(self, **arguments):
        thread = ControlledThread(**arguments, start_error=self.next_start_error)
        self.threads.append(thread)
        return thread

    def spawn(self, task="Read the authorized input.", parent="parent-1", key="request-key-1", profile=PROFILE["profile_id"]):
        return self.service.spawn_chat_task(task, parent, key, profile)

    def job(self, request_id):
        return self.service.latest_job(self.service.directory(request_id))

    def finish_send(self, request_id, **changes):
        directory = self.service.directory(request_id)
        config = self.service.prepared(request_id)["config"]
        receipt = {key: config[key] for key in ("launch_id", "task_id", "parent_session_id")}
        receipt.update(status="sent", target_url="https://chatgpt.com/c/12345678-abcd",
                       business_verified=False, response_observed=False)
        receipt.update(changes)
        chat.write_json(self.root / "evidence" / request_id / "result.json", receipt)
        chat.write_json(directory / "outcomes" / (self.job(request_id)["job_id"] + ".json"),
                        {"state": "finished", "result": receipt})
        self.threads[-1].alive = False
        return receipt

    def test_same_intent_replays_without_second_thread(self):
        first = self.spawn()
        second = self.spawn()
        self.assertEqual(first, second)
        self.assertEqual(first["state"], "running")
        self.assertFalse(first["business_verified"])
        self.thread_factory.assert_called_once()

    def test_same_key_changed_task_conflicts_without_writing(self):
        first = self.spawn()
        before = self.service.prepared(first["request_id"])
        with self.assertRaisesRegex(ValueError, "idempotency_key_conflict"):
            self.spawn(task="A different task.")
        self.assertEqual(self.service.prepared(first["request_id"]), before)
        self.thread_factory.assert_called_once()

    def test_other_parent_has_distinct_identity_after_worker_finishes(self):
        first = self.spawn()
        self.finish_send(first["request_id"])
        second = self.spawn(parent="parent-2")
        self.assertNotEqual(first["request_id"], second["request_id"])
        self.assertEqual(self.thread_factory.call_count, 2)

    def test_unknown_thread_start_outcome_is_never_retried_by_spawn(self):
        self.next_start_error = RuntimeError("offline simulated thread start error")
        first = self.spawn()
        self.assertEqual(first["state"], "unknown")
        second = self.spawn()
        self.assertEqual(first, second)
        self.thread_factory.assert_called_once()

    def test_incomplete_request_does_not_relaunch(self):
        first = self.spawn()
        directory = self.service.directory(first["request_id"])
        (directory / "current-job.json").unlink()
        self.assertEqual(self.spawn()["state"], "not_started")
        self.thread_factory.assert_called_once()

    def test_busy_spawn_creates_no_task_or_job_record(self):
        self.spawn()
        before = sorted(str(path.relative_to(self.root)) for path in self.root.rglob("*"))
        result = self.spawn(task="Another task.", key="second-key")
        after = sorted(str(path.relative_to(self.root)) for path in self.root.rglob("*"))
        self.assertEqual(result, {"state": "busy", "launched": False, "retry_same_arguments": True})
        self.assertEqual(before, after)
        self.thread_factory.assert_called_once()

    def test_restarted_service_reports_unfinished_job_unknown_without_resend(self):
        first = self.spawn()
        replacement = chat.Service(self.root, thread_factory=self.thread_factory, transport_runtime_root=self.scheduler)
        self.assertNotEqual(replacement.owner_instance, self.service.owner_instance)
        result = replacement.read_chat_task(first["request_id"])
        self.assertFalse(result["worker_alive"])
        self.assertEqual(result["state"], "unknown")
        replacement.spawn_chat_task("Read the authorized input.", "parent-1", "request-key-1", PROFILE["profile_id"])
        self.thread_factory.assert_called_once()

    def test_worker_runs_in_provider_with_only_persisted_identity_arguments(self):
        first = self.spawn()
        _, arguments = self.thread_factory.call_args
        job = self.job(first["request_id"])
        self.assertIs(arguments["target"].__self__, self.service)
        self.assertEqual(arguments["args"], (first["request_id"], "send", job["job_id"]))
        self.assertTrue(arguments["daemon"])
        self.assertEqual(job["owner_instance"], self.service.owner_instance)
        self.assertNotIn("worker_pid", job)
        self.assertNotIn("process_stamp", job)
        self.assertNotIn("env", arguments)

    def test_sequential_jobs_keep_service_and_transport_alive_without_permanent_busy(self):
        browser = types.ModuleType("playwright_extension_browser")
        browser.close_persistent_connection = Mock()
        self.transport.send.return_value = {"status": "sent", "connection_id": "connection-reused"}
        with patch.dict(sys.modules, {"playwright_extension_browser": browser}):
            first = self.spawn()
            self.threads[-1].run()
            self.assertFalse(self.service.browser_worker_busy())
            second = self.spawn(key="second-same-process-job")
            self.threads[-1].run()
            self.assertFalse(self.service.browser_worker_busy())
            self.assertEqual(self.transport.send.call_count, 2)
            for request in (first, second):
                stored = self.service.read_chat_task(request["request_id"])
                self.assertEqual(stored["connection_id"], "connection-reused")
                self.assertEqual(self.job(request["request_id"])["owner_instance"], self.service.owner_instance)
            browser.close_persistent_connection.assert_not_called()
            self.assertTrue(self.service.close())
            browser.close_persistent_connection.assert_called_once()

    def test_terminal_outcome_wins_over_live_thread_snapshot(self):
        first = self.spawn()
        self.finish_send(first["request_id"])
        self.threads[-1].alive = True  # Thread has persisted its result, but is returning.
        result = self.service.read_chat_task(first["request_id"])
        self.assertFalse(result["worker_alive"])
        self.assertEqual(result["state"], "finished")
        replacement = chat.Service(self.root, thread_factory=self.thread_factory, transport_runtime_root=self.scheduler)
        self.assertEqual(replacement.read_chat_task(first["request_id"]), result)

    def test_legacy_process_records_are_observed_without_migration_or_resend(self):
        first = self.spawn()
        request_id = first["request_id"]
        directory = self.service.directory(request_id)
        legacy = self.job(request_id)
        legacy.pop("owner_instance")
        legacy.update(worker_pid=12345, process_stamp="process-instance-A")
        chat.write_json(directory / "current-job.json", legacy)
        original = (directory / "current-job.json").read_bytes()
        self.threads[-1].alive = False
        self.assertTrue(self.service.read_chat_task(request_id)["worker_alive"])
        self.assertTrue(self.service.browser_worker_busy())
        self.assertEqual(self.spawn(key="blocked-by-legacy")["state"], "busy")
        self.stamps[12345] = "reused-pid-new-process"
        self.assertEqual(self.service.read_chat_task(request_id)["state"], "unknown")
        self.assertFalse(self.service.browser_worker_busy())
        self.spawn()
        self.assertEqual((directory / "current-job.json").read_bytes(), original)
        self.thread_factory.assert_called_once()

    def test_close_stops_callback_and_does_not_close_pool_under_live_worker(self):
        first = self.spawn()
        browser = types.ModuleType("playwright_extension_browser")
        browser.close_persistent_connection = Mock()
        with patch.dict(sys.modules, {"playwright_extension_browser": browser}):
            self.assertFalse(self.service.close(timeout=0.01))
            self.assertEqual(self.threads[-1].join_calls, [0.01])
            browser.close_persistent_connection.assert_not_called()
            self.threads[-1].run()
            self.transport.send.assert_not_called()
            result = self.service.read_chat_task(first["request_id"])
            self.assertEqual(result["reason"], "cancelled_before_worker")
            self.assertTrue(self.service.close())
            browser.close_persistent_connection.assert_called_once()

    def test_close_does_not_import_or_connect_unused_browser(self):
        with patch.dict(sys.modules, {"playwright_extension_browser": None}):
            self.assertTrue(self.service.close())
        self.transport.send.assert_not_called()
        self.transport.observe.assert_not_called()
        self.thread_factory.assert_not_called()

    def test_stopping_blocks_new_task_without_durable_request(self):
        self.service._stopping.set()
        result = self.spawn()
        self.assertEqual(result, {"state": "blocked", "reason": "service_stopping", "launched": False})
        self.assertFalse((self.root / "tasks").exists())
        self.thread_factory.assert_not_called()

    def test_stopping_blocks_new_observation_without_job(self):
        first = self.spawn()
        request_id = first["request_id"]
        self.finish_send(request_id)
        before = self.job(request_id)
        self.service._stopping.set()
        result = self.service.collect_chat_task(request_id)
        self.assertEqual(result["collect_reason"], "service_stopping")
        self.assertEqual(self.job(request_id), before)
        self.thread_factory.assert_called_once()

    def test_running_transport_callback_observes_service_shutdown(self):
        first = self.spawn()

        def send(_config, _evidence, cancelled, runtime_root):
            self.assertEqual(runtime_root, self.scheduler)
            self.assertFalse(cancelled())
            self.service._stopping.set()
            self.assertTrue(cancelled())
            return {"status": "unknown", "reason": "stopped_during_send"}

        self.transport.send.side_effect = send
        self.threads[-1].run()
        self.assertFalse(self.threads[-1].is_alive())
        result = self.service.read_chat_task(first["request_id"])
        self.assertEqual(result["reason"], "stopped_during_send")

    def test_connection_id_projection_prefers_result_and_bounds_browser_fallback(self):
        first = self.spawn()
        request_id = first["request_id"]
        config = self.service.prepared(request_id)["config"]
        browser = {key: config[key] for key in ("launch_id", "task_id")}
        browser.update(connection_id="initial-connection", private_field="DO_NOT_EXPOSE")
        browser_path = self.root / "evidence" / request_id / "browser.json"
        chat.write_json(browser_path, browser)
        self.assertEqual(self.service.read_chat_task(request_id)["connection_id"], "initial-connection")
        self.transport.send.return_value = {"status": "sent", "connection_id": "latest-connection"}
        self.threads[-1].run()
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["connection_id"], "latest-connection")
        self.assertNotIn("DO_NOT_EXPOSE", json.dumps(result))
        outcome = self.service.directory(request_id) / "outcomes" / (self.job(request_id)["job_id"] + ".json")
        chat.write_json(outcome, {"state": "finished", "result": {"connection_id": "https://invalid/private"}})
        self.assertEqual(self.service.read_chat_task(request_id)["connection_id"], "initial-connection")
        browser["task_id"] = "wrong-task"
        chat.write_json(browser_path, browser)
        self.assertNotIn("connection_id", self.service.read_chat_task(request_id))

    def test_foreign_worker_owner_is_rejected_before_transport(self):
        first = self.spawn()
        request_id = first["request_id"]
        directory = self.service.directory(request_id)
        job = self.job(request_id)
        job["owner_instance"] = str(uuid.uuid4())
        chat.write_json(directory / "jobs" / (job["job_id"] + ".json"), job)
        self.threads[-1].run()
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["reason"], "ValueError")
        self.transport.send.assert_not_called()
        self.transport.observe.assert_not_called()

    def test_collect_without_sent_receipt_never_retries_send(self):
        first = self.spawn()
        self.threads[-1].alive = False
        result = self.service.collect_chat_task(first["request_id"])
        self.assertFalse(result["collect_started"])
        self.assertEqual(result["collect_reason"], "no_observed_chat_to_revalidate")
        self.thread_factory.assert_called_once()
        self.transport.send.assert_not_called()

    def test_unknown_with_exact_message_proof_starts_only_observe_and_preserves_receipt(self):
        first = self.spawn()
        request_id = first["request_id"]
        receipt = self.finish_send(request_id, status="unknown", message_observed=True,
                                   reason="conversation_not_bound")
        receipt_path = self.root / "evidence" / request_id / "result.json"
        original_bytes = receipt_path.read_bytes()
        observed = self.service.read_chat_task(request_id)
        self.assertIs(observed["message_observed"], True)
        self.assertEqual(observed["launch_status"], "unknown")
        self.service.collect_chat_task(request_id)
        job = self.job(request_id)
        self.assertEqual(job["operation"], "observe")
        self.assertEqual(self.threads[-1].args[1], "observe")
        self.transport.observe.return_value = {"response_observed": True,
                                               "response_text": "Revalidated candidate."}
        self.service.worker(request_id, "observe", job["job_id"])
        self.transport.send.assert_not_called()
        self.transport.observe.assert_called_once()
        self.assertEqual(self.transport.observe.call_args.args[1], receipt)
        self.assertEqual(receipt_path.read_bytes(), original_bytes)
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["launch_status"], "unknown")
        self.assertEqual(result["state"], "response_observed")
        self.assertFalse(result["business_verified"])
        self.assertEqual(self.thread_factory.call_count, 2)

    def test_unknown_without_exact_boolean_message_proof_cannot_collect(self):
        cases = [("missing", {}), ("false", {"message_observed": False}),
                 ("string", {"message_observed": "true"}),
                 ("number", {"message_observed": 1}),
                 ("null", {"message_observed": None})]
        for label, changes in cases:
            with self.subTest(proof=label):
                first = self.spawn(key="unknown-proof-" + label)
                request_id = first["request_id"]
                self.finish_send(request_id, status="unknown", **changes)
                before_job = self.job(request_id)
                before_count = self.thread_factory.call_count
                current = self.service.read_chat_task(request_id)
                self.assertIs(current["message_observed"], False)
                result = self.service.collect_chat_task(request_id)
                self.assertFalse(result["collect_started"])
                self.assertEqual(result["collect_reason"], "no_observed_chat_to_revalidate")
                self.assertEqual(self.job(request_id), before_job)
                self.assertEqual(self.thread_factory.call_count, before_count)
        self.transport.send.assert_not_called()
        self.transport.observe.assert_not_called()

    def test_unknown_message_proof_without_recorded_url_cannot_collect(self):
        first = self.spawn()
        request_id = first["request_id"]
        self.finish_send(request_id, status="unknown", message_observed=True, target_url=None)
        before = self.job(request_id)
        result = self.service.collect_chat_task(request_id)
        self.assertFalse(result["collect_started"])
        self.assertEqual(result["collect_reason"], "no_observed_chat_to_revalidate")
        self.assertEqual(self.job(request_id), before)
        self.thread_factory.assert_called_once()

    def test_late_collect_starts_only_observation_even_after_send_expiry(self):
        first = self.spawn()
        request_id = first["request_id"]
        receipt = self.finish_send(request_id)
        directory = self.service.directory(request_id)
        prepared = self.service.prepared(request_id)
        prepared["config"]["expires_at"] = "2000-01-01T00:00:00+00:00"
        chat.write_json(directory / "request.json", prepared)
        self.service.collect_chat_task(request_id)
        job = self.job(request_id)
        self.assertEqual(job["operation"], "observe")
        self.assertEqual(self.threads[-1].args[1], "observe")
        observed = dict(receipt, response_observed=True, response_text="Candidate result.")
        self.transport.observe.return_value = observed
        self.service.worker(request_id, "observe", job["job_id"])
        self.transport.send.assert_not_called()
        self.transport.observe.assert_called_once()
        self.assertEqual(self.transport.observe.call_args.args[1], receipt)
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["state"], "response_observed")
        self.assertEqual(result["response_text"], "Candidate result.")
        self.assertFalse(result["business_verified"])
        self.assertEqual(self.thread_factory.call_count, 2)

    def test_collect_while_same_task_worker_active_is_noop(self):
        first = self.spawn()
        before = self.job(first["request_id"])
        self.assertEqual(self.service.collect_chat_task(first["request_id"]), first)
        self.assertEqual(self.job(first["request_id"]), before)
        self.thread_factory.assert_called_once()

    def test_collect_after_response_is_noop(self):
        first = self.spawn()
        self.finish_send(first["request_id"], response_observed=True, response_text="Candidate result.")
        result = self.service.collect_chat_task(first["request_id"])
        self.assertEqual(result["state"], "response_observed")
        self.thread_factory.assert_called_once()

    def test_collect_during_other_task_worker_does_not_create_job(self):
        first = self.spawn()
        request_id = first["request_id"]
        self.finish_send(request_id)
        self.spawn(task="Other authorized task.", key="second-task")
        before = self.job(request_id)
        result = self.service.collect_chat_task(request_id)
        self.assertFalse(result["collect_started"])
        self.assertEqual(result["collect_reason"], "browser_worker_busy")
        self.assertEqual(self.job(request_id), before)
        self.assertEqual(self.thread_factory.call_count, 2)

    def test_repeated_collect_obeys_cooldown_without_new_job(self):
        self.save_profiles(observed_at=1000)
        with patch.object(chat.time, "time", return_value=1000):
            first = self.spawn()
        request_id = first["request_id"]
        self.finish_send(request_id)
        with patch.object(chat.time, "time", return_value=1005):
            self.service.collect_chat_task(request_id)
        self.threads[-1].alive = False
        before = self.job(request_id)
        with patch.object(chat.time, "time", return_value=1006):
            result = self.service.collect_chat_task(request_id)
        self.assertFalse(result["collect_started"])
        self.assertEqual(result["collect_reason"], "observation_cooldown")
        self.assertEqual(self.job(request_id), before)
        self.assertEqual(self.thread_factory.call_count, 2)
        with patch.object(chat.time, "time", return_value=1021):
            self.service.collect_chat_task(request_id)
        self.assertEqual(self.job(request_id)["operation"], "observe")
        self.assertEqual(self.thread_factory.call_count, 3)

    def test_cancel_before_worker_blocks_send_and_is_recoverable(self):
        first = self.spawn()
        request_id = first["request_id"]
        cancelled = self.service.cancel_chat_task(request_id)
        self.assertTrue(cancelled["cancel_requested"])
        self.assertIn("already sent model turn may continue", cancelled["cancellation_scope"])
        self.service.worker(request_id, "send", self.job(request_id)["job_id"])
        self.transport.send.assert_not_called()
        self.transport.observe.assert_not_called()
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(result["reason"], "cancelled_before_worker")
        self.service.collect_chat_task(request_id)
        self.spawn()
        self.thread_factory.assert_called_once()

    def test_cancel_sent_task_does_not_claim_remote_model_stopped(self):
        first = self.spawn()
        receipt = self.finish_send(first["request_id"])
        result = self.service.cancel_chat_task(first["request_id"])
        self.assertEqual(result["launch_status"], "sent")
        self.assertEqual(result["target_url"], receipt["target_url"])
        self.assertTrue(result["cancel_requested"])
        self.assertIn("may continue", result["cancellation_scope"])
        self.service.collect_chat_task(first["request_id"])
        self.thread_factory.assert_called_once()

    def test_worker_early_return_is_recoverable_without_transport_receipt(self):
        first = self.spawn()
        request_id = first["request_id"]
        self.transport.send.return_value = {"status": "blocked", "reason": "scheduler_not_running", "launched": False}
        self.service.worker(request_id, "send", self.job(request_id)["job_id"])
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["state"], "finished")
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(result["reason"], "scheduler_not_running")
        self.assertFalse((self.root / "evidence").exists())
        self.thread_factory.assert_called_once()

    def test_worker_exception_is_safe_and_never_causes_resend(self):
        first = self.spawn()
        request_id = first["request_id"]
        self.transport.send.side_effect = RuntimeError("PRIVATE_TEST_SECRET")
        self.service.worker(request_id, "send", self.job(request_id)["job_id"])
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["reason"], "RuntimeError")
        self.assertNotIn("PRIVATE_TEST_SECRET", json.dumps(result))
        self.spawn()
        self.thread_factory.assert_called_once()

    def test_worker_operation_mismatch_cannot_send(self):
        first = self.spawn()
        request_id = first["request_id"]
        self.service.worker(request_id, "observe", self.job(request_id)["job_id"])
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["reason"], "ValueError")
        self.transport.send.assert_not_called()
        self.transport.observe.assert_not_called()

    def test_changed_prepared_request_is_rejected_before_transport(self):
        first = self.spawn()
        request_id = first["request_id"]
        directory = self.service.directory(request_id)
        prepared = self.service.prepared(request_id)
        prepared["config"]["prompt"] = "Changed after the worker was issued."
        chat.write_json(directory / "request.json", prepared)
        self.service.worker(request_id, "send", self.job(request_id)["job_id"])
        result = self.service.read_chat_task(request_id)
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["reason"], "ValueError")
        self.transport.send.assert_not_called()
        self.transport.observe.assert_not_called()

    def test_mismatched_receipt_is_rejected(self):
        first = self.spawn()
        self.finish_send(first["request_id"], task_id="wc_chat_" + uuid.uuid4().hex)
        with self.assertRaisesRegex(ValueError, "receipt_identity_mismatch"):
            self.service.read_chat_task(first["request_id"])

    def test_invalid_input_never_creates_task(self):
        cases = [("", "parent", "key"), ("x" * 12001, "parent", "key"),
                 ("task", "../parent", "key"), ("task", "parent", ""),
                 ("task", "parent", "x" * 129)]
        for task, parent, key in cases:
            with self.subTest(task_length=len(task), parent=parent, key_length=len(key)):
                with self.assertRaises(ValueError):
                    self.service.spawn_chat_task(task, parent, key, PROFILE["profile_id"])
        self.assertFalse((self.root / "tasks").exists())
        self.thread_factory.assert_not_called()

    def test_selected_snapshot_is_fingerprinted_and_requested_applied_are_distinct(self):
        first = self.spawn()
        prepared = self.service.prepared(first["request_id"])
        self.assertEqual(prepared["config"]["thinking_profile"], PROFILE)
        self.assertEqual(first["requested_thinking_profile"], PROFILE)
        self.assertNotIn("applied_thinking_profile", first)
        intent = {"task": "Read the authorized input.", "parent_task_id": "parent-1",
                  "idempotency_key": "request-key-1", "thinking_profile": PROFILE}
        self.assertEqual(prepared["intent_sha256"], chat.fingerprint(intent))
        browser = {key: prepared["config"][key] for key in ("launch_id", "task_id")}
        browser["thinking_profile"] = PROFILE
        path = self.root / "evidence" / first["request_id"] / "browser.json"
        chat.write_json(path, browser)
        self.assertEqual(self.service.read_chat_task(first["request_id"])["applied_thinking_profile"], PROFILE)
        browser["task_id"] = "wrong-child"
        chat.write_json(path, browser)
        self.assertNotIn("applied_thinking_profile", self.service.read_chat_task(first["request_id"]))

    def test_same_key_different_profile_conflicts_without_new_worker(self):
        first = self.spawn()
        before = self.service.prepared(first["request_id"])
        with self.assertRaisesRegex(ValueError, "idempotency_key_conflict"):
            self.spawn(profile=OTHER_PROFILE["profile_id"])
        self.assertEqual(self.service.prepared(first["request_id"]), before)
        self.thread_factory.assert_called_once()

    def test_existing_request_retains_snapshot_after_catalog_refresh_or_expiry(self):
        first = self.spawn()
        self.finish_send(first["request_id"])
        changed = dict(PROFILE, thinking_label="New catalog label")
        self.save_profiles(profiles=[changed], observed_at=1)
        repeated = self.spawn()
        self.assertEqual(repeated["requested_thinking_profile"], PROFILE)
        self.thread_factory.assert_called_once()
        refused = self.spawn(key="new-request")
        self.assertEqual(refused["state"], "profile_unavailable")
        self.assertEqual(refused["reason"], "thinking_profiles_expired_refresh_required")

    def test_missing_stale_unready_ambiguous_and_unknown_profiles_cannot_spawn(self):
        path = self.root / "thinking-profiles.json"
        path.unlink()
        self.assertEqual(self.spawn()["reason"], "discover_thinking_profiles_first")
        cases = [({"state": "failed"}, "discover_thinking_profiles_first"),
                 ({"observed_at": 1}, "thinking_profiles_expired_refresh_required"),
                 ({"observed_at": chat.time.time() + 60}, "thinking_profiles_expired_refresh_required"),
                 ({"profiles": [OTHER_PROFILE]}, "thinking_profile_not_discovered")]
        for arguments, reason in cases:
            with self.subTest(arguments=arguments):
                self.save_profiles(**arguments)
                self.assertEqual(self.spawn(), {"state": "profile_unavailable", "reason": reason,
                    "request_created": False, "launched": False, "thinking_profile_id": PROFILE["profile_id"]})
        self.save_profiles(profiles=[PROFILE, PROFILE])
        with self.assertRaisesRegex(ValueError, "duplicate_thinking_profile"):
            self.spawn()
        self.assertFalse((self.root / "tasks").exists())
        self.thread_factory.assert_not_called()

    def test_profile_expires_after_selection_then_refresh_preserves_key_and_single_request(self):
        selected = self.service.selected_profile(PROFILE["profile_id"])
        self.save_profiles(observed_at=1)
        refused = self.spawn(profile=selected["profile_id"])
        self.assertEqual(refused["state"], "profile_unavailable")
        self.assertFalse((self.root / "tasks").exists())
        self.thread_factory.assert_not_called()
        self.save_profiles()
        first = self.spawn(profile=selected["profile_id"])
        self.save_profiles(observed_at=1)
        self.assertEqual(self.spawn(profile=selected["profile_id"])["request_id"], first["request_id"])
        self.assertEqual(len(list((self.root / "tasks").glob("*/request.json"))), 1)
        self.thread_factory.assert_called_once()

    def test_arbitrary_selection_failure_is_not_proof_of_recoverable_profile_refusal(self):
        with patch.object(self.service, "selected_profile", side_effect=ValueError("unknown failure")):
            with self.assertRaisesRegex(ValueError, "unknown failure"):
                self.spawn()
        self.assertFalse((self.root / "tasks").exists())
        self.thread_factory.assert_not_called()

    def test_first_discovery_is_async_single_worker_and_blocks_spawn(self):
        (self.root / "thinking-profiles.json").unlink()
        first = self.service.discover_chat_profiles()
        self.assertEqual(first["state"], "discovering")
        self.assertEqual(self.service.discover_chat_profiles(), first)
        self.assertEqual(self.service.discover_chat_profiles(refresh=True), first)
        self.assertEqual(self.spawn()["state"], "busy")
        self.assertFalse((self.root / "tasks").exists())
        self.transport.discover_profiles.assert_not_called()
        self.threads[-1].run()
        ready = self.service.discover_chat_profiles()
        self.assertEqual(ready["state"], "ready")
        self.assertFalse(ready["refresh_required"])
        self.assertEqual(ready["profiles"], [PROFILE, OTHER_PROFILE])
        self.transport.discover_profiles.assert_called_once()
        self.assertFalse(self.transport.discover_profiles.call_args.kwargs["cancelled"]())
        self.thread_factory.assert_called_once()
        self.assertFalse(self.service.browser_worker_busy())
        self.assertEqual(self.spawn()["requested_thinking_profile"], PROFILE)

    def test_cached_discovery_is_read_only_until_explicit_refresh(self):
        cached = self.service.discover_chat_profiles()
        self.assertEqual(cached["state"], "ready")
        self.thread_factory.assert_not_called()
        self.transport.discover_profiles.assert_not_called()
        self.service.discover_chat_profiles(refresh=True)
        self.thread_factory.assert_called_once()
        self.transport.discover_profiles.return_value = {"profiles": [OTHER_PROFILE], "private": "NOT_CACHED"}
        self.threads[-1].run()
        result = self.service.discover_chat_profiles()
        self.assertEqual(result["profiles"], [OTHER_PROFILE])
        self.assertNotIn("NOT_CACHED", (self.root / "thinking-profiles.json").read_text())

    def test_profiles_read_never_discovers_missing_catalog(self):
        path = self.root / "thinking-profiles.json"
        path.unlink()
        self.assertEqual(self.service.read_chat_profiles(),
                         {"state": "not_discovered", "selection_required": True, "profiles": []})
        self.assertFalse(path.exists())
        self.thread_factory.assert_not_called()
        self.transport.discover_profiles.assert_not_called()

    def test_profiles_read_reports_ready_and_stale_cache_without_refresh(self):
        path = self.root / "thinking-profiles.json"
        for observed_at, refresh_required in ((chat.time.time(), False), (1, True)):
            with self.subTest(refresh_required=refresh_required):
                self.save_profiles(observed_at=observed_at)
                original = path.read_bytes()
                result = self.service.read_chat_profiles()
                self.assertEqual(result["state"], "ready")
                self.assertEqual(result["profiles"], [PROFILE, OTHER_PROFILE])
                self.assertEqual(result["refresh_required"], refresh_required)
                self.assertTrue(result["selection_required"])
                self.assertEqual(path.read_bytes(), original)
        self.thread_factory.assert_not_called()
        self.transport.discover_profiles.assert_not_called()

    def test_refresh_cannot_displace_a_task_worker_or_change_its_cache(self):
        self.spawn()
        path = self.root / "thinking-profiles.json"
        previous = path.read_bytes()
        result = self.service.discover_chat_profiles(refresh=True)
        self.assertFalse(result["discovery_started"])
        self.assertEqual(result["reason"], "browser_worker_busy")
        self.assertEqual(path.read_bytes(), previous)
        self.thread_factory.assert_called_once()

    def test_interrupted_discovery_requires_explicit_refresh_and_failure_is_sanitized(self):
        self.service.discover_chat_profiles(refresh=True)
        replacement = chat.Service(self.root, thread_factory=self.thread_factory, transport_runtime_root=self.scheduler)
        self.assertEqual(replacement.discover_chat_profiles()["state"], "unknown")
        self.thread_factory.assert_called_once()
        self.threads[-1].alive = False
        replacement.discover_chat_profiles(refresh=True)
        self.transport.discover_profiles.side_effect = RuntimeError("PRIVATE_ACCOUNT_ERROR")
        self.threads[-1].run()
        result = replacement.discover_chat_profiles()
        self.assertEqual(result["state"], "failed")
        self.assertEqual(result["reason"], "discovery_failed")
        self.assertNotIn("PRIVATE_ACCOUNT_ERROR", json.dumps(result))

    def test_discovery_shutdown_never_calls_transport(self):
        self.service.discover_chat_profiles(refresh=True)
        self.service._stopping.set()
        self.threads[-1].run()
        self.transport.discover_profiles.assert_not_called()
        self.assertEqual(self.service.discover_chat_profiles()["state"], "failed")

    def test_binding_calls_only_local_policy_and_old_requests_remain_readable(self):
        first = self.spawn()
        policy = types.ModuleType("chat_task_policy")
        policy.register_goal_binding = Mock(return_value={"state": "pending", "server_verified": False})
        with patch.dict(sys.modules, {"chat_task_policy": policy}):
            self.assertEqual(self.service.bind_chat_task_goal(first["request_id"], "project-1", "goal-1")["state"], "pending")
            policy.register_goal_binding.assert_called_once_with(first["request_id"], "project-1", "goal-1", runtime=self.root)
            prepared = self.service.prepared(first["request_id"])
            prepared["config"].pop("thinking_profile")
            chat.write_json(self.service.directory(first["request_id"]) / "request.json", prepared)
            self.assertNotIn("requested_thinking_profile", self.service.read_chat_task(first["request_id"]))
            with self.assertRaisesRegex(ValueError, "legacy_request_has_no_thinking_profile"):
                self.service.bind_chat_task_goal(first["request_id"], "project-1", "goal-1")
            policy.register_goal_binding.assert_called_once()

    def test_cli_discovery_delegates_without_using_browser_worker(self):
        module = types.ModuleType("cli_options")
        module.discover_cli_options = Mock(return_value={"cli_name": "gemini", "installed": False})
        with patch.dict(sys.modules, {"cli_options": module}):
            self.assertFalse(self.service.discover_cli_options("gemini")["installed"])
        module.discover_cli_options.assert_called_once_with("gemini")
        self.thread_factory.assert_not_called()

    def test_child_prompt_names_own_goal_registration_and_explicit_cli_choices(self):
        first = self.spawn()
        prompt = self.service.prepared(first["request_id"])["config"]["prompt"]
        for expected in ("bind_chat_task_goal", "不能复用父Goal", "600K", "800K", "discover_cli_options", "原生model/effort参数", PROFILE["profile_id"], first["request_id"]):
            self.assertIn(expected, prompt)

    def test_profile_refresh_requires_real_boolean(self):
        for value in (0, 1, "false", None):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "invalid_refresh"):
                self.service.discover_chat_profiles(value)
        self.thread_factory.assert_not_called()


    def test_enrollment_grant_cannot_enter_ordinary_spawn_or_native_fields(self):
        from native_server import SPAWN_PROPERTIES
        grant = "wc_en_" + "a" * 64
        with self.assertRaisesRegex(ValueError, "launch_claim_must_not_be_persisted"):
            self.spawn(task="Ordinary task must not carry " + grant)
        self.assertFalse((self.root / "tasks").exists())
        self.thread_factory.assert_not_called()
        self.assertTrue({"enrollment_delivery", "claim_grant", "runtime_root"}.isdisjoint(SPAWN_PROPERTIES))

    def test_worker_omits_grant_candidate_before_outcome_and_parent_read(self):
        grant = "wc_en_" + "a" * 64
        first = self.spawn()
        self.transport.send.return_value = {"status": "sent", "response_observed": True,
            "response_text": "omit this whole candidate " + grant}
        self.threads[-1].run()
        result = self.service.read_chat_task(first["request_id"])
        self.assertNotIn("response_text", result)
        self.assertFalse(result["response_observed"])
        self.assertTrue(result["response_omitted"])
        self.assertEqual(result["response_reason"], "sensitive_candidate_omitted")
        for path in self.root.rglob("*.json"):
            self.assertNotIn(grant, path.read_text(encoding="utf-8"))
            self.assertNotIn("omit this whole candidate", path.read_text(encoding="utf-8"))

    def test_profile_early_refusals_cannot_echo_a_grant_as_profile_id(self):
        for state, observed_at in (("not_discovered", 1), ("ready", 1), ("ready", None)):
            self.save_profiles(state=state, observed_at=observed_at)
            for grant in ("wc_en_" + "a" * 64, "wc_hc_" + "b" * 64):
                with self.subTest(state=state, expired=observed_at == 1, kind=grant[:6]):
                    with self.assertRaisesRegex(ValueError, "launch_claim_must_not_be_persisted"):
                        self.spawn(profile=grant)
                    self.assertFalse((self.root / "tasks").exists())
        self.thread_factory.assert_not_called()

    def test_parent_read_filters_a_legacy_secret_candidate_without_rewriting_evidence(self):
        grant = "wc_en_" + "b" * 64
        first = self.spawn()
        path = self.service.directory(first["request_id"]) / "outcomes" / (self.job(first["request_id"])["job_id"] + ".json")
        path.parent.mkdir(parents=True, exist_ok=True)
        # Deliberately seed a historical unsafe record outside the guarded writer.
        original = json.dumps({"state": "finished", "result": {"response_observed": True,
            "response_text": "legacy whole answer " + grant}})
        path.write_text(original, encoding="utf-8")
        result = self.service.read_chat_task(first["request_id"])
        self.assertTrue(result["response_omitted"])
        self.assertFalse(result["response_observed"])
        self.assertNotIn(grant, json.dumps(result))
        self.assertNotIn("response_text", result)
        self.assertEqual(path.read_text(encoding="utf-8"), original)

    def test_worker_rejects_secret_outside_candidate_without_error_echo(self):
        grant = "wc_en_" + "a" * 64
        first = self.spawn()
        self.transport.send.return_value = {"status": "sent", "reason": grant}
        self.threads[-1].run()
        result = self.service.read_chat_task(first["request_id"])
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["reason"], "ValueError")
        self.assertNotIn(grant, json.dumps(result))
        for path in self.root.rglob("*.json"):
            self.assertNotIn(grant, path.read_text(encoding="utf-8"))

    def test_goal_binding_labels_cannot_persist_or_return_a_grant(self):
        first = self.spawn()
        policy = types.ModuleType("chat_task_policy")
        policy.register_goal_binding = Mock(return_value={"state": "pending"})
        grant = "wc_en_" + "a" * 64
        with patch.dict(sys.modules, {"chat_task_policy": policy}):
            for project, goal in ((grant, "goal-1"), ("project-1", grant)):
                with self.assertRaisesRegex(ValueError, "launch_claim_must_not_be_persisted"):
                    self.service.bind_chat_task_goal(first["request_id"], project, goal)
            policy.register_goal_binding.assert_not_called()
            policy.register_goal_binding.return_value = {"state": "pending", "goal_id": grant}
            with self.assertRaisesRegex(ValueError, "launch_claim_must_not_be_persisted"):
                self.service.bind_chat_task_goal(first["request_id"], "project-1", "goal-1")

    def enrollment_binding(self):
        return {"queue_kind": "member", "queue_id": "queue-1",
            "enrollment_id": "wc_enrollment_" + "a" * 32, "agent_id": "wc_iagent_" + "b" * 32,
            "task_id": "wc_agent_task_" + "c" * 32, "root_task_id": "parent-1",
            "manager_agent_id": "wc_iagent_" + "d" * 32, "project": "agent:runner:project",
            "thinking_profile_id": PROFILE["profile_id"]}

    def delivery(self, enrollment_id=None):
        return chat.EnrollmentDelivery(enrollment_id or self.enrollment_binding()["enrollment_id"],
            "wc_en_" + "e" * 64, int(chat.time.time() * 1000) + 60000)

    def enrolled(self, *, provider=None, binding=None, key="request-key-1", service=None):
        return (service or self.service).spawn_enrolled_chat_task("Read the authorized input.",
            "parent-1", key, PROFILE["profile_id"],
            enrollment_binding=self.enrollment_binding() if binding is None else binding,
            enrollment_delivery_provider=provider if provider is not None else Mock(return_value=self.delivery()))

    def test_enrolled_request_freezes_binding_without_claiming_or_persisting_delivery(self):
        provider = Mock(return_value=self.delivery())
        binding = self.enrollment_binding()
        frozen = dict(binding)
        first = self.enrolled(provider=provider, binding=binding)
        provider.assert_not_called()
        self.assertEqual(first["request_id"], str(uuid.uuid5(chat.NAMESPACE, "parent-1\0request-key-1")))
        self.assertEqual(first["enrollment_binding"], frozen)
        prepared = self.service.prepared(first["request_id"])
        self.assertEqual(prepared["enrollment_binding"], frozen)
        self.assertNotIn("enrollment_binding", prepared["config"])
        self.assertNotIn("enrollment_delivery_provider", json.dumps(prepared))
        self.assertNotIn("wc_en_" + "e" * 64, json.dumps(prepared))
        binding["project"] = "changed-by-caller"
        first["enrollment_binding"]["agent_id"] = "changed-output"
        self.assertEqual(self.service.read_chat_task(first["request_id"])["enrollment_binding"], frozen)

    def test_enrolled_unissued_preflight_retry_preserves_failure_and_blocks_evidence(self):
        first = self.enrolled()
        request = first["request_id"]
        directory = self.service.directory(request)
        config = self.service.prepared(request)["config"]
        result = {key: config[key] for key in ("launch_id", "task_id", "parent_session_id")}
        result.update(status="blocked", failure_stage="preflight", send_boundary_reached=False)
        old = directory / "outcomes" / (self.job(request)["job_id"] + ".json")
        chat.write_json(old, {"state":"finished", "result":result})
        self.threads[-1].alive = False
        before = old.read_bytes()
        retry = self.enrolled()
        self.assertEqual(retry["request_id"], request)
        self.assertEqual(len(self.threads), 2)
        self.assertEqual(old.read_bytes(), before)
        chat.write_json(directory / "outcomes" / (self.job(request)["job_id"] + ".json"),
                        {"state":"finished", "result":result})
        self.threads[-1].alive = False
        (self.root / "evidence" / request).mkdir(parents=True)
        self.enrolled()
        self.assertEqual(len(self.threads), 2)

    def test_real_preflight_result_allows_bounded_retry_without_browser_or_delivery(self):
        # Exercise the actual transport refusal, not a hand-written outcome.
        pilot_path = str(Path(__file__).resolve().parent.parent / "subagent-pilot")
        with patch.object(sys, "path", [pilot_path, *sys.path]):
            import pilot_transport as pilot
        provider = Mock(return_value=self.delivery())
        session = Mock(side_effect=AssertionError("preflight must not open a browser"))

        def send(config, evidence_root, cancelled, **options):
            return pilot.run(config, evidence_root, external_stop=cancelled,
                session_factory=session, state_reader=lambda: {"mode": "paused", "generation": 1},
                lock_factory=lambda _: contextlib.nullcontext(),
                enrollment_delivery_provider=options["enrollment_delivery_provider"])

        self.transport.send.side_effect = send
        first = self.enrolled(provider=provider)
        self.threads[-1].run()
        request = first["request_id"]
        directory = self.service.directory(request)
        original = directory / "outcomes" / (self.job(request)["job_id"] + ".json")
        before = original.read_bytes()
        retry = self.enrolled(provider=provider)
        self.assertEqual(retry["request_id"], request)
        self.assertEqual(len(self.threads), 2)
        self.assertEqual(original.read_bytes(), before)
        self.assertFalse((self.root / "evidence" / request).exists())
        provider.assert_not_called()
        session.assert_not_called()

    def test_unissued_preflight_retry_renews_only_deadline_and_is_bounded(self):
        first = self.enrolled()
        request = first["request_id"]
        directory = self.service.directory(request)
        prepared = self.service.prepared(request)
        prepared["config"]["expires_at"] = "2000-01-01T00:00:00+00:00"
        chat.write_json(directory / "request.json", prepared)
        original = json.loads(json.dumps(prepared))
        for attempt in range(4):
            job = self.job(request)
            result = {key: prepared["config"][key]
                      for key in ("launch_id", "task_id", "parent_session_id")}
            result.update(status="blocked", failure_stage="preflight", send_boundary_reached=False)
            chat.write_json(directory / "outcomes" / (job["job_id"] + ".json"),
                            {"state": "finished", "result": result})
            self.threads[-1].alive = False
            self.enrolled()
            self.assertEqual(len(self.threads), min(attempt + 2, 4))
        current = self.service.prepared(request)
        self.assertNotEqual(current["config"]["expires_at"], original["config"]["expires_at"])
        current["config"]["expires_at"] = original["config"]["expires_at"]
        self.assertEqual(current, original)
        self.assertEqual(len(list((directory / "preflight-requests").glob("*.json"))), 3)
        self.assertEqual(len(list((directory / "outcomes").glob("*.json"))), 4)

    def test_unissued_preflight_retry_preserves_cancel(self):
        first = self.enrolled()
        request = first["request_id"]
        directory = self.service.directory(request)
        config = self.service.prepared(request)["config"]
        result = {key: config[key] for key in ("launch_id", "task_id", "parent_session_id")}
        result.update(status="blocked", failure_stage="preflight", send_boundary_reached=False)
        chat.write_json(directory / "outcomes" / (self.job(request)["job_id"] + ".json"),
                        {"state": "finished", "result": result})
        self.threads[-1].alive = False
        chat.write_json(directory / "cancel.json", {"requested": True})
        self.enrolled()
        self.assertEqual(len(self.threads), 1)
        self.assertFalse((directory / "preflight-requests").exists())

    def test_enrolled_replay_and_restart_never_call_a_new_provider(self):
        original = Mock(return_value=self.delivery())
        first = self.enrolled(provider=original)
        replacement = chat.Service(self.root, thread_factory=self.thread_factory,
                                   transport_runtime_root=self.scheduler)
        incoming = Mock(return_value=self.delivery())
        self.save_profiles(observed_at=0)
        observed = self.enrolled(service=replacement, provider=incoming)
        self.assertEqual(observed["request_id"], first["request_id"])
        self.assertEqual(observed["state"], "unknown")
        self.assertEqual(observed["enrollment_binding"], self.enrollment_binding())
        original.assert_not_called()
        incoming.assert_not_called()
        self.thread_factory.assert_called_once()

    def test_ordinary_spawn_cannot_replay_or_replace_an_enrolled_request(self):
        first = self.enrolled()
        prepared = self.service.prepared(first["request_id"])
        with self.assertRaisesRegex(ValueError, "idempotency_key_conflict"):
            self.spawn()
        self.assertEqual(self.service.prepared(first["request_id"]), prepared)
        self.thread_factory.assert_called_once()

    def test_enrolled_spawn_cannot_reinterpret_an_ordinary_request(self):
        first = self.spawn()
        provider = Mock()
        with self.assertRaisesRegex(ValueError, "idempotency_key_conflict"):
            self.enrolled(provider=provider)
        self.assertNotIn("enrollment_binding", self.service.prepared(first["request_id"]))
        provider.assert_not_called()
        self.thread_factory.assert_called_once()

    def test_enrollment_binding_changes_or_extra_authority_cannot_replay(self):
        first = self.enrolled()
        before = self.service.prepared(first["request_id"])
        for field in chat.ENROLLMENT_BINDING_FIELDS:
            binding = self.enrollment_binding()
            binding[field] = "fallback" if field == "queue_kind" else binding[field] + "-changed"
            provider = Mock()
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.enrolled(provider=provider, binding=binding)
            provider.assert_not_called()
            self.assertEqual(self.service.prepared(first["request_id"]), before)
        binding = {**self.enrollment_binding(), "grant": "wc_en_" + "e" * 64}
        with self.assertRaises(ValueError):
            self.enrolled(binding=binding)
        self.thread_factory.assert_called_once()

    def test_profile_refusal_and_busy_preflight_do_not_claim_enrollment(self):
        provider = Mock(return_value=self.delivery())
        self.save_profiles(observed_at=0)
        refused = self.enrolled(provider=provider)
        self.assertEqual(refused["state"], "profile_unavailable")
        self.assertFalse(refused["request_created"])
        self.assertFalse((self.root / "tasks").exists())
        provider.assert_not_called()
        self.save_profiles()
        first = self.enrolled(provider=provider)
        busy_provider = Mock()
        busy = self.enrolled(key="another-key", provider=busy_provider)
        self.assertEqual(busy["state"], "busy")
        self.assertFalse(busy["launched"])
        self.assertEqual(len(list((self.root / "tasks").iterdir())), 1)
        self.assertTrue(self.service.directory(first["request_id"]).exists())
        provider.assert_not_called()
        busy_provider.assert_not_called()

    def test_stopping_preflight_does_not_claim_or_create_enrolled_request(self):
        self.service._stopping.set()
        provider = Mock()
        self.assertEqual(self.enrolled(provider=provider)["reason"], "service_stopping")
        provider.assert_not_called()
        self.assertFalse((self.root / "tasks").exists())

    def test_enrolled_worker_forwards_only_one_use_callback_and_private_runtime(self):
        delivery = self.delivery()
        provider = Mock(return_value=delivery)
        first = self.enrolled(provider=provider)

        def send(config, evidence, *, cancelled, enrollment_delivery_provider, runtime_root):
            self.assertFalse(cancelled())
            self.assertEqual(runtime_root, self.scheduler)
            self.assertNotIn("enrollment_binding", config)
            self.assertNotIn("wc_en_" + "e" * 64, json.dumps(config))
            self.assertIs(enrollment_delivery_provider(), delivery)
            with self.assertRaisesRegex(ValueError, "enrollment_delivery_already_requested"):
                enrollment_delivery_provider()
            return {"status": "sent"}

        self.transport.send.side_effect = send
        self.threads[-1].run()
        provider.assert_called_once_with()
        self.assertEqual(self.service.read_chat_task(first["request_id"])["state"], "finished")
        for path in self.root.rglob("*.json"):
            self.assertNotIn("wc_en_" + "e" * 64, path.read_text(encoding="utf-8"))

    def test_wrong_enrollment_delivery_is_unknown_and_cannot_be_reclaimed(self):
        provider = Mock(return_value=self.delivery("wc_enrollment_" + "f" * 32))
        first = self.enrolled(provider=provider)
        self.transport.send.side_effect = lambda *_, **kwargs: kwargs["enrollment_delivery_provider"]()
        self.threads[-1].run()
        result = self.service.read_chat_task(first["request_id"])
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(result["reason"], "ValueError")
        provider.assert_called_once_with()
        second_provider = Mock()
        self.enrolled(provider=second_provider)
        second_provider.assert_not_called()
        self.thread_factory.assert_called_once()

    def test_enrolled_thread_start_failure_does_not_claim_or_reschedule(self):
        self.next_start_error = RuntimeError("wc_en_" + "e" * 64)
        provider = Mock()
        first = self.enrolled(provider=provider)
        self.assertEqual(first["state"], "unknown")
        self.assertEqual(self.enrolled(provider=provider)["request_id"], first["request_id"])
        provider.assert_not_called()
        self.thread_factory.assert_called_once()
        for path in self.root.rglob("*.json"):
            self.assertNotIn("wc_en_" + "e" * 64, path.read_text(encoding="utf-8"))

    def test_partial_enrolled_directory_blocks_reclaim_after_request_write_failure(self):
        provider = Mock()
        original = chat.write_json

        def failing(path, value):
            if Path(path).name == "request.json":
                raise OSError("wc_en_" + "e" * 64)
            return original(path, value)

        with patch.object(chat, "write_json", side_effect=failing):
            first = self.enrolled(provider=provider)
        self.assertEqual(first["state"], "unknown")
        self.assertEqual(self.enrolled(provider=provider)["state"], "unknown")
        provider.assert_not_called()
        self.thread_factory.assert_not_called()
        self.assertEqual(len(list((self.root / "tasks").iterdir())), 1)


if __name__ == "__main__":
    unittest.main()
