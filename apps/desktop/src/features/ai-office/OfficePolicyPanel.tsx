import { useEffect, useId, useState } from "react";
import type { AiOfficeApi, CliCatalog, CliProviderId, OfficePlan, OfficePlanEvaluation, OfficePolicy, OfficePolicyInput, OfficeProject, OfficeProvider } from "./types";

const PROVIDERS: { id: CliProviderId; name: string; mark: string }[] = [
  { id: "workbuddy", name: "WorkBuddy", mark: "W" },
  { id: "antigravity", name: "Antigravity", mark: "A" },
];

export function officeError(error: unknown, nextActionPrefix = ""): string {
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
    const nextAction = "next_action" in error && typeof error.next_action === "string" ? error.next_action.trim() : "";
    // Display only bounded human guidance as text, never diagnostic objects.
    return nextAction && nextAction.length <= 2048 ? `${error.message} ${nextActionPrefix}${nextAction}` : error.message;
  }
  return typeof error === "string" ? error : "请求未完成，请重试。";
}

function copyInput(policy: OfficePolicyInput): OfficePolicyInput {
  return { mode: policy.mode, subagents: { ...policy.subagents }, cli_per_agent: { ...policy.cli_per_agent }, providers: policy.providers.map((provider) => ({ ...provider })) };
}

