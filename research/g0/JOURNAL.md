
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

## G0-10 — 完成（2026-09-24）

- 修复定位：目标 PR #12725 本身已合并；merge/fix commit `9d439b7df499ceb2747abd528e7f3f316bc50e69`，第一父提交 `e5e17fa8ae4db46932b6a1f953ea060ee87c8ade`。PR 在 `src/stream/src/common/log_store_impl/kv_log_store/mod.rs` 修改 inline tests 并新增 `test_cancellation_safe`，按规则不作候选。
- 站点：父提交 `src/connector/src/sink/kafka.rs:525`，`KafkaLogSinker::consume_log_and_sink` 调 `futures::future::select(pin!(log_reader.next_item()), pin!(self.future_manager.next_truncate_offset()))`；truncate-offset 分支先完成时丢弃 `next_item` future，造成已弹出的 flushed chunk 丢失。父提交 Cargo.lock Tokio 1.32.0，源码 `select.rs` 不用 IntoFuture 构造（timeout 有 `#[track_caller]`、timeout_at 无），patchable=no；但实际站点本身为 futures::future::select。
- 预筛：父提交已有 `e2e_test/sink/kafka/create_sink.slt`，创建 Kafka sink 并插入数据，可沿 Kafka sink 运行到该 futures select 和 KvLogStoreReader。KV log-store 的既有单测直接测 reader，不调用 Kafka select。动态候选未运行：主判定因 site kind 已确定不在 Tokio A′ 覆盖内；该 SLT 还需构建 RisingWave 并临时提供 127.0.0.1:29092 Kafka，未启动任何服务。
- 判定 `ineligible (site_kind_not_covered)`；D2=`no`，因只有静态候选，没有动态 site_hit/branch_pending 证据，不把可达性假定为已命中。D3 `generic_site=no`（Kafka 专用 sink loop），`defect_path_pending=na`。用时约 27 分钟。D1 对目标合并 PR 无状态变更。

## G0-11 — 完成（2026-09-24）

- 修复定位：目标 PR #12479 已合并；merge/fix commit `e5ef22d1c21434a6b4041386727317612d8d4162`，第一父提交 `a838ec6e67692e7dc55e7234d86aeecd7922ddb8`。PR 改 `src/coord/src/coord.rs` 和 `src/dataflow-types/src/client/controller.rs`，未新增/修改测试。
- 站点：父提交 `src/coord/src/coord.rs:745`，`Coordinator::serve` 中带 `biased;` 的 `tokio::select!`；`self.dataflow_client.recv()` 分支在 internal command 或 external command 获胜时被丢弃。修复把它拆为 select 内 cancel-safe `ready()` 和 handler 中 `process()`。父提交 Tokio 1.17.0；select 宏不经 IntoFuture 构造、timeout 有 `#[track_caller]`、timeout_at 无，patchable=no。
- 预筛：父提交已有 `test/testdrive/coordinator-multiplicities.td`（SQL/SELECT）及 `test/sqllogictest/cluster.slt`（建集群、物化视图和查询），可沿 coordinator 消息环到达此 select。站点为共享 Coordinator 主消息循环，修复函数 Controller::recv 在别处，D3 `generic_site=yes`。D2=`na`。
- 判定 `ineligible (tokio_not_patchable)`。候选未动态运行：Tokio 1.17.0 的 IntoFuture 条件是确定否定；因此无动态命中数，D3 的 defect_path_pending 留空，不把静态调用链当作动态证据。未启动服务。用时约 24 分钟。D1 对已合并修复 PR 无状态变更。

## G0-12 — 完成（2026-09-24）

- 修复定位：目标 PR #10338 已合并；merge/fix commit `7e7f19fc4161ebfd64eadcf4816cfd4ebdef460a`，第一父提交 `c5f2ba45bd52496d9db90881c92186060b603716`。PR 在 `lib/storage/src/content_manager/consensus_manager.rs` 修改 timeout/awaiter 清理，并在同文件新增 6 个并发/超时回归测试；这些新增测试不计入候选。PR 还处理 `await_for_multiple_operations` 的第二个 timeout 和未 poll 就丢弃的 awaiter guard。
- 站点：父提交该文件第 763 行，`ConsensusManager::await_receiver` 中 `tokio::time::timeout(wait_timeout, receiver.recv())`。超时丢弃 receiver 后旧代码直接移除共享操作 map sender，导致其它尚未完成的 waiters 收到 sender dropped。父提交 Cargo.lock Tokio 1.53.1；select 分支经 IntoFuture 构造，timeout 和 timeout_at 都有 `#[track_caller]`，patchable=yes。
- 预筛：既有 `tests/consensus_tests/test_cluster_operation_coalescing.py::test_cluster_operation_coalescing` 启动三节点后并发循环删除同一 collection；调用链 API -> Dispatcher -> propose_consensus_op_with_await -> await_receiver。它是该超时站点的候选。
- 动态：在临时父提交源码上加 timeout/site 计数和内层 receiver 首次 Pending 计数，构建成功后该候选通过。探针输出各进程局部累计值，至少一个 peer 的计数达到 `site_hit=88`、`branch_pending=88`；CSV 记录可观测最大值而非跨 peer 求和。探针 diff：`probes/G0-12.diff`。无外部服务或 Docker。
- 环境偏差：初次构建缺少 protoc，提供 scratch-only protoc 31.1 后构建成功。pytest 的既有 `tmp_path` 默认把少量 peer 数据放在 `/tmp/pytest-of-ava/...`，违反了用户指定的工作目录限制；测试完成后立即删除该测试目录，检查时 `/tmp` 可用空间仍为 2.8 GiB，构建/target 未放入 `/tmp`。该路径偏差会在 RESULTS.md 透明列出。target、protoc、临时父快照和目标源码改动均已清理。
- 判定：`eligible`，因此 `eligible_pending` 也满足。D2=`na`；D3 `generic_site=no`，站点和修复清理代码同处 awaiter helper，`defect_path_pending=na`。D1 无 status 变化。用时约 20 分钟。

