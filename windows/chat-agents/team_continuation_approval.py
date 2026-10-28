"""Approve a late company-app card for one exact delivered system notice.

No prompt is sent. A durable fence precedes approval handling; an uncertain
click is never repeated after a Host restart or another observation sweep.
"""
import json
import os
from pathlib import Path
import re
import time

import approval_guard
from edge_tools import read_json, write_json


def handle(browser, control, target, route, attempt, probe, stopped, runtime_root,
           *, timeout=30, clock=time.monotonic, sleep=time.sleep):
    attempt_id=attempt['attempt_id']
    if re.fullmatch(r'wc_team_wake_[0-9a-f]{32}',attempt_id) is None:
        raise ValueError('invalid_continuation_approval_attempt')
    identity={'attempt_id':attempt_id,'target':target,'url':route['url']}
    folder=Path(runtime_root)/'team-notice-approvals'
    fence=folder/(attempt_id+'.fence.json')
    result_path=folder/(attempt_id+'.result.json')
    if fence.exists():
        if read_json(fence)!=identity:raise ValueError('continuation_approval_fence_mismatch')
        return {'state':'previously_fenced','outcome':read_json(result_path) if result_path.exists() else None}
    if (stopped() or probe.get('wake_count')!=1 or probe.get('wake_last') is not True
            or approval_guard.chat_url(probe.get('url'))!=route['url']):
        return {'state':'not_eligible'}
    request={'launch_id':attempt_id,'goal_id':target['task_id'],'expected_user_count':probe['user_count']}
    try:
        snapshot=approval_guard.inspect(browser,request)
        card,reason=approval_guard.candidate(snapshot)
    except Exception:
        return {'state':'observation_unavailable'}
    if card is None or reason not in (None,'scroll_required'):
        return {'state':'pending' if reason is None else 'blocked','reason':reason}
    def current():
        v=control.call('validate_target',target=target)
        return v.get('current') is True and v.get('target')==target
    if stopped() or not current():return {'state':'stale_target'}
    folder.mkdir(parents=True,exist_ok=True)
    try:
        with fence.open('x',encoding='utf-8') as record:
            json.dump(identity,record,sort_keys=True);record.flush();os.fsync(record.fileno())
    except FileExistsError:
        return {'state':'already_fenced'}
    next_check=[0.0]
    def cancelled():
        if stopped():return True
        if clock()>=next_check[0]:
            next_check[0]=clock()+1
            try:return not current()
            except Exception:return True
        return False
    try:
        outcome=approval_guard.run(browser,request,should_stop=cancelled,timeout=timeout,clock=clock,sleep=sleep)
    except Exception as error:
        outcome={'status':'unknown','reason':type(error).__name__,'business_verified':False}
    if not result_path.exists():write_json(result_path,outcome)
    return {'state':'handled','outcome':outcome}
