# 当前使用的 transport

`pilot_transport.py` 仍由相邻 `chat-agents/chat_transport.py` 导入，是生产单次发送、归属校验和结果观察实现。目录名称沿用现有 import 关系。

默认使用相邻 `chat-handoff`；安装与依赖见 [handoff 说明](../chat-handoff/README.md)。

为保留完整开发源码，本目录也收录早期实验的 `probe_tools.py`、`run_pilot.py`、`recover_result.py` 和离线测试 `test_probe_tools.py`。这些脚本不是正式 provider 的启动入口，构建和安装不会自动运行它们。正式子助手由 `chat-agents` 提供。

实验脚本需要开发者为一次明确授权的实验自行准备本目录的 `pilot-request.json`；未附带原电脑的请求、回执、会话或证据。请求字段和校验规则见 `pilot_transport.validate`。`run_pilot.py` 会启动该实验，可能发送网页消息；`recover_result.py` 会打开已记录的对话并观察结果。仅在使用者明确要求此实验及其浏览器操作时运行。仅需继续正式产品开发时，无需执行它们。

离线协议测试将网络、浏览器与子进程均替换为 mock，可从本目录运行 `python -m unittest test_probe_tools`。请求、dispatch、worker-results 和 evidence 是本地状态，不应提交。
