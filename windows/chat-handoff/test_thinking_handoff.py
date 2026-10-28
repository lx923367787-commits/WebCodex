"""Thinking policy integration checks; no Server or browser calls."""
import copy
import datetime as dt
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import uuid
import json

import auto_handoff as scheduler
import handoff_flow as flow
import edge_adapter as adapter


class ThinkingHandoffTests(unittest.TestCase):
    def setUp(self):
        self.entry = dict(project="project-test", goal_id="goal-test", source_context_epoch="epoch-A",
            checkpoint_sequence=1, checkpoint_revision=2, launch_id=str(uuid.uuid4()), deadline_at=0)
        child = str(uuid.uuid4())
        self.child = dict(request_id=child, task_id="wc_chat_"+uuid.UUID(child).hex,
            thinking_profile={"profile_id":"thinking_high","label":"High"})

    def request(self, child=None):
        return scheduler.make_request(self.entry, int(flow.utc_now().timestamp()), policy_resolver=lambda *_:child)

    def test_main_context_handoff_requests_pro_highest(self):
        request=self.request()
        self.assertEqual("main_pro_highest",request["thinking_policy"])
        self.assertNotIn("thinking_profile",request)
        self.assertIs(request,flow.validate_request(request))

    def test_child_epochs_inherit_original_profile(self):
        first=self.request(self.child)
        self.entry.update(source_context_epoch="epoch-B",checkpoint_sequence=2,launch_id=str(uuid.uuid4()))
        second=self.request(self.child)
        self.assertNotEqual(first["launch_id"],second["launch_id"])
        self.assertEqual(first["thinking_profile"],second["thinking_profile"])
        self.assertEqual("inherit_child",second["thinking_policy"])
        self.assertEqual(self.child["request_id"],second["chat_task_request_id"])
        self.assertIs(second,flow.validate_request(second))

    def test_child_without_override_never_reads_or_selects_page_strength(self):
        import edge_handoff
        child = dict(self.child, thinking_profile=None)
        request = self.request(child)
        self.assertIs(request, flow.validate_request(request))
        with patch("thinking_profiles.select_profile", side_effect=AssertionError("no setting lookup")):
            result = edge_handoff.configure_chat(None, request, None)
        self.assertEqual(result["thinking_adjustment"]["status"], "not_requested")

    def test_binding_error_does_not_fall_back_to_main(self):
        def fail(*_):raise ValueError("cancelled")
        with self.assertRaises(ValueError):
            scheduler.make_request(self.entry,0,policy_resolver=fail)

    def test_main_cannot_override_highest_and_child_needs_identity(self):
        request=self.request(); request["thinking_profile"]=self.child["thinking_profile"]
        with self.assertRaises(ValueError):flow.validate_request(request)
        for field in ("thinking_profile","chat_task_request_id","chat_task_id","thinking_policy"):
            request=self.request(self.child); request.pop(field)
            with self.subTest(field=field),self.assertRaises((ValueError,TypeError)):
                flow.validate_request(request)

    def test_policy_is_rechecked_for_cancel_and_new_binding(self):
        child=self.request(self.child)
        self.assertTrue(scheduler.thinking_selection_is_current(child,lambda *_:self.child))
        self.assertFalse(scheduler.thinking_selection_is_current(child,lambda *_:None))
        self.assertFalse(scheduler.thinking_selection_is_current(self.request(),lambda *_:self.child))
        changed=copy.deepcopy(self.child); changed["thinking_profile"]["label"]="Medium"
        self.assertFalse(scheduler.thinking_selection_is_current(child,lambda *_:changed))
        def fail(*_):raise ValueError("cancelled")
        self.assertFalse(scheduler.thinking_selection_is_current(child,fail))

    def test_receipt_cannot_be_reused_for_a_changed_thinking_intent(self):
        with tempfile.TemporaryDirectory() as temp,patch.dict(os.environ,WEBCODEX_HANDOFF_RUNTIME=temp):
            request=self.request(self.child)
            saved=flow.save_result(request,{"status":"sent"})
            self.assertEqual(saved,adapter._result(request))
            changed=copy.deepcopy(request); changed["thinking_profile"]["label"]="Medium"
            self.assertIsNone(adapter._result(changed))
            self.assertNotEqual(flow.thinking_fingerprint(request),flow.thinking_fingerprint(changed))

    def test_legacy_receipts_remain_queryable_without_new_policy(self):
        with tempfile.TemporaryDirectory() as temp,patch.dict(os.environ,WEBCODEX_HANDOFF_RUNTIME=temp):
            request=self.request(); request.pop("thinking_policy")
            saved=flow.save_result(request,{"status":"sent"})
            self.assertNotIn("thinking_fingerprint",saved)
            self.assertEqual(saved,adapter._result(request))

    def test_lost_binding_before_send_is_terminal_not_requeued_as_main(self):
        from test_handoff_reliability import FakeApi
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp); store=scheduler.Store(root/'scheduler.sqlite3')
            store.initialize('http://127.0.0.1:19876'); api=FakeApi(); calls=[]
            counter=[0]
            def resolve(*_):
                counter[0]+=1
                return self.child if counter[0]==1 else None
            try:
                with patch('chat_task_policy.resolve_goal_profile',side_effect=resolve):
                    for now in (220,225,230):
                        scheduler.poll_once(store,api,lambda *a:calls.append(a),clock=lambda:now,request_dir=root/'requests')
                self.assertEqual([],calls)
                self.assertEqual('failed',store.entries()[0]['status'])
                saved=json.loads(next((root/'requests').glob('*.json')).read_text(encoding='utf-8'))
                self.assertEqual('inherit_child',saved['thinking_policy'])
            finally:store.close()

    def test_busy_then_lost_binding_preserves_original_child_request(self):
        from test_handoff_reliability import FakeApi
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp); store=scheduler.Store(root/'scheduler.sqlite3')
            store.initialize('http://127.0.0.1:19876'); api=FakeApi(); calls=[]; binding=[self.child]
            def busy(path,stop):
                self.assertFalse(stop())
                calls.append(path)
                return {'status':'retryable','launched':False}
            try:
                with patch('chat_task_policy.resolve_goal_profile',side_effect=lambda *_:binding[0]):
                    scheduler.poll_once(store,api,busy,clock=lambda:220,request_dir=root/'requests')
                    self.assertEqual('waiting_ready',store.entries()[0]['status'])
                    original=calls[0].read_bytes(); binding[0]=None
                    scheduler.poll_once(store,api,busy,clock=lambda:225,request_dir=root/'requests')
                    self.assertEqual('failed',store.entries()[0]['status'])
                    self.assertEqual(original,calls[0].read_bytes())
                    self.assertEqual(1,len(calls))
            finally:store.close()

    def test_goal_policy_remains_child_across_scheduler_restart_and_epoch(self):
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/'scheduler.sqlite3'; store=scheduler.Store(path)
            store.freeze_thinking_policy(self.request(self.child)); store.close()
            store=scheduler.Store(path)
            try:
                self.entry.update(source_context_epoch='epoch-next',launch_id=str(uuid.uuid4()))
                store.freeze_thinking_policy(self.request(self.child))
                with self.assertRaises(scheduler.ThinkingPolicyChanged):
                    store.freeze_thinking_policy(self.request())
            finally:store.close()


if __name__=="__main__":unittest.main()
