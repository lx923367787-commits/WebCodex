import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { AiOfficeApi, OfficeSnapshot } from "../ai-office/types";
import { ProjectTeamSettings } from "./ProjectTeamSettings";

const project = { id: "project-beta", name: "Beta", path: "C:/work/Beta" };
const snapshot: OfficeSnapshot = {
  source: "runtime", observed_at_ms: 1, selected_project_id: "other-project", members: [], enforcement: "demo_plan_only", limitations: [],
  projects: [{ ...project, connected: true, active_jobs: 0, policy: { revision: 3, mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 }, providers: [] } }],
};

describe("Project collaboration tools", () => {
  function apiFixture(): AiOfficeApi { return { snapshot: vi.fn().mockResolvedValue(snapshot), savePolicy: vi.fn(), discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn() }; }
  it("loads the exact project without requiring a saved policy", async () => {
    const api = apiFixture(); api.snapshot = vi.fn().mockResolvedValue({ ...snapshot, projects: snapshot.projects.map((p) => ({ ...p, policy: { ...p.policy, revision: 0 } })) });
    render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    expect(await screen.findByText("按任务需要自由协作")).toBeVisible();
    expect(api.snapshot).toHaveBeenCalledWith(project.id);
    expect(api.savePolicy).not.toHaveBeenCalled();
    expect(screen.queryByText(/请先保存/)).not.toBeInTheDocument();
  });
  it("retains the folder and supports retry and back navigation", async () => {
    const api = apiFixture(); api.snapshot = vi.fn().mockRejectedValueOnce(new Error("连接暂时不可用")).mockResolvedValue(snapshot);
    const onBack = vi.fn();
    render(<ProjectTeamSettings project={project} api={api} onBack={onBack} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("连接暂时不可用");
    expect(screen.getByText(project.path)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "重试载入" }));
    expect(await screen.findByText("按任务需要自由协作")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /返回项目列表/ }));
    expect(onBack).toHaveBeenCalledOnce();
  });
  it("does not substitute another project's data", async () => {
    const api = apiFixture(); api.snapshot = vi.fn().mockResolvedValue({ ...snapshot, projects: [] });
    render(<ProjectTeamSettings project={project} api={api} onBack={vi.fn()} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("登记信息已变化");
    expect(screen.queryByText("按任务需要自由协作")).not.toBeInTheDocument();
  });
});
