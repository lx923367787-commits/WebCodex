"""Read only the explicit, single-Host control carrier supplied by its launcher.

Creating a factory performs no file access, credential loading or HTTP request.
The native provider's coordinator invokes it after startup. Ordinary Service
construction and tool calls never discover management credentials implicitly.
"""
from __future__ import annotations

import json
import hashlib
import os
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent / "chat-handoff"))
from cli_fallback_control_api import FallbackControlApi
from handoff_control_api import load_credentials
from handoff_controller import endpoint, identifier
from host_enrollment import ensure_no_launch_grant
from manager_bindings import AutoManagerBindings, _unique_object
from run_auto_handoff import DEFAULT_STATE
from team_agent_dispatch import TeamAgentControlApi

_CARRIER_FIELDS = {"schema_version", "server_url", "bootstrap_token_file", "source_ref",
                   "owner_user", "projects"}
_SOURCE_PREFIX = "desktop-local-bootstrap:"


def _read_object(path, limit=262144):
    path = Path(path)
    if not path.is_absolute():
        raise ValueError("automatic_control_path_must_be_absolute")
    with path.open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise ValueError("automatic_control_config_too_large")
    value = json.loads(data.decode("utf-8-sig"), object_pairs_hook=_unique_object)
    if not isinstance(value, dict):
        raise ValueError("automatic_control_config_invalid")
    return value


def _source_reference(value):
    """Normalize identity text only; never open the Bootstrap source file."""
    path = value.removeprefix(_SOURCE_PREFIX)
    if path.startswith("\\\\?\\UNC\\"):
        path = "\\\\" + path[8:]
    elif path.startswith("\\\\?\\"):
        path = path[4:]
    if not Path(path).is_absolute():
        raise ValueError("automatic_control_source_must_be_absolute")
    return _SOURCE_PREFIX + os.path.normcase(os.path.normpath(path))


def _read_carrier(path):
    value = _read_object(path, 65536)
    ensure_no_launch_grant(value)
    if (set(value) != _CARRIER_FIELDS or type(value["schema_version"]) is not int
            or value["schema_version"] != 2):
        raise ValueError("automatic_control_config_invalid")
    for field in _CARRIER_FIELDS - {"schema_version", "projects"}:
        text = value[field]
        if (not isinstance(text, str) or not text.strip() or len(text) > 32768
                or any(ord(char) < 32 or ord(char) == 127 for char in text)):
            raise ValueError("automatic_control_config_invalid")
    projects = value["projects"]
    if (not isinstance(projects, list) or len(projects) > 128
            or len(value["owner_user"]) > 256):
        raise ValueError("automatic_control_config_invalid")
    for project in projects:
        identifier(project)
    if len(set(projects)) != len(projects):
        raise ValueError("automatic_control_config_invalid")
    origin = endpoint(value["server_url"])
    if not Path(value["bootstrap_token_file"]).is_absolute():
        raise ValueError("automatic_control_path_must_be_absolute")
    source = _source_reference(value["source_ref"])
    environ = {"WEBCODEX_HANDOFF_CONTROL_ENDPOINT": origin,
               "WEBCODEX_HANDOFF_CONTROL_TOKEN_FILE": value["bootstrap_token_file"],
               "WEBCODEX_HANDOFF_OWNER_USER": value["owner_user"]}
    # The temporary file is only a credential carrier. Its rotation does not
    # change which source, owner and project own existing queue checkpoints.
    identity = {"credential_source": source, "owner_user": value["owner_user"], "agent_id": None}
    return origin, identity, environ, tuple(projects)


def _legacy_binding(runtime, origin, identity):
    """Preserve the original queue namespace and reject an unrelated owner."""
    path = runtime / "cli-fallback-worker.identity.json"
    if not path.exists():
        return None
    value = _read_object(path, 16384)
    ensure_no_launch_grant(value)
    if (set(value) != {"endpoint", "token_file"} or value["endpoint"] != origin
            or not isinstance(value["token_file"], str)):
        raise ValueError("automatic_control_legacy_identity_invalid")
    saved = json.loads(value["token_file"], object_pairs_hook=_unique_object)
    if (not isinstance(saved, dict)
            or set(saved) != {"credential_source", "owner_user", "project", "agent_id"}
            or saved["agent_id"] is not None
            or not isinstance(saved["credential_source"], str)):
        raise ValueError("automatic_control_legacy_identity_invalid")
    project = identifier(saved.pop("project"))
    saved["credential_source"] = _source_reference(saved["credential_source"])
    if saved != identity:
        raise ValueError("automatic_control_context_changed")
    # Keep the exact persisted identity for bind_connection's string equality.
    # Normalization above validates scope; it never rewrites legacy evidence.
    return project, value["token_file"]


