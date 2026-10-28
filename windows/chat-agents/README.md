# Windows 网页子任务 provider

入口为 `chat_agents.py`，使用 `webcodex-plugin-v1` JSONL 协议。完整安装、依赖、Host carrier 和离线检查见 [相邻 handoff 说明](../chat-handoff/README.md)。保持 `chat-handoff` 与 `subagent-pilot` 相邻。

当前提供八个工具：`discover_chat_profiles`、`read_chat_profiles`、`spawn_chat_task`、`read_chat_task`、`collect_chat_task`、`cancel_chat_task`、`bind_chat_task_goal`、`discover_cli_options`。准确参数以 `native_server.py` 中目录/schema 为准。

新任务可以省略 `thinking_profile_id`：不查询目录、不读页面强度、不生成默认档位 ID，直接保持页面原设置。显式 ID 只请求一次可选 UI 调整；档位缺失、过期或调整失败不会撤销合法任务或触发重发。相同任务保持 parent/idempotency key；结果未知时不换 key 重发。长子任务仍使用自己的真实 Host Goal 和已核验绑定。

Host 队列调度不自动发现或刷新思考档位，也不等待目录。需要查看选项时显式调用 `discover_chat_profiles`；`read_chat_profiles` 只读缓存。STOP/PAUSE、连接归属、身份与发送前校验仍然有效。

Host 登记的隔离任务不使用普通网页模式的 `bind_chat_task_goal`。任务首次创建 Goal 时，Server 在同一事务中登记不可替换的 Task/Goal 关联；`agent_identity.assignment_goal_id` 返回该关联，省略 `goal_bootstrap.goal_id` 只恢复此关联，不查找最近 Goal。`task_goal_id` 是交接后的执行范围字段，初始 run 中为空不代表任务未关联 Goal。普通工作不需要额外的本地追踪登记；交接仍使用原有身份、运行代际和执行权校验。

任务、目录缓存与回执由目标机器在 `runtime` 创建，不属于可共享源码。`chat-agents.provider.example.toml` 只提供无凭据路径模板；Desktop 管理自动队列的私有控制参数与生命周期。

`team_agent_start` 的公开参数和内部链路均支持省略档位。现有数据库以空字符串存储“未请求调整”，不把它当作档位 ID；旧请求的 `default` 仍兼容且不读回页面强度。CLI 模型选择规则独立保留。

Host 用 `thinking_adjustment.status` 分别记录 `not_requested`、`not_adjusted`、`applied`，设置错误单独记录。只有实际确认成功才提供 `applied_thinking_profile`；任务是否发送仍以发送回执判断。调整发生在发送前，不声称之后修改页面参数会追溯改变已发送请求。

CLI 计划中的 effort 同样可省略或为 null：实际命令不传强度覆盖参数，回执 effort=null 表示由 CLI/模型采用默认值，不能宣称为 high。显式 effort 仍验证能力；project_policy 仍可显式选取项目保存值。

隔离团队通过 `team_collaboration` 发布和读取公开协作事件。身份与原团队范围由 Server 推导；读取后确认已收到的 sequence，处理交付后发布新事件，需要等待时登记 reason/next_step。事件和收件人记录在同一 SQLite 事务提交，先发布后等待不会丢失；公开任务完成也在其事务内生成带框架来源的通知。状态分别显示任务生命周期和有时效的宿主执行观察。

原对话通知由现有 `AutomaticDispatch` 和同一个浏览器 worker 处理，不创建新任务或身份。普通初始成员从原 enrollment 发送回执定位；继任 run 和没有初始派发回执的主控需要精确 Host 路由登记，不能猜测最近对话。调度按任务轮转，保留 STOP/PAUSE，忙碌对话和用户草稿均不发送。发送前持久化一次性 fence；未知投递仅核对原消息，不换标识重发。发送前明确失败最多尝试四次，历史不删除。相同观察按内容去重保存。框架能力恢复使用受信管理入口生成 `origin=framework` 事件，不能伪称用户新增授权。

