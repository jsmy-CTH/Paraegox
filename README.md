# Paraegox

## 中文

Paraegox 是一个面向分布式具身智能的 Agent OS。它的目标是让 Agent 能够在设备、边缘节点与云端之间理解环境、持续推理，并通过受约束的工具和物理操作安全地影响现实世界。

这是一个全新的独立实现：不 fork PhanthyMotus，也不继承旧 ParaEGOX 的 Git 历史。旧 ParaEGOX、EAGOS 与 PhanthyMotus 仅作为设计、代码和故障经验的参考。

当前仓库实现了同机双 Node 基线：Node A 与 Node B 分别启动自己的 RuntimeHost 和长期 FabricService session，Node B 再通过自己的 session 读取 Node A 的 identity 与 readiness。当前只接受 loopback 端点，不需要 zenohd：

```bash
cargo run --locked --bin paraegox -- node run \
  --node-id node-a \
  --listen tcp/127.0.0.1:7447

cargo run --locked --bin paraegox -- node run \
  --node-id node-b \
  --listen tcp/127.0.0.1:7448 \
  --connect tcp/127.0.0.1:7447 \
  --probe-peer node-a
```

Node B 会先输出自己的状态，再输出它实际观察到的 Node A 状态，并继续运行到 Ctrl-C。`node probe` 仍作为外部诊断命令保留。这只是同机 loopback 双 Node 基线；尚未实现 Deck/Card、Agent/TUI、跨宿主安全连接、部署或硬件能力。开发与编译的标准环境是指定 Ubuntu 服务器，具体流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## English

Paraegox is a distributed embodied-intelligence Agent OS. Its goal is to let agents understand their environment, reason continuously, and safely affect the physical world through constrained tools and operations across devices, edge nodes, and the cloud.

This is a new, independent implementation. It is not a fork of PhanthyMotus and does not inherit the Git history of the legacy ParaEGOX repository. Legacy ParaEGOX, EAGOS, and PhanthyMotus are reference material only.

The same-host two-Node baseline is implemented: Node A and Node B each start their own RuntimeHost and long-lived FabricService session, and Node B uses its own session to read Node A's identity and readiness. This baseline accepts loopback endpoints only and does not require zenohd:

```bash
cargo run --locked --bin paraegox -- node run \
  --node-id node-a \
  --listen tcp/127.0.0.1:7447

cargo run --locked --bin paraegox -- node run \
  --node-id node-b \
  --listen tcp/127.0.0.1:7448 \
  --connect tcp/127.0.0.1:7447 \
  --probe-peer node-a
```

Node B first prints its own status, then the Node A status it actually observed, and remains running until Ctrl-C. `node probe` remains available as an external diagnostic command. This is a same-host loopback two-Node baseline only. Deck/Card, Agent/TUI, secure cross-host connectivity, deployment, and hardware are not implemented. The designated Ubuntu server remains the standard build environment; see [CONTRIBUTING.md](CONTRIBUTING.md).
