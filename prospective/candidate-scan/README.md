# 候选扫描：出错路径 / 取消路径不一致

本目录是对“某个跨 `.await` 的交换，在 `Err` 分支把连接标记为坏，但同一个挂起点被取消时不标记”这一静态特征的普查。扫描对象是 bb8 / deadpool / mobc 的反向依赖，加上一批自带连接池或复用连接的异步客户端。

**范围声明：本轮只做静态分析。** 没有 build、没有运行下载的源码、没有启动数据库或服务、没有在 GitHub 或任何地方开 issue / PR / 评论。下载的源码在仓库外（默认 `~/dropwise-scan-src/`），没有提交。是否已上报、是否为已知问题，除仓库内已有记录外均**未核实**。

## 一句话结论

- 4 个已知实例全部召回（lettre 0.11.23、bb8-memcached 0.7.0、sqlx-mysql 0.9.0、bb8 0.9.1），3 个补丁版都识别为“命中消失 / 已防护”。
- 全集 152 个 crate、9067 个 `src/*.rs`，扫描器给出 216 条命中；人工复核后 **21 条可疑**（分布在 6 个目标）、5 条不确定、190 条误报（其中 12 条是“已有 guard”）。
- 21 条可疑里 **19 条属于已知实例**；唯一的新目标是 **diesel-async 0.9.2 的 MySQL `is_broken` 默认实现**（bb8/deadpool manager 共用），静态链条完整，未动态验证。
- 随机抽 20 条未防护命中：18 条误报，2 条是已知的 bb8-memcached，误报率 **90%**（95% Wilson 区间约 70%–97%）；没有抽到新目标。这说明扫描器是召回优先，精度必须靠人工复核。

## 1. 方法

### 1.1 候选全集

`scanner/collect.py` 从 crates.io API 取：

1. `bb8`、`deadpool`、`mobc` 的反向依赖；
2. `deadpool-*` 系列（crates.io 搜索 `deadpool`，按名字前缀过滤）以及这些 manager 的反向依赖；
3. 一份人工维护的异步客户端种子列表（数据库、缓存、SMTP/IMAP、消息队列、RPC、对象存储、HTTP/2 等，约 130 个名字）。

合并去重后得到 815 个 crate，按 crates.io 的 `recent_downloads`（近 90 天）降序，取前 150 个；另外强制加入 `mysql_async`、`diesel-async`、`async-imap`，以及下载量很低但属于已覆盖/已知实例的 `bb8-memcached`、`deadpool-memcached`。最终 **152 个选中 crate**。`universe.csv` 记录了全部 815 个 crate 的排名、下载量、来源、是否已覆盖、是否选中。

“已覆盖”标记：`bb8`、`bb8-memcached`、`deadpool-memcached`、`sqlx-postgres`、`sqlx-mysql`、`lettre`、`tokio-postgres`、`deadpool-postgres`、`redis`、`moka`、`async-nats`、`tokio-tungstenite`。它们仍在全集与命中表中，但不再深挖。

### 1.2 源码获取

- 用 `static.crates.io` 的 `.crate` tarball，解压到 `~/dropwise-scan-src/src/<crate>-<version>/`。
- 解压前校验路径：拒绝绝对路径、`..`、链接/设备、顶层目录不匹配；单文件与总解压大小有上限。
- 不做 `cargo build`、不运行 `build.rs`、不跑测试/示例；扫描器只做文本/词法分析。
- 校准专用的源码在 `~/dropwise-scan-src/calibration/`；补丁版目录名带 `+fixed`。

### 1.3 扫描器

`scanner/scan.py`（Python 3，`python3 -I` 运行）：

- 手写 Rust 词法分析：跳过行/块注释（支持嵌套）、字符串/raw string/字节串/字符字面量/生命周期；保留位置与行号。
- 括号匹配 + 函数体切分；识别 `fn`、`async fn`、`impl` 上下文、`macro_rules!`。
- **模式 A（出错/取消不一致）**：找出在 `.await` 关联的错误路径里设置“坏”标记的位置，再找同一交换的取消保护。
  - 坏标记：字段名含 `broken|tainted|poison|closed|panic|invalid|dirty|healthy|failed`，或 `sending|in_flight|in_progress|busy`；`close()/abort()/shutdown()/taint()/set_broken/mark_broken...`；`state = ConnectionState::Invalid` 这类 CamelCase 坏值。
  - 错误路径：`Err` arm、`if let Err`、`is_err`、`map_err`、`inspect_err`、`?`，以及宏定义里的 `Err` arm。宏调用点会与宏定义关联（`try_smtp!` 就是这样召回的）。
  - 保护：await 前置坏/完成后恢复的悲观标记；带 `Drop` 的 guard（且 Drop 丢弃/回滚而不是放回池）；`Option::take` + 恢复；`has_broken` 读取一个在 await 前就置位的进行中标志。
