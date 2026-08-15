# Paraegox 系统模型

日期：2026-08-15

本文记录已确认的“分布式具身智能 Agent OS”方向和稳定边界。它不把所有语义角色预建为 crate、service、trait 或 wire type，也不代替 main 上的可执行证据。

## 系统目标

Paraegox 使 Agent 理解来自现实或模拟环境的 Observation，通过有 identity、deadline 和 terminal result 的 Command，请求设备所在 Node 执行受约束的 Operation，并把可核验结果返回给用户。

```text
Client / TUI
    → Agent Card @ DeckRun
    → AgentService
    → typed Tool / Command
    → Fabric
    → Device Node / DeviceService
    → local authorization, operation and readback
    → Tool Result
    → Authoritative Final
```

Fabric 是跨进程、跨宿主和跨 Node 的生产通信边界。Paraegox 不建立同时承担路由、调度、服务定位和业务语义的通用 Bus。

## Node、RuntimeHost 与 CoreService

```text
Node
└── RuntimeHost
    ├── Core Services
    │   ├── FabricService
    │   ├── AgentService
    │   └── DeviceService / Authority
    └── DeckRun
        └── CardInstance*
```

- **Node** 是可寻址的运行、资源和故障边界，拥有稳定配置身份与每次启动都变化的 incarnation。
- **RuntimeHost** 是 Node 内的本地执行 owner，拥有每次启动的 epoch，管理 CoreService 的启动、readiness、停止与宿主级 lifecycle deadline。CoreService 必须拥有并 join 自己的内部任务；通用 cancellation tree 或 drain protocol 只随真实消费者加入。Node 不等于 RuntimeHost。
- **CoreService** 是由 RuntimeHost 承载、具有明确生命周期和窄服务边界的平台能力。CoreService 不得取得完整 RuntimeHost 或动态 service locator。
- **FabricService** 是 CoreService，拥有生产通信 session、binding 和关闭。Message 语义由具体 owner 定义，Fabric 只承载和路由，不理解 AgentSession 或设备操作。
- **AgentService** 和未来的 **DeviceService** 是 CoreService；它们各自拥有 Agent 与设备领域语义，RuntimeHost 不吸收这些语义。

服务客户端应通过 typed service API 与 Fabric binding 访问 CoreService，不应调用“万能 Runtime API”或 raw transport。具体 crate 划分仍由当前 producer、consumer 和失败边界决定。

## Deck 与 Card

Deck/Card 是 Paraegox 的核心工作负载模型，不是可选的编辑器外观：

```text
CardDefinition → Card → CardInstance
DeckSpec       → DeckLock → DeckRun
```

- **CardDefinition** 是不可变、可复用、可版本化的能力定义；它没有运行身份。
- **Card** 是 CardDefinition 在一个 Deck 中的一次具名、配置使用。
- **CardInstance** 是 RuntimeHost 托管的运行身份；私有实现对象不能成为第二个 lifecycle owner。
- **Deck** 是由 Cards、必要时的 typed Links 和明确 Requirements 构成的声明式可执行 workload，不等于 Product、Application、Installation、Deployment、进程或 Runtime。
- **DeckLock** 是 DeckSpec 的精确、确定性解析结果；只有其 identity/digest 被真实 DeckRun 消费时才进入实现。
- **DeckRun** 表示一次运行事实，不是 RuntimeHost 对象引用。

AgentService 仍是 CoreService，拥有 Session、Turn、Model Provider 调用和唯一 authoritative final；具体 Agent 的 prompt、角色、模型/工具策略属于 Deck 中的 Agent Card。DeviceService 仍拥有 Driver、本地执行判断和 readback；面向某类设备的 Tool/Controller adapter 可以是 Card，但 Driver 本身不是 Card。

Card 只能取得 validated config、窄 Port handle、cancellation/clock 和显式 typed CoreService client。它不能取得完整 RuntimeHost、raw Fabric/Zenoh、Driver、Secret 或动态 service locator。Card 间正式交互必须经过编译并安装的 typed binding；同进程不得产生 direct-call 旁路。

## 当前可执行路径

当前有两条共享同一 Node、RuntimeHost 和 Fabric 边界的可执行路径。

第一条是同机双 Node。两个进程分别拥有自己的 Node identity、RuntimeHost epoch 与长期 Fabric session，Node B 使用自己的 session 请求 Node A：

```text
Process A: Node A → RuntimeHost → FabricService
Process B: Node B → RuntimeHost → FabricService → bounded peer request → Node A status
```

它证明了 loopback 上两个真实 Node 的 identity、生命周期、停止与重连。外部 `node probe` 仍只是诊断客户端，不计作第二个 Node。

第二条是在一个 Node 上运行最小 Agent Deck，再由独立终端进程通过同一 Fabric 进入 AgentService：

