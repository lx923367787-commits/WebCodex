# 状态权威契约

本文描述当前实现，也是本轮及后续重构的约束。[English](STATE_OWNERSHIP.md)。
配套依据：[Session](agent/session-model.md)、[权限](agent/permission-model.md)、
[持久 Agent](architecture/durable-agent-conversation.md)。

## 统一规则

- **Durable（D，持久）**：已成功写入配置的存储；不代表内存中的每次更新都能抗崩溃。
- **Ephemeral（E，临时）**：仅在当前进程有效。
- **Reconstructable（R，可重建）**：从指定权威源重建投影，绝不意味着重放执行。
- **Observation-only（O，仅观察）**：不授予执行权。
- **Authority-bearing（A，承载权威）**：只对表中明确指定的事实有效。

这些标签相互独立，持久 Audit 仍然只是观察。只有表中的写入方能通过既有校验 API
改变对应状态，其余组件都只能观察。共享 SQLite 句柄不等于共享状态权威。
Kernel 统一负责 Auth/Scope、Session、Goal、Permission、Project，Handler 接收 typed
ToolCall；Runner 继续执行本地路径、安全策略和进程所有权校验。

下面 S/R/H 分别表示 Server 重启、Runner 重启、Chat handoff。保留均以相同存储、
身份作用域及成功持久化为前提，仍受保留期和显式删除限制。协调恢复不等于自动重试。

## 执行与控制状态

| 状态；分类 | 唯一权威及写入方 | S / R / H | 权限、推导与冲突规则 |
| --- | --- | --- | --- |
| Host Goal 当前 Phase；D,A | Host Goal SQLite store；合法 Goal 状态转换 | 保留 / 保留 / 保留 | 限制允许的工作；不能从 Chat、Session 或 provider end-turn 推导完成，以已提交 Goal 行为准。 |
| 当前 Plan Version 和批准；D,A | Host Goal store；plan save/confirm | 保留 / 保留 / 保留 | 实施门禁；计划文件只是物化结果，文件存在不代表已批准。 |
| 当前 Node；D,A | Host Goal store；claim/check/checkpoint | 保留 / 保留 / 保留 | Node effect 的权威；Session 标题不能选择当前 Node。 |
| 当前 Attempt；D,A | Host Goal store；claim/check 和 Attempt 转换；bootstrap 只观察该记录 | 保留 / 保留 / 历史保留；退休后需继续业务执行时认领新 Attempt | 必须精确匹配 tuple；旧执行证据不能复活旧 Attempt。 |
| 当前 Fence；D,A | Host Goal store；原子分配 | 保留 / 保留 / 旧值成为历史，新 claim 分配新值 | 旧 Fence 必须失败，不受 Runner/Session 表面活跃状态影响。 |
| Context ACTIVE/DRAINING/RETIRED、identity_source；D,A | Host context SQLite state；可信身份绑定与退休转换 | 保留 / 保留 / 退休持久保留 | DRAINING 只允许既有规则许可的完成/协调；RETIRED 永不重新获得业务执行权。调用方字段不能替换可信 Host 身份。 |
| Operation intent、dispatch state、receipt、完成；D,A | Host Goal Operation store；准入和权威协调 | 保留 / 保留 / 保留 | receipt 必须绑定精确 Operation/Attempt/Fence。调用记录或超时不是成功；Unknown 只由 receipt/Job/Run 或已有显式解决流程消除。 |
| 普通 Job 实际执行；E + D/R 注册表投影，A | Runner JobManager 管进程，RunnerRegistry 校验实例、连接、序号与生命周期 | 同一活 Runner 协调 / 普通进程所有权丢失，执行恢复或 lost 规则 / 保留但不授权旧 Chat | Server 持久 Job 行是恢复依据，不是当前活跃证明。当前实例清单优先，旧实例/旧序号不能复活终态。 |
| Detached Job；D + 活 supervisor，A | Detached supervisor 和 Runner 持久 detached store；JobManager 协调 | 协调 / 按 supervisor receipt 恢复 / 保留 | 可独立于 Runner 存活，与普通 Job 不同；不能根据 Session 或响应缺失重新启动。 |
| Coding Agent Run；D + E 活 provider，A | Runner coding-agent manager/provider Run 状态及持久 Run 文件 | 协调 / 恢复终态或 interrupted/unknown，不能假定仍活跃 / 保留 | 必须匹配 provider instance/run。Server binding/cache 不是进程权威；持久 observation MAC 用于查找，不授予权限。 |
| Runner instance/connection lease、capability；E,R，路由 A | RunnerRegistry 接受可信注册并替换 lease | 重新注册 / 新实例 / 无关 | 接受的协议/capability 决定路由；旧连接断开与迟到结果不能覆盖新 lease。UI 健康状态不授予能力。 |
| Persistent shell；E,A | Runner/本地 shell manager 管进程；SessionShellRegistry 管路由绑定 | Server binding 丢失 / shell 所有权丢失 / 不授予新 context 权限 | 打开时固定 CWD/resource 绑定；后续 Session context 不能重定向旧 shell。 |
| Goal loop worker；E | GoalController 管本进程 worker；SQLite 管持久 Goal 状态 | worker 丢失，从 store 恢复 / 协调 Run / 无关 | worker map 只说明本地活跃，不能覆盖持久 Goal 完成状态。 |

