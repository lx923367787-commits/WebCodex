"""Chat task transport using the verified one-shot input path.

send() is asynchronous-worker work, not a plugin request handler. observe() only
reads the exact recorded conversation; it never sends or approves another tool.
No Goal, Server collaboration record, or production handoff module is changed.
"""
from __future__ import annotations

from pathlib import Path
import datetime as dt
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "subagent-pilot"))
import pilot_transport as pilot
from host_enrollment import ensure_no_launch_grant, sanitize_candidate_response

MARKER = "WEBCODEX_CHAT_TASK_RESULT"
IDENTITY = ("launch_id", "parent_session_id", "task_id")


def _runtime_root(value):
    root = pilot.RUNTIME if value is None else Path(value)
    if not root.is_absolute():
        raise ValueError("runtime_root_must_be_absolute")
    return root


def discover_profiles(cancelled=lambda: False, *, session_factory=None,
        state_reader=None, lock_factory=None, discoverer=None, runtime_root=None):
    """Inspect choices on an owned empty chat; never send a task or guess a model."""
    import thinking_profiles
    session_factory = session_factory or pilot.playwright_extension_browser.persistent_session
    runtime = _runtime_root(runtime_root)
    state_reader = state_reader or (lambda: pilot.edge_tools.scheduler_state(runtime))
    lock_factory = lock_factory or pilot.handoff_controller.watcher_lock
    discoverer = discoverer or thinking_profiles.discover_profiles
    deadline = time.monotonic() + 120
    previous_stop = pilot.handoff_flow._SHOULD_STOP
    try:
        with lock_factory(runtime / "edge-dispatch"):
            initial = state_reader()
            if initial.get("mode") != "running":
                raise ValueError("scheduler_not_running")
            def stopped():
                try:
                    return cancelled() or time.monotonic() >= deadline or state_reader() != initial
                except Exception:
                    return True
            if stopped():
                raise ValueError("discovery_cancelled")
            pilot.handoff_flow._SHOULD_STOP = stopped
            request = {"launch_id": str(uuid.uuid4()),
                "expires_at": (dt.datetime.now(dt.timezone.utc)+dt.timedelta(seconds=120)).isoformat()}
            with session_factory() as session:
                if stopped():
                    raise ValueError("discovery_cancelled")
                owned = session.create(pilot.playwright_extension_browser.CHAT_URL)
                owned.activate()
                result = discoverer(owned, request)
                result["connection_id"] = owned.identity().get("connection_id")
                ensure_no_launch_grant(result)
                return result
    finally:
        pilot.handoff_flow._SHOULD_STOP = previous_stop


def send(config, evidence_root, cancelled=lambda: False, *, enrollment_delivery=None,
         enrollment_delivery_provider=None, runtime_root=None):
    """One-shot send; the result's business_verified remains false.

The existing approval guard still requires the precise WebCodex app/card,
message binding, one visible button, fresh hit testing and cancellation checks.
This identity adapter removes only the pilot's Runtime Status title restriction;
it does not authorize another app or bypass those guard predicates.
"""
    ensure_no_launch_grant(config)
    if enrollment_delivery is not None and enrollment_delivery_provider is not None:
        raise ValueError("enrollment_delivery_ambiguous")
    if enrollment_delivery_provider is not None and not callable(enrollment_delivery_provider):
        raise ValueError("invalid_enrollment_delivery_provider")
    outcome = pilot.run(config, evidence_root, collect_response=True,
        response_marker=MARKER, external_stop=cancelled,
        session_factory=pilot.playwright_extension_browser.persistent_session,
        approval_adapter=lambda browser: browser,
        enrollment_delivery=enrollment_delivery, enrollment_delivery_provider=enrollment_delivery_provider,
        runtime_root=_runtime_root(runtime_root))
    return sanitize_candidate_response(outcome)


