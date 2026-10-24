"""Transport bookkeeping for the Server-owned, bounded handoff protocol.

The Server alone expires candidates and grants execution. This module only
deduplicates calls to the existing browser adapter and polls authoritative state.
"""
from __future__ import annotations

import json
from pathlib import Path
import uuid

import handoff_controller as hc

MAX_ATTEMPTS = 4
LAUNCH_DELAY_SECS = 120
STATES = {"waiting", "awaiting_ready", "prepared", "committed", "failed"}
FIELDS = ("handoff_id", "goal_id", "project", "source_context_epoch", "checkpoint_sequence",
          "plan_version", "status", "attempt_count", "attempt_id", "candidate_context_epoch",
          "deadline_at", "timeout_secs", "prepared_at", "execution_owner_epoch", "failure_reason",
          "created_at", "committed_at", "agent_id")
KEYS = ("project", "goal_id", "source_context_epoch", "checkpoint_sequence")


def validate_record(raw, candidate):
    if not isinstance(raw, dict) or raw.get("status") not in STATES:
        raise hc.ReadFailure()
    for key in KEYS:
        if raw.get(key) != candidate[key]:
            raise hc.ReadFailure()
    try:
        hc.identifier(raw.get("handoff_id"))
        count = raw.get("attempt_count")
        if not hc.number(count) or count > MAX_ATTEMPTS:
            raise ValueError()
        if not hc.number(raw.get("created_at")) or not hc.number(raw.get("timeout_secs")) or not raw["timeout_secs"]:
            raise ValueError()
        attempt = raw.get("attempt_id")
        if attempt is not None and str(uuid.UUID(attempt)) != attempt:
            raise ValueError()
        if raw["status"] in ("awaiting_ready", "prepared") and (
                not attempt or not count or not hc.number(raw.get("deadline_at"))):
            raise ValueError()
        if raw["status"] == "committed":
            owner = hc.identifier(raw.get("execution_owner_epoch"))
            if owner == candidate["source_context_epoch"] or not hc.number(raw.get("committed_at")):
                raise ValueError()
    except (ValueError, TypeError, AttributeError):
        raise hc.ReadFailure() from None
    # No task content, paths, or arbitrary HTTP response fields are persisted.
    result = {key: raw.get(key) for key in FIELDS}
    reason = result.get("failure_reason")
    result["failure_reason"] = reason if isinstance(reason, str) and len(reason) <= 120 and reason.replace("_", "").isalnum() else None
    return result


def observation(raw, candidate):
    if not isinstance(raw, dict) or raw.get("driver") != "host" or raw.get("projection") != "handoff":
        raise hc.ReadFailure()
    goal, source, cp = raw.get("goal"), raw.get("source_context"), raw.get("checkpoint")
    if not all(isinstance(value, dict) for value in (goal, source, cp)):
        raise hc.ReadFailure()
    if goal.get("goal_id") != candidate["goal_id"] or goal.get("project") != candidate["project"]:
        raise hc.ReadFailure()
    if source.get("context_epoch") != candidate["source_context_epoch"] or cp.get("source_context_epoch") != candidate["source_context_epoch"]:
        raise hc.ReadFailure()
    if cp.get("sequence") != candidate["checkpoint_sequence"] or cp.get("action") != "handoff":
        raise hc.ReadFailure()
    if not all(hc.number(cp.get(key)) for key in ("revision", "created_at")):
        raise hc.ReadFailure()
    if cp.get("source_fence") is not None and not hc.number(cp["source_fence"]):
        raise hc.ReadFailure()
    if source.get("lifecycle_state") not in ("draining", "retired", "stopped"):
        raise hc.ReadFailure()
    if goal.get("phase") not in {"planning", "awaiting_confirmation", "ready", "executing", "verifying", "paused", "blocked", "completed", "cancelled"}:
        raise hc.ReadFailure()
    return {"handoff": validate_record(raw.get("handoff"), candidate), "goal_phase": goal["phase"],
            "checkpoint_revision": cp["revision"], "source_fence": cp["source_fence"]}


def initialize(store):
    columns = {row[1] for row in store.db.execute("PRAGMA table_info(dispatch)")}
    with store.db:
        for key, declaration in (("handoff_id", "TEXT"), ("attempt_count", "INTEGER NOT NULL DEFAULT 0"),
                                 ("attempt_id", "TEXT"), ("ready_deadline_at", "INTEGER"), ("agent_id", "TEXT")):
            if key not in columns:
                store.db.execute(f"ALTER TABLE dispatch ADD COLUMN {key} {declaration}")
        store.db.execute("""CREATE TABLE IF NOT EXISTS dispatch_attempt (
            attempt_id TEXT PRIMARY KEY, handoff_id TEXT NOT NULL, attempt_number INTEGER NOT NULL,
            status TEXT NOT NULL, launched INTEGER, reason TEXT, request_file TEXT NOT NULL,
            started_at INTEGER NOT NULL, UNIQUE(handoff_id,attempt_number))""")


