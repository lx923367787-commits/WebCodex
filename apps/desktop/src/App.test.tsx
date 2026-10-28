import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopState } from "./models/topology";
import { LocaleProvider } from "./i18n/locale";

const api = vi.hoisted(() => ({
  getState: vi.fn(),
  updateTunnelConfig: vi.fn(),
  openPowerShellInstallGuide: vi.fn(),
  refresh: vi.fn(),
  observeChatgptActivity: vi.fn(),
  resumeSavedRuntime: vi.fn(),
  updateTunnelProxy: vi.fn(),
  activity: vi.fn(),
  configureLocal: vi.fn(),
  activateLocalProject: vi.fn(),
  listProjects: vi.fn(),
  listNamedProjects: vi.fn(),
  saveNamedProjects: vi.fn(),
  addProject: vi.fn(),
  switchProject: vi.fn(),
  mutateProject: vi.fn(),
  configureRemote: vi.fn(),
  startQuickShare: vi.fn(),
  stopQuickShare: vi.fn(),
  stopLocalRuntime: vi.fn(),
  startRegularTunnel: vi.fn(),
  stopRegularTunnel: vi.fn(),
  cancelOperation: vi.fn(),
  inspectProject: vi.fn(),
  getLaunchAtLogin: vi.fn(),
  setLaunchAtLogin: vi.fn(),
}));

