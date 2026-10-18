# 测试 oracle、obligation/资源追踪、事务/补偿：对 Dropwise 的可迁移机制

> 范围：(1) 自动/隐式 oracle；(2) obligation/资源追踪；(3) 事务与补偿概念；(4) 瞬时 vs 持久违例与“何时观察”。
> 标记：【经典】= 2010 年以前；【近期】= 2010 年及以后。每条后附“Dropwise 启发”（机制、粗略工作量、是否低垂果实）。
> 术语约定：pre 态 = 被取消操作开始前的可观察状态；post 态 = 未取消（baseline）运行中该操作完成后的状态；“半完成态” = 两者皆非。

---

## 问题 1：自动/隐式 oracle（差分、蜕变、强异常保证式状态相等、可线性化+“aborted”结果、不变式挖掘、状态机/PBT）

### Takeaway
最便宜、最可迁移的 oracle 不是“用户写不变式”，而是**参照执行**：把取消运行的可观察状态与 baseline 的 pre/post 态比较（Abrahams 的 StrongCheck、Witcher 的“committed 或 rolled back 二选一”输出等价、strict linearizability 的“pending 操作要么生效要么不生效”）。Dropwise 已经有 baseline 运行和按 Pending 边界枚举的计划，只差一个用户提供的 `observe()` 快照函数，就能把“半完成态”检测从手写不变式变成自动比较。这是本报告中最突出的低垂果实。

### Cited Findings

