# 本地轻量绘图工作流

在现有 WebCodex 中生成流程图、架构图和数据图表。GPT 使用 `run_process` 调用本机 Python 绘图入口，再用 `read_project_artifact(as_image=true)` 看到实际图片。本次不增加 MCP 工具，不修改 Rust，也不制作或修改 PPT。

| 类型 | 适用内容 | 模板 |
| --- | --- | --- |
| 流程图 | 操作步骤、判断分支、审批和反馈循环 | `flowchart` |
| 架构图 | 模块、分组及模块间关系 | `architecture` |
| 柱状图 | 分类或不同期间的数值对比 | `bar` |
| 折线图 | 时间、数值或既定分类顺序的趋势 | `line` |
| 饼图 | 非负数值的整体构成 | `pie` |
| 散点图 | 两个数值字段的分布和关系 | `scatter` |

流程图和架构图使用 Graphviz；数据图使用 Matplotlib。绘图规范是 JSON，可继续修改。每次输出到新目录，原 Excel、CSV 和旧图保留。

## 先确定工作目录

使用 `list_projects` 返回的真实项目 ID。当前本机 `E:\wbecodex` 对应 `agent:lixian-8e32bbbb5c704b42:wbecodex`；重装或换 Runner 后，应重新查询，不能沿用旧 ID。

模型端可通过已有通用入口查询：

```json
{"tool":"list_projects","arguments":{"query":"wbecodex","limit":5}}
```

这段参数交给 `call_runtime_tool`。后续 `run_process` 与图片读取必须使用同一个项目。省略 `cwd` 时，进程工作目录就是所选项目根；图形规范中的输入文件及输出目录均相对该根。

当前安装位置：

```text
E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools\python\python.exe
E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools\draw.py
```

首次使用可以运行 `draw.py doctor` 检查运行时，或运行 `draw.py templates` 列出六种模板。安装位置改变后使用实际路径；不要把脚本路径误当成工作项目。

## 最短调用：生成并看到一张图

将以下参数交给现有 `run_process`。`stdin` 是 JSON 文本，直接生成一张两节点流程图，不需要先创建规范文件。

```json
{
  "project": "agent:lixian-8e32bbbb5c704b42:wbecodex",
  "executable": "E:\\wbecodex\\anzhuang\\WebCodex Desktop\\mycodex-tools\\python\\python.exe",
  "args": [
    "-B",
    "E:\\wbecodex\\anzhuang\\WebCodex Desktop\\mycodex-tools\\draw.py",
    "render", "--spec", "-", "--output", "drawings/flowchart-v1"
  ],
  "stdin": "{\"kind\":\"flowchart\",\"title\":\"任务流程\",\"nodes\":[{\"id\":\"start\",\"label\":\"开始\",\"shape\":\"start\"},{\"id\":\"end\",\"label\":\"完成\",\"shape\":\"end\"}],\"edges\":[{\"from\":\"start\",\"to\":\"end\"}]}",
  "purpose": "operation",
  "timeout_secs": 120
}
```

`drawings/flowchart-v1` 必须不存在。也可以省略 `--output`，由程序生成带时间标识的新目录。不要为重复尝试而删除原有目录。

若返回 `job_id`，用 `observe_jobs` 继续观察同一 Job，直到进程完成并确认退出码为 0；不能重新提交 `render` 代替等候。然后直接使用本次 CLI JSON 结果中的 `preview.path`，它已经是项目相对路径；需要详细追溯时再读取清单。预览可能是 PNG 或 JPEG，必须使用实际文件名。

通过 `call_runtime_tool` 返回原生图片：

```json
{
  "tool": "read_project_artifact",
  "arguments": {
    "project": "agent:lixian-8e32bbbb5c704b42:wbecodex",
    "path": "<本次preview.path对应的项目相对路径>",
    "as_image": true
  }
}
```

这是现有 MCP 图片通道。`as_image=true` 不能同时带 `offset` 或 `length`，单张图上限为 1 MiB。必须收到实际 `image` 内容块并检查图形后，才能说“已看到并核对”；仅有路径、SVG 文本或进程成功不等于已看到图片。

## 从模板开始

模板在 `plugins/mycodex-tools/drawing/templates/`，安装后位于 `mycodex-tools/drawing/templates/`。通过同一个 Python 调用：

```text
draw.py init flowchart --spec drawings/flowchart-spec.json
draw.py init architecture --spec drawings/architecture-spec.json
draw.py init bar --spec drawings/bar-spec.json
draw.py render --spec drawings/bar-spec.json --output drawings/bar-v1
```

在 `run_process` 中，每个参数各占一个 `args` 元素；不要把整行命令当成一个参数。`init` 只创建新的规范文件，不覆盖已有文件。

四个数据图模板共用示例映射：`drawings/data.xlsx`、`Sheet1`、`A1:C7`，标题行为“月份、收入、成本”。模板中的文件名、工作表、范围和列名必须换成用户实际数据；模板不会自动创建或修改原 Excel，只读取磁盘上已保存的版本。柱状图和折线图按月份比较收入与成本，饼图展示各月收入构成，散点图以收入为 x、成本为 y。

