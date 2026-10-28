"""Open and verify one Edge recovery chat using browser input events."""
import json
from pathlib import Path
import re
import time
from urllib.parse import urlsplit

import handoff_flow as flow
import approval_guard

class FlowError(RuntimeError):
    pass


COMPOSER_JS = r'''function() {
''' + approval_guard.USER_MESSAGES_JS + r'''
  const shown = e => !!e && !!e.getClientRects().length && getComputedStyle(e).visibility === 'visible';
  const edits = [...document.querySelectorAll('[contenteditable="true"],textarea')].filter(shown);
  const preferred = edits.filter(e => e.id === 'prompt-textarea' || e.getAttribute('role') === 'textbox'
    || /询问|消息|Message|Ask/.test(e.getAttribute('data-placeholder') || e.getAttribute('placeholder') || ''));
  const candidates = preferred.length ? preferred : edits;
  const e = candidates.length === 1 ? candidates[0] : null;
  const all = [...document.querySelectorAll('[contenteditable="true"],textarea')];
  const allButtons = [...document.querySelectorAll('button')];
  const buttons = allButtons.filter(shown);
  const send = buttons.filter(b => b.getAttribute('data-testid') === 'send-button'
    || ['发送','发送提示','发送消息','Send','Send prompt','Send message'].includes(b.getAttribute('aria-label')));
  return {url:location.href, count:candidates.length, text:e ? (e.value || e.innerText || e.textContent || '') : null,
    document_visible:document.visibilityState === 'visible', document_focused:document.hasFocus(),
    html:e ? e.innerHTML : '', focused:!!e && (e === document.activeElement || e.contains(document.activeElement)),
    xpath:e ? `(//*[@contenteditable='true' or self::textarea])[${all.indexOf(e)+1}]` : null,
    user_count:users.length,
    send_count:send.length, send_enabled:send.length === 1 && !send[0].matches(':disabled')
      && send[0].getAttribute('aria-disabled') !== 'true',
    send_xpath:send.length === 1 ? `(//button)[${allButtons.indexOf(send[0])+1}]` : null,
    mention:!!e && [...e.querySelectorAll('[contenteditable="false"][app-mention-name]')]
      .filter(n => (n.getAttribute('app-mention-name') || '').toLowerCase() === 'webcodex'
        && /^app:\/\/[A-Za-z0-9_-]+$/.test(n.getAttribute('app-mention-path') || '')
        && n.getAttribute('app-mention-path') === n.getAttribute('data-prompt-link-href')
        && (n.textContent || '').replace(/\s+/g,' ').trim().toLowerCase() === 'webcodex').length === 1};
}'''

SETUP_JS = r'''function(){
  const norm=s=>(s||'').replace(/\s+/g,' ').trim();
  const visible=e=>{if(!e||!e.isConnected||e.closest('[hidden],[aria-hidden="true"]'))return false;
    for(let p=e;p;p=p.parentElement){const s=getComputedStyle(p);if(s.display==='none'||s.visibility!=='visible'||Number(s.opacity)<=0)return false;}
    const r=e.getBoundingClientRect();return r.width>1&&r.height>1;};
  const control=nodes=>{const a=[...nodes].filter(visible);if(a.length!==1)return {count:a.length};
    const e=a[0],r=e.getBoundingClientRect(),x=r.x+r.width/2,y=r.y+r.height/2,hit=document.elementFromPoint(x,y);
    const tag=e.localName,idx=[...document.querySelectorAll(tag)].indexOf(e)+1;
    return {count:1,xpath:`(//${tag})[${idx}]`,center:[x,y],clear:!!hit&&(hit===e||e.contains(hit)),
      enabled:!e.matches(':disabled')&&!e.closest('[aria-disabled="true"],[inert]'),
      text:norm(e.innerText),pressed:e.getAttribute('aria-pressed'),value:e.getAttribute('aria-valuenow'),
      min:e.getAttribute('aria-valuemin'),max:e.getAttribute('aria-valuemax')};};
  const groups=[...document.querySelectorAll('[role="group"]')].filter(e=>['撰写器模式','Composer mode'].includes(e.getAttribute('aria-label')));
  const chat=groups.flatMap(e=>[...e.querySelectorAll('button')]).filter(e=>['聊天','Chat'].includes(norm(e.innerText)));
  const triggers=[...document.querySelectorAll('button[data-codex-intelligence-trigger="true"],button[aria-label="选择 ChatGPT 模型"],button[aria-label="Choose ChatGPT model"]')];
  const menus=[...document.querySelectorAll('[role="menu"]')].filter(visible);
  const modes=menus.flatMap(e=>[...e.querySelectorAll('[data-model-picker-view-toggle="true"]')]).filter(visible);
  const sliders=menus.flatMap(e=>[...e.querySelectorAll('[data-reasoning-slider="true"] [role="slider"]')]);
  const trigger=triggers.filter(visible)[0];
  const active=document.activeElement;
  return {url:location.href,chat:control(chat),trigger:control(triggers),slider:control(sliders),
    menu_count:menus.length,intelligence:modes.length===1?norm(modes[0].innerText):null,
    effort:trigger?trigger.getAttribute('data-selected-reasoning-effort'):null,
    slider_focused:!!active&&(active.getAttribute('role')==='slider'||!!active.closest('[data-reasoning-slider="true"]')),
    page_focused:document.hasFocus(),page_visible:document.visibilityState==='visible'};
}'''


