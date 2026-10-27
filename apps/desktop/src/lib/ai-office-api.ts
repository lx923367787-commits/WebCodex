import { invoke } from "@tauri-apps/api/core";
import { demoNickname } from "./office-nicknames";
import type { AiOfficeApi, OfficeMember, OfficeSnapshot, OfficePolicy, TeamAction } from "../features/ai-office/types";

export class AiOfficeTeamError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
    this.name = "AiOfficeTeamError";
  }
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

async function teamRequest(action: TeamAction, params: Record<string, unknown>): Promise<unknown> {
  if (typeof params.project !== "string" || params.project.trim().length === 0) {
    throw new AiOfficeTeamError("team_project_required", "需要完整的已登记项目 ID。");
  }
  let result: unknown;
  try {
    // One native invocation only. Ambiguous write outcomes are never retried.
    result = await invoke("ai_office_team_call", { action, params });
  } catch (cause) {
    const error = objectValue(cause);
    const message = cause instanceof Error ? cause.message : typeof cause === "string" ? cause : typeof error?.message === "string" ? error.message : "团队服务请求未完成，请刷新观测核实结果。";
    throw new AiOfficeTeamError(typeof error?.code === "string" ? error.code : "team_transport_unavailable", message);
  }
  const envelope = objectValue(result);
  if (envelope?.success === true && Object.hasOwn(envelope, "output")) return envelope.output;
  if (envelope?.success !== false) throw new AiOfficeTeamError("team_invalid_response", "团队服务返回了无法识别的结果。");
  const output = objectValue(envelope.output);
  const error = objectValue(envelope.error);
  const code = typeof output?.error_kind === "string" ? output.error_kind : typeof error?.code === "string" ? error.code : typeof envelope.error === "string" && /^[a-z][a-z0-9_]{2,}$/.test(envelope.error) ? envelope.error : "team_request_failed";
  const message = typeof error?.message === "string" ? error.message : typeof envelope.error === "string" ? envelope.error : "团队服务未接受本次请求。";
  throw new AiOfficeTeamError(code, message);
}

export const aiOfficeApi: AiOfficeApi = {
  team: teamRequest,
  snapshot: (projectId) => invoke("get_ai_office_snapshot", { request: { projectId: projectId ?? null } }),
  savePolicy: (projectId, expectedRevision, policy) => invoke("save_ai_office_policy", { request: { projectId, expectedRevision, policy } }),
  discoverCli: () => invoke("discover_ai_office_clis"),
  refreshCli: (providerId) => invoke("refresh_ai_office_cli", { providerId }),
  evaluatePlan: (projectId, expectedRevision, plan) => invoke("evaluate_ai_office_plan", { request: { projectId, expectedRevision, plan } }),
};

async function previewRequest<T>(request: object): Promise<T> {
  const response = await fetch("/__ai-office", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(request),
  });
  const result = await response.json();
  if (!response.ok || !result.success) throw new Error(result.error?.message ?? result.error ?? "演示后端暂时不可用，请确认预览服务已启动。");
  return result.output as T;
}

const DEMO_PROJECTS = [
  { id: "demo-webcodex", name: "WebCodex 桌面端", path: "演示项目 / webcodex-desktop", active_jobs: 3 },
  { id: "demo-docs", name: "产品设计与文档", path: "演示项目 / product-docs", active_jobs: 1 },
  { id: "demo-idle", name: "下一个好点子", path: "演示项目 / next-idea", active_jobs: 0 },
];

function membersFor(projectId: string): OfficeMember[] {
  const base = { active_requests: 0, last_activity_at_ms: Date.now(), parent_id: null, progress: null, task: null };
  if (projectId === "demo-idle") return [];
  if (projectId === "demo-docs") return [
    { ...base, id: "docs-manager", role: "manager", label: demoNickname("docs-manager"), state: "working", active_requests: 1, task: "把今天的成果整理成一本小手册", progress: 60 },
  ];
  return [
    { ...base, id: "owner", role: "boss", label: "你 · 大老板", state: "idle", task: "确定目标、查看进展、验收成果" },
    { ...base, id: "manager", role: "manager", label: demoNickname("manager"), state: "working", active_requests: 1, task: "把大目标拆成小任务，照看大家的进度", parent_id: "owner", progress: 68, model: "GPT · 由主任务选择" },
    { ...base, id: "frontend", role: "subagent", label: demoNickname("frontend"), state: "working", active_requests: 1, task: "布置大家的办公室，让小伙伴们动起来", parent_id: "manager", progress: 75, model: "GPT 子 Agent" },
    { ...base, id: "backend", role: "subagent", label: demoNickname("backend"), state: "working", active_requests: 1, task: "记好团队的小约定，整理每个人的任务卡", parent_id: "manager", progress: 55, model: "GPT 子 Agent" },
    { ...base, id: "review", role: "subagent", label: demoNickname("review"), state: "waiting", task: "等伙伴交作业，再一起检查有没有小遗漏", parent_id: "manager", model: "GPT 子 Agent" },
    { ...base, id: "cli-workbuddy", role: "cli", label: demoNickname("cli-workbuddy"), state: "working", active_requests: 1, task: "示例：帮伙伴检查今天的成果（未真实运行）", parent_id: "frontend", provider_id: "workbuddy", model: "示例模型", progress: 45 },
    { ...base, id: "cli-antigravity", role: "cli", label: demoNickname("cli-antigravity"), state: "idle", task: "示例：准备好了，等一份新任务（未真实运行）", parent_id: "backend", provider_id: "antigravity", model: "按发现结果选择" },
  ];
}

/** Synthetic office activity; policy persistence, validation and CLI discovery use Rust. */
export const aiOfficeDemoApi: AiOfficeApi = {
  async snapshot(projectId) {
    const policies = await previewRequest<{ id: string; policy: OfficePolicy }[]>({ op: "policies" });
    const selected = DEMO_PROJECTS.find((p) => p.id === projectId)?.id ?? DEMO_PROJECTS[0].id;
    const snapshot: OfficeSnapshot = {
      source: "demo", observed_at_ms: Date.now(), selected_project_id: selected,
      projects: DEMO_PROJECTS.map((project) => ({ ...project, connected: true, policy: policies.find((p) => p.id === project.id)!.policy })),
      members: membersFor(selected), enforcement: "demo_plan_only",
      limitations: ["办公室人物和进度为演示数据。策略保存、配额验证与 CLI 模型刷新使用本机服务。", "当前验证委派计划，不启动 AI 或 CLI 任务；配置尚未约束其它对话或现有调度器。"],
    };
    return snapshot;
  },
  savePolicy: (projectId, expectedRevision, policy) => previewRequest({ op: "save_policy", args: { projectId, expectedRevision, policy } }),
  discoverCli: () => previewRequest({ op: "discover_cli" }),
  refreshCli: (providerId) => previewRequest({ op: "refresh_cli", providerId }),
  evaluatePlan: (projectId, expectedRevision, plan) => previewRequest({ op: "evaluate_plan", args: { projectId, expectedRevision, plan } }),
};
