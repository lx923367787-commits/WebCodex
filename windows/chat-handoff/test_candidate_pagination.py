"""Offline cursor/worker integration. The real Service uses non-running threads."""
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
import uuid
from unittest.mock import Mock, patch

from cli_fallback_control_api import CandidatePageCursor, FallbackControlApi
from team_agent_dispatch import TeamAgentControlApi
import cli_fallback_worker as worker

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "chat-agents"))
import chat_agents

PROJECT = "agent:runner:project"


class NoThread:
    def __init__(self, **_):
        pass

    def start(self):
        pass

    def is_alive(self):
        return False


class PagedControl:
    """Stand-in for HTTP; durable Store pagination/claims have Rust tests."""
    def __init__(self, identity):
        self.identity = identity
        self.rows = []
        self.list_requests = []
        for index in range(1, 102):
            self.add(index, "unknown" if index <= 100 else "queued")

    def add(self, index, state):
        self.rows.append({self.identity: "attempt-" + str(index), "project": PROJECT,
                          "state": state, "root_task_id": "root-" + str(index),
                          "source_task_id": "child-" + str(index),
                          "source_attempt_id": "cli-" + str(index),
                          "task": "Inspect existing results and verify the assigned change.",
                          "thinking_profile_id": "manager-selected",
                          "dispatch_attempt_id": str(uuid.uuid4()), "request_id": None,
                          "cancel_requested": False})

    def post(self, request):
        action = request["action"].rsplit("_", 1)[-1]
        if action == "list":
            self.list_requests.append(dict(request))
            cursor = json.loads(request["cursor"]) if request.get("cursor") else {
                "after": 0, "through": len(self.rows)}
            page = self.rows[cursor["after"]:cursor["through"]][:request["limit"]]
            next_after = cursor["after"] + len(page)
            continuation = (json.dumps({"after": next_after, "through": cursor["through"]})
                            if next_after < cursor["through"] else None)
            return {"candidates": [dict(row) for row in page], "next_cursor": continuation}
        row = next(row for row in self.rows if row[self.identity] == request[self.identity])
        if action == "acquire":
            permitted = row["state"] == "queued"
            if permitted:
                row["state"] = "starting"
            return {**row, "state": "claimed", "dispatch_permitted": permitted}
        if action == "record":
            row["request_id"] = request["request_id"]
            row["state"] = "running" if request["state"] == "dispatched" else request["state"]
            return {"recorded": True}
        raise AssertionError("Unexpected control action")


def client(cls, server, path):
    api = object.__new__(cls)
    api.owner_user, api.project = "owner", PROJECT
    api.post = server.post
    api._candidate_cursor = CandidatePageCursor(path)
    return api


class CandidatePaginationTests(unittest.TestCase):
    def test_both_queues_progress_past_100_unknown_roots_across_restarts_without_resending(self):
        for cls, identity in ((FallbackControlApi, "fallback_id"),
                              (TeamAgentControlApi, "team_agent_attempt_id")):
            with self.subTest(queue=identity), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                server = PagedControl(identity)
                cursor_path = root / "cursor.json"
                service = chat_agents.Service(root / "chat", thread_factory=NoThread)
                chat_agents.write_json(root / "chat" / "thinking-profiles.json", {
                    "state": "ready", "observed_at": time.time(),
                    "profiles": [{"profile_id": "manager-selected"}]})
                empty = Mock()
                empty.discover.return_value = []
                for sweep in range(4):
                    # Recreate both host objects every tick, like worker restart.
                    api = client(cls, server, cursor_path)
                    service = chat_agents.Service(root / "chat", thread_factory=NoThread)
                    fallback, ordinary = (api, None) if identity == "fallback_id" else (empty, api)
                    worker.advance(fallback, service, lambda: {"mode": "running"}, ordinary)
                    if sweep == 0:
                        self.assertEqual(len(list((root / "chat").glob("tasks/*/request.json"))), 0)
                    else:
                        self.assertEqual(len(list((root / "chat").glob("tasks/*/request.json"))), 1)
                self.assertIsNotNone(server.rows[100]["request_id"])
                self.assertEqual(server.rows[100]["state"], "unknown")
                self.assertEqual(len(list((root / "chat").glob("tasks/*/jobs/*.json"))), 1)
                self.assertTrue(server.list_requests[1].get("cursor"))

    def test_high_water_cycle_finishes_and_next_cycle_includes_new_roots(self):
        for cls, identity in ((FallbackControlApi, "fallback_id"),
                              (TeamAgentControlApi, "team_agent_attempt_id")):
            with self.subTest(queue=identity), tempfile.TemporaryDirectory() as temporary:
                server = PagedControl(identity)
                api = client(cls, server, Path(temporary) / "cursor.json")
                self.assertEqual(len(api.discover()), 100)
                api.finish_discovery()
                server.add(102, "queued")
                self.assertEqual([row[identity] for row in api.discover()], ["attempt-101"])
                api.finish_discovery()
                self.assertIsNone(api._candidate_cursor.cursor)
                self.assertEqual(len(api.discover()), 100)
                api.finish_discovery()
                self.assertEqual([row[identity] for row in api.discover()], ["attempt-101", "attempt-102"])

    def test_uncommitted_page_repeats_and_pause_does_not_advance_checkpoint(self):
        with tempfile.TemporaryDirectory() as temporary:
            server = PagedControl("fallback_id")
            path = Path(temporary) / "cursor.json"
            first = client(FallbackControlApi, server, path)
            first.discover()
            repeated = client(FallbackControlApi, server, path)
            self.assertEqual(len(repeated.discover()), 100)
            self.assertNotIn("cursor", server.list_requests[-1])
            states = iter([{"mode": "running"}, {"mode": "paused"}])
            service = Mock()
            with patch.object(worker, "dispatch") as dispatch:
                result = worker.advance(repeated, service, lambda: next(states))
                self.assertEqual(result["state"], "paused")
                dispatch.assert_not_called()
            self.assertFalse(path.exists())

    def test_malformed_or_missing_continuation_does_not_advance_cursor(self):
        with tempfile.TemporaryDirectory() as temporary:
            api = client(FallbackControlApi, PagedControl("fallback_id"), Path(temporary) / "cursor.json")
            for bad in ({"candidates": []}, {"candidates": [], "next_cursor": 1},
                        {"candidates": [], "next_cursor": "x" * 513}):
                api.post = Mock(return_value=bad)
                with self.assertRaises(ValueError):
                    api.discover()
                self.assertIsNone(api._candidate_cursor.cursor)


if __name__ == "__main__":
    unittest.main()
