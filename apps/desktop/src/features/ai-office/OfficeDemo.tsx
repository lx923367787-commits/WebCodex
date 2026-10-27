import { useState } from "react";
import brandIcon from "../../assets/brand.png";
import { aiOfficeDemoApi } from "../../lib/ai-office-api";
import type { DesktopState } from "../../models/topology";
import { ProjectsPanel, type ProjectRegistryApi } from "../projects/ProjectsPanel";
import { AiOffice } from "./AiOffice";

const previewState: DesktopState = {
  topology: { experience: "full", server: { kind: "local" }, runner: { kind: "local" }, exposure: { kind: "none" }, enrollment: { kind: "existing_profile", profile: "demo" } },
  readiness: { server: "ready", runner: "ready", exposure: "disabled", project: "ready", runtime_ready: true, ready_for_chatgpt: false, summary: "演示环境", summary_kind: "runtime_ready_local_only" },
  project: { path: "演示项目 / webcodex-desktop", allowed_root: "演示项目", is_git_repository: false, runtime_project_id: "demo-webcodex" },
  current_operation: null, activity_sequence: 0, openai_tunnel_configured: false,
  openai_tunnel_config: { tunnel_id_present: false, api_key_present: false, source: "environment", saved_tunnel_id: null },
  regular_tunnel_available: false, runtime_autostart: false, preferred_connection: "no_chat_gpt",
  tunnel_proxy: { mode: "direct", effective_source: "demo" },
};
const unavailable = async (): Promise<never> => { throw new Error("演示空间不会操作真实项目文件夹。"); };
const registryApi: ProjectRegistryApi = {
  async listProjects() {
    const snapshot = await aiOfficeDemoApi.snapshot();
    return {
      can_manage: true,
      projects: snapshot.projects.map((project) => ({
        id: project.id, name: project.name, path: project.path, revision: `demo:${project.policy.revision}`,
        enabled: true, connected: true, agent_status: "online", active_jobs: project.active_jobs,
      })),
    };
  },
  getState: async () => previewState,
  inspectProject: unavailable,
  addProject: unavailable, switchProject: unavailable, mutateProject: unavailable,
};

/** Preview the actual desktop page components; native project mutations stay unavailable. */
export function OfficeDemo() {
  const [page, setPage] = useState<"office" | "projects">(() => new URLSearchParams(window.location.search).get("view") === "projects" ? "projects" : "office");
  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><img className="brand-mark" src={brandIcon} alt="" /><div><strong>WebCodex</strong><span>Desktop</span></div></div>
      <nav aria-label="桌面预览导航">
        <button type="button" className={page === "projects" ? "active" : ""} aria-current={page === "projects" ? "page" : undefined} onClick={() => setPage("projects")}><span className="nav-icon nav-projects" aria-hidden="true" />项目</button>
        <button type="button" className={page === "office" ? "active" : ""} aria-current={page === "office" ? "page" : undefined} onClick={() => setPage("office")}><span className="nav-icon" aria-hidden="true" />AI 办公室</button>
      </nav>
      <div className="sidebar-status" style={{ marginTop: "auto" }}><i className="status-dot unknown" aria-hidden="true" /><div><strong>桌面页面 · 交互预览</strong><span>演示项目，不操作真实文件夹</span></div></div>
    </aside>
    <main className="main-content" style={page === "office" ? { padding: 18 } : undefined}>
      {page === "office" ? <AiOffice api={aiOfficeDemoApi} /> : <>
        <p className="project-help" role="note">演示空间：点击项目卡片的「团队设置」，与 AI 办公室共用同一份项目规则。</p>
        <ProjectsPanel state={previewState} onState={() => {}} onChooseProject={() => {}} api={registryApi} officeApi={aiOfficeDemoApi} preview />
      </>}
    </main>
  </div>;
}
