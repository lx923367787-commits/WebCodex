"""Local child-task thinking policy; never creates or authenticates a Goal.

The scheduler must first validate a real Goal handoff through its existing
Server path. A registration only associates local immutable task evidence with
the requested project/Goal names; those names never establish authority.
"""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
import re
import uuid

from edge_tools import read_json, write_json
from handoff_controller import watcher_lock


DEFAULT_RUNTIME = Path(__file__).resolve().parent.parent / "chat-agents" / "runtime"
MAX_PROFILE_BYTES = 8192
MAX_PROFILE_DEPTH = 8
MAX_PROFILE_NODES = 256
REGISTRATION_STATE = "awaiting_real_goal_handoff_validation"
_BINDING_FIELDS = {
    "schema_version", "project", "goal_id", "request_id", "task_id",
    "thinking_profile", "request_sha256", "profile_sha256", "registration_state",
}


class PolicyError(ValueError):
    """Controlled local validation code, never raw content or credentials."""


def _identity(value, maximum, label):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1," + str(maximum) + r"}", value):
        raise PolicyError("invalid_" + label)
    return value


def _request_id(value):
    try:
        if not isinstance(value, str) or str(uuid.UUID(value)) != value:
            raise ValueError()
    except (TypeError, AttributeError, ValueError):
        raise PolicyError("invalid_request_id") from None
    return value


def _encoded(value):
    try:
        return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False,
                          separators=(",", ":")).encode("utf-8")
    except (TypeError, ValueError, UnicodeError, RecursionError):
        raise PolicyError("invalid_json_value") from None


def _digest(value):
    return hashlib.sha256(_encoded(value)).hexdigest()


def _profile(value):
    if value is None:
        return None
    if not isinstance(value, dict):
        raise PolicyError("thinking_profile_required")
    profile_id = value.get("profile_id")
    if (not isinstance(profile_id, str) or not profile_id.strip()
            or len(profile_id) > 128 or "\0" in profile_id):
        raise PolicyError("invalid_thinking_profile_id")
    count = 0

    def visit(item, depth):
        nonlocal count
        count += 1
        if count > MAX_PROFILE_NODES or depth > MAX_PROFILE_DEPTH:
            raise PolicyError("thinking_profile_too_large")
        if isinstance(item, dict):
            for key, child in item.items():
                if not isinstance(key, str):
                    raise PolicyError("invalid_thinking_profile")
                visit(key, depth + 1)
                visit(child, depth + 1)
        elif isinstance(item, list):
            for child in item:
                visit(child, depth + 1)
        elif item is None or isinstance(item, (str, bool, int)):
            return
        elif isinstance(item, float) and math.isfinite(item):
            return
        else:
            raise PolicyError("invalid_thinking_profile")

    visit(value, 0)
    encoded = _encoded(value)
    if len(encoded) > MAX_PROFILE_BYTES:
        raise PolicyError("thinking_profile_too_large")
    return json.loads(encoded)


def _path(root, *parts):
    path = root.joinpath(*parts)
    if path.is_symlink() or not path.resolve().is_relative_to(root):
        raise PolicyError("policy_path_rejected")
    return path


def _load(path, missing_code):
    try:
        return read_json(path)
    except FileNotFoundError:
        raise PolicyError(missing_code) from None
    except (OSError, ValueError, UnicodeError, RecursionError):
        raise PolicyError("invalid_local_policy_record") from None


def _binding_path(root, project, goal_id):
    _identity(project, 512, "project")
    _identity(goal_id, 256, "goal_id")
    name = hashlib.sha256((project + "\0" + goal_id).encode("utf-8")).hexdigest()
    return _path(root, "goal-bindings", name + ".json")


