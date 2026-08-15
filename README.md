# Paraegox

## 中文

Paraegox 是一个面向分布式具身智能的 Agent OS。它的目标是让 Agent 能够在设备、边缘节点与云端之间理解环境、持续推理，并通过受约束的工具和物理操作安全地影响现实世界。

这是一个全新的独立实现：不 fork PhanthyMotus，也不继承旧 ParaEGOX 的 Git 历史。旧 ParaEGOX、EAGOS 与 PhanthyMotus 仅作为设计、代码和故障经验的参考。

当前仓库实现了第一条有界运行路径：一个可寻址 Node 在本地启动 RuntimeHost 与 FabricService，另一个进程通过固定 Zenoh TCP 端点读取其 identity 与 readiness。当前只接受 loopback 端点，不需要 zenohd：

```bash
cargo run --locked --bin paraegox -- node run --node-id node-a
cargo run --locked --bin paraegox -- node probe --target node-a
```

这只是单 Node、双进程基线；尚未实现 Agent/TUI、第二个 Node、跨宿主、安全远程连接、部署或硬件能力。开发与编译的标准环境是指定 Ubuntu 服务器，具体流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## English

Paraegox is a distributed embodied-intelligence Agent OS. Its goal is to let agents understand their environment, reason continuously, and safely affect the physical world through constrained tools and operations across devices, edge nodes, and the cloud.

This is a new, independent implementation. It is not a fork of PhanthyMotus and does not inherit the Git history of the legacy ParaEGOX repository. Legacy ParaEGOX, EAGOS, and PhanthyMotus are reference material only.

The first bounded runtime path is implemented: one addressable Node starts RuntimeHost and FabricService locally, while a second process reads its identity and readiness through a fixed Zenoh TCP endpoint. This baseline accepts loopback endpoints only and does not require zenohd:

```bash
cargo run --locked --bin paraegox -- node run --node-id node-a
cargo run --locked --bin paraegox -- node probe --target node-a
```

This is a single-Node, two-process baseline only. Agent/TUI, a second Node, cross-host secure connectivity, deployment, and hardware are not implemented. The designated Ubuntu server remains the standard build environment; see [CONTRIBUTING.md](CONTRIBUTING.md).
