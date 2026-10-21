# 写场景指南：给 AI 执行者的操作手册

> 状态：**Phase 4 专属规则仍是草稿，尚未冻结**（2026-10-19）；§2.5 的通用方法在 2026-10-21
> 整理时补充。本文件以方法为主，实验数字只作来源，引用处给出路径。
> 适用对象：为 phase 4 的单个目标写 B1/B2/B3/M1 场景的 AI 执行者；§2.5 也适用于其他阶段的
> 场景设计。目标名单和实验方案见 `prospective/phase4/PLAN.md`、`prospective/phase4/targets.md`。
> 每条规则后面的括号注明它来自哪次实验；第 9 节有汇总表，第 10–11 节规定直接句柄和 Python 的不同写法。
>
> **读法：** §2.5 是通用设计选择；§3–§9 是当前 Phase 4 的正式协议（B1/B2/B3/M1、同骨架、每项
> 3 轮等），§10–§11 是直接句柄和 Python 的写法。两者冲突时，正式实验以 §3–§9 为准。通用方法
> 不暗中修改实验协议，也不把探索性结果混进正式 Phase 4。

## 0. 先理解你要回答的问题

你要写的不是“这个客户端有没有 bug”的测试，而是一个**交接边界实验**：把目标操作停在某个
`Pending` 点并取消，然后让同一个池/连接的下一个使用者（幸存者）做一次有副作用的操作，
再由一个**不经过被测池**的观察者确认实际效果。引擎会用两条基线（操作从未开始
`pre`、操作完整完成 `post`，各采样两次）判断幸存者的观察是否等于允许集合中的某一个。

你写的场景决定实验能看到什么。引擎只负责枚举挂起点；看不到的东西不会因为跑得多而出现。

## 1. 开工前先定三件事

1. **目标操作**（最多两个）：先找“建立会话状态”的一次往返（事务、checkout、握手、
   `USE`/`SET`、`SELECT` 邮箱等），再找一个普通的一问一答请求。没有会话状态调用就只写
   一个普通请求。来源：phase 3 `PLAN.md` §2；`docs/choosing-targets.md` “Targets” 清单。
2. **复用单元和池/句柄**：确认这个目标真的会复用连接。直接句柄按第 10 节，不要求新增 manager。自带池的，把池大小设为 1；需要 manager
   的，用一个明确的 manager crate，并把 manager 版本记下来。来源：phase 3 `neo4rs`
   `max_connections(1)`（`RESULTS.md` §2.1），phase 3 各数据库池均设为 1
   （`RESULTS-db.md`）。
3. **观察者**：一条独立连接、独立进程或服务端自己的记录，**不经过被测池，也不经过
   Pending 代理**。来源：phase 3 HTTP 的独立服务器记录（`RESULTS-http.md` §1.4）、
   `neo4rs` 的独立 `Graph`（`RESULTS.md` §2.2）。

如果这三件事里有任何一件确认不了，先停下来报告，不要用猜测补上。

## 2. 最小场景骨架（Rust；Python 见第 11 节）

下面是一个最小形状，实际写法参考 `prospective/neo4rs/tests/neo4rs.rs`（池化读写、独立
观察者、M1-residue）和 `prospective/phase3/http-clients/tests/support/harness.rs`
（五个 HTTP 客户端共用的骨架）。**不要直接复制某个目标的代码**；每个目标的 setup 和观察
值不同。

```rust
use dropwise::{explore, explore_observed, Config, Ctx, Expect, Flavor, Race, Settle};
use std::time::Duration;

// 每个 run 都重新构造：池、唯一 key、服务端状态。
async fn setup() -> Result<Env, String> { /* ... */ }

// 取消目标操作。返回是否真的被取消。
async fn cut(ctx: &Ctx, env: &Env, op: Op) -> Result<bool, String> { /* ... */ }

// 幸存者：同一个池/连接、有副作用、碰被取消操作碰过的同一个对象。
async fn survive(env: Env) -> Result<Obs, String> { /* ... */ }

// B1：取消后立即结束；只判 panic / 超时 / obligation 泄漏。
fn b1() {
    let report = explore(&config(), |ctx| async move {
        let env = setup().await?;
        let _ = cut(&ctx, &env, Op::Primary).await;
        Ok(())
    });
    ladder::simple("B1", "OP1", &report);
}

// B2：同参数重试；只判 Ok。
// B3：换一个同类操作；只判 Ok。

// M1：校准 + 允许集合 + 幸存者 + 独立观察者。
fn m1() {
    let report = explore_observed(&config(), Expect::Atomic, |ctx| async move {
        let env = setup().await?;
        cut(&ctx, &env, Op::Primary).await?;
        let obs = survive(env).await?;
        Ok(obs) // Obs 必须实现 PartialEq，且包含具体值
    });
    ladder::observed("M", "OP1", &report);
}

fn config() -> Config {
    Config {
        flavor: Flavor::MultiThread { workers: 1 },
        races: vec![Race::Immediate, Race::AfterWake],
        max_cancellations: 1,
        max_runs: 64,
        scenario_timeout: Duration::from_secs(20),
        settle: Settle { tokio_time: Duration::ZERO, ..Settle::default() },
        ..Config::default()
    }
}
```

本节 Rust 配置必须和 phase 3 保持一致，方便合并比较：`Flavor::MultiThread { workers: 1 }`、
`races = [Immediate, AfterWake]`、`max_cancellations = 1`、每项 3 轮。`max_runs = 64`、
`scenario_timeout = 20 s`、`Settle { tokio_time: 0 }` 是 `neo4rs` 的完整取值，phase 4 沿用。
来源：phase 3 `PLAN.md` §3；`neo4rs/RESULTS.md` §2.2。

## 2.5 四项设计选择（通用方法）

这一节回答“场景怎么写才能看到问题”，不改变 §3 的 Phase 4 协议。四项可以单独使用，也可以组合；
不需要每个场景都跑满所有组合。更完整的动机见 `research/theory/THEORY.md` §2、§4。

### A. 分开观察目标进度、后继正确性和资源状态

**解决什么问题：** 被取消的操作可能合法地部分完成；后继可能拿到自己的结果，也可能拿到旧结果；
资源可能已经恢复、暂时不可用，或者仍然带着未完成状态。把三者压成一个“通过/失败”，就看不出
是哪一类问题，也容易把合法的部分进度误报成违规。

**怎么使用：**

- **目标进度：** 用 pre/post 两条基线判断被取消操作“没发生/完成”；多 cut 时还有 mixed。用
  `Expect::{Atomic, Completed, Undone}` 声明调用上下文允许哪一侧。`Expect` 是声明，不是推断。
- **后继正确性：** 让后继返回具体值，而不是 `is_ok()`；另设独立观察者确认实际效果。
- **资源状态：** 单独记录连接、事务、协议位置、未读字节、锁、邮箱元数据、任务数等。不要用后继
  `Ok` 代替资源状态，也不要用资源“还在”代替后继拿到了自己的结果。
- 至少给这三类各一个字段；如果某一类不在观察范围，在报告里写明覆盖不到什么。

