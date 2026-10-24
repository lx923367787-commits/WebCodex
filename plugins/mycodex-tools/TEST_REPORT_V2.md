# 本地办公工作流 v2 实施与验收

日期：2026-09-20（Asia/Shanghai）。

## 已交付

四个原生运行时入口：`present_document`、`document_edit`、`document_convert`、
`document_restore`。既有 11 个插件接口保留，Word/Excel 修改兼容入口复用新的
OOXML 定点修改实现。新增入口复用项目权限、原生 Job、MCP 图像及文件导出。

- Word：有序正文/表格、多 story、修订、保格式定点修改、真实 Office 页图。
- Excel：公式与缓存/重算结果、日期/格式/行列编辑、原生保存回读、错误值类型、
  未改单元格精度检查；不执行网络公式或刷新外部数据。
- PowerPoint：文字、表格、备注、图片、图表数据、位置及格式修改，真实页图。
- DOC/XLS/PPT：本机 Office 转换、读取和常用修改。
- PDF/图片：真实图像回传、简中/英文 OCR、局部文本对象修改、表单/批注/页操作，
  PDF 转可编辑 DOCX 采用文字/OCR 重排。
- 保存：源 SHA、工作副本、备份、原子写回、恢复、请求身份锁与结果回执。
- 项目：两个当前注册目录各自绑定 provider，避免同名文件串读写。
- 传输：单文件 64 MiB；批量附件部分失败保留逐项状态及成功路径，未知结果不可盲目重放。

## 自动化检查

| 检查 | 最终结果 |
| --- | --- |
| 候选独立 Python 全部测试（含真实 Office/OCR，无跳过） | 96/96 通过 |
| tool-contracts schema/权限/目录合约 | 97/97 通过 |
| tool-runtime-contracts 解析/兼容合约 | 73/73 通过 |
| HTTP 批量附件导入新增场景 | 2/2 通过 |
| MCP 直调/gateway、Job 续读、源变化拒绝 | 通过 |
| Runner provider 根/实例绑定与 Job 重连一致性 | 通过 |
| 既有导入/ModelSurface 回归 | 14 项、13 项通过 |
| Desktop provider 配置/恢复 | 9/9 通过 |
| Desktop 前端构建与嵌入资源探针 | 通过 |
| 使用最终候选的真实 HTTP MCP 隔离场景 | 7/7 通过 |

真实 Office 测试使用当前登录 Windows 身份，Server/Runner、项目、状态和凭据仍是
隔离测试材料。伪造 Windows 用户目录导致 Office 初始化报错的负例保留，未将其
混为产品通过；恢复真实身份目录后保留原断言通过。测试结束未遗留 Office 进程。

测试覆盖源文件变化、占用、备份损坏、原子提交失败、提交后回执失败、并发请求 ID、
备份恢复、两目录同名文档、中文字体、真实公式结果与错误值、外部公式拒绝、PPT/XML
保留、PDF 原文对象移除、中文表单外观、OCR 坐标和置信度等。

## 当前安装与实际连接验证

安装位置：`E:\wbecodex\anzhuang\WebCodex Desktop`。

运行时：0.4.1，源码 HEAD `15e4287cda7f`，`git_dirty=true`。保留未提交状态，未推送、
发布或改版本号。Server/Runner/CLI 使用 dogfood 构建；Desktop 同样使用 dogfood，
并启用 `tauri/custom-protocol` 嵌入前端资源。

实际 MCP 返回：1 个在线 Runner、2 个项目、2 个 ready 文档 provider、139 个运行时
工具。调用结束活动 Job 为 0。除新增插件 provider 外，Runner 配置与部署前一致；
通道密钥未修改。

通过用户当前连接对专用合成验收文件实际完成：

1. 读取 Word 正文、表格与页眉。
2. 原位修改“待更新”为“已更新”、表格状态为“验收通过”。
3. 回读并用 Word 16.0 渲染；实际页图显示文字和表格，保留原加粗及页眉。
4. 校验备份字节 SHA 与修改前完全相同。
5. 将 Word 转成新的 PDF。
6. 恢复原始字节，再恢复到已修改版本；两次 SHA 均匹配预期。
7. 收到 PDF 原生 MCP 图像和原件 ResourceLink；模型从图像识别出正文未描述的
   红色实心、黑色轮廓圆形。

验收文件保留在 `E:\wbecodex\sandbox\office-v2-live-20260920`，没有修改用户业务文档。
当前连接已验证；未使用 GUI 操作或代替用户在网页界面点击，网页特定 UI 展示仍与
当前连接/HTTP 协议验收区分记录。

## 部署过程与回退

部署过程中两次启动/替换检查未通过时均保留或恢复原程序和配置，随后修复并验证：

- PowerShell `File.Replace` 的空备份参数改用 `[NullString]::Value`；新增实际文件
  Apply/Restore、ACL、目标漂移及既有 bundle 复用检查。
- Desktop 生产构建启用 `tauri/custom-protocol`；增加不创建窗口、不读取凭据的
  `--check-embedded-assets` 探针，部署预检必须退出 0。

最终部署状态为 `applied_live_smoke_passed`。原版四个程序和 Runner 配置保留于：
`E:\wbecodex\.local\rollback\office-v2-20260920-185805`。
受控部署/回退脚本：`E:\wbecodex\.local\office-v2-build\deploy-office-v2.ps1`。
回退前应重新核对活动 Job 和文件状态，不覆盖独立修改。

## 明确边界与复核

- OCR 是识别结果，带置信度；风格字体测试出现过误识字，未称为始终准确。
- PDF 局部替换限可可靠定位的完整文字对象；跨对象、溢出和不能保证保留的情况会拒绝。
  PDF 转 Word 是可编辑文字重排，不承诺恢复复杂版式。
- 签名、加密、受保护或不能无交互处理的文档明确失败；不会点击提示框继续。
- PowerPoint 已有用户实例时可能返回 `office_busy`，不接管用户实例。
- 原件 ResourceLink 不等于原文件自动成为聊天附件；模型理解使用真实结构内容和页图。

本轮独立子代理复核与主代理回归已完成。外部辅助模型尝试未获得审稿正文：当前
Claude 低成本映射及免费模型限流，备用模型超时；未将其记为审稿通过。

自动审批审查拒绝清理首次失败的 staging，只返回 `blocked by policy`。
`E:\wbecodex\.local\office-v2-build\runtime-20260920` 已保留，未绕过拒绝。

机器可读记录位于 `E:\wbecodex\.local\reports`：
`office-v2-final-python-tests.log`、`office-v2-final-mcp-e2e.json`、
`office-v2-live-smoke.json`、`office-v2-final2-bundle-validation.json` 和
`office-v2-counter-review.txt`。
