"""Pure-memory helper checks using synthetic grants only."""
import copy
import json
import pickle
import unittest
from unittest.mock import patch

import host_enrollment as host


GRANT = "wc_en_" + "ab012345" * 8
ENROLLMENT_ID = "wc_enrollment_" + "0123456789abcdef" * 2
EXPIRY = 1000000


class GrantGuardTests(unittest.TestCase):
    def test_grant_validator_accepts_only_exact_rust_format(self):
        self.assertTrue(host.valid_enrollment_grant(GRANT))
        for value in (None, True, 1, [], {}, GRANT.upper(), GRANT + "0", GRANT[:-1],
                      " " + GRANT, GRANT + "\n", "wc_en_" + "g" * 64):
            with self.subTest(kind=type(value).__name__):
                self.assertFalse(host.valid_enrollment_grant(value))

    def test_nested_values_keys_and_substrings_are_guarded(self):
        for value in (GRANT, "prefix " + GRANT + " suffix", GRANT + "f",
                      {"safe": [None, {"value": GRANT}]}, {GRANT: "safe"}, (GRANT,)):
            with self.subTest(kind=type(value).__name__):
                self.assertTrue(host.contains_launch_grant(value))
                with self.assertRaisesRegex(ValueError, "^launch_claim_must_not_be_persisted$"):
                    host.ensure_no_launch_grant(value)

    def test_legacy_prefix_rule_and_enrollment_documentation_remain_distinct(self):
        for legacy in ("wc_hc_", "prefix wc_hc_anything", {"wc_hc_key": "safe"}):
            self.assertTrue(host.contains_launch_grant(legacy))
        for safe in (None, True, 1, [], {"format": "wc_en_ plus 64 lowercase hex", "id": ENROLLMENT_ID}):
            self.assertFalse(host.contains_launch_grant(safe))
            host.ensure_no_launch_grant(safe)

    def test_unicode_json_decoding_cannot_hide_real_grant(self):
        encoded = '"\\u0077\\u0063\\u005f\\u0065\\u006e\\u005f' + GRANT[6:] + '"'
        self.assertTrue(host.contains_launch_grant(json.loads(encoded)))

    def test_error_never_contains_secret_text(self):
        try:
            host.ensure_no_launch_grant({"private": GRANT})
        except ValueError as error:
            self.assertNotIn(GRANT, str(error))
            self.assertNotIn(GRANT, repr(error))
        else:
            self.fail("grant was accepted")


class CandidateTests(unittest.TestCase):
    def test_safe_data_returns_equal_independent_copy_with_same_shape(self):
        original = {"state": "finished", "results": [{"response_text": "safe", "response_observed": True}],
                    "tuple": ({"response_text": "also safe"},)}
        result = host.sanitize_candidate_response(original)
        self.assertEqual(result, original)
        self.assertIsNot(result, original)
        self.assertIsNot(result["results"], original["results"])
        self.assertIsInstance(result["tuple"], tuple)

    def test_candidate_omission_removes_entire_field_and_ready_state(self):
        original = {"state": "response_observed", "response_text": "before " + GRANT + " after",
                    "response_observed": True, "response_reason": "stable_candidate_only"}
        before = copy.deepcopy(original)
        result = host.sanitize_candidate_response(original)
        self.assertEqual(result, {"state": "unknown", "response_observed": False,
                                 "response_omitted": True, "response_reason": "sensitive_candidate_omitted"})
        self.assertEqual(original, before)
        self.assertNotIn(GRANT, json.dumps(result))

    def test_nested_candidate_omission_preserves_local_worker_finished(self):
        result = host.sanitize_candidate_response({"state": "finished", "result": [
            {"state": "finished", "response_text": GRANT, "response_observed": True},
            {"response_text": "wc_hc_legacy", "response_observed": True}]})
        self.assertEqual(result["state"], "finished")
        self.assertEqual(result["result"][0]["state"], "finished")
        for child in result["result"]:
            self.assertNotIn("response_text", child)
            self.assertFalse(child["response_observed"])
            self.assertTrue(child["response_omitted"])

    def test_other_fields_still_reject_even_when_candidate_is_omitted(self):
        for field in ("message", "response_reason", "state", "response_omitted", "response_observed"):
            with self.subTest(field=field):
                with self.assertRaisesRegex(ValueError, "^launch_claim_must_not_be_persisted$"):
                    host.sanitize_candidate_response({"response_text": GRANT, field: GRANT})
        with self.assertRaisesRegex(ValueError, "^launch_claim_must_not_be_persisted$"):
            host.sanitize_candidate_response({GRANT: {"response_text": GRANT}})

    def test_top_level_secret_and_non_candidate_nested_secret_are_not_sanitized(self):
        for value in (GRANT, [GRANT], {"text": GRANT}, {"child": {"response_text": "safe", "other": GRANT}}):
            with self.assertRaisesRegex(ValueError, "^launch_claim_must_not_be_persisted$"):
                host.sanitize_candidate_response(value)


