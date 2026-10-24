"""Offline connection lifecycle tests. Never read real configuration or Edge."""
import asyncio
from contextlib import asynccontextmanager
import json
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

import playwright_extension_browser as ext


def response(owner, value):
    body = {"bridge": "webcodex.extension.v1", "owner": owner, "value": value}
    return SimpleNamespace(isError=False,
        content=[SimpleNamespace(type="text", text="### Result\n" + json.dumps(body))])


class FakeTransport:
    def __init__(self, _configuration):
        self.connection_id = str(uuid.uuid4())
        self._closed = False
        self.calls = []
        self.pings = self.closes = 0
        self.owner = None
        self.fail_ping = self.fail_start = False
        self.fail_operation = None

    def start(self):
        if self.fail_start:
            raise ext.ExtensionBrowserError("extension_connection_failed")
        return self

    def close(self):
        self.closes += 1
        self._closed = True

    def ping(self):
        self.pings += 1
        if self.fail_ping:
            raise ext.ExtensionBrowserError("extension_call_outcome_unknown")

    def call(self, name, arguments):
        if self._closed:
            raise ext.ExtensionBrowserError("extension_session_closed")
        if name == "browser_tabs":
            self.calls.append((name, arguments))
            return response("unused", None)
        source = arguments["code"].split("const p = ", 1)[1]
        payload = json.JSONDecoder().raw_decode(source)[0]
        operation = payload["op"]
        self.calls.append((operation, payload))
        if operation == self.fail_operation:
            raise ext.ExtensionBrowserError("extension_call_outcome_unknown")
        if operation == "initialize":
            self.owner = payload["owner"]
        if payload["owner"] != self.owner:
            raise ext.ExtensionBrowserError("extension_tool_failed")
        values = {"find": ["h1"], "url": ext.CHAT_URL, "visible": True}
        return response(payload["owner"], values.get(operation, True))


