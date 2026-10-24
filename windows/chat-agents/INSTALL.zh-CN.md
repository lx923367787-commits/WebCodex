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

脚本要求 Windows、Python 3.11、Node/npm；在指定 ToolRoot 创建 `.venv`，按完整 Python 锁文件安装依赖，对两个 npm 目录执行 `npm ci`，再调用独立的 `configure-providers.ps1`，在 `provider-config` 生成含实际绝对路径的 TOML 模板。再次运行会按锁文件更新该工具环境并重新生成这些模板。此脚本默认会访问依赖仓库，但不会运行模型、访问网页、读取现有凭据、修改 Runner 配置或启动服务。

`.venv` 不能直接搬到另一台电脑，目标机应重新创建。已附带 `provider-python`、Node 和 npm 依赖的 Windows 离线运行包可直接运行 `configure-providers.ps1`，仅重新生成目标路径，不联网、不重新安装依赖。`setup-providers.ps1 -Offline -Wheelhouse <目录>` 则支持在已有 npm 依赖的前提下，从本地 wheelhouse 创建全新 venv。完整步骤与目录布局见 [PORTABLE.zh-CN.md](../PORTABLE.zh-CN.md)。

完成后按上述完整说明把需要的模板合并进目标 Runner，初始化本机账号和扩展授权，再用 Desktop 正式生命周期启用 Host 自动调度。不要复制旧电脑的 runtime、凭据、carrier、数据库或登录资料。构建清单记录的是构建时文件；后续安装的第三方依赖由各自锁文件确定。
