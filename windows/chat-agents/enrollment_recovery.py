"""Observe an original member delivery and repair its expired attach lease.

No new chat, business task, Goal or secret is created or read. Explicit local
operator authorization may add one durable framework recovery notice.
"""
from pathlib import Path
import hashlib
import json
import time
import uuid
import re

import approval_guard
import playwright_extension_browser as browser_api
from edge_tools import read_json, write_json
from handoff_controller import watcher_lock
from host_enrollment import ensure_no_launch_grant

PROBE = r'''function(_element, argument) {
  const a=JSON.parse(argument);
''' + approval_guard.USER_MESSAGES_JS + r'''
  const own=users.filter(e=>(e.textContent||'').includes(a.launch_id)&&(e.textContent||'').includes(a.task_id));
  return {url:location.href, count:own.length, last:own.length===1&&own[0]===users[users.length-1],
    user_count:users.length, visible:document.visibilityState==='visible'};
}'''


def inspect(browser, config):
    value=browser.execute_javascript(PROBE,json.dumps({key:config[key] for key in ("launch_id","task_id")}))
    return json.loads(value) if isinstance(value,str) else value


def eligible(status, request_id):
    enrollment=status.get("enrollment_status",{})
    return (status.get("request_id")==request_id and status.get("queue_state") in ("unknown","running")
            and status.get("cancel_requested") is False and enrollment.get("state")=="issued"
            and enrollment.get("run_state")=="pending" and enrollment.get("claim_expired") is True
            and enrollment.get("window_enabled") is False and enrollment.get("principal_revoked") is False)


def operator_permit(runtime, operation, request_id):
    """Optional local operator authorization; never accepted from a model tool.

    A new human recovery instruction may supersede an older intervening user
    message. Bind that decision to the exact original queue and manager run.
    Its immutable ID makes one new bounded attempt without erasing the prior
    blocked result or resetting the global scheduler generation.
    """
    path=Path(runtime)/"enrollment-recovery-permits"/(request_id+".json")
    if not path.exists():return None
    value=read_json(path)
    expected={"original_request_id":request_id,"queue_id":operation["queue_id"],
              "project":operation["status"]["binding"]["project"],
              "manager_run_id":operation["manager"]["manager_run_id"]}
    if (not isinstance(value,dict) or set(value)-{"notify_original"}!=set(expected)|{"authorization_id","source","allow_prior_launch"}
            or any(value.get(k)!=v for k,v in expected.items())
            or value.get("allow_prior_launch") is not True
            or not isinstance(value.get("authorization_id"),str)
            or re.fullmatch(r"[0-9a-f]{32}",value["authorization_id"]) is None
            or not isinstance(value.get("source"),str) or not 1<=len(value["source"])<=1000
            or ("notify_original" in value and type(value["notify_original"]) is not bool)):
        raise ValueError("original_recovery_operator_permit_mismatch")
    ensure_no_launch_grant(value)
    return value


def schedule(service, control, state_reader):
    initial=state_reader()
    if service._stopping.is_set() or service.browser_worker_busy() or initial.get("mode")!="running":
        return False
    for path in sorted((service.runtime/"tasks").glob("*/request.json"))[:100]:
        prepared=read_json(path)
        binding=prepared.get("enrollment_binding",{})
        if binding.get("queue_kind")!="member" or binding.get("project")!=control.legacy_api.project:
            continue
        config=prepared.get("config",{})
        request_id=path.parent.name
        if config.get("launch_id")!=request_id:
            continue
        receipt_path=service.runtime/"evidence"/request_id/"result.json"
        if not receipt_path.exists():
            continue
        receipt=read_json(receipt_path)
        url=approval_guard.chat_url(receipt.get("target_url"))
        if (not url or receipt.get("message_observed") is not True
                or receipt.get("launch_id")!=request_id or receipt.get("task_id")!=config.get("task_id")):
            continue
        operation=control.prepare(binding["queue_id"])
        if operation["status"]["binding"]!=binding:
            continue
        permit=operator_permit(service.runtime,operation,request_id)
        scope=request_id+"\0"+operation["manager"]["manager_run_id"]+"\0"+str(initial.get("generation"))+"\0original-attach-v1"
        if permit:scope+="\0operator:"+permit["authorization_id"]
        key=hashlib.sha256(scope.encode()).hexdigest()
        outcome=service.runtime/"enrollment-recovery"/(key+".json")
        reconcile=False
        if outcome.exists():
            notice_path=service.runtime/"enrollment-recovery-notices"/(key+".json")
            prior=read_json(outcome)
            if not (permit and permit.get("notify_original") is True and notice_path.exists()
                    and prior.get("notice",{}).get("state")=="unknown"):
                continue
            reconcile=True
            outcome=outcome.with_name(key+".reconcile.json")
            if outcome.exists():continue
        from enrollment_recovery_notice import pending
        if not (eligible(operation["status"],request_id) or reconcile and pending(operation["status"],request_id)):
            continue
        with watcher_lock(service.runtime/"registry"):
            if service._stopping.is_set() or service.browser_worker_busy():
                return False
            thread=service.thread_factory(target=worker,args=(service,control,operation,config,url,key,outcome,state_reader,initial,permit,reconcile),
                name="webcodex-original-enrollment-recovery",daemon=True)
            service._thread,service._active_job_id=thread,str(uuid.uuid4())
            thread.start()
            return True
    return False


