"""Closed-choice smoke client for official @playwright/mcp 0.0.83 extension mode.

This module is inert on import. Only main() starts the official MCP process.
No token, raw tool text, browser URL query, or chat contents enter stdout.
"""
from __future__ import annotations

import argparse
import asyncio
import contextlib
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import logging
import os
from pathlib import Path
import re
import shutil
import sys
import threading
import time
import uuid

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from mcp.types import Implementation

ROOT = Path(__file__).resolve().parent
NODE = Path(os.environ.get("WEBCODEX_NODE", "") or shutil.which("node") or
            str(Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "nodejs" / "node.exe"))
MCP_CLI = ROOT / "node_modules" / "@playwright" / "mcp" / "cli.js"
TOKEN_FILE = ROOT.parent / "runtime" / "playwright-extension-token.txt"
OUTPUT_DIR = ROOT.parent / "runtime" / "extension-mcp"
EXPECTED_VERSION = "0.0.83"
ENV_KEYS = {"SYSTEMROOT", "WINDIR", "COMSPEC", "PATH", "PATHEXT", "TEMP", "TMP", "USERPROFILE",
    "LOCALAPPDATA", "APPDATA", "PROGRAMDATA", "PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432",
    "HOMEDRIVE", "HOMEPATH", "HOME", "USERNAME", "COMPUTERNAME", "LANG", "LC_ALL"}
ACTIONS = ("list-tools", "status", "click-test", "chatgpt-click")


class SmokeError(Exception):
    """Only fixed diagnostic codes are printed."""


def load_token(required):
    try:
        with TOKEN_FILE.open("rb") as source:
            raw = source.read(4097)
    except FileNotFoundError:
        if not required:
            return None
        raise SmokeError("token_file_missing") from None
    if len(raw) > 4096:
        raise SmokeError("invalid_token_file")
    try:
        token = raw.decode("utf-8-sig").strip()
    except UnicodeError:
        raise SmokeError("invalid_token_file") from None
    if len(token) < 16 or any(char in token for char in ("\r", "\n", "\0")):
        raise SmokeError("invalid_token_file")
    return token


def server_parameters(browser, token):
    if browser != "msedge":
        raise SmokeError("unexpected_browser_configuration")
    package = json.loads(MCP_CLI.with_name("package.json").read_text(encoding="utf-8"))
    if package.get("name") != "@playwright/mcp" or package.get("version") != EXPECTED_VERSION:
        raise SmokeError("unexpected_mcp_package_version")
    if not NODE.is_file() or not MCP_CLI.is_file():
        raise SmokeError("mcp_runtime_missing")
    env = {key: value for key, value in os.environ.items() if key.upper() in ENV_KEYS}
    if token is not None:
        env["PLAYWRIGHT_MCP_EXTENSION_TOKEN"] = token
    return StdioServerParameters(command=str(NODE),
        args=[str(MCP_CLI), "--extension", "--browser", browser, "--profile-dir-name", "Default", "--no-webmcp", "--console-level", "error",
            "--timeout-action", "10000", "--timeout-navigation", "30000", "--image-responses", "omit",
            "--output-dir", str(OUTPUT_DIR)],
        env=env, cwd=ROOT, encoding="utf-8")


def text_content(result):
    # Never return this string to stdout or write it to a log. Official connect
    # pages can contain the extension token in their URL on some releases.
    return "\n".join(item.text for item in result.content if getattr(item, "type", None) == "text")


async def invoke(session, name, arguments):
    result = await session.call_tool(name, arguments, read_timeout_seconds=timedelta(seconds=45))
    if result.isError:
        raise SmokeError("mcp_" + name + "_failed")
    return text_content(result)


def ref_for(text, role, exact_name=None):
    names = []
    for line in text.splitlines():
        if not re.match(r"\s*-\s*" + re.escape(role) + r"(?:\s|:|\[)", line):
            continue
        if exact_name is not None and '"' + exact_name + '"' not in line:
            continue
        match = re.search(r"\[ref=([A-Za-z0-9_-]+)\]", line)
        if match:
            names.append(match.group(1))
    if len(names) != 1:
        raise SmokeError("ambiguous_" + role + "_snapshot")
    return names[0]


