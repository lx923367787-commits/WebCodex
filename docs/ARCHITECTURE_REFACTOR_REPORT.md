# WebCodex 架构收口重构报告

本文记录源码重构阶段。后续已按用户明确要求更新当前安装、重启并完成连接验收，
见 [安装重启与验收报告](ARCHITECTURE_REFACTOR_ACCEPTANCE.zh-CN.md)。

日期：2026-09-28。工作目录：`E:\wbecodex\source`。
基线是 `feat/mycodex-tools` / `15e4287cda7f5e673dc449c34ed2838f9bcad4ca`
**加上任务开始时全部未提交工作**。本报告的变化统计相对于这份工作区基线，
不是把既有 Goal、文档工具和协议变更算入本轮。

## 结果与对应关系

| 原结构 | 当前结构 | 结果 |
| --- | --- | --- |
| Runner `main.rs`，5,838 行 | `main.rs`，87 行；复用原 `webcodex_runner/` 命名空间 | 主文件只负责初始化、CLI/config、启动 Runner 和错误退出 |
| `dispatch.rs`，2,567 行 | `dispatch/mod.rs`，276 行；`pipeline.rs`、`projection.rs`、六个补齐的族 Handler | 顶层只有 typed variant → Handler 路由；业务和统一门禁分离 |
| ToolRuntime 28 个生产字段、29 个测试字段 | 11 个直接字段；独立领域服务及已有 SessionStore | 构造/注入依赖集中，33 个领域操作不再实现于 ToolRuntime |
| 隐含的状态归属 | 中英文 State Ownership Contract | 明确写入方、观察方、持久性、权限作用、冲突优先级和恢复边界 |

先完成 [分析计划](ARCHITECTURE_REFACTOR_PLAN.md) 和状态契约，再依次实施
Runner、Dispatch、Runtime。没有新增运行框架、crate、第三方依赖、RPC 或存储层。

## ToolRuntime 职责图

```text
ToolRuntime — composition root / compatibility facade
├── runner_registry: Arc<RunnerRegistry>  [保留原公开字段，唯一实例来源]
│   └── execution(): ExecutionService   [借用当前 registry]
│       ├── Project resolution / caller visibility
│       ├── Project list / register / create / managed worktree
│       └── Project lifecycle dispatch
├── runtime_info                         [保留原公开字段]
├── sessions: workflow-session::SessionStore
│   └── Ledger / context / message / attention / assignment fence
├── execution_state: ExecutionState
│   └── Coding Run bindings / persistent shells / workspace checkpoints / Git cursor key
├── goals: GoalService
│   └── 独立 Goal DB handle / live GoalController
├── gateways: GatewayService
│   └── Plugin bindings / MCP observations / SSH bindings（独立 store）
├── observation: ObservationService
│   └── Activity / connection observation / Window activity / metrics / audit query DB
├── memory: MemoryService
│   └── Memory CRUD / bootstrap / scope lifecycle
├── communication: CommunicationService
│   └── Agent identity / endpoint / Conversation / delivery / Wake / continuation
├── validation: ValidationService
│   └── 终态 Job 快照协调锁与测试 hook
└── config: RuntimeConfig
    └── deadlines / sync waits / exposure / PermissionEvaluator configuration
```

保留公开 `runner_registry` 有实际兼容性意义：调用方能够替换这个字段。
ExecutionService 因此借用当前 Arc，不存另一份可能过期的 Arc。新增回归验证了
替换后的解析使用新 registry，而之前 clone 的 Runtime 继续使用自己的 registry。
`RuntimeInfo` 同样保留原公开入口。

SessionStore 已由独立 `webcodex-workflow-session` crate 实现其领域服务，
没有增加一层只转发所有 ledger 方法的 SessionService。Memory scope 清理仍保持
原有 Runner inventory 锁跨度，跨域调用显式接收当前 registry。
Goal DB 不再通过名为 Communication 的字段查找；底层仍共享同一个 SQLite 实例，
没有新增表，也没有合并领域权威。

仍保留 ToolRuntime 上跨领域的 Goal、Coding Task、执行证据协调方法。本轮先收拢
依赖与稳定操作边界，未把这些带有安全顺序的流程一次性改写为另一套 service graph。

## Dispatch Handler 清单

