#!/usr/bin/env python3
"""Opt-in real HTTP MCP proof using disposable Server/Runner processes only.

Requires matching WebCodex binaries and the MyCodex tools virtual environment.
The temporary SQLite OAuth rows are synthetic auth fixtures, not an OAuth flow
test. No existing service, configuration, credential, or user project is opened.
All results use the real HTTP -> Server -> WebSocket Runner -> native Plugin
path. The report contains assertions and bounded diagnostics, never tokens.
"""
from __future__ import annotations

import argparse
import base64
import contextlib
import ctypes
import hashlib
import http.client
import io
import itertools
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid


HOST = "127.0.0.1"
PLUGIN_ID = "mycodex-tools"
SCOPES = "runtime:read project:read project:write job:run plugin:inspect plugin:invoke plugin:manage"
PLUGIN_TOOLS = {
    "docx_read", "docx_create", "docx_update", "xlsx_read", "xlsx_create",
    "xlsx_update", "pptx_read", "pptx_create", "pdf_read", "pdf_search",
    "pdf_create",
}
SLOW_FIXTURE = r'''import json, sys, time
from pathlib import Path
for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": "webcodex-plugin-v1"}
    elif method == "tools/list":
        result = {"tools": [{"name": "slow", "description": "Controlled E2E delay",
                  "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}}]}
    elif method == "tools/call":
        with Path("slow-dispatch.txt").open("a", encoding="utf-8") as target:
            target.write("started\n")
        time.sleep(10)
        result = {"content": [{"type": "text", "text": "finished"}], "isError": False}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
'''


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def content(result):
    """Keep the gateway envelope distinct from the provider's own structure."""
    value = result.get("structuredContent")
    if isinstance(value, dict):
        return value
    for item in result.get("content", []):
        if item.get("type") == "text":
            try:
                value = json.loads(item["text"])
                if isinstance(value, dict):
                    return value
            except (ValueError, TypeError):
                pass
    return result


def output(result):
    value = content(result)
    return value.get("output", value)


def ok(result):
    require(not result.get("isError") and "error" not in result,
            f"Tool failed: {json.dumps(result, ensure_ascii=False)[:2500]}")
    value = content(result)
    require(value.get("success") is not False,
            f"Runtime failed: {json.dumps(value, ensure_ascii=False)[:2500]}")
    return output(result)


def code_and_state(result):
    value = content(result)
    inner = value.get("output", value)
    error = inner.get("error", {})
    code = error.get("code") if isinstance(error, dict) else error
    return (code or inner.get("failure_kind"),
            inner.get("dispatchState", inner.get("dispatch_certainty")))


def free_port():
    with socket.socket() as sock:
        sock.bind((HOST, 0))
        return sock.getsockname()[1]


def wait_until(predicate, timeout, message):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(message)


class WindowsProcessTree:
    """One kill-on-close Job Object contains only this harness's children."""
    def __init__(self):
        from ctypes import wintypes as w

        class Basic(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64),
                        ("flags", w.DWORD), ("min_ws", ctypes.c_size_t),
                        ("max_ws", ctypes.c_size_t), ("active", w.DWORD),
                        ("affinity", ctypes.c_size_t), ("priority", w.DWORD),
                        ("scheduling", w.DWORD)]

        class Io(ctypes.Structure):
            _fields_ = [(name, ctypes.c_uint64) for name in
                        ("read_ops", "write_ops", "other_ops", "read_bytes", "write_bytes", "other_bytes")]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", Io), ("process_memory", ctypes.c_size_t),
                        ("job_memory", ctypes.c_size_t), ("peak_process", ctypes.c_size_t),
                        ("peak_job", ctypes.c_size_t)]

        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.CreateJobObjectW.argtypes = [ctypes.c_void_p, w.LPCWSTR]
        self.api.CreateJobObjectW.restype = w.HANDLE
        self.api.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
        self.api.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
        self.api.CloseHandle.argtypes = [w.HANDLE]
        self.handle = self.api.CreateJobObjectW(None, None)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not self.api.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            self.close()
            raise ctypes.WinError(ctypes.get_last_error())

    def attach(self, process):
        if not self.api.AssignProcessToJobObject(self.handle, int(process._handle)):
            process.kill()
            process.wait(timeout=5)
            raise ctypes.WinError(ctypes.get_last_error())

    def close(self):
        if self.handle:
            self.api.CloseHandle(self.handle)
            self.handle = None