@contextlib.contextmanager
def fixture_page():
    marker = uuid.uuid4().hex
    received = threading.Event()
    evidence = {}
    route = "/extension-smoke/" + marker
    title = "Extension Smoke " + marker
    button = "Click smoke " + marker
    html = ("<!doctype html><meta charset=utf-8><title>" + title + "</title><h1>" + title + "</h1>"
        "<button id=smoke>" + button + "</button><p id=result>Not clicked</p><script>"
        "let clicks=0;document.getElementById('smoke').addEventListener('click',e=>{clicks++;"
        "document.getElementById('result').textContent='Click verified';"
        "fetch(location.pathname+'/receipt',{method:'POST',headers:{'Content-Type':'application/json'},"
        "body:JSON.stringify({trusted:e.isTrusted,count:clicks})});});</script>").encode("utf-8")

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            if self.path != route:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(html)

        def do_POST(self):
            size = int(self.headers.get("Content-Length", "0"))
            if self.path != route + "/receipt" or not 0 < size <= 256:
                self.send_error(400)
                return
            try:
                value = json.loads(self.rfile.read(size))
                evidence.update(trusted=value.get("trusted") is True, count=value.get("count"))
            except (ValueError, AttributeError):
                self.send_error(400)
                return
            self.send_response(204)
            self.end_headers()
            received.set()

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield {"url": "http://127.0.0.1:" + str(server.server_port) + route,
            "marker": marker, "button": button, "event": received, "evidence": evidence}
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


async def exercise(session, action):
    catalog = await session.list_tools()
    specs = {tool.name: tool.inputSchema for tool in catalog.tools}
    if action == "list-tools":
        return {"ok": True, "action": action, "package_version": EXPECTED_VERSION,
            "tools": sorted(specs), "browser_tool_called": False}
    required = {"browser_tabs", "browser_snapshot", "browser_click"}
    if not required <= set(specs) or "target" not in specs["browser_click"].get("properties", {}):
        raise SmokeError("unexpected_browser_tool_contract")
    if action == "status":
        raw = await invoke(session, "browser_tabs", {"action": "list"})
        count = len(re.findall(r"(?m)^\s*-\s*\d+:", raw))
        return {"ok": True, "action": action, "extension_connected": True, "visible_tab_count": count,
            "navigated_existing_tab": False, "raw_browser_text_returned": False}
    if action == "click-test":
        with fixture_page() as fixture:
            created = await invoke(session, "browser_tabs", {"action": "new", "url": fixture["url"]})
            if fixture["marker"] not in created:
                raise SmokeError("new_fixture_tab_not_verified")
            before = await invoke(session, "browser_snapshot", {})
            if fixture["marker"] not in before:
                raise SmokeError("fixture_snapshot_mismatch")
            target = ref_for(before, "button", fixture["button"])
            await invoke(session, "browser_click", {"target": target, "element": "Isolated extension smoke button"})
            ready = await asyncio.to_thread(fixture["event"].wait, 3)
            if not ready or fixture["evidence"] != {"trusted": True, "count": 1}:
                raise SmokeError("click_receipt_not_verified")
            return {"ok": True, "action": action, "created_own_test_tab": True,
                "trusted_browser_click": True, "click_count": 1, "messages_sent": 0}
    raise SmokeError("unsupported_action")


