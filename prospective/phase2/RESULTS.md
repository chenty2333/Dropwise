# Phase 2: 应用层取消实验结果

实验范围仅为本文件、`PLAN.md` 与本轮代码/运行输出；不改写冻结预注册、T1–T3 汇总或原 `prospective/JOURNAL.md`。本轮是两个目标、四个场景的受控探索，不是新缺陷调查。测试是本项目新写的小型调用程序，调用目标库的真实 API；不是目标仓库原有应用测试。

## 1. 目标、实际路径与条件

| 目标 / 版本 | 实际调用路径 | 条件来源、恢复方式和范围 |
|---|---|---|
| Moka 0.12.16 | `moka::future::Cache::get_with` / `get`，同一缺失键上的并发请求 | 文档说明同键并发调用会合并为一次 initializer evaluation，其他调用等待该结果（[版本化 API 文档](https://docs.rs/moka/0.12.16/moka/future/struct.Cache.html#method.get_with)）。文档没有承诺取消安全；因此“取消 leader 后 waiter/retry 继续、返回同一值”以及“取消一个 waiter 不干扰其他调用”是本轮探索性应用条件，不冒充 Moka 明示的取消契约。允许丢弃被取消 caller 的结果、重选/重跑 initializer、重试；不允许剩余调用挂住、同键结果冲突、成功恢复后缓存仍不可用。 |
| async-nats 0.50.0 / JetStream | durable pull consumer `messages().next()`、`Message::double_ack`、取消后的 `Message::ack_with(Nak(None))` | 文档说明 pull consumer 使用显式 ack；未 ack 可 redeliver，`double_ack` 等待服务器确认（[consumer API](https://docs.rs/async-nats/0.50.0/async_nats/jetstream/consumer/struct.Consumer.html)、[Message API](https://docs.rs/async-nats/0.50.0/async_nats/jetstream/message/struct.Message.html)、[pull config](https://docs.rs/async-nats/0.50.0/async_nats/jetstream/consumer/pull/struct.Config.html)）。文档不承诺 pending 的 `StreamExt::next` 可取消安全，所以 N2.1 的同流续读是探索性条件。N2.2 以显式 NAK、同一 durable consumer 上的 redelivery、幂等调用方 sink 和最终 ack 检查验证消息责任；sink 是本测试调用方的内存夹具，不代表 async-nats 与外部数据库间有事务保证。允许至少一次语义下重复投递，禁止逻辑消息丢失、错序/错配、效果重复或 ack pending 永久占用。 |

入选项目均不在 survey 的 27 个仓库或既有 repro 中。筛过但未选的候选：Tower 0.5.3（其 buffer 源码说明队列中 caller 已离开的请求会被丢弃，效果归属主要由下游 service 决定，归因较弱）；lapin 4.12.0（与 JetStream 重复消息 broker/ack 机制）；deadpool 0.13.1（与已做的 bb8/sqlx 池路径重叠）；flume 0.12.0（主要是 channel primitive，不足以提供清晰的调用方集成条件）。没有搜目标的取消缺陷、修复 PR 或历史报告。

### 场景

- **M2.1 leader 取消：** 一个同键 `get_with` leader 在 loader gate 上 pending，两个 waiter 也已被驱动到同键等待；取消 leader 后执行一次新 `get_with`，并继续两个 waiter，核对缓存与三方结果一致、同键 initializer 峰值不超过一个。
- **M2.2 waiter 取消：** 一个独立运行的 gated leader、一个被选中的 waiter、一个存活 waiter；取消所选 waiter后释放 leader gate，核对 leader、存活 waiter 和缓存值一致，loader 仍只运行一次。
- **N2.1 同流续读：** 先读取并确认 prefix；当随后同一 `PullConsumer` stream 的 `.next()` 确认返回 `Pending`，事件驱动 producer 才发布 `event-1/2`。取消后不重建 stream，继续读取/确认，幂等 sink 核对逻辑顺序及 pending ack 清零。
- **N2.2 取出后取消：** 已取得 `work-1` delivery；第一事件 gate 在 effect 前，第二 gate 在幂等 effect commit 后、ack 前。取消时保留 delivery 并显式 NAK；同一 durable consumer 收到 redelivery 后去重并 double-ack，再处理 `work-2`，检查两个逻辑效果各一次、consumer 继续前进且无 pending ack。

## 2. 方法和预算

A 是无注入正常调用；B 是本地窄基线：观察 target 第一次 `Pending` 后令 `select!` 中的 one-shot competitor 就绪并立即丢弃 target；C-matched 使用 Dropwise `[Immediate]`、`max_runs=2`（基线 + 一个首边界计划）；C-full 使用 Dropwise `[Immediate, Reschedule(1), After(10ms)]`、`max_cancellations=1`、`max_runs=128`、`Settle::default()`、10s 场景 watchdog、`Flavor::MultiThread { workers:2 }`。每个场景/方法重复 3 次，先完成所有 A，再执行 B、C-matched、C-full。NATS 使用临时 `nats:2.11.6-alpine` JetStream，只把随机端口映射到 `127.0.0.1`，runner 退出时容器 `--rm` 清理。

A/B/C 的输入 ID、gate、恢复分支和正确性条件一致；观察/续读/ack 检查在被调场景内共同执行。B 只是单边界基线，不代表 `futures-testing` 完整能力。方法设计不是声称首创逐 Pending 注入：Hyperactor 的文档 helper 会计数 yield 并逐边界取消新运行、再用新运行核对结果；其 async 版本接受 `on_pending` 外部推进回调（[API](https://meta-pytorch.org/monarch/stable/rust-api/hyperactor/testing/cancel_safe/index.html)、[实现](https://meta-pytorch.org/monarch/stable/rust-api/src/hyperactor/testing/cancel_safe.rs.html)）。`futures-testing` 是叶子 Future 属性测试，随机交错 poll、driver 进展、取消、spurious poll 和 waker 替换，并支持可重放 seed（[README/用法](https://github.com/conradludgate/futures-testing)）。本轮既没有把 B 冒充这些工具，也没有对它们做性能比较。

## 3. 有效结果

表中每格运行时间是 3 次独立重复的**均值（最小–最大，秒）**；逐次完整输出在 `runs/reports/`、`runs/logs/`。时间从本测试调用入口到场景/`explore()` 返回：C 包含内部 baseline、计划运行及每次默认 100ms settle；不含 `cargo` 启动、NATS 服务启动。整轮脚本 48 次 cargo test 调用、NATS 启停共 13 秒。

| 场景 | A：baseline | B：首次 Pending 立即取消 | C-matched：1 个 Dropwise cut | C-full：完整预定 race 集 |
|---|---|---|---|---|
| M2.1 leader 取消 | 1 个 target Pending、0 cancel、成功；0.000593（0.000586–0.000605） | 1 观察 / 1 实际 cancel、恢复成功；0.000652（0.000640–0.000667） | 1/1 计划实际取消、无违反、exhaustive；0.204371（0.204289–0.204476） | 3/3 计划实际取消、无违反/未实现/截断、exhaustive；0.419827（0.419519–0.419993） |
| M2.2 waiter 取消 | 1 Pending、0 cancel、成功；0.000696（0.000675–0.000716） | 1/1 实际 cancel、其他调用完成；0.000677（0.000662–0.000698） | 1/1 实际取消、无违反、exhaustive；0.204474（0.204405–0.204585） | 3/3 实际取消、无违反/未实现/截断、exhaustive；0.419375（0.418848–0.419937） |
| N2.1 同流续读 | 1 Pending、0 cancel、事件消息均读到并确认；0.007061（0.006080–0.007800） | 1/1 实际 cancel、同流续读成功；0.006431（0.005804–0.006881） | 1/1 实际取消、无违反、exhaustive；0.217005（0.215462–0.219313） | 3/3 实际取消、无违反/未实现/截断、exhaustive；0.447605（0.446280–0.448564） |
| N2.2 commit 后/前取消与 NAK | 2 Pending、0 cancel、顺序处理/ack；0.006152（0.005607–0.006707） | 1/1 实际 cancel、NAK/redelivery 后恢复；0.006331（0.005457–0.007075） | 1/1 计划实际取消、无违反；`exhaustive=false` 是预先设定 `max_runs=2` 的预算截断，并非未实现计划；0.216611（0.214887–0.218056） | 6/6 计划实际取消（2 个边界 × 3 race），无违反/未实现/截断、exhaustive；0.788389（0.782483–0.798135） |

最终有效结果中：A 12/12 正常完成；B 12/12 实际取消且恢复成功；C-matched 12/12 一个计划均实际取消；C-full 总计 45 个计划跨 3 次重复，全部实际取消。所有 Dropwise 运行 baseline 成功；有效 Dropwise 计划无 violations、无 unrealized outcome、无 settle watchdog 截断。场景内所有网络调用均在 10s 内完成。

## 4. 比较增益和代价

没有方法在这些场景报告缺陷，不能把更多计划当成发现优势。C-matched 与 B 都取消同一个首个 Pending，四个目标都得到同样的正确结果；budget-matched 下没有观察到 Dropwise 的额外检测结果。C-full 对 M2.1、M2.2、N2.1 每个都只观测到一个 Pending，因此只是在同一边界尝试三种时机；这些额外时机没有改变结论。**N2.2 是本轮唯一新增边界：**完整 Dropwise 在本地 effect commit 后、ack 前也取消 3 次/重复，而 B 与 C-matched 只在第一边界取消；idempotent recovery 均成功。这是额外的状态空间覆盖，不是已发现 bug。

预算相等的耗时比较应把 B 的独立 baseline 与一次 B 注入耗时相加，再对比 C-matched（它内部含 baseline + 一个计划）。按均值，M2.1 为约 0.00125s vs 0.20437s，M2.2 0.00137s vs 0.20447s，N2.1 0.01349s vs 0.21701s，N2.2 0.01248s vs 0.21661s。差值主要包含 Dropwise 运行管理与每次 100ms 默认 settle，并非相同内核下的 benchmark。C-full 又包含更多计划，耗时更高；只报告成本，不将它视为无条件“优于”B。

## 5. 失败判读、修正与分类

按“场景/工具/环境 → 契约范围 → 目标实现”的顺序：

| 现象 | 判读 | 结果处理 |
|---|---|---|
| P2-A1 前，12 个 A 尝试都在场景开始前 panic：plain runner 在 Tokio runtime 外创建 `tokio::time::timeout`。 | 场景 runner 错误，无目标调用、无取消、无基线行为可判。 | P2-A1 把 timeout 构造放进 `block_on` async block；原始日志保留在 `runs/pre-correction/timeout-outside-runtime/`，纠正后重跑全部 A。 |
| P2-A1 后，NATS B/C 创建 stream 时出现 `subjects overlap`；另外 Moka C 的 `explore()` baseline 被 helper 错误要求“必须取消”，尽管 Dropwise baseline 本应无取消；Moka 计划 trials 本身是 clean。 | 资源命名冲突和 harness 对 Dropwise baseline 的判定错误；NATS 错误发生在目标注册之前，Moka 报告因 baseline 错误不能作为有效方法结果。不是目标违反或误报。 | P2-A2 在 subject 中加入 scene/mode/run/process/serial，并仅在 B 检查直接取消、在 C 由报告检查实际 plans；这些首次输出保留在 `runs/pre-correction/first-post-A1/`，修正后完整重跑所有方法。 |
| 修正后的有效结果 | 0 个目标违反报告。 | 误报：0；超出目标契约：0；可复现候选缺陷：0。没有需要做最小缺陷复现/根因调查的结果，也没有给上游提交测试、issue 或评论。 |

前两轮暴露的是 3 个 harness 问题（P2-A1 一个、P2-A2 两个），共 42 个无效/不纳入最终结果的调用日志：12 次 timeout panic、18 次 NATS setup error、12 次 Moka C baseline-helper error。每种修正前输出均保留；这些不是 42 个 cancellation violation，也不计为 false positive。首次运行后没有改变场景契约、输入、race 配置或 Dropwise 实现。

**事前与事后边界：**候选、目标 API、场景、正确性条件、A/B/C 方法、配置和预算在 `9812fa5` 固定；修正后的有效结果是这些事前条件下完成的前瞻运行。P2-A1/P2-A2 是看见首轮 harness 失败后才登记并实施的事后工具修复，所以原始失败轮次不能伪装成事前成功数据；它们未更换目标、契约、场景输入或配置，所有修正前输出均保留。没有根据有效结果新增目标或改写条件。

## 6. 代码规模、时间和局限

- 四个场景测试文件 `/home/ava/Desktop/Dropwise/prospective/phase2/tests/scenarios.rs`：737 行；本轮局部 runner `/home/ava/Desktop/Dropwise/prospective/phase2/src/lib.rs`：400 行；NATS/整体 runner：109 行。加总 1,246 行代码（不含 Cargo 锁文件），里面包含共用观测/比较 helper，不能全部算作目标专用接入代码。
- 场景从开始编写到 `cargo test --no-run` 成功并提交约 28 分钟墙钟时间（含编译修正；候选筛选/文档阅读时间不算，也不是主动编码时长计时）。随后两个 harness 修正、编译和诊断约 4 分钟墙钟；阶段选择设计单独记时未记录。
- A/B 每项 3 次；C-matched 是预算匹配的单计划比较；C-full 每场景受 `max_runs=128` 限制，本轮全部 exhaustive。C-matched 的 N2.2 明确非 exhaustive，不能把它当作两边界全覆盖。
- Pending 数不是源代码 `await` 覆盖。三个单边界场景各自只覆盖被标记 Future 上实际观测到的一个 Pending；N2.2 只覆盖两个事件 gate（effect 前、effect 后/ack 前）。不覆盖其他 key、更多 consumer、消息丢失窗口、server 故障、其他存储/side-effect 语义或全部调度。
- NATS 结论只对 `async-nats=0.50.0` 与本地 `nats:2.11.6-alpine` 这套配置有效。Moka/JetStream 的取消连续性条件未被维护者确认，不能外推成 API 保证。
- 没有产生可供上游的回归测试；这轮最合理结论是零发现。继续扩大不应以增加目标数量为目的；仅当能先固定更强的应用契约、并且有比此次内存 sink 更强的真实外部状态 oracle 时，才值得有选择地继续。就本轮样本而言，Dropwise 的具体增益是覆盖第二个 post-commit Pending 边界，尚未表现为新增缺陷检测；简单首边界取消在三个单边界场景已覆盖相同可见结果。

## 7. 本轮计划/修订与提交

计划提交 `9812fa5` 在场景代码前固定了候选、范围、条件、配置和预算。初始场景/runner 是 `dc73374`。首次 baseline 暴露 plain runner 问题后登记 P2-A1，修正提交 `ff3cfce`；第二轮暴露 subject namespace 和 Dropwise baseline helper 问题后登记 P2-A2，修正提交 `4e9acc1`。首次及中间输出保存在两个 `runs/pre-correction/` 子目录。上述改变均是实验工具修复，不改变正确性条件、目标 API 路径或预算。过程和运行 commit 继续追加在本目录 `JOURNAL.md`；冻结的 `prospective/PREREGISTRATION.md`、T1–T3 `prospective/RESULTS.md` 和根 `prospective/JOURNAL.md` 均未修改。
