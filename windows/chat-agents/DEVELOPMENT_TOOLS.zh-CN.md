# 开发辅助工具

`activate_provider.py` 保留原有 provider 检查、热加载和临时管理权限撤销流程，供需要继续维护这部分代码的开发者使用。它不是启动脚本，构建和 `setup-providers.ps1` 不会执行它，也不修改 Runner TOML。日常初始化使用正式 Desktop 与安装说明。

原机的固定用户名、Runner ID、端口、配置变更记录和用户目录已改为命令行参数。先完成目标机器自己的 Runner provider 配置并确认当前无任务，再读取本机 `desktop-state.json` 中的 Runner ID 与 Server origin、记录已审查的 Runner TOML 的 SHA-256，传入这些值。脚本要求 origin 是本机 HTTP 地址；仍会精确核验 Desktop 目标、Runner ID、配置哈希和空闲状态。

```powershell
python windows/chat-agents/activate_provider.py --help
# 显式执行时需传入本机实际值：
# --runner <Runner-ID> --origin <http://127.0.0.1:端口> --username <本机账户>
# --expected-config-sha256 <已审查配置的SHA256> --output-directory <新的本地报告目录>
# 可选 --desktop-state <目标Desktop状态文件>
```

执行会读取当前 Desktop 引用的本地凭据，在内存中创建十分钟 `plugin:manage` 权限，检查 `chat-agents` 与 `edge-handoff`，热加载 provider，并在结束时撤销临时权限。报告只记录状态与权限记录 ID，不保存凭据。报告目录必须是新的本地目录，建议放在仓库的 `ignore/` 下。此流程会访问本机 Server，只有操作者明确要求激活时才执行。

本次源码迁移只进行了语法、参数帮助与隔离的 mock 检查，没有执行真实激活、网页操作或服务变更。
