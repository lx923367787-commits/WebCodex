# AGENTS.md — WebCodex Repository Guide

These rules apply to ordinary repository work. Read linked domain guidance only when relevant; a deeper `AGENTS.md` governs its directory.

WebCodex is actively developed. Requested features, fixes, and reliability improvements are welcome; preserve credential, process-tree, transport, durability, and boundedness contracts.

## Collaboration and privacy

- Sub-agents and external CLIs are optional. Do not require any delegation or CLI call, minimum/maximum count, model tier or thinking strength. Choose them only as the task needs; omission uses the tool's supported default.
- Verify actual installed CLI arguments when selecting a model or effort; discovery is capability information, not an extra framework restriction or a prerequisite to use a supported default.
- Within the authorized project, participants share project/workspace files, tools, execution status, logs, stdout/stderr and results. Only memory, execution objectives and acceptance criteria use the soft personal-directory convention: maintain your own and do not read or modify another Agent/CLI instance's directories for those three categories or deliberately retrieve those contents from copies. This is an instruction, not a technical guarantee of unreadability. Never classify an entire Agent directory, workspace, tool or output as private. Existing account/project authorization and credential audiences remain in force.
- Read `agent_assignment` / `agent_identity` for the Agent's actual Server broker resource locations. For a launched CLI use its Runner-provided `WEBCODEX_MEMORY_DIR`, `WEBCODEX_OBJECTIVES_DIR` and `WEBCODEX_ACCEPTANCE_DIR`; do not guess paths or treat Server paths as Runner paths. Keep independently delegated task text as the receiver's own objective; do not automatically copy the parent's memory, objectives or acceptance criteria.
- Keep current identity, STOP/PAUSE, duplicate-dispatch prevention and same-task handoff continuity. Those lifecycle checks do not impose collaboration quotas.

## 1. Verify and preserve

- Work only in the repository, worktree, and external targets authorized by the user.
- At task start, verify root, branch, HEAD, status, relevant changes, and recent history. Recheck after Git operations, observed concurrent changes, or before committing.
- Treat prompt paths, hashes, branches, and runtime state as expectations to verify, not permission to overwrite actual state.
- Preserve unrelated and concurrent work. Use guarded edits; do not reset, rebase, restore, clean, or rewrite history without an explicit request for that operation.
- Read only implementation, tests, documentation, and diffs relevant to the task.

## 2. Design for the current need

- Make workflows simple and obvious. Choose the smallest coherent solution with the fewest concepts, states, and maintenance costs. Add abstractions, configuration, or compatibility only for a concrete need; do not build for hypothetical consumers or scale.
- Treat existing code, comments, and tests as evidence of current or historical behavior, not permanent product requirements. Trace current entry points, consumers, and Git history before retaining or removing a concept; retire obsolete configuration, adapters, and dedicated tests together while preserving real boundaries.
- Protect real boundaries: credentials, public entry points, destructive actions, execution targets, history, and published artifacts. Model new authority, credential audiences, and replaceable targets explicitly when sharing scopes, identities, or leases would broaden access or allow stale requests to retarget.
- Prefer structured process/argv and durable Job/observation primitives for model-facing execution. Use shell for shell semantics and short, tightly related command chains when that avoids unnecessary sequential tool round trips; keep unrelated effects, validation boundaries, and consequential mutations separate. Structured lifecycle state owns retry safety.
- Treat model-surface directness as preferred exposure and ergonomics, not as execution authority. A tool that is otherwise admitted may use an available generic dispatch path without bypassing its canonical schema, authorization, permission, capability, or effect checks; hidden/unadmitted tools remain unavailable.
- Keep ToolSpec/OpenAPI descriptions self-contained and accurate about selection, authority, effects, retries, continuation, uncertainty, and recovery. Guide file transfer toward host-native import/export paths when available instead of routing complete binary payloads through model-generated Base64. Use up to `MODEL_TOOL_DESCRIPTION_MAX_CHARS` when needed; arbitrary brevity must not remove semantics.
- Keep core execution and Job semantics protocol-, UI-, transport-, and OS-neutral. Host-specific features such as MCP App orchestration belong in optional adapters.
- HTTP/MCP requests are stateless unless their exact adapter contract supplies a stable `ClientWindow`. Never infer Workflow Session, model-context retention, or authority from transport/audit IDs, credentials, projects, or prior requests. Keep Connector Tasks, Workflow Sessions, durable Agents/Conversations, and Agent Tasks distinct; follow the Session and Agent contracts linked below.

Product direction: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## 3. Implement and review