- **模式 B（状态在 await 之后才提交）**：`transaction_depth`、`in_transaction`、`session`、`state`、`flags`、`*_count` 等字段在 await 之后才赋值，且 await 前没有 guard / 预先占位。
- **模式 C（池 manager 层）**：`has_broken/is_broken` 恒为 false，或只读取一个与取消无关的标志；`is_valid/recycle/ping/test_connected` 做 I/O 后接受任意 `Ok(_)` / `is_ok()` / `is_positive()`。
- 输出每条命中：crate、版本、文件:行号、模式、代码片段（≤15 行）、扫描器的 guarded 判断、置信度、说明；完全相同的 `(crate,file,line,pattern)` 去重。

扫描器**故意高召回**：字段名、`close()`、计数器和 `Drop` 的语义都只看文本，没有 MIR/类型信息，也不解析 feature 条件。精度由第 1.4 步保证。

### 1.4 人工复核

`reviews/REVIEW-GUIDE.md` 定了四个问题：

1. 这个 `.await` 是不是网络/会话往返（不是锁、通道、任务 join、文件、sleep）？
2. 被标记的状态是否属于一个会活过 future、会被复用的连接/会话（不是局部变量、任务句柄、文件、通道、测试）？
3. 取消路径有没有等价保护（悲观标记、Drop guard、take/restore、进行中标志）？
4. 如果 future 在这个 await 被 drop，下一个使用者具体看到什么？

结论只取三类：`可疑`（值得动态测试）、`误报`（写明哪一问不成立）、`不确定`（证据不足，不猜）。复核由 6 个子任务按 crate 分组并行完成，主任务统一了口径、补了 `test_connected` 一条命中，并对 lettre 的非池化 starttls/ehlo/auth/quit 做了覆盖。所有 216 行的结论都在 `candidates.csv`。

## 2. 校准

### 2.1 已知实例召回

在 `lettre 0.11.23`、`bb8-memcached 0.7.0`、`sqlx-mysql 0.9.0`、`bb8 0.9.1` 上运行，**4/4 召回**：

| 实例 | 扫描器命中 | 未修复时的判定 |
|---|---|---|
| lettre 0.11.23 异步池化 SMTP | `async_connection.rs:184/192/199/202`（`try_smtp!` 的 4 个 send 往返）+ `async_transport.rs:75` + `has_broken` C at `:206` | 可疑 |
| bb8-memcached 0.7.0 | `client.rs:143/161/177/198/219/239/253/266`（8 个方法只在自身 timeout 的 `Err(_elapsed_err)` 里 `taint()`）+ `lib.rs:91` `has_broken` C | 可疑 |
| sqlx-mysql 0.9.0 `begin` | `transaction.rs:31` B（`execute(BEGIN).await?` 之后才 `transaction_depth += 1`） | 可疑 |
| bb8 0.9.1 `Pool::get()` | `inner.rs:110` A（`is_valid().await` 期间 `state` 仍是 `Present`，Err 分支才置 `Invalid`） | 可疑 |

补充：lettre 的 `test_connected`（`async_connection.rs:271`）也被召回为模式 C：`NOOP` 的 `is_ok()` 接受 3xx（`response.rs:126-131`），残留的 `354` 会被当成健康回复。这一条不在最初的 4 个“已知实例”定义里，但属于同一根因链；扫描器的 `test_` 名称过滤最初把它漏掉了，后来加了 `test_connected/test_connection/test_ping` 例外。

### 2.2 补丁版对照

| 目标 | 未修复 | 补丁版 |
|---|---|---|
| lettre（PR #1171 等价补丁：`sending` 悲观标记 + `has_broken` 读取 + `abort` 检查） | send 路径 4 个 `try_smtp!` 点 A 未防护；`has_broken` C 未防护 | send 路径 4 个点标为 `pessimistic-flag`；`has_broken` 标为 `pessimistic-flag:send`。`test_connected` C **仍为未防护**：PR 没有改 NOOP 接受 3xx 的语义。starttls/ehlo/auth/quit 4 个点仍未防护，但它们不在池化 send 路径上（复核判误报）。 |
| sqlx-mysql（仓库 `patches/sqlx-mysql-begin.diff`：先 `transaction_depth += 1` 再 await，`Rollback` Drop guard） | `begin` B 未防护 | `begin` 的 B 命中**消失**；`commit`/`rollback` 仍有 B 命中，但人工复核确认 core `Transaction::drop → start_rollback` 会补偿，属于已防护。 |
| bb8（PR #325 等价补丁：await 前 `state = Invalid`，成功后 `Present`） | `get()` A 未防护 | 同一条命中标为 `pessimistic-flag`。 |
| bb8-memcached | 8 个 A + 1 个 C 未防护 | 上游没有对应补丁，无法做修复版对照；issue 只有报告。 |

