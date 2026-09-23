# 目标模式：执行、检查、反馈循环

本文说明后台 ACP Provider 目标模式。ChatGPT 网页中“讨论 → 保存本地计划 → 用户确认 → 分节点执行”的新流程见 [网页目标模式操作说明](HOST_GOAL_MODE.zh-CN.md)。

目标模式已通过本地专项测试、隔离进程实验和真实 Codex 执行与审稿验收，详见[验收记录](GOAL_MODE_ACCEPTANCE.zh-CN.md)。使用时需要包含本功能的 Server/Runner 构建、已配置的 ACP Provider 和相应权限；本次源码交付没有替换已有安装或部署服务。

目标模式在服务器中保存目标及验收条件，执行一轮工作，检查实际产物，再把失败证据交给下一轮。模型结束本轮回复只表示执行轮次结束；只有验收条件通过且最终产物检查满足要求，目标才进入 `completed`。关闭 Runtime Console 页面不会把执行循环变成浏览器任务。

## 使用入口

1. 打开 Runtime Console，以已有运行时凭据连接。
2. 在“项目与会话”中选择已授权的项目。
3. 进入“运行时与 Agent → 目标模式”。页面沿用当前项目。
4. 填写已配置的执行 Provider ID、目标描述、验收要求和项目内的相对产物路径，再设置轮数和总时长上限。
5. 点击“启动目标”，查看每轮检查证据和反馈。页面只观察服务器循环，不自行调用执行工具推进下一轮。

验收要求每行生成一项 `review` 条件，产物路径每行生成一项 `file_exists` 条件。审稿 Provider 留空时，表单会显式使用执行 Provider 的 ID 发起独立审稿运行；这不表示两个运行在权限上已作只读隔离。高级验收 JSON 可增加文件内容检查或命令检查。

轮次面板默认显示最新一轮的检查、证据和错误，可用“上一轮／下一轮”查看历史；自动刷新保持已选择的历史轮次。停止目标会请求取消当前执行和检查；已经完成或取消的目标不能恢复。暂停或受阻的目标可请求恢复，但原验收条件、时间预算和已绑定的执行身份不会因此重置。

每个项目同一时刻只能有一个已经启动、尚未完成或取消的目标；`paused` 和 `blocked` 仍占用该位置，须恢复该目标或将其取消。新建但尚未启动的排队记录可以通过列表查看，并在项目可用后显式恢复；当前实现不提供自动后台队列调度。

取消请求先保存 `cancel_requested: true` 并暂停推进。只有真实运行与检查 Job 均确认终止，状态才变为 `cancelled` 并释放项目位置；仍在执行或结果未知时保持暂停和占位。页面显示“正在确认执行已停止；可再次点击停止核对状态”，此时可再次点击停止以核对，不能恢复执行，也不会允许同项目的新目标并行启动。这是取消结果的核对，不是额外审批。

## 工具调用

模型工具为 `goal_start`、`goal_read`、`goal_list`、`goal_cancel` 和 `goal_resume`。HTTP 调用沿用既有 `POST /api/tools/call` 的认证、权限和工具调度流程：

```json
{
  "tool": "goal_start",
  "params": {
    "project": "<已注册且已授权的项目 ID>",
    "provider_id": "<已配置的 ACP Provider ID>",
    "objective": "生成项目测试结果报告，并说明仍未解决的失败。",
    "criteria": [
      {"kind": "file_exists", "id": "report", "description": "报告已生成", "path": "output/report.md"},
      {"kind": "file_contains", "id": "summary", "description": "报告包含测试结果", "path": "output/report.md", "text": "测试结果"}
    ],
    "artifact_paths": ["output/report.md"],
    "limits": {"max_rounds": 5, "total_timeout_secs": 1800, "max_no_progress_rounds": 3, "run_timeout_secs": 300},
    "idempotency_key": "<由调用方生成且用于此次启动的唯一键>"
  }
}
```

启动响应包含 `{ "goal": record, "replayed": false }`。如果启动响应丢失，应以相同参数和相同 `idempotency_key` 重试，以恢复同一个目标。不要因超时改用新键重新派发。

| 工具 | 主要参数 | 结果 |
|---|---|---|
| `goal_start` | `project`、`provider_id`、`objective`、`criteria`、`artifact_paths`、`idempotency_key`；可选 `reviewer_provider_id`、`limits`、`config` | 已保存目标与是否为重放 |
| `goal_list` | 可选 `project`、`limit` | 有界目标摘要，不携带完整执行输出 |
| `goal_read` | `goal_id`；可选 `round_offset`、`round_limit` | 轮次分页、证据、状态；额外返回分页元数据及 `delivery_freshness`、`delivery_ready` |
| `goal_cancel` | `goal_id` | 当前目标；请求停止当前执行及检查 |
| `goal_resume` | `goal_id` | 当前目标；显式恢复并核对既有运行身份 |