def setup_state(browser):
    value = browser.execute_javascript(SETUP_JS)
    if isinstance(value, str):
        value = json.loads(value)
    if not isinstance(value, dict):
        raise FlowError('invalid_setup_observation')
    u = urlsplit(value.get('url', ''))
    if u.scheme != 'https' or u.hostname != 'chatgpt.com' or u.username or u.password:
        raise FlowError('unexpected_origin')
    return value


def click_control(browser, key, request, deadline):
    previous = None
    while time.monotonic() < deadline:
        if flow.cancelled(request):
            raise FlowError('cancelled_before_send')
        state = setup_state(browser)
        item = state.get(key, {})
        usable = (item.get('count') == 1 and item.get('enabled') and item.get('clear')
                  and state.get('page_focused') and state.get('page_visible'))
        if usable and previous and previous.get('xpath') == item['xpath'] and all(
                abs(a-b) <= 2 for a,b in zip(previous['center'],item['center'])):
            element = browser.find_by_xpath(item['xpath'], timeout=2)
            fresh = setup_state(browser)
            check = fresh.get(key, {})
            if (fresh['url'] == state['url'] and check.get('xpath') == item['xpath']
                    and check.get('enabled') and check.get('clear') and check.get('count') == 1
                    and fresh.get('page_focused') and fresh.get('page_visible')
                    and all(abs(a-b)<=2 for a,b in zip(check['center'],item['center']))):
                if flow.cancelled(request) or time.monotonic() >= deadline:
                    raise FlowError('cancelled_before_send')
                element.click(simulative=True, move_mouse=True, delay_after=0.25,
                    before_actual=lambda: not flow.cancelled(request) and time.monotonic() < deadline)
                return
        previous = item if usable else None
        time.sleep(0.2)
    raise FlowError('setup_control_not_ready_' + key)


def configure_chat(browser, request, keys):
    """Apply an exact discovered profile; handoff defaults to observed Pro max."""
    if request.get("thinking_policy") == "inherit_child" and request.get("thinking_profile") is None:
        return {"thinking_adjustment": {"status": "not_requested"}}
    import thinking_profiles
    try:
        return thinking_profiles.select_profile(browser, request, keys, request.get("thinking_profile"))
    except thinking_profiles.ThinkingProfileError as error:
        raise FlowError(str(error)) from None


def composer(browser):
    result = browser.execute_javascript(COMPOSER_JS)
    if isinstance(result, str):
        result = json.loads(result)
    if not isinstance(result, dict):
        raise FlowError('invalid_composer_observation')
    u = urlsplit(result.get('url', ''))
    if u.scheme != 'https' or u.hostname != 'chatgpt.com' or u.username or u.password:
        raise FlowError('unexpected_origin')
    return result


def wait_composer(browser, request, deadline):
    while time.monotonic() < deadline:
        if flow.cancelled(request):
            raise FlowError('cancelled_before_send')
        state = composer(browser)
        if state.get('count') == 1:
            return state
        time.sleep(0.25)
    raise FlowError('composer_not_ready')


def empty_app_composer(state):
    """An empty draft may already contain the one verified app chip."""
    text = (state.get('text') or '').strip().casefold()
    return not state.get('user_count') and (not text or (
        state.get('mention') is True and text in ('webcodex', '@webcodex')))


