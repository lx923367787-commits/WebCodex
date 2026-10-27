"""Offline migration checks: synthetic SQLite/state, all HTTP calls mocked."""
import importlib.util
from contextlib import closing
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("native_setup",
    Path(__file__).with_name("configure-native-host.py"))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class NativeHostSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="webcodex-native-setup-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.host = "wc_host_" + "a" * 32
        self.database = self.root / "test.db"
        self.state = self.root / "desktop-state.json"
        self.token = self.root / "bootstrap.token"
        self.token.write_text("synthetic-test-secret", encoding="utf-8")
        self.state.write_text(json.dumps({"runtime": {
            "server_url": "http://127.0.0.1:12345", "native_host_id": self.host,
            "runtime_project_id": "test-project"}}), encoding="utf-8")
        with closing(sqlite3.connect(self.database)) as db, db:
            db.executescript("CREATE TABLE wc_native_agent_hosts(host_id TEXT,enabled INT);"
                "CREATE TABLE users(id TEXT,disabled INT,disabled_at INT);"
                "CREATE TABLE wc_native_host_auto_enrollment(host_id TEXT,owner_user TEXT,"
                "project TEXT,max_subagents INT,max_cli_calls INT);")
            db.execute("INSERT INTO wc_native_agent_hosts VALUES(?,1)", (self.host,))
            db.execute("INSERT INTO users VALUES('test-owner',0,NULL)")
        self.args = setup.parser().parse_args([
            "--server-url", "http://127.0.0.1:12345", "--host-id", self.host,
            "--owner-user", "test-owner", "--project", "test-project",
            "--bootstrap-token-file", str(self.token), "--desktop-state", str(self.state),
            "--database", str(self.database)])
        self.replies = {
            "/api/users/me": {"success": True, "auth": {"kind": "bootstrap", "is_bootstrap": True}},
            "/api/users/list": {"success": True, "users": [
                {"id": "test-owner", "disabled": False, "disabled_at": None}]},
            "/api/admin/dashboard": {"projects": [
                {"id": "test-project", "enabled": True, "readiness": "online"}]},
        }
        self.calls = []
        self.http = patch.object(setup, "post", side_effect=self.fake_post)
        self.http.start()
        self.addCleanup(self.http.stop)

    def fake_post(self, origin, path, token, body):
        self.assertEqual(origin, "http://127.0.0.1:12345")
        self.assertEqual(token, "synthetic-test-secret")
        self.calls.append(path)
        if path == "/api/admin/agent-isolation":
            self.assertEqual(body["action"], "configure_native_host_auto_enrollment")
            with closing(sqlite3.connect(self.database)) as db, db:
                db.execute("INSERT INTO wc_native_host_auto_enrollment VALUES(?,?,?,?,?)",
                    (body["host_id"], body["owner_user"], body["project"],
                     body["policy"]["max_subagents"], body["policy"]["max_cli_calls"]))
            return {"configured": True, "host_id": self.host, "project": "test-project"}
        return self.replies[path]

    def test_default_checks_do_not_write(self):
        report = setup.configure(self.args)
        self.assertEqual(report, {"checked": True, "applied": False, "already_configured": False,
            "policy": {"max_subagents": 0, "max_cli_calls": 0}})
        self.assertEqual(self.calls, list(self.replies))
        with closing(sqlite3.connect(self.database)) as db, db:
            self.assertEqual(db.execute("SELECT COUNT(*) FROM wc_native_host_auto_enrollment").fetchone(), (0,))

    def test_apply_sends_once_and_checks_readback(self):
        self.args.apply = True
        report = setup.configure(self.args)
        self.assertTrue(report["applied"])
        self.assertEqual(self.calls.count("/api/admin/agent-isolation"), 1)
        self.assertNotIn("synthetic-test-secret", json.dumps(report))
        self.assertNotIn(self.host, json.dumps(report))
        repeated = setup.configure(self.args)
        self.assertTrue(repeated["already_configured"])
        self.assertFalse(repeated["applied"])
        self.assertEqual(self.calls.count("/api/admin/agent-isolation"), 1)

    def test_remote_or_credentialed_urls_fail_before_file_reads(self):
        for origin in ("https://example.test", "http://localhost:12345", "http://127.0.0.1:12345/a",
                       "http://user:password@127.0.0.1:12345", "http://127.0.0.1:12345?secret=x"):
            with self.subTest(origin=origin), patch.object(Path, "read_text") as read:
                self.args.server_url = origin
                with self.assertRaises(setup.SetupError):
                    setup.configure(self.args)
                read.assert_not_called()
        self.assertFalse(self.calls)

    def test_wrong_desktop_target_never_sends_credentials(self):
        self.args.project = "other-project"
        with self.assertRaisesRegex(setup.SetupError, "desktop_target_mismatch"):
            setup.configure(self.args)
        self.assertFalse(self.calls)

    def test_disabled_host_is_not_revived(self):
        with closing(sqlite3.connect(self.database)) as db, db:
            db.execute("UPDATE wc_native_agent_hosts SET enabled=0")
        self.args.apply = True
        with self.assertRaisesRegex(setup.SetupError, "native_host_missing_or_disabled"):
            setup.configure(self.args)
        self.assertFalse(self.calls)

    def test_api_bootstrap_owner_and_project_checks_prevent_apply(self):
        changes = [("/api/users/me", {"success": True, "auth": {"kind": "api"}}),
                   ("/api/users/list", {"success": True, "users": []}),
                   ("/api/admin/dashboard", {"projects": [{"id": "test-project", "enabled": True,
                                                              "readiness": "offline"}]})]
        self.args.apply = True
        for path, bad in changes:
            with self.subTest(path=path), patch.dict(self.replies, {path: bad}):
                with self.assertRaises(setup.SetupError):
                    setup.configure(self.args)
        self.assertNotIn("/api/admin/agent-isolation", self.calls)

    def test_missing_database_does_not_create_one(self):
        missing = self.root / "missing.db"
        self.args.database = missing
        with self.assertRaises(sqlite3.OperationalError):
            setup.configure(self.args)
        self.assertFalse(missing.exists())
        self.assertFalse(self.calls)

    def test_unknown_control_outcome_is_not_retried(self):
        self.args.apply = True
        original = self.fake_post
        def failing(origin, path, token, body):
            if path == "/api/admin/agent-isolation":
                self.calls.append(path)
                raise setup.SetupError("server_unreachable_or_timed_out")
            return original(origin, path, token, body)
        with patch.object(setup, "post", side_effect=failing):
            with self.assertRaisesRegex(setup.SetupError, "timed_out"):
                setup.configure(self.args)
        self.assertEqual(self.calls.count("/api/admin/agent-isolation"), 1)


if __name__ == "__main__":
    unittest.main()
