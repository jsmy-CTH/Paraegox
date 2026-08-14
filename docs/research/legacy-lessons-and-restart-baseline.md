# 旧系统经验与重建基线

状态：Resolved
日期：2026-08-14
性质：Historical research; non-normative

本文总结旧 ParaEGOX 文档与代码、EAGOS 对话和运行路径所提供的设计输入。它不描述新 Paraegox 当前实现，也不产生开发授权。没有从旧仓库复制源代码或文档正文。

## 研究快照

- 旧 ParaEGOX：权威 main 4334a59af1656429f0401c0b780134c8871148e9；同时检查候选实现 e175f2a 及 2026-08-14 的本地 overlay。
- EAGOS：main 基线 6a28d11d72fd79d454c9abbb28f561507cd799b6，并检查 2026-08-14 的本地工作树。

Revision 只用于复核研究事实，不表示新 Paraegox 继承相应实现或 Git 历史。

## 研究问题

1. 分布式具身 Agent OS 的哪些方向应继续保留？
2. 哪些旧实现已经证明有产品价值？
3. 哪些复杂度阻塞了最基本的用户闭环？
4. 新仓库应按什么证据顺序重新开发？

## 仓库事实

### 旧 ParaEGOX

- 权威 main 长期没有稳定承载 TUI、AgentSession 和真实多轮对话；较多能力存在于候选分支或未提交 overlay。
- 后期实现把 Artifact、Deployment、Inspection、Ops、Receipt、治理和打包等控制面放到基本 Agent 对话之前。
- 已有 Agent 路径主要把当前一轮输入发给模型，没有稳定的 system prompt、历史上下文和 tool loop。
- TUI 与 Inspection 等诊断能力发生耦合，诊断失败可以阻塞最基本的聊天入口。
- 文档和 CI 精确记录了大量局部候选与运行证据，但这些记录没有持续证明 main 上的最小用户闭环。

### EAGOS

- EAGOS 存在真实的聊天入口、Session history、Model Provider、流式输出和 Tool 调用路径，证明这些语义具有直接产品价值。
- EAGOS 同时把简单对话接入较长的 Belief、Supervisor、Reasoner、Reporter、Router、双 transport 和完整系统启动链。
- TUI、CLI、Supervisor、Reasoner 和 console service 已膨胀为大型所有者，基本聊天还受到外部存储、daemon 或 transport 配置影响。
- EAGOS 的远程工具、Node 与 Device 概念值得参考，但没有一条系统证据完整闭合 TUI、Agent、远程设备和结果返回。

## 有价值的历史结论

- Paraegox 的目标仍是分布式具身 Agent OS，不是普通聊天框架。
- distributed-ready contracts 与 local-first execution 可以同时成立：先用本地闭环验证语义，再把同一合同跨进程、跨 Node。
- TUI 是薄客户端，不拥有 AgentSession、模型凭据、Fabric 或设备状态真值。
- AgentSession 拥有有序历史；Model Provider 保持窄接口；Token 与 Authoritative Final 分离。
- Agent 通过 Tool 或 Command 请求能力，不直接操作 Driver。
- Device Node 独占本地设备控制、claim、Safety、超时和实际结果。
- 网络分区或上游故障时，本地最低安全不能依赖云端继续在线。
- Event、Command 和 Stream 是不同语义；transport 只是适配层。
- 所有队列、重试、递归、并发和工具循环必须有界。

## 明确拒绝的失败模式

- 先按完整架构图创建 crate、服务、协议版本、registry、journal 和兼容层。
- 用单轮 echo、组件单测、fixture、候选分支或 CI 通过宣称 Agent 对话已经完成。
- 让 Inspection、Deployment、Artifact、Ops 或外部数据库成为聊天启动前置。
- 同时支持多种 transport、存储和部署方式，却没有一条稳定的产品路径。
- 把通用 Graph、VFS、State Store、Service Locator 或完整治理平台放入 Kernel。
- 把旧 ADR 的 Accepted 状态、旧协议编号、旧 milestone 和实现快照直接移植。
- 在正式文档中持续追加 commit、CI run、每日状态、候选证据和 authorization receipt。
- 为了未来兼容而保留没有当前 consumer 的抽象。

## 研究结论

第一项产品证据应是 TUI 与 Agent 的真实两轮上下文对话；之后应在同一入口逐步加入 Observation、低风险模拟设备、跨 Node 调用和真实设备，而不是横向建设控制面。稳定演进方向由 architecture/system-model.md 表达，当前提议范围和验收只由 plans/current.md 拥有。

每个新增公共概念必须在当前 golden scenario 中拥有 producer、consumer 和 failure test。

## 尚未决定

- 新仓库的开源许可证。
- M1 的终端 UI 实现方式和首个真实 Model Provider。
- Rust 核心与多语言 workload 之间何时出现第一条真实语言边界。
- M3 是否接受 Zenoh 为首个生产 transport，以及最小 wire contract。
- 首个真实设备和对应的本地 Safety 验收条件。

这些问题只在会改变当前里程碑实现时进入新的 Research 或 ADR，不提前展开为框架。
