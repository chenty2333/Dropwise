# Async Rust 及结构化并发生态中的取消（cancellation）：2018–2026 现状地图（面向 Dropwise 改进）

> 调研日期：2026-10-18。本文只收录本轮实际检索或抓取到的来源。没有抓取核验、只凭既有知识写的条目都标注了“（未本轮核验）”，或放在 Gaps 里。
> 每条后的 **Dropwise 启发** 写明三件事：做法是集成、借鉴还是区分；粗略工作量（S 约 ≤1 天，M 约几天，L 约数周以上）；是不是低垂果实。
> 已在 `research/RELATED.md` 中出现的条目（Hyperactor cancel_safe、Asupersync、futures-testing、tpt-async-guard、Clippy await_holding_*、OSDI'22 Sethi et al.）这里只补充新细节。

## 问题 1：工具与库：有没有更新的 cancel-safety 测试器、lint、分析器，确定性模拟器与运行时诊断是否支持取消？

### Takeaway
截至 2026-10，没有找到任何工具能对既有测试**自动地在每个 Pending 处 drop 被 select!/timeout 包裹的分支**，也没有工具对 select!/timeout 宏做插桩。最接近的有三个：Hyperactor 的 `assert_cancel_safe`（对单个 future 枚举第 k 次 Pending 后 drop，再重跑比较结果）、conradludgate 的 `futures-testing`（对叶子 future 模糊 poll 顺序、取消和 waker 交换）、Asupersync 的 lab runtime（只在自家运行时内部有 obligation、loser-drain 等 oracle）。静态侧新出现了 `async-rust-lsp`（基于 tree-sitter 的 select! 分支黑名单），另有 `aselect`、`guarden`、`cancel-safe-futures` 等“缓解型”库。Dropwise 的“既有测试 + 宏补丁 + 运行时 obligation”组合在生态中仍属空白。

### Cited Findings

**A. 直接相关的取消测试器**

