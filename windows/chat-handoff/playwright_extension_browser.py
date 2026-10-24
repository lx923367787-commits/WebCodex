"""Handoff façade over the official, pinned Playwright extension MCP.

Only the internal methods below generate code, from fixed templates. This module
does not expose an arbitrary-code tool, copy cookies, or close browser tabs.
"""
from __future__ import annotations

import asyncio
import atexit
from concurrent.futures import Future
from contextlib import asynccontextmanager
from datetime import timedelta
import json
import logging
import math
import os
from pathlib import Path
import re
import shutil
import threading
import time
import uuid
import weakref

ROOT = Path(__file__).resolve().parent


def _discover_node():
    explicit = os.environ.get("WEBCODEX_NODE", "")
    if explicit:
        return Path(explicit)
    bundled = ROOT.parent / "node" / "node.exe"
    if bundled.is_file():
        return bundled
    return Path(shutil.which("node") or
                str(Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "nodejs" / "node.exe"))


NODE = _discover_node()
CHAT_URL = "https://chatgpt.com/"
VERSION = "0.0.83"
RPC_TIMEOUT = 45
_ENV = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "TEMP", "TMP", "USERPROFILE",
        "LOCALAPPDATA", "APPDATA", "PROGRAMDATA", "PROGRAMFILES", "PROGRAMFILES(X86)",
        "PROGRAMW6432", "HOMEDRIVE", "HOMEPATH", "HOME", "USERNAME", "COMPUTERNAME",
        "LANG", "LC_ALL", "PYTHONUTF8", "SYSTEMDRIVE", "PROCESSOR_ARCHITECTURE"}
_FUNCTION = re.compile(r"\s*(?:async\s+)?function(?:\s+[A-Za-z_$][\w$]*)?\s*\(")
_KEYS = {"{RIGHT}": "ArrowRight", "{LEFT}": "ArrowLeft", "{SPACE}": "Space"}
_TRANSPORTS = weakref.WeakSet()
_BUSINESS_LOCK = threading.Lock()
_TRANSPORT_LIMIT = 8


def business_requests_settled():
    """Local RPC summary, used only after both business producer threads exit.

    A live transport retains unresolved business work even if its pool already
    discarded it. Thread exit proves only local quiescence, never remote task
    completion. This read never pings, connects, cancels or closes anything.
    """
    with _BUSINESS_LOCK:
        return all(transport._pending_business == 0 or not transport._thread.is_alive()
                   for transport in list(_TRANSPORTS))


class ExtensionBrowserError(RuntimeError):
    """Controlled codes only; never include raw MCP, page or connection output."""


def _configuration():
    try:
        package = ROOT / "extension-runtime/node_modules/@playwright/mcp"
        with (package / "package.json").open("rb") as stream:
            metadata = stream.read(65537)
        if len(metadata) > 65536 or json.loads(metadata).get("version") != VERSION:
            raise ValueError()
        if not NODE.is_file() or not (package / "cli.js").is_file():
            raise ValueError()
        with (ROOT / "runtime/playwright-extension-token.txt").open("rb") as stream:
            data = stream.read(4097)
        token = data.decode("utf-8-sig").strip()
        if len(data) > 4096 or not re.fullmatch(r"[A-Za-z0-9_-]{32,256}", token):
            raise ValueError()
        env = {key: value for key, value in os.environ.items() if key.upper() in _ENV}
        env["PLAYWRIGHT_MCP_EXTENSION_TOKEN"] = token
        return {"command": str(NODE), "args": [str(package / "cli.js"), "--extension", "--browser", "msedge",
                "--profile-dir-name", "Default", "--codegen", "none", "--image-responses", "omit",
                "--snapshot-mode", "none", "--console-level", "error", "--no-webmcp",
                "--output-dir", str(ROOT / "runtime/extension-mcp")],
                "env": env, "cwd": str(ROOT)}
    except Exception:
        raise ExtensionBrowserError("extension_configuration_unavailable") from None


def discover_endpoint():
    """Read-only compatibility preflight. No browser, process or network action."""
    _configuration()
    return "official_playwright_extension_stdio"


@asynccontextmanager
async def _client(configuration):
    from mcp import ClientSession
    from mcp.client.stdio import StdioServerParameters, stdio_client
    from mcp.types import Implementation

    with open(os.devnull, "w", encoding="utf-8") as errlog:
        async with stdio_client(StdioServerParameters(**configuration), errlog=errlog) as streams:
            async with ClientSession(*streams, read_timeout_seconds=timedelta(seconds=RPC_TIMEOUT),
                    client_info=Implementation(name="webcodex-handoff", version="1")) as client:
                await asyncio.wait_for(client.initialize(), timeout=15)
                yield client