| 判断 | 现有支持 | 边界 |
|---|---|---|
| 目标进度 | `explore_observed` + `Expect` + `Verdict`；每侧两次采样；多 cut 有 `Mixed` | 只支持 pre/post/mixed。任意前缀、子集、恢复等价不是现成的 `Expect` 取值 |
| 后继正确性 | 场景返回值 + 独立观察者；观察值实现 `PartialEq` | 没有自动 oracle；具体值、错误文本、超时都要自己写 |
| 资源状态 | `Obligation`、场景不变式、`after_settle`、`Settle`；可选的 `net::Proxy::residue` | 没有通用静止判定；`residue` 只适用部分本地 TCP 协议 |

**前缀和子集怎么办：** 如果契约允许前缀（`write_all`）或子集（一次操作有多个可独立生效的效果），
不要用 `Expect::Atomic` 硬套。在场景不变式中显式检查允许条件，例如实际内容是否为输入前缀；
需要延后检查时可使用 `after_settle`。仅把观察值写成“已写入的字节数 + 内容”，仍然会按
pre/post 端点相等比较，不会自动接受合法中间态。这是场景表达，不是 `Expect` 的新取值；未来可以把常见模板
收进 API，但当前不能假设它已经存在。

**已有例子：**

- `R3.5`/`R3.6` 用允许集合区分“事务在提交前被取消”和“两条自动提交写入只完成一条”；只和
  “正常完成”比较会把 6 个干净回滚误报成失败（`prospective/RESCAN.md` §3）。
- mysql_async MY2 的后继 `Ok` 是真的 `Ok`，但值是上一次查询的 12345；目标进度、后继正确性和
  资源状态必须分开看（`prospective/phase3/RESULTS-db.md`）。
- SMTP 的 `Ok(354)` 是“消息已投递、返回值错”；IMAP 的 retry SELECT 是“操作序列恢复、元数据
  不干净”。两者都说明只看一个 `Ok` 不够（`prospective/phase4-exploration/RESULTS.md` §2、§3）。

**适用条件：** 被取消操作可能合法部分完成，或者需要区分“有效果但返回值错”与“有效果且返回值对”
时，必须分开；如果只关心后继有没有拿到自己的结果，可以省略目标进度，但要在报告里说明。

### B. 系统化选择后继

**解决什么问题：** 同一个根因在不同后继下可见性不同。只做“同参数重试”，会漏掉错位回复；只做
“换一个操作”，会漏掉同一对象上的锁或事务冲突；只碰“自己的”对象，会漏掉被取消操作碰过的那个
对象上的问题。

**怎么使用：** 先选对象身份，再选操作，再决定是否显式恢复。

| 组合 | 回答的问题 | 已有例子 | 注意 |
|---|---|---|---|
| 同对象 + 同操作/参数（原地重试） | 旧状态是否阻止重试，或是否重复生效 | MongoDB MG1 B2 撞上未回滚事务的 `WriteConflict`；sqlx `Pool::begin` 重试 | 可能只看到冲突，不一定看到静默旧值 |
| 同对象 + 不同操作/参数 | 后继是否拿到自己的结果（响应关联） | neo4rs N2 的 `Ok([])`；mysql_async MY2 的 `Ok(12345)`；SMTP 的 `Ok(354)` | 只看 `Ok` 会漏；要具体值和独立观察者 |
| 不同对象 + 同操作/参数 | 资源/池/共享状态是否被污染；对象特定冲突不挡路时还能不能看见 | MongoDB MG1 的 M1 写了另一个文档，没看到同文档冲突 | 可能故意避开冲突，不能反推“同对象安全” |
| 显式恢复后继续（同对象或不同对象） | 恢复动作修好了哪一层，剩下什么 | IMAP retry SELECT 后可 APPEND/SEARCH/FETCH，但 flags 仍脏；BOLT direct 的 `reset()` 可恢复操作；bb8 丢弃连接后重建 | 恢复后可用不等于元数据、协议关联、共享状态都对 |

**对象身份和载荷标记分开。** 对象身份回答“冲突/残留落在哪个 key、行、连接、邮箱”；载荷标记回答
“这个返回值是不是这次请求的”。同一对象 + 可区分标记可同时检查对象关联与结果归属；没有标记时，bb8 的 `PONG`/`PONG` 或
SMTP 的 `354` 这类相同/中间回复会把错位藏起来。标记可以是载荷字段、消息 id、IMAP tag、请求号；同对象探针不要通过改变实体 key 来区分请求；
动态部分要先规范化再比较（见 D）。

**不要写成单一阶梯。** 这四类组合不是逐级增强：B2 能抓到 MG1，M1 抓不到；M1 能抓到 mysql_async
MY2，B2/B3 抓不到；IMAP 的 retry 恢复了操作但没恢复元数据。按要回答的问题选组合，并在报告里
说明没跑哪些组合、为什么。

**适用条件：** 有池/句柄复用、有会话状态、或者协议回复可能错位时，可优先考虑“同对象 + 可区分载荷”及一个对照；具体按操作能力和实验问题选择。不要为了显得完整而跑满矩阵。

### C. 分开立即复用和最终清理

**解决什么问题：** 取消后马上复用，看的是残留状态对下一位的影响；资源最终有没有被释放/回收，
是另一个问题。只立即观察，会把暂态当成泄漏；用长 sleep 等清理，会把“还没到”当成“没发生”。

**怎么使用：**

- **立即复用：** 取消后不等待，直接让后继使用同一对象（`Gap::Now` 或场景默认）。观察旧回复、
  锁、池状态、错误返回。这一层回答“下一位现在能不能正确使用”。
- **最终清理：** 等待覆盖相关清理工作的完成信号（如任务 join、服务端确认或明确的回收完成通知）
  后再观察资源。`after_settle` 只是等待窗口结束后的检查回调，本身不是完成信号。`Settle::tokio_time` / `real_time` 是观察窗口，不是完成证明；`unsettled` 和
  watchdog 要把结论降级。
- 需要探索“回复先到/后到”时，用 `Ctx::gap` + `Config::gap_choices`（单 cut），并记录实际赋值；
  不要用固定 sleep 代替可调度维度。
- 有限等待的结果按实际观察范围写：在窗口内没看到，不等于永远没有；不需要为此展开一大段
  不可能性讨论。

**已有例子：**

- io_uring 的立即观察在修复版上也看到 fd 增加，加 100 ms settle 后才区分暂态与持续泄漏；这是
  “观察点过早”的误报，不是修复版的新 bug（`research/mechanisms/RESULTS.md`）。
- Notify 的 lost-wakeup 需要额外取消时机才能稳定看到；这是“立即/单一计划”覆盖不足的例子。
- neo4rs 在取消后等 100 ms 再取 `residue`；lettre 的 50 ms 复用间隔被改成 `ctx.gap`，用来探索
  回复先到/后到（`prospective/RESCAN.md` §6.6）。
- phase4 exploration 用零 settle + 显式重连/超时，把“复用瞬间”和“清理是否完成”分开记录
  （`prospective/phase4-exploration/RESULTS.md` §1、§2）。

**适用条件：** 只要结论涉及“泄漏/恢复”，就必须有完成信号或明确标注观察窗口；只涉及“复用瞬间
是否错位”，立即观察就够。不要把“等得更久”当作证明。

### D. 保留身份与关联

**解决什么问题：** 只看返回值，无法判断这个值属于哪个请求、哪个对象、哪次操作；跨运行变化的
动态 id 被删掉后，又可能把“这次的结果”和“上次的结果”混在一起。

**怎么使用：**

- 观察值里同时保留：对象身份（key/row/mailbox/connection）、操作身份（请求号/tag/消息 id）、
  载荷标记（唯一值）。