def observe(store, candidate, value):
    h = value["handoff"]
    agent_id = candidate.get("agent_id") or h.get("agent_id")
    if candidate.get("agent_id") and h.get("agent_id") != candidate["agent_id"]:
        raise hc.ReadFailure()
    keys = tuple(candidate[key] for key in KEYS)
    status = {"waiting": "waiting", "awaiting_ready": "waiting_ready", "prepared": "waiting_ready",
              "committed": "resumed", "failed": "failed"}[h["status"]]
    reason = h["failure_reason"] if status == "failed" else None
    # The legacy transport column is NOT NULL. -1 represents an absent planning
    # fence locally; the authoritative snapshot retains null and grants nothing.
    observed_fence = value["source_fence"] if value["source_fence"] is not None else -1
    with store.db:
        store.db.execute("BEGIN IMMEDIATE")
        previous = store.db.execute("SELECT * FROM dispatch WHERE project=? AND goal_id=? AND source_context_epoch=? AND checkpoint_sequence=?", keys).fetchone()
        if previous:
            if (previous["handoff_id"] not in (None, h["handoff_id"])
                    or previous["checkpoint_revision"] != value["checkpoint_revision"]
                    or previous["source_fence"] != observed_fence
                    or previous["agent_id"] not in (None, agent_id)
                    or previous["attempt_count"] > h["attempt_count"]
                    or (previous["status"] in ("resumed", "failed") and previous["status"] != status)):
                raise hc.ReadFailure()
            launch_id = h["attempt_id"] or previous["launch_id"]
        else:
            launch_id = h["attempt_id"] or str(uuid.uuid4())
        if h["created_at"] < store.status()["armed_at"] * 1000 and status not in ("resumed", "failed"):
            status, reason = "cancelled", "predates_activation"
        deadline = (h["created_at"] + 999) // 1000 + LAUNCH_DELAY_SECS
        store.db.execute("""INSERT INTO dispatch(project,goal_id,source_context_epoch,checkpoint_sequence,
            checkpoint_revision,source_fence,retired_at,deadline_at,launch_id,status,reason,snapshot,
            handoff_id,attempt_count,attempt_id,ready_deadline_at,agent_id)
            VALUES(?,?,?,?,?,?,0,?,?,?,?,?,?,?,?,?,?)
            ON CONFLICT(project,goal_id,source_context_epoch,checkpoint_sequence) DO UPDATE SET
            launch_id=excluded.launch_id,status=excluded.status,reason=excluded.reason,snapshot=excluded.snapshot,
            handoff_id=excluded.handoff_id,attempt_count=excluded.attempt_count,
            attempt_id=excluded.attempt_id,ready_deadline_at=excluded.ready_deadline_at,agent_id=excluded.agent_id""",
            (*keys, value["checkpoint_revision"], observed_fence, deadline, launch_id, status, reason,
             json.dumps(value), h["handoff_id"], h["attempt_count"], h["attempt_id"], h["deadline_at"], agent_id))
    return store.entry(launch_id)


def attempt(store, attempt_id):
    row = store.db.execute("SELECT * FROM dispatch_attempt WHERE attempt_id=?", (attempt_id,)).fetchone()
    return dict(row) if row else None


def claim(store, entry, generation, now, path):
    with store.db:
        store.db.execute("BEGIN IMMEDIATE")
        state = store.status()
        if state["mode"] != "running" or state["generation"] != generation:
            return False
        return store.db.execute("""INSERT OR IGNORE INTO dispatch_attempt
            (attempt_id,handoff_id,attempt_number,status,request_file,started_at)
            VALUES(?,?,?,'launch_started',?,?)""",
            (entry["attempt_id"], entry["handoff_id"], entry["attempt_count"], str(path), now)).rowcount == 1


def finish(store, attempt_id, outcome):
    status = outcome.get("status") if isinstance(outcome, dict) else None
    if status not in ("sent", "unknown", "blocked", "retryable"):
        status = "unknown"
    launched = outcome.get("launched") if isinstance(outcome, dict) else None
    if launched not in (True, False):
        launched = None
    with store.db:
        store.db.execute("""UPDATE dispatch_attempt SET status=?,launched=?,reason=?
            WHERE attempt_id=? AND status='launch_started'""",
            (status, launched, None if status == "sent" else "adapter_not_confirmed", attempt_id))


