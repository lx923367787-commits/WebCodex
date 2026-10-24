import copy
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

import handoff_controller as hc


CONFIG = {"goal_id": "goal-test", "project": "project-test", "source_context_epoch": "epoch-A", "endpoint": "http://127.0.0.1:19876"}


def evidence():
    return {"driver": "host", "projection": "handoff",
        "goal": {"goal_id": "goal-test", "project": "project-test", "current_attempt_id": "attempt-B", "fence": 2},
        "source_context": {"context_epoch": "epoch-A", "identity_source": "explicit_context_only", "lifecycle_state": "retired", "retired_at": 110},
        "checkpoint": {"sequence": 1, "revision": 5, "action": "handoff", "source_context_epoch": "epoch-A", "source_fence": 1, "created_at": 110},
        "current_attempt": {"binding": {"goal_id": "goal-test", "node_id": "node-1", "attempt_id": "attempt-B", "plan_version": 1, "fence": 2},
            "status": "passed", "context_epoch": "epoch-B", "identity_source": "explicit_context_only", "started_at": 120, "finished_at": 130},
        "unresolved_operation_count": 0, "business_verified": True,
        "business_evidence": {"operation_id": "op-B", "attempt_id": "attempt-B", "status": "succeeded", "check_passed": True}}


class FakeApi:
    def __init__(self, value=None, reason=None):
        self.value, self.reason, self.calls = value, reason, 0

    def read(self, config):
        self.calls += 1
        if self.reason:
            raise hc.ReadFailure(self.reason)
        return self.value


class ControllerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "observer.sqlite3"
        self.store = hc.Store(self.path)
        self.store.initialize(CONFIG)

    def tearDown(self):
        self.store.close()
        self.temp.cleanup()

    def test_complete_with_fixed_source_and_advanced_fence(self):
        result = hc.poll_once(self.store, FakeApi(evidence()))
        snapshot = result["snapshot"]
        self.assertEqual(snapshot["phase"], "BUSINESS_VERIFIED")
        self.assertEqual(snapshot["assurance"], "local_protocol_only")
        self.assertFalse(snapshot["web_unattended_verified"])
        self.assertEqual([x[0] for x in self.store.db.execute("SELECT event FROM events")], list(hc.PHASES[1:4]))

    def test_launch_is_deduplicated_after_cold_restart(self):
        first = hc.poll_once(self.store, FakeApi(evidence()))["snapshot"]
        self.store.close()
        self.store = hc.Store(self.path)
        self.store.initialize(CONFIG)
        second = hc.poll_once(self.store, FakeApi(evidence()))["snapshot"]
        self.assertEqual(first["launch_id"], second["launch_id"])
        self.assertEqual(self.store.db.execute("SELECT COUNT(*) FROM handoff").fetchone()[0], 1)
        self.assertEqual(self.store.db.execute("SELECT COUNT(*) FROM events").fetchone()[0], 3)

    def test_bootstrap_is_not_claim(self):
        raw = evidence()
        raw["current_attempt"] = None
        self.assertEqual(hc.normalize(raw, CONFIG)["phase"], "WAITING_NEW_CHAT")

    def test_claim_without_business_is_not_success(self):
        raw = evidence()
        raw["business_verified"] = False
        self.assertEqual(hc.normalize(raw, CONFIG)["phase"], "B_CLAIMED")

    def test_completed_server_clears_active_pointer_but_retains_latest_passed_attempt(self):
        raw = evidence()
        raw["goal"]["current_attempt_id"] = None
        self.assertEqual(hc.normalize(raw, CONFIG)["phase"], "BUSINESS_VERIFIED")
        raw["current_attempt"]["status"] = "running"
        self.assertEqual(hc.normalize(raw, CONFIG)["reason"], "unknown")

    def test_running_successor_with_evidence_still_not_verified(self):
        raw = evidence()
        raw["current_attempt"]["status"] = "running"
        self.assertEqual(hc.normalize(raw, CONFIG)["phase"], "B_CLAIMED")

    def test_business_must_match_current_attempt(self):
        raw = evidence()
        raw["business_evidence"]["attempt_id"] = "attempt-A"
        self.assertEqual(hc.normalize(raw, CONFIG)["phase"], "B_CLAIMED")

    def test_no_fence_advance_cannot_claim(self):
        raw = evidence()
        raw["current_attempt"]["binding"]["fence"] = 1
        result = hc.normalize(raw, CONFIG)
        self.assertEqual(result["phase"], "WAITING_NEW_CHAT")
        self.assertEqual(result["reason"], "unknown")

    def test_source_identity_missing_is_blocked(self):
        raw = evidence()
        raw["source_context"]["identity_source"] = None
        result = hc.poll_once(self.store, FakeApi(raw))
        self.assertEqual(result["mode"], "blocked")
        self.assertEqual(result["reason"], "unknown")
        self.assertNotEqual(result["snapshot"]["phase"], "BUSINESS_VERIFIED")

    def test_source_cannot_be_overwritten_by_latest_epoch(self):
        raw = evidence()
        raw["source_context"]["context_epoch"] = "epoch-B"
        with self.assertRaises(hc.ReadFailure):
            hc.normalize(raw, CONFIG)

    def test_cross_goal_or_project_reply_is_rejected(self):
        for field in ("project", "goal_id"):
            raw = evidence()
            raw["goal"][field] = "other"
            with self.assertRaises(hc.ReadFailure):
                hc.normalize(raw, CONFIG)

    def test_legacy_checkpoint_unknown_never_success(self):
        for field in ("source_context_epoch", "action", "source_fence"):
            raw = evidence()
            raw["checkpoint"].pop(field)
            result = hc.normalize(raw, CONFIG)
            self.assertEqual(result["reason"], "unknown")
            self.assertEqual(result["phase"], "WAITING_CHECKPOINT")

    def test_handoff_requires_retirement(self):
        raw = evidence()
        raw["source_context"]["lifecycle_state"] = "draining"
        self.assertEqual(hc.normalize(raw, CONFIG)["reason"], "unknown")

    def test_reconciliation_is_persistent_and_resume_rechecks(self):
        raw = evidence()
        raw["unresolved_operation_count"] = 1
        state = hc.poll_once(self.store, FakeApi(raw))
        self.assertEqual((state["mode"], state["reason"]), ("blocked", "reconciliation"))
        self.store.close()
        self.store = hc.Store(self.path)
        api = FakeApi(evidence())
        self.assertEqual(hc.poll_once(self.store, api)["mode"], "blocked")
        self.assertEqual(api.calls, 0)
        self.assertEqual(hc.poll_once(self.store, api, resume=True)["mode"], "running")
        self.assertEqual(api.calls, 1)

    def test_stop_survives_restart_and_watch_does_not_read(self):
        self.store.set_mode("stopped")
        self.store.close()
        self.store = hc.Store(self.path)
        self.store.initialize(CONFIG)
        api = FakeApi(evidence())
        self.assertEqual(hc.poll_once(self.store, api)["mode"], "stopped")
        self.assertEqual(api.calls, 0)

    def test_pause_reason_persists_and_failed_resume_cannot_clear(self):
        self.store.set_mode("blocked", "tool_approval")
        state = hc.poll_once(self.store, FakeApi(reason="connector_auth"), resume=True)
        self.assertEqual((state["mode"], state["reason"]), ("blocked", "tool_approval"))

    def test_three_failures_pause_without_fourth_request(self):
        api = FakeApi(reason="connector_auth")
        for expected in (1, 2, 3):
            state = hc.poll_once(self.store, api)
            self.assertEqual(state["failures"], expected)
        self.assertEqual((state["mode"], state["reason"]), ("paused", "connector_auth"))
        hc.poll_once(self.store, api)
        self.assertEqual(api.calls, 3)

    def test_success_resets_consecutive_failure_counter(self):
        hc.poll_once(self.store, FakeApi(reason="unknown"))
        state = hc.poll_once(self.store, FakeApi(evidence()))
        self.assertEqual(state["failures"], 0)

    def test_stop_racing_successful_request_wins(self):
        class StopApi:
            def read(inner, config):
                self.store.set_mode("stopped")
                return evidence()
        self.assertEqual(hc.poll_once(self.store, StopApi())["mode"], "stopped")

    def test_stop_racing_resume_wins(self):
        self.store.set_mode("paused")
        class StopApi:
            def read(inner, config):
                self.store.set_mode("stopped")
                return evidence()
        self.assertEqual(hc.poll_once(self.store, StopApi(), resume=True)["mode"], "stopped")

    def test_interrupt_cannot_overwrite_explicit_stop_or_block(self):
        self.store.pause_if_running()
        self.assertEqual(self.store.status()["mode"], "paused")
        for mode, reason in (("stopped", None), ("blocked", "tool_approval")):
            self.store.set_mode(mode, reason)
            generation = self.store.status()["control_generation"]
            self.store.pause_if_running()
            current = self.store.status()
            self.assertEqual((current["mode"], current["reason"], current["control_generation"]), (mode, reason, generation))

    def test_atomic_checkpoint_identity_cannot_change(self):
        hc.poll_once(self.store, FakeApi(evidence()))
        raw = evidence()
        raw["checkpoint"]["revision"] += 1
        result = hc.poll_once(self.store, FakeApi(raw))
        self.assertEqual(result["failures"], 1)
        self.assertEqual(result["snapshot"]["checkpoint_revision"], 5)

    def test_later_claim_cannot_inherit_previous_business_verified_phase(self):
        old = hc.poll_once(self.store, FakeApi(evidence()))["snapshot"]
        raw = evidence()
        raw["goal"].update(current_attempt_id="attempt-C", fence=3)
        raw["current_attempt"]["binding"].update(attempt_id="attempt-C", fence=3)
        raw["current_attempt"].update(context_epoch="epoch-C", status="running")
        raw["business_verified"] = False
        raw["business_evidence"] = None
        result = hc.poll_once(self.store, FakeApi(raw))
        self.assertEqual(result["failures"], 1)
        self.assertEqual(result["snapshot"], old)
        persisted = json.loads(self.store.db.execute("SELECT snapshot FROM handoff").fetchone()[0])
        self.assertEqual(persisted, old)

    def test_later_verified_attempt_cannot_replace_original_verified_successor(self):
        old = hc.poll_once(self.store, FakeApi(evidence()))["snapshot"]
        raw = evidence()
        raw["goal"].update(current_attempt_id="attempt-C", fence=3)
        raw["current_attempt"]["binding"].update(attempt_id="attempt-C", fence=3)
        raw["current_attempt"].update(context_epoch="epoch-C")
        raw["business_evidence"].update(attempt_id="attempt-C", operation_id="op-C")
        result = hc.poll_once(self.store, FakeApi(raw))
        self.assertEqual(result["failures"], 1)
        self.assertEqual(result["snapshot"], old)

    def test_foreign_config_never_rebinds_existing_db(self):
        with self.assertRaises(ValueError):
            self.store.initialize({**CONFIG, "source_context_epoch": "epoch-B"})
        self.assertEqual(self.store.status()["config"], CONFIG)

    def test_recovery_prompt_is_bounded_and_no_claim_of_web_success(self):
        state = hc.poll_once(self.store, FakeApi(evidence()))
        prompt = hc.recovery_prompt(state)
        self.assertIn(CONFIG["goal_id"], prompt)
        self.assertIn("不代表", prompt)
        self.assertLess(len(prompt), 2000)

    def test_unsolicited_raw_fields_are_never_persisted(self):
        raw = evidence()
        raw["raw_auth_header"] = "Bearer synthetic-secret-never-store"
        raw["goal"]["objective"] = "synthetic-private-conversation"
        hc.poll_once(self.store, FakeApi(raw))
        persisted = "\n".join(self.store.db.iterdump())
        self.assertNotIn("synthetic-secret", persisted)
        self.assertNotIn("synthetic-private-conversation", persisted)

    def test_single_watcher_lock(self):
        with hc.watcher_lock(self.path):
            with self.assertRaises(ValueError):
                with hc.watcher_lock(self.path):
                    self.fail("second watcher acquired lock")