class DeliveryTests(unittest.TestCase):
    def delivery(self, **updates):
        return host.EnrollmentDelivery(**dict({"enrollment_id": ENROLLMENT_ID,
            "claim_grant": GRANT, "expires_at_unix_ms": EXPIRY}, **updates))

    def test_repr_str_and_containers_never_render_secret(self):
        delivery = self.delivery()
        for rendered in (repr(delivery), str(delivery), repr([delivery]), repr({"delivery": delivery})):
            self.assertIn("REDACTED", rendered)
            self.assertNotIn(GRANT, rendered)
        self.assertFalse(hasattr(delivery, "__dict__"))
        with self.assertRaisesRegex(TypeError, "enrollment_delivery_immutable"):
            delivery.expires_at_unix_ms = EXPIRY + 1

    def test_json_pickle_and_state_export_are_rejected_without_secret(self):
        delivery = self.delivery()
        operations = [lambda: json.dumps(delivery), lambda: vars(delivery), delivery.__getstate__]
        operations += [lambda protocol=protocol: pickle.dumps(delivery, protocol=protocol)
                       for protocol in range(pickle.HIGHEST_PROTOCOL + 1)]
        for operation in operations:
            with self.assertRaises(TypeError) as raised:
                operation()
            self.assertNotIn(GRANT, str(raised.exception))

    def test_invalid_delivery_metadata_has_fixed_error(self):
        for updates in ({"claim_grant": GRANT.upper()}, {"claim_grant": None},
                        {"enrollment_id": "wc_enrollment_" + "a" * 31},
                        {"enrollment_id": ENROLLMENT_ID.upper()},
                        {"expires_at_unix_ms": True}, {"expires_at_unix_ms": 1.5},
                        {"expires_at_unix_ms": 0}, {"expires_at_unix_ms": 1 << 63}):
            with self.subTest(field=next(iter(updates))):
                with self.assertRaisesRegex(ValueError, "^invalid_enrollment_delivery$"):
                    self.delivery(**updates)

    def test_ephemeral_message_keeps_prompt_and_delivery_unchanged(self):
        prompt = "核对已经授权的任务并给出验证结果。"
        delivery = self.delivery()
        message = host.build_enrollment_message(prompt, delivery, now_ms=EXPIRY - 1)
        self.assertEqual(prompt, "核对已经授权的任务并给出验证结果。")
        self.assertTrue(message.endswith(prompt))
        self.assertEqual(message.count(GRANT), 1)
        self.assertIn("agent_attach", message)
        self.assertIn("enrollment_id=" + ENROLLMENT_ID, message)
        self.assertIn("claim_grant=" + GRANT, message)
        self.assertEqual(delivery.expires_at_unix_ms, EXPIRY)
        with self.assertRaisesRegex(ValueError, "launch_claim_must_not_be_persisted"):
            host.ensure_no_launch_grant(message)

    def test_expiry_boundary_and_injected_clock_are_strict(self):
        for now in (EXPIRY, EXPIRY + 1):
            with self.assertRaisesRegex(ValueError, "^enrollment_delivery_expired$"):
                host.build_enrollment_message("task", self.delivery(), now_ms=now)
        with patch.object(host.time, "time", return_value=EXPIRY / 1000):
            with self.assertRaisesRegex(ValueError, "^enrollment_delivery_expired$"):
                host.build_enrollment_message("task", self.delivery())
        for now in (True, -1, 1.5, "1", 1 << 63):
            with self.assertRaisesRegex(ValueError, "^invalid_enrollment_time$"):
                host.build_enrollment_message("task", self.delivery(), now_ms=now)

    def test_message_rejects_bad_prompt_or_non_delivery_without_echo(self):
        for prompt in (None, "", " ", "\ud800"):
            with self.assertRaisesRegex(ValueError, "^invalid_enrollment_message$"):
                host.build_enrollment_message(prompt, self.delivery(), now_ms=1)
        with self.assertRaisesRegex(ValueError, "^invalid_enrollment_delivery$"):
            host.build_enrollment_message("task", {"claim_grant": GRANT}, now_ms=1)
        with self.assertRaisesRegex(ValueError, "^launch_claim_must_not_be_persisted$"):
            host.build_enrollment_message(GRANT, self.delivery(), now_ms=1)


if __name__ == "__main__":
    unittest.main()
