# ADR-0002：Node、RuntimeHost 与 Fabric 边界

状态：Accepted
日期：2026-08-14

## 背景

Paraegox 的第一个产品基线需要一个可被外部进程寻址的 Node，并在其上运行 Runtime。旧 ParaEGOX 和 EAGOS 的实现经验同时暴露了两种风险：把 Runtime 做成拥有所有业务的 service locator，以及把 Bus 做成通信、调度、服务发现和业务语义的通用入口。

## 决定

- Node 不等于 RuntimeHost。Node 是可寻址的运行、资源和故障边界；RuntimeHost 是该 Node 内的本地执行与生命周期 owner。
- RuntimeHost 只承载和监督 CoreService，管理 startup、readiness、stop 与宿主级 lifecycle deadline；它不拥有 Agent、Model、Device 或 Fabric 的业务语义。CoreService 拥有并 join 自己的内部任务；当前不预建通用 cancellation tree、task registry 或 drain protocol。
- CoreService 是平台拥有、具有明确生命周期和窄服务 API 的能力。它只获得当前工作所需的明确上下文和已声明依赖，不获得完整 RuntimeHost、动态 service locator 或 raw transport。
- FabricService 是 CoreService，是生产跨进程、跨宿主和跨 Node 通信的 owner。Paraegox 使用 Fabric，不另建 GlobalBus、LocalBus 或通用 Bus API。
- Message 与服务语义由具体 owner 定义；Fabric 负责承载、路由、binding 与连接生命周期，不吸收 AgentSession、Device Operation 或 Runtime 控制面。
- 当前首阶段只实现一个 Node、RuntimeHost、FabricService 与外部只读 probe。不实现 AgentService、TUI、DeviceService、Deployment、Artifact、Inspection、Journal、数据库或通用控制面。
- 当前无远程身份与加密合同，Fabric endpoint 仅限 loopback；不能把双进程证据描述为跨宿主分布式能力。

具体 Fabric 产品与 crate 数量由各实现切片的当前消费者决定，不在本 ADR 中永久冻结。

## 考虑过的替代方案

### TUI 直接调用 AgentSession

拒绝作为第一基线。它可以快速得到聊天 UI，但没有验证 Node、RuntimeHost 和分布式通信边界。

### 用通用 Bus 统一本地与远程调用

拒绝。通用 Bus 容易逐步承担 service locator、RPC、调度和业务协调，使 Runtime 与 CoreService 失去边界。

### 一开始建设完整 Node 控制面

拒绝。Discovery、Deployment、持久 presence、Journal 和恢复并不是当前 probe 路径的前置条件。

## 后果

正面后果：

- Node、RuntimeHost、CoreService 和 Fabric 从第一条执行路径就具有真实责任。
- RuntimeHost 可以保持为小型生命周期 owner，不成为业务容器。
- 后续 AgentService 和 DeviceService 能够在同一运行骨架上演进。

负面后果：

- 最初用户可见功能是 Node probe，而不是 Agent 对话。
- 跨进程 Fabric 引入了比纯本地函数调用更多的启动、超时和关闭失败面。

## 范围与重审条件

只有真实的运行证据表明 Node 与 RuntimeHost 无法分离、Fabric 不能承担当前通信，或 CoreService lifecycle 边界阻止了已授权产品路径时，才重审本 ADR。

新增 Agent、Device 或第二个 Node 只是后续里程碑，不自动推翻本决定。
