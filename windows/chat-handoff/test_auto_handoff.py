"""Legacy migration checks; current dispatch scenarios live in test_handoff_reliability."""
import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock

import auto_handoff as ah
import handoff_controller as hc
from test_handoff_reliability import CANDIDATE, FakeApi, ORIGIN, evidence


class LegacyObservationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.store = ah.Store(self.root / "state.sqlite3")
        self.store.initialize(ORIGIN)
        self.api = FakeApi()
        self.api.raw.pop("handoff")
        self.api.raw["source_context"].update(lifecycle_state="retired", retired_at=100000)
        self.launch = Mock()

    def tearDown(self):
        self.store.close()
        self.temp.cleanup()

    def poll(self):
        return ah.poll_once(self.store, self.api, self.launch, clock=lambda: 10000, request_dir=self.root / "requests")

    def test_old_server_only_observed_never_automatically_dispatched(self):
        for _ in range(3):
            self.poll()
        self.launch.assert_not_called()
        self.assertEqual("blocked", self.store.entries()[0]["status"])
        self.assertEqual("handoff_protocol_required", self.store.entries()[0]["reason"])

    def test_legacy_success_receipt_remains_readable_without_new_launch(self):
        self.api.raw["resume_started"] = {"context_epoch": "epoch-B", "identity_source": "window_bound", "started_at": 200000}
        self.poll()
        self.launch.assert_not_called()
        self.assertEqual("resumed", self.store.entries()[0]["status"])

    def test_source_epoch_receipt_is_not_success(self):
        self.api.raw["resume_started"] = {"context_epoch": "epoch-A", "identity_source": "window_bound", "started_at": 200000}
        with self.assertRaises(hc.ReadFailure):
            ah.read_observation(self.api, CANDIDATE)

    def test_unknown_outcomes_and_control_modes_survive_restart(self):
        self.poll()
        with self.store.db:
            self.store.db.execute("UPDATE dispatch SET status='launch_started'")
        self.store.set_mode("stopped")
        self.store.close()
        self.store = ah.Store(self.root / "state.sqlite3")
        self.store.recover_interrupted()
        self.poll()
        self.assertEqual("stopped", self.store.status()["mode"])
        self.assertEqual("unknown", self.store.entries()[0]["status"])
        self.launch.assert_not_called()

    def test_three_network_errors_pause_without_changing_stop(self):
        self.api.discover = Mock(side_effect=hc.ReadFailure("connector_auth"))
        for _ in range(4):
            self.poll()
        self.assertEqual(3, self.api.discover.call_count)
        self.assertEqual("paused", self.store.status()["mode"])
        self.store.set_mode("stopped")
        self.store.pause_if_running()
        self.assertEqual("stopped", self.store.status()["mode"])

    def test_global_desktop_lock_excludes_second_scheduler(self):
        with hc.watcher_lock(self.root / "desktop"):
            with self.assertRaises(ValueError):
                with hc.watcher_lock(self.root / "desktop"):
                    self.fail("second process acquired desktop lock")


if __name__ == "__main__":
    unittest.main()
