"""Actual Service + enrolled queue adapter; only the Store/transport are fake."""
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

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "chat-agents"))
from chat_agents import Service, write_json
from cli_fallback_dispatch import dispatch as fallback_dispatch
from team_agent_dispatch import dispatch as member_dispatch
from host_agent_dispatch import HostQueueControlApi
from host_enrollment import build_enrollment_message
from manager_bindings import RootManagerBindings


GRANT = "wc_en_" + "a" * 64
PROFILE = {"profile_id": "manager-selected", "model_label": "Fake model", "thinking_label": "Fake effort"}


class FakeQueue:
    owner_user, project = "owner", "project"

    def __init__(self, kind):
        self.kind = kind
        self.identity = "fallback_id" if kind == "fallback" else "team_agent_attempt_id"
        self.record_value = {self.identity: "queue-1", "project": "project", "root_task_id": "root-1",
            "source_task_id": "child-1", "source_attempt_id": "cli-1", "thinking_profile_id": PROFILE["profile_id"]}
        self.binding = {"queue_kind": kind, "queue_id": "queue-1", "enrollment_id": "wc_enrollment_" + "b" * 32,
            "agent_id": "wc_iagent_" + "c" * 32, "task_id": "recovery-1" if kind == "fallback" else "child-1",
            "root_task_id": "root-1", "manager_agent_id": "manager-1", "project": "project",
            "thinking_profile_id": PROFILE["profile_id"]}
        self.claims, self.records, self.attached = 0, [], False
        self.request_id, self.fence = None, None

    def read(self, queue_id):
        assert queue_id == "queue-1"
        return copy.deepcopy(self.record_value)

    def status(self):
        now = int(time.time() * 1000)
        return {"binding": copy.deepcopy(self.binding), "source_task_id": "child-1",
            "instruction": "Complete the remaining authorized work.", "cancel_requested": False,
            "queue_state": "unknown" if self.request_id else ("starting" if self.fence else "queued"),
            "dispatch_attempt_id": self.fence, "dispatch_count": self.claims,
            "delivery_state": "delivery_unknown" if self.fence else "reserved",
            "request_id": self.request_id, "response_observed": False, "target_task_state": "active",
            "enrollment_status": {"enrollment_id": self.binding["enrollment_id"],
                "agent_id": self.binding["agent_id"], "task_id": self.binding["task_id"],
                "run_id": "wc_agent_run_" + "d" * 32, "generation": 1,
                "state": "claimed" if self.attached else "issued",
                "run_state": "active" if self.attached else "pending", "window_enabled": self.attached,
                "window_binding_ref": "wc_host_binding_" + "e" * 64 if self.attached else None,
                "principal_revoked": False, "claim_expired": False,
                "claimed_at_unix_ms": now - 1 if self.attached else None,
                "claim_expires_at_unix_ms": now + 300000, "run_expires_at_unix_ms": now + 300000,
                "delivery_state": "delivery_unknown" if self.fence else "reserved"}}

    def post(self, payload):
        assert payload["manager_run_id"] == "manager-run-1"
        assert payload["expected_generation"] == 3
        assert payload["queue_id"] == "queue-1"
        if payload["action"] == "agent_host_dispatch_claim":
            assert self.claims == 0
            self.claims += 1
            self.fence = str(uuid.uuid4())
            return {"status": self.status(), "dispatch_permitted": True,
                    "claim_grant": GRANT, "expires_at_unix_ms": int(time.time() * 1000) + 300000}
        assert payload["action"] in {"agent_host_dispatch_prepare", "agent_host_dispatch_read"}
        return self.status()

    def record(self, **fields):
        assert fields["dispatch_attempt_id"] == self.fence
        assert fields[self.identity] == "queue-1"
        self.request_id = fields["request_id"]
        self.records.append(fields)
        return {"recorded": True}


class PendingThread:
    def __init__(self, *, target, args, **_):
        self.target, self.args, self.alive = target, args, False

    def start(self):
        self.alive = True

    def is_alive(self):
        return self.alive

    def run(self):
        try:
            self.target(*self.args)
        finally:
            self.alive = False


