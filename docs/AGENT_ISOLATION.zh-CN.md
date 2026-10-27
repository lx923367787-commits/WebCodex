# Agent 私有空间与会议厅

同题讨论使用任务级 `team_decision` 做证据审查：按原始来源去重，由主 Agent 核验引用，服务端约束结论状态；普通分工不启用。接口、版本与验收规则见 [团队证据审查](agent/team-evidence-review.md)。

## 业务项目文件与 CLI 工作区（2026-10-26）

Agent 私域与用户授权的业务目录分别检查。源码、参考资料和交付文件属于项目资源，不因某个 Agent 创建或修改而自动变成该 Agent 的私有状态。其他 Agent 的目标、记忆、检查点和 CLI 历史仍只通过既有私域或明确发布的成果接口访问。

`agent_assignment.project_access` 返回当前目录授权来源、可使用的精确项目标识，或明确的 `reason_code`；`named_project: null` 不再是唯一诊断。命名项目按当前 Host 的目录映射检查；旧的普通 Runner 项目聊天按其精确 Host/owner/project 授权检查，出现新的目录表后只保留表中仍授权的同一文件夹。文件访问不依赖 `max_cli_calls`，也不扩大已有聊天的 CLI 配额。Runner 在线、目录注册和写权限在实际操作时再次检查。

隔离工具提供 `agent_project_files` 的列表/单文件读写，以及 `read_file`、`read_files`、`search_project_texts`、`apply_text_edits`。后四者复用原有范围读取、结果预算、搜索和事务编辑实现；参数中的 `project` 必须来自本聊天的目录授权，不接受 Workflow Session 选择器。批量编辑保持当前 SHA、唯一匹配和回滚约束。撤权、错误项目、只读目录、敏感路径、路径逃逸和其他 Agent 的运行目录继续拒绝。

通过 `team_cli_start` 启动的项目任务可以获得独立的可写源码副本。Server 从当前文件授权和团队执行目录决定来源，把源项目与根目录纳入本次启动哈希；模型不能指定宿主绝对路径。Runner 必须明确公布 `project_workspace_copy` 支持，并在复制前核验当前注册目录和 allowed_roots。旧 Runner 不会静默降级为空工作区。没有项目目录授权的显式私域任务仍使用输入快照，执行提示明确说明其工作区为空。

Windows 副本仍位于原 LPAC Job 的独立 workspace，宿主源码 ACL 不变。复制保留目录结构，排除凭据、Agent 运行私域、生成目录和旧 publish 交付；遇到链接、特殊文件或预算超限时在启动模型进程前失败，不截断输入。当前上限为 20,000 个文件、256 MiB 总内容、32 层目录及最多 60 秒复制时间；依赖文件也计入预算。原文件清单与 SHA 保存在只读 `input/project-manifest.json`，路径通过 `WEBCODEX_PROJECT_MANIFEST` 提供，清单摘要同时记录在原 Job 的持久元数据中。

CLI 在副本中修改和验证，按原 publish/manifest 协议交付成果，再通过当前文件 SHA 合入业务项目。执行工具链仍来自本机已审核的资源配置；缺少编译器或依赖应报告明确阻塞，不继承宿主全部 PATH、登录目录或其他 Agent 的资源。新字段需要配套的 Server/Runner 构建；源码修改本身不会更新已运行服务。

本轮定向验证：Server 文件访问、隔离入口、工具目录与团队链路 72 项；Core 路径保护与执行契约 20 项；Registry 执行/重放/资源边界 25 项；Store Host 与目录授权 17 项；Runner 资源与关闭状态 54 项，以及显式 Windows 文件系统/LPAC 进程测试 3 项，均通过，共 191 项。Windows 测试使用新建临时目录和合成执行器，实际验证源码副本读写、只读清单、子进程验证、宿主与其他 Agent 文件拒绝、完整停止证明及资源释放；没有调用收费模型，也不把未运行的 ignored 用例或完整工作区测试计为通过。相关编译、修改文件格式和差异空白检查通过；生产服务、账号与授权未变更。

