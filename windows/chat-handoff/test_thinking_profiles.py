"""Offline model-menu tests with observed keyboard semantics and no browser."""
from copy import deepcopy
import unittest
from unittest.mock import Mock, patch

import edge_handoff as handoff
import thinking_profiles as profiles


class FakeBrowser:
    def __init__(self):
        self.value, self.lower, self.upper = 4, 0, 4
        self.labels = {0: "Instant", 1: "Medium", 2: "High", 3: "Extra High", 4: "Pro"}
        self.efforts = {0: "none", 1: "medium", 2: "high", 3: "max", 4: "medium"}
        self.focused = False
        self.enabled = True
        self.menu = False
        self.actions = []
        self.option_model = None
        self.change_model = False
        self.freeze = False
        self.hidden_focus = False
        self.fail_readback = False
        self.now = 0

    def state(self):
        options = [{"label": "Visible model", "selected": self.option_model is not None}]
        return {"origin": "https://chatgpt.com", "path": "/", "menu_count": int(self.menu), "owner_count": 1,
            "label": self.labels[self.value], "model": self.option_model,
            "model_options": options, "selected_model_count": int(self.option_model is not None),
            "reasoning_effort": self.efforts[self.value], "value": str(self.value),
            "min": str(self.lower), "max": str(self.upper), "focused": True, "visible": True,
            "control": {"xpath": "//visible-keyboard-menuitem", "center": [100, 200], "enabled": self.enabled,
                "clear": True, "focused": self.focused, "keys": "ArrowLeft ArrowRight"}}

    def execute_javascript(self, script):
        if script != profiles.PROBE_JS:
            raise AssertionError("unexpected script")
        return deepcopy(self.state())

    def setup_state(self, _):
        return {"chat": {"count": 1, "pressed": "true"},
                "trigger": {"count": 1, "text": "incorrect" if self.fail_readback else self.labels[self.value]},
                "menu_count": int(self.menu), "effort": self.efforts[self.value]}

    def control_click(self, _browser, key, _request, _deadline):
        self.assert_key(key)
        self.menu = True

    def assert_key(self, key):
        if key != "trigger":
            raise AssertionError("model/chat control must not be changed")

    def find_by_xpath(self, xpath, **kwargs):
        def click(before_actual=lambda: True, **_):
            if not before_actual():
                raise RuntimeError("cancelled")
            self.actions.append(("click", xpath))
            if xpath == "//visible-keyboard-menuitem":
                self.focused = not self.hidden_focus
                self.value = (self.lower + self.upper) // 2
            elif xpath == "//empty-composer":
                self.menu = False
            else:
                raise AssertionError("unexpected target")
        return Mock(click=click)

    def keys(self, key, **kwargs):
        if not self.focused:
            raise AssertionError("keyboard without focused menuitem")
        self.actions.append(("key", key))
        if self.change_model:
            self.option_model = "Visible model"
        if not self.freeze:
            self.value += 1 if key == "{RIGHT}" else -1


