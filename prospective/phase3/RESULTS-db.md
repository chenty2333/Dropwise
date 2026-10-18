# Phase 3 数据库目标结果：mongodb、mysql_async、clickhouse-arrow

> **复核注（2026-10-19）：** 本文把 mongodb MG1 记为确认的问题；复核时依据驱动 README 的“Warning about timeouts / cancellation”（RUST-937）改判为契约外行为，不计入 Q1，见 `AMENDMENTS.md` 第 16 条与汇总 `RESULTS.md`。下文的数字和复现过程不变。

> 2026-10-19。实验代码 `prospective/phase3/db-clients/`，运行入口
> `bash prospective/phase3/run-db.sh`（`TARGETS`、`REPS`、`VARIANT=release|patched`）。
> 原始输出现存于 `db-clients/logs/`，每个最终数字都来自那里的
> `release-1..3.log` / `patched-1..3.log`。与 PLAN 的偏离见
> `prospective/phase3/AMENDMENTS.md` 的数据库小节（#1–#7、#14–#15）。
>
> 本文件只覆盖本对话负责的三个数据库目标；HTTP 五个目标的结果在
> `RESULTS-http.md`，由另一个实验负责。

## 方法回顾

- 每个目标按 `docs/choosing-targets.md` 选两个操作：第一个建立会话状态的
  调用（事务 / `USE`），加一个普通请求；找不到会话状态 API 时只测后者
  （clickhouse-arrow 只有 `USE`/`SET` 这类语句）。
- 每个操作跑五个层级：B1（取消后停止）、B2（同参数重试）、B3（不同的同类
  操作，只看 `Ok`）、M（`explore_observed(Expect::Atomic)`，幸存者做有副作用的
  操作，独立连接确认）、M-residue（M 加上 `Proxy::residue()` 的未读字节）。
- 所有层级、所有操作都用同一个 `Config`：`Flavor::MultiThread { workers: 1 }`、
  `races = [Immediate, AfterWake]`、`max_cancellations = 1`、`max_runs = 64`、
  `scenario_timeout = 20 s`、`Settle { tokio_time: 0, ..default }`。
- Pending 手段：每个 crate 统一用 `dropwise::net::Proxy` 的
  `reply_delay = 50 ms` 扣住服务端回复；M-residue 在取消后真实等待 100 ms
  再取 `residue()`。没有用服务端 `SLEEP`，这样事务/`USE` 和普通请求能用
  同一种手段。
- 每层 3 轮。mysql/clickhouse 每次 runner 起一个容器、连跑 3 轮；mongodb
  每轮起一个新的单节点副本集容器（原因见 AMENDMENTS #14）。
- 计划数是 `Report::trials.len()`；违反数是 `report.violations().count()`；
  M 的 pre/post/mixed/neither/unknown 是 `verdict_count`。未实现的计划单列为
  `unrealized`，不算通过。

### 环境

| 目标 | crate 版本 | 服务端 | 说明 |
|---|---|---|---|
| mongodb | mongodb 3.9.1 | `mongo:7.0.21`，单节点副本集 `rs0` | `directConnection=true`；每轮新容器 |
| mysql_async | mysql_async 0.37.1 | 本地 `mariadb:11.4` | 池 `pool_min=1&pool_max=1`；直接 `Conn` 另测 |
| clickhouse-arrow | clickhouse-arrow 0.2.1 | `clickhouse/clickhouse-server:25.5.2.47-alpine`（revision 54477） | 原生 TCP 9000；bb8 `max_size(1)`；`inner_pool` 关闭 |

`clickhouse/clickhouse-server:24.8.14.39-alpine`（revision 54469）与
clickhouse-arrow 0.2.1 不兼容，见文末“环境发现”。所有容器只发布到
127.0.0.1，runner 退出时删除。

## mysql_async 0.37.1

### 操作与理由

- **MY1（池，会话状态）**：`Pool::start_transaction(TxOpts::default())`，即
  `START TRANSACTION` 往返。它建立服务端事务状态；完成后 `rollback`。
- **MY2（直接持有的 `Conn`，普通请求）**：`SELECT 12345`。直接 `Conn` 是
  `targets.md` 明确列出的第二种复用形态，和池不是同一个目标。

Pending 全部由 50 ms 扣住回复提供，四个层级相同。

### 结果（release，3 轮）

