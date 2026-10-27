use super::*;

fn environment() -> BTreeMap<String, String> {
    [
        "PATH",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "SystemRoot",
        "TEMP",
        "TMP",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
    ]
    .into_iter()
    .filter(|key| std::env::var_os(key).is_some())
    .map(|key| (key.into(), key.into()))
    .collect()
}

fn fixture(
    executable: String,
    args: Vec<String>,
) -> (tempfile::TempDir, Arc<CodingAgentManager>, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let registry = temp.path().join("projects");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&registry).unwrap();
    fs::write(
        registry.join("repo.toml"),
        format!("id = \"repo\"\npath = {:?}\n", root.to_string_lossy()),
    )
    .unwrap();
    let config = AcpConfig {
        max_concurrent_runs: 2,
        permission_timeout_secs: 1,
        agents: vec![AcpAgentConfig {
            id: "fixture".into(),
            name: "Configured provider".into(),
            executable,
            args,
            env_from_env: environment(),
            allowed_config_options: vec![],
        }],
    };
    let manager = CodingAgentManager::with_store(&config, temp.path().join("store")).unwrap();
    (temp, manager, registry)
}

fn request(
    manager: &CodingAgentManager,
    root: &Path,
    id: &str,
    seconds: u64,
) -> CodingAgentRequest {
    CodingAgentRequest::Start(webcodex_core::coding_agent::CodingAgentStartRequest {
        run_id: format!("wc_agent_run_{id}"), intent_fingerprint: format!("intent-{id}"),
        authority_fingerprint: "auth_fixture".into(), runtime_project_id: "agent:test:repo".into(),
        project_root: root.to_string_lossy().into_owned(), provider_id: "fixture".into(),
        provider_instance_id: manager.providers()[0].provider_instance_id.clone(),
        instruction: "Reply exactly WEBCODEX_SMOKE_OK. Do not use tools, read or write files, or run commands.".into(),
        config: BTreeMap::new(), timeout_secs: seconds,
    })
}

fn await_terminal(
    manager: &CodingAgentManager,
    run_id: &str,
    seconds: u64,
) -> CodingAgentObserveResult {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let entry = manager.runs.lock().unwrap().get(run_id).unwrap().clone();
    loop {
        let observed = entry.observe(None, 64, 1).unwrap();
        if observed.run.state.terminal() {
            return observed;
        }
        assert!(
            Instant::now() < deadline,
            "owned test Run exceeded deadline"
        );
    }
}

const MOCK: &str = r#"
import json,sys,os,time,subprocess
mode=sys.argv[1]
def send(x): print(json.dumps(x),flush=True)
def partial():
 with open('partial.txt','w',encoding='utf-8') as f: f.write('KEEP_EXISTING_WORK')
prompt_id=None
for line in sys.stdin:
 m=json.loads(line); method=m.get('method'); rid=m.get('id')
 if method=='initialize': send({'jsonrpc':'2.0','id':rid,'result':{'protocolVersion':1,'agentCapabilities':{}}})
 elif method=='session/new': send({'jsonrpc':'2.0','id':rid,'result':{'sessionId':'private-session','configOptions':[{'id':'model','name':'Model','type':'select','currentValue':'mock-model','options':[{'value':'mock-model','name':'Mock'}]}]}})
 elif method=='session/prompt':
  prompt_id=rid; partial()
  with open('prompts.log','a',encoding='utf-8') as f: f.write('prompt\n')
  send({'jsonrpc':'2.0','method':'_provider/extension_notification','params':{'informational':True}})
  for update in [{'sessionUpdate':'tool_call','toolCallId':'edit1','kind':'edit','title':'partial.txt','status':'completed'},{'sessionUpdate':'tool_call','toolCallId':'test2','kind':'execute','status':'in_progress'},{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'WEBCODEX_SMOKE_OK'}}]:
   send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'private-session','update':update}})
  if mode=='crash': sys.exit(7)
  if mode in ('timeout','cancel'): continue
  if mode=='unknown':
   send({'jsonrpc':'2.0','id':rid,'result':{'stopReason':'future_unknown'}})
   continue
  if mode in ('quota','rate'):
   data={'code':'insufficient_quota'} if mode=='quota' else {'status':429}
   send({'jsonrpc':'2.0','id':rid,'error':{'code':-32000,'message':'mock provider stopped','data':data}})
  else:
   subprocess.Popen([sys.executable,'-c',"import time,pathlib; time.sleep(3); pathlib.Path('late-write.txt').write_text('unsafe')"])
   send({'jsonrpc':'2.0','id':rid,'result':{'stopReason':'end_turn'}})
 elif method=='session/cancel': send({'jsonrpc':'2.0','id':prompt_id,'result':{'stopReason':'cancelled'}})
"#;

