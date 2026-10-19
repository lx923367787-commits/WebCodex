import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { runtimeDiagnostics, runtimeOverviewFreshUntil, runtimeProjectProjection, fetchRuntimeResponse, RUNTIME_OVERVIEW_STALE_MS } from "../dist/runtime_console_state.js";

function snapshot() {
  const observed = (status) => ({ status, age_secs: 0, stale_after_secs: 30, source: "registry" });
  return {
    connection_layers: {
      tunnel: { status: "not_observed", reason_code: "tunnel_health_not_reported" },
      runner_process: observed("ready"), server_transport: observed("connected"), project_registry: observed("registered"),
    },
    jobs: { recovering_count: 0, lost_after_reconcile_count: 0 },
    version_compatibility: { status: "compatible", source_alignment: { status: "aligned" } },
  };
}
const statusOf = (rows, name) => rows.find((row) => row.name === name).status;

test("runtime diagnostics distinguish current, unknown, unconfigured, offline and degraded layers", () => {
  const current = snapshot();
  let rows = runtimeDiagnostics(current, 1000, 1000);
  assert.equal(statusOf(rows, "Server"), "online");
  assert.equal(statusOf(rows, "Tunnel"), "unknown", "local Server does not prove tunnel health");
  assert.equal(statusOf(rows, "Runner"), "online");
  assert.equal(statusOf(rows, "Build consistency"), "online");
  current.connection_layers.project_registry.status = "not_configured";
  current.connection_layers.server_transport.status = "disconnected";
  current.jobs.recovering_count = 1;
  current.version_compatibility.source_alignment.status = "different";
  current.runners_stale = 1;
  rows = runtimeDiagnostics(current, 1000, 1000);
  assert.equal(statusOf(rows, "Project"), "unconfigured");
  assert.equal(statusOf(rows, "Runner transport"), "offline");
  assert.equal(statusOf(rows, "Runner"), "degraded");
  assert.equal(statusOf(rows, "Job"), "degraded");
  assert.equal(statusOf(rows, "Build consistency"), "degraded");
});

test("old observations and failed refreshes cannot remain healthy; a new observation recovers", () => {
  const current = snapshot();
  assert.equal(statusOf(runtimeDiagnostics(current, 1000, 31000), "Runner"), "degraded");
  const expired = runtimeDiagnostics(current, 1000, 1000 + RUNTIME_OVERVIEW_STALE_MS);
  assert.ok(expired.every((row) => row.status === "unknown"));
  const failure = runtimeDiagnostics(current, 1000, 1001, "transport_unreachable");
  assert.equal(statusOf(failure, "Server"), "offline");
  assert.equal(statusOf(failure, "Job"), "unknown");
  const timeout = runtimeDiagnostics(current, 1000, 1001, "request_timeout");
  assert.equal(statusOf(timeout, "Server"), "unknown");
  assert.equal(statusOf(runtimeDiagnostics(current, 5000, 5001), "Server"), "online");
  assert.ok(runtimeDiagnostics(null, 0, 1).every((row) => row.status === "unknown"));
  current.jobs = { recovering_count: null, lost_after_reconcile_count: null };
  assert.equal(statusOf(runtimeDiagnostics(current, 1000, 1001), "Job"), "unknown");
});

test("layer TTL never rewrites known disconnection, configuration, or unknown reasons", () => {
  const current = snapshot();
  for (const [raw, expected] of [["disconnected", "offline"], ["not_configured", "unconfigured"], ["unknown", "unknown"], ["stale", "degraded"]]) {
    current.connection_layers.server_transport = { status: raw, reason_code: "original_reason", age_secs: 100, stale_after_secs: 30 };
    const layer = runtimeDiagnostics(current, 1000, 1001).find((row) => row.name === "Runner transport");
    assert.equal(layer.status, expected);
    assert.equal(layer.reason, "original_reason");
  }
});