**A. 强异常保证式状态相等 oracle（与 Dropwise 结构几乎同构）**
- 【经典 1998/2000】Abrahams《Exception-Safety in Generic Components》定义 strong guarantee：操作要么成功完成，要么抛异常并“使程序状态与操作开始前完全一致”；basic guarantee 只要求保持组件不变式且不泄漏资源。其自动测试流程：在每个可能抛出的点（客户提供的操作、被插桩的全局 `operator new`）调用 `ThisCanThrow`，它递减全局计数器、到 0 时抛出；外层循环让计数器从 0 开始逐次加一，“使操作在其执行路径上每一个可能失败的步骤依次抛出”。`StrongCheck` 每轮先复制被测值，对副本执行操作，若捕获异常则断言副本与原值相等；每轮后对原值运行 `CheckInvariant`（容器可特化，例如 `size()==distance(begin(),end())`）。泄漏通过插桩 new/delete 并让每个类型参数持有堆指针来暴露。局限：只适用于异常中立的操作，若操作吞掉异常继续执行，计数器变负，后续失败点测不到 — [Boost: Exception-Safety in Generic Components](https://boost.org/doc/libs/1_34_0/more/generic_exception_safety.html)
  - **Dropwise 启发**：这就是 Dropwise 的“计数器→第 k 个 Pending 处取消”的前身，但它多了一个**零成本 oracle：快照相等**。建议新增 `ctx.observe(|| state_snapshot)`（返回 `impl Debug + PartialEq`），Dropwise 在 baseline 中记录目标操作开始前快照 S_pre 与完成后快照 S_post，取消+settle 后要求 S ∈ {S_pre, S_post}，否则报“半完成态”。正好对应“take/pop/计数器在 await 前改、await 后恢复/提交”的常见 bug 形状。工作量：小（~几百行，复用现有 baseline/plan 机制）。**低垂果实：是。** 注意 Abrahams 的“吞异常”局限对应 Dropwise 里 `select!` 保留（Kept）或程序自行重试的分支——这类计划要换用下面 C 的“最终态差分”。

**B. 崩溃一致性测试的双参照执行 oracle（最贴近“取消后要么全做要么没做”）**
- 【近期 2021】Witcher（SOSP 2021）从 NVM 访问之间的数据/控制依赖自动推断“likely correctness conditions”，再通过**有崩溃与无崩溃执行的输出等价检查**验证违例是否是真 bug；arXiv 版描述两个参照执行：一个假定崩溃时的操作已提交、一个假定已回滚，崩溃一致的程序应表现得“像该操作要么完全执行（committed）要么完全未执行（rolled back）”。在 20 个基于 PMDK 的 KV 存储上发现 47 个（36 个新）正确性 bug 和 158 个性能 bug — [Witcher SOSP'21 PDF](https://www3.cs.stonybrook.edu/~dongyoon/papers/SOSP-21-Witcher.pdf)；[arXiv 2012.06086](https://arxiv.org/abs/2012.06086)
  - **Dropwise 启发**：把“crash”换成“cancel”，Witcher 的 oracle 可以原样搬：对每个取消计划，构造“假定已提交”（继续跑完 baseline）和“假定已回滚”（跳过该操作）两条参照，比较取消运行之后的**后续可观察输出**（不只是内存快照）。与 A 互补：A 比状态，B 比后续行为（例如之后的 `recv` 序列）。其“从依赖推断 likely 条件、再用输出等价筛真 bug”的两段式也可借鉴为降误报流程。工作量：中。**低垂果实：部分是**（状态比较版是；行为比较版要求场景可重放后续输入）。
- 【近期 2018】CrashMonkey/B3（OSDI 2018）：在有界空间内穷举生成工作负载，在持久化点（fsync 相关调用）模拟崩溃，检查恢复后状态；bug 报告包含“workload、expected state、actual state”；依据是对历史 Linux 文件系统 bug 的研究发现所有已报告 bug 都源于 fsync 相关调用之后的崩溃；65 台机器跑两天复现 24 个已知 bug、发现 10 个新 bug — [USENIX OSDI'18 Mohan et al.](https://www.usenix.org/conference/osdi18/presentation/mohan)；[arXiv 1810.02904](https://arxiv.org/pdf/1810.02904)
  - **Dropwise 启发**：“有界穷举 + 只在持久化点崩溃”的剪枝论证，对应 Dropwise 的“只在观测到的 Pending 边界取消”；可借鉴的是它用 bug 研究来**论证剪枝充分性**，以及报告格式里显式给出 expected vs actual state（与 A 的快照 diff 天然配套）。工作量：小（报告格式）。低垂果实：报告格式是。

**C. 差分测试与蜕变测试（“取消+重试 ≡ 不取消”）**
- 【经典 1998】McKeeman《Differential Testing for Software》，Digital Technical Journal 10(1):100–107：同一输入喂给多个同类实现、比较执行差异来发现 bug — [dblp 记录](https://dblp.org/pid/40/4856)；[Wikipedia: Differential testing](https://en.wikipedia.org/wiki/Differential_testing)
- 【近期 2016】Segura 等《A Survey on Metamorphic Testing》（IEEE TSE 42(9), 2016）：蜕变测试检查“对输入施加变换后输出如何变化”，而非单个具体输出；蜕变关系概念于 1998 年提出 — [US 机构库记录](https://hdl.handle.net/11441/38271)
- 【近期 2013】Ramalingam & Vaswani《Fault Tolerance via Idempotence》（POPL 2013）：提出一个“简单而诱人”的通用正确性准则——应用在存在重复请求和进程失败时的行为应与不存在它们时**不可区分**；形式化为 failfree idempotence（Def 2.3：程序在真实语义下与 IDEAL 语义观察等价），包含安全性（idempotence）与进程性（failure-freedom）两部分，并给出自动保证该性质的 idempotence monad（含补偿扩展）— [POPL'13 PDF](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/popl38-ramalingam.pdf)
  - **Dropwise 启发**：这正是“取消+重试 ≡ 不取消”的形式化来源，可直接作为 Dropwise 的**默认差分 oracle**：对 `ctx.race` 循环这类程序本身会继续/重试的场景，要求取消运行 settle 后的最终 `observe()` 与 baseline 最终 `observe()` 相等（或满足用户选的关系：相等 / 多重集相等 / 允许重复 / 允许丢失）。quickstart 中 `check(&sink)` 其实就是手写版。工作量：小。**低垂果实：是（最高优先级之一）。** 弱点：调度/时间敏感的输出需要规范化（排序、去时间戳），可提供 `Relation::MultisetEq` 等预置关系。
- 【近期 2013/2016】Hummer 等《Testing Idempotence for Infrastructure as Code》（Middleware 2013）：IaC 自动化步骤应幂等以保证可重复与收敛，测试需验证系统从任意起始状态幂等地收敛到目标态；用抽象系统模型导出状态转移图、系统生成测试，在 Chef 公开脚本上检出非幂等自动化；Hanappi/Hummer/Dustdar（OOPSLA 2016）扩展到 Puppet 收敛性 — [Middleware'13 PDF](https://dsg.tuwien.ac.at/team/hummer/docs/2013-middleware-iac.pdf)；[OOPSLA'16 PDF](https://dsg.tuwien.ac.at/Staff/sd/papers/OOPSLA_2016_O_Hanappi.pdf)
  - **Dropwise 启发**：借“从中途状态重跑必须收敛到同一终态”这一关系：取消发生在第 k 个 Pending 后，**再次执行同一操作**（`ctx.retry(op)`）后的状态应等于 baseline 的 post 态。这把“被取消后能否安全重试”变成自动可检的蜕变关系，用户无须写不变式。工作量：小-中（需要场景能重新构造操作 future）。低垂果实：是（对 `target` 场景）。
- 【近期 2024】Stoica 等《If At First You Don't Succeed, Try, Try, Again…? Insights and LLM-informed Tooling for Detecting Retry Bugs》（SOSP 2024，工具 Wasabi）：重试功能普遍测试不足；ad-hoc 的重试实现难以被传统程序分析处理，但 LLM 能较好识别；**复用已有单元测试 + 故障注入**（在可能触发重试的位置抛出异常）可暴露多类重试问题 — [UChicago 全文 PDF](https://knowledge.uchicago.edu/record/14028/files/If-At-First-You-Dont-Succeed-Try-Try-Again-Insights-and-LLM-informed-Tooling.pdf)；[Chameleon 博客](https://blog.chameleoncloud.org/posts/if-at-first-you-dont-succeed-try-try-again-insights-and-llm-informed-tooling-for-detecting-retry-bugs-in-software-systems/)
  - **Dropwise 启发**：两点可抄：(1) **复用现有 `#[tokio::test]`**：提供 `#[dropwise::test]` 或包装器，把已有测试作为场景，配合 C 的“最终态差分”oracle，零手写条件；(2) 用 LLM/静态启发式定位“await 前 take/pop/replace/自增、await 后恢复”的位点，只对这些 future 标 `target`（或自动包 `oblige`）。工作量：(1) 中，(2) 中-大。低垂果实：(1) 是。

**D. 可线性化检查 + “aborted/indeterminate”结果**
- 【经典 1987/1993 → 实现】Knossos 实现了 Wing & Gong 检查器及 Lowe 的扩展；其 WGL 实现把操作分成 ok 与 crashed 两组（ok 占低编号 0..c，crashed 占 c+1..n）— [Knossos WGL API 文档](https://cljdoc.org/d/knossos/knossos/0.3.4/api/knossos.wgl)
- 【近期】Jepsen 文档：crashed（`:info`）操作代价高——每个都持续作为线性化候选，搜索空间可能指数增长（一个 241 个操作的历史达到约 10^34 种可能顺序）；如果操作**确定没有生效**，返回 `:type :fail` 会让检查高效得多并能抓到更多 bug；让错误崩溃（info）仍是合法的 — [Jepsen: checking correctness](https://cljdoc.org/d/io.jepsen/generator/0.1.2/doc/checking-correctness)；[aphyr: Computational techniques in Knossos](https://aphyr.com/posts/314-computational-techniques-in-knossos)
- 【近期】Porcupine（Go）：线性化检查一般是 NP 完全，Porcupine 用启发式，最坏指数时间；带超时时可能返回 `Unknown`，“视使用场景可把 Unknown 解释为 Ok”；作者称其经验上比 Knossos 快数千倍 — [Porcupine pkg.go.dev](https://pkg.go.dev/github.com/anishathalye/porcupine)；[Athalye: Testing distributed systems for linearizability](https://anishathalye.com/testing-distributed-systems-for-linearizability)；[GitHub issue #6](https://github.com/anishathalye/porcupine/issues/6)
- 【经典 2003 / 近期 2016】Strict linearizability（Aguilera & Frølund）：崩溃时 pending 的操作必须在崩溃前线性化或**根本不生效**（“limited effect”）；durable linearizability（Izraelevitz, Mendes, Scott, DISC 2016）更宽松：pending 操作可由崩溃后的恢复过程完成；两者对 pending 操作的“截止时间”不同；strict linearizability 不可组合，durable/recoverable 可组合 — [Survey of Persistent Memory Correctness Conditions, arXiv 2208.11114](https://arxiv.org/pdf/2208.11114)；[Izraelevitz et al. DISC 2016](https://www.cs.rochester.edu/u/scott/papers/2016_DISC_persistence.pdf)。另一篇 2023 论文指出按历史定义的 strict linearizability 实际不能保证 limited effect，并提出 “SLE linearizability” — [The Fence Complexity of Persistent Sets, arXiv 2309.05230](https://arxiv.org/pdf/2309.05230)（两者有争议，应注意）
  - **Dropwise 启发**：回答“能否把‘被取消操作必须原子’当作带 aborted 结果的可线性化来检查”——**能，且理论现成**：被取消的 future 就是 Jepsen 的 `:info` 操作，正确性条件就是 strict linearizability（取消时刻为截止）或 durable linearizability（允许 spawn 出去的后台工作在 settle 内完成）。实际做法：若用户已有顺序模型（proptest-state-machine 的 `ReferenceStateMachine`），把取消操作的结果标为 indeterminate，跑 WGL/Porcupine 式搜索；Dropwise 每次运行取消数很少（`max_cancellations`），因此 info 操作带来的指数爆炸基本不成问题。另一个有价值的区分：Dropwise 的 `Race` 结果应对齐 Jepsen 的 `ok/fail/info`——`Unrealized`/确定未开始的 = fail，`Cancelled` = info。工作量：中（需要历史记录 + 小型 WGL 搜索；有模型时）。低垂果实：**仅对已有顺序模型的用户是**；作为通用默认则不是。strict vs durable 的区分本身是低成本的**概念升级**：它恰好给“检查太早→误报”提供了原则性解释（见问题 4）。

**E. 不变式挖掘（Daikon / DIDUCE）**
- 【经典 1999/2001】Daikon：Ernst, Cockrell, Griswold, Notkin《Dynamically discovering likely program invariants to support program evolution》（IEEE TSE 27(2), 2001；早期版本 ICSE'99）：对捕获的值测试候选不变式，被充分测试且未被证伪者报告给程序员 — [摘要页](https://homes.cs.washington.edu/~mernst/pubs/invariants-tse2001-abstract.html)
- 【经典 2002】DIDUCE（Hangal & Lam, ICSE 2002）：插桩并观察运行，从**最严格的不变式**开始假设，遇到违例就放宽，用户无需写不变式；违例本身即异常报告；在 JSSE 库中隔离了一个时序相关 bug — [ICSE'02 PDF](https://cs.fit.edu/~pkc/id/related/hangal-icse02.pdf)；[Berkeley 摘要](https://people.eecs.berkeley.edu/~fox/summaries/dependable/lam1.html)
  - **Dropwise 启发**：Dropwise 的 baseline 本来就会经过所有 Pending 边界；在每个边界和 settle 后调用用户的 `observe()`（返回若干数值/长度字段），在**静止点**（scenario 返回且 settle 后）上挖掘少量模板：`x == y`、`x + y == 常量`（守恒）、`len(a) == counter`、`x >= 0`。DIDUCE 的“从严到松”适合在 baseline 的多次运行（不同输入）上学习，然后把学到的静止点不变式作为取消后 settle 的检查。重点只挖**静止点**不变式，避开瞬时中间态造成的误报。工作量：中（模板集小即可）。低垂果实：**半是**——守恒/相等这两类模板正中“计数器先加、await 后再配对”的 bug 形状；但单输入 baseline 会学到过拟合的常量不变式，需要多输入或只保留关系型模板。

**F. 状态机 / 基于模型的 PBT**
- 【近期 2023】proptest-state-machine（2023 年 6 月发布）：`ReferenceStateMachine`（抽象参照模型）+ `StateMachineTest`（在真实系统上执行转移），`prop_state_machine!` 生成测试；文档称目前只支持顺序执行转移 — [docs.rs: proptest-state-machine](https://docs.rs/proptest-state-machine/^0.1.0)；[State machine testing with Proptest](https://tzemanovic.codeberg.page/posts/state-machine-testing-with-proptest/)
- 【近期】Hypothesis 有状态测试：`RuleBasedStateMachine`、`precondition()`、`Bundle`、`initialize`（保证在普通规则前恰好运行一次）— [Hypothesis stateful docs](https://hypothesis.readthedocs.io/en/latest/stateful.html)；[Rule Based Stateful Testing](https://hypothesis.works/articles/rule-based-stateful-testing/)
  - **Dropwise 启发**：集成而非重造：提供一个适配器，使 proptest-state-machine 的每个转移可被包为 Dropwise `target`，参照模型在“被取消的转移”上分叉为 {未应用, 已应用} 两个可能参照状态（即 D 的 strict linearizability 在顺序情形下退化为“两状态之一”，无需搜索）。这让已有状态机测试的用户**免写取消专用条件**。顺序情形工作量：小-中。低垂果实：是（对 Rust 生态里已用 proptest 的项目）。

**G. 让历史“自证”的数据设计（Elle）**
- 【近期 2020/2021】Elle（Kingsbury & Alvaro, PVLDB 14(3)）：通过**刻意选择对象与操作**（如追加唯一值的列表）使读返回值揭示版本历史，从客户端观测推断 Adya 风格依赖图，可检测 Adya 形式化中除谓词外的所有异常并给出简明解释 — [arXiv 2003.10554](https://arxiv.org/abs/2003.10554)；[PVLDB](https://www.vldb.org/pvldb/volumes/14/paper/Elle%3A%20Inferring%20Isolation%20Anomalies%20from%20Experimental%20Observations)
  - **Dropwise 启发**：给 Dropwise 的模型（mpsc、Sink）默认使用**唯一、可追溯的负载**（obligation id 已存在），sink 记录追加历史，则可自动推断每条消息“丢失/重复/乱序”，不需要用户写 `check(&sink)`。工作量：小。**低垂果实：是。**

**H. 计数型测试替身（Rust std 内部）**
- 【近期】Rust `alloctests/testing/crash_test.rs` 的 `CrashTestDummy`：创建能记录 `clone`/`drop`/`query` 事件的测试对象（原子计数器，`dropped()` 返回被 drop 次数），并可配置在 clone/drop/query 处 panic — [rust crash_test.rs](https://rust.googlesource.com/rust/+/HEAD/library/alloctests/testing/crash_test.rs)
  - **Dropwise 启发**：提供 `dropwise::Dummy`：用户把它放进自己的队列/缓存/槽位当负载，Dropwise 在 settle 后自动比较 clone/drop 计数与 baseline——多 drop（丢失）或多 clone（重复）即违例，无需理解业务语义。工作量：小。**低垂果实：是。**

### Inferences
- 上述 A/B/C/F 本质上是同一个 oracle 族：**“取消后的可观察结果 ∈ 由 baseline 派生的允许集合”**。允许集合由上下文决定：`target` 且调用者放弃 → {S_pre, S_post}（strict/Witcher）；调用者重试 → {S_final_baseline}（Ramalingam/Hummer）；后台工作可完成 → 在静止点检查（durable）。建议在 Dropwise API 中把这三个设成枚举 `Expect::Atomic | Expect::RetryEquivalent | Expect::EventuallyAtomic`，用户只选上下文、不写条件。这直接针对“条件是读过修复后才写的”这一评估弱点：允许集合来自 baseline，而不是来自对 bug 的知识。
- 快照相等最大的风险是**非确定性噪声**（时间戳、HashMap 顺序、多线程调度）。可以借 DIDUCE 的思路：同一计划跑两次 baseline，自动屏蔽在 baseline 之间就不同的字段（“自差分去噪”），成本很低。
- 半完成态的定位：当某计划的 S ∉ {S_pre, S_post}，对快照做结构化 diff（Debug 树 diff）即可指出哪个字段被“拿走未还”，这比 obligation 报告更贴近 bug 形状。

### Gaps
- 未找到在 async 取消语境中使用“pre/post 双参照快照”的已发表工具（社区里 cancel-safety 讨论多为文档约定）；这可能正是未被连接的空白，但我未做穷尽检索。
- Hypothesis `@invariant` 的确切语义本次检索未能从一手文档确认（检索结果片段不足），未引用。
- QuickCheck/PULSE 的并行状态机 + 线性化（Claessen 等 ICFP 2009）未检索验证，未列入。
- Chen, Cheung, Yiu 1998 年蜕变测试原始技术报告的一手链接未找到，仅能通过 Segura 综述间接引用。

---

## 问题 2：obligation/资源追踪（线性/仿射类型、must-call、Weimer & Necula、Infer、泄漏检测器、goroutine 泄漏）

### Takeaway
各系统对“obligation 已履行”的定义高度一致：**到对象不可达（或方法退出）时，规定方法已被调用，或 obligation 已被显式转移给另一个 owner**；避免误报的关键都不是更强的别名分析，而是少量**所有权转移规则 + 默认值**（构造/返回值 owning、参数不 owning）以及“wrapper 别名”声明。对 Dropwise 最值钱的三件事：(1) 把“await 前 take/pop/replace”自动变成 obligation（借 Weimer 的补偿栈）；(2) 自动模型化 Tokio 自带的两阶段原语（`Permit`、`JoinHandle`、`oneshot::Sender`）；(3) 学 goleak/LeakProf 做**零配置的任务泄漏 oracle**。

### Cited Findings

**A. Checker Framework Resource Leak Checker（must-call 义务）**
- 【近期 2021/2025】Kellogg, Shadab, Sridharan, Ernst《Lightweight and Modular Resource Leak Verification》（ESEC/FSE 2021）：将泄漏检测归约为**累积（accumulation）问题**，无需整程序别名分析即可健全、模块化；must-call 性质 = 规定方法（如 `close()`）必须在对象可能变得不可达的每一点之前已被调用。三项精度技术：轻量所有权转移、专用资源别名分析、对非 final 资源字段重新赋值时创建新 obligation（`@CreatesMustCallFor`）。在广泛部署的软件中发现 49 个真实泄漏；扩展版 RLC# 在 C# 中发现 24 个（含 3 个 Azure 微服务）— [ESEC/FSE'21 PDF](https://homes.cs.washington.edu/~mernst/pubs/resource-leak-esecfse2021.pdf)；[STTT 2025 扩展版](https://homes.cs.washington.edu/~mernst/pubs/resource-leak-sttt2025.pdf)；[ResourceLeakChecker API](https://checkerframework.org/api/org/checkerframework/checker/resourceleak/ResourceLeakChecker.html)
- 所有权转移细则：`@Owning` 是声明注解；对 `@Owning` 左值的伪赋值**转移**右侧的 must-call obligation（视为已满足）；obligation 满足条件 = 已调用方法集合 ⊇ must-call 集合，或作为 owning 返回值返回，或作为 owning 参数传出，或存入 owning 字段。**默认值**：构造器返回总是 owning；未注解方法返回默认 `@Owning`，未注解参数和字段默认 `@NotOwning`——作者称这与观察到的编码模式吻合、降低注解负担，并保证对未注解第三方库的健全处理（从库返回的对象默认被追踪，传给未注解库**不**被视为履行）。`@MustCallAlias` 成对标注 wrapper（如 `BufferedOutputStream(OutputStream)`），关闭 wrapper 即履行底层义务，且该注解被验证而非信任。精度：案例研究中总体精度 29%（49/170），作者认为误报数量“一位开发者几小时可审完”；注解密度约每 1,500 行一个 — [ESEC/FSE'21 PDF](https://homes.cs.washington.edu/~mernst/pubs/resource-leak-esecfse2021.pdf)
  - **Dropwise 启发**：Dropwise 的 `Obligation` 目前只有 `discharge`/`abandon`/drop。可直接借：(1) `Obligation::transfer_to(owner_label)` / `into_owned_by(&TaskId)`：显式转移到某个**仍存活的任务或结构**，settle 后若该 owner 仍在且未 drop 则不算泄漏、owner 被 drop 时 obligation 随之报告——对应“held forever by some task”的误报/漏报区分；(2) **wrapper 别名**：`Obligation::map(f)` 让包装后的值继承同一 id（MustCallAlias），避免用户为了转换类型而 abandon；(3) 默认值哲学：模型交出的 obligation 默认 owning，传进用户未知代码不算履行——Dropwise 已经如此，值得在文档中显式声明。工作量：小。低垂果实：(1)(2) 是。

**B. Weimer & Necula：路径敏感 obligation 分析 + 运行时补偿栈**
- 【经典 2004】Weimer & Necula《Finding and preventing run-time error handling mistakes》（OOPSLA 2004, pp. 419–431）：流敏感、路径敏感分析追踪每条路径上的**未完成 obligation 多重集**；路径（正常或异常）带着未完成 obligation 离开方法即报告违例；**调用一个“履行”方法即使它抛异常也会移除 obligation**（不要求程序在 close 失败时循环重试，以免不必要的误报）；在近 400 万行 Java 中发现 800+ 处错误处理缺陷。分析不健全，但三条**简单过滤规则**消除了遇到的全部误报：(1) 路径经过 `t == null` 判断则移除一个同类型 obligation；(2) 路径上有 `field = t` 赋值则移除一个（对象通常有单独的 cleanup 方法）；(3) 路径上有 `return t` 则移除一个（wrapper，责任归调用者）— [OOPSLA'04 PDF](https://people.eecs.berkeley.edu/~necula/Papers/rte_oopsla04.pdf)
- 补偿栈（compensation stack）设计：程序动作可关联“补偿”（任意闭包），运行时存入一等公民的栈；一旦动作发生，程序不执行补偿就不能结束；栈对象离开作用域或（堆上的栈）被 finalize 时，以**逆序**执行补偿；补偿抛异常则记录并继续；可以**提前执行**某些补偿（尽早释放共享资源），也可以**不执行而显式履行**（成功路径上避免撤销）——因此程序员只需标注少数成功路径而非每条错误路径；可声明额外的栈得到“嵌套事务”效果。其设计明确改编自补偿事务与线性 saga（a1..an 全执行，或 a1..ak ck..c1 执行）。在 34kLOC 程序上改造后代码量减少 0.5%，速度提升 17%，行为更一致 — [OOPSLA'04 PDF](https://people.eecs.berkeley.edu/~necula/Papers/rte_oopsla04.pdf)
  - **Dropwise 启发**：这是对“take 在 await 前、restore 在 await 后”bug 形状最直接的工具化：提供 `dropwise::taken(&mut slot)` / `ctx.compensate(|| restore)` 风格 helper——**拿走状态的同一行就创建 obligation**，成功路径 `commit()` 显式履行，被取消时若 obligation 随 future 一起被 drop 且未 commit/restore，则报告“taken but not restored”。更进一步，Dropwise 可提供 `Option::take` / `mem::replace` / `VecDeque::pop_front` 的插桩版 trait 扩展（`take_obliged()`），让用户只改一个方法名。三条过滤规则对应 Dropwise 动态检查里的豁免：值为 None（未拿到东西）、存入字段（转移到结构）、返回给调用者（转移）。工作量：小-中。**低垂果实：是**（不改 Dropwise 核心，只加 helper；直接降低“手写条件”的负担）。

**C. Infer / Pulse**
- 【近期】Infer 的 changelog 称 Pulse 已默认启用，替代 Biabduction 成为主要内存/值分析引擎，并承担历史上由其他检查器报告的问题类型（空解引用、内存/资源泄漏）；Infer 主页称对 Java/Android 检查资源泄漏，对 C/C++/ObjC 检查内存泄漏 — [Infer Changelog](https://github.com/facebook/infer/blob/main/Changelog.md)；[Pulse 文档](https://fbinfer.com/docs/checker-pulse)；[fbinfer.com](https://fbinfer.com/)
  - **Dropwise 启发**：迁移弱。Infer 的静态路径分析对 Rust async 状态机不可用；唯一可借的是“在每个函数出口（含异常出口）检查资源状态”的思想，而 Dropwise 的动态 drop 检查已覆盖。低垂果实：否。

**D. 线性/仿射类型与 must-use**
- 【近期】Rust 社区关于线性类型的讨论：panic 展开（unwinding）会调用 `Drop::drop`，使“必须被消费”的值难以在 panic 中处理（2015 pre-RFC 已指出这是线性类型唯一的隐式 drop 途径）；Niko Matsakis 的 “must move” 设想通过 `?Drop` bound 实现；Yoshua Wuyts 将线性性重述为“保证其析构函数会被调用的类型”，保留 `Drop` 作为后备；内部讨论 `#[must_destructure]` 探讨“不易被 drop”的类型以支持 async cleanup — [Wuyts: Linearity and Control](https://blog.yoshuawuyts.com/linearity-and-control/)；[Linear Types One-Pager](https://blog.yoshuawuyts.com/linear-types-one-pager)；[internals: #[must_destructure]](https://internals.rust-lang.org/t/exploration-must-destructure-not-easily-dropped-types-async-cleanup/20389)；[pre-RFC linear type modifier](https://internals.rust-lang.org/t/pre-rfc-linear-type-modifier/1225)
  - **Dropwise 启发**：Dropwise 的 `Obligation` 就是“动态线性类型 / drop bomb”——**取消就是 async 中的隐式 drop 路径**，与 panic-unwinding 在语言设计讨论中是同一问题。可借的是论证与文档定位：Dropwise = 在语言没有 must-move 的情况下的测试期替代。也可以提供一个 `#[cfg(test)]` 下才生效的 drop-bomb 类型给用户在生产代码里标注“必须 commit 的守卫”（测试中由 Dropwise 追踪，生产中零成本）。工作量：小。低垂果实：是（设计已近似存在，主要是 API 与文档）。

**E. goroutine 泄漏检测（goleak / LeakProf）**
- 【近期 2022/2024】Uber LeakProf（博客 2022-11-10，Saioc & Chabbi）：三个前提——非平凡泄漏表现为**阻塞在 channel 操作上的 goroutine 数量激增**；少数源码位置贡献大多数泄漏；罕见泄漏可忽略；设计目标是低误报。CGO 2024 论文：LeakProf 从生产 goroutine profile 中一年发现并修复 21 个泄漏（共找到 24 个），部分服务 CPU 降低最多 34%、内存降低 9.2×；单元测试中的 goleak 发现 857 个既有泄漏、阻止约 260 个新泄漏 — [Uber 博客](https://www.uber.com/en-US/blog/leakprof-featherlight-in-production-goroutine-leak-detection/)；[arXiv 2312.12002](https://arxiv.org/abs/2312.12002)；[CGO 2024 页面](https://2024.cgo.org/details/cgo-2024-main-conference/36/Unveiling-and-Vanquishing-Goroutine-Leaks-in-Enterprise-Microservices-A-Dynamic-Anal)
- goleak 实现：`VerifyNone` 在测试结束时若有多余 goroutine 则失败；`IgnoreCurrent` 快照已存在的 goroutine 并排除；内部**重试**：默认最多 20 次，延迟从 1µs 起每次翻倍、上限 100ms；默认忽略测试运行器、syscall、os/signal、trace、DNS resolver 相关栈 — [goleak options.go](https://raw.githubusercontent.com/uber-go/goleak/master/options.go)；[goleak pkg.go.dev](https://pkg.go.dev/go.uber.org/goleak)
  - **Dropwise 启发**：**零配置任务泄漏 oracle**：在 `Flavor::CurrentThread` 下，settle 后读取 Tokio `RuntimeMetrics::num_alive_tasks()`（spawn 时增、任务退出时减；文档提醒多线程运行时一致性不强），与同一计划的 baseline settle 后值比较；取消运行多出的存活任务 = 取消把一个后台任务“孤儿化”（典型：被 drop 的 `JoinHandle` 使任务脱离、永远阻塞在 channel 上）。这正是 LeakProf 的“阻塞在 channel 上的 goroutine 增多”信号的单测版。“baseline 差分”天然实现了 goleak 的 `IgnoreCurrent`（框架/常驻任务在两边都有）。工作量：小（几十行）。**低垂果实：是。** 需注意 `num_alive_tasks` 的 Tokio 版本可用性未核实（见 Gaps）；更细的“哪个任务、阻塞在哪”需要 tokio-console/taskdump，成本更高。

**F. Tokio 自身的两阶段原语（可直接模型化为 obligation）**
- 【近期】Tokio mpsc：`send` 在 `select!` 中输掉时消息未发送但**值被丢弃**，文档建议用 `reserve`；`reserve` 等待容量并返回 `Permit`，通过 permit 发送是同步的，持有 permit 期间 `try_send` 会失败；`send`/`reserve` 按请求顺序完成，取消会失去排队位置；`recv` 是 cancel-safe 的（被取消则未收到消息）— [tokio::sync::mpsc::Sender 文档（镜像）](https://mirror.whynothugo.nl/vdirsyncer/v2.0.0-beta0/tokio/sync/mpsc/struct.Sender.html)；[tokio bounded.rs 源码](https://codebrowser.dev/tokio/tokio/src/sync/mpsc/bounded.rs.html)
  - **Dropwise 启发**：`Permit` 就是数据库里的 reservation（见问题 3）：被持有 = 占用容量的 obligation。可增加 `models::mpsc::Sender` 包装：`send(v)` 的输入值在被取消时被丢弃 → 自动报告“值随取消丢失”（这是 Tokio 文档明确承认的 cancel-unsafe 点）；`reserve()` 返回的 permit 作为 obligation，settle 后仍被持有则报告。同理可模型化 `oneshot::Sender`（必须 send 或显式放弃）、`JoinHandle`（必须 await/abort/显式 detach）、`OwnedSemaphorePermit`。工作量：每个模型小。**低垂果实：是**——扩大“自动 obligation 来源”，直接减少手写 `oblige`。

### Inferences
- “obligation 已履行”的统一定义可写成：在**观察点**（Dropwise 中为 settle 后的静止点），每个 obligation 要么已 discharge/abandon，要么其当前 owner 是一个仍可达、仍可能运行的实体（存活任务/结构），否则为泄漏。现有 Dropwise 的“dropped unresolved / outstanding after settle”两类可据此细分为：**孤儿**（owner 已死）vs **滞留**（owner 存活但永久阻塞）；后者需要静止点判定（问题 4），恰好可以用 goleak 式“阻塞任务数”交叉验证。
- 静态工具（RLC、Weimer、Infer）用来压误报的手段（默认 owning 规则、wrapper 别名、null 检查豁免）在动态工具里都有便宜的对应物，而动态工具没有它们最大的误报源（别名不精确），所以 Dropwise 的主要误报来源会是**时序**（观察太早），而不是所有权——这把投入重点导向问题 4。

### Gaps
- LeakSanitizer/Valgrind 的“可达性”判定细节、Java try-with-resources 的实证研究、typestate（Strom & Yemini 1986）本次未检索到一手来源，未引用；其对 Dropwise 的迁移价值预期较低（Rust 已有 RAII，取消问题不是忘记 close 而是 drop 在错误时机）。
- 未核实 `RuntimeMetrics::num_alive_tasks` 自哪个 Tokio 版本起在非 `tokio_unstable` 下可用；docs.rs 当前文档存在该方法，但版本门槛需查 CHANGELOG — [RuntimeMetrics docs](https://docs.rs/tokio/latest/tokio/runtime/struct.RuntimeMetrics.html)
- Gankra《The Pain of Real Linear Types in Rust》原文未能直接检索到，只看到二手引用。

---

## 问题 3：事务与补偿概念（saga、两阶段/预留、幂等键、exactly-once、outbox、写前意图）——能否自动分类效果、检测“过早提交”

### Takeaway
事务领域给出的不是新 oracle，而是一套**效果分类词汇**：可逆（有补偿）、幂等（可重做）、已提交（不可撤回点之后）。Dropwise 可以**不靠用户标注**、仅靠问题 1 的 pre/post 快照比较，对每个取消计划的结果自动分类为 rolled-back / committed / half-done，并沿 Pending 序号找出“不可撤回点”；“提交早于不可撤回点”= 在操作对调用者仍表现为“已取消”时，post 态效果已经可见（典型后果：调用者重试 → 重复）。

### Cited Findings
- 【经典 1987】Garcia-Molina & Salem《Sagas》（SIGMOD 1987, pp. 249–259, DOI 10.1145/38713.38742）— [dblp](https://dblp.org/rec/conf/sigmod/Garcia-MolinaS87)；[DOI](https://doi.org/10.1145/38713.38742)。按 Weimer & Necula 的转述：saga 是被视为原子动作序列 a1..an、带补偿事务 c1..cn 的长事务，系统保证“要么 a1..an 执行，要么 a1..ak ck..c1 执行”（补偿逆序）；补偿事务在原事务**提交之后**语义上撤销其效果 — [Weimer & Necula OOPSLA'04](https://people.eecs.berkeley.edu/~necula/Papers/rte_oopsla04.pdf)
  - **Dropwise 启发**：saga 的保证就是一个现成的“允许集合”：{S_pre, S_0..k 后接补偿 = S_pre, S_post}。若用户的 async 代码用 drop guard 做补偿，Dropwise 可在取消后检查“状态回到 S_pre 或到达 S_post”（即问题 1 A），并额外检查补偿是否**逆序**执行（记录补偿事件序列）。工作量：小（复用 A）。低垂果实：是（作为 A 的语义解释与文档）。
- 【近期 2013】Ramalingam & Vaswani（POPL 2013）：分布式工作流中，若进程在 debit 与 credit 之间失败，困难在于判断原进程是在某步之前还是之后失败，否则步骤可能重复执行；**幂等性 + 重试**在无需分布式协调的情况下就给出了工作流（原子动作的容错组合）的本质；其 idempotence monad 可扩展支持补偿 — [POPL'13 PDF](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/popl38-ramalingam.pdf)
  - **Dropwise 启发**：这篇的 debit/credit 例子与 Rust 中“await 前扣减、await 后入账”同构，可作为 Dropwise 文档的“bug 形状”理论注脚；其“按请求 id 记录已完成步骤”的 monad 机制对应 Dropwise 可提供的**幂等键 sink 模型**（下条）。
- 【近期】Transactional outbox（microservices.io）：消息中继可能在重启后再次发布消息，因此消费者必须幂等（例如记录已处理消息 id）；由于消息代理本身就可能多次投递，这通常不是问题 — [microservices.io: Transactional outbox](https://microservices.io/patterns/data/transactional-outbox)；MikroORM 文档指出 relay 发布是 at-least-once：worker 在发布后、标记已发送前崩溃即产生重复 — [MikroORM: Transactional Outbox](https://mikro-orm.io/docs/transactional-outbox)
  - **Dropwise 启发**：提供 `models::Sink` 带**幂等键/投递计数**：每个携带 obligation id 的消息被投递的次数记账，settle 后自动判定投递语义是 exactly-once / at-least-once（有重复）/ at-most-once（有丢失），并与用户声明的期望比较（`Delivery::ExactlyOnce` 等）。这把 README 中“received message must be delivered, requeued or explicitly abandoned”从手写条件变成模型内建。工作量：小。**低垂果实：是。**
- 【近期】Tokio `reserve()`/`Permit` 是两阶段“预留→提交”模式：先拿容量（可被取消、取消无副作用但失去排队位置），再同步提交 — [tokio mpsc Sender 文档（镜像）](https://mirror.whynothugo.nl/vdirsyncer/v2.0.0-beta0/tokio/sync/mpsc/struct.Sender.html)
  - **Dropwise 启发**：可把 reserve/commit 推广成 Dropwise 的**诊断建议**：当发现“值在 await 中被持有并随取消丢失”的违例时，报告中建议改写为“先 reserve（可取消，无效果），后同步 commit（无 await）”。这是唯一一种把不可撤回点移到最后一个 await 之后的通用改写。工作量：小（报告文案 + 检测 await 时是否持有被 take 的值）。低垂果实：是（可用性增益）。
- 【近期 2018】FCatch（Liu, Lu 等）：把 time-of-fault（TOF）bug 建模为一种新型并发 bug，**从一次正确执行中预测**，而非依赖故障注入；动机是开发者难以预见故障发生的所有时序和系统状态 — [EuroDW 2018 slides](https://conferences.inf.ed.ac.uk/EuroDW2018/slides/Liu-Haopeng%20-%20Haopeng%20Liu.pdf)；[AMiner 条目](https://www.aminer.org/pub/6218c4b15aee126c0fa1aa64/fcatch)。（venue：检索结果显示 ACM SIGPLAN Notices 53(2), 2018，通常即 ASPLOS'18 论文集；本次未能一手确认 ASPLOS 归属。）
  - **Dropwise 启发**：在 baseline 单次运行中记录每个 Pending 边界处“被 take 但尚未归还/提交的值”（来自问题 2B 的 helper 或 obligation 表）。若某边界处该集合非空，则**预测**在此取消会留下半完成态，优先（甚至只）运行这些计划——作为 `max_runs` 有限时的排序启发。工作量：中。低垂果实：部分（排序启发容易；准确预测较难）。

### Inferences
- **自动效果分类**（无需标注）：对目标操作 op 的第 k 个 Pending 边界的取消计划，取 settle 后快照 S_k：S_k = S_pre → “已回滚/未生效”；S_k = S_post → “已提交”；其他 → “半完成”（违例）。沿 k 递增，结果序列在正确代码中应是单调的 `pre…pre post…post`，**翻转点就是不可撤回点**（point of no return）。两类可报告问题：(a) 出现“半完成”；(b) 翻转点之后结果为 post 但调用者看到的是“已取消”（`Cancelled`）——若调用上下文会重试（问题 1C 的 `retry`），则必然重复（at-least-once 而非 exactly-once）。若再做一次“取消后重试”运行，可进一步把效果标为**幂等**（重试后仍 = S_post）或**非幂等**（重试后 ≠ S_post）。这实现了用户问的“自动将效果分类为 reversible/idempotent/committed 并检测提交早于不可撤回点”，全部由 baseline 派生。
- “可逆”需要补偿实际被执行；在 Dropwise 中补偿通常是 drop guard 或 spawn 的清理任务，因此“可逆”的判定与 settle 时机强相关（补偿可能在后台任务里异步运行）——这又回到问题 4：在静止点而非取消瞬间观察，对应 durable 而非 strict 语义。
- 事务领域的 write-ahead intent（先记录意图、再执行、再标记完成）对 Dropwise 的映射与 outbox 相同：可提供 `IntentLog` 模型，settle 后要求每条 intent 处于 {未开始, 已完成, 已显式撤销}，与 obligation 三态（未产生 / discharge / abandon）同构，无需新机制。

### Gaps
- Sagas 原文未能获取全文，以上 saga 保证的表述来自 Weimer & Necula 的二手转述。
- Helland《Idempotence Is Not a Medical Condition》（ACM Queue 10(4), 2012）只确认了引用信息，未读到全文；Kafka exactly-once（KIP-98）的测试方法、O'Neil escrow 事务（1986）本次未检索，未引用。
- 未找到任何把“不可撤回点”自动推断应用于 async 取消的已有工作。

---

## 问题 4：瞬时 vs 持久违例——何时观察（静止检测、最终一致性、flaky test 等待、三值判决）

### Takeaway
已有一个与 Dropwise 场景几乎一模一样的工业级先例：Go `testing/synctest` 的 `Wait()`——**等到气泡内所有其他 goroutine 都“持久阻塞”（只能被气泡内其他 goroutine 唤醒）再观察**，并且只在全部持久阻塞时才推进虚拟时间。Tokio 的 paused-time auto-advance 本身已经在内部判定“运行时无事可做”。Dropwise 应把固定的 `Settle::tokio_time` 窗口换成“**静止点**”判定，并在逻辑上把检查从“取消瞬间”（strict）移到“静止点”（durable），对仍有外部未决工作的情况给出第三值“inconclusive”（已有 `unsettled` 雏形）。

### Cited Findings
- 【近期 2024–2026】Go `testing/synctest`：气泡内 goroutine“持久阻塞”= 阻塞且只能被同一气泡内另一个 goroutine 解除；能被气泡外事件解除的不算。`Wait` 阻塞到气泡内所有其他 goroutine 都持久阻塞为止。持久阻塞的操作包括：气泡内创建的 channel 的收发与 select、`sync.Cond.Wait`、`sync.WaitGroup.Wait`（Add 在气泡内调用时）、`time.Sleep`；**mutex、网络 I/O、系统调用明确不算**。当所有 goroutine 持久阻塞时：若有人在 `Wait` 则返回；否则时间推进到下一个能解除至少一个 goroutine 阻塞的时刻；若不存在这样的时刻，则判定死锁并 panic — [pkg.go.dev: testing/synctest](https://pkg.go.dev/testing/synctest)；相关 Go 提案 issue 65336 引用了 Tokio“运行时无事可做时自动推进模拟时钟”的先例 — [golang/go#65336](https://golang.org/issue/65336)
  - **Dropwise 启发**：把 settle 定义为：**(1) 无可运行任务；(2) 无在途 spawn_blocking；(3) 下一个定时器的触发时间超出“观察视界”**（或没有定时器）→ 静止点。在静止点检查；如果到达静止点前有“外部可唤醒”的阻塞（真实 I/O、线程、spawn_blocking），则标为 inconclusive 而非 pass——与 synctest“mutex/I/O/syscall 不算持久阻塞”同一原则。synctest 的“全部持久阻塞且无未来定时器 → 死锁”还可以给 Dropwise 一个**免费的新违例类别**：取消后静止且某 obligation 的 owner 任务永久阻塞 = “滞留”而非“暂未完成”。工作量：中（需要读取运行时空闲状态，见下条）。**低垂果实：是（概念）/ 中等（实现）**。
- 【近期】Tokio paused time：时间暂停且运行时**无事可做**时，时钟自动推进到下一个挂起的定时器；需 `current_thread` 运行时；有 `spawn_blocking` 任务运行时自动推进被抑制（维护者确认“有任何 spawn_blocking 任务时时间不推进”）；PR #6113 曾提议一个禁用自动推进的开关（本次未确认是否合并）— [tokio::time::pause 文档（镜像）](https://dev.materialize.com/api/rust/tokio/time/fn.pause.html)；[tokio clock.rs 源码](https://codebrowser.dev/tokio/tokio/src/time/clock.rs.html)；[tokio PR #6113](https://redirect.github.com/tokio-rs/tokio/pull/6113)
- 【近期】Tokio `RuntimeMetrics::num_alive_tasks`：spawn 时增、任务退出时减；多线程运行时下一致性不强。Tokio issue #6696 指出目前没有可靠方法判断某 worker 是在 park（空闲）还是在运行但卡住 — [RuntimeMetrics docs](https://docs.rs/tokio/latest/tokio/runtime/struct.RuntimeMetrics.html)；[tokio #6696（镜像）](https://hub.harborframework.com/tasks/abundant/tokio-rs__tokio-6696)
  - **Dropwise 启发**：在 `CurrentThread + paused` 下，“自动推进发生”本身就是静止信号：Dropwise 可以用 `tokio::time::Instant::now()` 的跳变 + `sleep(ZERO)`/`yield_now` 探针检测“运行时一度无事可做”，配合 `num_alive_tasks` 与 obligation 表在连续两个静止点之间**不变**来确认稳定。README 已提到 paused time 在 spawn_blocking 期间不推进，这正好使“静止点”自动包含“无阻塞池工作”。多线程 flavor 下无可靠静止判定（#6696），应维持 inconclusive。工作量：中。低垂果实：对 CurrentThread 是。
- 【近期】goleak 的“指数退避重试直到 goroutine 退出”（1µs 起翻倍，上限 100ms，最多 20 次）是工业界对“观察太早导致泄漏误报”的实用解法 — [goleak options.go](https://raw.githubusercontent.com/uber-go/goleak/master/options.go)
  - **Dropwise 启发**：对 `Settle::real_time`（Tokio 视野外的线程/内核工作）直接照抄：不再固定等待，而是指数退避地重复检查“obligation 表 + 快照”直到连续两次不变或达到上限；达到上限仍在变 → inconclusive。工作量：小。**低垂果实：是。**
- 【近期 2014】Luo, Hariri, Eloussi, Marinov《An Empirical Analysis of Flaky Tests》（FSE 2014）：研究 51 个开源项目中 201 个可能修复 flaky 测试的提交；async wait 类中约三分之一的测试用带时间延迟的普通调用来强制顺序，作者建议改变这些延迟往往能暴露问题。（二手资料常引“async wait 占 45%”，本次未在原文中核实）— [FSE'14 PDF](https://huang.isis.vanderbilt.edu/cs8395/paper/flakytest.pdf)
  - **Dropwise 启发**：给固定窗口 settle 提供证据级动机：sleep 式等待是 flaky 的头号根因之一。可借其“改变延迟以暴露”的建议做**窗口敏感性自检**：同一计划用 settle 窗口 T 与 2T 各跑一次，结论不同 → 该违例是瞬时的（或窗口不足），自动降级为 inconclusive 并报告。工作量：小（计划数 ×2，可只对出现违例的计划复跑）。**低垂果实：是**——直接针对“早观察误报”。
- 【近期 2011】Bauer, Leucker, Schallhart《Runtime Verification for LTL and TLTL》（ACM TOSEM 20(4), 2011）：对有限观测前缀采用**三值语义**（true / false / inconclusive），给出最小且尽早判决的监视器构造，可监视的性质集合严格大于 safety 与 co-safety — [Lübeck 研究门户](https://research.uni-luebeck.de/en/publications/runtime-verification-for-ltl-and-tltl/)
  - **Dropwise 启发**：给“瞬时 vs 持久”一个形式化口径：“obligation 最终被处理”是 liveness（◇resolved），在有限窗口内只能得出 false（已被 drop 未处理——安全性违例，可立刻判）或 inconclusive（仍未处理但仍有 owner 存活）；只有在静止点且无未来事件时 inconclusive 才能升级为 false（因为之后不会再发生任何事）。这正好说明：**“dropped unresolved”可以即时判定，“outstanding”只能在静止点判定**——Dropwise 现有两类泄漏应采用不同观察策略。工作量：小（主要是语义整理与报告分级）。低垂果实：是（概念/报告层）。
- 【经典/近期】durable linearizability 允许 pending 操作由恢复过程在崩溃后完成，而 strict linearizability 要求在崩溃前完成或不生效 — [arXiv 2208.11114](https://arxiv.org/pdf/2208.11114)
  - **Dropwise 启发**：两种口径对应两种观察时机：strict = 在取消返回给调用者时观察（适合“调用者马上复用连接/状态”的上下文）；durable = 在静止点观察（适合“后台任务会收尾”的上下文）。允许用户按上下文选择，等于把“settle 窗口多长”这一启发式问题，换成“调用者在何时依赖该状态”这一语义问题。

### Inferences
- 推荐的观察算法（CurrentThread）：取消 → 驱动运行时直到首个静止点 Q1（无可运行任务、无 blocking 工作、下一定时器超出视界或不存在）→ 检查 safety 类（dropped unresolved、快照 ∉ 允许集合中**仅限不可恢复字段**）→ 允许时间推进到下一个定时器、再到静止点 Q2… 直到 obligation 表与快照在连续两个静止点不变（不动点）或达到视界 → 此时 outstanding 视为持久违例；外部可唤醒阻塞未清 → inconclusive。这把 `Settle::tokio_time` 从“时长”变为“视界（最多跨越几个定时器/多长虚拟时间）”，并且对周期性心跳任务也稳健（心跳会使快照不变而定时器始终存在，所以需要“不动点”而非“无定时器”判据）。
- 视界的默认值可以从 baseline 自适应：baseline 达到不动点所需的虚拟时间 × 常数因子，比全局固定窗口更不易误报/漏报。
- 分布式系统里的“最终一致性检查”（如 Jepsen 在恢复故障后做 final reads）与上述做法同构，但本次未检索到一手来源（见 Gaps）。

### Gaps
- Jepsen“修复故障后等待并做 final read”的具体文档、Bailis 等 PBS（probabilistic bounded staleness）、Dijkstra–Scholten / Mattern 分布式终止检测本次均未检索一手来源，未引用；其中终止检测算法对单进程 Tokio 运行时迁移价值有限（运行时本地即可知空闲状态）。
- Tokio 是否提供公开 API 直接查询“当前 current_thread 运行时是否空闲 / 下一个定时器时刻”未确认；若无，需要通过 auto-advance 跳变间接探测或使用 `tokio_unstable` 指标，存在实现风险。
- 未找到 flaky-test 文献中关于“选择观察时刻”的原则性方法（大多为经验修复模式），Luo 等的“45% async wait”数字未在原文核实。
