"""Small in-memory enrollment helpers; no control API, browser, or storage.

The only intentional secret-bearing output is build_enrollment_message(), for
delivery by the trusted launcher to this Agent's exact owned chat. Ordinary
config, receipts and parent results must pass the guards below instead.
"""
from __future__ import annotations

import json
import re
import time


_GRANT = re.compile(r"wc_en_[0-9a-f]{64}")
_ENROLLMENT_ID = re.compile(r"wc_enrollment_[0-9a-f]{32}")
_MAX_TIMESTAMP = (1 << 63) - 1


def valid_enrollment_grant(value):
    return isinstance(value, str) and _GRANT.fullmatch(value) is not None


def contains_launch_grant(value):
    """Inspect ordinary JSON data, including keys and decoded Unicode text."""
    encoded = json.dumps(value, ensure_ascii=False)
    # Keep the old handoff guard's broad prefix rule. Enrollment uses its exact
    # token format so explanatory text containing only "wc_en_" remains safe.
    return "wc_hc_" in encoded or _GRANT.search(encoded) is not None


def ensure_no_launch_grant(value):
    if contains_launch_grant(value):
        raise ValueError("launch_claim_must_not_be_persisted")


def sanitize_candidate_response(value):
    """Copy data, omitting whole secret-bearing candidate text fields only.

All other grant occurrences remain errors. No secret or redacted fragment is
returned in a candidate answer, and omission cannot leave a candidate ready.
"""
    def copy_value(item):
        if isinstance(item, dict):
            result = {}
            omitted = False
            for key, child in item.items():
                if key == "response_text" and contains_launch_grant(child):
                    omitted = True
                else:
                    result[key] = copy_value(child)
            if omitted:
                # Validate before overwriting markers: a grant originally in
                # response_reason/state is not an allowed candidate omission.
                ensure_no_launch_grant(result)
                result.update(response_omitted=True,
                              response_reason="sensitive_candidate_omitted",
                              response_observed=False)
                if result.get("state") == "response_observed":
                    result["state"] = "unknown"
            return result
        if isinstance(item, list):
            return [copy_value(child) for child in item]
        if isinstance(item, tuple):
            return tuple(copy_value(child) for child in item)
        return item

    sanitized = copy_value(value)
    ensure_no_launch_grant(sanitized)
    return sanitized


def _valid_metadata(enrollment_id, claim_grant, expires_at_unix_ms):
    return (isinstance(enrollment_id, str) and _ENROLLMENT_ID.fullmatch(enrollment_id) is not None
            and valid_enrollment_grant(claim_grant)
            and type(expires_at_unix_ms) is int and 0 < expires_at_unix_ms <= _MAX_TIMESTAMP)


class EnrollmentDelivery:
    """Trusted transient delivery; never add this object to ordinary config."""
    __slots__ = ("_enrollment_id", "_claim_grant", "_expires_at_unix_ms")

    def __init__(self, enrollment_id, claim_grant, expires_at_unix_ms):
        if not _valid_metadata(enrollment_id, claim_grant, expires_at_unix_ms):
            raise ValueError("invalid_enrollment_delivery")
        object.__setattr__(self, "_enrollment_id", enrollment_id)
        object.__setattr__(self, "_claim_grant", claim_grant)
        object.__setattr__(self, "_expires_at_unix_ms", expires_at_unix_ms)

    @property
    def enrollment_id(self):
        return self._enrollment_id

    @property
    def expires_at_unix_ms(self):
        return self._expires_at_unix_ms

    def __setattr__(self, name, value):
        raise TypeError("enrollment_delivery_immutable")

    def __repr__(self):
        return "EnrollmentDelivery([REDACTED])"

    __str__ = __repr__

    def __reduce__(self):
        raise TypeError("enrollment_delivery_not_serializable")

    def __reduce_ex__(self, protocol):
        raise TypeError("enrollment_delivery_not_serializable")

    def __getstate__(self):
        raise TypeError("enrollment_delivery_not_serializable")


def build_enrollment_message(base_prompt, delivery, *, now_ms=None):
    """Build the sole ephemeral secret-bearing message without changing input."""
    if not isinstance(base_prompt, str) or not base_prompt.strip():
        raise ValueError("invalid_enrollment_message")
    try:
        base_prompt.encode("utf-8")
    except UnicodeError:
        raise ValueError("invalid_enrollment_message") from None
    ensure_no_launch_grant(base_prompt)
    if type(delivery) is not EnrollmentDelivery or not _valid_metadata(
            delivery.enrollment_id, delivery._claim_grant, delivery.expires_at_unix_ms):
        raise ValueError("invalid_enrollment_delivery")
    now_ms = int(time.time() * 1000) if now_ms is None else now_ms
    if type(now_ms) is not int or not 0 <= now_ms <= _MAX_TIMESTAMP:
        raise ValueError("invalid_enrollment_time")
    if now_ms >= delivery.expires_at_unix_ms:
        raise ValueError("enrollment_delivery_expired")
    return (
        "这是仅交付给本目标新对话所属Agent的短期单用接入授权。"
        "先仅调用 agent_attach，将下列 enrollment_id 和 claim_grant 原样传入；"
        "确认 attached=true，并核对返回的本任务assignment、Agent和run后，才执行下方任务。"
        "接入失败或结果未知时停止业务操作，不更换身份、不猜测其他任务。"
        "grant只用于本次首次接入；禁止回显、写入文件或日志、放入会议厅、转发给父任务或其他Agent。"
        "它不是PAT、Bootstrap或其他Agent的凭据，不授予额外权限。\n"
        f"enrollment_id={delivery.enrollment_id}\n"
        f"claim_grant={delivery._claim_grant}\n"
        f"本次任务：\n{base_prompt}"
    )
