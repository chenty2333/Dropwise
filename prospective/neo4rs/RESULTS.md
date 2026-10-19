# neo4rs 0.8.0 动态验证结果

> 2026-10-19。实验代码 `prospective/neo4rs/`，运行入口
> `bash prospective/neo4rs/run-neo4rs.sh`（`REPS`、`VARIANT=release|patched`）。
> 原始输出现存于 `logs/`，每个最终数字都来自那里的 `release-1..3.log` /
> `patched-1..3.log`。触发点的手工时序证据在 `logs/probe-60ms.log`、
> `logs/probe-120ms.log`。
>
> 结论：**配对扫描的静态可疑成立。** 确认一个问题（同一根因，N1/N2 两个操作
> 都可见）；只有 M 抓到，B1/B2/B3 都没抓到。没有观察到跨借用者的记录内容
> 泄露。上游已有同一问题的公开报告（#295 / #298，已合并到 main，未发布），
> 见第 6 节。

## 1. 版本与环境

### 1.1 版本

`crates.io` 在 2026-10-19 的元数据：最新版本 `0.9.0-rc.10`（预发布），
**最新正式版本 `0.8.0`**（`max_stable_version`、`default_version` 都是
0.8.0），`updated_at = 2026-06-11`。按任务规则不使用 rc，因此测 **neo4rs
0.8.0**。

- `Cargo.lock` 解析到 `neo4rs 0.8.0`、`deadpool 0.9.5`、`tokio 1.53.3`；
  构建用 Rust 1.98.0。
- 也读了 `0.9.0-rc.10` 的源码作对照：它把源码移到 `lib/`，`Connection` 新增
  `recv_timeout`/`hints`，`recycle` 给 `reset()` 加了 5 s 超时，但 `reset()`
  仍只发一个 RESET、只读一条回复并接受任意 `SUCCESS`，仍没有“交换进行中/
  已取消”标记。**rc.10 未运行**（规则排除预发布版），这一条只是源码阅读，
  不是动态结果。
- 上游修复（#298）在 2026-09-25 合并到 main，晚于 rc.10，也没有进任何正式
  发布版。因此 0.8.0 与 rc.10 都缺少该修复；只有 main 有。

### 1.2 服务端

| 项目 | 值 |
|---|---|
| 镜像 | `neo4j:5.26.0-community` |
| digest | `neo4j@sha256:5a015e53de1895e7eee1574ae0325cf8c4b89587222778108c594bdd45a474b5`（manifest list；`docker image inspect` 的 `RepoDigests[0]`） |
| 内核 | Neo4j Kernel **5.26.0**，edition **community**（`CALL dbms.components()` 实际输出） |
| 协议 | 明文 `bolt://127.0.0.1:<随机端口>`，容器只发布到 127.0.0.1 |
| 鉴权 | `NEO4J_AUTH=neo4j/dropwise` |

runner 每次运行启动一个一次性容器，等待 `cypher-shell 'RETURN 1'` 通过后把
`DW_NEO4J_URL` 传给测试，退出时 `docker rm -f` 删除容器；与
`prospective/phase3/run-db.sh` 的做法一致。最终 6 次整轮运行后
`docker ps -a` 没有遗留的 neo4j 容器。

## 2. 场景

### 2.1 目标操作

| 目标 | 调用 | 说明 |
|---|---|---|
| **N1** | `Graph::run("MATCH (k:DwK {id: $id}) SET k.v = $v")` | 池化写。0.8.0 的 `Graph::run` 走 `Query::run_retryable`：RUN → 读 SUCCESS → DISCARD → 读 SUCCESS。节点由 setup 用独立 Graph 创建，初值 0；目标写 `v = 1`。 |
| **N2** | `Graph::execute("UNWIND [1, 2] AS i RETURN $tag + ':' + toString(i) AS t ORDER BY t")`，逐个 `next()` 读完 | 池化读。`fetch_size = 1`，因此有两轮 PULL；tag 每次运行唯一（`secret-<pid>-<n>`），使“读到了上一次交换的数据”可以直接辨认。 |

