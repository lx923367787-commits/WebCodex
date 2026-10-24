import { useEffect, useRef, useState } from "react";
import type { AiOfficeApi, OfficeCliCall, OfficePolicy } from "./types";
import { officeError } from "./OfficePolicyPanel";
import { RecoveryStatusPanel, type RecoveryObservation } from "./RecoveryStatusPanel";
import { OfficeGlyph, OfficeScene, type SceneMember } from "./OfficeScene";
import { nicknameForTask } from "../../lib/office-nicknames";
import "./team-task-panel.css";

interface PlanSummary {
  project_id: string; root_task_id: string; policy_revision: number;
  status: "active" | "cancelled"; cancel_requested: boolean;
  subagent_count: number; cli_call_count: number;
  created_at_unix_ms: number; updated_at_unix_ms: number;
}
interface PlanList { plans: PlanSummary[]; total_count: number; next_offset: number | null }
interface PlanMember { task_id: string; cli_calls: OfficeCliCall[] }
interface ManagedPlan {
  project_id: string; root_task_id: string; policy: OfficePolicy;
  plan: { manager: PlanMember; subagents: PlanMember[] };
  status: "active" | "cancelled"; cancel_requested: boolean;
}
interface CliAttempt {
  attempt_id: string; project_id: string; root_task_id: string; source_task_id: string;
  call_index: number; provider_id: string; model: string; effort: string | null;
  fallback_thinking_profile_id: string; timeout_secs: number | null;
  dispatch_state: "reserved" | "dispatching" | "bound" | "terminal" | "unknown";
  job_id: string | null; execution_state: "not_started" | "completed" | "timed_out" | "outcome_unknown" | null;
  error_code: string | null; observation_revision: number;
}
interface AttemptPage { attempts: CliAttempt[]; total_count: number; next_offset: number | null }
interface FallbackRecord extends RecoveryObservation {
  host_request_id?: string | null; thinking_profile_id?: string;
  cancel_requested?: boolean; response_observed?: boolean; slot_held?: boolean;
}
type AgentState = "queued" | "starting" | "running" | "unknown" | "cancelled" | "confirmed_not_started" | "verified_terminal";
interface TeamAgent {
  team_agent_attempt_id: string; project: string; root_task_id: string; source_task_id: string;
  thinking_profile_id: string; role: "subagent"; state: AgentState;
  host_request_id: string | null; cancel_requested: boolean; response_observed: boolean;
  revision: number; counts_toward_regular_min: true; counts_toward_regular_max: true; business_verified: false;
}
interface TaskSummary { task_id: string; title: string; state: "ready" | "active" | "succeeded" | "failed" }
interface MemberCompletion {
  task_id: string; role: "manager" | "subagent";
  planned_cli_calls: number; consumed_cli_calls: number; confirmed_cli_calls: number; required_cli_calls: number;
  dispatch_confirmed: boolean; minimum_met: boolean;
}
interface TeamCompletion {
  root_task_id: string; project_id: string; mode: "auto" | "custom"; task_state: TaskSummary["state"];
  planned_subagents: number; confirmed_subagents: number; required_subagents: number;
  planned_cli_calls: number; consumed_cli_calls: number; confirmed_cli_calls: number;
  minimums_satisfied: boolean; success_gate: "not_required" | "pending" | "satisfied" | "cancelled";
  members: MemberCompletion[];
}
interface PlanDetail {
  plan: ManagedPlan;
  attempts: AttemptPage;
  fallbacks: FallbackRecord[];
  fallbacks_may_be_truncated?: boolean;
  agents?: TeamAgent[];
  task_summaries?: TaskSummary[];
  completion?: TeamCompletion;
  enforcement: "managed_tasks";
}

const DISPATCH = { reserved: "调用槽已预留", dispatching: "正在发送", bound: "已关联 Job", terminal: "执行终态已观测", unknown: "发送结果待核实" };
const EXECUTION = { not_started: "未启动", completed: "执行已结束", timed_out: "执行超时", outcome_unknown: "执行结果待核实" };
const AGENT_STATES: Record<AgentState, string> = { queued: "等待派发", starting: "正在启动对话", running: "对话进行中", unknown: "派发结果待核实", cancelled: "已请求取消", confirmed_not_started: "已核实未启动", verified_terminal: "宿主已结束 · 待验收" };
const TASK_STATES: Record<TaskSummary["state"], string> = { ready: "任务记录待执行", active: "任务记录进行中", succeeded: "任务记录已完成", failed: "任务记录失败" };
const PAGE_SIZE = 20;