def chatgpt_click(backend=None, handoff=None):
    """Use the already validated semantic façade, never ARIA name guessing."""
    if backend is None or handoff is None:
        if str(ROOT.parent) not in sys.path:
            sys.path.insert(0, str(ROOT.parent))
        import edge_handoff
        import playwright_extension_browser
        backend = backend or playwright_extension_browser
        handoff = handoff or edge_handoff
    request = {"launch_id": str(uuid.uuid4()),
        "expires_at": (datetime.now(timezone.utc) + timedelta(seconds=120)).isoformat()}
    deadline = time.monotonic() + 90
    with backend.session() as session:
        browser = session.create("https://chatgpt.com/")
        browser.activate()
        owner = browser.identity()["owned_page_id"]
        url = browser.get_url()
        if url != "https://chatgpt.com/":
            raise SmokeError("new_chatgpt_tab_not_verified")
        state, editor = handoff.setup_state(browser), handoff.composer(browser)
        chat = state.get("chat", {}).get("count") == 1 and state["chat"].get("pressed") == "true"
        pro = state.get("trigger", {}).get("count") == 1 and state["trigger"].get("text") == "Pro"
        if (not chat or not pro or editor.get("count") != 1 or editor.get("user_count")
                or (editor.get("text") or "").strip()):
            raise SmokeError("logged_in_empty_chat_pro_ui_not_verified")
        handoff.click_control(browser, "trigger", request, deadline)
        opened = handoff.setup_state(browser)
        if opened.get("menu_count") != 1 or opened.get("intelligence") != "Pro":
            raise SmokeError("model_menu_not_verified")
        # Re-observe after opening the menu; resolve only the current composer.
        current = handoff.composer(browser)
        if current.get("count") != 1 or not current.get("xpath"):
            raise SmokeError("current_composer_not_unique")
        xpath = current["xpath"]

        def may_close():
            try:
                fresh = handoff.composer(browser)
                return (time.monotonic() < deadline and not handoff.flow.cancelled(request)
                    and browser.identity()["owned_page_id"] == owner and browser.get_url() == url
                    and fresh.get("count") == 1 and fresh.get("xpath") == xpath
                    and not fresh.get("user_count") and not (fresh.get("text") or "").strip()
                    and fresh.get("document_visible") is True and fresh.get("document_focused") is True)
            except Exception:
                return False

        if not may_close():
            raise SmokeError("owned_composer_changed")
        element = browser.find_by_xpath(xpath, timeout=3)
        element.click(simulative=True, move_mouse=True, delay_after=0.25, before_actual=may_close)
        final = handoff.setup_state(browser)
        if (browser.identity()["owned_page_id"] != owner or browser.get_url() != url
                or final.get("menu_count") != 0 or final.get("trigger", {}).get("text") != "Pro"):
            raise SmokeError("model_menu_close_not_verified")
        return {"ok": True, "action": "chatgpt-click", "backend": "official_playwright_extension",
            "created_own_chatgpt_tab": True, "logged_in_ui_observed": True,
            "chat_control_observed": True, "pro_control_observed": True,
            "model_menu_opened": True, "model_menu_closed": True, "actual_click_count": 2, "messages_sent": 0}


async def run(action, browser):
    if action == "chatgpt-click":
        return await asyncio.to_thread(chatgpt_click)
    token = load_token(required=action != "list-tools")
    parameters = server_parameters(browser, token)
    logging.getLogger("mcp").setLevel(logging.CRITICAL)
    # Discard raw child stderr: it may include an extension connection URL.
    with open(os.devnull, "w", encoding="utf-8") as diagnostics:
        async with stdio_client(parameters, errlog=diagnostics) as (read, write):
            async with ClientSession(read, write, read_timeout_seconds=timedelta(seconds=45),
                    client_info=Implementation(name="WebCodex Extension Smoke", version="0.0.1")) as session:
                await session.initialize()
                return await exercise(session, action)


def controlled_error(error):
    if isinstance(error, SmokeError):
        return str(error)
    for nested in getattr(error, "exceptions", ()):
        code = controlled_error(nested)
        if code is not None:
            return code
    return None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=ACTIONS, nargs="?", default="list-tools")
    parser.add_argument("--browser", choices=("msedge",), default="msedge")
    args = parser.parse_args(argv)
    try:
        value = asyncio.run(run(args.action, args.browser))
        code = 0
    except SmokeError as error:
        value, code = {"ok": False, "action": args.action, "reason": str(error)}, 2
    except Exception as error:
        # Exception repr/tracebacks can contain environment or tool messages.
        reason = controlled_error(error) or "smoke_transport_or_configuration_failed"
        value, code = {"ok": False, "action": args.action, "reason": reason}, 2
    print(json.dumps(value, ensure_ascii=False))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
