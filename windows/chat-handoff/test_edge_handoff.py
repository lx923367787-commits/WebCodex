"""Edge semantic-flow regressions; no live account or network actions."""
import ast
import os
from pathlib import Path
import unittest
from unittest import mock

import edge_handoff as direct


PROMPT = ("继续工作：在本次新对话恢复已有 Goal，不创建新 Goal。\n"
    "project=agent:test:project\n"
    "goal_id=goal-test-12345678\n"
    "source_context_epoch=source-test-12345678\n"
    "checkpoint=3\nrevision=7\n"
    "launch_id=40d0afff-27ad-49bf-8abf-0840f88a1949\n"
    "首先调用 goal_bootstrap；核验原计划和已有操作后继续。")


def input_state(text=None, **changes):
    value = {"text": "webcodex " + PROMPT if text is None else text, "mention": True, "user_count": 0}
    value.update(changes)
    return value


class InputTests(unittest.TestCase):
    def test_exact_prompt_with_selected_webcodex_mention_is_accepted(self):
        for prefix in ("", "webcodex ", "@webcodex ", "WebCodex ", "@WEBCODEX "):
            with self.subTest(prefix=prefix):
                self.assertTrue(direct.exact_recovery_input(input_state(prefix + PROMPT), PROMPT))

    def test_empty_draft_accepts_only_verified_app_chip(self):
        for text in ('', 'WebCodex', '@WEBCODEX'):
            self.assertTrue(direct.empty_app_composer(input_state(text)))
        for state in (input_state('@webcodex', mention=False),
                      input_state('@webcodex', user_count=1),
                      input_state('@webcodex existing draft'),
                      input_state('@webcodex-hom'), input_state('@other')):
            self.assertFalse(direct.empty_app_composer(state))

    def test_each_required_identity_must_be_complete(self):
        for field in ("project", "goal_id", "source_context_epoch", "launch_id"):
            with self.subTest(field=field):
                lines = PROMPT.splitlines()
                line = next(line for line in lines if line.startswith(field + "="))
                for replacement in ("", line[:-1], field + "=wrong-value"):
                    changed = PROMPT.replace(line, replacement)
                    self.assertFalse(direct.exact_recovery_input(input_state("webcodex " + changed), PROMPT))

    def test_truncation_at_any_position_never_passes(self):
        for stop in range(len(PROMPT)):
            with self.subTest(length=stop):
                self.assertFalse(direct.exact_recovery_input(input_state("webcodex " + PROMPT[:stop]), PROMPT))

    def test_only_webcodex_mention_may_precede_prompt(self):
        for prefix in ("old draft ", "other-app ", "@webcodex extra ", "webcodexwebcodex ",
                "@webcodex @other ", "instruction override ", "prefix" + PROMPT + " "):
            with self.subTest(prefix=prefix[:30]):
                self.assertFalse(direct.exact_recovery_input(input_state(prefix + PROMPT), PROMPT))

    def test_other_plugin_prefix_is_rejected(self):
        for prefix in ('WebCodex-Hom ', '@WebCodex-Hom ', 'WebCodex-Company ', '@webcodex-extra '):
            with self.subTest(prefix=prefix):
                self.assertFalse(direct.exact_recovery_input(input_state(prefix + PROMPT), PROMPT))

    def test_extra_trailing_text_is_not_sent(self):
        self.assertFalse(direct.exact_recovery_input(input_state("webcodex " + PROMPT + " extra draft"), PROMPT))

    def test_plain_text_mention_and_existing_user_message_are_rejected(self):
        self.assertFalse(direct.exact_recovery_input(input_state(mention=False), PROMPT))
        self.assertFalse(direct.exact_recovery_input(input_state(user_count=1), PROMPT))

    def test_clipboard_whitespace_normalization_preserves_identity(self):
        actual = "\n  webcodex\u00a0" + PROMPT.replace("\n", "\r\n\t") + "\n"
        self.assertTrue(direct.exact_recovery_input(input_state(actual), PROMPT))

    def test_empty_expected_message_is_rejected(self):
        self.assertFalse(direct.exact_recovery_input(input_state("webcodex"), ""))

    def test_direct_module_is_python_37_syntax_compatible(self):
        ast.parse(Path(direct.__file__).read_text(encoding="utf-8"), feature_version=(3, 7))


