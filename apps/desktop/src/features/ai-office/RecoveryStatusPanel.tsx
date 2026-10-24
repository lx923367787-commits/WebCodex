import { OfficeGlyph } from "./OfficeScene";
import "./ai-office.css";
import "./recovery-status.css";

export type RecoveryState = "waiting_source_stop" | "queued" | "starting" | "running" | "succeeded" | "failed" | "blocked" | "cancelled" | "unknown" | "confirmed_not_started" | "verified_terminal";

// Read-only projection fields agreed with the recovery coordinator. This local
// display contract does not define an API or grant permission to start work.
export interface RecoveryObservation {
  fallback_id: string;
  root_task_id: string;
  source_task_id: string;
  source_attempt_id: string;
  source_job_id: string | null;
  fallback_agent_id: string | null;
  policy_revision: number;
  reason_code: string;
  state: RecoveryState;
  blocked_reason: string | null;
  revision: number;
}

export interface RecoveryStatusPanelProps {
  observation?: RecoveryObservation | null;
  availability?: "not_connected" | "host_unconfigured" | "connected";
  /** Verified slot observation supplied by the host, never inferred from state. */
  slotOccupancy?: 0 | 1 | null;
  nickname?: string;
  source?: "live" | "sample";
}

const STATES: Record<RecoveryState, { label: string; detail: string; tone: string }> = {
  waiting_source_stop: { label: "等待原 CLI 停止确认", detail: "等待原 CLI 进程树的停止证据，尚未开始接管。", tone: "waiting" },
  queued: { label: "等待救场", detail: "恢复请求已排队，等待恢复槽与宿主就绪。", tone: "waiting" },
  starting: { label: "救场伙伴启动中", detail: "已进入启动阶段，正在等待启动结果。", tone: "waiting" },
  running: { label: "正在救场", detail: "救场伙伴正在处理原任务的恢复工作。", tone: "active" },
  succeeded: { label: "恢复已验收", detail: "恢复任务已通过验收。", tone: "success" },
  failed: { label: "恢复未成功", detail: "本次恢复未成功，原因与任务关联保留在下方。", tone: "attention" },
  blocked: { label: "恢复受阻", detail: "恢复目前无法继续，请查看阻塞原因。", tone: "attention" },
  cancelled: { label: "恢复已取消", detail: "恢复记录报告为已取消。", tone: "muted" },
  unknown: { label: "恢复结果待核实", detail: "发送或执行结果尚未确认，不能据此判断可以重新发起任务。", tone: "attention" },
  confirmed_not_started: { label: "已核实未启动", detail: "宿主已确认本次恢复没有启动。", tone: "muted" },
  verified_terminal: { label: "宿主终态已核实", detail: "恢复执行已结束，业务结果仍须另行验收；终态不等于任务成功。", tone: "muted" },
};

export function RecoveryStatusPanel({ observation = null, availability = "not_connected", slotOccupancy = null, nickname, source = "live" }: RecoveryStatusPanelProps) {
  // A disconnected host must never turn supplied sample/stale data into live work.
  const record = availability === "not_connected" && source === "live" ? null : observation;
  const slot = availability === "not_connected" && source === "live" ? null : slotOccupancy;
  const state = record ? STATES[record.state] ?? STATES.unknown : null;
  const banner = availability === "not_connected"
    ? { label: "恢复观测未接入", detail: "尚未连接恢复记录来源，当前无法确认是否有救场伙伴在工作。" }
    : availability === "host_unconfigured"
      ? { label: "等待配置 GPT 宿主", detail: "无界面的 GPT 创建、查询和停止接口尚未配置，当前不能启动救场伙伴。" }
      : null;

  return <section className="ao-recovery-panel" aria-label="救场伙伴状态">
    <header className="ao-recovery-heading"><div><span>RECOVERY COMPANION</span><h3>救场伙伴</h3></div><span className={"ao-recovery-source " + (source === "sample" ? "is-sample" : "")}>{source === "sample" ? "SAMPLE · 样式样例" : "只读观测"}</span></header>
    {source === "sample" && <p className="ao-recovery-sample" role="note">以下为展示样例，没有启动真实恢复任务。</p>}
    <div className="ao-recovery-overview">
      <div className="ao-recovery-resident"><OfficeGlyph kind="agent" residentId={record?.fallback_agent_id ?? record?.fallback_id ?? "recovery-standby"} /><div><strong>{record ? nickname || "救场伙伴" : "等待伙伴入驻"}</strong><span>独立恢复成员</span></div></div>
      <div className="ao-recovery-slot"><span>恢复槽占用</span><strong aria-label={slot === null ? "恢复槽占用未核实，上限 1" : "恢复槽占用 " + slot + "，上限 1"}>{slot ?? "—"}<small>/ 1</small></strong><span>{slot === null ? "尚无已核实读数" : slot === 1 ? "恢复槽已占用" : "恢复槽空闲"}</span></div>
    </div>
    <p className="ao-recovery-rule">每个根任务拥有 1 个独立恢复槽，不计入常规子 Agent 人数上下限。</p>
    {banner && <div className="ao-recovery-availability" role="status"><strong>{banner.label}</strong><p>{banner.detail}</p></div>}
    {record && state ? <>
      <div className={"ao-recovery-state is-" + state.tone} role="status"><span><i />{state.label}</span><p>{state.detail}</p></div>
      <div className="ao-recovery-reason"><span>触发原因</span><code>{record.reason_code || "未提供原因代码"}</code>{record.blocked_reason && <p><strong>阻塞原因</strong>{record.blocked_reason}</p>}</div>
      <dl className="ao-recovery-links">
        <LinkField label="原 CLI Job" value={record.source_job_id} empty="尚未关联 Job" prominent />
        <LinkField label="根任务" value={record.root_task_id} />
        <LinkField label="原任务" value={record.source_task_id} />
        <LinkField label="原执行尝试" value={record.source_attempt_id} />
        <LinkField label="救场伙伴 ID" value={record.fallback_agent_id} empty="尚未关联成员" />
        <LinkField label="恢复记录" value={record.fallback_id} />
      </dl>
      <footer className="ao-recovery-footer"><span>策略 v{record.policy_revision} · 记录 v{record.revision}</span><span>原 CLI Job 的状态需独立查看。</span></footer>
    </> : availability === "connected" && <div className="ao-recovery-empty"><strong>当前没有恢复记录</strong><p>有真实恢复记录后，这里会显示救场进展、原 CLI Job 与失败原因。</p></div>}
  </section>;
}

function LinkField({ label, value, empty = "未提供", prominent = false }: { label: string; value: string | null; empty?: string; prominent?: boolean }) {
  return <div className={prominent ? "is-prominent" : ""}><dt>{label}</dt><dd>{value ? <code title={value}>{value}</code> : <span>{empty}</span>}</dd></div>;
}
