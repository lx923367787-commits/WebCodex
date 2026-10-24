export type CliProviderId = "workbuddy" | "antigravity";
export type TeamAction = "policy-get" | "policy-save" | "plan-list" | "plan-read" | "task-create";
export type OfficeEnforcement = "demo_plan_only" | "managed_tasks";
export interface CountRange { min: number; max: number }
export interface OfficeProvider {
  provider_id: CliProviderId;
  enabled: boolean;
  model: string | null;
  effort: string | null;
}
export interface OfficePolicyInput {
  mode: "auto" | "custom";
  subagents: CountRange;
  cli_per_agent: CountRange;
  providers: OfficeProvider[];
}
export interface OfficePolicy extends OfficePolicyInput { revision: number }
export interface OfficeProject {
  id: string;
  name: string;
  path: string;
  connected: boolean;
  active_jobs: number;
  policy: OfficePolicy;
}
export interface OfficeMember {
  id: string;
  role: "boss" | "manager" | "subagent" | "cli" | "observed_agent";
  label: string;
  state: "working" | "idle" | "waiting" | "done" | "error";
  active_requests: number;
  last_activity_at_ms: number | null;
  task: string | null;
  parent_id: string | null;
  progress: number | null;
  model?: string;
  provider_id?: CliProviderId;
}
export interface OfficeSnapshot {
  source: "runtime" | "offline" | "demo";
  observed_at_ms: number;
  projects: OfficeProject[];
  selected_project_id: string | null;
  members: OfficeMember[];
  enforcement: OfficeEnforcement;
  limitations: string[];
}
export interface CliCatalog {
  providerId: CliProviderId;
  label: string;
  installed: boolean;
  version: string | null;
  models: string[];
  efforts: string[];
  source: "installed_help" | "live_catalog" | "unavailable";
  status: "ready" | "unavailable" | "error";
  message: string;
  checkedAt: string;
  accountAvailabilityVerified: boolean;
}
export interface OfficeCliCall { provider_id: CliProviderId; model: string; effort?: string | null; instruction?: string | null }
export interface OfficePlanAgent { id: string; task: string; cli_calls: OfficeCliCall[] }
export interface OfficePlan { manager: OfficePlanAgent; subagents: OfficePlanAgent[] }
export interface OfficePlanEvaluation {
  admitted: boolean;
  policy_revision: number;
  enforcement: OfficeEnforcement;
  subagent_count: number;
  cli_call_count: number;
  violations: { code: string; message: string; agent_id?: string | null }[];
}
export interface AiOfficeApi {
  team?(action: TeamAction, params: Record<string, unknown>): Promise<unknown>;
  snapshot(projectId?: string): Promise<OfficeSnapshot>;
  savePolicy(projectId: string, expectedRevision: number, policy: OfficePolicyInput): Promise<OfficePolicy>;
  refreshCli(providerId: CliProviderId): Promise<CliCatalog>;
  evaluatePlan(projectId: string, expectedRevision: number, plan: OfficePlan): Promise<OfficePlanEvaluation>;
}
