import { servicesReady } from "../../models/topology";
import { ProjectAvailability } from "./ProjectAvailability";
import type { DesktopState } from "../../models/topology";
import { useLocale } from "../../i18n/locale";
import {
  readinessNextAction,
  runtimeLabel,
  readinessSummary,
  runnerReadinessLabel,
  serverReadinessLabel,
} from "../../i18n/presentation";

interface DashboardProps {
  state: DesktopState;
  refreshing: boolean;
  onRefresh: () => void;
  onResumeRuntime: () => void;
  onConnectChatGpt: () => void;
  onChangeSetup: () => void;
  onNavigate: (page: "projects" | "connection" | "activity") => void;
  onStopQuickShare: () => void;
  onStopRuntime: () => void;
}

export function Dashboard({
  state,
  refreshing,
  onRefresh,
  onResumeRuntime,
  onConnectChatGpt,
  onChangeSetup,
  onNavigate,
  onStopQuickShare,
  onStopRuntime,
}: DashboardProps) {
  const { t } = useLocale();
  const isQuickShare = state.topology?.experience === "quick_share";
  const operationBusy = Boolean(state.current_operation);
  const connectionVerified = chatgptActivityObserved(state) && state.regular_tunnel?.status !== "error";
  const canResumeRuntime = !isQuickShare && Boolean(state.topology) && !servicesReady(state.readiness);
  const canConnectChatGpt = !isQuickShare &&
    state.topology?.server.kind === "local" &&
    servicesReady(state.readiness) &&
    state.openai_tunnel_configured &&
    !state.regular_tunnel;
  const summary = servicesReady(state.readiness) ? readinessSummary(state.readiness.summary_kind, state.readiness.summary, t) : runtimeLabel(state, t);
  const nextAction = readinessNextAction(
    state.readiness.next_action_kind,
    state.readiness.next_action,
    t,
  );
  return (
    <section
      className="page-section dashboard-page"
      aria-labelledby="home-title"
      aria-busy={refreshing}
      data-webcodex-page="home"
    >
      <div className="page-heading-row">
        <div>
          <div className="eyebrow">{t("home.eyebrow")}</div>
          <h1 id="home-title">WebCodex</h1>
          <p className="lede">{t("workspace.description")}</p>
        </div>
        <button
          className="secondary-button"
          onClick={onRefresh}
          disabled={refreshing || operationBusy}
          data-webcodex-action="refresh-runtime"
        >
          {refreshing ? t("home.checking") : t("home.refresh")}
        </button>
      </div>

      <div
        className={`readiness-banner ${state.readiness.ready_for_chatgpt ? "ready" : "pending"}`}
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        <div>
          <span className="section-kicker">{t("home.overall")}</span>
          <strong>{state.readiness.ready_for_chatgpt ? t("home.readyToUse") : summary}</strong>
        </div>
        <div className="readiness-actions">
          {canResumeRuntime && (
            <button className="primary-button" onClick={onResumeRuntime} disabled={operationBusy || refreshing} data-webcodex-action="resume-runtime">
              {t("home.resumeRuntime")}
            </button>
          )}
          {canConnectChatGpt && (
            <button className="primary-button" onClick={onConnectChatGpt} disabled={operationBusy} data-webcodex-action="connect-chatgpt">
              {t("home.connectChatGpt")}
            </button>
          )}
          {servicesReady(state.readiness) && !canConnectChatGpt && !connectionVerified && (
            <button className="primary-button" onClick={() => onNavigate("connection")}>{t("workspace.connectionSettings")}</button>
          )}
          {nextAction && !canResumeRuntime && !canConnectChatGpt && <span>{nextAction}</span>}
        </div>
      </div>

      <ProjectAvailability state={state} refreshing={refreshing} onManage={() => onNavigate("projects")} />

      {state.quick_share && (
        <div className="handoff-card">
          <div>
            <span className="section-kicker">{t("home.quickShareHandoff")}</span>
            <strong>{state.quick_share.ready_for_chatgpt ? t("home.handoffReady") : t("home.handoffAction")}</strong>
            {state.quick_share.mcp_url && <code>{state.quick_share.mcp_url}</code>}
            <span>{quickShareClipboardLabel(state.quick_share.clipboard_state, state.quick_share.clipboard_contains, t)}</span>
          </div>
          <button className="danger-button" onClick={onStopQuickShare} disabled={operationBusy} data-webcodex-action="stop-quick-share">{t("home.stopShare")}</button>
        </div>
      )}

      {!isQuickShare && (
        <details className="workflow-guide" open={!connectionVerified}>
          <summary>{t("workspace.progress")}</summary>
          <ol className="workflow-steps" aria-label={t("workspace.progress")}>
            <li className={servicesReady(state.readiness) ? "complete" : ""}>
              <span className="step-number" aria-hidden="true">01</span>
              <strong>{t("workspace.prepare")}</strong>
              <p>{servicesReady(state.readiness) ? t("sidebar.runtimeReady") : t("workspace.prepareHint")}</p>
            </li>
            <li className={servicesReady(state.readiness) && (state.readiness.ready_for_chatgpt || state.regular_tunnel?.ready_for_chatgpt) ? "complete" : ""}>
              <span className="step-number" aria-hidden="true">02</span>
              <strong>{t("workspace.connect")}</strong>
              <p>{connectionExplanation(state, t)}</p>
            </li>
            <li className={connectionVerified ? "complete" : ""}>
              <span className="step-number" aria-hidden="true">03</span>
              <strong>{t("workspace.verify")}</strong>
              <p>{!servicesReady(state.readiness) ? t("workspace.afterStart") : connectionVerified ? t("home.connectionObserved") : t("workspace.verifyHint")}</p>
            </li>
          </ol>
        </details>
      )}

      <div className="status-grid" aria-label={t("home.components")}>
          <StatusCard
            title={t("home.service")}
            value={serviceLabel(state, t)}
            state={state.readiness.server}
            explanation={serviceExplanation(state, t)}
          />
          <StatusCard
            title={t("home.runner")}
            value={runnerReadinessLabel(state.readiness.runner, t)}
            state={state.readiness.runner}
            explanation={t("home.runnerExplanation")}
          />
          <StatusCard
            title={t("home.connection")}
            value={connectionLabel(state, t)}
            state={connectionCardState(state)}
            explanation={connectionExplanation(state, t)}
          />
        </div>

      <details className="runtime-details">
        <summary>{t("workspace.diagnostics")}</summary>
        {!isQuickShare && state.topology && (
          <div className="runtime-actions">
            <span>{t("home.runtimeOwnership")}</span>
            <div className="runtime-action-buttons">
              <button className="secondary-button" onClick={onChangeSetup} disabled={operationBusy} data-webcodex-action="change-runtime-setup">{t("home.changeSetup")}</button>
              {servicesReady(state.readiness) && (
                <button className="secondary-button" onClick={onStopRuntime} disabled={operationBusy} data-webcodex-action="stop-runtime">{t("home.stopRuntime")}</button>
              )}
            </div>
          </div>
        )}
      </details>
    </section>
  );
}