**校准结论：** 已知实例 4/4 召回；3 个有补丁的目标都能识别出“命中消失或已防护”。lettre 补丁没有覆盖 `test_connected` 的宽松验证，这是补丁本身的边界，不是扫描器漏报。

### 2.3 启发式迭代（召回不全时改了什么）

初版在四个已知实例上并不全中，依次修了这些：

1. **`+=` 被词法器拆成 `+` 和 `=`**：sqlx-mysql `transaction_depth += 1` 完全没被模式 B 看到。改为合并 `:: -> => == != <= >= += -= *= /= |= && || ..` 等多字符运算符。
2. **宏里的错误分支**：lettre 的 `try_smtp!` 在宏定义里 `Err(err) => { $client.abort().await; return Err(...) }`，调用点的 await 在宏参数里。扫描器增加 `macro_rules!` 定义解析，把“Err arm 有坏标记”的宏和“参数里含 `.await`”的调用点关联起来。
3. **`state = ConnectionState::Invalid`**：bb8 的字段名是 `state`，不在坏字段表里，坏值在 `=` 后面第二个 token。增加 RHS 前 5 个 token 的 CamelCase 坏值识别；同时避免把任意 `field = true` 都当坏标记（这曾把 `retry=true`、`found=true`、`exhausted=true` 全扫进来）。
4. **补丁版的 guard 识别**：bb8 补丁是“await 前置 `Invalid`，Ok arm 恢复 `Present`”，初版只按字段值取 `toks[i+2]`，看不到 `ConnectionState::Invalid`。改为值序列匹配；宏命中则用“任意字段”的悲观标记检测，因为 `try_smtp!` 的字段名不一定出现在宏体里。
5. **模式 C 的“已防护”判定**：`has_broken` 返回的字段可能在 `abort/close/taint` 等错误处理函数里被置位，不能算悲观标记；补丁版 lettre 的 `sending = true` 在 `send()` 主路径、await 之前，才算 guard。增加了“赋值所在函数名 + 是否在 await 之前”的判断。
6. **测试代码过滤**：一开始按 `test_*`/`should_*` 名字直接跳过，结果把 lettre 的 `test_connected`（生产健康检查）漏了。改为：`#[test]`/`#[tokio::test]`/`#[cfg(test)]` 上下文 + 名字前缀，但 `test_connected/test_connection/test_ping` 例外。
7. **`mark_*` 过宽**：`mark_migrated`、`mark_down`、`mark_up` 不是连接坏标记，收窄为 `mark_broken|mark_tainted|mark_closed|mark_invalid|mark_failed|mark_unhealthy|mark_dirty|mark_poisoned`。
8. **去重**：同一文件同一行同一模式只保留一条（例如 jsonrpsee 的两个相同 close 调用）。

### 2.4 校准中的误报

校准集共 41 条命中，除 4 个已知实例外，还有不少周边误报：`sqlx-mysql` 的 `close`/测试清理、lettre 的 starttls/ehlo/auth/quit 宏点、`sqlx-mysql commit/rollback`（人工确认有 `Transaction::drop` guard）等。这说明扫描器不是“命中即漏洞”；校准只用于证明召回，不用来估计精度。

## 3. 全集结果

### 3.1 规模

| 项目 | 数量 |
|---|---:|
| 发现的反向依赖/种子 crate | 815 |
| 选中并下载源码 | 152 |
| 扫描的 `src/*.rs` 文件 | 9067 |
| 有命中的 crate | 62 |
| 命中总数 | 216 |
| 模式 A / B / C | 114 / 54 / 48 |

### 3.2 命中与复核统计

| 复核结论 | 条数 | 说明 |
|---|---:|---|
| 可疑 | 21 | 19 条属已知实例，2 条是 diesel-async 新线索 |
| 不确定 | 5 | bb8-oracle、bb8-tiberius、deadpool-tiberius、hdbconnect_async、rbdc |
| 误报 | 190 | 其中 12 条是“已有等价 guard”，复核时并入误报并在 note 前缀 `已防护：` |

按模式：