## G0-13 — 完成（2026-09-24）

- 修复定位：目标 PR #8680 已合并；merge/fix commit `ca9566c5a15dd579fa5224e0ebd9d479215e8e33`，第一父提交 `f6ea7b0b36fa39a5bb2c621f434c0e6ebe2bad89`。PR 只改 `lib/collection/src/update_workers/update_worker.rs`，没有新增测试文件。
- 站点：父提交 `lib/collection/src/shards/local_shard/shard_ops.rs:127`，`LocalShard::update` 中 `tokio::time::timeout(timeout, receiver)`；到期会丢弃 feedback oneshot receiver，但串行 update worker 仍可能停在 `wait_for_deferred_points_ready`。父提交 Tokio 1.51.1；select macro 用 IntoFuture 构造，timeout 有 `#[track_caller]`、timeout_at 无，patchable=yes。
- 预筛：父提交已有 `test_shard_transfer_deferred.py::test_shard_transfer_includes_deferred_points`（snapshot/stream_records）及 `test_resharding_deferred.py::test_resharding_transfer_deferred_points`（up/down），它们创建 prevent_unoptimized/deferred-point 场景并调用 wait=true 更新。该 LocalShard::update timeout 是共享站点，而修复在 update worker helper，故按 D3 generic_site=yes。
- 动态：构建通过；4 个既有候选参数用例全部通过。探针计数的每进程最大值为 site_hit=3、branch_pending=3；在被修复的 `wait_for_deferred_points_ready` 活动期计数仅当 site 分支 Pending 才增加，最终 `defect_path_pending=0`。因此 D3 路径条件失败，`ineligible (site_not_hit)`，尽管基础 timeout 表达式有命中。D2=`na`。探针 diff：`probes/G0-13.diff`。候选 peers 仅在回环地址启动，无 Docker 外部服务；pytest basetemp 明确设在 `/home/ava/dropwise-g0-work/qdrant` 下。
- 首次 pytest 因仓库配置要求 `pytest-xdist` 插件而未启动测试；补上该依赖后运行成功。protoc 31.1 只放在临时工作树。测试目录、target、protoc、父提交快照及目标源码改动均已清理。用时约 25 分钟；D1 对已合并 PR 无状态变更。

## G0-14 — 完成（2026-09-24）

- 修复定位：目标 PR #2572 已合并；merge/fix commit `32bb0f3be432676ca49473e75c7eb00db32a3673`，第一父提交 `8e4e586cece3968700a13562058f3a5c152c1805`。PR 仅改 `iroh-gossip/src/net.rs`，没有测试文件改动。
- 站点：父提交同文件第 656 行，`connection_loop` 中带 `biased;` 的 `tokio::select!`；`read_message(&mut recv, ...)` 在 `send_rx.recv()` 先完成时被丢弃，即使它已消费部分帧。PR 把 select 改为持久 send/receive 两个循环的 `tokio::try_join!`。父提交 Tokio 1.38.1；宏直接存放分支表达式、不经 IntoFuture，timeout 有 `#[track_caller]`、timeout_at 无，patchable=no。
- 预筛：父提交已有 `iroh-gossip/src/net.rs::test::gossip_net_smoke`，启动三个端点并加入 topic/广播，调用链到 `endpoint_loop -> Gossip::handle_connection -> connection_loop -> select!`。但宏源码已构成确定 patchability 否定，未进行动态构建/运行。D2=`na`、generic_site=no（连接循环专用于 gossip 且修复直接改同一函数）、defect_path_pending=`na`。状态 `ineligible (tokio_not_patchable)`；无外部 Docker/服务。用时约 16 分钟，D1 不改变状态。

## G0-15 — 完成（2026-09-24）