class _QuietWorker(logging.Filter):
    def __init__(self):
        super().__init__()
        self._worker = threading.get_ident()

    def filter(self, record):
        return record.thread != self._worker


class _Transport:
    """One asyncio task owns the MCP context managers and all serial requests."""
    def __init__(self, configuration):
        self.connection_id = str(uuid.uuid4())
        self._configuration = configuration
        self._ready = Future()
        self._loop = self._queue = self._task = None
        self._closed = False
        self._thread = threading.Thread(target=self._main, name="playwright-extension-mcp", daemon=True)
        with _BUSINESS_LOCK:
            # A failed close can leave an old thread alive after pool discard.
            # Never evict that evidence to make room for another connection.
            for transport in list(_TRANSPORTS):
                if (not transport._thread.is_alive()
                        and (transport._thread.ident is not None or transport._closed)):
                    _TRANSPORTS.discard(transport)
            if len(_TRANSPORTS) >= _TRANSPORT_LIMIT:
                raise ExtensionBrowserError("extension_transport_capacity_reached")
            self._pending_business = 0
            _TRANSPORTS.add(self)

    def start(self):
        with _BUSINESS_LOCK:
            if self._closed:
                raise ExtensionBrowserError("extension_session_closed")
            self._thread.start()
        try:
            self._ready.result(timeout=20)
        except Exception:
            self.close()
            raise ExtensionBrowserError("extension_connection_failed") from None
        return self

    def _main(self):
        # SDK parse diagnostics can contain raw server output. Suppress only
        # this worker thread's transport diagnostics; leave other clients alone.
        quiet = _QuietWorker()
        loggers = [logging.getLogger(name) for name in ("mcp.client.stdio", "mcp.client.session", "mcp.shared.session")]
        for logger in loggers:
            logger.addFilter(quiet)
        try:
            asyncio.run(self._serve())
        except BaseException:
            if not self._ready.done():
                self._ready.set_exception(ExtensionBrowserError("extension_connection_failed"))
        finally:
            self._closed = True
            for logger in loggers:
                logger.removeFilter(quiet)

    async def _serve(self):
        self._loop, self._task = asyncio.get_running_loop(), asyncio.current_task()
        self._queue = asyncio.Queue()
        pending = None
        try:
            async with _client(self._configuration) as client:
                self._configuration = None
                self._ready.set_result(True)
                while True:
                    item = await self._queue.get()
                    if item is None:
                        break
                    name, arguments, pending = item
                    try:
                        response = (await client.send_ping() if name == "__protocol_ping__"
                                    else await client.call_tool(name, arguments))
                    except BaseException:
                        self._closed = True
                        if not pending.done():
                            pending.set_exception(ExtensionBrowserError("extension_call_outcome_unknown"))
                        break
                    if name != "__protocol_ping__":
                        # The response settles this RPC even if its caller
                        # already timed out or the tool returned isError.
                        with _BUSINESS_LOCK:
                            self._pending_business -= 1
                    if not pending.done():
                        pending.set_result(response)
                    pending = None
        finally:
            self._configuration = None
            if pending is not None and not pending.done():
                pending.set_exception(ExtensionBrowserError("extension_call_outcome_unknown"))
            if self._queue is not None:
                while not self._queue.empty():
                    item = self._queue.get_nowait()
                    if item is not None and not item[2].done():
                        item[2].set_exception(ExtensionBrowserError("extension_session_closed"))

    def call(self, name, arguments):
        if name not in ("browser_tabs", "browser_run_code_unsafe"):
            raise ExtensionBrowserError("extension_session_closed")
        return self._request(name, arguments)

    def ping(self):
        """MCP protocol liveness only; never call a browser tool or create a tab."""
        return self._request("__protocol_ping__", {})

    def _request(self, name, arguments):
        pending = Future()
        with _BUSINESS_LOCK:
            if self._closed or self._queue is None:
                raise ExtensionBrowserError("extension_session_closed")
            if name != "__protocol_ping__":
                self._pending_business += 1
        try:
            self._loop.call_soon_threadsafe(self._queue.put_nowait, (name, arguments, pending))
            response = pending.result(timeout=RPC_TIMEOUT + 5)
            if getattr(response, "isError", False):
                raise ExtensionBrowserError("extension_tool_failed")
            return response
        except ExtensionBrowserError:
            raise
        except Exception:
            self.close()
            raise ExtensionBrowserError("extension_call_outcome_unknown") from None

    def close(self):
        if self._closed and not self._thread.is_alive():
            return
        self._closed = True
        if self._loop is not None and self._loop.is_running():
            self._loop.call_soon_threadsafe(self._task.cancel)
        if self._thread.ident is not None and threading.get_ident() != self._thread.ident:
            self._thread.join(timeout=10)


