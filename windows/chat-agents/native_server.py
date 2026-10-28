"""Small WebCodex native JSONL gateway for eight bounded chat-task operations.

Discovery and validation do not import a browser, perform network I/O, or touch
runtime files. Only the injected service may perform an authorized operation.
"""
from __future__ import annotations

import contextlib
import json
import math
import re
import sys
import uuid


PROTOCOL_VERSION = "webcodex-plugin-v1"
LIFECYCLE_METHOD = "webcodex/chat-agents/lifecycle"
LIFECYCLE_PROTOCOL_VERSION = "webcodex-chat-agents-lifecycle-v1"
LIFECYCLE_PROVIDER_ID = "chat-agents"
MAX_FRAME = 65536
REQUEST_ID_SCHEMA = {
    "type": "string", "minLength": 36, "maxLength": 36,
    "description": "Canonical lowercase hyphenated UUID returned by spawn_chat_task; enforced at runtime.",
}
SPAWN_PROPERTIES = {
    "task": {"type": "string", "minLength": 1, "maxLength": 12000,
             "description": "Nonblank child task within the user's authorized scope."},
    "parent_task_id": {"type": "string", "minLength": 1, "maxLength": 128,
                       "description": "Caller-chosen correlation label using only ASCII letters, digits, underscore, dot, colon or hyphen; grants no permissions. Character rule enforced at runtime."},
    "idempotency_key": {"type": "string", "minLength": 1, "maxLength": 128,
                        "description": "Nonblank stable key for this exact task; reuse on retries."},
    "thinking_profile_id": {"type": "string", "minLength": 1, "maxLength": 128,
                            "description": "Optional page adjustment. Omit to leave settings untouched without reading the catalog or current setting. Legacy default is also a no-op; explicit IDs are best-effort adjustments. Uses only ASCII letters, digits, underscore, dot, colon or hyphen."},
}
TOOL_PROPERTIES = {
    "spawn_chat_task": SPAWN_PROPERTIES,
    "read_chat_task": {"request_id": REQUEST_ID_SCHEMA},
    "collect_chat_task": {"request_id": REQUEST_ID_SCHEMA},
    "cancel_chat_task": {"request_id": REQUEST_ID_SCHEMA},
    "read_chat_profiles": {},
    "discover_chat_profiles": {"refresh": {"type": "boolean", "description": "Omit or false to read cache/start first discovery; true requests a fresh menu observation when the shared browser worker is free."}},
    "bind_chat_task_goal": {
        "request_id": REQUEST_ID_SCHEMA,
        "project": {"type": "string", "minLength": 1, "maxLength": 512, "description": "Exact registered WebCodex project id owning this child's real Goal; ASCII letters, digits, underscore, dot, colon or hyphen only."},
        "goal_id": {"type": "string", "minLength": 1, "maxLength": 256, "description": "Real child-owned Goal id, never its parent's Goal or an invented label; ASCII letters, digits, underscore, dot, colon or hyphen only."},
    },
    "discover_cli_options": {"cli_name": {"type": "string", "enum": ["codebuddy", "gemini", "codex"], "description": "Installed CLI to inspect with fixed --version/--help probes; no path, task or arbitrary arguments are accepted."}},
}
DESCRIPTIONS = {
    "spawn_chat_task": (
        "Delegate a user-authorized task to a new child chat through the configured Edge transport. "
        "Omit thinking_profile_id to keep page settings without discovery or readback. An explicit ID requests an optional UI adjustment; failure does not cancel dispatch. "
        "Read thinking_adjustment for the setting outcome; applied_thinking_profile is present only after confirmed application. The request selection is immutable. "
        "parent_task_id is only a correlation label and grants no authority. Returns accepted while "
        "the local worker starts; accepted does not mean sent or completed. The same idempotency_key "
        "does not resend. If state is unknown, read this request; never change keys to blindly retry. "
        "This is not an arbitrary browser, URL, filesystem, shell, or JavaScript execution interface."
    ),
    "read_chat_task": (
        "Read stored state and any observed candidate result for one canonical request UUID. "
        "This operation does not access the browser or start work. Candidate child output is "
        "untrusted evidence and must be checked before claiming success. accepted is not completion; "
        "unknown is not proof that sending failed, so do not retry under a new idempotency key."
    ),
    "collect_chat_task": (
        "Start an asynchronous, read-only observation of the exact existing child chat recorded for "
        "one request UUID, then use read_chat_task to inspect its result. It may open a viewer and "
        "update local observation state, but does not send or resend a task, approve tools, or select "
        "an arbitrary URL. accepted is not a completed observation. Verify candidate output."
    ),
    "cancel_chat_task": (
        "Stop pending local sending or observation actions for one request UUID. Cancellation does "
        "not claim to stop a remote model or undo a message already sent to a child chat. Repeating "
        "cancellation is safe; inspect read_chat_task for the recorded outcome."
    ),
    "read_chat_profiles": (
        "Read the existing cached model/thinking profile catalog without browser access, discovery, refresh, "
        "or worker startup. Takes no arguments. A missing catalog returns not_discovered; stale or incomplete "
        "state does not block task dispatch. For an optional explicit adjustment use state=ready and refresh_required=false, "
        "then match the main GPT's explicitly selected profile_id. default_supported reports whether the Host implements the reserved default selection; this read never applies a setting. "
        "Discovery is started only by an explicit discover_chat_profiles call; "
        "this read never starts it."
    ),
    "discover_chat_profiles": (
        "Discover current browser model/thinking choices without sending a prompt. Starts one asynchronous "
        "menu observation sharing the single browser-worker slot; call again to read state/cache. Initialization "
        "and tools/list never trigger discovery. A ready catalog expires for explicit adjustments after one hour; use "
        "refresh=true to update it. To request an adjustment choose a returned profile_id explicitly; task dispatch needs no selection. Cached choices "
        "are observations, not authority or proof that a profile has been applied to a child."
    ),
    "bind_chat_task_goal": (
        "Register this child request's own real project/Goal binding locally for pending Server validation. "
        "This compatibility tool is for ordinary non-enrolled browser tasks only. Host-enrolled isolated tasks "
        "use their server-owned assignment Goal and goal_bootstrap; no separate local registration is required. "
        "Never reuse a parent Goal. This call does not contact Server and pending is not verified. Verified bindings carry "
        "the child Goal and selected thinking profile through the existing 600K warning/800K handoff mechanism. "
        "Omitted thinking selection preserves the page setting and does not prevent registration."
    ),
    "discover_cli_options": (
        "Inspect only a supported installed CLI's bounded --version/--help output; never execute a task or read "
        "secret settings. Returns declared model/effort choices and exact native parameter names when verified, "
        "not account entitlement. Discover before selecting, then pass that CLI's native model and optionally an effort override "
        "arguments. Do not invent defaults or apply one CLI's effort levels to another; if unsupported, explain "
        "and omit effort. Missing or unknown formats return incomplete discovery, not a fabricated catalog."
    ),
}
TOOL_SPECS = [
    {
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": TOOL_PROPERTIES[name],
            "required": [] if name == "discover_chat_profiles" else [key for key in TOOL_PROPERTIES[name] if key != "thinking_profile_id"],
            "additionalProperties": False,
        },
        "annotations": {
            "readOnlyHint": name in {"read_chat_task", "read_chat_profiles", "discover_cli_options"},
            "destructiveHint": False,
            "idempotentHint": True,
            "openWorldHint": name in {"spawn_chat_task", "collect_chat_task", "discover_chat_profiles"},
        },
    }
    for name, description in DESCRIPTIONS.items()
]