| 族 | 实现 |
| --- | --- |
| Goal / Host Goal | `dispatch/goal.rs` → 既有 Goal/Host Goal 业务模块 |
| Coding Agent | `dispatch/coding_agent.rs` |
| Skill | `dispatch/skill.rs` |
| Agent Task | `dispatch/agent_task.rs` |
| Communication | `dispatch/communication.rs` → CommunicationService |
| Memory | `dispatch/memory.rs` → MemoryService |
| Discovery / Runtime | 既有 `discovery_tools.rs` |
| Runner config | 既有 `runner_config.rs` |
| Session / message / validation summary | 既有 `session_tools.rs` |
| Coding Task | 既有 `coding_task_tools.rs` |
| Handoff | 既有 `handoff_tools.rs` |
| Workspace checkpoint | 既有 `checkpoint.rs` |
| Computer | 既有 `computer_tools.rs` |
| Project | 既有 `project_tools.rs` → ExecutionService |
| Document | 既有 `documents.rs` |
| Process / detached process / script / shell | 既有 `shell_tools.rs` |
| Persistent shell | 既有 `session_shell.rs` |
| Patch | 既有 `patch_tools.rs` |
| Import / File / Artifact | 既有 `conversation_import.rs`、`file_tools.rs` |
| Git | 既有 `git_tools.rs` |
| Cargo / Go validation | 既有 `cargo_tools.rs` |
| Job | 既有 `job_tools.rs` |
| Hygiene | 既有 `hygiene_tools.rs` |
| LSP | 既有 `lsp_tools.rs` |
| Plugin / SSH / native MCP gateway | 保留既有 specialized governance 路径及对应 gateway |

统一链路仍为：HTTP/MCP → Kernel parsing/scope/context → canonical pipeline 的
Session/Project/Runner/Permission/Goal gate → typed Router/Handler → Runner。
Handler 没有新增 JSON 解析、Scope、PermissionEvaluator、epoch 或 fence 门禁。
动态 Gateway 的既有 specialized governance 不经过静态 metadata 的重复判断。
canonical 结果仍先记录证据，后由 projection 做模型响应精简。

## Runner 模块图

```text
main.rs                              启动组合
webcodex_runner/
├── cli.rs                           CLI 参数/帮助/版本
├── registration.rs                  注册/capability/build identity
├── transport.rs                     既有 WS/QUIC/runtime/shutdown orchestration
│   └── transport/
│       ├── http.rs                  有界 HTTP、decode/error/recovery classification
│       └── polling.rs               polling supervision、one-poll dispatch
├── jobs/
│   ├── mod.rs                       JobManager state / clone / owner lifetime
│   ├── queue.rs                     admission / capacity / start scheduling
│   ├── context.rs                   typed Job context validation
│   ├── process.rs                   bounded readers / ManagedChild lifecycle
│   ├── execution.rs                 structured process / script Job start
│   ├── shell.rs                     local/SSH shell Job workers
│   ├── detached.rs                  detached start / recovery / observation
│   ├── updates.rs                   state snapshots / bounded output / delivery queue
│   ├── lifecycle.rs                 stop / shutdown / terminal certainty
│   └── stream_tests.rs              moved UTF-8 stream tests
├── files/dispatch.rs                 file operation dispatch
├── projects/creation.rs              tracked project creation cleanup
├── shell.rs / persistent_shell.rs    既有 execution backends
├── plugin.rs / mcp_gateway.rs / coding_agent.rs / external_tools.rs
├── detached_job/                     既有 durable supervisor implementation
├── projects.rs                       既有 project registry
└── shutdown.rs                       既有 shutdown coordinator
```

迁移审查核对原 `main.rs` 的 157 个函数体：149 个去除空白和可见性后逐字一致，
其余 8 个仅多了 rustfmt 的尾逗号。复核时还为迁移的 10 条日志显式保留原
`webcodex_runner` tracing target，并修正了 Unix detached fixture 的子进程测试名路径。
Job update delivery retry 仍只是投递状态，
不重试业务执行。队列、锁跨度、owner drop、detached supervisor 与 shutdown
顺序没有改写。测试依赖放入 test-only `test_support.rs`，生产主文件不再承担
所有内部符号的转发。

## State Ownership Contract

- [English](STATE_OWNERSHIP.md)
- [中文](STATE_OWNERSHIP.zh-CN.md)

契约覆盖 Goal/Plan/Node/Attempt/Fence、Context retirement、Operation receipt、
Job/Detached Job/Run、Session history/message/context revision、两种 checkpoint、
Validation evidence、Project filesystem、Memory、Agent/Conversation/Wake、
Plugin/MCP/SSH binding/schema、permission、配置、Window/metrics/audit。
每项都有 D/E/R/O/A 分类、唯一写入方、三类生命周期保留规则和冲突判定。