| 操作 | 层级 | 计划/违反（每轮） | 备注 |
|---|---|---|---|
| MY1 | B1 | 6/0 | |
| MY1 | B2 | 6/0 | |
| MY1 | B3 | 6/0 | |
| MY1 | M | 6/0 | pre=0 post=6；M 的 pre/post 观察相等（退化） |
| MY1 | M-residue | 6/0 | residue 每轮 max=0 |
| MY2 | B1 | 2/0 | |
| MY2 | B2 | 2/0 | 重试返回 `Ok(12345)`（见下） |
| MY2 | B3 | 2/0 | 不同查询返回 `Ok([12345])` |
| MY2 | M | **2/2** | 两个计划都违反 |
| MY2 | M-residue | **2/2** | 与 M 同一批计划，residue=58 |

补丁版（`patches/mysql_async/` + `DW_MYSQL_GUARD=1`）3 轮全部为
`plans` 同、`violations=0`；MY1 6/0，MY2 2/0（B1/B2/B3/M/M-residue 都是 0）。

注意：MY1 的 M 0 只说明幸存者没有被污染；它的 pre/post 观察相等（退化），
不能证明被取消的 `BEGIN` 是否真的没有在服务端留下事务。MY2 的 M 不是退化
观察：`own` 的值 12345 与 pre/post 的 67890 不同。

### 违反明细与分诊

违反计划（每轮相同）：

- `#0@1:Immediate`
- `#0@1:AfterWake`

release 的观察：

- Immediate：`own=Ok(12345)`，`visible=Ok(0)`（第 1、2 轮）或 `Ok(1)`
  （第 3 轮），pre/post 都是 `own=Ok(67890)`、`visible=Ok(1)`。
- AfterWake：`own=Ok(12345)`，`visible=Ok(1)`；M-residue 的 `residue=58`。
- 两轮的所有计划里，pre/post 相同，`unknown=0`，`baseline_errors=0`。

B2/B3 的实测返回值：`MY2 B2 retry returned 12345`，
`MY2 B3 different query returned [12345]`（`DW_DIAG=1` 的诊断输出在日志里）。
两者都只判 `Ok`，所以都没有违反。

**结论：确认的问题（1 个，仅 M/M-residue 发现）。契约外行为：无。
测试框架错误：无（最终 3 轮 baseline_errors=0、unsettled=0、unrealized=0）。**

- 机制：mysql_async 的 `Conn::routine` 在命令开始时把连接标记为
  `disconnected = true`，正常结束才清除；查询 future 被丢弃后这个标记留在
  连接上，取消的 `SELECT` 的完整回复也还在流里。下一次查询不检查该标记，
  直接把旧回复当成自己的结果。
- 下一个使用者看到什么：直接持有同一个 `Conn`，取消 `SELECT 12345` 后，
  下一条 `SELECT 67890` **正常返回 `Ok`，但值是 12345**；再下一条写可能成功
  也可能不可见。`Conn::is_disconnected()` 此时为 true。没有跨池借用者：
  `Conn` 只有一个持有者。
- 池路径为什么干净：mysql_async 的 recycler 检查 `inner.disconnected`，
  丢弃这种连接（`src/conn/pool/recycler.rs` 的 `conn_decision`），所以 MY1
  的幸存者拿到的是新连接或干净连接；MY1 没有确认问题。
- 补丁对照：`patches/mysql_async/mysql_async-0.37.1-connection-closed.diff`
  在 `routine` 入口检查 `disconnected`，把静默的旧值变成显式的
  `Connection to the server is closed`；`DW_MYSQL_GUARD=1` 让调用方在
  `is_disconnected()` 为 true 时丢弃并重建 `Conn`，这是正确的使用方式。
  两者一起，MY2 的 3 轮全为 0。单独打补丁（不加 guard）时，B2/B3 会报
  “Connection to the server is closed”，即不再静默返回旧值；这条对照用于
  确认根因，不作最终表的数字。
- 契约：mysql_async 0.37.1 没有取消安全的文档说明；`Conn::is_disconnected`
  是公开方法。这里按“调用方丢弃 future 后仍复用连接”作为使用契约来判断，
  因此归为确认的问题，而不是契约外行为。它是单持有者复用，测试中没有把
  同一个 `Conn` 交给另一个任务，所以没有按“跨使用者读到别的请求数据”的
  安全条目停下。

### “M1 抓到、B3 没抓到”的计划

