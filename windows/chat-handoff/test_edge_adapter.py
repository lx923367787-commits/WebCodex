"""No network: verify the durable input boundary, concurrency and migration."""
import contextlib
import datetime
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest import mock

import edge_adapter as adapter
import handoff_flow as flow


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = mock.patch.dict(os.environ, WEBCODEX_HANDOFF_RUNTIME=str(self.root))
        self.env.start()
        self.addCleanup(self.env.stop)
        self.request = dict(launch_id="68efcd7a-ff7c-4b9f-9d80-675e7888e302", project="project-test",
            goal_id="goal-test", source_context_epoch="epoch-test", checkpoint_sequence=2,
            checkpoint_revision=3, recovery_prompt="continue exact test",
            expires_at=(flow.utc_now()+datetime.timedelta(minutes=5)).isoformat())
        self.path = self.root/"requests"/(self.request["launch_id"]+".json")
        flow.write_json(self.path, self.request)
        self.edge = mock.Mock()
        self.connection = mock.MagicMock()
        self.connection.__enter__.return_value = self.edge
        self.module = types.SimpleNamespace(persistent_session=mock.Mock(return_value=self.connection),
                                            discover_endpoint=mock.Mock(return_value="local"))
        self.run = mock.Mock(side_effect=lambda *a: flow.save_result(self.request, {"status":"sent"}))
        self.modules = mock.patch.dict("sys.modules", {"playwright_extension_browser":self.module,
            "edge_handoff":types.SimpleNamespace(run=self.run)})
        self.modules.start()
        self.addCleanup(self.modules.stop)

    def launch(self, should_stop=lambda:False):
        return adapter.launch(self.path, should_stop)

    def test_success_one_new_tab_and_repeat_returns_correlated_receipt(self):
        result = self.launch()
        self.assertEqual(result["status"], "sent")
        self.assertEqual(result["goal_id"], self.request["goal_id"])
        self.assertEqual(self.launch(), result)
        self.edge.create.assert_called_once_with("https://chatgpt.com/")
        self.run.assert_called_once()

    def test_durable_intent_precedes_new_page(self):
        def create(*args):
            value = json.loads((self.root/"edge-launches"/(self.request["launch_id"]+".json")).read_text())
            self.assertEqual(value["status"], "issued")
            self.assertEqual(value["goal_id"], self.request["goal_id"])
            raise RuntimeError("page creation outcome unknown")
        self.edge.create.side_effect = create
        self.assertEqual(self.launch()["status"], "unknown")
        self.assertEqual(self.launch()["status"], "unknown")
        self.edge.create.assert_called_once()

    def test_old_or_new_issued_or_send_intent_never_replays(self):
        for folder in ("edge-launches", "launches", "send-intents"):
            with self.subTest(folder=folder):
                file=self.root/folder/(self.request["launch_id"]+".json")
                flow.write_json(file, {"status":"issued"})
                self.assertEqual(self.launch()["status"], "unknown")
                self.edge.create.assert_not_called()
                file.unlink()

    def test_stop_and_expiry_before_connect_are_retryable_without_intent(self):
        result=self.launch(lambda:True)
        self.assertEqual((result["status"],result["launched"]),("retryable",False))
        self.request["expires_at"]="2000-01-01T00:00:00Z"
        flow.write_json(self.path,self.request)
        self.assertEqual(self.launch()["status"],"retryable")
        self.module.persistent_session.assert_not_called()
        self.assertFalse((self.root/"edge-launches").exists())

    def test_stop_during_connection_does_not_open_page(self):
        checks=iter([False,True])
        self.assertEqual(self.launch(lambda:next(checks))["status"],"retryable")
        self.edge.create.assert_not_called()
        self.assertFalse((self.root/"edge-launches").exists())

    def test_connection_failure_may_retry_without_changing_request(self):
        self.module.persistent_session.side_effect=RuntimeError("private endpoint detail")
        result=self.launch()
        self.assertEqual(result,{"status":"retryable","reason":"edge_connection_unavailable","launched":False})
        self.assertFalse((self.root/"edge-launches").exists())
        self.module.persistent_session.side_effect=None
        self.assertEqual(self.launch()["status"],"sent")

    def test_concurrent_owner_does_not_create_tab(self):
        @contextlib.contextmanager
        def busy(*args):
            raise ValueError("owned")
            yield
        with mock.patch.object(adapter.hc,"watcher_lock",busy):
            self.assertEqual(self.launch()["status"],"retryable")
        self.edge.create.assert_not_called()

    def test_cancellation_receipt_blocks_before_connect(self):
        flow.write_json(self.root/"cancellations"/(self.request["launch_id"]+".json"),{})
        self.assertEqual(self.launch()["reason"],"request_cancelled")
        self.module.persistent_session.assert_not_called()

    def test_wrong_receipt_identity_is_not_accepted(self):
        receipt=flow.save_result(self.request,{"status":"sent"})
        receipt["goal_id"]="other-goal"
        flow.write_json(flow.result_path(self.request),receipt)
        self.assertIsNone(adapter._result(self.request))
        self.assertEqual(self.launch()["reason"],"receipt_identity_or_format_mismatch")
        self.edge.create.assert_not_called()
        self.assertEqual(json.loads(flow.result_path(self.request).read_text()),receipt)

    def test_inspect_only_reads_configuration_and_receipt(self):
        result=adapter.inspect(self.request)
        self.assertEqual(result["state"],"not_started")
        self.assertTrue(result["endpoint_available"])
        self.module.persistent_session.assert_not_called()
        self.edge.create.assert_not_called()

    def test_malformed_request_cannot_connect(self):
        flow.write_json(self.path,{"launch_id":"../escape"})
        self.assertEqual(self.launch()["reason"],"invalid_request")
        self.module.persistent_session.assert_not_called()


if __name__ == "__main__":
    unittest.main()
