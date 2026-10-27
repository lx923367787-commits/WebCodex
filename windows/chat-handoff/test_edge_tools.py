"""Isolated tool and scheduler tests; no browser or real worker is launched."""
import asyncio
import datetime as dt
import io
import json
import os
from pathlib import Path
import tempfile
import time
import types
import unittest
from unittest import mock
import uuid

import auto_handoff as ah
import edge_tools as tools
import handoff_controller as hc
import handoff_flow as flow
import handoff_reliability as reliable
from test_auto_handoff import CANDIDATE, FakeApi, ORIGIN, evidence


class EdgeToolsTests(unittest.TestCase):
    def setUp(self):
        policy = mock.patch("chat_task_policy.resolve_goal_profile", return_value=None)
        policy.start()
        self.addCleanup(policy.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.runtime = Path(self.temp.name)
        self.now = time.time()
        self.raw = evidence()
        retired = int((self.now - 130) * 1000)
        self.raw["source_context"]["retired_at"] = retired
        self.raw["checkpoint"]["created_at"] = retired
        self.raw["handoff"]["created_at"] = retired
        self.api = FakeApi()
        self.api.raw = self.raw
        self.api.now = self.now
        self.store = ah.Store(self.runtime / "auto-handoff.sqlite3")
        self.store.initialize(ORIGIN)
        value = ah.read_observation(self.api, CANDIDATE)
        reliable.observe(self.store, CANDIDATE, value)
        _, entry = reliable.refresh(self.store, self.api, CANDIDATE, value, "begin_attempt")
        self.request = ah.make_request(entry, int(self.now))
        self.id = self.request["launch_id"]
        self.path = self.runtime / "requests" / (self.id + ".json")
        tools.write_json(self.path, self.request)
        self.spawn = mock.Mock(return_value=1234)
        self.backend = types.SimpleNamespace(launch=mock.Mock(return_value={"status": "sent"}),
            inspect=mock.Mock(return_value={"endpoint_available": True, "backend": "playwright_cdp"}))
        self.coordinator = mock.Mock(return_value={"state": "sent", "business_verified": False})
        self.service = tools.Service(self.runtime, spawner=self.spawn, loader=lambda: self.backend,
            available=lambda: True, clock=lambda: self.now, coordinator=self.coordinator, endpoint_check=lambda: False)

    def tearDown(self):
        self.store.close()
        self.temp.cleanup()

    def coordinate(self):
        return tools.run_scheduler_request(self.runtime, self.id, 0, loader=lambda: self.backend,
            credentials=lambda: (ORIGIN, "fake-secret", "operator-owned-token-path"),
            api_factory=lambda origin, token: self.api)

    def test_registered_request_starts_one_hidden_worker_and_duplicate_is_not_replayed(self):
        self.assertEqual(self.service.launch(self.id)["state"], "accepted")
        self.assertEqual(self.service.launch(self.id)["state"], "accepted")
        self.spawn.assert_called_once_with(self.runtime, self.id)
        self.assertEqual(self.service.worker(self.id), 0)
        self.assertEqual(self.service.status(self.id)["state"], "sent")
        self.assertEqual(self.service.launch(self.id)["state"], "sent")
        self.assertEqual(self.service.worker(self.id), 2)
        self.coordinator.assert_called_once()

    def test_unregistered_json_file_is_not_authority_to_send(self):
        request = dict(self.request, launch_id=str(uuid.uuid4()))
        tools.write_json(self.runtime / "requests" / (request["launch_id"] + ".json"), request)
        self.assertEqual(self.service.launch(request["launch_id"])["state"], "blocked")
        self.spawn.assert_not_called()

    def test_tool_request_id_cannot_supply_path_or_url(self):
        for value in ("../other", str(self.path), "https://chatgpt.com/", "not-a-uuid"):
            self.assertEqual(self.service.launch(value)["state"], "blocked")
        self.spawn.assert_not_called()

    def test_immutable_request_mutation_cannot_spawn_another_worker(self):
        self.service.launch(self.id)
        self.request["recovery_prompt"] += " injected change"
        tools.write_json(self.path, self.request)
        self.assertEqual(self.service.launch(self.id)["reason"], "request_changed")
        self.service.worker(self.id)
        self.assertEqual(self.service.status(self.id)["state"], "blocked")
        self.coordinator.assert_not_called()
        self.assertEqual(self.spawn.call_count, 1)

    def test_unknown_worker_result_never_replays(self):
        self.coordinator.return_value = {"state": "unknown", "reason": "transport_uncertain"}
        self.service.launch(self.id)
        self.service.worker(self.id)
        self.assertEqual(self.service.launch(self.id)["state"], "unknown")
        self.assertEqual(self.spawn.call_count, 1)

    def test_retry_requires_new_server_attempt_not_repeated_worker(self):
        self.coordinator.return_value = {"state": "retryable", "launched": False, "reason": "client_busy"}
        self.service.launch(self.id)
        self.service.worker(self.id)
        self.request["expires_at"] = ah.utc_iso(self.now + 900)
        tools.write_json(self.path, self.request)
        self.assertEqual(self.service.launch(self.id)["state"], "retryable")
        self.assertEqual(self.spawn.call_count, 1)

    def test_retryable_without_not_started_proof_is_unknown(self):
        self.coordinator.return_value = {"state": "retryable"}
        self.service.launch(self.id)
        self.service.worker(self.id)
        self.assertEqual(self.service.launch(self.id)["state"], "unknown")
        self.assertEqual(self.spawn.call_count, 1)

    def test_stop_between_enqueue_and_worker_prevents_scheduler_dispatch(self):
        self.service.launch(self.id)
        self.store.set_mode("stopped")
        self.service.worker(self.id)
        self.assertEqual(self.service.status(self.id)["state"], "retryable")
        self.coordinator.assert_not_called()

    def test_status_and_inspect_do_not_expose_page_or_secret_fields(self):
        self.backend.inspect.return_value.update(html="private page body", token="secret",
            result={"status": "unknown", "reason": "secret full page text"})
        value = self.service.inspect(self.id)
        self.assertTrue(value["snapshot"]["endpoint_available"])
        self.assertEqual(value["snapshot"]["backend"], "playwright_cdp")
        self.assertNotIn("secret", json.dumps(value))
        self.assertNotIn("private page body", json.dumps(value))
        self.spawn.assert_not_called()

    def test_status_of_watcher_owned_handoff_is_not_reported_as_not_started(self):
        with self.store.db:
            self.store.db.execute("UPDATE dispatch SET status='resumed' WHERE launch_id=?", (self.id,))
        self.backend.inspect.return_value["result"] = {"status": "sent", "message_observed": True}
        value = self.service.status(self.id)
        self.assertEqual(value["scheduler_state"], "resumed")
        self.assertEqual(value["state"], "resumed")
        self.assertEqual(value["backend"]["result"]["state"], "sent")
        self.assertFalse(self.service.status()["edge_endpoint_available"])

    def test_main_binds_explicit_runtime_before_native_backend_can_load(self):
        seen = []
        def service(runtime):
            seen.append(os.environ.get("WEBCODEX_HANDOFF_RUNTIME"))
            return self.service
        with mock.patch.dict(os.environ), mock.patch.object(tools, "Service", side_effect=service), \
                mock.patch.object(tools, "serve_native"):
            self.assertEqual(tools.main(["--runtime", str(self.runtime), "--native-plugin"]), 0)
        self.assertEqual(seen, [str(self.runtime.resolve())])

    def test_worker_subprocess_environment_is_allowlisted(self):
        fake = types.SimpleNamespace(pid=456)
        with mock.patch.dict(os.environ, {"WEBCODEX_READ_TOKEN": "secret1", "SERVER_TOKEN": "secret2",
                "CUSTOM_CREDENTIAL": "secret3", "SYSTEMROOT": "C:\\Windows"}), \
                mock.patch.object(tools.subprocess, "Popen", return_value=fake) as popen:
            self.assertEqual(tools.spawn_worker(self.runtime, self.id), 456)
        sent = popen.call_args.kwargs["env"]
        self.assertFalse({"WEBCODEX_READ_TOKEN", "SERVER_TOKEN", "CUSTOM_CREDENTIAL"} & set(sent))
        self.assertEqual(sent["WEBCODEX_HANDOFF_RUNTIME"], str(self.runtime))
        self.assertNotIn("secret", str(popen.call_args))

    def test_coordinator_uses_fresh_server_state_and_updates_original_dispatch(self):
        self.assertEqual(self.coordinate()["state"], "sent")
        self.backend.launch.assert_called_once()
        self.assertEqual(self.store.entry(self.id)["status"], "waiting_ready")
        self.assertGreaterEqual(len(self.api.actions), 3)

    def test_coordinator_cannot_bypass_120_second_deadline(self):
        with self.store.db:
            self.store.db.execute("UPDATE dispatch SET deadline_at=? WHERE launch_id=?", (int(time.time()) + 120, self.id))
        self.assertEqual(self.coordinate()["state"], "retryable")
        self.backend.launch.assert_not_called()

    def test_coordinator_manual_resume_blocks_automatic_send(self):
        self.api.commit()
        self.assertEqual(self.coordinate()["reason"], "goal_no_longer_waiting")
        self.assertEqual(self.store.entry(self.id)["status"], "resumed")
        self.backend.launch.assert_not_called()

    def test_running_scheduler_lease_is_not_bypassed(self):
        with hc.watcher_lock(self.runtime / "desktop-dispatch"):
            self.assertEqual(self.coordinate()["state"], "retryable")
        self.backend.launch.assert_not_called()

    def test_orphaned_claim_is_unknown_and_not_replayed(self):
        reliable.claim(self.store, self.store.entry(self.id), 0, int(self.now), self.path)
        self.store.recover_interrupted()
        self.assertEqual(self.coordinate()["state"], "unknown")
        self.assertEqual(reliable.attempt(self.store, self.id)["status"], "unknown")
        self.backend.launch.assert_not_called()

    def test_new_attempt_racing_fixed_request_never_launches_other_uuid(self):
        def advance(params):
            if params["action"] == "status":
                self.raw["handoff"].update(attempt_count=2, attempt_id=str(uuid.uuid4()))
                self.api.on_action = None
        self.api.on_action = advance
        self.coordinate()
        self.backend.launch.assert_not_called()

    def test_handoff_binding_tamper_rejected_before_worker(self):
        self.request["handoff_id"] = "other-handoff"
        tools.write_json(self.path, self.request)
        self.assertEqual("blocked", self.service.launch(self.id)["state"])
        self.spawn.assert_not_called()

    def test_native_jsonl_catalog_and_fixed_arguments(self):
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": tools.PROTOCOL_VERSION}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "handoff_status", "arguments": {}}},
            {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "handoff_launch", "arguments": {"launch_id": self.id, "url": "https://example.invalid"}}},
            {"jsonrpc": "2.0", "id": 5, "method": "execute_javascript", "params": {}},
        ]
        source = io.BytesIO(b"\n".join(json.dumps(value).encode() for value in requests) + b"\n")
        output = io.BytesIO()
        tools.serve_native(self.service, source, output)
        replies = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual({item["name"] for item in replies[1]["result"]["tools"]},
            {"handoff_status", "handoff_inspect", "handoff_launch"})
        self.assertEqual(replies[2]["result"]["structuredContent"]["scheduler"]["mode"], "running")
        self.assertEqual(replies[3]["error"]["code"], -32602)
        self.assertEqual(replies[4]["error"]["code"], -32601)
        self.spawn.assert_not_called()

    def test_mcp_stdio_catalog_contains_only_three_high_level_tools(self):
        server = tools.build_mcp(self.service)
        catalog = asyncio.run(server.list_tools())
        self.assertEqual({item.name for item in catalog}, {"handoff_status", "handoff_inspect", "handoff_launch"})