def refresh(store, api, candidate, value, action="status", reason=None):
    h = value["handoff"]
    params = {"goal_id": candidate["goal_id"], "action": action, "handoff_id": h["handoff_id"]}
    if h["attempt_id"] is not None:
        params["attempt_id"] = h["attempt_id"]
    if reason:
        params["reason"] = reason
    try:
        result = api.call("goal_handoff", params)
    except hc.ReadFailure:
        if action == "status":
            raise
        # The mutation may already have committed. Query before deciding to
        # reserve/fail another candidate; never blindly replay a lost response.
        result = api.call("goal_handoff", {"goal_id": candidate["goal_id"], "action": "status", "handoff_id": h["handoff_id"]})
    if not isinstance(result, dict):
        raise hc.ReadFailure()
    value = {**value, "handoff": validate_record(result.get("handoff"), candidate)}
    if value["handoff"]["handoff_id"] != h["handoff_id"]:
        raise hc.ReadFailure()
    return value, observe(store, candidate, value)


def poll_candidate(store, api, launcher, candidate, value, clock, request_dir, *, allow_advance=True, expected_attempt_id=None):
    import auto_handoff as ah

    entry = observe(store, candidate, value)
    if entry["status"] in ("resumed", "failed", "cancelled") or clock() < entry["deadline_at"]:
        return False
    generation = store.status()["generation"]
    value, entry = refresh(store, api, candidate, value)
    if entry["status"] in ("resumed", "failed", "cancelled"):
        return False
    if value["goal_phase"] not in ah.ACTIVE_PHASES:
        refresh(store, api, candidate, value, "abort", "goal_not_active")
        return False
    if store.status()["mode"] != "running" or store.status()["generation"] != generation:
        return False
    h = value["handoff"]
    if expected_attempt_id is not None and h["attempt_id"] != expected_attempt_id:
        return False
    old = attempt(store, h["attempt_id"]) if h["attempt_id"] else None
    # Positive no-launch evidence allows prompt progression, but still only
    # after a fresh Server status and successful revocation of that candidate.
    if allow_advance and old and old["launched"] == 0 and old["status"] in ("blocked", "retryable"):
        value, entry = refresh(store, api, candidate, value, "fail_attempt", "launch_failed")
        h = value["handoff"]
    if h["status"] in ("committed", "failed"):
        return False
    if old and h["attempt_id"] == old["attempt_id"] and h["status"] != "waiting" and clock() * 1000 < h["deadline_at"]:
        return False
    if allow_advance:
        value, entry = refresh(store, api, candidate, value, "begin_attempt")
    h = value["handoff"]
    if h["status"] not in ("awaiting_ready", "prepared") or clock() * 1000 >= h["deadline_at"]:
        return False
    path = Path(request_dir) / (h["attempt_id"] + ".json")
    if not claim(store, entry, generation, int(clock()), path):
        return False
    request = None
    read_failure = None
    last_check = float("-inf")
    policy_changed = False

    def should_stop(after_send=False, force_refresh=False):
        nonlocal value, entry, last_check, read_failure, policy_changed
        state = store.status()
        if read_failure or state["mode"] != "running" or state["generation"] != generation:
            return True
        if request is not None and not ah.thinking_selection_is_current(request):
            policy_changed = True
            return True
        if clock() * 1000 >= h["deadline_at"]:
            return True
        if force_refresh or clock() - last_check >= 5:
            last_check = clock()
            try:
                value, entry = refresh(store, api, candidate, value)
            except hc.ReadFailure as error:
                read_failure = error
                return True
        current = value["handoff"]
        state = store.status()
        return (state["mode"] != "running" or state["generation"] != generation
                or current["status"] not in ("awaiting_ready", "prepared")
                or current["attempt_id"] != h["attempt_id"])

    try:
        request = ah.make_request(entry, int(clock()))
        store.freeze_thinking_policy(request)
        ah.write_request(path, request)
        if should_stop(force_refresh=True):
            outcome = {"status": "blocked", "launched": False}
        else:
            # This is the same adapter entry point for all four attempts.
            outcome = launcher(path, should_stop)
        if policy_changed:
            value, entry = refresh(store, api, candidate, value, "abort", "thinking_policy_changed")
    except ah.ThinkingPolicyChanged:
        outcome = {"status": "blocked", "launched": False}
        value, entry = refresh(store, api, candidate, value, "abort", "thinking_policy_changed")
    except Exception:
        outcome = {"status": "unknown"}
    finish(store, h["attempt_id"], outcome)
    if read_failure:
        raise read_failure
    # Even a sent/accepted/unknown return is only transport evidence. Re-read
    # committed state now, including when the adapter lost its response.
    refresh(store, api, candidate, value)
    return True
