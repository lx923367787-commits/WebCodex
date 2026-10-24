# Windows 聊天工具安装

完整配置说明、依赖版本、Host carrier、启动边界和离线验证见 [chat-handoff/README.md](../chat-handoff/README.md)。

从仓库根目录准备源码 checkout 内的工具：

```powershell
pwsh -File windows/setup-providers.ps1 -Python python
```

若已通过 `windows/build.ps1` 生成便携应用目录，将 `ToolRoot` 指向包含 `chat-handoff`、`chat-agents`、`subagent-pilot`、`goal-provider` 的输出目录：

```powershell
pwsh -File windows/setup-providers.ps1 `
  -ToolRoot 'C:\WebCodexBuild\WebCodex Desktop' `
  -Python 'C:\Python311\python.exe'
```

脚本要求 Windows、Python 3.11、Node/npm；在指定 ToolRoot 创建 `.venv`，按完整 Python 锁文件安装依赖，对两个 npm 目录执行 `npm ci`，在 `provider-config` 生成含实际绝对路径的 TOML 模板。再次运行会按锁文件更新该工具环境并重新生成这些模板。此脚本会访问依赖仓库，但不会运行模型、访问网页、读取现有凭据、修改 Runner 配置或启动服务。

完成后按上述完整说明把需要的模板合并进目标 Runner，初始化本机账号和扩展授权，再用 Desktop 正式生命周期启用 Host 自动调度。不要复制旧电脑的 runtime、凭据、carrier、数据库或登录资料。构建清单记录的是构建时文件；后续安装的第三方依赖由各自锁文件确定。
