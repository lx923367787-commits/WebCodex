# 本项目的办公绘图能力

可用现有 WebCodex `run_process` 生成流程图、架构图及 Excel/CSV 数据图表，再用 `read_project_artifact(as_image=true)` 让 GPT 检查真实图片。原始数据只读，绘图输出另建目录。

本机入口：

- Python：`E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools\python\python.exe`
- 脚本：`E:\wbecodex\anzhuang\WebCodex Desktop\mycodex-tools\draw.py`
- 参数：`["-B", "<脚本绝对路径>", "render", "--spec", "-", "--output", "drawings/task-v1"]`
- 将 JSON 图定义作为 `stdin`，`project` 使用当前登记项目 ID，省略 `cwd`，`timeout_secs` 建议 120。输入和输出路径都相对此项目。输出目录必须是新目录。

图定义例子（流程图和架构图）：

```json
{"kind":"flowchart","title":"处理流程","direction":"LR","nodes":[{"id":"a","label":"提交"},{"id":"b","label":"处理"}],"edges":[{"from":"a","to":"b"}]}
```

架构图使用 `kind:"architecture"`，可加 `groups:[{"id":"g","label":"应用层"}]`，节点通过 `group:"g"` 归组。节点形状：`process/decision/start/end/database`。修改保持节点 ID 稳定。

数据图例子（按实际文件、工作表、范围和列名调整）：

```json
{"kind":"chart","chart_type":"bar","title":"月度收入","source":{"path":"数据.xlsx","sheet":"Sheet1","range":"A1:B7","header":true},"x":"月份","series":[{"column":"收入","label":"收入"}],"unit":"元"}
```

支持 `bar/line/pie/scatter`；数值不自动汇总或换算单位。CSV 或数字文本需显式 `numeric_strings:"decimal"`。公式仅读取已保存结果，缺少缓存时先用现有文档工具重算保存；不把空值、错误值当成 0。数据上限 2000 行、64 列、8 个系列；饼图最多 20 类。

完整流程：

1. 读取本指南、实际文件范围或上次的 `normalized_spec.json`；构造 JSON。
2. 运行后若返回 Job，观察同一 Job 到完成，不重复提交。退出码为 0 后取 stdout 的 `preview.path`。
3. 按 `review_material.continuation` 调用 `present_document`，传同一 `project`、受管原始 PNG 的 `path`、`view:"pages"`、`review:1`；该路径将真实源指纹、渲染依赖和原生图回执连入审核门禁。单独用 `read_project_artifact(as_image:true)` 查看缩放的 `preview.path` 仅作辅助，不能替代原始 PNG 的受管审核。检查实际图片，再按需要改定义生成 v2。
4. `manifest.json` 是生成完成标记，记录来源哈希、预览和产物；`review=1, review_status=pending` 表示仍需宿主 LLM 对实际图像作出版本绑定的质量判断。图定义可传整数 `review`，排版产物即使传 `0` 也会统一纠正为 `1` 并说明理由。检查实际图像后在受管 Host Goal 提交具体问题或通过依据；修正时生成新目录、新指纹和新一轮材料。
5. `review_material` 的预览只证明其绑定的 PNG；独立渲染的 PDF/SVG 需各自审核。`managed_artifacts` 显式列出文件与 SHA，但不把任意 shell 产物自动视为已纳管或已审核。满足当前版本审核后再用现有 `export_project_artifact` 交付。

每个 ZIP 保留图定义、SVG/PDF/PNG、结构源或选中数据快照、复现脚本。数据图的 `snapshot.spec.json` 可在原表变化后重画旧数据。SVG/DOT 通过 ZIP 交付。

还可调用 `draw.py doctor`、`draw.py templates`，或 `draw.py init <flowchart|architecture|bar|line|pie|scatter> --spec drawings/new.json` 创建新模板。详细说明在安装目录 `DRAWING_GUIDE.md`。
