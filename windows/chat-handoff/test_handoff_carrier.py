"""Only temporary SQLite, synthetic carrier files and fake control responses."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

# Import only this checkout's adjacent source. No production entry point is run.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "chat-agents"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import auto_handoff as ah
import edge_adapter
import handoff_controller as hc
from handoff_control_api import Api
import run_auto_handoff as runner
from handoff_carrier import CarrierControl

assert Path(runner.__file__).parent == Path(__file__).resolve().parent
assert Path(edge_adapter.__file__).parent == Path(__file__).resolve().parent

ORIGIN = "http://127.0.0.1:19876"
PROJECTS = ["project-a", "project-b", "project-c"]
HOST = "wc_host_" + "1" * 32


class CarrierTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="goal-carrier-stage-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.db_path = self.root / "scheduler.sqlite3"
        self.store = ah.Store(self.db_path)
        self.addCleanup(self.store.close)
        self.store.initialize(ORIGIN, armed_at=0)
        with self.store.db:
            self.store.db.execute("UPDATE scheduler SET generation=10 WHERE id=1")
            for index in range(7):
                self.store.db.execute("INSERT INTO dispatch(project,goal_id,source_context_epoch,"
                    "checkpoint_sequence,checkpoint_revision,source_fence,retired_at,deadline_at,"
                    "launch_id,status,snapshot) VALUES(?,?,?,1,1,1,1,120,?,?,?)",
                    (PROJECTS[0], "legacy-" + str(index), "epoch-old", "launch-old-" + str(index),
                     "resumed" if index < 5 else "cancelled", "{}"))
        self.state, self.carrier = self.root / "desktop.json", self.root / "control.json"
        self.bootstrap = self.root / "bootstrap-fixture.txt"
        self.bootstrap.write_text("synthetic-bootstrap", encoding="utf-8")
        self.user_reference, self.source_reference = self.root / "unread-user.token", self.root / "unread-server.env"
        self.state_value = {"runtime": {"server_url": ORIGIN, "native_host_id": HOST,
            "server_env_file": str(self.source_reference), "user_token_file": str(self.user_reference)}}
        self.carrier_value = {"schema_version": 2, "server_url": ORIGIN,
            "bootstrap_token_file": str(self.bootstrap), "source_ref": str(self.source_reference),
            "owner_user": "fixture-owner", "projects": PROJECTS.copy()}
        self.save()
        self.identity = runner.sidecar(self.db_path, "identity.json")
        self.old_identity = json.dumps({"endpoint": ORIGIN, "token_file": str(self.user_reference)}, indent=2).encode()
        self.identity.write_bytes(self.old_identity)
        self.posts = []
        self.fail_project = None
        self.instances = []
        owner = self

        class FakeApi(Api):
            def post(self, payload):
                owner.posts.append(dict(payload))
                if self.project == owner.fail_project:
                    raise hc.ReadFailure()
                if payload["action"] == "handoff_list":
                    candidate = {"project": self.project, "goal_id": "shared-goal",
                        "agent_id": "agent-" + self.project, "source_context_epoch": "new-epoch",
                        "checkpoint_sequence": 1}
                    return {"candidates": [candidate, candidate,
                        dict(candidate, goal_id="legacy-0") ] if self.project == PROJECTS[0]
                        else [candidate, candidate]}
                if payload["action"] == "handoff_launch_claim":
                    return {**{key: payload[key] for key in ("project", "goal_id", "agent_id", "handoff_id")},
                        "attempt_id": payload["expected_attempt_id"], "expires_at_ms": 200000,
                        "claim_grant": "wc_hc_" + "7e" * 32}
                return {"project": self.project, "agent_id": payload["agent_id"]}

        self.api_type = FakeApi
        network = patch("urllib.request.OpenerDirector.open", side_effect=AssertionError("no network"))
        network.start()
        self.addCleanup(network.stop)

    def save(self):
        self.state.write_text(json.dumps(self.state_value), encoding="utf-8")
        self.carrier.write_text(json.dumps(self.carrier_value), encoding="utf-8")

    def control(self):
        return CarrierControl(self.carrier, self.state, self.db_path, control_api_factory=self.api_type)

    def load_and_bind(self, control):
        origin, token, identity = control.load_credentials()
        control.bind_connection(self.db_path, origin, identity)
        return origin, token, identity

    def snapshot(self):
        return list(self.store.db.iterdump())

    def test_migration_preserves_all_database_rows_and_original_identity_backup(self):
        before = self.snapshot()
        env = set(os.environ)
        control = self.control()
        origin, token, identity = self.load_and_bind(control)
        self.assertEqual(token, "synthetic-bootstrap")
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(self.identity.with_name(self.identity.name + ".legacy-before-carrier.json").read_bytes(), self.old_identity)
        self.assertEqual(json.loads(self.identity.read_bytes()), {"endpoint": origin, "token_file": identity})
        self.assertEqual(set(os.environ), env)
        self.assertFalse(self.user_reference.exists())
        self.assertFalse(self.source_reference.exists())
        self.assertNotIn("synthetic-bootstrap", self.identity.read_text())
        self.assertNotIn(str(self.bootstrap), identity)
        original = self.identity.read_bytes()
        self.bootstrap = self.root / "rotated-bootstrap.txt"
        self.bootstrap.write_text("rotated-synthetic", encoding="utf-8")
        self.carrier_value["bootstrap_token_file"] = str(self.bootstrap)
        self.save()
        self.assertEqual(self.load_and_bind(self.control())[2], identity)
        self.assertEqual(self.identity.read_bytes(), original)
        self.assertEqual(self.snapshot(), before)

    def test_pause_stop_and_generation_changes_cannot_migrate_or_resume(self):
        for mode in ("paused", "stopped", "running"):
            with self.subTest(mode=mode):
                self.store.set_mode("running")
                control = self.control()
                origin, _, identity = control.load_credentials()
                self.store.set_mode(mode)
                latest = self.store.status()
                with self.assertRaisesRegex(ValueError, "generation_changed"):
                    control.bind_connection(self.db_path, origin, identity)
                self.assertEqual(self.store.status(), latest)
                self.assertEqual(self.identity.read_bytes(), self.old_identity)
                if mode != "running":
                    with self.assertRaises(ValueError):
                        self.control().load_credentials()

    def test_migration_rejects_changed_scope_host_source_and_legacy_ownership(self):
        mutations = [lambda: self.carrier_value.update(owner_user="different-owner"),
                     lambda: self.carrier_value.update(projects=[PROJECTS[1]]),
                     lambda: self.state_value["runtime"].update(native_host_id=None),
                     lambda: self.state_value["runtime"].update(user_token_file=str(self.root / "other.token")),
                     lambda: self.carrier_value.update(source_ref=str(self.root / "other.env"))]
        original_state = json.loads(json.dumps(self.state_value))
        original_carrier = dict(self.carrier_value)
        for mutate in mutations:
            control = self.control()
            origin, _, identity = control.load_credentials()
            mutate()
            self.save()
            before = self.snapshot()
            with self.assertRaises(ValueError):
                control.bind_connection(self.db_path, origin, identity)
            self.assertEqual(self.identity.read_bytes(), self.old_identity)
            self.assertEqual(self.snapshot(), before)
            self.state_value = json.loads(json.dumps(original_state))
            self.carrier_value = dict(original_carrier)
            self.save()
        for field, value in (("status", "unknown"), ("agent_id", "assigned-agent")):
            with self.store.db:
                self.store.db.execute("UPDATE dispatch SET " + field + "=? WHERE goal_id='legacy-0'", (value,))
            control = self.control()
            origin, _, identity = control.load_credentials()
            before = self.snapshot()
            with self.assertRaisesRegex(ValueError, "terminal_unassigned"):
                control.bind_connection(self.db_path, origin, identity)
            self.assertEqual(self.snapshot(), before)
            with self.store.db:
                self.store.db.execute("UPDATE dispatch SET status='resumed',agent_id=NULL WHERE goal_id='legacy-0'")

    def test_migration_lock_and_backup_conflict_do_not_overwrite_identity(self):
        control = self.control()
        origin, _, identity = control.load_credentials()
        with hc.watcher_lock(self.identity):
            with self.assertRaises(ValueError):
                control.bind_connection(self.db_path, origin, identity)
        self.assertEqual(self.identity.read_bytes(), self.old_identity)
        backup = self.identity.with_name(self.identity.name + ".legacy-before-carrier.json")
        backup.write_bytes(b"different historical evidence")
        with self.assertRaisesRegex(ValueError, "backup_conflict"):
            control.bind_connection(self.db_path, origin, identity)
        self.assertEqual(self.identity.read_bytes(), self.old_identity)
        self.assertEqual(backup.read_bytes(), b"different historical evidence")

    def test_three_scopes_share_store_without_duplicate_rows_or_adopting_history(self):
        control = self.control()
        self.load_and_bind(control)
        legacy = [dict(row) for row in self.store.db.execute("SELECT * FROM dispatch")]
        launcher = Mock(side_effect=AssertionError("terminal observation cannot launch"))
        seen = []

        def observation(api, candidate):
            seen.append((candidate["project"], candidate["goal_id"]))
            api.read(candidate)
            api.call("goal_handoff", {"action": "status", "goal_id": candidate["goal_id"]})
            return {"decision": "cancelled", "reason": None, "checkpoint_revision": 1,
                    "source_fence": 1, "retired_at": 1}

        with patch.object(ah, "read_observation", side_effect=observation):
            for _ in range(6):
                ah.poll_once(self.store, control, launcher, request_dir=self.root / "requests")
        self.assertEqual(seen, [(project, "shared-goal") for project in PROJECTS] * 2)
        self.assertEqual(self.store.db.execute("SELECT COUNT(*) FROM dispatch").fetchone()[0], 10)
        self.assertEqual([dict(row) for row in self.store.db.execute("SELECT * FROM dispatch WHERE goal_id LIKE 'legacy-%'")], legacy)
        for payload in self.posts:
            self.assertEqual(payload["owner_user"], "fixture-owner")
            if "agent_id" in payload:
                self.assertEqual(payload["agent_id"], "agent-" + payload["project"])
        launcher.assert_not_called()

    def test_failed_scope_rotates_and_removed_scope_blocks_frozen_claim(self):
        control = self.control()
        self.load_and_bind(control)
        self.fail_project = PROJECTS[0]
        with self.assertRaises(hc.ReadFailure):
            control.discover()
        candidate = control.discover()[0]
        self.assertEqual(candidate["project"], PROJECTS[1])
        control.read(candidate)
        request = dict(candidate, handoff_id="handoff-b", attempt_id="attempt-b")
        self.carrier_value["projects"] = [PROJECTS[0], PROJECTS[2]]
        self.save()
        calls = len(self.posts)
        with self.assertRaises(hc.ReadFailure):
            control.api_for_request(request)
        self.assertEqual(len(self.posts), calls)
        self.assertEqual(control.discover()[0]["project"], PROJECTS[2])

    def test_launch_claim_uses_same_bound_scope_without_ambient_control(self):
        control = self.control()
        self.load_and_bind(control)
        candidate = control.discover()[0]
        control.read(candidate)
        request = dict(candidate, handoff_id="handoff-a", attempt_id="attempt-a", expires_at="fixture")
        with patch.object(edge_adapter, "_control_api", side_effect=AssertionError("ambient control forbidden")), \
                patch.object(edge_adapter.flow, "parse_time", return_value=Mock(timestamp=lambda: 200)), \
                patch.object(edge_adapter.flow, "utc_now", return_value=Mock(timestamp=lambda: 100)):
            grant, outcome = edge_adapter._launch_claim(request, control.api_for_request)
        self.assertIsNone(outcome)
        self.assertEqual(grant, "wc_hc_" + "7e" * 32)
        self.assertEqual(self.posts[-1]["project"], PROJECTS[0])
        with self.assertRaises(hc.ReadFailure):
            control.api_for_request(dict(request, project=PROJECTS[1]))
        with patch.object(edge_adapter, "launch", return_value={"status": "unknown"}) as launch:
            stop = Mock()
            control.launch(self.root / "synthetic-request.json", stop)
        self.assertIs(launch.call_args.kwargs["control_api_factory"].__self__, control)

    def test_main_only_wires_existing_run_and_keeps_pause_command_unmodified(self):
        control = self.control()
        with patch("handoff_carrier.CarrierControl", return_value=control), \
                patch.object(runner, "run", return_value=0) as run:
            self.assertEqual(runner.main(["run", "--db", str(self.db_path), "--desktop-state", str(self.state),
                                          "--control-config", str(self.carrier)]), 0)
        self.assertIs(run.call_args.kwargs["credentials_loader"].__self__, control)
        self.assertIs(run.call_args.kwargs["connection_binder"].__self__, control)
        self.assertIs(run.call_args.kwargs["api_factory"].__self__, control)
        self.assertIs(run.call_args.kwargs["launcher"].__self__, control)
        self.assertEqual(self.identity.read_bytes(), self.old_identity)


if __name__ == "__main__":
    unittest.main()
