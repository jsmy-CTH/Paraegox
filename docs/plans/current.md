# 当前计划：M2a 同机双 Node Fabric 基线

状态：Active
日期：2026-08-15

## 授权边界

用户已授权继续完成首个分布式具身 Agent 闭环。本计划只拥有下一条依赖就绪切片：让两个真实 Paraegox Node 使用各自 RuntimeHost 承载的长期 Fabric session 完成同机 peer 请求。后续 Deck/Card、AgentService、TUI、远程认证和设备能力仍按独立里程碑进入，不得在本切片提前创建。

## Outcome

启动 Node A 与 Node B 两个独立 Node。每个 Node 都拥有独立 Node incarnation、RuntimeHost epoch 和长期 FabricService session；Node B 通过自己的 session 请求 Node A 的 typed runtime status，成功后继续运行直至 Ctrl-C。目标停止、超时和重启都有明确、有界结果。

这条路径证明同机双 Node，而不是“一个 Node 加一个临时 probe client”。它不声称已经完成认证跨宿主通信、Deck/Card、Agent 对话或具身执行。

## 当前基线

- M1 已通过 PR 与 Ubuntu GitHub Actions 合入 main；服务器 `/work/Paraegox` clean-checkout smoke 因管理链路不可达仍待补。
- 每个 Node 当前只声明一个 exact runtime-status binding。
- `node probe` 每次新开并关闭一个临时 Zenoh client session，只能作为诊断客户端，不能作为 M2 双 Node 证据。
- FabricService 已拥有长期 session，但 Node 尚无窄 handle 使用它发出请求。

## In scope

- 不新增 crate，不修改 Kernel 或 RuntimeHost 边界。
- `paraegox-fabric` 增加不暴露 Zenoh 类型的窄 `FabricHandle`：
  - 只能使用已 Ready 的 FabricService session 发出 bounded exact query。
  - Service start 完成后发布 session；stop 开始时先撤销 handle admission，再 undeclare、join、close。
  - 出站请求复用长期 session，不自行 open/close，不建立通用 RPC 或 service locator。
- FabricService 启动配置增加最多一个显式 loopback connect endpoint；multicast scouting 继续关闭。
- `paraegox-node` 持有自己的 FabricHandle，并提供 typed `probe_peer`；Node status key、JSON codec 与目标 NodeId 校验仍归 Node owner。
- `paraegox node run` 增加成对的 `--connect`、`--probe-peer` 和本次 peer deadline：
  - Node B 先进入 Ready 并输出本地状态。
  - 随后用自己的 Fabric session 请求目标 Node，输出 typed peer status。
  - peer 请求失败时先干净停止 Node B，再明确退出失败。
- 现有 `node probe` 保留为外部诊断命令，但不得进入 M2 验收路径。

## Non-goals

- 非 loopback endpoint、TLS、远程身份认证、凭据或跨宿主完成声明。
- discovery、membership、leader election、presence、peer registry 或 cluster controller。
- 第二个 inbound binding、远程触发 B 再次 probe 的管理 API、stdin 控制面或周期性监控任务。
- 通用 RPC、Bus、service registry、route topology 或 retry framework。
- Deck/Card、AgentService、TUI、Model、DeviceService、Deployment 或持久化。
- 多 endpoint、动态 connect/disconnect、应用层无限 retry 或兼容性协议家族。

## 实现顺序

1. 为现有 FabricService 增加共享 session admission 和长期 FabricHandle，保持 stop/start 错误与 deadline 有界。
2. 让 Node 在构造时取得 handle，并实现 typed peer status 请求。
3. 扩展 `node run` 的显式 peer 参数，贯通两个真实 Node 进程。
4. 用现有测试文件增加一条双进程场景和一条长期 B session 的重启场景；不建立新测试框架。
5. 更新 README 当前能力，运行本地授权环境的完整 gates，提交 PR 并通过 Ubuntu CI；服务器恢复后补同一 main revision 的 smoke。

## 验收

- Node A 与 Node B 都执行完整 `Node → RuntimeHost → FabricService` start/stop 路径。
- B 收到的 A NodeId、incarnation、RuntimeHostId、epoch 与 A 自己输出完全一致。
- 进程级验收不调用 `node probe`，不存在第三个临时 Fabric client。
- A 不存在时，B 的 peer probe 在 deadline 内失败，且 B 自己干净停止。
- 保持一个真实 B Fabric session，不重启 B：A 停止后请求有界失败；A 用同一 endpoint/NodeId 重启后，B 再次请求成功并观察到新的 incarnation/epoch。
- 两个 Node 的 Ctrl-C/显式 stop 均有界，端口可重新使用，没有遗留任务。
- 同一 revision 通过 fmt、check、clippy、workspace tests 和真实双 Node 场景。

## 测试约束

只保留三类高价值证据：一条 Fabric owner 被异常丢弃后立即撤销 handle admission 的窄生命周期测试；一条真实双 `node run` 进程场景；一条直接使用两个真实 Node/Fabric session 的断开重连场景。测试必须有独立 wall-clock deadline。不给 DTO、getter、简单解析包装器或 Zenoh 自身行为增加重复单元测试，不追求覆盖率数字，不创建测试 DSL、fixture 目录或 mock framework。

## Stop condition

两个真实 Node 使用各自长期 Fabric session 完成成功、停止、超时和重启 identity 场景并进入 main 后，本里程碑停止。若跨宿主认证尚未完成，只能声明“同机双 Node”；不得放宽 loopback 限制或提前进入分布式 Deck 完成声明。