尤其保留：旧 context 退休后不能恢复业务权；Session 的“调用过”不是执行成功；
响应丢失后的 OutcomeUnknown 通过原 receipt/Job/Run 协调，不自动重放副作用。

## 验证

Windows，本轮全部使用命令行。测试线程栈为 `RUST_MIN_STACK=33554432`；完整工作区
使用 `--test-threads=4`，真实进程组使用 `--test-threads=1`。原始日志、初始源码快照
和逐文件清单位于本地忽略目录 `target/architecture-refactor-baseline/`。

| 命令 / 检查 | 结果 |
| --- | --- |
| 修改前 `cargo check --workspace` | 通过 |
| 修改前 `cargo test --workspace`（默认 Windows 栈） | Host Goal MCP 测试发生栈溢出，不能作为完整结果 |
| 原 Server 测试二进制，大栈、4 线程 | 2,524 通过，29 失败 |
| `cargo fmt --check` | 通过；原工作区已有格式差异，本轮完成 rustfmt 整理 |
| `cargo check --workspace` | 通过，无新增编译警告（保留原平台警告） |
| `cargo check --workspace --tests`（最终文件状态） | 通过 |
| `cargo test --workspace --no-fail-fast -- --test-threads=4` | **5,027 通过，40 失败，79 ignored**；所有 workspace crate 和 doc-test 均执行 |
| Workspace 修正测试 PATH 后补测 | **60 通过，0 失败**；10 个原失败来自找不到 `sh` |
| `cargo test --locked -p webcodex-runner runner_real_process -- --ignored --test-threads=1` | **48 通过，0 失败** |
| 新增 registry 替换/clone 兼容性回归 | 通过 |
| 基线来源/机械迁移/契约类型差异复核 | core、store、runner-registry 源码相对初始快照未变；公开 facade 未删方法或字段 |
| 架构文档本地链接检查 | 通过 |
| `git diff --check` / 最终文件复核 | 通过；127 个本轮路径无尾随空白、无缺失文档链接；无未合并索引项 |

完整工作区各 crate 结果如下，Core 的通过数包含 1 个 integration test。
空二进制 target 和所有 doc-test 均通过（0 tests），没有把它们算作额外案例。

| Crate | 通过 | 失败 | Ignored |
| --- | ---: | ---: | ---: |
| webcodex | 2525 | 29 | 0 |
| webcodex-admin | 18 | 0 | 0 |
| webcodex-cli | 313 | 0 | 0 |
| webcodex-computer | 59 | 0 | 6 |
| webcodex-connector-runtime | 67 | 0 | 0 |
| webcodex-core | 235 | 1 | 0 |
| webcodex-persistent-shell | 3 | 0 | 8 |
| webcodex-process | 2 | 0 | 15 |
| webcodex-runner | 793 | 0 | 50 |
| webcodex-runner-config | 29 | 0 | 0 |
| webcodex-runner-registry | 247 | 0 | 0 |
| webcodex-store | 234 | 0 | 0 |
| webcodex-tool-contracts | 113 | 0 | 0 |
| webcodex-tool-runtime-contracts | 78 | 0 | 0 |
| webcodex-validation | 91 | 0 | 0 |
| webcodex-workflow-session | 170 | 0 | 0 |
| webcodex-workspace | 50 | 10 | 0 |

**失败归因与补测，不把失败写成通过：**

- Server 的 29 个失败集合与重构前完全相同，新测试使通过数增加 1。
  主要是已有 Goal 提示长度/Schema 上限、工具目录期望、旧错误文案、Office MIME
  路径夹具和 Windows ACL 夹具。没有为通过这些测试而修改公共 Schema 或权限。
- Core 的 `structured_validation_target_identity_hashes_are_stable` 在工作区合并
  `serde_json/preserve_order` feature 时失败。用**初始源码快照**显式启用这个
  feature 复现了相同哈希差异；单独无此 feature 的基线 Core 为 235/235 通过。
  这不是本轮修改的哈希实现或依赖，未更改它的公共身份语义。
- Workspace 的 10 个 `project_overview::validation_tests` 失败也在初始快照复现。
  它们调用 `sh`，原 Windows PATH 只有 Git 的 `cmd` 目录；仅对补测进程加入
  `C:\Program Files\Git\bin` 后 60/60 通过。夹具的临时 `git commit` 仍输出缺少
  测试提交身份的提示，但这些测试使用已暂存的 index，全部断言通过；未修改用户 Git 配置。