#[test]
#[ignore = "explicit owned process evidence; requires WEBCODEX_TEST_ACP_PYTHON"]
fn runner_real_process_takeover_acp_matrix() {
    let python =
        std::env::var("WEBCODEX_TEST_ACP_PYTHON").expect("absolute Python test executable");
    for (mode, expected) in [
        ("quota", "failed"),
        ("rate", "failed"),
        ("crash", "lost"),
        ("unknown", "lost"),
        ("timeout", "cancelled"),
        ("cancel", "cancelled"),
        ("end", "completed"),
    ] {
        let script_dir = tempfile::tempdir().unwrap();
        let script = script_dir.path().join("mock.py");
        fs::write(&script, MOCK).unwrap();
        let (temp, manager, registry) = fixture(
            python.clone(),
            vec![script.to_string_lossy().into_owned(), mode.into()],
        );
        let root = temp.path().join("repo");
        let request = request(
            &manager,
            &root,
            mode,
            if mode == "timeout" { 2 } else { 15 },
        );
        let run_id = request.run_id().to_owned();
        assert!(manager.handle(request.clone(), &registry).error.is_none());
        assert!(
            manager.handle(request, &registry).error.is_none(),
            "exact replay must not spawn another provider"
        );
        if mode == "cancel" {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join("partial.txt").exists() {
                assert!(Instant::now() < deadline);
                thread::sleep(ACP_POLL);
            }
            let competing = manager.handle(request_for_competitor(&manager, &root), &registry);
            assert_eq!(competing.error.unwrap().code, "coding_agent_workspace_busy");
            let second_root = temp.path().join("repo2");
            fs::create_dir_all(&second_root).unwrap();
            fs::write(
                registry.join("repo2.toml"),
                format!(
                    "id = \"repo2\"\npath = {:?}\n",
                    second_root.to_string_lossy()
                ),
            )
            .unwrap();
            let mut parallel = request_for_competitor(&manager, &second_root);
            if let CodingAgentRequest::Start(ref mut start) = parallel {
                start.runtime_project_id = "agent:test:repo2".into();
                start.run_id = "wc_agent_run_parallel".into();
            }
            assert!(manager.handle(parallel, &registry).error.is_none());
            let capacity = manager.handle(request_for_competitor(&manager, &root), &registry);
            assert_eq!(capacity.error.unwrap().code, "coding_agent_capacity_full");
            manager.handle(
                CodingAgentRequest::Cancel(webcodex_core::coding_agent::CodingAgentCancelRequest {
                    run_id: "wc_agent_run_parallel".into(),
                }),
                &registry,
            );
            await_terminal(&manager, "wc_agent_run_parallel", 15);
            manager.handle(
                CodingAgentRequest::Cancel(webcodex_core::coding_agent::CodingAgentCancelRequest {
                    run_id: run_id.clone(),
                }),
                &registry,
            );
        }
        let observed = await_terminal(&manager, &run_id, 25);
        assert_eq!(
            observed.run.state.as_str(),
            expected,
            "mode={mode}; {:?}",
            observed.run.terminal
        );
        if mode == "quota" || mode == "rate" {
            assert_eq!(
                observed
                    .run
                    .terminal
                    .as_ref()
                    .unwrap()
                    .error_code
                    .as_deref(),
                Some(if mode == "quota" {
                    "quota_exhausted"
                } else {
                    "rate_limited"
                })
            );
        }
        assert_eq!(
            fs::read_to_string(root.join("partial.txt")).unwrap(),
            "KEEP_EXISTING_WORK"
        );
        assert_eq!(
            fs::read_to_string(root.join("prompts.log"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(
            manager
                .drain_workers_until(Instant::now() + Duration::from_secs(5))
                .timed_out,
            0
        );
        let config = AcpConfig {
            max_concurrent_runs: 2,
            permission_timeout_secs: 1,
            agents: manager
                .providers
                .values()
                .map(|p| p.config.clone())
                .collect(),
        };
        drop(manager);
        let recovered = CodingAgentManager::with_store(&config, temp.path().join("store")).unwrap();
        let observation = recovered
            .runs
            .lock()
            .unwrap()
            .get(&run_id)
            .unwrap()
            .observe(None, 64, 0)
            .unwrap();
        assert_eq!(
            observation.run.observation_revision, observed.run.observation_revision,
            "restart must preserve the published terminal revision"
        );
        assert!(observation
            .events
            .iter()
            .any(|e| e.label.as_deref() == Some("edit1")));
        assert!(observation
            .events
            .iter()
            .any(|e| e.label.as_deref() == Some("test2")
                && e.status.as_deref() == Some("in_progress")));
        assert!(!root.join("late-write.txt").exists());
    }
}

fn request_for_competitor(manager: &CodingAgentManager, root: &Path) -> CodingAgentRequest {
    request(manager, root, "competing", 10)
}

#[test]
#[ignore = "one explicitly configured real provider, no tools or source changes; local auth/network required"]
fn runner_real_process_configured_agent_smoke() {
    let exe = std::env::var("WEBCODEX_TEST_AGENT_EXE").expect("explicit provider executable");
    let args = serde_json::from_str(
        &std::env::var("WEBCODEX_TEST_AGENT_ARGS").expect("explicit JSON argv"),
    )
    .unwrap();
    let (temp, manager, registry) = fixture(exe, args);
    let request = request(&manager, &temp.path().join("repo"), "real_smoke", 45);
    let id = request.run_id().to_owned();
    assert!(manager.handle(request, &registry).error.is_none());
    let observed = await_terminal(&manager, &id, 55);
    assert_eq!(
        manager
            .drain_workers_until(Instant::now() + Duration::from_secs(5))
            .timed_out,
        0
    );
    assert_eq!(
        observed.run.state,
        CodingAgentRunState::Completed,
        "terminal={:?}; event kinds={:?}",
        observed.run.terminal,
        observed
            .events
            .iter()
            .map(|event| (&event.kind, &event.label, &event.status))
            .collect::<Vec<_>>()
    );
    let reply = observed
        .events
        .iter()
        .filter(|event| event.kind == CodingAgentEventKind::AgentMessage)
        .filter_map(|event| event.text.as_deref())
        .collect::<String>();
    assert!(
        reply.contains("WEBCODEX_SMOKE_OK"),
        "Provider returned no expected smoke marker: {reply:?}"
    );
}
