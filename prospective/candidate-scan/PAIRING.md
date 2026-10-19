# 配对扫描：池 manager 适配器 × 底层客户端

本文件是 `candidate-scan` 的独立补充：`scanner/scan.py` 找的是单个 crate 内“出错路径置坏 / 取消路径不置坏”的不对称；这里找的是**池 manager 适配器与底层协议客户端之间的配对**——客户端不知道它会被池化，适配器不知道上一个交换有没有真正结束。

**范围声明：本轮只做静态文本/词法分析。** 没有 build、没有运行下载的源码、没有启动数据库或服务、没有执行 `build.rs`/测试/示例，也没有在 GitHub 或任何地方开 issue / PR / 评论。下载的源码在仓库外（`~/dropwise-scan-src/`），仓库里只新增 `pairing.py`、`pairing.csv` 和本文件；补丁只应用到 `/tmp` 下的副本。所有“可疑”都是静态判断，**不是已确认漏洞**；本轮**没有任何配对经过动态验证**。

## 一句话结论

- 全量 152 个 crate 中，识别出 **42 个 manager impl**、**46 条配对**；**40 条解析到底层客户端源码**，**6 条未解析或客户端源码缺失**。判定为 **可疑 8 条、不疑 27 条、不确定 11 条**。
- 校准全部符合预期：diesel-async 发布版 3/3 个 MySQL manager 路径可疑，打上 `diesel-async-mysql-is-broken.diff` 后 0/3；bb8 发布版 + bb8-memcached 可疑，打上 `bb8-invalid-until-validated.diff` 后不疑；deadpool-memcached 发布版仍可疑；bb8-redis、bb8-postgres、bb8-lapin 三个阳性对照均不疑。
- 8 条可疑里 7 条属于已知/已记录目标（diesel-async 3 条、bb8-memcached、deadpool-memcached、bb8-tiberius、deadpool-tiberius）。**唯一的新目标是 neo4rs 0.8.0 的 deadpool manager + 自带 `Connection`**（近 90 天 392,235 次下载），静态链条完整。**后续动态验证已确认**（[../neo4rs/RESULTS.md](../neo4rs/RESULTS.md)，只有 M 层级抓到），但上游已有同一问题的报告和修复（neo4rs#295 / #298，修复只在未发布的 main），不是新发现。
- `scan.py` 的结果没有被改动；本检查也不改变它的 216 条命中/21 条可疑。neo4rs 在 `candidates.csv` 里是 0 条命中，这是本轮唯一“扫描器 A/B/C 没抓到、配对规则抓到”的目标。

## 1. 方法

### 1.1 配对发现

对 `--root`（默认 `~/dropwise-scan-src/src`）下每个 `<crate>-<version>/src/*.rs` 做词法分析（跳过行/块注释、字符串、raw string、字符字面量、生命周期；保留 token 行号），识别三类 trait 的 impl：

- `bb8::ManageConnection`，关联类型 `type Connection = ...`；
- `deadpool::managed::Manager`（含 `managed::Manager`），关联类型 `type Type = ...`；
- `mobc::Manager`，关联类型 `type Connection = ...`。

只取**关联类型**作为被管理的连接类型；impl body 里的其他类型（错误类型、辅助结构）不参与。一条 impl 可以展开成多条配对（例如 diesel-async 的泛型 manager 同时有 MySQL 与 Postgres 两个具体 wrapper）；宏生成、`r2d2`、测试/文档示例里的 impl 跳过。

### 1.2 连接类型解析

解析顺序（每一步都记录 file:line 证据，解析不到就写 unresolved/unknown，不猜）：