class McpClient:
    def __init__(self, port, token):
        self.port, self.token = port, token
        self.ids = itertools.count(1)
        self.session = None
        self.protocol = "2025-06-18"

    def rpc(self, method, params=None, timeout=75):
        request_id = next(self.ids)
        body = json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method,
                           "params": params or {}}, ensure_ascii=False).encode("utf-8")
        headers = {"Authorization": "Bearer " + self.token,
                   "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": self.protocol, "Connection": "close"}
        if self.session:
            headers["Mcp-Session-Id"] = self.session
        connection = http.client.HTTPConnection(HOST, self.port, timeout=timeout)
        try:
            connection.request("POST", "/mcp", body=body, headers=headers)
            response = connection.getresponse()
            raw = response.read(8 * 1024 * 1024).decode("utf-8")
            self.session = response.getheader("Mcp-Session-Id") or self.session
            if response.getheader("Content-Type", "").startswith("text/event-stream"):
                events = [json.loads(line[5:].strip()) for line in raw.splitlines()
                          if line.startswith("data:") and line[5:].strip()]
                value = next(item for item in events if item.get("id") == request_id)
            else:
                value = json.loads(raw)
            value["_http_status"] = response.status
            return value
        finally:
            connection.close()

    def initialize(self):
        response = self.rpc("initialize", {"protocolVersion": self.protocol,
                            "capabilities": {}, "clientInfo": {"name": "mycodex-e2e", "version": "1"}})
        require(response["_http_status"] == 200 and "result" in response,
                f"MCP initialize failed: {response}")
        self.protocol = response["result"]["protocolVersion"]
        return response["result"]

    def call(self, name, arguments, timeout=75):
        response = self.rpc("tools/call", {"name": name, "arguments": arguments}, timeout)
        if response["_http_status"] != 200 or "result" not in response:
            return response
        return response["result"]

    def plugin(self, action, **arguments):
        return self.call("plugin_tool", {"action": action, **arguments})


