"""Exact Manager receipts and an atomic, non-authoritative local cache.

This file is configuration, not authority. Each Root control request rechecks
the registered host, active run, generation and assigned root in Store. Values
must come from the original attach/status or the exact Manager handoff receipt;
there is deliberately no latest-manager discovery or model credential input.
"""
from __future__ import annotations

import json
from pathlib import Path
import re
import time

from host_enrollment import ensure_no_launch_grant
from handoff_controller import watcher_lock
from edge_tools import write_json


_FIELDS = {"root_task_id", "host_id", "manager_agent_id", "manager_run_id", "expected_generation"}
_MAX_BYTES = 1024 * 1024
_AUTOMATIC_FIELDS = {"root_task_id", "host_id", "manager_agent_id", "manager_run_id",
    "generation", "owner_user", "project", "enrollment_id", "native_host_generation",
    "run_expires_at_ms", "window_binding_ref"}


def _identifier(value):
    if not isinstance(value, str) or re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", value) is None:
        raise ValueError("invalid_manager_binding")
    return value


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate_manager_binding_field")
        result[key] = value
    return result


class RootManagerBindings:
    """Explicit file reader retained for fixture and diagnostic use."""
    def __init__(self, value, *, owner_user, project):
        ensure_no_launch_grant(value)
        if (not isinstance(value, dict) or set(value) != {"owner_user", "project", "roots"}
                or value["owner_user"] != owner_user or value["project"] != project
                or not isinstance(value["roots"], list) or len(value["roots"]) > 4096):
            raise ValueError("invalid_manager_bindings_scope")
        self._roots = {}
        self._source = None
        for row in value["roots"]:
            if not isinstance(row, dict) or set(row) != _FIELDS:
                raise ValueError("invalid_manager_binding")
            selected = {key: _identifier(row[key]) for key in _FIELDS - {"expected_generation"}}
            generation = row["expected_generation"]
            if type(generation) is not int or not 1 <= generation <= (1 << 63) - 1:
                raise ValueError("invalid_manager_generation")
            selected["expected_generation"] = generation
            root = selected.pop("root_task_id")
            if root in self._roots:
                raise ValueError("duplicate_manager_root")
            self._roots[root] = selected

    @classmethod
    def from_file(cls, path, *, owner_user, project):
        path = Path(path)
        if not path.is_absolute():
            raise ValueError("manager_bindings_path_must_be_absolute")
        with path.open("rb") as source:
            raw = source.read(_MAX_BYTES + 1)
        if len(raw) > _MAX_BYTES:
            raise ValueError("manager_bindings_too_large")
        try:
            value = json.loads(raw.decode("utf-8-sig"), object_pairs_hook=_unique_object)
        except (ValueError, UnicodeError):
            raise ValueError("invalid_manager_bindings_file") from None
        result = cls(value, owner_user=owner_user, project=project)
        result._source = (path, owner_user, project)
        return result

    def for_root(self, root_task_id):
        root = _identifier(root_task_id)
        roots = self._roots
        if self._source is not None:
            # Only the trusted writer replaces this exact scoped receipt file.
            # A prepared dispatch captures its own copy, so a later handoff
            # update applies to a new observation, never to an in-flight send.
            path, owner, project = self._source
            roots = type(self).from_file(path, owner_user=owner, project=project)._roots
        selected = roots.get(root)
        if selected is None:
            raise ValueError("exact_manager_binding_required")
        return dict(selected)


class ManagerBindingCache:
    """CAS the original five-field projection, never create a new ledger."""
    def __init__(self, path, *, owner_user, project):
        self.path = Path(path)
        if not self.path.is_absolute():
            raise ValueError("manager_bindings_path_must_be_absolute")
        if (not isinstance(owner_user, str) or not owner_user.strip() or len(owner_user) > 256
                or not isinstance(project, str) or not project.strip() or len(project) > 512
                or any(ord(char) < 32 or ord(char) == 127 for char in owner_user + project)):
            raise ValueError("invalid_manager_bindings_scope")
        ensure_no_launch_grant({"owner_user": owner_user, "project": project})
        self.owner_user, self.project = owner_user, project

    def store(self, root_task_id, binding):
        root = _identifier(root_task_id)
        if not isinstance(binding, dict) or set(binding) != _FIELDS - {"root_task_id"}:
            raise ValueError("invalid_manager_binding")
        incoming = RootManagerBindings({"owner_user": self.owner_user, "project": self.project,
            "roots": [{"root_task_id": root, **binding}]},
            owner_user=self.owner_user, project=self.project)._roots[root]
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with watcher_lock(self.path):
            roots = (RootManagerBindings.from_file(self.path, owner_user=self.owner_user,
                                                  project=self.project)._roots
                     if self.path.exists() else {})
            previous = roots.get(root)
            if previous is not None:
                if (previous["host_id"] != incoming["host_id"]
                        or previous["manager_agent_id"] != incoming["manager_agent_id"]):
                    raise ValueError("manager_binding_identity_conflict")
                old_generation, new_generation = previous["expected_generation"], incoming["expected_generation"]
                if new_generation < old_generation:
                    raise ValueError("manager_binding_stale")
                if new_generation == old_generation:
                    if incoming != previous:
                        raise ValueError("manager_binding_generation_conflict")
                    return dict(previous)
                if incoming["manager_run_id"] == previous["manager_run_id"]:
                    raise ValueError("manager_binding_generation_conflict")
            roots[root] = dict(incoming)
            value = {"owner_user": self.owner_user, "project": self.project,
                     "roots": [{"root_task_id": key, **row} for key, row in sorted(roots.items())]}
            RootManagerBindings(value, owner_user=self.owner_user, project=self.project)
            if len(json.dumps(value, ensure_ascii=False).encode("utf-8")) > _MAX_BYTES:
                raise ValueError("manager_bindings_too_large")
            write_json(self.path, value)
            return dict(incoming)


