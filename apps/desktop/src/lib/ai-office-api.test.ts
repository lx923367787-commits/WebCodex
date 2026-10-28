import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { aiOfficeApi, aiOfficeDemoApi, AiOfficeTeamError } from "./ai-office-api";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);
beforeEach(() => { native.mockReset(); });

describe("AI office native team adapter", () => {
  it("uses the current project's Runner inventory for both discovery and models", async () => {
    const installation = { providerId: "codex", installed: true, adapterAvailable: true, executionReady: true };
    native.mockResolvedValueOnce({ success: true, output: { installations: [installation] } });
    await expect(aiOfficeApi.discoverCli("agent:runner:project")).resolves.toEqual([installation]);
    expect(native).toHaveBeenLastCalledWith("ai_office_team_call", { action: "cli-discover", params: { project: "agent:runner:project" } });
    native.mockResolvedValueOnce({ success: true, output: { observed_at_ms: 1000, capabilities: { providerId: "codex", models: [], efforts: [], status: "ready" } } });
    await expect(aiOfficeApi.refreshCli("codex", "agent:runner:project")).resolves.toMatchObject({ providerId: "codex", checkedAt: "1970-01-01T00:00:01.000Z", models: [] });
    expect(native).toHaveBeenLastCalledWith("ai_office_team_call", { action: "cli-models", params: { project: "agent:runner:project", provider_id: "codex" } });
    await expect(aiOfficeApi.discoverCli()).rejects.toMatchObject({ code: "team_project_required" });
    expect(native).toHaveBeenCalledTimes(2);
  });

  it("passes the exact project identity and unwraps the full ToolResult output", async () => {
    const output = { plans: [], total_count: 0, next_offset: null };
    native.mockResolvedValue({ success: true, output, error: null });
    await expect(aiOfficeApi.team!("plan-list", { project: "runtime/runner/project", offset: 0, limit: 20 })).resolves.toEqual(output);
    expect(native).toHaveBeenCalledWith("ai_office_team_call", { action: "plan-list", params: { project: "runtime/runner/project", offset: 0, limit: 20 } });
  });

  it("preserves structured business codes and does not retry rejected writes", async () => {
    native.mockResolvedValue({ success: false, output: { error_kind: "team_policy_conflict", state_changed: false }, error: "Policy revision changed" });
    await expect(aiOfficeApi.team!("policy-save", { project: "runtime/runner/project", expected_revision: 1, policy: { mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 }, providers: [] } })).rejects.toMatchObject({ name: "AiOfficeTeamError", code: "team_policy_conflict", message: "Policy revision changed" });
    expect(native).toHaveBeenCalledTimes(1);
  });

  it("preserves a direct business error code", async () => {
    native.mockResolvedValue({ success: false, output: null, error: "cli_fallback_host_unavailable" });
    await expect(aiOfficeApi.team!("plan-read", { project: "runtime/runner/project" })).rejects.toMatchObject({ code: "cli_fallback_host_unavailable" });
  });

  it("never retries an ambiguous native write result", async () => {
    native.mockRejectedValue(new Error("Native transport closed"));
    await expect(aiOfficeApi.team!("policy-save", { project: "runtime/runner/project", expected_revision: 1, policy: { mode: "auto", subagents: { min: 0, max: 4 }, cli_per_agent: { min: 0, max: 2 }, providers: [] } })).rejects.toMatchObject({ code: "team_transport_unavailable" });
    expect(native).toHaveBeenCalledTimes(1);
  });

  it("rejects malformed envelopes and missing project IDs", async () => {
    native.mockResolvedValue({ success: true });
    await expect(aiOfficeApi.team!("plan-list", { project: "runtime/runner/project" })).rejects.toMatchObject({ code: "team_invalid_response" });
    native.mockClear();
    await expect(aiOfficeApi.team!("plan-list", {})).rejects.toBeInstanceOf(AiOfficeTeamError);
    expect(native).not.toHaveBeenCalled();
  });

  it("does not expose live dispatch through the demonstration API", () => {
    expect(aiOfficeDemoApi.team).toBeUndefined();
  });
});
