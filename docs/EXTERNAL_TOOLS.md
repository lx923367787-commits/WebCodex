# Tools, accounts and moving to another computer

[English](#english) | [简体中文](#简体中文)

## English

WebCodex supplies its Server, Runner, management CLI and integration code. It does not supply another user's ready-to-use model account or tool environment. **Install model executors on each Runner computer, sign in there (or configure that user's API key), then configure and validate that Runner.** This applies to CodeBuddy/WorkBuddy, Antigravity, Codex, Gemini and optional Goal ACP executors, including executors nested inside adapter dependencies.

| Component | Distribution and destination setup |
| --- | --- |
| Model CLIs and Goal ACP executor dependencies | Integration source, examples and lockfiles may ship. Executables, installed packages and account state do not ship in a WebCodex payload. Users install the optional tools on their own computer. |
| Python, Node, PDFium, OCR engines/models, Graphviz and fonts | Account-independent runtime dependencies may ship with licenses, pinned versions and manifests. They do not provide Microsoft Office, model access or paid-service entitlement. |
| OpenAI tunnel-client and transport adapters | A pinned credential-free transport binary may ship. Each connection needs the operator's own key/authorization and Tunnel ID. Never bundle those values or a generated connection configuration. |
| ChatGPT/browser handoff | Integration code and Playwright libraries may ship. Browser installation, login, extension approval and local extension token belong to the destination user. Never copy a browser profile. |
| Office, CAD/CAE and other licensed applications | Users install and license the required applications on each machine. An adapter or Python wrapper is not the application or its license. |
| Runner state | Generate local paths, project grants, credentials and execution-resource approvals on the destination. Never migrate a live Runner configuration, account database, private carrier, lease or executable approval as a distributable default. |

### Runner setup and acceptance

Desktop and the assigned Agent's `cli` tool share a CLI adapter registry. The desktop scans installed CLI candidates instead of rendering a fixed product list. `cli` uses `discover` → `models` → `start` → `status`/`cancel`; `models` with `{task_id,cli_id}` reads one installation's capabilities without requiring a team plan or an execution-resource approval. Omitting `cli_id` retains the team catalog and fallback profile view. `start` still uses an authorized frozen slot and a stable idempotency key. Discovery and metadata reads grant no execution authority.

The default registry contains installation hints for common CLIs. Additional native protocols can be supplied by the local operator in `%LOCALAPPDATA%/webcodex/cli-adapters.json` on Windows, `$XDG_CONFIG_HOME/webcodex/cli-adapters.json` on Unix, or an absolute `WEBCODEX_CLI_ADAPTERS_FILE`. The file is a bounded JSON array of `CliAdapter` definitions; see `crates/webcodex-core/src/cli_adapters.json` for the data format. Explicit local entries replace the corresponding default recipe; duplicate local IDs are rejected. Do not put credentials in this file. An installation without an invocation/model-capability adapter is reported as needing adaptation, never as callable. Shell launchers are listed but not evaluated as shell commands.

Execution remains attached to the exact Runner resource manifest. An administrator-reviewed `registered-native-cli-v1` resource may carry its own `reviewed_controls.adapter`, bound into its review digest, so adding a native CLI no longer requires a product enum or changes to team scheduling. It must retain real control verification and independent review evidence. The model cannot submit a program path, arguments, adapter definition or approval record. Existing compiled adapters and stored plans remain readable; installing a new executable does not silently authorize it or replace an existing task.

Adapters may declare a bounded structured result contract. For these CLIs, a non-error terminal result is required in addition to exit code zero; reported errors, missing/ambiguous terminal events, malformed output and oversized output do not become success. Process completion remains distinct from deliverable acceptance.

Codex and Claude have native protocol recipes. Codex discovers models with a bounded `app-server` initialize → `model/list` exchange (including pagination), selects reasoning through `--config model_reasoning_effort="..."`, and executes `exec --json`. Its completion verifier requires one final `turn.completed` and rejects `turn.failed`/`error`. Claude reads the model catalog from the SDK initialization response, resolves aliases when the CLI supplies `resolvedModel`, and invokes `--print --model ... --effort ...` with a structured result. Unadvertised effort options are omitted. Both transport a complete per-model effort map to the UI and server; selecting an effort from another model is rejected before dispatch. Native metadata queries never send a prompt or start an inference turn.

