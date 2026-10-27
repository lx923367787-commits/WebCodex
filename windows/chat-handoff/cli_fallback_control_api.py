"""Narrow trusted CLI-recovery control client; credentials never reach a chat."""
from __future__ import annotations

import json
import os
from pathlib import Path
import uuid

from handoff_control_api import Api
import handoff_controller as hc


class CandidatePageCursor:
    """Checkpoint a completed discovery page, never a dispatch permission.

    A crash before finish() replays that page; durable Server claims still
    prevent duplicate sends. Both queues have independent cursor files under
    the worker's existing endpoint/owner/project connection binding.
    """
    def __init__(self, path=None):
        self.path = Path(path) if path is not None else None
        self.cursor = None
        self.pending = None
        self.has_pending = False
        if self.path is not None and self.path.exists():
            with self.path.open("rb") as source:
                raw = source.read(2049)
            if len(raw) > 2048:
                raise ValueError("invalid_candidate_cursor_checkpoint")
            saved = json.loads(raw)
            if not isinstance(saved, dict) or set(saved) != {"cursor"}:
                raise ValueError("invalid_candidate_cursor_checkpoint")
            self.cursor = self.validate(saved["cursor"])

    @staticmethod
    def validate(cursor):
        if cursor is not None and (not isinstance(cursor, str) or not cursor
                or len(cursor.encode("utf-8")) > 512):
            raise ValueError("invalid_candidate_cursor")
        return cursor

    def request(self):
        return {} if self.cursor is None else {"cursor": self.cursor}

    def stage(self, body):
        if not isinstance(body, dict) or "next_cursor" not in body:
            raise ValueError("candidate_pagination_unavailable")
        self.pending = self.validate(body["next_cursor"])
        self.has_pending = True

    def finish(self):
        if not self.has_pending:
            return
        if self.path is not None:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            temporary = self.path.with_name(self.path.name + "." + uuid.uuid4().hex + ".tmp")
            try:
                with temporary.open("x", encoding="utf-8") as target:
                    json.dump({"cursor": self.pending}, target)
                    target.flush()
                    os.fsync(target.fileno())
                temporary.replace(self.path)
            finally:
                if temporary.exists():
                    temporary.unlink()
        self.cursor = self.pending
        self.has_pending = False


def discovery_cursor(api):
    if not hasattr(api, "_candidate_cursor"):
        api._candidate_cursor = CandidatePageCursor()
    return api._candidate_cursor


class FallbackControlApi(Api):
    def _call(self, action, **fields):
        return self.post({"action": "cli_fallback_" + action,
                          "owner_user": self.owner_user, "project": self.project, **fields})

    def discover(self):
        cursor = discovery_cursor(self)
        body = self._call("list", limit=100, **cursor.request())
        candidates = body.get("candidates")
        if not isinstance(candidates, list) or len(candidates) > 100:
            raise hc.ReadFailure()
        result = []
        for candidate in candidates:
            if not isinstance(candidate, dict) or candidate.get("project") != self.project:
                raise hc.ReadFailure()
            fallback_id = hc.identifier(candidate.get("fallback_id"))
            if candidate.get("state") not in {"queued", "starting", "running", "unknown"}:
                raise hc.ReadFailure()
            result.append({"fallback_id": fallback_id, "state": candidate["state"]})
        if len({item["fallback_id"] for item in result}) != len(result):
            raise hc.ReadFailure()
        cursor.stage(body)
        return result

    def finish_discovery(self):
        discovery_cursor(self).finish()

    def acquire(self, fallback_id):
        return self._call("acquire", fallback_id=hc.identifier(fallback_id))

    def read(self, fallback_id):
        return self._call("read", fallback_id=hc.identifier(fallback_id))

    def record(self, *, fallback_id, dispatch_attempt_id, request_id, state, reason, response_observed=False):
        if state not in {"dispatched", "busy", "unknown", "cancel_requested", "profile_unavailable"}:
            raise ValueError("invalid_cli_fallback_observation")
        if type(response_observed) is not bool:
            raise ValueError("invalid_cli_fallback_observation")
        return self._call("record", fallback_id=hc.identifier(fallback_id),
                          dispatch_attempt_id=hc.identifier(dispatch_attempt_id),
                          request_id=request_id, state=state, reason=reason,
                          response_observed=response_observed)
