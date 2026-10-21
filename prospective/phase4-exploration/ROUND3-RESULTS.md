# 第三轮结果：取消后的状态、恢复义务与复用决定

更新：2026-10-22，完成审查后的当前摘要。未冻结探索，不是正式 Phase 4 数据，
也不是盲测或预注册研究。本文统一替代此前逐轮追加的矛盾口径；原始日志不改写，
具体执行配置及中间过程见各客户端报告。历史数字不能跨不同版本、场景或重跑直接相加。

## 1. 结论

这轮得到的是多个可复现现象、两个有实验修复对照的重点候选，以及一批有限覆盖下
未观察到违反的目标。它支持一个有用的搜索问题：

> 失败或取消打断状态更新后，实际状态是否仍与成功承诺、恢复义务、复用和清理决定一致？

这不是自动 oracle。调用者仍须声明契约，区分正常完成、允许的部分效果、安全拒绝、
恢复义务和真正错误。取消不保证回滚；明确作废坏连接可以是正确结果。

- **Skytable**：响应流未完成，下一次 query 或池验证消费了残留响应；已有原版/修复对照。
  对外仍称复用失败候选：取消契约未明确，上游新颖性未获确认。
- **Linux IPv4 multicast**：socket 侧 membership 已存在，设备组分配失败却返回成功。
  已实测收包失败；关闭这个 socket 还会误删随后成功加入的另一个 socket 所依赖的组。
  有修复对照，是本轮证据最完整的契约违反候选。
- **Python direct 事务**：手写事务取消后未恢复可使后继写入留在未提交事务中。
  它说明应用组合/恢复责任的重要性，不自动证明驱动违反取消契约。
- **阴性/防御正例**：池 rollback/reset、关闭坏连接及重连能阻止污染继续传播。
  阴性不反驳“错误复用会出问题”，也不构成所有调度都安全的证明。

Linux multicast 是不同于已有 eventpoll 的具体缺陷候选，但二者都涉及错误传播与
成功承诺不一致。不能把 multicast 宣称为全新的机制类别；也不能将所有响应错配
候选合并成一个独立 bug。**机制类别、独立根因、客户端家族和执行次数是不同单位。**

## 2. Rust 覆盖与有效结果

表中阶梯数字是违反/执行计划，不是 bug 数。修复后的显式拒绝仍可能被原阶梯标成
违反；必须再按契约分诊，不能为了全绿改 oracle。

| 客户端/模式 | 有效观察 | 当前判断 |
|---|---|---|
| skytable 0.8.12 pooled | 深入批次原版 27/48；修复 0/48 | 有修复对照的复用失败候选 |
| skytable direct | 原版 15/24；修复 18/24 均为显式 ConnectionAborted | 静默错配变成 fail-closed，调用者需丢弃重连；不是 0 违反 |
| tokio-modbus 0.17.0 direct | 每 rep B1 0/2，B2/B3/M1 各 2/2；stale transaction ID，服务端写已发生 | 响应错配候选；契约和修复对照未闭环 |
| async-pop 1.1.3 direct | 每 rep B1 0/8、B2 2/8、B3 6/8；M1 原始 Neither 8/8 | 其中 **2 个 RETR 切点是明确的失败现象，不是阴性**；6 个 login 切点保留契约待分诊 |
| orientdb-client 0.6.0 pool/held session | 主阶梯 0/120；generated 0/124；观察到 2 条物理 TCP 流 | 弱阴性；逻辑 session 数不能代替物理流复用证明 |
| gel-tokio 0.11.0 pooled/single | 96 次取消执行，零违反/Unknown/unrealized；M1 Pre21/Post3 | 有限场景阴性；Post 是合法提交，不表示取消回滚 |
| tiberius | 未执行 SQL Server 场景 | 许可未接受，覆盖缺口 |
| http-client H1Client | 未接入当前 Tokio runner | async-std 运行时边界，覆盖缺口；不是阴性 |

BOLT、IMAP、SMTP 是此前探索结果，不重复计成本轮新目标。

### Skytable 的证据和修复范围

[深入报告](clients/skytable/RESULTS-FRAGMENT-DEEP.md) 区分 direct read-cut、
pooled checkout/validation-cut、pooled update-cut。不同触发链共享响应流未完成这个
问题形态，但不能仅凭症状相似证明所有路径都是同一个代码缺陷。

实验补丁在 query 未完成时置 poisoned，池拒绝继续借出；direct 不自动重放请求。
本次审查发现 `execute_pipeline()` 能绕过 poisoned，并且 pipeline 自己取消也不置位，
已补同一套入口检查和 Drop guard，并加入无服务回归测试。已有 42/72 → pooled 0/48、
direct 明确拒绝的日志属于此前 query-only 补丁；不能拿它声称 pipeline 已做真实服务验证。
当前补丁仍非上游就绪：TLS、pool>1、并发 borrower、协议错误/服务端业务错误、部分写和
完整 pipeline 行为仍需针对性审查。

### POP 的计数纠正

