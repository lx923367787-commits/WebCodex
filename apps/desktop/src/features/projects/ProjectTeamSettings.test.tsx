import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { AiOfficeApi, OfficeSnapshot } from "../ai-office/types";
import { ProjectTeamSettings } from "./ProjectTeamSettings";

const project = { id: "project-beta", name: "Beta", path: "C:/work/Beta" };
const snapshot: OfficeSnapshot = {
  source: "runtime", observed_at_ms: 1, selected_project_id: "other-project", members: [], enforcement: "demo_plan_only", limitations: [],
  projects: [{ ...project, connected: true, active_jobs: 0, policy: { revision: 3, mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 }, providers: [] } }],
};

describe("Project team settings loading", () => {
  it("identifies cached policy as unconfirmed and retains server limitations", async () => {
    const api: AiOfficeApi = { snapshot: vi.fn().mockResolvedValue({ ...snapshot, limitations: ["Server 策略读取失败，显示旧缓存。"] }), savePolicy: vi.fn(), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn() };
    render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    expect(await screen.findByText("当前策略尚未获 Server 确认")).toBeInTheDocument();
    expect(screen.getByText("Server 策略读取失败，显示旧缓存。")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /我来设定边界/ })).toBeEnabled();
    expect(screen.queryByText(/Server 已确认当前项目策略/)).not.toBeInTheDocument();
  });

  it("reobserves the exact project after saving and confirms only a fresh managed revision", async () => {
    let finishRead!: (value: OfficeSnapshot) => void;
    const api: AiOfficeApi = {
      snapshot: vi.fn<AiOfficeApi["snapshot"]>().mockResolvedValueOnce(snapshot).mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; })),
      savePolicy: vi.fn<AiOfficeApi["savePolicy"]>(async (_id, revision, policy) => ({ ...policy, revision: revision + 1 })), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn(),
    };
    render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: /我来设定边界/ }));
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await screen.findByText("正在重新观测 Server，尚未确认策略生效…");
    expect(screen.queryByText(/Server 已确认当前项目策略/)).not.toBeInTheDocument();
    await act(async () => finishRead({ ...snapshot, enforcement: "managed_tasks", projects: [{ ...snapshot.projects[0], policy: { ...snapshot.projects[0].policy, mode: "custom", revision: 4 } }] }));
    expect(await screen.findByText(/Server 已确认当前项目策略 · 版本 4/)).toBeInTheDocument();
    expect(api.snapshot).toHaveBeenNthCalledWith(2, project.id);
    expect(api.savePolicy).toHaveBeenCalledWith(project.id, 3, expect.objectContaining({ mode: "custom" }));
  });

  it("does not overwrite a saved revision with an older cached observation", async () => {
    const api: AiOfficeApi = {
      snapshot: vi.fn().mockResolvedValue(snapshot),
      savePolicy: vi.fn<AiOfficeApi["savePolicy"]>(async (_id, revision, policy) => ({ ...policy, revision: revision + 1 })), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn(),
    };
    render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: /我来设定边界/ }));
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await screen.findByText(/最新读数尚未包含刚保存的策略版本/);
    expect(screen.getByText("当前版本 4 · Beta")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /我来设定边界/ })).toHaveAttribute("aria-pressed", "true");
    expect(screen.queryByText(/Server 已确认当前项目策略/)).not.toBeInTheDocument();
  });

  it("keeps demonstration policies explicitly separate from server enforcement", async () => {
    const api: AiOfficeApi = { snapshot: vi.fn().mockResolvedValue({ ...snapshot, source: "demo" }), savePolicy: vi.fn(), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn() };
    render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    expect(await screen.findByText("演示策略 · 不约束真实任务")).toBeInTheDocument();
    expect(screen.queryByText(/Server 已确认当前项目策略/)).not.toBeInTheDocument();
  });

  it("ignores an old folder's save completion after the selected folder changes", async () => {
    let finishSave!: (value: OfficeSnapshot["projects"][number]["policy"]) => void;
    const nextProject = { id: "project-gamma", name: "Gamma", path: "C:/work/Gamma" };
    const api: AiOfficeApi = {
      snapshot: vi.fn<AiOfficeApi["snapshot"]>(async (id) => id === project.id ? snapshot : { ...snapshot, enforcement: "managed_tasks", projects: [{ ...snapshot.projects[0], ...nextProject, policy: { ...snapshot.projects[0].policy, revision: 9 } }] }),
      savePolicy: vi.fn<AiOfficeApi["savePolicy"]>(() => new Promise((resolve) => { finishSave = resolve; })), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn(),
    };
    const { rerender } = render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: /我来设定边界/ }));
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await waitFor(() => expect(api.savePolicy).toHaveBeenCalledTimes(1));
    rerender(<ProjectTeamSettings project={nextProject} api={api} onBack={vi.fn()} />);
    await screen.findByText("当前版本 9 · Gamma");
    await act(async () => finishSave({ ...snapshot.projects[0].policy, mode: "custom", revision: 4 }));
    expect(screen.getByText("当前版本 9 · Gamma")).toBeInTheDocument();
    expect(api.snapshot).toHaveBeenCalledTimes(2);
  });

  it("keeps the folder context and allows retry after a load failure", async () => {
    const api: AiOfficeApi = { snapshot: vi.fn().mockRejectedValueOnce(new Error("连接暂时不可用")).mockResolvedValue(snapshot), savePolicy: vi.fn(), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn() };
    const onBack = vi.fn();
    render(<ProjectTeamSettings project={project} api={api} onBack={onBack} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("连接暂时不可用");
    expect(screen.getByText(project.path)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "重试载入" }));
    expect(await screen.findByRole("button", { name: /总经理自主安排/ })).toBeVisible();
    expect(api.snapshot).toHaveBeenNthCalledWith(2, project.id);
    fireEvent.click(screen.getByRole("button", { name: /返回项目列表/ }));
    expect(onBack).toHaveBeenCalledOnce();
  });

  it("does not substitute another project's policy when the requested id disappeared", async () => {
    const api: AiOfficeApi = { snapshot: vi.fn().mockResolvedValue({ ...snapshot, projects: [{ ...snapshot.projects[0], id: "other-project" }] }), savePolicy: vi.fn(), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn() };
    render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("登记信息已变化");
    expect(screen.queryByRole("button", { name: "保存团队策略" })).not.toBeInTheDocument();
    expect(api.savePolicy).not.toHaveBeenCalled();
  });
});