1. `Arc`/`Box`/`Pin`/`Mutex`/`SyncWrapper` 等透明包装逐层拆开；
2. 限定路径（`mysql_async::Conn`、`rt::Client`）按首段判断 crate；`crate::`/`self::`/`super::` 走本地定义；
3. 裸名先查当前文件的 `use` 映射（支持 `use foo::{Bar, Baz as Q}`、模块别名、`use ... as ...`），再查 manager crate 内的 struct/enum/type alias；本地 wrapper 只下钻**连接语义字段**（`conn`/`client`/`connection`/`stream`/`socket`/`inner` 等），避免把 `StatementCache`、`Instrumentation` 这类辅助字段当成客户端；
4. 本地找不到时，才在 manager crate 的 Cargo.toml 直接依赖里找唯一同名定义；同名多义或没有依赖证据时记为 ambiguous/unresolved；
5. 泛型关联类型只在 trait bound 是 `PoolableConnection` 时展开（diesel-async 的 `AsyncMysqlConnection`/`AsyncPgConnection`）；`AsyncConnectionWrapper<C>`、`SyncConnectionWrapper<C>` 这类泛型 wrapper 没有具体实例，记入 `resolution_note` 并跳过，不猜一个客户端。

客户端源码没下载（`oracle`、`rusqlite`、`hdbconnect_impl`）时保留 `client_crate` 名称，但 `client_version`/标记分析写“未核实”，判定为 unknown。

### 1.3 客户端有没有“交换进行中”标记

扫描底层客户端 crate 的字段与赋值：

- 字段名匹配 `in_flight|inflight|in_progress|busy|sending|disconnected|dirty|pending|pending_result|in_use|writing|reading|awaiting|broken|tainted|poisoned|closed` 等；
- **cancel-sensitive** 的判定：字段先被赋成坏值/`true`，同一函数后面有 `.await`，await 之后在成功路径被恢复为 `false`/`Ok`/`Connected` 等。`mysql_async::Conn::routine` 的 `disconnected = true`（`mysql_async-0.37.1/src/conn/mod.rs:405`）→ await → `disconnected = false`（`:410`）就是典型；只在错误路径设置的标志不算。
- 同时识别 `is_disconnected`/`is_closed`/`is_broken` 这类 accessor，后面用于判断 manager 是否读取该标记；
- 识别“取消时丢弃连接”的 Drop guard：Drop body 调用 `mark_dead`/`mark_connection_dead`/`mark_broken`/`set_broken`/`invalidate`/`taint`/`poison`，或 Guard/Reset/Operation/Stream 类类型在含 await 的函数里被构造，且 Drop 里 close/abort/discard/detach。普通析构函数里顺手关 socket 不算。

### 1.4 manager 验证方法

按 kind 取验证方法：

| kind | 异步验证（发请求） | 同步坏连接检查 |
|---|---|---|
| bb8 | `is_valid` | `has_broken` |
| deadpool | `recycle`（另有 `is_broken`） | `is_broken` |
| mobc | `check` | `validate` |

- `validation_io = yes` 当且仅当验证方法体里有 `.await`；同步状态检查（如 bb8-lapin 的 `conn.status()`、bb8-lapin/deadpool-lapin 的 `connected()`、ignite 的 `is_alive()`）为 `no`。
- 从验证方法 + 同步检查方法出发，在 manager crate 内按函数名做有限深度（≤6）调用图搜索，找客户端标记名或 accessor 名；找到记 `manager_checks_flag=yes` 并给 file:line，找不到记 `no`。
- `validation_strict`：验证方法体或它调用的客户端方法里，对回复做 `==`/`!=` 字面量、`starts_with`/`ends_with` 前缀检查，或对期望值做相等比较，记 `yes`；只 `Ok(_)`/`is_ok()`/`map(|_| ())` 记 `no`。
- 验证走 `spawn_blocking`/`run_blocking`/`interact` 的记 `blocking_validation=yes`，判 unknown：取消 await 不会停止阻塞线程，这是另一种风险（并发使用/阻塞线程），不是本规则要判的“残留回复”。

### 1.5 池核心 shield

验证被取消时，连接会不会被放回池，取决于池核心：