class NetworkTests(unittest.TestCase):
    def test_remote_endpoint_and_credentials_rejected(self):
        for value in ("https://chatgpt.com", "http://example.com:8080", "http://user:secret@localhost:80", "http://127.0.0.1:80/?token=secret", "http://127.0.0.1:80/api"):
            with self.assertRaises(ValueError):
                hc.endpoint(value)

    def run_server(self, handler, call):
        server = HTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            call(f"http://127.0.0.1:{server.server_port}")
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_real_http_read_shape_and_token_stays_out_of_projection(self):
        seen = {}
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                seen.update(path=self.path, body=json.loads(self.rfile.read(int(self.headers["Content-Length"]))), authorization=self.headers.get("Authorization"))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps({"success": True, "output": evidence()}).encode())
            def log_message(self, *args):
                pass
        def call(origin):
            result = hc.Api(origin, "synthetic-test-token").read(CONFIG)
            self.assertEqual(result, evidence())
        with patch.dict(os.environ, {"HTTP_PROXY": "http://invalid.example:9"}):
            self.run_server(Handler, call)
        self.assertEqual(seen["path"], "/api/tools/call")
        self.assertEqual(seen["body"]["tool"], "goal_read")
        self.assertEqual(seen["body"]["params"]["source_context_epoch"], "epoch-A")
        self.assertNotIn("project", seen["body"]["params"])
        self.assertNotIn("context_epoch", seen["body"]["params"])
        self.assertEqual(seen["authorization"], "Bearer synthetic-test-token")

    def test_redirect_never_followed(self):
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                self.send_response(307)
                self.send_header("Location", "http://invalid.example:9/steal")
                self.end_headers()
            def log_message(self, *args):
                pass
        def call(origin):
            with self.assertRaises(hc.ReadFailure):
                hc.Api(origin, "synthetic-token").read(CONFIG)
        self.run_server(Handler, call)


if __name__ == "__main__":
    unittest.main()
