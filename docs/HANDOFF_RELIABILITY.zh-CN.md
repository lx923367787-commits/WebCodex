# Handoff 可靠性修改与验证（2026-10-23 至 2026-10-24）

本轮实现持久化有限重试、一次性准备授权、就绪证明和同 Goal 执行所有者切换，保留现有代码的其他改动。启动工具使用窄授权在首次真实 MCP 调用时绑定新窗口，无需预知网页 URL 与匿名会话标识的对应关系。**真实网页端到端接管尚未实测，不宣称已上线。** 未推送、部署、重启服务或操作真实网页。

handoff 保持同一逻辑 Agent 身份，只替换 run/网页对话，不创建替代 Goal。按用户后续确认，自己的 Memory 和已授权会议厅权限保留；业务写入限本次 Goal，不能读取其他 Agent 的私域。

## 当前实现

1. `goal_checkpoint(patch.action=handoff)` 原子保存同 Goal Checkpoint 和 handoff 记录，冻结源业务；此时不退休。只接受可信上下文，不把自报 epoch 当身份。
2. 现有 Python 调度器通过既有 BootstrapOnly 管理路由的 `handoff_*` action 发现等待记录，保留原 120 秒启动延迟。服务端持久分配唯一尝试 UUID/截止时间和同 Agent 的 pending run，再实际调用既有 `edge_adapter.launch`。启动工具为当前尝试领取一次性准备授权，单独在内存中加入发往目标新聊天的消息。同一 UUID 的传输只认领一次；普通管理回包不含 run 凭据、准备授权或任务正文。
3. 新对话通过可信 OAuth 宿主连接，在首次 `prepare` 携带 `launch_claim`。Server 同事务核验授权、OAuth principal、Agent/project/source run、Goal/Checkpoint/尝试及原 deadline，只将全新窗口绑定为 Pending。然后实际读取私域 broker 中不可变计划、Checkpoint 和交接包，核验 SHA 后才记为 prepared。候选不能读普通 Memory、会议、其他 Goal 或推进业务；普通 UUID 或自报身份不能兑换。
4. `commit` 要求准确尝试、未过期候选、服务端准备证明和包 SHA。现有 Job/Run 取消及观测必须证明旧链终止；在事务中撤销旧节点 fence、切换 owner、退休源。候选再次 `status` 确认自己的执行权。
5. 状态响应丢失先查询实际状态。迟到失败、旧 abort、重复确认都按尝试 CAS，不得终止下一尝试或再次切权。异步停止只覆盖不可变源 Checkpoint 的 fence 范围。

初次启动加最多三次重试，**总计四次**。每次接管默认 **300 秒**；`WEBCODEX_HANDOFF_TAKEOVER_TIMEOUT_SECS` 可配置为 1–3600 秒，首轮确定后持久保存。未改变原 CLI 超时、600K/800K 阈值。耗尽后 `handoff=failed`、Goal=`blocked`、源上下文=`stopped`；没有第五次自动启动。最后 Checkpoint、已有成果和失败原因保留。未新增失败后一键恢复入口；失败状态不得由模型自行清除。通用 begin/activate/recover 在 native 交接未提交或失败时拒绝，防止第一次尝试前尚无 scope 的源 run 被提前替换或恢复成不受限制的 run；已成功提交的同任务可信恢复仍保留。

## 强制校验位置

- Store `context_goal_gate`：同一 Goal 等待/失败时禁止业务 mutation，切换后仅当前可信 owner；缺失上下文不能绕过。
- Store handoff 事务：尝试计数、截止时间、候选身份、准备摘要、owner/fence、源退休在同一 SQLite 事务校验/提交。撤销候选保留 tombstone。
- 原 host-window Kernel `host_goal_takeover_access` 保留严格执行门。native ModelAgent 入口使用 `agent_handoff_access`、Store `agent_task_scope`、`with_agent_run_for_path/for_goal`，将 run 的不可清除 `task_goal_id`、当前 owner/节点 fence 校验和真实文件操作放在同一事务内。
- native successor 可以读写本 Goal 独占的 `workspace/<任务目录>`；写入还需本 run 新的完整 node-read 和 claim。自己的 Memory 与已授权会议权限保留，旧 run 在实际 effect 事务中失效。跨 Goal、根目录扫描、链接/硬链接、任意执行通道被拒。目录重叠按不区分 ASCII 大小写检查。
- Generic activate、Exit、bootstrap 或重启不能清除 task scope。可信 Recover 也继承 scope，更新同 Goal 执行者并要求重新 claim；失败/pending handoff 不会借此清零重试状态。
- 现有 `ActiveOperationLease` / `guarded_goal_handoff`：在途请求和交接文件发布也属于旧执行链。发布在注册 lease 后复核源状态，持有至 Runner 写入/校验结束，切换不能跨越仍在进行的旧写入。
- `quiesce_host_goal_handoff`：仅取消原源 fence 下有可信回执的 Job/原生 Run，取消后再次观测；accepted、stop_requested、未知结果均不作为全链退出证明。
- native `agent_goal_handoff` 使用同事务 composed commit 撤销旧 run、激活同 Agent pending run。候选的窗口映射、当前尝试、deadline、准备 SHA 和 owner 一起校验；原子写入前不能调用普通业务 broker。native 通道未适配 CLI，任何历史未决执行记录都拒绝交接，不把它当作已经停止。
- issue/claim、pending 读取、自检准备和 commit 的期限检查包含数据库排队时间；切换前及提交前再次核验，过期则整笔回滚。已经提交的准确回执仍可在原 deadline 之后查询和幂等确认。

