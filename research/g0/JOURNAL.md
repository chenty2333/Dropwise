
## G0-01 — 完成（2026-09-24）

- 修复定位：issue `https://github.com/databendlabs/databend/issues/20044` 由已合并 PR #20050 修复；merge commit `ea71a8b560ce4d4147c410827c186ab791beaa66`，第一父提交 `75c6f5bff35819d1552af17c4652faf7469d948d`。PR 改动文件 `src/query/storages/stage/src/append/lance_dataset/pipeline.rs`（其中加入 inline `#[cfg(test)]` 测试）。
- 站点：父提交 `src/query/service/src/pipelines/executor/processor_async_task.rs:119`，`ProcessorAsyncTask::create` 中 `futures::future::select(left, right)`；`right=finished_notify` 获胜时丢弃 `left` 内层处理器 future。它不是 Tokio `select!`/timeout，`biased` 不适用。Cargo.lock Tokio 1.52.3；该版本源码确认 select! 分支经 `IntoFuture::into_future` 构造，timeout/timeout_at 有 `#[track_caller]`，因此 Tokio 补丁机制本身记 `patchable=yes`，但该缺陷站点不在覆盖范围。
- 预筛：父提交已有 `tests/nox/suites/copy/test_lance.py` 中 5 个 `test_copy_into_lance_*` 候选；调用链经 COPY INTO LANCE pipeline 的 writer/committer async processors 到上述通用执行器站点。
- 动态：在指定临时工作树构建成功并依次运行 5 个候选，全部通过。探针累计 `site_hit=31`、`branch_pending=108`；分测试为 8/32、11/32、5/15、5/21、2/8。临时 MinIO Docker 服务绑定 127.0.0.1:9900–9901，仅运行本次检查，已停止；本地 Databend meta/query 服务亦已停止。探针 diff：`probes/G0-01.diff`。该缺陷判为 `ineligible (site_kind_not_covered)`。
- 用时：51 分钟（含修复定位、静态预筛、构建和测试）。未偏离冻结规则。环境初始缺少 mold/protoc、vendored OpenSSL 依赖缺少 Perl FindBin，且浅克隆缺少版本 tag；仅在临时工作树通过 lld、系统 OpenSSL、scratch-local protoc 和 fetch tags 修复后，构建成功。目标仓库未提交任何改动，测试后已删除 target 目录。

## G0-02 — 完成（2026-09-24）

- 修复唯一性审查：survey 指向 commit `6dcc236a0fcbc6cc22fb48246641eb879899b790`（message: “Make H2 response header reads cancellation safe”），其 message 指向 PR #944。PR #944 声明 `Closes #934`，但 GitHub 记录 `merged_at=null`，故不是已合并修复 PR。commit 的 GitHub 关联只返回通用同步 PR #977；该 PR merge commit `09696b51bc59315353d96686355861604d0bb48c` 的第一父提交为 `e819abf69fbe41b855445d1dc2deadb0aac0ab2c`，而该父提交已包含 fix commit `6dcc236`（compare 显示后者是其祖先）。因此 #977 不能提供修复前父提交；按冻结规则不以它替代，`parent_sha` 与后续站点/测试检查保持未知。
- 判定：`unknown (no_unique_fix)`；未构建、未运行测试、未启动服务。用时 8 分钟。未偏离冻结规则。
