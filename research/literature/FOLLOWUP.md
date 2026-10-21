# 调研之后：两份调研的交叉验证与落地情况

> 更新于 2026-10-21。开头是当前概览；旧讨论和标注日期的表保留当时语境，不再逐条回写。实验细节仍以各阶段 RESULTS 为准。[REPORT.md](REPORT.md) 是本仓库五份笔记的综合报告。

## 当前主线（2026-10-21）

取消测试不能停在“目标 future 被 drop”这一刻。要接着看四件事：

1. 被取消的操作实际留下了什么——它可能合法地部分完成，也可能已经在远端生效；
2. 留下的资源被谁、以什么方式继续使用——池的验证/回收，还是同一持有者的下一次调用；
3. 后继拿到的返回值是否属于这次请求，实际效果是否正确——`Ok` 可能带着旧值、空结果、中间态回复或假成功；
4. 恢复动作恢复了哪一层——业务操作可用，不等于 API 元数据、协议关联和共享状态都恢复；未恢复的部分有没有传给做复用决定的一方。

这条主线把两类结果放在一起：目标选择决定“在哪里停”，后继与观察设计决定“停之后看什么”。2026-10-18 之后的主要产出来自后者；2026-10-21 的 phase4 exploration 又补了前者：在我们测过的样本里，checkout/验证、会话切换、SMTP DATA 中间态这些边界比普通请求的取消点更容易暴露问题。

## 案例与当前状态

| 案例 | 已经看到什么 | 证据/状态 |
|---|---|---|
| sqlx Postgres `Pool::begin` | BEGIN 往返中取消后连接留在事务内，后继自动提交写入对独立连接不可见。T3 的回显查询漏掉，换观察才看到。 | 上游 #4393 已修复；[RESCAN](../../prospective/RESCAN.md) §3 |
| sqlx MySQL `begin` | 同一模式；后继写入落进一个未提交的事务，后继看不到。 | 已提交 #4467 / PR #4468；RESCAN §6.1 |
| bb8 `is_valid` | 验证中途被丢弃，连接以 `Present` 回池；残留 `PONG` 被下一次验证读成自己的回复。 | 已提交 #324 / PR #325；RESCAN §4 |
| bb8/BOLT pooled | checkout 的 `is_valid`→RESET 被取消，旧连接回 idle；下一次 RESET 把旧 `SUCCESS` 当验证通过，业务 RUN 报 Ready 状态错误。修复版 0/60；原版最终复核 22/60（第二轮 23/60）。 | 探索性；[phase4-exploration](../../prospective/phase4-exploration/RESULTS.md) §4、§9；bb8 #324 的适配器证据 |
| bb8-memcached / deadpool-memcached | 静默返回别的 key 的值；或持续报错但写入已生效。 | bb8-memcached #16 已提交；deadpool 未上报；RESCAN §4.2、§5.2 |
| lettre `send` | DATA 阶段取消后池位卡死；修复用 `sending` 标记连接不可复用。 | #1170 / PR #1171；RESCAN §6.2 |
| diesel-async | 池不读 mysql_async 的 `disconnected`；下一查询读到旧结果。 | #304 / PR #305；RESCAN §7 |
| mysql_async direct `Conn` | 直接句柄的查询取消后，下一条查询返回 12345（应为 67890）；池路径因 recycler 检查 `disconnected` 而干净。 | #412 / PR #413；[phase3/RESULTS-db](../../prospective/phase3/RESULTS-db.md) |
| MongoDB MG1 | 事务第一条命令取消后事务留在服务端；同参数重试 `WriteConflict`；不同对象 M1 没看到。 | 契约外；phase3 `AMENDMENTS.md` #16 |
| neo4rs 0.8.0 | recycle 的 RESET 接受残留 `SUCCESS`；后继同池读丢自己的行。 | 已知 neo4rs#295/#298，修复只在 main；[neo4rs/RESULTS](../../prospective/neo4rs/RESULTS.md) |
| async-smtp | 取消后复用返回 `Ok(354)` 但消息已投递；`#0@3` 的 6/24 执行无投递、2 s 超时；第三封不自愈，新连接恢复。 | 探索性；issue #73；phase4-exploration §2 |
| async-imap | 取消 SELECT 后 APPEND 读到陈旧 untagged 数据；retry SELECT 恢复操作，但 `flags`/`permanent_flags` 仍脏。 | 探索性；PR #150；phase4-exploration §3、§9 |
| BOLT direct | 切点后复用失败；显式 `reset()` 可恢复操作，但公开文档没有写 drop 后的义务。 | 未文档化契约缺口，不是确认新 bug；phase4-exploration §4.6 |
| io_uring / Notify | 立即观察在修复版也报 fd 增加；加 settle 后才区分暂态与泄漏。Notify 需要额外取消时机。 | 机制对照，不是新 bug；[mechanisms](../mechanisms/RESULTS.md) |

“已提交”只说明链接创建成功，不代表维护者确认或已发布。phase4-exploration 是不冻结的首次接触，不并入正式 Phase 4；表中的计划执行数不是独立 bug 数。这些是案例级证据，不是检出率或覆盖率估计：phase3 的 8 个目标只确认 1 个问题，phase4 exploration 只覆盖 3 个客户端家族。

## 方法落地状态

