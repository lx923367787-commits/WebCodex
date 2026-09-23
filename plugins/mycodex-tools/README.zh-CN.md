# MyCodex 文档工具插件

[English](README.md)

新版通过现有 WebCodex MCP 提供四个日常办公入口，复用 Runner Job、项目权限和原生图片回传，不增加第二个 MCP 服务。详细流程见 [办公工作流](../../docs/OFFICE_WORKFLOW.zh-CN.md)；便携包内也附有 `docs/OFFICE_WORKFLOW.zh-CN.md`。

| 入口 | 用途 |
| --- | --- |
| `present_document` | 读取正文、表格及每次最多两页真实图像；短分页及来源 SHA 绑定的续读 |
| `document_edit` | 按结构化操作修改；默认原文件写回，先验证并自动备份，也可显式另存副本 |
| `document_convert` | 转换到新文件，保留原件；扫描 PDF/图片可 OCR 到可搜索 PDF 或可编辑文字 DOCX |
| `document_restore` | 列出备份，或校验当前 SHA 后恢复；恢复也生成修改前备份 |

Windows 办公读改优先使用本机 Office 2021 对象接口，禁止 GUI 点击；不会接管用户正在使用的 Office 实例。后台处理进入现有 Job 后，继续观察相同 Job，不能重新提交修改。原位修改须提供读取时的 `expected_sha256`；文件被占用或版本过期会拒绝写入。遇到未知结果先查文件与回执，不自动重放。

PDF/PNG/JPEG/WEBP 支持 PDFium 渲染和本地 Tesseract 简体中文、英文 OCR。PDF 局部换字限可唯一定位的完整顶层文字对象，保留其位置；缺字可使用随包中文字体，但遇到溢出、跨对象、旋转、透明、裁切或带替代文字的对象会拒绝，并建议转可编辑文档。中文表单填写限未旋转的单行文字字段。PDF 转 DOCX 统一使用文字提取或 OCR 重排，不调用 Word 的 PDF 导入，也不承诺恢复复杂表格和版式。

单文件 64 MiB，Office ZIP 实际解压总量 256 MiB；原生每张图像最多 1 MiB，超限可裁切或缩小页范围。Tesseract、依赖和中文字体可随便携候选包携带，Office 本体不打包。候选包生成不会安装、改配置或重启现有服务。

## 构建便携候选包

`build_local_bundle.py --help` 提供可重复构建命令。先用 `--runtime-only` 和 `--python-base`、`--font-dir`、`--tesseract-base` 准备独立运行时；源码确认后以 `--prepared-runtime` 输出到一个全新目录。脚本拒绝覆盖目标和链接路径，按 `requirements.lock` 安装精确依赖，并验证 pywin32、PDFium、Tesseract 及语言数据。`MANIFEST.json` 记录源码 HEAD/dirty、依赖版本及每个文件的 SHA-256（不包含清单自身）。

配置 `runner.example.toml` 中的 `document_worker`、`document_worker_args` 及准确项目根后，四入口才可用；下面保留旧 11 个插件工具的兼容说明。

## 旧 11 工具兼容接口

旧 **WebCodex Desktop 工具增强版安装器** 已随包提供旧接口的独立 Python、固定依赖和开源中文字体；这不表示现有安装已经升级为上面的四入口。
用户在桌面端选择工作项目后自动启用这些工具；Tunnel ID 和 API Key 由用户自己在桌面端填写。
无需运行本目录的安装脚本。下面的安装步骤仅用于源码部署或手动接入已有 Runner。

## 安装与启用

在本目录运行 `./install.ps1`，使用 Python 3.11 创建独立环境并按
`requirements.lock` 安装固定版本。默认环境位于
`E:\wbecodex\.local\venvs\mycodex-tools`，不会改动全局 Python 或现有服务。

`runner.example.toml` 是待审阅的配置片段。实施交付不会自动修改现有 Runner：

1. 将 provider 合并到指定可写 Runner 的 `[plugins]`，不要重复创建同名 TOML 表。
2. 设置绝对解释器、脚本、中文 TTF 字体路径；`cwd` 必须是目标项目根。
   样例使用已注册的 `E:\wbecodex\sandbox`。
3. 客户端需有 `plugin:inspect`、`plugin:invoke`；执行检查和热加载另需 `plugin:manage`。
   不自动扩大现有凭据权限，也不向只读 Runner 添加写入能力。
   shared-key OAuth 接入可使用 `--oauth-local-plugins` 申请 inspect/invoke；该选项不授予
   manage。已有客户端权限变化需要重新授权。
4. 按 `plugin_tool check → reload → list → describe → call(binding)` 启用和调用。
   热加载后重新 describe，旧 binding 不再有效，无需重启 Runner。

插件工作目录不会跟随模型当前项目自动变化。原生文件、执行和产物工具必须显式选择
同一个 sandbox 项目。其他目录的输入先通过已授权工具复制或导入；若需另一项目，
由本地配置另建绑定该根目录的 provider，模型不能传入任意根目录。

## 工具与闭环

