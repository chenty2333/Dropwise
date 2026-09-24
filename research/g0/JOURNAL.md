
## G0-01 — 完成（2026-09-24）

- 修复定位：issue `https://github.com/databendlabs/databend/issues/20044` 由已合并 PR #20050 修复；merge commit `ea71a8b560ce4d4147c410827c186ab791beaa66`，第一父提交 `75c6f5bff35819d1552af17c4652faf7469d948d`。PR 改动文件 `src/query/storages/stage/src/append/lance_dataset/pipeline.rs`（其中加入 inline `#[cfg(test)]` 测试）。
- 站点：父提交 `src/query/service/src/pipelines/executor/processor_async_task.rs:119`，`ProcessorAsyncTask::create` 中 `futures::future::select(left, right)`；`right=finished_notify` 获胜时丢弃 `left` 内层处理器 future。它不是 Tokio `select!`/timeout，`biased` 不适用。Cargo.lock Tokio 1.52.3；该版本源码确认 select! 分支经 `IntoFuture::into_future` 构造，timeout/timeout_at 有 `#[track_caller]`，因此 Tokio 补丁机制本身记 `patchable=yes`，但该缺陷站点不在覆盖范围。
- 预筛：父提交已有 `tests/nox/suites/copy/test_lance.py` 中 5 个 `test_copy_into_lance_*` 候选；调用链经 COPY INTO LANCE pipeline 的 writer/committer async processors 到上述通用执行器站点。
- 动态：在指定临时工作树构建成功并依次运行 5 个候选，全部通过。探针累计 `site_hit=31`、`branch_pending=108`；分测试为 8/32、11/32、5/15、5/21、2/8。临时 MinIO Docker 服务绑定 127.0.0.1:9900–9901，仅运行本次检查，已停止；本地 Databend meta/query 服务亦已停止。探针 diff：`probes/G0-01.diff`。该缺陷判为 `ineligible (site_kind_not_covered)`。
- 用时：51 分钟（含修复定位、静态预筛、构建和测试）。未偏离冻结规则。环境初始缺少 mold/protoc、vendored OpenSSL 依赖缺少 Perl FindBin，且浅克隆缺少版本 tag；仅在临时工作树通过 lld、系统 OpenSSL、scratch-local protoc 和 fetch tags 修复后，构建成功。目标仓库未提交任何改动，测试后已删除 target 目录。

## G0-02 — 完成（2026-09-24）

- 修复唯一性审查：survey 指向 commit `6dcc236a0fcbc6cc22fb48246641eb879899b790`（message: “Make H2 response header reads cancellation safe”），其 message 指向 PR #944。PR #944 声明 `Closes #934`，但 GitHub 记录 `merged_at=null`，故不是已合并修复 PR。commit 的 GitHub 关联只返回通用同步 PR #977；该 PR merge commit `09696b51bc59315353d96686355861604d0bb48c` 的第一父提交为 `e819abf69fbe41b855445d1dc2deadb0aac0ab2c`，而该父提交已包含 fix commit `6dcc236`（compare 显示后者是其祖先）。因此 #977 不能提供修复前父提交；按冻结规则不以它替代，`parent_sha` 与后续站点/测试检查保持未知。
- 判定：`unknown (no_unique_fix)`；未构建、未运行测试、未启动服务。用时 8 分钟。未偏离冻结规则。

## 偏离登记 D1–D3 — 用户指示（2026-09-24 16:44 Asia/Tokyo）

在执行任何 D1–D3 补查、改判、加列或重跑前，先登记本偏离并单独提交。登记时已完成并提交 2/22 项；按原冻结规则的当前计数为：eligible=0、eligible_pending=0、ineligible=1（G0-01/site_kind_not_covered）、unknown=1（G0-02/no_unique_fix）。这只是已完成子集，不是 G0 最终门槛计数；最终 RESULTS.md 将列出完整总体的调整前/后计数。

- D1：issue 没有已合并修复 PR 时，若默认分支存在唯一明确指向该缺陷的修复提交，则核验该提交在默认分支、diff 含修复且第一父提交不含修复，以提交及其第一父作为 fix/parent；适用于 G0-02 及后续全部缺陷。受影响记录在 notes 保留调整前 status，并记录新 status。
- D2：eligibility.csv 新增 `site_kind_alt_eligible`（yes/no/na），只作辅助计数、不改主门槛；RESULTS.md 报 `eligible_if_futures_covered = eligible + site_kind_alt_eligible=yes`。G0-01 按已取得的 31/108 site_hit/branch_pending 补填，但最终 yes/no 还按 D3 的 defect_path_pending 计算。
- D3：通用共享站点但修复函数在别处时，临时加入修复函数入口/出口活动计数及 Drop 守卫；仅当活动计数大于 0 时计入 `defect_path_pending`。eligibility.csv 新增 `generic_site`、`defect_path_pending`。generic_site=yes 的主 eligible 与 eligible_pending 均须 defect_path_pending>0；generic_site=no 沿用原门槛。G0-01 必须在 lance_dataset/pipeline.rs 修复所改函数加探针后重跑原 5 个候选测试；D2 alt 计数亦须满足 defect_path_pending>0。

除上述明确调整外，冻结计划的总体、预算、原门槛其余部分和禁止事项均不变。提交本记录后再开始执行。

## G0-02 D1 复核 — 完成（2026-09-24）

