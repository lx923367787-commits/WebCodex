# Agent 调用与任务接管

## 默认：直接调用已安装 CLI

普通 Agent CLI 不需要注册 ACP Provider，也不需要先创建 Durable Agent 或
AgentTask。使用现有项目和进程权限即可：

1. 根据该 CLI 当前帮助确认非交互参数、模型参数和输出格式。
2. 用 run_process(project, executable, args, cwd, timeout_secs) 直接执行。
   单次 Agent 调用默认等待 300 秒；长任务调用使用 timeout_secs=900，最大 3600 秒。
   需要为本次调用设置代理环境或使用脚本时，用 run_script 并传入相同预算。
   这些秒数只限制本次外部调用，不限制 WebCodex 整个工作流或后续工作。
3. 如果返回 job_id，用 observe_jobs、job_status、job_log 观察同一个 Job，
   不要因为一次请求结束或超时而再启动一次。
4. 只有工作确实需要跨 Runner 退出、升级或重启继续运行时，才选择
   run_detached_process（单次调用默认 900 秒），并保留其幂等键与 Job ID。

例如当前 CLI 支持相应参数时，可直接传入：

~~~json
{
  "tool": "run_process",
  "params": {
    "project": "agent:<client_id>:<project_id>",
    "executable": "C:/path/to/agent.exe",
    "args": ["-p", "具体任务和验收条件", "--output-format", "json"],
    "timeout_secs": 300,
    "sync_wait_secs": 1
  }
}
~~~

这不是所有 CLI 的固定协议。可执行文件和参数直接交给既有进程执行层；
不增加 Agent 专用注册表、启动协议或调度器。各 CLI 的 stdout/JSON 保持各自格式，
由协调者读取。退出码 0 只代表进程成功，不代表任务已经满足验收条件。

## 代理、模型和身份

- Agent 是执行者；模型通过该 CLI 已验证的参数选择；ACP 是可选传输方式。
- 不从品牌名推断模型，不假设登录或 CLI 参数永久有效。
- Windows 用户级环境变量后来变化，不会自动更新已运行父进程的环境。
  需要代理时，显式设置本次进程的 HTTP_PROXY、HTTPS_PROXY、NO_PROXY。
- 可以在当前 PowerShell 调用内读取已有用户级代理并赋给当前进程，再执行 CLI；
  不需要修改全局设置或注册 ACP Provider。
- 保持正常项目、进程和后台 Job 权限边界，不通过换入口绕过权限。

## 接管复用现有记录

保留原始目标、验收条件、Job ID、工作区文件、stdout/stderr 和测试结果。
需要跨会话管理目标时，复用已有 Goal 或 Workflow Session 的记录与交接能力，
不把它们变成每次 CLI 调用的必填前置流程。

接管前观察或取消原 Job，确认旧进程已停止。协调者必须避免同一工作区重叠写入。
超时、失联和不确定结果都不是安全重试凭据；不能确认时保存状态等待核对。
新执行者先检查已有文件和结果，再继续剩余工作，最后验证原始验收条件。
不要求失败 Agent 再生成总结，不自动清理、回滚或覆盖已有成果。

普通进程/Job 路径继续使用现有超时、并发、取消、日志与重放语义。
它不会自动获得 ACP 专用的工作区互斥或 AgentTask 绑定；需要这些专用语义时，
显式选择对应机制。需要 Runner 重启后继续执行，应使用 detached Job。

## 团队 CLI 的独立 GPT 保底

显式团队调用使用 `team_policy_*`、`team_plan_*` 与 `team_cli_start/observe`。
计划引用同一所有者、同一项目下的真实 AgentTask；Server 冻结常规成员、CLI
调用槽与策略版本，`team_cli_start` 消费原槽并调用现有 typed `run_process`。
普通直接 `run_process` 不会凭空获得根任务，也不自动加入保底调度。

`subagents.min/max` 和 `cli_per_agent.min/max` 仍约束整份常规计划的数量，
不是并发数。每个根任务另有一个 GPT 恢复槽：不占常规最大人数、不用于满足
最低人数，失败 CLI 的调用记录不退款。恢复助手不得再派外部 Agent CLI 或子助手。

