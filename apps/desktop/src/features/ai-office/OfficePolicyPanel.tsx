import { useEffect, useRef, useState } from "react";
import type { AiOfficeApi, CliCatalog, CliInstallation, OfficePolicy, OfficeProject } from "./types";

export function officeError(error: unknown, nextActionPrefix = ""): string {
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
    const nextAction = "next_action" in error && typeof error.next_action === "string" ? error.next_action.trim() : "";
    // Display only bounded human guidance as text, never diagnostic objects.
    return nextAction && nextAction.length <= 2048 ? `${error.message} ${nextActionPrefix}${nextAction}` : error.message;
  }
  return typeof error === "string" ? error : "请求未完成，请重试。";
}


export function OfficePolicyPanel({ project, api }: { project: OfficeProject; api: AiOfficeApi; onSaved: (policy: OfficePolicy) => void }) {
  const [installed, setInstalled] = useState<CliInstallation[]>([]);
  const [catalogs, setCatalogs] = useState<Record<string, CliCatalog>>({});
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState<string[]>([]);
  const [error, setError] = useState("");
  const generation = useRef(0);
  async function discover() {
    const current = ++generation.current;
    setBusy(true); setError(""); setInstalled([]); setCatalogs({}); setRefreshing([]);
    try {
      const result = await api.discoverCli(project.id);
      if (current === generation.current) setInstalled(result);
    } catch (cause) {
      if (current === generation.current) setError(officeError(cause));
    } finally { if (current === generation.current) setBusy(false); }
  }
  useEffect(() => { void discover(); return () => { generation.current += 1; }; }, [api, project.id]);
  async function refresh(provider: string) {
    const current = generation.current;
    setRefreshing((previous) => [...previous, provider]); setError("");
    try {
      const result = await api.refreshCli(provider, project.id);
      if (current !== generation.current) return;
      if (result.providerId !== provider) throw new Error("CLI 能力返回的标识不一致，请重新扫描。");
      setCatalogs((previous) => ({ ...previous, [provider]: result }));
    } catch (cause) { if (current === generation.current) setError(officeError(cause)); }
    finally { if (current === generation.current) setRefreshing((previous) => previous.filter((id) => id !== provider)); }
  }
  return <div className="ao-policy-panel">
    <section className="ao-card ao-policy-card"><h3>按任务需要自由协作</h3>
      <p>主 Agent 可以独立完成，也可以调用子 Agent 或 CLI。不强制调用，不设置人数、次数、模型或思考强度限制。</p>
      <p>模型和思考强度可按每次任务选择，也可以沿用工具默认设置。任务是否完成以实际交付和验收结果为准。</p>
      <p className="ao-inline-note">协作采用软约定：各 Agent 和 CLI 各自维护记忆、执行目标和验收标准，不读取或修改其他实例存放这三类内容的文件夹。其余项目目录、工具、执行状态和成果在原有授权范围内正常共享。</p>
      <p className="ao-muted">原有团队数量及模型限制已停用，无需为新聊天另存策略。</p>
    </section>
    <section className="ao-card ao-policy-card"><h3>本机 CLI</h3>
      <p className="ao-muted">从当前项目的 Runner 读取执行目录，与主 Agent 使用同一来源。发现安装、调用配置、Runner 加载和真实执行分别核实。</p>
      <button type="button" className="ao-button ao-button-subtle" disabled={busy} onClick={() => void discover()}>{busy ? "扫描本机 CLI 中…" : "重新扫描本机 CLI"}</button>
      {error && <p role="alert" className="ao-error">{error}</p>}
      {!busy && installed.length === 0 && !error && <p>未发现本机 CLI。主 Agent 仍可独立完成任务。</p>}
      <div className="ao-provider-list">{installed.map((entry) => {
        const catalog = Object.hasOwn(catalogs, entry.providerId) ? catalogs[entry.providerId] : undefined;
        return <article className="ao-provider" key={entry.providerId}><h4>{entry.label}</h4>
          <p>{entry.installed ? "已发现本机安装" : "当前未发现"}{catalog?.version ? ` · ${catalog.version}` : ""}</p>
          <dl className="ao-member-facts" aria-label={`${entry.label} 执行链状态`}>
            <div><dt>发现安装</dt><dd>{entry.installed ? "已发现" : "未发现"}</dd></div>
            <div><dt>调用配置</dt><dd>{entry.adapterAvailable ? "配置完成" : "调用配置未就绪"}</dd></div>
            <div><dt>Runner 加载</dt><dd>{entry.executionReady === true ? "Runner 已加载" : entry.executionReady === false ? "Runner 尚未加载可执行资源" : "加载状态未观测"}</dd></div>
            <div><dt>真实执行</dt><dd>以运行任务中的 Job 状态、输出和退出码为准；目录不证明执行成功。</dd></div>
            {entry.reasonCode && <div><dt>具体原因</dt><dd>{entry.reasonCode}</dd></div>}
          </dl>
          <p>{catalog?.message ?? entry.message}</p>
          <button type="button" className="ao-button ao-button-subtle" disabled={busy || refreshing.includes(entry.providerId) || !entry.installed} onClick={() => void refresh(entry.providerId)} aria-label={`刷新 ${entry.label} 模型列表`}>{refreshing.includes(entry.providerId) ? "发现中…" : "刷新模型"}</button>
          {catalog && <><p>模型：{catalog.models.join("、") || "可沿用 CLI 默认设置"}</p><p>思考选项：{catalog.efforts.join("、") || "使用工具默认设置"}</p><p className="ao-muted">{catalog.accountAvailabilityVerified ? "账号可用性已核验" : "账号可用性未核验"}</p></>}
        </article>;
      })}</div>
    </section>
  </div>;
}
