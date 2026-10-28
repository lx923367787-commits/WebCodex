import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, expectTypeOf, it, vi } from "vitest";
import type { AiOfficeApi, TeamAction } from "./types";
import { TeamTaskPanel } from "./TeamTaskPanel";
import { nicknameForTask } from "../../lib/office-nicknames";

const project = "runtime/runner/project";
const policy = { revision: 3, mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 }, providers: [] };
function summary(root = "root-a", cancelled = false) {
  return { project_id: project, root_task_id: root, policy_revision: 3, status: cancelled ? "cancelled" : "active", cancel_requested: cancelled, subagent_count: 1, cli_call_count: 1, created_at_unix_ms: 0, updated_at_unix_ms: 1 };
}
function details(root = "root-a", cancelled = false) {
  return {
    enforcement: "managed_tasks",
    fallbacks_may_be_truncated: false,
    plan: { project_id: project, root_task_id: root, policy, plan: { manager: { task_id: root, cli_calls: [] }, subagents: [{ task_id: "child-a", cli_calls: [{ provider_id: "workbuddy", model: "actual-model", effort: "high" }] }] }, status: cancelled ? "cancelled" : "active", cancel_requested: cancelled },
    attempts: { attempts: [{ attempt_id: "attempt-a", project_id: project, root_task_id: root, source_task_id: "child-a", call_index: 0, provider_id: "workbuddy", model: "actual-model", effort: "high", fallback_thinking_profile_id: "observed-profile", timeout_secs: 900, dispatch_state: "bound", job_id: "job-a", execution_state: "timed_out", error_code: "system_timeout", observation_revision: 2, instruction: "PRIVATE_INSTRUCTION_NEVER_RENDER", result_summary: "RAW_RESULT_NEVER_RENDER" }], total_count: 1, next_offset: null },
    fallbacks: [{ fallback_id: "fallback-a", root_task_id: root, source_task_id: "child-a", source_attempt_id: "attempt-a", source_job_id: "job-a", fallback_agent_id: null, policy_revision: 3, reason_code: "deadline_exceeded", state: "running", blocked_reason: null, revision: 2, thinking_profile_id: "observed-profile", host_request_id: "request-a", cancel_requested: false, response_observed: true, slot_held: true, counts_toward_regular_min: false, counts_toward_regular_max: false }],
  };
}
function fixture() {
  return vi.fn<NonNullable<AiOfficeApi["team"]>>(async (action, params) => {
    if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
    if (action === "plan-read") return details(String(params.root_task_id));
    throw new Error("Unexpected action " + action);
  });
}

function peopleDetail() {
  const detail = details();
  detail.plan.plan.subagents.push({ task_id: "child-b", cli_calls: [] });
  return { ...detail,
    task_summaries: [
      { task_id: "root-a", title: "整理项目的工作记录", state: "active" },
      { task_id: "child-a", title: "检查接口与恢复边界", state: "active" },
      { task_id: "child-b", title: "准备验收清单", state: "ready" },
    ],
    agents: [{ team_agent_attempt_id: "agent-attempt-a", project, root_task_id: "root-a", source_task_id: "child-a", thinking_profile_id: "discovered-balanced-profile", role: "subagent", state: "running", host_request_id: "chat-request-a", cancel_requested: false, response_observed: true, revision: 4, counts_toward_regular_min: true, counts_toward_regular_max: true, business_verified: false }],
  };
}

function pagedDetail(offset: number, total: number, root = "root-a", projectId = project) {
  const detail = root === "root-a" ? peopleDetail() : details(root);
  const attempts = Array.from({ length: total }, (_, index) => ({ ...detail.attempts.attempts[0],
    attempt_id: "attempt-" + (index + 1), project_id: projectId, root_task_id: root,
    job_id: (root === "root-a" ? "job-" : root + "-job-") + (index + 1), call_index: index,
    dispatch_state: "terminal", execution_state: index === 20 ? "timed_out" : "completed",
    error_code: index === 20 ? total > 25 ? "updated_failure_evidence" : "quota_exhausted" : null,
    observation_revision: total > 25 ? 3 : 2,
  }));
  return { ...detail, plan: { ...detail.plan, project_id: projectId }, attempts: { attempts: attempts.slice(offset, offset + 20), total_count: total, next_offset: offset + 20 < total ? offset + 20 : null } };
}

