# 当前计划：M1 可寻址 Node 与 RuntimeHost 基线

状态：Active
日期：2026-08-14

## 授权边界

用户已明确要求“开始 plan，然后开发”。本计划授权的只是一个可寻址 Node、其 RuntimeHost、FabricService 和外部 probe 构成的第一条可执行切片。

## Outcome

在一个进程中启动具有 identity 和 incarnation 的 Paraegox Node，由其 RuntimeHost 启动并承载 FabricService；第二个外部进程经真实 Fabric 在有限时间内 probe 该 Node，读取 Node、RuntimeHost 和 FabricService 的 identity 与 readiness。

该结果建立跨进程、可寻址的分布式 Node/Runtime 基线。它不声称已完成双 Node、跨宿主通信、Agent 对话或具身执行。

## 当前基线

- 本计划启动时，仓库只有单一最小 Rust CLI 和 CLI smoke test。
- Node、RuntimeHost、CoreService lifecycle、FabricService 与外部 probe 尚未实现。
- main 是唯一集成权威。用户已临时允许在 Mac 编译；服务器稳定后，同一 revision 还需在 Ubuntu clean checkout 验证。

## In scope

- 保留 Cargo workspace，建立当前路径真实使用的最小 crate 边界：
  - `paraegox`：CLI 与静态组合。
  - `paraegox-kernel`：当前路径使用的 Node identity、incarnation/epoch 等无 I/O 基础机制。
  - `paraegox-node`：Node identity 与窄 probe 语义。
  - `paraegox-runtime`：RuntimeHost 和最小 CoreService lifecycle。
  - `paraegox-fabric`：FabricService、生产 Fabric session 与当前固定 binding。
- `paraegox node run --node-id <id>` 启动 Node，并执行 `created → starting → ready → stopping → stopped`；RuntimeHost 对 CoreService lifecycle 施加宿主级 deadline，FabricService 在 stop 返回前 join 自己的请求任务。
- Node 拥有稳定 NodeId 与每次启动都变化的 incarnation；RuntimeHost 拥有运行 identity 与 epoch。
- FabricService 作为当前真实 CoreService，完成启动、readiness 和关闭。
- 当前 Fabric adapter 固定使用 Zenoh 1.9 的 peer/client loopback TCP query/reply；关闭 multicast scouting，不要求 zenohd，也不把 vendor 选择提升为永久架构合同。认证远程链路不在本切片内，因此拒绝非 loopback endpoint。
- `paraegox node probe --target <id>` 从第二个进程经 Fabric 发送固定、只读请求，并输出 Node/Runtime/Fabric 状态。
- probe 有明确 deadline 和唯一 terminal response；目标不存在、Runtime 未 Ready 或超时时明确失败。
- Ctrl-C 后停止接受新请求，并完成 RuntimeHost、Fabric session 和后台任务的有界关闭与 join。

上述 crate 必须在同一切片中进入真实可执行路径，不先合入空架子。

## Non-goals

- AgentService、AgentSession、Model Provider、TUI 和真实模型对话。
- DeviceService、Observation、Command、Driver 和物理操作。
- 第二个 Node、跨宿主验证、自动 discovery 和多 transport。
- 通用 Bus、动态 service registry、service graph、插件和 SDK。
- NodeDaemon、Deployment、Artifact、Inspection、Ops、Evidence 和 Web Console。
- Journal、数据库、恢复、迁移、长期 presence 或持久化节点状态。
- 为未来版本预建 protocol family、compatibility layer、fixture 仓库或通用测试 DSL。

## 交付切片

1. 建立五个获授权 crate 的最小单向依赖，只实现当前 producer 和 consumer 需要的 identity、lifecycle 和 Fabric binding。
2. 贯通 `node run` 与 `node probe` 的真实双进程路径，没有 direct-call 或伪成功 fallback。
3. 验证成功、目标不存在、超时、重启 identity 变化和 Ctrl-C 关闭。

每个中间 commit 都必须保持 workspace 可构建；不将无当前消费者的 interface、mock 或空 service 当作功能进度。

## 验收

- 进程 A 以明确 NodeId 启动并进入 Ready；进程 B 经生产 Fabric probe 它。
- probe 返回与运行实例一致的 NodeId、Node incarnation、RuntimeHost identity/epoch 和 Runtime/Fabric readiness。
- Node 不存在或停止后，probe 在 deadline 内明确失败，不回退到本地直调。
- 停止再启动同一 NodeId 后，Node incarnation 与 Runtime epoch 均变化。
- Ctrl-C 后 Node 进程有界退出，Fabric session 和 Runtime 任务没有遗留。
- 同一 revision 通过 `fmt`、`check`、`clippy`、workspace tests 与一条真实双进程系统场景。
- Mac 结果是服务器未稳定期间的临时证据；服务器恢复后，还需一次 Ubuntu clean-checkout smoke 才能对外声称完成。

## 测试约束

单元测试只覆盖风险较高、通过系统场景难以定位的不变量，例如非法 lifecycle 转换、旧 epoch 误用、deadline 和重复 terminal。不为 getter、简单构造、格式化或无分支包装器建立测试，不追求测试数量或表面覆盖率。

当前保留的验证层级为：

- 少量 owner 附近的单元测试。
- 一条真实 Fabric 双进程 integration test。
- 一次服务器手工 smoke。

## Stop condition

用户可以从两个独立进程完成 `node run` / `node probe`，成功与失败分支有界，重启 identity 变化，RuntimeHost 和 FabricService 可干净关闭，且同一 revision 完成当前可用环境的验证后，本里程碑停止。

不自动开始 AgentService、TUI、第二个 Node、DeviceService、Deployment 或持久化；后续切片需要用户重新授权。