- 修复定位：目标 PR #17902 已合并；merge/fix commit `ebeab9379e51fd787c6756273d9fbfc812e1ce57`，第一父提交 `11d99804b86b2ac3ab694c9f2368ab1681123962`。PR 只改 `src/common/storage/src/operator.rs`，未新增/修改测试。
- 站点：Databend 在父提交 `src/common/storage/src/operator.rs:141` 构造 OpenDAL `TimeoutLayer`；其依赖版本为 OpenDAL 0.53.1，内部 `TimeoutAccessor::timeout` 在 `src/layers/timeout.rs:196` 调 `tokio::time::timeout`（IO timeout 也有对应路径）。超时会丢弃 OpenDAL 的底层存储操作 future；PR 将此 layer 移到其它 storage layers 之前，以保护重试/运行时层次的可重入性。该表达式在依赖内部，不是 Databend 自己的 select/timeout 站点，因此 `site_kind=other`。父提交 Tokio 1.44.2 的宏使用 IntoFuture，timeout 有 `#[track_caller]`、timeout_at 无，底层 Tokio 函数 patchable=yes，但主站点类别不覆盖。
- 预筛：父提交已有 `src/query/service/tests/it/storages/fuse/operations/commit.rs::test_fuse_occ_retry` 和 `src/query/service/tests/it/storages/fuse/table.rs::test_fuse_table_normal_case`，经 Fuse table 存储访问可到 Databend `build_operator` 并进入 OpenDAL layer。没有动态运行：由依赖内部 timeout 站点类别确定不满足主门槛。D2=no；D3 generic_site=yes（共享 OpenDAL timeout layer，修复位于 Databend operator builder），路径计数未测所以 defect_path_pending 留空。无外部服务。用时约 25 分钟。D1 对合并 PR 无状态变更。

## G0-16 — 完成（2026-09-24）

- 修复定位：目标 PR #2536 关闭未合并；survey 指向的关联修复 PR #2539 已合并，并明确说明是 #2536 分阶段移除 Flume 修复的一部分。其 merge commit `22314a18228799e26de8ba2c0e44b45aec3b2af4`，第一父提交 `9052905d0d75d62c761139f02294d6abc1c53af6`。合并修复改 Cargo.lock/iroh-net 源文件，没有测试文件改动。
- 站点：父提交 `iroh-net/src/net/netmon/actor.rs:115`，`Actor::run` 带 `biased;` 的 `tokio::select!`；`self.mon_receiver.recv_async()` 在定时器分支获胜时被丢弃，Flume 可能丢失 wake notification。父提交 Tokio 1.38.1；对应宏不经 IntoFuture 构造，timeout 有 `#[track_caller]`、timeout_at 无，patchable=no。
- 预筛：父提交既有 `iroh-net/src/net/netmon.rs::tests::test_smoke_monitor` 创建 Monitor 并启动 Actor::run，可触达该 select。因 Tokio patchability 是确定负面，本项未运行动态测试。D2=`na`，generic_site=no（修复直接更换同一 net monitor 消息通道），defect_path_pending=`na`。状态 `ineligible (tokio_not_patchable)`；无外部服务。用时约 20 分钟。D1 对“目标 PR 关闭、但有关联合并修复 PR”的处理不改变最终状态。

## G0-17 — 完成（2026-09-24）

- 修复定位：目标 PR #12485 已合并；merge/fix commit `c7e70fb59cddeae461123ebba8032d51d5abd467`，第一父提交 `5216a08830e42526fa8edb26e21f465e7991749b`。PR 改 `src/dataflow-types/src/client.rs`、`client/partitioned.rs`、`src/materialized/src/lib.rs`，无测试文件改动。
- 站点：父提交 `src/coord/src/coord.rs:745`，`Coordinator::serve` 带 `biased;` 的 `tokio::select!` 中 `self.dataflow_client.recv()` 分支；内部/外部命令获胜时可在分区 client 部分重连期间丢弃该 future，导致对新旧集群连接状态混淆。修复在 Partitioned::recv 保存重连状态并阻止重连期间 send。父提交 Tokio 1.17.0，不经 IntoFuture 构造 select 分支，timeout 有 `#[track_caller]`、timeout_at 无，patchable=no。
- 预筛：既有 `test/testdrive/coordinator-multiplicities.td` 与 `test/sqllogictest/cluster.slt` 可经 SQL command/query 调用 coordinator 消息循环；父提交无现成的分区连接中途失败/重连测试。站点为共享 Coordinator loop、修复函数在 Partitioned client 另一文件，D3 generic_site=yes；D2=`na`。
- 判定 `ineligible (tokio_not_patchable)`，为确定否定；候选未动态运行。没有 site_hit/branch_pending/defect_path_pending 动态值，未将通用调用链表述成缺陷路径命中。无服务启动。用时约 20 分钟。D1 对已合并 PR 无状态变更。

## G0-18 — 完成（2026-09-25）