以下保留初期隔离实现与历史验收记录；其中“全部执行被禁”等描述反映当时状态，当前接口以以上说明和运行时工具契约为准。

本改造将模型身份与用户/管理凭据分开。`agent_id` 表示逻辑 Agent；`run_id` 和 generation 表示一次可撤销运行。Handoff 不改变逻辑身份和私有目录。同一任务交付前只有一个有效执行者：pending 运行不能使用业务工具；切换必须等旧访问结束，在同一事务撤销旧运行并激活新运行。

本文件描述源码实现与验证边界，不表示已部署到当前桌面服务。真实运行数据没有用于测试，也没有被自动认领或迁移。

## 审查发现与修复位置

| 原有风险 | 修复位置与处理 |
| --- | --- |
| AuthContext 只有账号/共享 key/project 授权；同账号模型共用 Goal/Memory | `src/auth/context.rs`、`tokens.rs`、store `agent_isolation.rs`：专用 ModelAgent 凭据与服务端绑定。运行凭据没有用户身份字段和管理 scope |
| 通过自报 agent_id、parent、goal_id 或旧接口访问同账号记录 | `src/agent_isolation.rs`、kernel/dispatch、AuthMiddleware：模型入口要求有效运行身份；私域接口不接受调用者选定 owner；未适配旧工具拒绝 |
| 旧 Conversation 可省略身份退回 Human；网页子任务仅凭 request_id 取结果 | 模型凭据只进入隔离工具；旧消息/provider 接口不开放。共享只经会议厅成员授权与追加发布 |
| cwd/env/Windows Job Object 不限制进程读取文件 | shell、CLI、script、构建检查、插件、MCP/SSH转发均未获准执行；返回 `execution_isolation_unavailable` 或 `agent_tool_not_isolated` |
| 用户/管理凭据可直接调用模型接口，匿名/开发模式可变成 bootstrap | AuthMiddleware 在兼容分支之前强制模型身份；受保护管理面要求显式认证；运行凭据不能调用管理、日志、下载和 Runner/QUIC 接口 |
| OAuth 与路由别名可能把运行凭据升级为用户权限 | 独立 ModelAgent 类型；OAuth 授权拒绝该身份；路由拒绝尾斜线、重复分隔符和编码别名，避免认证与路由解释不同 |
| 完整 trace 和账号级审计可能保存私文 | 隔离运行关闭旧完整 trace/账号事件；使用事务中的身份、固定动作、错误码审计；`wc_ar_` 加入凭据脱敏词表 |
| 历史记录归属只能证明用户/项目，不能证明逻辑 Agent | 管理迁移工具先备份并记录未分配隔离清单，不移动 payload、不合并记忆、不发布到会议厅 |

## 存储与信任边界

可信 Server 数据库保存注册、运行凭据哈希、授权、会议厅条目和审计。普通模型无数据库、配置、备份、原始日志接口。私文件位于数据库旁的 `agent-runtime`，不位于共享项目源码目录：

```text
可信 Server state/
  state.db                 # 控制记录、受控业务存储，不向模型开放 SQL
  agent-runtime/
    projects/<project 的稳定哈希>/
      agents/<agent_id>/private/
        identity/ goals/ tasks/ memory/ checkpoints/ handoff/
        workspace/ cli_sessions/
```

会议厅使用数据库事务、成员表与不可变 BLOB 快照实现受控对象，不是可被所有模型任意写入的共享目录。每个房间属于一个项目，只有显式加入的 Agent 可读。发布者、时间、版本由服务生成。报告和消息追加保存；更新自己的产物产生新版本；评审必须指向已存在的确切产物版本。模型不能覆盖他人条目、指定作者或将私有路径当成产物下载入口。

