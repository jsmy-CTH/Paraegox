# 当前计划：EAGOS 风格全屏聊天 TUI

状态：Active
日期：2026-08-15

## 为什么现在做这一件事

Paraegox 已经有一条经过两进程测试的真实对话路径：

```text
独立终端客户端
  → typed AgentConversationClient
  → Fabric exact binding
  → Node / RuntimeHost
  → AgentService + Agent Card
  → server-owned Session history
  → one Turn terminal
```

当前缺口是面向人的客户端仍只是行式输入输出。这个批次只把它收敛成可日常使用的全屏聊天界面；不改变 Node、Runtime、Fabric、Deck/Card 或 Agent 的 owner，也不借 UI 扩张控制平面。

## Outcome

- 交互终端默认进入 EAGOS 视觉语言的全屏聊天界面：深色背景、青色边框、角色分色、会话/聊天/状态三栏和底部输入。
- 布局按终端尺寸退化：宽屏三栏、中屏两栏、窄屏单栏；极小终端给出明确提示且仍可退出。
- 非交互 stdin 或 stdout 保留原有行模式及输出合同，继续服务脚本和真实两进程系统测试。
- Enter 提交，Esc 清空空闲输入或取消当前 Turn，Ctrl-C 有界取消后退出，`/quit` 仍可退出。
- 任意正常、错误或取消路径都恢复光标、raw mode 和 alternate screen。

## 边界

本批次不新增 crate，只在现有 `paraegox` binary 的 `tui` 模块中增加呈现与输入状态。

- TUI 仍只持有 `AgentConversationClient`，生产路径必须经过 Fabric；禁止直调 AgentService。
- Session history 仍由 Node 内 AgentService 独占，TUI 只保存当前进程的临时 SessionId 和有界显示记录。
- UI 只展示当前能证明的事实：配置的目标 Node/endpoint、Fabric client 是否打开、本地 Session、Turn 状态和终端结果。
- Provider 仍只返回 authoritative final，因此界面只能显示 `waiting for final`，不能伪造 token streaming、thinking 或 tool activity。
- Ratatui/Crossterm 只负责终端呈现和事件；不建立 UI framework 或新的 service。

## In scope

- 三档响应式布局和 EAGOS 风格配色。
- Unicode 文本输入、退格、提交、取消、退出和 resize。
- 有界输入与消息记录；禁止并发提交第二个 Turn。
- terminal lifecycle 的 RAII 恢复。
- 一个 render 测试覆盖宽、中、窄和极小终端。
- 保留现有独立 Node + 非 TTY TUI 两轮对话系统测试，不复制 fixture 或铺设低价值测试。
- README 中英文同步当前交互方式和能力边界。

## Non-goals

- EAGOS 的 Console Bridge、TUI 直连 Zenoh、临时内嵌服务器或 transport fallback。
- Session 列表/切换/持久化、历史同步或重连恢复。
- token streaming、Markdown、thinking、tool/approval、Trace、拓扑、日志、资源监控或命令面板。
- Device、Card Link、Memory、Deployment、NodeDaemon 或跨宿主安全。
- 新 UI crate、widget framework、theme system 或插件系统。

## 验收

- 不新增 crate；新增依赖仅限 Ratatui、Crossterm 及其异步事件适配。
- `cargo fmt --all -- --check` 通过。
- `cargo check --workspace --all-targets --locked` 通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- `cargo test --workspace --all-targets --locked` 通过。
- 真实独立 Node + 非 TTY TUI 两轮场景仍精确证明服务端历史和 `/quit` 有界退出。
- TestBackend 覆盖三档布局与极小尺寸，CHAT/INPUT 不因折叠丢失。
- PTY smoke 验证中文输入、提交、Ctrl-C、`/quit` 和 terminal restore；响应布局由 TestBackend 覆盖。
- 审查确认 TUI 没有绕过 Fabric、没有显示无法证明的状态、没有引入 detached task。
- 分支合入 main 并通过 Ubuntu CI 后，才能声称本批次代码完成。

## 外部待办

- 指定 Ubuntu 服务器对合入 revision 的 clean-checkout deterministic smoke。
- 仅在服务器已有安全环境变量时完成 DeepSeek V4 Flash 两轮非敏感对话；真实凭据 smoke 完成前，外部 Provider 仍是实现候选。

## Stop condition

全屏聊天 TUI、现有两轮路径、workspace 门禁、PTY smoke、审查和 Ubuntu CI 完成后立即停止，不自动进入 streaming、Session 管理、Device、Card Link 或下一里程碑。
