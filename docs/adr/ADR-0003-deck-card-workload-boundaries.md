# ADR-0003：Deck 与 Card 工作负载边界

状态：Accepted
日期：2026-08-15

## 背景

Paraegox 的目标不是只运行一组固定 CoreService，而是让可组合的 Agent、感知、工具和控制能力在 Node 的 RuntimeHost 上运行。旧 ParaEGOX 的 `CardDefinition → Card → CardInstance` 与 Deck workload 区分具有产品价值，但其实现又提前引入 Deployment、Artifact、Graph、版本化 wire 和大量没有真实消费者的机制。EAGOS 的 Bundle/Module 证明了组合心智模型的价值，也证明万能 Loader、Module 基类和 Runtime service locator 会阻塞最小用户闭环。

## 决定

- Paraegox 采用唯一语义链 `CardDefinition → Card → CardInstance`：
  - CardDefinition 是不可变、可复用、可版本化且无运行身份的能力定义。
  - Card 是该定义在一个 Deck 中的一次具名、配置使用。
  - CardInstance 是 RuntimeHost 托管的运行身份与私有实现。
- Paraegox 采用 `DeckSpec → DeckLock → DeckRun` 区分 workload 期望、精确解析结果和运行事实。
- Deck 是声明式可执行 workload，由 Cards、当前真实消费者需要的 typed Links 和明确 Requirements 构成。它不等于 Product、Application、Installation、Deployment、进程或 Runtime。
- RuntimeHost 拥有 DeckRun/CardInstance 的本地生命周期、generation、readiness、deadline 和关闭；Card 私有实现不能自行创建第二套运行身份或取得完整 RuntimeHost。
- AgentService 与 DeviceService 是 CoreService。具体 Agent profile 属于 Agent Card；设备 Tool/Controller adapter 可以是 Card；平台会话状态、Driver、本地授权和设备 readback 不归 Card 所有。
- Fabric 只安装和承载编译后的 binding，不解释 Deck、AgentSession 或 Device Operation；同进程 Card 不得以对象直调绕过正式路径。
- CardDefinition、Deck compiler 与 source workload 语义位于 Runtime 外；Runtime 只消费本 Node 当前运行所需的精确结果。

## 分阶段准入

- 第一个 Deck/Card 切片最多新增一个纯 `paraegox-deck` crate；不建立 card、deck-contracts、graph 或 application 等并列 crate。
- 首版只接受 exact built-in CardDefinition，不实现 Marketplace、Artifact resolution、semver solver、动态 plugin ABI 或多语言 worker。
- `DeckLock` 只有在其 exact identity/digest 被真实 DeckRun 使用时才实现；不先建设自定义 binary wire、签名或 migration family。
- Port/Link 只有在两张真实 Card 组成 producer/consumer 路径时进入代码；首版只支持静态、typed、1:1、有界 binding。
- Deck/Card 基础不能作为孤立框架合并，必须与同一交付批次的真实 CardInstance 和用户可观察路径一起进入 main。

## 明确不包含

- DeploymentController、自动 placement、rollout/reconciliation。
- Artifact/package/signature、Catalog/Marketplace。
- Application/Installation 与跨 DeckRun 私有持久状态。
- 通用 Graph Engine、动态 service registry、GlobalBus。
- fan-in/fan-out、动态图、热替换、协议版本矩阵和测试 DSL。

这些能力只有在当前 workload 路径出现无法规避的真实 producer、consumer、owner 与 failure evidence 后，才通过新的有界决定准入。

## 后果

Deck/Card 会在 AgentService 正式产品路径中尽早成为真实 owner，避免具体 Agent、工具与设备编排重新硬编码进 Node 或 Runtime。代价是第一版只能运行内置、静态、受信的 Rust Card，并且不提供完整应用安装或动态部署体验；这是当前阶段有意接受的限制。