`ConfigBuilder::max_connections(1)`：池大小 1，保证幸存者拿到的就是被取消
操作用过的那条连接（如果它没被 deadpool 丢弃）。

### 2.2 四个层级 + M1-residue

- **B1**：setup + 取消目标，之后停止。没有 observer 清理（原因见 4.4）。
- **B2**：取消后，通过同一个池用相同参数再做一次同一个操作；只判 `Ok`。
- **B3**：取消后，通过同一个池做不同的同类操作（N1 写 `v = 2`；N2 读
  `<tag>-other`）；只判 `Ok`。
- **M1**：`explore_observed(Expect::Atomic)`。幸存者通过同一个池对同一个节点
  写 `v = 7`，然后：
  1. `own`：同一个池读 `MATCH (k) RETURN k.v`，应为 7；
  2. `own_tag`：同一个池读 `RETURN '<tag>-survivor:1/2'`，应为两行。这个读
     与 N2 的被取消读有相同字段 `t`，如果上一条 RECORD 被错当成幸存者的行，
     这里会直接返回上一次的 tag（规范化后为 `:1`/`:2`）；
  3. `visible`：**独立 Graph**（不经过代理、不经过被测池）读同一个节点的 `k.v`，
     应为 7；
  4. `write`：幸存者写的返回值。

  观察值是 `{write, own, own_tag, visible, residue}`。`own_tag` 在比较前会剥掉
  本次运行的 tag 前缀，保证 pre/post 校准可比较。`residue` 只对 M1-residue
  是 `Some`。
- **M1-residue**：与 M1 相同，但在取消后真实 `sleep(100 ms)`，再取
  `Proxy::residue()` 的最大未读字节数；其余观察相同。

四个层级共用同一个 `Config`：`Flavor::MultiThread { workers: 1 }`、
`races = [Immediate, AfterWake]`、`max_cancellations = 1`、`max_runs = 64`、
`scenario_timeout = 20 s`、`Settle { tokio_time: 0, ..default }`。每项 3 轮。

### 2.3 Pending 手段

全部用 `dropwise::net::Proxy` 的 `ProxyOptions { reply_delay: 50 ms }` 扣住
每一个 server→client chunk。没有使用服务端延迟；Neo4j community 默认没有
APOC，也没有用 `apoc.sleep` 一类的假设。

`N1` 只有 3 个 Pending 边界（`#0@1..3`）：@1 是 `pool.get()` 里 deadpool 的
recycle RESET 回复，@2 是 RUN 的回复，@3 是 DISCARD 的回复。`N2` 有 4 个
（`#0@1..4`）：@1 recycle，@2 RUN 回复，@3/@4 两轮 PULL 的回复。

## 3. 结果总表

每轮的数字都相同，所以表中用 `计划/违反（R1, R2, R3）` 表示。所有行的
`unrealized=0`、`unsettled=0`、`baseline_errors=0`、`exhaustive=true`。
计划数是 `Report::trials.len()`，违反数是 `report.violations().count()`。

### 3.1 release

| 操作 | 层级 | 计划/违反（R1, R2, R3） | 分诊结论 |
|---|---|---|---:|
| N1 | B1 | 6/0, 6/0, 6/0 | 通过（无 panic/timeout/obligation 泄漏） |
| N1 | B2 | 6/0, 6/0, 6/0 | 通过（重试返回 Ok） |
| N1 | B3 | 6/0, 6/0, 6/0 | 通过（不同写返回 Ok） |
| N1 | M | **6/4, 6/4, 6/4** | **确认的问题**（4 个计划：@2/@3 × Immediate/AfterWake） |
| N1 | M-residue | **6/4, 6/4, 6/4** | 同一确认问题的 4 个计划，均有 `own: Err` 与 residue 24/100；没有额外计划 |
| N2 | B1 | 8/0, 8/0, 8/0 | 通过 |
| N2 | B2 | 8/0, 8/0, 8/0 | 通过（但 2/8 个重试返回 `Ok([])`，见 3.3） |
| N2 | B3 | 8/0, 8/0, 8/0 | 通过（但 2/8 个不同读返回 `Ok([])`，见 3.3） |
| N2 | M | **8/2, 8/2, 8/2** | **确认的问题**（2 个计划：@2 × Immediate/AfterWake） |
| N2 | M-residue | **8/6, 8/6, 8/6** | 2 个是同一确认问题（@2，`own: Err`，residue 26）；4 个是 residue-only（@3/@4，`own: Ok(7)`，residue 45/105）→ 不计入确认问题 |

