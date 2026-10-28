import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AiOffice } from "./AiOffice";
import { OfficePolicyPanel, officeError } from "./OfficePolicyPanel";
import type { AiOfficeApi, OfficePolicy, OfficeSnapshot } from "./types";

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
    discoverCli: vi.fn(async () => [{ providerId: "workbuddy", label: "WorkBuddy", installed: true, native: true, adapterAvailable: true, status: "discovered", message: "已发现" }, { providerId: "antigravity", label: "Antigravity", installed: true, native: true, adapterAvailable: true, status: "discovered", message: "已发现" }]),
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
describe("optional collaboration", () => {
  it("removes quotas and pinned-model controls even for an old custom policy", async () => {
    const api = apiFixture();
    render(<OfficePolicyPanel project={{ ...snapshot.projects[0], policy: { ...policy, mode: "custom", subagents: { min: 2, max: 4 } } }} api={api} onSaved={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "刷新 WorkBuddy 模型列表" })).toBeVisible();
    expect(screen.getByText(/不强制调用，不设置人数/)).toBeVisible();
    expect(screen.getByText(/协作采用软约定：各 Agent 和 CLI 各自维护记忆、执行目标和验收标准/)).toBeVisible();
    expect(screen.queryByText(/仅隔离各自/)).not.toBeInTheDocument();
    expect(screen.queryByRole("spinbutton")).not.toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "保存团队策略" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "配置本机 CLI 执行" })).not.toBeInTheDocument();
    expect(api.discoverCli).toHaveBeenCalledWith("active");
    expect(api.savePolicy).not.toHaveBeenCalled();
  });
  it("shows discovered capability without claiming account or execution readiness", async () => {
    const api = apiFixture();
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "刷新 WorkBuddy 模型列表" }));
    expect(await screen.findByText("模型：discovered-model")).toBeVisible();
    expect(screen.getByText("思考选项：high")).toBeVisible();
    expect(screen.getByText("账号可用性未核验")).toBeVisible();
    expect(screen.getAllByText("加载状态未观测")).toHaveLength(2);
    expect(api.refreshCli).toHaveBeenCalledWith("workbuddy", "active");
    expect(api.evaluatePlan).not.toHaveBeenCalled();
  });
  it("distinguishes Runner loading from actual execution and clears obsolete policy counts", async () => {
    const api = apiFixture();
    api.discoverCli = vi.fn(async () => [{ providerId: "custom", label: "Custom CLI", installed: true, native: false, adapterAvailable: true, executionReady: true, status: "discovered", message: "Runner inventory" }]);
    api.snapshot = vi.fn(async () => ({ ...snapshot, projects: snapshot.projects.map(project => ({ ...project, policy: { ...policy, mode: "custom" as const, subagents: { min: 2, max: 4 } } })) }));
    render(<AiOffice api={api} />);
    await screen.findByText("按任务需要自由协作");
    expect(screen.queryByText(/2–4 位子 Agent/)).not.toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: /团队策略/ })[0]);
    expect(await screen.findByText("Runner 已加载")).toBeVisible();
    expect(screen.getByText(/目录不证明执行成功/)).toBeVisible();
    expect(screen.getByRole("button", { name: "刷新 Custom CLI 模型列表" })).toBeEnabled();
  });
  it("does not retain an old capability response after rescan", async () => {
    const api = apiFixture();
    let finish!: (value: Awaited<ReturnType<AiOfficeApi["refreshCli"]>>) => void;
    api.refreshCli = vi.fn<AiOfficeApi["refreshCli"]>(() => new Promise((resolve) => { finish = resolve; }));
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "刷新 WorkBuddy 模型列表" }));
    fireEvent.click(screen.getByRole("button", { name: "重新扫描本机 CLI" }));
    await waitFor(() => expect(api.discoverCli).toHaveBeenCalledTimes(2));
    const result = await apiFixture().refreshCli("workbuddy");
    await act(async () => finish(result));
    expect(screen.queryByText("模型：discovered-model")).not.toBeInTheDocument();
  });
  it("allows independent work when no CLI is installed and formats errors safely", async () => {
    const api = apiFixture(); api.discoverCli = vi.fn().mockResolvedValue([]);
    render(<OfficePolicyPanel project={snapshot.projects[0]} api={api} onSaved={vi.fn()} />);
    expect(await screen.findByText(/主 Agent 仍可独立完成任务/)).toBeVisible();
    expect(officeError({ message: "Failed", next_action: "Retry", secret: "hidden" })).toBe("Failed Retry");
    expect(officeError({ secret: "hidden" })).not.toContain("hidden");
  });
});
