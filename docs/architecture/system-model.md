# Paraegox 系统模型

日期：2026-08-14

本文记录用户已经确认的“分布式具身 Agent OS”产品方向，并提出基于当前研究的工作边界。除产品方向外，具体语义仍需由后续实现证据或 ADR 确认；本文不定义当前实现能力，也不把文中角色预先冻结为 crate、service、trait 或 wire type。当前实现状态以根 README、main 上的代码和可执行场景为准。

## 系统目标

Paraegox 是分布式具身智能 Agent OS：Agent 理解来自现实或模拟环境的 Observation，通过有结果、有期限的 Command 请求设备所在 Node 执行受约束的 Operation，并把可核验结果返回给用户。

    Client / TUI
        → Human Turn
        → Agent Session
        → Model decision
        → Tool Command
        → Device Node
        → Local authorization and safety checks
        → Device Operation and state readback
        → Tool Result
        → Authoritative Final
        → Client / TUI

这些名称首先表达产品路径中的语义角色。只有出现当前 producer、consumer 和 failure test 时，它们才可以成为稳定公共合同或独立组件。

## Golden scenario

第一条长期保留的具身验收场景是：

> 用户在 TUI 要求 Agent 检查模拟机器人的指示灯；如果灯是关闭的，就打开它并说明实际结果。

Agent 基于同一 Session 理解请求，先消费带来源和时效的灯状态 Observation，再决定是否发出带 action identity 和 deadline 的 Command。设备所在 Node 在本地校验请求和安全条件、处理并发冲突、执行或拒绝操作，并回读最终状态。Agent 根据明确 Tool Result 生成本轮唯一 Final。

同一场景必须覆盖失败分支：

- Observation 过期或未知时不得被当成确定事实。
- 设备 Node 离线、超时或拒绝时，Agent 必须在有限时间内返回明确失败。
- Transport acknowledgement 不是物理成功。
- 结果未知的副作用不能自动重放。

首个执行器使用低风险、可逆、可回读的模拟指示灯。移动底盘、机械臂和其他高风险动作等待本地执行边界被真实验证后再进入范围。

## 已确认边界

- TUI 是薄客户端。它提交 Session identity 和本轮输入，显示 transcript，但不拥有模型上下文、凭据、设备状态或执行真值。
- Agent 侧拥有有序 Session history、Model Provider 调用和有界推理过程；Agent 不直接持有 Driver。
- Observation 必须带来源与时效。模型输入和模型文字都不能自动成为环境或设备真值。
- Command 有明确目标、identity、deadline 和 terminal result；Event 表达已经发生的事实；Stream 只承载增量显示或连续数据。它们是不同语义，即使未来共享同一种 transport。
- 设备所在 Node 拥有 Driver 和最终执行判断，在本地验证请求与安全条件、拒绝冲突、执行 deadline 与停止，并完成状态回读。
- 断网、上游 Agent 故障或云端不可用时，设备 Node 仍能拒绝不安全操作并进入本地最低安全状态。
- Token 如果存在，只用于显示；每轮只有一个 Authoritative Final 进入 Session 并完成客户端请求。
- 队列、重试、并发、递归、工具循环和 fan-out 必须有界。

## 演进方式

开发使用同一条产品路径逐步增强：先完成有上下文的本地 Agent 对话，再加入模拟 Observation 与低风险设备操作，然后把同一语义跨进程、跨 Node，最后接入一个真实可回读设备。

plans/current.md 只提议或记录一条有界切片，只有用户当前请求能够授予开发范围。本文不预先选择 TUI 库、模型供应商、transport、语言 ABI、部署方式或 crate 划分，也不授权提前建设 Deployment、Artifact、Inspection、Ops、通用 Graph、插件市场或其他控制面。
