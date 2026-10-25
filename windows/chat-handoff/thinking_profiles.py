"""Discover and select observed intensity profiles on an owned, empty chat.

The hidden slider supplies read-only numeric semantics. Trusted input targets its
visible keyboard menu item. No model option is clicked and raw effort is never
treated as a separate control: Pro currently reports raw effort ``medium``.
"""
import hashlib
import json
import time

import handoff_flow as flow


class ThinkingProfileError(RuntimeError):
    pass


PROBE_JS = r'''function(){
 const norm=s=>(s||'').replace(/\s+/g,' ').trim();
 const visible=e=>{if(!e||!e.isConnected||e.closest('[hidden],[aria-hidden="true"]'))return false;
   for(let p=e;p;p=p.parentElement){const s=getComputedStyle(p);if(s.display==='none'||s.visibility!=='visible'||Number(s.opacity)<=0)return false;}
   const r=e.getBoundingClientRect();return r.width>1&&r.height>1;};
 const menus=[...document.querySelectorAll('[role="menu"]')].filter(visible);
 const owners=menus.flatMap(e=>[...e.querySelectorAll('[role="menuitem"][data-reasoning-slider="true"]')]).filter(visible);
 const owner=owners.length===1?owners[0]:null;
 const sliders=owner?[...owner.querySelectorAll('[role="slider"]')]:[];
 const slider=sliders.length===1?sliders[0]:null;
 const modes=menus.flatMap(e=>[...e.querySelectorAll('[data-model-picker-view-toggle="true"]')]).filter(visible);
 const radios=menus.flatMap(e=>[...e.querySelectorAll('[role="menuitemradio"]')]).filter(visible);
 const selected=radios.filter(e=>e.getAttribute('aria-checked')==='true'||e.getAttribute('data-model-selected')==='true');
 const triggers=[...document.querySelectorAll('button[data-codex-intelligence-trigger="true"]')].filter(visible);
 let control=null;
 if(owner){const r=owner.getBoundingClientRect(),x=r.x+r.width/2,y=r.y+r.height/2,hit=document.elementFromPoint(x,y);
   control={xpath:`(//${owner.localName})[${[...document.querySelectorAll(owner.localName)].indexOf(owner)+1}]`,center:[x,y],
     clear:!!hit&&(hit===owner||owner.contains(hit)),enabled:!owner.matches(':disabled')&&!owner.closest('[data-disabled],[aria-disabled="true"],[inert]')&&!owner.querySelector('[aria-disabled="true"]'),
     focused:owner===document.activeElement||owner.contains(document.activeElement),keys:owner.getAttribute('aria-keyshortcuts')};}
 return {origin:location.origin,path:location.pathname,menu_count:menus.length,owner_count:owners.length,
   label:modes.length===1?norm(modes[0].innerText):null,
   model:selected.length===1?norm(selected[0].innerText):null,selected_model_count:selected.length,
   model_options:radios.map(e=>({label:norm(e.innerText),selected:e.getAttribute('aria-checked')==='true'||e.getAttribute('data-model-selected')==='true'})),
   reasoning_effort:triggers.length===1?triggers[0].getAttribute('data-selected-reasoning-effort'):null,
   value:slider?slider.getAttribute('aria-valuenow'):null,min:slider?slider.getAttribute('aria-valuemin'):null,max:slider?slider.getAttribute('aria-valuemax'):null,
   description:owner?norm((owner.getAttribute('aria-describedby')||'').split(/\s+/).map(id=>document.getElementById(id)?.textContent||'').join(' ')).slice(0,500):null,
   control,focused:document.hasFocus(),visible:document.visibilityState==='visible'};
}'''


def _read(browser, request, deadline):
    if flow.cancelled(request) or time.monotonic() >= deadline:
        raise ThinkingProfileError("thinking_selection_cancelled")
    value = browser.execute_javascript(PROBE_JS)
    value = json.loads(value) if isinstance(value, str) else value
    if (not isinstance(value, dict) or value.get("origin") != "https://chatgpt.com"
            or value.get("path") != "/" or value.get("menu_count") != 1
            or value.get("owner_count") != 1 or not value.get("focused") or not value.get("visible")):
        raise ThinkingProfileError("thinking_menu_not_ready")
    control = value.get("control") or {}
    if (not control.get("enabled") or not control.get("clear")
            or not {"ArrowLeft", "ArrowRight"} <= set((control.get("keys") or "").split())):
        raise ThinkingProfileError("thinking_control_disabled_or_unknown")
    _profile(value)  # validates all fields before any input can target them
    return value