- `#0@1:Immediate`：B3 的 `SELECT 67890` 读到被取消的 `SELECT 12345` 的
  成功结果包，照样返回 `Ok`（值是 12345）；B3 只判 `Ok`，值不检查。
- `#0@1:AfterWake`：同上。B2 的重试也返回 `Ok(12345)`，所以 B2 同样看不到。
- M1-residue 的这两个计划也是这个原因，`residue=58` 只是额外证据；M 的
  `own` 值已经足以抓到，不需要残留字节。

## mongodb 3.9.1

### 操作与理由

- **MG1（池，会话状态）**：`client.start_session()` → `start_transaction()` →
  `collection.insert_one(...).session(&mut session)`。被取消的是整段事务
  操作，真正建立服务端事务状态的是第一条带 `startTransaction: true` 的
  `insert`；完成路径会 `abort_transaction()`（不提交，保持 pre/post 可比）。
- **MG2（池，普通请求）**：通过池 `find_one({_id:"known"})`，返回 12345。
  幸存者读的是另一个文档 `_id:"own"`（67890），所以旧回复如果被消费会
  在 `Obs.own` 里显示成 12345。

服务端是单节点副本集（事务需要）。池 `maxPoolSize=1&minPoolSize=0`，
`directConnection=true`；观察者直连，池走 50 ms 扣住回复的 Proxy。

### 结果（release，3 轮）

| 操作 | 层级 | 计划/违反（轮 1 / 2 / 3） | 备注 |
|---|---|---|---|
| MG1 | B1 | 12/0 / 12/0 / 12/0 | |
| MG1 | B2 | **12/5 / 12/6 / 12/6** | 重试撞上被放弃的事务 |
| MG1 | B3 | 12/0 / 12/0 / 12/0 | 不同的 `_id`，不冲突 |
| MG1 | M | 12/0 / 12/0 / 12/0 | pre/post 相等（退化），residue 0 |
| MG1 | M-residue | 12/0 / 12/0 / 12/0 | residue max=0 |
| MG2 | B1 | 6/0 / 6/0 / 6/0 | |
| MG2 | B2 | 6/0 / 6/0 / 6/0 | |
| MG2 | B3 | 6/0 / 6/0 / 6/0 | |
| MG2 | M | 6/0 / 6/0 / 6/0 | pre/post 相等（退化） |
| MG2 | M-residue | 6/0 / 6/0 / 6/0 | residue max=0 |

补丁版（`patches/mongodb/mongodb-3.9.1-cancelled-transaction-abort.diff`）
3 轮全部 0：MG1 12/0（B1–M-residue），MG2 6/0。

注意：MG1/MG2 的 M 0 是退化观察（pre/post 相等），只能证明幸存者的
写入/读取没有被污染，不能证明事务/读取是否真的发生或回滚。

### 违反明细与分诊

MG1 B2 的违反计划是 `#0@2:Immediate`、`#0@2:AfterWake`、`#0@3:...`、
`#0@4:...`（轮 1 是 5 个：`#0@4:AfterWake` 未违反；轮 2、3 是 6 个）。
所有违反都是同一错误：

```
Error code 112 (WriteConflict): ... WiredTigerIdIndex::_insert ...
labels: {"TransientTransactionError"}
```

重试的是同一个函数、同一个 `_id: "mg1"`、同一个值。被取消的事务已经把一个
未提交的 `mg1` 文档写进服务端事务；`ClientSession` 被丢弃时本应
`abortTransaction`，但事务状态还停在 `Starting`，`Drop` 不进入 abort 分支，
于是服务端事务一直开着，重试插入同一个 `_id` 就冲突。

**结论：确认的问题（1 个，仅 B2 发现）。契约外行为：无。
测试框架错误：无（早期被 `dropDatabase` 锁住的运行已作为 harness 错误排除，
见 AMENDMENTS #14；最终 3 轮 baseline_errors=0、unsettled=0、unrealized=0）。**

- 契约：`mongodb` 的事务 API 文档明确写着未提交/未回滚的事务会在
  `ClientSession` 的 `Drop` 里自动回滚（`action/transaction.rs` 的
  `abort_transaction` 文档）。`Starting` 状态跳过这个分支，违反该契约。
- 下一个使用者看到什么：取消第一个 in-transaction 命令后，立即用相同参数
  重试，得到 `WriteConflict`（带 `TransientTransactionError` 标签），直到被
  放弃的事务超时/被回滚；期间它还持有文档锁。错误是**可见的**，不是静默
  错误值，但重试路径确实被破坏。
