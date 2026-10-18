# 故障注入测试与异步中断/取消语义史：对 Dropwise 可迁移的机制（1980s–2026）

> 记号：【旧】= 2010 年以前的经典工作；【中】= 2010–2019；【近】= 2020–2026。"Dropwise 启发"给出：可借鉴机制 / 粗略工作量 / 是否低垂果实（LHF）。工作量估计以 Dropwise 当前代码结构为参照（`src/explore.rs`、`src/obligation.rs`、`src/models.rs`；已有：baseline run、`Settle::tokio_time`/`real_time`/watchdog、`unrealized` 计划、`max_cancellations` 多目标、`Race::{Immediate,Reschedule,After,AfterWake}`、mpsc Receiver 模型、`ctx.after_settle`）。凡标注"未本次核验"的 URL 来自记忆，引用前应再核对。

## Q1. C++ 异常安全测试与"在第 N 个点失败"家族：strong guarantee 检查器能否直接作为取消的自动 oracle？

### Takeaway
"每个可失败点都注入一次失败、重跑、检查保证"的方法自 2000 年 Abrahams 起已成熟（STLport/Boost、SQLite、Linux `fail-nth`、LFI、FIFUZZ），其中 Abrahams 的 **StrongCheck（失败前复制状态，失败后断言状态未变）** 和 SQLite 的 **"要么完成要么完全回滚"** 是可以几乎原样移植到 Dropwise 的自动 oracle——但对取消需放宽为 **"settle 后状态 ∈ {未开始, 已完成}"（两点集合），并允许用户逐操作声明 basic/strong 级别**，因为很多合法的 Rust 取消（如已消费的字节）只满足 basic。