def observe(config, receipt, output_path, cancelled=lambda: False, *,
            session_factory=None, state_reader=None, lock_factory=None,
            browser_factory=None, inspector=None, collector=None,
            clock=time.monotonic, sleep=time.sleep, runtime_root=None):
    """Read an observed task message again, even after its send expiry.

output_path is supplied by the trusted service, not by the model. Each read has
a fresh maximum 30-second observation deadline; cached output is atomically
replaced. The caller may use distinct output paths to retain observation history.
"""
    config = pilot.validate(config)
    receipt = sanitize_candidate_response(receipt)
    if not isinstance(receipt, dict) or any(receipt.get(key) != config[key] for key in IDENTITY):
        raise ValueError("receipt_identity_mismatch")
    target = pilot.approval_guard.chat_url(receipt.get("target_url"))
    observable = (receipt.get("status") == "sent"
                  or (receipt.get("status") == "unknown" and receipt.get("message_observed") is True))
    # A bound message can be observed even if the later approval window lost
    # focus. Re-reading that chat never upgrades unknown to sent or resends it.
    if not observable or not target or target != receipt.get("target_url"):
        raise ValueError("no_verified_sent_chat")
    session_factory = session_factory or pilot.playwright_extension_browser.persistent_session
    runtime = _runtime_root(runtime_root)
    state_reader = state_reader or (lambda: pilot.edge_tools.scheduler_state(runtime))
    lock_factory = lock_factory or pilot.handoff_controller.watcher_lock
    browser_factory = browser_factory or pilot.playwright_extension_browser.Browser
    inspector = inspector or pilot.approval_guard.inspect
    collector = collector or pilot.collect_final
    deadline = clock() + 30
    outcome = {key: config[key] for key in IDENTITY}
    outcome.update(target_url=target, business_verified=False, response_observed=False,
                   method="same_chat_read_only_observation")

    def save(reason=None, **extra):
        proposed = dict(outcome)
        if reason:
            proposed["response_reason"] = reason
        proposed.update(extra, recorded_at=pilot.handoff_flow.timestamp())
        public = sanitize_candidate_response(proposed)
        pilot.edge_tools.write_json(Path(output_path), public)
        outcome.clear()
        outcome.update(public)
        return public

    try:
        with lock_factory(runtime / "edge-dispatch"):
            initial = state_reader()
            if initial.get("mode") != "running" or type(initial.get("generation")) is not int:
                return save("scheduler_not_running")

            def stopped():
                try:
                    # Sending expiry is deliberately absent: a long task can
                    # finish later, and this operation cannot resend it.
                    return cancelled() or clock() >= deadline or state_reader() != initial
                except Exception:
                    return True

            if stopped():
                return save("observation_cancelled")
            with session_factory() as session:
                if stopped():
                    return save("observation_cancelled")
                # Same already-sent URL, never a new task/composer. This is the
                # same bounded viewer route verified in recover_result.py.
                session._transport.call("browser_tabs", {"action": "new", "url": target})
                owned = browser_factory(session, str(uuid.uuid4()))
                owned._rpc("initialize")
                identity = owned.identity()
                if isinstance(identity, dict) and isinstance(identity.get("connection_id"), str):
                    outcome["connection_id"] = identity["connection_id"]
                while not stopped():
                    snapshot = inspector(owned, {
                        "launch_id": config["launch_id"], "goal_id": config["task_id"]})
                    # goal_id is solely the legacy DOM text correlation key.
                    # No Server Goal or attempt is created or recovered.
                    if (snapshot.get("bound") is True
                            and pilot.approval_guard.chat_url(snapshot.get("url")) == target):
                        break
                    sleep(min(0.5, max(0, deadline - clock())))
                else:
                    return save("exact_message_not_observed")
                candidate = collector(owned, config["task_id"], MARKER, target,
                                      stopped, deadline, clock=clock, sleep=sleep)
                return save(**candidate)
    except Exception as error:
        # Error messages and MCP response trailers may contain secrets. Persist
        # only controlled exception class names, never str(error) or raw replies.
        return save("observation_" + type(error).__name__)