- **Hyperactor `testing::cancel_safe`**（Meta monarch）：模块文档把 cancel-safe 定义为在任意 `Poll::Pending` 边界上同时满足三条：(1) State remains valid；(2) Restartability holds，即重新构造同一逻辑操作的新 future 仍能完成并得到期望结果；(3) No partial side effects。做法是先用 noop waker 把 future 跑到完成，数出 Pending 次数，然后对每个边界 k 新建 future、poll k 次再 drop，最后验证“新的一次运行”仍产出 expected。反例列了“flush 了半帧再返回 Pending 的 writer”和“从共享队列消费后 Pending、drop 时不回滚的 future”。— [monarch/hyperactor/src/testing/cancel_safe.rs](https://github.com/meta-pytorch/monarch/blob/main/hyperactor/src/testing/cancel_safe.rs)；该仓库 `channel/net/framed.rs` 内部用它测试 framed 通道。
  - **Dropwise 启发**：借鉴它的**“取消后重试 = 不取消”差分 oracle**（restartability）。很多 bug（read_exact 丢字节、recv 后处理再 await 丢消息）在“取消后再调用一次”时才暴露。可提供 `ctx.retry_after_cancel(make_fut)` 一类 API，自动比较“取消 k 次再重试”与“一次跑完”的输出和副作用。工作量 S–M，属低垂果实。Hyperactor 用 noop waker 手动 poll，不能处理依赖 Tokio 驱动（I/O、timer）的 future，Dropwise 在真实 runtime 中跑，这是差异点。

- **`futures-testing`**（conradludgate，0.1.1，2026-02-21，下载约 272）：README 称它会“randomly interleaves polling, driving, cancellation, spurious wakeups, and waker swaps, then asserts that the waker contract is upheld after every step”。能捕获 lost waker、stale waker、spurious poll intolerance，以及“state corruption when a future is dropped mid-await and retried”。用户提供 driver（对端）和 factory（产生被测 future），底层用 arbtest，失败可以按 `.seed()` 复现。— [GitHub README](https://github.com/conradludgate/futures-testing)；[crates.io 元数据（API 查询）](https://crates.io/crates/futures-testing)
  - **Dropwise 启发**：借鉴两种**正交的注入维度**：spurious poll（无唤醒再 poll）和 waker swap。它们揭示的是叶子 future 的 waker 契约 bug，常与取消后重试叠加出现（取消导致 waker 注销失败，重试时永远不被唤醒，表现为 hang）。Dropwise 可以在 `ctx.target` 包装层加一个可选模式：每次 Pending 后多插一次虚假 poll，或换一个 waker。工作量 S，属低垂果实，但对上层业务 bug 的收益不确定。区分点：futures-testing 面向“写叶子 future 的库作者”，Dropwise 面向“组合已有 future 的应用代码”。

- **Asupersync**（0.5.0，2026-09-12，下载约 37.8 万）：自称 “Spec-first, cancel-correct, capability-secure async runtime”。取消是 request→drain→finalize 三阶段；task 归属于 region，region 关闭须达到 quiescence。lab runtime 提供虚拟时间、确定性调度、trace replay、DPOR 风格调度探索（Mazurkiewicz/Foata 去重）。README 写明 lab report 当前从运行时状态喂入 24 个内置 oracle 中的 11 个：“task leak, obligation leak, quiescence, loser drain, finalizer, region tree, deadline monotonicity, the cancellation protocol, DOWN-message order, managed-supervisor restarts and GenServer replies”，另外 13 个（含 channel atomicity、waker dedup）“nothing in the runtime feeds”，报告里仍算作通过。— [Asupersync README](https://github.com/Dicklesworthstone/asupersync)；[docs.rs](https://docs.rs/crate/asupersync/0.5.0)
  - **Dropwise 启发**：借鉴 oracle **命名与分类**，尤其是 “loser drain”（race 失败方必须被彻底 drop 或排空）、“task leak”（被取消 future 派生的 task 不应存活）、“quiescence”（settle 后无悬挂工作）。这几项可以作为 Dropwise 的内置默认 oracle，无需用户写不变式。工作量 S–M（task leak 可借助 Tokio runtime metrics 的存活任务计数，见下文 tokio-metrics 条）。区分点：Asupersync 要求迁移到新运行时，Dropwise 直接作用于 Tokio 既有代码。另需注意，它自己承认 13 个 oracle 未接入却记为通过，引用其“覆盖面”时要谨慎。

- **`tpt-async-guard`**（0.1.0，2026-08-07）：crates.io 描述为 “Compile-time cancellation safety guard”，提供 `CancelGuard` 回滚闭包和 `#[cancel_safe]` 属性宏。— [crates.io](https://crates.io/crates/tpt-async-guard)（已在 RELATED.md）
  - **Dropwise 启发**：区分。静态检查只覆盖显式 guard 约定。Dropwise 可以把“存在 rollback guard”当作抑制误报的信号（与 G0 静态检测器的“mutated-before-await/restored-after”模式互补）。工作量 S，收益中。

**B. 静态 / 编辑器侧**

- **`async-rust-lsp`**（rgbkrk，0.3.1，2026-04-25，下载约 2.5 万）：独立 LSP，基于 tree-sitter-rust，有两条规则。`mutex-across-await` 针对 `tokio::sync::Mutex/RwLock` 的 guard 跨 await，README 指出 Clippy 只查 `std::sync::Mutex`。`cancel-unsafe-in-select` 只检查 select! 分支**左侧 future 表达式**中的 `read_exact, read_to_end, read_to_string, read_buf, read_line, read_until, write_all, write_buf, write_all_buf`，可用 `.async-rust-lsp.toml` 的 `extra` 加入项目内包装函数名。README 承认它 “can't follow function bodies across files”。— [GitHub README](https://github.com/rgbkrk/async-rust-lsp)
  - **注意矛盾**：该黑名单含 `read_buf`、`write_buf`，但 Tokio 官方文档把 `AsyncReadExt::read_buf` 和 `AsyncWriteExt::write_buf` 列为 cancel safe — [tokio::select! 文档](https://docs.rs/tokio/latest/tokio/macro.select.html)。
  - **Dropwise 启发**：区分并集成。(1) 它的“extra 包装函数名”配置说明用户确实需要声明自家 cancel-unsafe 包装。Dropwise 可以反过来**动态验证**这类声明：给出 `extra` 列表，自动对每个函数跑取消计划，确认或否认它是否真的不安全。(2) 它没覆盖 timeout、futures::select!、try_join，正好是 Dropwise 可行性研究里占多数的形态。工作量 M，非最低垂。

- **Clippy `let_underscore_future`**（Clippy 1.67+）：捕获 `let _ = some_future();` 这种静默丢弃 future 的写法，由 sunshowers 推荐。— [sunshowers: Cancelling async Rust](https://sunshowers.io/posts/cancelling-async-rust/)
  - **Dropwise 启发**：弱迁移。这是“从未 poll 就 drop”的问题，与 Pending 后取消不同。可在文档的“相关 lint”中推荐，工作量约 0。

**C. 缓解型库（改 API 而不是测试）**

- **`cancel-safe-futures`**（Oxide/sunshowers，0.1.5，2026-04-01，下载约 43.6 万）提供四类东西：(1) `SinkExt::reserve` 加 permit 的 `feed/send`，即“取消安全的预留，再同步提交”；(2) `then_try` 家族（`join_then_try!`、`join_all_then_try`、`collect_then_try` 等），某个 future 出错时**不取消**其余 future，动机是 [omicron PR 3707](https://github.com/oxidecomputer/omicron/pull/3707)；(3) 替代 `tokio::sync::Mutex` 的 `sync::Mutex`；(4) `coop_cancel` 协作式取消。— [README](https://github.com/oxidecomputer/cancel-safe-futures)
  - **Dropwise 启发**：借鉴“**错误触发的早取消**”（try_join 短路）这一取消来源。Dropwise 的竞争者时序维度可以加一种取值“competitor = Err”：在 `try_join!/try_join_all/try_for_each` 场景中让兄弟分支在任意 Pending 后返回错误，迫使目标被 drop。工作量 S，属低垂果实，并直接覆盖可行性研究里的 try_join_all 案例。

- **`aselect`**（avl，0.4.1，2026-09-05，下载约 144）：`tokio::select!` 的替代，“Never cancelling futures (except when explicitly asked to)”，future 跨循环迭代存活，且 handler 中不允许 async 代码，以避免 futurelock 式饥饿。README 中的 `receive_command` 例子很典型：先 receive、再写 security log、再查 DB，在第三步被取消时命令已被消费并记日志，却没有任何效果。— [README](https://github.com/avl/aselect)
  - **Dropwise 启发**：`receive_command` 正是“消费后、提交前”窗口的教科书例子，可直接收为 Dropwise 的示例或回归测试。工作量 S，属低垂果实（文档/示例层面）。

- **`guarden`**（ZnqbuZ，0.4.2，2026-10-05，下载约 7.5 万）：“Await inline; spawn on cancellation”。被等待的异步工作若在等待被 drop 时尚未完成，剩余 future 转交给 spawner 在后台跑完。README 自己写明这“does not guarantee completion after runtime shutdown, process exit, or a panic”。— [README](https://github.com/ZnqbuZ/guarden)
  - **Dropwise 启发**：Obligation 模型需要第三种状态“已转交（detached-to-spawn）”。被取消的 future 把义务转交给后台任务时，settle 后应检查后台任务确实履约，而不是在 drop 当下判失败。同理适用于 Dropshot 那种每请求 spawn 的模式。工作量 S–M，属低垂果实（避免误报）。

- 其他（未深入）：`async_nursery`（结构化并发原语，0.6.0，2025-01）；`futures-scopes`（0.4.0，2025-07）；`cancel-this`（协作式取消 + liveness 监控，0.6.0，2026-07）；`async-dropper`、`tokio-async-drop`（ad-hoc AsyncDrop，2023）。— [crates.io 搜索 API 结果](https://crates.io/api/v1/crates?q=structured%20concurrency)
  - **Dropwise 启发**：基本无直接迁移价值。ad-hoc AsyncDrop 实现通常在 Drop 中 spawn，因此 Dropwise 必须在 settle 阶段等待这类清理任务（与 guarden 同理）。

**D. 测试基础设施（可集成）**

- **`futures-test` 0.3.34**（rust-lang/futures-rs 官方）：提供 `FutureTestExt::{pending_once, assert_unmoved, run_in_background, interleave_pending}`、`AsyncReadTestExt::{interleave_pending, limited}`、`AsyncWriteTestExt::{interleave_pending_write, track_closed}` 以及 Stream/Sink 对应的扩展。源码示例显示，`interleave_pending` 让 reader、writer、stream 在每次操作之间先返回一次 `Poll::Pending`（例如 `poll_write` 先 Pending 再 Ready）。— [docs.rs futures-test](https://docs.rs/futures-test/latest/futures_test/)；源码 `futures-test/src/io/write/mod.rs`、`stream/mod.rs`（本轮浅克隆，commit 706e29b，2026-10-01）
  - **Dropwise 启发**：集成，属**低垂果实**。很多 I/O 在测试里一次就 Ready（内存 duplex、mock），被测 future 根本不出现 Pending，取消计划空间为空（这正是 G0 中“Pending 不足、未观测到”的成因之一）。给 `ctx` 提供 `ctx.pendingify(reader)`，内部用 `interleave_pending` 或等价的 Tokio 版包装，可以人为制造取消点，并让 `read_exact`、`write_all` 的“部分进度”窗口变得可达。工作量 S（需要给 tokio `AsyncRead/AsyncWrite` 写同款包装，futures-test 只针对 futures-io trait）。

- **`tokio-test`**（0.4.6，2026-09-25）：Tokio 官方测试工具，crates.io 描述为 “Testing utilities for Tokio- and futures-based code”。— [crates.io API](https://crates.io/crates/tokio-test)。（未本轮核验）据既有知识，其中 `io::Builder` 可脚本化 mock 读写和等待，`task::spawn` 可手动 poll 并检查 `is_woken`。
  - **Dropwise 启发**：用 `io::Builder` 的 wait 步骤在 mock I/O 中构造可控 Pending，与上一条互补。工作量 S，属低垂果实。

- **`reord`**（Ekleog，0.2.4，2025-03）：在生产代码中插入 `reord::point().await`，测试时按随机但可复现的交错一次只运行一个任务，可放进 fuzzer。README 明确说它不是模型检查器，适用于 loom 无法替换锁的场景（如 PostgreSQL 事务）。— [README](https://github.com/Ekleog/reord)
  - **Dropwise 启发**：借鉴“**显式插桩点 + 可复现 seed**”的接口形态。它和 `ctx.target` 本质相同（用户标注点），说明这类 opt-in 插桩在社区可接受。工作量约 0（只是定位参考）。

- **Shuttle**（awslabs）：随机化并发测试，支持 PCT（`check_pct`，可配置 bug depth 与执行次数）、`check_random`、DFS，可确定性复现。docs 称它为换取规模而牺牲完备性，与 loom 的穷举不同。— [docs.rs shuttle](https://docs.rs/shuttle)；[GitHub](https://github.com/awslabs/shuttle)。对 async/Tokio 的官方支持程度本轮未核验。
  - **Dropwise 启发**：正交维度。取消计划 × 调度 seed 的组合爆炸可以借鉴 PCT 的“bug depth”思想：大多数取消 bug 只需在**一个**点取消（深度 1），Dropwise 默认只枚举单点取消、按需升到深度 2，与 PCT 的经验一致。工作量约 0（策略层面），属低垂果实（指导默认值）。

- **turmoil / madsim**：turmoil 是 Tokio 生态的确定性分布式系统模拟（单线程多 host），现在拆出 turmoil-net/fs/io-uring；madsim 是 tokio 兼容的确定性模拟运行时，可做 libc 级拦截。— [S2: Deterministic simulation testing for async Rust](https://s2.dev/blog/dst)；[turmoil 源码文档](https://dev.materialize.com/api/rust/src/turmoil/lib.rs.html)；本轮未找到二者对“取消注入”的专门支持。
  - **Dropwise 启发**：集成，非最低垂。turmoil 的网络分区和断连可以作为“客户端断连导致 handler 被 drop”的**真实取消来源**，把 Dropwise 的计划映射为“在第 k 个 Pending 后对该 host 断网”。工作量 M–L。

- **loom**：本轮未检索到 loom 对 async 取消的支持。（未本轮核验）据既有知识，loom 面向原子操作/线程交错的穷举，`loom::future::block_on` 只支持简单 future，不建模取消。
  - **Dropwise 启发**：区分。基本不重叠，迁移价值弱。

**E. 运行时诊断**

- **Tokio task dump**：需要 `tokio_unstable` + `tokio_taskdump` cfg，仅限 Linux 的 aarch64/x86/x86_64。`Handle::dump().await` 后遍历 `dump.tasks()` 调 `task.trace()`，可得到 async 调用栈（含 `yield_now` 等叶帧）。官方例子在 Ctrl+C 时 dump 一个故意死锁的程序。— [tokio tests/dump.rs](https://codebrowser.dev/tokio/tokio/tests/dump.rs.html)；[examples/dump.rs](https://codebrowser.dev/tokio/examples/dump.rs.html)。第三方基准站列出了若干相关问题（多线程 dump 不完整、追踪中 future 完成导致死锁等），未核验是否已修复 — [harborframework 任务页](https://hub.harborframework.com/tasks/abundant/tokio-rs__tokio-6036)
  - **Dropwise 启发**：集成。当 settle 超时，也就是疑似 futurelock 或丢唤醒导致 hang 时，自动附上 task dump 作为诊断，把“超时”变成“卡在哪个 await”。工作量 S–M（feature-gated），属低垂果实（UX 收益大），但限 Linux 和 unstable cfg。

- **`tokio-dtrace`**（Oxide，0.1.1，2025-07）：Tokio 的 DTrace probes — [crates.io API](https://crates.io/crates/tokio-dtrace)。Oxide 的 futurelock 事故（omicron#9259）正是靠 DTrace 追函数调用才定位 — [RFD 609](https://rfd.shared.oxide.computer/rfd/0609)
  - **Dropwise 启发**：弱迁移，偏生产诊断，不入测试主路径。

- **tokio-metrics**（0.5.2，2026-08-28）：Runtime 与 task 级指标 — [crates.io API](https://crates.io/crates/tokio-metrics)。
  - **Dropwise 启发**：（未本轮核验）Tokio 的 `RuntimeMetrics` 提供存活任务数一类指标，可用作“取消后无孤儿任务”的廉价 oracle，在场景前后各取一次差值。工作量 S，属低垂果实。

- **tokio-console、Miri、Kani、cargo-mutants**：本轮检索没有找到它们针对取消的专门能力。Kani 的 2026 论文（arXiv 2607.01504）讲 MIR→CBMC 和合约，未提 async — [arXiv 2607.01504](https://arxiv.org/abs/2607.01504)。
  - **Dropwise 启发**：cargo-mutants 有一个有意思的反向用法：用变异体（例如删掉 rollback 语句、把 `reserve`+`send` 改回 `send`）评估 Dropwise oracle 的检出率，可作为工具自评指标。工作量 M，属研究向，不算低垂。

### Inferences

- **生态空白确认**：所有找到的工具要么（a）只测单个 future（Hyperactor、futures-testing），要么（b）要求换运行时（Asupersync），要么（c）是 select! 左侧名单匹配（async-rust-lsp）。**没有工具复用既有集成测试、在 Tokio select!/timeout 处系统性注入取消**。Dropwise 的宏补丁方向是真正的差异化点。
- **低垂果实优先级汇总**（按收益/成本粗排，结合后文各节）：
  1. **内置“官方 cancel-safety 表”模型库**（见问题 4）：把 Tokio 与 tokio-util 文档中逐方法标注的“丢数据 / 丢队位 / 保留部分进度”三类，转成 auto-obligation 或自动标注。S–M。
  2. **Pending 制造器**：`interleave_pending` 风格的 tokio I/O 包装与 tokio-test mock，解决“没有 Pending 就没有取消点”的覆盖问题。S。
  3. **取消后重试差分 oracle**（Hyperactor 的 restartability）。S–M。
  4. **错误触发早取消**（try_join 短路）作为竞争者时序的一种取值。S。
  5. **默认 oracle**：孤儿任务（task leak）、loser drain、quiescence；以及 settle 超时时附 task dump。S–M。
  6. **Obligation 的“转交给 spawn”状态**（guarden / Dropshot 模式），减少误报。S–M。
  7. **futurelock 检测**（见问题 2）：settle 时发现“仍被持有但无人 poll 的 future 持有资源”，判 hang 而非超时。M。

### Gaps

- 未能确认 tokio-console、Miri、Kani 对 async 取消有任何专门支持（检索无结果）；也未找到 loom/shuttle 官方文档关于 Tokio 集成的当前状态。
- crates.io 搜索中 “poll by poll”“drop at every await” 无相关命中；GitHub 代码级搜索没有系统做，可能漏掉未发布到 crates.io 的内部工具（例如各公司内部的 select! 插桩）。
- tokio-test `io::Builder` 和 tokio-metrics 存活任务指标的具体 API 本轮未抓取核验。

## 问题 2：语言与库提案、从业者知识：async drop、poll_cancel、Leak/Forget/Move、结构化并发、futurelock、cancel-safety 文档与博客

### Takeaway
语言层的根治方案（async drop、`poll_cancel`、不可遗忘/不可 drop 类型、`Move`/`Forget`）在 2026-10 仍未稳定，短期不会改变“drop 即取消”的语义，所以 Dropwise 的问题域在未来 1–2 年内依然成立。从业者知识最成体系的是 Oxide 的两份 RFD：RFD 400 讲 cancel safety，RFD 609 讲 futurelock；加上 Rain 在 RustConf 2025 的演讲，它们给出了“cancel safety（局部）vs cancel correctness（全局）”的术语和一份事实上的取消来源清单，可直接作为 Dropwise 的取消来源和 oracle 检查表。

### Cited Findings

**A. 术语与从业者指南**

- **sunshowers（Rain），“Cancelling async Rust”**（2025-10-03 发布，2025-10-24 更新，改写自 RustConf 2025 演讲）。核心定义：cancel safety 是单个 future 的局部性质；cancel correctness 是系统的全局性质，只有三者同时成立才被破坏：存在 cancel-unsafe future、它确实被取消、取消破坏了系统性质（数据丢失、不变式破坏、清理未执行）。列举的风险包括：Tokio Mutex（持锁期间暂时无效的状态，例如 `Option::take()` 后卡在 None）、`mpsc::Sender::send`（丢消息）、`reserve`（丢队位）、`write_all`、select! 循环、`try_join!`（Oxide 实际 bug：一个服务出错导致不再等待其他服务退出）、Dropshot 在 TCP 断开时 drop 请求 future（远端行为驱动的取消）。缓解措施：reserve-then-send、`write_all_buf` + cursor、select 循环中 pin 后用 `&mut fut` 续 poll、用 `tokio::spawn` 跑完 cancel-unsafe 工作（Dropshot 已改为每请求 spawn）。文中**未提任何取消测试工具**。— [sunshowers.io](https://sunshowers.io/posts/cancelling-async-rust/)；演讲资料仓库（含幻灯片与 [视频](https://www.youtube.com/watch?v=zrv5Cy1R7r4)）[sunshowers/cancelling-async-rust](https://github.com/sunshowers/cancelling-async-rust)；LWN 报道（2025-09-24）[Canceling asynchronous Rust](https://lwn.net/Articles/1036924/)
  - **Dropwise 启发**：借鉴。把 Dropwise 的定位写成“**测 cancel correctness，而不只是 cancel safety**”，与 Hyperactor、futures-testing 这类局部测试器区分，这正好对应“三者同时成立”的定义：Dropwise 让“确实被取消”发生，再用不变式和 obligation 检查“破坏系统性质”。工作量约 0（文档定位），但对外沟通价值高。“该演讲未提测试工具”这一点本身也说明生态空白。

- **Oxide RFD 400 “Dealing with cancel safety in async Rust”**（作者 Rain；页面未显示日期）。定义了 cancel-safe、cancel correctness、**mostly cancel-safe**（仅丢公平性/队位）、**cancellation blast radius**、**early cancellation**（try-join 类）。取消来源清单：select! 循环、`tokio::time::timeout`（包裹 `Sender::send` 会丢值）、try-join 系（`tokio::try_join!`、futures `try_join*`、`try_for_each`、`try_collect`）、`JoinHandle::abort`、**runtime shutdown（以任意顺序取消所有任务）**、`SinkExt::send`、Tokio Mutex 跨 await、“read 后再 await 处理”的 recv 型方法（串口控制台案例）。命名建议：cancel-unsafe 方法不要叫 `next()/recv()/reserve()/acquire()`；可以用按值取 `self` 防止在循环中重建；文档加 “Cancel safety” 小节。RFD 本身**不含测试指导或 lint 建议**。— [RFD 400](https://rfd.shared.oxide.computer/rfd/0400)；相关的 [RFD 397](https://rfd.shared.oxide.computer/rfd/397)
  - **Dropwise 启发**：借鉴，属低垂果实。RFD 400 的取消来源清单可以逐条对应 Dropwise 的“取消注入点”覆盖矩阵：select!、timeout、try_join 早取消、abort、runtime shutdown、客户端断连。其中 **runtime shutdown 以任意顺序取消** 和 **abort 在任意 await 处取消** 是 Dropwise 目前列表中较少强调的，可以加一个“在 Pending k 处 abort 整个 task”和“在 Pending k 处 shutdown runtime 后检查持久状态”的计划类型。工作量 S–M。“blast radius”可作为报告中的影响范围描述字段。

- **Oxide RFD 609 “Futurelock”**（作者 David Pacheco；页面无日期，HN 讨论约 2025-10 底）：futurelock 是一种死锁。任务先 poll 过 F2（F2 因而进入了锁或信号量等的等待队列），随后转去 await F1，而 F1 需要 F2 持有或将获得的资源；任务不再 poll F2，于是永远无进展。典型例子是 `tokio::select!` 中用 `&mut future1`（等锁）和 500ms sleep 竞争：sleep 赢后分支体内又 await 同一把锁，公平 Mutex 把锁交给了 future1，但已无人 poll 它，程序挂起。`FuturesOrdered/FuturesUnordered` 流循环中同理；`join_all` 不受影响。真实事故见 omicron#9259：所有用 DB 的请求挂起，容量 1 的 mpsc 空着但发送者阻塞，靠 DTrace 定位。缓解：spawn 成 task 后在 select! 中用 JoinHandle；避免“select! 中 `&mut fut` + 分支体内 await”的组合；RFD 明说 `send_timeout()` 不是解法。Lint 被列为开放问题：警告“select! 分支传 `&mut future`”和“select! 分支体内 `await`”。— [RFD 609](https://rfd.shared.oxide.computer/rfd/0609)；[HN 讨论](https://news.ycombinator.com/item?id=45774086)
  - **Dropwise 启发**：借鉴，是**一个新的 bug 类，且与取消正交**：问题不是 drop 太早，而是“没有 drop、也没有 poll”。Dropwise 的 select! 宏补丁天然处于检测位置。补丁版 select! 能识别分支是 `&mut` 引用（未被 drop），在分支体执行期间，若该被保留的 future 处于 Pending 且持有 Tokio 公平队列中的位置（可通过内置的 Mutex/Semaphore 模型知道），而分支体又在等同一资源，就判为 futurelock 风险。较简单的版本：settle 超时时报告“存在被 select! 保留但 N ms 未被 poll 的 future”。工作量 M，收益高（真实事故类、生态无工具），算“中垂果实”。

- **Yosh Wuyts，“Async Cancellation I”**（2021-11-10，后有更新）：drop 即取消，取消只在 `.await` 发生；嵌套 future 随父 drop 级联取消，但 tokio/async-std 中 drop JoinHandle 只是 detach，smol 则会取消；提出结构化并发三原则（子不比父长寿、取消父即取消子、子错误上传）；用 “halt-safety” 指资源在任何停止方式下都应释放；反对用 `Interrupted` 错误做信号式取消。— [blog.yoshuawuyts.com](https://blog.yoshuawuyts.com/async-cancellation-1/)。async WG 会议笔记中，他把 halt safety 描述为 “Everything is in a consistent state at every await point”，以区别于 cancellation safety，并提过 `AtomicFuture` 标记 — [wg-async hackmd 笔记](https://hackmd.io/TbDc4RWCSkGEketBWMLmfA)
  - **Dropwise 启发**：借鉴术语。“halt safety = 每个 await 点上状态都一致”正是 Dropwise 中“每个 Pending 后 drop 再 settle 后检查不变式”的形式化表述，可以在文档中引用。另外，**JoinHandle drop 即 detach**意味着“取消父 future 后子 task 仍在跑”，这是孤儿任务 oracle 的动机。工作量约 0 到 S。

- **其他实践文章**：biriukov.dev（2026-02）指出 `stream.next().then(async ...)` 若闭包内有 await 则不是 cancel safe，会丢数据 — [biriukov.dev](https://biriukov.dev/posts/async-rust-gocha-tokio-cancelation-select-future-then/)；Google Comprehensive Rust 有取消一节 — [Comprehensive Rust: Cancellation](https://google.github.io/comprehensive-rust/concurrency/async-pitfalls/cancellation.html)；Cybernetist（2024-04）Tokio 任务取消模式 — [cybernetist.com](https://cybernetist.com/2024/04/19/rust-tokio-task-cancellation-patterns/)；Matthias247 早期分析 “Async/Await – The challenges besides syntax – Cancellation”（2019）— [gist](https://gist.github.com/Matthias247/ffc0f189742abf6aa41a226fe07398a8)
  - **Dropwise 启发**：`StreamExt::then` 加内部 await 是 futures 组合子类别的真实陷阱，可加入 Dropwise 的“futures 组合子取消点”示例集。工作量 S，属低垂果实（示例/回归测试）。

**B. 语言/库提案（截至 2026-10 均未稳定）**

- **withoutboats “Asynchronous clean-up”**（2024-02-24）：提出给 `Future` 加 `poll_cancel`（默认立即 Ready 的 no-op），让被取消的 future 能异步清理，并提出 `do … final` 块（final 中可 await）。他认为隐式 async 析构器有隐式 await 点、`Send` 约束、trait object 安全、强制串行等问题，转而支持 Niko 的 “undroppable types + 显式 AsyncDrop 方法”；“unforgettable types”（`Leak` auto trait）是较弱的替代。还强调代码不能依赖取消清理来保证 soundness。— [without.boats](https://without.boats/blog/asynchronous-clean-up/)
- **Eric Holk “A mechanism for async cancellation”**（2023-11-14）：提出 `poll_cancel`，自称等价于 boats 早先的 `poll_drop_ready` RFC — [theincredibleholk.org](https://theincredibleholk.org/blog/2023/11/14/a-mechanism-for-async-cancellation/)；internals 讨论 “Add `poll_cancel` to `Future`” — [internals.rust-lang.org](https://internals.rust-lang.org/t/add-poll-cancel-to-future/16050)。wg-async 笔记认为 `poll_cancel` 未必适合作为 async Drop 的底层机制 — [hackmd](https://hackmd.io/@wg-async/S1XYOiGNa)
- **Async drop**：2025 年的 Rust Project Goals 把 async drop 列为旗舰目标中的实验性原型，官方原话是距离稳定 “significant language design work” — [Project Goals Dec Update（2025-01-23）](https://blog.rust-lang.org/2025/01/23/Project-Goals-Dec-Update/)；2026 年的 “just add async” 路线图聚焦 AFIT/async closures，提到 streams 需要 “guaranteed destructors” — [Rust goals 2026 roadmap](https://rust-lang.github.io/goals/2026/roadmap-just-add-async.html)；async fundamentals 的 async drop 路线 — [async_drop roadmap](https://rust-lang.github.io/async-fundamentals-initiative/roadmap/async_drop.html)
- **`Move`/`Destruct`/`Forget` auto traits**：2026 项目目标 “Immobile types and guaranteed destructors” 拟引入这三个 auto trait，让类型可以拒绝移动或 `mem::forget`，以实现安全的 scoped spawn、async drop、pin-by-default。这是项目目标，不等于语言变更已被接受 — [rust-lang goals 2026: move-trait](https://rust-lang.github.io/goals/2026/move-trait.html)；2025 年的 Pre-RFC “`Forget` Marker Trait” — [internals](https://internals.rust-lang.org/t/pre-rfc-forget-marker-trait/22456)；早期 compiler-team MCP（`Forgettable`/`Leak`）— [compiler-team#727](https://github.com/rust-lang/compiler-team/issues/727)
- **LWN 对三条路线的归纳**：async drop、unforgettable types、undroppable types — [LWN](https://lwn.net/Articles/1036924/)
  - **Dropwise 启发（以上提案合并）**：短期以区分为主，长期有一个维度要留出。(1) 1–2 年内不会稳定，“drop 即同步取消”的问题空间持续存在。(2) 若 `poll_cancel` 或 async drop 落地，取消将变成**多阶段**（请求取消、异步清理 Pending、完成）。Dropwise 的计划空间要预留“在清理阶段的第 j 个 Pending 再 drop”的维度，这在 Trio 的 shield 和 Cats Effect 的 uncancelable 中已有对应（见问题 5）。(3) 现在的 ad-hoc async drop（Drop 里 spawn）意味着 settle 必须等后台清理，与 guarden 条一致。工作量：(1)(2) 只需设计预留，约 0；(3) S。

**C. 结构化并发**

- **moro**（Niko Matsakis 实验）：`async_scope!` 允许 spawn 的任务借用作用域外的栈数据，作用域在子任务完成前不结束；只做并发、不做并行（子任务只在被 poll 时推进）。README 指出：只要你 await 了某个东西，就把控制权交给了调用者，而调用者可以永不再 poll 你，因此在当前模型下无法安全实现引用外部数据的并行 scope。— [moro README](https://github.com/nikomatsakis/moro)
- **futures-concurrency**（Yosh，7.7.1，2026-01-18，下载约 1378 万）：自称 “Structured concurrency operations for async Rust” — [crates.io API](https://crates.io/crates/futures-concurrency)
- **async-scoped**：RFD 400 将其列为带注意事项的替代方案 — [RFD 400](https://rfd.shared.oxide.computer/rfd/0400)；`Scope::scope_and_block` 文档 — [docs.rs](https://docs.rs/async-scoped/latest/async_scoped/struct.Scope.html#method.scope_and_block)
- **tokio-util `CancellationToken`**：`cancelled()`、`cancelled_owned()` 是 cancel safe 的；`run_until_cancelled(fut)` “only cancel safe if `fut` is cancel safe” — tokio-util 源码 `sync/cancellation_token.rs`（本轮浅克隆 tokio 仓库，commit cdd9dae，2026-10-16）；[docs.rs CancellationToken](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html)
  - **Dropwise 启发**：区分并补充取消来源。(1) moro 和 futures-concurrency 的 race、merge 是 select! 之外的取消发起点，宏补丁覆盖不到，只能靠 `ctx.race` 或给这些库打补丁。可以优先补丁 futures-concurrency 的 `Race`，它的下载量大。工作量 M。(2) CancellationToken 的协作式取消不是 drop，Dropwise 可以增加“在 Pending k 时触发 token.cancel()”的计划类型，测试协作式取消路径的正确性（cancel-safe-futures 的 `coop_cancel` 同理）。工作量 S，属低垂果实。

### Inferences

- Oxide 的 RFD 400、RFD 609 和 Rain 的演讲已经成为社区引用的事实标准，Dropwise 的文档与报告术语（cancel safety vs correctness、mostly cancel-safe、blast radius、early cancellation、futurelock）应与之对齐，便于传播。
- futurelock 是“取消的反面”（应取消却未取消、或保留却不 poll），Oxide 明确列出想要的 lint 却没有实现。Dropwise 的 select! 补丁可以在运行时近乎零成本地检测“`&mut` 分支 + 分支体 await”的组合，这是 Dropwise 可以率先占据的点。

### Gaps

- RFD 400 与 RFD 609 页面不显示确切发布日期。RFD 400 提到 Tokio 1.33 已把 `Sender::send` 文档化为 cancel-unsafe，可间接推断其写于 2023 年后，但未核实。
- 未抓取 Yosh 的 “Linearity and Control”（2023-03-23）、“Tasks are the wrong abstraction”（2024-04-27）、“Context Managers: Undroppable Types for Free”（2024-06-22）全文，标题来自检索摘要。
- 未找到 Tokio 团队关于结构化并发（JoinSet 作用域化等）的官方提案或 issue，检索无命中。Cloudflare 关于取消的公开文章本轮未检索到。
- `unsafe_pinned` 与取消的关系本轮未检索。

## 问题 3：实证研究与分析器：async Rust bug 研究、静态分析器、Go 的取消/泄漏研究；可用作内置模型检查表的分类

### Takeaway
截至 2026-10，**没有找到专门研究 async Rust 取消 bug 的同行评审实证研究**；多轮检索只命中 rustc 编译器 bug 研究、通用 Rust 死锁静态检测（锁和条件变量层面，不覆盖 await）以及一篇 2026-08 的 async/await 设计空间论文。最可用的“分类法”来自从业者文献（RFD 400、Rain 的演讲、RFD 609）和 OSDI'22 的跨语言取消研究。Go 侧有较成熟的实证和工具：ASPLOS'19 并发 bug 研究、Uber 的 goleak/LeakProf、`go vet lostcancel`、`testing/synctest`。

### Cited Findings

- **rustc 编译器 bug 研究**（arXiv 2503.23985，2025-03）只在编译器层面谈 async 降级为状态机，不涉及运行时取消 — [arXiv](https://arxiv.org/abs/2503.23985)；**rustc 类型检查器 soundness 研究**（arXiv 2608.28713，2026-08）同样无关 — [arXiv](https://arxiv.org/abs/2608.28713)
  - **Dropwise 启发**：无迁移。列出来只是为了证明“不存在 async 取消实证研究”这一空白。

- **“A Design Space Exploration of Async/Await”**（Gray, Krishnamurthi, Crichton；arXiv 2608.20677，2026-08-21；页面另列 DOI 10.1145/3839519）：把 “How can a task handle being cancelled?” 列为九个设计维度之一 — [arXiv](https://arxiv.org/abs/2608.20677)。一份检索摘要称该文把 Rust（Tokio、smol）归为 “unaware cancellation”、Python 和 Swift 为 “aware cancellation”，但本轮只抓到摘要页，**未能在原文核实该表述**。
  - **Dropwise 启发**：可在文档中引用“Rust 的取消对被取消方不可见（unaware）”这一定位：Dropwise 存在的必要性正是因为被取消代码无法自察。工作量约 0。

- **Rust 静态死锁检测**：Zhang 等 “Static Deadlock Detection for Rust Programs”（arXiv 2401.01114，v2 修订于 2026-01）覆盖 double lock、conflict lock、条件变量死锁，基于指针分析，不面向 future/await — [arXiv](https://arxiv.org/abs/2401.01114)；另有一篇论文指出，在 MIR 中识别并发原语本就困难，async/await 会让它更难 — [Deadlock-free asynchronous message reordering in Rust with MPST](https://arxiv.org/pdf/2112.12693)；“Language-Agnostic Static Deadlock Detection for Futures”（Müller）指出既有 graph-types 算法不可靠（unsound），并提出类型系统方法，年份未核实 — [ACM DL](https://dl.acm.org/doi/pdf/10.1145/3627535.3638487)；TSE 2024 “Understanding and Detecting Real-World Safety Issues in Rust” — [ACM DL](https://dl.acm.org/doi/abs/10.1109/TSE.2024.3380393)
  - **Dropwise 启发**：区分，迁移弱。现有静态分析器（lockbud 一系）不建模 await 和 drop-取消，futurelock 这类 async 死锁目前只能靠动态手段发现，这进一步支持 Dropwise 的运行时路线。工作量约 0。

- **OSDI 2022，Sethi 等 “Cancellation in Systems: An Empirical Study of Task Cancellation Patterns and Failures”**：已在 RELATED.md 中 — [USENIX](https://www.usenix.org/conference/osdi22/presentation/sethi)
  - **Dropwise 启发**：仍是唯一的系统性取消失败研究。其分类与下面汇总的从业者分类可以交叉形成检查表。

- **Go 侧**：
  - Tu、Liu、Song、Zhang，“Understanding Real-World Concurrency Bugs in Go”（ASPLOS'19）：6 个项目（Docker、Kubernetes、gRPC 等）中的 171 个并发 bug，过半与 Go 特有机制相关；数据集公开 — [PSU pure](https://pure.psu.edu/en/publications/understanding-real-world-concurrency-bugs-in-go/)；[数据集](https://github.com/system-pclub/go-concurrency-bugs)；[Morning Paper 解读](https://blog.acolyer.org/2019/05/17/understanding-real-world-concurrency-bugs-in-go/)
  - Uber 单体仓库 goroutine 泄漏研究（arXiv 2312.12002）：提出 **Goleak**（单元测试中检测 partial deadlock / goroutine 泄漏）和 **LeakProf** — [arXiv](https://export.arxiv.org/abs/2312.12002)
  - **`go vet lostcancel`**：检查 `context.WithCancel/WithTimeout/WithDeadline/WithCancelCause` 返回的 cancel 函数是否被调用，否则派生 context 会一直存活到父 context 取消（从 Background 派生则永不释放）— [pkg.go.dev lostcancel](https://pkg.go.dev/golang.org/x/tools/go/analysis/passes/lostcancel)
  - **`testing/synctest`**：Go 1.24 实验（2025-02 博客），Go 1.25 正式 GA（`synctest.Test` + `synctest.Wait`）。bubble 内时间虚拟化，所有 goroutine **durably blocked**（只能被 bubble 内其他 goroutine 解锁）时时钟瞬间前进；`Wait` 等到 bubble 静止；若 durably blocked 且时间无法推进，`Run` 会 panic（死锁检测）。阻塞在外部事件（网络读、mutex）上的 goroutine 不算 durably blocked — [Go blog: synctest](https://go.dev/blog/synctest)；[Go 1.25 release notes](https://go.dev/doc/go1.25)
  - **Dropwise 启发**：
    - **goleak 对应孤儿任务 oracle**：测试结束时断言没有残留任务，直接迁移为 Dropwise 的默认检查。S，属低垂果实。
    - **lostcancel 对应“取消句柄未被调用/未被 drop”**：Rust 中的类比是 `CancellationToken::child_token()` 或 `DropGuard`、`AbortOnDropHandle` 被 `mem::forget`，以及 JoinHandle drop 导致任务 detach。迁移是弱到中，因为 Rust 的 RAII 已解决大部分问题；剩下的是“detach 后无人等待”，仍归入孤儿任务 oracle。
    - **synctest 的 “durably blocked” 语义**是 Dropwise “settle” 的现成精确定义：settle = 所有任务都阻塞在 runtime 内部原语上、且（paused 时间下）无可推进的定时器。还可借鉴两点：一是“阻塞在外部 I/O 不算静止”；二是“静止 + 时间无法推进 = 死锁”，从而把 settle 超时细分为**死锁（futurelock 嫌疑）**和**仍在运行**两种判定。工作量 S–M，属低垂果实（明确语义、减少 flaky）。

**从业者文献汇总的取消缺陷分类（可作 Dropwise 内置模型 / oracle 检查表）**（来源：[RFD 400](https://rfd.shared.oxide.computer/rfd/0400)、[sunshowers](https://sunshowers.io/posts/cancelling-async-rust/)、[RFD 609](https://rfd.shared.oxide.computer/rfd/0609)、[Hyperactor 文档](https://github.com/meta-pytorch/monarch/blob/main/hyperactor/src/testing/cancel_safe.rs)、[aselect](https://github.com/avl/aselect)）：

| 类别 | 典型实例（来源） | Dropwise 对应 oracle / 模型 |
|---|---|---|
| 数据丢失：已消费未交付 | `mpsc::Sender::send` 被 drop（消息丢）、recv 后再 await（RFD 400 串口案例）、`receive_command`（aselect） | 消息守恒 obligation（已有 mpsc Receiver 模型可推广） |
| 部分 I/O 进度丢失 / 协议失步 | `read_exact`、`write_all`；length-prefix 协议被错位（async-rust-lsp 案例 “frame too large”） | 字节守恒 / 帧边界不变式；Pending 制造器 |
| 不变式破坏：暂时无效状态跨 await | Tokio Mutex 内 `Option::take()` 卡在 None（sunshowers） | “mutated-before-await / restored-after” 静态候选加动态确认（与 G0 检测器衔接） |
| 清理未执行 | 需要异步清理却在 Drop 中无法 await（RFD 400 Future directions） | obligation 未解除 |
| 公平性丢失（mostly cancel-safe） | Mutex/RwLock/Semaphore/Notify/`reserve` 丢队位（Tokio 文档） | 通常不应判错，单独标为 “fairness-only” 警告 |
| 早取消 | `try_join!`、`try_join_all`、`try_for_each`（RFD 400、omicron#3707） | 竞争者 = Err 的计划类型 |
| 强制取消 | `JoinHandle::abort`、runtime shutdown（RFD 400） | abort / shutdown 计划类型 |
| 远端驱动取消 | Dropshot 客户端断连 drop handler（sunshowers） | turmoil 断网或 hyper 连接 drop 场景 |
| 保留未 poll（futurelock） | select! `&mut fut` + 分支体 await；FuturesUnordered 循环中 await 他物（RFD 609） | settle 中检测“被持有但未 poll 且占资源的 future” |
| 叶子 future 的 waker 契约 | lost/stale waker、虚假 poll 不容忍（futures-testing） | 可选的虚假 poll / waker swap 注入 |
| 取消后不可重启 | Hyperactor restartability | 取消后重试差分 oracle |

### Inferences

- “无同行评审的 async Rust 取消实证研究”本身就是一个可靠信号：Dropwise 的 22 个真实修复 bug 集合（Qdrant、Materialize 等）在规模上很可能已经是公开资料中最大的 async Rust 取消 bug 集，可作为工具回归基准持续维护（不为写论文，而为防回归）。
- 上表 11 类中，Dropwise 已有或易有的是前 3 类和 obligation；早取消、abort/shutdown、futurelock、重试差分是可以直接加的计划类型或 oracle，都是 S–M 工作量。

### Gaps

- 未能找到 2023–2026 年 ICSE/FSE/ASE/OOPSLA 上专门研究 async Rust bug 的论文。三轮不同措辞检索均无命中，但不能排除存在未被搜索引擎收录的论文（例如中文期刊或 workshop）。
- lockbud、rCanary、Rudra 是否增加了 async 支持，本轮未核验。
- Go 的 context 泄漏是否有独立实证研究，未找到（只找到 goleak/LeakProf 与 ASPLOS'19）。

## 问题 4：哪些 Tokio / futures / tower / hyper API 被文档化为非 cancel-safe？能否构成内置模型 / auto-obligation 库？

### Takeaway
Tokio 的 cancel-safety 文档化非常系统：本轮对 Tokio 主仓库（commit cdd9dae，2026-10-16）源码做了全量提取，`tokio/src` 下共 119 处 `# Cancel safety` 小节，自写脚本粗分为 73 处 “cancel safe”、17 处“丢队位”、27 处“不安全”（其中少数是正则误判，剔除 JoinHandle、Signal::recv、named pipe connect 这 3 处后为 24 处）、2 处“部分进度”。不安全的分三类：**丢数据**（read_exact、全部 `read_u16…read_f64_le` 定长读、read_line、read_to_end/read_to_string、write_all、`Sender::send`、`Barrier::wait`、tokio-util 的 `write_all_vectored`、`Framed/FramedWrite` 的 `SinkExt::send`）；**丢队位**（Mutex/RwLock/Semaphore/Notify 的全部获取方法、`reserve*`）；**保留部分进度但调用方要自行续接**（`read_until`、`write_all_buf`、`flush`）。相比之下，futures-rs 全仓库没有任何 cancel safety 文档，而可行性研究中多数 bug 恰好出在 futures 组合子和自定义包装上。把 Tokio 表转成内置模型是低垂果实，但收益上限有限，真正的空白在 futures 组合子。

### Cited Findings

- **`tokio::select!` 文档中的官方清单**（docs.rs 显示 Tokio 1.53.3）：
  - cancel safe：`mpsc::Receiver::recv`、`UnboundedReceiver::recv`、`broadcast::Receiver::recv`、`watch::Receiver::changed`、`TcpListener::accept`、`UnixListener::accept`、`signal::unix::Signal::recv`、`AsyncReadExt::read`/`read_buf`、`AsyncWriteExt::write`/`write_buf`、`tokio_stream::StreamExt::next`、`futures::stream::StreamExt::next`。
  - 不安全（会丢数据）：`AsyncReadExt::read_exact`、`read_to_end`、`read_to_string`、`AsyncWriteExt::write_all`。
  - 因公平队列而不安全（丢队位）：`Mutex::lock`、`RwLock::read`、`RwLock::write`、`Semaphore::acquire`、`Notify::notified`。
  - `biased;` 让分支按书写顺序 poll；在循环中复用 `&mut sleep` 需要 `Unpin`（`tokio::pin!` 或 `Box::pin`）。
  — [docs.rs tokio::select!](https://docs.rs/tokio/latest/tokio/macro.select.html)

- **源码逐方法提取（本轮浅克隆 tokio-rs/tokio，commit cdd9dae，2026-10-16，自写脚本提取 `# Cancel safety` 小节）**：
  - **不安全（丢数据）**：`AsyncReadExt::read_exact` 以及 `read_u16/u32/u64/u128/i16/…/f32/f64` 及其 `_le` 变体（文档原文为 “not cancel safe… some data may be lost”；`read_u8`、`read_i8` 为 cancel safe，因为只读 1 字节）；`AsyncBufReadExt::read_line`（“data is lost… current implementation replaces `buf` with the empty string”）；`AsyncWriteExt::write_all`（“future calls to `write_all` will start over from the beginning”）；`mpsc::Sender::send`（“the message is dropped and will be lost”，同时 send/reserve 共享公平队列）；`sync::Barrier::wait`（“This method is not cancel safe.”）。
  - **丢队位**：`Mutex::lock/lock_owned`、`RwLock::read/read_owned/write/write_owned`、`Semaphore::acquire/acquire_many/acquire_owned/acquire_many_owned`、`Notify::notified/notified_owned`、`mpsc::Sender::reserve/reserve_owned/reserve_many`。
  - **部分进度保留型（不丢数据，但调用方需正确续接）**：`AsyncBufReadExt::read_until`（已读字节追加到 buf，可再调用续读，但返回的计数会重置）；`AsyncWriteExt::write_all_buf`（buf 已按写出量 advance）；`flush`（标注 cancel safe，但缓冲数据可能已部分 flush）。
  - **cancel safe 且有明确保证**：`JoinSet::join_next(_with_id)`（未移除任何任务）；`&mut JoinHandle`（输出不丢）；`watch::Receiver::changed/wait_for`；`oneshot::Receiver`；`broadcast::recv`；`mpsc::recv/recv_many/closed`；`time::Interval::tick`；`process::Child::wait`；UDP/Unix datagram 的 `send/send_to/recv/recv_from`；TCP/Unix 的 `readable/writable/ready/peek/accept`；`AsyncFd::readable/writable`；`Lines::next_line`；`fs::ReadDir::next_entry`；`task::LocalSet::run_until`（当且仅当内部 future cancel safe）。
  - **tokio-util**：`TaskTracker::wait` 是 cancel safe 的，但在 select! 循环中用作条件时会失去防 ABA 能力；`JoinQueue::join_all` 不安全（“calls `join_next` in a loop”）；`io::write_all_vectored` 不安全；`Framed`/`FramedWrite` 的 `SinkExt::send` 不安全（文档说明被 drop 时消息未发送）；`FramedRead` 经 `StreamExt::next` 是 cancel safe；`DelayQueue` 的 `next` 安全；`JoinMap::join_next` 安全；`CancellationToken::run_until_cancelled` 仅当内部 future 安全时才安全。
  - `tokio::time::timeout` 文档只说 “Cancelling a timeout is done by dropping the future. No additional cleanup”；`io::copy`、`copy_bidirectional`、`BufReader`、`BufWriter` 源码中**没有**任何 cancel safety 小节（grep 无命中）。
  — 源码路径：`tokio/src/io/util/async_read_ext.rs`、`async_buf_read_ext.rs`、`async_write_ext.rs`、`tokio/src/sync/{mpsc/bounded.rs,mutex.rs,rwlock.rs,semaphore.rs,notify.rs,barrier.rs}`、`tokio-util/src/{task/task_tracker.rs,task/join_queue.rs,io/write_all_vectored.rs,codec/framed.rs,codec/framed_write.rs}`，见 [tokio-rs/tokio](https://github.com/tokio-rs/tokio)

- **futures-rs**（浅克隆 commit 706e29b，2026-10-01）：全仓库 grep `cancel safe|cancellation safe|cancel safety` **零命中**，`SinkExt::send`、`try_join_all`、`select!`、`StreamExt::then` 等都没有取消语义文档 — [rust-lang/futures-rs](https://github.com/rust-lang/futures-rs)。RFD 400 则明确把 `SinkExt::send` 列为 inherently cancel-unsafe，把 futures 的 `try_join*`、`try_for_each`、`try_collect` 列为早取消来源 — [RFD 400](https://rfd.shared.oxide.computer/rfd/0400)
- **hyper / tower**：本轮未检索到它们的 cancel safety 文档化条目（见 Gaps）。sunshowers 记录了 Dropshot 曾因客户端断连 drop handler future，后改为每请求 spawn — [sunshowers](https://sunshowers.io/posts/cancelling-async-rust/)

- **Dropwise 启发（本节合并）**：
  1. **把 Tokio 表做成机器可读的 `known_apis.toml`**（三类：data-loss、fairness-only、partial-progress），用途有三。(a) select! 或 timeout 补丁命中时，若分支 future 的类型名或调用链匹配 data-loss 类，在报告中提示“官方文档已标注不安全”，用来解释失败原因，**不**作为判错依据。(b) 对 `Sender::send`、`write_all`、`read_exact` 自动注册 obligation（“取消时消息或字节必须已交付或可恢复”），无需用户写不变式。(c) 对 fairness-only 类默认降级为警告，避免误报。工作量 S–M，属**低垂果实**。注意：由于 Rust 宏无法得知类型，运行时匹配需借助 `std::any::type_name` 或 `#[track_caller]` 位置加源码扫描；最简单是静态扫描 select!/timeout 的分支表达式（类似 async-rust-lsp），但要**以 Tokio 文档为准**，不要沿用 async-rust-lsp 把 `read_buf/write_buf` 列为不安全的错误。
  2. **自动验证用户自家 API 的 `# Cancel safety` 文档声明**：Tokio 的声明句式高度模板化（“If X is used as a branch in select! and another branch completes first, it is guaranteed that no messages were received”）。Dropwise 可以提供 `dropwise::conformance::recv_like(make_pair)` 和 `send_like(...)` 等通用一致性测试：对 recv 类 API 在每个 Pending 处 drop，再断言“没有消息被消费”。这类似 Trio 的 `check_one_way_stream` 一致性套件（见问题 5）。工作量 S–M，属**低垂果实**，也是能为 Tokio 生态库作者直接带来价值的功能。
  3. **futures 组合子是真正的空白**：futures-rs 零文档，而可行性研究中多数 bug 不在 select! 上。与其只补丁 Tokio 宏，不如同时为 `futures::future::{select, try_join_all, join_all}`、`futures::select!`、`StreamExt::{then, buffer_unordered}` 提供 `dropwise::futures` 替身（同名同签名，按 feature 切换），这是覆盖面增长最快的方向。工作量 M。
  4. `Barrier::wait`、`TaskTracker::wait` 的 ABA 问题、`JoinQueue::join_all` 这些小众项可以作为模型库的“长尾”。`io::copy` 和 `BufReader`、`BufWriter` **没有官方说法**，Dropwise 若实测出它们的取消行为，例如 BufReader 被 drop 时内部缓冲随之丢失，可以作为对社区有价值的产出（提 PR 补文档）。

### Inferences

- 内置模型库的收益上限有限：可行性研究中 13/22 是 select! 或 timeout，而它们所包含的叶子 API 中真正属于“Tokio 文档化不安全”的只是一部分；多数 bug 是应用层的“消费后、提交前”窗口（例如 aselect 的 `receive_command`），只能靠用户不变式或通用 oracle（重试差分、消息守恒）发现。因此模型库应定位为“解释器 + 免费 oracle”，而非主要检出手段。

### Gaps

- hyper、tower、h2、reqwest、tonic、sqlx、deadpool 等库的 cancel safety 文档化情况未系统提取，本轮时间有限。建议用同样的 grep 脚本对这些仓库各跑一次，成本 S。
- 自写脚本按关键词归类，个别条目（如 `JoinHandle`、`Signal::recv`、named pipe `connect`）被正则误判为不安全，已人工核对原文更正；仍可能有少量漏判。

## 问题 5：同类生态（Trio/AnyIO、Kotlin、Swift、ZIO、Cats Effect、Haskell）测试了什么而 Rust 没有？

### Takeaway
“感知型取消”（aware cancellation）生态的测试工具都围绕三件事：**取消点的存在性与位置**（Trio 的 `assert_checkpoints`、flake8-async 的 ASYNC100/910/913）、**不可取消区与清理阶段的正确性**（Trio shield、flake8-async ASYNC102/120、Cats Effect `uncancelable`/`poll` 及其 laws、dejafu 对 `mask`/`throwTo` 的系统化探索），以及**确定性运行时下的静止和死锁判定**（Trio `MockClock` autojump 与 `wait_all_tasks_blocked`、Cats Effect `TestControl.isDeadlocked`、Go synctest）。Rust 生态目前三者都缺，Dropwise 可以低成本借鉴第一项和第三项。第二项要等 async drop 或 `poll_cancel` 落地后才完全适用。

### Cited Findings

- **Trio `trio.testing`**：`assert_checkpoints()` 断言代码块 “either exits with an exception or executes at least one checkpoint”，`assert_no_checkpoints()` 断言块内没有 checkpoint；`MockClock` 的 `autojump_threshold` 在所有任务阻塞超过阈值时把时间跳到下一个超时；`wait_all_tasks_blocked(cushion)`；`Sequencer` 强制跨任务的执行顺序；`lockstep_stream_pair()`（无缓冲、每次 `send_all` 都阻塞到被接收，“useful for finding edge cases”）；`check_one_way_stream / check_two_way_stream / check_half_closeable_stream` 是自定义 stream 实现的通用一致性测试，可传入 “clogged” stream 以测试阻塞行为 — [Trio testing 文档](https://trio.readthedocs.io/en/stable/reference-testing.html)
  - **Dropwise 启发**：
    - **`assert_cancellation_points(at_least = 1)`**：断言 target 在场景中至少到达一次 Pending，否则计划空间为空，测试“通过”也没有意义。这直接对应 G0 报告中的 early-observation 和 Pending 不足问题，可以把它变成**默认的覆盖率断言或警告**。工作量 S，属**低垂果实**。
    - **lockstep stream 对应 Tokio 的 `duplex(1)` 或零缓冲 mock**：最大化 Pending 次数，从而最大化取消点。与 futures-test 的 `interleave_pending` 同类。S。
    - **一致性套件**：见问题 4 的第 2 点，对用户实现的 Stream、AsyncRead、channel 跑通用取消一致性测试。S–M。

- **flake8-async**（Trio/AnyIO/asyncio 的 linter，ruff 也实现了部分规则）：ASYNC100 是 timeout 上下文内无 checkpoint，因而超时永远不会触发；ASYNC102 是 `finally`、取消捕获 `except` 或 `__aexit__` 中的 await 必须放进带超时的 shielded cancel scope，否则新的取消会替换正在处理的取消；ASYNC103/104 是捕获取消后未重新抛出；ASYNC119 是异步生成器在上下文管理器里 yield，清理可能被推迟到无法 await 的时候；ASYNC120 是 except 中的 checkpoint 可能把当前异常替换为 Cancelled；ASYNC121 是 nursery 或 task group 内的 return/break/continue；ASYNC910/911/912/913 是函数、生成器、取消域或无限循环“无保证 checkpoint”（913 指可能死锁或无法取消）— [flake8-async rules](https://flake8-async.readthedocs.io/en/latest/rules.html)
  - **Dropwise 启发**：大多是**弱迁移**，因为 Rust 没有异常式取消，也没有 finally 中的 await。可以迁移的有两条。(1) **ASYNC100 的 Rust 对应**：`tokio::time::timeout(d, fut)` 里的 `fut` 从不返回 Pending（例如同步重计算，或 Tokio coop 预算被 `unconstrained` 关闭），超时永远不会触发。Dropwise 在 timeout 补丁里观测到“目标从未 Pending”时可以顺手报告。工作量 S，属低垂果实，复用 `assert_cancellation_points` 的数据。(2) **ASYNC913 的对应**是 Tokio coop 预算问题，即 `loop {}` 中只 poll 永远 Ready 的东西，与取消关系弱。

- **Cats Effect**：`TestControl` 是单线程 mock runtime，在每个 yield 点**随机选择**待运行的 fiber（可设 seed），时间虚拟化；`tick`、`tickAll`、`nextInterval`、`advanceAndTick` 用于推进；`isDeadlocked` 区分“全部 fiber 挂起”与“在睡眠”；`executeEmbed` 中，死锁表现为 `NonTerminationException`，取消表现为 `CancellationException`；`IO.cede.foreverM` 这类不停 yield 的程序会让 `tick` 不返回，`foreverM` 这类不死锁的无限循环无法检测 — [Cats Effect TestControl](https://typelevel.org/cats-effect/docs/core/test-runtime)。`MonadCancel` 提供 `uncancelable`（内含 `poll` 恢复可取消性，可以嵌套，错序或重复的 poll 为 no-op）、`onCancel`、`bracketFull`，还支持用 `IO.canceled` 自我取消 — [herding cats: MonadCancel](https://eed3si9n.com/herding-cats/MonadCancel.html)；[Baeldung](https://www.baeldung.com/scala/cats-effect-cancellation)
  - **Dropwise 启发**：(1) **settle 判定三态**：完成、死锁（所有任务阻塞在 runtime 原语且无定时器可推进）、仍在运行（busy 或外部 I/O）。借鉴 `isDeadlocked` 加 Go synctest 的语义，把当前“settle 超时”细分，死锁态直接指向 futurelock 或丢唤醒类 bug。工作量 S–M，属低垂果实。(2) **`IO.canceled` 自取消**对应 Rust 中“future 内部主动返回导致外层 select 结束”，迁移弱。(3) `uncancelable`/`poll` 的 laws 测试对应“不可取消区”，Rust 中最接近的是 spawn 后 await JoinHandle（任务不随 handle drop 而取消），Dropwise 可以加一个 oracle：被 spawn 保护的临界区在取消后仍完整执行。这与 guarden 的转交语义一致。工作量 S。

- **Haskell dejafu**：基于 `concurrency` 包的系统化、确定性并发测试库，**建模异步异常与 masking 状态**。changelog 显示：0.7.3.0 修复了“通过异常逃出 mask 后 masking 状态恢复错误”（#118）；1.0.0.1 修复“unmask 中嵌套的 mask 在系统化测试中有时不被抢占”；2.0.0.0 起向当前线程抛出的异步异常即使在 masked 时也会中断它（#267）；2.4.0.0 的 `Throw/ThrowTo` trace 动作带上了结果 masking 状态（#324）— [Stackage dejafu](https://www.stackage.org/package/dejafu)；[Hackage changelog](https://hackage.haskell.org/package/dejafu-1.0.0.1/changelog)
  - **Dropwise 启发**：dejafu 说明“取消注入（throwTo）× 调度交错 × 掩码区”可以系统化枚举，而且掩码区的语义 bug（包括测试工具自身的 bug）很常见。对 Dropwise 的直接迁移弱，因为 Rust 没有异步异常或 mask。间接启发是：**测试工具本身要有对取消语义的自测**，比如 Dropwise 的 G0 配对对照。工作量约 0。

- **ZIO**：`TestAspect.nonFlaky` 重复执行测试直到首次失败；`timeout` aspect 借助 ZIO 的中断机制取消超时的测试；fiber 有 `InterruptStatus`，WindDown 模式下即使被中断也继续运行；ZIO 1.x 起子 fiber 随父 fiber 中断（daemon 除外）；有一个 PR 讨论了在 uninterruptible 区内 join 子 fiber 会导致中断一直等到子 fiber 完成 — [ZIO timeout aspect](https://zio.dev/reference/test/aspects/timing-out-tests/)；[TestAspect API](https://javadoc.io/static/dev.zio/zio-test_2.13/2.0.0-M4/zio/test/TestAspect$.html)；[InterruptStatus](https://javadoc.io/static/dev.zio/zio_3/2.0.14/zio/InterruptStatus.html)；[zio PR #3125](https://github.com/zio/zio/pull/3125)；[RC17 发布说明](https://newreleases.io/project/github/zio/zio/release/v1.0.0-RC17)
  - **Dropwise 启发**：本轮**没有找到** ZIO 内置的“在每个 yield 点注入中断”的测试切面，只有 nonFlaky 重复加 timeout。这说明即便在“感知型取消”生态，**系统化取消点枚举测试也不是标配**，Dropwise 的计划枚举仍然是差异化点。“uninterruptible 区内 join 导致中断挂起”这个 bug 模式对应 Rust 中“取消父 future 时在 Drop 中 block_on 或同步等待子任务”。工作量约 0（认知层面）。

- **Go `testing/synctest`、`lostcancel`、goleak**：见问题 3。

### Inferences

- 同类生态共同强调**取消点可见性**，Rust 恰恰是 unaware cancellation，被取消方看不到取消点。Dropwise 的 `ctx.target` 和宏补丁实际上是在给 Rust 补上“取消点可观测性”。“到达了哪些 Pending、在哪些点取消过、哪些点从未到达”的覆盖报告，可能是比 bug 检出更容易被用户接受的第一价值。
- 只有在“感知型”生态中才可迁移的部分（shield、finally 中 await、取消异常吞没），要等 Rust 的 async drop 或 `poll_cancel` 落地后才有意义。目前可以只在设计上预留“清理阶段的取消计划”维度，不必投入实现。

### Gaps

- **Kotlin coroutines**（`kotlinx-coroutines-test` 的 `runTest`、`TestScope`、`advanceUntilIdle`、`NonCancellable`）和 **Swift concurrency**（`withTaskCancellationHandler`、Point-Free `swift-concurrency-extras` 的确定性执行器）本轮**未检索核验**。它们是否提供“在每个挂起点注入取消”的工具，留作后续检索。
- **AnyIO** 自身的测试插件、Trio 是否有官方或社区的“checkpoint 处注入 Cancelled”的模糊工具（如 hypothesis-trio），本轮未找到也未专门检索。
- **Effect-TS** 的 interruption 测试工具（TestClock 等）本轮未检索。
- Cats Effect 的 `GenSpawn`/`MonadCancel` laws（discipline 规则集）的具体条目未抓取，只有间接资料。
