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

## 5. deadpool 对照（2026-10-18）

deadpool 0.13.1 是下载量最大的 Rust 异步连接池（近 90 天 4100 万次，bb8 为 930 万次）。运行入口同为 `run-bb8-fix.sh` 的第 3 部分，每项重复 3 次，结果完全一致。

### 5.1 核心：验证中途的窗口在设计上已经关闭

`timeout_get` 在 `recycle` 期间把对象放在 `UnreadyObject` 里；这个守卫被丢弃时会丢掉对象（`size -= 1`、`detach`），而不是放回池中（`pool.rs:481-502`）。这正是 bb8 补丁的做法。

使用与 T2 回扫相同的进程内协议模型：

| ID | 取消落点 | 严格 recycle（回复必须是 PONG） | 宽松 recycle（任何回复都通过） |
|---|---|---|---|
| D1 | `get()` 内的 `recycle`（Dropwise） | 2 计划，干净 | 2 计划，干净 |
| D2 | deadpool 自己的 `recycle_timeout` | 干净（Pre） | 干净（Pre） |
| D3 | 用户在自己的请求中途取消 | 4 计划，干净 | **4 计划中 2 个违反** |

剩下的窗口是 D3：用户丢弃 `Object` 后，对象会被无条件放回池中（`object.rs:85-93`、`pool.rs:455-467`）。deadpool 没有类似 `has_broken` 的同步关口，唯一的关口是下一次 `recycle`。因此是否安全，取决于 `recycle` 能不能识别残留的回复。

### 5.2 已发布的 deadpool manager

静态阅读近 90 天下载量最高的 deadpool manager 的 `recycle`：

- **deadpool-postgres（681 万）：** tokio-postgres 多路复用；`Fast` 模式只检查连接是否已关闭。
- **deadpool-redis（369 万）：** 多路复用；每次 `PING` 都带递增编号，并核对回复。
- **deadpool-lapin、deadpool-amqprs：** 只检查连接状态。
- **deadpool-ldap：** ldap3 多路复用。
- **deadpool-tiberius：** tiberius 在每次新请求前用 `flush_stream` 清理残留回复。

其中唯一一个“单条字节流 + 宽松校验”的是 **deadpool-memcached 0.5.0**（近 90 天 839 次）。它的 `recycle` 调用 async-memcached 0.7.0 的 `version()`，而 `version()` 只要求响应行不短于 8 字节，然后截掉前 8 个字符，**不检查是否以 `VERSION ` 开头**（`lib.rs:169-183`）。所以 `STORED\r\n` 这样的残留回复也会被当作验证通过。

D4：deadpool-memcached 连接真实的 memcached 1.6，用户在 `get` 或 `set` 请求中途被取消：

| 用户取消的请求 | 计划 | 违反 | 下一位使用者的会话 |
|---|---:|---:|---|
| `get` | 4 | **2** | `set`、`get(a)`、`get(b)`、`get(own)` 全部返回协议错误；但 `set` 实际已在服务端生效（独立连接可见） |
| `set` | 4 | **2** | 同上 |

两次违反都出现在 Pending #2，也就是用户请求本身。本次观察到的是“每个操作都报错、写入却已生效”，**没有观察到 bb8-memcached 那种静默返回其他 key 值的情况**。宽松的 `version()` 会让这个连接一直通过 `recycle`、留在池中。

### 5.3 小结

- deadpool 的核心在 bb8 #324 这个窗口上是安全的。
- 风险转移到了“用户中途取消 + recycle 校验宽松”这一组合上。主流 manager 校验严格或使用多路复用，不受影响；deadpool-memcached 受影响，后果是持续报错和“报错但已写入”。
- 一处修复点：async-memcached 的 `version()` 检查 `VERSION ` 前缀。另一处可选的修复点是 deadpool-memcached 在请求中途被丢弃时标记连接。**均未上报。**

## 6. 扫描更多客户端（2026-10-18）

