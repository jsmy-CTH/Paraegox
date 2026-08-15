# Paraegox

## 中文

Paraegox 是一个面向分布式具身智能的 Agent OS。它的目标是让 Agent 能够在设备、边缘节点与云端之间理解环境、持续推理，并通过受约束的工具和物理操作安全地影响现实世界。

这是一个全新的独立实现：不 fork PhanthyMotus，也不继承旧 ParaEGOX 的 Git 历史。旧 ParaEGOX、EAGOS 与 PhanthyMotus 仅作为设计、代码和故障经验的参考。

当前仓库已经有两条相连的最小路径：同机双 Node 可以通过各自长期持有的 Fabric session 互相寻址；一个 Node 也可以运行内置 Agent Deck，由独立终端客户端完成有服务端历史的多轮对话。当前只接受 loopback 端点，不需要 zenohd。

先在一个终端启动带 Agent Deck 的 Node：

```bash
cargo run --locked --bin paraegox -- node run \
  --node-id node-a \
  --listen tcp/127.0.0.1:7447 \
  --deck builtin-agent
```

再在另一个终端连接：

```bash
cargo run --locked --bin paraegox -- tui \
  --target node-a \
  --connect tcp/127.0.0.1:7447
```

Node 状态会展示真实的 DeckRun、DeckLock digest 和 Agent CardInstance。TUI 只保存 SessionId 与输入输出；有序历史由 Node 内的 AgentService 保存，因此第二轮确定性回答会引用第一轮用户输入。当前回答器只是用于验证路径的 deterministic responder，不是真实模型。`node probe` 和双 Node peer probe 仍保留。

M4 正在收敛唯一的真实模型候选。使用 DeepSeek V4 Flash 时，在同一个 Node 命令上增加 provider 选择：

```bash
cargo run --locked --bin paraegox -- node run \
  --node-id node-a \
  --listen tcp/127.0.0.1:7447 \
  --deck builtin-agent \
  --provider deepseek-v4-flash
```

启动该进程前，进程环境必须已经提供 `DEEPSEEK_API_KEY`；密钥不能进入命令行、仓库、Deck、Fabric 或 TUI。此模式会把对话内容发送给外部 DeepSeek API。真实 credentialed smoke 尚待在指定服务器执行，所以目前不能把外部 Provider 路径称为已完成。省略 `--provider` 时仍使用本地 deterministic responder。

尚未实现 Card Link、DeviceService、跨宿主认证加密、部署或硬件能力。开发与编译的标准环境是指定 Ubuntu 服务器，具体流程见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## English

Paraegox is a distributed embodied-intelligence Agent OS. Its goal is to let agents understand their environment, reason continuously, and safely affect the physical world through constrained tools and operations across devices, edge nodes, and the cloud.

This is a new, independent implementation. It is not a fork of PhanthyMotus and does not inherit the Git history of the legacy ParaEGOX repository. Legacy ParaEGOX, EAGOS, and PhanthyMotus are reference material only.

Two connected minimal paths are implemented. Two same-host Nodes can address each other through their independently owned, long-lived Fabric sessions. A Node can also run the built-in Agent Deck while a separate terminal client holds a multi-turn conversation whose history remains on the Node. The current baseline accepts loopback endpoints only and does not require zenohd.

Start a Node with the Agent Deck in one terminal:

```bash
cargo run --locked --bin paraegox -- node run \
  --node-id node-a \
  --listen tcp/127.0.0.1:7447 \
  --deck builtin-agent
```

Connect from another terminal:

```bash
cargo run --locked --bin paraegox -- tui \
  --target node-a \
  --connect tcp/127.0.0.1:7447
```

Node status exposes the real DeckRun, DeckLock digest, and Agent CardInstance. The TUI owns only its SessionId and presentation; ordered history stays in AgentService on the Node, so the deterministic second reply references the first user input. This responder validates the path and is not a real model. `node probe` and the two-Node peer probe remain available.

M4 is narrowing real-model access to one candidate. To select DeepSeek V4 Flash, add the provider option to the same Node command:

```bash
cargo run --locked --bin paraegox -- node run \
  --node-id node-a \
  --listen tcp/127.0.0.1:7447 \
  --deck builtin-agent \
  --provider deepseek-v4-flash
```

The process environment must already provide `DEEPSEEK_API_KEY` before startup; the credential must not enter the command line, repository, Deck, Fabric, or TUI. This mode sends conversation content to the external DeepSeek API. A real credentialed smoke test is still pending on the designated server, so the external Provider path is not yet claimed complete. Omitting `--provider` keeps the local deterministic responder.

Card Links, DeviceService, authenticated cross-host Fabric, deployment, and hardware are not implemented yet. The designated Ubuntu server remains the standard build environment; see [CONTRIBUTING.md](CONTRIBUTING.md).