class Harness:
    def __init__(self, args):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix="mycodex-mcp-e2e-")).resolve()
        self.temp_parent = Path(tempfile.gettempdir()).resolve()
        self.processes, self.logs, self.credentials = [], [], []
        self.job = WindowsProcessTree() if os.name == "nt" else None
        self.report = {"kind": "real_http_mcp", "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                       "auth_fixture": "synthetic temporary shared-key OAuth rows",
                       "production_services_touched": False, "scenarios": []}
        self.runner_id = "mycodex-e2e-" + secrets.token_hex(5)
        self.project_id = "agent:" + self.runner_id + ":fixture"
        self.port = free_port()
        self.project = self.root / "project"
        self.bindings = {}

    def sanitized(self, value):
        text = json.dumps(value, ensure_ascii=False)
        for credential in self.credentials:
            text = text.replace(credential, "[redacted-test-credential]")
        return json.loads(text)

    def token(self, prefix="wc_oat_"):
        value = prefix + secrets.token_hex(32)
        self.credentials.append(value)
        return value

    def environment(self):
        # Deliberately do not inherit provider/API/production WebCodex credentials.
        allowed = ("PATH", "PATHEXT", "SystemRoot", "WINDIR", "COMSPEC", "SYSTEMDRIVE",
                   "NUMBER_OF_PROCESSORS", "PROCESSOR_ARCHITECTURE")
        env = {key: value for key, value in os.environ.items() if key.upper() in {x.upper() for x in allowed}}
        env.update({"HOME": str(self.root / "home"), "USERPROFILE": str(self.root / "home"),
                    "APPDATA": str(self.root / "appdata"), "LOCALAPPDATA": str(self.root / "localappdata"),
                    "XDG_CONFIG_HOME": str(self.root / "config"), "XDG_STATE_HOME": str(self.root / "state"),
                    "TEMP": str(self.root / "temp"), "TMP": str(self.root / "temp"),
                    "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8", "RUST_LOG": "warn"})
        for name in ("home", "appdata", "localappdata", "config", "state", "temp"):
            (self.root / name).mkdir(exist_ok=True)
        return env

    def spawn(self, executable, arguments, env, label):
        log = (self.root / (label + ".log")).open("wb", buffering=0)
        self.logs.append(log)
        kwargs = {"creationflags": subprocess.CREATE_NO_WINDOW} if os.name == "nt" else {"start_new_session": True}
        process = subprocess.Popen([str(executable), *arguments], cwd=self.root, env=env,
                                   stdin=subprocess.DEVNULL, stdout=log, stderr=log, **kwargs)
        self.processes.append(process)
        if self.job:
            self.job.attach(process)
        return process

    def setup(self):
        for path in (self.args.server, self.args.runner, self.args.python, self.args.plugin, self.args.font):
            require(path.is_file(), f"Required file is missing: {path}")
        self.project.mkdir()
        registry = self.root / "registry"
        registry.mkdir()
        (self.project / "README.md").write_text("# Disposable MyCodex test project\n", encoding="utf-8")
        (self.root / "slow_plugin.py").write_text(SLOW_FIXTURE, encoding="utf-8")
        self.shared_key = self.token("e2e_shared_")
        q = lambda value: json.dumps(str(value), ensure_ascii=False)
        (registry / "fixture.toml").write_text(
            f'id = "fixture"\nname = "Disposable fixture"\npath = {q(self.project)}\nallow_patch = true\n', encoding="utf-8")
        self.runner_config = self.root / "runner.toml"
        self.runner_config.write_text(
            f'server_url = "http://{HOST}:{self.port}"\ntoken = {q(self.shared_key)}\n'
            f'client_id = {q(self.runner_id)}\ndisplay_name = "MyCodex E2E"\nowner = "e2e"\n'
            f'project_registry_dir = {q(registry)}\ntransport = "websocket"\npoll_interval_ms = 100\n'
            f'[policy]\nallowed_roots = [{q(self.project)}]\nmax_timeout_secs = 90\nmax_output_bytes = 262144\n'
            '[shell.env]\nPYTHONUTF8 = "1"\nPYTHONIOENCODING = "utf-8"\n'
            '[plugins]\nrequest_timeout_secs = 60\n'
            f'[[plugins.providers]]\nid = "{PLUGIN_ID}"\nname = "MyCodex Tools"\n'
            f'command = {q(self.args.python)}\nargs = [{q(self.args.plugin)}, "--font-path", {q(self.args.font)}]\n'
            f'cwd = {q(self.project)}\ntimeout_secs = 60\n'
            '[[plugins.providers]]\nid = "slow-fixture"\nname = "Controlled E2E timeout"\n'
            f'command = {q(self.args.python)}\nargs = [{q(self.root / "slow_plugin.py")}]\n'
            f'cwd = {q(self.project)}\ntimeout_secs = 3\n', encoding="utf-8")
        env = self.environment()
        server_env = {**env, "WEBCODEX_ADDR": f"{HOST}:{self.port}",
                      "WEBCODEX_DATA": str(self.root / "server-data"), "WEBCODEX_TOKEN": self.token("e2e_bootstrap_"),
                      "WEBCODEX_SHARED_KEY_ENABLED": "true",
                      "WEBCODEX_OAUTH2_ENABLED": "true", "WEBCODEX_OAUTH2_ISSUER": f"http://{HOST}:{self.port}",
                      "WEBCODEX_MCP_MODEL_SURFACE": "full-operator-v1"}
        self.server = self.spawn(self.args.server, [], server_env, "server")

        def ready():
            require(self.server.poll() is None, "Temporary Server exited before readiness")
            connection = http.client.HTTPConnection(HOST, self.port, timeout=1)
            try:
                connection.request("GET", "/openapi.json")
                response = connection.getresponse()
                response.read()
                return response.status < 500
            except OSError:
                return False
            finally:
                connection.close()

        wait_until(ready, 30, "Temporary Server did not become ready")
        self.clients = self.seed_auth()
        self.client = self.clients["all"]
        self.report["server_info"] = self.client.initialize().get("serverInfo", {})
        self.runner = self.spawn(self.args.runner, ["--config", str(self.runner_config)], env, "runner")

        last_runner_observation = {}

        def runner_ready():
            require(self.runner.poll() is None, "Temporary Runner exited before readiness")
            result = self.client.plugin("list")
            last_runner_observation.clear()
            last_runner_observation.update(result)
            return not result.get("isError") and self.runner_id in json.dumps(content(result))

        try:
            wait_until(runner_ready, 45, "Temporary Runner did not register Plugin capability")
        except AssertionError as error:
            raise AssertionError(f"{error}; last response: {last_runner_observation}") from error

    def seed_auth(self):
        database = self.root / "server-data" / "webcodex.db"
        require(database.resolve().is_relative_to(self.root), "Auth fixture escaped temporary root")
        shared_hash = hashlib.sha256(self.shared_key.encode()).hexdigest()
        client_id = "wc_client_" + secrets.token_hex(16)
        now = int(time.time())
        clients = {}
        with contextlib.closing(sqlite3.connect(database, timeout=10)) as db:
            db.execute("PRAGMA foreign_keys=ON")
            db.execute("INSERT INTO oauth_clients (id,client_id,client_secret_hash,name,owner_shared_key_hash,redirect_uris,allowed_scopes,created_at) VALUES (?,?,?,?,?,?,?,?)",
                       (uuid.uuid4().hex, client_id, hashlib.sha256(self.token().encode()).hexdigest(),
                        "Synthetic local E2E", shared_hash, "http://127.0.0.1/callback", SCOPES, now))
            for name, scopes in {"all": SCOPES, "inspect": "runtime:read plugin:inspect",
                                 "invoke": "runtime:read plugin:invoke", "manage": "runtime:read plugin:manage"}.items():
                token = self.token()
                db.execute("INSERT INTO oauth_access_tokens (id,token_hash,client_id,subject_kind,subject_id,scopes,shared_key_hash,created_at,expires_at) VALUES (?,?,?,?,?,?,?,?,?)",
                           (uuid.uuid4().hex, hashlib.sha256(token.encode()).hexdigest(), client_id,
                            "shared_key", shared_hash, scopes, shared_hash, now, now + 3600))
                clients[name] = McpClient(self.port, token)
            db.commit()
        clients["baseline"] = McpClient(self.port, self.shared_key)
        return clients

    def scenario(self, name, function):
        started = time.monotonic()
        try:
            facts = function() or {}
            record = {"name": name, "passed": True, "facts": facts}
        except Exception as error:
            record = {"name": name, "passed": False, "error": str(error)}
        record["seconds"] = round(time.monotonic() - started, 3)
        self.report["scenarios"].append(self.sanitized(record))
        print(f"[{ 'PASS' if record['passed'] else 'FAIL' }] {name}", flush=True)

    def describe(self, name, plugin=PLUGIN_ID, client=None):
        value = ok((client or self.client).plugin("describe", runner=self.runner_id, plugin=plugin, tool=name))
        require(str(value.get("binding", "")).startswith("wc_pbind_"), "Describe omitted opaque binding")
        return value["binding"]

    def invoke(self, name, arguments):
        if name not in self.bindings:
            self.bindings[name] = self.describe(name)
        return ok(self.client.plugin("call", binding=self.bindings[name], arguments=arguments))

    def builtin(self, name, **arguments):
        return ok(self.client.call(name, arguments))

    def discovery(self):
        response = self.client.rpc("tools/list")
        require(response["_http_status"] == 200, "MCP tools/list failed")
        names = {item["name"] for item in response["result"]["tools"]}
        require("plugin_tool" in names and not names.intersection(PLUGIN_TOOLS),
                "Provider names leaked into outer MCP tools/list or gateway is missing")
        providers = ok(self.client.plugin("list", runner=self.runner_id))
        require(PLUGIN_ID in json.dumps(providers), "MyCodex provider absent")
        catalog = ok(self.client.plugin("list", runner=self.runner_id, plugin=PLUGIN_ID))
        tools = {item["name"] for item in catalog["tools"]}
        require(PLUGIN_TOOLS <= tools, f"Missing native tools: {PLUGIN_TOOLS - tools}")
        check = ok(self.client.plugin("check", runner=self.runner_id, plugin=PLUGIN_ID))
        require(check.get("ready") is True, f"Disposable Plugin check failed: {check}")
        return {"outer_tool_count": len(names), "provider_tools": sorted(tools), "check_ready": True}

    def text_and_process(self):
        self.builtin("write_project_file", project=self.project_id, path="hello.txt", content="hello webcodex")
        hello = self.builtin("read_file", project=self.project_id, path="hello.txt", with_line_numbers=False)
        require(hello.get("text") == "hello webcodex", f"Native hello.txt content mismatch: {hello}")
        require(hello.get("sha256") == hashlib.sha256(b"hello webcodex").hexdigest(), "hello.txt hash mismatch")
        text = "中文 UTF-8\nsecond line\n"
        self.builtin("write_project_file", project=self.project_id, path="原生文本.txt", content=text)
        read = self.builtin("read_file", project=self.project_id, path="原生文本.txt", with_line_numbers=False)
        # read_file is line-oriented and does not retain the final line terminator.
        require(read.get("text") == "\n".join(text.splitlines()), f"UTF-8 native text mismatch: {read}")
        require(read.get("sha256") == hashlib.sha256(text.encode("utf-8")).hexdigest(), "UTF-8 file hash mismatch")
        self.builtin("write_project_file", project=self.project_id, path="hello.py",
                     content="import sys, time\nprint('hello webcodex', flush=True)\nif '--hold' in sys.argv:\n    time.sleep(3)\n")
        raw = self.client.call("run_process", {"project": self.project_id, "executable": str(self.args.python),
                                               "args": ["hello.py"], "timeout_secs": 10})
        result = ok(raw)
        require(content(raw).get("success") is True, "hello.py did not produce a terminal success result")
        # The native synchronous-success projection omits exit_code=0; the Job
        # scenario below observes the same file's explicit terminal exit code.
        require(result.get("stdout_tail", "").replace("\r\n", "\n") == "hello webcodex\n",
                f"hello.py stdout mismatch: {result}")
        return {"hello_txt_exact_content": "hello webcodex", "utf8_roundtrip": True,
                "python_file_executed": "hello.py", "stdout": "hello webcodex", "terminal_success": True}

    def powershell_script(self):
        result = self.builtin("run_script", project=self.project_id, language="powershell",
                              script="Write-Output 'hello powershell'", timeout_secs=10)
        require(result.get("stdout_tail", "").replace("\r\n", "\n") == "hello powershell\n",
                f"PowerShell stdout mismatch: {result}")
        return {"language": "powershell", "stdout": "hello powershell"}

    def job_continuation(self):
        initial = self.builtin("run_process", project=self.project_id, executable=str(self.args.python),
                               args=["hello.py", "--hold"], timeout_secs=15, sync_wait_secs=1)
        job_id = initial.get("job_id")
        require(isinstance(job_id, str) and job_id, f"Structured process did not hand off a Job: {initial}")
        seen = initial.get("stdout_tail", "")
        token = initial.get("observation_token")
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            args = {"job_id": job_id, "tail_lines": 20}
            if token:
                args.update(after_observation_token=token, wait_secs=3)
            log = self.builtin("job_log", **args)
            seen += log.get("stdout_tail", "")
            token = log.get("observation_token")
            status = self.builtin("job_status", job_id=job_id)
            if status.get("status") == "completed":
                require(status.get("job_id") == job_id and status.get("exit_code") == 0,
                        f"Wrong terminal Job identity/exit code: {status}")
                require("hello webcodex\n" in seen.replace("\r\n", "\n"), "Terminal Job output omitted stdout")
                return {"same_job_id": True, "python_file_executed": "hello.py", "terminal": "completed",
                        "stdout": "hello webcodex", "exit_code": 0}
            time.sleep(0.1)
        raise AssertionError("Existing Job did not finish within deadline")

    def native_image(self):
        script = ("from PIL import Image, ImageDraw; import os; "
                  "im=Image.new('RGB',(200,200),'white'); d=ImageDraw.Draw(im); "
                  "d.rectangle((25,25,175,175),fill='navy'); im.save('geometry.png'); "
                  "Image.frombytes('RGB',(800,800),os.urandom(800*800*3)).save('large.png'); "
                  "print('images-created')")
        self.builtin("run_process", project=self.project_id, executable=str(self.args.python),
                     args=["-c", script], timeout_secs=10)
        result = self.client.call("read_project_artifact", {"project": self.project_id,
                                  "path": "geometry.png", "as_image": True})
        value = ok(result)
        images = [item for item in result.get("content", []) if item.get("type") == "image"]
        require(len(images) == 1 and images[0]["mimeType"] == "image/png", "Expected native MCP PNG image block")
        from PIL import Image
        with Image.open(io.BytesIO(base64.b64decode(images[0]["data"], validate=True))) as image:
            require(image.size == (200, 200), "MCP image has wrong dimensions")
        require("content_base64" not in json.dumps(value), "MCP structured content duplicates base64")
        large_size = (self.project / "large.png").stat().st_size
        require(large_size > 1024 * 1024, "Large-image fixture is too small")
        rejected = self.client.call("read_project_artifact", {"project": self.project_id,
                                    "path": "large.png", "as_image": True})
        require(rejected.get("isError") is True, "Image above 1 MiB was not rejected")
        require("MCP image too large; maximum is 1048576 bytes" in json.dumps(rejected),
                f"Oversized image failed for the wrong reason: {rejected}")
        info = self.builtin("read_project_artifact_metadata", project=self.project_id, path="geometry.png")
        require(info.get("width") == 200 and info.get("height") == 200, f"Wrong native image metadata: {info}")
        return {"dimensions": [200, 200], "native_mcp_image": True, "oversize_bytes": large_size,
                "oversize_rejected": True}

    def office(self):
        self.invoke("docx_create", {"output_path": "中文.docx", "blocks": [
            {"type": "heading", "text": "中文标题", "level": 1},
            {"type": "paragraph", "text": "WebCodex document test"},
            {"type": "table", "rows": [["A", "B"], ["1", "2"]]}]})
        source = self.invoke("docx_read", {"path": "中文.docx"})
        require(source["paragraphs"][1]["text"] == "WebCodex document test", "DOCX original body mismatch")
        source_hash = hashlib.sha256((self.project / "中文.docx").read_bytes()).hexdigest()
        self.invoke("docx_update", {"path": "中文.docx", "expected_sha256": source["sha256"],
            "output_path": "中文副本.docx", "paragraph_updates": [{"index": 1, "text": "更新正文"}],
            "cell_updates": [{"table": 0, "row": 1, "column": 1, "text": "更新表格"}]})
        changed = self.invoke("docx_read", {"path": "中文副本.docx"})
        encoded = json.dumps(changed, ensure_ascii=False)
        require("更新正文" in encoded and "更新表格" in encoded, "DOCX changed content missing")
        require(hashlib.sha256((self.project / "中文.docx").read_bytes()).hexdigest() == source_hash,
                "DOCX update changed source")
        self.invoke("xlsx_create", {"output_path": "数据.xlsx", "sheets": [
            {"name": "数据", "cells": [{"coordinate": "A1", "type": "text", "text": "中文"},
             {"coordinate": "B1", "type": "number", "number": 42},
             {"coordinate": "C1", "type": "formula", "formula": "=B1*2"}]},
            {"name": "备注", "cells": []}]})
        source = self.invoke("xlsx_read", {"path": "数据.xlsx", "sheet": "数据", "range": "A1:C2"})
        xlsx_hash = hashlib.sha256((self.project / "数据.xlsx").read_bytes()).hexdigest()
        require("=B1*2" in json.dumps(source), "XLSX formula string missing")
        self.invoke("xlsx_update", {"path": "数据.xlsx", "expected_sha256": source["sha256"],
            "output_path": "数据副本.xlsx", "updates": [{"sheet": "数据", "cells": [
                {"coordinate": "B1", "type": "number", "number": 50}]}]})
        changed = self.invoke("xlsx_read", {"path": "数据副本.xlsx", "sheet": "数据", "range": "A1:C2"})
        require(any(cell.get("coordinate") == "B1" and cell.get("number") == 50
                    for row in changed["rows"] for cell in row["cells"]), "XLSX updated number missing")
        require(hashlib.sha256((self.project / "数据.xlsx").read_bytes()).hexdigest() == xlsx_hash,
                "XLSX update changed source")
        self.invoke("pptx_create", {"output_path": "汇报.pptx", "slides": [
            {"title": "中文标题", "body": "正文", "bullets": ["条目一", "条目二"]},
            {"title": "第二页", "body": "阶段结论", "bullets": []}]})
        pptx = self.invoke("pptx_read", {"path": "汇报.pptx"})
        require(pptx["slide_count"] == 2 and "阶段结论" in json.dumps(pptx, ensure_ascii=False),
                "PPTX page/text roundtrip failed")
        return {"docx_create_read_copy_update": True, "xlsx_create_read_copy_update": True,
                "source_hashes_unchanged": True, "pptx_slides": 2}

    def pdf(self):
        self.invoke("pdf_create", {"output_path": "中文报告.pdf", "title": "中文报告",
                    "pages": [{"blocks": [{"type": "heading", "text": "验收标题"},
                               {"type": "paragraph", "text": "中文检索目标：工具闭环成功。"}]},
                              {"blocks": [{"type": "paragraph", "text": "第二页内容"}]}]})
        read = self.invoke("pdf_read", {"path": "中文报告.pdf"})
        require(read.get("page_count_total") == 2, "PDF read did not report two pages")
        require("工具闭环成功" in json.dumps(read, ensure_ascii=False), "Chinese PDF extraction failed")
        found = self.invoke("pdf_search", {"path": "中文报告.pdf", "query": "工具闭环成功"})
        require(found.get("match_count") == 1 and found["matches"][0]["text"] == "工具闭环成功",
                f"Chinese PDF search failed: {found}")
        from pypdf import PdfReader
        pdf = PdfReader(self.project / "中文报告.pdf")
        require(len(pdf.pages) == 2, "PDF artifact does not contain two pages")
        fonts = pdf.pages[0]["/Resources"]["/Font"].get_object().values()
        require(any("/FontFile2" in font.get_object().get("/FontDescriptor", {}) for font in fonts),
                "Generated PDF does not embed the configured TrueType font")
        return {"page_count": 2, "chinese_font_embedded": True, "text_roundtrip": True, "search": True}

    def scope_matrix(self):
        binding = self.describe("pdf_read", client=self.clients["inspect"])
        valid_call = {"binding": binding, "arguments": {"path": "中文报告.pdf"}}
        ok(self.clients["inspect"].plugin("list", runner=self.runner_id, plugin=PLUGIN_ID))
        ok(self.clients["invoke"].plugin("call", **valid_call))
        check = ok(self.clients["manage"].plugin("check", runner=self.runner_id, plugin=PLUGIN_ID))
        require(check.get("ready") is True, "Manage-only scope could not check provider")
        denied = [
            ("inspect", "call", valid_call),
            ("inspect", "check", {"runner": self.runner_id, "plugin": PLUGIN_ID}),
            ("inspect", "reload", {"runner": self.runner_id}),
            ("invoke", "list", {"runner": self.runner_id}),
            ("invoke", "reload", {"runner": self.runner_id}),
            ("manage", "call", valid_call),
            ("baseline", "list", {"runner": self.runner_id}),
        ]
        for scope, action, arguments in denied:
            result = self.clients[scope].plugin(action, **arguments)
            require(result.get("_http_status") == 403, f"{scope} unexpectedly admitted {action}: {result}")
            if scope == "baseline":
                require("plugin:inspect" in result.get("error", ""), f"Wrong shared-key denial: {result}")
            else:
                require(result.get("error") == "insufficient_scope", f"Wrong OAuth denial: {result}")
        return {"allowed": 3, "denied": len(denied), "scopes_independent": True}

    def stale_binding(self):
        old_binding = self.describe("pdf_read")
        ok(self.clients["manage"].plugin("reload", runner=self.runner_id))
        stale = self.client.plugin("call", binding=old_binding, arguments={"path": "中文报告.pdf"})
        code, state = code_and_state(stale)
        require(stale.get("isError") and state == "not_started" and code == "plugin_replaced",
                f"Old binding did not fail closed: {stale}")
        self.bindings.clear()
        self.invoke("pdf_read", {"path": "中文报告.pdf"})
        return {"stale_code": code, "dispatch_state": state, "fresh_binding_works": True}

    def busy_and_unknown(self):
        binding = self.describe("slow", plugin="slow-fixture")
        first = {}

        def dispatch():
            try:
                first["result"] = self.client.plugin("call", binding=binding, arguments={})
            except Exception as error:
                first["error"] = str(error)

        thread = threading.Thread(target=dispatch, name="controlled-plugin-timeout", daemon=True)
        thread.start()
        marker = self.project / "slow-dispatch.txt"
        wait_until(marker.exists, 5, "Slow fixture was not dispatched")
        busy = self.client.plugin("call", binding=binding, arguments={})
        code, state = code_and_state(busy)
        require(busy.get("isError") and code == "plugin_provider_busy" and state == "not_started",
                f"Concurrent call not rejected as busy: {busy}")
        thread.join(timeout=10)
        require(not thread.is_alive() and "result" in first, f"Timeout response absent: {first}")
        timeout_code, timeout_state = code_and_state(first["result"])
        require(first["result"].get("isError") and timeout_state == "outcome_unknown",
                f"Started timeout did not retain uncertainty: {first}")
        require(marker.read_text(encoding="utf-8").splitlines() == ["started"],
                "Busy/timeout path replayed provider call")
        return {"busy_code": code, "timeout_code": timeout_code, "timeout_state": timeout_state,
                "provider_dispatch_count": 1}

    def close(self):
        if self.job:
            self.job.close()
        else:
            import signal
            for process in reversed(self.processes):
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
        for process in reversed(self.processes):
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                if os.name != "nt":
                    import signal
                    os.killpg(process.pid, signal.SIGKILL)
                else:
                    process.kill()
                process.wait(timeout=5)
        for log in self.logs:
            log.close()
        require(self.root.parent == self.temp_parent and self.root.name.startswith("mycodex-mcp-e2e-")
                and self.root.resolve() == self.root, "Refused cleanup outside owned temporary root")
        if any(not item["passed"] for item in self.report["scenarios"]):
            self.report["log_tails"] = self.sanitized({path.name: path.read_text(encoding="utf-8", errors="replace")[-4000:]
                                                        for path in self.root.glob("*.log")})
        shutil.rmtree(self.root)
        self.report["temporary_processes_stopped"] = all(process.poll() is not None for process in self.processes)
        self.report["temporary_root_removed"] = not self.root.exists()