@unittest.skipUnless(os.environ.get('WEBCODEX_TEST_CHROMIUM'), 'Offline DOM requires Chromium')
class AppMentionDomTests(unittest.TestCase):
    def test_case_variants_keep_app_uri_and_unique_chip_checks(self):
        from playwright.sync_api import sync_playwright
        with sync_playwright() as driver:
            browser = driver.chromium.launch(headless=True, executable_path=os.environ['WEBCODEX_TEST_CHROMIUM'])
            page = browser.new_page()
            page.route('**/*', lambda route: route.abort())
            for name, label, href, copies, accepted in (
                ('webcodex', 'WebCodex', 'app://home', 1, True),
                ('WEBCODEX', 'webcodex', 'app://home', 1, True),
                ('webcodex-hom', 'webcodex-hom', 'app://home', 1, False),
                ('webcodex', 'webcodex', 'app://other', 1, False),
                ('webcodex', 'webcodex', 'app://home', 2, False),
            ):
                chip = (f'<span contenteditable="false" app-mention-name="{name}" '
                        f'app-mention-path="app://home" data-prompt-link-href="{href}">{label}</span>')
                page.set_content('<div id="prompt-textarea" contenteditable="true">' + chip * copies + '</div>')
                with self.subTest(name=name, href=href, copies=copies):
                    self.assertEqual(accepted, page.evaluate('(' + direct.COMPOSER_JS + ')()')['mention'])
            browser.close()


class IndependenceTests(unittest.TestCase):
    def test_direct_has_no_dependency_on_original_application_or_saved_selectors(self):
        source = Path(direct.__file__).read_text(encoding="utf-8")
        for legacy_reference in ("0431acf4-3b23-4a6d-bcf4-c17a4a7e31ad", "923002790447775746",
                "ThumbInput", "xinkaiduihua", "SelectorStore", "APP_DIR"):
            with self.subTest(reference=legacy_reference):
                self.assertFalse(legacy_reference in source, "Legacy application dependency remains: " + legacy_reference)

    def test_direct_does_not_import_the_recorded_selector_store(self):
        tree = ast.parse(Path(direct.__file__).read_text(encoding="utf-8"))
        imports = [node for node in ast.walk(tree) if isinstance(node, ast.ImportFrom)]
        self.assertFalse(any(node.module == "xbot.selector" for node in imports))


class SetupControlTests(unittest.TestCase):
    def setUp(self):
        self.now = 0.0
        self.clicks = []
        self.lookup_delay = 0
        self.state = {"url": "https://chatgpt.com/", "page_focused": True, "page_visible": True,
            "trigger": {"count": 1, "enabled": True, "clear": True,
                "xpath": "(//button)[1]", "center": [100, 100]}}
        self.browser = mock.Mock()
        self.browser.execute_javascript.side_effect = lambda *_: self.state
        self.element = mock.Mock()
        self.element.click.side_effect = lambda **kwargs: self.clicks.append((self.now, kwargs))
        def lookup(*args, **kwargs):
            self.now += self.lookup_delay
            return self.element
        self.browser.find_by_xpath.side_effect = lookup

    def invoke(self, deadline=1):
        def advance(seconds):
            self.now += seconds
        with mock.patch.object(direct.time, "monotonic", side_effect=lambda: self.now), \
                mock.patch.object(direct.time, "sleep", side_effect=advance), \
                mock.patch.object(direct.flow, "cancelled", return_value=False):
            return direct.click_control(self.browser, "trigger", {}, deadline)

    def test_semantic_control_requires_two_stable_observations(self):
        self.invoke()
        self.assertEqual(len(self.clicks), 1)
        self.assertGreaterEqual(self.clicks[0][0], 0.2)

    def test_unfocused_page_cannot_receive_setup_mouse_input(self):
        self.state["page_focused"] = False
        with self.assertRaises(direct.FlowError):
            self.invoke()
        self.assertEqual(self.clicks, [])

    def test_hidden_page_cannot_receive_setup_mouse_input(self):
        self.state["page_visible"] = False
        with self.assertRaises(direct.FlowError):
            self.invoke()
        self.assertEqual(self.clicks, [])

    def test_lookup_that_exceeds_deadline_cannot_click(self):
        self.lookup_delay = 2
        with self.assertRaises(direct.FlowError):
            self.invoke(deadline=1)
        self.assertEqual(self.clicks, [])


