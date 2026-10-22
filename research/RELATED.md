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

## 跨域责任与在线门控

2026-10-22 补充。以下按原论文或官方文档的相关章节核对，服务于 [进度见证研究提案](theory/PROGRESS-GUARDS.md)，不改变上面的第三阶段评估范围。

| 工作 | 可复用的机制 | 要研究的差异，而非已经证明的新颖性 |
|---|---|---|
| Flatt、Findler，PLDI 2004，[Kill-Safe Synchronization Abstractions](https://users.cs.utah.edu/plt/publications/pldi04-ff.pdf) | 让可被终止的任务使用不被其终止破坏的同步抽象；管理线程和放弃通知帮助维护共享状态。 | 后继安全和责任移交已有先例。我们的候选问题是带外部协议／内核效果的资源交接，不将模型差异直接当作贡献。 |
| Weimer、Necula，OOPSLA 2004，[Finding and Preventing Run-Time Error Handling Mistakes](https://people.eecs.berkeley.edu/~necula/Papers/rte_oopsla04.pdf)，§7 | 用一等补偿栈动态记录责任，支持提前执行、显式解除；补偿抛异常时记录并继续其他补偿。 | 不应声称前人假定清理不会失败。可研究异步责任转交、恢复完成条件及恢复期间的复用权限。 |
| Filipović、O’Hearn、Rinetzky、Yang，TCS 2010，[Abstraction for Concurrent Objects](https://flint.cs.yale.edu/cs428/doc/abscobj.pdf) | 将并发对象正确性与客户端可观察行为的精化联系起来。 | survivor 可作为区分上下文；有限场景不是对所有客户端的精化证明，pre/post 也不是通用规范。 |
| Fowler、Lindley、Morris、Decova，POPL 2019，[Exceptional Asynchronous Session Types: Session Types without Tiers](https://www.pure.ed.ac.uk/ws/portalfiles/portal/77051141/exceptional_asynchronous_session_types_final.pdf) | 将异步会话类型与异常处理结合，用显式取消及传播维护协议行为，并在 Links 中实现。 | 可借鉴端点取消与责任传播；论文不是“静态类型必须退化成运行时监控”的结论。单侧兼容既有服务的门控需另行验证。 |
| Yang、Sar、Engler，OSDI 2006，[EXPLODE](https://www.usenix.org/legacy/event/osdi06/tech/full_papers/yang_junfeng/yang_junfeng.pdf) | 通过受控探索和检查器查找真实存储系统错误，包含故障／恢复后的状态检查。 | 可复用故障后继续观察的测试结构；取消保留哪些易失状态、如何恢复须由自己的故障模型定义。 |
| Porter 等，SOSP 2009，[TxOS](https://www.cs.unc.edu/~porter/txos/) | 为一组系统调用提供隔离执行和原子发布。 | 门控只保护声明的交接条件，不提供整组系统调用事务。不能以未经核实的“未被主线采用原因”来论证自己的优势。 |
| Saha、Lozi、Thomas、Lawall、Muller，DSN 2013，[Hector](https://jean-pierre.lozi.org/documents/dsn13.pdf) | 利用函数自身错误处理代码识别资源释放遗漏。 | 可借鉴阶段／释放关系的局部推断；动态验证补充失败对象销毁对其他存活对象的影响，不宣称静态方法不能处理生命周期。 |
| Pakki、Lu，CCS 2020，[Exaggerated Error Handling Hurts! An In-Depth Study and Context-Aware Detection](https://www-users.cse.umn.edu/~kjlu/papers/eecatch.pdf) | 工具 **EeCatch** 根据上下文判断错误处理是否过于严重。 | 可借鉴“防护不能比原错误造成更大伤害”的评估视角；不是 ErrDoc，也不是专门的共享引用清理验证器。 |
| [Linux Runtime Verification](https://docs.kernel.org/trace/rv/runtime-verification.html) | 根据规范监控事件轨迹，并提供 reactor；不局限于离线告警。 | 优先复用自动机／事件组织。候选增量在于从取消反例提取哪些规则、保护哪项交接，而不是首次在线执行规则。 |
| [SQLx Transaction](https://docs.rs/sqlx/0.9.0/sqlx/struct.Transaction.html)、[Pool acquire](https://docs.rs/sqlx/0.9.0/sqlx/struct.Pool.html#method.acquire) 与项目已有守卫补丁 | 已有事务 Drop 回滚、acquire 取消后淘汰连接等处理。 | 必须作为成熟基线，不能把主流库概括为“只在 I/O Error 时丢弃”。比较阶段覆盖、误拒绝、恢复成本和接入成本。 |

这些工作支持把进度、责任和交接一起研究，但不支持“加几个 bit 即零开销”“有限测试证明零误报”或“已有在线规则必然需要离线综合”。提案中的可证伪实验用于区分这些可能性。


## 构造期与复用边界的直接先例（2026-10-22 收尾）

- [`tokio-postgres` 0.7.18 的 `TransactionBuilder::start`](https://docs.rs/tokio-postgres/0.7.18/src/tokio_postgres/transaction_builder.rs.html) 在等待 BEGIN 前建立 `RollbackIfNotDone`。这直接覆盖“服务端效果已开始、返回给调用者的 Transaction 尚未建立”的窗口，不能宣称现有 RAII 未考虑构造期取消。[本地真实上游对照](../prospective/upstream-pg-baseline/README.md) 已验证 typed BEGIN 取消后服务端回到 idle。
- [Go `database/sql/driver`](https://pkg.go.dev/database/sql/driver) 将复用前的 `SessionResetter` 与归还前的 `Validator` 分开，且明确讨论未回池而直接复用。接口本身不保证每个驱动都实现完整清理，但不是“只有回池才有检查”。
- [PostgreSQL Pipeline](https://www.postgresql.org/docs/current/libpq-pipeline-mode.html) 要求关联请求队列与返回结果，并处理 Sync 之前的错误传播。它与独立 stream 的多路复用不等价；应分别检验请求级关联和连接级恢复权限。

本轮采用“离线发现为主、在线强制器验证修复”的定位，具体决策与对最新评审的事实修正见 [研究提案的收尾更新](theory/PROGRESS-GUARDS.md#当前收尾判断跨层责任中断与状态失步)。已有类型状态、补偿与会话取消机制是可复用的基础；本项目要验证的增量在跨层遗漏的系统发现、契约表达、迁移成本及强制边界，而不是所有权令牌本身。
