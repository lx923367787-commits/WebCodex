# 架构重构：当前安装更新、重启与验收

日期：2026-09-28。用户在源码重构完成后明确要求重启连接并验收。
本轮已更新并后台重启当前 WebCodex 安装，HTTP、实际 MCP 连接和 Runner 均恢复。

## 安装与构建身份

| 项目 | 结果 |
| --- | --- |
| 安装目标 | `E:\wbecodex\anzhuang\WebCodex Desktop\webcodex-runtime` |
| 更新文件 | `webcodex-server.exe`、`webcodex-runner.exe`、`webcodex.exe` |
| 构建方式 | `cargo build --locked --profile dogfood`，Server / Runner / CLI |
| 版本 | `0.4.1` |
| Git | `feat/mycodex-tools`，`15e4287cda7f`，`git_dirty=true` |
| 构建时间标记 | `1790587541`，2026-09-28 17:25:41 +08:00 |
| 本地 Server | `http://127.0.0.1:55326` |
| 新进程 | Desktop `17612`；Server `4700`；Runner `18056`；CLI `16920` |
| Runner | `lixian-8e32bbbb5c704b42`，WebSocket 在线，3 个项目 |
| 新 Runner instance | `e9e88acb-a191-4164-b27e-24eea50b0fdb` |

构建前后核对 916 个源码/构建输入文件，构建期间和安装后没有输入变化。
使用本轮工作区内容，保留 dirty 标记；未提交、推送或发布版本。
Runtime 的 `source_alignment` 对 dirty build 仍保守显示 `different`，没有掩盖这个
诊断。本轮另外通过安装文件 SHA-256、构建时间和实际 MCP 返回的 Server PID
确认连接确实落在这些已验证字节上。

| 文件 | 安装后 SHA-256 |
| --- | --- |
| Server | `e0d72b3367d5ad80a32a7de1609d8f36dfab9545429428711327d842bca12ee3` |
| Runner | `82fd067434c3e3cdf6de1f2e85403c14040079575bdde60868a564e9c5861cbf` |
| CLI | `5c1e215adbf5386426dab911b945d4a03870a8701f8b0eafdf9f0cf427a93d7a` |

## 重启过程与数据保留

停机前确认没有 active Job、未完成 Host Operation 或活跃 Agent Task Coding Run。
只终止核准的 Desktop `14880` 父进程；其拥有的 Server、Runner 等子进程均在
观察窗口内退出，没有额外强制终止子进程。正式子进程验证了退出和监听释放，
没有采集退出码；隔离验收中的 Server 正常退出码为 0。旧监听释放后，完成冷备份、
验证候选 SHA 并替换三个运行时文件；原 Desktop 主程序以 `--background`、隐藏窗口
方式重新启动。整个过程使用命令行和 API，没有 GUI 点击或键鼠自动化。

备份：`E:\wbecodex\backup\architecture-refactor-20260928-173715`。
该目录 ACL 限当前 Windows 用户和 SYSTEM；凭据副本保存在此受保护目录。

- `binaries/`：原 Server、Runner、CLI，附原文件哈希和替换 journal。
- `live/`、`cold/`：一致性 SQLite、Session ledger、Goal/private 文件、配置、凭据及项目注册。
- 活备份、冷备份和重启后的 SQLite `quick_check` 均为 `ok`。
- 三个项目 ID/root、配置、凭据、Desktop 主程序字节全部保留。
- **21 个 Workflow Session 的完整 ledger 内容相同**。
- **12 条 Context authority 记录完全相同**，其中 **2 条 RETIRED** 继续退休。
- Goal、Plan、Attempt、Review、Checkpoint、Operation、current 指针等九个业务表
  按完整行摘要逐项相同；127 条既有 Operation 保留。
- 原待确认 Goal 保持原状态。生产数据库没有创建测试 Goal，没有代替用户批准既有计划。

需要回滚二进制时，应先重新核准当前安装的进程身份并停止其运行时，再按备份清单
恢复三个文件。数据库备份保留作一致性恢复依据；本轮没有自动覆盖业务数据库。

## 验收结果

| 验收组 | 结果 | 覆盖内容 |
| --- | --- | --- |
| 候选 Host Goal/Handoff | **34/34 通过** | 真实 Server/Runner、计划物化、claim、checkpoint、DRAINING 拒写、RETIRED 拒写、同 receipt 重放、Server 重启、B bootstrap、新 Attempt/Fence、原 Goal 完成、文件哈希 |
| 候选 Goal/ACP/Review | **7/7 通过** | 独立执行/审阅 fixture、验收证据、修复回合、产物变化使 delivery freshness 失效 |
| 候选 Job 重启恢复 | **8/8 通过** | HTTP run_process → Job、只重启 Server、原 Runner 继续、同 Job ID 恢复、原进程完成、副作用文件始终只出现一次 |
| 正式安装 | **50/50 通过** | 构建身份、安装 SHA、Runner 在线、项目与历史保留、Goal 工具可发现、HTTP → Kernel → Runner 实际文件读取 |
| 实际 MCP 连接 | **通过** | runtime_status 返回新 Server PID `4700`；read_files 返回本地 State Ownership 文件的相同 SHA |
| 实际 Plugin Gateway | **通过** | 发现 3 个 ready provider；目标 provider 列出 11 个工具；describe 产生新 binding；docx_read 通过 Runner 返回夹具标记及相同文件 SHA |
| 最终状态复核 | **通过** | 无 active Job；Context authority 和完整 Session ledger 再次核对相同；SQLite 完整性正常 |

三个隔离验收均使用本轮候选二进制和临时身份/目录；Windows Job Objects 确认
测试进程树已清空，临时 profile 已删除。ACP 是确定性测试 provider，没有外部模型调用。
Handoff 是显式 context 的本地协议驱动，不宣称验证了真实网页自动创建下一段 Chat。

正式 Plugin 测试夹具保存在：
`E:\wbecodex\sandbox\architecture-refactor-20260928\plugin-read.docx`。
原文为 `architecture-refactor-native-plugin-ok`，SHA-256 为
`e565af7df1ab62fd8def42dc5b4b77af58107e1c7b1eaf90adaf52d599efd065`。
读取使用 Python DOCX 解析，不启动 Office GUI。

验收调整记录：MCP 批读首次给出的 `max_result_bytes=4096` 低于连接器的 8192 下限，
在参数校验阶段被拒绝；改为 8192 后通过。Plugin 首次读取报告目录中的夹具时，
`.local` 受保护路径被正确拒绝；重新在允许的 sandbox 创建无敏感内容的测试夹具后
读取通过。未放宽路径策略、权限或连接器约束。

## 证据与范围

本地证据目录：`E:\wbecodex\.local\reports\architecture-refactor-20260928`。

- `build-request.json`、`build.log`、`source-manifest.json`、`candidate-binaries.json`
- `handoff/`、`goal/`、`job-restart-acceptance.json`
- `stop-result.json`、`restart.json`、`installed-processes.json`、`installed-binaries.json`
- `installed-acceptance.json`、`state-preservation.json`、`native-mcp-acceptance.json`
- `deploy_runtime.py`、`restart-installed.ps1`：本次目标的受限部署及验收脚本。

本次重启验收通过，不等于修复了源码阶段的全量测试遗留问题。原 Server 29 项基线
失败及 Core 1 项 `serde_json/preserve_order` 组合相关失败仍按
[源码重构报告](ARCHITECTURE_REFACTOR_REPORT.md) 记录。Workspace 的 10 项 PATH
问题已在加入 Git `sh` 后补测通过。本轮没有扩大范围修改这些业务/测试契约。