function object(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}
function validCount(value: unknown): value is number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0; }
function parseList(value: unknown, projectId: string): PlanList {
  const result = object(value);
  if (!result || !Array.isArray(result.plans) || !validCount(result.total_count) || !(result.next_offset === null || validCount(result.next_offset)) || result.plans.length > PAGE_SIZE) throw new Error("团队计划列表格式无效，请刷新观测。");
  for (const entry of result.plans) {
    const item = object(entry);
    if (!item || item.project_id !== projectId || typeof item.root_task_id !== "string" || !validCount(item.subagent_count) || !validCount(item.cli_call_count) || typeof item.cancel_requested !== "boolean") throw new Error("团队计划的项目归属或返回字段无效。");
  }
  return result as unknown as PlanList;
}
function validMember(value: unknown): boolean {
  const member = object(value);
  return Boolean(member && typeof member.task_id === "string" && Array.isArray(member.cli_calls) && member.cli_calls.every((item) => {
    const call = object(item);
    return call && typeof call.provider_id === "string" && typeof call.model === "string";
  }));
}
function parseAttemptPage(value: unknown, projectId: string, rootId: string, offset: number): AttemptPage {
  const page = object(value);
  if (!page || !Array.isArray(page.attempts) || page.attempts.length > PAGE_SIZE || !validCount(page.total_count) || !(page.next_offset === null || validCount(page.next_offset) && page.next_offset > offset)) throw new Error("CLI 尝试分页信息无效。");
  for (const entry of page.attempts) {
    const attempt = object(entry);
    if (!attempt || attempt.project_id !== projectId || attempt.root_task_id !== rootId || typeof attempt.attempt_id !== "string" || typeof attempt.source_task_id !== "string" || typeof attempt.model !== "string" || typeof attempt.provider_id !== "string") throw new Error("CLI 尝试记录的任务归属无效。");
  }
  return page as unknown as AttemptPage;
}
function parseDetail(value: unknown, projectId: string, rootId: string): PlanDetail {
  const result = object(value); const plan = object(result?.plan); const assignments = object(plan?.plan); const policy = object(plan?.policy); const attempts = object(result?.attempts);
  if (!result || result.enforcement !== "managed_tasks" || !plan || plan.project_id !== projectId || plan.root_task_id !== rootId || !policy || !validCount(policy.revision) || !assignments || !validMember(assignments.manager) || !Array.isArray(assignments.subagents) || !assignments.subagents.every(validMember) || !attempts || !Array.isArray(attempts.attempts) || !validCount(attempts.total_count) || !(attempts.next_offset === null || validCount(attempts.next_offset))) throw new Error("团队任务详情格式或项目归属无效。");
  for (const entry of attempts.attempts) {
    const attempt = object(entry);
    if (!attempt || attempt.project_id !== projectId || attempt.root_task_id !== rootId || typeof attempt.attempt_id !== "string" || typeof attempt.source_task_id !== "string" || typeof attempt.model !== "string" || typeof attempt.provider_id !== "string") throw new Error("CLI 尝试记录的任务归属无效。");
  }
  if (!Array.isArray(result.fallbacks)) throw new Error("服务未提供恢复记录观测，当前无法确认恢复槽状态。");
  for (const entry of result.fallbacks) {
    const fallback = object(entry);
    if (!fallback || fallback.root_task_id !== rootId || typeof fallback.fallback_id !== "string" || typeof fallback.state !== "string") throw new Error("救场记录的任务归属无效。");
  }
  const managerId = (assignments.manager as PlanMember).task_id;
  const subagentIds = new Set((assignments.subagents as PlanMember[]).map((member) => member.task_id));
  const taskIds = new Set([managerId, ...subagentIds]);
  if (result.agents !== undefined) {
    if (!Array.isArray(result.agents)) throw new Error("普通伙伴派发观测格式无效。");
    const seen = new Set<string>();
    for (const entry of result.agents) {
      const agent = object(entry);
      if (!agent || agent.project !== projectId || agent.root_task_id !== rootId || typeof agent.source_task_id !== "string" || !subagentIds.has(agent.source_task_id) || seen.has(agent.source_task_id) || agent.role !== "subagent" || typeof agent.team_agent_attempt_id !== "string" || typeof agent.thinking_profile_id !== "string" || typeof agent.state !== "string" || typeof agent.cancel_requested !== "boolean" || typeof agent.response_observed !== "boolean" || !(agent.host_request_id === null || typeof agent.host_request_id === "string") || !validCount(agent.revision) || agent.counts_toward_regular_min !== true || agent.counts_toward_regular_max !== true) throw new Error("普通伙伴的项目或任务关联无效。");
      seen.add(agent.source_task_id);
    }
  }
  if (result.task_summaries !== undefined) {
    if (!Array.isArray(result.task_summaries)) throw new Error("任务标题观测格式无效。");
    const seen = new Set<string>();
    for (const entry of result.task_summaries) {
      const summary = object(entry);
      if (!summary || typeof summary.task_id !== "string" || !taskIds.has(summary.task_id) || seen.has(summary.task_id) || typeof summary.title !== "string" || typeof summary.state !== "string") throw new Error("任务标题不属于当前登记计划。");
      seen.add(summary.task_id);
    }
  }
  if (result.completion !== undefined) {
    const completion = object(result.completion);
    const countFields = ["planned_subagents", "confirmed_subagents", "required_subagents", "planned_cli_calls", "consumed_cli_calls", "confirmed_cli_calls"];
    if (!completion || completion.project_id !== projectId || completion.root_task_id !== rootId || !countFields.every((key) => validCount(completion[key])) || !["auto", "custom"].includes(String(completion.mode)) || !["ready", "active", "succeeded", "failed"].includes(String(completion.task_state)) || !["not_required", "pending", "satisfied", "cancelled"].includes(String(completion.success_gate)) || typeof completion.minimums_satisfied !== "boolean" || !Array.isArray(completion.members)) throw new Error("实际执行计数的任务归属或完成条件无效。");
    const seen = new Set<string>();
    for (const entry of completion.members) {
      const member = object(entry);
      if (!member || typeof member.task_id !== "string" || !taskIds.has(member.task_id) || seen.has(member.task_id) || member.role !== (member.task_id === managerId ? "manager" : "subagent") || !["planned_cli_calls", "consumed_cli_calls", "confirmed_cli_calls", "required_cli_calls"].every((key) => validCount(member[key])) || typeof member.dispatch_confirmed !== "boolean" || typeof member.minimum_met !== "boolean") throw new Error("成员实际执行计数不属于当前登记计划。");
      seen.add(member.task_id);
    }
    if (seen.size !== taskIds.size) throw new Error("成员实际执行计数尚未完整返回。");
  }
  // Select only explicit fields when rendering; instruction/result text is never displayed.
  return result as unknown as PlanDetail;
}

