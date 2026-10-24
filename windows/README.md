# Windows 当前功能源码快照

**去家里的电脑继续开发：先看 [完整客户端下载与开发接续说明](HOME-DEVELOPMENT.zh-CN.md)。现用客户端、工具依赖及离线准备材料另附在 GitHub 的 `windows-home-20261024` 草稿资产中。**

此分支保存 2026-10-24 Windows 部署所用的核心源码和配套工具。部署证据指向的实际构建目录与原开发 checkout 不同；本快照采用实际构建目录的源码，并补入原先在仓库外的工具。它是可构建的源码交付，不是包含个人账号、会话数据库的磁盘镜像。

GitHub 目标为 `lx923367787-commits/WebCodex`，分支 `windows/current-20261024`。本次不更新 `main`、iOS/Linux 分支、发布标签或安装服务。共用 Rust 模块必须保留，才能构建 Windows；原有其他平台源码和发布脚本保留原状。

## 包含的功能和位置

| 功能 | 源码 |
| --- | --- |
| Server、Runner、CLI、MCP、会话、工具契约 | `src/`、`crates/`、`frontend/` |
| Windows Desktop、AI Office、团队任务、主机控制和自动接续 carrier | `apps/desktop/` |
| Goal、原生 Agent 执行、隔离、恢复、CLI fallback | `src/`、`crates/` 中对应模块及测试 |
| Word/Excel/PowerPoint/PDF/OCR、文件预览/编辑/转换/恢复 | `plugins/mycodex-tools/` |
| 图表、流程图、架构图与可复现素材包 | `plugins/mycodex-tools/drawing/`、`draw.py` |
| 网页主机、自动接续和子助手工具 | `windows/chat-handoff/`、`windows/chat-agents/` |
| 接续传输适配器、Goal ACP 适配器 | `windows/subagent-pilot/`、`windows/goal-provider/` |
| 安全删除工具 | `plugins/safe-delete/` |

## 新电脑构建

使用 Windows x64，在 Visual Studio 2022 **Developer PowerShell** 中运行 PowerShell 7.2+。安装 MSVC C++ Build Tools、Windows SDK、Rust MSVC 工具链、Node.js/npm、Python 3.11、Git 和 WebView2 Runtime。已核验的版本、依赖下载地址、SHA-256 和许可见 [DEPENDENCIES.zh-CN.md](DEPENDENCIES.zh-CN.md) 与 [dependencies.json](dependencies.json)。Cargo 和 npm 的 lockfile、Office 和绘图的 Python lockfile 均随源码保存。

```powershell
git clone --branch windows/current-20261024 --single-branch https://github.com/lx923367787-commits/WebCodex.git
Set-Location WebCodex
pwsh -File windows/build.ps1 `
  -PythonBase C:\BuildDeps\python-3.11.9-embed-amd64 `
  -TesseractBase C:\BuildDeps\tesseract `
  -GraphvizRoot C:\BuildDeps\graphviz `
  -GraphvizArchive C:\BuildDeps\graphviz-16.1.0.zip
```

`PythonBase` 必须是新解压的官方 embedded Python，不能是已加装 `sitecustomize.py` 的旧运行包。`TesseractBase` 包含引擎/DLL、`doc/`、`tessdata/chi_sim.traineddata`、`eng.traineddata` 和 `configs/tsv`。`GraphvizRoot` 是 ZIP 中含 `bin/dot.exe` 的根目录。字体及 OFL 许可已随源码附带。脚本验证 Graphviz 压缩包和解压内容，按锁文件安装 Office/绘图依赖，执行前端测试和无窗口资源检查，在新的 `ignore/windows-build-*` 目录产生完整 Desktop、三个运行时 exe、Office/OCR/绘图包及工具源码和文件校验清单。

脚本不安装或启动服务、不调用浏览器、不触碰旧安装目录。它产生便携目录，不创建旧版 NSIS 安装器的注册表和卸载入口。

## 工具和账号配置

1. 按[聊天工具安装说明](chat-agents/INSTALL.zh-CN.md)运行 `windows/setup-providers.ps1`，以 `-ToolRoot` 指向本次构建输出的 `WebCodex Desktop` 目录。它安装 Python/npm 锁定依赖并生成新机器的 provider 路径模板；保留 `chat-agents`、`chat-handoff`、`subagent-pilot` 相邻目录关系。输出目录也附带相同的 `setup-providers.ps1`，可在目标机器重新准备路径。
2. 在新机器初始化 Desktop 的本地 Server/Runner，登记实际项目目录。Office provider 模板见 `plugins/mycodex-tools/runner.example.toml`，将路径指向本次构建输出的 `mycodex-tools`，包括 `document_worker`、字体与 OCR 路径。
3. 配置 `chat-agents` provider 后，Desktop 的主机控制代码为自己拥有的本地 runtime 生成私有 carrier；Runner lifecycle 管理自动队列。不要复制旧电脑的 carrier、token、租约、窗口 ID 或会话数据库。
4. 登录自己使用的 ChatGPT/外部 CLI 账号，并完成浏览器扩展授权。网页接续依赖该账号的可用档位和页面行为；首次登录、扩展设置和权限配置由使用者完成。
5. 原生 Office 渲染/转换需安装已授权的 Microsoft Office。Office、WebView2、浏览器和第三方 CLI 本体不属于本项目自有源码，不在 Git 中重新分发；对应安装条件保留在依赖文档中。

源码恢复不等于自动获得原账号权限。测试报告见 [VALIDATION.md](VALIDATION.md)，其中区分本次通过的离线检查、既有运行证据和需要新机账号/软件后才能完成的端到端验收。
