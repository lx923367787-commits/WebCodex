import asyncio
from concurrent.futures import Future
from contextlib import asynccontextmanager, redirect_stdout, redirect_stderr
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import playwright_extension_browser as ext


def payload(arguments):
    source = arguments["code"].split("const p = ", 1)[1]
    return json.JSONDecoder().raw_decode(source)[0]


def response(owner, value):
    body = {"bridge": "webcodex.extension.v1", "owner": owner, "value": value}
    return SimpleNamespace(isError=False, content=[SimpleNamespace(type="text", text="### Result\n" + json.dumps(body))])


class FakeTransport:
    def __init__(self, _configuration):
        self.calls = []
        self.owner = None
        self.failure = None
        self.closed = False

    def start(self):
        return self

    def close(self):
        self.closed = True

    def call(self, name, arguments):
        if name == "browser_tabs":
            self.calls.append((name, arguments))
            return response("unused", None)
        p = payload(arguments)
        self.calls.append((p["op"], p))
        if p["op"] == self.failure:
            raise ext.ExtensionBrowserError("extension_call_outcome_unknown")
        if p["op"] == "initialize":
            self.owner = p["owner"]
        if p["owner"] != self.owner:
            raise ext.ExtensionBrowserError("extension_tool_failed")
        values = {"find": ["h1"], "visible": True, "url": ext.CHAT_URL, "evaluate": {"fixture": True}}
        return response(p["owner"], values.get(p["op"], True))


class ConfigurationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        package = self.root / "extension-runtime/node_modules/@playwright/mcp"
        package.mkdir(parents=True)
        (package / "package.json").write_text('{"version":"0.0.83"}', encoding="utf-8")
        (package / "cli.js").write_text("// fixture", encoding="utf-8")
        self.node = self.root / "node.exe"
        self.node.write_bytes(b"fixture, never executed")
        (self.root / "runtime").mkdir()
        self.token_file = self.root / "runtime/playwright-extension-token.txt"
        self.token = "a" * 43
        self.token_file.write_text(self.token, encoding="utf-8")
        self.patches = [patch.object(ext, "ROOT", self.root), patch.object(ext, "NODE", self.node)]
        for p in self.patches:
            p.start()
        self.addCleanup(self.temp.cleanup)
        for p in self.patches:
            self.addCleanup(p.stop)

    def test_pinned_stdio_configuration_whitelists_environment_and_keeps_token_off_argv(self):
        with patch.dict(os.environ, {"SERVER_TOKEN": "secret", "OPENAI_API_KEY": "secret", "WEBCODEX_READ_TOKEN": "secret"}):
            configuration = ext._configuration()
        self.assertEqual(configuration["command"], str(self.node))
        self.assertNotIn(self.token, " ".join(configuration["args"]))
        self.assertEqual(configuration["env"]["PLAYWRIGHT_MCP_EXTENSION_TOKEN"], self.token)
        self.assertTrue(set(k.upper() for k in configuration["env"]) <= ext._ENV | {"PLAYWRIGHT_MCP_EXTENSION_TOKEN"})
        self.assertEqual(configuration["args"][1:7], ["--extension", "--browser", "msedge", "--profile-dir-name", "Default", "--codegen"])
        self.assertEqual(configuration["args"][-2:], ["--output-dir", str(self.root / "runtime/extension-mcp")])

    def test_preflight_reads_only_and_never_prints_token(self):
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.object(ext, "_Transport") as transport, redirect_stdout(stdout), redirect_stderr(stderr):
            self.assertEqual(ext.discover_endpoint(), "official_playwright_extension_stdio")
        transport.assert_not_called()
        self.assertEqual(stdout.getvalue() + stderr.getvalue(), "")

    def test_invalid_token_and_version_fail_with_controlled_errors(self):
        for token in ("short", "a" * 5000, "a" * 40 + "\nsecret", "PLAYWRIGHT_MCP_EXTENSION_TOKEN=" + self.token):
            self.token_file.write_text(token, encoding="utf-8")
            with self.assertRaises(ext.ExtensionBrowserError) as error:
                ext.discover_endpoint()
            self.assertEqual(str(error.exception), "extension_configuration_unavailable")
        self.token_file.write_text(self.token, encoding="utf-8")
        (self.root / "extension-runtime/node_modules/@playwright/mcp/package.json").write_text('{"version":"latest"}')
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_configuration_unavailable"):
            ext.discover_endpoint()

    def test_node_explicit_override_has_priority_without_fallback(self):
        explicit = self.root / "explicit-node.exe"
        with patch.dict(os.environ, {"WEBCODEX_NODE": str(explicit)}, clear=True), \
                patch.object(ext.shutil, "which") as lookup, patch.object(Path, "is_file") as exists:
            self.assertEqual(ext._discover_node(), explicit)
        lookup.assert_not_called()
        exists.assert_not_called()

    def test_node_bundled_with_application_is_found_without_launcher(self):
        bundled = self.root / "node" / "node.exe"
        bundled.parent.mkdir()
        bundled.write_bytes(b"fixture, never executed")
        with patch.object(ext, "ROOT", self.root / "chat-handoff"), \
                patch.dict(os.environ, {}, clear=True), patch.object(ext.shutil, "which") as lookup:
            self.assertEqual(ext._discover_node(), bundled)
        lookup.assert_not_called()

    def test_node_system_path_and_program_files_remain_fallbacks(self):
        program_files = self.root / "Program Files"
        with patch.object(ext, "ROOT", self.root / "chat-handoff"), \
                patch.dict(os.environ, {"ProgramFiles": str(program_files)}, clear=True):
            with patch.object(ext.shutil, "which", return_value=str(self.node)):
                self.assertEqual(ext._discover_node(), self.node)
            with patch.object(ext.shutil, "which", return_value=None):
                self.assertEqual(ext._discover_node(), program_files / "nodejs" / "node.exe")


