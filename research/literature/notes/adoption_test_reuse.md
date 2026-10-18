# 低摩擦采用：复用现有测试与插桩的 bug-finding 工具（1990s–2026）及对 Dropwise 的可迁移机制

> 记号说明：【老】= 2015 年前的工作或机制；【新】= 2015 年后，【很新】= 2024–2026。每条后面的 **Dropwise 启发** 写清三件事：可以借的具体机制、粗略工作量（S = 一天以内，M = 一周以内，L = 一个月以上）、算不算 low-hanging fruit（LHF）。迁移性弱的地方会直接写明。
> 背景假设：Dropwise 的 A′ 路线是给 Tokio 的 `select!`/`timeout` 打上与版本适配的补丁，然后对现有 `cargo test` 注入取消（参见本仓库 `research/g0/probes/D4-tokio-1.17.0-select.diff`：补丁用 `Branch::new(file!(), line!(), fut)` 包住每个分支，并统计 Pending 次数）。
> 本文没有把调研范围限定在某一本书或某一篇综述，各条都尽量引用一手来源；凡是没有在一手来源中核实的内容，都放进 Gaps。

---

## Q1. Sanitizer 模型：如何“编译开关一开，现有测试不改”就被采用？CancelSan 需要什么？哪些先例打过依赖或运行时的补丁来注入行为？

### Takeaway
Sanitizer 类工具能成功，靠的是四件事的组合：一个开关（编译 flag 或依赖替换）、现有测试原样运行、报告自带栈（还有去重和 suppression）、开销可以接受，只在部分测试或 CI 上开。Rust 生态里已经有几乎一一对应的先例：madsim 用 `package = "madsim-tokio"` 加 `RUSTFLAGS="--cfg madsim"`，并声明“正常构建时补丁 crate 与原版相同”；Cargo 支持在 config 文件里写 `[patch]`，官方明说这种写法适合“由外部构建工具自动生成”。所以 CancelSan 最便宜的形态是一个 `cargo dropwise test` 子命令。它从 `Cargo.lock` 读出 tokio 的精确版本，把对应版本的补丁源生成到 `target/` 下，再通过临时 config 注入 `[patch]` 和 `--cfg dropwise`，整个过程不改用户的 `Cargo.toml`。

### Cited Findings

