# 目标模式验收记录

记录日期：2026-09-21。本地验收通过，原始七条要求均有对应证据。验证基于 `feat/mycodex-tools` 分支、基线 HEAD `15e4287` 的当前工作树；保留原有未提交变更，未提交、推送、发布或替换现有安装。

## 已有证据

| 验证层 | 当前结果 | 证据范围 |
|---|---|---|
| 目标运行时与公开入口 | 24 通过，0 失败 | `cargo test --profile dogfood -p webcodex --lib goal`；默认线程栈，未设置 `RUST_MIN_STACK`；真实控制器和 Store，确定性 Runner fixture；包含传输告警、进度说明和最终审稿结果的区分 |
| 持久 Store | 11 通过 | 所有者隔离、幂等、CAS、不可变验收条件、重启读取、项目独占与释放 |
| Windows 路径回归 | 2 通过 | 持久存储长 Unicode 路径的首次保存、原子覆盖、重新打开和扫描 |
| ACP 消息阶段 | 3 通过 | 明确区分 `commentary` 和 `final_answer`；未知或缺失阶段、reasoning 分类保持原语义 |
| 工具契约 | 100 + 75 通过；最终 Goal schema 3 项再次通过 | 覆盖注册、输入/输出 schema、公开入口及既有契约回归；不表示 175 项均为新增目标模式用例 |
| 前端状态与构建 | 目标面板 10 通过，构建/监听 3 通过 | 创建验收参数、幂等重试、旧响应丢弃、版本防回退、分页、交付提示和取消待核对状态；typecheck 与构建通过 |
| 隔离进程端到端 | 7 个脚本断言通过，实际执行 2 轮 | 真实 Server + Runner + ACP stdio 子进程、原生命令检查、独立审稿调用；ACP 模型由确定性 fixture 提供 |
| 真实 Codex 模型 | 5 个脚本断言通过，实际执行 1 轮 | 真实执行模型生成文件，文件存在、内容、原生命令和独立模型审稿四项条件全部通过；通过后再次修改文件会取消交付就绪 |

运行时的本地证据为 [runtime-tests.txt](../../.local/reports/goal-mode/runtime-tests.txt)。最终两轮隔离进程实验的报告为 [fixture E2E](../../.local/reports/goal-mode/goal-e2e-20260921T102112Z-7614ed5e.json)，真实模型报告为 [live E2E](../../.local/reports/goal-mode/goal-e2e-20260921T102556Z-1f11feb7.json)。这些报告保存在仓库外的本地 `.local/reports`，不随源码分发；跨机器复核应重新运行并记录报告。

该 E2E 报告包含 `result_exists`、`result_ready`、`result_command`、`result_review` 四项验收条件，确认首轮不合格、第二轮通过。清理记录中 `cleanup.tree_empty`、Server/Runner 的 `exited` 及 `temporary_profile_removed` 均为 `true`。报告中的“7 个断言”与下表原始“7 条验收要求”不是一一对应关系；取消、时间预算和重启等分支另由专项用例覆盖。

## 原始七条验收要求

| 编号 | 要求 | 当前可引用证据 | 证据边界 |
|---|---|---|---|
| 1 | 首轮不合格自动进入第二轮 | 两轮隔离 E2E；`goals_background_repairs_and_completes_without_read_or_continue`；命令失败后下一轮复查用例 | 确定性 ACP fixture 验证了真实跨进程循环，不能代替真实模型稳定性统计 |
| 2 | 模型声称完成但检查失败，目标仍未完成 | E2E 第一轮主动输出完成声明，但内容检查失败；`goals_model_claim_does_not_override_failed_acceptance_or_round_budget` | 不把 Provider 的 `end_turn` 或自述当作目标通过 |
| 3 | 所有必需项通过后停止并生成交付记录 | E2E 四项条件通过、保存证据并返回 `delivery_ready: true`；Store 完成证据约束用例 | 交付记录是持久目标、轮次检查与产物哈希；不承诺自动向外部系统发送或发布 |
| 4 | 检查器异常、缺项或无证据不假成功 | 文件检查异常、无效/缺失审稿证据、丢失命令 Job 的专项用例；Store 缺失/失败/过期证据拒绝用例 | 正常 E2E 不故意覆盖全部故障注入；异常分支由专项测试证明 |
| 5 | 取消、预算耗尽或无进展时停止 | 取消仍在运行/结果未知时保留项目占位用例；总时限、轮数、无进展用例；前端取消标志测试 | `cancel_requested` 是请求，不等于执行已停止；确认真实 Run/Job 终止后才释放项目 |
| 6 | 重启或重复事件不重复派发 | E2E 重复启动键返回相同目标；控制器重开核对既有 Run、第二个命令恢复及 Store CAS/重启用例 | 此次真实进程 E2E 未重启 Server；重启分支证据来自真实 Store 与确定性 Runner 测试 |
| 7 | 检查后产物改变，旧结论不可交付 | E2E 修改 `result.txt` 后返回 `delivery_freshness: changed` 且 `delivery_ready: false`；恢复时快照改变不沿用旧检查的专项用例 | 哈希核验反映最近一次读取时的文件；之后再改文件仍需再次核验 |

