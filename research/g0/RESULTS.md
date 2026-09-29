# G0 可行性检查结果

> **历史结果提示（2026-09-29）：** 下文原结论基于 D1–D3，不能用作当前路线裁决。`REVIEW.md` 已指出 patchability 判据错误，`JOURNAL.md` 的 D4/D5 登记要求补做；当前应按“未决”处理，D4 收尾结果将追加在本文末尾。原数据和结论保留用于比较。

## 结论

**G0 不通过。** 冻结总体 22 个缺陷：eligible=2，eligible+unknown=3，低于 6 的停止门槛。即使把唯一的 unknown 后续解决并判成 eligible，也只能得到 3，不能改变本轮 G0 的门槛结论。

- eligible：2/22
- eligible_pending：2/22（两例 eligible 的落败分支均有已观察 Pending）
- ineligible：19/22
- unknown：1/22（G0-05，budget）
- `eligible_if_futures_covered`：3/22，即主门槛 eligible 2，加 D2 辅助列 `site_kind_alt_eligible=yes` 的 G0-01 一例。

## 调整前后计数

下表按最终完整 22 例数据回算；冻结原规则下的反事实基线，保留 D1/D3 所改变的状态。D2 不改变主状态，只新增 futures 系站点辅助计数。D1–D3 登记时实际只完成 2/22，登记记录中的当时部分计数是 eligible=0、eligible_pending=0、ineligible=1、unknown=1；那不是完整总体计数。

| 规则状态 | eligible | eligible_pending | ineligible | unknown | eligible_if_futures_covered |
|---|---:|---:|---:|---:|---:|
| 冻结原规则（完整结果反事实） | 3 | 3 | 17 | 2 | 不适用 |
| 加 D1 | 3 | 3 | 18 | 1 | 不适用 |
| 加 D1+D2 | 3 | 3 | 18 | 1 | 4 |
| 加 D1+D2+D3（最终） | 2 | 2 | 19 | 1 | 3 |

- **D1**：G0-02 Pingora 从 `unknown/no_unique_fix` 变为 `ineligible/site_kind_not_covered`。修复提交在默认分支且其 diff 含修复，第一父提交不含修复；其父版本有测试命中，但站点是自定义 `pingora_timeout::timeout` 包装，父提交 Tokio 版本不可锁定，不能通过已冻结站点门槛。
- **D2**：G0-01 以 31 次 site hit、107 次 branch Pending、76 次 defect-path Pending 满足辅助列条件，`site_kind_alt_eligible=yes`。原始无路径关联探针为 31/108；最终计数采用 D3 关联路径复跑。全体 D2 值为 yes=1、no=7、na=14。
- **D3**：G0-13 从原规则下 eligible/eligible_pending 变为 `ineligible/site_not_hit`：timeout 本身命中 3 次、分支 Pending 3 次，但通用站点活动守卫下 `defect_path_pending=0`。G0-01 和 G0-06 的相关路径计数分别为 76 和 1，均大于 0。

## 不合格原因

| 原因 | 数量 | 个案 |
|---|---:|---|
| `site_kind_not_covered` | 8 | G0-01、02、07、10、15、18、19、20 |
| `tokio_not_patchable` | 8 | G0-03、09、11、14、16、17、21、22 |
| `no_preexisting_test` | 2 | G0-04、08 |
| `site_not_hit` | 1 | G0-13（D3 路径关联未命中） |

G0-05 是唯一 unknown：`unknown/budget`。构建卡在 `faiss-sys` 的 CMake `Could NOT find BLAS`；要求的旧 nightly 下载也遇到 TLS EOF。该项记录耗时 125 分钟，超过冻结的 90 分钟上限 35 分钟，未运行测试，不能视为命中。若另行解决，具体成本是准备兼容 BLAS/MKL 并恢复父提交指定 nightly，再重建、运行预筛最短的既有 sink 测试；即便它成为 eligible，G0 总 eligible 也仅 3。

## 站点与 A′ 启示

- 22 个修复路径中，直接 `tokio::select!` 有 10 个，直接 `tokio::time::timeout` 有 3 个；futures 家族组合子有 4 个（3 个 `futures::future::select`，1 个 `try_join_all`），其余自定义包装、框架/库内部或其他站点有 5 个。Tokio-only 注入面无法覆盖所有被标注为 select/timeout 的根因；D2 把已验证的 futures 替代候选计入后也只有 3/22，不足以挽回本轮门槛。
- 父版本 Tokio 源码版本可确定 19/22；3 个父提交没有 `Cargo.lock` 且 manifest 版本范围不足以确定具体版本。精确版本中 1.17.0 出现 3 次；1.52.3、1.38.1、1.44.2 各 2 次；其余 10 个版本各 1 次。patchability 总体为 yes=9、no=10、unknown=3。版本跨度意味着补丁验证不能只针对当前 Tokio 版本。
- 9 个站点被标成 `generic_site=yes`。其中 2 个相关路径计数为正，1 个为零；其余 6 个因已有确定的主门槛否定项未进入动态路径确认。G0-13 说明仅有通用站点命中会造成假阳性，D3 的路径相关计数确实改变了一个最终分类。
- 5/22 个案运行了候选测试并获得实际站点计数；其中 2/22 使用临时 Docker 外部服务（Databend 的 loopback MinIO、Pingora 的 loopback OpenResty），都已停止。Qdrant 测试使用本地 peer 进程而非 Docker。Kafka/MySQL 等仅在未运行候选所需的服务没有被启动。
- 两个最终 eligible 个案均是 Tokio timeout（G0-06、G0-12），都有正的分支 Pending；因此对于这些具体缺陷，既有测试到达站点并实际经历过未就绪状态。但全总体的候选规模和分类仍未达到路线门槛。