- Runner 初轮 792/1/50 的唯一失败来自 Windows 保留设备名 `nul.txt`；改为
  `nul-bytes.txt`，保留相同字节、上限和断言后，最终 793/0/50。生产读文件逻辑未改。
- 48 项 Runner 真实进程证据包含 process tree、timeout/stop race、owner drop、
  detached supervisor、Plugin blocked stdin、SSH、Git、Validation 与 shutdown。
  其余 ignored/manual/外部服务测试没有隐式执行；未执行 GUI 自动化。

关键链路回归均通过：

| 边界 | 代表性测试（名称后缀） |
| --- | --- |
| HTTP → Kernel → Dispatch → Runner | `http_tools_call_generic_path_dispatches_representative_project_tools`、`http_projects_run_job_dispatches_to_runtime` |
| MCP tools/call / Goal 全流程 | `host_goal_mcp_plan_confirmation_claim_and_fresh_conversation_restore`；Host Goal 的 plan/confirm/claim/checkpoint/check/recovery suites |
| Handoff 原子退休、新 Attempt/Fence、旧 Chat 拒绝 | `host_goals_handoff_atomic_retirement_requires_a_new_attempt_and_keeps_source_evidence`、`host_goals_draining_allows_only_existing_acceptance_and_retired_blocks_activation_even_with_forged_epoch` |
| Unknown side effect 不重放 | `dispatched_bridge_disconnect_is_outcome_unknown_and_not_replayed` |
| Plugin describe/call/schema change | `generic_runtime_plugin_gateway_list_describe_call_and_error_certainty_share_exact_dispatch`、`plugin_binding_runner_replacement_and_schema_change_fail_closed` |
| MCP provider schema fail closed | `schema_change_blocks_effectful_call_without_retiring_provider` |
| 唯一权限判断 | `kernel_path_does_not_double_evaluate_or_duplicate_request_id` |
| 公开 registry 的兼容性 | `execution_service_uses_replaced_public_registry_and_clone_keeps_its_registry` |

## 未完成项与验证限制

- **工作区测试全绿尚未达成**：剩余 29 个 Server 基线失败及 1 个 feature 组合相关
  Core 基线失败。修复这些问题会涉及本轮范围外的已有实现/测试契约，未静默改变它们。
- 本机验证 Windows；没有声称已在 Linux/macOS 执行 native tests。已复核 cfg 迁移，
  并修正迁移后 Unix detached 子进程 fixture 的精确测试名。
- 没有部署真实服务或运行生产环境 E2E；HTTP/MCP、Goal/Handoff 使用已有本地
  fixture/RunnerRegistry 回归，另有上述 48 项真实进程证据。

## 最后五个问题

1. **ToolRuntime 是否仍有 God Context 倾向？** 有残余，主要是跨领域 Goal/Coding Task/
   evidence orchestration；字段已分域，33 个领域操作迁入服务，外部兼容字段没有重复副本。
2. **dispatch 是否仍是主要扩展瓶颈？** 不是。276 行顶层是穷尽路由，新业务进入对应
   Handler；共用安全与证据顺序集中保留在 pipeline。
3. **Runner main 是否只承担 composition/startup？** 是。87 行主文件只做入口组合，
   Job、Transport 辅助与执行实现已移出。
4. **State authority 能否唯一确定？** 能。契约按具体事实明确唯一权威；Unknown 仍是
   Unknown，不能用观察投影或 Session 历史伪造确定结果。
5. **是否改变外部行为？** 没有设计或实施外部业务行为变更；工具名称、Schema、HTTP/
   MCP、Runner wire、SQLite、permission、Goal/Handoff 与恢复/幂等语义保持。
   测试并非全绿，以上基线失败和平台验证范围是这个结论的明确边界。

## 刻意保留的后续拆分点

1. `transport.rs` 的现有 WebSocket、QUIC 和 runtime orchestration 还可继续独立，
   本轮优先抽出原主文件中的 HTTP/polling，避免同时改动传输状态机。
2. `jobs/shell.rs` 保留约千行的 local/SSH 工作循环，先确认稳定职责边界再拆。
3. ToolRuntime 的 Goal/Host Goal、Coding Task 与执行证据仍有跨域 orchestration；
   后续可逐段移动到领域服务，继续保护 admission、receipt 与 evidence 顺序。
4. 原有 Handler 接收 ToolRuntime facade 的部分路径仍可收窄依赖；本轮没有为了
   形式统一而新增庞大的 context/trait/factory 层。