M 的 pre/post 逐轮一致：N1 `pre=0 post=2`、N2 `pre=0 post=6`，`unknown=0`。
pre 和 post 的观察相等（`{write: Ok, own: Ok(7), own_tag: Ok(["-survivor:1",
"-survivor:2"]), visible: Ok(7)}`），所以 M 只能说明“共享状态被污染”，不能
区分被取消的写到底有没有提交；这是 `Expect::Atomic` 下的退化观察，不影响
违反判定（非 pre 非 post 即 `Neither`）。

**分诊汇总：** 确认的问题 **1 个**（同一根因，N1/N2 都可见；M 每轮分别
4/6、2/8）。契约外行为 0 个；测试框架错误 0 个（最终 3 轮的
`baseline_errors=0`；开发早期 B1 清理阻塞的问题已在最终场景中移除，见 4.4）。
M1-residue 中 4 个 residue-only 计划按 PLAN 第 3 节单独统计，没有用户可见
后果，不计入确认问题。

### 3.2 patched（补丁对照，见第 5 节）

| 操作 | 层级 | 计划/违反（R1, R2, R3） | 分诊结论 |
|---|---|---|---:|
| N1 | B1 | 6/0, 6/0, 6/0 | 通过 |
| N1 | B2 | 6/0, 6/0, 6/0 | 通过 |
| N1 | B3 | 6/0, 6/0, 6/0 | 通过 |
| N1 | M | **6/0, 6/0, 6/0** | 违反消失 |
| N1 | M-residue | 6/4, 6/4, 6/4 | 全部 residue-only（`own: Ok(7)`、`visible: Ok(7)`）；补丁丢弃了连接，无用户可见后果 |
| N2 | B1 | 8/0, 8/0, 8/0 | 通过 |
| N2 | B2 | 8/0, 8/0, 8/0 | 通过；0 个空结果重试 |
| N2 | B3 | 8/0, 8/0, 8/0 | 通过；0 个空结果读 |
| N2 | M | **8/0, 8/0, 8/0** | 违反消失 |
| N2 | M-residue | 8/6, 8/6, 8/6 | 全部 residue-only（`own: Ok(7)`、`visible: Ok(7)`）；无用户可见后果 |

补丁前后：**N1 M 4/6 → 0/6；N2 M 2/8 → 0/8；N2 的 B2/B3 空结果 2/8 → 0/8。**
补丁后的 M-residue 仍报残留字节，因为 `residue` 是在幸存者开始前测的，此时
被取消连接的未读字节还在它的 socket 里；补丁只是让下一次 recycle 丢弃这条
连接，幸存者拿到干净连接，所以 `own/own_tag/visible` 都正确。这符合 PLAN
对 M1-residue 的定位：单独统计，不计入确认问题。

### 3.3 低层级实际看到了什么

- **N1 B2/B3**：取消后重试/换写都返回 `Ok`，没有观察到错误或挂起。B2/B3
  只判 `Ok`，不读值、不查观察者，因此看不到任何东西。
- **N2 B2/B3**：8 个计划中有 **2 个**（@2:Immediate、@2:AfterWake，每轮相同）
  的重试/不同读返回 **`Ok([])`**：查询本应返回两行，实际一行都没有。另外 6 个
  计划返回正确的两行（或换 tag 后的两行）。`Ok([])` 仍是 `Ok`，所以 B2/B3
  按规则不违反。这是低层级“看不见”的直接证据。
- **B1**：全部计划正常结束；没有 panic、没有 obligation 泄漏、没有 20 s
  超时（见 4.4 的额外观察）。
