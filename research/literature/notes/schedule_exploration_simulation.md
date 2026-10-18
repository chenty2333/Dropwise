# 调度探索、系统化并发测试与确定性仿真：可迁移到 Dropwise 的机制

> 范围：1997–2026 年的 systematic concurrency testing（SCT）、bounded/probabilistic 调度探索、覆盖率引导的调度 fuzzing、工业界 deterministic simulation testing（DST）、显式探索时序（timer、事件顺序、select）的工作，以及模式层面的思想（bug depth、small scope、swarm testing）。
> 每条后面的「→ Dropwise 启发」写明：借什么机制、粗略工作量（S = 几天以内 / M = 1–3 周 / L = 一个月以上或要改架构），以及是不是低垂果实。迁移性弱的地方直接说弱。
> 年代标记：〔年份〕；2023 年及以后标「新」。
> Dropwise 当前语义（来自仓库 README）：plan = 哪个 target 输、在第几个观察到的 `Pending` 之后输、竞争分支何时 ready（Immediate / Reschedule(n) / After(d) / AfterWake）；按广度优先在 `max_cancellations` 以内枚举；运行后 settle，检查 invariant 和 Obligation；没到达的 boundary 报为 unrealized。`Flavor::MultiThread` 用真实时间，Pending 次数会变，表现为 unrealized。

---

## Q1 系统化/有界探索（CHESS、preemption/context/delay bounding、PCT、POS、DPOR、SMC、loom、shuttle）：哪些 bounding/优先级思想能迁移，能否给出「深度 d 的 bug 以 ≥p 的概率被找到」这样的保证？

### Takeaway
Dropwise 的 `max_cancellations` + 广度优先，本质上就是 CHESS 的 iterative context bounding。真正还没借过来的有三样：(1) PCT/POS/SURW 那套「随机采样 + 每轮下界概率」，可以给超大 plan 空间一个可以写进报告的保证；(2) DPOR/sleep-set 风格的等价归并，用来压缩「同一 `.await` 被 poll 成 Pending 很多次」的 boundary；(3) 实证结果：bound 取 1–2 就能抓到大多数 bug，朴素随机也出奇地强。这样默认预算就有了依据。