_SCRIPT = r"""async (page) => {
  const p = __PAYLOAD__;
  const key = '__webcodex_handoff_registry_v1';
  const checkUrl = () => {
    const u = new URL(page.url());
    if (u.protocol !== 'https:' || u.hostname !== 'chatgpt.com' || u.username || u.password || (u.port && u.port !== '443'))
      throw new Error('owned_origin_changed');
    return u.origin + u.pathname;
  };
  checkUrl();
  if (p.op === 'initialize') {
    if (page[key]) throw new Error('page_already_owned');
    Object.defineProperty(page, key, {value:{owner:p.owner, handles:new Map(), next:0}, configurable:true});
  }
  const r = page[key];
  if (!r || r.owner !== p.owner) throw new Error('owned_page_changed');
  let value = null;
  const handle = () => {
    const h = r.handles.get(p.handle);
    if (!h) throw new Error('unknown_handle');
    return h;
  };
  switch (p.op) {
    case 'initialize': value = true; break;
    case 'guard': value = true; break;
    case 'url': value = checkUrl(); break;
    case 'activate': await page.bringToFront(); value = true; break;
    case 'evaluate': value = await page.evaluate('(' + p.js + ')(null,' + JSON.stringify(p.argument) + ')'); break;
    case 'find': {
      const locator = page.locator('xpath=' + p.xpath);
      if (!p.all) {
        await locator.waitFor({state:'attached', timeout:p.timeout});
        if (await locator.count() !== 1) throw new Error('element_not_unique');
      }
      const handles = p.all ? await locator.elementHandles() : [await locator.elementHandle({timeout:p.timeout})];
      if (handles.length > 64 || handles.some(h => !h) || r.handles.size + handles.length > 2048) throw new Error('handle_limit');
      value = handles.map(h => {const id='h'+(++r.next);r.handles.set(id,h);return id;});
      break;
    }
    case 'visible': value = await handle().isVisible(); break;
    case 'trial': await handle().click({button:'left', trial:true, timeout:p.timeout}); value = true; break;
    case 'click': await handle().click({button:'left', trial:false, timeout:p.timeout}); value = true; break;
    case 'text': await page.keyboard.insertText(p.text); value = true; break;
    case 'key':
      if (!['ArrowRight','ArrowLeft','Space'].includes(p.key)) throw new Error('unsupported_key');
      await page.keyboard.press(p.key); value = true; break;
    case 'append': {
      const focused = await handle().evaluate(e => e === document.activeElement || e.contains(document.activeElement));
      if (!focused) throw new Error('composer_not_focused');
      await page.keyboard.insertText(p.text); value = true; break;
    }
    default: throw new Error('unsupported_operation');
  }
  checkUrl();
  return {bridge:'webcodex.extension.v1', owner:p.owner, value:value === undefined ? null : value};
}"""


def _code(owner, operation, **data):
    return _SCRIPT.replace("__PAYLOAD__", json.dumps(dict(data, owner=owner, op=operation), ensure_ascii=False))


def _result(response, owner):
    try:
        content = response.content
        if len(content) != 1 or content[0].type != "text":
            raise ValueError()
        text = content[0].text
        if len(text) > 1048576 or not text.startswith("### Result\n"):
            raise ValueError()
        body = text[len("### Result\n"):].lstrip()
        value, end = json.JSONDecoder().raw_decode(body)
        tail = body[end:]
        if tail.strip() and not tail.startswith("\n### "):
            raise ValueError()
        if not isinstance(value, dict) or value.get("bridge") != "webcodex.extension.v1" or value.get("owner") != owner:
            raise ValueError()
        return value["value"]
    except Exception:
        raise ExtensionBrowserError("extension_response_invalid") from None


def _delay(value, maximum=10, positive=False):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0 or value > maximum or (positive and value == 0):
        raise ExtensionBrowserError("invalid_extension_timeout")
    return float(value)