- **M1**：每轮共 14 个计划（N1 6 + N2 8），其中 6 个计划违反（N1 4 + N2 2，
  三轮相同）。后继者的实际观察统一为：
  - `write: Ok(())`；
  - `own: Err("value query returned no row")`（驱动层是 `Graph::execute`
    成功返回、但流里没有行；我们的读取辅助函数把它变成这个错误串）；
  - `own_tag: Ok(["-survivor:1", "-survivor:2"])`（下一条同池读恢复正常）；
  - `visible: Ok(7)`（独立观察者看到幸存者的写确实生效）。

  也就是说：**写成功了、独立观察者也看到了，但紧接着通过同一个池的读静默
  地丢掉了它自己的行。** 没有挂起，没有错误返回给写；它读到的是上一次交换的
  `SUCCESS` 控制回复（这造成错位），但没有读到上一次查询的 `RECORD` 内容或
  tag，因此没有观察到跨借用者的数据泄露。

## 4. 机制与触发点

### 4.1 静态假设的核对

0.8.0 的事实（源码）：

- `ConnectionManager::recycle` 只做 `Ok(conn.reset().await?)`
  （`src/pool.rs:53`）；`reset()` 发一个 RESET、读一条回复，`SUCCESS` 就接受
  （`src/connection.rs:80-85`）。
- `Connection` 只有 `version`、`stream` 两个字段
  （`src/connection.rs:29-32`），没有“交换进行中”标记，也没有取消时置坏的
  Drop guard。
- deadpool 0.9.5 的 `Pool::get()` 在把对象交给调用者**之前**调用
  `manager.recycle`（`deadpool-0.9.5/src/managed/mod.rs:390-410`）；被取消的
  用户操作返回的对象先回到槽位，下一次 `get()` 才发 RESET。这正好给出“残留
  回复被 recycle 读走”的窗口。

动态结果与静态假设的对应：

| 取消暂停在 | 残留 | recycle 的 reset() | 后果 | 实测 |
|---|---|---|---|---|
| `pool.get()` 的 recycle RESET 回复（@1） | 无（deadpool 的 `UnreadyObject` 在 get future 被丢弃时 detach 该连接） | 不执行 | 安全：下一次 get 新建连接 | N1/N2 的 @1 计划全部干净（M、M-residue 都不违反） |
| RUN 回复未读（@2） | 一条 `SUCCESS`（N1 的 RUN SUCCESS；N2 的 RUN SUCCESS） | 读到它，当成 RESET 的 SUCCESS → 接受 | 错位：RESET 自己的 SUCCESS 留在流里；下一次操作读到上一条回复 | N1 `own: Err`、N2 `own: Err`；residue 24/26；B2/B3 `Ok([])` |
| DISCARD 回复未读（N1 @3） | 一条 `SUCCESS`（DISCARD 的） | 读到它 → 接受 | 同上；写已提交，观察者看到 7 | N1 `own: Err`、`visible: Ok(7)`；residue 100 |
| PULL 回复未读（N2 @3/@4） | 以 `RECORD` 开头（后跟 SUCCESS） | 读到 `RECORD` → `unexpected` → `Err` | deadpool 丢弃连接；下一次 get 新建连接 | M 干净；M-residue 只有 residue（45/105），`own/visible` 都正确 |

结论：**静态假设成立，但要按残留消息的“第一条”分支来说**。只有残留的第一条
是 `SUCCESS` 时，`reset()` 才会误接受并让错位连接回池；残留以 `RECORD` 开头
时是安全的（连接被丢弃）；取消发生在 deadpool 的 recycle 窗口时也是安全的
（连接被 detach）。不是“任何取消都错位”。

### 4.2 触发需要停在哪个挂起点

- 必须在 `pool.get()` **已经完成之后**再取消（否则连接在 `UnreadyObject` 里被
  detach，不会回池）。
- 必须在某个请求已经发出、其**终止回复**（`SUCCESS`）已经产生/到达但还没被
  客户端读走时取消；下一次 `get()` 的 RESET 会把这个 `SUCCESS` 当成自己的
  确认。N1 的 RUN 回复（@2）和 DISCARD 回复（@3）都能触发；N2 的 RUN 回复
  （@2）能触发。
- 如果取消时 PULL 的回复还没被读，残留以 `RECORD` 开头，`reset()` 返回错误，
  deadpool 丢弃连接，安全（N2 @3/@4）。