- 动态 id 不要因为跨运行变化就删除；先做符号归一化（剥掉运行前缀、映射成稳定 token）再比较。
  归一化只去掉运行间噪声，不破坏“这次/上次”的关联。
- 如果协议没有 correlation id，用唯一载荷、主题、文档字段代替；不要让两个请求的回包长得一样。
- 报告里写清哪个字段对应哪个请求/效果；不要把“值相等”直接当成“属于这次请求”。

**已有例子：**

- neo4rs 每次运行用唯一 tag，比较前用 `normalize()` 剥掉本次运行的前缀；既让 pre/post 可比，
  又保留“上一条 RECORD 是不是自己的”这个判断（`prospective/neo4rs/RESULTS.md` §2.2）。
- SMTP 每封信带唯一主题/message-id，IMAP 用 tag；bb8 的验证回复恰好也是 `PONG`，相同回复把
  错位藏了起来（`prospective/RESCAN.md` §4）。
- SQLx 用唯一写入 + 独立连接确认，避免只看同一条连接的回显（`prospective/RESCAN.md` §3）。

**现有支持：** `dropwise::support::unique(prefix)` 可生成进程内唯一值；归一化目前是场景侧代码
（例如 neo4rs 的 `normalize()`），不是核心 API。符号归一化、关联候选提取是后续自动化方向，
不是现有实现。

**适用条件：** 有 correlation id、多消息响应、池化复用、跨运行唯一标记时必做；单请求单回复且
返回值唯一时可以简化，但仍要保留对象身份。

### 四项怎么组合

一个最小顺序：选目标（§3.1 和本目录的 `choosing-targets.md`）→ 选后继组合（B）→ 选立即复用还是
完成信号后清理（C）→ 给三类观察各写字段并保留身份（A、D）→ 声明 `Expect`。Phase 4 的 M1 默认
把幸存者放在同一对象上做有副作用的操作，B2/B3 是另外两个对照；这不是唯一组合，也不要求每个
目标都跑完 B 表的所有行。

这四项目前是人工设计规则，不是自动生成算法；后继组合生成、观察字段/关联提示、动态值规范化、
中间效果模板的现状和边界见 `../research/literature/FOLLOWUP.md` 的“后续自动化方向”。

## 3. 七条硬规则

### 3.1 幸存者必须碰被取消操作碰过的同一个对象

**规则：** 幸存者除了做一次自己的有副作用操作，还必须对**被取消操作碰过的同一个 key、
同一行、同一个文档、同一个会话对象**做一次操作；独立观察者也要看这个对象。只碰“自己的”
对象不够。

**为什么：** phase 3 的 mongodb MG1 是唯一一个 B2 抓到、M1 没抓到的情况。被放弃的事务
锁住的是 `_id = "mg1"` 的文档；B2 用完全相同的参数重试，正好撞上 `WriteConflict`；M1 的
幸存者写的是另一个文档 `survivor`，观察者读的也是自己的文档，所以什么也没看到。后来
`neo4rs` 的 M 把幸存者写成“同一个节点 `v = 7` + 独立 `Graph` 读同一个节点”，才抓到
“写成功、同池读静默丢行”。来源：phase 3 `RESULTS.md` “从 MG1 学到的”一节；
`RESULTS-db.md` MG1；`neo4rs/RESULTS.md` §2.2。

**写法：**

- 写目标：对 `key = K` 写 `v = 1`。幸存者：对**同一个 K** 写 `v = 7`，再读回 K，独立
  观察者读 K。
- 读目标：对 `key = K` 读一个已知值。幸存者：对**同一个 K** 读另一个已知值，并写一个
  marker；独立观察者确认 marker。
- 事务目标：让幸存者碰被取消事务碰过的同一行（在超时保护下）。如果这一行被服务端锁住，
  记一次 bounded 超时，不要把它误报成目标本身挂起。
- 不要把“同一个对象”理解成“同一个类型的另一个对象”。MG1 的教训就是 `mg1-other` 不算。

### 3.2 池大小设为 1

**规则：** 有池的目标把最大连接数设为 1（`max_connections(1)` / `max_size(1)` /
`maxPoolSize=1`）。这样可以减少幸存者借到其他空闲连接的机会，但**不保证物理连接仍是旧连接**：
客户端可能合法作废并重建连接，必须记录 reuse/reconnect，不能阻止自愈后声称测的是正常池行为。HTTP keep-alive 客户端没有统一的 max size 参数时，
用 `pool_max_idle_per_host(1)` 或等价的“每个 host 只保留一条空闲连接”配置，并记录实际
配置；不要假设默认值就是 1。

**来源：** `neo4rs` 0.8.0 的 `ConfigBuilder::max_connections(1)`（`RESULTS.md` §2.1）；
phase 3 的 mongodb `maxPoolSize=1`、clickhouse-arrow `max_size(1)`、mysql_async 池
（`RESULTS-db.md`）。phase 3 HTTP 用 `pool_max_idle_per_host(1)` 控制空闲连接
（`RESULTS-http.md` §3.1）。

**直接句柄层（不是纳入规则的例外）：** 如果客户端没有池，只有一个长期持有的连接（phase 3 的 mysql_async MY2 直接
`Conn`），那么“池大小 1”表现为“幸存者必须使用同一个句柄”。这种情况下在报告里明确写
“没有交接边界，只有同一持有者的连续使用”，不要把它和池化目标混在一起统计。

### 3.3 观察者必须不经过被测的池

**规则：** 独立观察者必须是另一条连接、另一个客户端实例、服务端自己的记录，或者另一个
进程。它不能从被测池里借连接，也不能经过 `dropwise::net::Proxy`（否则它和被测对象共享
同一个残留字节窗口）。

**来源：** phase 3 HTTP 的观察者是进程内服务器自己的记录，不经过客户端（`RESULTS-http.md`
§1.4）；`neo4rs` 的 `visible` 是一个独立的 `Graph`，**不经过代理、不经过被测池**
（`RESULTS.md` §2.2）；phase 3 mongodb/clickhouse 的观察者都是直连的独立客户端
（`RESULTS-db.md`）。

**写法：** `setup()` 里先建观察者，再建被测池；观察者的连接串不要经过代理。M1 的观察值
至少要有一个字段来自观察者。若要区分被取消操作是否发生，必须有能区分 pre/post 的字段；
只研究幸存者时可接受退化观察，但须按 §3.5 写明覆盖边界。

### 3.4 低层级把 `Ok` 当通过，M1 必须检查具体值

**规则：** B2 只判“同参数重试返回 `Ok`”，B3 只判“不同操作返回 `Ok`”。M1 的观察值必须
包含**具体的返回值、行数、值、key、服务端可见效果**，不能只有 `is_ok()`。特别地，要能
区分“正确的空结果”和“错误的空结果”。

**为什么：**

- phase 3 mysql_async MY2：B2 的重试返回 `Ok(12345)`、B3 的不同查询返回
  `Ok([12345])`，两者都“通过”；M1 比较值，发现 `SELECT 67890` 拿到了 `12345`。
  来源：`RESULTS-db.md` MY2。
- `neo4rs` N2：8 个计划里有 2 个，B2/B3 返回 **`Ok([])`**——查询本应返回两行，一行都
  没有。`Ok([])` 仍是 `Ok`，所以低层级按规则通过；M1 的 `own` 要求看到具体的行，才报
  违反。来源：`neo4rs/RESULTS.md` §3.3、§7。