| 模式 | 可疑 | 不确定 | 误报 |
|---|---:|---:|---:|
| A（Err 置坏 / 取消不置坏） | 14 | 0 | 100 |
| B（状态在 await 之后提交） | 1 | 0 | 53 |
| C（池 manager 验证） | 6 | 5 | 37 |

21 条可疑按目标：

| 目标 | 可疑条数 | 性质 |
|---|---:|---|
| sqlx-mysql 0.9.0 `begin` | 1 | 已知（#4467 / PR #4468） |
| bb8 0.9.1 `Pool::get` | 1 | 已知（#324 / PR #325） |
| lettre 0.11.23 send/`has_broken`/`test_connected` | 7 | 已知（#1170 / PR #1171；NOOP 3xx 为同链路的额外点） |
| diesel-async 0.9.2 MySQL `is_broken` / bb8 manager | 2 | **新线索，未动态验证** |
| deadpool-memcached 0.5.0 `recycle` | 1 | 已覆盖（RESCAN 5.2 D4） |
| bb8-memcached 0.7.0 | 9 | 已覆盖（RESCAN 4.2） |

### 3.3 随机抽样误报率

从主运行的 215 条未防护命中中用固定种子 `20261018` 随机抽 20 条（`random-sample-20.csv`；第 216 条 `test_connected` 是后来人工补入的，不在抽样框内）：

- 误报 **18/20 = 90%**（95% Wilson 区间 69.9%–97.2%）。
- 可疑 2/20，都是 bb8-memcached（已知实例）。
- **新目标 0/20**。

样本里最有代表性的误报类型：`File::close`、`JoinHandle::abort`、`processed_count`/`count` 这类局部计数、`RwLock::write().await` 后面的 `dirty=true`、`Transaction::drop` 已经兜底的 commit/rollback、mysql_async `routine` 在 await 前已置 `disconnected=true`。完整 20 行及理由见 `random-sample-20.csv`。

样本很小，90% 只能说明“扫描器单独使用时误报很多”，不能外推为所有命中的精确误报率，也不能用来估计召回率。

### 3.4 为什么误报这么多 / 哪些形态其实是安全的

复核确认了以下几类“看起来像、实际安全”的形态，下一轮可以先排除：

1. **POST-await 的局部状态更新**：`let msg = recv().await?; count -= 1;`。future 只能在 await 处被 drop；如果 recv 没返回，`count` 不会减，消息也没被消费。只有“await 已经发出请求、效果在服务端生效”才危险。
2. **后台 worker 拥有整个操作**：toasty 的 `exec_plan` 在 `ConnectionTask` 里跑完整个 BEGIN/步骤/COMMIT；调用方 drop 只丢 oneshot，连接回池前 plan 已经结束。sqlx-sqlite 的 worker 也类似（RESCAN 6.4 已注明未验证）。
3. **多路复用驱动**：tokio-postgres、redis `MultiplexedConnection`、cdrs-tokio 的 stream id、klickhouse 的后台 socket、clickhouse-arrow 的后台 IO task。取消只丢 oneshot，回复由驱动按请求 ID 消费/丢弃，不会错位。
4. **Drop 丢弃而不是放回池**：sqlx `Floating` 被 drop 时连接直接销毁、池大小减一；deadpool 的 `UnreadyObject` 被 drop 时对象丢弃；mysql_async 自己的 Pool recycler 检查 `disconnected`；diesel-async `SyncConnectionWrapper` 的 `CancelWrapper` 会阻塞等 `spawn_blocking` 完成。
5. **已有的 `Transaction::drop` guard**：sqlx 的 `commit/rollback` 取消时，`Transaction` 的 `Drop` 会 `start_rollback`，depth>0 时排队 ROLLBACK。扫描器仍会报 B，但人工复核判已防护。
6. **`has_broken` 恒 false 不等于漏洞**：bb8-redis、bb8-diesel、async-bb8-diesel、hdbconnect、rsmq 的 false 是设计选择（多路复用 / 阻塞线程 / 驱动自己处理），需要看驱动层才能判断。
7. **`close()/abort()/shutdown()` 不一定是连接**：文件、通道、任务句柄、子进程、raft 节点、对象存储 writer 都会出现这些词。

这也是为什么 `REVIEW-GUIDE.md` 把“这个 await 是不是网络往返”“被标记的对象是否会被复用”放在前两位；扫描器本身不看所有权。

## 4. 可疑短名单（按下载量排序）

短名单按 **目标** 合并（同一目标的多个命中在 `candidates.csv` 里仍逐条保留），最多 20 个；本轮只有 6 个目标。前 5 个是：

