# Windows Codex ACP 入口

当前 Runner 的 `acp.agents` 使用此 `start.mjs`。入口设置 `NO_BROWSER=1`、`INITIAL_AGENT_MODE=agent`，随后加载 npm 锁定的 Codex ACP 实现。

在仓库根目录运行 `npm ci --prefix windows/goal-provider`，再按 `codex.acp.example.toml` 把实际 Node/入口路径配置到目标 Runner。锁文件保存 `@agentclientprotocol/codex-acp@1.9.0`、SDK `1.5.0`、`@openai/codex@0.153.4` 及其传递依赖；npm 根据目标 Windows 架构安装适用包。锁中的其它平台可选依赖是 npm 元数据，不代表本次构建或发布其它平台。

Codex 账号登录、模型访问和权限在目标机器单独配置；不包含认证资料。模型及实际支持的思考参数仍需使用 CLI 自身发现结果明确选择。