- 更一般地，`Ok` 也可能是中间态回复：async-smtp 取消后第二次 `send` 返回 `Ok(354)`，消息
  却已经投递；只判 `Ok` 会把“返回值错、效果已发生”当成成功。这是 phase4 exploration 的
  探索性结果，不并入正式 Phase 4，但说明具体返回码和独立效果要一起看。来源：
  `prospective/phase4-exploration/RESULTS.md` §2。

**写法：**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obs {
    write: Result<(), String>,       // 幸存者写的返回值
    own: Result<i64, String>,        // 同一个池读同一个对象，期望具体的 7
    same_object: Result<Vec<String>, String>, // 同一个对象上的第二证据
    visible: Result<i64, String>,    // 独立观察者，期望 7
    residue: Option<usize>,          // 只有 M1-residue 是 Some
}
```

每个 `Result` 都要带错误文本；超时用 `bounded()` 变成 `Err("... timed out")`，不要让它
把整个 run 挂死。来源：`neo4rs/tests/neo4rs.rs` 的 `bounded`、`Obs`。

### 3.5 pre 和 post 的观察相等时：可以接受，但要知道它意味着什么

**规则：** `explore_observed` 会把同一个场景跑两遍校准：`pre` 在第一次 poll 前丢掉目标
（`after_pending == 0` 的内部切点），`post` 让目标完整完成。如果两次校准的观察相等，
Dropwise 可能把它判成 `Post`（分类时先看 post）并给一条退化提示。这是**允许**的，但只在
下面这个条件下接受：

> 你的问题只是“幸存者有没有拿到正确结果”，而被取消的操作本身是否发生/回滚**不在**观察
> 值里；并且 `Expect::Atomic` 允许“发生了”和“没发生”两种结果。

**什么时候不能接受：** 如果你的结论需要“被取消的操作确实没有生效/确实回滚了”，或者
`Expect::Completed`/`Expect::Undone` 要求某一侧，那么 pre == post 说明观察值没有覆盖这个
问题，必须加一个能区分 pre/post 的观察字段（通常是独立观察者看服务端状态），否则只能
报告“无法判定”，不能报告“通过”。如果契约允许前缀或子集，先按 §2.5 A 显式检查允许条件；不要用 `Expect::Atomic` 把合法的部分进度判成 Neither。

**实例：**

- `neo4rs` N1/N2 的 M：pre 和 post 都是
  `{write: Ok, own: Ok(7), own_tag: Ok([...]), visible: Ok(7)}`，所以 M 只能说明“共享状态
  被污染”，不能区分被取消的写到底有没有提交；这不影响违反判定（违反的运行既不是 pre
  也不是 post）。来源：`neo4rs/RESULTS.md` §3.1、§9。
- phase 3 HTTP：A 不在观察值里，pre 和 post 都是 `{B, C, 服务端记录}`；M 只能看 B/C 是否
  被污染，不能证明 A 是否发生。来源：`RESULTS-http.md` §1.5。
- 反例：mongodb MG1 的 M 因为观察值退化，看不到“事务留在服务端”，只有 B2 撞上
  `WriteConflict`；如果你的目标是事务状态，必须把服务端状态加进观察值。来源：
  `RESULTS-db.md` MG1。

**记录要求：** 在报告里写 `pre`/`post` 是否相等、为什么可以接受、这个退化观察覆盖不到
什么。不要只写“M 通过了”。

**校准样本不一致：** 同一侧（pre 或 post）的两次校准如果观察值不同，Dropwise 判
`Unknown`，不是违反。这通常说明场景里有非确定性（随机 key、时间、并发），先修场景，不要
把它算成目标的问题。来源：`docs/choosing-targets.md` “Observations”。

**`Expect` 怎么选：**

- `Expect::Atomic`：调用方可以放弃操作；观察值必须等于“从未开始”或“完整完成”之一。
  phase 4 的 M1 默认用这个。
- `Expect::Completed`：调用方会重试/继续；只允许“完整完成”。B2 式的场景可以用它，但
  phase 4 的 B2 按 phase 3 的规则只判 `Ok`，不用 `explore_observed`。
- `Expect::Undone`：结果必须交到调用方手里（消息、任务、token）；只允许“从未开始”。
  不要把 `Atomic` 套在“消息丢了但看起来像完成”的操作上。

来源：`docs/choosing-targets.md` “Template”；`THEORY.md` §2。

### 3.6 制造 Pending 的手段，以及各自的适用范围

同一个目标的所有层级（B1/B2/B3/M1，若跑 M1-residue 也包括）**必须用同一种手段**；手段
必须在写场景前定好并记录。来源：phase 3 `AMENDMENTS.md` #3；`PLAN.md` §3。

| 手段 | 怎么做 | 适用 | 不适用 / 注意 |
|---|---|---|---|
| 服务端延迟 | SQL `SLEEP`、服务端脚本 sleep、代理之外的延迟 | 数据库/服务端支持延迟原语（MySQL、PostgreSQL 等） | 很多协议没有 `SLEEP`；不要在检测阶段改服务端实现 |
| 大负载 | 16 MiB 请求体、8 MiB 正文、256 KiB 响应体填充 | 让“写请求体/读响应体”出现 `Pending`；HTTP 的 L2 形状 | 只对调用方 future 真的负责写的客户端有效；hyper/reqwest/isahc 的写发生在内部任务/线程里，调用方看不到写侧边界（phase 3 HTTP `RESULTS-http.md` §1.3、§9） |
| 慢的进程内假服务器 | 服务器在回复头/正文前 sleep；每条回复加 10–40 ms | 协议简单、公开规范明确；HTTP、SMTP 等 | 假服务器必须按协议顺序回复；不能改变协议语义；要记录它是不是真实服务器的模型 |
| `ProxyOptions::reply_delay` | `Proxy::start_with(upstream, ProxyOptions { reply_delay: 50 ms })`，扣住每个 server→client chunk | 服务端没有延迟原语；会话状态操作（`START TRANSACTION`、`USE`、Bolt RESET/RUN） | **真实时间**，只适合 `Flavor::MultiThread`；`Flavor::CurrentThread` 下 Tokio 时间暂停，sleep 不前进。扣住期间回复在代理里，`residue()` 看不到（RESCAN 6.8） |
| `Proxy` 残留探针 | M1-residue 在取消后等 100–200 ms 再读 `Proxy::residue()` | 残留检查（非 phase 4 Q3）；代理能覆盖的本地 TCP 目标 | 残留本身不计入确认问题；只把用户可见结果算数。phase 3 的等待是 100 ms（db，AMENDMENTS #5）或 200 ms（HTTP，AMENDMENTS #11），`neo4rs` 是 100 ms |

**选择顺序：** 有服务端 `SLEEP` 就优先用它（代码少、语义清楚，RESCAN 6.8 的对照说明
proxy 在已有 `SLEEP` 的场景里只是更贵的替代品）；没有就用 `ProxyOptions::reply_delay`；写侧
`Pending` 用大负载；协议简单且可以控制回复顺序时用慢的假服务器。来源：RESCAN 6.8、
phase 3 `AMENDMENTS.md` #3/#9/#10。

**不要做的事：** 不要为了制造 `Pending` 修改被测 crate 的实现；不要在检测阶段打补丁；
不要混用两种手段（否则层级之间不可比）。补丁只用于分诊对照，打在副本上。来源：phase 3
`PLAN.md` §3、`AMENDMENTS.md` #4/#15。

### 3.7 四个层级共用同一个场景骨架，只换取消之后测什么

**规则：** 同一个目标的 B1/B2/B3/M1 使用同一个 `setup`、同一个目标操作、同一个 `Config`、
同一种 Pending 手段；区别只在取消之后：

- **B1**：取消后立即结束，用 `explore`；只判 panic、超时、obligation 泄漏。
- **B2**：取消后通过同一个池/连接，用**完全相同的参数**重做同一个操作；只判 `Ok`。
- **B3**：取消后通过同一个池/句柄做一个**不同的同类操作**（换 key/查询/收件人）；只判 `Ok`。
- **M1**：`explore_observed(Expect::Atomic)`；幸存者做有副作用的操作，独立观察者确认；
  观察值检查具体值。
- **M1-residue（可选，残留补充分析）**：M1 加上取消后等待、`Proxy::residue()` 的最大未读
  字节数；它单独统计，不把“错位但用户结果正确”算成确认问题；不是 phase 4 的 Q3。

**来源：** phase 3 `PLAN.md` §3；phase 3 HTTP 和数据库两套 ladder；`neo4rs/RESULTS.md`
§2.2。B1/B2/B3 的定义也来自 RESCAN 6.7。

**为什么必须同骨架：** 只有同骨架才能逐计划比较“M1 抓到、B3 没抓到”的同一个
`#target@pending:race`。phase 3 的 HTTP 和数据库结果都按这个方式报告；`neo4rs` 的 §7
就是逐计划解释 B3 为什么看不到。