function labelForPlan(plan: Pick<PlanSummary, "status" | "cancel_requested">): string {
  return plan.cancel_requested || plan.status === "cancelled" ? "已请求取消" : "计划已登记";
}

export function TeamTaskPanel({ api, projectId, demo = false }: { api: Pick<AiOfficeApi, "team">; projectId: string; demo?: boolean }) {
  const team = demo ? undefined : api.team;
  const [offset, setOffset] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const [page, setPage] = useState<PlanList | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<PlanDetail | null>(null);
  const [listLoading, setListLoading] = useState(false);
  const [detailLoading, setDetailLoading] = useState(false);
  const [listError, setListError] = useState("");
  const [detailError, setDetailError] = useState("");

  useEffect(() => {
    setOffset(0); setPage(null); setSelected(null); setDetail(null); setListError(""); setDetailError("");
  }, [projectId, team]);

  useEffect(() => {
    if (!team) return;
    let current = true;
    setListLoading(true); setListError("");
    void team("plan-list", { project: projectId, offset, limit: PAGE_SIZE }).then((result) => {
      if (!current) return;
      const next = parseList(result, projectId);
      setPage(next);
      setSelected((previous) => next.plans.some((plan) => plan.root_task_id === previous) ? previous : next.plans[0]?.root_task_id ?? null);
    }).catch((cause) => { if (current) { setListError(officeError(cause)); setPage(null); setSelected(null); } }).finally(() => { if (current) setListLoading(false); });
    return () => { current = false; };
  }, [team, projectId, offset, refresh]);

  useEffect(() => {
    setDetail((previous) => previous?.plan.root_task_id === selected && previous.plan.project_id === projectId ? previous : null); setDetailError("");
    if (!team || !selected) { setDetailLoading(false); return; }
    let current = true;
    setDetailLoading(true);
    void team("plan-read", { project: projectId, root_task_id: selected }).then((result) => {
      if (current) setDetail(parseDetail(result, projectId, selected));
    }).catch((cause) => { if (current) { setDetail(null); setDetailError(officeError(cause)); } }).finally(() => { if (current) setDetailLoading(false); });
    return () => { current = false; };
  }, [team, projectId, selected, refresh]);

  useEffect(() => {
    if (!team || listLoading || detailLoading) return;
    const interval = window.setInterval(() => { if (document.visibilityState === "visible") setRefresh((value) => value + 1); }, 15_000);
    return () => window.clearInterval(interval);
  }, [team, listLoading, detailLoading]);

  if (!team) return <section className="ao-team-unconnected"><span className="ao-small-kicker">MANAGED TASKS</span><h3>真实派工尚未接入</h3><p>{demo ? "当前是演示空间，人物与样例任务不代表真实执行。" : "当前接口未提供 Server 团队任务观测。"}</p><p>这里用于查看登记计划与真实观测；启动和取消操作需等待可信后端接入。</p><RecoveryStatusPanel /></section>;

  const heldSlots = detail?.fallbacks.filter((record) => record.slot_held === true).length ?? 0;
  const slot: 0 | 1 | null = heldSlots === 1 ? 1 : heldSlots === 0 && detail?.fallbacks_may_be_truncated === false && detail.fallbacks.every((record) => typeof record.slot_held === "boolean") ? 0 : null;
  return <section className="ao-team-panel" aria-label="运行任务">
    <header className="ao-team-heading"><div><span className="ao-small-kicker">SERVER · MANAGED TASKS</span><h3>登记的任务，真实的执行记录</h3><p>这里只读展示 Server 记录；窗口活跃请求不等同于任务完成进度。</p></div><button type="button" className="ao-button ao-button-outline" disabled={listLoading || detailLoading} onClick={() => setRefresh((value) => value + 1)}>刷新任务</button></header>
    <div className="ao-team-candidate" role="note" aria-label="执行能力状态"><strong>启动与取消控制尚未接入</strong><p>当前没有可信的执行与取消后端。读取到登记计划或既有执行记录，不代表已能启动新任务或发送取消请求。</p></div>
    {listError && <p className="ao-team-error" role="alert">{listError}</p>}
    {listLoading && !page && <p className="ao-team-placeholder" role="status">正在读取登记计划…</p>}
    {page && <><div className="ao-team-plan-list" aria-label="登记计划列表">{page.plans.map((plan) => <button type="button" key={plan.root_task_id} className={"ao-team-plan-row " + (selected === plan.root_task_id ? "is-selected" : "")} aria-pressed={selected === plan.root_task_id} aria-label={"查看任务 " + plan.root_task_id} onClick={() => setSelected(plan.root_task_id)}><div><code>{plan.root_task_id}</code><span>{labelForPlan(plan)}</span></div><p>计划 {plan.subagent_count} 位常规子 Agent · {plan.cli_call_count} 个计划 CLI 调用槽 · 策略 v{plan.policy_revision}</p></button>)}</div>{page.plans.length === 0 && <div className="ao-team-placeholder"><strong>当前页没有已登记计划</strong><p>等待总经理通过 Server 工具登记真实任务。</p></div>}<nav className="ao-team-pagination" aria-label="计划分页"><span>第 {Math.floor(offset / PAGE_SIZE) + 1} 页 · 共 {page.total_count} 个计划</span><button type="button" disabled={offset === 0 || listLoading} onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}>上一页</button><button type="button" disabled={page.next_offset === null || listLoading} onClick={() => { if (page.next_offset !== null) setOffset(page.next_offset); }}>下一页</button></nav></>}
    {detailLoading && <p className="ao-team-placeholder" role="status">正在读取任务关联与执行记录…</p>}
    {detailError && <p className="ao-team-error" role="alert">{detailError}</p>}
    {detail && <div className="ao-team-detail">
      <section className="ao-team-card"><header><div><span className="ao-small-kicker">ROOT TASK</span><h4>{taskSummary(detail, detail.plan.root_task_id)?.title || detail.plan.root_task_id}</h4>{taskSummary(detail, detail.plan.root_task_id)?.title && <code>{detail.plan.root_task_id}</code>}</div><button type="button" className="ao-button ao-button-outline" disabled title="尚无可信的取消接口，本页不会发送取消请求。">{detail.plan.cancel_requested || detail.plan.status === "cancelled" ? "已请求取消（只读）" : "取消暂不可用"}</button></header><p className="ao-team-note">取消控制需等待可信接口接入。已有取消记录不表示进程已停止；原 Job、普通伙伴和救场伙伴仍需独立观测。</p>{detail.task_summaries === undefined && <p className="ao-team-note">Server 尚未提供任务标题与业务状态观测，当前仅显示登记的任务 ID。</p>}<div className="ao-team-assignment-grid"><Assignment member={detail.plan.plan.manager} role="总经理" summary={taskSummary(detail, detail.plan.plan.manager.task_id)} dispatchObserved={detail.agents !== undefined} />{detail.plan.plan.subagents.map((member) => <Assignment key={member.task_id} member={member} role="常规子 Agent" summary={taskSummary(detail, member.task_id)} agent={detail.agents?.find((agent) => agent.source_task_id === member.task_id)} dispatchObserved={detail.agents !== undefined} />)}</div></section>
      <ManagedTeamOffice key={detail.plan.root_task_id} detail={detail} />
      <CompletionPanel detail={detail} />
      <RegularAgentList detail={detail} />
      <CliAttemptsPanel key={projectId + "/" + detail.plan.root_task_id} team={team} projectId={projectId} rootId={detail.plan.root_task_id} initialPage={detail.attempts} />
      <section className="ao-team-recovery"><header><div><h4>独立救场伙伴</h4><p>不占常规人数上下限，候选回复仍需验收。</p></div><strong aria-label={"根任务恢复槽占用 " + (slot ?? "未核实")}>{slot ?? "—"}<small>/ 1</small></strong></header>{detail.fallbacks_may_be_truncated !== false && <p className="ao-team-note">恢复记录可能未完整返回，不能据此认定未显示的恢复槽已释放。</p>}{detail.fallbacks.length === 0 ? <RecoveryStatusPanel availability="connected" slotOccupancy={slot} /> : detail.fallbacks.map((record) => <div className="ao-team-fallback" key={record.fallback_id}><RecoveryStatusPanel observation={{ ...record, policy_revision: record.policy_revision ?? detail.plan.policy.revision, blocked_reason: record.blocked_reason ?? null }} availability="connected" slotOccupancy={typeof record.slot_held === "boolean" ? record.slot_held ? 1 : 0 : null} source="live" />{record.thinking_profile_id && <p className="ao-team-note">总经理已选档位：<code>{record.thinking_profile_id}</code></p>}{record.host_request_id && <p className="ao-team-note">宿主请求：<code>{record.host_request_id}</code></p>}{record.response_observed && <p className="ao-team-candidate" role="status">已收到候选结果，等待业务验收。收到回复不代表任务成功，也不自动释放恢复槽。</p>}{record.cancel_requested && <p className="ao-team-note">已请求取消恢复，仍需核实宿主停止结果。</p>}</div>)}</section>
    </div>}
  </section>;
}