def exact_recovery_input(state, message):
    norm = lambda value: re.sub(r'\s+', ' ', value or '').strip()
    actual, expected = norm(state.get('text')), norm(message)
    if not expected or not actual.endswith(expected):
        return False
    prefix = actual[:-len(expected)].strip()
    return prefix.casefold() in ('', 'webcodex', '@webcodex') and state.get('mention') and not state.get('user_count')


def run(browser, request, should_stop=lambda **_: False, *, launch_claim=None):
    sent = False
    step = 'open_new_tab'
    try:
        message = flow.prepare_message(request, lambda: should_stop(after_send=sent), launch_claim=launch_claim)
        launch_claim = None
        browser.activate()
        def check():
            if flow.cancelled(request):
                raise FlowError('cancelled_before_send')
            composer(browser)  # validates this returned browser's origin

        step = 'configure_chat'
        configuration = configure_chat(browser, request, browser.keys)
        configuration.update(browser.identity())
        flow.write_json(flow.runtime_dir()/'edge-launches'/(request['launch_id']+'.browser.json'), configuration)

        step = 'select_webcodex'
        deadline = time.monotonic() + 30
        state = wait_composer(browser, request, deadline)
        if not empty_app_composer(state):
            raise FlowError('new_chat_not_empty')
        editor = browser.find_by_xpath(state['xpath'], timeout=3)
        editor.click(simulative=True, move_mouse=True, delay_after=0,
            before_actual=lambda: not flow.cancelled(request) and time.monotonic() < deadline)
        state = composer(browser)
        if not state.get('focused') or not state.get('document_focused') or not state.get('document_visible'):
            raise FlowError('composer_focus_not_confirmed')
        check()
        if not state.get('mention'):
            browser.keys('@webcodex', contains_hotkey=False, force_ime_eng=True, delay_after=0.5)
            check()
            browser.keys('{SPACE}', contains_hotkey=True, delay_after=0.5)
        state = wait_composer(browser, request, deadline)
        # Do not silently send an unbound ordinary text @mention.
        if not state.get('mention'):
            raise FlowError('webcodex_mention_not_selected')
        step = 'input_recovery_message'
        check()
        # Browser text insertion preserves newlines without clipboard changes
        # or interpreting prompt characters as keyboard shortcuts.
        editor = browser.find_by_xpath(state['xpath'], timeout=3)
        editor.clipboard_input(message, append=True, delay_after=0.5)
        state = composer(browser)
        if not exact_recovery_input(state, message):
            raise FlowError('recovery_input_verification_failed')
        if not state.get('send_enabled'):
            raise FlowError('send_button_not_ready')
        if flow.cancelled(request):
            raise FlowError('cancelled_before_send')
        send_button = browser.find_by_xpath(state['send_xpath'], timeout=3)
        final_state = composer(browser)
        if (not exact_recovery_input(final_state, message) or not final_state.get('document_visible')
                or not final_state.get('document_focused') or not final_state.get('focused')
                or not final_state.get('send_enabled') or final_state.get('send_xpath') != state['send_xpath']):
            raise FlowError('send_target_changed')
        if flow.cancelled(request) or should_stop(force_refresh=True):
            raise FlowError('cancelled_before_send')
        intents = flow.runtime_dir() / 'send-intents'
        intents.mkdir(parents=True, exist_ok=True)
        with (intents / (request['launch_id'] + '.json')).open('x', encoding='utf-8') as handle:
            json.dump({'launch_id': request['launch_id'], 'recorded_at': flow.timestamp()}, handle)
        step = 'send_once'
        def before_send():
            nonlocal sent
            if flow.cancelled(request) or should_stop(force_refresh=True):
                return False
            sent = True  # uncertainty after the input boundary is never replayable
            return True
        send_button.click(simulative=True, move_mouse=True, delay_after=0, before_actual=before_send)
        step = 'approvals'
        result = flow.after_send(browser)
        return result
    except Exception as error:
        result = flow.save_result(request, {'status': 'unknown' if sent else 'blocked',
            'reason': step + ':' + (str(error) if isinstance(error, FlowError) else type(error).__name__)})
        return result

    finally:
        flow._ACTIVE_REQUEST = None
        flow._SHOULD_STOP = lambda: False
