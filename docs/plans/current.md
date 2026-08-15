# 当前计划：TUI–Agent 基础收口

状态：Active
日期：2026-08-15

## 为什么现在只做这一件事

当前最优先目标不是 Device、Card Link 或新的分布式能力，而是让开发者能清楚地启动一个 Node，并从独立终端与它承载的 Agent 完成最简单的多轮对话。

main 已经存在一条真实的确定性路径：

```text
独立 TUI
  → typed AgentConversationClient
  → Fabric exact binding
  → Node / RuntimeHost
  → AgentService + Agent Card
  → server-owned Session history
  → one Turn terminal
```

这条路径已通过两进程两轮系统测试，但默认 responder 只是用于证明链路和历史的确定性实现。DeepSeek V4 Flash 代码候选也已存在，真实凭据的指定服务器 smoke 尚未完成。因此必须区分“本地对话链路已验证”和“真实模型对话已验收”。

## 本批次 Outcome

- 保持现有 Node、RuntimeHost、FabricService、Deck/Card 和 AgentService 运行语义不变。
- 把已经过大的单文件按真实 owner 拆成少量私有模块；公共 crate 路径保持不变。
- 保留一个简单、可理解的行式 TUI：提示用户目标 Node、支持多轮输入、`/quit`、EOF 和 Ctrl-C。
- 独立 Node 与独立 TUI 的现有系统场景明确使用 `/quit` 退出，并继续证明第二轮历史位于 AgentService。
- 通过当前 focused 与 workspace 门禁；服务器网络恢复后，在指定 Ubuntu 服务器复跑同一 revision。

## 代码边界

本批次不新增 crate。

- `paraegox-agent`
  - `service`：Session、Turn、history、deadline/cancel、Agent Card profile 生命周期。
  - `provider`：deterministic responder 与固定 DeepSeek adapter。
  - `transport`：typed conversation client、Fabric binding 与 wire bounds。
  - crate root 只保留稳定合同和 re-export。
- `paraegox-runtime`
  - 分开 status、Deck/Card runtime ownership 和 RuntimeHost 生命周期编排。
- `paraegox-fabric`
  - 分开服务端 lifecycle/admission 与独立客户端。
- `paraegox` binary
  - TUI 循环从命令解析和 Node composition 中分离。

`paraegox-deck`、`paraegox-node` 和 `paraegox-kernel` 当前仍足够小且职责凝聚，保持单文件。模块只因现有职责边界而存在，不为未来功能预留空壳。

## In scope

- 机械整理现有代码，不改变公共 API、wire、Session/Turn 语义或启动停止顺序。
- 保留 deterministic responder 作为无凭据、本地可重复的默认路径。
- 保留固定 DeepSeek V4 Flash 候选以及已有的 secret、body、output、deadline 和 terminal 边界。
- 只调整现有高价值测试；不因拆文件复制测试或建立 fixture/mock 框架。
- README 清楚说明当前 TUI 是行式界面，确定性路径和真实模型路径处于不同验证状态。

## Non-goals

- Device crate、DeviceService、模拟硬件、Observation/Command、Card Link 或工具调用。
- 全屏 TUI、streaming token、Markdown renderer、会话持久化或重连恢复。
- Provider registry、动态模型选择、自动 retry/fallback 或多 Provider 平台。
- 新的 Runtime/Fabric/Agent contracts crate、service locator、通用消息总线或 graph engine。
- 跨宿主安全、Deployment、NodeDaemon、安装升级或硬件接入。

## 验收

- 不新增 crate、依赖或设备概念；生产模块均有当前真实 consumer。
- `cargo fmt --all -- --check` 通过。
- `cargo check --workspace --all-targets --locked` 通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- `cargo test --workspace --all-targets --locked` 通过。
- 独立 Node + 独立 TUI 的两轮确定性场景通过，第二轮精确引用第一轮输入，并由 `/quit` 有界退出。
- 代码审查未发现 TUI 绕过 Fabric 直调 AgentService，也未让 RuntimeHost 退化为 service locator。
- 分支合入 main 并通过 Ubuntu CI 后，才能声称本批次代码完成。

## 外部待办

以下证据不会阻塞本地代码整理，但必须诚实保留：

- 指定 Ubuntu 服务器对合入 revision 的 clean-checkout deterministic smoke。
- 仅在服务器已有安全环境变量时，使用真实 `DEEPSEEK_API_KEY` 完成同一 Node/TUI 两轮非敏感对话；不在命令、日志、仓库或 artifact 中记录密钥。

在真实凭据 smoke 完成前，DeepSeek 路径仍只能称为实现候选，不能称为已完成的真实 Agent 对话。

## Stop condition

完成上述模块整理、最简单两轮 TUI 场景、workspace 门禁、审查和 Ubuntu CI 后立即停止。不要自动进入 Device、Card Link、全屏 UI、streaming、Memory 或下一里程碑。