文件访问通过 broker。路径使用 ASCII 安全组件，分类目录（workspace、memory 等）固定小写；普通文件名保留大小写并支持 `.gitignore` 等 dotfile。拒绝绝对路径、`..`、尾点/空格、不区分大小写的 Windows 设备名、ADS、符号链接、junction/reparse 和多硬链接；内部 `.snapshot-` 名称及大小写别名不向调用者开放。写入使用完整临时文件与原子发布，覆盖必须提交已读 SHA256。读取、搜索和结果均有上限；错误不返回宿主路径。写入结果不确定时返回 `outcome_unknown`，应先重读自己的确切路径，不能盲目重复写。

普通 Agent 没有宿主任意代码执行能力。目录、环境变量、cwd 和提示词不作为 OS 隔离证明。本实现不抵御宿主管理员；也不把用户在系统终端自行启动的同账号模型进程宣称为受支持的隔离执行方式。

## 身份与单一执行者

可信用户/服务通过 `POST /api/admin/agent-isolation` 管理注册与运行。该接口只接受显式 bootstrap 控制凭据，模型接口拒绝该凭据。操作包括：

* `create`：明确 owner_user、project 和可选 parent_agent_id；生成全新逻辑身份与初始运行。
* `create_meeting`：明确项目、成员和授权 brief。
* `begin_handoff` / `activate`：用于没有进行原生 Goal 交接的普通运行切换。pending 不能认证为普通运行；激活核验同 Agent、项目、前驱运行、generation 和有效期，原子撤销旧运行。已进入原生 Goal 交接的运行必须走组合协议，不能用通用切换跳过 ready 或替换候选运行。
* `generation` / `recover`：管理员明确指定逻辑 Agent 并用当前 generation 做 CAS，恢复普通运行或已完成接管的同任务运行；不选取最近任务、不创建替代逻辑身份、不合并其他记忆。原生交接仍在 waiting/awaiting_ready/prepared/failed 时，通用恢复拒绝执行，防止首次交接尚未写入 task scope 时恢复成无限任务范围的运行。首次失败后的人工接续流程尚未提供，保留状态供可信管理处理，不自动重新启动。
* `revoke`：撤销明确运行。

新运行 token 只在可信管理响应出现一次，数据库只保存哈希，不能放入模型 prompt、会议消息、源码或日志。由可信宿主为各独立 Agent 配置其运行凭据。直接 HTTP/MCP 模型请求携带自己的 `wc_ar_` bearer；请求中的身份标签不提供权限。共享 OAuth 宿主仅在下述明确登记的窄入口内代送请求，不能进入旧共享用户 dispatcher。

用户已明确确认：同一逻辑 Agent 交接后保留自己的记忆和已授权会议厅，业务写入仍限当前任务。任务范围不应把自己的记忆重新当作他人私域，也不能仅因 run 改变取消既有会议成员资格；旧 run 仍须失效。任务工作目录与 Goal 执行状态的写入授权，和 Agent 自己的记忆/会议授权分开核验。

运行校验在实际数据访问事务内再次执行。私文件操作持有 SQLite Immediate 事务覆盖验证及文件访问；其他连接不能在旧写入过程中完成激活。会议发布同样在事务中核验当前运行。旧 token、旧已解析 binding、pending 和过期运行均不能继续业务访问。

已增加显式可信宿主适配：`bind_host_window` 由 Bootstrap 控制面登记 OAuth 的确切 client、subject 与窗口绑定，并明确确认宿主独占可信外层窗口元数据。来源身份使用 OAuth 验证器的持久化记录，不使用名称或模型参数；project-share OAuth 不进入该适配。原始 OAuth 仅可进入 `/mcp` 窄入口，实际业务调用新建不含用户/管理员字段的 ModelAgent 上下文，并再次核验运行权限。

原生 Goal 交接由旧运行写检查点并进入 draining，可信调度器调用 `handoff_begin_attempt` 建立同一 Agent 的 pending run。可信启动回调必须先用 `bind_handoff_window` 登记准确候选窗口；模型即使知道 attempt UUID，也不能自行认领。候选只可查询、prepare 和 commit：读取并核验准确的计划、检查点与交接文件后，组合事务切换 Goal owner/fence 与运行权限。旧窗口继续绑定旧 run，不能自动指向新 run。管理 `revoke_host_principal` 可撤销该宿主入口；既有完整 trace 与账号事件不保存此入口的私有内容。