这些是后续机会，不宣称本轮已彻底消除所有大模块。完整修改文件清单见下文，测试结果见上文。
最终 branch/HEAD 与基线相同，`Cargo.toml`、`Cargo.lock` 和 Runner manifest 未改变；
原有未提交工作保留，本轮代码也留在工作区。所有本轮 Cargo/测试命令已结束。
本轮没有提交、推送、发布、部署或重启外部服务。

## 修改文件清单

以下 127 个路径按任务开始时的工作区快照计算，保留原有未提交内容。
其中 contracts 与部分测试入口的变化仅为工作区 rustfmt 格式整理。

| 路径 | 本轮操作 |
| --- | --- |
| [crates/webcodex-runner/src/main.rs](../crates/webcodex-runner/src/main.rs) | 修改 |
| [crates/webcodex-runner/src/main_tests/file_read.rs](../crates/webcodex-runner/src/main_tests/file_read.rs) | 修改 |
| [crates/webcodex-runner/src/test_support.rs](../crates/webcodex-runner/src/test_support.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/cli.rs](../crates/webcodex-runner/src/webcodex_runner/cli.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/dispatch.rs](../crates/webcodex-runner/src/webcodex_runner/dispatch.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/files.rs](../crates/webcodex-runner/src/webcodex_runner/files.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/files/dispatch.rs](../crates/webcodex-runner/src/webcodex_runner/files/dispatch.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/job_manager_tests.rs](../crates/webcodex-runner/src/webcodex_runner/job_manager_tests.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/context.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/context.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/detached.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/detached.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/execution.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/execution.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/lifecycle.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/lifecycle.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/mod.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/mod.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/process.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/process.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/queue.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/queue.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/shell.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/shell.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/stream_tests.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/stream_tests.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/jobs/updates.rs](../crates/webcodex-runner/src/webcodex_runner/jobs/updates.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/mod.rs](../crates/webcodex-runner/src/webcodex_runner/mod.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/plugin_tests.rs](../crates/webcodex-runner/src/webcodex_runner/plugin_tests.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/projects.rs](../crates/webcodex-runner/src/webcodex_runner/projects.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/projects/creation.rs](../crates/webcodex-runner/src/webcodex_runner/projects/creation.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/registration.rs](../crates/webcodex-runner/src/webcodex_runner/registration.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/ssh.rs](../crates/webcodex-runner/src/webcodex_runner/ssh.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/transport.rs](../crates/webcodex-runner/src/webcodex_runner/transport.rs) | 修改 |
| [crates/webcodex-runner/src/webcodex_runner/transport/http.rs](../crates/webcodex-runner/src/webcodex_runner/transport/http.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/transport/polling.rs](../crates/webcodex-runner/src/webcodex_runner/transport/polling.rs) | 新增 |
| [crates/webcodex-runner/src/webcodex_runner/transport_tests.rs](../crates/webcodex-runner/src/webcodex_runner/transport_tests.rs) | 修改 |
| [crates/webcodex-tool-contracts/src/registry/input_schemas.rs](../crates/webcodex-tool-contracts/src/registry/input_schemas.rs) | 修改 |
| [crates/webcodex-tool-contracts/src/registry/mod.rs](../crates/webcodex-tool-contracts/src/registry/mod.rs) | 修改 |
| [crates/webcodex-tool-contracts/src/registry/output_schemas.rs](../crates/webcodex-tool-contracts/src/registry/output_schemas.rs) | 修改 |
| [crates/webcodex-tool-contracts/src/tests/mod.rs](../crates/webcodex-tool-contracts/src/tests/mod.rs) | 修改 |
| [crates/webcodex-tool-contracts/src/tool_definition.rs](../crates/webcodex-tool-contracts/src/tool_definition.rs) | 修改 |
| [crates/webcodex-tool-runtime-contracts/src/lib.rs](../crates/webcodex-tool-runtime-contracts/src/lib.rs) | 修改 |
| [crates/webcodex-tool-runtime-contracts/src/tool_call_test_support.rs](../crates/webcodex-tool-runtime-contracts/src/tool_call_test_support.rs) | 修改 |
| [docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md) | 修改 |
| [docs/ARCHITECTURE_REFACTOR_PLAN.md](../docs/ARCHITECTURE_REFACTOR_PLAN.md) | 新增 |
| [docs/ARCHITECTURE_REFACTOR_REPORT.md](../docs/ARCHITECTURE_REFACTOR_REPORT.md) | 新增 |
| [docs/STATE_OWNERSHIP.md](../docs/STATE_OWNERSHIP.md) | 新增 |
| [docs/STATE_OWNERSHIP.zh-CN.md](../docs/STATE_OWNERSHIP.zh-CN.md) | 新增 |
| [src/admin_http.rs](../src/admin_http.rs) | 修改 |
| [src/admin_project_lifecycle.rs](../src/admin_project_lifecycle.rs) | 修改 |
| [src/agent_continuation_tests.rs](../src/agent_continuation_tests.rs) | 修改 |
| [src/connector_runtime/host_adapter.rs](../src/connector_runtime/host_adapter.rs) | 修改 |
| [src/connector_runtime/integration.rs](../src/connector_runtime/integration.rs) | 修改 |
| [src/connector_runtime/mod.rs](../src/connector_runtime/mod.rs) | 修改 |
| [src/mcp.rs](../src/mcp.rs) | 修改 |
| [src/mcp/resources.rs](../src/mcp/resources.rs) | 修改 |
| [src/mcp_gateway.rs](../src/mcp_gateway.rs) | 修改 |
| [src/mcp_tests.rs](../src/mcp_tests.rs) | 修改 |
| [src/mcp_tests/host_goal_transport.rs](../src/mcp_tests/host_goal_transport.rs) | 修改 |
| [src/mcp_tests/model_surface.rs](../src/mcp_tests/model_surface.rs) | 修改 |
| [src/mcp_tests/plugin_tools.rs](../src/mcp_tests/plugin_tools.rs) | 修改 |
| [src/plugin_gateway.rs](../src/plugin_gateway.rs) | 修改 |
| [src/runtime_console_http.rs](../src/runtime_console_http.rs) | 修改 |
| [src/runtime_http.rs](../src/runtime_http.rs) | 修改 |
| [src/runtime_http/projects.rs](../src/runtime_http/projects.rs) | 修改 |
| [src/runtime_http_tests.rs](../src/runtime_http_tests.rs) | 修改 |
| [src/ssh_resource_gateway.rs](../src/ssh_resource_gateway.rs) | 修改 |
| [src/tool_runtime/agent_task.rs](../src/tool_runtime/agent_task.rs) | 修改 |
| [src/tool_runtime/cargo.rs](../src/tool_runtime/cargo.rs) | 修改 |
| [src/tool_runtime/checkpoint.rs](../src/tool_runtime/checkpoint.rs) | 修改 |
| [src/tool_runtime/coding_agent.rs](../src/tool_runtime/coding_agent.rs) | 修改 |
| [src/tool_runtime/coding_task.rs](../src/tool_runtime/coding_task.rs) | 修改 |
| [src/tool_runtime/communication.rs](../src/tool_runtime/communication.rs) | 修改 |
| [src/tool_runtime/computer_tools.rs](../src/tool_runtime/computer_tools.rs) | 修改 |
| [src/tool_runtime/context_projection.rs](../src/tool_runtime/context_projection.rs) | 修改 |
| `src/tool_runtime/dispatch.rs` | 迁入 dispatch/ |
| [src/tool_runtime/dispatch/agent_task.rs](../src/tool_runtime/dispatch/agent_task.rs) | 新增 |
| [src/tool_runtime/dispatch/coding_agent.rs](../src/tool_runtime/dispatch/coding_agent.rs) | 新增 |
| [src/tool_runtime/dispatch/communication.rs](../src/tool_runtime/dispatch/communication.rs) | 新增 |
| [src/tool_runtime/dispatch/goal.rs](../src/tool_runtime/dispatch/goal.rs) | 新增 |
| [src/tool_runtime/dispatch/memory.rs](../src/tool_runtime/dispatch/memory.rs) | 新增 |
| [src/tool_runtime/dispatch/mod.rs](../src/tool_runtime/dispatch/mod.rs) | 新增 |
| [src/tool_runtime/dispatch/pipeline.rs](../src/tool_runtime/dispatch/pipeline.rs) | 新增 |
| [src/tool_runtime/dispatch/projection.rs](../src/tool_runtime/dispatch/projection.rs) | 新增 |
| [src/tool_runtime/dispatch/skill.rs](../src/tool_runtime/dispatch/skill.rs) | 新增 |
| [src/tool_runtime/documents.rs](../src/tool_runtime/documents.rs) | 修改 |
| [src/tool_runtime/files/artifacts.rs](../src/tool_runtime/files/artifacts.rs) | 修改 |
| [src/tool_runtime/files/inspection.rs](../src/tool_runtime/files/inspection.rs) | 修改 |
| [src/tool_runtime/files/mutations.rs](../src/tool_runtime/files/mutations.rs) | 修改 |
| [src/tool_runtime/files/search.rs](../src/tool_runtime/files/search.rs) | 修改 |
| [src/tool_runtime/git.rs](../src/tool_runtime/git.rs) | 修改 |
| [src/tool_runtime/git_review.rs](../src/tool_runtime/git_review.rs) | 修改 |
| [src/tool_runtime/goal.rs](../src/tool_runtime/goal.rs) | 修改 |
| [src/tool_runtime/handoff.rs](../src/tool_runtime/handoff.rs) | 修改 |
| [src/tool_runtime/host_goal.rs](../src/tool_runtime/host_goal.rs) | 修改 |
| [src/tool_runtime/host_goal_project_files.rs](../src/tool_runtime/host_goal_project_files.rs) | 修改 |
| [src/tool_runtime/host_goal_review.rs](../src/tool_runtime/host_goal_review.rs) | 修改 |
| [src/tool_runtime/hygiene.rs](../src/tool_runtime/hygiene.rs) | 修改 |
| [src/tool_runtime/jobs.rs](../src/tool_runtime/jobs.rs) | 修改 |
| [src/tool_runtime/kernel.rs](../src/tool_runtime/kernel.rs) | 修改 |
| [src/tool_runtime/lsp_tools.rs](../src/tool_runtime/lsp_tools.rs) | 修改 |
| [src/tool_runtime/memory.rs](../src/tool_runtime/memory.rs) | 修改 |
| [src/tool_runtime/mod.rs](../src/tool_runtime/mod.rs) | 修改 |
| [src/tool_runtime/patch.rs](../src/tool_runtime/patch.rs) | 修改 |
| [src/tool_runtime/process.rs](../src/tool_runtime/process.rs) | 修改 |
| [src/tool_runtime/project_resolution.rs](../src/tool_runtime/project_resolution.rs) | 修改 |
| [src/tool_runtime/project_tools.rs](../src/tool_runtime/project_tools.rs) | 修改 |
| [src/tool_runtime/projects.rs](../src/tool_runtime/projects.rs) | 修改 |
| [src/tool_runtime/read_files.rs](../src/tool_runtime/read_files.rs) | 修改 |
| [src/tool_runtime/runner_authorization.rs](../src/tool_runtime/runner_authorization.rs) | 修改 |
| [src/tool_runtime/runtime.rs](../src/tool_runtime/runtime.rs) | 修改 |
| [src/tool_runtime/runtime_info.rs](../src/tool_runtime/runtime_info.rs) | 修改 |
| [src/tool_runtime/script.rs](../src/tool_runtime/script.rs) | 修改 |
| [src/tool_runtime/search_project_texts.rs](../src/tool_runtime/search_project_texts.rs) | 修改 |
| [src/tool_runtime/semantic_navigation.rs](../src/tool_runtime/semantic_navigation.rs) | 修改 |
| [src/tool_runtime/services.rs](../src/tool_runtime/services.rs) | 新增 |
| [src/tool_runtime/session_shell.rs](../src/tool_runtime/session_shell.rs) | 修改 |
| [src/tool_runtime/session_tools.rs](../src/tool_runtime/session_tools.rs) | 修改 |
| [src/tool_runtime/shell.rs](../src/tool_runtime/shell.rs) | 修改 |
| [src/tool_runtime/specialized.rs](../src/tool_runtime/specialized.rs) | 修改 |
| [src/tool_runtime/tests/agent_tasks.rs](../src/tool_runtime/tests/agent_tasks.rs) | 修改 |
| [src/tool_runtime/tests/dispatch.rs](../src/tool_runtime/tests/dispatch.rs) | 修改 |
| [src/tool_runtime/tests/files.rs](../src/tool_runtime/tests/files.rs) | 修改 |
| [src/tool_runtime/tests/goals.rs](../src/tool_runtime/tests/goals.rs) | 修改 |
| [src/tool_runtime/tests/host_goals.rs](../src/tool_runtime/tests/host_goals.rs) | 修改 |
| [src/tool_runtime/tests/memory.rs](../src/tool_runtime/tests/memory.rs) | 修改 |
| [src/tool_runtime/tests/metadata.rs](../src/tool_runtime/tests/metadata.rs) | 修改 |
| [src/tool_runtime/tests/mod.rs](../src/tool_runtime/tests/mod.rs) | 修改 |
| [src/tool_runtime/tests/reconnect.rs](../src/tool_runtime/tests/reconnect.rs) | 修改 |
| [src/tool_runtime/tests/sessions_resolver.rs](../src/tool_runtime/tests/sessions_resolver.rs) | 修改 |
| [src/tool_runtime/tests/support/runtime.rs](../src/tool_runtime/tests/support/runtime.rs) | 修改 |
| [src/tool_runtime/tests/targeted_inventory.rs](../src/tool_runtime/tests/targeted_inventory.rs) | 修改 |
| [src/tool_runtime/tests/validation_handoff.rs](../src/tool_runtime/tests/validation_handoff.rs) | 修改 |
| [src/tool_runtime/tests/work_on_project.rs](../src/tool_runtime/tests/work_on_project.rs) | 修改 |
| [src/tool_runtime/validation_events.rs](../src/tool_runtime/validation_events.rs) | 修改 |

