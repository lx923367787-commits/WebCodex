# WebCodex

**外部工具与账号：** 模型 CLI 由用户在每台 Runner 电脑自行安装、登录或配置自己的 key，再配置并验证 Runner。WebCodex 交付对接代码和通用运行依赖，不交付另一用户配置好的工具环境。见[工具、账号与迁移规则](docs/EXTERNAL_TOOLS.md#简体中文)。

**Windows 当前版本（含 2026-10-25 连接修复）：[完整源码、工具、依赖和复刻步骤](windows/README.md)。**

**换电脑继续开发：[源码构建、本机账号配置与历史迁移记录](windows/HOME-DEVELOPMENT.zh-CN.md)。**

[English](README.md) | [简体中文](README.zh-CN.md)

**让 ChatGPT、Claude 和其他 AI Agent 使用你自己机器上的代码、开发工具和办公文件。**

WebCodex 把 AI 客户端连接到已授权的项目，提供文件读取与修改、Git、命令、测试、长任务和代码导航。本仓库在 WebCodex 基础上加入目标计划与验收、跨对话交接、审核反馈，以及 Office / PDF / OCR 和绘图工作流。

源码源自 [yyjeqhc/webcodex](https://github.com/yyjeqhc/webcodex)。上游部分保留 [Apache-2.0](LICENSE)；定制贡献者拥有版权的原创新增及修改使用 [MIT](LICENSE-MIT)，详见[许可范围](LICENSING.md)。当前源码版本声明为 `0.4.1`；具体能力以本仓库提交、构建身份和实际配置为准。上游发布包与本定制源码应分别使用。

## 功能

| 能力 | 可以做什么 | 使用条件 |
| --- | --- | --- |
| 项目与代码 | 读取、搜索、受保护修改、Git 检查、代码导航 | 注册项目并授予相应权限 |
| 执行与 Job | 运行真实编译器、测试、脚本；观察和取消长任务 | Runner 可用且工具链已安装 |
| Desktop | 管理本地 Server / Runner、项目、连接及活动 | 对应平台的 Desktop 构建 |
| 网页目标模式 | 讨论需求、保存计划、确认后分节点执行与验收 | 支持 Host Goal 的 Server / Runner |
| 后台 ACP 目标模式 | 执行、检查、反馈修复；限制轮数、时间和无进展次数 | 单独配置 ACP Provider 及模型认证 |
| 交接与审核 | 保存计划与检查点，换对话后恢复，跟踪审核结论 | 使用持久目标记录和当前工具契约 |
| Office / PDF / OCR | 读取、修改、转换、备份恢复与页面预览 | 配置文档 worker、渲染器和相关依赖 |
| 绘图 | 流程图、架构图、数据图表及可编辑 JSON 规范 | 配置绘图 Python 环境及所需渲染工具 |

这些能力复用项目权限和现有执行通道。安装依赖或开启连接不会自动扩大模型的授权范围。

## 工作方式

```text
ChatGPT / Claude / 其他 MCP 客户端
                 │ MCP / HTTPS
                 ▼
              Server
        API、权限、计划、持久状态
                 │
                 ▼
              Runner
       文件、Git、命令、Job、插件
                 │
                 ▼
         本机仓库与开发 / 办公工具

Desktop：管理本地运行时、项目和连接设置
```

仓库文件仍保存在所属机器；读取的内容和工具输出可能发送给所连接的 AI 服务。详见[架构](docs/ARCHITECTURE.md)、[状态归属](docs/STATE_OWNERSHIP.zh-CN.md)及[认证模型](docs/AUTH_MODEL.zh-CN.md)。

## 开始使用

> **请替换为自己的插件名称：** 示例中的 `@WebCodex`（或 `@webcodex`）必须替换为你在 ChatGPT 中实际连接的插件名称。例如，家里的插件叫 `WebCodex-Hom`，就使用 `@WebCodex-Hom`；公司环境使用对应的公司插件。通过脚本新建聊天、接续对话或创建子 GPT agent 时，还需同步修改脚本中的 @ 输入、已选插件校验和授权弹窗名称匹配，保持名称一致。

### 日常使用：Desktop 或常驻 Server + Runner

Windows / macOS 可按 [Desktop 安装指南](docs/desktop-install.zh-CN.md)准备应用，再按[桌面使用指南](docs/desktop-guide.zh-CN.md)注册真实工作目录、配置连接并检查 Runner 状态。CLI、Linux、已有 Server 和高级部署见[完整使用指南](docs/PERSONAL_SETUP.zh-CN.md)。

需要本仓库的定制功能时，使用包含相应提交的构建。文档中指向上游的下载或 npm 命令用于上游版本；本仓库不预设自己的公开下载地址。

公网 HTTPS、Cloudflare Tunnel 和 OpenAI Secure MCP Tunnel 是连接 Server 的不同方式。API Key、Tunnel ID 和其他连接凭据在实际运行环境配置，不随源码分发。

### 本仓库快速运行：单项目临时分享

完成下文源码构建后，在待使用的项目中运行已构建的 CLI：

```powershell
# Windows：替换为自己的源码构建目录和项目目录
Set-Location 'C:\work\my-project'
& 'C:\work\webcodex\target\dogfood\webcodex.exe' share
```

```bash
# Linux / macOS：替换为自己的实际路径
cd /path/to/my-project
/path/to/webcodex/target/dogfood/webcodex share
```

`share` 是临时、单项目、受限环境，命令退出后临时连接失效；完整日常环境仍按 Server + Runner 配置。连接选项和边界见[快速试用](docs/QUICK_START.zh-CN.md)和 [CLI](docs/CLI.zh-CN.md)。

### 使用目标模式

网页目标模式由当前对话中的模型推进，不要求单独配置后台 ACP Provider：

```text
开启目标模式：完成这个项目的……，请先阅读资料、讨论需求并保存计划。
```

模型保存本地计划后，用户阅读并确认相应版本，随后按节点执行、检查和验收。更换对话前保存交接检查点，新对话通过恢复入口读取已保存状态。保存计划本身不授权执行。完整流程见[网页目标模式](docs/HOST_GOAL_MODE.zh-CN.md)。

需要后台持续执行、检查和反馈修复时，另行配置 ACP Provider，使用[后台目标模式](docs/GOAL_MODE.zh-CN.md)。后台循环受既定预算约束；模型结束回复或生成文件均不等于目标已经验收完成。

### 使用文档与绘图工具

配置 `plugins/mycodex-tools` 中的文档 worker 后，可使用 `present_document`、`document_edit`、`document_convert` 和 `document_restore`。修改基于当前源文件 SHA，原位修改先备份；转换保留原件。长操作返回 Job 时继续观察同一任务。详见[办公工作流](docs/OFFICE_WORKFLOW.zh-CN.md)和[插件说明](plugins/mycodex-tools/README.zh-CN.md)。

Office 原生读改和预览依赖本机 Office，Office 本体不打包；部分操作可以使用 OOXML 工具。PDF / OCR 使用 PDFium、Tesseract 等依赖，复杂 PDF 转 DOCX 不保证原版式还原。生成成功与视觉审核通过是不同结果，必须检查实际页面和审核状态。

绘图使用 JSON 规范、Graphviz 和 Matplotlib，可生成流程图、架构图及数据图表。规范、预览和产物清单便于继续修改，详见[绘图工作流](docs/DRAWING_WORKFLOW.zh-CN.md)。文档中的历史机器路径和项目 ID 必须替换为当前配置。

## 从源码构建

### 环境要求

- Rust stable / Cargo，以及目标平台需要的原生编译工具。
- Node.js / npm，用于网页前端和 Desktop 前端；依赖版本由各自 lockfile 管理。
- Windows 原生构建需要 MSVC Build Tools 和 Windows SDK；运行 Desktop 需要 WebView2。macOS / Linux 按目标平台准备原生开发依赖。
- Office / OCR / 绘图是可选扩展；启用时再准备插件指定的 Python、字体、渲染器和外部软件。

下面命令均在**本仓库根目录**执行。在本地资料集合中，对应目录是 `source`，不是它的父目录。

### Server / Runner / CLI

先构建 Server 内嵌的网页资源，再构建运行时：

```bash
npm ci --prefix frontend
npm run build --prefix frontend
cargo build --locked --profile dogfood --workspace --bins
```

产物位于 `target/dogfood/`，包括 `webcodex`、`webcodex-server` 和 `webcodex-runner`；Windows 带 `.exe` 后缀。`dogfood` 用于日常开发构建。正式发布使用 `release`，并遵循[发布流程](docs/agent/release-process.md)及[发布检查表](docs/RELEASE_CHECKLIST.md)。

### Desktop

Desktop 是独立 Cargo workspace，上面的运行时构建不会自动生成 Desktop：

```bash
npm ci --prefix apps/desktop
npm run build --prefix apps/desktop
cargo build --locked --profile dogfood --features tauri/custom-protocol --manifest-path apps/desktop/src-tauri/Cargo.toml --bin webcodex-desktop
```

Desktop 产物位于 `apps/desktop/src-tauri/target/dogfood/`（配置了 `CARGO_TARGET_DIR` 时以实际路径为准）。完整安装包还须准备对应 runtime、工具资源和 bundle 配置；直接编译 Desktop EXE 不等于已生成可分发安装器。安装包构建见[发布流程](docs/agent/release-process.md)和 `scripts/prepare_desktop_bundle.ps1` / `scripts/prepare_desktop_bundle_macos.py`。

## 验证与开发

按变更范围运行相关检查，例如：

```bash
python scripts/check_markdown_links.py --root .
npm run typecheck --prefix frontend
npm test --prefix frontend
npm run typecheck --prefix apps/desktop
npm test --prefix apps/desktop
cargo test --locked -p webcodex --lib metadata
```

纯文档修改无需编译 Rust；真实进程、端到端和发布测试有各自前置条件，见[测试指南](docs/TESTING.md)。本仓库保留历史验收报告，其结果只适用于记录中的源码、构建与环境，不能替代当前 checkout 的验证。

## 仓库结构

| 路径 | 内容 |
| --- | --- |
| `src/` | Server、HTTP / MCP 接口、工具调度及目标工作流 |
| `crates/` | Runner、CLI、核心协议、存储、权限契约及其他 Rust 模块 |
| `frontend/` | 管理和运行时网页、目标面板、构建与测试 |
| `apps/desktop/` | Tauri / React Desktop，含独立 Rust workspace |
| `plugins/mycodex-tools/` | Office / PDF / OCR、绘图和文档 worker |
| `plugins/safe-delete/` | 需显式配置及授权的删除插件 |
| `scripts/` | 构建、验证、部署和发布工具 |
| `docs/` | 用户、架构、功能和运维文档 |
| `.github/workflows/` | CI 与发布工作流；自己的仓库需核对发布目标和 secrets |
| `npm/webcodex/` | npm 包装与发布工具，现有包名属于上游 |

## 平台范围

| 平台 | 基础运行时 | Desktop |
| --- | --- | --- |
| Windows x64 | CLI、Runner、本机前台 Server、临时 share | 支持；定制办公流程重点在此平台 |
| Windows arm64 | CLI、Runner、前台 Server、share；Cloudflare 需可信外部二进制 | 当前安装器面向 x64 |
| macOS x64 / arm64 | CLI、Server、Runner、share | 支持；扩展依赖需另行核对 |
| Linux x64 / arm64 | CLI、Server、Runner、share | 以 CLI / 自托管路径为主 |

基础运行时支持平台不代表 Office 原生功能或每项定制能力已在该平台验收。Windows 独立 Server 的服务托管与 Desktop 自己管理的前台运行时有不同边界，见[部署指南](docs/DEPLOYMENT.zh-CN.md)。

## 文档导航

- [安装与连接](docs/desktop-install.zh-CN.md)、[完整配置](docs/PERSONAL_SETUP.zh-CN.md)、[故障排查](docs/TROUBLESHOOTING.zh-CN.md)
- [网页目标模式](docs/HOST_GOAL_MODE.zh-CN.md)、[后台 ACP 模式](docs/GOAL_MODE.zh-CN.md)、[审核反馈](docs/REVIEW_FEEDBACK_LOOP.zh-CN.md)
- [办公工作流](docs/OFFICE_WORKFLOW.zh-CN.md)、[绘图工作流](docs/DRAWING_WORKFLOW.zh-CN.md)
- [架构](docs/ARCHITECTURE.md)、[状态归属](docs/STATE_OWNERSHIP.zh-CN.md)、[MCP](docs/MCP.zh-CN.md)、[CLI](docs/CLI.zh-CN.md)
- [安全](SECURITY.md)、[贡献](CONTRIBUTING.zh-CN.md)、[完整文档索引](docs/INDEX.zh-CN.md)

## 在自己的 GitHub 上维护

源码仓库可独立构建；父目录中的安装器、离线工具链和本机迁移资料不属于必需源码。推送前保留并整理实际修改及新增文件，检查待提交内容和历史中的凭据；不要把旧 HEAD 当作包含全部定制功能的版本。

保留上游许可证和已有版权声明。发布自己的 npm 包、容器镜像或安装器前，核对包名、仓库元数据、CI 发布目标、secrets 和下载地址；现有发布脚本中的上游目标不应直接用于个人仓库发布。仓库名、远程地址和下载链接以自己的实际设置为准。

## 贡献与许可证

提交问题时说明版本 / commit、操作系统、客户端、复现步骤和经过脱敏的日志。贡献应保持范围清晰，并附相关验证；详见 [CONTRIBUTING.zh-CN.md](CONTRIBUTING.zh-CN.md) 和 [AGENTS.md](AGENTS.md)。

基于 [yyjeqhc/webcodex](https://github.com/yyjeqhc/webcodex) 开发，感谢上游贡献者及 [LINUX DO](https://linux.do/) 社区。保留上游 [Apache License 2.0](LICENSE)、适用于原创新增与修改的 [MIT](LICENSE-MIT) 及[来源声明](NOTICE)；[许可范围](LICENSING.md)说明两者边界。第三方依赖按各自许可证使用。
