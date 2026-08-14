# 当前计划：M1 本地 Agent 对话

状态：Proposed
日期：2026-08-14

## 授权边界

本文是一份待确认的交付计划，不自行授权 M1 实现。只有用户明确要求开始 M1 后，状态才能变为 Active。

## Outcome

在指定 Ubuntu 服务器的 clean checkout 中，通过一个明确命令启动最小 TUI，并与拥有真实 Session history 的 Agent 完成两轮对话。

M1 的价值是建立分布式具身 Agent OS 的用户入口和 AgentSession owner。它不是把 Paraegox 降级为聊天产品；M2 会在同一入口加入模拟 Observation、受约束 Tool 和 Device Operation。

## 当前基线

- 仓库当前只有一个最小 Rust CLI 和工程 smoke tests。
- AgentSession、Model Provider、TUI、stream/final 和真实模型调用尚未实现。
- main 是唯一集成权威；开发、编译和验证在指定 Ubuntu compute container 执行。

## In scope

- 一个无需 daemon、Inspection 或数据库即可启动的最小终端对话入口。
- TUI 每轮只提交 Session identifier 与本轮 user text；TUI transcript 只是显示投影。
- AgentSession owner 负责追加有序 history，并为 Model Provider 构造 messages。
- 一个窄 Model Provider seam。
- deterministic provider，用于无凭据的稳定两轮测试。
- 一个 opt-in 的真实模型两轮 smoke，凭据只来自服务器环境。
- 每轮唯一 Authoritative Final；只有不引入额外框架时才加入增量 token。
- 启动失败、模型失败和请求超时的有界行为，以及 Ctrl-C 干净退出。

优先在现有 crate 内完成。只有用户明确批准且当前语言、进程、生命周期或安全边界确实需要时才增加 crate。

## Non-goals

- Tool、Device、Observation 和物理操作；这些属于 M2。
- 跨进程 Bus、Zenoh、远程 Node 或能力发现；这些属于 M3。
- 长期记忆、外部数据库、模型路由、预算、反思和多 Agent。
- Deployment、Artifact、Inspection、Ops、Evidence、Web Console 或打包发布。
- 语音、多模态、硬件驱动、ROS 或设备厂商 SDK。
- 为未来阶段预建协议、trait、目录、fixture 或兼容层。

## 建议交付切片

1. 在现有 CLI 内建立最小终端 UI、in-memory AgentSession、deterministic Provider 与 final-only 两轮场景。
2. 在相同 Provider seam 上加入一个真实模型 adapter 与服务器 opt-in smoke。
3. 只有 Provider 已原生提供增量结果且不需要通用 streaming/cancellation 框架时，才加入 token streaming。

每个切片必须保持同一产品路径可运行，不能先合入只有 interface、mock 或无 consumer 的基础设施。

## 用户可观察验收

- 用户运行一个公开命令即可进入 TUI。
- 第一轮要求 Agent 记住一个确定值，第二轮询问该值时得到正确答案。
- TUI 每轮只提交 Session identifier 和本轮 user text；它不能把 transcript 作为模型上下文权威。
- instrumented deterministic Provider 能观察到由 AgentSession owner 形成的完整有序 messages，而不是由 TUI 或 fixture 注入历史。
- deterministic 场景在无网络、无模型凭据时稳定证明 history、顺序、唯一 Final 和错误行为。
- 有凭据时，真实 Provider smoke 证明两轮请求传递、非空 Final、错误凭据失败和资源清理；不以模型的精确措辞证明 Session 正确性。
- 缺少或错误凭据时返回明确错误，不阻塞 TUI，也不回退为伪成功。
- 每轮只有一个 Final；若实现 token，则 token 只用于显示。模型或客户端退出后没有遗留 owner process。

GitHub CI 只运行 deterministic 场景。真实模型 smoke 在指定服务器显式运行，不把凭据带入 CI。

## 开始前需要决定

- 最小 TUI 的实现方式。
- 首个真实 Model Provider；选择条件是当前可用、凭据可由环境注入，并且供应商消息格式不会成为内部核心合同。
- M1 是否需要第一条 Rust 与其他语言的真实进程边界；没有当前 consumer 时保持单语言、单进程。

## Stop condition

上述两轮 deterministic 与真实模型场景在同一 main revision 上通过后，M1 完成并停止。不得自动开始 M2、拆分 crate、接入 Fabric、增加设备或扩展控制面；后续需要用户重新授权。