class ThinkingProfileTests(unittest.TestCase):
    def setUp(self):
        self.browser = FakeBrowser()
        self.request = {"launch_id": "offline", "expires_at": "2100-01-01T00:00:00Z"}
        patches = [patch.object(profiles.flow, "cancelled", return_value=False),
            patch.object(profiles.time, "monotonic", side_effect=lambda: self.browser.now),
            patch.object(profiles.time, "sleep", side_effect=lambda n: setattr(self.browser, "now", self.browser.now+n)),
            patch.object(handoff, "setup_state", side_effect=self.browser.setup_state),
            patch.object(handoff, "composer", return_value={"user_count": 0, "text": ""}),
            patch.object(handoff, "click_control", side_effect=self.browser.control_click),
            patch.object(handoff, "wait_composer", return_value={"xpath": "//empty-composer"})]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)

    def catalog(self):
        return profiles.discover_profiles(self.browser, self.request)

    def select(self, snapshot=None):
        return profiles.select_profile(self.browser, self.request, self.browser.keys, snapshot)

    def test_discovery_observes_five_profiles_and_restores_exact_original(self):
        catalog = self.catalog()
        self.assertEqual(["Instant", "Medium", "High", "Extra High", "Pro"], [p["label"] for p in catalog["profiles"]])
        self.assertEqual(["none", "medium", "high", "max", "medium"], [p["reasoning_effort"] for p in catalog["profiles"]])
        self.assertTrue(all(p["model"] is None for p in catalog["profiles"]))
        self.assertEqual(4, self.browser.value)
        self.assertFalse(self.browser.menu)
        self.assertEqual(catalog["profiles"][-1]["profile_id"], catalog["restored_profile_id"])
        self.assertFalse(any("observed_at" in p for p in catalog["profiles"]))

    def test_same_visible_snapshot_gets_stable_profile_id(self):
        first = self.catalog()["profiles"]
        self.browser.actions.clear()
        second = self.catalog()["profiles"]
        self.assertEqual(first, second)

    def test_exact_selection_reads_after_focus_click_moves_slider(self):
        wanted = self.catalog()["profiles"][0]
        self.browser.actions.clear()
        self.browser.focused = False
        result = self.select(wanted)
        self.assertEqual(wanted, result["thinking_profile"])
        self.assertEqual(0, self.browser.value)
        self.assertEqual([("key", "{LEFT}"), ("key", "{LEFT}")],
                         [a for a in self.browser.actions if a[0] == "key"])

    def test_default_goes_to_observed_maximum_not_first_pro_label_or_hardcoded_four(self):
        self.browser.upper = 7
        self.browser.labels.update({4: "Pro", 5: "Other", 6: "Other", 7: "Pro"})
        self.browser.efforts.update({5: "high", 6: "max", 7: "medium"})
        result = self.select()
        self.assertEqual(7, result["thinking_profile"]["slider_value"])
        self.assertEqual("Pro", result["intelligence"])
        self.assertEqual("medium", result["reasoning_effort"])

    def test_default_rejects_nonpro_maximum(self):
        self.browser.labels[4] = "Unknown top"
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "pro_not_observed_at_maximum"):
            self.select()

    def test_disabled_control_does_not_receive_input(self):
        self.browser.enabled = False
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "disabled_or_unknown"):
            self.catalog()
        self.assertEqual([], self.browser.actions)

    def test_missing_keyboard_focus_stops_before_arrows(self):
        self.browser.hidden_focus = True
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "keyboard_focus_missing"):
            self.catalog()
        self.assertFalse(any(a[0] == "key" for a in self.browser.actions))

    def test_no_model_switch_is_attempted_when_requested_context_differs(self):
        wanted = self.catalog()["profiles"][0]
        self.browser.option_model = "Visible model"
        self.browser.actions.clear()
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "model_or_range_mismatch"):
            self.select(wanted)
        self.assertEqual([], self.browser.actions)

    def test_model_change_during_arrow_navigation_stops(self):
        self.browser.change_model = True
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "model_or_range_changed"):
            self.catalog()

    def test_tampered_snapshot_rejected_before_input(self):
        wanted = self.catalog()["profiles"][0]
        wanted["label"] = "forged"
        self.browser.actions.clear()
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "snapshot_invalid"):
            self.select(wanted)
        self.assertEqual([], self.browser.actions)

    def test_live_label_change_cannot_satisfy_stored_profile(self):
        wanted = self.catalog()["profiles"][0]
        self.browser.labels[0] = "Renamed"
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "snapshot_changed"):
            self.select(wanted)

    def test_frozen_slider_stops_with_bounded_attempt(self):
        self.browser.freeze = True
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "did_not_advance"):
            self.catalog()
        self.assertEqual(1, sum(a[0] == "key" for a in self.browser.actions))

    def test_closed_menu_readback_mismatch_stops(self):
        self.browser.fail_readback = True
        with self.assertRaisesRegex(profiles.ThinkingProfileError, "readback_failed"):
            self.select()

    def test_cancelled_request_never_targets_controls(self):
        with patch.object(profiles.flow, "cancelled", return_value=True):
            with self.assertRaisesRegex(profiles.ThinkingProfileError, "selection_cancelled"):
                self.select()
        self.assertEqual([], self.browser.actions)


if __name__ == "__main__":
    unittest.main()