- **bb8**：`Pool::get()` 在 `is_valid().await` 前是否把 `conn.state` 置为 `Invalid`，成功后恢复 `Present`。发布版只有 `Err` 分支才置 Invalid（`bb8-0.9.1/src/inner.rs:110-124`），补丁版在 await 前置 Invalid（补丁副本 `src/inner.rs:114`）。这就是本轮校准用的 shield。
- **deadpool**：`try_recycle` 在 `manager.recycle(...).await` 前把对象放进 `UnreadyObject`（`deadpool-0.13.1/src/managed/pool.rs:161-168`），它的 `Drop` 调用 `detach`（`:495-501`）。
- **mobc**：`validate_conn` 里 `check(raw).await` 拿走 raw 连接；future 被 drop 时 raw 随之 drop，不会回 idle（`mobc-0.9.0/src/lib.rs:401-424`）。

shield 只覆盖“**验证进行中被取消**”这个窗口，不覆盖“**用户请求进行中被取消**”后对象被放回池的窗口。因此 shield + 严格验证才是 protected；shield + 宽松验证仍按可疑处理。

### 1.6 判定规则（精确顺序）

```text
if 非协议客户端（如 mock server 池）            -> no
elif validation_io == "no"                       -> no（同步状态检查）
elif 多路复用/后台任务客户端                     -> no（回复由驱动按请求路由）
elif blocking_validation == "yes"                -> unknown
elif 客户端有取消即丢弃的 Drop guard             -> no
elif 客户端有 cancel-sensitive 进行中标记:
        manager 读了这个标记                     -> no
        manager 没读                             -> yes
        无法判断                                 -> unknown
else（客户端没有有效标记）:
        shield == yes 且 validation_strict == yes -> no
        shield == yes 且 validation_strict == no  -> yes
        shield == unknown                         -> unknown
        shield == no                              -> yes
```

关键点：**没有 shield 时，验证回复查得再严也不能排除“残留回复恰好就是期望回复”**——被取消的验证自己留下的 `PONG`/`VERSION` 与下一次验证的期望回复不可区分（bb8 的 `PONG` 例子）。严格性只在 shield 已经关掉验证窗口之后，才用于判断“用户取消留下的任意回复”能否通过验证。

### 1.7 多路复用 / 非 I/O 豁免

多路复用或后台任务客户端不满足“单条字节流一问一答”的前提：取消只丢掉该请求的 oneshot/future，驱动仍会把回复读走并按请求 id/顺序处理，下一位不会读到错位回复。本轮用一份**显式 allowlist**（crate + 类型），每条都在 `pairing.csv` 的 `reason` 里写明原因和证据：

| 客户端 | 类型 | 豁免原因 | 证据 |
|---|---|---|---|
| redis | `MultiplexedConnection` / `Connection` / `ClusterConnection` | redis 在单 socket 上多路复用，内部按请求路由回复 | `redis-1.7.1/src/aio/multiplexed_connection.rs` |
| tokio-postgres | `Client` | 后台连接任务 + 每查询 channel | `tokio-postgres-0.7.18/src/client.rs` |
| lapin | `Connection` | AMQP channel / delivery tag 多路复用 | `lapin-4.12.2/src/connection.rs` |
| ldap3 | `Ldap` | 后台任务按 LDAP message id 路由 | `ldap3-0.12.1/src/ldap.rs` |
| clickhouse-arrow | `Client` | IO task + oneshot 回复 | `clickhouse-arrow-0.2.1/src/client/connection.rs:222,374` |
| klickhouse | `Client` | 后台 task + oneshot 回复 | `klickhouse-0.16.0/src/client.rs:338,248` |
| rustis | `ExclusiveClient` | network task + oneshot 回复 | `rustis-0.27.1/src/client/client.rs:38`；`src/client/command_future.rs:55` |
| gremlin-client | `Conn` | `Cmd::Msg` 带 `Uuid` 请求 id 的 worker | `gremlin-client-0.8.10/src/aio/connection.rs:55-72` |
| toasty | `ConnectionHandle` | 操作经 connection task 串行化 + oneshot | `toasty-0.11.0/src/db/connection_task.rs:6-9,87-102` |
| wiremock | `BareMockServer` | in-process mock server 池，`reset()` 不是网络协议交换 | `wiremock-0.6.5/src/mock_server/pool.rs:54-77` |

