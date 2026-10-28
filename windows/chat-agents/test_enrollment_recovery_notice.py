import copy
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

import chat_agents
import enrollment_recovery_notice as notice
from test_team_continuation import Browser, Clock, TARGET, ROUTE


class NoticeTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.path=Path(self.temp.name)/'notice.json';self.clock=Clock();self.browser=Browser()
        self.operation={'queue_id':'queue','manager':{'manager_run_id':'run'},
                        'status':{'binding':{'task_id':TARGET['task_id']}}}
        self.config={'launch_id':'original-launch','task_id':'original-local-task'}
        self.permit={'authorization_id':'a'*32,'notify_original':True}
        self.status={'request_id':self.config['launch_id'],'queue_state':'unknown','cancel_requested':False,
                     'enrollment_status':{'state':'issued','run_state':'pending','claim_expired':False,
                                          'window_enabled':False,'principal_revoked':False}}
        self.control=types.SimpleNamespace(read_status=lambda _:copy.deepcopy(self.status))
        self.probe={'url':ROUTE['url'],'anchor_count':1,'wake_count':0,'wake_last':False,
                    'user_count':2,'busy':False,'modal':False,'visible':True}
    def inspect(self,*args):
        value=dict(self.probe)
        if self.browser.sent:value.update(wake_count=1,wake_last=True,user_count=3)
        return value
    def run_notice(self,**kwargs):
        with patch.object(notice.edge_handoff,'composer',side_effect=lambda _:self.browser.composer()):
            return notice.notify(self.browser,self.control,self.operation,self.config,ROUTE['url'],
                'notice-key',self.path,self.permit,lambda:False,inspector=self.inspect,
                clock=self.clock.clock,sleep=self.clock.sleep,**kwargs)
    def test_one_send_has_durable_fence_and_restart_observes_without_resend(self):
        original=self.browser.find_by_xpath
        def find(path,**kwargs):
            element=original(path,**kwargs);write=element.clipboard_input
            def input_text(text,**options):
                self.assertTrue(self.path.is_file())
                self.assertNotIn('grant=',text)
                self.assertIn('agent_attach',text)
                write(text,**options)
            element.clipboard_input=input_text
            return element
        self.browser.find_by_xpath=find
        self.assertEqual(self.run_notice()['state'],'delivered')
        self.assertEqual(self.browser.sent,1)
        self.assertEqual(self.run_notice()['state'],'observed')
        self.assertEqual(self.browser.sent,1)
    def test_unknown_send_never_retries_even_when_message_is_missing(self):
        original=self.browser.find_by_xpath
        def find(path,**kwargs):
            element=original(path,**kwargs)
            if path=='send':element.click=lambda **options:(_ for _ in ()).throw(OSError('uncertain input'))
            return element
        self.browser.find_by_xpath=find
        self.assertEqual(self.run_notice()['state'],'unknown')
        self.assertTrue(self.path.exists())
        self.browser.find_by_xpath=original;self.browser.draft=''
        self.assertEqual(self.run_notice()['state'],'unknown')
        self.assertEqual(self.browser.sent,0)
    def test_busy_draft_changed_identity_and_expired_lease_do_not_type(self):
        for condition in ('busy','draft','url','expired','claimed'):
            with self.subTest(condition=condition):
                self.setUp()
                if condition=='busy':self.probe['busy']=True
                if condition=='draft':self.browser.draft='Human draft'
                if condition=='url':self.probe['url']='https://chatgpt.com/c/another-fixture'
                if condition=='expired':self.status['enrollment_status']['claim_expired']=True
                if condition=='claimed':self.status['enrollment_status']['state']='claimed'
                self.assertEqual(self.run_notice()['state'],'not_sent')
                self.assertFalse(self.path.exists());self.assertEqual(self.browser.sent,0)
    def test_new_user_message_after_fence_prevents_send_and_keeps_uncertainty(self):
        original=self.browser.find_by_xpath
        def find(path,**kwargs):
            element=original(path,**kwargs);write=element.clipboard_input
            if path=='editor':
                def input_text(text,**options):
                    write(text,**options);self.probe['user_count']=3
                element.clipboard_input=input_text
            return element
        self.browser.find_by_xpath=find
        self.assertEqual(self.run_notice()['state'],'unknown')
        self.assertEqual(self.browser.sent,0);self.assertTrue(self.path.exists())
    def test_prior_approval_permission_alone_does_not_authorize_notice(self):
        self.permit.pop('notify_original')
        with self.assertRaisesRegex(ValueError,'authorization_required'):self.run_notice()
        self.assertFalse(self.path.exists());self.assertEqual(self.browser.sent,0)


if __name__=='__main__':unittest.main()