| # | 目标 | 近 90 天下载 | 文件:行号 | 性质 | 适配难度 |
|---:|---|---:|---|---|---|
| 1 | sqlx-mysql 0.9.0 `Pool::begin` | 42,568,773 | `sqlx-mysql-0.9.0/src/transaction.rs:27-31` | 已知 | 低（RESCAN 6.1 已动态复现） |
| 2 | bb8 0.9.1 `Pool::get` | 9,267,014 | `bb8-0.9.1/src/inner.rs:110-124` | 已知 | 中（RESCAN 4.2 已用假连接复现） |
| 3 | lettre 0.11.23 池化 SMTP | 7,672,068 | `lettre-0.11.23/src/transport/smtp/client/async_connection.rs:184-208`, `:271`; `async_transport.rs:75` | 已知 | 低（RESCAN 6.2 已动态复现） |
| 4 | diesel-async 0.9.2 MySQL `is_broken` / bb8 manager | 5,304,616 | `src/pooled_connection/mod.rs:209-211`; `src/mysql/mod.rs:366`; `src/pooled_connection/bb8.rs:90`; `src/pooled_connection/deadpool.rs:96-103` | **新线索** | 中（需 MySQL/MariaDB + 延迟首包代理） |
| 5 | deadpool-memcached 0.5.0 `recycle` | 840 | `deadpool-memcached-0.5.0/src/lib.rs:46-51` | 已覆盖 | 低（RESCAN 5.2 D4 已动态复现） |
| 6 | bb8-memcached 0.7.0 | 132 | `bb8-memcached-0.7.0/src/client.rs:143-266`; `src/lib.rs:91` | 已覆盖 | 低（RESCAN 4.2 已动态复现） |

### 4.1 sqlx-mysql 0.9.0 `begin`

- **为什么可疑**：`conn.execute(statement).await?` 完成 BEGIN 往返后才 `transaction_depth += 1`；`start_rollback` 在 depth=0 时是 no-op。取消落在 BEGIN 往返时，池把连接拿回去，depth 仍是 0，服务端却在事务里。下一位通过池做的自动提交写入会落进这个永远不会提交的事务。
- **建议动态场景**：Op1 = `Pool::begin()`；用代理/延迟服务端把 BEGIN 的回复扣住，在等待中 drop future；Op2 = 通过同一池执行一条有副作用的 `INSERT`；独立观察者 = 另一条数据库连接查行数（应为 1，实际 0）。RESCAN 6.1 已经是这个场景。
- **适配难度**：低。已有 MariaDB/Mailpit 之外的最小复现，补丁 `patches/sqlx-mysql-begin.diff` 已在仓库。

### 4.2 bb8 0.9.1 `Pool::get()`

- **为什么可疑**：`is_valid(&mut conn).await` 期间连接已经包成 `PooledConnection`、`state` 仍是 `Present`；只有 `Err` 分支才置 `Invalid`。取消（调用方 drop，或 bb8 自己的 `connection_timeout`）时 `PooledConnection::drop` 以 `Present` 调 `put_back`，未验证完的连接直接回池。单流 manager 的下一位会读到验证请求的残留回复。
- **建议动态场景**：Op1 = `pool.get()`；manager 的 `is_valid` 做一次有序请求/应答（如 `version`/`NOOP`）；用代理扣住验证回复，drop `get()` future；Op2 = 下一位借用人发一个与残留回复可区分的请求（如 `ECHO`）；独立观察者 = 服务端收到的字节流 / 另一条连接。RESCAN 4.2 用 bb8-memcached 做过 2/2 违反。
- **适配难度**：中。需要一个“有序请求/应答”的假 manager 或回环代理；bb8 PR #325 的回归测试 `test_drop_during_is_valid` 可直接复用。

### 4.3 lettre 0.11.23 池化 SMTP

- **为什么可疑**：`try_smtp!` 只在命令失败时调用 `abort()` 置 `panic`；`has_broken()` 只看 `panic`。取消在 `MAIL/RCPT/DATA/message` 的 await 时不会置位，`PooledConnection::drop` 把连接回收进池。取消在等 `354` 时服务端已进入 DATA 状态，下一次 `NOOP` 读到残留的 `354` 并判健康，B 的 `MAIL FROM` 被当正文，B 挂起。`test_connected` 的 `NOOP` 还接受 3xx（`response.rs:126-131`），是同一链路的第二道宽松关口。
- **建议动态场景**：Op1 = 池化 `send()`，代理扣住 `354`；drop future；Op2 = 同一池再发 B/C 两封（每封 3s 超时）；独立观察者 = Mailpit 的实际收件箱（主题按进程/运行打标签）。RESCAN 6.2 的 L1/L2 已做。
- **适配难度**：低。已有 Mailpit 与补丁；`test_connected` 的 3xx 语义可以单独用假 SMTP 服务器做单元级验证。

