import { useCallback, useEffect, useRef, useState } from "react";
import type { AiOfficeApi, OfficeMember, OfficePolicy, OfficeSnapshot } from "./types";
import { OfficeGlyph, OfficeScene, type SceneMember } from "./OfficeScene";
import { officeError, OfficePolicyPanel } from "./OfficePolicyPanel";
import { TeamTaskPanel } from "./TeamTaskPanel";
import "./ai-office.css";

const ROLE_LABELS: Record<OfficeMember["role"], string> = { boss: "大老板", manager: "总经理", subagent: "小伙伴", cli: "工具伙伴", observed_agent: "工作窗口" };
const STATE_LABELS: Record<OfficeMember["state"], string> = { working: "工作中", idle: "待命中", waiting: "等待中", done: "已完成", error: "需要关注" };

function timeLabel(timestamp: number | null): string {
  if (timestamp === null) return "暂无活动时间";
  return new Date(timestamp).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });
}

function progressValue(value: number | null): number | null {
  return value === null || !Number.isFinite(value) ? null : Math.max(0, Math.min(100, value));
}

function roleKind(member: OfficeMember): SceneMember["kind"] {
  return member.role === "manager" ? "manager" : member.role === "cli" ? "cli" : "agent";
}

export function AiOffice({ api }: { api: AiOfficeApi }) {
  const [snapshot, setSnapshot] = useState<OfficeSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [selectedMemberId, setSelectedMemberId] = useState<string | null>(null);
  const [pendingProjectId, setPendingProjectId] = useState<string | null>(null);
  const [view, setView] = useState<"office" | "tasks" | "team" | "policy">("office");
  const [query, setQuery] = useState("");
  const requestVersion = useRef(0);
  const requestPending = useRef(false);
  const currentProject = useRef<string | undefined>(undefined);
  const mounted = useRef(true);

  const fetchSnapshot = useCallback(async (projectId?: string, quiet = false) => {
    const version = ++requestVersion.current;
    requestPending.current = true;
    if (!quiet) setLoading(true);
    setError("");
    try {
      const next = await api.snapshot(projectId);
      if (!mounted.current || version !== requestVersion.current) return;
      currentProject.current = next.selected_project_id ?? undefined;
      setSnapshot(next);
      setSelectedMemberId((previous) => next.members.some((member) => member.id === previous && member.role !== "boss") ? previous : next.members.find((member) => member.role === "manager")?.id ?? next.members.find((member) => member.role !== "boss")?.id ?? null);
    } catch (cause) {
      if (mounted.current && version === requestVersion.current) setError(officeError(cause));
    } finally {
      if (mounted.current && version === requestVersion.current) { requestPending.current = false; setLoading(false); setPendingProjectId(null); }
    }
  }, [api]);

  useEffect(() => {
    mounted.current = true;
    void fetchSnapshot();
    const interval = window.setInterval(() => {
      if (document.visibilityState === "visible" && !requestPending.current) void fetchSnapshot(currentProject.current, true);
    }, 15_000);
    return () => { mounted.current = false; ++requestVersion.current; window.clearInterval(interval); };
  }, [fetchSnapshot]);

  function chooseProject(projectId: string) {
    setPendingProjectId(projectId);
    void fetchSnapshot(projectId);
  }

  const project = snapshot?.projects.find((item) => item.id === snapshot.selected_project_id);
  const members = snapshot?.members.filter((member) => member.role !== "boss") ?? [];
  const selectedMember = members.find((member) => member.id === selectedMemberId);
  const activeMembers = members.filter((member) => member.state === "working");
  const activeProjects = snapshot?.projects.filter((item) => item.active_jobs > 0).length ?? 0;
  const filteredProjects = snapshot?.projects.filter((item) => `${item.name} ${item.path}`.toLowerCase().includes(query.toLowerCase())) ?? [];
  const demo = snapshot?.source === "demo";
  const sourceLabel = demo ? "DEMO · 模拟演示" : snapshot?.source === "runtime" ? "运行时快照" : "离线 · 配置可用";
  const sceneMembers: SceneMember[] = members.map((member) => ({ id: member.id, name: member.label, role: ROLE_LABELS[member.role], state: STATE_LABELS[member.state], task: member.task ?? "暂无任务说明", parentId: member.parent_id, working: member.state === "working", kind: roleKind(member) }));

  function policySaved(policy: OfficePolicy) {
    setSnapshot((previous) => previous ? { ...previous, projects: previous.projects.map((item) => item.id === project?.id ? { ...item, policy } : item) } : previous);
  }

  return <div className="ai-office">
    <header className="ao-topbar"><div className="ao-brand"><div className="ao-brand-icon" aria-hidden="true"><i /><i /><i /><i /></div><div><strong>AI 办公室</strong><span>WEBCODEX WORKSPACE</span></div></div><div className="ao-topbar-right"><span className={`ao-source-tag ${demo ? "is-demo" : snapshot?.source === "runtime" ? "is-runtime" : ""}`}><i />{snapshot ? sourceLabel : "正在连接办公室…"}</span><button type="button" className="ao-icon-button" aria-label="刷新办公室" disabled={loading} onClick={() => void fetchSnapshot(currentProject.current)}><span className={loading ? "ao-spin" : ""}>↻</span></button><div className="ao-user-avatar" aria-label="你，大老板">B</div></div></header>
    {demo && <div className="ao-demo-banner"><span>✦ 演示空间</span><span>小人、任务与进度均为模拟数据；策略操作只影响演示环境。</span><span>可交互 DEMO</span></div>}
    {error && <div className="ao-global-error" role="alert"><span>{error}</span><button type="button" disabled={loading} onClick={() => void fetchSnapshot(currentProject.current)}>重新连接</button></div>}
    <div className="ao-layout">
      <aside className="ao-project-sidebar"><div className="ao-sidebar-title"><span>我的项目</span><span>{snapshot?.projects.length ?? "—"}</span></div><label className="ao-search"><span aria-hidden="true">⌕</span><input aria-label="搜索项目" placeholder="搜索项目…" value={query} onChange={(event) => setQuery(event.target.value)} /></label><div className="ao-project-list" aria-label="项目文件夹">{filteredProjects.map((item, index) => <button type="button" className={`ao-project ${project?.id === item.id ? "is-selected" : ""}`} aria-pressed={project?.id === item.id} key={item.id} onClick={() => chooseProject(item.id)}><span className={`ao-folder-icon ao-folder-${index % 3}`} aria-hidden="true" /><span className="ao-project-copy"><strong>{item.name}</strong><span>{pendingProjectId === item.id ? "正在进入…" : item.active_jobs > 0 ? <><i className="ao-status-dot" />{item.active_jobs} 项工作进行中</> : "闲置 · 可以预设团队"}</span></span><span className="ao-project-arrow">›</span></button>)}{snapshot && filteredProjects.length === 0 && <p className="ao-sidebar-empty">{query ? "没有匹配的项目" : "暂无已登记项目"}</p>}</div><div className="ao-sidebar-bottom"><div className="ao-sidebar-stats"><strong>{activeProjects}<span>项目在忙</span></strong><strong>{snapshot?.projects.length ?? 0}<span>全部项目</span></strong></div><p><i className="ao-status-dot" />工作发生的地方，随时看见。</p><span className="ao-sidebar-footnote">闲置项目也可以配置团队策略</span></div></aside>
      <main className="ao-workspace">
        <div className="ao-workspace-heading"><div><span className="ao-breadcrumb">工作空间 <span>/</span> {project?.name ?? "AI 办公室"}</span><h1>{project?.name ?? "你的 AI 团队，在这里"}<span className={`ao-project-state ${activeMembers.length ? "is-active" : ""}`}><i />{loading && !snapshot ? "载入中" : activeMembers.length ? demo ? "正在协作" : "有活跃窗口" : "准备就绪"}</span></h1><p>{project?.path ?? "选择一个项目，了解任务、进度与分工。"}</p></div><button type="button" className="ao-button ao-button-outline" onClick={() => setView("policy")} disabled={!project}><span aria-hidden="true">⚙</span>团队策略<span className="ao-button-arrow">↗</span></button></div>
        <div className="ao-workspace-toolbar"><nav className="ao-tabs" aria-label="办公室视图">{([{ id: "office", label: "办公室", icon: "▦" }, { id: "tasks", label: demo ? "样例进度" : "窗口活动", icon: "☷" }, { id: "team", label: "运行任务", icon: "↗" }, { id: "policy", label: "团队策略", icon: "⚙" }] as const).map((tab) => <button type="button" key={tab.id} aria-current={view === tab.id ? "page" : undefined} className={view === tab.id ? "is-active" : ""} onClick={() => setView(tab.id)}><span aria-hidden="true">{tab.icon}</span>{tab.label}</button>)}</nav><span className="ao-updated">{snapshot ? `${demo ? "模拟快照" : "最近观测"} ${timeLabel(snapshot.observed_at_ms)}` : "等待数据"}</span></div>
        {loading && !snapshot ? <div className="ao-loading" role="status"><span className="ao-spin">✳</span><strong>正在打开你的办公室</strong><p>读取项目与运行中的任务…</p></div> : !project ? <div className="ao-no-project"><OfficeGlyph kind="manager" /><h2>先为团队准备一个项目</h2><p>已登记项目会显示在左侧，闲置项目也可以提前设置团队。</p></div> : view === "team" ? <TeamTaskPanel key={project.id} api={api} projectId={project.id} demo={demo} /> : view === "policy" ? <OfficePolicyPanel key={project.id} project={project} api={api} onSaved={policySaved} /> : <>
          <div className="ao-metrics"><Metric label={demo ? "正在工作的成员" : "有活跃请求的窗口"} value={activeMembers.length.toString().padStart(2, "0")} note={`${members.length} ${demo ? "位可见成员" : "个可见窗口"}`} tone="green" /><Metric label="当前活跃请求" value={members.reduce((sum, member) => sum + member.active_requests, 0).toString().padStart(2, "0")} note={demo ? "模拟请求，未执行任务" : "来自运行时观测"} tone="orange" /><Metric label="团队调度方式" value={project.policy.mode === "auto" ? "自主安排" : "自定义"} note={project.policy.mode === "auto" ? "总经理灵活分工" : `${project.policy.subagents.min}–${project.policy.subagents.max} 位子 Agent`} tone="neutral" /></div>
          <div className="ao-office-body"><div className="ao-main-column">{view === "office" ? <OfficeScene members={sceneMembers} selectedId={selectedMemberId} onSelect={setSelectedMemberId} demo={demo} /> : <section className="ao-card ao-task-board"><div className="ao-card-heading"><div><span className="ao-small-kicker">TEAM ASSIGNMENTS</span><h3>每一份工作，都有着落</h3></div><span className="ao-tag">{members.length} 位成员</span></div><div className="ao-task-cards">{members.map((member) => <button type="button" key={member.id} className={`ao-task-card ${selectedMemberId === member.id ? "is-selected" : ""}`} onClick={() => setSelectedMemberId(member.id)}><div><span className={`ao-state-label is-${member.state}`}><i />{STATE_LABELS[member.state]}</span><small>{ROLE_LABELS[member.role]}</small></div><strong>{member.task ?? "任务说明暂未提供"}</strong><span>{member.label}</span><Progress value={member.progress} /></button>)}{members.length === 0 && <p className="ao-muted">当前没有已观测到的成员。团队策略仍然可用。</p>}</div></section>}
            <section className="ao-card ao-activity-card"><div className="ao-card-heading"><h3>工作动态 <small>最近活动快照</small></h3><span className="ao-tag">{demo ? "演示数据" : "已观测"}</span></div><div className="ao-timeline">{[...members].filter((member) => member.last_activity_at_ms !== null).sort((a, b) => (b.last_activity_at_ms ?? 0) - (a.last_activity_at_ms ?? 0)).slice(0, 4).map((member) => <button type="button" className="ao-timeline-row" key={member.id} onClick={() => setSelectedMemberId(member.id)}><time>{timeLabel(member.last_activity_at_ms)}</time><i className={`ao-timeline-dot is-${member.state}`} /><div><strong>{member.label}<span>{STATE_LABELS[member.state]}</span></strong><p>{member.task ?? "当前活动没有任务描述"}</p></div><span className="ao-timeline-arrow">↗</span></button>)}{!members.some((member) => member.last_activity_at_ms !== null) && <p className="ao-empty-activity">新任务开始后，可观测的活动会出现在这里。</p>}</div></section>
          </div><aside className="ao-inspector"><MemberDetails member={selectedMember} members={members} demo={demo} /><section className="ao-policy-preview"><div><span>团队运行规则</span><button type="button" onClick={() => setView("policy")}>调整 ↗</button></div><strong>{project.policy.mode === "auto" ? "总经理自主安排" : "按你的边界开展工作"}</strong><p>{project.policy.mode === "auto" ? "人数、工具与分工，由总经理按任务安排。" : `${project.policy.subagents.min}–${project.policy.subagents.max} 位子 Agent · 每位 ${project.policy.cli_per_agent.min}–${project.policy.cli_per_agent.max} 次 CLI`}</p><span>{snapshot?.enforcement === "managed_tasks" ? "已关联 Server 登记任务，可在运行任务页查看" : "当前支持保存与委派计划验证"}</span></section></aside></div>
        </>}
        {snapshot && snapshot.limitations.length > 0 && <details className="ao-data-note"><summary>数据与执行边界 <span>{snapshot.limitations.length}</span></summary><ul>{snapshot.limitations.map((limitation) => <li key={limitation}>{limitation}</li>)}</ul></details>}
      </main>
    </div>
  </div>;
}