class ProSelectionTests(unittest.TestCase):
    def test_handoff_without_snapshot_delegates_to_pro_maximum_policy(self):
        browser, keys = mock.Mock(), mock.Mock()
        expected = {"intelligence": "Pro", "reasoning_effort": "medium", "thinking_profile": {"profile_id": "observed"}}
        with mock.patch("thinking_profiles.select_profile", return_value=expected) as select:
            self.assertEqual(expected, direct.configure_chat(browser, {}, keys))
        select.assert_called_once_with(browser, {}, keys, None)

    def test_child_snapshot_is_passed_without_editing(self):
        browser, keys = mock.Mock(), mock.Mock()
        request = {"thinking_profile": {"profile_id": "exact_saved_profile"}}
        with mock.patch("thinking_profiles.select_profile", return_value={}) as select:
            direct.configure_chat(browser, request, keys)
        select.assert_called_once_with(browser, request, keys, request["thinking_profile"])

    def test_profile_failure_stops_handoff(self):
        import thinking_profiles
        with mock.patch("thinking_profiles.select_profile", side_effect=thinking_profiles.ThinkingProfileError("pro_not_observed_at_maximum")):
            with self.assertRaisesRegex(direct.FlowError, "pro_not_observed_at_maximum"):
                direct.configure_chat(mock.Mock(), {}, mock.Mock())


@unittest.skipUnless(os.environ.get("WEBCODEX_TEST_CHROMIUM"), "Offline DOM test requires existing Chromium path")
class SetupDomTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from playwright.sync_api import sync_playwright
        cls.driver = sync_playwright().start()
        cls.browser = cls.driver.chromium.launch(headless=True, executable_path=os.environ["WEBCODEX_TEST_CHROMIUM"])
        cls.context = cls.browser.new_context(viewport={"width": 1000, "height": 800})
        cls.context.route("**/*", lambda route: route.abort())
        cls.page = cls.context.new_page()

    @classmethod
    def tearDownClass(cls):
        cls.context.close()
        cls.browser.close()
        cls.driver.stop()

    def setUp(self):
        self.page.set_content("""<!doctype html><style>button{padding:12px;margin:8px} [role=menu]{padding:15px}
            [role=slider]{width:80px;height:24px;background:#ddd}</style>
            <div role="group" aria-label="撰写器模式"><button id="chat" aria-pressed="true">聊天</button></div>
            <section id="trigger-parent"><button id="model" data-codex-intelligence-trigger="true"
                data-selected-reasoning-effort="extended">Pro</button></section>
            <div role="menu"><button id="mode" data-model-picker-view-toggle="true">Pro</button>
                <div id="reasoning" data-reasoning-slider="true"><div id="slider" role="slider" tabindex="0"
                    aria-valuemin="0" aria-valuemax="4" aria-valuenow="4"></div></div></div>""")
        self.page.locator("#chat").focus()

    def probe(self):
        return self.page.evaluate("(" + direct.SETUP_JS + ")")

    def test_semantic_chat_pro_and_slider_state_are_read_without_recorded_resources(self):
        state = self.probe()
        self.assertEqual(state["chat"]["count"], 1)
        self.assertEqual(state["chat"]["pressed"], "true")
        self.assertEqual(state["trigger"]["text"], "Pro")
        self.assertEqual(state["intelligence"], "Pro")
        self.assertEqual((state["slider"]["min"], state["slider"]["max"], state["slider"]["value"]), ("0", "4", "4"))
        self.assertEqual(state["effort"], "extended")
        for key in ("chat", "trigger", "slider"):
            self.assertTrue(state[key]["clear"])
            self.assertTrue(state[key]["enabled"])
            self.assertEqual(self.page.locator("xpath=" + state[key]["xpath"]).count(), 1)

    def test_duplicate_chat_and_model_controls_are_ambiguous(self):
        self.page.evaluate("document.querySelector('[role=group]').append(document.querySelector('#chat').cloneNode(true))")
        self.page.evaluate("document.body.append(document.querySelector('#model').cloneNode(true))")
        state = self.probe()
        self.assertEqual(state["chat"]["count"], 2)
        self.assertEqual(state["trigger"]["count"], 2)
        self.assertNotIn("xpath", state["chat"])
        self.assertNotIn("xpath", state["trigger"])

    def test_overlay_marks_each_setup_control_obscured(self):
        self.page.evaluate("""() => {let e=document.createElement('div');e.style='position:fixed;inset:0;z-index:99999;background:white';document.body.append(e)}""")
        state = self.probe()
        for key in ("chat", "trigger", "slider"):
            self.assertFalse(state[key]["clear"])

    def test_native_aria_and_inert_disabled_controls_are_detected(self):
        self.page.evaluate("document.querySelector('#chat').disabled=true")
        self.page.evaluate("document.querySelector('#model').setAttribute('aria-disabled','true')")
        self.page.evaluate("document.querySelector('#reasoning').setAttribute('inert','')")
        state = self.probe()
        for key in ("chat", "trigger", "slider"):
            self.assertFalse(state[key]["enabled"])

    def test_transparent_ancestor_hides_setup_control(self):
        self.page.evaluate("document.querySelector('#trigger-parent').style.opacity='0'")
        self.assertEqual(self.probe()["trigger"]["count"], 0)

    def test_slider_focus_is_read_from_actual_focused_element(self):
        self.page.locator("#slider").focus()
        self.assertTrue(self.probe()["slider_focused"])
        self.page.locator("#chat").focus()
        self.assertFalse(self.probe()["slider_focused"])

    def test_duplicate_intelligence_labels_do_not_confirm_pro(self):
        self.page.evaluate("document.querySelector('[role=menu]').append(document.querySelector('#mode').cloneNode(true))")
        self.assertIsNone(self.probe()["intelligence"])


