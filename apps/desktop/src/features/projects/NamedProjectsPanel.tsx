import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { desktopApi } from "../../lib/desktop-api";
import type { DesktopState, RegisteredProject } from "../../models/topology";
import { ProjectTeamSettings } from "./ProjectTeamSettings";
import "./named-projects.css";

export interface NamedProject { id: string; name: string; enabled: boolean; folders: { project: string; name: string; path: string }[]; team_project?: string | null }
export interface WorkspaceCatalog { host_id: string; revision: number; projects: NamedProject[] }
export interface WorkspaceDraft { id: string; name: string; enabled: boolean; folders: string[]; team_project?: string | null }

type Api = Pick<typeof desktopApi, "listNamedProjects" | "saveNamedProjects" | "listProjects" | "inspectProject" | "addProject" | "mutateProject">;
type CurrentOperation = () => boolean;
const draftOf = (project: NamedProject): WorkspaceDraft => ({ id: project.id, name: project.name, enabled: project.enabled, folders: project.folders.map(folder => folder.project), team_project: project.team_project });
const displayPath = (path: string) => path.replace(/^\\\\\?\\/, "");
function comparablePath(path: string) {
  const clean = displayPath(path).replace(/\\/g, "/");
  return (/^[A-Za-z]:\//.test(clean) || clean.startsWith("//") ? clean.toLowerCase() : clean).replace(/\/+$/, "");
}

export function NamedProjectsPanel({ state, onState, onStartServices, startingServices = false, api = desktopApi }: {
  state: DesktopState; onState: (state: DesktopState) => void; onChooseProject?: () => void; api?: Api;
  onStartServices?: () => void; startingServices?: boolean;
}) {
  const [catalog, setCatalog] = useState<WorkspaceCatalog | null>(null);
  const [folders, setFolders] = useState<RegisteredProject[]>([]);
  const [draft, setDraft] = useState<WorkspaceDraft | null>(null);
  const [path, setPath] = useState("");
  const [removing, setRemoving] = useState<NamedProject | null>(null);
  const [teamProject, setTeamProject] = useState<NamedProject | null>(null);
  const [busy, setBusy] = useState(false);
  const [needsRefresh, setNeedsRefresh] = useState(false);
  const [canManage, setCanManage] = useState(false);
  const [error, setError] = useState("");
  const lock = useRef(false);
  const mounted = useRef(true);
  const generation = useRef(0);
  const dialogTrigger = useRef<HTMLButtonElement | null>(null);
  const createTrigger = useRef<HTMLButtonElement | null>(null);
  const editing = useRef(false);
  editing.current = !!draft || !!removing;
  const connected = state.topology?.experience === "full" && state.topology.server.kind === "local" && state.readiness.server === "ready" && state.readiness.runner === "ready";
  const connection = useRef(connected);
  connection.current = connected;
  const available = connected && !state.current_operation;
  const mutable = available && canManage && !!catalog && !busy && !needsRefresh;

  async function refresh(current: CurrentOperation) {
    const [next, registry] = await Promise.all([api.listNamedProjects(), api.listProjects()]);
    if (current()) {
      setCatalog(next); setFolders(registry.projects); setCanManage(registry.can_manage); setNeedsRefresh(false);
    }
  }

  async function operate(action: (current: CurrentOperation) => Promise<void>, saving = false) {
    if (lock.current || !available) return;
    lock.current = true; setBusy(true); setError("");
    const epoch = generation.current;
    const current = () => mounted.current && connection.current && epoch === generation.current;
    try { await action(current); }
    catch {
      if (current()) {
        setError(saving ? "保存未完成或结果未确认。请取消后刷新列表，确认最新状态再重试。" : "操作未完成，请检查文件夹路径和本机连接后重试。");
        if (saving) setNeedsRefresh(true);
      }
    } finally {
      lock.current = false;
      if (mounted.current) setBusy(false);
      if (mounted.current && connection.current && epoch !== generation.current && !editing.current) void operate(refresh);
    }
  }

  useEffect(() => {
    mounted.current = true;
    if (available && !editing.current) void operate(refresh);
    if (!connected) { generation.current += 1; setCatalog(null); setNeedsRefresh(true); }
    return () => { mounted.current = false; };
  }, [available, connected, api]);

  const dialogOpen = !!draft || !!removing;
  useEffect(() => {
    if (!dialogOpen) return;
    const trigger = dialogTrigger.current;
    return () => { queueMicrotask(() => { if (mounted.current) (trigger?.isConnected ? trigger : createTrigger.current)?.focus(); }); };
  }, [dialogOpen]);

  function closeDialog() {
    if (lock.current) return;
    setDraft(null); setRemoving(null); setPath(""); setError("");
  }

  function dialogKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") { event.preventDefault(); closeDialog(); }
    if (event.key !== "Tab") return;
    const controls = event.currentTarget.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled)");
    if (!controls.length) { event.preventDefault(); return; }
    const first = controls[0], last = controls[controls.length - 1];
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
  }

  function edit(project: NamedProject, trigger: HTMLButtonElement) {
    dialogTrigger.current = trigger;
    setError(""); setPath(""); setDraft(draftOf(project));
  }

  async function save(projects: WorkspaceDraft[], current: CurrentOperation) {
    if (!catalog || !current()) return;
    const next = await api.saveNamedProjects(catalog.revision, projects, catalog.host_id, Object.fromEntries(folders.map(folder => [folder.id, folder.revision])));
    if (current()) { setCatalog(next); setDraft(null); setRemoving(null); setPath(""); }
  }

  async function addPaths(paths: string[], current: CurrentOperation) {
    if (!draft || !current()) return;
    let registry = await api.listProjects();
    if (!current()) return;
    if (!registry.can_manage) throw new Error("project_management_unavailable");
    setFolders(registry.projects);
    const selected = new Set(draft.folders);
    for (const sourcePath of paths) {
      if (!current()) return;
      const inspected = await api.inspectProject(sourcePath);
      if (!current()) return;
      let folder = registry.projects.find(item => comparablePath(item.path) === comparablePath(inspected.path));
      if (folder && selected.has(folder.id)) continue;
      if (selected.size >= 16) { setError("每个项目最多添加 16 个源文件夹。"); return; }
      // Reuse the exact observed registration. Only new folders need registration.
      if (!folder) {
        const nextState = await api.addProject(inspected.path);
        if (!current()) return;
        onState(nextState);
        registry = await api.listProjects();
        if (!current()) return;
        setFolders(registry.projects);
        folder = registry.projects.find(item => comparablePath(item.path) === comparablePath(inspected.path));
      }
      if (!folder) { setError("此文件夹当前不可用，请刷新列表或选择其他文件夹。"); return; }
      selected.add(folder.id);
      setDraft({ ...draft, folders: [...selected] });
    }
    if (current()) setPath("");
  }

  async function chooseFolders(current: CurrentOperation) {
    const selection = await open({ directory: true, multiple: true, title: "添加源文件夹" });
    if (selection && current()) await addPaths(typeof selection === "string" ? [selection] : selection, current);
  }

  async function enableFolder(folder: RegisteredProject, current: CurrentOperation) {
    if (!current()) return;
    const nextState = await api.mutateProject(folder.id, folder.revision, "enable");
    if (!current()) return;
    onState(nextState);
    const registry = await api.listProjects();
    if (!current()) return;
    setFolders(registry.projects); setCanManage(registry.can_manage);
    if (!registry.projects.some(item => item.id === folder.id && item.enabled)) setError("尚未确认文件夹已启用，请取消后刷新列表。");
  }

  const duplicateName = !!draft && catalog?.projects.some(project => project.id !== draft.id && project.name.trim().toLocaleLowerCase() === draft.name.trim().toLocaleLowerCase());
  const teamSelectionMissing = !!draft && draft.folders.length > 1 && !draft.folders.includes(draft.team_project ?? "");
  const invalid = !draft?.name.trim() || !draft.folders.length || draft.folders.length > 16 || duplicateName || teamSelectionMissing || draft.folders.some(id => !folders.some(folder => folder.id === id && folder.enabled));
  const isEditing = !!draft && catalog?.projects.some(project => project.id === draft.id);
  const teamFolder = teamProject?.folders.find(folder => folder.project === teamProject.team_project) ?? (teamProject?.folders.length === 1 ? teamProject.folders[0] : null);
  if (teamProject && teamFolder) return <ProjectTeamSettings
    key={teamFolder.project}
    project={{ id: teamFolder.project, name: teamProject.name, path: teamFolder.path }}
    onBack={() => setTeamProject(null)}
  />;

  return <section className="page-section named-projects-page" aria-labelledby="named-projects-title" data-webcodex-page="projects">
    <div className="project-page-heading">
      <h1 id="named-projects-title">我的项目</h1>
      <div className="project-row-actions">
        <button className="secondary-button" disabled={!available || busy || dialogOpen} onClick={() => void operate(refresh)}>刷新列表</button>
        <button ref={createTrigger} className="primary-button" disabled={!mutable || dialogOpen || (catalog?.projects.length ?? 0) >= 32} onClick={event => {
          dialogTrigger.current = event.currentTarget; setError(""); setPath("");
          setDraft({ id: "wc_workspace_" + crypto.randomUUID().replaceAll("-", ""), name: "", enabled: true, folders: [] });
        }}>新建项目</button>
      </div>
    </div>
    <p className="lede">为项目命名，添加源文件夹，然后设置团队的工作方式。</p>
    {!connected && <div className="empty-state"><p role="status">{state.readiness.server === "ready" && state.readiness.runner === "ready" ? "当前连接不支持管理本机项目。" : "服务尚未就绪。无需先创建或选择项目，即可启动 Server 和 Runner。"}</p>
      {(state.readiness.server !== "ready" || state.readiness.runner !== "ready") && onStartServices && <button type="button" className="primary-button" disabled={busy || startingServices || Boolean(state.current_operation)} onClick={onStartServices} data-webcodex-action="start-project-services">{startingServices ? "正在启动服务…" : "启动服务"}</button>}
    </div>}
    {connected && !canManage && catalog && <p role="status">当前连接不支持管理本机项目。</p>}
    {needsRefresh && connected && <p role="status">请{dialogOpen ? "取消当前操作后" : ""}刷新列表，再继续管理项目。</p>}
    {!dialogOpen && error && <p role="alert" className="project-error">{error}</p>}
    {busy && !dialogOpen && <p role="status">正在处理……</p>}
    {catalog?.projects.length === 0 && <div className="empty-state">还没有项目。点击“新建项目”，填写名称并添加文件夹。</div>}
    <div className="project-registry" aria-busy={busy}>
      {catalog?.projects.map(project => <article className="detail-card project-registry-card" key={project.id} aria-label={project.name}>
        <div className="project-row-title"><h2>{project.name}</h2><span className="project-status">{project.enabled ? "已启用" : "已停用"} · {project.folders.length} 个文件夹</span></div>
        {project.folders.map(folder => <p key={folder.project} className="project-path">{displayPath(folder.path)}{project.folders.length > 1 && project.team_project === folder.project && <span className="named-project-team-tag">团队工作文件夹</span>}</p>)}
        <div className="project-row-actions">
          <button className="secondary-button" disabled={!mutable || dialogOpen} onClick={event => edit(project, event.currentTarget)}>编辑项目</button>
          <button className="secondary-button" disabled={!mutable || dialogOpen || !project.enabled} onClick={event => {
            if (project.folders.length > 1 && !project.folders.some(folder => folder.project === project.team_project)) edit(project, event.currentTarget);
            else setTeamProject(project);
          }}>团队设置</button>
          <button className="secondary-button" disabled={!mutable || dialogOpen} onClick={() => void operate(current => save(catalog.projects.map(item => ({ ...draftOf(item), enabled: item.id === project.id ? !item.enabled : item.enabled })), current), true)}>{project.enabled ? "停用项目" : "启用项目"}</button>
          <button className="secondary-button project-remove" disabled={!mutable || dialogOpen} onClick={event => { dialogTrigger.current = event.currentTarget; setError(""); setRemoving(project); }}>移除项目</button>
        </div>
      </article>)}
    </div>
    {catalog && catalog.projects.length > 0 && <p className="project-help named-projects-help">在 ChatGPT 中指定项目名称即可开始工作。团队设置用于新聊天，已有任务保持原有边界。</p>}
    {draft && <div className="project-dialog-backdrop" onKeyDown={dialogKeyDown}>
      <form role="dialog" aria-modal="true" aria-labelledby="named-project-dialog-title" aria-describedby="named-project-grant-help" className="detail-card named-project-dialog" onSubmit={event => {
        event.preventDefault();
        if (mutable && !invalid && catalog) void operate(current => save([...catalog.projects.filter(project => project.id !== draft.id).map(draftOf), { ...draft, name: draft.name.trim(), team_project: draft.folders.length === 1 ? null : draft.team_project }], current), true);
      }}>
        <div className="named-project-dialog-heading"><h2 id="named-project-dialog-title">{isEditing ? "编辑项目" : "创建项目"}</h2><button type="button" aria-label="关闭" className="named-project-close" disabled={busy} onClick={closeDialog}>×</button></div>
        <label className="field-group">项目名称<input autoFocus value={draft.name} maxLength={128} disabled={busy} onChange={event => setDraft({ ...draft, name: event.target.value })} placeholder="例如：EAS" required aria-invalid={duplicateName || undefined} /></label>
        {duplicateName && <p className="field-help">已有同名项目，请使用其他名称。</p>}
        <fieldset className="named-project-sources" disabled={!mutable}>
          <legend>源文件夹</legend>
          {draft.folders.length === 0 && <p className="project-help">添加此项目需要使用的本机文件夹。</p>}
          {draft.folders.map(id => {
            const folder = folders.find(item => item.id === id);
            return <div className="named-project-source" key={id}>
              <div><strong>{folder?.name ?? "文件夹已移除"}</strong><span className="project-path">{folder ? displayPath(folder.path) : id}{folder && !folder.enabled && "（已停用）"}</span></div>
              {folder && !folder.enabled && <button type="button" className="secondary-button" onClick={() => void operate(current => enableFolder(folder, current))}>启用文件夹</button>}
              <button type="button" className="named-project-close" aria-label={"移除源文件夹 " + (folder?.name ?? id)} onClick={() => setDraft({ ...draft, folders: draft.folders.filter(item => item !== id), team_project: draft.team_project === id ? null : draft.team_project })}>×</button>
            </div>;
          })}
          <div className="named-project-path-entry"><label className="field-group">文件夹路径<input value={path} onChange={event => setPath(event.target.value)} placeholder="输入完整文件夹路径" onKeyDown={event => { if (event.key === "Enter") { event.preventDefault(); if (path.trim()) void operate(current => addPaths([path.trim()], current)); } }} /></label><button type="button" className="secondary-button" disabled={!path.trim()} onClick={() => void operate(current => addPaths([path.trim()], current))}>添加路径</button></div>
          <button type="button" className="secondary-button named-project-browse" onClick={() => void operate(chooseFolders)}>添加文件夹…</button>
        </fieldset>
        {draft.folders.length > 1 && <label className="field-group">团队工作文件夹<select disabled={!mutable} value={draft.team_project ?? ""} onChange={event => setDraft({ ...draft, team_project: event.target.value || null })}><option value="">请选择团队工作文件夹</option>{draft.folders.map(id => { const folder = folders.find(item => item.id === id); return <option key={id} value={id}>{folder ? folder.name + " — " + displayPath(folder.path) : id}</option>; })}</select><span className="field-help">团队在此文件夹开展项目工作。子 Agent、CLI、模型和思考强度按任务选择。</span></label>}
        <p id="named-project-grant-help" className="project-help named-project-grant-help">{isEditing ? "保存" : "创建"}后，项目可访问这些源文件夹。取消不会保存新的项目授权。</p>
        {error && <p role="alert" className="project-error">{error}</p>}
        {busy && <p role="status">正在处理……</p>}
        <div className="project-row-actions named-project-dialog-actions"><button className="secondary-button" type="button" disabled={busy} onClick={closeDialog}>取消</button><button className="primary-button" type="submit" disabled={!mutable || !!invalid}>{isEditing ? "保存项目" : "创建项目"}</button></div>
      </form>
    </div>}
    {removing && <div className="project-dialog-backdrop" onKeyDown={dialogKeyDown}>
      <section role="dialog" aria-modal="true" aria-labelledby="remove-named-project-title" aria-describedby="remove-named-project-description" className="detail-card project-remove-dialog">
        <h2 id="remove-named-project-title">移除“{removing.name}”？</h2>
        <p id="remove-named-project-description">此项目将从“我的项目”中移除。本地文件不会删除；已有任务不会终止。</p>
        {error && <p role="alert" className="project-error">{error}</p>}
        {busy && <p role="status">正在处理……</p>}
        <div className="project-row-actions"><button autoFocus className="secondary-button" disabled={busy} onClick={closeDialog}>取消</button><button className="primary-button" disabled={!mutable} onClick={() => { if (catalog) void operate(current => save(catalog.projects.filter(project => project.id !== removing.id).map(draftOf), current), true); }}>确认移除</button></div>
      </section>
    </div>}
  </section>;
}
