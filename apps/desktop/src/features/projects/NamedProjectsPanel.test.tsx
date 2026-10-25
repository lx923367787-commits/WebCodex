import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { NamedProjectsPanel } from "./NamedProjectsPanel";
import type { DesktopState, RegisteredProject } from "../../models/topology";
vi.mock("./ProjectsPanel", () => ({ ProjectsPanel: () => <div>Folder registry</div> }));
vi.mock("./ProjectTeamSettings", () => ({ ProjectTeamSettings: ({ project, onBack }: { project: { id: string; name: string; path: string }; onBack: () => void }) => <div><h1>{project.name} · 设置</h1><output aria-label="团队设置目标">{project.id}</output><p>{project.path}</p><button onClick={onBack}>返回命名项目</button></div> }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
const folders = ["code", "docs"].map(name => ({ id: `agent:runner:${name}`, name, path: `D:/${name}`, enabled: true, revision: "1", connected: true, agent_status: "online", active_jobs: 0 })) as RegisteredProject[];
const state = { topology: { experience: "full", server: { kind: "local" } }, readiness: { server: "ready", runner: "ready" }, current_operation: null } as DesktopState;
const api = { listNamedProjects: vi.fn(), saveNamedProjects: vi.fn(), listProjects: vi.fn(), inspectProject: vi.fn(), addProject: vi.fn() };
beforeEach(() => { vi.resetAllMocks(); api.listNamedProjects.mockResolvedValue({ host_id: "host", revision: 4, projects: [] }); api.listProjects.mockResolvedValue({ projects: folders, can_manage: true }); });
function show() { render(<NamedProjectsPanel state={state} onState={vi.fn()} onChooseProject={vi.fn()} api={api} />); }
it("saves one named project with two exact folder grants and observed revision", async () => {
  show(); await waitFor(() => expect(screen.getByText("新建命名项目")).toBeEnabled());
  fireEvent.click(screen.getByText("新建命名项目"));
  fireEvent.change(screen.getByLabelText("项目名称"), { target: { value: "EAS" } });
  expect(screen.getByText("保存并授权")).toBeDisabled();
  fireEvent.click(screen.getByLabelText(/code —/)); fireEvent.click(screen.getByLabelText(/docs —/));
  api.saveNamedProjects.mockImplementation(async (_rev, projects) => ({ revision: 5, projects: projects.map((p: object) => ({ ...p, folders: [] })) }));
  fireEvent.click(screen.getByText("保存并授权"));
  await waitFor(() => expect(api.saveNamedProjects).toHaveBeenCalledWith(4, [expect.objectContaining({ name: "EAS", folders: folders.map(f => f.id) })], "host", Object.fromEntries(folders.map(f => [f.id, f.revision]))));
  expect(api.addProject).not.toHaveBeenCalled();
});
it("does not save or grant folders when editing is canceled", async () => {
  show(); await waitFor(() => expect(screen.getByText("新建命名项目")).toBeEnabled());
  fireEvent.click(screen.getByText("新建命名项目")); fireEvent.click(screen.getByLabelText(/docs —/)); fireEvent.click(screen.getByText("取消"));
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
});
it("preserves a rejected revision and requires observation instead of automatic retry", async () => {
  api.listNamedProjects.mockResolvedValue({ revision: 4, projects: [{ id: "wc_workspace_" + "a".repeat(32), name: "EAS", enabled: true, folders: folders.map(f => ({ project: f.id, name: f.name, path: f.path })) }] });
  api.saveNamedProjects.mockRejectedValue({ code: "workspace_revision_conflict" });
  show(); fireEvent.click(await screen.findByText("编辑名称与文件夹"));
  fireEvent.click(screen.getByText("保存并授权"));
  expect(await screen.findByRole("alert")).toHaveTextContent("workspace_revision_conflict");
  expect(api.saveNamedProjects).toHaveBeenCalledTimes(1);
  expect(screen.getByLabelText("项目名称")).toHaveValue("EAS");
});
it("opens the existing folder policy from its named project without rewriting saved settings", async () => {
  api.listNamedProjects.mockResolvedValue({ host_id: "host", revision: 4, projects: [{ id: "wc_workspace_" + "a".repeat(32), name: "测文件", enabled: true, folders: [{ project: folders[0].id, name: folders[0].name, path: folders[0].path }] }] });
  show();
  fireEvent.click(await screen.findByText("团队设置"));
  expect(screen.getByRole("heading", { name: "测文件 · 设置" })).toBeInTheDocument();
  expect(screen.getByLabelText("团队设置目标")).toHaveTextContent(folders[0].id);
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
  fireEvent.click(screen.getByText("返回命名项目"));
  expect(screen.getByRole("heading", { name: "测文件" })).toBeInTheDocument();
});
it("requires an explicit folder selection for a named project with multiple policies", async () => {
  api.listNamedProjects.mockResolvedValue({ host_id: "host", revision: 4, projects: [{ id: "wc_workspace_" + "a".repeat(32), name: "EAS", enabled: true, folders: folders.map(f => ({ project: f.id, name: f.name, path: f.path })) }] });
  show();
  fireEvent.click(await screen.findByText("团队设置"));
  expect(screen.queryByLabelText("团队设置目标")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "docs — D:/docs" }));
  expect(screen.getByLabelText("团队设置目标")).toHaveTextContent(folders[1].id);
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
});