- 修复定位：目标 PR #22128 已合并；merge/fix commit `1a825138bdf0d0e0e218bd339c2f5b4eb7fbc793`，第一父提交 `c0ab2178e3029882de40806a3738d7826d40566d`。PR 改 MySQL CDC source/client 及 e2e SQL；新增 ignored unit test `test_mysql_async_with_connection_pool`，其中的 tokio::time::timeout 最小复现按规则不计候选。
- 站点：父提交 `src/connector/src/source/cdc/external/mysql.rs:462`，`MySqlExternalTableReader::snapshot_read_inner` await `mysql_async::Conn::exec_drop("SET time_zone", ...)`。future 被丢弃会令 mysql_async 连接残留协议字节，之后复用可导致空读/EOF。父提交该函数周围没有 Databend 自己的 Tokio select/timeout 站点；PR 描述的 timeout 是新增复现方式，取消源实际由调用者/驱动发起，故 site_kind=other。父提交 Tokio 1.44.2 的宏/timeout 源符合补丁条件，patchable=yes，但站点不在冻结覆盖类型中。
- 预筛候选：父提交既有但 ignored 的 `test_mysql_table_reader` 直接调用 snapshot_read；既有 `e2e_test/source_legacy/cdc/mysql_cdc.sql` 驱动 MySQL CDC snapshot。同文件新增 timeout 单测不计。未运行动态候选：站点是 mysql_async future 的取消，不是本仓库可定位的 Tokio timeout；需要的 MySQL e2e 服务也未启动。
- 判定 `ineligible (site_kind_not_covered)`。D2=no；D3 generic_site=no（MySQL reader 专用路径，修复直接改此路径），defect_path_pending=na。无外部服务。用时约 20 分钟；D1 对目标合并修复 PR 无状态变化。

## G0-19 — 完成（2026-09-25）

- 修复定位：target PR #1584 关闭未合并；survey 的关联修复 PR #1585 已合并，merge/fix commit `f2d464ac79b47f988bffc826b80cf7d107f80694`，第一父提交 `1f95f58837e5fd78b0e9bb1a51276c38bc9d559c`。修复 PR 修改 `src/client/mod.rs`、pool/mock 辅助代码和 `src/client/tests.rs`，新增 `checkout_win_allows_connect_future_to_be_pooled`；新增测试不计候选。
- 站点：父提交 `src/client/mod.rs:321`，`Client::send_request` 中 `checkout.select(connect)`（futures 0.1 的 Future::select）；pool checkout 获胜时旧代码丢弃尚未完成的 connect future，不能在之后将连接放回池。父提交无 Cargo.lock，Cargo.toml 声明 Tokio 0.1.7，故 Tokio 精确版本/patchable=unknown；此站点本身是 futures 组合子而非 Tokio。
- 预筛候选：父提交已有 `src/client/tests.rs::retryable_request` 和 `conn_reset_after_write`，都调用 Client::request 并沿 `send_request -> checkout.select(connect)`。D2 只能在运行后确认 futures alternative。
- 动态尝试：为既有 `retryable_request` 做构建，但 Cargo 在编译前解析失败：无锁父提交的依赖解析器将全部可用 `spmc 0.2.x` 版本判为 yanked。没有运行测试、没有 site_hit/branch_pending；没有外部服务。主状态仍为确定 `ineligible (site_kind_not_covered)`，因为实际站点是 futures 0.1 select 而非 Tokio。D2=`no`（尚无动态正证据），D3 generic_site=no（修复修改同一 Client::send_request 路径），defect_path_pending=na。用时约 20 分钟。D1 将原 closed-unmerged target 改为已合并的关联 PR #1585，但无主 status 变化。

## G0-20 — 完成（2026-09-25）

- 修复定位：目标 PR #8252 已合并；merge/fix commit `91d3b4c0bccf2234fc3ed19e605e2cd402f19437`，第一父提交 `a46338401b9e0ffc9bd68c31100ee99cee717481`。PR 改 Tokio runtime time-wheel/entry 源，并新增 `tokio/src/runtime/time_alt/tests.rs` 中 `insert_of_already_cancelled_entry_does_not_enter_wheel` 与 `cancel_races_with_insert`，新测试不计入候选。
- 站点：父提交 `tokio/src/runtime/time_alt/wheel/mod.rs:68`，`Wheel::insert`。EntryHandle 在 cancellation sender 注册前被取消时，旧代码仍把它加入 wheel，令无可用 cancellation sender 的 entry 只能等自然过期或 runtime 关闭。站点类型记 other：取消源是任意 caller 丢弃 Sleep/timeout future，Tokio 内部 wheel insert 不是一个可定位的用户 `tokio::select!`/timeout 表达式。父提交 Tokio package version 1.53.0（本身无 Cargo.lock）；宏使用 IntoFuture，timeout/timeout_at 有 `#[track_caller]`，所以 Tokio 源形态 patchable=yes，但 site kind 不覆盖。
- 预筛：父提交已有 `tokio/tests/time_timeout.rs::future_and_timeout_in_future`、`timeout_is_not_exhausted_by_future`，通过 runtime timer driver 到 wheel insert；time_alt/tests.rs 现有 cancellation queue tests 不直接调用 wheel insert。虽然候选静态可达，主判定因站点类别有确定负面证据，未运行动态测试。D2=no、D3 generic_site=no（fix 直接改 Wheel::insert/EntryHandle），defect_path_pending=na。无外部服务。用时约 18 分钟；D1 对目标 merged PR 无状态变更。

## G0-21 — 完成（2026-09-25）

