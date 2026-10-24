# Windows 依赖与离线准备

本目录记录 Windows x64 当前功能所需的外部运行时和可复用依赖。精确下载地址、文件大小、SHA-256 和来源在 [`dependencies.json`](dependencies.json)。这些文件可以放在任意本地磁盘的新目录，不依赖原开发机器的路径。

对应的 Windows 离线运行包使用方法见 [PORTABLE.zh-CN.md](PORTABLE.zh-CN.md)。它将可移植运行时与源码分开交付；`configure-providers.ps1` 独立生成新电脑路径，不要求联网安装依赖。Python venv 必须在目标机重建，不能直接复制旧机器的 `.venv`。

## 源码内已提供的内容

Rust Server/Runner/CLI、Desktop、网页前端、Office/PDF 文档工具和绘图工具的源码与锁文件均在仓库中。中文字体 [`assets/fonts/ZCOOLXiaoWei-Regular.ttf`](assets/fonts/ZCOOLXiaoWei-Regular.ttf) 和原始 [`OFL.txt`](assets/fonts/OFL.txt) 一并保存；复制或打包字体时保留许可证。构建时继续使用仓库锁文件，避免执行依赖升级命令。

| 组件 | 当前版本或约束 | 获取依据 |
| --- | --- | --- |
| 嵌入 Python | 3.11.9，Windows x64 | [Python 官方版本页](https://www.python.org/downloads/release/python-3119/) |
| Office/PDF Python 包 | `plugins/mycodex-tools/requirements.lock` 精确版本 | PyPI，构建脚本使用二进制 wheel、禁止隐式依赖解析 |
| PDFium | pypdfium2 5.13.0，当前内置 PDFium 153.0.7999.0 | 随 pypdfium2 wheel 提供 |
| Tesseract | 5.5.3.20260724，Windows x64 | [发布方 Windows 说明](https://github.com/UB-Mannheim/tesseract/wiki)、[上游发布页](https://github.com/tesseract-ocr/tesseract/releases/tag/5.5.3) |
| OCR 模型 | `tessdata_fast` 的 `eng`、`chi_sim`，内容由 SHA-256 固定 | [上游模型仓库](https://github.com/tesseract-ocr/tessdata_fast) |
| Graphviz | 16.1.0，Windows x64 ZIP | [Graphviz 官方下载页](https://graphviz.org/download/)及同页 SHA-256 文件 |
| 图表 Python 包 | `plugins/mycodex-tools/drawing/requirements.lock` 精确版本 | 独立安装到 `drawing/vendor`，保留 Office 包版本 |
| 中文字体 | ZCOOL XiaoWei 原始 TTF，OFL-1.1 | [Google Fonts 源目录](https://github.com/google/fonts/tree/main/ofl/zcoolxiaowei)，仓库已含相同字节 |
| Goal ACP provider | `windows/goal-provider/package-lock.json` | `npm ci` 恢复锁定 npm 包及 Windows 可选依赖 |
| 网页接续与子任务 | `windows/chat-handoff/requirements.lock` 与 `extension-runtime/package-lock.json` | 专用 Python venv；官方 Playwright MCP 扩展依赖 |

## 手动准备外部运行时

以下 PowerShell 示例在克隆仓库根目录执行。将输出目录换成一个尚不存在的新目录；下载和解压不会安装系统服务。`7z.exe` 是解开 Tesseract NSIS 包的命令行解压器，应由构建机提供。

```powershell
$dependencyRoot = Join-Path $PWD 'ignore/windows-dependencies'
if (Test-Path -LiteralPath $dependencyRoot) { throw '请为依赖选择新的空目录。' }
New-Item -ItemType Directory -Path $dependencyRoot | Out-Null
$manifest = Get-Content -Raw -LiteralPath '.\windows\dependencies.json' | ConvertFrom-Json

foreach ($artifact in $manifest.artifacts) {
    if ($artifact.repository_path) { continue }
    $destination = Join-Path $dependencyRoot $artifact.filename
    Invoke-WebRequest -Uri $artifact.url -OutFile $destination
    $actual = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $artifact.sha256) { throw "SHA-256 不一致：$($artifact.id)" }
}

Expand-Archive -LiteralPath (Join-Path $dependencyRoot 'python-3.11.9-embed-amd64.zip') `
    -DestinationPath (Join-Path $dependencyRoot 'python-base')
Expand-Archive -LiteralPath (Join-Path $dependencyRoot 'windows_10_cmake_Release_Graphviz-16.1.0-win64.zip') `
    -DestinationPath (Join-Path $dependencyRoot 'graphviz')

$tesseractRoot = Join-Path $dependencyRoot 'tesseract-base'
& 7z.exe x (Join-Path $dependencyRoot 'tesseract-ocr-w64-setup-5.5.3.20260724.exe') "-o$tesseractRoot"
if ($LASTEXITCODE -ne 0) { throw 'Tesseract 解压失败。' }
Copy-Item -LiteralPath (Join-Path $dependencyRoot 'eng.traineddata') `
    -Destination (Join-Path $tesseractRoot 'tessdata\eng.traineddata')
Copy-Item -LiteralPath (Join-Path $dependencyRoot 'chi_sim.traineddata') `
    -Destination (Join-Path $tesseractRoot 'tessdata\chi_sim.traineddata')
```

解压后应存在：

```text
windows-dependencies/
  python-base/python.exe
  python-base/python311.zip
  tesseract-base/tesseract.exe
  tesseract-base/*.dll
  tesseract-base/doc/
  tesseract-base/tessdata/configs/tsv
  tesseract-base/tessdata/eng.traineddata
  tesseract-base/tessdata/chi_sim.traineddata
  graphviz/Graphviz-16.1.0-win64/bin/dot.exe
  windows_10_cmake_Release_Graphviz-16.1.0-win64.zip
```

保留 Graphviz 原 ZIP：`build_drawing_addon.py` 同时核对 ZIP 哈希和解压文件集合。Tesseract 的执行文件与 TSV 配置哈希也列在清单中，可以进一步验证提取结果。`main` 下载地址如日后变化，必须停止并核查；不要自动接受新哈希。

## 构建机与目标机条件

构建需要 Windows x64、PowerShell 7、Git、Rust MSVC 工具链、Visual Studio C++ Build Tools 与 Windows SDK、Node.js 22 或更新的兼容版本/npm，以及带 pip 的 Python 3.11 x64。嵌入 Python 仅用于随包运行，不作为 pip 的执行环境。网页接续的 Python 包安装到独立 venv，不覆盖 Office 包的 pywin32 等依赖版本。

Desktop 运行需要 Microsoft WebView2。完整原生 Word/Excel/PowerPoint 读取、编辑、重算、旧格式转换和页图渲染还需要目标机器自行安装并授权 Microsoft Office；当前实现使用本地 Office COM API。仓库不分发 Office 本体或授权。常规 OOXML、PDF/OCR 和绘图依赖由上表锁定。

各 agent CLI、浏览器接续及 tunnel 使用目标机器自己的账号和配置。浏览器相关能力还需要相应浏览器扩展、可连接的浏览器会话及 Windows 工具配置；按各子目录说明部署。凭据、浏览器 profile、个人文档、数据库和旧机器的任务记录不属于构建材料。

## 验证与复现边界

下载后先比较清单 SHA-256；从 `build_local_bundle.py` 生成的新包会验证 Python 依赖、pywin32、PDFium 和 OCR 语言，并写出包内文件清单。绘图扩展独立校验 Graphviz。应再执行办公工具单元测试、绘图 `doctor` 和项目提供的 Windows 验证脚本；涉及本机 Office 的检查必须在已安装 Office 的目标机验证。

当前 Python requirements 锁定版本，但未记录所有 wheel 哈希。Rust 的编译器、SDK 和构建时间也会影响最终 EXE 字节；本清单用于恢复同版本依赖与功能，不能据此宣称二进制逐字节可重现或已在一台全新机器验证。
