//! Isolated Agent entry for ordinary project programs. Identity, Runner/root,
//! resource fingerprints and Job ids are server-derived; there is no CLI slot.
use crate::tool_runtime::{ToolResult, ToolRuntime};
use crate::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use webcodex_core::agent_program::{validate_relative_cwd, AgentProgramBinding, AgentProgramStart};
use webcodex_core::runner_protocol::{AgentExecutionTarget, AgentRunRef};
use webcodex_runner_registry::{AgentProgramStoreAdmission, RunnerAccess, RunnerFeature};
use webcodex_store::{AgentProjectFilesGrant, VerifiedAgentRun};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Catalog,
    Browser {
        program_id: String,
        steps: Vec<webcodex_core::agent_browser::BrowserStep>,
        timeout_secs: Option<u64>,
        idempotency_key: String,
    },
    BrowserObserve {
        action_id: String,
    },
    BrowserImage {
        action_id: String,
        step: Option<u16>,
    },
    Start {
        resource: Option<String>,
        #[serde(default = "cwd")]
        cwd: String,
        #[serde(default)]
        args: Vec<String>,
        stdin: Option<String>,
        timeout_secs: Option<u64>,
        local_service_port: Option<u16>,
        resume_from_program: Option<String>,
        idempotency_key: String,
    },
    Observe {
        program_id: String,
        tail_lines: Option<usize>,
    },
    Stop {
        program_id: String,
    },
}
fn cwd() -> String {
    ".".into()
}
struct Failure {
    code: String,
    uncertain: bool,
    program_id: Option<String>,
    action_id: Option<String>,
}
impl From<String> for Failure {
    fn from(code: String) -> Self {
        Self {
            code,
            uncertain: false,
            program_id: None,
            action_id: None,
        }
    }
}
impl From<&str> for Failure {
    fn from(code: &str) -> Self {
        code.to_owned().into()
    }
}
fn uncertain(code: String, program: Option<&str>) -> Failure {
    Failure {
        code,
        uncertain: true,
        program_id: program.map(str::to_owned),
        action_id: None,
    }
}
struct Admission<'a> {
    db: &'a Database,
    run: &'a VerifiedAgentRun,
}
struct Observation(webcodex_runner_registry::VerifiedAgentProgramObservation);
impl webcodex_store::AuthenticatedAgentProgramObservation for Observation {
    fn binding(&self) -> &AgentProgramBinding {
        self.0.binding()
    }
    fn job(&self) -> &webcodex_core::runner_protocol::ShellJobInfo {
        self.0.job()
    }
}
impl AgentProgramStoreAdmission for Admission<'_> {
    fn claim_dispatch(&self, start: &AgentProgramStart) -> Result<bool, String> {
        self.db.claim_agent_program_dispatch(self.run, start)
    }
}
struct BrowserObservation(webcodex_runner_registry::VerifiedAgentBrowserReceipt);
impl webcodex_store::AuthenticatedAgentBrowserReceipt for BrowserObservation {
    fn action(&self) -> &webcodex_core::agent_browser::AgentBrowserAction {
        self.0.action()
    }
    fn receipt(&self) -> &webcodex_core::agent_browser::AgentBrowserReceipt {
        self.0.receipt()
    }
}
impl webcodex_runner_registry::AgentBrowserStoreAdmission for Admission<'_> {
    fn claim_browser(
        &self,
        action: &webcodex_core::agent_browser::AgentBrowserAction,
    ) -> Result<bool, String> {
        self.db.claim_agent_browser(self.run, action)
    }
}
fn browser_projection(record: webcodex_store::AgentBrowserRecord) -> Value {
    json!({"action_id":record.action.action_id,"program_id":record.action.service.program_id,
        "state":record.receipt.as_ref().map_or(record.state.as_str(),|r|r.state.as_str()),"receipt":record.receipt,"business_verified":false})
}
pub(crate) fn spec() -> Value {
    json!({"name":"agent_process","description":"Run registered programs in the authorized shared project directory. catalog lists currently loaded resources. General Shell and process tools remain available independently. start accepts resource (omit only for a unique available resource), project-relative cwd (default project root), args, optional stdin and timeout_secs (default 60, max 86400), and a stable idempotency_key. Returns a real program_id/Job; acceptance is not completion. Observe the same program for stdout/stderr and actual exit status; unknown exit is never success. stop requests this task's exact process tree and must be followed by observation. No arbitrary host executable, environment, private Agent path, identity or CLI provider is accepted. Workspace effects update the shared project. local_service_port requests TCP access only on 127.0.0.1 at that port; the process receives WEBCODEX_PORT. observe reports a URL only after kernel listener ownership is verified. resume_from_program references an exact own stopped program; shared project data already persists; live, foreign or unknown-stop sources are refused. browser uses a fixed Runner controller and a fresh sandboxed Chromium session, never caller Python/JavaScript on the host. It runs fixed steps against your own currently listening service program_id, with stable idempotency_key and optional timeout_secs (default/max 30). Steps navigate by relative path, click/fill/select/text by CSS selector, expect_text to wait for a nonblank text substring within the action deadline, or screenshot; click may set accept_dialog=true for its own JavaScript confirmation; default dismiss, with dialog messages recorded. No URL, script, executable or profile is accepted. browser_observe reads action_id. browser_image reads the recorded PNG for action_id and the zero-based screenshot step (omit only when the action has one screenshot); MCP returns a native image. It does not rerun the browser. A fenced or unknown action is never repeated; failed actions may have submitted earlier steps. Inspect original evidence before further actions.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["catalog","start","observe","stop","browser","browser_observe","browser_image"]},"action_id":{"type":"string"},"step":{"type":"integer","minimum":0,"maximum":39},"steps":{"type":"array","minItems":1,"maxItems":40,"items":{"type":"object","properties":{"action":{"enum":["navigate","click","fill","select","text","expect_text","screenshot"]},"path":{"type":"string"},"selector":{"type":"string","maxLength":1000},"text":{"type":"string","maxLength":16384},"value":{"type":"string","maxLength":16384},"accept_dialog":{"type":"boolean","description":"Click only: explicitly accept JavaScript dialogs caused by this click; default dismiss. Dialog evidence is recorded."}},"required":["action"],"additionalProperties":false}},"resource":{"type":"string"},"cwd":{"type":"string"},"args":{"type":"array","maxItems":256,"items":{"type":"string"}},"stdin":{"type":"string","maxLength":1048576},"timeout_secs":{"type":"integer","minimum":1,"maximum":86400},"local_service_port":{"type":"integer","minimum":1024,"maximum":65535},"resume_from_program":{"type":"string"},"idempotency_key":{"type":"string","minLength":1,"maxLength":128},"program_id":{"type":"string"},"tail_lines":{"type":"integer","minimum":1,"maximum":500}},"required":["action"],"additionalProperties":false}})
}
pub(crate) async fn call(runtime: &ToolRuntime, run: &VerifiedAgentRun, args: Value) -> ToolResult {
    match execute(runtime, run, args).await {
        Ok(value) => ToolResult::ok(value),
        Err(error) if error.uncertain => ToolResult::err_with_output(
            error.code.clone(),
            json!({"error_kind":error.code,"program_id":error.program_id,"action_id":error.action_id,"state_changed":null,"dispatch_certainty":"outcome_unknown","recovery":"Observe the original program_id or browser action_id. If its receipt was lost, repeat the identical request and original idempotency key to recover it; never use a new key to bypass uncertainty."}),
        ),
        Err(error) if error.action_id.is_some() => ToolResult::err_with_output(
            error.code.clone(),
            json!({"error_kind":error.code,"program_id":error.program_id,"action_id":error.action_id,"dispatch_certainty":"not_started","browser_effects_started":false,"recovery":"Resolve the reported availability issue, then retry the identical browser request and original key."}),
        ),
        Err(error) => crate::agent_isolation::denial(&error.code),
    }
}
async fn execute(
    runtime: &ToolRuntime,
    run: &VerifiedAgentRun,
    args: Value,
) -> Result<Value, Failure> {
    let request: Request =
        serde_json::from_value(args).map_err(|_| "agent_program_invalid_input")?;
    let db = runtime
        .isolation_database()
        .ok_or("agent_isolation_unavailable")?;
    db.validate_agent_run(run)
        .map_err(|e| e.code().to_string())?;
    let user = db
        .get_user_by_id(run.owner_user())
        .map_err(|_| "agent_program_owner_unavailable")?
        .filter(|u| !u.is_disabled())
        .ok_or("agent_program_owner_unavailable")?;
    let access = RunnerAccess {
        global_visibility: false,
        owner_bypass: false,
        username: Some(user.username),
        group: None,
    };
    if let Request::BrowserImage { action_id, step } = &request {
        let record = db.read_agent_browser(run, action_id)?;
        let step = match step {
            Some(step) => *step,
            None => {
                let mut screenshots =
                    record.action.steps.iter().enumerate().filter_map(|(i, s)| {
                        matches!(s, webcodex_core::agent_browser::BrowserStep::Screenshot {})
                            .then_some(i as u16)
                    });
                let step = screenshots.next().ok_or("agent_browser_image_missing")?;
                if screenshots.next().is_some() {
                    return Err("agent_browser_image_selection_required".into());
                }
                step
            }
        };
        let observed = runtime
            .runner_registry
            .agent_browser(&access, &record.action, None, Some(step))
            .await?;
        let image = observed
            .image()
            .cloned()
            .ok_or("agent_browser_image_missing")?;
        db.record_agent_browser(run, &BrowserObservation(observed))?;
        return Ok(
            json!({"action_id":action_id,"step":step,"path":format!("browser/{action_id}/step-{step:02}.png"),"mime_type":"image/png","content_base64":image.content_base64,"file_bytes":image.file_bytes,"sha256":image.sha256,"business_verified":false}),
        );
    }
    if matches!(
        &request,
        Request::Browser { .. } | Request::BrowserObserve { .. }
    ) {
        let (record, may_start) = match request {
            Request::Browser {
                program_id,
                steps,
                timeout_secs,
                idempotency_key,
            } => (
                db.prepare_agent_browser(
                    run,
                    &program_id,
                    steps,
                    timeout_secs.unwrap_or(30),
                    &idempotency_key,
                )?,
                true,
            ),
            Request::BrowserObserve { action_id } => {
                (db.read_agent_browser(run, &action_id)?, false)
            }
            _ => unreachable!(),
        };
        if matches!(record.state.as_str(), "terminal" | "cancelled")
            || (!may_start && record.state == "prepared")
        {
            return Ok(browser_projection(record));
        }
        if may_start && record.state == "prepared" {
            let grant = db
                .agent_project_files_grant(run, true)
                .map_err(|e| e.code().to_string())?;
            let service = &record.action.service;
            match &grant {
                AgentProjectFilesGrant::Named(workspace)
                    if workspace.folders.iter().any(|f| {
                        f.project == service.workspace_project && f.path == service.workspace_root
                    }) => {}
                AgentProjectFilesGrant::Registered { project }
                    if project == &service.workspace_project => {}
                _ => return Err("agent_browser_workspace_denied".into()),
            }
            let resolved = runtime
                .execution()
                .resolve_project_input(&service.workspace_project)
                .await
                .map_err(|_| "agent_browser_project_unavailable")?;
            if resolved.resolved_id != service.workspace_project
                || resolved.config.path != service.workspace_root
                || !resolved.config.allow_patch
            {
                return Err("agent_browser_project_changed_or_read_only".into());
            }
            if runtime
                .config
                .permission_evaluator
                .evaluate("run_process", Some(&service.workspace_project))
                .is_some_and(|decision| !decision.allows_execution())
            {
                return Err("agent_browser_policy_denied".into());
            }
            if db
                .agent_project_files_grant(run, true)
                .map_err(|e| e.code().to_string())?
                != grant
            {
                return Err("agent_browser_project_changed_or_read_only".into());
            }
        }
        let admission = Admission { db, run };
        let gate: Option<&dyn webcodex_runner_registry::AgentBrowserStoreAdmission> =
            if may_start && record.state == "prepared" {
                Some(&admission)
            } else {
                None
            };
        let observation = runtime
            .runner_registry
            .agent_browser(&access, &record.action, gate, None)
            .await
            .map_err(|code| {
                let mut error = uncertain(code, Some(&record.action.service.program_id));
                error.action_id = Some(record.action.action_id.clone());
                if db
                    .read_agent_browser(run, &record.action.action_id)
                    .is_ok_and(|r| r.state == "prepared")
                {
                    error.uncertain = false;
                }
                error
            })?;
        let observed = db
            .record_agent_browser(run, &BrowserObservation(observation))
            .map_err(|code| {
                let mut error = uncertain(code, Some(&record.action.service.program_id));
                error.action_id = Some(record.action.action_id.clone());
                error
            })?;
        return Ok(browser_projection(observed));
    }
    // An existing request is resolved before optional resource discovery. A
    // changed catalog or reconnect must not replace its original Job/command.
    if let Request::Start {
        resource,
        cwd,
        args,
        stdin,
        timeout_secs,
        idempotency_key,
        local_service_port,
        resume_from_program,
    } = &request
    {
        if let Some((original, receipt)) = db.agent_program_by_key(run, idempotency_key)? {
            if resource
                .as_ref()
                .is_some_and(|r| r != &original.binding.resource_set_id)
                || cwd != &original.binding.relative_cwd
                || args != &original.args
                || stdin != &original.stdin
                || timeout_secs.unwrap_or(60) != original.timeout_secs
                || local_service_port != &original.binding.local_service_port
                || resume_from_program != &original.binding.resume_from_program
            {
                return Err("agent_program_idempotency_conflict".into());
            }
            if matches!(receipt.state.as_str(), "terminal" | "cancelled") {
                return Ok(
                    json!({"program":receipt,"replayed":true,"business_verified":false,"observation_source":"retained_original_receipt"}),
                );
            }
            let job = runtime
                .runner_registry
                .dispatch_agent_program(&access, original, &Admission { db, run })
                .await
                .map_err(|e| uncertain(e, Some(&receipt.program_id)))?;
            return Ok(
                json!({"program":receipt,"job":job,"replayed":true,"business_verified":false}),
            );
        }
    }
    if let Request::Observe {
        program_id,
        tail_lines,
    } = request
    {
        let (start, _) = db.agent_program_start(run, &program_id)?;
        let observed = runtime
            .runner_registry
            .agent_program_observation(&access, &start.binding)
            .await?;
        let receipt = db.record_agent_program_observation(run, &Observation(observed))?;
        let (job, stdout, stderr, _, _) = runtime
            .runner_registry
            .hidden_job_log_for_auth(
                Some(&access),
                &start.binding.job_id,
                Some(tail_lines.unwrap_or(100).clamp(1, 500)),
            )
            .await?;
        let (service, service_error) = if start.binding.local_service_port.is_some() {
            match runtime.runner_registry.inspect_agent_program(&access,&start.binding).await{
                Ok(observation)=>(observation.service.map(|s|json!({"port":s.port,"process_id":s.process_id,"observed_at_unix_ms":s.observed_at_unix_ms,"url":format!("http://127.0.0.1:{}",s.port),"verification":"kernel-owned TCP listener; HTTP and business behavior require separate verification"})),None),
                Err(code)=>(None,Some(code)),
            }
        } else {
            (None, None)
        };
        return Ok(
            json!({"program":receipt,"job":job,"stdout":stdout,"stderr":stderr,"service":service,"service_observation_error":service_error,"business_verified":false}),
        );
    }
    if let Request::Stop { program_id } = request {
        let (start, _) = db.agent_program_start(run, &program_id)?;
        runtime
            .runner_registry
            .observe_agent_program(&access, &start.binding)
            .await?;
        let job = runtime
            .runner_registry
            .stop_agent_program(&access, &start.binding)
            .await
            .map_err(|e| uncertain(e, Some(&program_id)))?;
        let observed = runtime
            .runner_registry
            .agent_program_observation(&access, &start.binding)
            .await
            .map_err(|e| uncertain(e, Some(&program_id)))?;
        let receipt = db
            .record_agent_program_observation(run, &Observation(observed))
            .map_err(|e| uncertain(e, Some(&program_id)))?;
        return Ok(
            json!({"program":receipt,"job":job,"stop_requested":true,"business_verified":false}),
        );
    }
    let write = matches!(request, Request::Start { .. });
    let grant = db
        .agent_project_files_grant(run, write)
        .map_err(|e| e.code().to_string())?;
    let folder = db
        .agent_team_execution_folder(run)
        .map_err(|e| e.code().to_string())?;
    let project_id = folder
        .as_ref()
        .map_or_else(|| run.project().to_owned(), |f| f.project.clone());
    let expected = match &grant {
        AgentProjectFilesGrant::Named(workspace) => Some(
            workspace
                .folders
                .iter()
                .find(|f| f.project == project_id)
                .ok_or("agent_program_folder_not_authorized")?
                .path
                .as_str(),
        ),
        AgentProjectFilesGrant::Registered { project } if project == &project_id => None,
        _ => return Err("agent_program_folder_not_authorized".into()),
    };
    let resolved = runtime
        .execution()
        .resolve_project_input(&project_id)
        .await
        .map_err(|_| "agent_program_project_unavailable")?;
    if resolved.resolved_id != project_id
        || expected.is_some_and(|p| p != resolved.config.path)
        || webcodex_core::sensitive_paths::is_secret_path(&resolved.config.path)
    {
        return Err("agent_program_project_changed".into());
    }
    let view = runtime
        .runner_registry
        .get_runner_semantic_view_checked_for_auth(&resolved.config.client_id, Some(&access))
        .await?;
    if !view.view.connected || !view.supports(RunnerFeature::AgentProgramExecutionV1) {
        return Err("agent_program_backend_unavailable".into());
    }
    let target = AgentExecutionTarget {
        client_id: view.view.client_id,
        runner_instance_id: view.view.runner_instance_id,
    };
    let resources = runtime
        .runner_registry
        .list_agent_execution_resources(&access, &target)
        .await?
        .into_iter()
        .filter(|r| {
            r.provider.is_none() && r.reviewed_controls.is_none() && r.project_workspace_copy
        })
        .collect::<Vec<_>>();
    if matches!(request, Request::Catalog) {
        return Ok(
            json!({"resources":resources,"project":project_id,"network":"none by default; optional exact local_service_port subject to native policy admission","cli_budget_required":false}),
        );
    }
    let Request::Start {
        resource,
        cwd,
        args,
        stdin,
        timeout_secs,
        idempotency_key,
        local_service_port,
        resume_from_program,
    } = request
    else {
        unreachable!()
    };
    validate_relative_cwd(&cwd)?;
    let selected = match resource {
        Some(id) => resources
            .iter()
            .find(|r| r.resource_set_id == id)
            .ok_or("agent_program_resource_unavailable")?,
        None if resources.len() == 1 => &resources[0],
        _ => return Err("agent_program_resource_selection_required".into()),
    };
    if !resolved.config.allow_patch {
        return Err("agent_program_project_read_only".into());
    }
    if let Some(decision) = runtime
        .config
        .permission_evaluator
        .evaluate("run_process", Some(&project_id))
    {
        if !decision.allows_execution() {
            return Err("agent_program_policy_denied".into());
        }
    }
    if db
        .agent_project_files_grant(run, true)
        .map_err(|e| e.code().to_string())?
        != grant
    {
        return Err("agent_program_project_changed".into());
    }
    let assigned = db
        .read_agent_enrollment(run)
        .map_err(|e| e.code().to_string())?;
    let mut start = AgentProgramStart {
        binding: AgentProgramBinding {
            local_service_port,
            resume_from_program,
            version: 1,
            program_id: format!("wc_program_{}", uuid::Uuid::new_v4().simple()),
            enrollment_id: assigned.enrollment_id,
            task_id: assigned.task_id,
            run: AgentRunRef {
                agent_id: run.agent_id().into(),
                agent_run_id: run.run_id().into(),
                generation: run.generation(),
            },
            project: run.project().into(),
            target,
            job_id: uuid::Uuid::new_v4().to_string(),
            start_request_id: uuid::Uuid::new_v4().to_string(),
            workspace_project: project_id,
            workspace_root: resolved.config.path,
            relative_cwd: cwd,
            resource_set_id: selected.resource_set_id.clone(),
            resource_manifest_hash: selected.manifest_hash.clone(),
            launch_intent_hash: "0".repeat(64),
            expires_at_unix_ms: run.expires_at_unix_ms(),
        },
        args,
        stdin,
        timeout_secs: timeout_secs.unwrap_or(60),
    };
    start.binding.launch_intent_hash = start.compute_launch_hash()?;
    let receipt = db
        .prepare_agent_program(run, start, &idempotency_key)
        .map_err(|e| {
            if e == "agent_program_store_unavailable" {
                uncertain(e, None)
            } else {
                e.into()
            }
        })?;
    let (original, _) = db
        .agent_program_start(run, &receipt.program_id)
        .map_err(|e| uncertain(e, Some(&receipt.program_id)))?;
    let job = runtime
        .runner_registry
        .dispatch_agent_program(&access, original, &Admission { db, run })
        .await
        .map_err(|e| uncertain(e, Some(&receipt.program_id)))?;
    Ok(json!({"program":receipt,"job":job,"business_verified":false}))
}
