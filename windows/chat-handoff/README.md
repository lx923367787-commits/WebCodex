# Windows 自动接续与网页子任务运行组件

本目录与相邻 `chat-agents`、`subagent-pilot`、`goal-provider` 保存 2026-10-24 Windows 正在使用的仓库外工具源码。它们补充仓库内的 Server、Runner、Desktop、`plugins/mycodex-tools` 和 `plugins/safe-delete`。保持这四个目录相邻。

| 目录 | 当前作用 |
| --- | --- |
| `chat-handoff` | 120 秒交接守候、持久 STOP/PAUSE、Host enrollment、自动队列调度适配、思考档位与官方 Playwright 扩展连接 |
| `chat-agents` | 常驻 native provider、网页子任务、结果收集、档位发现/只读查询、CLI 参数发现与 Host 生命周期 |
| `subagent-pilot` | 当前 `chat_transport.py` 仍导入的单次发送/观察实现 `pilot_transport.py` |
| `goal-provider` | 当前 Runner 的 Codex ACP 入口及完整 npm 锁文件 |

代码只做必要的路径可移植调整：Node 从 `WEBCODEX_NODE` 或 PATH 发现；Desktop state 使用当前用户目录；CLI 路径使用当前用户目录；pilot 默认查找相邻 handoff。调度、权限、已发送/未知结果不重放等逻辑保留。

## 安装依赖

需要 Windows x64、Python 3.11、Node.js 22 或更新的兼容版本、当前 Microsoft Edge，以及与此源码一起构建的 WebCodex Desktop/Server/Runner。下列命令在仓库根目录执行，只安装依赖，不启动服务、访问网页或发送任务：

```powershell
py -3.11 -m venv windows/.venv
windows/.venv/Scripts/python.exe -m pip install -r windows/chat-handoff/requirements.lock
npm ci --prefix windows/chat-handoff/extension-runtime
npm ci --prefix windows/goal-provider
```

Python 锁文件记录当前可用 Windows Python 3.11 环境中 `mcp==1.27.2`、`playwright==1.63.0` 的完整直接/间接依赖版本。官方扩展 MCP 锁定为 `@playwright/mcp@0.0.83`。无需 `playwright install` 下载另一份浏览器。第三方依赖从锁文件重新安装，不把 `node_modules`、虚拟环境或浏览器 profile 放入 Git。

## 配置与授权

1. 在目标 Windows 机器完成 Desktop 本地运行环境初始化、项目登记和 Host 连接。新机器使用自己的凭据与 Host 身份。
2. 将 `edge-handoff.provider.example.toml`、相邻 `chat-agents/chat-agents.provider.example.toml` 中示例绝对路径改为实际路径，再合并进目标 Runner 的配置；保留现有 provider，不重复已有 id。`goal-provider/codex.acp.example.toml` 对应当前 Codex ACP Agent。插件候选检查、目录加载与运行实例状态分别验证。
3. 在日常 Edge 的 `Default` profile 安装官方 Playwright 扩展并登录对应网站。扩展连接授权和账号登录属于目标机器的首次配置，不能通过复制旧 cookies 或令牌复刻。
4. 扩展令牌保存在 `chat-handoff/runtime/playwright-extension-token.txt`。已有 `extension-runtime/capture_token.py` 仅在操作者已明确复制扩展自己的命名令牌后运行：`python windows/chat-handoff/extension-runtime/capture_token.py --user-copied-token`。它不输出令牌，并设置当前用户/SYSTEM 文件 ACL。不要提交或共享该文件。

当前 Desktop 的 `host_control.rs`/`host_control_config.rs` 为已存在的精确 `chat-agents` provider 生成受保护的 schema v2 carrier，加入 `--control-config`、`--desktop-state` 并设置 `runtime_lifecycle = "chat_agents_v1"`。Runner 的生命周期提交负责激活自动队列。不要手写假 carrier、复制另一台机器的 Bootstrap 凭据，或把配置文件存在等同于 provider 已激活。

`chat-agents` 没有 carrier 时仍能提供显式调用工具，但不会自动接收 Host 队列。后台交接守候需使用同一 Desktop 管理的 carrier：

```powershell
# $controlConfig 必须是目标 Desktop 正式生成的绝对 carrier 路径。
$python = (Resolve-Path windows/.venv/Scripts/python.exe).Path
$entry = (Resolve-Path windows/chat-handoff/run_auto_handoff.py).Path
& $python $entry status
& $python $entry run --control-config $controlConfig
```

`run` 是持续运行命令，只应在实际启用自动接续时执行。持久 STOP/PAUSE 继续有效；`resume` 明确解除停止，但不会启动进程或重发未知请求。后台启动可使用同一参数和 `Start-Process -WindowStyle Hidden`，需正确引用含空格的路径。另一条显式控制接口支持 `WEBCODEX_HANDOFF_CONTROL_ENDPOINT`、`WEBCODEX_HANDOFF_CONTROL_TOKEN_FILE`、`WEBCODEX_HANDOFF_OWNER_USER`、`WEBCODEX_HANDOFF_PROJECT`（可选 `WEBCODEX_HANDOFF_AGENT_ID`）；控制凭据必须来自本机正式授权，不能拿普通模型/用户令牌替代。

可选路径设置：`WEBCODEX_NODE` 指向 Node 可执行文件；`WEBCODEX_DESKTOP_STATE` 指向 Desktop state；`WEBCODEX_CODEBUDDY`/`WEBCODEX_CODEX` 指向已安装 CLI。默认状态与凭据路径由当前 Windows 用户解析。应优先保持随附目录布局和默认 runtime 位置；迁移已有 runtime 时保留数据库、身份与未知发送证据，不删除状态来重试。

## 离线验证

下列命令只执行 mock/临时文件单元测试。浏览器测试必须显式 opt-in，默认跳过；本次 GitHub 备份验证不执行 `live_handoff_test.py`、`extension_smoke.py` 或实际模型任务。

```powershell
$env:PYTHONDONTWRITEBYTECODE = '1'
Remove-Item Env:WEBCODEX_EXTENSION_HEADLESS_TEST,Env:WEBCODEX_APPROVAL_HTML,Env:WEBCODEX_TEST_CHROMIUM -ErrorAction SilentlyContinue
windows/.venv/Scripts/python.exe -B -m unittest discover -s windows/chat-handoff -p 'test_*.py'
windows/.venv/Scripts/python.exe -B -m unittest discover -s windows/chat-agents -p 'test_*.py'
windows/.venv/Scripts/python.exe -B -m unittest discover -s windows/subagent-pilot -p 'test_*.py'
```

生产源码来自当前运行目录；对应 2026-10-24 E3 自动调度和 H1 carrier 验收的离线测试一并收录，补齐部署目录中未更新的旧测试。测试数据是合成 fixture。新机器仍需验证其登录、扩展连接、provider ready、Host 自动调度及一次真实交接，离线通过不能替代这些外部条件。

## 未上传的本机内容

`runtime`、数据库、任务正文/回执、页面证据、日志、账号凭据、cookies、旧备份以及一次性安装激活记录均未上传。2026-10-23 的 `activate_provider.py` 只用于当时那台机器的一次激活，硬编码旧 Runner 并依赖本机变更回执，不是生产调用链；此次用无凭据配置模板和当前 Desktop 生命周期流程替代其安装用途。原型目录只保留当前仍被导入的 transport 与相应测试。
