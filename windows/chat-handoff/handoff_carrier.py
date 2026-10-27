"""Desktop carrier for the original Goal watcher, with a guarded legacy bind.

No import starts a watcher or reads credentials. Historic unassigned Goals are
retained as evidence and never adopted by this connection migration.
"""
from __future__ import annotations

import json
import os
from contextlib import closing
from pathlib import Path
import sqlite3
import sys
import time

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "chat-agents"))
from automatic_dispatch_config import _read_carrier, _read_object, _source_reference, read_native_host_id
from handoff_control_api import Api, load_credentials
import handoff_controller as hc


def _path_identity(value):
    if not isinstance(value, str):
        raise ValueError("invalid_desktop_credential_reference")
    if value.startswith("\\\\?\\UNC\\"):
        value = "\\\\" + value[8:]
    elif value.startswith("\\\\?\\"):
        value = value[4:]
    if not Path(value).is_absolute():
        raise ValueError("invalid_desktop_credential_reference")
    return os.path.normcase(os.path.normpath(value))


class CarrierControl:
    def __init__(self, carrier, desktop_state, db_path, *, credential_loader=load_credentials,
                 control_api_factory=Api):
        self.carrier, self.desktop_state, self.db_path = map(Path, (carrier, desktop_state, db_path))
        if not all(path.is_absolute() for path in (self.carrier, self.desktop_state, self.db_path)):
            raise ValueError("control_paths_must_be_absolute")
        self.credential_loader, self.control_api_factory = credential_loader, control_api_factory
        self.timeout, self._last_project, self._candidate = 5, "", None
        self._pinned = self._initial = self._generation = None
        self._legacy_goals = set()

    def _context(self):
        origin, identity, environ, projects = _read_carrier(self.carrier)
        host = read_native_host_id(self.desktop_state, origin)
        state = _read_object(self.desktop_state)
        runtime = state.get("runtime") or {}
        if (host is None or not projects
                or _source_reference(runtime.get("server_env_file", "")) != identity["credential_source"]):
            raise ValueError("handoff_carrier_unavailable")
        user_reference = _path_identity(runtime.get("user_token_file"))
        key = json.dumps(identity, sort_keys=True)
        pinned = (origin, key, host, user_reference)
        if self._pinned is not None and pinned != self._pinned:
            raise ValueError("handoff_carrier_changed")
        return origin, key, environ, tuple(sorted(projects)), pinned

    def load_credentials(self):
        origin, identity, environ, projects, pinned = self._context()
        # This repair intentionally requires the existing scheduler. Its STOP,
        # history and ownership cannot be replaced by creating a new database.
        with closing(sqlite3.connect(self.db_path.as_uri() + "?mode=ro", uri=True)) as db:
            row = db.execute("SELECT endpoint,mode,generation FROM scheduler WHERE id=1").fetchone()
            if row is None or row[0] != origin or row[1] != "running":
                raise ValueError("scheduler_not_running_or_connection_changed")
            self._generation = row[2]
            self._legacy_goals = set(db.execute(
                "SELECT project,goal_id FROM dispatch WHERE agent_id IS NULL OR agent_id=''"))
        self._pinned, self._initial = pinned, projects
        loaded_origin, token, _ = self.credential_loader(
            environ={**environ, "WEBCODEX_HANDOFF_PROJECT": projects[0]})
        if loaded_origin != origin or self._context()[3] != projects:
            raise ValueError("handoff_carrier_changed")
        return origin, token, identity

    def initialize_new(self):
        """Explicit first installation only; never repair/reset existing state."""
        import auto_handoff as ah
        from run_auto_handoff import sidecar
        origin, identity, environ, projects, pinned = self._context()
        self._pinned = pinned
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        lock_path = ah.ROOT / "runtime" / "desktop-dispatch"
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        path = sidecar(self.db_path, "identity.json")
        with hc.watcher_lock(lock_path), hc.watcher_lock(path):
            existing = [self.db_path, path, sidecar(self.db_path, "runner.json"),
                        path.with_name(path.name + ".legacy-before-carrier.json")]
            existing += [Path(str(self.db_path) + suffix) for suffix in ("-wal", "-shm", "-journal")]
            if any(os.path.lexists(item) for item in existing):
                raise ValueError("scheduler_already_exists")
            loaded_origin, token, _ = self.credential_loader(
                environ={**environ, "WEBCODEX_HANDOFF_PROJECT": projects[0]})
            del token
            if loaded_origin != origin or self._context()[3] != projects:
                raise ValueError("handoff_carrier_changed")
            # Exclusive creation fences competing initializers. Partial failures
            # remain visible and fail closed; no history is deleted on retry.
            with self.db_path.open("xb"):
                pass
            with closing(ah.Store(self.db_path)) as store:
                store.initialize(origin, armed_at=int(time.time()))
            with path.open("x", encoding="utf-8") as output:
                json.dump({"endpoint": origin, "token_file": identity}, output)
                output.flush()
                os.fsync(output.fileno())

    def bind_connection(self, db_path, origin, identity):
        from run_auto_handoff import sidecar, write_json
        if Path(db_path) != self.db_path or self._pinned is None or (origin, identity) != self._pinned[:2]:
            raise ValueError("handoff_connection_mismatch")
        path = sidecar(db_path, "identity.json")
        expected = {"endpoint": origin, "token_file": identity}
        with hc.watcher_lock(path), closing(sqlite3.connect(self.db_path)) as db, db:
            db.execute("BEGIN IMMEDIATE")
            # The write lock keeps another connection's STOP/PAUSE from being
            # overwritten during the file replacement. No state value changes.
            changed = db.execute("UPDATE scheduler SET generation=generation "
                "WHERE id=1 AND endpoint=? AND mode='running' AND generation=?",
                (origin, self._generation)).rowcount
            if changed != 1:
                raise ValueError("scheduler_generation_changed")
            if self._context()[3] != self._initial:
                raise ValueError("handoff_carrier_changed")
            if not path.is_file():
                raise ValueError("existing_scheduler_identity_required")
            previous = _read_object(path, 16384)
            original = path.read_bytes()
            if previous == expected:
                return
            if (set(previous) != {"endpoint", "token_file"} or previous["endpoint"] != origin
                    or _path_identity(previous["token_file"]) != self._pinned[3]):
                raise ValueError("legacy_identity_mismatch")
            if db.execute("SELECT 1 FROM dispatch WHERE status NOT IN ('resumed','cancelled','failed') "
                          "OR (agent_id IS NOT NULL AND agent_id<>'') LIMIT 1").fetchone():
                raise ValueError("legacy_dispatch_not_terminal_unassigned")
            prior_projects = {row[0] for row in db.execute(
                "SELECT project FROM dispatch UNION SELECT project FROM goal_thinking_policy")}
            if not prior_projects <= set(self._initial):
                raise ValueError("legacy_project_outside_carrier")
            backup = path.with_name(path.name + ".legacy-before-carrier.json")
            if backup.exists():
                if backup.read_bytes() != original:
                    raise ValueError("legacy_identity_backup_conflict")
            else:
                with backup.open("xb") as output:
                    output.write(original)
                    output.flush()
                    os.fsync(output.fileno())
            if path.read_bytes() != original or self._context()[3] != self._initial:
                raise ValueError("handoff_carrier_changed")
            write_json(path, expected)

    def api_factory(self, origin, _token, timeout=5):
        if self._pinned is None or origin != self._pinned[0]:
            raise hc.ReadFailure()
        self.timeout = timeout
        return self

    def _api(self, project, agent=None):
        try:
            if self._pinned is None:
                raise ValueError()
            origin, _, environ, projects, _ = self._context()
            if project not in projects:
                raise ValueError()
            scoped = {**environ, "WEBCODEX_HANDOFF_PROJECT": project}
            if agent is not None:
                scoped["WEBCODEX_HANDOFF_AGENT_ID"] = hc.identifier(agent)
            loaded_origin, token, _ = self.credential_loader(environ=scoped)
            if loaded_origin != origin or project not in self._context()[3]:
                raise ValueError()
            return self.control_api_factory(origin, token, timeout=self.timeout, environ=scoped)
        except Exception:
            raise hc.ReadFailure("connector_auth") from None

    def discover(self):
        self._candidate = None
        try:
            projects = self._context()[3]
        except Exception:
            raise hc.ReadFailure("connector_auth") from None
        # One bounded existing discovery per tick, including readiness. A failed
        # scope advances too; a multi-project sweep cannot extend the deadline.
        project = next((value for value in projects if value > self._last_project), projects[0])
        self._last_project = project
        result, seen = [], set()
        for candidate in self._api(project).discover():
            if (project, candidate["goal_id"]) in self._legacy_goals:
                continue
            key = tuple(candidate[field] for field in
                        ("project", "goal_id", "agent_id", "source_context_epoch", "checkpoint_sequence"))
            if key not in seen:
                seen.add(key)
                result.append(candidate)
        return result

    def read(self, candidate):
        self._candidate = None
        project, goal, agent = (candidate.get(key) for key in ("project", "goal_id", "agent_id"))
        if (project, goal) in self._legacy_goals or agent is None:
            raise hc.ReadFailure()
        value = self._api(project, agent).read(candidate)
        # poll_once and its launch callback are synchronous. Keep the exact
        # selected tuple, never resolve an unscoped Goal ID across projects.
        self._candidate = (project, goal, agent)
        return value

    def call(self, tool, params):
        if self._candidate is None or params.get("goal_id") != self._candidate[1]:
            raise hc.ReadFailure()
        project, _, agent = self._candidate
        return self._api(project, agent).call(tool, params)

    def api_for_request(self, request):
        project, goal, agent = (request.get(key) for key in ("project", "goal_id", "agent_id"))
        if ((project, goal) in self._legacy_goals or agent is None
                or (project, goal, agent) != self._candidate):
            raise hc.ReadFailure()
        return self._api(project, agent)

    def launch(self, request_file, should_stop):
        from edge_adapter import launch
        return launch(request_file, should_stop, control_api_factory=self.api_for_request)
