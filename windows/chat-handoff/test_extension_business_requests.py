"""Pure fake transport checks: no SDK, process, browser, network or real config."""
import asyncio
from concurrent.futures import Future
from contextlib import asynccontextmanager
import gc
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import weakref

import playwright_extension_browser as ext


class ImmediateTimeout(Future):
    def result(self, timeout=None):
        return super().result(timeout=0)


class FakeThread:
    def __init__(self, alive=True, ident=123456):
        self.alive, self.ident = alive, ident
        self.joins = []

    def is_alive(self):
        return self.alive

    def join(self, timeout):
        self.joins.append(timeout)


class BusinessRequestsTests(unittest.TestCase):
    def setUp(self):
        registry = patch.object(ext, "_TRANSPORTS", weakref.WeakSet())
        registry.start()
        self.addCleanup(registry.stop)
        forbidden = patch.object(ext, "_configuration", side_effect=AssertionError("no configuration"))
        forbidden.start()
        self.addCleanup(forbidden.stop)

    def transport(self):
        transport = ext._Transport({})
        transport._thread = FakeThread()
        transport._loop = SimpleNamespace(is_running=lambda: False,
            call_soon_threadsafe=lambda callback, *args: callback(*args))
        transport._queue = SimpleNamespace(put_nowait=Mock())
        return transport

    def serve(self, transport, item, *, is_error=False, failure=None):
        """Process one admitted request through actual _serve, on this test thread."""
        @asynccontextmanager
        async def client(_configuration):
            class Client:
                async def call_tool(self, name, arguments):
                    if failure is not None:
                        raise failure
                    return SimpleNamespace(isError=is_error)

                async def send_ping(self):
                    return SimpleNamespace()

            transport._queue.put_nowait(item)
            transport._queue.put_nowait(None)
            yield Client()

        with patch.object(ext, "_client", client):
            asyncio.run(transport._serve())

    def timeout_request(self, transport, *, name="browser_tabs"):
        items = []
        transport._queue.put_nowait = items.append
        with patch.object(ext, "Future", ImmediateTimeout):
            with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_call_outcome_unknown"):
                transport._request(name, {})
        return items[0]

    def test_idle_and_protocol_ping_never_count_as_business(self):
        transport = self.transport()
        self.assertTrue(ext.business_requests_settled())
        transport._queue.put_nowait = lambda item: self.serve(transport, item)
        transport.ping()
        self.assertEqual(transport._pending_business, 0)
        self.assertTrue(ext.business_requests_settled())
        self.assertEqual(transport._thread.joins, [])

    def test_timed_out_ping_does_not_block_local_business_summary(self):
        transport = self.transport()
        self.timeout_request(transport, name="__protocol_ping__")
        self.assertTrue(transport._thread.is_alive())
        self.assertEqual(transport._pending_business, 0)
        self.assertTrue(ext.business_requests_settled())

    def test_business_is_counted_before_enqueue_and_settled_by_response(self):
        transport = self.transport()

        def enqueue(item):
            self.assertEqual(transport._pending_business, 1)
            self.assertFalse(ext.business_requests_settled())
            self.serve(transport, item)

        transport._queue.put_nowait = enqueue
        transport.call("browser_tabs", {})
        self.assertEqual(transport._pending_business, 0)
        self.assertTrue(ext.business_requests_settled())

    def test_tool_error_response_is_known_and_settles_business(self):
        transport = self.transport()
        transport._queue.put_nowait = lambda item: self.serve(transport, item, is_error=True)
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_tool_failed"):
            transport.call("browser_tabs", {})
        self.assertEqual(transport._pending_business, 0)
        self.assertTrue(ext.business_requests_settled())

    def test_closed_transport_rejects_before_counting(self):
        transport = self.transport()
        transport._closed = True
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_session_closed"):
            transport.call("browser_tabs", {})
        transport._queue.put_nowait.assert_not_called()
        self.assertEqual(transport._pending_business, 0)
        self.assertTrue(ext.business_requests_settled())

    def test_enqueue_failure_remains_unsettled_until_actual_thread_exit(self):
        transport = self.transport()
        transport._loop.call_soon_threadsafe = Mock(side_effect=RuntimeError("synthetic enqueue failure"))
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_call_outcome_unknown"):
            transport.call("browser_tabs", {})
        self.assertTrue(transport._closed)
        self.assertEqual(transport._pending_business, 1)
        self.assertFalse(ext.business_requests_settled())
        transport._thread.alive = False
        self.assertTrue(ext.business_requests_settled())

    def test_timeout_does_not_mean_transport_worker_exited(self):
        transport = self.transport()
        self.timeout_request(transport)
        self.assertEqual(transport._pending_business, 1)
        self.assertEqual(transport._thread.joins, [10])
        self.assertFalse(ext.business_requests_settled())
        transport._thread.alive = False
        self.assertTrue(ext.business_requests_settled())

    def test_future_exception_and_cancelled_serve_keep_pending_while_alive(self):
        for failure in (RuntimeError("synthetic transport failure"), asyncio.CancelledError()):
            with self.subTest(failure=type(failure).__name__):
                transport = self.transport()
                transport._queue.put_nowait = lambda item: self.serve(transport, item, failure=failure)
                with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_call_outcome_unknown"):
                    transport.call("browser_tabs", {})
                self.assertEqual(transport._pending_business, 1)
                self.assertFalse(ext.business_requests_settled())
                transport._thread.alive = False
                self.assertTrue(ext.business_requests_settled())

    def test_late_explicit_response_settles_original_timed_out_request(self):
        transport = self.transport()
        item = self.timeout_request(transport)
        self.assertFalse(ext.business_requests_settled())
        self.serve(transport, item)
        self.assertTrue(transport._thread.is_alive())
        self.assertEqual(transport._pending_business, 0)
        self.assertTrue(ext.business_requests_settled())

    def test_discarded_transport_remains_visible_while_worker_owns_it(self):
        transport = self.transport()
        self.timeout_request(transport)
        pool = ext.ConnectionPool()
        pool._transport = transport
        worker_target = transport._main  # A live threading.Thread retains its bound target.
        reference = weakref.ref(transport)
        pool._discard(transport)
        self.assertIsNone(pool._transport)
        del transport
        gc.collect()
        self.assertIsNotNone(reference())
        self.assertFalse(ext.business_requests_settled())
        worker_target.__self__._thread.alive = False
        self.assertTrue(ext.business_requests_settled())
        del worker_target
        gc.collect()
        self.assertIsNone(reference())

    def test_summary_never_constructs_connects_or_closes(self):
        transport = self.transport()
        self.timeout_request(transport)
        with patch.object(ext, "_Transport", side_effect=AssertionError("no construction")), \
                patch.object(ext, "_client", side_effect=AssertionError("no client")), \
                patch.object(transport, "close", side_effect=AssertionError("no close")):
            self.assertFalse(ext.business_requests_settled())

    def test_capacity_never_evicts_live_unresolved_transports(self):
        transports = [self.transport() for _ in range(8)]
        for transport in transports:
            self.timeout_request(transport)
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_transport_capacity_reached"):
            ext._Transport({})
        self.assertEqual(len(ext._TRANSPORTS), 8)
        self.assertTrue(all(transport in ext._TRANSPORTS for transport in transports))
        self.assertFalse(ext.business_requests_settled())
        transports[0]._thread.alive = False
        replacement = self.transport()
        self.assertIn(replacement, ext._TRANSPORTS)
        self.assertNotIn(transports[0], ext._TRANSPORTS)
        self.assertEqual(len(ext._TRANSPORTS), 8)
        self.assertFalse(ext.business_requests_settled())

    def test_unstarted_transports_reserve_capacity_and_closed_ones_cannot_revive(self):
        transports = [ext._Transport({}) for _ in range(8)]
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_transport_capacity_reached"):
            ext._Transport({})
        transports[0].close()
        replacement = ext._Transport({})
        self.assertIn(replacement, ext._TRANSPORTS)
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_session_closed"):
            transports[0].start()
        self.assertIsNone(transports[0]._thread.ident)
        self.assertTrue(ext.business_requests_settled())


if __name__ == "__main__":
    unittest.main()
