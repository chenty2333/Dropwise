# T1–T3 回扫：零发现里有多少来自目标选择与观察方式

> 事后（post-hoc）回扫，与冻结的 `PREREGISTRATION.md` 研究分开报告。冻结场景、`runs/` 历史结果与 `RESULTS.md` 不变。
> 触发原因：SQLx #4393（`Pool::begin()` 在 BEGIN 往返中被取消后，池化连接停留在事务内）是 sqlx 0.9.0 中的真实缺陷，而 T3 当时测试的正是 0.9.0、结论为零发现。

## 1. 问题

T1（tokio-tungstenite 0.30.0）、T2（bb8 0.9.1）、T3（sqlx 0.9.0）的预注册场景都是零发现。若改用 [`docs/choosing-targets.md`](../docs/choosing-targets.md) 的目标清单与幸存者观察方式，同样版本上能看到什么？零发现中哪些归因于**目标选择**，哪些归因于**观察方式**？

这不是新缺陷挖掘的无偏估计：回扫者已知 #4393，清单也是在知道它之后写出的。结果只回答“当时的盲区在哪里”。

## 2. 计划（运行前确定）

判定统一使用允许集合（`support/allowed.rs`）：

- **校准**：同一场景运行两次“操作完全不执行”（pre）和两次“操作正常完成”（post）；两次不一致记 Unknown，不判定取消计划。
- **期望**：调用方放弃操作时 `Atomic`，观察结果须等于 pre 或 post；调用方会重试或继续时 `Completed`，须等于 post。
- **观察**：幸存者（下一位使用者或对端）实际做一次有副作用的操作，并由独立观察者确认结果，而不是只看回显。

归因表中，“旧目标”指预注册时已经取消过的调用，“旧观察”指当时的检查。

| ID | 目标 | 新/旧目标 | 观察 | 新/旧观察 | 期望 | 契约 |
|---|---|---|---|---|---|---|
| R1.1 | tungstenite `send`，写端背压，取消后重试 | 新 | 对端收到的数据序列 | 新 | Completed | 无（Sink 侧无保证） |
| R1.2 | 同上，取消后放弃 | 新 | 同上；另记“报告已取消但对端收到” | 新 | Atomic | 无 |
| R1.3 | `next()` 收到 ping 后 pong 写入挂起 | 旧 | 对端收到的 pong 数与 payload | 新 | Completed | C1 相邻（读侧） |
| R1.4 | `client_async` 握手中取消 | 新 | 同一顺序 accept 循环能否服务下一位客户端 | 新 | Atomic | 无 |
| R2.1 | bb8 `get()`，取消落在 `is_valid` 中 | 旧 | 下一位借用者的请求得到自己的回复；服务端请求日志 | 新 | Atomic | C2 相邻 |
| R2.2 | 同上，取消来自 bb8 自己的 `connection_timeout` | 旧（内部） | 同上 | 新 | Atomic | C2 相邻 |
| R2.3 | 借用者持有连接、请求已发未收时取消 | 新 | 同上 | 新 | Atomic | 无（manager/调用方责任） |
| R2.c | R2.1/R2.3 换成用 `has_broken` 识别在途请求的 manager | — | 同上 | 新 | Atomic | 对照 |
| R3.1 | sqlx `Pool::begin()` | 新 | 自动提交写入 + 独立连接确认 | 新 | Atomic | 无公开契约；上游已认定为缺陷 |
| R3.1e | 同上 | 新 | 当时的回显式后续查询 | 旧 | Atomic | 同上 |
| R3.2 | `Pool::acquire()` | 新 | 写入 + 独立确认 | 新 | Atomic | 无 |
| R3.3 | `PgConnection::prepare()`（池化连接） | 新 | 用同一语句再执行 + 写入 + 独立确认 | 新 | Atomic | 无 |
| R3.4 | 当时的 `fetch_one` 目标 | 旧 | 写入 + 独立确认 | 新 | Atomic | C3 |
| R3.5 | 整个事务 `begin; insert; commit` 作为一个目标 | 新 | 写入 + 独立确认 | 新 | Atomic | 允许集合正对照 |
| R3.6 | 两条自动提交 insert，中间有 await | 新 | 同上 | 新 | Atomic | 允许集合负对照（应判 Neither） |

R3.1、R3.5 在发布版 sqlx 0.9.0 与打上上游修复 PR #4394（只改 `sqlx-postgres/src/transaction.rs`）的同版本上各跑一次。