[POP 报告](clients/pop/RESULTS-FRAGMENT.md) 原文的 “clean, unambiguous case”
指校准干净、失败证据明确，不是测试通过。`#1@1` 两种 race 的 pre/post 均成功，
取消后 STAT/RETR 返回错误；3 reps 共 **6 次明确失败现象**。另外 18 次 login-cut
原始 Neither 保持待契约分诊。稳定错误端点本身不意味着校准无效；这里待定的是
认证部分完成后允许什么，以及恢复责任。不要把这 18 次写成已确认 bug 或阴性。

## 3. Python 覆盖与有效结果

| 客户端/模式 | 有效结果 | 分诊 |
|---|---|---|
| psycopg 3.3.6 direct | 修正后 B1/B2/B3 各 0/18，M1 12/18 | 手写事务未回滚的应用组合现象；不是驱动违约的充分证据 |
| psycopg pool | M1 0/18 | release rollback / 丢弃坏连接的正例 |
| asyncpg 0.32.0 direct | 修正后 B1/B2/B3 各 0/9，M1 9/9 | **6/9 后继效果对独立连接不可见，3/9 仅 acquire-time 状态差异**；不能统称永久丢写或已确认驱动 bug |
| asyncpg pool | 各级 0/12 | 当前场景的 reset/复用正例 |
| aiomysql 0.3.2 direct | 修正后 B1 0/9，B2/B3/M1 各 9/9 原始失败 | 明确拒绝已取消连接；安全防御，不计静默污染或库 bug |
| aiomysql pool | 修正批次零违反 | 坏连接不交给后继的正例 |
| mysql-connector-python async direct | B2/B3 显式错误；M1 12/12 原始 Neither | 需拆分具体效果与状态，仍是无修复对照的候选 |
| redis-py 8.1.0 | pool/direct × tx/set 共 120 trials，零违反 | 包含显式 DISCARD 清理，不是“所有 direct 操作无需恢复” |
| pymongo 4.19.0 | txn/plain 两单元共 **72** trials，零违反 | 当前 driver/场景阴性 |
| neo4j 6.4.1 | txn/plain 两单元共 **72** trials，零违反 | 当前 driver/场景阴性；两包合计 144，不是每包 144 |
| aiohttp 3.14.5 | 四级共 12 trials，零违反 | 仅一个外层 suspension，弱覆盖 |
| curl-cffi 0.16.3 | 四级共 12 trials，零违反 | 主阶梯切在发送前；额外 mid-flight probe 不是阶梯 |
| httpx 0.28.1 / httpcore 1.0.9 | B1 0/18，B2/B3/M1 各 3/18 | pool slot 卡住，已有上游相关报告；不是新颖性结论 |
| aiosmtplib 5.1.3 | B1 0/12，B2/B3/M1 各 12/12 | SMTP 残留响应/超时候选；简单错误检测也能看到，不证明 M1 独占增益 |

[PG/MySQL 修正和恢复对照](python/pg-mysql/RESULTS-FRAGMENT-fixed.md)、
[MySQL 新增单元](python/pg-mysql/RESULTS-FRAGMENT-round3b.md)、
[Redis](python/redis/RESULTS-FRAGMENT.md)、
[Mongo/Neo4j](python/mongo-neo4j/RESULTS-FRAGMENT.md)、
[HTTP/SMTP](python/http-mongo-neo4j-smtp/RESULTS-FRAGMENT.md)、
[aiohttp/curl](python/http-mongo-neo4j-smtp/RESULTS-FRAGMENT-ROUND3-TARGETS.md)。

### 事务恢复：观察与解释分开

有效阶梯来源 `python/pg-mysql/logs/run.1HLY38/`，恢复对照 `run.WcLcOp/`。

- 显式 SQL ROLLBACK 和重连在所测 direct 切点恢复了后继可见性。
- psycopg 在已存在的外层事务中使用事务上下文，可能只管理 savepoint；块正常退出
  不能证明外层已提交。这是事务嵌套语义，不应写成 context manager 自身假成功。
- asyncpg 已有手写事务时，事务上下文可明确拒绝，属于可见失败。
- asyncpg k1 的客户端 `in_tx=False`，后继写之后进入未提交事务；`reset()` 对该切点
  没有恢复可见性，显式 ROLLBACK 有效。**“BEGIN 仍在队列里”是对时序的解释，
  不是这些异步采样本身直接证明的驱动内部事实。** 客户端状态和服务端查询不是原子快照。
- 事务上下文覆盖 target 的对照仍有部分切点未恢复，需要单独定位进入/退出时序及清理
  取消，不能直接把手写事务的结论推广为官方事务 API 违约。

“独立观察者暂时看不到未提交写”不是“永久丢写”；状态字段差异也不能单独证明错误。

## 4. Linux：11 个新操作与 multicast 候选

[第四轮报告](../../research/linux/RESULTS-round4.md) 包含 fail-nth/signal 的 11 个
新 target。mmap/mremap/mprotect 的部分进度按接口边界分诊，不纳入确认缺陷；
其他未见违反的目标只对实际注入路径和观察范围成立。

