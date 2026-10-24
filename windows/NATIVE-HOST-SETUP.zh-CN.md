# 新电脑的原生聊天自动连接

2026-10-25 的修复让新聊天在首次调用 `runtime_status`、无参数 `agent_identity` 或无参数 `agent_assignment` 时，创建它自己的独立 Agent、任务和私有工作目录。同一聊天重复调用沿用该身份。`agent_host_window_not_active` 描述的是服务端绑定，不是桌面窗口是否置顶。

新 Server 仍需本机管理员明确设置 Host 对应的用户和项目。只替换 EXE 不会替新电脑生成这项授权；不要复制旧电脑的 Host ID、OAuth client、window ID、carrier、数据库、账号凭据或聊天注册凭据。

## 取得这台电脑的实际参数

1. 按 Windows 安装说明初始化自己的本地 Desktop、Server、Runner 和项目，配置自己的 Tunnel 与账号。确认使用本次修复后的 Server、Runner 在线、目标项目已登记并在线。保持当前 Desktop 选中的项目就是需要自动连接的项目。
2. 通过本机真实 connector 连接和 Server 的可信 Host 注册记录确认 `host_id`；Desktop 的 `desktop-state.json` 中 `runtime.native_host_id` 应与该记录一致。它是 `wc_host_` 加 32 位十六进制的本机注册 ID，不是 Tunnel ID、OAuth client ID 或聊天 window ID。没有真实登记就先完成 Host/Tunnel 初始化；不要使用旧机器的值，也不要让模型编造。
3. 记录本机 Desktop 的 `runtime.server_url` 和 `runtime.runtime_project_id`。`owner_user` 是本机 Server 启用用户的 **数据库 ID**，不是显示名或 username。用管理员只读用户列表或本机可信用户记录确认。默认状态文件在 `%LOCALAPPDATA%\dev.webcodex.desktop\desktop-state.json`。
4. 使用本机 Desktop 管理的 `secrets\chat-agents\control.json` 中 `bootstrap_token_file` 指向的现有文件。只把该文件路径传给脚本，不在聊天、命令参数或 Git 中粘贴令牌。不要复制整份 carrier；它只是供本机操作者确认路径和身份的可信记录。

## 先检查，再应用

脚本使用 Python 3.11 标准库。源码 checkout 中脚本在 `windows/`；便携包中它与本说明位于应用根目录。下列变量必须由操作者替换为这台电脑上已核实的实际值：

```powershell
$setup = @(
  '--server-url', $VerifiedLocalServerUrl,
  '--bootstrap-token-file', $VerifiedBootstrapTokenFile,
  '--host-id', $VerifiedNativeHostId,
  '--owner-user', $VerifiedOwnerUserId,
  '--project', $VerifiedRuntimeProjectId
)
python windows/configure-native-host.py @setup --check
python windows/configure-native-host.py @setup --apply
```

便携包应用根目录中把脚本路径改为 `configure-native-host.py`；没有系统 Python 时，可用包内 `mycodex-tools\python\python.exe` 运行。自定义 Desktop 数据目录时显式加入 `--desktop-state <实际状态文件>`，数据库位于其他位置时加入 `--database <实际webcodex.db>`。默认数据库是状态文件同级的 `runtime/local/data/webcodex.db`。

不传 `--apply` 就只检查：脚本先核对显式目标与本机 Desktop 的 Server/Host/项目，再以 SQLite `mode=ro` 检查启用 Host 和用户；随后通过现有只读 API 验证凭据确为 Bootstrap、目标用户启用、项目在线。只接受带端口的数字 loopback HTTP 地址，例如 `http://127.0.0.1:<本机实际端口>`，禁用代理和重定向。它不会猜测或自动替换任何身份。

`--apply` 在同样检查后最多发送一次 `configure_native_host_auto_enrollment` 控制请求，并只读回查保存结果。相同配置已存在时不重复写入。两个委派上限默认都是 **0**，与此次修复的本机策略一致；仅当操作者明确需要时，才分别设置 `--max-subagents`、`--max-cli-calls`（范围 0–32）。这不会复活已撤销的 Host 或旧聊天运行，也不会恢复旧机器的任务历史。

输出只有检查结果、是否写入及额度，不返回令牌、完整 API 响应或聊天身份；脚本不保存报告。若请求超时或回查不一致，先重跑 `--check` 确认当前状态，再决定是否应用，不把不确定结果当作未写入。

## 验收

在自己的真实新聊天中调用 `runtime_status`（可用 `compact: true`），确认 `connected: true`，随后用 `agent_assignment` 读取该聊天的任务。同一聊天重读应保持身份，另一新聊天应拥有独立身份。若返回 `agent_state: enrollment_required`，检查上述本机配置；仅 `tool_manifest` 的工具发现不会建立 Agent。已撤销或待交接窗口不能靠首次注册复活，接续仍使用现有协议。

此次交付仅做了脚本帮助、参数和 mock 测试，没有替家中电脑执行配置。实际新机连接验收须在其本机账号和 Tunnel 初始化后完成。

开发者可从仓库根目录运行 `python -B -m unittest discover -s windows -p test_configure_native_host.py` 重跑 8 个离线测试；测试使用合成 SQLite 数据，所有 HTTP 请求均被 mock。
