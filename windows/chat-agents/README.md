# Windows 网页子任务 provider

入口为 `chat_agents.py`，使用 `webcodex-plugin-v1` JSONL 协议。完整安装、依赖、Host carrier 和离线检查见 [相邻 handoff 说明](../chat-handoff/README.md)。保持 `chat-handoff` 与 `subagent-pilot` 相邻。

当前提供八个工具：`discover_chat_profiles`、`read_chat_profiles`、`spawn_chat_task`、`read_chat_task`、`collect_chat_task`、`cancel_chat_task`、`bind_chat_task_goal`、`discover_cli_options`。准确参数以 `native_server.py` 中目录/schema 为准。

新任务先发现真实档位并显式传入 `thinking_profile_id`；`read_chat_profiles` 只读缓存。相同任务保持 parent/idempotency key；结果未知时不换 key 重发。长子任务使用自己的真实 Host Goal 并登记原档位。CLI 发现只读已安装程序的帮助/版本，不运行模型任务，也不证明账号有相应模型额度。

已激活且处于 running 的 Host 在成功核验队列后维护档位目录：缺失、过期或距离一小时有效期不足五分钟时，空闲浏览器工作线程执行真实菜单发现。正常任务优先，STOP/PAUSE 和连接归属检查不变；失败后至少等待五分钟再尝试。刷新期间保留尚有效的旧目录，只有真实发现成功才更新观察时间；失败不续期，过期目录仍拒绝派发。初始化、工具枚举和只读查询不会启动发现。

任务、目录缓存与回执由目标机器在 `runtime` 创建，不属于可共享源码。`chat-agents.provider.example.toml` 只提供无凭据路径模板；Desktop 管理自动队列的私有控制参数与生命周期。
