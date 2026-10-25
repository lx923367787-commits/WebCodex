import { StrictMode } from "react";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { open } from "@tauri-apps/plugin-dialog";
import { NamedProjectsPanel, type NamedProject, type WorkspaceCatalog, type WorkspaceDraft } from "./NamedProjectsPanel";
import type { DesktopState, RegisteredProject } from "../../models/topology";

vi.mock("./ProjectTeamSettings", () => ({ ProjectTeamSettings: ({ project, onBack }: { project: { id: string; name: string; path: string }; onBack: () => void }) => <div><h1>{project.name} · 设置</h1><output aria-label="团队设置目标">{project.id}</output><p>{project.path}</p><button onClick={onBack}>返回我的项目</button></div> }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const folders = ["code", "docs"].map(name => ({ id: "agent:runner:" + name, name, path: "D:/" + name, enabled: true, revision: "1", connected: true, agent_status: "online", active_jobs: 0 })) as RegisteredProject[];
const state = { topology: { experience: "full", server: { kind: "local" } }, readiness: { server: "ready", runner: "ready" }, current_operation: null } as DesktopState;
const api = { listNamedProjects: vi.fn(), saveNamedProjects: vi.fn(), listProjects: vi.fn(), inspectProject: vi.fn(), addProject: vi.fn(), mutateProject: vi.fn() };
const onState = vi.fn();
let registry: RegisteredProject[];
let catalog: WorkspaceCatalog;

function project(name = "EAS", selected = folders, teamProject?: string): NamedProject {
  return { id: "wc_workspace_" + name.padEnd(32, "a"), name, enabled: true, folders: selected.map(folder => ({ project: folder.id, name: folder.name, path: folder.path })), team_project: teamProject };
}
beforeEach(() => {
  vi.resetAllMocks();
  registry = [...folders];
  catalog = { host_id: "host", revision: 4, projects: [] };
  api.listNamedProjects.mockImplementation(async () => catalog);
  api.listProjects.mockImplementation(async () => ({ projects: registry, can_manage: true }));
  api.inspectProject.mockImplementation(async (path: string) => ({ path, allowed_root: path, is_git_repository: true }));
  api.addProject.mockResolvedValue(state);
  api.saveNamedProjects.mockImplementation(async (_revision: number, projects: WorkspaceDraft[]) => {
    catalog = { host_id: "host", revision: 5, projects: projects.map(draft => ({ ...draft, folders: draft.folders.map(id => {
      const folder = registry.find(item => item.id === id)!;
      return { project: id, name: folder.name, path: folder.path };
    }) })) };
    return catalog;
  });
});
function show(initialState = state) {
  return render(<NamedProjectsPanel state={initialState} onState={onState} api={api} />);
}
async function create(name = "EAS") {
  await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "新建项目" }));
  fireEvent.change(screen.getByLabelText("项目名称"), { target: { value: name } });
}
async function addPath(path: string) {
  fireEvent.change(screen.getByLabelText("文件夹路径"), { target: { value: path } });
  fireEvent.click(screen.getByRole("button", { name: "添加路径" }));
  await waitFor(() => expect(screen.getByLabelText("文件夹路径")).toHaveValue(""));
}

it("creates a project with exact folder grants, an explicit team folder, and observed revision fences", async () => {
  show(); await create();
  expect(screen.getByRole("button", { name: "创建项目" })).toBeDisabled();
  await addPath("D:/code"); await addPath("D:/docs");
  expect(screen.getByRole("button", { name: "创建项目" })).toBeDisabled();
  fireEvent.change(screen.getByRole("combobox", { name: /团队工作文件夹/ }), { target: { value: folders[1].id } });
  fireEvent.click(screen.getByRole("button", { name: "创建项目" }));
  await waitFor(() => expect(api.saveNamedProjects).toHaveBeenCalledWith(4, [expect.objectContaining({ name: "EAS", folders: folders.map(folder => folder.id), team_project: folders[1].id })], "host", Object.fromEntries(folders.map(folder => [folder.id, folder.revision]))));
  expect(api.addProject).not.toHaveBeenCalled();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(screen.queryByText(/高级：|此电脑上的项目|当前项目/)).not.toBeInTheDocument();
});

