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
