import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import manager_bindings as managers
from manager_bindings import AutoManagerBindings, ManagerBindingCache, RootManagerBindings


class ManagerBindingsTests(unittest.TestCase):
    def setUp(self):
        self.row = {"root_task_id": "root-a", "host_id": "host-a", "manager_agent_id": "manager-a",
                    "manager_run_id": "run-a", "expected_generation": 4}
        self.value = {"owner_user": "owner", "project": "project", "roots": [self.row]}

    def load(self, value=None):
        return RootManagerBindings(self.value if value is None else value, owner_user="owner", project="project")

    def test_exact_root_receipt_is_copied_and_never_latest(self):
        bindings = self.load()
        result = bindings.for_root("root-a")
        self.assertEqual(result["manager_run_id"], "run-a")
        result["manager_run_id"] = "changed"
        self.row["manager_run_id"] = "also-changed"
        self.assertEqual(bindings.for_root("root-a")["manager_run_id"], "run-a")
        with self.assertRaisesRegex(ValueError, "exact_manager_binding_required"):
            bindings.for_root("root-b")

    def test_wrong_scope_and_duplicate_root_rejected(self):
        for field in ("owner_user", "project"):
            value = copy.deepcopy(self.value)
            value[field] = "other"
            with self.assertRaises(ValueError):
                self.load(value)
        self.value["roots"].append(dict(self.row))
        with self.assertRaisesRegex(ValueError, "duplicate_manager_root"):
            self.load()

    def test_no_credentials_or_implicit_generation(self):
        for generation in (True, None, 0, -1, 1.5, "4", 1 << 63):
            value = copy.deepcopy(self.value)
            value["roots"][0]["expected_generation"] = generation
            with self.assertRaises(ValueError):
                self.load(value)
        self.value["roots"][0]["token"] = "synthetic-not-a-credential"
        with self.assertRaises(ValueError):
            self.load()

    def test_grant_in_metadata_rejected_without_echo(self):
        self.row["manager_run_id"] = "wc_en_" + "a" * 64
        with self.assertRaisesRegex(ValueError, "launch_claim_must_not_be_persisted"):
            self.load()

    def test_file_scope_and_duplicate_json_keys(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manager-bindings.json"
            path.write_text(json.dumps(self.value), encoding="utf-8")
            loaded = RootManagerBindings.from_file(path, owner_user="owner", project="project")
            self.assertEqual(loaded.for_root("root-a")["expected_generation"], 4)
            captured = loaded.for_root("root-a")
            self.row.update(manager_run_id="exact-handoff-run", expected_generation=5)
            path.write_text(json.dumps(self.value), encoding="utf-8")
            self.assertEqual(loaded.for_root("root-a")["manager_run_id"], "exact-handoff-run")
            self.assertEqual(captured["manager_run_id"], "run-a")
            path.write_text('{"owner_user":"owner","owner_user":"other"}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid_manager_bindings_file"):
                loaded.for_root("root-a")
        with self.assertRaisesRegex(ValueError, "path_must_be_absolute"):
            RootManagerBindings.from_file("relative.json", owner_user="owner", project="project")


class ManagerBindingCacheTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / "manager-bindings.json"
        self.cache = ManagerBindingCache(self.path, owner_user="owner", project="project")
        self.binding = {"host_id": "host-a", "manager_agent_id": "manager-a",
                        "manager_run_id": "run-a", "expected_generation": 4}

    def read(self, root="root-a"):
        return RootManagerBindings.from_file(self.path, owner_user="owner", project="project").for_root(root)

    def test_first_store_is_original_five_field_schema_and_independent_copy(self):
        actual = self.cache.store("root-a", self.binding)
        self.assertEqual(self.read(), self.binding)
        raw = json.loads(self.path.read_text(encoding="utf-8"))
        self.assertEqual(set(raw), {"owner_user", "project", "roots"})
        self.assertEqual(set(raw["roots"][0]), managers._FIELDS)
        actual["manager_run_id"] = "changed-output"
        self.binding["manager_run_id"] = "changed-input"
        self.assertEqual(self.read()["manager_run_id"], "run-a")

    def test_each_writer_reloads_other_roots_under_existing_file_lock(self):
        other = ManagerBindingCache(self.path, owner_user="owner", project="project")
        self.cache.store("root-a", self.binding)
        second = {**self.binding, "manager_agent_id": "manager-b", "manager_run_id": "run-b"}
        other.store("root-b", second)
        successor = {**self.binding, "manager_run_id": "run-next", "expected_generation": 7}
        self.cache.store("root-a", successor)
        self.assertEqual(self.read(), successor)
        self.assertEqual(self.read("root-b"), second)

    def test_identical_receipt_does_not_rewrite_or_return_shared_state(self):
        self.cache.store("root-a", self.binding)
        before = self.path.read_bytes()
        with patch.object(managers, "write_json") as writer:
            same = self.cache.store("root-a", self.binding)
        writer.assert_not_called()
        self.assertEqual(self.path.read_bytes(), before)
        self.assertIsNot(same, self.binding)

    def test_lower_generation_same_generation_conflicts_and_identity_changes_never_overwrite(self):
        self.cache.store("root-a", self.binding)
        before = self.path.read_bytes()
        for change in ({"expected_generation": 3, "manager_run_id": "old"},
                       {"manager_run_id": "same-generation-other-run"},
                       {"manager_agent_id": "different-agent"},
                       {"host_id": "different-host"},
                       {"expected_generation": 5},
                       {"expected_generation": 5, "manager_agent_id": "different-agent", "manager_run_id": "new"}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.cache.store("root-a", {**self.binding, **change})
            self.assertEqual(self.path.read_bytes(), before)

    def test_same_agent_successor_updates_generation_without_changing_captured_receipt(self):
        captured = self.cache.store("root-a", self.binding)
        newer = {**self.binding, "manager_run_id": "successor", "expected_generation": 6}
        self.cache.store("root-a", newer)
        self.assertEqual(captured, self.binding)
        self.assertEqual(self.read(), newer)

    def test_corrupt_or_foreign_existing_file_is_not_replaced(self):
        for content in ('{"owner_user":"owner","owner_user":"duplicate"}',
                        json.dumps({"owner_user": "other", "project": "project", "roots": []})):
            self.path.write_text(content, encoding="utf-8")
            before = self.path.read_bytes()
            with self.assertRaises(ValueError):
                self.cache.store("root-a", self.binding)
            self.assertEqual(self.path.read_bytes(), before)

    def test_busy_file_lock_does_not_lose_other_roots(self):
        self.cache.store("root-a", self.binding)
        before = self.path.read_bytes()
        with managers.watcher_lock(self.path):
            with self.assertRaises(ValueError):
                self.cache.store("root-b", self.binding)
        self.assertEqual(self.path.read_bytes(), before)
        self.cache.store("root-b", self.binding)
        self.assertEqual(self.read(), self.binding)
        self.assertEqual(self.read("root-b"), self.binding)

    def test_write_failure_leaves_previous_receipt_and_does_not_return_cached_authority(self):
        self.cache.store("root-a", self.binding)
        before = self.path.read_bytes()
        with patch.object(managers, "write_json", side_effect=OSError("fixture write failure")):
            with self.assertRaises(OSError):
                self.cache.store("root-a", {**self.binding, "manager_run_id": "new", "expected_generation": 5})
        self.assertEqual(self.path.read_bytes(), before)

    def test_invalid_input_cannot_create_the_cache(self):
        for change in ({"expected_generation": True}, {"manager_run_id": "wc_en_" + "a" * 64},
                       {"token": "not-accepted"}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.cache.store("root-a", {**self.binding, **change})
            self.assertFalse(self.path.exists())
        with self.assertRaisesRegex(ValueError, "path_must_be_absolute"):
            ManagerBindingCache("relative.json", owner_user="owner", project="project")


class AutoManagerBindingsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / "manager-bindings.json"
        self.root = "wc_agent_task_" + "1" * 32
        self.host = "wc_host_" + "2" * 32
        self.projection = {"owner_user": "owner", "project": "project", "root_task_id": self.root,
            "host_id": self.host, "manager_agent_id": "wc_iagent_" + "3" * 32,
            "manager_run_id": "wc_agent_run_" + "4" * 32, "generation": 7,
            "enrollment_id": "wc_enrollment_" + "5" * 32, "native_host_generation": 33,
            "run_expires_at_ms": 2_000_000, "window_binding_ref": "wc_host_binding_" + "6" * 64}
        self.api = Mock(owner_user="owner", project="project", origin="http://127.0.0.1:19876")
        self.api.post.side_effect = lambda payload: {"binding": copy.deepcopy(self.projection)}
        self.host_provider = Mock(side_effect=lambda: self.host)
        self.resolver = AutoManagerBindings.from_control(self.api, cache_path=self.path,
                                                        host_id_provider=self.host_provider)
        self.clock = patch.object(managers.time, "time", return_value=1000.0)
        self.clock.start()
        self.addCleanup(self.clock.stop)

    def read_cache(self):
        return RootManagerBindings.from_file(self.path, owner_user="owner", project="project").for_root(self.root)

    def test_exact_bootstrap_projection_automatically_populates_only_original_fields(self):
        self.assertFalse(self.path.exists())
        result = self.resolver.for_root(self.root)
        self.api.post.assert_called_once_with({"action": "agent_host_manager_binding_read",
            "owner_user": "owner", "project": "project", "root_task_id": self.root, "host_id": self.host})
        self.assertEqual(result["expected_generation"], 7)
        self.assertNotEqual(result["expected_generation"], self.projection["native_host_generation"])
        self.assertEqual(self.read_cache(), result)
        data = json.loads(self.path.read_text(encoding="utf-8"))
        self.assertEqual(set(data["roots"][0]), managers._FIELDS)
        self.assertNotIn("window_binding_ref", data["roots"][0])
        self.assertNotIn("enrollment_id", data["roots"][0])

    def test_every_lookup_queries_real_control_even_when_cache_matches(self):
        first = self.resolver.for_root(self.root)
        second = self.resolver.for_root(self.root)
        self.assertEqual(first, second)
        self.assertIsNot(first, second)
        self.assertEqual(self.api.post.call_count, 2)

    def test_none_or_failed_lookup_never_falls_back_to_existing_cache(self):
        self.resolver.for_root(self.root)
        before = self.path.read_bytes()
        self.api.post.side_effect = None
        self.api.post.return_value = {"binding": None}
        with self.assertRaisesRegex(ValueError, "^manager_binding_unavailable$"):
            self.resolver.for_root(self.root)
        self.api.post.side_effect = RuntimeError("private detail wc_en_" + "a" * 64)
        with self.assertRaisesRegex(ValueError, "^manager_binding_lookup_failed$") as raised:
            self.resolver.for_root(self.root)
        self.assertNotIn("wc_en_", str(raised.exception))
        self.assertEqual(self.path.read_bytes(), before)

    def test_host_clear_or_provider_error_stops_before_control_request(self):
        self.resolver.for_root(self.root)
        before, calls = self.path.read_bytes(), self.api.post.call_count
        self.host = None
        with self.assertRaisesRegex(ValueError, "^manager_host_context_unavailable$"):
            self.resolver.for_root(self.root)
        self.host_provider.side_effect = RuntimeError("private context")
        with self.assertRaisesRegex(ValueError, "^manager_host_context_unavailable$"):
            self.resolver.for_root(self.root)
        self.assertEqual(self.api.post.call_count, calls)
        self.assertEqual(self.path.read_bytes(), before)

    def test_changed_host_or_api_scope_during_query_cannot_publish_response(self):
        for mutation in (lambda: setattr(self, "host", "wc_host_" + "f" * 32),
                         lambda: setattr(self.api, "project", "other"),
                         lambda: setattr(self.api, "origin", "http://127.0.0.1:19877")):
            with self.subTest(mutation=mutation):
                self.host = self.projection["host_id"]
                self.api.project = "project"
                self.api.origin = "http://127.0.0.1:19876"
                def post(payload):
                    reply = {"binding": copy.deepcopy(self.projection)}
                    mutation()
                    return reply
                self.api.post.side_effect = post
                with self.assertRaisesRegex(ValueError, "^manager_binding_context_changed$"):
                    self.resolver.for_root(self.root)
                self.assertFalse(self.path.exists())

    def test_wrong_scope_expired_run_missing_window_and_bad_numbers_do_not_touch_cache(self):
        self.resolver.for_root(self.root)
        before = self.path.read_bytes()
        original = copy.deepcopy(self.projection)
        for change in ({"owner_user": "other"}, {"project": "other"}, {"root_task_id": "other"},
                       {"host_id": "wc_host_" + "f" * 32}, {"run_expires_at_ms": 1_000_000},
                       {"window_binding_ref": ""}, {"window_binding_ref": None},
                       {"generation": True}, {"generation": 0}, {"native_host_generation": 0},
                       {"run_expires_at_ms": "2000000"}, {"manager_run_id": "wc_ar_" + "a" * 64},
                       {"manager_agent_id": "wc_en_" + "a" * 64}):
            with self.subTest(change=change):
                self.projection = {**original, **change}
                with self.assertRaises(ValueError):
                    self.resolver.for_root(self.root)
                self.assertEqual(self.path.read_bytes(), before)

    def test_only_actual_b6_dto_is_accepted_not_fake_authority_flags(self):
        for response in ({}, {"binding": False}, {"binding": None, "active": True},
                         {"binding": {**self.projection, "active": True}},
                         {"binding": {key: value for key, value in self.projection.items() if key != "window_binding_ref"}}):
            with self.subTest(response=response):
                self.api.post.side_effect = None
                self.api.post.return_value = response
                with self.assertRaises(ValueError):
                    self.resolver.for_root(self.root)
                self.assertFalse(self.path.exists())

    def test_handoff_updates_cache_but_old_prepared_delivery_cannot_retarget(self):
        expected = self.resolver.for_root(self.root)
        self.projection.update(manager_run_id="wc_agent_run_" + "7" * 32, generation=9)
        with self.assertRaisesRegex(ValueError, "^manager_binding_changed_before_delivery$"):
            self.resolver.validate_for_delivery(self.root, expected)
        self.assertEqual(expected["expected_generation"], 7)
        self.assertEqual(self.read_cache()["expected_generation"], 9)
        current = self.resolver.for_root(self.root)
        self.assertIsNone(self.resolver.validate_for_delivery(self.root, current))

    def test_delivery_guard_freshly_checks_host_and_unavailable_manager(self):
        expected = self.resolver.for_root(self.root)
        calls = self.api.post.call_count
        self.host = None
        with self.assertRaises(ValueError):
            self.resolver.validate_for_delivery(self.root, expected)
        self.assertEqual(self.api.post.call_count, calls)
        self.host = self.projection["host_id"]
        self.api.post.side_effect = None
        self.api.post.return_value = {"binding": None}
        with self.assertRaisesRegex(ValueError, "^manager_binding_unavailable$"):
            self.resolver.validate_for_delivery(self.root, expected)
        self.assertEqual(self.api.post.call_count, calls + 1)

    def test_stale_or_conflicting_control_observation_does_not_return_new_authority(self):
        self.resolver.for_root(self.root)
        before = self.path.read_bytes()
        original = copy.deepcopy(self.projection)
        for change in ({"generation": 6, "manager_run_id": "wc_agent_run_" + "7" * 32},
                       {"manager_run_id": "wc_agent_run_" + "7" * 32},
                       {"generation": 8, "manager_agent_id": "wc_iagent_" + "8" * 32}):
            self.projection = {**original, **change}
            with self.assertRaises(ValueError):
                self.resolver.for_root(self.root)
            self.assertEqual(self.path.read_bytes(), before)

    def test_wrong_api_scope_or_missing_provider_cannot_construct(self):
        cache = ManagerBindingCache(self.path, owner_user="other", project="project")
        with self.assertRaisesRegex(ValueError, "invalid_manager_bindings_scope"):
            AutoManagerBindings(self.api, cache, self.host_provider)
        with self.assertRaisesRegex(ValueError, "invalid_manager_binding_control"):
            AutoManagerBindings.from_control(self.api, cache_path=self.path, host_id_provider=None)


if __name__ == "__main__":
    unittest.main()
