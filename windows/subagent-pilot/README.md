# 当前使用的 transport

`pilot_transport.py` 仍由相邻 `chat-agents/chat_transport.py` 导入，是生产单次发送、归属校验和结果观察实现。目录名称沿用现有 import 关系。

这里只收录这个实现与离线测试，不复制早期原型请求、回执、证据或已停用的原型启动脚本。默认使用相邻 `chat-handoff`；安装与依赖见 [handoff 说明](../chat-handoff/README.md)。