## 4. Rust 场景的步骤（按顺序做；Python 接口见第 11 节）

### 4.1 只读 README、文档和公开 API

在写场景之前，允许读：crate 的 README、CHANGELOG、docs.rs、公开 API 的签名/示例、
Cargo.toml（版本、feature）、服务端镜像文档。**不要读取消相关的实现代码**（连接池的
validation/recycle、取消 guard、reset 逻辑等），也不要用“我知道它内部怎么错”来设计
切点。这是 phase 3 选目标时的规则，phase 4 继续保持；否则实验就变成“按已知实现写测试”，
不再是前瞻的。

分诊阶段可以读源码解释根因，但要在报告里分开写“检测阶段没读”和“分诊阶段读了什么”。
来源：phase 3 `PLAN.md` §2、`targets.md` 开头；`PREREGISTRATION.md` §2。

### 4.2 选操作，并写下为什么

对每个目标写一到两个操作：

1. 第一个建立会话状态的调用（事务、checkout、握手、`USE`/`SET`、`SELECT` 邮箱）；
2. 一个普通的一问一答请求。

写一句“这个操作让什么状态活过 future”：服务端事务、协议位置、连接上的未读回复、租约。
如果找不到这样的状态，说明目标选错了。来源：`docs/choosing-targets.md` “Targets”。

### 4.3 setup 每个 run 重建，状态唯一，先预热

- `setup()` 在场景闭包里调用，不在测试外面建一次池。校准会把同一个场景跑多次；全局池会
  把上一轮的残留带进来。来源：`docs/choosing-targets.md` “Calibration”；
  `neo4rs/tests/neo4rs.rs` 的 `setup()`。
- 每次 run 用唯一的 key/id/tag（`process id + 原子计数`），不要让上一轮的数据影响观察。
  来源：`neo4rs` 的 `id`/`tag`；phase 3 mongodb 每个 run 唯一数据库。
- 先做一次 warm-up，把握手/第一个请求的 `Pending` 消耗掉，让计划里的 `#0@...` 对应目标
  操作，而不是连接建立。来源：`neo4rs` `setup()` 的 warm-up 注释。
- 服务端状态（表、文档、邮箱）由独立观察者或 setup 建立；不要依赖被测客户端的第一次调用
  来建状态。
- 钉住 crate 版本、manager 版本和服务端镜像 tag/digest，写进 `SCENARIO.md`；用 `Cargo.lock`
  记录实际解析结果。`neo4rs` 的经验：镜像 digest、服务端版本、客户端版本都要能从日志回溯。
  来源：`neo4rs/RESULTS.md` §1.1、§1.2。
- 如果服务端会留下锁/事务，不要在 B1 的取消后做会阻塞的清理（见 4.6）。

### 4.4 写 B1/B2/B3/M1，先编译再跑

- 四个层级写在同一个测试文件里，共享 `setup`/`cut`/`survive`，但每个层级有独立的
  `#[test]` 外壳。参考 `neo4rs/tests/neo4rs.rs`。
- B2/B3 的“重试/不同操作”也要有超时（`bounded`），超时按失败计，不要挂死整个报告。
- M1 的 `Obs` 必须 `PartialEq`；每个字段都要有确定性的错误文本；不要把 `Instant`、
  随机数、未规范化的 tag 放进观察值。`neo4rs` 用 `normalize()` 把每轮 tag 剥掉，保证
  pre/post 可比较。
- M1 的幸存者步骤按“同一个对象 → 自己的有副作用操作 → 独立观察者”排列；如果某一步失败，
  把失败写进观察值，不要提前 `return Err`（否则校准和违反判定都看不到它）。例外：setup
  失败可以返回 `Err`，那是场景错误。
- 不要用“再等久一点”证明操作完成；用显式的完成信号（服务端记录、独立观察者读到的值、
  协议里的终止回复）。更长的 sleep 只能用来等已发生的效果到达，不能当作判定。来源：
  `docs/choosing-targets.md` “Observations” 最后一条。
- 在第一次检测运行前，把场景文件、目标操作、Pending 手段、池大小、观察值写进
  `SCENARIO.md`；运行后只允许修 harness 错误，且每次修改记录“改了什么、为什么、是否
  已经看过该目标的违反”。来源：`PREREGISTRATION.md` §4；phase 3 `PLAN.md` 的冻结规则。

### 4.5 先跑 reuse 诊断，再解释零

在正式 ladder 之前，跑一个不算进结果的 `reuse` 诊断：连续做三次不取消的请求，打印服务端
看到的连接 id（或等价证据），确认池真的复用了同一条连接。如果池没有复用，零发现不能说明
任何问题。来源：phase 3 `AMENDMENTS.md` #12；`RESULTS-http.md` §6 的阳性对照。

如果目标有已知的阳性对照（例如一个故意在读完回复前把连接还池的客户端），在共享环境里
保留它；它证明观察器能抓到错位。来源：phase 3 HTTP 阳性对照（`control_ladder.rs`，
`RESULTS-http.md` §6）。

### 4.6 处理取消后的清理和锁

- B1 不要在取消后做可能阻塞的 observer 清理。`neo4rs` 最初的 B1 在取消后 `DETACH
  DELETE` 同一个节点，被未结束的 auto-commit 事务持有的写锁阻塞，报成 20 s liveness
  违反；后来改成 B1 不做清理，`explore` 返回后按 id 前缀清理。来源：
  `neo4rs/RESULTS.md` §4.4。
- 数据库/事务目标每个 repetition 用新鲜服务端，或至少用唯一数据库/表；不要在前一个 run
  的未提交事务还开着时 `dropDatabase`。phase 3 mongodb 的 `dropDatabase` 等待被放弃事务的
  锁，客户端超时但服务端 drop 继续跑，后续 run 全部被污染；最终改成每个 repetition 一个
  新容器。来源：phase 3 `AMENDMENTS.md` #14。