- N1 的 @2 与 @3 还有一个服务端区别：@2 时 auto-commit 事务还没被 DISCARD
  消费，`SET` 未提交（probe 里 `v=0`）；@3 时 DISCARD 已发出，`SET` 已提交
  （probe 里 `v=1`）。两种都在 `Expect::Atomic` 的允许集合内（pre 或 post），
  但后继者的读都被污染。

### 4.3 手工时序证据

`tests/probe.rs` 不是 ladder 的一部分，只用于确认“取消点 → 残留字节 →
后继者读到什么”的对应关系。它用 `tokio::time::timeout` 在指定墙钟时刻取消
一个池化写，等待 200 ms，打印 `Proxy::residue()`、独立观察者读到的 `v`，再通过
同一个池执行 `RETURN 'fresh' AS tag`：

| probe | 取消时刻 | residue | 取消后独立观察者 `v` | 后继者 `RETURN 'fresh'` | 连接数 |
|---|---:|---:|---:|---|---|
| `logs/probe-60ms.log` | 61.7 ms | 24 B | 0（RUN 回复未读，写未提交） | `<none>`（应为 `fresh`） | 1（同一个连接） |
| `logs/probe-120ms.log` | 121.1 ms | 96 B | 1（DISCARD 已发出，写已提交） | `<none>`（应为 `fresh`） | 1（同一个连接） |

两次的后继者都复用了同一个 TCP 连接（`Proxy::connections()` 只有一条，且
`to_upstream/to_client` 继续增长），并且都静默地返回空结果。这与 4.1 的
“`SUCCESS` 残留 → reset 误接受 → 错位”一致。

### 4.4 额外观察（不计入）

B1 最初在取消后做 observer 清理（`DETACH DELETE` 同一个节点），在 N1 的
@2 计划上出现 `scenario did not finish within 20s` 的 liveness 违反。原因是
被取消的写停在 RUN 之后、DISCARD 之前，服务端留下一个未结束的 auto-commit
事务并持有节点写锁；独立连接的 DELETE 阻塞在这把锁上。这个超时是**场景的
清理步骤**造成的，不是驱动挂起，也不属于 B1 要判的“取消本身”。因此最终
B1 不做清理（连接关闭时事务回滚），改为在 `explore` 返回后按 id 前缀清理。
这条只记为观察：**被取消的写会一直持有服务端锁，直到下一次 borrow 触发
RESET 或连接关闭**；不计入本轮的确认问题。

## 5. 补丁对照

补丁在 `patches/neo4rs/neo4rs-0.8.0-cancelled-exchange.diff`，只应用于
`$DROPWISE_NEO4RS_WORK` 下的 neo4rs 0.8.0 副本，通过
`[patch.crates-io]` 让场景副本使用它；没有改 `src/` 或检测阶段的 neo4rs。

做法（“取消时置坏的标记”版本）：

- `Connection` 增加 `dirty: bool`；`begin_exchange()` 置 true，
  `finish_exchange()` 置 false，`is_dirty()` 读取。
- `Query::run_retryable` / `Query::execute_retryable` 在开始交换前
  `begin_exchange()`；`run_retryable` 只在完整成功（RUN + DISCARD 都读完）后
  `finish_exchange()`；`execute_retryable` 保持 dirty，直到
  `DetachedRowStream::next()` 读到流的终止 summary（`RowStream::is_complete()`）
  才清除。
- `ConnectionManager::recycle` 在发 RESET 之前检查 `is_dirty()`；dirty 就返回
  `Error::UnfinishedExchange`，deadpool 丢弃该连接，不再把残留 SUCCESS 当成
  RESET 的回复。

结果（3 轮 × 两个操作）：

| 观察 | release | patched |
|---|---:|---:|
| N1 M 违反 | 4/6 每轮 | **0/6 每轮** |
| N2 M 违反 | 2/8 每轮 | **0/8 每轮** |
| N2 B2/B3 返回 `Ok([])` | 2/8 每轮 | **0/8 每轮** |
| M-residue 中 `own: Err` | N1 4 + N2 2 每轮 | **0** |
| M-residue 中只有 residue | — | N1 4/6、N2 6/8 每轮（`own/visible` 正确） |