- 修复定位：目标 PR #14317 已合并；merge/fix commit `3d1123f8b09cecfa57a93d8b8b7d19af2b45f070`，第一父提交 `c30d95f2e36cb3519e1e23c0934b388ebba6bc2c`。PR 只改 `cli/file_watcher.rs`，无测试文件改动。
- 站点：父提交 `cli/file_watcher.rs:190`，`watch_func` 的无 `biased;` `tokio::select!`；`next_restart(&mut resolver, &mut receiver)` 分支在 operation future 完成时被丢弃，而 DebouncedReceiver::recv 的局部路径集合会随 future 一起丢失。父提交 Tokio 1.17.0，select macro 不使用 IntoFuture 构造，timeout 有 `#[track_caller]`、timeout_at 无，patchable=no。
- 预筛：父提交已有 `cli/tests/integration/watcher_tests.rs` 中 `run_watch`、`bundle_js_watch`、`lint_watch_test` 等 CLI `--watch` 测试，沿 `watch_func -> next_restart -> DebouncedReceiver::recv` 到达 select。站点是共享 watcher loop，修复在 helper 另处，D3 generic_site=yes。D2=`na`。
- 判定 `ineligible (tokio_not_patchable)`；Tokio 源码已提供确定否定条件，未运行动态候选，因而没有 site_hit/branch_pending/defect_path_pending 测量；D3 path 计数留空。无外部服务。用时约 20 分钟。D1 对目标 merged PR 无状态变更。

## G0-22 — 完成（2026-09-25）

- 修复定位：issue #7062 的任务列表含一组相关 PR；按冻结规则，#7235 是唯一已合并且标题/说明直接针对 dangling `handle_walreceiver_connection` task 的修复 PR（#7234 为 preliminary refactor，#7260/#7233 为 follow-on refactor）。merge/fix commit `cdf12ed008c27fa7d59e296c498ce34ce681bddb`，第一父提交 `12512f31736a5c5b3d3973c5c5cfd43dd58acb3d`。PR 改 walreceiver source files，无测试文件改动。
- 站点：父提交 `pageserver/src/tenant/timeline/walreceiver.rs:99`，task_mgr-spawned manager task 中 `tokio::select!`；`connection_manager_loop_step` 在 `task_mgr::shutdown_watcher()` 获胜时被 drop，可能中断 `drop_old_connection`，使已取出的 connection task handle 丢失。站点无 `biased;`。父提交 Cargo.lock Tokio 1.36.0；其 select macro 不用 IntoFuture 构造，timeout 有 `#[track_caller]`、timeout_at 无，patchable=no。
- 预筛：已有 `test_runner/regress/test_timeline_delete.py` 删除 timeline，调用 Timeline::shutdown/task_mgr shutdown path，可到 walreceiver manager outer task select。connection_manager 内部测试只测单步状态，不是该 outer select。修复涉及通用任务 loop 和另一个 helper，D3 generic_site=yes；D2=`na`。
- 判定 `ineligible (tokio_not_patchable)`。候选未动态运行：Tokio 1.36.0 是确定负面条件；故无动态 site_hit/branch_pending，D3 defect_path_pending 留空。无外部服务。用时约 20 分钟。D1 依上述合并修复 PR 选择不改变 status。

## CSV 记录归一化（2026-09-25）

- 终检发现 D1 重新处理 G0-02 时曾保留一条原 `unknown/no_unique_fix` 旧行，同时追加了修订行，导致 eligibility.csv 暂有 23 行记录。现合并为每个冻结缺陷唯一一行，保留 D1 后 `ineligible/site_kind_not_covered` 的最终字段，并在 notes 中明确保存调整前 `unknown/no_unique_fix`。冻结总体仍为 22 个 URL；仅消除重复记录，不改变判定规则或缺陷状态。

## G0-13 D3 状态变更注记（2026-09-25）

- 完整总体的计数回算确认：若不应用 D3，G0-13 的既有候选已观察到 `site_hit=3`、`branch_pending=3`，且 Tokio timeout 可补丁，按原门槛会为 eligible/eligible_pending；D3 要求通用站点和修复路径相关联，但 `defect_path_pending=0`，故最终改为 `ineligible/site_not_hit`。已在 CSV notes 留存该调整前后状态。规则不变。

## 偏离登记 D4 — 项目负责人指示（2026-09-25）

依据 `REVIEW.md`，冻结计划第 2 节的 patchable 判据有误。本记录在任何 D4 重判或动态确认之前登记并单独提交。本轮由项目负责人侧（Claude）直接执行，不经 Codex。

登记时计数（D1–D3 后的最终值）：eligible=2、eligible_pending=2、ineligible=19、unknown=1、eligible_if_futures_covered=3。