function completionDetail() {
  const detail = peopleDetail();
  const call = { provider_id: "workbuddy", model: "actual-model", effort: "high" };
  return { ...detail,
    plan: { ...detail.plan, policy: { ...detail.plan.policy, mode: "custom" }, plan: { manager: { task_id: "root-a", cli_calls: [call] }, subagents: [{ task_id: "child-a", cli_calls: [call, call] }, { task_id: "child-b", cli_calls: [call] }] } },
    completion: { root_task_id: "root-a", project_id: project, mode: "custom", task_state: "active", planned_subagents: 2, confirmed_subagents: 0, required_subagents: 2, planned_cli_calls: 4, consumed_cli_calls: 3, confirmed_cli_calls: 0, minimums_satisfied: false, success_gate: "pending", members: [
      { task_id: "root-a", role: "manager", planned_cli_calls: 1, consumed_cli_calls: 1, confirmed_cli_calls: 0, required_cli_calls: 1, dispatch_confirmed: false, minimum_met: false },
      { task_id: "child-a", role: "subagent", planned_cli_calls: 2, consumed_cli_calls: 2, confirmed_cli_calls: 0, required_cli_calls: 1, dispatch_confirmed: false, minimum_met: false },
      { task_id: "child-b", role: "subagent", planned_cli_calls: 1, consumed_cli_calls: 0, confirmed_cli_calls: 0, required_cli_calls: 1, dispatch_confirmed: false, minimum_met: false },
    ] },
  };
}

