# Phase 3 冻结计划：共享状态提前变更与取消

冻结时间：2026-09-24T01:31:25Z。计划先于检测器源码；首个提交只包含本文件与本阶段日志。冻结后不增删召回集、精确率目标、判读规则或抽样规则。若有必要变更，必须先在 `JOURNAL.md` 登记为偏离，再执行，并并列报告变更前后指标。

## 1. 研究问题与范围

- **Q1（召回）：**在经过双标注/裁决的 97 个取消缺陷中，识别“共享/持久状态先变、恢复/提交晚于可取消 await”的缺陷模式后，轻量语法检测器能在修复前代码中命中多少缺陷所在函数？
- **Q2（精确率）：**同一冻结检测器在 8 个固定当前提交上报告的函数候选中，有多少经盲判属于真实风险？
- **Q3（确认）：**真实风险候选能否由 Dropwise 对当前目标代码作执行级确认？不能直接执行的缩减模型只能记为 `modelled`，不得称为新缺陷。仅执行级测试在修复前行为上重现了违反，才可列为“新缺陷候选”。

研究限定于本文件所定义的单一机制，不估算整个 Rust 生态或数据集之外的召回。计划、检测器、运行表、审阅理由、结果、发现与相关工作只写入 `research/phase3/`；不改用户列明的冻结路径，不改 `src/`，不 push、不发 issue/评论、不对外发布。

## 2. 机制的操作性定义

### 2.1 模式

一个实例必须同时满足：

1. 代码位于同一个 `async fn` 或 `async {}`/`async move {}` 作用域；
2. 在一个可能返回 `Pending` 的 `.await` 之前，代码先改变了可在该 Future 被销毁后继续存在或被再次使用的状态；
3. 该状态的恢复、回滚或逻辑提交发生在该 `.await` 之后；在 await 处丢弃 Future 可跳过该动作，并留下未恢复的状态/责任。

检测器按 AST 及词法顺序近似实现，不证明路径可达或 await 实际返回 Pending。每个候选行对函数级报告留一条最早的匹配记录。

### 2.2 计入的“状态改动”

- 从共享/持久位置取走或删除：`Option::take`、队列 `pop*`、容器 `remove`、`replace`、`std::mem::replace`/`swap` 等；
- 对共享对象原地写入：完成/已提交/已发送一类布尔字段，计数器增减，阶段/状态机字段赋值；以及同类原子 `store`/`swap`/`fetch_add`/`fetch_sub`；
- “共享/持久”由语法可见的 `self` 字段、引用参数、字段/索引访问，或明显的 `Arc`、锁、原子、共享容器访问判定。只操作当前 Future 内新建且无共享/持久来源的局部值不计。

### 2.3 “恢复/提交在 await 之后”

对取出/删除类：await 后在同一作用域重新写入同一状态位置（赋值、insert/replace/push 等），且可由同一目标路径或同一提取值的标识符作语法关联。对提前置位/计数/状态类：await 后仍有同一目标字段的回滚/完成写入，或 await 的调用链/紧邻后续调用显式表现为 `finish`、`commit`、`ack`、`send`、`flush`、`complete`、`finalize` 一类终结操作。名称列表固定于检测器测试；纯语义不可见的“稍后可能提交”不由检测器猜测。

### 2.4 防护与排除

以下情况对人工判读可判为“已防护”，且若检测器看见可验证的回滚实现，应抑制该候选：

- 变更前已建立覆盖该 await 的 scopeguard/defer/RAII/`Drop` 守卫；其可见清理体在 Future 销毁时确实把同一状态/值恢复或回滚；
- 提取值在 await 前已移入此类回滚守卫，且守卫的 `Drop` 清理体恢复同一共享位置；
- 改动在 await 前已完成不可撤回的逻辑提交，且后续只是在等待确认（这不是“先改状态、后提交”的本模式；若确认本身可能重复或丢失责任，则按独立机制排除）。

不把普通锁卫、`Arc`、`Option` 包装、仅有 `Drop` 但不回滚状态的类型、`.await` 后才创建的守卫视为防护。库/宏展开不可见时，报告候选并在人工判读中说明不确定性；不得仅因类型/宏名称含 `Guard` 就抑制。

明确排除：纯局部变量且无逃逸/共享；状态仅在 await 后修改；await 前未 poll 即销毁的 Future；合作式取消并正常返回；只有 futurelock；I/O/协议中间态但没有上述先行共享状态改动；在不同 async 作用域间拼接的“先改后 await”；以及不属于 Rust 源代码的生成物。

## 3. Q1 召回评估集与分母