### 4.4 diesel-async 0.9.2 MySQL `is_broken`（新线索）

这是本轮唯一不在已知清单里的高价值目标。静态链条如下：

1. `diesel-async-0.9.2/src/pooled_connection/mod.rs:209-211` 的默认 `is_broken` 只调用 diesel 的 `TransactionManager::is_broken_transaction_manager`，也就是只看 `TransactionManagerStatus`。
2. `diesel-async-0.9.2/src/mysql/mod.rs:366` 是空的 `impl PoolableConnection for AsyncMysqlConnection {}`，**没有覆写** `is_broken` 去查 mysql_async 的 `disconnected`。对比之下 Postgres 在 `src/pg/mod.rs:1059` 覆写为 `... || self.conn.is_closed()`。
3. `mysql_async-0.37.1/src/conn/mod.rs:405-411` 的 `routine` 在 `f.call(&mut *self).await` **之前**置 `self.inner.disconnected = true`，只有 `Ok`/非致命 `Server` 错误才恢复 false；future 被 drop 时保持 true。
4. 取消若落在 `read_result_set` 的 `read_packet().await`（`src/conn/routines/helpers.rs:44-73`）等首包期间，`pending_result` 仍是 `None`（`set_pending_result` 在读到首包之后才调用）。之后 `clean_dirty`（`src/queryable/mod.rs:105-111`）→ `drop_result` 查不到待处理结果，旧回复留在 socket 上。
5. bb8 manager 在归还时调 `has_broken`（`src/pooled_connection/bb8.rs:90-92`）→ 上述默认 `is_broken` → false，连接以“健康”回 idle。下一次 `get()` 的 `is_valid` 用默认 `RecyclingMethod::Verified` 发 `SELECT 1`（`mod.rs:52-64`、`bb8.rs:86-89`），`clean_dirty` 仍看不到 pending，于是**把旧查询的回复当成 `SELECT 1` 的结果**；如果旧回复是一列整数的结果集（例如 `SELECT 12345`、`SELECT SLEEP(2)`），`get_result::<i32>` 会接受，ping 通过，ping 自己的回复又留给下一位。deadpool manager 的 `recycle` 同样先 `is_broken` 再 `ping`（`src/pooled_connection/deadpool.rs:96-103`），机制相同。
6. 为什么 mysql_async 自己的 Pool 不受影响：它的 recycler 检查 `conn.inner.disconnected`（`mysql_async-0.37.1/src/conn/pool/recycler.rs:90`），会丢弃；diesel-async 绕过了这个 Pool，直接持有 `mysql_async::Conn`，所以这个检查没有生效。

**建议动态场景**：

- 目标调用：在 diesel-async 0.9.2 + bb8（或 deadpool）池上执行 `SELECT 12345`（或 `SELECT SLEEP(2)`，一列整数），在首包到达前 drop future。用回环 TCP 代理扣住第一条查询的完整回复，确保取消发生在 `read_packet` 等首包期间。
- Op2：同一池上执行 `SELECT 67890`（或一条写操作）。
- 独立观察者：另一条 MySQL 连接读 `SELECT 67890` 的结果 / 查询日志；如果 Op2 返回 `1`（ping 的回复）或协议错误，就说明 ping 读到了旧回复。
- 停在挂起点的手段：代理延迟首包；或让服务端 `SELECT SLEEP(2)` 后立即取消，但代理更确定。
- 适配难度：中。需要 MySQL/MariaDB、diesel-async 0.9.2、bb8/deadpool、一个能延迟首包的代理；不需要改库。**本轮未做动态验证**，静态链条完整但尚未证明可触发。

### 4.5 deadpool-memcached 0.5.0 `recycle`

- **为什么可疑**：`recycle` 只做 `conn.version().await`，`Ok(_) => Ok(())`；async-memcached 的 `version()` 只要求响应行 ≥8 字节并截掉前 8 字符，不校验 `VERSION ` 前缀。`STORED\r\n` 之类的残留也能通过。用户中途取消后 `Object::drop` 无条件放回，错位连接一直留在池里。
- **建议动态场景**：Op1 = 池化 `get`/`set` 请求中途 drop；Op2 = 下一位做 `set/get(A)/get(B)/get(own)`；独立观察者 = memcached 本身 / 另一条连接。RESCAN 5.2 D4 已做（4 计划 2 违反）。
- **适配难度**：低。已有 memcached 1.6 与 D4 场景。