def _task_evidence(root, request_id):
    request_id = _request_id(request_id)
    if _path(root, "tasks", request_id, "cancel.json").exists():
        raise PolicyError("chat_task_cancelled")
    request = _load(_path(root, "tasks", request_id, "request.json"), "chat_task_request_missing")
    config = request.get("config")
    task_id = "wc_chat_" + uuid.UUID(request_id).hex
    if not isinstance(config, dict) or config.get("launch_id") != request_id or config.get("task_id") != task_id:
        raise PolicyError("chat_task_identity_mismatch")
    profile = _profile(config.get("thinking_profile"))
    browser = _load(_path(root, "evidence", request_id, "browser.json"), "chat_task_browser_evidence_missing")
    if browser.get("launch_id") != request_id or browser.get("task_id") != task_id:
        raise PolicyError("chat_task_browser_identity_mismatch")
    actual_profile = _profile(browser.get("thinking_profile"))
    adjustment = browser.get("thinking_adjustment", {}).get("status")
    if profile is None or adjustment in {"not_requested", "not_adjusted"}:
        # Carry the lack of an applied override, not an invented page value.
        profile = None
    elif actual_profile is None or actual_profile["profile_id"] != profile["profile_id"]:
        raise PolicyError("chat_task_profile_not_confirmed")
    if profile is not None and profile.get("mode") == "default":
        if actual_profile.get("mode") != "default" or not isinstance(actual_profile.get("observed_profile"), dict):
            raise PolicyError("chat_task_profile_not_confirmed")
        profile = actual_profile
    if _path(root, "tasks", request_id, "cancel.json").exists():
        raise PolicyError("chat_task_cancelled")
    return {"request_id": request_id, "task_id": task_id, "thinking_profile": profile,
            "request_sha256": _digest(request), "profile_sha256": _digest(profile)}


def _read_binding(path, project, goal_id):
    record = _load(path, "goal_binding_missing")
    if (set(record) != _BINDING_FIELDS or type(record.get("schema_version")) is not int
            or record["schema_version"] != 1 or record.get("project") != project
            or record.get("goal_id") != goal_id or record.get("registration_state") != REGISTRATION_STATE):
        raise PolicyError("invalid_goal_binding")
    return record


def _verify_binding(record, evidence):
    if any(record.get(key) != value for key, value in evidence.items()):
        raise PolicyError("goal_binding_evidence_changed")


def _projection(record):
    return {key: record[key] for key in
            ("registration_state", "project", "goal_id", "request_id", "task_id", "thinking_profile")}


def register_goal_binding(request_id, project, goal_id, runtime=DEFAULT_RUNTIME):
    """Register local policy only; Goal existence and handoff stay unverified."""
    root = Path(runtime).resolve()
    _request_id(request_id)
    path = _binding_path(root, project, goal_id)
    if not root.is_dir():
        raise PolicyError("chat_task_request_missing")
    with watcher_lock(root / "registry"):
        evidence = _task_evidence(root, request_id)
        if path.exists():
            record = _read_binding(path, project, goal_id)
            if record.get("request_id") != request_id:
                raise PolicyError("goal_binding_conflict")
            _verify_binding(record, evidence)
            return dict(_projection(record), replayed=True)
        record = {"schema_version": 1, "registration_state": REGISTRATION_STATE,
                  "project": project, "goal_id": goal_id, **evidence}
        write_json(path, record)
        return dict(_projection(record), replayed=False)


def resolve_goal_profile(project, goal_id, runtime=DEFAULT_RUNTIME):
    """Resolve only after the caller validates a fresh real Server handoff.

    No registration returns None for the main-agent default. A registered but
    missing, changed or cancelled child fails closed instead of losing policy.
    """
    root = Path(runtime).resolve()
    path = _binding_path(root, project, goal_id)
    if not root.is_dir():
        return None
    with watcher_lock(root / "registry"):
        if not path.exists():
            return None
        record = _read_binding(path, project, goal_id)
        evidence = _task_evidence(root, record.get("request_id"))
        _verify_binding(record, evidence)
        return _projection(record)
