"""Register an observed original-chat route through trusted local Host control.

This operator entry point is not a model tool. It reads identity and anchor
counts only; it never reads response/Goal/Memory text or sends a message.
"""
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
import sys
import time
import uuid

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--scheduler-runtime", type=Path, required=True)
    parser.add_argument("--desktop-state", type=Path, required=True)
    parser.add_argument("--control-config", type=Path, required=True)
    parser.add_argument("--registration", type=Path, required=True,
                        help="JSON containing project, target and route metadata")
    args = parser.parse_args()
    if any(not value.is_absolute() for value in vars(args).values()):
        raise ValueError("absolute_operator_paths_required")
    # Use the explicitly selected installed Host and its pinned browser runtime.
    # A source checkout need not contain that Host's local Node dependencies.
    host_code=args.runtime.parent
    handoff_code=args.scheduler_runtime.parent
    if not (host_code/"chat_agents.py").is_file() or not (handoff_code/"handoff_controller.py").is_file():
        raise ValueError("installed_host_code_required")
    sys.path.insert(0,str(host_code))
    sys.path.insert(0,str(handoff_code))
    with args.registration.open("rb") as source:
        raw = source.read(16385)
    if len(raw) > 16384:
        raise ValueError("route_registration_too_large")
    from manager_bindings import _unique_object
    value = json.loads(raw.decode("utf-8-sig"), object_pairs_hook=_unique_object)
    if not isinstance(value, dict) or set(value) != {"project", "target", "route"}:
        raise ValueError("invalid_route_registration")
    import chat_agents
    import team_continuation as continuation
    import team_continuation_transport as transport
    from automatic_dispatch_config import load_context_factory
    from handoff_controller import watcher_lock
    import playwright_extension_browser as browser_api
    continuation.validate_target(value["target"])
    continuation.validate_route(value["route"])
    factory = load_context_factory(args.runtime, desktop_state=args.desktop_state,
                                   control_config=args.control_config, scheduler_runtime=args.scheduler_runtime)
    contexts = [c for c in factory() if c["member_api"].project == value["project"]]
    if len(contexts) != 1:
        raise ValueError("exact_route_project_unavailable")
    context = contexts[0]
    if Path(context["scheduler_runtime"]).resolve() != args.scheduler_runtime.resolve():
        raise ValueError("route_scheduler_scope_mismatch")
    service = chat_agents.Service(args.runtime, transport_runtime_root=args.scheduler_runtime)
    control = continuation.Control(context["member_api"], context["manager_bindings"].host_id_provider)

    def observe(route, target):
        deadline = time.monotonic() + 30
        with watcher_lock(args.scheduler_runtime / "edge-dispatch"):
            with browser_api.persistent_session() as session:
                session._transport.call("browser_tabs", {"action": "new", "url": route["url"]})
                browser = browser_api.Browser(session, str(uuid.uuid4()))
                browser._rpc("initialize")
                browser.activate()
                while time.monotonic() < deadline:
                    proof = transport.inspect(browser, route, target)
                    if continuation.route_matches(route, target, proof):
                        return {key: proof[key] for key in ("url", "anchor_count", "visible")}
                    time.sleep(min(.25, max(0, deadline - time.monotonic())))
        raise ValueError("continuation_route_anchor_unverified")

    result = continuation.register_original_route(service, control, value["target"],
                                                 value["route"], observe=observe)
    print(json.dumps(result))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Exceptions from an HTTP/browser provider can include page content or
        # credentials. Expose only a bounded operator code, never their repr.
        code=str(error)
        print(json.dumps({"registered": False, "error": type(error).__name__,
                          "code":code if re.fullmatch(r"[a-z_]{1,100}",code) else "details_redacted"}))
        raise SystemExit(1)
