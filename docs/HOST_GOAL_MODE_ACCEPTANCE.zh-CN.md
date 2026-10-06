# ChatGPT 网页目标模式：历史源码验收记录

> 历史记录：以下测试、计数和构建指纹仅对应记录当日，不是当前操作手册或当前安装状态。2026-09-29 源码新 epoch 默认值为 **600,000/800,000**，已有 epoch 保留原策略。当前流程见[Host Goal 操作说明](HOST_GOAL_MODE.zh-CN.md)。

日期：2026-09-22。工作目录：`E:\wbecodex\source`。基线：`feat/mycodex-tools`，`15e4287cda7f5e673dc449c34ed2838f9bcad4ca`。

本记录针对 2026-09-22 当时工作树中的 Host Goal 功能。工作树原有 ACP Goal、文档和绘图工具等未提交改动，均予以保留；本次没有提交或发布。

## 验收范围

- 在 ChatGPT 网页中输入“开启目标模式：……”后，由模型调用工具创建本地目标。
- 讨论后通过 `goal_plan_save` 发布同一版本的 `plan.json` 和 `plan.md`，返回实际路径、版本及哈希。
- 保存计划后保持 `awaiting_confirmation`。用户阅读并在后续网页消息中明确通知开始，模型才能确认该版本并认领节点。
- 计划包含最终交付、验收标准和方法、串行节点、State 保存与恢复规则。节点和最终验收均不能由普通检查点直接绕过。
- State、计划版本、Attempt、检查点、操作回执、当前目标和上下文计量保存在本地 SQLite。更换对话后可恢复同一任务。
- 当时验证了该版本的两级可见工具文本提醒策略；该历史默认值已经被替代。当前源码新 epoch 使用 600,000 预警、800,000 进入 draining，见现行操作说明。

## 已完成的验证

| 验证 | 结果 | 证据 |
| --- | --- | --- |
| Store 的 Host Goal 与上下文测试 | 36 项通过 | `target/host-goal-store-final.log` |
| 前端目标面板及构建测试 | 19 项通过 | `node --test test/goal_panel.test.mjs test/build.test.mjs` |
| 前端类型检查、构建产物一致性检查 | 通过 | `npm run typecheck`、`npm run check:dist` |
| 工具契约 Goal 测试 | 13 项通过 | `cargo test --profile dogfood -p webcodex-tool-contracts goal` |
| ToolCall 解析与审计 Goal 契约 | 4 项通过 | `cargo test --profile dogfood -p webcodex-tool-runtime-contracts goal` |
| Host Goal 运行时、MCP / HTTP / OpenAPI、文件及恢复测试 | 52 项通过 | `target/host-goal-final-tests.log` |
| 原 ACP Goal 运行时 | 20 项通过 | `target/host-goal-regression-tool_runtime__tests__goals__.log` |
| 原 Goal 权限、输出和公共契约 | 4 项通过 | `target/host-goal-regression-tool_runtime__tests__goal_contracts__.log` |
| MCP Plugin 与 SSH 调用 | 23 + 8 项通过 | 对应 `target/host-goal-regression-mcp__tests__*.log` |
| Plugin 与 SSH 网关 | 6 + 4 项通过 | 对应 `target/host-goal-regression-*gateway__tests__.log` |
| 内置工作流、非规范工作流拒绝、OpenAPI 参数展开 | 4 + 1 + 1 项通过 | 对应 `target/host-goal-regression-*.log` |
| 启动指引、严格 Schema、恢复与长度上限 | 14 项通过 | `target/host-goal-startup-final.log` |
| 服务端生产入口的 dogfood 构建 | 通过 | `target/host-goal-server-build.log` |
| Host Goal 核心新增文件格式检查 | 通过 | 定向 `rustfmt --check --edition 2021 --config skip_children=true` |
| 工作树差异空白检查 | 通过 | `git -c core.autocrlf=false diff --check` |

以上共 209 项定向测试通过。最后一次测试支持代码与启动指引更新后，使用新生成的测试程序重新执行了 Host Goal、旧 ACP Goal、公共契约、Plugin / SSH 及工作流相关回归。

构建命令为 `cargo build --profile dogfood -p webcodex --bin webcodex-server`。产物：`E:\wbecodex\source\target\dogfood\webcodex-server.exe`，77,404,160 字节，SHA-256：`B61926B3B44F4F27FE43450D237C4F46FD875824A9A7475F7CC593B1E1807EBD`。构建成功，仍有 Windows 平台未使用代码等编译警告，未为清除这些警告扩大修改范围。

本轮发现并修复了两类验证问题：测试专用 Schema 校验器遗漏了字典 `additionalProperties` 的值校验，补齐后状态字典负例按预期被拒绝；新增通用启动指引一度包含宿主专用名称，现改为通用“当前对话”表述，网页操作说明仍保留 ChatGPT 使用场景。没有放宽运行时权限、状态校验或测试断言。

## 关键回归场景

验证覆盖计划未保存、未确认和版本不匹配时的执行拒绝；数据库重新打开后的恢复；新对话先读取当前节点必要信息；状态局部更新和单条事实删除；大量状态和计划的有界、可完整分页读取；过期版本、Attempt 和操作身份隔离；用户所有权及项目隔离。

执行恢复验证使用本地测试数据库和 Runner 协议夹具，核对原 Job / CodingAgentRun，避免未知结果触发替代任务。人工核对记录不能成为机器验收证据，仍活跃或有可观察执行的操作不能被人工覆盖。取消与暂停恢复只观察已存在的执行，不派发遗留检查命令。

验收验证包括二进制文件指纹、命令输入变化、同批检查期间文件变化，以及两个节点按计划将同一文件从草稿更新为最终成果。节点级 `verification_paths` 可以覆盖计划默认值，早期节点不依赖未来产物；末节点重新检查最终标准。

## 实际能力边界

这些是源码、公开 MCP / HTTP 入口和本地协议测试，不是对当前安装的 ChatGPT 网页连接进行上线验收。本次没有 GUI 自动化、替换安装目录、部署或重启现有 Server / Runner。当前安装需要更新后才会暴露新工具。

确认文本由 ChatGPT 模型转达，绑定已保存计划的版本和哈希，不能当作独立认证的人类签名。模型必须等待用户阅读计划后的明确开始消息。

计量仅覆盖 WebCodex 可见工具文本，不能读取 ChatGPT 的真实上下文容量或占用率，也无法在没有工具调用的纯聊天期间持续检测。可靠宿主身份优先；无可靠身份时，显式 `context_epoch` 仅约束携带该标记的调用，不能推断未标记请求属于哪段对话。

采用串行节点；不会自动操作网页或启动新对话。已验收结果是当时的证据，读取完成目标时会检查声明的产物指纹，不能宣称后台持续监控所有外部变化。

操作说明见 [HOST_GOAL_MODE.zh-CN.md](HOST_GOAL_MODE.zh-CN.md)。

## 复核与交付状态

已完成独立子代理对实现、权限、恢复和有界读取的反方复核。外部辅助模型调用及测试缓存清理被自动审批以 `blocked by policy` 拒绝，没有重试绕过；`frontend/.cache/goal-ui` 缓存保留。

最后检查无冲突文件、无差异空白错误，也无本次遗留的 Cargo / Rust 编译进程。安装目录中的 Server / Runner 仍为原进程，未替换、重启或部署。代码保持在当前未提交工作树中，没有 commit、push 或发布操作。