- 清理失败要记为 harness 错误，不要算成目标的违反。

### 4.7 记录成本和卡点

每个目标记录：

- 开始/结束时间（墙钟），实际工时；
- 场景代码行数（B1/B2/B3/M1/M1-residue 分开；不含共享环境）；
- 场景改了几次，每次的原因；
- 卡在哪里：环境、制造 Pending、观察者、判定、编译/版本、服务端行为；
- 是否读过取消相关实现（应该是“没有”），分诊阶段读了什么。

这些数字是 phase 4 的 Q2 数据，不是可选的。来源：phase 3 `RESULTS.md` Q2、
`neo4rs/RESULTS.md` §8。

## 5. 分诊：三类结论，只有一类计入确认问题

### 5.1 三类

1. **确认的问题（confirmed）：** 能说明后继者看到了什么用户可见的后果，并且有一个能消除
   它的补丁或配置对照。只有这一类计入 Q1。来源：phase 3 `PLAN.md` §4。
2. **契约外行为（contract-external）：** 库的文档明确不保证这种行为，或者属于协议本身
   不原子。不计入 Q1，但要在结果里单列。来源：phase 3 `AMENDMENTS.md` #16。
3. **测试框架错误（harness error）：** 场景/环境/清理/补丁本身的错误；修好后违反消失。
   不计入 Q1，要记录原因和修复。来源：phase 3 `AMENDMENTS.md` #14/#15；
   `neo4rs/RESULTS.md` §4.4。

### 5.2 确认问题的两个必要条件

- **后继者可见：** 写清楚“下一个使用者看到了什么”：错误值、空结果、`WriteConflict`、
  超时、读不到自己的行、读到上一个请求的结果。不要只写“连接错位了”。
- **可消除的对照：** 在**被测 crate 的副本**上打补丁（`[patch.crates-io]`），或用配置/
  调用方 guard 作为对照；对照运行后违反消失。补丁不能改仓库里的库，也不能进检测构建。
  来源：phase 3 `PLAN.md` §3、`AMENDMENTS.md` #4/#15；`neo4rs/RESULTS.md` §5。

如果只有“残留字节”或“协议位置错位”，但没有用户可见后果，按 M1-residue 的规则单独统计，
不算确认问题。来源：`neo4rs/RESULTS.md` §3.1 的 residue-only 计划；phase 3 `PLAN.md` §3。

### 5.3 契约外行为的例子：mongodb MG1

phase 3 最初把 MG1（事务第一条命令被取消、事务留在服务端、同参数重试
`WriteConflict`）判为确认的问题。复核时读到驱动 README 的 “Warning about timeouts /
cancellation”：丢弃驱动返回的 future “can leave the driver's internals in an
inconsistent state”（RUST-937），用户被要求把 future poll 完，例如在 spawned task 的
`JoinHandle` 上做超时。`PLAN.md` §4 规定文档明确不保证的行为算契约外，因此 MG1 改判为
契约外行为，不计入 Q1；复现、补丁对照和 ladder 数字都保留。来源：phase 3
`AMENDMENTS.md` #16。

**对你写场景的要求：** 在把任何违反标成“确认”之前，先在 README/docs 里搜
`cancel`、`cancellation`、`timeout`、`drop`、`abort`、`consistent state` 一类的说明。
找到明确的不保证，就按契约外处理，并在报告里引用原文和位置。找不到，也不要立刻说
“没有契约”；先看公开 API 文档和 CHANGELOG，再由独立 Claude 会话复核。

### 5.4 执行 AI 初判，独立 Claude 复核，用户裁决分歧

1. 执行 AI 写 `TRIAGE-AI.md`：有效违反计划、完整观察值、契约、根因假设、对照和建议类别。
2. **另一个 Claude 会话**先不看执行 AI 的结论和理由，只拿场景、原始日志、公开文档和
   对照运行事实，写 `TRIAGE-CLAUDE.md`。本会话模拟另一种口吻不算独立复核。
3. 两边交卷后才对账，保留各自原始结论；证据不足记 unresolved，不能强迫确认。
4. 两者不一致、未决、安全/预算例外交用户裁决，记录用户耗时、裁决次数和分歧类型。
   不需要用户对每个目标独立重做一次分诊。双 AI 一致也不代替消除后果的对照证据。

phase 4 Q3 是**两个 AI 的一致率**，主分母为有有效违反且双评完成的目标单元；按 crate/package
聚类另报，零发现的覆盖复核不灌入主一致率。另列用户裁决的请求/完成/未决次数、类型和分钟数，
详见 phase 4 `PLAN.md` §5。预计用户总投入3h、上限6h；超预算未决项保留未决，不能默认通过。
此流程是用户本次决定，不把 phase 3 的人工复核事后改写成 Claude 复核。

### 5.5 已知问题和披露

- 在写“新发现”之前，检索上游 issue/PR（只读）：搜索词包括 crate 名、`cancel`、
  `reset`、`pool`、`reuse`、`desync`。记录命中的编号、标题、日期、是否修复、修复在哪个
  版本。`neo4rs` 的结论就是“已知问题，0.8.0 仍有，修复只在未发布的 main”。来源：
  `neo4rs/RESULTS.md` §6。
- 不要在本轮向任何上游地址发 issue/PR/评论/邮件；上报由项目负责人在看完结果后决定。
  来源：phase 3 `PLAN.md` §5；`neo4rs/RESULTS.md` §6 的范围声明。
- 如果观察到可能涉及安全的问题（下一个使用者读到别的请求的数据），先停下这个目标，只在
  本地记录，不写进任何会公开的文件，交给项目负责人决定披露方式。来源：phase 3
  `PLAN.md` §5。

## 6. 每个目标的报告模板

```markdown
# <target> <version>

## 目标与环境
- crate/version、manager/pool、pool size、服务端镜像/digest、观察者
- 操作 1（会话状态）：...；操作 2（普通请求）：...
- Pending 手段：...；为什么适用
- 检测阶段读过的文档/公开 API（列链接）；没有读取消实现

## 结果
| 操作 | 层级 | plans | violations | unrealized | baseline_errors |
|---|---:|---:|---:|---:|---:|
| OP1 | B1 | | | | |
| ... | | | | | |

## 违反与分诊
### V1: #0@2:Immediate
- 观察值（完整 Debug）
- 幸存者看到了什么
- 根因假设
- 对照：patch/config；运行结果
- 类别：confirmed / contract-external / harness error
- 已知上游报告（编号/日期/是否修复）

## pre/post
- 是否相等；相等时为什么可以接受；覆盖不到什么

## 成本
- 墙钟、场景行数、修改次数、卡点
```

## 7. 常见错误清单