class FacadeTests(unittest.TestCase):
    def setUp(self):
        self.transport = FakeTransport({})
        self.config = patch.object(ext, "_configuration", return_value={})
        self.factory = patch.object(ext, "_Transport", return_value=self.transport)
        self.config.start()
        self.factory.start()
        self.addCleanup(self.config.stop)
        self.addCleanup(self.factory.stop)

    def test_one_new_page_and_disconnect_without_browser_close(self):
        with ext.session() as session:
            with self.assertRaises(ext.ExtensionBrowserError):
                session.create("https://example.com/")
            browser = session.create(ext.CHAT_URL)
            self.assertEqual(browser.identity()["scope"], "created_for_handoff")
            self.assertEqual(browser.get_url(), ext.CHAT_URL)
            with self.assertRaises(ext.ExtensionBrowserError):
                session.create(ext.CHAT_URL)
        self.assertEqual(self.transport.calls[0], ("browser_tabs", {"action": "new", "url": ext.CHAT_URL}))
        self.assertTrue(self.transport.closed)
        self.assertFalse(any(name == "browser_close" for name, _ in self.transport.calls))
        with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_session_closed"):
            browser.get_url()

    def test_same_handle_trial_stop_check_then_one_actual_click(self):
        with ext.session() as session:
            element = session.create(ext.CHAT_URL).find_by_xpath("(//button)[1]", timeout=2)
            checks = []
            element.click(before_actual=lambda: checks.append([name for name, _ in self.transport.calls]) or True)
        names = [name for name, _ in self.transport.calls]
        self.assertEqual(names[-3:], ["trial", "guard", "click"])
        self.assertEqual(checks[0][-1], "guard")
        clicks = [p for name, p in self.transport.calls if name in ("trial", "click")]
        self.assertEqual([p["handle"] for p in clicks], ["h1", "h1"])

    def test_stop_or_exception_after_trial_never_calls_actual(self):
        with ext.session() as session:
            element = session.create(ext.CHAT_URL).find_by_xpath("//button")
            for callback in (lambda: False, lambda: None, lambda: 1, Mock(side_effect=RuntimeError("private text"))):
                with self.assertRaises(ext.ExtensionBrowserError):
                    element.click(before_actual=callback)
        self.assertFalse(any(name == "click" for name, _ in self.transport.calls))

    def test_actual_unknown_has_one_dispatch_and_no_retry(self):
        with ext.session() as session:
            element = session.create(ext.CHAT_URL).find_by_xpath("//button")
            self.transport.failure = "click"
            with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_call_outcome_unknown"):
                element.click()
        self.assertEqual(sum(name == "click" for name, _ in self.transport.calls), 1)

    def test_unicode_and_multiline_are_json_data_not_hotkeys_or_node_source(self):
        text = "继续工作\n{ENTER} 中文🙂\n\"; throw new Error('injection'); //"
        with ext.session() as session:
            browser = session.create(ext.CHAT_URL)
            browser.keys("@webcodex", contains_hotkey=False, force_ime_eng=True)
            browser.keys("{SPACE}")
            browser.keys("{RIGHT}")
            browser.find_by_xpath("//*[@contenteditable='true']").clipboard_input(text, append=True)
            browser.execute_javascript("function(_element,argument){return argument}", text)
        append = next(p for name, p in self.transport.calls if name == "append")
        self.assertEqual(append["text"], text)
        self.assertEqual([p["key"] for name, p in self.transport.calls if name == "key"], ["Space", "ArrowRight"])

    def test_result_parser_rejects_page_text_and_wrong_owner_without_exposing_it(self):
        for value in (SimpleNamespace(content=[SimpleNamespace(type="text", text="private connect URL and token")]),
                      response("other-owner", True)):
            with self.assertRaises(ext.ExtensionBrowserError) as error:
                ext._result(value, "owner")
            self.assertEqual(str(error.exception), "extension_response_invalid")