用例实现见 [运行时测试](../src/tool_runtime/tests/goals.rs)、[公开入口测试](../src/tool_runtime/tests/goal_contracts.rs)、[Store 测试](../crates/webcodex-store/src/goal_tests.rs) 和 [前端状态测试](../frontend/test/goal_panel.test.mjs)。

## 隔离与清理复查

本轮没有使用 GUI 自动化。验收脚本仅创建自有临时 Server、Runner、项目注册、数据目录、端口和凭据，不连接已有 WebCodex 服务，不修改生产 `.local/data` 或服务配置。真实 Provider 环境仅通过显式 `--env-pass` 从调用进程捕获，映射给 ACP 子进程；服务器和 Runner 的状态目录仍位于临时根。

Windows 子进程采用 `CREATE_NO_WINDOW | CREATE_SUSPENDED` 创建，先加入私有、关闭时终止成员的 Job Object，再恢复执行，因此不会在加入 Job 前启动未受管理的后代。清理在 `finally` 中终止该 Job，查询活动成员归零，再等待自有进程退出；失败不会记录为清理成功。临时目录由 `TemporaryDirectory` 持有并删除，报告目录不作为清理目标。该脚本目前限定 Windows，其他系统拒绝运行。

上述结论针对脚本的隔离边界及已保存的实验清理报告。复用真实 Codex 登录并不代表复制或另建了 Codex 账户隔离环境；本报告不声称生产部署、GUI 验收或全部真实模型场景完成。

## 真实模型最终记录

- 适配器：`@agentclientprotocol/codex-acp 1.9.0`，实际捆绑 `@openai/codex 0.153.4`；复用已有 ChatGPT 登录。
- 本机 Codex 配置中的模型为 `gpt-6-astra`；本次 Goal 未覆盖模型，只显式设置 `reasoning_effort: low`，未改变用户或产品的默认配置。
- 2026-09-21 10:21:28–10:25:56 UTC，执行与独立审稿均成功，一轮达成目标；四项条件为 `result_exists`、`result_ready`、`result_command`、`result_review`。
- live 与 fixture 的 `cleanup.tree_empty`、Server/Runner 退出和 `temporary_profile_removed` 均为 `true`。
- 一轮真实模型成功验证了模型接线；自动修复进入第二轮另由确定性跨进程实验验证，不将二者混称为真实模型多轮稳定性统计。

## 复核中修复的问题和边界

已通过当前 Claude 配置完成只读反方审查，并结合实际实验修复：恢复时旧检查与新文件错误绑定、旧检查 Job 未被调和、`job:detach` 权限缺口、固定读取前 2000 行造成的误判、取消尚未确认却释放项目占位，以及 Windows ACP 长路径持久化失败。

真实模型实验还发现进度说明和网络回退提示会与最终答案共用消息流。Runner 现在保留明确的消息阶段，目标轮次持久化阶段识别状态；使用有阶段标记的执行器时，只将 `final_answer` 交给严格 JSON 验收，旧的纯 JSON 执行器仍兼容。没有通过截取任意 JSON 片段或放宽验收条件来获得通过。

执行是否已经发生仍无法判明时，目标保留暂停和项目占位，不盲目重发。产物哈希针对声明的文件及文件类验收条件，不是整个文件系统的写锁。命令条件验证指定命令的确定终态和退出码；要求测试数量、覆盖范围等时，应将这些要求写入验收命令。

## Windows 复现实验

使用当前工作区构建的 Server/Runner 二进制；以下脚本默认使用确定性 ACP fixture，`--review` 增加独立审稿调用：

```powershell
python scripts/e2e_goal_mode.py --server-bin target/dogfood/webcodex-server.exe --runner-bin target/dogfood/webcodex-runner.exe --review --output-dir ../.local/reports/goal-mode
```

真实模型需显式提供 `--provider-executable`、逐项 `--provider-arg` 及确有需要的 `--env-pass`，并可为首次短时 smoke 设置 `--reasoning-effort low`。该参数只覆盖本次请求，并由脚本开放对应 Provider 配置项，不修改产品默认值。复现实验入口：[scripts/e2e_goal_mode.py](../scripts/e2e_goal_mode.py)（Windows 限定）。
