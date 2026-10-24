//! Office projections and plan previews. Native policies come from the Server;
//! the isolated browser preview retains its own explicitly synthetic policy file.
use super::*;
use crate::models::RegisteredProject;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

const POLICY_FILE: &str = "ai-office-policies.json";
const MAX_PROJECTS: usize = 128;
const MAX_SUBAGENTS: u32 = 32;
const MAX_CLI_PER_AGENT: u32 = 32;
static PREVIEW_POLICY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    Auto,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CountRange {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderPreference {
    pub provider_id: String,
    pub enabled: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfficePolicyInput {
    pub mode: PolicyMode,
    /// Total child assignments in one complete plan, not minimum concurrency.
    pub subagents: CountRange,
    /// Total planned CLI calls for each agent, including the manager.
    pub cli_per_agent: CountRange,
    pub providers: Vec<ProviderPreference>,
}

impl Default for OfficePolicyInput {
    fn default() -> Self {
        Self {
            mode: PolicyMode::Auto,
            subagents: CountRange { min: 0, max: 4 },
            cli_per_agent: CountRange { min: 0, max: 2 },
            providers: ["workbuddy", "antigravity"]
                .into_iter()
                .map(|id| ProviderPreference {
                    provider_id: id.into(),
                    enabled: true,
                    model: None,
                    effort: None,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OfficePolicy {
    pub revision: u64,
    #[serde(flatten)]
    pub settings: OfficePolicyInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficeProject {
    pub id: String,
    pub name: String,
    pub path: String,
    pub connected: bool,
    /// Runner Jobs, which may include ordinary tools and are not an AI headcount.
    pub active_jobs: u64,
    pub policy: OfficePolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct OfficeMember {
    pub id: String,
    pub role: &'static str,
    pub label: String,
    pub state: &'static str,
    pub active_requests: u64,
    pub last_activity_at_ms: Option<i64>,
    pub task: Option<String>,
    pub parent_id: Option<String>,
    pub progress: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct OfficeSnapshot {
    pub source: &'static str,
    pub observed_at_ms: u64,
    pub projects: Vec<OfficeProject>,
    pub selected_project_id: Option<String>,
    pub members: Vec<OfficeMember>,
    pub enforcement: &'static str,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfficeCliCall {
    pub provider_id: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfficeAgentPlan {
    pub id: String,
    pub task: String,
    pub cli_calls: Vec<OfficeCliCall>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfficePlan {
    pub manager: OfficeAgentPlan,
    pub subagents: Vec<OfficeAgentPlan>,
}

#[derive(Debug, Serialize)]
pub struct OfficeViolation {
    pub code: &'static str,
    pub message: String,
    pub agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OfficePlanEvaluation {
    pub admitted: bool,
    pub policy_revision: u64,
    pub enforcement: &'static str,
    pub subagent_count: usize,
    pub cli_call_count: usize,
    pub violations: Vec<OfficeViolation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedProject {
    scope: String,
    project: OfficeProject,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyStore {
    projects: BTreeMap<String, SavedProject>,
}

fn office_error(code: &str, message: &str) -> DesktopError {
    DesktopError::new(
        code,
        message,
        "Refresh the AI office and retry with the current policy revision.",
    )
}

fn valid_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn known_provider(id: &str) -> bool {
    matches!(id, "workbuddy" | "antigravity")
}

fn validate_policy(policy: &OfficePolicyInput) -> DesktopResult<()> {
    if policy.subagents.min > policy.subagents.max
        || policy.subagents.max > MAX_SUBAGENTS
        || policy.cli_per_agent.min > policy.cli_per_agent.max
        || policy.cli_per_agent.max > MAX_CLI_PER_AGENT
    {
        return Err(office_error("ai_office_invalid_policy", "Count ranges must be ordered; at most 32 subagents and 32 CLI calls per agent are supported."));
    }
    let mut seen = BTreeSet::new();
    if policy.providers.len() > 2
        || policy.providers.iter().any(|p| {
            !known_provider(&p.provider_id)
                || !seen.insert(&p.provider_id)
                || p.model.as_deref().is_some_and(|v| !valid_text(v, 256))
                || p.effort.as_deref().is_some_and(|v| !valid_text(v, 64))
        })
    {
        return Err(office_error("ai_office_invalid_policy", "Provider preferences must be unique supported providers with bounded model and effort values."));
    }
    if policy.mode == PolicyMode::Custom
        && policy.cli_per_agent.min > 0
        && !policy.providers.iter().any(|p| p.enabled)
    {
        return Err(office_error(
            "ai_office_invalid_policy",
            "A positive CLI minimum requires an enabled provider.",
        ));
    }
    Ok(())
}

fn read_store(path: &Path) -> DesktopResult<PolicyStore> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(PolicyStore::default()),
        Err(_) => {
            return Err(office_error(
                "ai_office_store_unavailable",
                "Cannot read saved AI office policies.",
            ))
        }
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    (&mut file)
        .take(DESKTOP_STATE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            office_error(
                "ai_office_store_unavailable",
                "Cannot read saved AI office policies.",
            )
        })?;
    if bytes.len() as u64 > DESKTOP_STATE_MAX_BYTES {
        return Err(office_error(
            "ai_office_store_invalid",
            "Saved AI office policies exceed the bounded size.",
        ));
    }
    let store: PolicyStore = serde_json::from_slice(&bytes).map_err(|_| {
        office_error(
            "ai_office_store_invalid",
            "Saved AI office policies are invalid; the file was preserved.",
        )
    })?;
    if store.projects.len() > MAX_PROJECTS {
        return Err(office_error(
            "ai_office_store_invalid",
            "Too many saved AI office projects.",
        ));
    }
    for row in store.projects.values() {
        validate_policy(&row.project.policy.settings)?;
    }
    Ok(store)
}

fn scope_for(core: &DesktopCore) -> DesktopResult<String> {
    let runtime = core.config.runtime.as_ref().ok_or_else(|| {
        office_error(
            "runtime_not_ready",
            "Configure a runtime before saving project policies.",
        )
    })?;
    let client = stored_runner_client_id(&core.config).ok_or_else(|| {
        office_error(
            "runtime_not_ready",
            "The saved Runner identity is unavailable.",
        )
    })?;
    Ok(format!(
        "{}\n{}",
        runtime.server_url.trim_end_matches('/'),
        client
    ))
}

fn store_key(scope: &str, id: &str) -> String {
    format!("{scope}\n{id}")
}

fn office_project(project: &RegisteredProject, policy: OfficePolicy) -> OfficeProject {
    OfficeProject {
        id: project.id.clone(),
        name: project.name.clone(),
        path: project.path.clone(),
        connected: project.connected,
        active_jobs: project.active_jobs,
        policy,
    }
}

fn selected_offline_project(
    core: &DesktopCore,
    store: &PolicyStore,
    scope: &str,
) -> Option<OfficeProject> {
    let project = core.config.project.as_ref()?;
    let id = core
        .config
        .runtime
        .as_ref()?
        .runtime_project_id
        .clone()
        .or_else(|| project.runtime_project_id.clone())?;
    let policy = store
        .projects
        .get(&store_key(scope, &id))
        .map(|p| p.project.policy.clone())
        .unwrap_or_default();
    Some(OfficeProject {
        id,
        name: Path::new(&project.path)
            .file_name()
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Project".into()),
        path: project.path.clone(),
        connected: false,
        active_jobs: 0,
        policy,
    })
}

fn save_policy(
    path: &Path,
    scope: &str,
    project: OfficeProject,
    expected: u64,
    settings: OfficePolicyInput,
) -> DesktopResult<OfficePolicy> {
    validate_policy(&settings)?;
    let mut store = read_store(path)?;
    let key = store_key(scope, &project.id);
    let revision = store
        .projects
        .get(&key)
        .map_or(0, |p| p.project.policy.revision);
    if revision != expected {
        return Err(office_error(
            "ai_office_revision_conflict",
            "The project policy changed in another request.",
        ));
    }
    if !store.projects.contains_key(&key) && store.projects.len() >= MAX_PROJECTS {
        return Err(office_error(
            "ai_office_store_full",
            "The AI office has reached its saved-project limit.",
        ));
    }
    let policy = OfficePolicy {
        revision: revision.checked_add(1).ok_or_else(|| {
            office_error(
                "ai_office_revision_conflict",
                "The policy revision limit was reached.",
            )
        })?,
        settings,
    };
    store.projects.insert(
        key,
        SavedProject {
            scope: scope.into(),
            project: OfficeProject {
                policy: policy.clone(),
                connected: false,
                active_jobs: 0,
                ..project
            },
        },
    );
    let bytes = serde_json::to_vec_pretty(&store).map_err(|_| {
        office_error(
            "ai_office_store_invalid",
            "Could not encode AI office policies.",
        )
    })?;
    if bytes.len() as u64 > DESKTOP_STATE_MAX_BYTES {
        return Err(office_error(
            "ai_office_store_full",
            "Saved AI office policies exceed the bounded size.",
        ));
    }
    write_atomic_file(path, &bytes).map_err(|_| {
        office_error(
            "ai_office_store_unavailable",
            "Could not atomically save AI office policies.",
        )
    })?;
    Ok(policy)
}

fn policy_contract_error(message: &str) -> DesktopError {
    DesktopError::new(
        "webcodex_contract_invalid",
        message,
        "只读重新查询原项目策略以核实结果；保存可能已提交，不要自动重发或更新版本号后重存旧设置。",
    )
}

fn policy_response_error(error: DesktopError) -> DesktopError {
    if error.code == "webcodex_contract_invalid" {
        policy_contract_error("Server 团队策略回执未能核实。")
    } else {
        error
    }
}

fn server_policy_from_result(envelope: Value) -> DesktopResult<OfficePolicy> {
    // The finite human Office endpoint returns a bare TeamPolicy. The exact
    // request project is fenced by ai_office_team_call's registry lookup and
    // captured connection checks before and after the CLI request, not by a
    // caller-supplied or legacy project field in the policy payload.
    let output =
        super::team_bridge::successful_team_output(envelope).map_err(policy_response_error)?;
    if !output.as_object().is_some_and(|fields| {
        fields.keys().all(|key| {
            matches!(
                key.as_str(),
                "revision" | "mode" | "subagents" | "cli_per_agent" | "providers"
            )
        })
    }) {
        return Err(policy_contract_error("Server 团队策略格式无效。"));
    }
    let policy: OfficePolicy = serde_json::from_value(output)
        .map_err(|_| policy_contract_error("Server 团队策略格式无效。"))?;
    if policy.revision > i64::MAX as u64 {
        return Err(policy_contract_error("Server 策略版本超出范围。"));
    }
    validate_policy(&policy.settings)
        .map_err(|_| policy_contract_error("Server 团队策略内容未通过校验。"))?;
    Ok(policy)
}

pub fn evaluate_plan(
    policy: &OfficePolicy,
    plan: &OfficePlan,
) -> DesktopResult<OfficePlanEvaluation> {
    validate_policy(&policy.settings)?;
    if plan.subagents.len() > MAX_SUBAGENTS as usize
        || std::iter::once(&plan.manager)
            .chain(&plan.subagents)
            .any(|a| a.cli_calls.len() > MAX_CLI_PER_AGENT as usize)
    {
        return Err(office_error(
            "ai_office_plan_too_large",
            "The plan exceeds the demo's bounded agent or CLI-call limit.",
        ));
    }
    let mut violations = Vec::new();
    let custom = policy.settings.mode == PolicyMode::Custom;
    let mut add = |code, message: String, agent_id: Option<String>| {
        violations.push(OfficeViolation {
            code,
            message,
            agent_id,
        })
    };
    if custom
        && !(policy.settings.subagents.min..=policy.settings.subagents.max)
            .contains(&(plan.subagents.len() as u32))
    {
        add(
            "subagent_count",
            format!(
                "The plan requires {}–{} child assignments.",
                policy.settings.subagents.min, policy.settings.subagents.max
            ),
            None,
        );
    }
    let mut ids = BTreeSet::new();
    let mut cli_call_count = 0;
    for agent in std::iter::once(&plan.manager).chain(&plan.subagents) {
        if !valid_text(&agent.id, 128) || !valid_text(&agent.task, 4096) {
            add(
                "agent_identity",
                "Each agent needs a unique bounded id and a task description.".into(),
                None,
            );
            continue;
        }
        if !ids.insert(&agent.id) {
            add(
                "agent_identity",
                "Each agent id must be unique.".into(),
                Some(agent.id.clone()),
            );
        }
        cli_call_count += agent.cli_calls.len();
        if custom
            && !(policy.settings.cli_per_agent.min..=policy.settings.cli_per_agent.max)
                .contains(&(agent.cli_calls.len() as u32))
        {
            add(
                "cli_count",
                format!(
                    "This agent requires {}–{} CLI calls in the complete plan.",
                    policy.settings.cli_per_agent.min, policy.settings.cli_per_agent.max
                ),
                Some(agent.id.clone()),
            );
        }
        for call in &agent.cli_calls {
            if !known_provider(&call.provider_id)
                || !valid_text(&call.model, 256)
                || call.effort.as_deref().is_some_and(|v| !valid_text(v, 64))
            {
                add("cli_selection", "Every CLI call needs a supported provider and an explicit bounded model selection.".into(), Some(agent.id.clone()));
                continue;
            }
            if custom {
                let pref = policy
                    .settings
                    .providers
                    .iter()
                    .find(|p| p.provider_id == call.provider_id && p.enabled);
                match pref {
                    None => add(
                        "provider_disabled",
                        format!("{} is not enabled in the project policy.", call.provider_id),
                        Some(agent.id.clone()),
                    ),
                    Some(pref) => {
                        if pref.model.as_deref().is_some_and(|m| m != call.model) {
                            add(
                                "model_mismatch",
                                "The planned model differs from the project's pinned model.".into(),
                                Some(agent.id.clone()),
                            );
                        }
                        if pref
                            .effort
                            .as_ref()
                            .is_some_and(|e| call.effort.as_ref() != Some(e))
                        {
                            add(
                                "effort_mismatch",
                                "The planned effort differs from the project's pinned effort."
                                    .into(),
                                Some(agent.id.clone()),
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(OfficePlanEvaluation {
        admitted: violations.is_empty(),
        policy_revision: policy.revision,
        enforcement: "demo_plan_only",
        subagent_count: plan.subagents.len(),
        cli_call_count,
        violations,
    })
}

/// Headless preview shares the real policy validator and persistence code, while
/// using a separate file and fixed synthetic project scope. A launcher that uses
/// multiple short-lived processes must serialize them around the whole request.
pub(crate) fn preview_request(data_dir: &Path, request: Value) -> DesktopResult<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Envelope {
        op: String,
        #[serde(default)]
        args: Option<Value>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Save {
        project_id: String,
        expected_revision: u64,
        policy: OfficePolicyInput,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Evaluate {
        project_id: String,
        expected_revision: u64,
        plan: OfficePlan,
    }
    fn invalid() -> DesktopError {
        office_error(
            "ai_office_invalid_preview_request",
            "Invalid AI office preview request.",
        )
    }
    fn demo_project(id: &str) -> DesktopResult<OfficeProject> {
        let (name, path) = match id {
            "demo-webcodex" => ("WebCodex", "演示/WebCodex"),
            "demo-docs" => ("产品文档", "演示/产品文档"),
            "demo-idle" => ("新项目", "演示/新项目"),
            _ => return Err(invalid()),
        };
        Ok(OfficeProject {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            connected: false,
            active_jobs: 0,
            policy: OfficePolicy::default(),
        })
    }
    let _guard = PREVIEW_POLICY_LOCK.lock().map_err(|_| {
        office_error(
            "ai_office_store_unavailable",
            "The preview policy worker was interrupted.",
        )
    })?;
    let envelope: Envelope = serde_json::from_value(request).map_err(|_| invalid())?;
    let scope = "ai-office-demo";
    // This never opens desktop-state.json or the installed app's policy file.
    let path = data_dir.join("ai-office-preview-policies.json");
    match envelope.op.as_str() {
        "policies" => {
            if envelope
                .args
                .as_ref()
                .is_some_and(|v| !v.is_null() && v != &serde_json::json!({}))
            {
                return Err(invalid());
            }
            let store = read_store(&path)?;
            let projects = ["demo-webcodex", "demo-docs", "demo-idle"]
                .into_iter()
                .map(|id| {
                    let policy = store
                        .projects
                        .get(&store_key(scope, id))
                        .map(|row| row.project.policy.clone())
                        .unwrap_or_default();
                    serde_json::json!({ "id": id, "policy": policy })
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!(projects))
        }
        "save_policy" => {
            let args: Save = serde_json::from_value(envelope.args.ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let project = demo_project(&args.project_id)?;
            let policy = save_policy(&path, scope, project, args.expected_revision, args.policy)?;
            serde_json::to_value(policy).map_err(|_| invalid())
        }
        "evaluate_plan" => {
            let args: Evaluate = serde_json::from_value(envelope.args.ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            demo_project(&args.project_id)?;
            let store = read_store(&path)?;
            let policy = store
                .projects
                .get(&store_key(scope, &args.project_id))
                .map(|row| row.project.policy.clone())
                .unwrap_or_default();
            if policy.revision != args.expected_revision {
                return Err(office_error(
                    "ai_office_revision_conflict",
                    "The preview policy changed. Refresh before evaluating.",
                ));
            }
            serde_json::to_value(evaluate_plan(&policy, &args.plan)?).map_err(|_| invalid())
        }
        _ => Err(invalid()),
    }
}

impl AppState {
    pub async fn ai_office_snapshot(
        &self,
        requested: Option<String>,
    ) -> DesktopResult<OfficeSnapshot> {
        if requested.as_deref().is_some_and(|id| !valid_text(id, 512)) {
            return Err(office_error(
                "ai_office_invalid_project",
                "Invalid project id.",
            ));
        }
        let team_connection_before = self.team_connection().await.ok();
        let scope_before = {
            let slot = self.core.lock().await;
            slot.as_ref().and_then(|core| scope_for(core).ok())
        };
        let registry = self.list_projects().await;
        let slot = self.core.lock().await;
        let core = slot.as_ref().ok_or_else(|| {
            office_error(
                "desktop_operation_busy",
                "A desktop operation is in progress.",
            )
        })?;
        let scope = scope_for(core).ok();
        if scope != scope_before {
            return Err(office_error(
                "project_connection_changed",
                "The runtime connection changed while observing projects.",
            ));
        }
        let (store, cache_unavailable) = match read_store(&core.data_dir.join(POLICY_FILE)) {
            Ok(store) => (store, false),
            Err(_) => (PolicyStore::default(), true),
        };
        let mut limitations = vec![
            "Server 团队策略只约束通过托管团队接口登记和派发的任务；独立 GPT、Host Agent 或其它命令不会自动纳入。".into(),
            "Job counts include ordinary tools. Runtime windows do not establish manager/subagent relationships, task ownership or percentage progress. Idle means no observed in-flight tool request; a Host model may still be thinking.".into(),
            "Plan admission does not launch a CLI or prove model/account availability; live options must be discovered again before execution.".into(),
        ];
        if cache_unavailable {
            limitations.push("本地旧策略缓存不可读，原文件已保留；以 Server 当前策略为准。".into());
        }
        let source = if registry.is_ok() {
            "runtime"
        } else {
            "offline"
        };
        let mut projects: Vec<OfficeProject> = match registry {
            Ok(registry) => registry
                .projects
                .iter()
                .map(|p| {
                    let policy = scope
                        .as_deref()
                        .and_then(|s| store.projects.get(&store_key(s, &p.id)))
                        .map(|p| p.project.policy.clone())
                        .unwrap_or_default();
                    office_project(p, policy)
                })
                .collect(),
            Err(_) => {
                limitations.push("Runtime activity is unavailable. Saved project rows are shown without live activity.".into());
                let mut rows: Vec<_> = store
                    .projects
                    .values()
                    .filter(|p| Some(&p.scope) == scope.as_ref())
                    .map(|p| OfficeProject {
                        connected: false,
                        active_jobs: 0,
                        ..p.project.clone()
                    })
                    .collect();
                if let Some(selected) = scope
                    .as_deref()
                    .and_then(|scope| selected_offline_project(core, &store, scope))
                {
                    if !rows.iter().any(|p| p.id == selected.id) {
                        rows.push(selected);
                    }
                }
                rows
            }
        };
        let selected = requested
            .or_else(|| {
                core.config
                    .runtime
                    .as_ref()
                    .and_then(|r| r.runtime_project_id.clone())
            })
            .filter(|id| projects.iter().any(|p| &p.id == id));
        let probe = selected.as_ref().and_then(|id| {
            Some((
                core.adapter.binaries().ok()?.webcodex.clone(),
                project_management::connection_from_config(&core.config)?,
                id.clone(),
            ))
        });
        drop(slot);
        let mut enforcement = "demo_plan_only";
        if source == "runtime" {
            if let Some(project) = selected.as_deref() {
                match self.server_team_policy(project).await {
                    Ok(policy) => {
                        if let Some(row) = projects.iter_mut().find(|row| row.id == project) { row.policy = policy; }
                        enforcement = "managed_tasks";
                    }
                    Err(error) => limitations.push(format!("无法确认当前 Server 团队策略：{} 显示的旧缓存或默认值未获 Server 确认，不能视为已应用。", error.message)),
                }
            }
        } else {
            limitations
                .push("当前离线，策略仅作缓存预览；读取或保存有效团队规则需要连接 Server。".into());
        }
        let mut members = Vec::new();
        if source == "runtime" {
            if let Some((binary, connection, id)) = probe {
                let cancellation = CancellationContext::new(
                    CancellationSignal::new(),
                    self.shutdown_signal.clone(),
                );
                match WebCodexAdapter::ai_office_windows(&binary, &connection, &id, &cancellation)
                    .await
                {
                    Ok(windows) => {
                        members = windows;
                    }
                    Err(_) => limitations.push(
                        "Window activity could not be observed for the selected project.".into(),
                    ),
                }
            }
        }
        let current_scope = {
            let slot = self.core.lock().await;
            slot.as_ref().and_then(|core| scope_for(core).ok())
        };
        if current_scope != scope {
            return Err(office_error(
                "project_connection_changed",
                "The runtime connection changed while observing activity.",
            ));
        }
        if let Some(connection) = team_connection_before.as_ref() {
            self.verify_team_connection(connection).await?;
        }
        Ok(OfficeSnapshot {
            source,
            observed_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            projects,
            selected_project_id: selected,
            members,
            enforcement,
            limitations,
        })
    }

    pub async fn save_ai_office_policy(
        &self,
        id: String,
        expected: u64,
        settings: OfficePolicyInput,
    ) -> DesktopResult<OfficePolicy> {
        validate_policy(&settings)?;
        if expected > i64::MAX as u64 {
            return Err(office_error(
                "ai_office_revision_conflict",
                "策略版本超出范围，请重新载入。",
            ));
        }
        let envelope = self
            .ai_office_team_call(
                "policy-save",
                serde_json::json!({
                    "project": id, "expected_revision": expected, "policy": settings,
                }),
            )
            .await
            .map_err(policy_response_error)?;
        server_policy_from_result(envelope)
    }

    async fn server_team_policy(&self, project: &str) -> DesktopResult<OfficePolicy> {
        let envelope = self
            .ai_office_team_call("policy-get", serde_json::json!({"project": project}))
            .await?;
        server_policy_from_result(envelope)
    }

    pub async fn evaluate_ai_office_plan(
        &self,
        id: String,
        expected: u64,
        plan: OfficePlan,
    ) -> DesktopResult<OfficePlanEvaluation> {
        let policy = self.server_team_policy(&id).await?;
        if policy.revision != expected {
            return Err(office_error(
                "ai_office_revision_conflict",
                "The project policy changed. Refresh before evaluating.",
            ));
        }
        evaluate_plan(&policy, &plan)
    }
}

#[cfg(test)]
#[path = "ai_office_tests.rs"]
mod tests;
