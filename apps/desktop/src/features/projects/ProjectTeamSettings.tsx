import { useEffect, useState } from "react";
import { aiOfficeApi } from "../../lib/ai-office-api";
import type { RegisteredProject } from "../../models/topology";
import { OfficePolicyPanel, officeError } from "../ai-office/OfficePolicyPanel";
import type { AiOfficeApi, OfficeProject } from "../ai-office/types";

export function ProjectTeamSettings({ project, onBack, api = aiOfficeApi }: { project: Pick<RegisteredProject, "id" | "name" | "path">; onBack: () => void; api?: AiOfficeApi }) {
  const [loaded, setLoaded] = useState<OfficeProject | null>(null);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let active = true;
    setLoaded(null); setError("");
    void api.snapshot(project.id).then((snapshot) => {
      if (!active) return;
      const exact = snapshot.projects.find((item) => item.id === project.id);
      if (!exact) throw new Error("这个项目的登记信息已变化，请返回项目列表刷新后重试。");
      setLoaded(exact);
    }).catch((cause) => { if (active) setError(officeError(cause)); });
    return () => { active = false; };
  }, [api, project.id, attempt]);
  return <section className="page-section project-team-page" aria-labelledby="project-team-title" data-webcodex-page="project-team-settings">
    <div className="project-team-heading"><h1 id="project-team-title">项目协作与工具</h1><button type="button" className="secondary-button" onClick={onBack}>← 返回项目列表</button></div>
    <div className="project-team-folder"><div><h2>{project.name}</h2><p className="project-path">{project.path}</p></div></div>
    {!loaded && !error && <p role="status">正在载入项目…</p>}
    {error && <div role="alert" className="project-team-load-error"><p>{error}</p><button type="button" className="secondary-button" onClick={() => setAttempt((value) => value + 1)}>重试载入</button></div>}
    {loaded && <div className="ai-office project-team-settings"><OfficePolicyPanel key={loaded.id} project={loaded} api={api} onSaved={() => {}} /></div>}
  </section>;
}