it("uses the native folder chooser and registers only new canonical paths before saving", async () => {
  const added = { ...folders[0], id: "agent:runner:new", name: "new", path: "D:/new", revision: "9" };
  vi.mocked(open).mockResolvedValue(["D:/code", "D:/alias"]);
  api.inspectProject.mockImplementation(async (path: string) => ({ path: path === "D:/alias" ? added.path : path }));
  api.addProject.mockImplementation(async () => { registry = [...folders, added]; return state; });
  show(); await create();
  fireEvent.click(screen.getByRole("button", { name: "添加文件夹…" }));
  await screen.findByText("new", { selector: "strong" });
  await waitFor(() => expect(screen.queryByText("正在处理……")).not.toBeInTheDocument());
  expect(open).toHaveBeenCalledWith({ directory: true, multiple: true, title: "添加源文件夹" });
  expect(api.addProject).toHaveBeenCalledExactlyOnceWith("D:/new");
  expect(onState).toHaveBeenCalledExactlyOnceWith(state);
  fireEvent.change(screen.getByRole("combobox", { name: /团队工作文件夹/ }), { target: { value: added.id } });
  fireEvent.click(screen.getByRole("button", { name: "创建项目" }));
  await waitFor(() => expect(api.saveNamedProjects).toHaveBeenCalledWith(4, [expect.objectContaining({ folders: [folders[0].id, added.id] })], "host", expect.objectContaining({ [added.id]: "9" })));
});

it("does not duplicate an already registered or already selected Windows folder", async () => {
  show(); await create();
  await addPath("D:/code");
  await addPath("\\\\?\\d:\\CODE\\");
  expect(screen.getAllByRole("button", { name: "移除源文件夹 code" })).toHaveLength(1);
  expect(api.addProject).not.toHaveBeenCalled();
});

it("reuses a Windows drive root regardless of drive-letter case", async () => {
  registry = [{ ...folders[0], path: "Z:\\" }];
  show(); await create(); await addPath("\\\\?\\z:\\");
  expect(api.addProject).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "创建项目" })).toBeEnabled();
});

it("only re-enables a registered folder after an explicit click and confirms its new revision", async () => {
  registry = [{ ...folders[0], enabled: false, revision: "disabled-revision" }];
  api.mutateProject.mockImplementation(async () => {
    registry = [{ ...folders[0], revision: "enabled-revision" }];
    return state;
  });
  show(); await create(); await addPath("D:/code");
  expect(api.addProject).not.toHaveBeenCalled();
  expect(api.mutateProject).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "创建项目" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "启用文件夹" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "创建项目" })).toBeEnabled());
  expect(api.mutateProject).toHaveBeenCalledExactlyOnceWith(folders[0].id, "disabled-revision", "enable");
  fireEvent.click(screen.getByRole("button", { name: "创建项目" }));
  await waitFor(() => expect(api.saveNamedProjects).toHaveBeenCalledWith(4, expect.any(Array), "host", { [folders[0].id]: "enabled-revision" }));
});

