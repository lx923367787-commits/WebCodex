"""Offline facade checks. Browser, network and process creation are mocked."""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock, patch
import uuid

import probe_tools as probe


class ProbeToolsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="webcodex-probe-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.root_patch = patch.object(probe, "ROOT", self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)
        # Any unintended process attempt fails instead of launching a browser.
        self.process_patch = patch.object(probe.subprocess, "Popen",
                                         side_effect=AssertionError("unexpected_process"))
        self.popen = self.process_patch.start()
        self.addCleanup(self.process_patch.stop)
        self.request_id = str(uuid.uuid4())
        self.config = {
            "launch_id": self.request_id,
            "parent_session_id": "offline-parent",
            "task_id": "wc_probe_" + uuid.uuid4().hex,
            "prompt": "A fixed offline-only request.",
            "expires_at": "2099-01-01T00:00:00+00:00",
        }
        self.save_request()

    def save_request(self):
        (self.root / "pilot-request.json").write_text(
            json.dumps(self.config), encoding="utf-8")

    def permit_mock_child(self):
        self.popen.side_effect = None
        self.popen.return_value = types.SimpleNamespace(pid=4242)

    def dispatch_path(self):
        return self.root / "dispatch" / (self.request_id + ".json")

    def transport(self, result):
        module = types.ModuleType("pilot_transport")
        module.run = Mock(return_value=result)
        return module

    def test_unprepared_request_is_rejected_without_process(self):
        with self.assertRaisesRegex(ValueError, "request_not_prepared"):
            probe.spawn_chat_task(str(uuid.uuid4()))
        self.popen.assert_not_called()
        self.assertFalse((self.root / "dispatch").exists())

    def test_noncanonical_uuid_is_rejected(self):
        with self.assertRaises(ValueError):
            probe.spawn_chat_task(self.request_id.replace("-", ""))
        self.popen.assert_not_called()

    def test_read_before_launch_has_no_effect(self):
        self.assertEqual(probe.read_chat_task(self.request_id),
                         {"request_id": self.request_id, "state": "not_started"})
        self.popen.assert_not_called()
        self.assertFalse((self.root / "dispatch").exists())

    def test_spawn_is_durable_and_same_id_does_not_spawn_twice(self):
        self.permit_mock_child()
        first = probe.spawn_chat_task(self.request_id)
        second = probe.spawn_chat_task(self.request_id)
        self.assertEqual(first, second)
        self.assertEqual(first["state"], "accepted")
        self.assertFalse(first["business_verified"])
        self.assertEqual(json.loads(self.dispatch_path().read_text()), first)
        self.popen.assert_called_once()

    def test_worker_argv_and_environment_do_not_forward_credentials(self):
        self.permit_mock_child()
        environment = {
            "PATH": "offline-test-path", "TEMP": "offline-temp",
            "WEBCODEX_TOKEN": "secret-marker", "OPENAI_API_KEY": "secret-marker",
            "PLAYWRIGHT_MCP_EXTENSION_TOKEN": "secret-marker",
            "MCP_AUTH_HEADER": "secret-marker", "TASK_CUSTOM_SECRET": "secret-marker",
        }
        with patch.dict(probe.os.environ, environment, clear=True):
            probe.spawn_chat_task(self.request_id)
        args, kwargs = self.popen.call_args
        self.assertEqual(args[0][-2:], ["--worker", self.request_id])
        self.assertEqual(Path(args[0][1]).resolve(), Path(probe.__file__).resolve())
        self.assertEqual(kwargs["cwd"], str(self.root))
        self.assertEqual(kwargs["env"], {"PATH": "offline-test-path", "TEMP": "offline-temp"})
        self.assertIs(kwargs["stdin"], subprocess.DEVNULL)
        self.assertIs(kwargs["stdout"], subprocess.DEVNULL)
        self.assertIs(kwargs["stderr"], subprocess.DEVNULL)
        self.assertTrue(kwargs["close_fds"])
        self.assertNotIn("shell", kwargs)

    def test_changed_request_after_issue_is_rejected_without_relaunch(self):
        self.permit_mock_child()
        probe.spawn_chat_task(self.request_id)
        self.config["prompt"] = "A different request is not a retry."
        self.save_request()
        for operation in (probe.read_chat_task, probe.spawn_chat_task):
            with self.assertRaisesRegex(ValueError, "prepared_request_changed"):
                operation(self.request_id)
        self.popen.assert_called_once()

    def test_request_key_order_is_not_an_intent_change(self):
        self.permit_mock_child()
        first = probe.spawn_chat_task(self.request_id)
        self.config = dict(reversed(list(self.config.items())))
        self.save_request()
        self.assertEqual(probe.spawn_chat_task(self.request_id), first)
        self.popen.assert_called_once()

    def test_process_creation_error_is_unknown_and_never_replayed(self):
        self.popen.side_effect = OSError("offline simulated Popen error")
        first = probe.spawn_chat_task(self.request_id)
        self.assertEqual(first["state"], "unknown")
        self.assertEqual(probe.spawn_chat_task(self.request_id), first)
        self.popen.assert_called_once()

    def test_uncertain_acceptance_persistence_does_not_start_another_child(self):
        self.permit_mock_child()
        original_write = probe.write_json
        writes = 0

        def write_once_fails(path, value):
            nonlocal writes
            writes += 1
            if writes == 1:
                raise OSError("offline simulated accepted-record write failure")
            return original_write(path, value)

        with patch.object(probe, "write_json", side_effect=write_once_fails):
            result = probe.spawn_chat_task(self.request_id)
        self.assertEqual(result["state"], "unknown")
        self.assertEqual(probe.spawn_chat_task(self.request_id), result)
        self.popen.assert_called_once()

    def test_partial_dispatch_file_fails_closed_without_process(self):
        self.dispatch_path().parent.mkdir()
        self.dispatch_path().write_text("{", encoding="utf-8")
        with self.assertRaises(ValueError):
            probe.spawn_chat_task(self.request_id)
        self.popen.assert_not_called()

    def test_worker_uses_prepared_request_and_does_not_spawn_process(self):
        self.permit_mock_child()
        probe.spawn_chat_task(self.request_id)
        self.popen.reset_mock()
        transport = self.transport({"status": "blocked", "reason": "offline-test"})
        with patch.dict(sys.modules, {"pilot_transport": transport}):
            probe.worker(self.request_id)
        transport.run.assert_called_once_with(self.config, self.root / "evidence",
                                              collect_response=True)
        self.popen.assert_not_called()

    def test_worker_rejects_changed_request_before_transport(self):
        self.permit_mock_child()
        probe.spawn_chat_task(self.request_id)
        self.config["prompt"] = "Changed before child begins."
        self.save_request()
        transport = self.transport({"status": "sent"})
        with patch.dict(sys.modules, {"pilot_transport": transport}):
            outcome = probe.worker(self.request_id)
        self.assertEqual(outcome, {"state": "unknown", "reason": "ValueError"})
        saved = self.root / "worker-results" / (self.request_id + ".json")
        self.assertEqual(json.loads(saved.read_text()), outcome)
        transport.run.assert_not_called()

    def test_worker_without_dispatch_record_does_not_call_transport(self):
        transport = self.transport({"status": "sent"})
        with patch.dict(sys.modules, {"pilot_transport": transport}):
            outcome = probe.worker(self.request_id)
        self.assertEqual(outcome, {"state": "unknown", "reason": "FileNotFoundError"})
        saved = self.root / "worker-results" / (self.request_id + ".json")
        self.assertEqual(json.loads(saved.read_text()), outcome)
        transport.run.assert_not_called()
        self.popen.assert_not_called()

    def test_early_blocked_transport_outcome_is_visible_without_evidence_folder(self):
        self.permit_mock_child()
        accepted = probe.spawn_chat_task(self.request_id)
        blocked = {"status": "blocked", "reason": "scheduler_not_running",
                   "launched": False, "business_verified": False}
        transport = self.transport(blocked)
        with patch.dict(sys.modules, {"pilot_transport": transport}):
            outcome = probe.worker(self.request_id)
        self.assertFalse((self.root / "evidence").exists())
        self.assertEqual(outcome, {"state": "finished", "result": blocked})
        observed = probe.read_chat_task(self.request_id)
        self.assertEqual(observed["worker_outcome"], outcome)
        self.assertEqual(json.loads(self.dispatch_path().read_text()), accepted)
        self.assertFalse(observed["business_verified"])
        probe.spawn_chat_task(self.request_id)
        self.popen.assert_called_once()

    def test_worker_exception_is_persisted_without_sensitive_details(self):
        self.permit_mock_child()
        probe.spawn_chat_task(self.request_id)
        transport = self.transport(None)
        transport.run.side_effect = RuntimeError("DO_NOT_LEAK_CREDENTIAL_MARKER")
        with patch.dict(sys.modules, {"pilot_transport": transport}):
            outcome = probe.worker(self.request_id)
        self.assertEqual(outcome, {"state": "unknown", "reason": "RuntimeError"})
        observed = probe.read_chat_task(self.request_id)
        self.assertEqual(observed["worker_outcome"], outcome)
        self.assertNotIn("DO_NOT_LEAK", json.dumps(observed))
        probe.spawn_chat_task(self.request_id)
        self.popen.assert_called_once()

    def test_observed_result_is_not_marked_business_verified(self):
        self.permit_mock_child()
        probe.spawn_chat_task(self.request_id)
        evidence = self.root / "evidence" / self.request_id
        evidence.mkdir(parents=True)
        observation = {"status": "sent", "response_observed": True,
                       "response_text": "Candidate text.", "business_verified": False}
        (evidence / "result.json").write_text(json.dumps(observation), encoding="utf-8")
        record = probe.read_chat_task(self.request_id)
        self.assertEqual(record["observation"], observation)
        self.assertFalse(record["business_verified"])
        self.assertEqual(record["state"], "accepted")


if __name__ == "__main__":
    unittest.main()
