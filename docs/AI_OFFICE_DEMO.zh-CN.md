# AI 办公室与项目团队设置

入口为 **WebCodex 桌面端侧栏「AI 办公室」**，项目卡片提供「团队设置」。本机网页用于独立预览，不嵌入 GPT 聊天。

实现保存在独立集成副本 `E:\wbecodex\.local\work\cli-fallback-20261023`，宿主配套文件位于 `E:\wbecodex\.local\work\cli-fallback-host-20261023`。没有覆盖其他 Agent 正在修改的原 `source`，也没有部署、发布或重启正式服务。

## 查看演示

- [AI 办公室](http://127.0.0.1:4180/?ai-office=demo)
- [项目团队设置](http://127.0.0.1:4180/?ai-office=demo&view=projects)

人物为原创透明 Q 版时尚角色，具有站立和左右步帧、独立走动、暂停及减少动画偏好。昵称在刷新和轮询时保持一致，真实任务 ID 单独保留。素材和生成提示词位于 `apps/desktop/src/assets/office-residents-v2.png` 和同名 `.md`。

演示项目、人物关系、任务和百分比明确标为示例；策略持久保存、CAS 冲突检查、计划验证与本机模型发现使用 Rust 后端。预览不会创建 GPT 对话或派发模型任务，也禁用真实项目的添加、切换和删除。

重启独立预览：构建 `apps/desktop/src-tauri` 的 `ai-office-preview`，再在 `apps/desktop` 设置 `$env:AI_OFFICE_PREVIEW_PORT='4180'` 并执行 `npm run demo:office`。默认端口 4179 可覆盖，本轮保留旧预览并使用 4180。仅绑定 `127.0.0.1`，不自动打开窗口，策略存储在该副本的 `.ai-office-preview`。冒烟脚本使用相同端口环境变量。

## 项目设置与真实任务

默认「总经理自主安排」。自定义模式设置普通子 Agent 最少/最多人数、每个 Agent（含总经理）的 CLI 最少/最多调用次数、允许的 CLI、模型和思考强度。上限在冻结整份计划和消费调用槽时限制；自定义下限还在任务标为成功时按真实执行证据检查，预填槽不满足最低要求。这些数量不是并发数。配置上限为 32，恢复助手另计。

真实桌面通过已有保存连接和用户 PAT 调用 Server。策略按所有者和项目隔离，以版本号防止并发覆盖；保存并重读确认后才显示已保存。离线缓存仅供查看，不能冒充已同步。

Server 提供 `team_policy_get/save`、`team_plan_register/list/read/cancel`、`team_cli_start/observe`、`team_agent_start/observe`。计划引用已有真实 AgentTask ID，核对所有者、项目和存活状态后冻结普通成员、每次 CLI 的模型/effort/任务与策略版本。两种已支持 CLI 的每个执行槽都必须明确填写模型与 effort；项目策略仍可留空交给经理选择。多个 CLI 调用必须分别给出子任务；单个调用可继承所属 AgentTask 的任务。

原生 `ai_office_team_call` 与 CLI `webcodex office` 共用这些工具。CLI 只接受固定 action 和有界 JSON stdin，使用与保存连接一致的用户 token，不借用 Bootstrap 凭据。业务错误原样返回，不自动重试写入。

`enforcement=managed_tasks` 只覆盖通过团队接口登记并执行的计划。已有独立聊天、直接 `run_process` 和外部工具不会自动归属或受这份配额控制；预览仍为 `demo_plan_only`。

真实页面显示任务标题、成员角色、GPT 档位与宿主请求、CLI 原始 Job 和恢复槽。没有上报的信息显示「未提供」；未派发的成员不显示正在工作，总经理仅表示主任务协调。候选回答和宿主结束均不等于业务验收成功。

`team_plan_read` 的 CLI 尝试支持 `attempts_offset/attempts_limit`，默认每页 20、最大 20。按不可变插入顺序排列，新的尝试追加、状态更新不重排；用 `attempts.next_offset` 继续。页面可加载后续记录，刷新会重新读取已经展开的页并保留人物选择。分页不限制 `completion` 的计数范围。

`completion` 分别返回计划数、已消费预算、已核实派发/调用、每位成员最低要求和配额完成门槛。全部保留的尝试都消耗原预算，包括失败、未启动和结果未知。当前 GPT 宿主记录只能在 `response_observed=true` 时严格确认已发生派发；仅排队、占用 fence 或存在 request_id 不算。CLI 只有真实绑定 Job 已观测 `completed/timed_out` 才满足调用下限，非零退出和超时的实际执行也计数；未启动或结果未知须继续核对原执行，不能伪造或重复调用凑数。

成功门槛在 `complete_agent_task_attempt` 与 CodingAgentRun 终态归并这两个现有写入事务中统一检查。根任务检查整个冻结团队；普通子任务检查自身 CLI 下限。只有符合门槛才能写入 `succeeded`，拒绝时不消耗成功幂等键；失败仍可正常报告。默认自主模式不强加自定义下限。已经成功完成的相同幂等请求仍按原记录重放。满足最低要求不代表业务验收成功，页面保留独立的真实任务状态。

## 总经理调用顺序

1. 读取真实项目与当前团队策略；发现实际 CLI 模型/effort，通过 `chat-agents.discover_chat_profiles` 发现真实 GPT 档位。
2. 用已有 `create_agent_task` 创建同项目的主任务和普通子任务，保留真实 ID。
3. 用 `team_plan_register` 一次登记完整成员和 CLI 调用计划，携带策略版本与幂等键。
4. 需要 GPT 子任务时调用 `team_agent_start`，明确传入总经理选择的 `thinking_profile_id`，沿用新建 ChatGPT 对话、关联 WebCodex、填写任务的现有宿主流程。
5. 需要 CLI 时调用 `team_cli_start`，指定冻结成员和 `call_index`，并传入失败接管所用的 `fallback_thinking_profile_id`；执行参数来自冻结槽，不接受任意命令。
6. 通过团队只读工具和原 Job 工具观察同一次执行；相同幂等意图返回原记录，未知结果不换 key 重发。

档位由总经理选择，不自动选最高档或默认档。Server 使用纯缓存 `read_chat_profiles` 核实精确选择、ready 和有效期；发现与缓存读取不同。已有尝试重放保留原选择，不因目录后来过期而重新派发。

## 本机 CLI 适配

WorkBuddy 实际调用 `codebuddy`；本机 2.162.0 帮助返回 20 个模型和 6 档 effort。Antigravity 的正确命令为 **`agy`**，本机 1.4.5 无需更新。此前发现进程未使用系统代理，并把 stderr 进度混入目录；修复后实际预览链约 9.7 秒返回 13 个模型、5 档 effort。目录发现不代表账号额度或模型执行已验证。

发现仅运行固定 `--version`、`--help`、`models`，每次有 30 秒绝对期限、输出上限和进程树清理。只解析 stdout，失败不返回预置模型表。自主模式由经理选模型，手动固定模型先切到「我来设定边界」。

真实执行使用现有 typed process/Job：

- WorkBuddy：`codebuddy --model M --effort E --print --input-format text --output-format json`，任务从 stdin 传入。
- Antigravity：`agy --model M --effort E --input-format stream-json --output-format stream-json`，stdin 为一个 user NDJSON 事件并关闭输入。

不添加跳过权限的参数。Runner 优先原 PATH；仅在正常解析失败且没有隔离/环境覆盖时尝试当前用户固定安装目录。只有准确安装路径且参数匹配的 Agy 子进程会采用已启用、不含凭据的系统代理；保留显式代理（包括空值）及 `NO_PROXY`，仅对该子进程禁用自动更新。不修改全局环境、登录或 CLI 安装。

## CLI 无响应恢复

已与「设计 CLI 无响应时的 GPT 兜底」聊天完成同一副本的集成。团队 CLI 短执行也保留真实公开 Job；观察器沿用原绝对期限。读接口本身不派发、不重试。

服务重启后，协调者以原幂等键、原参数调用 `team_cli_start` 可恢复同一个 Job 的观察。若 CLI 终态已落盘但恢复入队中断，原请求重放只补幂等恢复预约，并返回独立的 `recovery_enqueue` 结果；不会再次执行 CLI。本版没有启动时扫描所有历史任务的调度器，纯读页面也不承担恢复写入。

只有来源身份、真实 Job、可信终态和已支持的故障分类一致时才恢复。已确认进程树停止的超时和已分类启动错误可以进入；协议错误仅识别明确支持的结构化错误码。普通非零退出、无日志、目录刷新失败、用户取消、权限/隔离拒绝不单独触发。`lost/recovering/stop_requested/outcome_unknown` 不能当停止证明。

每个根任务只有一个占用中的 GPT 恢复槽，多次故障排队。恢复助手不占普通人数、不满足最低人数、不退回原 CLI 次数，不再派 CLI 或子助手。未知发送保留占槽，只有确认未发送的 busy 可有限重排。根任务取消先原子标记所有恢复和普通 GPT 记录，再请求 Job 停止；取消请求不冒充停止确认。

宿主入口 `chat-handoff/cli_fallback_worker.py` 让普通 GPT 和恢复 GPT 复用同一个 `chat-agents.Service`、浏览器互斥、STOP/PAUSE 和 generation。沿用既有控制连接配置与专用凭据，凭据不进入对话。只有受保护控制端点可以 acquire/record/resolve。当前 worker 绑定显式配置的所有者和项目，不自动遍历所有项目；页面能显示多个项目，不等于单个 worker 已为全部项目启用 GPT 派发。

恢复状态为 `queued/starting/running/unknown/blocked/cancelled/confirmed_not_started/verified_terminal`。`response_observed` 只是候选回答，不释放恢复槽、不标业务成功；可信协调者核实结束或确实未启动后通过 control resolve 记录证据。此版本不把回答文本自动当作验收结果。

## 验证与启用

源码集成和本机目录查询完成；尚未运行真实模型任务或发送真实 GPT 测试对话。正式启用仍有身份链阻塞：当前 office CLI 要求用户 PAT，而模型工具入口要求有效 Agent run；经理和新 GPT 对话的身份归属也须统一接入。仅重新配对或增加 scope 不能解决此冲突。本轮只补分页和完成门槛，不放宽鉴权、不使用共享管理凭据；该身份链须由协调任务另行修复并做真实端到端验收，不能据本轮单元测试宣称正式桌面调用可用。

针对性验证覆盖配额/身份/幂等、不同 CLI 子任务冻结、真实 Job 保留、恢复单槽、普通 GPT 控制、Windows 路径/代理、原生桥接、页面状态、共享 worker 与缓存档位。使用临时数据库、假传输或测试进程，不能替代部署后的真实账号验收。

2026-10-24 集成复核：桌面前端 127 项测试、CSS 契约、类型检查和生产构建通过；团队存储 19 项、恢复存储 11 项、Runner 本机适配 8 项及既有结构化进程 19 项、原生桥接 7 项通过。新版 4180 预览冒烟通过，真实 Agy 目录返回 13 个模型（约 10.5 秒），WorkBuddy 返回 20 个模型。构建产物与本机预览文件保留用于复核；正式源目录保持独立。

CLI 回路复核 7 项通过，其中 3 项覆盖终态漏入队的修补、重复修补不产生第二个 CLI Job、来源 Job 暂不可用时明确返回错误。原完整团队运行时过滤测试另有 10 项通过；两个数量有重叠，不合并计数。

后续审查修复的定向验证：CLI 分页存储 2 项与请求合同 1 项通过；25 条后再追加 5 条，状态更新不重排，第 21 条及后续均可读取，跨所有者/项目读取和越界页被拒绝。完成门槛 8 项通过（先运行 7 项，再单独运行新增关键用例）；既有 AgentTask 域回归 33 项通过。普通子任务能在只完成自身 CLI 后独立结束，但根任务在子 Agent 只有 queued 或 running/request_id、没有派发证据时仍拒绝成功。运行时 `tool_runtime::team_task::tests` 本轮 9 项全通过，包含原 7 项恢复回归、分页默认值和实际第 21–25 条读取/全计划计数。

本轮前端定向测试 42 项及 TypeScript 检查通过，覆盖第 21 条的 Job/失败信息、25→30 条刷新不漏不重、迟到分页响应不污染其他项目/任务、人物选择保留及实际完成门槛显示。本轮未重跑整套前端测试或生产打包，未做 GUI、正式服务重启或真实账号验收。新测试使用临时数据库并自动释放；保留现有独立构建缓存，没有本轮待删除的临时交付文件。

`apps/desktop/scripts/ai-office-smoke.mjs` 验证预览持久保存、CAS、数量范围、来源限制、操作白名单及 WorkBuddy 目录，完成后恢复原演示策略。所有检查通过无界面接口完成，没有浏览器点击或截图自动化。

## 素材与参考

参考 [Pixel Agents](https://github.com/pixel-agents-hq/pixel-agents)、[Star Office UI](https://github.com/ringhyacinth/Star-Office-UI)、[Agent Virtual Office](https://github.com/k1dav-c/agent-virtual-office) 的工作区与角色呈现。家具为项目 SVG/CSS，人物由内置 imagegen 原创生成，没有复制这些仓库美术。

CLI 参考 [Antigravity headless](https://antigravity.google/docs/cli/headless/) 和[故障排查](https://antigravity.google/docs/cli/troubleshooting/)。以本机查询作为目录验证依据，不以文档样例冒充账号可用性。