配置：T1/T2 用 `Flavor::CurrentThread`（与预注册相同），`races = [Immediate, AfterWake]`，`max_cancellations = 1`。T3 用 `Flavor::MultiThread { workers: 1 }`，与 `survivors.rs` 相同。每项重复 3 次。计数报告计划数、违反数、未实现数，以及取消计划中 Pre/Post/Neither/Unknown 的分布。

预算：半天。临时 PostgreSQL 只监听 127.0.0.1，结束后停止。不向上游提交任何东西。

## 3. 结果（2026-10-18）

运行入口为 `bash prospective/run-rescan.sh`。每项重复 3 次，**3 次结果完全一致**。计划穷尽，未实现 0，基线错误 0。新增代码：判定模块 216 行，三个回扫文件共 744 行，脚本 73 行。单轮墙钟：T1 约 0.03 s，T2 约 0.01 s，T3 约 3.7 s（不含编译）。补丁版与发布版的 lockfile 只差 sqlx-postgres 的 `source`/`checksum` 两行。

“违反”指在允许集合下判为 Neither 的取消计划数。

| ID | 计划 | 违反 | 观察到的情况 | 归类 |
|---|---:|---:|---|---|
| R1.1 send+重试 | 18 | **18** | 对端收到 `[a, a, b]`：被取消的 send 仍然送达，重试又发一次 | 契约外（Sink 无保证）；重复投递 |
| R1.2 send+放弃 | 18 | 0 | 18/18 判为 Post：调用方得到“已取消”，对端却收到了 | 契约外；静默提交 |
| R1.3 pong | 2 | 0 | pong 数与 payload 都正确 | 干净 |
| R1.4 握手 | 2 | 0 | 下一位客户端照常得到服务 | 干净 |
| R2.1 get / is_valid | 2 | **2** | 下一位借用者的 `ECHO` 收到 `"PONG"`，服务端回给它的 echo 无人读取 | C2 未覆盖的状态缺口，见下 |
| R2.1c 谨慎 manager | 2 | 0 | `has_broken` 发现在途请求，连接被丢弃并重建 | 对照 |
| R2.2 bb8 内部超时 | — | **Neither** | 同 R2.1，触发者是 bb8 自己的 `connection_timeout`，调用方没有取消任何东西 | 同上 |
| R2.2c 谨慎 manager | — | 0（Pre） | — | 对照 |
| R2.3 借用者请求中途 | 4 | 2 | 违反都出现在 Pending #1（即 `get()` 内的 `is_valid`）。请求本身中途取消（#2）是干净的：残留回复是 `"borrower"` 而不是 `"PONG"`，下一次 `is_valid` 会拒绝并丢弃该连接 | 同 R2.1 |
| R2.3c 谨慎 manager | 4 | 0 | — | 对照 |
| R3.1 `Pool::begin` [发布版] | 4 | **2** | Pending #2（BEGIN 往返），两种时机下后继者的写入都不可见（0 行，应为 1） | 上游缺陷 #4393 |
| R3.1 [PR #4394] | 4 | 0 | — | 修复版对照 |
| R3.2 `acquire` | 2 | 0 | 两版本均干净 | 干净 |
| R3.3 `prepare` | 2 | 0 | 两版本均干净 | 干净 |
| R3.4 原 `fetch_one` 目标 | 4 | 0 | 两版本均干净 | 干净 |
| R3.5 整个事务 [发布版] | 8 | **2** | 2 个 Neither 落在 `begin()` 内（#4393）；其余 4 个 Pre、2 个 Post | #4393 |
| R3.5 [PR #4394] | 8 | 0 | 6 个 Pre、2 个 Post | 允许集合正对照 |
| R3.6 两次自动提交写入 | 10 | **6** | 两版本相同：第一条已提交、第二条没有（可见 2 行，pre=1，post=3） | 负对照：真实的半完成态 |

另外，按允许集合改写的 `survivors.rs` 与 `commit_reply.rs` 结果不变：危险写法仍然报出，安全写法干净（`run-interruption.sh`，3 轮）。

### 归因：零发现来自哪里

| 目标 | 当时的观察 | 新观察 |
|---|---|---|
| **T3 旧目标**（查询 future） | 零发现（冻结研究） | R3.4：仍然干净 |
| **T3 新目标**（`begin`） | R3.1 的 echo 观察：后继者的回显查询全部正确，**漏掉 #4393** | R3.1：抓到 |
| **T2 旧目标**（`get`） | 零发现（manager 只 sleep，连接上没有协议状态） | R2.1/R2.2：抓到 |
| **T1 旧目标**（`next`） | 零发现 | R1.3：仍然干净 |
| **T1 新目标**（`send`、握手） | — | send 在契约外重复或静默提交；握手干净 |

- **T3**：目标和观察**缺一不可**。只换观察（R3.4）或只换目标（R3.1 的 echo 观察）都看不到问题，两者同时换才看到。
- **T2**：目标当年已经选对，缺口在**观察对象的模型**。测试 manager 的连接没有协议状态，“验证到一半被取消后放回池里”这件事无从显现。
- **T1**：在其书面契约内（读侧），零发现在新观察下依然成立。契约外的发送侧，取消后的 send 在首次 Pending 时就已经“提交”，取消后重试会导致重复投递。

### 允许集合与“等于基线”的差别

R3.5 在修复版上有 6 个计划判为 Pre，也就是事务在提交前被取消、干净地回滚了。如果只拿“等于正常完成”来判，这 6 个都会成为误报；用允许集合判是 0 个。在同一结构下，R3.6 的 6 个半完成态和 R3.5 发布版中 #4393 造成的 2 个计划仍然被报出。

### R2：bb8 在 `is_valid` 中途被取消

`Pool::get()` 先把连接包成 `PooledConnection`，再 await `is_valid`（`bb8-0.9.1/src/inner.rs:93-113`）。一旦在这期间被取消，包括被 bb8 自己的 `connection_timeout` 取消，`PooledConnection` 就会以 `Present` 状态把连接放回池中（`api.rs:531-541`，`inner.rs:143-158`），唯一的拦截点是同步的 `has_broken`。验证请求的回复如果没被读取，就会被下一次验证读到，而它恰好也是 `"PONG"`，于是验证通过，此后每个请求都错位一位。

- **按书面契约 C2（不丢连接、不丢容量）不算违反**，连接确实没有丢。问题在于契约没有提到：被打断的验证会留下状态。
- **有两种低成本缓解。** 一是 manager 在 `has_broken` 里报告在途请求（R2.1c/R2.2c/R2.3c 均干净）。二是 bb8 在 await `is_valid` 前把状态设为 `Invalid`、成功后再恢复，已在第 4 节实现并验证。
- 真实 manager 上的影响见第 4 节。
- 按标题检索 bb8 的 issue（关键词 `cancel`、`is_valid`），没有找到描述此问题的条目。2026-10-18 已上报，见第 4 节末尾。

### 局限

- 回扫者已知 #4393，清单是知道它之后写的，不能据此估计未知缺陷的检出率。
- T2 的协议 manager 是我们构造的模型，不是某个真实 manager 的代码。
- 只重复 3 次，配置固定；T3 使用 `MultiThread` 实时时间，Pending 数可能随环境变化（本轮 3 次相同）。
- 没有修改冻结研究的任何文件；冻结研究的结论（预注册契约内零发现）仍然成立，本文只解释其边界。

## 4. bb8：修复验证与真实 manager（2026-10-18）

运行入口为 `bash prospective/run-bb8-fix.sh`，每项重复 3 次，**结果完全一致**。补丁 `t2-bb8/patches/bb8-invalid-until-validated.diff` 只改动 `Pool::get()`：await `is_valid` 前把连接标为 `Invalid`，验证成功后恢复为 `Present`。这样，验证中途被丢弃的连接会被 bb8 丢弃并补建，而不是放回池中。bb8 main（`6dff1ee`）和最新发布版 0.9.1 中，这段代码与补丁前相同。补丁同时适用于 bb8 0.8.6 与 0.9.1。

### 4.1 修复本身

- **T2 回扫：** R2.1、R2.2、R2.3 在补丁版上全部干净，带谨慎 manager 的对照组也仍然干净。
- **冻结场景 S2.1–S2.5：** 补丁版上违反数为 0。阳性对照 mutant 的违反数与冻结研究完全相同（3/0/3/0/3/42）。S2.5 的计划数由 54 变为 63：验证中途被丢弃的连接要重建，`connect` 多出几个 Pending 边界。没有退化。
- **代价：** 验证被打断时会多建一次连接。

### 4.2 已发布的 manager

先按 crates.io 近 90 天下载量，阅读排名前 10 的 bb8 manager 的 `is_valid`/`has_broken` 实现（静态），再对本机能起服务的三个做动态测试。动态测试中，池经由一个回环代理连接真实服务：

- **两种取消来源：** 一是 Dropwise 在 `get()` 的验证中途取消；二是代理把验证回复扣住 500 ms，让 bb8 自己的 `connection_timeout`（200 ms）触发取消。
- **幸存者的操作：** 在同一个连接上依次执行写自己的 key、读两个预先写入的 key A 和 B、读回自己的 key，遇到报错也继续往下执行；另由一条独立连接确认写入。

| manager（近 90 天下载） | 连接类型 | 动态：Dropwise 取消 | 动态：bb8 内部超时 | 补丁版 bb8 |
|---|---|---|---|---|
| bb8-redis 0.26.0（134 万） | redis `MultiplexedConnection` | 2 计划，干净 | 干净 | 干净 |
| bb8-postgres 0.9.0（120 万） | tokio-postgres `Client`（后台连接任务） | 2 计划，干净 | 干净 | 干净 |
| bb8-memcached 0.7.0（131；累计 10 万），基于 bb8 0.8.6 | memcache-async，单条字节流一问一答 | **2/2 违反** | **违反** | 两种取消均干净 |

bb8-memcached 违反时，幸存者会话为 `set(own)=OK, get(a)=InvalidData, get(b)="value-a", get(own)="value-b"`。也就是说，在一次报错之后，**连续两次读取静默地返回了另一个 key 的值**。原因如下：

1. 验证（`version`）的回复残留在连接上，下一次验证读到它，验证通过。
2. 此后每个请求读到的都是前一个请求的回复。
3. memcache-async 解析 `get` 回复时不核对 key（`ascii.rs:52-57`）。
4. bb8-memcached 只在它**自己的**超时触发时才把连接标为 tainted（`client.rs:143-146` 等），外部丢弃的请求不会被标记，所以这个错位的连接会一直留在池里。

其余七个 manager 只做了静态阅读，**未动态验证**：

- **bb8-tiberius：** tiberius 每次发新请求前调用 `flush_stream()`，丢弃上一个请求未读完的回复（`connection.rs:231`），主路径可以自我恢复。“取消正好落在 flush 过程中”这种边缘情况没有检查。
- **async-bb8-diesel / bb8-diesel：** 验证在阻塞线程中执行，取消不会打断它。
- **bb8-lapin：** 验证只是同步检查状态，没有 await。
- **bb8-ldap / bb8-mongodb：** 驱动本身是多路复用的。
- **bb8-oracle：** 验证在 `spawn_blocking` 里继续执行，可能与下一位借用者并发使用同一个连接，取决于 ODPI-C 的线程模型，未检查。

### 4.3 严重程度

- **单次后果严重：** 静默返回错误数据，而且是别的 key 的数据。在缓存场景里，这可能意味着把一个用户的数据返回给另一个用户。错位的连接会留在池中，持续影响后续借用者，直到它因为别的原因被关闭。
- **触发不苛刻：** `test_on_check_out` 默认开启；取消可以来自调用方，例如请求超时、客户端断开后 handler 被丢弃、`select!`，也可以来自 bb8 自己的 `connection_timeout`，只要验证回复慢于超时。
- **受影响面窄：** 下载量最大的 bb8-redis、bb8-postgres 经实测不受影响。已确认受影响的 bb8-memcached 下载量很小（近 90 天 131 次）。自写的、基于单条字节流的 `ManageConnection` 实现无法统计，是主要的未知面。
- **修复面在 bb8：** 6 行改动就能保护所有 manager，不需要每个 manager 自己在 `has_broken` 里跟踪在途请求。

2026-10-18 已上报：

- bb8 issue [djc/bb8#324](https://github.com/djc/bb8/issues/324)；
- 修复 PR [djc/bb8#325](https://github.com/djc/bb8/pull/325)：即本补丁，另附一个不依赖外部服务的回归测试 `test_drop_during_is_valid`。该测试在 bb8 main 上失败、打上补丁后通过；bb8 原有测试在 stable 和 MSRV 1.75 下均通过；
- bb8-memcached issue [dqminh/bb8-memcached#16](https://github.com/dqminh/bb8-memcached/issues/16)，与它已有的 #9 相关。
