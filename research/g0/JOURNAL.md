
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

## G0-04 — 完成（2026-09-24）

- 修复：目标 PR #12714 本身是唯一已合并修复 PR；merge commit `9834520fc2e2c918f16383878f38ab0eb4803c40` 第一父提交 `87495221ccf6b242b07a247f244c3c06be2c04c7`。PR 仅修改 `src/storage/src/source/mod.rs`，没有新增/修改测试文件。
- 站点：父提交 `src/storage/src/source/mod.rs:883` 的 `create_raw_source_simple` 中无 biased 的 `tokio::select!`；竞争分支是 `timestamper.tick()` 与 pin 后的 source future。Tokio 1.18.2 select macro 不经 `IntoFuture::into_future`，故 patchable=no。
- 静态预筛：在父提交受检的 source/test 路径中，`create_raw_source_simple` 只有声明/文档引用，没有调用点；也未找到 `SimpleSource` 实现或调用此站点的既有测试函数，故无候选，`ineligible (no_preexisting_test)`。按冻结规则未进入动态构建/测试。
- D2=`na`；D3 `generic_site=no`，defect_path_pending=`na`。用时 20 分钟；未偏离计划。

## G0-05 — 完成（2026-09-24；dynamic unknown）

- 修复定位：issue #26409 对应已合并 PR #26412；merge commit `e459ee339b8cdbcf8b6cecea2e1fe0a472aa7cdc` 第一父提交 `e83b32ffa85a877dcfcd700da37507d792e7642c`。PR 只改 `src/stream/src/executor/sink.rs`，增加 inline no-op update reader regression tests（这些新增测试不作候选）。
- 站点：父提交 `src/stream/src/executor/sink.rs:822`，`SinkExecutor::execute_consume_log` 的 `tokio::select!`，无 biased；与重建通知竞争的被取消分支是消费 sink/log-reader 的 `future`。Cargo.lock Tokio 1.49.0；宏分支由 IntoFuture 构造且 timeout 有 track_caller，patchable=yes。
- 预筛：父提交已有 7 个测试构造并执行 SinkExecutor，最短候选 `test_empty_barrier_sink` 可沿 SinkExecutor::execute -> execute_consume_log 到达该 select。
- 动态预算内尝试未到测试。一次由调用错误的包名导致 Cargo 拒绝目标；更正为 `omicron-nexus` 的处理属前一缺陷，不计此项。G0-05 构建依赖已解析，但 faiss-sys CMake 报 `Could NOT find BLAS`，导致 `risingwave_stream` 测试不能完成。静态父提交夜间版为 nightly-2026-06-11，rustup 官方下载反复 TLS EOF；重试使用已安装较新 nightly 后仍在 BLAS 处失败。
- 按预算计时 125 分钟，超过冻结 90 分钟 35 分钟；现停止该缺陷并记 `unknown (budget)`，未把失败构建/未运行测试记为命中。该超时为执行偏差，保留在此记录。D2=`na`，D3 `generic_site=no`。target 已清理，无外部服务。

## G0-06 — 完成（2026-09-24）

- 修复定位：目标 PR #7530 已合并；merge commit `c86aa1a000e2cecfdf1320897910a83afd1f0a66`，第一父提交 `80ae57f0b312cdfec05ebcf546c529b06c222a42`。diff 将 `SegmentsSearcher::search` 中 spawn_blocking 返回的 JoinHandle 包装为 AbortOnDropHandle；未新增或修改测试文件。父提交保留原先裸 JoinHandle。
- 取消站点：父提交 `lib/collection/src/shards/local_shard/search.rs:137`，`LocalShard::do_search_impl` 的 `tokio::time::timeout(timeout, search_request)`；到期丢弃内层 `SegmentsSearcher::search` future，令已排队的 blocking 搜索 JoinHandle 继续存活。站点类型为直接 Tokio timeout。父提交 Cargo.lock Tokio 1.47.1；该源码的 select! 通过 IntoFuture 构造，timeout 有 `#[track_caller]`、timeout_at 没有，patchable=yes。
- 预筛唯一候选 `lib/collection/src/tests/hw_metrics.rs::test_hw_metrics_cancellation`：父提交既有 Tokio 测试，明确将 LocalShard::do_search 的超时设为 10ms 并断言超时，调用链经过目标 timeout 及被修复的 SegmentsSearcher::search。
- 动态：候选测试通过，`site_hit=1`、`branch_pending=1`。D3 按通用站点规则在被改动的 `SegmentsSearcher::search` 放置 Drop 守卫活动计数，timeout 内层 future 首次 Pending 时仅在活动计数大于 0 时计 defect path；`defect_path_pending=1`。故 generic_site=yes，主状态 eligible，eligible_pending=yes。D2=`na`。无外部服务。探针 diff：`probes/G0-06.diff`。
- 初次构建因缺少 protoc 停止；只在临时工作树提供 protoc 31.1、其 include 文件并补齐探针编译错误后，构建和测试成功。用时约 25 分钟；测试后已移除临时工具、target 和源改动。无计划外规则变更。

## G0-07 — 完成（2026-09-24）

