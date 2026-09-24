# G0 预注册可行性检查计划

冻结时间：2026-09-24（Asia/Tokyo）

## 研究问题与总体

问题：对总体 G 中每个已知缺陷，在唯一修复提交的第一父提交中，是否有当时已经存在的测试能够执行到修复所针对的取消站点？只有 `tokio::select!`、`tokio::time::timeout` 或 `timeout_at` 且能通过 Tokio 补丁观测的站点可能成为 A′ 候选。

总体筛选严格按 `survey/labels/human/relevant.csv` 中 `relevant=yes`，与 `survey/labels/human/v3.csv` 中 `cancel_source ∈ {select, timeout}`，及 `survey/labels/human/evidence.csv` 中 `evidence=fix_merged` 按 `url` 内连接。冻结前脚本核对结果：22 行；app=17、library=5；仓库分布为 materialize=4、qdrant=4、risingwave=3、databend=2、hyper=2、iroh=2，pingora、omicron、tokio、deno、neon 各 1。筛选结果如与此计数不一致，停止实证工作，只记录不一致及其原因。

## 冻结缺陷名单

| ID | 缺陷 URL | 已标注 fix_url |
|---|---|---|
| G0-01 | https://github.com/databendlabs/databend/issues/20044 | https://github.com/databendlabs/databend/pull/20050 |
| G0-02 | https://github.com/cloudflare/pingora/issues/934 | https://github.com/cloudflare/pingora/commit/6dcc236 |
| G0-03 | https://github.com/oxidecomputer/omicron/issues/3356 | https://github.com/oxidecomputer/omicron/pull/3411 |
| G0-04 | https://github.com/MaterializeInc/materialize/pull/12714 | — |
| G0-05 | https://github.com/risingwavelabs/risingwave/issues/26409 | https://github.com/risingwavelabs/risingwave/pull/26412 |
| G0-06 | https://github.com/qdrant/qdrant/pull/7530 | — |
| G0-07 | https://github.com/qdrant/qdrant/issues/9670 | https://github.com/qdrant/qdrant/pull/9671 |
| G0-08 | https://github.com/hyperium/hyper/issues/4040 | https://github.com/hyperium/hyper/pull/4042 |
| G0-09 | https://github.com/MaterializeInc/materialize/pull/28816 | — |
| G0-10 | https://github.com/risingwavelabs/risingwave/pull/12725 | — |
| G0-11 | https://github.com/MaterializeInc/materialize/pull/12479 | — |
| G0-12 | https://github.com/qdrant/qdrant/pull/10338 | — |
| G0-13 | https://github.com/qdrant/qdrant/pull/8680 | — |
| G0-14 | https://github.com/n0-computer/iroh/pull/2572 | — |
| G0-15 | https://github.com/databendlabs/databend/pull/17902 | — |
| G0-16 | https://github.com/n0-computer/iroh/pull/2536 | https://github.com/n0-computer/iroh/pull/2539 |
| G0-17 | https://github.com/MaterializeInc/materialize/pull/12485 | — |
| G0-18 | https://github.com/risingwavelabs/risingwave/pull/22128 | — |
| G0-19 | https://github.com/hyperium/hyper/pull/1584 | https://github.com/hyperium/hyper/pull/1585 |
| G0-20 | https://github.com/tokio-rs/tokio/pull/8252 | — |
| G0-21 | https://github.com/denoland/deno/pull/14317 | — |
| G0-22 | https://github.com/neondatabase/neon/issues/7062 | — |

## 第 1 节：修复和父提交

- 对 PR，以已合并 PR 的 merge commit 为修复提交，并取其第一父提交作为修复前父提交。
- 对 issue，只采用唯一关联且已合并的修复 PR。找不到唯一对应修复时记 `unknown`；不得以“相似”提交替代。
- 对每个缺陷记录 `fix_url`、`fix_commit`、`parent_sha` 和修复 PR 新增或修改的测试文件列表。复用 `research/phase3/recall.csv` 中的父提交信息前必须重新核验。

## 第 2 节：出错站点与 Tokio 可补丁性

