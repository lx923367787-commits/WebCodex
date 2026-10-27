import assert from "node:assert/strict";

const port = Number(process.env.AI_OFFICE_PREVIEW_PORT ?? 4179);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error("Invalid AI_OFFICE_PREVIEW_PORT");
const endpoint = `http://127.0.0.1:${port}/__ai-office`;
async function request(value, headers = {}) {
  const response = await fetch(endpoint, { method: "POST", headers: { "Content-Type": "application/json", ...headers }, body: JSON.stringify(value) });
  return { status: response.status, ...(await response.json()) };
}
const initial = await request({ op: "policies" });
assert.equal(initial.success, true);
assert.equal(initial.output.length, 3);
const original = initial.output.find((project) => project.id === "demo-idle").policy;
const { revision, ...originalSettings } = original;
const custom = { ...originalSettings, mode: "custom", subagents: { min: 1, max: 2 }, cli_per_agent: { min: 1, max: 2 }, providers: [{ provider_id: "workbuddy", enabled: true, model: "hy3", effort: "high" }] };
const save = { op: "save_policy", args: { projectId: "demo-idle", expectedRevision: revision, policy: custom } };
const race = await Promise.all([request(save), request(save)]);
assert.equal(race.filter((result) => result.success).length, 1, "CAS admits one concurrent writer");
assert.equal(race.find((result) => !result.success).error.code, "ai_office_revision_conflict");
const currentRevision = race.find((result) => result.success).output.revision;
try {
  const reloaded = await request({ op: "policies" });
  assert.deepEqual(reloaded.output.find((project) => project.id === "demo-idle").policy, { ...custom, revision: currentRevision });
  const agent = (id) => ({ id, task: "演示后端契约验证", cli_calls: [{ provider_id: "workbuddy", model: "hy3", effort: "high" }] });
  const evaluate = (subagents) => request({ op: "evaluate_plan", args: { projectId: "demo-idle", expectedRevision: currentRevision, plan: { manager: agent("manager"), subagents } } });
  assert.equal((await evaluate([agent("child-1")])).output.admitted, true);
  assert.equal((await evaluate([])).output.admitted, false, "minimum is enforced");
  assert.equal((await evaluate([agent("1"), agent("2"), agent("3")])).output.admitted, false, "maximum is enforced");
  const stale = await request(save);
  assert.equal(stale.error.code, "ai_office_revision_conflict");
  const foreign = await request({ op: "policies" }, { Origin: "https://example.com" });
  assert.equal(foreign.status, 403);
  const arbitrary = await request({ op: "run_process", args: { command: "echo never-run" } });
  assert.equal(arbitrary.status, 400);
  const catalog = await request({ op: "refresh_cli", providerId: "workbuddy" });
  assert.equal(catalog.success, true);
  assert.equal(catalog.output.status, "ready");
  assert.ok(catalog.output.models.length > 0);
  assert.equal(catalog.output.accountAvailabilityVerified, false);
  console.log(JSON.stringify({ persistence: "passed", concurrentCAS: "passed", minMaxPlan: "passed", originAndOperationBounds: "passed", workbuddy: { version: catalog.output.version, modelCount: catalog.output.models.length, efforts: catalog.output.efforts } }));
} finally {
  const restored = await request({ op: "save_policy", args: { projectId: "demo-idle", expectedRevision: currentRevision, policy: originalSettings } });
  assert.equal(restored.success, true, "restore demo policy after smoke");
}
