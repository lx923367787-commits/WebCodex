import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import auto_handoff as ah
import handoff_controller as hc
import run_auto_handoff as runner


ORIGIN = "http://127.0.0.1:19876"
SECRET = "test-only-fake-credential-do-not-log"


class Clock:
    def __init__(self):
        self.now = 0.0

    def clock(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class FakeApi:
    def __init__(self, origin, token, timeout):
        self.origin, self.token, self.timeout = origin, token, timeout
        self.calls = 0

    def discover(self):
        self.calls += 1
        return []


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.db = self.root / "runtime" / "scheduler.sqlite3"
        self.desktop = self.root / "desktop"
        self.desktop.mkdir()
        self.token_path = self.desktop / "local-connections" / "user.token"
        self.token_path.parent.mkdir()
        self.token_path.write_text(SECRET, encoding="utf-8")
        self.state = self.desktop / "desktop-state.json"
        self.write_state()
        self.time = Clock()
        self.instances = []

    def tearDown(self):
        self.temp.cleanup()

    def write_state(self, origin=ORIGIN, token_path=None):
        self.state.write_text(json.dumps({"runtime": {"server_url": origin,
            "user_token_file": str(token_path or self.token_path)}}), encoding="utf-8")

    def factory(self, origin, token, timeout):
        instance = FakeApi(origin, token, timeout)
        self.instances.append(instance)
        return instance

    def run_once(self):
        # Independent test lock path; no production scheduler or GUI is started.
        with patch.object(ah, "ROOT", self.root):
            return runner.run(self.db, self.state, api_factory=self.factory, launcher=lambda *_: None,
                once=True, clock=self.time.clock, sleep=self.time.sleep, wall_clock=lambda: 1000,
                credentials_loader=lambda: runner.load_credentials(self.state))

    def test_exact_desktop_credentials_remain_in_memory_and_not_environment_or_logs(self):
        before = dict(os.environ)
        output, errors = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            self.assertEqual(self.run_once(), 0)
        self.assertEqual((self.instances[0].origin, self.instances[0].token), (ORIGIN, SECRET))
        self.assertEqual(os.environ, before)
        self.assertNotIn(SECRET, output.getvalue() + errors.getvalue())
        for path in self.db.parent.glob("*"):
            if path.is_file():
                self.assertNotIn(SECRET.encode(), path.read_bytes(), str(path))
        self.assertEqual(runner.status(self.db)["armed_at"], 1000)

    def test_remote_endpoint_rejected_without_api_connection(self):
        self.write_state(origin="https://example.invalid")
        self.assertEqual(self.run_once(), 2)
        self.assertEqual(self.instances, [])
        self.assertNotIn(SECRET, runner.sidecar(self.db, "log").read_text())

    def test_failed_handoff_status_and_log_report_preserved_stop_without_private_fields(self):
        store = ah.Store(self.db)
        store.initialize(ORIGIN)
        try:
            self.assertNotIn("handoff_failure", runner.summary(store))
            with store.db:
                store.db.execute("""INSERT INTO dispatch(project,goal_id,source_context_epoch,
                    checkpoint_sequence,checkpoint_revision,source_fence,retired_at,deadline_at,
                    launch_id,status,reason,snapshot) VALUES(?,?,?,1,1,1,0,0,?,'failed',?,?)""",
                    ("test-project", "test-goal", "test-source", "test-launch", SECRET, SECRET))
            notice = "交接失败，已停止执行，已保留可人工恢复的状态。只有用户明确要求恢复后才能重新启动。"
            self.assertEqual(notice, runner.status(self.db)["handoff_failure"])
            logger = MagicMock()
            runner.log_event(logger, "state", store)
            logged = json.loads(logger.info.call_args.args[0])
            self.assertEqual(notice, logged["handoff_failure"])
            self.assertEqual({"failed": 1}, logged["dispatch_counts"])
            self.assertNotIn(SECRET, logger.info.call_args.args[0])
        finally:
            store.close()

    def test_token_file_outside_desktop_state_rejected(self):
        outside = self.root / "another.token"
        outside.write_text(SECRET)
        self.write_state(token_path=outside)
        self.assertEqual(self.run_once(), 2)
        self.assertEqual(self.instances, [])

    def test_new_connection_cannot_reuse_existing_runner_identity(self):
        self.assertEqual(self.run_once(), 0)
        changed = self.desktop / "local-connections" / "another-user.token"
        changed.write_text("different-test-principal")
        self.write_state(token_path=changed)
        self.assertEqual(self.run_once(), 2)
        self.assertEqual(len(self.instances), 1)

    def test_paused_and_stopped_modes_survive_initial_run_without_api_reads(self):
        store = ah.Store(self.db)
        store.initialize(ORIGIN, armed_at=900)
        try:
            for mode in ("paused", "stopped"):
                store.set_mode(mode)
                prior = store.status()
                self.assertEqual(self.run_once(), 2)
                self.assertEqual(store.status()["mode"], mode)
                self.assertEqual(store.status()["generation"], prior["generation"])
                self.assertEqual(store.status()["armed_at"], 900)
        finally:
            store.close()
        self.assertEqual(self.instances, [])

    def test_readiness_has_one_absolute_120_second_deadline(self):
        store = ah.Store(self.db)
        store.initialize(ORIGIN)
        time = self.time
        class Unready:
            timeout = 5
            calls = 0
            def discover(inner):
                inner.calls += 1
                time.now += inner.timeout
                raise hc.ReadFailure()
        api = Unready()
        try:
            self.assertFalse(runner.wait_ready(store, api, clock=time.clock, sleep=time.sleep))
        finally:
            store.close()
        self.assertEqual(time.now, 120)
        self.assertLess(api.calls, 30)

    def test_stop_during_readiness_is_preserved_and_ends_wait(self):
        store = ah.Store(self.db)
        store.initialize(ORIGIN)
        class StopsOnRead:
            timeout = 5
            def discover(inner):
                store.set_mode("stopped")
                raise hc.ReadFailure()
        try:
            self.assertFalse(runner.wait_ready(store, StopsOnRead(), clock=self.time.clock, sleep=self.time.sleep))
            self.assertEqual(store.status()["mode"], "stopped")
            self.assertLess(self.time.now, 1)
        finally:
            store.close()

    def test_status_rejects_reused_pid_by_process_creation_identity(self):
        runner.write_json(runner.sidecar(self.db, "runner.json"),
            {"pid": 123, "process_stamp": "old", "active": True, "state": "running"})
        with patch.object(runner, "process_stamp", return_value="new"):
            self.assertFalse(runner.status(self.db)["runner"]["pid_alive"])
        with patch.object(runner, "process_stamp", return_value="old"):
            self.assertTrue(runner.status(self.db)["runner"]["pid_alive"])

    def test_stop_command_uses_existing_store_without_credentials(self):
        store = ah.Store(self.db)
        store.initialize(ORIGIN)
        store.close()
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = runner.main(["stop", "--db", str(self.db), "--desktop-state", str(self.root / "missing.json")])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue())["mode"], "stopped")
        self.assertNotIn(SECRET, output.getvalue())

    def test_keyboard_interrupt_preserves_stop_racing_with_pause_write(self):
        control = ah.Store(self.db)
        control.initialize(ORIGIN)
        stopped_generation = []
        def interrupt(store, *_):
            connection = store.db
            proxy = MagicMock(wraps=connection)
            proxy.__enter__.side_effect = connection.__enter__
            proxy.__exit__.side_effect = connection.__exit__
            def execute(sql, *args):
                if sql.startswith("UPDATE scheduler SET mode=") and not stopped_generation:
                    control.set_mode("stopped")
                    stopped_generation.append(control.status()["generation"])
                return connection.execute(sql, *args)
            proxy.execute.side_effect = execute
            store.db = proxy
            raise KeyboardInterrupt()
        try:
            with patch.object(ah, "poll_once", side_effect=interrupt):
                self.assertEqual(self.run_once(), 130)
            self.assertEqual(stopped_generation, [1])
            self.assertEqual(control.status()["mode"], "stopped")
            self.assertEqual(control.status()["generation"], stopped_generation[0])
        finally:
            control.close()

    def test_raw_exception_never_reaches_log_or_output(self):
        def broken_api(*args, **kwargs):
            raise RuntimeError("simulated error contains " + SECRET)
        output, errors = io.StringIO(), io.StringIO()
        with patch.object(ah, "ROOT", self.root), contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            code = runner.run(self.db, self.state, api_factory=broken_api, once=True,
                credentials_loader=lambda: runner.load_credentials(self.state))
        self.assertEqual(code, 2)
        self.assertNotIn(SECRET, output.getvalue() + errors.getvalue() + runner.sidecar(self.db, "log").read_text())


if __name__ == "__main__":
    unittest.main()