代码依据：`webcodex-store/src/host_goal*.rs`、`goal.rs`；
`tool_runtime/host_goal*.rs`、`goal.rs`、`coding_agent.rs`；RunnerRegistry 的
`reconciliation/job_updates/jobs`；Runner JobManager 和 `detached_job/coding_agent/persistent_shell`。

## 连续性、证据与业务记录

| 状态；分类 | 唯一权威及写入方 | S / R / H | 权限、推导与冲突规则 |
| --- | --- | --- | --- |
| Workflow Session 历史；配置 ledger 时 D，否则 E；历史 A | Workflow Session store、v2 JSON ledger；canonical recorder/Session API | 已持久且保留的历史 / 保留 / 显式授权后保留 | 记录观察到的事实，不证明进程成功；recording/business Session 独立授权，不能从连接/window 推导。 |
| Session guards/project/lifecycle；D/E,A | Session store；create/update/close | 随 ledger / 保留 / 保留 | Kernel 校验精确目标、scope、active 状态和 guards；当前 store 胜过调用方上下文。 |
| Session message、assignment fence、attention/resolution；D/E，消息事实 A | Workflow Session 消息 API 和原子 assignment 转换 | 随 ledger / 保留 / 保留 | assignment fence 与 Goal fence 不同；ACK 仅是当前请求还记得消息的证据，不是持久处理完成或执行权限。 |
| Session context revision；D/E，连续性水位 A | Session recorder 分配 checkpoint revision | 随 ledger / 保留 / 保留 | ACK 不证明送达、不授权；历史丢失时有界恢复，不能编造历史。 |
| Host Goal handoff checkpoint；D,A | Host Goal checkpoint 事务及 context retirement | 保留 / 保留 / 保留 | canonical checkpoint 胜过 handoff 文件/summary；文件缺失也不能撤销退休。 |
| Workspace checkpoint；D | Runner 项目快照/文件和 checkpoint API；本地路径的 Server metadata store | 保留 / 保留 / 保留 | 是文件恢复产物，不是 Host Goal phase/fence/checkpoint；恢复依赖精确快照与路径校验。 |
| Tool execution evidence；D/E,O | canonical Session recorder 记录有界、脱敏的 start/result pair | 随 ledger / 保留 / 保留 | “run_process 已调用”不能推出成功，必须依赖 Operation receipt 或 Job/Run 权威结果。 |
| Validation evidence；D/E,O，验收输入 | Validation 解释结构化 command/Job 结果，Session 存证据及协调标记 | 随 ledger / 协调 Job / 保留 | Validation 管断言语义/新鲜度，Job 管实际执行；终态快照锁保持标记顺序。检查通过仍须合法 Goal 转换才能推进 Goal。 |
| Project filesystem/registry；D，内容与路径 A | Runner-owned project 和文件工具，也接受用户/外部文件修改 | 保留 / 保留 / 保留 | 当前 filesystem、canonical root/path policy 优先；Server inventory/hash 必须检查新鲜度，不授予写权限。 |
| Memory；D，记忆记录 A | Server SQLite Memory domain；有 scope 的 Memory API | 保留 / 保留 / 保留 | 是参考上下文，不是执行权或 Goal 完成事实；revision 检查胜过旧模型副本。 |
| Agent identity/Conversation/message/Wake/Agent Task/Attempt；D，域内 A | Server SQLite Communication/Agent Task API | 保留 / 保留 / 保留 | 独立于 Workflow Session 和 Host Goal；endpoint generation、task fence、幂等键不可混用。 |
| Host continuation endpoint/controller；E，当前投递绑定 A | 注入 Communication DB 时创建的 AgentContinuationController | 丢失 / 无关 / 必须符合当前绑定 | Agent 身份可持久，进程内 endpoint 不持久；历史消息不证明 endpoint 活跃。 |

