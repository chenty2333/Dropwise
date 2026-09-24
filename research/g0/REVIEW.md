# G0 复核（2026-09-25，项目负责人侧）

## 结论：G0 应为“未决”，而非“不通过”

`tokio_not_patchable` 的 8 例（G0-03、09、11、14、16、17、21、22）全部是 `tokio::select!` 站点，
被排除的唯一理由是该版本 `select!` 不经 `IntoFuture::into_future` 构造分支。这一判据来自 G0 prompt
（项目负责人撰写），把 tokio 1.53.1 的补丁位置误写成了可打补丁的前提：

- tokio 1.28.2 `src/macros/select.rs:474` 为 `let mut futures = ( $( $fut , )+ );`，同样是逐分支构成元组，
  可由按版本适配的补丁逐分支包装。`IntoFuture` 只影响补丁写在哪一行，不影响可行性。
- `timeout_at` 缺少 `#[track_caller]` 同理：补丁可自行加上。

这 8 例的静态预筛均有候选测试，但因判据短路，从未做动态确认。按冻结门槛：
eligible 2 + 待确认 8 + unknown 1 = 11 ≥ 6，属于“未决”。需以偏离 D4 纠正判据后补做动态确认。

## 其余分类

抽查 `site_kind_not_covered`（8）、`no_preexisting_test`（2）、`site_not_hit`（1）与 2 个 eligible，
未发现问题。这 8 个站点类型不覆盖（futures 组合子、自定义超时包装、依赖库内部超时层、tokio 运行时内部）
是 A′ 的真实覆盖上限，应如实报告。

## 对 A′ 成本的提示

候选测试中相当比例是端到端框架（materialize testdrive/sqllogictest、neon pytest regress、qdrant pytest），
每个取消计划起一个进程意味着每次都要启动整套服务。补做时应记录测试框架类型与单次基线墙钟时间。