| 错误 | 后果 | 修法 | 来源 |
|---|---|---|---|
| 幸存者碰的是另一个 key/行 | M1 看不到 B2 能看到的问题 | 改成同一个对象，观察者也看它 | phase 3 MG1 |
| 池大小 > 1 | 幸存者拿到干净连接，零发现无意义 | 设为 1；先跑 reuse 诊断 | `neo4rs` |
| 观察者从被测池借连接 | 观察者和被测对象一起被污染 | 独立连接/服务端记录 | phase 3 |
| B2/B3 只看 `Ok` 就当成通过 | `Ok([])`、`Ok(旧值)` 被漏掉 | M1 检查具体值/行数 | `neo4rs` N2、mysql MY2 |
| pre 和 post 相等还写“证明回滚了” | 过度解读 | 写“退化观察”，补一个能区分 pre/post 的字段 | `neo4rs` §3.1 |
| 用全局池/表，不在场景里重建 | 校准互相污染 | setup 放场景内，唯一 id | `docs/choosing-targets.md` |
| 取消后做会阻塞的清理 | 把服务端锁误报成 liveness 违反 | B1 不清理；explore 后按前缀清理 | `neo4rs` §4.4 |
| 每个 repetition 不换服务端/数据库 | 未提交事务锁住后续 run | 新容器/唯一库；不要 `dropDatabase` | phase 3 `AMENDMENTS.md` #14 |
| 把文档明确不保证的行为算确认问题 | Q1 虚高 | 查 README/docs，按契约外单列 | phase 3 MG1 改判 |
| 没跑 reuse 就解释零 | 零发现可能只是没复用 | 先跑 reuse/阳性对照 | phase 3 `AMENDMENTS.md` #12 |

## 8. 参考文件

| 文件 | 看什么 |
|---|---|
| `prospective/neo4rs/tests/neo4rs.rs` | 池化读写、同一对象幸存者、独立观察者、M1-residue、bounded、warm-up、B1 清理 |
| `prospective/neo4rs/tests/probe.rs` | 手工时序 probe（不属于 ladder）：取消时刻、残留字节、后继者读到什么 |
| `prospective/phase3/http-clients/tests/support/harness.rs` | 五个 HTTP 客户端共用的 B1/B2/B3/M1 骨架、L1/L2 形状 |
| `prospective/phase3/http-clients/tests/control_ladder.rs` | 阳性对照：故意错位的客户端 |
| `prospective/phase3/db-clients/tests/mysql_async.rs` | 池路径 MY1 与直接 `Conn` MY2 的对比 |
| `prospective/phase3/db-clients/tests/mongodb.rs` | 事务/会话状态、唯一数据库、观察者 |
| `prospective/phase3/db-clients/tests/clickhouse.rs` | `USE` 会话状态、bb8 `max_size(1)`、观察者计数 |
| `prospective/support/ladder.rs` | `LADDER`/`LADDER-VIOLATION` 输出格式，保证和 phase 3 可比 |
| `docs/choosing-targets.md` | 目标操作和观察的通用原则、`Expect` 的选择 |
| `research/theory/THEORY.md` §2、§5 | 判定条件、允许集合、成本与分诊的分工 |

## 9. 规则来源汇总

| 规则 | 来源实验 | 具体位置 |
|---|---|---|
| 幸存者碰同一个 key/行/文档 | phase 3 mongodb MG1 | `prospective/phase3/RESULTS.md` “从 MG1 学到的”；`RESULTS-db.md` MG1 |
| 三类观察分开：目标进度/后继正确性/资源状态 | phase 3、RESCAN、phase4 exploration | `docs/choosing-targets.md`；`prospective/RESCAN.md` §3；`phase4-exploration/RESULTS.md` §2、§3 |
| 后继组合：同/异对象 × 同/异操作 × 原地重试/显式恢复 | phase 3 MG1、`neo4rs`、phase4 exploration | `phase3/RESULTS.md`；`neo4rs/RESULTS.md` §3、§7；`phase4-exploration/RESULTS.md` §4 |
| 对象身份与载荷标记分开 | `neo4rs` N2、SMTP、MG1 | `neo4rs/RESULTS.md` §2.2；`phase4-exploration/RESULTS.md` §2；`phase3/RESULTS-db.md` |
| 立即复用与完成信号后清理分开 | mechanisms、RESCAN 6.6 | `research/mechanisms/RESULTS.md`；`RESCAN.md` §6.6 |
| 动态值先归一化，不直接删 | `neo4rs` | `neo4rs/RESULTS.md` §2.2（`normalize()`） |
| 直接句柄没有池的关口，责任要单独写 | phase 3 mysql_async MY2、phase4 exploration BOLT direct | `phase3/RESULTS-db.md`；`phase4-exploration/RESULTS.md` §4.6 |
| 池大小 1 | phase 3 数据库目标、`neo4rs` | `neo4rs/RESULTS.md` §2.1；`RESULTS-db.md` |
| 观察者不经过被测池/代理 | phase 3 HTTP、mongodb、clickhouse、`neo4rs` | `RESULTS-http.md` §1.4；`RESULTS-db.md`；`neo4rs/RESULTS.md` §2.2 |
| 低层级只判 `Ok` 会漏 `Ok([])`/旧值 | phase 3 mysql MY2、`neo4rs` N2 | `RESULTS-db.md` MY2；`neo4rs/RESULTS.md` §3.3、§7 |
| pre == post 是退化观察 | `neo4rs` N1/N2、phase 3 HTTP | `neo4rs/RESULTS.md` §3.1、§9；`RESULTS-http.md` §1.5 |
| 服务端 SLEEP 优先 | RESCAN 6.7/6.8 | `prospective/RESCAN.md` §6.7、§6.8 |
| `ProxyOptions::reply_delay` 只适合 MultiThread | phase 3 数据库修订、RESCAN 6.8 | `phase3/AMENDMENTS.md` #3；`RESCAN.md` §6.8 |
| 一个目标一种 Pending 手段 | phase 3 数据库 | `phase3/AMENDMENTS.md` #3；`PLAN.md` §3 |
| 四个层级同骨架 | phase 3、RESCAN 6.7、`neo4rs` | `phase3/PLAN.md` §3；`RESCAN.md` §6.7；`neo4rs/RESULTS.md` §2.2 |
| 先跑 reuse 诊断 | phase 3 HTTP | `phase3/AMENDMENTS.md` #12；`RESULTS-http.md` §6 |
| B1 取消后不清理 | `neo4rs` | `neo4rs/RESULTS.md` §4.4 |
| 钉 crate/manager/镜像版本 | `neo4rs` | `neo4rs/RESULTS.md` §1.1、§1.2 |
| 用显式完成信号，不用更长 sleep | `docs/choosing-targets.md` | `docs/choosing-targets.md` “Observations” |
| 每个 repetition 新服务端 | phase 3 mongodb | `phase3/AMENDMENTS.md` #14 |
| 确认问题要有补丁/配置对照 | phase 3、`neo4rs` | `phase3/PLAN.md` §4；`neo4rs/RESULTS.md` §5 |
| 契约外行为：文档明确不保证 | phase 3 mongodb MG1 | `phase3/AMENDMENTS.md` #16 |
| 执行 AI + 独立 Claude + 用户裁决 | 用户本次 phase 4 决定 | `prospective/phase4/PLAN.md` §5；不改写 phase 3 人工复核记录 |
| 已知问题检索、不擅自上报 | `neo4rs` | `neo4rs/RESULTS.md` §6；`phase3/PLAN.md` §5 |
| 成本/修改次数/卡点记录 | phase 3 Q2、`neo4rs` §8 | `phase3/RESULTS.md` Q2；`neo4rs/RESULTS.md` §8 |


## 10. 直接持有连接句柄：不是“没有池的例外”