这些是**按 crate/类型列出的启发式豁免**，不是对所有“后台任务”客户端的通用证明；新增客户端需要人工确认后加入。

### 1.8 局限

1. **纯文本/词法，无类型信息/MIR。** 字段是不是连接状态、`Drop` 是丢弃还是放回池、`async_trait` 展开后的签名，都靠 token 与人工规则；不解析 feature `cfg`，也不展开宏（`redis-1.7.1/src/bb8.rs` 的宏生成 impl 因此没有作为 manager 发现）。
2. **类型解析是启发式。** `use` 别名、本地 type alias、struct 字段名优先级、Cargo 依赖白名单都可能漏；同名多义、泛型 wrapper、跨 crate 的 re-export 会记 unresolved，不猜。
3. **标记检测是启发式。** 字段名表可能漏掉自定义命名；用 helper 方法/原子操作/`Option::take` 设置的标记可能识别不到；Drop guard 必须能在含 await 的函数里找到构造点。
4. **manager 调用图只在本 crate 内按函数名搜索。** trait 默认实现、trait object、宏、跨 crate 的 helper 可能漏；`manager_checks_flag=no` 不等于“绝对没检查”，只是本规则没找到。
5. **严格性判断是文本比较。** 只认字面量比较/前后缀/相等比较；自定义解析器的语义可能更强或更弱。`validation_strict` 只在 shield 分支使用。
6. **shield 只覆盖验证窗口。** 用户请求被取消后对象回池的窗口没有被 shield 覆盖；这也是 deadpool-memcached 仍判可疑的原因。
7. **多路复用 allowlist 是 crate/类型级**，见 1.7；没有动态验证这些驱动的回复路由。
8. **版本缺口。** bb8-memcached 0.7.0 的 Cargo.toml 依赖 bb8 0.8（RESCAN 4.2 记录为 0.8.6），这里用下载的 bb8 0.9.1 作为池核心（仓库补丁同时适用于 0.8.6 与 0.9.1）；deadpool-memcached 0.5.0 依赖 async-memcached 0.7，下载到的是 0.8.1；diesel-async 0.9.2 的 Cargo.toml 依赖 mysql_async 0.36，本轮按任务要求用下载的 0.37.1（0.36 的 `is-disconnected` 回移补丁未使用）。这些版本差异写在 `pairing.csv` 的 `client_version`/`resolution_note` 里。
9. **没有动态测试，也没有核对上游是否已知。** 除仓库已有记录外，neo4rs 等目标是否已有公开报告，**未核实**。

## 2. 校准

校准副本只放在 `/tmp`，没有修改 `~/dropwise-scan-src` 或仓库源码：

```bash
REPO=/home/ava/Desktop/Dropwise
rm -rf /tmp/pairing-calib && mkdir -p /tmp/pairing-calib
cp -a ~/dropwise-scan-src/src/diesel-async-0.9.2 /tmp/pairing-calib/diesel-async-0.9.2+fixed
(cd /tmp/pairing-calib/diesel-async-0.9.2+fixed && \
  patch -p1 -s < "$REPO/prospective/scan-diesel-async/patches/diesel-async-mysql-is-broken.diff")
```

### 2.1 已知目标：发布版 vs 补丁版

