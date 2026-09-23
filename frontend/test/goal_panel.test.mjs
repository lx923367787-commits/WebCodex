import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { goalStartParameters, goalActions, GoalPanel, GoalPanelState, newestGoal, goalDeliveryText, goalPageMetadata,
  isHostGoal, hostGoalGuidance, goalContextView, goalContinuationInstruction, readHostGoalSection } from "../dist/goal_panel.js";

const form = (changes = {}) => ({ project: "agent:r:project", provider: "worker", reviewer: "", objective: "Produce a report",
  acceptance: "Includes the requested conclusions", artifacts: "output/report.md", criteriaJson: "[]", maxRounds: "5", minutes: "30", ...changes });

test("goal form builds explicit review and deterministic artifact criteria", () => {
  const params = goalStartParameters(form({ artifacts: "output/report.md\noutput/report.md" }));
  assert.equal(params.reviewer_provider_id, "worker");
  assert.deepEqual(params.artifact_paths, ["output/report.md"]);
  assert.equal(params.criteria.length, 2);
  assert.equal(params.criteria[0].kind, "file_exists");
  assert.equal(params.criteria[1].kind, "review");
  assert.deepEqual(params.limits, { max_rounds: 5, total_timeout_secs: 1800, max_no_progress_rounds: 3, run_timeout_secs: 300 });
  const noReview = goalStartParameters(form({ acceptance: "" }));
  assert.equal(noReview.reviewer_provider_id, undefined);
});

test("goal form rejects missing artifacts, escaping paths, invalid checks and unbounded limits", () => {
  for (const artifacts of ["", "../outside.md", "E:\\outside.md", "/outside.md", "out/../outside.md"]) {
    assert.throws(() => goalStartParameters(form({ artifacts })), /project-relative artifact/);
  }
  assert.throws(() => goalStartParameters(form({ criteriaJson: "{}" })), /array/);
  assert.throws(() => goalStartParameters(form({ criteriaJson: '[{"kind":"review","id":"artifact_1","description":"duplicate"}]' })), /unique id/);
  assert.throws(() => goalStartParameters(form({ criteriaJson: '[{"kind":"command","id":"test","description":"test","program":"cargo","args":"test","timeout_secs":30}]' })), /fields/);
  for (const maxRounds of ["0", "-1", "1.5", "51", "Infinity"]) assert.throws(() => goalStartParameters(form({ maxRounds })));
  assert.throws(() => goalStartParameters(form({ minutes: "1441" })), /1–1440/);
});

test("goal actions never treat model termination as a completion control", () => {
  assert.deepEqual(goalActions("running"), { stop: true, resume: false });
  assert.deepEqual(goalActions("blocked"), { stop: true, resume: true });
  assert.deepEqual(goalActions("completed"), { stop: false, resume: false });
  assert.deepEqual(goalActions("unrecognized"), { stop: false, resume: false });
});

test("pending cancellation permits another stop check and never resumes execution", () => {
  assert.deepEqual(goalActions("paused", true), { stop: true, resume: false });
  assert.deepEqual(goalActions("blocked", true), { stop: true, resume: false });
  assert.deepEqual(goalActions("cancelled", true), { stop: false, resume: false });
});

test("uncertain start retry preserves its identity and cannot silently change its goal", () => {
  const state = new GoalPanelState(); state.reset("credential-1:project-a");
  let keys = 0;
  const first = state.start(goalStartParameters(form()), () => "key-" + (++keys));
  assert.equal(state.start(goalStartParameters(form()), () => "key-" + (++keys)), first);
  assert.equal(keys, 1);
  assert.throws(() => state.start(goalStartParameters(form({ objective: "Different goal" })), () => "new"), /unknown/);
});

test("credential or project changes discard late responses and prior create state", () => {
  const state = new GoalPanelState(); state.reset("credential-1:project-a");
  const generation = state.generation;
  state.start({ objective: "Private" }, () => "key");
  state.selected = "private-goal"; state.detail = { objective: "Private" };
  state.reset("credential-2:project-b");
  assert.equal(state.current(generation, "credential-1:project-a"), false);
  assert.equal(state.pendingStart, null);
  assert.equal(state.detail, null);
  assert.equal(state.selected, "");
});

test("late observations cannot overwrite a newer cancellation or round revision", () => {
  const current = { goal_id: "g", revision: 9, state: "cancelled" };
  assert.equal(newestGoal(current, { goal_id: "g", revision: 8, state: "running" }), current);
  assert.equal(newestGoal(current, { goal_id: "g", revision: 10, state: "queued" }).state, "queued");
});

test("delivery readiness requires completed state, current artifact hashes and explicit ready", () => {
  assert.match(goalDeliveryText("completed", "current", true), /^Delivery ready/);
  assert.doesNotMatch(goalDeliveryText("running", "current", true), /^Delivery ready/);
  assert.doesNotMatch(goalDeliveryText("completed", "changed", true), /^Delivery ready/);
  assert.doesNotMatch(goalDeliveryText("completed", "unavailable", true), /^Delivery ready/);
  assert.doesNotMatch(goalDeliveryText("completed", "current", false), /^Delivery ready/);
});