@unittest.skipUnless(os.environ.get("WEBCODEX_TEST_CHROMIUM"), "Offline DOM test requires existing Chromium path")
class ComposerDomTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from playwright.sync_api import sync_playwright
        cls.driver = sync_playwright().start()
        cls.browser = cls.driver.chromium.launch(headless=True, executable_path=os.environ["WEBCODEX_TEST_CHROMIUM"])
        cls.context = cls.browser.new_context(viewport={"width": 1000, "height": 800})
        cls.context.route("**/*", lambda route: route.abort())
        cls.page = cls.context.new_page()

    @classmethod
    def tearDownClass(cls):
        cls.context.close()
        cls.browser.close()
        cls.driver.stop()

    def setUp(self):
        self.page.set_content("""<!doctype html><style>
          #prompt-textarea{width:600px;min-height:60px;border:1px solid black;padding:8px}
          button{padding:10px}</style>
          <div id="prompt-textarea" role="textbox" contenteditable="true"><span contenteditable="false" app-mention-name="webcodex" app-mention-path="app://test_app" data-prompt-link-href="app://test_app">webcodex</span> test</div>
          <button id="send" aria-label="发送">Send icon</button>
          <button id="elsewhere">Other</button>""")
        self.page.locator("#prompt-textarea").focus()

    def probe(self):
        return self.page.evaluate("(" + direct.COMPOSER_JS + ")")

    def test_chinese_aria_send_is_unique_and_focus_is_confirmed(self):
        value = self.probe()
        self.assertEqual(value["count"], 1)
        self.assertEqual(value["send_count"], 1)
        self.assertTrue(value["send_enabled"])
        self.assertTrue(value["document_visible"])
        self.assertTrue(value["document_focused"])
        self.assertTrue(value["focused"])
        self.assertTrue(value["mention"])
        self.assertEqual(self.page.locator("xpath=" + value["send_xpath"]).get_attribute("id"), "send")

    def test_native_disabled_send_cannot_pass(self):
        self.page.locator("#send").evaluate("e => e.disabled=true")
        self.assertFalse(self.probe()["send_enabled"])

    def test_literal_paste_line_breaks_preserve_the_complete_recovery_prompt(self):
        prompt = 'project=project-test\ngoal_id=hgoal_test\nsource_context_epoch=epoch-A\nlaunch_id=launch-test'
        self.page.locator('#prompt-textarea').evaluate("""(e, text) => {
            e.innerHTML='<span contenteditable="false" app-mention-name="webcodex" app-mention-path="app://test_app" data-prompt-link-href="app://test_app">webcodex</span> <span data-prompt-literal-paste="">'
                + text.replaceAll('\\n','<br>') + '</span>';
        }""", prompt)
        self.assertTrue(direct.exact_recovery_input(self.probe(), prompt))

    def test_current_user_message_markup_prevents_treating_existing_chat_as_empty(self):
        self.page.evaluate("""() => {const e=document.createElement('div');
            e.setAttribute('data-markdown-text-tone','user-message'); e.textContent='owned test message'; document.body.append(e)}""")
        self.assertEqual(self.probe()['user_count'], 1)

    def test_aria_disabled_send_cannot_pass(self):
        self.page.locator("#send").evaluate("e => e.setAttribute('aria-disabled','true')")
        self.assertFalse(self.probe()["send_enabled"])

    def test_two_visible_send_buttons_are_rejected(self):
        self.page.evaluate("document.body.append(document.querySelector('#send').cloneNode(true))")
        value = self.probe()
        self.assertEqual(value["send_count"], 2)
        self.assertFalse(value["send_enabled"])
        self.assertIsNone(value["send_xpath"])

    def test_hidden_duplicate_does_not_change_observed_send_index(self):
        self.page.evaluate("""() => {const e=document.querySelector('#send').cloneNode(true); e.style.display='none'; document.body.prepend(e)}""")
        value = self.probe()
        self.assertEqual(value["send_count"], 1)
        target = self.page.locator("xpath=" + value["send_xpath"])
        self.assertTrue(target.is_visible())

    def test_focus_leaving_editor_is_observed(self):
        self.page.locator("#elsewhere").focus()
        self.assertFalse(self.probe()["focused"])

    def test_plain_text_webcodex_is_not_a_selected_mention(self):
        self.page.locator("#prompt-textarea").evaluate("e => e.textContent='@webcodex test'")
        self.assertFalse(self.probe()["mention"])

    def test_other_plugin_token_is_rejected(self):
        for name in ('WebCodex-Hom', 'WebCodex-Company', 'webcodex-extra'):
            with self.subTest(name=name):
                self.page.locator('[app-mention-name]').evaluate("""(e, name) => {
                    e.setAttribute('app-mention-name', name); e.textContent = name;
                }""", name)
                self.assertFalse(self.probe()['mention'])

    def test_generic_data_type_and_plain_noneditable_text_are_not_app_mentions(self):
        for markup in ('<p data-type="paragraph">@webcodex</p>',
                '<span data-type="mention">webcodex</span>',
                '<span contenteditable="false">webcodex</span>'):
            self.page.locator('#prompt-textarea').evaluate('(e, html) => e.innerHTML=html',markup)
            self.assertFalse(self.probe()['mention'])

    def test_duplicate_app_tokens_or_disagreeing_app_paths_are_rejected(self):
        self.page.locator('#prompt-textarea').evaluate('e => e.append(e.firstElementChild.cloneNode(true))')
        self.assertFalse(self.probe()['mention'])
        self.setUp()
        self.page.locator('[app-mention-name]').evaluate("e => e.setAttribute('data-prompt-link-href','app://other')")
        self.assertFalse(self.probe()['mention'])

    def test_multiple_preferred_composers_are_ambiguous(self):
        self.page.evaluate("document.body.append(document.querySelector('#prompt-textarea').cloneNode(true))")
        value = self.probe()
        self.assertEqual(value["count"], 2)
        self.assertIsNone(value["xpath"])
        self.assertFalse(value["focused"])


if __name__ == "__main__":
    unittest.main()