| 工具 | 首期能力 |
| --- | --- |
| `docx_read/create/update` | 正文、标题样式、顶层表格；基础创建、指定段落/单元格替换、末尾追加 |
| `xlsx_read/create/update` | Sheet 列表、A1 区域读取、多 Sheet 创建、带类型的单元格更新 |
| `pptx_read/create` | 页数、标题、形状文字与表格；16:9 基础可编辑幻灯片 |
| `pdf_read/search/create` | 分页文字、页数、元数据、书签；纯文本检索；基础 A4 段落、标题与表格生成 |

所有创建/修改都指定新的 `output_path`，父目录须已存在。Word/Excel 修改还需
`path` 和上次读取的 `expected_sha256`；源文件变化则拒绝，源文件始终保留。
写入先保存临时文件、回读验证，再原子发布到不存在的目标。
Excel 另存还会核验未编辑的基础单元格；若发生数值精度或类型变化，拒绝发布副本。

例如 `docx_create` 的参数：

```json
{"output_path":"test.docx","blocks":[{"type":"heading","text":"工作汇报","level":1},{"type":"paragraph","text":"WebCodex document test"}]}
```

随后 `docx_read {"path":"test.docx"}` 回读，再使用对应项目的
`read_project_artifact_metadata` 检查实际哈希、大小和类型。完整接口、修改示例和
Excel 类型字段见英文 README；运行时以 `plugin_tool describe` 返回的 schema 为准。

Word/PPT 的段落、表格、页索引从 0 开始；PDF 页码从 1 开始。
大内容按范围读取，长文本用 `text_offset/max_chars` 续读，并检查返回的总量及剩余标记。
大量 Sheet 使用 `sheet_start/sheet_count` 分页；单页大量 PPT 对象使用
`shape_start/shape_count` 分页，避免只能缩小页数却仍无法读取。
超限会明确报错，不隐藏未返回内容。

图片用同一虚拟环境中的 Pillow 脚本，经 `run_process` 生成；再核对 PNG/JPEG
类型、尺寸，并通过 `read_project_artifact(as_image=true)` 返回真实图片。原生单图上限为 1 MiB。
Python 长任务继续观察同一个 Job，不重新执行命令。

## 明确限制

- 单文件 64 MiB、Office ZIP 解压总量 256 MiB；旧插件 RPC 调用预算 60 秒，结果还受现有 JSON 限制。
- 路径拒绝越界、绝对路径、符号链接/junction、凭据与 Git 控制路径。
  这是防误操作检查；受信任本地插件不提供操作系统沙箱。
- 目标已存在、源哈希过期、文件损坏、缺少字体、排版溢出均明确失败。
  对 `OutcomeUnknown` 必须先回读核验，不自动重放写入；`busy` 不表示已排队。
- Word 替换内容可能改变该处行内格式；Excel 的复杂对象、外部链接不保证无损。
  公式保存为公式文本，不重新计算、不冒充最新计算值。
- 旧 `pdf_read/search/create` 不做 OCR，不恢复语义表格；扫描页报告无可提取文字。中文生成必须指定覆盖中文的本机 TTF 字体，
  缺失即报错，安装包不分发系统字体。
- PPT 采用基础可编辑布局，标题 32 pt、正文 24 pt，不复制动画、SmartArt 或复杂母版。
- UTF-8 是默认文本编码；旧编码通过 Python 显式指定，不自动猜测或覆盖转换。

依赖选用 python-docx、openpyxl、python-pptx、pypdf、ReportLab、Pillow 和 defusedxml。
旧 11 工具只做薄适配，不复制第三方解析器；这些旧接口无需 Office/COM 或 OCR。新版四入口另用本机 Office、pywin32、PDFium 和 Tesseract，不需要付费模型 API。

## 测试与回退

聚焦测试：专用 Python 执行 `-m unittest discover -s tests -p 'test_*.py' -v`。
真实链路测试：执行 `tests/e2e_mcp.py --help` 查看参数；仅启动自有临时 Server/Runner，
使用合成测试凭据，不触碰现有服务。实际结果见 [测试报告](TEST_REPORT.md)。

需要停用时，从指定 Runner 配置移除该 provider 后热加载；已生成文件保留。
新版 OCR、渲染和原位修改走四个文档入口，旧接口继续保持兼容。

## 流程图、架构图与 Excel 数据图表

独立的 `draw.py` 通过现有 `run_process` 生成流程图、分组架构图，以及 XLSX/CSV 的柱状、折线、饼图和散点图。输出 PNG/SVG/PDF、可修改的 JSON 定义、数据快照和 ZIP；使用现有 `read_project_artifact(as_image=true)` 回传实际预览。

参见 [绘图工作流与调用示例](../../docs/DRAWING_WORKFLOW.zh-CN.md)。绘图依赖安装到独立的 `drawing/vendor`，不替换 Office 依赖，不增加 MCP 工具。`build_drawing_addon.py --help` 提供增量构建入口；目标必须是新目录。保留原工作簿，修改图定义后另存新版本。
