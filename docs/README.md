# Paraegox 文档工程

Paraegox 文档用于保存稳定方向、关键决定、研究结论和当前交付边界。文档不能替代代码、可执行测试或服务器上的系统场景，也不能自行授权新的开发工作。

## 按问题划分权威

| 问题 | 权威位置 |
| --- | --- |
| 用户现在能运行什么 | 根目录 README 与 main 上的可执行路径 |
| 已确认的产品方向和长期边界是什么 | architecture/system-model.md |
| 为什么接受一个难以逆转的决定 | Accepted ADR |
| 调查过什么、哪些结论仍不确定 | research/ |
| 当前获授权交付什么 | 用户当前请求 |
| 下一条有界切片如何提议或记录 | plans/current.md |
| 当前实际上实现了什么 | main 上的代码、测试和服务器系统场景 |

不同文档回答不同问题。发生冲突时，不使用计划或架构图覆盖实现事实，也不使用研究材料覆盖 Accepted ADR。任何计划只有在用户明确授权后才是 Active；文档本身不产生授权。

## 当前结构

- architecture/system-model.md：产品主链、系统边界、演进顺序和非目标。
- adr/：少量、长期、难以逆转的架构或项目边界决定。
- research/：有明确问题、证据、取舍和结论的非规范性研究。
- plans/current.md：唯一当前计划；完成后由下一份计划替换。

只有出现真实内容时才创建新目录。首个稳定公共合同出现后可以增加 reference/；首个需要反复执行的任务出现后可以增加 guides/；首个长期运行且需要恢复的服务出现后可以增加 runbooks/。

不创建 progress/、status/、evidence/、governance/、workbench/ 或空的分类目录。

## 文档生命周期

一个范围清楚、容易回滚的改动可以直接实现。只有重大未知会改变边界、风险或实现路线时才创建 Research；只有跨边界、长期且难以回滚的决定才创建 ADR。Architecture 吸收稳定结论，Proposed Plan 可以描述下一条有界切片，但只有用户明确授权后才能变为 Active。实现完成后，根 README 才能增加当前能力声明。

Research 状态只使用 Active、Resolved 或 Parked。ADR 状态只使用 Proposed、Accepted、Superseded 或 Rejected。Current Plan 状态只使用 Proposed 或 Active；交付结束后替换该文件，历史由 Git 保存。

Accepted ADR 不追加实现进度。被替代的 ADR 新增指向后继决定的简短说明，不重写原始决定。

## 写作与准入规则

- 一份文档只拥有一个问题；其他文档通过链接引用，不复制正文。
- 明确区分仓库事实、外部事实、推断、目标设计和开放问题。
- 当前能力声明必须有同一 main revision 上的真实 producer、consumer、失败行为和用户可观察场景。
- 新概念如果没有当前 producer、consumer 和 failure test，只能作为目标路径中的语义角色，不能被提升为独立组件、公共类型或 Reference 合同。
- 不记录每日进度、实现状态 commit hash、CI run、候选分支、authorization receipt 或人工复制的测试日志。Research 可以记录用于复核事实的 immutable source revision。
- 不从旧 ParaEGOX、EAGOS 或 PhanthyMotus 整篇复制文档；只保留中立结论，并遵守适用许可证与归属。
- 不在文档中记录凭据、模型密钥、设备秘密、隧道配置或其他敏感数据。
- 内部文档使用中文正文和稳定英文术语。当前仅根 README 维护中英文产品简介。
- 文档失效时更新、合并或删除；不通过追加层层补丁说明维持表面兼容。
- 只有两个真实问题具有不同所有权或生命周期时才拆分文件，不能仅因篇幅或架构图层级拆分。

## 与 CI 的关系

CI 是廉价回归门，不是产品完成证明。文档可以链接稳定测试入口，但不记录某次 CI 运行。当前不建设静态文档网站、文档状态检查器、治理 manifest 或文档专用 CI；出现真实发布读者和足够稳定的内容后再评估。