- User outcomes, scope, prohibitions, and acceptance conditions are hard constraints. Suggested files and steps are guidance unless explicitly mandatory; adapt them to verified code and conventions.
- Follow existing architecture and naming. Avoid unrelated cleanup, duplicate representations, speculative compatibility, and broad refactors without a current need.
- For cross-layer features, map the authoritative path before editing and complete it end to end. Update affected enums, registries, schemas, adapters, and projections; do not touch unrelated interfaces merely because they exist.
- Use compiler, type, schema, and exhaustiveness failures to locate missing integration before broadening validation.
- Add focused tests where practical and update documentation when public behavior or operations change.
- Review the completed diff separately from implementation. Check completeness, then relevant authority, bounds, privacy, and replay risks. Match review depth to the change; resolve demonstrated issues without designing around hypothetical reviewer concerns.
- Ask only when required information cannot be discovered, instructions materially conflict, or proceeding could destroy work. Otherwise continue and report material deviations.

## 4. Validate changed behavior

- Start with the smallest check that can detect a regression; stop when sufficient relevant evidence passes. Reuse results when the validated source and relevant dependencies are unchanged. Repeat only after relevant changes, invalidating conflicts, incomplete/flaky runs, or a user request.
- Documentation-only changes need no Cargo compilation or tests. Rust changes normally need affected formatting, the smallest relevant package check when compilation may be affected, and focused tests.
- Use the `dogfood` Cargo profile for optimized development builds; reserve `release` for formal release/publication artifacts.
- Full library/workspace tests, `--all-targets`, ignored tests, real-process harnesses, and E2E are not defaults. Use them for explicit requests, release/deployment, cross-cutting boundaries, broad conflict resolution, or gaps in focused coverage. State the reason first.
- Put tests in existing dedicated module trees, grouped by domain. Keep inline `#[cfg(test)]` blocks small and tied to private helpers; process/network/integration fixtures belong in dedicated test modules.
- Async readiness must use a `wait_*` path with one absolute deadline that progress never resets. Use `probe_*` only when immediate absence is valid or inside an already-owned deadline.
- Distinguish current failures, pre-existing failures, expected negative cases, and failures resolved by retry. Never weaken authentication, authorization, validation, schemas, sandboxing, or tests to get a passing result.

Testing guidance: [`docs/TESTING.md`](docs/TESTING.md).

## 5. Deliver only what is authorized

- Never print, commit, or expose credentials, authorization headers, private keys, tokens, secret files, or sensitive command output.
- Before committing, review status and diff. Keep unrelated work separate; do not amend an unrelated commit.
- Push, publish only when explicitly requested with a named destination.
- Never force-push, move published tags, overwrite releases, or rewrite published history without an explicit request naming the operation and target. Release operations must also obey the immutable artifact contract below; failed pre-publication tag recovery is permitted only through its explicitly authorized guarded reclaim path.
- Named development/dogfood deployments are separate from releases. Deploy the reviewed source commit, record its build identity without hiding dirty state, preserve rollback, and run focused smoke. They do not authorize version bumps, tagging, or publication.
- Before finishing, check the final diff, whitespace, worktree, conflicts, and active jobs. Report the outcome, changed files/targets, validation, Git/job state, material deviations, and remaining risks. State whether push, publication, deployment, or service operations occurred.

For release, recovery, or deployment work, follow [`docs/agent/release-process.md`](docs/agent/release-process.md) and, for release/publication gates, [`docs/RELEASE_CHECKLIST.md`](docs/RELEASE_CHECKLIST.md).

## 6. Load relevant domain rules

- Public runtime and API surfaces: [`docs/agent/openapi-guidelines.md`](docs/agent/openapi-guidelines.md).
- Workflow Sessions and request identity: [`docs/agent/session-model.md`](docs/agent/session-model.md).
- Manual multi-window collaboration: [`docs/agent/manual-window-collaboration.md`](docs/agent/manual-window-collaboration.md).
- Durable Agent identity, Conversation/Wake, and asynchronous Agent work: [`docs/architecture/durable-agent-runtime.md`](docs/architecture/durable-agent-runtime.md).
- Current durable Agent/Conversation/Wake implementation contract: [`docs/architecture/durable-agent-conversation.md`](docs/architecture/durable-agent-conversation.md).
- Authority boundaries: [`docs/agent/permission-model.md`](docs/agent/permission-model.md).
- Architecture decisions: [`docs/agent/architecture-decisions.md`](docs/agent/architecture-decisions.md).

Use these as domain sources of truth rather than duplicating their detailed contracts here.

## 7. 文件生命周期管理（长期生效）