def main():
    root = Path(__file__).resolve().parents[3]
    local = root.parent / ".local"
    binaries = local / "npm/node_modules/@yyjeqhc/webcodex/vendor/bin"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=binaries / "webcodex-server.exe")
    parser.add_argument("--runner", type=Path, default=binaries / "webcodex-runner.exe")
    parser.add_argument("--python", type=Path, default=local / "venvs/mycodex-tools/Scripts/python.exe")
    parser.add_argument("--plugin", type=Path, default=root / "plugins/mycodex-tools/plugin.py")
    parser.add_argument("--font", type=Path, default=Path("C:/Windows/Fonts/simhei.ttf"))
    parser.add_argument("--report", type=Path, default=local / "reports/mycodex-tools-e2e.json")
    args = parser.parse_args()
    for name in ("server", "runner", "python", "plugin", "font", "report"):
        setattr(args, name, getattr(args, name).resolve())
    harness = Harness(args)
    try:
        harness.setup()
        for name, function in [
            ("mcp_initialize_discovery_check", harness.discovery),
            ("native_utf8_text_and_python", harness.text_and_process),
            ("native_powershell_script", harness.powershell_script),
            ("native_job_continuation", harness.job_continuation),
            ("native_image_and_size_limit", harness.native_image),
            ("office_content_roundtrip", harness.office),
            ("chinese_pdf_roundtrip", harness.pdf),
            ("plugin_scope_matrix", harness.scope_matrix),
            ("reload_stale_binding", harness.stale_binding),
            ("provider_busy_and_outcome_unknown", harness.busy_and_unknown),
        ]:
            harness.scenario(name, function)
    except Exception as error:
        harness.report["scenarios"].append(harness.sanitized({"name": "setup", "passed": False, "error": str(error)}))
    finally:
        try:
            harness.close()
        except Exception as error:
            harness.report["scenarios"].append(harness.sanitized({"name": "cleanup", "passed": False, "error": str(error)}))
        harness.report["passed"] = all(item["passed"] for item in harness.report["scenarios"])
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(harness.sanitized(harness.report), ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"{'PASS' if harness.report['passed'] else 'FAIL'}: {len(harness.report['scenarios'])} scenarios; report: {args.report}")
    return 0 if harness.report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