- 为什么 B1/B3/M 没抓到：B1 只看取消本身；B3 写的是 `_id: "mg1-other"`，
  不与被放弃事务的 `mg1` 冲突；M 的幸存者写的是 `survivor`/读 `own`，也不
  冲突。只有 B2 用完全相同的参数，才撞上同一个 `_id`。
- 补丁对照：4 部分补丁（`Starting` 当作 `InProgress`；登记并等待 Drop 时
  的 abort；abort 完成才标 `Aborted`；把等待放在 `start_session` 而不是
  `ClientSession::new`）。3 轮 B2 12/0、M 12/0。**注意：**最初的 3 部分
  版本把等待放在 `ClientSession::new`，会递归阻塞 abort 任务自己的 session
  创建，导致 M 的 12 个计划全部 20 s liveness 失败；该尝试的日志在
  `logs/superseded/mongodb/`，最终 runner 用的是修好的 4 部分版本。见
  AMENDMENTS #15。

### “M1 抓到、B3 没抓到”的计划

**没有。** 本次是反过来的：B2 抓到、M1 和 B3 都没抓到。原因是 M1/B3
故意使用不同的文档/不同的操作，不会与被放弃事务的 `_id` 冲突。MG1 的 M
观察还是退化的（pre/post 相等），只能看污染，不能看事务是否发生。

## clickhouse-arrow 0.2.1

### 操作与理由

- **CH1（bb8 池，会话状态）**：`execute("USE dw_p3_ch_<pid>_<n>")`。0.2.1
  没有事务 API；`USE` 是每连接的会话状态，建立它需要一次往返。幸存者用
  不带库名的 `INSERT INTO t SELECT 1`，只有连接仍在目标库里才成功；观察者
  数 `db.t`。
- **CH2（bb8 池，普通请求）**：`query_rows("SELECT 12345")` 并取第一行。
  幸存者读 67890（旧回复若被消费会在 `Obs.own` 里显示 12345），再写一行，
  观察者数行。

`inner_pool` 关闭（AMENDMENTS #1），池 `max_size(1)`，`.with_check()` 不用
（bb8 的本地状态检查已经足够，避免额外的 ping 往返）；观察者是直连的
独立 `ArrowClient`。Pending 同样是 50 ms 扣住回复。

### 结果（release，3 轮）

| 操作 | 层级 | 计划/违反（每轮） | M 判定 |
|---|---|---|---|
| CH1 | B1 | 4/0 | — |
| CH1 | B2 | 4/0 | — |
| CH1 | B3 | 4/0 | — |
| CH1 | M | 4/0 | pre=0 post=4 |
| CH1 | M-residue | 4/0 | pre=0 post=4；residue max=0 |
| CH2 | B1 | 6/0 | — |
| CH2 | B2 | 6/0（第 2 轮 `unrealized=1`，其余 0） | — |
| CH2 | B3 | 6/0 | — |
| CH2 | M | 6/0 | pre=0 post=6 |
| CH2 | M-residue | 6/0 | pre=0 post=6；residue max=0 |

没有 `LADDER-VIOLATION`。第 2 轮 CH2/B2 的 `unrealized=1` 表示有一个计划的
Pending 边界没有到达，该计划测试的内容少一个；不是违反，也不是通过。

### 分诊

- **确认的问题：无。**
- **契约外行为：无。**
- **测试框架错误：无。**
- CH1 的 M 全部是 post：被取消的 `USE` 仍在服务端生效，幸存者的不带库名
  写入成功。`Expect::Atomic` 允许 post，所以不是违反；这也说明这个场景
  不能区分“取消的 USE 是否执行”，只能发现残留回复/错误状态造成的污染。
- CH2 的 M 是退化的（pre/post 都是 `own=67890, visible=1, residue=0`，
  `LADDER-NOTE` 有记录），只能通过 `own` 发现串话；3 轮没有串话。
- M-residue 的 3 轮 `residue` 全为 0；这个目标没有残留字节，也没有额外的
  Q3 发现。

### “M1 抓到、B3 没抓到”的计划

没有，因为 M1 没有抓到任何计划。

## 环境发现：clickhouse-arrow 0.2.1 不能对 ClickHouse 24.8.14.39

这不是取消问题，不计入 Q1，但会挡住“冻结目标能否运行”：

