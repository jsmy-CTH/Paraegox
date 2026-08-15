# 当前计划：M3 Deck Agent 确定性终端对话

状态：Implemented candidate（Mac 已验证，等待 Ubuntu CI/main）
日期：2026-08-15

## 授权边界

用户已授权按完整路线继续开发，并明确 Deck/Card 是 Paraegox 的核心模型、AgentService 属于 CoreService，同时要求初期只保留少量高价值测试。本文只拥有下一条可执行切片：把最小 Deck/Card、真实 DeckRun/CardInstance、AgentService 与独立终端聊天客户端放进同一条生产路径。

M2b 的 mTLS 方案已经收束，但真实跨宿主完成依赖服务器网络与外部证书部署；它不阻塞本地 Deck/Agent 路径，也不得被本计划暗中降级为明文远程连接。

## Outcome

一个 Node 启动 RuntimeHost、FabricService、AgentService 和内置 Agent Deck。RuntimeHost 创建真实 DeckRun 与 Agent CardInstance；该 Card 激活 Agent profile 后，另一个终端进程经 typed Fabric client 连续提交两轮输入，第二轮确定性回答必须引用服务端保存的第一轮历史。

```text
paraegox node run --node-id node-a --deck builtin-agent
    Node
      └── RuntimeHost
          ├── FabricService
          ├── AgentService
          └── DeckRun
              └── Agent CardInstance

paraegox tui --target node-a
    → typed AgentConversationClient
    → Fabric exact binding
    → AgentService session/history
    → one authoritative final per turn
```

该结果证明 Deck/Card 已进入真实 Agent 用户路径，并解决最小终端多轮对话；它不声称已经接入真实模型、全屏 TUI、工具循环、设备或跨宿主安全链路。

## 当前基线

- M1 与 M2a 已经通过 Ubuntu CI 合入 main；当前 main 能运行同机 loopback 双 Node，并由 Node B 使用自己的长期 Fabric session 查询 Node A。
- 当前分支已经把 RuntimeHost 收敛为非泛型的有界 CoreService owner，并让 FabricService 承载构造期固定的 runtime-status 与 Agent bindings。
- 当前分支已经实现最小 Deck compiler、真实 DeckRun/CardInstance、AgentService、ephemeral Session/Turn 与独立行式终端客户端；回答器仍是确定性实现，不是真实模型。
- 指定服务器 clean-checkout smoke 仍待网络恢复；用户已临时允许在 Mac 编译验证。

## In scope

- 新增唯一 `paraegox-deck` crate，保存纯 workload 语义与最小编译：
  - `CardDefinitionRef → Card → ResolvedCard`
  - `DeckSpec → DeckLock`
  - exact built-in definition、重复 key/未知 definition 拒绝、确定性 lock identity
  - 不依赖 Tokio、Runtime、Fabric 或 Agent
- 新增唯一 `paraegox-agent` crate，作为真实领域与生命周期边界，容纳：
  - AgentService CoreService 与不延长 owner 生命周期的 typed handle
  - ephemeral Session、ordered history、Turn identity 与唯一 terminal
  - built-in deterministic Agent Card implementation
  - typed AgentConversationClient 与当前 JSON codec
  - M3 内部 deterministic responder；真实 Provider seam 到 M4 有真实 adapter 时再公开
- RuntimeHost 改为非泛型 owner：
  - 构造时接收数量有上限、不可动态查找的 CoreService 序列
  - 按显式顺序启动、逆序停止，不提供 name/TypeId/Any lookup 或 service accessor
  - CoreService 全部启动后启动至多一个 DeckRun；停止时先停 CardInstance/DeckRun
  - 任一步失败都逆序清理已经启动的 owner，并聚合 cleanup failure
- Runtime 真实拥有 `DeckRunId`、`CardInstanceId`、generation、状态与 snapshot；Node status 展示当前 DeckLock identity、DeckRun 和 CardInstance。
- AgentService 只有一个当前 Agent profile、一个有界请求入口和有界内存：
  - 没有 live Agent Card 时拒绝对话
  - 成功才原子提交 user/final；取消、超时、失败不提交 partial
  - 同一 Session 一次最多一个 active Turn；重复 TurnId 内容一致时返回已有结果，冲突内容拒绝
  - stop 撤销 admission、取消 active Turn 并等待已有请求结束
