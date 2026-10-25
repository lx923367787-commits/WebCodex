# 在家里的 Windows 电脑继续开发

**当前迁移规则：** 先阅读[工具、账号与迁移](../docs/EXTERNAL_TOOLS.md#简体中文)。Runner 的模型 CLI 必须在目标机自行安装、登录后配置。本页 20261025 下载清单记录历史附件；`-corrected` 包仍含 Goal ACP 内嵌 Codex，当前源码已停止打包该依赖。本轮未重发附件，请用最新源码在新目录构建，不能用历史包作为新规范验收证据。

`windows/current-20261024` 分支保存全部项目源码和自有开发工具；`windows-home-20261025` GitHub 草稿附件保存当时的 Windows 客户端与运行依赖，供历史核对。草稿附件需要登录仓库所有者 `lx923367787-commits` 才能访问。保持草稿是为了不触发现有 `release: published` 的 Linux 镜像发布流程；这不是一个跨平台版本发布。

## 已纠正的打包范围

旧迁移包误收了 WorkBuddy/CodeBuddy、反重力和独立 Codex CLI 程序本体，已从 GitHub 撤下。文件名带 `-corrected` 的历史附件修正了这三项，但仍包含下面所述的嵌套依赖。这些外部程序由家里电脑自行安装和登录，仓库继续保留 WebCodex 对接代码。

历史 `goal-provider/node_modules` 还嵌套 Codex 0.153.4，不能因为它属于适配器依赖就忽略目标机账号配置。当前打包只保留 Goal 适配器源码和锁文件，不复制安装后的 npm 树；目标机需要时运行 `setup-providers.ps1 -IncludeGoalProvider` 安装，再登录并显式配置。已有安装由用户按需迁移，不直接删除正在使用的依赖。

## 历史附件下载与核对

以下命令仅用于核对历史附件；新安装请按后文从当前源码构建。安装 Git/GitHub CLI 后，以同一 GitHub 账号登录。使用新的 `WebCodex-Transfer-corrected` 下载目录，避免混入以前下载的 ZIP 或校验文件：

```powershell
gh auth login
gh api user --jq .login
gh release download windows-home-20261025 --repo lx923367787-commits/WebCodex --dir .\WebCodex-Transfer-corrected
Get-Content .\WebCodex-Transfer-corrected\SHA256SUMS.txt
Get-FileHash .\WebCodex-Transfer-corrected\WebCodex-Windows-Home-20261025-corrected.zip -Algorithm SHA256
Get-FileHash .\WebCodex-Transfer-corrected\WebCodex-Source-20261025-corrected.zip -Algorithm SHA256
```

将哈希与 `SHA256SUMS.txt` 对比，再解压客户端包到一个新的目录。不要覆盖另一套安装或复制旧卸载器。包中 `MANIFEST.json` 逐文件记录大小与 SHA-256，独立的 `Windows-Package-MANIFEST.json` 供下载前检查。

客户端包保存的是当前安装的 `WebCodex.exe`、Server/Runner/CLI 三个 EXE，以及它们各自的真实版本和哈希；没有伪装成用新的 Git 提交重新编译的发行版。旧二进制的 `dirty=true` 构建标记原样保留。源码和这些运行文件的部署对应关系已经核对。

## 历史附件内容（当前源码打包范围见上方规则）

| 材料 | 位置/用途 |
| --- | --- |
| 现用 Windows Desktop 与三个运行时 EXE | `WebCodex.exe`、`webcodex-runtime/` |
| Office/PDF/OCR/绘图工具及运行依赖 | `mycodex-tools/`，含嵌入 Python、PDFium、Tesseract、中文/英文模型、Graphviz、Matplotlib、字体 |
| 网页 Host、接续、子助手和开发辅助脚本 | `chat-handoff/`、`chat-agents/`、`subagent-pilot/` |
| Goal ACP | 历史附件含内嵌 Codex；当前只交付 `goal-provider/` 源码和锁文件，目标机按需安装 |
| Node 24.15.0 与独立聊天 Python | `node/`、`provider-python/`；没有搬运不可移植的旧 `.venv` |
| Python 离线重建材料 | `offline/provider-wheels/` 和 `offline/` 内 embedded Python 压缩包 |
| OpenAI Tunnel 客户端 0.0.12 | `tunnel/tunnel-client.exe`；不含 Tunnel ID 或 API key |
| 安全删除插件 | `safe-delete/` |
| 路径配置和启动入口 | `configure-providers.ps1`、`setup-providers.ps1`、`launch-portable.cmd` |

Gemini CLI 没有安装在此次原机清单中，不能把 Antigravity 或它的模型选项当成 Gemini CLI。所有外部 CLI 的账号和模型访问权限仍由新机登录决定。

## 2026-10-25 连接修复

以下记录 20261025 历史包的连接修复；不表示其满足当前工具分发规则。此前 20261024 包里的 Server 尚未包含首次聊天自动注册修复。新包保持现用 Desktop/Runner/CLI 和工具依赖，并更新 Server。新电脑必须按 [NATIVE-HOST-SETUP.zh-CN.md](NATIVE-HOST-SETUP.zh-CN.md) 配置本机 Host 的自动注册；只填写 API key/Tunnel ID 不能替代这一步。不要复制旧机 Host、OAuth 绑定、carrier 或数据库。新聊天接入仍保持独立 Agent 隔离，CLI/子助手的执行限制沿用原策略。

## 接着开发

优先克隆分支以保留完整 Git 开发流程；源码 ZIP 是同一提交的备用导出，不含 `.git` 历史。

```powershell
git clone --branch windows/current-20261024 --single-branch https://github.com/lx923367787-commits/WebCodex.git D:\Dev\WebCodex
Set-Location D:\Dev\WebCodex
git status
git log -1 --oneline
```

编译依赖与版本见 [DEPENDENCIES.zh-CN.md](DEPENDENCIES.zh-CN.md)，主构建入口为 [build.ps1](build.ps1)。Windows 安装器源码已补在 [installer/](installer/README.zh-CN.md)，Tauri 原安装器配置仍在 `apps/desktop/src-tauri/`。Visual Studio/MSVC、Windows SDK、Rust、Office、Edge/WebView2 属于新机需要安装的外部系统软件；本项目不会把其安装状态或授权复制到 Git 中。

移动便携包到最终目录后，在 **Windows 自带 PowerShell** 中运行配置生成器，例如：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File 'D:\Apps\WebCodex Desktop\configure-providers.ps1' `
  -ProjectRoot 'D:\Dev\WebCodex'
```

它只生成包含新路径的 TOML 模板。按 [PORTABLE.zh-CN.md](PORTABLE.zh-CN.md) 合并到新机实际 Runner，重新登记项目。使用 `launch-portable.cmd` 让 Desktop 继承所选 Node 和随包 Tunnel 路径；Node 优先使用包内版本，没有时查找系统安装。外部 CLI 由家里电脑自行安装，沿用原用户环境；没有改动系统 PATH。首次使用仍须配置自己的 API key/Tunnel ID、CLI 登录及浏览器扩展授权。Host carrier、执行器权限/路径指纹由新机正式流程生成，不应手工复制旧 nonce、租约或权限状态。

## 明确排除的内容

API key、Tunnel ID、登录 cookie/token、浏览器 profile、个人项目文档、会话/任务数据库和历史回执都未上传。它们不是功能源码；复制旧机器身份反而会妨碍新机正常登记。当前旧 `uninstall.exe` 绑定原安装登记，不适用于便携包；新机安装时由已有 Tauri/NSIS 源码生成正确的卸载器。

当前实现、配套工具、现用客户端和依赖材料已收齐。离线构建/测试、便携导入和文件校验见 [VALIDATION.md](VALIDATION.md)；家里电脑的授权联网任务、Office 渲染与实际交接需要在那里验收。

## 协作测试发现与通用修复

本节描述源码行为，不复制某台电脑的 CLI 安装、账号、端口或准入记录。踩坑经过与提交范围见[2026-10-25 协作复盘](../docs/agent/collaboration-lessons-2026-10-25.zh-CN.md)。

## 命名项目与多个授权文件夹（本机开发版）

项目页现在以命名项目组织工作。点击“新建命名项目”，填写例如 `EAS`，勾选多个已登记文件夹，或通过“添加文件夹”多选代码、资料等目录，最后“保存并授权”。一个项目最多 16 个文件夹，最多 32 个项目；项目名不能重复。目录不会被移动，授权不包含相邻目录。原有目录登记、启停与团队设置保留在“高级”区域。

在本机实际连接的 WebCodex 插件中（下文 `@webcodex` 是通用名称示例），可以说“在 EAS 项目里修改……”。原生聊天通过 `agent_projects` 查询当前电脑的可用命名项目，通过 `agent_select_project` 选择返回的精确 ID。多个项目且未指定时应先询问用户；只有一个可用命名项目时，首次状态检查可以自动选择。项目选择创建独立 Agent，不需要复制会话令牌。已有聊天保持原项目，切换到其他项目需新开聊天，桌面当前目录不会改变聊天的授权。

`agent_assignment` 返回所选项目当前授权的文件夹。`agent_project_files` 使用其中的精确文件夹 ID 和相对路径进行有界列目录、读取、创建或替换 UTF-8 文件。覆盖需提供最近读取的 SHA256；Runner 目录边界、敏感路径策略、所有者访问检查、只读设置及服务端写入策略仍然生效。停用项目或取消文件夹授权后，后续文件请求会被拒绝；已接纳的在途请求可能完成。绑定 Goal 的同一 Agent 接续运行也必须逐次通过当前 assignment、唯一 Goal 和原生 Host 文件夹授权验证。此入口不提供 Shell/CLI 执行。

旧版已绑定聊天继续保留原身份；创建命名项目后，请在新聊天中选择它。未创建命名项目的安装保留旧版首次连接行为。配置保存在本机 Server 数据库，重启保留；不自动同步到另一台电脑。家里和公司各自使用对应电脑的插件连接及独立 Tunnel ID，按外部工具迁移说明分别配置。

### 隔离 CLI 的本机代理

已审核的隔离执行资源启用 `user_https_proxy` 时，Runner 优先读取当前用户注册表 `Environment/HTTPS_PROXY`；只有该值不存在时，才读取已启用的 Windows 系统代理 `Internet Settings/ProxyServer`。支持统一端口或协议列表中的 HTTPS 项，仍只接受无凭据的数字回环 HTTP 代理，不解析 PAC，也不回退到局域网地址。显式配置无效时直接报错。代理读取成功不代表 CLI 账号、模型权限或隔离资源已就绪；临时回环访问授权仍由原有逐 Job 持久日志记录和回收。

Runner 的 `codebuddy-2.175.0-node-files-v1` 加载映射固定 Node 可执行文件摘要及完整 CLI 包摘要，包含 `product.internal.json` 等区域配置。遗漏区域文件会让原生 CLI 回退至不同服务端并产生误导性的登录错误，因此缺文件、增加文件或内容变化均拒绝执行。此映射仍需要真实验证证据、独立审查和受信任资源登记；安装或更新本机 CLI 不会自动取得生产执行权限。隔离状态目录独立，使用原生选项关闭宿主 Git 身份查询与 Shell 快照预热，不继承宿主环境或修改宿主登录记录。

CodeBuddy 原生登录刷新需要在凭据文件旁创建锁并原子替换文件。Runner 只读取得单个授权登录文件的私有副本，只让本次执行的 AppContainer 在独立 `home` 中的凭据叶目录刷新副本；宿主源文件、其他执行及祖先目录的授权不扩大，不向宿主写回。文件工具对凭据路径的拒绝策略保持生效。进程树停止后按有界、拒绝链接的句柄清理移除该叶目录中的登录副本、临时文件及锁文件内容；清理失败不报告已移除。不能将旧失败槽位重放成成功记录。

### 命名项目的团队设置

项目卡片的“团队设置”编辑该项目已授权文件夹原有的策略，不另建一套设置。只有一个文件夹且已明确保存策略时，新聊天会继承子 Agent 上限，并按 `(子 Agent 上限 + 总经理 1 人) × 每人 CLI 上限` 计算团队调用预算。现有聊天的权限不追溯扩大；遇到 `workspace_team_new_chat_required` 时在同一连接新建聊天并选择原项目。多个文件夹不会自动猜测团队执行目录。

新团队固定保存原主机、文件夹标识和路径。项目停用、授权目录变更或 Runner 登记不匹配时停止派发，不能沿用旧任务转向新目录。保存的模型选择与 CLI 实际执行资源分别验证；模型目录能显示某个选项，不代表 Runner 已配置对应的隔离资源或账号可调用该模型。

冻结计划时可显式传入 `effort: "project_policy"`，表示使用 `expected_policy_revision` 对应的已保存 effort。该选择要求 provider 已启用、model 与保存值完全一致；服务端在同一事务内解析，冻结记录存储实际值（不支持 effort 时为 JSON `null`），不会将选择符交给 CLI。 Antigravity 的实际模型目录可包含明确档位的 slug；保存 effort 为 null 时保持此精确模型、不另加 `--effort` 覆盖。目录必须同时证实该模型和其档位，未知 slug 或裸模型名不会被当成显式档位。相同请求的重试返回原冻结记录，即使项目设置后来改变。直接传实际 effort 或 `null` 的方式仍受原有验证约束。

原生聊天交接后，`agent_project_files` 继续检查同一已认领任务、继任实例绑定的唯一 Goal、当前 Host 和命名项目的文件夹授权。写入还检查任务仍有效和当前运行的业务执行门禁；每次 Runner 查询后再次检查授权，并固定本次请求的实际目标。这里的命名项目文件权限独立于 Agent 私有 Goal 工作区，不扩大后者的路径权限。旧运行、已撤销文件夹和已释放任务的写入仍拒绝。

### 同一团队的 CLI 正式交付与追加轮次

CLI 计划项支持 `inputs: [{name, content}]`：最多 8 个唯一标签、序列化 JSON 总量最多 32 KiB。材料在计划登记时冻结，经 Worker enrollment 生成 SHA-256 快照后进入固定 stdin 和启动哈希。标签不是宿主机路径，隔离工作目录初始为空；CLI 使用文本材料或自行创建工作副本，不能读取父目录补材料。调用方须提供任务所需源码/契约，并明确要求 `publish/files/` 中的文件和最后写入的 `publish/manifest.json`。退出码 0 不代替收集、版本读取和正式审核。缺少发布目录/清单返回 `agent_cli_delivery_not_published`；任意 Runner 错误文本仍不透传。

用户明确要求保留团队再执行一轮时，本地 Bootstrap 控制端可调用 `team_cli_round_register`（不对 Agent 开放）：`owner_user`、`root_task_id`、`expected_round`（首次为 0）、`expected_policy_revision`、`plan`、`idempotency_key`。请求必须来自回环连接；验证原经理、成员委派、当前命名项目授权和保存策略，且之前所有槽位已有终态与停止证据。每轮仍受原 assignment 和当前项目策略限制，总计最多 32 个槽位/成员、最多 15 个追加轮次。每次调用仅授权一个明确工作包，不授权循环重试。

原计划、旧 attempt、成员身份和失败记录不修改。追加记录不可更新/删除，并返回各成员的 `first_call_indices`；原生 `team_cli_start` 使用新索引、新稳定 key。`team_plan_read.cli_rounds` 展示追加轮次的槽位、配置和输入摘要哈希，不重复输出整份源码；完成计数包含全部轮次；计数仍不是成果验收。缺少或不确定停止证据时拒绝开新轮，不能用新任务绕开旧进程。部署后应先跑输入/轮次/发布错误专项检查，再验证真实 CLI 发布和审核；不得把诊断 marker 算作游戏成果。
### 阻塞状态的原生交接

显式保存的 native Agent handoff checkpoint 可以保留 `blocked` Goal 并接续到同一 Agent 的新 run，供读取、排障和核对原任务；交接不会将 Goal 改为 ready，也不会跳过节点、身份或执行授权检查。`paused`、终态 Goal 和旧版非 Agent 的 blocked 交接仍不会自动启动。

若旧调度器在分配第一个 attempt 前以 `goal_not_active` 中止了该交接，受信本地维护端可调用现有 `handoff_recover_failed`，以 `expected_attempt_id: null` 精确恢复这个零 attempt 失败。接口仍检查 owner/project/Agent/run/generation/Goal revision、原失败 checkpoint、无活跃候选和执行已关闭；保留失败回执与原 phase，不恢复源对话业务权限，不补充尝试预算。其他零 attempt 失败不接受此恢复。

已超时或失败的 CLI 仍可能在停止前正式发布了可用文件。收集后可按确切 artifact/version、当前 worker revision 和稳定 key 独立接受或拒绝产物；若 worker 已是 Failed，审查只新增不可变 review，不改写原任务失败、revision、原因或 completion key。产物接受与进程执行成功必须分开报告。未结束的进程仍须等待停止证据，不能靠审查强行完成。

原生成员已领取独立身份且其 enrollment、Host dispatch binding、任务、所有者和项目均与执行谱系一致时，纳入现有后代执行收尾，不再作为不透明的旧外部任务永久阻塞经理交接。收尾仍关闭整条谱系的 CLI admission，并要求所有原执行器的停止凭据；不取消成员身份、任务或修改历史。未绑定、未领取、作用域不一致的委派以及旧 coding/fallback 仍阻塞。已收尾的成员 run 若要再启动 CLI，必须先通过其自身的正式交接获得新 run，不能重新打开旧 admission。