### 4.6 bb8-memcached 0.7.0

- **为什么可疑**：8 个方法只在自身 timeout 的 `Err(_elapsed_err)` 分支调用 `taint()`；外部 drop future 不会置 `tainted`，`has_broken` 仍 false，bb8 把连接放回池。单条 memcache 流上的残留 `VALUE/END` 让下一位读到前一条回复；memcache-async 解析 `get` 不核对 key，会静默返回别的 key 的值。
- **建议动态场景**：Op1 = `set/get/get_multi` 中途 drop；Op2 = 下一位 `get` 自己的 key；独立观察者 = 真实 memcached 的 key 值。RESCAN 4.2 已做（2/2 违反）。
- **适配难度**：低。已有场景与 issue 报告。

## 5. 不确定 / 待补源码

这些没有进“可疑”短名单，因为缺一个关键证据；写在 `candidates.csv` 的 `不确定` 行里，下一轮补源码即可收敛：

| 命中 | 目标 | 缺什么 |
|---|---|---|
| H074 | bb8-oracle 0.3.0 `is_valid` | `spawn_blocking(move || conn_clone.ping())` 克隆 `Arc<Connection>`；取消只丢 JoinHandle，bb8 已把连接放回池。是否损坏取决于 `oracle` 0.6 / ODPI-C 的线程模型。`oracle` 0.6.3 源码已下载，但 ODPI-C 的并发保证未核实。 |
| H077 | bb8-tiberius 0.16.0 `has_broken` 恒 false | Cargo.toml 要求 tiberius 0.12；已补下载 0.12.3。0.12.3 有 `flush_stream`（清空缓冲并排空未处理包），但没看到 0.13.0 的 `poisoned` 守卫。取消在写一半时会不会留下半包，未核实。 |
| H088 | deadpool-tiberius `recycle` | `simple_query("")` 接受任意 Ok；同 H077 的 tiberius 版本/半包问题。 |
| H113 | hdbconnect_async 0.33.0 | `has_broken` 恒 false；`is_valid` 只轮询 `is_broken`。已补下载 `hdbconnect_impl` 0.33.0：`is_broken` 只是 `tcp_client.has_died()`（`src/conn/connection_core.rs:636-638`），socket 活着就判健康；取消是否留下未读回复需要 HANA 服务/协议细节，未核实。 |
| H169 | rbdc 4.9.10 | `check` 做 `ping().await` 并接受 Ok；rbdc-mysql 的 ping 是单流 `send_packet(Ping)+recv_ok()`，取消会留未读 OK。但 `rbdc` 本体的 `check` 没有调用点，真正的池在未扫描的 `rbdc-pool`。 |
| H209 | taos-query | `has_broken` 忽略连接参数，读 builder 的 `ready()`；r2d2 是同步池，没有 drop 取消。语义最差，但不属于本轮取消目标；未纳入短名单。 |

**不同类别（值得单独做，但不是“池化连接复用”）**

- **object_store 0.14.2** `src/upload.rs:232-239`：`finish()` 在 `Err` 分支 `abort()` + `tasks.shutdown()` + `upload.abort()`；取消在 `upload.complete().await` 时不走 abort，S3/GCS 会留下未完成的 multipart upload。复核判为误报（没有连接复用），但它是真实的“Err 清理 / 取消不清理”不对称，适合按 FOLLOWUP 里“对象存储分块上传”这一类单独测：Op1 = 分块上传的 `finish()`，代理扣住 complete 回复，drop；观察者 = S3 `ListMultipartUploads` / bucket 生命周期。难度中。
- **pict-rs / autumn 的池化连接**：复核过 `queue_length()` 的 5s 内部 timeout、UI 持 `conn` 跨两次查询等；这些是 deadpool/diesel-async + tokio-postgres 多路复用，取消只丢结果，未发现连接错位。可作为“内部 timeout 作为取消源”的对照，不列入短名单。

## 6. 局限与未完成

