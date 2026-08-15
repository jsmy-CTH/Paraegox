# 当前计划：M4 固定 DeepSeek V4 Flash Provider

状态：Active
日期：2026-08-15

## 授权边界

M3 Deck Agent 确定性终端对话已经合入 main 并通过 Ubuntu CI：RuntimeHost 创建真实 DeckRun/CardInstance，Agent Card 激活 AgentService profile，独立 TUI 通过 Fabric 完成有服务端历史的两轮对话。指定服务器对该 main revision 的 clean-checkout smoke 仍待网络恢复后补充；这项待办不能被 Mac 或 GitHub Actions 证据代替。

本文只拥有 M4 的下一条有界切片：在保留 deterministic 默认路径的同时，为同一个 AgentService 接入一个固定的 DeepSeek V4 Flash Provider，并用真实凭据完成最小两轮对话。本文不授权模型平台、Provider 生态或工具 Agent 扩展。

## Outcome

不指定 Provider 时，M3 行为不变：

```text
node run --deck builtin-agent
    → deterministic responder
```

显式选择 Provider 时，同一 Deck/Card/Session/Turn 路径调用唯一外部模型：

```text
node run --deck builtin-agent --provider deepseek-v4-flash
    → AgentService committed history
    → fixed DeepSeek Chat Completions adapter
    → one complete provider response
    → one Authoritative Final
```

TUI 命令和 Fabric wire 不感知 API key、HTTP endpoint 或模型响应格式。外部 Provider 成功后，第二轮请求必须由 AgentService 发送第一轮已提交的 user/assistant 历史；取消、deadline、HTTP 错误或非完整模型响应仍只产生一个 terminal，且不提交 partial。

## 当前基线与外部合同