test("round paging uses server totals and offsets rather than the bounded page length", () => {
  assert.deepEqual(goalPageMetadata({ goal: { rounds: [{ number: 4 }] }, total_rounds: 12, round_offset: 3 }), { total: 12, offset: 3 });
  assert.deepEqual(goalPageMetadata({ goal: { rounds: [{ number: 12 }] } }), { total: 12, offset: 11 });
  assert.deepEqual(goalPageMetadata({ goal: { rounds: [] } }), { total: 0, offset: 0 });
  const state = new GoalPanelState(); state.roundOffset = 3; state.totalRounds = 12;
  state.reset("new-selection"); assert.equal(state.roundOffset, null); assert.equal(state.totalRounds, 0);
});

test("goal entry point uses typed kernel transport and text-only evidence rendering", async () => {
  const [source, html, runtime] = await Promise.all(["../src/goal_panel.ts", "../src/runtime.html", "../src/runtime.ts"].map((path) => readFile(new URL(path, import.meta.url), "utf8")));
  assert.doesNotMatch(source, /innerHTML/);
  assert.match(source, /report\.textContent = JSON\.stringify/);
  assert.match(html, /data-operations-target="runtime-operations-goals"/);
  assert.match(runtime, /api\("tools\/call", \{ tool, params \}, undefined, "\/api\/"\)/);
  assert.doesNotMatch(html, /id="goal-complete"/);
});

test("host guidance requires exact saved plan confirmation and separates awaiting from ready", () => {
  const goal = { goal_id: "h", phase: "awaiting_confirmation", plan_version: 3, plan_hash: "hash-3", plan_materialized: true };
  assert.equal(isHostGoal(goal), true);
  assert.equal(isHostGoal({ state: "queued", spec: {} }), false);
  assert.match(hostGoalGuidance(goal), /explicitly tell ChatGPT to start/);
  goal.phase = "ready";
  assert.match(hostGoalGuidance(goal), /does not match/);
  goal.confirmation = { plan_version: 2, plan_hash: "hash-3" };
  assert.match(hostGoalGuidance(goal), /does not match/);
  goal.confirmation = { plan_version: 3, plan_hash: "hash-2" };
  assert.match(hostGoalGuidance(goal), /does not match/);
  goal.confirmation.plan_hash = "hash-3";
  assert.match(hostGoalGuidance(goal), /has user confirmation/);
  goal.plan_materialized = false;
  assert.match(hostGoalGuidance(goal), /does not match/);
});

test("host context estimates obey both thresholds and never invent missing usage", () => {
  for (const [count, level] of [[0, "normal"], [23999, "normal"], [24000, "warning"], [31999, "warning"], [32000, "handoff"]]) {
    const view = goalContextView({ observed_tokens: count });
    assert.equal(view.level, level); assert.equal(view.count, count);
  }
  assert.equal(goalContextView({ observed_tokens: 5000, warning_tokens: 4000, handoff_tokens: 5000 }).level, "handoff");
  for (const meter of [null, {}, { observed_tokens: -1 }, { observed_tokens: "40000" }, { observed_tokens: Infinity }]) {
    assert.equal(goalContextView(meter).count, null);
    assert.equal(goalContextView(meter).level, "unavailable");
  }
  assert.match(goalContextView({ observed_tokens: 32000 }).advice, /Save a checkpoint, then open/);
  assert.doesNotMatch(goalContextView({ observed_tokens: 32000 }).advice, /saved|capacity|remaining/);
});

test("continuation identifies the exact goal and preserves confirmation phase", () => {
  const instruction = goalContinuationInstruction({ goal_id: "host-goal-42" });
  assert.match(instruction, /host-goal-42/);
  assert.match(instruction, /goal_bootstrap/);
  assert.match(instruction, /等待确认/);
  const state = new GoalPanelState(); state.hostDetail = { plan: { hash: "private" } };
  state.reset("new-credential"); assert.equal(state.hostDetail, null);
});