当前实际浏览器启动器返回的 URL 尚不能证明它对应哪个可信 `openai/session`，也没有已验证的注册回调。不能从 URL 猜测窗口、靠 prompt 传 secret、替换共享 connector token 或首次收到的元数据补登记。因此：上述服务端组合路径可在合成服务测试中验证；真实浏览器自动成功接续仍未验证，缺少回调时按现有尝试次数与期限失败关闭。详细调度机制和验证边界见 `HANDOFF_RELIABILITY.zh-CN.md`。

## 正常协作

原有 Host Goal 的安全子集也已接入私域：`goal_mode_start`、`goal_plan_save`、`goal_confirm_plan`、`goal_bootstrap`、`goal_read`、`goal_claim_node`、`goal_checkpoint`。复用原状态机、revision/幂等、用户确认与节点完整读取凭证；Goal 写事务在重放检查之前核验运行身份。计划与检查点保存为自己的 broker 文件，Bootstrap 必须给出明确的自己的 goal_id。模型不能自报 context_epoch；计量使用现有可见文本估算和已保存阈值，不将其冒充真实模型上下文总量。

要求执行 shell/CLI 的 `goal_check_node` 与自主 CLI Goal 仍被阻止；普通 checkpoint 不能伪造验收完成。原生交接还提供 `goal_handoff` 的 status/prepare/commit；候选获权前不能调用一般业务工具。继任运行的 `task_goal_id` 固化在运行记录中，不能靠解绑、重复交接或恢复清除。新任务默认工作目录为 `workspace/<goal_id>`；重叠目录或整个 workspace 不能充当独占的交接范围。保留已有目录的旧记录不会被自动改写。

任务工作文件写入在与实际文件操作相同的事务内检查确切 Goal、已确认计划、执行阶段、当前节点 attempt、run 与 fence。新运行必须重新完整读取节点并 claim，才能写任务目录；普通运行一旦显式绑定 Goal，也不能绕过这项业务门禁。自己的 Memory 和既有会议成员资格独立保留，但旧运行进入 draining 后不能继续写记忆或发布会议内容。

1. 用户/可信服务注册独立 Agent，并授权其加入某项目任务的会议厅；共同需求、约束与验收标准放在 brief。
2. Agent 调用 `agent_identity` 查看自身逻辑身份和运行，不枚举其他私有空间。
3. `agent_private` 在自己的分类路径执行 read/write/list/search。自己的任务、记忆与草稿不自动发布。
4. 委派请求用 `agent_meeting` 发布 message，明确任务范围与验收要求。接收方保留自己的内部目标和规划；父子关系不授予私域权限。
5. 用 artifact_content 发布独立成果快照，或发布简短 report。重用幂等 key 必须保持相同内容。
6. 其他获授权成员读取确切版本，用 review_artifact_id 与 review_version 提交自己的评审。更新产物后旧版本与旧评审仍保留。
7. 同一 Agent 换运行时，可信服务完成单一执行者切换。不同独立 Agent 继续工作时，依赖明确发布的 brief、报告与产物，不接管他人私目录。

普通单 Agent 的 REST 请求使用 `POST /api/tools/call`，Authorization 为该 Agent 的运行凭据；以下 JSON 是业务参数，不包含身份凭据。MCP 使用同名工具和相同参数对象。

```json
{"tool":"agent_private","params":{"action":"write","path":"workspace/result.txt","content":"已完成的结果"}}
```

后续覆盖必须携带上一响应的 `expected_sha256`。公开发布只上传明确选中的快照：

```json
{"tool":"agent_meeting","params":{"action":"publish","room_id":"<已授权房间>","kind":"artifact","content":"阶段结果说明","artifact_content":"已完成的结果","idempotency_key":"result-v1"}}
```

另一位获授权成员按返回的确切 artifact_id/version 评审，不填写作者：