快照源为 `survey/labels/human/{relevant,v3,evidence}.csv`，版本见 `survey/DATASET.md`（2026-09-24）。97 个 `relevant=yes` 缺陷中，冻结纳入候选集合 **S=45**：`holder ∈ {dropped_future, shared_object}` 且 `phase ∈ {in_progress, recorded_ahead}`。此 45 项全部列在下面，不能根据后续检测结果增删。

固定报告两个分母：

- 主召回率 = 命中的“可评估修复前函数”数 / 可评估修复前函数数；
- 保守覆盖下界 = 命中数 / 45。没有已合并修复、不能找到修复前父提交、不能定位相关 Rust 函数或取不到源文件时，标为**不可评估**，仍留在 45 项分母和逐项表中；不得静默删除或当成非命中。分别报告可评估数、不可评估数、命中/漏检。

逐项冻结名单（URL、v3 机制标签、证据值均来自上述 CSV）：

- `dropped_future/in_progress`; `reproducer` — https://github.com/risingwavelabs/risingwave/issues/26180
- `dropped_future/in_progress`; `fix_merged` — https://github.com/databendlabs/databend/issues/20015
- `shared_object/in_progress`; `maintainer` — https://github.com/neondatabase/neon/issues/10023
- `shared_object/in_progress`; `maintainer` — https://github.com/tokio-rs/tokio/issues/6493
- `dropped_future/in_progress`; `fix_merged` — https://github.com/cloudflare/pingora/issues/934
- `shared_object/recorded_ahead`; `fix_merged` — https://github.com/databendlabs/databend/issues/20020
- `dropped_future/in_progress`; `maintainer` — https://github.com/qdrant/qdrant/issues/9745
- `dropped_future/in_progress`; `fix_merged` — https://github.com/oxidecomputer/omicron/issues/3356
- `shared_object/in_progress`; `reproducer` — https://github.com/cloudflare/pingora/issues/931
- `shared_object/in_progress`; `fix_merged` — https://github.com/qdrant/qdrant/pull/3367
- `shared_object/in_progress`; `fix_merged` — https://github.com/risingwavelabs/risingwave/issues/26409
- `shared_object/in_progress`; `fix_merged` — https://github.com/neondatabase/neon/issues/3478
- `dropped_future/in_progress`; `fix_merged` — https://github.com/qdrant/qdrant/issues/9670
- `shared_object/recorded_ahead`; `fix_merged` — https://github.com/qdrant/qdrant/pull/7787
- `dropped_future/in_progress`; `maintainer` — https://github.com/tokio-rs/tokio/issues/5285
- `shared_object/in_progress`; `fix_merged` — https://github.com/denoland/deno/pull/20316
- `dropped_future/in_progress`; `fix_merged` — https://github.com/MaterializeInc/materialize/pull/28816
- `dropped_future/in_progress`; `fix_merged` — https://github.com/hyperium/hyper/issues/3995
- `dropped_future/in_progress`; `fix_merged` — https://github.com/risingwavelabs/risingwave/pull/12725
- `dropped_future/in_progress`; `fix_merged` — https://github.com/MaterializeInc/materialize/pull/12479
- `dropped_future/in_progress`; `maintainer` — https://github.com/qdrant/qdrant/issues/9665
- `shared_object/in_progress`; `fix_merged` — https://github.com/qdrant/qdrant/pull/10338
- `shared_object/in_progress`; `fix_merged` — https://github.com/risingwavelabs/risingwave/issues/3909
- `dropped_future/in_progress`; `fix_merged` — https://github.com/MaterializeInc/materialize/pull/38577
- `shared_object/in_progress`; `fix_merged` — https://github.com/risingwavelabs/risingwave/pull/6985
- `dropped_future/in_progress`; `fix_merged` — https://github.com/n0-computer/iroh/pull/2572
- `dropped_future/in_progress`; `fix_merged` — https://github.com/neondatabase/neon/pull/12345
- `dropped_future/in_progress`; `reproducer` — https://github.com/risingwavelabs/risingwave/issues/26176
- `dropped_future/in_progress`; `maintainer` — https://github.com/hyperium/hyper/issues/3199
- `shared_object/in_progress`; `fix_merged` — https://github.com/MaterializeInc/materialize/pull/12485
- `shared_object/in_progress`; `maintainer` — https://github.com/cloudflare/pingora/issues/932
- `dropped_future/in_progress`; `fix_merged` — https://github.com/MaterializeInc/materialize/pull/29221
- `shared_object/recorded_ahead`; `fix_merged` — https://github.com/oxidecomputer/omicron/pull/3579
- `dropped_future/in_progress`; `maintainer` — https://github.com/paritytech/polkadot-sdk/pull/11013
- `shared_object/in_progress`; `fix_merged` — https://github.com/risingwavelabs/risingwave/pull/22128
- `shared_object/recorded_ahead`; `reproducer` — https://github.com/cloudflare/pingora/issues/933
- `dropped_future/in_progress`; `fix_merged` — https://github.com/hyperium/hyper/pull/1584
- `dropped_future/in_progress`; `maintainer` — https://github.com/seanmonstar/reqwest/issues/949
- `dropped_future/in_progress`; `fix_merged` — https://github.com/risingwavelabs/risingwave/pull/6658
- `shared_object/in_progress`; `maintainer` — https://github.com/tokio-rs/tokio/issues/4908
- `shared_object/in_progress`; `fix_merged` — https://github.com/oxidecomputer/omicron/pull/11263
- `dropped_future/in_progress`; `fix_merged` — https://github.com/redis-rs/redis-rs/pull/597
- `dropped_future/in_progress`; `maintainer` — https://github.com/hyperium/hyper/issues/2113
- `dropped_future/in_progress`; `fix_merged` — https://github.com/oxidecomputer/omicron/pull/3351
- `shared_object/in_progress`; `fix_merged` — https://github.com/risingwavelabs/risingwave/pull/8145