class GrantBoundaryTests(unittest.TestCase):
    """Temporary records and fake providers only; never load a live launcher."""
    enrollment = "wc_en_" + "ab" * 32
    handoff = "wc_hc_" + "7e" * 32

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.id = "68efcd7a-ff7c-4b9f-9d80-675e7888e302"
        self.service = tools.Service(self.root, spawner=mock.Mock(), loader=mock.Mock(),
            available=lambda: False, endpoint_check=lambda: False)

    def assert_fixed_rejection(self, action):
        with self.assertRaises(ValueError) as rejected:
            action()
        self.assertEqual(str(rejected.exception), "launch_claim_must_not_be_persisted")
        self.assertNotIn(self.enrollment, str(rejected.exception))
        self.assertNotIn(self.handoff, str(rejected.exception))

    def test_both_json_writers_reject_nested_grants_before_any_open_or_directory_creation(self):
        for writer in (flow.write_json, tools.write_json):
            for grant in (self.enrollment, self.handoff):
                for value in ({"config": {"brief": "prefix " + grant}},
                              {"issued": [{"response_text": grant}]}, {grant: "key"}):
                    target = self.root / "must-not-exist" / "record.json"
                    with self.subTest(writer=writer.__module__, value_kind=list(value)[0]), \
                            mock.patch.object(Path, "open") as opened:
                        self.assert_fixed_rejection(lambda: writer(target, value))
                        opened.assert_not_called()
                    self.assertFalse(target.parent.exists())

    def test_rejected_write_preserves_an_existing_record(self):
        target = self.root / "existing.json"
        tools.write_json(target, {"state": "unknown", "revision": 1})
        before = target.read_bytes()
        for writer in (flow.write_json, tools.write_json):
            self.assert_fixed_rejection(lambda: writer(target, {"config": self.enrollment}))
            self.assertEqual(before, target.read_bytes())
        self.assertEqual([target], list(self.root.iterdir()))

    def test_read_legacy_candidates_omits_the_entire_body_without_rewriting_evidence(self):
        for grant in (self.enrollment, self.handoff):
            path = self.root / "old-candidate.json"
            value = {"result": {"items": [{"state": "response_observed", "response_observed": True,
                "response_text": "private-before " + grant + " private-after", "task_id": "task-test"}]}}
            # A legacy input fixture deliberately bypasses the protected writer.
            path.write_text(json.dumps(value), encoding="utf-8")
            before = path.read_bytes()
            result = tools.read_json(path)
            candidate = result["result"]["items"][0]
            self.assertNotIn("response_text", candidate)
            self.assertTrue(candidate["response_omitted"])
            self.assertEqual(candidate["response_reason"], "sensitive_candidate_omitted")
            self.assertFalse(candidate["response_observed"])
            self.assertEqual(candidate["state"], "unknown")
            self.assertEqual(candidate["task_id"], "task-test")
            self.assertNotIn("private-before", json.dumps(result))
            self.assertNotIn("private-after", json.dumps(result))
            self.assertNotIn(grant, json.dumps(result))
            self.assertEqual(before, path.read_bytes())

    def test_read_metadata_grants_fail_closed_even_next_to_a_sensitive_candidate(self):
        path = self.root / "old-record.json"
        for field in ("config", "reason", "response_reason", self.enrollment):
            value = {"response_text": self.enrollment, field: self.enrollment}
            path.write_text(json.dumps(value), encoding="utf-8")
            self.assert_fixed_rejection(lambda: tools.read_json(path))
        safe = {"response_text": "A safe candidate", "response_observed": True, "state": "response_observed"}
        tools.write_json(path, safe)
        self.assertEqual(tools.read_json(path), safe)

    def test_parent_status_and_native_response_never_echo_legacy_candidate_grants(self):
        path = self.service.path(self.id)
        path.parent.mkdir()
        path.write_text(json.dumps({"launch_id": self.id, "state": "unknown", "result": {
            "response_text": "whole private candidate " + self.enrollment,
            "response_observed": True, "state": "response_observed"}}), encoding="utf-8")
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": tools.PROTOCOL_VERSION}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "handoff_status", "arguments": {"launch_id": self.id}}},
        ]
        output = io.BytesIO()
        tools.serve_native(self.service, io.BytesIO(b"\n".join(json.dumps(v).encode() for v in requests) + b"\n"), output)
        replies = [json.loads(line) for line in output.getvalue().splitlines()]
        candidate = replies[1]["result"]["structuredContent"]["result"]
        self.assertTrue(candidate["response_omitted"])
        self.assertFalse(candidate["response_observed"])
        self.assertNotIn(self.enrollment.encode(), output.getvalue())
        self.assertNotIn(b"whole private candidate", output.getvalue())
        path.write_text(json.dumps({"state": "unknown", "reason": self.enrollment}), encoding="utf-8")
        self.assertEqual(self.service.status(self.id), {"state": "blocked", "reason": "invalid_launch_record"})
        self.service.spawner.assert_not_called()
        self.service.loader.assert_not_called()

    def test_backend_metadata_and_reason_cannot_smuggle_an_enrollment_grant(self):
        self.assertEqual(tools.safe_reason(self.enrollment, "safe"), "safe")
        self.assertEqual(tools.safe_reason("wc_hc_legacy-prefix", "safe"), "safe")
        self.assertEqual(tools.safe_reason("wc_en_", "safe"), "wc_en_")
        self.assertEqual(tools.outcome({"status": "unknown", "reason": self.enrollment})["reason"], "backend_result")
        backend = types.SimpleNamespace(inspect=mock.Mock(return_value={"backend": self.enrollment}))
        with mock.patch.object(tools, "load_request", return_value=(None, {}, "test-digest")), \
                mock.patch.object(self.service, "loader", return_value=backend):
            result = self.service.inspect(self.id)
        self.assertEqual(result, {"state": "blocked", "reason": "inspect_failed"})

    def test_old_handoff_claim_remains_transient_and_ordinary_requests_reject_new_grants(self):
        request = {"launch_id": self.id, "project": "project-test", "goal_id": "goal-test",
            "source_context_epoch": "epoch-test", "checkpoint_sequence": 1, "checkpoint_revision": 1,
            "expires_at": (flow.utc_now() + dt.timedelta(minutes=5)).isoformat(),
            "recovery_prompt": "Continue the exact existing task", "handoff_id": "handoff-test",
            "attempt_id": self.id, "attempt_number": 1, "agent_id": "agent-test"}
        with mock.patch.dict(os.environ, WEBCODEX_HANDOFF_RUNTIME=str(self.root)), \
                mock.patch.object(flow, "_ACTIVE_REQUEST", None), mock.patch.object(flow, "_SHOULD_STOP", lambda: False):
            message = flow.prepare_message(request, launch_claim=self.handoff)
            self.assertIn("launch_claim=" + self.handoff, message)
            self.assertNotIn(self.handoff, json.dumps(request))
            self.assertNotIn(self.handoff, json.dumps(flow._ACTIVE_REQUEST))
            self.assert_fixed_rejection(lambda: flow.write_json(self.root / "message.json", {"message": message}))
            for value in (dict(request, recovery_prompt=self.enrollment),
                          dict(request, configuration={"claim_grant": self.enrollment})):
                self.assert_fixed_rejection(lambda: flow.prepare_message(value, launch_claim=self.handoff))
        self.assertFalse(flow.valid_launch_claim(self.enrollment))
        self.assertFalse((self.root / "message.json").exists())


if __name__ == "__main__":
    unittest.main()