class _Session:
    def __init__(self):
        self._transport = None
        self._created = False
        self._persistent = False

    def __enter__(self):
        self._transport = _Transport(_configuration()).start()
        return self

    def __exit__(self, *_args):
        if self._transport is not None:
            transport, self._transport = self._transport, None
            transport.close()

    def create(self, url):
        if url != CHAT_URL or self._transport is None or self._created:
            raise ExtensionBrowserError("extension_new_page_not_allowed")
        self._created = True
        self._transport.call("browser_tabs", {"action": "new", "url": CHAT_URL})
        browser = Browser(self, str(uuid.uuid4()))
        browser._rpc("initialize")
        return browser


def session():
    return _Session()


class _TransportLease:
    """Failure is sticky even when a caller catches the exception."""
    def __init__(self, transport):
        self.transport = transport
        self.connection_id = transport.connection_id
        self.failed = False
        self.released = False

    def call(self, name, arguments):
        try:
            if self.failed or self.released:
                raise ExtensionBrowserError("extension_session_closed")
            return self.transport.call(name, arguments)
        except BaseException:
            self.failed = True
            raise


class ConnectionPool:
    """One process, one MCP connection, one nonblocking logical-session lease.

The lease covers whole browser workflows, not just individual RPCs: all code
uses the MCP backend's single current tab. Protocol ping cannot certify that
the extension is still connected; an action failure is never retried here.
"""
    def __init__(self, transport_factory=None, configuration_factory=None):
        self._factory = transport_factory
        self._configuration_factory = configuration_factory
        self._lease_lock = threading.Lock()
        self._state_lock = threading.Lock()
        self._transport = self._active = None
        self._closed = self._borrowed = False
        self._created_count = self._reuse_count = 0

    def _discard(self, transport):
        with self._state_lock:
            if self._transport is transport:
                self._transport = None
        if transport is not None:
            transport.close()

    def acquire(self):
        if not self._lease_lock.acquire(blocking=False):
            raise ExtensionBrowserError("extension_pool_busy")
        try:
            with self._state_lock:
                if self._closed:
                    raise ExtensionBrowserError("extension_pool_closed")
                self._borrowed = True
                transport = self._transport
            reused = False
            if transport is not None:
                try:
                    if transport._closed:
                        raise ExtensionBrowserError("extension_session_closed")
                    transport.ping()
                    reused = True
                except Exception:
                    self._discard(transport)
                    transport = None
            if transport is None:
                with self._state_lock:
                    if self._closed:
                        raise ExtensionBrowserError("extension_pool_closed")
                factory = self._factory or _Transport
                configuration = self._configuration_factory or _configuration
                transport = factory(configuration())
                try:
                    transport.start()  # exactly one fresh attempt at this boundary
                except BaseException:
                    transport.close()
                    raise
            with self._state_lock:
                if self._closed:
                    rejected = True
                else:
                    rejected = False
                    self._transport = transport
                    if reused:
                        self._reuse_count += 1
                    else:
                        self._created_count += 1
                    self._active = _TransportLease(transport)
                    return self._active
            if rejected:
                transport.close()
                raise ExtensionBrowserError("extension_pool_closed")
        except BaseException:
            with self._state_lock:
                self._borrowed = False
            self._lease_lock.release()
            raise

    def release(self, lease):
        with self._state_lock:
            if self._active is not lease:
                raise ExtensionBrowserError("extension_lease_invalid")
            lease.released = True
            self._active = None
            self._borrowed = False
            discard = lease.failed or lease.transport._closed or self._closed
        try:
            if discard:
                self._discard(lease.transport)
        finally:
            self._lease_lock.release()

    def close(self):
        """Terminate this process's pool; never close a browser tab explicitly."""
        with self._state_lock:
            self._closed = True
            transport, self._transport = self._transport, None
            if self._active is not None:
                self._active.failed = True
        if transport is not None:
            transport.close()

    def status(self):
        """Pure memory snapshot; connected means MCP transport open, not browser health."""
        with self._state_lock:
            transport = self._transport
            return {"connection_id": transport.connection_id if transport is not None else None,
                    "created_count": self._created_count, "reuse_count": self._reuse_count,
                    "connected": transport is not None and not transport._closed,
                    "borrowed": self._borrowed, "closed": self._closed,
                    "health_scope": "mcp_transport_only"}


class _PersistentSession(_Session):
    def __init__(self, pool):
        super().__init__()
        self._pool = pool
        self._persistent = True

    def __enter__(self):
        self._transport = self._pool.acquire()
        return self

    def __exit__(self, *_args):
        if self._transport is not None:
            lease, self._transport = self._transport, None
            self._pool.release(lease)


_CONNECTION_POOL = ConnectionPool()


def persistent_session():
    return _PersistentSession(_CONNECTION_POOL)


def close_persistent_connection():
    _CONNECTION_POOL.close()