class PoolTests(unittest.TestCase):
    def setUp(self):
        self.transports = []

        def factory(configuration):
            self.assertEqual({}, configuration)
            value = FakeTransport(configuration)
            self.transports.append(value)
            return value

        self.pool = ext.ConnectionPool(factory, lambda: {})
        self.addCleanup(self.pool.close)

    def session(self):
        return ext._PersistentSession(self.pool)

    def test_two_workflows_share_connection_and_keep_distinct_owned_pages(self):
        with self.session() as first:
            first_browser = first.create(ext.CHAT_URL)
            identity1 = first_browser.identity()
        self.assertFalse(self.transports[0]._closed)
        with self.session() as second:
            identity2 = second.create(ext.CHAT_URL).identity()
        self.assertEqual(identity1["connection_id"], identity2["connection_id"])
        self.assertNotEqual(identity1["owned_page_id"], identity2["owned_page_id"])
        self.assertTrue(identity1["persistent"])
        self.assertEqual(1, len(self.transports))
        self.assertEqual(1, self.transports[0].pings)
        self.assertEqual(2, sum(name == "browser_tabs" for name, _ in self.transports[0].calls))
        status = self.pool.status()
        self.assertEqual(1, status["created_count"])
        self.assertEqual(1, status["reuse_count"])
        self.assertFalse(status["borrowed"])

    def test_whole_workflow_lease_is_nonblocking_and_exclusive_across_threads(self):
        seen = []
        with self.session():
            def contender():
                try:
                    with self.session():
                        seen.append("unexpected")
                except ext.ExtensionBrowserError as error:
                    seen.append(str(error))
            thread = threading.Thread(target=contender)
            thread.start()
            thread.join(timeout=1)
            self.assertFalse(thread.is_alive())
            self.assertTrue(self.pool.status()["borrowed"])
        self.assertEqual(["extension_pool_busy"], seen)
        self.assertEqual(1, len(self.transports))

    def test_old_browser_and_raw_lease_fail_after_exit(self):
        with self.session() as session:
            browser = session.create(ext.CHAT_URL)
            lease = session._transport
        count = len(self.transports[0].calls)
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_session_closed"):
            browser.get_url()
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_session_closed"):
            lease.call("browser_tabs", {"action": "new", "url": ext.CHAT_URL})
        self.assertEqual(count, len(self.transports[0].calls))

    def test_closed_connection_is_replaced_only_at_next_lease(self):
        with self.session():
            pass
        previous = self.transports[0]
        previous._closed = True
        with self.session() as session:
            self.assertNotEqual(previous.connection_id, session._transport.connection_id)
        self.assertEqual(2, len(self.transports))
        self.assertEqual(0, previous.pings)
        self.assertTrue(previous._closed)

    def test_ping_failure_discards_then_creates_once_before_any_action(self):
        with self.session():
            pass
        previous = self.transports[0]
        previous.fail_ping = True
        with self.session() as session:
            self.assertEqual([], previous.calls)
            session.create(ext.CHAT_URL)
        self.assertEqual(2, len(self.transports))
        self.assertTrue(previous._closed)
        self.assertEqual(1, previous.pings)
        self.assertEqual(0, self.pool.status()["reuse_count"])

    def test_caught_input_failure_is_sticky_and_never_replayed(self):
        with self.session() as session:
            browser = session.create(ext.CHAT_URL)
            transport = self.transports[0]
            transport.fail_operation = "text"
            with self.assertRaises(ext.ExtensionBrowserError):
                browser.keys("one message", contains_hotkey=False)
            with self.assertRaises(ext.ExtensionBrowserError):
                browser.get_url()
            self.assertFalse(transport._closed)
            self.assertEqual(1, len(self.transports))
        self.assertTrue(transport._closed)
        self.assertEqual(1, sum(name == "text" for name, _ in transport.calls))
        with self.session():
            pass
        self.assertEqual(2, len(self.transports))

    def test_failed_fresh_start_is_not_retried_and_unlocks_pool(self):
        failed = FakeTransport({})
        failed.fail_start = True
        factory_calls = []

        def factory(_):
            factory_calls.append(True)
            return failed

        pool = ext.ConnectionPool(factory, lambda: {})
        self.addCleanup(pool.close)
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_connection_failed"):
            pool.acquire()
        self.assertEqual(1, len(factory_calls))
        self.assertFalse(pool.status()["borrowed"])
        self.assertTrue(failed._closed)

    def test_default_short_session_still_closes_at_exit(self):
        transport = FakeTransport({})
        with patch.object(ext, "_configuration", return_value={}), patch.object(ext, "_Transport", return_value=transport):
            with ext.session() as session:
                identity = session.create(ext.CHAT_URL).identity()
        self.assertFalse(identity["persistent"])
        self.assertEqual(transport.connection_id, identity["connection_id"])
        self.assertTrue(transport._closed)
        self.assertEqual(0, transport.pings)

    def test_status_and_public_helpers_never_read_configuration_or_connect(self):
        with patch.object(ext, "_CONNECTION_POOL", self.pool), patch.object(ext, "_configuration", side_effect=AssertionError("must not read")):
            status = ext.persistent_connection_status()
            self.assertFalse(status["connected"])
            self.assertIsNone(status["connection_id"])
            self.assertEqual([], self.transports)
            with ext.persistent_session():
                pass
            ext.close_persistent_connection()
            self.assertTrue(ext.persistent_connection_status()["closed"])
            self.assertFalse(ext.persistent_connection_status()["connected"])

    def test_close_during_lease_invalidates_actions_and_future_leases(self):
        with self.session() as session:
            browser = session.create(ext.CHAT_URL)
            self.pool.close()
            with self.assertRaises(ext.ExtensionBrowserError):
                browser.get_url()
        self.assertFalse(self.pool.status()["borrowed"])
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_pool_closed"):
            self.pool.acquire()


class ProtocolPingTests(unittest.TestCase):
    def test_real_transport_queue_uses_protocol_ping_not_tool_call(self):
        events = []

        @asynccontextmanager
        async def fake_client(_configuration):
            class Client:
                async def send_ping(self):
                    events.append(("ping", asyncio.current_task()))
                    return SimpleNamespace()

                async def call_tool(self, name, arguments):
                    events.append((name, asyncio.current_task()))
                    return response("owner", True)

            events.append(("enter", asyncio.current_task()))
            try:
                yield Client()
            finally:
                events.append(("exit", asyncio.current_task()))

        with patch.object(ext, "_client", fake_client):
            transport = ext._Transport({}).start()
            try:
                transport.ping()
                transport.call("browser_tabs", {"action": "new", "url": ext.CHAT_URL})
            finally:
                transport.close()
        self.assertEqual(["enter", "ping", "browser_tabs", "exit"], [name for name, _ in events])
        self.assertTrue(all(task is events[0][1] for _, task in events))


if __name__ == "__main__":
    unittest.main()
