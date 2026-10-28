import contextlib
import time
import types
import unittest
import tempfile
from pathlib import Path
from unittest.mock import Mock,patch
import chat_agents
import enrollment_recovery as recovery

CONFIG={"launch_id":"11111111-1111-4111-8111-111111111111","task_id":"original-local-task"}
URL="https://chatgpt.com/c/12345678-fixture"
STATUS={"request_id":CONFIG["launch_id"],"queue_state":"unknown","cancel_requested":False,
        "enrollment_status":{"state":"issued","run_state":"pending","claim_expired":True,
                             "window_enabled":False,"principal_revoked":False}}

class Tests(unittest.TestCase):
    def exercise(self, *, last=True, count=1, mode="running", permit=None, notice_state="delivered", notice_path=None):
        self.control=types.SimpleNamespace(read_status=Mock(return_value=STATUS),
            renew_original_delivery=Mock(return_value={"expires_at_unix_ms":int(time.time()*1000)+300000}))
        session=types.SimpleNamespace(_transport=Mock());self.session=session
        browser=types.SimpleNamespace(_rpc=Mock(),activate=Mock())
        self.guard=Mock(return_value={"status":"sent","reason":"no_approval","approval_click_count":0})
        self.notifier=Mock(return_value={"state":notice_state,"last":True,"user_count":2})
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        self.notice_path=notice_path or Path(temporary.name)/"notice.json"
        with patch.object(recovery.browser_api,"Browser",return_value=browser):
            return recovery.recover(self.control,{"status":{"binding":{"task_id":"actual-task"}}},CONFIG,URL,"key",lambda:{"mode":mode},
                runtime_root=Path("fixture"),session_factory=lambda:contextlib.nullcontext(session),
                lock_factory=lambda _:contextlib.nullcontext(),
                inspector=lambda *a:{"url":URL,"count":count,"last":last,"user_count":1,"visible":True},
                guard=self.guard,clock=lambda:0,sleep=lambda _:None,
                permit=permit,
                notice_path=self.notice_path,notifier=self.notifier,
                cancelled=(lambda:count!=1))
    def test_renews_only_original_message_and_never_sends_a_prompt(self):
        result=self.exercise()
        self.assertEqual(self.control.renew_original_delivery.call_args.args[1:],(CONFIG["launch_id"],"key"))
        self.session._transport.call.assert_called_once_with("browser_tabs",{"action":"new","url":URL})
        self.guard.assert_called_once()
        self.assertFalse(result["business_task_resent"])
        self.assertEqual(result["state"],"unresolved")
    def test_later_user_message_pause_or_unverified_original_prevents_renewal(self):
        for args in ({"last":False},{"mode":"paused"},{"count":0}):
            with self.subTest(args=args):
                self.exercise(**args);self.control.renew_original_delivery.assert_not_called();self.guard.assert_not_called()
    def test_claimed_or_revoked_enrollment_is_ineligible(self):
        for change in ({"state":"claimed"},{"run_state":"retired"},{"principal_revoked":True},{"claim_expired":False}):
            changed={**STATUS,"enrollment_status":{**STATUS["enrollment_status"],**change}}
            self.assertFalse(recovery.eligible(changed,CONFIG["launch_id"]))

    def test_explicit_operator_permit_allows_prior_launch_but_preserves_stop(self):
        permit={"authorization_id":"a"*32}
        result=self.exercise(last=False,permit=permit)
        self.control.renew_original_delivery.assert_called_once()
        self.assertTrue(self.guard.call_args.kwargs['approved_prior_launch'])
        self.assertEqual(result['operator_authorization_id'],permit['authorization_id'])
        self.assertFalse(result['business_task_resent'])
        self.exercise(last=False,permit=permit,mode="paused")
        self.control.renew_original_delivery.assert_not_called()

    def test_operator_permit_is_bound_to_original_request_queue_project_and_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            operation={"queue_id":"queue","status":{"binding":{"project":"project"}},"manager":{"manager_run_id":"run"}}
            self.assertIsNone(recovery.operator_permit(root,operation,CONFIG['launch_id']))
            permit={"original_request_id":CONFIG['launch_id'],"queue_id":"queue","project":"project","manager_run_id":"run", "authorization_id":"a"*32,"source":"Explicit human instruction in fixture","allow_prior_launch":True}
            path=root/'enrollment-recovery-permits'/(CONFIG['launch_id']+'.json')
            recovery.write_json(path,permit)
            self.assertEqual(recovery.operator_permit(root,operation,CONFIG['launch_id']),permit)
            for key in ('original_request_id','queue_id','project','manager_run_id'):
                recovery.write_json(path,{**permit,key:'different'})
                with self.assertRaisesRegex(ValueError,'permit_mismatch'):
                    recovery.operator_permit(root,operation,CONFIG['launch_id'])

    def test_notice_binds_approval_to_new_last_framework_message(self):
        permit={"authorization_id":"a"*32,"notify_original":True}
        result=self.exercise(last=False,permit=permit)
        self.notifier.assert_called_once()
        self.assertEqual(result['notice']['state'],'delivered')
        self.assertEqual(self.guard.call_args.args[1],{'launch_id':'key','goal_id':'actual-task','expected_user_count':2})
        self.assertFalse(self.guard.call_args.kwargs['approved_prior_launch'])
        path=self.notice_path
        result=self.exercise(last=False,permit=permit,notice_path=path)
        self.assertEqual(result['reason'],'original_notice_approval_already_fenced')
        self.guard.assert_not_called()

        result=self.exercise(last=False,permit=permit,notice_state='unknown')
        self.assertEqual(result['reason'],'recovery_notice_unknown')
        self.guard.assert_not_called()

    def test_unknown_notice_gets_one_reconciliation_without_new_scheduler_generation(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);threads=[]
            def factory(**kwargs):
                threads.append(kwargs['args'])
                return types.SimpleNamespace(start=lambda:None,is_alive=lambda:False)
            service=chat_agents.Service(root,thread_factory=factory,transport_runtime_root=root/'scheduler')
            binding={'queue_kind':'member','project':'project','queue_id':'queue'}
            status={**STATUS,'binding':binding,'enrollment_status':{**STATUS['enrollment_status'],'claim_expired':False}}
            operation={'queue_id':'queue','status':status,'manager':{'manager_run_id':'run'}}
            control=types.SimpleNamespace(legacy_api=types.SimpleNamespace(project='project'),prepare=lambda _:operation)
            recovery.write_json(root/'tasks'/CONFIG['launch_id']/'request.json',{'config':CONFIG,'enrollment_binding':binding})
            recovery.write_json(root/'evidence'/CONFIG['launch_id']/'result.json',{**CONFIG,'target_url':URL,'message_observed':True})
            permit={'original_request_id':CONFIG['launch_id'],'queue_id':'queue','project':'project','manager_run_id':'run',
                    'authorization_id':'a'*32,'source':'Explicit human recovery fixture','allow_prior_launch':True,'notify_original':True}
            recovery.write_json(root/'enrollment-recovery-permits'/(CONFIG['launch_id']+'.json'),permit)
            scope='\0'.join((CONFIG['launch_id'],'run','7','original-attach-v1','operator:'+permit['authorization_id']))
            key=recovery.hashlib.sha256(scope.encode()).hexdigest()
            recovery.write_json(root/'enrollment-recovery'/(key+'.json'),{'notice':{'state':'unknown'}})
            recovery.write_json(root/'enrollment-recovery-notices'/(key+'.json'),{'fenced':True})
            state=lambda:{'mode':'running','generation':7}
            self.assertTrue(recovery.schedule(service,control,state))
            self.assertTrue(threads[0][-1])
            self.assertEqual(threads[0][6].name,key+'.reconcile.json')
            recovery.write_json(threads[0][6],{'state':'unresolved'})
            self.assertFalse(recovery.schedule(service,control,state))
            self.assertEqual(len(threads),1)

if __name__=="__main__":unittest.main()
