"""Run the existing ChatGPT task transport for Server-approved recovery work.

This is an explicit local worker entry point, not an import-time service. It
uses the dedicated control credential and existing STOP/PAUSE generation. No
runtime token or arbitrary endpoint is included in a child task or receipt.
"""
from __future__ import annotations

import argparse
from pathlib import Path
import sys
import time

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "chat-agents"))

from cli_fallback_control_api import CandidatePageCursor, FallbackControlApi
from cli_fallback_dispatch import dispatch
from team_agent_dispatch import TeamAgentControlApi, dispatch as dispatch_team_agent
from handoff_control_api import load_credentials
from handoff_controller import watcher_lock


def advance(api, service, state_reader, team_api=None):
    initial = state_reader()
    if initial.get("mode") != "running":
        return {"state": "paused", "observed": 0}
    groups = [(api, dispatch, "fallback_id")]
    if team_api is not None:
        groups.append((team_api, dispatch_team_agent, "team_agent_attempt_id"))
    observed = 0
    failures = 0
    # Both kinds share this exact Service and browser lock; queued recovery has
    # priority without introducing a second sender or changing ordinary quotas.
    for control, send, identity_field in groups:
        try:
            candidates = control.discover()
            if not isinstance(candidates, list) or len(candidates) > 100:
                raise ValueError("invalid_team_worker_candidates")
        except Exception:
            # A rejected cursor or unavailable domain cannot prevent the
            # other queue from making progress. Keep the failed page cursor.
            failures += 1
            continue
        for candidate in candidates:
            if state_reader() != initial:
                return {"state": "paused", "observed": observed}
            # Active requests still need cancellation checks while new queued
            # work waits for the one browser worker.
            if candidate["state"] == "queued" and service.browser_worker_busy():
                continue
            try:
                result = send(control, service, candidate[identity_field])
                if not isinstance(result, dict):
                    raise ValueError("invalid_dispatch_observation")
                observed += 1
            except Exception:
                # This is an attempted observation, not permission to retry a
                # send. Server claims/unknown records still own replay safety.
                failures += 1
                continue
            chat = result.get("chat_state")
            if (isinstance(chat, dict) and result.get("state") == "dispatched"
                    and result.get("request_id") and not chat.get("response_observed")
                    and not chat.get("worker_alive") and not chat.get("cancel_requested")
                    and state_reader() == initial):
                try:
                    service.collect_chat_task(result["request_id"])
                except Exception:
                    # Collection is read-only. One unavailable chat must not
                    # prevent later roots from being observed in this sweep.
                    pass
        # Commit only a fully processed page, including queued work deferred by
        # the busy sender. It will be revisited in the next bounded sweep.
        if state_reader() != initial:
            return {"state": "paused", "observed": observed}
        try:
            control.finish_discovery()
        except Exception:
            failures += 1
    if failures:
        raise ValueError("candidate_processing_failed")
    return {"state": "observed", "observed": observed}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--interval", type=float, default=5)
    parser.add_argument("--manager-bindings", type=Path, required=True,
                        help="Absolute path to exact Manager attach/status or handoff receipts by root")
    args = parser.parse_args(argv)
    if not 1 <= args.interval <= 60:
        parser.error("--interval must be between 1 and 60 seconds")
    from chat_agents import Service
    from edge_tools import scheduler_state
    from run_auto_handoff import bind_connection
    from manager_bindings import RootManagerBindings
    from host_agent_dispatch import HostQueueControlApi
    origin, token, identity = load_credentials()
    api = FallbackControlApi(origin, token)
    team_api = TeamAgentControlApi(origin, token)
    managers = RootManagerBindings.from_file(args.manager_bindings,
        owner_user=api.owner_user, project=api.project)
    runtime = ROOT.parent / "chat-agents" / "runtime"
    service = Service(runtime=runtime, transport_runtime_root=ROOT / "runtime")
    try:
        # Reusing a state directory for another owner/project/server is denied
        # by the same explicit connection binding used by the handoff worker.
        with watcher_lock(runtime / "cli-fallback-worker"):
            bind_connection(runtime / "cli-fallback-worker.sqlite3", origin, identity)
            api._candidate_cursor = CandidatePageCursor(runtime / "cli-fallback-discovery.json")
            team_api._candidate_cursor = CandidatePageCursor(runtime / "team-agent-discovery.json")
            api = HostQueueControlApi(api, "fallback", managers)
            team_api = HostQueueControlApi(team_api, "member", managers)
            while True:
                try:
                    advance(api, service, lambda: scheduler_state(ROOT / "runtime"), team_api)
                except Exception:
                    # State and any dispatch uncertainty remain in Server/local
                    # receipts. Never print exception text or replay a send.
                    if args.once:
                        return 1
                if args.once:
                    return 0
                time.sleep(args.interval)
    finally:
        service.close()


if __name__ == "__main__":
    raise SystemExit(main())