## 修复提交与父提交要点

最终 eligible：

- G0-06 Qdrant：fix `c86aa1a000e2cecfdf1320897910a83afd1f0a66`，parent `80ae57f0b312cdfec05ebcf546c529b06c222a42`。
- G0-12 Qdrant：fix `7e7f19fc4161ebfd64eadcf4816cfd4ebdef460a`，parent `c5f2ba45bd52496d9db90881c92186060b603716`。

调整关键项：

- G0-02 Pingora（D1）：fix `6dcc236a0fcbc6cc22fb48246641eb879899b790`，parent `b646bfb87cc28aebe576f4c2f0cdfda85383bf6a`。
- G0-01 Databend（D2/D3）：fix `ea71a8b560ce4d4147c410827c186ab791beaa66`，parent `75c6f5bff35819d1552af17c4652faf7469d948d`。
- G0-13 Qdrant（D3）：fix `ca9566c5a15dd579fa5224e0ebd9d479215e8e33`，parent `f6ea7b0b36fa39a5bb2c621f434c0e6ebe2bad89`。

22 项完整 `fix_commit`、`parent_sha`、站点、测试链和探针计数见同目录 `eligibility.csv`。

## 耗时

`minutes_used` 按每个缺陷记录的墙钟分钟加总为 **697 分钟（约 11 小时 37 分钟）**。各值为实际执行日志中的约数，G0-05 包含并明确保留的 35 分钟预算超出。

| ID | 个案 | 分钟 | 最终状态 |
|---|---|---:|---|
| G0-01 | Databend #20044 | 70 | ineligible / site_kind_not_covered |
| G0-02 | Pingora #934 | 35 | ineligible / site_kind_not_covered |
| G0-03 | Omicron #3356 | 85 | ineligible / tokio_not_patchable |
| G0-04 | Materialize #12714 | 20 | ineligible / no_preexisting_test |
| G0-05 | RisingWave #26409 | 125 | unknown / budget |
| G0-06 | Qdrant #7530 | 25 | eligible |
| G0-07 | Qdrant #9670 | 22 | ineligible / site_kind_not_covered |
| G0-08 | Hyper #4040 | 18 | ineligible / no_preexisting_test |
| G0-09 | Materialize #28816 | 22 | ineligible / tokio_not_patchable |
| G0-10 | RisingWave #12725 | 27 | ineligible / site_kind_not_covered |
| G0-11 | Materialize #12479 | 24 | ineligible / tokio_not_patchable |
| G0-12 | Qdrant #10338 | 20 | eligible |
| G0-13 | Qdrant #8680 | 25 | ineligible / site_not_hit |
| G0-14 | Iroh #2572 | 16 | ineligible / tokio_not_patchable |
| G0-15 | Databend #17902 | 25 | ineligible / site_kind_not_covered |
| G0-16 | Iroh #2536 | 20 | ineligible / tokio_not_patchable |
| G0-17 | Materialize #12485 | 20 | ineligible / tokio_not_patchable |
| G0-18 | RisingWave #22128 | 20 | ineligible / site_kind_not_covered |
| G0-19 | Hyper #1584 | 20 | ineligible / site_kind_not_covered |
| G0-20 | Tokio #8252 | 18 | ineligible / site_kind_not_covered |
| G0-21 | Deno #14317 | 20 | ineligible / tokio_not_patchable |
| G0-22 | Neon #7062 | 20 | ineligible / tokio_not_patchable |

## 偏离与执行注记

- D1–D3：按用户追加指示先登记并提交后再执行，冻结的 22 项总体、预算和其余判据未放宽。D1 的 Pingora 旧 status 和新 status 均留在 CSV notes；D2 只新增辅助计数；D3 的候选路径条件按登记实施。
- G0-05：超过每缺陷 90 分钟预算 35 分钟后停止并记 unknown/budget；不将构建失败或未运行的候选算作命中。
- G0-12：pytest 的默认 `tmp_path` 曾短暂在 `/tmp/pytest-of-ava/...` 创建小型测试 peer 目录；发现后立即删除。构建与 `target` 均在指定工作树，未在 `/tmp` 构建；无遗留文件。
- G0-02 D1 过程中曾暂留旧行和修订行；终检时合并为唯一 G0-02 行，旧 `unknown/no_unique_fix` 状态仍在 notes 中。最终 CSV 对应冻结名单 22 个唯一 URL。
- 不存在对外操作；目标仓库改动仅在指定 scratch worktree 中，Dropwise 的源代码与冻结调查输入未修改。
