# 取消安全相关工作

以下链接均指向可核查的项目文档、源码仓库或论文原始页面（访问核对日期：2026-09-24）。它们分别覆盖运行时语义、future 测试、API 指南、静态 lint、RAII guard 与经验研究；本项目的第三阶段仅评估 Rust 中“共享/持久状态先改变、await 后恢复/提交”这一种轻量语法检测模式，不将任何单项工具的范围扩大解释为通用取消安全证明。

| 工作 | 主要方法/范围 | 与 Dropwise 的关系及边界 |
|---|---|---|
| [Hyperactor `hyperactor::testing::cancel_safe`](https://meta-pytorch.org/monarch/stable/rust-api/hyperactor/testing/cancel_safe/index.html) | Monarch 的 Rust API 提供 `assert_cancel_safe` 与 `assert_cancel_safe_async` 一类测试辅助：推进 future 到各个 Pending/yield 边界，丢弃后重建并检查操作是否能按预期完成；文档把状态有效、可重启、无半提交副作用列为 cancel-safe 要求。 | 与 Dropwise 最接近的是“按 Pending 边界取消并检查重试”的动态思想。Dropwise 用场景探索和调用方不变量，涵盖共享应用状态与后续恢复；Hyperactor 的 helper 更聚焦单一 future 的重启结果。两者都需要调用者提供预期/契约，测试通过不等于对所有执行作静态证明。 |
| [Asupersync 项目与集成文档](https://github.com/Dicklesworthstone/asupersync), [架构/取消协议](https://github.com/Dicklesworthstone/asupersync/blob/main/docs/integration.md) | Rust 异步运行时项目，强调结构化任务所有权、显式取消/排空/最终化，以及部分通信表面的 reserve/commit 和取消正确原语；提供测试运行时与确定性执行设施。 | 这是将取消语义纳入运行时/组件设计的路线，而非本研究使用的源代码模式检测器。Dropwise 可用于检查采用常规 Tokio 异步接口的应用场景，不要求迁移到特定运行时；本阶段未测量 Asupersync 的覆盖率或性能，也不把其项目声明当作本实验结论。 |
| [`futures-testing`](https://github.com/conradludgate/futures-testing) | 对 leaf future 进行属性化/随机 poll 测试；驱动端可交错推进、取消、伪唤醒及 Waker 变更，检查 Waker 契约和取消后的状态。 | 与 Dropwise 的互补之处是它能压力测试 poll/Waker 交互和多种随机交错；Dropwise 则突出按取消计划探索被标记的应用 future，并由用户写可观察不变量。Dropwise 不做 Waker 合约模糊测试，`futures-testing` 也不是本阶段这一模式的跨项目扫描器。 |
| [Tokio `select!` cancellation safety 文档](https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety) | 说明 `select!` 中 loser future 会被丢弃，并讨论在循环中重建 future 的取消安全要求及常见接收操作。 | 提供本研究的执行语义和 API 背景：被丢弃 future 的局部进度不会自动续接。Dropwise 对任意调用点的共享状态作场景级断言；Tokio 文档主要讲 API 使用契约，不从任意应用代码自动找出“先改状态后 await”。 |
| [Clippy `await_holding_lock`](https://rust-lang.github.io/rust-clippy/master/index.html#await_holding_lock)、[`await_holding_refcell_ref`](https://rust-lang.github.io/rust-clippy/master/index.html#await_holding_refcell_ref)、[`await_holding_invalid_type`](https://rust-lang.github.io/rust-clippy/master/index.html#await_holding_invalid_type) | 检查锁卫/`RefCell` 借用等特定类型跨 await 持有；可配置不应跨 await 的类型。 | 这些是成熟的相关静态 lint，关注 guard/借用跨挂起点的风险。第三阶段模式关注状态先取走或写入、稍后恢复/提交；二者有交集，但这些 Clippy lint 不追踪一般的 `take/pop/remove/replace` 到后续恢复责任，Dropwise 的启发式扫描也不替代 Clippy。 |
| [`tpt-async-guard`](https://docs.rs/crate/tpt-async-guard/latest) 与 [`#[cancel_safe]` 宏说明](https://docs.rs/tpt-async-guard/latest/tpt_async_guard/attr.cancel_safe.html) | 提供 `CancelGuard`，其 rollback closure 在未 commit 时于 Drop 运行；属性宏静态拒绝持有未提交 guard 跨 await 的模式。 | 它把一种显式 RAII 回滚约定编码进类型/宏；与计划中“识别确实回滚同一状态的 Drop guard”为已防护情况相关。它主要覆盖显式 guard 与受限检查，不是一般共享状态操作的通用污点/恢复关联分析；Dropwise 候选需要按源码/测试核验 guard 的真实语义。 |
| Sethi 等，[OSDI 2022《Cancellation in Systems: An Empirical Study of Task Cancellation Patterns and Failures》](https://www.usenix.org/conference/osdi22/presentation/sethi)（[论文 PDF](https://www.usenix.org/system/files/osdi22-sethi.pdf)） | 对 13 个 Java、C#、Go 分布式/并发系统中的 62 个取消功能请求和 156 个取消相关 bug 作经验研究，并从反模式构建静态检查器。 | 为取消缺陷的真实动机、反模式归纳和静态筛查提供研究先例；但对象语言不包括 Rust，不能替代 Rust Future Drop 语义或 Tokio 实验。Dropwise 此阶段对一个从已核验 Rust 缺陷提炼的模式作冻结评估，不声称复现论文规模，也不声称覆盖其缺陷类型。 |

## 本阶段定位

`research/phase3/detector/` 是小型、语法驱动的筛查器，输出“值得核查的同一 async scope”而非缺陷结论；Q1/Q2 的召回、精确率与不可评估/未知数见 [`phase3/RESULTS.md`](phase3/RESULTS.md)。Q3 用 Dropwise 将高可信候选落实到指定执行取消边界，并区分模型与真实目标源码执行；本阶段 16 个模型结果不作目标应用缺陷证据，Neon 隔离源码结果也未达到预注册的修复前父提交比较门槛。
