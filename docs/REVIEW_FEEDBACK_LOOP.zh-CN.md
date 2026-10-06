# 任务审核与反馈闭环

`review` 是交付前的审核要求，只接受 JSON 整数 `0`、`1`。它不代表审核通过。Host Goal 仍使用现有计划确认、节点、attempt、fence、checks、Job 和权限边界；本机制不配置额外模型 API，也不会自动唤醒新聊天。

## 声明与兼容

在计划的每个节点声明 `review`、`delivery_kind` 和 `managed_artifacts`。`delivery_kind` 可取 `lightweight`、`code`、`data`、`layout`。例如：

```json
{
  "review": 0,
  "delivery_kind": "lightweight",
  "managed_artifacts": [{"path": "settings.txt", "kind": "lightweight"}]
}
```

精确文本替换等轻量操作保留必要机器校验，直接按原路径验收。代码、数据解释、未知复杂度，以及原有 Review criterion 均要求 `review=1`。排版类型、受管 Office/图像文件、实际文档修改操作强制为 `1`，传 `0` 会明确纠正。普通读取默认 `0`，读取待审核图片应显式传 `review=1`。

新保存的计划写出有效值、`review_reason` 和默认 `review_max_rounds=3`（可在首次准备前配置 1–5）。旧计划的缺失字段保持缺失，读取时计算有效要求，历史未记录显示 `not_recorded`；不会重写旧计划或确认哈希。实际文档操作造成的审核升级保存在审核记录，不改不可变计划。

通用命令产生的任意文件不会自动成为受管交付。调用者须在 `managed_artifacts` 中列出实际源文件，尤其是 PPTX、PDF、工作簿和绘图。把独立生成的 PDF 登记为 PPTX 的预览不成立。最终节点会纳入前序节点明确登记的排版交付，检查整份当前交付。

## 宿主模型调用顺序

1. 执行或修正后调用 `goal_check_node`，先不传 `evidence`。真实文件/命令校验成功后返回 `review_packet`，节点仍处于执行阶段；`review.status=pending` 表示尚不可交付。
2. 阅读回包中的实际文本、代码 diff、机器结果和原始 manifest。代码片段有界，需更多上下文时沿返回的 `read_file` / diff 入口继续读取。
3. 排版源使用 `present_document` 的 `view=pages, review=1`，跟随全部游标；位图绘图用 `read_project_artifact(as_image=true)`。每次最多两张文档图片，每张不超过 1 MiB。只有实际构造原生 image 内容后才记录本上下文的送达范围；路径、Base64 文本、ready、OCR 和部分页不够。裁切放大只作补充，不能替代全页。
4. 模型产生新的具体结论，再次调用 `goal_check_node` 提交 `model_self_check`。有 Review criterion 时使用各 criterion id；否则使用 `__delivery_review`。提交字段包括原 `review_packet_id`、完整 `artifacts`、`reviewed_scopes`、`source_refs`、具体 `evidence` 和 `findings`。失败 findings 必须包含 `location`、`impact`、`required_change`；通过需说明具体检查依据，不能保留未解决 findings。
5. 失败后按回包 `recovery` 主动诊断并修正实际源文件，修复调用及复验携带 `goal_context.repair` 的当前身份，每轮首次调用保存具体 `diagnosis`。普通业务保持暂停，原验收通过才放行。以新的 idempotency key 准备新材料并重新回传、判断；相同 key 仅用于不确定结果的续取。分页不消耗模型审核轮数。每次明确模型结论计一轮，达到审核或修复预算仍失败则保留问题，停止自动实施并说明所需人工决定。

审核提交还会通过只读文档 `review_probe` 重新计算实际源、渲染程序/库、选项及字体依赖指纹。慢探测使用现有 Job，`goal_review_probe_pending` 时重试原提交 key，恢复同一个 Job。新的提交 key 进行新的探测；不以旧终态 manifest 证明当前环境。渲染不支持、依赖探测不可用、文件损坏或图片缺页都保持未通过。

## 持久化与证据范围

`wc_host_goal_reviews` 保存节点要求、受管源、当前材料、被替换但未判断的材料，以及不可变的模型轮次快照。每份快照包含 binding、context epoch、源 SHA256、预览 SHA256、渲染依赖 SHA256、范围、机器结果和模型问题。存储层完成入口独立核对该通过记录；不能在提交时给旧意见重新贴当前文件哈希。

handoff 和重启保留审核要求、轮次和问题。新 attempt/上下文需要准备新材料并重新收到待审图片，历史送达不能冒充当前上下文送达。旧 epoch、错 hash、重复但内容不同的判断、降级审核和删除已有审核节点均不能放行。

机器检查来源为 `automated`；图片回传来源为 `runtime_mcp_image`；模型结论为 `model_self_check`。传输回执只证明工具响应包含图片，不证明模型内部理解，也不构成用户验收。原始审核轮次只读保留，报告可读取 `goal_read(view=detail, section=reviews)` 分页历史，不能重建或伪造通过记录。

## 明确未覆盖的版面

Office 渲染需要支持实际源格式的后台组件。缺少原生渲染时保留未覆盖状态。Excel 除打印页外还列出工作表使用区域和图表范围；没有实际覆盖映射的区域保持 missing，因此不会用一张截图宣称整工作簿通过。图像审核不能代替公式计算和关键数字的机器校验。

独立的真实 PDF 两轮视觉证据与模拟 reviewer 协议测试属于不同证据：前者验证可见缺陷确实修正，后者验证门禁/恢复逻辑。工程自检通过后仍由用户决定最终验收。
