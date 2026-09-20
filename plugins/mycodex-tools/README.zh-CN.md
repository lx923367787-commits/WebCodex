# MyCodex 文档工具插件

[English](README.md)

为现有 WebCodex 增加 11 个无界面的 Word、Excel、PowerPoint、PDF 工具。
文件、PowerShell、Python、Job 和图片回传继续使用原生接口；不增加 Agent 框架或第二个 MCP 服务。

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

- 单文件 32 MiB、Office ZIP 解压总量 128 MiB、调用预算 60 秒；结果还受现有 JSON 限制。
- 路径拒绝越界、绝对路径、符号链接/junction、凭据与 Git 控制路径。
  这是防误操作检查；受信任本地插件不提供操作系统沙箱。
- 目标已存在、源哈希过期、文件损坏、缺少字体、排版溢出均明确失败。
  对 `OutcomeUnknown` 必须先回读核验，不自动重放写入；`busy` 不表示已排队。
- Word 替换内容可能改变该处行内格式；Excel 的复杂对象、外部链接不保证无损。
  公式保存为公式文本，不重新计算、不冒充最新计算值。
- PDF 不做 OCR，不恢复语义表格；扫描页报告无可提取文字。中文生成必须指定覆盖中文的本机 TTF 字体，
  缺失即报错，安装包不分发系统字体。
- PPT 采用基础可编辑布局，标题 32 pt、正文 24 pt，不复制动画、SmartArt 或复杂母版。
- UTF-8 是默认文本编码；旧编码通过 Python 显式指定，不自动猜测或覆盖转换。

依赖选用 python-docx、openpyxl、python-pptx、pypdf、ReportLab、Pillow 和 defusedxml。
只做薄适配，不复制第三方解析器。无需 Office、COM、LibreOffice、OCR、OpenCV 或付费 API。

## 测试与回退

聚焦测试：专用 Python 执行 `-m unittest discover -s tests -p 'test_*.py' -v`。
真实链路测试：执行 `tests/e2e_mcp.py --help` 查看参数；仅启动自有临时 Server/Runner，
使用合成测试凭据，不触碰现有服务。实际结果见 [测试报告](TEST_REPORT.md)。

需要停用时，从指定 Runner 配置移除该 provider 后热加载；已生成文件保留。
后续 OCR、丰富模板、渲染和更多项目接入分别评估，不扩大首期接口。