`goal_read` 默认只返回最新一轮，`round_offset` 从零开始，`round_limit` 最多为 5。顶层 `total_rounds`、`round_offset`、`next_round_offset` 和 `has_more` 说明历史分页范围；`goal.rounds` 仅包含当页。启动、恢复和停止响应同样仅携带最新一轮，完整历史须分页读取。

`config` 仅用于 Provider 已公开且允许覆盖的 ACP 配置项，不用于传递密钥。凭据继续由现有 Runner/Provider 环境映射负责。

首次真实模型 smoke 可显式传入 `"config": {"reasoning_effort": "low"}`，先验证协议和完整循环，避免沿用账户的 `ultra` 设置而超出短时测试预算。前提是 Provider 实际公布该选项，且 Runner 的 `allowed_config_options` 包含 `reasoning_effort`；不支持时应保留明确错误。此设置只属于该次 smoke 请求，不改变模型或产品默认值。Windows 验收脚本提供对应的 `--reasoning-effort low` 参数。

启动和恢复需要明确的 `coding_agent:run`、`project:read`、`project:write`、`job:run` 和 `job:detach` 权限。`job:detach` 用于持久化命令验收，不会自动扩大旧凭据的权限；缺少权限时启动会被拒绝。

## 验收条件和预算

所有条件都需要唯一 `id` 和非空 `description`。目标需要至少一项条件和一个最终产物路径。

| `kind` | 附加字段 | 检查依据 |
|---|---|---|
| `file_exists` | `path` | 项目文件存在且能取得内容哈希 |
| `file_contains` | `path`、`text` | 分页检查规范文本内容，支持跨页匹配；每页核对同一文件哈希，读至命中或文件末尾 |
| `command` | `program`、`args`、`timeout_secs`；可选 `cwd` | 结构化进程和持久 Job 的最终状态、退出码与日志 |
| `review` | 无 | 审稿运行按精确条件 ID 返回结构化判断和非空证据；必须指定 `reviewer_provider_id` |

路径必须是授权项目内的相对路径。命令使用独立的程序名和参数数组，不依赖拼接的 shell 字符串。命令验收受当前运行时的执行权限约束。

| 限制 | 默认值 | 范围 |
|---|---:|---:|
| 最大轮数 `max_rounds` | 5 | 1–50 |
| 总时长 `total_timeout_secs` | 1800 秒 | 1–86400 秒 |
| 连续无进展轮数 `max_no_progress_rounds` | 3 | 1–50 |
| 单次 Provider 运行时限 `run_timeout_secs` | 300 秒 | 1–3600 秒 |
| 验收条件数 | — | 1–32 |
| 最终产物数 | — | 1–64 |

完成后读取目标仍会检查当前产物哈希。`delivery_freshness: "current"` 且 `delivery_ready: true` 表示最近一次读取时的产物仍与验收记录匹配。`changed` 或 `unavailable` 不抹去历史完成事实，但不能把当前文件标为已验证可交付。后续文件修改仍可能使此前读取结果过时。

## 验收映射

下表列出原始验收要求；对应通过结果、实验报告和证据范围见[验收记录](GOAL_MODE_ACCEPTANCE.zh-CN.md)。

| 编号 | 要求 | 应验证的场景 |
|---|---|---|
| 1 | 首轮不合格自动进入第二轮 | 第一轮留下可复现的失败；第二轮自动收到对应证据并继续工作 |
| 2 | 模型报完成但检查失败时仍未完成 | Provider 正常结束并声称完成，但任意必需验收失败时，目标不能变为完成 |
| 3 | 所有必需项通过后停止并生成交付记录 | 检查通过且最终产物匹配时保存完成状态、完整证据与产物哈希，不再派发后续轮次 |
| 4 | 检查器异常或无证据不假成功 | 命令异常、审稿返回缺项或空证据、读取失败时不能完成目标 |
| 5 | 取消、预算耗尽或无进展时停止 | 在执行或检查时取消，或达到轮数、总时长、无进展上限后停止推进；恢复不重置原预算 |
| 6 | 重启或重复事件不重复派发 | 在派发和持久化边界模拟重启与重复请求；使用精确运行/Job 身份核对，未知结果不得盲目重发 |
| 7 | 检查后产物变更时旧结论不可交付 | 检查期间、恢复前或完成后修改产物；旧审稿/命令结果不得证明新产物通过，交付读取不能给出就绪状态 |

前端的交付就绪提示、创建幂等键、凭据/项目切换后的旧响应丢弃，以及轮次版本防回退有独立测试。运行时的完整验收应以对应构建下执行的专项测试结果为准。