[实际投递与补丁审查](../../research/linux/RESULTS-round4-multicast.md) 的关键对照：

| 场景 | 原版 | 修复版 |
|---|---|---|
| 正常 multicast join | 接收 4/4 | 接收 4/4 |
| N=2 分配失败 | syscall 返回成功，设备组缺失，接收 0/4 | ENOBUFS、socket/设备无残留；接收 0/4 是正确失败 |
| 失败后正常重新加入 | 原版残留会阻碍同 socket 重试 | 正常加入并接收 2/2 |
| A 失败、B 随后成功加入，再关闭 A | B 从接收 2/2 变成 0/2，设备组被误删 | B 保持成员状态及接收 |

原版在 v6.19 和 v7.3-rc2 上有证据，修复对照为 v7.3-rc2。仅 loopback + 故障注入，
没有物理网卡、真实内存压力触发或 IGMP 抓包，不推断发生频率或安全影响。

补丁把设备组分配失败传回 syscall，并在失败时不发布 socket membership。
RTNL 串行化 writer，不排除 RCU reader；“下一次 IGMP query 自动修复缺组”是错误说法。
本次同步修正补丁文件本身的旧注释，而不只更改报告。legacy void wrapper 仍保持
best-effort；该路径、source-filter、通知和 RCU 发布顺序仍是上游准备的审查重点。

## 5. 哪些证据作废，方法增量还能说明什么

[适配器审计](ADAPTER-AUDIT-ROUND3.md) 列出具体目录。旧 PG/MySQL B2/B3 吞错数字、
asyncpg 已释放句柄诊断产生的 target error、旧 POP/GreenMail 卡住批次，不进入有效比较。

- 吞错修复后的 psycopg/asyncpg 对照仍说明：仅检查后继是否抛错会遗漏外部效果不可见。
- aiomysql 的旧 B2/B3=0 是适配器假阴性，不是 M1 优势；修正后简单检测也会报告失败。
- aiosmtplib 的后继错误也无需 M1 才能发现；完整观察增加的是机制解释和效果区分。
- 各 level 多数是分开执行，**不是同一执行轨迹上的配对消融**。不能据此计算无偏检出率
  或定量宣称某方法普遍更强。重复样本、相关客户端和同一根因不能当独立样本。
- `ROUND3-PREDICTIONS.md` 只是工作期记录；不计算预注册预测准确率。

本次引擎审查进一步修复：结构化 target 失败不能成为成功 baseline；后续 post 校准
失败不能成为允许端点；未实现的 cut 不判违反；显式空 plans 不启动默认搜索，零
suspension/部分计划不声称完整覆盖。原始历史日志未重新标注为新引擎结果。

## 6. 当前工程状态与下一步

审查修正了 runner 的退出状态传播及部分 Rust runner 的工作目录，multicast runner
现在拒绝网络/控制失败、缺少 scenario 或子进程失败，并默认使用新输出目录。
`MCAST_DONE` 表示 guest 到达末尾，不单独证明实验有效。

本次收尾验证：Rust 全功能测试通过（含网络代理及 doctest）；Skytable 修复副本
12 个库测试及 fixed harness 全 target 编译通过；Python 引擎 14 个测试、PG 适配器
11 个测试、runner 8 个测试、multicast runner 4 个测试及适配器自测通过。
Skytable 导出补丁无 fuzz 应用后与修复副本一致；Linux 补丁通过基线应用检查。

真实 PostgreSQL 复测中发现启动探测会误认 initdb 的临时 Unix-socket 服务，已改为
等待正式 TCP 服务并在等待超时后失败。修正后 psycopg pool/direct 各执行一轮四级
阶梯：各 24 trials，pool 零违反，direct 仅 M1 4/6 原始 Neither，与旧现象一致。
asyncpg 已完成的 pool/direct 三重复阶梯分别为 48/36 trials，M1 为 0/12 与 9/9。
初次整批运行受启动竞态及运行中脚本变更影响，**不作为整批通过证据**；上述单独
完成的阶梯也不替代原始实验数据。未重跑所有客户端或真实服务 pipeline。
multicast 本次三次 QEMU 启动均完成七个场景，通过控制/完整性检查，重现原版缺组
及修复版干净失败；仍不代表物理网卡验证。临时验证输出不纳入提交，原始日志不改写。

优先继续两条补丁线，而非扩框架：
1. Linux multicast：核对最新上游基线、legacy wrapper/source-filter/RCU/通知，补 veth
   接收和必要回归，整理可发送补丁及最小复现。
2. Skytable：验证 guard 覆盖所有异步入口、部分写/读、业务错误与协议错误，补 pool
   多连接/并发/TLS，核实维护者期望的取消契约，整理可审查变更。

mysql-connector/modbus/POP 的修复对照尚未完成；tiberius 许可、H1Client 运行时覆盖
仍未解决。尚未对外发邮件、issue 或 PR；本地提交不代表上游接受。