test("slow response uses request-start age and expires aggregates before old layer claims return", () => {
  const current = snapshot();
  current.connection_layers.runner_process.age_secs = 29;
  const startedAt = 1000;
  const receivedAt = startedAt + 20000;
  assert.equal(runtimeOverviewFreshUntil(current, startedAt), startedAt + 1000);
  assert.ok(receivedAt >= runtimeOverviewFreshUntil(current, startedAt));
  assert.equal(statusOf(runtimeDiagnostics(current, startedAt, receivedAt), "Runner"), "degraded");
});

test("Project connection and Session facts retain their own observation source and freshness", () => {
  const oldProject = { id: "p", connected: true, agent_status: "online" };
  const overviewProject = { id: "p", connected: false, agent_status: "offline", sessions: { running_sessions: 1 } };
  const updated = runtimeProjectProjection(oldProject, overviewProject, 1000, 2000, true, true);
  assert.equal(updated.connected, false);
  assert.equal(updated.agent_status, "offline");
  assert.equal(updated.connection_fresh, true);
  const unrelatedRefresh = runtimeProjectProjection(oldProject, null, 1000, 90000, false, true);
  assert.equal(unrelatedRefresh.connection_fresh, false, "unrelated overview cannot renew a retained Project");
  const projectScoped = runtimeProjectProjection(oldProject, null, 5000, 0, true, false);
  assert.equal(projectScoped.connection_fresh, true, "project:read remains usable when overview is forbidden");
  assert.equal(projectScoped.connected, true);
  assert.equal(projectScoped.sessions_fresh, false);
  const latestProjects = runtimeProjectProjection(oldProject, overviewProject, 5000, 2000, true, false);
  assert.equal(latestProjects.connected, true, "new Projects response wins over old overview");
  assert.equal(latestProjects.sessions_fresh, false, "new Projects response cannot renew Session facts");
  const failedProjects = runtimeProjectProjection(oldProject, overviewProject, 5000, 2000, false, true);
  assert.equal(failedProjects.connection_fresh, false, "old overview cannot restore latest failed Project observation");
});

test("effective Project rendering uses matching fresh overview facts and works without runtime:read", async () => {
  const source = await readFile(new URL("../dist/runtime.js", import.meta.url), "utf8");
  const start = source.indexOf("function effectiveProjects(");
  const end = source.indexOf("\n}", start) + 2;
  let now = 3000;
  const context = vm.createContext({
    homeProjectRows: [{ id: "p", connected: false, agent_status: "offline" }],
    state: { projectsObservedAt: 1000, projectsObservationFailed: false },
    runtimeOverviewReceivedAt: 2000, runtimeOverviewIsFresh: () => true,
    runtimeProjectProjection, RUNTIME_OVERVIEW_STALE_MS, Date: { now: () => now },
  });
  vm.runInContext(source.slice(start, end), context);
  const projects = [{ id: "p", connected: true, agent_status: "online" }];
  assert.equal(context.effectiveProjects(projects)[0].connected, false);
  context.homeProjectRows = [];
  context.runtimeOverviewReceivedAt = 0;
  context.runtimeOverviewIsFresh = () => false;
  context.state.projectsObservedAt = 2500;
  assert.equal(context.effectiveProjects(projects)[0].connection_fresh, true);
  context.state.projectsObservationFailed = true;
  assert.equal(context.effectiveProjects(projects)[0].connection_fresh, false);
  context.state.projectsObservationFailed = false;
  now = 2500 + RUNTIME_OVERVIEW_STALE_MS;
  assert.equal(context.effectiveProjects(projects)[0].connection_fresh, false);
});