| 方法元素 | 当前状态 | 入口/证据 |
|---|---|---|
| 允许集合 + 二次校准 | **已进入公共 API**：`explore_observed`、`Expect`、`Verdict`（pre/post/mixed/neither/unknown）；每侧两次采样，不一致记 Unknown | `src/explore.rs`；`docs/using-dropwise.md`；RESCAN §3 |
| 幸存者 + 独立观察者 | **实验方法**：没有自动 oracle，观察值由场景写；已在 sqlx、bb8、mysql_async、mongodb、SMTP、IMAP 等使用 | `docs/choosing-targets.md`；`docs/writing-scenarios.md` §2.5 |
| 同对象 + 载荷标记 | **方法**：对象身份决定冲突落在哪，标记决定旧回复是否被认出来；归一化目前是场景侧代码 | `neo4rs` 的 `normalize()`；phase3 MG1；phase4-exploration §2 |
| 直接句柄 | **形状已支持**：`ctx.target` 只丢弃目标 future，句柄留在场景里；没有单独的“直接模式”API | `examples/direct_handle.rs`；`docs/writing-scenarios.md` §10 |
| 可调度 gap | **已进入公共 API**：`Ctx::gap` + `Config::gap_choices`（单 cut）；`ProxyOptions::reply_delay` 在 `net` feature 下 | `src/explore.rs`；`docs/using-dropwise.md` |
| 重复取消同一站点 | **已进入公共 API**：`Config::repeat_at_site` | `src/explore.rs`；README “Repeated cancellation” |
| 计划重放 | **已进入公共 API**：`Trial::replay_plan`、`replay_observed`；真实 I/O 下是重试，不是确定性重放 | `src/explore.rs`；README “Plan replay” |
| 清理时机 | **部分**：`Settle`、`after_settle`、watchdog；没有通用静止判定 | `src/explore.rs`；mechanisms RESULTS |
| 动态值归一化 | **场景方法**：没有自动规范化 API | `neo4rs` 的 `normalize()`；`support::unique` |
| 后继操作/参数组合 | **实验侧原型已实现**：注册操作后按同域 object/payload 展开，逐 case 校准、预算和 replay；mysql direct、BOLT pooled 用单一 `ParamSlots` 声明同时提取形状和 realize，不再平行手写 `target_shape`/`successor_shape`/`realize` | `prospective/support/combine.rs`；[使用说明](../../prospective/support/README.md) |
| 观察字段/关联提示、中间效果采样与模板 | **部分**：只读结构化字段级 diff 和可选的中立身份标注已落地，不改变判定；字段候选提示、中间效果采样与模板未实现 | [诊断说明](../../prospective/support/README.md#structured-observation-diagnostics)；本文“后续自动化方向” |

## 后续自动化方向

这些方向的实现程度不同，不能整体称为现有功能或未实现。下面按“已有实现 / 已有实验支持的方法 / 待验证的自动化设想”标注，不预先承诺零人工知识、固定标注行数、完备覆盖或检出率。

1. **后继操作与参数组合生成（实验侧原型已有）。** `prospective/support/combine.rs` 根据适配器注册的操作及同域槽位生成组合；mysql direct、BOLT pooled 已接入。它不会发现客户端 API、推断契约或搜索恢复序列。两种真实适配器已用单一强类型 `ParamSlots` 声明减少 shape/realize 重复，同时保持具体输入、枚举顺序和预算语义。
2. **观察字段与身份关系候选提示（待验证）。** 从返回结构、协议消息、日志字段中提示“这个值可能属于哪个请求/对象”“哪个字段能区分 pre/post”。当前靠人读代码和协议文档；`Trial` 保留计划、观察值和判定等信息，但不推断字段关系。
3. **动态值规范化（已有场景侧做法，未自动化）。** neo4rs 的 `normalize()` 是手写规则：剥掉本次运行的前缀，再比较。可以把它做成模板，但必须保留“这次/上次”的关联；不能因为动态值跨运行变化就直接删除。
4. **中间效果采样与允许结果模板（待验证）。** 在操作进行中采样前缀、子集或资源状态，并给出可比较的允许结果模板。当前的 `Proxy::residue`、独立观察者、`after_settle` 只是零件；没有“采样点 + 允许模板”的通用机制。

### 本轮 Gemini 建议的取舍（2026-10-21）

采纳“参数声明去重，再做结构化观察”的顺序，实施边界见
[生成器与诊断说明](../../prospective/support/README.md#structured-observation-diagnostics)。
已复用 Serde 的只读遍历能力落地字段级 diff，且不改变原有判定；暂不做自动稳定性掩码、
缩减和长序列。

不采纳几项过强结论：同域相等不是因果溯源，不能自动标成“取消目标泄漏”；合法 post 本来可能
包含目标写入。动态标量仍可能携带错误值，不能仅因校准中变化就屏蔽。pre/post/mixed 是不同的
允许端点，不能混在一起学习“噪声”，也不能用两次无取消预热替代。代表元不能保证上下文合法，
仍须保留适用性及关系后验检查。当前 mysql 观察直接返回实际值，没有建议中声称可删除的手写
12345/67890 对账逻辑。代码量、接入触碰点和耗时收益需要实测，不承诺“零误报”或固定削减比例。

## Codex 调研的要点

Codex 覆盖了 1980 年代到 2026 年的异常安全、任务取消、并发测试、崩溃恢复、RPC 重试和责任追踪。它的核心判断是：开发者往往分别考虑“如何开始操作”“如何取消等待”“如何释放资源”，却漏掉这些动作之间的交接，以及取消之后其他参与者继续运行时会发生什么。具体有六组线索：

1. **取消是局部故障，要检查活下来的人。** 来源：Mesa（1980）；Kill-Safe Synchronization Abstractions（Flatt & Findler，PLDI 2004）；FSCQ、ALICE 的崩溃恢复思路。
2. **最危险的是“已交出、尚未接住”。** 来源：CQS（PLDI 2023）中取消与唤醒的竞争；Kotlin `onUndeliveredElement`。
3. **“超时了”不等于“没执行”。** 来源：Birrell–Nelson RPC（1984）；RIFL（SOSP 2015）；Sagas（1987）。
4. **单个组件正确，组合起来未必正确。** timeout、retry、缓存、限流的包装顺序会改变语义。来源：Frank Tip 2026 年的预印本。
5. **安全性与活性要分开判，观察不到时保留“未知”。** 来源：Alpern–Schneider（1985）；Lilo（OOPSLA 2025）；Rust async 运行时响应性验证（2026 年预印本）。
6. **清理本身也会被打断。** 来源：Marlow 等，Asynchronous Exceptions in Haskell（PLDI 2001）。

方法上，它建议把关键交互压缩成小场景（一个资源、两个使用者、一次取消、一个晚到结果、一段后续操作），并整理少量“有前提的关系”作为测试模板。

## 交叉验证

| | 内容 |
|---|---|
| **两边独立都找到** | Abrahams 的逐点注入循环；Lincheck `promptCancellation`（≈ `AfterWake`）；Kotlin `onUndeliveredElement`（≈ `Obligation`）；Hyperactor `cancel_safe`；Sagas；Haskell 异步异常；蜕变测试；CHESS / CrashMonkey 的“小界限够用”；安全性与活性分开判 |
| **只有 Codex 有** | Kill-safe（已核实存在）、CQS、RIFL、Birrell–Nelson、FSCQ、Lilo，以及两篇 2026 年预印本（均已核实存在。Codex 引用的两个细节在摘要中查不到：“包装顺序改变语义”“模型未纳入取消导致的义务撤销”，未核实） |
| **只有本报告有** | 静止判定（Go synctest、Trio、Eio、Cats Effect、VOPR）、孤儿任务计数、NonDex 与零配置接入（CancelSan）、Tokio cancel-unsafe API 表、futurelock、PCT 概率界与 Fray 现实性标签、Witcher 的 {pre, post} 判定 |
| **分歧** | Codex 主张先做场景和检查、不扩框架；本报告把 CancelSan 放在第二梯队。实际结果支持 Codex 的排序：阳性结果都来自场景设计和观察方式，而不是注入面 |

Kill-safe 的“幸存者”视角是本报告五份笔记都没有覆盖的，事后看也是收益最大的一条。

## REPORT.md 清单：当前落地状态

编号对应 [REPORT.md](REPORT.md) 的排序表。“公共 API”指当前根 crate 导出的接口；“实验方法”指在场景/实验代码中验证过、但没有独立 API 的做法。本表 2026-10-21 更新。

| # | 建议 | 状态 | 证据与效果 |
|---|---|---|---|
| 1 | {pre, post} 允许集合 | **已进入公共 API** | `explore_observed` + `Expect` + `Verdict`；pre 用“第一次 poll 前丢弃”校准，多 cut 有 mixed，每侧两次采样。R3.5 在修复版 sqlx 上，有 6 个计划会被“等于基线”误报，用允许集合判为 0；R3.6 的 6 个真实半完成态仍然报出 |
| 2 | 结果与效果一致性 | **部分** | `Expect::Undone` 能声明“结果必须交到调用方”，但还没有自动比较“返回值 vs 独立效果”的 oracle。R1.2 的“报告已取消但对端收到”18/18、SMTP 的 `Ok(354)` 都说明需要这层检查 |
| 3 | 孤儿任务计数 | 做了对照，未纳入 | 能抓到不带 `Obligation` 的遗留任务，但延迟清理会误报，“一个退出、一个泄漏”会漏报；只适合作为提示信号 |
| 4 | 窗口敏感性自检（T 与 2T） | 做了对照，结论是否定 | 反例表明 T 与 2T 结果相同不代表稳定；应当用明确的完成信号 |
| 5 | 静止判定 `Settle::Quiescent` | 未做 | — |
| 6 | 基线确定性自检 | **已进入公共 API** | 每侧校准跑两次，不一致即记 Unknown；校准失败进 `calibration_errors`，不算违反 |
| 7 | Pending 制造器 | 临时实现 | 用小缓冲 duplex、Stutter 传输、慢服务端、回复闸门、`ProxyOptions::reply_delay` 等手段各自实现；没有做成通用 helper |
| 8 | 内置模型扩展 | 未做 | bb8 + oneshot 的交接实验 12 个计划全部干净，与“收益上限有限”的判断一致 |
| 9 | 新的 plan 维度 | **部分进入 API** | `Race::AfterWake`、`Config::repeat_at_site`、`Ctx::gap`/`Config::gap_choices`、`replay` 已实现；“库内部超时”仍是场景侧来源（bb8 `connection_timeout`、deadpool `recycle_timeout`），abort/runtime shutdown 等未做成维度 |
| 10 | 取消后重试差分 | **实验方法已用** | R1.1 抓到 tungstenite send 重复投递；commit_reply 抓到重复提交。没有独立 API |
| 11–14 | 报告、现实性标签、分层采样、等价归并 | 未做 | 报告已有 site、plan label 和 replay，但没有现实性标签、概率采样或效果归并 |
| 15 | `cargo dropwise test`（CancelSan） | 未做 | 在运行脚本中手动用过 `[patch.crates-io]` 替换 sqlx-postgres 和 bb8，机制可行。遇到过一次同名同版本 path 包冲突，靠改版本号绕开 |
| 16 | futurelock 检测 | 未做 | — |
| 17 | coop 叶子层注入 | 未做 | — |

## 实际发现（截至 2026-10-19）

> 下表保留 2026-10-19 的原记录；2026-10-21 的 phase4 exploration 结果见开头“案例与当前状态”，不并入本表。

| 发现 | 性质 | 状态 |
|---|---|---|
| sqlx 0.9.0 `Pool::begin()` 取消后池化连接停留在事务内 | 上游已知缺陷（#4393），**T3 当年漏掉**；事后复现，修复版对照为 0 | 上游已修复 |
| bb8 `get()` 在 `is_valid` 中途被丢弃时，连接被放回池中 | 新问题；字节流协议的 manager 会错位，bb8-memcached 会静默返回其他 key 的值 | 已提交 [djc/bb8#324](https://github.com/djc/bb8/issues/324)、修复 PR [#325](https://github.com/djc/bb8/pull/325)（CI 全部通过） |
| bb8-memcached 只在自身超时触发时标记 tainted | 新问题 | 已提交 [dqminh/bb8-memcached#16](https://github.com/dqminh/bb8-memcached/issues/16) |
| deadpool-memcached / async-memcached 的 `version()` 不检查前缀 | 新问题；用户中途取消后连接持续报错，且写入照样生效 | 未上报 |
| sqlx-mysql 0.9.0 `Pool::begin()` 取消后连接停留在事务内，后继者的写入丢失 | 新问题；与 #4393 同一模式，上游的修复只覆盖了 Postgres | 已提交 [transact-rs/sqlx#4467](https://github.com/transact-rs/sqlx/issues/4467)、修复 PR [#4468](https://github.com/transact-rs/sqlx/pull/4468) |
| lettre 0.11.23 池化 `send` 在 DATA 阶段被取消后，池位持续卡死 | 新问题；连接池是默认 feature | 已提交 [lettre/lettre#1170](https://github.com/lettre/lettre/issues/1170)、修复 PR [#1171](https://github.com/lettre/lettre/pull/1171) |
| tungstenite send 被取消后仍会送达 | 契约外行为（Sink 侧没有承诺） | 不需要上报 |

## 当前优先级（2026-10-21）

1. **继续用“目标清单 + 后继 + 允许集合”扫更多库。** 优先选 checkout/验证、会话建立、SMTP DATA/IMAP APPEND 这类多步交换；直接句柄单独记录责任边界，不混进池化统计。
2. **把已经稳定的方法做成模板或工具，而不是再改判定引擎。** 允许集合、二次校准、gap、重复取消和 replay 已进入公共 API；剩余主要是后继组合选择、三类观察字段、立即复用与完成信号后清理的时序、身份/关联、动态值规范化、中间效果模板。不要预先承诺零人工知识、固定标注行数、完备覆盖或检出率。
3. **静止判定、futurelock、CancelSan 继续推迟。** 它们仍缺实现和外部接入证据；先让场景生成和观察提示变得可复用，再评估这些 M 级投入。

---

## 历史记录（2026-10-18 至 10-19）

> 以下保留当时的讨论、判断和“未做/仍缺”的描述，用于理解当时的决策；当前状态以开头概览为准，旧文不回写。

## Gemini 的建议与评估（2026-10-18）

用户另外向 Gemini 询问了同一组问题（背景：被取消的交换污染了被复用的连接）。下面按“与我们的实测是否吻合”来评估，不是全文转录。凡是写“已核实”的，指本次检索或我们的实验确认过；其余均未核实。

**与我们的发现直接吻合、值得马上采用：**

- **“错误路径与 Drop 路径不一致”作为静态特征。** 如果某个 `.await` 之后的 `Err` 分支会把 `self` 标记为坏（例如 `is_broken`、`panic`、`tainted`、`closed`），而同一个挂起点的取消（drop）路径不做同样的标记，就说明作者已经承认“交换没完成时连接是脏的”，却漏掉了取消这条路径。我们已有的四个发现都完全符合这个特征：lettre 的 `try_smtp!` 只在出错时设置 `panic`；bb8-memcached 只在自身超时时标记 tainted；async-memcached 在 #91 中只为 meta 操作加了 guard；sqlx-mysql 的 `begin` 在出错和被取消时都没有清理。这个特征不需要 MIR，可以先用 syn 或文本启发式，在依赖 bb8、deadpool、mobc、r2d2 的 crate 中做普查。它类似 Rudra（SOSP'21）在 unwind 方向上的做法。
- **不依赖协议知识的“残留字节”判定。** 连接空闲、或刚被借出还没写任何字节时，在静止之后，接收缓冲区应当为空（可以用 `FIONREAD` 或 `MSG_PEEK` 检查）。bb8-memcached、async-memcached 和 lettre 的 `354` 这三类问题，不用写任何协议相关的判定就能抓到。它抓不到 sqlx-mysql 这种“字节对齐但会话状态脏了”的情况；对于服务端主动推送的协议（pub/sub、通知），也必须先排除才能用。Gemini 说它“100% 确定”，这一点说过头了：前提是先确认对端已经静止。
- **两个盲区：**
  1. **测试框架在复用前等了一会儿。** lettre 场景在复用前固定 `sleep(50ms)`，bb8 场景也是等回复到达后才让后继者取用。“回复晚于下一位借用者的 poll 才到达”，以及“回收任务还没跑完就被复用”这两种顺序都没有覆盖。我们的 reply gate 和 Proxy 已经能控制回复到达的时刻，只需要把它变成一个可调度的维度。
  2. **异步清理与下一位借用者之间的竞争。** sqlx 是把 ROLLBACK 排进队列，lettre 和 bb8 是 spawn 一个回收任务；如果下一位借用者抢在清理之前拿到连接，会怎样？#4445 也属于这一类。

**有价值、但成本较高：**

- **双生连接字节流差分：** 把后继者的请求字节在两条干净连接上重放，掩码掉动态字段之后比较回复。它能覆盖会话状态污染，但重放有副作用的请求需要隔离服务端状态（快照或独立容器），成本高。
- **“多米诺效应”基准：** 注入 1 秒的延迟尖峰，画出错 key 率随时间变化的曲线，对比修复前后。适合用来说明严重性（issue、文章），但对找新 bug 帮助不大。
- **扫描范围扩展到网络客户端之外：** 带状态的流包装器（`Framed`、压缩流、TLS 流上的 `write_all` 半途取消）、tower 的 `ready()` 之后取消导致的许可泄漏、对象存储的分块上传。可以作为目标清单的补充。

**修复范式：** Gemini 推荐的“悲观投毒守卫”（开始交换前标记为脏，完整闭环后解除；被 drop 时直接丢弃连接），正是我们这四个补丁的共同做法：bb8 在 `is_valid` 期间标为 `Invalid`；lettre 用 `sending`；sqlx-mysql 先增加深度再用 `Rollback` guard。它对 AsyncDrop 的批评也成立：在析构时去排空 socket，会把触发了超时的调用方再卡一次。

**跨语言先例：** 这些可以在 issue 里用来说明“连接一旦被打断就作废”是业界常规做法，均未逐条核对原文。
- Java NIO 的 `ClosedByInterruptException`：线程被中断时，channel 自动关闭；
- Go `database/sql` 的 `driver.ErrBadConn`、`SessionResetter`（Go 1.10）和 `Validator`（Go 1.15；Gemini 说的是 1.8/1.10，有误）；
- .NET TDS 协议的 `RESETCONNECTION` 状态位，以及 PostgreSQL `ReadyForQuery` 的事务状态字节（我们在 sqlx 源码中已核实，见 4.2 节）；
- MySQL 的 `COM_RESET_CONNECTION`；
- PortSwigger 的 response queue poisoning；
- CWE-226、CWE-672。

Gemini 关于 .NET SqlClient 的细节（“内核态重置”“取消时直接置为 Broken”）有夸大或无法核实的部分，不要引用。

## Gemini 第二轮：本质、根治与盲区（2026-10-18）

第二轮问的是“本质是什么、怎么从根上解决、我们可能想错在哪”。下面同样只按“能否核实、和我们已有结果是否吻合”评估，不转录原文。

### 本轮实验核实

- **客户端断开会立即丢弃 handler future。** 用 [disconnect-probe](disconnect-probe/)（axum 0.8.9、hyper 1.12.0、hyper-util 0.1.21、tokio 1.53.3）测试：handler 内 `sleep(2s)`，客户端发出请求后 200ms 断开。FIN、RST、只关写端（半关闭）三种方式下，handler 都在 200.0–200.3ms 被 drop，没有跑完。
  - **意义：** 外部客户端可以用自己的时机决定服务端在哪个 `.await` 被取消，服务端代码里不需要有 `select!` 或 `timeout`。我们已上报的 sqlx-mysql `begin`、lettre `send` 等问题，触发条件因此从“调用方设了超时”扩大到“任何能断开 HTTP 连接的客户端”。
  - **尚未演示：** 端到端的“一个请求断开、另一个请求受害”。断开需要落在一次后端往返的窗口内，被污染的连接还要被后续请求借到；窗口很短，但攻击者可以反复尝试。
- **tokio-postgres 的事务清理。** 0.7.18 源码中，`Transaction::drop` 往同一条有序通道塞一条 `ROLLBACK`，与 Gemini 的描述一致。但 `TransactionBuilder::start` 还专门写了一个 `RollbackIfNotDone` guard，处理“BEGIN 已经发出、`Transaction` 还没构造出来”这个窗口。可见后台任务架构只能免疫**字节流错位**，**会话状态**照样要显式 guard。Gemini 说这类设计“天然免疫”，只对了一半。

### 已经覆盖，不是新盲区

| Gemini 的建议 | 我们已有的 |
|---|---|
| 盲区 2：“已唤醒、未再 poll”窗口 | `Race::AfterWake` 专门覆盖这个窗口；RESCAN 的配置就是 `[Immediate, AfterWake]`；bb8 + oneshot 交接实验 12 个计划全部干净 |
| 盲区 1 / “第三件事”：去测 moka 的 leader 取消 | phase2 的 M2.1（取消 leader）、M2.2（取消 waiter）已测 moka 0.12.16，全部干净 |
| 类别 5：消息队列消费者的 `select!` 落败分支 | phase2 的 N2.1、N2.2 已测 async-nats JetStream，全部干净 |
| 取消原子性公理 τ_cancel ∈ {τ_abort, τ_commit} | 就是 REPORT #1 的 {pre, post} 允许集合，已经在用。资源守恒判据对应 C2(b) 的连接数比较；活性判据就是我们抓到 lettre 挂住的方式 |

Gemini 把轨迹定义成“返回值 + 字节流 + 耗时”，其中后两项不能直接用：修复后的正确行为是丢弃脏连接、重新建连，后继者的字节流里会多出一次握手，与两条基线都不相等；耗时也一样。应该比较语义层面的观察（返回值、独立观察者看到的状态），这正是我们现在的做法。

### 错误或夸大

- **tokio-rustls 的 nonce 重用：** 不成立。rustls 在把明文加密进自己的 TLS 发送缓冲区时就推进记录序号，缓冲区和序号都保存在连接对象里，future 被 drop 不会回滚它们。真实存在的只是应用层的帧被截断：`write_all` 本身就不是 cancel safe（Tokio 文档写明），这和普通 TCP 上一样。`FramedWrite` 在 `start_send` 之后，条目已经进了缓冲区，也不会被撕裂。
- **`tower::buffer` 的 leader 被取消：** 不成立。buffer 由独立的 worker 任务驱动，调用方只持有 oneshot（phase2 筛选目标时已经看过）。`load_shed` 里没有 leader。
- **“`write_all` 单独使用是安全的”：** 不对，理由同上。“取消安全在顺序组合下不封闭”这个论点本身成立，但 Oxide RFD 400 和 Rain 在 RustConf 2025 的演讲都讲过，适合当论文的 framing，算不上新发现。
- **在 `.await` 处 `fork()` 分叉：** 这个设想和它自己第 1 节的论点矛盾。fork 分叉不了对端状态，也分叉不了内核里的 socket，父子进程共享同一个连接。可行的替代就是我们现在的“重跑前缀 + 基线校准”。至于“寄生在已有测试套件上”这个目标，就是 REPORT #15 的 CancelSan。
- **后台清洗队列“排空后放回池里”：** 要排空，就得知道还欠几个回复，这需要协议知识；不懂协议时，只能把连接丢掉。丢弃的代价只是重连一次，实现简单得多。
- **能区分“正常析构 / 取消析构”的 Drop 钩子“零成本”：** 用一个 `done` 标志现在就能做到。我们的补丁和 `RollbackIfNotDone` 都是这么写的；语言层面的支持只是省掉样板代码。

### 值得采纳

- **把连接的所有权移进 future（take-and-restore）。** 交换开始时把连接从槽位里取出来，移进 future；正常完成才放回去。中途被 drop 时，连接随 future 一起销毁，不需要手写 guard。这是“默认作废”在类型层面的写法，可以写进 issue 和 PR 的修复建议，也可以作为静态扫描里“已防护”的判据。代价有两点：闭包式 API 用起来不顺手；任何 `Err` 都会丢掉连接，包括业务层面的错误。
- **在连接池层反转默认值。** 只要写过字节，连接就默认是脏的；协议层显式标记“干净”之后才能放回池里，归还时再做一次残留字节检查。可以作为 bb8/deadpool 的可选功能提出。代价：如果协议库不配合标记，每次归还都要重连。
- **新目标类别：**
  - **完成型 I/O（tokio-uring、monoio）：** 被 drop 的 recv 仍可能在内核里完成，把字节读走，后继读者就会丢数据。Buffer 所有权模型本来就是为这类问题设计的，所以问题本身是已知的；但读走的字节最后怎么处理，值得用幸存者模板测一测。这两个库的使用面都小。
  - **对象存储分块上传（object_store、opendal）：** 取消之后写入器处于什么状态。
- **Saga / 远端资源（kube-rs、云 SDK）：** 需要真实的远端环境，成本高，暂不做。

### 下一步

1. **端到端演示：** axum + sqlx-mysql 0.9.0。客户端在 `begin` 往返期间断开连接，之后另一个请求的写入丢失。这能加强 #4467 的严重性说明，而且只涉及已经公开的问题。
2. **继续上一轮定下的两件事：** 写“出错路径 / 取消路径不一致”的启发式扫描器；把复用前的固定等待改成可调度的维度。
3. **可选：** 补测完成型 I/O 目标。

## Gemini 第三轮：先例、修复与盲区（2026-10-18）

第三轮按上一节末尾的问法提问：先列出已覆盖的范围，再追问“客户端断开”这条线。这一轮基本没有重复我们做过的事，质量比前两轮好。下面只记录观点和初步判断，**引用均未上网核对**，判断凭记忆，具体工作以后再做。Gemini 关于可利用性的一节不转录；论证严重性时，用下面的先例和我们自己的受控演示，不依赖攻击模型推演。

### 先例：最有价值，引用前逐条核对

| 先例 | Gemini 的说法 | 初步判断 |
|---|---|---|
| redis-py CVE-2023-28858 / 28859，2023 年 3 月 ChatGPT 跨用户数据泄露 | asyncio 命令写出后、读响应前被取消，连接带着未读响应回到池中，后继请求读到别人的数据 | 机制和我们的发现几乎一样，是 issue 和论文里最贴切的先例。待核对：两个 CVE 的先后关系（印象中 28859 是对 pipeline 的补漏，与 Gemini 说的相反）；OpenAI 事后报告只说“请求被取消”，不一定是客户端断开 |
| GitHub 博客 “Three bugs in the Go MySQL Driver”（2020） | context 在 `Rows.Scan` 期间取消，驱动覆写连接缓冲区，读到错乱数据 | 文章存在，其中确有 context 取消与 `Scan` 竞争的 bug。“GitHub 定性为严重安全漏洞”需查原文 |
| Go `database/sql` 与 `driver.ErrBadConn` | 驱动返回 `ctx.Err()` 而非 `ErrBadConn` 时，半读的连接被放回空闲池 | 机制合理；Gemini 给的 go-sql-driver #1385 编号存疑 |

**各语言怎么处理“请求中止”与连接复用：**

- **Node.js：** Promise 不会被隐式取消，后端调用会跑完并干净归还；
- **ASP.NET Core：** 取消要显式传 `RequestAborted`，驱动在读取途中被取消时把连接标为 broken；
- **Go：** `r.Context()` 断开即取消，Go 1.21 新增 `context.WithoutCancel`；
- **Rust（axum/hyper）：** 隐式、无条件取消，见上一节的 disconnect-probe。

大方向可用，但有两处要修正：Rust 不是“唯一”隐式取消的，Python asyncio 也是，redis-py 事故本身就是例子；`WithoutCancel` 的动机比“专门解决断连污染”宽。

### 修复：可以写进 issue 和 PR

- **连接内部 take-and-restore。** 客户端内部存 `Option<Transport>`，操作开始时 `take()`，跨 `.await` 的读写都在局部变量上做，完整读完响应帧才放回。中途被 drop 时，局部变量随 future 析构，槽位留空，池的 `has_broken` / `recycle` 看到 `None` 就会丢弃连接。`&mut self` API 不变。注意：读写缓冲区等协议状态也要一起移出。
- **池包装层看不出问题。** bb8 `PooledConnection`、deadpool `Object`、sqlx `PoolConnection` 内部都存 `Option<Conn>`，但通过 `DerefMut` 交出 `&mut Conn` 之后，归还时分不清借出期间操作是走完了还是被打断了。所以修复只能做在协议库这一层（待对照源码确认）。
- **两层脏位（借鉴 HikariCP `dirtyBits`）：**
  - **wire dirty：** 写第一个字节前置位，读到完整响应帧才清零；归还时仍为 1，只能关闭连接。
  - **session dirty：** 发送 `BEGIN`、`SET` 等改变会话状态的命令**之前**置位，收到 `COMMIT`/`ROLLBACK` 等确认后清零；归还时 wire 干净但 session 脏，发 `ROLLBACK`、`COM_RESET_CONNECTION`、`DISCARD ALL` 这类轻量 reset 即可，不必重连。

  sqlx-mysql 的问题正好是置位太晚：BEGIN 往返完成后才记入事务状态。
- **要纠正的一点：** Gemini 说 HikariCP 不用操心半截帧，是因为线程中断时 NIO 会自动关闭 socket。但 JDBC 驱动大多用阻塞的 `java.net.Socket`，`Thread.interrupt()` 不会关掉它。真正的原因是 Java 线程不会在任意挂起点被隐式丢弃。

### 结构性盲区：两条值得查，两条不是取消特有的

1. **消费型操作与“已完成”基线（值得查）。** 被取消的操作已经从共享对象中取走了消息，调用方却没拿到返回值；这时后继者看到的状态和“已完成”基线一致，判定通过，但消息丢了。对有守恒性质的操作，“已完成”基线还应要求调用方确实收到了结果，否则只能接受“没发生”基线，或者显式检查总量守恒。**待办：** 查 Dropwise 和 phase2 NATS 场景的判定是否已经处理这种情况。Gemini 举的 Redis `BLPOP` 例子不算库的 bug：网络断开时同样会丢，这是协议本身不原子，Redis 因此才有 `LMOVE` 这类可靠队列用法。
2. **一次取消只泄漏一点，累积后才出事（值得查）。** 我们的流程是取消一次、观察一次。如果每次取消只泄漏一个有限资源（HTTP/2 连接级流控窗口、预处理语句句柄、许可）中的一小部分，单次观察看不出来。**待办：** 加一个“连续取消 N 次再观察”的维度，成本很低。候选目标是 h2/tonic 的流控窗口、sqlx 的语句缓存。Gemini 引用的 CVE-2023-26964 不对：那是 h2 的 RST 导致内存耗尽，不是窗口泄漏。
3. **客户端自愈、服务端留下残留（不是取消特有的）。** S3/MinIO 分块上传被取消后留下未中止的分片，这是文档写明的行为，靠生命周期规则清理；Postgres 后端在客户端断开后可能继续持锁一段时间，这是通用问题。保留为目标清单的补充。
4. **多路复用连接共享会话状态（不是取消特有的）。**
   - **redis-rs：** 在 `MultiplexedConnection` 上用 `WATCH`，不取消也一样有问题。
   - **deadpool-postgres：** 在 `RecyclingMethod::Fast` 下，残留的 advisory lock、`SET`、游标等，用户代码走 `?` 提前返回时同样会泄漏。

   按第一轮定下的“出错路径 / 取消路径不一致”标准，这两条都不符合。

### 以后可能做的事

1. 核对上面先例表里的引用，用于 #4467、#1170 等 issue 的补充说明和论文的 related work。
2. 检查判定逻辑对消费型操作的处理（盲区 1）。
3. 增加“连续取消 N 次再观察”的维度（盲区 2）。
4. 在修复建议里加入 take-and-restore 和两层脏位的写法。

## Gemini 第四轮：同一套模板能否直接用在未知目标上（2026-10-18）

问题：面对事先不知道有没有问题的新目标，“取消者 + 复用者 + 独立观察者 + 双基线”这套模板，是否比简单的取消测试更有效，而且不需要为每个目标重写专家级判据？Gemini 的回答是肯定的。下面按我们自己的数据评估。

### 我们的数据支持的部分

Gemini 说简单取消测试有三种常见写法，各有盲点。RESCAN 的回扫里，三种盲点都有对应的实例：

- **只看后继者的返回值：** R3.1e 用回显查询观察，后继者的结果全部正确，漏掉了 #4393；改用“有副作用的写入 + 独立连接确认”才抓到。
- **残留回复和期望回复长得一样：** bb8 验证留下的残留回复是 `"PONG"`，下一次验证读到的恰好也是 `"PONG"`，验证通过，错位被掩盖；改用 `ECHO` 作为后继操作才暴露出来。
- **只和“正常完成”比较：** R3.5 在修复版上有 6 个误报，用双基线判为 0。

### 说过头的部分

- **“不需要专家级判据”不等于“不需要专家判断”。** 判定这一步确实是通用的，但专业知识转移到了三个地方：
  - **选哪个调用：** T3 只换观察、不换目标，仍然干净（R3.4）；
  - **让操作停在关键挂起点：** reply gate、慢服务端、8 MiB 正文、库内部超时，都是逐个目标实现的；
  - **选择 Op2 和观察者：** lettre 用 Mailpit API，sqlx 用独立连接。

  诚实的说法是：所需知识从“协议层”降到了“API 层”（知道哪个调用会建立会话状态），但没有降到零。
- **“100% 漏报 / 100% 捕获”是修辞。** RESCAN 已写明，回扫时已知 #4393，不能据此估计检出率。sqlx-mysql 和 lettre 的问题事先未知，更接近前瞻性证据，但目标是按清单挑的，带着对这类模式的先验。

### 三项“非退化自检”

| 自检 | 判断 |
|---|---|
| 1. 校准时要求两条基线不相等 | 采纳，但只发警告、不判失败。Op1 是纯读操作时，两条基线本来就相等，这样的场景只能抓错位 |
| 2. 要求 Op1 与 Op2 的返回值不同 | 方向对，正是 bb8 `PONG`/`PONG` 的教训。但只比 API 层返回值不够：bb8 的残留来自库内部的 `is_valid`，Op1 在 API 层返回的是一个连接。真正的要求是“Op2 的回复与线上任何可能的残留回复都可区分”，这需要协议知识，或者靠残留字节检查兜底 |
| 3. 连续完整执行两次 Op1，结果不同就判为消费型操作，只允许“没发生”基线 | **不采纳。** 结果不同只说明不幂等：INCR、INSERT、发邮件都不幂等，但被取消后停在“已完成”是合法的（“超时不等于没执行”，R1.2 即是）。按这条规则，所有发出后被取消的写操作都会被误报。正确的做法是让模板作者声明一个比特：结果必须交到调用方手里（相当于 Kotlin `onUndeliveredElement` / 我们的 `Obligation`），不能靠推断 |

**三种共享拓扑骨架：** 串行复用、并发 leader/waiter 我们已经有了；“同一挂起点连续取消 M 次再观察”就是第三轮的盲区 2。目前配置是 `max_cancellations = 1`，值得补上。

### 结论：把它变成实验

“模板对未知目标是否更有效、适配成本多高”本身就是论文的核心论点，应当用前瞻性实验回答，而不是引用 Gemini 的论证。计划：

1. 用“出错路径 / 取消路径不一致”扫描器产出一批事先没看过的目标，冻结候选清单与流程（仿照 `PREREGISTRATION.md`）；
2. 每个目标同时跑两种方法：
   - 简单取消测试：只看 panic、挂起和 `Op2.is_ok()`；
   - 模板：幸存者 + 双基线。
3. 记录两种方法各自的发现数，以及每个目标的适配代码行数、工时和花在哪里，重点看 Pending 制造器和观察者。

第一步，即扫描器和候选清单，交给另一个会话做，产出放在 `prospective/candidate-scan/`。

## Gemini 第五至七轮：论文定位、元概念与防御模式（2026-10-19）

### 第五轮：适配成本与论文定位（多处有误，慎用）

这一轮把 diesel-async 归成"静默的事务问题"（实际是回复错位，B3 全部能抓到）；编造了"每个目标接入只需 30~60 行"（实际实验文件 127~243 行）；说残留字节检查能"100%"抓到错位（忽略了池的 ping/NOOP 会先把残留读掉）；还把"跨租户串扰"说成已证实。可用的只有三点：把静态扫描改写成连接池回收卫生的生态普查；把结果干净的目标放进主表，检验"后台 I/O 任务 / 多路复用的架构天然安全"这一假设；两层工作流（先用报错、卡死、残留字节做便宜的筛查，再用完整模板查静默问题）作为方法章节的组织方式。

### 第六轮：元概念、冷门同构工作与 Linux（质量高）

**已核实的出处：**

| 说法 | 核实结果 |
|---|---|
| 可线性化对未决调用的处理（Herlihy & Wing, TOPLAS 1990） | **属实，而且比转述更贴切。** 原文：“extending H to H’ captures the notion that some pending invocations may have taken effect even though their responses have not yet been returned to the caller … Restricting attention to complete(H’) captures the notion that the remaining pending invocations have not yet had an effect.” 每个未决调用各自要么算作已生效、要么算作没发生，正是我们的两条基线；多个 cut 时各自独立选择，正是 `Verdict::Mixed`。 |
| Java 废弃 `Thread.stop()` 的理由 | **属实。** 官方文档：停止线程会释放它持有的所有 monitor，被保护的对象若正处于修改中途，其他线程会看到不一致的“damaged”对象；损坏可能在事后很久才显现。 |
| I2C 总线卡死后发 9 个时钟脉冲恢复 | **属实（二手核实）。** 多个 Linux 内核 I2C 驱动补丁引用 UM10204 第 3.1.16 节“Bus clear”：SDA 被拉低时，主控发 9 个时钟脉冲，占住总线的设备应在其间释放。NXP 官网的 PDF 未能直接下载，原文措辞未逐字核对。 |
| 内核 fail-nth 接口 | **属实，路径要更正。** 当前文档写的是 `/proc/<pid>/fail-nth` 和 `/proc/self/task/<tid>/fail-nth`（没有 `thread-self` 形式）；写入 N 让该任务第 N 次故障点失败，读回 0 表示已注入；按任务生效，覆盖所有故障类型（slab、futex 等）。 |
| FIFUZZ（USENIX Security 2020） | **属实，但它测的是 9 个用户态 C 程序，不是内核；判定只靠崩溃和 ASan**（论文局限一节：“we only detect the bugs causing crashes and those reported by ASan”）。它是“错误路径系统注入”的代表工作，正好说明“幸存者 + 两条基线”这种判定在这条线上仍是空白（未做全面检索）。 |

未逐一重新核对、但属于常识的：Honda 1993 会话类型、T.Y. Chen 1998 蜕变测试、Abrahams 的异常安全保证、Kettle 2019 的 HTTP Desync、Bronson 等 HotOS'21 的 metastable failures、Haskell 异步异常（Marlow 等 PLDI 2001）。

**值得采纳的观点：**

- **理论锚点：** 判定方式 = 在“资源被轮流借用”的场景下，检查被放弃的操作是否满足可线性化对未决调用的规则。
- **根因：** Rust 的仿射所有权（值可随时丢弃）承载了线性的协议会话（必须走完），而 `Drop` 是同步的，无法异步补偿。
- **与崩溃一致性的区别：** 崩溃时整个进程一起死、重启后有统一的恢复流程；取消时只有协程死了，内核缓冲区和远端会话都活着，没有任何恢复流程被触发，脏状态被“热交接”给下一个人。
- **三元模型：** 短命执行体 / 长命共享基座 / 租约边界；残留字节检查推广为“交接时管道必须静止、排空”。
- **Linux 原型路线：** fail-nth（或在可中断等待处用 eBPF 发信号）+ 通过 fork / 传 fd 共享对象的幸存者进程 + 两条基线。不用改内核。

**夸大或有误：** 把安全影响说成已证实（“攻击者精确控制断开时机投毒”没有端到端演示）；把 diesel-async 算进“跨 key 读到脏数据”；现编的名词（“前缀双值性”“零在途熵”）和与 FLP 的类比只是修辞；“先测 kTLS/sockmap”是猜测；内核里的“引用计数增量”在用户态不能直接观察，需要调试接口或 eBPF。

### 第七轮：防御模式（总体可靠，作讨论章节的骨架）

- **“不可能三角”**（外部强制丢弃 + 同步 `Drop` + 跨 `.await` 的 `&mut` 借用）：准确，是对根因的好概括。
- **AsyncDrop 不是银弹：** 清理本身可能在坏网络上挂起、隐式的中间状态没有守卫对象、`mem::forget` 和运行时关闭仍是逃生口。与我们之前对 AsyncDrop 的评估一致。
- **四种防御模式：**
  - 悲观标记（先置脏、走完才清）：就是我们 bb8、lettre、sqlx-mysql 补丁的做法；mysql_async 自己的 `disconnected` 也是这个模式，diesel-async#305 只是把它接到了池上。
  - 按值传递所有权：即之前记录的 take-and-restore。
  - 后台清洗队列：带硬超时、用协议的 reset 命令（`COM_RESET_CONNECTION`、`DISCARD ALL`）时可行，tokio-postgres 把 ROLLBACK 排进有序通道是先例；不带协议知识的“排空”仍不可行。
  - 传输层静默检查：方向对，但有两点要纠正。
- **需要纠正的：**
  - 静默检查**抓不到服务端还在计算的回复**：被取消的是一个慢查询时（diesel-async 回归测试里的 `SLEEP(0.5)`），借出时缓冲区仍是空的，下一个查询照样读到旧结果。所以它只能作为“进行中”标志之外的补充，不能替代。
  - 示例代码不能直接用（tokio 的 `try_io` 里调用 `peek` 的写法不对）；通用池也拿不到底层 socket 和编解码缓冲区，这个检查只能放在 manager 或客户端里。
  - “Kotlin、Trio、Go 很少出现这类问题”说过头了：redis-py 的 CVE 正是在 Python asyncio 的异常式取消下发生的，Go 的 `database/sql` 也有 `ErrBadConn` 的历史。异常式取消只是让清理代码有机会运行，库仍然必须记得作废连接。

## Gemini 第八至十轮：可检测性、跨语言案例与内核线索（2026-10-19）

第八、九轮接在同一个长对话后面，第十轮新开对话、重新写了完整背景。**第九轮的具体线索几乎全是编的，第十轮基本可信**，差别主要来自对话长度，以及第十轮明确要求"不确定编号就给关键词，不要猜"。

### 第八轮：缺陷形状的理论名字、新颖性、Linux 的坑、AI 写场景

- **可以引用的相关工作**（凭记忆核对，未逐篇读原文）：EIO（Gunawi 等，FAST 2008）和 Rubio-González 等（PLDI 2009）讲的是文件系统里错误码在跨层接口处丢失，和我们的"信号—决策缺口"同构；Garlan、Allen 和 Ockerbloom 的"架构失配"（1995）对应"独立驱动配通用池出问题、整合在一起的库干净"；Clark 的 fate-sharing（SIGCOMM 1988）、Candea 和 Fox 的 crash-only software（HotOS 2003）与 microreboot（OSDI 2004）对应"销毁重建"这条路。
- **新颖性的比较对象：** strict linearizability（Aguilera 和 Frølund，2003）和 durable linearizability（Izraelevitz 等，DISC 2016）是理论上最近的邻居，论文必须正面区分；Flatt 和 Findler 的 kill-safe 抽象（PLDI 2004）给的是构造原语，不是测试判定。
- **Linux 判定要补的一条：** 返回错误码后，如果后继者立刻收到明确的致命错误（内核主动把对象标坏），也算合法，相当于池丢弃坏连接（第二轮原型记为 `OK_POISONED`）。它说的其余几个坑（短写、预先触碰用户缓冲区、只比较功能语义状态）第一轮原型大多已经处理了。
- **"两条基线必须可区分"：** Dropwise 已经有了（`src/explore.rs` 的 `degeneracy_note`，pre 与 post 相等时给出提示）。它只提示、不拒绝是有意的：幂等操作两条基线本来就相等，这时测的是有没有污染共享状态。用于 AI 写场景时，可以在工作流层面升级成准入检查。
- **会议名有误：** LFI 是 DSN 2009（不是 ICSE），APEx 是 USENIX Security 2016（不是 OOPSLA），CRIX 是 USENIX Security 2019（不是 S&P）；"Java Interrupt Testing（Gligoric）"未能确认存在。

### 第九轮：大面积编造（不要使用）

给出的 22 个 GitHub issue / PR 编号里，只有 redis/redis-py#2624 和 #2641 是真的（`gh api` 逐个查过）；两个 kTLS 提交号在本地查不到（本地仓库是浅克隆，只能算未核实）。它对 epoll 的描述是错的：`ep_modify` 不会"出错后留下半截状态"，而是忽略了一个错误、返回成功，见下面的内核线索。理论部分和第十轮一致，见下文。

### 第十轮：跨语言案例（已核实存在）

16 个 issue 和 4 个 commit 全部存在，标题和年份都对得上（`gh api` 逐个查过）。下表“内容”一栏是 Gemini 的描述，我们只核对了它与标题一致，引用前要读原 issue：

| 生态 | 编号 | 内容 |
|---|---|---|
| Python | aio-libs/aiohttp#12795（2026-06），修复 commit `4b2e470` | Gemini 的描述：`CancelledError` 继承 `BaseException`，`data_received` 只捕获 `Exception`，取消时没有关闭连接。修复（3.14.1）确实是改成捕获 `BaseException`，但 Python 回溯实验（research/python-retro）发现：任务取消不会走进这个同步回调，两个版本在取消场景下都干净；issue 报的约 2 ms 失败其实是连接回池后读超时定时器没有撤销，由 #12954（3.14.2）修复，与取消无关 |
| Python | sqlalchemy/sqlalchemy#6592（2021-06），修复 commit `2fa3931` | asyncpg 的连接遇到取消或超时后没有被作废，带着未结束的事务回到池里。与 sqlx#4467 同一形状 |
| Python | MagicStack/asyncpg#537、#1310（2026-03） | 取消过程中再出错时状态没有复位，下一个借用者报 "another operation is in progress"；`_cancel()` 抛出的异常类型绕过了上层的断连识别 |
| Python | aio-libs/aiohttp#3281（2018-09） | 后台写 chunk 的任务还没停，连接已被下一个请求复用 |
| Python | redis/redis-py#2624、#2641（2023） | CVE-2023-28858/28859，ChatGPT 事故 |
| Go | redis/go-redis#267、#268（2016-03），#2556（2023-04） | 超时后连接带着未读数据回池；维护者解释了为什么阻塞命令被取消时连接必须销毁 |
| Go | golang/go#22976（2017-12）、#31480（2019-04）、#43507（2021-01），回移植 commit `42e7927` / `399ce80` | `database/sql` 的 BeginTx 与上下文取消的竞态、事务泄漏 |
| Go | jackc/pgx#494（2018-12） | 把真正的网络超时误当成上下文取消，坏连接没有被丢弃 |
| Java | brettwooldridge/HikariCP#1388（2019-06）、#2238（2024-09） | 语句超时后连接被直接关闭而不先回滚；据称 Oracle 驱动关闭物理连接时会提交残留事务（未核实） |
| Java | reactor/reactor-netty#1165（2020-06） | 下游取消订阅后出现 SSLException |
| Node.js | brianc/node-postgres#2774（2022-07） | 讨论为什么中止查询后不能直接把连接还回池 |

**意义：** 这类问题跨语言存在，不是 Rust 特有的。Python asyncio 的取消发生在 `await` 处，与 Rust 最像，适合做第二领域的回溯检测（修复前报违反、修复后干净）。

### 第十轮：内核线索（一半可信）

| 说法 | 核实结果 |
|---|---|
| io_uring/futex 错误路径释放了数据却没有清 `async_data` 和标志位（CVE-2025-39698） | **属实**（NVD）。但半截状态在单个请求对象上，后果是 use-after-free，KASAN 能抓到，不是我们要找的静默问题 |
| eventpoll 把 `ep_insert()` 拆成分配和注册两个阶段 | **属实，但合入的是 7.2-rc1**（vfs-7.2-rc1.eventpoll），不是它说的 7.1.5 稳定版；"为了避免改了一半再回滚"是它的解读，未核实 |
| overlayfs 的 double `end_creating()`（CVE-2026-89767） | **搜不到**，按编造处理 |
| `connect` 被信号打断后返回 EINTR，socket 已处于连接中 | 是 man page 写明的行为，属于**契约外**，可作论文里"契约外"的例子 |

**我们自己发现的线索（读 v6.19 源码）：** `fs/eventpoll.c` 里，`ep_insert`（约 1629–1634 行）处理了 `ep_create_wakeup_source()` 的错误并回滚，`ep_modify`（约 1708–1710 行）却忽略了同一个函数的返回值。所以分配失败时，`epoll_ctl(MOD)` 加 `EPOLLWAKEUP` 仍返回 0，掩码已经改了，唤醒源却没有建出来。这正是"信号存在、一条路径读了、另一条没读"。影响很小（需要 CAP_BLOCK_SUSPEND，后果是系统可能在该保持唤醒时休眠），不涉及安全。**Linux 第二轮已确认：** fail-nth 下 MOD 有 234 次违反，ADD 对照为 0；v6.19 和 v7.3-rc2 都能复现，v7.3-rc6 的源码仍是这样；非穷尽的检索没有找到已有报告（research/linux/RESULTS-round2.md 第 3 节）。

### 第十轮：理论（采纳）

- **两层表述：** 对外的正确性条件是"跨交接边界的取消原子性"（第 2 节的条件不变）；对内的实现契约是"交接边界上的静止态可检测性"：资源是否静止必须能被做复用决定的一方读到，并且以此作为放行前提，否则重置或销毁。
- **和可检测性（detectable recoverability，Friedman 等，PPoPP 2018；recoverable linearizability，Berryhill 等，2015/2016，会议待核实）的两点区别：** 提问的不是原来的调用者，而是交接边界上的守门人；要区分的不是"生效了还是没生效"，而是"静止（两者皆可）还是半截"。守门人检测到半截状态时可以直接销毁资源，不必像持久内存那样补完或回滚。
- **为什么判定点在交接边界，而不是丢弃的那一刻：** 网络请求可能已经在服务端执行，客户端在 drop 那一刻无法阻止它生效，所以要求被取消的操作"在 drop 之前生效或永不生效"（strict linearizability 的形式）是做不到的。
- **协议本身常常提供了静止信号：** PostgreSQL 的 `ReadyForQuery` 消息带一个事务状态字节（`I` 空闲、`T` 事务中、`E` 失败的事务中）。问题出在通用池没有去读这类信号。
- **其他同构概念：** Rust `std::sync::Mutex` 的中毒机制（持锁时 panic 会让锁中毒，后来者必须显式处理），与"取消时给连接打标记、池回收时检查"完全同构；TxOS（Porter 等，SOSP 2009）讲的是系统调用改了多个内核对象后、出错时回滚不全的问题；分布式 lease 用 epoch / fencing token 隔离迟到的消息，而连接池为了省建连开销，恰恰放弃了这层隔离。

### 不采纳

- "通道状态的判定零误报"：说过头了，phase3 里 mongodb MG1 那个契约外的结果就是反例。
- 投稿建议和"杰出论文"之类的评价：不作为依据。