function taskSummary(detail: PlanDetail, taskId: string): TaskSummary | undefined {
  return detail.task_summaries?.find((summary) => summary.task_id === taskId);
}
function dispatchLabel(agent: TeamAgent | undefined, observed: boolean): string {
  if (!observed) return "派发观测未提供";
  if (!agent) return "尚未派发";
  return AGENT_STATES[agent.state] ?? "派发状态待核实";
}
function Assignment({ member, role, summary, agent, dispatchObserved }: { member: PlanMember; role: string; summary?: TaskSummary; agent?: TeamAgent; dispatchObserved: boolean }) {
  return <div className="ao-team-assignment"><span>{role} · {role === "总经理" ? "主任务协调" : dispatchLabel(agent, dispatchObserved)}</span><strong>{summary?.title || "任务标题未提供"}</strong><code>{member.task_id}</code><small>{summary ? TASK_STATES[summary.state] ?? "任务状态未提供" : "任务状态未提供"}</small><small>{member.cli_calls.length} 个已登记 CLI 调用槽</small>{member.cli_calls.map((call, index) => <p key={index}>{call.provider_id === "workbuddy" ? "WorkBuddy" : call.provider_id === "antigravity" ? "Antigravity" : call.provider_id} · {call.model}{call.effort ? " / " + call.effort : ""}</p>)}</div>;
}
function Fact({ label, value }: { label: string; value: string }) { return <div><dt>{label}</dt><dd>{value}</dd></div>; }