- **D4 判据更正。** 原判据要求“select! 分支经 `IntoFuture::into_future` 构造”，这是把 tokio 1.53.1 上补丁的具体位置误当成了可否打补丁的前提。更正为：只要该 Tokio 版本的 `select!` 宏把各分支 future 逐个放入一个元组（1.x 形式为 `( $( $fut , )+ )` 或 `( $( IntoFuture::into_future(...) , )+ )`），就能用按版本适配的补丁逐分支包装，`patchable=yes`。`timeout`/`timeout_at` 是否带 `#[track_caller]` 不影响可行性，补丁可以自行加上。对每个 Tokio 版本，在 `patchable_reason` 中记录补丁应改的文件和行号。父提交无 `Cargo.lock` 时，在临时工作树中用父提交时间点可解析的版本生成 lockfile，并记录方法和所得版本。
- **补做范围。**
  - 对 G0-03、09、11、14、16、17、21、22 按 D4 重判 patchable。其余条件未被否定者，按原第 4 节与 D3 做动态确认：临时探针，最多运行 10 个预筛候选，通用站点记录 `defect_path_pending`，每例 90 分钟，超时记 `unknown (budget)`。
  - G0-04 与 G0-10 只更新 patchable 列，主状态由其它确定否定条件决定，不重做。
  - G0-05 不重做。
- **新增两列。**
  - `harness_kind`：cargo_test / cargo_integration / sqllogictest / testdrive / pytest / other。
  - `baseline_wall_seconds`：单个候选测试在无探针、无注入时的一次运行墙钟，用于估算 A′“每个计划一个进程”的成本。
  - 对所有运行过测试的个案补填，无法补填写 `na`。
- **门槛不变：** eligible≥6 通过；eligible+unknown≥6 未决；否则不通过。RESULTS.md 并列报告 D4 前后计数。

其余冻结规则、预算与禁止事项不变。

## 偏离登记 D5 — D4 收尾执行交接（2026-09-29）

用户在本轮明确授权 Codex 依次完成第一优先（D4 收尾、历史结果入口提示、轻量 CI）及第二优先（预先冻结的机制对照评估）。本记录在检查目标工作树、重判或动态执行之前提交：D4 原记“Claude 执行、不经 Codex”的人员分工由本次授权替代；技术判据、22 项总体、D3 路径相关要求及 D4 范围不变。

- 按 G0-03、09、11、14、16、17、21、22 顺序处理 D4 动态候选，每项构建/测试仍至多 90 分钟、候选至多 10 个；确定的环境/构建阻塞可提前结束为 unknown/build，不将未运行解释成 site_not_hit。G0-04、10 只更新 patchability；G0-05 不重做。
- 可只读检查已有 scratch 工作树和未跟踪探针，不能把旧探针的存在当作执行成功。保留原有未跟踪文件，不覆盖或顺手纳入提交；需要新探针时使用 D4 专属文件名。目标仓库修改只在指定工作目录，不能回退其他工作。
- 补填 harness_kind 和 baseline_wall_seconds；没有实际无探针单次测量时填 na，不从带探针计时推断。
- 历史 D1–D3 结果继续保留；以追加章节及入口提示记录 D4 后现状和未解决项。每项结果单独提交。
- 第一优先中的 CI 和 Phase 3 入口提示属于此次另行授权的工程/文档工作，不是 G0 样本或判据变更。第二优先另立计划并先提交，沿用已知案例，不宣称前瞻发现。
- 本轮不 push、不启动第三优先，不新增多版本产品兼容层或完整 Tokio 注入 runner。

## G0-03 D4 收尾（2026-09-29）

- `patchable=yes`：已检查本机 Tokio 1.28.2 `src/macros/select.rs:474` 的逐分支元组构造。无须 IntoFuture，旧排除理由撤回。
- 在原父提交 `6d5da99`、Rust 1.70.0 下执行 `CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=.../omicron/d4-review-target timeout 300s cargo test --locked --offline -p omicron-nexus --lib test_serial_console_stream_proxying --no-run`。本次实际构建在 nexus-db-model 的 build script 失败：`DEP_PQ_LIBDIRS` 未设置；不是测试或站点未命中。
- 另核实父版本所需 dendrite OpenAPI 下载 URL（`b9444d0c.../dpd.json`）仍 HTTP 404。历史记录将 dpd 与 ddm 名称混写，本轮依据实际 dpd-client/build.rs/package-manifest.toml，不沿用旧拼写。
- 最终 `unknown/build`，无 site/Pending 动态计数；harness=cargo_test，baseline_wall_seconds=na。没有启动服务，构建加检查约 5 分钟，未用满 90 分钟但已有明确阻塞。恢复成本：准备匹配 libpq 构建环境及该父版本私有/历史 OpenAPI 产物，再重建候选。
- 仅清理本轮创建的 d4-review-target，不删除既有 scratch 文件。

## G0-04 / G0-10 D4 静态更正（2026-09-29）

检查 Tokio 1.18.2 上游版本源码 select.rs:463、已有 Tokio 1.32.0 版本源码 select.rs:474，均为逐分支 future 元组，patchable 改 yes（源形态可行，不声称补丁已执行验证）。G0-04 仍 ineligible/no_preexisting_test；G0-10 实际为 futures::future::select，仍 ineligible/site_kind_not_covered。依 D4 不运行动态候选，约 1 分钟，无服务或构建。

## G0-09 D4 收尾（2026-09-29）