it("keeps saving disabled if a folder enable response is not confirmed by the registry", async () => {
  registry = [{ ...folders[0], enabled: false }];
  api.mutateProject.mockResolvedValue(state);
  show(); await create(); await addPath("D:/code");
  fireEvent.click(screen.getByRole("button", { name: "启用文件夹" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("尚未确认文件夹已启用");
  expect(screen.getByRole("button", { name: "创建项目" })).toBeDisabled();
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
});

it("canceling after adding a folder never saves new named-project authorization", async () => {
  const added = { ...folders[0], id: "agent:runner:new", name: "new", path: "D:/new" };
  api.addProject.mockImplementation(async () => { registry = [...folders, added]; return state; });
  show(); await create(); await addPath(added.path);
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
  expect(catalog.projects).toEqual([]);
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("keeps an invalid folder editable and allows a corrected path", async () => {
  api.inspectProject.mockRejectedValueOnce(new Error("invalid_path"));
  show(); await create();
  fireEvent.change(screen.getByLabelText("文件夹路径"), { target: { value: "missing" } });
  fireEvent.click(screen.getByRole("button", { name: "添加路径" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("检查文件夹路径");
  expect(api.addProject).not.toHaveBeenCalled();
  await addPath("D:/code");
  expect(screen.getByRole("button", { name: "创建项目" })).toBeEnabled();
});

it("requires fresh observation after a rejected save and does not automatically retry", async () => {
  catalog.projects = [project("EAS", [folders[0]])];
  api.saveNamedProjects.mockRejectedValue({ code: "workspace_revision_conflict" });
  show(); fireEvent.click(await screen.findByRole("button", { name: "编辑项目" }));
  fireEvent.click(screen.getByRole("button", { name: "保存项目" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("取消后刷新列表");
  expect(api.saveNamedProjects).toHaveBeenCalledTimes(1);
  expect(screen.getByLabelText("项目名称")).toHaveValue("EAS");
  expect(screen.getByRole("button", { name: "保存项目" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(screen.getByRole("button", { name: "新建项目" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "刷新列表" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toBeEnabled());
});

it("confirms removal and only changes the named catalog, preserving other project grants", async () => {
  const other = project("Other", folders, folders[1].id);
  catalog.projects = [project("EAS", [folders[0]]), other];
  show();
  fireEvent.click(within(await screen.findByRole("article", { name: "EAS" })).getByRole("button", { name: "移除项目" }));
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog")).toHaveTextContent("本地文件不会删除");
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
  fireEvent.click(within(screen.getByRole("article", { name: "EAS" })).getByRole("button", { name: "移除项目" }));
  fireEvent.click(screen.getByRole("button", { name: "确认移除" }));
  await waitFor(() => expect(api.saveNamedProjects).toHaveBeenCalledWith(4, [expect.objectContaining({ id: other.id, name: other.name, folders: folders.map(folder => folder.id), team_project: folders[1].id })], "host", expect.any(Object)));
  expect(screen.queryByRole("article", { name: "EAS" })).not.toBeInTheDocument();
  expect(api.addProject).not.toHaveBeenCalled();
});

it("can recreate a removed project by adding its still registered folder path", async () => {
  catalog.projects = [project("EAS", [folders[0]])];
  show(); fireEvent.click(await screen.findByRole("button", { name: "移除项目" }));
  fireEvent.click(screen.getByRole("button", { name: "确认移除" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  await create("Recreated"); await addPath("D:/code");
  fireEvent.click(screen.getByRole("button", { name: "创建项目" }));
  await screen.findByRole("article", { name: "Recreated" });
  expect(api.addProject).not.toHaveBeenCalled();
  expect(api.saveNamedProjects).toHaveBeenLastCalledWith(5, [expect.objectContaining({ name: "Recreated", folders: [folders[0].id] })], "host", expect.any(Object));
});

it("toggles project availability without rewriting its team selection", async () => {
  catalog.projects = [project("EAS", folders, folders[1].id)];
  show(); fireEvent.click(await screen.findByRole("button", { name: "停用项目" }));
  await screen.findByRole("button", { name: "启用项目" });
  expect(api.saveNamedProjects).toHaveBeenCalledWith(4, [expect.objectContaining({ enabled: false, team_project: folders[1].id })], "host", expect.any(Object));
});

it("opens the sole folder policy directly without rewriting saved settings", async () => {
  catalog.projects = [project("测文件", [folders[0]])];
  show(); fireEvent.click(await screen.findByRole("button", { name: "团队设置" }));
  expect(screen.getByRole("heading", { name: "测文件 · 设置" })).toBeInTheDocument();
  expect(screen.getByLabelText("团队设置目标")).toHaveTextContent(folders[0].id);
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "返回我的项目" }));
  expect(screen.getByRole("heading", { name: "测文件" })).toBeInTheDocument();
});

it("opens the explicitly saved team folder for a multi-folder project", async () => {
  catalog.projects = [project("EAS", folders, folders[1].id)];
  show(); fireEvent.click(await screen.findByRole("button", { name: "团队设置" }));
  expect(screen.getByLabelText("团队设置目标")).toHaveTextContent(folders[1].id);
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
});

it("requires a legacy multi-folder project to save an explicit team folder before opening settings", async () => {
  catalog.projects = [project()];
  show(); fireEvent.click(await screen.findByRole("button", { name: "团队设置" }));
  expect(screen.queryByLabelText("团队设置目标")).not.toBeInTheDocument();
  expect(screen.getByRole("dialog", { name: "编辑项目" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "保存项目" })).toBeDisabled();
  fireEvent.change(screen.getByRole("combobox", { name: /团队工作文件夹/ }), { target: { value: folders[1].id } });
  fireEvent.click(screen.getByRole("button", { name: "保存项目" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  fireEvent.click(screen.getByRole("button", { name: "团队设置" }));
  expect(screen.getByLabelText("团队设置目标")).toHaveTextContent(folders[1].id);
});

it("clears the team folder if the user removes it from a project", async () => {
  const third = { ...folders[0], id: "agent:runner:third", name: "third", path: "D:/third" };
  registry = [...folders, third];
  catalog.projects = [project("EAS", registry, folders[1].id)];
  show(); fireEvent.click(await screen.findByRole("button", { name: "编辑项目" }));
  fireEvent.click(screen.getByRole("button", { name: "移除源文件夹 docs" }));
  expect(screen.getByRole("combobox", { name: /团队工作文件夹/ })).toHaveValue("");
  expect(screen.getByRole("button", { name: "保存项目" })).toBeDisabled();
});

it("traps dialog focus and restores it after Escape", async () => {
  show(); await create(); await addPath("D:/code");
  const name = screen.getByLabelText("项目名称");
  expect(name).toHaveFocus();
  const createButton = screen.getByRole("button", { name: "创建项目" });
  createButton.focus();
  fireEvent.keyDown(createButton, { key: "Tab" });
  expect(screen.getByRole("button", { name: "关闭" })).toHaveFocus();
  fireEvent.keyDown(screen.getByRole("button", { name: "关闭" }), { key: "Tab", shiftKey: true });
  expect(createButton).toHaveFocus();
  fireEvent.keyDown(createButton, { key: "Escape" });
  await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toHaveFocus());
  expect(api.saveNamedProjects).not.toHaveBeenCalled();
});

it("does not register a selected folder after the runtime disconnects", async () => {
  let resolveSelection!: (value: string) => void;
  vi.mocked(open).mockImplementation(() => new Promise(resolve => { resolveSelection = resolve; }));
  const view = show(); await create();
  fireEvent.click(screen.getByRole("button", { name: "添加文件夹…" }));
  const offline = { ...state, readiness: { ...state.readiness, runner: "stopped" } } as DesktopState;
  view.rerender(<NamedProjectsPanel state={offline} onState={onState} api={api} />);
  await act(async () => { resolveSelection("D:/new"); });
  expect(api.inspectProject).not.toHaveBeenCalled();
  expect(api.addProject).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "创建项目" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  view.rerender(<NamedProjectsPanel state={state} onState={onState} api={api} />);
  await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toBeEnabled());
});

it("prevents duplicate save submissions while the response is pending", async () => {
  let finish!: (value: WorkspaceCatalog) => void;
  api.saveNamedProjects.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  show(); await create(); await addPath("D:/code");
  fireEvent.submit(screen.getByRole("dialog"));
  fireEvent.submit(screen.getByRole("dialog"));
  expect(api.saveNamedProjects).toHaveBeenCalledTimes(1);
  await act(async () => { finish({ host_id: "host", revision: 5, projects: [project("EAS", [folders[0]])] }); });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("loads correctly in the desktop StrictMode lifecycle", async () => {
  render(<StrictMode><NamedProjectsPanel state={state} onState={onState} api={api} /></StrictMode>);
  await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toBeEnabled());
});

it("waits for an existing desktop operation before loading projects", async () => {
  const view = show({ ...state, current_operation: { id: "existing" } } as DesktopState);
  expect(api.listNamedProjects).not.toHaveBeenCalled();
  view.rerender(<NamedProjectsPanel state={state} onState={onState} api={api} />);
  await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toBeEnabled());
});
