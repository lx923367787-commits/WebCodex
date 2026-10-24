"""Offline only: no browser, credentials, network, or production writes."""
import contextlib
import datetime as dt
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
import uuid

import pilot_transport as pilot


class FakeElement:
    def __init__(self, browser, send=False):
        self.browser, self.send = browser, send

    def click(self, before_actual=lambda: True, **kwargs):
        if self.send and self.browser.before_send:
            self.browser.before_send()
        if not before_actual():
            raise RuntimeError("cancelled")
        if self.send:
            self.browser.sends += 1
            if self.browser.fail_send:
                raise OSError("lost acknowledgement")

    def clipboard_input(self, text, **kwargs):
        self.browser.text += text


class FakeBrowser:
    def __init__(self):
        self.text = ""
        self.mention = False
        self.sends = 0
        self.fail_send = False
        self.before_send = None

    def activate(self):
        pass

    def identity(self):
        return {"owned_page_id": "fake"}

    def keys(self, text, **kwargs):
        if text == "@webcodex":
            self.text = "webcodex "
        else:
            self.mention = True

    def find_by_xpath(self, path, **kwargs):
        return FakeElement(self, send=path == "//send")

    def execute_javascript(self, script, argument=None):
        if script != pilot.edge_handoff.COMPOSER_JS:
            raise AssertionError("Unexpected browser operation")
        return {"url": "https://chatgpt.com/", "count": 1, "xpath": "//editor",
                "send_xpath": "//send", "user_count": 0, "text": self.text,
                "mention": self.mention, "focused": True, "document_visible": True,
                "document_focused": True, "send_enabled": True}


class TransportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        launch = str(uuid.uuid4())
        task = "wc_probe_" + uuid.uuid4().hex
        self.config = {"launch_id": launch, "parent_session_id": "parent_test", "task_id": task,
            "prompt": f"Read-only test launch_id={launch} task_id={task}",
            "expires_at": (dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=10)).isoformat()}
        self.browser = FakeBrowser()
        self.state = {"mode": "running", "generation": 4}
        self.locks = []
        self.sessions = 0
        self.approvals = Mock(return_value={"status": "sent", "reason": "observed",
            "target_url": "https://chatgpt.com/c/12345678-abcd", "message_observed": True})
        self.setup = patch.object(pilot.edge_handoff, "configure_chat", return_value={"intelligence": "fake"})
        self.setup.start()
        self.addCleanup(self.setup.stop)

    @contextlib.contextmanager
    def lock(self, path):
        self.locks.append(path)
        yield

    @contextlib.contextmanager
    def session(self):
        self.sessions += 1
        yield Mock(create=Mock(return_value=self.browser))

    def run_pilot(self, **kwargs):
        return pilot.run(self.config, self.root, session_factory=self.session,
            state_reader=lambda: dict(self.state), lock_factory=self.lock,
            approval_runner=self.approvals, **kwargs)

    def test_sent_is_durable_and_not_business_success(self):
        value = self.run_pilot()
        folder = self.root / self.config["launch_id"]
        self.assertEqual("sent", value["status"])
        self.assertFalse(value["business_verified"])
        self.assertEqual(1, self.browser.sends)
        self.assertEqual([pilot.RUNTIME / "edge-dispatch"], self.locks)
        self.assertEqual(value, json.loads((folder / "result.json").read_text(encoding="utf-8")))
        self.assertTrue((folder / "issued.json").exists())
        self.assertTrue((folder / "send-intent.json").exists())
        self.assertEqual(self.config["task_id"], self.approvals.call_args.args[1]["goal_id"])

    def test_duplicate_never_opens_second_session(self):
        self.run_pilot()
        value = self.run_pilot()
        self.assertEqual("launch_id_already_used", value["reason"])
        self.assertEqual(1, self.sessions)

    def test_unknown_send_never_retries(self):
        self.browser.fail_send = True
        self.assertEqual("unknown", self.run_pilot()["status"])
        self.assertEqual("launch_id_already_used", self.run_pilot()["reason"])
        self.assertEqual(1, self.browser.sends)
        self.approvals.assert_not_called()

    def test_paused_and_stopped_do_not_reserve_or_open(self):
        for mode in ("paused", "stopped"):
            self.state["mode"] = mode
            self.assertEqual("scheduler_not_running", self.run_pilot()["reason"])
        self.assertEqual(0, self.sessions)
        self.assertFalse((self.root / self.config["launch_id"]).exists())

    def test_generation_change_at_send_cancels_and_does_not_replay(self):
        self.browser.before_send = lambda: self.state.update(generation=5)
        original_callback = pilot.handoff_flow._SHOULD_STOP
        value = self.run_pilot()
        self.assertEqual("unknown", value["status"])
        self.assertEqual(0, self.browser.sends)
        self.assertIs(original_callback, pilot.handoff_flow._SHOULD_STOP)
        self.assertEqual("launch_id_already_used", self.run_pilot()["reason"])

    def test_existing_partial_reservation_forbids_replay(self):
        (self.root / self.config["launch_id"]).mkdir()
        self.assertEqual("launch_id_already_used", self.run_pilot()["reason"])
        self.assertEqual(0, self.sessions)

    def test_held_production_lock_prevents_session(self):
        with patch.object(self, "lock", side_effect=ValueError("busy")):
            self.assertEqual("blocked", self.run_pilot()["status"])
        self.assertEqual(0, self.sessions)

    def test_invalid_prompt_identity_rejected_before_activity(self):
        self.config["prompt"] = "not bound"
        with self.assertRaises(ValueError):
            self.run_pilot()
        self.assertEqual([], self.locks)

    def test_expiry_prevents_launch(self):
        self.config["expires_at"] = "2000-01-01T00:00:00Z"
        self.assertEqual("stopped_before_launch", self.run_pilot()["reason"])
        self.assertEqual(0, self.sessions)

    def test_only_runtime_status_approval_titles_allowed(self):
        for title, allowed in (("Runtime Status", True), ("runtime_status", True), ("Call Runtime Tool", False)):
            browser = Mock()
            browser.execute_javascript.return_value = {"cards": [{"tool": title, "exact_app": True}]}
            value = pilot.RestrictedApprovals(browser).execute_javascript(pilot.approval_guard.PROBE_JS)
            self.assertEqual(allowed, value["cards"][0]["exact_app"])

    def test_collect_requires_stability_identity_and_no_stop_button(self):
        url = "https://chatgpt.com/c/12345678-abcd"
        text = "WEBCODEX_PILOT_RESULT " + self.config["task_id"]
        browser = Mock()
        browser.execute_javascript.side_effect = [
            {"url": url, "text": text, "stopped": False},
            {"url": url, "text": text, "stopped": True},
            {"url": url, "text": text, "stopped": True}]
        value = pilot.collect_final(browser, self.config["task_id"], "WEBCODEX_PILOT_RESULT",
            url, lambda: False, 10, clock=lambda: 0, sleep=lambda _: None)
        self.assertTrue(value["response_observed"])
        self.assertEqual("stable_candidate_only", value["response_reason"])
        self.assertEqual(3, browser.execute_javascript.call_count)

    def test_collect_rejects_other_conversation(self):
        browser = Mock()
        browser.execute_javascript.return_value = {"url": "https://chatgpt.com/c/99999999-abcd"}
        value = pilot.collect_final(browser, self.config["task_id"], "marker",
            "https://chatgpt.com/c/12345678-abcd", lambda: False, 10, clock=lambda: 0)
        self.assertFalse(value["response_observed"])

    def test_response_read_failure_preserves_sent_receipt(self):
        with patch.object(pilot, "collect_final", side_effect=OSError("lost connection")):
            value = self.run_pilot(collect_response=True)
        self.assertEqual("sent", value["status"])
        self.assertFalse(value["response_observed"])
        self.assertFalse(value["business_verified"])


if __name__ == "__main__":
    unittest.main()