| 目标 | 发布版判定 | 补丁版判定 | 依据 |
|---|---|---|---|
| diesel-async 0.9.2 + mysql_async 0.37.1，bb8 manager | **可疑 yes** | **no** | 发布版 `PoolableConnection for AsyncMysqlConnection` 是空实现（`diesel-async-0.9.2/src/mysql/mod.rs:366`），`has_broken` 读不到 `disconnected`；补丁版在 `src/mysql/mod.rs:371` 读 `is_disconnected()`，`manager_checks_flag=yes` |
| 同上，deadpool manager | **可疑 yes** | **no** | 发布版 `recycle` 先 `is_broken()` 再 `ping`，`is_broken` 默认实现不查 `disconnected`（`src/pooled_connection/deadpool.rs:96-103`）；补丁后同上 |
| 同上，mobc manager | **可疑 yes** | **no** | `check` 发 ping；mobc 的同步 `validate` 也读 `is_broken`。补丁后 `validate` 能读到 `is_disconnected()`（`manager_checks_flag=yes`） |
| bb8 0.9.1 池 + bb8-memcached 0.7.0 + memcache-async 0.10.1 | **可疑 yes** | **no** | 发布版 bb8 的 `get()` 在 `is_valid().await` 期间 state 仍是 Present（`bb8-0.9.1/src/inner.rs:110-124`）；补丁版在 await 前 `state=Invalid`、成功后 `Present`（补丁副本 `src/inner.rs:114`），`shield=yes`。memcache-async 的 `version()` 严格检查 `VERSION` 前缀（`memcache-async-0.10.1/src/ascii.rs:238`），因此 shield + strict 判 no |
| deadpool 0.13.1 池 + deadpool-memcached 0.5.0 + async-memcached 0.8.1 | **可疑 yes** | 无仓库补丁 | `recycle` 只做 `conn.version().await` 并接受 `Ok(_)`；async-memcached 的 `version()` 只要求长度 ≥8（`async-memcached-0.8.1/src/lib.rs:172`），宽松验证 + 用户取消窗口 → shield 仍判 yes |
| bb8-redis 0.26.0 + redis 1.7.1 `MultiplexedConnection` | **no** | **no** | 多路复用豁免（`reason` 写明 redis 内部路由） |
| bb8-postgres 0.9.0 + tokio-postgres 0.7.18 `Client` | **no** | **no** | 多路复用豁免（后台连接任务 + 每查询 channel） |
| bb8-lapin 0.8.1 + lapin 4.12.2 `Connection` | **no** | **no** | `is_valid` 只读 `conn.status()`，没有 await（`bb8-lapin-0.8.1/src/lib.rs:75-85`），非 I/O 豁免 |

校准运行的实际计数：

- diesel-async（release + `+fixed` 两个 variant）：`crates=153 manager_impls=6 pairs=12 resolved=12 unresolved=0 suspicious=yes:3 no:9 unknown:0`。3 条 yes 全部是 release 的 MySQL manager；3 条 patched MySQL 全部 no；Postgres 的 6 条全部 no（多路复用）。
- bb8 发布版：`crates=152 manager_impls=5 pairs=5 resolved=5 unresolved=0 suspicious=yes:2 no:3 unknown:0`。yes 是 bb8-memcached 与 deadpool-memcached；no 是 bb8-lapin/bb8-postgres/bb8-redis。
- bb8 补丁版（`--pool-patch bb8=...`）：`crates=153 manager_impls=5 pairs=5 resolved=5 unresolved=0 suspicious=yes:1 no:4 unknown:0`。唯一 yes 是 deadpool-memcached（deadpool 池不受 bb8 补丁影响）；bb8-memcached 从 yes 变为 no。`pool bb8 0.9.1+patched: shield=yes (conn.state=Invalid before is_valid await, Present after)` 来自实际输出。

### 2.2 补丁路径

