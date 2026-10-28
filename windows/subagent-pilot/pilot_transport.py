"""Isolated one-shot chat pilot. No Goal or collaboration API mutations.

The CLI reads only pilot-request.json beside this file. run() is an internal
Python interface for the fixed facade and offline dependency-injected tests.
"""
from __future__ import annotations

import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parent
PRODUCTION = Path(os.environ.get("WEBCODEX_HANDOFF_ROOT", "") or str(ROOT.parent / "chat-handoff"))
RUNTIME = Path(os.environ.get("WEBCODEX_HANDOFF_RUNTIME", "") or str(PRODUCTION / "runtime"))
CONFIG = ROOT / "pilot-request.json"
sys.path.insert(0, str(ROOT.parent / "chat-handoff"))
import approval_guard
import edge_handoff
import edge_tools
import handoff_controller
import handoff_flow
import playwright_extension_browser
from host_enrollment import (
    EnrollmentDelivery, build_enrollment_message, ensure_no_launch_grant,
    sanitize_candidate_response,
)

FIELDS = {"launch_id", "parent_session_id", "task_id", "prompt", "expires_at"}
OPTIONAL_FIELDS = {"thinking_profile"}
ALLOWED_APPROVALS = {"Runtime Status", "runtime_status"}
FINAL_JS = r'''function() {
  const visible = e => !!e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden';
  // Current ChatGPT markup, observed on the exact pilot reply; retain legacy fallback.
  const modern = [...document.querySelectorAll('[data-markdown-text-style="assistant-message"]')];
  const messages = modern.length ? modern : [...document.querySelectorAll('[data-message-author-role="assistant"]')];
  const current = messages[messages.length - 1];
  const stops = [...document.querySelectorAll('button')].filter(visible).filter(e =>
    e.getAttribute('data-testid') === 'stop-button' ||
    /^(Stop|Stop generating|停止|停止生成)$/.test(e.getAttribute('aria-label') || ''));
  return {url:location.href, stopped:stops.length === 0,
    text:current ? (current.innerText || current.textContent || '').slice(0, 16385) : ''};
}'''


def validate(config):
    ensure_no_launch_grant(config)
    if not isinstance(config, dict) or not FIELDS <= set(config) or set(config) - (FIELDS | OPTIONAL_FIELDS):
        raise ValueError("invalid_request_fields")
    value = dict(config)
    if str(uuid.UUID(value["launch_id"])) != value["launch_id"]:
        raise ValueError("invalid_launch_id")
    if not isinstance(value["parent_session_id"], str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,200}", value["parent_session_id"]):
        raise ValueError("invalid_parent_session_id")
    if not isinstance(value["task_id"], str) or not re.fullmatch(r"wc_(?:probe|chat)_[0-9a-f]{32}", value["task_id"]):
        raise ValueError("invalid_task_id")
    prompt = value["prompt"]
    if (not isinstance(prompt, str) or not 1 <= len(prompt) <= 32768
            or value["launch_id"] not in prompt or value["task_id"] not in prompt):
        raise ValueError("prompt_missing_identity")
    if not isinstance(value["expires_at"], str):
        raise ValueError("invalid_expiry")
    handoff_flow.parse_time(value["expires_at"])
    if "thinking_profile" in value:
        profile = value["thinking_profile"]
        if not isinstance(profile, dict) or not isinstance(profile.get("profile_id"), str):
            raise ValueError("invalid_thinking_profile")
        if len(json.dumps(profile, ensure_ascii=False, allow_nan=False).encode("utf-8")) > 8192:
            raise ValueError("thinking_profile_too_large")
    return value


def create_record(path, value):
    """Exclusive and durable: never replace a prior intent or receipt."""
    ensure_no_launch_grant(value)
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, ensure_ascii=False, allow_nan=False)
        output.flush()
        os.fsync(output.fileno())


class RestrictedApprovals:
    """Keep the existing card verification but reject other tool titles."""
    def __init__(self, browser):
        self.browser = browser

    def __getattr__(self, name):
        return getattr(self.browser, name)

    def execute_javascript(self, script, argument=None):
        value = self.browser.execute_javascript(script, argument)
        if script == approval_guard.PROBE_JS:
            value = json.loads(value) if isinstance(value, str) else value
            for card in value.get("cards", []):
                if card.get("tool") not in ALLOWED_APPROVALS:
                    card["exact_app"] = False
        return value


def collect_final(browser, task_id, marker, target_url, should_stop, deadline,
                  clock=time.monotonic, sleep=time.sleep):
    previous = None
    while clock() < deadline and not should_stop():
        value = browser.execute_javascript(FINAL_JS)
        value = json.loads(value) if isinstance(value, str) else value
        if approval_guard.chat_url(value.get("url")) != target_url:
            return {"response_observed": False, "response_reason": "conversation_changed"}
        text = value.get("text")
        ready = (value.get("stopped") is True and isinstance(text, str)
                 and 0 < len(text) <= 16384 and task_id in text and marker in text)
        if ready and text == previous:
            return sanitize_candidate_response({
                "response_observed": True, "response_text": text,
                "response_reason": "stable_candidate_only",
            })
        previous = text if ready else None
        sleep(min(0.5, max(0, deadline - clock())))
    return {"response_observed": False, "response_reason": "observation_ended"}


