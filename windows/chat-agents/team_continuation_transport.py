"""Bounded transport of one persisted event notification to its original chat."""
from __future__ import annotations
import json
import re
import time
import uuid
from pathlib import Path

import approval_guard
import edge_handoff
import handoff_controller
import playwright_extension_browser as browser_api
from team_continuation import route_matches
from team_continuation_approval import handle as handle_approval

PROBE = r'''function(_element, argument) {
  const a=JSON.parse(argument);
''' + approval_guard.USER_MESSAGES_JS + r'''
  const all=users;
  const anchors=a.anchor_launch_id&&a.anchor_task_id?all.filter(e=>(e.textContent||'').includes(a.anchor_launch_id)&&(e.textContent||'').includes(a.anchor_task_id)):[];
  const wake=all.filter(e=>a.attempt_id&&(e.textContent||'').includes(a.attempt_id)&&(e.textContent||'').includes(a.task_id));
  const visible=e=>!!e&&e.getClientRects().length>0&&!e.closest('[hidden],[aria-hidden="true"]');
  const busy=[...document.querySelectorAll('button')].some(e=>visible(e)&&
    (/^(?:stop-button|stop-streaming-button|stop-generation-button)$/.test(e.getAttribute('data-testid')||'')||
     /^(?:Stop|Stop generating|Stop streaming|停止|停止生成|停止流式传输)$/i.test(e.getAttribute('aria-label')||'')));
  return {url:location.href,anchor_count:anchors.length,wake_count:wake.length,
    wake_last:wake.length===1&&wake[0]===all[all.length-1],user_count:all.length,busy,
    visible:document.visibilityState==='visible',modal:[...document.querySelectorAll('[role="dialog"],[role="alertdialog"]')].some(visible)};
}'''


def inspect(browser, route, target, attempt=None):
    value=browser.execute_javascript(PROBE,json.dumps({**route,"task_id":target["task_id"],"attempt_id":attempt},ensure_ascii=False))
    return json.loads(value) if isinstance(value,str) else value


def empty_draft(state):
    value=(state.get("text") or "").strip().casefold()
    return state.get("count")==1 and (not value or state.get("mention") is True and value in ("webcodex","@webcodex"))


def exact_draft(state,message):
    norm=lambda s:re.sub(r"\s+"," ",s or "").strip()
    actual,expected=norm(state.get("text")),norm(message)
    if actual==expected:return True
    if not state.get("mention") or not actual.endswith(expected):return False
    return actual[:-len(expected)].strip().casefold() in ("webcodex","@webcodex")


