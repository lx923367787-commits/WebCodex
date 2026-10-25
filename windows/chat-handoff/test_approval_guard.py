"""Approval guard tests. Fake browser by default; optional saved-HTML DOM test."""
import copy
import json
import os
from pathlib import Path
import unittest

import approval_guard as guard


REQUEST = {"launch_id": "40d0afff-27ad-49bf-8abf-0840f88a1949", "goal_id": "test-goal"}
URL = "https://chatgpt.com/c/12345678-test"


def card(**changes):
    value = {"exact_app": True, "tool": "Call Runtime Tool", "owner": "assistant-message-1",
        "button_count": 1, "enabled": True, "in_view": True, "unobscured": True,
        "center": [100, 200], "signature": "approval-1", "xpath": "//button[@id='allow']"}
    value.update(changes)
    return value


def observation(cards=None, **changes):
    value = {"url": URL, "document_visible": True, "bound": True, "user_count": 1,
        "cards": [card()] if cards is None else cards, "modal": False}
    value.update(changes)
    return value


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def clock(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class FakeElement:
    def __init__(self, browser, displayed=True, enabled=True):
        self.browser = browser
        self.displayed = displayed
        self.enabled = enabled

    def is_displayed(self):
        return self.displayed

    def is_enabled(self):
        return self.enabled

    def click(self, **options):
        if self.browser.on_trial:
            self.browser.on_trial()
        callback = options.pop('before_actual', None)
        if callback is not None and callback() is not True:
            raise RuntimeError('stopped_before_input')
        self.browser.clicks.append({"at": self.browser.clock.now, "options": options})
        if self.browser.click_error:
            raise RuntimeError("uncertain mouse result")
        if self.browser.on_click:
            self.browser.on_click()
        elif self.browser.remove_after_click:
            self.browser.current = observation(cards=[])


class FakeBrowser:
    def __init__(self, clock, snapshots=None):
        self.clock = clock
        self.queue = copy.deepcopy(snapshots or [])
        self.current = observation()
        self.clicks = []
        self.probes = []
        self.finds = []
        self.elements = [FakeElement(self)]
        self.remove_after_click = True
        self.click_error = False
        self.probe_error = None
        self.on_find = None
        self.on_click = None
        self.on_trial = None

    def execute_javascript(self, script, argument):
        args = json.loads(argument)
        self.probes.append(args)
        if self.probe_error:
            raise self.probe_error
        if self.queue:
            self.current = self.queue.pop(0)
        return copy.deepcopy(self.current)

    def get_url(self):
        return self.current["url"]

    def find_all_by_xpath(self, xpath, timeout):
        self.finds.append(xpath)
        if self.on_find:
            self.on_find()
        return self.elements


class GuardTests(unittest.TestCase):
    def setUp(self):
        self.time = FakeClock()
        self.browser = FakeBrowser(self.time)

    def run_guard(self, **options):
        options.setdefault("timeout", 2)
        return guard.run(self.browser, REQUEST, clock=self.time.clock, sleep=self.time.sleep, **options)

    def test_only_exact_chatgpt_chat_urls_are_accepted(self):
        for value in ("http://chatgpt.com/c/12345678", "https://chatgpt.com.attacker.test/c/12345678",
                "https://user@chatgpt.com/c/12345678", "https://chatgpt.com:444/c/12345678",
                "https://chatgpt.com/share/12345678", "https://chatgpt.com/c/short", None):
            with self.subTest(url=value):
                self.assertIsNone(guard.chat_url(value))
        self.assertEqual(guard.chat_url(URL + "?temporary=true#tail"), URL)
        self.assertEqual(guard.chat_url("https://chatgpt.com/g/g-test/c/12345678"),
            "https://chatgpt.com/g/g-test/c/12345678")

    def test_missing_unique_message_marker_never_clicks(self):
        self.browser.current = observation(bound=False)
        result = self.run_guard()
        self.assertEqual(result["status"], "unknown")
        self.assertFalse(result["message_observed"])
        self.assertEqual(self.browser.clicks, [])

    def test_url_changes_after_binding_stop_before_click(self):
        self.browser.queue = [observation(), observation(url="https://chatgpt.com/c/another-chat")]
        result = self.run_guard()
        self.assertEqual(result["reason"], "target_conversation_changed")
        self.assertEqual(self.browser.clicks, [])

    def test_message_marker_changes_after_binding_stop(self):
        self.browser.queue = [observation(), observation(bound=False)]
        self.assertEqual(self.run_guard()["reason"], "target_message_changed")
        self.assertEqual(self.browser.clicks, [])

    def test_other_app_is_rejected_with_or_without_tool(self):
        for value in (card(exact_app=False), card(exact_app=False, tool=None)):
            with self.subTest(card=value):
                self.browser.current = observation([value])
                self.assertEqual(self.run_guard()["reason"], "unrecognized_approval")
                self.assertEqual(self.browser.clicks, [])

    def test_description_only_app_approval_clicks_without_inventing_tool_name(self):
        self.browser.current = observation([card(tool=None)])
        result = self.run_guard()
        self.assertEqual(result['status'], 'sent')
        self.assertEqual(result['approval_click_count'], 1)
        self.assertEqual(result['approved_tools'], [])
        self.assertFalse(result['business_verified'])

    def test_multiple_cards_and_multiple_buttons_are_rejected(self):
        for cards, reason in (([card(), card(signature="other")], "ambiguous_approval_cards"),
                ([card(button_count=2)], "ambiguous_approval_button"),
                ([card(owner="")], "ambiguous_approval_button")):
            with self.subTest(reason=reason):
                self.browser.current = observation(cards)
                self.assertEqual(self.run_guard()["reason"], reason)
                self.assertEqual(self.browser.clicks, [])

    def test_disabled_and_obscured_buttons_are_never_clicked(self):
        for changes, reason in (({"enabled": False}, "button_disabled"),
                ({"unobscured": False}, "button_obscured")):
            with self.subTest(reason=reason):
                self.browser.current = observation([card(**changes)])
                self.assertEqual(self.run_guard()["reason"], reason)
                self.assertEqual(self.browser.clicks, [])

    def test_modal_is_not_dismissed_or_approved(self):
        self.browser.current = observation(modal=True)
        self.assertEqual(self.run_guard()["reason"], "unexpected_modal")
        self.assertEqual(self.browser.clicks, [])

    def test_two_stable_observations_and_fresh_probe_precede_one_mouse_click(self):
        result = self.run_guard()
        self.assertEqual(result["status"], "sent")
        self.assertEqual(result["approved_tools"], ["Call Runtime Tool"])
        self.assertFalse(result["business_verified"])
        self.assertEqual(len(self.browser.clicks), 1)
        self.assertGreaterEqual(self.browser.clicks[0]["at"], 0.5)
        self.assertEqual(self.browser.clicks[0]["options"],
            {"button": "left", "simulative": True, "move_mouse": True, "delay_after": 0})
        self.assertGreaterEqual(len(self.browser.probes), 4)

    def test_moving_layout_must_settle_before_mouse_input(self):
        self.browser.queue = [observation([card(center=[100, 200])]),
            observation([card(center=[160, 200])]), observation([card(center=[160, 200])])]
        self.run_guard(timeout=3)
        self.assertEqual(len(self.browser.clicks), 1)
        self.assertGreaterEqual(self.browser.clicks[0]["at"], 1.0)

    def test_stop_during_actionability_trial_prevents_actual_approval_click(self):
        stopped = [False]
        self.browser.on_trial = lambda: stopped.__setitem__(0, True)
        result = self.run_guard(should_stop=lambda: stopped[0])
        self.assertEqual(result['status'], 'unknown')
        self.assertEqual(self.browser.clicks, [])

    def test_fresh_probe_position_change_prevents_click(self):
        self.browser.queue = [observation(), observation(), observation([card(center=[160, 200])])]
        self.run_guard(timeout=1)
        self.assertEqual(self.browser.clicks, [])

    def test_scroll_requires_a_new_stable_observation(self):
        self.browser.queue = [observation([card(in_view=False)]), observation(), observation(), observation()]
        self.run_guard(timeout=3)
        self.assertEqual(len(self.browser.clicks), 1)
        self.assertTrue(any(p["scroll_signature"] == "approval-1" for p in self.browser.probes))
        self.assertGreaterEqual(self.browser.clicks[0]["at"], 1.0)

    def test_click_exception_is_unknown_and_not_retried(self):
        self.browser.click_error = True
        result = self.run_guard(timeout=60)
        self.assertEqual(result["reason"], "approval_click_outcome_unknown")
        self.assertEqual(result["status"], "unknown")
        self.assertEqual(len(self.browser.clicks), 1)

    def test_card_still_present_15_seconds_after_click_is_unknown_without_retry(self):
        self.browser.remove_after_click = False
        result = self.run_guard(timeout=60)
        self.assertEqual(result["reason"], "approval_click_unconfirmed")
        self.assertEqual(len(self.browser.clicks), 1)
        self.assertEqual(self.time.now, 15.5)

    def test_reappeared_approval_is_not_clicked_again(self):
        self.browser.on_click = lambda: setattr(self.browser, "queue", [observation(cards=[]), observation()])
        result = self.run_guard(timeout=10)
        self.assertEqual(result["reason"], "approval_reappeared")
        self.assertEqual(len(self.browser.clicks), 1)

    def test_stop_before_observation_produces_no_mouse_input(self):
        result = self.run_guard(should_stop=lambda: True)
        self.assertEqual(result["status"], "unknown")
        self.assertEqual(self.browser.clicks, [])
        self.assertEqual(self.browser.probes, [])

    def test_stop_during_locator_resolution_prevents_mouse_input(self):
        stopped = [False]
        self.browser.on_find = lambda: stopped.__setitem__(0, True)
        result = self.run_guard(should_stop=lambda: stopped[0])
        self.assertEqual(result["reason"], "cancelled_before_approval")
        self.assertEqual(self.browser.clicks, [])

    def test_timeout_is_not_reset_by_successful_approval(self):
        result = self.run_guard(timeout=2)
        self.assertEqual(result["status"], "sent")
        self.assertEqual(self.time.now, 2.0)

    def test_no_popup_with_matching_message_is_sent_but_not_business_success(self):
        self.browser.current = observation(cards=[])
        result = self.run_guard()
        self.assertEqual(result["status"], "sent")
        self.assertTrue(result["message_observed"])
        self.assertFalse(result["business_verified"])
        self.assertEqual(result["approved_tools"], [])
        self.assertEqual(self.browser.clicks, [])

    def test_three_observation_failures_return_unknown(self):
        self.browser.probe_error = RuntimeError("page inaccessible")
        self.assertEqual(self.run_guard()["reason"], "browser_observation_failed")
        self.assertEqual(len(self.browser.probes), 3)
        self.assertEqual(self.browser.clicks, [])

    def test_multiple_live_locator_results_are_rejected(self):
        self.browser.elements = [FakeElement(self.browser), FakeElement(self.browser)]
        self.assertEqual(self.run_guard()["reason"], "button_identity_changed")
        self.assertEqual(self.browser.clicks, [])

    def test_overlay_appearing_during_locator_resolution_prevents_click(self):
        self.browser.on_find = lambda: setattr(self.browser, "current", observation([card(unobscured=False)]))
        self.run_guard()
        self.assertEqual(self.browser.clicks, [])

    def test_locator_exception_returns_unknown_without_click(self):
        def fail_locator():
            raise RuntimeError("stale locator")
        self.browser.on_find = fail_locator
        result = self.run_guard()
        self.assertEqual(result["status"], "unknown")
        self.assertEqual(self.browser.clicks, [])

    def test_sdk_false_enabled_does_not_override_fresh_enabled_dom(self):
        self.browser.elements[0].enabled = False
        self.run_guard()
        self.assertEqual(len(self.browser.clicks), 1)


@unittest.skipUnless(os.environ.get("WEBCODEX_TEST_CHROMIUM"), "Offline DOM test requires a Chromium executable")
class DescriptionCardDomTests(unittest.TestCase):
    """Synthetic, credential-free cards based on the observed bilingual UI."""
    @classmethod
    def setUpClass(cls):
        from playwright.sync_api import sync_playwright
        cls.driver = sync_playwright().start()
        cls.browser = cls.driver.chromium.launch(headless=True,
            executable_path=os.environ['WEBCODEX_TEST_CHROMIUM'])
        cls.context = cls.browser.new_context(viewport={"width": 1200, "height": 900})
        cls.context.route('**/*', lambda route: route.abort())
        cls.page = cls.context.new_page()

    @classmethod
    def tearDownClass(cls):
        cls.context.close()
        cls.browser.close()
        cls.driver.stop()

    def probe(self, title, button='始终允许'):
        self.page.set_content('''<div data-markdown-text-tone="user-message"></div>
            <section data-chatgpt-search-message-ids="offline-assistant-1">
            <div data-codex-approval-surface="true"><div role="alert">
            <span>webcodex</span><p id="title"></p>
            <div>Attaches a trusted host window using a one-time enrollment grant.</div>
            <button id="always"></button><button>拒绝</button><button>允许一次</button>
            </div></div></section>''')
        self.page.locator('[data-markdown-text-tone]').evaluate(
            '(e, text) => e.textContent=text', REQUEST['launch_id'] + ' ' + REQUEST['goal_id'])
        self.page.locator('#title').evaluate('(e, text) => e.textContent=text', title)
        self.page.locator('#always').evaluate('(e, text) => e.textContent=text', button)
        return self.read_probe()

    def read_probe(self):
        return self.page.evaluate('arg => (' + guard.PROBE_JS + ')(null, arg)', json.dumps(REQUEST))

    def test_bilingual_description_cards_accept_case_spacing_and_punctuation(self):
        for title in ('允许 ChatGPT 使用 webcodex?', '允许 ChatGPT 使用 webcodex 吗？',
                '允许ChatGPT使用WEBCODEX', 'Allow ChatGPT to use webcodex?',
                'ALLOW  ChatGPT TO USE webcodex？'):
            for button in ('始终允许', 'Always allow', '\u200bAlways\u00a0Allow '):
                with self.subTest(title=title, button=button):
                    snapshot = self.probe(title, button)
                    actual, reason = guard.candidate(snapshot)
                    self.assertIsNone(reason)
                    self.assertIsNotNone(actual)
                    self.assertIsNone(actual['tool'])
                    self.assertEqual(self.page.locator('xpath=' + actual['xpath']).count(), 1)

    def test_other_apps_and_unrelated_titles_do_not_authorize(self):
        for title in ('Allow ChatGPT to use WebCodex-Hom?', 'Allow ChatGPT to use webcodex-extra?',
                '允许 ChatGPT 使用 other-app?', 'Quoted: Allow ChatGPT to use webcodex?'):
            with self.subTest(title=title):
                self.assertEqual(guard.candidate(self.probe(title))[1], 'unrecognized_approval')

    def test_hidden_matching_title_and_non_always_buttons_do_not_authorize(self):
        for button in ('允许一次', 'Allow once', 'Do not always allow'):
            self.assertEqual(guard.candidate(self.probe('允许 ChatGPT 使用 webcodex?', button))[1],
                'ambiguous_approval_button')
        self.probe('允许 ChatGPT 使用 webcodex?')
        self.page.locator('#title').evaluate("e => e.style.display='none'")
        self.assertEqual(guard.candidate(self.read_probe())[1], 'unrecognized_approval')


@unittest.skipUnless(os.environ.get("WEBCODEX_APPROVAL_HTML"), "Saved HTML DOM probe is opt-in and offline")
class SavedHtmlDomTests(unittest.TestCase):
    """Uses existing Playwright and Chromium, never live ChatGPT or the network."""
    @classmethod
    def setUpClass(cls):
        from playwright.sync_api import sync_playwright
        cls.driver = sync_playwright().start()
        cls.browser = cls.driver.chromium.launch(headless=True,
            executable_path=os.environ.get("WEBCODEX_TEST_CHROMIUM"))
        cls.context = cls.browser.new_context(viewport={"width": 1200, "height": 900}, java_script_enabled=True)
        cls.context.route("**/*", lambda route: route.abort())
        cls.page = cls.context.new_page()
        raw = Path(os.environ["WEBCODEX_APPROVAL_HTML"]).read_text(encoding="utf-8-sig")
        # DOMParser parses inertly: no original script is executed or chat text
        # exposed. Keep only the actual approval card, stripped of URL attrs.
        cls.fixture = cls.page.evaluate("""raw => {
            const doc = new DOMParser().parseFromString(raw, 'text/html');
            const cards = doc.querySelectorAll('[data-codex-approval-surface="true"]');
            if (cards.length !== 1) throw Error('expected exactly one source approval card');
            const card = cards[0].cloneNode(true);
            card.querySelectorAll('script,style,iframe,object,embed,img,link').forEach(e => e.remove());
            for (const el of [card, ...card.querySelectorAll('*')]) {
                for (const attr of [...el.attributes]) {
                    if (attr.name.startsWith('on') || ['src','href','srcset'].includes(attr.name)) el.removeAttribute(attr.name);
                }
            }
            return card.outerHTML;
        }""", raw)

    @classmethod
    def tearDownClass(cls):
        cls.context.close()
        cls.browser.close()
        cls.driver.stop()

    def setUp(self):
        self.page.set_content("""<!doctype html><style>body{margin:20px;font:16px Arial}button{padding:10px;min-width:80px}
            [data-codex-approval-surface]{border:1px solid #888;padding:16px;width:800px;margin-top:30px}</style>"""
            + '<div data-markdown-text-tone="user-message">' + REQUEST["goal_id"] + " " + REQUEST["launch_id"] + "</div>"
            + '<section data-chatgpt-search-message-ids="offline-assistant-1">' + self.fixture + "</section>")

    def probe(self):
        return self.page.evaluate("arg => (" + guard.PROBE_JS + ")(null, arg)", json.dumps(REQUEST))

    def test_saved_actual_card_exact_app_tool_and_single_unobscured_button(self):
        value = self.probe()
        self.assertTrue(value["bound"])
        self.assertEqual(len(value["cards"]), 1)
        actual = value["cards"][0]
        self.assertTrue(actual["exact_app"])
        self.assertEqual(actual["tool"], "Call Runtime Tool")
        self.assertEqual(actual["button_count"], 1)
        self.assertTrue(actual["enabled"])
        self.assertTrue(actual["in_view"])
        self.assertTrue(actual["unobscured"])
        self.assertEqual(self.page.locator("xpath=" + actual["xpath"]).count(), 1)

    def test_dom_duplicate_user_marker_does_not_bind(self):
        self.page.evaluate("document.body.append(document.querySelector('[data-markdown-text-tone]').cloneNode(true))")
        self.assertFalse(self.probe()["bound"])

    def test_legacy_message_attribute_remains_supported(self):
        self.page.evaluate("""() => {const e=document.querySelector('[data-markdown-text-tone]');
            e.removeAttribute('data-markdown-text-tone'); e.setAttribute('data-message-author-role','user')}""")
        self.assertTrue(self.probe()['bound'])

    def test_dom_other_app_is_rejected(self):
        self.page.evaluate("document.body.innerHTML = document.body.innerHTML.replaceAll('webcodex','other-app')")
        self.assertFalse(self.probe()["cards"][0]["exact_app"])

    def test_dom_overlay_is_detected(self):
        self.page.evaluate("""() => {const e=document.createElement('div'); e.style='position:fixed;inset:0;z-index:999999;background:white'; document.body.append(e)}""")
        self.assertFalse(self.probe()["cards"][0]["unobscured"])

    def test_dom_fully_transparent_parent_does_not_expose_approval_card(self):
        self.page.evaluate("document.querySelector('[data-chatgpt-search-message-ids]').style.opacity='0'")
        self.assertEqual(self.probe()["cards"], [])

    def test_dom_disabled_button_is_detected(self):
        value = self.probe()
        self.page.locator("xpath=" + value["cards"][0]["xpath"]).evaluate("e => e.disabled=true")
        self.assertFalse(self.probe()["cards"][0]["enabled"])

    def test_dom_aria_disabled_ancestor_is_respected(self):
        self.page.evaluate("document.querySelector('[data-codex-approval-surface]').setAttribute('aria-disabled','true')")
        self.assertFalse(self.probe()['cards'][0]['enabled'])

    def test_dom_normalized_button_text_still_has_unique_locator(self):
        value = self.probe()
        self.page.locator("xpath=" + value["cards"][0]["xpath"]).evaluate("e => e.textContent='\\u200b始终允许\\u00a0'")
        actual = self.probe()["cards"][0]
        self.assertEqual(actual["button_count"], 1)
        self.assertEqual(self.page.locator("xpath=" + actual["xpath"]).count(), 1)


if __name__ == "__main__":
    unittest.main()