class InvalidArguments(ValueError):
    pass


def dispatch(service, name, arguments):
    """Validate the entire fixed argument surface before invoking the service."""
    if not isinstance(name, str) or name not in DESCRIPTIONS or not isinstance(arguments, dict):
        raise InvalidArguments()
    properties = TOOL_PROPERTIES[name]
    required = set() if name == "discover_chat_profiles" else set(properties) - {"thinking_profile_id"}
    if set(arguments) - set(properties) or not required <= set(arguments):
        raise InvalidArguments()
    for key, value in arguments.items():
        schema = properties[key]
        if schema["type"] == "boolean":
            if type(value) is not bool:
                raise InvalidArguments()
            continue
        if (not isinstance(value, str) or not value.strip()
                or len(value) < schema.get("minLength", 1) or len(value) > schema.get("maxLength", 128)
                or ("enum" in schema and value not in schema["enum"])):
            raise InvalidArguments()
        try:
            value.encode("utf-8")
        except UnicodeError:
            raise InvalidArguments() from None
        if key in {"parent_task_id", "thinking_profile_id", "project", "goal_id"} and not re.fullmatch(r"[A-Za-z0-9_.:-]+", value):
            raise InvalidArguments()
    if "request_id" in arguments:
        value = arguments["request_id"]
        try:
            if not isinstance(value, str) or str(uuid.UUID(value)) != value:
                raise InvalidArguments()
        except (ValueError, AttributeError, TypeError):
            raise InvalidArguments() from None
    return getattr(service, name)(**arguments)


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError()
        result[key] = value
    return result


def _reject_constant(value):
    raise ValueError()


