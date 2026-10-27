# 通用 Agent 执行与任务接管交付报告

后续简化：普通 CLI Agent 现明确走既有 `run_process` / `run_detached_process`，
不要求 ACP Provider 注册。此前新增的 `json_cli`-inside-ACP 适配及其专用测试已移除；
ACP 仅保留为可选专用路径。下面的测试数字与实现清单是原阶段历史记录，
当前调用方式以 [Agent 调用与接管](architecture/agent-takeover.md) 为准。

日期：2026-10-22。项目：`E:\wbecodex\source`。

本次完成现有 Rust 执行链路的接管扩展。核心场景已通过端到端 Mock 测试：
Agent A 写入部分成果后额度耗尽，系统保存任务和执行证据；Agent B 收到原始目标与
交接记录，确认已有文件后完成剩余工作，最终文件满足原始验收条件。
WorkBuddy 的真实 ACP 小任务通过；Antigravity 的真实推理验证未通过，详见下文。

## 1. 复用的现有模块

- `AgentTask`、`AgentTaskAttempt` 和 SQLite：保留任务身份、执行者、租约、代次、
  幂等键及旧尝试的围栏，没有另建任务库或调度器。
- `CodingAgentRun`、类型化 Runner 协议、现有权限校验：继续负责执行分发及精确观察。
- `ManagedChild`：复用进程树所有权、终止与退出确认。
- Agent、Conversation、Inbox、Delivery、Wake：保留现有职责，没有复制消息系统。
- Workflow Session 的验证与交接记录、现有 Goal 的不可变验收合同：继续独立使用。
  Run 结束并不替代总任务的独立验收。

## 2. 新增能力

- 归一化输出、工具回执、测试输出片段和模型标识可以持久恢复，不依赖失败模型再次总结。
  不持久保存内部推理文本或私有 ACP 会话 ID。
- 工具回执按操作 ID 关联，交接摘要区分完成、失败和不确定，并提供最后成功回执的序号。
  回执是 Provider 报告的执行证据，最终仍需检查实际文件及验收条件。
- `reconcile_agent_task_coding_run` 返回并保存 `handoff_brief`。相同执行主体在 Runner
  离线、Server 重建后仍可读取已保存交接信息；空的离线观察不会覆盖已有证据。
- 对已精确确认终止的失败或取消 Run，`assign_agent_task` 可显式恢复任务并更换执行者。
  下一个 Attempt 的交接指令在事务内冻结，重试不会因为后来补充日志而改变执行意图。
- 旧执行的迟到终态不能撤销已发生的重新分配；手工完成操作不能越过仍活动或不确定的 Run。
- 同一 Runner 内，活动或结果不确定的 Run 排斥相同及重叠工作区的新 Run。
- 增加可配置的一次性 JSON CLI 传输，复用同一 Run 生命周期。ACP 仍为默认协议。
  没有写死 GPT、WorkBuddy、Antigravity 或任何模型身份。
- 修复未知 ACP 扩展通知被误当成请求并取消任务的问题。未知请求仍拒绝执行；
  缺失、损坏或未知终止响应保留为不确定结果。

## 3. 源码范围

修改现有文件：

- `crates/webcodex-runner/src/webcodex_runner/coding_agent.rs`
- `crates/webcodex-runner/src/webcodex_runner/config.rs`
- `crates/webcodex-store/src/agent_task.rs`
- `crates/webcodex-store/src/agent_task_tests.rs`
- `src/tool_runtime/agent_task.rs`
- `src/tool_runtime/coding_agent.rs`
- `src/tool_runtime/tests/agent_tasks.rs`
- `crates/webcodex-tool-contracts/src/tool_definition/agent_tasks.rs`
- `crates/webcodex-tool-contracts/src/registry/output_schemas/agent_tasks.rs`
- `crates/webcodex-tool-contracts/src/registry/output_schemas/coding_agents.rs`

新增局部实现及测试：

- `crates/webcodex-runner/src/webcodex_runner/coding_agent/json_cli.rs`
- `crates/webcodex-runner/src/webcodex_runner/coding_agent/recovery.rs`
- `crates/webcodex-runner/src/webcodex_runner/coding_agent/recovery/tests.rs`
- `crates/webcodex-runner/src/webcodex_runner/coding_agent/recovery/process_tests.rs`
- `crates/webcodex-store/src/agent_task/takeover.rs`
- `crates/webcodex-store/src/agent_task_tests/takeover_tests.rs`
- `src/tool_runtime/tests/agent_tasks/takeover.rs`

同步更新 ACP 和 Durable Agent 架构文档，新增
[`architecture/agent-takeover.md`](architecture/agent-takeover.md)，其中包含调用流程、配置示例、
存储边界和可复现的测试命令。

## 4. 部分成果保存与接管

没有清理、回滚或覆盖失败 Agent 的项目文件。Run 保存规范化工作区路径、执行身份、
持久状态和有界事件；SQLite 保存原始指令、各次尝试及交接摘要。

