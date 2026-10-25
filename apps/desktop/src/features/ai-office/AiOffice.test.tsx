import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AiOffice } from "./AiOffice";
import { OfficePolicyPanel, officeError } from "./OfficePolicyPanel";
import type { AiOfficeApi, CliCatalog, OfficePolicy, OfficeSnapshot } from "./types";

const policy: OfficePolicy = {
  revision: 7, mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 },
  providers: [{ provider_id: "workbuddy", enabled: true, model: null, effort: null }, { provider_id: "antigravity", enabled: false, model: null, effort: null }],
};
const snapshot: OfficeSnapshot = {
  source: "runtime", observed_at_ms: 1_760_000_000_000, selected_project_id: "active", enforcement: "demo_plan_only", limitations: ["仅展示有证据的关系。"],
  projects: [
    { id: "active", name: "活跃项目", path: "E:\\projects\\active", connected: true, active_jobs: 1, policy },
    { id: "idle", name: "闲置项目", path: "E:\\projects\\idle", connected: true, active_jobs: 0, policy },
  ],
  members: [{ id: "worker", label: "执行员", role: "observed_agent", state: "working", active_requests: 1, last_activity_at_ms: 1_760_000_000_000, task: "检查项目的接口实现", parent_id: null, progress: null }],
};
function apiFixture(): AiOfficeApi {
  return {
    snapshot: vi.fn<AiOfficeApi["snapshot"]>(async (id?: string) => ({ ...snapshot, selected_project_id: id ?? "active", members: id === "idle" ? [] : snapshot.members })),
    savePolicy: vi.fn<AiOfficeApi["savePolicy"]>(async (_id, revision, input) => ({ ...input, revision: revision + 1 })),
    refreshCli: vi.fn<AiOfficeApi["refreshCli"]>(async (id) => ({ providerId: id, label: "WorkBuddy", installed: true, version: "test-version", models: ["discovered-model"], efforts: ["high"], source: "installed_help", status: "ready", message: "从本机 CLI 帮助发现。", checkedAt: "2026-10-23T00:00:00Z", accountAvailabilityVerified: false })),
    evaluatePlan: vi.fn<AiOfficeApi["evaluatePlan"]>(async (_id, revision, plan) => ({ admitted: true, policy_revision: revision, enforcement: "demo_plan_only", subagent_count: plan.subagents.length, cli_call_count: plan.manager.cli_calls.length + plan.subagents.reduce((sum, agent) => sum + agent.cli_calls.length, 0), violations: [] })),
  };
}

describe("AI office", () => {
  it("opens real team observations using the selected project identity", async () => {
    const api = apiFixture();
    api.team = vi.fn(async () => ({ plans: [], total_count: 0, next_offset: null }));
    render(<AiOffice api={api} />);
    await screen.findByRole("button", { name: "查看执行员的任务" });
    fireEvent.click(screen.getByRole("button", { name: "运行任务" }));
    await screen.findByText("当前页没有已登记计划");
    expect(api.team).toHaveBeenCalledWith("plan-list", { project: "active", offset: 0, limit: 20 });
    expect(screen.getByRole("button", { name: "窗口活动" })).toBeInTheDocument();
  });

  it("shows observed members without inventing reporting relationships or progress", async () => {
    const api = apiFixture();
    render(<AiOffice api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "查看执行员的任务" }));
    const detail = screen.getByRole("heading", { name: "执行员" }).closest("section")!;
    expect(within(detail).getByText("未提供关联证据")).toBeInTheDocument();
    expect(within(detail).getByText("任务进度")).toBeInTheDocument();
    expect(within(detail).getAllByText("未提供")).toHaveLength(2);
    expect(within(detail).queryByText(/\d+%/)).not.toBeInTheDocument();
    expect(screen.queryByText("DEMO · 模拟演示")).not.toBeInTheDocument();
    expect(api.refreshCli).not.toHaveBeenCalled();
    expect(api.evaluatePlan).not.toHaveBeenCalled();
  });

  it("keeps idle projects configurable and sends their identity plus revision on save", async () => {
    const api = apiFixture();
    render(<AiOffice api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: /闲置项目/ }));
    await screen.findByText("这里安静，随时准备开工");
    fireEvent.click(screen.getByRole("button", { name: "团队策略" }));
    fireEvent.click(screen.getByRole("button", { name: /我来设定边界/ }));
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最少数量"), { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await waitFor(() => expect(api.savePolicy).toHaveBeenCalledWith("idle", 7, expect.objectContaining({ mode: "custom", subagents: { min: 1, max: 4 } })));
    expect(await screen.findByText("已保存到「闲置项目」，策略版本 8")).toBeInTheDocument();
  });

  it("labels every demo as simulated", async () => {
    const api = apiFixture();
    api.snapshot = vi.fn<AiOfficeApi["snapshot"]>(async () => ({ ...snapshot, source: "demo" }));
    render(<AiOffice api={api} />);
    expect(await screen.findByText("DEMO · 模拟演示")).toBeInTheDocument();
    expect(screen.getByText(/小人、任务与进度均为模拟数据/)).toBeInTheDocument();
  });

  it("ignores older project responses when the user switches projects quickly", async () => {
    const api = apiFixture();
    let finishIdle!: (value: OfficeSnapshot) => void;
    vi.mocked(api.snapshot).mockImplementation(async (id) => id === "idle" ? new Promise((resolve) => { finishIdle = resolve; }) : { ...snapshot });
    render(<AiOffice api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: /闲置项目/ }));
    fireEvent.click(screen.getByRole("button", { name: /活跃项目/ }));
    await waitFor(() => expect(api.snapshot).toHaveBeenCalledTimes(3));
    await act(async () => finishIdle({ ...snapshot, selected_project_id: "idle", members: [] }));
    expect(screen.getByRole("button", { name: /活跃项目/ })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "查看执行员的任务" })).toBeInTheDocument();
  });
});

