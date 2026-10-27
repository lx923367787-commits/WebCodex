import { useCallback, useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { DesktopState, ProjectAction, ProjectRegistrySnapshot, RegisteredProject } from "../../models/topology";
import { desktopApi } from "../../lib/desktop-api";
import { useLocale, type MessageKey } from "../../i18n/locale";
import { normalizeDesktopError } from "../../i18n/presentation";
import { aiOfficeApi } from "../../lib/ai-office-api";
import type { AiOfficeApi } from "../ai-office/types";
import { ProjectTeamSettings } from "./ProjectTeamSettings";

export type ProjectRegistryApi = Pick<typeof desktopApi, "listProjects" | "inspectProject" | "addProject" | "switchProject" | "mutateProject" | "getState">;

export function ProjectsPanel({ state, onState, onChooseProject, api = desktopApi, officeApi = aiOfficeApi, preview = false }: {
  state: DesktopState;
  onState: (state: DesktopState) => void;
  onChooseProject: () => void;
  api?: ProjectRegistryApi;
  officeApi?: AiOfficeApi;
  preview?: boolean;
}) {
  const { t } = useLocale();
  const [registry, setRegistry] = useState<ProjectRegistrySnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<MessageKey | null>(null);
  const [removing, setRemoving] = useState<RegisteredProject | null>(null);
  const [teamProject, setTeamProject] = useState<RegisteredProject | null>(null);
  const sequence = useRef(0);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  const removeTrigger = useRef<HTMLButtonElement | null>(null);
  const available = state.topology?.experience === "full" && state.readiness.server === "ready" && state.readiness.runner === "ready";
  const canObserve = available && !state.current_operation;
  const local = state.topology?.server.kind === "local";
  const busy = pending || loading || Boolean(state.current_operation);

  const refresh = useCallback(async (): Promise<ProjectRegistrySnapshot | null> => {
    const request = ++sequence.current;
    setLoading(true);
    setError(null);
    try {
      const next = await api.listProjects();
      if (mounted.current && request === sequence.current) { setRegistry(next); return next; }
    } catch (value) {
      if (mounted.current && request === sequence.current) {
        setRegistry(null);
        setError(projectError(value));
      }
    } finally {
      if (mounted.current && request === sequence.current) setLoading(false);
    }
    return null;
  }, [api]);

  useEffect(() => {
    mounted.current = true;
    if (canObserve) void refresh();
    else if (!available) { sequence.current += 1; setRegistry(null); setLoading(false); }
    return () => { mounted.current = false; sequence.current += 1; };
  }, [available, canObserve, refresh]);

  const operate = async (operation: () => Promise<DesktopState>, afterSuccess?: (registry: ProjectRegistrySnapshot) => void) => {
    if (preview || inFlight.current || busy) return;
    inFlight.current = true;
    sequence.current += 1;
    setPending(true);
    setError(null);
    let failure: MessageKey | null = null;
    try {
      onState(await operation());
    } catch (value) {
      failure = projectError(value);
      // A lost response can follow a committed registry/selection change.
      try { onState(await api.getState()); } catch { /* Keep the last verified state. */ }
    } finally {
      inFlight.current = false;
      if (mounted.current) {
        setPending(false);
        const next = await refresh();
        if (failure) setError(failure);
        else if (next && mounted.current) afterSuccess?.(next);
      }
    }
  };

  const add = async () => {
    if (preview || busy || inFlight.current) return;
    inFlight.current = true;
    setPending(true);
    let selection: string | string[] | null = null;
    try {
      selection = await open({ directory: true, multiple: false, title: t("setup.chooseProject") });
    } catch (value) { if (mounted.current) setError(projectError(value)); }
    finally { inFlight.current = false; if (mounted.current) setPending(false); }
    if (typeof selection === "string" && mounted.current) {
      const path = selection;
      let inspectedPath: string | null = null;
      await operate(async () => {
        inspectedPath = (await api.inspectProject(path)).path;
        return api.addProject(path);
      }, (next) => {
        const canonicalPath = inspectedPath;
        if (canonicalPath === null) return;
        const matching = next.projects.filter((project) => comparableProjectPath(project.path) === comparableProjectPath(canonicalPath));
        const selected = matching.length === 1 ? matching[0] : null;
        if (selected) setTeamProject(selected);
      });
    }
  };

  const mutate = (project: RegisteredProject, action: ProjectAction) =>
    operate(() => api.mutateProject(project.id, project.revision, action));

  const closeRemoval = () => {
    setRemoving(null);
    removeTrigger.current?.focus();
  };

  if (teamProject) {
    return <ProjectTeamSettings key={teamProject.id} project={teamProject} api={officeApi} onBack={() => setTeamProject(null)} />;
  }

  return (
    <section className="page-section" aria-labelledby="projects-title" data-webcodex-page="projects">
      <div className="eyebrow">{t("project.eyebrow")}</div>
      <div className="project-page-heading">
        <h1 id="projects-title">{t("project.title")}</h1>
        <div className="project-row-actions">
          {available && <button className="secondary-button" onClick={() => void refresh()} disabled={busy}>{t("project.refresh")}</button>}
          <button className="primary-button" onClick={() => available && local ? void add() : onChooseProject()} disabled={busy || preview}>{t("project.add")}</button>
        </div>
      </div>
      <p className="lede">{t("project.description")}</p>
      <p className="project-help">{t("project.registrationOnly")}</p>
      <p className="project-help">每个项目文件夹都可以独立设置团队人数、CLI 使用与模型。默认由总经理自主安排。</p>
      {preview && <p className="project-help">演示项目支持团队设置与策略保存；文件夹管理操作在桌面端使用。</p>}
      {error && <p role="alert" className="project-error">{t(error)}</p>}
      {!available ? <div className="empty-state">{t("project.runtimeRequired")}</div> : <>
        {loading && <p role="status">{t("project.loading")}</p>}
        {registry && !registry.can_manage && <p>{t("project.localManagementOnly")}</p>}
        {registry?.projects.length === 0 && <div className="empty-state">{t("project.emptyRegistry")}</div>}
        <div className="project-registry" aria-busy={busy}>
          {registry?.projects.map((project) => {
            const current = state.project?.runtime_project_id === project.id;
            const online = project.agent_status === "online";
            const mutable = registry.can_manage && online && !busy && !preview;
            return (
              <article className="detail-card project-registry-card" key={project.id} aria-label={project.name}>
                <div className="project-row-title">
                  <h2>{project.name}</h2>
                  {current && <span className="project-tag">{t("project.current")}</span>}
                  <span className="project-status">{t(!project.enabled ? "project.disabled" : project.connected ? "project.online" : "project.offline")}</span>
                </div>
                <p className="project-path">{project.path.replace(/^\\\\\?\\/, "")}</p>
                {project.active_jobs > 0 && <p className="project-help">{t("project.activeJobs", { count: project.active_jobs })}</p>}
                <div className="project-row-actions">
                  <button className="secondary-button" disabled={busy} onClick={() => setTeamProject(project)}>团队设置</button>
                  <button className="secondary-button" disabled={!mutable || !project.enabled || current} onClick={() => void operate(() => api.switchProject(project.id, project.revision))}>{t(current ? "project.current" : "project.switch")}</button>
                  <button className="secondary-button" disabled={!mutable} onClick={() => void mutate(project, project.enabled ? "disable" : "enable")}>{t(project.enabled ? "project.disable" : "project.enable")}</button>
                  <button className="secondary-button project-remove" disabled={!mutable || project.active_jobs > 0} onClick={(event) => { removeTrigger.current = event.currentTarget; setRemoving(project); }}>{t("project.remove")}</button>
                </div>
              </article>
            );
          })}
        </div>
      </>}
      {removing && <div className="project-dialog-backdrop" onKeyDown={(event) => {
        if (event.key === "Escape") closeRemoval();
        if (event.key === "Tab") {
          const buttons = event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)");
          const first = buttons[0], last = buttons[buttons.length - 1];
          if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
          else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
        }
      }}>
        <section role="dialog" aria-modal="true" aria-labelledby="remove-project-title" aria-describedby="remove-project-description" className="detail-card project-remove-dialog">
          <h2 id="remove-project-title">{t("project.removeTitle", { name: removing.name })}</h2>
          <p className="project-path">{removing.path}</p>
          <p id="remove-project-description">{t("project.removeDescription")}</p>
          {state.project?.runtime_project_id === removing.id && <p>{t("project.removeCurrent")}</p>}
          <div className="project-row-actions">
            <button autoFocus className="secondary-button" onClick={closeRemoval}>{t("project.cancel")}</button>
            <button className="primary-button" disabled={busy} onClick={() => { const target = removing; closeRemoval(); void mutate(target, "unregister"); }}>{t("project.confirmRemove")}</button>
          </div>
        </section>
      </div>}
    </section>
  );
}

function comparableProjectPath(path: string): string {
  const clean = path.replace(/^\\\\\?\\/, "").replace(/\\/g, "/").replace(/\/+$/, "");
  return /^[A-Za-z]:\//.test(clean) || clean.startsWith("//") ? clean.toLowerCase() : clean;
}

function projectError(value: unknown): MessageKey {
  switch (normalizeDesktopError(value).code) {
    case "project_revision_conflict": case "project_connection_changed": return "project.changed";
    case "project_active_jobs_conflict": return "project.jobsConflict";
    case "project_agent_unavailable": case "runtime_not_ready": return "project.runtimeRequired";
    case "project_operation_indeterminate": return "project.unknownOutcome";
    case "project_forbidden": case "project_unauthorized": return "project.permissionDenied";
    default: return "project.operationFailed";
  }
}
