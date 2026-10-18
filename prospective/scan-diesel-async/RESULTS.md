# diesel-async 0.9.2：MySQL 连接在查询中途被取消后被池复用（2026-10-18）

> **已上报（2026-10-19）：** issue [diesel-rs/diesel_async#304](https://github.com/diesel-rs/diesel_async/issues/304)，修复 PR [#305](https://github.com/diesel-rs/diesel_async/pull/305)。上游 main（ecc4b8b，mysql_async 0.37）上同样复现：bb8 默认 2/6、缓存语句 4/6、关闭借出验证 4/4、deadpool 默认 2/8 个取消点导致下一个查询失败；在 main 上加入 PR 的修改后，五种配置全部为 0。PR 附两个回归测试，在 main 上失败、修复后通过（各 3 次），完整测试套件在 `mysql` 与 `mariadb` 两个后端下对 MariaDB 11.4 通过。

扫描器给出的唯一新线索（`../candidate-scan/README.md`）的动态验证。运行入口 `bash prospective/run-scan-diesel.sh`：MariaDB 11.4 只监听回环地址，发布版与补丁版各 3 轮。

## 机制

- mysql_async 在每个命令开始时把连接标记为 `disconnected`，命令正常结束才清除（0.36.2 `conn/mod.rs` 的 `routine`）。被取消的查询会把这个标记留在连接上，回复可能还在流里没读。
- diesel-async 的 `PoolableConnection for AsyncMysqlConnection` 是空实现，默认的 `is_broken` 只看 diesel 自己的事务状态，不看这个标记。bb8 的 `has_broken` 和 deadpool 的回收都依赖 `is_broken`，所以这条连接会被放回池里，留给下一个借用者。
- 上游 main（diesel-rs/diesel_async，已依赖 mysql_async 0.37）仍是空实现；0.37 已经有公开的 `Conn::is_disconnected()`。

## 场景

池大小为 1，先预热。被取消的借用者执行一个查询，服务端用 `SLEEP(0.05)` 把结果推迟 50ms，让查询停在等待结果处（本机 MariaDB 不推迟时，查询在第一次 poll 就完成，没有可取消的点）。幸存者通过同一个池执行一个结果已知的查询，再写一行，由独立连接计数。判定用 `explore_observed(Expect::Atomic)`。

| ID | 池 | 语句 |
|---|---|---|
| DA1 | bb8，默认（借出时验证） | 每次不同的 SQL 文本 |
| DA2 | bb8，`test_on_check_out(false)` | 同上 |
| DA3 | bb8，默认 | 同一条参数化语句，取自语句缓存，只换参数 |
| DA4 | bb8，`test_on_check_out(false)` | 同上 |
| DA5 | deadpool，默认（回收时验证） | 同上 |

## 结果

计划数随真实时间浮动（2–8 个），这里给出每轮的违反数。

| ID | 发布版（3 轮） | 补丁版（3 轮） |
|---|---|---|
| DA1 | 2–4 | 0 |
| DA2 | 2–4 | 0 |
| DA3 | 2–4 | 0 |
| DA4 | 2–3 | 0 |
| DA5 | 1–3 | 0 |

在所有违反中，幸存者看到的只有三种情况：

1. 自己的查询报协议解析错误（`can't parse: buf doesn't have enough data`），后面的写入成功；
2. 同上，但后面的写入报"连接已关闭"；
3. 只在不做借出验证时出现（DA2、DA4）：自己的查询返回"没有记录"，即读到一个空结果；后面的写入报 `packet out of order`。用 `load()` 时，这相当于不报错地返回一个空列表。

**五种配置各 3 轮，幸存者一次都没有读到被取消查询的值（12345）。** 这不能证明读到别人的数据不可能发生，只说明这些形态下没有出现。

## 补丁

`patches/` 里有两个文件，只用于验证根因：

- `diesel-async-mysql-is-broken.diff`：`is_broken` 额外检查 `self.conn.is_disconnected()`；
- `mysql_async-0.36-is-disconnected.diff`：把 0.37 的 `is_disconnected()` 回移到 0.36.2（diesel-async 0.9.2 依赖 0.36，那里没有这个方法）。

在上游 main 上，只需要第一处修改。

## 消融阶梯：B1/B2/B3/M（2026-10-18，单独实验）

运行入口 `LADDER=1 bash prospective/run-scan-diesel.sh`，发布版与补丁版各 3 轮，**三轮的计划数与违反数完全一致**。四个层级用同一个场景骨架、同一个 Dropwise 配置，只在取消之后换动作和判据：

- **B1：** 取消后场景立即结束，用 `dropwise::explore`；不 panic、不超时、没有 Obligation 泄漏即通过。
- **B2：** 取消后通过同一个池，用完全相同的参数重做同一个操作；返回 `Ok` 即通过。
- **B3：** 取消后通过同一个池做一个不同的同类操作（DA1/DA2 换 SQL 文本，DA3–DA5 换缓存语句的参数）；只看 `Ok`，不看值。
- **M：** 现有的 `explore_observed(Expect::Atomic)` 幸存者 + 独立观察者。

表中为“计划数/违反数”。

| ID | 版本 | B1 | B2 | B3 | M |
|---|---|---|---|---|---|
| DA1 | 发布版 | 6/0 | 6/**4** | 6/**4** | 6/**4** |
| DA1 | 补丁版 | 6/0 | 6/0 | 6/0 | 6/0 |
| DA2 | 发布版 | 4/0 | 4/**4** | 4/**4** | 4/**4** |
| DA2 | 补丁版 | 4/0 | 4/0 | 4/0 | 4/0 |
| DA3 | 发布版 | 6/0 | 6/**4** | 6/**4** | 6/**4** |
| DA3 | 补丁版 | 6/0 | 6/0 | 6/0 | 6/0 |
| DA4 | 发布版 | 4/0 | 4/**4** | 4/**4** | 4/**4** |
| DA4 | 补丁版 | 4/0 | 4/0 | 4/0 | 4/0 |
| DA5 | 发布版 | 8/0 | 8/**2** | 8/**2** | 8/**2** |
| DA5 | 补丁版 | 8/0 | 8/0 | 8/0 | 8/0 |

M 在发布版上的违反计划为 DA1/DA3 的 Pending #1、#3，DA2/DA4 的 #1、#2，DA5 的 #4（各两种时机）；补丁版全部判为 Post。

**“M 抓到、但 B3 没抓到”的计划：没有。** B2、B3、M 三者在每个场景、每个版本、每一轮抓到的计划键集合完全相同。原因是：每个 M 违反里，幸存者取消后的**第一条**查询就已经失败——DA1/DA3/DA5 是协议解析错误 `can't parse: buf doesn't have enough data`，DA2/DA4 的 Pending #1 是 `Record not found`，Pending #2 是解析错误；而 B3 的“不同操作”也是取消后在同一个池化连接上发出的第一条查询，读到同一段残留回复，于是同样返回 `Err`。也就是说，这个缺陷在下一个借用者身上表现为**可见的错误**，B3 的 `Ok` 检查已经足够，不需要独立观察者。

唯一接近“静默”的是 DA2/DA4 的 Pending #1：残留回复让下一条查询读到一个空结果。这里用 `get_result`（取一行），空结果是 `Err("Record not found")`，所以 B3 仍能抓到；如果换成 `load()` 形态，空结果会是 `Ok(空列表)`，B3 就会漏掉。本次没有出现该形态。

**补丁版：** 四级均为 0；M 的所有计划都等于 post（DA1–DA5 分别为 6/4/6/4/8 个 Post），没有需要分诊的固定版违反。

两点记录：

- 本轮 DA1–DA4 的发布版违反数都是 4，DA4 高于上文旧表的 2–3。同一轮未改动的 `mysql_cached` 对照组也给出 4，属于实时时间下 Pending 数与命中计划的变化，不是阶梯引入的；旧表保留为当时的记录。
- 计划数在本轮 3 次运行中稳定为 6/4/6/4/8，与旧表“2–8 浮动”的区间不同，同样是环境差异。

## 扣住回复与 SLEEP 对照：DA1、DA3（2026-10-18，单独实验）

运行入口 `PROXY=1 bash prospective/run-scan-diesel.sh`，发布版与补丁版各 3 轮，三轮结果一致。做法：去掉 SQL 里的 `SLEEP(0.05)`，连接改走 `dropwise::net::Proxy`；代理把服务端发来的每一块数据扣住 50 ms 真实时间再转发给客户端（`Proxy::start_with` 的 `reply_delay`），客户端到服务端方向不变。观察者仍直连数据库。

| 变体 | 发布版计划 | 发布版违反 | 发布版判定 | 补丁版计划 | 补丁版违反 | 补丁版判定 |
|---|---:|---:|---|---:|---:|---|
| DA1，SLEEP 对照 | 6 | 4 | pre=0 post=2 mixed=0 neither=4 unknown=0 | 6 | 0 | post=6 |
| DA1，扣住回复 | 6 | 4 | pre=0 post=2 mixed=0 neither=4 unknown=0 | 6 | 0 | post=6 |
| DA3，SLEEP 对照 | 6 | 4 | pre=0 post=2 mixed=0 neither=4 unknown=0 | 6 | 0 | post=6 |
| DA3，扣住回复 | 6 | 4 | pre=0 post=2 mixed=0 neither=4 unknown=0 | 6 | 0 | post=6 |

两种方式的计划数、违反数、Pre/Post/Neither 分布完全相同，违反都落在 Pending #1 和 #3（各两种时机），Pending #2 为 Post。原因是代理虽然把借出验证的回复也一起推迟了，但目标 future 的 Pending 边界数没有变化，取消仍然落在同一批位置上；幸存者读到的残留回复也相同。**没有需要解释的差异。**

场景代码行数（定义：场景闭包 + 包裹它的 `explore_observed` 调用 + `#[test]` 函数，非空且首字符不是 `/` 的行；不含 `Env`/`setup`/`survive`/`config`/报告函数。扣住回复版本另外计入它独有的 URL 重写辅助函数和延迟常量）：

| ID | SLEEP 版本 | 扣住回复版本 | 差值 |
|---|---:|---:|---:|
| DA1 | 20 | 35 | +15 |
| DA3 | 13 | 35 | +22 |

扣住回复版本多出的代码集中在三处：把 `DW_MYSQL_URL` 的主机端口改写成代理地址（14 行辅助函数）、每个场景启动/持有一个 `Proxy`、以及一个 50 ms 的延迟常量。DA3 的 SLEEP 版本复用了 DA1 的通用 `cached_query_cancelled` 包裹函数，所以基数更小；单独看场景闭包，两者只差 SQL 文本里的 `SLEEP` 与参数化形式。

结论：只要服务端能配合延迟，`SLEEP` 更省代码；扣住回复适合**服务端没有延迟原语**（例如 sqlx-mysql S1 的 BEGIN 往返）或不想改 SQL 的场景。它要求 `Flavor::MultiThread` 的真实时间，而且扣住期间回复还在代理里，`residue()` 看不到它。
