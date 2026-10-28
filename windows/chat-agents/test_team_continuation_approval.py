from pathlib import Path
import tempfile
import types
import unittest

import chat_agents
import team_continuation_approval as approval
from test_team_continuation import TARGET, ROUTE, ATTEMPT
from test_approval_guard import FakeBrowser, FakeClock, observation, card


class Tests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.clock=FakeClock();self.browser=FakeBrowser(self.clock)
        self.browser.current['url']=ROUTE['url']
        self.control=types.SimpleNamespace(call=lambda action,**kw:{'current':True,'target':kw['target']})
        self.probe={'url':ROUTE['url'],'wake_count':1,'wake_last':True,'user_count':1}
    def run_guard(self,stopped=lambda:False):
        return approval.handle(self.browser,self.control,TARGET,ROUTE,ATTEMPT,self.probe,stopped,self.root,
                               timeout=2,clock=self.clock.clock,sleep=self.clock.sleep)
    def test_card_arriving_after_delivery_is_approved_once_and_survives_restart(self):
        self.browser.current=observation(cards=[],url=ROUTE['url'])
        self.assertEqual(self.run_guard()['state'],'pending')
        self.assertFalse((self.root/'team-notice-approvals').exists())
        self.browser.current=observation(url=ROUTE['url'])
        self.assertEqual(self.run_guard()['state'],'handled')
        self.assertEqual(len(self.browser.clicks),1)
        self.browser.current=observation(url=ROUTE['url'])
        self.assertEqual(self.run_guard()['state'],'previously_fenced')
        self.assertEqual(len(self.browser.clicks),1)
    def test_uncertain_click_is_never_repeated(self):
        self.browser.click_error=True
        result=self.run_guard()
        self.assertEqual(result['outcome']['status'],'unknown')
        self.assertEqual(len(self.browser.clicks),1)
        self.run_guard()
        self.assertEqual(len(self.browser.clicks),1)
    def test_foreign_app_later_message_stop_and_stale_run_never_approve(self):
        self.browser.current=observation([card(exact_app=False)],url=ROUTE['url'])
        self.assertEqual(self.run_guard()['reason'],'unrecognized_approval')
        self.browser.current=observation(url=ROUTE['url'])
        self.probe['wake_last']=False
        self.assertEqual(self.run_guard()['state'],'not_eligible')
        self.probe['wake_last']=True
        self.assertEqual(self.run_guard(lambda:True)['state'],'not_eligible')
        self.control.call=lambda *a,**k:{'current':False,'target':TARGET}
        self.assertEqual(self.run_guard()['state'],'stale_target')
        self.assertEqual(self.browser.clicks,[])
        self.assertFalse((self.root/'team-notice-approvals').exists())


if __name__=='__main__':unittest.main()
