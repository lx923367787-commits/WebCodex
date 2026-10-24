# 在家里的 Windows 电脑继续开发

本次交付包含两部分：`windows/current-20261024` 分支保存全部项目源码和自有开发工具；`windows-home-20261025` GitHub 草稿附件保存当前 Windows 客户端与运行依赖。草稿附件需要登录仓库所有者 `lx923367787-commits` 才能访问。保持草稿是为了不触发现有 `release: published` 的 Linux 镜像发布流程；这不是一个跨平台版本发布。

## 已纠正的打包范围

旧迁移包误收了 WorkBuddy/CodeBuddy、反重力和独立 Codex CLI 程序本体，已从 GitHub 撤下。请使用文件名带 `-corrected` 的新包。这三类外部程序由家里电脑自行安装和登录，仓库继续保留 WebCodex 对接代码。

`goal-provider/node_modules` 中的 Codex 0.153.4 是 Goal ACP 适配器默认启动 `app-server` 所需的固定依赖，明确作为该组件的私有运行依赖保留；它与已移除的全局 Codex 0.159.2 安装包不同。需要改用系统 Codex 时，必须按适配器的 `CODEX_PATH` 和 Runner 显式环境映射契约配置，不能只删目录。

## 下载与核对

在家中安装 Git/GitHub CLI 后，以同一 GitHub 账号登录：

```powershell
gh auth login
gh api user --jq .login
gh release download windows-home-20261025 --repo lx923367787-commits/WebCodex --dir .\WebCodex-Transfer
Get-Content .\WebCodex-Transfer\SHA256SUMS.txt
Get-FileHash .\WebCodex-Transfer\WebCodex-Windows-Home-20261025-corrected.zip -Algorithm SHA256
Get-FileHash .\WebCodex-Transfer\WebCodex-Source-20261025-corrected.zip -Algorithm SHA256
```

将哈希与 `SHA256SUMS.txt` 对比，再解压客户端包到一个新的目录。不要覆盖另一套安装或复制旧卸载器。包中 `MANIFEST.json` 逐文件记录大小与 SHA-256，独立的 `Windows-Package-MANIFEST.json` 供下载前检查。

客户端包保存的是当前安装的 `WebCodex.exe`、Server/Runner/CLI 三个 EXE，以及它们各自的真实版本和哈希；没有伪装成用新的 Git 提交重新编译的发行版。旧二进制的 `dirty=true` 构建标记原样保留。源码和这些运行文件的部署对应关系已经核对。

## 包里有什么

| 材料 | 位置/用途 |
| --- | --- |
| 现用 Windows Desktop 与三个运行时 EXE | `WebCodex.exe`、`webcodex-runtime/` |
| Office/PDF/OCR/绘图工具及运行依赖 | `mycodex-tools/`，含嵌入 Python、PDFium、Tesseract、中文/英文模型、Graphviz、Matplotlib、字体 |
| 网页 Host、接续、子助手和开发辅助脚本 | `chat-handoff/`、`chat-agents/`、`subagent-pilot/` |
| ACP 接口及其独立 Codex 0.153.4 | `goal-provider/`，含锁定的 `node_modules` |
| Node 24.15.0 与独立聊天 Python | `node/`、`provider-python/`；没有搬运不可移植的旧 `.venv` |
| Python 离线重建材料 | `offline/provider-wheels/` 和 `offline/` 内 embedded Python 压缩包 |
| OpenAI Tunnel 客户端 0.0.12 | `tunnel/tunnel-client.exe`；不含 Tunnel ID 或 API key |
| 安全删除插件 | `safe-delete/` |
| 路径配置和启动入口 | `configure-providers.ps1`、`setup-providers.ps1`、`launch-portable.cmd` |

Gemini CLI 没有安装在此次原机清单中，不能把 Antigravity 或它的模型选项当成 Gemini CLI。所有外部 CLI 的账号和模型访问权限仍由新机登录决定。

## 2026-10-25 连接修复

请使用本页的 20261025 新包。此前 20261024 包里的 Server 尚未包含首次聊天自动注册修复。新包保持现用 Desktop/Runner/CLI 和工具依赖，并更新 Server。新电脑必须按 [NATIVE-HOST-SETUP.zh-CN.md](NATIVE-HOST-SETUP.zh-CN.md) 配置本机 Host 的自动注册；只填写 API key/Tunnel ID 不能替代这一步。不要复制旧机 Host、OAuth 绑定、carrier 或数据库。新聊天接入仍保持独立 Agent 隔离，CLI/子助手的执行限制沿用原策略。

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
