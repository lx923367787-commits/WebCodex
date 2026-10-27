import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { LocaleProvider } from "../../i18n/locale";
import type { DesktopState, RegisteredProject } from "../../models/topology";
import { ProjectsPanel } from "./ProjectsPanel";
import type { AiOfficeApi, OfficePolicy, OfficeSnapshot } from "../ai-office/types";

const api = vi.hoisted(() => ({ listProjects: vi.fn(), inspectProject: vi.fn(), addProject: vi.fn(), switchProject: vi.fn(), mutateProject: vi.fn(), getState: vi.fn() }));
const dialog = vi.hoisted(() => ({ open: vi.fn() }));
vi.mock("../../lib/desktop-api", () => ({ desktopApi: api }));
vi.mock("@tauri-apps/plugin-dialog", () => dialog);

const projects: RegisteredProject[] = ["Alpha", "Beta", "Gamma"].map((name, index) => ({
  id: "agent:runner:" + name, name, path: "C:\\work\\" + name, revision: "sha256:" + index,
  enabled: index !== 1, connected: true, agent_status: "online", active_jobs: 0,
}));
const state = {
  topology: { experience: "full", server: { kind: "local" } },
  readiness: { server: "ready", runner: "ready", runtime_ready: true },
  project: { path: projects[0].path, runtime_project_id: projects[0].id },
  current_operation: null,
} as DesktopState;

function renderPanel(initial = state, officeApi?: AiOfficeApi, preview = false) {
  const onState = vi.fn();
  const view = render(<LocaleProvider><ProjectsPanel state={initial} onState={onState} onChooseProject={vi.fn()} officeApi={officeApi} preview={preview} /></LocaleProvider>);
  return { ...view, onState };
}

function officeFixture(registered = projects): AiOfficeApi {
  const policy: OfficePolicy = { revision: 7, mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 }, providers: ["workbuddy", "antigravity"].map((provider_id) => ({ provider_id: provider_id as "workbuddy" | "antigravity", enabled: true, model: null, effort: null })) };
  const snapshot: OfficeSnapshot = { source: "runtime", observed_at_ms: 1, selected_project_id: projects[0].id, members: [], enforcement: "demo_plan_only", limitations: [], projects: registered.map((project) => ({ ...project, policy })) };
  return {
    snapshot: vi.fn(async () => snapshot),
    savePolicy: vi.fn(async (_project, revision, input) => ({ ...input, revision: revision + 1 })),
    discoverCli: vi.fn().mockResolvedValue([]), refreshCli: vi.fn(), evaluatePlan: vi.fn(),
  };
}

async function row(name: string) {
  return within(await screen.findByRole("article", { name }));
}