def read_native_host_id(desktop_state, expected_origin):
    """Read C's owned-ready metadata; never open user_token_file or relay auth."""
    raw = _read_object(desktop_state)
    runtime = raw.get("runtime")
    if not isinstance(runtime, dict):
        return None
    host_id = runtime.get("native_host_id")
    if host_id is None:
        return None
    if (not isinstance(host_id, str) or re.fullmatch(r"wc_host_[0-9a-f]{32}", host_id) is None
            or endpoint(runtime.get("server_url", "")) != expected_origin):
        raise ValueError("automatic_host_context_mismatch")
    return host_id


def load_context_factory(runtime, *, desktop_state=DEFAULT_STATE, control_config=None,
                         credentials_loader=load_credentials,
                         fallback_api_factory=FallbackControlApi,
                         member_api_factory=TeamAgentControlApi,
                         bindings_factory=AutoManagerBindings.from_control,
                         scheduler_runtime=None):
    """Return a lazy factory. No carrier means no automatic queue admission.

    Credentials come only from C's private six-field carrier. Desktop metadata
    supplies the owned ready Host; Root decides its actual current authority.
    """
    runtime, desktop_state = Path(runtime), Path(desktop_state)
    scheduler = (ROOT.parent / "chat-handoff" / "runtime" if scheduler_runtime is None
                 else Path(scheduler_runtime))
    carrier = Path(control_config) if control_config is not None else None
    if (not runtime.is_absolute() or not desktop_state.is_absolute() or not scheduler.is_absolute()
            or carrier is not None and not carrier.is_absolute()):
        raise ValueError("automatic_control_path_must_be_absolute")

    def context():
        if carrier is None:
            return None
        origin, identity, environ, projects = _read_carrier(carrier)
        legacy_binding = _legacy_binding(runtime, origin, identity)
        legacy_project = legacy_binding[0] if legacy_binding is not None else None
        if not projects or read_native_host_id(desktop_state, origin) is None:
            return None
        # Credentials are loaded once, using one explicitly registered scope.
        loaded_origin, token, _ = credentials_loader(
            environ={**environ, "WEBCODEX_HANDOFF_PROJECT": projects[0]})
        if loaded_origin != origin:
            raise ValueError("automatic_control_context_changed")
        contexts = []
        for project in projects:
            def host_id_provider(project=project):
                current_origin, current_identity, _, current_projects = _read_carrier(carrier)
                if ((current_origin, current_identity) != (origin, identity)
                        or project not in current_projects):
                    raise ValueError("automatic_control_context_changed")
                if _legacy_binding(runtime, origin, identity) != legacy_binding:
                    raise ValueError("automatic_control_context_changed")
                return read_native_host_id(desktop_state, origin)

            # Detect teardown or scope changes during credential loading too.
            if host_id_provider() is None:
                return None
            queue_runtime = (runtime if project == legacy_project else
                             runtime / "project-queues" / hashlib.sha256(project.encode("utf-8")).hexdigest())
            scoped_environ = {**environ, "WEBCODEX_HANDOFF_PROJECT": project}
            fallback = fallback_api_factory(origin, token, timeout=5, environ=scoped_environ)
            member = member_api_factory(origin, token, timeout=5, environ=scoped_environ)
            bindings = bindings_factory(fallback, cache_path=queue_runtime / "manager-bindings.json",
                                        host_id_provider=host_id_provider)
            contexts.append({"fallback_api": fallback, "member_api": member, "manager_bindings": bindings,
                "origin": origin, "connection_identity": (legacy_binding[1] if project == legacy_project else
                    json.dumps({**identity, "project": project}, sort_keys=True)),
                "global_connection_identity": json.dumps(identity, sort_keys=True),
                "queue_runtime": queue_runtime, "scheduler_runtime": scheduler})
        return contexts

    return context
