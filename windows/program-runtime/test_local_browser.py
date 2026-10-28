"""Synthetic framework pages only; never an application acceptance test."""
import json
import os
import subprocess
import sys
from pathlib import Path
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import local_browser


class ValidationTests(unittest.TestCase):
    def test_carrier_rejects_extra_fields_without_executing_code(self):
        result = subprocess.run([sys.executable, "-I", str(Path(local_browser.__file__).resolve())],
            input=json.dumps({"script":"raise SystemExit(0)"})+"\n", text=True,
            capture_output=True, timeout=5, encoding="utf-8")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stdout), {"success":False,"error":"browser_carrier_fields_invalid"})

    def test_exact_origin_and_structured_actions(self):
        self.assertEqual(local_browser.local_origin("http://127.0.0.1:8080"), "http://127.0.0.1:8080")
        for origin in ("https://127.0.0.1:8080", "http://localhost:8080", "http://127.0.0.1:8080/",
                       "http://user@127.0.0.1:8080", "http://127.0.0.1:80"):
            with self.subTest(origin=origin), self.assertRaises(ValueError):
                local_browser.local_origin(origin)
        for step in ({"action": "evaluate", "code": "1"}, {"action": "navigate", "path": "//other"},
                     {"action": "click", "selector": "body >> xpath=//*"},
                     {"action": "screenshot", "path": "outside.png"},
                     {"action": "fill", "selector": "#name", "text": "x", "script": "extra"}):
            with self.subTest(step=step), self.assertRaises(ValueError):
                local_browser.validate_steps([step])
        self.assertFalse(local_browser.allowed_url("file:///C:/private", "http://127.0.0.1:8080"))
        self.assertFalse(local_browser.allowed_url("http://127.0.0.1:8081/", "http://127.0.0.1:8080"))


@unittest.skipUnless(os.environ.get("WEBCODEX_LOCAL_BROWSER_TEST"), "explicit real framework browser lane")
class NativeTests(unittest.TestCase):
    def test_real_fill_click_select_screenshot_and_cross_origin_block(self):
        external_hits = []

        class External(BaseHTTPRequestHandler):
            def do_GET(self):
                external_hits.append(self.path)
                self.send_response(200)
                self.end_headers()

            def log_message(self, *args):
                pass

        external = ThreadingHTTPServer(("127.0.0.1", 0), External)
        thread = threading.Thread(target=external.serve_forever, daemon=True)
        thread.start()

        class Page(BaseHTTPRequestHandler):
            def do_GET(self):
                html = ("<title>Framework Browser Fixture</title><input id='name'>"
                        "<select id='role'><option value='a'>A</option><option value='b'>B</option></select>"
                        "<button id='send' onclick=\"setTimeout(()=>{document.querySelector('#result').textContent="
                        "'Hello '+document.querySelector('#name').value+':'+document.querySelector('#role').value},200)\">Send</button>"
                        "<p id='result'></p><button id='confirm' onclick=\"if(confirm('Confirm fixture?'))document.querySelector('#result').textContent='Confirmed'\">Confirm</button>"
                        f"<a id='external' href='http://127.0.0.1:{external.server_port}/denied'>Outside</a>")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.end_headers()
                self.wfile.write(html.encode())

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Page)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            base = Path(os.environ["WEBCODEX_LOCAL_BROWSER_TEST"])
            with tempfile.TemporaryDirectory(dir=base, prefix="local-browser-") as folder:
                root = Path(folder)
                binary = Path(os.environ["WEBCODEX_LOCAL_BROWSER_BINARY"])
                origin = f"http://127.0.0.1:{server.server_port}"
                result = local_browser.run(origin=origin, browser_executable=binary,
                    artifact_directory=root / "success", steps=[
                        {"action": "navigate", "path": "/"},
                        {"action": "fill", "selector": "#name", "text": "fixture"},
                        {"action": "select", "selector": "#role", "value": "b"},
                        {"action": "click", "selector": "#send"},
                        {"action": "expect_text", "selector": "#result", "text": "Hello fixture:b"},
                        {"action": "screenshot"}])
                self.assertEqual(result["steps"][4]["text"], "Hello fixture:b")
                self.assertGreater((root / "success/step-05.png").stat().st_size, 1000)
                self.assertFalse(result["business_verified"])
                confirmed = local_browser.run(origin=origin, browser_executable=binary,
                    artifact_directory=root / "confirmed", steps=[
                        {"action":"navigate","path":"/"},
                        {"action":"click","selector":"#confirm","accept_dialog":True},
                        {"action":"expect_text","selector":"#result","text":"Confirmed"}])
                self.assertEqual(confirmed["dialogs"][0]["message"], "Confirm fixture?")
                self.assertTrue(confirmed["dialogs"][0]["accepted"])
                with self.assertRaises(Exception):
                    local_browser.run(origin=origin, browser_executable=binary,
                        artifact_directory=root / "blocked", steps=[
                            {"action": "navigate", "path": "/"},
                            {"action": "click", "selector": "#external"}])
                self.assertEqual(external_hits, [])
                failure=json.loads((root / "blocked/failure.json").read_text(encoding="utf-8"))
                self.assertFalse(failure["completed"])
                self.assertIn("request_outside_local_origin", failure["scope_violations"])
                print(json.dumps({"framework_browser_fixture": True, "engine_version": result["engine_version"],
                                  "interaction": result["steps"][4]["text"], "external_requests_received": len(external_hits)}))
        finally:
            server.shutdown()
            external.shutdown()
            server.server_close()
            external.server_close()


if __name__ == "__main__":
    unittest.main()
