"""Durable, bounded dispatch for Server-confirmed Host Goal handoffs."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import sqlite3
import sys
import time
import urllib.error
import urllib.request
import uuid

import handoff_controller as hc

ROOT = Path(__file__).resolve().parent
DEFAULT_DB = ROOT / "runtime" / "auto-handoff.sqlite3"
ACTIVE_PHASES = {"planning", "awaiting_confirmation", "ready", "executing", "verifying"}
GOAL_PHASES = ACTIVE_PHASES | {"paused", "blocked", "completed", "cancelled"}
TERMINAL_DISPATCH = {"resumed", "cancelled", "unknown", "blocked", "failed"}


def utc_iso(seconds):
    return datetime.fromtimestamp(seconds, timezone.utc).isoformat()


class LegacyReadApi(hc.Api):
    """Historical model-tool protocol for explicit test/migration reads only."""
    def call(self, tool, params):
        request = urllib.request.Request(self.origin + "/api/tools/call",
            json.dumps({"tool": tool, "params": params}).encode(),
            {"Authorization": "Bearer " + self.token, "Content-Type": "application/json"})
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                data = response.read(hc.MAX_RESPONSE + 1)
            if len(data) > hc.MAX_RESPONSE:
                raise hc.ReadFailure()
            body = json.loads(data)
            if not isinstance(body, dict) or body.get("success") is not True:
                raise hc.ReadFailure()
            return body["output"]
        except urllib.error.HTTPError as error:
            error.close()
            raise hc.ReadFailure({401: "connector_auth", 429: "rate_limit"}.get(error.code, "unknown")) from None
        except (OSError, urllib.error.URLError, ValueError, KeyError):
            raise hc.ReadFailure() from None

    def discover(self):
        raw = self.call("goal_list", {"limit": 100})
        if not isinstance(raw, dict) or not isinstance(raw.get("goals"), list):
            raise hc.ReadFailure()
        candidates = []
        for goal in raw["goals"]:
            if not isinstance(goal, dict) or goal.get("driver") != "host":
                continue
            if "latest_handoff" not in goal:
                raise hc.ReadFailure()
            latest = goal.get("latest_handoff")
            if latest is None:
                continue
            if not isinstance(latest, dict) or not hc.number(latest.get("checkpoint_sequence")):
                raise hc.ReadFailure()
            try:
                candidates.append({"project": hc.identifier(goal.get("project")),
                    "goal_id": hc.identifier(goal.get("goal_id")),
                    "source_context_epoch": hc.identifier(latest.get("source_context_epoch")),
                    "checkpoint_sequence": latest["checkpoint_sequence"]})
            except ValueError:
                raise hc.ReadFailure() from None
        return candidates


def read_observation(api, candidate):
    raw = api.read(candidate)
    if isinstance(raw, dict) and raw.get("handoff") is not None:
        import handoff_reliability as reliable
        return reliable.observation(raw, candidate)
    observation = hc.normalize(raw, candidate)
    goal = raw["goal"]
    phase = goal.get("phase")
    if phase not in GOAL_PHASES:
        raise hc.ReadFailure()
    if (observation["checkpoint_sequence"] != candidate["checkpoint_sequence"]
            or observation["lifecycle_state"] != "retired"
            or not hc.number(observation["retired_at"]) or "resume_started" not in raw):
        raise hc.ReadFailure()
    receipt = raw.get("resume_started")
    if receipt is not None:
        if (not isinstance(receipt, dict) or receipt.get("identity_source") not in hc.IDENTITIES
                or receipt.get("context_epoch") == candidate["source_context_epoch"]
                or not hc.number(receipt.get("started_at"))
                or receipt["started_at"] < observation["retired_at"]):
            raise hc.ReadFailure()
        try:
            hc.identifier(receipt.get("context_epoch"))
        except ValueError:
            raise hc.ReadFailure() from None
    observation["goal_phase"] = phase
    observation["resume_started"] = receipt
    # A committed handoff can legally retain running background Jobs. Opening
    # a successor to observe/reconcile them does not claim or replay business.
    observation["business_reconciliation_required"] = observation["reason"] == "reconciliation"
    if observation["business_reconciliation_required"]:
        observation["reason"] = None
    if receipt is not None or observation["phase"] in ("B_CLAIMED", "BUSINESS_VERIFIED"):
        observation["decision"] = "resumed"
    elif phase not in ACTIVE_PHASES:
        observation["decision"] = "cancelled"
    else:
        # Historical one-shot records remain observable, but an older Server
        # cannot provide the candidate revocation and commit contract required
        # for another automatic launch.
        observation["decision"] = "blocked"
        observation["reason"] = "handoff_protocol_required"
    return observation


class ThinkingPolicyChanged(ValueError):
    pass


class Store:
    def __init__(self, path):
        self.path = Path(path).resolve()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(self.path, timeout=15)
        self.db.row_factory = sqlite3.Row
        self.db.executescript("""
            CREATE TABLE IF NOT EXISTS scheduler (
                id INTEGER PRIMARY KEY CHECK(id=1), endpoint TEXT NOT NULL,
                mode TEXT NOT NULL, generation INTEGER NOT NULL DEFAULT 0,
                failures INTEGER NOT NULL DEFAULT 0, reason TEXT, armed_at INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS dispatch (
                project TEXT NOT NULL, goal_id TEXT NOT NULL, source_context_epoch TEXT NOT NULL,
                checkpoint_sequence INTEGER NOT NULL, checkpoint_revision INTEGER NOT NULL,
                source_fence INTEGER NOT NULL, retired_at INTEGER NOT NULL, deadline_at INTEGER NOT NULL,
                launch_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL, reason TEXT,
                snapshot TEXT NOT NULL, request_file TEXT, launch_started_at INTEGER,
                PRIMARY KEY(project,goal_id,source_context_epoch,checkpoint_sequence));
            CREATE TABLE IF NOT EXISTS goal_thinking_policy (
                project TEXT NOT NULL, goal_id TEXT NOT NULL, selection TEXT NOT NULL,
                PRIMARY KEY(project,goal_id));
        """)
        if 'armed_at' not in {row[1] for row in self.db.execute('PRAGMA table_info(scheduler)')}:
            self.db.execute('ALTER TABLE scheduler ADD COLUMN armed_at INTEGER NOT NULL DEFAULT 0')
            self.db.commit()
        if 'retry_at' not in {row[1] for row in self.db.execute('PRAGMA table_info(dispatch)')}:
            self.db.execute('ALTER TABLE dispatch ADD COLUMN retry_at INTEGER')
            self.db.commit()
        import handoff_reliability as reliable
        reliable.initialize(self)

    def close(self):
        self.db.close()

    def freeze_thinking_policy(self, request):
        """A later retry/epoch cannot turn a known child into a default main."""
        selection = thinking_selection_json(request)
        keys = (request["project"], request["goal_id"])
        with self.db:
            self.db.execute("INSERT OR IGNORE INTO goal_thinking_policy(project,goal_id,selection) VALUES(?,?,?)",
                (*keys, selection))
            frozen = self.db.execute("SELECT selection FROM goal_thinking_policy WHERE project=? AND goal_id=?", keys).fetchone()[0]
            if frozen != selection:
                raise ThinkingPolicyChanged("thinking_policy_changed")

    def initialize(self, origin, armed_at=0):
        origin = hc.endpoint(origin)
        with self.db:
            self.db.execute("INSERT OR IGNORE INTO scheduler(id,endpoint,mode,armed_at) VALUES(1,?,'running',?)", (origin, int(armed_at)))
        if self.status()["endpoint"] != origin:
            raise ValueError("Scheduler DB is bound to another server")

    def status(self):
        row = self.db.execute("SELECT * FROM scheduler WHERE id=1").fetchone()
        if row is None:
            raise ValueError("Initialize with watch --endpoint")
        return dict(row)

    def entries(self):
        return [dict(row) for row in self.db.execute("SELECT * FROM dispatch ORDER BY deadline_at,launch_id")]

    def entry(self, launch_id):
        row = self.db.execute("SELECT * FROM dispatch WHERE launch_id=?", (launch_id,)).fetchone()
        return dict(row) if row else None

    def set_mode(self, mode):
        if mode not in ("running", "paused", "stopped"):
            raise ValueError("Invalid mode")
        with self.db:
            self.db.execute("UPDATE scheduler SET mode=?,generation=generation+1 WHERE id=1", (mode,))

    def pause_if_running(self):
        """An interrupt cannot overwrite a newer explicit STOP or PAUSE."""
        with self.db:
            self.db.execute("""UPDATE scheduler SET mode='paused',generation=generation+1
                WHERE id=1 AND mode='running'""")

    def failure(self, reason):
        with self.db:
            self.db.execute("""UPDATE scheduler SET failures=failures+1,reason=?,
                mode=CASE WHEN failures+1>=3 AND mode='running' THEN 'paused' ELSE mode END WHERE id=1""", (reason,))

    def clear_failures(self):
        with self.db:
            self.db.execute("UPDATE scheduler SET failures=0,reason=NULL WHERE id=1 AND mode='running'")

    def observe(self, candidate, observation):
        keys = tuple(candidate[k] for k in ("project", "goal_id", "source_context_epoch", "checkpoint_sequence"))
        with self.db:
            self.db.execute("BEGIN IMMEDIATE")
            row = self.db.execute("""SELECT * FROM dispatch WHERE project=? AND goal_id=?
                AND source_context_epoch=? AND checkpoint_sequence=?""", keys).fetchone()
            if row:
                for key in ("checkpoint_revision", "source_fence", "retired_at"):
                    if row[key] != observation[key]:
                        raise hc.ReadFailure()
                status = row["status"]
                # Successor evidence can settle an unknown launch, but a weaker
                # read cannot resurrect a cancelled or already dispatched one.
                if observation["decision"] == "resumed":
                    status = "resumed"
                elif observation["decision"] == "cancelled" and status not in TERMINAL_DISPATCH:
                    status = "cancelled"
                self.db.execute("UPDATE dispatch SET status=?,snapshot=?,reason=? WHERE launch_id=?",
                    (status, json.dumps(observation), observation["reason"], row["launch_id"]))
                launch_id = row["launch_id"]
            else:
                launch_id = str(uuid.uuid4())
                status = observation['decision']
                reason = observation['reason']
                if observation['retired_at'] < self.status()['armed_at'] * 1000:
                    status, reason = 'cancelled', 'predates_activation'
                self.db.execute("""INSERT INTO dispatch(project,goal_id,source_context_epoch,checkpoint_sequence,
                    checkpoint_revision,source_fence,retired_at,deadline_at,launch_id,status,reason,snapshot)
                    VALUES(?,?,?,?,?,?,?,?,?,?,?,?)""", (*keys, observation["checkpoint_revision"],
                    observation["source_fence"], observation["retired_at"], (observation["retired_at"] + 999) // 1000 + 120,
                    launch_id, status, reason, json.dumps(observation)))
        return self.entry(launch_id)

    def recover_interrupted(self):
        # Invoked only with the global desktop lock held. A dead process may
        # have sent the message before losing its response; never replay it.
        with self.db:
            self.db.execute("""UPDATE dispatch SET status='unknown',reason='interrupted_launch'
                WHERE status='launch_started'""")
            self.db.execute("""UPDATE dispatch_attempt SET status='unknown',reason='interrupted_launch'
                WHERE status='launch_started'""")


def make_request(entry, now, *, policy_resolver=None):
    source = {key: entry[key] for key in ("project", "goal_id", "source_context_epoch")}
    if policy_resolver is None:
        from chat_task_policy import resolve_goal_profile
        policy_resolver = resolve_goal_profile
    child_policy = policy_resolver(entry["project"], entry["goal_id"])
    prompt = (f"继续工作：在本次新对话恢复已有 Goal，不创建新 Goal。\n"
        f"project={entry['project']}\ngoal_id={entry['goal_id']}\n"
        f"source_context_epoch={entry['source_context_epoch']}\n"
        f"checkpoint={entry['checkpoint_sequence']}\nrevision={entry['checkpoint_revision']}\n"
        f"launch_id={entry['launch_id']}\n"
        "首先按工具契约调用 goal_bootstrap，明确指定上述 goal_id 与 project，activate=true，"
        "使用本次新聊天的上下文身份，登记已开始接续。不要将 source_context_epoch 作为新聊天身份。\n"
        "核对返回的 Goal 和项目，再读取 goal_read(view=handoff, source_context_epoch=上述源值)"
        "核验源已退休及准确的 checkpoint、revision。不匹配时停靠并报告，不猜测其他任务。\n"
        "读取保存的计划、当前状态与原 Job／Run，按当前 phase 继续；规划继续规划，"
        "已确认且未改变的计划无需重新确认，completed 仅报告完成。需要继续业务执行时，"
        "按工具要求读完当前节点、核对原操作、取得本聊天有效的新 attempt/binding 后执行；"
        "不复用旧 binding，不重跑未知结果的原业务，摘要不能替代完整状态与验收证据。\n"
        "登录、验证码或未授权的新权限范围需要人工处理时明确报告并停靠。"
        "launch_id 仅关联本次自动交接；恢复成功与业务完成仍以 Server 回执为准。")
    request = {"schema_version": 1, **source, "checkpoint_sequence": entry["checkpoint_sequence"],
        "checkpoint_revision": entry["checkpoint_revision"], "launch_id": entry["launch_id"],
        "recovery_prompt": prompt, "deadline_at": utc_iso(entry["deadline_at"]),
        "expires_at": utc_iso(now + 600), "thinking_policy": "main_pro_highest"}
    if entry.get("handoff_id"):
        request.update(handoff_id=entry["handoff_id"], attempt_id=entry["attempt_id"],
            attempt_number=entry["attempt_count"], expires_at=utc_iso(entry["ready_deadline_at"] / 1000))
        if entry.get("agent_id"):
            request["agent_id"] = entry["agent_id"]
        request["recovery_prompt"] = (
            "继续同一任务：沿用同一逻辑Agent身份，使用本次新run/新对话的独立可信绑定。"
            "你现在是候选接管者，只能读取和自检，尚未获得业务执行权。\n"
            f"project={entry['project']}\ngoal_id={entry['goal_id']}\n"
            f"source_context_epoch={entry['source_context_epoch']}\n"
            f"checkpoint={entry['checkpoint_sequence']}\nrevision={entry['checkpoint_revision']}\n"
            f"handoff_id={entry['handoff_id']}\nattempt_id={entry['attempt_id']}\n"
            f"launch_id={entry['launch_id']}\n"
            "通过真实 WebCodex 工具连接，使用本次新聊天绑定的可信身份；不得自报或沿用源 context_epoch。\n"
            "先调用 goal_handoff(action=prepare, goal_id=上述Goal, handoff_id=上述交接ID, "
            "attempt_id=上述尝试ID, checkpoint_sequence=上述checkpoint；首次prepare还携带"
            "本次启动消息末尾的launch_claim)。"
            "核验响应中的同一project、Goal、Checkpoint版本、交接包及安全工具通道自检回执。"
            "pending准备阶段只读取本次授权交接包和成果，不读取Memory或会议。"
            "获权后保留同一逻辑Agent的自身Memory与已授权会议，业务写入仍限定本Goal任务；"
            "不得读取其他Agent私有内容。\n"
            "准备就绪后调用 goal_handoff(action=commit，携带同一goal_id/handoff_id/attempt_id/checkpoint_sequence，"
            "package_sha256使用prepare返回并核验的原始交接包SHA256)。"
            "随后调用 goal_handoff(action=status，携带同一goal_id/handoff_id/attempt_id)，"
            "只有服务端明确返回 execution_granted=true 且 handoff.status=committed，"
            "确认执行者为本次身份，才可继续原Goal业务。响应丢失时先status查询，不新建Goal或重放业务。\n"
            "网页创建、消息发送、accepted/pending、口头接管和bootstrap均不是成功。"
            "超时、尝试失效、失败或无执行权时立即停止，不自行重试接管或恢复旧Agent。"
            "接续前按原Goal核对当前节点、完整状态和原Job/Run；不重跑未知结果的原业务。")
    if child_policy is not None:
        request.update(thinking_policy="inherit_child", thinking_profile=child_policy["thinking_profile"],
            chat_task_request_id=child_policy["request_id"], chat_task_id=child_policy["task_id"])
        request["recovery_prompt"] += ("\n这是已登记的独立子任务续聊；保持原子任务的思考档位。"
            f"原子任务request_id={child_policy['request_id']}，task_id={child_policy['task_id']}。"
            "继续使用上述同一Goal；后续达到上下文限度时按原交接机制保存状态，登记的档位会继续继承。")
    else:
        request["recovery_prompt"] += "\n这是主任务上下文续聊，已要求新聊天使用当前可用的Pro最高档。"
    return request


def write_request(path, payload):
    from handoff_flow import ensure_no_launch_claim
    ensure_no_launch_claim(payload)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    temporary.replace(path)


def thinking_selection_is_current(request, resolver=None):
    """Recheck immutable local preferences; this never authorizes a Goal effect."""
    if resolver is None:
        from chat_task_policy import resolve_goal_profile
        resolver = resolve_goal_profile
    try:
        current = resolver(request["project"], request["goal_id"])
        if request.get("thinking_policy") == "inherit_child":
            return bool(current and current["request_id"] == request["chat_task_request_id"]
                and current["task_id"] == request["chat_task_id"]
                and current["thinking_profile"] == request["thinking_profile"])
        return current is None
    except Exception:
        return False


def thinking_selection_json(request):
    return json.dumps({"thinking_policy": request.get("thinking_policy", "main_pro_highest"),
        "thinking_profile": request.get("thinking_profile"),
        "chat_task_request_id": request.get("chat_task_request_id"),
        "chat_task_id": request.get("chat_task_id")}, sort_keys=True, ensure_ascii=False,
        separators=(",", ":"), allow_nan=False)


def poll_once(store, api, launcher, clock=time.time, request_dir=None):
    state = store.status()
    if state["mode"] != "running":
        return state
    request_dir = Path(request_dir or ROOT / "runtime" / "requests")
    try:
        candidates = api.discover()
        # Keep polling launches waiting for permissions even if they fall out
        # of the bounded discovery list. They must not be treated as unsent.
        known = {(c["project"], c["goal_id"], c["source_context_epoch"], c["checkpoint_sequence"]) for c in candidates}
        for entry in store.entries():
            key = tuple(entry[k] for k in ("project", "goal_id", "source_context_epoch", "checkpoint_sequence"))
            if entry["status"] in ("waiting", "waiting_ready", "launch_started", "sent", "unknown") and key not in known:
                candidates.append(dict(zip(("project", "goal_id", "source_context_epoch", "checkpoint_sequence"), key)))
                if entry.get("agent_id"):
                    candidates[-1]["agent_id"] = entry["agent_id"]
        for candidate in candidates:
            if store.status()["mode"] != "running":
                break
            observation = read_observation(api, candidate)
            if "handoff" in observation:
                import handoff_reliability as reliable
                if reliable.poll_candidate(store, api, launcher, candidate, observation, clock, request_dir):
                    break
                continue
            store.observe(candidate, observation)
        store.clear_failures()
    except hc.ReadFailure as error:
        store.failure(error.reason)
    return store.status()


def report(store):
    result = {**store.status(), "dispatches": [{k: row[k] for k in (
        "project", "goal_id", "source_context_epoch", "checkpoint_sequence", "launch_id", "deadline_at", "status", "reason",
        "handoff_id", "attempt_count", "attempt_id", "ready_deadline_at", "agent_id")}
        for row in store.entries()]}
    for entry in result["dispatches"]:
        if entry["status"] == "failed":
            entry["message"] = "交接失败，已停止执行，已保留可人工恢复的状态。只有用户明确要求恢复后才能重新启动。"
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, default=DEFAULT_DB)
    sub = parser.add_subparsers(dest="command", required=True)
    watch = sub.add_parser("watch")
    watch.add_argument("--endpoint")
    watch.add_argument("--interval", type=float, default=5)
    watch.add_argument("--once", action="store_true")
    for command in ("status", "pause", "stop", "resume"):
        sub.add_parser(command)
    args = parser.parse_args(argv)
    store = None
    try:
        if args.command != "watch" and not args.db.is_file():
            raise ValueError("Scheduler DB does not exist")
        store = Store(args.db)
        connection = None
        if args.command == "watch":
            from handoff_control_api import load_credentials
            connection = load_credentials()
            if args.endpoint and hc.endpoint(args.endpoint) != connection[0]:
                raise ValueError("Explicit endpoint conflicts with handoff control configuration")
            store.initialize(connection[0], armed_at=int(time.time()))
        store.status()
        if args.command != "watch":
            if args.command != "status":
                store.set_mode({"pause": "paused", "stop": "stopped", "resume": "running"}[args.command])
            print(json.dumps(report(store), ensure_ascii=False))
            return 0
        if not 0.1 <= args.interval <= 60:
            raise ValueError("Polling interval must be 0.1 to 60 seconds")
        from edge_adapter import launch
        api = Api(connection[0], connection[1])
        (ROOT / "runtime").mkdir(parents=True, exist_ok=True)
        with hc.watcher_lock(ROOT / "runtime" / "desktop-dispatch"):
            store.recover_interrupted()
            while store.status()["mode"] == "running":
                state = poll_once(store, api, launch)
                print(json.dumps(report(store), ensure_ascii=False), flush=True)
                if args.once or state["mode"] != "running":
                    break
                until = time.monotonic() + args.interval
                while time.monotonic() < until and store.status()["mode"] == "running":
                    time.sleep(min(0.25, max(0, until - time.monotonic())))
        return 0 if store.status()["mode"] == "running" else 2
    except (ValueError, OSError, sqlite3.Error, ImportError):
        print("Automatic handoff failed: check config, token environment, adapter, and desktop ownership.", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        if store is not None:
            store.pause_if_running()
        return 130
    finally:
        if store is not None:
            store.close()


from handoff_control_api import Api


if __name__ == "__main__":
    raise SystemExit(main())
