# 本地办公绘图验收记录

> 发布说明：本文的实例标识和本机用户目录已脱敏；验收日期、结论与构建指纹保留。

验收日期：2026-09-21。范围：流程图、分组架构图、Excel/CSV 数据图表的本地生成、修改重画、真实图片回传和文件导出。

## 实现与安装

- 沿用现有 `run_process` 和 `read_project_artifact(as_image=true)`，未增加 MCP 工具或修改 Rust/PPT。
- `plugins/mycodex-tools/draw.py` 提供 `doctor/templates/init/render`。流程和架构图使用 Graphviz，四种数据图使用 Matplotlib。
- 已增量安装到 `E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools`。新增 `draw.py`、`drawing/`、`DRAWING_GUIDE.md`、`DRAWING_MANIFEST.json`，2493 个清单文件哈希全部验证通过。
- 绘图依赖使用独立 `drawing/vendor`；没有覆盖 Office 的 Python 包、修改通道密钥或重启服务。原 Office `MANIFEST.json` SHA256 保持 `7eb27513e84ae775037591b669b05a6a3f9a6100548cc9e7134627b6e97e8575`。
- 新增绘图清单 SHA256：`e69d15ef1eb1faa4a64e4cd142af472c31c7748c4bc38d49c051fda261684195`。
- 运行时实测：Matplotlib 3.11.2、NumPy 2.4.6、openpyxl 3.1.5、Pillow 12.3.0、pypdf 6.19.0，字体 Microsoft YaHei。Graphviz 16.1.0 官方便携 ZIP SHA256 为 `733e49626c492242eb8dca30ea627b6ead20710e207998c7933b4909d92d6abc`，构建时同时校验 ZIP 和已解压文件。
- 两个已登记工作目录均新建简短 `DRAWING_GUIDE.md`：`E:\wbecodex`、`C:\Users\<WindowsUser>\Desktop\web codex`。
- 增量构建目录：`E:\wbecodex\.local\drawing-build\addon-20260921-v1`。安装与回退记录：`E:\wbecodex\.local\rollback\drawing-addon-20260921`。旧 Office 文件未被替换。

## 自动化结果

| 检查 | 结果 | 覆盖范围 |
| --- | --- | --- |
| `test_drawing_cli.py` | 7/7 通过 | JSON/体积/路径边界，拒绝覆盖，真实图片压缩，归档内容，渲染与 ZIP 失败的完成状态 |
| `test_drawing_diagrams.py` | 7/7 通过，无跳过 | 节点边分组校验、DOT 转义、稳定 ID、超时、真实中文 PNG/SVG/PDF |
| `test_drawing_charts.py` | 11/11 通过 | 四种图与实际模板、中文渲染、公式缓存、缺值和错误、CSV 精度、源路径、数据和日期轴重放 |
| 安装后 `e2e_drawing.py` | 12/12 检查通过 | 六模板 init/render、三格式解析、预览上限、ZIP/哈希、源文件不变、流程图修改 v2、四图复现 v2、占用目标/越界/无公式缓存拒绝 |

E2E 使用安装后的嵌入式 Python 和 `draw.py`，保留现场与结果：`E:\wbecodex\drawing-acceptance-20260921\drawing-e2e-receipt.json`。没有把进程退出成功等同于版面合格；另外打开并检查了中文样图，修复了图表标题与图例重叠。

复跑使用新的测试目录：

```powershell
$drawingPython = 'E:\wbecodex\.local\venvs\mycodex-tools-v2\Scripts\python.exe'
$env:WEBCODEX_TEST_GRAPHVIZ_DOT = 'E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools\drawing\graphviz\bin\dot.exe'
& $drawingPython -m unittest discover -s plugins/mycodex-tools/tests -p 'test_drawing_*.py' -v
& $drawingPython plugins/mycodex-tools/tests/e2e_drawing.py --python 'E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools\python\python.exe' --renderer 'E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools\draw.py' --project '<新的已授权测试目录>'
```

## 实际 MCP 验收

通过当前真实 WebCodex 连接执行 `doctor` 和三类生成，再通过 gateway 调用 `read_project_artifact(as_image=true)`。每次实际返回一个原生 `image` 内容块，模型能看到图片中的中文和颜色。

| 产物 | 实际回传预览 | 路径（相对 `E:\wbecodex`） |
| --- | --- | --- |
| 流程图 | PNG，60,791 字节 | `drawings/mcp-flowchart-20260921-v1/preview.png` |
| 架构图 | PNG，125,027 字节 | `drawings/mcp-architecture-20260921-v1/preview.png` |
| Excel 数据柱状图 | PNG，57,082 字节 | `drawings/mcp-chart-20260921-v1/preview.png` |

视觉观察：流程图的“否”分支回到修改再核对，“是”分支进入保存交付；架构图显示三组模块；六个月数据图中收入为蓝色、成本为青绿色，中文和数值标签可读。

另外实际调用 `export_project_artifact`，为图表 ZIP（127,715 字节）和架构 PDF（38,900 字节）获得原生 ResourceLink。记录位于 `E:\wbecodex\drawings\drawing-mcp-receipt-20260921.json`。资源链接创建与本地文件完整性已验收，不把它表述为所有宿主都已完成附件下载。

本次是在当前 Codex 对话中验证真实 MCP；未另开网页 GPT 会话。因此本机和 MCP 验收已通过，独立网页会话验收尚未记录。

## 复核与适用边界

- 独立代码复核发现“ZIP 失败后可能已有成功清单”，已改为归档完成后最后发布清单，并用同一故障案例验证。源数据边界、缺值混合坏数据与样式开关问题也已修正。
- 按当前配置的低成本辅助模型仅尝试一次外部快审，返回 HTTP 429，没有审查正文；不计为外部审查通过。独立代理复核已完成，未发现剩余已证实的 P0/P1。
- 当前输出为独立 PNG/SVG/PDF 和可重画素材包，不向原 Excel 插入原生图表，不执行公式重算。需要最新公式结果时先使用已有文档工具重算保存。
- 图形关系和数据均来自指定规范与范围，不自动推断业务正确性、汇总或换算单位。复杂图可能存在交叉线，应拆图并检查实际预览。文字、日期及来源行号可追溯，但不承诺跨字体和渲染器版本的逐字节一致。

使用步骤参见 [绘图工作流](DRAWING_WORKFLOW.zh-CN.md)。