def _profile(state):
    try:
        numbers = [float(state[key]) for key in ("value", "min", "max")]
        if any(not number.is_integer() for number in numbers):
            raise ValueError()
        value, lower, upper = map(int, numbers)
        if not lower <= value <= upper or not 0 <= upper - lower <= 32:
            raise ValueError()
        label, effort, model = state["label"], state["reasoning_effort"], state.get("model")
        options = state["model_options"]
        if (not isinstance(label, str) or not 1 <= len(label) <= 160
                or effort is not None and (not isinstance(effort, str) or len(effort) > 64)
                or model is not None and (not isinstance(model, str) or not 1 <= len(model) <= 160)
                or state.get("selected_model_count", 0) > 1
                or not isinstance(options, list) or len(options) > 16):
            raise ValueError()
        for option in options:
            if (set(option) != {"label", "selected"} or not isinstance(option["label"], str)
                    or not 1 <= len(option["label"]) <= 160 or type(option["selected"]) is not bool):
                raise ValueError()
        profile = {"schema_version": 1, "label": label, "reasoning_effort": effort,
                   "model": model, "model_options": options,
                   "slider_value": value, "slider_min": lower, "slider_max": upper}
        profile["profile_id"] = _profile_id(profile)
        return profile
    except (ValueError, TypeError, KeyError, OverflowError):
        raise ThinkingProfileError("thinking_profile_not_observed") from None


