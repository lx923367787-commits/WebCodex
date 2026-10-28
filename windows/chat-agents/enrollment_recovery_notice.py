"""One durable framework notice for an explicitly authorized original enrollment.

The original business prompt and grant are never copied or resent. This module
uses the existing browser worker, exact original URL and current Server queue.
After its input fence exists, any uncertain result is observe-only forever.
"""
from pathlib import Path
import json
import hashlib
import os
import time

import approval_guard
import edge_handoff
from edge_tools import read_json
from team_continuation_transport import inspect, empty_draft, exact_draft


def pending(status, request_id):
    enrollment=status.get("enrollment_status",{})
    return (status.get("request_id")==request_id and status.get("queue_state") in ("unknown","running")
            and status.get("cancel_requested") is False and enrollment.get("state")=="issued"
            and enrollment.get("run_state")=="pending" and enrollment.get("claim_expired") is False
            and enrollment.get("window_enabled") is False and enrollment.get("principal_revoked") is False)


def notify(browser, control, operation, config, url, key, path, permit, stopped,
           *, inspector=inspect, clock=time.monotonic, sleep=time.sleep):
    if not permit or permit.get("notify_original") is not True:
        raise ValueError("original_notice_operator_authorization_required")
    binding=operation["status"]["binding"]
    target={"task_id":binding["task_id"]}
    route={"url":url,"anchor_launch_id":config["launch_id"],"anchor_task_id":config["task_id"]}
    message=("WebCodex 框架接入恢复通知。依据已记录的用户恢复授权，恢复原请求；不新增业务范围。\n"
             f"notice_id={key}\ntask_id={target['task_id']}\noriginal_request_id={config['launch_id']}\n"
             "服务器已续期原启动消息中的接入凭据。请从该原消息取用原 enrollment_id 与 grant 调用 agent_attach，"
             "不要回显或转发 grant。接入后先调用 goal_bootstrap 恢复已有 Goal；只有确实不存在自己的 Goal 才创建并关联一次。"
             "继续原任务的已授权工作，使用当前正式工具；旧 bind_chat_task_goal 只做已有归属核对，不是执行前置条件。"
             "原未知投递及错误记录保持不变，本通知和发送成功都不表示接入或业务完成。")
    identity={"original_request_id":config["launch_id"],"queue_id":operation["queue_id"],
              "task_id":target["task_id"],"manager_run_id":operation["manager"]["manager_run_id"],
              "authorization_id":permit["authorization_id"],"notice_id":key,"url":url,
              "message_sha256":hashlib.sha256(message.encode("utf-8")).hexdigest()}
    path=Path(path)
    deadline=clock()+15

    def observation():return inspector(browser,route,target,key)
    def bound(value):
        return (approval_guard.chat_url(value.get("url"))==url
                and value.get("anchor_count")==1 and value.get("visible") is True)
    def current():return not stopped() and pending(control.read_status(operation),config["launch_id"])
    result={"notice_id":key,"state":"not_sent","business_task_resent":False}
    probe=observation()
    if not bound(probe):return dict(result,reason="original_chat_not_verified")
    if path.exists():
        if read_json(path)!=identity:raise ValueError("original_notice_fence_mismatch")
        return dict(result,state="observed" if probe.get("wake_count")==1 else "unknown",
                    user_count=probe.get("user_count"),last=probe.get("wake_last"))
    if probe.get("wake_count"):
        return dict(result,state="unknown",reason="unrecorded_notice_marker")
    if probe.get("busy") or probe.get("modal") or not current():
        return dict(result,reason="chat_or_enrollment_not_ready")
    sleep(.25)
    if observation()!=probe:return dict(result,reason="chat_state_changed")
    draft=edge_handoff.composer(browser)
    if not empty_draft(draft):return dict(result,reason="composer_has_user_draft")
    editor=browser.find_by_xpath(draft["xpath"],timeout=3)
    editor.click(delay_after=0,before_actual=current)
    focused=edge_handoff.composer(browser)
    if (not current() or not all(focused.get(k) for k in ("focused","document_focused","document_visible"))
            or not empty_draft(focused) or observation()!=probe):
        return dict(result,reason="composer_or_context_changed")
    # Exclusive creation and fsync precede any text input. An incomplete fence
    # after a crash is also uncertainty, never permission to send again.
    path.parent.mkdir(parents=True,exist_ok=True)
    try:
        with path.open("x",encoding="utf-8",newline="\n") as record:
            json.dump(identity,record,sort_keys=True)
            record.flush();os.fsync(record.fileno())
    except FileExistsError:
        return dict(result,state="unknown",reason="notice_already_fenced")
    try:
        if not current():return dict(result,state="unknown",reason="cancelled_after_fence")
        editor.clipboard_input(message,append=True,delay_after=.25)
        prepared=edge_handoff.composer(browser)
        if (not current() or not exact_draft(prepared,message)
                or not prepared.get("send_enabled") or prepared.get("send_count")!=1):
            return dict(result,state="unknown",reason="send_target_not_ready")
        def before_actual():
            fresh=observation();draft=edge_handoff.composer(browser)
            return (current() and clock()<deadline and bound(fresh)
                    and fresh.get("user_count")==probe.get("user_count") and not fresh.get("busy")
                    and not fresh.get("modal") and exact_draft(draft,message)
                    and draft.get("send_enabled") and draft.get("send_xpath")==prepared["send_xpath"])
        browser.find_by_xpath(prepared["send_xpath"],timeout=3).click(delay_after=0,before_actual=before_actual)
        while clock()<deadline and not stopped():
            fresh=observation()
            if bound(fresh) and fresh.get("wake_count")==1:
                return dict(result,state="delivered",user_count=fresh.get("user_count"),last=fresh.get("wake_last"))
            sleep(min(.25,max(0,deadline-clock())))
        return dict(result,state="unknown",reason="notice_send_unconfirmed")
    except Exception as error:
        return dict(result,state="unknown",reason=type(error).__name__)