function CompletionPanel({ detail }: { detail: PlanDetail }) {
  const completion = detail.completion;
  if (!completion) return <section className="ao-team-card" aria-label="实际执行与完成条件"><header><h4>实际执行与完成条件</h4></header><p className="ao-team-note">Server 尚未提供实际执行计数与完成条件，不能从计划人数、调用槽或派发记录数量推算。</p></section>;
  const gate = {
    not_required: { title: "自主模式 · 不设置最低执行门槛", note: "总经理自主安排人数与调用次数，任务仍需业务验收。" },
    pending: { title: "最低执行要求尚未核实满足", note: "当前不能将该团队任务标记为成功；等待可信派发与调用证据。" },
    satisfied: { title: "最低执行要求已核实满足", note: "数量条件已满足，仍须验收任务成果；这不等于业务成功。" },
    cancelled: { title: "计划已请求取消", note: "取消中的计划不能继续标记成功，停止结果仍须独立核实。" },
  }[completion.success_gate];
  return <section className="ao-team-card ao-team-completion" aria-label="实际执行与完成条件"><header><h4>实际执行与完成条件</h4><span>{TASK_STATES[completion.task_state]}</span></header>
    <div className={"ao-completion-gate is-" + completion.success_gate} role="status"><strong>{gate.title}</strong><p>{gate.note}</p></div>
    <div className="ao-completion-metrics"><CompletionCount label="计划子 Agent" value={completion.planned_subagents} /><CompletionCount label="已核实派发" value={completion.confirmed_subagents} /><CompletionCount label="最低派发要求" value={completion.required_subagents} /><CompletionCount label="计划 CLI 调用" value={completion.planned_cli_calls} /><CompletionCount label="已占用调用预算" value={completion.consumed_cli_calls} /><CompletionCount label="已核实 CLI 调用" value={completion.confirmed_cli_calls} /></div>
    <p className="ao-team-note">已核实数量为 0 表示尚无足够证据，不代表从未执行。占用预算包含预留、失败和结果未知的尝试，不等于已执行次数。</p>
    <div className="ao-completion-members">{completion.members.map((member) => <article className="ao-completion-member" key={member.task_id} aria-label={"成员执行计数 " + member.task_id}><header><div><span>{member.role === "manager" ? "总经理" : nicknameForTask(member.task_id) + " · 常规伙伴"}</span><strong>{taskSummary(detail, member.task_id)?.title || "任务标题未提供"}</strong><code>{member.task_id}</code></div><span>{member.role === "manager" ? "主任务协调" : member.dispatch_confirmed ? "派发已核实" : "派发证据待确认"}</span></header><dl><Fact label="计划 CLI" value={String(member.planned_cli_calls)} /><Fact label="已占用预算" value={String(member.consumed_cli_calls)} /><Fact label="已核实调用" value={String(member.confirmed_cli_calls)} /><Fact label="最低调用要求" value={String(member.required_cli_calls)} /></dl><p>{completion.success_gate === "not_required" ? "自主模式不设置最低调用门槛。" : member.minimum_met ? "该成员最低调用数量已满足，仍需业务验收。" : "该成员最低调用证据尚未满足，阻止任务标记成功。"}</p></article>)}</div>
    <p className="ao-team-note">GPT 目前以可信宿主回复核实派发；CLI 以真实 Job 已结束或超时的观测核实调用，失败的实际调用也计入。候选回复不能代替业务验收。</p>
  </section>;
}
function CompletionCount({ label, value }: { label: string; value: number }) {
  return <div className="ao-completion-count"><span>{label}</span><strong aria-label={label + " " + value}>{value}</strong></div>;
}