describe("office policy", () => {
  it("rejects impossible minimum CLI budgets but keeps feasible upper ranges", async () => {
    const api = apiFixture();
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: /我来设定边界/ }));
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最多数量"), { target: { value: "32" } });
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最少数量"), { target: { value: "16" } });
    fireEvent.change(screen.getByLabelText("每个 Agent 的 CLI 调用最少数量"), { target: { value: "2" } });
    expect(screen.getByRole("alert")).toHaveTextContent("最低分工需要 34 次 CLI");
    expect(screen.getByRole("button", { name: "保存团队策略" })).toBeDisabled();
    expect(api.savePolicy).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最少数量"), { target: { value: "15" } });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await waitFor(() => expect(api.savePolicy).toHaveBeenCalledWith("active", 7, expect.objectContaining({ subagents: { min: 15, max: 32 }, cli_per_agent: { min: 2, max: 2 } })));
  });

  it("allows the default policy to be explicitly saved before its first use", async () => {
    const api = apiFixture();
    render(<OfficePolicyPanel project={{ ...snapshot.projects[0], policy: { ...policy, revision: 0 } }} api={api} onSaved={vi.fn()} />);
    expect(screen.getByRole("button", { name: "保存团队策略" })).toBeEnabled();
    expect(screen.getByRole("button", { name: /验证委派计划/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await waitFor(() => expect(api.savePolicy).toHaveBeenCalledWith("active", 0, expect.objectContaining({ mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 } })));
  });

  it("shows native read-only guidance without retrying or confirming an unknown save", async () => {
    const api = apiFixture();
    const nextAction = "只读重新查询原项目策略以核实结果；保存可能已提交，不要自动重发或更新版本号后重存旧设置。";
    vi.mocked(api.savePolicy).mockRejectedValue({ code: "webcodex_contract_invalid", message: "Server 团队策略回执未能核实。", next_action: nextAction, details: { diagnostic: "PRIVATE_DETAILS" } });
    const onSaved = vi.fn();
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={onSaved} />);
    fireEvent.click(screen.getByRole("button", { name: /我来设定边界/ }));
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最多数量"), { target: { value: "6" } });
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("请先点击「载入最新策略」核查。");
    expect(alert).toHaveTextContent(nextAction);
    expect(alert).not.toHaveTextContent("PRIVATE_DETAILS");
    expect(api.savePolicy).toHaveBeenCalledExactlyOnceWith("active", 7, expect.objectContaining({ subagents: { min: 0, max: 6 } }));
    expect(api.snapshot).not.toHaveBeenCalled();
    expect(onSaved).not.toHaveBeenCalled();
    expect(screen.getByText("当前版本 7 · 活跃项目")).toBeInTheDocument();
    expect(screen.getByLabelText("总经理可委派的子 Agent最多数量")).toHaveValue(6);
    expect(screen.queryByText(/已保存到/)).not.toBeInTheDocument();

    vi.mocked(api.snapshot).mockResolvedValue({ ...snapshot, projects: [{ ...snapshot.projects[0], policy: { ...policy, revision: 8 } }] });
    fireEvent.click(within(alert).getByRole("button", { name: "载入最新策略（替换草稿）" }));
    await screen.findByText("已载入最新策略，本地草稿已替换。");
    expect(api.snapshot).toHaveBeenCalledExactlyOnceWith("active");
    expect(api.savePolicy).toHaveBeenCalledTimes(1);
    expect(onSaved).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ revision: 8 }));
    expect(screen.queryByText(/已保存到/)).not.toBeInTheDocument();
  });

  it("only formats bounded textual next actions and never serializes error details", () => {
    expect(officeError({ message: "未能核实", next_action: "  只读核查  ", details: { token: "PRIVATE_DETAILS" } })).toBe("未能核实 只读核查");
    for (const next_action of [null, {}, [], 42, " ", "x".repeat(2049)]) {
      expect(officeError({ message: "未能核实", next_action, details: { token: "PRIVATE_DETAILS" } }, "载入最新策略：")).toBe("未能核实");
    }
    expect(officeError(new Error("原有错误"))).toBe("原有错误");
    expect(officeError("原有文本错误")).toBe("原有文本错误");
  });

  it("restores valid defaults when switching from invalid custom ranges to autonomous mode", async () => {
    const api = apiFixture();
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: /我来设定边界/ }));
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最少数量"), { target: { value: "5" } });
    expect(screen.getByText("最少数量不能大于最多数量。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存团队策略" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /总经理自主安排 默认/ }));
    expect(screen.getByLabelText("总经理可委派的子 Agent最少数量")).toHaveValue(0);
    expect(screen.getByLabelText("总经理可委派的子 Agent最多数量")).toHaveValue(4);
    expect(screen.getByRole("button", { name: "保存团队策略" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await waitFor(() => expect(api.savePolicy).toHaveBeenCalledWith("active", 7, expect.objectContaining({ mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 } })));
  });

  it("preserves dirty values after a revision conflict and only replaces them on explicit reload", async () => {
    const api = apiFixture();
    api.savePolicy = vi.fn(async () => { throw new Error("策略版本冲突：请载入最新版本。"); });
    const onSaved = vi.fn();
    const { rerender } = render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={onSaved} />);
    fireEvent.click(screen.getByRole("button", { name: /我来设定边界/ }));
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最多数量"), { target: { value: "6" } });
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await screen.findByText("策略版本冲突：请载入最新版本。");
    rerender(<OfficePolicyPanel project={{ ...snapshot.projects[0], policy: { ...policy, revision: 8 } }} api={api} onSaved={onSaved} />);
    expect(screen.getByLabelText("总经理可委派的子 Agent最多数量")).toHaveValue(6);
    expect(onSaved).not.toHaveBeenCalled();
    api.snapshot = vi.fn(async () => ({ ...snapshot, projects: [{ ...snapshot.projects[0], policy: { ...policy, revision: 8 } }] }));
    fireEvent.click(screen.getByRole("button", { name: "载入最新策略（替换草稿）" }));
    await screen.findByText("已载入最新策略，本地草稿已替换。");
    expect(screen.getByLabelText("总经理可委派的子 Agent最多数量")).toHaveValue(4);
    expect(onSaved).toHaveBeenCalledWith(expect.objectContaining({ revision: 8 }));
  });

  it("shows CLI discovery loading, preserves actual model/effort values, and qualifies account availability", async () => {
    const api = apiFixture();
    let finish!: (catalog: CliCatalog) => void;
    api.refreshCli = vi.fn(() => new Promise<CliCatalog>((resolve) => { finish = resolve; }));
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: /我来设定边界/ }));
    fireEvent.click(screen.getByRole("button", { name: "刷新 WorkBuddy 模型列表" }));
    expect(screen.getByRole("button", { name: "刷新 WorkBuddy 模型列表" })).toBeDisabled();
    expect(screen.getByText("发现中…")).toBeInTheDocument();
    await act(async () => finish({ providerId: "workbuddy", label: "WorkBuddy", installed: true, version: "1.2", models: ["actual-model"], efforts: ["actual-effort"], source: "installed_help", status: "ready", message: "已读取本机帮助", checkedAt: "now", accountAvailabilityVerified: false }));
    fireEvent.change(screen.getByLabelText("WorkBuddy 模型"), { target: { value: "actual-model" } });
    fireEvent.change(screen.getByLabelText("WorkBuddy 思考强度"), { target: { value: "actual-effort" } });
    expect(screen.getByText(/账号可用性未核验/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await waitFor(() => expect(api.savePolicy).toHaveBeenCalledWith("active", 7, expect.objectContaining({ providers: expect.arrayContaining([expect.objectContaining({ provider_id: "workbuddy", model: "actual-model", effort: "actual-effort" })]) })));
  });

  it("does not invent model options when discovery fails", async () => {
    const api = apiFixture();
    api.refreshCli = vi.fn(async () => { throw new Error("未发现可用 CLI 程序"); });
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "刷新 Antigravity 模型列表" }));
    await screen.findByText("未发现可用 CLI 程序");
    expect(within(screen.getByLabelText("Antigravity 模型")).getAllByRole("option")).toHaveLength(1);
  });

  it("validates an explicit plan against the saved revision without launching work", async () => {
    const api = apiFixture();
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("计划子 Agent 数量"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: /验证委派计划/ }));
    await screen.findByText("✓ 计划符合已保存策略");
    expect(api.evaluatePlan).toHaveBeenCalledWith("active", 7, expect.objectContaining({ manager: expect.objectContaining({ cli_calls: [] }), subagents: expect.arrayContaining([expect.objectContaining({ id: "agent-3", cli_calls: [] })]) }));
    expect(screen.getByText("仅验证，没有启动 Agent 或 CLI 进程。")).toBeInTheDocument();
    expect(api.savePolicy).not.toHaveBeenCalled();
    expect(api.refreshCli).not.toHaveBeenCalled();
  });
});