### 范围与责任


- AI 对本项目文件的创建、修改、合并、清理和删除负责；创建前先查找可复用实现，避免重复文件和临时产物长期堆积。
- 本章适用于本仓库及其子目录中的开发任务，仅授权处理当前任务已授权的仓库或 worktree。不得将权限扩大到用户主目录、其他项目、其他 worktree 或项目外文件；操作前核实绝对路径及符号链接、目录联接的实际目标。
- 本章补充而不覆盖前述开发、验证、保密、发布及协作规则，也不削弱子目录的重要指令。确认无用且满足以下恢复及并发保护条件的普通项目文件，允许自主清理和删除，无需逐项征求用户批准；这不授权批量 `git clean`、重置工作区、改写历史或自动提交。

### 调查与判断

- 对用途不明的文件，主动调查入口、引用、导入、构建与打包配置、动态加载、测试、文档和 Git 历史；优先使用 `rg`、Git 和现有项目工具。不得仅凭文件名、时间、一次搜索无结果、未跟踪或被忽略状态断言无用，也不得仅因不了解用途而永久保留。
- 清理重复实现、废弃代码时，应验证调用方、运行时约定及外部消费方式，并同步处理本轮受影响的引用和相关测试。只运行能验证受影响行为的现有检查；纯规则或文档变更按第 4 节处理，不引入额外框架。
- 对无法验证的候选文件暂时保留，在现有任务记录或交接记录中记下路径、已查证据、保留原因及下一步验证条件，后续相关任务继续调查；没有现成记录时在交付说明中记录，不为每个文件另建报告。

### 删除前必须可恢复

- 删除前必须确保能恢复文件的当前完整内容，并记录恢复来源和方法。对已提交且工作区、暂存区均未变化的文件，可使用已验证存在的 Git 提交及路径作为恢复依据；仅仅“被 Git 跟踪”不代表当前版本已经备份。
- 对未提交修改、暂存内容、未跟踪文件及被忽略文件，Git 提交通常不能完整恢复。删除或破坏性合并前，使用现有备份机制，或在本项目内明确隔离、不会进入提交和发布产物的恢复目录保存副本；验证副本可读且内容一致（例如大小和哈希），记录原路径、备份位置及恢复方法。无 Git 时同样要求已验证的备份。
- 备份不得放入即将删除的目录，不得覆盖旧备份或在同一轮清理中删除恢复副本；不得把密钥或敏感数据加入 Git、普通日志或共享备份。无法安全备份或验证恢复时保留原文件并记录原因。
- 不得自动永久删除用户数据、数据库及其恢复文件、密钥或项目外文件；即使被忽略或名称像临时文件，也不获得删除授权。此类内容不属于普通自主清理对象，另需用户针对具体对象和操作的明确授权。

### 并发保护与开发收尾

- 不得删除其他 Agent 正在修改的文件。任务开始时检查工作区现状和可用的协作记录；删除前重新检查目标的内容、状态及任务归属。已有未提交或未跟踪内容不能视为本轮垃圾，Git 状态干净也不能证明没有其他 Agent 在操作。
- 使用现有协作机制和 worktree 隔离，不新增锁框架；协作记录不是文件锁。发现并发变化、活动写入或无法排除其他 Agent 正在修改时，停止对该文件的删除，保留并记录待协调原因。
- 每次开发结束后，自动检查本轮产生的临时文件、重复实现、废弃代码及未跟踪文件，同时检查本轮涉及的被忽略产物。结合任务开始时的状态，优先使用 `git status --short`、`git diff`、`git diff --cached`、`git ls-files --others --exclude-standard`；对具体候选使用 `git check-ignore -v -- <path>`。不把全仓历史存量自动纳入本轮清理。
- 对确认无用且恢复、依赖及并发检查均通过的文件自主清理；对无法验证的文件保留并记录原因。完成后复查差异、相关测试和工作区状态，在交付说明中简述清理项、恢复来源和保留项；无候选时注明无本轮清理项即可。

### 单一规则来源

- 文件生命周期规则仅维护在本文件本章。其他 Agent 的入口文件仅引用根目录 `AGENTS.md`，不得复制本章正文；已有其他重要指令必须保留。
- Agent 开始工作及接续任务时读取本文件和相关子目录指令。若客户端不自动加载 `AGENTS.md`，通过其现有指令入口显式要求读取本文件；文件可读取不等于已经加载，正在运行的会话需重新读取后才能应用更新。
- 本机制是随开发任务执行的持久项目指令，不是后台定时清理服务。本次安装仅修改并验证规则，不执行历史文件的大规模清理。
