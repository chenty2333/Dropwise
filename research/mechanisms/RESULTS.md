# 已知机制对照结果（2026-09-29）

## 结论

**两个已知案例说明了不同的价值来源，不支持“完整 Dropwise 普遍优于简单取消测试”的结论。**

- **Notify：** C-full 在 buggy 程序中三次均报警，修复版均不报警；首次 Pending 取消（B、B-settle、C-matched）均未发现。额外取消时机在此案例有用，但 C-full 使用三个计划，不是等预算算法优势。
- **io_uring：** B-settle 已能三次区分父提交与修复提交，C-matched 和 C-full 没有增加案例级检出。这里的关键是给清理/完成事件观察时间，不需要复杂探索才能发现。
- **立即观察有误报风险：** B 与 C-no-settle 在 io_uring 修复版也报警。它是观察点过早导致的暂态 fd 增加，不能据此报告新缺陷。加上相同 100ms Tokio settle 后，修复版全部不报警。

本研究按已知缺陷选例、阅读修复后写条件。**新缺陷发现数仍为 0**，没有外部用户接入试验，也不是总体精确率/检出率评估。

## 执行范围

计划提交 `b2b27c4`，场景提交 `1c8da7b`，均早于本轮案例运行。真实 Tokio 父/修复提交见 PLAN.md，两个 lockfile 只有 Tokio git 来源一处不同，其余依赖相同。N 的应用层 buggy/fixed 均运行在父版本；U 才切换依赖提交。没有修改 Dropwise 库或历史 repro。

- 固定 72 行全部执行：2 案例 × 2 变体 × 6 方法 × 3 重复，详见 [measurements.jsonl](measurements.jsonl)。
- 全部 A 的 12 次无注入基线先通过，再执行其余方法。C 自带基线也没有错误。
- 两个 Tokio 版本在单独、不计时的 strace 检查中均实际调用成功的 io_uring_setup 和 io_uring_enter；不是 blocking-pool fallback。
- 36 个 C 运行都在配置范围内 exhaustive，unrealized=0、unsettled=0。A/B 不定义 exhaustive。**C-no-settle 的 unsettled=0 仅表示完成了零等待配置，不表示观察窗口足够。**
- B 的两个独立辅助控制测试通过：Ready 不取消；Pending 只切一次；借用的 future 由调用方保留，不误计成底层 future 已丢弃。两种依赖编译成功，场景 Clippy all-targets 无警告。

## 观察表

单元格是“三次重复中，至少一次条件报警的次数”，不是独立缺陷数。一个 C-full 运行有三个计划，不能把三个报警当成三个发现。

| 方法 | N buggy | N fixed | U parent | U fix |
|---|---:|---:|---:|---:|
| A | 0/3 | 0/3 | 0/3 | 0/3 |
| B | 0/3 | 0/3 | 3/3 | **3/3** |
| B-settle | 0/3 | 0/3 | 3/3 | 0/3 |
| C-matched | 0/3 | 0/3 | 3/3 | 0/3 |
| C-full | 3/3 | 0/3 | 3/3 | 0/3 |
| C-no-settle | 3/3 | 0/3 | 3/3 | **3/3** |

N 的 C-full/C-no-settle 每次有 1/3 计划报警，修复版均为 Kept。U 的 C-full 父版本每次 3/3 计划报警，修复版 0/3；C-no-settle 修复版每次 1/3 计划报警。C-matched 每次一个计划。

### 暂态与泄漏的区分

U 的立即观察在父版本和修复版都记录 `fd count increased: 8 -> 9`。延后观察后，父版本仍增加，修复版恢复。核对真实修复：它在收到已取消 open 的 CQE 时，以及相应 driver 生命周期清理点，接管并关闭返回的 fd。因此立即检查并不保证该清理已经执行。

这里保留预注册的全部立即观察结果，不事后删除 fixed 报警、不把 B 换成 B-settle 后仍称作原 B。历史 repro 中“only after settling”的特定观测不能推广成本环境中必然看不到立即增量：本轮立即增量确实可见，但不能可靠区分暂态与持续泄漏。

C-full 同时包含 Immediate、Reschedule(1)、AfterWake。本轮测量表聚合到运行级，没有保留每个失败计划的 race 标签，因而只把 N 的增益归于预选时机集合，不进一步把当前三次观测精确归因给某一个 race。既有 AfterWake 单独回归测试不等于这轮重新测量了该单独消融。

## 成本与可复现性

运行：`python research/mechanisms/run.py --output /path/to/new-measurements.jsonl`。需要 Rust、Python、Linux io_uring、strace 及 lockfile 指定依赖缓存；脚本默认 offline，scratch 为 `/home/ava/dropwise-mechanism-work/`，可用 `--work` 修改。输出已存在时拒绝覆盖。只编译及准备可用 `--prepare-only`。没有外部服务。

下表是 binary 内部计时的中位数（毫秒），包含该方法的 runtime 生命周期；C 还包含自带基线。暂停的 Tokio 时间不是墙钟。

| 方法 | N buggy | N fixed | U parent | U fix |
|---|---:|---:|---:|---:|
| A | 0.267 | 0.247 | 0.373 | 0.366 |
| B | 0.241 | 0.253 | 0.362 | 0.350 |
| B-settle | 0.248 | 0.255 | 0.355 | 0.402 |
| C-matched | 4.160 | 7.216 | 4.387 | 3.323 |
| C-full | 6.998 | 8.791 | 8.570 | 9.131 |
| C-no-settle | 1.310 | 10.417 | 4.178 | 2.816 |

72 行进程墙钟合计 0.568s，内部计时合计 0.236s，均不包括编译和 backend 检查。首次两个版本构建分别 9.17s、9.34s；正式运行前带缓存重建分别 0.51s、0.53s。这些极短运行有噪声，只有三次重复且方法预算不同，不报告加速比、显著性或端到端应用性能结论。

接入实现为 272 行 Rust（含两个 B 控制测试、输出和两案例）及 98 行 Python runner。计划到可执行场景提交间隔约 12 分钟，穿插 G0 工作，**不是净编码工时或外部接入成本测量**。条件来自已知历史案例，也不能代表陌生用户能否自行写出正确条件。

## 本轮能支持的下一步判断

保留完整工具作为可控取消时机与观察窗口的研究原型有依据；把它包装成广泛的自动缺陷发现工具仍缺证据。G0 仍未决，且本轮 U 的 B-settle 已匹配案例级结果。若继续扩大研究，应先讨论外部接入和新案例选择，不自动开始第三优先或扩张检测器。
