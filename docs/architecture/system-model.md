# Paraegox 系统模型

日期：2026-08-14

本文记录已确认的“分布式具身智能 Agent OS”方向和稳定边界。它不把所有语义角色预建为 crate、service、trait 或 wire type，也不代替 main 上的可执行证据。

## 系统目标

Paraegox 使 Agent 理解来自现实或模拟环境的 Observation，通过有 identity、deadline 和 terminal result 的 Command，请求设备所在 Node 执行受约束的 Operation，并把可核验结果返回给用户。

```text
Client / TUI
    → Agent CoreService
    → Observation / Command
    → Fabric
    → Device Node
    → local authorization, operation and readback
    → Tool Result
    → Authoritative Final
```

Fabric 是跨进程、跨宿主和跨 Node 的生产通信边界。Paraegox 不建立同时承担路由、调度、服务定位和业务语义的通用 Bus。

## Node、RuntimeHost 与 CoreService

```text
Node
└── RuntimeHost
    ├── FabricService              # CoreService
    ├── AgentService               # 后续里程碑
    └── DeviceService / Authority  # 后续里程碑
```

- **Node** 是可寻址的运行、资源和故障边界，拥有稳定配置身份与每次启动都变化的 incarnation。
- **RuntimeHost** 是 Node 内的本地执行 owner，拥有每次启动的 epoch，管理 CoreService 的启动、readiness、停止与宿主级 lifecycle deadline。CoreService 必须拥有并 join 自己的内部任务；通用 cancellation tree 或 drain protocol 只随真实消费者加入。Node 不等于 RuntimeHost。
- **CoreService** 是由 RuntimeHost 承载、具有明确生命周期和窄服务边界的平台能力。CoreService 不得取得完整 RuntimeHost 或动态 service locator。
- **FabricService** 是 CoreService，拥有生产通信 session、binding 和关闭。Message 语义由具体 owner 定义，Fabric 只承载和路由，不理解 AgentSession 或设备操作。
- **AgentService** 和未来的 **DeviceService** 是 CoreService；它们各自拥有 Agent 与设备领域语义，RuntimeHost 不吸收这些语义。

服务客户端应通过 typed service API 与 Fabric binding 访问 CoreService，不应调用“万能 Runtime API”或 raw transport。具体 crate 划分仍由当前 producer、consumer 和失败边界决定。

## 当前第一条可执行路径

第一里程碑不先做孤立聊天应用，而是先证明一个 Node 可以被外部进程寻址，并在其上真实运行 RuntimeHost 和 FabricService：

```text
Process A: paraegox node run --node-id node-a
    Node(node-a, incarnation)
        └── RuntimeHost(host identity, epoch)
            └── FabricService(ready)

Process B: paraegox node probe --target node-a
    → Fabric request
    → Node / Runtime lifecycle snapshot
    → bounded terminal response
```

这条路径证明了一个可寻址 Node 的跨进程分布式基线，但不声称已完成双 Node、跨宿主或具身执行闭环。窄 probe 只读取 Node、RuntimeHost 和 FabricService 的 identity 与 readiness，不演变为通用控制面。

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

1. 建立单个可寻址 Node、RuntimeHost、FabricService 与外部进程 probe。
2. 在该 RuntimeHost 上运行 AgentService，由薄 TUI 通过 Fabric 完成有上下文对话。
3. 加入模拟 Observation、受约束 Command 和低风险 Device Operation。
4. 将同一语义扩展到第二个真实 Node、跨宿主链路和故障验证。
5. 接入一个真实可回读设备。

`plans/current.md` 只记录当前获授权的有界切片。本文不授权提前建设 Deployment、Artifact、Inspection、Ops、通用 Graph、插件市场或其他控制面。
