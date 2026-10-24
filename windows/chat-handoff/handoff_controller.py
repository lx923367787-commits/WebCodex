"""Read-only local handoff observer. Never creates chats or mutates a Goal."""
from __future__ import annotations

import argparse
import contextlib
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

REASONS = ("login", "connector_auth", "tool_approval", "security_challenge", "rate_limit", "unknown", "reconciliation", "handoff_failed")
IDENTITIES = ("window_bound", "explicit_context_only")
PHASES = ("WAITING_CHECKPOINT", "WAITING_NEW_CHAT", "B_CLAIMED", "BUSINESS_VERIFIED", "HANDOFF_FAILED")
DEFAULT_DB = Path(__file__).resolve().parent / "controller-state.sqlite3"
MAX_RESPONSE = 1_048_576


class ReadFailure(Exception):
    """Only a controlled reason is exposed; raw network errors may contain secrets."""
    def __init__(self, reason="unknown"):
        self.reason = reason if reason in REASONS else "unknown"
        super().__init__(self.reason)


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,200}", value):
        raise ValueError("Identifiers must contain only letters, digits, _, ., :, or - (1–200 characters)")
    return value


def endpoint(value):
    parsed = urllib.parse.urlsplit(value)
    if (parsed.scheme != "http" or parsed.hostname not in ("127.0.0.1", "localhost", "::1")
            or parsed.username or parsed.password or parsed.query or parsed.fragment
            or parsed.path not in ("", "/") or parsed.port is None):
        raise ValueError("Endpoint must be an explicit http loopback origin, e.g. http://127.0.0.1:19876")
    return value.rstrip("/")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ReadFailure("unknown")


class Api:
    def __init__(self, origin, token, timeout=10):
        self.origin = endpoint(origin)
        if not token or "\r" in token or "\n" in token:
            raise ValueError("A bearer token is required in the configured environment variable")
        self.token, self.timeout = token, timeout
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def read(self, config):
        payload = {"tool": "goal_read", "params": {
            "goal_id": config["goal_id"],
            "view": "handoff", "source_context_epoch": config["source_context_epoch"],
        }}
        request = urllib.request.Request(self.origin + "/api/tools/call", json.dumps(payload).encode(),
            {"Authorization": "Bearer " + self.token, "Content-Type": "application/json"})
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                data = response.read(MAX_RESPONSE + 1)
            if len(data) > MAX_RESPONSE:
                raise ReadFailure()
            body = json.loads(data)
            if not isinstance(body, dict) or body.get("success") is not True:
                output = body.get("output", {}) if isinstance(body, dict) else {}
                kind = output.get("error_kind") if isinstance(output, dict) else None
                raise ReadFailure({"login_required": "login", "authentication_required": "connector_auth",
                    "tool_approval_required": "tool_approval", "security_challenge": "security_challenge",
                    "rate_limit_exceeded": "rate_limit", "reconciliation_required": "reconciliation"}.get(kind, "unknown") if isinstance(kind, str) else "unknown")
            return body["output"]
        except urllib.error.HTTPError as error:
            # HTTP 403 alone does not establish that a tool approval is required.
            error.close()
            raise ReadFailure({401: "connector_auth", 429: "rate_limit"}.get(error.code, "unknown")) from None
        except (OSError, urllib.error.URLError, ValueError, KeyError):
            raise ReadFailure() from None


