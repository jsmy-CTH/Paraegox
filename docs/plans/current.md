# 当前计划：Textual 备用客户端与聊天前端收口

状态：Active
日期：2026-08-16

## 为什么现在做这一件事

Paraegox 已经有一条真实的两进程对话路径，也已经有可用的 Ratatui 全屏终端：

```text
Ratatui / line client
  → typed AgentConversationClient
  → Fabric exact binding
  → Node / RuntimeHost
  → AgentService + Agent Card
  → server-owned Session history
  → one authoritative Turn terminal
```

用户希望保留一个更接近旧 EAGOS 视觉与交互方式的 Textual 前端。这个批次增加一个显式备用客户端，同时整理现有 Ratatui 模块；不改变 Node、Runtime、Fabric、Deck/Card 或 Agent 的 owner，也不让 Python 成为第二个 transport owner。

## Outcome

- `paraegox tui` 继续默认使用已经验证的 Ratatui 界面，非 TTY 时继续保持原有行模式。
- `clients/textual` 提供一个可显式启动的 Python Textual 备用界面，采用深色三栏布局，并在窄终端折叠。
- Textual 只启动同版本的本地 Rust binary；对话仍经过 `AgentConversationClient → Fabric → AgentService`。
- Rust 提供仅供 bundled Textual 客户端使用的有界 JSONL stdio adapter，支持 ready、submit、cancel、唯一 terminal 和 graceful shutdown。
- 现有 Ratatui 单文件按 lifecycle、application state、view 三个私有职责整理，公共行为不变。

## 边界

- 不新增 Rust crate、CoreService、HTTP Bridge、Python Zenoh client 或第二套 Agent wire。
- Textual 是备用 presentation adapter，不是自动 fallback，也不是独立远程客户端。
- JSONL 是同仓、同版本前端之间的私有本地合同，不宣称为第三方公共 API。
- Python 生成真实 UUID `TurnId`；同一 `SessionId + TurnId` 用于 submit、cancel 和 terminal，不增加第二套 request identity。
- 一个 Textual 子进程只拥有一个临时 Session，最多一个 active Turn，不排队并发请求。
- `ready` 只表示本地 adapter 与 Fabric client 已打开，不能显示为 Agent、Provider 或目标 Node 已健康。
- Session history、幂等与 authoritative terminal 仍由 Node 内 AgentService 独占。
- 模型文字按纯文本显示，不启用 Rich markup，也不伪造 streaming、thinking 或 tool activity。
- stdin、stdout、错误、等待、取消和退出全部有明确上限；stdout 只承载 JSONL，stderr 只承载有限诊断。

## In scope

- Textual 的 Session、Chat、Target/Agent 三栏界面与响应式折叠。
- Enter 提交、Esc 取消、Ctrl-C 有界退出，以及子进程异常的诚实状态。
- Rust `--stdio-jsonl` 私有适配模式及一个真实 Node/Fabric/Agent 系统场景。
- 一个 Textual headless 界面测试，不铺设低价值 widget 测试。
- 将 Ratatui `tui.rs` 机械拆为入口/lifecycle、状态和 view 私有模块，零功能扩张。
- README 中英文启动说明与现有 CI 中的一个最小 Python 门禁。

## Non-goals

- TUI 直连 Zenoh、复制 Agent private wire、HTTP Console Bridge 或临时内嵌 Node。
- token streaming、Markdown 渲染、thinking、tool/approval、Trace、日志、资源监控或命令面板。
- Session 列表、切换、持久化、历史同步或断线恢复。
- Device、Card Link、Memory、Deployment、NodeDaemon、跨宿主安全或新的 Provider。
- 发布 PyPI 包、公共多语言 SDK、协议协商、schema registry、代码生成或兼容层。
- 自动在 Ratatui 与 Textual 之间切换。

## 验收

- 新增 Rust crate：0；Python 只依赖固定版本 Textual 和标准库。
- 真实 Node + JSONL client 完成 ready、空闲取消不下传、两轮服务端历史和有界 stopped/exit。
- Textual headless 测试覆盖 submit、final、cancel、shutdown，并证明模型样式标记按字面显示。
- 原 Ratatui 全屏、非 TTY 行模式、响应式布局和 terminal restore 语义不变。
- `cargo fmt --all --check` 通过。
- `cargo check --workspace --all-targets --locked` 通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- `cargo test --workspace --all-targets --locked` 通过。
- Python 锁文件同步，headless test 在 CI 中通过。
- 审查确认没有 transport 旁路、detached 子进程、无界 framing 或虚假健康状态。
- 分支进入 main 并通过 Ubuntu CI 后，才能声称本批次代码完成。

## 外部待办

- 指定 Ubuntu 服务器恢复后，对合入 revision 做 clean-checkout deterministic 与 Textual PTY smoke。
- 仅在服务器已有安全环境变量且允许发送非敏感文本时，完成 DeepSeek V4 Flash 两轮 smoke；真实凭据 smoke 完成前，外部 Provider 仍是实现候选。

## Stop condition

Textual 备用客户端、Rust adapter、Ratatui 结构整理、focused/workspace 门禁、审查和 Ubuntu CI 完成后立即停止，不自动进入 streaming、Session 管理、Device、Card Link 或下一里程碑。
