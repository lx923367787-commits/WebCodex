# Windows Codex ACP 入口

[外部工具安装与账号边界](../../docs/EXTERNAL_TOOLS.md#简体中文)同样适用于适配器嵌套的 Codex；锁文件描述依赖版本，不代表用户账号、模型权限或 Runner 隔离执行资源已就绪。

这是可选的目标机模型执行器；WebCodex 分发包只包含本目录源码与锁文件，不包含安装后的 `node_modules`。默认 `setup-providers.ps1` 不安装它。用户需要时在 Runner 电脑运行 `setup-providers.ps1 -IncludeGoalProvider`，该操作会下载锁定的 ACP 与内嵌 Codex；完成本机登录和 Runner 配置后才能使用。

配置后的 Runner `acp.agents` 使用此 `start.mjs`。入口设置 `NO_BROWSER=1`、`INITIAL_AGENT_MODE=agent`，随后加载 npm 锁定的 Codex ACP 实现。

在仓库根目录运行 `npm ci --prefix windows/goal-provider`，再按 `codex.acp.example.toml` 把实际 Node/入口路径配置到目标 Runner。锁文件保存 `@agentclientprotocol/codex-acp@1.9.0`、SDK `1.5.0`、`@openai/codex@0.153.4` 及其传递依赖；npm 根据目标 Windows 架构安装适用包。锁中的其它平台可选依赖是 npm 元数据，不代表本次构建或发布其它平台。

Codex 账号登录、模型访问和权限在目标机器单独配置；不包含认证资料。模型及实际支持的思考参数仍需使用 CLI 自身发现结果明确选择。