def run(control,target,route,attempt,state_reader,*,cancelled=lambda:False,runtime_root,
        session_factory=None,lock_factory=None,inspector=inspect,clock=time.monotonic,sleep=time.sleep):
    session_factory=session_factory or browser_api.persistent_session
    lock_factory=lock_factory or handoff_controller.watcher_lock
    deadline=clock()+60
    initial=state_reader()
    result={"target":target,"state":"not_started","business_verified":False}
    if attempt is not None and attempt.get("target")!=target:
        return dict(result,state="deferred",reason="attempt_target_changed")
    def stopped():
        return cancelled() or clock()>=deadline or state_reader()!=initial
    def change(action,current):
        return control.call(action,target=target,attempt_id=current["attempt_id"])
    if initial.get("mode")!="running" or stopped():
        return dict(result,reason="scheduler_not_running")
    with lock_factory(Path(runtime_root)/"edge-dispatch"):
        with session_factory() as session:
            if stopped():return dict(result,reason="cancelled")
            # A viewer of the recorded conversation, never a new conversation.
            session._transport.call("browser_tabs",{"action":"new","url":route["url"]})
            browser=browser_api.Browser(session,str(uuid.uuid4()))
            browser._rpc("initialize");browser.activate()
            probe=None
            while not stopped():
                probe=inspector(browser,route,target,attempt["attempt_id"] if attempt else None)
                if route_matches(route,target,probe):break
                sleep(min(.25,max(0,deadline-clock())))
            else:return dict(result,reason="original_chat_not_verified")
            if attempt and attempt.get("state") in ("dispatching","unknown","delivered"):
                if probe.get("wake_count")==1:
                    change("delivered",attempt)
                    approval=handle_approval(browser,control,target,route,attempt,probe,stopped,runtime_root,
                        timeout=min(30,max(1,deadline-clock())))
                    change("running" if probe.get("busy") else "idle",attempt)
                    return dict(result,state="observed",attempt_id=attempt["attempt_id"],running=probe.get("busy"),approval=approval)
                return dict(result,state="unknown",attempt_id=attempt["attempt_id"],reason="original_send_unconfirmed")
            if probe.get("busy") or probe.get("modal"):
                if probe.get("busy"):control.call("busy",target=target)
                return dict(result,state="busy",reason="original_chat_busy")
            # Require repeated idle observation, never infer idle from task.active.
            sleep(.25)
            fresh=inspector(browser,route,target)
            if stopped() or fresh!=probe:return dict(result,state="busy",reason="chat_state_changed")
            editor_state=edge_handoff.composer(browser)
            if not empty_draft(editor_state):
                control.call("draft",target=target)
                return dict(result,state="busy",reason="composer_has_user_draft")
            if attempt is None:
                claim=control.call("claim",target=target)
                if not claim.get("claimed"):
                    return dict(result,state="deferred")
                attempt=claim["attempt"]
            if attempt.get("target")!=target or attempt.get("state")!="claimed":
                return dict(result,state="deferred",reason="attempt_not_claimed")
            crossed=False
            try:
                message=("WebCodex 框架协作事件通知（不是用户新增授权）。\n"
                    f"attempt_id={attempt['attempt_id']}\ntask_id={target['task_id']}\n"
                    "请在当前对话保持原身份与原Goal，调用 team_collaboration action=read 读取公开事件，"
                    "按 next_sequence 分页并 acknowledge 已收到的事件；在原任务授权内继续处理。"
                    "完成交付后发布公开通知；等待依赖时登记 wait。不要重复创建任务或Goal，"
                    "不要因通知而扩大权限；若事件已处理，仅核对原产物。")
                editor=browser.find_by_xpath(editor_state["xpath"],timeout=3)
                editor.click(delay_after=0,before_actual=lambda:not stopped())
                focused=edge_handoff.composer(browser)
                if not all(focused.get(k) for k in ("focused","document_focused","document_visible")):
                    change("not_sent",attempt);return dict(result,state="not_sent",reason="composer_not_focused")
                editor.clipboard_input(message,append=True,delay_after=.25)
                prepared=edge_handoff.composer(browser)
                if (stopped() or not exact_draft(prepared,message)
                        or not prepared.get("send_enabled") or prepared.get("send_count")!=1):
                    change("not_sent",attempt);return dict(result,state="not_sent",reason="send_target_not_ready")
                # Persist intent before entering native input. A lost response or
                # exception after this call is uncertainty, never a fresh send.
                crossed=True
                permit=change("dispatch",attempt)
                if not permit.get("send_permitted"):
                    return dict(result,state="deferred",reason="send_already_fenced")
                send=browser.find_by_xpath(prepared["send_xpath"],timeout=3)
                def before_actual():
                    current=edge_handoff.composer(browser)
                    observed=inspector(browser,route,target)
                    return (not stopped() and route_matches(route,target,observed) and not observed.get("busy")
                        and not observed.get("modal") and current.get("send_xpath")==prepared["send_xpath"]
                        and current.get("send_enabled") and exact_draft(current,message))
                send.click(delay_after=0,before_actual=before_actual)
                while not stopped():
                    observed=inspector(browser,route,target,attempt["attempt_id"])
                    if observed.get("wake_count")==1 and approval_guard.chat_url(observed.get("url"))==route["url"]:
                        change("delivered",attempt)
                        # Guard only this exact last system notification; old
                        # approval cards and other apps remain ineligible.
                        approval=handle_approval(browser,control,target,route,attempt,observed,stopped,runtime_root,
                            timeout=min(30,max(1,deadline-clock())))
                        change("running" if observed.get("busy") else "idle",attempt)
                        return dict(result,state="delivered",attempt_id=attempt["attempt_id"],running=observed.get("busy"),approval=approval)
                    sleep(min(.25,max(0,deadline-clock())))
                change("unknown",attempt)
                return dict(result,state="unknown",attempt_id=attempt["attempt_id"])
            except Exception:
                try:change("unknown" if crossed else "not_sent",attempt)
                except Exception:pass
                raise
