"""Dependency-injected transport checks. No live browser or network calls."""
import contextlib
import datetime as dt
import json
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import Mock, patch
import uuid

import chat_transport as chat


class ChatTransportTests(unittest.TestCase):
    def test_profile_discovery_only_opens_owned_blank_page(self):
        with tempfile.TemporaryDirectory() as temp:
            session=Mock(); page=Mock(); session.create.return_value=page
            page.identity.return_value={"connection_id":"profile-connection"}
            @contextlib.contextmanager
            def connection():yield session
            previous=chat.pilot.handoff_flow._SHOULD_STOP
            discoverer=Mock(return_value={"profiles":[{"profile_id":"thinking_high"}]})
            result=chat.discover_profiles(session_factory=connection,
                state_reader=lambda:{"mode":"running","generation":7},
                lock_factory=lambda *_:contextlib.nullcontext(),discoverer=discoverer,
                runtime_root=Path(temp) / "scheduler")
            session.create.assert_called_once_with("https://chatgpt.com/")
            page.activate.assert_called_once()
            self.assertEqual("profile-connection",result["connection_id"])
            self.assertIs(previous,chat.pilot.handoff_flow._SHOULD_STOP)
            page.keys.assert_not_called()

    def test_profile_discovery_cancel_prevents_connection(self):
        connection=Mock()
        with self.assertRaisesRegex(ValueError,"discovery_cancelled"):
            chat.discover_profiles(cancelled=lambda:True,session_factory=connection,
                state_reader=lambda:{"mode":"running","generation":7},
                lock_factory=lambda *_:contextlib.nullcontext(), runtime_root=self.runtime)
        connection.assert_not_called()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.runtime = self.root / "scheduler"
        launch = str(uuid.uuid4())
        task = "wc_chat_" + uuid.uuid4().hex
        self.config = {"launch_id": launch, "parent_session_id": "parent_test", "task_id": task,
            "prompt": f"Authorized task launch={launch} task_id={task}",
            "expires_at": (dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=10)).isoformat()}
        self.receipt = {key: self.config[key] for key in chat.IDENTITY}
        self.url = "https://chatgpt.com/c/12345678-abcd"
        self.receipt.update(status="sent", target_url=self.url)
        self.state = {"mode": "running", "generation": 4}
        self.session_object = Mock()
        self.sessions = 0
        self.locks = []
        self.browser = Mock()
        self.browser.identity.return_value = {"connection_id": "test-persistent-connection"}
        self.inspector = Mock(return_value={"bound": True, "url": self.url})
        self.collector = Mock(return_value={"response_observed": True,
            "response_text": '{"marker":"WEBCODEX_CHAT_TASK_RESULT"}',
            "response_reason": "stable_candidate_only"})
        self.output = self.root / "response.json"
        self.now = 0

    @contextlib.contextmanager
    def session(self):
        self.sessions += 1
        yield self.session_object

    @contextlib.contextmanager
    def lock(self, path):
        self.locks.append(path)
        yield

    def sleep(self, seconds):
        self.now += seconds

    def observe(self, **kwargs):
        return chat.observe(self.config, self.receipt, self.output,
            session_factory=self.session, state_reader=lambda: dict(self.state),
            lock_factory=self.lock, browser_factory=lambda *_: self.browser,
            inspector=self.inspector, collector=self.collector,
            clock=lambda: self.now, sleep=self.sleep, runtime_root=self.runtime, **kwargs)

    def test_send_reuses_one_shot_logic_and_full_webcodex_guard(self):
        cancelled = lambda: False
        with patch.object(chat.pilot, "run", return_value={"status": "sent"}) as run:
            self.assertEqual({"status": "sent"}, chat.send(self.config, self.root, cancelled, runtime_root=self.runtime))
        self.assertEqual((self.config, self.root), run.call_args.args)
        options = run.call_args.kwargs
        self.assertEqual(chat.MARKER, options["response_marker"])
        self.assertTrue(options["collect_response"])
        self.assertIs(options["session_factory"], chat.pilot.playwright_extension_browser.persistent_session)
        self.assertIs(cancelled, options["external_stop"])
        self.assertIs(self.browser, options["approval_adapter"](self.browser))

    def test_new_and_legacy_task_identifiers_validate(self):
        self.assertEqual(self.config, chat.pilot.validate(self.config))
        legacy = dict(self.config)
        legacy["task_id"] = self.config["task_id"].replace("wc_chat_", "wc_probe_")
        legacy["prompt"] = self.config["prompt"].replace("wc_chat_", "wc_probe_")
        self.assertEqual(legacy, chat.pilot.validate(legacy))

    def test_send_external_cancel_prevents_browser_session(self):
        session = Mock()
        result = chat.pilot.run(self.config, self.root, external_stop=lambda: True,
            session_factory=session, state_reader=lambda: dict(self.state), lock_factory=self.lock,
            runtime_root=self.runtime)
        self.assertEqual("stopped_before_launch", result["reason"])
        session.assert_not_called()

    def test_observe_opens_only_exact_sent_chat_and_persists_candidate(self):
        result = self.observe()
        self.assertTrue(result["response_observed"])
        self.assertFalse(result["business_verified"])
        self.assertEqual([self.runtime / "edge-dispatch"], self.locks)
        self.session_object._transport.call.assert_called_once_with(
            "browser_tabs", {"action": "new", "url": self.url})
        self.browser._rpc.assert_called_once_with("initialize")
        self.assertEqual([("_rpc", ("initialize",), {}), ("identity", (), {})], self.browser.mock_calls)
        self.assertEqual("test-persistent-connection", result["connection_id"])
        self.assertEqual(result, json.loads(self.output.read_text(encoding="utf-8")))
        self.assertEqual(self.config["task_id"], self.inspector.call_args.args[1]["goal_id"])
        self.assertEqual(chat.MARKER, self.collector.call_args.args[2])

    def test_long_task_observation_does_not_reuse_send_expiry(self):
        self.config["expires_at"] = "2000-01-01T00:00:00Z"
        self.assertTrue(self.observe()["response_observed"])

    def test_all_receipt_identity_fields_must_match(self):
        for key in chat.IDENTITY:
            with self.subTest(key=key):
                saved = self.receipt[key]
                self.receipt[key] = "different"
                with self.assertRaisesRegex(ValueError, "receipt_identity_mismatch"):
                    self.observe()
                self.receipt[key] = saved
        self.assertEqual(0, self.sessions)

    def test_unknown_send_cannot_open_viewer(self):
        self.receipt["status"] = "unknown"
        with self.assertRaisesRegex(ValueError, "no_verified_sent_chat"):
            self.observe()
        self.assertEqual(0, self.sessions)

    def test_unknown_with_observed_message_allows_read_only_reobservation(self):
        self.receipt.update(status="unknown", message_observed=True)
        result = self.observe()
        self.assertTrue(result["response_observed"])
        self.assertFalse(result["business_verified"])
        self.assertEqual("unknown", self.receipt["status"])
        self.assertNotIn("status", result)
        self.inspector.assert_called_once()
        self.assertEqual([("_rpc", ("initialize",), {}), ("identity", (), {})], self.browser.mock_calls)
        self.assertEqual("test-persistent-connection", result["connection_id"])

    def test_unknown_requires_boolean_true_message_proof(self):
        self.receipt["status"] = "unknown"
        for proof in (False, "true", 1, None):
            with self.subTest(proof=proof):
                self.receipt["message_observed"] = proof
                with self.assertRaisesRegex(ValueError, "no_verified_sent_chat"):
                    self.observe()
        self.assertEqual(0, self.sessions)

    def test_unknown_observation_still_requires_fresh_message_binding(self):
        self.receipt.update(status="unknown", message_observed=True)
        self.inspector.return_value = {"bound": False, "url": self.url}
        self.assertFalse(self.observe()["response_observed"])
        self.collector.assert_not_called()

    def test_blocked_status_remains_ineligible_even_with_message_proof(self):
        self.receipt.update(status="blocked", message_observed=True)
        with self.assertRaisesRegex(ValueError, "no_verified_sent_chat"):
            self.observe()
        self.assertEqual(0, self.sessions)

    def test_noncanonical_or_other_origin_url_rejected(self):
        for target in ("https://example.com/c/12345678-abcd", self.url + "?extra=1"):
            self.receipt["target_url"] = target
            with self.assertRaisesRegex(ValueError, "no_verified_sent_chat"):
                self.observe()
        self.assertEqual(0, self.sessions)

    def test_stop_pause_and_external_cancel_prevent_viewer(self):
        for mode in ("stopped", "paused"):
            self.state["mode"] = mode
            self.assertEqual("scheduler_not_running", self.observe()["response_reason"])
        self.state["mode"] = "running"
        self.assertEqual("observation_cancelled", self.observe(cancelled=lambda: True)["response_reason"])
        self.assertEqual(0, self.sessions)

    def test_generation_change_during_binding_stops_collection(self):
        def changed(*_):
            self.state["generation"] = 5
            return {"bound": False, "url": self.url}
        self.inspector.side_effect = changed
        self.assertFalse(self.observe()["response_observed"])
        self.collector.assert_not_called()

    def test_absent_exact_message_is_bounded_to_30_seconds(self):
        self.inspector.return_value = {"bound": False, "url": self.url}
        result = self.observe()
        self.assertEqual("exact_message_not_observed", result["response_reason"])
        self.assertEqual(30, self.now)
        self.collector.assert_not_called()

    def test_wrong_chat_is_never_collected(self):
        self.inspector.return_value = {"bound": True, "url": "https://chatgpt.com/c/99999999-abcd"}
        self.assertFalse(self.observe()["response_observed"])
        self.collector.assert_not_called()

    def test_errors_never_persist_raw_transport_details(self):
        self.inspector.side_effect = RuntimeError("secret connection token example")
        result = self.observe()
        self.assertEqual("observation_RuntimeError", result["response_reason"])
        self.assertNotIn("secret", self.output.read_text(encoding="utf-8"))

    def test_internal_delivery_parameter_never_changes_persistent_config(self):
        from host_enrollment import EnrollmentDelivery
        grant = "wc_en_" + "a" * 64
        delivery = EnrollmentDelivery("wc_enrollment_" + "b" * 32, grant,
                                      int(time.time() * 1000) + 60000)
        original = dict(self.config)
        with patch.object(chat.pilot, "run", return_value={"status": "sent"}) as run:
            chat.send(self.config, self.root, enrollment_delivery=delivery, runtime_root=self.runtime)
        self.assertIs(run.call_args.kwargs["enrollment_delivery"], delivery)
        self.assertEqual(run.call_args.kwargs["runtime_root"], self.runtime)
        self.assertEqual(self.config, original)
        self.assertNotIn(grant, json.dumps(run.call_args.args[0]))

    def test_internal_delivery_provider_is_forwarded_without_being_called(self):
        provider = Mock()
        original = dict(self.config)
        with patch.object(chat.pilot, "run", return_value={"status": "sent"}) as run:
            chat.send(self.config, self.root, enrollment_delivery_provider=provider, runtime_root=self.runtime)
        self.assertIs(run.call_args.kwargs["enrollment_delivery_provider"], provider)
        self.assertEqual(run.call_args.kwargs["runtime_root"], self.runtime)
        self.assertEqual(self.config, original)
        provider.assert_not_called()

    def test_direct_and_deferred_delivery_are_mutually_exclusive_before_transport(self):
        from host_enrollment import EnrollmentDelivery
        delivery = EnrollmentDelivery("wc_enrollment_" + "a" * 32, "wc_en_" + "b" * 64,
                                      int(time.time() * 1000) + 60000)
        provider = Mock()
        with patch.object(chat.pilot, "run") as run:
            with self.assertRaisesRegex(ValueError, "enrollment_delivery_ambiguous"):
                chat.send(self.config, self.root, enrollment_delivery=delivery,
                          enrollment_delivery_provider=provider, runtime_root=self.runtime)
            run.assert_not_called()
        provider.assert_not_called()

    def test_late_candidate_echo_is_omitted_before_disk_and_return(self):
        for grant in ("wc_en_" + "a" * 64, "wc_hc_" + "b" * 64):
            with self.subTest(kind=grant[:6]):
                self.collector.return_value = {"response_observed": True,
                    "response_text": "whole answer must be omitted " + grant}
                result = self.observe()
                self.assertNotIn("response_text", result)
                self.assertFalse(result["response_observed"])
                self.assertTrue(result["response_omitted"])
                self.assertEqual(result["response_reason"], "sensitive_candidate_omitted")
                self.assertNotIn(grant, self.output.read_text(encoding="utf-8"))
                self.assertNotIn("whole answer", self.output.read_text(encoding="utf-8"))

    def test_send_rejects_secret_in_ordinary_config_before_transport(self):
        self.config["prompt"] += " wc_en_" + "a" * 64
        with patch.object(chat.pilot, "run") as run:
            with self.assertRaisesRegex(ValueError, "launch_claim_must_not_be_persisted"):
                chat.send(self.config, self.root, runtime_root=self.runtime)
            run.assert_not_called()

    def test_secret_in_error_does_not_reach_observation_artifacts(self):
        grant = "wc_en_" + "a" * 64
        self.inspector.side_effect = RuntimeError(grant)
        result = self.observe()
        self.assertEqual(result["response_reason"], "observation_RuntimeError")
        self.assertNotIn(grant, self.output.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