Session ledger 有异步写入：普通已排队更新可能在 flush 前崩溃丢失；需要持久确认的
API 继续使用现有 durable-write 路径。保留期、cold restore、v2 行格式不变。
本轮不强化持久性承诺，也不迁移旧格式。

## Gateway、配置和可观测性

| 状态；分类 | 唯一权威及写入方 | S / R / H | 权限、推导与冲突规则 |
| --- | --- | --- | --- |
| Plugin schema；E,R,O | 当前 provider describe observation，经 Gateway 校验 | 重新 describe / 使旧实例 observation 失效 / 仅匹配绑定有效 | revision 不同 fail closed；schema 说明参数/effect，不证明执行。 |
| Plugin binding；E，process-local，仅绑定 A | Plugin Gateway 经校验 describe 后写 binding store | 丢失 / 旧 provider 绑定失效 / 继续遵守 scope | 不是执行 receipt；schema/instance 改变后重 describe，不从 Audit 恢复。 |
| MCP provider schema；E,R,O | MCP Gateway 按当前 provider/caller 记录 observation | 丢失 / 重新观察 / scope 不变 | describe 后校验 call，schema 变化 fail closed，再 describe。 |
| SSH resource binding；E，精确资源选择 A | SSH Gateway 的合法 describe/binding | 丢失 / 重新验证 Runner/config / scope 不变 | 不把凭据拷入 Session；路由绑定不证明操作完成。 |
| Runtime config/limits/exposure；E,R | composition startup/builders 和合法 Runner config reload | 重新加载 / 重新加载 / 无变化 | Kernel 使用当前策略；对外 config/capability summary 不能反向覆盖配置。 |
| Permission decision；E，本次调用 A；记录副本 O | 唯一 canonical PermissionEvaluator 及硬安全门禁 | 重新判断 / 重新判断 / 重新判断 | 历史 auto-approval 不授权下次请求，硬拒绝优先于软策略。 |
| Git continuation MAC；E，cursor 完整性 A | Runtime key 和 Git cursor 校验 | 失效 / 不变 / scope 继续校验 | cursor 完整性不是项目权限，也不是快照新鲜度。 |
| Window Activity/loop timing；E + 持久 audit projection，R,O | WindowActivityRegistry 和 ActionAudit 投影 | 活跃视图重置，audit 保留 / 无关 / 仅保留历史关系 | 不推导 Session、context 权威、model turn 或执行 lease。 |
| Runtime connection/activity；E,R,O | RuntimeObservations 与注入的 activity recorder | 内存重置，持久 sink 历史保留 / 新 observation / 历史 | 最近成功调用/连接不代表当前执行权。 |
| Metrics；O | metrics sink 消费事实，fail-open | 取决于 sink / 无关 / 无关 | 不参与权限、重试、持久化决策。 |
| Audit/Action Audit Session；D,O | Server SQLite 请求/audit recorder | 保留 / 保留 / 保留 | 仅历史证据；不是 Workflow Session，不能独立授权或协调副作用。 |

## 冲突和恢复步骤

1. 先确定具体事实与表中唯一权威，不能让多个 cache 投票。
2. 按现有域规则校验 identity、project、context、attempt/fence、Runner/provider
   instance、schema revision 和 sequence。
3. 查询权威 receipt/state，保留 NotStarted、OutcomeUnknown、Running、Succeeded、
   Failed、Cancelled 的区别；已有 wire lifecycle 拼写不变。
4. dispatch 后响应丢失时，只协调原 Operation/Job/Run；可能有副作用的调用不得自动
   重放。保留期 cache 中查不到不等于 NotStarted。
5. handoff 必须持久退休旧 context。新 context bootstrap 原 Goal，按实际阶段继续；
   需要业务执行时，先读节点要求并对账原操作，再按契约 claim 新 Attempt/Fence。
   已确认且未变化的计划不重复确认，completed 不因恢复而重新认领。旧 receipt 只作为历史证据。
   重连、Session resume、provider
   re-describe、复制 epoch 都不能撤销退休。

不确定或冲突证据继续保持 Unknown，或按既有策略 fail closed。观察投影可重建或丢弃，
不能编造执行事实来让恢复看起来已经完成。