```text
Process A: paraegox node run --node-id node-a --deck builtin-agent
    Node(node-a, incarnation)
        └── RuntimeHost(host identity, epoch)
            ├── FabricService(status + agent exact bindings)
            ├── AgentService(ephemeral sessions, ordered history, one active turn)
            └── DeckRun(lock identity, generation)
                └── Agent CardInstance(profile admission)

Process B: paraegox tui --target node-a
    AgentConversationClient → Fabric → AgentService → one terminal
```

RuntimeHost 生成 DeckRunId、CardInstanceId 与 generation；CLI 不伪造运行事实。Agent Card 激活 profile 后对话才能入场，成功才提交 user/final，取消、超时或被中止的 future 会封存为唯一 `Cancelled`/`TimedOut` terminal 而不写入成功历史。Session 与幂等记录都有上限，最旧的 inactive Session 可被淘汰。

CoreService 按当前 Node composition 安装。没有 Agent workload 的 Node 不为证明框架而常驻 AgentService 或 Agent binding；选择 `builtin-agent` 时，AgentService、binding 与真实 Card consumer 在同一条路径出现。当前回答器只是 deterministic provider，用来证明第二轮读取的是服务端历史，不代表真实模型已经接入。

两条路径目前都限制在 loopback。没有远程认证与加密，因此不声称已完成跨宿主通信或具身执行闭环。

## Golden scenario

长期保留的具身验收场景是：

> 用户在 TUI 要求 Agent 检查模拟机器人的指示灯；如果灯是关闭的，就打开它并说明实际结果。

Agent 基于同一 Session 理解请求，消费带来源和时效的 Observation，再决定是否发出带 action identity 和 deadline 的 Command。设备所在 Node 在本地校验请求和安全条件、处理并发冲突、执行或拒绝操作，并回读最终状态。Agent 根据明确 Tool Result 生成本轮唯一 Final。

同一场景必须覆盖：

- Observation 过期或未知时不得被当成确定事实。
- 设备 Node 离线、超时或拒绝时，Agent 必须在有限时间内返回明确失败。
- Transport acknowledgement 不是物理成功。
- 结果未知的副作用不能自动重放。

首个执行器使用低风险、可逆、可回读的模拟指示灯。移动底盘、机械臂和其他高风险动作等待本地执行边界被真实验证后再进入范围。

## 已确认边界

- TUI 是薄客户端，不拥有模型上下文、凭据、设备状态或执行真值。
- AgentService 拥有有序 Session history、Model Provider 调用和有界推理过程；Agent 不直接持有 Driver。
- Observation 必须带来源与时效。模型输入和模型文字不能自动成为环境或设备真值。
- Command 有明确目标、identity、deadline 和 terminal result；Event 表达已经发生的事实；Stream 只承载增量显示或连续数据。
- 设备所在 Node 拥有 Driver 和最终执行判断，在本地完成校验、冲突拒绝、deadline、停止和状态回读。
- 断网、上游 Agent 故障或云端不可用时，设备 Node 仍能拒绝不安全操作并进入本地最低安全状态。
- Token 如果存在，只用于显示；每轮只有一个 Authoritative Final 进入 Session。
- 队列、重试、并发、递归、工具循环和 fan-out 必须有界。

## 演进方式

1. 收口单个可寻址 Node、RuntimeHost、FabricService 与外部进程 probe，并在服务器验证后合入 main。
2. 建立两个真实 Node；每个 Node 使用自己长期持有的 Fabric session 完成有界 peer 请求。先做同机双 Node，不把普通 probe client冒充第二个 Node。
3. 引入最小 Deck/Card 执行语义，并让 RuntimeHost 创建真实 DeckRun/CardInstance；Deck substrate 不脱离同批真实 Card consumer 单独合并。
4. 在 Deck 中运行 Agent Card，由 AgentService 和薄 TUI 完成确定性、再到真实模型的有上下文对话。
5. 加入首个 typed Card Link、模拟 DeviceService、Observation、受约束 Command、readback 和 Unknown 不重放语义。
6. 将同一 Deck 的 Card 分布到两个 Node，通过认证加密的跨宿主 Fabric 完成模拟具身闭环。
7. 在不改变上层合同的前提下接入一个低风险、可逆、可回读的真实设备。

跨宿主认证与加密是第 6 步的完成前置门；服务器网络暂时不可用时可以与第 3、4 步并行研究和实现，但不能因此降低远程 endpoint 的安全限制。Deployment、Artifact、自动 placement、持久 reconciliation、Application/Installation、通用 Graph 和插件系统只在出现真实消费者与故障证据后另行准入。

`plans/current.md` 只记录当前获授权的有界切片。本文不授权提前建设 Deployment、Artifact、Inspection、Ops、通用 Graph、插件市场或其他控制面。
