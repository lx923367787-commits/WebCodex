"""Bounded queue coordination inside the existing native provider lifecycle."""
from __future__ import annotations

from pathlib import Path
import sys
import threading
import time

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "chat-handoff"))

from cli_fallback_control_api import CandidatePageCursor
from cli_fallback_worker import advance
from edge_tools import scheduler_state
from handoff_controller import watcher_lock
from host_agent_dispatch import HostQueueControlApi
from run_auto_handoff import bind_connection


CONTEXT_FIELDS = {"fallback_api", "member_api", "manager_bindings", "origin",
                  "connection_identity", "global_connection_identity", "queue_runtime",
                  "scheduler_runtime"}


class AutomaticDispatch:
    """Use the same Service, queue claims, cursors and browser worker as callers.

    The trusted factory is invoked only by the started coordinator. None or a
    failed lookup is retried on the next bounded tick, including before the
    Desktop tunnel is ready. Context objects and exception text are never logged.
    """

    def __init__(self, service, context_factory, *, interval=5,
                 thread_factory=threading.Thread, advance_fn=advance,
                 state_reader=scheduler_state, lock_factory=watcher_lock,
                 connection_binder=bind_connection, clock=time.monotonic):
        if not callable(context_factory) or not 1 <= interval <= 60:
            raise ValueError("invalid_automatic_dispatch_configuration")
        self.service, self.context_factory = service, context_factory
        self.interval, self.thread_factory = interval, thread_factory
        self.advance, self.state_reader = advance_fn, state_reader
        self.lock_factory, self.connection_binder = lock_factory, connection_binder
        self._thread = None
        self._last_scope = ""
        self.state = "pending"
        self.clock = clock
        self._profile_retry_at = float("-inf")

    def start(self):
        if self._thread is None and not self.service._stopping.is_set():
            self._thread = self.thread_factory(target=self._run,
                name="chat-agents-automatic-dispatch", daemon=True)
            self._thread.start()
        return self

    def close(self, timeout=2):
        self.service._stopping.set()
        if self._thread is not None and self._thread.is_alive():
            self._thread.join(timeout=timeout)
        return self._thread is None or not self._thread.is_alive()

    def _context(self, *, select=True):
        if self.service._stopping.is_set():
            return None
        try:
            contexts = self.context_factory()
            if contexts is None or contexts == []:
                self.state = "pending"
                return None
            if not isinstance(contexts, list) or len(contexts) > 128:
                raise ValueError("invalid_automatic_dispatch_context")
            scopes = {}
            connection = None
            for context in contexts:
                if not isinstance(context, dict) or set(context) != CONTEXT_FIELDS:
                    raise ValueError("invalid_automatic_dispatch_context")
                scheduler, queue = Path(context["scheduler_runtime"]), Path(context["queue_runtime"])
                transport = self.service.transport_runtime_root
                if (not scheduler.is_absolute() or transport is None
                        or scheduler.resolve() != Path(transport).resolve()
                        or not queue.is_absolute()
                        or not queue.resolve().is_relative_to(self.service.runtime.resolve())):
                    raise ValueError("automatic_dispatch_runtime_mismatch")
                if any(not isinstance(context[key], str) or not context[key]
                       for key in ("origin", "connection_identity", "global_connection_identity")):
                    raise ValueError("invalid_automatic_dispatch_connection")
                current = (context["origin"], context["global_connection_identity"])
                if connection is not None and current != connection:
                    raise ValueError("automatic_dispatch_connection_mismatch")
                connection = current
                key = str(queue.resolve())
                if key in scopes:
                    raise ValueError("duplicate_automatic_dispatch_scope")
                scopes[key] = context
            ordered = sorted(scopes)
            key = next((key for key in ordered if key > self._last_scope), ordered[0])
            # Advance even if this scope's control API or persisted state fails.
            # One unavailable project must not starve the next registered one.
            if select:
                self._last_scope = key
            return scopes[key]
        except Exception:
            self.state = "unavailable"
            return None

    def _tick(self, context):
        if self.service._stopping.is_set():
            return
        runtime = self.service.runtime
        self.connection_binder(runtime / "automatic-dispatch.sqlite3",
                               context["origin"], context["global_connection_identity"])
        queue_runtime = Path(context["queue_runtime"])
        self.connection_binder(queue_runtime / "cli-fallback-worker.sqlite3",
                               context["origin"], context["connection_identity"])
        fallback, member = context["fallback_api"], context["member_api"]
        fallback._candidate_cursor = CandidatePageCursor(queue_runtime / "cli-fallback-discovery.json")
        member._candidate_cursor = CandidatePageCursor(queue_runtime / "team-agent-discovery.json")
        fallback = HostQueueControlApi(fallback, "fallback", context["manager_bindings"])
        member = HostQueueControlApi(member, "member", context["manager_bindings"])

        def read_state():
            if self.service._stopping.is_set():
                return {"mode": "stopped"}
            state = self.state_reader(Path(context["scheduler_runtime"]))
            return {"mode": "stopped"} if self.service._stopping.is_set() else state

        self.advance(fallback, self.service, read_state, member)
        # Refresh is Host lifecycle work, never a side effect of catalog reads.
        # Successful queue observation first proves the current control route;
        # existing task work keeps priority over the one browser worker slot.
        initial = read_state()
        if (initial.get("mode") == "running" and self.clock() >= self._profile_retry_at
                and read_state() == initial):
            # Failed menu observations must not open a new tab every five seconds.
            self._profile_retry_at = self.clock() + 300
            if not self.service.maintain_chat_profiles():
                self._profile_retry_at = self.clock() + self.interval
        self.state = "running"

    def _run(self):
        stopping = self.service._stopping
        try:
            while not stopping.is_set():
                context = self._context(select=False)
                if context is not None and not stopping.is_set():
                    try:
                        # The same lock also excludes the explicit standalone
                        # worker. Retain ownership while configuration recovers.
                        with self.lock_factory(self.service.runtime / "cli-fallback-worker"):
                            while not stopping.is_set():
                                # Re-read after acquiring the original worker
                                # lock: a standalone worker could have bound its
                                # legacy scope while we were waiting for it.
                                context = self._context()
                                if context is not None:
                                    try:
                                        self._tick(context)
                                    except Exception:
                                        self.state = "unavailable"
                                if stopping.wait(self.interval):
                                    return
                    except Exception:
                        self.state = "unavailable"
                if stopping.wait(self.interval):
                    return
        finally:
            self.state = "stopped"