### Cited Findings
- 【旧 2000】Abrahams 定义三级保证：basic = "the invariants of the component are preserved, and no resources are leaked"；strong = 操作要么成功完成要么抛异常，且程序状态与操作开始前完全相同；no-throw = "the operation will not throw an exception"。出处：D. Abrahams, "Exception-Safety in Generic Components", in *Generic Programming* (Jazayeri, Loos, Musser eds.), Springer LNCS, 2000 — [Boost 镜像全文](https://cs.brown.edu/people/jwicks/boost/more/generic_exception_safety.html)；[出版信息](https://unpaywall.org/10.1007%2F3-540-39953-4_6)
  - **Dropwise 启发**：这三级正好对应取消的三类契约：basic ≈ Dropwise 现有的"不变量 + Obligation 不泄漏"；strong ≈ "被取消的操作没有可观察效果"（Tokio 文档里的 "cancel safe" 本质上是单操作 strong guarantee）；no-throw ≈ "此操作不可被取消/必须 spawn 出去"。建议在 API 层显式引入 `Guarantee::{Basic, Strong, Uncancellable}` 让用户逐 target 声明。工作量小（类型 + 报告字段）。LHF：是。
- 【旧 2000】Abrahams 的自动测试流程：在每个客户端可能抛异常的操作开头和被测泛型操作的每个可能抛点（含被插桩的全局 `operator new`）调用 `ThisCanThrow`，它递减一个计数器、到 0 时抛异常；外层循环以递增的初值重跑操作，从而"在执行路径上每个可能的失败点都抛一次"。`StrongCheck` 在每次尝试前复制值、在复制品上运行操作，捕获异常时断言复制品仍等于原值；每次尝试后（无论是否抛出）调用 `CheckInvariant`；通过插桩全局 new/delete 检测泄漏 — [Abrahams 2000 全文](https://cs.brown.edu/people/jwicks/boost/more/generic_exception_safety.html)
  - **Dropwise 启发**：Dropwise 的"按 Pending 序号枚举计划"就是 `ThisCanThrow` 计数器的同构物（已实现）。**缺的是 StrongCheck**：新增 `ctx.target_strong(fut, || snapshot())`——在 target 首次 poll 前取用户提供的抽象状态快照（`PartialEq + Debug`），baseline 中再取一次完成后的快照；取消 + settle 后断言 `state ∈ {pre, post}`。约 150–300 行。LHF：**是，最高优先级之一**；它把"state taken before await, never restored"这类 Dropwise 已观察到的主流 bug 从"用户需手写不变量"变成"用户只需给出快照函数"。
- 【旧 2000】Abrahams 指出该技术要求被测操作是 exception-neutral：如果操作捕获异常后尝试恢复并继续，计数器会变负，后续失败点就测不到了 — [Abrahams 2000](https://cs.brown.edu/people/jwicks/boost/more/generic_exception_safety.html)
  - **Dropwise 启发**：Rust 中的同构现象是：程序在 `select!` 循环里**重建**被取消的 future（重试），使后续 Pending 的序号语义漂移。Dropwise 已有 `Kept`/`Unrealized` 结果区分，可再加一个诊断："计划 k 的取消发生后，同一 target 标签被重新创建了 n 次"，帮助用户理解重试路径是否被覆盖。工作量小。LHF：中等价值。
- 【旧 2005–2010】Boost.Test 曾提供 `itest::exception_safety`（Gennadiy Rozental 2005 年宣布），"在不同点让代码失败以覆盖所有可能执行路径"，Test Tool 失败即视为不变量被破坏并结束该路径；邮件中说明该方案"first used by Dave A. for STLPort testing"；2010 年用户反映该功能**没有文档页**，并希望知道"异常是在哪里抛出的" — [Boost 邮件 2005](https://listarchives.boost.org/Archives/boost/2005/12/98095.php)；[exception_safety.ipp](https://live.boost.org/doc/libs/1_51_0/boost/test/impl/exception_safety.ipp)
  - **Dropwise 启发**：教训而非机制：该工具默默消亡的主要用户诉求是"告诉我失败注入在哪个位置"。Dropwise 的报告应对每个违例打印注入点的**源码位置**（`#[track_caller]` 捕获 `ctx.target/race` 调用处 + Pending 序号 + 可选 `std::backtrace::Backtrace::capture()` 于 target 的 poll 内）。工作量小。LHF：是。
- 【近 rust std】Rust 标准库自身用 `CrashTestDummy` 测试集合的 panic 安全：实例记录 clone/drop/query 事件，并可配置在选定点 panic — [rust-lang/rust crash_test.rs 镜像](https://cdn05042023.gitlink.org.cn/dnrops/rust/src/branch/master/library/alloctests/testing/crash_test.rs)；[googlesource](https://rust.googlesource.com/rust/+/3fdb443e4e5ce8f04b818754950ff28537d50d93/library/alloc/src/testing)
  - **Dropwise 启发**：说明 Rust 社区已有"异常注入 + drop 计数"的 panic-safety 传统，而 Dropwise 的 `Obligation`（"必须恰好被处理一次"）就是 async 版本的 drop 计数器——这是向 Rust 用户解释 Dropwise 的最好类比（"cancel-safety 之于 async = panic-safety 之于集合"）。可选机制：在 target 内部注入 panic（而非取消）复用同一计划枚举，顺带测 unwind safety。工作量小–中。LHF：叙事上是；功能上中等。
- 【旧–近 SQLite】SQLite OOM 测试：替换 malloc，"在若干次分配后失败"，两种模式分别跑——**失败一次后恢复** 和 **首次失败后持续失败**；循环从第 1 次分配开始，每次推进一位，"直到整个操作在没有遇到模拟 OOM 的情况下完成"为止；I/O 错误测试同理（定制 VFS），之后关闭故障并运行 `PRAGMA integrity_check` 确认无损坏；测试框架**每次运行都自动报告资源泄漏** — [SQLite: How SQLite Is Tested](https://sqlite.org/testing.html)
  - **Dropwise 启发**：(1) "循环至操作不再命中注入点即终止"= Dropwise 的 frontier 发现，已有。(2) **fail-once vs fail-continuously 两种模式是 Dropwise 缺的维度**：当前计划是"在第 k 个 Pending 取消一次"；"持续模式"对应"程序重试/重建 future 后，在**每一次**重试的第 k 个 Pending 都再取消"（模拟持续超时/持续抢占），专门暴露"每次取消泄漏一点点"（计数器只增不减、重复 requeue、重复 take）。实现：Plan 增加 `Mode::Once | Mode::Persistent { max }`，按 target 标签匹配。约 100–200 行。LHF：**是**。(3) "每次运行都跑泄漏检查 + 结构完整性检查"→ 见 Q4 的 task-leak oracle。
- 【旧–近 SQLite】TH3 崩溃测试：内存 VFS 在若干次 I/O 后对整个文件系统做快照，循环推进快照点直到操作完成而不命中；之后回退到快照并引入随机损坏，检查"事务要么完整完成要么完全回滚"，**每个快照用不同随机损坏重复测几次** — [SQLite testing](https://sqlite.org/testing.html)
  - **Dropwise 启发**：(a) "完成或完全回滚"正是上面建议的 `{pre, post}` 两点 oracle 的权威先例。(b) "同一注入点重复多次、每次扰动不同"→ Dropwise 每个 (target, Pending k) 可在多种 `Race` 时序下重复（部分已有），可再加"settle 期间的调度扰动种子"。工作量小。LHF：是（a）；中（b）。
- 【近 Linux】Linux 内核故障注入框架：failslab、fail_page_alloc、fail_usercopy、fail_futex、fail_make_request、fail_function（对 `ALLOW_ERROR_INJECTION()` 标记函数强制返回错误）等；属性 probability/interval/times/space/verbose(2=打印调用栈)/task-filter/**require-start/require-end（调用栈中必须有某个调用者落在此地址区间）**/reject-start/reject-end/stacktrace-depth。`/proc/<pid>/fail-nth`：写入 N 让该任务第 N 次调用失败；**读回 0 表示已注入，正数表示尚未注入**；官方示例循环写入 i=1,2,3…调用 `socketpair()`，读回非零即停，从而得知该调用恰有 15 个故障点 — [Linux kernel docs: Fault injection capabilities infrastructure](https://docs.kernel.org/fault-injection/fault-injection.html)
  - **Dropwise 启发**：(1) "读回判断是否真的注入"= Dropwise 的 `unrealized` 概念，已有，说明设计方向正确。(2) **require/reject 调用栈过滤** 是可直接借的：允许用户按模块/crate 过滤计划（如只在 `my_crate::` 帧内的 Pending 取消，排除 tokio 内部或测试脚手架），或按 target 标签前缀过滤。工作量小（标签过滤）到中（真栈过滤需 backtrace）。LHF：标签过滤是。(3) `verbose=2` 打印注入时的调用栈 → 见上"报告注入位置"。
- 【旧 2009 / 中 2011】LFI（Marinescu & Candea, DSN 2009）：静态分析库二进制得到其"故障画像"（可能返回哪些错误），在**共享库与应用的边界**注入；后续 TOCS 2011 "Efficient Testing of Recovery Code Using Fault Injection" 报告恢复代码覆盖率由"几乎为零"升至 60% 且无需新测试 — [EPFL LFI 项目页](https://dslab.epfl.ch/research/lfi)；[LFI DSN'09 PDF](https://infoscience.epfl.ch/record/139395/files/lfi.pdf?version=1)
  - **Dropwise 启发**：**"库边界自动注入"是 Dropwise 最大的易用性杠杆**：目前用户需手工 `ctx.target/race` 标记。可提供一个 feature-gated 的薄封装层（`dropwise::tokio_shim::{mpsc, oneshot, Semaphore, Mutex::lock, TcpStream::read, sleep}`），其每个 await 自动注册为 target（标签 = 调用点 `Location`）。用户只需把 `use tokio::sync` 换成 shim。工作量中（每个 API 数十行，挑 10–15 个高频 API）。LHF：**是，价值高**。
- 【近 2020】FIFUZZ（Jiang, Bai, Lu, Hu, USENIX Security 2020）：**上下文敏感**的软件故障注入——同一错误点在不同调用上下文中分别注入，并把注入序列与程序输入一起变异；在 9 个 C 程序上报告 317 个告警、50 个唯一 bug、32 个已确认 — [USENIX 页](https://www.usenix.org/conference/usenixsecurity20/presentation/jiang)；[PDF](https://www-users.cse.umn.edu/~kjlu/papers/fifuzz.pdf)
  - **Dropwise 启发**：Dropwise 按"运行中第 k 个 Pending"编号，天然就是执行路径敏感的（类似 fail-nth），已经比"按源码位置"更细。可借的是**反方向**：用 (调用点 Location, 调用上下文哈希) 给计划聚类去重，报告"同一源码点在 37 个上下文中均违例 → 1 个根因"。工作量小。LHF：报告聚类是。
- 【中 2011】FATE/DESTINI（Gunawi et al., NSDI 2011）：引入 **failure ID** 抽象来系统化注入（多）故障；在 HDFS、ZooKeeper、Cassandra 上探索 4 万+ 失败场景（约 80 小时），写 74 条规约，发现 16 个新 bug、复现 51 个旧 bug；剪枝策略带来一个数量级加速且发现同样数量的 bug；DESTINI 用 Datalog 写恢复规约，每条约 5 行 — [UW 页面](https://research.cs.wisc.edu/adsl/Publications/fate-destini-nsdi11.html)；[NSDI'11 PDF](https://usenix.org/legacy/events/nsdi11/tech/full_papers/Gunawi.pdf)；[技术报告](https://www2.eecs.berkeley.edu/Pubs/TechRpts/2010/EECS-2010-127.html)；[slides](https://static.usenix.org/events/nsdi11/tech/slides/gunawi.pdf)
  - **Dropwise 启发**：(1) 多故障组合 + failure-ID 剪枝 → Dropwise 的 `max_cancellations>1` 组合爆炸时，可借 FATE 的剪枝：若两个计划在注入点的 failure ID（target 标签 + 调用点 + 已取消前缀）等价，则只跑一个。(2) DESTINI 式"恢复规约"→ Dropwise 的 `after_settle` 已是命令式规约；可提供小型声明式断言库（"每个 Obligation 标签 X 的结局 ∈ {delivered, requeued}"、"计数器 settle 后回到 baseline"）。工作量中。LHF：(1) 中；(2) 小而有用。
- 【近 2024】Legolas（Wu, Pan, Huang, NSDI 2024）：静态分析放置故障钩子，并**从代码推断抽象状态**，测试算法据抽象状态决定"何处、何时"注入；在 6 个被广泛测试的分布式系统最新版中发现 20 个导致部分服务失效的新 bug — [USENIX 页](https://usenix.org/conference/nsdi24/presentation/wu-haoze)；[PDF](https://www.usenix.org/system/files/nsdi24-wu-haoze.pdf)
  - **Dropwise 启发**：**抽象状态引导的计划剪枝/排序** 是一个低成本高回报的组合：若用户提供了 strong 快照函数，则在每个 Pending 边界顺便记录快照哈希；**相邻 Pending 的抽象状态相同 → 取消效果相同，只测一个**；状态"刚离开 pre 还没到 post"（take 后未 restore）的窗口优先测。这直接瞄准 Dropwise 的主 bug 类。工作量中（200–400 行）。LHF：是，若先做了 StrongCheck 则几乎免费。

### Inferences
- **strong guarantee 检查器可以迁移，但不是"直接"迁移**：C++ 异常会把控制流交还给调用者，"失败后状态"在 catch 时即可比较；Rust 取消的清理分散在 Drop、被 spawn 的后台任务、以及调用者后续逻辑中，所以比较点必须放在 **settle 之后**（Dropwise 已发现立即检查会误报，与此一致）；且合法结果是两点集合 `{pre, post}`（因为 Pending 后的操作可能已在别处提交完成，例如 `AfterWake` 计划下事件已到达），而非 C++ 的单点 `pre`。
- **对很多 IO 操作只能要求 basic**（如 `read` 已消费部分字节、`AsyncWrite` 已写出部分数据）。因此应让用户按 target 声明保证级别，而不是全局要求 strong；否则会产生大量"正确但非 strong"的误报，重演早期"立即检查误报"的问题。
- 从 Abrahams → SQLite → fail-nth → LFI 的演进看，**工具能存活的关键是"自动注入点 + 自动 oracle + 精确报告位置"三件套**；Dropwise 目前注入点枚举强，oracle（只有用户不变量 + Obligation）和自动标记（需手工 target）是短板。

### Gaps
- 未找到 STLport 原始异常安全测试套件的源码存档以核对其实现细节（只在 Boost 邮件中被提及）。
- 未核验 AFEX（Banabic & Candea，常被引为 EuroSys 2012 "Fast black-box testing of system recovery code"）的发表信息；检索只找到 2008 EPFL 技术报告。其"按影响度量引导的故障空间搜索"思想可用于计划排序，但需核实后再引用。
- CrashTuner（Lu et al., SOSP 2019，"meta-info variable" 访问点作为崩溃注入点）只核实了标题作者，未读到方法细节；其思想（只在关键元信息变量被读写处注入）与 Legolas 类似，可作为"只在 take/restore 窗口内取消"的先例，但需读原文确认。

## Q2. 崩溃一致性测试（ALICE/BOB、CrashMonkey/ACE、Chipmunk、eXplode/FiSC）：cancel = future 崩溃、settle = 恢复，哪些界定启发式与 oracle 可迁移？

### Takeaway
崩溃一致性领域的两个核心经验可直接迁移：(1) **小界限穷举（bounded exhaustive）**——绝大多数真实 bug 在极短的操作序列（≤3 个操作）中复现；(2) **"恢复后状态 ∈ 合法状态集合"的 oracle**（原子性：旧或新）。另一个重要教训来自 Chipmunk：只在"持久化点"（类比：被标记 future 的 Pending 边界）注入会漏掉"操作内部"的崩溃状态——这对应 Dropwise "未标记的内部 future 不被枚举"的已知局限。

### Cited Findings
- 【中 2014】ALICE/BOB（Pillai et al., OSDI 2014 "All File Systems Are Not Created Equal"）：BOB 实测 6 个 Linux 文件系统的持久化属性差异很大；ALICE 分析应用的"更新协议"，找出依赖特定持久化属性才正确的代码，在 11 个广泛使用的系统中找到 60 个崩溃漏洞 — [USENIX 页](https://www.usenix.org/node/186195)；[PDF](https://cs.uwaterloo.ca/~alkiswan/papers/alice-osdi14.pdf)；Dgraph 用 ALICE 在 Badger 中找到 2 个漏洞 — [Dgraph 博客讨论](https://discuss.dgraph.io/t/making-badger-crash-resilient-with-alice-dgraph-blog/1685)
  - **Dropwise 启发**：ALICE 的贡献在于把违例**归因到被违反的假设**（原子性 vs 顺序）。Dropwise 报告可对每个违例自动分类："跨 await 原子性假设被违反"（state ∉ {pre,post}）、"Obligation 未结清"（责任丢失）、"效果在调用者认为已取消后仍发生"（顺序/报告不一致，见 Q4 Sethi 的 incorrect reporting）。工作量小（分类逻辑），需先有 strong oracle。LHF：中。
- 【中 2018】CrashMonkey + ACE（Mohan et al., OSDI 2018，"Finding Crash-Consistency Bugs with Bounded Black-Box Crash Testing"，B3）：限定工作负载的操作数或允许的操作集，然后**在界内穷举**生成；动机研究发现绝大多数已报告的 Linux 文件系统崩溃 bug 在新文件系统上用 ≤3 个操作即可复现，且都涉及 fsync 类调用后的崩溃；复现了过去 5 年 26 个已报告 bug 中的 24 个，并在成熟文件系统中发现 10 个新 bug（7 个可追溯到 2014）；作者承认 B3 不保证找全，且界限无法暴露需要很多操作或耗尽资源的 bug — [USENIX 页](https://usenix.org/conference/osdi18/presentation/mohan)；[PDF](https://www.usenix.net/system/files/osdi18-mohan.pdf)；[arXiv](https://arxiv.org/pdf/1810.02904)
  - **Dropwise 启发**：(1) **"小范围假设"的实证支撑**：Dropwise 默认 `max_cancellations` 设为 1–2、场景内操作数小即可覆盖大部分 bug，应在文档中明说并以此作为默认值依据。(2) **ACE 式场景生成器**：给定一组操作（如 `send/recv/ack/requeue/timeout`）和 ≤3 的长度界，自动生成场景序列，每个序列再跑 Dropwise 计划——即"B3 × 取消计划"的二维穷举。工作量中（需用户提供操作字母表，类似 proptest 的 state machine）。LHF：(1) 是；(2) 中。
- 【近 2023】Chipmunk（LeBlanc et al., EuroSys 2023，Best Paper）：记录对持久内存的更新并重放生成可能的崩溃状态镜像，在 5 个 PM 文件系统中发现 23 个新 bug（多数已确认修复），后果包括无法挂载、rename 原子性被破坏；它建立在 CrashMonkey 之上 — [PDF](https://www.cs.utexas.edu/~isil/chipmunk.pdf)；[NSF PAR](https://par.nsf.gov/biblio/10469308-chipmunk-investigating-crash-consistency-persistent-memory-file-systems)
  - **Dropwise 启发**：Chipmunk 的关键点（就我所知并在其论文中强调）是在**系统调用内部**的中间持久化状态也注入崩溃，而不只是调用边界。类比：Dropwise 只在被标记 future 的 Pending 处取消，未标记的内部子 future 不枚举（README 已承认）。可借"记录-重放"思路：用一个自定义 Waker/poll 包装统计 target 内部**每次 poll 返回 Pending 的深层位置**（通过 task-local 计数 + `tokio::task::yield_now` 以外的 Pending 来源），但真正"在内层子 future 处丢弃"在 Rust 中等价于丢弃最外层 target（drop 是自外向内的），所以**Dropwise 在外层第 k 个 Pending 处丢弃已经覆盖了内部所有挂起点**——差异仅在于"哪个内层 await 恰好挂起"未被报告。结论：迁移价值主要在**报告**（告诉用户被丢弃时卡在哪个内层 await），而非新增注入点。工作量中（需 async backtrace，Tokio 的 task dump 为 unstable）。LHF：否。
- 【旧 2006】EXPLODE（Yang, Sar, Engler, OSDI 2006）：借鉴模型检查思想但保持轻量，运行用户编写的（可系统特定的）检查器把存储系统推入边界情况（含崩溃恢复失败），无需源码；在版本控制系统、Berkeley DB、NFS、文件系统、RAID、VMware GSX 中共发现 36 个 bug — [USENIX 页](https://static.usenix.org/event/osdi06/tech/yang_junfeng.html)；[PDF](https://web.stanford.edu/%7Eengler/explode-osdi06.pdf)
  - **Dropwise 启发**：EXPLODE 的"用户检查器 + 系统化选择点"架构与 Dropwise 的 `ctx.target` + `after_settle` 同构（已有）。可借的是"**检查器库**"思路：提供可复用的通用检查器（计数器回到 baseline、队列长度守恒、无重复投递、锁/信号量许可全部归还），用户组合即可。工作量小–中。LHF：是（许可归还/计数守恒两类最常用）。
- 【旧 2003】Crash-only software（Candea & Fox, HotOS IX 2003）：主张"停止的唯一方式是崩溃，启动的唯一方式是恢复"，使恢复路径成为常走路径 — [USENIX HotOS'03](https://www.usenix.org/legacy/events/hotos03/tech/full_papers/candea/candea.pdf)（URL 未本次核验）
  - **Dropwise 启发**：概念映射："cancel-only" 设计——如果一个异步组件的正常结束路径与取消路径共用同一清理逻辑（如 Drop guard 负责提交/回滚），则取消 bug 大幅减少。这与 Sethi 等发现的"取消时正确的释放例程被短路"（见 Q4）互相印证。对工具的启发是**报告建议**而非新机制。LHF：文档层面是。

### Inferences
- "cancel = 崩溃、settle = 恢复"的类比在 **oracle 层面高度成立**（恢复后状态必须属于合法集合），在**注入点层面部分成立**：文件系统崩溃会丢失未持久化的写，而 Rust 取消丢失的是 future 内部的局部状态（栈上被 take 出来的值）——所以 Dropwise 的"状态"应是**堆上共享状态 + Obligation**，这与 Sethi 等"80% 清理 bug 涉及堆数据而非持久数据"的发现一致（见 Q4）。
- B3 的"≤3 操作"与 Yuan 等"98% 失败在 ≤3 节点上显现"（见 Q4）共同支持"小范围假设"，可作为 Dropwise 默认界限的依据。
- 崩溃测试中"随机损坏重复多次"对应 Dropwise 的"同一取消点下扰动 settle 期间调度"，这能弥补多线程 runtime 下的不确定性，但单线程 + 暂停时钟下价值有限。

### Gaps
- 未取得 ACE 按 seq-1/seq-2/seq-3 分解的 bug 数量。
- 未读 FiSC（Yang et al., OSDI 2004 "Using Model Checking to Find Serious File System Errors"）原文，未纳入具体数字。
- Chipmunk "系统调用内部崩溃点"的具体比例未核实，上面的解读基于其摘要与我对该文的已有了解。

## Q3. 其他语言/运行时如何定义和测试异步中断：Haskell、Java、POSIX、.NET、Go、Erlang、Kotlin、Swift、Trio/asyncio、OCaml Eio、JS

### Takeaway
历史走向非常一致：**"可在任意点注入的异步中断"（Java `Thread.stop`、.NET `Thread.Abort`）被证明不可安全使用而被废弃，各生态收敛到"只在显式检查点中断 + 作用域化屏蔽 + 结构化清理"**（POSIX 延迟取消点、Haskell mask/interruptible、Trio 检查点与 cancel scope、Kotlin 协作式取消、Eio Cancel.protect）。Rust 的"在每个返回 Pending 的 await 处 drop"恰好是这条光谱的中间位置（类似 POSIX 延迟取消：await 即取消点），但**缺少屏蔽原语和异步清理**，所以 Rust 的 bug 主要集中在"take 后跨 await 未恢复"。可直接迁移给 Dropwise 的最强测试机制来自 Go `testing/synctest`、Trio `wait_all_tasks_blocked`、Eio mock clock 的**"静默（quiescence）检测"**和 Go goleak 的**任务泄漏检查**，以及 Kotlin `onUndeliveredElement` 所承认的"通道元素在取消中丢失"问题。

### Cited Findings

**Haskell**
- 【旧 2001】Marlow, Peyton Jones, Moran, Reppy, "Asynchronous Exceptions in Haskell", PLDI 2001：为解决微妙的竞态，引入基于两个作用域组合子 `block`/`unblock` 的推迟异步异常投递机制，并给出"可挂起操作"的令人意外的语义——无限期阻塞的操作必须是可中断的，这样锁才能被安全获取与释放；给出形式语义 — [MSR 页](https://www.microsoft.com/en-us/research/publication/asynchronous-exceptions-haskell-3/)；[PDF（UChicago 课程镜像）](https://www.classes.cs.uchicago.edu/archive/2007/spring/32102-1/papers/p274-marlow.pdf)；后来 Marlow 在邮件中承认 GHC 6.4/6.6 实现与论文语义不一致、保证更弱 — [haskell-cafe](https://mailman.haskell.org/archives/list/haskell-cafe@haskell.org/thread/FBNQZDQC3YUEIMXHD2WQEWE6ICIWHW3X)
- 【近 GHC base 文档】`mask`："Executes an IO computation with asynchronous exceptions masked"；被屏蔽线程在**阻塞**时仍可能收到异步异常（`MaskedInterruptible`："asynchronous exceptions are masked, but blocking operations may still be interrupted"）；`uninterruptibleMask` "SHOULD BE USED WITH GREAT CARE"，阻塞时线程会不可响应、不可杀；文档推荐用 `bracket` 抽象"acquire 与 release 配对"；捕获一切异常"normally not what you want"，会吞掉 `UserInterrupt`（Ctrl-C）和忽略 `ThreadKilled` — [Control.Exception](https://hackage-content.haskell.org/package/base-4.22.0.0/docs/Control-Exception.html)
- 【近】dejafu：Haskell 并发的系统化、确定性测试库，changelog 中修复过"通过抛异常离开 mask 时恢复正确屏蔽状态"、"主线程被 throwTo 杀死时 trace 不正确"等问题，说明它在系统化探索中建模了 `throwTo`/`mask` — [Stackage dejafu](https://www.stackage.org/lts-24.58/package/dejafu)
  - **Dropwise 启发**：(1) 概念映射可写进文档：Rust 的 `.await` = Haskell 的 interruptible operation；两个 await 之间的同步代码 = 隐式 mask；Drop guard = `bracket`/`onException`；`tokio::spawn` 出去的工作 ≈ `uninterruptibleMask`（不会被调用者取消，但可能"永远挂起"——正是 Dropwise "held forever" 违例）。(2) Haskell `bracket` 存在的根本理由——"acquire 之后、安装清理之前"的窗口——就是 Dropwise 的主 bug 类"take 后跨 await 未恢复"。**工具可做的**：对违例的 Pending 位置，若 strong 快照显示状态 ∉ {pre, post}，报告文本中直接给出修复模式（"用 drop guard 包裹 take/restore"）。工作量小。LHF：是（报告/文档）。(3) dejafu 把"异步异常投递"和"调度交错"联合系统探索——Dropwise 只探索前者；与 loom/shuttle 联合是更大工程。LHF：否。

**Java**
- 【旧 JDK 1.2 起】Oracle "Java Thread Primitive Deprecation"：`Thread.stop` "inherently unsafe"，因为线程停止时会解锁它持有的所有监视器，受保护对象可能处于不一致（"damaged"）状态；`ThreadDeath` 静默杀死线程，损坏可能在"hours or days"后才显现；捕获 `ThreadDeath` 修复损坏不现实，因为它"几乎可以在任何地方"抛出，且**清理过程中可能再来第二个 ThreadDeath，清理需重复直到成功**；替代方案是 volatile 标志 + `Thread.interrupt`，不能立即处理 `InterruptedException` 的方法应**重新设置中断状态** — [Oracle 文档](https://docs.oracle.com/javase/8/docs/technotes/guides/concurrency/threadPrimitiveDeprecation.html)
- 【近 2022】JDK 20 中 JDK-8289610 使 `Thread.stop()` 无条件抛 `UnsupportedOperationException`（方法未删除）；JDK 19 起对虚拟线程已抛 UOE；`suspend/resume` 在同一版本经 JDK-8249627 降级 — [openjdk PR #10230](https://github.com/openjdk/jdk/pull/10230)；[inside.java](https://inside.java/2022/11/09/quality-heads-up/)
  - **Dropwise 启发**：(1) "damaged objects 可能数小时后才显现"= Dropwise 需要 settle + 事后检查的根本原因，可作为文档动机。(2) **"清理中的第二次中断"** 是 Dropwise 尚未系统覆盖的维度：在 Rust 中对应"第一次取消后，补偿/重试/清理逻辑本身是 async 的（被 spawn 或在调用者的下一轮 select! 中），它又被取消"。Dropwise 已有 `max_cancellations`，但应**优先生成"第二次取消落在第一次取消触发的清理/重试路径上"的计划**（即第二个 target 是在第一次取消之后才创建的 future）。工作量小–中（计划排序策略）。LHF：是。Sethi 等也独立报告了此类（HIVE-15997，见 Q4）。

**POSIX threads**
- 【旧 1995 POSIX.1c】`pthread_cancel` 默认为延迟（deferred）取消，只在"取消点"（一组规定的阻塞函数，如 `read`、`pthread_cond_wait`）生效；`pthread_setcancelstate` 可禁用取消；`pthread_cleanup_push/pop` 注册清理处理函数，取消时按 LIFO 执行；异步取消类型只在 async-cancel-safe 代码中允许 — [pthread_cancel(3)](https://man7.org/linux/man-pages/man3/pthread_cancel.3.html)；[pthreads(7) 取消点列表](https://man7.org/linux/man-pages/man7/pthreads.7.html)（URL 未本次核验，内容为标准文档）
  - **Dropwise 启发**：Rust 的取消模型几乎就是 POSIX 延迟取消：取消点 = 返回 Pending 的 await；清理处理函数 = Drop（LIFO 顺序一致）；`setcancelstate(DISABLE)` 在 Rust 中**不存在**（只能 spawn 绕开）。Dropwise 枚举"取消点"的方式与 POSIX 的"取消点表"对应：可在报告中输出每个 target 的"取消点表"（观察到的 Pending 序列及其源码位置），供代码审查。工作量小。LHF：是（与"报告注入位置"同一实现）。

**.NET**
- 【近 2020】.NET 5 起 `Thread.Abort` 标记为 obsolete（警告 SYSLIB0006），在 .NET Core/.NET 5+ 上调用抛 `PlatformNotSupportedException`；理由：中止另一线程无法确定哪些代码执行了、哪些没执行，"may prevent the execution of static constructors or the release of managed or unmanaged resources"；推荐用 `CancellationToken` + `ThrowIfCancellationRequested` — [.NET 5 breaking change](https://learn.microsoft.com/en-us/dotnet/core/compatibility/core-libraries/5.0/thread-abort-obsolete)；[SYSLIB0006](https://learn.microsoft.com/en-sg/dotnet/fundamentals/syslib-diagnostics/syslib0006)
  - **Dropwise 启发**：.NET 选择"令牌显式传递"，其特有 bug 是"invisible token"——令牌没被传给被调者（Sethi 等，见 Q4）。Rust 没有令牌，drop 自动结构化传播，**但 `tokio::spawn` 打破了结构**：丢弃 `JoinHandle` 不会取消已 spawn 的任务，这正是 Rust 版的"invisible token"——父被取消、子仍在跑。Dropwise 可借此定义**孤儿任务 oracle**：settle 后存活任务数相对 baseline 增加 → 报告（见 Go goleak）。工作量小。LHF：**是**。

**Go**
- 【中 2014】Go 采用 `context.Context` 显式传递取消/截止时间（Go 博客 "Go Concurrency Patterns: Context", Sameer Ajmani, 2014）— [go.dev/blog/context](https://go.dev/blog/context)（URL 未本次核验）
- 【近 2024】Saioc, Shirchenko, Chabbi（Uber），CGO 2024 "Unveiling and Vanquishing Goroutine Leaks in Enterprise Microservices"：在约 7500 万行、约 2500 个微服务的 Go monorepo 中，单测期运行的 **GOLEAK** 在遗留代码中发现 857 个已存在的 goroutine 泄漏（"partial deadlock"），一年内阻止约 260 个新泄漏；生产期 LEAKPROF 发现 24 个、修复 21 个，部分服务获得最高 34% 加速与 9.2× 内存下降 — [arXiv 2312.12002](https://arxiv.org/abs/2312.12002v1)；[Uber 博客](https://www.uber.com/us/en/blog/leakprof-featherlight-in-production-goroutine-leak-detection)
- 【近 2025】Go `testing/synctest`（Go 1.24 实验，**Go 1.25 正式 GA，2025-08**）：测试在"bubble"中运行，使用虚拟时间；`synctest.Wait` 阻塞直到 bubble 内所有其他 goroutine **durably blocked**（只能被 bubble 内其他 goroutine 唤醒；通道收发、全 bubble 通道的 select、`sync.Cond.Wait`、`time.Sleep` 等算，mutex/IO/系统调用不算）；所有 goroutine 都 durably blocked 时虚拟时钟跳到下一个能唤醒某物的时刻；若无物可唤醒且根 goroutine 未退出则以死锁 panic — [pkg.go.dev testing/synctest](https://pkg.go.dev/testing/synctest)；[Go 博客 "Testing Time"](https://go.dev/blog/testing-time)；[Go 1.25 发布说明](https://go.googlesource.com/website/+show/refs/heads/master/_content/blog/go1.25.md)
  - **Dropwise 启发**：(1) **task-leak oracle（goleak 同构）**：Tokio 稳定 API `RuntimeMetrics::num_alive_tasks()`（"Returns the current number of alive tasks in the runtime"，多线程下"may not have strong consistency"）— [docs.rs](https://docs.rs/tokio/latest/tokio/runtime/struct.RuntimeMetrics.html)。settle 后若 trial 的存活任务数 > baseline 的存活任务数 → 报"取消导致孤儿/泄漏任务"。约 50–100 行。LHF：**是，最高优先级之一**（goleak 的 857 个数字说明这类泄漏极常见，而当前 Dropwise 只在泄漏任务恰好持有 Obligation 时才发现）。(2) **以"静默"代替"固定 settle 窗口"**：synctest 的"durably blocked + 时钟跳到下一个事件 + 无事件即判死锁"三段语义，可映射为 Dropwise 的 `Settle::Quiescent`：在暂停时钟的 current_thread runtime 中，反复 `yield` 直到无可运行任务，然后依赖 Tokio 的 auto-advance（"If time is paused and the runtime has no work to do, the clock is auto-advanced to the next pending timer"，spawn_blocking 运行时暂停 auto-advance — [tokio::time::pause](https://docs.rs/tokio/latest/tokio/time/fn.pause.html)）推进到下一个计时器；当没有任何计时器、没有 blocking 工作、存活任务数不变时判定"已静默"，若此时仍有未结 Obligation 则直接判"held forever"而非等到超时。好处：settle 时间不再是需要调参的魔数，且"立即检查误报 vs 窗口太短漏报"的两难被消除。工作量中（Tokio 无公开 idle API，需用 metrics + 自定义 sleep 探针近似，或 `tokio_unstable` 下更精确）。LHF：中–高；**这是 Dropwise 早期误报问题的"正解"形态**。

**Erlang/OTP**
- 【旧 1980s–近】Erlang 进程终止时向所有链接（link）进程发送 exit signal；接收方未 trap exits 时以同一原因终止，trap 时转为 `{'EXIT', From, Reason}` 消息；通过 `exit/2` 显式发送原因 `kill` 时不可捕获、无条件终止（原因变为 `killed`）；monitor 是单向的，进程终止后发送 `'DOWN'` 消息；信号只在同一发送者-接收者对之间保序 — [Erlang 参考手册: Processes](https://www.erlang.org/doc/system/ref_man_processes.html)
- 【中 2013】Concuerror：Erlang 无状态模型检查器，系统化探索单元测试触发的进程交错，用避免进程阻塞 + 抢占界限控制状态爆炸，可确定性重放错误轨迹 — [Erlang Factory 教程](https://erlang-factory.com/euc2015/stavros-aronis)；[论文记录](https://kar.kent.ac.uk/58947/)
  - **Dropwise 启发**：Erlang 的"let it crash + supervisor 恢复"与 Dropwise "cancel + settle"在哲学上最接近：取消被当作常态。可迁移的是 **"取消原因"维度**：Erlang 区分 normal/kill/自定义原因，Rust 的 drop 不带原因；Dropwise 计划中的 `Race` 实际编码了"为何被取消"（超时 vs 另一分支就绪 vs 唤醒后被抢占），报告中把它表述为"取消原因"有助用户理解。迁移弱，主要是叙述。Concuerror 类交错探索同 dejafu，不是 LHF。

**Kotlin coroutines**
- 【近 2020】kotlinx.coroutines 1.4.0 起所有通道挂起函数提供 **prompt cancellation guarantee**：若 send/receive 挂起时作业被取消，即使通道状态已改变也不会成功恢复，而是抛 `CancellationException`；后果是"可关闭资源可能被成功从通道取出，但若接收操作同时被取消则仍会丢失"；为此 `Channel()` 提供 `onUndeliveredElement`，对"已 send 但未送达"的每个元素调用一次（缓冲溢出丢弃、send 被取消或通道关闭、**receive 取出元素后在后续代码恢复前被取消**、通道被取消时缓冲中剩余元素）；文档还警告"若在 `openResource()` 与 `channel.send(...)` 之间有任何工作，资源在失败时需由你确保关闭" — [Kotlin Channel API 文档](https://kotlinlang.org/api/kotlinx.coroutines/kotlinx-coroutines-core/kotlinx.coroutines.channels/-channel/)
- 【近】Kotlin 文档 "Asynchronous timeout and resources" 一节警告 `withTimeout` 的超时事件相对于块内代码是异步的，可能在块即将返回前触发，并用大量协程示例展示资源泄漏；`withTimeoutOrNull` API 文档重复此警告 — [withTimeoutOrNull API](https://kotlinlang.org/api/kotlinx.coroutines/kotlinx-coroutines-core/kotlinx.coroutines/with-timeout-or-null.html)；[指南镜像](https://docs.w3cub.com/kotlin~1.9/docs/cancellation-and-timeouts)
  - **Dropwise 启发**：Kotlin 在**运行时 API 层**承认并修补了"取消中丢失已取出元素"问题，`onUndeliveredElement` 本质就是 Dropwise `Obligation` 的运行时版本——这是 Dropwise 设计的有力外部佐证。可迁移的具体机制：(1) **发送侧模型**：Dropwise 目前只有 `models::mpsc::Receiver`；仿照 Kotlin 的 "send 被取消" 情形，给 `Sender::send(value)` 加模型：被移入 send future 的值若随 future 一起被 drop 且未送达 → 未结 Obligation（Tokio 的 `send` 在被取消时值随 future 丢弃）。工作量小（一个 wrapper）。LHF：**是**。(2) 把"取出后、交付前被取消"作为文档中的标准 bug 模式，引用 Kotlin 为先例。

**Swift structured concurrency**
- 【近 2021–】Swift 取消是协作式的；`withTaskCancellationHandler` 文档：函数在运行 operation 前不挂起，若在 operation 中途取消，handler 与 operation **并发运行**；handler 可能在任务持有内部锁时运行，若 handler 获取锁可能与恢复 continuation 或取消任务的代码死锁 — [Apple 文档](https://developer.apple.com/documentation/swift/withtaskcancellationhandler(operation:oncancel:).md)；SE-0304 评审中作者确认"任务已被取消时 handler 立即运行"是有意设计 — [Swift 论坛 SE-0304 第三轮](https://forums.swift.org/t/se-0304-3rd-review-structured-concurrency/48847)
  - **Dropwise 启发**：迁移弱。唯一相关点：Swift 的"取消 handler 与 operation 并发"对应 Rust 中 Drop 与被 spawn 的清理任务/其他任务并发——Dropwise 在多线程 flavor 下才会暴露这类竞态。不建议投入。

**Python Trio / asyncio**
- 【近 2018】Nathaniel J. Smith, "Timeouts and cancellation for humans"（2018-01-11）：对比 cancel token（显式传参）与 Trio 的 cancel scope（隐式栈）；区分 **edge-triggered**（`Task.cancel`、`Thread.interrupt`：一次事件命中一次）与 **level-triggered**（进入已取消状态后后续每个操作都立即失败）取消，并以 websocket 例子说明 edge-triggered 下 `finally` 中的 `ws.close()` 可能永远挂起；`shield=True` 作为罕用逃生口；Trio 中每个 `await` 都是可抛 `Cancelled` 的检查点；文章指出 C# 高层异步网络函数接受 cancel token 参数却忽略它 — [vorpus.org 博文](https://vorpus.org/blog/timeouts-and-cancellation-for-humans/)
- 【近】Trio 测试工具：`wait_all_tasks_blocked(cushion)`——调用者阻塞直到所有其他任务都已阻塞至少 cushion 秒（以真实时间计）；`MockClock(autojump_threshold)`——检测到所有任务阻塞超过阈值即把时钟跳到下一个调度超时；`assert_checkpoints()` / `assert_no_checkpoints()` 断言代码块是否执行了检查点 — [Trio testing 文档](https://trio.readthedocs.io/en/stable/reference-testing.html)
- 【近】flake8-async（原 flake8-trio）静态规则：ASYNC100 超时/cancel scope 内无检查点因而永不触发；**ASYNC102 在 `finally`/捕获取消的 `except`/`__aexit__` 中 `await` 需要 shielded 超时 scope**；ASYNC103/104 捕获取消后未重新抛出；ASYNC120 except 中的检查点可能用 `Cancelled` 覆盖当前异常；ASYNC910–913 函数/生成器/循环可能不经检查点退出 — [flake8-async rules](https://flake8-async.readthedocs.io/en/latest/rules.html)
- 【近 Python 3.8+/3.11+】asyncio：`CancelledError` 直接继承 `BaseException`（"so most code will not need to be aware of it"）；建议用 try/finally 清理，若显式捕获 `CancelledError` 应在清理后重新抛出；`TaskGroup` 与 `asyncio.timeout()` 内部依赖取消实现，"might misbehave if a coroutine swallows CancelledError"；`cancelling()`/`uncancel()` 维护**取消请求计数**以隔离结构化块的取消；`shield()` 只保护内部任务，调用者仍收到 `CancelledError` — [asyncio Task 文档](https://docs.python.org/3/library/asyncio-task.html)
  - **Dropwise 启发**：(1) `wait_all_tasks_blocked` + `autojump` 与 Go synctest 一起，构成"静默检测替代固定窗口"的**两个独立先例**（见上 Go 部分）；API 形状可直接借：`Settle::Quiescent { cushion_yields: n }`。(2) Trio 的 edge vs level 区分对 Rust 的含义：Rust drop 是"极端 edge-triggered"——被 drop 的 future 不再有机会运行，**任何需要 await 的清理都必须 spawn 出去**，因此 ASYNC102 的 Rust 对应物是"Drop 中 spawn 的清理任务"，它恰好需要 settle 才能观察到——这解释了 Dropwise 早期"立即检查误报"的根源，并支持把 spawn 出的清理任务纳入"静默"判定。(3) ASYNC100 "scope 永不触发"↔ Dropwise 的 "target 在 baseline 中从未返回 Pending"——可以作为显式警告（"此 timeout/select 分支在该场景下不可能取消它"），工作量极小。LHF：是。(4) 静态规则启发：可做 Clippy 风格的 lint（"在 `select!` 分支内 `take()`/`mem::replace` 后跨 await"），但这超出 Dropwise 动态工具定位，非 LHF。

**OCaml 5 Eio**
- 【近 2022–】Eio：每个 fiber 有取消上下文，构成树；`Fiber.both` 中一方失败则另一方被取消；`Fiber.first` 返回先完成者并取消另一者（README 警告它不能可靠保证只执行一个动作）；任何可切换 fiber 的操作都可能抛 `Cancelled`；`Cancel.protect` 创建不随父取消的上下文；`Switch.run` 等待其 fiber 退出后释放附着资源；测试方面，`Eio_mock.Backend.run` 可**自动检测死锁**，`run_full` 提供在无可运行者时自动推进的 mock 时钟，同一 domain 内调度确定 — [Eio README](https://github.com/ocaml-multicore/eio)
  - **Dropwise 启发**：(1) `Fiber.first` 的警告（"不能保证只执行一个"）正是 `tokio::select!` 的取消问题，Eio 作者在 README 中明说，可作为 Dropwise 动机的又一外部佐证。(2) Eio mock 的"自动推进时钟 + 死锁检测"是第三个"静默检测"先例。(3) `Switch` 资源附着 ↔ Tokio `JoinSet`（drop 时 abort 全部任务）——Dropwise 可在报告中建议"用 JoinSet 代替裸 spawn"以消除孤儿任务。LHF：文档/报告层是。

**JavaScript AbortController**
- 【近 2017–】Web/Node 用 `AbortController`/`AbortSignal` 显式传递取消，Promise 本身不可取消；`AbortSignal.timeout()`、`AbortSignal.any()` 组合信号 — [MDN AbortController](https://developer.mozilla.org/en-US/docs/Web/API/AbortController)（URL 未本次核验）
  - **Dropwise 启发**：令牌式模型，与 C#/Go 同类，bug 类型为"信号没传下去"；对 Rust drop 式取消迁移弱。不建议投入。

### Inferences
- **历史教训的统一结论**：所有"任意点异步中断"的设计（Thread.stop、Thread.Abort、早期 Haskell 无屏蔽 throwTo）都因"无法知道哪些代码运行过 → 对象被损坏"而被废弃或加屏蔽。Rust 的 await-点取消比 Thread.stop 安全（仅在 await 处），但**没有屏蔽原语和异步清理**，因此"每个 await 都是潜在的 Thread.stop 点"这一说法对**跨 await 持有的临时状态**成立。Dropwise 的定位可以表述为"为 Rust 补上其他生态靠 mask/shield/onUndeliveredElement 在语言/运行时层面解决、而 Rust 只能靠测试发现的问题"。
- **三个独立生态（Go synctest、Trio、Eio mock）收敛到同一测试原语：静默检测 + 自动推进虚拟时钟 + 无进展即判死锁**。Tokio 已有一半（暂停时钟的 auto-advance），Dropwise 补上"静默判定 + 无进展即判 held-forever"即可，这是本次调研中**最明确、最少人在 Rust 取消测试语境下连接过的低垂果实**之一（据我所知；未在 Rust 生态中检索到等价工具，但这本身属于他人调研范围）。
- 令牌式生态（C#、Go、JS、Java interrupt）的主流 bug 是"取消没传到"；drop 式（Rust）的主流 bug 是"取消到得太突然，状态没恢复"。所以从令牌式生态迁移的应是**泄漏/孤儿检查**（因为 Rust 的 spawn 恰好重新引入了"没传到"问题），而从 Haskell/Kotlin/Trio 迁移的应是**状态恢复与已取出元素丢失**的 oracle。

### Gaps
- 未检索到 Kotlin、Swift、JS 生态中专门针对"取消正确性"的系统化测试工具（kotlinx-coroutines-test 的 `runTest`/`advanceUntilIdle` 我知道存在，但本次未核验其文档；它是又一个"推进直到空闲"先例）。
- 未核验 Haskell `safe-exceptions`/`unliftio`（区分同步/异步异常以避免误吞）的文档；FP Complete/Snoyman 的相关博文未读。
- PULSE（Claessen et al., ICFP 2009，QuickCheck 控制 Erlang 调度）未检索到原文。
- POSIX、Go context、JS AbortController、crash-only software 的 URL 未在本次逐一核验（标注于条目中）。

## Q4. 取消/超时/错误处理的实证研究：哪些简单检查能抓住大部分 bug？

### Takeaway
三项研究给出一致信号：(1) Yuan 等（OSDI'14）——灾难性失败中 92% 源于对**非致命错误的错误处理**，77% 可由单元测试复现，98% 在 ≤3 个节点上显现；(2) Sethi 等（OSDI'22）——156 个取消 bug 中，**取消后清理有缺陷**占 20% 以上，其中 80% 涉及**堆数据**而非持久数据，最常见修复是"释放资源、使被修改数据失效或回滚"，且症状以资源泄漏（30）和性能问题（29）居多；(3) Go 研究——阻塞类 bug 中消息传递占比高，Uber 的 goleak 在测试期发现 857 个泄漏。对应 Dropwise 的"简单检查"是：**settle 后堆状态回到 {pre, post}、存活任务数回到 baseline、许可/计数守恒、调用者观察到的结果与实际效果一致**。

### Cited Findings
- 【中 2014】Yuan et al., "Simple Testing Can Prevent Most Critical Failures: An Analysis of Production Failures in Distributed Data-Intensive Systems", OSDI 2014：研究 Cassandra、HBase、HDFS、MapReduce、Redis 中 198 个随机抽样的用户报告失败；"Almost all catastrophic failures (48 in total – 92%) are the result of incorrect handling of non-fatal errors"；77% 的生产失败可由单元测试复现；98% 在 ≤3 个节点上显现、84% 在 ≤2 个；74% 是确定性的；约三分之一灾难性失败由琐碎错误处理错误引起，其中 23% 用错误处理代码的 100% 语句覆盖即可暴露 — [USENIX 页](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/yuan)；数字引自 [Adrian Colyer 摘要](https://blog.acolyer.org/2016/10/06/simple-testing-can-prevent-most-critical-failures/)（92% 未在摘要原文中核对，引用前应核对 PDF：[gwern 镜像](https://gwern.net/doc/cs/algorithm/2014-yuan.pdf)）
- 【中 2014】Aspirator 三条规则：错误处理器为空或只打日志；处理器注释含 TODO/FIXME；捕获过于宽泛的异常（Exception/Throwable）后执行中止系统等激烈动作；在 9 个系统中发现 143 个已确认/修复的 bug 与坏实践（摘要）；Colyer 摘要称共 500 个新 bug/坏实践、115 个误报、报告 171 个 — [USENIX](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/yuan)；[Colyer](https://blog.acolyer.org/2016/10/06/simple-testing-can-prevent-most-critical-failures/)
  - **Dropwise 启发**：(1) "错误处理代码 100% 语句覆盖就能抓 23%"→ Dropwise 可报告**取消路径覆盖**：哪些 Drop impl / drop guard / 补偿分支在至少一个计划中被执行过（用户可用 `dropwise::probe!("label")` 标注清理代码，报告未被任何计划触达的清理探针）。工作量小。LHF：是。(2) "≤3 节点 / 单测可复现"支持 Dropwise 的单进程小场景定位。(3) Aspirator 规则在 Rust 的对应物（`let _ = tx.send(...)` 忽略取消路径上的错误、空 Drop）属于 lint 而非 Dropwise 范围。
- 【近 2022】Sethi, Pan, Lu, Musuvathi, Nath, "Cancellation in Systems: An Empirical Study of Task Cancellation Patterns and Failures", OSDI 2022（pp.127–141）：研究 13 个 Java/C#/Go 流行分布式/并发系统中的 62 个取消特性请求与 156 个取消相关 bug；问题出现在取消的所有阶段：决定何时取消哪个任务（约三分之一）、把取消请求从发起者传播到目标（约四分之一）、在目标中履行取消（约三分之一）；作者用 CodeQL 实现了 5 个反模式静态检查器，在 13 个应用的最新版本中找到约 200 个实例；作者认为取消问题被低报，因为"cancel code can be difficult to exercise during testing" — [USENIX 页](https://www.usenix.org/conference/osdi22/presentation/sethi)；[PDF](https://www.usenix.org/system/files/osdi22-sethi.pdf)
- 【近 2022】Sethi 等关于**缺陷清理**（超过全部 bug 的 20%）：四类错误——清理时把变量改成错误值（10 个，如 SOLR-8372 把更新日志错误置为 active，可能丢数据）、清理不完整（14 个，如 CASSANDRA-7803 只删正常位置文件不删临时文件）、完全缺失清理（4 个，如 HBASE-13877 被取消任务修改的 memstore snapshot 未失效，被后续任务复用导致数据损坏）、清理对共享变量操作顺序错误（3 个）；**80% 的清理 bug 目标是堆数据而非持久数据**（任务跟踪结构、指向持久数据的指针、系统指标/元数据）；最常见补丁是释放资源、使被修改数据失效或回滚——原任务通常已有正确释放例程，但取消时被**短路**；例如 CASSANDRA-15674 任务对共享 `totalDiskSpaceUsed` 做了一次修改，取消时应记得减回同样的值；清理中再次抛异常（"double-exception"）可能跳过剩余清理（HIVE-15997）— [Sethi et al. PDF](https://www.usenix.org/system/files/osdi22-sethi.pdf)
- 【近 2022】Sethi 等症状统计（Table 6）：资源泄漏 30、性能问题 29、任务 API 损坏 17、数据损坏/丢失 5、**错误报告给用户 10**（如系统报告作业已取消而实际没有：HIVE-14942、SPARK-18665；或反之 SPARK-2666）、未指明 65，共 156 — [Sethi et al. PDF](https://www.usenix.org/system/files/osdi22-sethi.pdf)
  - **Dropwise 启发**：这是与 Dropwise 主 bug 类最贴合的实证。(1) "堆状态（计数器、跟踪表）在取消后未回滚" = CASSANDRA-15674 型 = Dropwise 的 "state taken / partially committed before an await"；**strong/计数守恒 oracle 正对此**（Q1）。(2) "正确释放例程被取消短路" → 报告建议"把释放移入 Drop guard"。(3) **"错误报告"类 → 新 oracle：结果-效果一致性**：若调用者观察到"被取消/超时"（`ctx.target` 返回 `None`、`timeout` 返回 `Err(Elapsed)`），则 settle 后效果应为 pre（或用户声明为幂等）；若效果是 post，则调用者必须能得知（例如结果通过 Obligation/通道送达）。实现上即 strong oracle 的"按调用者所见结果条件化"版本：`Cancelled ⇒ pre`、`Completed ⇒ post`，额外标出 "Cancelled 但 post" 为"静默提交"违例。工作量小（在 strong oracle 之上 50 行）。LHF：**是**。(4) "double-exception 跳过剩余清理" → 见 Q3 Java 部分"第二次取消落在清理路径上"的计划优先级。(5) 资源泄漏是第一大症状 → task-leak + 许可/计数守恒检查器。
- 【中 2018】Dai, He, Gu, Lu, "Understanding Real-World Timeout Problems in Cloud Server Systems", IEEE IC2E 2018：研究 11 个云服务器系统的 156 个超时 bug；按根因，误用超时值占 47%、缺失超时检查占 31% — [IC2E'18 slides](https://conferences.computer.org/IC2E/2018/pdf/ic2e18_slides_tingdai.pdf)；后续 TFix+ 引用该组更早研究称 78% 的超时 bug 来自缺失或误用超时机制 — [TFix+ arXiv](https://arxiv.org/pdf/2110.04101)
  - **Dropwise 启发**：超时值误用（太小/太大）主要是性能/活性问题，与 Dropwise 的取消正确性正交；可借的只是 `Race::After(d)` 在 d 附近扫描（目标的计时器边界前后各一点）以覆盖"恰好在临界时间被取消"的情况——但 Dropwise 已按 Pending 枚举，时间扫描冗余。迁移弱，非 LHF。
- 【中 2019】Tu, Liu, Song, Zhang, "Understanding Real-World Concurrency Bugs in Go", ASPLOS 2019：研究 Docker、Kubernetes、gRPC 等 6 个 Go 项目中的 171 个并发 bug，过半由 Go 特有问题引起；阻塞类 bug 中约 58% 源于消息传递、42% 源于共享内存保护（数字来自二手摘要，需核对原文）— [Colyer 摘要](https://blog.acolyer.org/2019/05/17/understanding-real-world-concurrency-bugs-in-go/)；[数据集](https://github.com/system-pclub/go-concurrency-bugs)；[PSU 记录](https://pure.psu.edu/en/publications/understanding-real-world-concurrency-bugs-in-go/)
  - **Dropwise 启发**：Go 中典型的取消相关阻塞 bug 是"父在 `ctx.Done()` 分支返回后，子 goroutine 向无人接收的无缓冲通道发送而永远阻塞"。Tokio 中对应：bounded `mpsc::Sender::send().await` 在接收者存活但已不再读取时永远挂起（例如接收循环的 future 被取消后接收者被另存起来）——这类挂起的 spawned 任务不持有 Obligation 时，Dropwise 当前**看不到**；task-leak oracle 可以看到。LHF：是（同 task-leak）。
- 【近 2024】Uber goleak/LeakProf 数据（857 个既有泄漏、约 260 个新泄漏被阻止）见 Q3 Go 部分 — [arXiv 2312.12002](https://arxiv.org/abs/2312.12002v1)

### Inferences
- **"简单检查"清单（按预期收益/成本排序，均为 Dropwise 低垂果实）**：
  1. **Task-leak / 孤儿任务 oracle**：settle 后 `num_alive_tasks()` 相对 baseline 不增加（goleak 同构；Sethi 最大症状类为资源泄漏；Rust 的 spawn 重新引入了"取消没传到"问题）。≈50–100 行。
  2. **Strong / 原子性 oracle**：用户给快照函数，settle 后 state ∈ {pre, post}（Abrahams StrongCheck + SQLite/CrashMonkey 的"完成或回滚"；直击 Sethi 的 80% 堆数据清理 bug 和 Dropwise 的主 bug 类）。≈150–300 行。
  3. **结果-效果一致性 oracle**：调用者看到 Cancelled ⇒ 效果为 pre；看到 Completed ⇒ post；"Cancelled 但 post" 报为静默提交（Sethi 的 incorrect reporting 类；在 2 之上 ≈50 行）。
  4. **静默式 settle（Settle::Quiescent）**：无可运行任务 + 无计时器 + 无 blocking 工作 + 存活任务数稳定即判定完成，无进展而仍有未结 Obligation 即判 held-forever（Go synctest、Trio wait_all_tasks_blocked/autojump、Eio mock 三个先例；消除 settle 窗口调参与"立即检查误报"）。中等工作量。
  5. **Persistent（持续）取消模式**：重试/重建的同一 target 在每次重试的第 k 个 Pending 都取消（SQLite fail-continuously），暴露"每次取消泄漏一点"的累积 bug。≈100–200 行。
  6. **发送侧 Obligation 模型**：`Sender::send(v)` 被 drop 时 v 未送达即未结（Kotlin `onUndeliveredElement` 先例）。小。
  7. **库边界自动标记（tokio shim）**：常用 tokio 同步/IO API 的 await 自动成为 target（LFI 先例）。中等，易用性收益最大。
  8. **报告增强**：注入点源码位置 + 每 target 的取消点表 + 按 (位置, 上下文) 聚类 + 违例分类（原子性/责任丢失/静默提交）+ 未被触达的清理探针（Boost itest 教训、Linux fail-nth verbose、FIFUZZ、ALICE、Yuan 覆盖率发现）。小。
  9. **计划优先级**：第二次取消优先落在第一次取消触发的清理/重试路径上（Java ThreadDeath 文档 + Sethi HIVE-15997）；有快照时按抽象状态去重/排序（Legolas、FATE 剪枝）。中。
- 弱迁移、不建议投入：超时值扫描（Dai et al./TFix）、Swift cancellation handler 竞态、JS AbortController、与 loom/shuttle 的联合交错探索（价值高但不是低垂果实）。
- 一个"社区尚未连接"的组合（据本次调研范围内的检索）：**Abrahams StrongCheck（2000）× Go synctest 静默判定（2025）× goleak（2024）**——三者分别来自 C++ 异常安全、Go 测试工具、Go 泄漏检测，组合后正好构成"取消 → 静默 → {pre,post} 状态检查 + 无孤儿任务"的完整自动 oracle，且每一部分在 Tokio 中都有现成的稳定原语（暂停时钟 auto-advance、`RuntimeMetrics::num_alive_tasks`、`PartialEq` 快照）。

### Gaps
- Sethi 等 5 个反模式的完整列表与每个的命中数只部分读到（Java InterruptedException 吞掉 21 例中 14 例真阳；C# 循环不检查令牌 154 例；ASP.NET Core/Roslyn 中令牌未传递各 9 例），未逐一核对；反模式均为令牌/interrupt 式，对 Rust 直接迁移弱。
- Tu et al. 的 58%/42% 及"race detector 只检测到 21 个复现阻塞 bug 中的 2 个"为二手数字，未核对原文。
- 未检索到专门研究 Rust async 取消 bug 的实证论文（属其他研究者范围）；也未检索 Kotlin/Python asyncio 取消 bug 的实证研究。
- TFix（He, Dai, Gu, ICDCS 2019）原文未读，仅通过 TFix+ arXiv 间接引用。