class TransportTests(unittest.TestCase):
    def test_stdio_context_lifetime_and_calls_stay_in_one_async_task(self):
        events = []

        @asynccontextmanager
        async def client(_configuration):
            task = asyncio.current_task()
            events.append(("enter", task))

            class Client:
                async def call_tool(self, name, arguments):
                    events.append((name, asyncio.current_task()))
                    return response("owner", True)
            try:
                yield Client()
            finally:
                events.append(("exit", asyncio.current_task()))

        with patch.object(ext, "_client", client):
            transport = ext._Transport({}).start()
            transport.call("browser_tabs", {"action": "new", "url": ext.CHAT_URL})
            transport.close()
        self.assertEqual([name for name, _ in events], ["enter", "browser_tabs", "exit"])
        self.assertTrue(all(task is events[0][1] for _, task in events))

    def test_transport_exception_is_unknown_sanitized_and_never_retried(self):
        calls = []

        @asynccontextmanager
        async def client(_configuration):
            class Client:
                async def call_tool(self, name, arguments):
                    calls.append(name)
                    raise RuntimeError("private handshake URL and token")
            yield Client()

        with patch.object(ext, "_client", client):
            transport = ext._Transport({}).start()
            with self.assertRaisesRegex(ext.ExtensionBrowserError, "extension_call_outcome_unknown"):
                transport.call("browser_run_code_unsafe", {"code": "unused fixture"})
            transport.close()
        self.assertEqual(calls, ["browser_run_code_unsafe"])


class IsolatedNodeTemplateTests(unittest.TestCase):
    def test_fresh_vm_contexts_keep_node_page_handle_identity_without_browser(self):
        if not ext.NODE.is_file():
            self.skipTest("Fixed local Node executable unavailable")
        owner = "isolated-owner"
        scripts = [ext._code(owner, "initialize"), ext._code(owner, "find", xpath="//button", all=False, timeout=200),
                   ext._code(owner, "trial", handle="h1", timeout=200), ext._code(owner, "click", handle="h1", timeout=200),
                   ext._code("wrong-owner", "click", handle="h1", timeout=200)]
        fixture = r"""
const vm=require('vm');let input='';process.stdin.setEncoding('utf8');process.stdin.on('data',s=>input+=s);
process.stdin.on('end',async()=>{
  const codes=JSON.parse(input),events=[];
  const old={detached:false,click:async function(o){if(this.detached)throw Error('detached');events.push(o.trial?'trial':'actual');}};
  const replacement={click:async()=>events.push('replacement')};
  let target=old;
  const page={url:()=> 'https://chatgpt.com/',locator:()=>({waitFor:async()=>{},count:async()=>1,elementHandle:async()=>target})};
  const values=[];
  for(let i=0;i<codes.length;i++){
    if(i===3){old.detached=true;target=replacement;}
    try{values.push(await vm.runInNewContext('('+codes[i]+')(page)',{page,URL}));}
    catch(e){values.push({error:true});}
  }
  process.stdout.write(JSON.stringify({values,events}));
});
"""
        result = subprocess.run([str(ext.NODE), "-e", fixture], input=json.dumps(scripts), capture_output=True,
                                text=True, encoding="utf-8", timeout=10, check=True,
                                env={k:v for k,v in os.environ.items() if k.upper() in ext._ENV})
        data = json.loads(result.stdout)
        self.assertEqual(data["values"][1]["value"], ["h1"])
        self.assertTrue(data["values"][2]["value"])
        self.assertEqual(data["values"][3:], [{"error": True}, {"error": True}])
        self.assertEqual(data["events"], ["trial"])