补丁用“丢弃被取消交换的连接”消除了 M 的违反；它没有改变服务端已经发生的
写（N1 的 `visible: Ok(7)` 仍在），只是阻止错位连接被下一个借用者使用。按
PLAN 第 4 节，这满足“确认的问题”：能说明后继者看到了什么，并且有一个能消除
它的补丁对照。补丁是保守的（任何未干净完成的交换都丢弃连接），它的目的只是
确认根因，不是建议上游采用的具体实现；上游 #298 的 `pending_responses` /
`is_reusable` 方案更精细。

## 6. 已知报告检索

只读检索，没有在 GitHub、crates.io 或任何地方发 issue/PR/评论，也没有联系
维护者。检索时间 2026-10-19，主要用 GitHub Search API 和 raw 文件。搜索词为
`repo:neo4j-labs/neo4rs reset`、`cancel`、`desync`、`pool reuse`（分别命中
6/2/2/5 条）；与本问题直接对应的是 #295 和 #298，#293/#300/#301 是相关背景。

| 来源 | 内容 | 与本次的关系 |
|---|---|---|
| [neo4rs#295](https://github.com/neo4j-labs/neo4rs/issues/295)（2026-09-22 开，2026-09-25 关，ChrisRega） | “Cancelled or timed-out exchanges can leave pooled Bolt connections desynchronized.” 描述 0.9.0-rc.10/main `19f244a`：取消或接收超时把连接放回池，recycle 的 RESET 可能读到旧的 SUCCESS，后续请求读到上一次交换的回复；建议跟踪 outstanding terminal replies / interrupted I/O，在 RESET 前拒绝不可复用连接。明确说 RECORD 不是终止消息，SUCCESS/FAILURE/IGNORED 才是。 | **同一问题**，与静态假设和本次动态结果一致 |
| [neo4rs#298](https://github.com/neo4j-labs/neo4rs/pull/298)（2026-09-22 开，2026-09-25 合并，ChrisRega） | “Discard pooled connections with cancelled or unfinished Bolt exchanges.” 加了 `pending_responses`/`io_in_progress`、`is_reusable()`，recycle 在 RESET 前拒绝不可复用连接；`Fixes #295`。merge commit 在 main `19f244ae` 之后，**没有进任何发布版**。 | 上游修复；比本次补丁精细，但同样说明问题是真实的 |
| [neo4rs#293](https://github.com/neo4j-labs/neo4rs/issues/293)（2026-08-19 开，2026-10-09 关） | 每个池化查询 3 个 RTT（RESET + RUN + PULL），并给出 0.8.0 的 `pool.rs:53`、`connection.rs:81` 行号。 | 相关背景，不是取消正确性问题本身 |
| [neo4rs#300](https://github.com/neo4j-labs/neo4rs/issues/300)（2026-09-25 开，2026-10-09 关） | #298 之后，RECORD 解码错误会让流卡住；`finish()` 以前会把 PULL 的 SUCCESS 当成 DISCARD 的回复（同一类错位）。 | 相关后继问题；说明 #298 的修复路径 |
| [neo4rs#301](https://github.com/neo4j-labs/neo4rs/pull/301)（2026-09-28 开，2026-10-09 合并） | 把 PULL/DISCARD 与 RUN 流水线化；`recycle()` 会 drain pending responses 再 RESET；未消费的流不再丢弃连接。 | 与 main 的后续行为有关；0.8.0 没有这些 |
| [CHANGELOG.md](https://github.com/neo4j-labs/neo4rs/blob/main/CHANGELOG.md) | `Unreleased` 有流水线（#293）和 RECORD 解码（#300）的条目，其中提到 “Connections with interrupted reads are discarded rather than returned to the pool”；没有单独的 #295/#298 条目。 | 说明修复只在 main/Unreleased，发布版没有 |

结论：**这是已知问题，不是新发现。** 0.8.0（最新正式版）仍有；0.9.0-rc.10
（最新预发布）源码也仍没有 #298 的复用检查；修复只在未发布的 main。本次动态
验证的价值是：在真实 Neo4j 5.26 上确认了 0.8.0 的可触发停点、后继者可见
后果、以及 M1 相对 B1/B2/B3 的探测差异，而不是首次报告该缺陷。

## 7. 为什么 M1 抓到、B3 没抓到

每轮相同，共 6 个计划。低层级看不到的原因逐条如下：

| 计划 | M1 观察 | B3（不同操作）实际返回 | 为什么 B3 看不到 |
|---|---|---|---|
| N1 `#0@2:Immediate` | `own: Err(no row)`，`write: Ok`，`visible: 7` | `Ok(())`（SET v=2） | B3 只判 `Ok`，不同写没有观察者；被错位的 RESET 回复让写照样返回成功 |
| N1 `#0@2:AfterWake` | 同上 | `Ok(())` | RUN 的 SUCCESS 已被唤醒但未读，B3 的下一次写把残留 SUCCESS 当自己的 RUN 回复，仍是 `Ok` |
| N1 `#0@3:Immediate` | 同上 | `Ok(())` | DISCARD 已发出、写已提交，B3 的不同写返回 `Ok`，而 B3 不查服务端效果 |
| N1 `#0@3:AfterWake` | 同上 | `Ok(())` | 残留的 DISCARD SUCCESS 在 recycle 时被当 RESET 确认，下一写仍只看 `Ok` |
| N2 `#0@2:Immediate` | `own: Err(no row)`，`visible: 7` | `Ok([])`（不同 tag 的读） | 残留 RUN SUCCESS 被当成 B3 的 RUN 摘要，PULL 结束为空流；空行集仍是 `Ok` |
| N2 `#0@2:AfterWake` | 同上 | `Ok([])` | RUN 回复已在缓冲区，B3 的读同样把旧 SUCCESS 当摘要，返回 `Ok([])`，B3 不检查行数/值 |

B2 与 B3 同理：N2 的 B2 重试在这两个计划上返回 `Ok([])`，在 N1 上返回
`Ok(())`；B2 只判 `Ok`。M1 的幸存者读要求“看到 7”，所以空的流/缺失的行
立刻变成 `Neither`。

M1 多出来的 M1-residue 计划（N2 @3/@4，residue 45/105）没有用户可见后果：
这些切点残留以 RECORD 开头，recycle 丢弃连接，幸存者的 `own/own_tag/visible`
都正确；按 PLAN 单独统计，不计入确认问题。

## 8. 成本、代码行数、未完成

### 8.1 代码行数

`tests/neo4rs.rs` 共 449 行（含注释/空行）。按层级（不含共享环境代码）：

| 部分 | 行数 |
|---|---:|
| B1（`fn b1`） | 14 |
| B2（`fn b2`） | 27 |
| B3（`fn b3`） | 30 |
| M/M-residue（`fn m` + `async fn survive`） | 32 + 10 = 42 |
| 共享环境（`connect`/`Env`/`setup`/`cut`/`config`/`cleanup_prefix` 等） | 约 124 |
| 其余（imports、常量、`Obs`、`normalize`、`bounded`、测试入口） | 约 212 |
| 诊断用 `tests/probe.rs`（不计入 ladder） | 108 |

计数口径是函数体的物理行数，包含注释；B2/B3 共享 `cut`（19 行）算在共享环境。
`run-neo4rs.sh` 110 行；`patches/neo4rs/` 131 行（diff 129 + manifest 2）。

### 8.2 工时

约 **0.75 人时**（墙钟 12:40–13:15，未精确计时；不含写本文件后的收尾）。
大致分布：

| 花在哪 | 比例 |
|---|---:|
| 读 PAIRING/PLAN/phase3 场景、查上游、设计停点 | ~15% |
| 起 Neo4j、手工 probe、确认触发时序 | ~20% |
| 写场景/runner/observer/tag 检查 | ~25% |
| release 3 轮 + patched 3 轮（含编译） | ~20% |
| 分诊、写补丁、补丁对照 | ~15% |
| 整理结果 | ~5% |

### 8.3 未完成 / 未核实

- **0.9.0-rc.10 没有动态跑**：规则要求不用 rc；只做了源码对照。main 的上游
  修复（#298/#301）也没有动态验证，它不在任何发布版里。
- **没有直接记录 recycle 的 `Err` 被 deadpool 丢弃的那一刻**：N2 @3/@4 的
  “RECORD → reset Err → discard”是源码 + 观察推断，没有插桩日志直接证明。
- **没有做跨借用者记录内容泄露的穷举**：只在 M 的 `own_tag` 和 B3 的不同 tag
  上检查；所有实际输出都没有出现别的运行的 tag。没有观察到泄露，不等于所有
  切点/所有 0.8.0 API 路径都不会。
- 只测了 `Graph::run`/`Graph::execute`；没有测 `Txn`、`into_stream` 系列、
  TLS、routing、`unstable-bolt-protocol-impl-v2`。
- 只测 Neo4j 5.26.0 community 一个服务端版本；只用 loopback 和 50 ms 固定
  回复延迟；`Flavor::MultiThread` 的计划数在本轮 3 次重复中完全一致，但不能
  保证在其他机器上稳定。
- 补丁是“取消即丢弃连接”的保守对照，不是上游 #298 的实现；没有做性能/连接
  复用率评估。

## 9. 与任务设计的偏离

1. 最新正式版就是 0.8.0，所以没有“更高版本的正式版”可测；按规则没有测
   `0.9.0-rc.10`，只在第 1.1 节做了源码说明。
2. N1 用“对同一节点 SET”而不是 `CREATE`（任务给了两种选择）；这样 B2 同参数
   重试、B3 换值、M1 同节点都有明确语义，也避免重复节点影响观察者计数。
3. M1 的幸存者除了有副作用的写和独立观察者，还读了同一个池两次（`own` 和
   `own_tag`）。`own_tag` 专门用于检测“上一条 RECORD 被当成本次的行”；它仍然
   是观察，不改变“幸存者做有副作用的操作 + 独立观察者确认”的模板。
4. B1 最终不做 observer 清理，改为 explore 结束后按前缀清理；原因见 4.4。
   这是为了避免把服务端锁导致的清理阻塞误报成 B1 的 liveness 违反。
5. 补丁选“取消时置脏”的版本，不选“一直读到 RESET 自己的 SUCCESS”：后者在
   协议上无法仅凭回复内容区分旧的 SUCCESS 和 RESET 的 SUCCESS，容易变成
   启发式。任务允许两种补丁中的任一种。
6. 上游已有 #295，所以本轮结论是“动态确认一个已知问题”，不是“首次发现”。
   第 6 节记录了检索结果，未向任何上游地址发送内容。
7. 没有创建 `PRIVATE-NOTES.md`：没有观察到跨借用者读到上一次查询的记录内容
   （`own_tag` 始终是本次的 `-survivor` 值；B2/B3 只出现本查询的 tag 或空集）。
   后继者看到的是成功但空的结果 / `no row`，不是别人的数据。

## 10. 文件

| 文件 | 内容 |
|---|---|
| `Cargo.toml`, `Cargo.lock` | 独立实验 crate；neo4rs 0.8.0、deadpool 0.9.5 |
| `tests/neo4rs.rs` | N1/N2 × B1/B2/B3/M/M-residue；449 行 |
| `tests/probe.rs` | 手工时序诊断，不属于 ladder；108 行 |
| `run-neo4rs.sh` | 启动/停止固定镜像的容器，跑 release 或 patched 变体，写 `logs/` |
| `patches/neo4rs/neo4rs-0.8.0-cancelled-exchange.diff` | 分诊用的取消置脏补丁 |
| `patches/neo4rs/Cargo.patch.toml` | `[patch.crates-io]` 片段 |
| `logs/release-1..3.log` | release 三轮原始输出 |
| `logs/patched-1..3.log` | patched 三轮原始输出 |
| `logs/probe-60ms.log`, `logs/probe-120ms.log` | 两个取消时刻的手工 probe |
| `logs/environment.txt` | 镜像 tag/digest、服务端版本、工具链的实际记录 |
| `logs/README.md` | 日志与数字的对应关系 |
| `README.md` | 实验目录说明与运行方法 |
| `RESULTS.md` | 本文件 |
