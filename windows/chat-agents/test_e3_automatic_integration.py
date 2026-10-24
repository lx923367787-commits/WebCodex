"""Offline E3 carrier -> manager resolver -> queue adapter -> real Service.

Only temporary synthetic files are read. Store/control and browser transport
are fake: these tests do not prove Server quota admission or real Host attach.
"""
from __future__ import annotations

import copy
import json
from pathlib import Path
import sys
import tempfile
import time
import types
import unittest
from unittest.mock import Mock, patch
import uuid

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "chat-handoff"))

from automatic_dispatch import AutomaticDispatch
from automatic_dispatch_config import load_context_factory
from chat_agents import Service, write_json
from test_host_enrollment_integration import FakeQueue, GRANT, PendingThread


HOST = "wc_host_" + "1" * 32


class Queue(FakeQueue):
    """Reuse E2's synthetic queue receipt; add only E3 discovery metadata."""
    origin = "http://127.0.0.1:1"

    def __init__(self, kind, selected, project="project"):
        super().__init__(kind)
        self.project = project
        self.selected = selected
        self.available = True
        self.next_cursor = None
        self.calls, self.pages = [], []
        def identity(name, prefix):
            return prefix + uuid.uuid5(uuid.NAMESPACE_URL, project + "/" + name).hex
        self.root_id = identity("root", "wc_task_")
        self.source_id = identity("source", "wc_task_")
        self.queue_id = identity(kind, "queue_")
        self.manager_id = identity("manager", "wc_iagent_")
        self.manager_run = identity("manager-run", "wc_agent_run_")
        self.binding.update(queue_id=self.queue_id, project=project, root_task_id=self.root_id,
            manager_agent_id=self.manager_id, agent_id=identity(kind + "-agent", "wc_iagent_"),
            enrollment_id=identity(kind + "-enrollment", "wc_enrollment_"),
            task_id=identity("recovery", "wc_task_") if kind == "fallback" else self.source_id)
        self.record_value.update({self.identity: self.queue_id, "project": project,
            "root_task_id": self.root_id, "source_task_id": self.source_id,
            "source_attempt_id": identity("cli", "wc_team_cli_attempt_")})
        self.profile = {"profile_id": project + ":" + kind + "-selected", "model_label": "合成模型",
                        "thinking_label": "经理明确选择", "metadata": {"effort": "fixture"}}
        self.binding["thinking_profile_id"] = self.profile["profile_id"]
        self.record_value["thinking_profile_id"] = self.profile["profile_id"]

    def discover(self):
        self.pages.append(self._candidate_cursor.request())
        self._candidate_cursor.stage({"next_cursor": self.next_cursor})
        return ([{self.identity: self.queue_id, "state": self.status()["queue_state"]}]
                if self.selected else [])

    def finish_discovery(self):
        self._candidate_cursor.finish()

    def read(self, queue_id):
        assert queue_id == self.queue_id
        return copy.deepcopy(self.record_value)

    def status(self):
        status = super().status()
        status["source_task_id"] = self.source_id
        status["instruction"] = "Complete authorized work for " + self.project
        status["enrollment_status"]["run_id"] = "wc_agent_run_" + uuid.uuid5(
            uuid.NAMESPACE_URL, self.project + "/" + self.kind + "/run").hex
        return status

    def record(self, **fields):
        assert fields["dispatch_attempt_id"] == self.fence
        assert fields[self.identity] == self.queue_id
        self.request_id = fields["request_id"]
        self.records.append(fields)
        return {"recorded": True}

    def post(self, payload):
        self.calls.append(copy.deepcopy(payload))
        assert (payload["owner_user"], payload["project"]) == (self.owner_user, self.project)
        if payload["action"] == "agent_host_manager_binding_read":
            assert (payload["root_task_id"], payload["host_id"]) == (self.root_id, HOST)
            return {"binding": None if not self.available else {
                "root_task_id": self.root_id, "host_id": HOST, "owner_user": self.owner_user,
                "project": self.project, "manager_agent_id": self.manager_id,
                "manager_run_id": self.manager_run, "generation": 3,
                "enrollment_id": "wc_enrollment_" + "4" * 32,
                "native_host_generation": 9, "run_expires_at_ms": int(time.time() * 1000) + 300000,
                "window_binding_ref": "wc_host_binding_" + "5" * 64}}
        assert payload["queue_kind"] == self.kind and payload["queue_id"] == self.queue_id
        assert payload["manager_agent_id"] == self.manager_id
        assert payload["manager_run_id"] == self.manager_run and payload["expected_generation"] == 3
        if payload["action"] == "agent_host_dispatch_claim":
            assert self.claims == 0, "A second grant must never be requested"
            self.claims += 1
            self.fence = str(uuid.uuid4())
            return {"status": self.status(), "dispatch_permitted": True,
                    "claim_grant": GRANT, "expires_at_unix_ms": int(time.time() * 1000) + 300000}
        assert payload["action"] in {"agent_host_dispatch_prepare", "agent_host_dispatch_read"}
        return self.status()