```json
{"tool":"agent_meeting","params":{"action":"publish","room_id":"<已授权房间>","kind":"review","content":"检查结果与建议","review_artifact_id":"<产物ID>","review_version":1,"idempotency_key":"review-v1"}}
```

创建 Goal 后和任务限定运行中，上述文件路径应使用响应中的明确 task_directory（例如 `workspace/<goal_id>/result.txt`），且先完成确认、节点读取和 claim。自己的 Memory 及授权会议仍可使用；其他任务的工作目录不因此获权。

CLI 能力发现和现有思考档位代码保留，但能力发现成功不授予执行权限。当前尚无完成实测的逐 Agent OS 执行适配，所有独立 CLI/任意 shell/插件执行均被阻止；不得改用共享账户执行作为降级。

## 数据保护、迁移与回滚

本轮修改前的源码保护位于 `E:\wbecodex\backups\agent-isolation-20261023-01`，包括 1,240 个文件、原 Git 状态/HEAD 和 SHA256 清单。该仓库原先已有大量未提交改动；回滚必须按本轮文件差异恢复，不能重置整个工作区或覆盖另一个 Handoff agent 的并发修改。

管理迁移脚本为 `scripts/migrate_agent_isolation.py`。默认只读 plan 只输出白名单表计数和归属列元数据，不输出内容。非空 WAL/journal 会要求先停服与 checkpoint，避免漏掉未合并数据。

`--apply --service-stopped --database <绝对路径> --backup-dir <全新目录>` 先锁定写入、使用 SQLite 一致性备份并生成 SHA256 manifest，再登记旧表未分配清单。未知归属留在可信管理存储，不自动归主 Agent。重复执行校验原备份后返回 already_applied。新备份目录受限；不批量修改宿主目录或登录权限。

`--verify-backup <目录>` 验证备份完整性。恢复必须在服务停止后由可信管理者确认目标，先另外保护当前数据库，再按已验证备份恢复；脚本不自动覆盖数据库。代码降级会重新打开旧授权模型，不能在仍接入多 Agent 的情况下当作安全回滚。

## 验证记录

测试仅使用独立临时目录、合成内容和数据库。隔离测试与迁移日志在 `E:\wbecodex\validation\agent-isolation-20261023`；最新合并的原生 Handoff/可信宿主/任务范围回归在 `E:\wbecodex\validation\handoff-reliability-20261023`。以下为实际运行结果，各过滤组可能有重叠，不能相加当成独立测试总数。标注 `running 0 tests` 的旧过滤日志不计作验证。

* 存储：`cargo test -p webcodex-store --profile dogfood --lib agent_`，覆盖同账号身份分离、父子无继承、pending/旧run、原子切换、跨连接单写入者、会议版本/成员/作者、持久化、幂等和审计；原生交接以 `host_goal` 过滤复核。
* 文件：`cargo test -p webcodex --profile dogfood --lib agent_private_files -- --nocapture`，实际创建文件、硬链接、junction、符号链接并尝试越界。
* 服务：`cargo test -p webcodex --profile dogfood --lib agent_isolation_tests -- --nocapture`，使用生产认证与 handler，验证 HTTP、MCP、权限升级、旧接口和路径别名。其他最终过滤为 `agent_host_auth_tests`、`agent_handoff_access_tests`、`agent_goal_handoff::tests`、`agent_goal_adapter_tests`、`agent_handoff_control::tests`、`auth::`、`isolated_agent`、`route_metadata` 和 `host_goal`。编译后可对同一测试二进制直接使用这些过滤，不必每组重新构建。
* 迁移：`python -m unittest discover -s scripts -p test_migrate_agent_isolation.py -v`，实际备份/恢复合成数据，覆盖 WAL、幂等、路径链接与不泄漏。