interface LoadedAttempts extends AttemptPage { through: number }

function mergeAttempts(previous: CliAttempt[], incoming: CliAttempt[]): CliAttempt[] {
  const entries = new Map(previous.map((attempt) => [attempt.attempt_id, attempt]));
  for (const attempt of incoming) {
    const prior = entries.get(attempt.attempt_id);
    if (!prior || attempt.observation_revision >= prior.observation_revision) entries.set(attempt.attempt_id, attempt);
  }
  return [...entries.values()];
}

function CliAttemptsPanel({ team, projectId, rootId, initialPage }: { team: NonNullable<AiOfficeApi["team"]>; projectId: string; rootId: string; initialPage: AttemptPage }) {
  const initial = { ...initialPage, through: initialPage.attempts.length };
  const [loaded, setLoaded] = useState<LoadedAttempts>(initial);
  const [loading, setLoading] = useState<"refresh" | "more" | null>(null);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const latest = useRef(initial);
  const desiredThrough = useRef(initial.through);
  const sequence = useRef(0);
  const busy = useRef(false);

  function commit(page: AttemptPage, through: number, records: CliAttempt[]) {
    const previous = latest.current;
    const next = {
      attempts: mergeAttempts(previous.attempts, records),
      total_count: Math.max(previous.total_count, page.total_count),
      next_offset: through >= previous.through && page.total_count >= previous.total_count ? page.next_offset : previous.next_offset,
      through: Math.max(through, previous.through),
    };
    latest.current = next; desiredThrough.current = Math.max(desiredThrough.current, next.through); setLoaded(next);
  }

  async function readPage(offset: number): Promise<AttemptPage> {
    const response = await team("plan-read", { project: projectId, root_task_id: rootId, attempts_offset: offset, attempts_limit: PAGE_SIZE });
    return parseAttemptPage(parseDetail(response, projectId, rootId).attempts, projectId, rootId, offset);
  }

  useEffect(() => {
    const version = ++sequence.current;
    busy.current = true; setLoading("refresh"); setError("");
    async function refreshLoadedPages() {
      try {
        let page = parseAttemptPage(initialPage, projectId, rootId, 0);
        let records = page.attempts;
        let through = page.attempts.length;
        // Keep the loaded range. When the previous tail grows, also reveal the
        // page containing its first new record, without fetching an unbounded tail.
        const target = latest.current.next_offset === null && page.total_count > latest.current.through
          ? Math.max(desiredThrough.current, latest.current.through + 1)
          : desiredThrough.current;
        while (page.next_offset !== null && through < target) {
          const offset = page.next_offset;
          page = await readPage(offset);
          if (sequence.current !== version) return;
          records = mergeAttempts(records, page.attempts);
          through = offset + page.attempts.length;
        }
        if (sequence.current === version) commit(page, through, records);
      } catch (cause) {
        if (sequence.current === version) setError("CLI 尝试刷新失败，保留已加载记录。" + officeError(cause));
      } finally {
        if (sequence.current === version) { busy.current = false; setLoading(null); }
      }
    }
    void refreshLoadedPages();
    return () => { ++sequence.current; };
  }, [initialPage, team, projectId, rootId, retry]);

  async function loadMore() {
    if (busy.current || latest.current.next_offset === null) return;
    const offset = latest.current.next_offset;
    desiredThrough.current = Math.max(desiredThrough.current, offset + PAGE_SIZE);
    const version = ++sequence.current;
    busy.current = true; setLoading("more"); setError("");
    try {
      const page = await readPage(offset);
      if (sequence.current === version) commit(page, offset + page.attempts.length, page.attempts);
    } catch (cause) {
      if (sequence.current === version) setError("未能读取更多 CLI 尝试。" + officeError(cause));
    } finally {
      if (sequence.current === version) { busy.current = false; setLoading(null); }
    }
  }

  return <section className="ao-team-card" aria-label="CLI 执行尝试"><header><h4>CLI 执行尝试</h4><span>已显示 {loaded.attempts.length} / {loaded.total_count} 条记录</span></header>
    <p className="ao-team-note">按创建顺序读取，每页最多 20 条；记录条数不等于已核实执行次数。</p>
    {error && <div className="ao-team-error" role="alert"><p>{error}</p><button type="button" className="ao-button ao-button-outline" disabled={Boolean(loading)} onClick={() => setRetry((value) => value + 1)}>重读已加载记录</button></div>}
    <div className="ao-team-attempts">{loaded.attempts.map((attempt) => <article className="ao-team-attempt" key={attempt.attempt_id} data-attempt-id={attempt.attempt_id}><div><strong>{attempt.provider_id === "workbuddy" ? "WorkBuddy" : attempt.provider_id === "antigravity" ? "Antigravity" : attempt.provider_id}</strong><span>{DISPATCH[attempt.dispatch_state] ?? "未知发送状态"}</span></div><dl><Fact label="原任务" value={attempt.source_task_id} /><Fact label="执行尝试" value={attempt.attempt_id} /><Fact label="Job" value={attempt.job_id ?? "尚未关联"} /><Fact label="模型 / 强度" value={attempt.model + (attempt.effort ? " / " + attempt.effort : "")} /><Fact label="进程执行状态" value={attempt.execution_state ? EXECUTION[attempt.execution_state] ?? "未知执行状态" : "未提供可信观测"} /><Fact label="已选恢复档位" value={attempt.fallback_thinking_profile_id || "未提供"} />{attempt.error_code && <Fact label="错误代码" value={attempt.error_code} />}</dl></article>)}</div>
    {loaded.attempts.length === 0 && <p className="ao-team-note">暂无已登记的 CLI 执行尝试。</p>}
    <div className="ao-team-attempt-pagination">{loading === "refresh" && <span role="status">正在刷新已加载尝试…</span>}{loaded.next_offset !== null && <button type="button" className="ao-button ao-button-outline" disabled={Boolean(loading)} onClick={() => void loadMore()}>{loading === "more" ? "正在加载更多尝试…" : "加载更多尝试"}</button>}{loaded.next_offset === null && loaded.attempts.length >= loaded.total_count && <span>已显示当前全部尝试</span>}{loaded.next_offset === null && loaded.attempts.length < loaded.total_count && <span>服务尚未提供下一页位置，当前记录不完整。</span>}</div>
  </section>;
}

