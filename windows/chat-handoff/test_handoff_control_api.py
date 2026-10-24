"""Synthetic control contracts; no production configuration or credentials."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import handoff_control_api as control
import handoff_controller as hc

ENV = {"WEBCODEX_HANDOFF_CONTROL_ENDPOINT": "http://127.0.0.1:19876",
       "WEBCODEX_HANDOFF_CONTROL_TOKEN": "synthetic-control-only",
       "WEBCODEX_HANDOFF_OWNER_USER": "owner-test", "WEBCODEX_HANDOFF_PROJECT": "project-test"}
CANDIDATE = {"project": "project-test", "goal_id": "goal-test", "agent_id": "agent-test",
             "source_context_epoch": "source-test", "checkpoint_sequence": 3}


class ControlApiTests(unittest.TestCase):
    def api(self):
        return control.Api(ENV["WEBCODEX_HANDOFF_CONTROL_ENDPOINT"], "synthetic-control-only", environ=ENV)

    def test_no_implicit_desktop_user_or_read_token_fallback(self):
        for field in ("WEBCODEX_HANDOFF_CONTROL_ENDPOINT", "WEBCODEX_HANDOFF_CONTROL_TOKEN", "WEBCODEX_HANDOFF_OWNER_USER", "WEBCODEX_HANDOFF_PROJECT"):
            values = {**ENV, "WEBCODEX_READ_TOKEN": "old-user-token"}
            values.pop(field)
            with self.subTest(field=field), self.assertRaises(ValueError):
                control.load_credentials(values)

    def test_explicit_file_credential_never_in_connection_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "control.token"
            path.write_text("synthetic-file-secret")
            values = dict(ENV)
            values.pop("WEBCODEX_HANDOFF_CONTROL_TOKEN")
            values["WEBCODEX_HANDOFF_CONTROL_TOKEN_FILE"] = str(path)
            origin, token, identity = control.load_credentials(values)
            self.assertEqual("synthetic-file-secret", token)
            self.assertNotIn(token, identity)
            self.assertEqual(ENV["WEBCODEX_HANDOFF_CONTROL_ENDPOINT"], origin)
            values["WEBCODEX_HANDOFF_CONTROL_TOKEN"] = "ambiguous"
            with self.assertRaises(ValueError):
                control.load_credentials(values)

    def test_request_uses_only_control_route(self):
        api = self.api()
        response = MagicMock()
        response.__enter__.return_value = response
        response.read.return_value = b'{"candidates":[]}'
        api.opener = MagicMock()
        api.opener.open.return_value = response
        self.assertEqual([], api.discover())
        request = api.opener.open.call_args.args[0]
        self.assertEqual(ENV["WEBCODEX_HANDOFF_CONTROL_ENDPOINT"] + control.CONTROL_PATH, request.full_url)
        payload = json.loads(request.data)
        self.assertEqual("handoff_list", payload["action"])
        self.assertNotIn("token", json.dumps(payload))

    def test_discover_retains_agent_for_exact_status_and_cas(self):
        api = self.api()
        with patch.object(api, "post", side_effect=[{"candidates": [CANDIDATE]}, {"handoff": {}}, {"handoff": {}}]) as post:
            self.assertEqual([CANDIDATE], api.discover())
            api.read(CANDIDATE)
            api.call("goal_handoff", {"action": "begin_attempt", "goal_id": "goal-test", "handoff_id": "handoff-test", "attempt_id": "old-attempt"})
        self.assertEqual("agent-test", post.call_args.args[0]["agent_id"])
        self.assertEqual("old-attempt", post.call_args.args[0]["expected_attempt_id"])

    def test_restart_status_can_use_persisted_agent_binding(self):
        api = self.api()
        with patch.object(api, "post", return_value={}) as post:
            api.read(CANDIDATE)
        self.assertEqual("agent-test", post.call_args.args[0]["agent_id"])

    def test_other_project_or_agent_and_unscoped_goal_are_rejected(self):
        api = self.api()
        with self.assertRaises(hc.ReadFailure):
            api.read({**CANDIDATE, "project": "other-project"})
        with self.assertRaises(hc.ReadFailure):
            api.call("goal_handoff", {"goal_id": "unknown", "action": "status"})
        api.agent_id = "only-agent"
        with self.assertRaises(hc.ReadFailure):
            api.read(CANDIDATE)

    def test_model_business_tools_are_not_control_operations(self):
        api = self.api()
        for name, action in (("goal_read", "status"), ("goal_handoff", "prepare"), ("goal_handoff", "commit")):
            with self.subTest(name=name, action=action), self.assertRaises(hc.ReadFailure):
                api.call(name, {"action": action})

    def test_control_worker_inherits_explicit_config_but_browser_environment_does_not(self):
        import edge_tools
        import playwright_extension_browser as browser
        with tempfile.TemporaryDirectory() as temp, patch.dict("os.environ", ENV, clear=True), \
                patch.object(edge_tools.subprocess, "Popen", return_value=MagicMock(pid=123)) as spawn:
            edge_tools.spawn_worker(temp, "3d5c6a40-46da-487d-9c25-f9e34877e668")
        child = spawn.call_args.kwargs["env"]
        self.assertEqual(ENV["WEBCODEX_HANDOFF_CONTROL_TOKEN"], child["WEBCODEX_HANDOFF_CONTROL_TOKEN"])
        self.assertNotIn(ENV["WEBCODEX_HANDOFF_CONTROL_TOKEN"], str(spawn.call_args.args))
        self.assertTrue(set(ENV).isdisjoint(browser._ENV))


if __name__ == "__main__":
    unittest.main()
