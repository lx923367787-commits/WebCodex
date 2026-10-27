# MyCodex 实施与验收报告

> 发布说明：本文的实例标识和本机用户目录已脱敏；验收日期、结论与构建指纹保留。

验收日期：2026-09-20（Asia/Shanghai）。

## 实现范围

- 分支：`feat/mycodex-tools`；基线：`f080c8f3ea70e37bd9f17fdd0e1b4c3a3aa330f8`。
- WebCodex Server/Runner：现有 0.4.1 二进制，未改 Rust 核心或重编译核心。
- 新增 11 个 Native Plugin 工具：DOCX 读/创建/另存修改、XLSX 读/创建/另存修改、
  PPTX 读/创建、PDF 读/搜索/创建。
- 文件、PowerShell、Python、Job、图片回传继续使用现有工具。
- 运行环境：Windows x64、Python 3.11；独立 venv 使用锁定依赖。
- 未提交、推送、发布或启用到现有 Runner；未修改现有服务、凭据或运行配置。

## 已执行检查

| 检查 | 结果 |
| --- | --- |
| `install.ps1` 实际运行 | 通过；按锁文件安装/复用依赖 |
| `python -m pip check` | 通过，无依赖冲突 |
| `python -m unittest discover -s tests -p 'test_*.py' -v` | 39/39 通过，12.401 秒 |
| `python tests/e2e_mcp.py` | 10/10 真实 HTTP MCP 场景通过 |
| 中文 PDF 两页渲染检查 | 已用 pdftoppm 渲染并查看；文字、表格无缺字、重叠或裁切 |

聚焦测试构成：文件/边界 7 项，Office 13 项，PDF 14 项，协议 5 项。
损坏 PDF 负面用例的 `invalid pdf header` / `EOF marker not found` 是预期库诊断，
该用例通过，没有把无效文件当作成功。

运行命令（在插件目录中）：

```powershell
$py = 'E:\wbecodex\.local\venvs\mycodex-tools\Scripts\python.exe'
.\install.ps1
& $py -m unittest discover -s tests -p 'test_*.py' -v
& $py tests/e2e_mcp.py
```

## 真实链路证据

最终 E2E 开始时间：`2026-09-20T00:16:06Z`。链路为真实 HTTP MCP → Server →
WebSocket Runner → Native Plugin/原生工具，使用一次性项目、配置、进程与合成凭据。
OAuth scope 检查使用临时数据库中的合成 token 行；没有把它表述为真实用户 OAuth 登录流程测试。

| 场景 | 核验事实 |
| --- | --- |
| MCP 发现与 check | 11 个插件工具均可发现、schema 被真实 Runner 接纳，未加入外层工具列表 |
| 文件与 Python | `hello.txt` 精确等于 `hello webcodex`；UTF-8 中文回读；真实 `hello.py` 输出一致 |
| PowerShell | 原生 `run_script(language="powershell")` 输出 `hello powershell` |
| Job 续接 | 同一 `hello.py --hold` 的同一个 Job 到达 completed，退出码为 0 |
| 图片 | Pillow 生成 200×200 PNG；收到真实 MCP image；metadata 尺寸一致；1,923,529 字节 PNG 的图像回传被拒绝 |
| Office | Word/Excel 创建、读取、另存修改成功，原文件 SHA 不变；PPTX 两页文字回读成功 |
| PDF | 两页中文 PDF 回读与搜索成功，并检查实际嵌入 FontFile2 |
| 权限 | 3 项允许、7 项拒绝；inspect/invoke/manage 权限独立，403 被作为拒绝核验 |
| 热加载 | 旧 binding 返回 `plugin_replaced / not_started`；新 binding 正常工作 |
| 忙碌与超时 | 返回 `plugin_provider_busy` 和 `plugin_timeout / outcome_unknown`；受控 provider 实际只执行一次 |

原始 JSON 报告位于本机 `E:\wbecodex\.local\reports\mycodex-tools-e2e.json`，
包含断言结果和耗时，不含凭据。原生同步执行成功响应可能省略退出码，因此同步用例核验
成功状态与精确 stdout，明确退出码通过同文件的 Job 终态单独验证。

## 复核中修复的问题

- Windows 临时文件 fsync 改用可写句柄，避免正确生成后无法发布。
- Excel 校验所有实际存储的基础单元格，保留空文本；未编辑数值发生精度损失时拒绝发布。
  稀疏末尾单元格不展开为巨大矩形。
- 1600 个 Sheet 可分页读取；Excel 写入只返回定长摘要，避免文件已发布却报返回超限。
- 单页 400 个分组对象可按 shape 分页，能够继续读取。
- 中文 PDF 检查字体字形覆盖及逐页全文回读；缺字、内容不符或布局溢出不发布。
- PPTX 使用保守行宽/高度限制与明确行距，过长中文标题和过多条目被拒绝。
- E2E 自身修正临时身份绑定、原生返回格式、HTTP 403 判断和 SQLite 连接关闭；
  未放宽产品权限或功能断言。

独立代码复核已完成，上述 Excel/分页/写入摘要问题均经复现、修复和回归验证。
Claude 当前配置的辅助模型返回 429/1309（套餐已到期），未提供有效审查；
随后使用机器级 `Free_API_KEY` 调用免费模型，返回 429/1302 限流，最终按既有偏好
改用备用模型完成公共边界复核。备用意见逐项核验后没有新增成立的缺陷：

- 大小写保留名与 `<`/`>` 已被拒绝；实际调用 `Con.docx`、`aUx.docx`、
  `file<1>.docx`、`file>1.docx` 均返回 `path_rejected`。
- 同目录临时文件不会跨文件系统发布；硬链不支持时明确失败符合“不覆盖”合同。
  未采纳回退到 `os.replace()` 的建议，因为这会覆盖已有目标。
- 内存循环对象在深度上限处返回 `result_too_large`，未无限递归；网络 JSON 本身也不能编码引用环。

辅助模型原始意见保存在本机 `E:\wbecodex\.local\reports\mycodex-free-review.txt`。
没有改动 Claude 模型配置，也没有把密钥写入项目或报告。

## 修改文件与部署边界

新插件包含 `plugin.py`、`mycodex_tools/{common,office,pdf}.py`、安装脚本、
依赖锁、Runner 样例、英文/中文 README、聚焦测试和 E2E harness。
现有 `docs/PLUGINS.md` 与中文版仅增加插件索引。

生产启用按 README 中的明确步骤进行。本次只安装到独立 venv 并验证临时环境，
未重启或替换已有服务；核心未修改，未运行全 Rust workspace 测试。

最新成功 E2E 的自有进程树已停止、临时目录已清理。首轮失败测试留下的
`C:\Users\<WindowsUser>\AppData\Local\Temp\mycodex-mcp-e2e-lwevwfsq`
包含合成测试配置/数据；其测试进程已停止。自动审批审查拒绝对该目录的递归清理，
仅返回 `blocked by policy`，因此未重试或绕过拒绝，目录暂时保留。

Office 复杂格式、公式重算、OCR 和完整视觉保真不属于本轮承诺；详见 README 的能力边界。