**Sanitizer 家族（Google）**
- 【老，2012】AddressSanitizer（Serebryany, Bruening, Potapenko, Vyukov，USENIX ATC 2012）：平均运行时开销约 73%，内存约 3.4×；在 Chromium 中发现 300 多个此前未知的 bug。采用方式是编译器 flag 插桩，加上运行时库替换 malloc，现有测试和 fuzz 目标不用改。— [USENIX ATC'12](https://www.usenix.org/conference/atc12/technical-sessions/presentation/serebryany)；[PDF](https://usenix.org/system/files/conference/atc12/atc12-final39.pdf)；[Google Research](https://research.google/pubs/addresssanitizer-a-fast-address-sanity-checker/)
  - **Dropwise 启发**：ASan 的成功模式是“一个 flag 就让已有执行变成 oracle”，即把隐式错误（越界）变成显式崩溃并附上栈。Dropwise 也应该把“取消导致的不变量破坏”尽量变成显式失败，比如 drop 时检查义务（obligation）是否完成，失败时 panic 并打印注入点和栈，而不是只依赖测试自己的断言。工作量 M；leak/obligation 检查部分属于 LHF。
- 【老，2009】ThreadSanitizer（Serebryany & Iskhodzhanov, WBIA 2009），后来成为 Go race detector 的运行时。— [ACM DL](https://dl.acm.org/doi/10.1145/1791194.1791203)；Go 博客也确认 Go 的 race detector 基于 TSan 运行时：[Go Blog 2013](https://go.dev/blog/race-detector)
- 【新，2015】MemorySanitizer（Stepanov & Serebryany, CGO 2015）：未初始化内存检测，同样是编译期插桩。— [Google Research](https://research.google/pubs/memorysanitizer-fast-detector-of-uninitialized-memory-use-in-c/)
- 【新】UBSan 提供“报告后继续”（recover）、trap 模式和 suppression 文件等分级选项，让同一工具既能在测试里用，也能在生产里低开销使用。— [Clang UBSan 文档](https://clang.llvm.org/docs/UndefinedBehaviorSanitizer.html)；TSan suppression 机制：[google/sanitizers wiki](https://github.com/google/sanitizers/wiki/ThreadSanitizerSuppressions)
  - **Dropwise 启发**：提供 `DROPWISE_OPTIONS=halt_on_first=0:suppressions=dropwise.supp` 风格的环境变量。suppression 按 `file:line`、crate 或函数路径匹配，用于屏蔽已知可接受的取消点，例如 `ctrl_c()` 分支和 shutdown 信号分支。工作量 S，LHF。

**Go `-race`**
- 【老，2013】Go 1.1 内置 race detector。设置 `-race` 后编译器对所有内存访问插桩；集成到 Go 之后（2012 年 9 月起）在标准库中发现 42 个 race，并已纳入持续构建。race 版二进制的 CPU 和内存开销可达约 10×，所以官方建议不要一直开，而是挑一部分测试（负载测试、集成测试）开启。— [Go Blog: Introducing the Go Race Detector](https://go.dev/blog/race-detector)
  - **Dropwise 启发**：“不必所有测试都开，只在会触发并发路径的集成测试上开”，正好对应 G0 观察到的情况：真正到达取消点、观测到 Pending 的往往是集成或e2e 测试。Dropwise 可以先做一次“baseline 探测运行”，只对实际观察到 Pending 的 select!/timeout 站点所在的测试启用注入。工作量 S（复用现有 D4 探针），LHF。
- 【新，2021–2022】Uber 在约 5000 万行的 Go monorepo（约 2100 个服务）中部署 race 检测。他们没有在 PR 阶段阻断，而是对代码快照离线周期运行全部单元测试，用栈 trace 哈希去重，用启发式找出责任人并自动建 Jira 任务。约 6 个月检测出约 2000 个 race，平均每天约 5 个新报告；210 名工程师用 790 个补丁修复了 1011 个。开启 race 后，P95 测试时长从约 25 分钟升到约 100 分钟。不放在 PR 阶段的理由有两条：检测非确定（PR 引入的 race 可能测不出，无关 PR 反而可能被误挡），以及存量 race 太多。— [Uber Blog: Dynamic Data Race Detection in Go Code](https://www.uber.com/blog/dynamic-data-race-detection-in-go-code/)；论文 Chabbi & Ramanathan, PLDI 2022：[arXiv 2204.00764](https://arxiv.org/pdf/2204.00764.pdf)（数字在不同来源中略有出入，约 1000 和约 1100 的修复数都出现过）
  - **Dropwise 启发**：这是“非确定注入工具在大仓库落地”的最佳模板，按 post-merge、周期运行、栈哈希去重、自动指派的流程来做，不要做 PR gate。Dropwise 的随机单次注入同样是非确定的，应当默认“报告而不阻断”。工作量 M（主要是 CI 脚本和报告去重），LHF。

**泄漏检查（goleak / Go 1.26 goroutineleak）**
- 【新，2017– 】Uber `goleak`：每个测试加 `defer goleak.VerifyNone(t)`，或每个包加 `goleak.VerifyTestMain(m)`，在测试结束时检查残留 goroutine。提供 `IgnoreTopFunction`、`IgnoreAnyFunction`、`IgnoreCurrent`（记录基线时已有的 goroutine 并忽略）等选项；`VerifyNone` 与 `t.Parallel` 不兼容，因为它分不清“泄漏”和“别的测试还没跑完”。— [goleak README](https://github.com/uber-go/goleak)；[pkg.go.dev](https://pkg.go.dev/go.uber.org/goleak)
  - **Dropwise 启发**：(a) `IgnoreCurrent` 的“基线快照差分”思路可以直接用于 Dropwise 的 obligation 检查：注入前先记录未完成的义务集合，只报告注入之后新增的。(b) goleak 与并行测试冲突这一点，正是 Dropwise 所说“观察过早”误报的同类问题，说明检查点必须放在测试或运行时静止之后。工作量 S–M，LHF。
- 【很新，2026】Go 1.26 引入实验性的 `goroutineleak` profile（`GOEXPERIMENT=goroutineleakprofile`）。它借助 GC 可达性判断：goroutine 阻塞在一个已经没有任何可运行 goroutine 能到达的同步原语上，就视为永久泄漏；官方说这能捕获一大类泄漏但不是全部，并计划在 Go 1.27 默认开启。— [Go 1.26 Release Notes](https://go.googlesource.com/website/+show/HEAD/_content/doc/go1.26.md)
  - **Dropwise 启发**：这是解决“观察过早”误报的关键概念：判据不应该是“到某个时刻还没完成”，而应该是“已经不可能再完成”。Tokio 中可近似为两种做法：一是等运行时静止（所有任务 idle、无就绪 I/O，且在 `time::pause` 下没有待触发的定时器）之后再判定义务泄漏；二是检查对应 waker 或 channel 发送端是否已经全部被 drop。工作量 M；“静止后再判定”是 LHF，可达性判定不是。

**Miri（Rust）**
- 【很新，POPL 2026】Jung 等，《Miri: Practical Undefined Behavior Detection for Rust》：在超过 10 万个 Rust 库上评估，Miri 能运行它们合计 70% 以上的测试；发现了数十个真实 bug；已集成进 Rust 标准库和许多知名库的 CI。论文强调 Miri 实现了足够多的 OS API，可以“不改代码地运行真实代码”。— [POPL'26 页面](https://popl26.sigplan.org/details/POPL-2026-popl-research-papers/50/Miri-Practical-Undefined-Behavior-Detection-for-Rust)；[PDF](https://research.ralfj.de/papers/2026-popl-miri.pdf)
  - **Dropwise 启发**：Miri 的采用指标是“能跑起来的现有测试比例”（>70%），而不是只看 bug 数。Dropwise 的 A′ 路线也应该把“在 N 个真实项目上无改动跑通 `cargo dropwise test` 的比例”当作首要可用性指标，G0 里 6 个因旧构建环境受阻的案例恰好就是这个指标的分母问题。工作量 S（只是改评估口径），LHF。

**补丁依赖或运行时的先例（Rust）**
- 【新】madsim：在 `Cargo.toml` 中写 `tokio = { version = "0.2", package = "madsim-tokio" }`，运行 `RUSTFLAGS="--cfg madsim" cargo test`；传递依赖用 `[patch.crates-io]` 指向 fork（getrandom、quanta、tokio-retry、tokio-postgres、tokio-stream 等），并声明“When built normally, these crates are identical to the original ones.” — [madsim README](https://github.com/madsim-rs/madsim)
  - **Dropwise 启发**：照搬“双重开关”：补丁 crate 在 `cfg(not(dropwise))` 下与上游逐字节等价，只有 `--cfg dropwise` 才启用 `Branch` 包装。这样即使用户把补丁常驻在 `Cargo.toml` 里，也没有行为风险。madsim 还表明，要覆盖 `tokio-stream`、`tokio-retry` 等周边 crate，只能逐个 fork，这直接对应 Q3 里“combinator 和自定义 wrapper 覆盖不到”的问题。工作量 M，LHF。
- 【新】Cargo `[patch]`：只有工作区根 `Cargo.toml` 里的 patch 生效，依赖中定义的 patch 会被忽略；可以用 `package` 键对同一 crate 的多个版本同时打补丁；补丁源的版本号必须满足依赖方的版本要求，否则补丁不会被使用。— [Cargo Book: Overriding Dependencies](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html)
- 【新】Cargo 配置文件（`.cargo/config.toml`）中也可以写 `[patch]`，格式相同，优先级高于 `Cargo.toml` 里的 patch。官方原话是这种做法“generally only appropriate when the patch section is automatically generated by an external build tool”。— [Cargo Book: Configuration § patch](https://doc.rust-lang.org/cargo/reference/config.html)
  - **Dropwise 启发**：这就是 `cargo dropwise test` 的实现路径。读 `Cargo.lock` 拿到 tokio 的精确版本（1.17–1.53 都有可能），从 `~/.cargo/registry/src` 复制出该版本源码，到 `target/dropwise/tokio-1.x.y/` 应用对应的补丁 hunk，再生成临时 config 写入 `[patch.crates-io] tokio = { path = ... }` 并设置 `--cfg dropwise`。补丁源版本号与 lock 中完全一致，所以 semver 匹配不会出问题；同一图中如果有多个 tokio 版本，就用 `package` 键各补一份。用户无需改任何文件。工作量 M，**LHF（最高优先级）**。
- 【新】Tokio 自身用 `RUSTFLAGS="--cfg tokio_unstable"` 开启不稳定特性；loom 的用法是 `RUSTFLAGS="--cfg loom" cargo test`，Tokio 内部的同步原语在 `cfg(loom)` 下切换成 loom 版本。— [Tokio docs: unstable features](https://docs.rs/tokio/latest/tokio/#unstable-features)；[loom README](https://github.com/tokio-rs/loom)
  - **Dropwise 启发**：`--cfg` 是 Rust 生态里“全局行为切换”的约定俗成做法，用户熟悉它。但 RUSTFLAGS 一变就会让整个依赖图重新编译（Gap：对大项目，如 Materialize 规模，代价未测）。可以考虑让 `cargo dropwise` 使用独立的 `--target-dir`，避免与正常构建缓存互相冲刷。工作量 S，LHF。
- 【新】shuttle（AWS）：测试体包进 `shuttle::check_random(...)`，并把 `std::sync`/thread 的 import 换成 shuttle 版本；它实现了 PCT 等随机调度器，失败可以确定性重放；它是 unsound 的（通过不代表正确），但比穷举式的 loom 能扩展到大得多的测试。— [shuttle GitHub](https://github.com/awslabs/shuttle)；[docs.rs](https://docs.rs/shuttle)
  - **Dropwise 启发**：shuttle 和 loom 要求改 import、改测试入口，属于“高摩擦”的反例，这正是 Dropwise 现有 `ctx.target/ctx.race` 场景 API 的处境。迁移点只有两个：PCT 式“有界随机加可复现种子”的调度思想，以及“unsound 但可扩展”的定位声明。工作量不适用；弱迁移。

**补丁运行时或二进制来注入行为的先例（非 Rust）**
- 【老，2009】LFI（Marinescu & Candea, DSN 2009）：在共享库与应用的边界拦截调用并注入错误返回值，不需要库源码；通过对库二进制做静态分析，自动推导“库可能返回哪些错误”。后续项目页称它在不写新测试的情况下，把恢复代码的覆盖率提高了 35%–60%，并在 BIND、Git、MySQL、Pidgin 等系统中发现 bug。— [EPFL Infoscience](https://infoscience.epfl.ch/record/139395?ln=en)；[DSLab 项目页](https://dslab.epfl.ch/research/lfi)
  - **Dropwise 启发**：“在库边界注入、自动推导可注入集合、用现有测试驱动”与 A′ 是同构的；LFI 用“恢复代码覆盖率提升”作为无 bug 时的价值度量，Dropwise 同样可以报告“取消路径覆盖”，即在多少个 `.await` 点上实际执行过 drop。但 LD_PRELOAD 式拦截对 Rust 泛型、单态化的 future 不适用，只能在源码层（宏、crate 补丁）做。迁移性：思想强，机制弱。
- 【老→新】Byteman（JBoss）：通过 Java agent 在运行时改写字节码，规则采用 Event-Condition-Action 语言，可以注入抛异常、提前返回等；BMUnit 把它集成进 JUnit/TestNG，用注解声明规则，由框架负责装载 agent 和规则；还能改无法重新编译的库代码。— [Byteman 主页](https://byteman.jboss.org/)；[Programmer's Guide](https://downloads.jboss.org/byteman/3.0.6/ProgrammersGuide.html)
  - **Dropwise 启发**：Rust 没有运行时字节码改写，所以 agent 式“零重编译”的路走不通，宏补丁加重编译就是 Rust 的等价物。可以借鉴的是 BMUnit 的“按测试注解声明注入规则”，可以提供 `#[dropwise::only(sites = "crate::wal::*")]` 一类的可选注解，作为零注解默认之外的精细控制。工作量 M；不算 LHF。
- 【新】Chaos Monkey for Spring Boot（codecentric）：watcher 挂在 `@Service`、`@Controller` 等 Spring 构造型 bean 的 public 方法上，触发 latency、exception、kill app 等 assault；`chaos.monkey.assaults.level` 控制攻击频率（1 = 每个请求，10 = 每 10 个请求一次）。— 只找到第三方材料，官方 README 未取回：[DZone](https://dzone.com/articles/chaos-monkey-for-spring-boot-microservices)；[Gremlin 概述](https://gremlin.com/chaos-monkey-alternatives/spring-boot)
  - **Dropwise 启发**：“按框架约定的挂点自动注入”加上“频率旋钮”。Dropwise 的挂点就是 Tokio 的宏和 `timeout`，旋钮可以是 `DROPWISE_RATE=1/N` 或“每站点每次运行至多一次”。工作量 S，LHF。

### Inferences
- CancelSan 的最小可行形态是 `cargo dropwise test [-- <cargo test args>]`，由五件事组成：(1) 解析 `Cargo.lock` 得到 tokio 精确版本；(2) 从本地 registry 复制源码并应用该版本对应的补丁（只改 `macros/select.rs`、`macros/support.rs` 和 `time/timeout.rs` 几处）；(3) 生成临时 cargo config 注入 `[patch]` 和 `--cfg dropwise`；(4) 使用独立的 target-dir；(5) 测试照常运行，注入行为由环境变量（种子、速率、站点过滤）控制。这完全是 madsim 和 Cargo 官方文档背书的机制组合，不需要用户改代码，也不需要发布 fork 到 crates.io。
- Tokio 1.17–1.53 的版本跨度：补丁面越小、越稳定，适配成本越低。建议把补丁做成“在 select! 的 `( $( $fut , )+ )` 元组构造处包一层”和“在 `Timeout::poll` 中加一个钩子”这两个锚点，按 minor 版本维护少量补丁变体，用脚本对每个 minor 版本跑一遍冒烟测试。这是推断，具体有几个变体需要实测 `select.rs` 的历史 diff。
- madsim 对周边 crate 逐个 fork 的维护负担说明，“对每个 combinator 打补丁”不可持续。Q3 和 Q2 中给出了更通用的 leaf 注入思路。

### Gaps
- 没有找到 RUSTFLAGS 改变导致全量重编译对 Rust 大型项目（Materialize、Databend、RisingWave）的实际耗时数据。
- 没有核实 goleak 内部的“重试并等待 goroutine 退出”的具体参数（README 和 pkg.go.dev 页面都未说明；有说法称存在 `MaxRetryAttempts`/`MaxSleepInterval`，未在一手文档中确认）。
- Chaos Monkey for Spring Boot 的官方文档没有取回，assault 种类和默认值以第三方材料为准，各版本之间也有出入。
- 没有找到 Rust `-Zsanitizer` 在生态中的采用率数据。

---

## Q2. 用现有测试做注入后的 oracle：它们如何区分“真 bug”和“测试本身对故障敏感（脆弱）”？

### Takeaway
最可迁移的范式是 NonDex：只探索规格允许的行为。它先在不扰动的情况下跑一次，过滤掉本身就失败的测试，只报告“无扰动通过、扰动后失败”的测试；然后用二分法定位到单个扰动调用点，并提供种子用于复现。它对 Dropwise 的意义很直接：Future 契约允许任何时刻返回 Pending（Tokio 的 coop budget 在生产中就会这么做），所以“额外注入 Pending、让其它分支先完成”属于规格允许的扰动，测试在这种扰动下失败，原则上就是代码对调度做了错误假设。第二个范式是 Filibuster 的条件断言（`was_fault_injected_on(...)`），用来处理那些在故障下本来就该表现不同的测试；它在 Netflix 示例中跑了 1606 个测试，只需要 9 条条件断言。

### Cited Findings

**NonDex：最接近的类比**
- 【新，FSE 2016 Demo；前作 ICST 2016】Gyori, Lambeth, Shi, Legunsen, Marinov，《NonDex: A Tool for Detecting and Debugging Wrong Assumptions on Java API Specifications》。要点如下。
  - 在现有测试下随机探索 41 个规格欠定（underdetermined）的 Java API 的合法行为，例如 `HashSet` 的迭代顺序。
  - 只需在 `pom.xml` 加几行，或直接运行 `mvn edu.illinois:nondex-maven-plugin:<ver>:nondex`。
  - detector 先在无随机化的情况下跑一遍所有测试，再用不同种子重跑多次，只报告“不扰动通过、扰动失败”的测试，因为本身就失败的测试另有原因。
  - 参数包括 `nondexSeed`、`nondexRuns`、`nondexMode`（ONE 或 FULL，ONE 的确定性更高）。
  - `.nondex/` 目录记录每次执行的配置、复现所需的种子，以及选择生成器被调用的次数。
  - debugger 用二分法在 API 调用区间中只对一半启用扰动；被禁用的调用仍然推进随机数生成器，以保证同一种子下行为可复现。二分最终收敛到单个调用点；如果在单个测试方法内收敛不了，就扩大到整个测试类，再到整个测试套件。在 75 个失败中，有 74 个被最小化到单个调用点。
  - 结果：在 8 个 GitHub 项目中发现 21 个新 bug；提交 13 个 PR，接受 12 个；Checkstyle 把 NonDex 集成进了自己的构建。
  - 论文提到 Google 内部有一个更早的同类实现，只覆盖一个 API、只有一个模式、未开源。
  - — [NonDex FSE'16 PDF](https://mir.cs.illinois.edu/gyori/pubs/nondex.pdf)；[GitHub](https://github.com/TestingResearchIllinois/NonDex)；前作 Shi et al., ICST 2016《Detecting assumptions on deterministic implementations of non-deterministic specifications》：[Illinois Experts](https://experts.illinois.edu/en/publications/detecting-assumptions-on-deterministic-implementations-of-non-det/)
  - **Dropwise 启发（最重要的一条）**：照搬三件事。(1) baseline-first：每个测试先无注入跑 k 次（例如 3 次），不稳定的直接排除，报告条件严格限定为“baseline 全过、注入后失败”。(2) 种子加调用计数：注入决策由 `(seed, site, 该站点第 n 次 Pending)` 决定，持久化到 `.dropwise/<exec-id>/`。(3) 二分 debugger：如果一次运行注入了多个点，就按 NonDex 的方法二分启用集合，被禁用的站点仍然消耗随机数，最终输出单个 `file:line#n` 加栈。工作量 M；baseline-first 和种子都是 S 级的 LHF。
- 【新，ICSE 2021】DexFix：在 200 个项目上运行 NonDex，检测出 275 个因错误假设而失败的测试，并为它们做领域特定的自动修复。— [ICSE 2021 页面](https://2021.icse-conferences.org/details/icse-2021-papers/84/Domain-Specific-Fixes-for-Flaky-Tests-with-Wrong-Assumptions-on-Underdetermined-Speci)
  - **Dropwise 启发**：说明“扰动 + 现有测试”在规模化后会产出大量有效报告，下一步自然是自动修复建议。Dropwise 未来可以针对常见模式给出修复提示，例如“把 `recv().await` 移出 select! 循环”“使用 `tokio::pin!` 加 `&mut fut` 保持跨迭代”“改用 cancel-safe API”。工作量 L，不是 LHF。

**规格允许的扰动在 Tokio 中已经存在**
- 【很新，Tokio 1.53.3 文档】`select!` 默认随机挑选先检查的分支（`biased;` 可以关闭这一行为），完成一个分支后取消其余分支；文档要求：如果一个未完成的 future 被 drop 后重新创建，必须是无副作用的。— [tokio::select! docs](https://docs.rs/tokio/latest/tokio/macro.select.html)
- 【新】Tokio 的协作调度预算：`poll_proceed` 会递减任务预算，预算耗尽时返回 `Poll::Pending`；`unconstrained` 可以让某个 future 不受此约束。— [tokio::task::coop docs](https://docs.rs/tokio/latest/tokio/task/coop/index.html)
- 【新】`futures-test` 的 `FutureTestExt::interleave_pending()` 在每次 poll 之间插入一个额外的 `Poll::Pending`，`pending_once()` 则只在第一次 poll 前插入一次；`assert_unmoved()` 在 poll 和 Drop 时检查 future 没有被移动。— [futures-test docs](https://docs.rs/futures-test/latest/futures_test/future/trait.FutureTestExt.html)
  - **Dropwise 启发**：这三条一起构成 Dropwise 的“规格允许论证”。生产中的 Tokio 本来就会因为 coop budget 在任意资源上返回 Pending，`select!` 本来就随机化分支顺序，所以 Dropwise 只要额外制造 Pending，再在 paused time 下让另一个分支（定时器）就绪，被测代码就不能依赖“这个分支不会输”。这让测试失败可以直接归因到代码，而不是注入不合法，可以写进 README 作为“误报为什么少”的说明。另外，在 Tokio 的 `coop::poll_proceed` 这一 leaf 层注入 Pending 是 combinator 无关的：`futures::select!`、`FuturesUnordered` 和自定义 wrapper 内部的 Tokio 资源都会受影响，可以部分缓解“非 tokio 宏站点覆盖不到”的问题，这一点是推断，需要实验验证。工作量 S（文档论证）或 M（leaf 注入），LHF。

**Filibuster：用现有功能测试驱动服务级故障注入**
- 【新，SoCC 2021】Meiklejohn, Estrada, Song, Miller, Padhye，《Service-Level Fault Injection Testing》。要点如下。
  - 从“已经通过的功能测试”出发，Filibuster 基于 OpenTelemetry 风格的插桩，对 Python `requests` 和 Flask 做扩展，在客户端调用点注入异常或错误响应；`requests` 一个库就有 23 种可能抛出的异常。
  - 每次执行只注入一组故障，每组故障都对应一次完整的测试重跑。
  - 测试在注入下失败后，由开发者用辅助模块写条件断言，例如“如果在 stats 服务上注入了故障，仍应能播放有声书”，再用 counterexample 文件重放失败用例来验证这些断言。
  - Netflix 示例执行了 1606 个测试，只需 9 条条件断言就覆盖了全部行为；语料中所有 bug 都在开发环境中被找到。
  - 论文明确假设测试没有可观察的调度和数据非确定性，理由是：如果有，注入前它就不会稳定通过。
  - 超时类注入会多等 1ms，累积起来可能产生误报，作者认为这是可以接受的折中。
  - 静态分析采用过近似，可能促使开发者为生产中不可能出现的故障写处理代码。
  - — [SoCC'21 PDF](https://rohan.padhye.org/files/filibuster-socc21.pdf)；条件断言 API `filibuster.assertions.was_fault_injected()`、`was_fault_injected_on("payments")`：[作者博客](https://christophermeiklejohn.com/filibuster/2021/10/14/filibuster-4.html)
  - **Dropwise 启发**：提供 `dropwise::was_cancelled()` 和 `dropwise::was_cancelled_at("wal::recv")`，供用户在少数“本来就应该在取消下表现不同”的测试里写条件断言，例如测试断言请求成功，而注入使 `timeout` 分支获胜。默认零注解；只有当报告被用户判定为“测试脆弱”时，才引导用户加一行条件断言，而不是写 suppression。1606 个测试对 9 条断言这个比例说明，维护成本可以很低。工作量 S，**LHF**。
- 【新，2022】DoorDash 落地：Meiklejohn 在实习期间把 Filibuster 扩展到 Kotlin 和 gRPC；Java 版插桩与 JUnit、JaCoCo 集成，支持 HTTP 和 gRPC。— [InfoQ 2022](https://www.infoq.com/news/2022/09/filibuster-reliability-doordash/)；[DoorDash 博客](https://careersatdoordash.com/?p=7163)；[filibuster-java-instrumentation](https://github.com/filibuster-testing/filibuster-java-instrumentation)
  - **Dropwise 启发**：工业落地的主要工作量在“为每个通信库补插桩”，与 Dropwise 必须为每个 combinator 或 Tokio 版本做适配是同一类问题。应当优先覆盖调用量最大的一两个挂点（`tokio::select!`、`tokio::time::timeout`），其余用 leaf 注入兜底。弱迁移，但能帮助设定优先级预期。

**其它把现有测试当 oracle 的工作**
- 【老，OSDI 2014】Yuan 等，《Simple Testing Can Prevent Most Critical Failures》：在 Cassandra、HBase、HDFS、MapReduce、Redis 的 198 个用户报告的失败中，92% 的灾难性失败（共 48 个）源于对显式报告的非致命错误处理不当；58% 的灾难性失败本可以通过对错误处理代码的简单测试发现。作者的静态检查器 Aspirator 发现了 143 个新 bug 或坏实践。— [OSDI'14 PDF](https://www.eecg.utoronto.ca/~stumm/Papers/Yuan-OSDI-14.pdf)；[;login: 版](https://www.usenix.org/publications/login/feb15/yuan)
  - **Dropwise 启发**：取消路径是 Rust async 里的“隐形错误处理路径”，类似 Java 的 catch 块，几乎没有被测试过。这篇论文为“现有测试加简单注入就能抓到大部分”提供了动机，可以用在 Dropwise 的 README 或论证里。没有工程工作量。
- 【新，ICST 2019】iDFlakies（Lam 等）：通过对测试重新排序并重跑来检测 flaky 测试，并部分区分顺序相关（OD）与非顺序相关（NOD）；构建了 422 个 flaky 测试的数据集，OD 和 NOD 约各占一半。后续的 ISSRE 2020 工作对每个测试套件重跑 4000 次，发现许多 NOD 实际上也与顺序有关。— [iDFlakies PDF](https://cs.gmu.edu/~winglam/publications/2019/LamETAL19iDFlakies.pdf)；[ISSRE 2020](https://cs.gmu.edu/~winglam/publications/2020/LamETAL20ISSRE.pdf)
  - **Dropwise 启发**：(a) baseline 的重跑次数决定了误把 flaky 当成 bug 的概率，而 NOD flaky 需要大量重跑才能暴露，所以 Dropwise 对重 e2e 测试的 baseline 预算要单独设置。(b) 可以提供“分类器”式报告：在相同种子但不注入的情况下重跑 k 次仍然通过，并且注入下能稳定复现，才标为 likely-bug；否则标为 inconclusive。工作量 S，LHF。
- 【新，Go 1.17+】`go test -shuffle=on|N`：工具链内置随机化测试顺序，并打印种子以便复现。— [cmd/go Testing flags](https://pkg.go.dev/cmd/go#hdr-Testing_flags)
  - **Dropwise 启发**：再次说明“随机化加打印种子”是标准做法，`cargo dropwise` 应在失败时打印 `DROPWISE_SEED=… cargo test <name>` 这样可直接复制的复现命令。工作量 S，LHF。
- 【新，EMSE 2019】DSpot（Danglot, Vera-Pérez, Baudry, Monperrus）：对 10 个成熟开源项目中 40 个测试类做测试放大（amplification，即变换输入并生成新断言），在 26/40 的情况下提升了测试；向开发者提交了 19 个改进，13 个被接受合并。— [arXiv 1811.08330](https://arxiv.org/abs/1811.08330)
  - **Dropwise 启发**：DSpot 说明“以现有测试为种子做放大”能产出开发者愿意合并的成果，但它的放大是生成新测试，迁移到 Dropwise 时成本偏高。可以借的一小点是：当 Dropwise 发现某个注入导致失败时，自动生成一个“最小化的回归测试骨架”，包含原测试名、注入点和种子，方便开发者直接提交到仓库。工作量 M，不算 LHF。
- 【老→新】FATE/DESTINI（Gunawi 等，NSDI 2011）：拦截所有 I/O 并注入崩溃、网络分区、磁盘错误等故障，以 failure ID 记住已探索的故障；正确恢复的规格用 Datalog 声明，每个检查大约 5 行；共探索 4 万多个故障场景，写了 74 条规格，在 HDFS、ZooKeeper、Cassandra 中发现 16 个新 bug，复现 51 个旧 bug。— [USENIX NSDI'11](https://usenix.org/conference/nsdi11/fate-and-destini-framework-cloud-recovery-testing)；[技术报告摘要](https://www2.eecs.berkeley.edu/Pubs/TechRpts/2010/EECS-2010-127.html)；[slides](https://static.usenix.org/events/nsdi11/tech/slides/gunawi.pdf)
  - **Dropwise 启发**：(a) failure ID 等价于 Dropwise 的 cancellation ID，可以定义为 `(site file:line:col, 分支序号, 该站点第 n 次 Pending, 调用栈哈希)`。(b) 当测试断言不足时，借鉴 DESTINI 的“短小声明式恢复规格”，即 Dropwise 现有的 invariant，作为可选的增强 oracle，而不是必需项。工作量 S，LHF。
- 【新，2016–2018】Netflix ChAP：在生产环境把一小部分真实流量（基线和金丝雀集群各约占原集群的 1%）分流到 control 和 experiment 两组，只对 experiment 注入故障，比较的是用户侧 KPI（每秒播放数），而不是服务自身的错误数；chaos 暴露总流量上限为 5%，并设有全局“停止所有 chaos”按钮。— [Basiri et al., arXiv 1702.05849](https://arxiv.org/pdf/1702.05849)；[InfoQ 2018](https://www.infoq.com/news/2018/07/chaos-eng-netflix)
  - **Dropwise 启发**：ChAP 的“control 与 experiment 成对，比较高层 KPI”对应 Dropwise 已有的 paired controls：同种子、仅有无注入之差。对 sqllogictest、testdrive 这类 e2e harness，oracle 可以直接用 harness 的整体结果（某个 `.slt` 文件是否通过）作为 KPI，不需要深入理解每个断言。工作量 S，LHF。
- 【新，2018】LinkedIn LinkedOut：在 Rest.li 请求过滤器层注入错误或延迟（开源部分在 r2-disruptor 和 restli-disruptor 中）；通过 LiX（A/B 测试和特性开关框架）或请求的 invocation context 定向触发，范围可以小到单个用户请求。— [LinkedIn Engineering 2018](https://engineering.linkedin.com/blog/2018/05/linkedout--a-request-level-failure-injection-framework)；LiX 的定向细节只见于二手报道：[iThome](https://www.ithome.com.tw/news/124117)
  - **Dropwise 启发**：把“请求上下文定向”迁移成“任务上下文定向”：只对当前测试 `#[tokio::test]` 运行时里、由被测 crate 代码创建的 select! 注入，不碰 harness 自身（例如 sqllogictest runner 和测试框架里的 select!）。具体做法是在补丁的 `Branch::new` 中用 `file!()` 判断站点是否在被测 workspace 内，这样无需运行时成本。工作量 S，LHF。

### Inferences
- Dropwise 的推荐 oracle 是分层组合，不需要用户写场景。
  1. 测试自身的断言、panic 和非零退出，前提是 baseline 稳定通过（NonDex 式差分）。
  2. 挂起或超时：超时阈值由 baseline 时长乘以系数得出，参考 cargo-mutants 的做法（见 Q3）。
  3. 测试结束时，在运行时静止之后进行义务或泄漏检查，采用 goleak 的 `IgnoreCurrent` 式差分，并借鉴 Go 1.26 的“不可能再完成”判据。
  4. 可选的条件断言 `was_cancelled_at` 和可选的不变量（Filibuster 与 DESTINI 的做法）。
- 对“测试本来就该在故障下失败”的问题：由于 Dropwise 注入的是规格允许的 Pending，而不是错误返回，所以与 Filibuster 注入 503 相比，测试“合法失败”的情况应该少得多。主要的合法失败来源是“注入的延迟让 timeout 分支获胜，而测试断言成功”。对此可以把报告分成两类：一类是在 timeout 分支获胜下失败，可能是测试脆弱，建议加条件断言；另一类是在另一业务分支获胜下失败、出现数据或义务不一致、或挂起，判为高可信 bug。这一分类是推断，需要在 G0 的 4 个确认案例上验证。
- 种子、单点注入、二分定位这三者，能把“报告即可复现、即可最小化”做成默认能力，这是 NonDex 被 Checkstyle 接纳的关键前提之一（推断）。

### Gaps
- 没有找到 NonDex 在 ONE 与 FULL 两种模式下误报率的定量比较，论文把细节放在 ICST 2016 的前作里，本次未取回。
- 没有取回 DSpot 论文中开发者拒绝 PR 的原因细节。
- 没有找到 Filibuster 在 DoorDash 落地时“条件断言数与测试数之比”的工业数据，只有论文语料中的 1606 对 9。
- 没有找到 Gremlin 一手的技术资料，它主要是商业产品，迁移价值低，因此跳过。

---

## Q3. 重复重跑的成本：fork server、快照、checkpoint、persistent mode、测试选择、站点去重、采样和跨 CI 摊销

### Takeaway
对 Dropwise 来说，真正的 LHF 不在快照或 fork 这类系统级技巧。原因有两条：Tokio 多线程运行时与 `fork()` 相性差；重型 e2e harness 往往跨进程甚至跨语言。LHF 在于“少跑”：(1) 用 baseline 探测运行做测试和站点选择，只重跑实际观察到 Pending 的测试（PIT 和 Google 突变测试的“只对被覆盖的代码运行”）；(2) 每次运行在多个不同站点各注入至多一次，并用种子轮换，把穷举计划摊到每天的 CI 运行里（Go `-race` 和 Uber 的离线周期运行）；(3) 用 Filibuster 和 FATE 式的站点等价类剪枝；(4) 在 Tokio `time::pause` 下让注入引起的等待不消耗墙钟时间。snapshot 和 fork-server 类技术迁移弱，不算 LHF。

### Cited Findings

**进程或快照复用**
- 【老→新】AFL/AFL++ fork server 与 persistent mode：persistent mode 在一个长寿进程内循环执行多个输入（`__AFL_LOOP`），免去每次 fork，AFL++ 文档称通常快 10–20 倍；要求目标可以重复调用，并且每轮状态完全重置，可用 UI 中的 stability 指标检查是否有残留状态。社区案例（Bitcoin）在启用 deferred forkserver 加 `__AFL_LOOP(1000)` 后，从约 50 exec/s 提升到约 1.3–1.6 万 exec/s，属于社区基准，并非官方数据。— [AFL++ README.persistent_mode](https://android.kernel.org/platform/external/AFLplusplus/+/HEAD/instrumentation/README.persistent_mode.md)；[Bitcoin PR 镜像](https://mirror.b10c.me/bitcoin-bitcoin/10415)
  - **Dropwise 启发**：persistent mode 的等价物是“在同一进程内对同一测试函数重复执行多次计划”。Rust 的 libtest 不支持重复执行同一测试，但 Dropwise 可以通过补丁后的 `#[tokio::test]` 宏（它本来就负责构造运行时）在宏展开内部循环：每次新建运行时，用不同计划跑测试体，以此省掉进程启动和链接开销。但这要求测试体可重入、没有全局状态（与 AFL 的 stability 要求相同），所以必须做“首轮与次轮结果一致”的 stability 检查，不一致就回退到每计划一个进程。工作量 M；对轻量单元测试是 LHF，对 e2e 无效。
  - Fork server 本身：在多线程 Tokio 运行时启动后再 fork，只会复制调用线程，所以“在 setup 之后 fork”对 `rt-multi-thread` 基本不可行；只能在运行时创建之前 fork，收益有限。迁移弱。（推断：基于 POSIX `fork` 只复制调用线程的语义，本次未专门检索来源。）
- 【新，USENIX Security 2021】Nyx（Schumilo 等）：基于 hypervisor 的快速快照恢复，每秒可以重载被测系统数千次，作者认为这是性能的关键；共发现 44 个新 bug，申请了 22 个 CVE。后续 Nyx-Net 把这套方法扩展到网络服务。— [USENIX Sec'21](https://www.usenix.org/conference/usenixsecurity21/presentation/schumilo)；[PDF](https://usenix.org/system/files/sec21-schumilo.pdf)
  - **Dropwise 启发**：快照可以让“在第 n 次 Pending 处分叉出多个计划”变得廉价，理论上能把前缀执行摊销掉，但它需要 KVM 或专用 hypervisor 环境，工程量和用户摩擦都极高。只适合作为研究方向，不是 LHF。CRIU（[criu.org](https://criu.org/Main_Page)）作为用户态 checkpoint/restore 工具同理：对含 socket 和子进程（pytest、数据库）的 e2e 测试恢复很脆弱，迁移弱。
- 【新】cargo-nextest 默认每个测试一个进程，并支持重试和 flaky 标记。— [nextest: why process-per-test](https://nexte.st/docs/design/why-process-per-test/)；[nextest retries](https://nexte.st/docs/features/retries/)
  - **Dropwise 启发**：每计划一个进程的执行器可以直接复用 nextest（`cargo nextest run --exact <test>` 加环境变量传计划），获得并行、超时、JUnit 报告和重试判定，不用自己写 runner。工作量 S，LHF。

**测试选择与覆盖引导**
- 【新，ICSE-SEIP 2018；TSE 2021】Google 突变测试（Petrović & Ivanković）：采用基于 diff 的概率性方法，不对没有语句覆盖的行和启发式判定为 arid（无趣）的节点生成突变体，结果在 code review 中展示；约 6000 名工程师在日常评审中使用，覆盖全 Google 约 30% 有覆盖数据的 diff（摘要中作者数在不同版本里分别是 13000 和 14000）。— [Google Research](https://research.google/pubs/state-of-mutation-testing-at-google/)；[PDF](https://web.eecs.umich.edu/~weimerw/2022-481W/readings/mutation-google.pdf)；后续《Practical Mutation Testing at Scale》：[arXiv 2102.11378](https://arxiv.org/pdf/2102.11378v1)
  - **Dropwise 启发**：(a) 只对 baseline 中被覆盖、并且确实返回过 Pending 的站点注入，这就是 Dropwise 的“observed Pending”过滤，应设为默认。(b) arid 站点启发式：`select!` 中一侧是 `ctrl_c()`、`shutdown.recv()`、`sleep(大常量)` 的分支，以及测试代码内部的 select!，默认不注入或降权。(c) diff 模式：只对 PR 改动的文件或函数中的站点注入。工作量 S–M，**LHF**。
- 【新】cargo-mutants：先在临时目录对未修改的源码树做一次 baseline 构建和测试，确认测试本来能过，并据此自动设定超时；`--baseline=skip` 可以在分片 CI 中省掉重复的 baseline。此外支持 nextest、分片（`--shard`）和原地运行（in-place）等模式。— [cargo-mutants: baseline](https://mutants.rs/baseline.html)；[mutants.rs](https://mutants.rs/)
  - **Dropwise 启发**：可以直接照搬三处。(1) baseline 自动推导超时，例如 `max(5×baseline, baseline+30s)`，用来判定注入导致的挂起，挂起本身就是一种 oracle（例如 lost wakeup 和取消后的死锁）。(2) `--shard k/n`，用于在 CI 中并行分摊计划。(3) `--in-diff`（Gap：本次没有取回 cargo-mutants 的 in-diff 文档页）。工作量 S，LHF。
- 【新，ISSTA 2015】Ekstazi（Gligoric, Eloussi, Marinov）：基于动态文件依赖的回归测试选择，只重跑依赖于已改动文件的测试。— [ACM DL](https://dl.acm.org/doi/10.1145/2771783.2771784)；[ekstazi.org](http://ekstazi.org/)
  - **Dropwise 启发**：baseline 探测运行时顺带记录“测试到站点集合”的映射，缓存在 `target/dropwise/sites.json` 中。之后只对改动触及的站点所在测试重新注入，这正是 Ekstazi 思路在取消站点粒度上的版本。工作量 M，LHF（如果探测运行已经存在）。
- 【新】PIT（Java 突变测试）：利用覆盖数据，只对覆盖了被突变代码的测试运行该突变；支持增量分析。— [pitest.org](https://pitest.org/)；[incremental analysis](https://pitest.org/quickstart/incremental_analysis/)
  - **Dropwise 启发**：与上面相同，体现为“站点到测试”的反向索引。另外，PIT 的“先跑最快的覆盖测试，一杀即停”可以对应到 Dropwise：同一站点的注入计划按测试耗时升序执行，一旦某个测试暴露该站点的 bug，就停止对该站点的其余测试，因为后续只会产生重复报告。工作量 S，LHF。

**去重和剪枝**
- 【新，SoCC 2021】Filibuster 的 dynamic reduction 依据“服务封装”：调用者只能通过中间服务的响应看到深层故障，所以已经观察过的组合可以推导、不必重跑。在 Audible 语料上，需要的 69 个测试只跑了 31 个（约 55% 的削减）；Netflix 宽扇出示例从 1606 个只降到 1603 个（低于 0.2%），而在带 bug 的较深 Netflix 图中只需执行 25%。作者指出该算法一般来说不 sound，前提是响应确定、不依赖先前故障，并且没有可观察的非确定性。— [SoCC'21 PDF](https://rohan.padhye.org/files/filibuster-socc21.pdf)；[博客](https://christophermeiklejohn.com/filibuster/2021/10/14/filibuster-4.html)
  - **Dropwise 启发**：Dropwise 的组合爆炸来自“多站点、每站点第 n 次 Pending”。可以借用的等价类有两种：(a) 同一站点、同一 Pending 序号 n 之后的行为大多同构，可以只测 n=1、n=2 和“最后一次”，属于 Filibuster 式的“结果已知就不重跑”；(b) 同一站点在不同测试中只需在最便宜的测试里先测。不要追求多点组合，单点注入覆盖了绝大多数已知取消 bug，这一点需要用 G0 的 22 个案例核对。工作量 S，LHF。
- 【老，NSDI 2011】FATE 的剪枝策略带来约一个数量级的加速，发现的 bug 数与不剪枝相同（这一说法只出现在 slides 中）。— [NSDI'11 slides](https://static.usenix.org/events/nsdi11/tech/slides/gunawi.pdf)
- 【新，SIGMOD 2015 / SoCC 2016】Lineage-driven fault injection（LDFI；Alvaro 等）在 Netflix 落地：基于已有的请求追踪和故障注入基础设施，用一次成功执行的 lineage 推导“哪些故障组合可能破坏结果”，以此代替随机注入或专家直觉。— [Alvaro et al., SoCC 2016（NSF PAR）](https://par.nsf.gov/servlets/purl/10053503)；[InfoQ 2016](https://www.infoq.com/news/2016/03/failure-testing-netflix)
  - **Dropwise 启发**：Dropwise 的 baseline trace（哪个 select! 在何时 Pending，被哪个分支赢下）就是 lineage。可以只对“输掉分支持有未提交状态”的站点注入，例如分支 future 内部已经发生过一次 Ready 的子 await、已经从 channel 取出消息或已经写了一半缓冲。这需要补丁记录“分支内部已推进的 poll 次数”，D4 的 `Branch.pending` 计数器已经接近这个能力。工作量 M；方向正确，但不是第一批 LHF。

**采样和摊销**
- 【老→新】Go `-race`：官方建议只在部分测试上开启，并纳入持续构建，让它持续捕获新引入的问题；Uber 采用离线周期运行全部单元测试的方式（见 Q1）。— [Go Blog](https://go.dev/blog/race-detector)；[Uber Blog](https://www.uber.com/blog/dynamic-data-race-detection-in-go-code/)
- 【新】Chaos Monkey for Spring Boot 的 `assaults.level`（每 N 个请求攻击一次）。— [DZone](https://dzone.com/articles/chaos-monkey-for-spring-boot-microservices)
- 【新】NonDex 的 `nondexRuns`：每次运行用由主种子派生的不同种子重跑。— [NonDex PDF](https://mir.cs.illinois.edu/gyori/pubs/nondex.pdf)
  - **Dropwise 启发（摊销策略）**：对重型 e2e 测试（sqllogictest、testdrive、pytest 驱动的服务）不做“每计划一个进程”，而是在一次运行中对每个站点至多注入一次，多个站点可以同时注入；每次 CI 运行使用新种子（例如以日期或提交号为种子），让“站点 × Pending 序号”的空间在数周的 CI 中逐步被覆盖。发现失败后，再用 NonDex 式二分把同次运行中的多个注入点缩小到一个。代价是多点同时注入可能互相掩盖或叠加，所以报告必须经过二分确认之后才标为 likely-bug。工作量 M，**LHF（对重 harness 而言，这是唯一现实的方案）**。

**墙钟时间**
- 【新】Tokio `time::pause()` 和 `#[tokio::test(start_paused = true)]`：暂停时间后，运行时在空闲时自动推进到下一个定时器。— [tokio::time::pause docs](https://docs.rs/tokio/latest/tokio/time/fn.pause.html)
  - **Dropwise 启发**：如果测试本身使用 paused time，那么“注入 Pending 直到 timeout 分支获胜”几乎不耗墙钟时间；如果测试使用真实时间，就需要为 Dropwise 注入引入的“额外延迟”设上限（例如 timeout 时长加 ε），这与 Filibuster“超时注入多等 1ms 可能导致误报”的问题相同。工作量 S，LHF。

### Inferences
- 推荐的成本阶梯，从便宜到贵：
  1. baseline 探测运行（一次），得到“测试 → 站点 → Pending 次数”的索引。
  2. 只对观察到 Pending 的非 arid 站点生成计划。
  3. 每个站点先在最便宜的测试上试，暴露即停。
  4. 轻量测试用宏内循环（persistent 风格），并做 stability 检查。
  5. 重型 e2e 测试每次运行每站点注入一次，种子跨 CI 轮换，失败后二分确认。
  6. 快照、fork 和 CRIU 暂不做。
- “每站点、每次运行至多一次”在 Go `-race`、Chaos Monkey 和 NonDex 的组合里都有先例，而 Dropwise 现有的穷举式“每个计划重跑一次”在重 harness 上不可持续。两种模式可以共存：`--exhaustive` 用于小测试，`--sample` 用于 CI。

### Gaps
- 没有找到“在多线程 async 运行时中做 fork-server”的成功先例或研究，上文对 fork 不可行的判断基于 POSIX 语义推断，没有检索一手来源。
- 本次没有取回 cargo-mutants 关于 `--in-diff`、`--shard` 和 timeout 系数的具体文档页，只确认了 baseline 页的内容。
- 没有找到 Nyx 每秒执行次数的具体数字，摘要只说每秒可以重载数千次。
- “单点注入能覆盖绝大多数取消 bug”尚无文献支持，需要用 Dropwise 自己的 22 个案例统计。

---

## Q4. 报告与分诊：去重、最小复现、确定性重放种子，如何降低误报和维护成本

### Takeaway
成熟工具的报告有三个共同点：(1) 以站点或栈为键去重（Uber 用栈哈希，sanitizer 有 suppression，FATE 有 failure ID）；(2) 每份报告都附一条可直接执行的复现命令或种子，并把失败用例持久化，下次优先重放（proptest 的 `proptest-regressions/`、NonDex 的 `.nondex/`、Filibuster 的 counterexample 文件、rr 的录制、Antithesis 的确定性重放）；(3) 在开发者已经在看的地方展示，并且默认不阻断（Google 突变测试放在 code review，Uber 自动建 Jira）。

### Cited Findings
- 【新】proptest failure persistence：失败用例按源文件保存到 `proptest-regressions/` 目录树，保存的是产生失败的种子而不是值，这样即使 strategy 改了，种子仍能产生合法输入；之后每次运行先重放已保存的用例，再生成新的随机用例；官方建议把这些文件提交到版本控制。— [proptest book: Failure Persistence](https://proptest-rs.github.io/proptest/proptest/failure-persistence.html)
  - **Dropwise 启发**：提供 `dropwise-regressions/<test>.txt`，每行一个 `(seed, site, n)`。`cargo dropwise test` 总是先重放这些用例，用户可以提交到仓库，这样 CI 中一次偶然发现的取消 bug 就不会“下次消失”。保存种子加站点描述而不是绝对行号，可以更好地承受代码移动（建议用 `函数路径 + 站点在函数内的序号` 做键）。工作量 S，**LHF**。
- 【新】NonDex `.nondex/` 目录保存每次执行的配置、种子和调用计数；debug 阶段输出复现失败的命令行参数，以及唯一调用点的栈；如果不能收敛到单点，就报告“可能因其它原因 flaky”。— [NonDex PDF](https://mir.cs.illinois.edu/gyori/pubs/nondex.pdf)；[GitHub](https://github.com/TestingResearchIllinois/NonDex)
  - **Dropwise 启发**：报告模板可以包括以下内容：测试名、注入站点 `file:line`（落在用户 crate 内的那一层）、被取消的分支序号和第 n 次 Pending、赢下的分支、被 drop 的 future 的创建栈与 drop 栈、失败类型（断言、panic、挂起、义务泄漏）、复现命令、baseline 稳定性（k/k 次通过）。不能收敛的报告标为 inconclusive，不进入默认输出。工作量 S，LHF。
- 【新】Uber race 报告：每条包含冲突地址、两条访问栈、读写类型；用栈哈希去重；用启发式确定责任人，自动建任务；前 2–3 个月由团队主动督促修复。调查中 52% 的工程师认为有用，8% 认为没用。— [Uber Blog](https://www.uber.com/blog/dynamic-data-race-detection-in-go-code/)
  - **Dropwise 启发**：去重键可以用 `(被取消站点栈哈希, 失败签名)`，失败签名取 panic 消息或断言位置。一个站点导致多个测试失败时，只报一次，并列出受影响的测试。工作量 S，LHF。
- 【新，USENIX ATC 2017】rr（O'Callahan 等，《Engineering Record And Replay For Deployability》）：录制普通 Linux 用户态进程并确定性重放，重放时可以反向执行；它的 chaos mode 会随机化调度来提高间歇性 bug 的复现率。— [USENIX ATC'17](https://www.usenix.org/conference/atc17/technical-sessions/presentation/ocallahan)；[rr chaos mode 博客](https://robert.ocallahan.org/2016/02/introducing-rr-chaos-mode.html)
  - **Dropwise 启发**：遇到“注入后失败、但同种子重跑不稳定”的报告（例如多线程运行时的调度差异），可以提示用户 `rr record -h cargo test ...` 并设置相同的 `DROPWISE_SEED`，用 rr 的录制拿到一次失败轨迹。这只需要写文档，零开发，属于 LHF；但 rr 依赖特定硬件计数器，环境不一定可用。
- 【新】Antithesis：在确定性 hypervisor 中运行用户提供的 Docker 容器（可以用 docker-compose 编排），宣称不要求修改应用代码，并提供故障注入和确定性重放；客户端负载以 “test template”（Test Composer）的形式提供；etcd 项目有一个 3 节点集群加客户端容器的示例。— [Antithesis docs: environment](https://antithesis.com/docs/using_antithesis/environment.html)；[etcd tests/antithesis](https://chromium.googlesource.com/external/github.com/coreos/etcd/+/HEAD/tests/antithesis)
  - **Dropwise 启发**：Antithesis 采用“现有集成负载 + 外部确定性环境”的路线，与 Dropwise 的“进程内 Tokio 补丁”互补，迁移弱。可以借鉴它的“sometimes assertions”概念（本次未取回原文），也就是声明“这个取消点在测试中至少被注入过一次”，作为覆盖报告的一部分，防止 Dropwise 静默地什么也没测。工作量 S，LHF（就是覆盖率报告）。
- 【新】Google 突变测试：结果在 code review 中展示，用 arid 节点过滤减少噪声，以“开发者是否觉得有用”为核心指标。— [Google Research](https://research.google/pubs/state-of-mutation-testing-at-google/)
  - **Dropwise 启发**：(a) 输出 SARIF 或 GitHub Actions annotation，把报告挂到 PR 中站点所在的行，而不是只出现在 CI 日志里。(b) 每条报告带“有用/无用”反馈入口（最简单的是 suppression 文件的 PR 记录），用来迭代 arid 规则。工作量 S–M，LHF。
- 【新】Filibuster：counterexample 文件支持重放之前失败的测试，以验证新加的条件断言。— [SoCC'21 PDF](https://rohan.padhye.org/files/filibuster-socc21.pdf)
- 【老→新】Sanitizer suppression 文件（按函数、源文件或模块屏蔽已知报告）。— [TSan suppressions wiki](https://github.com/google/sanitizers/wiki/ThreadSanitizerSuppressions)；[Clang UBSan docs](https://clang.llvm.org/docs/UndefinedBehaviorSanitizer.html)

### Inferences
- 降低误报最有效的三道闸，按成本从低到高：
  1. baseline-first 差分（NonDex），排除本身就 flaky 的测试。
  2. 静止后判定义务泄漏（goleak 与 Go 1.26 的思路），排除“观察过早”。
  3. 失败必须能用同一种子、单一注入点复现两次以上（NonDex 二分加 iDFlakies 式重跑），否则标为 inconclusive。
  
  这三道闸都不需要用户写代码，是 Dropwise A′ 路线最值得先做的部分。
- 维护成本控制：报告默认不阻断 CI（Uber 和 Go 的做法）；回归文件提交到仓库（proptest 的做法）；suppression 与条件断言二选一，并优先引导用户写条件断言（Filibuster 的做法），因为条件断言同时也是一份规格。

### Gaps
- 没有找到专门研究“非确定性注入工具报告对开发者信任影响”的定量研究，Uber 的 52%/8% 调查是唯一数据点。
- 本次没有取回 Antithesis “Sometimes Assertions” 文档原文。
- 没有找到任何 Rust async 取消测试工具已有的报告格式先例，似乎是空白。

---

### 汇总：按优先级排序的 LHF 清单（供报告作者直接使用；均为上文推断的汇总）
1. `cargo dropwise test`：读 lock 得到 tokio 版本，用本地源码打补丁，生成 config 中的 `[patch]` 和 `--cfg dropwise`，使用独立 target-dir，用户零改动（依据：madsim、Cargo config patch 文档）。工作量 M。
2. baseline-first 差分加稳定性重跑，只报告“baseline 过、注入挂”的测试（依据：NonDex、cargo-mutants、iDFlakies）。工作量 S。
3. 种子、复现命令和 `dropwise-regressions/` 持久化，并且总是先重放（依据：proptest、NonDex、go test -shuffle）。工作量 S。
4. 只对 baseline 中观察到 Pending 的非 arid 站点注入，并做站点到测试的索引和 diff 模式（依据：Google 突变测试、PIT、Ekstazi）。工作量 S–M。
5. 重 harness 采用每站点、每次运行一次注入，跨 CI 轮换种子，失败后二分确认（依据：Go race、Uber、NonDex debugger）。工作量 M。
6. 挂起 oracle：超时阈值由 baseline 自动推导（依据：cargo-mutants）。工作量 S。
7. 义务泄漏检查在运行时静止之后进行，并对基线快照做差分（依据：goleak 的 IgnoreCurrent、Go 1.26 goroutineleak）。工作量 S–M。
8. 条件断言 `dropwise::was_cancelled_at(...)` 加 suppression 文件（依据：Filibuster、sanitizers）。工作量 S。
9. 报告按栈哈希去重，输出 SARIF 或 PR annotation，默认不阻断（依据：Uber、Google）。工作量 S–M。
10. 探索性方向，非 LHF：在 `coop::poll_proceed` 这一 leaf 层注入 Pending，以覆盖 futures combinator 和自定义 wrapper（依据：Tokio coop 文档、futures-test 的 `interleave_pending`）。工作量 M，需要实验。