`team_cli_start.fallback_thinking_profile_id` 可省略，保持网页当前设置；
目录查询、缺失或过期不应阻止合法派发。显式选择是可选页面调整，实际结果
单独记入 Host 回执，不能把未确认的设置记为已应用。选择随执行尝试固定。

已有零 CLI 计划可通过本机受信管理入口 `team_cli_round_register` 登记追加轮次。
原计划、原成员身份和原预算记录不变；新轮次的精确调用槽是独立的操作员授权，
仍受项目策略、经理总预算、单成员累计槽上限和版本校验约束。未分配新调用的成员
保留空调用列表；成员只能使用自己的已登记槽。未知或未停止的旧执行不能据此重启。

只有真实绑定的 Job 与可信故障分类一致时才能预约恢复。Runner 确认完整进程树
停止后的超时、已分类的启动故障、额度/网络等结构化错误可以进入保底；普通非零
退出、输出沉默、目录刷新失败、用户取消、权限/隔离拒绝不单独触发。`lost`、
`recovering`、`stop_requested`、`outcome_unknown` 和缺失证据均不能当停止证明。

`wc_cli_fallbacks` 保存独立恢复记录、来源 Job、经理档位与派发 fence。
同一源尝试只建一个逻辑恢复；同根并行竞争只能占用一个槽。已接受或结果未知
保留占槽；只有明确未发送的 busy 可以有限重排，同一逻辑任务的聊天幂等键不变。
根任务取消原子标记所有恢复记录，不受展示页大小限制。

可选档位调整失败时记录 `not_adjusted`，保持原有聊天幂等键与任务身份。
已有请求、未知发送或未知记录回执仍保留原派发状态，不能通过刷新目录获得重发权限。

保底和普通 GPT 候选发现均按固定高水位分页，每页最多 100 条。游标绑定当前
所有者、项目和查询范围，不授予执行权限；每轮高水位之后的新记录在下一轮纳入。
共享 worker 处理完整页后分别持久保存两个队列的游标，重启可续扫；未完成的页
可以重读，原有派发 fence 防止重复发送。一个队列或候选观察失败不会阻断其他
队列或后续根任务的扫描，既有保底优先级保持不变。

宿主沿用既有“新建 ChatGPT 对话 → @webcodex → 任务描述”流程，读取已有成果
后续接。保底控制操作复用 `/api/admin/agent-isolation` 的显式 Bootstrap 门禁，
使用 `cli_fallback_list/acquire/read/record/resolve` 动作；普通模型不能领取该权限。
控制凭据永不进入聊天任务、结果或模型参数。

本地入口为 `chat-handoff/cli_fallback_worker.py`，复用现有独立控制连接配置
`WEBCODEX_HANDOFF_CONTROL_ENDPOINT`、显式 token 或 token file、owner user 与 project。
它遵守原 STOP/PAUSE 和 generation，并复用已登录的 `chat-agents` 发送与只读收集。
启动前需要部署配套 Server、只读档位工具及宿主文件；源码测试不等于服务已启用。

`response_observed` 只是候选回答，不代表模型或业务的可信终态；它不会释放
恢复槽，也不会把任务标成成功。可信协调者核实结束或确实未启动后，才通过
control `resolve` 记录证据。未知发送不能换 key 重发，本地取消也不能冒充远端停止。
恢复不绕过项目授权、Runner 容量或既有私域执行限制。

## ACP 仅为可选专用路径

确实需要 ACP 协议能力时，继续使用已配置的 ACP Provider，以及
coding_agent_start/observe/cancel。显式 AgentTask 也保留其 ACP Run 绑定、
幂等分发、租约围栏、执行证据和 handoff_brief。

这些接口仍需相应 ACP/通信权限；它们不是普通 CLI 调用的推荐入口或必经步骤。
此前新增的 json_cli-inside-ACP 适配已移除；旧 json_cli 配置会被拒绝，
避免把非 ACP 命令误当作 ACP 启动。已有真正的 ACP 配置不变。

原 ACP 接管扩展中的有界证据、未知结果保护、已确认失败后的显式恢复和存储迁移
仍保留，详细协议边界见 [ACP 合同](../agent/acp-coding-agent-run.md)。
不把两条执行路径的状态、权限或恢复保证混为一谈。