def run(config, evidence_root=ROOT / "evidence", *, collect_response=False,
        response_marker="WEBCODEX_PILOT_RESULT", session_factory=None,
        state_reader=None, lock_factory=None, approval_runner=None,
        external_stop=lambda: False, approval_adapter=None,
        enrollment_delivery: EnrollmentDelivery | None = None,
        enrollment_delivery_provider=None, runtime_root=None):
    config = validate(config)
    if enrollment_delivery is not None and enrollment_delivery_provider is not None:
        raise ValueError("enrollment_delivery_ambiguous")
    if enrollment_delivery_provider is not None and not callable(enrollment_delivery_provider):
        raise ValueError("invalid_enrollment_delivery_provider")
    # The delivery is an internal, short-lived capability. Only the exact
    # target's temporary message receives it; config and evidence stay clean.
    message = (build_enrollment_message(config["prompt"], enrollment_delivery)
               if enrollment_delivery is not None else config["prompt"])
    runtime = Path(runtime_root) if runtime_root is not None else RUNTIME
    session_factory = session_factory or playwright_extension_browser.session
    state_reader = state_reader or (lambda: edge_tools.scheduler_state(runtime))
    lock_factory = lock_factory or handoff_controller.watcher_lock
    approval_runner = approval_runner or approval_guard.run
    approval_adapter = approval_adapter or RestrictedApprovals
    identity = {key: config[key] for key in ("launch_id", "parent_session_id", "task_id")}
    folder = Path(evidence_root) / config["launch_id"]
    deadline = time.monotonic() + 240
    boundary = False
    issued = False
    failure_stage = "preflight"
    prior_stop = handoff_flow._SHOULD_STOP

    def result(status, reason, **extra):
        value = {**identity, "status": status, "reason": reason,
                 "business_verified": False, "recorded_at": handoff_flow.timestamp(), **extra}
        ensure_no_launch_grant(value)
        return value

    try:
        with contextlib.ExitStack() as stack:
            stack.enter_context(lock_factory(runtime / "edge-dispatch"))
            if folder.exists():
                return result("unknown", "launch_id_already_used")
            initial = state_reader()
            if initial.get("mode") != "running" or type(initial.get("generation")) is not int:
                return result("blocked", "scheduler_not_running", launched=False,
                              failure_stage="preflight", send_boundary_reached=False)

            def should_stop():
                try:
                    return (external_stop() or state_reader() != initial or time.monotonic() >= deadline
                            or handoff_flow.parse_time(config["expires_at"]) <= handoff_flow.utc_now()
                            or (not boundary and enrollment_delivery is not None
                                and int(time.time() * 1000) >= enrollment_delivery.expires_at_unix_ms))
                except Exception:
                    return True

            if should_stop():
                return result("blocked", "stopped_before_launch", launched=False,
                              failure_stage="preflight", send_boundary_reached=False)
            folder.parent.mkdir(parents=True, exist_ok=True)
            folder.mkdir()  # atomic reservation; even a partial issued file forbids replay
            issued = True
            digest = hashlib.sha256(json.dumps(config, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
            create_record(folder / "issued.json", result("issued", "one_shot_reserved", request_sha256=digest,
                scheduler_generation=initial["generation"]))
            handoff_flow._SHOULD_STOP = should_stop
            stack.callback(setattr, handoff_flow, "_SHOULD_STOP", prior_stop)
            edge = stack.enter_context(session_factory())
            if should_stop():
                raise RuntimeError("stopped_before_page")
            browser = edge.create("https://chatgpt.com/")
            browser.activate()
            failure_stage = "configure_chat"
            setup = configure_task_chat(browser, config)
            create_record(folder / "browser.json", {**identity, **setup, **browser.identity()})
            failure_stage = "prepare_composer"
            state = edge_handoff.wait_composer(browser, config, min(deadline, time.monotonic() + 30))
            if not edge_handoff.empty_app_composer(state):
                raise RuntimeError("new_chat_not_empty")
            editor = browser.find_by_xpath(state["xpath"], timeout=3)
            editor.click(delay_after=0, before_actual=lambda: not should_stop())
            state = edge_handoff.composer(browser)
            if not all(state.get(key) for key in ("focused", "document_focused", "document_visible")) or should_stop():
                raise RuntimeError("composer_not_ready")
            failure_stage = "select_app_mention"
            if not state.get("mention"):
                browser.keys("@webcodex", contains_hotkey=False, force_ime_eng=True, delay_after=0.5)
                if should_stop():
                    raise RuntimeError("stopped_before_mention")
                browser.keys("{SPACE}", contains_hotkey=True, delay_after=0.5)
            state = edge_handoff.composer(browser)
            if not state.get("mention") or should_stop():
                raise RuntimeError("mention_not_selected")
            failure_stage = "prepare_message"
            if enrollment_delivery_provider is not None:
                def enrollment_target_ready(snapshot):
                    return (snapshot.get("user_count") == 0 and snapshot.get("mention") is True
                            and edge_handoff.empty_app_composer(snapshot)
                            and all(snapshot.get(key) is True for key in
                                    ("focused", "document_focused", "document_visible")))
                if not enrollment_target_ready(state):
                    raise RuntimeError("enrollment_target_not_ready")
                enrollment_delivery = enrollment_delivery_provider()
                message = build_enrollment_message(config["prompt"], enrollment_delivery)
                fresh = edge_handoff.composer(browser)
                if (should_stop() or not enrollment_target_ready(fresh)
                        or fresh.get("url") != state.get("url") or fresh.get("xpath") != state.get("xpath")):
                    raise RuntimeError("stopped_after_enrollment_delivery")
                state = fresh
            browser.find_by_xpath(state["xpath"], timeout=3).clipboard_input(message, append=True, delay_after=0.5)
            state = edge_handoff.composer(browser)
            send = browser.find_by_xpath(state["send_xpath"], timeout=3)
            failure_stage = "verify_send_target"
            fresh = edge_handoff.composer(browser)
            if (not edge_handoff.exact_recovery_input(fresh, message)
                    or not all(fresh.get(key) for key in ("focused", "document_focused", "document_visible", "send_enabled"))
                    or fresh.get("send_xpath") != state["send_xpath"] or should_stop()):
                raise RuntimeError("send_target_changed")
            create_record(folder / "send-intent.json", result("issued", "before_send_boundary"))
            failure_stage = "send"

            def before_send():
                nonlocal boundary
                if should_stop():
                    return False
                boundary = True
                return True

            send.click(delay_after=0, before_actual=before_send)
            if not boundary:
                raise RuntimeError("send_not_started")
            failure_stage = "observe_approval"
            # Legacy goal_id is only a DOM correlation token in approval_guard.
            # task_id is NOT a Server Goal, Todo, attempt, binding, or resume.
            receipt = sanitize_candidate_response(approval_runner(approval_adapter(browser),
                {"launch_id": config["launch_id"], "goal_id": config["task_id"]},
                should_stop=should_stop, timeout=min(120, max(1, deadline - time.monotonic()))))
            ensure_no_launch_grant(receipt)
            if receipt.get("status") not in {"sent", "unknown", "blocked"}:
                raise RuntimeError("invalid_approval_receipt")
            outcome = {**receipt, **identity, "business_verified": False,
                       "recorded_at": handoff_flow.timestamp()}
            target_url = approval_guard.chat_url(receipt.get("target_url"))
            if collect_response and receipt["status"] == "sent" and target_url:
                try:
                    outcome.update(collect_final(browser, config["task_id"], response_marker,
                                                 target_url, should_stop, deadline))
                except Exception:
                    outcome.update(response_observed=False, response_reason="observation_failed")
            outcome = sanitize_candidate_response(outcome)
            ensure_no_launch_grant(outcome)
            create_record(folder / "result.json", outcome)
            return outcome
    except Exception as error:
        outcome = result("unknown" if boundary or issued else "blocked",
                         "pilot_" + type(error).__name__, send_boundary_reached=boundary,
                         failure_stage=failure_stage)
        if issued:
            try:
                create_record(folder / "result.json", outcome)
            except OSError:
                outcome["reason"] = "receipt_persistence_unknown"
        return outcome


def configure_task_chat(browser, config):
    """Optional adjustment before send; never use main-Goal Pro policy here."""
    selection = config.get("thinking_profile")
    if selection is None or selection.get("mode") == "default":
        return {"thinking_adjustment": {"status": "not_requested"}}
    if selection.get("setting_error"):
        return {"thinking_adjustment": {"status": "not_adjusted",
                                       "reason": "thinking_profile_unavailable"}}
    try:
        result = edge_handoff.configure_chat(browser, config, browser.keys)
        if result.get("thinking_profile") != selection:
            raise ValueError("unconfirmed_setting")
        return {**result, "thinking_adjustment": {"status": "applied"}}
    except Exception as error:
        # A partial UI change cannot be claimed as applied or rolled back.
        # Dismiss the menu, then the normal fresh composer/stop checks still
        # guard delivery. Never retry this send because of a setting error.
        try:
            if not handoff_flow.cancelled(config):
                browser.keys("{ESC}", contains_hotkey=True, delay_after=0)
        except Exception:
            pass
        return {"thinking_adjustment": {"status": "not_adjusted",
                                       "reason": "setting_" + type(error).__name__}}


def main():
    config = edge_tools.read_json(CONFIG)
    print(json.dumps(run(config, collect_response=True), ensure_ascii=False))


if __name__ == "__main__":
    main()