1. **没有动态测试。** 所有结论是静态的；diesel-async 的新线索没有实际触发，不能称为已确认漏洞。
2. **只扫了 152/815。** 按下载量取前 150 + 强制项；低下载量 crate 里仍可能有 bb8-memcached 这类问题（它只有 132 次/90 天）。全集尾部未逐一下载/复核。
3. **只看 `src/`。** 没有扫 tests/examples/benches；测试代码用属性/`cfg(test)` 过滤，仍可能有漏网或误杀（`test_connected` 就是漏杀后补的例外）。
4. **没有类型/MIR/feature 信息。** 字段是不是连接状态、`Drop` 是丢弃还是放回池、feature 开关是否启用，都是人工读代码判断；扫描器只给候选。
5. **版本缺口。** tiberius 0.12.3、oracle 0.6.3、hdbconnect_impl 0.33.0 已补下载，但没有做完整的二次分析；rbdc-pool、taos 的驱动源码没有。
6. **随机样本只有 20 条。** 90% 误报率的不确定区间很宽；它只描述“这 20 条”，不是全库精确率，也不是召回率。
7. **没有核对上游是否已知。** 除仓库里已有的 issue/PR 记录外，diesel-async、hdbconnect、rbdc、taos 等线索是否已有公开报告，**未核实**；本轮也没有联系维护者。
8. **工作区里有本任务之外的未提交改动**（`Cargo.toml`、`src/`、`prospective/scan-clients/tests/` 等）。本任务的写入只发生在 `prospective/candidate-scan/` 下；那些改动不是本扫描产生的。

## 7. 复现

```bash
# 一键：发现 → 下载 → 扫描（写入 ../hits.csv）
bash prospective/candidate-scan/scanner/run.sh

# 只重跑扫描（源码已下载）
python3 -I prospective/candidate-scan/scanner/scan.py \
  --root ~/dropwise-scan-src/src \
  --out prospective/candidate-scan/hits.csv

# 合并人工复核（需要 reviews/reviewed-*.csv）
python3 -I prospective/candidate-scan/scanner/merge_reviews.py

# 重建四个校准目标并扫描（会重置 ~/dropwise-scan-src/calibration）
bash prospective/candidate-scan/scanner/calibrate.sh
```

`collect.py` 的 API 响应缓存在 `~/dropwise-scan-src/api-cache/`；源码在 `~/dropwise-scan-src/src/`；两者都不在仓库内。`run.sh` 不运行任何下载的代码。

## 8. 文件

| 文件 | 内容 |
|---|---|
| `scanner/collect.py` | crates.io 发现、元数据、`.crate` 安全解压 |
| `scanner/scan.py` | Rust 词法/结构扫描，模式 A/B/C |
| `scanner/merge_reviews.py` | 把分组复核合并成 `candidates.csv`，折叠 `已防护` |
| `scanner/run.sh` | 一键发现 + 下载 + 扫描 |
| `scanner/calibrate.sh` | 四个校准目标（含补丁版）重建并扫描 |
| `universe.csv` | 815 个候选、下载量、来源、是否覆盖/选中 |
| `hits.csv` | 扫描器原始命中（216 条） |
| `candidates.csv` | 216 条命中 + 复核结论 + 理由 |
| `calibration-hits.csv` | 7 个校准目录的扫描结果（41 条） |
| `random-sample-20.csv` | 随机 20 条及逐条误报/可疑判定 |
| `reviews/REVIEW-GUIDE.md` | 复核四问与结论标准 |
| `reviews/worklist-*.csv` | 分组输入 |
| `reviews/reviewed-*.csv` | 分组复核输出（含 `reviewed-G-manual.csv` 的 `test_connected` 补录） |

## 9. 与已知记录的差异

- **lettre PR #1171** 修的是 `sending` 悲观标记、`has_broken`、`abort`；本扫描同时指出 `test_connected` 的 NOOP 仍接受 3xx，补丁版仍命中。这不是说 PR 不成立，而是说明“修了主路径”不等于“验证关口也收紧了”。
- **sqlx-mysql begin** 与 RESCAN 6.1 一致；commit/rollback 的 B 命中在复核中判为已防护（`Transaction::drop → start_rollback`），没有把它们算成新问题。
- **bb8-memcached / deadpool-memcached / bb8** 的结论与 RESCAN 4.2、5.2、6.2 一致；本轮没有重复动态测试。
- **diesel-async** 是新线索：它绕过了 mysql_async 自己 Pool 的 `disconnected` 检查；静态链条完整，但是否能稳定触发、是否已有上游报告，均未核实。

## 10. 最小结论

如果只问“这次扫描有没有找到新的、值得马上动态测的目标”：**有 1 个**——diesel-async 0.9.2 的 MySQL `is_broken` + bb8/deadpool manager。其余可疑目标都是仓库里已经记录/复现过的实例。扫描器对“Err 置坏 / 取消不置坏”的召回是够用的（4/4），但单独使用时误报率约 90%，必须配合所有权/复用路径的人工复核；这也是本轮方法上最确定的结论。