数据图当前读取 `.xlsx` 和 `.csv`。使用 CSV 时，将 `source.path` 改成实际 CSV 路径，移除 `source.sheet`，并显式设置 `numeric_strings: "decimal"`；编码可选 `utf-8-sig`、`utf-8` 或 `gb18030`。不猜测千分位、百分号或货币符号的含义。其他 Excel 格式可以先明确转换到新的 `.xlsx` 副本。

## 修改与追溯

每个完成的输出目录保留：

| 文件 | 用途 |
| --- | --- |
| `spec.json` | 这次调用使用的规范 |
| `normalized_spec.json` | 已校验的规范；流程/架构图补齐默认值，数据图保留输入规范 |
| `snapshot.spec.json` | 数据图的快照规范，读取同包的 `data.csv`，保留实际 x 轴类型 |
| `source.dot` 或 `data.csv` | 图的结构来源，或实际用于绘图的数据 |
| PNG、SVG、PDF | 清晰位图、矢量图和可打印文件；具体名称以清单为准 |
| `preview.png` 或 `preview.jpg` | 适合 MCP 原生图片通道的预览 |
| `manifest.json` | 输出文件、来源和校验信息 |
| `reproduce.py` | 随图保存的复现入口 |
| `bundle.zip` | 完整素材包，便于下载和继续修改 |

继续修改时，先读上次规范及实际图像，修改所需节点、连线、列选择或标题，再输出到 `v2` 等新目录。数据图的来源信息在 `manifest.result.metadata`；数据源也可能变化，应查看新的 `source_sha256`、工作表、区域、实际行号和跳过行号，不能仅凭文件名认定两次使用了同一版本数据。

数据图有两种重画方式：

- 复现当时的数据：使用 `reproduce.py`，它优先读取 `snapshot.spec.json` 和保存的 `data.csv`，不重新读取原 Excel。CSV 保留实际参与绘图的数值及原工作表行号或 CSV 记录号；快照规范保留数值/日期/分类 x 轴及必要的文字转义规则。
- 使用当前原表：读取并修改 `spec.json` 或 `normalized_spec.json`，再调用 `render`。这是一次新的数据读取，检查返回的来源 SHA 和行号。

从包含素材包的已登记项目根运行复现脚本，并选择新输出目录，例如将 `drawings/chart-v1/reproduce.py --output drawings/chart-v2` 作为同一 Python 的参数执行。移动到其他目录时，应把 ZIP 中的快照规范、CSV 和复现脚本一起解压到项目内；脚本会重新定位同包 CSV。换电脑或安装位置后，使用 `--python`、`--renderer` 指定该机器的实际 Python 和 `draw.py` 路径。复现以保存的规范和数据重画，不保证不同字体或渲染版本下文件字节完全相同。

需要交付文件时，使用已有 `export_project_artifact` 导出本次 PNG、PDF 或 `bundle.zip`。当前通道不直接支持 SVG/DOT 的原生图片展示；矢量图和结构源文件可随 ZIP 交付。不要通过模型拼接整份 Base64，也不要假定 ResourceLink 一定变成聊天附件。

## 数据与版面约定

- 图结构上限为 80 个节点、160 条连线和 16 个分组；节点 ID 使用字母、数字、下划线或连字符，引用必须存在，分组不能为空。方向为 `TB` 或 `LR`，默认 `TB`。普通节点、判断、开始、结束及数据库形状分别使用 `process`、`decision`、`start`、`end`、`database`。节点的 `description` 用于 SVG 提示和规范记录，不会成为 PNG/PDF 正文。
- 图表最多读取 2000 行数据、64 列，使用 1 到 8 个系列。列可用标题名或区域内从 0 开始的列索引选择。输入只支持受控字段，不执行任意绘图代码、DOT 属性或 HTML 标签。
- `missing` 默认 `error`；显式 `skip` 只跳过缺值。布尔值、非数值、Excel 错误值及缺缓存的公式仍会报错，不能用 0 冒充数据。绘图不会重算或保存原 Excel。
- 跳过缺值行会返回 `rows_skipped` 警告；使用已保存的公式结果会返回 `saved_formula_values` 警告。后者表示读取缓存，不是执行了重算。
- 饼图必须恰好一个系列，最多 20 类，数值非负且总和大于 0。散点图的 x 必须为数值。折线图支持数值、日期或按原始顺序排列的分类；需要明确时可指定 `x_type` 为 `category`、`number` 或 `date`，默认 `auto`。
- 规范可以调整标题、轴标签、单位，以及受支持的画布大小、分辨率、字号和数值标签开关。`unit` 只显示单位文字，不换算数值；标成“万元”前应确认数据本身就是万元。默认样式适合清爽的日常汇报。内容过密时应减少元素或分拆图形，而不是把整张图缩得无法阅读。
- 当前绘图输出是静态图形。SVG 用于保留矢量质量；后续内容修改以规范及数据为依据，不承诺任意 SVG 对象都保留可编辑文本。

## 让后续 GPT 会话发现能力

项目 README 可以链接本指南。工作目录可放一个简短的 `DRAWING_GUIDE.md`，说明已安装路径、六种模板和“生成后用原生图片通道查看”的调用链。该指南只应在用户已授权的目录中以新文件创建；存在同名文件时先读取并保留，不自动覆盖。

指南是给模型读取的使用说明，不需要另开 GUI 平台。遇到不确定的现有工具参数，调用 `tool_manifest` 获取实际契约；不通过增加 MCP 工具数量来解决发现问题。
