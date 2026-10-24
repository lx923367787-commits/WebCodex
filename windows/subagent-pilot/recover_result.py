"""Re-observe the one recorded pilot chat without sending another message."""
import json
from pathlib import Path
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "chat-handoff"))
import approval_guard
import edge_tools
import handoff_controller
import playwright_extension_browser as browser
import pilot_transport


def main():
    config = pilot_transport.validate(edge_tools.read_json(ROOT / "pilot-request.json"))
    folder = ROOT / "evidence" / config["launch_id"]
    receipt = edge_tools.read_json(folder / "result.json")
    if any(receipt.get(key) != config[key] for key in ("launch_id", "parent_session_id", "task_id")):
        raise ValueError("receipt_identity_mismatch")
    target = approval_guard.chat_url(receipt.get("target_url"))
    if receipt.get("status") != "sent" or not target:
        raise ValueError("no_verified_sent_chat")
    initial = edge_tools.scheduler_state(pilot_transport.RUNTIME)
    if initial["mode"] != "running":
        raise ValueError("scheduler_not_running")
    stop = lambda: edge_tools.scheduler_state(pilot_transport.RUNTIME) != initial
    with handoff_controller.watcher_lock(pilot_transport.RUNTIME / "edge-dispatch"):
        with browser.session() as session:
            # New viewer tab of a fixed existing conversation. Never type or send.
            session._transport.call("browser_tabs", {"action": "new", "url": target})
            owned = browser.Browser(session, str(uuid.uuid4()))
            owned._rpc("initialize")
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline and not stop():
                observed = approval_guard.inspect(owned, {
                    "launch_id": config["launch_id"], "goal_id": config["task_id"]})
                if observed.get("bound") and approval_guard.chat_url(observed.get("url")) == target:
                    break
                time.sleep(0.5)
            else:
                raise ValueError("exact_message_not_observed")
            result = pilot_transport.collect_final(owned, config["task_id"], "WEBCODEX_PILOT_RESULT",
                target, stop, deadline)
            result.update(launch_id=config["launch_id"], task_id=config["task_id"],
                target_url=target, business_verified=False, method="same_chat_read_only_recovery")
            # Never store raw MCP trailers: they may include connection credentials.
            pilot_transport.create_record(folder / "response.json", result)
            print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