## 授权边界与未验证部分

新的窄 MCP 宿主入口复用已经验证的 OAuth client/subject，并要求 Bootstrap 显式登记源窗口。它独立于文件导入 allowlist，默认无授权；不回退用户/父 Agent/最近 Goal。匹配到 active run 后构造只含该 run 的 ModelAgent 权限，不继承 OAuth user/admin 字段。私域原始请求、结果和模型自填名称不会进入账号全局 trace/action ledger。

已授权宿主的私域工具目录保持稳定，避免客户端缓存初始目录后在获权时找不到文件/Goal 工具。目录仅有通用接口描述，不含私域数据，也不授调用权；未知、Pending 或已撤销窗口的业务调用仍由服务端逐次拒绝。

本轮宿主适配仅接受 managed-user/shared-key OAuth 主体；project-share OAuth 保留其原项目授权边界，不得借这个入口变成其他项目的 Agent。

管理路由提供 `bind_host_window`、`bind_handoff_window`、`revoke_host_principal`，以及 `handoff_list/status/begin_attempt/fail_attempt/abort/launch_claim`。源窗口初次绑定要求控制端明确确认该 OAuth 客户端独占生成外层 `openai/session` metadata。候选可兑换服务端签发的一次性准备授权；原有可信控制端精确预登记仍可用。attempt UUID 仅作关联，不充当授权。

准备授权由 256-bit 随机秘密构成，只存域隔离哈希及允许的 principal，绑定当前 pending run；不能用作普通 bearer、业务执行或另一尝试。重复发行拒绝，不旋转凭证；发行响应丢失先查状态，未成功则等待原有限 deadline。兑换仅接受新窗口，跨 principal 的旧窗口、源窗口、其他 Agent、旧候选和撤销 tombstone 全拒。同窗可幂等读回原兑换结果，其他窗口不能重用；超时、abort、重试均使旧授权失效。

本地持久 request、`_ACTIVE_REQUEST`、回执、文件名、日志、argv/env 和截图不携带 raw grant；发送时以独立局部消息传给目标新聊天，不复制旧 run token 或替换全局 connector。**目标 ChatGPT 会收到这段限权授权消息，宿主可能保存它**；一次性、短期限和精确绑定限制其效力，不能声称秘密在宿主端从不留存。