- FabricService 只因第二个真实 consumer 增加最窄能力：
  - 多个构造期固定 exact query binding，payload 与并发有界
  - opaque async handler，不理解 Agent、Session 或 Deck
  - 长期 `FabricClient` 提交 bounded request；不暴露 raw Zenoh
  - Runtime status 与 Agent conversation 都不得 direct-call fallback
- `paraegox node run` 增加 `--deck builtin-agent`；未指定 Deck 时保留 M2 Node 行为。
- `paraegox tui --target <node> --connect <endpoint>` 是独立、行式最小终端 TUI：只持 SessionId、输入与显示，不持 history、profile、provider 或 raw Fabric。

## Non-goals

- 真实 Model Provider、API key、模型 registry/router/fallback、本地模型或 token streaming。
- ratatui/full-screen layout、会话列表、Inspection、HUD、后台 watch 或 Web Console。
- Tool loop、SubAgent、Memory/Belief、Reflection、Task、Budget 或长期 session persistence。
- Deck 文件 loader、Catalog、Artifact、semver solver、签名、Deployment、placement、reconciliation 或多 Deck。
- Card Port/Link、Graph、fan-in/out、动态 plugin ABI 或多语言 worker。
- DeviceService、Observation、Command、Driver 或物理操作。
- TLS/mTLS 实现、非 loopback endpoint 或跨宿主完成声明。

## 实现顺序

1. 实现纯 Deck compiler 与确定性 DeckLock，并由 built-in Agent definition 成为真实 consumer。
2. 将 RuntimeHost 收敛为固定 CoreService 序列 + 单 DeckRun owner，补启动失败逆序回滚。
3. 将 Fabric 的单 binding 收敛为固定、有界 opaque bindings，并增加长期 typed client 所需的 payload request。
4. 实现 AgentService、Agent Card 激活/撤销、Session/Turn terminal 与 deterministic responder。
5. 在 Node composition 中显式取得 typed handles 后移交 owner，贯通 `node run --deck` 与独立 `tui`。
6. 更新 README 当前能力，跑完整 gates、PR、Ubuntu CI；服务器恢复后补同一 main revision smoke。

中间提交必须保持当前 M2 路径可构建。Deck substrate、Agent 空壳或 Runtime 通用框架都不能脱离最终两轮用户路径单独合入 main。

## 验收

- RuntimeHost 创建真实 DeckRun/CardInstance；Node status 中的 identity、generation 与 DeckLock 对应，不由 CLI 伪造。
- AgentService 的 wire handler 在没有 live Agent Card 时明确拒绝对话；不含 Agent workload 的 Node 可以完全不安装 AgentService 或 Agent binding。
- 独立终端进程使用同一 Session 连续输入两轮；第二轮 final 精确引用第一轮用户输入，证明 history 在 AgentService。
- 每轮恰好一个 terminal；deadline 与 cancel 都有 wall-clock bound，provider 晚到结果不能生成第二个 terminal。
- Card stop 后 profile 失效；Node shutdown 顺序为 Card/DeckRun → AgentService/Fabric 的明确逆序，所有 owned task 被 join。
- M1 external probe 与 M2 双 Node/长期 session 场景无回归。
- 同一 revision 通过 fmt、check、Clippy `-D warnings`、workspace tests 和真实 Node/TUI 双进程场景。

## 测试约束

只保留四组高价值证据：Deck compiler 的确定性与 fail-fast；Runtime 多 owner 的启动/逆序回滚；Agent turn 的唯一 terminal/cancel/timeout；一条真实 Node + 独立 TUI 两轮系统场景。优先在同一测试中覆盖相关失败分支，不为 DTO、getter、显示格式或无分支 wrapper 堆单测，不创建 mock framework、fixture 目录或测试 DSL。

## Stop condition

真实 DeckRun/Agent Card 成为 AgentService 的 admission 前提，独立终端客户端完成确定性两轮有上下文对话，取消/超时/关闭有界，M2 无回归，并通过当前可用环境与 Ubuntu CI 后，本里程碑停止。

不自动加入真实模型、全屏 TUI、Port/Link、DeviceService、Deployment 或持久化；它们分别由后续里程碑拥有。