- 修复定位：issue #9670 唯一关联的 PR #9671 已合并；merge commit `7b59e7c40c800d4a94aa014461f92c7bff9aa214`，GitHub commit API 核实第一父提交为 `f65a4d1071f0f8e6a8abc6bbaebb46954490a515`。PR 新增 `lib/collection/src/shards/local_shard/optimizer_config_update_tests.rs`，新增回归测试不计入候选。
- 站点：父提交 `lib/collection/src/collection/collection_ops.rs:377`，`Collection::recreate_optimizers` 的 `future::try_join_all(updates)`。其中一个 shard 更新返回 Err 时，try_join_all 会立即返回并丢弃其它正在执行的 `ReplicaSet::on_optimizer_config_update` futures。PR diff 把它替换为 `join_all`；这是 futures-util 组合子而非 Tokio `select!`/timeout。父提交 Tokio 1.52.3；对应 select macro 通过 IntoFuture 构造、timeout 有 `#[track_caller]`，Tokio patchable=yes，但本缺陷站点不受 A′ 覆盖。
- 预筛候选：两组 OpenAPI collection update 测试修改 HNSW/optimizer/vector 核心配置，调用链经 collection metadata 更新到后台 optimizer recreation 和该 try_join_all；另有 `test_dirty_shard_survives_update_collection` 经 UpdateCollection 重放到相同路径。后者显式单 shard；OpenAPI fixture 默认 `default_shard_number()=1`。父提交没有现成的跨 shard「一支失败、另一 Pending」测试；PR 新回归测试专门添加了该场景。
- 判定：`ineligible (site_kind_not_covered)`。D2=`no`：既有候选没有缺陷所需的被丢弃 Pending sibling（均为单 shard/default 单 shard）；新增的跨 shard测试按规则不计。D3 `generic_site=no`，因为 site 与修复修改的函数相同，`defect_path_pending=na`。因主门槛有确定否定证据，未构建运行动态候选；无外部服务。用时约 22 分钟。D1 对本已合并 PR 的规则无状态变更。

## G0-08 — 完成（2026-09-24）

- 修复定位：issue #4040 由已合并 PR #4042 修复。merge commit `5b17a69ebcf969471c1a19b25ed2cb81299d1be6`，第一父提交 `7211ec25eff2ea6ee783817fee2a221d4eb2ed03`。PR 改 `src/client/dispatch.rs`、`src/proto/h2/client.rs`、`src/proto/h2/mod.rs`，并修改 `tests/client.rs` 新增回归测试 `h2_pipe_task_cancelled_on_response_future_drop`；新增测试不计入既有测试。
- 站点：父提交 `src/client/dispatch.rs:360`，`SendWhen::poll` 中 `Callback::poll_canceled(cx)` 的手动轮询分支。调用者丢弃 response future 后，该分支让 send task 结束但未通知 H2 pipe task，pipe task 继续持有 SendStream。外部 caller timeout 是缺陷的触发方式；Hyper 代码中没有对应 Tokio `select!`/timeout 表达式，站点类型记 `other`。父提交没有 Cargo.lock（manifest 仅 `tokio = "1"`），版本/patchable=unknown。
- 预筛：父提交的 `tests/client.rs` 没有既有测试在请求体仍 Pending 时取消/超时丢弃 HTTP/2 response future。现有 keep-alive timeout 测试不是该取消路径；唯一精确回归测试由 PR 新增。无候选，`ineligible (no_preexisting_test)`；未构建/运行，无外部服务。D2=no；D3 generic_site=no（触发取消的是仓库外 caller，Hyper 内无共享 Tokio 站点需要关联）。用时约 18 分钟。

## G0-09 — 完成（2026-09-24）

- 修复定位：目标 PR #28816 本身是已合并修复；merge commit `5f02d4fe4120fc97ed45d31d88b5c6f8839935bb`，第一父提交 `62875cc04674971788bdf5141eed69a22ef96375`。PR 改 compute/service 源文件，没有测试文件改动。
- 站点：父提交 `src/compute-client/src/controller/replica.rs:241`，`ReplicaTask::run_message_loop` 无 `biased;` 的 `tokio::select!`；`response = client.recv()`（`SequentialHydration::recv`）分支在 `command_rx.recv()` 分支获胜时会被丢弃。修复将 `SequentialHydration::observe_response` 的异步发送改为同步 channel 通知，以保证 recv cancel-safe。父提交 Cargo.lock Tokio 1.38.0；其 select.rs 直接收集分支表达式，不经 `IntoFuture::into_future`，timeout 有 `#[track_caller]`、timeout_at 没有，故 patchable=no。
- 预筛候选：父提交已有 `test/testdrive/sequential-hydration.td`，通过创建计算集群/物化视图并改变 replica factor，可能运行 compute replica message loop 并触达上述 response 分支。该站点是共享通用 message loop，修复位于其调用的 `SequentialHydration::observe_response/recv`，故 generic_site=yes（D3）。D2=`na`。
- 判定：`ineligible (tokio_not_patchable)`，为确定负面条件。候选未动态运行：检查 Tokio 1.38 宏源码后已确定 A′ 的冻结 patchability 条件不满足；不构建庞大的 Materialize testdrive 服务。没有 site_hit/branch_pending/defect_path_pending 动态值，不把静态调用链写成命中。尝试读取现成 mzcompose CLI 时，它开始在 scratch 创建 Python 虚拟环境并安装依赖；未启动 Docker 服务/项目构建即停止并清理。D3 计数未运行，故 defect_path_pending 留空。用时约 22 分钟。D1 对已合并修复 PR 无状态变更。
