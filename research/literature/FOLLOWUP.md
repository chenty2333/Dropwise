# 调研之后：两份调研的交叉验证与落地情况

> 更新于 2026-10-18。[REPORT.md](REPORT.md) 是本仓库五份笔记的综合报告。同一天，Codex 会话独立做了一轮文献调研，本文补记它的结论，并逐条说明两份调研的建议后来做了哪些、效果如何。实验细节见 [prospective/RESCAN.md](../../prospective/RESCAN.md) 和 README 中的 interruption 实验条目。

## 一句话结论

两份调研都认为，杠杆在“取消之后怎么判、观察谁”，而不在“在哪里取消”。实际产出也确实来自这里：

- **Codex 的“幸存者”视角**：检查被打断的工作之外、继续使用同一共享对象的参与者。
- **本报告的“允许集合”判定**：用基线校准出“操作没发生”和“操作完成”两种可接受状态。
- **一份目标清单**：优先取消建立会话状态的往返、库内部的超时等。

三者组合起来，在之前判为“零发现”的同一批库上，找到了一个已知上游缺陷（sqlx#4393，事后复现），以及 bb8 和两个 memcached 连接池中此前没有报告过的问题。报告中排名靠后的工程项（CancelSan、静止判定、futurelock 等）还没有做，效果未知。

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

## REPORT.md 低垂果实清单：落地情况

编号对应 REPORT.md 的排序表。“实验代码”指 `prospective/support/allowed.rs` 等实验内实现，没有进入 Dropwise 公共 API。

| # | 建议 | 状态 | 证据与效果 |
|---|---|---|---|
| 1 | {pre, post} 允许集合 | **已做（实验代码）** | 用“第一次 poll 前丢弃”作 pre 校准。R3.5 在修复版 sqlx 上，有 6 个计划会被“等于基线”误报，用允许集合判为 0；R3.6 的 6 个真实半完成态仍然报出 |
| 2 | 结果与效果一致性 | 部分 | R1.2 统计到“报告已取消但对端收到”18/18；没有做成通用检查 |
| 3 | 孤儿任务计数 | 做了对照，未纳入 | 能抓到不带 `Obligation` 的遗留任务，但延迟清理会误报，“一个退出、一个泄漏”会漏报；只适合作为提示信号 |
| 4 | 窗口敏感性自检（T 与 2T） | 做了对照 | 反例表明 T 与 2T 结果相同不代表稳定；应当用明确的完成信号 |
| 5 | 静止判定 `Settle::Quiescent` | 未做 | — |
| 6 | 基线确定性自检 | **已做** | 每侧校准跑两次，不一致即记 Unknown |
| 7 | Pending 制造器 | 临时实现 | 用小缓冲 duplex、Stutter 传输、慢服务端、回复闸门等手段各自实现；没有做成通用 helper |
| 8 | 内置模型扩展 | 未做 | bb8 + oneshot 的交接实验 12 个计划全部干净，与“收益上限有限”的判断一致 |
| 9 | 新的 plan 维度 | 部分 | 新增并验证了“库内部超时”这一取消来源（bb8 `connection_timeout`、deadpool `recycle_timeout`），bb8 的问题正是由它触发 |
| 10 | 取消后重试差分 | **已做** | R1.1 抓到 tungstenite send 重复投递；commit_reply 抓到重复提交 |
| 11–14 | 报告、现实性标签、分层采样、等价归并 | 未做 | — |
| 15 | `cargo dropwise test`（CancelSan） | 未做 | 在运行脚本中手动用过 `[patch.crates-io]` 替换 sqlx-postgres 和 bb8，机制可行。遇到过一次同名同版本 path 包冲突，靠改版本号绕开 |
| 16 | futurelock 检测 | 未做 | — |
| 17 | coop 叶子层注入 | 未做 | — |

## 实际发现

| 发现 | 性质 | 状态 |
|---|---|---|
| sqlx 0.9.0 `Pool::begin()` 取消后池化连接停留在事务内 | 上游已知缺陷（#4393），**T3 当年漏掉**；事后复现，修复版对照为 0 | 上游已修复 |
| bb8 `get()` 在 `is_valid` 中途被丢弃时，连接被放回池中 | 新问题；字节流协议的 manager 会错位，bb8-memcached 会静默返回其他 key 的值 | 已提交 [djc/bb8#324](https://github.com/djc/bb8/issues/324)、修复 PR [#325](https://github.com/djc/bb8/pull/325)（CI 全部通过） |
| bb8-memcached 只在自身超时触发时标记 tainted | 新问题 | 已提交 [dqminh/bb8-memcached#16](https://github.com/dqminh/bb8-memcached/issues/16) |
| deadpool-memcached / async-memcached 的 `version()` 不检查前缀 | 新问题；用户中途取消后连接持续报错，且写入照样生效 | 未上报 |
| sqlx-mysql 0.9.0 `Pool::begin()` 取消后连接停留在事务内，后继者的写入丢失 | 新问题；与 #4393 同一模式，上游的修复只覆盖了 Postgres | 已提交 [transact-rs/sqlx#4467](https://github.com/transact-rs/sqlx/issues/4467)、修复 PR [#4468](https://github.com/transact-rs/sqlx/pull/4468) |
| lettre 0.11.23 池化 `send` 在 DATA 阶段被取消后，池位持续卡死 | 新问题；连接池是默认 feature | 已提交 [lettre/lettre#1170](https://github.com/lettre/lettre/issues/1170)、修复 PR [#1171](https://github.com/lettre/lettre/pull/1171) |
| tungstenite send 被取消后仍会送达 | 契约外行为（Sink 侧没有承诺） | 不需要上报 |

## 调整后的优先级

1. **继续用“目标清单 + 幸存者 + 允许集合”这套组合扫更多库。** 成本低，已经证明能出结果。优先选在单条字节流上一问一答、并且带池化或复用的客户端。
2. **把允许集合校准和幸存者模板收进 Dropwise API**，目前它们还是实验代码。只有确定 API 形态之后，CancelSan 这类零配置接入才有意义。
3. 静止判定、futurelock、CancelSan 保留在 REPORT.md 的清单中，等前两项稳定后再评估。

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