## Server 基线失败清单

前后失败集合逐项相同（29 项）：

- `mcp::tests::model_surface::adaptive_runtime_tool_manifest_exact_projection_is_sparse_and_routes_explicitly`
- `mcp::tests::model_surface::adaptive_runtime_tools_list_is_small_core_plus_gateway`
- `mcp::tests::model_surface::apply_patch_schema_reaches_full_local_and_adaptive_surfaces`
- `mcp::tests::model_surface::apply_text_edits_discriminated_schema_reaches_full_and_local_coding_mcp_surfaces`
- `mcp::tests::model_surface::mcp_tool_manifest_advertises_image_mode_without_changing_runtime_schema`
- `mcp::tests::tools::mcp_tools_list_returns_same_names_as_runtime`
- `project_entry::regular_tunnel_service::tests::authorization_file_rejects_empty_bootstrap_credentials`
- `project_entry::windows_migration_tests::legacy_runtime_acl_migration_does_not_create_absent_sqlite_sidecars`
- `project_entry::windows_migration_tests::legacy_runtime_file_acls_are_migrated_without_changing_contents`
- `project_entry::windows_migration_tests::legacy_runtime_parent_acls_are_hardened_before_future_children`
- `tool_runtime::tests::builtin_coding_workflow::builtin_coding_workflow_defaults_are_required_and_bounded`
- `tool_runtime::tests::files::office_artifact_mime_policy_accepts_matching_save_and_upload_paths`
- `tool_runtime::tests::metadata::tool_manifest_model_fields_and_hidden_start_compatibility_stay_separate`
- `tool_runtime::tests::schema::discovery::tool_discovery_groups_drive_tool_categories`
- `tool_runtime::tests::schema::flattened_args::tool_call_request_flattened_properties_have_explicit_sources`
- `tool_runtime::tests::schema::policy::required_runner_capability_matches_metadata_risk_table`
- `tool_runtime::tests::startup_brief::explicit_coding_task_resume_reuses_rules_without_repeating_content`
- `tool_runtime::tests::startup_brief::fresh_coding_task_session_loads_all_bounded_repository_rules`
- `tool_runtime::tests::startup_brief::minimal_standard_and_full_coding_workflow_diagnostics_validate_against_strict_schema`
- `tool_runtime::tests::startup_brief::repository_without_project_instructions_still_receives_builtin_workflow`
- `tool_runtime::tests::startup_brief::worst_case_startup_with_huge_repository_stays_below_hard_limit`
- `tool_runtime::tests::unified_diff::apply_unified_diff_schema_matches_flat_runtime_contract_and_old_tools_are_absent`
- `tool_runtime::tests::work_on_project::coding_workflow_standard_and_full_accept_valid_repository_overview`
- `tool_runtime::tests::work_on_project::managed_worktree_bootstrap_recovers_same_operation_and_binds_session_to_final_project`
- `tool_runtime::tests::work_on_project::work_on_project_can_omit_instruction_bodies_for_a_fresh_session`
- `tool_runtime::tests::work_on_project::work_on_project_new_task_is_lightweight_and_preserves_startup_context`
- `tool_runtime::tests::work_on_project::work_on_project_projection_is_sparse_for_defaults_and_keeps_noteworthy_state`
- `tool_runtime::tests::work_on_project::work_on_project_sizes_and_runner_request_reduction_are_stable`
- `tool_runtime::tests::work_on_project::work_on_project_without_session_id_always_creates_fresh_session`
