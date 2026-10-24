export const GOAL_ZH_TEXT = {
    "Goals": "目标模式",
    "Execute, check and improve": "执行、检查并改进",
    "Goal workspace": "目标工作区",
    "The server runs each goal and checks its acceptance criteria after every round.": "服务器执行目标，并在每一轮结束后检查验收条件。",
    "Selected project": "当前项目",
    "Choose a project in Projects & Sessions first.": "请先在“项目与会话”中选择项目。",
    "Create a goal": "创建目标",
    "Execution provider ID": "执行 Provider ID",
    "Reviewer provider ID (optional)": "审稿 Provider ID（可选）",
    "Leave blank to use the execution provider for a separate review run.": "留空时使用执行 Provider 发起独立检查。",
    "Goal objective": "目标描述",
    "Acceptance criteria (one per line)": "验收要求（每行一项）",
    "Artifact paths (one per line)": "产物路径（每行一项）",
    "Artifact paths add file-existence checks. Acceptance text adds reviewer checks.": "产物路径会增加文件存在检查；验收要求会增加审稿检查。",
    "Maximum rounds": "最多轮数",
    "Total time limit (minutes)": "总时长上限（分钟）",
    "Advanced acceptance checks (JSON)": "高级验收检查（JSON）",
    "Additional criteria": "附加验收条件",
    "Start goal": "启动目标",
    "Retry same start": "重试同一次启动",
    "Refresh goals": "刷新目标",
    "Recent goals": "最近目标",
    "Select a goal to inspect its rounds and evidence.": "选择目标以查看轮次和检查证据。",
    "No goals for this project.": "此项目还没有目标。",
    "Checks and evidence": "检查与证据",
    "Stop goal": "停止目标",
    "Confirming execution has stopped; click Stop goal again to reconcile its status.": "正在确认执行已停止；可再次点击停止核对状态。",
    "Resume goal": "恢复目标",
    "Round": "轮次",
    "No checks recorded yet.": "尚未记录检查结果。",
    "No rounds recorded yet.": "尚未记录执行轮次。",
    "Goal request failed.": "目标请求失败。",
    "Goal request outcome is unknown. Retry the same request.": "目标请求结果尚不确定，请重试同一次请求。",
    "Goal started.": "目标已启动。",
    "Goal start recovered.": "已恢复此前启动的目标。",
    "Goal updated.": "目标已更新。",
    "Working…": "处理中…",
    "Loading goals…": "正在加载目标…",
    "Project, provider and objective are required.": "请填写项目、Provider ID 和目标描述。",
    "Add at least one acceptance check or artifact path.": "请至少添加一项验收条件或产物路径。",
    "Acceptance JSON must be an array of criteria.": "验收 JSON 必须是条件数组。",
    "Each criterion needs a unique id, description and supported kind.": "每项条件需要唯一 id、description 和受支持的 kind。",
    "Invalid acceptance criterion fields.": "验收条件字段无效。",
    "Add at least one project-relative artifact path.": "请至少填写一个项目内的相对产物路径。",
    "Use 1–50 rounds and 1–1440 minutes.": "请设置 1–50 轮和 1–1440 分钟。",
    "Use at most 32 acceptance checks and 64 artifacts.": "验收条件最多 32 项，产物最多 64 个。",
    "Delivery ready at latest refresh; accepted artifact hashes still match.": "最近一次刷新确认可交付，产物哈希仍与验收时一致。",
    "Artifacts changed since acceptance; the current files are not verified for delivery.": "产物在验收后已改变，当前文件尚未通过交付核验。",
    "Artifact freshness could not be verified. Refresh before delivery.": "未能核实产物是否变更，请在交付前刷新检查。",
    "Delivery verification is pending.": "尚未通过交付核验。",
    "Round details": "轮次详情",
    "Previous round": "上一轮", "Next round": "下一轮",
    "Displayed round": "当前显示轮次",
    "Execution": "执行", "Review": "审稿", "queued": "等待执行", "needs_work": "需要改进",
    "Goal limits must be positive whole numbers.": "目标上限必须是正整数。",
    "Reconnect to load goals.": "请连接运行时以加载目标。",
    "Goal access is unavailable to this credential.": "当前凭据无权访问目标。",
    "pending": "等待执行", "running": "执行中", "checking": "检查中",
    "paused": "已暂停", "blocked": "受阻", "completed": "已完成",
    "cancelled": "已停止", "failed": "失败", "passed": "通过",
    "needs_revision": "需要改进", "budget_exhausted": "已达上限",
    "outcome_unknown": "结果不确定", "waiting_permission": "等待权限",
    "Start in ChatGPT with “开启目标模式：<目标>”. Review the saved local plan, then tell ChatGPT to begin.": "在 ChatGPT 中输入“开启目标模式：<目标>”。查看本地保存的计划后，在对话中通知 ChatGPT 开始执行。",
    "Create a background ACP goal": "创建后台 ACP 目标",
    "ChatGPT goal": "ChatGPT 目标", "Background ACP goal": "后台 ACP 目标",
    "planning": "规划中", "awaiting_confirmation": "等待用户确认计划", "ready": "计划已确认，等待接手",
    "executing": "执行中", "verifying": "验收中", "interrupted": "已中断",
    "Local plan": "本地计划", "Saved task state": "已保存的任务状态",
    "Node steps and acceptance": "节点步骤与验收", "Latest checkpoint and evidence": "最近检查点与证据",
    "Copy continuation instruction": "复制续接指令", "Continuation instruction copied.": "续接指令已复制。",
    "Copy unavailable. Select and copy the continuation instruction below.": "无法自动复制，请选中下方续接指令手动复制。",
    "Visible tool text only; ordinary chat, other apps and images are not fully measured. This is not actual context capacity.": "仅估算可见工具文本；普通聊天、其他应用和图片未完整计量。这不是实际上下文容量。",
    "No saved context estimate for this goal.": "此目标尚无已保存的对话上下文估算。",
    "Save progress and prepare a handoff.": "请保存当前进度并整理交接信息。",
    "Save a checkpoint, then open a new ChatGPT conversation.": "请成功保存检查点后，新开 ChatGPT 对话继续。",
    "Visible text estimate is below the configured warning threshold.": "可见文本估算尚未达到配置的预警阈值。",
    "Latest ChatGPT conversation: estimated visible tokens": "最近活动 ChatGPT 对话的可见文本累计估算 tokens", "Warning / handoff": "预警 / 建议换对话",
    "Discuss the goal and save the full plan locally before execution.": "先讨论目标并把完整计划保存到本地，再等待用户确认。",
    "Review this saved plan, then explicitly tell ChatGPT to start in the conversation.": "请查看此版本的本地计划，在 ChatGPT 对话中明确确认后通知其开始执行。",
    "This plan version has user confirmation. Continue in ChatGPT to claim the next node.": "此计划版本已有用户确认，请在 ChatGPT 中接手下一节点。",
    "Confirmation does not match the current plan. Return to ChatGPT to verify the saved version.": "确认记录与当前计划不一致，请回到 ChatGPT 核对本地计划版本。",
    "Continue in ChatGPT using the saved state; reconcile unfinished operations first.": "在 ChatGPT 中读取已保存状态继续，先核对尚未结束的操作。",
    "Goal finished. Review the recorded acceptance evidence.": "目标已结束，请查看已记录的验收证据。",
    "Plan has not been saved locally yet.": "计划尚未完成本地保存。",
    "Completed summary": "已完成摘要", "Facts": "任务事实", "Decisions": "决策",
    "Issues": "未解决问题", "Artifacts": "产物引用", "Next step": "下一步", "Plan draft": "计划草稿",
    "No saved checkpoint yet.": "尚无已保存的检查点。", "No nodes planned yet.": "尚未保存节点计划。",
    "Plan version": "计划版本", "Plan hash": "计划哈希", "Revision": "状态版本",
    "Inputs": "输入", "Outputs": "输出", "Acceptance": "验收", "Failure action": "失败处理",
    "State management": "状态管理规则", "Final acceptance": "最终验收",
    "automated": "自动验证", "model_self_check": "模型自检", "Unfinished operations": "未完成操作",
    "Local plan files failed verification. Resolve this in ChatGPT before continuing.": "本地计划文件核验失败，请在 ChatGPT 中处理后再继续。",
    "Showing a partial summary. Refresh to load the complete saved details.": "当前仅展示部分摘要，请刷新以加载完整保存内容。",
    "Status omitted from summary": "摘要未包含此节点状态",
};
export function goalStartParameters(values) {
    if (!values.project || !values.provider.trim() || !values.objective.trim()) {
        throw new Error("Project, provider and objective are required.");
    }
    const lines = (text) => text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
    const artifacts = [...new Set(lines(values.artifacts))];
    if (!artifacts.length || artifacts.some((path) => /^(?:[A-Za-z]:|[\\/])/.test(path) || path.split(/[\\/]/).includes(".."))) {
        throw new Error("Add at least one project-relative artifact path.");
    }
    let additional;
    try {
        additional = JSON.parse(values.criteriaJson.trim() || "[]");
    }
    catch {
        throw new Error("Acceptance JSON must be an array of criteria.");
    }
    if (!Array.isArray(additional))
        throw new Error("Acceptance JSON must be an array of criteria.");
    const criteria = [
        ...artifacts.map((path, index) => ({ kind: "file_exists", id: "artifact_" + (index + 1), description: path, path })),
        ...lines(values.acceptance).map((description, index) => ({ kind: "review", id: "review_" + (index + 1), description })),
        ...additional,
    ];
    if (!criteria.length)
        throw new Error("Add at least one acceptance check or artifact path.");
    if (criteria.length > 32 || artifacts.length > 64)
        throw new Error("Use at most 32 acceptance checks and 64 artifacts.");
    const ids = new Set();
    for (const criterion of criteria) {
        if (!criterion || typeof criterion.id !== "string" || !criterion.id.trim() || ids.has(criterion.id)
            || typeof criterion.description !== "string" || !criterion.description.trim()
            || !["file_exists", "file_contains", "command", "review"].includes(criterion.kind)) {
            throw new Error("Each criterion needs a unique id, description and supported kind.");
        }
        ids.add(criterion.id);
        if ((["file_exists", "file_contains"].includes(criterion.kind) && (typeof criterion.path !== "string" || !criterion.path.trim()))
            || (criterion.kind === "file_contains" && (typeof criterion.text !== "string" || !criterion.text))
            || (criterion.kind === "command" && (typeof criterion.program !== "string" || !criterion.program.trim()
                || !Array.isArray(criterion.args) || !criterion.args.every((value) => typeof value === "string")
                || !Number.isInteger(criterion.timeout_secs) || criterion.timeout_secs <= 0))) {
            throw new Error("Invalid acceptance criterion fields.");
        }
    }
    const maxRounds = Number(values.maxRounds), minutes = Number(values.minutes);
    if (![maxRounds, minutes].every((value) => Number.isSafeInteger(value) && value > 0)) {
        throw new Error("Goal limits must be positive whole numbers.");
    }
    if (maxRounds > 50 || minutes > 1440)
        throw new Error("Use 1–50 rounds and 1–1440 minutes.");
    const reviewer = values.reviewer.trim() || (criteria.some((criterion) => criterion.kind === "review") ? values.provider.trim() : "");
    return {
        project: values.project, provider_id: values.provider.trim(), objective: values.objective.trim(),
        criteria, artifact_paths: artifacts,
        ...(reviewer ? { reviewer_provider_id: reviewer } : {}),
        limits: { max_rounds: maxRounds, total_timeout_secs: minutes * 60, max_no_progress_rounds: 3, run_timeout_secs: 300 },
    };
}
export function goalActions(status, cancelRequested = false) {
    if (cancelRequested && !["cancelled", "completed"].includes(String(status)))
        return { stop: true, resume: false };
    return {
        stop: ["queued", "running", "checking", "paused", "blocked"].includes(String(status)),
        resume: ["paused", "blocked"].includes(String(status)),
    };
}
export function newestGoal(current, incoming) {
    return current?.goal_id === incoming?.goal_id && Number(current.revision) > Number(incoming.revision) ? current : incoming;
}
export function goalDeliveryText(state, freshness, ready) {
    if (state !== "completed")
        return "Delivery verification is pending.";
    if (freshness === "current" && ready === true)
        return "Delivery ready at latest refresh; accepted artifact hashes still match.";
    if (freshness === "changed")
        return "Artifacts changed since acceptance; the current files are not verified for delivery.";
    return "Artifact freshness could not be verified. Refresh before delivery.";
}
export function goalPageMetadata(output) {
    const rounds = Array.isArray(output?.goal?.rounds) ? output.goal.rounds : [];
    const total = Number.isInteger(output?.total_rounds) ? Math.max(0, output.total_rounds)
        : Math.max(0, Number(rounds[rounds.length - 1]?.number) || 0);
    const offset = Number.isInteger(output?.round_offset) ? Math.max(0, output.round_offset) : Math.max(0, total - rounds.length);
    return { total, offset };
}
export function isHostGoal(goal) { return typeof goal?.phase === "string"; }
export function hostGoalGuidance(goal) {
    if (goal.phase === "planning")
        return "Discuss the goal and save the full plan locally before execution.";
    if (goal.phase === "awaiting_confirmation")
        return "Review this saved plan, then explicitly tell ChatGPT to start in the conversation.";
    if (["completed", "cancelled"].includes(goal.phase))
        return "Goal finished. Review the recorded acceptance evidence.";
    if (goal.phase === "ready") {
        const confirmation = goal.confirmation;
        return goal.plan_materialized === true && !!goal.plan_hash && confirmation?.plan_version === goal.plan_version && confirmation?.plan_hash === goal.plan_hash
            ? "This plan version has user confirmation. Continue in ChatGPT to claim the next node."
            : "Confirmation does not match the current plan. Return to ChatGPT to verify the saved version.";
    }
    return "Continue in ChatGPT using the saved state; reconcile unfinished operations first.";
}
export function goalContextView(meter) {
    const warning = Number.isSafeInteger(meter?.warning_tokens) && meter.warning_tokens > 0 ? meter.warning_tokens : 24000;
    const handoff = Number.isSafeInteger(meter?.handoff_tokens) && meter.handoff_tokens > warning ? meter.handoff_tokens : Math.max(32000, warning + 1);
    const count = Number.isSafeInteger(meter?.observed_tokens) && meter.observed_tokens >= 0 ? meter.observed_tokens : null;
    if (count === null)
        return { count, warning, handoff, level: "unavailable", advice: "No saved context estimate for this goal." };
    const level = count >= handoff ? "handoff" : count >= warning ? "warning" : "normal";
    return { count, warning, handoff, level, advice: level === "handoff" ? "Save a checkpoint, then open a new ChatGPT conversation."
            : level === "warning" ? "Save progress and prepare a handoff." : "Visible text estimate is below the configured warning threshold." };
}
export function goalContinuationInstruction(goal) {
    return "继续本地当前任务，目标 ID：" + String(goal.goal_id) + "。先调用 goal_bootstrap 读取本地计划和状态，按当前阶段继续；如仍等待确认，先展示此版本计划供我确认。";
}
// Only the console reassembles detail materials. Model-facing reads and mutation
// receipts remain bounded and never disguise a summary as writable GoalState.
export const readHostGoalSection = async (read, goal, section) => {
    let cursor, text = "";
    const seen = new Set();
    for (let page = 0; page < 2048; page++) {
        const output = await read({ goal_id: goal.goal_id, view: "detail", section, ...(cursor ? { cursor } : {}) });
        if (!output || output.projection !== "detail" || output.section !== section || output.encoding !== "json_text"
            || output.goal?.goal_id !== goal.goal_id || output.goal?.revision !== goal.revision
            || output.goal?.plan_version !== goal.plan_version || typeof output.text !== "string") {
            throw new Error("Goal details changed during loading. Refresh to read one consistent revision.");
        }
        text += output.text;
        if (text.length > 8 * 1024 * 1024)
            throw new Error("Goal detail exceeds the console loading limit.");
        if (output.has_more === false)
            return JSON.parse(text);
        if (output.has_more !== true || typeof output.next_cursor !== "string" || !output.next_cursor || seen.has(output.next_cursor)) {
            throw new Error("Goal detail returned an invalid continuation cursor.");
        }
        const next = output.next_cursor;
        cursor = next;
        seen.add(next);
    }
    throw new Error("Goal detail exceeds the console page limit.");
};
// A context change invalidates late reads and mutation responses before they can render.
export class GoalPanelState {
    constructor() {
        this.context = "";
        this.generation = 0;
        this.selected = "";
        this.goals = [];
        this.detail = null;
        this.hostDetail = null;
        this.delivery = null;
        this.roundOffset = null;
        this.totalRounds = 0;
        this.pageOffset = 0;
        this.pendingStart = null;
    }
    reset(context) {
        this.context = context;
        this.generation += 1;
        this.selected = "";
        this.goals = [];
        this.detail = null;
        this.hostDetail = null;
        this.delivery = null;
        this.pendingStart = null;
        this.roundOffset = null;
        this.totalRounds = 0;
        this.pageOffset = 0;
    }
    current(generation, context) {
        return generation === this.generation && context === this.context;
    }
    start(params, key) {
        const fingerprint = JSON.stringify(params);
        if (!this.pendingStart)
            this.pendingStart = { fingerprint, params: { ...params, idempotency_key: key() } };
        if (this.pendingStart.fingerprint !== fingerprint)
            throw new Error("Goal request outcome is unknown. Retry the same request.");
        return this.pendingStart.params;
    }
}
export class GoalPanel {
    constructor(root, host) {
        this.root = root;
        this.host = host;
        this.state = new GoalPanelState();
        this.reading = false;
        this.mutating = false;
        this.detailRequest = 0;
        this.node("goal-create-form").addEventListener("submit", (event) => { event.preventDefault(); void this.create(); });
        this.node("goal-refresh").addEventListener("click", () => void this.refresh());
        this.node("goal-stop").addEventListener("click", () => void this.mutate("goal_cancel"));
        this.node("goal-resume").addEventListener("click", () => void this.mutate("goal_resume"));
        this.node("goal-round-previous").addEventListener("click", () => void this.page(-1));
        this.node("goal-round-next").addEventListener("click", () => void this.page(1));
        this.node("goal-copy-continuation").addEventListener("click", () => void this.copyContinuation());
    }
    node(id) { return this.root.querySelector("#" + id); }
    text(id, text) { this.node(id).textContent = String(text ?? ""); }
    message(text) { this.text("goal-status", this.host.translate(text)); }
    contextKey() { const context = this.host.context(); return context.key + ":" + context.project; }
    sync() {
        const context = this.host.context();
        if (this.state.context !== this.contextKey()) {
            this.state.reset(this.contextKey());
            this.reading = false;
            this.mutating = false;
            this.message("");
        }
        this.text("goal-project", context.project || this.host.translate("Choose a project in Projects & Sessions first."));
        this.render();
    }
    current(generation) { return this.state.current(generation, this.contextKey()); }
    async request(tool, params, generation) {
        const response = await this.host.call(tool, params);
        if (!this.current(generation))
            return null;
        if (response?.status === 401) {
            this.host.unauthorized();
            return null;
        }
        if (!response?.ok || response?.data?.success !== true) {
            // Only definite admission failures release the key. A durable store or
            // dispatch failure may follow a committed goal and must preserve replay.
            if (tool === "goal_start" && (response?.status === 403
                || (response?.status === 400 && response?.data?.success === undefined)
                || response?.data?.output?.error_kind === "invalid_goal"))
                this.state.pendingStart = null;
            this.message(response?.status === 403 ? "Goal access is unavailable to this credential."
                : String(response?.data?.error || response?.data?.message || (response?.status ? "Goal request failed." : "Goal request outcome is unknown. Retry the same request.")));
            return null;
        }
        return response.data.output;
    }
    async refresh() {
        this.sync();
        const context = this.host.context(), generation = this.state.generation;
        if (!context.connected || !context.project || this.reading || this.mutating)
            return;
        this.reading = true;
        this.message("Loading goals…");
        try {
            const output = await this.request("goal_list", { project: context.project }, generation);
            if (!output)
                return;
            this.state.goals = Array.isArray(output.goals) ? output.goals.map((goal) => newestGoal(this.state.goals.find((prior) => prior.goal_id === goal.goal_id), goal)) : [];
            if (!this.state.goals.some((goal) => goal.goal_id === this.state.selected)) {
                this.state.selected = this.state.goals[0]?.goal_id || "";
                this.state.roundOffset = null;
            }
            await this.readSelected(generation);
            if (this.current(generation))
                this.message("");
        }
        finally {
            if (this.current(generation)) {
                this.reading = false;
                this.render();
            }
        }
    }
    async readSelected(generation) {
        const selected = this.state.selected;
        const request = ++this.detailRequest;
        if (!selected) {
            this.state.detail = null;
            this.state.hostDetail = null;
            this.state.delivery = null;
            return;
        }
        let output = await this.request("goal_read", {
            goal_id: selected, round_limit: 1,
            ...(this.state.roundOffset === null ? {} : { round_offset: this.state.roundOffset }),
        }, generation);
        if (output?.projection === "summary" && isHostGoal(output.goal)) {
            const summary = output;
            try {
                const read = (params) => this.state.selected === selected && request === this.detailRequest
                    ? this.request("goal_read", params, generation) : Promise.resolve(null);
                const sections = {};
                for (const section of ["plan", "goal", "checkpoints", "attempts", "operations"]) {
                    sections[section] = await readHostGoalSection(read, summary.goal, section);
                }
                output = { ...summary, goal: sections.goal, plan: sections.plan,
                    checkpoints: sections.checkpoints.items, attempt: sections.attempts.items[sections.attempts.items.length - 1] ?? null,
                    operations: sections.operations.items, has_more: sections.checkpoints.has_more,
                    next_sequence: sections.checkpoints.next_sequence };
            }
            catch (error) {
                if (this.current(generation) && request === this.detailRequest)
                    this.message(String(error.message));
                output = summary;
            }
        }
        if (output && this.state.selected === selected && request === this.detailRequest) {
            const accepted = newestGoal(this.state.detail, output.goal);
            if (accepted === output.goal) {
                this.state.hostDetail = isHostGoal(output.goal) ? output : null;
                this.state.delivery = { freshness: output.delivery_freshness, ready: output.delivery_ready };
                const page = goalPageMetadata(output);
                this.state.totalRounds = page.total;
                this.state.pageOffset = page.offset;
            }
            this.state.detail = accepted;
        }
    }
    async page(direction) {
        if (this.reading || this.mutating || !this.state.detail)
            return;
        const next = this.state.pageOffset + direction;
        if (next < 0 || next >= this.state.totalRounds)
            return;
        this.state.roundOffset = next;
        await this.refresh();
    }
    async create() {
        this.sync();
        if (this.mutating || !this.host.context().connected)
            return;
        const value = (id) => this.node(id).value;
        let params;
        try {
            params = this.state.start(goalStartParameters({
                project: this.host.context().project, provider: value("goal-provider"), reviewer: value("goal-reviewer"),
                objective: value("goal-objective"), acceptance: value("goal-acceptance"), artifacts: value("goal-artifacts"),
                criteriaJson: value("goal-criteria-json"), maxRounds: value("goal-max-rounds"), minutes: value("goal-minutes"),
            }), this.host.operationKey);
        }
        catch (error) {
            this.message(String(error.message));
            return;
        }
        const generation = this.state.generation;
        this.mutating = true;
        this.message("Working…");
        this.render();
        try {
            const output = await this.request("goal_start", params, generation);
            if (!output)
                return;
            this.state.pendingStart = null;
            this.state.detail = output.goal;
            this.state.selected = output.goal.goal_id;
            this.state.delivery = null;
            ++this.detailRequest;
            this.state.roundOffset = null;
            const page = goalPageMetadata(output);
            this.state.totalRounds = page.total;
            this.state.pageOffset = page.offset;
            this.state.goals = [output.goal, ...this.state.goals.filter((goal) => goal.goal_id !== output.goal.goal_id)];
            this.message(output.replayed ? "Goal start recovered." : "Goal started.");
        }
        finally {
            if (this.current(generation)) {
                this.mutating = false;
                this.render();
            }
        }
    }
    async mutate(tool) {
        this.sync();
        if (this.mutating || !this.state.detail || isHostGoal(this.state.detail))
            return;
        const generation = this.state.generation, goalId = this.state.detail.goal_id;
        ++this.detailRequest;
        this.state.delivery = null;
        this.mutating = true;
        this.message("Working…");
        this.render();
        try {
            const output = await this.request(tool, { goal_id: goalId }, generation);
            if (!output)
                return;
            if (this.state.selected === goalId) {
                if (this.state.roundOffset !== null)
                    await this.readSelected(generation);
                else {
                    this.state.detail = newestGoal(this.state.detail, output.goal);
                    const page = goalPageMetadata(output);
                    this.state.totalRounds = page.total;
                    this.state.pageOffset = page.offset;
                }
            }
            this.state.goals = this.state.goals.map((goal) => goal.goal_id === goalId ? newestGoal(goal, output.goal) : goal);
            this.message("Goal updated.");
        }
        finally {
            if (this.current(generation)) {
                this.mutating = false;
                this.render();
            }
        }
    }
    async copyContinuation() {
        if (!isHostGoal(this.state.detail))
            return;
        try {
            await navigator.clipboard.writeText(goalContinuationInstruction(this.state.detail));
            this.message("Continuation instruction copied.");
        }
        catch {
            const instruction = this.node("goal-continuation");
            instruction.focus();
            instruction.select();
            this.message("Copy unavailable. Select and copy the continuation instruction below.");
        }
    }
    renderHostGoal(goal) {
        const tr = this.host.translate, output = this.state.hostDetail, version = output?.plan;
        const plan = version?.plan, meter = goalContextView(output?.context_meter);
        this.text("goal-detail-status", tr(String(goal.phase)) + " · " + tr("Revision") + " " + goal.revision);
        this.text("goal-detail-note", goal.state ? "" : tr("Showing a partial summary. Refresh to load the complete saved details."));
        this.text("goal-host-guidance", tr(output?.plan_artifacts_valid === false
            ? "Local plan files failed verification. Resolve this in ChatGPT before continuing." : hostGoalGuidance(goal)));
        this.node("goal-host-delivery-status").hidden = goal.phase !== "completed";
        this.text("goal-host-delivery-status", goal.phase === "completed"
            ? tr(goalDeliveryText(goal.phase, output?.delivery_freshness, output?.delivery_ready)) : "");
        this.text("goal-context-count", meter.count === null ? tr("No saved context estimate for this goal.")
            : tr("Latest ChatGPT conversation: estimated visible tokens") + ": " + meter.count.toLocaleString() + " · " + tr("Warning / handoff") + ": " + meter.warning.toLocaleString() + " / " + meter.handoff.toLocaleString());
        this.text("goal-context-advice", meter.count === null ? "" : tr(meter.advice));
        this.node("goal-context-warning").setAttribute("data-level", meter.level);
        const files = version?.materialization;
        this.text("goal-host-plan", version ? [tr("Plan version") + ": " + version.version, tr("Plan hash") + ": " + version.hash,
            ...(files ? ["plan.json: " + files.json_path, "SHA-256: " + files.json_hash, "plan.md: " + files.markdown_path, "SHA-256: " + files.markdown_hash]
                : [tr("Plan has not been saved locally yet.")])].join("\n") : tr("Plan has not been saved locally yet."));
        this.node("goal-continuation").value = goalContinuationInstruction(goal);
        const state = this.node("goal-host-state");
        state.replaceChildren();
        const savedState = goal.state ?? output?.progress_summary;
        for (const [label, value] of [["Completed summary", savedState?.completed_summary], ["Next step", savedState?.next_step],
            ["Issues", savedState?.issues], ["Decisions", goal.state?.decisions], ["Facts", goal.state?.facts], ["Artifacts", goal.state?.artifact_refs],
            ["Plan draft", goal.state?.plan_draft], ["State management", plan?.state_management], ["Final acceptance", plan?.final_criteria]]) {
            const section = document.createElement("details"), title = document.createElement("summary"), content = document.createElement("pre");
            section.open = ["Completed summary", "Next step", "Issues"].includes(String(label));
            title.textContent = tr(String(label));
            content.textContent = typeof value === "string" ? value || "—" : JSON.stringify(value ?? null, null, 2);
            section.append(title, content);
            state.appendChild(section);
        }
        const nodes = this.node("goal-host-nodes");
        nodes.replaceChildren();
        for (const [index, node] of (Array.isArray(plan?.nodes) ? plan.nodes : []).entries()) {
            const section = document.createElement("details"), title = document.createElement("summary"), work = document.createElement("p"), details = document.createElement("pre");
            section.className = "goal-host-node";
            section.open = node.id === goal.current_node_id;
            const status = goal.completed_node_ids?.includes(node.id) ? "passed" : node.id === goal.current_node_id ? goal.phase
                : goal.completed_node_ids_partial ? "Status omitted from summary" : "pending";
            title.textContent = (index + 1) + ". " + String(node.title) + " · " + tr(status);
            work.textContent = String(node.work);
            details.className = "goal-host-text";
            details.textContent = JSON.stringify({ id: node.id, inputs: node.inputs, outputs: node.outputs, criterion_ids: node.criterion_ids,
                acceptance: node.acceptance, failure_action: node.failure_action }, null, 2);
            section.append(title, work, details);
            if (output?.attempt?.binding?.node_id === node.id) {
                for (const check of Array.isArray(output.attempt.checks) ? output.attempt.checks : []) {
                    const evidence = document.createElement("pre");
                    evidence.className = "goal-check " + (check.passed ? "goal-check-pass" : "goal-check-fail");
                    evidence.textContent = String(check.criterion_id) + " · " + tr(check.passed ? "passed" : "failed") + " · " + tr(String(check.source)) + "\n" + String(check.evidence);
                    section.appendChild(evidence);
                }
            }
            nodes.appendChild(section);
        }
        if (!nodes.childElementCount) {
            const empty = document.createElement("p");
            empty.textContent = tr("No nodes planned yet.");
            nodes.appendChild(empty);
        }
        const checkpoints = Array.isArray(output?.checkpoints) ? output.checkpoints : [];
        this.text("goal-host-checkpoint", checkpoints.length || output?.operations?.length || output?.attempt ? JSON.stringify({
            checkpoints, has_more: output?.has_more, next_sequence: output?.next_sequence, attempt: output?.attempt,
            operations: output?.operations,
        }, null, 2) : tr("No saved checkpoint yet."));
    }
    render() {
        const tr = this.host.translate, context = this.host.context();
        this.node("goal-create-fields").disabled = this.mutating || !!this.state.pendingStart || !context.project || !context.connected;
        const start = this.node("goal-start");
        start.disabled = this.mutating || !context.project || !context.connected;
        start.textContent = tr(this.state.pendingStart ? "Retry same start" : "Start goal");
        this.node("goal-refresh").disabled = this.reading || this.mutating || !context.project || !context.connected;
        const list = this.node("goal-list");
        list.replaceChildren();
        for (const goal of this.state.goals) {
            const button = document.createElement("button");
            button.type = "button";
            button.className = "goal-list-item" + (goal.goal_id === this.state.selected ? " selected" : "");
            button.setAttribute("aria-pressed", String(goal.goal_id === this.state.selected));
            const title = document.createElement("strong"), meta = document.createElement("span");
            title.textContent = String(goal.objective || goal.spec?.objective || goal.goal_id);
            meta.textContent = tr(isHostGoal(goal) ? "ChatGPT goal" : "Background ACP goal") + " · " + tr(String(isHostGoal(goal) ? goal.phase : goal.state || "queued"));
            button.append(title, meta);
            button.addEventListener("click", () => {
                this.state.selected = goal.goal_id;
                this.state.detail = null;
                this.state.hostDetail = null;
                this.state.delivery = null;
                this.state.roundOffset = null;
                this.render();
                const generation = this.state.generation;
                void this.readSelected(generation).then(() => { if (this.current(generation))
                    this.render(); });
            });
            list.appendChild(button);
        }
        this.node("goal-list-empty").hidden = this.state.goals.length > 0;
        const goal = this.state.detail;
        this.node("goal-detail").hidden = !goal;
        this.node("goal-detail-empty").hidden = !!goal;
        if (!goal)
            return;
        this.text("goal-detail-objective", goal.objective || goal.spec?.objective);
        this.text("goal-detail-id", goal.goal_id);
        const hostGoal = isHostGoal(goal);
        this.node("goal-host-detail").hidden = !hostGoal;
        this.node("goal-acp-detail").hidden = hostGoal;
        if (hostGoal) {
            this.renderHostGoal(goal);
            return;
        }
        this.text("goal-detail-status", tr(String(goal.state || "queued")) + " · " + tr("Round") + " " + this.state.totalRounds + " / " + String(goal.spec?.limits?.max_rounds ?? "—"));
        this.text("goal-detail-note", goal.cancel_requested === true && !["cancelled", "completed"].includes(goal.state)
            ? tr("Confirming execution has stopped; click Stop goal again to reconcile its status.") : goal.reason || "");
        this.text("goal-delivery-status", tr(goalDeliveryText(goal.state, this.state.delivery?.freshness, this.state.delivery?.ready)));
        const actions = goalActions(goal.state, goal.cancel_requested === true);
        this.node("goal-stop").disabled = this.mutating || !actions.stop;
        this.node("goal-resume").disabled = this.mutating || !actions.resume;
        this.node("goal-round-previous").disabled = this.reading || this.mutating || this.state.pageOffset <= 0;
        this.node("goal-round-next").disabled = this.reading || this.mutating || this.state.pageOffset + 1 >= this.state.totalRounds;
        this.text("goal-round-page", tr("Displayed round") + " " + (this.state.totalRounds ? this.state.pageOffset + 1 : 0) + " / " + this.state.totalRounds);
        const rounds = this.node("goal-rounds");
        rounds.replaceChildren();
        for (const [index, round] of (Array.isArray(goal.rounds) ? goal.rounds : []).entries()) {
            const section = document.createElement("details"), summary = document.createElement("summary"), report = document.createElement("pre");
            section.className = "goal-round";
            section.open = index === goal.rounds.length - 1;
            summary.textContent = tr("Round") + " " + String(round.number) + " · " + tr(String(round.verdict || round.review_state || round.execution_state || "queued"));
            section.appendChild(summary);
            const feedback = document.createElement("p");
            feedback.textContent = String(round.feedback || "");
            section.appendChild(feedback);
            for (const check of Array.isArray(round.checks) ? round.checks : []) {
                const item = document.createElement("div"), title = document.createElement("strong"), evidence = document.createElement("pre");
                item.className = "goal-check " + (check.passed ? "goal-check-pass" : "goal-check-fail");
                const criterion = goal.spec?.criteria?.find((entry) => entry.id === check.criterion_id);
                title.textContent = String(criterion?.description || check.criterion_id) + " · " + tr(check.passed ? "passed" : "failed");
                evidence.textContent = String(check.evidence || "") + (check.error ? "\n" + String(check.error) : "");
                item.append(title, evidence);
                section.appendChild(item);
            }
            // JSON preserves complete typed check reports and exact evidence paths without interpreting tool output as markup.
            report.textContent = JSON.stringify(round, null, 2);
            const details = document.createElement("details"), label = document.createElement("summary");
            label.textContent = tr("Round details");
            details.append(label, report);
            section.appendChild(details);
            rounds.appendChild(section);
        }
        if (!rounds.childElementCount) {
            const empty = document.createElement("p");
            empty.textContent = tr("No rounds recorded yet.");
            rounds.appendChild(empty);
        }
    }
}
