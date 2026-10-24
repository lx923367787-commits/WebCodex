"""Private carrier tests: only explicit temporary files, never a live Host."""
import json
import hashlib
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import Mock, patch

import automatic_dispatch_config as config
from run_auto_handoff import bind_connection


HOST = "wc_host_" + "1" * 32
ORIGIN = "http://127.0.0.1:19876"


class FakeApi:
    def __init__(self, origin, token, timeout, *, environ):
        self.origin, self.token, self.timeout = origin, token, timeout
        self.environ = dict(environ)
        self.owner_user = environ["WEBCODEX_HANDOFF_OWNER_USER"]
        self.project = environ["WEBCODEX_HANDOFF_PROJECT"]

    def post(self, payload):
        raise AssertionError("No control request is expected while loading configuration")


class ContextConfigTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="webcodex-auto-config-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.state = self.root / "desktop-state.json"
        self.carrier = self.root / "control.json"
        self.token = self.root / "private-token.txt"
        self.token.write_text("synthetic-bootstrap", encoding="utf-8")
        self.state_value = {"runtime": {"server_url": ORIGIN, "native_host_id": HOST,
                            "user_token_file": str(self.root / "must-not-read-pat")}}
        self.carrier_value = {"schema_version": 2, "server_url": ORIGIN,
            "bootstrap_token_file": str(self.token),
            "source_ref": "desktop-local-bootstrap:" + str(self.root / "must-not-read-source.env"),
            "owner_user": "fixture-owner", "projects": ["fixture-project"]}
        self.save()
        self.bindings = Mock(side_effect=lambda api, **kwargs: types.SimpleNamespace(api=api, **kwargs))

    def save(self):
        self.state.write_text(json.dumps(self.state_value), encoding="utf-8")
        self.carrier.write_text(json.dumps(self.carrier_value), encoding="utf-8")

    def factory(self, **kwargs):
        options = {"desktop_state": self.state, "control_config": self.carrier,
                   "fallback_api_factory": FakeApi, "member_api_factory": FakeApi,
                   "bindings_factory": self.bindings, "scheduler_runtime": self.root / "scheduler"}
        options.update(kwargs)
        return config.load_context_factory(self.root / "provider", **options)

    def queue_path(self, project):
        return self.root / "provider" / "project-queues" / hashlib.sha256(project.encode()).hexdigest()

    def legacy_identity(self, **fields):
        identity = {"credential_source": config._source_reference(self.carrier_value["source_ref"]),
                    "owner_user": self.carrier_value["owner_user"], "project": "fixture-project",
                    "agent_id": None}
        identity.update(fields)
        path = self.root / "provider" / "cli-fallback-worker.identity.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        value = {"endpoint": ORIGIN, "token_file": json.dumps(identity, sort_keys=True)}
        path.write_text(json.dumps(value), encoding="utf-8")
        return path

    def test_construction_and_absent_carrier_do_not_read_files_or_ambient_credentials(self):
        loader = Mock(side_effect=AssertionError("credential loading forbidden"))
        with patch.object(Path, "open", side_effect=AssertionError("file access forbidden")):
            self.factory(credentials_loader=loader)
            self.assertIsNone(self.factory(control_config=None, credentials_loader=loader)())
        loader.assert_not_called()
        self.bindings.assert_not_called()

    def test_ready_context_uses_only_explicit_bootstrap_file_and_lazy_manager_reader(self):
        # These paths intentionally do not exist. Their mere references must
        # never cause source.env or the Desktop user's PAT to be read.
        with patch.dict("os.environ", {"WEBCODEX_HANDOFF_CONTROL_TOKEN": "ambient-forbidden",
                                     "WEBCODEX_HANDOFF_OWNER_USER": "wrong-owner"}):
            results = self.factory()()
        self.assertEqual(len(results), 1)
        result = results[0]
        self.assertEqual(set(result), {"fallback_api", "member_api", "manager_bindings", "origin",
            "connection_identity", "global_connection_identity", "queue_runtime", "scheduler_runtime"})
        self.assertEqual(result["fallback_api"].token, "synthetic-bootstrap")
        self.assertEqual(result["member_api"].owner_user, "fixture-owner")
        self.assertEqual(result["member_api"].timeout, 5)
        self.assertNotIn("WEBCODEX_HANDOFF_CONTROL_TOKEN", result["member_api"].environ)
        self.assertEqual(result["manager_bindings"].host_id_provider(), HOST)
        self.assertEqual(result["queue_runtime"], self.queue_path("fixture-project"))
        self.assertEqual(result["manager_bindings"].cache_path, result["queue_runtime"] / "manager-bindings.json")
        self.assertEqual(result["scheduler_runtime"], self.root / "scheduler")
        self.assertNotIn("synthetic-bootstrap", result["connection_identity"])
        self.assertNotIn("project", json.loads(result["global_connection_identity"]))
        self.assertFalse((self.root / "provider").exists())

    def test_not_ready_host_never_reads_token_or_uses_user_pat(self):
        self.state_value["runtime"].pop("native_host_id")
        self.carrier_value["bootstrap_token_file"] = str(self.root / "absent-token")
        self.save()
        loader = Mock(side_effect=AssertionError("credential loading forbidden"))
        self.assertIsNone(self.factory(credentials_loader=loader)())
        loader.assert_not_called()
        self.bindings.assert_not_called()

    def test_invalid_host_or_different_desktop_origin_is_unavailable_before_credentials(self):
        loader = Mock(side_effect=AssertionError("credential loading forbidden"))
        for key, value in (("native_host_id", "not-a-native-host"),
                           ("server_url", "http://127.0.0.1:19877")):
            with self.subTest(key=key):
                self.state_value["runtime"] = {"native_host_id": HOST, "server_url": ORIGIN}
                self.state_value["runtime"][key] = value
                self.save()
                with self.assertRaises(ValueError):
                    self.factory(credentials_loader=loader)()
        loader.assert_not_called()

    def test_private_carrier_requires_exact_version_and_bounded_explicit_fields(self):
        original = dict(self.carrier_value)
        for field, value in (("schema_version", True), ("schema_version", 1),
                             ("server_url", "https://example.invalid"),
                             ("bootstrap_token_file", "relative-token"),
                             ("source_ref", "relative-source"), ("owner_user", ""),
                             ("projects", ["unsafe/project"]), ("owner_user", "bad\towner"),
                             ("token", "not-accepted")):
            with self.subTest(field=field, value=value):
                self.carrier_value = dict(original, **{field: value})
                self.save()
                with self.assertRaises(ValueError):
                    self.factory()()
        self.carrier_value = original
        self.carrier_value.pop("source_ref")
        self.save()
        with self.assertRaises(ValueError):
            self.factory()()

    def test_duplicate_or_oversized_carrier_is_rejected(self):
        for raw in ('{"schema_version":2,"schema_version":2}', " " * 65537):
            self.carrier.write_text(raw, encoding="utf-8")
            with self.assertRaises(ValueError):
                self.factory()()

    def test_token_file_rotation_keeps_existing_queue_identity(self):
        context = self.factory()
        first = context()[0]
        database = self.root / "provider.sqlite3"
        bind_connection(database, first["origin"], first["connection_identity"])
        replacement = self.root / "new-private-token.txt"
        replacement.write_text("synthetic-rotated", encoding="utf-8")
        self.carrier_value["bootstrap_token_file"] = str(replacement)
        # Both accepted source-ref forms name the same canonical source.
        self.carrier_value["source_ref"] = self.carrier_value["source_ref"].removeprefix("desktop-local-bootstrap:")
        self.save()
        second = context()[0]
        self.assertEqual(first["connection_identity"], second["connection_identity"])
        self.assertEqual(first["global_connection_identity"], second["global_connection_identity"])
        self.assertEqual(first["manager_bindings"].host_id_provider(), HOST)
        self.assertEqual(second["fallback_api"].token, "synthetic-rotated")
        bind_connection(database, second["origin"], second["connection_identity"])

    def test_scope_change_invalidates_prepared_host_reader_and_existing_connection(self):
        original = dict(self.carrier_value)
        for field, value in (("owner_user", "other-owner"), ("projects", ["other-project"]),
                             ("source_ref", str(self.root / "different-source.env")),
                             ("server_url", "http://127.0.0.1:19877")):
            with self.subTest(field=field):
                self.carrier_value = dict(original)
                self.state_value["runtime"]["server_url"] = ORIGIN
                self.save()
                first = self.factory()()[0]
                database = self.root / (field + ".sqlite3")
                bind_connection(database, first["origin"], first["connection_identity"])
                self.carrier_value[field] = value
                if field == "server_url":
                    self.state_value["runtime"]["server_url"] = value
                self.save()
                with self.assertRaises(ValueError):
                    first["manager_bindings"].host_id_provider()
                second = self.factory()()[0]
                with self.assertRaises(ValueError):
                    bind_connection(database, second["origin"], second["connection_identity"])

    def test_desktop_host_clear_and_carrier_removal_invalidate_prepared_reader(self):
        first = self.factory()()[0]
        self.state_value["runtime"]["native_host_id"] = None
        self.save()
        self.assertIsNone(first["manager_bindings"].host_id_provider())
        self.assertIsNone(self.factory()())
        self.carrier.unlink()
        with self.assertRaises(OSError):
            first["manager_bindings"].host_id_provider()

    def test_context_clear_during_credential_load_never_constructs_api(self):
        def clear_during_load(**kwargs):
            self.state_value["runtime"]["native_host_id"] = None
            self.save()
            return ORIGIN, "synthetic-unused", "unused-temp-identity"
        api = Mock(side_effect=AssertionError("API construction forbidden"))
        self.assertIsNone(self.factory(credentials_loader=clear_during_load,
                                      fallback_api_factory=api)())
        api.assert_not_called()

    def test_default_bindings_implementation_is_lazy_and_not_manual_configuration(self):
        result = self.factory(bindings_factory=config.AutoManagerBindings.from_control)()[0]
        self.assertIsInstance(result["manager_bindings"], config.AutoManagerBindings)
        self.assertFalse((self.root / "provider").exists())

    def test_multiple_projects_use_separate_queues_and_shared_global_identity(self):
        self.carrier_value["projects"] = ["agent:runner:p1", "agent:runner:p2"]
        self.save()
        first, second = self.factory()()
        self.assertEqual(first["fallback_api"].project, "agent:runner:p1")
        self.assertEqual(second["member_api"].project, "agent:runner:p2")
        self.assertNotEqual(first["connection_identity"], second["connection_identity"])
        self.assertEqual(first["global_connection_identity"], second["global_connection_identity"])
        for item in (first, second):
            self.assertEqual(item["queue_runtime"], self.queue_path(item["fallback_api"].project))
            self.assertEqual(item["manager_bindings"].cache_path, item["queue_runtime"] / "manager-bindings.json")
            self.assertEqual(item["manager_bindings"].host_id_provider(), HOST)
        self.assertFalse((self.root / "provider").exists())

    def test_empty_scope_list_never_loads_credentials(self):
        self.carrier_value["projects"] = []
        self.save()
        loader = Mock(side_effect=AssertionError("credential loading forbidden"))
        self.assertIsNone(self.factory(credentials_loader=loader)())
        loader.assert_not_called()

    def test_project_lists_are_unique_bounded_and_typed(self):
        for projects in (None, "fixture-project", ["same", "same"], [""], [None], [[]],
                         ["x" * 201], ["bad\nproject"], [str(i) for i in range(129)]):
            with self.subTest(projects=projects):
                self.carrier_value["projects"] = projects
                self.save()
                with self.assertRaises(ValueError):
                    self.factory()()
        self.carrier_value["projects"] = [str(i) for i in range(128)]
        self.save()
        self.assertEqual(len(self.factory()()), 128)

    def test_scope_list_addition_preserves_old_callback_but_removal_revokes_it(self):
        first = self.factory()()[0]
        self.carrier_value["projects"].append("another-project")
        self.save()
        self.assertEqual(first["manager_bindings"].host_id_provider(), HOST)
        both = self.factory()()
        self.carrier_value["projects"] = ["another-project"]
        self.save()
        with self.assertRaises(ValueError):
            first["manager_bindings"].host_id_provider()
        self.assertEqual(both[1]["manager_bindings"].host_id_provider(), HOST)

    def test_matching_legacy_project_keeps_its_original_queue_files(self):
        legacy = self.legacy_identity()
        original = legacy.read_bytes()
        self.carrier_value["projects"].append("another-project")
        self.save()
        old, new = self.factory()()
        self.assertEqual(old["queue_runtime"], self.root / "provider")
        self.assertEqual(old["manager_bindings"].cache_path, self.root / "provider" / "manager-bindings.json")
        self.assertEqual(new["queue_runtime"], self.queue_path("another-project"))
        bind_connection(old["queue_runtime"] / "cli-fallback-worker.sqlite3",
                        old["origin"], old["connection_identity"])
        self.assertEqual(legacy.read_bytes(), original)

    def test_removed_legacy_project_does_not_allow_changed_owner_or_source(self):
        legacy = self.legacy_identity()
        original = legacy.read_bytes()
        self.carrier_value["projects"] = ["another-project"]
        self.save()
        self.assertEqual(self.factory()()[0]["queue_runtime"], self.queue_path("another-project"))
        for field, value in (("owner_user", "other-owner"),
                             ("source_ref", str(self.root / "other-source.env")),
                             ("server_url", "http://127.0.0.1:19877")):
            with self.subTest(field=field):
                saved = self.carrier_value[field]
                self.carrier_value[field] = value
                self.save()
                with self.assertRaises(ValueError):
                    self.factory()()
                self.carrier_value[field] = saved
        self.assertEqual(legacy.read_bytes(), original)

    def test_legacy_raw_source_and_json_format_keep_exact_original_identity(self):
        legacy = self.legacy_identity()
        raw_identity = json.dumps({
            "project": "fixture-project", "agent_id": None,
            "owner_user": self.carrier_value["owner_user"],
            "credential_source": self.carrier_value["source_ref"].removeprefix("desktop-local-bootstrap:")
        }, indent=2)
        legacy.write_text(json.dumps({"token_file": raw_identity, "endpoint": ORIGIN}, indent=4),
                          encoding="utf-8")
        original = legacy.read_bytes()
        self.carrier_value["projects"].append("another-project")
        self.save()
        old, new = self.factory()()
        self.assertEqual(old["queue_runtime"], self.root / "provider")
        self.assertEqual(old["connection_identity"], raw_identity)
        self.assertEqual(old["manager_bindings"].host_id_provider(), HOST)
        bind_connection(old["queue_runtime"] / "cli-fallback-worker.sqlite3",
                        old["origin"], old["connection_identity"])
        self.assertEqual(legacy.read_bytes(), original)
        self.assertEqual(new["queue_runtime"], self.queue_path("another-project"))
        self.assertEqual(json.loads(new["connection_identity"])["credential_source"],
                         config._source_reference(self.carrier_value["source_ref"]))
        self.assertEqual(new["global_connection_identity"], old["global_connection_identity"])
        # A token carrier and its actual Bootstrap source are not aliases.
        self.legacy_identity(credential_source=str(self.token))
        with self.assertRaises(ValueError):
            self.factory()()

    def test_malformed_legacy_identity_is_not_bypassed_by_new_scope_or_empty_list(self):
        path = self.legacy_identity()
        valid = json.loads(path.read_text())
        inner = json.loads(valid["token_file"])
        invalid = [" " * 16385, '{"endpoint":"x","endpoint":"x"}', "[]",
                   json.dumps({**valid, "extra": True}), json.dumps({**valid, "token_file": {}}),
                   json.dumps({**valid, "token_file": "not-json"}),
                   json.dumps({**valid, "token_file": '{"project":"a","project":"b"}'})]
        for changes in ({"agent_id": "an-agent"}, {"project": "unsafe/project"},
                        {"credential_source": "relative"}, {"unknown": True}):
            invalid.append(json.dumps({**valid, "token_file": json.dumps({**inner, **changes})}))
        for projects in (["another-project"], []):
            self.carrier_value["projects"] = projects
            self.save()
            for raw in invalid:
                with self.subTest(projects=projects, raw=raw[:80]):
                    path.write_text(raw, encoding="utf-8")
                    with self.assertRaises(ValueError):
                        self.factory()()

    def test_prepared_callback_rechecks_legacy_identity(self):
        path = self.legacy_identity()
        first = self.factory()()[0]
        self.legacy_identity(owner_user="other-owner")
        with self.assertRaises(ValueError):
            first["manager_bindings"].host_id_provider()
        path.unlink()
        with self.assertRaises(ValueError):
            first["manager_bindings"].host_id_provider()

    def test_global_connection_rejects_new_owner_even_on_another_project(self):
        first = self.factory()()[0]
        database = self.root / "provider" / "automatic-dispatch.sqlite3"
        bind_connection(database, first["origin"], first["global_connection_identity"])
        self.carrier_value.update(owner_user="other-owner", projects=["another-project"])
        self.save()
        second = self.factory()()[0]
        with self.assertRaises(ValueError):
            bind_connection(database, second["origin"], second["global_connection_identity"])


if __name__ == "__main__":
    unittest.main()