function Metric({ label, value, note, tone }: { label: string; value: string; note: string; tone: string }) {
  return <div className={`ao-metric is-${tone}`}><span>{label}</span><div><strong>{value}</strong><small>{note}</small></div><i aria-hidden="true" /></div>;
}

function Progress({ value }: { value: number | null }) {
  const progress = progressValue(value);
  return <div className="ao-progress"><div><span>任务进度</span><strong>{progress === null ? "未提供" : `${Math.round(progress)}%`}</strong></div><span className={`ao-progress-track ${progress === null ? "is-unknown" : ""}`} role="progressbar" aria-label="任务进度" aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress ?? undefined} aria-valuetext={progress === null ? "未提供" : `${Math.round(progress)}%`}><i style={{ width: `${progress ?? 0}%` }} /></span></div>;
}

function MemberDetails({ member, members, demo }: { member?: OfficeMember; members: OfficeMember[]; demo: boolean }) {
  const parent = members.find((item) => item.id === member?.parent_id);
  const children = members.filter((item) => item.parent_id === member?.id);
  if (!member) return <section className="ao-card ao-member-detail ao-member-empty"><span className="ao-small-kicker">MEET THE TEAM</span><OfficeGlyph kind="agent" color="#a7b49a" /><h3>等待成员入驻</h3><p>点击办公室中的小人，即可查看它的任务、状态和分工。</p></section>;
  return <section className="ao-card ao-member-detail"><div className="ao-detail-heading"><span className="ao-small-kicker">MEMBER PROFILE</span><span className="ao-tag">{ROLE_LABELS[member.role]}</span></div><div className="ao-member-hero"><OfficeGlyph kind={roleKind(member)} residentId={member.id} color={member.role === "manager" ? "#527665" : "#b17f5b"} working={member.state === "working"} /></div><h2>{member.label}</h2><span className={`ao-state-label is-${member.state}`}><i />{STATE_LABELS[member.state]}</span><div className="ao-current-task"><span>正在负责</span><p>{member.task ?? "运行时尚未提供具体任务说明。"}</p></div><Progress value={member.progress} /><dl className="ao-member-facts"><div><dt>汇报对象</dt><dd>{member.role === "manager" && demo ? "你 · 大老板" : parent?.label ?? "未提供关联证据"}</dd></div><div><dt>协作成员</dt><dd>{children.length ? `${children.length} 位直属成员` : "暂无已确认下属"}</dd></div>{member.provider_id && <div><dt>搭档工具</dt><dd>{member.provider_id === "workbuddy" ? "WorkBuddy" : "Antigravity"}</dd></div>}<div><dt>使用模型</dt><dd>{member.model ?? "未提供"}</dd></div><div><dt>活跃请求</dt><dd>{member.active_requests} 项</dd></div><div><dt>最近活动</dt><dd>{timeLabel(member.last_activity_at_ms)}</dd></div></dl>{children.length > 0 && <div className="ao-delegates"><span>已委派给</span>{children.map((child) => <div key={child.id}><i className={`ao-status-dot ${child.state === "working" ? "" : "is-muted"}`} /><strong>{child.label}</strong><small>{ROLE_LABELS[child.role]}</small></div>)}</div>}<div className="ao-member-note">{demo ? "这里展示模拟的任务关系与进度。" : "只展示运行时提供的关系与进度。"}</div></section>;
}