按“下载量大、单条连接上按顺序一问一答或带会话状态、会被池化复用”挑选扫描对象。运行入口为 `bash prospective/run-scan-clients.sh`，实验代码在 `prospective/scan-clients/`。服务端是只监听回环地址的 MariaDB 11.4 和 Mailpit v1.27（真实的 SMTP 服务，用它的 HTTP API 作独立观察者）。每项重复 3 次，结果一致。补丁在 `scan-clients/patches/`。

### 6.1 sqlx-mysql 0.9.0（近 90 天 4247 万次下载）

`MySqlTransactionManager::begin` 要等 BEGIN 往返完成后才把 `transaction_depth` 加一，这和 #4393 修复前的 Postgres 是同一个模式，而且没有任何 guard。上游 main 至今未改，也没有检索到相关的 issue 或 PR（#4394 和 #4409 都只涉及 Postgres）。

| ID | 目标 | 发布版 | 补丁版 |
|---|---|---|---|
| S1 | `Pool::begin()` | 4 个计划中 **2 个违反**（Pending #2，即 BEGIN 往返，两种时机都违反） | 0 |
| S2 | `begin; insert; commit` 整体 | 8 个计划中 **2 个违反**，另有 4 个 Pre、2 个 Post | 0（6 个 Pre、2 个 Post） |

违反时，后继者通过池做的自动提交写入对独立连接**不可见**（0 行，应为 1 行）：它落进了一个永远不会提交的事务里，连接关闭时被回滚。补丁 `sqlx-mysql-begin.diff` 照搬 #4394 的做法：在往返之前就增加深度，再用 guard 在被取消时排队一条 ROLLBACK。

### 6.2 lettre 0.11.23（近 90 天 765 万次；`pool` 是默认 feature）

被取消的 `send` 会丢弃 `PooledConnection`，连接随后被回收，除非 `has_broken()` 为真；而 `panic` 只在命令失败时才会被置位。复用前，池会发一次 `NOOP`，并接受任何正面回复，包括 3xx 中间态回复。异步传输没有读超时。幸存者通过同一个池再发 B、C 两封邮件，每封设 3 秒超时；独立观察者是 Mailpit 实际存下的邮件（主题按“进程号加运行号”打标签，避免不同运行之间互相串扰）。

| ID | 目标 | 发布版 | 补丁版 |
|---|---|---|---|
| L1 | 16 字节正文的 `send` | 10 个计划中 **2 个违反** | 0 |
| L2 | 8 MiB 正文的 `send`（写正文时也会 Pending） | 16–22 个计划中 **8–10 个违反**（实时时间下 Pending 数会浮动） | 0 |

- **取消落在等待 `354` 时（L1/L2 的 Pending #4）：** 服务端已进入 DATA 状态。下一次取用时，`NOOP` 读到残留的 `354` 并判定连接健康，B 的 `MAIL FROM` 于是被当作正文内容，得不到回复，B 卡住。B 被超时丢弃时，连接**再次被回收**，C 也卡住。这个池位会持续不可用；调用方如果没设超时，发送就会永远挂起。
- **取消落在写正文的过程中：** 下一次 `NOOP` 得不到回复，B 卡住，但 B 被丢弃时连接是在池外被关闭的，所以 C 正常发出。
- 本次测试**没有观察到**邮件内容串到别人的邮件里：客户端总是先等回复再写下一条命令，所以 B 的正文从来没有被写出去。
- 补丁 `lettre-send-in-progress.diff`：从 `send` 开始到完整结束，标记“发送中”，并在 `has_broken()` 中报告；处于这个状态时，`abort()` 不发送 `QUIT`，直接关闭连接。原因是服务端正在读取正文，不会回复 `QUIT`，等待回复会让回收任务永远挂着、socket 不被关闭。这一处是在写上游回归测试时发现并补上的，补丁版上 L1、L2 全部干净。

### 6.3 上报（2026-10-18）