export function OfficePolicyPanel({ project, api, onSaved }: { project: OfficeProject; api: AiOfficeApi; onSaved: (policy: OfficePolicy) => void }) {
  const [draft, setDraft] = useState(() => copyInput(project.policy));
  const [revision, setRevision] = useState(project.policy.revision);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [reloading, setReloading] = useState(false);
  const [catalogs, setCatalogs] = useState<Partial<Record<CliProviderId, CliCatalog>>>({});
  const [refreshing, setRefreshing] = useState<Partial<Record<CliProviderId, boolean>>>({});
  const [catalogErrors, setCatalogErrors] = useState<Partial<Record<CliProviderId, string>>>({});
  const [planTask, setPlanTask] = useState("完成当前项目任务，并检查交付结果");
  const [planAgents, setPlanAgents] = useState(2);
  const [planCliCalls, setPlanCliCalls] = useState(0);
  const [planProvider, setPlanProvider] = useState<CliProviderId>("workbuddy");
  const [planModel, setPlanModel] = useState("");
  const [planEffort, setPlanEffort] = useState("");
  const [evaluating, setEvaluating] = useState(false);
  const [evaluation, setEvaluation] = useState<OfficePlanEvaluation | null>(null);
  const [evaluationError, setEvaluationError] = useState("");
  const [diagnosticsOpen, setDiagnosticsOpen] = useState(false);
  const diagnosticsId = useId();

  useEffect(() => {
    if (!dirty) {
      setDraft(copyInput(project.policy));
      setRevision(project.policy.revision);
    }
  }, [project.policy, dirty]);

  function change(next: OfficePolicyInput) {
    setDraft(next); setDirty(true); setError(""); setMessage(""); setEvaluation(null);
  }
  function updateProvider(id: CliProviderId, values: Partial<OfficeProvider>) {
    const existing = draft.providers.find((provider) => provider.provider_id === id);
    const provider: OfficeProvider = { provider_id: id, enabled: false, model: null, effort: null, ...existing, ...values };
    change({ ...draft, providers: [...draft.providers.filter((item) => item.provider_id !== id), provider] });
  }
  async function refreshCatalog(id: CliProviderId) {
    setRefreshing((current) => ({ ...current, [id]: true }));
    setCatalogErrors((current) => ({ ...current, [id]: "" }));
    try {
      const catalog = await api.refreshCli(id);
      setCatalogs((current) => ({ ...current, [id]: catalog }));
    } catch (cause) {
      setCatalogErrors((current) => ({ ...current, [id]: officeError(cause) }));
    } finally {
      setRefreshing((current) => ({ ...current, [id]: false }));
    }
  }
  const invalidRange = draft.mode === "custom" && (draft.subagents.min > draft.subagents.max || draft.cli_per_agent.min > draft.cli_per_agent.max);
  const minimumCliCalls = (draft.subagents.min + 1) * draft.cli_per_agent.min;
  const invalidBudget = draft.mode === "custom" && minimumCliCalls > 32;
  const maximumCliCalls = Math.min((draft.subagents.max + 1) * draft.cli_per_agent.max, 32);
  async function save() {
    if (invalidRange || invalidBudget || saving) return;
    setSaving(true); setError(""); setMessage("");
    try {
      const policy = await api.savePolicy(project.id, revision, draft);
      setDraft(copyInput(policy)); setRevision(policy.revision); setDirty(false);
      setMessage(`已保存到「${project.name}」，策略版本 ${policy.revision}`);
      onSaved(policy);
    } catch (cause) {
      setError(officeError(cause, "请先点击「载入最新策略」核查。 "));
    } finally { setSaving(false); }
  }
  async function reload() {
    setReloading(true); setError(""); setMessage("");
    try {
      const snapshot = await api.snapshot(project.id);
      const latest = snapshot.projects.find((item) => item.id === project.id)?.policy;
      if (!latest) throw new Error("当前项目未在最新快照中，请刷新项目列表。");
      setDraft(copyInput(latest)); setRevision(latest.revision); setDirty(false); setEvaluation(null);
      onSaved(latest); setMessage("已载入最新策略，本地草稿已替换。");
    } catch (cause) { setError(officeError(cause)); }
    finally { setReloading(false); }
  }
  async function evaluate() {
    if (revision === 0 || dirty || evaluating) return;
    setEvaluating(true); setEvaluation(null); setEvaluationError("");
    const calls = Array.from({ length: planCliCalls }, () => ({ provider_id: planProvider, model: planModel, ...(planEffort ? { effort: planEffort } : {}) }));
    const plan: OfficePlan = {
      manager: { id: "manager", task: planTask, cli_calls: calls },
      subagents: Array.from({ length: planAgents }, (_, index) => ({ id: `agent-${index + 1}`, task: `${planTask} · 子任务 ${index + 1}`, cli_calls: calls.map((call) => ({ ...call })) })),
    };
    try { setEvaluation(await api.evaluatePlan(project.id, revision, plan)); }
    catch (cause) { setEvaluationError(officeError(cause)); }
    finally { setEvaluating(false); }
  }
  const planCatalog = catalogs[planProvider];
  const policyChangedElsewhere = project.policy.revision !== revision;

  return <div className="ao-policy-layout">
    <div className="ao-policy-main">
      <section className="ao-card ao-policy-card">
        <div className="ao-card-heading"><div><span className="ao-small-kicker">MANAGEMENT STYLE</span><h3>把团队，交给你的方式</h3></div><span className="ao-tag">仅当前项目</span></div>
        <p className="ao-muted">项目闲置时也可以预先设置；设置会保存到这个项目。</p>
        <div className="ao-mode-options" role="group" aria-label="调度模式">
          <button type="button" className={`ao-mode-card ${draft.mode === "auto" ? "is-active" : ""}`} disabled={saving} aria-pressed={draft.mode === "auto"} onClick={() => change({ ...draft, mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 } })}><span className="ao-mode-icon">✳</span><strong>总经理自主安排 <small>默认</small></strong><span>由 AI 判断任务规模、分工与 CLI 使用。</span><i /></button>
          <button type="button" className={`ao-mode-card ${draft.mode === "custom" ? "is-active" : ""}`} disabled={saving} aria-pressed={draft.mode === "custom"} onClick={() => change({ ...draft, mode: "custom" })}><span className="ao-mode-icon">≡</span><strong>我来设定边界</strong><span>明确子 Agent 人数、CLI 次数与模型。</span><i /></button>
        </div>
        <div className={`ao-range-grid ${draft.mode === "auto" ? "is-inactive" : ""}`}>
          <RangeFields title="总经理可委派的子 Agent" caption="0 人允许独立完成；最少人数按已核实派发检查。" name="subagents" value={draft.subagents} max={32} disabled={draft.mode === "auto" || saving} onChange={(value) => change({ ...draft, subagents: value })} />
          <RangeFields title="每个 Agent 的 CLI 调用" caption="包含总经理；最少次数按已核实调用检查。" name="cli" value={draft.cli_per_agent} max={32} disabled={draft.mode === "auto" || saving} onChange={(value) => change({ ...draft, cli_per_agent: value })} />
        </div>
        {invalidRange && <p role="alert" className="ao-error">最少数量不能大于最多数量。</p>}
        {invalidBudget && <p role="alert" className="ao-error">最低分工需要 {minimumCliCalls} 次 CLI，超过每轮团队总预算 32 次。请减少最少人数或每人最少调用次数。</p>}
        <p className="ao-inline-note">新聊天最多可委派 {draft.subagents.max} 位子 Agent，{draft.mode === "custom" ? `每人最多 ${draft.cli_per_agent.max} 次 CLI；` : "CLI 次数由总经理分配；"}包含总经理，每轮团队总预算最多 {maximumCliCalls} 次，不要求用满。</p>
        {draft.mode === "auto" && <p className="ao-inline-note">自主模式不设置自定义最低执行门槛，人数与工具由总经理安排；已冻结的调用预算和运行权限仍有效。</p>}
      </section>
      <section className="ao-card ao-policy-card">
        <div className="ao-card-heading"><div><span className="ao-small-kicker">CLI TOOLBOX</span><h3>为团队配好工具</h3></div><span className="ao-tag">模型来自 CLI 发现</span></div>
        <p className="ao-muted">刷新会查询本机 CLI 的模型与思考选项。帮助目录不代表账号可用性。</p>{draft.mode === "auto" && <p className="ao-inline-note">当前由总经理自主选择模型。想固定模型，请先选择上方的「我来设定边界」。</p>}
        <div className="ao-provider-list">{PROVIDERS.map(({ id, name, mark }) => {
          const provider = draft.providers.find((item) => item.provider_id === id);
          const catalog = catalogs[id];
          const models = catalog?.models ?? [];
          const efforts = catalog?.efforts ?? [];
          return <div className="ao-provider" key={id}>
            <div className="ao-provider-heading"><span className={`ao-provider-mark is-${id}`}>{mark}</span><div><strong>{name}</strong><span>{catalog ? catalog.installed ? catalog.version ? `已发现 · ${catalog.version}` : "已发现本机 CLI" : "本机不可用" : "尚未发现"}</span></div><label className="ao-toggle"><input type="checkbox" aria-label={`启用 ${name}`} checked={provider?.enabled ?? false} disabled={draft.mode === "auto" || saving} onChange={(event) => updateProvider(id, { enabled: event.target.checked })} /><span /></label></div>
            <div className="ao-provider-fields"><label>模型<select aria-label={`${name} 模型`} value={provider?.model ?? ""} disabled={draft.mode === "auto" || !provider?.enabled || saving} onChange={(event) => updateProvider(id, { model: event.target.value || null })}><option value="">由总经理选择</option>{provider?.model && !models.includes(provider.model) && <option value={provider.model}>{provider.model} · 待发现验证</option>}{models.map((model) => <option value={model} key={model}>{model}</option>)}</select></label><button type="button" className="ao-button ao-button-subtle" disabled={Boolean(refreshing[id])} onClick={() => void refreshCatalog(id)} aria-label={`刷新 ${name} 模型列表`}><span className={refreshing[id] ? "ao-spin" : ""}>↻</span>{refreshing[id] ? "发现中…" : "刷新模型"}</button></div>
            <label className="ao-effort-field">思考强度<select aria-label={`${name} 思考强度`} value={provider?.effort ?? ""} disabled={draft.mode === "auto" || !provider?.enabled || efforts.length === 0 || saving} onChange={(event) => updateProvider(id, { effort: event.target.value || null })}><option value="">{efforts.length ? "由总经理选择" : "未发现支持的思考选项"}</option>{provider?.effort && !efforts.includes(provider.effort) && <option value={provider.effort}>{provider.effort} · 待发现验证</option>}{efforts.map((effort) => <option key={effort} value={effort}>{effort}</option>)}</select></label>
            {catalog && <p className={`ao-catalog-note ${catalog.status === "error" ? "ao-error" : ""}`}>{catalog.message}<span>{catalog.accountAvailabilityVerified ? "账号可用性已核验" : "账号可用性未核验"} · {catalog.source === "live_catalog" ? "实时目录" : catalog.source === "installed_help" ? "本机 CLI 帮助" : "无可用目录"}</span></p>}
            {catalogErrors[id] && <p className="ao-error" role="alert">{catalogErrors[id]}</p>}
          </div>;
        })}</div>
      </section>
      <div className="ao-save-bar"><div><strong>{dirty ? "有尚未保存的修改" : revision === 0 ? "首次使用请保存团队策略" : "没有待保存修改"}</strong><span>当前版本 {revision} · {project.name}</span></div><button type="button" className="ao-button ao-button-primary" disabled={saving || reloading || invalidRange || invalidBudget || (!dirty && revision > 0)} onClick={() => void save()}>{saving ? "保存中…" : "保存团队策略"}</button></div>
      {message && <p className="ao-success" role="status">{message}</p>}
      {(error || policyChangedElsewhere) && <div className="ao-policy-error" role="alert"><p>{error || "策略已在其他位置更新。当前草稿保留，请载入最新策略后再编辑。"}</p><button type="button" className="ao-button ao-button-subtle" disabled={reloading || saving} onClick={() => void reload()}>{reloading ? "载入中…" : "载入最新策略（替换草稿）"}</button></div>}
      <section className="ao-advanced-diagnostics">
        <button type="button" className="ao-advanced-toggle" aria-expanded={diagnosticsOpen} aria-controls={diagnosticsId} onClick={() => setDiagnosticsOpen((open) => !open)}><span>高级诊断</span><span aria-hidden="true">{diagnosticsOpen ? "收起 −" : "展开 +"}</span></button>
        {diagnosticsOpen && <div id={diagnosticsId} className="ao-plan-card"><h3>假设计划校验</h3><p className="ao-muted">仅检查下面的假设计划是否符合已保存策略，不修改设置或启动任务。</p><label>假设的总任务<input aria-label="验证计划的总任务" disabled={evaluating} value={planTask} onChange={(event) => { setPlanTask(event.target.value); setEvaluation(null); }} /></label><div className="ao-plan-counts"><label>假设的子 Agent 人数<input type="number" aria-label="计划子 Agent 数量" disabled={evaluating} min={0} max={32} value={planAgents} onChange={(event) => { setPlanAgents(boundedCount(event.target.value, 32)); setEvaluation(null); }} /></label><label>假设的每人 CLI 次数<input type="number" aria-label="计划每个 Agent CLI 次数" disabled={evaluating} min={0} max={32} value={planCliCalls} onChange={(event) => { setPlanCliCalls(boundedCount(event.target.value, 32)); setEvaluation(null); }} /></label></div>
        {planCliCalls > 0 && <div className="ao-plan-cli"><label>假设的 CLI 工具<select aria-label="验证计划 CLI 工具" value={planProvider} onChange={(event) => { setPlanProvider(event.target.value as CliProviderId); setPlanModel(""); setPlanEffort(""); setEvaluation(null); }}><option value="workbuddy">WorkBuddy</option><option value="antigravity">Antigravity</option></select></label><label>假设的调用模型<select aria-label="验证计划模型" value={planModel} onChange={(event) => { setPlanModel(event.target.value); setEvaluation(null); }}><option value="">先刷新并选择模型</option>{planCatalog?.models.map((model) => <option key={model} value={model}>{model}</option>)}</select></label>{Boolean(planCatalog?.efforts.length) && <label>假设的思考强度<select aria-label="验证计划思考强度" value={planEffort} onChange={(event) => { setPlanEffort(event.target.value); setEvaluation(null); }}><option value="">请选择</option>{planCatalog?.efforts.map((effort) => <option key={effort} value={effort}>{effort}</option>)}</select></label>}</div>}
        <button type="button" className="ao-button ao-button-primary" onClick={() => void evaluate()} disabled={evaluating || dirty || revision === 0 || !planTask.trim() || (planCliCalls > 0 && (!planModel || (Boolean(planCatalog?.efforts.length) && !planEffort)))}>{evaluating ? "验证中…" : "验证委派计划"}<span>↗</span></button>
        {(dirty || revision === 0) && <p className="ao-inline-note">诊断使用已保存的策略，请先保存上方设置。</p>}
        {evaluation && <div className={`ao-evaluation ${evaluation.admitted ? "is-admitted" : "is-rejected"}`} role="status"><strong>{evaluation.admitted ? "✓ 计划符合已保存策略" : "计划未通过策略检查"}</strong><span>{evaluation.subagent_count} 位子 Agent · {evaluation.cli_call_count} 次 CLI · 策略 v{evaluation.policy_revision}</span>{evaluation.violations.length > 0 && <ul>{evaluation.violations.map((violation, index) => <li key={`${violation.code}-${index}`}>{violation.agent_id ? `${violation.agent_id}：` : ""}{violation.message}</li>)}</ul>}<small>仅验证，没有启动 Agent 或 CLI 进程。</small></div>}
        {evaluationError && <p className="ao-error" role="alert">{evaluationError}</p>}
        </div>}
      </section>
    </div>
  </div>;
}

function boundedCount(value: string, max: number): number {
  return Math.max(0, Math.min(max, Math.trunc(Number(value) || 0)));
}

function RangeFields({ title, caption, name, value, max, disabled, onChange }: { title: string; caption: string; name: string; value: { min: number; max: number }; max: number; disabled: boolean; onChange: (value: { min: number; max: number }) => void }) {
  return <fieldset className="ao-count-range" disabled={disabled}><legend>{title}</legend><p>{caption}</p><div><label htmlFor={`ao-${name}-min`}>最少<input id={`ao-${name}-min`} aria-label={`${title}最少数量`} type="number" min={0} max={max} value={value.min} onChange={(event) => onChange({ ...value, min: boundedCount(event.target.value, max) })} /></label><span>—</span><label htmlFor={`ao-${name}-max`}>最多<input id={`ao-${name}-max`} aria-label={`${title}最多数量`} type="number" min={0} max={max} value={value.max} onChange={(event) => onChange({ ...value, max: boundedCount(event.target.value, max) })} /></label></div></fieldset>;
}
