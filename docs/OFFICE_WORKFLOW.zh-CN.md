# 本地办公文件双向工作流

四个入口服务于短办公任务：先选已登记目录，让 GPT 读取正文、表格及真实页面；再按当前文件版本修改、转换或恢复。读取的是磁盘上已保存的文件，不读取 Office 窗口里尚未保存的内容。无需整库上下文、向量索引或文件实时同步。

## 工具与约定

| 工具 | 用途 |
| --- | --- |
| `present_document` | `view=auto/text/pages`，返回真实结构内容、源 SHA、按需最多两张页图和续读游标 |
| `document_edit` | 基于 `expected_sha256` 和结构化 `operations` 修改；默认原地保存并先备份，指定 `output_path` 默认另存副本 |
| `document_convert` | 按明确的 `output_path` 转换，保留源文件 |
| `document_restore` | 不传 `backup_id` 时分页列备份；传入备份 ID 和当前目标 SHA 时恢复 |

所有入口要求 `project` 是已登记项目 ID，文件路径相对于该目录。不能传绝对 root、解释器路径、命令或任意 argv。`tool_manifest(tool_name="document_edit")` 可取得操作列表及各格式示例。

`present_document` 的结构内容在 MCP `structuredContent` 中；实际图像是 MCP 原生 `image` 内容块，不是文本中的 Base64。`export_file=true` 可附带原件 ResourceLink；该链接可传输原文件，不表示网页客户端必然将其登记为聊天附件。此工作流不需要 GPT Actions。

`auto` 尽可能返回文本及页图；缺少渲染条件时仍可返回文字，并报告原因。`pages` 要求页图，不能生成时明确失败。检查 `visual_delivery`：`mcp_images` 才表示本次 MCP 响应带有原生页图，`partial/unavailable` 不应被描述为完整视觉检查。普通 HTTP 工具响应中的 `ready` 只表示预览产物已准备好。

## 示例

先读取当前内容和编辑定位符：

```json
{"project":"agent:my-runner:office","path":"本周汇报.docx","view":"auto"}
```

Word 替换一个已读取的段落中的文字；`target.id` 必须取自当前读取结果：

```json
{"project":"agent:my-runner:office","path":"本周汇报.docx","expected_sha256":"<当前64位SHA>","save_mode":"in_place","operations":[{"op":"replace_text","target":{"id":"word/document.xml#p0"},"find":"待完成","text":"已完成","occurrence":0}]}
```

Excel 单元格使用工作表及 A1 地址，明确值类型，避免把文字当公式：

```json
{"project":"agent:my-runner:office","path":"统计.xlsx","expected_sha256":"<当前64位SHA>","output_path":"统计-修订.xlsx","save_mode":"copy","operations":[{"op":"set_cells","sheet":"Sheet1","cells":[{"coordinate":"B2","type":"number","value":42},{"coordinate":"C2","type":"formula","value":"=B2*2"}]}],"options":{"recalculate":true}}
```

PowerPoint 幻灯片及段落索引从 0 开始，`shape_id` 使用读取返回的实际 ID：

```json
{"project":"agent:my-runner:office","path":"汇报.pptx","expected_sha256":"<当前64位SHA>","operations":[{"op":"set_text","target":{"slide":0,"shape_id":2,"paragraph":0},"text":"本周进展"}]}
```

PDF 页码从 1 开始。文字替换必须精确命中一个完整文本对象；复杂版式宜显式转换后修改：

```json
{"project":"agent:my-runner:office","path":"说明.pdf","expected_sha256":"<当前64位SHA>","operations":[{"op":"replace_text","page":1,"old_text":"Old title","new_text":"New title"}]}
```

转换及恢复：

```json
{"project":"agent:my-runner:office","path":"汇报.pptx","output_path":"汇报.pdf"}
{"project":"agent:my-runner:office","path":"本周汇报.docx","offset":0}
{"project":"agent:my-runner:office","path":"本周汇报.docx","backup_id":"<此前返回的备份ID>","expected_sha256":"<恢复前目标的当前SHA>"}
```

上面三个请求依次用于 `document_convert`、`document_restore`、`document_restore`。备份列表每批 50 项，以 `next_offset` 续读。

Word 支持段落/表格文字、格式和修订接受/拒绝；Excel 支持单元格、格式、行列结构和重算；PPTX 支持文字、表格、备注、几何属性、图片与图表数据。PDF 支持文本对象替换、页选择/删除/重排/旋转/合并、表单和批注。详细字段见 `document_edit` schema；每种格式支持的操作不同，后端对不支持的组合报错，不应承诺任意版式都能无损编辑。