def persistent_connection_status():
    return _CONNECTION_POOL.status()


atexit.register(close_persistent_connection)


class Browser:
    def __init__(self, session, owner):
        self._session, self._owner = session, owner

    def _rpc(self, operation, **data):
        if self._session._transport is None:
            raise ExtensionBrowserError("extension_session_closed")
        response = self._session._transport.call("browser_run_code_unsafe", {"code": _code(self._owner, operation, **data)})
        return _result(response, self._owner)

    def _guard(self):
        if self._rpc("guard") is not True:
            raise ExtensionBrowserError("extension_page_not_owned")

    def identity(self):
        self._guard()
        return {"owned_page_id": self._owner, "scope": "created_for_handoff", "origin": "https://chatgpt.com",
                "connection": "official_playwright_extension",
                "connection_id": getattr(self._session._transport, "connection_id", None),
                "persistent": self._session._persistent}

    def get_url(self):
        return self._rpc("url")

    def activate(self):
        self._rpc("activate")

    def execute_javascript(self, js, argument=None):
        if not isinstance(js, str) or len(js) > 65536 or not _FUNCTION.match(js):
            raise ExtensionBrowserError("invalid_handoff_observation_function")
        return self._rpc("evaluate", js=js, argument=argument)

    def _find(self, xpath, timeout, all_matches):
        milliseconds = _delay(timeout, 30, positive=True) * 1000
        if not isinstance(xpath, str) or len(xpath) > 8192 or "\0" in xpath or not xpath.startswith(("/", "(", "./")):
            raise ExtensionBrowserError("invalid_handoff_xpath")
        handles = self._rpc("find", xpath=xpath, timeout=milliseconds, all=all_matches)
        if not isinstance(handles, list) or len(handles) > 64 or any(not isinstance(h, str) or not re.fullmatch(r"h[1-9][0-9]*", h) for h in handles):
            raise ExtensionBrowserError("extension_response_invalid")
        return [Element(self, handle, milliseconds) for handle in handles]

    def find_by_xpath(self, xpath, timeout=20):
        values = self._find(xpath, timeout, False)
        if len(values) != 1:
            raise ExtensionBrowserError("handoff_element_not_unique")
        return values[0]

    def find_all_by_xpath(self, xpath, timeout=20):
        return self._find(xpath, timeout, True)

    def type_text(self, text, *, delay_after=0):
        delay = _delay(delay_after)
        if not isinstance(text, str):
            raise ExtensionBrowserError("invalid_handoff_text")
        self._rpc("text", text=text)
        if delay:
            time.sleep(delay)

    def keys(self, value, *, contains_hotkey=True, force_ime_eng=False, delay_after=0):
        delay = _delay(delay_after)
        if not isinstance(contains_hotkey, bool) or not isinstance(force_ime_eng, bool):
            raise ExtensionBrowserError("invalid_handoff_key_options")
        if not contains_hotkey:
            return self.type_text(value, delay_after=delay)
        if not isinstance(value, str) or value not in _KEYS:
            raise ExtensionBrowserError("unsupported_handoff_key")
        self._rpc("key", key=_KEYS[value])
        if delay:
            time.sleep(delay)


class Element:
    def __init__(self, browser, handle, timeout):
        self._browser, self._handle, self._timeout = browser, handle, timeout

    def is_displayed(self):
        return self._browser._rpc("visible", handle=self._handle)

    def click(self, *, button="left", simulative=True, move_mouse=True, delay_after=0, before_actual=None):
        delay = _delay(delay_after)
        if button != "left" or not isinstance(simulative, bool) or not isinstance(move_mouse, bool) or (before_actual is not None and not callable(before_actual)):
            raise ExtensionBrowserError("unsupported_handoff_click")
        self._browser._rpc("trial", handle=self._handle, timeout=self._timeout)
        self._browser._guard()
        if before_actual is not None:
            try:
                allowed = before_actual()
            except Exception:
                raise ExtensionBrowserError("handoff_click_guard_failed") from None
            if allowed is not True:
                raise ExtensionBrowserError("handoff_click_cancelled")
        self._browser._rpc("click", handle=self._handle, timeout=self._timeout)
        if delay:
            time.sleep(delay)

    def clipboard_input(self, text, *, append=False, delay_after=0):
        delay = _delay(delay_after)
        if append is not True or not isinstance(text, str):
            raise ExtensionBrowserError("handoff_input_requires_append")
        self._browser._rpc("append", handle=self._handle, text=text)
        if delay:
            time.sleep(delay)