class AutoManagerBindings:
    """Read B's exact active Manager projection on every access, then cache it.

The Bootstrap getter owns role/current-run/window authorization. Its real DTO
has no invented active or purpose booleans; null is unavailable. This reader
checks the exact query scope, real identifier formats and current run expiry.
The cache is never an authorization fallback, including during network failure.
"""
    def __init__(self, api, cache, host_id_provider):
        if not callable(getattr(api, "post", None)) or not callable(host_id_provider):
            raise ValueError("invalid_manager_binding_control")
        self.api, self.cache, self.host_id_provider = api, cache, host_id_provider
        self._scope = (getattr(api, "origin", None), getattr(api, "owner_user", None),
                       getattr(api, "project", None))
        if self._scope[1:] != (cache.owner_user, cache.project):
            raise ValueError("invalid_manager_bindings_scope")

    @classmethod
    def from_control(cls, api, *, cache_path, host_id_provider):
        cache = ManagerBindingCache(cache_path, owner_user=getattr(api, "owner_user", None),
                                    project=getattr(api, "project", None))
        return cls(api, cache, host_id_provider)

    def _context(self):
        scope = (getattr(self.api, "origin", None), getattr(self.api, "owner_user", None),
                 getattr(self.api, "project", None))
        if scope != self._scope:
            raise ValueError("manager_binding_context_changed")
        try:
            host_id = self.host_id_provider()
        except Exception:
            raise ValueError("manager_host_context_unavailable") from None
        if not isinstance(host_id, str) or re.fullmatch(r"wc_host_[0-9a-f]{32}", host_id) is None:
            raise ValueError("manager_host_context_unavailable")
        return host_id

    def _projection(self, value, root, host_id):
        if not isinstance(value, dict) or set(value) != _AUTOMATIC_FIELDS:
            raise ValueError("invalid_manager_binding_projection")
        ensure_no_launch_grant(value)
        if (value["owner_user"] != self.cache.owner_user or value["project"] != self.cache.project
                or value["root_task_id"] != root or value["host_id"] != host_id):
            raise ValueError("manager_binding_scope_mismatch")
        for field, pattern in (("manager_agent_id", r"wc_iagent_[0-9a-f]{32}"),
                               ("manager_run_id", r"wc_agent_run_[0-9a-f]{32}"),
                               ("enrollment_id", r"wc_enrollment_[0-9a-f]{32}"),
                               ("window_binding_ref", r"wc_host_binding_[0-9a-f]{64}")):
            if not isinstance(value[field], str) or re.fullmatch(pattern, value[field]) is None:
                raise ValueError("invalid_manager_binding_projection")
        for field in ("generation", "native_host_generation", "run_expires_at_ms"):
            if type(value[field]) is not int or not 1 <= value[field] <= (1 << 63) - 1:
                raise ValueError("invalid_manager_binding_projection")
        if value["run_expires_at_ms"] <= int(time.time() * 1000):
            raise ValueError("manager_binding_run_expired")
        # NativeHost credential generation is metadata, not Agent generation.
        # Enrollment grant expiry is deliberately absent from this B6 DTO.
        return {"host_id": host_id, "manager_agent_id": value["manager_agent_id"],
                "manager_run_id": value["manager_run_id"], "expected_generation": value["generation"]}

    def for_root(self, root_task_id):
        root = _identifier(root_task_id)
        ensure_no_launch_grant(root)
        host_id = self._context()
        payload = {"action": "agent_host_manager_binding_read", "owner_user": self.cache.owner_user,
                   "project": self.cache.project, "root_task_id": root, "host_id": host_id}
        try:
            response = self.api.post(payload)
        except Exception:
            raise ValueError("manager_binding_lookup_failed") from None
        if self._context() != host_id:
            raise ValueError("manager_binding_context_changed")
        if not isinstance(response, dict) or set(response) != {"binding"}:
            raise ValueError("invalid_manager_binding_response")
        if response["binding"] is None:
            raise ValueError("manager_binding_unavailable")
        projection = self._projection(response["binding"], root, host_id)
        result = self.cache.store(root, projection)
        # Do not return an earlier observation if context clears or the run
        # expires while its non-authoritative cache is being persisted.
        if self._context() != host_id:
            raise ValueError("manager_binding_context_changed")
        self._projection(response["binding"], root, host_id)
        return result

    def validate_for_delivery(self, root_task_id, expected_manager_binding):
        """Recheck before claim; never retarget a previously prepared callback."""
        root = _identifier(root_task_id)
        if (not isinstance(expected_manager_binding, dict)
                or set(expected_manager_binding) != _FIELDS - {"root_task_id"}):
            raise ValueError("invalid_manager_binding")
        expected = RootManagerBindings({"owner_user": self.cache.owner_user, "project": self.cache.project,
            "roots": [{"root_task_id": root, **expected_manager_binding}]},
            owner_user=self.cache.owner_user, project=self.cache.project)._roots[root]
        if self.for_root(root) != expected:
            raise ValueError("manager_binding_changed_before_delivery")