test("host renderer shows local state and evidence without exposing ACP execution controls", async () => {
  class Element {
    children = []; listeners = {}; hidden = false; attributes = {};
    textContent = ""; value = "";
    append(...children) { this.children.push(...children); }
    appendChild(child) { this.append(child); }
    replaceChildren(...children) { this.children = children; }
    addEventListener(event, handler) { this.listeners[event] = handler; }
    setAttribute(key, value) { this.attributes[key] = value; }
    get childElementCount() { return this.children.length; }
  }
  const nodes = new Map(), root = { querySelector: (id) => {
    if (!nodes.has(id)) nodes.set(id, new Element());
    return nodes.get(id);
  } };
  const priorDocument = globalThis.document;
  globalThis.document = { createElement: () => new Element() };
  try {
    const calls = [];
    const panel = new GoalPanel(root, { context: () => ({ key: "k", project: "p", connected: true }), translate: (s) => s,
      operationKey: () => "op", unauthorized: () => {}, call: async (tool) => { calls.push(tool); } });
    panel.sync();
    panel.state.detail = { goal_id: "h", objective: "Saved task", phase: "awaiting_confirmation", revision: 4,
      state: { completed_summary: "Done research", issues: ["Pending access"], next_step: "Confirm saved plan" }, completed_node_ids: [] };
    panel.state.hostDetail = { goal: panel.state.detail, plan_artifacts_valid: true, context_meter: { observed_tokens: 32000 },
      plan: { version: 1, hash: "planhash", materialization: { json_path: "C:/data/goals/h/plan.json", json_hash: "jsonhash",
        markdown_path: "C:/data/goals/h/plan.md", markdown_hash: "mdhash" }, plan: { nodes: [{ id: "n1", title: "Implement", work: "Implement required change" }] } }, checkpoints: [] };
    panel.render();
    assert.equal(nodes.get("#goal-host-detail").hidden, false);
    assert.equal(nodes.get("#goal-acp-detail").hidden, true);
    assert.match(nodes.get("#goal-host-guidance").textContent, /explicitly tell ChatGPT/);
    assert.match(nodes.get("#goal-host-plan").textContent, /C:\/data\/goals\/h\/plan.md/);
    assert.equal(nodes.get("#goal-context-warning").attributes["data-level"], "handoff");
    assert.equal(nodes.get("#goal-host-nodes").childElementCount, 1);
    nodes.get("#goal-resume").listeners.click();
    assert.deepEqual(calls, []);
    panel.state.hostDetail.plan_artifacts_valid = false; panel.render();
    assert.match(nodes.get("#goal-host-guidance").textContent, /failed verification/);
    panel.state.detail.phase = "completed";
    panel.state.hostDetail.delivery_freshness = "changed";
    panel.state.hostDetail.delivery_ready = true;
    panel.render();
    assert.equal(nodes.get("#goal-host-delivery-status").hidden, false);
    assert.match(nodes.get("#goal-host-delivery-status").textContent, /Artifacts changed since acceptance/);
    panel.state.hostDetail.delivery_freshness = "current";
    panel.state.hostDetail.delivery_ready = false; panel.render();
    assert.doesNotMatch(nodes.get("#goal-host-delivery-status").textContent, /^Delivery ready/);
    delete panel.state.hostDetail.delivery_freshness; delete panel.state.hostDetail.delivery_ready; panel.render();
    assert.match(nodes.get("#goal-host-delivery-status").textContent, /Refresh before delivery/);
    panel.state.hostDetail.delivery_freshness = "current";
    panel.state.hostDetail.delivery_ready = true; panel.render();
    assert.match(nodes.get("#goal-host-delivery-status").textContent, /^Delivery ready/);
  } finally { globalThis.document = priorDocument; }
});

test("host UI explains chat confirmation and offers no approval substitute", async () => {
  const html = await readFile(new URL("../src/runtime.html", import.meta.url), "utf8");
  const host = html.split('<section id="goal-host-detail"')[1].split('<section id="goal-acp-detail"')[0];
  assert.match(host, /goal-copy-continuation/);
  assert.doesNotMatch(host, /goal-(start|resume|confirm)/);
  assert.match(html, /then tell ChatGPT to begin/);
});

test("console reconstructs bounded detail pages without mixing revisions or targets", async () => {
  const goal = { goal_id: "h", revision: 9, plan_version: 2 };
  const state = { facts: { source: "事实来源".repeat(1000) }, next_step: "Verify" };
  const text = JSON.stringify(state), calls = [];
  const read = async (params) => {
    calls.push(params);
    const second = params.cursor === "next";
    return { projection: "detail", section: "state", encoding: "json_text", goal,
      text: second ? text.slice(2000) : text.slice(0, 2000), has_more: !second, next_cursor: second ? null : "next" };
  };
  assert.deepEqual(await readHostGoalSection(read, goal, "state"), state);
  assert.equal(calls.length, 2);
  assert.equal(calls[1].cursor, "next");
  for (const changed of [{ ...goal, goal_id: "other" }, { ...goal, revision: 10 }, { ...goal, plan_version: 3 }]) {
    await assert.rejects(readHostGoalSection(async () => ({ projection: "detail", section: "state", encoding: "json_text", goal: changed, text: "{}", has_more: false }), goal, "state"), /consistent revision/);
  }
  await assert.rejects(readHostGoalSection(async () => ({ projection: "detail", section: "state", encoding: "json_text", goal, text: "", has_more: true, next_cursor: "loop" }), goal, "state"), /invalid continuation/);
});