test("expired summaries cannot revive through rerender and verified refresh restores them", async () => {
  const source = await readFile(new URL("../dist/runtime.js", import.meta.url), "utf8");
  const start = source.indexOf("function renderRuntimeOverviewMetrics(");
  const end = source.indexOf("\n}", start) + 2;
  let fresh = true;
  const fields = new Map();
  const context = vm.createContext({
    runtimeOverviewIsFresh: () => fresh,
    setText: (id, value) => fields.set(id, value),
    tr: (value) => value,
    countLabel: (count, label) => `${count || 0} ${label}`,
    attentionLabel: () => "attention",
    runtimeLanguage: "en",
  });
  vm.runInContext(source.slice(start, end), context);
  const current = { runner_count: 2, runners_online: 2, active_jobs: 1, workflow_sessions: { running: 1 }, recent_sessions: {} };
  context.renderRuntimeOverviewMetrics(current);
  assert.match(fields.get("runtime-server-alignment"), /2 online/);
  assert.match(fields.get("runtime-server-jobs"), /1 active Job/);
  fresh = false;
  context.renderRuntimeOverviewMetrics(current);
  for (const id of ["runtime-server-alignment", "runtime-server-jobs", "runtime-server-sessions"]) {
    assert.match(fields.get(id), /unknown.*Last observed/);
  }
  context.renderRuntimeOverviewMetrics(current);
  assert.doesNotMatch(fields.get("runtime-server-alignment"), /online/);
  fresh = true;
  context.renderRuntimeOverviewMetrics(current);
  assert.match(fields.get("runtime-server-alignment"), /2 online/);
});

test("diagnostic refresh propagates stale and recovered observations to every summary surface", async () => {
  const source = await readFile(new URL("../dist/runtime.js", import.meta.url), "utf8");
  const start = source.indexOf("function renderRuntimeDiagnostics(");
  const end = source.indexOf("\n}", start) + 2;
  const rendered = [];
  const context = vm.createContext({
    el: () => ({}), clearNode() {}, runtimeDiagnostics: () => [], setText() {}, tr: (value) => value,
    runtimeOverviewSnapshot: {}, runtimeOverviewReceivedAt: 1000, runtimeOverviewFailure: null,
    runtimeOverviewIsFresh: () => false, runtimeLanguage: "en",
    runnerRows: [], recentSessionRows: [], recentSessionMetaSnapshot: {}, projectRows: [], projectRowsTruncated: false,
    renderRunnerFleet: () => rendered.push("runner"), renderRuntimeOverviewMetrics: () => rendered.push("summary"),
    renderRecentSessions: () => rendered.push("recent"), renderProjectSelectors: () => rendered.push("project"),
  });
  vm.runInContext(source.slice(start, end), context);
  context.renderRuntimeDiagnostics();
  assert.deepEqual(rendered, ["runner", "summary", "recent", "project"]);
});

test("business failure is returned once and cannot cause a reconnect or request replay", async () => {
  let calls = 0;
  const response = await fetchRuntimeResponse("/operation", { method: "POST" }, 100, async () => {
    calls++;
    return { ok: false, status: 422, json: async () => ({ error: "operation_failed" }) };
  });
  assert.equal(calls, 1);
  assert.equal(response.status, 422);
  assert.equal(response.data.error, "operation_failed");
});

test("transport timeout, network loss, and caller cancellation never replay mutations", async () => {
  let calls = 0;
  const pending = (_url, init) => {
    calls++;
    return new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true }));
  };
  const timedOut = await fetchRuntimeResponse("/operation", { method: "POST" }, 5, pending);
  assert.equal(timedOut.reason, "request_timeout");
  assert.equal(calls, 1);
  const lost = await fetchRuntimeResponse("/operation", {}, 100, async () => { throw new Error("network"); });
  assert.equal(lost.reason, "transport_unreachable");
  const controller = new AbortController();
  const cancelled = fetchRuntimeResponse("/operation", { signal: controller.signal }, 100, pending);
  controller.abort();
  assert.equal(await cancelled, null);
  assert.equal(calls, 2);
  assert.equal(await fetchRuntimeResponse("/operation", { signal: controller.signal }, 100, pending), null);
  assert.equal(calls, 2, "pre-cancelled request must not start");
});

test("timeout while reading response body remains unknown instead of successful", async () => {
  const response = await fetchRuntimeResponse("/operation", {}, 5, async (_url, init) => ({
    ok: true, status: 200,
    json: () => new Promise((_resolve, reject) => init.signal.addEventListener("abort", () => reject(new Error("body lost")), { once: true })),
  }));
  assert.equal(response.ok, false);
  assert.equal(response.reason, "request_timeout");
});
