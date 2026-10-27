import { useEffect, useRef, useState } from "react";
import { aiOfficeApi } from "../../lib/ai-office-api";
import type { RegisteredProject } from "../../models/topology";
import { OfficePolicyPanel, officeError } from "../ai-office/OfficePolicyPanel";
import type { AiOfficeApi, OfficePolicy, OfficeProject, OfficeSnapshot } from "../ai-office/types";
import "../ai-office/ai-office.css";
import "./project-team-settings.css";

export function ProjectTeamSettings({ project, api = aiOfficeApi, onBack }: {
  project: Pick<RegisteredProject, "id" | "name" | "path">;
  api?: AiOfficeApi;
  onBack: () => void;
}) {
  const [loaded, setLoaded] = useState<OfficeProject | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [confirmation, setConfirmation] = useState<"unconfirmed" | "checking" | "server" | "demo">("unconfirmed");
  const [limitations, setLimitations] = useState<string[]>([]);
  const [verificationError, setVerificationError] = useState("");
  const [observedSource, setObservedSource] = useState<OfficeSnapshot["source"] | null>(null);
  const readVersion = useRef(0);
  const currentScope = useRef({ projectId: project.id, api });
  currentScope.current = { projectId: project.id, api };

  function acceptSnapshot(snapshot: OfficeSnapshot, exact: OfficeProject, expectedRevision?: number) {
    setLimitations(snapshot.limitations);
    setObservedSource(snapshot.source);
    if (expectedRevision !== undefined && exact.policy.revision < expectedRevision) {
      setConfirmation("unconfirmed");
      setVerificationError("最新读数尚未包含刚保存的策略版本，请继续核实；当前保留已保存的编辑内容。");
      return;
    }
    setLoaded(exact);
    setConfirmation(snapshot.source === "demo" ? "demo" : snapshot.enforcement === "managed_tasks" ? "server" : "unconfirmed");
    setVerificationError("");
  }

  useEffect(() => {
    let current = true;
    const version = ++readVersion.current;
    setLoading(true); setError(""); setLoaded(null);
    setConfirmation("unconfirmed"); setLimitations([]); setVerificationError(""); setObservedSource(null);
    void api.snapshot(project.id).then((snapshot) => {
      if (!current || readVersion.current !== version) return;
      const exact = snapshot.projects.find((row) => row.id === project.id);
      if (!exact) throw new Error("这个项目的登记信息已变化，请返回项目列表刷新后重试。");
      acceptSnapshot(snapshot, exact);
    }).catch((cause) => {
      if (current) setError(officeError(cause));
    }).finally(() => { if (current) setLoading(false); });
    return () => { current = false; ++readVersion.current; };
  }, [api, project.id, attempt]);

  async function verifyPolicy(expectedRevision: number) {
    if (currentScope.current.projectId !== project.id || currentScope.current.api !== api) return;
    const version = ++readVersion.current;
    setConfirmation("checking"); setVerificationError("");
    try {
      const snapshot = await api.snapshot(project.id);
      if (readVersion.current !== version) return;
      const exact = snapshot.projects.find((row) => row.id === project.id);
      if (!exact) throw new Error("最新观测中未找到当前项目。");
      acceptSnapshot(snapshot, exact, expectedRevision);
    } catch (cause) {
      if (readVersion.current !== version) return;
      setConfirmation("unconfirmed");
      setVerificationError("最新 Server 观测未确认：" + officeError(cause));
    }
  }

  function policySaved(policy: OfficePolicy) {
    if (currentScope.current.projectId !== project.id || currentScope.current.api !== api) return;
    setLoaded((current) => current?.id === project.id ? { ...current, policy } : current);
    void verifyPolicy(policy.revision);
  }

  return <section className="page-section project-team-page" aria-labelledby="project-team-title" data-webcodex-page="project-team-settings">
    <div className="project-team-heading">
      <div><div className="eyebrow">PROJECT TEAM</div><h1 id="project-team-title">项目团队设置</h1></div>
      <button type="button" className="secondary-button" onClick={onBack}>← 返回项目列表</button>
    </div>
    <div className="project-team-folder"><span aria-hidden="true">▱</span><div><h2>{project.name}</h2><p className="project-path">{project.path.replace(/^\\\\\?\\/, "")}</p></div></div>
    <p className="project-help">这套规则用于此工作文件夹的团队任务。保存后，请新开聊天并选择这个项目，新的 Agent 才会获得对应人数和 CLI 额度；已有聊天和任务保留原来的授权边界。没有 AI 工作时也能预先设置。</p>
    {loaded?.policy.revision === 0 && <p role="status" className="project-help">尚未保存团队策略。请先保存默认安排或自定义边界，再开启新聊天。</p>}
    {loaded && confirmation === "unconfirmed" && observedSource !== "demo" && <div role="alert" className="project-team-load-error"><strong>当前策略尚未获 Server 确认</strong><p>当前显示的缓存或未确认读数不代表配额已在 Server 生效。仍可编辑，保存会向 Server 提交版本校验。</p>{verificationError && <p>{verificationError}</p>}{limitations.length > 0 && <ul>{limitations.map((limitation) => <li key={limitation}>{limitation}</li>)}</ul>}<button type="button" className="secondary-button" onClick={() => void verifyPolicy(loaded.policy.revision)}>重新核实 Server 策略</button></div>}
    {loaded && confirmation === "checking" && <p role="status" className="project-team-loading">{observedSource === "demo" ? "正在重新读取演示策略…" : "正在重新观测 Server，尚未确认策略生效…"}</p>}
    {loaded && confirmation === "server" && <p role="status" className="project-help">Server 已确认当前项目策略 · 版本 {loaded.policy.revision}。已登记任务使用各自冻结的策略版本。</p>}
    {loaded && observedSource === "demo" && <div role="note" className="project-team-load-error"><strong>演示策略 · 不约束真实任务</strong><p>当前项目与设置属于演示环境，不代表 Server 真实派工规则已生效。</p>{verificationError && <p>{verificationError}</p>}{limitations.length > 0 && <ul>{limitations.map((limitation) => <li key={limitation}>{limitation}</li>)}</ul>}</div>}
    {loading && <p role="status" className="project-team-loading">正在载入这个项目的团队策略…</p>}
    {error && <div role="alert" className="project-team-load-error"><p>{error}</p><button type="button" className="secondary-button" onClick={() => setAttempt((value) => value + 1)}>重试载入</button></div>}
    {loaded && <div className="ai-office project-team-settings"><OfficePolicyPanel key={loaded.id} project={loaded} api={api} onSaved={policySaved} /></div>}
  </section>;
}
