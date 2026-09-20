"""WebCodex native plugin entry point (stdio NDJSON, no MCP SDK)."""
from __future__ import annotations

import argparse
import contextlib
import json
from pathlib import Path
import sys

from mycodex_tools.common import (
    Context, ToolError, MAX_FRAME_BYTES, MAX_ARGUMENT_BYTES, MAX_RESULT_BYTES,
    bounded_json, encode, validate_args,
)
from mycodex_tools import office, pdf

PROTOCOL_VERSION = "webcodex-plugin-v1"
TOOL_SPECS = office.TOOL_SPECS + pdf.TOOL_SPECS
HANDLERS = {**office.HANDLERS, **pdf.HANDLERS}
SPECS = {item["name"]: item for item in TOOL_SPECS}

# Tool annotations describe effects, never replace gateway authorization.
for spec in TOOL_SPECS:
    read_only = spec["name"].endswith(("_read", "_search"))
    spec["annotations"] = {
        "readOnlyHint": read_only, "destructiveHint": False,
        "idempotentHint": read_only, "openWorldHint": False,
    }


def call_tool(ctx, name, arguments):
    try:
        if name not in SPECS:
            raise ToolError("unknown_tool", "Tool is not in this provider's catalog.")
        bounded_json(arguments, MAX_ARGUMENT_BYTES, "arguments_too_large")
        validate_args(arguments, SPECS[name]["inputSchema"])
        # Libraries must never corrupt protocol stdout with progress or diagnostics.
        with contextlib.redirect_stdout(sys.stderr):
            value = HANDLERS[name](ctx, arguments)
        if not isinstance(value, dict):
            raise ToolError("invalid_result", "Tool result must be an object.")
        bounded_json(value, MAX_RESULT_BYTES)
        return {"content": [{"type": "text", "text": f"{name} completed; see structuredContent."}],
                "structuredContent": value, "isError": False}
    except ToolError as error:
        return {"content": [{"type": "text", "text": error.message}],
                "structuredContent": {"error": {"code": error.code, "message": error.message}},
                "isError": True}
    except Exception as error:
        # Never include raw library exceptions (document text, absolute paths) in model results.
        print(f"{name}: {type(error).__name__}", file=sys.stderr)
        message = "Operation failed. Check the input format and local file permissions."
        return {"content": [{"type": "text", "text": message}],
                "structuredContent": {"error": {"code": "operation_failed", "message": message}},
                "isError": True}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field")
        result[key] = value
    return result


def serve(ctx, source, destination):
    initialized = False

    def send(message):
        frame = encode(message)
        if len(frame) > MAX_FRAME_BYTES:
            raise RuntimeError("response frame exceeds native ABI bounds")
        destination.write(frame + b"\n")
        destination.flush()

    def rpc_error(request_id, code, message):
        send({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})

    while True:
        line = source.readline(MAX_FRAME_BYTES + 1)
        if not line:
            return
        if len(line) > MAX_FRAME_BYTES:
            rpc_error(None, -32600, "request frame exceeds bounds")
            return  # Do not interpret fragments of an overlong frame as another request.
        try:
            request = json.loads(line.decode("utf-8"), object_pairs_hook=unique_object,
                                 parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
            if not isinstance(request, dict):
                raise ValueError()
        except (ValueError, UnicodeError, RecursionError):
            rpc_error(None, -32700, "invalid JSON")
            continue
        request_id = request.get("id")
        if (request.get("jsonrpc") != "2.0" or type(request_id) not in (str, int)
                or not isinstance(request.get("params", {}), dict)):
            rpc_error(None, -32600, "invalid request")
            continue
        method = request.get("method")
        params = request.get("params", {})
        if method == "initialize":
            if params.get("protocolVersion") != PROTOCOL_VERSION:
                rpc_error(request_id, -32602, "unsupported protocol version")
                continue
            initialized = True
            result = {"protocolVersion": PROTOCOL_VERSION}
        elif not initialized:
            rpc_error(request_id, -32000, "initialize first")
            continue
        elif method == "tools/list":
            result = {"tools": TOOL_SPECS}
        elif method == "tools/call":
            if not isinstance(params.get("name"), str):
                rpc_error(request_id, -32602, "tool name is required")
                continue
            result = call_tool(ctx, params["name"], params.get("arguments", {}))
        else:
            rpc_error(request_id, -32601, "method not found")
            continue
        send({"jsonrpc": "2.0", "id": request_id, "result": result})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--font-path", type=Path, help="Operator-configured local Chinese TTF font")
    args = parser.parse_args()
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8")
    serve(Context(Path.cwd(), args.font_path), sys.stdin.buffer, sys.stdout.buffer)


if __name__ == "__main__":
    main()
