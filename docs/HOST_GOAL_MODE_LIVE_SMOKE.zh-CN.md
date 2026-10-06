# 网页目标模式：历史安装部署与在线烟测

> 历史记录：以下测试、计数和构建指纹仅对应记录当日，不是当前操作手册或当前安装状态。2026-09-29 源码新 epoch 默认值为 **600,000/800,000**，已有 epoch 保留原策略。当前流程见[Host Goal 操作说明](HOST_GOAL_MODE.zh-CN.md)。

时间：2026-09-22 20:40 起（北京时间）。用户授权更新当前安装、重启并测试。

## 部署结果

- 安装目标：`E:\wbecodex\anzhuang\WebCodex Desktop\webcodex-runtime\webcodex-server.exe`。
- 来源：`E:\wbecodex\source\target\dogfood\webcodex-server.exe`。
- 源码基线：`15e4287cda7f5e673dc449c34ed2838f9bcad4ca`，构建为 `0.4.1`、`git_dirty=true`；未隐藏工作树改动。
- 安装后 SHA-256：`B61926B3B44F4F27FE43450D237C4F46FD875824A9A7475F7CC593B1E1807EBD`，与已验证构建一致。
- 只替换 Server 二进制。通过终止已核验 Desktop 父进程、等待其拥有的 Server / Runner / Tunnel 正常退出，再以 `--background` 后台启动 Desktop。新 Server PID 为 `9888`，Runner PID 为 `7664`；两者在线。
- 实际远端工具数从 144 增至 151，新增 Host Goal 接口可发现并调用。
- 重启期间首个连接请求返回一次 HTTP 504；连接恢复后的只读重试成功，后续工具调用正常。

回退材料：

- 旧 Server：`E:\wbecodex\anzhuang\WebCodex Desktop\webcodex-runtime\backups\host-goal-20260922-204032\webcodex-server.exe`。
- 部署清单位于同目录的 `deployment.json`。
- 停机数据库及状态备份：`C:\Users\Administrator\AppData\Local\dev.webcodex.desktop\runtime\local\data\backups\before-host-goal-20260922-204032`。副本保留原文件 ACL。

未修改 Runner / CLI 二进制，没有版本发布、Git 提交或推送。

## 在线验证

测试目标：`hgoal_8afd96f8348f4f3e9f30c53a5f4476bc`，属于已授权的 `wbecodex` 项目。新模式部署前，该身份没有当前 Host Goal。

| 检查 | 实际结果 |
| --- | --- |
| 工具连接、Server / Runner 与项目清单 | 成功恢复；3 个原有项目在线，烟测前无活跃 Job |
| `goal_mode_start` | 创建 `planning` 目标，并识别真实宿主对话绑定 |
| `goal_plan_save` | 本地生成 `plan.json` 和 `plan.md`，版本 1，进入 `awaiting_confirmation` |
| 本地物化校验 | 两文件存在，接口报告 `plan_artifacts_valid=true` |
| 未确认认领节点 | 先实际读取完整 Node 包；再认领被拒绝，明确要求用户确认计划 |
| 未确认业务写入 | 返回 `goal_context_required`；随后核对测试文件确实没有生成 |
| `goal_checkpoint(handoff)` | 完成摘要、事实、问题、产物引用及下一步成功保存，revision 变为 4 |
| 独立客户端续接 | 新 loopback MCP 客户端不提供目标 ID，仍可读到本地当前目标、同一计划版本和等待确认阶段 |
| 完整状态恢复 | `goal_read(view=detail, section=state)` 的事实标记、安装指纹、问题、下一步、产物引用均与保存内容一致 |
| 上下文两级提示 | 本次测试阈值设为 4000 / 6000；实际分别在 4306 / 6019 估算 tokens 时触发 warning / handoff_recommended，之后为短提示 |
| 新客户端独立计量 | 新显式 epoch 首次恢复仅累计 642 tokens，保留当时构建的默认策略；没有继承原对话计数。该历史数值不作为当前默认 |
| 测试阈值收尾 | 按当时版本复用原创建身份恢复该次测试的阈值；这是历史实现记录，当前实现不能用重复启动改写已有 epoch。没有新建目标、清零计数或改变已保存计划及待确认阶段 |

独立客户端使用当前安装已配置的 MCP 用户凭据访问同一 Server 的 loopback `/mcp`；凭据仅用于本机请求，没有写入测试脚本、报告或工具输出。该检查证明不同客户端可从本地存储恢复任务，不冒称自动打开了新的 ChatGPT 网页。

计划目录：`C:\Users\Administrator\AppData\Local\dev.webcodex.desktop\runtime\local\data\goals\hgoal_8afd96f8348f4f3e9f30c53a5f4476bc\plans\v1`。

- 确认所绑定的计划哈希：`13b209ba8312d25b7b8c0f70c0b48ab2f26944cd3c7f36e3e01f0e062fd9272e`。
- `plan.json` 文件 SHA-256：`79EC5DB48B1AB8018A1C4E70CEAD553F7BD7874325A9DEA3F6A9DD53A2F0642B`。
- `plan.md` 文件 SHA-256：`1E6D7608DE76E6CAE5ECB647C5FAB28302DBAEB8546EBB9352E2110DF2785292`。

计划身份哈希与排版后的物化文件哈希分别校验，不能混用。

## 确认后的执行

本地测试计划只允许读取已有源码验收报告并由运行时核对两段固定文字，不修改业务文件、不启动后台 Provider。已经向用户展示实际计划的路径、版本和哈希，等待用户阅读后明确通知开始；尚未代替用户调用成功确认。

在收到这次具体确认前，不能把先前“重启并测试”写成“已阅读并确认本计划”。因此本记录只证明上表已经实际完成的在线检查，不把源码隔离测试冒充本次线上确认与完成证据。