@unittest.skipUnless(os.environ.get("WEBCODEX_EXTENSION_HEADLESS_TEST") == "1", "Explicit isolated headless test only")
class IsolatedOfficialMcpTests(unittest.TestCase):
    def test_real_mcp_keeps_handles_and_delivers_trusted_click_without_network(self):
        package = ext.ROOT / "extension-runtime/node_modules/@playwright/mcp"
        self.assertEqual(json.loads((package / "package.json").read_text(encoding="utf-8"))["version"], ext.VERSION)
        with tempfile.TemporaryDirectory(prefix="webcodex-headless-mcp-") as output:
            configuration = {"command": str(ext.NODE), "args": [str(package / "cli.js"),
                "--isolated", "--headless", "--browser", "msedge", "--block-service-workers",
                "--codegen", "none", "--image-responses", "omit", "--snapshot-mode", "none",
                "--console-level", "error", "--no-webmcp", "--output-dir", output],
                "env": {k:v for k,v in os.environ.items() if k.upper() in ext._ENV}, "cwd": str(ext.ROOT)}
            setup = r"""async (page) => {
              const c = page.context();
              c.__fixtureRequests = {fulfilled:0, blocked:0};
              await c.route('**/*', async route => {
                if (route.request().url() !== 'https://chatgpt.com/') {
                  c.__fixtureRequests.blocked++; await route.abort(); return;
                }
                c.__fixtureRequests.fulfilled++;
                await route.fulfill({status:200, contentType:'text/html', body:`<!doctype html>
                  <button id="target">Fixture button</button><button id="replace">Replace fixture</button>`});
              });
              return true;
            }"""
            transport = None
            with patch.object(ext, "_configuration", return_value=configuration):
                with ext.session() as session:
                    transport = session._transport
                    try:
                        transport.call("browser_run_code_unsafe", {"code": setup})
                        browser = session.create(ext.CHAT_URL)
                        self.assertTrue(browser.execute_javascript("function(_element,argument){window.receipt=[];for(const t of ['pointerdown','pointerup','click'])document.addEventListener(t,e=>window.receipt.push({type:t,trusted:e.isTrusted,target:e.target.id}));return true}"))
                        target = browser.find_by_xpath("//*[@id='target']", timeout=2)
                        self.assertTrue(target.is_displayed())
                        read = "function(_element,argument){return window.receipt}"
                        checks = []
                        def before_actual():
                            checks.append(browser.execute_javascript(read))
                            return True
                        with self.assertRaisesRegex(ext.ExtensionBrowserError, "handoff_click_cancelled"):
                            target.click(before_actual=lambda: False)
                        self.assertEqual(browser.execute_javascript(read), [])
                        target.click(before_actual=before_actual)
                        receipt = browser.execute_javascript(read)
                        self.assertEqual(checks, [[]])
                        self.assertEqual([event["type"] for event in receipt], ["pointerdown", "pointerup", "click"])
                        self.assertTrue(all(event["trusted"] and event["target"] == "target" for event in receipt))
                        old = browser.find_by_xpath("//*[@id='replace']", timeout=2)
                        browser.execute_javascript("function(_element,argument){const old=document.getElementById('replace');old.replaceWith(old.cloneNode(true));return true}")
                        with self.assertRaises(ext.ExtensionBrowserError):
                            old.click()
                        self.assertEqual(len(browser.execute_javascript(read)), 3)
                        requests = transport.call("browser_run_code_unsafe", {"code": "async(page)=>({bridge:'webcodex.extension.v1',owner:'fixture',value:page.context().__fixtureRequests})"})
                        self.assertEqual(ext._result(requests, "fixture")["fulfilled"], 1)
                    finally:
                        # Test-only access to the standard close tool. This MCP
                        # owns an isolated browser; the production transport
                        # intentionally never exposes browser_close.
                        closed = Future()
                        transport._loop.call_soon_threadsafe(transport._queue.put_nowait, ("browser_close", {}, closed))
                        result = closed.result(timeout=ext.RPC_TIMEOUT + 5)
                        self.assertFalse(getattr(result, "isError", False))
            self.assertFalse(transport._thread.is_alive())


if __name__ == "__main__":
    unittest.main()