- **diesel-async**：`prospective/scan-diesel-async/patches/diesel-async-mysql-is-broken.diff`，给 `impl PoolableConnection for AsyncMysqlConnection` 加 `is_broken`，检查 `self.conn.is_disconnected()`。`manager_checks_flag` 的 BFS 从 `has_broken` → `is_broken` 找到 `src/mysql/mod.rs:371` 的 `is_disconnected`，所以 release/patched 的判定翻转是可解释的，不是按 crate 名特判。
- **bb8**：`prospective/t2-bb8/patches/bb8-invalid-until-validated.diff`，在 `is_valid().await` 前 `conn.state = ConnectionState::Invalid`，成功后 `Present`。未打补丁时 `shield=no`；打补丁后 `shield=yes`（`src/inner.rs:114`），再结合 memcache-async 的严格 `VERSION` 检查，bb8-memcached 从可疑变不疑。
- **mysql_async-0.36-is-disconnected.diff** 未使用：本轮按任务指定用 0.37.1，0.37 已有 `is_disconnected`（`mysql_async-0.37.1/src/conn/mod.rs:211-213`）。

### 2.3 校准限制

- bb8-memcached 0.7.0 的 Cargo.toml 写的是 bb8 0.8（RESCAN 4.2 记录为 0.8.6），而下载/扫描的是 bb8 0.9.1；补丁本身同时适用于 0.8.6 与 0.9.1，但这里没有单独下载 0.8.6 复跑。
- deadpool-memcached 0.5.0 的 Cargo.toml 写的是 async-memcached 0.7，下载到的是 0.8.1；两者 `version()` 都是长度 ≥8 的宽松检查（0.8.1 行号已给），结论不依赖 0.7/0.8 差异。
- 没有动态测试；校准只证明“规则能区分已知发布版/补丁版”，不证明真实网络条件下一定能触发。

## 3. 全集结果

### 3.1 规模

| 项目 | 数量 |
|---|---:|
| 扫描的 crate | 152 |
| 识别的 manager impl | 42 |
| 配对行 | 46 |
| 解析到客户端源码 | 40 |
| 未解析 / 客户端源码缺失 | 6 |
| 可疑 yes | 8 |
| 不疑 no | 27 |
| 不确定 unknown | 11 |

按 manager kind：

| kind | 配对行 | 可疑 yes |
|---|---:|---:|
| bb8 | 20 | 3 |
| deadpool | 22 | 4 |
| mobc | 4 | 1 |

配对数多于 impl 数，是因为 diesel-async 的泛型 manager 展开成 MySQL 与 Postgres 两个具体客户端；46 行涉及 37 个不同的 manager crate。

### 3.2 8 条可疑

| 目标 | 性质 | 说明 |
|---|---|---|
| diesel-async 0.9.2 的 MySQL + bb8 / deadpool / mobc | 已知（`scan-diesel-async` 已动态验证，已上报） | 3 条；客户端 `disconnected` 是 cancel-sensitive 标记，发布版 manager 不读 |
| bb8-memcached 0.7.0 + memcache-async 0.10.1 | 已知（RESCAN 4.2 / bb8 issue #324） | 1 条；release bb8 无 shield，验证窗口回池 |
| deadpool-memcached 0.5.0 + async-memcached 0.8.1 | 已知（RESCAN 5.2 D4） | 1 条；宽松 `version()` + 用户取消窗口 |
| bb8-tiberius 0.16.0 + tiberius 0.13.0 | 已记录为不确定（RESCAN 4.2） | 1 条；tiberius 的 `poisoned` 是写入期标记，manager 不读；驱动 `flush_stream` 的清理不在本规则内 |
| deadpool-tiberius 0.1.9 + tiberius 0.13.0 | 已记录为不确定（RESCAN 5.2） | 1 条；同上，`recycle` 接受任意 `Ok(_)` |
| **neo4rs 0.8.0 + 自带 `Connection`** | **新（本轮唯一）** | 1 条；见第 4 节 |

所以：8 条可疑 = 7 条已知/已记录 + **1 条新的 neo4rs**。

### 3.3 11 条不确定（没有进“可疑”）