function RegularAgentList({ detail }: { detail: PlanDetail }) {
  return <section className="ao-team-card"><header><h4>普通 GPT 小伙伴</h4><span>{detail.agents === undefined ? "派发观测未提供" : detail.agents.length + " 条派发记录"}</span></header>
    {detail.agents === undefined ? <p className="ao-team-note">Server 尚未提供普通伙伴派发记录，不能确认成员是否已启动。</p> : detail.agents.length === 0 ? <p className="ao-team-note">计划中的普通伙伴尚未派发。</p> : <div className="ao-team-attempts">{detail.agents.map((agent) => <article className="ao-team-attempt" key={agent.team_agent_attempt_id}><div><strong>{nicknameForTask(agent.source_task_id)} · 普通 GPT 伙伴</strong><span>{dispatchLabel(agent, true)}</span></div><p className="ao-team-assigned-title">{taskSummary(detail, agent.source_task_id)?.title || "任务标题未提供"}</p><dl><Fact label="真实任务 ID" value={agent.source_task_id} /><Fact label="派发记录" value={agent.team_agent_attempt_id} /><Fact label="总经理所选档位" value={agent.thinking_profile_id} /><Fact label="宿主请求" value={agent.host_request_id ?? "尚未关联"} /><Fact label="记录版本" value={String(agent.revision)} /></dl>{agent.response_observed && <p className="ao-team-candidate" role="status">普通伙伴已返回候选回复，业务结果尚未验收。</p>}{agent.state === "verified_terminal" && <p className="ao-team-note">宿主执行终态已核实，不能据此认定业务任务成功。</p>}{agent.cancel_requested && <p className="ao-team-note">普通伙伴已收到取消请求，停止结果仍以宿主观测为准。</p>}</article>)}</div>}
  </section>;
}