官方将 `openai/session` 定义为用于关联同一会话工具调用的匿名标识，没有保证它与 `/c/<id>` 等价。本实现不猜测该关系，直接用已认证宿主提供的实际窗口加一次性授权完成绑定。[OpenAI Reference](https://developers.openai.com/plugins/reference)

Python 控制入口已适配私有 Agent Goal，必须显式配置单独 Bootstrap 控制凭据、owner/project，不再 fallback 到 Desktop user token。普通 CLI/插件仍缺少已验证的 OS 级任务隔离，相关入口拒绝执行。实际私域文件接续可测试，不等于任意项目工作树/CLI 已可用。其他 Agent 的并行私域适配工作不计入本轮完成证据。

自动启动适用于已经绑定可信逻辑 Agent 的原 Goal。旧的、没有可验证 Agent 归属的记录保留为只读，不凭同账号、目录名或旧 UUID 自动迁移。失败后的 Goal、最后 Checkpoint、成果和原因保留；首次 failed 尚无受支持的一键恢复入口，通用 Recover 不会重置它。失败通知通过状态回包和现有 runner 报告输出，未验证向旧网页聊天主动推送。私域节点 claim 现在明确返回 `workspace_write_allowed=true` 与 `execution_adapters_available=false`，避免把文件任务获权和任意 CLI 能力混为一谈。

## 验证记录

测试使用临时 SQLite、合成可信 window、模拟 Runner，以及单独创建的测试进程。没有使用生产任务数据。

| 检查 | 当前证据 |
| --- | --- |
| Store Host Goal 回归 | 最终 108/108 通过，含一次性授权、全窗口 tombstone、独立连接竞争、真实数据库锁等待跨 deadline、连续交接及通用 begin/activate/recover 旁路拒绝；Agent isolation 16/16、会议 9/9 通过 |
| native successor 业务权限 | 10 项通过，含实际私域写入、Own Memory/会议继承、跨 Agent 拒绝、恢复重新 claim、真实软硬链接拒绝；普通 Goal adapter 6 项和文件 broker 15 项回归通过，后者含真实 Windows junction、大小写与 dotfile |
| native 准备与控制 | 最终应用二进制：原生交接 11/11、管理控制 6/6、HTTP/OAuth 4/4 通过；HTTP 测试实际领取并兑换 claim grant，不依赖人工候选预登记。包括同身份切权、等待会议发布拒绝、获权继承、丢响应查状态、重启、真实文件继续写入、失败 notice 和固定拒绝审计 |
| 身份入口回归 | 最终应用二进制 auth 113/113、HTTP 隔离 8/8、路由元数据 11/11、隔离 OAuth 3/3 通过；不把曾误用过滤器的 0-test 日志计入 |
| Python 调度器全套离线检查 | 窄授权接线后 262 项：233 通过、29 按既有条件跳过、0 失败；含真实发送流程配合模拟 Browser 的秘密投递/不落本地盘、错误不回显及发行失响应检查 |
| Python commit 参数补充回归 | 两个定向套件 37 项通过 |
| 工具契约 | 本次相关 15 项通过（含拒绝自报身份/就绪字段）；模块全套 114 通过、2 失败，失败是 `run_process` 说明文字与断言失配，本轮未改该说明/CLI 行为或弱化测试 |
| 实际进程树停止 | 既有 `explicit_terminate_kills_tree` 1 项通过，仅使用测试自行创建的进程 |
| Runtime Host Goal 集成 | 92 项通过；最终代码的接管专项 5 项、MCP 请求元数据握手 4 项、公共 registry/schema/权限映射 2 项全部通过。MCP 用生产 metadata parser→dispatcher→prepare/commit→重开测试实例后 status 校验 |
| 真实网页接管 | 未执行；旧 `resume_started` 网页证据不适用于本版 ready/owner/new-run 协议 |

用户列出的首次/第四次成功、四次耗尽、迟到/并发/重复、旧链、响应丢失、隔离拒绝、重启场景由 Store、调度器和 Runtime 分层测试覆盖；各层模拟结果不能相加成为真实网页端到端证明。

日志及源码 SHA 清单位于 `E:/wbecodex/validation/handoff-reliability-20261023`。外部调度器改前源码备份位于 `E:/wbecodex/_archive/handoff-reliability-20261023`，只包含源码，没有运行数据库或凭据。

最终应用编译与 HTTP 测试见 `launch-claim-host-http-final.log`，同一应用二进制的其他组见 `claim-final-*.log`；Store 完整验证见 `store-claim-deadline-host-goal-fc46f9aa.log`，Python 见 `python-tests-launch-claim.txt`。分组可能重叠，不相加为独立用例总数。早期编译错误、旧二进制断言失败及 0-test 记录保留作过程证据，不计入最终通过结果。源码清单描述共享脏工作区的被测文件内容，不代表整个工作区都是本轮修改或已有干净快照。

## 八项验收对照

| 场景 | 证据与实际边界 |
| --- | --- |
| 首次接管 | HTTP/OAuth 整链领取 grant → 陌生窗口 prepare → 四文件核验 → commit/status → 新 node-read/claim → 同一任务实际文件续写；源只在 commit 后失效 |
| 第四次成功 | Store 与 Python 模拟前三次失败、第四次成功；断言四次不同 attempt 的既有 adapter 调用，只有最终候选获权 |
| 四次全失败 | Store/控制/runner 都保持 failed、count=4，后续不产生第五次；Checkpoint、成果和失败原因保留，原生 status 含明确中文停止提示 |
| 迟到、重复、并发 | 旧 grant/attempt、源窗口、其他身份及撤销窗口拒绝；同窗重放读回原结果；独立数据库连接二窗竞争只有一个候选；数据库排队跨 deadline 拒绝并回滚 |
| 旧源与旧链 | 旧 bearer 和缓存 binding 的真实文件写入失败；共享事务等候在途文件操作。legacy 后台取消/终态协议用模拟 Runner 验证，另有测试自建进程树的实际终止测试；未验证任意 CLI 强隔离，native 一律拒该通道 |
| 成功响应丢失 | 已提交状态查询及重复 commit 返回同一 owner，不重新拉起；发行 grant 响应丢失也先查状态，同次不会重发或轮换授权 |
| 任务与私域权限 | 真实私域文件、另 Agent sentinel、符号/硬链接、Windows junction、跨 Goal/root 扫描拒绝；Pending 只读包，获权后同 Agent 自有 Memory/授权会议按用户确认保留 |
| 重启保持 | 重新打开 SQLite 后保留 owner、scope、attempt 计数、window mapping、claim hash 及已消费关系；本地 transport 去重也持久化 |

以上证明的是源码协议及本地自动化行为。没有把模拟浏览器、模拟 Runner 或本地 HTTP 请求称为真实 ChatGPT 网页端到端成功，也没有宣称已部署。

## 修改文件

仓库 `E:/wbecodex/source`：

- `crates/webcodex-store/src/host_goal.rs`、`host_goal_context.rs`、新增 `host_goal_handoff.rs`、`host_goal_handoff_tests.rs`；相关 `host_goal_tests.rs`、`host_goal_review.rs`、`host_goal_review_tests.rs`、`lib.rs`。
- 新增 Store `agent_task_scope.rs/tests`、`agent_host_window.rs/tests`；共享 `agent_isolation.rs` 增补 owner key、持久 scope、保留 scope 的可信恢复、拒绝 generic scoped activation 和窄 host 管理元数据。其他私域基础功能属于并行隔离任务。
- `src/agent_goal_adapter.rs`、`agent_isolation.rs`；新增 `agent_handoff_access.rs/tests`、`agent_goal_handoff.rs/tests`、`agent_handoff_control.rs/tests`、`agent_host_auth.rs/tests`、`mcp/agent_host.rs`；`auth/middleware.rs`、`mcp.rs`、`action_audit.rs`、`lib.rs` 只增加窄宿主接线与私域日志保护。
- `src/tool_runtime/host_goal.rs`、`host_goal_handoff.rs`、`host_goal_context.rs`、`host_goal_project_files.rs`、`host_goal_execution_refs.rs`、`host_goal_projection.rs`、新增 `host_goal_takeover_access.rs`、`goal.rs`。
- `src/tool_runtime/kernel.rs`、`mod.rs`、`dispatch/goal.rs`、`dispatch/mod.rs`：仅本轮工具路由/执行门接线，保留并行身份改动。
- `crates/webcodex-tool-runtime-contracts/src/tool_call.rs`、`tool_audit.rs`；`crates/webcodex-tool-contracts/src/tool_catalog.rs`、`registry/input_schemas.rs`、`registry/input_schemas/host_goals.rs`、`registry/output_schemas/host_goals.rs`、`tool_definition/host_goals.rs`、`tests/host_goal_schemas.rs`。
- `src/tool_runtime/tests/host_goals.rs`、`host_goal_handoff.rs`、`host_goal_handoff_background.rs`、`host_goal_sync_receipts.rs`、新增 `host_goal_takeover.rs`。
- `src/tool_runtime/tests/support/runtime.rs`：新增工具的公共参数样例。
- `src/mcp_tests/host_goal_transport.rs`：适配可信新旧窗口的 MCP 协议测试，保留弱身份被拒和分页恢复覆盖。
- `docs/HOST_GOAL_MODE.zh-CN.md`、本文件。

外部现有调度器 `E:/wbecodex/chat-handoff`：

- `auto_handoff.py`、新增 `handoff_reliability.py`、`handoff_control_api.py`、`handoff_controller.py`、`edge_adapter.py`、`handoff_flow.py`、`edge_tools.py`、`run_auto_handoff.py`（在 status 与现有日志中明确报告交接失败、执行停止及状态保留）。
- `edge_handoff.py`：只接收独立内存授权参数，持久 request 始终不含秘密。
- `test_auto_handoff.py`、新增 `test_handoff_reliability.py`、`test_handoff_control_api.py`、`test_launch_claim.py`、`test_handoff_controller.py`、`test_thinking_handoff.py`、`test_edge_tools.py`、`test_run_auto_handoff.py`。
- `AUTO_HANDOFF.md`、新增 `HANDOFF_RELIABILITY.md`。

未删除用途不明文件；临时测试/构建产物保留在专用 validation 目录，没有清理其他 Agent 的工作。
