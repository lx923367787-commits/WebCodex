import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { desktopApi } from "../../lib/desktop-api";
import type { DesktopState, RegisteredProject } from "../../models/topology";
import { ProjectsPanel } from "./ProjectsPanel";
import { ProjectTeamSettings } from "./ProjectTeamSettings";

export interface NamedProject { id: string; name: string; enabled: boolean; folders: { project: string; name: string; path: string }[] }
export interface WorkspaceCatalog { host_id: string; revision: number; projects: NamedProject[] }
export interface WorkspaceDraft { id: string; name: string; enabled: boolean; folders: string[] }
type Api = Pick<typeof desktopApi, "listNamedProjects" | "saveNamedProjects" | "listProjects" | "inspectProject" | "addProject">;
const draftOf = (p: NamedProject): WorkspaceDraft => ({ ...p, folders: p.folders.map(f => f.project) });

export function NamedProjectsPanel({ state, onState, onChooseProject, api = desktopApi }: {
  state: DesktopState; onState: (state: DesktopState) => void; onChooseProject: () => void; api?: Api;
}) {
  const [catalog, setCatalog] = useState<WorkspaceCatalog | null>(null);
  const [folders, setFolders] = useState<RegisteredProject[]>([]);
  const [draft, setDraft] = useState<WorkspaceDraft | null>(null);
  const [teamProject, setTeamProject] = useState<NamedProject | null>(null);
  const [teamFolder, setTeamFolder] = useState<NamedProject["folders"][number] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const lock = useRef(false);
  const editing = useRef(false);
  editing.current = !!draft;
  const mounted = useRef(true);
  const available = state.topology?.experience === "full" && state.topology.server.kind === "local" && state.readiness.server === "ready" && state.readiness.runner === "ready" && !state.current_operation;
  async function refresh() {
    const [next, registry] = await Promise.all([api.listNamedProjects(), api.listProjects()]);
    if (mounted.current) { setCatalog(next); setFolders(registry.projects); }
  }
  async function operate(action: () => Promise<void>) {
    if (lock.current) return;
    lock.current = true; setBusy(true); setError("");
    try { await action(); }
    catch (e) { if (mounted.current) setError(`操作未完成，请刷新后重试。${typeof e === "object" && e && "code" in e ? String(e.code) : ""}`); }
    finally { lock.current = false; if (mounted.current) setBusy(false); }
  }
  useEffect(() => { mounted.current = true; if (available && !editing.current) void operate(refresh); return () => { mounted.current = false; }; }, [available]);
  async function save(projects: WorkspaceDraft[]) {
    if (!catalog) return;
    const next = await api.saveNamedProjects(catalog.revision, projects, catalog.host_id, Object.fromEntries(folders.map(f => [f.id, f.revision])));
    if (mounted.current) { setCatalog(next); setDraft(null); }
  }
  async function addFolders() {
    const selection = await open({ directory: true, multiple: true, title: "选择要授权给此项目的文件夹" });
    if (!selection || !draft) return;
    const paths = typeof selection === "string" ? [selection] : selection;
    if (paths.length + draft.folders.length > 16) { setError("每个项目最多包含 16 个文件夹。"); return; }
    const selected = new Set(draft.folders);
    for (const path of paths) {
      const inspected = await api.inspectProject(path);
      onState(await api.addProject(path));
      const registry = await api.listProjects();
      const normalize = (p: string) => p.replace(/^\\\\\?\\/, "").replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
      const folder = registry.projects.find(p => normalize(p.path) === normalize(inspected.path));
      if (!folder) throw new Error("folder_registration_unconfirmed");
      selected.add(folder.id);
      if (mounted.current) { setFolders(registry.projects); setDraft({ ...draft, folders: [...selected] }); }
    }
  }
  const invalid = !draft?.name.trim() || !draft.folders.length || draft.folders.length > 16 || catalog?.projects.some(p => p.id !== draft.id && p.name.trim().toLocaleLowerCase() === draft.name.trim().toLocaleLowerCase());
  if (teamProject && teamFolder) return <ProjectTeamSettings
    key={teamFolder.project}
    project={{ id: teamFolder.project, name: teamProject.name, path: teamFolder.path }}
    onBack={() => { setTeamProject(null); setTeamFolder(null); }}
  />;
  return <section className="projects-page" aria-labelledby="named-projects-title">
    <div className="project-page-heading"><h1 id="named-projects-title">我的项目</h1><div className="project-row-actions">
      <button className="secondary-button" disabled={!available || busy || !!draft} onClick={() => void operate(refresh)}>刷新列表</button>
      <button className="primary-button" disabled={!available || busy || !catalog || !!draft || catalog.projects.length >= 32} onClick={() => setDraft({ id: `wc_workspace_${crypto.randomUUID().replaceAll("-", "")}`, name: "", enabled: true, folders: [] })}>新建命名项目</button>
    </div></div>
    <p className="lede">给项目起一个名字，并授权它可以使用的多个文件夹。文件夹中的本地文件不会被移动。</p>
    <p className="project-help">在 ChatGPT 中说“@webcodex 在 EAS 项目中……”即可指定项目。有多个项目而未指定时，会先让你选择；只有一个可用项目时自动选择。已有聊天保持原项目，换项目请开新聊天。</p>
    {!available && <p role="status">请先启动本机 Server、Runner 和 Tunnel，再管理命名项目。</p>}
    {error && <p role="alert" className="project-error">{error}</p>}
    {busy && <p role="status">正在处理……</p>}
    {teamProject && !teamFolder && <section className="detail-card" aria-label="选择团队设置的文件夹">
      <h2>{teamProject.name} · 团队设置</h2>
      <p className="project-help">此项目包含多个文件夹，请选择要查看或修改团队设置的文件夹。各文件夹保留原有设置。</p>
      {teamProject.folders.map(folder => <button key={folder.project} type="button" className="secondary-button" onClick={() => setTeamFolder(folder)}>{folder.name} — {folder.path}</button>)}
      <button type="button" className="secondary-button" onClick={() => setTeamProject(null)}>取消选择</button>
    </section>}
    {catalog?.projects.length === 0 && !draft && <div className="empty-state">还没有命名项目。新建项目后，可勾选已登记文件夹或添加其他文件夹。</div>}
    {draft && <form className="detail-card" onSubmit={e => { e.preventDefault(); if (!invalid && catalog) void operate(() => save([...catalog.projects.filter(p => p.id !== draft.id).map(draftOf), { ...draft, name: draft.name.trim() }])); }}>
      <h2>{catalog?.projects.some(p => p.id === draft.id) ? "编辑项目与授权" : "新建项目与授权"}</h2>
      <label>项目名称 <input autoFocus value={draft.name} maxLength={128} disabled={busy} onChange={e => setDraft({ ...draft, name: e.target.value })} placeholder="例如：EAS" required /></label>
      <fieldset disabled={busy}><legend>授权的文件夹（可多选，最多 16 个）</legend>
        {folders.filter(f => f.enabled || draft.folders.includes(f.id)).map(f => <label key={f.id} style={{ display: "block", padding: "8px 0" }}>
          <input type="checkbox" checked={draft.folders.includes(f.id)} onChange={e => setDraft({ ...draft, folders: e.target.checked ? [...draft.folders, f.id] : draft.folders.filter(id => id !== f.id) })} /> {f.name} — {f.path}{!f.enabled && "（已停用，请取消勾选或先启用）"}
        </label>)}
        {draft.folders.filter(id => !folders.some(f => f.id === id)).map(id => <p key={id}>文件夹已移除：{id} <button type="button" onClick={() => setDraft({ ...draft, folders: draft.folders.filter(f => f !== id) })}>取消授权</button></p>)}
        <button type="button" className="secondary-button" onClick={() => void operate(addFolders)}>添加文件夹…</button>
      </fieldset>
      <p className="project-help">保存即信任并授权此项目访问选中的文件夹；不包括其相邻目录。取消勾选后，后续项目文件请求将不再获准访问该文件夹。</p>
      <div className="project-row-actions"><button className="primary-button" type="submit" disabled={busy || !!invalid}>保存并授权</button><button className="secondary-button" type="button" disabled={busy} onClick={() => setDraft(null)}>取消</button></div>
    </form>}
    <div className="project-registry">{catalog?.projects.map(project => <article className="detail-card project-registry-card" key={project.id}>
      <div className="project-row-title"><h2>{project.name}</h2><span>{project.enabled ? "已授权" : "已停用"} · {project.folders.length} 个文件夹</span></div>
      {project.folders.map(f => <p key={f.project} className="project-path">{f.name} — {f.path}</p>)}
      <div className="project-row-actions"><button className="secondary-button" disabled={busy || !!draft || !available} onClick={() => setDraft(draftOf(project))}>编辑名称与文件夹</button>
        <button className="secondary-button" disabled={busy || !!draft || !available} onClick={() => void operate(() => save(catalog.projects.map(p => ({ ...draftOf(p), enabled: p.id === project.id ? !p.enabled : p.enabled }))))}>{project.enabled ? "停用授权" : "启用授权"}</button>
        <button className="secondary-button" disabled={busy || !!draft || !available || !project.enabled} onClick={() => { setTeamProject(project); setTeamFolder(project.folders.length === 1 ? project.folders[0] : null); }}>团队设置</button>
      </div></article>)}</div>
    <details style={{ marginTop: 24 }}><summary>高级：已登记文件夹与团队设置</summary><ProjectsPanel state={state} onState={onState} onChooseProject={onChooseProject} /></details>
  </section>;
}