| 验证组 | 实际结果 |
| --- | --- |
| Store 身份隔离 | 16 通过，0 失败，见 `store-final-recover-guard-isolation-57e25765.log` |
| Store 会议厅 | 9 通过，0 失败，见 `store-final-binary-agent-meeting-225229df.log` |
| Store `host_goal` | 101 通过，0 失败，含原生连续交接及通用 begin/activate/recover 不能绕过未完成交接；见 `store-final-recover-guard-rebuilt-548f4291.log` |
| 私域文件（含崩溃遗留 staging、大小写和 dotfile） | 15 通过，0 跳过；真实 Windows junction/symlink/hardlink 全部执行，最终见 `final-agent_private_files.log` |
| HTTP/MCP 与真实 127.0.0.1 服务 | 8 通过，0 失败；真实端口测试后确认关闭 |
| 私域 Goal adapter | 6 通过，0 失败，含 Exit 同事务暂停/解绑、失败回滚与 draining 重放拒绝 |
| 任务范围与恢复 | 10 通过，0 失败；含新 run 重读/claim 后真实写入、恢复后续作、原记忆/会议继承、其他任务拒绝和 scope 不能解绑洗掉 |
| 原生交接文件 adapter | 6 通过，0 失败；实际 broker 文件读取、内容哈希、prepare 与 commit 前再次核验 |
| 可信宿主 HTTP | 最终构建后 4 通过，0 失败；明确注册、准确 principal/window、默认拒绝、权限限制、丢响应与重启续查，含等待时会议禁写、成功后会议继承及固定拒绝审计；见 `native-host-http-final.log` |
| 窄管理调度入口 | 5 通过，0 失败；不向调度器投影私有正文或运行 secret |
| OAuth run 凭据升级回归 | 3 通过，0 失败 |
| 管理迁移/备份 | 13 通过，0 失败 |
| 认证扩大回归 | 113 通过，0 失败，0 跳过；保留真实管理 scope/Runner 边界并更新旧模型身份预期 |
| 路由元数据闭包 | 11 通过，0 失败；新增控制路由的声明与挂载一一对应 |
| 新凭据脱敏词表 | 1 通过，`wc_ar_` 继承现有 secret 检测规则 |
| 原 Host Goal 运行时 | 最终 92 通过，0 失败，见 `final-host_goal.log` |

最后一次服务端构建通过后，对同一二进制重新运行了表中全部 Server 相关过滤；`final-*.log` 分别记录 113、15、10、8、6、6、5、3、11、92 项通过。可信宿主 4 项记录在 `native-host-http-final.log`。没有把较早的零测试输出或后来被替代的构建结果当成最终证据。

首轮文件测试暴露 Windows 路径式原子 rename 与目录保护句柄冲突，已改为相对于已验证目录句柄的原子发布；短名称原生缓冲与 junction 测试路径问题也已修复后重跑。HTTP 首轮一个错误码预期与新的精确安全错误码不一致，已保持“拒绝、无任务写入、无内容泄漏”断言并修正预期后通过。认证首轮 14 项失败涉及已废止的匿名/共享模型授权预期及缺失认证挑战头；更新契约测试、修复协议响应后全部通过。路由闭包测试遗漏的一处旧数量常量已随新增路由同步。没有通过跳过真实链接测试或放开权限获取通过结果。

独立复查另外修复了 private Goal 的 Exit：原适配顺序可能在 draining 时先提交 Pause 再解绑失败；现在 Exit 的停止阶段检查在重放和任何写入前执行，暂停、fence、检查点与仅该 owner/run/Goal 的解绑同事务完成，注入解绑失败会整体回滚。

最终接线复查还收紧了两条路径：原生交接尚未完成时，通用 begin/activate/recover 均不能绕过 ready、替换候选或发出未限定任务的新运行；宿主入口关闭账号级审计后，未知窗口、权限不足和工具拒绝仍需写入固定安全事件，不记录原始窗口、业务参数或私有正文。已有 scope 的可信恢复仍可在已提交交接后继续同一任务，并要求新的节点读取与 claim。

最终编译曾发现审计闭包参数不满足安全审计 API 的静态字符串要求（E0521），已将其精确声明为 `&'static str`，保留固定错误码边界后重新编译并通过宿主 HTTP 测试；没有通过放宽审计接口解决。