### Cited Findings
- **VeriSoft / stateless model checking**〔1997〕：Godefroid 提出不保存状态、只靠重放来系统化探索真实程序的调度。— [Godefroid, POPL 1997](https://dl.acm.org/doi/10.1145/263699.263717)
  → Dropwise 启发：Dropwise「每个 plan 重跑一次」本身就是 stateless 探索，所以 SMC 文献里的「重放前缀开销」「非确定性破坏重放」两个老问题，Dropwise 都会原样遇到（见 Q3 的确定性自检）。概念上的确认，不需要新工作。
- **Context bounding**〔2005〕：Qadeer & Rehof 证明在有界 context switch 下，带过程调用的并发程序模型检查可判定，并主张很多 bug 只需要少量 context switch。— [Qadeer & Rehof, TACAS 2005](https://link.springer.com/chapter/10.1007/978-3-540-31980-1_7)
  → Dropwise 启发：`max_cancellations` 就是「cancellation bound」。报告里可以像 CHESS 那样写成「在 bound=c、时序集合 T 下穷尽」。几乎零成本。
- **Iterative context bounding / CHESS**〔2007, 2008〕：Musuvathi & Qadeer 按 preemption 数从 0 开始逐层加大来做系统化测试（PLDI 2007）；CHESS 把它用在真实 Windows 代码上，找并复现 Heisenbug（OSDI 2008）。— [Musuvathi & Qadeer, PLDI 2007](https://www.microsoft.com/en-us/research/publication/iterative-context-bounding-for-systematic-testing-of-multithreaded-programs/)；[Musuvathi et al., OSDI 2008](https://www.usenix.org/legacy/event/osdi08/tech/full_papers/musuvathi/musuvathi.pdf)
  → Dropwise 启发：Dropwise 的 BFS 已经是 ICB。还能借 CHESS 的「逐层覆盖声明」：报告按层给出 `c=1: 全部 N1 个 plan 跑完；c=2: 跑了 N2/M2`，预算用完时说清楚哪一层不完整，而不是只给一个 `!exhaustive`。工作量 S，是低垂果实（主要是报告格式）。
- **Delay bounding**〔2011〕：Emmi、Qadeer、Rakamarić 把调度器的非确定性表示为「确定性调度器 + 有限次 delay（跳过它本来要选的任务）」。限制 delay 次数后，只要探索很少的调度就能找到 bug。— [Emmi et al., POPL 2011](https://www.microsoft.com/en-us/research/publication/delay-bounded-scheduling-2/)
  → Dropwise 启发：这和 Dropwise 的模型几乎同构：Tokio CurrentThread + paused time 就是「确定性调度器」，`Reschedule(n)` 就是给竞争分支 n 次 delay。可以把所有 `Reschedule(n)`、`After(d)` 统一成一个 delay 预算，探索「总 delay ≤ D」的 plan，不必为每个 n 单独列 timing。工作量 S–M；把时序维度参数化有价值，但不紧急。
- **SCTBench 实证**〔2014/2016〕：Thomson、Donaldson、Betts 在 49 个（PPoPP 版为 52 个）真实 bug 上比较了 DFS、preemption bounding、delay bounding、受控随机和 PCT。PCT(d=3) 找到其他技术找到的全部 bug 外加 3 个；delay bounding 优于 preemption bounding；朴素随机比 preemption bounding 找到的多，只比 delay bounding 少 2 个；大多数 bug 在 bound 1–2 就能暴露；要做 SCT 先得消除非确定性、控制所有同步，这一步「不简单」。— [Thomson et al., TOPC 2016](https://www.doc.ic.ac.uk/~afd/papers/2016/TOPC.pdf)；[PPoPP 2014 版](https://multicore.doc.ic.ac.uk/publications/ppopp-14.html)
  → Dropwise 启发：(a) 默认 `max_cancellations=1` 先穷尽、再对 2 做采样，有实证依据；(b) 应该加一个「随机 plan」模式作为便宜基线，SCTBench 显示随机往往够用。工作量 S，是低垂果实。
- **PCT**〔2010〕：Burckhardt、Kothari、Musuvathi、Nagarakatte 定义 bug depth = 暴露 bug 所需的最少调度约束数。对 n 个线程、k 步的程序，PCT 单次运行找到深度 d 的 bug 的概率 ≥ 1/(n·k^(d-1))；d=1、2 时分别是 1/n 和 1/(nk)。— [Burckhardt et al., ASPLOS 2010](https://people.cs.rutgers.edu/~sn349/papers/asplos2010.pdf)；[MSR 页面](https://www.microsoft.com/en-us/research/?p=158870)
  → Dropwise 启发：可以给 Dropwise 定义「cancellation bug depth」：需要的取消数 c，加上每次取消需要的约束（哪个 boundary、哪种时序）。随机模式下，分层采样（先均匀选 target，再选 boundary，再选 timing）对一个特定的单取消 bug，每次运行的命中概率 ≥ 1/(|targets|·K_max·|T|)，c 次取消就是这个数的 c 次方。报告里可以直接打印「若存在单取消 bug，R 次未命中的概率 ≤ (1−p)^R」。工作量 S，是低垂果实（这是推导，不是文献结论，见 Inferences）。
- **PCTCP（消息传递/分布式版 PCT）**〔2018〕：Ozkan、Majumdar、Niksic 等人在执行中在线揭示的偏序事件上做随机调度。找到深度 d 的 bug 的概率 ≥ 1/(w²·n^(d-1))，w 是偏序宽度，n 是偏序大小。在 ZooKeeper 和 Cassandra 上找到 bug，获 OOPSLA 2018 Distinguished Paper。— [Ozkan et al., OOPSLA 2018](https://2018.splashcon.org/event/splash-2018-oopsla-randomized-testing-of-distributed-systems-with-probabilistic-guarantees)
  → Dropwise 启发：async 任务之间更像消息传递（waker、channel），不太像共享内存线程。概率界里的「宽度 w」对应同时 pending 的 target 数。启发是：多目标取消时按偏序宽度而不是总步数来设计采样，界会好得多。工作量 M；理论意义大于工程意义。
- **POS**〔2018〕：Yuan、Yang、Gu 给每个事件随机优先级，每执行一个事件，就给所有与它竞争的事件重新随机一次优先级。他们证明这比之前的采样方法有更好的检错概率保证，并在 SpiderMonkey 等真实程序上找到 bug。— [Yuan et al., CAV 2018](https://www.cs.columbia.edu/~junfeng/papers/pos-cav18.pdf)
  → Dropwise 启发：核心教训是「朴素随机游走会偏向步数多的分支」。对 Dropwise 来说，一个在重试循环里 Pending 1000 次的 target，如果对 (target, boundary) 均匀采样，会吸走绝大部分预算。应该按 target 分层采样，或者在等价类上采样（见 DPOR 一条）。工作量 S。
- **SURW**〔2025·新〕：Zhao、Wolff、Mathur、Roychoudhury 只在用户/工具选定的「interesting events」子集上，对交错做均匀采样。在标准 benchmark 上比同类随机算法找到更多 bug、找得更快。— [Zhao et al., ASPLOS 2025](https://www.comp.nus.edu.sg/~umathur/papers/surw-asplos25.pdf)
  后续的 Fray 发现：KAFKA-17379 一例有 91 个共享内存位置，随机选 20 个当 interesting 抓不到 bug，人工标出相关状态后 SURW 才抓到。结论是「怎么选 interesting events」才是关键。— [Fray, arXiv 2501.12618](https://arxiv.org/pdf/2501.12618)
  → Dropwise 启发：Dropwise 的「marked target」就是 SURW 的 interesting events，人工标注在这里是优点，不是缺点。可以借鉴「只在 interesting 子集上均匀」：只在 Obligation 持有期间的 boundary 上采样或穷尽。工作量 S–M。
- **Fray**〔2025·新〕：Li 等人做的 JVM 平台默认用 POS，同时支持 Random、PCT、POS、SURW，加一个新算法约 200 行代码。在 Kafka/Lucene/Guava 的 2,664 个现有测试上，Fray-Random 让 291 个测试失败，POS/SURW 各 360+（平均分别要 190 次和 82 次迭代）；rr chaos mode 只有 5 个；JPF 一个都跑不起来。上报 18 个 bug，16 个已确认。作者强调「applicability」（能不能直接跑在真实测试上）比搜索算法更重要。— [Li et al., OOPSLA 2025, arXiv 扩展版](https://arxiv.org/pdf/2501.12618)；[SPLASH 页面](https://2025.splashcon.org/details/OOPSLA/56/Fray-An-Efficient-General-Purpose-Concurrency-Testing-Platform-for-the-JVM)
  → Dropwise 启发：这和 Dropwise 可行性研究的结论（真实项目里取消点多是 combinator 或自定义 timeout wrapper）一致：先解决「能挂到现有测试上」，再谈搜索算法。Fray 对 timed wait 的处理和「不太可能发生」的问题见 Q4。
- **DPOR 与 optimal DPOR**〔2005, 2014〕：Flanagan & Godefroid 在运行中动态找出相互依赖的操作，只回溯会改变结果的交错（POPL 2005）。Abdulla 等人给出每个 Mazurkiewicz 等价类只探索一次的 optimal DPOR（POPL 2014）。— [Flanagan & Godefroid, POPL 2005](https://dl.acm.org/doi/10.1145/1040305.1040315)；[Abdulla et al., POPL 2014](https://dl.acm.org/doi/10.1145/2535838.2535845)
  → Dropwise 启发（重点）：「取消的偏序归约」。对同一个 target，第 j 和第 j+1 个 Pending boundary 之间如果没有发生可观测效果（没有 Obligation 创建或 resolve、没有用户标记的 effect、models 里没有消息出入队），在两处取消得到的外部状态通常等价。可以在每个 boundary 记一个廉价的 effect 签名（例如 obligation ledger 计数 + 用户 `ctx.effect()` 计数），每个签名等价类只取第一个 boundary 去取消，报告里写「K 个 boundary 归并成 m 类」。这直接缓解 e2e 测试里 Pending 次数爆炸的问题。注意 target 内部状态（例如部分读入的缓冲）可能在没有外部效果时也变了，所以这是启发式，不是可靠的归约。工作量 M，是低垂果实（价值很高）。
- **事件驱动程序的 SMC**〔2015〕：Jensen、Møller、Raychev、Dimitrov、Vechev 把 stateless model checking 用到事件驱动应用上。— [Jensen et al., OOPSLA 2015](https://dl.acm.org/doi/10.1145/2814270.2814282)
  → Dropwise 启发：async 执行器是事件驱动的，这条线说明可以把「回调/事件」当作调度单元做 DPOR，不必用线程。迁移性中等；本次没有复核细节（见 Gaps）。
- **loom**〔Rust〕：loom 实现了 CDSChecker 论文的技术，用状态归约避免组合爆炸，并支持 preemption bound（`LOOM_MAX_PREEMPTIONS`），文档说设成 2–3 就能抓到大多数 bug。Tokio CI 用 2，用 1 当快速冒烟测试。另有文献指出 preemption bound 和 DPOR 很难组合：直接用 bound 剪枝 DPOR 是不 sound 的。— [loom docs](https://docs.rs/crate/loom/^0.3.1)；[Tokio CONTRIBUTING 相关 PR](https://github.com/tokio-rs/tokio/pull/6515)；[MPI-SWS FMCAD 2023](https://www.mpi-sws.org/~viktor/papers/fmcad2023-rounder.pdf)
  → Dropwise 启发：迁移性弱。loom 针对的是 atomics/内存模型层面的交错，不是 future drop。唯一能借的是那个提醒：上面的「effect 等价归并」和 `max_cancellations` bound 叠在一起时，可能丢掉某些 plan，所以报告里要把两者分开计数。
- **shuttle**〔Rust〕：AWS 的 shuttle 实现了「一批随机化并发测试技术」，并引用了 PCT。它明确不 sound，「不是穷尽检查器」，但能比 loom 扩展到大得多的测试；提供 wrapper 方便测试用了 `tokio`、`rand` 的代码。— [awslabs/shuttle](https://github.com/awslabs/shuttle)
  → Dropwise 启发：Dropwise `MultiThread` 模式的问题（真实时间、Pending 次数不稳定）本质上是缺一个受控调度器。长期方案是让场景跑在 shuttle 的 async 执行器上，把「cancellation plan × PCT 调度」做成乘积空间，概率界相乘。工作量 L，需要用户代码可以替换运行时，不是低垂果实。

### Inferences
- 「取消深度」的一个可操作定义（推导，非文献结论）：深度 = 需要的取消次数 c + 每次取消需要的时序约束数。Immediate/After(d) 约束的是竞争者相对 target 的就绪时刻；AfterWake 多一条约束：「target 已被唤醒但还没被 poll」，所以深度 +1。Notify lost-wakeup 只有多时序探索才能找到，符合「深度 ≥2 的 bug 单一时序必漏」的预期；io_uring fd 泄漏一个朴素基线就够，符合「深度 1 的 bug 随便找都能找到」。PCT 框架正好解释了 Dropwise 已有的两个实验结果。
- PCT 的优势来自「plan 空间大、预算小」的情形（重型 e2e 测试正是如此）；小场景下穷尽 BFS 更好。所以合理组合是：c=1 穷尽，c≥2 用分层随机采样 + 打印概率界。
- DPOR 风格的 effect 签名归并，加上 POS「避免偏向长分支」的教训，是这组文献里对 Dropwise 最直接、最便宜的一项。

### Gaps
- 没有找到任何文献把 PCT/DPOR 用在「future 被 drop 的位置」这个选择空间上；Rust 异步取消测试工具在 2025 年仍缺位（LWN 对 RustConf 2025 Rain 演讲的报道说，现有技术「没有一个是系统性解决方案」）。— [LWN](https://lwn.net/SubscriberLink/1036924/c522bdeea0cff791)
- OOPSLA 2015 事件驱动 SMC 的 bounding 细节（例如 conflict-reversal bounding）本次未复核全文。
- Fray 论文给出了各算法的整体数字，但没有给出针对「协程/单线程交错」的结果（作者承认 Fray 无法交错单线程协程）。

---

## Q2 覆盖率引导/反馈驱动的调度 fuzzing：现有的「交错覆盖率」指标里，哪些能变成「取消覆盖率」？

### Takeaway
并发 fuzzing 的反馈信号都是「行为对」或「状态摘要」：GFuzz 用 channel 操作对、channel 状态和满度，Krace 用 alias 对，RFF 用 reads-from 对，Mallory 用 happens-before 摘要，FIFUZZ 用「错误点 + 调用上下文」。Dropwise 最该借的是三种：(1) 用「取消点 × 调用上下文 × 取消瞬间持有的 Obligation 集合」做 cancellation-state coverage；(2) 用「只在被取消的运行里执行到的代码」(cancellation-induced coverage)衡量清理路径；(3) 用 Antithesis 风格的 `sometimes` 可达性断言，把「unrealized plan」推广成用户自定义的覆盖目标。

### Cited Findings
- **GFuzz（Go select 重排）**〔2022〕：Liu、Xia、Liang、Song、Hu 把每个 `select` 的各个 case 视为并发消息，用三元组 (s_i, c_i, e_i)（第几个 select、case 数、选中的 case）标识一种顺序，把 `select` 改写成强制先执行某个 case 的结构，每种顺序最多执行一次。反馈打分用 channel 运行时信息：channel 操作的交错（操作对）、channel 创建/关闭、未关闭 channel 数、满的 channel 数；分高的顺序被优先变异。运行时 sanitizer 跟踪 channel 引用在 goroutine 间的传递：如果某个阻塞的 goroutine 等的 channel 已没有其他 goroutine 持有，就判为阻塞 bug。在 Docker、Kubernetes、gRPC 等 7 个项目找到 184 个新 bug（170 个阻塞型，14 个非阻塞型），12 个误报；为了防死锁引入等待时间，开销约 3 倍，每秒约 0.62 个单元测试。局限：只考虑同一个 select 内的 case；改变不了返回值。— [Liu et al., ASPLOS 2022（NSF PAR 全文）](https://par.nsf.gov/servlets/purl/10321048)；[Penn State 记录](https://pure.psu.edu/en/publications/who-goes-first-detecting-go-concurrency-bugs-via-message-reorderi/)；[USTC 中文解读](https://s4plus.ustc.edu.cn/_upload/article/files/04/36/b629286e4b6094e4cb40059c2af2/6722b815-04b7-4bea-a6e8-a3cb5edaaa9a.pdf)
  → Dropwise 启发（三点）：(1) GFuzz 的 sanitizer 和 Dropwise 的「Obligation 被某个任务永远持有」是同一类 oracle，可以借它的思路：settle 后如果某个 Obligation 只被已阻塞且不可唤醒的任务持有，直接报「永久泄漏」，不必等 watchdog。工作量 M。(2) 打分信号对应到 Dropwise：取消瞬间 channel/队列深度（用 models 的 mpsc 记录）、未 resolve 的 Obligation 数、被取消后还有多少 waker 注册着。工作量 S–M。(3) 「每种顺序最多跑一次」的去重表，Dropwise 已经通过 plan 枚举做到了。
- **RFF（reads-from 引导的 greybox 并发 fuzzing）**〔2024·新〕：Wolff、Shi、Duck、Mathur、Roychoudhury 用 reads-from 关系引导有偏随机搜索，每个调度找到的 bug 显著多于其他工具，靠更均匀的探索严格优于随机基线。— [Wolff et al., ASPLOS 2024](https://www.comp.nus.edu.sg/~umathur/papers/rff-asplos24.pdf)；[RFF 代码](https://github.com/dylanjwolff/RFF)
  → Dropwise 启发：对应的取消版 reads-from 是「谁观察到了取消的后果」：被 requeue 的消息被哪个任务、在哪个 site 收到；被放弃的 Obligation 由谁 resolve。在 models 里记录 (cancel_site, observer_site) 对，作为新颖性信号，决定 c=2 时优先组合哪些 plan。工作量 M；只在 random/guided 模式下有用。
- **Krace（alias coverage）**〔2020〕：Xu、Kashyap、Zhao、Kim 用新的 alias coverage 衡量并发维度上的进展，用演化算法生成多线程 syscall 序列，在 ext4/btrfs/VFS 找到 23 个 data race，其中 9 个确认有害。— [Krace, IEEE S&P 2020（见 OS fuzzing 综述引用）](https://arxiv.org/pdf/2502.13163)
  → Dropwise 启发：alias coverage 统计的是「两个线程访问同一内存」的对。取消版可以是「被取消的 future 和存活的任务触碰同一个共享资源」的对（同一个 `Arc<Mutex<_>>`、同一个 Obligation 队列）。需要插桩，工作量 M–L；迁移性中等。
- **Muzz、Snowboard、PERIOD 等**〔2020–2022〕：Muzz 做线程感知的 greybox fuzzing（USENIX Security 2020）；Snowboard 按潜在的线程间内存通信聚类，挑选并发输入（SOSP 2021）；PERIOD 把并发执行建模为周期性执行，结合已测交错来引导调度，在 10 个 CVE 和 36 个 benchmark 上优于其他 CCT 技术，找到 5 个新 bug（ICSE 2022）。— [Muzz](https://www.usenix.org/conference/usenixsecurity20/presentation/chen-hongxu)；[Snowboard](https://dl.acm.org/doi/10.1145/3477132.3483549)；[PERIOD, ICSE 2022](https://conf.researchr.org/details/icse-2022/icse-2022-papers/140/Controlled-Concurrency-Testing-via-Periodical-Scheduling)；[PERIOD 工具页](https://sites.google.com/view/period-cct/)
  → Dropwise 启发：共同模式是「先跑一次 profile 出候选通信对，再只对候选做调度」。Dropwise 的 baseline run 已经在做 profile（记录 Pending）。可以多记一样：每个 boundary 时刻持有哪些 Obligation、哪些 waker 注册在哪。下一节的覆盖率就建立在这上面。（Muzz、Snowboard 的细节本次未复核全文，只用到其大意。）
- **Mallory（分布式系统 greybox fuzzing）**〔2023·新〕：Meng、Pîrlea、Roychoudhury、Sergey 在运行时构建 Lamport timeline，抽象成 happens-before 摘要作为反馈，用 Q-learning 决定注入什么故障。CCS 演讲摘要称，比 Jepsen 24 小时内多探索 54.27% 的不同状态，发现 22 个零日 bug；但需要人工标注重要代码块。— [Meng et al., CCS 2023, arXiv](https://arxiv.org/pdf/2305.02601)
  → Dropwise 启发：「happens-before 摘要当状态」可以简化成「取消后 settle 期间发生的事件序列的哈希」（例如 Obligation resolve 的顺序、models 的 requeue 事件）。哈希不同的 plan 就是「行为不同」，可以用来去重报告、挑 c=2 的组合。RL 部分不建议引入（工作量 L，收益不确定）。
- **FIFUZZ（上下文敏感的错误注入）**〔2020〕：Jiang、Bai 等人在注入错误时区分错误点的调用上下文（同一个错误点从不同调用栈到达算不同的点），以此 fuzz 错误处理代码。— [FIFUZZ, USENIX Security 2020](https://www.usenix.org/conference/usenixsecurity20/presentation/jiang)
  → Dropwise 启发：取消就是在 await 点注入的一种「错误」。Dropwise 目前按 target 序号和 Pending 序号标识 plan；可以加一个 site ID = `#[track_caller]` 位置 + 截短的 backtrace 哈希。这样能跨测试、跨运行合并同一取消点，算出「项目级取消点覆盖率」，也解决「同一个 target 在循环里被创建多次」时的命名问题。工作量 S–M，是低垂果实。
- **Antithesis 的 sometimes assertion**〔工程实践〕：Antithesis 提供「sometimes」断言，要求某个条件在整个搜索中至少成立一次；如果预期会到达的分支从未执行，说明测试设置漏了重要情况。其探索由一个用强化学习「寻找新的有趣状态」的引导组件驱动。— [Antithesis: How it works](https://antithesis.com/docs/introduction/how_antithesis_works.md)；[第三方解读](https://jwatte.com/blog/blog-antithesis-deterministic-testing/)
  → Dropwise 启发（重点）：给 Dropwise 加 `ctx.sometimes(cond, "cancelled while holding a half-written frame")`。所有 plan 跑完后，如果一次都没成立，就像 unrealized 一样单独报告，`assert_cancel_correct` 也因此失败。这把 unrealized 从「Pending boundary 没到」推广成「用户关心的取消状态没到」，在 e2e 测试里尤其有用。工作量 S，是低垂果实。
- **STADS / Good-Turing 残余风险**〔2018〕：Böhme 把测试看作物种发现。下一个输入发现新「物种」的概率 U(n) 可以用 Good-Turing 估计 f₁/n（f₁ = 只出现过一次的物种数），作为「还没发现的东西」的统计上界。— [Böhme, TOSEM 2018, arXiv](https://arxiv.org/pdf/1803.02130)；[Assurances in Software Testing roadmap](https://arxiv.org/pdf/1807.10255)
  → Dropwise 启发：随机/swarm 模式下，把「取消状态签名」当物种，报告里打印「下一个 plan 发现新取消状态的估计概率 = f₁/n」。这是对 `!exhaustive` 的定量补充，用户可以据此判断预算够不够。工作量 S，是低垂果实（前提是先有上面的签名）。
- **Maple 与 SCT 的比较**：Fray 文中转述，Thomson 等人比较过 PCT 和覆盖率引导的 Maple，结论是 PCT 最有效，朴素随机次之。— [Fray, arXiv 2501.12618](https://arxiv.org/pdf/2501.12618)
  → Dropwise 启发：覆盖率引导不一定胜过好的随机。建议覆盖率先用于「报告和去重」，不急着用来「引导搜索」。

### Inferences
- 一份「取消覆盖率」报告建议分四层（由易到难）：
  1. **Site 覆盖**：用 FIFUZZ 式上下文 ID 标识的取消点里，有多少被触达、观察到 Pending、真正被取消过（Dropwise 已有一部分）。
  2. **时序覆盖**：每个 site 实际实现了哪些 timing（Immediate/Reschedule/After/AfterWake），哪些因为 unrealized 而缺失。
  3. **取消状态覆盖**：取消瞬间的签名 = (site, 持有的 Obligation 标签集合, target 是否已被唤醒, 队列深度分桶, Pending 序号的 log2 分桶)。思路分别来自 GFuzz 的 channel 状态、Krace 的对和 AfterWake。
  4. **清理路径覆盖**：只在被取消的运行中执行到的代码块（cancellation-induced coverage）。可以用 `-C instrument-coverage` 对比 baseline 和各 plan 得到，比较重（M），但能直接回答「Drop 实现和 requeue 分支有没有被测到」。
- 第 3 层签名同时服务于 Q1 的等价归并、Q2 的 Good-Turing 估计、Q3 的报告去重，一份数据三处用，最划算。

### Gaps
- 没有找到专门针对「取消/中止」的覆盖率指标文献；上面的映射都是本笔记的推导。
- GFuzz 打分公式的具体权重、select 等待窗口的具体毫秒数，本次只拿到中文解读，没有复核原文。

---

## Q3 工业界确定性仿真测试（FoundationDB、TigerBeetle VOPR、Antithesis、madsim/turmoil/S2、Jepsen）：如何处理 timeout/取消？有哪些便宜地跑大量 plan 的技术（snapshot、fork-server、checkpoint、进程内重置）？

### Takeaway
DST 系统几乎都把 timer 当成仿真器控制的非确定性选择（P# 的 Timer machine、FoundationDB 的 `delay()` + BUGGIFY），并且都把运行分成「注入故障」和「停止注入、看能否恢复」两个阶段（FDB 的 speedUpSimulation、VOPR 的 liveness mode）。这正是 Dropwise 的 settle，而且它们的做法更精细：按收敛条件提前结束、区分「不可恢复故障」和「bug」。降成本方面，真正可落地的是进程内重跑（madsim/turmoil/S2）、fixture 复用 + 稳定性自检（AFL++ persistent mode），以及确定性自检（同 seed 跑两次、比较 trace）。fork/snapshot 遇上多线程 Tokio 风险很高。

### Cited Findings
- **FoundationDB 单进程仿真**〔2010s〕：FDB 团队做了一个确定性仿真框架，在一个物理进程里模拟一组交互的进程，包括磁盘、进程、网络和请求级故障。— [FoundationDB, CACM 2023 版](https://cacmb4.acm.org/magazines/2023/6/273229-foundationdb-a-distributed-key-value-store)
  → Dropwise 启发：Dropwise 已经在进程内。真正的差距是：被测的真实项目 e2e 测试（Qdrant、Iroh）本身不是单进程确定性的。FDB 的路线需要项目从一开始就为仿真设计，对已有项目迁移性弱。
- **BUGGIFY**〔FDB〕：BUGGIFY 只在仿真中为真。每个 BUGGIFY 点第一次求值时决定本次运行启用还是禁用；启用的点默认 25% 概率返回真，`BUGGIFY_WITH_PROB(p)` 可调。用途包括在高层插入 `wait(delay(5))`、强制走罕见的恢复路径、把调优 knob 随机化成极端值；`delay()` 内部本身也有低概率的 BUGGIFY 延长 sleep。仿真到 300 秒后设置 `speedUpSimulation`，高破坏性的点只以极低概率触发，让系统恢复并结束。目标是「做坏事，但别太多」。— [transactional.blog: BUGGIFY](https://transactional.blog/simulation/buggify)；[Go 移植的说明](https://pkg.go.dev/fdb.dev/pkg/dst)
  → Dropwise 启发（三点）：(1) **按运行启用 target**（swarm，见 Q5）：每次运行随机启用一部分 target，被启用的才参与取消，避免外层取消总是遮住内层。工作量 S。(2) **knob 化 timeout**：对 Dropwise 没法包装的自定义 timeout wrapper，给用户一个 `dropwise::knob(Duration::from_secs(30))`，测试时由 plan 返回 0、1 tick 或原值。这是覆盖「非 select!/timeout 的取消点」的便宜办法。工作量 S–M，是低垂果实。(3) **先乱后稳**：FDB 的 speedUpSimulation 等同于 Dropwise 的 settle，可以作为 settle 设计的旁证。
- **TigerBeetle VOPR 的 swarm 与 liveness mode**〔2020s〕：VOPR 用随机 seed 决定故障参数（丢包/重排、分区、读写损坏）。源码里 `options_swarm(prng)` 每个 seed 随机副本数、客户端数、批大小等；safety 阶段结束后调用 `transition_to_liveness_mode(core)`：选一个全连通的副本「core」，把崩溃等故障概率设为 0，最多跑 `ticks_max_convergence` 个 tick，等 `pending()==null`。如果不收敛，先判断 `cluster_recoverable`：可恢复却不收敛才报 liveness bug；safety 阶段没跑完时，先用全连通 core 修复所有可修复的块，之后只剩相关故障。— [TigerBeetle vopr.zig](https://github.com/tigerbeetle/tigerbeetle/blob/main/src/vopr.zig)；[VOPR 文档](https://docs.tigerbeetle.com/about/vopr)；[HACKING.md](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/HACKING.md)
  → Dropwise 启发（重点）：(1) **settle 到收敛而不是固定时长**：定义 quiescence 谓词（没有未 resolve 的 Obligation、没有可运行任务、在 horizon 内没有 timer），满足就提前结束，上限作为 watchdog，报告里写「t=… 收敛」。既省时间，也能把「暂态效应」和「持久效应」分开：Dropwise 已观察到「取消后立刻检查」在修复后的代码上误报。工作量 S–M，是低垂果实。(2) **可恢复性分类**：借 `cluster_recoverable`，把 violation 分成「settle 后仍不满足（持久）」和「settle 期间暂时不满足但最终满足（暂态，降级为 info）」。工作量 S。
- **P#/Coyote：timer 是受控的非确定性，liveness 用 hot/cold monitor**〔2016〕：在 Azure Storage vNext 的案例里，timer 被建模成一个 machine，每次 `PSharp.Nondet()` 为真才发 `TimerTickEvent`，由 P# 运行时控制 timeout 何时送达并系统化探索。liveness 用 monitor 表达：进入 hot 状态表示「欠着一个进展」，离开就转 cold；超过用户给的大界限的执行被当作「无限执行」，结束时还在 hot 就报 liveness 违规。— [Deligiannis et al., FAST 2016](https://www.doc.ic.ac.uk/~afd/papers/2016/FAST.pdf)；[USENIX 页面](https://www.usenix.org/conference/fast16/technical-sessions/presentation/deligiannis)
  Coyote（P# 的后继）自述为 .NET 并发单元测试库，接管调度、系统化探索 timeout 和消息顺序。— [microsoft/coyote](https://github.com/microsoft/coyote/)
  → Dropwise 启发：Dropwise 的 Obligation 实际上就是 P# 的 hot 状态（未 resolve = hot，resolve = cold，settle 上限 = 「有界无限」），这一点可以写进文档作为理论依据。可新借的是：对自定义 timer（不是 `tokio::time`），提供一个 `dropwise::Timer` 替身，由 plan 决定「现在触发」，这正是 P# Timer machine 的做法。工作量 M。
- **S2 的 Tokio DST**〔2024–2025·新〕：S2 用 Tokio 单线程调度器 + paused time（时间只在 `sleep()` 时前进），代码统一用 `tokio::time::Instant`。Tokio 内部 RNG（决定 `select!` 分支顺序等）可以设 seed。网络用 turmoil，单进程模拟多 host；mad-turmoil（源自 MadSim）覆盖 libc 的 `getrandom`/`getentropy`/`clock_gettime`。他们在 CI 里有一个「meta test」：同一个 seed 跑两次，比较 TRACE 级日志，确认「直到线上最后一个字节都一致」。每个 PR 都跑，夜间几千次，已发现 17 个值得注意的问题。剩余的非确定性来源有 `HashMap` 随机哈希、依赖库自己起线程/读时间/做 I/O。— [S2 blog: DST for async Rust](https://s2.dev/blog/dst)
  → Dropwise 启发（重点）：(1) **确定性自检**：Dropwise 在跑 plan 之前先把 baseline 跑两次，比较每个 target 的 Pending 序列（以及可选的 trace 哈希）。不一致就预先告诉用户「这个场景不确定，后面的 unrealized 是噪声」，而不是事后才看到一堆 unrealized。工作量 S，是低垂果实。(2) CurrentThread 模式下默认设置 Tokio 的 rng seed，让 `select!` 的分支顺序可复现（见下一条）。
- **Tokio `rng_seed`**〔2022〕：PR #4910 给 `runtime::Builder` 加了 `rng_seed`，目的就是让 `tokio::select!` 的随机 poll 顺序在测试中可复现。它在 `tokio_unstable` 下；讨论中指出多线程下任务到线程的分配不受控，完全确定只在单线程调度器 + 受控 I/O 时现实。— [tokio PR #4910](https://github.com/tokio-rs/tokio/pull/4910)；[tokio util/rand 源码](https://codebrowser.dev/tokio/tokio/src/util/rand.rs.html)
  → Dropwise 启发：Dropwise 用 `ctx.race` 接入真实的 `select!` 时，分支 poll 顺序本身就是一个非确定性维度（GFuzz 探索的正是这个，见 Q4）。可以先用固定 seed 去掉它，再把「seed」当作 plan 的一个可选维度。需要 `tokio_unstable`，工作量 S。
- **madsim / turmoil**〔2022–2023〕：madsim 用 `madsim-tokio`、`madsim-tonic` 等替身 crate 和打补丁的传递依赖（`getrandom`、`quanta` 等），在 `--cfg madsim` 下确定性执行，要求所有 I/O 都被 mock，并提供 kill 进程、断网等控制 API。turmoil 在单线程里跑多个 host，模拟 host、时间和网络，README 自称「非常实验性」。— [madsim](https://github.com/madsim-rs/madsim)；[turmoil](https://github.com/tokio-rs/turmoil)；[Announcing turmoil](https://tokio.rs/blog/2023-01-03-announcing-turmoil)；[RisingWave DST 博客](https://risingwave.com/blog/deterministic-simulation-a-new-era-of-distributed-system-testing/)
  → Dropwise 启发：Dropwise 应该能在 madsim/turmoil 运行时里跑（或者至少不冲突），让已经采用 DST 的项目（RisingWave 等）直接获得确定、进程内、可重放的 plan 执行。这对已有 DST 的项目是低垂果实（做兼容层，工作量 M）；对没有 DST 的项目没有帮助。
- **Antithesis**〔2020s〕：确定性 hypervisor 让整个系统（一个 VM 模拟一台或多台机器）成为可复现单元；每个事件都可能开出一条新时间线，形成「多重宇宙」，由 RL 引导组件寻找新状态。— [How Antithesis works](https://antithesis.com/docs/introduction/how_antithesis_works.md)；[Deterministic hypervisor](https://antithesis.com/blog/deterministic_hypervisor/)
  → Dropwise 启发：「从中间状态分叉」正好对应「在第 j 个 Pending 处分叉出所有 timing 变体」，可以省掉重跑前缀。但这需要整机快照，Dropwise 自己做不了（L，不是低垂果实）。能借的是 sometimes assertion（见 Q2）。
- **AFL++ deferred forkserver / persistent mode**〔工程实践〕：把 `__AFL_INIT()` 放在昂贵初始化之后，可获得「10x+」加速；但不能放在创建线程或子进程之后，也不能放在 `setitimer()` 之后，还要小心 socket、临时文件和带偏移的 fd。persistent mode（`__AFL_LOOP(1000)`）在一个进程里跑多次，「轻松快 10–20 倍」，前提是状态能完全重置；用 `stability` 指标检测状态残留，循环次数上限用来限制泄漏的影响。— [AFL++ persistent mode 文档](https://github.com/AFLplusplus/AFLplusplus/blob/stable/instrumentation/README.persistent_mode.md)
  → Dropwise 启发（重点，针对「每个 plan 一个进程太贵」）：(1) **fixture 复用 = persistent mode**：API 形如 `assert_cancel_correct_with(setup, reset, scenario)`，重型 setup（起服务、建索引）只做一次，每个 plan 前调用用户的 `reset`。工作量 M，是低垂果实。(2) **稳定性自检**：每 N 个 plan 插一次 baseline 重跑，Pending 序列和 baseline 不一致就报「reset 不干净」，对应 AFL 的 stability。工作量 S。(3) **fork 不推荐**：AFL 自己警告线程之后不能 fork，多线程 Tokio + blocking pool 正属这种情况；只有 CurrentThread 且确认没有 blocking 线程时才可以考虑 fork。
- **Nyx-Net 增量快照**〔2022〕：基于 hypervisor 快照的网络 fuzzer，对消息序列的前缀做增量快照，再变异后缀；比当时最先进的方法吞吐量最高高 300 倍、覆盖率最多高 70%。— [Schumilo et al., EuroSys 2022, arXiv](https://arxiv.org/pdf/2111.03013)
  → Dropwise 启发：从概念上说，「前缀快照 + 后缀变异」正是 Dropwise 的理想执行模型：前缀跑到第 j 个 Pending，后缀是各种 timing。但工程上要 VM 级快照（L），对 Rust 测试工具不现实；如果以后做 CRIU 或容器 checkpoint 集成，这是参考设计。不是低垂果实。
- **fail-rs（TiKV failpoint）**〔Rust 工程实践〕：`fail_point!` 宏在 `failpoints` feature 下才编译进来；动作语法 `[p%][cnt*]task[(arg)]`，task 包括 `off`、`return(arg)`、`sleep(ms)`、`panic(msg)`、`print(msg)`、`pause`（停住直到配置改变）、`yield`、`delay(ms)`（忙等），可以用 `->` 串联；通过 `FAILPOINTS` 环境变量或 `fail::cfg` 在运行时配置；测试用 `FailScenario::setup()/teardown()`。— [fail::cfg 文档](https://docs.rs/fail/latest/fail/fn.cfg.html)；[tikv/fail-rs](https://github.com/tikv/fail-rs)
  → Dropwise 启发：(1) 借语法：让 Dropwise 的 plan 也能用环境变量指定（`DROPWISE_PLAN=site@3:AfterWake`），不改代码就能在重型 e2e 测试里复现单个 plan，还能拆成多个进程或多台 CI 机器并行跑（「每 plan 一进程」的成本可以靠水平扩展摊薄）。工作量 S，是低垂果实。(2) 已经用 fail-rs 的项目（TiKV 系）里，failpoint 的位置就是现成的候选取消点。`pause` 动作在语义上接近「target 被卡住，竞争者先 ready」。
- **Jepsen 的不确定操作**〔工程实践〕：客户端超时时，操作记为 `:info`（不知道成没成功）；checker 认为崩溃的操作「可能发生也可能没发生」。Knossos 对每个不确定操作分别按成功和失败各检查一次，只要有一种历史可线性化就通过。不确定操作越多，并发度越高，验证时间指数增长，checker 的检出力也越弱；能确定没发生的应该返回 `:fail`。— [Jepsen nemesis 教程](https://github.com/jepsen-io/jepsen/blob/master/doc/tutorial/05-nemesis.md)；[Knossos 计算技术](https://aphyr.com/posts/314-computational-techniques-in-knossos)；[Couchbase Jepsen 博客](https://www.couchbase.com/blog/introduction-to-jepsen-testing-at-couchbase/)
  → Dropwise 启发（重点）：被取消的操作就是 Jepsen 的 `:info`。提供 `ctx.indeterminate(op_id)` 加一组 invariant 助手（`applied_at_most_once`、`applied_exactly_once_after_settle`、「状态 ∈ {做了, 没做}」），让用户写「允许两种结果」的 invariant，而不是写死其中一种，可以减少 fixed code 上的误报。Jepsen 的教训也提醒：多取消（c≥2）时，不确定操作越多 oracle 越弱，这是把 c 保持在小值的另一个理由。工作量 S–M，是低垂果实。
- **Filibuster（服务级故障注入）**〔2021〕：Meiklejohn 等人从组织已有的功能测试出发，用静态分析和 concolic 风格的执行把它们扩展到故障场景；用「dynamic reduction」算法（利用服务封装性：下游故障只能通过上游的响应被观察到）减少冗余组合。— [Meiklejohn et al., SoCC 2021](https://people.eecs.berkeley.edu/~rohanpadhye/files/filibuster-socc21.pdf)；[Dynamic Reduction 博客](https://christophermeiklejohn.com/filibuster/2021/10/14/filibuster-4.html)
  → Dropwise 启发：(1) 「复用现有测试」和 Dropwise 的可行性研究同一路线，Filibuster 证明这条路可行。(2) **嵌套取消的 dynamic reduction**：外层 timeout 包着内层 future 时，「取消内层」和「取消外层」如果让调用方观察到同样的结果（同一错误、同样的 Obligation 结局），c=2 时就不必再组合它们。工作量 M。
- **FATE/DESTINI**〔2011〕：Gunawi 等人拦截每次 I/O，用「failure ID」抽象标识可注入的故障点，系统化地探索多故障组合；剪枝策略带来约一个数量级的加速，同时找到的 bug 数不变。在 HDFS、ZooKeeper、Cassandra 上探索了 4 万多个故障场景，找到 16 个新 bug。— [Gunawi et al., NSDI 2011](https://usenix.org/conference/nsdi11/fate-and-destini-framework-cloud-recovery-testing)；[论文 PDF](https://usenix.org/legacy/events/nsdi11/tech/full_papers/Gunawi.pdf)
  → Dropwise 启发：failure ID ≈ Dropwise 的 site ID + 取消状态签名（Q2）。FATE 的多故障剪枝（按 failure ID 等价合并组合）可以直接用来剪 c≥2 的 plan。工作量 M；要先有签名。

### Inferences
- 「每个 plan 一个进程太贵」的现实方案，按性价比排：① 进程内重跑（已有）+ fixture 复用 + 稳定性自检（AFL persistent 思路，M）；② 用环境变量选 plan + CI 水平拆分（fail-rs 思路，S）；③ 效果等价归并 + 嵌套剪枝，减少 plan 数（DPOR/FATE/Filibuster，M）；④ near-miss 优先（Q4），只跑最有希望的 plan（S–M）；⑤ 兼容 madsim/turmoil（M，只对已有 DST 的项目）；⑥ fork/快照（L，有线程安全风险，暂不建议）。
- VOPR 和 FDB 共同说明：「故障阶段 → 停止注入 → 收敛检查」的两段式加上「可恢复才算 bug」，是工业界处理暂态和持久效应的成熟模式；Dropwise 的 settle 应该从「固定时长」升级为「收敛谓词 + 上限 + 暂态/持久分类」。

### Gaps
- 没有找到 Antithesis 快照/分叉机制的公开技术细节，只有「多重宇宙」的高层描述。
- 没有找到任何 DST 框架专门讨论「future 在 await 点被 drop」这种取消（它们讨论的是节点崩溃、网络和磁盘故障）。DST 管的是环境故障，Dropwise 管的是程序内部的控制流取消，两者互补而非重叠。
- madsim 的 `MADSIM_TEST_NUM`、确定性检查等运行参数，本次没有在 README 里确认（页面没写），需要查 docs.rs。

---

## Q4 显式探索「时序」选择（timer 触发顺序、虚拟时间、「立刻触发 timeout」、事件顺序、select）而非线程交错的工作

### Takeaway
最接近 Dropwise 的先例有三个：GFuzz（强制 Go `select` 先选某个 case，否则回退）、Lincheck（Kotlin 协程的 `cancellableOnSuspension` / `promptCancellation`，后者就是 AfterWake），以及 JS/Node 的事件重排（Node.fz、NodeRacer、AjaxRacer）。它们共同给出两条 Dropwise 还没用的经验：(1) 用 baseline 的 happens-before 或 near-miss 信息挑「值得翻转的时序」，可以显著减少误报（NodeRacer、TSVD）；(2) 「对抗性时序」得到的失败里，有一部分开发者认为「现实中不可能」而不修（Fray），所以报告要标注时序是否合理。

### Cited Findings
- **Lincheck：协程取消的系统化测试**〔2020–2023〕：`@Operation(cancellableOnSuspension = true)` 让 Lincheck 也探索「挂起的操作被取消」的场景；只能在操作真正挂起之后取消，如果在取消前已经 resume，取消失败、操作正常完成。`promptCancellation = true` 进一步允许「已经 resume 但还没来得及运行」时取消，文档说这模拟了「协程在被 resume 和真正处理结果之间被取消」的现实行为。CAV 2023 论文说，他们扩展了 dual data structures 形式化（在挂起点把操作切成两半、分别线性化），允许挂起的请求被取消；还支持 `@Validate` 在测试结束时检查数据结构不变量（用它发现过一个内存泄漏：逻辑删除的节点在结束时仍可达）。Lincheck 已用于 Kotlin Coroutines 的开发。— [Lincheck 文档：执行选项](https://kotlinlang.org/docs/lincheck-operation-execution-options.html)；[Koval et al., CAV 2023（ISTA 全文）](https://research-explorer.ista.ac.at/download/14260/14275)；[Springer](https://link.springer.com/chapter/10.1007/978-3-031-37706-8_8)
  → Dropwise 启发（重点）：(1) **外部验证**：Dropwise 的 `AfterWake` 和 Lincheck 的 `promptCancellation` 是同一个想法，独立在 Kotlin 生态出现过；Dropwise 的「unrealized」和「resume 后取消失败」也一致。写文档和定位时可以引用这个先例，同时说明 Dropwise 的增量在哪里：Rust 的 drop 语义、Obligation、真实应用场景，而不是数据结构线性化。(2) 能借的是「操作级规格 + 允许取消的线性化」，用于库级测试（自写的 channel/semaphore/连接池）。工作量 L，不是低垂果实，但对 Tokio 原语的作者有价值。
- **GFuzz 的 select 强制**〔2022〕：（详见 Q2）把 `select` 改写为先尝试指定 case，并引入等待时间防止死锁；开销约 3 倍。— [Liu et al., ASPLOS 2022](https://par.nsf.gov/servlets/purl/10321048)；[USTC 中文解读](https://s4plus.ustc.edu.cn/_upload/article/files/04/36/b629286e4b6094e4cb40059c2af2/6722b815-04b7-4bea-a6e8-a3cb5edaaa9a.pdf)
  → Dropwise 启发：`tokio::select!` 的分支 poll 顺序（随机，或 `biased;`）是 Dropwise 目前没有显式探索的维度：多个分支同时 ready 时谁赢，正是「哪个 future 被 drop」的另一种来源。可以加一个 `Race::Tie` timing（竞争者和 target 在同一次 poll 中都 ready），配合 rng seed 两种顺序都试。工作量 S–M，是低垂果实。
- **Node.fz**〔2017〕：Davis、Thekumparampil、Lee 先研究真实 Node.js 程序里的并发 bug，然后随机打乱事件循环的任务和事件队列；暴露已知 bug 的频率远高于原版 Node.js，还发现了新 bug。实现方式是改 Node.js 内部 C/C++（基于 v0.12.7），后来被指出难以随 Node 演进维护。— [Davis et al., EuroSys 2017](https://www3.cs.stonybrook.edu/~dongyoon/papers/EUROSYS-17-NodeFz.pdf)；[Morning Paper 解读](https://blog.acolyer.org/2017/06/09/node-fz-fuzzing-the-server-side-event-driven-architecture/)；[ECOOP 2025 NACD 的评述](https://drops.dagstuhl.de/storage/00lipics/lipics-vol333-ecoop2025/html/LIPIcs.ECOOP.2025.9/LIPIcs.ECOOP.2025.9.html)
  → Dropwise 启发：对应 Tokio 的「run queue 打乱」。Dropwise 的 `Reschedule(n)` 已经部分利用了「Tokio 通常先跑其他 ready 任务」。教训是：改运行时内部的方案维护成本很高，Dropwise 应该坚持只用公开 API（yield、paused time）。
- **NodeRacer**〔2020〕：Endo、Møller 等人用 happens-before 关系引导，有选择地推迟事件来检测 Node.js 事件竞争，完全靠代码插桩，不改 Node.js；找到竞争错误的概率高于最先进的 fuzzer，happens-before 关系有助于避免误报；也可用于诊断事件竞争导致的 flaky test。注意它发表在 ICST 2020，不是 ICSE。— [NodeRacer 论文](https://users-cs.au.dk/~amoeller/papers/noderacer/paper.pdf)；[ICST 2020](https://conf.researchr.org/details/icst-2020/icst-2020-papers/7/NodeRacer-Event-Race-Detection-for-Node-js-Applications)
  → Dropwise 启发：「先跑一次记录 happens-before，再只推迟可以推迟的事件」对应 Dropwise 的「只在 target 和竞争者之间确实没有因果依赖的 boundary 上取消」。如果竞争者的就绪本身依赖 target 先完成（例如竞争者在等 target 写的数据），这种 plan 在现实中不可能发生，应标为「不可行」而不是报 bug。工作量 M。
- **NACD**〔2025·新〕：ECOOP 2025 的工作改为在核心异步操作周围注入延迟，并指出 Node.fz 只重排调度队列、NodeRacer 只重排事件处理回调，各有局限。— [ECOOP 2025 (LIPIcs)](https://drops.dagstuhl.de/storage/00lipics/lipics-vol333-ecoop2025/html/LIPIcs.ECOOP.2025.9/LIPIcs.ECOOP.2025.9.html)
  → Dropwise 启发：「在 I/O 原语周围注入延迟」对应 Dropwise 的 `After(d)`；佐证「延迟注入」和「重排」需要同时存在，Dropwise 两者都有（After 和 Reschedule）。只作确认。
- **AjaxRacer（对抗性调度 + 差分 oracle）**〔2018〕：Adamsen、Møller、Alimadadi、Tip 对每个测试跑两种调度：「同步」（响应处理紧接着请求处理）和「对抗性」（中间插入别的处理），自动比较两者最终页面的截图；在 20 个网站中的 12 个找到有害竞争，共 72 份报告，误报很少。— [Adamsen et al., ESEC/FSE 2018](https://users-cs.au.dk/amoeller/papers/ajaxracer/paper.pdf)
  → Dropwise 启发：**差分 oracle**：用户不写 invariant 时，默认把「被取消的运行 settle 后的可观测状态投影」和「baseline 的投影」以及「操作没发生的投影」对比，不属于两者之一就报。这和 Jepsen `:info` 是同一个想法（Q3）。工作量 S–M，是低垂果实（需要用户给一个 `observe()` 投影函数）。
- **TSVD（near-miss 引导的延迟注入）**〔2019〕：TSVD 监控对线程不安全方法的调用来找嫌疑点，在 Microsoft 内部数千个项目中发现 1000 多个线程安全违规，获 SOSP 最佳论文。按 MIT 课程的二手描述，它记录两次冲突访问相距 100 ms 以内的「near miss」，下次在其中一处注入 100 ms 延迟。— [Li et al., SOSP 2019](https://6826.csail.mit.edu/2020/papers/tsvd.pdf)；[MIT 6.826 讲义（二手描述）](https://6826.csail.mit.edu/2020/lec/l12-tsvd.txt)
  → Dropwise 启发（重点，针对预算）：**near-miss 优先**：baseline 里记录每个 target 完成的时刻，以及它真实竞争者（例如外层 timeout）的剩余时间或就绪时刻；两者越接近，翻转的现实可能性越大，越该优先跑。同一份数据还给出「合理性」标签（下一条）。预算有限的 e2e 测试只跑 near-miss 排在前面的 plan。工作量 S–M，是低垂果实。
- **Waffle（MemOrder bug 的主动延迟注入）**〔2023·新〕：Stoica、Lu、Musuvathi、Nath 针对「访问对象与对象初始化/释放之间时序错误」的 MemOrder bug（use-before-init、use-after-free），改进延迟注入的设计，在 11 个开源 C# 应用上以更低开销找到更多 bug。（常被引为 EuroSys 2022，产物仓库标注为 EuroSys '23。）— [Waffle 代码](https://github.com/bastoica/waffle)；[NSF PAR](https://par.nsf.gov/biblio/10447172)
  → Dropwise 启发：取消后「连接被当作在消息边界上复用」本质上是 use-after-dispose/use-before-reinit 的时序 bug。Waffle 的做法是在「释放点」和「使用点」之间插延迟。Dropwise 里对应的是：取消后让 settle 期间的下一个使用者尽量早来（Immediate）或尽量晚来，两种都试。迁移性中等。
- **Fray 对 timed wait 的处理与「不太可能的调度」**〔2025·新〕：Fray 把 `Thread.sleep` 改写成 no-op，把 `wait(timeout)`、`awaitNanos`、`parkNanos` 改成不带超时的版本，并通过建模「随时可能的虚假唤醒」覆盖超时触发的情况。作者承认这会产生「现实中不太可能」的调度（例如 sleep 10 秒还早于另一线程的任何进展），有些开发者因此不愿修；未来打算用统计方法或用户配置抑制这类警告。实验表格单独标出依赖 timer 的失败。Fray 还记录并重放 `System.nanoTime()` 和 `hashCode`，以保证重放确定。— [Fray, arXiv 2501.12618](https://arxiv.org/pdf/2501.12618)
  → Dropwise 启发（重点）：这正对应 Dropwise 的「false alarm」担忧。建议给每个 violation 标注 **plausibility**：`Immediate`/`After(0)` 对上真实的 30 s timeout 时标「对抗性」，near-miss 时标「现实」，并按此分级报告（参照 Fray 把依赖 timer 的失败单列）。另外，Fray 的「记录/重放 nanoTime」对应 Dropwise MultiThread 模式：记录 baseline 的时间读取并在 plan 中重放，可减少 unrealized。工作量：标注 S，重放 M。

### Inferences
- Lincheck 的 `promptCancellation` 说明 AfterWake 维度有独立先例，Dropwise 在 Notify lost-wakeup 上的发现（只有多时序找得到）与之相符。在 Rust 生态里，没有找到其他工具探索这个维度。
- 「时序合理性」是这批文献里最被低估、最直接回应 Dropwise 误报问题的点：NodeRacer 用 happens-before 排除不可行的、TSVD 用 near-miss 找最可能的、Fray 承认对抗性时序会被开发者拒绝。合起来就是：给 plan 打「可行 / 现实 / 对抗」三级标签，默认只让前两级让 `assert_cancel_correct` 失败（可配置）。
- `select!` 分支平局（两边在同一次 poll 都 ready）是 GFuzz 的核心维度，Dropwise 还没显式覆盖，加入成本低。

### Gaps
- 没有找到专门「fuzz timer 触发顺序」（多个 timer 同一虚拟时刻到期时的顺序）的论文；Tokio paused time 下同一时刻到期的 timer 顺序是否可控，需要另查 Tokio 源码。
- Android 事件竞争（EventRacer、CAFA 等）本次没有检索，因为它们与 JS 的思路高度重合；如需完整性可以补。
- TSVD near-miss 的具体参数只来自二手讲义。

---

## Q5 模式级迁移：small-scope hypothesis、bug depth、swarm testing 能否用于取消配置？（含低垂果实总排序）

### Takeaway
三条实证（Lu 2008：96% 的并发 bug 只需两个线程之间的偏序；Yuan 2014：92% 的灾难性故障源于对显式报告的非致命错误处理不当；SCTBench：bound 1–2 足够）共同支持「单取消、少时序」的默认预算。swarm testing（每次运行随机关掉一部分特性）在 FDB（BUGGIFY 按运行启用）和 TigerBeetle（`options_swarm`）都有工业实现，在 Dropwise 里的对应物是「每次运行随机启用一部分 target 和 timing 种类」。它在随机/大规模模式下有用，在 c=1 穷尽模式下没用。

### Cited Findings
- **Bug depth 实证（并发）**〔2008〕：Lu、Park、Seo、Zhou 研究 105 个真实并发 bug：96% 只要强制两个线程之间的某个偏序就一定出现；约 92% 只要强制最多 4 次内存访问之间的顺序就能稳定触发；97% 的死锁是两个线程循环等待最多两个资源。— [Lu et al., ASPLOS 2008](https://www.cs.columbia.edu/~junfeng/08fa-e6998/sched/readings/concurrency-bugs.pdf)
  → Dropwise 启发：支持默认 `max_cancellations=1`，以及「每个取消只需 1–2 个时序约束」（Immediate/AfterWake 等）。不需要新工作，用作配置默认值的依据。
- **错误处理是主要来源**〔2014〕：Yuan 等人分析 Cassandra、HBase、HDFS、MapReduce、Redis 的 198 个用户报告故障：92% 的灾难性故障源于对显式报告的非致命错误处理不当；几乎所有故障最多 3 个节点就能复现（二手来源给出 90% ≤3 节点、58% 可由简单测试错误处理代码发现，未复核原文）。— [Yuan et al., OSDI 2014](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/yuan)；[PDF](https://www.eecg.utoronto.ca/~stumm/Papers/Yuan-OSDI-14.pdf)
  → Dropwise 启发：取消是 async Rust 里最常见、最隐蔽的「非致命错误路径」。这为「把 Dropwise 挂到现有的小规模集成测试上」提供了动机：大多数问题不需要大规模场景。可以用来定位产品，不需要额外工作。
- **Swarm testing**〔2012〕：Groce、Zhang、Eide、Chen、Regehr 放弃「每个测试都包含所有特性」，改为大量随机配置，每个配置随机省略部分特性。两种机制：有的特性会主动压制有趣行为（例如 `pop` 让栈永远到不了溢出检查），特性之间也会争抢测试里的「空间」。在一周测试中，比精心调过的默认配置多发现 42% 种让 C 编译器崩溃的方式。— [Groce et al., ISSTA 2012](https://www.cefns.nau.edu/~adg326/issta12.pdf)；[Regehr 博客](https://blog.regehr.org/archives/591)
  → Dropwise 启发：Dropwise 里「压制」的典型情况是：外层 target（例如整个请求的 timeout）一取消，内层 target 的 Pending 就永远到不了，表现为 unrealized；而 Reschedule/After 这类 timing 又会让其他任务抢先运行，改变内层的到达路径。swarm 模式：每次运行随机启用一部分 target（其余永不取消）、随机启用一部分 timing 种类。只在随机模式或 c≥2 的采样中启用。工作量 S，在随机模式下是低垂果实；c=1 穷尽模式下没有意义（迁移性弱）。
- **工业界的 swarm**：FDB 的 BUGGIFY 在每次运行中按点随机启用或禁用（启用后默认 25% 触发），目标是「做坏事，但别太多」；TigerBeetle 的 `options_swarm` 每个 seed 随机集群规模和参数，源码中还有一条 TODO：「Swarm testing: 分开测试长而少的崩溃和短而多的崩溃」。— [BUGGIFY](https://transactional.blog/simulation/buggify)；[vopr.zig](https://github.com/tigerbeetle/tigerbeetle/blob/main/src/vopr.zig)
  → Dropwise 启发：同上；另外「长而少 vs 短而多」可以直接对应「少量取消 + 长 After(d)」和「多次 Immediate 取消」两种 swarm 配置族。
- **Small-scope hypothesis**：多数 bug 能在小规模反例中暴露。这是 Alloy/Jackson 的经典主张，Yuan 2014（≤3 节点）和 Lu 2008（2 线程）可视为分布式和并发领域的实证。— [Yuan et al., OSDI 2014](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/yuan)；[Lu et al., ASPLOS 2008](https://www.cs.columbia.edu/~junfeng/08fa-e6998/sched/readings/concurrency-bugs.pdf)
  → Dropwise 启发：鼓励用户为 Dropwise 写「小场景」（1–2 条消息、1 个 target）而不是直接挂 e2e；可以在文档里给出「从 e2e 测试里抽小场景」的指南。工作量 S（文档）。

### Inferences
**低垂果实总排序**（综合 Q1–Q5，按「价值 / 工作量」排序；均为本笔记的推导建议）：

| # | 机制 | 来源 | 解决 Dropwise 的什么问题 | 工作量 | 低垂? |
|---|---|---|---|---|---|
| 1 | baseline 跑两次的确定性自检（比较 Pending 序列 / trace 哈希），提前预警 unrealized | S2 meta test；AFL stability | MultiThread 噪声；误以为没测到 | S | 是 |
| 2 | settle 到收敛（quiescence 谓词 + 上限），并把 violation 分成暂态和持久 | TigerBeetle liveness mode、`cluster_recoverable`；FDB speedUpSimulation；P# hot/cold | 立刻检查导致的误报；settle 成本 | S–M | 是 |
| 3 | plan 合理性标签（可行 / 现实 / 对抗）+ near-miss 优先排序 | Fray「不太可能的调度」；TSVD near-miss；NodeRacer happens-before | 误报、开发者不认；e2e 预算 | S–M | 是 |
| 4 | 效果等价归并：没有中间效果的 Pending boundary 合并成一类 | DPOR / sleep set；FATE failure ID；POS「避免偏向长分支」 | Pending 次数爆炸；重型测试预算 | M | 是 |
| 5 | `ctx.sometimes(cond, label)` 可达性断言，未满足的像 unrealized 一样报告 | Antithesis | 「到底测到了什么取消状态」 | S | 是 |
| 6 | 不确定操作的 oracle 助手（`indeterminate`、at-most-once、差分投影） | Jepsen `:info`/Knossos；AjaxRacer | 误报；invariant 难写 | S–M | 是 |
| 7 | 随机/swarm 模式 + 分层采样 + 打印 PCT 式概率界和 Good-Turing 残余风险 | PCT；POS；Swarm；BUGGIFY；STADS | 大 plan 空间下的可报告保证 | S–M | 是 |
| 8 | 上下文敏感的 site ID（track_caller + backtrace 哈希）和项目级取消点覆盖率 | FIFUZZ；GFuzz 三元组 | 跨测试汇总；combinator 内的取消点命名 | S–M | 是 |
| 9 | 用环境变量选 plan（fail-rs 语法），单 plan 复现 + CI 水平拆分 | fail-rs `FAILPOINTS` | 每 plan 一进程的成本、复现 | S | 是 |
| 10 | 自定义 timeout/timer 的 knob 或替身（plan 决定「现在触发」） | BUGGIFY knob；P# Timer machine；Fray 改写 timed wait | 真实项目里多是自定义 wrapper，不是 select!/timeout | S–M | 是 |
| 11 | `select!` 平局 timing + Tokio `rng_seed` | GFuzz；tokio PR #4910 | 未覆盖的分支顺序维度 | S–M | 是 |
| 12 | fixture 复用（persistent mode）+ 周期性 baseline 稳定性检查 | AFL++ persistent / deferred forkserver | 重型 setup 成本 | M | 部分 |
| 13 | 嵌套取消的 dynamic reduction / c≥2 组合剪枝 | Filibuster；FATE | c≥2 爆炸 | M | 部分 |
| 14 | 兼容 madsim/turmoil 运行时 | madsim、turmoil、S2 | 已有 DST 的项目获得完全确定的 plan 执行 | M | 对已有 DST 的项目是 |
| 15 | 跑在 shuttle 执行器上，plan × PCT 调度的乘积空间 | shuttle、PCT | MultiThread 不确定性 | L | 否 |
| 16 | VM/进程快照，在第 j 个 Pending 分叉 | Antithesis、Nyx-Net、AFL forkserver | 前缀重跑成本 | L（多线程下有风险） | 否 |
| 17 | Lincheck 式「允许取消的线性化」规格检查 | Lincheck CAV 2023 | 库级原语的取消正确性 | L | 否 |
| 18 | RL/Q-learning 引导 | Mallory、Antithesis | — | L | 否（收益不明） |

- 迁移性弱或不建议的：loom（内存模型层面，和 drop 无关）；改运行时内部做事件打乱（Node.fz 的维护教训）；在多线程 Tokio 上 fork（AFL 自己的警告）。
- 叙事上的「未被连接的组合」（这正是用户要的「别人还没连起来」的点）：**Lincheck 的 promptCancellation ≈ AfterWake** + **Jepsen 的 `:info` 语义当取消 oracle** + **VOPR 的 liveness mode 当 settle** + **DPOR 式效果等价归并当 plan 去重**。这四者分别来自协程线性化检查、分布式一致性测试、DST、模型检查四个社区。本次检索没有发现任何工作把它们合用于 Rust future 的 drop 取消。

### Gaps
- 没有找到 Andoni/Daniliuc/Khurshid/Marinov「Evaluating the small scope hypothesis」的可访问原文，small-scope 一条只用了间接实证。
- 没有找到针对 async Rust 取消 bug 的 bug-depth 实证研究（例如「多少取消 bug 只需一次取消、一种时序」）。Dropwise 自己的 survey/语料可以填补这一空白，并为第 7 项的概率界提供经验先验。
- Yuan 2014 的 90% / 58% 两个数字来自二手来源，引用前应核对原文。