function StatusCard({
  title,
  value,
  state,
  explanation,
}: {
  title: string;
  value: string;
  state: string;
  explanation: string;
}) {
  const tone = state === "ready" || state === "remote_ready" ? "ready" : state === "error" ? "error" : state === "unknown" ? "unknown" : "pending";
  return (
    <article className="status-card">
      <span className="section-kicker">{title}</span>
      <div className="status-value"><i className={`status-dot ${tone}`} aria-hidden="true" />{value}</div>
      <p>{explanation}</p>
    </article>
  );
}

type Translate = ReturnType<typeof useLocale>["t"];

function serviceLabel(state: DesktopState, t: Translate) {
  if (state.readiness.server === "ready" && state.topology?.server.kind === "remote") return t("common.connected");
  return serverReadinessLabel(state.readiness.server, t);
}

function serviceExplanation(state: DesktopState, t: Translate) {
  if (state.topology?.server.kind === "remote") return t("home.serviceRemote", { url: state.topology.server.url });
  return t("home.serviceLocal");
}

function quickShareClipboardLabel(state: string, contains: string, t: Translate) {
  if (state !== "copied") return t("clipboard.unavailable");
  if (contains === "tunnel_id") return t("clipboard.tunnelId");
  if (contains === "bearer_credential") return t("clipboard.bearer");
  if (contains === "sensitive_mcp_url") return t("clipboard.sensitiveUrl");
  return t("clipboard.copied");
}

function chatgptActivityObserved(state: DesktopState) {
  return servicesReady(state.readiness) && Boolean(state.chatgpt_activity?.observed);
}

function connectionCardState(state: DesktopState) {
  if (!servicesReady(state.readiness)) return "unknown";
  if (state.regular_tunnel?.status === "error") return "error";
  if (chatgptActivityObserved(state)) return "ready";
  return state.readiness.exposure;
}

function connectionLabel(state: DesktopState, t: Translate) {
  if (!servicesReady(state.readiness)) return runtimeLabel(state, t);
  if (chatgptActivityObserved(state) && state.regular_tunnel?.status !== "error") {
    return t("home.connectionObservedLabel");
  }
  if (state.regular_tunnel?.status === "ready" && state.regular_tunnel.ready_for_chatgpt) return t("connection.tunnelReady");
  const exposure = state.topology?.exposure;
  if (!exposure || exposure.kind === "none") return t("connection.noDesktopTunnel");
  if (exposure.kind === "cloudflare") return "Cloudflare";
  if (exposure.kind === "open_ai_tunnel") return "OpenAI Secure Tunnel";
  return "Existing HTTPS";
}

function connectionExplanation(state: DesktopState, t: Translate) {
  if (!servicesReady(state.readiness)) return t("workspace.afterStart");
  if (state.regular_tunnel?.status !== "error" && chatgptActivityObserved(state)) {
    return t("home.connectionObserved");
  }
  if (state.regular_tunnel?.status === "ready" && state.regular_tunnel.ready_for_chatgpt) return t("home.connectionTunnelReady");
  if (state.readiness.exposure === "remote_ready") return t("home.connectionRemoteReady");
  if (state.readiness.exposure === "local_ready") return t("home.connectionLocalReady");
  return t("home.connectionUnverified");
}
