import contextlib
import io
import json
import unittest
from unittest.mock import Mock

import native_server as native


REQUEST_ID = "16e4c3d2-943d-4a57-82f7-6dca7e7cbef0"
SPAWN = {"task": "Compute 17 * 19 and explain the check.", "parent_task_id": "parent-1", "idempotency_key": "child-1", "thinking_profile_id": "fixture-standard"}


def rpc(method, params=None, request_id=1):
    return {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}


def frame(value):
    return json.dumps(value, ensure_ascii=False).encode("utf-8") + b"\n"


INITIALIZE = frame(rpc("initialize", {"protocolVersion": native.PROTOCOL_VERSION}))


class FakeService:
    def __init__(self, result=None, failure=None):
        self.calls = []
        self.result = result if result is not None else {"request_id": REQUEST_ID, "state": "accepted"}
        self.failure = failure

    def __getattr__(self, name):
        if name not in native.DESCRIPTIONS:
            raise AssertionError("Unexpected service access")

        def invoke(**kwargs):
            self.calls.append((name, kwargs))
            if self.failure:
                raise self.failure
            return self.result
        return invoke


class NativeServerTests(unittest.TestCase):
    def run_protocol(self, raw, service=None, *, lifecycle=None):
        self.service = service or FakeService()
        destination = io.BytesIO()
        native.serve(self.service, io.BytesIO(raw), destination, lifecycle=lifecycle)
        return [json.loads(line) for line in destination.getvalue().splitlines()]

    def call(self, name, arguments, service=None):
        return self.run_protocol(INITIALIZE + frame(rpc("tools/call", {"name": name, "arguments": arguments}, 2)), service)[-1]

    def test_initialize_checks_protocol_before_allowing_discovery(self):
        replies = self.run_protocol(frame(rpc("initialize", {"protocolVersion": "wrong"})) + frame(rpc("tools/list")))
        self.assertEqual([reply["error"]["message"] for reply in replies], ["unsupported_protocol", "initialize_first"])
        self.assertEqual(self.service.calls, [])

    def test_discovery_is_fixed_and_has_no_service_calls(self):
        replies = self.run_protocol(INITIALIZE + frame(rpc("tools/list", request_id=2)))
        specs = replies[1]["result"]["tools"]
        self.assertEqual({spec["name"] for spec in specs}, {
            "spawn_chat_task", "read_chat_task", "collect_chat_task", "cancel_chat_task",
            "read_chat_profiles", "discover_chat_profiles", "bind_chat_task_goal", "discover_cli_options"})
        self.assertEqual(self.service.calls, [])
        for spec in specs:
            self.assertEqual(spec["annotations"]["readOnlyHint"], spec["name"] in {"read_chat_task", "read_chat_profiles", "discover_cli_options"})
            self.assertTrue(spec["annotations"]["idempotentHint"])
            self.assertFalse(spec["inputSchema"]["additionalProperties"])

    def test_all_eight_calls_route_to_exact_service_method(self):
        for name in native.DESCRIPTIONS:
            args = {"spawn_chat_task": SPAWN, "discover_chat_profiles": {}, "read_chat_profiles": {},
                    "bind_chat_task_goal": {"request_id": REQUEST_ID, "project": "project-1", "goal_id": "goal-1"},
                    "discover_cli_options": {"cli_name": "codebuddy"}}.get(name, {"request_id": REQUEST_ID})
            with self.subTest(name=name):
                reply = self.call(name, args)
                self.assertEqual(self.service.calls, [(name, args)])
                self.assertEqual(reply["result"]["structuredContent"]["state"], "accepted")

    def test_cached_profiles_read_takes_no_arguments_and_is_local_only(self):
        spec = next(spec for spec in native.TOOL_SPECS if spec["name"] == "read_chat_profiles")
        self.assertEqual(spec["inputSchema"]["properties"], {})
        self.assertEqual(spec["inputSchema"]["required"], [])
        self.assertTrue(spec["annotations"]["readOnlyHint"])
        self.assertFalse(spec["annotations"]["openWorldHint"])
        for arguments in ({"refresh": False}, {"refresh": True}, {"profile_id": "default"}, {"url": "https://example.com"}):
            with self.subTest(arguments=arguments):
                self.assertEqual(self.call("read_chat_profiles", arguments)["error"]["code"], -32602)
                self.assertEqual(self.service.calls, [])

    def test_catalog_uses_runner_supported_schema_profile(self):
        allowed = {"type", "title", "description", "properties", "required", "additionalProperties",
                   "enum", "const", "minLength", "maxLength", "minItems", "maxItems", "items"}

        def check(schema):
            self.assertLessEqual(set(schema), allowed)
            for child in schema.get("properties", {}).values():
                check(child)
            if isinstance(schema.get("items"), dict):
                check(schema["items"])
            if isinstance(schema.get("additionalProperties"), dict):
                check(schema["additionalProperties"])

        for spec in native.TOOL_SPECS:
            with self.subTest(tool=spec["name"]):
                check(spec["inputSchema"])

    def test_extra_fields_and_missing_fields_do_not_reach_service(self):
        invalid = [{**SPAWN, key: "arbitrary"} for key in ("path", "url", "javascript")]
        invalid += [{key: value for key, value in SPAWN.items() if key != missing} for missing in SPAWN if missing != "thinking_profile_id"]
        for args in invalid:
            with self.subTest(args=args):
                self.assertEqual(self.call("spawn_chat_task", args)["error"]["code"], -32602)
                self.assertEqual(self.service.calls, [])

    def test_spawn_can_omit_thinking_profile(self):
        args = {key: value for key, value in SPAWN.items() if key != "thinking_profile_id"}
        self.assertIn("result", self.call("spawn_chat_task", args))
        spec = next(spec for spec in native.TOOL_SPECS if spec["name"] == "spawn_chat_task")
        self.assertNotIn("thinking_profile_id", spec["inputSchema"]["required"])

    def test_spawn_text_bounds_and_types(self):
        for key, maximum in (("task", 12000), ("parent_task_id", 128), ("idempotency_key", 128), ("thinking_profile_id", 128)):
            for value in (None, True, 5, [], {}, "", " \t\n", "x" * (maximum + 1), "\ud800"):
                with self.subTest(key=key, type=type(value).__name__):
                    with self.assertRaises(native.InvalidArguments):
                        native.dispatch(FakeService(), "spawn_chat_task", {**SPAWN, key: value})
            for value in ("x", "x" * maximum):
                service = FakeService()
                native.dispatch(service, "spawn_chat_task", {**SPAWN, key: value})
                self.assertEqual(len(service.calls), 1)

    def test_only_canonical_uuid_is_accepted(self):
        for name in ("read_chat_task", "collect_chat_task", "cancel_chat_task"):
            for value in (None, True, [], "", REQUEST_ID.upper(), REQUEST_ID.replace("-", ""), "{" + REQUEST_ID + "}", "../record", "https://chatgpt.com/c/example"):
                with self.subTest(name=name, value=value):
                    self.assertEqual(self.call(name, {"request_id": value})["error"]["code"], -32602)
                    self.assertEqual(self.service.calls, [])

    def test_parent_label_matches_service_identity_alphabet(self):
        for value in ("parent/child", "parent task", "父任务", "https://example.com", "parent\n"):
            with self.subTest(value=value):
                self.assertEqual(self.call("spawn_chat_task", {**SPAWN, "parent_task_id": value})["error"]["code"], -32602)
                self.assertEqual(self.service.calls, [])
        self.assertIn("result", self.call("spawn_chat_task", {**SPAWN, "parent_task_id": "parent_1.child:2-3"}))

    def test_profile_and_new_tool_arguments_are_exactly_validated(self):
        invalid = [("spawn_chat_task", {**SPAWN, "thinking_profile_id": "../profile"}),
                   ("discover_chat_profiles", {"refresh": "false"}),
                   ("discover_chat_profiles", {"refresh": 0}),
                   ("discover_chat_profiles", {"refresh": True, "url": "https://example.com"}),
                   ("discover_cli_options", {"cli_name": "wordbuddy"}),
                   ("discover_cli_options", {"cli_name": "codebuddy", "args": ["--print"]}),
                   ("bind_chat_task_goal", {"request_id": REQUEST_ID, "project": "p", "goal_id": ""}),
                   ("bind_chat_task_goal", {"request_id": REQUEST_ID, "project": "../p", "goal_id": "g"}),
                   ("bind_chat_task_goal", {"request_id": REQUEST_ID, "project": "p", "goal_id": "x" * 257}),
                   ("bind_chat_task_goal", {"request_id": REQUEST_ID.upper(), "project": "p", "goal_id": "g"}),
                   ("bind_chat_task_goal", {"request_id": REQUEST_ID, "project": "x" * 513, "goal_id": "g"})]
        for name, arguments in invalid:
            with self.subTest(name=name, arguments=arguments):
                self.assertEqual(self.call(name, arguments)["error"]["code"], -32602)
                self.assertEqual(self.service.calls, [])
        for refresh in (False, True):
            self.assertIn("result", self.call("discover_chat_profiles", {"refresh": refresh}))
        self.assertIn("result", self.call("bind_chat_task_goal", {"request_id": REQUEST_ID, "project": "p" * 512, "goal_id": "g" * 256}))

    def test_unknown_tool_and_bad_envelopes_never_reach_service(self):
        for params in ({"name": "eval", "arguments": {}}, {"name": [], "arguments": {}},
                       {"name": "read_chat_task", "arguments": []},
                       {"name": "read_chat_task", "arguments": {"request_id": REQUEST_ID}, "url": "https://example.com"}):
            reply = self.run_protocol(INITIALIZE + frame(rpc("tools/call", params)))[-1]
            self.assertEqual(reply["error"]["code"], -32602)
            self.assertEqual(self.service.calls, [])

    def test_duplicate_json_keys_and_nonfinite_numbers_are_rejected(self):
        for raw in (b'{"id":1,"id":2}\n', b'{"params":{"task":"a","task":"b"}}\n',
                    b'{"id":NaN}\n', b'{"id":Infinity}\n', b'{"id":-Infinity}\n', b'{"id":1e9999}\n', b'\xff\n'):
            with self.subTest(raw=raw):
                replies = self.run_protocol(raw + INITIALIZE)
                self.assertEqual(replies[0]["error"]["message"], "invalid_json")
                self.assertEqual(replies[1]["result"]["protocolVersion"], native.PROTOCOL_VERSION)

    def test_oversized_frame_stops_before_following_request(self):
        replies = self.run_protocol(b" " * (native.MAX_FRAME + 1) + b"\n" + INITIALIZE)
        self.assertEqual(len(replies), 1)
        self.assertEqual(replies[0]["error"]["message"], "request_frame_too_large")

    def test_invalid_request_envelopes_have_fixed_errors(self):
        for request in ([], None, rpc("tools/list", request_id=True), rpc("tools/list", request_id=None),
                        {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": []},
                        {"jsonrpc": "1.0", "id": 1, "method": "tools/list"}):
            with self.subTest(request=request):
                self.assertEqual(self.run_protocol(frame(request))[0]["error"]["message"], "invalid_request")

    def test_service_errors_never_expose_exception_text(self):
        for failure in (ValueError("secret task payload"), RuntimeError("private filesystem path")):
            reply = self.call("read_chat_task", {"request_id": REQUEST_ID}, FakeService(failure=failure))
            self.assertEqual(reply["error"], {"code": -32603, "message": "tool_failed"})

    def test_invalid_or_oversized_service_result_has_fixed_error(self):
        for result in ({"result": "x" * native.MAX_FRAME}, {"value": float("nan")}, {"value": object()}):
            reply = self.call("read_chat_task", {"request_id": REQUEST_ID}, FakeService(result=result))
            self.assertEqual(reply["error"]["message"], "invalid_or_oversized_response")

    def test_stdout_diagnostics_cannot_corrupt_protocol(self):
        class NoisyService:
            def read_chat_task(self, request_id):
                print("worker diagnostic")
                return {"state": "accepted"}
        log = io.StringIO()
        with contextlib.redirect_stderr(log):
            reply = self.call("read_chat_task", {"request_id": REQUEST_ID}, NoisyService())
        self.assertEqual(reply["result"]["structuredContent"], {"state": "accepted"})
        self.assertEqual(log.getvalue(), "worker diagnostic\n")

    def test_unconfigured_initialize_keeps_the_original_protocol_and_tools(self):
        replies = self.run_protocol(INITIALIZE + frame(rpc("tools/list", request_id=2)))
        self.assertEqual(replies[0]["result"], {"protocolVersion": native.PROTOCOL_VERSION})
        self.assertEqual(len(replies[1]["result"]["tools"]), 8)

    def test_configured_initialize_only_advertises_private_lifecycle(self):
        lifecycle = Mock(side_effect=AssertionError("initialization must remain inert"))
        replies = self.run_protocol(INITIALIZE + frame(rpc("tools/list", request_id=2)) + INITIALIZE,
                                    lifecycle=lifecycle)
        self.assertEqual(replies[0]["result"], {"protocolVersion": native.PROTOCOL_VERSION,
            "runtimeLifecycle": {"protocolVersion": native.LIFECYCLE_PROTOCOL_VERSION,
                                 "providerId": "chat-agents"}})
        self.assertEqual(replies[2]["result"], replies[0]["result"])
        self.assertEqual(len(replies[1]["result"]["tools"]), 8)
        self.assertNotIn(native.LIFECYCLE_METHOD, {tool["name"] for tool in replies[1]["result"]["tools"]})
        self.assertEqual(self.service.calls, [])
        lifecycle.assert_not_called()

    def test_lifecycle_requires_initialize_and_an_explicit_private_callback(self):
        params = {"protocolVersion": native.LIFECYCLE_PROTOCOL_VERSION, "providerId": "chat-agents",
                  "providerInstanceId": "a" * 32, "action": "activate"}
        lifecycle = Mock()
        early = self.run_protocol(frame(rpc(native.LIFECYCLE_METHOD, params)), lifecycle=lifecycle)
        self.assertEqual(early[0]["error"]["message"], "initialize_first")
        lifecycle.assert_not_called()
        absent = self.run_protocol(INITIALIZE + frame(rpc(native.LIFECYCLE_METHOD, params)))
        self.assertEqual(absent[-1]["error"]["message"], "method_not_found")
        self.assertEqual(self.service.calls, [])

    def test_lifecycle_parameters_are_exact_and_use_runner_simple_uuid(self):
        valid = {"protocolVersion": native.LIFECYCLE_PROTOCOL_VERSION, "providerId": "chat-agents",
                 "providerInstanceId": "a" * 32, "action": "activate"}
        invalid = [{**valid, "extra": "untrusted"}]
        invalid += [{key: value for key, value in valid.items() if key != missing} for missing in valid]
        invalid += [{**valid, key: value} for key, value in (
            ("protocolVersion", "webcodex-plugin-v1"), ("providerId", "other-provider"),
            ("providerInstanceId", REQUEST_ID), ("providerInstanceId", "A" * 32),
            ("providerInstanceId", "a" * 31), ("providerInstanceId", "a" * 33),
            ("providerInstanceId", True), ("providerInstanceId", 123),
            ("providerInstanceId", []), ("providerInstanceId", "a" * 31 + "\n"),
            ("action", "restart"), ("action", True), ("action", []), ("action", {}))]
        for params in invalid:
            with self.subTest(field_names=list(params)):
                lifecycle = Mock()
                replies = self.run_protocol(INITIALIZE + frame(rpc(native.LIFECYCLE_METHOD, params)),
                                            lifecycle=lifecycle)
                self.assertEqual(replies[-1]["error"],
                                 {"code": -32602, "message": "invalid_lifecycle_arguments"})
                lifecycle.assert_not_called()
                self.assertEqual(self.service.calls, [])

    def test_lifecycle_result_has_only_the_four_frozen_fields(self):
        for state in ("active", "stopped", "unknown"):
            with self.subTest(state=state):
                params = {"protocolVersion": native.LIFECYCLE_PROTOCOL_VERSION, "providerId": "chat-agents",
                          "providerInstanceId": "a" * 32, "action": "deactivate"}
                lifecycle = Mock(return_value=state)
                replies = self.run_protocol(INITIALIZE + frame(rpc(native.LIFECYCLE_METHOD, params, 2)),
                                            lifecycle=lifecycle)
                self.assertEqual(replies[-1]["result"], {"protocolVersion": native.LIFECYCLE_PROTOCOL_VERSION,
                    "providerId": "chat-agents", "providerInstanceId": "a" * 32, "state": state})
                lifecycle.assert_called_once_with("deactivate", "a" * 32)
                self.assertEqual(self.service.calls, [])

    def test_lifecycle_cannot_be_called_as_a_model_tool(self):
        lifecycle = Mock()
        replies = self.run_protocol(INITIALIZE + frame(rpc("tools/call",
            {"name": native.LIFECYCLE_METHOD, "arguments": {"action": "activate"}}, 2)),
            lifecycle=lifecycle)
        self.assertEqual(replies[-1]["error"]["message"], "invalid_tool_arguments")
        lifecycle.assert_not_called()
        self.assertEqual(self.service.calls, [])

    def test_lifecycle_failure_or_invalid_state_never_echoes_internal_details(self):
        params = {"protocolVersion": native.LIFECYCLE_PROTOCOL_VERSION, "providerId": "chat-agents",
                  "providerInstanceId": "a" * 32, "action": "activate"}
        callbacks = [Mock(side_effect=ValueError("synthetic-secret-do-not-echo")),
                     Mock(return_value="synthetic-secret-do-not-echo"), Mock(return_value=None),
                     Mock(return_value={"state": "active"})]
        for lifecycle in callbacks:
            replies = self.run_protocol(INITIALIZE + frame(rpc(native.LIFECYCLE_METHOD, params)),
                                        lifecycle=lifecycle)
            self.assertEqual(replies[-1]["error"], {"code": -32603, "message": "lifecycle_failed"})
            self.assertNotIn("synthetic-secret", json.dumps(replies))


if __name__ == "__main__":
    unittest.main()
