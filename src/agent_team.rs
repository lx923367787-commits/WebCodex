//! Model-only team surface. No user principal or legacy dispatcher is exposed.
use crate::tool_runtime::ToolResult;
use crate::Database;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webcodex_store::{
    AgentAssignmentOperation, AgentEnrollmentInput, AgentEnrollmentPolicy, AgentMemberRequest,
    AgentTaskState, CommunicationStoreError, TeamPlanInput, VerifiedAgentRun,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    task_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    task_id: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanRead {
    root_task_id: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    root_task_id: String,
    expected_policy_revision: i64,
    plan: TeamPlanInput,
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberCreate {
    root_task_id: String,
    title: String,
    brief: String,
    inputs: Vec<AgentEnrollmentInput>,
    room_ids: Vec<String>,
    max_cli_calls: u32,
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskStart {
    task_id: String,
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskComplete {
    task_id: String,
    execution_id: String,
    expected_revision: i64,
    outcome: String,
    terminal_result: Option<String>,
    terminal_reason: Option<String>,
    completion_key: String,
}
fn task_completion_outcome(outcome: &str) -> Result<AgentTaskState, &'static str> {
    match outcome {
        "succeeded" => Ok(AgentTaskState::Succeeded),
        "failed" => Ok(AgentTaskState::Failed),
        "ready" | "active" => Err("invalid_public_task_completion"),
        _ => Err("agent_team_invalid_input"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentStart {
    pub root_task_id: String,
    pub source_task_id: String,
    pub enrollment_id: String,
    pub thinking_profile_id: String,
    pub idempotency_key: String,
}
impl AgentStart {
    /// Shape only. Catalog selection and B2's transactional assignment/plan
    /// checks belong to the asynchronous execution adapter.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !bounded(&self.root_task_id)
            || !bounded(&self.source_task_id)
            || !bounded(&self.enrollment_id)
            || !bounded(&self.thinking_profile_id)
            || !bounded(&self.idempotency_key)
            || self.root_task_id == self.source_task_id
        {
            Err("agent_team_invalid_input")
        } else {
            Ok(())
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CliStart {
    pub root_task_id: String,
    pub source_task_id: String,
    pub call_index: u32,
    pub fallback_thinking_profile_id: String,
    pub idempotency_key: String,
}
fn bounded(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn limit() -> usize {
    20
}
fn member_task_directory(binding: &VerifiedAgentRun, key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"webcodex.agent.member.directory.v1\0");
    digest.update(binding.agent_id().as_bytes());
    digest.update(b"\0");
    digest.update(key.as_bytes());
    format!("workspace/member-{:x}", digest.finalize())
}
fn denied(code: &str) -> ToolResult {
    if matches!(
        code,
        "public_task_worker_delivery_pending" | "public_task_business_review_required"
    ) {
        return ToolResult::err_with_output(
            code,
            json!({
                "error_kind":code,"state_changed":false,"dispatch_certainty":"not_started",
                "next_action":"The authorized caller or root manager must read the original CLI delivery and review its exact artifact version before completing the task. Keep the original attempt and key; do not restart the CLI to bypass review."
            }),
        );
    }
    ToolResult::err_with_output(
        code,
        json!({"error_kind":code,"state_changed":false,"dispatch_certainty":"not_started"}),
    )
}
fn output<T: serde::Serialize>(result: Result<T, CommunicationStoreError>) -> ToolResult {
    match result {
        Err(error) => denied(error.code()),
        Ok(value) => match serde_json::to_value(value) {
            Ok(value) => ToolResult::ok(value),
            Err(_) => ToolResult::err_with_output(
                "agent_team_result_unavailable",
                json!({"error_kind":"agent_team_result_unavailable","state_changed":null,"dispatch_certainty":"unknown"}),
            ),
        },
    }
}

pub(crate) fn supports(tool: &str) -> bool {
    supports_execution(tool)
        || matches!(
            tool,
            "team_policy_get"
                | "team_decision"
                | "team_plan_register"
                | "team_plan_list"
                | "team_plan_read"
                | "team_task_read"
                | "team_receipt_list"
                | "team_member_create"
                | "team_task_start"
                | "team_task_execution_read"
                | "team_task_complete"
        )
}

/// These names are routed to the asynchronous isolated execution adapter.
pub(crate) fn supports_execution(tool: &str) -> bool {
    crate::agent_team_delivery::supports(tool)
        || matches!(
            tool,
            "team_cli_start"
                | "team_cli_observe"
                | "team_cli_close"
                | "team_cli_catalog"
                | "team_cli_delivery_collect"
                | "team_agent_start"
        )
}

pub(crate) fn call(
    db: &Database,
    binding: &VerifiedAgentRun,
    tool: &str,
    arguments: Value,
) -> ToolResult {
    if let Err(error) = db.validate_agent_run(binding) {
        return denied(error.code());
    }
    macro_rules! input {
        ($kind:ty) => {
            match serde_json::from_value::<$kind>(arguments) {
                Ok(value) => value,
                Err(_) => return denied("agent_team_invalid_input"),
            }
        };
    }
    match tool {
        "team_decision" => {
            let request = input!(webcodex_store::team_decision::TeamDecisionRequest);
            output(db.team_decision(binding, request))
        }
        "team_policy_get" => {
            let request = input!(Task);
            output(db.agent_team_policy(binding, &request.task_id))
        }
        "team_plan_register" => {
            let request = input!(Register);
            output(db.register_agent_team_plan(
                binding,
                &request.root_task_id,
                request.expected_policy_revision,
                request.plan,
                &request.idempotency_key,
            ))
        }
        "team_plan_list" => {
            let request = input!(Page);
            output(db.list_agent_team_plans(
                binding,
                &request.task_id,
                request.offset,
                request.limit,
            ))
        }
        "team_plan_read" => {
            let request = input!(PlanRead);
            output(db.read_agent_team_plan(
                binding,
                &request.root_task_id,
                request.offset,
                request.limit,
            ))
        }
        "team_task_read" => {
            let request = input!(Task);
            output(db.read_agent_team_task(binding, &request.task_id))
        }
        "team_receipt_list" => {
            let request = input!(Page);
            output(db.list_agent_team_receipts(
                binding,
                &request.task_id,
                request.offset,
                request.limit,
            ))
        }
        "team_member_create" => {
            let request = input!(MemberCreate);
            if !bounded(&request.root_task_id)
                || !bounded(&request.idempotency_key)
                || request.max_cli_calls > 32
            {
                return denied("agent_team_invalid_input");
            }
            if let Err(error) = db.authorize_agent_assignment(
                binding,
                &request.root_task_id,
                AgentAssignmentOperation::Delegate,
            ) {
                return denied(error.code());
            }
            let directory = member_task_directory(binding, &request.idempotency_key);
            output(db.create_agent_member(
                binding,
                AgentMemberRequest {
                    title: request.title,
                    brief: request.brief,
                    task_directory: directory,
                    inputs: request.inputs,
                    room_ids: request.room_ids,
                    policy: AgentEnrollmentPolicy {
                        max_subagents: 0,
                        max_cli_calls: request.max_cli_calls,
                    },
                },
                &request.idempotency_key,
            ))
        }
        "team_task_start" => {
            let request = input!(TaskStart);
            output(db.start_agent_public_task(binding, &request.task_id, &request.idempotency_key))
        }
        "team_task_execution_read" => {
            let request = input!(Task);
            output(db.read_agent_public_task_execution(binding, &request.task_id))
        }
        "team_task_complete" => {
            let request = input!(TaskComplete);
            let outcome = match task_completion_outcome(&request.outcome) {
                Ok(outcome) => outcome,
                Err(code) => return denied(code),
            };
            output(db.complete_agent_public_task(
                binding,
                &request.task_id,
                &request.execution_id,
                request.expected_revision,
                outcome,
                request.terminal_result.as_deref(),
                request.terminal_reason.as_deref(),
                &request.completion_key,
            ))
        }
        "team_cli_start" => {
            let request = input!(CliStart);
            if !bounded(&request.root_task_id)
                || !bounded(&request.source_task_id)
                || !bounded(&request.fallback_thinking_profile_id)
                || !bounded(&request.idempotency_key)
                || request.call_index >= 32
            {
                return denied("agent_team_invalid_input");
            }
            if let Err(error) = db.authorize_agent_assignment(
                binding,
                &request.source_task_id,
                AgentAssignmentOperation::UseCli,
            ) {
                return denied(error.code());
            }
            // No proven isolated execution adapter is connected to this entry.
            // Do not reserve a slot or fall back to a generic process command.
            denied("team_execution_backend_unavailable")
        }
        "team_agent_start" => {
            let request = input!(AgentStart);
            if let Err(error) = request.validate() {
                return denied(error);
            }
            // Do not bypass real profile discovery through this synchronous
            // read/lifecycle entry. The async adapter owns B2 reservation.
            denied("agent_team_async_execution_required")
        }
        _ => denied("agent_team_operation_not_admitted"),
    }
}

pub(crate) fn tool_specs() -> Vec<Value> {
    let task = json!({"type":"string"});
    let page = json!({"type":"object","properties":{"task_id":task,"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["task_id"],"additionalProperties":false});
    let exact = json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"],"additionalProperties":false});
    let root_page = json!({"type":"object","properties":{"root_task_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":20}},"required":["root_task_id"],"additionalProperties":false});
    let cli = json!({"type":"object","properties":{"provider_id":{"type":"string"},"model":{"type":"string"},"effort":{"type":["string","null"],"minLength":1,"description":"Use an explicitly discovered effort, null for an effort-pinned Antigravity model slug or when unsupported, or project_policy to resolve the saved effort at expected_policy_revision. project_policy requires the enabled provider and its exact saved model; the frozen plan stores the resolved value, including null, never the selector."},"instruction":{"type":["string","null"]},"inputs":{"type":"array","maxItems":8,"description":"Explicit immutable UTF-8 material, at most 32 KiB serialized JSON total; labels do not grant host filesystem access.","items":{"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":128},"content":{"type":"string"}},"required":["name","content"],"additionalProperties":false}}},"required":["provider_id","model","effort"],"additionalProperties":false});
    let member = json!({"type":"object","properties":{"task_id":{"type":"string"},"cli_calls":{"type":"array","maxItems":32,"items":cli}},"required":["task_id","cli_calls"],"additionalProperties":false});
    let register = json!({"type":"object","properties":{"root_task_id":{"type":"string"},"expected_policy_revision":{"type":"integer","minimum":0},"idempotency_key":{"type":"string"},"plan":{"type":"object","properties":{"manager":member,"subagents":{"type":"array","maxItems":32,"items":member}},"required":["manager","subagents"],"additionalProperties":false}},"required":["root_task_id","expected_policy_revision","idempotency_key","plan"],"additionalProperties":false});
    let agent_start = json!({"type":"object","properties":{"root_task_id":{"type":"string","minLength":1,"maxLength":128},"source_task_id":{"type":"string","minLength":1,"maxLength":128},"enrollment_id":{"type":"string","minLength":1,"maxLength":128},"thinking_profile_id":{"type":"string","minLength":1,"maxLength":128},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["root_task_id","source_task_id","enrollment_id","thinking_profile_id","idempotency_key"],"additionalProperties":false});
    let cli_start = json!({"type":"object","properties":{"root_task_id":{"type":"string"},"source_task_id":{"type":"string"},"call_index":{"type":"integer","minimum":0,"maximum":31},"fallback_thinking_profile_id":{"type":"string","minLength":1},"idempotency_key":{"type":"string","minLength":1}},"required":["root_task_id","source_task_id","call_index","fallback_thinking_profile_id","idempotency_key"],"additionalProperties":false});
    let member_create = json!({"type":"object","properties":{"root_task_id":{"type":"string","minLength":1,"maxLength":128},"title":{"type":"string","minLength":1},"brief":{"type":"string","minLength":1,"maxLength":8192},"inputs":{"type":"array","maxItems":8,"items":{"type":"object","properties":{"name":{"type":"string"},"content":{"type":"string"}},"required":["name","content"],"additionalProperties":false}},"room_ids":{"type":"array","maxItems":64,"items":{"type":"string"}},"max_cli_calls":{"type":"integer","minimum":0,"maximum":32},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["root_task_id","title","brief","inputs","room_ids","max_cli_calls","idempotency_key"],"additionalProperties":false});
    let task_start = json!({"type":"object","properties":{"task_id":{"type":"string"},"idempotency_key":{"type":"string","minLength":1,"maxLength":128}},"required":["task_id","idempotency_key"],"additionalProperties":false});
    let task_complete = json!({"type":"object","properties":{"task_id":{"type":"string"},"execution_id":{"type":"string"},"expected_revision":{"type":"integer","minimum":1},"outcome":{"type":"string","enum":["succeeded","failed"]},"terminal_result":{"type":["string","null"]},"terminal_reason":{"type":["string","null"]},"completion_key":{"type":"string","minLength":1,"maxLength":128}},"required":["task_id","execution_id","expected_revision","outcome","completion_key"],"additionalProperties":false});
    [
        ("team_policy_get","Read public project team policy and your immutable assignment budget. No policy changes are allowed.",exact.clone()),
        ("team_plan_register","Manager only: freeze the assigned root plan using already delegated child tasks. Original policy bounds, per-member limits, assignment total budgets and terminal-task rules all apply. Each CLI call can include additional bounded immutable inputs with name/content. Native project tasks receive a separate authorized source copy when the Runner supports it; private-only assignments use these inputs without host repository access. Required tools and dependencies must exist in the execution resources or project copy. Reuse a key only for the same intent.",register),
        ("team_plan_list","List only the plan for your assigned manager task, with exact bounded pagination. This cannot enumerate another task owned by the same human.",page.clone()),
        ("team_plan_read","Manager only: read your original frozen plan, separately authorized immutable cli_rounds metadata (slot selections, input hashes/counts and first_call_indices; source bundles are not repeated), public task briefs, full-ledger completion gate, current decision_reviews and one receipt page. A receipt page never substitutes for completion evidence. Agent/CLI execution counts are not votes; report each registered issue's current conclusion and independent evidence counts.",root_page),
        ("team_task_read","Read the public title, brief and state of your exact assigned task. Private Goal, Memory and logs are excluded.",exact.clone()),
        ("team_receipt_list","Read bounded execution receipts: managers see their explicitly delegated plan, or only their own task before a plan is registered; members see only their own task. Receipt text and raw logs are excluded.",page),
        ("team_member_create","Manager only: create a real child task and reserve an independent Member identity under your assigned root. Supply an explicit brief, authorized input snapshots, permitted room IDs and the child's CLI ceiling. Identity, project and private task directory are derived by the server. Returns public IDs, never a claim grant; this does not launch a host. Reuse the same key and identical request to recover the same child.",member_create),
        ("team_task_start","Start the public lifecycle of your exact assigned task using a stable idempotency key. This records logical ownership and does not start a process or grant new permissions. Exact replay returns the original execution receipt.",task_start),
        ("team_task_execution_read","Read the public lifecycle receipt of your exact assigned task, or null before it starts. This is not Runner process or stop evidence.",exact.clone()),
        ("team_task_complete","Complete your exact assigned public task with its execution ID, current revision, succeeded or failed outcome, and stable completion key. Only publish authorized result summaries. The server requires quiescence and, for success, full-ledger team completion evidence plus current verified support for explicitly required team_decision facts. Completion freezes decision_reviews; ordinary tasks without issues gain no evidence gate. Report insufficient/disputed conclusions honestly. This neither stops processes nor completes a parent's task; exact replay preserves the original result.",task_complete),
        ("team_cli_start","Request your own frozen CLI slot with an explicitly discovered fallback thinking profile and stable key. The server validates the actual provider catalog and delegation controls, creates one independent worker identity on the original slot, and dispatches only a committed isolated Job. Replays preserve original worker/target/model and never blindly resend unknown work. Missing prerequisites reject before reservation; an existing incomplete preparation requires reconciliation.",cli_start),
        ("team_agent_start","Manager only: queue an already created Member from your frozen plan using its exact task and enrollment receipt. Explicitly choose a current discovered thinking profile and a stable idempotency key. The server rechecks your assignment, plan, member identity and ordinary slot in one transaction. Exact replay retains the original queue binding; a queued receipt is not evidence that a host window started. Never supply credentials or invent another identity on retry.",agent_start),
    ].into_iter().map(|(name,description,input)|json!({"name":name,"description":description,"inputSchema":input}))
        .chain(std::iter::once(crate::agent_team_decision::tool_spec())).collect()
}