class HostEnrollmentIntegrationTests(unittest.TestCase):
    def managers(self):
        return RootManagerBindings({"owner_user": "owner", "project": "project", "roots": [{
            "root_task_id": "root-1", "host_id": "host-1", "manager_agent_id": "manager-1",
            "manager_run_id": "manager-run-1", "expected_generation": 3}]}, owner_user="owner", project="project")

    def test_both_queues_late_claim_exact_profile_attach_and_restart_without_resend(self):
        for kind, dispatch in (("member", member_dispatch), ("fallback", fallback_dispatch)):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                threads = []
                def factory(**arguments):
                    thread = PendingThread(**arguments)
                    threads.append(thread)
                    return thread
                service = Service(root / "tasks-runtime", thread_factory=factory, transport_runtime_root=root / "transport")
                queue = FakeQueue(kind)
                api = HostQueueControlApi(queue, kind, self.managers())
                refused = dispatch(api, service, "queue-1")
                self.assertEqual(refused["state"], "profile_unavailable")
                self.assertEqual(queue.claims, 0)
                self.assertEqual(threads, [])
                write_json(service.runtime / "thinking-profiles.json", {"state": "ready", "observed_at": time.time(), "profiles": [PROFILE]})
                scheduled = dispatch(api, service, "queue-1")
                self.assertEqual(scheduled["state"], "starting")
                self.assertEqual(queue.claims, 0)
                self.assertEqual(len(threads), 1)
                transport = types.ModuleType("chat_transport")
                def send(config, evidence_root, *, enrollment_delivery_provider, runtime_root, **_):
                    self.assertEqual(runtime_root, root / "transport")
                    self.assertEqual(config["thinking_profile"], PROFILE)
                    delivery = enrollment_delivery_provider()
                    self.assertEqual(queue.records[0]["state"], "unknown")
                    self.assertIn(GRANT, build_enrollment_message(config["prompt"], delivery))
                    queue.attached = True
                    return {"status": "sent", "response_observed": True, "response_text": "Candidate only."}
                transport.send = Mock(side_effect=send)
                with patch.dict(sys.modules, {"chat_transport": transport}):
                    threads[0].run()
                observed = dispatch(api, service, "queue-1")
                self.assertEqual(observed["state"], "dispatched")
                self.assertTrue(observed["enrollment_verified"])
                self.assertFalse(observed["business_verified"])
                self.assertEqual(queue.claims, 1)
                restarted = Service(service.runtime, thread_factory=factory, transport_runtime_root=root / "transport")
                replay = dispatch(api, restarted, "queue-1")
                self.assertEqual(replay["request_id"], observed["request_id"])
                self.assertEqual(len(threads), 1)
                self.assertEqual(queue.claims, 1)
                self.assertNotIn(GRANT, repr(replay))
                for file in root.rglob("*"):
                    if file.is_file():
                        self.assertNotIn(GRANT.encode(), file.read_bytes())

    def test_crash_before_callback_keeps_same_local_request_without_claim_or_resend(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            factory = Mock(side_effect=RuntimeError("synthetic start failure"))
            service = Service(root, thread_factory=factory, transport_runtime_root=root / "transport")
            write_json(root / "thinking-profiles.json", {"state": "ready", "observed_at": time.time(), "profiles": [PROFILE]})
            queue = FakeQueue("fallback")
            api = HostQueueControlApi(queue, "fallback", self.managers())
            first = fallback_dispatch(api, service, "queue-1")
            second = fallback_dispatch(api, Service(root, thread_factory=factory,
                transport_runtime_root=root / "transport"), "queue-1")
            self.assertEqual(first["state"], "unknown")
            self.assertEqual(second["state"], "unknown")
            self.assertEqual(queue.claims, 0)
            self.assertEqual(factory.call_count, 1)
            self.assertEqual(len(list((root / "tasks").iterdir())), 1)

    def test_worker_entry_wraps_both_original_queues_with_exact_manager_bindings(self):
        import cli_fallback_worker as worker
        import run_auto_handoff
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binding_file = root / "manager-bindings.json"
            binding_file.write_text(json.dumps({"owner_user": "owner", "project": "project", "roots": [{
                "root_task_id": "root-1", "host_id": "host-1", "manager_agent_id": "manager-1",
                "manager_run_id": "manager-run-1", "expected_generation": 3}]}), encoding="utf-8")
            queues = [FakeQueue("fallback"), FakeQueue("member")]
            def advance(recovery, service, state_reader, member):
                self.assertIsInstance(recovery, HostQueueControlApi)
                self.assertIsInstance(member, HostQueueControlApi)
                self.assertIs(recovery.legacy_api, queues[0])
                self.assertIs(member.legacy_api, queues[1])
                self.assertEqual(recovery.manager_bindings.for_root("root-1")["expected_generation"], 3)
                self.assertEqual(service.transport_runtime_root, root / "chat-handoff" / "runtime")
                self.assertTrue(recovery.enrollment_required and member.enrollment_required)
                return {"state": "observed", "observed": 0}
            with patch.object(worker, "ROOT", root / "chat-handoff"), \
                    patch.object(worker, "load_credentials", return_value=("https://fixture.invalid", "synthetic", "fixture")), \
                    patch.object(worker, "FallbackControlApi", return_value=queues[0]), \
                    patch.object(worker, "TeamAgentControlApi", return_value=queues[1]), \
                    patch.object(run_auto_handoff, "bind_connection"), \
                    patch.object(worker, "advance", side_effect=advance) as advanced, \
                    patch.object(Service, "close"):
                self.assertEqual(worker.main(["--once", "--manager-bindings", str(binding_file)]), 0)
            advanced.assert_called_once()


if __name__ == "__main__":
    unittest.main()