- Tokio 1.38.0 select.rs:498 可逐分支包装，patchable=yes。候选保持 `test/testdrive/sequential-hydration.td`，D3 仍要求 ReplicaTask 外层站点与 SequentialHydration 修复路径相关。
- 在既有父版本 scratch `materialize/w28816` 尝试 `cargo +1.80.0 build --locked -p mz-environmentd -p mz-testdrive`。先修复该版本工具链的残缺安装（不改默认 toolchain）；offline 缺 git 依赖后在线重试，下载成功。随后 vendored OpenSSL 构建因 Perl 模块缺失失败；补用已有 FindBin helper 后仍失败，未升级目标依赖或修改 Cargo.lock。
- 在线构建 94.03 秒，helper 重试 0.88 秒；含检查和工具链恢复约 3 分钟。最终 unknown/build，不是 site_not_hit。未启动服务，未运行候选；harness=testdrive，baseline_wall_seconds=na。
- 恢复成本：完整的 Perl/OpenSSL native 构建依赖、完整 environmentd/testdrive 构建及测试服务；其后才能观察 D3 计数。仅清理本轮 d4-review-target。

## G0-11 D4 收尾（2026-09-29）

- patchable=yes（Tokio 1.17.0 select.rs:461）。核对 w12479 的初始 git tree 与原父提交 a838ec6：只有 vendoring 的 .cargo/config 和未复制的无关 sqlite 子模块不同；现有修改是探针，不把 synthetic `parent` commit 当上游 SHA。
- 尝试重新构建既有 sqllogictest/computed/storaged（Rust 1.60、locked/offline）。149.51 秒后遇旧 protobuf 的 CMake policy 错误；使用已有 protoc 后 9.20 秒遇 OpenSSL 缺 FindBin；再使用系统 OpenSSL 后，旧 librdkafka 构建仍被 CMake 删除 <3.5 兼容性阻止。含检查约 5 分钟。
- 不采信已有成功日志/旧二进制作为本轮通过。最终 unknown/build；harness=sqllogictest，baseline_wall_seconds=na，未运行候选、未启动服务、没有新的站点或 D3 路径计数。
- 恢复成本：匹配旧 native 构建环境（尤其 CMake/librdkafka），重建并运行既有 cluster.slt，再收集关联计数。本次尝试用了既有 target，不删除其原有构建产物或原探针。

## G0-14 D4 动态确认（2026-09-29）

- Tokio 1.38.1 select.rs:498，patchable=yes；在父源码归档 `iroh-8e4e586...` 的新副本 `iroh/d4-review-14` 构建，未修改原 scratch 或原未跟踪探针。
- `cargo +1.80.0 test --locked -p iroh-gossip --lib gossip_net_smoke --no-run` 成功，构建 164.24 秒。无探针 binary `iroh_gossip-cbdd3d175a3898c9 net::test::gossip_net_smoke --exact`：1/1 通过，墙钟 1.50 秒。
- 添加最小计数探针后重编译 14.68 秒，相同既有测试通过（1.393 秒），site_hit=39、branch_pending=29。每个分支实例只计第一次 Pending。generic_site=no，D3 关联列为 na。探针为 `probes/D4-G0-14.diff`；与旧未跟踪 G0-14.diff 不同，不移动原有 cfg 属性。
- 最终 eligible / eligible_pending。仅证明既有测试触达该站点且实际 Pending，不证明触发历史缺陷。
- 测试内嵌 relay 监听 loopback；endpoint 会使用本机网卡地址，均随测试退出。未启动 Docker 或外部服务。TMPDIR 在本例目录下。含工具等待和用户提示处理墙钟约 14 分钟，未超 90 分钟；清理本例 target。

## G0-17 D4 收尾（2026-09-29）

- patchable=yes：Tokio 1.17.0 select.rs:461 为逐分支元组。核对父提交归档与 w12485 的 coordinator、partitioned client、Cargo.lock 一致。纠正旧记录的函数拼写：该父版本外层 select 在 coord.rs:739，745 行是 `self.dataflow_client.ready()`，经 Controller::ready 和 compute client stream 到 Partitioned::recv，并非 G0-11 父版本的直接 recv。D3 仍需关联重连路径，不改变门槛。
- 安装父版本声明的 Rust 1.60.0 minimal 后，执行 `CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=$PWD/d4-review-target timeout 300s cargo +1.60.0 build --locked --offline -p mz-sqllogictest -p storaged -p materialized`。205.39 秒后 openssl-sys 构建失败，Perl 缺 FindBin。
- 最终 unknown/build；既有 cluster.slt 未执行，计数不填零，baseline_wall_seconds=na。约 5 分钟，无服务启动。恢复需匹配 native 构建依赖并完整构建测试服务。

## G0-16 D4 动态确认（2026-09-29）