## Runner 配置与身份绑定

每个项目对应一个 provider，`cwd` 必须与该项目登记根目录规范化后完全一致。Desktop 为不同根生成稳定的 provider ID，保留用户自定义 provider。匹配为零或多个时明确失败，不选择第一个，也不隐式切换目录。

```toml
[[plugins.providers]]
id = "office-project-a"
name = "Office project A"
command = 'C:\tools\office-venv\Scripts\python.exe'
args = ['C:\tools\mycodex-tools\plugin.py']
cwd = 'E:\office'
document_worker = 'C:\tools\mycodex-tools\document_worker.py'
document_worker_args = ['--font-path', 'C:\Windows\Fonts\simhei.ttf', '--tesseract-path', 'C:\tools\tesseract\tesseract.exe', '--tessdata-path', 'C:\tools\tesseract\tessdata']
```

这些路径由操作者配置，模型不能覆盖。worker 从 stdin 读取请求，产物清单固定写到 `.webcodex-office/cache/<request_id>/result.json`。Runner 在 Job 接收时再次核对项目根、provider 实例和固定 argv；配置替换不允许旧请求重新指向新 provider。

继续读取已完成的历史 Job 不要求原 provider 进程仍存活，因为读取的是该 Job 的既定结果清单；仍要求当前调用者有该 Job 和项目权限、项目根未改变、工具名匹配。不会重新运行 worker。

权限复用现有 project read/write、plugin invoke、Job 执行及独立权限评估。只读展示也需要执行 worker 的权限。旧插件调用和普通 Job 不增加强制参数。

## 慢操作、冲突与恢复

执行开始时即由原生 process Job 托管，同步等待 5 秒，总预算 300 秒。超过同步等待窗口返回 `job_id`，以**同一工具**仅传 `project + job_id` 继续；不要重复原始修改指令。`list_jobs/job_status` 对文档 Job 增加操作名、相对路径、请求 ID，便于找回丢失的启动响应。

源文件的 SHA 用于防止覆盖用户在电脑上另行保存的修改；显示结果前会再检查当前源 SHA。游标和预览也绑定读取版本。源变化返回 `document_source_changed`，应重新读取，而不是把旧页图当成最新文件。

原地修改在验证前后内容后原子发布，并保存备份与收据；恢复同样要求目标当前 SHA。`.webcodex-office` 包含缓存、锁及备份，默认文本搜索排除此目录。用户文档参数不能直接访问其中的内部文件。不要把 Git 文本 checkpoint 当成 Office 二进制备份。

进程结束、连接中断、清单损坏或结果不确定时，检查原 Job、目标文件与备份状态后再决定后续动作；错误不意味着原操作一定没执行。

## 格式、资源与验证边界

- 支持现代 DOCX/XLSX/PPTX、旧 DOC/XLS/PPT、PDF，以及图像/OCR相关转换。普通 OOXML 读取及局部修改优先离线实现；旧格式、真实 Office 排版、转换和部分重算依赖已安装的 Microsoft Office 和 pywin32。不是鼠标键盘或窗口截图自动化。
- Microsoft Office 不可用时报告依赖缺失；没有承诺 WPS/LibreOffice 与所有 Office 专有功能等价。PowerPoint 已运行时部分原生操作明确返回 `office_busy`，避免连接到用户正在使用的实例。
- 单文件 64 MiB，Office ZIP 展开合计 256 MiB；一次操作最多 100 项。结构结果 128 KiB，worker 清单 256 KiB。默认每次最多 2 页，单张 MCP 图像不超过 1 MiB；大文档使用返回的游标续读。
- OCR 使用本地 Tesseract，语言为简体中文/英文；OCR 内容带识别来源和置信度，不能当成无误的原生文本。扫描 PDF 不能直接安全替换任意文字；可转换为可编辑文档。
- PDF 对签名、加密、复杂表单和不能安全定位的文字修改保持明确失败；页图与文字抽取的覆盖范围应随结果一起报告。
- 原件 ResourceLink 仍受现有导出大小、期限、鉴权和源版本复核约束；工具不会把完整二进制经过模型生成 Base64。

协议测试验证服务端原生图像、Job 续读和目录绑定；真实网页 ChatGPT 对某账号的显示及理解效果仍需实际宿主验收。不能仅凭服务端成功状态宣称网页已看到图像，也不能将原件链接宣称为原生聊天附件。当前构建/部署与验收结果以本轮测试报告为准，本文描述代码契约。
