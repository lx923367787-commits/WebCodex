import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import plugin
from mycodex_tools.common import Context, MAX_FRAME_BYTES, encode


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.ctx = Context(Path(self.temp.name))

    def run_frames(self, frames):
        source = io.BytesIO(b"\n".join(encode(frame) for frame in frames) + b"\n")
        output = io.BytesIO()
        plugin.serve(self.ctx, source, output)
        return [json.loads(line) for line in output.getvalue().splitlines()]

    def test_handshake_and_catalog_have_exactly_eleven_tools(self):
        replies = self.run_frames([
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": plugin.PROTOCOL_VERSION}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        ])
        self.assertEqual(replies[0]["result"]["protocolVersion"], plugin.PROTOCOL_VERSION)
        names = [item["name"] for item in replies[1]["result"]["tools"]]
        self.assertEqual(len(names), 11)
        self.assertEqual(set(names), set(plugin.HANDLERS))
        self.assertLess(len(encode(replies[1])), MAX_FRAME_BYTES)

    def test_calls_require_handshake_and_valid_arguments(self):
        replies = self.run_frames([{"jsonrpc": "2.0", "id": 1, "method": "tools/list"}])
        self.assertIn("error", replies[0])
        result = plugin.call_tool(self.ctx, "docx_read", {"path": "x.docx", "run_command": "bad"})
        self.assertTrue(result["isError"])
        self.assertEqual(result["structuredContent"]["error"]["code"], "invalid_arguments")

    def test_catalog_schema_uses_only_supported_keywords(self):
        allowed = {"type", "title", "description", "enum", "const", "properties", "required",
                   "additionalProperties", "minLength", "maxLength", "minItems", "maxItems", "items"}
        def visit(schema):
            self.assertTrue(schema.keys() <= allowed, schema.keys() - allowed)
            self.assertIsInstance(schema["type"], str)
            for child in schema.get("properties", {}).values():
                visit(child)
            if "items" in schema:
                visit(schema["items"])
        for spec in plugin.TOOL_SPECS:
            visit(spec["inputSchema"])

    def test_duplicate_keys_and_oversized_frames_fail_closed(self):
        output = io.BytesIO()
        plugin.serve(self.ctx, io.BytesIO(b'{"id":1,"id":2}\n'), output)
        self.assertEqual(json.loads(output.getvalue())["error"]["code"], -32700)
        output = io.BytesIO()
        plugin.serve(self.ctx, io.BytesIO(b"x" * (MAX_FRAME_BYTES + 1)), output)
        self.assertEqual(json.loads(output.getvalue())["error"]["code"], -32600)

    def test_real_subprocess_is_utf8_protocol_only(self):
        request = encode({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                          "params": {"protocolVersion": plugin.PROTOCOL_VERSION}}) + b"\n"
        proc = subprocess.run([sys.executable, str(Path(plugin.__file__))], input=request,
                              capture_output=True, cwd=self.ctx.root, timeout=15,
                              creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        self.assertEqual(json.loads(proc.stdout)["result"]["protocolVersion"], plugin.PROTOCOL_VERSION)


if __name__ == "__main__":
    unittest.main()