上述源码能力仍需部署并核验实际 Host 路由和浏览器行为；夹具测试不等同于真实业务自动续跑验收。

### Public continuation status and original-chat registration

`team_collaboration action=status` includes same-team public activity, wait reasons, next steps and last public progress. It does not read private Goal/Memory. Task `active` is not model liveness.

Initial member routes use retained Host delivery receipts. Managers without such receipts, and successor runs after formal handoff, can use the trusted local operator entry point `scripts/register_team_continuation_route.py`. Its registration JSON contains only `project`, exact `target`, and `route`: retained member routes use `url`, `anchor_launch_id`, `anchor_task_id`; an ordinary manager chat may instead use `url` and `window_source="openai-session"`, whose conversation ID must hash to the existing Server window binding. It validates the current Server binding before and after observing only URL, anchor count and visibility. It sends no message and reads no response body. A run's route is immutable; a new run cannot reuse a registered predecessor conversation. The entry point is absent from model tools. Every subsequent send still verifies the original chat and current run fence. Successor runs can also derive their route automatically from an observed formal handoff receipt, only when its Agent and conversation hash match the current target. This reads result metadata, never the private recovery prompt or checkpoint.

### Original pending enrollment recovery

The automatic coordinator may renew an expired member attach lease only after observing the original request in its recorded original conversation, still as the last user message. It then runs the existing exact-app approval guard. It does not create a chat, send another business prompt, change the enrollment secret hash, replace an Agent/Task/Goal, or erase delivery_unknown. The Server requires the exact original queue request, current manager authority, pending unclaimed run, no prior assignment/window, and no cancellation/closure. Renewal history is immutable; the same recovery key never extends the lease again, and at most three renewals are admitted. STOP/PAUSE and changed user messages prevent input. Host outcome records prevent repeated work under unchanged scheduler/manager generations.

Old enrolled prompts may call `bind_chat_task_goal`; the isolated broker now provides a read-only compatibility projection of their existing server association. A different Goal is rejected, missing association is not fabricated, and no execution/handoff permission is granted. New prompts continue to prefer `goal_bootstrap` directly. The ordinary non-enrolled provider operation remains separate.

An explicit new human recovery instruction can supersede an already-existing later user message. The local operator may create an immutable `enrollment-recovery-permits/<original-request-id>.json` with exact `original_request_id`, `queue_id`, `project`, `manager_run_id`, a fresh 32-hex `authorization_id`, bounded `source` recording that instruction, and `allow_prior_launch: true`. This is not a model tool or an enrollment grant. The permit adds one distinct recovery attempt without resetting scheduler generations or deleting prior outcomes. It retains the original unique message anchor, observed total user-message count, exact app/card checks, current enrollment/run authorization, STOP/PAUSE and the existing server renewal limit. A further user message during approval invalidates the observation.

For an explicitly authorized original-chat recovery notice, add `notify_original: true` to a fresh operator authorization record. Preserve the previous authorization file before replacing the active selection, use a new authorization ID, and retain all prior outcomes. The normal automatic coordinator renews only an eligible original pending enrollment, then sends one framework capability-restoration notice referencing the original request. It never copies the grant or business prompt. A durable exclusive input fence records the exact target, source authorization and message hash before any typing; any uncertain/fenced replay is observe-only. The notification uses the existing shared browser worker and original chat; draft, busy, live enrollment, current manager, STOP/PAUSE, message-count and exact send-target checks still apply. Successful send does not attest successful attach or business execution.

An unknown fenced notice may receive one automatic reconciliation observation under the same scheduler generation and original authorization. It never sends again. If its original attach lease expired, the same Server renewal cap still applies to a separate stable reconciliation key. An approval fence precedes handling the observed notice, so an uncertain approval click is not repeated. Both old and current ChatGPT user-message markers are observed and nested wrappers are deduplicated; receipt reconciliation reads identity/count metadata only.