Restricted Claude startup ignores user customizations. The host metadata adapter therefore reads only the bounded `env` allowlist for account routing and model aliases in the user's `settings.json`; it does not load hooks, plugins, MCP configuration, shell helpers or subagent settings. Account tokens remain private child-environment values. An explicit environment account/endpoint takes precedence as a group, so a different endpoint cannot silently reuse a settings-file token. Codex/Claude metadata also respect the enabled credential-free system proxy unless an explicit proxy is present. This host context is not an isolated Runner credential grant: a production Runner still needs its own verified resource and credential mapping.

Protocol references: [Codex model discovery](https://developers.openai.com/codex/app-server), [Codex noninteractive output](https://developers.openai.com/codex/noninteractive), [Claude SDK control protocol](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py), and [Claude gateway configuration](https://code.claude.com/docs/en/llm-gateway). Installed CLI help and returned capabilities remain authoritative for the local version.

1. Install the chosen CLI from its official distribution on the machine that will execute the work. Discover its actual version, model and effort options; do not assume another machine's defaults or entitlement.
2. Sign in locally or configure that user's key using the CLI's supported mechanism. Do not put secrets in Git, examples, archives, screenshots or command arguments.
3. Configure the Runner's local executable/package paths, required credential mapping and execution resources through the supported setup. A Desktop model list is discovery only; it does not change a previously approved Runner resource. Isolated jobs still require their sandbox/resource checks and must not inherit an entire host home or arbitrary environment.
4. Test the exact selected model through the intended Runner execution path. A CLI `--help` listing, a successful host-shell call, and a successful isolated Runner job are different evidence. Missing login, permissions or compatible resources must be reported, never silently replaced by a bundled CLI or a different account.
5. After a CLI upgrade or move, refresh discovery and revalidate its execution resources. Do not disable hash, ownership or sandbox checks to reuse the old approval.

`team_cli_catalog` keeps usable selections in `providers` and reports omitted providers in `unavailable_providers` using bounded reason codes. `agent_isolated_execution_unavailable` means the bound Runner has not advertised the required isolated execution capabilities; `agent_cli_provider_resource_unavailable` means that provider's execution resource is absent. Neither proves the host CLI is uninstalled. Inspect the exact Runner's local resource configuration and current registration. `team_cli_start` returns `team_plan_required` for an authorized existing task without a frozen plan; register the intended team plan rather than creating a replacement manager. Catalog discovery and a successful host CLI smoke do not replace isolated execution acceptance.

For optional Windows Goal ACP, run `windows/setup-providers.ps1 -IncludeGoalProvider` on the destination, or follow [its guide](../windows/goal-provider/README.md). The normal setup omits this model executor. Offline preparation requires dependencies already installed/prepared by that user; the distributable does not copy the publisher's installed Goal npm tree.

Company and home Runners have independent installations and logins. For two independent plugins, configure each plugin and its matching desktop with a distinct Tunnel ID. Changing a local ID does not update a plugin connection. Sharing one ID between simultaneously running clients does not identify the intended computer.

### Packaging checks and audit limits

Build from fresh inputs, not a live installation. Windows portable, source-build and installer payload gates reject known private-state paths and model executors, including nested Goal dependencies. The gate is a path policy, not a complete secret scanner: review manifests and content separately, including unfamiliar third-party tool configuration, service endpoints and licenses. A checksum proves file identity, not absence of credentials or account availability.

The 2026-10-25 review found standalone model CLIs in the original transfer, and a Codex executor nested in the corrected transfer's Goal ACP dependencies. Current source packaging excludes both. Previously downloaded archives and installed directories are not changed by a source update; use a fresh build or a separately verified subsequent artifact. Historical hashes and manifests must remain historical evidence, not be relabeled as the new payload.

See the [2026-10-25 collaboration review](agent/collaboration-lessons-2026-10-25.zh-CN.md) for the tested failure modes. Node CodeBuddy metadata discovery now uses an explicit administrator-owned `codebuddy_config_directory` in its reviewed Runner resource profile; it no longer assumes one installation's account directory. The directory is included in the approval digest. Configure and re-review it on each machine; never ship its credentials or reuse another machine's approval.

## 简体中文

WebCodex 提供自己的 Server、Runner、管理 CLI 和工具对接代码。**模型 CLI 必须在每台 Runner 所在电脑由用户自行安装，登录自己的账号或配置自己的 API key，再配置 Runner 并验证。**此规则同样适用于 CodeBuddy/WorkBuddy、Antigravity、Codex、Gemini，以及 Goal ACP 依赖中嵌套的模型执行器。

| 内容 | 如何交付与配置 |
| --- | --- |
| 模型 CLI、Goal ACP 执行依赖 | 可交付对接源码、示例和锁文件；不把发布者安装好的程序、npm 包、登录状态打进 WebCodex 包。由目标机用户按需安装。 |
| Python、Node、PDFium、OCR 引擎/模型、Graphviz、字体 | 可按许可证与校验清单交付不依赖账号的通用运行依赖；这不包含 Office 本体或模型服务权限。 |
| Tunnel 客户端与传输适配器 | 可以包含固定版本、不带凭据的传输程序；API key、Tunnel ID 和生成的连接配置由用户在本机填写。 |
| ChatGPT、浏览器接续、扩展 | 可以包含对接代码和 Playwright 库；浏览器安装、账号登录、扩展授权及 token 由用户完成，不复制浏览器 profile。 |
| Office、CAD/CAE、商业软件 | 由用户在目标机安装并取得许可；MCP/COM/Python 适配器不能代替应用本体及授权。 |
| Runner 与本地运行状态 | 本机生成路径、项目授权、凭据映射和执行资源准入；不随包复制旧 Runner 配置、账号数据库、carrier、租约、可执行文件批准状态。 |

操作顺序是：**本机安装 → 本机登录/填 key → 查询实际版本与模型 → 配置本机 Runner → 通过该 Runner 实际执行验收**。界面能列出模型、宿主命令行能调用、隔离 Runner 能执行，是三种不同的验证，不能相互替代。更新 CLI 后重新验证资源；不能为了复用公司配置而绕过路径、摘要、身份或沙箱检查。凭据只按受支持的最小映射传入，不能把整个用户目录或全部环境变量交给隔离任务。

本次隔离调用与正式交付问题见[协作复盘](agent/collaboration-lessons-2026-10-25.zh-CN.md)。Node CodeBuddy 的元数据查询改用受信 Runner 资源 profile 中显式指定的 `codebuddy_config_directory`，不假定某套安装的账号目录。目录参与审核摘要，换电脑或目录需重新审核；不要复制凭据内容或另一台机器的准入记录。代理端口、浏览器应用名称及登录账号同样按目标机核实，源码中的通用示例不代表实际连接。

Windows 普通聊天工具安装不再自动安装 Goal ACP；需要时在目标机运行 `windows/setup-providers.ps1 -IncludeGoalProvider`，再按 [Goal ACP 说明](../windows/goal-provider/README.md) 登录与配置。公司和家里分别安装、登录、授权。使用两个插件时，各自与对应电脑配置不同的 Tunnel ID，不能指望一个 ID 自动判断应该连接哪台电脑。

2026-10-25 审计确认：旧迁移包含独立 CLI，远端纠正版虽然去掉了它们，但 Goal ACP 依赖仍嵌套 Codex；当前打包源码也已排除此项。现有安装目录、已下载 ZIP 和历史 Release 附件不会因源码更新而自动变干净。本轮不把历史附件改称新规范产物，也不删除用户已安装的工具或登录资料。

打包应从新目录准备输入。路径检查会拒绝已知账号状态、私有配置和模型执行器；它不是完整密钥扫描器，仍须检查文件内容、第三方工具的配置和许可证。通用依赖、适配器源码以及不含凭据的 Tunnel 程序可保留。检查通过或文件哈希一致，都不能作为“所有秘密绝不存在”或“新用户账号已经可用”的保证。