- 在父提交中定位修复实际针对的取消站点；记录文件、行号、所在函数、站点类型（`tokio::select!`、`tokio::time::timeout`、`timeout_at`、`futures::select!`、tower 超时、自定义组合子或其他）、`biased;` 与否、以及被丢弃的分支。若取消源来自框架/库内部不可定位站点，记 `other` 并解释。
- 记录父提交 `Cargo.lock` 中 Tokio 版本；检查该版本 Tokio 的 `src/macros/select.rs` 是否通过 `IntoFuture::into_future` 构造分支，并检查 timeout 实现是否带 `#[track_caller]`。据此给出 `patchable=yes/no/unknown` 和可复核理由。

## 第 3 节：静态预筛

- 每个缺陷最多 30 分钟。已存在测试指父提交中已有的单元/集成/`#[tokio::test]` 测试或项目测试框架（含 sqllogictest、slt、e2e 脚本）；修复 PR 新增测试不算。修复 PR 修改过的测试须按父提交版本判断。
- 从站点函数沿调用链向上检索，记录所有可能执行到站点的父提交既有测试及调用链依据。候选按调用链由短到长排序。没有候选时定为 `ineligible (no_preexisting_test)`。

## 第 4 节：动态确认

- 仅对静态预筛存在候选者的缺陷进行。工作目录为 `/home/ava/dropwise-g0-work/<repo>/`，不得使用 `/tmp`。一次只构建一个大型仓库，必要时限制并行度；每个缺陷结束后删除该缺陷的 `target` 目录。
- 在修复前父提交的临时工作树为站点加入最小临时探针，只保留在上述工作目录，不向目标仓库或 Dropwise 仓库提交；将探针 diff 保存为 `research/g0/probes/<id>.diff`。记录 `site_hit`（进入 select/timeout 表达式次数）与 `branch_pending`（出错分支/timeout 内层 future 至少一次返回 Pending 的次数），后者用内联包装 future 计数。
- 运行排序后最多前 10 个预筛候选测试。需要服务时，只能临时在 `127.0.0.1` 上用 Docker 启动；结束后停止并记录服务及操作。
- 每缺陷构建和测试合计 90 分钟墙钟。超时记 `unknown (budget)` 并记卡点；构建失败时允许安装父提交 `rust-toolchain` 指定工具链，仍失败记 `unknown (build)`。不得超预算。

## 第 5 节：判定与冻结门槛

- `eligible`：父提交至少一个既有测试 `site_hit > 0`，站点类型属于 `tokio::select!` / `timeout` / `timeout_at`，且 `patchable=yes`。
- `ineligible`：对上述任一条件有确定否定证据；原因类别限于 `no_preexisting_test`、`site_not_hit`、`site_kind_not_covered`、`tokio_not_patchable`、`no_unique_fix`。
- `unknown`：预算耗尽、构建失败或证据不足。
- 另计 `eligible_pending`：eligible 且 `branch_pending > 0`。
- G0 门槛：eligible≥6 为通过；eligible<6 且 eligible+unknown≥6 为未决，并逐一列明解决 unknown 的具体成本；eligible+unknown<6 为不通过。

## 记录、预算与边界

- 每缺陷总预算上限：静态预筛 30 分钟；若进入动态阶段，再最多 90 分钟构建加测试。记录每项实际墙钟及全项目总墙钟。
- 每完成一个缺陷，将该缺陷结果追加到只追加的 `JOURNAL.md` 并单独提交；每次提交包括该次结果改动和所需探针 diff。规则改变须先在 JOURNAL 追加偏离记录并提交，再执行，并报告规则变化前后两个计数。
- 最终交付 `eligibility.csv`、`RESULTS.md` 和需要的探针 diff。除此之外不得修改 `src/`、`tests/`、`survey/`、`repro/`、`prospective/`、`research/phase3/` 或以前冻结文件。不得修改 Dropwise 库本身、开发 Tokio 补丁/runner、push、开 issue、评论或对外发布。目标仓库中的所有更改仅存在于指定临时工作目录。