def recover(control,operation,config,url,key,state_reader,*,runtime_root,cancelled=lambda:False,
            session_factory=None,lock_factory=None,inspector=inspect,guard=None,expected_state=None,
            clock=time.monotonic,sleep=time.sleep,permit=None,notice_path=None,notifier=None,reconcile=False):
    session_factory=session_factory or browser_api.persistent_session
    lock_factory=lock_factory or watcher_lock
    guard=guard or approval_guard.run
    initial=state_reader() if expected_state is None else expected_state;deadline=clock()+45
    def stopped():
        return cancelled() or clock()>=deadline or initial!=state_reader() or initial.get("mode")!="running"
    result={"request_id":config["launch_id"],"business_task_resent":False,"business_verified":False}
    if permit:result["operator_authorization_id"]=permit["authorization_id"]
    if stopped():
        return dict(result,state="not_started",reason="scheduler_not_running")
    with lock_factory(Path(runtime_root)/"edge-dispatch"):
        with session_factory() as session:
            session._transport.call("browser_tabs",{"action":"new","url":url})
            browser=browser_api.Browser(session,str(uuid.uuid4()))
            browser._rpc("initialize");browser.activate()
            while not stopped():
                probe=inspector(browser,config)
                if approval_guard.chat_url(probe.get("url"))==url and probe.get("count")==1 and probe.get("visible"):
                    break
                sleep(min(.25,max(0,deadline-clock())))
            else:
                return dict(result,state="not_started",reason="original_message_unverified")
            if not probe.get("last") and permit is None:
                return dict(result,state="blocked",reason="later_user_message_present")
            status=control.read_status(operation)
            from enrollment_recovery_notice import pending
            replay_notice=reconcile and notice_path is not None and Path(notice_path).exists()
            if stopped() or not (eligible(status,config["launch_id"]) or replay_notice and pending(status,config["launch_id"])):
                return dict(result,state="not_started",reason="original_enrollment_no_longer_eligible")
            renewal_key=key+":notice-observe" if replay_notice and status["enrollment_status"]["claim_expired"] else key
            renewal=control.renew_original_delivery(operation,config["launch_id"],renewal_key)
            result["renewal"]=renewal
            if renewal.get("expires_at_unix_ms",0)<=int(time.time()*1000):
                return dict(result,state="blocked",reason="original_renewal_expired")
            next_check=[0.0]
            def approval_stopped():
                if stopped():return True
                if clock()>=next_check[0]:
                    next_check[0]=clock()+1
                    try:
                        latest=control.read_status(operation)
                    except Exception:
                        return True
                    if latest.get("cancel_requested") is not False or latest.get("enrollment_status",{}).get("state")!="issued":
                        return True
                return False
            approval_request={"launch_id":config["launch_id"],"goal_id":config["task_id"],
                              "expected_user_count":probe["user_count"]}
            prior_launch=permit is not None
            if permit and permit.get("notify_original") is True:
                if notice_path is None:raise ValueError("original_notice_fence_required")
                if notifier is None:
                    from enrollment_recovery_notice import notify as notifier
                notice=notifier(browser,control,operation,config,url,key,notice_path,permit,approval_stopped)
                result["notice"]=notice
                if notice.get("state") not in ("delivered","observed") or notice.get("last") is not True:
                    return dict(result,state="unresolved",reason="recovery_notice_"+str(notice.get("state")))
                approval_request={"launch_id":key,"goal_id":operation["status"]["binding"]["task_id"],
                                  "expected_user_count":notice["user_count"]}
                prior_launch=False
                # An approval with an uncertain native click must not be
                # retried by a later reconciliation or Host restart.
                import os
                approval_fence=Path(notice_path).with_suffix(".approval.json")
                try:
                    with approval_fence.open("x",encoding="utf-8") as saved:
                        json.dump({"notice_id":key,"authorization_id":permit["authorization_id"]},saved)
                        saved.flush();os.fsync(saved.fileno())
                except FileExistsError:
                    return dict(result,state="unresolved",reason="original_notice_approval_already_fenced")
            approval=guard(browser,approval_request,should_stop=approval_stopped,timeout=min(30,max(1,deadline-clock())),
                approved_prior_launch=prior_launch)
            # No response text, private Goal, Memory or token is projected.
            result["approval"]={k:approval.get(k) for k in ("status","reason","approval_click_count")}
            status=control.read_status(operation)
            enrolled=status.get("enrollment_status",{})
            return dict(result,state="claimed" if enrolled.get("state")=="claimed" else "unresolved",
                        enrollment_state=enrolled.get("state"),run_state=enrolled.get("run_state"))


def worker(service,control,operation,config,url,key,outcome,state_reader,initial,permit=None,reconcile=False):
    try:
        result=recover(control,operation,config,url,key,state_reader,runtime_root=service.transport_runtime_root,
                       cancelled=service._stopping.is_set,expected_state=initial,permit=permit,
                       notice_path=service.runtime/"enrollment-recovery-notices"/(key+".json"),reconcile=reconcile)
    except Exception as error:
        result={"request_id":config["launch_id"],"state":"unknown","reason":type(error).__name__,
                "business_task_resent":False,"business_verified":False}
    ensure_no_launch_grant(result)
    if not outcome.exists():
        write_json(outcome,result)
