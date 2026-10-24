# Windows 新电脑继续开发与离线运行

源码分支与 Windows 离线运行包承担不同用途：克隆 `windows/current-20261024` 继续修改代码；解压对应 Windows 运行包即可使用已构建的 Desktop、Server/Runner、Office/PDF/OCR/绘图工具和聊天 provider。运行包不要覆盖到现有安装目录，先解压到新的可写目录，例如 `C:\WebCodexPortable\WebCodex Desktop`。

## 运行包目录约定

```text
WebCodex Desktop/
  WebCodex.exe
  webcodex-runtime/                 # webcodex、server、runner 可执行文件
  mycodex-tools/                    # Office/PDF/OCR/绘图完整独立运行时
  provider-python/                  # 聊天工具的独立 Python 3.11 及锁定依赖
  node/                            # Node 可执行文件及许可证
  tunnel/tunnel-client.exe          # Tunnel 客户端 0.0.12，不含 Tunnel ID
  chat-handoff/extension-runtime/node_modules/
  chat-agents/
  subagent-pilot/
  goal-provider/node_modules/
  safe-delete/
  configure-providers.ps1
  setup-providers.ps1
  launch-portable.ps1
  launch-portable.cmd
```

保留这些目录的相邻关系。`provider-python` 与 Office 自带的 Python 包版本不同，不能混用。离线运行包使用相对路径的独立 Python，不携带原电脑的 `.venv`。

## 新路径配置：可离线执行

将源码克隆到家用电脑的开发目录后，使用 Windows 自带的 PowerShell 5.1 或更新版执行：

```powershell
$app = 'C:\WebCodexPortable\WebCodex Desktop'
powershell -NoProfile -ExecutionPolicy Bypass -File "$app\configure-providers.ps1" -ProjectRoot 'C:\code\WebCodex'
```

这个命令只在包内 `provider-config` 生成新的绝对路径模板，不安装依赖、不联网、不读取旧账号、不修改 Runner、不启动应用或服务。它会优先使用包内 `provider-python/python.exe` 和 `node/node.exe`，并生成 `edge-handoff`、`chat-agents`、Goal ACP、Office 四份 TOML 片段。Office 的 `cwd` 必须是 Runner 登记的项目根目录，因此 `-ProjectRoot` 应填实际项目目录。对其他部署布局可显式提供 `-ToolRoot`、`-Python`、`-Node`、`-OfficeRoot`、`-OutputDirectory`。

初始化本机 Desktop/Server/Runner 并登记项目后，将需要的片段合并到该 Runner 配置，避免重复 provider id 或重复 `[plugins]` 表。默认超时可以保留 Runner 已有值。Desktop 管理 Host 的私有 carrier、控制授权和 `runtime_lifecycle`；不能把旧机器生成的 carrier、token、租约或队列当成新机配置。

需要启动客户端时，由使用者双击 `launch-portable.cmd`，无需另装 PowerShell 7；也可在已经安装 PowerShell 7.2+ 的机器运行：

```powershell
pwsh -File "$app\launch-portable.ps1"
```

启动脚本优先选择包内 `node/node.exe`；源码构建目录没有随包 Node 时，只使用 PATH 中第一个真实的 `node.exe`。两处都没有 Node 或缺少 `WebCodex.exe` 时会报错并停止。脚本只向新客户端传入所选 Node 的 `WEBCODEX_NODE` 和 PATH，以及存在时的 `WEBCODEX_TUNNEL_CLIENT_BIN`，不修改系统 PATH。用户自行安装的 CLI 仍通过原有环境和产品配置发现，启动器不指定或附加独立 CLI 路径。Desktop 的部分工具发现逻辑优先查当前用户的安装目录；目标机若另装了不同版本，应在该功能的配置中选择期望的可执行文件。脚本不会写入 Runner 配置或安装系统服务。

离线包保留 Goal ACP 锁定依赖内置的 Codex `0.153.4`，它是适配器运行依赖。其他独立 CLI 由用户按需另行安装并登录；包中不携带这些软件或原电脑账号和认证资料。首次模型调用需要在新机完成相应登录。

## 从源码重新准备聊天依赖

源码 checkout 不把 `node_modules`、Python 环境或构建输出放进 Git。重新构建用 `build.ps1`；从源码安装聊天依赖可执行：

```powershell
pwsh -File windows/setup-providers.ps1 -Python 'C:\Python311\python.exe'
```

它使用当前机器完整安装的 Python 3.11 创建 `.venv`，安装锁定的 Python/npm 依赖，再调用独立配置脚本。**Python venv 不能跨电脑直接搬运**：`pyvenv.cfg` 与启动器依赖原解释器路径。迁移源码后在新电脑创建新 venv；如目标目录已有从旧机复制的 `.venv`，先将它移到备份目录，再运行安装脚本。不要使用 Office 嵌入解释器创建 venv，也不要覆盖当前在用环境。

如果已经准备齐 `requirements.lock` 的全部 Windows x64 / CPython 3.11 wheels，以及两个 provider 的锁定 `node_modules`，可完全禁止安装期间联网：

```powershell
pwsh -File windows/setup-providers.ps1 `
  -ToolRoot 'C:\WebCodexPortable\WebCodex Desktop' `
  -Python 'C:\Python311\python.exe' `
  -Offline -Wheelhouse 'C:\WebCodexDependencies\chat-wheels' `
  -ProjectRoot 'C:\code\WebCodex'
```

`-Offline` 要求显式 wheelhouse，用 `pip --no-index` 安装并检查依赖，保留已打包的 npm 目录。对已经附带 `provider-python` 和 `node_modules` 的离线运行包，只需 `configure-providers.ps1`，无需重复安装或创建 venv。

## 需要在目标机器重新配置的内容

API key、Tunnel ID、登录令牌、浏览器登录资料、账号数据库、个人任务和旧机器的私有控制凭据不随公开 GitHub 包分发。新机器使用自己的登录与授权。正常使用网页接续还需要 Edge、相应 Playwright 扩展及本机扩展 token；原生 Word/Excel/PowerPoint COM 功能需要已安装、已授权的 Microsoft Office；Desktop 需要 WebView2。第三方商业软件及其许可证不能由源码包替代。

离线包省去运行时依赖下载，并不表示模型服务或网页接续能离线使用。继续编译开发仍需 Git、Rust MSVC 工具链、Windows SDK、Visual Studio C++ Build Tools、Node/npm 及构建所需依赖缓存。包内文件清单可以核对文件完整性；账号相关和 Office 操作必须在家用电脑完成一次端到端验收。
