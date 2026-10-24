"""Explicit local control connection; never falls back to a model/user token."""
from __future__ import annotations

import json
import os
from pathlib import Path
import urllib.error
import urllib.request

import handoff_controller as hc

CONTROL_PATH = "/api/admin/agent-isolation"


def load_credentials(environ=None):
    values = os.environ if environ is None else environ
    origin = hc.endpoint(values.get("WEBCODEX_HANDOFF_CONTROL_ENDPOINT", ""))
    token = values.get("WEBCODEX_HANDOFF_CONTROL_TOKEN", "")
    token_file = values.get("WEBCODEX_HANDOFF_CONTROL_TOKEN_FILE", "")
    if bool(token) == bool(token_file):
        raise ValueError("Configure exactly one explicit handoff control credential")
    if token_file:
        path = Path(token_file)
        if not path.is_absolute():
            raise ValueError("Handoff control credential path must be absolute")
        with path.open("rb") as source:
            data = source.read(16385)
        if len(data) > 16384:
            raise ValueError("Invalid handoff control credential")
        token = data.decode("utf-8-sig").strip()
        identity = str(path.resolve())
    else:
        identity = "env:WEBCODEX_HANDOFF_CONTROL_TOKEN"
    if not token or len(token) > 16384 or any(c in token for c in ("\r", "\n", "\0")):
        raise ValueError("Invalid handoff control credential")
    # Explicit targets are part of the local connection identity. A changed
    # target cannot silently reuse another scheduler database.
    owner, project, agent = targets(values)
    identity = json.dumps({"credential_source": identity, "owner_user": owner,
        "project": project, "agent_id": agent}, sort_keys=True)
    return origin, token, identity


def targets(values):
    owner = values.get("WEBCODEX_HANDOFF_OWNER_USER", "")
    if not isinstance(owner, str) or not owner.strip() or len(owner) > 256 or any(c in owner for c in ("\r", "\n", "\0")):
        raise ValueError("Explicit handoff owner_user is required")
    project = hc.identifier(values.get("WEBCODEX_HANDOFF_PROJECT"))
    agent = values.get("WEBCODEX_HANDOFF_AGENT_ID") or None
    if agent is not None:
        hc.identifier(agent)
    return owner, project, agent


class Api(hc.Api):
    def __init__(self, origin, token, timeout=10, *, environ=None):
        super().__init__(origin, token, timeout)
        self.owner_user, self.project, self.agent_id = targets(os.environ if environ is None else environ)
        self.agents = {}

    def post(self, payload):
        request = urllib.request.Request(self.origin + CONTROL_PATH, json.dumps(payload).encode(),
            {"Authorization": "Bearer " + self.token, "Content-Type": "application/json"})
        try:
            with self.opener.open(request, timeout=self.timeout) as response:
                raw = response.read(hc.MAX_RESPONSE + 1)
            if len(raw) > hc.MAX_RESPONSE:
                raise hc.ReadFailure()
            body = json.loads(raw)
            if not isinstance(body, dict) or "error" in body:
                raise hc.ReadFailure()
            return body
        except urllib.error.HTTPError as error:
            error.close()
            raise hc.ReadFailure({401: "connector_auth", 403: "connector_auth", 429: "rate_limit"}.get(error.code, "unknown")) from None
        except (OSError, urllib.error.URLError, ValueError):
            raise hc.ReadFailure() from None

    def discover(self):
        payload = {"action": "handoff_list", "owner_user": self.owner_user, "project": self.project, "limit": 100}
        if self.agent_id:
            payload["agent_id"] = self.agent_id
        body = self.post(payload)
        if not isinstance(body.get("candidates"), list):
            raise hc.ReadFailure()
        candidates = []
        for raw in body["candidates"]:
            try:
                if not isinstance(raw, dict) or raw.get("project") != self.project:
                    raise ValueError()
                c = {key: hc.identifier(raw.get(key)) for key in ("project", "goal_id", "agent_id", "source_context_epoch")}
                if self.agent_id and c["agent_id"] != self.agent_id:
                    raise ValueError()
                if not hc.number(raw.get("checkpoint_sequence")):
                    raise ValueError()
                c["checkpoint_sequence"] = raw["checkpoint_sequence"]
                self.agents[c["goal_id"]] = c["agent_id"]
                candidates.append(c)
            except ValueError:
                raise hc.ReadFailure() from None
        return candidates

    def identity(self, goal_id, agent_id=None):
        selected = agent_id or self.agents.get(goal_id) or self.agent_id
        try:
            hc.identifier(goal_id)
            hc.identifier(selected)
            if self.agent_id and selected != self.agent_id:
                raise ValueError()
        except ValueError:
            raise hc.ReadFailure() from None
        self.agents[goal_id] = selected
        return {"owner_user": self.owner_user, "project": self.project, "agent_id": selected, "goal_id": goal_id}

    def read(self, config):
        if config.get("project") != self.project:
            raise hc.ReadFailure()
        return self.post({"action": "handoff_status", **self.identity(config["goal_id"], config.get("agent_id"))})

    def call(self, tool, params):
        if tool != "goal_handoff" or params.get("action") not in ("status", "begin_attempt", "fail_attempt", "abort"):
            raise hc.ReadFailure()
        payload = {"action": "handoff_" + params["action"], **self.identity(params.get("goal_id"))}
        for key in ("handoff_id", "reason"):
            if key in params:
                payload[key] = params[key]
        # Explicit null denotes the first reservation. Old callbacks must carry
        # their exact attempt rather than acquiring authority from the Goal ID.
        payload["expected_attempt_id"] = params.get("attempt_id")
        return self.post(payload)

    def issue_launch_claim(self, request):
        """One request only. An uncertain reply must be reconciled, not reissued."""
        if request.get("project") != self.project:
            raise hc.ReadFailure()
        return self.post({"action": "handoff_launch_claim",
            **self.identity(request["goal_id"], request.get("agent_id")),
            "handoff_id": request["handoff_id"], "expected_attempt_id": request["attempt_id"]})