- 24.8.14.39 的 hello revision 是 54469，低于
  `DBMS_MIN_PROTOCOL_VERSION_WITH_CHUNKED_PACKETS = 54470`。
  clickhouse-arrow 0.2.1 的 `src/client/reader.rs:229` 在 revision < 54470
  时硬编码 `(ChunkedProtocolMode::default(), ChunkedProtocolMode::default())`，
  即 `ChunkedOptional`，而 `supports_chunked_send/recv()` 把
  `ChunkedOptional` 当成分块；客户端于是向不认识分块包的服务器发分块包。
  服务端日志：`Unknown packet 52 from client (UNKNOWN_PACKET_FROM_CLIENT)`；
  客户端 DDL 全部 3 s 超时。显式设置 `NotChunked` 也不生效（走 else 分支）。
- 一行补丁把 pre-54470 的回退改成 `(NotChunked, NotChunked)` 后，24.8 上的
  `ch1_b1` 得到 `plans=4 violations=0 baseline_errors=0 exhaustive=true`，
  服务端日志不再有未知包。补丁和原始日志在
  `logs/clickhouse/incompatible-24.8-chunked-fallback.{patch,log,server.log}`。
- 因此报告的主结果使用官方 `25.5.2.47-alpine`（revision 54477，crate 自己
  README/测试使用的版本）。这是 AMENDMENTS #7；24.8 配对不是“未完成”，
  而是被客户端 bug 证明不可用。

## 总表：目标 × 层级 × 版本

“计划/违反”，每格是 3 轮的实际值；`0` 表示三轮都是 0。计划数稳定时只写一个数。

| 目标 | 操作 | 版本 | B1 | B2 | B3 | M | M-residue |
|---|---|---|---|---|---|---|---|
| mysql_async | MY1 池/BEGIN | release | 6/0 | 6/0 | 6/0 | 6/0 | 6/0 |
| mysql_async | MY1 池/BEGIN | patched | 6/0 | 6/0 | 6/0 | 6/0 | 6/0 |
| mysql_async | MY2 直接 Conn/SELECT | release | 2/0 | 2/0 | 2/0 | **2/2** | **2/2** |
| mysql_async | MY2 直接 Conn/SELECT | patched | 2/0 | 2/0 | 2/0 | 2/0 | 2/0 |
| mongodb | MG1 事务第一命令 | release | 12/0 | **12/5, 12/6, 12/6** | 12/0 | 12/0 | 12/0 |
| mongodb | MG1 事务第一命令 | patched | 12/0 | 12/0 | 12/0 | 12/0 | 12/0 |
| mongodb | MG2 普通读 | release | 6/0 | 6/0 | 6/0 | 6/0 | 6/0 |
| mongodb | MG2 普通读 | patched | 6/0 | 6/0 | 6/0 | 6/0 | 6/0 |
| clickhouse-arrow | CH1 `USE` | release | 4/0 | 4/0 | 4/0 | 4/0 | 4/0 |
| clickhouse-arrow | CH2 普通读 | release | 6/0 | 6/0* | 6/0 | 6/0 | 6/0 |

\* CH2/B2 第 2 轮 `unrealized=1`；违反仍是 0。clickhouse-arrow 没有补丁版：
没有确认问题，无可对照的补丁。

## Q1：各层级多发现了多少确认的问题

本次三个目标上共 2 个确认的问题，分别只被一个层级发现：

| 层级 | 确认的问题 | 具体 |
|---|---|---|
| B1 | 0 | 只判取消本身 |
| B2 | 1 | mongodb MG1：同参数重试撞上未回滚的事务 |
| B3 | 0 | 不同操作/不同 `_id`，避开了冲突 |
| M1 | 1 | mysql_async MY2：下一条查询静默读到被取消查询的 12345 |
| M1-residue | 1（与 M1 同一批计划，不额外计） | 额外给出 residue=58 的证据 |

所以在这三个目标上 **M1 不是 B1–B3 的超集**：M1 比 B1/B3 多发现 1 个
（mysql MY2），但 B2 也发现了 1 个 M1 没发现的（mongodb MG1）。M1 与 B2
各发现一个不同的问题，净数相同；M1 比 B3 多一个计划组（2 个计划）。
样本只有三个客户端，不能外推。

### “M1 抓到、B3 没抓到”的逐条原因

1. mysql_async MY2 `#0@1:Immediate`：B3 的 `SELECT 67890` 把被取消的
   `SELECT 12345` 的成功结果包读成自己的结果，返回 `Ok`，而 B3 不检查值。