- 核对父提交 9052905 的 Cargo.lock、netmon actor 与既有 test_smoke_monitor 源码和 d4-review-16 副本一致。Tokio 1.38.1 select.rs:498 可逐分支包装，patchable=yes。
- Rust 1.80.0 首次 offline 构建缺 addr2line 缓存，改用 locked 在线构建后成功（201.56 秒）；无探针 `net::netmon::tests::test_smoke_monitor --exact` 1/1 通过，墙钟 15.01 秒。
- 新增独立 D4 探针，重建 30.61 秒；同一既有测试两次均通过，第二次程序化计数 site_hit=63、branch_pending=62，15.018 秒。每分支实例只计第一次 Pending。generic_site=no，defect_path_pending=na。第一次运行输出未用于最终计数。
- 最终 eligible / eligible_pending，只证明站点可达并经历 Pending，不证明历史缺陷触发。探针为 probes/D4-G0-16.diff，原未跟踪 G0-16.diff 未改动。仅启动测试自己的网卡状态观察器，没有改动网络配置或外部服务。约 12 分钟（含穿插的 G0-17 等待，不与其耗时相加解释为独立总墙钟）。

## G0-21 D4 动态确认（2026-09-29）

- 父提交归档与 scratch HEAD 的 file_watcher.rs、既有 watcher_tests.rs、Cargo.lock 一致；Tokio 1.17.0 select.rs:461 可包装，patchable=yes。
- Rust 1.59.0 `cargo test --locked --offline -p deno --test integration_tests --no-run` 构建成功，431.66 秒。无探针 binary 执行 `integration::watcher::run_watch --exact`，1/1 通过、1.58 秒。
- D4 探针只观察站点和首次 Pending；D3 仅当该 branch 的一次 poll 中，已收集文件路径的 DebouncedReceiver 内层循环实际 Pending 才计数。另做探针本身的隔离正/负/重复 poll 控制：无关联路径=0、有路径=1、不重复计数，均通过。这不是新增候选，不进入 G0 分母。
- 探针重建 58.30 秒。三个原预筛既有测试均通过：run_watch 6/2/0（1.641 秒），bundle_js_watch 2/1/0（1.502 秒），lint_watch_test 3/3/0（1.241 秒），依次为 site_hit/branch_pending/defect_path_pending。合计 11/6/0；按 D3 判为 ineligible/site_not_hit，只适用于这组候选，不声称整个应用永远不可达。
- 调用错误单独说明：最初 test target 误写 integration；一次漏设 CARGO_TARGET_DIR 导致默认 target 重建，14.43 秒后停止；一次 exact filter 名称错误运行 0 测试；一次重建命令 cwd 错误。均非项目失败、均未计作通过。随后使用实际 binary 和 `--list` 中的精确名称执行上述测量。
- TMPDIR 位于本例 d4-tmp；无外部服务或网络设置变更。约 14 分钟。原有未跟踪探针和默认 target 的原内容保留，仅清理本次 d4-review-target。

## G0-22 D4 收尾（2026-09-29）

- 新建 detached parent 12512f3 worktree，未复制旧 scratch 的 Cargo.lock、bindgen、C header 等改动。Tokio 1.36.0 select.rs:474 可包装，patchable=yes。
- Rust 1.77.0 locked/offline 构建 pageserver/safekeeper/control_plane，首次因缺 protoc 失败（125.48 秒）。指定已有 scratch protoc 及旧 scratch 的 PostgreSQL install 路径后成功（180.69 秒），storage_broker 单独成功（53.40 秒）。旧 PostgreSQL 原生产物只是构建输入；本轮未将它视为经过独立验证的干净父版本产物。最初 package 名误写 neon_local，已按实际 Cargo manifest 改为 control_plane。
- 既有 `test_timeline_delete` 的 debug/release-pg16 两参数实例均在 fixture setup 阶段失败：找不到 poetry，fixture 准备启动 moto mock S3。测试体未执行，未启动 moto 或 Neon 服务，0.81 秒启动失败不是有效 baseline_wall_seconds。
- 最终 unknown/build（测试环境准备阻塞，Rust 二进制 build_ok=yes），站点/路径计数保持未知，不填零。恢复需要该历史版本支持的 Poetry/moto 测试环境，且外部 mock 服务须满足冻结的 loopback Docker 限制；随后才可测无探针基线及 D3。没有为强行完成而替换测试、升级目标依赖或启动主机外部服务。
- 约 10 分钟；只清理本次 d4-review-target，旧 work 和原未跟踪探针不动。

## D4 完整总体收尾与第一优先验证（2026-09-29）

- 22 个唯一 URL 保持不变：eligible=4、eligible_pending=4、ineligible=12、unknown=6、eligible_if_futures_covered=5。4<6 且 4+6≥6，仍未决。补填历史执行项 harness；无历史无探针计时则保持 na。minutes_used 补入前一会话 D4 近似耗时，与本次新增行统一为累积工作分钟；结果入口明确重叠和非精确总墙钟限制。
- 核心本机重新运行：26 个测试通过，1 个既有 doctest ignored；locked Clippy all-targets、warnings-denied docs、quickstart 均通过。CI 工作流和 Phase 3 历史提示已由前一会话提交，未 push，所以没有本轮远端 CI 通过结论。
- 第一优先范围已收尾。第二优先的计划和场景已分开提交，实际对照运行尚未开始。未进入第三优先、未对外发布。