describe("Desktop project registry", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    localStorage.setItem("webcodex.desktop.locale", "en-US");
    api.listProjects.mockResolvedValue({ projects, can_manage: true });
    api.getState.mockResolvedValue(state);
    api.inspectProject.mockImplementation(async (path: string) => ({ path, allowed_root: path, is_git_repository: false }));
    api.addProject.mockResolvedValue(state);
    api.switchProject.mockResolvedValue(state);
    api.mutateProject.mockResolvedValue(state);
  });

  it("lists all registered folders and distinguishes current, disabled and active jobs", async () => {
    api.listProjects.mockResolvedValue({ projects: projects.map((p, i) => ({ ...p, active_jobs: i === 2 ? 1 : 0 })), can_manage: true });
    renderPanel();
    expect((await row("Alpha")).getByRole("button", { name: "Current project" })).toBeDisabled();
    expect((await row("Beta")).getByRole("button", { name: "Use this project" })).toBeDisabled();
    expect((await row("Beta")).getByRole("button", { name: "Enable" })).toBeEnabled();
    expect((await row("Gamma")).getByRole("button", { name: "Remove" })).toBeDisabled();
    expect(screen.getByText(projects[0].path)).toBeVisible();
  });

  it("adds the selected folder without changing the current project", async () => {
    const view = renderPanel();
    await row("Alpha");
    dialog.open.mockResolvedValue("C:\\work\\New");
    fireEvent.click(screen.getByRole("button", { name: "Add project" }));
    await waitFor(() => expect(api.addProject).toHaveBeenCalledWith("C:\\work\\New"));
    await waitFor(() => expect(view.onState).toHaveBeenCalledWith(state));
    expect(api.switchProject).not.toHaveBeenCalled();
  });

  it("switches with the observed identity and revision and blocks duplicate clicks", async () => {
    let finish!: (value: DesktopState) => void;
    api.switchProject.mockReturnValue(new Promise<DesktopState>((resolve) => { finish = resolve; }));
    const view = renderPanel();
    const button = (await row("Gamma")).getByRole("button", { name: "Use this project" });
    fireEvent.click(button); fireEvent.click(button);
    expect(api.switchProject).toHaveBeenCalledTimes(1);
    expect(api.switchProject).toHaveBeenCalledWith(projects[2].id, projects[2].revision);
    expect((await row("Alpha")).getByRole("button", { name: "Disable" })).toBeDisabled();
    const selected = { ...state, project: { ...state.project!, path: projects[2].path, runtime_project_id: projects[2].id } };
    await act(async () => finish(selected));
    expect(view.onState).toHaveBeenCalledWith(selected);
  });

  it("enables and disables through the registry without stopping jobs or deleting files", async () => {
    const cleared = { ...state, project: null };
    api.mutateProject.mockResolvedValueOnce(cleared).mockResolvedValue(state);
    const view = renderPanel();
    fireEvent.click((await row("Alpha")).getByRole("button", { name: "Disable" }));
    await waitFor(() => expect(view.onState).toHaveBeenCalledWith(cleared));
    await waitFor(() => expect(within(screen.getByRole("article", { name: "Beta" })).getByRole("button", { name: "Enable" })).toBeEnabled());
    fireEvent.click((await row("Beta")).getByRole("button", { name: "Enable" }));
    await waitFor(() => expect(api.mutateProject).toHaveBeenCalledWith(projects[1].id, projects[1].revision, "enable"));
    expect(api.mutateProject).toHaveBeenCalledWith(projects[0].id, projects[0].revision, "disable");
  });

  it("requires explicit removal confirmation and explains that local files are retained", async () => {
    renderPanel();
    const remove = (await row("Alpha")).getByRole("button", { name: "Remove" });
    fireEvent.click(remove);
    const modal = await screen.findByRole("dialog");
    expect(within(modal).getByText(/local folder, source files and Git branches are kept/)).toBeVisible();
    expect(within(modal).getByText(/restarting will not re-add/)).toBeVisible();
    fireEvent.click(within(modal).getByRole("button", { name: "Cancel" }));
    expect(api.mutateProject).not.toHaveBeenCalled();
    fireEvent.click(remove);
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Remove registration" }));
    await waitFor(() => expect(api.mutateProject).toHaveBeenCalledWith(projects[0].id, projects[0].revision, "unregister"));
  });

  it("re-lists an unknown result without retrying the mutation or showing optimistic success", async () => {
    api.mutateProject.mockRejectedValue({ code: "project_operation_indeterminate", message: "Uncertain", next_action: "Refresh" });
    api.getState.mockResolvedValue({ ...state, project: null });
    const view = renderPanel();
    fireEvent.click((await row("Beta")).getByRole("button", { name: "Enable" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("outcome is unconfirmed");
    expect(api.mutateProject).toHaveBeenCalledTimes(1);
    expect(api.listProjects).toHaveBeenCalledTimes(2);
    expect(view.onState).toHaveBeenCalledWith(expect.objectContaining({ project: null }));
  });

  it("keeps the selected project after a stale revision rejection and refreshes evidence", async () => {
    api.switchProject.mockRejectedValue({ code: "project_revision_conflict", message: "Changed", next_action: "Refresh" });
    const view = renderPanel();
    fireEvent.click((await row("Gamma")).getByRole("button", { name: "Use this project" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("project or connection changed");
    expect(view.onState).toHaveBeenCalledWith(state);
    expect(api.switchProject).toHaveBeenCalledTimes(1);
  });

  it("does not present a failed inventory request as an empty project list", async () => {
    api.listProjects.mockRejectedValue({ code: "project_agent_unavailable", message: "Unavailable", next_action: "Refresh" });
    renderPanel();
    expect(await screen.findByRole("alert")).toBeVisible();
    expect(screen.queryByText("No registered projects. Add a folder to get started.")).not.toBeInTheDocument();
    expect(api.mutateProject).not.toHaveBeenCalled();
  });

  it("opens a disabled idle folder's own team policy and saves with its observed revision", async () => {
    const office = officeFixture();
    renderPanel(state, office);
    fireEvent.click((await row("Beta")).getByRole("button", { name: "团队设置" }));
    expect(await screen.findByRole("heading", { name: "项目团队设置" })).toBeVisible();
    expect(await screen.findByRole("button", { name: /总经理自主安排/ })).toHaveAttribute("aria-pressed", "true");
    expect(office.snapshot).toHaveBeenCalledWith(projects[1].id);
    fireEvent.click(screen.getByRole("button", { name: /我来设定边界/ }));
    fireEvent.change(screen.getByLabelText("总经理可委派的子 Agent最多数量"), { target: { value: "6" } });
    fireEvent.click(screen.getByRole("button", { name: "保存团队策略" }));
    await waitFor(() => expect(office.savePolicy).toHaveBeenCalledWith(projects[1].id, 7, expect.objectContaining({ mode: "custom", subagents: { min: 0, max: 6 } })));
    expect(api.switchProject).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /返回项目列表/ }));
    expect(await screen.findByRole("article", { name: "Beta" })).toBeVisible();
  });

  it("opens the verified newly registered folder's team settings without switching workspaces", async () => {
    const added = { ...projects[0], id: "agent:runner:New", name: "New", path: "C:\\work\\New" };
    api.listProjects.mockResolvedValueOnce({ projects, can_manage: true }).mockResolvedValue({ projects: [...projects, added], can_manage: true });
    const office = officeFixture([...projects, added]);
    renderPanel(state, office);
    await row("Alpha");
    dialog.open.mockResolvedValue("C:\\work\\New");
    fireEvent.click(screen.getByRole("button", { name: "Add project" }));
    expect(await screen.findByRole("heading", { name: "项目团队设置" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "New" })).toBeVisible();
    await waitFor(() => expect(office.snapshot).toHaveBeenCalledWith(added.id));
    expect(api.switchProject).not.toHaveBeenCalled();
  });

  it("does not guess an added folder after an uncertain registration result", async () => {
    api.addProject.mockRejectedValue({ code: "project_operation_indeterminate", message: "Uncertain", next_action: "Refresh" });
    const office = officeFixture();
    renderPanel(state, office);
    await row("Alpha");
    dialog.open.mockResolvedValue("C:\\work\\New");
    fireEvent.click(screen.getByRole("button", { name: "Add project" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("outcome is unconfirmed");
    expect(office.snapshot).not.toHaveBeenCalled();
    expect(screen.queryByRole("heading", { name: "项目团队设置" })).not.toBeInTheDocument();
  });

  it("matches the inspected canonical folder when the chosen path is a junction", async () => {
    const selectedPath = "C:\\links\\project-alias";
    const added = { ...projects[0], id: "agent:runner:Canonical", name: "Canonical", path: "E:\\projects\\canonical" };
    api.listProjects.mockResolvedValueOnce({ projects, can_manage: true }).mockResolvedValue({ projects: [...projects, added], can_manage: true });
    api.inspectProject.mockResolvedValue({ path: "\\\\?\\E:\\projects\\canonical", allowed_root: "E:\\projects", is_git_repository: true });
    const office = officeFixture([...projects, added]);
    renderPanel(state, office);
    await row("Alpha");
    dialog.open.mockResolvedValue(selectedPath);
    fireEvent.click(screen.getByRole("button", { name: "Add project" }));
    expect(await screen.findByRole("heading", { name: "项目团队设置" })).toBeVisible();
    expect(api.inspectProject).toHaveBeenCalledWith(selectedPath);
    expect(api.addProject).toHaveBeenCalledWith(selectedPath);
    expect(api.inspectProject.mock.invocationCallOrder[0]).toBeLessThan(api.addProject.mock.invocationCallOrder[0]);
    await waitFor(() => expect(office.snapshot).toHaveBeenCalledWith(added.id));
    expect(api.switchProject).not.toHaveBeenCalled();
  });

  it("does not mistake a concurrently registered different folder for the chosen folder", async () => {
    const unrelated = { ...projects[0], id: "agent:runner:Other", name: "Other", path: "C:\\work\\Other" };
    api.listProjects.mockResolvedValueOnce({ projects, can_manage: true }).mockResolvedValue({ projects: [...projects, unrelated], can_manage: true });
    const office = officeFixture([...projects, unrelated]);
    renderPanel(state, office);
    await row("Alpha");
    dialog.open.mockResolvedValue("C:\\work\\New");
    fireEvent.click(screen.getByRole("button", { name: "Add project" }));
    expect(await screen.findByRole("article", { name: "Other" })).toBeVisible();
    expect(office.snapshot).not.toHaveBeenCalled();
    expect(screen.queryByRole("heading", { name: "项目团队设置" })).not.toBeInTheDocument();
    expect(api.switchProject).not.toHaveBeenCalled();
  });

  it("lets a preview configure teams while all folder mutations remain disabled", async () => {
    const office = officeFixture();
    renderPanel(state, office, true);
    expect((await row("Gamma")).getByRole("button", { name: "Use this project" })).toBeDisabled();
    expect((await row("Gamma")).getByRole("button", { name: "Disable" })).toBeDisabled();
    expect((await row("Gamma")).getByRole("button", { name: "Remove" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add project" })).toBeDisabled();
    fireEvent.click((await row("Gamma")).getByRole("button", { name: "团队设置" }));
    expect(await screen.findByRole("button", { name: /总经理自主安排/ })).toBeVisible();
    expect(office.snapshot).toHaveBeenCalledWith(projects[2].id);
    expect(api.mutateProject).not.toHaveBeenCalled();
  });
});
