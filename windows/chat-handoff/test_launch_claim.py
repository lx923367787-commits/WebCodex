"""Native claim delivery through the real flow and a fake browser, entirely offline."""
import contextlib
import datetime
import io
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import MagicMock, patch

import auto_handoff
import edge_adapter as adapter
import edge_handoff
import edge_tools
import handoff_controller as hc
import handoff_control_api
import handoff_flow as flow
from test_handoff_control_api import ENV

GRANT = "wc_hc_" + "7e" * 32


class Element:
    def __init__(self, browser, send=False):
        self.browser, self.send = browser, send

    def click(self, **options):
        callback = options.get("before_actual")
        if callback and not callback():
            raise RuntimeError("stopped")
        if self.send:
            self.browser.sent.append(self.browser.text)
            if self.browser.fail_send:
                raise RuntimeError("sensitive simulated transport failure " + GRANT)

    def clipboard_input(self, text, **_):
        self.browser.text += text


class Browser:
    def __init__(self):
        self.text, self.sent, self.mention, self.fail_send = "", [], False, False

    def activate(self):
        pass

    def identity(self):
        return {"owned_page_id": "test-owned-page", "origin": "https://chatgpt.com", "scope": "created_for_handoff"}

    def find_by_xpath(self, xpath, **_):
        return Element(self, xpath == "//send")

    def keys(self, value, **_):
        if value == "@webcodex":
            self.text, self.mention = "webcodex ", True


class LaunchClaimTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = patch.dict(os.environ, WEBCODEX_HANDOFF_RUNTIME=str(self.root))
        self.env.start()
        self.addCleanup(self.env.stop)
        self.request = {"launch_id": "68efcd7a-ff7c-4b9f-9d80-675e7888e302", "project": "project-test",
            "goal_id": "goal-test", "agent_id": "agent-test", "source_context_epoch": "source-test",
            "checkpoint_sequence": 2, "checkpoint_revision": 3, "handoff_id": "handoff-test",
            "attempt_id": "68efcd7a-ff7c-4b9f-9d80-675e7888e302", "attempt_number": 1,
            "expires_at": (flow.utc_now() + datetime.timedelta(minutes=5)).isoformat(),
            "recovery_prompt": "Continue exact task using the one-time launch_claim in prepare."}
        # The real protocol uses integer milliseconds.
        expiry = round(flow.parse_time(self.request["expires_at"]).timestamp() * 1000)
        self.request["expires_at"] = auto_handoff.utc_iso(expiry / 1000)
        self.path = self.root / "requests" / (self.request["launch_id"] + ".json")
        flow.write_json(self.path, self.request)
        self.browser = Browser()
        self.edge = MagicMock()
        self.edge.create.return_value = self.browser
        session = MagicMock()
        session.__enter__.return_value = self.edge
        self.api = MagicMock()
        self.api.issue_launch_claim.return_value = {key: self.request[key] for key in ("project", "goal_id", "agent_id", "handoff_id", "attempt_id")}
        self.api.issue_launch_claim.return_value.update(claim_grant=GRANT, expires_at_ms=expiry)
        self.api.call.return_value = {"handoff": {"status": "awaiting_ready"}}
        self.patches = contextlib.ExitStack()
        self.addCleanup(self.patches.close)
        self.patches.enter_context(patch.object(adapter, "_control_api", return_value=self.api))
        self.patches.enter_context(patch.dict("sys.modules", {"playwright_extension_browser": types.SimpleNamespace(persistent_session=lambda: session)}))
        self.patches.enter_context(patch.object(edge_handoff, "configure_chat", return_value={"profile": "test"}))
        self.patches.enter_context(patch.object(edge_handoff, "composer", side_effect=lambda b: {
            "count": 1, "xpath": "//editor", "text": b.text, "mention": b.mention, "user_count": 0,
            "focused": True, "document_focused": True, "document_visible": True,
            "send_enabled": True, "send_xpath": "//send"}))
        self.patches.enter_context(patch.object(flow, "after_send", side_effect=self.after_send))

    def after_send(self, browser):
        self.assertNotIn(GRANT, json.dumps(flow._ACTIVE_REQUEST))
        self.assertIs(browser, self.browser)
        return flow.save_result(flow._ACTIVE_REQUEST, {"status": "sent", "message_observed": True})

    def assert_no_persisted_grant(self):
        for file in self.root.rglob("*"):
            self.assertNotIn(GRANT, file.name)
            if file.is_file():
                self.assertNotIn(GRANT.encode(), file.read_bytes(), str(file))
                self.assertNotIn(file.suffix.lower(), (".png", ".jpg", ".jpeg", ".zip", ".html"))

    def test_real_message_flow_delivers_grant_only_to_owned_chat_not_request_or_receipts(self):
        before = self.path.read_bytes()
        bootstrap = "synthetic-bootstrap-must-not-enter-chat"
        old_run = "wc_ar_" + "c" * 64
        # Unexpected upstream fields must never be rendered as recovery text.
        self.api.issue_launch_claim.return_value.update(control_token=bootstrap, run_token=old_run)
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = adapter.launch(self.path)
            repeated = adapter.launch(self.path)
        self.assertEqual("sent", result["status"])
        self.assertEqual(result, repeated)
        self.assertEqual(1, len(self.browser.sent))
        self.assertIn("launch_claim=" + GRANT, self.browser.sent[0])
        self.assertNotIn(bootstrap, self.browser.sent[0])
        self.assertNotIn(old_run, self.browser.sent[0])
        self.edge.create.assert_called_once_with("https://chatgpt.com/")
        self.api.issue_launch_claim.assert_called_once()
        self.assertEqual(before, self.path.read_bytes())
        self.assertNotIn(GRANT, json.dumps(result) + stdout.getvalue() + stderr.getvalue())
        self.assertIsNone(flow._ACTIVE_REQUEST)
        self.assert_no_persisted_grant()
        for file in self.root.rglob("*"):
            if file.is_file():
                self.assertNotIn(bootstrap.encode(), file.read_bytes())
                self.assertNotIn(old_run.encode(), file.read_bytes())

    def test_native_missing_grant_never_opens_chat_or_reissues_same_attempt(self):
        self.api.issue_launch_claim.return_value.pop("claim_grant")
        for _ in range(2):
            self.assertEqual("unknown", adapter.launch(self.path)["status"])
        self.edge.create.assert_not_called()
        self.api.issue_launch_claim.assert_called_once()
        self.api.call.assert_called_once()
        self.assert_no_persisted_grant()

    def test_removing_agent_identity_cannot_downgrade_native_launch_to_legacy(self):
        self.request.pop("agent_id")
        flow.write_json(self.path, self.request)
        self.assertEqual("blocked", adapter.launch(self.path)["status"])
        self.edge.create.assert_not_called()
        self.api.issue_launch_claim.assert_not_called()

    def test_lost_issue_response_checks_committed_before_any_open(self):
        self.api.issue_launch_claim.side_effect = hc.ReadFailure()
        self.api.call.return_value = {"handoff": {"status": "committed", "handoff_id": self.request["handoff_id"],
            "goal_id": self.request["goal_id"], "project": self.request["project"]}}
        result = adapter.launch(self.path)
        self.assertEqual("handoff_already_committed", result["reason"])
        self.edge.create.assert_not_called()
        self.api.call.assert_called_once()
        self.assert_no_persisted_grant()

    def test_wrong_binding_or_expired_grant_never_opens_chat(self):
        self.api.issue_launch_claim.return_value["agent_id"] = "other-agent"
        self.assertEqual("unknown", adapter.launch(self.path)["status"])
        self.edge.create.assert_not_called()
        self.assert_no_persisted_grant()

    def test_transport_exception_containing_secret_never_reaches_output_or_disk(self):
        self.browser.fail_send = True
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = adapter.launch(self.path)
        self.assertEqual("unknown", result["status"])
        self.assertNotIn(GRANT, json.dumps(result) + stdout.getvalue() + stderr.getvalue())
        self.assert_no_persisted_grant()

    def test_all_persisted_record_writers_reject_a_raw_claim(self):
        for writer in (flow.write_json, edge_tools.write_json, auto_handoff.write_request):
            target = self.root / (writer.__module__ + ".json")
            with self.subTest(writer=writer.__module__), self.assertRaises(ValueError):
                writer(target, {"reason": GRANT})
            self.assertFalse(target.exists())
        self.assertEqual("safe", edge_tools.safe_reason(GRANT, "safe"))
        for invalid in ("wc_hc_" + "7" * 32, "wc_hc_" + "7" * 65, "wc_hc_" + "A" * 64, "wc_hc_" + "z" * 64):
            self.assertFalse(flow.valid_launch_claim(invalid))
        with self.assertRaises(ValueError):
            flow.prepare_message(self.request)
        self.assert_no_persisted_grant()

    def test_control_issue_is_exact_single_call_and_does_not_mutate_request(self):
        api = handoff_control_api.Api(ENV["WEBCODEX_HANDOFF_CONTROL_ENDPOINT"], "synthetic-control", environ=ENV)
        with patch.object(api, "post", return_value={"claim_grant": GRANT}) as post:
            result = api.issue_launch_claim(self.request)
        self.assertEqual(GRANT, result["claim_grant"])
        self.assertEqual("handoff_launch_claim", post.call_args.args[0]["action"])
        self.assertEqual(self.request["attempt_id"], post.call_args.args[0]["expected_attempt_id"])
        self.assertNotIn(GRANT, json.dumps(post.call_args.args[0]) + json.dumps(self.request))

    def test_extension_configuration_keeps_capture_off_and_grants_out_of_environment(self):
        # Build a synthetic package/config; this calls no subprocess or browser.
        self.patches.close()
        import playwright_extension_browser as browser
        package = self.root / "extension-runtime/node_modules/@playwright/mcp"
        package.mkdir(parents=True)
        (package / "package.json").write_text('{"version":"0.0.83"}')
        (package / "cli.js").write_text("// synthetic")
        node = self.root / "node.exe"
        node.write_bytes(b"not executable")
        (self.root / "runtime").mkdir()
        (self.root / "runtime/playwright-extension-token.txt").write_text("e" * 43)
        with patch.object(browser, "ROOT", self.root), patch.object(browser, "NODE", node), \
                patch.dict(os.environ, {"CLAIM": GRANT, "WEBCODEX_HANDOFF_CONTROL_TOKEN": "synthetic-bootstrap", "PLAYWRIGHT_MCP_SAVE_SESSION": "1", "DEBUG": "pw:*"}):
            config = browser._configuration()
        self.assertNotIn(GRANT, json.dumps(config))
        self.assertNotIn("synthetic-bootstrap", json.dumps(config))
        self.assertNotIn("--save-session", config["args"])
        self.assertNotIn("--config", config["args"])
        self.assertNotIn("DEBUG", config["env"])
        self.assertNotIn("PLAYWRIGHT_MCP_SAVE_SESSION", config["env"])
        self.assertEqual("none", config["args"][config["args"].index("--snapshot-mode") + 1])
        self.assertEqual("omit", config["args"][config["args"].index("--image-responses") + 1])
        self.assert_no_persisted_grant()


if __name__ == "__main__":
    unittest.main()