- **sqlx：** issue [transact-rs/sqlx#4467](https://github.com/transact-rs/sqlx/issues/4467)，修复 PR [#4468](https://github.com/transact-rs/sqlx/pull/4468)。PR 附回归测试 `it_rolls_back_a_transaction_cancelled_during_begin`：在 main 上失败，修复后通过；对 MariaDB 11.4 跑完整的 MySQL 测试套件也通过。PR 中说明了它与 #4445（嵌套 `begin` 的回滚问题，属于 sqlx-core）的关系。
- **lettre：** issue [lettre/lettre#1170](https://github.com/lettre/lettre/issues/1170)，修复 PR [#1171](https://github.com/lettre/lettre/pull/1171)。两个使用进程内假服务器的确定性测试，结果如下：master 上两个都失败；只改 `has_broken()` 时，“写正文中途取消”那个测试仍然失败；完整修复后两个都通过（各跑 3 次）。

### 6.4 静态排查后未做动态测试的

- **sqlx-sqlite：** `begin` 交给 worker 线程执行，取消异步调用不会中止 worker 里的命令。未验证。
- **mysql_async、diesel-async、async-imap：** 已下载源码，本轮未分析。

一处测试框架的错误：L2 的第一版各次运行共用 Mailpit 收件箱且主题没有打标签，于是在修复版上把上一次运行晚到的 A 误判为“A 出现两次”。改为按运行打标签后，这个现象消失。上面所有数字都来自修正后的版本。

### 6.5 改用 Dropwise 判定 API（2026-10-18）

S1/S2 与 L1/L2 已从实验内的 `allowed.rs` 判定改为库的 `explore_observed`（`Expect::Atomic`）：场景不再接收 `Mode`，`ctx.target` 直接标记操作；`pre` 由 `after_pending = 0` 的校准计划给出，`post` 是不取消任何东西的基线；两次采样不一致记为 Unknown，校准运行失败记入 `baseline_errors`，不判为违反。R2 的四个 cut 实验同样改用该 API；R2.2 没有 Dropwise 的 cut，继续使用 `allowed.rs` 的 `Calibration`，`allowed.rs` 也仍供 t1/t3 使用。改写前的 commit 为 `c32e302`。

对照结果（`bash prospective/run-scan-clients.sh`，3 轮；之后修正多 cut 校准、合并 `explore` 与 `explore_observed` 的主循环后又跑 1 轮复核，结果相同。S1/S2/L1 每轮完全一致，L2 的计划数与 Pre/Post 分布随实时时间浮动）：

| ID | 改写前 | 改写后 |
|---|---|---|
| S1 | 4 个计划，2 违反 | 4，2（0 Pre / 2 Post / 2 Neither） |
| S2 | 8，2（4 Pre / 2 Post） | 8，2（4 Pre / 2 Post / 2 Neither） |
| L1 | 10，2 | 10，2（6 Pre / 2 Post / 2 Neither） |
| L2 | 16–22，8–10 | 16–18，9–10（6 Pre / 0–3 Post / 9–10 Neither；补丁版最多 3 个未实现） |
| 补丁版 | 全部 0 | 全部 0 |

R2.1/R2.1c/R2.3/R2.3c 在发布版上仍为 2/2、2/0、4/2、4/0，fixed 上全部 0。汇总行里的计数现在来自 `Report`（只统计取消计划，不含基线），字段名沿用旧格式。

新增维度（可调度 gap、Proxy 残留字节）是单独的新实验，见 6.6，没有改动上面的对照组。

### 6.6 新维度：可调度间隔与残留字节（2026-10-18，单独实验）

运行入口 `NEW_DIMENSIONS=1 bash prospective/run-scan-clients.sh`（需要 `net` feature，仅限 Linux），发布版与补丁版各 2 轮，结果完全一致。两组都在 L1（16 字节正文）上做：

- **L1-gap**：把复用前固定的 `sleep(50ms)` 换成 `ctx.gap(Gap::Sleep(50ms))`，另外探索 `Gap::Now` 和 `Gap::Yield(1)`。
- **L1-residue**：SMTP 连接经过 `dropwise::net::Proxy`；被取消的 `send` 之后等 50ms，在 B、C 开始之前用 `FIONREAD` 读取客户端 socket 中未读的字节数，并把它加进观察值。

| ID | 变体 | 计划 | 违反 | Pre / Post / Neither |
|---|---|---:|---:|---|
| L1-gap | 发布版 | 30 | 6 | 18 / 6 / 6 |
| L1-gap | 补丁版 | 30 | 0 | 24 / 6 / 0 |
| L1-residue | 发布版 | 10 | **8** | 2 / 0 / 8 |
| L1-residue | 补丁版 | 10 | 0 | 8 / 2 / 0（残留始终为 0） |

- **可调度间隔：** 三种间隔下，违反都落在 Pending #4（等待 `354`），各 2 个。在这个场景里，复用前等多久不改变结果。
- **残留字节：** 对照组 L1 只有 Pending #4 的 2 个违反；加上残留字节后多出 6 个：

  | 取消落点 | 残留字节 | 幸存者 B、C | 原观察的判定 |
  |---|---:|---|---|
  | Pending #2、#3（两种时机） | 14 | 都成功，A 未存储 | Pre |
  | Pending #4 | 50 | 都卡住 | Neither（对照组已报） |
  | Pending #5 | 48 | 都成功，A 已存储 | Post |

  14 字节与 `250 2.1.0 Ok\r\n` 的长度相同（回复原文未逐字核对）。也就是说，连接被放回池时还欠着一条回复，此后每条命令读到的很可能是上一条命令的回复；因为这些回复都是 2xx，lettre 没有察觉。**已验证（2026-10-18，见 6.9）：** 失败发生在哪一步决定后果。RCPT 的 `550` 会被后面的命令读到，但 `send` 仍然返回 `Err`；DATA 之后的 `554` 会被留成下一条未读回复，`send` 返回 `Ok(354)` 而服务器已经拒收——这是用户可见的假成功。补丁版（#1171，`send` 全程标记“发送中”）把这样的连接丢弃，残留为 0。
- 两组都是新实验，计划数不能和对照组直接相加或比较。

### 6.7 消融阶梯：简单取消测试能走到哪一级（2026-10-18，单独实验）

目的：同一场景骨架、同一 Dropwise 配置，只在“取消之后测什么、怎么判”上分层，检查简单取消测试是不是完整模板的稻草人。运行入口：

- `LADDER=1 bash prospective/run-scan-clients.sh`（sqlx-mysql、lettre，发布版与补丁版各 3 轮）；
- `LADDER=1 bash prospective/run-bb8-fix.sh`（bb8，发布版与补丁版各 3 轮）。

新增实验文件 `scan-clients/tests/sqlx_mysql_ladder.rs`、`scan-clients/tests/lettre_ladder.rs`、`t2-bb8/tests/rescan_ladder.rs`，共享报告模块 `support/ladder.rs`；原有测试文件未改动。四个层级：

- **B1：** 取消后场景立即结束，用 `dropwise::explore`；只要不 panic、不超时、没有 Obligation 泄漏就算通过。
- **B2：** 取消后通过同一个池/连接，用**完全相同的参数**重做同一个操作；返回 `Ok` 就算通过（超时算失败）。
- **B3：** 取消后通过同一个池做一个**不同的**同类操作（换查询、换参数、换收件人）；只看是否返回 `Ok`，不检查值。
- **M：** 现有的 `explore_observed(Expect::Atomic)` 幸存者 + 独立观察者。lettre 另跑 **M-residue**（现有 `lettre_residue` 的观察值）：把复用前的残留字节加进观察值；和现有实验一样只做 L1。

表中为“计划数/违反数”。除 L2 外，3 次重复完全一致；L2 的计划数随实时时间的 Pending 数浮动，表中给出范围。

| 场景 | 版本 | B1 | B2 | B3 | M | M-residue |
|---|---|---|---|---|---|---|
| S1 `Pool::begin` | 发布版 | 4/0 | 4/0 | 4/0 | 4/**2** | — |
| S1 `Pool::begin` | 补丁版 | 4/0 | 4/0 | 4/0 | 4/0 | — |
| S2 整个事务 | 发布版 | 8/0 | 8/0 | 8/0 | 8/**2** | — |
| S2 整个事务 | 补丁版 | 8/0 | 8/0 | 8/0 | 8/0 | — |
| L1 16 字节 `send` | 发布版 | 10/0 | 10/**2** | 10/**2** | 10/**2** | 10/**8** |
| L1 16 字节 `send` | 补丁版 | 10/0 | 10–14/0 | 10/0 | 10/0 | 10/0 |
| L2 8 MiB `send` | 发布版 | 16–18/0 | 16–22/**9** | 16–22/**9** | 16–18/**8–9** | 未运行 |
| L2 8 MiB `send` | 补丁版 | 16–18/0 | 16–18/0 | 16/0 | 16–18/0 | 未运行 |
| R2.1 `get` | 发布版 | 2/0 | 2/0 | 2/0 | 2/**2** | — |
| R2.1 `get` | 补丁版 | 2/0 | 2/0 | 2/0 | 2/0 | — |
| R2.3 借用者请求中途 | 发布版 | 4/0 | 4/0 | 4/0 | 4/**2** | — |
| R2.3 借用者请求中途 | 补丁版 | 4/0 | 4/0 | 4/0 | 4/0 | — |

发布版 M 的违反计划与原实验一致：S1/S2 在 Pending #2（BEGIN），L1/L2 在 Pending #4 及正文写入阶段，R2.1/R2.3 在 Pending #1（`is_valid` 内部）。固定版的 M 计划数也与原实验相同；L2 固定版有 0–1 个计划未实现（实时时间下 Pending 数变化），不计为违反。L1 固定版 B2 的 14 个计划里 4 个未实现是同一类现象。

**B1 在所有场景、两个版本上都是 0。** 按定义它只看取消本身，看不到取消之后才出现的状态；这是预期的“零”，不是阳性结果。

#### “M 抓到、但 B3 没抓到”的计划

| 场景 | 计划（`#target@pending:race`） | M 看到 | 为什么 B3 看不到 |
|---|---|---|---|
| S1 | `#0@2:Immediate`、`#0@2:AfterWake` | 幸存者的自动提交 INSERT 返回 `Ok`，独立连接看到 0 行（应为 1） | B3 的自动提交 INSERT 也返回 `Ok`：MySQL 接受了它，只是它落在未结束的事务里；B3 不看独立连接，所以看不到 0 行 |
| S2 | `#0@2:Immediate`、`#0@2:AfterWake` | 同上 | 同上；B2 重做整个事务也返回 `Ok`，事务状态对返回值不可见 |
| R2.1 | `#0@1:Immediate`、`#0@1:AfterWake` | 幸存者的 `ECHO survivor` 得到 `Ok("PONG")`，服务端日志里没有这次 ECHO | 残留的验证回复 `PONG` 本身就是成功回复；B3 的 `ECHO ladder` 得到 `Ok("PONG")`，它不比较 payload，也不看服务端日志 |
| R2.3 | `#0@1:Immediate`、`#0@1:AfterWake` | 同上（取消落在 `get()` 的 `is_valid` 里，`ECHO borrower` 还没发出） | 同上 |
| L1 M-residue | `#0@2:Immediate`、`#0@2:AfterWake` | 残留 14 字节，B、C 成功、A 未存储 | B3 的 send 返回 `Ok`（NOOP 消费了残留的 2xx），且 B3 不看残留字节；去掉残留的普通 M 观察到 `stored=[B,C,W]`，等于 pre，也判不到 |
| L1 M-residue | `#0@3:Immediate`、`#0@3:AfterWake` | 同上 | 同上 |
| L1 M-residue | `#0@5:Immediate`、`#0@5:AfterWake` | 残留 48 字节；B、C 都成功，A、B、C 都已存储 | 返回值与普通观察都等于 post；只有残留字节不同，B3 不检查它 |
| L2 | `#0@9:AfterWake`（第 1 轮） | 8 MiB 正文写完后最后一个 Pending 边界上，幸存者 B、C 卡住 | 同一轮 B3 有 1 个计划未实现；`#9` 是实时时间下最后的边界，B3 没有稳定地走到它。属于运行间计划集差异，不是判据更弱 |
| L2 | `#0@8:Immediate`（第 2 轮） | 幸存者 B、C 卡住 | 同一轮 B3 枚举了 `#0@8:Immediate`，但它的不同 send 返回了 `Ok`；两次运行独立调度，8 MiB 正文留下的残留字节不同。不是稳定的观察缺口 |

其余计划没有“M 抓到、B3 没抓到”的情况：

- **L1 的普通 M：** B2/B3 在 Pending #4 的同一批挂起点上也会超时，抓到的计划键集合与 M 相同。L1 的 M-residue 中，`#0@4` 也被 B3 抓到（B 超时），只有残留下的 `#0@2/#0@3/#0@5` 是 M-only。
- **L2 的计划集在实时时间下不嵌套：** 除上表两个 M-only 键外，B3 还在 `#0@7:AfterWake`、`#0@8:Immediate`（第 1 轮）和 `#0@5:Immediate`（第 2 轮）上报了违反，而 M 没有。两组是独立运行，Pending 边界和残留字节都可能不同；因此 L2 的 M-only 计数不能当作稳定的方法差异，只能当作实时时间下的观测。
- **L2 的 M-residue 未运行**，原因见上：现有 `lettre_residue` 只做 L1，本次沿用同一实验，没有新造 L2 版本。
- **R2.3 的 Pending #2 计划**（请求本身被取消）在 M 里判为 Post，不是违反，因此没有可供 B3 漏掉的目标。

补丁版上四个层级全部 0 违反，没有需要分诊的违反。B2/B3 在补丁版上的零来自与 M 相同的修复效果：脏连接被丢弃/重建，下一次操作在干净连接上完成。

### 6.8 扣住回复：用代理替代服务端 SLEEP（2026-10-18，单独实验）

`dropwise::net::Proxy` 新增 `Proxy::start_with(upstream, ProxyOptions { reply_delay })`：服务端到客户端方向读到的每一块数据，先在代理里留 `reply_delay`（真实时间）再写给客户端，顺序不变；客户端到服务端方向不变。`Proxy::start` 的行为保持不变。实现和文档同时写明：

- 这是**真实时间**的延迟，只适合 `Flavor::MultiThread`；`Flavor::CurrentThread` 下 Tokio 时间被暂停，sleep 不会前进。
- 扣住期间回复还在代理里，没有进入客户端 socket，所以 `residue()` 看不到它；放行后才计入。

核心测试在 `tests/net_reply_delay.rs`（`--features net`，只用进程内服务器）：有 1000 ms 延迟时客户端的一次读在 100 ms 内不完成（至少返回一次 Pending）；分块内容与顺序不变；两个方向的字节计数分别等于请求和回复长度；Linux 上扣住期间 `residue` 为 0、放行后在客户端未读时为回复长度。默认 CI 与 `--features net` 的 CI 都通过。

#### sqlx-mysql S1：无延迟对照组 vs 扣住 BEGIN 回复

S1 本来就没有 `SLEEP`：`Pool::begin()` 的 BEGIN 往返在本机也会返回 Pending #2。新实验只把池的连接改走代理，让代理把每条上游回复扣 50 ms；观察者仍直连数据库。运行入口 `PROXY=1 bash prospective/run-scan-clients.sh`，发布版与补丁版各 3 轮，三轮一致。

| 方式 | 发布版 | 补丁版 |
|---|---|---|
| 现有 S1（无服务端延迟） | 4 计划，**2 违反**（pre=0 post=2 neither=2） | 4，0（pre=0 post=4） |
| S1 + Proxy 50 ms | 4 计划，**2 违反**（pre=0 post=2 neither=2） | 4，0（pre=0 post=4） |

两种方式的结果完全相同，没有需要解释的差异。原因是代理只把 BEGIN 回复的到达时间推后，没有改变目标 future 的 Pending 边界：取消仍落在 Pending #2；连接被放回池时，BEGIN 的 OK 在代理里，50 ms 后才到达客户端 socket，下一次借出时被当成健康检查的回复读掉，之后的自动提交写入仍落进未结束的事务。也就是说，扣住回复解决的是“怎样让操作停在等待回复处”，不改变“取消后留下什么”。

场景代码行数（定义：非空、首字符不是 `/`；场景闭包 + `explore_observed` 调用 + `#[test]` 外壳；不含 `setup`/`survive`/`config`、报告和 `summary`；扣住回复版本另计它独有的 URL 重写辅助函数）：

| 变体 | 行数 | 说明 |
|---|---:|---|
| 现有 S1 | 10 | 复用 `rescan()` 包装，场景本身只有 6 行 |
| S1 + Proxy | 27 | 多出 17 行：代理场景本身 10 行（比对照组闭包多 4 行，用于启动/持有代理并把代理地址传给 `setup`）、`DW_MYSQL_URL` 改写辅助函数 11 行、测试外壳 6 行（比对照组多 2 行） |

如果把代理测试里内联的报告打印也算进测试函数，proxy 变体是 36 行；上表为了和现有 S1 对齐（它把打印放在共享的 `rescan()` 里），两边都不计报告行。

结论：在 S1 这种“服务端本来就会让 BEGIN 返回 Pending”的场景里，扣住回复是**代码更多、结果相同**的替代品；它的价值在服务端没有延迟原语、又不想改 SQL 的目标上。服务端能配合时，`SLEEP` 仍然是更省代码的做法。

### 6.9 错位回复的后果：DATA 之后的拒收会变成假成功（2026-10-18，单独实验）

6.6 只证明了取消后连接上可能欠一条回复；那句“遇到失败回复时错误会被算到后面的命令上”当时标为推测。本节验证它是否会造成用户可见的错误结果。

**设计。** 用一个新的进程内 SMTP 服务器代替 Mailpit，因为需要让服务器在指定阶段拒收：服务端对 `reject@example.com` 在 RCPT 返回 `550`，对主题为 `reject-after-data` 的邮件在读完 DATA 正文后返回 `554`；其余邮件返回 `250` 并记录主题。服务器还记录每个被接收/拒绝的主题，作为独立观察者。为了不依赖真实 MTA 的行为，实验只使用公开 SMTP 语义：这两个拒绝码都是协议允许的。

场景仍是 L1 的形状：先用一封正常邮件预热池，把 A（16 字节正文）标为取消目标；取消后等 50ms，用 `Proxy::residue()` 记录客户端 socket 里的未读字节（只作为诊断，不进入被判定值），然后通过同一个池发 B（要观察的失败邮件）和 C（正常邮件）。B、C 各设 3s 超时。判定用 `explore_observed(Expect::Atomic)`：

- 被判定值只包含 B 的 `send` 结果（响应码/错误/超时）、服务器是否接收了 B、C 的结果和 C 是否被接收；**不包含残留字节本身**。这样“错位但结果正确”不会被误报，只有用户能看到的结果不同才算违反。
- 残留字节单独记录为 `(residue, B result)` 样本，用来证明错位确实存在。

两个变体：

- **M1（DATA 之后拒收）：** B 在 RCPT 和 DATA 都被接受，正文读完后再被 `554` 拒绝。干净连接上 `send(B)` 应为 `Err(554)`。
- **M2（RCPT 拒收）：** B 的收件人被 `550` 拒绝。干净连接上 `send(B)` 应为 `Err(550)`。

服务器在每条命令回复前等 10ms（慢的进程内服务器），否则本机回环经常在第一次 poll 前就把回复送到，Pending 边界数会随调度变化。即使这样，计划的 Pending 数仍有 10–14 的浮动；原始日志保留了每一轮。

运行入口：`MISALIGN=1 bash prospective/run-scan-clients.sh`（需要 `net`，仅限 Linux），发布版与补丁版各 3 轮。实验代码在 `scan-clients/tests/lettre_misalignment.rs`；补丁版的构建方式与 6.1/6.2 相同。

#### 结果

表中为“计划数 / 违反数”，按第 1、2、3 轮列出；“假成功计划”是 `send(B)` 返回 `Ok(354)`、而服务器记录 B 被拒收的计划并集。

| 变体 | 版本 | 计划（3 轮） | 违反（3 轮） | 假成功计划 | 残留样本（residue → B 结果） |
|---|---|---|---|---|---|
| M1 DATA 后拒收 | 发布版 | 12 / 14 / 10 | **4 / 4 / 4** | `#0@2:AfterWake`、`#0@3:Immediate`、`#0@5:Immediate`、`#0@5:AfterWake` | 14 → `Ok(354)`、18 → `Ok(354)`、37 → `Err("hung")` |
| M1 DATA 后拒收 | 补丁版 | 10 / 10 / 12 | 0 / 0 / 0 | 无 | 无（残留始终为 0） |
| M2 RCPT 拒收 | 发布版 | 12 / 14 / 10 | 2 / 1 / 1 | 无 | 14 → `Err(550)`、18 → `Err(550)`、37 → `Err("hung")` |
| M2 RCPT 拒收 | 补丁版 | 10 / 12 / 12 | 0 / 0 / 0 | 无 | 无（残留始终为 0） |

M1 的 4 个违反中，每一轮都有 2–4 个是假成功，其余是 Pending #4 的已知卡住（`b`、`c` 都超时）；M2 的违反全是 Pending #4 的卡住，没有假成功。补丁版上两个变体的 M 判定全部为 Post，没有违反。

**错位确实造成了用户可见的错误结果。** M1 的假成功计划里：

- 取消后的连接上残留 14 或 18 字节（都是早期命令的成功回复），池的 `NOOP` 读掉它并认为连接健康；
- B 的 `MAIL FROM`、`RCPT`、`DATA` 依次读到上一条命令的 `250`，都被当作成功；
- 正文写完后，`read_response` 读到服务器对 DATA 的 `354`，而 `354` 也是 positive，于是 `send(B)` 返回 `Ok(354)`；
- 服务器实际上在正文后返回了 `554` 并拒收 B；这条 `554` 留在连接上，没有任何调用方读到；
- 下一次借用的 `NOOP` 读到 `554`，连接被判坏并丢弃，C 在新建连接上成功。**B 的拒收从未返回给调用方。**

M2 的结果说明“错误算到后面的命令上”本身不等于后果：B 的 RCPT `550` 被后面的 DATA 命令读到，`send(B)` 仍返回 `Err(550)`，只是读得晚了一步；调用方看到的仍是失败。真正危险的是 DATA **之后**的拒收，因为它是这一轮交互的最后一条回复，后面没有命令会去读它。

补丁版（#1171）在 `send` 的邮件事务开始前置 `sending`，取消后 `has_broken()` 为真，连接被关闭而不是放回池：残留为 0，B 在干净连接上得到 `Err(554)`（M2 为 `Err(550)`），C 正常。两个变体各 3 轮均为 0 违反。

#### 局限

- 服务器是为实验写的最小 SMTP 模型，不是真实 MTA；`554` after DATA 是协议允许的行为，但**没有对具体真实服务器核实**。
- 只测了 L1 形状（16 字节正文、A 在第一封）；L2 的正文写入阶段未单独测试。
- 计划数随实时时间在 10–14 之间浮动，个别计划未实现；但发布版 3 轮都出现了假成功，补丁版 3 轮都没有残留。
- 服务器每条回复加 10ms 延迟，只是为了稳定 Pending 边界；它不改变命令/回复的顺序。
- 本节和 6.6 都是同类实验，但判定值不同：6.6 把残留放进观察值，用来发现“欠回复”；本节把残留排除在判定值之外，只问用户可见的结果。