const clipboard = vi.hoisted(() => ({ writeText: vi.fn() }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => clipboard);

const tauriEvents = vi.hoisted(() => ({
  handler: null as null | ((event: { payload: unknown }) => void),
  listen: vi.fn(),
}));

vi.mock("./lib/desktop-api", () => ({
  desktopApi: api,
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: tauriEvents.listen,
}));

import App from "./App";
import { open } from "@tauri-apps/plugin-dialog";

const readyState: DesktopState = {
  topology: {
    experience: "full",
    server: { kind: "local" },
    runner: { kind: "local" },
    exposure: { kind: "none" },
    enrollment: { kind: "managed_pairing" },
  },
  readiness: {
    server: "ready",
    runner: "ready",
    exposure: "local_ready",
    project: "ready",
    runtime_ready: true,
    ready_for_chatgpt: false,
    summary: "Runtime ready on this computer",
    summary_kind: "runtime_ready_local_only",
    next_action: "Choose a ChatGPT connection in Connection.",
    next_action_kind: "choose_connection",
  },
  project: {
    path: "C:\\fixture\\repo",
    allowed_root: "C:\\fixture",
    is_git_repository: true,
    runtime_project_id: "agent:desktop:repo",
  },
  binaries: {
    directory: "C:\\fixture\\bin",
    version: "0.3.9",
    git_commit: "0123456789abcdef",
    source: "WEBCODEX_DESKTOP_BIN_DIR",
  },
  quick_share: null,
  regular_tunnel: null,
  activity_sequence: 0,
  openai_tunnel_configured: true,
  openai_tunnel_config: {
    tunnel_id_present: true,
    api_key_present: true,
    source: "environment",
    saved_tunnel_id: null,
  },
  regular_tunnel_available: true,
  runtime_autostart: false,
  preferred_connection: "no_chat_gpt",
  tunnel_proxy: {
    mode: "auto",
    custom_url: null,
    effective_source: "direct",
    effective_url: null,
    detected_url: null,
  },
};

const firstRunState: DesktopState = {
  readiness: {
    server: "unknown",
    runner: "unknown",
    exposure: "unknown",
    project: "none",
    runtime_ready: false,
    ready_for_chatgpt: false,
    summary: "WebCodex Service needs attention",
    summary_kind: "service_needs_attention",
    next_action: "Start or reconnect the WebCodex Service.",
    next_action_kind: "start_or_reconnect_service",
  },
  activity_sequence: 0,
  openai_tunnel_configured: true,
  openai_tunnel_config: {
    tunnel_id_present: true,
    api_key_present: true,
    source: "environment",
    saved_tunnel_id: null,
  },
  regular_tunnel_available: true,
  runtime_autostart: false,
  preferred_connection: "no_chat_gpt",
  tunnel_proxy: {
    mode: "auto",
    custom_url: null,
    effective_source: "direct",
    effective_url: null,
    detected_url: null,
  },
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function setupState(): DesktopState {
  return {
    ...readyState,
    readiness: {
      ...readyState.readiness,
      server: "stopped",
      runner: "stopped",
      exposure: "disabled",
      project: "configured",
      runtime_ready: false,
      ready_for_chatgpt: false,
      summary: "WebCodex Service needs attention",
      summary_kind: "service_needs_attention",
      next_action: "Start or reconnect the WebCodex Service.",
      next_action_kind: "start_or_reconnect_service",
    },
    current_operation: null,
  };
}

function localSetupOperationState(
  phase: "running" | "cancelling" = "running",
  activitySequence = 1,
): DesktopState {
  return {
    ...setupState(),
    activity_sequence: activitySequence,
    current_operation: {
      id: "desktop-operation-a",
      kind: "local_setup",
      phase,
      started_at_ms: 1_000,
      cancellable: true,
    },
  };
}

function renderApp() {
  return render(
    <LocaleProvider>
      <App />
    </LocaleProvider>,
  );
}

describe("semantic Desktop UI", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriEvents.handler = null;
    tauriEvents.listen.mockImplementation(
      async (_eventName: string, handler: (event: { payload: unknown }) => void) => {
        tauriEvents.handler = handler;
        return () => {
          if (tauriEvents.handler === handler) tauriEvents.handler = null;
        };
      },
    );
    api.activity.mockResolvedValue([]);
    api.listProjects.mockResolvedValue({ projects: [], can_manage: true });
    api.listNamedProjects.mockResolvedValue({ host_id: "fixture-host", revision: 1, projects: [] });
    api.getLaunchAtLogin.mockResolvedValue(false);
    api.openPowerShellInstallGuide.mockResolvedValue(undefined);
    api.setLaunchAtLogin.mockImplementation(async (enabled: boolean) => enabled);
    api.resumeSavedRuntime.mockResolvedValue(readyState);
    api.observeChatgptActivity.mockResolvedValue(readyState);
    api.updateTunnelProxy.mockResolvedValue(readyState);
    api.startRegularTunnel.mockResolvedValue(readyState);
    api.stopRegularTunnel.mockResolvedValue(readyState);
  });

  it.each([true, false])("starts services from Projects without a project (saved topology: %s)", async (savedTopology) => {
    api.getState.mockResolvedValue(savedTopology ? { ...setupState(), project: null } : firstRunState);
    const starting = deferred<DesktopState>();
    if (savedTopology) api.resumeSavedRuntime.mockReturnValueOnce(starting.promise);
    else api.configureLocal.mockReturnValueOnce(starting.promise);
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "项目" }));
    expect(screen.getByText(/无需先创建或选择项目/)).toBeVisible();
    expect(api.listProjects).not.toHaveBeenCalled();
    expect(api.listNamedProjects).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "启动服务" }));
    expect(savedTopology ? api.resumeSavedRuntime : api.configureLocal).toHaveBeenCalledWith();
    expect(screen.getByRole("button", { name: "正在启动服务…" })).toBeDisabled();
    expect(api.inspectProject).not.toHaveBeenCalled();
    expect(api.addProject).not.toHaveBeenCalled();
    await act(async () => starting.resolve({ ...readyState, project: null }));
    expect(await screen.findByText(/还没有项目/)).toBeVisible();
  });

  it("keeps a user stop consistent after refresh and offers Start", async () => {
    const stopped = setupState();
    api.getState.mockResolvedValue(stopped);
    api.refresh.mockResolvedValue(stopped);
    renderApp();
    await screen.findByRole("heading", { name: "WebCodex", level: 1 });
    expect(screen.getByRole("status")).toHaveTextContent("已停止");
    expect(screen.getByRole("button", { name: "启动" })).toBeEnabled();
    expect(screen.queryByText(/运行环境正常/)).not.toBeInTheDocument();
    expect(screen.queryByText(/服务需要处理/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    await waitFor(() => expect(api.refresh).toHaveBeenCalledOnce());
    expect(screen.getByRole("status")).toHaveTextContent("已停止");
    expect(screen.getAllByText("启动后继续").length).toBeGreaterThan(1);
  });

  it("retries and repeats Tunnel ID copying without restarting or exposing an API key", async () => {
    const writeText = clipboard.writeText.mockRejectedValueOnce(new Error("clipboard denied")).mockResolvedValue(undefined);
    api.getState.mockResolvedValue({ ...readyState,
      openai_tunnel_config: { ...readyState.openai_tunnel_config, effective_tunnel_id: "tunnel_fixture" },
      regular_tunnel: { provider: "openai", status: "ready", clipboard_state: "unavailable", clipboard_contains: "tunnel_id", ready_for_chatgpt: true },
    });
    const view = renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    const copy = screen.getByRole("button", { name: "复制 Tunnel ID" });
    fireEvent.click(copy);
    expect(await screen.findByText(/复制失败，请重试/)).toBeInTheDocument();
    fireEvent.click(copy);
    await screen.findByText("连接信息已复制到剪贴板");
    fireEvent.click(copy);
    await waitFor(() => expect(writeText).toHaveBeenCalledTimes(3));
    expect(writeText).toHaveBeenLastCalledWith("tunnel_fixture");
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
    expect(api.stopRegularTunnel).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Tunnel API key")).toHaveValue("");
    view.unmount();
  });

  it("shows named project results and hides routine process events", async () => {
    api.getState.mockResolvedValue(readyState);
    api.activity.mockResolvedValue([
      { sequence: 1, timestamp_ms: 1, source: "desktop", level: "info", event_kind: "operation_started", message: "Desktop operation started: local_project_activate" },
      { sequence: 2, timestamp_ms: 2, source: "runner", level: "info", event_kind: "process_started", message: "" },
      { sequence: 3, timestamp_ms: 3, source: "desktop", level: "info", event_kind: "project_activated", message: "sample-project" },
    ]);
    renderApp();
    await screen.findByRole("heading", { name: "WebCodex", level: 1 });
    expect(screen.getByText(/没有项目时/)).toBeInTheDocument();
    expect(screen.queryByText(/读取.*README/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "活动" }));
    await screen.findByText("已切换到 sample-project");
    expect(screen.getAllByRole("article")[0]).toHaveTextContent("已切换到 sample-project");
    expect(screen.getAllByRole("article")).toHaveLength(2);
    expect(screen.getAllByRole("article")[1]).toHaveTextContent("正在激活项目");
  });

  it("starts a tunnel only after explicit action and allows retry after failure", async () => {
    api.getState.mockResolvedValue(readyState);
    api.startRegularTunnel.mockRejectedValueOnce({ code: "tunnel_unavailable", message: "Tunnel failed", next_action: "Retry." });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    fireEvent.click(screen.getByRole("radio", { name: /OpenAI Secure Tunnel/ }));
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
    const start = screen.getByRole("button", { name: "启动安全隧道" });
    fireEvent.click(start);
    expect(await screen.findByRole("alert")).toHaveTextContent("tunnel_unavailable");
    await waitFor(() => expect(start).toBeEnabled());
    fireEvent.click(start);
    await waitFor(() => expect(api.startRegularTunnel).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
  });

  it("can connect before registering the first named project without selecting a legacy folder", async () => {
    const unselected: DesktopState = { ...readyState, project: null, preferred_connection: "open_ai_tunnel",
      readiness: { ...readyState.readiness, project: "none", runtime_ready: false, ready_for_chatgpt: false } };
    api.getState.mockResolvedValue(unselected);
    api.startRegularTunnel.mockResolvedValue({ ...unselected, regular_tunnel: {
      provider: "openai", status: "ready", clipboard_state: "copied", clipboard_contains: "tunnel_id", ready_for_chatgpt: true,
    } });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    const start = screen.getByRole("button", { name: "启动安全隧道" });
    expect(start).toBeEnabled();
    fireEvent.click(start);
    await waitFor(() => expect(api.startRegularTunnel).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "项目" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "新建项目" })).toBeEnabled());
    expect(api.addProject).not.toHaveBeenCalled();
    expect(api.configureLocal).not.toHaveBeenCalled();
    expect(api.activateLocalProject).not.toHaveBeenCalled();
  });

  it.each(["none", "configured", "reload_required", "error", "ready"] as const)("keeps Home and Connection service actions consistent with project state %s", async project => {
    const state: DesktopState = { ...readyState, project: null, preferred_connection: "open_ai_tunnel", readiness: { ...readyState.readiness, project, runtime_ready: false } };
    api.getState.mockResolvedValue(state);
    api.observeChatgptActivity.mockResolvedValue(state);
    renderApp();
    expect(await screen.findByRole("button", { name: "启动安全隧道" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "启动" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "连接" }));
    expect(screen.getByRole("button", { name: "启动安全隧道" })).toBeEnabled();
    expect(api.configureLocal).not.toHaveBeenCalled();
    expect(api.activateLocalProject).not.toHaveBeenCalled();
  });

  it("keeps stop failures visible and never duplicates a failed tunnel", async () => {
    api.getState.mockResolvedValue({
      ...readyState,
      regular_tunnel: { provider: "openai", status: "error", clipboard_state: "unavailable", clipboard_contains: "tunnel_id", ready_for_chatgpt: false },
    });
    api.stopRegularTunnel.mockRejectedValueOnce({ code: "tunnel_unavailable", message: "Stop failed", next_action: "Retry." });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    expect(screen.queryByRole("button", { name: "启动安全隧道" })).not.toBeInTheDocument();
    const stop = document.querySelector<HTMLButtonElement>('[data-webcodex-action="stop-regular-tunnel"]')!;
    fireEvent.click(stop);
    expect(await screen.findByRole("alert")).toHaveTextContent("Stop failed");
    await waitFor(() => expect(stop).toBeEnabled());
    fireEvent.click(stop);
    await waitFor(() => expect(screen.getByRole("button", { name: "启动安全隧道" })).toBeEnabled());
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
  });

  it("shows project availability separately without a global folder picker", async () => {
    api.getState.mockResolvedValue({ ...readyState, project: null });
    api.listNamedProjects.mockResolvedValue({ host_id: "host", revision: 1, projects: [] });
    renderApp();
    expect(await screen.findByText("尚无可用项目，可在项目管理中创建")).toBeInTheDocument();
    expect(screen.queryByText("当前工作项目")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "选择其他项目" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "启动安全隧道" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "管理项目" }));
    expect(await screen.findByRole("heading", { name: "我的项目" })).toBeInTheDocument();
    expect(api.activateLocalProject).not.toHaveBeenCalled();
  });

  it("saves Tunnel settings without starting a connection or returning the secret", async () => {
    api.getState.mockResolvedValue(readyState);
    const savedState: DesktopState = { ...readyState, openai_tunnel_config: {
      tunnel_id_present: true, api_key_present: true, source: "file", saved_tunnel_id: "tunnel_saved",
    } };
    api.updateTunnelConfig.mockResolvedValue(savedState);
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    fireEvent.click(screen.getByText("可选：检查 ChatGPT 安全隧道配置"));
    fireEvent.change(screen.getByLabelText("Tunnel ID"), { target: { value: "tunnel_saved" } });
    const key = screen.getByLabelText("Tunnel API key");
    expect(key).toHaveAttribute("type", "password");
    fireEvent.change(key, { target: { value: "test-only-key" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => expect(api.updateTunnelConfig).toHaveBeenCalledWith({ action: "save", tunnelId: "tunnel_saved", apiKey: "test-only-key" }));
    expect(key).toHaveValue("");
    await screen.findByText("当前来源：本机配置文件（优先）");
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => expect(api.updateTunnelConfig).toHaveBeenLastCalledWith({ action: "save", tunnelId: "tunnel_saved", apiKey: null }));
    await waitFor(() => expect(screen.getByRole("button", { name: "清除已保存配置，改用环境变量" })).toBeEnabled());
    api.updateTunnelConfig.mockResolvedValue(readyState);
    fireEvent.click(screen.getByRole("button", { name: "清除已保存配置，改用环境变量" }));
    await waitFor(() => expect(api.updateTunnelConfig).toHaveBeenLastCalledWith({ action: "use_environment" }));
    expect(await screen.findByText("当前来源：Desktop 进程的环境变量")).toBeInTheDocument();
  });

  it("clears submitted keys on save failure and never submits runtime setup from a credential input", async () => {
    api.getState.mockResolvedValue(firstRunState);
    api.updateTunnelConfig.mockRejectedValue({ code: "tunnel_config_save_failed", message: "Could not save", next_action: "Retry." });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: /在此电脑使用 WebCodex/ }));
    const tunnelInput = screen.getByLabelText("Tunnel ID");
    fireEvent.change(tunnelInput, { target: { value: "tunnel_test" } });
    const key = screen.getByLabelText("Tunnel API key");
    fireEvent.change(key, { target: { value: "test-only-key" } });
    const event = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    key.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(api.configureLocal).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "保存配置", hidden: true }));
    expect(await screen.findByRole("alert", { hidden: true })).toBeInTheDocument();
    expect(key).toHaveValue("");
    expect(tunnelInput).toHaveValue("tunnel_test");
  });

  it("supports keyboard navigation without intercepting activity search typing", async () => {
    api.getState.mockResolvedValue(readyState);
    api.activity.mockResolvedValue([
      { sequence: 1, timestamp_ms: 1, source: "runner", level: "info", event_kind: "process_started", message: "" },
      { sequence: 2, timestamp_ms: 2, source: "service", level: "error", event_kind: "process_exited", message: "" },
    ]);
    renderApp();
    await screen.findByRole("button", { name: "活动" });
    const language = screen.getByRole("combobox", { name: "界面语言" });
    language.focus();
    fireEvent.keyDown(language, { key: "4", metaKey: true });
    const search = screen.getByRole("searchbox");
    await waitFor(() => expect(screen.getAllByRole("article")).toHaveLength(1));
    fireEvent.click(screen.getByRole("checkbox", { name: "显示进程详情" }));
    expect(screen.getAllByRole("article")).toHaveLength(2);
    fireEvent.click(screen.getByRole("checkbox", { name: "只看警告和错误" }));
    expect(screen.getAllByRole("article")).toHaveLength(1);
    fireEvent.change(search, { target: { value: "no matching source" } });
    expect(screen.queryByRole("article")).not.toBeInTheDocument();
    expect(screen.getByText("没有匹配的活动，请调整搜索或筛选条件。")).toBeInTheDocument();
    fireEvent.keyDown(search, { key: "a", ctrlKey: true });
    expect(screen.getByRole("searchbox")).toBeInTheDocument();
    fireEvent.change(search, { target: { value: "" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "只看警告和错误" }));
    expect(screen.getAllByRole("article")).toHaveLength(2);
    fireEvent.keyDown(window, { key: "2", ctrlKey: true });
    expect(screen.getByRole("button", { name: "项目" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("main")).toHaveFocus();
  });

  it("shows named projects without switching the local project or restarting an active Tunnel", async () => {
    const tunneledState: DesktopState = {
      ...readyState,
      topology: { ...readyState.topology!, exposure: { kind: "open_ai_tunnel" } },
      regular_tunnel: {
        provider: "openai",
        status: "ready",
        clipboard_state: "copied",
        clipboard_contains: "tunnel_id",
        ready_for_chatgpt: true,
      },
      preferred_connection: "open_ai_tunnel",
    };
    const projectC = {
      path: "C:\\work\\next",
      allowed_root: "C:\\work\\next",
      is_git_repository: true,
      runtime_project_id: "agent:desktop:next",
    };
    api.getState.mockResolvedValue(tunneledState);
    api.observeChatgptActivity.mockResolvedValue(tunneledState);
    api.listNamedProjects.mockResolvedValue({ host_id: "fixture-host", revision: 2, projects: [
      { id: "wc_workspace_next", name: "Next", enabled: true, folders: [
        { project: projectC.runtime_project_id, name: "Next", path: projectC.path },
      ], team_project: projectC.runtime_project_id },
    ] });
    api.listProjects.mockResolvedValue({ can_manage: true, projects: [
      { id: readyState.project!.runtime_project_id, name: "Current", path: readyState.project!.path, revision: "sha256:current", enabled: true, connected: true, agent_status: "online", active_jobs: 0 },
      { id: projectC.runtime_project_id, name: "Next", path: projectC.path, revision: "sha256:next", enabled: true, connected: true, agent_status: "online", active_jobs: 0 },
    ] });

    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "项目" }));
    expect(screen.getByRole("heading", { level: 1, name: "我的项目" })).toBeInTheDocument();
    expect(screen.getByRole("main")).toHaveFocus();
    expect(await screen.findByText(projectC.path)).toBeVisible();
    expect(api.listNamedProjects).toHaveBeenCalled();
    expect(api.switchProject).not.toHaveBeenCalled();
    expect(api.configureLocal).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "设为当前" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /在此电脑使用 WebCodex/ })).not.toBeInTheDocument();
    expect(screen.getByText("Tunnel 已就绪，等待 ChatGPT")).toBeInTheDocument();
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
  });

  it("keeps named project management disabled until the local runtime is ready", async () => {
    const noProject: DesktopState = { ...readyState, project: null, readiness: { ...readyState.readiness, server: "stopped", runner: "stopped", runtime_ready: false, ready_for_chatgpt: false } };
    api.getState.mockResolvedValue(noProject);
    api.observeChatgptActivity.mockResolvedValue(noProject);

    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "项目" }));
    expect(screen.getByRole("heading", { name: "我的项目" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "新建项目" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "刷新列表" })).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent("无需先创建或选择项目，即可启动 Server 和 Runner。");
    expect(screen.getByRole("button", { name: "启动服务" })).toBeEnabled();
    expect(api.listNamedProjects).not.toHaveBeenCalled();
    expect(api.activateLocalProject).not.toHaveBeenCalled();
    expect(api.configureLocal).not.toHaveBeenCalled();
  });



  it("does not start a duplicate Tunnel when local setup runs while one is already active", async () => {
    const tunneledState: DesktopState = {
      ...readyState,
      topology: { ...readyState.topology!, exposure: { kind: "open_ai_tunnel" } },
      regular_tunnel: {
        provider: "openai",
        status: "ready",
        clipboard_state: "copied",
        clipboard_contains: "tunnel_id",
        ready_for_chatgpt: true,
      },
      preferred_connection: "open_ai_tunnel",
    };
    api.getState.mockResolvedValue(tunneledState);
    api.observeChatgptActivity.mockResolvedValue(tunneledState);
    api.configureLocal.mockResolvedValue(tunneledState);

    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    fireEvent.click(screen.getByRole("button", { name: "更改运行方式" }));
    fireEvent.click(screen.getByRole("button", { name: /在此电脑使用 WebCodex/ }));
    expect(screen.queryByRole("checkbox", { name: "配置完成后连接 ChatGPT" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "配置 WebCodex" }));

    await waitFor(() => expect(api.configureLocal).toHaveBeenCalledWith());
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
    expect(await screen.findByRole("heading", { level: 1, name: "WebCodex" })).toBeInTheDocument();
  });

  it("navigates by accessible role/name and marks the current page", async () => {
    api.getState.mockResolvedValue(readyState);
    api.refresh.mockResolvedValue(readyState);
    renderApp();

    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    const home = screen.getByRole("button", { name: "首页" });
    const connection = screen.getByRole("button", { name: "连接" });
    expect(home).toHaveAttribute("aria-current", "page");
    expect(screen.getAllByRole("status")).toHaveLength(1);

    fireEvent.click(connection);
    expect(await screen.findByRole("heading", { level: 1, name: "ChatGPT 连接" })).toBeInTheDocument();
    expect(connection).toHaveAttribute("aria-current", "page");
    expect(home).not.toHaveAttribute("aria-current");

    expect(screen.getByRole("radiogroup", { name: "连接方式" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /OpenAI Secure Tunnel/ })).not.toBeChecked();
    expect(screen.queryByRole("radio", { name: /Cloudflare/ })).not.toBeInTheDocument();
  });

  it("separates observed ChatGPT use from Desktop-managed tunnel state", async () => {
    const observed: DesktopState = {
      ...readyState,
      chatgpt_activity: {
        observed: true,
        last_meaningful_activity_at_ms: 1_234,
      },
    };
    api.getState.mockResolvedValue(observed);
    api.refresh.mockResolvedValue(observed);
    renderApp();

    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    const connectionCard = screen.getByText("ChatGPT 连接").closest("article");
    expect(connectionCard).not.toBeNull();
    expect(connectionCard!.querySelector(".status-dot")).toHaveClass("ready");
    expect(within(connectionCard!).getByText("已验证 ChatGPT 使用")).toBeInTheDocument();
    expect(connectionCard).toHaveTextContent("已观察到 ChatGPT 对当前项目的真实 WebCodex 调用。");
    expect(screen.queryByText("暂不连接 ChatGPT")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "连接" }));
    expect(await screen.findByRole("heading", { level: 1, name: "ChatGPT 连接" })).toBeInTheDocument();
    expect(screen.getByText("已验证 ChatGPT 使用")).toBeInTheDocument();
    expect(screen.getByText(/连接方式与 Desktop 管理的 Tunnel 状态分开显示/)).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /不由 Desktop 管理连接/ })).toBeChecked();
  });

  it("does not equate an unmanaged Desktop tunnel with ChatGPT being disconnected", async () => {
    api.getState.mockResolvedValue(readyState);
    api.refresh.mockResolvedValue(readyState);
    renderApp();

    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    const connectionCard = screen.getByText("ChatGPT 连接").closest("article");
    expect(connectionCard).not.toBeNull();
    expect(connectionCard).toHaveTextContent("不由 Desktop 管理连接");
    expect(connectionCard).toHaveTextContent("这不代表 ChatGPT 一定不可用");
    expect(connectionCard).not.toHaveTextContent("暂不连接 ChatGPT");
  });

  it("rechecks ChatGPT use when the user returns to an already-open Desktop", async () => {
    vi.useFakeTimers();
    try {
      const observed: DesktopState = {
        ...readyState,
        chatgpt_activity: {
          observed: true,
          last_meaningful_activity_at_ms: 5_000,
        },
      };
      let serverObserved = false;
      api.getState.mockImplementation(async () => serverObserved ? observed : readyState);
      api.observeChatgptActivity.mockImplementation(async () => serverObserved ? observed : readyState);
      const view = renderApp();
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(api.observeChatgptActivity).toHaveBeenCalledTimes(1);
      expect(screen.queryByText("已验证 ChatGPT 使用")).not.toBeInTheDocument();

      await act(async () => {
        window.dispatchEvent(new Event("blur"));
        await Promise.resolve();
      });
      serverObserved = true;
      await act(async () => {
        window.dispatchEvent(new Event("focus"));
        await Promise.resolve();
        await Promise.resolve();
      });

      expect(api.observeChatgptActivity).toHaveBeenCalledTimes(2);
      const connectionCard = screen.getByText("ChatGPT 连接").closest("article");
      expect(connectionCard).not.toBeNull();
      expect(within(connectionCard!).getByText("已验证 ChatGPT 使用")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(60_000);
      });
      expect(api.observeChatgptActivity).toHaveBeenCalledTimes(2);
      view.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("surfaces Tunnel configuration as presence-only current-process diagnostics", async () => {
    const missingKey: DesktopState = {
      ...readyState,
      openai_tunnel_configured: false,
      openai_tunnel_config: {
        tunnel_id_present: true,
        api_key_present: false,
        source: "environment",
        saved_tunnel_id: null,
      },
    };
    const missingId: DesktopState = {
      ...missingKey,
      openai_tunnel_config: {
        tunnel_id_present: false,
        api_key_present: true,
        source: "environment",
        saved_tunnel_id: null,
      },
    };
    const bothMissing: DesktopState = {
      ...missingKey,
      openai_tunnel_config: {
        tunnel_id_present: false,
        api_key_present: false,
        source: "environment",
        saved_tunnel_id: null,
      },
    };
    const configured: DesktopState = {
      ...missingKey,
      openai_tunnel_configured: true,
      openai_tunnel_config: {
        tunnel_id_present: true,
        api_key_present: true,
        source: "environment",
        saved_tunnel_id: null,
      },
    };
    api.getState.mockResolvedValue(missingKey);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    fireEvent.click(screen.getByRole("button", { name: "连接" }));

    const diagnostics = screen.getByText("OpenAI Tunnel 配置检测").closest("article");
    expect(diagnostics).not.toBeNull();
    expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel ID").parentElement).toHaveTextContent("已检测");
    expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel API key").parentElement).toHaveTextContent("未检测");
    expect(within(diagnostics!).getByText(/关闭窗口只会隐藏到托盘\/菜单栏/)).toBeInTheDocument();
    expect(diagnostics).not.toHaveTextContent("CONTROL_PLANE_API_KEY");

    api.getState.mockResolvedValueOnce(missingId);
    fireEvent.click(within(diagnostics!).getByRole("button", { name: "重新检测配置" }));
    await waitFor(() => {
      expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel ID").parentElement).toHaveTextContent("未检测");
      expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel API key").parentElement).toHaveTextContent("已检测");
    });

    api.getState.mockResolvedValueOnce(bothMissing);
    fireEvent.click(within(diagnostics!).getByRole("button", { name: "重新检测配置" }));
    await waitFor(() => {
      expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel ID").parentElement).toHaveTextContent("未检测");
      expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel API key").parentElement).toHaveTextContent("未检测");
    });

    api.getState.mockResolvedValueOnce(configured);
    fireEvent.click(within(diagnostics!).getByRole("button", { name: "重新检测配置" }));
    await waitFor(() => {
      expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel ID").parentElement).toHaveTextContent("已检测");
      expect(within(diagnostics!.querySelector("dl")!).getByText("Tunnel API key").parentElement).toHaveTextContent("已检测");
    });
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
  });

  it("navigates to existing Activity and Settings pages from the tray host event", async () => {
    api.getState.mockResolvedValue(readyState);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    await waitFor(() => expect(tauriEvents.handler).not.toBeNull());

    act(() => {
      tauriEvents.handler?.({ payload: "settings" });
    });
    expect(await screen.findByRole("heading", { level: 1, name: "Desktop 设置" })).toBeInTheDocument();

    act(() => {
      tauriEvents.handler?.({ payload: "activity" });
    });
    expect(await screen.findByRole("heading", { level: 1, name: "最近的运行活动" })).toBeInTheDocument();
    await waitFor(() => expect(api.activity).toHaveBeenCalled());

    act(() => {
      tauriEvents.handler?.({ payload: "https://example.invalid" });
    });
    expect(screen.getByRole("button", { name: "活动" })).toHaveAttribute("aria-current", "page");
  });

  it("reads and updates Launch at Login through the narrow Desktop host API", async () => {
    api.getState.mockResolvedValue(readyState);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    fireEvent.click(screen.getByRole("button", { name: "设置" }));

    const launchAtLogin = await screen.findByRole("checkbox", { name: "登录时启动 WebCodex" });
    await waitFor(() => expect(launchAtLogin).toBeEnabled());
    expect(launchAtLogin).not.toBeChecked();
    expect(screen.getByText(/WebCodex 会在后台启动/)).toBeInTheDocument();

    fireEvent.click(launchAtLogin);
    await waitFor(() => expect(api.setLaunchAtLogin).toHaveBeenCalledWith(true));
    await waitFor(() => expect(launchAtLogin).toBeChecked());
  });

  it("starts and connects a fresh Desktop with zero projects and no folder grant", async () => {
    const noProject: DesktopState = { ...readyState, project: null, readiness: { ...readyState.readiness, project: "none" } };
    api.getState.mockResolvedValue(firstRunState);
    api.configureLocal.mockResolvedValue(noProject);
    api.startRegularTunnel.mockResolvedValue({ ...noProject, regular_tunnel: { provider: "openai", status: "ready", ready_for_chatgpt: true, clipboard_state: "copied", clipboard_contains: "tunnel_id" } });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: /在此电脑使用 WebCodex/ }));
    expect(screen.queryByRole("button", { name: "选择文件夹" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "配置 WebCodex" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "配置 WebCodex" }));
    await waitFor(() => expect(api.startRegularTunnel).toHaveBeenCalledTimes(1));
    expect(api.configureLocal).toHaveBeenCalledWith();
    expect(open).not.toHaveBeenCalled();
    expect(api.addProject).not.toHaveBeenCalled();
    expect(await screen.findByRole("heading", { level: 1, name: "WebCodex" })).toBeInTheDocument();
  });

  it("still requires an explicit project for Quick Share", async () => {
    api.getState.mockResolvedValue(firstRunState);
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: /快速共享项目/ }));
    expect(screen.getByRole("button", { name: "选择文件夹" })).toBeInTheDocument();
    fireEvent.submit(document.querySelector("form")!);
    expect(api.startQuickShare).not.toHaveBeenCalled();
  });

  it("guides Windows users to PowerShell 7 without blocking the 5.1 fallback", async () => {
    const missingPwsh: DesktopState = {
      ...firstRunState,
      powershell_runtime: {
        pwsh_available: false,
        windows_powershell_available: true,
      },
    };
    const detectedPwsh: DesktopState = {
      ...missingPwsh,
      powershell_runtime: {
        pwsh_available: true,
        windows_powershell_available: true,
      },
    };
    api.getState.mockResolvedValueOnce(missingPwsh).mockResolvedValueOnce(detectedPwsh);
    renderApp();

    fireEvent.click(await screen.findByRole("button", { name: /在此电脑使用 WebCodex/ }));
    const guidance = screen.getByText("建议安装 PowerShell 7").closest("article");
    expect(guidance).not.toBeNull();
    expect(guidance).toHaveTextContent("Windows PowerShell 5.1");
    expect(guidance).toHaveTextContent("winget install --id Microsoft.PowerShell --source winget");
    expect(screen.getByRole("button", { name: "配置 WebCodex" })).toBeEnabled();

    fireEvent.click(within(guidance!).getByRole("button", { name: "打开 Microsoft 安装说明" }));
    await waitFor(() => expect(api.openPowerShellInstallGuide).toHaveBeenCalledTimes(1));
    fireEvent.click(within(guidance!).getByRole("button", { name: "重新检测" }));
    await waitFor(() => expect(screen.queryByText("建议安装 PowerShell 7")).not.toBeInTheDocument());
    expect(api.configureLocal).not.toHaveBeenCalled();
  });

  it("keeps first-run service errors through intermediate polling and Tunnel failure", async () => {
    vi.useFakeTimers();
    try {
      const setupResult = deferred<DesktopState>();
      api.getState.mockResolvedValue(firstRunState);
      api.configureLocal.mockReturnValueOnce(setupResult.promise).mockResolvedValue(readyState);
      api.startRegularTunnel.mockRejectedValue({ code: "tunnel_unavailable", message: "Tunnel failed", next_action: "Retry." });
      vi.mocked(open).mockResolvedValue(readyState.project!.path);
      api.inspectProject.mockResolvedValue(readyState.project);
      const view = renderApp();
      await act(async () => {});
      fireEvent.click(screen.getByRole("button", { name: /在此电脑使用 WebCodex/ }));
      await act(async () => {});
      fireEvent.click(screen.getByRole("button", { name: "配置 WebCodex" }));
      expect(api.configureLocal).toHaveBeenCalledWith();

      api.getState.mockResolvedValue(localSetupOperationState());
      await act(async () => { await vi.advanceTimersByTimeAsync(1_500); });
      expect(screen.getByRole("heading", { level: 1, name: "在此电脑配置 WebCodex" })).toBeInTheDocument();
      expect(document.querySelector("[data-webcodex-action=configure-local]")).toBeDisabled();

      api.getState.mockResolvedValue(setupState());
      await act(async () => {
        setupResult.reject({ code: "server_unreachable", message: "Service failed", next_action: "Retry." });
        await vi.advanceTimersByTimeAsync(1_000);
      });
      expect(screen.getByRole("alert")).toHaveTextContent("server_unreachable");
      fireEvent.click(screen.getByRole("button", { name: "配置 WebCodex" }));
      await act(async () => {});
      expect(screen.getByRole("alert")).toHaveTextContent("tunnel_unavailable");
      expect(screen.getByRole("heading", { level: 1, name: "在此电脑配置 WebCodex" })).toBeInTheDocument();
      api.startRegularTunnel.mockResolvedValue(readyState);
      fireEvent.click(screen.getByRole("button", { name: "配置 WebCodex" }));
      await act(async () => {});
      expect(screen.getByRole("heading", { level: 1, name: "WebCodex" })).toBeInTheDocument();
      view.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows an initial status failure and retries the complete fresh-start bootstrap", async () => {
    const retryState = deferred<DesktopState>();
    api.getState
      .mockRejectedValueOnce({
        code: "desktop_state_unavailable",
        message: "Desktop status is temporarily unavailable",
        next_action: "Retry reading the Desktop status.",
      })
      .mockReturnValueOnce(retryState.promise)
      .mockResolvedValue(readyState);
    renderApp();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("desktop_state_unavailable");
    expect(screen.queryByText("正在加载 WebCodex…")).not.toBeInTheDocument();
    expect(api.configureLocal).not.toHaveBeenCalled();
    expect(api.resumeSavedRuntime).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("status")).toHaveTextContent("正在加载 WebCodex…");
    expect(screen.queryByRole("button", { name: "重试" })).not.toBeInTheDocument();
    await waitFor(() => expect(api.getState).toHaveBeenCalledTimes(2));

    await act(async () => {
      retryState.resolve(firstRunState);
    });
    expect(await screen.findByRole("button", { name: /在此电脑使用 WebCodex/ })).toBeInTheDocument();
    expect(api.configureLocal).not.toHaveBeenCalled();
    expect(api.resumeSavedRuntime).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("keeps repeated initial status failures visible without automatically retrying", async () => {
    api.getState.mockRejectedValue({
      code: "desktop_state_unavailable",
      message: "Desktop status is unavailable",
      next_action: "Retry reading the Desktop status.",
    });
    renderApp();

    expect(await screen.findByRole("alert")).toHaveTextContent("desktop_state_unavailable");
    expect(api.getState).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("desktop_state_unavailable");
    expect(screen.getByRole("button", { name: "重试" })).toBeEnabled();
    expect(api.getState).toHaveBeenCalledTimes(2);
    expect(api.configureLocal).not.toHaveBeenCalled();
    expect(api.resumeSavedRuntime).not.toHaveBeenCalled();
  });

  it("renders localized failures as an alert while keeping safe diagnostics available", async () => {
    const setupState: DesktopState = {
      ...readyState,
      readiness: {
        ...readyState.readiness,
        runtime_ready: false,
        ready_for_chatgpt: false,
        server: "stopped",
        runner: "stopped",
        project: "configured",
        exposure: "disabled",
        summary_kind: "service_needs_attention",
        next_action_kind: "start_or_reconnect_service",
      },
    };
    api.getState.mockResolvedValue(setupState);
    api.refresh.mockResolvedValue(setupState);
    api.resumeSavedRuntime.mockRejectedValue({
      code: "server_unreachable",
      message: "WebCodex Service did not become ready",
      next_action: "Check diagnostics.",
    });

    renderApp();
    const submit = await screen.findByRole("button", { name: "启动" });
    fireEvent.click(submit);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("WebCodex 服务不可用");
    expect(alert).toHaveTextContent("server_unreachable");
  });

  it("retries service setup without selecting a project", async () => {
    api.getState.mockResolvedValue(readyState);
    api.configureLocal.mockRejectedValueOnce({ code: "server_unreachable", message: "Service failed", next_action: "Retry." }).mockResolvedValueOnce(readyState);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    fireEvent.click(screen.getByRole("button", { name: "更改运行方式" }));
    fireEvent.click(screen.getByRole("button", { name: /在此电脑使用 WebCodex/ }));
    fireEvent.click(screen.getByRole("button", { name: "配置 WebCodex" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("server_unreachable");
    fireEvent.click(screen.getByRole("button", { name: "配置 WebCodex" }));
    await waitFor(() => expect(api.configureLocal).toHaveBeenCalledTimes(2));
    expect(api.inspectProject).not.toHaveBeenCalled();
  });

  it("uses the backend restart_quick_share presentation identity after a stopped share", async () => {
    const stoppedShare: DesktopState = {
      ...readyState,
      topology: {
        experience: "quick_share",
        server: { kind: "local" },
        runner: { kind: "local" },
        exposure: { kind: "cloudflare" },
        enrollment: { kind: "existing_profile", profile: "temporary_share" },
      },
      readiness: {
        server: "stopped",
        runner: "stopped",
        exposure: "error",
        project: "configured",
        runtime_ready: false,
        ready_for_chatgpt: false,
        summary: "Quick Share stopped",
        summary_kind: "quick_share_stopped",
        next_action: "Start Quick Share again.",
        next_action_kind: "restart_quick_share",
      },
      quick_share: {
        provider: "cloudflare",
        project: "C:\\fixture\\repo",
        mcp_url: null,
        clipboard_state: "unavailable",
        clipboard_contains: "bearer_credential",
        ready_for_chatgpt: false,
      },
    };
    api.getState.mockResolvedValue(stoppedShare);
    api.refresh.mockResolvedValue(stoppedShare);
    renderApp();

    expect(await screen.findByText("重新启动 Quick Share。")).toBeInTheDocument();
  });

  it("does not offer a duplicate start when the regular tunnel is running but handoff is degraded", async () => {
    const degradedTunnel: DesktopState = {
      ...readyState,
      topology: {
        ...readyState.topology!,
        exposure: { kind: "open_ai_tunnel" },
      },
      readiness: {
        ...readyState.readiness,
        exposure: "degraded",
        ready_for_chatgpt: false,
        summary: "ChatGPT connection is not verified",
        summary_kind: "connection_unverified",
        next_action: "Restore clipboard access, then restart the secure tunnel handoff.",
        next_action_kind: "restore_clipboard_handoff",
      },
      regular_tunnel: {
        provider: "openai",
        status: "ready",
        clipboard_state: "unavailable",
        clipboard_contains: "tunnel_id",
        ready_for_chatgpt: false,
      },
    };
    api.getState.mockResolvedValue(degradedTunnel);
    api.refresh.mockResolvedValue(degradedTunnel);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });

    fireEvent.click(screen.getByRole("button", { name: "连接" }));
    expect(await screen.findByText("安全隧道正在运行，但连接信息仍需要处理")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "启动安全隧道" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "停止安全隧道" })).toBeInTheDocument();
  });

  it("keeps navigation and Activity usable while a pending setup is globally observable", async () => {
    vi.useFakeTimers();
    try {
      const initial = setupState();
      const running = localSetupOperationState("running", 1);
      const runningWithNewActivity = localSetupOperationState("running", 2);
      const setupResult = deferred<DesktopState>();
      api.getState
        .mockResolvedValueOnce(initial)
        .mockResolvedValueOnce(running)
        .mockResolvedValueOnce(runningWithNewActivity)
        .mockResolvedValue(runningWithNewActivity);
      api.refresh.mockResolvedValue(initial);
      api.resumeSavedRuntime.mockReturnValue(setupResult.promise);
      api.activity.mockResolvedValue([]);

      const view = renderApp();
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      });

      fireEvent.click(screen.getByRole("button", { name: "启动" }));
      expect(api.resumeSavedRuntime).toHaveBeenCalledTimes(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_500);
      });

      const operationStatus = screen.getByRole("status", { name: "当前 Desktop 操作" });
      expect(operationStatus).toHaveTextContent("正在配置本机 WebCodex");
      expect(operationStatus).toHaveTextContent("停止当前操作不会自动重复执行尚未确认的步骤");

      fireEvent.click(screen.getByRole("button", { name: "活动" }));
      expect(screen.getByRole("heading", { level: 1, name: "最近的运行活动" })).toBeInTheDocument();
      expect(api.activity).toHaveBeenCalledTimes(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_000);
      });
      expect(api.activity).toHaveBeenCalledTimes(2);

      setupResult.resolve(readyState);
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(screen.queryByRole("status", { name: "当前 Desktop 操作" })).not.toBeInTheDocument();
      view.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("cancels only the observed operation id and presents the cancelling phase", async () => {
    const running = localSetupOperationState("running", 1);
    const cancelling = localSetupOperationState("cancelling", 2);
    api.getState.mockResolvedValueOnce(running).mockResolvedValue(cancelling);
    api.cancelOperation.mockResolvedValue(cancelling);

    renderApp();
    const cancel = await screen.findByRole("button", { name: "停止当前操作" });
    fireEvent.click(cancel);

    await waitFor(() => {
      expect(api.cancelOperation).toHaveBeenCalledWith("desktop-operation-a");
    });
    const cancellingStatus = screen.getByRole("status", { name: "当前 Desktop 操作" });
    expect(cancellingStatus).toHaveTextContent("正在停止当前操作…");
    const disabledCancel = screen.getByRole("button", { name: "正在停止当前操作…" });
    expect(disabledCancel).toBeDisabled();
    fireEvent.click(disabledCancel);
    expect(api.cancelOperation).toHaveBeenCalledTimes(1);
  });

  it("does not let an older polling response resurrect a completed operation", async () => {
    vi.useFakeTimers();
    try {
      const running = localSetupOperationState("running", 1);
      const stalePoll = deferred<DesktopState>();
      const terminal = { ...readyState, current_operation: null, activity_sequence: 2 };
      api.getState
        .mockResolvedValueOnce(running)
        .mockReturnValueOnce(stalePoll.promise)
        .mockResolvedValue(terminal);
      api.cancelOperation.mockResolvedValue(terminal);

      const view = renderApp();
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(screen.getByRole("status", { name: "当前 Desktop 操作" })).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_000);
      });
      expect(api.getState).toHaveBeenCalledTimes(2);

      fireEvent.click(screen.getByRole("button", { name: "停止当前操作" }));
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(screen.queryByRole("status", { name: "当前 Desktop 操作" })).not.toBeInTheDocument();

      stalePoll.resolve(running);
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(screen.queryByRole("status", { name: "当前 Desktop 操作" })).not.toBeInTheDocument();
      view.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("never promotes a locally ready Regular Tunnel to ChatGPT-connected state", async () => {
    vi.useFakeTimers();
    try {
      const locallyReadyTunnel: DesktopState = {
        ...readyState,
        topology: {
          ...readyState.topology!,
          exposure: { kind: "open_ai_tunnel" },
        },
        readiness: {
          ...readyState.readiness,
          exposure: "local_ready",
          ready_for_chatgpt: false,
          summary: "OpenAI Secure Tunnel is ready; waiting for ChatGPT to connect",
          summary_kind: "tunnel_ready_waiting_for_chat_gpt",
          next_action: "Connect the Tunnel in ChatGPT, then verify it with one real project read.",
          next_action_kind: "check_connection",
        },
        regular_tunnel: {
          provider: "openai",
          status: "ready",
          clipboard_state: "copied",
          clipboard_contains: "tunnel_id",
          ready_for_chatgpt: true,
        },
      };
      const failedTunnel: DesktopState = {
        ...locallyReadyTunnel,
        readiness: {
          ...locallyReadyTunnel.readiness,
          exposure: "error",
          ready_for_chatgpt: false,
          summary: "ChatGPT connection is not verified",
          summary_kind: "connection_unverified",
          next_action: "Restart the secure tunnel.",
          next_action_kind: "restart_secure_tunnel",
        },
        regular_tunnel: {
          ...locallyReadyTunnel.regular_tunnel!,
          status: "error",
          ready_for_chatgpt: false,
        },
      };

      api.getState
        .mockResolvedValueOnce(locallyReadyTunnel)
        .mockResolvedValueOnce(failedTunnel);
      api.refresh.mockResolvedValue(locallyReadyTunnel);
      const view = renderApp();
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      });

      const readyStatus = screen.getByRole("status");
      expect(readyStatus).toHaveTextContent("OpenAI Secure Tunnel 已就绪，等待 ChatGPT 连接");
      expect(readyStatus).not.toHaveTextContent("可以使用");
      expect(screen.queryByText("外部连接已验证")).not.toBeInTheDocument();
      expect(screen.getByText("Tunnel 已就绪，等待 ChatGPT")).toBeInTheDocument();
      const connectionCard = screen.getByText("ChatGPT 连接").closest("article");
      expect(connectionCard).not.toBeNull();
      expect(connectionCard!.querySelector(".status-dot")).toHaveClass("pending");
      expect(api.getState).toHaveBeenCalledTimes(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_500);
      });

      expect(api.getState).toHaveBeenCalledTimes(2);
      expect(api.refresh).toHaveBeenCalledTimes(0);
      const failedStatus = screen.getByRole("status");
      expect(failedStatus).toHaveTextContent("ChatGPT 连接尚未验证");
      expect(failedStatus).toHaveTextContent("重新启动安全隧道。");
      expect(screen.queryByText("外部连接已验证")).not.toBeInTheDocument();
      expect(screen.getAllByText("ChatGPT 使用尚未验证").length).toBeGreaterThan(0);

      view.unmount();
      const callsAfterUnmount = api.getState.mock.calls.length;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
      });
      expect(api.getState).toHaveBeenCalledTimes(callsAfterUnmount);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps regular tunnel controls off a remote Server connection page", async () => {
    const remote: DesktopState = {
      ...readyState,
      topology: {
        experience: "full",
        server: { kind: "remote", url: "https://server.example.test" },
        runner: { kind: "local" },
        exposure: { kind: "existing_https", url: "https://server.example.test" },
        enrollment: { kind: "managed_pairing" },
      },
      readiness: {
        ...readyState.readiness,
        exposure: "unknown",
        ready_for_chatgpt: false,
        summary_kind: "connection_unverified",
        next_action_kind: "check_connection",
      },
    };
    api.getState.mockResolvedValue(remote);
    api.refresh.mockResolvedValue(remote);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });

    fireEvent.click(screen.getByRole("button", { name: "连接" }));
    expect(await screen.findByRole("heading", { level: 2, name: "远程 WebCodex Server" })).toBeInTheDocument();
    expect(screen.getByText("由远程 Server 管理")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "启动安全隧道" })).not.toBeInTheDocument();
  });

  it("leaves automatic saved runtime recovery to native Desktop startup", async () => {
    const stopped = { ...setupState(), runtime_autostart: true };
    api.getState.mockResolvedValue(stopped);
    api.resumeSavedRuntime.mockResolvedValue(readyState);

    renderApp();

    expect(await screen.findByRole("heading", { level: 1, name: "WebCodex" })).toBeInTheDocument();
    expect(api.resumeSavedRuntime).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "配置 WebCodex" })).not.toBeInTheDocument();
  });

  it("does not race native startup when a remembered OpenAI Tunnel exists", async () => {
    const stopped: DesktopState = {
      ...setupState(),
      runtime_autostart: true,
      preferred_connection: "open_ai_tunnel",
    };
    const resumed: DesktopState = {
      ...readyState,
      runtime_autostart: true,
      preferred_connection: "open_ai_tunnel",
    };
    api.getState.mockResolvedValue(stopped);
    api.resumeSavedRuntime.mockResolvedValue(resumed);
    api.startRegularTunnel.mockResolvedValue(resumed);

    renderApp();

    await screen.findByRole("heading", { level: 1, name: "WebCodex" });
    expect(api.resumeSavedRuntime).not.toHaveBeenCalled();
    expect(api.startRegularTunnel).not.toHaveBeenCalled();
  });

  it("reuses an existing remote enrollment without selecting a project", async () => {
    const remoteState: DesktopState = {
      ...readyState,
      topology: {
        experience: "full",
        server: { kind: "remote", url: "https://server.example.test" },
        runner: { kind: "local" },
        exposure: { kind: "existing_https", url: "https://server.example.test" },
        enrollment: { kind: "managed_pairing" },
      },
      project: {
        path: "C:\\fixture\\project-a",
        allowed_root: "C:\\fixture\\project-a",
        is_git_repository: true,
        runtime_project_id: "agent:desktop:project-a",
      },
    };
    const projectB = {
      path: "C:\\fixture\\project-b",
      allowed_root: "C:\\fixture\\project-b",
      is_git_repository: true,
      runtime_project_id: null,
    };
    api.getState.mockResolvedValue(remoteState);
    api.inspectProject.mockResolvedValue(projectB);
    api.configureRemote.mockResolvedValue({
      ...remoteState,
      project: { ...projectB, runtime_project_id: "agent:desktop:project-b" },
    });
    vi.mocked(open).mockResolvedValue(projectB.path);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });

    fireEvent.click(screen.getByRole("button", { name: "更改运行方式" }));
    fireEvent.click(screen.getByRole("button", { name: /连接现有 Server/ }));
    expect(screen.getByText("将复用现有连接")).toBeInTheDocument();
    expect(screen.queryByLabelText("一次性登录码")).not.toBeInTheDocument();

    expect(screen.getByText("将复用现有连接")).toBeInTheDocument();
    expect(screen.queryByLabelText("一次性登录码")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重新连接电脑" }));
    await waitFor(() =>
      expect(api.configureRemote).toHaveBeenCalledWith(
        "https://server.example.test",
        "",
        "",
      ),
    );
  });

  it("exposes pairing recovery when a saved remote enrollment is no longer reusable", async () => {
    const remoteState: DesktopState = {
      ...readyState,
      topology: {
        experience: "full",
        server: { kind: "remote", url: "https://server.example.test" },
        runner: { kind: "local" },
        exposure: { kind: "existing_https", url: "https://server.example.test" },
        enrollment: { kind: "managed_pairing" },
      },
      project: {
        path: "C:\\fixture\\project-a",
        allowed_root: "C:\\fixture\\project-a",
        is_git_repository: true,
        runtime_project_id: "agent:desktop:project-a",
      },
    };
    const projectB = {
      path: "C:\\fixture\\project-b",
      allowed_root: "C:\\fixture\\project-b",
      is_git_repository: true,
      runtime_project_id: null,
    };
    api.getState.mockResolvedValue(remoteState);
    api.inspectProject.mockResolvedValue(projectB);
    api.configureRemote
      .mockRejectedValueOnce({
        code: "pairing_code_invalid",
        message: "The saved Runner identity is not reusable and no new WebCodex pairing code was provided",
        next_action: "Refresh this Runner connection with a new code.",
      })
      .mockResolvedValueOnce({
        ...remoteState,
        project: { ...projectB, runtime_project_id: "agent:desktop:project-b" },
      });
    vi.mocked(open).mockResolvedValue(projectB.path);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });

    fireEvent.click(screen.getByRole("button", { name: "更改运行方式" }));
    fireEvent.click(screen.getByRole("button", { name: /连接现有 Server/ }));
    expect(screen.queryByLabelText("一次性登录码")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重新连接电脑" }));
    expect(await screen.findByLabelText("一次性登录码")).toBeInTheDocument();
    expect(screen.queryByText("将复用现有连接")).not.toBeInTheDocument();
    const submit = screen.getByRole("button", { name: "连接电脑" });
    expect(submit).toBeDisabled();

    fireEvent.change(screen.getByLabelText("一次性登录码"), {
      target: { value: "wc_pair_recovery" },
    });
    expect(submit).toBeEnabled();
    fireEvent.click(submit);
    await waitFor(() =>
      expect(api.configureRemote).toHaveBeenLastCalledWith(
        "https://server.example.test",
        "wc_pair_recovery",
        "",
      ),
    );
  });

  it("keeps Remote Server as a first-class runtime choice after local setup", async () => {
    api.getState.mockResolvedValue(readyState);
    renderApp();
    await screen.findByRole("heading", { level: 1, name: "WebCodex" });

    fireEvent.click(screen.getByRole("button", { name: "更改运行方式" }));

    expect(await screen.findByRole("button", { name: /连接现有 Server/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /在此电脑使用 WebCodex/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /快速共享项目/ })).toBeInTheDocument();
  });
});