function ManagedTeamOffice({ detail }: { detail: PlanDetail }) {
  const [open, setOpen] = useState(false);
  const [selectedId, setSelectedId] = useState(detail.plan.plan.manager.task_id);
  const managerId = detail.plan.plan.manager.task_id;
  const members: SceneMember[] = [detail.plan.plan.manager, ...detail.plan.plan.subagents].map((member) => {
    const manager = member.task_id === managerId;
    const agent = detail.agents?.find((record) => record.source_task_id === member.task_id);
    return {
      id: member.task_id, name: nicknameForTask(member.task_id), role: manager ? "总经理" : "GPT 伙伴",
      state: manager ? "主任务协调" : dispatchLabel(agent, detail.agents !== undefined),
      task: taskSummary(detail, member.task_id)?.title || "任务标题未提供",
      parentId: manager ? null : managerId, working: !manager && agent?.state === "running", kind: manager ? "manager" : "agent",
    };
  });
  const person = members.find((member) => member.id === selectedId) ?? members[0];
  const agent = detail.agents?.find((record) => record.source_task_id === person?.id);
  const summary = person ? taskSummary(detail, person.id) : undefined;
  const regionId = "ao-task-office-" + detail.plan.root_task_id;
  return <section className="ao-team-office"><button type="button" className="ao-team-office-toggle" aria-expanded={open} aria-controls={regionId} onClick={() => setOpen((value) => !value)}><div><span className="ao-small-kicker">YOUR REAL TEAM</span><strong>{open ? "收起任务小人办公室" : "展开任务小人办公室"}</strong></div><span>{members.length} 个计划任务 {open ? "−" : "+"}</span></button>{open && <div className="ao-team-office-content" id={regionId}><p className="ao-team-note">小人对应真实任务 ID，昵称仅用于辨认。走动是装饰；未派发的成员不会标为工作中。</p><OfficeScene members={members} selectedId={person?.id ?? null} onSelect={setSelectedId} demo={false} />{person && <section className="ao-team-person" aria-label="选中伙伴的真实任务"><OfficeGlyph kind={person.kind} residentId={person.id} /><div><strong>{person.name} · {person.role}</strong><h4>{person.task}</h4><dl><Fact label="真实任务 ID" value={person.id} /><Fact label="实际派发状态" value={person.state} /><Fact label="任务记录状态" value={summary ? TASK_STATES[summary.state] ?? "未提供" : "未提供"} /><Fact label="已选档位" value={agent?.thinking_profile_id ?? "未提供"} /><Fact label="宿主请求" value={agent?.host_request_id ?? "未关联"} /></dl>{person.id === managerId && <p className="ao-team-note">当前观测未关联总经理的真实对话，因此不将它标记为正在工作。</p>}{agent?.response_observed && <p className="ao-team-candidate">已收到候选回复，业务验收尚未确认。</p>}</div></section>}</div>}</section>;
}
