# 已知机制对照：预注册小规模评估

冻结于 2026-09-29，场景实现和本轮取消运行之前。承接已授权的第二优先级；G0 收尾后才运行本评估，不开发 Tokio 注入 runner，不扩大样本，不对外发布。

## 问题与固定案例

这是按已知机制选择的回顾性验证，不是前瞻发现，也不估计真实项目检出率。

1. **N：Notify 通知丢失**。沿用 `repro/tokio-3825` 的单客户端、4 秒后 notify_waiters、循环重新创建 Notified 的适配程序；修复版在循环外固定同一个 Notified。不使用原报告不可实现的 target-first biased + After(5s) 组合，C 使用无 biased 的 select，竞争分支在源码中列在前面。共同条件：通知发出后客户端应退出；统一用外层 8 秒 Tokio 虚拟时间超时报告未退出。该期限是本次显式适配，不是原应用完整行为。
2. **U：取消 io_uring open 后 fd 晚到**。沿用 `repro/tokio-7979` 的普通文件及 warm-up open。真实 Tokio 修复父提交 `ad8c59add6a1988d8c327fb3358beeeae3bbb5cd` 与修复提交 `c79121391db8f8d36d4213feeb25381caee110c7` 对照。共同条件：warm-up 后至观察点，进程 fd 数不得增加。必须确认实际使用 io_uring，不能把 blocking-pool fallback 当成此机制结果。

两案例共用该 Tokio 父/修复版本，N 的 buggy/fixed 指应用循环变体，U 的 buggy/fixed 指依赖提交。N 在父版本上比较即可；U 在两版本上比较。不修改历史 repro、调查样本、Dropwise 库或以前结果。

## 方法与公平性

每种方法使用同一输入、相同修复变体及同一条件。B 必须是独立的一次 Pending 观察器，不调用 Dropwise 执行取消。

- **A**：普通无注入执行，保留共同后续检查和 100ms settle。
- **B**：第一个 Pending 后立即丢弃目标，只切一次；调用方继续，立即检查，不增加 settle。
- **B-settle**：与 B 完全相同，但观察前运行 100ms Tokio settle。用来区分延后观察的贡献和复杂探索的贡献。
- **C-matched**：Dropwise Immediate，max_runs=2（基线加至多一个取消计划）、max_cancellations=1、100ms settle。与 B-settle 的取消预算相同；C 包含额外基线，时间不可当成每计划等价成本。
- **C-full**：Dropwise races=[Immediate, Reschedule(1), AfterWake]，max_runs=32、max_cancellations=1、100ms settle。更多计划不能被解释为预算无关的优势。
- **C-no-settle**：与 C-full 相同，但 Tokio/real settle 均为零。保持同一 after_settle oracle；这仅移除显式等待，runtime/driver 仍可能推进，因此零等待不保证完全看不到晚到事件。

CurrentThread、paused Tokio time；scenario watchdog=2s 墙钟、settle watchdog=2s。每进程外层 timeout=20s。独立 A/B runtime 同样单线程、paused time，执行后才做选择的 settle。U 使用 Linux io_uring 编译配置；先用 strace 的 io_uring_setup/enter 检查实际 backend，strace 不计入计时重复。

## 执行与判读

先提交计划，再实现、编译并提交场景。先运行全部 A（两个案例的 buggy/fixed 各三次），只对 A 满足条件且 backend 可评估的案例继续 B、B-settle、C-matched、C-full、C-no-settle，各三次，顺序运行。固定总分母为 2 案例 × 2 变体 × 6 方法 × 3 重复 = 72 行；构建、backend、基线阻塞的行保留为不可评估，不填零或删除。无阈值调参或结果后换案例。

记录每行是否可评估、错误/违反、计划数、实际取消数、unrealized、unsettled、exhaustive（A/B 不适用）、运行墙钟。C 的基线错误与取消后违反分开。结果按案例而非计划数计增益；B-settle 能找到的结果不归因于探索算法。报告构建/接入工作成本与测量局限，不做显著性或总体性能推断。

每版本构建上限 15 分钟，总评估执行上限 30 分钟。工具/场景错误先记录、修复、提交，再重跑受影响行；失败输出不冒充项目缺陷。锁定依赖，scratch 位于 `/home/ava/dropwise-mechanism-work/`；不启动外部服务、不改变内核/网络配置。结果只保留必要测量表和简短说明，不另建证据归档。