def _profile_id(profile):
    stable = {key: value for key, value in profile.items() if key != "profile_id"}
    return "thinking_" + hashlib.sha256(json.dumps(stable, sort_keys=True,
        ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()).hexdigest()[:24]


def _context(profile):
    return {key: profile[key] for key in ("model", "model_options", "slider_min", "slider_max")}


def _open(browser, request, deadline):
    import edge_handoff as handoff
    while time.monotonic() < deadline:
        if flow.cancelled(request):
            raise ThinkingProfileError("thinking_selection_cancelled")
        state = handoff.setup_state(browser)
        if state["chat"].get("count") == 1 and state["trigger"].get("count") == 1:
            break
        time.sleep(.25)
    else:
        raise ThinkingProfileError("thinking_composer_not_ready")
    composer = handoff.composer(browser)
    if not handoff.empty_app_composer(composer):
        raise ThinkingProfileError("thinking_chat_not_empty")
    if state["chat"].get("pressed") != "true":
        handoff.click_control(browser, "chat", request, deadline)
    if handoff.setup_state(browser)["chat"].get("pressed") != "true":
        raise ThinkingProfileError("thinking_chat_mode_not_confirmed")
    if handoff.setup_state(browser).get("menu_count") == 0:
        handoff.click_control(browser, "trigger", request, deadline)
    return _read(browser, request, deadline)


def _focus(browser, request, deadline):
    state = _read(browser, request, deadline)
    if state["control"].get("focused"):
        return state
    before = state["control"]
    time.sleep(.15)
    current = _read(browser, request, deadline)["control"]
    if (current["xpath"] != before["xpath"]
            or any(abs(a-b) > 2 for a, b in zip(current["center"], before["center"]))):
        raise ThinkingProfileError("thinking_control_changed")
    browser.find_by_xpath(current["xpath"], timeout=2).click(delay_after=.25,
        before_actual=lambda: not flow.cancelled(request) and time.monotonic() < deadline)
    # Clicking the track can move the value. Never calculate a step from the
    # pre-click reading (the live 4 -> 2 focus click exposed this behavior).
    state = _read(browser, request, deadline)
    if not state["control"].get("focused"):
        raise ThinkingProfileError("thinking_keyboard_focus_missing")
    return state


def _step(browser, request, keys, direction, context, deadline):
    before = _read(browser, request, deadline)
    if not before["control"].get("focused") or _context(_profile(before)) != context:
        raise ThinkingProfileError("thinking_context_changed")
    keys("{RIGHT}" if direction > 0 else "{LEFT}", contains_hotkey=True, delay_after=.3)
    end = min(deadline, time.monotonic() + 2)
    while time.monotonic() < end:
        after = _read(browser, request, deadline)
        if _context(_profile(after)) != context:
            raise ThinkingProfileError("thinking_model_or_range_changed")
        difference = _profile(after)["slider_value"] - _profile(before)["slider_value"]
        if difference * direction > 0:
            return after
        if difference:
            raise ThinkingProfileError("thinking_direction_changed")
        time.sleep(.1)
    raise ThinkingProfileError("thinking_slider_did_not_advance")


def _move_to(browser, request, keys, target, context, deadline):
    state = _read(browser, request, deadline)
    if _context(_profile(state)) != context:
        raise ThinkingProfileError("thinking_model_or_range_changed")
    if _profile(state)["slider_value"] != target:
        state = _focus(browser, request, deadline)
    for _ in range(34):
        current = _profile(state)
        if _context(current) != context:
            raise ThinkingProfileError("thinking_model_or_range_changed")
        if current["slider_value"] == target:
            return state
        state = _step(browser, request, keys, 1 if target > current["slider_value"] else -1, context, deadline)
    raise ThinkingProfileError("thinking_target_not_reached")


def _close(browser, request, profile, deadline):
    import edge_handoff as handoff
    editor = handoff.wait_composer(browser, request, deadline)
    browser.find_by_xpath(editor["xpath"], timeout=2).click(delay_after=.25,
        before_actual=lambda: not flow.cancelled(request) and time.monotonic() < deadline)
    state = handoff.setup_state(browser)
    # The current Chinese trigger includes its control caption in innerText;
    # the open menu's exact profile label remains unchanged (e.g. Pro at max).
    labels = {profile["label"], "思考强度 " + profile["label"]}
    if (state.get("menu_count") or state["trigger"].get("count") != 1
            or state["trigger"].get("text") not in labels
            or state.get("effort") != profile["reasoning_effort"]):
        raise ThinkingProfileError("thinking_profile_readback_failed")


def discover_profiles(browser, request, keys=None):
    """Enumerate the current model's observed intensity positions, then restore."""
    deadline = time.monotonic() + 90
    keys = keys or browser.keys
    initial = _profile(_open(browser, request, deadline))
    context = _context(initial)
    observed = {initial["slider_value"]: initial}
    state = _focus(browser, request, deadline)
    for direction, bound in ((-1, "slider_min"), (1, "slider_max")):
        for _ in range(34):
            profile = _profile(state)
            if _context(profile) != context:
                raise ThinkingProfileError("thinking_model_or_range_changed")
            observed[profile["slider_value"]] = profile
            if profile["slider_value"] == initial[bound]:
                break
            state = _step(browser, request, keys, direction, context, deadline)
        else:
            raise ThinkingProfileError("thinking_range_not_reached")
    restored = _profile(_move_to(browser, request, keys, initial["slider_value"], context, deadline))
    if restored != initial:
        raise ThinkingProfileError("thinking_restore_failed")
    _close(browser, request, restored, deadline)
    profiles = [observed[value] for value in sorted(observed)]
    highest = profiles[-1]
    return {"schema_version": 1, "scope": "current_model_intensity_only", "observed_at": flow.timestamp(),
            "model_context": context, "profiles": profiles, "restored_profile_id": restored["profile_id"],
            "default_profile_id": highest["profile_id"] if highest["label"] == "Pro" else None,
            "execution_availability": "not_verified_by_setting_selection"}


def select_profile(browser, request, keys, selection=None):
    deadline = time.monotonic() + 60
    initial = _profile(_open(browser, request, deadline))
    context = _context(initial)
    if selection is not None:
        try:
            valid = (isinstance(selection, dict) and set(selection) == set(initial)
                     and selection.get("schema_version") == 1
                     and selection.get("profile_id") == _profile_id(selection))
        except (TypeError, ValueError):
            valid = False
        if not valid:
            raise ThinkingProfileError("thinking_profile_snapshot_invalid")
        if _context(selection) != context:
            raise ThinkingProfileError("thinking_profile_model_or_range_mismatch")
        target = selection.get("slider_value")
        if type(target) is not int or not initial["slider_min"] <= target <= initial["slider_max"]:
            raise ThinkingProfileError("thinking_profile_snapshot_invalid")
    else:
        target = initial["slider_max"]
    chosen = _profile(_move_to(browser, request, keys, target, context, deadline))
    if selection is not None and chosen != selection:
        raise ThinkingProfileError("thinking_profile_snapshot_changed")
    if selection is None and chosen["label"] != "Pro":
        raise ThinkingProfileError("pro_not_observed_at_maximum")
    _close(browser, request, chosen, deadline)
    return {"intelligence": chosen["label"], "reasoning_effort": chosen["reasoning_effort"],
            "thinking_profile": chosen}