def number(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def normalize(raw, config):
    """Select bounded authoritative fields. No raw response is persisted."""
    if not isinstance(raw, dict) or raw.get("projection") != "handoff" or raw.get("driver") != "host":
        raise ReadFailure()
    goal, source = raw.get("goal"), raw.get("source_context")
    if not isinstance(goal, dict) or not isinstance(source, dict):
        raise ReadFailure()
    if goal.get("goal_id") != config["goal_id"] or goal.get("project") != config["project"]:
        raise ReadFailure()
    if source.get("context_epoch") != config["source_context_epoch"]:
        raise ReadFailure()
    lifecycle, identity = source.get("lifecycle_state"), source.get("identity_source")
    if lifecycle not in ("unknown", "active", "warning", "draining", "retired", "stopped"):
        raise ReadFailure()
    unresolved = raw.get("unresolved_operation_count")
    if not number(unresolved):
        raise ReadFailure()
    result = {"phase": "WAITING_CHECKPOINT", "reason": None,
        "identity_source": identity if identity in IDENTITIES else None,
        "lifecycle_state": lifecycle, "checkpoint_sequence": None,
        "checkpoint_revision": None, "source_fence": None,
        "retired_at": source.get("retired_at") if number(source.get("retired_at")) else None,
        "checkpoint_created_at": None, "unresolved_operation_count": unresolved,
        "successor_attempt_id": None, "successor_epoch": None,
        "successor_fence": None, "business_operation_id": None,
        "assurance": "local_protocol_only", "web_unattended_verified": False}
    if identity not in IDENTITIES or lifecycle == "unknown":
        result["reason"] = "unknown"
        return result
    cp = raw.get("checkpoint")
    if cp is None:
        if lifecycle == "retired":
            result["reason"] = "unknown"
        return result
    if not isinstance(cp, dict):
        raise ReadFailure()
    if cp.get("action") != "handoff":
        if lifecycle == "retired":
            result["reason"] = "unknown"
        return result
    if (cp.get("source_context_epoch") != config["source_context_epoch"]
            or not all(number(cp.get(k)) for k in ("sequence", "revision", "created_at"))
            or not (number(cp.get("source_fence")) or (raw.get("handoff") is not None and cp.get("source_fence") is None))):
        result["reason"] = "unknown"
        return result
    result.update(checkpoint_sequence=cp["sequence"], checkpoint_revision=cp["revision"],
        source_fence=cp["source_fence"], checkpoint_created_at=cp["created_at"])
    if raw.get("handoff") is not None:
        import handoff_reliability as reliable
        candidate = {key: config[key] for key in ("project", "goal_id", "source_context_epoch")}
        candidate["checkpoint_sequence"] = cp["sequence"]
        handoff = reliable.observation(raw, candidate)["handoff"]
        result["phase"] = "WAITING_NEW_CHAT"
        if handoff["status"] == "failed":
            result.update(phase="HANDOFF_FAILED", reason="handoff_failed")
        elif handoff["status"] == "committed":
            result.update(phase="B_CLAIMED", successor_attempt_id=handoff["attempt_id"],
                          successor_epoch=handoff["execution_owner_epoch"], successor_fence=goal.get("fence"))
        return result
    if lifecycle != "retired" or result["retired_at"] is None:
        # Atomic handoff cannot be accepted when the source has not retired.
        result["reason"] = "unknown"
        return result
    result["phase"] = "WAITING_NEW_CHAT"
    if unresolved:
        result["reason"] = "reconciliation"
        return result
    attempt = raw.get("current_attempt")
    if attempt is None:
        return result
    if not isinstance(attempt, dict):
        raise ReadFailure()
    successor_epoch = attempt.get("context_epoch")
    # A bootstrap without a successor attempt cannot advance the state.
    if successor_epoch == config["source_context_epoch"]:
        return result
    binding = attempt.get("binding")
    if (not successor_epoch or attempt.get("identity_source") not in IDENTITIES
            or not isinstance(binding, dict) or binding.get("goal_id") != config["goal_id"]
            or not number(binding.get("fence")) or binding["fence"] <= cp["source_fence"]
            or not binding.get("attempt_id")
            or (binding["attempt_id"] != goal.get("current_attempt_id")
                and not (goal.get("current_attempt_id") is None and attempt.get("status") == "passed"))
            or binding["fence"] != goal.get("fence")
            or attempt.get("status") not in ("running", "passed", "failed", "interrupted")):
        result["reason"] = "unknown"
        return result
    try:
        identifier(successor_epoch)
        identifier(binding["attempt_id"])
    except ValueError:
        raise ReadFailure() from None
    result.update(phase="B_CLAIMED", successor_attempt_id=binding["attempt_id"],
        successor_epoch=successor_epoch, successor_fence=binding["fence"])
    evidence = raw.get("business_evidence")
    if (raw.get("business_verified") is True and attempt["status"] == "passed"
            and isinstance(evidence, dict) and evidence.get("attempt_id") == binding["attempt_id"]
            and evidence.get("status") == "succeeded" and evidence.get("check_passed") is True):
        try:
            identifier(evidence.get("operation_id"))
        except ValueError:
            raise ReadFailure() from None
        result.update(phase="BUSINESS_VERIFIED", business_operation_id=evidence["operation_id"])
    return result


class Store:
    def __init__(self, path):
        self.path = Path(path).resolve()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(self.path, timeout=15)
        self.db.row_factory = sqlite3.Row
        self.db.executescript("""
            CREATE TABLE IF NOT EXISTS controller (
                id INTEGER PRIMARY KEY CHECK(id=1), config TEXT NOT NULL,
                mode TEXT NOT NULL, reason TEXT, failures INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT, updated_at INTEGER NOT NULL, control_generation INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS handoff (
                source_epoch TEXT NOT NULL, checkpoint_sequence INTEGER NOT NULL,
                launch_id TEXT NOT NULL UNIQUE, checkpoint_revision INTEGER NOT NULL,
                source_fence INTEGER NOT NULL, phase TEXT NOT NULL, snapshot TEXT NOT NULL,
                PRIMARY KEY(source_epoch, checkpoint_sequence));
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY, launch_id TEXT, event TEXT NOT NULL,
                created_at INTEGER NOT NULL);
        """)

    def close(self):
        self.db.close()

    def initialize(self, config):
        config = dict(config)
        for key in ("goal_id", "project", "source_context_epoch"):
            identifier(config[key])
        config["endpoint"] = endpoint(config["endpoint"])
        with self.db:
            self.db.execute("INSERT OR IGNORE INTO controller VALUES(1,?,'running',NULL,0,NULL,?,0)",
                (json.dumps(config, sort_keys=True), int(time.time())))
            if self.status()["config"] != config:
                raise ValueError("This DB is bound to another observation; select a separate --db")

    def status(self):
        row = self.db.execute("SELECT * FROM controller WHERE id=1").fetchone()
        if row is None:
            raise ValueError("Controller is not initialized; run watch with goal, project, source and endpoint")
        result = dict(row)
        result.pop("id")
        result["config"] = json.loads(result["config"])
        result["snapshot"] = json.loads(result["snapshot"]) if result["snapshot"] else None
        return result

    def set_mode(self, mode, reason=None):
        if mode not in ("running", "paused", "stopped", "blocked") or reason not in (None, *REASONS):
            raise ValueError("Invalid mode or block reason")
        self.status()
        with self.db:
            self.db.execute("UPDATE controller SET mode=?,reason=?,updated_at=?,control_generation=control_generation+1 WHERE id=1",
                (mode, reason, int(time.time())))
            self.db.execute("INSERT INTO events(event,created_at) VALUES(?,?)", (mode, int(time.time())))

    def failure(self, reason):
        with self.db:
            self.db.execute("""UPDATE controller SET failures=failures+1,
                mode=CASE WHEN failures+1>=3 AND mode='running' THEN 'paused' ELSE mode END,
                reason=CASE WHEN mode='running' THEN ? ELSE reason END,updated_at=? WHERE id=1""",
                (reason, int(time.time())))
        return self.status()

    def pause_if_running(self):
        """An interrupt must not overwrite a newer explicit STOP or block."""
        with self.db:
            changed = self.db.execute("""UPDATE controller SET mode='paused',reason=NULL,
                updated_at=?,control_generation=control_generation+1 WHERE id=1 AND mode='running'""", (int(time.time()),)).rowcount
            if changed:
                self.db.execute("INSERT INTO events(event,created_at) VALUES('paused',?)", (int(time.time()),))

    def observe(self, observation, resume=False, expected_control_generation=None):
        with self.db:
            # Serialize control commands and snapshot acceptance. A STOP racing with
            # an HTTP response is preserved; only explicit resume may clear it.
            self.db.execute("BEGIN IMMEDIATE")
            state = self.status()
            source = state["config"]["source_context_epoch"]
            seq = observation["checkpoint_sequence"]
            if seq is not None:
                fence = observation["source_fence"] if observation["source_fence"] is not None else -1
                row = self.db.execute("SELECT * FROM handoff WHERE source_epoch=? AND checkpoint_sequence=?", (source, seq)).fetchone()
                if row and (row["checkpoint_revision"] != observation["checkpoint_revision"] or row["source_fence"] != fence):
                    raise ReadFailure()
                launch_id = row["launch_id"] if row else str(uuid.uuid4())
                observation = {**observation, "launch_id": launch_id}
                previous = row["phase"] if row else "WAITING_CHECKPOINT"
                # Reject regression rather than grafting a historical phase onto
                # a fresh, weaker proof. A completed handoff retains its original
                # successor even if the Goal later starts another attempt.
                if PHASES.index(observation["phase"]) < PHASES.index(previous):
                    raise ReadFailure()
                if row and previous == "BUSINESS_VERIFIED":
                    prior = json.loads(row["snapshot"])
                    for field in ("successor_attempt_id", "successor_epoch", "successor_fence", "business_operation_id"):
                        if observation[field] != prior[field]:
                            raise ReadFailure()
                self.db.execute("""INSERT INTO handoff VALUES(?,?,?,?,?,?,?)
                    ON CONFLICT(source_epoch,checkpoint_sequence) DO UPDATE SET
                    phase=excluded.phase,snapshot=excluded.snapshot""", (source, seq, launch_id,
                    observation["checkpoint_revision"], fence, observation["phase"], json.dumps(observation)))
                transitions = (["HANDOFF_FAILED"] if observation["phase"] == "HANDOFF_FAILED"
                    else PHASES[PHASES.index(previous)+1:PHASES.index(observation["phase"])+1])
                for phase in transitions:
                    self.db.execute("INSERT INTO events(launch_id,event,created_at) VALUES(?,?,?)", (launch_id, phase, int(time.time())))
            mode, reason = state["mode"], state["reason"]
            if mode == "running" or (resume and state["control_generation"] == expected_control_generation):
                mode = "blocked" if observation["reason"] else "running"
                reason = observation["reason"]
            self.db.execute("UPDATE controller SET mode=?,reason=?,failures=0,snapshot=?,updated_at=? WHERE id=1",
                (mode, reason, json.dumps(observation), int(time.time())))
        return self.status()


@contextlib.contextmanager
def watcher_lock(path):
    """OS-owned lock released on process death, independent from durable STOP."""
    lock = open(str(Path(path).resolve()) + ".watch.lock", "a+b")
    try:
        if os.name == "nt":
            import msvcrt
            if lock.seek(0, 2) == 0:
                lock.write(b"0")
                lock.flush()
            lock.seek(0)
            try:
                msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            except OSError:
                raise ValueError("Another watch or resume process owns this controller") from None
        else:
            import fcntl
            try:
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                raise ValueError("Another watch or resume process owns this controller") from None
        yield
    finally:
        lock.close()


def poll_once(store, api, resume=False):
    state = store.status()
    if state["mode"] != "running" and not resume:
        return state
    try:
        return store.observe(normalize(api.read(state["config"]), state["config"]), resume=resume,
            expected_control_generation=state["control_generation"])
    except ReadFailure as error:
        return store.failure(error.reason)


def watch(store, api, interval=5, once=False):
    while True:
        state = poll_once(store, api)
        print(json.dumps(state, ensure_ascii=False), flush=True)
        if once or state["mode"] != "running" or (state["snapshot"] or {}).get("phase") == "BUSINESS_VERIFIED":
            return state
        deadline = time.monotonic() + interval
        while time.monotonic() < deadline:
            if store.status()["mode"] != "running":
                return store.status()
            time.sleep(min(0.25, max(0, deadline-time.monotonic())))


def recovery_prompt(state):
    c, s = state["config"], state["snapshot"] or {}
    if s.get("checkpoint_sequence") is None or s.get("lifecycle_state") != "retired":
        raise ValueError("No verified source handoff checkpoint is available")
    return (f"在当前新对话恢复已有 Goal {c['goal_id']}（项目 {c['project']}）。\n"
        f"源 context_epoch={c['source_context_epoch']}，checkpoint={s['checkpoint_sequence']}，"
        f"revision={s['checkpoint_revision']}，launch_id={s['launch_id']}。\n"
        "先通过既有只读工具核验该交接，再使用既有 bootstrap／恢复与认领流程。"
        "保持同一 Goal、权限及验收合同；使用当前宿主窗口身份，缺少宿主身份时明确标注 explicit_context_only。\n"
        "观察并对账原 Job／Run ID，未知副作用或未决任务未解决时停靠；不要重跑原业务。"
        "领取新 attempt 后才继续业务，不能复用旧会话 binding。"
        "登录、连接器授权、工具批准或安全验证需要用户处理时明确报告并停靠。\n"
        "本提示由本地只读控制器生成，不代表已建新对话、已认领或网页无人值守通过。")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, default=DEFAULT_DB)
    sub = parser.add_subparsers(dest="command", required=True)
    start = sub.add_parser("watch")
    start.add_argument("--goal-id")
    start.add_argument("--project")
    start.add_argument("--source-context-epoch")
    start.add_argument("--endpoint")
    start.add_argument("--interval", type=float, default=5)
    start.add_argument("--once", action="store_true")
    for name in ("status", "pause", "stop", "resume", "recovery-prompt"):
        command = sub.add_parser(name)
        if name == "pause":
            command.add_argument("--reason", choices=REASONS)
    for command in (start, sub.choices["resume"]):
        command.add_argument("--token-env", default="WEBCODEX_READ_TOKEN")
        command.add_argument("--timeout", type=float, default=10)
    args = parser.parse_args(argv)
    store = None
    try:
        if args.command != "watch" and not args.db.is_file():
            raise ValueError("Controller DB does not exist")
        store = Store(args.db)
        if args.command == "watch":
            supplied = (args.goal_id, args.project, args.source_context_epoch, args.endpoint)
            if any(supplied):
                if not all(supplied):
                    raise ValueError("Provide goal, project, source epoch and endpoint together")
                store.initialize(dict(zip(("goal_id", "project", "source_context_epoch", "endpoint"), supplied)))
            if args.interval < 0.1:
                raise ValueError("Polling interval must be at least 0.1 seconds")
        if args.command in ("watch", "resume"):
            if not 0 < args.timeout <= 10:
                raise ValueError("Read timeout must be greater than 0 and at most 10 seconds")
            state = store.status()
            if args.command == "watch" and state["mode"] != "running":
                print(json.dumps(state, ensure_ascii=False))
                return 2
            api = Api(state["config"]["endpoint"], os.environ.get(args.token_env, ""), args.timeout)
            with watcher_lock(args.db):
                if args.command == "resume":
                    state = poll_once(store, api, resume=True)
                    print(json.dumps(state, ensure_ascii=False))
                else:
                    state = watch(store, api, args.interval, args.once)
            return 0 if state["mode"] == "running" and state["failures"] == 0 else 2
        if args.command == "pause":
            store.set_mode("blocked" if args.reason else "paused", args.reason)
        elif args.command == "stop":
            store.set_mode("stopped")
        state = store.status()
        print(recovery_prompt(state) if args.command == "recovery-prompt" else json.dumps(state, ensure_ascii=False))
        return 0
    except (ValueError, sqlite3.Error):
        # Validation errors are controlled; SQLite filenames and responses are not printed.
        print("Controller command failed: check arguments, state DB, token environment and watcher ownership.", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        if store is not None:
            store.pause_if_running()
        return 130
    finally:
        if store is not None:
            store.close()


if __name__ == "__main__":
    raise SystemExit(main())