class DeferredThread(PendingThread):
    def join(self, timeout):
        # Do not execute a background browser worker from a shutdown call.
        pass


class AutomaticIntegrationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="webcodex-e3-integration-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        network = patch("urllib.request.OpenerDirector.open",
                        side_effect=AssertionError("offline test attempted network"))
        network.start()
        self.addCleanup(network.stop)

    def fixture(self, kind, projects=("project",)):
        root = self.root / kind
        root.mkdir()
        runtime, scheduler = root / "provider", root / "scheduler"
        state, carrier, token = root / "desktop.json", root / "control.json", root / "token.txt"
        token.write_text("synthetic-control-token", encoding="utf-8")
        write_json(state, {"runtime": {"native_host_id": HOST, "server_url": Queue.origin}})
        carrier_value = {"schema_version": 2, "server_url": Queue.origin,
            "bootstrap_token_file": str(token), "source_ref": "desktop-local-bootstrap:" + str(root / "source.env"),
            "owner_user": "owner", "projects": list(projects)}
        write_json(carrier, carrier_value)
        queues = {project: {queue_kind: Queue(queue_kind, kind == queue_kind, project)
                           for queue_kind in ("fallback", "member")} for project in projects}
        fallback, member = queues[projects[0]]["fallback"], queues[projects[0]]["member"]
        threads = []

        def thread_factory(**kwargs):
            thread = DeferredThread(**kwargs)
            threads.append(thread)
            return thread

        def api_factory(queue_kind):
            def create(origin, supplied_token, *, timeout, environ):
                self.assertEqual((origin, supplied_token, timeout),
                                 (Queue.origin, "synthetic-control-token", 5))
                return queues[environ["WEBCODEX_HANDOFF_PROJECT"]][queue_kind]
            return create

        service = Service(runtime, thread_factory=thread_factory, transport_runtime_root=scheduler)
        write_json(runtime / "thinking-profiles.json", {"state": "ready", "observed_at": time.time(),
            "profiles": [{"profile_id": "unselected-default", "model_label": "Do not select"},
                         *[api.profile for scope in queues.values() for api in scope.values()]]})
        factory = load_context_factory(runtime, desktop_state=state, control_config=carrier,
            scheduler_runtime=scheduler, fallback_api_factory=api_factory("fallback"),
            member_api_factory=api_factory("member"))
        pump = AutomaticDispatch(service, factory, state_reader=lambda _: {"mode": "running", "generation": 7})
        return types.SimpleNamespace(root=root, state=state, runtime=runtime, scheduler=scheduler,
            carrier=carrier, carrier_value=carrier_value, queues=queues,
            fallback=fallback, member=member, queue=fallback if kind == "fallback" else member,
            threads=threads, thread_factory=thread_factory, service=service, factory=factory, pump=pump)

    def tick(self, fixture):
        context = fixture.pump._context()
        self.assertIsNotNone(context)
        fixture.pump._tick(context)
        fixture.queue_runtime = Path(context["queue_runtime"])
        return context["fallback_api"].project

    def test_selected_profile_and_unknown_survive_automatic_ticks_and_service_restart(self):
        for kind in ("fallback", "member"):
            with self.subTest(kind=kind):
                f = self.fixture(kind)
                self.tick(f)
                self.assertEqual((len(f.threads), f.queue.claims), (1, 0))
                prepared = json.loads(next((f.runtime / "tasks").glob("*/request.json")).read_text(encoding="utf-8"))
                self.assertEqual(prepared["config"]["thinking_profile"], f.queue.profile)
                self.assertIn(f.queue.status()["instruction"], prepared["config"]["prompt"])
                self.assertEqual(prepared["enrollment_binding"]["queue_kind"], kind)
                if kind == "fallback":
                    self.assertNotIn(prepared["enrollment_binding"]["task_id"], (f.queue.root_id, f.queue.source_id))
                    self.assertEqual(f.member.claims, 0)

                def send(config, evidence_root, *, enrollment_delivery_provider, runtime_root, **_):
                    self.assertEqual(config["thinking_profile"], f.queue.profile)
                    self.assertEqual((evidence_root, runtime_root), (f.runtime / "evidence", f.scheduler))
                    enrollment_delivery_provider()
                    self.assertEqual(f.queue.records[0]["state"], "unknown")
                    return {"status": "sent", "response_observed": True, "response_text": "Unverified candidate."}

                transport = types.SimpleNamespace(send=Mock(side_effect=send))
                with patch.dict(sys.modules, {"chat_transport": transport}):
                    f.threads[0].run()
                    identity = (f.queue.request_id, f.queue.fence)
                    self.tick(f)
                    write_json(f.runtime / "thinking-profiles.json", {"state": "ready",
                        "observed_at": time.time(), "profiles": [dict(f.queue.profile, thinking_label="Changed catalog")]})
                    f.service = Service(f.runtime, thread_factory=f.thread_factory, transport_runtime_root=f.scheduler)
                    f.pump = AutomaticDispatch(f.service, f.factory,
                        state_reader=lambda _: {"mode": "running", "generation": 7})
                    self.tick(f)
                transport.send.assert_called_once()
                self.assertEqual((len(f.threads), f.queue.claims), (1, 1))
                self.assertEqual((f.queue.request_id, f.queue.fence), identity)
                self.assertEqual(f.queue.status()["queue_state"], "unknown")
                self.assertTrue(all(row["state"] == "unknown" for row in f.queue.records))
                observed = f.service.read_chat_task(f.queue.request_id)
                self.assertFalse(observed["business_verified"])
                self.assertEqual(observed["requested_thinking_profile"], f.queue.profile)
                self.assertEqual(len(list((f.runtime / "tasks").iterdir())), 1)
                for path in (f.queue_runtime / "cli-fallback-discovery.json", f.queue_runtime / "team-agent-discovery.json"):
                    self.assertEqual(json.loads(path.read_text(encoding="utf-8")), {"cursor": None})
                for path in f.runtime.rglob("*.json"):
                    self.assertNotIn(GRANT, path.read_text(encoding="utf-8"))

    def test_cached_manager_is_not_authority_after_context_loss_or_null_lookup(self):
        for loss in ("host_context", "manager_binding"):
            with self.subTest(loss=loss):
                f = self.fixture(loss)
                # The selected queue is fallback; the subdirectory name is only fixture isolation.
                f.queue = f.fallback
                f.fallback.selected, f.member.selected = True, False
                self.tick(f)
                self.assertTrue((f.queue_runtime / "manager-bindings.json").exists())
                if loss == "host_context":
                    write_json(f.state, {"runtime": {"native_host_id": None, "server_url": Queue.origin}})
                else:
                    f.fallback.available = False
                input_sent = []

                def send(config, evidence_root, *, enrollment_delivery_provider, **_):
                    enrollment_delivery_provider()
                    input_sent.append(config["launch_id"])
                    return {"status": "sent"}

                with patch.dict(sys.modules, {"chat_transport": types.SimpleNamespace(send=send)}):
                    f.threads[0].run()
                self.assertEqual(input_sent, [])
                self.assertEqual((f.fallback.claims, f.fallback.records), (0, []))
                self.assertEqual(f.service.read_chat_task(f.threads[0].args[0])["state"], "unknown")
                self.assertTrue((f.queue_runtime / "manager-bindings.json").exists())

    def test_service_stop_cancels_pending_input_without_new_claim_or_second_sender(self):
        f = self.fixture("fallback")
        f.member.selected = True
        self.tick(f)
        # The queued member is deferred by the same Service's browser worker.
        self.assertEqual((len(f.threads), f.fallback.claims, f.member.claims), (1, 0, 0))
        calls = (len(f.fallback.calls), len(f.member.calls))
        self.assertTrue(f.pump.close())
        self.assertTrue(f.service._stopping.is_set())
        self.assertFalse(f.service.close(timeout=0))
        transport = types.SimpleNamespace(send=Mock(side_effect=AssertionError("input after stop")))
        with patch.dict(sys.modules, {"chat_transport": transport}):
            f.threads[0].run()
        f.pump._tick({})  # Stopped Service returns before touching any control context.
        transport.send.assert_not_called()
        self.assertEqual((len(f.fallback.calls), len(f.member.calls)), calls)
        self.assertEqual((f.fallback.claims, f.member.claims), (0, 0))
        self.assertEqual(len(f.threads), 1)

    def test_two_registered_projects_keep_scope_when_ui_selection_and_membership_change(self):
        projects = ("agent:runner:project-a", "agent:runner:project-b")
        f = self.fixture("fallback", projects)
        contexts = sorted(f.factory(), key=lambda context: str(Path(context["queue_runtime"]).resolve()))
        first, second = (context["fallback_api"].project for context in contexts)
        paths = {context["fallback_api"].project: Path(context["queue_runtime"]) for context in contexts}
        unavailable, active = f.queues[first]["fallback"], f.queues[second]["fallback"]
        self.assertNotEqual(unavailable.root_id, active.root_id)
        self.assertNotEqual(unavailable.source_id, active.source_id)
        self.assertNotEqual(unavailable.manager_id, active.manager_id)
        self.assertNotEqual(paths[first], paths[second])
        unavailable.available = False

        # An unavailable Manager cannot borrow the other project's authority or
        # prevent the next scope from reaching the existing shared Service.
        self.assertEqual(self.tick(f), first)
        self.assertEqual(f.threads, [])
        self.assertEqual(self.tick(f), second)
        self.assertEqual(len(f.threads), 1)
        # Synthetic UI metadata is not the registered carrier. The real UI
        # selected_project_id/currentProject producer is covered on C's side.
        write_json(f.state, {"runtime": {"native_host_id": HOST, "server_url": Queue.origin},
                             "selected_project_id": first})
        sent = []

        def send(config, evidence_root, *, enrollment_delivery_provider, runtime_root, **_):
            self.assertEqual(runtime_root, f.scheduler)
            enrollment_delivery_provider()
            sent.append(config["launch_id"])
            return {"status": "sent", "response_observed": True, "response_text": "Candidate only."}

        transport = types.SimpleNamespace(send=Mock(side_effect=send))
        with patch.dict(sys.modules, {"chat_transport": transport}):
            f.threads[0].run()
            bound = (active.request_id, active.fence, active.binding["task_id"])
            self.assertEqual(active.claims, 1)
            self.assertEqual(unavailable.claims, 0)
            self.assertEqual(self.tick(f), first)
            unavailable.available = True
            self.assertEqual(self.tick(f), second)
            self.assertEqual(self.tick(f), first)
            self.assertEqual(len(f.threads), 2)

            # Remove a scope after prepare. The retained callback must not
            # claim using its cached binding, while the remaining scope runs.
            write_json(f.carrier, dict(f.carrier_value, projects=[second]))
            f.threads[1].run()
            self.assertEqual(unavailable.claims, 0)
            self.assertEqual(sent, [active.request_id])
            active.next_cursor = "second-scope-resume-page"
            self.assertEqual(self.tick(f), second)
            cursor_path = paths[second] / "cli-fallback-discovery.json"
            saved_cursor = cursor_path.read_bytes()
            self.assertEqual(json.loads(saved_cursor), {"cursor": active.next_cursor})

            # Unknown work stays in the removed scope's namespace. Re-adding
            # it resumes the saved page and exact request, never a new send.
            write_json(f.carrier, dict(f.carrier_value, projects=[first]))
            self.assertEqual(self.tick(f), first)
            self.assertEqual(cursor_path.read_bytes(), saved_cursor)
            write_json(f.state, {"runtime": {"native_host_id": HOST, "server_url": Queue.origin},
                                 "selected_project_id": second})
            write_json(f.carrier, f.carrier_value)
            returned = {context["fallback_api"].project: Path(context["queue_runtime"])
                        for context in f.factory()}
            self.assertEqual(returned, paths)
            self.assertEqual({self.tick(f), self.tick(f)}, set(projects))
            self.assertEqual(active.pages[-1], {"cursor": active.next_cursor})

        self.assertEqual((active.request_id, active.fence, active.binding["task_id"]), bound)
        self.assertEqual((active.claims, unavailable.claims, len(f.threads)), (1, 0, 2))
        self.assertEqual(sent, [active.request_id])
        self.assertEqual(active.status()["queue_state"], "unknown")
        self.assertFalse(f.service.read_chat_task(active.request_id)["business_verified"])
        for project, directory in paths.items():
            cache = json.loads((directory / "manager-bindings.json").read_text(encoding="utf-8"))
            queue = f.queues[project]["fallback"]
            self.assertEqual(cache["project"], project)
            self.assertEqual([row["root_task_id"] for row in cache["roots"]], [queue.root_id])
            self.assertEqual(cache["roots"][0]["manager_agent_id"], queue.manager_id)


if __name__ == "__main__":
    unittest.main()
