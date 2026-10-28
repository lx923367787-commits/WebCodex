"""Framework tests only: synthetic chats, no browser/network/model execution."""
import contextlib
import copy
import hashlib
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import Mock,patch

import chat_agents
import team_continuation as coordinator
import team_continuation_transport as transport

TARGET={"task_id":"wc_agent_task_"+"a"*32,"agent_id":"wc_iagent_"+"b"*32,
        "run_id":"wc_agent_run_"+"c"*32,"generation":1,"window_key":"d"*64}
ROUTE={"url":"https://chatgpt.com/c/12345678-fixture","anchor_launch_id":"original-launch","anchor_task_id":"original-task"}
ATTEMPT={"attempt_id":"wc_team_wake_"+"e"*32,"target":TARGET,"state":"claimed"}

class Clock:
    value=0
    def clock(self):return self.value
    def sleep(self,delay):self.value+=delay

class Browser:
    def __init__(self):self.draft="";self.sent=0;self.focused=False
    def _rpc(self,*args):pass
    def activate(self):pass
    def find_by_xpath(self,path,**kwargs):
        def click(**kwargs):
            if kwargs.get("before_actual",lambda:True)():
                if path=="send":self.sent+=1;self.draft=""
                else:self.focused=True
        return types.SimpleNamespace(click=click,clipboard_input=lambda text,**kwargs:setattr(self,"draft",text))
    def composer(self):
        return {"count":1,"text":self.draft,"xpath":"editor","send_xpath":"send","send_count":1,
                "send_enabled":True,"focused":self.focused,"document_focused":True,"document_visible":True}

class TransportTests(unittest.TestCase):
    def setUp(self):
        self.browser=Browser();self.clock=Clock();self.calls=[]
        self.control=types.SimpleNamespace(call=self.call)
        self.session=types.SimpleNamespace(_transport=Mock())
        self.probe={"url":ROUTE["url"],"anchor_count":1,"wake_count":0,"wake_last":False,
                    "user_count":3,"busy":False,"visible":True,"modal":False}
    def call(self,action,**kwargs):
        self.calls.append(action)
        if action=="claim":return {"claimed":True,"attempt":copy.deepcopy(ATTEMPT)}
        return {"send_permitted":action=="dispatch"}
    def inspect(self,*args):
        value=dict(self.probe)
        if self.browser.sent:value.update(wake_count=1,wake_last=True,user_count=4)
        return value
    def run_transport(self,attempt=None,**kwargs):
        with patch.object(transport.browser_api,"Browser",return_value=self.browser),patch.object(transport.edge_handoff,"composer",side_effect=lambda _:self.browser.composer()),patch.object(transport.approval_guard,"run",return_value={"status":"sent"}):
            return transport.run(self.control,TARGET,ROUTE,attempt,lambda:{"mode":"running","generation":1},
                runtime_root=Path(tempfile.gettempdir()),session_factory=lambda:contextlib.nullcontext(self.session),
                lock_factory=lambda _:contextlib.nullcontext(),inspector=self.inspect,
                clock=self.clock.clock,sleep=self.clock.sleep,**kwargs)
    def test_original_idle_chat_crosses_one_fence_then_records_observed_delivery(self):
        result=self.run_transport();self.assertEqual(result["state"],"delivered")
        self.assertEqual(self.calls,["claim","dispatch","delivered","idle"])
        self.assertEqual(self.browser.sent,1)
        self.session._transport.call.assert_called_once_with("browser_tabs",{"action":"new","url":ROUTE["url"]})
    def test_busy_or_user_draft_never_claims_or_types(self):
        for kind in ("busy","draft","modal"):
            with self.subTest(kind=kind):
                self.setUp()
                if kind=="draft":self.browser.draft="Human unfinished input"
                else:self.probe[kind]=True
                self.assertEqual(self.run_transport()["state"],"busy")
                self.assertEqual(self.calls,[] if kind=="modal" else [kind]);self.assertEqual(self.browser.sent,0)
    def test_unknown_send_is_observed_without_another_send_even_if_message_missing(self):
        attempt={**ATTEMPT,"state":"unknown"}
        self.assertEqual(self.run_transport(attempt)["state"],"unknown")
        self.assertEqual(self.calls,[]);self.assertEqual(self.browser.sent,0)
        self.probe["wake_count"]=1
        self.assertEqual(self.run_transport(attempt)["state"],"observed")
        self.assertEqual(self.calls,["delivered","idle"]);self.assertEqual(self.browser.sent,0)
    def test_duplicate_dispatch_permission_does_not_click(self):
        self.control.call=lambda action,**kwargs:({"claimed":True,"attempt":copy.deepcopy(ATTEMPT)} if action=="claim" else {"send_permitted":False})
        self.assertEqual(self.run_transport()["state"],"deferred")
        self.assertEqual(self.browser.sent,0)
    def test_lost_dispatch_response_is_uncertain_and_never_retried(self):
        original=self.call
        def fail(action,**kwargs):
            if action=="dispatch":self.calls.append(action);raise OSError("synthetic lost response")
            return original(action,**kwargs)
        self.control.call=fail
        with self.assertRaises(OSError):self.run_transport()
        self.assertEqual(self.calls,["claim","dispatch","unknown"]);self.assertEqual(self.browser.sent,0)
    def test_unverified_original_chat_never_claims(self):
        self.probe["anchor_count"]=0
        self.assertEqual(self.run_transport()["reason"],"original_chat_not_verified")
        self.assertEqual(self.calls,[])
    def test_app_chip_is_allowed_but_other_draft_prefix_is_not(self):
        self.assertTrue(transport.empty_draft({"count":1,"text":"@webcodex","mention":True}))
        self.assertTrue(transport.exact_draft({"text":"@webcodex message","mention":True},"message"))
        self.assertFalse(transport.exact_draft({"text":"Other message","mention":True},"message"))

class CoordinatorTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)
        self.service=chat_agents.Service(self.root,transport_runtime_root=self.root/"scheduler")
    def save_route(self,target=TARGET,key="original"):
        config={"launch_id":key,"task_id":"original-task"}
        binding={"task_id":target["task_id"],"agent_id":target["agent_id"]}
        coordinator.write_json(self.root/"tasks"/key/"request.json",{"config":config,"enrollment_binding":binding})
        coordinator.write_json(self.root/"evidence"/key/"result.json",{**config,"target_url":ROUTE["url"],"message_observed":True,"status":"unknown"})
    def test_exact_initial_route_does_not_silently_follow_successor(self):
        self.save_route();self.assertIsNotNone(coordinator.original_route(self.service,TARGET))
        successor={**TARGET,"generation":2,"run_id":"wc_agent_run_"+"f"*32}
        self.assertIsNone(coordinator.original_route(self.service,successor))
    def test_ambiguous_original_routes_do_not_select_latest(self):
        self.save_route();self.save_route(key="second")
        self.assertIsNone(coordinator.original_route(self.service,TARGET))
    def test_round_robin_prevents_unknown_attempt_starving_other_tasks(self):
        other={**TARGET,"task_id":"wc_agent_task_"+"f"*32}
        selected=[]
        def thread_factory(**kwargs):
            selected.append(kwargs["args"][2]["target"]["task_id"])
            return types.SimpleNamespace(start=lambda:None,is_alive=lambda:False)
        self.service.thread_factory=thread_factory
        api=types.SimpleNamespace(owner_user="fixture",project="fixture",post=lambda _: {"candidates":[{"target":TARGET,"attempt":{**ATTEMPT,"state":"unknown"}},{"target":other,"attempt":None}]})
        with patch.object(coordinator,"original_route",return_value=ROUTE),patch.object(coordinator,"watcher_lock",side_effect=lambda _:contextlib.nullcontext()):
            for _ in range(3):self.assertTrue(coordinator.schedule(self.service,api,lambda:"host",lambda:{"mode":"running"}))
        self.assertEqual(selected,[TARGET["task_id"],other["task_id"],TARGET["task_id"]])
    def test_formal_route_registration_is_observed_current_immutable_and_scoped(self):
        calls=[]
        def validate(action, **kwargs):
            calls.append(action)
            return {"target":kwargs["target"],"current":True}
        control=types.SimpleNamespace(call=validate)
        observe=lambda route,target:{"url":route["url"],"anchor_count":1,"visible":True}
        result=coordinator.register_original_route(self.service,control,TARGET,ROUTE,observe=observe)
        self.assertFalse(result["replayed"])
        self.assertEqual(calls,["validate_target","validate_target"])
        self.assertEqual(coordinator.original_route(self.service,TARGET),ROUTE)
        self.assertTrue(coordinator.register_original_route(self.service,control,TARGET,ROUTE,observe=observe)["replayed"])
        with self.assertRaisesRegex(ValueError,"immutable"):
            coordinator.register_original_route(self.service,control,TARGET,{**ROUTE,"anchor_launch_id":"changed"},observe=observe)
        successor={**TARGET,"generation":2,"run_id":"wc_agent_run_"+"f"*32}
        self.assertIsNone(coordinator.original_route(self.service,successor))
        with self.assertRaisesRegex(ValueError,"already_bound"):
            coordinator.register_original_route(self.service,control,successor,ROUTE,observe=observe)
        next_route={**ROUTE,"url":"https://chatgpt.com/c/87654321-successor","anchor_launch_id":"handoff-launch"}
        coordinator.register_original_route(self.service,control,successor,next_route,observe=observe)
        self.assertEqual(coordinator.original_route(self.service,successor),next_route)

    def test_successor_cannot_reuse_retained_initial_enrollment_chat(self):
        self.save_route()
        successor={**TARGET,"generation":2,"run_id":"wc_agent_run_"+"f"*32}
        control=types.SimpleNamespace(call=lambda action,**kwargs:{"target":kwargs["target"],"current":True})
        with self.assertRaisesRegex(ValueError,"already_bound"):
            coordinator.register_original_route(self.service,control,successor,ROUTE,
                observe=lambda route,target:{"url":route["url"],"anchor_count":1,"visible":True})

    def test_route_rejects_null_url(self):
        with self.assertRaises(ValueError):
            coordinator.validate_route({**ROUTE,"url":None})

    def test_manager_route_uses_current_session_hash_without_launch_marker(self):
        route={"url":ROUTE["url"],"window_source":"openai-session"}
        conversation=route["url"].rsplit("/",1)[-1]
        target={**TARGET,"window_key":hashlib.sha256(b"webcodex.client-window.v1\0openai-session\0"+conversation.encode()).hexdigest()}
        control=types.SimpleNamespace(call=lambda action,**kwargs:{"target":kwargs["target"],"current":True})
        observe=lambda route,target:{"url":route["url"],"anchor_count":0,"visible":True}
        coordinator.register_original_route(self.service,control,target,route,observe=observe)
        self.assertEqual(coordinator.original_route(self.service,target),route)
        self.assertFalse(coordinator.route_matches(route,TARGET,observe(route,TARGET)))
        self.assertFalse(coordinator.route_matches(route,target,{"url":route["url"],"visible":False}))
        self.assertFalse(coordinator.route_matches(route,target,{"url":route["url"]+"-other","visible":True}))

    def test_successor_automatically_uses_only_matching_formal_receipt(self):
        route={"url":ROUTE["url"],"window_source":"openai-session"}
        conversation=route["url"].rsplit("/",1)[-1]
        target={**TARGET,"generation":2,"window_key":hashlib.sha256(b"webcodex.client-window.v1\0openai-session\0"+conversation.encode()).hexdigest()}
        path=self.service.transport_runtime_root/"results"/"formal-handoff.json"
        receipt={"agent_id":target["agent_id"],"target_url":route["url"],"message_observed":True,"status":"unknown"}
        coordinator.write_json(path,receipt)
        self.assertEqual(coordinator.original_route(self.service,target),route)
        self.assertIsNone(coordinator.original_route(self.service,{**target,"window_key":"f"*64}))
        coordinator.write_json(path,{**receipt,"agent_id":"wc_iagent_"+"f"*32})
        self.assertIsNone(coordinator.original_route(self.service,target))

    def test_route_registration_rejects_unverified_anchor_and_stale_target(self):
        control=types.SimpleNamespace(call=lambda *a,**k:{"target":TARGET,"current":True})
        with self.assertRaisesRegex(ValueError,"anchor_unverified"):
            coordinator.register_original_route(self.service,control,TARGET,ROUTE,observe=lambda *a:{"url":ROUTE["url"],"anchor_count":2,"visible":True})
        control.call=lambda *a,**k:{"target":TARGET,"current":False}
        with self.assertRaisesRegex(ValueError,"target_unverified"):
            coordinator.register_original_route(self.service,control,TARGET,ROUTE,observe=lambda *a:None)
        self.assertIsNone(coordinator.original_route(self.service,TARGET))

    def test_stopped_scheduler_does_not_discover_or_start(self):
        api=Mock();self.assertFalse(coordinator.schedule(self.service,api,lambda:"host",lambda:{"mode":"paused"}))
        api.post.assert_not_called()

if __name__=="__main__":unittest.main()