| 类别 | 配对 | 原因 |
|---|---|---|
| 阻塞线程验证 | async-bb8-diesel 0.3.0、bb8-diesel 0.2.1、deadpool-diesel 0.7.0（2 行）、miden-client-sqlite-store 0.17.3 | 验证在 `spawn_blocking`/`run_blocking`/`interact` 里，取消 await 不会停止阻塞线程；这是并发/阻塞风险，不是“残留回复”规则能判的 |
| 客户端源码缺失 | bb8-oracle 0.3.0（oracle）、data-connector 2.4.0（oracle）、deadpool-sqlite 0.14.0（rusqlite）、matrix-sdk-sqlite 0.19.1（rusqlite）、hdbconnect_async 0.33.0（hdbconnect_impl） | 下载集里没有对应客户端源码，不能检查标记；写 unknown 而不是猜 |
| 关联类型无法解析 | taos-query 0.12.4 | `type Type = <T as AsyncTBuilder>::Target` 是关联类型投影，未解析 |

27 条 no 主要是：多路复用/后台任务客户端（redis、tokio-postgres、lapin、ldap3、clickhouse-arrow、klickhouse、rustis、gremlin、toasty）、同步状态检查（bb8-lapin、deadpool-lapin、esrs、ignite、gcp-bigquery）、非协议 mock server（wiremock）、以及 mssql-tiberius-bridge（客户端 `Operation`/`SessionReset` Drop guard 在取消时 `mark_connection_dead`）。

## 4. 新的可疑配对

排除已知目标（deadpool-memcached + async-memcached、diesel-async + mysql_async、bb8 / bb8-memcached、lettre、sqlx-mysql）和 `candidate-scan/README.md` 短名单里的目标后，按 `universe.csv` 的 90 天下载量排序：

| # | 目标 | manager 90 天下载 | client 90 天下载 | 文件:行号 | 理由 |
|---:|---|---:|---:|---|---|
| 1 | **neo4rs 0.8.0** deadpool manager + 自带 `neo4rs::Connection` | 392,235 | 392,235 | `neo4rs-0.8.0/src/pool.rs:44`（impl）、`:53-54`（`recycle`）；`neo4rs-0.8.0/src/connection.rs:29-32`（字段）、`:80-89`（`reset`/`send_recv`） | `recycle` 只做一次 `conn.reset().await` 并接受 `BoltResponse::Success`；`Connection` 只有 `version`/`stream` 两个字段，没有交换进行中标记，也没有取消时置坏的 Drop guard。RESET 是单条字节流上的一问一答，被取消查询的残留回复可能先被 `reset()` 读走并被当成 RESET 的成功回复，连接带着错位状态回池；下一个借用者读到的就是上一个交换的回复。 |

**这是静态可疑，不是已确认漏洞，本轮没有做任何动态测试。** deadpool 的 `UnreadyObject` 只覆盖“recycle 进行中被取消”的窗口；本条的触发点还包括用户请求被取消后 `Object` 回池、下一次 `recycle` 读到残留回复的窗口，所以仍判 yes。

没有进入本表的接近项及原因：

- `bb8-tiberius` / `deadpool-tiberius`：已在 RESCAN 4.2 / 5.2 记录为不确定；tiberius 的 `flush_stream` 会在下一次请求前清掉未读回复，本规则没有把它建模成标记。
- `mssql-tiberius-bridge`：客户端的 `SessionReset` / `Operation` Drop guard 在取消时 `mark_connection_dead`，判 no。
- `clickhouse-arrow`、`klickhouse`、`rustis`、`gremlin-client`、`toasty`：后台任务/oneshot 回复路由，按多路复用豁免判 no；已列在 1.7 的 allowlist。
- `wiremock`：mock server 池，不是协议客户端，判 no。
- `async-bb8-diesel`、`bb8-diesel`、`deadpool-diesel`、`miden-client-sqlite-store`：阻塞线程验证，unknown；没有足够证据升为可疑。
- `bb8-oracle`、`data-connector`、`deadpool-sqlite`、`matrix-sdk-sqlite`、`hdbconnect_async`、`taos-query`：客户端源码缺失或关联类型无法解析，unknown。

