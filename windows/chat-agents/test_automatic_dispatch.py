"""Offline lifecycle checks with temporary state and injected control APIs."""
from __future__ import annotations

import contextlib
import io
import json
from pathlib import Path
import tempfile
import threading
import types
import unittest
from unittest.mock import Mock, call, patch

import automatic_dispatch as automatic
import chat_agents as chat
import native_server


class StepStop:
    """Drive bounded ticks without sleeping or starting background work."""

    def __init__(self, stop_after=1):
        self.stopped = False
        self.stop_after = stop_after
        self.waits = []

    def is_set(self):
        return self.stopped

    def set(self):
        self.stopped = True

    def wait(self, timeout):
        self.waits.append(timeout)
        if len(self.waits) >= self.stop_after:
            self.set()
        return self.stopped


class EmptyApi:
    def __init__(self):
        self.discovered = []
        self.finished = 0

    def discover(self):
        self.discovered.append(self._candidate_cursor.request())
        self._candidate_cursor.stage({"next_cursor": "page-" + str(len(self.discovered))})
        return []

    def finish_discovery(self):
        self.finished += 1
        self._candidate_cursor.finish()


class ControlledThread:
    def __init__(self, *, target, name, daemon):
        self.target, self.name, self.daemon = target, name, daemon
        self.alive = False
        self.joins = []

    def start(self):
        self.alive = True

    def is_alive(self):
        return self.alive

    def join(self, timeout):
        self.joins.append(timeout)


class AutomaticDispatchTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="webcodex-auto-dispatch-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.scheduler = self.root / "scheduler"
        self.service = chat.Service(self.root / "provider", transport_runtime_root=self.scheduler)
        self.service._stopping = StepStop()
        self.fallback, self.member = EmptyApi(), EmptyApi()
        self.context = {"fallback_api": self.fallback, "member_api": self.member,
            "manager_bindings": object(), "origin": "https://fixture.invalid",
            "connection_identity": "fixture-owner/project/host",
            "global_connection_identity": "fixture-owner/host", "queue_runtime": self.service.runtime,
            "scheduler_runtime": self.scheduler}
        self.factory = Mock(return_value=[self.context])
        self.lock = Mock(side_effect=lambda path: contextlib.nullcontext())
        self.binder = Mock()
        self.state_reader = Mock(return_value={"mode": "running", "generation": 7})
        maintenance = patch.object(self.service, "maintain_chat_profiles", return_value=False)
        self.profile_maintenance = maintenance.start()
        self.addCleanup(maintenance.stop)

    def dispatcher(self, **kwargs):
        options = {"state_reader": self.state_reader, "lock_factory": self.lock,
                   "connection_binder": self.binder}
        options.update(kwargs)
        return automatic.AutomaticDispatch(self.service, self.factory, **options)

    def test_constructor_is_idle_and_start_never_uses_browser_worker_slot(self):
        browser_thread = object()
        self.service._thread = browser_thread
        threads = Mock(side_effect=ControlledThread)
        pump = self.dispatcher(thread_factory=threads)
        self.factory.assert_not_called()
        self.lock.assert_not_called()
        self.binder.assert_not_called()
        self.assertIs(pump.start(), pump)
        pump.start()
        self.assertEqual(threads.call_count, 1)
        self.assertIs(self.service._thread, browser_thread)
        self.factory.assert_not_called()
        self.assertFalse(pump.close(timeout=0.1))
        self.assertTrue(self.service._stopping.is_set())
        self.assertEqual(pump._thread.joins, [0.1])

    def test_late_context_retries_without_logging_errors(self):
        self.service._stopping = StepStop(3)
        self.factory.side_effect = [None, ValueError("sensitive-fixture-do-not-log"),
                                    [self.context], [self.context]]
        calls = []

        def advance(fallback, service, read_state, member):
            calls.append(service)
            self.assertIs(service, self.service)
            self.assertEqual(read_state(), {"mode": "running", "generation": 7})
            self.assertEqual((fallback.queue_kind, member.queue_kind), ("fallback", "member"))
            self.assertIs(fallback.manager_bindings, self.context["manager_bindings"])

        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.dispatcher(advance_fn=advance)._run()
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(calls, [self.service])
        self.assertEqual(self.factory.call_count, 4)
        self.assertEqual(self.service._stopping.waits, [5, 5, 5])
        self.lock.assert_called_once_with(self.service.runtime / "cli-fallback-worker")
        self.assertEqual(self.binder.call_args_list, [
            call(self.service.runtime / "automatic-dispatch.sqlite3", self.context["origin"],
                 self.context["global_connection_identity"]),
            call(self.service.runtime / "cli-fallback-worker.sqlite3", self.context["origin"],
                 self.context["connection_identity"])])

    def test_existing_two_cursor_files_resume_and_commit_independently(self):
        self.service._stopping = StepStop(2)
        runtime = self.service.runtime
        (runtime / "cli-fallback-discovery.json").write_text(json.dumps({"cursor": "fallback-old"}))
        (runtime / "team-agent-discovery.json").write_text(json.dumps({"cursor": "member-old"}))
        self.dispatcher()._run()
        self.assertEqual(self.fallback.discovered, [{"cursor": "fallback-old"}, {"cursor": "page-1"}])
        self.assertEqual(self.member.discovered, [{"cursor": "member-old"}, {"cursor": "page-1"}])
        self.assertEqual((self.fallback.finished, self.member.finished), (2, 2))
        for name in ("cli-fallback-discovery.json", "team-agent-discovery.json"):
            self.assertEqual(json.loads((runtime / name).read_text()), {"cursor": "page-2"})
        self.assertEqual(self.lock.call_count, 1)

    def test_pause_and_stop_do_not_discover_or_send(self):
        for mode in ("paused", "stopped"):
            with self.subTest(mode=mode):
                self.service._stopping = StepStop()
                self.state_reader.return_value = {"mode": mode, "generation": 7}
                self.dispatcher()._run()
        self.assertEqual(self.fallback.discovered, [])
        self.assertEqual(self.member.discovered, [])
        self.assertEqual(self.fallback.finished, 0)
        self.profile_maintenance.assert_not_called()

    def test_dispatch_never_discovers_or_refreshes_profiles(self):
        self.profile_maintenance.side_effect = AssertionError("no automatic discovery")
        pump = self.dispatcher()
        for _ in range(3):
            pump._tick(self.context)
        self.profile_maintenance.assert_not_called()
        self.assertEqual(self.fallback.finished, 3)

    def test_profile_maintenance_never_runs_after_control_generation_changes(self):
        self.state_reader.side_effect = [
            {"mode": "running", "generation": 7}, {"mode": "paused", "generation": 8}]
        self.dispatcher(advance_fn=Mock())._tick(self.context)
        self.profile_maintenance.assert_not_called()

    def test_shutdown_during_state_read_prevents_dispatch(self):
        def state_reader(path):
            self.assertEqual(path, self.scheduler)
            self.service._stopping.set()
            return {"mode": "running", "generation": 7}

        self.dispatcher(state_reader=state_reader)._run()
        self.assertEqual(self.fallback.discovered, [])
        self.assertEqual(self.member.discovered, [])

    def test_event_queue_is_integrated_in_existing_worker_and_failure_keeps_enrollment_recovery(self):
        self.context["manager_bindings"]=types.SimpleNamespace(host_id_provider=lambda:"fixture-host")
        advance=Mock()
        with patch("team_continuation.schedule",side_effect=ValueError("synthetic unavailable")) as schedule:
            self.dispatcher(advance_fn=advance)._tick(self.context)
        schedule.assert_called_once()
        self.assertEqual(schedule.call_args.args[0],self.service)
        advance.assert_called_once()

    def test_context_loss_retries_while_retaining_existing_lock(self):
        self.service._stopping = StepStop(3)
        self.factory.side_effect = [[self.context], [self.context], None, [self.context]]
        self.dispatcher()._run()
        self.assertEqual(self.factory.call_count, 4)
        self.assertEqual(self.fallback.finished, 2)
        self.assertEqual(self.lock.call_count, 1)

    def test_locked_worker_retries_without_running_a_second_sender(self):
        self.service._stopping = StepStop(2)
        self.lock.side_effect = [ValueError("fixture-locked"), contextlib.nullcontext()]
        self.dispatcher()._run()
        self.assertEqual(self.lock.call_count, 2)
        self.assertEqual(self.fallback.finished, 1)

    def test_invalid_or_mismatched_context_does_not_touch_controls(self):
        for context in ({}, dict(self.context, scheduler_runtime=self.root / "different"),
                        dict(self.context, scheduler_runtime="relative"),
                        dict(self.context, queue_runtime=self.root / "outside-provider")):
            with self.subTest(context_keys=list(context)):
                self.service._stopping = StepStop()
                self.factory.return_value = [context]
                self.dispatcher()._run()
        self.lock.assert_not_called()
        self.binder.assert_not_called()
        self.state_reader.assert_not_called()

    def test_connection_change_cannot_retarget_existing_state(self):
        self.service._stopping = StepStop(2)
        self.factory.side_effect = [[self.context], [self.context],
            [dict(self.context, connection_identity="different-fixture-owner/project/host")]]
        self.dispatcher(connection_binder=automatic.bind_connection)._run()
        self.assertEqual(self.fallback.finished, 1)
        saved = json.loads((self.service.runtime / "cli-fallback-worker.identity.json").read_text())
        self.assertEqual(saved["token_file"], self.context["connection_identity"])

    def scope(self, project):
        return dict(self.context, fallback_api=EmptyApi(), member_api=EmptyApi(),
                    queue_runtime=self.service.runtime / "project-queues" / project,
                    connection_identity="fixture-owner/host/" + project)

    def test_failed_scope_advances_round_robin_and_retains_separate_cursors(self):
        first, second = self.scope("a"), self.scope("b")
        self.factory.return_value = [second, first]  # Producer order is not scheduling priority.
        self.service._stopping = StepStop(4)
        calls = []

        def advance(fallback, service, read_state, member):
            self.assertIs(service, self.service)
            calls.append(fallback.legacy_api)
            if fallback.legacy_api is first["fallback_api"]:
                raise ValueError("unavailable-project-a")
            return automatic.advance(fallback, service, read_state, member)

        self.dispatcher(advance_fn=advance, connection_binder=automatic.bind_connection)._run()
        self.assertEqual(calls, [first["fallback_api"], second["fallback_api"]] * 2)
        self.assertEqual(second["fallback_api"].discovered, [{}, {"cursor": "page-1"}])
        self.assertFalse((first["queue_runtime"] / "cli-fallback-discovery.json").exists())
        self.assertEqual(json.loads((second["queue_runtime"] / "cli-fallback-discovery.json").read_text()),
                         {"cursor": "page-2"})
        self.assertEqual(self.lock.call_count, 1)

    def test_removed_and_added_scopes_do_not_reset_round_robin(self):
        first, second, third = (self.scope(project) for project in ("a", "b", "c"))
        self.factory.return_value = [first, second]
        pump = self.dispatcher()
        self.assertIs(pump._context(), first)
        self.factory.return_value = [third, second]
        self.assertIs(pump._context(), second)
        self.factory.return_value = [first, second, third]
        self.assertIs(pump._context(), third)
        self.assertIs(pump._context(), first)

    def test_changed_global_connection_cannot_hide_behind_a_new_project_directory(self):
        first, second = self.scope("a"), self.scope("b")
        second["global_connection_identity"] = "different-owner/source"
        self.factory.side_effect = [[first], [first], [second]]
        self.service._stopping = StepStop(2)
        self.dispatcher(connection_binder=automatic.bind_connection)._run()
        self.assertEqual(first["fallback_api"].finished, 1)
        self.assertEqual(second["fallback_api"].finished, 0)
        self.assertFalse(second["queue_runtime"].exists())
        saved = json.loads((self.service.runtime / "automatic-dispatch.identity.json").read_text())
        self.assertEqual(saved["token_file"], first["global_connection_identity"])

    def test_duplicate_scope_or_mixed_global_connections_never_enter_worker(self):
        first, second = self.scope("a"), self.scope("b")
        for contexts in ([first, first], [first, dict(second, origin="https://other.invalid")]):
            with self.subTest(contexts=len(contexts)):
                self.service._stopping = StepStop()
                self.factory.return_value = contexts
                self.dispatcher()._run()
        self.lock.assert_not_called()
        self.binder.assert_not_called()

    def test_one_failed_tick_is_bounded_and_can_recover(self):
        self.service._stopping = StepStop(2)
        advance = Mock(side_effect=[ValueError("sensitive-fixture-do-not-log"), {}])
        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.dispatcher(advance_fn=advance)._run()
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(advance.call_count, 2)
        self.assertEqual(self.lock.call_count, 1)

    def test_stopped_service_does_not_start_coordinator(self):
        self.service._stopping.set()
        threads = Mock()
        pump = self.dispatcher(thread_factory=threads).start()
        self.assertTrue(pump.close())
        threads.assert_not_called()
        self.factory.assert_not_called()

    def test_shutdown_before_next_context_read_does_not_read_configuration(self):
        pump = self.dispatcher()
        self.service._stopping.set()
        self.assertIsNone(pump._context())
        self.factory.assert_not_called()
        self.lock.assert_not_called()

    def test_shutdown_inside_context_lookup_cannot_dispatch(self):
        def factory():
            self.service._stopping.set()
            return [self.context]

        self.factory.side_effect = factory
        self.dispatcher()._run()
        self.factory.assert_called_once()
        self.lock.assert_not_called()
        self.binder.assert_not_called()
        self.assertEqual(self.fallback.discovered, [])

    def test_main_native_eof_stops_pump_before_existing_service_close(self):
        events = []
        self.service._stopping = threading.Event()
        close = Mock(side_effect=lambda **kwargs: events.append("service-close") or True)
        self.service.close = close
        factory = Mock(return_value=None)
        pump = Mock()
        pump.start.side_effect = lambda: events.append("pump-start") or pump

        def close_pump(**kwargs):
            self.assertTrue(self.service._stopping.is_set())
            events.append("pump-close")
            return True

        pump.close.side_effect = close_pump
        source, destination = io.BytesIO(), io.BytesIO()

        def serve(service, incoming, outgoing, *, lifecycle):
            self.assertIs(service, self.service)
            self.assertIs(incoming, source)
            self.assertIs(outgoing, destination)
            self.assertFalse(service._stopping.is_set())
            self.assertTrue(callable(lifecycle))
            pump.start.assert_not_called()
            events.append("native-eof")

        with patch.object(chat, "Service", return_value=self.service) as create_service, \
                patch.object(automatic, "AutomaticDispatch", return_value=pump) as create_pump, \
                patch.object(native_server, "serve", side_effect=serve), \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=source)), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=destination)):
            chat.main(["--runtime", str(self.service.runtime)], context_factory=factory,
                      transport_runtime_root=self.scheduler)
        create_service.assert_called_once_with(self.service.runtime, transport_runtime_root=self.scheduler)
        create_pump.assert_called_once_with(self.service, factory)
        self.assertEqual(events, ["native-eof", "pump-close", "service-close"])
        pump.start.assert_not_called()
        factory.assert_not_called()
        self.assertEqual(destination.getvalue(), b"")

    def test_main_always_closes_service_when_coordinator_close_fails(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock()
        pump = Mock()
        pump.start.return_value = pump
        pump.close.side_effect = ValueError("fixture-close-failure")
        with patch.object(chat, "Service", return_value=self.service), \
                patch.object(automatic, "AutomaticDispatch", return_value=pump), \
                patch.object(native_server, "serve"), \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=io.BytesIO())), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=io.BytesIO())):
            chat.main(["--runtime", str(self.service.runtime)], context_factory=lambda: None,
                      transport_runtime_root=self.scheduler)
        self.service.close.assert_called_once()
        self.assertTrue(self.service._stopping.is_set())
        pump.start.assert_not_called()

    def test_main_passes_explicit_paths_to_lazy_context_loader(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock()
        factory = Mock(return_value=None)
        loader = Mock(return_value=factory)
        module = types.SimpleNamespace(load_context_factory=loader)
        pump = Mock()
        pump.start.return_value = pump
        state_path = self.root / "desktop-state-fixture.json"
        control_path = self.root / "private-control-fixture.json"
        with patch.object(chat, "Service", return_value=self.service), \
                patch.object(automatic, "AutomaticDispatch", return_value=pump) as create_pump, \
                patch.dict(chat.sys.modules, {"automatic_dispatch_config": module}), \
                patch.object(native_server, "serve"), \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=io.BytesIO())), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=io.BytesIO())):
            chat.main(["--runtime", str(self.service.runtime), "--desktop-state", str(state_path),
                       "--control-config", str(control_path)],
                      transport_runtime_root=self.scheduler)
        loader.assert_called_once_with(self.service.runtime, desktop_state=state_path,
                                       control_config=control_path)
        create_pump.assert_called_once_with(self.service, factory)
        pump.start.assert_not_called()
        factory.assert_not_called()
        self.assertFalse(state_path.exists())
        self.assertFalse(control_path.exists())

    def test_main_without_carrier_has_no_lifecycle_or_configuration_reader(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock()
        factory = Mock(return_value=None)
        loader = Mock(return_value=factory)
        module = types.SimpleNamespace(load_context_factory=loader)
        pump = Mock()
        state_path = self.root / "desktop-state-fixture.json"
        with patch.object(chat, "Service", return_value=self.service), \
                patch.object(automatic, "AutomaticDispatch", return_value=pump) as create_pump, \
                patch.dict(chat.sys.modules, {"automatic_dispatch_config": module}), \
                patch.object(native_server, "serve") as serve, \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=io.BytesIO())), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=io.BytesIO())):
            chat.main(["--runtime", str(self.service.runtime), "--desktop-state", str(state_path)],
                      transport_runtime_root=self.scheduler)
        loader.assert_not_called()
        create_pump.assert_not_called()
        self.assertIsNone(serve.call_args.kwargs["lifecycle"])
        factory.assert_not_called()
        self.assertFalse(state_path.exists())

    def test_main_closes_coordinator_even_if_start_partially_succeeds_then_raises(self):
        events = []
        self.service._stopping = threading.Event()
        self.service.close = Mock(side_effect=lambda **kwargs: events.append("service-close") or True)
        pump = Mock()
        pump.start.side_effect = RuntimeError("fixture-start-failure")

        def close_pump(**kwargs):
            self.assertTrue(self.service._stopping.is_set())
            events.append("pump-close")
            return True

        pump.close.side_effect = close_pump
        def serve(service, source, destination, *, lifecycle):
            self.assertEqual(lifecycle("activate", "a" * 32), "unknown")
            self.assertNotEqual(lifecycle("activate", "a" * 32), "active")

        with patch.object(chat, "Service", return_value=self.service), \
                patch.object(automatic, "AutomaticDispatch", return_value=pump), \
                patch.object(native_server, "serve", side_effect=serve), \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=io.BytesIO())), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=io.BytesIO())):
            chat.main(["--runtime", str(self.service.runtime)], context_factory=lambda: None,
                      transport_runtime_root=self.scheduler)
        pump.start.assert_called_once()
        self.assertEqual(events, ["pump-close", "service-close", "pump-close", "service-close"])

    def test_main_native_io_failure_still_stops_both_lifecycles(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock()
        pump = Mock()
        with patch.object(chat, "Service", return_value=self.service), \
                patch.object(automatic, "AutomaticDispatch", return_value=pump), \
                patch.object(native_server, "serve", side_effect=OSError("fixture-io-failure")), \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=io.BytesIO())), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=io.BytesIO())):
            with self.assertRaisesRegex(OSError, "fixture-io-failure"):
                chat.main(["--runtime", str(self.service.runtime)], context_factory=lambda: None,
                          transport_runtime_root=self.scheduler)
        self.assertTrue(self.service._stopping.is_set())
        pump.close.assert_called_once()
        self.service.close.assert_called_once()

    def test_lifecycle_binds_exact_instance_and_never_reactivates_after_stop(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock(return_value=True)
        pump = Mock()
        pump.close.return_value = True
        gate = chat.RuntimeLifecycle(self.service, pump)
        pump.start.assert_not_called()
        self.assertEqual(gate.handle("activate", "a" * 32), "active")
        self.assertEqual(gate.handle("activate", "a" * 32), "active")
        for action in ("activate", "deactivate"):
            with self.assertRaisesRegex(ValueError, "instance_mismatch"):
                gate.handle(action, "b" * 32)
        self.assertEqual(gate.handle("deactivate", "a" * 32), "stopped")
        self.assertEqual(gate.handle("activate", "a" * 32), "stopped")
        self.assertEqual(gate.handle("deactivate", "a" * 32), "stopped")
        pump.start.assert_called_once()
        pump.close.assert_called_once()
        self.service.close.assert_called_once()

    def test_lifecycle_deactivate_before_activate_permanently_blocks_start(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock(return_value=True)
        pump = Mock()
        pump.close.return_value = True
        gate = chat.RuntimeLifecycle(self.service, pump)
        self.assertEqual(gate.handle("deactivate", "a" * 32), "stopped")
        self.assertEqual(gate.handle("activate", "a" * 32), "stopped")
        with self.assertRaisesRegex(ValueError, "instance_mismatch"):
            gate.handle("activate", "b" * 32)
        pump.start.assert_not_called()

    def test_lifecycle_both_threads_share_one_two_second_deadline(self):
        self.service._stopping = threading.Event()
        now = [100.0]
        pump = Mock()

        def close_pump(*, timeout):
            self.assertTrue(self.service._stopping.is_set())
            self.assertEqual(timeout, 2)
            now[0] += 1.5
            return True

        def close_service(*, timeout, close_connection):
            self.assertEqual(timeout, 0.5)
            self.assertFalse(close_connection)
            now[0] += 0.5
            return True

        pump.close.side_effect = close_pump
        self.service.close = Mock(side_effect=close_service)
        gate = chat.RuntimeLifecycle(self.service, pump, clock=lambda: now[0])
        self.assertEqual(gate.handle("deactivate", "a" * 32), "stopped")
        self.assertEqual(now[0], 102)
        self.assertEqual(gate.close(), "stopped")
        pump.close.assert_called_once()
        self.service.close.assert_called_once()

    def test_lifecycle_unknown_cleanup_never_restarts_its_deadline(self):
        self.service._stopping = threading.Event()
        now = [100.0]
        pump = Mock()
        pump.close.return_value = False
        self.service.close = Mock(return_value=True)
        gate = chat.RuntimeLifecycle(self.service, pump, clock=lambda: now[0])
        self.assertEqual(gate.handle("deactivate", "a" * 32), "unknown")
        now[0] = 110
        self.assertEqual(gate.close(), "unknown")
        self.assertEqual(pump.close.call_args, call(timeout=0))
        self.assertEqual(self.service.close.call_args, call(timeout=0, close_connection=False))
        pump.start.assert_not_called()

    def test_lifecycle_requires_both_actual_threads_to_finish(self):
        self.service._stopping = threading.Event()
        pump = self.dispatcher(thread_factory=ControlledThread)
        browser = ControlledThread(target=lambda: None, name="fake-browser", daemon=True)
        browser.start()
        self.service._thread = browser
        gate = chat.RuntimeLifecycle(self.service, pump)
        self.assertEqual(gate.handle("activate", "a" * 32), "active")
        browser_module = types.SimpleNamespace(business_requests_settled=Mock(return_value=True),
            close_persistent_connection=Mock(
                side_effect=AssertionError("pool cleanup is outside the lifecycle handshake")))
        with patch.dict(chat.sys.modules, {"playwright_extension_browser": browser_module}):
            self.assertEqual(gate.handle("deactivate", "a" * 32), "unknown")
            browser_module.business_requests_settled.assert_not_called()
            pump._thread.alive = False
            self.assertEqual(gate.close(), "unknown")
            browser_module.business_requests_settled.assert_not_called()
            browser.alive = False
            self.assertEqual(gate.close(), "stopped")
        browser_module.close_persistent_connection.assert_not_called()
        browser_module.business_requests_settled.assert_called_once()
        self.factory.assert_not_called()

    def test_lifecycle_pending_business_requests_prevent_stopped_and_do_not_extend_deadline(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock(return_value=True)
        pump = Mock()
        pump.close.return_value = True
        now = [100.0]
        browser = types.ModuleType("playwright_extension_browser")
        browser.business_requests_settled = Mock(side_effect=[False, True])
        browser.close_persistent_connection = Mock(
            side_effect=AssertionError("lifecycle must not close the pool"))
        gate = chat.RuntimeLifecycle(self.service, pump, clock=lambda: now[0])
        with patch.dict(chat.sys.modules, {"playwright_extension_browser": browser}):
            self.assertEqual(gate.handle("deactivate", "a" * 32), "unknown")
            self.assertTrue(self.service._stopping.is_set())
            now[0] = 110
            self.assertEqual(gate.close(), "stopped")
            self.assertEqual(gate.close(), "stopped")
        self.assertEqual(pump.close.call_args, call(timeout=0))
        self.assertEqual(self.service.close.call_args, call(timeout=0, close_connection=False))
        self.assertEqual(browser.business_requests_settled.call_count, 2)
        browser.close_persistent_connection.assert_not_called()
        pump.start.assert_not_called()

    def test_lifecycle_missing_failing_or_nonboolean_business_proof_is_unknown(self):
        for proof in (None, Mock(side_effect=ValueError("synthetic-secret-do-not-log")),
                      Mock(return_value=1), Mock(return_value={"settled": True})):
            with self.subTest(proof_present=proof is not None):
                self.service._stopping = threading.Event()
                self.service.close = Mock(return_value=True)
                pump = Mock()
                pump.close.return_value = True
                browser = types.ModuleType("playwright_extension_browser")
                if proof is not None:
                    browser.business_requests_settled = proof
                browser.close_persistent_connection = Mock(
                    side_effect=AssertionError("lifecycle must not close the pool"))
                gate = chat.RuntimeLifecycle(self.service, pump)
                with patch.dict(chat.sys.modules, {"playwright_extension_browser": browser}), \
                        contextlib.redirect_stdout(io.StringIO()) as stdout, \
                        contextlib.redirect_stderr(io.StringIO()) as stderr:
                    self.assertEqual(gate.handle("deactivate", "a" * 32), "unknown")
                self.assertEqual(stdout.getvalue(), "")
                self.assertEqual(stderr.getvalue(), "")
                browser.close_persistent_connection.assert_not_called()

    def test_lifecycle_does_not_accept_a_dynamically_fabricated_business_proof(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock(return_value=True)
        pump = Mock()
        pump.close.return_value = True
        browser = types.ModuleType("playwright_extension_browser")
        browser.__getattr__ = Mock(return_value=lambda: True)
        gate = chat.RuntimeLifecycle(self.service, pump)
        with patch.dict(chat.sys.modules, {"playwright_extension_browser": browser}):
            self.assertEqual(gate.handle("deactivate", "a" * 32), "unknown")
        browser.__getattr__.assert_not_called()

    def test_main_initialize_and_tools_list_never_start_or_read_private_context(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock(return_value=True)
        factory = Mock(side_effect=AssertionError("context must stay unread"))
        loader = Mock(return_value=factory)
        module = types.SimpleNamespace(load_context_factory=loader)
        pump = Mock()
        pump.close.return_value = True
        frames = [{"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": native_server.PROTOCOL_VERSION}},
                  {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}]
        source = io.BytesIO(b"".join(json.dumps(frame).encode() + b"\n" for frame in frames))
        destination = io.BytesIO()
        with patch.object(chat, "Service", return_value=self.service), \
                patch.object(automatic, "AutomaticDispatch", return_value=pump), \
                patch.dict(chat.sys.modules, {"automatic_dispatch_config": module}), \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=source)), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=destination)):
            chat.main(["--runtime", str(self.service.runtime), "--desktop-state", str(self.root / "state"),
                       "--control-config", str(self.root / "carrier")], transport_runtime_root=self.scheduler)
        replies = [json.loads(line) for line in destination.getvalue().splitlines()]
        self.assertEqual(replies[0]["result"]["runtimeLifecycle"], {
            "protocolVersion": native_server.LIFECYCLE_PROTOCOL_VERSION, "providerId": "chat-agents"})
        self.assertEqual(len(replies[1]["result"]["tools"]), 8)
        pump.start.assert_not_called()
        factory.assert_not_called()
        self.assertFalse((self.root / "carrier").exists())

    def test_main_private_activation_is_separate_from_the_eight_tools(self):
        self.service._stopping = threading.Event()
        self.service.close = Mock(return_value=True)
        factory = Mock(return_value=None)
        pump = Mock()
        pump.close.return_value = True
        frames = [{"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": native_server.PROTOCOL_VERSION}}]
        for index, action in enumerate(("activate", "activate", "deactivate", "activate"), 2):
            frames.append({"jsonrpc": "2.0", "id": index, "method": native_server.LIFECYCLE_METHOD,
                "params": {"protocolVersion": native_server.LIFECYCLE_PROTOCOL_VERSION,
                           "providerId": "chat-agents", "providerInstanceId": "a" * 32, "action": action}})
        source = io.BytesIO(b"".join(json.dumps(frame).encode() + b"\n" for frame in frames))
        destination = io.BytesIO()
        with patch.object(chat, "Service", return_value=self.service), \
                patch.object(automatic, "AutomaticDispatch", return_value=pump), \
                patch.object(chat.sys, "stdin", types.SimpleNamespace(buffer=source)), \
                patch.object(chat.sys, "stdout", types.SimpleNamespace(buffer=destination)):
            chat.main(["--runtime", str(self.service.runtime)], context_factory=factory,
                      transport_runtime_root=self.scheduler)
        replies = [json.loads(line) for line in destination.getvalue().splitlines()]
        self.assertEqual([reply["result"]["state"] for reply in replies[1:]],
                         ["active", "active", "stopped", "stopped"])
        pump.start.assert_called_once()
        pump.close.assert_called_once()
        self.service.close.assert_called_once()


if __name__ == "__main__":
    unittest.main()