扩展检查还记录到工具契约包 `cargo test -p webcodex-tool-contracts --lib` 的 114 通过、2 失败：`reliability_tool_descriptions_are_selection_dense_and_keep_lifecycle_contracts` 与 `tool_specs_describe_default_coding_loop_preferences`，涉及旧 `run_process` 描述缺少期望文字（`same execution`、权限提示）。该描述和对应断言在本轮修改前的源码备份中已存在；没有删断言、跳过或放宽权限掩盖失败。它们不计为通过，完整工作区测试也未宣称通过。

| 用户验收项 | 已验证证据或限制 |
| --- | --- |
| 1：自己任务/记忆/文件可读写 | 文件与 HTTP 实际读写、CAS、列表、搜索通过 |
| 2–3：其他 Agent、父子私域不可访问 | 同用户/同项目/父子/跨项目的文件与服务负例通过 |
| 4：身份/Goal/parent/旧接口冒用 | 严格输入字段、真实凭据与 own Goal 查询、旧 API 负例通过 |
| 5：shell/CLI/脚本/链接绕过 | 实际链接与路径负例通过；执行请求在启动前拒绝，真实网络请求未生成指定 sentinel。未声称任意 OS 进程沙箱已通过 |
| 6–8：发布/评审/版本/作者/成员 | 独立快照、确切版本、不能改他人产物或伪造作者、跨房间/项目拒绝通过 |
| 9：同身份续跑与旧运行失效 | pending、原子切换、跨连接单执行者、旧凭据/旧 binding 拒绝、新运行读取原记忆通过。真实网页自动交接未验证且被阻止 |
| 10：CLI 不继承历史 | 当前没有获准启动的独立 CLI，未发生自动恢复；实际模型 CLI 历史隔离执行未验证 |
| 11：CLI 中断后的公开成果续作 | 会议成果独立持久化并可由成员读取/评审已验证；真实 CLI 中断场景因执行被禁未验证，不计为通过 |
| 12：旧数据未知归属 | 合成数据库迁移/备份证明保留原 payload、不自动分配、不发布会议厅 |
| 13：可追溯审计且无私文 | 固定动作/错误码、Agent/run 标识审计；旧 trace 与账号事件不保留隔离运行私文；测试不含私文泄漏 |
| 14：无真实隔离的 CLI 明确阻止 | MCP、HTTP、真实回环网络均验证 not_started 拒绝，无静默宿主执行降级 |

主要改动文件：store 的 `agent_isolation.rs`、`agent_meeting.rs`、`agent_task_scope.rs`、`agent_host_window.rs` 及测试；Server 的 `agent_isolation.rs`、`agent_private_files.rs`、`agent_goal_adapter.rs`、`agent_handoff_access.rs`、`agent_goal_handoff.rs`、`agent_handoff_control.rs`、`agent_host_auth.rs`、`mcp/agent_host.rs` 及测试；认证/context/token/middleware、OAuth授权、HTTP/MCP/kernel/dispatch、trace/audit、核心凭据脱敏与路由元数据；已有 HostGoal store 的 agent_run 事务准入与私有读取；管理迁移脚本及测试。其余相关测试 fixture 补充了新的身份字段。与另一个 agent 的 Handoff 状态机/网页调度改动分工协作，未重置或覆盖既有未提交工作。

操作状态：修改留在原工作区，未提交、推送、发布、部署或重启服务；生产数据未迁移。实际运行的已安装服务不因源码修改而自动获得这些保护。验证过程中生成的构建/日志保留在专用 validation 目录；只清理本轮迁移单测生成且已确认无用的 Python 字节码，没有清理历史工作、测试证据或另一个 Agent 的文件。Unix broker 实现尚未在 Linux 实机运行，本轮实际文件边界证据限 Windows。

区分状态：源码实现不等于部署；存储事务测试不等于浏览器自动接续；真实文件 broker 检查不等于 OS 沙箱；明确拒绝不安全 CLI 是已落实的保护，不能写成 CLI 隔离执行已通过。