## 5. 与 `scan.py` 结果的差异

1. **`scan.py` 不变。** 本轮没有修改 `scan.py`，也没有重写 `hits.csv`/`candidates.csv`；本文件与 `pairing.csv` 是独立新增的检查。`scan.py` 的 216 条命中/21 条可疑结论仍然成立。
2. **配对规则复现了已知目标。** bb8-memcached、deadpool-memcached、diesel-async 的 MySQL 路径都在配对表里被判 yes；diesel-async 打补丁后翻转，bb8-memcached 在 bb8 补丁后翻转。
3. **新目标 neo4rs 是 `scan.py` 没抓到的。** `scan.py` 的模式 C 只命中“`is_valid`/`recycle` 里做 I/O 后接受任意 `Ok(_)`/`is_ok()`”之类的形态；neo4rs 的 `recycle` 是 `Ok(conn.reset().await?)`，既不是 `Ok(_)` 也不是 `is_ok()`，所以 `candidates.csv` 里 neo4rs 是 0 条。配对规则从“客户端没有 cancel-sensitive 标记 + 验证做真实 I/O”这一层抓到了它。
4. **它仍然不是漏洞列表。** 27 条 no 与 11 条 unknown 说明配对规则也有大量不适用/证据不足的形态；8 条 yes 里 7 条是已知/已记录。neo4rs 是否真的可触发，需要动态测试（Op1 = 查询中途取消，Op2 = 同一池的 RESET/下一条查询，观察者 = 另一条连接或服务端日志），本轮没有做。

## 6. 复现

```bash
# 全量配对表（写入仓库内的 pairing.csv）
python3 -I prospective/candidate-scan/scanner/pairing.py \
  --root ~/dropwise-scan-src/src \
  --out prospective/candidate-scan/pairing.csv

# diesel-async 发布版 + 补丁版（补丁副本先按 2 节准备到 /tmp/pairing-calib）
python3 -I prospective/candidate-scan/scanner/pairing.py \
  --root ~/dropwise-scan-src/src \
  --calibration-dir /tmp/pairing-calib \
  --only diesel-async \
  --out /tmp/calib-diesel.csv

# bb8-memcached / deadpool-memcached / 三个阳性对照：发布版
python3 -I prospective/candidate-scan/scanner/pairing.py \
  --root ~/dropwise-scan-src/src \
  --only bb8-memcached,deadpool-memcached,bb8-redis,bb8-postgres,bb8-lapin \
  --out /tmp/calib-bb8-release.csv

# 同一个子集：bb8 池打补丁
python3 -I prospective/candidate-scan/scanner/pairing.py \
  --root ~/dropwise-scan-src/src \
  --only bb8-memcached,deadpool-memcached,bb8-redis,bb8-postgres,bb8-lapin \
  --pool-patch bb8=prospective/t2-bb8/patches/bb8-invalid-until-validated.diff \
  --out /tmp/calib-bb8-patched.csv
```

扫描器只用 Python 3 标准库；`--pool-patch` 只用内置的极简 unified-diff 应用器，把补丁打到 `/tmp` 的副本上，不修改下载源码。`--only` 按 manager crate 名过滤，`--calibration-dir` 可重复，`--pool-source KIND=DIR` 可直接使用已准备好的池核心源码。

## 7. 文件

| 文件 | 内容 |
|---|---|
| `scanner/pairing.py` | 配对发现、类型解析、标记/验证/shield 分析、CSV 输出 |
| `pairing.csv` | 46 条配对（含 header；`suspicious` 列取 yes/no/unknown） |
| `PAIRING.md` | 本文件 |
| `README.md` | 只加了一行指向本文件的指针；`scan.py` 与原有 CSV 未改 |