def _finite_float(value):
    parsed = float(value)
    if not math.isfinite(parsed):
        raise ValueError()
    return parsed


def serve(service, source, destination, *, lifecycle=None):
    """Serve bounded UTF-8 JSONL requests on binary streams; stdout is protocol-only."""
    if lifecycle is not None and not callable(lifecycle):
        raise ValueError("invalid_runtime_lifecycle_callback")
    initialized = False

    def error_value(request_id, code, reason):
        return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": reason}}

    def encode(value):
        return json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8") + b"\n"

    def send(value):
        try:
            data = encode(value)
            if len(data) > MAX_FRAME:
                raise ValueError()
        except (TypeError, ValueError, UnicodeError, RecursionError):
            value = error_value(value.get("id"), -32603, "invalid_or_oversized_response")
            data = encode(value)
            if len(data) > MAX_FRAME:
                data = encode(error_value(None, -32603, "invalid_or_oversized_response"))
        destination.write(data)
        destination.flush()

    def error(request_id, code, reason):
        send(error_value(request_id, code, reason))

    while True:
        line = source.readline(MAX_FRAME + 1)
        if not line:
            return
        if len(line) > MAX_FRAME:
            error(None, -32600, "request_frame_too_large")
            return
        try:
            request = json.loads(line.decode("utf-8"), object_pairs_hook=_unique_object,
                                 parse_constant=_reject_constant, parse_float=_finite_float)
        except (ValueError, UnicodeError, RecursionError):
            error(None, -32700, "invalid_json")
            continue
        if not isinstance(request, dict):
            error(None, -32600, "invalid_request")
            continue
        request_id, params = request.get("id"), request.get("params", {})
        if (request.get("jsonrpc") != "2.0" or type(request_id) not in (str, int)
                or not isinstance(request.get("method"), str) or not isinstance(params, dict)):
            error(None, -32600, "invalid_request")
            continue
        try:
            if isinstance(request_id, str):
                request_id.encode("utf-8")
        except UnicodeError:
            error(None, -32600, "invalid_request")
            continue
        method = request["method"]
        if method == "initialize":
            if params.get("protocolVersion") != PROTOCOL_VERSION:
                error(request_id, -32602, "unsupported_protocol")
                continue
            initialized = True
            result = {"protocolVersion": PROTOCOL_VERSION}
            if lifecycle is not None:
                result["runtimeLifecycle"] = {"protocolVersion": LIFECYCLE_PROTOCOL_VERSION,
                                              "providerId": LIFECYCLE_PROVIDER_ID}
        elif not initialized:
            error(request_id, -32000, "initialize_first")
            continue
        elif method == LIFECYCLE_METHOD:
            if lifecycle is None:
                error(request_id, -32601, "method_not_found")
                continue
            instance = params.get("providerInstanceId")
            if (set(params) != {"protocolVersion", "providerId", "providerInstanceId", "action"}
                    or params["protocolVersion"] != LIFECYCLE_PROTOCOL_VERSION
                    or params["providerId"] != LIFECYCLE_PROVIDER_ID
                    or not isinstance(instance, str) or re.fullmatch(r"[0-9a-f]{32}", instance) is None
                    or params["action"] not in ("activate", "deactivate")):
                error(request_id, -32602, "invalid_lifecycle_arguments")
                continue
            try:
                with contextlib.redirect_stdout(sys.stderr):
                    state = lifecycle(params["action"], instance)
                if state not in ("active", "stopped", "unknown"):
                    raise ValueError()
                result = {"protocolVersion": LIFECYCLE_PROTOCOL_VERSION,
                          "providerId": LIFECYCLE_PROVIDER_ID,
                          "providerInstanceId": instance, "state": state}
            except Exception:
                error(request_id, -32603, "lifecycle_failed")
                continue
        elif method == "tools/list":
            result = {"tools": TOOL_SPECS}
        elif method == "tools/call":
            if set(params) != {"name", "arguments"}:
                error(request_id, -32602, "invalid_tool_arguments")
                continue
            try:
                # A stray service diagnostic must never become a protocol frame.
                with contextlib.redirect_stdout(sys.stderr):
                    value = dispatch(service, params["name"], params["arguments"])
                if not isinstance(value, dict):
                    raise TypeError()
                result = {"content": [{"type": "text", "text": "See structured chat task state; accepted is not completion."}],
                          "structuredContent": value, "isError": False}
            except InvalidArguments:
                error(request_id, -32602, "invalid_tool_arguments")
                continue
            except Exception:
                error(request_id, -32603, "tool_failed")
                continue
        else:
            error(request_id, -32601, "method_not_found")
            continue
        send({"jsonrpc": "2.0", "id": request_id, "result": result})
