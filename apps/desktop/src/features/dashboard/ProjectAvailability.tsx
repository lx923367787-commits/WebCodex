import { useEffect, useState } from "react";
import { desktopApi } from "../../lib/desktop-api";
import type { DesktopState } from "../../models/topology";
import { useLocale } from "../../i18n/locale";

export function ProjectAvailability({ state, refreshing, onManage }: { state: DesktopState; refreshing: boolean; onManage: () => void }) {
  const { t } = useLocale();
  const [count, setCount] = useState<number | null>(null);
  const servicesReady = state.readiness.server === "ready" && state.readiness.runner === "ready";
  useEffect(() => {
    let current = true;
    setCount(null);
    if (servicesReady && !state.current_operation && !refreshing) {
      void Promise.all([desktopApi.listNamedProjects(), desktopApi.listProjects()]).then(([catalog, registry]) => {
        if (!current) return;
        const folders = new Set(registry.projects.filter(p => p.enabled && p.agent_status === "online").map(p => p.id));
        setCount(catalog.projects.filter(p => p.enabled && p.folders.some(f => folders.has(f.project))).length);
      }).catch(() => { if (current) setCount(null); });
    }
    return () => { current = false; };
  }, [servicesReady, state.current_operation?.id, state.activity_sequence, state.topology?.server.kind, refreshing]);
  return <article className="detail-card">
    <span className="section-kicker">{t("home.projects")}</span>
    <strong>{count === null ? t("home.projectsUnchecked") : count === 0 ? t("home.projectsEmpty") : t("home.projectsAvailable", { count })}</strong>
    <p>{t("home.projectTaskBoundary")}</p>
    <button className="secondary-button" onClick={onManage}>{t("home.manageProjects")}</button>
  </article>;
}