协调者的流程为：观察原 Run → reconcile 获取交接信息 → 检查已有成果及不确定操作 →
显式 assign → 创建新 Attempt → 由协调者手工继续，或交给配置的 Provider → 按原始验收条件验证。
下一位执行者无需用户重复描述任务或人工复制消息。

`lost/outcome_unknown` 不会被自动重新执行。没有可用 Provider 或网页协调者当前不可执行时，
保存现状等待后续显式激活，不假装可以自动唤醒 ChatGPT 网页。

## 5. 预算、超时与并发

- 每个 AgentTask 最多预留 8 个 CodingAgentRun，跨重分配和重启累计；精确重放同一绑定不重复占用。
- 继续执行 `acp.max_concurrent_runs` 的原子并发限制，并将正在清理的工作线程计入占用。
- ACP 终态发布前确认所拥有的进程树已经退出。超时不等于停止，也不等于安全重试。
- JSON CLI 超时或取消后由 WebCodex 终止进程树；缺少有效结果时仍记为不确定。
- 对结构化错误区分 429 限流、额度耗尽、认证、权限和服务不可用；无法确定的错误保留通用分类。
- 不自动重试、购买额度或绕过 Provider 限制。

这里限制的是外部 Run 调用数及运行时间，不能核算第三方 CLI 内部每次模型调用的账单。
需要内部轮次或费用上限时，应按已验证 CLI 能力配置相应参数。

## 6. 数据迁移和权限

SQLite 仅增加两列：`wc_agent_task_coding_runs.evidence_json` 和
`wc_agent_task_attempts.continuation_instruction`；旧行保持原样。
Runner 私有记录新增可缺省的工作区及观察版本信息，新版本可读旧记录。
旧版本严格解码器可能拒绝新记录，降级前需要单独审查数据和备份方案。

证据读取仍要求任务所有权、执行权限及相同执行主体；任务关联项目并不赋予项目操作权限。
通用审计不记录交接正文、原始指令或 Attempt fence。损坏的执行记录会保留并阻止新执行，
不会忽略它们后放行接管。

## 7. 验证结果

本地定向测试：任务存储 29 项、运行时/端到端 10 项、Runner 20 项、CLI 配置 1 项、
隔离进程矩阵 2 项、工具合同 5 项，共 67 项，最终均通过。
覆盖预算、同工作区冲突、不同工作区并发上限、429、额度耗尽、未知响应、进程异常、
超时/取消、部分文件保留、避免重复分发、重启、离线交接和接管者不可用。

真实 CLI：

- **WorkBuddy：通过。** 使用本机 `codebuddy.exe --acp`，在临时目录执行禁止工具和文件操作的小任务，
  收到预期文本，确认进程退出。过程中发现并修复 ACP 扩展通知兼容问题。
- **Antigravity：未通过真实推理验证。** 本机 `agy.exe` 的帮助确认支持非交互 JSON，
  但本轮小任务没有返回结果。独立探测的自带超时未生效；受 WebCodex 管理的探测在 45 秒截止时
  终止进程树，保存 `coding_agent_cli_interrupted / lost`，未重试。未确认其阻塞原因，
  没有将其归因为额度或认证问题，也没有把配置示例当作已验证的输出协议。

Mock 进程使用 Python 仅作测试夹具，正式实现全部为 Rust。未运行整个工作区测试、
Unix 专用进程测试或发布构建；没有安装新工具链或更新依赖。
编译中已有的 Windows 未使用项警告未作无关修改。
最终定向格式检查、Diff 空白检查通过，Git 没有未解决的合并冲突。

## 8. 当前限制与交付状态

- 工作区互斥覆盖同一 Runner 管理的 CodingAgentRun，不是阻止独立终端、其他 Runner 存储、
  人工操作或远端副作用的操作系统沙箱。协调者仍需协调这些外部写入。
- 原始事件为有界保留，超出时明确标记；不存在无限日志或完整内部推理记录。
- 保留最多 128 个 Run 后停止新准入，等待操作者归档已审阅历史；本次没有新增自动删除或归档接口。
- 无法从现有证据确认的操作继续等待核对，不提供跳过核对的强制自动接管。
- AgentTask 的执行终态不取代总体目标验收；协调者或现有 Goal 验收合同负责最后验证。
- Antigravity 的实际成功响应结构与登录/服务状态仍需在其恢复响应后确认。

开始时分支为 `feat/mycodex-tools`，HEAD 为
`15e4287cda7f5e673dc449c34ed2838f9bcad4ca`。保留了已有未提交工作。
原文件、初始状态和验证日志位于 `E:\wbecodex\backups\agent-takeover-20261022`。
备份流程有一处遗漏：`src/tool_runtime/coding_agent.rs` 未在首次编辑前复制。
该文件仅新增执行主体核对辅助函数；已另存修改后文件，并通过移除这个精确新增函数重建备份，
但不将重建版本当作已验证的修改前逐字节副本。详情记录在备份目录的 `BACKUP_NOTES.txt`。
本次只交付源码和测试，没有提交、推送、发布、部署或重启服务。