- main 已包含 M1、M2a、M3，并通过 M3 对应的 Ubuntu CI；M3 指定服务器 smoke 仍 pending。
- 当前 AgentService 已拥有有界 ephemeral Session、ordered committed history、服务级单 active Turn、cancel/deadline 和幂等 terminal；这些语义不能转移给 Provider。
- 当前 deterministic responder 是已验证默认实现，必须保留，避免没有外部凭据时破坏本地开发与系统测试。
- DeepSeek 当前 OpenAI-compatible endpoint 是 `https://api.deepseek.com/chat/completions`，本切片只使用 `deepseek-v4-flash`，并显式关闭 thinking、关闭 stream。旧的 `deepseek-chat` / `deepseek-reasoner` 名称不进入实现。
- DeepSeek Chat Completions 是无状态调用；多轮 `messages` 由 Paraegox 重建。参考 [DeepSeek Chat Completions API](https://api-docs.deepseek.com/api/create-chat-completion) 与 [DeepSeek Multi-round Conversation](https://api-docs.deepseek.com/guides/multi_round_chat)。
- DeepSeek 文档没有本切片可依赖的远端 cancel 合同。关闭本地 HTTP future 只能停止 Paraegox 等待；不能宣称远端推理或计费已经停止。参考 [DeepSeek Rate Limit & Isolation](https://api-docs.deepseek.com/quick_start/rate_limit)。

## In scope

- 在现有 `paraegox-agent` owner 内加入第一个有真实 consumer 的最窄 Provider seam 和 DeepSeek adapter；不新增 Provider crate。
- 生产配置固定为：
  - endpoint `https://api.deepseek.com/chat/completions`
  - model `deepseek-v4-flash`
  - non-streaming
  - thinking disabled
  - 本地有界 output、response body 与总 deadline
- 请求只包含 built-in Agent system profile、已成功提交的有序 user/assistant history 和当前 user input；Cancelled、TimedOut、Failed turn 不进入上下文。
- 只从 Node 进程环境读取 `DEEPSEEK_API_KEY`，以 Bearer header 发送；缺失或无效时明确失败，但不输出密钥、Authorization header 或完整敏感 body。
- 把 HTTP/Provider 结果收敛到现有 terminal：
  - 只有 `finish_reason=stop`、非空且有界的完整 content 可以成为 Authoritative Final
  - `length`、`content_filter`、`tool_calls`、`insufficient_system_resource`、非法 schema 和超限 body 都作为失败，不提交 partial
  - `400/422`、`401`、`402`、`429`、`500/503` 与 transport/decode/timeout 分型到少量 provider-neutral failure
- `node run` 只增加精确枚举值 `--provider deepseek-v4-flash`；省略该参数仍选择 deterministic responder。
- timeout/cancel 后立即封存现有唯一 terminal；晚到 Provider 结果必须被丢弃，不能产生第二个 terminal 或写入成功历史。

## Non-goals

- Provider registry、动态 discovery、router、模型自动选择、fallback 或多 Provider 配置。
- 自动 retry、backoff、hedging、请求 replay 或跨 Provider failover。
- streaming token、SSE、reasoning display、tool calls、JSON mode、Responses API 或 Anthropic API。
- 自定义生产 base URL、代理配置、客户端传 endpoint、客户端传 model 或任意 OpenAI-compatible 服务。
- API key 文件、Deck secret、CLI key 参数、Fabric secret、Secret manager 框架或密钥轮换系统。
- 持久 Session/Memory、Model cache、prompt template 系统、token budget scheduler、usage billing service 或 observability 平台。
- Card Link、DeviceService、跨宿主认证加密、部署和硬件能力。

## 实现顺序

1. 在 `paraegox-agent` 内定义由 deterministic responder 与一个 DeepSeek adapter 实际消费的窄调用合同；不公开 registry 或 service locator。
2. 实现固定 request/response codec、Bearer header、body/output 上限和 provider-neutral error mapping；生产 URL 与 model 不接受外部覆盖。
3. 将 AgentService 已提交历史映射成 `system/user/assistant` messages；只有完整成功 content 进入现有原子 commit。
4. 把 Turn cancellation 和绝对 deadline 传播到 HTTP future；证明 late response 不能改变 terminal 或 history。
5. 在 Node composition 中加入精确 `--provider deepseek-v4-flash` 选择；无参数时保留 deterministic 路径，TUI 与 Fabric wire 不变。
6. 通过本地无密钥的 fake-server 合同测试和现有 workspace gates；服务器可用后，以进程环境中的真实凭据完成显式、非敏感的 Node/TUI 两轮 smoke。

## 验收

- 不指定 `--provider` 时，现有 deterministic 两轮系统路径、M1 probe 与 M2 双 Node 场景无回归。
- 指定 `--provider deepseek-v4-flash` 但进程环境没有 `DEEPSEEK_API_KEY` 时，Node fail-fast，输出中不包含凭据或 Authorization header。
- fake-server 证据覆盖请求历史顺序、Bearer header 存在但不泄露、完整成功 response、Provider error，以及 timeout/cancel 后 late response 不提交。
- 外部模式仍由 AgentService 独占 Session/history/terminal；TUI、Fabric、DeckLock 与 Card snapshot 不出现 DeepSeek credential 或 HTTP DTO。
- 同一 revision 通过 fmt、check、Clippy `-D warnings`、workspace tests 和 deterministic Node/TUI 系统场景。
- 指定 Ubuntu 服务器使用真实 `DEEPSEEK_API_KEY` 启动 Node，独立 TUI 完成两轮非敏感对话，第二轮请求消费第一轮已提交历史；该 credentialed smoke 的 revision 与结果被明确记录。

在最后一项真实服务器证据完成前，M4 保持 Active，README 和系统模型只能称其为候选或待验证路径。

## 测试约束

只增加一组高价值 Provider 合同测试，合并覆盖成功、错误、deadline/cancel 与 late response；不为每个 HTTP 字段建立单测，不新增 mock framework 或 fixture 目录。真实 credentialed smoke 不进入默认 CI，也不在日志、命令行或测试 artifact 中保存 key、完整 prompt 或完整 response。

## Stop condition

固定 DeepSeek V4 Flash 的非流式调用通过现有 AgentService 完成真实两轮对话，唯一 terminal 与成功历史语义保持不变，deterministic 默认路径无回归，Ubuntu CI 与指定服务器 credentialed smoke 均有证据后，M4 停止。

不自动进入 Provider registry、retry/fallback、stream、tools、DeviceService 或其他后续里程碑。