这是独立分析层，来源是 phase 3 mysql_async MY2（`RESULTS-db.md`）。适用于用户长期持有
一个连接、依次发多个请求的公开 API；目标操作被取消后，幸存者继续使用**同一个句柄**。
以下只是以后编写场景的方法，本次修订不创建任何目标场景。

- **所有权放在目标操作外。** setup 建立持久连接，目标 future 只借用它；Rust 先销毁借用它的
  future 再让幸存者借用，不能连同连接一起 move 进被丢弃任务。Python 把连接放在 Scenario
  实例上，target 的 Task 被取消不销毁实例。不要用私有 API 抽出内部 socket。
- **不重置边界。** 目标与幸存者之间不得 close/reconnect、重新登录、RESET、回池再借，
  也不能把整个连接丢掉后新建。公开契约要求的恢复动作若要测试，属于另一配置对照，不能
  悄悄加入检测路径。库自行关闭/标坏同一对象时记录真实错误，不伪装成调用方重连后的阴性。
- **恢复动作单独分组。** 如果确实要测 reset/reconnect/retry SELECT，把它作为显式恢复配置，
  记录恢复了哪一层（操作可用、元数据、协议关联、共享状态）。BOLT direct 的 `reset()` 在
  小样本里恢复了操作，但公开文档没有说明 drop future 后的义务；这只能证明“调用方有工具”，
  不能证明库已经把责任写清（`prospective/phase4-exploration/RESULTS.md` §4.6）。
- B1 只取消目标并结束；B2 在该句柄同参数重试；B3 在该句柄换同类请求；M1 在该句柄
  碰同一对象并做有副作用操作，独立观察者绕过这个句柄查实际效果。保持四层 setup/目标/挂起
  手段相同，不把另一个池的“成功请求”充当幸存者。
- 先用三次无取消请求记录对象身份、公开连接标识或服务端连接记录。对象身份相同不证明底层
  TCP 不变；分别报告逻辑句柄存活、旧物理流复用、客户端自发重连。直连层的结论是同持有者
  连续使用，不称“池租约交接”。
- 同 crate/package 有池化和直接模式时，场景/结果/成本分路径记录，跨路径按同一客户端族
  cluster 去重；不重用会相互污染的全局句柄，不把两模式当两个独立 crate。
- 逻辑 Client/Session/Cursor/事务对象不自动等于持久物理连接。公开 API 无法证明单流绑定的
  标为未核实；IMAP tag、Modbus transaction id 等须说明为何该路径只有一个 in-flight 请求，
  不凭编号的存在或缺失推断并发语义。

## 11. Python asyncio：复用已有引擎，不照搬 Rust 的打断机制

方法依据：`research/python-retro/README.md`、`RESULTS.md` §1、§3、§7–8，以及现有
`dropwise_py/` 的公开 Scenario/Report 接口。该目录只读，不修改其引擎或既有案例。
本指南不提供新目标代码、runner 或服务配置；只有未来另获授权才实现。

### 11.1 接口和四层的对应

每个 run 通过 Scenario factory 新建隔离实例，使用现有 `setup`、`target`、`same`、
`different`、`survivor`、`observe`、`close` 接口。setup 保留被测 pool 或持久连接和独立
观察者；`same` 对应 B2、`different` 对应 B3。M1 的 `survivor` 返回具体值/写入与同连接
读回，`observe` 返回独立连接/服务端效果；错误保留类型/文本，不能全部压成布尔值。

- B1：调用现有 `level_b1`，正常 CancelledError 不是违反，其他异常/超时待分诊。
- B2/B3：调用现有 `level_retry`，分别 same/different，只看是否异常/超时，不检查值。
- M1：现有 `explore_observed` + `Expect.ATOMIC`，pre/post 各两次；同侧不一致 Unknown。
  字节、错误和容器结构采用确定性规范化；不把随机 id、耗时或对象地址放进观察。
- 池上限1；直接模式在相同对象上继续。HTTP 限 HTTP/1.1；禁 pipeline/channel/push 并发。
  native 后端即使只有一个外层让出点也照实报，不把它当成库内部状态覆盖。
- 当前正式包版本、CPython 正式版本、池/依赖版本写清；不回退到 python-retro 的旧版来
  提高发现率。库文档的 latest 可能是 dev/beta，版本以 PyPI 正式发布记录为准。

### 11.2 挂起、取消与清理

引擎继承纯 Python `_PyTask`，每次 `_Task__step` 返回且任务仍未结束记一次让出；在第 k 次
后 `Task.cancel()`。这是运行时目标 Task 的让出数 K，不是源码 await 数，不是 Rust poll，
也不自动包含子任务、C 扩展或其他 callback 的内部状态。不要手工给协程 throw CancelledError，
那会绕过当前等待 future、`cancelling()`、shield/uncancel 的生产语义。

取消后把目标 await 到终止，让 finally/except 执行。**不加固定 settle sleep**，不为等旧回复
到齐而推迟幸存者；这种等待会让池 peek/重连掩盖窗口（python-retro §8.4）。目标终止也不
证明 shield 派生任务或远端副作用已结束；记录后台工作与公开完成信号，无法确认就保留缺口。
`close` 在幸存者/观察之后做，不能提前归还直连句柄；清理超时单列，不能算确认问题。

未来每项3轮、每轮每计划3次重复、引擎超时15s。用服务端阻塞/延迟或透明回复延迟等**一种**
挂起手段，各层共用；不为了制造切点修改目标实现，也不复制 retro 的特定 bug 注入。
`Task.cancel()` 与超时异常是不同入口，本期纯取消不混入 timeout-BEGIN 式注入。

幸存者、观察者和清理不得吞掉 runner 的 `CancelledError` 后继续工作；业务异常捕获须
让控制取消传播。python-retro §8.2 已证实：池泄漏加测试侧吞取消也会让 `wait_for` 无法结束，
并非事件循环停止。真正同步阻塞同样会使 asyncio 超时失效。未来 runner 须有独立进程60s
硬截止；硬杀/同步卡住标 incomplete，不把“被杀”直接算目标违反。不能通过排除一个卡点
后仍写“完整枚举”。本轮不写或运行这个 runner。

### 11.3 覆盖先于阴性解释

- 各层、各轮先记录无取消 K 和基线状态，列出实际计划集合、attempted/realized/unrealized、
  Unknown、baseline_errors、排除点与理由、硬截止。K=0 或基线失败写无法评价。
- 现有引擎可能给 unrealized trial 也生成判定，汇总必须显式过滤 unrealized、Unknown、
  基线失败；不能直接用 Report.violations() 的长度当确认数。`exhaustive` 标志也不保证全部
  真实调度和语义窗口都覆盖，必须以具体字段说明。
- 同轮各层的 pending#k 未必对应同一个内部阶段，先给公开运行阶段/挂起来源的对应证据；
  只比较实际对应且 realized 的计划。无法对应就只做层级汇总，不假装逐切点比较。
- **同步 `data_received` 等回调中的状态转移枚举不到**；这是打断模型的局限，不是多跑几次
  可以消除的误差。批量到达、原生 I/O、池自愈、shield 清理也可能缩小可见窗口。
- 每个“未观察到违反”后都附上述覆盖和 reuse/reconnect、pre==post 是否退化；不能写
  “干净”“证明安全”。Python 与 Rust 单独统计，最终只可合并描述性表，不比较原始 trial
  计数或将 Task.cancel 的结果当成 Rust drop 的验证。