describe("managed team task panel", () => {
  it("includes Runner inventory reads in the existing human bridge", () => {
    expectTypeOf<TeamAction>().toEqualTypeOf<"policy-get" | "policy-save" | "plan-list" | "plan-read" | "task-create" | "cli-discover" | "cli-models">();
  });

  it("leaves demonstration and unavailable interfaces disconnected from live dispatch", () => {
    const team = fixture();
    const { rerender } = render(<TeamTaskPanel projectId={project} api={{ team }} demo />);
    expect(screen.getByText("真实派工尚未接入")).toBeInTheDocument();
    expect(screen.getByText(/当前是演示空间/)).toBeInTheDocument();
    expect(team).not.toHaveBeenCalled();
    rerender(<TeamTaskPanel projectId={project} api={{}} />);
    expect(screen.getByText(/当前接口未提供 Server 团队任务观测/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /取消/ })).not.toBeInTheDocument();
  });

  it("reads exact scoped task/Job/model evidence without instructions or invented progress", async () => {
    const team = fixture();
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("已关联 Job");
    expect(team).toHaveBeenCalledWith("plan-list", { project, offset: 0, limit: 20 });
    expect(team).toHaveBeenCalledWith("plan-read", { project, root_task_id: "root-a" });
    expect(screen.getByText("actual-model / high")).toBeInTheDocument();
    expect(screen.getByText("执行超时")).toBeInTheDocument();
    expect(screen.getAllByText("job-a").length).toBeGreaterThan(0);
    expect(screen.queryByText("PRIVATE_INSTRUCTION_NEVER_RENDER")).not.toBeInTheDocument();
    expect(screen.queryByText("RAW_RESULT_NEVER_RENDER")).not.toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(team.mock.calls.every(([action]) => action === "plan-list" || action === "plan-read")).toBe(true);
  });

  it("keeps candidate replies separate from business completion and reads the held slot", async () => {
    const team = fixture();
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    expect(await screen.findByText(/已收到候选结果，等待业务验收/)).toBeInTheDocument();
    expect(screen.getByLabelText("根任务恢复槽占用 1")).toBeInTheDocument();
    expect(screen.getByLabelText("恢复槽占用 1，上限 1")).toBeInTheDocument();
    expect(screen.queryByText("恢复已验收")).not.toBeInTheDocument();
  });

  it("displays verified host termination without claiming task success", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const detail = details();
      detail.fallbacks[0] = { ...detail.fallbacks[0], state: "verified_terminal", slot_held: false };
      return detail;
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    expect(await screen.findByText("宿主终态已核实")).toBeInTheDocument();
    expect(screen.getByText(/终态不等于任务成功/)).toBeInTheDocument();
    expect(screen.getByLabelText("根任务恢复槽占用 0")).toBeInTheDocument();
  });

  it("paginates plan lists using the server offset with a twenty-record limit", async () => {
    const team = fixture();
    team.mockImplementation(async (action, params) => action === "plan-list" ? { plans: [summary(params.offset === 20 ? "root-b" : "root-a")], total_count: 21, next_offset: params.offset === 20 ? null : 20 } : details(String(params.root_task_id)));
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    fireEvent.click(await screen.findByRole("button", { name: "下一页" }));
    await screen.findByRole("button", { name: "查看任务 root-b" });
    expect(team).toHaveBeenCalledWith("plan-list", { project, offset: 20, limit: 20 });
    expect(screen.getByRole("button", { name: "下一页" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "上一页" }));
    await screen.findByRole("button", { name: "查看任务 root-a" });
    expect(screen.getByRole("button", { name: "上一页" })).toBeDisabled();
  });

  it("keeps cancellation unavailable and makes no mutation when clicked or refreshed", async () => {
    const team = fixture();
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    const cancel = await screen.findByRole("button", { name: "取消暂不可用" });
    expect(cancel).toBeDisabled();
    expect(cancel).toHaveAttribute("title", "尚无可信的取消接口，本页不会发送取消请求。");
    fireEvent.click(cancel);
    fireEvent.click(screen.getByRole("button", { name: "刷新任务" }));
    await waitFor(() => expect(team.mock.calls.filter(([action]) => action === "plan-list")).toHaveLength(2));
    expect(team.mock.calls.every(([action]) => action === "plan-list" || action === "plan-read")).toBe(true);
    expect(screen.queryByText("进程已停止")).not.toBeInTheDocument();
  });

  it("reads an existing cancellation without exposing a cancellation dispatcher", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary("root-a", true)], total_count: 1, next_offset: null };
      return details("root-a", true);
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    const observed = await screen.findByRole("button", { name: "已请求取消（只读）" });
    expect(observed).toBeDisabled();
    fireEvent.click(observed);
    expect(screen.getByText(/已有取消记录不表示进程已停止/)).toBeInTheDocument();
    expect(team.mock.calls.every(([action]) => action === "plan-list" || action === "plan-read")).toBe(true);
  });

  it("does not treat a managed plan response as an available execution backend", async () => {
    const team = fixture();
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("已关联 Job");
    const capability = screen.getByRole("note", { name: "执行能力状态" });
    expect(within(capability).getByText("启动与取消控制尚未接入")).toBeInTheDocument();
    expect(within(capability).getByText(/读取到登记计划或既有执行记录，不代表已能启动新任务或发送取消请求/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /启动|开始执行|派发任务/ })).not.toBeInTheDocument();
    expect(team.mock.calls.every(([action]) => action === "plan-list" || action === "plan-read")).toBe(true);
  });

  it("discards old detail responses after another task is selected", async () => {
    const team = fixture();
    let finishOld!: (value: unknown) => void;
    team.mockImplementation(async (action, params) => {
      if (action === "plan-list") return { plans: [summary(), summary("root-b")], total_count: 2, next_offset: null };
      if (params.root_task_id === "root-a") return new Promise((resolve) => { finishOld = resolve; });
      return details("root-b");
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await waitFor(() => expect(team).toHaveBeenCalledWith("plan-read", { project, root_task_id: "root-a" }));
    fireEvent.click(screen.getByRole("button", { name: "查看任务 root-b" }));
    await screen.findByRole("heading", { name: "root-b" });
    await act(async () => finishOld(details()));
    expect(screen.getByRole("heading", { name: "root-b" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "root-a" })).not.toBeInTheDocument();
  });

  it("rejects records from another project and does not infer an empty recovery slot from missing data", async () => {
    const team = fixture();
    team.mockResolvedValue({ plans: [{ ...summary(), project_id: "other-project" }], total_count: 1, next_offset: null });
    const { unmount } = render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("团队计划的项目归属或返回字段无效。");
    expect(team.mock.calls.every(([action]) => action === "plan-list")).toBe(true);
    unmount();
    const incomplete = fixture();
    incomplete.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const { fallbacks: _fallbacks, ...rest } = details();
      return rest;
    });
    render(<TeamTaskPanel projectId={project} api={{ team: incomplete }} />);
    await screen.findByText("服务未提供恢复记录观测，当前无法确认恢复槽状态。");
    expect(screen.queryByLabelText("根任务恢复槽占用 0")).not.toBeInTheDocument();
  });

  it("loads the twenty-first Job and failure evidence while preserving the office selection", async () => {
    const team = fixture();
    team.mockImplementation(async (action, params) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      return pagedDetail(Number(params.attempts_offset ?? 0), 25);
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    fireEvent.click(await screen.findByRole("button", { name: /展开任务小人办公室/ }));
    fireEvent.click(screen.getByRole("button", { name: "查看" + nicknameForTask("child-b") + "的任务" }));
    expect(screen.queryByText("job-21")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "加载更多尝试" }));
    const attempt = (await screen.findByText("job-21")).closest("article")!;
    expect(within(attempt).getByText("执行超时")).toBeInTheDocument();
    expect(within(attempt).getByText("quota_exhausted")).toBeInTheDocument();
    expect(screen.getByText("已显示 25 / 25 条记录")).toBeInTheDocument();
    expect(team).toHaveBeenCalledWith("plan-read", { project, root_task_id: "root-a", attempts_offset: 20, attempts_limit: 20 });
    expect(screen.getByRole("button", { name: "查看" + nicknameForTask("child-b") + "的任务" })).toHaveAttribute("aria-pressed", "true");
  });

  it("refreshes the loaded depth and appended records without duplicates or state regression", async () => {
    const team = fixture();
    let total = 25;
    team.mockImplementation(async (action, params) => action === "plan-list" ? { plans: [summary()], total_count: 1, next_offset: null } : pagedDetail(Number(params.attempts_offset ?? 0), total));
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    fireEvent.click(await screen.findByRole("button", { name: "加载更多尝试" }));
    await screen.findByText("job-25");
    total = 30;
    fireEvent.click(screen.getByRole("button", { name: "刷新任务" }));
    await screen.findByText("job-30");
    expect(screen.getByText("updated_failure_evidence")).toBeInTheDocument();
    const region = screen.getByRole("region", { name: "CLI 执行尝试" });
    const ids = Array.from(region.querySelectorAll("[data-attempt-id]")).map((item) => item.getAttribute("data-attempt-id"));
    expect(ids).toEqual(Array.from({ length: 30 }, (_, index) => "attempt-" + (index + 1)));
    expect(screen.getByText("已显示 30 / 30 条记录")).toBeInTheDocument();
  });

  it("discards a late attempt page after switching the selected task", async () => {
    const team = fixture();
    let finishOldPage!: (value: unknown) => void;
    team.mockImplementation(async (action, params) => {
      if (action === "plan-list") return { plans: [summary(), summary("root-b")], total_count: 2, next_offset: null };
      if (params.root_task_id === "root-a" && params.attempts_offset === 20) return new Promise((resolve) => { finishOldPage = resolve; });
      return pagedDetail(Number(params.attempts_offset ?? 0), params.root_task_id === "root-b" ? 1 : 25, String(params.root_task_id));
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    fireEvent.click(await screen.findByRole("button", { name: "加载更多尝试" }));
    fireEvent.click(screen.getByRole("button", { name: "查看任务 root-b" }));
    await screen.findByText("root-b-job-1");
    await act(async () => finishOldPage(pagedDetail(20, 25)));
    expect(screen.queryByText("job-21")).not.toBeInTheDocument();
    expect(screen.getByText("已显示 1 / 1 条记录")).toBeInTheDocument();
  });

  it("discards a late attempt page after the project changes", async () => {
    const team = fixture();
    let finishOldPage!: (value: unknown) => void;
    team.mockImplementation(async (action, params) => {
      const other = params.project === "runtime/runner/other";
      if (action === "plan-list") return { plans: [{ ...summary(other ? "root-b" : "root-a"), project_id: String(params.project) }], total_count: 1, next_offset: null };
      if (!other && params.attempts_offset === 20) return new Promise((resolve) => { finishOldPage = resolve; });
      return pagedDetail(Number(params.attempts_offset ?? 0), other ? 1 : 25, other ? "root-b" : "root-a", String(params.project));
    });
    const { rerender } = render(<TeamTaskPanel key={project} projectId={project} api={{ team }} />);
    fireEvent.click(await screen.findByRole("button", { name: "加载更多尝试" }));
    rerender(<TeamTaskPanel key="other" projectId="runtime/runner/other" api={{ team }} />);
    await screen.findByText("root-b-job-1");
    await act(async () => finishOldPage(pagedDetail(20, 25)));
    expect(screen.queryByText("job-21")).not.toBeInTheDocument();
    expect(screen.getByText("root-b-job-1")).toBeInTheDocument();
  });

  it("uses completion evidence rather than planned slots or agent records for minimums", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => action === "plan-list" ? { plans: [summary()], total_count: 1, next_offset: null } : completionDetail());
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("不设置最低执行门槛");
    expect(screen.getByLabelText("计划子 Agent 2")).toBeInTheDocument();
    expect(screen.getByLabelText("已核实派发 0")).toBeInTheDocument();
    expect(screen.getByLabelText("计划 CLI 调用 4")).toBeInTheDocument();
    expect(screen.getByLabelText("已登记调用尝试 3")).toBeInTheDocument();
    expect(screen.getByLabelText("已核实 CLI 调用 0")).toBeInTheDocument();
    expect(screen.queryByText(/当前不能将该团队任务标记为成功/)).not.toBeInTheDocument();
    expect(screen.getByText(/为 0 表示尚无足够证据，不代表从未执行/)).toBeInTheDocument();
    const member = screen.getByRole("article", { name: "成员执行计数 child-a" });
    expect(within(member).getByText("派发证据待确认")).toBeInTheDocument();
    expect(within(member).getByText(/调用次数仅作记录/)).toBeInTheDocument();
  });

  it("does not impose a minimum in auto mode or substitute counts for absent completion data", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const value = completionDetail();
      value.completion.mode = "auto"; value.completion.success_gate = "not_required";
      value.completion.required_subagents = 0; value.completion.minimums_satisfied = true;
      value.completion.members = value.completion.members.map((member) => ({ ...member, required_cli_calls: 0, minimum_met: true }));
      return value;
    });
    const { unmount } = render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("不设置最低执行门槛");
    expect(screen.queryByText(/当前不能将该团队任务标记为成功/)).not.toBeInTheDocument();
    unmount();
    render(<TeamTaskPanel projectId={project} api={{ team: fixture() }} />);
    await screen.findByText(/Server 尚未提供实际执行计数与完成条件/);
    expect(screen.queryByLabelText("已核实派发 0")).not.toBeInTheDocument();
  });

  it("keeps satisfied numeric minimums separate from business completion", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const value = completionDetail();
      value.completion.success_gate = "satisfied"; value.completion.minimums_satisfied = true;
      value.completion.confirmed_subagents = 2; value.completion.confirmed_cli_calls = 3;
      value.completion.members = value.completion.members.map((member) => ({ ...member, confirmed_cli_calls: 1, minimum_met: true, dispatch_confirmed: member.role !== "manager" }));
      return value;
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("不设置最低执行门槛");
    expect(screen.getByText("子 Agent 和 CLI 均为可选，任务按实际成果验收。")).toBeInTheDocument();
  });

  it("does not call a truncated recovery list an empty slot", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      return { ...details(), fallbacks: [], fallbacks_may_be_truncated: true };
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByLabelText("根任务恢复槽占用 未核实");
    expect(screen.getByLabelText("恢复槽占用未核实，上限 1")).toBeInTheDocument();
    expect(screen.getByText(/恢复记录可能未完整返回/)).toBeInTheDocument();
  });

  it("renders actual assignment titles and ordinary host observations without promoting a candidate reply", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => action === "plan-list" ? { plans: [summary()], total_count: 1, next_offset: null } : peopleDetail());
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    expect(await screen.findByRole("heading", { name: "整理项目的工作记录" })).toBeInTheDocument();
    const card = screen.getByText(nicknameForTask("child-a") + " · 普通 GPT 伙伴").closest("article")!;
    expect(within(card).getByText("检查接口与恢复边界")).toBeInTheDocument();
    expect(within(card).getByText("discovered-balanced-profile")).toBeInTheDocument();
    expect(within(card).getByText("chat-request-a")).toBeInTheDocument();
    expect(within(card).getByText("普通伙伴已返回候选回复，业务结果尚未验收。")).toBeInTheDocument();
    expect(screen.queryByText("PRIVATE_INSTRUCTION_NEVER_RENDER")).not.toBeInTheDocument();
    expect(team.mock.calls.every(([action]) => action === "plan-list" || action === "plan-read")).toBe(true);
  });

  it("connects clickable real-task characters while leaving the manager and undispatched members idle", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => action === "plan-list" ? { plans: [summary()], total_count: 1, next_offset: null } : (() => { const value = peopleDetail(); return { ...value, task_summaries: value.task_summaries.map((task) => task.task_id === "child-a" ? { ...task, activity: { execution_state: "running", wait_reason: "", next_step: "", last_progress_at_unix_ms: Date.now() } } : task) }; })());
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    fireEvent.click(await screen.findByRole("button", { name: /展开任务小人办公室/ }));
    const manager = screen.getByRole("button", { name: "查看" + nicknameForTask("root-a") + "的任务" });
    const running = screen.getByRole("button", { name: "查看" + nicknameForTask("child-a") + "的任务" });
    const pending = screen.getByRole("button", { name: "查看" + nicknameForTask("child-b") + "的任务" });
    expect(manager).not.toHaveClass("is-working");
    expect(running).toHaveClass("is-working");
    expect(pending).not.toHaveClass("is-working");
    fireEvent.click(pending);
    const profile = screen.getByRole("region", { name: "选中伙伴的真实任务" });
    expect(within(profile).getByText("child-b")).toBeInTheDocument();
    expect(within(profile).getByText("准备验收清单")).toBeInTheDocument();
    expect(within(profile).getByText("尚未派发")).toBeInTheDocument();
    fireEvent.click(running);
    expect(within(profile).getByText("discovered-balanced-profile")).toBeInTheDocument();
    expect(within(profile).getByText("child-a")).toBeInTheDocument();
  });

  it("does not infer execution from a dispatched conversation and accepts CLI default records", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const value = peopleDetail();
      return { ...value, attempts: { ...value.attempts, attempts: value.attempts.attempts.map((item) => ({ ...item, model: null, effort: null })) },
        task_summaries: value.task_summaries.map((task) => task.task_id === "child-a" ? { ...task, activity: { execution_state: "waiting_dependencies", wait_reason: "等待公开协议", next_step: "核对后继续实现" } } : task) };
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("CLI 默认模型");
    fireEvent.click(await screen.findByRole("button", { name: /展开任务小人办公室/ }));
    const child = screen.getByRole("button", { name: "查看" + nicknameForTask("child-a") + "的任务" });
    expect(child).not.toHaveClass("is-working");
    fireEvent.click(child);
    const profile = screen.getByRole("region", { name: "选中伙伴的真实任务" });
    expect(within(profile).getByText("等待依赖")).toBeInTheDocument();
    expect(within(profile).getByText("等待公开协议")).toBeInTheDocument();
    expect(within(profile).getByText("核对后继续实现")).toBeInTheDocument();
  });
  it.each([undefined, null, "", "default"])("accepts a tool default profile %s without legacy quota fields", async (profile) => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const value = peopleDetail();
      return { ...value, agents: value.agents.map(agent => ({ ...agent, thinking_profile_id: profile, counts_toward_regular_min: undefined, counts_toward_regular_max: undefined })) };
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    expect(await screen.findByText("工具默认设置")).toBeVisible();
    expect(screen.queryByText("普通伙伴的项目或任务关联无效。")).not.toBeInTheDocument();
  });

  it("keeps missing agent/title fields explicitly unobserved", async () => {
    const team = fixture();
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("Server 尚未提供普通伙伴派发记录，不能确认成员是否已启动。");
    expect(screen.getByText(/Server 尚未提供任务标题与业务状态观测/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /展开任务小人办公室/ }));
    const person = screen.getByRole("button", { name: "查看" + nicknameForTask("child-a") + "的任务" });
    expect(person).not.toHaveClass("is-working");
    fireEvent.click(person);
    expect(within(screen.getByRole("region", { name: "选中伙伴的真实任务" })).getByText("派发观测未提供")).toBeInTheDocument();
  });

  it.each(["project", "root_task_id", "source_task_id"])("rejects an ordinary agent with an unrelated %s", async (field) => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const detail = peopleDetail();
      return { ...detail, agents: [{ ...detail.agents[0], [field]: "unrelated" }] };
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("普通伙伴的项目或任务关联无效。");
    expect(screen.queryByText("discovered-balanced-profile")).not.toBeInTheDocument();
  });

  it("does not confuse a host terminal observation with an ordinary task's business verification", async () => {
    const team = fixture();
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      const detail = peopleDetail();
      detail.agents[0].state = "verified_terminal";
      return detail;
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    await screen.findByText("宿主执行终态已核实，不能据此认定业务任务成功。");
    fireEvent.click(screen.getByRole("button", { name: /展开任务小人办公室/ }));
    expect(screen.getByRole("button", { name: "查看" + nicknameForTask("child-a") + "的任务" })).not.toHaveClass("is-working");
  });

  it("preserves the expanded office and selected person while refreshing the same task", async () => {
    const team = fixture();
    let reads = 0;
    let finishRead!: (value: unknown) => void;
    team.mockImplementation(async (action) => {
      if (action === "plan-list") return { plans: [summary()], total_count: 1, next_offset: null };
      if (++reads === 1) return peopleDetail();
      return new Promise((resolve) => { finishRead = resolve; });
    });
    render(<TeamTaskPanel projectId={project} api={{ team }} />);
    fireEvent.click(await screen.findByRole("button", { name: /展开任务小人办公室/ }));
    fireEvent.click(screen.getByRole("button", { name: "查看" + nicknameForTask("child-b") + "的任务" }));
    fireEvent.click(screen.getByRole("button", { name: "刷新任务" }));
    expect(screen.getByRole("button", { name: /收起任务小人办公室/ })).toHaveAttribute("aria-expanded", "true");
    await act(async () => finishRead(peopleDetail()));
    expect(screen.getByRole("button", { name: "查看" + nicknameForTask("child-b") + "的任务" })).toHaveAttribute("aria-pressed", "true");
  });
});