修复前位置的判定方法固定为：若项目缺陷行是修复 PR 本身，取该 PR merge commit 的第一父提交；若 issue 关联 merged fixing PR，取该 PR 的第一父提交；若标记为 `fix_merged` 但关联信息不足，查原始 issue/PR 中明确指向的修复，记录 URL 与父提交。若无法建立唯一对应，不换成“相似”提交，记不可评估。每项记录 issue/PR URL、修复链接、父提交、路径/函数/行号、检测器是否命中与不可评估原因。

## 4. Q2 精确率目标（冻结提交）

提交固定为在 2026-09-24 约 01:26 UTC 对 GitHub 默认 `HEAD` 读取的完整 commit hash；分析只检出这些 hash，不跟踪后续 HEAD。六个数据集大型应用加两个非数据集 Tokio 应用：

| 仓库 | 用途 | 固定 commit |
|---|---|---|
| [qdrant/qdrant](https://github.com/qdrant/qdrant) | 数据集大型应用 | `6ab21cac18ebb6f4ae29102c7f8f5cc11affd5de` |
| [risingwavelabs/risingwave](https://github.com/risingwavelabs/risingwave) | 数据集大型应用 | `0d32b959c513370b47eda4e5e7787185472c5cf2` |
| [databendlabs/databend](https://github.com/databendlabs/databend) | 数据集大型应用 | `3a1be1ebf2af8968cfa01297fc4f4d18177ed520` |
| [MaterializeInc/materialize](https://github.com/MaterializeInc/materialize) | 数据集大型应用 | `36a9d421672f86e1485f848823a6be3ca2529140` |
| [neondatabase/neon](https://github.com/neondatabase/neon) | 数据集大型应用 | `fa504217c61bbcaf5c512d75830564541f917f8f` |
| [cloudflare/pingora](https://github.com/cloudflare/pingora) | 数据集大型应用 | `4487f7b2ab50f159e4a2cf4f6a6b813f61bb6e19` |
| [shotover/shotover-proxy](https://github.com/shotover/shotover-proxy) | 泛化应用；不在 `survey/repos.txt` 的 27 个仓库；冻结快照含 Tokio workspace dependency | `5cb1e0dd3c9a8835c7bb7ae5e6ca6a46aaa9ec9e` |
| [GreptimeTeam/greptimedb](https://github.com/GreptimeTeam/greptimedb) | 泛化应用；不在 `survey/repos.txt` 的 27 个仓库；冻结快照含 Tokio workspace dependency | `c7fa48ef95bc5da1f245d9e292ee53b28a4f2136` |

只扫描固定快照中 Git 跟踪的 Rust 源文件：至少在 `src/` 路径下；不扫描 `.git`、`target`、`vendor`、构建输出和纯生成代码。测试、examples、benches、fuzz 目录排除。所有目标同一检测器版本、同一 CLI/配置。记录每仓库扫描文件数、解析失败/跳过数、函数级候选总数、执行时长、固定 commit 和完整命中位置。不得基于候选多少替换目标。

## 5. 候选盲判、抽样与精确率

判读前不看目标运行结果。候选单位为 `(仓库, 相对路径, 函数/async 块起始行)`，同一函数多条语法命中折叠为一个候选。若每仓库候选不超过 10 个，则全量判读；若超过 10 个，则以固定 seed `20260924` 做简单随机、不放回抽样：按 `SHA-256("20260924/<repo>/<relative-path>:<line>/<scope-name>")` 排序，取最小 10 个。保留总候选数、排序/抽中名单、抽样数和逐条理由。

在看到输出类别前，以冻结的源码/调用上下文判读：

- **真实风险**：await 允许挂起且 Future 可被销毁；持久状态已先改变；中间没有有效回滚/提交守卫；销毁能跳过恢复/提交并可从源码/项目不变量说明推导出可观察的错误后果。
- **已防护**：可见 `Drop`/defer/scopeguard/等效所有权守卫在该 await 的取消路径确实恢复、回滚或幂等提交，或提交早于 await 且符合本模式外语义。
- **取消不可达**：只有代码能证明该 await 不会让 Future 处于可被销毁的 Pending 边界，或该路径不可能在变更后抵达；不能用“没看到 select!/abort”作证明。识别不到即不得判此类。
- **无法判断**：宏展开、动态调用、隐藏契约、CFG/别名/锁语义或源码证据不足以支持前三类。

每条理由需指出命中/状态操作行、await 行、恢复/守卫/不变量的源码位置；无法支持的推测标为未知。精确率主值为 `真实风险 / 全部抽样判读候选`，即 `无法判断` 保守计入非真阳性；另报解析失败与未判读总量。若抽样，给出每仓库 `TP/n` 和依候选数加权的总体估计 `Σ(N_r * TP_r/n_r) / ΣN_r`；同时报实际已审样本率与“将所有无法判断当非 TP”的保守率，不把样本比率称为全仓库精确真值。保护/不可达/无法判断必须分别列数。

## 6. Dropwise 确认标准

对每个精确率判为真实风险的候选，优先在固定当前代码构建执行级测试：通过新的 `research/phase3/confirmation/` 测试工程/夹具调用目标真实函数或最小真实 API 路径，不改目标 `src/`。在被测操作 await 处由 Dropwise 探索 Future 销毁，并用同一调用方恢复/重试或共享对象检查来断言契约。

若大型目标无法隔离执行，才做照搬相关函数控制流、字段和终结调用的缩减模型，记录为 `modelled`，并在 `FINDINGS.md` 逐条引用原仓库固定 commit 的函数 URL、路径、准确原始行号及模型与原代码差异。`modelled` 是机制演示，不是当前仓库缺陷证据。测试准备失败、fixture/harness 失败和目标违反分开记录。仅当执行级 Dropwise 测试在**修复前父提交行为**复现违反，才命名“新缺陷候选”；仅当前版本/模型重现不能满足该标准。不得自行上报。

## 7. 结果记录、预算与停止规则

- 阶段预算：冻结后总计最多 **6 小时墙钟时间**，从计划提交时间计至最后一次验证；计划内依序进行检测器、召回、精确率、确认、报告，不扩大仓库或缺陷范围。
- 停止：6 小时到点或既定问题完成即停止；无可定位父提交/代码按不可评估记录；不能构建目标时保留原因并继续其余已冻结目标，不用替代仓库；不因零发现扩样或换检测器。命中超过抽样上限按第 5 节抽样。
- 每阶段在 `JOURNAL.md` 追加 UTC 时间、开始/结束或用时、commit/固定输入、命令和结果摘要、失败及其类别、任何偏离。保留复现研究需要的逐项召回 CSV、抽样审阅理由和汇总，但不另造证据包、固定哈希清单或推广材料。
- Q1 至少记录 45 行逐项结果及评估父提交；Q2 记录 8 仓库扫描总数、抽样方案、每条判读理由；Q3 记录测试是否执行、Dropwise 计划/实际取消和断言结果；结果文档报告召回/精确率的分子分母、不可评估/不确定数、类计数、发现数、代码行数与用时、限制和偏离。

## 8. 固定执行顺序

1. 本计划与初始日志先提交；此时不写/构建/运行检测器。
2. 编写 `research/phase3/detector/`（`syn` 解析 async 函数/块，按第 2 节规则）；仅对检测器自身 `cargo test` 与编译，不读取/扫描召回数据或目标仓库；提交检测器。
3. 用已提交检测器运行修复前召回集；保存逐项结果。
4. 对已固定的 8 个 HEAD 运行同一已提交检测器；按第 5 节判读/抽样。
5. 对真实风险按第 6 节 Dropwise 确认；写 `FINDINGS.md`。
6. 添加带逐条可核查链接的 `research/RELATED.md`，包括 Hyperactor 的 `hyperactor::testing::cancel_safe`、Asupersync、`futures-testing`、Tokio 文档、Clippy `await_holding_*` lints、`tpt-async-guard`、OSDI 2022《Cancellation in Systems》，写明功能与本研究范围的关系；再写 `RESULTS.md` 并完成必要检查/提交。

截至冻结时没有检测器源码或运行结果，也没有对固定目标执行检测器。计划无偏离。