2. mysql_async MY2 `#0@1:AfterWake`：同上。

mongodb 与 clickhouse-arrow 没有“M1 抓到、B3 没抓到”的计划。

## Q2：成本

| 目标 | 开始–结束（JST） | 场景代码行数 B1/B2/B3/M/M-res | 时间大致比例（环境/制造 Pending/观察者/判定） |
|---|---|---|---|
| mysql_async | 02:50–03:39（含并行等待） | 22 / 49 / 60 / 26 / 39 | 25% / 10% / 35% / 30% |
| mongodb | 02:55–03:38（含并行等待） | 15 / 22 / 28 / 16 / 29 | 45% / 5% / 20% / 30% |
| clickhouse-arrow | 02:55–03:15（约 20 min） | 19 / 22 / 28 / 19 / 36 | 55% / 5% / 25% / 15% |

行数口径：非空、首个非空白字符不是 `/`；计层级函数（闭包 + `explore`/
`explore_observed` 包装）和该层级的两个 `#[test]`，不计 `ladder::*`/note
打印、`Env`/`setup`/`survive`/`config` 以及共享的 target 构造函数。
M 与 M-residue 共享骨架：M 不计残留分支，M-residue 计残留分支。两个操作的
层级函数是共享的，所以这些数字不是 MY1 与 MY2（或 MG1/MG2）各自的行数。

时间比例是人工估计；“环境”包含 Docker/副本集/端口/用户名、以及
mongodb 那轮被 `dropDatabase` 锁住的排查（见 AMENDMENTS #14）。

## Q3：残留字节

- 三个目标都能用 `Proxy`（都是本地 TCP 回环），M1-residue 都跑了 3 轮。
- 只有 mysql_async MY2 的取消计划有残留：58 字节，对应 `own=12345` 的同一批
  计划；M 本身已经用 `own` 抓到，所以 Q3 没有额外的确认问题。
- mysql_async MY1、mongodb（MG1/MG2）、clickhouse-arrow（CH1/CH2）的
  residue 全部为 0。
- 结论：残留检查在这三个目标上可用；它确认了 mysql 的机制（旧回复仍在
  客户端缓冲区），但没有带来 M 之外的新问题。

## 安全条目

没有发现需要按“跨使用者读到别的请求的数据”停下并只写入
`PRIVATE-NOTES.md` 的问题；本次没有创建 `PRIVATE-NOTES.md`。
mysql_async MY2 的旧值返回发生在**同一个持有者**对同一个 `Conn` 的连续
调用之间；测试没有把该 `Conn` 交给另一个任务，也没有测试跨任务移交，
因此按正确性问题记录，没有把它当作已确认的安全问题。

## 未完成、未运行与偏离

- 三个目标都完成了 3 轮完整阶梯；没有“未完成”的目标。
- clickhouse-arrow 的严格 24.8.14.39 配对无法运行（客户端分块协议 bug，
  已用一行补丁对照确认根因）；主结果在 25.5.2.47 上。见 AMENDMENTS #7。
- clickhouse-arrow 没有补丁版跑数：零违反，没有可对照的修复。`run-db.sh`
  的 `VARIANT=patched` 对 clickhouse 只是重复 release（没有
  `patches/clickhouse/`），这一点在脚本注释里说明。
- mongodb 的第一次补丁（3 部分）修好了 B2，但让 M 全部 liveness 失败；
  最终改用 4 部分补丁。两次运行都保留在
  `logs/superseded/mongodb/` 和 `logs/mongodb/patched-*.log`，见 AMENDMENTS #15。
- 与 PLAN 的其他偏离：#1（clickhouse `inner_pool` 关闭）、#2（mongodb 单节点
  副本集）、#3（统一用 Proxy 扣住回复）、#4（mysql 补丁 + guard）、#5
  （residue 前等 100 ms）、#6（镜像标签）、#14（mongodb 不做每轮
  `dropDatabase`，每轮新容器）。

## 原始日志索引

- mysql：`db-clients/logs/mysql_async/release-1..3.log`、`patched-1..3.log`
- mongodb：`db-clients/logs/mongodb/release-1..3.log`、`patched-1..3.log`
- clickhouse：`db-clients/logs/clickhouse/release-1..3.log`；24.8 兼容性证据
  在 `incompatible-24.8-*`
- 被取代/污染的运行：`db-clients/logs/superseded/`