- D1 核验通过：fix commit `6dcc236a0fcbc6cc22fb48246641eb879899b790` 位于默认分支 `origin/main` 历史中；其唯一第一父提交是 `b646bfb87cc28aebe576f4c2f0cdfda85383bf6a`。diff 将 `read_response_header` 从 `resp_fut.take()` 后直接 await 改为经 `poll_response_header`/`poll_fn` 驱动，父提交保留原缺陷代码。修复 inline 新增两个测试，但均不计入既有测试。
- 站点与补查：父提交 `pingora-core/src/protocols/http/v2/client.rs:199`，`Http2Session::read_response_header` 调用的是 `pingora_timeout::timeout` 自定义包装，非直接 Tokio timeout。父提交无 Cargo.lock（manifest 仅 `tokio = "1"`），故按计划 Tokio 版本及 patchable 记 unknown。父提交既有本地候选 `pingora-proxy/tests/test_upstream.rs::test_h2_upstream_no_end_stream_read_timeout` 设定 4 秒 read_timeout，经 `proxy_h2` 到该 timeout；另有使用公网 1.1.1.1 的 `test_https_check`，按外部服务限制未运行。
- 动态：在父提交临时工作树运行本地 H2 候选，通过；`site_hit=1`、内层 future `branch_pending=1`。测试需要的 OpenResty 通过临时 Docker 容器提供，仅映射到 127.0.0.1:8000/8001/8443–8446，已停止。构建在临时无锁解析下选到 Tokio 1.53.1；不将其冒充父提交锁定版本。临时 target 已删除。
- 调整前 status=`unknown`（no_unique_fix）；D1 后 status=`ineligible`（site_kind_not_covered）。D2 为 `na`，D3 `generic_site=no`，故 defect_path_pending 不适用。未偏离已登记规则。

## G0-01 D2/D3 复核 — 完成（2026-09-24）

- 已按 D3 重跑修复前父提交中原有的 5 个 Nox 候选测试，全部通过。为限制通用执行器假阳性，在 `pipeline.rs::append_data_to_lance_dataset` 入口创建 RAII guard，并把 guard 捕获在 pipeline 的 finished callback 中，使计数跨越该 Lance pipeline 的执行期；callback 被执行/丢弃（包括取消销毁）时由 `Drop` 递减。共享 atomic 位于临时 `databend_common_pipeline::core`，执行器只在活动计数大于 0 时累计 `defect_path_pending`。
- D3 动态结果：site_hit=31、branch_pending=107、defect_path_pending=76。分测试为 parallel_manifest_complete 8/30/21，overwrite_raw_path_cleanup_prefix 11/33/17，default_path_with_query_id_directory 5/15/9，detailed_output_is_aggregated_once 5/22/22，string_literal_projection 2/7/7。初始未关联路径探针曾测得 31/108；最终 CSV 采用本次关联路径重跑数值。
- D2+D3：站点是 `futures::future::select`，且既有测试命中、branch_pending>0、defect_path_pending>0，故 `site_kind_alt_eligible=yes`；generic_site=yes。主 status 在调整前后均为 ineligible/site_kind_not_covered（D3 路径条件已满足，但 Tokio 覆盖门槛不满足）；eligible_pending 不变。
- 累计用时约 70 分钟（此次复跑约 19 分钟）；仅使用绑定 127.0.0.1 的临时 MinIO Docker 服务，已停止。target 已删除，目标源码探针仅保留在 `probes/G0-01.diff`。未偏离 D1–D3 登记规则。

## G0-03 — 完成（2026-09-24）

- 修复定位：已合并 PR #3411，merge commit `7e979f08b9c95c98d5e51d08719fd95de549e7e1`，其第一父提交 `6d5da99ca4368488c7fb20039e0dc158ae230e4e`。修复改动 `nexus/src/app/instance.rs`，以 cancel-safe reserve/send 和缓冲替换 select 中可丢弃的 `SinkExt::send`。
- 站点：父提交 `nexus/src/app/instance.rs:1456`，`Nexus::proxy_instance_serial_ws` 内无 `biased;` 的 `tokio::select!`；潜在丢弃分支是 `nexus_write`/`propolis_write`。Cargo.lock Tokio 1.28.2。对应源码 `select.rs` 不用 `IntoFuture::into_future` 构造分支，故 patchable=no；timeout 有 `#[track_caller]`，timeout_at 无，但不影响该 select 结论。
- 预筛：父提交已有 `#[tokio::test] test_serial_console_stream_proxying`，直接调用该方法并在两个方向收发 websocket 数据；PR 修改了该既有测试的日志初始化，按父提交版本仍是候选。其它同名 serial-console 测试不调用 Nexus websocket proxy。
- 动态预算内尝试：首次因 Cargo Git SSL 传输错误中断；改用 Git CLI 后依赖可取回。依父提交 `rust-toolchain.toml` 使用 Rust 1.70.0；构建随后因 `dpd-admin-client` 所需的 `out/downloads/dpd-admin-38735f1f1c8101121553e271e9da0d7a38485687.json` 缺失而失败，官方仓库脚本下载该固定版本时返回 HTTP 404。未运行候选测试；没有 site_hit/branch_pending 动态证据。状态仍为 `ineligible (tokio_not_patchable)`，这是确定的独立否定条件；build_ok=no 已另记，未把静态候选描述为动态命中。
- D2=`na`、D3 `generic_site=no`/defect_path_pending=`na`。无外部服务。target 已清理；探针 diff 仅在 `probes/G0-03.diff`。用时约 85 分钟，未偏离冻结规则。
